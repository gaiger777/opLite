//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod clone;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_lv_snapshots, vm_placement};

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Debug, Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨을 다시 지워 보는 간격
    pub orphan_retry: Duration,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            orphan_retry: Duration::from_secs(60),
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
    retry.abort();
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_clone = matches!(task.spec.op, TaskOp::VmClone { .. });
        return (is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op) || vm_clone)
            .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| uses(&vm.spec))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error).await;
        } else {
            execute(&ctx, task).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    if !begin(ctx, task.id).await {
        return;
    }
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", ctx.config.timeout)),
    };
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
async fn cleanup(ctx: &Ctx, task: &Task) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op
        && let Err(e) = snapshot::discard_pending(ctx, snapshot).await
    {
        tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let TaskOp::VmClone { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

async fn perform(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
    }
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. LVM-thin 볼륨은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에
/// 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    match user {
        Some((vm, _)) if matches!(vm.status.phase, VmPhase::Starting | VmPhase::Stopping) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. LV는 QEMU가
        // 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 새 크기를 알린다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            if uses_lv_snapshots(&pool) {
                ctx.storage
                    .resize_volume(&pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => ctx
            .storage
            .resize_volume(&pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let wanted = unfinished
            .iter()
            .any(|t| matches!(&t.spec.op, TaskOp::VolumeImport { source, .. } if *source == path));
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
    unmounted
}
