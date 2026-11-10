//! 라이브 마이그레이션의 받는 쪽(대상 노드). 원본 노드의 `VmMigrate` Task가 노드 사이 RPC로 부른다
//! (설계 §2의 3·5단계와 실패 때의 취소).

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_driver::{DriverError, Hypervisor, StopMode, Storage};
use opl_peer::{
    IncomingDisk, IncomingError, IncomingMigration, MigrateIncomingRequest,
    MigrateIncomingResponse, MigrationReceiver,
};
use opl_reconciler::FirewallGate;
use opl_reconciler::resolve::incoming_spec;
use opl_types::node::NodeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{DiskMove, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use crate::migration_disks::{self, DiskRecord, RecordedVolume, Records, Role};

/// 원본 노드가 기록한 `Migrating` 상태가 이 노드에 적용되기를 기다리는 시간. 원본은 상태를 쓴 직후
/// 부르므로 이 노드의 저장소가 조금 늦을 수 있다.
const STATUS_WAIT: Duration = Duration::from_secs(10);
const STATUS_POLL: Duration = Duration::from_millis(50);

pub struct IncomingConfig {
    /// 이 노드의 id
    pub node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(reconciler와 같다)
    pub mount_root: PathBuf,
    /// 디스크 볼륨을 켜고 끄는 저장소(reconciler와 같다)
    pub storage: Arc<dyn Storage>,
    /// 받는 QEMU가 들을 IP(opld `cluster_listen`의 IP). 원본은 이 노드의 클러스터 주소의 호스트로
    /// 접속한다(노드 인증서에 있는 이름).
    pub listen_host: String,
    /// 받는 포트 범위(`[migration] ports`). 디스크를 복사하면 NBD 서버도 이 범위의 포트를 쓴다.
    pub ports: RangeInclusive<u16>,
    /// 마이그레이션 디스크 기록 파일의 디렉터리(`{data_dir}/migration-disks`, Task 실행기와 같다)
    pub migration_disks: PathBuf,
}

/// 대상 노드에서 마이그레이션을 받는다. 받는 VM은 아직 원본 노드에 배정돼 있으므로(`spec.node`)
/// 이 노드의 reconciler는 손대지 않는다. 확정 뒤 넘겨받는 것은 reconciler다.
#[derive(Clone)]
pub struct IncomingMigrations {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<IncomingConfig>,
    /// 이 노드의 방화벽(reconciler와 같다). 없으면 보안 그룹을 건 NIC가 있는 VM을 받지 않는다.
    firewall: Option<Arc<dyn FirewallGate>>,
}

impl IncomingMigrations {
    pub fn new(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: IncomingConfig) -> Self {
        Self {
            cluster,
            hv,
            config: Arc::new(config),
            firewall: None,
        }
    }

    /// 보안 그룹을 건 NIC가 있는 VM을 받기 직전에 이 노드의 방화벽 규칙을 넣는다.
    pub fn with_firewall(mut self, firewall: Option<Arc<dyn FirewallGate>>) -> Self {
        self.firewall = firewall;
        self
    }

    fn read_vm(&self, id: Uuid) -> Result<Option<Vm>, IncomingError> {
        self.cluster
            .get::<VmSpec>(id)
            .map_err(|e| IncomingError::Failed(format!("cannot read VM {id}: {e}")))
    }

    /// VM이 `task`로 이 노드에 오는 중이면 그 VM. 아니면 거절 이유.
    fn migrating_here(&self, vm: Option<Vm>, id: Uuid, task: Uuid) -> Result<Vm, String> {
        let me = self.config.node;
        let vm = vm.ok_or_else(|| format!("VM {id} not found"))?;
        if vm.spec.node == Some(me) {
            return Err(format!("VM {:?} is already on this node", vm.name));
        }
        let expected = Some((me, task));
        if vm.status.phase != VmPhase::Migrating
            || vm.status.migration.as_ref().map(|m| (m.node, m.task)) != expected
        {
            return Err(format!(
                "VM {:?} is not migrating to this node by task {task}",
                vm.name
            ));
        }
        Ok(vm)
    }

    /// 복사할 디스크를 이 노드에 만든다(설계 §3.3의 4): 디스크마다 기록 파일(`target`)에 먼저 적고
    /// 원본의 가상 크기로 계획의 풀·형식에 만든 뒤 만들었다고 다시 적고(`created`) 크기가 같은지 본다.
    /// 이미 있으면(공유인 풀을 로컬로
    /// 설정했거나 한 호스트의 두 노드) 기록에서 빼고 실패한다(남의 파일을 지우지 않는다). 실패하면 이
    /// 호출이 만든 볼륨을 지우고 기록을 지운다. 만든 볼륨을 돌려준다.
    async fn create_disks(
        &self,
        records: &Records,
        vm: &Vm,
        task: Uuid,
        plan: &[DiskMove],
        disks: &[IncomingDisk],
    ) -> Result<Vec<RecordedVolume>, String> {
        let dir = records.dir();
        let mut record = DiskRecord {
            vm: vm.id,
            task,
            role: Role::Target,
            volumes: Vec::new(),
        };
        let mut made = Vec::new();
        let created = self
            .create_recorded(dir, &mut record, &mut made, plan, disks)
            .await;
        if created.is_err() {
            let storage = &*self.config.storage;
            if migration_disks::delete_volumes(&self.cluster, storage, vm.id, &made).await
                && let Err(e) = migration_disks::remove(dir, task)
            {
                tracing::warn!(vm = %vm.id, error = %e, "cannot remove a migration disk record");
            }
        }
        created.map(|()| made)
    }

    /// [`create_disks`](Self::create_disks). 만든 볼륨을 `made`에 적는다.
    async fn create_recorded(
        &self,
        dir: &Path,
        record: &mut DiskRecord,
        made: &mut Vec<RecordedVolume>,
        plan: &[DiskMove],
        disks: &[IncomingDisk],
    ) -> Result<(), String> {
        let storage = &*self.config.storage;
        let me = self.config.node;
        for (m, disk) in plan.iter().zip(disks) {
            let read = |e: opl_cluster::ClusterError| e.to_string();
            let name = self
                .cluster
                .get::<VolumeSpec>(m.volume)
                .map_err(read)?
                .map_or_else(|| m.volume.to_string(), |v| v.name);
            let pool = self
                .cluster
                .get_by_name::<StoragePoolSpec>(&m.pool)
                .map_err(read)?
                .ok_or_else(|| format!("storage pool {:?} not found", m.pool))?;
            // 만들기 전에 적는다. 만들었다고 적기 전에 죽으면 정리는 이 항목을 지우지 않는다(남의 파일일
            // 수 있다).
            record.volumes.push(RecordedVolume {
                id: m.volume,
                pool: m.pool.clone(),
                format: m.format,
                created: false,
            });
            migration_disks::write(dir, record)
                .map_err(|e| format!("cannot record the disks to copy: {e}"))?;
            match storage
                .create_volume(&pool, m.volume, disk.size_bytes, m.format)
                .await
            {
                Ok(_) => {
                    let entry = record.volumes.last_mut().expect("just pushed");
                    entry.created = true;
                    made.push(entry.clone());
                    migration_disks::write(dir, record)
                        .map_err(|e| format!("cannot record the disks to copy: {e}"))?;
                }
                Err(DriverError::VolumeExists(_)) => {
                    record.volumes.pop();
                    migration_disks::write(dir, record)
                        .map_err(|e| format!("cannot record the disks to copy: {e}"))?;
                    let node = self
                        .cluster
                        .get::<NodeSpec>(me)
                        .ok()
                        .flatten()
                        .map_or_else(|| me.to_string(), |n| n.name);
                    return Err(format!(
                        "volume {name:?} already exists on node {node:?} in pool {:?}; is the pool really node-local?",
                        m.pool
                    ));
                }
                Err(e) => {
                    // 만들다 만 파일이 남았을 수 있지만 만들었는지 모르므로 지우지 않는다.
                    tracing::warn!(volume = %m.volume, pool = %m.pool, error = %e, "creating a disk to copy failed; a partial file may be left");
                    return Err(format!(
                        "cannot create disk {name:?} in pool {:?}: {e}",
                        m.pool
                    ));
                }
            }
            let size = storage
                .volume_info(&pool, m.volume, m.format)
                .await
                .map_err(|e| format!("cannot read disk {name:?}: {e}"))?
                .virtual_bytes;
            if size != disk.size_bytes {
                return Err(format!(
                    "disk {name:?} would change size on pool {:?} ({} → {size} bytes)",
                    m.pool, disk.size_bytes
                ));
            }
        }
        Ok(())
    }

    /// 계획의 디스크가 VM에서 몇 번째 디스크인지(드라이버의 `disk{i}`)
    fn exports(&self, vm: &Vm, plan: &[DiskMove]) -> Result<Vec<usize>, String> {
        let mut exports = Vec::new();
        for (i, disk) in vm.spec.disks.iter().enumerate() {
            let volume = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .map_err(|e| e.to_string())?;
            if volume.is_some_and(|v| plan.iter().any(|m| m.volume == v.id)) {
                exports.push(i);
            }
        }
        Ok(exports)
    }

    /// [`incoming`](MigrationReceiver::incoming)의 나머지. 부르는 쪽(원본의 RPC)이 끊겨도 끝나도록 따로
    /// 돈다. 복사할 디스크가 있으면 받는 QEMU를 띄울 때까지 이 노드의 디스크 기록 잠금을 쥔다(정리가 막
    /// 만든 볼륨을 지우지 않게). 잠금을 기다리는 사이 원본이 되돌렸을 수 있으므로 VM이 아직 이 Task로 이
    /// 노드에 오는 중인지 다시 본다. 이 VM의 남은 디스크 기록을 정리한 뒤에도 같은 (볼륨, 풀)을 적은 다른
    /// Task의 기록이 남았으면(지우지 못했다) 받지 않는다(그 기록의 정리가 같은 위치의 새 디스크를 지우지
    /// 않게).
    async fn receive(
        &self,
        req: MigrateIncomingRequest,
        exports: Vec<usize>,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let c = &self.config;
        let records = if req.disks.is_empty() {
            None
        } else {
            Some(migration_disks::lock(&c.migration_disks).await)
        };
        let vm = self
            .migrating_here(self.read_vm(req.vm)?, req.vm, req.task)
            .map_err(IncomingError::Rejected)?;
        let plan = vm
            .status
            .migration
            .as_ref()
            .map_or(&[][..], |m| &m.disks[..]);
        let mut made = Vec::new();
        if let Some(records) = &records {
            // 같은 VM이 되돌아오면 이 노드에 남은 원본 사본이 새 볼륨을 막는다.
            migration_disks::clean_up(
                &self.cluster,
                &*c.storage,
                &*self.hv,
                c.node,
                records,
                Some(vm.id),
                None,
            )
            .await;
            if let Some(left) = self.left_copy(records.dir(), req.task, plan) {
                return Err(IncomingError::Failed(left));
            }
            made = self
                .create_disks(records, &vm, req.task, plan, &req.disks)
                .await
                .map_err(IncomingError::Failed)?;
        }
        // 실패하면 incoming_spec이 켠 볼륨을 끈다.
        let started =
            match incoming_spec(&self.cluster, &vm, c.node, &c.mount_root, &*c.storage).await {
                Ok(spec) => match self
                    .hv
                    .start_incoming(&spec, req.task, &c.listen_host, c.ports.clone(), &exports)
                    .await
                {
                    Ok(ports) => Ok(ports),
                    Err(e) => {
                        deactivate_disks(&self.cluster, &*c.storage, &vm, &made).await;
                        Err(e.to_string())
                    }
                },
                Err(e) => Err(e.to_string()),
            };
        let ports = match started {
            Ok(ports) => ports,
            Err(error) => {
                if let Some(records) = &records
                    && migration_disks::delete_volumes(&self.cluster, &*c.storage, vm.id, &made)
                        .await
                    && let Err(e) = migration_disks::remove(records.dir(), req.task)
                {
                    tracing::warn!(vm = %vm.id, error = %e, "cannot remove a migration disk record");
                }
                return Err(IncomingError::Failed(error));
            }
        };
        drop(records);
        tracing::info!(vm = %vm.id, task = %req.task, port = ports.migration, nbd = ?ports.nbd, "receiving a live migration");
        Ok(MigrateIncomingResponse {
            port: ports.migration,
            nbd_port: ports.nbd,
        })
    }

    /// 다른 Task의 기록에 아직 적힌 계획의 (볼륨, 풀)이 있으면 그 까닭
    fn left_copy(&self, dir: &Path, task: Uuid, plan: &[DiskMove]) -> Option<String> {
        let left = migration_disks::list(dir);
        plan.iter().find_map(|m| {
            let record = left.iter().find(|r| {
                r.task != task
                    && r.volumes
                        .iter()
                        .any(|v| v.id == m.volume && v.pool == m.pool)
            })?;
            let name = self
                .cluster
                .get::<VolumeSpec>(m.volume)
                .ok()
                .flatten()
                .map_or_else(|| m.volume.to_string(), |v| v.name);
            Some(format!(
                "an earlier copy of disk {name:?} in pool {:?} is not cleaned up on this node yet (migration task {}); try again later",
                m.pool, record.task
            ))
        })
    }

    /// 원본이 막 쓴 `Migrating` 상태가 보일 때까지 잠시 기다린다.
    async fn wait_migrating_here(&self, id: Uuid, task: Uuid) -> Result<Vm, IncomingError> {
        let deadline = tokio::time::Instant::now() + STATUS_WAIT;
        loop {
            match self.migrating_here(self.read_vm(id)?, id, task) {
                Ok(vm) => return Ok(vm),
                Err(why) if tokio::time::Instant::now() >= deadline => {
                    return Err(IncomingError::Rejected(why));
                }
                Err(_) => tokio::time::sleep(STATUS_POLL).await,
            }
        }
    }
}

/// VM 디스크의 볼륨을 이 노드에서 끈다(받으려고 켰거나, 마이그레이션을 보낸 원본·남은 받는 QEMU의
/// 디스크). 복사하는 디스크(`copies`, 이 노드의 디스크 기록)는 볼륨 기록 대신 이 노드에서 실제로 가진
/// 풀·형식으로 끈다(받는 쪽은 계획의 풀, 확정 뒤 원본은 원래 풀). 끄지 못하면 경고만 남긴다(남은
/// 볼륨은 Task 실행기의 정리가 끈다).
pub(crate) async fn deactivate_disks(
    cluster: &Cluster,
    storage: &dyn Storage,
    vm: &Vm,
    copies: &[RecordedVolume],
) {
    for disk in &vm.spec.disks {
        let found = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .and_then(|volume| {
                let Some(volume) = volume else {
                    return Ok(None);
                };
                let (pool, format) = match copies.iter().find(|c| c.id == volume.id) {
                    Some(c) => (&c.pool, c.format),
                    None => (&volume.spec.pool, volume.spec.format),
                };
                let pool = cluster.get_by_name::<StoragePoolSpec>(pool)?;
                Ok(pool.map(|pool| (volume, pool, format)))
            });
        let (volume, pool, format) = match found {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                continue;
            }
        };
        if let Err(e) = storage.deactivate_volume(&pool, volume.id, format).await {
            tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
        }
    }
}

#[async_trait]
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지(복사할 디스크가 VM의 계획과 같은지)
    /// 본 뒤 (보안 그룹이 있으면 방화벽 규칙을 넣고) 복사할 디스크를 만들고(이 VM의 남은 디스크 기록을
    /// 먼저 정리한다) 디스크를 켜고(공유 디스크는 원본이 아직 쓰므로 공유로) 받는 QEMU를 띄운다(복사할
    /// 디스크는 NBD로 내보낸다). 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 먼저 끈다.
    /// 실패하면 켠 디스크를 끄고 만든 디스크를 지운다(띄운 QEMU는 드라이버가 끈다).
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let mine = self
            .hv
            .qemu_version()
            .await
            .map_err(|e| IncomingError::Failed(format!("cannot read the QEMU version: {e}")))?;
        if mine != req.qemu_version {
            return Err(IncomingError::Incompatible(format!(
                "the target node runs QEMU {mine} but the source runs {}",
                req.qemu_version
            )));
        }
        let vm = self.wait_migrating_here(req.vm, req.task).await?;
        let plan = vm
            .status
            .migration
            .as_ref()
            .map_or(&[][..], |m| &m.disks[..]);
        if !plan
            .iter()
            .map(|m| m.volume)
            .eq(req.disks.iter().map(|d| d.volume))
        {
            return Err(IncomingError::Rejected(format!(
                "the disks to copy of VM {:?} do not match its migration",
                vm.name
            )));
        }
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        let left = self.hv.list_incoming().await.map_err(fail)?;
        if left
            .iter()
            .any(|&(id, task)| id == req.vm && task != req.task)
        {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an earlier migration");
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        }
        // 보안 그룹을 건 NIC가 있으면 받는 QEMU(tap)를 띄우기 전에 방화벽 규칙을 넣는다. 넣지 못하면
        // 받지 않는다.
        if vm.spec.has_firewall() {
            let Some(gate) = &self.firewall else {
                return Err(IncomingError::Failed(
                    "firewall: this node has no firewall (nftables)".into(),
                ));
            };
            gate.sync()
                .await
                .map_err(|e| IncomingError::Failed(format!("firewall: {e}")))?;
        }
        let exports = self.exports(&vm, plan).map_err(IncomingError::Failed)?;
        // 원본의 RPC가 끊겨도 디스크 만들기·기록과 받는 QEMU 띄우기를 함께 끝낸다(원본은 늦은 결과를
        // 되돌림으로 다룬다).
        let this = self.clone();
        tokio::spawn(async move { this.receive(req, exports).await })
            .await
            .map_err(|e| IncomingError::Failed(format!("receiving the migration failed: {e}")))?
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.migrating_here(self.read_vm(req.vm)?, req.vm, req.task)
            .map_err(IncomingError::Rejected)?;
        self.hv
            .incoming_wait(req.vm)
            .await
            .map_err(|e| IncomingError::Failed(e.to_string()))
    }

    /// 확정 전이면(`spec.node`가 이 노드가 아니다) 이 Task의 받는 중 표시가 있는 QEMU를 끄고 디스크를
    /// 끄고, 디스크 기록 파일대로 복사하려고 만든 디스크를 지운다. 받는 QEMU가 이미 끝났어도(받기
    /// 실패) 디스크는 끈다. 확정됐거나 VM이 다른 마이그레이션
    /// 중이면 거절하고 아무것도 하지 않는다. 다른 Task의 받는 QEMU는 그대로 둔다(정리가 끈다).
    /// 끝날 때까지 이 노드의 디스크 기록 잠금을 쥔다(받기가 끝난 뒤에 한다).
    /// 원본의 RPC가 끊겨도 끝나도록 따로 돈다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        let this = self.clone();
        tokio::spawn(async move { this.abort_receiving(req).await })
            .await
            .map_err(|e| IncomingError::Failed(format!("aborting the migration failed: {e}")))?
    }
}

impl IncomingMigrations {
    /// [`abort`](MigrationReceiver::abort)의 본문
    async fn abort_receiving(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        let c = &self.config;
        let records = migration_disks::lock(&c.migration_disks).await;
        let vm = self.read_vm(req.vm)?;
        if let Some(vm) = &vm {
            if vm.spec.node == Some(self.config.node) {
                return Err(IncomingError::Rejected(format!(
                    "the migration of VM {:?} has already been committed",
                    vm.name
                )));
            }
            if let Some(m) = &vm.status.migration
                && m.task != req.task
            {
                return Err(IncomingError::Rejected(format!(
                    "VM {:?} is in another migration (task {})",
                    vm.name, m.task
                )));
            }
        }
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        let left = self.hv.list_incoming().await.map_err(fail)?;
        if left.contains(&(req.vm, req.task)) {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an aborted migration");
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        } else if self.hv.inspect(req.vm).await.map_err(fail)?.is_some() {
            // 받는 중 표시가 없는 QEMU는 받는 QEMU가 아니다. 그 디스크도 쓰는 중이다.
            return Ok(());
        }
        if let Some(vm) = &vm {
            // 복사하려고 만든 디스크는 기록의 풀에 있다.
            let copies = migration_disks::load(records.dir(), req.task)
                .map(|r| r.volumes)
                .unwrap_or_default();
            deactivate_disks(&self.cluster, &*c.storage, vm, &copies).await;
        }
        // 복사하려고 만든 디스크를 지운다(확정되지 않았다).
        migration_disks::clean_up(
            &self.cluster,
            &*c.storage,
            &*self.hv,
            c.node,
            &records,
            Some(req.vm),
            Some(req.task),
        )
        .await;
        Ok(())
    }
}
