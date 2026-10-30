//! 라이브 마이그레이션의 원본 쪽 Task(설계 §2의 2~8단계와 실패 되돌리기)와 남은 마이그레이션 QEMU
//! 정리(설계 §2 "중간에 죽으면").
//!
//! 두 QEMU가 함께 도는 일은 없다: 대상 QEMU는 멈춘 채 받고 확정(`spec.node`를 대상으로 쓰기) 뒤에만
//! 대상 노드의 reconciler가 이어서 실행한다. 원본은 확정되지 않았다고 확인한 뒤에만 이어서 실행한다.
//! 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용된다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::ClusterError;
use opl_cluster::types::{NodeId, Rejection};
use opl_driver::{MigrateOptions, MigrateTarget, RuntimeStatus};
use opl_peer::{IncomingMigration, MigrateIncomingRequest, Peers};
use opl_reconciler::ReconcilerHandle;
use opl_types::task::Task;
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::incoming::deactivate_disks;

/// 확정 쓰기가 (과반수가 없는 등으로) 실패할 때 다시 시도하는 시간. 지나면 되돌린다.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 확정 뒤 status(`Running`, 대상 노드)를 쓰다 실패할 때 다시 시도하는 시간. 지나면 대상 노드의
/// reconciler가 넘겨받으며 마이그레이션 표시를 지우므로 그대로 둔다.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

/// 실행 중인 VM을 `target` 노드로 옮긴다. 확정 전에 실패하면 원본을 계속 돌리고 실패한다.
pub(super) async fn migrate(ctx: &Ctx, task: &Task, target: Uuid) -> Result<(), String> {
    let id = task.spec.target;
    let peers = ctx
        .config
        .peers
        .clone()
        .ok_or("this node cannot reach other nodes")?;
    // 대상 노드는 클러스터 주소의 호스트(opld `address`)에서 받는다. 같은 이름이 노드 인증서에 있다.
    let addr = ctx
        .cluster
        .members()
        .addr(NodeId(target))
        .map(str::to_string)
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let host = addr_host(&addr)
        .ok_or_else(|| format!("cannot tell the host of node address {addr:?}"))?
        .to_string();
    let migration = MigrationStatus {
        node: target,
        task: task.id,
    };
    let _hold = Hold::begin(&ctx.reconciler, id).await;
    match mark_migrating(ctx, id, migration).await {
        Ok(()) => {}
        Err(Mark::Refused(e)) => return Err(e),
        // 기록됐을 수도 있다.
        Err(Mark::Unknown(e)) => {
            roll_back(ctx, &peers, &addr, id, migration).await;
            return Err(e);
        }
    }
    let sent = send(ctx, &peers, &addr, &host, id, migration).await;
    let error = match sent {
        Ok(()) => commit(ctx, id, migration).await.err(),
        Err(e) => Some(e),
    };
    if let Some(error) = error {
        match roll_back(ctx, &peers, &addr, id, migration).await {
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
    finish(ctx, id, migration).await;
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
fn addr_host(addr: &str) -> Option<&str> {
    let (host, port) = addr.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

enum Mark {
    /// 기록하지 않았다.
    Refused(String),
    /// 기록됐는지 모른다.
    Unknown(String),
}

/// 2단계: 이 노드에서 실행 중인 VM인지 다시 보고 `Migrating`과 대상을 기록한다.
async fn mark_migrating(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<(), Mark> {
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
        status.migration = Some(migration);
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(Mark::Unknown(format!("cannot record the migration: {e}"))),
        }
    }
}

/// 3~5단계: 대상 노드에 받을 준비를 시키고, 보내고, 대상이 다 받았는지 확인한다.
async fn send(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    host: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Result<(), String> {
    let qemu_version = ctx
        .hv
        .running_qemu_version(id)
        .await
        .map_err(|e| format!("cannot read the QEMU version: {e}"))?;
    let req = MigrateIncomingRequest {
        vm: id,
        task: migration.task,
        qemu_version,
    };
    let port = peers
        .migrate_incoming(addr, req)
        .await
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    let target = MigrateTarget {
        host: host.to_string(),
        port,
        tls_hostname: host.to_string(),
    };
    let opts = MigrateOptions {
        bandwidth: ctx.config.migration_bandwidth,
        timeout: ctx.config.migration_timeout,
    };
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
        .map_err(|e| format!("the target node did not receive the VM: {e}"))
}

/// 6단계: `spec.node`를 대상 노드로 쓴다(revision 확인). 이 쓰기가 적용되면 확정이다. 결과를 모르는
/// 오류는 `COMMIT_TIMEOUT` 동안 다시 시도한다(같은 revision 확인이므로 두 번 적용되지 않는다).
async fn commit(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<(), String> {
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
        if vm.spec.node != Some(ctx.config.node) || vm.status.migration != Some(migration) {
            return Err(format!("VM {:?} changed during the migration", vm.name));
        }
        let mut spec = vm.spec.clone();
        spec.node = Some(migration.node);
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

enum Outcome {
    /// 확정되지 않았다. 원본을 이어서 실행하지 못했으면 그 이유
    RolledBack(Option<String>),
    /// 확정 쓰기가 적용돼 있었다.
    Committed,
}

/// 확정 전 실패: 마이그레이션 표시를 지워(읽은 revision 확인) 확정을 막은 뒤 대상의 받는 QEMU를
/// 끄게 하고, 원본이 멈춰 있으면(`postmigrate`, `paused`) 이어서 실행한다. 지우기 전에 확정이
/// 적용됐으면 `Committed`다. 결과를 알 수 없으면(과반수 없음) 알 때까지 다시 시도한다. 그동안 원본은
/// 멈춰 있을 수 있지만 두 QEMU가 함께 돌지는 않는다.
async fn roll_back(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Outcome {
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
        if vm.status.migration != Some(migration) {
            break;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
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
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    // 닿지 않으면 대상 노드의 정리가 끈다(마이그레이션 표시가 없다).
    if let Err(e) = peers.migrate_incoming_abort(addr, incoming).await {
        tracing::warn!(vm = %id, error = %e, "cannot abort the incoming migration on the target node");
    }
    let resumed = match ctx.hv.inspect(id).await {
        Ok(Some(state))
            if state.status == postmigrate() || state.status == RuntimeStatus::Paused =>
        {
            tracing::info!(vm = %id, "resuming the source of a failed migration");
            ctx.hv.resume(id).await.map_err(|e| e.to_string())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    Outcome::RolledBack(resumed.err())
}

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
/// 남긴다(남은 원본은 정리가 끈다).
async fn finish(ctx: &Ctx, id: Uuid, migration: MigrationStatus) {
    let deadline = Instant::now() + RECORD_TIMEOUT;
    let vm = loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                return;
            }
        };
        // 대상 노드의 reconciler가 이미 넘겨받아 표시를 지웠다.
        if vm.status.migration != Some(migration) {
            break vm;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        if status.node != Some(migration.node) {
            status.node = Some(migration.node);
            status.pid = None;
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
    stop_source(ctx, &vm).await;
}

/// 마이그레이션을 보내고 멈춘 원본 QEMU(`postmigrate`)를 끄고(탭은 드라이버가 지운다) 이 노드의
/// 디스크를 끈다. 다른 상태의 QEMU는 끄지 않고, QEMU가 남아 있으면 디스크도 그대로 둔다.
async fn stop_source(ctx: &Ctx, vm: &Vm) {
    let stopped = match ctx.hv.stop_migrated(vm.id).await {
        Ok(true) => true,
        Ok(false) => matches!(ctx.hv.inspect(vm.id).await, Ok(None)),
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
            false
        }
    };
    if stopped {
        deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
    }
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
/// - 마이그레이션을 보내고 멈춘(`postmigrate`) 원본인데 VM이 다른 노드로 확정됐다(원본 opld가 확정
///   뒤 정리 전에 죽었다).
/// - 받는 중 표시가 있는 QEMU인데 VM이 이 노드로 확정되지 않았고 이 노드로 오는 중도 아니다(원본이
///   되돌렸거나 확정 전에 죽은 뒤 되돌려졌다). VM이 지워졌어도 끈다.
///
/// 확정 전의 원본(`spec.node`가 이 노드)은 마이그레이션 Task나 reconciler가 다룬다.
pub(super) async fn sweep(ctx: &Ctx) {
    let me = ctx.config.node;
    match ctx.hv.list().await {
        Ok(states) => {
            for state in states.into_iter().filter(|s| s.status == postmigrate()) {
                let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(state.id) else {
                    continue;
                };
                if vm.spec.node == Some(me) {
                    continue;
                }
                tracing::info!(vm = %vm.id, "stopping the source QEMU of a migration committed elsewhere");
                stop_source(ctx, &vm).await;
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
    }
    let incoming = match ctx.hv.list_incoming().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list incoming migrations");
            return;
        }
    };
    for id in incoming {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(vm) => vm,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                continue;
            }
        };
        if let Some(vm) = &vm
            && (vm.spec.node == Some(me) || vm.status.migration.is_some_and(|m| m.node == me))
        {
            continue;
        }
        tracing::info!(vm = %id, "stopping the receiving QEMU of a migration that did not complete");
        if let Err(e) = ctx.hv.stop(id, opl_driver::StopMode::Force).await {
            tracing::warn!(vm = %id, error = %e, "cannot stop the receiving QEMU");
            continue;
        }
        if let Some(vm) = &vm {
            deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
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
