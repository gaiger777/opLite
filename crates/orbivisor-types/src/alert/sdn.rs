//! SDN 경보 평가(계획 67 §11). 모든 SDN 조건은 리더가 평가한다. 입력은 Raft(노드 상태·`SdnStatus`·라우터·부하 분산기)와
//! 리더 OvnSync가 읽어 메모리에 둔 관찰([`SdnObservations`] — DGP `hosting-chassis`, SB `Service_Monitor`)이다.
//!
//! [`evaluate`]는 규칙 하나의 대상마다 [`SdnFinding`]을 낸다. `holds`가 `None`이면 지금은 알 수 없다(오프라인 노드의
//! chassis·physnet, 이번 임기의 관찰이 아직 없음) — 엔진은 단계를 그대로 둔다. SDN이 설정 전이거나 준비되지 않았으면
//! 대상이 없다(경보 없음). `sdn_gateway_failover`는 이벤트라 바뀐 라우터만 `holds: Some(true)`로 낸다.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use super::{AlertCondition, AlertRuleSpec, AlertSubject, Severity, SubjectKind};
use crate::load_balancer::LoadBalancer;
use crate::network::Network;
use crate::node::Node;
use crate::router::Router;
use crate::sdn::{
    ChassisState, LockdownView, RaftRole, Sdn, SdnPhase, gateway_candidates, port_name,
};
use crate::vm::Vm;

/// `SdnSync.at`(유닉스 초)이 이만큼 넘게 낡으면 동기화가 멈춘 것으로 본다(펜싱으로 멈춤 포함).
pub const SYNC_STALE_SECS: u64 = 300;
/// 관찰이 이만큼(ms) 넘게 낡으면 관찰 조건을 모르는 것으로 본다(OvnSync가 OVN을 읽지 못하는 동안).
pub const OBSERVATION_STALE_MS: i64 = 120_000;

/// 리더 OvnSync가 OVN에서 읽은 것(같은 프로세스의 메모리 — Raft에 쓰지 않는다)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SdnObservations {
    /// 읽은 리더의 Raft 임기. 리더가 바뀌면 새 리더의 첫 관찰까지 관찰 조건을 평가하지 않는다.
    pub term: u64,
    /// 읽은 시각(유닉스 ms)
    pub at: i64,
    /// 라우터 id → 게이트웨이 포트를 가진 chassis 이름(노드 UUID 글, `status:hosting-chassis`가 있는 것만)
    pub hosting: BTreeMap<Uuid, String>,
    /// SB `Service_Monitor` 행
    pub monitors: Vec<MonitorObservation>,
}

/// SB `Service_Monitor` 행 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorObservation {
    pub logical_port: String,
    pub port: u16,
    /// `tcp`·`udp`(비면 tcp로 본다)
    pub protocol: String,
    /// `online`·`offline`·`error`, 아직 없으면 빈 글
    pub status: String,
}

/// 평가 입력
pub struct SdnInputs<'a> {
    pub sdn: Option<&'a Sdn>,
    pub nodes: &'a [Node],
    pub networks: &'a [Network],
    pub routers: &'a [Router],
    pub load_balancers: &'a [LoadBalancer],
    pub vms: &'a [Vm],
    /// 지금 임기의 마지막 관찰(없으면 관찰 조건을 모른다)
    pub observed: Option<&'a SdnObservations>,
    /// 같은 임기의 그 앞 관찰(`sdn_gateway_failover`가 견준다. 없으면 견주지 않는다)
    pub previous: Option<&'a SdnObservations>,
    /// 지금(유닉스 ms)
    pub now_ms: i64,
}

/// 대상 하나를 본 결과
#[derive(Debug, Clone, PartialEq)]
pub struct SdnFinding {
    pub subject: AlertSubject,
    /// 참·거짓, 모르면 `None`
    pub holds: Option<bool>,
    /// 알림 문구에 붙일 까닭(참일 때)
    pub detail: Option<String>,
}

impl SdnFinding {
    fn new(subject: AlertSubject, holds: Option<bool>, detail: Option<String>) -> Self {
        let detail = if holds == Some(true) { detail } else { None };
        Self {
            subject,
            holds,
            detail,
        }
    }
}

/// 규칙 `rule`(SDN 조건)의 대상마다 본 결과. SDN 조건이 아니거나 SDN이 준비되지 않았으면 빈 목록이다.
pub fn evaluate(rule: &AlertRuleSpec, inputs: &SdnInputs) -> Vec<SdnFinding> {
    let Some(sdn) = inputs
        .sdn
        .filter(|s| s.status.phase == SdnPhase::Ready && !s.status.centrals.is_empty())
    else {
        return Vec::new();
    };
    // 관찰은 지금 임기의 것이고 낡지 않았을 때만 쓴다.
    let observed = inputs
        .observed
        .filter(|o| inputs.now_ms.saturating_sub(o.at) <= OBSERVATION_STALE_MS);
    let mut out = match &rule.condition {
        AlertCondition::SdnQuorumLost => vec![quorum_lost(sdn, inputs.nodes)],
        AlertCondition::SdnCentralDown => central_down(sdn, inputs.nodes),
        AlertCondition::SdnChassisDown => chassis_down(inputs.nodes),
        AlertCondition::SdnGatewayUnavailable => gateway_unavailable(inputs),
        AlertCondition::SdnGatewayFailover => gateway_failover(inputs, observed),
        AlertCondition::SdnSyncFailed => vec![sync_failed(sdn, inputs)],
        AlertCondition::SdnPhysnetNotReady => physnet_not_ready(inputs.nodes),
        AlertCondition::SdnLbMemberDown => lb_member_down(inputs, observed),
        _ => Vec::new(),
    };
    // 노드 거르기는 대상이 노드인 조건만 받는다(규칙 검증).
    out.retain(|f| f.subject.kind != SubjectKind::Node || rule.applies_to_node(&f.subject.name));
    out
}

fn node_subject(n: &Node) -> AlertSubject {
    AlertSubject {
        kind: SubjectKind::Node,
        id: n.id,
        name: n.name.clone(),
        node: Some(n.name.clone()),
        port: None,
    }
}

fn sdn_subject(sdn: &Sdn) -> AlertSubject {
    AlertSubject {
        kind: SubjectKind::Sdn,
        id: sdn.id,
        name: sdn.name.clone(),
        node: None,
        port: None,
    }
}

fn role_name(r: RaftRole) -> &'static str {
    match r {
        RaftRole::Leader => "leader",
        RaftRole::Follower => "follower",
        RaftRole::Candidate => "candidate",
        RaftRole::Joining => "joining",
        RaftRole::Down => "down",
        RaftRole::Unknown => "unknown",
    }
}

/// 중앙 노드의 (NB, SB) 역할. 오프라인이거나 알리지 않았으면 `down`이다.
fn roles(sdn: &Sdn, nodes: &[Node]) -> Vec<(String, RaftRole, RaftRole)> {
    sdn.status
        .centrals
        .iter()
        .map(|c| {
            let node = nodes.iter().find(|n| n.id == c.node);
            let name = node.map_or_else(|| c.node.to_string(), |n| n.name.clone());
            match node
                .filter(|n| n.status.online)
                .and_then(|n| n.status.sdn.as_ref()?.central)
            {
                Some(h) => (name, h.nb, h.sb),
                None => (name, RaftRole::Down, RaftRole::Down),
            }
        })
        .collect()
}

fn quorum_lost(sdn: &Sdn, nodes: &[Node]) -> SdnFinding {
    let roles = roles(sdn, nodes);
    let total = roles.len();
    let mut why = Vec::new();
    for (db, pick) in [
        (
            "NB",
            (|r: &(String, RaftRole, RaftRole)| r.1) as fn(&_) -> RaftRole,
        ),
        ("SB", |r| r.2),
    ] {
        let leaders = roles.iter().filter(|r| pick(r) == RaftRole::Leader).count();
        let members = roles
            .iter()
            .filter(|r| matches!(pick(r), RaftRole::Leader | RaftRole::Follower))
            .count();
        if leaders == 0 || members * 2 <= total {
            let detail: Vec<String> = roles
                .iter()
                .map(|r| format!("{} {}", r.0, role_name(pick(r))))
                .collect();
            why.push(format!(
                "{db}: {members} of {total} centrals in the cluster{} ({})",
                if leaders == 0 { ", no leader" } else { "" },
                detail.join(", ")
            ));
        }
    }
    SdnFinding::new(
        sdn_subject(sdn),
        Some(!why.is_empty()),
        Some(format!(
            "The OVN central quorum is lost: {}",
            why.join("; ")
        )),
    )
}

fn central_down(sdn: &Sdn, nodes: &[Node]) -> Vec<SdnFinding> {
    sdn.status
        .centrals
        .iter()
        .filter_map(|c| nodes.iter().find(|n| n.id == c.node))
        .map(|n| {
            let why = if !n.status.online {
                Some("the node is offline".to_string())
            } else {
                match n.status.sdn.as_ref().and_then(|s| s.central) {
                    None => Some("the node reports no central state".to_string()),
                    Some(h) => {
                        let bad = |r: RaftRole| matches!(r, RaftRole::Down | RaftRole::Candidate);
                        (bad(h.nb) || bad(h.sb))
                            .then(|| format!("NB {}, SB {}", role_name(h.nb), role_name(h.sb)))
                    }
                }
            };
            SdnFinding::new(
                node_subject(n),
                Some(why.is_some()),
                why.map(|w| format!("OVN central {} is down: {w}", n.name)),
            )
        })
        .collect()
}

fn chassis_down(nodes: &[Node]) -> Vec<SdnFinding> {
    nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter_map(|n| {
            let s = n.status.sdn.as_ref()?;
            if matches!(s.chassis, ChassisState::Missing | ChassisState::Unknown) {
                return None;
            }
            // 오프라인 노드의 보고는 낡았다(그 노드는 `node_offline`이 알린다).
            let holds = n.status.online.then_some(matches!(
                s.chassis,
                ChassisState::Down | ChassisState::Configuring
            ));
            let state = match s.chassis {
                ChassisState::Down => "ovn-controller is not running",
                _ => "the chassis is not connected to the southbound DB",
            };
            let mut detail = format!("OVN chassis {} is down: {state}", n.name);
            if let Some(e) = &s.error {
                detail.push_str(&format!(" ({e})"));
            }
            Some(SdnFinding::new(node_subject(n), holds, Some(detail)))
        })
        .collect()
}

fn router_subject(r: &Router) -> AlertSubject {
    AlertSubject {
        kind: SubjectKind::Router,
        id: r.id,
        name: r.name.clone(),
        node: None,
        port: None,
    }
}

/// 노드 UUID 글 → 노드 이름(모르면 그 글)
fn node_name(nodes: &[Node], chassis: &str) -> String {
    nodes
        .iter()
        .find(|n| n.id.to_string() == chassis)
        .map_or_else(|| chassis.to_string(), |n| n.name.clone())
}

fn gateway_unavailable(inputs: &SdnInputs) -> Vec<SdnFinding> {
    inputs
        .routers
        .iter()
        .filter_map(|r| {
            let ext = r.spec.external.as_ref()?;
            let physnet = inputs
                .networks
                .iter()
                .find(|n| n.name == ext.network)?
                .spec
                .physnet()?;
            let candidates = gateway_candidates(inputs.nodes, physnet);
            let why = if candidates.is_empty() {
                Some(format!("no gateway node has physnet {physnet} ready"))
            } else {
                let online = candidates
                    .iter()
                    .any(|(id, _)| inputs.nodes.iter().any(|n| n.id == *id && n.status.online));
                (!online).then(|| {
                    let names: Vec<String> = candidates
                        .iter()
                        .map(|(id, _)| node_name(inputs.nodes, &id.to_string()))
                        .collect();
                    format!("all gateway candidates are offline ({})", names.join(", "))
                })
            };
            Some(SdnFinding::new(
                router_subject(r),
                Some(why.is_some()),
                why.map(|w| format!("Router {} has no gateway to {}: {w}", r.name, ext.network)),
            ))
        })
        .collect()
}

fn gateway_failover(inputs: &SdnInputs, observed: Option<&SdnObservations>) -> Vec<SdnFinding> {
    let (Some(now), Some(before)) = (observed, inputs.previous) else {
        return Vec::new();
    };
    if now.term != before.term || now.at == before.at {
        return Vec::new();
    }
    inputs
        .routers
        .iter()
        .filter_map(|r| {
            let (old, new) = (before.hosting.get(&r.id)?, now.hosting.get(&r.id)?);
            (old != new).then(|| {
                let (old, new) = (node_name(inputs.nodes, old), node_name(inputs.nodes, new));
                SdnFinding::new(
                    router_subject(r),
                    Some(true),
                    Some(format!(
                        "The gateway of router {} moved from {old} to {new}",
                        r.name
                    )),
                )
            })
        })
        .collect()
}

fn sync_failed(sdn: &Sdn, inputs: &SdnInputs) -> SdnFinding {
    let mut why = Vec::new();
    let now_secs = u64::try_from(inputs.now_ms / 1000).unwrap_or(0);
    // 기록이 없으면(한 번도 맞추지 않음) 언제부터인지 모르므로 보지 않는다.
    if let Some(sync) = &sdn.status.sync {
        if let Some(e) = &sync.error {
            why.push(format!("the last sync failed: {e}"));
        } else if now_secs.saturating_sub(sync.at) > SYNC_STALE_SECS {
            why.push(format!(
                "no sync for {} s (the leader may be fenced)",
                now_secs.saturating_sub(sync.at)
            ));
        }
        if !sync.failed.is_empty() {
            why.push(format!("failed batches: {}", sync.failed.join("; ")));
        }
    }
    // 잠금(§10.5): 모든 중앙이 바뀐 뒤 노드가 표를 맞추지 못했다.
    if sdn.spec.lockdown
        && let Some(view) = LockdownView::new(sdn, inputs.nodes)
        && !view.pending
        && sdn
            .status
            .centrals
            .iter()
            .all(|c| view.applied.contains(&c.node))
    {
        let online: BTreeSet<Uuid> = inputs
            .nodes
            .iter()
            .filter(|n| n.status.online)
            .map(|n| n.id)
            .collect();
        let bad: Vec<String> = view
            .nodes
            .iter()
            .filter(|n| online.contains(&n.node) && !n.firewall)
            .map(|n| match &n.error {
                Some(e) => format!("{} ({e})", n.name),
                None => n.name.clone(),
            })
            .collect();
        if !bad.is_empty() {
            why.push(format!(
                "the OVN allow-list is not applied on {}",
                bad.join(", ")
            ));
        }
    }
    SdnFinding::new(
        sdn_subject(sdn),
        Some(!why.is_empty()),
        Some(format!("OVN sync is failing: {}", why.join("; "))),
    )
}

fn physnet_not_ready(nodes: &[Node]) -> Vec<SdnFinding> {
    nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter_map(|n| {
            let s = n.status.sdn.as_ref()?;
            // 설정한 physnet도 옮기기 다리도 없는 노드는 대상이 아니다.
            let configured = n.spec.sdn.as_ref().is_some_and(|c| !c.physnets.is_empty());
            if !configured && s.physnets.is_empty() {
                return None;
            }
            let bad: Vec<String> = s
                .physnets
                .iter()
                .filter(|p| !p.ready)
                .map(|p| match &p.error {
                    Some(e) => format!("{} ({e})", p.name),
                    None => p.name.clone(),
                })
                .collect();
            let holds = n.status.online.then_some(!bad.is_empty());
            Some(SdnFinding::new(
                node_subject(n),
                holds,
                Some(format!(
                    "physnets not ready on {}: {}",
                    n.name,
                    bad.join(", ")
                )),
            ))
        })
        .collect()
}

fn lb_member_down(inputs: &SdnInputs, observed: Option<&SdnObservations>) -> Vec<SdnFinding> {
    inputs
        .load_balancers
        .iter()
        .filter_map(|lb| {
            // 건강 검사가 있는 NIC 멤버: (논리 포트, 멤버 포트, 프로토콜, 이름)
            let checked: Vec<(String, u16, &str, String)> = lb
                .spec
                .listeners
                .iter()
                .filter(|l| l.health.is_some())
                .flat_map(|l| {
                    l.members.iter().filter_map(move |m| {
                        let (vm, mac) = m.nic()?;
                        let name = inputs
                            .vms
                            .iter()
                            .find(|v| v.id == vm)
                            .map_or_else(|| vm.to_string(), |v| v.name.clone());
                        Some((
                            port_name(vm, mac),
                            m.port,
                            l.protocol.as_str(),
                            format!("{name}:{} ({}/{})", m.port, l.protocol.as_str(), l.port),
                        ))
                    })
                })
                .collect();
            if checked.is_empty() {
                return None;
            }
            let subject = AlertSubject {
                kind: SubjectKind::LoadBalancer,
                id: lb.id,
                name: lb.name.clone(),
                node: None,
                port: None,
            };
            let Some(obs) = observed else {
                return Some(SdnFinding::new(subject, None, None));
            };
            let down: Vec<String> = checked
                .iter()
                .filter_map(|(lport, port, proto, name)| {
                    let m = obs.monitors.iter().find(|m| {
                        m.logical_port == *lport
                            && m.port == *port
                            && (m.protocol.as_str() == *proto
                                || (m.protocol.is_empty() && *proto == "tcp"))
                    })?;
                    matches!(m.status.as_str(), "offline" | "error")
                        .then(|| format!("{name} {}", m.status))
                })
                .collect();
            Some(SdnFinding::new(
                subject,
                Some(!down.is_empty()),
                Some(format!(
                    "Load balancer {} has members down: {}",
                    lb.name,
                    down.join(", ")
                )),
            ))
        })
        .collect()
}

/// "SDN 경보 켜기"(§11)의 권하는 규칙: (이름, 스펙). 대상은 `targets`다. 끝의 둘은 링크 조건(계획 75 §3.4 — 업링크 포트
/// 끊김 critical 30초, 본딩 저하 warning 60초)이고 기능 `link-alerts`가 클러스터 전체일 때만 만든다(API).
pub fn default_rules(targets: &[String]) -> Vec<(&'static str, AlertRuleSpec)> {
    let rule = |condition, severity, for_secs| AlertRuleSpec {
        enabled: true,
        condition,
        for_secs,
        severity,
        targets: targets.to_vec(),
        repeat_secs: super::DEFAULT_REPEAT_SECS,
        send_resolved: true,
        nodes: Vec::new(),
        vms: Vec::new(),
    };
    vec![
        (
            "sdn-quorum-lost",
            rule(AlertCondition::SdnQuorumLost, Severity::Critical, 0),
        ),
        (
            "sdn-central-down",
            rule(AlertCondition::SdnCentralDown, Severity::Warning, 60),
        ),
        (
            "sdn-chassis-down",
            rule(AlertCondition::SdnChassisDown, Severity::Warning, 60),
        ),
        (
            "sdn-gateway-unavailable",
            rule(
                AlertCondition::SdnGatewayUnavailable,
                Severity::Critical,
                30,
            ),
        ),
        (
            "sdn-gateway-failover",
            rule(AlertCondition::SdnGatewayFailover, Severity::Info, 0),
        ),
        (
            "sdn-sync-failed",
            rule(AlertCondition::SdnSyncFailed, Severity::Warning, 120),
        ),
        (
            "sdn-physnet-not-ready",
            rule(AlertCondition::SdnPhysnetNotReady, Severity::Warning, 60),
        ),
        (
            "sdn-lb-member-down",
            rule(AlertCondition::SdnLbMemberDown, Severity::Warning, 30),
        ),
        (
            "node-link-down",
            rule(
                AlertCondition::NodeLinkDown {
                    ports: None,
                    role: super::LinkRole::Uplink,
                },
                Severity::Critical,
                30,
            ),
        ),
        (
            "bond-degraded",
            rule(AlertCondition::BondDegraded, Severity::Warning, 60),
        ),
    ]
}

#[cfg(test)]
mod tests;
