//! 라이브 마이그레이션의 받는 쪽(대상 노드). 원본 노드의 `VmMigrate` Task가 노드 사이 RPC로 부른다
//! (설계 §2의 3·5단계와 실패 때의 취소).

use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_driver::{Hypervisor, StopMode, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::resolve::incoming_spec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use uuid::Uuid;

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
    /// 받는 포트 범위(`[migration] ports`)
    pub ports: RangeInclusive<u16>,
}

/// 대상 노드에서 마이그레이션을 받는다. 받는 VM은 아직 원본 노드에 배정돼 있으므로(`spec.node`)
/// 이 노드의 reconciler는 손대지 않는다. 확정 뒤 넘겨받는 것은 reconciler다.
pub struct IncomingMigrations {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: IncomingConfig,
}

impl IncomingMigrations {
    pub fn new(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: IncomingConfig) -> Self {
        Self {
            cluster,
            hv,
            config,
        }
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
        let expected = Some(MigrationStatus { node: me, task });
        if vm.status.phase != VmPhase::Migrating || vm.status.migration != expected {
            return Err(format!(
                "VM {:?} is not migrating to this node by task {task}",
                vm.name
            ));
        }
        Ok(vm)
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
/// 디스크). 끄지 못하면 경고만 남긴다(남은 볼륨은 Task 실행기의 정리가 끈다).
pub(crate) async fn deactivate_disks(cluster: &Cluster, storage: &dyn Storage, vm: &Vm) {
    for disk in &vm.spec.disks {
        let found = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .and_then(|volume| {
                let Some(volume) = volume else {
                    return Ok(None);
                };
                let pool = cluster.get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                Ok(pool.map(|pool| (volume, pool)))
            });
        let (volume, pool) = match found {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                continue;
            }
        };
        if let Err(e) = storage
            .deactivate_volume(&pool, volume.id, volume.spec.format)
            .await
        {
            tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
        }
    }
}

#[async_trait]
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 디스크를 공유로 켜고(원본이
    /// 아직 쓴다) 받는 QEMU를 띄운다. 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 먼저
    /// 끈다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
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
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        let left = self.hv.list_incoming().await.map_err(fail)?;
        if left
            .iter()
            .any(|&(id, task)| id == req.vm && task != req.task)
        {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an earlier migration");
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        }
        let c = &self.config;
        // 실패하면 incoming_spec이 켠 볼륨을 끈다.
        let spec = incoming_spec(&self.cluster, &vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| IncomingError::Failed(e.to_string()))?;
        match self
            .hv
            .start_incoming(&spec, req.task, &c.listen_host, c.ports.clone())
            .await
        {
            Ok(port) => {
                tracing::info!(vm = %vm.id, task = %req.task, port, "receiving a live migration");
                Ok(port)
            }
            Err(e) => {
                deactivate_disks(&self.cluster, &*self.config.storage, &vm).await;
                Err(IncomingError::Failed(e.to_string()))
            }
        }
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
    /// 끈다. 받는 QEMU가 이미 끝났어도(받기 실패) 디스크는 끈다. 확정됐거나 VM이 다른 마이그레이션
    /// 중이면 거절하고 아무것도 하지 않는다. 다른 Task의 받는 QEMU는 그대로 둔다(정리가 끈다).
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        let vm = self.read_vm(req.vm)?;
        if let Some(vm) = &vm {
            if vm.spec.node == Some(self.config.node) {
                return Err(IncomingError::Rejected(format!(
                    "the migration of VM {:?} has already been committed",
                    vm.name
                )));
            }
            if let Some(m) = vm.status.migration
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
            deactivate_disks(&self.cluster, &*self.config.storage, vm).await;
        }
        Ok(())
    }
}
