//! 경보 엔진(설계 §8): 모든 노드(위트니스 제외)에서 돈다. `tick`(5초)마다 상태 규칙을 보고, `Cluster::watch`로 끝난
//! Task를 받는다.
//!
//! - **노드 경보**(지표·`VmFailed`·로컬 풀 `PoolUsage`·`TaskFailed`·`HaRecovery`)는 그 일이 일어난 노드가 평가하고 상태를
//!   노드 장부(`alerts/state.json`)에 둔다. 공유 풀은 그 풀이 `ready`인 온라인 노드 가운데 id가 가장 작은 노드가 본다.
//!   Raft에 쓰지 않는다.
//! - **리더 경보**(`NodeOffline`·`BackupJobFailed`, 계획 67의 SDN 조건 여덟, 계획 75의 링크 조건 둘)는 리더가 평가하고
//!   상태를 `AlertRule.status`(Raft)에 둔다. 상태를 먼저 쓰고 쓰기가 된 뒤에만 보내므로 리더가 바뀌어도 다시 보내지
//!   않는다. SDN 조건은 [`orbivisor_types::alert::sdn::evaluate`]가 Raft와 리더 OvnSync의 관찰(같은 프로세스의 메모리)로 본다.
//!
//! 단계는 [`orbivisor_types::alert::advance`]다: pending → firing(알림) → `repeat_secs`마다 다시 → 거짓인 채 60초 → 해제(알림).
//! 노드 장부도 파일에 먼저 쓰고 보내므로 opld를 다시 시작해도 같은 알림을 다시 보내지 않는다.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use orbivisor_cluster::types::NodeId;
use orbivisor_cluster::watch::{Watch, WatchError, WatchEvent};
use orbivisor_cluster::{Cluster, Typed};
use orbivisor_types::alert::sdn::{SdnFinding, SdnInputs, SdnObservations};
use orbivisor_types::alert::{
    ActiveAlert, AlertCondition, AlertRule, AlertRuleSpec, AlertRuleStatus, AlertState,
    AlertSubject, Evaluator, Notice, NotificationState, Observation, RESOLVE_AFTER_MS, SubjectKind,
    advance,
};
use orbivisor_types::backup::{BackupSpec, VerifyState};
use orbivisor_types::backup_job::{BackupJob, BackupJobSpec};
use orbivisor_types::backup_store::BackupStoreSpec;
use orbivisor_types::clock::{Clock, SystemClock};
use orbivisor_types::cluster::ClusterSpec;
use orbivisor_types::load_balancer::LoadBalancerSpec;
use orbivisor_types::metrics::{MetricsQuery, MetricsScope, NodeSample, VmSample};
use orbivisor_types::network::NetworkSpec;
use orbivisor_types::node::{Node, NodeSpec};
use orbivisor_types::physical::{self, Impact};
use orbivisor_types::resource::Kind;
use orbivisor_types::router::RouterSpec;
use orbivisor_types::sdn::{SDN_NAME, SdnSpec};
use orbivisor_types::storage::{StoragePoolSpec, is_shared};
use orbivisor_types::task::{Task, TaskOp, TaskState};
use orbivisor_types::vm::{Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::book::{AlertBook, EVENT_KEEP_MS, MAX_ACTIVE, NodeAlertState};
use super::send::{Notification, Sender, SenderConfig};
use crate::state::AppState;

/// 리비전만 바뀌었을 때 상태 파일을 다시 쓰는 간격(다시 시작하면 그 뒤부터 이어 본다)
const REVISION_PERSIST_MS: i64 = 60_000;

/// 경보 엔진 설정
#[derive(Clone)]
pub struct AlertEngineConfig {
    /// 상태 규칙을 보는 간격(지표 측정 간격과 같은 5초)
    pub tick: Duration,
    /// 조건이 거짓인 채 이만큼 지나야 해제한다.
    pub resolve_after_ms: i64,
    pub clock: Arc<dyn Clock>,
    pub sender: SenderConfig,
}

impl Default for AlertEngineConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
            resolve_after_ms: RESOLVE_AFTER_MS,
            clock: Arc::new(SystemClock),
            sender: SenderConfig::default(),
        }
    }
}

impl AlertEngineConfig {
    /// 시험 빌드의 `[test] alert_tick_ms`: 간격을 바꾸고 해제 대기도 같은 비율(12 tick)로 줄인다.
    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick = tick;
        self.resolve_after_ms = tick.as_millis() as i64 * (RESOLVE_AFTER_MS / 5_000);
        self
    }
}

/// 경보 엔진. `AppState.alerts`(경보 장부)가 없으면 아무것도 하지 않는다.
pub struct AlertEngine {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl AlertEngine {
    pub fn spawn(state: Arc<AppState>, config: AlertEngineConfig) -> Self {
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

async fn run(state: Arc<AppState>, config: AlertEngineConfig, mut stop: oneshot::Receiver<()>) {
    let Some(book) = state.alerts.clone() else {
        let _ = stop.await;
        return;
    };
    let sender = Sender::spawn(
        state.cluster.clone(),
        book.clone(),
        config.clock.clone(),
        config.sender.clone(),
    );
    let mut engine = Engine {
        revision: book.snapshot().revision,
        state,
        book,
        sender,
        clock: config.clock.clone(),
        resolve_after_ms: config.resolve_after_ms,
        last_t: HashMap::new(),
        leader_ready: false,
        persisted_at: i64::MIN,
        last_observed: None,
        events_sent: HashMap::new(),
    };
    let mut watch = engine.subscribe(true);
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = interval.tick() => engine.tick().await,
            ev = next(&mut watch) => match ev {
                Ok(ev) => engine.on_event(ev).await,
                Err(e) => {
                    tracing::warn!(error = %e, "the alert engine lost the change stream; following from now on");
                    watch = engine.subscribe(false);
                    if watch.is_none() {
                        // 저장소가 닫혔다(멈추는 중).
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            },
        }
    }
    engine.sender.shutdown().await;
}

async fn next(watch: &mut Option<Watch>) -> Result<WatchEvent, WatchError> {
    match watch {
        Some(w) => w.next().await,
        None => std::future::pending().await,
    }
}

struct Engine {
    state: Arc<AppState>,
    book: AlertBook,
    sender: Sender,
    clock: Arc<dyn Clock>,
    resolve_after_ms: i64,
    /// 처리한 watch 리비전
    revision: u64,
    /// 시리즈(노드·VM id)마다 마지막으로 본 점의 시각
    last_t: HashMap<Uuid, i64>,
    /// 리더가 된 뒤 장벽을 지났다.
    leader_ready: bool,
    /// 상태 파일을 마지막으로 쓴 때
    persisted_at: i64,
    /// 리더로서 마지막으로 평가에 쓴 SDN 관찰(`sdn_gateway_failover`가 다음 관찰과 견준다). 게이트웨이 주인은 같은 임기에서
    /// 마지막으로 안 것을 남긴다(리뷰 2 m5 — 주인이 없던 관찰을 지나 옮겨도 놓치지 않게, [`merge_hosting`]).
    last_observed: Option<SdnObservations>,
    /// 이벤트 규칙이 대상·문구마다 마지막으로 보낸 때((규칙, 대상, 문구) → ms — 리뷰 2 m5, [`damp_events`])
    events_sent: HashMap<(Uuid, Uuid, Option<String>), i64>,
}

/// 새 관찰 `now`에 같은 임기의 앞 관찰 `prev`가 알던 게이트웨이 주인 가운데 `now`에 없는 것을 더한다(리뷰 2 m5: A → 주인
/// 없음 → B도 A → B로 본다).
fn merge_hosting(prev: Option<&SdnObservations>, mut now: SdnObservations) -> SdnObservations {
    if let Some(prev) = prev.filter(|p| p.term == now.term) {
        for (router, host) in &prev.hosting {
            now.hosting.entry(*router).or_insert_with(|| host.clone());
        }
    }
    now
}

/// 이벤트 규칙(`sdn_gateway_failover`)의 참인 발견 가운데 같은 대상의 같은 일(문구 — "A에서 B로")을 `repeat_secs` 안에
/// 이미 보낸 것은 뺀다(리뷰 2 m5 — BFD가 두 chassis 사이를 오가도 관찰마다 알리지 않게: A→B·B→A 한 번씩). 보낼 것은
/// `sent`에 지금을 적는다.
fn damp_events(
    sent: &mut HashMap<(Uuid, Uuid, Option<String>), i64>,
    rule: &AlertRule,
    findings: Vec<SdnFinding>,
    now: i64,
) -> Vec<SdnFinding> {
    let window = i64::from(rule.spec.repeat_secs) * 1000;
    findings
        .into_iter()
        .filter(|f| f.holds == Some(true))
        .filter(|f| {
            let key = (rule.id, f.subject.id, f.detail.clone());
            if sent.get(&key).is_some_and(|at| now - at < window) {
                return false;
            }
            sent.insert(key, now);
            true
        })
        .collect()
}

/// 경보 하나와 보낼 알림
struct Pending {
    rule: AlertRule,
    alert: ActiveAlert,
    state: NotificationState,
    detail: Option<String>,
}

/// 링크 조건의 영향 문구에 쓰는 Raft 입력(장애가 있는 tick에 한 번 읽음)
struct LinkWorld {
    networks: Vec<orbivisor_types::network::Network>,
    vms: Vec<Vm>,
}

/// SDN 조건의 Raft 입력(한 tick에 한 번 읽음)
struct SdnWorld {
    sdn: Option<orbivisor_types::sdn::Sdn>,
    networks: Vec<orbivisor_types::network::Network>,
    routers: Vec<orbivisor_types::router::Router>,
    load_balancers: Vec<orbivisor_types::load_balancer::LoadBalancer>,
    vms: Vec<Vm>,
}

impl SdnWorld {
    fn inputs<'a>(
        &'a self,
        nodes: &'a [Node],
        observed: Option<&'a SdnObservations>,
        previous: Option<&'a SdnObservations>,
        now: i64,
    ) -> SdnInputs<'a> {
        SdnInputs {
            sdn: self.sdn.as_ref(),
            nodes,
            networks: &self.networks,
            routers: &self.routers,
            load_balancers: &self.load_balancers,
            vms: &self.vms,
            observed,
            previous,
            now_ms: now,
        }
    }
}

impl Engine {
    fn cluster(&self) -> &Cluster {
        &self.state.cluster
    }

    /// 변경 구독. `resume`이면 장부의 리비전부터 이어 보고, 그 기록이 없어졌거나 처음이면 지금부터 본다.
    fn subscribe(&mut self, resume: bool) -> Option<Watch> {
        let current = self
            .cluster()
            .list::<AlertRuleSpec>()
            .map(|(r, _)| r)
            .ok()?;
        if resume && self.revision > 0 {
            match self.cluster().watch(self.revision) {
                Ok(w) => return Some(w),
                Err(e) => tracing::warn!(
                    error = %e,
                    "alert events since the last run are no longer kept; following from now on"
                ),
            }
        }
        self.revision = current;
        self.cluster().watch(current).ok()
    }

    async fn on_event(&mut self, ev: WatchEvent) {
        self.revision = self.revision.max(ev.revision());
        if let WatchEvent::Put {
            kind: Kind::Task,
            resource,
        } = ev
            && let Ok(task) =
                serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
        {
            self.on_task(task).await;
        }
    }

    /// 켠 규칙. 읽지 못하면 `None`이다(빈 목록으로 보면 활성 경보를 모두 알림 없이 뺀다).
    fn rules(&self) -> Option<Vec<AlertRule>> {
        let rules = self.cluster().list::<AlertRuleSpec>().ok()?.1;
        Some(rules.into_iter().filter(|r| r.spec.enabled).collect())
    }

    fn me(&self) -> Option<Node> {
        self.cluster()
            .get::<NodeSpec>(self.state.node)
            .ok()
            .flatten()
            .filter(|n| !n.spec.witness)
    }

    /// 이 노드에서 끝난 Task의 이벤트 규칙(`TaskFailed`·`HaRecovery`). 같은 Task로는 한 번만 보낸다.
    async fn on_task(&mut self, task: Task) {
        if task.spec.node != self.state.node || !task.status.state.is_finished() {
            return;
        }
        let rules: Vec<AlertRule> = self
            .rules()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                matches!(
                    r.spec.condition,
                    AlertCondition::TaskFailed { .. } | AlertCondition::HaRecovery
                )
            })
            .collect();
        if rules.is_empty() {
            return;
        }
        let Some(me) = self.me() else { return };
        let op = op_name(&task.spec.op);
        let vm = self
            .cluster()
            .get::<VmSpec>(task.spec.target)
            .ok()
            .flatten();
        let target_name = vm
            .as_ref()
            .map_or_else(|| task.spec.target.to_string(), |v| v.name.clone());
        let now = self.clock.now_ms();
        let mut state = self.book.snapshot();
        let mut out = Vec::new();
        for rule in rules {
            let detail = match &rule.spec.condition {
                AlertCondition::TaskFailed { .. }
                    if task.status.state == TaskState::Failed && rule.spec.applies_to_op(&op) =>
                {
                    Some(format!(
                        "Task {op} {target_name} failed: {}",
                        task.status.error.as_deref().unwrap_or("no error message")
                    ))
                }
                AlertCondition::HaRecovery
                    if matches!(task.spec.op, TaskOp::VmHaRecover { .. })
                        && rule.spec.applies_to_vm(&target_name) =>
                {
                    let how = if task.status.state == TaskState::Failed {
                        format!(
                            "failed: {}",
                            task.status.error.as_deref().unwrap_or("no error message")
                        )
                    } else {
                        format!("VM {target_name} is back on node {}", me.name)
                    };
                    Some(format!("HA recovery of {target_name} {how}"))
                }
                _ => None,
            };
            let Some(detail) = detail else { continue };
            if !rule.spec.applies_to_node(&me.name) {
                continue;
            }
            let subject = AlertSubject {
                kind: SubjectKind::Task,
                id: task.id,
                name: format!("{op} {target_name}"),
                node: Some(me.name.clone()),
                port: None,
            };
            let key = subject.key(rule.id);
            if state.events.contains_key(&key) {
                continue;
            }
            state.events.insert(key, now);
            out.push(Pending {
                alert: event_alert(&rule, subject, now),
                rule,
                state: NotificationState::Event,
                detail: Some(detail),
            });
        }
        if out.is_empty() {
            return;
        }
        state.revision = self.revision;
        // 기억한 뒤 보낸다(다시 시작해도 같은 Task로 다시 보내지 않게).
        self.save(state, true).await;
        self.send(out, now);
    }

    async fn tick(&mut self) {
        let now = self.clock.now_ms();
        // 규칙·노드를 읽지 못하면 이번 tick은 건너뛴다(활성 경보를 알림 없이 빼지 않게).
        let (Some(rules), Ok((_, nodes))) = (self.rules(), self.cluster().list::<NodeSpec>())
        else {
            tracing::debug!("cannot read alert rules or nodes; skipping this alert tick");
            return;
        };
        self.node_rules(now, &rules, &nodes).await;
        self.leader_rules(now, &nodes).await;
    }

    /// 이 노드가 평가하는 상태 규칙
    async fn node_rules(&mut self, now: i64, rules: &[AlertRule], nodes: &[Node]) {
        let old = self.book.snapshot();
        let mut observed: Vec<(&AlertRule, AlertSubject, Option<Observation>)> = Vec::new();
        let mut partial = false;
        if let Some(me) = nodes
            .iter()
            .find(|n| n.id == self.state.node && !n.spec.witness)
        {
            partial = !self.observe(rules, me, nodes, &mut observed);
        }
        let Stepped {
            active,
            out,
            overflow,
        } = step_node_alerts(observed, &old.active, partial, now, self.resolve_after_ms);
        let mut events = old.events.clone();
        events.retain(|_, at| now - *at < EVENT_KEEP_MS);
        let new = NodeAlertState {
            revision: self.revision,
            active,
            events,
            overflow,
        };
        if overflow && !old.overflow {
            tracing::warn!(
                limit = MAX_ACTIVE,
                "too many active alerts on this node; new ones are not followed"
            );
        }
        let persist = !old.durable_eq(&new)
            || (new.revision != old.revision
                && now.saturating_sub(self.persisted_at) >= REVISION_PERSIST_MS);
        self.save(new, persist).await;
        self.send(out, now);
    }

    /// 이 노드가 평가하는 상태 규칙의 (규칙, 대상, 본 것). 목록(VM·풀)을 읽지 못해 다 보지 못했으면 false다.
    /// 잠깐 볼 수 없는 대상(준비되지 않은 로컬 풀, 마이그레이션 중인 VM의 지표)은 값을 모름(`None`)으로 넣어 단계를
    /// 그대로 둔다(빠졌다가 다시 보이면 해제 없이 다시 알리지 않게).
    fn observe<'r>(
        &mut self,
        rules: &'r [AlertRule],
        me: &Node,
        nodes: &[Node],
        out: &mut Vec<(&'r AlertRule, AlertSubject, Option<Observation>)>,
    ) -> bool {
        let mut complete = true;
        let here = |r: &AlertRule| {
            r.spec.condition.evaluator() == Evaluator::Node
                && !r.spec.condition.is_event()
                && r.spec.applies_to_node(&me.name)
        };
        let rules: Vec<&AlertRule> = rules.iter().filter(|r| here(r)).collect();
        if rules.is_empty() {
            return true;
        }
        let kinds = |f: fn(&AlertCondition) -> bool| rules.iter().any(|r| f(&r.spec.condition));
        // 노드 지표(유지 보수 중에는 보지 않고, 활성 경보는 알림 없이 뺀다)
        if kinds(|c| matches!(c, AlertCondition::NodeMetric { .. })) && !me.spec.maintenance {
            let sample = self.latest_node();
            let subject = AlertSubject {
                kind: SubjectKind::Node,
                id: me.id,
                name: me.name.clone(),
                node: Some(me.name.clone()),
                port: None,
            };
            for r in rules
                .iter()
                .filter(|r| matches!(r.spec.condition, AlertCondition::NodeMetric { .. }))
            {
                let seen = sample
                    .as_ref()
                    .and_then(|s| r.spec.condition.observe_node(s));
                out.push((r, subject.clone(), seen));
            }
        }
        // 이 노드의 VM
        let vm_rules: Vec<&&AlertRule> = rules
            .iter()
            .filter(|r| {
                matches!(
                    r.spec.condition,
                    AlertCondition::VmMetric { .. } | AlertCondition::VmFailed
                )
            })
            .collect();
        if !vm_rules.is_empty() {
            let all = self.cluster().list::<VmSpec>().map(|(_, v)| v);
            complete &= all.is_ok();
            let vms: Vec<Vm> = all
                .unwrap_or_default()
                .into_iter()
                .filter(|v| v.status.node.or(v.spec.node) == Some(me.id))
                .collect();
            let want_samples = vm_rules
                .iter()
                .any(|r| matches!(r.spec.condition, AlertCondition::VmMetric { .. }));
            for vm in &vms {
                let subject = AlertSubject {
                    kind: SubjectKind::Vm,
                    id: vm.id,
                    name: vm.name.clone(),
                    node: Some(me.name.clone()),
                    port: None,
                };
                // 도는 VM만 지표를 본다(멈춘 VM의 지표 경보는 알림 없이 뺀다).
                let running = vm.status.phase == VmPhase::Running;
                let sample = (want_samples && running)
                    .then(|| self.latest_vm(vm.id))
                    .flatten();
                for r in vm_rules.iter().filter(|r| r.spec.applies_to_vm(&vm.name)) {
                    match r.spec.condition {
                        AlertCondition::VmMetric { .. } if running => {
                            let seen = sample.as_ref().and_then(|s| r.spec.condition.observe_vm(s));
                            out.push((r, subject.clone(), seen));
                        }
                        // 마이그레이션 중에는 값을 모른다(끝나 다른 노드로 가면 그때 뺀다).
                        AlertCondition::VmMetric { .. }
                            if vm.status.phase == VmPhase::Migrating =>
                        {
                            out.push((r, subject.clone(), None));
                        }
                        AlertCondition::VmFailed => {
                            let failed = vm.status.phase == VmPhase::Failed;
                            out.push((r, subject.clone(), Some(Observation::new(failed, None))));
                        }
                        _ => {}
                    }
                }
            }
            // 이 노드를 떠난 VM의 시리즈는 잊는다.
            if complete {
                self.last_t
                    .retain(|id, _| *id == me.id || vms.iter().any(|v| v.id == *id));
            }
        }
        // 풀 사용률: 로컬 풀은 이 노드, 공유 풀은 준비된 온라인 노드 가운데 id가 가장 작은 노드
        let pool_rules: Vec<&&AlertRule> = rules
            .iter()
            .filter(|r| matches!(r.spec.condition, AlertCondition::PoolUsage { .. }))
            .collect();
        if !pool_rules.is_empty() {
            let pools = self.cluster().list::<StoragePoolSpec>().map(|(_, p)| p);
            complete &= pools.is_ok();
            let pools = pools.unwrap_or_default();
            for (name, ps) in &me.status.pools {
                let Some(pool) = pools.iter().find(|p| p.name == *name && p.id == ps.pool_id)
                else {
                    continue;
                };
                let subject = AlertSubject {
                    kind: SubjectKind::Pool,
                    id: pool.id,
                    name: pool.name.clone(),
                    node: Some(me.name.clone()),
                    port: None,
                };
                // 잠깐 준비되지 않은 로컬 풀은 값을 모른다. 공유 풀은 준비된 다른 노드가 이어 본다.
                if !ps.ready {
                    if !is_shared(pool) {
                        for r in &pool_rules {
                            out.push((r, subject.clone(), None));
                        }
                    }
                    continue;
                }
                if is_shared(pool) {
                    let evaluator = nodes
                        .iter()
                        .filter(|n| !n.spec.witness && n.status.online)
                        .filter(|n| {
                            n.status
                                .pools
                                .get(name)
                                .is_some_and(|s| s.ready && s.pool_id == pool.id)
                        })
                        .map(|n| n.id)
                        .min();
                    if evaluator != Some(me.id) {
                        continue;
                    }
                }
                for r in &pool_rules {
                    let seen = r
                        .spec
                        .condition
                        .observe_pool(ps.total_bytes, ps.available_bytes);
                    out.push((r, subject.clone(), seen));
                }
            }
        }
        // 인증서 만료(계획 73 §11.6): 이 노드 공개 리스너가 낼 수 있는 인증서와 내부 노드 인증서 가운데 가장 먼저 끝나는 것.
        // 값은 남은 날이다. 읽을 인증서가 없으면(시험·위트니스) 값을 모른다.
        let soonest = crate::acme::soonest_expiry(&self.state.acme);
        for r in &rules {
            if let AlertCondition::CertificateExpiring { days } = r.spec.condition {
                let subject = AlertSubject {
                    kind: SubjectKind::Node,
                    id: me.id,
                    name: me.name.clone(),
                    node: Some(me.name.clone()),
                    port: None,
                };
                let seen = soonest.map(|not_after| {
                    let left = (not_after - crate::backups::now() as i64) as f64 / 86_400.0;
                    Observation::new(left < f64::from(days), Some(left.max(0.0).floor()))
                });
                out.push((r, subject, seen));
            }
        }
        complete
    }

    /// 이 노드의 지난 tick 뒤 새 점 가운데 마지막 것
    fn latest_node(&mut self) -> Option<NodeSample> {
        let metrics = self.state.metrics.as_ref()?;
        let me = self.state.node;
        let reply = metrics.read(&MetricsQuery {
            scope: MetricsScope::Nodes,
            since: self.last_t.get(&me).copied(),
        });
        let s = reply.node?.samples.pop()?;
        self.last_t.insert(me, s.t);
        Some(s)
    }

    /// VM의 지난 tick 뒤 새 점 가운데 마지막 것
    fn latest_vm(&mut self, vm: Uuid) -> Option<VmSample> {
        let metrics = self.state.metrics.as_ref()?;
        let reply = metrics.read(&MetricsQuery {
            scope: MetricsScope::Vm { id: vm },
            since: self.last_t.get(&vm).copied(),
        });
        let s = reply
            .vms
            .into_iter()
            .flat_map(|v| v.samples)
            .max_by_key(|s| s.t)?;
        self.last_t.insert(vm, s.t);
        Some(s)
    }

    /// 리더가 평가하는 규칙(`NodeOffline`·`BackupJobFailed`). 상태를 Raft에 쓴 뒤에만 보낸다.
    async fn leader_rules(&mut self, now: i64, nodes: &[Node]) {
        if self.cluster().members().leader != Some(NodeId(self.state.node)) {
            self.leader_ready = false;
            self.last_observed = None;
            self.events_sent.clear();
            return;
        }
        // 새 리더는 앞 리더가 쓴 상태를 아직 적용하지 않았을 수 있으므로 장벽 뒤에 다음 tick부터 본다.
        if !self.leader_ready {
            self.leader_ready = self.cluster().read_barrier().await.is_ok();
            return;
        }
        // 끈 규칙의 리더 경보도 지운다(목록에 끈 규칙이 없으므로 따로 읽는다).
        let all = self
            .cluster()
            .list::<AlertRuleSpec>()
            .map(|(_, r)| r)
            .unwrap_or_default();
        // SDN 조건(계획 67 §11)의 입력은 그런 규칙이 있을 때만 한 번 읽는다.
        let sdn = all
            .iter()
            .any(|r| r.spec.condition.is_sdn())
            .then(|| self.sdn_world())
            .flatten();
        let observed = self.observed();
        // 링크 조건(계획 75)의 영향 문구에 쓰는 네트워크·VM: 장애가 있을 때만 이 tick에 한 번 읽는다.
        let mut link_world: Option<LinkWorld> = None;
        for rule in all
            .into_iter()
            .filter(|r| r.spec.condition.evaluator() == Evaluator::Leader)
        {
            let (status, out) = match rule.spec.condition {
                AlertCondition::NodeOffline => self.node_offline(now, &rule, nodes),
                AlertCondition::BackupJobFailed => self.backup_failed(now, &rule),
                // 백업 저장소(계획 68 §10.5·§6): 목록을 읽지 못하면 그대로 둔다.
                AlertCondition::BackupVerifyFailed => match self.verify_findings() {
                    Some(f) => self.subject_rule(now, &rule, f, "The backup was deleted."),
                    None => continue,
                },
                AlertCondition::BackupStoreUnavailable => match self.store_findings(nodes) {
                    Some(f) => self.subject_rule(now, &rule, f, "The backup store was deleted."),
                    None => continue,
                },
                // ACME 주문이 이어 실패함(계획 73 §11.2): 목록을 읽지 못하면 그대로 둔다.
                AlertCondition::CertificateRenewFailed { failures } => {
                    match self.renew_findings(failures) {
                        Some(f) => self.subject_rule(now, &rule, f, "The certificate was deleted."),
                        None => continue,
                    }
                }
                // 적용 묶음 드리프트(계획 73 §12.5 — 리더 DriftChecker가 status에 쓴 것): 목록을 읽지 못하면 그대로 둔다.
                AlertCondition::ApplySetDrift => match self.drift_findings() {
                    Some(f) => self.subject_rule(now, &rule, f, "The apply set was deleted."),
                    None => continue,
                },
                // 링크 조건(계획 75 §3.4): 노드 상태의 포트·본딩. 영향을 읽지 못하면 그대로 둔다.
                ref c if c.is_link() => match self.link_findings(&rule, nodes, &mut link_world) {
                    Some(f) => self.sdn_rule(now, &rule, f),
                    None => continue,
                },
                ref c if c.is_sdn() => {
                    // 입력을 읽지 못했으면 그대로 둔다(빈 입력으로 보면 경보를 알림 없이 뺀다).
                    let Some(world) = &sdn else { continue };
                    let findings = orbivisor_types::alert::sdn::evaluate(
                        &rule.spec,
                        &world.inputs(nodes, observed.as_ref(), self.last_observed.as_ref(), now),
                    );
                    self.sdn_rule(now, &rule, findings)
                }
                _ => continue,
            };
            if status == rule.status {
                self.send(out, now);
                continue;
            }
            match self
                .cluster()
                .update_status_checked::<AlertRuleSpec>(rule.id, rule.revision, &status)
                .await
            {
                Ok(_) => self.send(out, now),
                // 그사이 규칙이 바뀌었으면 다음 tick에 다시 본다.
                Err(e) => {
                    tracing::debug!(rule = %rule.name, error = %e, "cannot store leader alerts")
                }
            }
        }
        if let Some(o) = observed {
            self.last_observed = Some(merge_hosting(self.last_observed.as_ref(), o));
        }
    }

    /// 지금 임기의 SDN 관찰(리더 OvnSync가 이 프로세스에 둔 것). 다른 임기의 것은 쓰지 않는다(§11 — 새 리더의 첫
    /// 관찰까지 관찰 조건을 평가하지 않음).
    fn observed(&self) -> Option<SdnObservations> {
        let term = self.cluster().leader_term()?;
        self.state
            .ovn
            .as_ref()?
            .observations
            .get()
            .filter(|o| o.term == term)
    }

    /// SDN 조건의 Raft 입력. 하나라도 읽지 못하면 None이다.
    fn sdn_world(&self) -> Option<SdnWorld> {
        let c = self.cluster();
        Some(SdnWorld {
            sdn: c.get_by_name::<SdnSpec>(SDN_NAME).ok()?,
            networks: c.list::<NetworkSpec>().ok()?.1,
            routers: c.list::<RouterSpec>().ok()?.1,
            load_balancers: c.list::<LoadBalancerSpec>().ok()?.1,
            vms: c.list::<VmSpec>().ok()?.1,
        })
    }

    /// 링크 규칙 하나(계획 75 §3.4)의 대상: 노드의 끊긴 포트(`node_link_down`)·저하된 본딩(`bond_degraded`)마다 하나 —
    /// 경보 키에 포트가 들어가 포트마다 따로다. 문구는 [`physical::describe`](그 노드만 [`physical::build`]한 영향).
    /// 오프라인 노드의 상태는 낡았으므로 그 노드의 경보는 그대로 둔다(노드 꺼짐은 `node_offline`이 알린다). 다시 올라온
    /// 포트는 거짓으로 보아 해제 규칙(60초)을 따른다. 네트워크·VM을 읽지 못하면 None이다.
    fn link_findings(
        &self,
        rule: &AlertRule,
        nodes: &[Node],
        world: &mut Option<LinkWorld>,
    ) -> Option<Vec<SdnFinding>> {
        let mut out = Vec::new();
        for node in nodes.iter().filter(|n| rule.spec.applies_to_node(&n.name)) {
            let mine = rule
                .status
                .active
                .iter()
                .filter(|a| a.subject.id == node.id && a.subject.port.is_some());
            if !node.status.online {
                out.extend(mine.map(|a| SdnFinding {
                    subject: a.subject.clone(),
                    holds: None,
                    detail: None,
                }));
                continue;
            }
            let faults = match &rule.spec.condition {
                AlertCondition::NodeLinkDown { ports, role } => {
                    physical::link_faults(&node.status, *role, ports.as_deref())
                }
                _ => physical::bond_faults(&node.status),
            };
            out.extend(
                mine.filter(|a| {
                    !faults
                        .iter()
                        .any(|f| a.subject.port.as_ref() == Some(&f.port))
                })
                .map(|a| SdnFinding {
                    subject: a.subject.clone(),
                    holds: Some(false),
                    detail: None,
                }),
            );
            if faults.is_empty() {
                continue;
            }
            if world.is_none() {
                *world = Some(LinkWorld {
                    networks: self.cluster().list::<NetworkSpec>().ok()?.1,
                    vms: self.cluster().list::<VmSpec>().ok()?.1,
                });
            }
            let w = world.as_ref().expect("just read");
            let topology = physical::build(std::slice::from_ref(node), &w.networks, &w.vms);
            let impacts = topology
                .nodes
                .first()
                .map(|n| n.impact.as_slice())
                .unwrap_or_default();
            for f in faults {
                // 본딩 경보는 본딩의 영향, 없으면(구성 포트가 끊김) 그 본딩에 든 포트의 영향
                let impact = impacts
                    .iter()
                    .find(|i| i.port == f.port)
                    .or_else(|| {
                        impacts
                            .iter()
                            .find(|i| i.bond.as_deref() == Some(f.port.as_str()))
                    })
                    .cloned()
                    .unwrap_or_else(|| Impact {
                        port: f.port.clone(),
                        severity: f.severity,
                        bond: f.bond.clone(),
                        bridges: Vec::new(),
                        underlay: false,
                        networks: Vec::new(),
                        vms: 0,
                        vm_ids: Vec::new(),
                    });
                out.push(SdnFinding {
                    subject: AlertSubject {
                        kind: SubjectKind::Node,
                        id: node.id,
                        name: node.name.clone(),
                        node: Some(node.name.clone()),
                        port: Some(f.port.clone()),
                    },
                    holds: Some(true),
                    detail: Some(physical::describe(&node.name, &impact)),
                });
            }
        }
        Some(out)
    }

    /// SDN 규칙 하나(계획 67 §11)를 본 결과로 진행한다. 상태 조건은 [`advance`](값을 모르면 단계를 그대로), 이벤트
    /// (`sdn_gateway_failover`)는 참인 대상마다 한 번 보낸다. 대상이 사라진(라우터·노드를 지움, 중앙이 아니게 됨) `firing`
    /// 경보는 해제로 알린다. 끈 규칙은 알림 없이 비운다. 링크 규칙(계획 75)도 이 길이다.
    fn sdn_rule(
        &mut self,
        now: i64,
        rule: &AlertRule,
        findings: Vec<SdnFinding>,
    ) -> (AlertRuleStatus, Vec<Pending>) {
        let mut status = rule.status.clone();
        let mut out = Vec::new();
        if rule.spec.condition.is_event() {
            status.active.clear();
            if rule.spec.enabled {
                for f in damp_events(&mut self.events_sent, rule, findings, now) {
                    out.push(Pending {
                        alert: event_alert(rule, f.subject, now),
                        rule: rule.clone(),
                        state: NotificationState::Event,
                        detail: f.detail,
                    });
                }
            }
            return (status, out);
        }
        let mut current: HashMap<String, ActiveAlert> =
            status.active.iter().map(|a| (a.key(), a.clone())).collect();
        let mut active = Vec::new();
        if rule.spec.enabled {
            for f in findings {
                let before = current.remove(&f.subject.key(rule.id));
                let seen = f.holds.map(|h| Observation::new(h, None));
                let (after, notice) = advance(
                    rule.id,
                    &rule.spec,
                    &f.subject,
                    before.clone(),
                    seen,
                    now,
                    self.resolve_after_ms,
                );
                if let Some(mut a) = after.clone() {
                    a.subject = f.subject.clone();
                    active.push(a);
                }
                if let Some(notice) = notice
                    && let Some(alert) = after.or(before)
                {
                    out.push(Pending {
                        rule: rule.clone(),
                        alert,
                        state: notice_state(notice),
                        detail: (notice != Notice::Resolve).then_some(f.detail).flatten(),
                    });
                }
            }
            for (_, a) in current {
                if rule.spec.send_resolved && a.state == AlertState::Firing {
                    out.push(Pending {
                        rule: rule.clone(),
                        alert: a,
                        state: NotificationState::Resolved,
                        detail: Some("The subject no longer applies to this condition.".into()),
                    });
                }
            }
        }
        active.sort_by_key(|a| a.key());
        status.active = active;
        (status, out)
    }

    fn node_offline(
        &self,
        now: i64,
        rule: &AlertRule,
        nodes: &[Node],
    ) -> (orbivisor_types::alert::AlertRuleStatus, Vec<Pending>) {
        let mut status = rule.status.clone();
        let mut current: HashMap<String, ActiveAlert> =
            status.active.iter().map(|a| (a.key(), a.clone())).collect();
        let mut active = Vec::new();
        let mut out = Vec::new();
        if rule.spec.enabled {
            for node in nodes
                .iter()
                .filter(|n| !n.spec.maintenance && rule.spec.applies_to_node(&n.name))
            {
                let subject = AlertSubject {
                    kind: SubjectKind::Node,
                    id: node.id,
                    name: node.name.clone(),
                    node: Some(node.name.clone()),
                    port: None,
                };
                let before = current.remove(&subject.key(rule.id));
                let seen = Observation::new(!node.status.online, None);
                let (after, notice) = advance(
                    rule.id,
                    &rule.spec,
                    &subject,
                    before.clone(),
                    Some(seen),
                    now,
                    self.resolve_after_ms,
                );
                if let Some(a) = &after {
                    active.push(a.clone());
                }
                if let Some(notice) = notice
                    && let Some(alert) = after.or(before)
                {
                    out.push(Pending {
                        rule: rule.clone(),
                        alert,
                        state: notice_state(notice),
                        detail: None,
                    });
                }
            }
        }
        // 지운 노드의 `firing` 경보는 해제로 알린다(설계 §11). 유지 보수·거르기·끈 규칙은 알림 없이 뺀다.
        for (_, a) in current {
            let removed = !nodes.iter().any(|n| n.id == a.subject.id);
            if removed
                && rule.spec.enabled
                && rule.spec.send_resolved
                && a.state == AlertState::Firing
            {
                out.push(Pending {
                    rule: rule.clone(),
                    alert: a,
                    state: NotificationState::Resolved,
                    detail: Some("The node was removed from the cluster.".into()),
                });
            }
        }
        active.sort_by_key(|a| a.key());
        status.active = active;
        (status, out)
    }

    /// 검증이 실패한 백업(`status.verify.state == failed`)마다 참. 저장소 백업만 본다.
    fn verify_findings(&self) -> Option<Vec<(AlertSubject, bool, Option<String>)>> {
        let (_, backups) = self.cluster().list::<BackupSpec>().ok()?;
        Some(
            backups
                .iter()
                .filter(|b| b.status.verify.is_some())
                .map(|b| {
                    let v = b.status.verify.as_ref().expect("filtered");
                    let failed = v.state == VerifyState::Failed;
                    let detail = failed.then(|| {
                        let mut d = format!(
                            "Backup {} failed verification ({} bad chunks)",
                            b.name, v.bad_chunks
                        );
                        if let Some(e) = &v.error {
                            d.push_str(&format!(": {e}"));
                        }
                        d
                    });
                    let subject = AlertSubject {
                        kind: SubjectKind::Backup,
                        id: b.id,
                        name: b.name.clone(),
                        node: None,
                        port: None,
                    };
                    (subject, failed, detail)
                })
                .collect(),
        )
    }

    /// 인증서마다: 이어진 주문 실패가 `failures`번 이상이면 참(마지막 오류를 자세히)
    /// 묶음마다: 드리프트 검사가 선언과 다른 객체를 하나라도 찾았으면 참(값은 그 수)
    fn drift_findings(&self) -> Option<Vec<(AlertSubject, bool, Option<String>)>> {
        let (_, sets) = self
            .cluster()
            .list::<orbivisor_types::apply_set::ApplySetSpec>()
            .ok()?;
        Some(
            sets.iter()
                .map(|s| {
                    let n = s.status.drift.as_ref().map_or(0, |d| d.items.len());
                    let subject = AlertSubject {
                        kind: SubjectKind::ApplySet,
                        id: s.id,
                        name: s.name.clone(),
                        node: None,
                        port: None,
                    };
                    let detail = (n > 0).then(|| {
                        format!(
                            "Apply set {} drifted: {n} object(s) differ from the declaration",
                            s.name
                        )
                    });
                    (subject, n > 0, detail)
                })
                .collect(),
        )
    }

    fn renew_findings(&self, failures: u32) -> Option<Vec<(AlertSubject, bool, Option<String>)>> {
        let (_, certs) = self
            .cluster()
            .list::<orbivisor_types::acme::CertificateSpec>()
            .ok()?;
        Some(
            certs
                .iter()
                .map(|c| {
                    let failing = c.status.failures >= failures;
                    let subject = AlertSubject {
                        kind: SubjectKind::Certificate,
                        id: c.id,
                        name: c.name.clone(),
                        node: None,
                        port: None,
                    };
                    // 노드마다 인증서는 노드마다 센다(요약 `failures`는 가장 많이 실패한 노드) — 그 노드들을 적는다.
                    let detail = failing.then(|| {
                        let nodes: Vec<String> = c
                            .status
                            .node_failures
                            .values()
                            .filter(|f| f.failures >= failures)
                            .map(|f| {
                                format!(
                                    "- {} in a row: {}",
                                    f.failures,
                                    f.error.as_deref().unwrap_or("unknown error")
                                )
                            })
                            .collect();
                        if nodes.is_empty() {
                            format!(
                                "Certificate {} failed {} ACME orders in a row: {}",
                                c.name,
                                c.status.failures,
                                c.status.error.as_deref().unwrap_or("unknown error")
                            )
                        } else {
                            format!(
                                "Certificate {} failed ACME orders on {} node(s):\n{}",
                                c.name,
                                nodes.len(),
                                nodes.join("\n")
                            )
                        }
                    });
                    (subject, failing, detail)
                })
                .collect(),
        )
    }

    /// 저장소마다: 이 저장소를 쓰는 온라인 노드 가운데 닿지 않는다고 보고한 노드가 있으면 참(노드 보고
    /// `NodeStatus.backup_stores`, 60초마다).
    fn store_findings(&self, nodes: &[Node]) -> Option<Vec<(AlertSubject, bool, Option<String>)>> {
        let (_, stores) = self.cluster().list::<BackupStoreSpec>().ok()?;
        Some(
            stores
                .iter()
                .map(|s| {
                    let failing: Vec<String> = nodes
                        .iter()
                        .filter(|n| n.status.online)
                        .filter_map(|n| {
                            let r = n.status.backup_stores.iter().find(|r| r.store == s.id)?;
                            (!r.ok).then(|| {
                                format!(
                                    "- {}: {}",
                                    n.name,
                                    r.error.as_deref().unwrap_or("unreachable")
                                )
                            })
                        })
                        .collect();
                    let subject = AlertSubject {
                        kind: SubjectKind::BackupStore,
                        id: s.id,
                        name: s.name.clone(),
                        node: None,
                        port: None,
                    };
                    let detail = (!failing.is_empty()).then(|| {
                        format!(
                            "Backup store {} is unreachable from:\n{}",
                            s.name,
                            failing.join("\n")
                        )
                    });
                    (subject, !failing.is_empty(), detail)
                })
                .collect(),
        )
    }

    /// 대상마다 참·거짓을 본 상태 조건(`for_secs`·해제 규칙은 [`advance`]). 목록에서 사라진 대상의 `firing` 경보는
    /// `gone`과 함께 해제로 알린다. 끈 규칙은 알림 없이 비운다.
    fn subject_rule(
        &self,
        now: i64,
        rule: &AlertRule,
        findings: Vec<(AlertSubject, bool, Option<String>)>,
        gone: &str,
    ) -> (orbivisor_types::alert::AlertRuleStatus, Vec<Pending>) {
        let mut status = rule.status.clone();
        let mut current: HashMap<String, ActiveAlert> =
            status.active.iter().map(|a| (a.key(), a.clone())).collect();
        let mut active = Vec::new();
        let mut out = Vec::new();
        if rule.spec.enabled {
            for (subject, holds, detail) in findings {
                let before = current.remove(&subject.key(rule.id));
                let (after, notice) = advance(
                    rule.id,
                    &rule.spec,
                    &subject,
                    before.clone(),
                    Some(Observation::new(holds, None)),
                    now,
                    self.resolve_after_ms,
                );
                if let Some(a) = &after {
                    active.push(a.clone());
                }
                if let Some(notice) = notice
                    && let Some(alert) = after.or(before)
                {
                    out.push(Pending {
                        rule: rule.clone(),
                        alert,
                        state: notice_state(notice),
                        detail: (notice != Notice::Resolve).then_some(detail).flatten(),
                    });
                }
            }
            for (_, a) in current {
                if rule.spec.send_resolved && a.state == AlertState::Firing {
                    out.push(Pending {
                        rule: rule.clone(),
                        alert: a,
                        state: NotificationState::Resolved,
                        detail: Some(gone.into()),
                    });
                }
            }
        }
        active.sort_by_key(|a| a.key());
        status.active = active;
        (status, out)
    }

    fn backup_failed(
        &self,
        now: i64,
        rule: &AlertRule,
    ) -> (orbivisor_types::alert::AlertRuleStatus, Vec<Pending>) {
        let mut out = Vec::new();
        // 읽지 못하면 그대로 둔다(빈 목록으로 보면 알린 기록을 잊어 다시 알린다).
        let Ok((_, jobs)) = self.cluster().list::<BackupJobSpec>() else {
            return (rule.status.clone(), out);
        };
        let mut status = rule.status.clone();
        status.active.clear();
        // 끈 동안에도 알린 것으로 적어 둔다(다시 켰을 때 끈 동안의 실패를 보내지 않게).
        let failed = new_failures(&mut status.notified, &jobs, created_ms(rule.id));
        if !rule.spec.enabled {
            return (status, out);
        }
        for job in failed {
            let Some(run) = &job.status.last_run else {
                continue;
            };
            let mut detail = format!(
                "Backup job {} finished with {} of {} backups failed",
                job.name,
                run.failed,
                run.ok + run.failed
            );
            for e in job.status.errors.iter().take(5) {
                detail.push_str(&format!("\n- {}: {}", e.vm, e.error));
            }
            let subject = AlertSubject {
                kind: SubjectKind::BackupJob,
                id: job.id,
                name: job.name.clone(),
                node: None,
                port: None,
            };
            out.push(Pending {
                alert: event_alert(rule, subject, now),
                rule: rule.clone(),
                state: NotificationState::Event,
                detail: Some(detail),
            });
        }
        (status, out)
    }

    /// 장부에 둔다(`persist`면 파일에 먼저 쓴다). 파일에 쓰지 못하면 경고하고 메모리에는 둔다.
    async fn save(&mut self, new: NodeAlertState, persist: bool) {
        let book = self.book.clone();
        let stored = tokio::task::spawn_blocking(move || {
            book.store(new.clone(), persist).or_else(|e| {
                book.store(new, false)?;
                Err(e)
            })
        })
        .await;
        match stored {
            Ok(Ok(())) if persist => self.persisted_at = self.clock.now_ms(),
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cannot write the alert state file"),
            Err(e) => tracing::warn!(error = %e, "cannot write the alert state file"),
        }
    }

    /// 알림을 규칙의 대상마다 대기열에 넣는다.
    fn send(&self, out: Vec<Pending>, now: i64) {
        if out.is_empty() {
            return;
        }
        let cluster = cluster_name(self.cluster());
        for p in out {
            let n = Arc::new(Notification {
                cluster: cluster.clone(),
                state: p.state,
                severity: p.rule.spec.severity,
                rule: Some(p.rule.name.clone()),
                condition: Some(p.rule.spec.condition.kind().into()),
                subject: Some(p.alert.subject.clone()),
                value: p.alert.value,
                threshold: p.rule.spec.condition.threshold(),
                since: Some(p.alert.since),
                at: now,
                detail: p.detail,
            });
            for target in &p.rule.spec.targets {
                self.sender.enqueue(target, n.clone());
            }
        }
    }
}

/// 이벤트 알림의 경보 꼴(상태를 두지 않음)
fn event_alert(rule: &AlertRule, subject: AlertSubject, now: i64) -> ActiveAlert {
    ActiveAlert {
        rule: rule.id,
        subject,
        state: AlertState::Firing,
        since: now,
        value: None,
        last_sent: Some(now),
        clear_since: None,
    }
}

fn notice_state(n: Notice) -> NotificationState {
    match n {
        Notice::Fire | Notice::Repeat => NotificationState::Firing,
        Notice::Resolve => NotificationState::Resolved,
    }
}

/// Task 종류 이름(`vm-start` …)
fn op_name(op: &TaskOp) -> String {
    serde_json::to_value(op)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
        .unwrap_or_default()
}

/// UUIDv7 id의 만든 시각(ms). 시각이 없는 id는 0이다.
/// 노드 경보 한 걸음의 결과
struct Stepped {
    active: Vec<ActiveAlert>,
    out: Vec<Pending>,
    overflow: bool,
}

/// 본 것(`observed`)으로 노드 경보(`old_active`)를 한 걸음 진행한다. 새 경보는 활성 경보가 아직 보지 않은 지금 경보까지
/// 합쳐 [`MAX_ACTIVE`]개 미만일 때만 받는다(넘으면 `overflow`). 보지 못한 지금 경보는 `partial`(목록을 읽지 못해 다
/// 보지 못함)이면 그대로 두고, 아니면(대상이 사라짐, 규칙을 끄거나 지움, 유지 보수) 알림 없이 뺀다.
fn step_node_alerts(
    observed: Vec<(&AlertRule, AlertSubject, Option<Observation>)>,
    old_active: &[ActiveAlert],
    partial: bool,
    now: i64,
    resolve_after_ms: i64,
) -> Stepped {
    let mut current: HashMap<String, ActiveAlert> =
        old_active.iter().map(|a| (a.key(), a.clone())).collect();
    let mut active = Vec::new();
    let mut out = Vec::new();
    let mut overflow = false;
    for (rule, subject, seen) in observed {
        let before = current.remove(&subject.key(rule.id));
        if before.is_none()
            && active.len() + current.len() >= MAX_ACTIVE
            && seen.is_some_and(|s: Observation| s.holds)
        {
            overflow = true;
            continue;
        }
        let (after, notice) = advance(
            rule.id,
            &rule.spec,
            &subject,
            before.clone(),
            seen,
            now,
            resolve_after_ms,
        );
        if let Some(mut a) = after.clone() {
            // 이름이 바뀌었으면 새 이름으로 알린다.
            a.subject = subject.clone();
            active.push(a);
        }
        if let Some(notice) = notice
            && let Some(alert) = after.or(before)
        {
            out.push(Pending {
                rule: rule.clone(),
                alert,
                state: notice_state(notice),
                detail: None,
            });
        }
    }
    if partial {
        active.extend(current.into_values());
    }
    active.sort_by_key(|a| a.key());
    Stepped {
        active,
        out,
        overflow,
    }
}

/// 백업 작업의 아직 알리지 않은 실패한 마지막 실행(규칙을 만든 뒤 끝난 것)을 `notified`에 적고 그 작업들을 돌려준다.
/// 지운 작업은 잊는다.
fn new_failures<'j>(
    notified: &mut BTreeMap<Uuid, i64>,
    jobs: &'j [BackupJob],
    created: i64,
) -> Vec<&'j BackupJob> {
    notified.retain(|id, _| jobs.iter().any(|j| j.id == *id));
    let mut out = Vec::new();
    for job in jobs {
        let Some(run) = &job.status.last_run else {
            continue;
        };
        let at = i64::try_from(run.finished_at)
            .unwrap_or(i64::MAX / 1000)
            .saturating_mul(1000);
        if run.failed == 0 || at < created || notified.get(&job.id).is_some_and(|t| *t >= at) {
            continue;
        }
        notified.insert(job.id, at);
        out.push(job);
    }
    out
}

fn created_ms(id: Uuid) -> i64 {
    id.get_timestamp().map_or(0, |ts| {
        let (secs, nanos) = ts.to_unix();
        secs as i64 * 1000 + i64::from(nanos / 1_000_000)
    })
}

/// 클러스터 이름(없으면 `orbivisor`)
pub(crate) fn cluster_name(cluster: &Cluster) -> String {
    cluster
        .list::<ClusterSpec>()
        .ok()
        .and_then(|(_, c)| c.into_iter().next())
        .map_or_else(|| "orbivisor".into(), |c| c.name)
}

/// 이 노드가 가진 `firing` 경보 수(심각도 순: info, warning, critical). 노드 경보와, 리더면 리더 경보다. 장부가 없으면
/// None이다(Prometheus `orbivisor_alerts_firing`).
pub(crate) fn firing_counts(state: &AppState) -> Option<[u64; 3]> {
    let book = state.alerts.as_ref()?;
    let rules: BTreeMap<Uuid, Typed<AlertRuleSpec>> = state
        .cluster
        .list::<AlertRuleSpec>()
        .ok()?
        .1
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let mut counts = [0u64; 3];
    let mut add = |a: &ActiveAlert| {
        if a.state == AlertState::Firing
            && let Some(r) = rules.get(&a.rule)
        {
            counts[r.spec.severity as usize] += 1;
        }
    };
    book.firing().iter().for_each(&mut add);
    if state.cluster.members().leader == Some(NodeId(state.node)) {
        for r in rules.values() {
            r.status.active.iter().for_each(&mut add);
        }
    }
    Some(counts)
}

#[cfg(test)]
mod tests {
    use orbivisor_types::resource::Resource;
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    /// 리뷰 2 m5: 같은 임기의 관찰은 앞서 안 게이트웨이 주인을 이어 받는다(주인이 없던 관찰을 지나도 A → B를 본다).
    /// 다른 임기의 것은 받지 않는다.
    #[test]
    fn hosting_is_remembered_across_observations_without_it() {
        let r = Uuid::from_u128(7);
        let obs = |term: u64, host: Option<&str>| SdnObservations {
            term,
            at: 1,
            hosting: host.map(|h| (r, h.to_string())).into_iter().collect(),
            monitors: vec![],
        };
        let a = merge_hosting(None, obs(3, Some("a")));
        let none = merge_hosting(Some(&a), obs(3, None));
        assert_eq!(none.hosting.get(&r).map(String::as_str), Some("a"));
        let b = merge_hosting(Some(&none), obs(3, Some("b")));
        assert_eq!(b.hosting.get(&r).map(String::as_str), Some("b"));
        let other = merge_hosting(Some(&a), obs(4, None));
        assert!(other.hosting.is_empty());
    }

    /// 리뷰 2 m5: 같은 대상의 같은 일은 `repeat_secs` 안에 한 번만 보낸다. 다른 대상·다른 일(되돌아감)·창이 지난 뒤에는
    /// 보낸다.
    #[test]
    fn events_are_damped_per_subject() {
        let mut r = rule();
        r.spec.repeat_secs = 60;
        let found = |n: u128, d: &str| SdnFinding {
            subject: vm(n),
            holds: Some(true),
            detail: Some(d.to_string()),
        };
        let mut sent = HashMap::new();
        assert_eq!(
            damp_events(&mut sent, &r, vec![found(1, "a→b")], NOW).len(),
            1
        );
        assert_eq!(
            damp_events(&mut sent, &r, vec![found(1, "b→a")], NOW + 5_000).len(),
            1
        );
        assert!(damp_events(&mut sent, &r, vec![found(1, "a→b")], NOW + 10_000).is_empty());
        assert!(damp_events(&mut sent, &r, vec![found(1, "b→a")], NOW + 15_000).is_empty());
        assert_eq!(
            damp_events(&mut sent, &r, vec![found(2, "a→b")], NOW + 10_000).len(),
            1
        );
        assert_eq!(
            damp_events(&mut sent, &r, vec![found(1, "a→b")], NOW + 61_000).len(),
            1
        );
    }

    fn rule() -> AlertRule {
        let spec: AlertRuleSpec = serde_json::from_value(json!({
            "condition": {"kind": "vm_failed"}, "severity": "warning", "targets": ["ops"]
        }))
        .unwrap();
        Resource::new("vm-failed", spec)
    }

    fn vm(n: u128) -> AlertSubject {
        AlertSubject {
            kind: SubjectKind::Vm,
            id: Uuid::from_u128(n),
            name: format!("vm{n}"),
            node: Some("node1".into()),
            port: None,
        }
    }

    fn firing(rule: &AlertRule, n: u128) -> ActiveAlert {
        ActiveAlert {
            rule: rule.id,
            subject: vm(n),
            state: AlertState::Firing,
            since: NOW - 60_000,
            value: None,
            last_sent: Some(NOW - 60_000),
            clear_since: None,
        }
    }

    /// 아직 보지 않은 지금 경보까지 세어 새 경보를 받으므로 활성 경보는 상한을 넘지 않는다(리뷰 1 #6).
    #[test]
    fn the_active_alert_cap_counts_alerts_not_yet_seen() {
        let r = rule();
        let old: Vec<ActiveAlert> = (0..MAX_ACTIVE as u128).map(|n| firing(&r, n)).collect();
        let holds = Some(Observation::new(true, None));
        // 새 대상이 앞에, 지금 경보가 모두 뒤에 온다.
        let observed = std::iter::once((&r, vm(10_000), holds))
            .chain((0..MAX_ACTIVE as u128).map(|n| (&r, vm(n), holds)))
            .collect();
        let s = step_node_alerts(observed, &old, false, NOW, RESOLVE_AFTER_MS);
        assert!(s.overflow);
        assert_eq!(s.active.len(), MAX_ACTIVE);
        assert!(s.out.is_empty());
    }

    /// 목록을 읽지 못해 보지 못한 경보는 알림 없이 빼지 않고 그대로 둔다. 다 보았는데 없으면 뺀다(리뷰 1 #7).
    #[test]
    fn unseen_alerts_stay_when_the_lists_could_not_be_read() {
        let r = rule();
        let old = vec![firing(&r, 1), firing(&r, 2)];
        let observed = vec![(&r, vm(1), Some(Observation::new(true, None)))];
        let kept = step_node_alerts(observed.clone(), &old, true, NOW, RESOLVE_AFTER_MS);
        assert_eq!(kept.active, old);
        assert!(kept.out.is_empty());
        let dropped = step_node_alerts(observed, &old, false, NOW, RESOLVE_AFTER_MS);
        assert_eq!(dropped.active, [firing(&r, 1)]);
        // 값을 모르면(`None`) 단계를 그대로 둔다.
        let unknown = vec![(&r, vm(1), None), (&r, vm(2), None)];
        let s = step_node_alerts(unknown, &old, false, NOW, RESOLVE_AFTER_MS);
        assert_eq!(s.active, old);
    }

    fn job(n: u128, finished_at: u64, failed: u32) -> BackupJob {
        let spec = serde_json::from_value(json!({
            "vms": {"labels": {"tier": "prod"}}, "pool": "backups", "schedule": {"at": "03:00"},
        }))
        .unwrap();
        let mut job: BackupJob = Resource::new(format!("job{n}"), spec);
        job.id = Uuid::from_u128(n);
        job.status.last_run = Some(orbivisor_types::backup_job::JobRunResult {
            started_at: finished_at - 10,
            finished_at,
            ok: 1,
            failed,
        });
        job
    }

    /// 규칙을 끈 동안의 실패도 알린 것으로 적어 두므로 다시 켰을 때 보내지 않는다. 새 실패만 보낸다(리뷰 1 #13).
    #[test]
    fn failures_while_a_rule_was_off_are_not_sent_later() {
        let created = 1_000_000;
        let mut notified = BTreeMap::new();
        // 규칙이 꺼진 동안 실패했다(꺼진 동안에도 부른다).
        let jobs = vec![job(1, 2_000, 1), job(2, 2_000, 0)];
        let failed = new_failures(&mut notified, &jobs, created);
        assert_eq!(failed.len(), 1);
        assert_eq!(notified, BTreeMap::from([(Uuid::from_u128(1), 2_000_000)]));
        // 다시 켰다: 같은 실행은 알리지 않고 새 실패만 알린다.
        assert!(new_failures(&mut notified, &jobs, created).is_empty());
        let jobs = vec![job(1, 3_000, 2)];
        assert_eq!(new_failures(&mut notified, &jobs, created).len(), 1);
        // 규칙을 만들기 전에 끝난 실행과 지운 작업은 보지 않는다.
        assert!(new_failures(&mut notified, &[job(3, 500, 1)], created).is_empty());
        assert!(!notified.contains_key(&Uuid::from_u128(1)));
    }
}
