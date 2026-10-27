//! 예약 스냅샷(설계 §5). 리더만 `tick`마다 예약이 있는 VM을 보고, 보관 개수를 넘은 예약 스냅샷을
//! 하나 지우거나 주기가 지났으면 새 예약 스냅샷을 요청한다. 요청은 API 핸들러와 같은 함수로 하므로
//! 같은 검사를 받는다. 거절되면 그 이유(API 오류 code·메시지)를 `status.schedule_error`에 남긴다.
//! 만들기 Task의 결과와 예약 정리 삭제(`scheduled: true`) Task의 실패는 Task 실행기(노드가 끝내기
//! 전에 정리 작업이 끝냈으면 정리 작업)가 `task_failed`로 남긴다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{ScheduleError, ScheduleEvery, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::ApiError;
use crate::snapshots::{start_create, start_delete, vm_placement};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct SnapshotSchedulerConfig {
    /// 예약을 확인하는 간격
    pub tick: Duration,
    /// 주기의 한 시간. 매일은 24배, 매주는 168배다. 초 단위로 센다(스냅샷의 `created_at`이 초다).
    /// 테스트만 줄인다.
    pub hour: Duration,
}

impl Default for SnapshotSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            hour: Duration::from_secs(3600),
        }
    }
}

/// 예약 스냅샷 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct SnapshotScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl SnapshotScheduler {
    pub fn spawn(state: Arc<AppState>, config: SnapshotSchedulerConfig) -> Self {
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
    config: SnapshotSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    // VM별 마지막 예약 만들기·삭제 시도. 메모리에만 두므로 리더가 바뀌거나 opld가 다시 시작하면 잊고,
    // 그때는 한 번 일찍 다시 시도한다.
    let mut attempts: HashMap<Uuid, Attempts> = HashMap::new();
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
            _ = tick(&state, &config, &mut attempts) => {}
        }
    }
}

/// 예약 작업이 VM에 할 일
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// 보관 개수를 넘은 가장 오래된 예약 스냅샷을 지운다.
    Delete(Uuid),
    /// 이 이름으로 예약 스냅샷을 만든다.
    Create { name: String, memory: bool },
}

/// 마지막 예약 만들기 시도와 그 시각(초)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// 요청이 4xx로 거절되었다(바쁨, 노드 오프라인 등). 잠깐일 수 있으므로 한 시간 뒤에 다시 한다.
    Refused(u64),
    /// Task를 만들었다. 실패했으면 다음 주기에 다시 한다.
    Sent(u64),
}

/// VM 하나에 예약 작업이 한 마지막 시도
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Attempts {
    create: Option<Attempt>,
    /// 지우려 한(요청했거나 거절된) 예약 스냅샷과 그 시각
    delete: Option<(Uuid, u64)>,
}

/// 주기의 길이(초)
fn period_secs(every: ScheduleEvery, hour: Duration) -> u64 {
    let hours = match every {
        ScheduleEvery::Hourly => 1,
        ScheduleEvery::Daily => 24,
        ScheduleEvery::Weekly => 168,
    };
    hour.as_secs() * hours
}

/// `auto-YYYYMMDD-HHMM`(UTC). VM에 같은 이름이 있으면 `-2`, `-3`…을 붙인다.
fn auto_name(now: u64, snapshots: &[Snapshot]) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let base = format!(
        "auto-{:04}{:02}{:02}-{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    );
    let taken = |name: &str| snapshots.iter().any(|s| s.spec.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .unwrap()
}

/// VM 하나에 지금 할 일. `snapshots`는 이 VM의 스냅샷이다. 예약이 없거나, 이 VM의 스냅샷·복제
/// Task가 끝나지 않았거나, 만드는 중인 스냅샷이 있으면 아무것도 하지 않는다. 보관 개수를 넘었으면
/// 한 번에 하나씩 지우고(만들지 않는다), 아니면 마지막 예약 스냅샷에서 주기가 지났을 때 만든다.
/// 마지막 만들기 시도가 그 뒤면, 거절된 것은 한 시간 뒤에, 보낸 Task가 실패한 것은 다음 주기에
/// 다시 한다. 지우려 한 스냅샷이 아직 있으면(지우기가 실패했다) 그 삭제는 한 시간 뒤에 다시 하고,
/// 그동안은 만들기를 본다(보관 정리가 막혀도 새 스냅샷은 만든다).
fn plan(
    now: u64,
    hour: Duration,
    vm: &Vm,
    snapshots: &[Snapshot],
    tasks: &[Task],
    attempts: &Attempts,
) -> Option<Plan> {
    let schedule = vm.spec.snapshot_schedule?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::SnapshotCreate { .. }
                    | TaskOp::SnapshotRevert { .. }
                    | TaskOp::SnapshotDelete { .. }
                    | TaskOp::VmClone { .. }
            )
    });
    if busy
        || snapshots
            .iter()
            .any(|s| s.status.phase == SnapshotPhase::Pending)
    {
        return None;
    }
    let mut scheduled: Vec<&Snapshot> = snapshots
        .iter()
        .filter(|s| s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
        .collect();
    scheduled.sort_by_key(|s| s.status.created_at);
    if scheduled.len() > schedule.keep as usize {
        let oldest = scheduled[0].id;
        // 끝나지 않은 Task가 없는데 아직 있으면 지우기가 실패했거나 거절되었다.
        let failed = attempts
            .delete
            .is_some_and(|(id, at)| id == oldest && now < at + hour.as_secs());
        if !failed {
            return Some(Plan::Delete(oldest));
        }
    }
    let period = period_secs(schedule.every, hour);
    let retry = attempts.create.map(|a| match a {
        Attempt::Refused(at) => at + hour.as_secs(),
        Attempt::Sent(at) => at + period,
    });
    let due = scheduled
        .last()
        .and_then(|s| s.status.created_at)
        .map(|last| last + period)
        .max(retry);
    if due.is_some_and(|due| now < due) {
        return None;
    }
    let running = vm.status.phase == VmPhase::Running;
    Some(Plan::Create {
        name: auto_name(now, snapshots),
        memory: schedule.memory && running && !vm.spec.disks.is_empty(),
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn tick(
    state: &AppState,
    config: &SnapshotSchedulerConfig,
    attempts: &mut HashMap<Uuid, Attempts>,
) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        attempts.clear();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<VmSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
        ))
    })();
    let Ok((vms, snapshots, tasks)) = lists else {
        return;
    };
    attempts.retain(|id, _| vms.iter().any(|vm| vm.id == *id));
    let now = now_secs();
    for vm in vms.iter().filter(|vm| vm.spec.snapshot_schedule.is_some()) {
        let mine: Vec<Snapshot> = snapshots
            .iter()
            .filter(|s| s.spec.vm == vm.id)
            .cloned()
            .collect();
        let attempted = attempts.get(&vm.id).copied().unwrap_or_default();
        let Some(plan) = plan(now, config.hour, vm, &mine, &tasks, &attempted) else {
            continue;
        };
        let result = match check_node(state, vm) {
            Err(e) => Err(e),
            Ok(()) => match &plan {
                Plan::Delete(id) => start_delete(state, vm, &id.to_string(), true).await,
                Plan::Create { name, memory } => {
                    start_create(state, vm, name.clone(), String::new(), Some(*memory), true).await
                }
            },
        };
        // 요청했거나 거절된 것은 시도로 센다. 저장소 오류는 VM의 잘못이 아니므로 세지 않는다.
        let refused = result.as_ref().is_err_and(|e| e.status.is_client_error());
        if result.is_ok() || refused {
            let entry = attempts.entry(vm.id).or_default();
            match &plan {
                Plan::Delete(id) => entry.delete = Some((*id, now)),
                Plan::Create { .. } if refused => entry.create = Some(Attempt::Refused(now)),
                Plan::Create { .. } => entry.create = Some(Attempt::Sent(now)),
            }
        }
        match result {
            Ok(task) => {
                tracing::info!(vm = %vm.name, task = %task.id, ?plan, "scheduled snapshot task")
            }
            // 검사에 걸렸다. 다음 tick에 다시 본다.
            Err(e) if e.status.is_client_error() => {
                tracing::info!(vm = %vm.name, ?plan, error = %e.message, "scheduled snapshot refused");
                let error = ScheduleError {
                    code: e.code.into(),
                    message: e.message,
                };
                if let Err(w) = set_schedule_error(cluster, vm.id, Some(error)).await {
                    tracing::warn!(vm = %vm.name, error = %w, "cannot record schedule error");
                }
            }
            // 저장소 오류(리더를 잃는 등)는 VM의 잘못이 아니므로 남기지 않는다.
            Err(e) => {
                tracing::warn!(vm = %vm.name, ?plan, error = %e.message, "cannot request scheduled snapshot")
            }
        }
    }
}

/// Task를 실행할 노드가 오프라인이면 요청하지 않는다(Task가 그 노드가 돌아올 때까지 남는다).
fn check_node(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let Some(node) = vm_placement(&state.cluster, vm) else {
        return Ok(());
    };
    match state.cluster.get::<NodeSpec>(node)? {
        Some(n) if n.status.online => Ok(()),
        n => Err(ApiError::conflict(
            "node_offline",
            format!(
                "node {:?} holding VM {:?} is offline",
                n.map_or_else(|| node.to_string(), |n| n.name),
                vm.name
            ),
        )),
    }
}

/// 실패를 VM의 `schedule_error`에 남길 Task면 그 VM: 예약 스냅샷의 만들기와 예약 정리의 삭제.
/// 사용자가 지운 예약 스냅샷의 삭제는 남기지 않는다. `find`는 id로 스냅샷을 찾는다. 예약 정리의
/// 삭제는 스냅샷이 이미 지워졌어도 남기도록 Task의 대상(VM)으로 정한다.
pub(crate) fn recorded_vm(
    task: &TaskSpec,
    find: impl FnOnce(Uuid) -> Option<Snapshot>,
) -> Option<Uuid> {
    match task.op {
        TaskOp::SnapshotCreate { snapshot } => find(snapshot)
            .filter(|s| s.spec.scheduled)
            .map(|s| s.spec.vm),
        TaskOp::SnapshotDelete {
            scheduled: true, ..
        } => Some(task.target),
        _ => None,
    }
}

/// 실패한 Task의 예약 오류
pub(crate) fn task_failed(message: String) -> ScheduleError {
    ScheduleError {
        code: "task_failed".into(),
        message,
    }
}

/// VM의 `status.schedule_error`를 `error`로 바꾼다. 같으면 쓰지 않는다. reconciler·Task 실행기도
/// status를 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다. VM이 없으면 그만둔다.
pub(crate) async fn set_schedule_error(
    cluster: &Cluster,
    vm: Uuid,
    error: Option<ScheduleError>,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<VmSpec>(vm)? else {
            return Ok(());
        };
        if current.status.schedule_error == error {
            return Ok(());
        }
        let status = VmStatus {
            schedule_error: error.clone(),
            ..current.status.clone()
        };
        match cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::task::{TaskState, TaskStatus};
    use opl_types::vm::{CpuSpec, DiskBus, DiskSpec, MachineType, PowerState, SnapshotSchedule};

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    /// 2026-10-27 05:07:00 UTC
    const NOW: u64 = 1_793_077_620;

    fn vm(every: ScheduleEvery, keep: u32, memory: bool) -> Vm {
        let mut vm: Vm = Resource::new(
            "web",
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![DiskSpec {
                    volume: "root".into(),
                    bus: DiskBus::VirtioBlk,
                    boot_order: None,
                }],
                nics: vec![],
                power: PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: Some(SnapshotSchedule {
                    every,
                    keep,
                    memory,
                }),
            },
        );
        vm.status.phase = VmPhase::Running;
        vm
    }

    fn snap(vm: &Vm, name: &str, scheduled: bool, created_at: Option<u64>) -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: vm.id,
                name: name.into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled,
                vm_spec: vm.spec.clone(),
            },
        );
        s.status = SnapshotStatus {
            phase: if created_at.is_some() {
                SnapshotPhase::Ready
            } else {
                SnapshotPhase::Pending
            },
            created_at,
            vmstate_bytes: None,
        };
        s
    }

    fn task(op: TaskOp, target: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target,
                node: Uuid::now_v7(),
            },
        );
        t.status = TaskStatus { state, error: None };
        t
    }

    /// 만들기를 시도한 적이 없을 때의 판단
    fn plan(
        now: u64,
        hour: Duration,
        vm: &Vm,
        snapshots: &[Snapshot],
        tasks: &[Task],
    ) -> Option<Plan> {
        super::plan(now, hour, vm, snapshots, tasks, &Attempts::default())
    }

    /// 만들기 시도 `attempt`가 있었을 때의 판단
    fn after(now: u64, vm: &Vm, snapshots: &[Snapshot], attempt: Attempt) -> Option<Plan> {
        let attempts = Attempts {
            create: Some(attempt),
            delete: None,
        };
        super::plan(now, HOUR, vm, snapshots, &[], &attempts)
    }

    fn create(name: &str, memory: bool) -> Option<Plan> {
        Some(Plan::Create {
            name: name.into(),
            memory,
        })
    }

    #[test]
    fn names_are_utc_minutes_with_a_number_when_taken() {
        let vm = vm(ScheduleEvery::Hourly, 3, false);
        assert_eq!(auto_name(NOW, &[]), "auto-20261027-0507");
        assert_eq!(auto_name(0, &[]), "auto-19700101-0000");
        let taken = [
            snap(&vm, "auto-20261027-0507", true, Some(1)),
            snap(&vm, "auto-20261027-0507-2", false, Some(2)),
        ];
        assert_eq!(auto_name(NOW, &taken), "auto-20261027-0507-3");
    }

    #[test]
    fn creates_first_and_after_each_period() {
        let hourly = vm(ScheduleEvery::Hourly, 3, false);
        // 예약 스냅샷이 없으면 바로 만든다. 사용자가 만든 스냅샷은 주기에 세지 않는다.
        let user = snap(&hourly, "mine", false, Some(NOW - 10));
        assert_eq!(
            plan(NOW, HOUR, &hourly, std::slice::from_ref(&user), &[]),
            create("auto-20261027-0507", false)
        );
        let last = snap(&hourly, "a", true, Some(NOW - 3599));
        assert_eq!(plan(NOW, HOUR, &hourly, &[last.clone(), user], &[]), None);
        assert_eq!(
            plan(NOW + 1, HOUR, &hourly, std::slice::from_ref(&last), &[]),
            create("auto-20261027-0507", false)
        );
        // 매일은 24시간, 매주는 168시간이다.
        let daily = vm(ScheduleEvery::Daily, 3, false);
        let last = snap(&daily, "a", true, Some(NOW - 24 * 3600 + 1));
        assert_eq!(
            plan(NOW, HOUR, &daily, std::slice::from_ref(&last), &[]),
            None
        );
        assert!(plan(NOW + 1, HOUR, &daily, &[last], &[]).is_some());
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        let last = snap(&weekly, "a", true, Some(NOW - 167 * 3600));
        assert_eq!(
            plan(NOW, HOUR, &weekly, std::slice::from_ref(&last), &[]),
            None
        );
        assert!(plan(NOW + 3600, HOUR, &weekly, &[last], &[]).is_some());
        // 테스트용 짧은 한 시간
        let last = snap(&hourly, "a", true, Some(NOW - 2));
        assert!(plan(NOW, Duration::from_secs(2), &hourly, &[last], &[]).is_some());
    }

    #[test]
    fn a_failed_attempt_waits_a_period() {
        let v = vm(ScheduleEvery::Hourly, 3, false);
        // 예약 스냅샷이 없어도 방금 Task를 보냈으면(실패해 스냅샷이 없다) 다음 tick에 다시 하지 않고
        // 한 주기를 기다린다.
        let attempt = Attempt::Sent(NOW);
        assert_eq!(after(NOW + 60, &v, &[], attempt), None);
        assert_eq!(after(NOW + 3599, &v, &[], attempt), None);
        assert!(after(NOW + 3600, &v, &[], attempt).is_some());
        // 마지막 예약 스냅샷 뒤에 실패한 시도가 있으면 그 시도에서 센다.
        let last = snap(&v, "a", true, Some(NOW - 3600));
        let snaps = std::slice::from_ref(&last);
        assert_eq!(after(NOW + 10, &v, snaps, attempt), None);
        assert!(after(NOW + 3600, &v, snaps, attempt).is_some());
        // 마지막 스냅샷보다 앞선 시도는 상관없다.
        for old in [Attempt::Sent(NOW - 7200), Attempt::Refused(NOW - 7200)] {
            assert_eq!(
                after(NOW + 10, &v, snaps, old),
                create("auto-20261027-0507", false)
            );
        }
        // 매주면 실패한 Task 뒤 일주일을 기다린다.
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        assert_eq!(after(NOW + 3600, &weekly, &[], attempt), None);
        assert!(after(NOW + 168 * 3600, &weekly, &[], attempt).is_some());
        // 넘친 것을 지우는 일은 시도와 상관없다.
        let keep1 = vm(ScheduleEvery::Hourly, 1, false);
        let (a, b) = (
            snap(&keep1, "a", true, Some(NOW - 10)),
            snap(&keep1, "b", true, Some(NOW - 5)),
        );
        assert_eq!(
            after(NOW, &keep1, &[a.clone(), b], attempt),
            Some(Plan::Delete(a.id))
        );
    }

    #[test]
    fn a_failed_delete_waits_an_hour_and_lets_creation_go_on() {
        let v = vm(ScheduleEvery::Hourly, 1, false);
        let (a, b) = (
            snap(&v, "a", true, Some(NOW - 2 * 3600)),
            snap(&v, "b", true, Some(NOW - 1800)),
        );
        let snaps = [a.clone(), b.clone()];
        // 방금 지우려 한 a가 (끝나지 않은 Task 없이) 아직 있다. 다시 지우지 않고, 만들 때가 아니면
        // 아무것도 하지 않는다.
        let failed = Attempts {
            create: None,
            delete: Some((a.id, NOW - 60)),
        };
        assert_eq!(super::plan(NOW, HOUR, &v, &snaps, &[], &failed), None);
        // 만들 때가 되면 만든다.
        assert_eq!(
            super::plan(NOW + 1800, HOUR, &v, &snaps, &[], &failed),
            create("auto-20261027-0537", false)
        );
        // 지우기 Task가 아직 끝나지 않았으면 실패가 아니라 기다린다.
        let t = task(
            TaskOp::SnapshotDelete {
                snapshot: a.id,
                scheduled: false,
            },
            v.id,
            TaskState::Running,
        );
        assert_eq!(super::plan(NOW, HOUR, &v, &snaps, &[t], &failed), None);
        // 한 시간 뒤에 다시 지운다.
        assert_eq!(
            super::plan(NOW - 60 + 3600, HOUR, &v, &snaps, &[], &failed),
            Some(Plan::Delete(a.id))
        );
        // 다른 스냅샷을 지우려 했던 것은 상관없다.
        let other = Attempts {
            create: None,
            delete: Some((b.id, NOW - 60)),
        };
        assert_eq!(
            super::plan(NOW, HOUR, &v, &snaps, &[], &other),
            Some(Plan::Delete(a.id))
        );
    }

    #[test]
    fn a_refused_attempt_waits_an_hour() {
        // 거절(바쁨, 노드 오프라인 등)은 잠깐일 수 있으므로 주기가 길어도 한 시간 뒤에 다시 한다.
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        let attempt = Attempt::Refused(NOW);
        assert_eq!(after(NOW + 60, &weekly, &[], attempt), None);
        assert_eq!(after(NOW + 3599, &weekly, &[], attempt), None);
        assert!(after(NOW + 3600, &weekly, &[], attempt).is_some());
        // 마지막 예약 스냅샷에서 주기가 지나지 않았으면 거절과 상관없이 기다린다.
        let last = snap(&weekly, "a", true, Some(NOW - 3600));
        let snaps = std::slice::from_ref(&last);
        assert_eq!(after(NOW + 7200, &weekly, snaps, attempt), None);
    }

    #[test]
    fn memory_only_when_asked_running_and_with_disks() {
        let mut v = vm(ScheduleEvery::Hourly, 3, true);
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", true)
        );
        v.status.phase = VmPhase::Stopped;
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", false)
        );
        v.status.phase = VmPhase::Running;
        v.spec.disks.clear();
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", false)
        );
        let off = vm(ScheduleEvery::Hourly, 3, false);
        assert_eq!(
            plan(NOW, HOUR, &off, &[], &[]),
            create("auto-20261027-0507", false)
        );
    }

    #[test]
    fn deletes_the_oldest_scheduled_one_at_a_time_before_creating() {
        let v = vm(ScheduleEvery::Hourly, 2, false);
        let (a, b, c) = (
            snap(&v, "a", true, Some(NOW - 3 * 3600)),
            snap(&v, "b", true, Some(NOW - 2 * 3600)),
            snap(&v, "c", true, Some(NOW - 3 * 3600 - 1)),
        );
        // 사용자가 만든 오래된 스냅샷은 세지도 지우지도 않는다.
        let user = snap(&v, "mine", false, Some(1));
        let all = [a.clone(), user.clone(), b.clone(), c.clone()];
        // 주기가 지났어도 넘친 것을 먼저 지운다.
        assert_eq!(plan(NOW, HOUR, &v, &all, &[]), Some(Plan::Delete(c.id)));
        assert_eq!(
            plan(NOW, HOUR, &v, &[a.clone(), user.clone(), b.clone()], &[]),
            create("auto-20261027-0507", false)
        );
        // keep개 이하면 지우지 않는다.
        assert_eq!(
            plan(NOW - 2 * 3600 + 10, HOUR, &v, &[a, user, b], &[]),
            None
        );
    }

    #[test]
    fn skips_without_schedule_or_with_unfinished_work() {
        let mut none = vm(ScheduleEvery::Hourly, 2, false);
        none.spec.snapshot_schedule = None;
        assert_eq!(plan(NOW, HOUR, &none, &[], &[]), None);

        let v = vm(ScheduleEvery::Hourly, 2, false);
        let pending = snap(&v, "p", false, None);
        assert_eq!(plan(NOW, HOUR, &v, &[pending], &[]), None);
        let some = Uuid::now_v7();
        for op in [
            TaskOp::SnapshotCreate { snapshot: some },
            TaskOp::SnapshotRevert { snapshot: some },
            TaskOp::SnapshotDelete {
                snapshot: some,
                scheduled: false,
            },
            TaskOp::VmClone {
                name: "copy".into(),
                snapshot: None,
                pool: None,
            },
        ] {
            for state in [TaskState::Pending, TaskState::Running] {
                let t = task(op.clone(), v.id, state);
                assert_eq!(plan(NOW, HOUR, &v, &[], &[t]), None, "{op:?}");
            }
            // 끝난 Task나 다른 VM의 Task는 상관없다.
            let done = task(op.clone(), v.id, TaskState::Failed);
            let other = task(op, Uuid::now_v7(), TaskState::Running);
            assert!(plan(NOW, HOUR, &v, &[], &[done, other]).is_some());
        }
        // 전원 Task는 만들기 요청의 검사(vm_busy)가 거절한다.
        let start = task(TaskOp::VmStart, v.id, TaskState::Running);
        assert!(plan(NOW, HOUR, &v, &[], &[start]).is_some());
    }

    #[test]
    fn records_scheduled_creation_and_scheduled_deletion() {
        let v = vm(ScheduleEvery::Hourly, 3, false);
        let (auto, manual) = (snap(&v, "a", true, None), snap(&v, "m", false, None));
        let find = |s: &Snapshot| {
            let s = s.clone();
            move |_| Some(s)
        };
        let create = |s: &Snapshot| {
            task(
                TaskOp::SnapshotCreate { snapshot: s.id },
                v.id,
                TaskState::Failed,
            )
        };
        assert_eq!(recorded_vm(&create(&auto).spec, find(&auto)), Some(v.id));
        assert_eq!(recorded_vm(&create(&manual).spec, find(&manual)), None);
        let delete = |scheduled| {
            let op = TaskOp::SnapshotDelete {
                snapshot: auto.id,
                scheduled,
            };
            task(op, v.id, TaskState::Failed)
        };
        // 예약 정리의 삭제는 스냅샷이 이미 지워졌어도 대상 VM으로 남긴다.
        assert_eq!(recorded_vm(&delete(true).spec, |_| None), Some(v.id));
        // 사용자가 지운 예약 스냅샷의 삭제는 남기지 않는다.
        assert_eq!(recorded_vm(&delete(false).spec, find(&auto)), None);
    }
}
