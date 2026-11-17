//! 로컬 디스크 복제(계획 55)의 원본 쪽 Task(`vm-replicate`): VM이 도는 노드에서 노드 로컬 풀의 디스크를 대상
//! 노드의 같은 id 복제본으로 한 시점에 보낸다. 처음(또는 복제본이 온전하지 않거나 크기가 바뀌었거나 비트맵이 없는
//! 디스크)은 전체, 그 뒤는 앞 회차의 dirty bitmap(`opl-repl-{대상}-{회차}`) 뒤에 바뀐 블록만 보낸다. 디스크
//! 데이터는 원본 QEMU가 대상의 받는 QEMU(NBD+TLS)에 바로 쓰고, 대상 노드는 증분을 겹층에 받아 회차가 끝나야
//! 복제본에 합친다(`replica_receive`).
//!
//! 비트맵 규칙: 회차가 성공해 Raft에 기록한 뒤에만 그 대상의 앞 비트맵을 지운다. 받는 쪽이 회차를 버린 것이
//! 확실하면 새 비트맵을 지우고, 결과를 모르면(끝내기 응답을 잃음) 둘 다 남긴다 — 다음 회차는 Raft의 옛 회차
//! 비트맵으로 증분하는데, 옛 비트맵은 새 시점 뒤의 변경을 모두 담으므로 받는 쪽이 새 시점까지 합쳤어도 맞다.

use std::path::Path;

use opl_cluster::ClusterError;
use opl_cluster::types::NodeId;
use opl_driver::{MirrorProgress, Quiesced, ReplicaDiskTarget, ReplicateRequest};
use opl_peer::{
    PrepareMode, PrepareVolume, ReplicaAbortRequest, ReplicaError, ReplicaFinishRequest,
    ReplicaPrepareRequest, ReplicaPrepareResponse,
};
use opl_reconciler::replicas::{self, RecordedReplica, ReplicaRecord, ReplicaState, Role};
use opl_types::node::{Node, NodeSpec};
use opl_types::replication::{
    ReplicaStatus, ReplicaVolume, ReplicationState, ReplicationTarget, parse_replica_bitmap,
    replica_bitmap,
};
use opl_types::runtime::ImageFormat;
use opl_types::storage::Volume;
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::watch;
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use super::migrate::{addr_host, report_copy};
use crate::backups::now;
use crate::replication::update_replica;
use crate::scheduler::{Inventory, disk_moves};

/// VM이 이 노드에서 돌지 않아 건너뛴 회차의 Task 경고
const SKIPPED: &str = "replication run skipped: the VM is not running on this node";

/// 회차에서 보낼 디스크 하나
#[derive(Debug, Clone)]
struct Disk {
    /// 도는 QEMU의 디스크 순번(`disk{i}`)
    index: usize,
    /// 원본 볼륨
    volume: Volume,
    /// 대상 노드에서 복제본을 둘 풀과 형식
    pool: String,
    format: ImageFormat,
    /// 볼륨의 크기(`spec.size_bytes` — 회차 기록과 HA·마이그레이션이 견주는 척도)
    size_bytes: u64,
    /// 원본 이미지의 가상 크기(백엔드가 맞춘 크기 — 받는 쪽이 복제본을 만들고 견주는 척도)와 실제로 쓰는 바이트
    virtual_bytes: u64,
    allocated_bytes: u64,
    /// 증분이면 기준 비트맵과 그 뒤 바뀐 바이트(비트맵의 `count`)
    base: Option<(String, u64)>,
}

/// VM을 Task의 대상 노드로 복제한다(회차 id = Task id). 회차를 건너뛰었거나 게스트 에이전트가 파일 시스템을 얼리지
/// 못했으면 그 경고다. 회차가 끝나면 대상 노드에 펌웨어 사본(UEFI 변수·TPM 상태)이 없을 때 밀어 둔다(계획 59 §10).
pub(super) async fn replicate(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let TaskOp::VmReplicate { node: target } = task.spec.op else {
        return Err("not a replication task".into());
    };
    let id = task.spec.target;
    let peers = ctx
        .config
        .peers
        .clone()
        .ok_or("this node cannot reach other nodes")?;
    let dir = ctx
        .config
        .replica_dir
        .clone()
        .ok_or("this node cannot record replicas")?;
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let Some((vm, how, node)) = check(ctx, id, target)? else {
        // VM이 이 노드에서 돌지 않는다(옮겨 갔거나 꺼짐): 실패가 아니다. 스케줄러가 VM이 도는 노드에서 다시 만든다.
        // 보낸 것이 없으니 경고로 남긴다(CLI가 앞 회차를 이번 것으로 알리지 않게).
        tracing::info!(vm = %id, task = %task.id, "skipping a replication run: the VM is not running on this node");
        return Ok(Some(SKIPPED.into()));
    };
    let addr = ctx
        .cluster
        .members()
        .addr(NodeId(target))
        .map(str::to_string)
        .ok_or_else(|| format!("node {:?} is not a cluster member", node.name))?;
    let inv = Inventory::load(&ctx.cluster).map_err(read)?;
    let entry = vm
        .status
        .replication
        .iter()
        .find(|r| r.node == target)
        .cloned()
        .unwrap_or_else(|| ReplicaStatus::pending(target));
    let mut disks = plan(ctx, &vm, &inv, &how, &entry).await?;
    // 증분의 기준 회차 시점. 받는 쪽이 그 회차(또는 더 새것)를 가졌는지 본다.
    let base_at = entry.synced_at.unwrap_or(0);
    let tried = now();
    let full = disks.iter().any(|d| d.base.is_none());
    // 전체 복사는 복제본 위에 바로 쓰므로 끝날 때까지 HA가 이 대상을 쓰지 않는다.
    update_replica(&ctx.cluster, id, target, |r| {
        r.last_try = Some(tried);
        if full && r.state == ReplicationState::Ready {
            r.state = ReplicationState::Syncing;
        }
    })
    .await
    .map_err(|e| format!("cannot record the replication: {e}"))?;
    record_primary(&dir, &vm, &disks).await?;
    let mut reply = prepare(&peers, &addr, task, &node, &disks, base_at).await?;
    if !reply.refused.is_empty() {
        // 받는 쪽에 기준 회차가 없다(기록을 잃는 등). 그 디스크를 전체로 바꿔 한 번 더 부른다.
        for d in disks
            .iter_mut()
            .filter(|d| reply.refused.contains(&d.volume.id))
        {
            tracing::info!(vm = %vm.name, disk = d.index, "the target node has no complete replica of disk{}; sending it in full", d.index);
            d.base = None;
        }
        update_replica(&ctx.cluster, id, target, |r| {
            if r.state == ReplicationState::Ready {
                r.state = ReplicationState::Syncing;
            }
        })
        .await
        .map_err(|e| format!("cannot record the replication: {e}"))?;
        reply = prepare(&peers, &addr, task, &node, &disks, base_at).await?;
        if !reply.refused.is_empty() {
            return Err(format!("node {:?} refused the increments again", node.name));
        }
    }
    if reply.exports.len() != disks.len() {
        return Err(format!(
            "node {:?} exported {} disks instead of {}",
            node.name,
            reply.exports.len(),
            disks.len()
        ));
    }
    // 대상 노드에는 준비 요청이 닿은 링크(지금 쓰는 링크)의 호스트로 접속한다(마이그레이션과 같다).
    let link = peers.active_link(&addr);
    let host =
        addr_host(&link).ok_or_else(|| format!("cannot tell the host of node address {link:?}"))?;
    let bitmap = replica_bitmap(target, task.id);
    let started = Instant::now();
    let at = now();
    let quiesce = vm.spec.guest_agent.as_ref().is_some_and(|g| g.freeze_fs);
    let sent = send(ctx, id, task.id, &disks, &reply, host, &bitmap, quiesce).await;
    let finish = |ok| ReplicaFinishRequest {
        vm: id,
        task: task.id,
        run: task.id,
        ok,
        synced_at: at,
        volumes: disks.iter().map(|d| d.volume.id).collect(),
    };
    let (bytes, quiesced) = match sent {
        Ok(sent) => sent,
        Err(e) => {
            // 드라이버가 새 비트맵을 지웠다. 받는 쪽에 회차를 버리게 한다(못 하면 정리·청소가 한다).
            if let Err(w) = peers.replica_finish(&addr, finish(false)).await {
                tracing::warn!(vm = %vm.name, error = %w, "cannot tell the target node to discard the run");
            }
            return Err(e);
        }
    };
    match peers.replica_finish(&addr, finish(true)).await {
        Ok(()) => {}
        // 받는 쪽이 회차를 끝냈는지 모른다: 두 비트맵을 모두 남긴다(머리말).
        Err(e @ ReplicaError::Unavailable(_)) => {
            return Err(format!(
                "cannot tell whether node {:?} kept the replica: {e}",
                node.name
            ));
        }
        // 받는 쪽이 회차를 버렸다.
        Err(e) => {
            drop_bitmaps(ctx, id, std::slice::from_ref(&bitmap)).await;
            return Err(format!(
                "node {:?} did not keep the replica: {e}",
                node.name
            ));
        }
    }
    let volumes: Vec<ReplicaVolume> = disks
        .iter()
        .map(|d| ReplicaVolume {
            id: d.volume.id,
            size_bytes: d.size_bytes,
        })
        .collect();
    let secs = started.elapsed().as_secs();
    let full = disks.iter().any(|d| d.base.is_none());
    // 기록하지 못하면 두 비트맵을 남긴다(Raft의 옛 회차로 다음이 증분한다).
    update_replica(&ctx.cluster, id, target, |r| {
        r.state = ReplicationState::Ready;
        r.generation = Some(task.id);
        r.synced_at = Some(at);
        r.volumes = volumes.clone();
        r.last_bytes = bytes;
        r.last_secs = secs;
        r.last_full = full;
        r.error = None;
    })
    .await
    .map_err(|e| format!("cannot record the replica: {e}"))?;
    tracing::info!(vm = %vm.name, node = %node.name, bytes, full, "replicated VM");
    // 이 대상의 앞 회차 비트맵(남은 것 포함)은 이제 쓸모가 없다.
    match ctx.hv.backup_bitmaps(id).await {
        Ok(all) => {
            let old: Vec<String> = all
                .iter()
                .flat_map(|d| &d.bitmaps)
                .map(|b| b.name.clone())
                .filter(|n| {
                    *n != bitmap && parse_replica_bitmap(n).is_some_and(|(node, _)| node == target)
                })
                .collect();
            if !old.is_empty() {
                drop_bitmaps(ctx, id, &old).await;
            }
        }
        Err(e) => {
            tracing::warn!(vm = %vm.name, error = %e, "cannot read the dirty bitmaps to remove old ones")
        }
    }
    super::firmware::push_replica_copy(ctx, id, target).await;
    Ok(quiesced.warning())
}

/// VM이 이 노드에서 실행 중이고(옮기는 중이 아님) 대상이 아직 복제 대상이며 온라인인지 본다. VM, 그 대상의 설정,
/// 대상 노드를 돌려준다. VM이 이 노드에서 돌지 않으면(옮겨 갔거나 꺼짐 — 회차를 건너뛴다) None이다.
fn check(
    ctx: &Ctx,
    id: Uuid,
    target: Uuid,
) -> Result<Option<(Vm, ReplicationTarget, Node)>, String> {
    let read = |e: ClusterError| format!("cannot read the cluster state: {e}");
    let vm = ctx
        .cluster
        .get::<VmSpec>(id)
        .map_err(read)?
        .ok_or("VM was deleted")?;
    let running = vm.spec.node == Some(ctx.config.node)
        && vm.spec.power == PowerState::Running
        && vm.status.phase == VmPhase::Running
        && vm.status.migration.is_none();
    if !running {
        return Ok(None);
    }
    let node = ctx.cluster.get::<NodeSpec>(target).map_err(read)?;
    let name = node
        .as_ref()
        .map_or_else(|| target.to_string(), |n| n.name.clone());
    let Some(how) = vm
        .spec
        .replication
        .iter()
        .find(|t| t.node == target)
        .cloned()
    else {
        return Err(format!("node {name:?} is no longer a replication target"));
    };
    // 노드의 온라인 표시는 리더가 리스로 정한다.
    let Some(node) = node.filter(|n| n.status.online) else {
        return Err(format!("node {name:?} is offline"));
    };
    Ok(Some((vm, how, node)))
}

/// 보낼 디스크(도는 QEMU의 디스크 가운데 노드 로컬 풀의 것)와 디스크마다의 방식을 정한다. 증분은 그 대상의 항목이
/// `ready`이고 그 볼륨을 같은 크기로 담았고, 그 회차의 비트맵이 디스크에 있고 기록 중이며 inconsistent가 아닐 때다.
/// 아니면 전체이고 이유를 남긴다.
async fn plan(
    ctx: &Ctx,
    vm: &Vm,
    inv: &Inventory,
    how: &ReplicationTarget,
    entry: &ReplicaStatus,
) -> Result<Vec<Disk>, String> {
    let pool = match &how.pool {
        Some(name) => Some(
            inv.pools
                .iter()
                .find(|p| &p.name == name)
                .ok_or_else(|| format!("storage pool {name:?} not found"))?,
        ),
        None => None,
    };
    // 디스크 순번은 도는 QEMU의 것이다(핫플러그 기록이 있으면 그 디스크들).
    let mut spec = vm.spec.clone();
    if let Some(hw) = &vm.status.hardware {
        spec.disks = hw.disks.clone();
    }
    let moves = disk_moves(&spec, inv, pool);
    if moves.is_empty() {
        return Err(format!("VM {:?} has no disks on node-local pools", vm.name));
    }
    let bitmaps = ctx
        .hv
        .backup_bitmaps(vm.id)
        .await
        .map_err(|e| format!("cannot read the dirty bitmaps: {e}"))?;
    let base = entry
        .generation
        .filter(|_| entry.state == ReplicationState::Ready)
        .map(|g| replica_bitmap(entry.node, g));
    let mut disks = Vec::new();
    for (i, disk) in spec.disks.iter().enumerate() {
        let Some(volume) = inv.volumes.iter().find(|v| v.name == disk.volume) else {
            continue;
        };
        let Some(m) = moves.iter().find(|m| m.volume == volume.id) else {
            continue;
        };
        let source = inv
            .pools
            .iter()
            .find(|p| p.name == volume.spec.pool)
            .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
        let info = ctx
            .storage
            .volume_info(source, volume.id, volume.spec.format)
            .await
            .map_err(|e| format!("cannot read disk {:?}: {e}", volume.name))?;
        let held = entry.volumes.iter().find(|v| v.id == volume.id);
        let found = base.as_ref().and_then(|name| {
            bitmaps
                .get(i)
                .and_then(|d| d.bitmaps.iter().find(|b| &b.name == name))
        });
        let incremental = match (&base, held, found) {
            (None, _, _) | (_, None, _) => Err("no complete replica".to_string()),
            (_, Some(v), _) if v.size_bytes != volume.spec.size_bytes => {
                Err(format!("disk{i} changed size"))
            }
            (Some(name), _, Some(b)) if b.recording && !b.inconsistent => {
                Ok((name.clone(), b.count))
            }
            _ => Err(format!("no dirty bitmap on disk{i}")),
        };
        let base = match incremental {
            Ok(base) => Some(base),
            Err(why) => {
                tracing::info!(vm = %vm.name, disk = i, reason = %why, "sending disk{i} in full: {why}");
                None
            }
        };
        disks.push(Disk {
            index: i,
            volume: volume.clone(),
            pool: m.pool.clone(),
            format: m.format,
            size_bytes: volume.spec.size_bytes,
            virtual_bytes: info.virtual_bytes,
            allocated_bytes: info.allocated_bytes,
            base,
        });
    }
    Ok(disks)
}

/// 이 노드가 VM의 원본이라고 기록 파일에 적는다(HA 복구 뒤 돌아온 옛 원본을 알아보는 근거).
async fn record_primary(dir: &Path, vm: &Vm, disks: &[Disk]) -> Result<(), String> {
    let _vm = replicas::lock_vm(dir, vm.id).await;
    let records = replicas::lock(dir).await;
    let record = ReplicaRecord {
        vm: vm.id,
        role: Role::Primary,
        generation: None,
        synced_at: None,
        volumes: disks
            .iter()
            .map(|d| RecordedReplica {
                id: d.volume.id,
                pool: d.volume.spec.pool.clone(),
                format: d.volume.spec.format,
                size_bytes: d.virtual_bytes,
                created: false,
                state: ReplicaState::Ready,
                overlay: None,
            })
            .collect(),
    };
    replicas::write(records.dir(), &record)
        .map_err(|e| format!("cannot write the replica record: {e}"))
}

/// 대상 노드에 받을 준비를 시킨다. 증분 디스크의 기준 회차 시점은 `base_at`이다.
async fn prepare(
    peers: &opl_peer::Peers,
    addr: &str,
    task: &Task,
    node: &Node,
    disks: &[Disk],
    base_at: u64,
) -> Result<ReplicaPrepareResponse, String> {
    let req = ReplicaPrepareRequest {
        vm: task.spec.target,
        task: task.id,
        run: task.id,
        volumes: disks
            .iter()
            .map(|d| PrepareVolume {
                id: d.volume.id,
                pool: d.pool.clone(),
                format: d.format,
                size_bytes: d.virtual_bytes,
                allocated_bytes: d.allocated_bytes,
                mode: match d.base {
                    Some(_) => PrepareMode::Incremental {
                        base_synced_at: base_at,
                    },
                    None => PrepareMode::Full,
                },
            })
            .collect(),
        dirty_bytes: disks
            .iter()
            .filter_map(|d| d.base.as_ref())
            .map(|b| b.1)
            .sum(),
    };
    peers
        .replica_prepare(addr, req)
        .await
        .map_err(|e| format!("node {:?} cannot receive the replica: {e}", node.name))
}

/// 디스크마다 받는 QEMU의 내보내기로 보내고 그동안 진행률을 Task에 쓴다. 대역폭 상한은 디스크 수로 나눈다. 보낸
/// 바이트와 얼리기 결과를 돌려준다(`quiesce`면 회차 시점 앞뒤로 게스트 파일 시스템을 얼린다). 실패하면 드라이버가
/// 작업을 치우고 새 비트맵을 지운다.
#[allow(clippy::too_many_arguments)]
async fn send(
    ctx: &Ctx,
    vm: Uuid,
    task: Uuid,
    disks: &[Disk],
    reply: &ReplicaPrepareResponse,
    host: &str,
    bitmap: &str,
    quiesce: bool,
) -> Result<(u64, Quiesced), String> {
    let targets = disks
        .iter()
        .zip(&reply.exports)
        .map(|(d, export)| ReplicaDiskTarget {
            disk: d.index,
            host: host.to_string(),
            port: reply.port,
            export: export.clone(),
            tls_hostname: host.to_string(),
            base: d.base.as_ref().map(|b| b.0.clone()),
        })
        .collect::<Vec<_>>();
    let (tx, rx) = watch::channel(MirrorProgress::default());
    let req = ReplicateRequest {
        speed: ctx
            .config
            .replication_bandwidth
            .map(|b| b / targets.len().max(1) as u64),
        targets,
        bitmap: bitmap.to_string(),
        stall: ctx.config.replication_stall,
        progress: Some(tx),
        quiesce,
    };
    // 복제가 끝나면 req(보내는 쪽)가 버려져 진행률 쓰기도 끝난다.
    let replicate = async move { ctx.hv.replicate_disks(vm, &req).await };
    let (sent, _) = tokio::join!(replicate, report_copy(ctx, task, "replicating", rx));
    sent.map_err(|e| e.to_string())
}

/// 도는 VM에서 비트맵들을 지운다. 실패는 경고만 남긴다(다음 성공한 회차가 다시 지운다).
async fn drop_bitmaps(ctx: &Ctx, vm: Uuid, names: &[String]) {
    if let Err(e) = ctx.hv.remove_named_bitmaps(vm, names).await {
        tracing::warn!(vm = %vm, ?names, error = %e, "cannot remove replication bitmaps");
    }
}

/// 실패하거나 중단된 복제 Task의 정리: 그 대상의 항목에 이유를 남기고, 원본 QEMU에 남은 플리싱과 작업을 치우고(VM의
/// 줄 안이라 다른 복제·백업·마이그레이션은 돌지 않는다 — opld가 회차 도중 다시 시작했으면 필터가 디스크 장치를 쥐고
/// 있다), 받는 쪽에 회차를 버리게 한다(실패는 경고만 — 받는 쪽 청소가 끝난 회차의 받는 QEMU와 겹층을 치운다). 대상
/// 노드가 오프라인이면 받는 쪽은 부르지 않는다.
pub(super) async fn discard(ctx: &Ctx, task: &Task, error: &str) {
    let TaskOp::VmReplicate { node } = task.spec.op else {
        return;
    };
    let vm = task.spec.target;
    if let Err(e) = update_replica(&ctx.cluster, vm, node, |r| {
        r.error = Some(error.to_string())
    })
    .await
    {
        tracing::warn!(vm = %vm, error = %e, "cannot record the replication error");
    }
    match ctx.hv.clear_backup_leftovers(vm).await {
        Ok(()) | Err(opl_driver::DriverError::NotRunning(_)) => {}
        Err(e) => tracing::warn!(vm = %vm, error = %e, "cannot remove the fleecing of the run"),
    }
    let online = matches!(ctx.cluster.get::<NodeSpec>(node), Ok(Some(n)) if n.status.online);
    let addr = ctx.cluster.members().addr(NodeId(node)).map(str::to_string);
    let (Some(peers), Some(addr), true) = (&ctx.config.peers, addr, online) else {
        return;
    };
    let req = ReplicaAbortRequest {
        vm,
        task: task.id,
        run: task.id,
    };
    if let Err(e) = peers.replica_abort(&addr, req).await {
        tracing::warn!(vm = %vm, task = %task.id, error = %e, "cannot tell the target node to discard the run; its sweep will");
    }
}
