//! 클러스터 정리 작업(견고성 설계 §3). 리더만 `tick`마다 노드·Task·스냅샷 목록을 보고, 오프라인이
//! 유예보다 오래 간 노드의 `pending` Task와 제거된 노드의 끝나지 않은 Task를 실패로 끝내고, 만들
//! Task 없이 남은 `pending` 스냅샷을 지운다. 실패로 끝낸 Task의 정리(볼륨·태그)는 하지 않는다.
//! 예약 스냅샷의 만들기·예약 정리의 삭제를 끝냈으면 실행기처럼 `task_failed`로 VM에 남긴다.
//! 끝난 Task는 기간·개수 기준으로 지운다(웹 정리 설계 §2).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::schedule::{recorded_vm, set_schedule_error, task_failed};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct ClusterJanitorConfig {
    /// 목록을 확인하는 간격
    pub tick: Duration,
    /// 노드가 이보다 오래 오프라인이면 그 노드의 `pending` Task를 실패로 끝낸다.
    pub offline_grace: Duration,
    /// 만들 Task 없이 `pending`인 스냅샷을 처음 본 뒤 이만큼 지나도 그대로면 지운다. API는 스냅샷을
    /// 만든 뒤 Task를 만들므로 그 사이(전달된 쓰기는 하나에 최대 `write_timeout`)보다 길어야 한다.
    pub orphan_grace: Duration,
    /// 만든 지 이보다 오래된 끝난 Task를 지운다. 만든 시각은 UUIDv7 id의 시각이다.
    pub task_max_age: Duration,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    pub task_max_finished: usize,
    /// 개수 기준으로는 끝난 것을 처음 본 뒤 이만큼 지난 Task만 지운다. 막 끝난 Task를 기다리는
    /// 쪽(oplctl `wait_task`, 웹 `waitForTask`)이 결과를 읽을 시간이다.
    pub finished_grace: Duration,
}

/// 한 tick에 지우는 끝난 Task의 최대 수. 나머지는 다음 tick에 지운다.
const TASK_DELETES_PER_TICK: usize = 200;

impl Default for ClusterJanitorConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            offline_grace: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(30),
            task_max_age: Duration::from_secs(7 * 24 * 3600),
            task_max_finished: 1000,
            finished_grace: Duration::from_secs(600),
        }
    }
}

/// 클러스터 정리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct ClusterJanitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ClusterJanitor {
    pub fn spawn(state: Arc<AppState>, config: ClusterJanitorConfig) -> Self {
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
    config: ClusterJanitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut seen = Seen::default();
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
            _ = tick(&state, &config, &mut seen) => {}
        }
    }
}

/// 이전 tick들에서 본 것. 메모리에만 두므로 리더가 바뀌면 유예를 다시 잰다.
#[derive(Debug, Default)]
struct Seen {
    /// 노드가 오프라인으로 처음 보인 시각
    offline_since: HashMap<Uuid, Instant>,
    /// 만들 Task 없이 `pending`인 스냅샷과 처음 그렇게 보인 시각
    orphans: HashMap<Uuid, Instant>,
    /// 끝난 Task와 처음 끝난 것으로 보인 시각
    finished: HashMap<Uuid, Instant>,
}

/// 정리 작업이 할 일. 읽은 revision에서만 쓴다(그사이 바뀌었으면 다음 tick에 다시 본다).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    FailTask {
        id: Uuid,
        revision: u64,
        error: String,
    },
    DeleteSnapshot {
        id: Uuid,
        revision: u64,
    },
    DeleteTask {
        id: Uuid,
        revision: u64,
    },
}

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십의 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
fn decide(
    now: Instant,
    config: &ClusterJanitorConfig,
    nodes: &[Node],
    members: &BTreeSet<Uuid>,
    tasks: &[Task],
    snapshots: &[Snapshot],
    seen: &mut Seen,
) -> Vec<Action> {
    // 다시 온라인이 되었거나 목록에서 빠진 노드는 잊는다.
    seen.offline_since
        .retain(|id, _| nodes.iter().any(|n| n.id == *id && !n.status.online));
    for node in nodes.iter().filter(|n| !n.status.online) {
        seen.offline_since.entry(node.id).or_insert(now);
    }
    let mut actions = vec![];
    for task in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let node = task.spec.node;
        let error = match nodes.iter().find(|n| n.id == node) {
            None if members.contains(&node) => continue,
            // 제거된 노드는 돌아오지 않으므로 실행 중이던 Task도 끝낸다.
            None => format!("node {node} was removed"),
            // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 두고, 시작 전인
            // Task만 유예 뒤에 끝낸다.
            Some(_) if task.status.state != TaskState::Pending => continue,
            Some(_) => match seen.offline_since.get(&node) {
                Some(since) if now.duration_since(*since) >= config.offline_grace => {
                    format!("node {node} is offline")
                }
                _ => continue,
            },
        };
        actions.push(Action::FailTask {
            id: task.id,
            revision: task.revision,
            error,
        });
    }
    // API는 스냅샷을 만든 뒤에 Task를 만들므로 그 사이를 고아로 보지 않도록 처음 본 뒤 유예가
    // 지나도 고아일 때 지운다. 그 사이 Task가 보였으면 다시 잰다.
    let orphans: HashMap<Uuid, Instant> = snapshots
        .iter()
        .filter(|s| s.status.phase == SnapshotPhase::Pending)
        .filter(|s| {
            !tasks.iter().any(|t| {
                !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id)
            })
        })
        .map(|s| (s.id, seen.orphans.get(&s.id).copied().unwrap_or(now)))
        .collect();
    for s in snapshots.iter() {
        if orphans
            .get(&s.id)
            .is_some_and(|since| now.duration_since(*since) >= config.orphan_grace)
        {
            actions.push(Action::DeleteSnapshot {
                id: s.id,
                revision: s.revision,
            });
        }
    }
    seen.orphans = orphans;
    actions
}

/// 지울 끝난 Task를 오래된 것부터 최대 `limit`개 고르고 `seen`을 고친다. 만든 지 `task_max_age`가
/// 넘은 것과 `task_max_finished`를 넘는 만큼 오래된 것 중 끝난 것을 처음 본 뒤 `finished_grace`가
/// 지난 것이다. 끝나지 않은 Task는 고르지 않는다.
fn expired_tasks(
    now: SystemTime,
    instant: Instant,
    config: &ClusterJanitorConfig,
    tasks: &[Task],
    seen: &mut Seen,
    limit: usize,
) -> Vec<Action> {
    let mut finished: Vec<(SystemTime, &Task)> = tasks
        .iter()
        .filter(|t| t.status.state.is_finished())
        .map(|t| (created(t.id), t))
        .collect();
    finished.sort_by_key(|(at, t)| (*at, t.id));
    // 지워졌거나 없어진 Task는 잊는다.
    seen.finished = finished
        .iter()
        .map(|(_, t)| (t.id, seen.finished.get(&t.id).copied().unwrap_or(instant)))
        .collect();
    let over = finished.len().saturating_sub(config.task_max_finished);
    let old = finished
        .iter()
        .take_while(|(at, _)| now.duration_since(*at).unwrap_or_default() > config.task_max_age)
        .count();
    let settled = |t: &Task| instant.duration_since(seen.finished[&t.id]) >= config.finished_grace;
    finished
        .iter()
        .enumerate()
        .filter(|(i, (_, t))| (*i < old || *i < over) && settled(t))
        .take(limit)
        .map(|(_, (_, t))| Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        })
        .collect()
}

/// UUIDv7 id에 담긴 만든 시각. 시각이 없는 id는 가장 오래된 것으로 본다.
fn created(id: Uuid) -> SystemTime {
    id.get_timestamp().map_or(SystemTime::UNIX_EPOCH, |ts| {
        let (secs, nanos) = ts.to_unix();
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    })
}

async fn tick(state: &AppState, config: &ClusterJanitorConfig, seen: &mut Seen) {
    let cluster = &state.cluster;
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        *seen = Seen::default();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
        ))
    })();
    let Ok((nodes, tasks, snapshots)) = lists else {
        return;
    };
    let ids: BTreeSet<Uuid> = members
        .voters
        .keys()
        .chain(members.learners.keys())
        .map(|id| id.0)
        .collect();
    let now = Instant::now();
    let mut actions = decide(now, config, &nodes, &ids, &tasks, &snapshots, seen);
    actions.extend(expired_tasks(
        SystemTime::now(),
        now,
        config,
        &tasks,
        seen,
        TASK_DELETES_PER_TICK,
    ));
    let mut skip_deletes = false;
    for action in actions {
        if skip_deletes && matches!(action, Action::DeleteTask { .. }) {
            continue;
        }
        let result = apply(cluster, &action).await;
        // 그사이 바뀌었으면(실행기가 시작했다 등) 다음 tick에 다시 본다.
        match result {
            Ok(_) => tracing::info!(?action, "janitor"),
            Err(e) if written_anyway(cluster, &action, &e) => {
                tracing::info!(?action, "janitor (reply lost)")
            }
            Err(e) => {
                tracing::info!(?action, error = %e, "janitor action not applied");
                if stops_deletes(&action, &e) {
                    tracing::warn!(error = %e, "janitor: deleting finished tasks again next tick");
                    skip_deletes = true;
                }
                continue;
            }
        }
        if let Action::FailTask { id, error, .. } = action {
            record_schedule(state, &tasks, &snapshots, id, error).await;
        }
    }
}

/// 할 일을 읽은 revision에서만 쓴다.
async fn apply(cluster: &Cluster, action: &Action) -> Result<u64, ClusterError> {
    match action {
        Action::FailTask {
            id,
            revision,
            error,
        } => {
            let status = TaskStatus {
                state: TaskState::Failed,
                error: Some(error.clone()),
            };
            cluster
                .update_status_checked::<TaskSpec>(*id, *revision, &status)
                .await
        }
        Action::DeleteSnapshot { id, revision } => {
            cluster.delete::<SnapshotSpec>(*id, Some(*revision)).await
        }
        Action::DeleteTask { id, revision } => {
            cluster.delete::<TaskSpec>(*id, Some(*revision)).await
        }
    }
}

/// 끝난 Task 지우기가 거부(그사이 바뀜·없어짐)가 아닌 오류(시간 초과, 리더 아님 등)로 실패했으면
/// 클러스터가 쓰기를 받지 못하는 것이므로 이 tick의 남은 지우기는 건너뛴다(한 tick에 최대
/// `TASK_DELETES_PER_TICK`번 기다리지 않게).
fn stops_deletes(action: &Action, err: &ClusterError) -> bool {
    matches!(action, Action::DeleteTask { .. }) && !matches!(err, ClusterError::Rejected(_))
}

/// 실패 쓰기가 `RevisionMismatch`로 거부됐어도 앞선 시도의 응답만 잃은 자기 쓰기일 수 있다(전달
/// 재시도). Task를 다시 읽어 이 오류로 실패해 있으면 쓰인 것으로 본다.
fn written_anyway(cluster: &Cluster, action: &Action, err: &ClusterError) -> bool {
    let (
        Action::FailTask { id, error, .. },
        ClusterError::Rejected(Rejection::RevisionMismatch { .. }),
    ) = (action, err)
    else {
        return false;
    };
    cluster
        .get::<TaskSpec>(*id)
        .ok()
        .flatten()
        .is_some_and(|t| {
            t.status.state == TaskState::Failed && t.status.error.as_deref() == Some(error)
        })
}

/// 실패로 끝낸 Task가 예약 스냅샷의 만들기나 예약 정리의 삭제면 실행기처럼 VM에 남긴다.
async fn record_schedule(
    state: &AppState,
    tasks: &[Task],
    snapshots: &[Snapshot],
    id: Uuid,
    error: String,
) {
    let Some(task) = tasks.iter().find(|t| t.id == id) else {
        return;
    };
    let find = |s: Uuid| snapshots.iter().find(|x| x.id == s).cloned();
    let Some(vm) = recorded_vm(&task.spec, find) else {
        return;
    };
    if let Err(e) = set_schedule_error(&state.cluster, vm, Some(task_failed(error))).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::vm::{CpuSpec, MachineType, PowerState, VmSpec};

    use super::*;

    const GRACE: Duration = Duration::from_secs(60);
    const ORPHAN_GRACE: Duration = Duration::from_secs(30);
    const FINISHED_GRACE: Duration = Duration::from_secs(600);
    const CONFIG: ClusterJanitorConfig = ClusterJanitorConfig {
        tick: Duration::from_secs(10),
        offline_grace: GRACE,
        orphan_grace: ORPHAN_GRACE,
        task_max_age: Duration::from_secs(7 * 24 * 3600),
        task_max_finished: 1000,
        finished_grace: FINISHED_GRACE,
    };

    fn node(online: bool) -> Node {
        let mut n: Node = Resource::new(
            "n",
            NodeSpec {
                address: "h:1".into(),
                maintenance: false,
            },
        );
        n.status = NodeStatus {
            online,
            ..Default::default()
        };
        n
    }

    fn task(op: TaskOp, node: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node,
            },
        );
        t.revision = 7;
        t.status = TaskStatus { state, error: None };
        t
    }

    fn pending_snapshot() -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: Uuid::now_v7(),
                name: "s".into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: false,
                vm_spec: VmSpec {
                    cpu: CpuSpec {
                        sockets: 1,
                        cores: 1,
                        model: Default::default(),
                    },
                    memory_mib: 256,
                    firmware: Default::default(),
                    machine: MachineType::Virt,
                    disks: vec![],
                    nics: vec![],
                    power: PowerState::Stopped,
                    restart: Default::default(),
                    node: None,
                    snapshot_schedule: None,
                },
            },
        );
        s.revision = 3;
        s.status = SnapshotStatus::default();
        s
    }

    fn fail(t: &Task, error: String) -> Action {
        Action::FailTask {
            id: t.id,
            revision: t.revision,
            error,
        }
    }

    #[test]
    fn pending_tasks_of_an_offline_node_fail_after_the_grace() {
        let (up, down) = (node(true), node(false));
        let nodes = [up.clone(), down.clone()];
        let members = BTreeSet::from([up.id, down.id]);
        let pending = task(TaskOp::VmStart, down.id, TaskState::Pending);
        let running = task(TaskOp::VmStop, down.id, TaskState::Running);
        let done = task(TaskOp::VmStart, down.id, TaskState::Failed);
        let elsewhere = task(TaskOp::VmStart, up.id, TaskState::Pending);
        let tasks = [pending.clone(), running, done, elsewhere];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, &nodes, &members, &tasks, &[], &mut seen);
        // 처음 오프라인으로 본 시각에서 유예를 잰다.
        assert_eq!(at(t0), vec![]);
        assert_eq!(at(t0 + GRACE - Duration::from_secs(1)), vec![]);
        // `running`은 건드리지 않는다.
        let error = format!("node {} is offline", down.id);
        assert_eq!(at(t0 + GRACE), vec![fail(&pending, error)]);
    }

    #[test]
    fn coming_back_online_restarts_the_grace() {
        let mut n = node(false);
        let members = BTreeSet::from([n.id]);
        let pending = task(TaskOp::VmStart, n.id, TaskState::Pending);
        let tasks = std::slice::from_ref(&pending);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let half = GRACE / 2;
        assert_eq!(
            decide(
                t0,
                &CONFIG,
                std::slice::from_ref(&n),
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        n.status.online = true;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + half, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert!(seen.offline_since.is_empty());
        // 다시 오프라인이 된 때부터 잰다.
        n.status.online = false;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + GRACE, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + GRACE + half,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + 2 * GRACE,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            )
            .len(),
            1
        );
    }

    #[test]
    fn unfinished_tasks_of_a_removed_node_fail_at_once() {
        let up = node(true);
        let gone = Uuid::now_v7();
        let pending = task(TaskOp::VmStart, gone, TaskState::Pending);
        let running = task(TaskOp::VmStop, gone, TaskState::Running);
        let done = task(TaskOp::VmStop, gone, TaskState::Succeeded);
        let tasks = [pending.clone(), running.clone(), done];
        let error = format!("node {gone} was removed");
        let mut seen = Seen::default();
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![fail(&pending, error.clone()), fail(&running, error)]
        );
        // 멤버십에 있으면 아직 노드를 등록하지 않은 새 노드다.
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id, gone]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
    }

    #[test]
    fn pending_snapshots_without_a_create_task_go_after_the_orphan_grace() {
        let up = node(true);
        let nodes = std::slice::from_ref(&up);
        let members = BTreeSet::from([up.id]);
        let (orphan, busy, mut ready) =
            (pending_snapshot(), pending_snapshot(), pending_snapshot());
        ready.status.phase = SnapshotPhase::Ready;
        let create =
            |s: &Snapshot, state| task(TaskOp::SnapshotCreate { snapshot: s.id }, up.id, state);
        // 끝난 만들기 Task는 세지 않는다.
        let tasks = [
            create(&busy, TaskState::Running),
            create(&orphan, TaskState::Failed),
        ];
        let snapshots = [orphan.clone(), busy, ready];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, nodes, &members, &tasks, &snapshots, &mut seen);
        // tick이 짧아도 처음 본 뒤 유예가 지나기 전에는 지우지 않는다.
        for ms in (0..ORPHAN_GRACE.as_millis() as u64).step_by(100) {
            assert_eq!(at(t0 + Duration::from_millis(ms)), vec![], "{ms}ms");
        }
        let delete = Action::DeleteSnapshot {
            id: orphan.id,
            revision: orphan.revision,
        };
        assert_eq!(at(t0 + ORPHAN_GRACE), vec![delete]);

        // 고아로 보인 뒤 Task가 보이면 지우지 않고, 다시 고아가 된 때부터 잰다.
        let mut seen = Seen::default();
        let snapshots = std::slice::from_ref(&orphan);
        let started = [create(&orphan, TaskState::Pending)];
        let mut at = |now, tasks: &[Task]| {
            decide(now, &CONFIG, nodes, &members, tasks, snapshots, &mut seen)
        };
        assert_eq!(at(t0, &[]), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &started), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &[]), vec![]);
        assert_eq!(
            at(t0 + 2 * ORPHAN_GRACE - Duration::from_secs(1), &[]),
            vec![]
        );
        assert_eq!(at(t0 + 2 * ORPHAN_GRACE, &[]).len(), 1);
    }

    /// `ago` 전에 만든 끝난(`state`) Task
    fn task_made(now: SystemTime, ago: Duration, state: TaskState) -> Task {
        let at = (now - ago).duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), state);
        t.id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            at.as_secs(),
            at.subsec_nanos(),
        ));
        t
    }

    fn delete(t: &Task) -> Action {
        Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        }
    }

    /// 끝난 것을 처음 본 뒤 `finished_grace`가 지난 리더가 고르는 것
    fn expired(
        now: SystemTime,
        config: &ClusterJanitorConfig,
        tasks: &[Task],
        limit: usize,
    ) -> Vec<Action> {
        let t0 = Instant::now();
        let mut seen = Seen::default();
        expired_tasks(now, t0, config, tasks, &mut seen, limit);
        let later = t0 + config.finished_grace;
        expired_tasks(now, later, config, tasks, &mut seen, limit)
    }

    #[test]
    fn just_finished_tasks_over_the_max_count_stay_for_the_grace() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let day = Duration::from_secs(24 * 3600);
        let older = task_made(now, 2 * min, TaskState::Succeeded);
        let newer = task_made(now, min, TaskState::Succeeded);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at =
            |instant, tasks: &[Task]| expired_tasks(now, instant, &config, tasks, &mut seen, 200);
        // 막 끝난 것으로 보인 Task는 개수가 넘어도 남는다.
        let tasks = [older.clone(), newer.clone()];
        assert_eq!(at(t0, &tasks), vec![]);
        assert_eq!(
            at(t0 + FINISHED_GRACE - Duration::from_secs(1), &tasks),
            vec![]
        );
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&older)]);
        // 지워진 Task는 잊고, 새로 끝난 Task는 처음 본 때부터 잰다.
        let newest = task_made(now, Duration::ZERO, TaskState::Failed);
        let tasks = [newer.clone(), newest];
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&newer)]);
        // 기간 기준도 끝난 것을 처음 본 뒤 유예를 기다린다(오래 꺼져 있던 노드의 Task가 늦게 실패한 경우).
        let old = task_made(now, 8 * day, TaskState::Failed);
        let tasks = std::slice::from_ref(&old);
        assert_eq!(at(t0 + FINISHED_GRACE, tasks), vec![]);
        assert_eq!(
            at(t0 + 2 * FINISHED_GRACE - Duration::from_secs(1), tasks),
            vec![]
        );
        assert_eq!(at(t0 + 2 * FINISHED_GRACE, tasks), vec![delete(&old)]);
        assert!(seen.finished.contains_key(&old.id));
        assert!(!seen.finished.contains_key(&older.id));
    }

    #[test]
    fn finished_tasks_older_than_the_max_age_go() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let old_ok = task_made(now, 8 * day, TaskState::Succeeded);
        let old_failed = task_made(now, 9 * day, TaskState::Failed);
        let young = task_made(now, 6 * day, TaskState::Succeeded);
        // 끝나지 않은 Task는 오래됐어도 남긴다.
        let old_pending = task_made(now, 30 * day, TaskState::Pending);
        let old_running = task_made(now, 30 * day, TaskState::Running);
        let tasks = [
            old_ok.clone(),
            young,
            old_pending,
            old_failed.clone(),
            old_running,
        ];
        // 오래된 것부터 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 200),
            vec![delete(&old_failed), delete(&old_ok)]
        );
    }

    #[test]
    fn finished_tasks_over_the_max_count_go_oldest_first() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 2,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let finished: Vec<Task> = (1..=4)
            .map(|i| task_made(now, i * min, TaskState::Succeeded))
            .collect();
        let pending = task_made(now, 10 * min, TaskState::Pending);
        let mut tasks = finished.clone();
        tasks.push(pending);
        // 끝나지 않은 Task는 세지 않는다.
        assert_eq!(
            expired(now, &config, &tasks, 200),
            vec![delete(&finished[3]), delete(&finished[2])]
        );
        assert_eq!(expired(now, &config, &tasks[2..], 200), vec![]);
    }

    #[test]
    fn a_task_exactly_at_the_max_age_stays_and_ties_go_by_id() {
        let now = SystemTime::now();
        // id의 시각은 밀리초까지다.
        let now = SystemTime::UNIX_EPOCH
            + Duration::from_millis(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            );
        let at_limit = task_made(now, CONFIG.task_max_age, TaskState::Succeeded);
        assert_eq!(expired(now, &CONFIG, &[at_limit], 200), vec![]);

        // 만든 시각이 같으면 id가 작은 것을 먼저 지운다.
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let mut tied = [
            task_made(now, min, TaskState::Succeeded),
            task_made(now, min, TaskState::Failed),
        ];
        assert_ne!(tied[0].id, tied[1].id);
        tied.sort_by_key(|t| std::cmp::Reverse(t.id));
        assert_eq!(expired(now, &config, &tied, 200), vec![delete(&tied[1])]);
    }

    #[test]
    fn deletes_per_tick_are_capped() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let tasks: Vec<Task> = (0..5)
            .map(|i| task_made(now, (10 + i) * day, TaskState::Failed))
            .collect();
        // 상한까지 오래된 것부터 고르고 나머지는 다음 tick에 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 2),
            vec![delete(&tasks[4]), delete(&tasks[3])]
        );
        assert_eq!(expired(now, &CONFIG, &tasks[..3], 2).len(), 2);
        assert_eq!(expired(now, &CONFIG, &tasks[..1], 2).len(), 1);
    }

    #[test]
    fn a_task_delete_that_cannot_be_written_stops_the_rest_of_the_deletes() {
        let t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Succeeded);
        let rejected = || {
            ClusterError::Rejected(Rejection::RevisionMismatch {
                expected: 1,
                actual: 2,
            })
        };
        let not_leader = || ClusterError::NotLeader {
            leader_id: None,
            leader_addr: None,
        };
        // 쓰기를 받지 못하면 남은 지우기를 다음 tick으로 미룬다.
        for err in [
            ClusterError::Timeout,
            ClusterError::Unreachable("x".into()),
            not_leader(),
        ] {
            assert!(stops_deletes(&delete(&t), &err), "{err:?}");
        }
        // 그 Task만 바뀌었거나 없어진 것이면 다음 것을 지운다.
        assert!(!stops_deletes(&delete(&t), &rejected()));
        // 다른 동작의 실패는 지우기를 막지 않는다.
        assert!(!stops_deletes(
            &fail(&t, "x".into()),
            &ClusterError::Timeout
        ));
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    #[tokio::test]
    async fn failing_a_task_the_executor_started_meanwhile_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        // 정리 작업이 `pending`으로 읽은 뒤 실행기가 먼저 시작했다.
        let action = fail(&t, "node x is offline".into());
        let running = TaskStatus {
            state: TaskState::Running,
            error: None,
        };
        cluster
            .update_status_checked::<TaskSpec>(t.id, t.revision, &running)
            .await
            .unwrap();
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClusterError::Rejected(Rejection::RevisionMismatch { .. })
            ),
            "{err:?}"
        );
        let got = cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
        assert_eq!(got.status, running);
    }

    #[tokio::test]
    async fn a_rejected_failure_that_was_already_written_counts_as_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        let action = fail(&t, "node x is offline".into());
        apply(&cluster, &action).await.unwrap();
        // 응답만 잃고 다시 보낸 쓰기는 거부되지만 Task는 이 오류로 실패해 있다.
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(written_anyway(&cluster, &action, &err), "{err:?}");
        // 다른 오류로 실패해 있으면 다른 쪽이 쓴 것이다.
        let other = fail(&t, "node x was removed".into());
        let err = apply(&cluster, &other).await.unwrap_err();
        assert!(!written_anyway(&cluster, &other, &err), "{err:?}");
    }
}
