//! SDN 경보 평가 표(계획 67 §14 순수 시험)

use std::net::IpAddr;

use serde_json::json;

use super::*;
use crate::node::{NodeSpec, NodeStatus};
use crate::resource::Resource;
use crate::sdn::{
    CentralHealth, LockdownNodeState, LockdownState, NodeSdn, NodeSdnConfig, PhysnetState,
    SdnCentral, SdnSpec, SdnStatus, SdnSync,
};

const NOW: i64 = 1_800_000_000_000;

fn node(n: u128, online: bool, sdn: Option<NodeSdn>) -> Node {
    let mut node = Resource::new(
        format!("n{n}"),
        NodeSpec {
            sdn: None,
            address: format!("10.0.0.{n}:8006"),
            maintenance: false,
            witness: false,
            roles: None,
            host_resources: None,
        },
    );
    node.id = Uuid::from_u128(n);
    node.status = NodeStatus {
        online,
        sdn,
        ..NodeStatus::default()
    };
    node
}

fn central(nb: RaftRole, sb: RaftRole) -> Option<NodeSdn> {
    Some(NodeSdn {
        chassis: ChassisState::Ready,
        central: Some(CentralHealth {
            nb,
            sb,
            northd_active: false,
        }),
        ..NodeSdn::default()
    })
}

fn sdn(centrals: &[u128]) -> Sdn {
    let mut s: Sdn = Resource::new("sdn", SdnSpec::default());
    s.status = SdnStatus {
        phase: SdnPhase::Ready,
        centrals: centrals
            .iter()
            .map(|n| SdnCentral {
                node: Uuid::from_u128(*n),
                address: format!("10.0.0.{n}").parse::<IpAddr>().unwrap(),
            })
            .collect(),
        sync: Some(SdnSync {
            at: (NOW / 1000) as u64 - 10,
            ..SdnSync::default()
        }),
        ..SdnStatus::default()
    };
    s
}

fn rule(kind: &str) -> AlertRuleSpec {
    serde_json::from_value(json!({
        "condition": {"kind": kind}, "severity": "warning", "targets": ["ops"]
    }))
    .unwrap()
}

fn inputs<'a>(s: Option<&'a Sdn>, nodes: &'a [Node]) -> SdnInputs<'a> {
    SdnInputs {
        sdn: s,
        nodes,
        networks: &[],
        routers: &[],
        load_balancers: &[],
        vms: &[],
        observed: None,
        previous: None,
        now_ms: NOW,
    }
}

fn holds(f: &[SdnFinding]) -> Vec<(String, Option<bool>)> {
    f.iter()
        .map(|f| (f.subject.name.clone(), f.holds))
        .collect()
}

use RaftRole::{Candidate, Down, Follower, Leader};

/// 정족수: 셋 가운데 하나가 죽으면 그대로, 둘이 죽거나(오프라인 보고 포함) 리더가 없으면 잃음
#[test]
fn quorum_of_three_centrals() {
    let s = sdn(&[1, 2, 3]);
    let r = rule("sdn_quorum_lost");
    let case = |nodes: Vec<Node>| {
        let f = evaluate(&r, &inputs(Some(&s), &nodes));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].subject.kind, SubjectKind::Sdn);
        (f[0].holds, f[0].detail.clone())
    };
    let all = vec![
        node(1, true, central(Leader, Leader)),
        node(2, true, central(Follower, Follower)),
        node(3, true, central(Follower, Follower)),
    ];
    assert_eq!(case(all), (Some(false), None));
    let one_dead = vec![
        node(1, true, central(Leader, Leader)),
        node(2, true, central(Follower, Follower)),
        node(3, true, central(Down, Down)),
    ];
    assert_eq!(case(one_dead).0, Some(false));
    let two_dead = vec![
        node(1, true, central(Leader, Follower)),
        node(2, true, central(Down, Down)),
        node(3, true, central(Down, Down)),
    ];
    let (h, detail) = case(two_dead);
    assert_eq!(h, Some(true));
    let detail = detail.unwrap();
    assert!(detail.contains("NB: 1 of 3"), "{detail}");
    assert!(
        detail.contains("SB: 1 of 3 centrals in the cluster, no leader"),
        "{detail}"
    );
    // 오프라인 노드의 보고(낡은 follower)는 down으로 센다.
    let offline = vec![
        node(1, true, central(Leader, Leader)),
        node(2, false, central(Follower, Follower)),
        node(3, false, central(Follower, Follower)),
    ];
    assert_eq!(case(offline).0, Some(true));
    // 과반이 따라도 리더가 없으면(선거 중) 잃음
    let electing = vec![
        node(1, true, central(Candidate, Leader)),
        node(2, true, central(Follower, Follower)),
        node(3, true, central(Follower, Follower)),
    ];
    let (h, detail) = case(electing);
    assert_eq!(h, Some(true));
    assert!(
        detail
            .unwrap()
            .starts_with("The OVN central quorum is lost: NB:")
    );
}

/// SDN이 설정 전이거나 준비되지 않았으면 대상이 없다(모든 조건).
#[test]
fn nothing_before_the_sdn_is_ready() {
    let nodes = vec![node(1, false, central(Down, Down))];
    let mut s = sdn(&[1]);
    s.status.phase = SdnPhase::Bootstrapping;
    for kind in [
        "sdn_quorum_lost",
        "sdn_central_down",
        "sdn_chassis_down",
        "sdn_sync_failed",
        "sdn_physnet_not_ready",
    ] {
        assert!(evaluate(&rule(kind), &inputs(None, &nodes)).is_empty());
        assert!(evaluate(&rule(kind), &inputs(Some(&s), &nodes)).is_empty());
    }
}

/// 중앙 꺼짐: 오프라인, 보고 없음, NB·SB가 down·candidate. 노드 거르기를 따른다.
#[test]
fn central_down_per_node() {
    let s = sdn(&[1, 2, 3, 4]);
    let nodes = vec![
        node(1, true, central(Leader, Follower)),
        node(2, false, central(Follower, Follower)),
        node(3, true, central(Follower, Candidate)),
        node(4, true, None),
        node(5, true, central(Down, Down)), // 중앙이 아님
    ];
    let r = rule("sdn_central_down");
    let f = evaluate(&r, &inputs(Some(&s), &nodes));
    assert_eq!(
        holds(&f),
        [
            ("n1".into(), Some(false)),
            ("n2".into(), Some(true)),
            ("n3".into(), Some(true)),
            ("n4".into(), Some(true)),
        ]
    );
    assert_eq!(
        f[1].detail.as_deref(),
        Some("OVN central n2 is down: the node is offline")
    );
    assert_eq!(
        f[2].detail.as_deref(),
        Some("OVN central n3 is down: NB follower, SB candidate")
    );
    let mut only = r.clone();
    only.nodes = vec!["n3".into()];
    assert_eq!(
        holds(&evaluate(&only, &inputs(Some(&s), &nodes))),
        [("n3".into(), Some(true))]
    );
}

/// chassis 꺼짐: 구성한 노드만, 오프라인 노드는 모름
#[test]
fn chassis_down_per_node() {
    let s = sdn(&[1]);
    let with = |c: ChassisState| {
        Some(NodeSdn {
            chassis: c,
            ..NodeSdn::default()
        })
    };
    let nodes = vec![
        node(1, true, with(ChassisState::Ready)),
        node(2, true, with(ChassisState::Down)),
        node(3, true, with(ChassisState::Configuring)),
        node(4, false, with(ChassisState::Down)),
        node(5, true, with(ChassisState::Missing)),
        node(6, true, None),
    ];
    let f = evaluate(&rule("sdn_chassis_down"), &inputs(Some(&s), &nodes));
    assert_eq!(
        holds(&f),
        [
            ("n1".into(), Some(false)),
            ("n2".into(), Some(true)),
            ("n3".into(), Some(true)),
            ("n4".into(), None),
        ]
    );
    assert_eq!(
        f[1].detail.as_deref(),
        Some("OVN chassis n2 is down: ovn-controller is not running")
    );
}

fn provider(name: &str) -> Network {
    let spec = serde_json::from_value(json!({
        "sdn": {"provider": {"physnet": "ext"}},
        "subnets": [{"cidr": "203.0.113.0/24", "gateway": "203.0.113.1"}]
    }))
    .unwrap();
    Resource::new(name, spec)
}

fn router(n: u128, external: bool) -> Router {
    let spec = if external {
        json!({"external": {"network": "ext", "ips": ["203.0.113.50"]}})
    } else {
        json!({})
    };
    let mut r: Router = Resource::new(format!("r{n}"), serde_json::from_value(spec).unwrap());
    r.id = Uuid::from_u128(100 + n);
    r
}

fn gateway(n: u128, online: bool, priority: Option<u16>, ready: bool) -> Node {
    let mut node = node(
        n,
        online,
        Some(NodeSdn {
            chassis: ChassisState::Ready,
            physnets: vec![PhysnetState {
                name: "ext".into(),
                bridge: "vmbr1".into(),
                ready,
                error: (!ready).then(|| "no bridge vmbr1".into()),
            }],
            ..NodeSdn::default()
        }),
    );
    node.spec.sdn = Some(NodeSdnConfig {
        physnets: [("ext".to_string(), "vmbr1".to_string())].into(),
        gateway: priority,
    });
    node
}

/// 게이트웨이 없음: 후보가 없거나(우선순위 없음·physnet 준비 안 됨·유지보수) 모두 오프라인. 바깥 없는 라우터는 대상이 아님
#[test]
fn gateway_unavailable_per_router() {
    let s = sdn(&[1]);
    let networks = vec![provider("ext")];
    let routers = vec![router(1, true), router(2, false)];
    let run = |nodes: &[Node]| {
        let mut i = inputs(Some(&s), nodes);
        i.networks = &networks;
        i.routers = &routers;
        evaluate(&rule("sdn_gateway_unavailable"), &i)
    };
    let ok = vec![
        gateway(1, true, Some(200), true),
        gateway(2, true, None, true),
    ];
    assert_eq!(holds(&run(&ok)), [("r1".into(), Some(false))]);
    let none = vec![
        gateway(1, true, None, true),
        gateway(2, true, Some(100), false),
    ];
    let f = run(&none);
    assert_eq!(f[0].holds, Some(true));
    assert_eq!(
        f[0].detail.as_deref(),
        Some("Router r1 has no gateway to ext: no gateway node has physnet ext ready")
    );
    let offline = vec![
        gateway(1, false, Some(200), true),
        gateway(2, false, Some(100), true),
    ];
    let f = run(&offline);
    assert_eq!(f[0].holds, Some(true));
    assert!(
        f[0].detail
            .as_deref()
            .unwrap()
            .ends_with("all gateway candidates are offline (n1, n2)"),
        "{f:?}"
    );
}

fn observed(term: u64, at: i64, hosting: &[(u128, u128)]) -> SdnObservations {
    SdnObservations {
        term,
        at,
        hosting: hosting
            .iter()
            .map(|(r, n)| (Uuid::from_u128(100 + r), Uuid::from_u128(*n).to_string()))
            .collect(),
        monitors: Vec::new(),
    }
}

/// 게이트웨이 넘김(이벤트): 같은 임기의 앞 관찰과 견주어 바뀐 라우터만. 앞 관찰이 없거나 임기가 다르면 견주지 않는다.
#[test]
fn gateway_failover_compares_observations() {
    let s = sdn(&[1]);
    let nodes = vec![node(1, true, None), node(2, true, None)];
    let routers = vec![router(1, true), router(2, true)];
    let before = observed(7, NOW - 10_000, &[(1, 1), (2, 1)]);
    let after = observed(7, NOW - 1_000, &[(1, 2), (2, 1)]);
    let run = |obs: Option<&SdnObservations>, prev: Option<&SdnObservations>| {
        let mut i = inputs(Some(&s), &nodes);
        i.routers = &routers;
        i.observed = obs;
        i.previous = prev;
        evaluate(&rule("sdn_gateway_failover"), &i)
    };
    let f = run(Some(&after), Some(&before));
    assert_eq!(holds(&f), [("r1".into(), Some(true))]);
    assert_eq!(
        f[0].detail.as_deref(),
        Some("The gateway of router r1 moved from n1 to n2")
    );
    assert!(run(Some(&after), None).is_empty());
    let other_term = observed(6, NOW - 10_000, &[(1, 1)]);
    assert!(run(Some(&after), Some(&other_term)).is_empty());
    // 낡은 관찰은 쓰지 않는다.
    let old = observed(7, NOW - OBSERVATION_STALE_MS - 1, &[(1, 2)]);
    assert!(run(Some(&old), Some(&before)).is_empty());
}

/// 동기화 실패: 오류, 300초 넘게 낡음, 실패한 묶음 이름이 문구에, 잠금 표가 맞지 않는 온라인 노드
#[test]
fn sync_failed_reasons() {
    let r = rule("sdn_sync_failed");
    let run = |s: &Sdn, nodes: &[Node]| {
        let f = evaluate(&r, &inputs(Some(s), nodes));
        (f[0].holds, f[0].detail.clone())
    };
    let nodes = vec![node(1, true, central(Leader, Leader))];
    let mut s = sdn(&[1]);
    assert_eq!(run(&s, &nodes), (Some(false), None));
    s.status.sync.as_mut().unwrap().error = Some("NB unreachable".into());
    s.status.sync.as_mut().unwrap().failed = vec!["port-group web,ssh".into()];
    let (h, d) = run(&s, &nodes);
    assert_eq!(h, Some(true));
    assert_eq!(
        d.as_deref(),
        Some(
            "OVN sync is failing: the last sync failed: NB unreachable; failed batches: port-group web,ssh"
        )
    );
    let mut s = sdn(&[1]);
    s.status.sync.as_mut().unwrap().at = (NOW / 1000) as u64 - SYNC_STALE_SECS - 5;
    let (h, d) = run(&s, &nodes);
    assert_eq!(h, Some(true));
    assert!(d.unwrap().contains("no sync for 305 s"));
}

/// 잠금이 켜졌고 모든 중앙이 바뀐 뒤 어떤 온라인 노드의 허용 목록이 맞지 않으면 `sdn_sync_failed`. 중앙을 바꾸는 중이거나
/// 오프라인 노드는 보지 않는다.
#[test]
fn sync_failed_when_the_allow_list_is_not_applied() {
    let r = rule("sdn_sync_failed");
    let firewall = |ok: bool| {
        let mut s = central(Leader, Leader).unwrap();
        s.lockdown = Some(LockdownNodeState {
            firewall: ok,
            allowed: 3,
            error: (!ok).then(|| "nft failed".into()),
        });
        Some(s)
    };
    let mut s = sdn(&[1, 2]);
    s.spec.lockdown = true;
    s.status.lockdown = Some(LockdownState {
        applied: vec![Uuid::from_u128(1), Uuid::from_u128(2)],
        pending: false,
        error: None,
    });
    let nodes = vec![
        node(1, true, firewall(true)),
        node(2, true, firewall(false)),
    ];
    let f = evaluate(&r, &inputs(Some(&s), &nodes));
    assert_eq!(f[0].holds, Some(true));
    assert_eq!(
        f[0].detail.as_deref(),
        Some("OVN sync is failing: the OVN allow-list is not applied on n2 (nft failed)")
    );
    // 바꾸는 중
    let mut busy = s.clone();
    busy.status.lockdown.as_mut().unwrap().pending = true;
    assert_eq!(
        evaluate(&r, &inputs(Some(&busy), &nodes))[0].holds,
        Some(false)
    );
    let mut half = s.clone();
    half.status.lockdown.as_mut().unwrap().applied.pop();
    assert_eq!(
        evaluate(&r, &inputs(Some(&half), &nodes))[0].holds,
        Some(false)
    );
    // 오프라인 노드
    let nodes = vec![
        node(1, true, firewall(true)),
        node(2, false, firewall(false)),
    ];
    assert_eq!(
        evaluate(&r, &inputs(Some(&s), &nodes))[0].holds,
        Some(false)
    );
    // 잠금을 끄면 보지 않는다.
    let mut off = s.clone();
    off.spec.lockdown = false;
    let nodes = vec![
        node(1, true, firewall(true)),
        node(2, true, firewall(false)),
    ];
    assert_eq!(
        evaluate(&r, &inputs(Some(&off), &nodes))[0].holds,
        Some(false)
    );
}

/// physnet 준비 안 됨(옮기기 다리 포함): 까닭이 문구에, 오프라인은 모름, physnet이 없는 노드는 대상이 아님
#[test]
fn physnet_not_ready_per_node() {
    let s = sdn(&[1]);
    let mut bridge = node(
        3,
        true,
        Some(NodeSdn {
            chassis: ChassisState::Ready,
            physnets: vec![PhysnetState {
                name: "mig0123abcd".into(),
                bridge: "oplv100".into(),
                ready: false,
                error: None,
            }],
            ..NodeSdn::default()
        }),
    );
    bridge.spec.sdn = None;
    let nodes = vec![
        gateway(1, true, None, true),
        gateway(2, true, None, false),
        bridge,
        gateway(4, false, None, false),
        node(5, true, central(Leader, Leader)),
    ];
    let f = evaluate(&rule("sdn_physnet_not_ready"), &inputs(Some(&s), &nodes));
    assert_eq!(
        holds(&f),
        [
            ("n1".into(), Some(false)),
            ("n2".into(), Some(true)),
            ("n3".into(), Some(true)),
            ("n4".into(), None),
        ]
    );
    assert_eq!(
        f[1].detail.as_deref(),
        Some("physnets not ready on n2: ext (no bridge vmbr1)")
    );
    assert_eq!(
        f[2].detail.as_deref(),
        Some("physnets not ready on n3: mig0123abcd")
    );
}

/// LB 멤버 죽음: 건강 검사가 있는 NIC 멤버만, LB마다 하나, 관찰이 없으면 모름
#[test]
fn lb_member_down_per_load_balancer() {
    let s = sdn(&[1]);
    let vm_id = Uuid::from_u128(500);
    let mac = "52:54:00:00:00:01";
    let mut vm: Vm = Resource::new(
        "web1",
        serde_json::from_value(json!({"cpu": {"cores": 1}, "memory_mib": 128, "machine": "virt"}))
            .unwrap(),
    );
    vm.id = vm_id;
    let lb = |name: &str, health: bool| {
        let mut listener = json!({"protocol": "tcp", "port": 80, "members": [
            {"vm": vm_id, "mac": mac, "port": 8080},
            {"ip": "10.10.0.9", "port": 8080}
        ]});
        if health {
            listener["health"] = json!({});
        }
        let spec = serde_json::from_value(json!({
            "network": "t1", "vip": "10.10.0.50", "listeners": [listener]
        }))
        .unwrap();
        Resource::new(name, spec)
    };
    let lbs = vec![lb("web", true), lb("plain", false)];
    let vms = vec![vm];
    let lport = port_name(vm_id, mac.parse().unwrap());
    let obs = |status: &str| SdnObservations {
        term: 1,
        at: NOW,
        hosting: BTreeMap::new(),
        monitors: vec![MonitorObservation {
            logical_port: lport.clone(),
            port: 8080,
            protocol: String::new(),
            status: status.into(),
        }],
    };
    let run = |o: Option<&SdnObservations>| {
        let mut i = inputs(Some(&s), &[]);
        i.load_balancers = &lbs;
        i.vms = &vms;
        i.observed = o;
        evaluate(&rule("sdn_lb_member_down"), &i)
    };
    assert_eq!(holds(&run(None)), [("web".into(), None)]);
    assert_eq!(
        holds(&run(Some(&obs("online")))),
        [("web".into(), Some(false))]
    );
    let f = run(Some(&obs("offline")));
    assert_eq!(f[0].holds, Some(true));
    assert_eq!(f[0].subject.kind, SubjectKind::LoadBalancer);
    assert_eq!(
        f[0].detail.as_deref(),
        Some("Load balancer web has members down: web1:8080 (tcp/80) offline")
    );
    assert_eq!(run(Some(&obs("error")))[0].holds, Some(true));
}

/// 권하는 묶음(§11): SDN 여덟 조건과 링크 조건 둘(계획 75), 이름·심각도·지속 시간, 모두 검증을 지난다.
#[test]
fn default_rules_cover_every_condition() {
    let rules = default_rules(&["ops".into()]);
    assert_eq!(rules.len(), 10);
    let mut kinds = BTreeSet::new();
    for (name, spec) in &rules {
        spec.validate().unwrap();
        assert!(spec.condition.is_sdn() || spec.condition.is_link());
        assert_eq!(*name, spec.condition.kind().replace('_', "-"));
        kinds.insert(spec.condition.kind());
    }
    assert_eq!(kinds.len(), 10);
    assert_eq!(
        rules.iter().filter(|(_, s)| s.condition.is_link()).count(),
        2
    );
    let find = |n: &str| rules.iter().find(|(name, _)| *name == n).unwrap().1.clone();
    assert_eq!(
        (
            find("sdn-quorum-lost").severity,
            find("sdn-quorum-lost").for_secs
        ),
        (Severity::Critical, 0)
    );
    assert_eq!(find("sdn-gateway-unavailable").for_secs, 30);
    assert_eq!(find("sdn-gateway-failover").severity, Severity::Info);
    assert_eq!(find("sdn-sync-failed").for_secs, 120);
    assert_eq!(find("sdn-lb-member-down").for_secs, 30);
    let link = find("node-link-down");
    assert_eq!((link.severity, link.for_secs), (Severity::Critical, 30));
    assert_eq!(
        link.condition,
        AlertCondition::NodeLinkDown {
            ports: None,
            role: crate::alert::LinkRole::Uplink
        }
    );
    assert_eq!(
        (
            find("bond-degraded").severity,
            find("bond-degraded").for_secs
        ),
        (Severity::Warning, 60)
    );
}
