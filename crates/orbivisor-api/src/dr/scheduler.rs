//! DR 실행 엔진(계획 76 설계 §3.2): 리더가 틱마다(5초) 도는 `DrRun`마다 관찰(Task 결과, VM 상태, 게스트 에이전트의 답·주소,
//! 네트워크, 사본, 계획된 이전이면 운영 쪽)을 모아 순수 상태 기계 [`dr::advance`]에 넣고, 나온 할 일([`DrAction`])을 지금
//! 길(복원·`vm-start`·VM 지우기 Task, SDN 네트워크, 운영 쪽 프록시 부름)로 한 뒤 결과를 [`DrRunStatus::record`]로 적어 저장한다.
//!
//! - 이어 가기: 진행은 실행 기록(Raft)에 있다. 새 리더는 기록에서 이어 가고, 할 일을 내기 전에 이미 낸 것을 찾는다 — 같은
//!   이름·복구 지점의 복원 Task, 같은 VM의 시작·정지 Task(단계에 든 뒤 만든 것), 운영 쪽은 실행마다 정한 백업 이름(409
//!   `name_taken`이면 그 백업)과 이미 이 클러스터로 가는 사본. 그래서 기록을 저장하기 전에 리더가 바뀌어도 같은 일을 두 번
//!   하지 않는다.
//! - 되살린 VM: 복원 Task가 끝나면 그 VM(Task 대상 id)에 라벨(`orbivisor.io/dr`·`orbivisor.io/dr-test` = 계획)을 단다.
//!   `ha`는 복원이 늘 뺀다. 시험 폴더 `dr-test/<plan>`는 없으면 만들고, 계획의 `folder`(장애 조치)가 없으면 복원 기본(이곳에
//!   있는 백업한 VM의 폴더, 아니면 루트)에 둔다(경고 로그).
//! - 거품 네트워크: 라우터·업링크 없는 SDN 네트워크. 서브넷은 본뜰 DR 네트워크(`like`)의 서브넷(유동 IP 풀·IPv6 알리기
//!   빼고), 그것이 없으면 그 원본 네트워크에 붙는 NIC의 IP 규칙 주소에서 셈(`10.20.0.5/24` → `10.20.0.0/24`), 둘 다 없으면
//!   서브넷 없음(L2만).
//! - 뒷정리는 시험 라벨이 이 계획인 VM과 네트워크만 지운다(같은 이름의 다른 VM은 건드리지 않는다). VM 디스크도 지운다.
//! - 끝: 계획 `status.run`을 비우고, 시험이면 `last_test`. 취소한 시험은 끝난 상태를 먼저 저장한 뒤(독점 값 `dr-run`을
//!   풀어야 한다) 뒷정리 실행을 시작한다. 경보 `dr_run_failed`는 끝난 실행을 본다.
//! - 계획된 이전의 운영 쪽(원격, 옛 판이어도 되는 지금 API만): 정지 `POST /vms/{vm}/actions/stop`(ACPI — 설계의 `shutdown`),
//!   5분 뒤 `?mode=force`(설계의 `stop`), 백업 `POST /vms/{vm}/backups`(그 VM의 가장 새 운영 쪽 백업과 같은 풀·저장소, 증분),
//!   복제 `POST /backups/{b}/actions/replicate`(운영 쪽에 등록된 이 클러스터 — CA 지문으로 찾음 — 의 백업 풀: 그 VM 사본의
//!   풀). 운영 쪽에 닿지 않으면 상태 기계가 기다린다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method};
use bytes::Bytes;
use http_body_util::Full;
use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_types::auth::Role;
use orbivisor_types::backup::BackupSpec;
use orbivisor_types::cloud_init::{CloudInitDns, CloudInitNet};
use orbivisor_types::dr::{
    self, DR_LABEL, DR_TEST_LABEL, DrAction, DrObservation, DrPhase, DrResult, DrRun, DrRunKind,
    DrRunSpec, DrRunStatus, DrWait, ERR_NAME_TAKEN, LastTest, RecoveryPlanSpec, RecoveryPlanStatus,
    SourceBackup, SourceObservation, TaskOutcome, VmObservation,
};
use orbivisor_types::folder::{FolderKind, FolderSpec, parse_folder_path};
use orbivisor_types::network::{NetworkSpec, SdnNet, Subnet};
use orbivisor_types::node::{DR_PLANS, NodeSpec};
use orbivisor_types::remote::{Remote, RemoteSpec};
use orbivisor_types::resource::Resource;
use orbivisor_types::security_group::Cidr;
use orbivisor_types::storage::StoragePoolSpec;
use orbivisor_types::task::{TaskOp, TaskSpec, TaskState};
use orbivisor_types::vm::{Vm, VmPhase, VmSpec};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::auth::Principal;
use crate::backups::{RestoreRequest, now, start_restore};
use crate::cluster::missing_feature;
use crate::error::{ApiError, Query};
use crate::remote_client::{RemoteClient, RemoteError};
use crate::state::AppState;
use crate::vms::{ActionQuery, StopMode, is_stopped};

/// 운영 쪽 부름 하나의 제한 시간
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct DrSchedulerConfig {
    /// 실행을 보는 간격
    pub tick: Duration,
}

impl Default for DrSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
        }
    }
}

/// DR 실행 반복 작업. 모든 노드에서 띄우고 리더인 노드에서만 일한다.
pub struct DrScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl DrScheduler {
    pub fn spawn(state: Arc<AppState>, config: DrSchedulerConfig) -> Self {
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

async fn run(state: Arc<AppState>, config: DrSchedulerConfig, mut shutdown: oneshot::Receiver<()>) {
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

/// 엔진이 API 처리기를 부를 때의 주체(실행을 시작한 관리자를 대신한다). 처리기 안의 권한 검사를 하지 않는 길만 이것으로
/// 부르고, 복원은 권한 검사를 빼는 [`start_restore`]로 한다.
fn engine() -> Principal {
    Principal {
        user: Uuid::nil(),
        name: "dr-engine".into(),
        role: Role::Admin,
        token: None,
    }
}

async fn tick(state: &Arc<AppState>) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    // 옛 노드는 실행 기록을 풀지 못한다(실행은 기능이 생긴 뒤에만 만들어지지만 옛 노드가 막 들어왔을 수 있다).
    if !missing_feature(cluster, DR_PLANS).is_ok_and(|old| old.is_empty()) {
        return;
    }
    let Ok((_, runs)) = cluster.list::<DrRunSpec>() else {
        return;
    };
    for run in runs.iter().filter(|r| !r.status.state.is_finished()) {
        if let Err(e) = Box::pin(step_run(state, run)).await {
            tracing::warn!(run = %run.name, error = %e, "DR run step failed");
        }
    }
    // 끝난 실행을 계획에 적지 못했으면(리더가 바뀜) 여기서 마저 한다.
    let Ok((_, plans)) = cluster.list::<RecoveryPlanSpec>() else {
        return;
    };
    for plan in plans {
        let Some(id) = plan.status.run else {
            continue;
        };
        match cluster.get::<DrRunSpec>(id) {
            Ok(Some(run)) if run.status.state.is_finished() => settle(cluster, &run).await,
            Ok(None) => clear_plan_run(cluster, plan.id, id).await,
            _ => {}
        }
    }
}

/// 실행 하나의 한 걸음
async fn step_run(state: &Arc<AppState>, run: &DrRun) -> Result<(), String> {
    let cluster = &state.cluster;
    let remote = cluster
        .get_by_name::<RemoteSpec>(&run.spec.plan_spec.source)
        .map_err(|e| e.to_string())?;
    let mut source = match (&remote, run.spec.kind) {
        (Some(r), DrRunKind::FailoverPlanned) => SourceSide::new(state, r),
        _ => None,
    };
    let obs = Box::pin(observe(state, run, remote.as_ref(), source.as_mut())).await?;
    label_restored(cluster, run, &obs).await;
    let (mut next, actions) = dr::advance(&run.spec, &run.status, &obs);
    for action in &actions {
        if matches!(action, DrAction::StartCleanup) {
            continue;
        }
        let result = Box::pin(act(state, run, &next, &obs, source.as_mut(), action)).await;
        if let DrResult::Failed(e) = &result {
            tracing::info!(run = %run.name, ?action, error = %e, "DR step failed");
        }
        next.record(action, result, obs.now);
    }
    if next == run.status {
        return Ok(());
    }
    if !save(cluster, run, &mut next).await {
        return Ok(());
    }
    if next.state.is_finished() {
        let done = DrRun {
            status: next,
            ..run.clone()
        };
        settle(cluster, &done).await;
        if actions.iter().any(|a| matches!(a, DrAction::StartCleanup)) {
            start_cleanup(state, &done).await;
        }
    }
    Ok(())
}

/// 상태를 저장한다. 그사이 취소 표시만 바뀌었으면(API) 합쳐 다시 쓰고, 다른 것이 바뀌었으면 이번 걸음을 버린다(다음 틱에
/// 기록에서 다시 — 이미 낸 Task는 찾아 쓴다).
async fn save(cluster: &Cluster, run: &DrRun, next: &mut DrRunStatus) -> bool {
    let mut base = run.status.clone();
    let mut revision = run.revision;
    for _ in 0..5 {
        match cluster
            .update_status_checked::<DrRunSpec>(run.id, revision, next)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                let Ok(Some(fresh)) = cluster.get::<DrRunSpec>(run.id) else {
                    return false;
                };
                let mut same = fresh.status.clone();
                same.cancel_requested = base.cancel_requested;
                if same != base {
                    return false;
                }
                next.cancel_requested |= fresh.status.cancel_requested;
                base = fresh.status;
                revision = fresh.revision;
            }
            Err(e) => {
                tracing::warn!(run = %run.name, error = %e, "cannot store the DR run");
                return false;
            }
        }
    }
    false
}

// ---- 관찰 ----

fn outcome(state: TaskState, error: Option<String>) -> TaskOutcome {
    match state {
        TaskState::Pending | TaskState::Running => TaskOutcome::Running,
        TaskState::Succeeded => TaskOutcome::Succeeded,
        TaskState::Failed => TaskOutcome::Failed(error.unwrap_or_else(|| "task failed".into())),
    }
}

/// 실행이 보는 VM 이름(되살린·지울 VM)
fn run_vms(status: &DrRunStatus) -> BTreeSet<String> {
    status
        .steps
        .iter()
        .filter_map(|s| s.new_vm.clone())
        .collect()
}

/// 그룹의 기다림(`guest`·`ip`)을 보려면 게스트 에이전트에 물어야 하는 단계인지
fn needs_guest(run: &DrRun, step: &dr::DrStep) -> bool {
    step.phase == DrPhase::Waiting
        && run
            .spec
            .plan_spec
            .groups
            .iter()
            .find(|g| g.name == step.group)
            .is_some_and(|g| g.wait != DrWait::Running)
}

async fn observe(
    state: &Arc<AppState>,
    run: &DrRun,
    remote: Option<&Remote>,
    source: Option<&mut SourceSide>,
) -> Result<DrObservation, String> {
    let cluster = &state.cluster;
    let err = |e: ClusterError| e.to_string();
    let mut obs = DrObservation {
        now: now(),
        source_ca: remote.map(|r| r.spec.ca_fingerprint.clone()),
        ..Default::default()
    };
    for step in &run.status.steps {
        if let Some(t) = step.task
            && let Some(task) = cluster.get::<TaskSpec>(t).map_err(err)?
        {
            obs.tasks
                .insert(t, outcome(task.status.state, task.status.error.clone()));
        }
    }
    let nodes: BTreeMap<Uuid, String> = cluster
        .list::<NodeSpec>()
        .map_err(err)?
        .1
        .into_iter()
        .map(|n| (n.id, n.name))
        .collect();
    let wanted = run_vms(&run.status);
    let guests: BTreeSet<String> = run
        .status
        .steps
        .iter()
        .filter(|s| needs_guest(run, s))
        .filter_map(|s| s.new_vm.clone())
        .collect();
    for vm in cluster.list::<VmSpec>().map_err(err)?.1 {
        if !wanted.contains(&vm.name) {
            continue;
        }
        let running = vm.status.phase == VmPhase::Running;
        let mut seen = VmObservation {
            running,
            node: vm
                .status
                .node
                .or(vm.spec.node)
                .and_then(|n| nodes.get(&n).cloned()),
            ..Default::default()
        };
        if running
            && guests.contains(&vm.name)
            && let Some(info) = Box::pin(crate::guest::cached_info(state, &vm)).await
        {
            seen.guest = true;
            seen.ips = info
                .interfaces
                .iter()
                .flat_map(|n| n.addresses.iter().map(|a| a.ip))
                .collect();
        }
        obs.vms.insert(vm.name.clone(), seen);
    }
    obs.networks = cluster
        .list::<NetworkSpec>()
        .map_err(err)?
        .1
        .into_iter()
        .map(|n| n.name)
        .collect();
    obs.backups = cluster.list::<BackupSpec>().map_err(err)?.1;
    // 계획된 이전의 운영 쪽 준비 중에만 원격을 본다.
    let preparing = run
        .status
        .steps
        .iter()
        .any(|s| s.phase == DrPhase::Preparing);
    if let Some(source) = source
        && preparing
        && !run.status.cancel_requested
    {
        obs.source = source.observe(&run.status).await;
    }
    Ok(obs)
}

/// 복원 Task가 끝난 VM(Task의 대상 id)에 계획 라벨을 단다.
async fn label_restored(cluster: &Cluster, run: &DrRun, obs: &DrObservation) {
    let key = if run.spec.kind == DrRunKind::Test {
        DR_TEST_LABEL
    } else {
        DR_LABEL
    };
    for step in &run.status.steps {
        let (DrPhase::Restoring, Some(t)) = (step.phase, step.task) else {
            continue;
        };
        if obs.tasks.get(&t) != Some(&TaskOutcome::Succeeded) {
            continue;
        }
        let Ok(Some(task)) = cluster.get::<TaskSpec>(t) else {
            continue;
        };
        let Ok(Some(vm)) = cluster.get::<VmSpec>(task.spec.target) else {
            continue;
        };
        if vm.labels.get(key) == Some(&run.spec.plan_name) {
            continue;
        }
        let mut labels = vm.labels.clone();
        labels.insert(key.to_string(), run.spec.plan_name.clone());
        if let Err(e) = cluster
            .update_spec::<VmSpec>(vm.id, Some(vm.revision), labels, &vm.spec)
            .await
        {
            tracing::warn!(vm = %vm.name, error = %e, "cannot label the recovered VM");
        }
    }
}

// ---- 할 일 ----

async fn act(
    state: &Arc<AppState>,
    run: &DrRun,
    status: &DrRunStatus,
    obs: &DrObservation,
    source: Option<&mut SourceSide>,
    action: &DrAction,
) -> DrResult {
    let failed = |e: String| DrResult::Failed(e);
    match action {
        DrAction::CreateBubble { bubble, name, like } => {
            let from = status
                .bubbles
                .get(*bubble)
                .map(|b| b.source.clone())
                .unwrap_or_default();
            create_bubble(state, run, status, obs, &from, name, like)
                .await
                .map_or_else(failed, |()| DrResult::Done)
        }
        DrAction::Restore { .. } => Box::pin(restore(state, run, status, obs, action))
            .await
            .unwrap_or_else(failed),
        DrAction::Start { step, vm } => Box::pin(start(state, status, *step, vm))
            .await
            .unwrap_or_else(failed),
        DrAction::DeleteVm { vm, .. } => Box::pin(delete_vm(state, run, vm))
            .await
            .unwrap_or_else(failed),
        DrAction::DeleteNetwork { name, .. } => delete_network(state, run, name)
            .await
            .map_or_else(failed, |()| DrResult::Done),
        DrAction::ShutdownSource { vm, .. } | DrAction::StopSource { vm, .. } => {
            let force = matches!(action, DrAction::StopSource { .. });
            match source {
                Some(s) => s.stop(vm, force).await,
                None => DrResult::Done,
            }
        }
        DrAction::BackupSource { vm, .. } => match source {
            Some(s) => s.backup(run, vm).await,
            None => DrResult::Done,
        },
        DrAction::ReplicateSource { step, backup } => match source {
            Some(s) => s.replicate(state, status, obs, *step, backup).await,
            None => DrResult::Done,
        },
        DrAction::StartCleanup => DrResult::Done,
    }
}

/// uuid v7 id의 만든 시각(유닉스 초)
fn made_at(id: Uuid) -> u64 {
    id.get_timestamp().map_or(0, |t| t.to_unix().0)
}

/// 단계에 든 뒤(`since`) 만든 Task 가운데 `pick`이 고른 것(새것부터)
fn task_since(
    cluster: &Cluster,
    since: u64,
    pick: impl Fn(&orbivisor_types::task::Task) -> bool,
) -> Result<Option<Uuid>, String> {
    let (_, tasks) = cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks
        .iter()
        .filter(|t| made_at(t.id) + 1 >= since && pick(t))
        .max_by_key(|t| t.id)
        .map(|t| t.id))
}

/// API 오류의 문구. 이름 겹침은 설계의 `dr_name_taken`으로 적는다.
fn api_error(e: ApiError) -> String {
    if e.code == "name_taken" {
        format!("{ERR_NAME_TAKEN}: {}", e.message)
    } else {
        e.message
    }
}

/// 시험 폴더(만듦)나 계획의 폴더의 id. id나 경로(`/a/b`), 빈 경로는 루트. 계획의 폴더가 없으면 `None` — VM을 실패시키지
/// 않고 복원 기본(백업한 VM의 폴더가 있으면 그것, 아니면 루트)에 둔다(설계 §3.1 `folder`는 고르는 것).
async fn resolve_folder(
    cluster: &Cluster,
    folder: &str,
    create: bool,
) -> Result<Option<Uuid>, String> {
    let (_, folders) = cluster.list::<FolderSpec>().map_err(|e| e.to_string())?;
    if let Ok(id) = folder.parse::<Uuid>() {
        return match folders.iter().find(|f| f.id == id) {
            Some(f) if f.spec.kind == FolderKind::Vm => Ok(Some(id)),
            _ => {
                tracing::warn!(%folder, "DR plan folder not found; restoring to the default folder");
                Ok(None)
            }
        };
    }
    let parts = parse_folder_path(folder).map_err(|e| e.to_string())?;
    let mut cur: Option<Uuid> = None;
    for part in parts {
        let found = folders.iter().find(|f| {
            f.spec.kind == FolderKind::Vm
                && f.spec.parent == cur
                && f.spec.name.eq_ignore_ascii_case(part)
        });
        cur = Some(match found {
            Some(f) => f.id,
            None if create => {
                let id = Uuid::now_v7();
                let spec = FolderSpec {
                    name: part.to_string(),
                    kind: FolderKind::Vm,
                    parent: cur,
                    quota: None,
                };
                spec.validate(id).map_err(|e| e.to_string())?;
                let mut f: Resource<FolderSpec, _> = Resource::new(id.to_string(), spec);
                f.id = id;
                cluster.create(&f).await.map_err(|e| e.to_string())?;
                id
            }
            None => {
                tracing::warn!(%folder, "DR plan folder not found; restoring to the default folder");
                return Ok(None);
            }
        });
    }
    Ok(cur)
}

/// 복원: 복구 지점을 새 VM으로(이미 낸 같은 복원 Task가 있으면 그것). 맵은 그 백업이 쓰는 이름만 남긴다(복원 API는 백업에
/// 없는 원본 이름을 받지 않는다). IP 규칙은 백업의 cloud-init과 합치고, 없는 NIC의 규칙은 버린다.
async fn restore(
    state: &Arc<AppState>,
    run: &DrRun,
    status: &DrRunStatus,
    obs: &DrObservation,
    action: &DrAction,
) -> Result<DrResult, String> {
    let DrAction::Restore {
        step,
        backup,
        name,
        pool,
        node,
        folder,
        networks,
        security_groups,
        ip,
        live,
        ..
    } = action
    else {
        return Err("not a restore".into());
    };
    let cluster = &state.cluster;
    let since = status.steps.get(*step).map_or(0, |s| s.at);
    let same = |t: &orbivisor_types::task::Task| match &t.spec.op {
        TaskOp::BackupRestore {
            backup: b, name: n, ..
        }
        | TaskOp::BackupLiveRestore {
            backup: b, name: n, ..
        } => b == backup && n == name,
        _ => false,
    };
    if let Some(t) = task_since(cluster, since, same)? {
        return Ok(DrResult::Task(t));
    }
    let Some(b) = obs.backups.iter().find(|b| b.id == *backup) else {
        return Err(format!("recovery point {backup} is gone"));
    };
    let vm = &b.spec.vm_spec;
    let uses_net = |n: &String| vm.nics.iter().any(|nic| &nic.network == n);
    let uses_group = |g: &String| vm.nics.iter().any(|nic| nic.security_groups.contains(g));
    let rules: Vec<dr::DrIpRule> = ip
        .iter()
        .filter(|r| (r.nic as usize) < vm.nics.len())
        .cloned()
        .collect();
    if rules.len() < ip.len() {
        tracing::warn!(run = %run.name, vm = %name, "IP rules for missing NICs are ignored");
    }
    let (cloud_init_networks, cloud_init_dns): (Option<Vec<CloudInitNet>>, Option<CloudInitDns>) =
        if rules.is_empty() {
            (None, None)
        } else {
            let base = vm
                .cloud_init
                .as_ref()
                .map(|c| c.networks.clone())
                .unwrap_or_default();
            let (nets, dns) = dr::cloud_init_networks(&base, &rules);
            (Some(nets), dns)
        };
    let folder = match folder {
        Some(f) => resolve_folder(cluster, f, run.spec.kind == DrRunKind::Test).await?,
        None => None,
    };
    let req = RestoreRequest {
        name: name.clone(),
        pool: pool.clone(),
        node: node.clone(),
        folder,
        live: *live,
        networks: networks
            .iter()
            .filter(|(k, v)| uses_net(k) && k != v)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        security_groups: security_groups
            .iter()
            .filter(|(k, v)| uses_group(k) && k != v)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        cloud_init_networks,
        cloud_init_dns,
    };
    match start_restore(state, None, b, req).await {
        Ok(task) => Ok(DrResult::Task(task.id)),
        Err(e) => Err(api_error(e)),
    }
}

/// 시작: `vm-start` Task(이미 낸 것이 있으면 그것)
async fn start(
    state: &Arc<AppState>,
    status: &DrRunStatus,
    step: usize,
    name: &str,
) -> Result<DrResult, String> {
    let cluster = &state.cluster;
    let vm = cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("VM {name} is gone"))?;
    let since = status.steps.get(step).map_or(0, |s| s.at);
    if let Some(t) = task_since(cluster, since, |t| {
        t.spec.target == vm.id && t.spec.op == TaskOp::VmStart
    })? {
        return Ok(DrResult::Task(t));
    }
    power(state, &vm, "start", StopMode::Acpi).await
}

async fn power(
    state: &Arc<AppState>,
    vm: &Vm,
    action: &str,
    mode: StopMode,
) -> Result<DrResult, String> {
    let (_, crate::error::Json(task)) = crate::vms::vm_action(
        State(state.clone()),
        engine(),
        Path((vm.id.to_string(), action.to_string())),
        Query(ActionQuery { mode }),
    )
    .await
    .map_err(api_error)?;
    Ok(DrResult::Task(task.id))
}

/// 뒷정리: 시험 VM을 끄고(켜져 있으면 끄는 Task) 지운 뒤 그 디스크를 지운다.
async fn delete_vm(state: &Arc<AppState>, run: &DrRun, name: &str) -> Result<DrResult, String> {
    let cluster = &state.cluster;
    let Some(vm) = cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
    else {
        return Ok(DrResult::Done);
    };
    if vm.labels.get(DR_TEST_LABEL) != Some(&run.spec.plan_name) {
        return Err(format!(
            "VM {name} is not a test VM of plan {} (no label {DR_TEST_LABEL}={}); not deleting it",
            run.spec.plan_name, run.spec.plan_name
        ));
    }
    if !is_stopped(&vm) {
        let stopping = |t: &orbivisor_types::task::Task| {
            t.spec.target == vm.id
                && matches!(t.spec.op, TaskOp::VmStop | TaskOp::VmForceStop)
                && !t.status.state.is_finished()
        };
        if let Some(t) = task_since(cluster, 0, stopping)? {
            return Ok(DrResult::Task(t));
        }
        return power(state, &vm, "stop", StopMode::Force).await;
    }
    let volumes: Vec<String> = vm.spec.disks.iter().map(|d| d.volume.clone()).collect();
    crate::vms::delete_vm(
        State(state.clone()),
        engine(),
        Path(vm.id.to_string()),
        HeaderMap::new(),
    )
    .await
    .map_err(api_error)?;
    let mut last = None;
    for volume in volumes {
        match crate::volumes::delete_volume(State(state.clone()), engine(), Path(volume.clone()))
            .await
        {
            Ok((_, crate::error::Json(task))) => last = Some(task.id),
            Err(e) => {
                tracing::warn!(vm = %name, volume = %volume, error = %e.message, "cannot delete a test VM disk");
            }
        }
    }
    Ok(last.map_or(DrResult::Done, DrResult::Task))
}

/// `addr/prefix`가 든 망(`10.20.0.5/24` → `10.20.0.0/24`)
fn network_of(address: &str) -> Option<Cidr> {
    let (ip, prefix) = address.split_once('/')?;
    Cidr::network(ip.parse::<IpAddr>().ok()?, prefix.parse().ok()?)
}

/// 거품 네트워크의 서브넷: 본뜰 DR 네트워크(`like`)의 서브넷(유동 IP 풀·IPv6 알리기 빼고), 없으면 원본 네트워크 `from`에
/// 붙는 NIC의 IP 규칙 주소가 든 망(주소 체계마다 처음 것, 게이트웨이는 그 규칙의 것).
fn bubble_subnets(
    cluster: &Cluster,
    run: &DrRun,
    status: &DrRunStatus,
    obs: &DrObservation,
    from: &str,
    like: &str,
) -> Result<Vec<Subnet>, String> {
    if let Some(net) = cluster
        .get_by_name::<NetworkSpec>(like)
        .map_err(|e| e.to_string())?
        && !net.spec.subnets.is_empty()
    {
        return Ok(net
            .spec
            .subnets
            .into_iter()
            .map(|mut s| {
                s.floating_pools.clear();
                s.ipv6_mode = None;
                s
            })
            .collect());
    }
    let mut out: Vec<Subnet> = Vec::new();
    for step in &status.steps {
        let Some(b) = step
            .backup_id
            .and_then(|id| obs.backups.iter().find(|b| b.id == id))
        else {
            continue;
        };
        let Some((_, v)) = run.spec.plan_spec.vms().find(|(_, v)| v.vm == step.vm) else {
            continue;
        };
        for rule in &v.ip {
            let on = b
                .spec
                .vm_spec
                .nics
                .get(rule.nic as usize)
                .is_some_and(|n| n.network == from);
            let Some(cidr) = rule.address.as_deref().filter(|_| on).and_then(network_of) else {
                continue;
            };
            if out
                .iter()
                .any(|s| s.cidr.addr.is_ipv4() == cidr.addr.is_ipv4())
            {
                continue;
            }
            let gateway = rule
                .gateway
                .as_deref()
                .and_then(|g| g.parse::<IpAddr>().ok())
                .filter(|g| cidr.contains(*g));
            let subnet: Subnet =
                serde_json::from_value(json!({ "cidr": cidr.to_string(), "gateway": gateway }))
                    .map_err(|e| e.to_string())?;
            out.push(subnet);
        }
    }
    Ok(out)
}

/// 거품 SDN 네트워크(라우터·업링크 없음, 시험 라벨)
async fn create_bubble(
    state: &Arc<AppState>,
    run: &DrRun,
    status: &DrRunStatus,
    obs: &DrObservation,
    from: &str,
    name: &str,
    like: &str,
) -> Result<(), String> {
    let cluster = &state.cluster;
    if cluster
        .get_by_name::<NetworkSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    crate::sdn::require_ready(cluster).map_err(|e| e.message)?;
    let mtu = cluster
        .get_by_name::<NetworkSpec>(like)
        .map_err(|e| e.to_string())?
        .filter(|n| n.spec.is_sdn())
        .and_then(|n| n.spec.mtu);
    let spec = NetworkSpec {
        bridge: None,
        vlan: None,
        vxlan: None,
        sdn: Some(SdnNet::default()),
        mtu,
        subnets: bubble_subnets(cluster, run, status, obs, from, like)?,
        folder: None,
    };
    spec.validate().map_err(|e| e.to_string())?;
    let mut net = Resource::new(name, spec);
    net.labels
        .insert(DR_TEST_LABEL.to_string(), run.spec.plan_name.clone());
    match cluster.create(&net).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 거품 네트워크를 지운다(시험 라벨이 이 계획인 것만, 쓰는 VM이 없어야).
async fn delete_network(state: &Arc<AppState>, run: &DrRun, name: &str) -> Result<(), String> {
    let cluster = &state.cluster;
    let Some(net) = cluster
        .get_by_name::<NetworkSpec>(name)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    if net.labels.get(DR_TEST_LABEL) != Some(&run.spec.plan_name) {
        return Err(format!(
            "network {name} is not a test network of plan {}; not deleting it",
            run.spec.plan_name
        ));
    }
    let (_, vms) = cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.nics.iter().any(|n| n.network == name))
    {
        return Err(format!("network {name} is used by VM {}", vm.name));
    }
    match cluster.delete::<NetworkSpec>(net.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => return Err(e.to_string()),
    }
    crate::permissions::drop_orphans(cluster).await;
    Ok(())
}

// ---- 끝 ----

/// 끝난 실행을 계획에 적는다: `status.run`을 비우고(이 실행이면), 시험이면 `last_test`.
async fn settle(cluster: &Cluster, run: &DrRun) {
    for _ in 0..10 {
        let Ok(Some(plan)) = cluster.get::<RecoveryPlanSpec>(run.spec.plan) else {
            return;
        };
        let mut status: RecoveryPlanStatus = plan.status.clone();
        if status.run == Some(run.id) {
            status.run = None;
        }
        if run.spec.kind == DrRunKind::Test {
            let at = run.status.finished_at.unwrap_or_else(now);
            if status.last_test.as_ref().is_none_or(|t| t.at <= at) {
                status.last_test = Some(LastTest {
                    run: run.id,
                    at,
                    rto_secs: run.status.rto_secs,
                    result: run.status.state,
                });
            }
        }
        if status == plan.status {
            return;
        }
        match cluster
            .update_status_checked::<RecoveryPlanSpec>(plan.id, plan.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => {
                tracing::warn!(plan = %plan.name, error = %e, "cannot record the finished DR run");
                return;
            }
            Ok(_) => return,
        }
    }
}

/// 계획의 `status.run`이 없어진 실행을 가리키면 비운다.
async fn clear_plan_run(cluster: &Cluster, plan: Uuid, run: Uuid) {
    let Ok(Some(p)) = cluster.get::<RecoveryPlanSpec>(plan) else {
        return;
    };
    if p.status.run != Some(run) {
        return;
    }
    let status = RecoveryPlanStatus {
        run: None,
        ..p.status.clone()
    };
    let _ = cluster
        .update_status_checked::<RecoveryPlanSpec>(p.id, p.revision, &status)
        .await;
}

/// 취소한 시험의 뒷정리 실행(남은 시험 VM·거품 네트워크가 있을 때)
async fn start_cleanup(state: &Arc<AppState>, test: &DrRun) {
    let cluster = &state.cluster;
    let Ok(Some(plan)) = cluster.get::<RecoveryPlanSpec>(test.spec.plan) else {
        return;
    };
    let Ok(runs) = super::runs_of(cluster, plan.id) else {
        return;
    };
    if super::active(&runs).is_some() {
        return;
    }
    let target = match super::leftovers(state, &plan, &runs) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(plan = %plan.name, error = %e.message, "cannot find what the cancelled test left");
            return;
        }
    };
    if target.vms.is_empty() && target.networks.is_empty() {
        return;
    }
    if let Err(e) = super::start_run(state, &plan, DrRunKind::Cleanup, Some(target)).await {
        tracing::warn!(plan = %plan.name, error = %e.message, "cannot start the cleanup after a cancelled test");
    }
}

// ---- 운영 쪽(계획된 이전) ----

/// 실행 `run`이 운영 쪽 VM `vm`에 뜨는 마지막 백업 이름(실행마다 정해 다시 부르면 409 `name_taken` — 같은 백업을 둘 만들지
/// 않는다)
fn source_backup_name(run: Uuid, vm: &str) -> String {
    let tag = run.simple().to_string();
    let tag = &tag[tag.len() - 8..];
    let tail = format!("-dr-{tag}");
    let keep = 63usize.saturating_sub(tail.len()).min(vm.len());
    format!("{}{tail}", vm[..keep].trim_end_matches('-'))
}

fn path_segment(s: &str) -> String {
    // 리소스 이름 규칙상 `[a-z0-9-]`이라 그대로 쓴다. 혹시 모를 다른 글자는 퍼센트로.
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 운영 클러스터(원격) 쪽
struct SourceSide {
    client: RemoteClient,
    /// 이 클러스터의 CA 지문(운영 쪽이 이 클러스터를 등록한 원격 이름을 찾는다)
    local_ca: Option<String>,
    /// 운영 쪽에 등록된 이 클러스터의 원격 이름(처음 물을 때 찾음)
    here: Option<Option<String>>,
    /// 이번 걸음에서 읽은 운영 쪽 백업(이름 → JSON)
    backups: BTreeMap<String, Value>,
}

impl SourceSide {
    fn new(state: &AppState, remote: &Remote) -> Option<Self> {
        let client = RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token)
            .map_err(|e| tracing::warn!(remote = %remote.name, error = %e, "cannot make the remote client"))
            .ok()?;
        let local_ca = crate::cluster::cluster_info(state)
            .ok()
            .and_then(|i| orbivisor_peer::pki::fingerprint(&i.spec.ca_cert_pem).ok());
        Some(Self {
            client,
            local_ca,
            here: None,
            backups: BTreeMap::new(),
        })
    }

    async fn get(&self, path: &str) -> Result<Value, RemoteError> {
        tokio::time::timeout(REMOTE_TIMEOUT, self.client.get_json::<Value>(path))
            .await
            .unwrap_or_else(|_| Err(RemoteError::Unreachable("timed out".into())))
    }

    /// `POST`(JSON 본문). 2xx면 본문, 아니면 [`RemoteError::Status`](오류 본문의 `code`와 `message`)
    async fn post(&self, path: &str, body: &Value) -> Result<Value, (Option<u16>, String, String)> {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            axum::http::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
        let work = async {
            let response = self
                .client
                .request(
                    Method::POST,
                    path,
                    headers,
                    Full::new(Bytes::from(body.to_string())),
                )
                .await
                .map_err(|e| (None, String::new(), e.to_string()))?;
            let status = response.status().as_u16();
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .map(|b| b.to_bytes())
                .unwrap_or_default();
            let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if (200..300).contains(&status) {
                Ok(value)
            } else {
                let code = value["code"].as_str().unwrap_or_default().to_string();
                let message = value["message"]
                    .as_str()
                    .map_or_else(|| format!("HTTP {status}"), str::to_string);
                Err((Some(status), code, message))
            }
        };
        tokio::time::timeout(REMOTE_TIMEOUT, work)
            .await
            .unwrap_or_else(|_| Err((None, String::new(), "timed out".into())))
    }

    /// 운영 쪽에 등록된 이 클러스터의 원격 이름
    async fn here(&mut self) -> Result<Option<String>, RemoteError> {
        if let Some(h) = &self.here {
            return Ok(h.clone());
        }
        let list = self.get("/remotes").await?;
        let found = list["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| {
                self.local_ca.as_deref().is_some_and(|ca| {
                    r["spec"]["ca_fingerprint"].as_str() == Some(ca)
                        || r["ca_fingerprint"].as_str() == Some(ca)
                })
            })
            .and_then(|r| r["name"].as_str())
            .map(str::to_string);
        self.here = Some(found.clone());
        Ok(found)
    }

    /// 운영 쪽 관찰: VM이 꺼졌는지, 실행이 뜬 백업의 상태와 이 클러스터로 가는 사본. 하나라도 읽지 못하면 `None`(기다림).
    async fn observe(&mut self, status: &DrRunStatus) -> Option<SourceObservation> {
        let result: Result<SourceObservation, RemoteError> = async {
            let vms = self.get("/vms").await?;
            let mut out = SourceObservation::default();
            for vm in vms["items"].as_array().into_iter().flatten() {
                if let Some(name) = vm["name"].as_str() {
                    out.vms
                        .insert(name.to_string(), vm["status"]["phase"] == "stopped");
                }
            }
            let names: Vec<String> = status
                .steps
                .iter()
                .filter(|s| s.phase == DrPhase::Preparing)
                .filter_map(|s| s.prepare.as_ref()?.source_backup.clone())
                .collect();
            let here = if names.is_empty() {
                None
            } else {
                self.here().await?
            };
            self.backups.clear();
            for name in names {
                let b = match self.get(&format!("/backups/{}", path_segment(&name))).await {
                    Ok(b) => b,
                    Err(RemoteError::Status(404, _)) => continue,
                    Err(e) => return Err(e),
                };
                let phase = b["status"]["phase"].as_str().unwrap_or_default();
                let replica_error = b["status"]["replicas"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|r| here.is_some() && r["remote"].as_str() == here.as_deref())
                    .filter(|r| r["state"] == "failed")
                    .map(|r| {
                        r["error"]
                            .as_str()
                            .unwrap_or("replication failed")
                            .to_string()
                    });
                let id = b["id"].as_str().and_then(|s| s.parse().ok());
                if let Some(id) = id {
                    out.backups.insert(
                        name.clone(),
                        SourceBackup {
                            id,
                            ready: phase == "ready",
                            error: (phase == "failed").then(|| {
                                b["status"]["error"]
                                    .as_str()
                                    .unwrap_or("backup failed")
                                    .to_string()
                            }),
                            replica_error,
                        },
                    );
                }
                self.backups.insert(name, b);
            }
            Ok(out)
        }
        .await;
        match result {
            Ok(o) => Some(o),
            Err(e) => {
                tracing::info!(error = %e, "cannot read the source cluster; waiting");
                None
            }
        }
    }

    /// 운영 쪽 VM 끄기(ACPI, `force`면 곧바로). 실패해도 상태 기계가 다시 본다(5분 뒤 강제, 1분마다 다시).
    async fn stop(&self, vm: &str, force: bool) -> DrResult {
        let path = format!(
            "/vms/{}/actions/stop{}",
            path_segment(vm),
            if force { "?mode=force" } else { "" }
        );
        if let Err((_, _, e)) = self.post(&path, &json!({})).await {
            tracing::info!(vm, force, error = %e, "cannot stop the source VM; will retry");
        }
        DrResult::Done
    }

    /// 운영 쪽 마지막 백업(실행마다 정한 이름, 이미 있으면 그것). 풀·저장소는 그 VM의 가장 새 운영 쪽 백업의 것이고 증분으로
    /// 뜬다. 닿지 않으면 `Done`(1분 뒤 다시), 운영 쪽이 거절하면 실패.
    async fn backup(&self, run: &DrRun, vm: &str) -> DrResult {
        let name = source_backup_name(run.id, vm);
        let lookup = |b: &Value| -> Option<DrResult> {
            let id = b["id"].as_str()?.parse().ok()?;
            Some(DrResult::SourceBackup {
                name: name.clone(),
                id,
            })
        };
        match self.get(&format!("/backups/{}", path_segment(&name))).await {
            Ok(b) => return lookup(&b).unwrap_or(DrResult::Done),
            Err(RemoteError::Status(404, _)) => {}
            Err(_) => return DrResult::Done,
        }
        let list = match self.get(&format!("/backups?vm={}", path_segment(vm))).await {
            Ok(l) => l,
            Err(_) => return DrResult::Done,
        };
        let newest = list["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["spec"]["vm_name"] == vm && b["spec"]["origin"].is_null())
            .max_by_key(|b| b["status"]["created_at"].as_u64().unwrap_or(0));
        let mut body = json!({ "name": name, "incremental": true });
        match newest {
            Some(b) if b["spec"]["store"].is_string() => body["store"] = b["spec"]["store"].clone(),
            Some(b) if b["spec"]["pool"].is_string() => body["pool"] = b["spec"]["pool"].clone(),
            _ => {
                return DrResult::Failed(format!(
                    "VM {vm} has no backup on the source cluster to take the backup pool from"
                ));
            }
        }
        // 막 끈 VM은 정지 Task가 끝날 때까지 409 `vm_busy`다: 잠깐씩 몇 번 다시 하고, 그래도면 다음 차례(1분 뒤)에.
        for attempt in 0..5 {
            match self
                .post(&format!("/vms/{}/backups", path_segment(vm)), &body)
                .await
            {
                Ok(_) => break,
                Err((Some(409), code, _)) if code == "name_taken" => break,
                Err((Some(409), code, _)) if code == "vm_busy" => {
                    if attempt == 4 {
                        return DrResult::Done;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err((Some(s), _, e)) if (400..500).contains(&s) => {
                    return DrResult::Failed(format!("source backup of {vm}: {e}"));
                }
                Err(_) => return DrResult::Done,
            }
        }
        match self.get(&format!("/backups/{}", path_segment(&name))).await {
            Ok(b) => lookup(&b).unwrap_or(DrResult::Done),
            Err(_) => DrResult::Done,
        }
    }

    /// 운영 쪽 백업을 이 클러스터로 복제(이미 이 클러스터로 가는 사본이 있으면 할 일 없음). 받는 풀은 그 VM 사본의 풀(없으면
    /// 이름이 가장 앞선 백업 풀).
    async fn replicate(
        &mut self,
        state: &AppState,
        status: &DrRunStatus,
        obs: &DrObservation,
        step: usize,
        backup: &str,
    ) -> DrResult {
        let here = match self.here().await {
            Ok(Some(h)) => h,
            Ok(None) => {
                return DrResult::Failed(
                    "the source cluster has no remote for this cluster; register it there first"
                        .into(),
                );
            }
            Err(e) => return DrResult::Failed(format!("cannot read the source remotes: {e}")),
        };
        if let Some(b) = self.backups.get(backup)
            && b["status"]["replicas"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["remote"] == here.as_str() && r["state"] != "failed")
        {
            return DrResult::Done;
        }
        let vm = status
            .steps
            .get(step)
            .map(|s| s.vm.clone())
            .unwrap_or_default();
        let pool = obs
            .source_ca
            .as_deref()
            .and_then(|ca| dr::recovery_point(&vm, &obs.backups, ca))
            .filter(|b| b.spec.store.is_none())
            .map(|b| b.spec.pool.clone())
            .or_else(|| {
                let (_, pools) = state.cluster.list::<StoragePoolSpec>().ok()?;
                pools
                    .into_iter()
                    .filter(|p| p.spec.backups)
                    .map(|p| p.name)
                    .min()
            });
        let Some(pool) = pool else {
            return DrResult::Failed("this cluster has no backup pool to receive the copy".into());
        };
        match self
            .post(
                &format!("/backups/{}/actions/replicate", path_segment(backup)),
                &json!({ "remote": here, "pool": pool }),
            )
            .await
        {
            Ok(_) => DrResult::Done,
            // 같은 복제가 이미 돈다(다른 리더가 냈다).
            Err((Some(409), code, _)) if code == "backup_busy" => DrResult::Done,
            Err((_, _, e)) => DrResult::Failed(format!("replicating {backup}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_backup_names_fit_and_differ_per_run() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert_ne!(source_backup_name(a, "web1"), source_backup_name(b, "web1"));
        assert!(source_backup_name(a, "web1").starts_with("web1-dr-"));
        let long = "a".repeat(63);
        let n = source_backup_name(a, &long);
        assert_eq!(n.len(), 63);
        orbivisor_types::resource::validate_name(&n).unwrap();
    }

    #[test]
    fn networks_of_addresses() {
        assert_eq!(
            network_of("10.20.0.5/24").unwrap().to_string(),
            "10.20.0.0/24"
        );
        assert_eq!(network_of("fd00::5/64").unwrap().to_string(), "fd00::/64");
        assert!(network_of("10.20.0.5").is_none());
        assert!(network_of("10.20.0.5/40").is_none());
    }
}
