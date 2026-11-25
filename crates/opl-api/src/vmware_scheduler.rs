//! VMware 이전 스케줄러(계획 72 설계 §9.3): 리더에서 틱마다(15초) 계획을 보고 `vmware-sync` Task를 만든다. 진행은
//! Raft(`vmware-vm`)에 있으므로 리더가 바뀌어도 다음 틱에 잇는다.
//!
//! - `running` 계획: `pending` → 전체 복사, `copying`(끊긴·실패한 시도) → 체크포인트부터 다시, `syncing`·`rolled-back`이고
//!   `next_sync_at`이 지남 → 델타. 델타가 먼저(이미 내려 둔 VM의 주기를 지킴), 그다음 이어 가기, 그다음 새 전체 복사.
//! - 한도: 계획의 끝나지 않은 동기화·전환 Task 수 ≤ `max_parallel`, 같은 ESXi 호스트 ≤ `max_per_host`, 같은 대상 노드 ≤
//!   `max_per_node`(호스트·노드는 모든 계획의 Task를 센다).
//! - 노드: 항목의 `spec.node` > 규칙의 `node` > 디스크의 풀을 모두 쓸 수 있는 온라인 노드 중 VMware Task가 가장 적은 것(고른
//!   노드는 항목에 먼저 적는다 — 대상 볼륨이 그 노드의 로컬 풀에 생긴다). 노드가 오프라인이면 그 항목을 건너뛴다.
//! - 예약 전환(`cutover.at`이 지난 `running` 계획): `ready`·`syncing` 항목마다 `vmware-cutover`(같은 한도, 델타와 함께 먼저).
//!   항목마다 그 시각 뒤 한 번만 한다(`cutover_requested_at` — 실패해 되돌린 항목을 다시 끄지 않는다).
//! - `cancelled` 계획: 원본·우리 쪽에 남긴 것이 있는 항목은 `vmware-cleanup`(대상 볼륨도 지움), 없는 항목은 곧 `cancelled`.
//! - 모든 항목이 끝난(`finalized`·`reverted`·`cancelled`) `running` 계획은 `finished`다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::node::{Node, NodeSpec, VMWARE_MIGRATE};
use opl_types::storage::{StoragePool, StoragePoolSpec, pool_usable_on};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vmware::{
    MAX_CLEANUP_FAILURES, VmwareMigration, VmwareMigrationPhase, VmwareMigrationSpec,
    VmwareMigrationStatus, VmwareVm, VmwareVmSpec, VmwareVmState,
};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backups::now;
use crate::cluster::missing_feature;
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vmware::client::split_datastore_path;
use crate::vmware_migrations::counts;

#[derive(Debug, Clone)]
pub struct VmwareSchedulerConfig {
    /// 계획을 보는 간격
    pub tick: Duration,
}

impl Default for VmwareSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(15),
        }
    }
}

/// VMware 이전 반복 작업. 모든 노드에서 띄우고 리더인 노드에서만 일한다.
pub struct VmwareScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl VmwareScheduler {
    pub fn spawn(state: Arc<AppState>, config: VmwareSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: VmwareSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = Box::pin(tick(&state)) => {}
        }
    }
}

/// 스케줄러가 할 일 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// 동기화 Task를 `node`에 만든다. `pick`이면 고른 노드를 항목에 먼저 적는다.
    Sync {
        item: Uuid,
        node: Uuid,
        full: bool,
        pick: bool,
    },
    /// 예약 전환: 요청 시각을 항목에 먼저 적고 `vmware-cutover`를 항목의 노드에 만든다.
    Cutover { item: Uuid, node: Uuid },
    /// 취소된 계획의 항목이 남긴 것을 치운다(대상 볼륨도).
    Cleanup { item: Uuid, node: Uuid },
    /// 남긴 것이 없는 항목은 곧 `cancelled`
    Cancel { item: Uuid },
    /// 모든 항목이 끝난 계획
    Finish { migration: Uuid },
}

/// 동기화·전환 Task(동시 수를 센다)
fn counts_toward_limits(op: &TaskOp) -> bool {
    matches!(op, TaskOp::VmwareSync { .. } | TaskOp::VmwareCutover)
}

fn is_vmware_task(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VmwareSync { .. }
            | TaskOp::VmwareCutover
            | TaskOp::VmwareRevert
            | TaskOp::VmwareCleanup { .. }
    )
}

/// 항목이 원본·우리 쪽에 남긴 것이 있다(정리가 필요하다).
fn holds_something(item: &VmwareVm) -> bool {
    let s = &item.status;
    s.snapshot.is_some()
        || s.pending_snapshot.is_some()
        || s.cbt_enabled_by_us
        || s.target_vm.is_some()
        || !s.source_changes.is_empty()
        || s.disks.iter().any(|d| d.volume.is_some())
}

/// 항목 디스크의 대상 풀 이름(덮어쓰기 > 데이터스토어 규칙 > 기본 풀). 모르는 디스크는 뺀다.
fn item_pools(m: &VmwareMigration, item: &VmwareVm) -> Vec<String> {
    let Some(source) = &item.status.source else {
        return Vec::new();
    };
    let overrides = item.spec.overrides.as_ref();
    let mut out: Vec<String> = source
        .vm
        .disks
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            let ds = split_datastore_path(&d.file)
                .map(|(ds, _)| ds)
                .unwrap_or("");
            overrides
                .and_then(|o| o.pools.get(i))
                .map(String::as_str)
                .or_else(|| m.spec.rules.pool_for(ds))
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 동기화·전환할 차례인 항목(차례: 전환·델타 0·이어 가기 1·새 복사 2, 그다음 시각·이름)
struct Due<'a> {
    order: (u8, u64, String),
    m: &'a VmwareMigration,
    item: &'a VmwareVm,
    full: bool,
    cutover: bool,
}

/// 예약 전환할 항목: 시각이 지났고, 복사를 끝낸 `ready`·`syncing`이고, 그 시각 뒤에 전환을 요청한 적이 없다.
fn cutover_due(now: u64, m: &VmwareMigration, item: &VmwareVm) -> Option<u64> {
    let at = m.spec.cutover.at.filter(|at| *at <= now)?;
    let s = &item.status;
    (matches!(s.state, VmwareVmState::Ready | VmwareVmState::Syncing)
        && s.rounds > 0
        && s.cutover_requested_at.is_none_or(|t| t < at))
    .then_some(at)
}

/// 틱 하나에 할 일을 고른다(`now`는 유닉스 초). 순수 함수.
pub(crate) fn plan(
    now: u64,
    migrations: &[VmwareMigration],
    items: &[VmwareVm],
    tasks: &[Task],
    nodes: &[Node],
    pools: &[StoragePool],
) -> Vec<Action> {
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished() && is_vmware_task(&t.spec.op))
        .collect();
    let busy = |item: Uuid| unfinished.iter().any(|t| t.spec.target == item);
    let item_of = |id: Uuid| items.iter().find(|i| i.id == id);
    // 끝나지 않은 동기화·전환: 계획·호스트·노드별
    let mut per_migration: BTreeMap<Uuid, usize> = BTreeMap::new();
    let mut per_host: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_node: BTreeMap<Uuid, usize> = BTreeMap::new();
    for t in unfinished
        .iter()
        .filter(|t| counts_toward_limits(&t.spec.op))
    {
        *per_node.entry(t.spec.node).or_default() += 1;
        if let Some(item) = item_of(t.spec.target) {
            *per_migration.entry(item.spec.migration).or_default() += 1;
            if let Some(host) = item.status.source.as_ref().and_then(|s| s.host.clone()) {
                *per_host.entry(host).or_default() += 1;
            }
        }
    }
    let online = |id: Uuid| {
        nodes
            .iter()
            .any(|n| n.id == id && n.status.online && !n.spec.maintenance)
    };
    let mut out = Vec::new();
    let mut due: Vec<Due<'_>> = Vec::new();
    for m in migrations {
        let mine: Vec<&VmwareVm> = items.iter().filter(|i| i.spec.migration == m.id).collect();
        match m.status.phase {
            VmwareMigrationPhase::Running => {
                if !mine.is_empty() && mine.iter().all(|i| i.status.state.is_final()) {
                    out.push(Action::Finish { migration: m.id });
                    continue;
                }
                for item in mine {
                    if busy(item.id) {
                        continue;
                    }
                    if let Some(at) = cutover_due(now, m, item) {
                        due.push(Due {
                            order: (0, at, item.name.clone()),
                            m,
                            item,
                            full: false,
                            cutover: true,
                        });
                        continue;
                    }
                    let s = &item.status;
                    let order = match s.state {
                        VmwareVmState::Syncing | VmwareVmState::RolledBack => {
                            match s.next_sync_at {
                                Some(at) if at <= now => (0, at),
                                _ => continue,
                            }
                        }
                        VmwareVmState::Copying if s.next_sync_at.is_none_or(|at| at <= now) => {
                            (1, 0)
                        }
                        VmwareVmState::Pending => (2, 0),
                        _ => continue,
                    };
                    let full =
                        s.state != VmwareVmState::Syncing && s.state != VmwareVmState::RolledBack;
                    due.push(Due {
                        order: (order.0, order.1, item.name.clone()),
                        m,
                        item,
                        full,
                        cutover: false,
                    });
                }
            }
            VmwareMigrationPhase::Cancelled => {
                for item in mine {
                    // 전환한(`done`) 항목과 전환 중인 항목은 취소가 건드리지 않는다 — 마무리나 되돌리기로 끝낸다.
                    if item.status.state.is_final()
                        || matches!(
                            item.status.state,
                            VmwareVmState::Done | VmwareVmState::CuttingOver
                        )
                        || busy(item.id)
                    {
                        continue;
                    }
                    if !holds_something(item) {
                        out.push(Action::Cancel { item: item.id });
                    } else if item.status.cleanup_failures < MAX_CLEANUP_FAILURES {
                        let node = item.spec.node.filter(|n| online(*n)).or_else(|| {
                            nodes
                                .iter()
                                .filter(|n| n.status.online && !n.spec.witness)
                                .map(|n| n.id)
                                .min()
                        });
                        if let Some(node) = node {
                            out.push(Action::Cleanup {
                                item: item.id,
                                node,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    due.sort_by(|a, b| a.order.cmp(&b.order));
    for Due {
        m,
        item,
        full,
        cutover,
        ..
    } in due
    {
        let limits = &m.spec.limits;
        if per_migration.get(&m.id).copied().unwrap_or(0) >= usize::from(limits.max_parallel) {
            continue;
        }
        let host = item.status.source.as_ref().and_then(|s| s.host.clone());
        if let Some(h) = &host
            && per_host.get(h).copied().unwrap_or(0) >= usize::from(limits.max_per_host)
        {
            continue;
        }
        // 전환은 대상 볼륨이 있는 항목의 노드에서만(동기화가 고른 노드)
        if cutover {
            let Some(node) = item.spec.node.filter(|n| online(*n)) else {
                continue;
            };
            if per_node.get(&node).copied().unwrap_or(0) >= usize::from(limits.max_per_node) {
                continue;
            }
            *per_migration.entry(m.id).or_default() += 1;
            *per_node.entry(node).or_default() += 1;
            if let Some(h) = host {
                *per_host.entry(h).or_default() += 1;
            }
            out.push(Action::Cutover {
                item: item.id,
                node,
            });
            continue;
        }
        let (node, pick) = match item.spec.node.or(m.spec.rules.node) {
            Some(n) => (n, item.spec.node.is_none()),
            None => {
                let wanted = item_pools(m, item);
                let usable = |n: &Node| {
                    wanted.iter().all(|p| {
                        pools
                            .iter()
                            .find(|pool| pool.name == *p)
                            .is_some_and(|pool| pool_usable_on(pool, n))
                    })
                };
                let best = nodes
                    .iter()
                    .filter(|n| {
                        n.status.online && !n.spec.maintenance && !n.spec.witness && usable(n)
                    })
                    .filter(|n| {
                        per_node.get(&n.id).copied().unwrap_or(0) < usize::from(limits.max_per_node)
                    })
                    .min_by_key(|n| (per_node.get(&n.id).copied().unwrap_or(0), n.id));
                match best {
                    Some(n) => (n.id, true),
                    None => continue,
                }
            }
        };
        if !online(node)
            || per_node.get(&node).copied().unwrap_or(0) >= usize::from(limits.max_per_node)
        {
            continue;
        }
        *per_migration.entry(m.id).or_default() += 1;
        *per_node.entry(node).or_default() += 1;
        if let Some(h) = host {
            *per_host.entry(h).or_default() += 1;
        }
        out.push(Action::Sync {
            item: item.id,
            node,
            full,
            pick,
        });
    }
    out
}

async fn tick(state: &AppState) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    // 옛 노드는 이전 Task를 모른다(계획은 기능이 생긴 뒤에만 만들어지지만 새 노드가 막 빠졌을 수 있다).
    if !missing_feature(cluster, VMWARE_MIGRATE).is_ok_and(|old| old.is_empty()) {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<VmwareMigrationSpec>()?.1,
            cluster.list::<VmwareVmSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<StoragePoolSpec>()?.1,
        ))
    })();
    let Ok((migrations, items, tasks, nodes, pools)) = lists else {
        return;
    };
    if migrations.is_empty() {
        return;
    }
    for action in plan(now(), &migrations, &items, &tasks, &nodes, &pools) {
        if let Err(e) = Box::pin(act(state, &action)).await {
            tracing::warn!(?action, error = %e, "VMware migration scheduler step failed");
        }
    }
}

async fn act(state: &AppState, action: &Action) -> Result<(), String> {
    let cluster = &state.cluster;
    match *action {
        Action::Sync {
            item,
            node,
            full,
            pick,
        } => {
            if pick {
                let current = cluster
                    .get::<VmwareVmSpec>(item)
                    .map_err(|e| e.to_string())?
                    .ok_or("vmware-vm was deleted")?;
                let spec = VmwareVmSpec {
                    node: Some(node),
                    ..current.spec.clone()
                };
                cluster
                    .update_spec::<VmwareVmSpec>(
                        item,
                        Some(current.revision),
                        current.labels,
                        &spec,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
            }
            create_task(cluster, TaskOp::VmwareSync { full }, item, node)
                .await
                .map_err(|e| e.message)?;
        }
        Action::Cutover { item, node } => {
            let current = cluster
                .get::<VmwareVmSpec>(item)
                .map_err(|e| e.to_string())?
                .ok_or("vmware-vm was deleted")?;
            let mut status = current.status.clone();
            status.cutover_requested_at = Some(now());
            status.push_history(now(), "scheduled cutover");
            // 먼저 적는다: Task를 만들지 못해도 같은 시각에 다시 끄지 않는다(다음 계획 시각·손 전환은 된다).
            cluster
                .update_status_checked::<VmwareVmSpec>(item, current.revision, &status)
                .await
                .map_err(|e| e.to_string())?;
            create_task(cluster, TaskOp::VmwareCutover, item, node)
                .await
                .map_err(|e| e.message)?;
        }
        Action::Cleanup { item, node } => {
            create_task(
                cluster,
                TaskOp::VmwareCleanup {
                    delete_target: true,
                },
                item,
                node,
            )
            .await
            .map_err(|e| e.message)?;
        }
        Action::Cancel { item } => {
            let current = cluster
                .get::<VmwareVmSpec>(item)
                .map_err(|e| e.to_string())?
                .ok_or("vmware-vm was deleted")?;
            let mut status = current.status.clone();
            status.state = VmwareVmState::Cancelled;
            status.next_sync_at = None;
            status.push_history(now(), "cancelled");
            cluster
                .update_status_checked::<VmwareVmSpec>(item, current.revision, &status)
                .await
                .map_err(|e| e.to_string())?;
            recount(state, current.spec.migration).await;
        }
        Action::Finish { migration } => {
            let m = cluster
                .get::<VmwareMigrationSpec>(migration)
                .map_err(|e| e.to_string())?
                .ok_or("vmware-migration was deleted")?;
            let (_, items) = cluster.list::<VmwareVmSpec>().map_err(|e| e.to_string())?;
            let items: Vec<VmwareVm> = items
                .into_iter()
                .filter(|i| i.spec.migration == migration)
                .collect();
            let status = VmwareMigrationStatus {
                phase: VmwareMigrationPhase::Finished,
                counts: counts(&items),
                error: None,
            };
            match cluster
                .update_status_checked::<VmwareMigrationSpec>(migration, m.revision, &status)
                .await
            {
                Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Ok(())
}

async fn recount(state: &AppState, migration: Uuid) {
    let Ok(Some(m)) = state.cluster.get::<VmwareMigrationSpec>(migration) else {
        return;
    };
    let Ok((_, items)) = state.cluster.list::<VmwareVmSpec>() else {
        return;
    };
    let items: Vec<VmwareVm> = items
        .into_iter()
        .filter(|i| i.spec.migration == migration)
        .collect();
    let status = VmwareMigrationStatus {
        counts: counts(&items),
        ..m.status.clone()
    };
    if status != m.status {
        let _ = state
            .cluster
            .update_status_checked::<VmwareMigrationSpec>(migration, m.revision, &status)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;
    use opl_types::task::{TaskState, TaskStatus};
    use opl_types::vm_import::{SourceDisk, SourceVm};
    use opl_types::vmware::MAX_SYNC_FAILURES;
    use opl_types::vmware::{VmwareMigrationMode, VmwareSourceVm, VmwareSyncDisk};

    fn node(name: &str) -> Node {
        let mut n: Node = Resource::new(
            name,
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: "127.0.0.1:0".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status.online = true;
        n
    }

    fn pool(name: &str) -> StoragePool {
        Resource::new(
            name,
            serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
                "backend": { "kind": "dir", "path": format!("/p/{name}") }
            }))
            .unwrap(),
        )
    }

    fn migration(limits: (u16, u16, u16)) -> VmwareMigration {
        let mut m: VmwareMigration = Resource::new(
            "wave1",
            VmwareMigrationSpec {
                source: Uuid::now_v7(),
                mode: VmwareMigrationMode::Warm,
                rules: serde_json::from_value(serde_json::json!({ "default_pool": "local" }))
                    .unwrap(),
                limits: Default::default(),
                sync: Default::default(),
                cutover: Default::default(),
                folder: None,
            },
        );
        m.spec.limits.max_parallel = limits.0;
        m.spec.limits.max_per_host = limits.1;
        m.spec.limits.max_per_node = limits.2;
        m.status.phase = VmwareMigrationPhase::Running;
        m
    }

    fn item(m: &VmwareMigration, name: &str, state: VmwareVmState, host: &str) -> VmwareVm {
        let mut i: VmwareVm = Resource::new(
            format!("wave1-{name}"),
            VmwareVmSpec {
                migration: m.id,
                source_ref: format!("vm-{name}"),
                instance_uuid: format!("uuid-{name}"),
                target_name: name.into(),
                overrides: None,
                node: None,
            },
        );
        i.status.state = state;
        i.status.source = Some(VmwareSourceVm {
            vm: SourceVm {
                name: name.into(),
                guest_os: "otherGuest64".into(),
                firmware: Default::default(),
                vcpus: 1,
                cores_per_socket: 1,
                memory_mib: 512,
                disks: vec![SourceDisk {
                    name: "disk0".into(),
                    file: format!("[ds1] {name}/{name}.vmdk"),
                    capacity_bytes: 1 << 20,
                    controller: "scsi".into(),
                }],
                nics: Vec::new(),
            },
            host: Some(host.into()),
            tools: None,
            power: None,
            guest_ips: Default::default(),
        });
        i
    }

    fn running(item: &VmwareVm, node: Uuid, op: TaskOp) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: item.id,
                node,
            },
        );
        t.status = TaskStatus {
            state: TaskState::Running,
            error: None,
            progress: None,
        };
        t
    }

    fn syncs(actions: &[Action]) -> Vec<Uuid> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Sync { item, .. } => Some(*item),
                _ => None,
            })
            .collect()
    }

    /// 델타(주기 지남)가 먼저, 그다음 끊긴 복사, 그다음 새 전체 복사. 주기가 남은 항목·실패·준비된 항목은 건너뛴다.
    #[test]
    fn deltas_go_first_then_resumes_then_new_copies() {
        let n1 = node("n1");
        let m = migration((8, 8, 8));
        let new = item(&m, "a", VmwareVmState::Pending, "esx1");
        let resumed = item(&m, "b", VmwareVmState::Copying, "esx1");
        let mut delta = item(&m, "c", VmwareVmState::Syncing, "esx1");
        delta.status.next_sync_at = Some(100);
        let mut later = item(&m, "d", VmwareVmState::Syncing, "esx1");
        later.status.next_sync_at = Some(500);
        let failed = item(&m, "e", VmwareVmState::Failed, "esx1");
        let mut stopped = item(&m, "f", VmwareVmState::Syncing, "esx1");
        stopped.status.next_sync_at = None;
        let items = vec![
            new.clone(),
            resumed.clone(),
            delta.clone(),
            later,
            failed,
            stopped,
        ];
        let actions = plan(
            200,
            &[m],
            &items,
            &[],
            std::slice::from_ref(&n1),
            &[pool("local")],
        );
        assert_eq!(syncs(&actions), vec![delta.id, resumed.id, new.id]);
        assert!(actions.iter().all(|a| matches!(a,
            Action::Sync { node, full, pick, item } if *node == n1.id && *pick && *full == (*item != delta.id))));
    }

    /// 한도 셋: 계획의 동시 수, 같은 ESXi 호스트, 같은 대상 노드(도는 Task를 센다).
    #[test]
    fn limits_count_running_tasks_per_migration_host_and_node() {
        let n1 = node("n1");
        let n2 = node("n2");
        let nodes = [n1.clone(), n2.clone()];
        let pools = [pool("local")];
        let m = migration((2, 8, 8));
        let a = item(&m, "a", VmwareVmState::Pending, "esx1");
        let b = item(&m, "b", VmwareVmState::Pending, "esx1");
        let c = item(&m, "c", VmwareVmState::Pending, "esx1");
        let items = vec![a.clone(), b.clone(), c.clone()];
        // 계획당 2
        assert_eq!(
            syncs(&plan(
                0,
                std::slice::from_ref(&m),
                &items,
                &[],
                &nodes,
                &pools
            )),
            vec![a.id, b.id]
        );
        let tasks = [running(&a, n1.id, TaskOp::VmwareSync { full: true })];
        assert_eq!(
            syncs(&plan(
                0,
                std::slice::from_ref(&m),
                &items,
                &tasks,
                &nodes,
                &pools
            )),
            vec![b.id]
        );
        // 호스트당 1
        let mut m = migration((8, 1, 8));
        m.id = a.spec.migration;
        let items = vec![a.clone(), b.clone()];
        assert_eq!(
            syncs(&plan(
                0,
                std::slice::from_ref(&m),
                &items,
                &[],
                &nodes,
                &pools
            ))
            .len(),
            1
        );
        // 노드당 1: 두 노드에 하나씩, 덜 바쁜 노드 먼저
        let mut m = migration((8, 8, 1));
        m.id = a.spec.migration;
        let actions = plan(0, &[m], &items, &[], &nodes, &pools);
        let picked: Vec<Uuid> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Sync { node, .. } => Some(*node),
                _ => None,
            })
            .collect();
        assert_eq!(picked.len(), 2);
        assert_ne!(picked[0], picked[1]);
    }

    /// 예약 전환: 시각이 지나면 복사를 끝낸 `ready`·`syncing` 항목마다 그 노드에 전환(델타 대신, 한도 안), 시각 뒤에 요청한
    /// 항목·되돌린 항목·시각 전에는 하지 않는다.
    #[test]
    fn scheduled_cutovers_run_once_per_item_after_the_time() {
        let n1 = node("n1");
        let mut m = migration((2, 8, 8));
        m.spec.cutover.at = Some(1000);
        let mut ready = item(&m, "a", VmwareVmState::Syncing, "esx1");
        ready.status.rounds = 2;
        ready.status.next_sync_at = Some(10);
        ready.spec.node = Some(n1.id);
        let mut cold = item(&m, "b", VmwareVmState::Ready, "esx1");
        cold.status.rounds = 1;
        cold.spec.node = Some(n1.id);
        let mut asked = cold.clone();
        asked.id = Uuid::now_v7();
        asked.name = "wave1-c".into();
        asked.status.cutover_requested_at = Some(1001);
        let mut rolled = item(&m, "d", VmwareVmState::RolledBack, "esx1");
        rolled.status.rounds = 2;
        rolled.spec.node = Some(n1.id);
        let mut copying = item(&m, "e", VmwareVmState::Pending, "esx1");
        copying.spec.node = Some(n1.id);
        let items = vec![ready.clone(), cold.clone(), asked, rolled, copying.clone()];
        let nodes = std::slice::from_ref(&n1);
        let pools = [pool("local")];
        // 시각 전: 델타와 새 복사만
        let before = plan(999, std::slice::from_ref(&m), &items, &[], nodes, &pools);
        assert!(
            !before.iter().any(|a| matches!(a, Action::Cutover { .. })),
            "{before:?}"
        );
        // 시각 뒤: 전환 둘이 먼저(한도 2 — 새 복사는 기다림), 이미 요청한 항목·되돌린 항목은 아님
        let after = plan(1005, std::slice::from_ref(&m), &items, &[], nodes, &pools);
        assert_eq!(
            after,
            vec![
                Action::Cutover {
                    item: ready.id,
                    node: n1.id
                },
                Action::Cutover {
                    item: cold.id,
                    node: n1.id
                },
            ]
        );
        // 항목 노드가 오프라인이면 건너뛴다.
        let mut off = n1.clone();
        off.status.online = false;
        let none = plan(1005, &[m], &items, &[], &[off], &pools);
        assert!(none.is_empty(), "{none:?}");
    }

    /// 노드: 항목에 적힌 노드(오프라인이면 건너뜀), 풀을 쓸 수 없는 노드는 고르지 않는다.
    #[test]
    fn nodes_must_be_online_and_see_the_pools() {
        let n1 = node("n1");
        let mut n2 = node("n2");
        let m = migration((8, 8, 8));
        let a = item(&m, "a", VmwareVmState::Pending, "esx1");
        // 풀은 n2에만
        let mut local = pool("local");
        local.spec.nodes = vec![n2.id];
        let actions = plan(
            0,
            std::slice::from_ref(&m),
            std::slice::from_ref(&a),
            &[],
            &[n1.clone(), n2.clone()],
            std::slice::from_ref(&local),
        );
        assert_eq!(
            actions,
            vec![Action::Sync {
                item: a.id,
                node: n2.id,
                full: true,
                pick: true
            }]
        );
        n2.status.online = false;
        assert!(
            plan(
                0,
                std::slice::from_ref(&m),
                std::slice::from_ref(&a),
                &[],
                &[n1.clone(), n2.clone()],
                std::slice::from_ref(&local)
            )
            .is_empty()
        );
        let mut pinned = a.clone();
        pinned.spec.node = Some(n2.id);
        assert!(
            plan(
                0,
                std::slice::from_ref(&m),
                std::slice::from_ref(&pinned),
                &[],
                &[n1.clone(), n2.clone()],
                std::slice::from_ref(&local)
            )
            .is_empty()
        );
        n2.status.online = true;
        assert_eq!(
            plan(
                0,
                &[m],
                std::slice::from_ref(&pinned),
                &[],
                &[n1, n2.clone()],
                std::slice::from_ref(&local)
            ),
            vec![Action::Sync {
                item: pinned.id,
                node: n2.id,
                full: true,
                pick: false
            }]
        );
    }

    /// 취소된 계획: 남긴 것이 있으면 정리(도는 Task가 있거나 정리가 세 번 실패했으면 아님), 없으면 곧 취소. 멈춘 계획은 아무것도.
    /// 모든 항목이 끝난 계획은 끝.
    #[test]
    fn cancelled_plans_clean_up_and_finished_plans_finish() {
        let n1 = node("n1");
        let mut m = migration((8, 8, 8));
        m.status.phase = VmwareMigrationPhase::Cancelled;
        let bare = item(&m, "a", VmwareVmState::Pending, "esx1");
        let mut held = item(&m, "b", VmwareVmState::Syncing, "esx1");
        held.status.disks = vec![VmwareSyncDisk {
            key: 2000,
            file: "[ds1] b/b.vmdk".into(),
            capacity_bytes: 1 << 20,
            volume: Some(Uuid::now_v7()),
            change_id: None,
            copied_bytes: 0,
            rounds: 1,
            last_delta_bytes: 0,
            source_mtime: None,
        }];
        held.spec.node = Some(n1.id);
        let mut gave_up = held.clone();
        gave_up.id = Uuid::now_v7();
        gave_up.status.cleanup_failures = MAX_CLEANUP_FAILURES;
        // 동기화를 세 번 실패한 항목도 정리한다(정리 실패 수는 따로 센다 — 리뷰 1).
        let mut sync_failed = held.clone();
        sync_failed.id = Uuid::now_v7();
        sync_failed.status.state = VmwareVmState::Failed;
        sync_failed.status.sync_failures = MAX_SYNC_FAILURES;
        // 전환한 항목·전환 중인 항목은 취소가 건드리지 않는다(리뷰 1 — 대상 VM을 지우지 않게).
        let mut cut_over = held.clone();
        cut_over.id = Uuid::now_v7();
        cut_over.status.state = VmwareVmState::Done;
        cut_over.status.target_vm = Some(Uuid::now_v7());
        let mut cutting = cut_over.clone();
        cutting.id = Uuid::now_v7();
        cutting.status.state = VmwareVmState::CuttingOver;
        let done = item(&m, "d", VmwareVmState::Cancelled, "esx1");
        let items = vec![
            bare.clone(),
            held.clone(),
            gave_up,
            sync_failed.clone(),
            cut_over,
            cutting,
            done.clone(),
        ];
        let actions = plan(
            0,
            std::slice::from_ref(&m),
            &items,
            &[],
            std::slice::from_ref(&n1),
            &[],
        );
        assert_eq!(
            actions,
            vec![
                Action::Cancel { item: bare.id },
                Action::Cleanup {
                    item: held.id,
                    node: n1.id
                },
                Action::Cleanup {
                    item: sync_failed.id,
                    node: n1.id
                }
            ]
        );
        let tasks = [
            running(&held, n1.id, TaskOp::VmwareSync { full: false }),
            running(&sync_failed, n1.id, TaskOp::VmwareSync { full: false }),
        ];
        assert_eq!(
            plan(
                0,
                std::slice::from_ref(&m),
                &items,
                &tasks,
                std::slice::from_ref(&n1),
                &[]
            ),
            vec![Action::Cancel { item: bare.id }]
        );
        m.status.phase = VmwareMigrationPhase::Paused;
        assert!(
            plan(
                0,
                std::slice::from_ref(&m),
                &items,
                &[],
                std::slice::from_ref(&n1),
                &[]
            )
            .is_empty()
        );
        m.status.phase = VmwareMigrationPhase::Running;
        assert_eq!(
            plan(0, std::slice::from_ref(&m), &[done], &[], &[n1], &[]),
            vec![Action::Finish { migration: m.id }]
        );
    }

    #[test]
    fn pools_come_from_overrides_rules_and_the_default() {
        let mut m = migration((1, 1, 1));
        m.spec.rules.datastores = vec![opl_types::vmware::VmwareMapping {
            from: "ds1".into(),
            to: "fast".into(),
        }];
        let mut i = item(&m, "a", VmwareVmState::Pending, "esx1");
        assert_eq!(item_pools(&m, &i), vec!["fast".to_string()]);
        i.spec.overrides = Some(opl_types::vmware::VmwareOverrides {
            pools: vec!["slow".into()],
            ..Default::default()
        });
        assert_eq!(item_pools(&m, &i), vec!["slow".to_string()]);
    }
}
