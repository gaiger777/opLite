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
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec {});
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
    let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec {});
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
