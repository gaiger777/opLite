//! 균형 조정기(설계 §8.3): 리더가 `interval`마다 노드 부하로 불균형을 재고 옮기기를 계산한다([`crate::balance`]).
//! 모드 `recommend`면 추천을 게시판([`PlacementBoard`])에 두고, `auto`면 보통의 마이그레이션 Task로 곧바로 옮기고
//! 감사 로그에 `placement.move`를 남긴다. 리더 메모리에만 있다(리더를 잃으면 게시판과 옮긴 VM 표를 비운다). 균형 옮기기
//! 중인지(히스테리시스)와 노드가 살아 있음을 처음 본 때도 리더 메모리다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use opl_types::audit::AuditTarget;
use opl_types::node::{NodeSpec, PLACEMENT};
use opl_types::placement::{ImbalanceView, MoveReason, PlacementMove};
use opl_types::settings::{PlacementMode, PlacementPolicy, PlacementSettings};
use opl_types::task::{TaskOp, TaskSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::audit::system_entry;
use crate::auth::unix_now;
use crate::balance::{
    self, Imbalance, MAX_CONCURRENT, MAX_RECOMMENDATIONS, Model, Plan, VM_COOLDOWN,
};
use crate::error::ApiError;
use crate::ha_health::Health;
use crate::placement_board::{PlacementBoard, permille};
use crate::scheduler::Inventory;
use crate::score::Loads;
use crate::state::AppState;
use crate::vms::{RuleCheck, start_migration};

/// 리더가 된 뒤 첫 회차까지 기다리는 가장 긴 시간
const FIRST_RUN: Duration = Duration::from_secs(60);

/// `pack` 정책이면 균형 옮기기를 하지 않는다(규칙 바로잡기만).
const PACK_PAUSED: &str = "policy is pack";

#[derive(Debug, Clone)]
pub struct BalancerConfig {
    /// 리더인지·회차 때인지 보는 간격
    pub tick: Duration,
    /// 회차 간격. 리더가 된 뒤 첫 회차는 이것과 60초 가운데 짧은 것 뒤다.
    pub interval: Duration,
    /// 균형 옮기기를 받는 노드는 이 리더가 이만큼 계속 살아 있음을 본 노드다. 기본은 HA 되돌아가기 지연(30초) + 60초로,
    /// 돌아온 노드는 HA가 되돌린 뒤에 채운다.
    pub settle: Duration,
}

impl Default for BalancerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            interval: Duration::from_secs(300),
            settle: crate::ha::HaManagerConfig::default().failback_after + Duration::from_secs(60),
        }
    }
}

/// 균형 조정기 반복 작업. 위트니스가 아닌 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct Balancer {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Balancer {
    pub fn spawn(state: Arc<AppState>, config: BalancerConfig) -> Self {
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

/// 이 리더의 기억. 리더 자리를 잃으면 비운다.
#[derive(Debug, Default)]
struct State {
    /// 리더로 본 임기와 그 임기에서 리더인 것을 처음 본 때(리스 기준점 — HA와 같다)
    base: Option<(u64, Instant)>,
    /// 다음 회차
    due: Option<Instant>,
    /// 이 리더가 옮긴 VM과 그 때(유닉스 초). 끝난 Task가 지워져도 [`VM_COOLDOWN`] 동안 다시 옮기지 않는다.
    moved: BTreeMap<Uuid, u64>,
    /// 옮기기가 거절된 VM(로그를 VM마다 한 번만 남긴다)
    refused: BTreeSet<Uuid>,
    /// 마지막으로 남긴 회차 요약(바뀔 때만 로그)
    logged: Option<String>,
    /// 균형 옮기기 중이다(지난 회차의 [`Plan::balancing`])
    balancing: bool,
    /// 노드마다 이 리더가 살아 있음([`balance::live`])을 계속 본 처음 때. 임기가 바뀌면 비운다.
    alive_since: BTreeMap<Uuid, Instant>,
    /// 마지막으로 본 최적 배치 설정. 바뀌면 게시판의 추천을 비운다(모드·문턱을 바꾼 뒤 옛 추천을 적용하지 않게).
    settings: Option<PlacementSettings>,
}

async fn run(state: Arc<AppState>, config: BalancerConfig, mut shutdown: oneshot::Receiver<()>) {
    let mut memory = State::default();
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 읽기 장벽이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut memory) => {}
        }
    }
}

async fn tick(app: &AppState, config: &BalancerConfig, s: &mut State) {
    let cluster = &app.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        if s.due.is_some() {
            *s = State::default();
            app.placement.update(|b| *b = Default::default());
        }
        return;
    }
    let now = Instant::now();
    if let Ok(settings) = crate::settings::current(cluster) {
        let placement = settings.spec.placement();
        if s.settings.is_some_and(|p| p != placement) {
            app.placement.update(|b| b.recommendations.clear());
        }
        s.settings = Some(placement);
    }
    observe(app, s, now);
    // 리더에서 추천을 적용할 때 균형 조정기와 같은 쉼·살아 있음을 본다.
    app.placement.update(|b| {
        b.alive_since = s.alive_since.clone();
        b.base = s.base.map(|(_, at)| at);
        b.settle = config.settle;
    });
    let due = match s.due {
        Some(due) => due,
        None => {
            let due = now + config.interval.min(FIRST_RUN);
            s.due = Some(due);
            publish_next(&app.placement, due, now);
            due
        }
    };
    if now < due {
        return;
    }
    let next = now + config.interval;
    s.due = Some(next);
    run_once(app, s, config.settle).await;
    publish_next(&app.placement, next, Instant::now());
}

/// 리스 기준점(HA와 같다): 이 임기에서 리더인 것을 처음 본 때. 임기가 바뀌면 노드가 살아 있음을 본 때도 잊는다(그사이의
/// 리스 갱신을 이 리더가 보지 못했다).
fn base(s: &mut State, term: u64, now: Instant) -> Instant {
    match s.base {
        Some((t, b)) if t == term => b,
        _ => {
            s.base = Some((term, now));
            s.alive_since.clear();
            now
        }
    }
}

/// 노드마다 살아 있음을 처음 본 때를 적는다(살아 있지 않으면 지운다).
fn observe(app: &AppState, s: &mut State, now: Instant) {
    let Ok((_, nodes)) = app.cluster.list::<NodeSpec>() else {
        return;
    };
    let leases = app.cluster.leases();
    let b = base(s, leases.term, now);
    for n in &nodes {
        if balance::live(n, &leases, now, b) {
            s.alive_since.entry(n.id).or_insert(now);
        } else {
            s.alive_since.remove(&n.id);
        }
    }
    s.alive_since
        .retain(|id, _| nodes.iter().any(|n| n.id == *id));
}

/// 다음 회차 시각을 게시판에 적는다.
fn publish_next(board: &PlacementBoard, due: Instant, now: Instant) {
    let at = unix_now() + due.saturating_duration_since(now).as_secs();
    board.update(|b| b.next_run = Some(at));
}

fn view(i: Imbalance) -> ImbalanceView {
    ImbalanceView {
        cpu_pct: permille(i.cpu),
        mem_pct: permille(i.mem),
        total_pct: permille(i.total),
    }
}

/// 한 회차: 장벽 뒤 클러스터를 읽고, 쉴 이유가 없으면 옮기기를 계산해(무거운 계산은 `spawn_blocking`) 모드대로
/// 게시판에 두거나 옮긴다. 계산한 계획을 돌려준다(쉬었거나 읽지 못했으면 None). `settle`만큼 살아 있음을 본 노드만
/// 균형 옮기기를 받는다.
async fn run_once(app: &AppState, s: &mut State, settle: Duration) -> Option<Plan> {
    let cluster = &app.cluster;
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    cluster.read_barrier().await.ok()?;
    let settings = crate::settings::current(cluster).ok()?.spec.placement();
    let inv = Inventory::load(cluster).ok()?;
    let (_, tasks) = cluster.list::<TaskSpec>().ok()?;
    let missing = crate::cluster::missing_feature(cluster, PLACEMENT).ok()?;
    let leases = cluster.leases();
    let now = Instant::now();
    let b = base(s, leases.term, now);
    let live: BTreeMap<Uuid, bool> = s
        .alive_since
        .iter()
        .map(|(id, since)| (*id, now.saturating_duration_since(*since) >= settle))
        .collect();
    let unix = unix_now();
    s.moved
        .retain(|_, at| at.saturating_add(VM_COOLDOWN.as_secs()) > unix);
    let paused = {
        let health = Health::load(cluster, &leases).ok()?;
        balance::paused(
            &inv,
            &leases,
            now,
            b,
            &settings,
            &tasks,
            &missing,
            &health,
            &s.alive_since,
        )
    };
    let plan = match &paused {
        Some(_) => None,
        None => {
            let migrating = tasks
                .iter()
                .filter(|t| {
                    !t.status.state.is_finished() && matches!(t.spec.op, TaskOp::VmMigrate { .. })
                })
                .count();
            let max_moves = match settings.mode {
                PlacementMode::Auto => MAX_CONCURRENT.saturating_sub(migrating),
                _ => MAX_RECOMMENDATIONS,
            };
            let (cluster, moved, inv) = (cluster.clone(), s.moved.clone(), inv.clone());
            let balancing = s.balancing;
            // 계산은 노드 × VM만큼 걸리므로 실행기 밖에서 한다.
            let computed = tokio::task::spawn_blocking(move || {
                let loads = Loads::from_leases(&leases, &inv.nodes, now);
                let health = Health::load(&cluster, &leases).ok()?;
                let model = Model::new(&inv, &loads, &health, &tasks, &moved, unix, &live);
                Some(balance::plan(&model, &settings, max_moves, balancing))
            })
            .await;
            match computed {
                Ok(plan) => plan,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot compute placement moves");
                    None
                }
            }
        }
    };
    if let Some(plan) = &plan {
        s.balancing = plan.balancing;
    }
    let paused = paused.or_else(|| {
        (settings.policy == PlacementPolicy::Pack && plan.is_some()).then(|| PACK_PAUSED.into())
    });
    let mut moves: Vec<PlacementMove> = plan
        .iter()
        .flat_map(|p| &p.moves)
        .map(|m| {
            let vm = inv.vms.iter().find(|v| v.id == m.vm);
            let name = |id: Uuid| {
                inv.nodes
                    .iter()
                    .find(|n| n.id == id)
                    .map_or_else(String::new, |n| n.name.clone())
            };
            PlacementMove {
                id: Uuid::now_v7(),
                vm: m.vm,
                vm_name: vm.map_or_else(String::new, |v| v.name.clone()),
                from: m.from,
                from_name: name(m.from),
                to: m.to,
                to_name: name(m.to),
                reason: m.reason,
                gain_pct: permille(m.gain),
                cost_mib: m.cost_mib,
                rules: m.rules.clone(),
                task: None,
            }
        })
        .collect();
    if settings.mode == PlacementMode::Auto
        && let Some(plan) = &plan
    {
        moves = start_moves(app, s, plan, moves, unix).await;
    }
    let summary = format!(
        "imbalance {}, {} moves, paused: {}",
        plan.as_ref()
            .map_or_else(|| "-".into(), |p| format!("{:.1}%", p.before.total * 100.0)),
        moves.len(),
        paused.as_deref().unwrap_or("no")
    );
    if s.logged.as_ref() != Some(&summary) {
        tracing::info!(mode = ?settings.mode, "placement balancer: {summary}");
        s.logged = Some(summary);
    }
    app.placement.update(|board| {
        board.last_run = Some(unix);
        board.imbalance = plan.as_ref().map(|p| view(p.before));
        board.paused = paused;
        board.recommendations = moves;
    });
    plan
}

/// auto: 계획한 옮기기를 차례로 시작하고 결과를 [`settle`]로 반영한다. 시작한 것만 Task id와 함께 돌려주고 감사 로그에
/// 남긴다.
async fn start_moves(
    app: &AppState,
    s: &mut State,
    plan: &Plan,
    moves: Vec<PlacementMove>,
    unix: u64,
) -> Vec<PlacementMove> {
    let mut results = Vec::new();
    for m in &moves {
        let result = start_migration(app, m.vm, m.to, None, Some(m.from), RuleCheck::Enforce).await;
        results.push(result.map(|(task, _)| task.id));
    }
    let (started, audits) = settle(s, plan, moves, results, unix);
    for (key, detail) in audits {
        let target = AuditTarget {
            kind: "vms".into(),
            key,
        };
        app.audit
            .record(system_entry("placement.move", target, detail));
    }
    started
}

/// auto의 시작 결과(옮기기마다 Task id나 거절)를 반영한다: 시작한 것만 Task id와 함께 돌려주고 이 리더의 옮긴 VM 표에
/// 넣고, 감사 항목(VM 이름, 문구)을 만든다. 문구의 괄호는 이유(`balance`, `rule <규칙>`)이고 "imbalance X -> Y"는 회차
/// 처음 불균형에서 앞서 시작한 옮기기의 효과만 뺀 것이다. 거절(그사이 바뀐 상태)은 VM마다 한 번 info 로그를 남기고 다음 회차에 다시 본다.
fn settle(
    s: &mut State,
    plan: &Plan,
    moves: Vec<PlacementMove>,
    results: Vec<Result<Uuid, ApiError>>,
    unix: u64,
) -> (Vec<PlacementMove>, Vec<(String, String)>) {
    let mut started = Vec::new();
    let mut audits = Vec::new();
    let mut total = plan.before.total;
    for ((mut m, planned), result) in moves.into_iter().zip(&plan.moves).zip(results) {
        match result {
            Ok(task) => {
                let before = total;
                total -= planned.gain;
                s.moved.insert(m.vm, unix);
                s.refused.remove(&m.vm);
                let reason = match m.reason {
                    MoveReason::Balance => "balance".to_string(),
                    MoveReason::Rule => format!("rule {}", m.rules.join(", ")),
                };
                let detail = format!(
                    "auto: {} {} -> {} ({reason}), imbalance {:.1}% -> {:.1}%, cost {} MiB",
                    m.vm_name,
                    m.from_name,
                    m.to_name,
                    before * 100.0,
                    total * 100.0,
                    m.cost_mib
                );
                tracing::info!(task = %task, "placement balancer: {detail}");
                audits.push((m.vm_name.clone(), detail));
                m.task = Some(task);
                started.push(m);
            }
            Err(e) => {
                if s.refused.insert(m.vm) {
                    tracing::info!(
                        vm = %m.vm_name,
                        from = %m.from_name,
                        to = %m.to_name,
                        code = e.code,
                        error = %e.message,
                        "placement balancer: cannot move the VM; trying again next run"
                    );
                }
            }
        }
    }
    (started, audits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balance::Move;

    fn planned(gain: f64) -> Move {
        Move {
            vm: Uuid::now_v7(),
            from: Uuid::nil(),
            to: Uuid::nil(),
            reason: MoveReason::Balance,
            gain,
            cost_mib: 1024,
            rules: vec![],
        }
    }

    fn shown(m: &Move, name: &str) -> PlacementMove {
        PlacementMove {
            id: Uuid::now_v7(),
            vm: m.vm,
            vm_name: name.into(),
            from: m.from,
            from_name: "a".into(),
            to: m.to,
            to_name: "b".into(),
            reason: m.reason,
            gain_pct: 0,
            cost_mib: m.cost_mib,
            rules: vec![],
            task: None,
        }
    }

    #[test]
    fn the_audited_imbalance_subtracts_only_started_moves() {
        let plan = Plan {
            before: Imbalance {
                total: 0.2,
                ..Default::default()
            },
            moves: vec![planned(0.05), planned(0.03)],
            ..Default::default()
        };
        let moves = vec![shown(&plan.moves[0], "v1"), shown(&plan.moves[1], "v2")];
        let task = Uuid::now_v7();
        let results = vec![Err(ApiError::conflict("vm_busy", "busy")), Ok(task)];
        let mut s = State::default();
        let (started, audits) = settle(&mut s, &plan, moves, results, 100);
        assert_eq!(started.len(), 1);
        assert_eq!(
            (started[0].vm, started[0].task),
            (plan.moves[1].vm, Some(task))
        );
        assert_eq!(
            s.moved.keys().copied().collect::<Vec<_>>(),
            [plan.moves[1].vm]
        );
        assert!(s.refused.contains(&plan.moves[0].vm));
        assert_eq!(
            audits,
            [(
                "v2".to_string(),
                "auto: v2 a -> b (balance), imbalance 20.0% -> 17.0%, cost 1024 MiB".to_string()
            )]
        );
        // 규칙 바로잡기는 규칙 이름을 적는다.
        let mut rule = planned(0.0);
        rule.reason = MoveReason::Rule;
        let plan = Plan {
            moves: vec![rule],
            ..Default::default()
        };
        let mut shown = shown(&plan.moves[0], "v3");
        shown.rules = vec!["on-b".into(), "zone".into()];
        let (_, audits) = settle(&mut s, &plan, vec![shown], vec![Ok(task)], 100);
        assert_eq!(
            audits[0].1,
            "auto: v3 a -> b (rule on-b, zone), imbalance 0.0% -> 0.0%, cost 1024 MiB"
        );
    }

    /// 임기가 바뀌면 노드가 살아 있음을 본 때를 잊는다(새 임기의 리스 갱신으로 다시 센다).
    #[test]
    fn a_new_term_forgets_when_nodes_were_seen_alive() {
        let t0 = Instant::now();
        let mut s = State::default();
        assert_eq!(base(&mut s, 1, t0), t0);
        s.alive_since.insert(Uuid::nil(), t0);
        assert_eq!(base(&mut s, 1, t0 + Duration::from_secs(1)), t0);
        assert_eq!(s.alive_since.len(), 1);
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(base(&mut s, 2, t1), t1);
        assert!(s.alive_since.is_empty());
    }
}
