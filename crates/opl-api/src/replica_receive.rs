//! 로컬 디스크 복제의 받는 쪽(대상 노드, 계획 55). 원본 노드의 `VmReplicate` Task가 노드 사이 RPC로 부른다:
//! 받을 준비(복제본을 만들거나 겹층을 두고 받는 QEMU를 띄움), 끝내기(회차를 기록하고 겹층을 합침), 버리기.
//! 이 노드의 복제본 기록 파일([`opl_reconciler::replicas`])이 복제본 파일을 지우는 유일한 근거이고, 청소
//! ([`IncomingReplicas::sweep`])가 지운 VM·뺀 대상·뗀 디스크의 복제본과 끝난 회차의 겹층·받는 QEMU를 치운다.

use std::collections::{BTreeMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_driver::{DriverError, Hypervisor, ReceiverExport, Storage, disk_location};
use opl_peer::{
    IncomingDisk, PrepareMode, PrepareVolume, ReplicaAbortRequest, ReplicaError,
    ReplicaFinishRequest, ReplicaPrepareRequest, ReplicaPrepareResponse, ReplicaReceiver,
};
use opl_reconciler::replicas::{
    self, Overlay, RecordedReplica, Records, ReplicaRecord, ReplicaState, Role,
};
use opl_types::node::{NODE_REPLICATION, NodeSpec};
use opl_types::storage::{StoragePool, StoragePoolSpec, VolumeSpec, pool_state_on};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{DiskMove, Vm, VmSpec};
use uuid::Uuid;

use crate::cluster::missing_feature;
use crate::vms::vm_uses_volume;

/// 청소([`IncomingReplicas::sweep`]) 간격
pub const REPLICA_SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// 증분 겹층에 바뀐 바이트 말고 더 남겨 둘 공간(qcow2 메타데이터, 다른 쓰기)
const OVERLAY_HEADROOM: u64 = 1 << 30;

/// 원본이 만든 `VmReplicate` Task가 이 노드의 저장소에 보이기를 기다리는 시간
const TASK_WAIT: Duration = Duration::from_secs(10);
const TASK_POLL: Duration = Duration::from_millis(50);

pub struct ReplicaConfig {
    /// 이 노드의 id
    pub node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(reconciler와 같다)
    pub mount_root: PathBuf,
    /// 복제본을 만들고 겹층을 합치는 저장소(reconciler와 같다)
    pub storage: Arc<dyn Storage>,
    /// 받는 QEMU의 NBD 서버가 들을 IP(마이그레이션과 같다)
    pub listen_host: String,
    /// NBD 포트 범위(`[migration] ports`, 마이그레이션과 나눠 쓴다)
    pub ports: RangeInclusive<u16>,
    /// 복제본 기록 파일의 디렉터리(`{data_dir}/replicas`, reconciler와 같다)
    pub replica_dir: PathBuf,
}

/// 대상 노드에서 복제 회차를 받는다. 복제본은 Raft 리소스가 아니므로(원본과 같은 볼륨 id) 이 노드의 reconciler는
/// 손대지 않는다.
#[derive(Clone)]
pub struct IncomingReplicas {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReplicaConfig>,
}

/// 준비 중에 이 호출이 바꾼 것. 실패하면 되돌린다.
#[derive(Default)]
struct Made {
    /// 새로 만든 복제본
    volumes: Vec<RecordedReplica>,
    /// 새로 만든 겹층
    overlays: Vec<PathBuf>,
}

impl IncomingReplicas {
    pub fn new(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReplicaConfig) -> Self {
        Self {
            cluster,
            hv,
            config: Arc::new(config),
        }
    }

    fn node_name(&self) -> String {
        let me = self.config.node;
        self.cluster
            .get::<NodeSpec>(me)
            .ok()
            .flatten()
            .map_or_else(|| me.to_string(), |n| n.name)
    }

    fn read_vm(&self, id: Uuid) -> Result<Option<Vm>, String> {
        self.cluster
            .get::<VmSpec>(id)
            .map_err(|e| format!("cannot read VM {id}: {e}"))
    }

    fn pool(&self, name: &str) -> Result<StoragePool, String> {
        self.cluster
            .get_by_name::<StoragePoolSpec>(name)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("storage pool {name:?} not found"))
    }

    /// 끝나지 않은 그 VM의 `VmReplicate`·`VmMigrate` Task들(id)
    fn unfinished_tasks(&self, vm: Uuid) -> Result<Vec<Uuid>, String> {
        let (_, tasks) = self
            .cluster
            .list::<TaskSpec>()
            .map_err(|e| format!("cannot list tasks: {e}"))?;
        Ok(tasks
            .into_iter()
            .filter(|t| {
                t.spec.target == vm
                    && !t.status.state.is_finished()
                    && matches!(
                        t.spec.op,
                        TaskOp::VmReplicate { .. } | TaskOp::VmMigrate { .. }
                    )
            })
            .map(|t| t.id)
            .collect())
    }

    /// 원본이 막 만든 회차의 Task가 보일 때까지 잠시 기다린다(청소는 Task가 없는 회차의 받는 QEMU를 끈다).
    async fn wait_task(&self, req: &ReplicaPrepareRequest) -> Result<(), ReplicaError> {
        let deadline = tokio::time::Instant::now() + TASK_WAIT;
        loop {
            let task = self
                .cluster
                .get::<TaskSpec>(req.run)
                .map_err(|e| ReplicaError::Failed(format!("cannot read task {}: {e}", req.run)))?;
            match task {
                Some(t) if t.spec.target == req.vm && !t.status.state.is_finished() => {
                    return Ok(());
                }
                Some(_) => {
                    return Err(ReplicaError::Rejected(format!(
                        "task {} is not an unfinished replication of VM {}",
                        req.run, req.vm
                    )));
                }
                None if tokio::time::Instant::now() >= deadline => {
                    return Err(ReplicaError::Rejected(format!(
                        "replication task {} not found",
                        req.run
                    )));
                }
                None => tokio::time::sleep(TASK_POLL).await,
            }
        }
    }

    /// VM이 이 노드를 복제 대상으로 두고 다른 노드에서 돈다. 아니면 거절 이유.
    fn check_target(&self, vm: Option<Vm>, id: Uuid) -> Result<Vm, String> {
        let me = self.config.node;
        let vm = vm.ok_or_else(|| format!("VM {id} not found"))?;
        if !vm.spec.replication.iter().any(|t| t.node == me) {
            return Err(format!(
                "node {:?} is not a replication target of VM {:?}",
                self.node_name(),
                vm.name
            ));
        }
        if vm.spec.node == Some(me) {
            return Err(format!("VM {:?} runs on this node", vm.name));
        }
        Ok(vm)
    }

    /// 같은 VM의 다른 회차의 받는 QEMU를 끈다(원본은 VM마다 회차를 하나씩 돌리므로 앞 회차는 끝났다).
    async fn stop_other_receivers(&self, vm: Uuid, run: Uuid) -> Result<(), String> {
        let runs = self
            .hv
            .replica_receivers()
            .await
            .map_err(|e| e.to_string())?;
        for other in runs.into_iter().filter(|&r| r != run) {
            let ours = self
                .cluster
                .get::<TaskSpec>(other)
                .map_err(|e| e.to_string())?
                .is_some_and(|t| t.spec.target == vm);
            if ours {
                tracing::info!(vm = %vm, run = %other, "stopping the replica receiver of an earlier run");
                self.hv
                    .stop_replica_receiver(other)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// [`prepare`](ReplicaReceiver::prepare)의 본문. 그 VM의 잠금(앞 회차의 합치기가 끝나기를 기다린다)을 받는 QEMU를
    /// 띄울 때까지 쥔다. 기록 디렉터리 잠금은 쥐지 않는다(남은 겹층 합치기와 받는 QEMU 띄우기 동안 다른 VM을 막지
    /// 않게 — [`replicas::vm_records`]).
    async fn prepare_locked(
        &self,
        req: ReplicaPrepareRequest,
    ) -> Result<ReplicaPrepareResponse, ReplicaError> {
        let c = &self.config;
        let vm_lock = replicas::lock_vm(&c.replica_dir, req.vm).await;
        let records = replicas::vm_records(&c.replica_dir, &vm_lock);
        let vm = self
            .check_target(self.read_vm(req.vm).map_err(ReplicaError::Failed)?, req.vm)
            .map_err(ReplicaError::Rejected)?;
        self.stop_other_receivers(vm.id, req.run)
            .await
            .map_err(ReplicaError::Failed)?;
        let mut record = self.settled_record(&records, &vm).await?;
        let refused = refused(&record, &req.volumes);
        if !refused.is_empty() {
            tracing::info!(vm = %vm.id, run = %req.run, ?refused, "refusing increments without a complete base replica");
            return Ok(ReplicaPrepareResponse {
                port: 0,
                exports: Vec::new(),
                refused,
            });
        }
        let mut made = Made::default();
        let started = self
            .prepare_exports(&records, &mut record, &req, &mut made)
            .await;
        let started = match started {
            Ok(exports) => self
                .hv
                .start_replica_receiver(req.run, &exports, &c.listen_host, c.ports.clone())
                .await
                .map(|port| (port, exports))
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        match started {
            Ok((port, exports)) => {
                tracing::info!(vm = %vm.id, run = %req.run, port, "receiving a replica");
                Ok(ReplicaPrepareResponse {
                    port,
                    exports: exports.into_iter().map(|e| e.name).collect(),
                    refused: Vec::new(),
                })
            }
            Err(error) => {
                self.undo(&records, &mut record, made).await;
                Err(ReplicaError::Failed(error))
            }
        }
    }

    /// 그 VM의 기록을 먼저 청소하고, 남은 겹층을 합치거나 버린 기록(앞 회차의 받는 QEMU는 부르는 쪽이 껐다). 기록이
    /// 없으면 빈 복제본 기록이다. 돌아온 옛 원본의 기록은 옛 복제본(`stale`)으로 바꾼다 — 이 노드의 옛 사본인 볼륨만 남기고
    /// 아직 이 노드의 진짜 디스크인 볼륨은 지우지 않고 뺀다([`demote`](Self::demote)). 부르는 쪽이 잠금을 쥔다.
    async fn settled_record(
        &self,
        records: &Records,
        vm: &Vm,
    ) -> Result<ReplicaRecord, ReplicaError> {
        let storage = &*self.config.storage;
        let record = match replicas::read(records.dir(), vm.id) {
            Ok(Some(record)) => {
                self.sweep_record(records, record).await;
                match replicas::read(records.dir(), vm.id) {
                    Ok(Some(record)) => Some(
                        replicas::settle(&self.cluster, storage, records, record, &[])
                            .await
                            .map_err(ReplicaError::Failed)?,
                    ),
                    Ok(None) => None,
                    Err(e) => return Err(record_error(e)),
                }
            }
            Ok(None) => None,
            Err(e) => return Err(record_error(e)),
        };
        let mut record = record.unwrap_or(ReplicaRecord {
            vm: vm.id,
            role: Role::Replica,
            generation: None,
            synced_at: None,
            volumes: Vec::new(),
        });
        // 돌아온 옛 원본: 이 노드의 디스크는 이제 옛것이라 전체로만 받는다.
        if record.role == Role::Primary {
            tracing::info!(vm = %vm.id, "this node was the VM's primary; its disks become stale replicas");
            self.demote(&mut record).map_err(ReplicaError::Failed)?;
            replicas::write(records.dir(), &record).map_err(record_error)?;
        }
        Ok(record)
    }

    /// 요청 디스크마다 풀·형식이 바뀐 옛 복제본을 지운다(새로 만든다).
    async fn drop_moved_replicas(
        &self,
        records: &Records,
        record: &mut ReplicaRecord,
        volumes: &[PrepareVolume],
    ) -> Result<(), String> {
        let storage = &*self.config.storage;
        for v in volumes {
            let stale = record
                .volumes
                .iter()
                .position(|r| r.id == v.id && (r.pool != v.pool || r.format != v.format));
            if let Some(i) = stale {
                let old = record.volumes[i].clone();
                tracing::info!(vm = %record.vm, volume = %v.id, pool = %old.pool, "replacing a replica kept in another pool");
                if !replicas::delete_replica(&self.cluster, storage, record.vm, &old).await {
                    return Err(format!(
                        "cannot delete the old replica of volume {} in pool {:?}",
                        v.id, old.pool
                    ));
                }
                record.volumes.remove(i);
                replicas::write(records.dir(), record).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// 이 노드로 오는 마이그레이션(Task `task`)의 계획에서 이 노드의 복제본으로 옮기는 디스크(`existing`, 계획 55)를
    /// 받을 준비를 한다: 그 VM의 잠금 안에서 받는 QEMU를 끄고 기록을 청소·정리한 뒤, 디스크마다 복제본이 없으면
    /// 같은 id로 만들고 작으면 늘려 `syncing`으로 적는다(따라잡기가 복제본에 바로 쓴다 — 마이그레이션이 되돌려지면 다음
    /// 복제 회차가 끝날 때까지 쓰지 않는다). 원본이 바뀐 블록만 보내려는 디스크(`base_synced_at`) 가운데 이 노드에 그
    /// 시점(또는 더 새것)의 온전한 복제본이 없는 것을 돌려준다(원본은 그 디스크를 전체로 보낸다). 실패하면 이 호출이
    /// 만든 복제본을 지운다.
    pub(crate) async fn prepare_migration(
        &self,
        vm: &Vm,
        task: Uuid,
        plan: &[DiskMove],
        disks: &[IncomingDisk],
    ) -> Result<Vec<Uuid>, String> {
        let volumes: Vec<PrepareVolume> = plan
            .iter()
            .zip(disks)
            .filter(|(m, _)| m.existing)
            .map(|(m, d)| PrepareVolume {
                id: m.volume,
                pool: m.pool.clone(),
                format: m.format,
                size_bytes: d.size_bytes,
                allocated_bytes: 0,
                mode: match d.base_synced_at {
                    Some(at) => PrepareMode::Incremental { base_synced_at: at },
                    None => PrepareMode::Full,
                },
            })
            .collect();
        if volumes.is_empty() {
            return Ok(Vec::new());
        }
        // VM 잠금만 쥔다: 남은 겹층 합치기와 복제본 만들기 동안 다른 VM을 막지 않는다.
        let vm_lock = replicas::lock_vm(&self.config.replica_dir, vm.id).await;
        let records = replicas::vm_records(&self.config.replica_dir, &vm_lock);
        self.stop_other_receivers(vm.id, task).await?;
        let mut record = self
            .settled_record(&records, vm)
            .await
            .map_err(|e| e.to_string())?;
        let refused = refused(&record, &volumes);
        let mut made = Made::default();
        let prepared = async {
            self.drop_moved_replicas(&records, &mut record, &volumes)
                .await?;
            for v in &volumes {
                let pool = self.pool(&v.pool)?;
                self.prepare_full(records.dir(), &mut record, &pool, v, &mut made)
                    .await?;
            }
            Ok::<(), String>(())
        }
        .await;
        if let Err(e) = prepared {
            self.undo(&records, &mut record, made).await;
            return Err(e);
        }
        if !refused.is_empty() {
            tracing::info!(vm = %vm.id, task = %task, ?refused, "the replicas cannot take only the changes; the source sends them in full");
        }
        Ok(refused)
    }

    /// 요청 디스크마다 복제본(전체)이나 겹층(증분)을 마련해 받는 QEMU가 내보낼 것을 돌려준다. 공간이 모자라면
    /// 아무것도 만들지 않고 실패한다.
    async fn prepare_exports(
        &self,
        records: &Records,
        record: &mut ReplicaRecord,
        req: &ReplicaPrepareRequest,
        made: &mut Made,
    ) -> Result<Vec<ReceiverExport>, String> {
        let c = &self.config;
        let dir = records.dir();
        // 풀·형식이 바뀐 옛 복제본은 지우고 새로 만든다.
        self.drop_moved_replicas(records, record, &req.volumes)
            .await?;
        self.check_space(record, req)?;
        let mut exports = Vec::new();
        for v in &req.volumes {
            let pool = self.pool(&v.pool)?;
            let overlay = match v.mode {
                PrepareMode::Full => {
                    self.prepare_full(dir, record, &pool, v, made).await?;
                    None
                }
                PrepareMode::Incremental { .. } => {
                    let path = replicas::overlay_path(dir, req.run, v.id);
                    std::fs::create_dir_all(replicas::overlay_dir(dir))
                        .map_err(|e| format!("cannot create the overlay directory: {e}"))?;
                    // 만들기 전에 적는다(만들다 죽어도 청소가 지운다).
                    let entry = entry_mut(record, v.id);
                    entry.overlay = Some(Overlay {
                        run: req.run,
                        path: path.clone(),
                        received: false,
                    });
                    replicas::write(dir, record).map_err(|e| e.to_string())?;
                    made.overlays.push(path.clone());
                    c.storage
                        .create_overlay(&pool, v.id, v.format, &path)
                        .await
                        .map_err(|e| {
                            format!(
                                "cannot create the increment overlay of volume {}: {e}",
                                v.id
                            )
                        })?;
                    Some(path)
                }
            };
            c.storage
                .activate_volume(&pool, v.id, v.format)
                .await
                .map_err(|e| format!("cannot activate the replica of volume {}: {e}", v.id))?;
            let source =
                disk_location(&pool, &c.mount_root, v.id, v.format).map_err(|e| e.to_string())?;
            exports.push(ReceiverExport {
                name: v.id.simple().to_string(),
                source,
                format: v.format,
                overlay,
            });
        }
        Ok(exports)
    }

    /// 새 복제본(풀마다 원본이 쓰는 바이트의 합)과 증분을 합친 만큼 늘 복제본(바뀐 바이트)이 이 노드가 알린 풀의 남은
    /// 공간에, 증분 겹층(바뀐 바이트 + 1 GiB)이 기록 디렉터리의 파일 시스템에 들어가는지 본다. 풀 공간을 알리지
    /// 않았으면 풀은 보지 않는다.
    fn check_space(
        &self,
        record: &ReplicaRecord,
        req: &ReplicaPrepareRequest,
    ) -> Result<(), String> {
        let c = &self.config;
        let mut need: BTreeMap<&str, u64> = BTreeMap::new();
        let mut merged: Vec<&str> = Vec::new();
        for v in &req.volumes {
            match v.mode {
                PrepareMode::Full if !record.volumes.iter().any(|r| r.id == v.id) => {
                    *need.entry(&v.pool).or_default() += v.allocated_bytes;
                }
                PrepareMode::Full => {}
                // 바뀐 바이트는 회차 전체의 합이라 풀마다 한 번 센다.
                PrepareMode::Incremental { .. } if !merged.contains(&v.pool.as_str()) => {
                    merged.push(&v.pool);
                    *need.entry(&v.pool).or_default() += req.dirty_bytes;
                }
                PrepareMode::Incremental { .. } => {}
            }
        }
        let node = self
            .cluster
            .get::<NodeSpec>(c.node)
            .map_err(|e| e.to_string())?;
        for (name, need) in need {
            let pool = self.pool(name)?;
            if let Some(node) = &node
                && let Some(state) = pool_state_on(&pool, node)
                && need > state.available_bytes
            {
                return Err(format!(
                    "not enough space in storage pool {name:?} on node {:?}: need {need}, available {}",
                    node.name, state.available_bytes
                ));
            }
        }
        if req
            .volumes
            .iter()
            .any(|v| matches!(v.mode, PrepareMode::Incremental { .. }))
        {
            let dir = replicas::overlay_dir(&c.replica_dir);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let available = replicas::free_bytes(&dir)
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
            let need = req.dirty_bytes.saturating_add(OVERLAY_HEADROOM);
            if need > available {
                return Err(format!(
                    "not enough space in {} on node {:?} for the increment: need {need}, available {available}",
                    dir.display(),
                    self.node_name()
                ));
            }
        }
        Ok(())
    }

    /// 전체 회차의 디스크 하나: 복제본이 없으면 같은 id로 만들고(만들기 전에 적는다), 작으면 늘린다. 항목은
    /// `syncing`이 된다(복제본에 바로 쓴다).
    async fn prepare_full(
        &self,
        dir: &std::path::Path,
        record: &mut ReplicaRecord,
        pool: &StoragePool,
        v: &PrepareVolume,
        made: &mut Made,
    ) -> Result<(), String> {
        let storage = &*self.config.storage;
        let write = |record: &ReplicaRecord| {
            replicas::write(dir, record)
                .map_err(|e| format!("cannot write the replica record: {e}"))
        };
        // 만들기 전에 적고 끝나지 않은 항목(만들다 죽음)은 다시 만든다(이미 있으면 남의 파일이다).
        record.volumes.retain(|r| r.id != v.id || r.created);
        if let Some(entry) = record.volumes.iter_mut().find(|r| r.id == v.id) {
            entry.state = ReplicaState::Syncing;
            write(record)?;
            // 만든 복제본 파일이 없어졌으면(손으로 지움 등) 같은 id로 다시 만든다.
            if let Err(DriverError::VolumeNotFound(_)) =
                storage.volume_info(pool, v.id, v.format).await
            {
                tracing::warn!(volume = %v.id, pool = %v.pool, "the replica file is gone; creating it again");
                storage
                    .create_volume(pool, v.id, v.size_bytes, v.format)
                    .await
                    .map_err(|e| {
                        format!(
                            "cannot create the replica of volume {} in pool {:?}: {e}",
                            v.id, v.pool
                        )
                    })?;
                made.volumes.push(entry_mut(record, v.id).clone());
            }
        } else {
            record.volumes.push(RecordedReplica {
                id: v.id,
                pool: v.pool.clone(),
                format: v.format,
                size_bytes: v.size_bytes,
                created: false,
                state: ReplicaState::Syncing,
                overlay: None,
            });
            write(record)?;
            match storage
                .create_volume(pool, v.id, v.size_bytes, v.format)
                .await
            {
                Ok(_) => {
                    let entry = entry_mut(record, v.id);
                    entry.created = true;
                    made.volumes.push(entry.clone());
                    write(record)?;
                }
                Err(DriverError::VolumeExists(_)) => {
                    record.volumes.retain(|r| r.id != v.id);
                    write(record)?;
                    return Err(format!(
                        "volume {} already exists on node {:?} in pool {:?} but is not a replica recorded by this node; is the pool really node-local?",
                        v.id,
                        self.node_name(),
                        v.pool
                    ));
                }
                Err(e) => {
                    // 만들다 만 파일이 남았을 수 있지만 만들었는지 모르므로 지우지 않는다.
                    tracing::warn!(volume = %v.id, pool = %v.pool, error = %e, "creating a replica failed; a partial file may be left");
                    return Err(format!(
                        "cannot create the replica of volume {} in pool {:?}: {e}",
                        v.id, v.pool
                    ));
                }
            }
        }
        let read = |e: DriverError| format!("cannot read the replica of volume {}: {e}", v.id);
        let mut size = storage
            .volume_info(pool, v.id, v.format)
            .await
            .map_err(read)?
            .virtual_bytes;
        if size < v.size_bytes {
            tracing::info!(volume = %v.id, from = size, to = v.size_bytes, "growing a replica");
            storage
                .resize_volume(pool, v.id, v.format, v.size_bytes)
                .await
                .map_err(|e| format!("cannot grow the replica of volume {}: {e}", v.id))?;
            size = storage
                .volume_info(pool, v.id, v.format)
                .await
                .map_err(read)?
                .virtual_bytes;
        }
        entry_mut(record, v.id).size_bytes = size;
        write(record)?;
        if size != v.size_bytes {
            return Err(format!(
                "the replica of volume {} would change size on pool {:?} ({} → {size} bytes)",
                v.id, v.pool, v.size_bytes
            ));
        }
        Ok(())
    }

    /// 실패한 준비를 되돌린다: 이 호출이 만든 복제본과 겹층을 지우고 기록에서 뺀다(기록이 비면 지운다).
    async fn undo(&self, records: &Records, record: &mut ReplicaRecord, made: Made) {
        let c = &self.config;
        for path in &made.overlays {
            if let Err(e) = replicas::discard_overlay(path) {
                tracing::warn!(path = %path.display(), error = %e, "cannot remove a replica increment");
            }
        }
        for v in &mut record.volumes {
            if v.overlay
                .as_ref()
                .is_some_and(|o| made.overlays.contains(&o.path))
            {
                v.overlay = None;
            }
        }
        for v in &made.volumes {
            if replicas::delete_replica(&self.cluster, &*c.storage, record.vm, v).await {
                record.volumes.retain(|r| r.id != v.id);
            }
        }
        let written = if record.volumes.is_empty() {
            replicas::remove(records.dir(), record.vm)
        } else {
            replicas::write(records.dir(), record)
        };
        if let Err(e) = written {
            tracing::warn!(vm = %record.vm, error = %e, "cannot write the replica record");
        }
    }

    /// [`finish`](ReplicaReceiver::finish)의 본문: 그 VM의 잠금을 쥐고 받는 QEMU를 닫은 뒤 받은 회차를 기록하고 곧바로
    /// 답한다. 받은 겹층은 RPC 밖에서 합친다([`merge_later`](Self::merge_later)). 기록 디렉터리 잠금은 기록을 읽고 쓰는
    /// 동안만 쥔다.
    async fn finish_locked(&self, req: ReplicaFinishRequest) -> Result<(), ReplicaError> {
        let c = &self.config;
        let _vm = replicas::lock_vm(&c.replica_dir, req.vm).await;
        let missing =
            || ReplicaError::Rejected(format!("no replica of VM {} on this node", req.vm));
        {
            let records = replicas::lock(&c.replica_dir).await;
            replicas::read(records.dir(), req.vm)
                .map_err(record_error)?
                .ok_or_else(missing)?;
        }
        let finished = if req.ok {
            self.hv
                .finish_replica_receiver(req.run)
                .await
                .map_err(|e| e.to_string())
        } else {
            Err("the source did not send every disk".to_string())
        };
        let records = replicas::lock(&c.replica_dir).await;
        let dir = records.dir();
        let mut record = replicas::read(dir, req.vm)
            .map_err(record_error)?
            .ok_or_else(missing)?;
        if let Err(why) = finished {
            // 회차를 버린다: 받는 QEMU를 끄고 받지 못한 겹층을 지운다(전체 복사한 복제본은 `syncing`으로 남는다).
            self.drop_run(&records, &mut record, req.run).await;
            if req.ok {
                return Err(ReplicaError::Failed(format!(
                    "cannot finish receiving the replica: {why}"
                )));
            }
            tracing::info!(vm = %req.vm, run = %req.run, "discarded a failed replication run");
            return Ok(());
        }
        // 받은 회차를 먼저 기록한다(그 뒤로는 합치기가 끊겨도 다시 하면 같아진다). 이 회차가 보낸 디스크만 온전해진다.
        let sent = |id: Uuid| req.volumes.is_empty() || req.volumes.contains(&id);
        for v in &mut record.volumes {
            if let Some(o) = v.overlay.as_mut().filter(|o| o.run == req.run) {
                o.received = true;
            }
            if v.state == ReplicaState::Syncing && sent(v.id) {
                v.state = ReplicaState::Ready;
            }
        }
        record.generation = Some(req.run);
        record.synced_at = Some(req.synced_at);
        replicas::write(dir, &record).map_err(record_error)?;
        tracing::info!(vm = %req.vm, run = %req.run, synced_at = req.synced_at, "received a replica");
        if record
            .volumes
            .iter()
            .any(|v| v.overlay.as_ref().is_some_and(|o| o.received))
        {
            self.merge_later(req.vm);
        }
        Ok(())
    }

    /// 받은 겹층을 RPC 밖에서 합친다: 그 VM의 잠금을 기다려 쥐고(같은 VM의 다음 받기·승격·청소가 합치기를 기다린다)
    /// 합치는 동안 기록 디렉터리 잠금은 쥐지 않는다(다른 VM의 받기를 막지 않는다). 합치지 못하면 받은 겹층이 기록에
    /// 남아 다음 받기·청소·승격이 먼저 합친다.
    fn merge_later(&self, vm: Uuid) {
        let this = self.clone();
        tokio::spawn(async move {
            let c = &this.config;
            let _vm = replicas::lock_vm(&c.replica_dir, vm).await;
            let merged =
                replicas::merge_received(&this.cluster, &*c.storage, &c.replica_dir, vm).await;
            if let Err(e) = merged {
                tracing::warn!(vm = %vm, error = %e, "cannot merge a received increment yet");
            }
        });
    }

    /// 회차 `run`을 버린다: 받는 QEMU를 끄고 그 회차의 받지 못한 겹층을 지운다.
    async fn drop_run(&self, records: &Records, record: &mut ReplicaRecord, run: Uuid) {
        if let Err(e) = self.hv.stop_replica_receiver(run).await {
            tracing::warn!(vm = %record.vm, run = %run, error = %e, "cannot stop the replica receiver");
        }
        let mut changed = false;
        for v in &mut record.volumes {
            let Some(o) = v.overlay.as_ref().filter(|o| o.run == run && !o.received) else {
                continue;
            };
            match replicas::discard_overlay(&o.path) {
                Ok(()) => {
                    v.overlay = None;
                    changed = true;
                }
                Err(e) => {
                    tracing::warn!(vm = %record.vm, path = %o.path.display(), error = %e, "cannot remove a replica increment")
                }
            }
        }
        if changed && let Err(e) = replicas::write(records.dir(), record) {
            tracing::warn!(vm = %record.vm, error = %e, "cannot write the replica record");
        }
    }

    /// [`abort`](ReplicaReceiver::abort)의 본문
    async fn abort_locked(&self, req: ReplicaAbortRequest) -> Result<(), ReplicaError> {
        let _vm = replicas::lock_vm(&self.config.replica_dir, req.vm).await;
        let records = replicas::lock(&self.config.replica_dir).await;
        match replicas::read(records.dir(), req.vm).map_err(record_error)? {
            Some(mut record) => self.drop_run(&records, &mut record, req.run).await,
            None => {
                self.hv
                    .stop_replica_receiver(req.run)
                    .await
                    .map_err(|e| ReplicaError::Failed(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// 이 노드의 복제본 기록을 시작할 때와 그 뒤 [`REPLICA_SWEEP_INTERVAL`]마다 청소한다([`sweep`](Self::sweep)).
    pub async fn sweep_every(self) {
        loop {
            self.sweep().await;
            tokio::time::sleep(REPLICA_SWEEP_INTERVAL).await;
        }
    }

    /// 끝났거나 없는 회차의 받는 QEMU를 끄고, 기록마다 청소 규칙대로 정리한다(설계 §9, 정한 것 4):
    /// - 복제본(`replica`): VM이 없으면 만든 복제본·겹층·기록을 지운다. VM이 이 노드로 왔고 볼륨 기록도 이 노드면
    ///   (승격 끝) `primary`로 바꾼다. 이 노드가 대상이 아니고 VM이 이 노드에 있지 않고 그 VM의 끝나지 않은
    ///   복제·마이그레이션 Task가 없으면 지운다. VM 디스크에 없는 볼륨(뗀 디스크)은 그 복제본만 지운다. 받은 겹층은
    ///   합치고, 끝난 회차의 받지 못한 겹층은 버린다.
    /// - 원본(`primary`): VM이 없으면 기록만 지운다(파일은 남은 볼륨 리소스의 것이다). 볼륨 기록이 다른 노드로
    ///   옮겨 갔고 VM이 이 노드에 있지 않고 끝나지 않은 마이그레이션이 없으면, 이 노드가 대상이면 옛
    ///   복제본(`stale`)으로 바꾸고 아니면 기록의 볼륨과 기록을 지운다.
    ///
    /// 원본 기록을 옛 복제본으로 바꾸거나 지울 때는 이 노드의 옛 사본인 볼륨([`stale_copy`](Self::stale_copy))만
    /// 지울 수 있게 남기고, 아직 이 노드의 진짜 디스크인 볼륨은 지우지 않고 기록에서 뺀다.
    ///
    /// 기록마다 그 VM의 잠금을 쥔다(합치는 중인 VM은 기다린다). 실패는 경고만 남긴다(다음 청소가 다시 한다).
    pub async fn sweep(&self) {
        let dir = &self.config.replica_dir;
        self.stop_finished_receivers().await;
        for listed in replicas::list(dir) {
            let _vm = replicas::lock_vm(dir, listed.vm).await;
            let records = replicas::lock(dir).await;
            // 잠금을 기다리는 동안 바뀌었을 수 있다.
            match replicas::read(records.dir(), listed.vm) {
                Ok(Some(record)) => self.sweep_record(&records, record).await,
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(vm = %listed.vm, error = %e, "cannot read a replica record")
                }
            }
        }
    }

    /// 회차의 Task가 끝났거나 없는 받는 QEMU를 끈다.
    async fn stop_finished_receivers(&self) {
        let runs = match self.hv.replica_receivers().await {
            Ok(runs) => runs,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list replica receivers");
                return;
            }
        };
        for run in runs {
            let live = match self.cluster.get::<TaskSpec>(run) {
                Ok(task) => task.is_some_and(|t| !t.status.state.is_finished()),
                Err(e) => {
                    tracing::warn!(run = %run, error = %e, "cannot read task");
                    continue;
                }
            };
            if live {
                continue;
            }
            tracing::info!(run = %run, "stopping the replica receiver of a finished run");
            if let Err(e) = self.hv.stop_replica_receiver(run).await {
                tracing::warn!(run = %run, error = %e, "cannot stop the replica receiver");
            }
        }
    }

    /// 기록 하나를 청소한다([`sweep`](Self::sweep)). 부르는 쪽이 잠금을 쥔다.
    async fn sweep_record(&self, records: &Records, record: ReplicaRecord) {
        let vm = record.vm;
        let read = || -> Result<_, String> {
            let current = self.read_vm(vm)?;
            let busy = self.unfinished_tasks(vm)?;
            let mut volumes = Vec::new();
            for v in &record.volumes {
                volumes.extend(
                    self.cluster
                        .get::<VolumeSpec>(v.id)
                        .map_err(|e| e.to_string())?,
                );
            }
            let used = match &current {
                Some(current) => self.vm_volumes(current)?,
                None => HashSet::new(),
            };
            Ok((current, busy, volumes, used))
        };
        let (current, busy, volumes, used) = match read() {
            Ok(read) => read,
            Err(e) => {
                tracing::warn!(vm = %vm, error = %e, "cannot read the VM of a replica record");
                return;
            }
        };
        let me = self.config.node;
        let node_of = |id: Uuid| {
            volumes
                .iter()
                .find(|v| v.id == id)
                .and_then(|v| v.status.node)
        };
        let target = current
            .as_ref()
            .is_some_and(|c| c.spec.replication.iter().any(|t| t.node == me));
        match record.role {
            Role::Replica => {
                let Some(current) = current else {
                    tracing::info!(vm = %vm, "deleting the replicas of a deleted VM");
                    self.delete_record(records, record).await;
                    return;
                };
                if current.spec.node == Some(me) {
                    // 승격이 끝났으면(볼륨 기록이 이 노드) 이 노드가 원본이다. 아니면 승격을 기다린다.
                    if record.volumes.iter().all(|v| node_of(v.id) == Some(me)) {
                        let mut record = record;
                        record.role = Role::Primary;
                        record.generation = None;
                        record.synced_at = None;
                        tracing::info!(vm = %vm, "the replicas on this node became the VM's disks");
                        write_or_warn(records, &record);
                    }
                    return;
                }
                if !target && busy.is_empty() {
                    tracing::info!(vm = %vm, "deleting the replicas of a VM no longer replicated to this node");
                    self.delete_record(records, record).await;
                    return;
                }
                let mut record = record;
                let (keep, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut record.volumes)
                    .into_iter()
                    .partition(|v| used.contains(&v.id));
                record.volumes = keep;
                for mut v in gone {
                    match self.stale_copy(vm, v.id) {
                        Ok(true) => {}
                        // 아직 이 노드의 진짜 디스크다(다른 VM에 붙였다 등): 지우지 않고 잊는다.
                        Ok(false) => v.created = false,
                        Err(e) => {
                            tracing::warn!(vm = %vm, volume = %v.id, error = %e, "cannot read the volume of a replica");
                            record.volumes.push(v);
                            continue;
                        }
                    }
                    tracing::info!(vm = %vm, volume = %v.id, "deleting the replica of a detached disk");
                    if !replicas::delete_replica(&self.cluster, &*self.config.storage, vm, &v).await
                    {
                        record.volumes.push(v);
                    }
                }
                write_or_warn(records, &record);
                if record.volumes.is_empty() {
                    if let Err(e) = replicas::remove(records.dir(), vm) {
                        tracing::warn!(vm = %vm, error = %e, "cannot remove a replica record");
                    }
                    return;
                }
                let storage = &*self.config.storage;
                if let Err(e) =
                    replicas::settle(&self.cluster, storage, records, record, &busy).await
                {
                    tracing::warn!(vm = %vm, error = %e, "cannot settle the replica increments");
                }
            }
            Role::Primary => {
                let Some(current) = current else {
                    if let Err(e) = replicas::remove(records.dir(), vm) {
                        tracing::warn!(vm = %vm, error = %e, "cannot remove a replica record");
                    }
                    return;
                };
                let moved = record
                    .volumes
                    .iter()
                    .any(|v| node_of(v.id).is_some_and(|n| n != me))
                    && record
                        .volumes
                        .iter()
                        .all(|v| node_of(v.id).is_none_or(|n| n != me));
                if !moved
                    || current.spec.node == Some(me)
                    || current.status.migration.is_some()
                    || !busy.is_empty()
                {
                    return;
                }
                let mut record = record;
                // 원본 노드의 기록 볼륨은 이 노드의 진짜 디스크였다. 옛 사본이 된 것만 지울 수 있게 남긴다.
                if let Err(e) = self.demote(&mut record) {
                    tracing::warn!(vm = %vm, error = %e, "cannot read the volumes of an old primary record");
                    return;
                }
                if target {
                    tracing::info!(vm = %vm, "the VM moved away; its disks on this node become stale replicas");
                    if record.volumes.is_empty() {
                        if let Err(e) = replicas::remove(records.dir(), vm) {
                            tracing::warn!(vm = %vm, error = %e, "cannot remove a replica record");
                        }
                    } else {
                        write_or_warn(records, &record);
                    }
                } else {
                    tracing::info!(vm = %vm, "the VM moved away; deleting its old disks on this node");
                    self.delete_record(records, record).await;
                }
            }
        }
    }

    /// 기록의 볼륨(만든 것만, 그 가운데 아직 이 노드의 진짜 디스크인 것은 빼고 — [`stale_copy`](Self::stale_copy))과
    /// 겹층을 지우고, 다 지웠으면 기록을 지운다(못 지운 것은 기록에 남긴다).
    async fn delete_record(&self, records: &Records, mut record: ReplicaRecord) {
        let storage = &*self.config.storage;
        let mut left = Vec::new();
        for mut v in std::mem::take(&mut record.volumes) {
            if v.created {
                match self.stale_copy(record.vm, v.id) {
                    Ok(true) => {}
                    Ok(false) => v.created = false,
                    Err(e) => {
                        tracing::warn!(vm = %record.vm, volume = %v.id, error = %e, "cannot read the volume of a replica");
                        left.push(v);
                        continue;
                    }
                }
            }
            if !replicas::delete_replica(&self.cluster, storage, record.vm, &v).await {
                left.push(v);
            }
        }
        record.volumes = left;
        let written = if record.volumes.is_empty() {
            replicas::remove(records.dir(), record.vm)
        } else {
            replicas::write(records.dir(), &record)
        };
        if let Err(e) = written {
            tracing::warn!(vm = %record.vm, error = %e, "cannot write the replica record");
        }
    }

    /// 이 노드의 볼륨 파일 `id`(VM `vm`의 기록)가 옛 사본이라 이 노드가 지워도 되는지: 그 볼륨 리소스가 없거나 이제
    /// 다른 노드에 있고(`status.node`), 다른 VM이 쓰지 않는다(`vm_uses_volume`). 아니면 그 파일은 이 노드의 진짜
    /// 디스크일 수 있다(원본이 떼어 다른 VM에 붙인 디스크, 노드를 모르는 볼륨 등).
    fn stale_copy(&self, vm: Uuid, id: Uuid) -> Result<bool, String> {
        let me = self.config.node;
        let Some(volume) = self
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
        else {
            return Ok(true);
        };
        if volume.status.node.is_none_or(|n| n == me) {
            return Ok(false);
        }
        let (_, vms) = self.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        Ok(!vms
            .iter()
            .any(|v| v.id != vm && vm_uses_volume(v, &volume.name)))
    }

    /// 원본 기록(`primary`)을 옛 복제본(`stale`)으로 바꾼다: 이 노드의 옛 사본인 볼륨([`stale_copy`](Self::stale_copy))만
    /// 이 노드가 지울 수 있게(`created`) 남기고, 그 밖의 볼륨(아직 이 노드의 진짜 디스크)은 지우지 않고 기록에서 뺀다.
    fn demote(&self, record: &mut ReplicaRecord) -> Result<(), String> {
        let mut stale = Vec::new();
        for v in &record.volumes {
            stale.push(self.stale_copy(record.vm, v.id)?);
        }
        let volumes = std::mem::take(&mut record.volumes);
        for (mut v, stale) in volumes.into_iter().zip(stale) {
            if stale {
                v.created = true;
                v.state = ReplicaState::Stale;
                record.volumes.push(v);
            } else {
                tracing::info!(vm = %record.vm, volume = %v.id, "leaving out a disk that is still a volume on this node");
            }
        }
        record.role = Role::Replica;
        record.generation = None;
        record.synced_at = None;
        Ok(())
    }

    /// VM이 쓰는 볼륨(스펙의 디스크와 도는 QEMU에 아직 붙은 디스크)의 id
    fn vm_volumes(&self, vm: &Vm) -> Result<HashSet<Uuid>, String> {
        let names = vm.spec.disks.iter().map(|d| &d.volume).chain(
            vm.status
                .hardware
                .iter()
                .flat_map(|hw| hw.disks.iter().map(|d| &d.volume)),
        );
        let mut ids = HashSet::new();
        for name in names {
            if let Some(v) = self
                .cluster
                .get_by_name::<VolumeSpec>(name)
                .map_err(|e| e.to_string())?
            {
                ids.insert(v.id);
            }
        }
        Ok(ids)
    }
}

/// 증분을 받을 수 없는 디스크: 이 노드의 복제본이 온전하지 않거나(`ready`가 아님, 같은 풀·형식이 아님) 기록의
/// 시점이 원본의 기준 시점보다 옛것이다(기록을 잃었다 등).
fn refused(record: &ReplicaRecord, volumes: &[PrepareVolume]) -> Vec<Uuid> {
    volumes
        .iter()
        .filter_map(|v| {
            let PrepareMode::Incremental { base_synced_at } = v.mode else {
                return None;
            };
            let ok = record.volumes.iter().any(|r| {
                r.id == v.id
                    && r.pool == v.pool
                    && r.format == v.format
                    && r.state == ReplicaState::Ready
                    && r.size_bytes == v.size_bytes
            }) && record.synced_at.is_some_and(|at| at >= base_synced_at);
            (!ok).then_some(v.id)
        })
        .collect()
}

fn entry_mut(record: &mut ReplicaRecord, id: Uuid) -> &mut RecordedReplica {
    record
        .volumes
        .iter_mut()
        .find(|r| r.id == id)
        .expect("the replica is recorded")
}

fn record_error(e: std::io::Error) -> ReplicaError {
    ReplicaError::Failed(format!("cannot read or write the replica record: {e}"))
}

fn write_or_warn(records: &Records, record: &ReplicaRecord) {
    if let Err(e) = replicas::write(records.dir(), record) {
        tracing::warn!(vm = %record.vm, error = %e, "cannot write the replica record");
    }
}

#[async_trait]
impl ReplicaReceiver for IncomingReplicas {
    /// 모든 노드가 복제를 알고, 원본이 만든 회차 Task가 보이고, VM이 이 노드를 대상으로 두고 다른 노드에서
    /// 돌면 받는다. 증분 디스크의 기준 회차가 이 노드에 없으면 아무것도 하지 않고 그 디스크를 거절한다. 원본의
    /// RPC가 끊겨도 끝까지 하도록 따로 돈다.
    async fn prepare(
        &self,
        req: ReplicaPrepareRequest,
    ) -> Result<ReplicaPrepareResponse, ReplicaError> {
        let old = missing_feature(&self.cluster, NODE_REPLICATION)
            .map_err(|e| ReplicaError::Failed(format!("cannot read nodes: {e}")))?;
        if !old.is_empty() {
            return Err(ReplicaError::Rejected(format!(
                "some nodes do not support node replication yet ({}); upgrade them first",
                old.join(", ")
            )));
        }
        self.wait_task(&req).await?;
        let this = self.clone();
        tokio::spawn(async move { this.prepare_locked(req).await })
            .await
            .map_err(|e| ReplicaError::Failed(format!("preparing the replica failed: {e}")))?
    }

    async fn finish(&self, req: ReplicaFinishRequest) -> Result<(), ReplicaError> {
        let this = self.clone();
        tokio::spawn(async move { this.finish_locked(req).await })
            .await
            .map_err(|e| ReplicaError::Failed(format!("finishing the replica failed: {e}")))?
    }

    async fn abort(&self, req: ReplicaAbortRequest) -> Result<(), ReplicaError> {
        let this = self.clone();
        tokio::spawn(async move { this.abort_locked(req).await })
            .await
            .map_err(|e| ReplicaError::Failed(format!("aborting the replica failed: {e}")))?
    }
}
