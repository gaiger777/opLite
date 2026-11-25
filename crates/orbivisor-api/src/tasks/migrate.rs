//! 라이브 마이그레이션의 원본 쪽 Task(설계 §2의 2~8단계와 실패 되돌리기)와 남은 마이그레이션 QEMU
//! 정리(설계 §2 "중간에 죽으면"). 노드 로컬 풀의 디스크는 대상 노드에 새로 만든 볼륨으로 미러해
//! 복사하고, 확정 뒤 볼륨 기록을 옮기고 원본을 지운다(로컬 디스크 마이그레이션 설계 §3.3~§3.7). 대상 노드가 VM의 복제
//! 대상이면(계획 55) 디스크를 미러하지 않고 대상의 복제본에 복제 뒤 바뀐 블록만 몇 차례 보낸 뒤(따라잡기) 스위치오버
//! 직전에 마지막으로 바뀐 것을 보낸다. 확정하면 복제 방향을 뒤집고(대상 → 이 노드) 이 노드의 원본은 복제본으로 남긴다.
//!
//! 두 QEMU가 함께 도는 일은 없다: 대상 QEMU는 멈춘 채 받고 확정(`spec.node`를 대상으로 쓰기) 뒤에만
//! 대상 노드의 reconciler가 이어서 실행한다. 원본은 확정되지 않았다고 확인한 뒤에만 이어서 실행한다.
//! 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용된다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::{NodeId, Rejection};
use orbivisor_driver::{
    CatchUp, CatchUpState, MigrateOptions, MigrateTarget, MirrorOptions, MirrorProgress,
    MirrorTarget, RuntimeStatus,
};
use orbivisor_peer::{IncomingDisk, IncomingMigration, MigrateIncomingRequest, Peers};
use orbivisor_reconciler::ReconcilerHandle;
use orbivisor_reconciler::replicas::{self, RecordedReplica, ReplicaRecord, ReplicaState};
use orbivisor_reconciler::resolve::{apply_disk_moves, set_disks_shared};
use orbivisor_types::hotplug;
use orbivisor_types::node::NodeSpec;
use orbivisor_types::numa::HostPlacement;
use orbivisor_types::replication::{
    ReplicaStatus, ReplicaVolume, ReplicationState, ReplicationTarget, replica_bitmap,
};
use orbivisor_types::storage::{StoragePoolSpec, VolumeSpec, pool_state_on};
use orbivisor_types::task::{Task, TaskOp, TaskProgress, TaskSpec, TaskState, TaskStatus};
use orbivisor_types::vm::{DiskMove, Firmware, MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::watch;
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::backups::now;
use crate::incoming::deactivate_disks;
use crate::migration_disks::{self, DiskRecord, RecordedVolume, Role};
use crate::scheduler::{Inventory, migration_moves};

/// 확정 쓰기가 (과반수가 없는 등으로) 실패할 때 다시 시도하는 시간. 지나면 되돌린다.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 확정 뒤 status(`Running`, 대상 노드)를 쓰다 실패할 때 다시 시도하는 시간. 지나면 대상 노드의
/// reconciler가 넘겨받으며 마이그레이션 표시를 지우므로 그대로 둔다.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

/// 디스크 복사량의 합이 이 시간 동안 늘지 않으면 실패한다(끊긴 링크를 일찍 잡는다).
pub(super) const DISK_STALL: Duration = Duration::from_secs(10 * 60);

/// 따라잡기(계획 55)는 패스 뒤 바뀐 바이트가 이 이하면 멈추고 스위치오버의 마지막 패스로 간다.
const CATCH_UP_LIMIT: u64 = 64 << 20;

/// 따라잡기 패스 수의 최댓값(쓰기가 계속 많아도 이만큼 뒤에는 스위치오버로 간다)
const CATCH_UP_PASSES: u8 = 5;

/// 디스크 복사의 진행률을 Task에 쓰는 최소 간격(바뀌었을 때만 쓴다)
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

/// 실행 중인 VM을 `target` 노드로 옮긴다. 노드 로컬 풀의 디스크는 `pool`(없으면 같은 이름의 풀)에
/// 복사한다. 확정 전에 실패하면 원본을 계속 돌리고 실패한다.
pub(super) async fn migrate(
    ctx: &Ctx,
    task: &Task,
    target: Uuid,
    pool: Option<&str>,
) -> Result<(), String> {
    let id = task.spec.target;
    let peers = ctx
        .config
        .peers
        .clone()
        .ok_or("this node cannot reach other nodes")?;
    let addr = ctx
        .cluster
        .members()
        .addr(NodeId(target))
        .map(str::to_string)
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let migration = MigrationStatus {
        node: target,
        task: task.id,
        disks: plan_disks(ctx, id, target, pool)?,
    };
    // 대상의 복제본으로 옮기는 디스크의 이 노드 원본(확정 뒤 이 노드의 복제본으로 남는다)
    let kept = kept_volumes(ctx, &migration).await?;
    let _hold = Hold::begin(&ctx.reconciler, id).await;
    let vm = match mark_migrating(ctx, id, &migration).await {
        Ok(vm) => vm,
        Err(Mark::Refused(e)) => return Err(e),
        // 기록됐을 수도 있다.
        Err(Mark::Unknown(e)) => {
            roll_back(ctx, &peers, &addr, id, &migration).await;
            return Err(e);
        }
    };
    // API 검사 뒤 기록 전에 들어온 Task를 막는다. 기록 뒤의 요청은 API가 거절한다.
    let sent = match check_no_other_task(ctx, &vm) {
        Err(e) => Err(e),
        // 대상 노드가 받는 동안 디스크를 함께 연다(공유 LVM은 LV 잠금을 공유로).
        // 로컬 풀의 디스크는 할 일이 없다(공유 LVM만 잠금을 바꾼다).
        Ok(()) => match set_disks_shared(&ctx.cluster, &vm, &*ctx.storage, true).await {
            Ok(()) => send(ctx, &peers, &addr, &vm, &migration).await,
            Err(e) => Err(format!("cannot share the disks with the target node: {e}")),
        },
    };
    // 보내기가 끝난 때가 스위치오버 시점이다(남긴 복제본의 시점).
    let mut switched = None;
    let mut placement = None;
    let error = match sent {
        Ok(target_placement) => {
            placement = target_placement;
            switched = Some(now());
            match record_kept(ctx, id, &kept).await {
                Ok(()) => commit(ctx, id, &migration, &kept).await.err(),
                Err(e) => Some(e),
            }
        }
        Err(e) => Some(e),
    };
    if let Some(error) = error {
        match roll_back(ctx, &peers, &addr, id, &migration).await {
            Outcome::RolledBack(None) => return Err(error),
            Outcome::RolledBack(Some(resume)) => {
                return Err(format!(
                    "{error}; the source VM could not be resumed: {resume}"
                ));
            }
            // 실패로 본 확정 쓰기가 적용됐다.
            Outcome::Committed => {}
        }
    }
    finish(
        ctx,
        id,
        &migration,
        &kept,
        switched.unwrap_or_else(now),
        placement,
    )
    .await;
    Ok(())
}

/// `begin_migration`을 끝에(실패, 버려진 future 포함) 푼다. 버려져서 풀리면 작업자가 opld 재시작
/// 때처럼 남은 마이그레이션을 되돌린다.
struct Hold {
    reconciler: ReconcilerHandle,
    id: Uuid,
}

impl Hold {
    async fn begin(reconciler: &ReconcilerHandle, id: Uuid) -> Self {
        reconciler.begin_migration(id).await;
        Self {
            reconciler: reconciler.clone(),
            id,
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.reconciler.end_migration(self.id);
    }
}

/// `host:port`(IPv6는 `[addr]:port`)의 호스트
pub(super) fn addr_host(addr: &str) -> Option<&str> {
    let (host, port) = addr.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// 복사할 디스크 계획(설계 §3.2): 지금의 VM·볼륨·풀 기록과 `pool`로 다시 만든다(API 검사 뒤의 변경을
/// 따른다). 대상이 복제 대상이면 그 복제본으로의 이동이다([`migration_moves`]).
fn plan_disks(
    ctx: &Ctx,
    id: Uuid,
    target: Uuid,
    pool: Option<&str>,
) -> Result<Vec<DiskMove>, String> {
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let vm = ctx
        .cluster
        .get::<VmSpec>(id)
        .map_err(read)?
        .ok_or("VM was deleted")?;
    let inv = Inventory::load(&ctx.cluster).map_err(read)?;
    let pool = match pool {
        Some(name) => Some(
            inv.pools
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| format!("storage pool {name:?} not found"))?,
        ),
        None => None,
    };
    migration_moves(&vm, &inv, target, pool)
}

/// 계획에서 대상의 복제본으로 옮기는 디스크(`existing`)의 이 노드 원본: 지금 볼륨 기록의 풀·형식과 이미지의 가상 크기(받는
/// 쪽 기록의 척도 — 다음 역방향 회차의 크기와 견준다). 확정 뒤 이 노드의 복제본으로 남는다(지우지 않는다).
async fn kept_volumes(
    ctx: &Ctx,
    migration: &MigrationStatus,
) -> Result<Vec<RecordedReplica>, String> {
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let mut kept = Vec::new();
    for m in migration.disks.iter().filter(|m| m.existing) {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(m.volume)
            .map_err(read)?
            .ok_or_else(|| format!("volume {} was deleted", m.volume))?;
        let pool = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
            .map_err(read)?
            .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
        let info = ctx
            .storage
            .volume_info(&pool, volume.id, volume.spec.format)
            .await
            .map_err(|e| format!("cannot read disk {:?}: {e}", volume.name))?;
        kept.push(RecordedReplica {
            id: volume.id,
            pool: volume.spec.pool.clone(),
            format: volume.spec.format,
            size_bytes: info.virtual_bytes,
            created: true,
            state: ReplicaState::Ready,
            overlay: None,
        });
    }
    Ok(kept)
}

/// 확정 쓰기 전에 대상의 복제본으로 옮기는 디스크(`kept`)를 이 노드의 원본 기록에 적는다: 확정 뒤 이 노드의 opld가 끝내기
/// 전에 죽어도 청소가 그 디스크를 옛 복제본으로 알아보게 한다(기록이 없으면 다음 역방향 회차가 남의 볼륨으로 보고
/// 거절한다). 확정이 실패해도 그대로 이 노드의 디스크이므로 해가 없다. 끝내기는 이 기록을 복제본 기록으로 바꾼다.
async fn record_kept(ctx: &Ctx, id: Uuid, kept: &[RecordedReplica]) -> Result<(), String> {
    let Some(dir) = ctx.config.replica_dir.as_ref().filter(|_| !kept.is_empty()) else {
        return Ok(());
    };
    let _vm = replicas::lock_vm(dir, id).await;
    let records = replicas::lock(dir).await;
    let fail = |e: std::io::Error| format!("cannot record the disks to keep on this node: {e}");
    let mut record = match replicas::read(records.dir(), id).map_err(fail)? {
        Some(record) if record.role == replicas::Role::Primary => record,
        _ => ReplicaRecord {
            vm: id,
            role: replicas::Role::Primary,
            generation: None,
            synced_at: None,
            volumes: Vec::new(),
        },
    };
    for k in kept {
        if !record.volumes.iter().any(|v| v.id == k.id) {
            // 아직 이 노드의 진짜 디스크다.
            record.volumes.push(RecordedReplica {
                created: false,
                ..k.clone()
            });
        }
    }
    replicas::write(records.dir(), &record).map_err(fail)
}

/// 대상 노드 `node`로의 쓸 수 있는 복제 항목(`ready`이고 끝난 회차가 있음)
fn ready_entry(vm: &Vm, node: Uuid) -> Option<&ReplicaStatus> {
    vm.status
        .replication
        .iter()
        .find(|r| r.node == node && r.state == ReplicationState::Ready && r.generation.is_some())
}

enum Mark {
    /// 기록하지 않았다.
    Refused(String),
    /// 기록됐는지 모른다.
    Unknown(String),
}

/// 2단계: 이 노드에서 실행 중인 VM인지 다시 보고 `Migrating`과 대상을 기록한다. 기록한 VM(읽은 것)을
/// 돌려준다.
async fn mark_migrating(ctx: &Ctx, id: Uuid, migration: &MigrationStatus) -> Result<Vm, Mark> {
    let refused = |e: String| Mark::Refused(e);
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| refused(e.to_string()))?
            .ok_or_else(|| refused("VM was deleted".into()))?;
        if vm.spec.node != Some(ctx.config.node) {
            return Err(refused(format!(
                "VM {:?} is no longer on this node",
                vm.name
            )));
        }
        let running = vm.spec.power == PowerState::Running && vm.status.phase == VmPhase::Running;
        let state = ctx
            .hv
            .inspect(id)
            .await
            .map_err(|e| refused(e.to_string()))?;
        if !running || state.is_none_or(|s| s.status != RuntimeStatus::Running) {
            return Err(refused(format!("VM {:?} is not running", vm.name)));
        }
        let mut status = vm.status.clone();
        status.phase = VmPhase::Migrating;
        status.migration = Some(migration.clone());
        // 따라잡기가 대상의 복제본에 바로 쓰므로 다음 복제 회차가 끝날 때까지 HA가 그 복제본을 쓰지 않는다.
        if migration.disks.iter().any(|m| m.existing) {
            for r in status
                .replication
                .iter_mut()
                .filter(|r| r.node == migration.node)
            {
                if r.state == ReplicationState::Ready {
                    r.state = ReplicationState::Syncing;
                }
            }
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(vm),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(Mark::Unknown(format!("cannot record the migration: {e}"))),
        }
    }
}

/// 마이그레이션을 기록한 뒤 다시 본다: 이 VM의 스냅샷 만들기·되돌리기·복제·백업·핫플러그 Task나 디스크의 크기
/// 늘리기 Task가 끝나지 않았으면 실패다. API는 마이그레이션 Task와 이것들을 함께 받지 않지만, 검사와
/// 기록 사이에 들어온 요청은 지나갔을 수 있다. 같은 VM의 줄에 앞서 선 Task는 이미 끝났다.
fn check_no_other_task(ctx: &Ctx, vm: &Vm) -> Result<(), String> {
    let read = |e: ClusterError| format!("cannot read tasks: {e}");
    let mut disks = Vec::new();
    for disk in &vm.spec.disks {
        if let Some(volume) = ctx
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .map_err(read)?
        {
            disks.push(volume.id);
        }
    }
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(read)?;
    for t in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let doing = match t.spec.op {
            TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::VmClone { .. }
            | TaskOp::VmBackup { .. }
                if t.spec.target == vm.id =>
            {
                "being snapshotted, reverted, cloned or backed up"
            }
            TaskOp::VolumeResize { .. } if disks.contains(&t.spec.target) => {
                "having a disk resized"
            }
            TaskOp::VmHotplug if t.spec.target == vm.id => "having its hardware changed",
            _ => continue,
        };
        return Err(format!(
            "VM {:?} is {doing} (task {}); try again when the task has finished",
            vm.name, t.id
        ));
    }
    Ok(())
}

/// 3~5단계: 대상 노드에 받을 준비를 시키고(복사할 디스크가 있으면 기록하고 공간을 본 뒤 대상에 만들게
/// 하고 미러로 복사한다), 보내고, 대상이 다 받았는지 확인한다.
async fn send(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    vm: &Vm,
    migration: &MigrationStatus,
) -> Result<Option<HostPlacement>, String> {
    let id = vm.id;
    // 받는 쪽은 스펙의 디스크·NIC와 하드웨어 기록의 배치로 켠다. 원본 QEMU가 다르면 받는 쪽을 켜거나 디스크를 복사하기
    // 전에 멈춘다(보내기 직전에도 다시 본다).
    let devices = hotplug::qemu_devices(&vm.spec, vm.status.hardware.as_ref());
    ctx.hv
        .check_devices(id, &devices)
        .await
        .map_err(|e| e.to_string())?;
    let disks = if migration.disks.is_empty() {
        Vec::new()
    } else {
        prepare_copy(ctx, vm, migration).await?
    };
    let qemu_version = ctx
        .hv
        .running_qemu_version(id)
        .await
        .map_err(|e| format!("cannot read the QEMU version: {e}"))?;
    let req = MigrateIncomingRequest {
        vm: id,
        task: migration.task,
        qemu_version,
        disks: disks.clone(),
    };
    let reply = peers
        .migrate_incoming(addr, req)
        .await
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    // 대상 노드에는 받을 준비 요청이 닿은 클러스터 링크(그 노드에 지금 쓰는 링크; 첫 링크가 끊겼으면 다른 링크)의
    // 호스트로 접속한다. 요청 전에 읽으면 아직 모르는 죽은 첫 링크일 수 있다. 모든 링크의 이름이 노드 인증서에 있다.
    let link = peers.active_link(addr);
    let host =
        addr_host(&link).ok_or_else(|| format!("cannot tell the host of node address {link:?}"))?;
    let (copied, catch_up) = if migration.disks.is_empty() {
        (None, None)
    } else {
        let nbd = reply
            .nbd_port
            .ok_or("the target node did not export the disks to copy")?;
        let (copied, catch_up) =
            copy_disks(ctx, vm, migration, host, nbd, &disks, &reply.refused).await?;
        (Some(copied), catch_up)
    };
    let target = MigrateTarget {
        host: host.to_string(),
        port: reply.port,
        tls_hostname: host.to_string(),
    };
    let opts = MigrateOptions {
        bandwidth: ctx.config.migration_bandwidth,
        timeout: ctx.config.migration_timeout,
        switchover_mirrors: copied.is_some() && catch_up.is_none(),
        switchover_catch_up: catch_up,
        // 그 사이 핫플러그가 없었는지 보내기 직전에 다시 본다.
        devices: Some(devices),
    };
    if let Some(copied) = copied {
        write_progress(ctx, migration.task, "migrating-memory", copied).await;
    }
    ctx.hv
        .migrate(id, &target, &opts)
        .await
        .map_err(|e| e.to_string())?;
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    peers
        .migrate_incoming_wait(addr, incoming)
        .await
        .map_err(|e| format!("the target node did not receive the VM: {e}"))?;
    Ok(reply.host_placement)
}

/// 복사할 디스크의 원본(풀·id·형식)을 이 노드의 기록 파일(`source`)에 쓰고, 대상 풀마다 할당량의 합이
/// 대상 노드가 알린 남은 공간 이하인지 본다(알리지 않았으면 보지 않는다). 대상에 만들 디스크(원본의
/// 가상 크기)를 돌려준다.
async fn prepare_copy(
    ctx: &Ctx,
    vm: &Vm,
    migration: &MigrationStatus,
) -> Result<Vec<IncomingDisk>, String> {
    let dir = ctx
        .config
        .migration_disks
        .as_deref()
        .ok_or("this node cannot record the disks to copy")?;
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let mut record = DiskRecord {
        vm: vm.id,
        task: migration.task,
        role: Role::Source,
        volumes: Vec::new(),
    };
    let mut disks = Vec::new();
    let mut need: BTreeMap<&str, u64> = BTreeMap::new();
    // 복제본으로 옮기는 디스크: 그 대상의 복제 항목과 디스크마다의 복제 비트맵
    let entry = ready_entry(vm, migration.node);
    let bitmaps = if migration.disks.iter().any(|m| m.existing) {
        ctx.hv
            .backup_bitmaps(vm.id)
            .await
            .map_err(|e| format!("cannot read the dirty bitmaps: {e}"))?
    } else {
        Vec::new()
    };
    for m in &migration.disks {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(m.volume)
            .map_err(read)?
            .ok_or_else(|| format!("volume {} was deleted", m.volume))?;
        let pool = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
            .map_err(read)?
            .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
        let info = ctx
            .storage
            .volume_info(&pool, volume.id, volume.spec.format)
            .await
            .map_err(|e| format!("cannot read disk {:?}: {e}", volume.name))?;
        let held = entry.and_then(|e| e.volumes.iter().find(|v| v.id == volume.id));
        // 복제본으로 옮기는 디스크의 원본은 확정 뒤에도 이 노드의 복제본으로 남으므로 지울 원본으로 적지 않는다.
        if !m.existing {
            record.volumes.push(RecordedVolume {
                id: volume.id,
                pool: volume.spec.pool.clone(),
                format: volume.spec.format,
                created: false,
            });
        }
        let base_synced_at = if m.existing {
            let index = vm.spec.disks.iter().position(|d| d.volume == volume.name);
            let disk_bitmaps = index.and_then(|i| bitmaps.get(i));
            // 복제 항목은 볼륨 크기(`spec.size_bytes`)를 적는다(받는 쪽에 만들 디스크는 이미지의 가상 크기).
            match replica_base(entry, held, volume.spec.size_bytes, disk_bitmaps) {
                Ok(at) => Some(at),
                Err(why) => {
                    tracing::info!(vm = %vm.name, volume = %volume.name, reason = %why, "copying disk {:?} in full: {why}", volume.name);
                    None
                }
            }
        } else {
            None
        };
        disks.push(IncomingDisk {
            volume: volume.id,
            size_bytes: info.virtual_bytes,
            base_synced_at,
        });
        // 이미 있는 복제본은 대상의 공간을 이미 쓴다.
        if !m.existing || held.is_none() {
            *need.entry(&m.pool).or_default() += info.allocated_bytes;
        }
    }
    migration_disks::write(dir, &record)
        .map_err(|e| format!("cannot record the disks to copy: {e}"))?;
    let node = ctx.cluster.get::<NodeSpec>(migration.node).map_err(read)?;
    for (name, need) in need {
        let pool = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(name)
            .map_err(read)?;
        if let (Some(node), Some(pool)) = (&node, pool)
            && let Some(state) = pool_state_on(&pool, node)
            && need > state.available_bytes
        {
            return Err(format!(
                "not enough space in storage pool {name:?} on node {:?}: need {need}, available {}",
                node.name, state.available_bytes
            ));
        }
    }
    Ok(disks)
}

/// 복제본으로 옮기는 디스크가 복제 뒤 바뀐 블록만 보낼 수 있으면 그 기준 회차의 시점: 대상의 항목이 `ready`이고 그 볼륨을
/// 같은 크기로 담았고, 그 회차의 복제 비트맵이 디스크에 있고 기록 중이며 inconsistent가 아니다. 아니면 전체로 보내는
/// 이유다.
fn replica_base(
    entry: Option<&ReplicaStatus>,
    held: Option<&ReplicaVolume>,
    size_bytes: u64,
    bitmaps: Option<&orbivisor_driver::DiskBitmaps>,
) -> Result<u64, String> {
    let Some((entry, held)) = entry.zip(held) else {
        return Err("no complete replica".into());
    };
    if held.size_bytes != size_bytes {
        return Err("the disk changed size".into());
    }
    let (Some(generation), Some(at)) = (entry.generation, entry.synced_at) else {
        return Err("no complete replica".into());
    };
    let name = replica_bitmap(entry.node, generation);
    let found = bitmaps.and_then(|d| d.bitmaps.iter().find(|b| b.name == name));
    match found {
        Some(b) if b.recording && !b.inconsistent => Ok(at),
        _ => Err("no dirty bitmap of the replica".into()),
    }
}

/// 복사할 디스크마다 대상 노드의 NBD 내보내기(`disk{i}`)로 미러를 시작하고 모두 따라잡기를 기다린다
/// (설계 §3.4). 대상의 복제본으로 옮기는 디스크가 있으면(계획 55) 미러 대신 따라잡는다: 기준 시점을 보낸 디스크
/// (`disks`의 `base_synced_at`, 대상이 거절한 `refused`는 빼고)는 복제 비트맵 뒤 바뀐 블록부터, 나머지는 전체부터 패스를
/// 거듭해 바뀐 것이 적어지면 멈춘다(마지막 패스는 스위치오버에서). 대역폭 상한은 디스크 수로 나눈다. 그동안 진행률을
/// Task에 쓴다. 복사량의 마지막 값과 따라잡기의 상태를 돌려준다. 실패하면 드라이버가 미러·따라잡기를 치운다.
async fn copy_disks(
    ctx: &Ctx,
    vm: &Vm,
    migration: &MigrationStatus,
    host: &str,
    nbd: u16,
    disks: &[IncomingDisk],
    refused: &[Uuid],
) -> Result<(MirrorProgress, Option<CatchUpState>), String> {
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let mut targets = Vec::new();
    let mut volumes = Vec::new();
    for (i, disk) in vm.spec.disks.iter().enumerate() {
        let volume = ctx
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .map_err(read)?;
        let Some(m) = volume.and_then(|v| migration.disks.iter().find(|m| m.volume == v.id)) else {
            continue;
        };
        targets.push(MirrorTarget {
            disk: i,
            host: host.to_string(),
            port: nbd,
            export: format!("disk{i}"),
            tls_hostname: host.to_string(),
            format: m.format,
        });
        volumes.push(m.volume);
    }
    if migration.disks.iter().any(|m| m.existing) {
        return catch_up(ctx, vm, migration, targets, &volumes, disks, refused).await;
    }
    let (tx, rx) = watch::channel(MirrorProgress::default());
    let opts = MirrorOptions {
        speed: ctx
            .config
            .migration_bandwidth
            .map(|b| b / targets.len().max(1) as u64),
        timeout: ctx.config.migration_disk_timeout,
        stall: DISK_STALL,
        progress: Some(tx),
    };
    // 미러가 끝나면 opts(보내는 쪽)가 버려져 진행률 쓰기도 끝난다.
    let mirror = async move { ctx.hv.mirror_disks(vm.id, &targets, &opts).await };
    let (mirrored, last) = tokio::join!(
        mirror,
        report_copy(ctx, migration.task, "copying-disks", rx)
    );
    mirrored.map_err(|e| e.to_string())?;
    Ok((last, None))
}

/// [`copy_disks`]의 따라잡기(계획 55): `targets`(차례로 볼륨 `volumes`)를 대상의 복제본으로 따라잡게 한다.
async fn catch_up(
    ctx: &Ctx,
    vm: &Vm,
    migration: &MigrationStatus,
    targets: Vec<MirrorTarget>,
    volumes: &[Uuid],
    disks: &[IncomingDisk],
    refused: &[Uuid],
) -> Result<(MirrorProgress, Option<CatchUpState>), String> {
    let generation = ready_entry(vm, migration.node).and_then(|e| e.generation);
    let base = volumes
        .iter()
        .map(|v| {
            let incremental = !refused.contains(v)
                && disks
                    .iter()
                    .any(|d| d.volume == *v && d.base_synced_at.is_some());
            generation
                .filter(|_| incremental)
                .map(|g| replica_bitmap(migration.node, g))
        })
        .collect();
    let (tx, rx) = watch::channel(MirrorProgress::default());
    let req = CatchUp {
        speed: ctx
            .config
            .migration_bandwidth
            .map(|b| b / targets.len().max(1) as u64),
        targets,
        base,
        task: migration.task,
        limit: CATCH_UP_LIMIT,
        passes: CATCH_UP_PASSES,
        stall: DISK_STALL,
        // 드라이버가 기한을 지키고 넘기면 작업과 플리싱을 치운다(바깥에서 버리면 플리싱이 남는다).
        timeout: ctx.config.migration_disk_timeout,
        progress: Some(tx),
    };
    // 따라잡기가 끝나면 req(보내는 쪽)가 버려져 진행률 쓰기도 끝난다.
    let copy = async move {
        ctx.hv
            .catch_up_disks(vm.id, &req)
            .await
            .map_err(|e| e.to_string())
    };
    let (state, last) = tokio::join!(copy, report_copy(ctx, migration.task, "copying-disks", rx));
    let state = state?;
    tracing::info!(vm = %vm.name, passes = state.passes, bytes = last.done_bytes, "caught up the replicas on the target node");
    Ok((last, Some(state)))
}

/// 미러(복제)의 진행이 바뀌면 단계 `stage`로 Task에 쓴다(첫 변화는 곧바로, 그 뒤 `PROGRESS_INTERVAL`에 한 번까지).
/// Task가 이미 끝났으면 그 뒤로 쓰지 않는다. 보내는 쪽이 사라지면 마지막 값을 돌려준다.
pub(super) async fn report_copy(
    ctx: &Ctx,
    task: Uuid,
    stage: &str,
    mut rx: watch::Receiver<MirrorProgress>,
) -> MirrorProgress {
    let mut written = None;
    let mut due = Instant::now();
    let mut pending = false;
    let mut reporting = true;
    loop {
        tokio::select! {
            changed = rx.changed() => {
                if changed.is_err() {
                    return *rx.borrow();
                }
                pending = reporting;
            }
            () = tokio::time::sleep_until(due), if pending => {
                pending = false;
                let now = *rx.borrow_and_update();
                if written != Some(now) {
                    reporting = write_progress(ctx, task, stage, now).await;
                    written = Some(now);
                }
                due = Instant::now() + PROGRESS_INTERVAL;
            }
        }
    }
}

/// 실행 중인 Task의 진행률을 쓴다(읽은 revision 확인). Task가 이미 `Running`이 아니면(다른 쪽이
/// 끝냈다) 되살리지 않도록 쓰지 않고 거짓이다. 실패는 경고만 남긴다.
pub(super) async fn write_progress(
    ctx: &Ctx,
    task: Uuid,
    stage: &str,
    progress: MirrorProgress,
) -> bool {
    let status = TaskStatus {
        state: TaskState::Running,
        error: None,
        progress: Some(TaskProgress {
            stage: stage.into(),
            done_bytes: progress.done_bytes,
            total_bytes: progress.total_bytes,
        }),
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(task) {
            Ok(Some(current)) => current,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %task, error = %e, "cannot record the migration progress");
                return true;
            }
        };
        if current.status.state != TaskState::Running {
            return false;
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(task, current.revision, &status)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(task = %task, error = %e, "cannot record the migration progress");
                return true;
            }
        }
    }
}

/// 6단계: `spec.node`를 대상 노드로 쓴다(revision 확인). 이 쓰기가 적용되면 확정이다. 결과를 모르는
/// 오류는 `COMMIT_TIMEOUT` 동안 다시 시도한다(같은 revision 확인이므로 두 번 적용되지 않는다).
async fn commit(
    ctx: &Ctx,
    id: Uuid,
    migration: &MigrationStatus,
    kept: &[RecordedReplica],
) -> Result<(), String> {
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return Err("VM was deleted".into()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if vm.spec.node == Some(migration.node) {
            return Ok(());
        }
        if vm.spec.node != Some(ctx.config.node) || vm.status.migration.as_ref() != Some(migration)
        {
            return Err(format!("VM {:?} changed during the migration", vm.name));
        }
        // 정지 요청이 검사를 지나 마이그레이션 중에 적용됐다. 대상에서 다시 켜지 않는다.
        if vm.spec.power != PowerState::Running {
            return Err(format!("VM {:?} was stopped during the migration", vm.name));
        }
        let mut spec = vm.spec.clone();
        spec.node = Some(migration.node);
        // 복제본으로 옮겼으면 같은 쓰기에서 복제 방향을 뒤집는다(대상 → 이 노드, 이 노드의 원본이 복제본이 된다).
        if !kept.is_empty() {
            reverse_target(&mut spec, migration, kept, ctx.config.node);
        }
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e @ ClusterError::Rejected(_)) => return Err(e.to_string()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => return Err(format!("cannot record the migration: {e}")),
        }
    }
}

/// 복제본으로 옮긴 VM의 `spec.replication`: 대상 노드를 원본 노드 `source`로 바꾼다(일정 그대로). 원본의 풀은 디스크마다
/// 대상 풀과 같은 이름이면 비우고, 아니면 첫 디스크의 원본 풀이다(대상 하나에는 풀을 하나만 둘 수 있다).
fn reverse_target(
    spec: &mut VmSpec,
    migration: &MigrationStatus,
    kept: &[RecordedReplica],
    source: Uuid,
) {
    let same = migration
        .disks
        .iter()
        .filter(|m| m.existing)
        .all(|m| kept.iter().any(|k| k.id == m.volume && k.pool == m.pool));
    let pool = (!same)
        .then(|| kept.first().map(|k| k.pool.clone()))
        .flatten();
    for t in spec
        .replication
        .iter_mut()
        .filter(|t| t.node == migration.node)
    {
        *t = ReplicationTarget {
            node: source,
            schedule: t.schedule.clone(),
            pool: pool.clone(),
        };
    }
}

enum Outcome {
    /// 확정되지 않았다. 원본을 이어서 실행하지 못했으면 그 이유
    RolledBack(Option<String>),
    /// 확정 쓰기가 적용돼 있었다.
    Committed,
}

/// 확정 전 실패: 마이그레이션 표시를 지워(읽은 revision 확인) 확정을 막은 뒤, 원본이 아직 보내고
/// 있으면 취소하고(나중에 끝나 원본이 멈추지 않게, 남은 디스크 미러도 치운다) 대상의 받는 QEMU를
/// 끄게 하고(복사하려고 만든 디스크도 지운다), 원본이 멈춰
/// 있으면(`postmigrate`, `paused`) 이어서 실행한다(못 하면 보류가 풀린 뒤 reconciler가 한다). HA VM은
/// 여기서 이어서 실행하지 않는다: 보류가 풀린 뒤 reconciler가 리스와 watchdog를 확인하고 이어서 실행한다
/// (그 사이 리스가 끝났으면 끈다). 디스크는
/// 보류가 풀린 뒤 reconciler가 다시 이 노드만 쓰게 한다(대상이 아직 끄지 못했으면 될 때까지 다시
/// 한다 — 그동안 공유로 남아도 원본은 돈다). 지우기 전에 확정이 적용됐으면 `Committed`다. 결과를 알 수 없으면(과반수 없음) 알 때까지 다시 시도한다. 그동안 원본은
/// 멈춰 있을 수 있지만 두 QEMU가 함께 돌지는 않는다.
async fn roll_back(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    id: Uuid,
    migration: &MigrationStatus,
) -> Outcome {
    let mut ha = false;
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        if vm.spec.node == Some(migration.node) {
            return Outcome::Committed;
        }
        ha = vm.spec.ha_active();
        let current = vm.status.migration.as_ref() == Some(migration);
        let revision = vm.revision;
        let mut status = vm.status.clone();
        if !current {
            break;
        }
        status.migration = None;
        status.phase = VmPhase::Running;
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot roll back the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    // 남은 디스크 미러가 대상의 NBD 내보내기를 쥐고 있으면 대상이 받는 QEMU를 끄기 전에 치운다.
    if let Err(e) = ctx.hv.cancel_migration(id).await {
        tracing::warn!(vm = %id, error = %e, "cannot cancel the outgoing migration");
    }
    // 따라잡기가 남긴 플리싱(필터가 디스크 장치를 쥐고 있다)도 뗀다. VM의 줄 안이라 다른 복제·백업은 돌지 않는다.
    match ctx.hv.clear_backup_leftovers(id).await {
        Ok(()) | Err(orbivisor_driver::DriverError::NotRunning(_)) => {}
        Err(e) => {
            tracing::warn!(vm = %id, error = %e, "cannot remove the fleecing left by the disk copy")
        }
    }
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    // 닿지 않으면 대상 노드의 정리가 끈다(마이그레이션 표시가 없다). 복사하려고 만든 디스크도 그렇다.
    if let Err(e) = peers.migrate_incoming_abort(addr, incoming).await {
        tracing::warn!(vm = %id, error = %e, "cannot abort the incoming migration on the target node");
    }
    let resumed = match ctx.hv.inspect(id).await {
        Ok(Some(state))
            if !ha && (state.status == postmigrate() || state.status == RuntimeStatus::Paused) =>
        {
            tracing::info!(vm = %id, "resuming the source of a failed migration");
            ctx.hv.resume(id).await.map_err(|e| e.to_string())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    // 원본 볼륨은 그대로 쓴다.
    if !migration.disks.is_empty()
        && let Some(dir) = &ctx.config.migration_disks
        && let Err(e) = migration_disks::remove(dir, migration.task)
    {
        tracing::warn!(vm = %id, error = %e, "cannot remove a migration disk record");
    }
    Outcome::RolledBack(resumed.err())
}

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로(UEFI 변수도 대상 노드에) 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 디스크를 복사했으면 status를 쓰기 전에 볼륨 기록을 옮기고, 원본 QEMU가 꺼진 뒤 원본 볼륨을
/// 지운다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
/// 남긴다(남은 원본은 정리가 끄고 지운다).
async fn finish(
    ctx: &Ctx,
    id: Uuid,
    migration: &MigrationStatus,
    kept: &[RecordedReplica],
    switched: u64,
    placement: Option<HostPlacement>,
) {
    let deadline = Instant::now() + RECORD_TIMEOUT;
    // 복사한 디스크의 볼륨 기록을 옮긴 위치로 고친 뒤에만 표시를 지운다. 못 고치면 대상 노드의
    // reconciler가 고치고 지운다.
    let moved = migration.disks.is_empty() || apply_moves(ctx, id, deadline).await;
    let vm = loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return,
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
            // 원본 QEMU는 정리(`sweep`)가 끈다.
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; leaving the source QEMU to the sweep");
                return;
            }
        };
        // 대상 노드의 reconciler가 이미 넘겨받아 표시를 지웠다.
        if vm.status.migration.as_ref() != Some(migration) || !moved {
            break vm;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        if status.node != Some(migration.node) {
            status.node = Some(migration.node);
            status.pid = None;
        }
        // 대상이 고른 pCPU·호스트 노드(계획 64). 게스트 모양(`numa`)은 그대로다.
        if let (Some(hw), Some(p)) = (&mut status.hardware, &placement) {
            hw.host_placement = Some(p.clone());
        }
        // QEMU가 UEFI 변수(pflash)를 대상 노드의 변수 파일로 옮겼다(설계 §5). TPM 상태도 swtpm이 대상 노드에 썼다
        // (계획 59 §10): 사본은 비우고 대상의 작업자가 곧 해시를 다시 적고 밀어 둔다.
        if vm.spec.firmware == Firmware::Uefi {
            let tpm = vm.spec.tpm || status.hardware.as_ref().is_some_and(|hw| hw.tpm);
            orbivisor_types::firmware::move_firmware_node(&mut status, migration.node, tpm);
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => break vm,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM");
                break vm;
            }
        }
    };
    // 복사한 디스크의 원본은 볼륨 기록(이제 대상의 풀)이 아니라 기록 파일의 원래 풀에 있다. 복제본으로 남길 원본도 그렇다.
    let mut copies = recorded(ctx, id, Role::Source, Some(migration.task));
    copies.extend(kept.iter().map(|k| RecordedVolume {
        id: k.id,
        pool: k.pool.clone(),
        format: k.format,
        created: false,
    }));
    // 원본 QEMU가 꺼졌으면 복사한 디스크의 원본 볼륨을 지운다(기록 파일과 볼륨 기록의 위치로 다시
    // 확인한다). 지우지 못하면 정리가 다시 한다.
    let stopped = stop_source(ctx, &vm, &copies).await;
    if !kept.is_empty() {
        keep_replicas(ctx, id, migration, kept, stopped, switched).await;
    }
    if stopped
        && !migration.disks.is_empty()
        && let Some(dir) = &ctx.config.migration_disks
    {
        let records = migration_disks::lock(dir).await;
        migration_disks::clean_up(
            &ctx.cluster,
            &*ctx.storage,
            &*ctx.hv,
            ctx.config.node,
            &records,
            Some(id),
            None,
        )
        .await;
    }
}

/// 복제본으로 옮긴 VM의 원본(이 노드)을 뒤집힌 복제의 복제본으로 남긴다(계획 55): 원본 QEMU가 꺼졌으면(`stopped`, 디스크를
/// 더 쓰지 않는다) 이 노드의 디스크를 스위치오버 시점(`at`)의 온전한 복제본으로 기록하고(회차 = 마이그레이션 Task),
/// status에서 대상 노드(이제 VM의 노드)의 항목을 지우고 이 노드의 항목을 그 회차로 `ready`로 쓴다. 꺼지지 않았으면 대상의
/// 항목만 지운다(청소가 이 노드의 디스크를 옛 복제본으로 바꾸고 다음 회차가 전체다). 실패는 경고만 남긴다.
async fn keep_replicas(
    ctx: &Ctx,
    id: Uuid,
    migration: &MigrationStatus,
    kept: &[RecordedReplica],
    stopped: bool,
    at: u64,
) {
    let me = ctx.config.node;
    let mut recorded = false;
    if stopped && let Some(dir) = &ctx.config.replica_dir {
        let _vm = replicas::lock_vm(dir, id).await;
        let records = replicas::lock(dir).await;
        let record = ReplicaRecord {
            vm: id,
            role: replicas::Role::Replica,
            generation: Some(migration.task),
            synced_at: Some(at),
            volumes: kept.to_vec(),
        };
        match replicas::write(records.dir(), &record) {
            Ok(()) => recorded = true,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the disks left on this node as replicas")
            }
        }
    }
    // 복제 항목은 볼륨의 `spec.size_bytes` 척도다(기록 파일은 이미지의 가상 크기).
    let volumes: Vec<ReplicaVolume> = kept
        .iter()
        .map(|k| ReplicaVolume {
            id: k.id,
            size_bytes: match ctx.cluster.get::<VolumeSpec>(k.id) {
                Ok(Some(v)) => v.spec.size_bytes,
                _ => k.size_bytes,
            },
        })
        .collect();
    let deadline = Instant::now() + RECORD_TIMEOUT;
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM to record its reversed replication");
                return;
            }
        };
        let mut status = vm.status.clone();
        status
            .replication
            .retain(|r| r.node != migration.node && r.node != me);
        if recorded && vm.spec.replication.iter().any(|t| t.node == me) {
            status.replication.push(ReplicaStatus {
                state: ReplicationState::Ready,
                generation: Some(migration.task),
                synced_at: Some(at),
                volumes: volumes.clone(),
                ..ReplicaStatus::pending(me)
            });
        }
        if status.replication == vm.status.replication {
            return;
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the reversed replication; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the reversed replication");
                return;
            }
        }
    }
}

/// 확정된 VM의 복사한 디스크의 볼륨 기록을 고친다(`apply_disk_moves`). 결과를 모르는 오류는
/// `deadline`까지 다시 시도한다. 고쳤으면(또는 대상 노드가 이미 고쳐 표시를 지웠으면) 참이다.
async fn apply_moves(ctx: &Ctx, id: Uuid, deadline: Instant) -> bool {
    loop {
        let applied = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => apply_disk_moves(&ctx.cluster, &vm).await,
            Ok(None) => return false,
            Err(e) => Err(e),
        };
        match applied {
            Ok(()) => return true,
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot move the volume records; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot move the volume records; leaving them to the target node");
                return false;
            }
        }
    }
}

/// 마이그레이션을 보내고 멈춘 원본 QEMU(`postmigrate`)를 끄고(탭은 드라이버가 지운다) 이 노드의
/// 디스크를 끈다(복사한 디스크는 `copies`, 원본 기록 파일의 풀로). 다른 상태의 QEMU는 끄지 않고, QEMU가
/// 남아 있으면 디스크도 그대로 둔다. QEMU가 없으면 참이다.
async fn stop_source(ctx: &Ctx, vm: &Vm, copies: &[RecordedVolume]) -> bool {
    let stopped = match ctx.hv.stop_migrated(vm.id).await {
        Ok(true) => true,
        Ok(false) => matches!(ctx.hv.inspect(vm.id).await, Ok(None)),
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
            false
        }
    };
    if stopped {
        deactivate_disks(&ctx.cluster, &*ctx.storage, vm, copies).await;
    }
    stopped
}

/// 남은 마이그레이션 QEMU를 시작할 때와 그 뒤 `tag_sweep`마다 끈다([`sweep`]). 실행기가 멈추면
/// 함께 멈춘다.
pub(super) async fn sweep_every(ctx: Arc<Ctx>) {
    loop {
        sweep(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 남은 마이그레이션 QEMU를 끄고 디스크를 끈다:
/// - 마이그레이션을 보내고 멈춘(`postmigrate`) 원본인데 VM이 다른 노드로 확정됐거나 지워졌다(원본
///   opld가 확정 뒤 정리 전에 죽었거나 정리하지 못했다). VM이 이 노드에 배정돼 있어도 이 노드에서 돌던
///   VM이 아니면(`status.node`가 다른 노드이거나 phase가 `Running`·`Migrating`이 아니다) 확정 뒤
///   이 노드로 다시 옮겨진 것이므로 낡은 원본이다: QEMU만 끈다(디스크는 이 노드의 reconciler가 다룬다).
/// - 받는 중 표시가 있는 QEMU인데 VM이 이 노드로 확정되지 않았고 그 표시의 Task로 이 노드에 오는
///   중도 아니다(원본이 되돌렸거나 확정 전에 죽은 뒤 되돌려졌거나, 앞선 마이그레이션의 것이다). VM이
///   지워졌어도 끈다.
///
/// 확정 전의 원본(`spec.node`·`status.node`가 이 노드)은 마이그레이션 Task나 reconciler가 다룬다.
/// 그 뒤 마이그레이션 디스크 기록 파일을 정리 규칙(설계 §3.6)대로 정리한다: 되돌려졌거나 확정 전에
/// 끝난 마이그레이션이 대상에 만든 볼륨, 확정 뒤 지우지 못한 원본 볼륨을 지운다.
pub(super) async fn sweep(ctx: &Ctx) {
    let me = ctx.config.node;
    match ctx.hv.list().await {
        Ok(states) => {
            for state in states.into_iter().filter(|s| s.status == postmigrate()) {
                match ctx.cluster.get::<VmSpec>(state.id) {
                    Ok(Some(vm))
                        if vm.spec.node == Some(me)
                            && vm.status.node == Some(me)
                            && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating) => {
                    }
                    Ok(Some(vm)) if vm.spec.node == Some(me) => {
                        tracing::info!(vm = %vm.id, "stopping the stale source QEMU of a migration committed elsewhere");
                        if let Err(e) = ctx.hv.stop_migrated(vm.id).await {
                            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
                        }
                    }
                    Ok(Some(vm)) => {
                        tracing::info!(vm = %vm.id, "stopping the source QEMU of a migration committed elsewhere");
                        stop_source(ctx, &vm, &recorded(ctx, vm.id, Role::Source, None)).await;
                    }
                    // 옮긴 뒤 끄고 지웠다. 볼륨은 VM과 함께 지워지지 않지만 이 노드에서 켜진 채
                    // 남은 볼륨은 실행기의 정리가 끈다.
                    Ok(None) => {
                        tracing::info!(vm = %state.id, "stopping the source QEMU of a deleted VM");
                        if let Err(e) = ctx.hv.stop_migrated(state.id).await {
                            tracing::warn!(vm = %state.id, error = %e, "cannot stop the migrated source QEMU");
                        }
                    }
                    Err(e) => tracing::warn!(vm = %state.id, error = %e, "cannot read VM"),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
    }
    sweep_incoming(ctx).await;
    // 남은 QEMU를 끈 뒤 디스크 기록 파일을 정리한다(받기와 겹치지 않게 잠금을 쥐고).
    if let Some(dir) = &ctx.config.migration_disks {
        let records = migration_disks::lock(dir).await;
        migration_disks::clean_up(
            &ctx.cluster,
            &*ctx.storage,
            &*ctx.hv,
            me,
            &records,
            None,
            None,
        )
        .await;
    }
}

/// 이 노드의 디스크 기록 가운데 VM `vm`의 `role` 기록(`task`가 있으면 그 Task의 것)이 적은 볼륨들
fn recorded(ctx: &Ctx, vm: Uuid, role: Role, task: Option<Uuid>) -> Vec<RecordedVolume> {
    let Some(dir) = &ctx.config.migration_disks else {
        return Vec::new();
    };
    migration_disks::list(dir)
        .into_iter()
        .filter(|r| r.vm == vm && r.role == role && task.is_none_or(|t| t == r.task))
        .flat_map(|r| r.volumes)
        .collect()
}

/// [`sweep`]: 확정되지 않은 채 남은 받는 QEMU를 끈다.
async fn sweep_incoming(ctx: &Ctx) {
    let me = ctx.config.node;
    let incoming = match ctx.hv.list_incoming().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list incoming migrations");
            return;
        }
    };
    for (id, task) in incoming {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(vm) => vm,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                continue;
            }
        };
        if let Some(vm) = &vm
            && (vm.spec.node == Some(me)
                || vm
                    .status
                    .migration
                    .as_ref()
                    .is_some_and(|m| m.node == me && m.task == task))
        {
            continue;
        }
        // 그 사이 같은 VM의 새 마이그레이션을 받기 시작했으면(앞선 받는 QEMU를 끄고 새로 띄웠다) 새
        // QEMU를 끄지 않는다.
        match ctx.hv.list_incoming().await {
            Ok(now) if now.contains(&(id, task)) => {}
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list incoming migrations");
                continue;
            }
        }
        tracing::info!(vm = %id, "stopping the receiving QEMU of a migration that did not complete");
        if let Err(e) = ctx.hv.stop(id, orbivisor_driver::StopMode::Force).await {
            tracing::warn!(vm = %id, error = %e, "cannot stop the receiving QEMU");
            continue;
        }
        if let Some(vm) = &vm {
            // 복사하려고 만든 디스크는 기록의 풀(계획의 풀)에 있다.
            let copies = recorded(ctx, id, Role::Target, Some(task));
            deactivate_disks(&ctx.cluster, &*ctx.storage, vm, &copies).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::addr_host;

    #[test]
    fn host_of_a_cluster_address() {
        assert_eq!(addr_host("10.0.0.2:7443"), Some("10.0.0.2"));
        assert_eq!(addr_host("node-b.lan:7443"), Some("node-b.lan"));
        assert_eq!(addr_host("[fd00::2]:7443"), Some("fd00::2"));
        assert_eq!(addr_host("10.0.0.2"), None);
        assert_eq!(addr_host(":7443"), None);
    }
}
