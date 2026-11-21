//! 리더 OvnSync(계획 65 스테이지 3): 한 노드 클러스터의 Raft 리소스를 가짜 NB·SB 서버(JSON-RPC, 평문 TCP)에 맞춘다.
//! 리소스는 시험이 저장소에 직접 만든다(SDN API는 스테이지 6).

mod common;

use std::time::Duration;

use common::Env;
use opl_api::{OvnSync, OvnSyncConfig, OvnSyncer, OvnTransport, SyncOutcome};
use opl_driver::ovn::ovsdb::parse_map;
use opl_driver::ovn::{FakeOvsdb, NB_DB, SB_DB};
use opl_types::network::{Network, NetworkSpec, SdnNet};
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, Sdn, SdnCentral, SdnPhase, SdnSpec, SdnSync};
use serde_json::json;
use uuid::Uuid;

const TAG: &str = "abcdef012345";

struct Ovn {
    env: Env,
    nb: FakeOvsdb,
    sb: FakeOvsdb,
}

impl Ovn {
    async fn start() -> Self {
        let env = Box::pin(Env::start()).await;
        let nb = FakeOvsdb::start(NB_DB).await;
        let sb = FakeOvsdb::start(SB_DB).await;
        Self { env, nb, sb }
    }

    fn config(&self) -> OvnSyncConfig {
        let mut c = OvnSyncConfig::new(TAG, OvnTransport::Tcp);
        c.nb_port = self.nb.addr().port();
        c.sb_port = self.sb.addr().port();
        c.timeout = Duration::from_secs(5);
        c.debounce = Duration::from_millis(50);
        c
    }

    fn syncer(&self) -> OvnSyncer {
        OvnSyncer::new(self.env.cluster.clone(), self.config())
    }

    /// 이 노드가 유일한 중앙인 `Sdn`(phase)
    async fn sdn(&self, phase: SdnPhase) {
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status.phase = phase;
        sdn.status.centrals = vec![SdnCentral {
            node: self.env.node,
            address: "127.0.0.1".parse().unwrap(),
        }];
        self.env.cluster.create(&sdn).await.unwrap();
    }

    fn sdn_status(&self) -> (u64, Option<SdnSync>) {
        let s = self
            .env
            .cluster
            .get_by_name::<SdnSpec>(SDN_NAME)
            .unwrap()
            .unwrap();
        (s.revision, s.status.sync)
    }

    async fn network(&self, name: &str) -> Network {
        let net: Network = Resource::new(
            name,
            NetworkSpec {
                bridge: None,
                vlan: None,
                vxlan: None,
                sdn: Some(SdnNet::default()),
                mtu: None,
                subnets: Vec::new(),
                folder: None,
            },
        );
        self.env.cluster.create(&net).await.unwrap();
        net
    }

    fn switches(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .nb
            .rows("Logical_Switch")
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    }

    fn fence(&self) -> Option<String> {
        self.nb
            .rows("NB_Global")
            .first()
            .and_then(|g| parse_map(&g["external_ids"]).remove(&format!("orbivisor:{TAG}:fence")))
    }
}

fn term(env: &Env) -> u64 {
    env.cluster.leader_term().unwrap()
}

#[tokio::test]
async fn idles_until_sdn_is_ready() {
    let ovn = Ovn::start().await;
    let mut s = ovn.syncer();
    ovn.network("tenant-a").await;
    assert_eq!(s.sync().await, SyncOutcome::Idle);
    ovn.sdn(SdnPhase::Bootstrapping).await;
    assert_eq!(s.sync().await, SyncOutcome::Idle);
    assert!(ovn.nb.with(|n| n.transacts.is_empty()));
    assert!(ovn.switches().is_empty());
}

#[tokio::test]
async fn syncs_switches_records_the_token_and_writes_status_only_on_change() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let a = ovn.network("tenant-a").await;
    let mut s = ovn.syncer();
    let outcome = s.sync().await;
    assert!(
        matches!(
            outcome,
            SyncOutcome::Synced {
                transactions: 1,
                failed: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(ovn.switches(), vec![format!("opl-{}", a.id)]);
    let row = &ovn.nb.rows("Logical_Switch")[0];
    let ids = parse_map(&row["external_ids"]);
    assert_eq!(ids["orbivisor:cluster"], TAG);
    assert_eq!(ids["orbivisor:id"], a.id.to_string());
    assert_eq!(ids["orbivisor:name"], "tenant-a");
    let t = term(&ovn.env);
    let (rev, sync) = ovn.sdn_status();
    let sync = sync.unwrap();
    assert_eq!(sync.term, t);
    assert!(sync.index > 0);
    assert_eq!(ovn.fence(), Some(format!("{t}:{}", sync.index)));
    assert_eq!((sync.nb_cfg, sync.error.as_deref()), (1, None));

    // 같으면 트랜잭션도, 기록 쓰기도 없다.
    let commits = ovn.nb.with(|n| n.commits);
    let outcome = s.sync().await;
    assert!(
        matches!(
            outcome,
            SyncOutcome::Synced {
                transactions: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(ovn.nb.with(|n| n.commits), commits);
    assert_eq!(ovn.sdn_status().0, rev);

    // 네트워크를 지우면 스위치가 사라지고, 태그 없는 스위치는 남는다.
    ovn.nb
        .with(|n| n.insert("Logical_Switch", json!({"name": "admin"})));
    ovn.env
        .cluster
        .delete::<NetworkSpec>(a.id, None)
        .await
        .unwrap();
    s.sync().await;
    assert_eq!(ovn.switches(), vec!["admin".to_string()]);
    assert_eq!(ovn.sdn_status().1.unwrap().nb_cfg, 2);
}

#[tokio::test]
async fn restores_rows_after_a_restart_or_an_emptied_nb() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let a = ovn.network("a").await;
    let b = ovn.network("b").await;
    let mut s = ovn.syncer();
    s.sync().await;
    assert_eq!(ovn.switches().len(), 2);
    // 서버가 다시 떠서 연결이 끊기고 누가 우리 스위치 하나를 지웠다.
    ovn.nb.disconnect_all();
    ovn.nb
        .with(|n| n.delete_where("Logical_Switch", "name", &json!(format!("opl-{}", a.id))));
    let outcome = s.sync().await;
    assert!(
        matches!(
            outcome,
            SyncOutcome::Synced {
                transactions: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    let mut want = vec![format!("opl-{}", a.id), format!("opl-{}", b.id)];
    want.sort();
    assert_eq!(ovn.switches(), want);
    // 새 OvnSyncer(리더가 다시 뜬 것)도 같은 답을 낸다 — 할 일이 없다.
    let mut again = ovn.syncer();
    assert!(matches!(
        again.sync().await,
        SyncOutcome::Synced {
            transactions: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn a_newer_term_in_the_nb_fences_this_leader() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    ovn.network("a").await;
    let t = term(&ovn.env);
    ovn.nb.with(|n| {
        n.insert(
            "NB_Global",
            json!({"external_ids": ["map", [[format!("orbivisor:{TAG}:fence"), format!("{}:1", t + 1)]]]}),
        )
    });
    let mut s = ovn.syncer();
    assert_eq!(s.sync().await, SyncOutcome::Fenced { term: t + 1 });
    assert!(ovn.switches().is_empty());
    assert_eq!(ovn.fence(), Some(format!("{}:1", t + 1)));
    assert!(ovn.sdn_status().1.is_none());
    // 같은 임기의 토큰(내가 전에 쓴 것)이면 이어 쓴다.
    ovn.nb.with(|n| {
        let g = n
            .tables
            .get_mut("NB_Global")
            .unwrap()
            .values_mut()
            .next()
            .unwrap();
        g.insert(
            "external_ids".into(),
            json!([
                "map",
                [[format!("orbivisor:{TAG}:fence"), format!("{t}:1")]]
            ]),
        );
    });
    assert!(matches!(
        s.sync().await,
        SyncOutcome::Synced {
            transactions: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn an_unreachable_nb_records_the_error_and_deletes_nothing() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    ovn.network("a").await;
    let mut config = ovn.config();
    // 아무도 듣지 않는 포트
    config.nb_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut s = OvnSyncer::new(ovn.env.cluster.clone(), config);
    let outcome = s.sync().await;
    assert!(matches!(outcome, SyncOutcome::Failed(_)), "{outcome:?}");
    let sync = ovn.sdn_status().1.unwrap();
    assert!(sync.error.unwrap().contains("no OVSDB server"));
    // 팔로어만 있으면 쓰지 않는다(팔로어의 행은 낡았을 수 있다).
    ovn.nb.with(|n| n.leader = false);
    let mut s = ovn.syncer();
    assert!(
        matches!(s.sync().await, SyncOutcome::Failed(e) if e.contains("not the cluster leader"))
    );
    assert!(ovn.nb.with(|n| n.commits) == 0);
    // 리더가 돌아오면 오류가 지워진다.
    ovn.nb.with(|n| n.leader = true);
    s.sync().await;
    assert_eq!(ovn.sdn_status().1.unwrap().error, None);
}

#[tokio::test]
async fn removes_the_sb_chassis_of_nodes_that_left() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let me = ovn.env.node.to_string();
    let gone = Uuid::now_v7().to_string();
    ovn.sb.with(|s| {
        let encap = s.insert("Encap", json!({"type": "geneve", "ip": "10.0.0.9"}));
        s.insert("Chassis", json!({"name": gone, "encaps": ["uuid", encap]}));
        s.insert("Chassis_Private", json!({"name": gone}));
        s.insert("Chassis", json!({"name": me}));
        s.insert("Chassis", json!({"name": "admin-chassis"}));
    });
    let mut s = ovn.syncer();
    // 모든 노드가 `sdn-services`를 알면 리더는 SB에 쓰지 않는다(계획 67 §10.2 — 중앙 에이전트가 한다).
    assert!(matches!(
        s.sync().await,
        SyncOutcome::Synced {
            chassis_deleted: 0,
            ..
        }
    ));
    assert_eq!(ovn.sb.rows("Chassis").len(), 3);
    // 옛 노드가 섞인 동안은 리더가 계획 65처럼 지운다.
    let node = ovn
        .env
        .cluster
        .get::<opl_types::node::NodeSpec>(ovn.env.node)
        .unwrap()
        .unwrap();
    let mut status = node.status;
    status
        .features
        .retain(|f| f != opl_types::node::SDN_SERVICES);
    ovn.env
        .cluster
        .update_status::<opl_types::node::NodeSpec>(ovn.env.node, &status)
        .await
        .unwrap();
    let outcome = s.sync().await;
    assert!(
        matches!(
            outcome,
            SyncOutcome::Synced {
                chassis_deleted: 1,
                ..
            }
        ),
        "{outcome:?}"
    );
    let mut names: Vec<String> = ovn
        .sb
        .rows("Chassis")
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    let mut want = vec![me.clone(), "admin-chassis".to_string()];
    want.sort();
    assert_eq!(names, want);
    assert!(ovn.sb.rows("Chassis_Private").is_empty());
    // 참조가 사라진 Encap도 따라 사라진다.
    assert!(ovn.sb.rows("Encap").is_empty());
    assert!(matches!(
        s.sync().await,
        SyncOutcome::Synced {
            chassis_deleted: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn the_loop_follows_network_changes() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let sync = OvnSync::spawn(ovn.env.cluster.clone(), ovn.config());
    let a = ovn.network("a").await;
    let want = format!("opl-{}", a.id);
    let mut seen = false;
    for _ in 0..100 {
        if ovn.switches() == vec![want.clone()] {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "{:?}", ovn.switches());
    ovn.env
        .cluster
        .delete::<NetworkSpec>(a.id, None)
        .await
        .unwrap();
    for _ in 0..100 {
        if ovn.switches().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(ovn.switches().is_empty());
    sync.shutdown().await;
    // 쓴 것은 모두 우리 펜싱 토큰을 가진다.
    let txs = ovn.nb.with(|n| n.transacts.clone());
    assert!(txs.iter().any(|t| t[1]["op"] == "wait"));
}

/// 주소 목록(스테이지 6)은 API 노드가 NB의 `up`과 SB의 바인딩 chassis를 읽어 NIC마다 보인다.
#[tokio::test]
async fn the_ip_list_shows_live_port_state() {
    let nb = FakeOvsdb::start(NB_DB).await;
    let sb = FakeOvsdb::start(SB_DB).await;
    let mut config = OvnSyncConfig::new(TAG, OvnTransport::Tcp);
    config.nb_port = nb.addr().port();
    config.sb_port = sb.addr().port();
    config.timeout = Duration::from_secs(5);
    let for_state = config.clone();
    let env = Box::pin(Env::start_with_state(move |s| s.ovn = Some(for_state))).await;
    let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
    sdn.status.phase = SdnPhase::Ready;
    sdn.status.centrals = vec![SdnCentral {
        node: env.node,
        address: "127.0.0.1".parse().unwrap(),
    }];
    env.cluster.create(&sdn).await.unwrap();
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "tenant", "spec": { "sdn": {}, "subnets": [{ "cidr": "10.10.0.0/24" }] } }),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{body}");
    let mut req = common::vm_request("a");
    req["spec"]["nics"] = json!([{ "network": "tenant", "model": "virtio" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{vm}");
    let mut s = OvnSyncer::new(env.cluster.clone(), config);
    assert!(matches!(s.sync().await, SyncOutcome::Synced { .. }));
    // 바인딩 전: up 거짓, 노드 없음
    let (_, ips) = admin.get("/networks/tenant/ips").await;
    assert_eq!(
        ips["allocations"][0]["live"],
        json!({ "up": false }),
        "{ips}"
    );
    assert!(ips.get("live_error").is_none(), "{ips}");
    // northd·ovn-controller가 한 것처럼
    let port = nb.rows("Logical_Switch_Port")[0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    nb.with(|st| {
        for row in st
            .tables
            .get_mut("Logical_Switch_Port")
            .unwrap()
            .values_mut()
        {
            row.insert("up".into(), json!(true));
        }
    });
    sb.with(|st| {
        let chassis = st.insert("Chassis", json!({ "name": env.node.to_string() }));
        st.insert(
            "Port_Binding",
            json!({ "logical_port": port, "chassis": ["uuid", chassis] }),
        );
    });
    let (_, ips) = admin.get("/networks/tenant/ips").await;
    assert_eq!(
        ips["allocations"][0]["live"],
        json!({ "up": true, "node": "node1" }),
        "{ips}"
    );
    // NB가 없으면 live는 null이고 까닭이 있다.
    drop(nb);
    let (_, ips) = admin.get("/networks/tenant/ips").await;
    assert!(ips["allocations"][0]["live"].is_null(), "{ips}");
    assert!(ips["live_error"].is_string(), "{ips}");
}

/// 리뷰 2 m-2: 한 차례가 네트워크마다 트랜잭션을 나눠도 `nb_cfg`는 한 번만 오른다(올릴 때마다 모든 chassis가 SB에 쓴다).
#[tokio::test]
async fn nb_cfg_rises_once_per_round() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    ovn.network("a").await;
    ovn.network("b").await;
    ovn.network("c").await;
    let mut s = ovn.syncer();
    let outcome = s.sync().await;
    assert!(
        matches!(
            outcome,
            SyncOutcome::Synced {
                transactions: 3,
                failed: 0,
                ..
            }
        ),
        "{outcome:?}"
    );
    let g = &ovn.nb.rows("NB_Global")[0];
    assert_eq!(g["nb_cfg"], 1);
    assert_eq!(ovn.sdn_status().1.unwrap().nb_cfg, 1);
}

/// 리뷰 2 m-2: 논리 포트에 쓰이지 않는 VM 쓰기(상태의 오류 문구 같은 것)로는 루프가 NB를 다시 읽지 않는다. 주소가 바뀌면
/// 읽고 맞춘다.
#[tokio::test]
async fn the_loop_ignores_vm_writes_that_do_not_touch_ports() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let net = ovn.network("a").await;
    let mut vm: opl_types::vm::Vm = Resource::new(
        "v",
        serde_json::from_value(json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt",
            "nics": [{ "network": "a", "model": "virtio", "mac": "52:54:00:00:00:01", "ips": [] }]
        }))
        .unwrap(),
    );
    ovn.env.cluster.create(&vm).await.unwrap();
    let mut config = ovn.config();
    config.resync = Duration::from_secs(600);
    let sync = OvnSync::spawn(ovn.env.cluster.clone(), config);
    let ports = || ovn.nb.rows("Logical_Switch_Port").len();
    for _ in 0..100 {
        if ports() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ports(), 1, "{:?}", ovn.switches());
    assert_eq!(ovn.switches(), vec![format!("opl-{}", net.id)]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reads = || ovn.nb.with(|n| n.transacts.len());
    let before = reads();
    for i in 0..3 {
        vm.status.last_error = Some(format!("noise {i}"));
        ovn.env
            .cluster
            .update_status::<opl_types::vm::VmSpec>(vm.id, &vm.status)
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(reads(), before);
    let current = ovn
        .env
        .cluster
        .get::<opl_types::vm::VmSpec>(vm.id)
        .unwrap()
        .unwrap();
    let mut spec = current.spec.clone();
    spec.power = opl_types::vm::PowerState::Running;
    ovn.env
        .cluster
        .update_spec(vm.id, None, current.labels.clone(), &spec)
        .await
        .unwrap();
    for _ in 0..100 {
        if reads() > before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(reads() > before);
    sync.shutdown().await;
}

/// 계획 67 §7.2: 새 네트워크·라우터·LB가 한 차례에 함께 생겨도 그 차례 안에 붙는다(미룬 붙이기를 곧바로 한 번 더 맞춤).
/// LB 쓰기는 반복 작업을 깨운다.
#[tokio::test]
async fn a_new_load_balancer_is_attached_in_the_same_round_and_wakes_the_loop() {
    use opl_types::load_balancer::LoadBalancer;
    use opl_types::router::Router;
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let t1: Network = Resource::new(
        "t1",
        serde_json::from_value(json!({"sdn": {}, "subnets": [{"cidr": "10.10.0.0/24"}]})).unwrap(),
    );
    ovn.env.cluster.create(&t1).await.unwrap();
    let r1: Router = Resource::new(
        "r1",
        serde_json::from_value(json!({"interfaces": [{"network": "t1"}]})).unwrap(),
    );
    ovn.env.cluster.create(&r1).await.unwrap();
    let lb = |name: &str, vip: &str| -> LoadBalancer {
        Resource::new(
            name,
            serde_json::from_value(json!({
                "network": "t1", "vip": vip,
                "listeners": [{"protocol": "tcp", "port": 80, "members": [{"ip": "10.10.0.9", "port": 80}]}]
            }))
            .unwrap(),
        )
    };
    ovn.env
        .cluster
        .create(&lb("web", "10.10.0.50"))
        .await
        .unwrap();
    let mut s = ovn.syncer();
    assert!(matches!(
        s.sync().await,
        SyncOutcome::Synced { failed: 0, .. }
    ));
    let lbs = ovn.nb.rows("Load_Balancer");
    assert_eq!(lbs.len(), 1);
    let uuid = lbs[0]["_uuid"].clone();
    for table in ["Logical_Switch", "Logical_Router"] {
        let rows = ovn.nb.rows(table);
        assert_eq!(rows[0]["load_balancer"], uuid, "{table}: {rows:?}");
    }
    drop(s);

    let sync = OvnSync::spawn(ovn.env.cluster.clone(), ovn.config());
    tokio::time::sleep(Duration::from_millis(300)).await;
    ovn.env
        .cluster
        .create(&lb("api", "10.10.0.51"))
        .await
        .unwrap();
    let mut seen = false;
    for _ in 0..100 {
        if ovn.nb.rows("Load_Balancer").len() == 2 {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen);
    sync.shutdown().await;
}

/// 리뷰 1 I4: SDN 경보의 관찰 읽기는 OvnSync 루프 밖에서 돈다 — SB가 답하지 않아도(꺼진 중앙에 막힌 연결) NB 쓰기는 곧바로
/// 이어진다. 관찰은 NB가 답하는 대로 읽어 두고, SB가 다시 답하면 이어 쓴다.
#[tokio::test]
async fn a_silent_sb_does_not_delay_nb_writes() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    // 붙기는 받지만 아무 답도 하지 않는 SB
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let mut conns = Vec::new();
        while let Ok((c, _)) = silent.accept().await {
            conns.push(c);
        }
    });
    let mut config = ovn.config();
    config.sb_port = silent_port;
    config.timeout = Duration::from_secs(30);
    config.observe_every = Duration::from_millis(50);
    let observations = config.observations.clone();
    let sync = OvnSync::spawn(ovn.env.cluster.clone(), config);
    let started = std::time::Instant::now();
    let a = ovn.network("a").await;
    let want = format!("opl-{}", a.id);
    while ovn.switches() != vec![want.clone()] {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            ovn.switches()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 관찰 하나가 막혀도(SB 2초 제한) NB 쓰기는 그것을 기다리지 않는다.
    let b = ovn.network("b").await;
    let again = std::time::Instant::now();
    while !ovn.switches().contains(&format!("opl-{}", b.id)) {
        assert!(
            again.elapsed() < Duration::from_secs(2),
            "the NB write waited for the observation"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(observations.get().is_none(), "SB never answered");
    sync.shutdown().await;
    held.abort();
}

/// 관찰 Task는 이어 쓰는 연결로 `observe_every`마다 읽어 둔다(리뷰 1 I4 — 차례마다 새로 붙지 않는다).
#[tokio::test]
async fn observations_are_read_over_kept_connections() {
    let ovn = Ovn::start().await;
    ovn.sdn(SdnPhase::Ready).await;
    let mut config = ovn.config();
    config.observe_every = Duration::from_millis(30);
    let observations = config.observations.clone();
    let sync = OvnSync::spawn(ovn.env.cluster.clone(), config);
    let started = std::time::Instant::now();
    let mut seen = std::collections::BTreeSet::new();
    while seen.len() < 4 {
        if let Some(o) = observations.get() {
            assert_eq!(o.term, term(&ovn.env));
            seen.insert(o.at);
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{seen:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    sync.shutdown().await;
    // SB에는 관찰 연결 하나만 붙었다(읽을 때마다 새로 붙지 않음).
    assert_eq!(ovn.sb.with(|s| s.accepted), 1);
}
