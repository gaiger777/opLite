//! 경보 엔진(설계 §8): 모든 노드(위트니스 제외)에서 돈다. `tick`(5초)마다 상태 규칙을 보고, `Cluster::watch`로 끝난
//! Task를 받는다.
//!
//! - **노드 경보**(지표·`VmFailed`·로컬 풀 `PoolUsage`·`TaskFailed`·`HaRecovery`)는 그 일이 일어난 노드가 평가하고 상태를
//!   노드 장부(`alerts/state.json`)에 둔다. 공유 풀은 그 풀이 `ready`인 온라인 노드 가운데 id가 가장 작은 노드가 본다.
//!   Raft에 쓰지 않는다.
//! - **리더 경보**(`NodeOffline`·`BackupJobFailed`)는 리더가 평가하고 상태를 `AlertRule.status`(Raft)에 둔다. 상태를 먼저
//!   쓰고 쓰기가 된 뒤에만 보내므로 리더가 바뀌어도 다시 보내지 않는다.
//!
//! 단계는 [`opl_types::alert::advance`]다: pending → firing(알림) → `repeat_secs`마다 다시 → 거짓인 채 60초 → 해제(알림).
//! 노드 장부도 파일에 먼저 쓰고 보내므로 opld를 다시 시작해도 같은 알림을 다시 보내지 않는다.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::NodeId;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, Typed};
use opl_types::alert::{
    ActiveAlert, AlertCondition, AlertRule, AlertRuleSpec, AlertState, AlertSubject, Evaluator,
    Notice, NotificationState, Observation, RESOLVE_AFTER_MS, SubjectKind, advance,
};
use opl_types::backup_job::BackupJobSpec;
use opl_types::clock::{Clock, SystemClock};
use opl_types::cluster::ClusterSpec;
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, VmSample};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Kind;
use opl_types::storage::{StoragePoolSpec, is_shared};
use opl_types::task::{Task, TaskOp, TaskState};
use opl_types::vm::{Vm, VmPhase, VmSpec};
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
}

/// 경보 하나와 보낼 알림
struct Pending {
    rule: AlertRule,
    alert: ActiveAlert,
    state: NotificationState,
    detail: Option<String>,
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

    fn rules(&self) -> Vec<AlertRule> {
        self.cluster()
            .list::<AlertRuleSpec>()
            .map(|(_, rules)| rules)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.spec.enabled)
            .collect()
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
        let rules = self.rules();
        let now = self.clock.now_ms();
        let nodes = self
            .cluster()
            .list::<NodeSpec>()
            .map(|(_, n)| n)
            .unwrap_or_default();
        self.node_rules(now, &rules, &nodes).await;
        self.leader_rules(now, &nodes).await;
    }

    /// 이 노드가 평가하는 상태 규칙
    async fn node_rules(&mut self, now: i64, rules: &[AlertRule], nodes: &[Node]) {
        let old = self.book.snapshot();
        let mut observed: Vec<(&AlertRule, AlertSubject, Option<Observation>)> = Vec::new();
        if let Some(me) = nodes
            .iter()
            .find(|n| n.id == self.state.node && !n.spec.witness)
        {
            self.observe(rules, me, nodes, &mut observed);
        }
        let mut current: HashMap<String, ActiveAlert> =
            old.active.iter().map(|a| (a.key(), a.clone())).collect();
        let mut active = Vec::new();
        let mut out = Vec::new();
        let mut overflow = false;
        for (rule, subject, seen) in observed {
            let before = current.remove(&subject.key(rule.id));
            if before.is_none()
                && active.len() >= MAX_ACTIVE
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
                self.resolve_after_ms,
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
        // 남은 것(대상이 사라짐, 규칙을 끄거나 지움, 유지 보수)은 알림 없이 뺀다.
        active.sort_by_key(|a| a.key());
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

    /// 이 노드가 평가하는 상태 규칙의 (규칙, 대상, 본 것)
    fn observe<'r>(
        &mut self,
        rules: &'r [AlertRule],
        me: &Node,
        nodes: &[Node],
        out: &mut Vec<(&'r AlertRule, AlertSubject, Option<Observation>)>,
    ) {
        let here = |r: &AlertRule| {
            r.spec.condition.evaluator() == Evaluator::Node
                && !r.spec.condition.is_event()
                && r.spec.applies_to_node(&me.name)
        };
        let rules: Vec<&AlertRule> = rules.iter().filter(|r| here(r)).collect();
        if rules.is_empty() {
            return;
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
            let vms: Vec<Vm> = self
                .cluster()
                .list::<VmSpec>()
                .map(|(_, v)| v)
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
                        AlertCondition::VmFailed => {
                            let failed = vm.status.phase == VmPhase::Failed;
                            out.push((r, subject.clone(), Some(Observation::new(failed, None))));
                        }
                        _ => {}
                    }
                }
            }
            // 이 노드를 떠난 VM의 시리즈는 잊는다.
            self.last_t
                .retain(|id, _| *id == me.id || vms.iter().any(|v| v.id == *id));
        }
        // 풀 사용률: 로컬 풀은 이 노드, 공유 풀은 준비된 온라인 노드 가운데 id가 가장 작은 노드
        let pool_rules: Vec<&&AlertRule> = rules
            .iter()
            .filter(|r| matches!(r.spec.condition, AlertCondition::PoolUsage { .. }))
            .collect();
        if !pool_rules.is_empty() {
            let pools = self
                .cluster()
                .list::<StoragePoolSpec>()
                .map(|(_, p)| p)
                .unwrap_or_default();
            for (name, ps) in &me.status.pools {
                let Some(pool) = pools.iter().find(|p| p.name == *name && p.id == ps.pool_id)
                else {
                    continue;
                };
                if !ps.ready {
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
                let subject = AlertSubject {
                    kind: SubjectKind::Pool,
                    id: pool.id,
                    name: pool.name.clone(),
                    node: Some(me.name.clone()),
                };
                for r in &pool_rules {
                    let seen = r
                        .spec
                        .condition
                        .observe_pool(ps.total_bytes, ps.available_bytes);
                    out.push((r, subject.clone(), seen));
                }
            }
        }
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
        for rule in all
            .into_iter()
            .filter(|r| r.spec.condition.evaluator() == Evaluator::Leader)
        {
            let (status, out) = match rule.spec.condition {
                AlertCondition::NodeOffline => self.node_offline(now, &rule, nodes),
                AlertCondition::BackupJobFailed => self.backup_failed(now, &rule),
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
    }

    fn node_offline(
        &self,
        now: i64,
        rule: &AlertRule,
        nodes: &[Node],
    ) -> (opl_types::alert::AlertRuleStatus, Vec<Pending>) {
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

    fn backup_failed(
        &self,
        now: i64,
        rule: &AlertRule,
    ) -> (opl_types::alert::AlertRuleStatus, Vec<Pending>) {
        let mut status = rule.status.clone();
        status.active.clear();
        let mut out = Vec::new();
        let jobs = self
            .cluster()
            .list::<BackupJobSpec>()
            .map(|(_, j)| j)
            .unwrap_or_default();
        status
            .notified
            .retain(|id, _| jobs.iter().any(|j| j.id == *id));
        if !rule.spec.enabled {
            return (status, out);
        }
        // 규칙을 만들기 전에 끝난 실행은 알리지 않는다.
        let created = created_ms(rule.id);
        for job in &jobs {
            let Some(run) = &job.status.last_run else {
                continue;
            };
            let at = i64::try_from(run.finished_at)
                .unwrap_or(i64::MAX / 1000)
                .saturating_mul(1000);
            if run.failed == 0
                || at < created
                || status.notified.get(&job.id).is_some_and(|t| *t >= at)
            {
                continue;
            }
            status.notified.insert(job.id, at);
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
