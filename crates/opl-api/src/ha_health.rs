//! 리더의 HA 상태 검사 판단(설계 §8.2). 노드가 리스 갱신에 실어 보낸 검사 결과(`Leases.checks`)를 틱마다 보고,
//! 살아 있는 노드에서 `grace_secs` 넘게 실패한 검사에 `on_fail`대로 대응한다. grace는 리더 시계로만 재고, 리더를
//! 잃거나 임기가 바뀌면 처음부터 다시 잰다(안전한 쪽).
//!
//! 같은 검사를 보고한 노드의 과반이 실패하면 노드 문제가 아니라 공유 장비 문제로 보고 경고만 한다(기록 Task 한
//! 번). 자동 관리 검사(`cluster-link-*`)는 늘 경고만 한다. 대응은 모든 노드가 `ha-checks`를 알 때만 하고, 아니면
//! 경고 로그만 남긴다. 자동 대응은 HA VM(`ha_active`)만 한다.
//!
//! 대상 거르기([`Health::blocks`]): 노드 장애 복구·재시작 옮기기·되돌아가기·`migrate`는 그 VM에 영향을 주는
//! `storage`·`vm` 검사가 실패한 노드로 옮기지 않는다(grace·과반과 상관없이).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::Duration;

use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterError, Leases};
use opl_types::fence::NodeFenceSpec;
use opl_types::ha_check::{
    AUTO_PREFIX, CheckReport, CheckState, DEFAULT_GRACE_SECS, HaCheck, HaCheckAction, HaCheckRole,
    HaCheckSpec, HaCheckTarget,
};
use opl_types::node::{InterfaceKind, Node};
use opl_types::resource::Resource;
use opl_types::storage::is_shared;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Vm, VmHaStatus, VmPhase, VmSpec, VmStatus};
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::unix_now;
use crate::ha::{
    Ctx, Pick, PowerFencing, RELOCATE_WINDOW, TASK_FAIL_ATTEMPTS, create, pick, ready, runs_ha,
    update_status_checked,
};
use crate::placement::Assign;
use crate::scheduler::Inventory;
use crate::vms::{RuleCheck, start_migration};

/// 거절된 마이그레이션을 다시 해 보기까지 기다리는 시간
const MIGRATE_RETRY: Duration = Duration::from_secs(60);

/// 한 틱의 검사 상태: 켜진 `HaCheck`와 노드마다 마지막 리스 갱신에 실린 결과
pub(crate) struct Health<'a> {
    term: u64,
    checks: Vec<HaCheck>,
    reports: &'a BTreeMap<NodeId, Vec<CheckReport>>,
}

/// 검사 하나의 판단 규칙. 자동 관리 검사는 리소스가 없다(`spec` 없음).
struct Rule<'c> {
    action: HaCheckAction,
    grace: Duration,
    spec: Option<&'c HaCheckSpec>,
}

impl<'a> Health<'a> {
    pub(crate) fn load(cluster: &Cluster, leases: &'a Leases) -> Result<Self, ClusterError> {
        let (_, mut checks) = cluster.list::<HaCheckSpec>()?;
        checks.retain(|c| c.spec.enabled);
        Ok(Self {
            term: leases.term,
            checks,
            reports: &leases.checks,
        })
    }

    /// 노드 `node`가 마지막 갱신에 보낸 검사 `check`의 결과
    fn report(&self, node: Uuid, check: &str) -> Option<&CheckReport> {
        self.reports
            .get(&NodeId(node))?
            .iter()
            .find(|r| r.name == check)
    }

    fn rule(&self, name: &str) -> Option<Rule<'_>> {
        if let Some(c) = self.checks.iter().find(|c| c.name == name) {
            return Some(Rule {
                action: c.spec.action(),
                grace: Duration::from_secs(u64::from(c.spec.grace_secs)),
                spec: Some(&c.spec),
            });
        }
        name.starts_with(AUTO_PREFIX).then_some(Rule {
            action: HaCheckAction::Warn,
            grace: Duration::from_secs(u64::from(DEFAULT_GRACE_SECS)),
            spec: None,
        })
    }

    /// 노드 `node`로 VM `vm`을 옮기면 안 된다: 그 노드가 `failing`으로 보고한 켜진 `storage`·`vm` 검사 가운데 이
    /// VM에 영향을 주는 것이 있다. 보고가 없는 노드(옛 노드)와 `unknown`은 막지 않는다.
    pub(crate) fn blocks(&self, vm: &Vm, node: &Node, inv: &Inventory) -> bool {
        self.checks
            .iter()
            .filter(|c| matches!(c.spec.role, HaCheckRole::Storage | HaCheckRole::Vm))
            .filter(|c| {
                self.report(node.id, &c.name)
                    .is_some_and(|r| r.state == CheckState::Failing)
            })
            .any(|c| affects(&c.spec, vm, node, inv))
    }

    /// 살아 있는 노드(`alive`) 가운데 검사 `check`를 `ok`·`degraded`·`failing`으로 보고한 노드 수와 그 가운데
    /// `failing`인 수(`failing`, `reporting`). `unknown`은 세지 않는다.
    fn tally(&self, check: &str, alive: &BTreeSet<Uuid>) -> (usize, usize) {
        let mut failing = 0;
        let mut reporting = 0;
        for node in alive {
            match self.report(*node, check).map(|r| r.state) {
                Some(CheckState::Failing) => {
                    failing += 1;
                    reporting += 1;
                }
                Some(CheckState::Ok | CheckState::Degraded) => reporting += 1,
                Some(CheckState::Unknown) | None => {}
            }
        }
        (failing, reporting)
    }

    /// 보고한 노드의 과반이 실패한다(공유 장비 문제로 보고 경고만 한다). 보고한 노드가 하나면 늘 과반이다.
    fn shared(&self, check: &str, alive: &BTreeSet<Uuid>) -> bool {
        let (failing, reporting) = self.tally(check, alive);
        reporting > 0 && failing * 2 > reporting
    }
}

/// 검사가 노드 `node`에서 VM `vm`에 영향을 주는지(VM이 그 노드에 있는지는 보지 않는다).
/// - `storage` + `pool`: 디스크가 그 풀에 있다. `storage` + `link`·`ping`: 디스크가 공유 풀에 하나라도 있다.
/// - `vm` + `link`(`ping`의 `via`): NIC의 브리지가 그 인터페이스 뒤([`bridges_behind`])에 있다. `vm` + `via` 없는
///   `ping`: NIC가 하나라도 있다.
/// - `management`: 노드 전체(전원 끄기 대응의 기록용)
pub(crate) fn affects(check: &HaCheckSpec, vm: &Vm, node: &Node, inv: &Inventory) -> bool {
    let target = check.target_for(&node.name);
    match (check.role, target) {
        (HaCheckRole::Management, _) => true,
        (HaCheckRole::Storage, target) => vm.spec.disks.iter().any(|disk| {
            let Some(volume) = inv.volumes.iter().find(|v| v.name == disk.volume) else {
                return false;
            };
            match target {
                HaCheckTarget::Pool { name } => &volume.spec.pool == name,
                HaCheckTarget::Link { .. } | HaCheckTarget::Ping { .. } => inv
                    .pools
                    .iter()
                    .any(|p| p.name == volume.spec.pool && is_shared(p)),
            }
        }),
        (HaCheckRole::Vm, HaCheckTarget::Link { iface })
        | (
            HaCheckRole::Vm,
            HaCheckTarget::Ping {
                via: Some(iface), ..
            },
        ) => {
            let behind = bridges_behind(node, iface);
            vm.spec.nics.iter().any(|nic| {
                inv.networks
                    .iter()
                    .find(|n| n.name == nic.network)
                    .is_some_and(|n| {
                        if n.spec.vxlan.is_some() {
                            behind.vxlan
                        } else {
                            behind.bridges.contains(&n.spec.bridge_name())
                        }
                    })
            })
        }
        (HaCheckRole::Vm, HaCheckTarget::Ping { via: None, .. }) => !vm.spec.nics.is_empty(),
        (HaCheckRole::Vm, HaCheckTarget::Pool { .. }) => false,
    }
}

/// 노드 `node`에 자리 잡은(`spec.node`, 켜기로 됨) VM 가운데 검사가 영향을 주는 것(이름 순)
pub(crate) fn affected<'i>(check: &HaCheckSpec, node: &Node, inv: &'i Inventory) -> Vec<&'i Vm> {
    let mut vms: Vec<&Vm> = inv
        .vms
        .iter()
        .filter(|vm| {
            vm.spec.node == Some(node.id) && vm.spec.power == opl_types::vm::PowerState::Running
        })
        .filter(|vm| affects(check, vm, node, inv))
        .collect();
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    vms
}

/// 인터페이스 뒤의 브리지([`bridges_behind`])
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Behind {
    /// 그 인터페이스 자신(브리지면)과 `master`를 위로 따라간 브리지
    pub bridges: BTreeSet<String>,
    /// 그 인터페이스(또는 그 위)가 노드의 underlay IP를 가졌다: VXLAN 브리지(`oplv*`) 모두가 뒤에 있다.
    pub vxlan: bool,
}

/// 노드가 보고한 인터페이스(`status.interfaces`)로 `iface` 뒤의 브리지를 찾는다: `iface`부터 `master`를 따라
/// 올라가며(포트 → 본딩 → 브리지) 브리지를 모은다. 그 사슬의 인터페이스가 노드의 underlay IP를 가졌으면 VXLAN
/// 네트워크도 그 인터페이스를 지난다.
pub(crate) fn bridges_behind(node: &Node, iface: &str) -> Behind {
    let interfaces = &node.status.interfaces;
    let mut behind = Behind::default();
    let mut seen = BTreeSet::new();
    let mut current = Some(iface.to_string());
    while let Some(name) = current.take() {
        if !seen.insert(name.clone()) {
            break;
        }
        let Some(i) = interfaces.iter().find(|i| i.name == name) else {
            break;
        };
        if i.kind == InterfaceKind::Bridge {
            behind.bridges.insert(i.name.clone());
        }
        if let Some(underlay) = node.status.underlay
            && i.addresses.iter().any(|a| has_address(a, underlay))
        {
            behind.vxlan = true;
        }
        current = i.master.clone();
    }
    behind
}

/// CIDR 문자열(`10.0.0.5/24`)이 주소 `ip`다.
fn has_address(cidr: &str, ip: IpAddr) -> bool {
    let addr = cidr.split('/').next().unwrap_or(cidr);
    addr.parse::<IpAddr>().is_ok_and(|a| a == ip)
}

/// 노드 하나에서 검사 하나가 실패하는 동안의 기록(리더 메모리)
#[derive(Debug, Clone, PartialEq, Eq)]
struct Episode {
    /// 이 리더가 실패를 처음 본 때(grace의 기준)
    since: Instant,
    /// 이 실패에 대응했다(경고·전원 끄기는 한 번, `migrate`는 첫 기록 Task).
    acted: bool,
    /// 옮길 노드가 없다고 경고한 VM(한 번만 경고한다)
    no_target: BTreeSet<Uuid>,
}

impl Episode {
    fn new(since: Instant) -> Self {
        Self {
            since,
            acted: false,
            no_target: BTreeSet::new(),
        }
    }
}

/// 리더의 검사 상태. 리더 자리를 잃거나 임기가 바뀌면 비운다.
#[derive(Debug, Default)]
pub(crate) struct CheckWatch {
    term: u64,
    /// 이번 틱의 살아 있는 노드
    alive: BTreeSet<Uuid>,
    /// (노드, 검사) → 실패 기록
    failing: BTreeMap<(Uuid, String), Episode>,
    /// 공유 실패로 기록한 검사(과반 실패가 이어지는 동안 기록 Task를 다시 쓰지 않는다)
    shared: BTreeSet<String>,
    /// 원본 노드 → 이 리더가 시작한 마이그레이션 Task → VM
    migrating: BTreeMap<Uuid, BTreeMap<Uuid, Uuid>>,
    /// 마이그레이션이 거절된 VM과 마지막으로 거절된 때
    refused: BTreeMap<Uuid, Instant>,
    /// 기능이 모자라 대응하지 않는다고 이 임기에 경고했다.
    old_warned: bool,
}

impl CheckWatch {
    /// 리더가 아니다.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// 이번 틱의 결과를 본다: 살아 있는 노드(`alive`)가 `failing`으로 보고한 검사는 실패 기록을 (없으면) 지금으로
    /// 시작하고, 아니면(회복, 보고가 사라짐, 노드가 죽음) 지운다. 과반 실패가 끝난 검사는 남은 실패의 grace를
    /// 처음부터 다시 잰다(공유 장비가 돌아오는 동안 늦게 회복한 노드에 대응하지 않게).
    pub(crate) fn observe(&mut self, health: &Health<'_>, alive: BTreeSet<Uuid>, now: Instant) {
        if health.term != self.term {
            *self = Self {
                term: health.term,
                ..Self::default()
            };
        }
        self.alive = alive;
        let mut seen = BTreeSet::new();
        for (node, reports) in health.reports {
            if !self.alive.contains(&node.0) {
                continue;
            }
            for r in reports.iter().filter(|r| r.state == CheckState::Failing) {
                let key = (node.0, r.name.clone());
                self.failing.entry(key.clone()).or_insert_with(|| {
                    tracing::info!(node = %node.0, check = %r.name, error = ?r.error, "HA check is failing");
                    Episode::new(now)
                });
                seen.insert(key);
            }
        }
        self.failing.retain(|(node, check), _| {
            let keep = seen.contains(&(*node, check.clone()));
            if !keep {
                tracing::info!(node = %node, check = %check, "HA check is no longer failing");
            }
            keep
        });
        let ended: Vec<String> = self
            .shared
            .iter()
            .filter(|c| !health.shared(c, &self.alive))
            .cloned()
            .collect();
        for check in ended {
            tracing::info!(check = %check, "HA check is no longer failing on most nodes");
            self.shared.remove(&check);
            for ((_, c), episode) in &mut self.failing {
                if *c == check {
                    *episode = Episode::new(now);
                }
            }
        }
    }

    /// 이 리더가 시작한 마이그레이션이 끝났거나 없어졌으면 뺀다.
    fn prune(&mut self, cluster: &Cluster) {
        for tasks in self.migrating.values_mut() {
            tasks.retain(|task, _| {
                matches!(cluster.get::<TaskSpec>(*task), Ok(Some(t)) if !t.status.state.is_finished())
            });
        }
        self.migrating.retain(|_, tasks| !tasks.is_empty());
    }
}

/// [`decide`]의 판단
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// 경고만(경고 로그 + 기록 Task)
    Warn {
        node: Uuid,
        check: String,
        detail: String,
    },
    /// 보고한 `reporting`개 노드 가운데 `failing`개가 실패한다: 공유 실패로 보고 경고만 한다. `node`는 기록
    /// Task의 대상(실패한 노드 하나)이다.
    Shared {
        node: Uuid,
        check: String,
        failing: usize,
        reporting: usize,
    },
    /// 영향받는 HA VM(`vms`)을 라이브 마이그레이션한다. `unprotected`는 영향받는 HA 아닌 VM 수다.
    Migrate {
        node: Uuid,
        check: String,
        vms: Vec<Uuid>,
        unprotected: usize,
        detail: String,
    },
    /// 노드 전원을 끈다.
    Fence {
        node: Uuid,
        check: String,
        detail: String,
    },
}

/// grace가 지난 실패에 할 일. 경고·전원 끄기는 실패마다 한 번, `migrate`는 옮길 VM이 남은 동안 틱마다 낸다. 과반
/// 실패면 검사마다 한 번 [`Decision::Shared`]만 낸다. 모든 노드가 `ha-checks`를 알지 못하면(`everywhere`가 아님)
/// 모두 경고다. 이 계획의 다음 단계 전까지 `restart_elsewhere`는 경고다.
pub(crate) fn decide(
    health: &Health<'_>,
    inv: &Inventory,
    watch: &CheckWatch,
    now: Instant,
    everywhere: bool,
) -> Vec<Decision> {
    let mut out = vec![];
    let mut shared = BTreeSet::new();
    for ((node, check), episode) in &watch.failing {
        let Some(rule) = health.rule(check) else {
            continue;
        };
        if now.saturating_duration_since(episode.since) < rule.grace {
            continue;
        }
        if health.shared(check, &watch.alive) {
            if !watch.shared.contains(check) && shared.insert(check.clone()) {
                let (failing, reporting) = health.tally(check, &watch.alive);
                out.push(Decision::Shared {
                    node: *node,
                    check: check.clone(),
                    failing,
                    reporting,
                });
            }
            continue;
        }
        let detail = health
            .report(*node, check)
            .and_then(|r| r.error.clone())
            .unwrap_or_else(|| "failing".into());
        let action = match rule.action {
            _ if !everywhere => HaCheckAction::Warn,
            HaCheckAction::RestartElsewhere => HaCheckAction::Warn,
            action => action,
        };
        match (action, rule.spec) {
            (HaCheckAction::Migrate, Some(spec)) => {
                let Some(n) = inv.nodes.iter().find(|n| n.id == *node) else {
                    continue;
                };
                let (ha, other): (Vec<&Vm>, Vec<&Vm>) = affected(spec, n, inv)
                    .into_iter()
                    .partition(|vm| vm.spec.ha_active());
                out.push(Decision::Migrate {
                    node: *node,
                    check: check.clone(),
                    vms: ha.iter().map(|vm| vm.id).collect(),
                    unprotected: other.len(),
                    detail,
                });
            }
            _ if episode.acted => {}
            (HaCheckAction::Fence, _) => out.push(Decision::Fence {
                node: *node,
                check: check.clone(),
                detail,
            }),
            _ => out.push(Decision::Warn {
                node: *node,
                check: check.clone(),
                detail,
            }),
        }
    }
    out
}

/// 리스·대상 고르기에 쓰는 것
pub(crate) struct Place<'a> {
    pub inv: &'a Inventory,
    pub leases: &'a Leases,
    pub now: Instant,
    pub b: Instant,
}

/// [`decide`]의 판단대로 한다.
pub(crate) async fn react(
    ctx: Ctx<'_>,
    decisions: Vec<Decision>,
    watch: &mut CheckWatch,
    power: &mut PowerFencing,
    health: &Health<'_>,
    place: Place<'_>,
    everywhere: bool,
) {
    watch.prune(ctx.cluster);
    if !everywhere && !decisions.is_empty() && !watch.old_warned {
        tracing::warn!(
            "some nodes do not support HA checks yet; failing HA checks only warn until every node is upgraded"
        );
        watch.old_warned = true;
    }
    for decision in decisions {
        match decision {
            Decision::Shared {
                node,
                check,
                failing,
                reporting,
            } => {
                let detail = format!(
                    "failing on {failing} of {reporting} nodes; treating it as a shared failure"
                );
                tracing::warn!(check = %check, failing, reporting, "HA check fails on most nodes; only warning");
                record_check(ctx, node, &check, HaCheckAction::Warn, vec![], detail, true).await;
                watch.shared.insert(check);
            }
            Decision::Warn {
                node,
                check,
                detail,
            } => {
                tracing::warn!(node = %node_name(place.inv, node), check = %check, detail = %detail, "HA check failed");
                record_check(ctx, node, &check, HaCheckAction::Warn, vec![], detail, true).await;
                watch.acted(node, &check);
            }
            Decision::Fence {
                node,
                check,
                detail,
            } => fence(ctx, watch, power, place.inv, node, check, detail).await,
            Decision::Migrate {
                node,
                check,
                vms,
                unprotected,
                detail,
            } => {
                let detail = if unprotected > 0 {
                    format!("{detail}; also affects {unprotected} VMs without HA")
                } else {
                    detail
                };
                migrate(ctx, watch, health, &place, node, check, vms, detail).await;
            }
        }
    }
}

impl CheckWatch {
    fn acted(&mut self, node: Uuid, check: &str) {
        if let Some(e) = self.failing.get_mut(&(node, check.to_string())) {
            e.acted = true;
        }
    }

    /// 거절된 마이그레이션을 아직 다시 하지 않을 VM
    fn waiting(&self, vm: Uuid, now: Instant) -> bool {
        self.refused
            .get(&vm)
            .is_some_and(|at| now.saturating_duration_since(*at) < MIGRATE_RETRY)
    }
}

fn node_name(inv: &Inventory, node: Uuid) -> String {
    inv.nodes
        .iter()
        .find(|n| n.id == node)
        .map_or_else(|| node.to_string(), |n| n.name.clone())
}

/// 노드 전원 끄기: `NodeFence`가 있으면 전원 끄기 펜싱에 요청한다(다음 틱에 끄고, 꺼짐을 확인하면 노드 장애처럼
/// 모든 HA VM을 옮긴다). 없으면 경고와 실패 기록 Task.
async fn fence(
    ctx: Ctx<'_>,
    watch: &mut CheckWatch,
    power: &mut PowerFencing,
    inv: &Inventory,
    node: Uuid,
    check: String,
    detail: String,
) {
    let name = node_name(inv, node);
    let mut vms: Vec<String> = inv
        .vms
        .iter()
        .filter(|vm| runs_ha(vm, node))
        .map(|vm| vm.name.clone())
        .collect();
    vms.sort();
    let (detail, ok) = match ctx.cluster.get::<NodeFenceSpec>(node) {
        Ok(Some(_)) if node == ctx.cluster.id().0 => {
            tracing::warn!(node = %name, check = %check, "HA check failed on the leader; it cannot power itself off");
            (
                format!("{detail}; the node is the leader and cannot power itself off"),
                false,
            )
        }
        Ok(Some(_)) => {
            tracing::warn!(node = %name, check = %check, detail = %detail, "HA check failed; requesting a power-off of the node");
            power.request(node, format!("HA check {check} failed"));
            (format!("{detail}; requested a power-off of the node"), true)
        }
        Ok(None) => {
            tracing::warn!(node = %name, check = %check, "HA check failed but the node has no power fencing");
            (format!("{detail}; no power fencing configured"), false)
        }
        Err(e) => {
            tracing::warn!(node = %name, error = %e, "cannot read power fencing settings; will retry");
            return;
        }
    };
    record_check(ctx, node, &check, HaCheckAction::Fence, vms, detail, ok).await;
    watch.acted(node, &check);
}

/// 영향받는 HA VM을 이름 순으로 라이브 마이그레이션한다. 원본 노드마다 진행 중 Task는 `ha_start_concurrency`개까지
/// (나머지는 다음 틱), 옮기는 중이거나 실행 중이 아닌 VM은 건너뛴다. 창 안에서 이미 `max_relocate`번 옮긴 VM은
/// 옮기지 않고 `ha.error`를 남긴다. 대상은 그 노드가 아니고 리스가 살아 있으며 같은 검사가 막지 않는 노드다.
/// 시작하면 옮긴 기록(`relocations`, `relocated_from`)을 더한다. 첫 대응과 VM을 옮기기 시작한 틱에 기록 Task를 쓴다.
#[allow(clippy::too_many_arguments)]
async fn migrate(
    ctx: Ctx<'_>,
    watch: &mut CheckWatch,
    health: &Health<'_>,
    place: &Place<'_>,
    node: Uuid,
    check: String,
    vms: Vec<Uuid>,
    detail: String,
) {
    let inv = place.inv;
    let limit = match crate::settings::current(ctx.cluster) {
        Ok(s) => s.spec.ha_start_concurrency(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read cluster settings; will retry");
            return;
        }
    };
    let in_flight: BTreeSet<Uuid> = watch
        .migrating
        .values()
        .flat_map(|tasks| tasks.values().copied())
        .collect();
    let mut assign = Assign::default();
    let mut started = vec![];
    let mut no_target = vec![];
    for vm in vms
        .iter()
        .filter_map(|id| inv.vms.iter().find(|vm| vm.id == *id))
    {
        if vm.status.phase != VmPhase::Running
            || vm.status.migration.is_some()
            || in_flight.contains(&vm.id)
            || watch.waiting(vm.id, place.now)
        {
            continue;
        }
        let policy = vm.spec.ha.clone().unwrap_or_default();
        let unix = unix_now();
        let mut ha = vm.status.ha.clone().unwrap_or_default();
        ha.relocations
            .retain(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix);
        if ha.relocations.len() >= usize::from(policy.max_relocate) {
            let error = format!(
                "HA check {check} failed; not moving the VM: gave up after {} relocations",
                ha.relocations.len()
            );
            if vm.status.ha.as_ref().and_then(|h| h.error.as_ref()) != Some(&error) {
                tracing::warn!(vm = %vm.name, check = %check, error = %error, "not moving HA VM");
                let status = VmStatus {
                    ha: Some(VmHaStatus {
                        error: Some(error),
                        ..vm.status.ha.clone().unwrap_or_default()
                    }),
                    ..vm.status.clone()
                };
                if let Err(e) =
                    update_status_checked::<VmSpec>(ctx, vm.id, vm.revision, &status).await
                {
                    tracing::info!(vm = %vm.name, error = %e, "cannot record HA error; will retry");
                }
            }
            continue;
        }
        if watch.migrating.get(&node).map_or(0, BTreeMap::len) >= limit {
            continue;
        }
        let eligible = |n: &Node| {
            n.id != node && ready(place.now, place.b, place.leases, n) && !health.blocks(vm, n, inv)
        };
        let target = match pick(vm, inv, &assign, eligible) {
            Pick::Node(t) | Pick::Violating(t, _) => t,
            Pick::NoNode | Pick::Blocked(_) => {
                no_target.push(vm);
                continue;
            }
        };
        match start_migration(
            ctx.state,
            vm.id,
            target,
            None,
            vm.spec.node,
            RuleCheck::Enforce,
        )
        .await
        {
            Ok((task, _)) => {
                tracing::warn!(vm = %vm.name, check = %check, to = %node_name(inv, target), task = %task.id, "HA check failed; migrating the VM");
                watch
                    .migrating
                    .entry(node)
                    .or_default()
                    .insert(task.id, vm.id);
                watch.refused.remove(&vm.id);
                assign.place(vm.id, target);
                started.push(vm.name.clone());
                count_relocation(ctx, vm, node, unix).await;
            }
            Err(e) => {
                if watch.refused.insert(vm.id, place.now).is_none() {
                    tracing::info!(vm = %vm.name, node = %target, error = %e.message, "HA check migration refused");
                }
            }
        }
    }
    let Some(episode) = watch.failing.get_mut(&(node, check.clone())) else {
        return;
    };
    for vm in no_target {
        if episode.no_target.insert(vm.id) {
            tracing::warn!(vm = %vm.name, check = %check, "HA check failed but no node can take the VM");
        }
    }
    if episode.acted && started.is_empty() {
        return;
    }
    episode.acted = true;
    record_check(
        ctx,
        node,
        &check,
        HaCheckAction::Migrate,
        started,
        detail,
        true,
    )
    .await;
}

/// 검사 실패로 옮긴 VM의 기록: 창 밖 기록을 버리고 지금 시각을 더하고 떠난 노드를 `relocated_from`에 둔다(되돌아가기가
/// 창 안에서 그 노드로 되돌리지 않게). 다른 쓰기와 엇갈리면 다시 읽어 쓴다.
async fn count_relocation(ctx: Ctx<'_>, vm: &Vm, from: Uuid, unix: u64) {
    for _ in 0..TASK_FAIL_ATTEMPTS {
        let Ok(Some(current)) = ctx.cluster.get::<VmSpec>(vm.id) else {
            return;
        };
        let mut ha = current.status.ha.clone().unwrap_or_default();
        ha.relocations
            .retain(|at| at.saturating_add(RELOCATE_WINDOW.as_secs()) > unix);
        ha.relocations.push(unix);
        ha.relocated_from = Some(from);
        let status = VmStatus {
            ha: Some(ha),
            ..current.status.clone()
        };
        match update_status_checked::<VmSpec>(ctx, vm.id, current.revision, &status).await {
            Ok(_) => return,
            Err(e) => {
                tracing::info!(vm = %vm.name, error = %e, "cannot count the HA check migration as a relocation");
            }
        }
    }
}

/// 검사 대응의 기록 Task(`ha-check`, 대상 노드, 끝난 상태)를 남긴다. 옛 노드가 이 Task를 풀지 못하므로 모든 노드가
/// `ha-checks`일 때만 쓴다. 실패(`ok`가 아님)면 `detail`이 오류다.
pub(crate) async fn record_check(
    ctx: Ctx<'_>,
    node: Uuid,
    check: &str,
    action: HaCheckAction,
    vms: Vec<String>,
    detail: String,
    ok: bool,
) {
    match crate::cluster::missing_feature(ctx.cluster, opl_types::node::HA_CHECKS) {
        Ok(old) if old.is_empty() => {}
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(node = %node, error = %e, "cannot record the HA check task");
            return;
        }
    }
    let (state, error) = if ok {
        (TaskState::Succeeded, None)
    } else {
        (TaskState::Failed, Some(detail.clone()))
    };
    let mut record: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::HaCheck {
                check: check.to_string(),
                action,
                vms,
                detail,
            },
            target: node,
            node,
        },
    );
    record.name = record.id.to_string();
    // 실행기는 끝난 Task를 실행하지 않는다.
    record.status = TaskStatus {
        state,
        error,
        progress: None,
    };
    if let Err(e) = create(ctx, &record).await {
        tracing::warn!(node = %node, error = %e, "cannot record the HA check task");
    }
}

#[cfg(test)]
mod tests {
    use opl_types::network::{NetworkSpec, Vxlan};
    use opl_types::node::{NodeInterface, NodeSpec, NodeStatus};
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
    use opl_types::vm::{
        CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec, PowerState, VmHa,
    };

    use super::*;

    fn iface(
        name: &str,
        kind: InterfaceKind,
        master: Option<&str>,
        addresses: &[&str],
    ) -> NodeInterface {
        NodeInterface {
            name: name.into(),
            kind,
            state: "up".into(),
            carrier: Some(true),
            speed_mbps: None,
            mac: None,
            master: master.map(Into::into),
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            bond: None,
        }
    }

    fn node(name: &str, interfaces: Vec<NodeInterface>, underlay: Option<&str>) -> Node {
        let mut n = Resource::new(
            name,
            NodeSpec {
                address: "a".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        n.status = NodeStatus {
            online: true,
            interfaces,
            underlay: underlay.map(|u| u.parse().unwrap()),
            ..Default::default()
        };
        n
    }

    fn vm(name: &str, node: &Node, disks: &[&str], nics: &[&str], ha: bool) -> Vm {
        Resource::new(
            name,
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 1024,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: disks
                    .iter()
                    .map(|d| DiskSpec {
                        volume: d.to_string(),
                        bus: DiskBus::VirtioBlk,
                        boot_order: None,
                    })
                    .collect(),
                nics: nics
                    .iter()
                    .map(|n| NicSpec {
                        network: n.to_string(),
                        model: NicModel::Virtio,
                        mac: None,
                        security_groups: vec![],
                    })
                    .collect(),
                power: PowerState::Running,
                restart: Default::default(),
                node: Some(node.id),
                snapshot_schedule: None,
                ha: ha.then(VmHa::default),
            },
        )
    }

    fn pool(name: &str, backend: PoolBackend, shared: bool) -> opl_types::storage::StoragePool {
        Resource::new(
            name,
            StoragePoolSpec {
                backend,
                nodes: vec![],
                shared,
                backups: false,
            },
        )
    }

    fn dir(path: &str) -> PoolBackend {
        PoolBackend::Dir { path: path.into() }
    }

    fn volume(name: &str, pool: &str) -> opl_types::storage::Volume {
        Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
            },
        )
    }

    fn bridge_net(name: &str, bridge: &str) -> opl_types::network::Network {
        Resource::new(
            name,
            NetworkSpec {
                bridge: Some(bridge.into()),
                vlan: None,
                vxlan: None,
                mtu: None,
            },
        )
    }

    fn vxlan_net(name: &str, vni: u32) -> opl_types::network::Network {
        Resource::new(
            name,
            NetworkSpec {
                bridge: None,
                vlan: None,
                vxlan: Some(Vxlan { vni }),
                mtu: None,
            },
        )
    }

    fn check(role: HaCheckRole, target: HaCheckTarget) -> HaCheckSpec {
        HaCheckSpec {
            role,
            target,
            on_fail: None,
            grace_secs: 10,
            nodes: BTreeMap::new(),
            enabled: true,
        }
    }

    fn link(iface: &str) -> HaCheckTarget {
        HaCheckTarget::Link {
            iface: iface.into(),
        }
    }

    fn ping(via: Option<&str>) -> HaCheckTarget {
        HaCheckTarget::Ping {
            addresses: vec!["10.0.0.1".parse().unwrap()],
            via: via.map(Into::into),
            need: 1,
        }
    }

    fn names(vms: Vec<&Vm>) -> Vec<&str> {
        vms.iter().map(|vm| vm.name.as_str()).collect()
    }

    #[test]
    fn storage_checks_affect_vms_with_disks_on_the_pool_or_on_shared_pools() {
        let n = node("n1", vec![], None);
        let other = node("n2", vec![], None);
        let mut inv = Inventory {
            nodes: vec![n.clone(), other.clone()],
            pools: vec![
                pool(
                    "nas",
                    PoolBackend::Nfs {
                        server: "s".into(),
                        export: "/e".into(),
                        options: None,
                    },
                    false,
                ),
                pool("shared-dir", dir("/srv"), true),
                pool("local", dir("/var"), false),
            ],
            volumes: vec![
                volume("d-nas", "nas"),
                volume("d-shared", "shared-dir"),
                volume("d-local", "local"),
            ],
            ..Default::default()
        };
        inv.vms = vec![
            vm("on-nas", &n, &["d-local", "d-nas"], &[], true),
            vm("on-shared", &n, &["d-shared"], &[], true),
            vm("local-only", &n, &["d-local"], &[], true),
            vm("no-disk", &n, &[], &[], true),
            // 다른 노드의 VM과 꺼진 VM은 이 노드의 영향 목록에 없다.
            vm("elsewhere", &other, &["d-nas"], &[], true),
        ];
        let mut off = vm("off", &n, &["d-nas"], &[], true);
        off.spec.power = PowerState::Stopped;
        inv.vms.push(off);
        let by_pool = check(
            HaCheckRole::Storage,
            HaCheckTarget::Pool { name: "nas".into() },
        );
        assert_eq!(names(affected(&by_pool, &n, &inv)), ["on-nas"]);
        // 링크·핑 검사는 공유 풀(NFS, shared dir)에 디스크가 있는 VM
        for target in [link("eth1"), ping(None)] {
            let c = check(HaCheckRole::Storage, target);
            assert_eq!(names(affected(&c, &n, &inv)), ["on-nas", "on-shared"]);
        }
        // affects는 자리를 보지 않는다(대상 거르기).
        assert!(affects(&by_pool, &inv.vms[4], &n, &inv));
    }

    #[test]
    fn vm_checks_follow_masters_to_bridges_and_the_underlay_to_vxlan() {
        // eno1·eno2 → bond0 → vmbr0(underlay IP), eno3 → vmbr1, eno4는 브리지 없음
        let n = node(
            "n1",
            vec![
                iface("eno1", InterfaceKind::Ethernet, Some("bond0"), &[]),
                iface("eno2", InterfaceKind::Ethernet, Some("bond0"), &[]),
                iface("bond0", InterfaceKind::Bond, Some("vmbr0"), &[]),
                iface("vmbr0", InterfaceKind::Bridge, None, &["10.0.0.5/24"]),
                iface("eno3", InterfaceKind::Ethernet, Some("vmbr1"), &[]),
                iface("vmbr1", InterfaceKind::Bridge, None, &["192.168.1.5/24"]),
                iface("eno4", InterfaceKind::Ethernet, None, &["172.16.0.5/24"]),
            ],
            Some("10.0.0.5"),
        );
        assert_eq!(
            bridges_behind(&n, "eno1"),
            Behind {
                bridges: BTreeSet::from(["vmbr0".to_string()]),
                vxlan: true,
            }
        );
        assert_eq!(
            bridges_behind(&n, "eno3"),
            Behind {
                bridges: BTreeSet::from(["vmbr1".to_string()]),
                vxlan: false,
            }
        );
        assert_eq!(bridges_behind(&n, "eno4"), Behind::default());
        assert_eq!(bridges_behind(&n, "missing"), Behind::default());
        let inv = Inventory {
            nodes: vec![n.clone()],
            networks: vec![
                bridge_net("lan0", "vmbr0"),
                bridge_net("lan1", "vmbr1"),
                vxlan_net("overlay", 100),
            ],
            vms: vec![
                vm("a-lan0", &n, &[], &["lan0"], true),
                vm("b-lan1", &n, &[], &["user", "lan1"], true),
                vm("c-overlay", &n, &[], &["overlay"], true),
                vm("d-user", &n, &[], &["user"], true),
                vm("e-none", &n, &[], &[], true),
            ],
            ..Default::default()
        };
        let on = |target| names(affected(&check(HaCheckRole::Vm, target), &n, &inv));
        // 본딩 포트·본딩·브리지 어디를 보든 그 위의 브리지와 underlay를 지나는 VXLAN
        for i in ["eno2", "bond0", "vmbr0"] {
            assert_eq!(on(link(i)), ["a-lan0", "c-overlay"], "{i}");
        }
        assert_eq!(on(link("eno3")), ["b-lan1"]);
        assert!(on(link("eno4")).is_empty());
        // 핑은 `via`가 있으면 링크처럼, 없으면 NIC가 있는 모든 VM
        assert_eq!(on(ping(Some("eno3"))), ["b-lan1"]);
        assert_eq!(on(ping(None)), ["a-lan0", "b-lan1", "c-overlay", "d-user"]);
        // underlay가 없는 노드는 VXLAN을 어느 인터페이스에도 묶지 않는다.
        let mut plain = n.clone();
        plain.status.underlay = None;
        assert!(!bridges_behind(&plain, "eno1").vxlan);
    }

    #[test]
    fn a_node_override_target_is_used_for_that_node() {
        let n = node(
            "n1",
            vec![
                iface("eth9", InterfaceKind::Ethernet, Some("vmbr9"), &[]),
                iface("vmbr9", InterfaceKind::Bridge, None, &[]),
            ],
            None,
        );
        let inv = Inventory {
            nodes: vec![n.clone()],
            networks: vec![bridge_net("lan9", "vmbr9")],
            vms: vec![vm("a", &n, &[], &["lan9"], true)],
            ..Default::default()
        };
        let mut c = check(HaCheckRole::Vm, link("eth0"));
        assert!(affected(&c, &n, &inv).is_empty());
        c.nodes.insert("n1".into(), link("eth9"));
        assert_eq!(names(affected(&c, &n, &inv)), ["a"]);
    }

    fn report(name: &str, state: CheckState) -> CheckReport {
        CheckReport::new(name, state, Some(format!("{name} is {state:?}")), None)
    }

    fn resource(name: &str, spec: HaCheckSpec) -> HaCheck {
        Resource::new(name, spec)
    }

    /// 노드마다 검사 `c`의 상태로 [`Health`]의 보고를 만든다.
    fn reports(states: &[(Uuid, CheckState)]) -> BTreeMap<NodeId, Vec<CheckReport>> {
        states
            .iter()
            .map(|(n, s)| (NodeId(*n), vec![report("c", *s)]))
            .collect()
    }

    /// 경고 검사 `c`(grace 10초)가 `states`대로 보고될 때 grace가 지난 뒤의 판단
    fn decided(states: &[(Uuid, CheckState)]) -> Vec<Decision> {
        let reports = reports(states);
        let mut spec = check(HaCheckRole::Management, link("eth0"));
        spec.on_fail = Some(HaCheckAction::Warn);
        let health = Health {
            term: 1,
            checks: vec![resource("c", spec)],
            reports: &reports,
        };
        let t0 = Instant::now();
        let mut watch = CheckWatch::default();
        watch.observe(&health, states.iter().map(|(n, _)| *n).collect(), t0);
        let inv = Inventory::default();
        assert!(decide(&health, &inv, &watch, t0 + Duration::from_secs(9), true).is_empty());
        decide(&health, &inv, &watch, t0 + Duration::from_secs(10), true)
    }

    #[test]
    fn a_failure_on_most_reporting_nodes_is_shared_and_unknown_does_not_count() {
        use CheckState::{Degraded, Failing, Ok, Unknown};
        let mut ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
        ids.sort();
        let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
        let warn = |node| Decision::Warn {
            node,
            check: "c".into(),
            detail: "c is Failing".into(),
        };
        let shared = |node, failing, reporting| Decision::Shared {
            node,
            check: "c".into(),
            failing,
            reporting,
        };
        // 두 노드 가운데 하나: 노드 문제
        assert_eq!(decided(&[(a, Failing), (b, Ok)]), [warn(a)]);
        // degraded는 통과다.
        assert_eq!(decided(&[(a, Failing), (b, Degraded)]), [warn(a)]);
        // 세 노드 가운데 둘: 공유 실패(한 번)
        assert_eq!(
            decided(&[(a, Failing), (b, Failing), (c, Ok)]),
            [shared(a, 2, 3)]
        );
        // 보고한 노드가 하나면 늘 과반이다.
        assert_eq!(decided(&[(a, Failing)]), [shared(a, 1, 1)]);
        // unknown은 세지 않는다: 1/1과 1/2
        assert_eq!(decided(&[(a, Failing), (b, Unknown)]), [shared(a, 1, 1)]);
        assert_eq!(decided(&[(a, Failing), (b, Ok), (c, Unknown)]), [warn(a)]);
        // 네 노드 가운데 둘은 과반이 아니다.
        assert_eq!(
            decided(&[(a, Failing), (b, Failing), (c, Ok), (d, Ok)]),
            [warn(a), warn(b)]
        );
    }

    #[test]
    fn grace_restarts_on_a_new_term_and_after_recovery() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let mut spec = check(HaCheckRole::Management, link("eth0"));
        spec.on_fail = Some(HaCheckAction::Fence);
        let failing = reports(&[(a, CheckState::Failing), (b, CheckState::Ok)]);
        let passing = reports(&[(a, CheckState::Ok), (b, CheckState::Ok)]);
        let health = |term, reports| Health {
            term,
            checks: vec![resource("c", spec.clone())],
            reports,
        };
        let alive = BTreeSet::from([a, b]);
        let inv = Inventory::default();
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut watch = CheckWatch::default();
        watch.observe(&health(1, &failing), alive.clone(), t0);
        let fence = vec![Decision::Fence {
            node: a,
            check: "c".into(),
            detail: "c is Failing".into(),
        }];
        assert_eq!(
            decide(&health(1, &failing), &inv, &watch, t0 + s(10), true),
            fence
        );
        // 대응한 실패에는 다시 내지 않는다.
        watch.acted(a, "c");
        watch.observe(&health(1, &failing), alive.clone(), t0 + s(11));
        assert!(decide(&health(1, &failing), &inv, &watch, t0 + s(11), true).is_empty());
        // 임기가 바뀌면(새 리더) 처음부터 다시 잰다.
        watch.observe(&health(2, &failing), alive.clone(), t0 + s(12));
        assert!(decide(&health(2, &failing), &inv, &watch, t0 + s(21), true).is_empty());
        assert_eq!(
            decide(&health(2, &failing), &inv, &watch, t0 + s(22), true),
            fence
        );
        // 회복하면 지우고, 다시 실패하면 새 실패다.
        watch.observe(&health(2, &passing), alive.clone(), t0 + s(23));
        assert!(watch.failing.is_empty());
        watch.observe(&health(2, &failing), alive.clone(), t0 + s(24));
        assert_eq!(
            decide(&health(2, &failing), &inv, &watch, t0 + s(34), true),
            fence
        );
        // 리스가 끝난 노드(살아 있지 않음)의 보고는 보지 않는다.
        watch.observe(&health(2, &failing), BTreeSet::from([b]), t0 + s(35));
        assert!(watch.failing.is_empty());
        // 기능이 모자라면 경고다.
        watch.observe(&health(2, &failing), alive, t0 + s(36));
        assert_eq!(
            decide(&health(2, &failing), &inv, &watch, t0 + s(46), false),
            [Decision::Warn {
                node: a,
                check: "c".into(),
                detail: "c is Failing".into(),
            }]
        );
    }

    #[test]
    fn a_shared_failure_that_ends_restarts_the_grace_of_the_rest() {
        let (a, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let mut spec = check(HaCheckRole::Management, link("eth0"));
        spec.on_fail = Some(HaCheckAction::Fence);
        let both = reports(&[
            (a, CheckState::Failing),
            (b, CheckState::Failing),
            (c, CheckState::Ok),
        ]);
        let one = reports(&[
            (a, CheckState::Failing),
            (b, CheckState::Ok),
            (c, CheckState::Ok),
        ]);
        let health = |reports| Health {
            term: 1,
            checks: vec![resource("c", spec.clone())],
            reports,
        };
        let alive = BTreeSet::from([a, b, c]);
        let inv = Inventory::default();
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut watch = CheckWatch::default();
        watch.observe(&health(&both), alive.clone(), t0);
        let decisions = decide(&health(&both), &inv, &watch, t0 + s(10), true);
        assert!(matches!(decisions[..], [Decision::Shared { .. }]));
        watch.shared.insert("c".into());
        assert!(decide(&health(&both), &inv, &watch, t0 + s(11), true).is_empty());
        // b가 회복해 1/3: a는 grace를 다시 기다린 뒤에 대응한다.
        watch.observe(&health(&one), alive, t0 + s(12));
        assert!(watch.shared.is_empty());
        assert!(decide(&health(&one), &inv, &watch, t0 + s(21), true).is_empty());
        assert!(matches!(
            decide(&health(&one), &inv, &watch, t0 + s(22), true)[..],
            [Decision::Fence { node, .. }] if node == a
        ));
    }

    #[test]
    fn auto_checks_only_warn_and_unknown_checks_are_ignored() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let reports = BTreeMap::from([
            (
                NodeId(a),
                vec![
                    report("cluster-link-1", CheckState::Failing),
                    report("deleted", CheckState::Failing),
                ],
            ),
            (
                NodeId(b),
                vec![
                    report("cluster-link-1", CheckState::Ok),
                    report("deleted", CheckState::Ok),
                ],
            ),
        ]);
        let health = Health {
            term: 1,
            checks: vec![],
            reports: &reports,
        };
        let t0 = Instant::now();
        let mut watch = CheckWatch::default();
        watch.observe(&health, BTreeSet::from([a, b]), t0);
        let inv = Inventory::default();
        assert!(decide(&health, &inv, &watch, t0 + Duration::from_secs(9), true).is_empty());
        assert_eq!(
            decide(&health, &inv, &watch, t0 + Duration::from_secs(10), true),
            [Decision::Warn {
                node: a,
                check: "cluster-link-1".into(),
                detail: "cluster-link-1 is Failing".into(),
            }]
        );
    }

    #[test]
    fn only_failing_storage_and_vm_checks_that_affect_the_vm_block_a_node() {
        let n = node("n1", vec![], None);
        let quiet = node("n2", vec![], None);
        let inv = Inventory {
            nodes: vec![n.clone(), quiet.clone()],
            pools: vec![pool("nas", dir("/srv"), true)],
            volumes: vec![volume("d", "nas")],
            vms: vec![
                vm("on-nas", &n, &["d"], &[], true),
                vm("no-disk", &n, &[], &[], true),
            ],
            ..Default::default()
        };
        let nas = check(
            HaCheckRole::Storage,
            HaCheckTarget::Pool { name: "nas".into() },
        );
        let mut mgmt = check(HaCheckRole::Management, link("eth0"));
        mgmt.on_fail = Some(HaCheckAction::Fence);
        let mut off = nas.clone();
        off.enabled = false;
        let reports = BTreeMap::from([(
            NodeId(n.id),
            vec![
                report("nas", CheckState::Failing),
                report("mgmt", CheckState::Failing),
            ],
        )]);
        let health = Health {
            term: 1,
            checks: vec![resource("nas", nas), resource("mgmt", mgmt)],
            reports: &reports,
        };
        assert!(health.blocks(&inv.vms[0], &n, &inv));
        assert!(!health.blocks(&inv.vms[1], &n, &inv));
        // 보고가 없는 노드는 막지 않는다.
        assert!(!health.blocks(&inv.vms[0], &quiet, &inv));
        // degraded·unknown은 막지 않는다.
        for state in [CheckState::Degraded, CheckState::Unknown] {
            let reports = BTreeMap::from([(NodeId(n.id), vec![report("nas", state)])]);
            let h = Health {
                reports: &reports,
                ..health_with(&health)
            };
            assert!(!h.blocks(&inv.vms[0], &n, &inv));
        }
    }

    fn health_with<'a>(h: &Health<'a>) -> Health<'a> {
        Health {
            term: h.term,
            checks: h.checks.clone(),
            reports: h.reports,
        }
    }
}
