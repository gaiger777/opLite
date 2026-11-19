//! Ceph 자동 구성(계획 56)의 API `/ceph`와 노드 디스크 목록. 노드의 명령은 가짜 실행기가 받고 파일은 임시
//! 디렉터리에 쓴다. Task 실행기는 멈춰 두어(만든 Task가 끝나지 않음) API가 만든 Task와 리소스만 본다.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Client, Env, WAIT};
use opl_api::{AppState, AuditConfig, AuditLog};
use opl_driver::ceph::{CephHost, CephOptions, CephPaths, CmdOutput, FakeRunner};
use opl_types::audit::AuditQuery;
use opl_types::ceph::{CEPH_NAME, CephMon, CephPhase, CephSpec, CephStatus, valid_key};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const LSBLK: &[u8] = include_bytes!("../../opl-driver/src/ceph/fixtures/lsblk-root.json");
const PVS: &[u8] = include_bytes!("../../opl-driver/src/ceph/fixtures/pvs.json");

/// 상태 보고서의 명령 여덟(앞부분)과 ceph 19.2 고정 출력
const REPORT: [(&str, &[u8]); 8] = [
    (
        "ceph --connect-timeout 10 status",
        include_bytes!("../../opl-driver/src/ceph/fixtures/status-ok.json"),
    ),
    (
        "ceph --connect-timeout 10 quorum_status",
        include_bytes!("../../opl-driver/src/ceph/fixtures/quorum-status.json"),
    ),
    (
        "ceph --connect-timeout 10 mgr dump",
        include_bytes!("../../opl-driver/src/ceph/fixtures/mgr-dump.json"),
    ),
    (
        "ceph --connect-timeout 10 osd tree",
        include_bytes!("../../opl-driver/src/ceph/fixtures/osd-tree.json"),
    ),
    (
        "ceph --connect-timeout 10 osd df",
        include_bytes!("../../opl-driver/src/ceph/fixtures/osd-df.json"),
    ),
    (
        "ceph --connect-timeout 10 df",
        include_bytes!("../../opl-driver/src/ceph/fixtures/df.json"),
    ),
    (
        "ceph --connect-timeout 10 osd metadata",
        include_bytes!("../../opl-driver/src/ceph/fixtures/osd-metadata.json"),
    ),
    (
        "ceph --connect-timeout 10 osd pool ls detail",
        include_bytes!("../../opl-driver/src/ceph/fixtures/pool-ls-detail.json"),
    ),
];

/// OSD 하나가 up인 호스트 하나의 `ceph osd tree`
const ONE_HOST_TREE: &str = r#"{"nodes":[{"id":-1,"name":"default","type":"root","children":[-3]},{"id":-3,"name":"node1","type":"host","children":[0]},{"id":0,"device_class":"hdd","name":"osd.0","type":"osd","crush_weight":0.0098,"status":"up","reweight":1}],"stray":[]}"#;

/// 가짜 Ceph 단계를 쓰는 API(같은 노드의 둘째 API 서버). 그 감사 로그도 둔다.
struct Ceph {
    env: Env,
    _dir: TempDir,
    fake: Arc<FakeRunner>,
    host: Arc<CephHost>,
    admin: Client,
    audit: Arc<AuditLog>,
}

impl Ceph {
    async fn start() -> Self {
        let mut env = Env::start().await;
        env.stop_runner().await;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let fake = Arc::new(FakeRunner::new());
        let paths = CephPaths {
            etc_ceph: root.join("etc/ceph"),
            var_lib_ceph: root.join("var/lib/ceph"),
            client_dir: root.join("etc/opl/ceph"),
            by_id: root.join("dev/disk/by-id"),
        };
        let options = CephOptions {
            any_os: true,
            ..CephOptions::default()
        };
        let host = Arc::new(CephHost::new(fake.clone(), paths, options));
        let audit = Arc::new(
            AuditLog::open(AuditConfig {
                dir: root.join("audit"),
                node: "node1".into(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap(),
        );
        let base = serve(&env, |s| {
            s.ceph = Some(host.clone());
            s.audit = audit.clone();
        })
        .await;
        let mut admin = env.admin().await;
        admin.base = base;
        Self {
            env,
            _dir: dir,
            fake,
            host,
            admin,
            audit,
        }
    }

    /// `client`가 같은 토큰으로 이 API에 묻게 한다.
    fn here(&self, mut client: Client) -> Client {
        client.base = self.admin.base.clone();
        client
    }

    fn ceph(&self) -> Option<Resource<CephSpec, CephStatus>> {
        self.env.cluster.get_by_name::<CephSpec>(CEPH_NAME).unwrap()
    }

    /// `Ceph` 리소스를 상태 `status`로 만든다(public 망 127.0.0.0/8).
    async fn create(&self, status: CephStatus) -> Uuid {
        let mut ceph = Resource::new(
            CEPH_NAME,
            CephSpec {
                fsid: Uuid::new_v4(),
                public_network: "127.0.0.0/8".parse().unwrap(),
                cluster_network: None,
                keys: opl_types::ceph::CephKeys {
                    mon: "AQmon".into(),
                    admin: "AQadmin".into(),
                    bootstrap_osd: "AQosd".into(),
                    client: "AQclient".into(),
                },
            },
        );
        ceph.status = status;
        self.env.cluster.create(&ceph).await.unwrap();
        ceph.id
    }

    async fn set_status(&self, status: CephStatus) {
        let id = self.ceph().unwrap().id;
        self.env
            .cluster
            .update_status::<CephSpec>(id, &status)
            .await
            .unwrap();
    }

    /// 이 노드에 mon·mgr이 있는 준비된 클러스터
    fn ready(&self) -> CephStatus {
        CephStatus {
            phase: CephPhase::Ready,
            mons: vec![mon(self.env.node, "node1", "127.0.0.1")],
            mgrs: vec![self.env.node],
            release: Some("19.2.3 squid".into()),
            init_node: Some(self.env.node),
            ..CephStatus::default()
        }
    }

    /// 상태 보고서의 명령 여덟에 고정 출력으로 답한다.
    fn report(&self) {
        for (prefix, out) in REPORT {
            self.fake.reply(prefix, CmdOutput::ok(out));
        }
    }

    /// 상태 보고서의 monmap을 mon `names`(정족수는 `status-ok.json`의 node1·node2·node3)로 한다.
    fn monmap(&self, names: &[&str]) {
        let mons: Vec<Value> = names
            .iter()
            .map(|n| json!({ "name": n, "public_addrs": { "addrvec": [{ "type": "v2", "addr": "127.0.0.1:3300" }] } }))
            .collect();
        self.fake.reply(
            "ceph --connect-timeout 10 quorum_status",
            CmdOutput::ok(json!({ "quorum_names": names, "monmap": { "mons": mons } }).to_string()),
        );
    }

    /// `prefix`로 시작하는 명령 줄의 수
    fn count(&self, prefix: &str) -> usize {
        self.fake
            .calls()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }

    /// 이 API의 감사 로그에서 동작 `action`이 202로 끝난 가장 새 항목의 설명(적힐 때까지 기다린다)
    async fn audit_detail(&self, action: &str) -> Option<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let entries = self
                .audit
                .read(AuditQuery {
                    since: 0,
                    until: u64::MAX,
                    user: None,
                    action: Some(action.into()),
                    limit: 100,
                })
                .await
                .unwrap();
            if let Some(e) = entries
                .into_iter()
                .find(|e| e.action == action && e.status == 202)
            {
                return e.detail;
            }
            assert!(Instant::now() < deadline, "no audit entry for {action}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn mon(node: Uuid, name: &str, addr: &str) -> CephMon {
    CephMon {
        node,
        name: name.into(),
        addr: addr.parse().unwrap(),
    }
}

/// 같은 노드의 API를 `tweak`한 상태로 하나 더 띄우고 그 주소를 돌려준다.
async fn serve(env: &Env, tweak: impl FnOnce(&mut AppState)) -> String {
    let mut state = AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads2"),
    );
    tweak(&mut state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    base
}

/// 이름이 `name`인 노드 리소스(온라인, 모든 기능)를 만든다.
async fn add_node(env: &Env, name: &str, address: &str, witness: bool, online: bool) -> Uuid {
    let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            host_resources: None,
            address: address.into(),
            maintenance: false,
            witness,
            roles: None,
        },
    );
    r.status.online = online;
    r.status.version = Some(opl_types::version::BUILD.into());
    r.status.features = opl_types::node::FEATURES
        .iter()
        .map(|f| f.to_string())
        .collect();
    env.cluster.create(&r).await.unwrap();
    r.id
}

fn task(cluster: &opl_cluster::Cluster, body: &Value) -> Task {
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    cluster.get::<TaskSpec>(id).unwrap().unwrap()
}

fn message(body: &Value) -> &str {
    body["message"].as_str().unwrap_or_default()
}

/// 부트스트랩: 키 넷과 fsid를 만들어 `bootstrapping` 리소스와 `ceph-init` Task를 만든다. 키는 응답·watch 어디에도
/// 나가지 않는다. 같은 노드로 다시 부르면 끝난 Task 뒤에 새 Task만 만들고, 다른 노드면 거절한다.
#[tokio::test]
async fn init_creates_the_cluster_and_keeps_the_keys_out_of_responses() {
    let mut c = Ceph::start().await;
    let (status, body) = c
        .admin
        .post(
            "/ceph",
            json!({ "node": "node1", "public_network": "127.0.0.0/8" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let first = task(&c.env.cluster, &body);
    let ceph = c.ceph().unwrap();
    assert_eq!(
        (first.spec.op.clone(), first.spec.target, first.spec.node),
        (TaskOp::CephInit, ceph.id, c.env.node)
    );
    assert_eq!(ceph.status.phase, CephPhase::Bootstrapping);
    assert_eq!(ceph.status.init_node, Some(c.env.node));
    assert_eq!(ceph.spec.public_network.to_string(), "127.0.0.0/8");
    let keys = &ceph.spec.keys;
    let all = [&keys.mon, &keys.admin, &keys.bootstrap_osd, &keys.client];
    for key in all {
        assert!(valid_key(key), "{key}");
    }
    assert_eq!(
        c.audit_detail("POST /ceph").await.as_deref(),
        Some("ceph init on node1 (public 127.0.0.0/8)")
    );

    let viewer = c.here(c.env.user("viewer", "viewer").await);
    let (status, view) = viewer.get("/ceph").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["fsid"], ceph.spec.fsid.to_string());
    assert_eq!(view["phase"], "bootstrapping");
    assert_eq!(view["tasks"], json!([first.id]));
    assert!(
        view["managed_conf"]
            .as_str()
            .unwrap()
            .ends_with("etc/opl/ceph/ceph.conf"),
        "{view}"
    );
    let text = view.to_string();
    for key in all {
        assert!(!text.contains(key.as_str()), "{text}");
    }
    let (status, body) = viewer.get("/watch?kinds=ceph&since=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 부트스트랩 Task가 끝나지 않았다.
    let again = json!({ "node": "node1" });
    let (status, body) = c.admin.post("/ceph", again.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_busy");
    assert_eq!(
        message(&body),
        format!("another Ceph task is running (task {})", first.id)
    );
    // 이 실행기는 Ceph Task를 돌리지 못해 실패로 끝낸다. 그 뒤 다시 부르면 새 Task만 만든다.
    c.env.start_runner().await;
    c.admin.wait_task(&json!({ "id": first.id })).await;
    let (status, body) = c.admin.post("/ceph", again).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let second = task(&c.env.cluster, &body);
    assert_ne!(second.id, first.id);
    assert_eq!(second.spec.op, TaskOp::CephInit);
    assert_eq!(c.ceph().unwrap().spec.fsid, ceph.spec.fsid);
    c.admin.wait_task(&body).await;
    // 만든 뒤 망은 바뀌지 않는다.
    let (status, body) = c
        .admin
        .post(
            "/ceph",
            json!({ "node": "node1", "public_network": "10.0.0.0/24" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");

    // 다른 노드에서는 다시 하지 않는다.
    add_node(&c.env, "node2", "127.0.0.2:1", false, true).await;
    let (status, body) = c.admin.post("/ceph", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");
    assert_eq!(
        message(&body),
        "Ceph is being set up on node \"node1\"; retry there"
    );
    // 준비된 클러스터는 다시 세우지 않는다.
    c.set_status(c.ready()).await;
    let (status, body) = c.admin.post("/ceph", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");
}

/// 부트스트랩 요청의 확인: 권한, 노드, 위트니스, 망, 온라인, 기능
#[tokio::test]
async fn init_checks_the_request() {
    let c = Ceph::start().await;
    add_node(&c.env, "witness", "127.0.0.3:1", true, true).await;
    add_node(&c.env, "down", "127.0.0.4:1", false, false).await;
    let operator = c.here(c.env.user("op", "operator").await);
    let (status, _) = operator.post("/ceph", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let cases = [
        (json!({ "node": "nope" }), "node \"nope\" not found"),
        (
            json!({ "node": "witness" }),
            "node \"witness\" is a witness and cannot run Ceph daemons",
        ),
        (
            json!({ "node": "node1", "public_network": "10.0.0.1/24" }),
            "",
        ),
        (
            json!({ "node": "node1", "public_network": "10.9.0.0/24" }),
            "node \"node1\" has no address in the Ceph public network 10.9.0.0/24",
        ),
        // 127.0.0.1을 가진 인터페이스가 없다.
        (
            json!({ "node": "node1" }),
            "give public_network: cannot tell the network of node \"node1\" from its address \"127.0.0.1:0\"",
        ),
        (
            json!({ "node": "node1", "public_network": "127.0.0.0/8", "cluster_network": "x" }),
            "",
        ),
    ];
    for (req, want) in cases {
        let (status, body) = c.admin.post("/ceph", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{req} {body}");
        if !want.is_empty() {
            assert_eq!(message(&body), want, "{req}");
        }
    }
    let (status, body) = c.admin.post("/ceph", json!({ "node": "down" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_offline");
    assert!(c.ceph().is_none());

    // 이 기능을 모르는 노드가 있다.
    let mut old: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            host_resources: None,
            address: "127.0.0.5:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    old.status.online = true;
    c.env.cluster.create(&old).await.unwrap();
    let (status, body) = c
        .admin
        .post(
            "/ceph",
            json!({ "node": "node1", "public_network": "127.0.0.0/8" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert!(c.ceph().is_none());
}

/// mon·mgr 더하기: 준비 전 거절, 이미 그 역할, mon 상한(더하는 중인 mon 포함), Ceph의 monmap에 Orbivisor가 모르는
/// mon, 끝나지 않은 mon·mgr Task(Raft에 잡은 자리)
#[tokio::test]
async fn adding_monitors_and_managers() {
    let c = Ceph::start().await;
    c.report();
    c.monmap(&["node1"]);
    let node2 = add_node(&c.env, "node2", "127.0.0.2:1", false, true).await;
    add_node(&c.env, "node3", "127.0.0.3:1", false, true).await;
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let id = c.create(CephStatus::default()).await;
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_not_ready");

    c.set_status(c.ready()).await;
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");
    assert_eq!(message(&body), "node \"node1\" already runs a monitor");
    let (status, body) = c.admin.post("/ceph/mgrs", json!({ "node": "node1" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(message(&body), "node \"node1\" already runs a manager");

    // mon이 다섯이면 더하지 않는다.
    let mut full = c.ready();
    for i in 2..=5u128 {
        full.mons
            .push(mon(Uuid::from_u128(i), &format!("m{i}"), "127.0.0.9"));
    }
    c.set_status(full).await;
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_limit");
    assert_eq!(
        message(&body),
        "a Ceph cluster here can have at most 5 monitors"
    );

    // Ceph의 monmap에 Orbivisor가 모르는 mon(앞서 실패한 더하기가 남긴 것 등)이 있다.
    c.set_status(c.ready()).await;
    c.monmap(&["node1", "node7"]);
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_busy");
    assert_eq!(
        message(&body),
        "monitor node7 is in the Ceph monitor map but not in Orbivisor; bring it back or remove \
         it by hand with: ceph mon remove node7"
    );
    // 더하려는 노드의 mon이 이미 monmap에 있으면(앞 Task가 멈춤) 다시 한다.
    c.monmap(&["node1", "node2"]);
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let t = task(&c.env.cluster, &body);
    assert_eq!(
        (t.spec.op.clone(), t.spec.target, t.spec.node),
        (TaskOp::CephMonAdd, id, node2)
    );
    assert_eq!(
        c.ceph().unwrap().status.pending,
        Some(opl_types::ceph::CephPending {
            op: TaskOp::CephMonAdd,
            node: node2,
            task: t.id
        })
    );
    assert_eq!(
        c.audit_detail("POST /ceph/mons").await.as_deref(),
        Some("ceph mon add node2")
    );
    // mon Task가 끝나지 않았다(실행기가 멈춰 있다).
    let (status, body) = c.admin.post("/ceph/mgrs", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_busy");
    let (status, view) = c.admin.get("/ceph").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["tasks"], json!([t.id]));
    // mon 넷에 하나를 더하는 중이면 다섯째로 센다.
    let mut four = c.ceph().unwrap().status;
    for i in 2..=4u128 {
        four.mons
            .push(mon(Uuid::from_u128(i), &format!("m{i}"), "127.0.0.9"));
    }
    c.set_status(four).await;
    let (status, body) = c.admin.post("/ceph/mons", json!({ "node": "node3" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_limit");

    // 자리를 잡았지만 아직 Task가 없으면(다른 노드의 API가 만드는 중) 바쁘다. 오래된 것(그 API가 멈춤)은 넘겨받는다.
    let mut held = c.ready();
    held.pending = Some(opl_types::ceph::CephPending {
        op: TaskOp::CephMgrAdd,
        node: c.env.node,
        task: Uuid::now_v7(),
    });
    c.set_status(held.clone()).await;
    c.env.cluster.delete::<TaskSpec>(t.id, None).await.unwrap();
    let (status, body) = c.admin.post("/ceph/mgrs", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_busy");
    let old = Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        1_700_000_000,
        0,
    ));
    held.pending.as_mut().unwrap().task = old;
    c.set_status(held).await;
    let (status, body) = c.admin.post("/ceph/mgrs", json!({ "node": "node2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // 끝나지 않은 Task의 노드가 꺼져 있으면 그것을 알린다.
    for t in c.env.cluster.list::<TaskSpec>().unwrap().1 {
        c.env.cluster.delete::<TaskSpec>(t.id, None).await.unwrap();
    }
    let dead = add_node(&c.env, "dead", "127.0.0.4:1", false, false).await;
    let mut stuck: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::CephMgrAdd,
            target: id,
            node: dead,
        },
    );
    stuck.name = stuck.id.to_string();
    c.env.cluster.create(&stuck).await.unwrap();
    c.set_status(c.ready()).await;
    let (status, body) = c.admin.post("/ceph/mgrs", json!({ "node": "node3" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_busy");
    assert!(
        message(&body).ends_with(
            "; node \"dead\" is offline; bring it back or remove it with `cluster remove-node`"
        ),
        "{body}"
    );

    // admin만 더한다.
    let operator = c.here(c.env.user("op", "operator").await);
    let (status, _) = operator
        .post("/ceph/mgrs", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = operator.get("/ceph").await;
    assert_eq!(status, StatusCode::OK);
}

/// OSD 더하기: 노드의 디스크 목록으로 먼저 확인하고, Task에는 by-id 경로를 적는다.
#[tokio::test]
async fn adding_osds_checks_the_disk_first() {
    let c = Ceph::start().await;
    c.fake.reply("lsblk", CmdOutput::ok(LSBLK));
    c.fake.reply("pvs", CmdOutput::ok(PVS));
    c.fake
        .reply("lvs", CmdOutput::ok(r#"{"report":[{"lv":[]}]}"#));
    c.fake
        .reply("ceph-volume lvm list --format json", CmdOutput::ok("{}"));
    let by_id = c.host.paths().by_id.clone();
    std::fs::create_dir_all(&by_id).unwrap();
    let wwn = by_id.join("wwn-0x5000c500a1b2c3d4");
    std::os::unix::fs::symlink("/dev/sdc", &wwn).unwrap();
    let wwn = wwn.to_string_lossy().into_owned();
    let id = c.create(c.ready()).await;

    let (status, disks) = c.admin.get("/nodes/node1/disks").await;
    assert_eq!(status, StatusCode::OK, "{disks}");
    let states: Vec<(&str, &str)> = disks
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["path"].as_str().unwrap(), d["state"].as_str().unwrap()))
        .collect();
    assert!(states.contains(&("/dev/sdb", "available")), "{disks}");
    assert!(states.contains(&("/dev/sdc", "wipeable")), "{disks}");
    assert!(states.contains(&("/dev/sda", "in-use")), "{disks}");
    // Ceph 단계가 없는 API(위트니스처럼)는 이 노드의 디스크를 읽지 못한다.
    let (status, body) = c.env.admin().await.get("/nodes/node1/disks").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    let post = |device: &str, wipe: bool| {
        c.admin.post(
            "/ceph/osds",
            json!({ "node": "node1", "device": device, "wipe": wipe }),
        )
    };
    let (status, body) = post("/dev/sdz", false).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(message(&body), "node \"node1\" has no disk \"/dev/sdz\"");
    let (status, body) = post("/dev/sda", true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "disk_in_use");
    assert_eq!(
        message(&body),
        "/dev/sda cannot hold an OSD: system disk (/ is on it)"
    );
    let (status, body) = post("/dev/sdc", false).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "disk_not_empty");
    assert!(
        message(&body).starts_with("/dev/sdc is not empty (")
            && message(&body).ends_with("); pass wipe to erase it"),
        "{body}"
    );
    assert_eq!(c.count("ceph-volume lvm zap"), 0);

    let (status, body) = post("/dev/sdc", true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let t = task(&c.env.cluster, &body);
    assert_eq!(
        (t.spec.op, t.spec.target, t.spec.node),
        (
            TaskOp::CephOsdAdd {
                device: wwn.clone(),
                wipe: true
            },
            id,
            c.env.node
        )
    );
    assert_eq!(
        c.audit_detail("POST /ceph/osds").await.as_deref(),
        Some("ceph osd add node1 /dev/sdc (wipe)")
    );
    // by-id 경로로 골라도 된다. by-id가 없는 디스크는 장치 경로를 적는다.
    let (status, body) = post(&wwn, true).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = post("/dev/sdb", false).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        task(&c.env.cluster, &body).spec.op,
        TaskOp::CephOsdAdd {
            device: "/dev/sdb".into(),
            wipe: false
        }
    );
}

/// 풀 만들기: 크기는 OSD가 up인 호스트 수로 확인하고, 풀 Task는 mon 노드에서 돈다.
#[tokio::test]
async fn creating_a_pool_checks_the_size_against_the_hosts() {
    let c = Ceph::start().await;
    c.report();
    c.fake.reply(
        "ceph --connect-timeout 10 osd tree",
        CmdOutput::ok(ONE_HOST_TREE),
    );
    let id = c.create(c.ready()).await;
    let cases = [
        (
            json!({ "name": "vmpool" }),
            StatusCode::BAD_REQUEST,
            "only one host has OSDs; pass size 1 to keep a single copy (no redundancy)",
        ),
        (
            json!({ "name": "vmpool", "size": 2 }),
            StatusCode::BAD_REQUEST,
            "size 2 needs OSDs on at least 2 hosts (now 1)",
        ),
        (
            json!({ "name": "vmpool", "size": 4 }),
            StatusCode::BAD_REQUEST,
            "size must be 1, 2 or 3",
        ),
        (
            json!({ "name": "vmpool", "pool": "bad/name", "size": 1 }),
            StatusCode::BAD_REQUEST,
            "",
        ),
    ];
    for (req, code, want) in cases {
        let (status, body) = c.admin.post("/ceph/pools", req.clone()).await;
        assert_eq!(status, code, "{req} {body}");
        if !want.is_empty() {
            assert_eq!(message(&body), want, "{req}");
        }
    }
    // 다른 Orbivisor 풀이 이미 그 Ceph 풀을 쓴다(관리 conf가 아니어도).
    let used = Resource::new(
        "ext",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: "used".into(),
                namespace: None,
                user: None,
                conf: Some("/etc/ceph/ceph.conf".into()),
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    c.env.cluster.create(&used).await.unwrap();
    let (status, body) = c
        .admin
        .post(
            "/ceph/pools",
            json!({ "name": "vms2", "pool": "used", "size": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_exists");
    assert_eq!(
        message(&body),
        "storage pool \"ext\" already uses the Ceph pool used; choose another Ceph pool name"
    );

    // 같은 이름의 Orbivisor 풀이 있다.
    let taken = Resource::new(
        "taken",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/var/lib/opl/taken".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    c.env.cluster.create(&taken).await.unwrap();
    let (status, body) = c
        .admin
        .post("/ceph/pools", json!({ "name": "taken", "size": 1 }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_exists");

    let (status, body) = c
        .admin
        .post(
            "/ceph/pools",
            json!({ "name": "vms", "pool": "vmpool", "size": 1 }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let t = task(&c.env.cluster, &body);
    assert_eq!(
        (t.spec.op, t.spec.target, t.spec.node),
        (
            TaskOp::CephPoolCreate {
                name: "vms".into(),
                pool: "vmpool".into(),
                size: 1
            },
            id,
            c.env.node
        )
    );
    assert_eq!(
        c.audit_detail("POST /ceph/pools").await.as_deref(),
        Some("ceph pool create vms size 1")
    );
    // 호스트 수는 잠깐 둔 상태에서 읽었다(명령 여덟을 한 번만).
    assert_eq!(c.count("ceph --connect-timeout 10 osd tree"), 1);

    // OSD가 없다.
    let c = Ceph::start().await;
    c.report();
    c.fake.reply(
        "ceph --connect-timeout 10 osd tree",
        CmdOutput::ok(r#"{"nodes":[],"stray":[]}"#),
    );
    c.create(c.ready()).await;
    let (status, body) = c
        .admin
        .post("/ceph/pools", json!({ "name": "vms", "size": 1 }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_not_ready");
    assert_eq!(message(&body), "add OSDs first");
}

/// 상태: mon 노드에서 읽어 잠깐 두고, 못 읽으면 마지막 것을 `stale`로, 그것도 없으면 503이다.
#[tokio::test]
async fn the_status_is_read_from_a_monitor_and_cached() {
    let c = Ceph::start().await;
    c.report();
    let (status, body) = c.admin.get("/ceph/status").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    c.create(c.ready()).await;
    // 관리 conf를 쓰는 RBD 풀은 그 Ceph 풀로 이어진다.
    let conf = c.host.client_conf().to_string_lossy().into_owned();
    let vms = Resource::new(
        "vms",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: "vmpool".into(),
                namespace: None,
                user: Some("orbivisor".into()),
                conf: Some(conf),
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    c.env.cluster.create(&vms).await.unwrap();

    let viewer = c.here(c.env.user("viewer", "viewer").await);
    let (status, report) = viewer.get("/ceph/status").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["health"]["status"], "HEALTH_OK");
    assert_eq!(report["osd_hosts_up"], 2);
    assert_eq!(report["via"], c.env.node.to_string());
    assert!(report["read_at"].as_u64().unwrap() > 0);
    assert!(report.get("stale").is_none(), "{report}");
    let pool = report["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "vmpool")
        .unwrap();
    assert_eq!(pool["orbivisor_pool"], "vms");
    assert_eq!(c.fake.calls().len(), 8);

    // 잠깐 둔 것을 돌려준다. `refresh`면 새로 읽는다.
    let (status, _) = viewer.get("/ceph/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(c.fake.calls().len(), 8);
    let (status, _) = viewer.get("/ceph/status?refresh=true").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(c.fake.calls().len(), 16);

    // 지금 읽지 못하면 마지막 보고서를 `stale`로 준다.
    c.fake.reply(
        "ceph --connect-timeout 10 osd tree",
        CmdOutput::fail(
            1,
            "[errno 110] RADOS timed out (error connecting to the cluster)\n",
        ),
    );
    let (status, stale) = viewer.get("/ceph/status?refresh=true").await;
    assert_eq!(status, StatusCode::OK, "{stale}");
    assert_eq!(stale["stale"], true);
    assert!(
        stale["error"].as_str().unwrap().contains("RADOS timed out"),
        "{stale}"
    );
    assert_eq!(stale["health"]["status"], "HEALTH_OK");

    // 처음부터 읽지 못하면 503이다.
    let base = serve(&c.env, |s| {
        s.ceph = Some(c.host.clone());
        s.ceph_status_ttl = Duration::from_secs(600);
    })
    .await;
    let mut other = viewer.clone();
    other.base = base;
    let (status, body) = other.get("/ceph/status").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["code"], "ceph_unreachable");
    assert!(
        message(&body).starts_with("cannot read the Ceph status from any monitor node: ")
            && message(&body).contains("RADOS timed out")
            && message(&body).contains("check that ports 3300 and 6789 are open"),
        "{body}"
    );
    // 실패도 잠깐 둔다: 그동안 다시 묻지 않고 같은 답이다. `refresh`면 다시 읽는다.
    c.report();
    let before = c.fake.calls().len();
    let (status, body) = other.get("/ceph/status").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(c.fake.calls().len(), before);
    let (status, body) = other.get("/ceph/status?refresh=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(c.fake.calls().len(), before + 8);
    // 그 뒤 실패하면 잠깐 동안 마지막 보고서를 `stale`로 준다(다시 읽지 않는다).
    c.fake.reply(
        "ceph --connect-timeout 10 osd tree",
        CmdOutput::fail(1, "[errno 110] RADOS timed out\n"),
    );
    let (status, body) = other.get("/ceph/status?refresh=true").await;
    assert_eq!(
        (status, &body["stale"]),
        (StatusCode::OK, &json!(true)),
        "{body}"
    );
    let (status, body) = other.get("/ceph/status").await;
    assert_eq!(
        (status, &body["stale"]),
        (StatusCode::OK, &json!(true)),
        "{body}"
    );
    assert_eq!(c.fake.calls().len(), before + 16);
}

/// 클라이언트 꾸러미 설치: 위트니스는 거절한다.
#[tokio::test]
async fn installing_client_packages() {
    let c = Ceph::start().await;
    add_node(&c.env, "witness", "127.0.0.3:1", true, true).await;
    let id = c.create(c.ready()).await;
    let (status, body) = c
        .admin
        .post("/ceph/clients", json!({ "node": "witness" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = c
        .admin
        .post("/ceph/clients", json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let t = task(&c.env.cluster, &body);
    assert_eq!(
        (t.spec.op, t.spec.target, t.spec.node),
        (TaskOp::CephClientInstall, id, c.env.node)
    );
    assert_eq!(
        c.audit_detail("POST /ceph/clients").await.as_deref(),
        Some("ceph client install node1")
    );
}

/// 부트스트랩 노드가 지워졌거나 꺼져 있고 아직 mon이 없으면 다른 노드로 옮겨 다시 한다(같은 fsid·키, 버려진 옛
/// Task는 바쁨으로 보지 않는다). mon이 섰거나 그 노드가 살아 있으면 옮기지 않는다.
#[tokio::test]
async fn init_moves_off_a_dead_bootstrap_node() {
    let c = Ceph::start().await;
    let gone = add_node(&c.env, "gone", "127.0.0.4:1", false, false).await;
    let node2 = add_node(&c.env, "node2", "127.0.0.2:1", false, true).await;
    let id = c
        .create(CephStatus {
            init_node: Some(gone),
            ..CephStatus::default()
        })
        .await;
    // 옛 노드에서 돌다 만 부트스트랩 Task와 그 자리
    let mut old: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::CephInit,
            target: id,
            node: gone,
        },
    );
    old.name = old.id.to_string();
    c.env.cluster.create(&old).await.unwrap();
    let mut status = c.ceph().unwrap().status;
    status.pending = Some(opl_types::ceph::CephPending {
        op: TaskOp::CephInit,
        node: gone,
        task: old.id,
    });
    // mon이 이미 섰으면 옮기지 않는다.
    status.mons = vec![mon(gone, "gone", "127.0.0.4")];
    c.set_status(status.clone()).await;
    let (code, body) = c.admin.post("/ceph", json!({ "node": "node2" })).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");
    assert_eq!(
        message(&body),
        "Ceph is being set up on node \"gone\"; retry there"
    );

    status.mons.clear();
    c.set_status(status).await;
    let fsid = c.ceph().unwrap().spec.fsid;
    let (code, body) = c.admin.post("/ceph", json!({ "node": "node2" })).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    let t = task(&c.env.cluster, &body);
    assert_eq!((t.spec.op.clone(), t.spec.node), (TaskOp::CephInit, node2));
    let ceph = c.ceph().unwrap();
    assert_eq!(ceph.spec.fsid, fsid);
    assert_eq!(ceph.status.init_node, Some(node2));
    assert_eq!(ceph.status.pending.map(|p| p.task), Some(t.id));
    // 옛 노드의 Task는 실패로 끝낸다(돌아와도 시작하지 않는다).
    let abandoned = c.env.cluster.get::<TaskSpec>(old.id).unwrap().unwrap();
    assert_eq!(
        (abandoned.status.state, abandoned.status.error.as_deref()),
        (
            opl_types::task::TaskState::Failed,
            Some("the Ceph bootstrap was moved to node \"node2\"")
        )
    );
    assert_eq!(
        c.audit_detail("POST /ceph").await.as_deref(),
        Some("ceph init on node2 (public 127.0.0.0/8, retry, moved from gone)")
    );
    // 이제 node2에서 세우는 중이다(살아 있다).
    let (code, body) = c.admin.post("/ceph", json!({ "node": "node1" })).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");

    // 지워진 부트스트랩 노드
    let c = Ceph::start().await;
    c.create(CephStatus {
        init_node: Some(Uuid::now_v7()),
        ..CephStatus::default()
    })
    .await;
    let (code, body) = c.admin.post("/ceph", json!({ "node": "node1" })).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{body}");
    assert_eq!(c.ceph().unwrap().status.init_node, Some(c.env.node));

    // 준비된 클러스터의 mon을 모두 잃었으면(mon 노드를 모두 뺌) 다시 세우지 않는다.
    let c = Ceph::start().await;
    let fsid = {
        c.create(CephStatus {
            mons: vec![],
            ..c.ready()
        })
        .await;
        c.ceph().unwrap().spec.fsid
    };
    let (code, body) = c.admin.post("/ceph", json!({ "node": "node1" })).await;
    assert_eq!(code, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "ceph_exists");
    assert_eq!(
        message(&body),
        format!(
            "the Ceph cluster (fsid {fsid}) is set up but Orbivisor knows none of its monitors; \
             Orbivisor does not set it up again — recover a monitor by hand, or remove the Ceph \
             record from the cluster state (Raft) by hand before a new ceph init"
        )
    );
}

/// Ceph 데몬 노드를 클러스터에서 빼면 Orbivisor의 Ceph 기록에서도 빼고(Ceph는 그대로), 손으로 할 일을 경고로 준다.
/// Ceph와 상관없는 노드는 지금처럼 204다.
#[tokio::test]
async fn removing_a_ceph_node_forgets_its_daemons() {
    let c = Ceph::start().await;
    let dead = add_node(&c.env, "dead", "127.0.0.4:1", false, false).await;
    add_node(&c.env, "other", "127.0.0.5:1", false, false).await;
    let mut status = c.ready();
    status.mons.push(mon(dead, "dead", "127.0.0.4"));
    status.mgrs.push(dead);
    status.osd_nodes = vec![dead, c.env.node];
    c.create(status).await;

    let (code, body) = c.admin.delete("/nodes/dead").await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let warnings: Vec<&str> = body["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(
        warnings,
        [
            "node \"dead\" ran a Ceph monitor; remove it from the Ceph monitor map by hand with: \
             ceph mon remove dead",
            "node \"dead\" had Ceph OSDs; they stay in Ceph as down until you remove them by hand \
             (ceph osd purge ID --yes-i-really-mean-it)",
            "node \"dead\" ran a Ceph manager; remove its key by hand with: ceph auth del mgr.dead",
        ]
    );
    let status = c.ceph().unwrap().status;
    assert_eq!(status, {
        let mut want = c.ready();
        want.osd_nodes = vec![c.env.node];
        want
    });

    let (code, body) = c.admin.delete("/nodes/other").await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
}
