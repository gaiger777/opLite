//! 폴더·grant 권한의 시행(계획 62 설계 §8·§14): 폴더 사용자의 모든 목록이 자기 폴더만 돌려주고, 남의 객체는 404,
//! 권한 밖 동작은 403, `/watch` 거르기·옮긴 뒤 가짜 지우기·권한이 바뀌면 닫기, 지표·경보·감사의 루트 요구, 위임과
//! 상승 막기, 기능 게이트, 폴더·역할·그룹·grant·유효 권한 API.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{Client, Env, vm_request};
use futures_util::StreamExt;
use opl_types::backup::BackupSpec;
use opl_types::resource::Resource;
use opl_types::snapshot::SnapshotSpec;
use opl_types::storage::VolumeSpec;
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::VmSpec;
use opl_types::vm_import::{ImportSource, OvaSource, VmImportSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(client: &Client, path: &str) -> Socket {
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// 다음 JSON 메시지(시간 안에 없으면 None)
async fn next_json(ws: &mut Socket, wait: Duration) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(wait, ws.next()).await.ok()??.ok()?;
        if let Message::Text(text) = msg {
            return Some(serde_json::from_str(&text).unwrap());
        }
    }
}

fn names(list: &Value) -> BTreeSet<String> {
    list["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"))
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_string())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

async fn folder(admin: &Client, name: &str, kind: &str, parent: Option<&str>) -> String {
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": name, "kind": kind, "parent": parent }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

fn vm_in(name: &str, folder: Option<&str>) -> Value {
    let mut req = vm_request(name);
    if let Some(f) = folder {
        req["spec"]["folder"] = f.into();
    }
    req
}

async fn id_of(client: &Client, path: &str) -> String {
    let (status, body) = client.get(path).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body["id"].as_str().unwrap().to_string()
}

/// 관리자가 만든 세계: VM 트리 /teamA·/teamB, 네트워크 폴더 netA, 스토리지 폴더 stA. 그룹 team-a(alice)에
/// /teamA·netA·stA operator. alice의 전역 역할은 `none`이다.
struct World {
    env: Env,
    admin: Client,
    alice: Client,
    alice_id: String,
    group: String,
    a: String,
    b: String,
    net_a: String,
    st_a: String,
}

async fn world() -> World {
    let env = Env::start().await;
    let admin = env.admin().await;
    let a = folder(&admin, "teamA", "vm", None).await;
    let b = folder(&admin, "teamB", "vm", None).await;
    let net_a = folder(&admin, "netA", "network", None).await;
    let st_a = folder(&admin, "stA", "storage", None).await;
    for (name, f) in [
        ("a1", Some(&a)),
        ("a2", Some(&a)),
        ("b1", Some(&b)),
        ("r1", None),
    ] {
        let (status, body) = admin.post("/vms", vm_in(name, f.map(|s| s.as_str()))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    for (name, f) in [("lan-a", Some(&net_a)), ("lan-root", None)] {
        let (status, body) = admin
            .post(
                "/networks",
                json!({ "name": name, "spec": { "bridge": "vmbr0", "folder": f } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    for (name, f) in [("pool-a", Some(&st_a)), ("pool-root", None)] {
        let dir = env.dir.path().join(name);
        let (status, body) = admin
            .post(
                "/storage-pools",
                json!({ "name": name, "spec": { "backend": { "kind": "dir", "path": dir }, "folder": f } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "alice", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let alice_id = body["id"].as_str().unwrap().to_string();
    let (status, body) = admin
        .post(
            "/groups",
            json!({ "name": "team-a", "spec": { "members": [alice_id] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let group = body["id"].as_str().unwrap().to_string();
    for target in [&a, &net_a, &st_a] {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "group", "id": group },
                    "role": { "type": "builtin", "role": "operator" },
                    "target": { "type": "folder", "id": target },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let alice = env.login("alice", "password-1").await;
    World {
        env,
        admin,
        alice,
        alice_id,
        group,
        a,
        b,
        net_a,
        st_a,
    }
}

impl World {
    async fn vm_id(&self, name: &str) -> Uuid {
        Uuid::parse_str(&id_of(&self.admin, &format!("/vms/{name}")).await).unwrap()
    }

    /// 볼륨·스냅샷·백업·Task·가져오기를 저장소에 바로 쓴다(실행기 없이 목록 거르기만 본다).
    async fn seed_derived(&self) {
        let c = &self.env.cluster;
        let (a1, b1) = (self.vm_id("a1").await, self.vm_id("b1").await);
        for (name, pool) in [
            ("vol-pool-a", "pool-a"),
            ("vol-pool-root", "pool-root"),
            ("vol-a1", "pool-root"),
            ("vol-b1", "pool-root"),
        ] {
            let v: Resource<VolumeSpec, _> = Resource::new(
                name,
                serde_json::from_value(
                    json!({ "pool": pool, "size_bytes": 1048576, "format": "qcow2" }),
                )
                .unwrap(),
            );
            c.create(&v).await.unwrap();
        }
        // a1·b1의 디스크로 붙인다(볼륨은 붙은 VM을 따른다).
        for (vm, vol) in [("a1", "vol-a1"), ("b1", "vol-b1")] {
            let mut stored = c.get_by_name::<VmSpec>(vm).unwrap().unwrap();
            stored.spec.disks = vec![
                serde_json::from_value(json!({ "volume": vol, "bus": "virtio-blk" })).unwrap(),
            ];
            c.update_spec(stored.id, None, stored.labels, &stored.spec)
                .await
                .unwrap();
        }
        let vm_spec: VmSpec = serde_json::from_value(vm_request("x")["spec"].clone()).unwrap();
        for (name, vm) in [("snap-a1", a1), ("snap-b1", b1)] {
            let s: Resource<SnapshotSpec, _> = Resource::new(
                name,
                SnapshotSpec {
                    vm,
                    name: name.into(),
                    parent: None,
                    memory: false,
                    description: String::new(),
                    scheduled: false,
                    vm_spec: vm_spec.clone(),
                    hardware: None,
                },
            );
            c.create(&s).await.unwrap();
        }
        // 지운 VM의 백업은 만든 때의 폴더를 따른다.
        let mut gone_a = vm_spec.clone();
        gone_a.folder = Some(Uuid::parse_str(&self.a).unwrap());
        for (name, vm, spec) in [
            ("bk-a1", a1, vm_spec.clone()),
            ("bk-b1", b1, vm_spec.clone()),
            ("bk-gone-a", Uuid::now_v7(), gone_a),
            ("bk-gone-root", Uuid::now_v7(), vm_spec.clone()),
        ] {
            let b: Resource<BackupSpec, _> = Resource::new(
                name,
                BackupSpec {
                    vm,
                    vm_name: name.into(),
                    pool: "pool-root".into(),
                    vm_spec: spec,
                    job: None,
                    origin: None,
                    incremental: false,
                    max_chain: None,
                },
            );
            c.create(&b).await.unwrap();
        }
        let node = self.env.node;
        for (name, op, target) in [
            ("task-a1", TaskOp::VmStart, a1),
            ("task-b1", TaskOp::VmStart, b1),
            ("task-node", TaskOp::NodeUpdateCheck, node),
        ] {
            let t: Resource<TaskSpec, _> = Resource::new(name, TaskSpec { op, target, node });
            c.create(&t).await.unwrap();
        }
        let i: Resource<VmImportSpec, _> = Resource::new(
            "imp",
            VmImportSpec {
                node,
                source: ImportSource::Ova(OvaSource {
                    file_name: "x.ova".into(),
                    size_bytes: 1,
                }),
            },
        );
        c.create(&i).await.unwrap();
    }
}

/// 설계 §14: 폴더 사용자로 **모든 목록 경로**가 자기 것만 돌려준다(관리자는 전부).
#[tokio::test]
async fn every_list_shows_only_the_folders_of_a_folder_user() {
    let w = world().await;
    w.seed_derived().await;
    let alice = &w.alice;
    let admin = &w.admin;

    let (_, vms) = alice.get("/vms").await;
    assert_eq!(names(&vms), set(&["a1", "a2"]));
    assert_eq!(
        names(&admin.get("/vms").await.1),
        set(&["a1", "a2", "b1", "r1"])
    );

    assert_eq!(names(&alice.get("/networks").await.1), set(&["lan-a"]));
    assert_eq!(
        names(&alice.get("/storage-pools").await.1),
        set(&["pool-a"])
    );
    // 볼륨: 붙은 VM(a1) 또는 풀(pool-a)을 따른다.
    assert_eq!(
        names(&alice.get("/volumes").await.1),
        set(&["vol-a1", "vol-pool-a"])
    );
    assert_eq!(names(&alice.get("/snapshots").await.1), set(&["snap-a1"]));
    // 백업: 있는 VM은 지금 폴더, 지운 VM은 만든 때의 폴더
    assert_eq!(
        names(&alice.get("/backups").await.1),
        set(&["bk-a1", "bk-gone-a"])
    );
    // Task: VM Task는 VM을 따르고, 노드 Task는 루트 `cluster.read`다.
    assert_eq!(names(&alice.get("/tasks").await.1), set(&["task-a1"]));
    assert!(names(&admin.get("/tasks").await.1).contains("task-node"));
    // 가져오기는 루트 객체다.
    assert_eq!(names(&alice.get("/vm-imports").await.1), set(&[]));
    assert_eq!(names(&admin.get("/vm-imports").await.1), set(&["imp"]));
    // 폴더: 받은 폴더만
    let (_, folders) = alice.get("/folders").await;
    let ids: BTreeSet<String> = folders["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        [w.a.clone(), w.net_a.clone(), w.st_a.clone()]
            .into_iter()
            .collect()
    );
    let (_, vm_folders) = alice.get("/folders?kind=vm").await;
    assert_eq!(vm_folders["items"].as_array().unwrap().len(), 1);
    // grant: 자기 그룹의 것만(대상의 permission.modify가 없다)
    let (_, grants) = alice.get("/grants").await;
    assert_eq!(grants["items"].as_array().unwrap().len(), 3, "{grants}");
    assert!(
        grants["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["spec"]["principal"]["id"] == w.group.as_str())
    );
    // 경보·알림 기록: 볼 수 있는 것만(여기는 아무 경보도 없다), 403이 아니다.
    let (status, alerts) = alice.get("/alerts").await;
    assert_eq!(status, StatusCode::OK, "{alerts}");
    let (status, history) = alice.get("/alerts/history").await;
    assert_eq!(status, StatusCode::OK, "{history}");
    // 목록의 revision은 거르기와 관계없다.
    assert_eq!(
        vms["revision"].as_u64().is_some(),
        admin.get("/vms").await.1["revision"].as_u64().is_some()
    );

    // 루트 종류는 루트 권한(전역 역할 없음 → 403)
    for path in [
        "/nodes",
        "/settings",
        "/security-groups",
        "/backup-jobs",
        "/ha-groups",
        "/alert-rules",
        "/audit",
        "/metrics/cluster",
        "/metrics/nodes",
        "/cluster",
        "/groups",
        "/users",
    ] {
        assert_eq!(alice.get(path).await.0, StatusCode::FORBIDDEN, "{path}");
    }
}

/// 남의 객체는 404(없는 것과 같은 문구), 볼 수 있으나 권한 밖 동작은 403, 만들기는 폴더가 있어야 한다.
#[tokio::test]
async fn objects_out_of_view_are_404_and_missing_privileges_are_403() {
    let w = world().await;
    w.seed_derived().await;
    let alice = &w.alice;

    let (status, body) = alice.get("/vms/b1").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (_, missing) = alice.get("/vms/nope").await;
    assert_eq!(body["message"], "vm \"b1\" not found");
    assert_eq!(missing["code"], body["code"]);
    for path in ["/vms/b1/metrics", "/vms/b1/snapshots", "/vms/b1/guest"] {
        assert_eq!(alice.get(path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(alice.delete("/vms/b1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(alice.get("/volumes/vol-b1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(alice.get("/backups/bk-b1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(alice.get("/tasks/task-b1").await.0, StatusCode::NOT_FOUND);
    assert_eq!(alice.get("/tasks/task-node").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        alice.get("/networks/lan-root").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        alice.get(&format!("/folders/{}", w.b)).await.0,
        StatusCode::NOT_FOUND
    );

    // 자기 것은 읽는다.
    for path in [
        "/vms/a1",
        "/volumes/vol-a1",
        "/backups/bk-a1",
        "/tasks/task-a1",
        "/networks/lan-a",
        "/storage-pools/pool-a",
    ] {
        assert_eq!(alice.get(path).await.0, StatusCode::OK, "{path}");
    }
    // operator에는 네트워크·풀 설정과 폴더 관리가 없다(403).
    assert_eq!(
        alice.delete("/networks/lan-a").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        alice.delete("/storage-pools/pool-a").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = alice.delete(&format!("/folders/{}", w.a)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "forbidden");

    // 만들기: 폴더가 없으면 400 folder_required, 남의 폴더는 404, 자기 폴더는 된다.
    let (status, body) = alice.post("/vms", vm_in("new", None)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "folder_required");
    let (status, body) = alice.post("/vms", vm_in("new", Some(&w.b))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = alice.post("/vms", vm_in("new", Some(&w.a))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 볼 수 없는 네트워크에 NIC를 붙이지 못한다.
    let mut req = vm_in("new2", Some(&w.a));
    req["spec"]["nics"] = json!([{ "network": "lan-root", "model": "virtio" }]);
    assert_eq!(alice.post("/vms", req).await.0, StatusCode::NOT_FOUND);
    let mut req = vm_in("new2", Some(&w.a));
    req["spec"]["nics"] = json!([{ "network": "lan-a", "model": "virtio" }]);
    let (status, body) = alice.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // PUT은 폴더를 바꾸지 않는다(옮기기 API로).
    let (_, vm) = alice.get("/vms/a2").await;
    let mut spec = vm["spec"].clone();
    spec["folder"] = w.b.clone().into();
    let (status, body) = alice.put("/vms/a2", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("/members"),
        "{body}"
    );
    // 다른 칸은 바꾼다.
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = 512.into();
    let (status, body) = alice.put("/vms/a2", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 옮기기는 원본과 대상 둘 다 `inventory.move`. 관리자가 옮기면 폴더 사용자의 보기가 바뀐다.
#[tokio::test]
async fn moving_members_needs_both_sides_and_changes_the_view() {
    let w = world().await;
    let (status, body) = w
        .alice
        .post(
            &format!("/folders/{}/members", w.a),
            json!({ "items": [{ "kind": "vm", "key": "a1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .admin
        .post(
            &format!("/folders/{}/members", w.b),
            json!({ "items": [
                { "kind": "vm", "key": "a1" },
                { "kind": "network", "key": "lan-a" },
                { "kind": "vm", "key": "nope" },
            ] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["ok"], true, "{body}");
    assert_eq!(items[1]["ok"], false);
    assert_eq!(items[1]["error"]["code"], "invalid_request");
    assert_eq!(items[2]["error"]["code"], "not_found");
    assert_eq!(names(&w.alice.get("/vms").await.1), set(&["a2"]));
    // 루트로
    let (status, body) = w
        .admin
        .post(
            "/folders/root/members",
            json!({ "items": [{ "kind": "vm", "key": "a1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, vm) = w.admin.get("/vms/a1").await;
    assert!(vm["spec"].get("folder").is_none(), "{vm}");
    // 비지 않은 폴더는 지우지 못한다.
    let (status, body) = w.admin.delete(&format!("/folders/{}", w.a)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "folder_not_empty");
}

/// `/watch`: 볼 수 있는 것만, 옮겨져 안 보이면 이름과 id만 담은 지우기, 자기 권한이 바뀌면 `permissions`로 닫는다.
/// 남의 토큰이 생겨도(세대만 바뀜) 닫지 않는다.
#[tokio::test]
async fn watch_filters_events_and_closes_when_permissions_change() {
    let w = world().await;
    let (_, list) = w.alice.get("/vms").await;
    let since = list["revision"].as_u64().unwrap();
    let mut ws = connect(&w.alice, &format!("/watch?kinds=vm&since={since}")).await;
    let short = Duration::from_millis(500);

    w.admin.post("/vms", vm_in("b2", Some(&w.b))).await;
    w.admin.post("/vms", vm_in("a3", Some(&w.a))).await;
    let ev = next_json(&mut ws, Duration::from_secs(10)).await.unwrap();
    assert_eq!(
        (ev["type"].as_str(), ev["resource"]["name"].as_str()),
        (Some("put"), Some("a3"))
    );

    // 다른 사용자가 로그인해 토큰이 생겨도(세대가 바뀜) 계속 받는다.
    let _bob = w.env.user("bob", "viewer").await;
    w.admin
        .post(
            &format!("/folders/{}/members", w.b),
            json!({ "items": [{ "kind": "vm", "key": "a3" }] }),
        )
        .await;
    let ev = next_json(&mut ws, Duration::from_secs(10)).await.unwrap();
    assert_eq!(ev["type"], "deleted", "{ev}");
    assert_eq!(ev["resource"]["name"], "a3");
    assert!(ev["resource"]["spec"].is_null(), "{ev}");
    // 그 뒤 b 쪽의 변경은 오지 않는다.
    w.admin.post("/vms/a3/actions/start", Value::Null).await;
    assert!(next_json(&mut ws, short).await.is_none());

    // alice의 권한이 바뀐다: 닫는다.
    let (status, body) = w
        .admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": w.alice_id },
                "role": { "type": "builtin", "role": "viewer" },
                "target": { "type": "folder", "id": w.b },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    w.admin.post("/vms", vm_in("a4", Some(&w.a))).await;
    let ev = next_json(&mut ws, Duration::from_secs(10)).await.unwrap();
    assert_eq!(ev["type"], "error", "{ev}");
    assert_eq!(ev["code"], "permissions");

    // 루트 종류는 구독할 때 본다.
    let (status, _) = w.alice.get("/watch?kinds=node&since=0").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 위임(설계 §6.5): 대상의 `permission.modify`, 자기 권한 안에서만, 루트 grant·역할·그룹은 `cluster.admin`.
#[tokio::test]
async fn delegation_cannot_escalate() {
    let w = world().await;
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "lead", "privileges": ["permission.modify", "vm.power", "vm.console"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let role_id = role["id"].as_str().unwrap().to_string();
    let (status, body) = w
        .admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": w.alice_id },
                "role": { "type": "custom", "id": role_id },
                "target": { "type": "folder", "id": w.a },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, bob) = w
        .admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bob}");
    let bob_id = bob["id"].as_str().unwrap();
    let grant = |role: Value, target: Value| json!({ "principal": { "type": "user", "id": bob_id }, "role": role, "target": target });
    let folder_a = json!({ "type": "folder", "id": w.a });
    // alice는 /teamA에서 operator(그룹) + lead다. admin 역할은 줄 수 없다.
    let (status, body) = w
        .alice
        .post(
            "/grants",
            grant(
                json!({ "type": "builtin", "role": "admin" }),
                folder_a.clone(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("cannot grant privileges you do not hold:"),
        "{body}"
    );
    // 가진 것 안의 역할은 된다.
    let (status, body) = w
        .alice
        .post(
            "/grants",
            grant(
                json!({ "type": "builtin", "role": "operator" }),
                folder_a.clone(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let given = body["id"].as_str().unwrap().to_string();
    // 남의 폴더(볼 수 없음)는 404, 루트는 403
    let (status, _) = w
        .alice
        .post(
            "/grants",
            grant(
                json!({ "type": "builtin", "role": "viewer" }),
                json!({ "type": "folder", "id": w.b }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = w
        .alice
        .post(
            "/grants",
            grant(
                json!({ "type": "builtin", "role": "viewer" }),
                json!({ "type": "root" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 역할·그룹 쓰기는 관리자만
    assert_eq!(
        w.alice
            .post(
                "/access-roles",
                json!({ "name": "x", "privileges": ["vm.power"] })
            )
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // 이 grant가 bob의 목록과 유효 권한에 보인다.
    let bob = w.env.login("bob", "password-1").await;
    assert_eq!(names(&bob.get("/vms").await.1), set(&["a1", "a2"]));
    let (status, eff) = w
        .alice
        .get("/permissions/effective?user=bob&object=vm/a1")
        .await;
    assert_eq!(status, StatusCode::OK, "{eff}");
    assert!(
        eff["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("vm.power"))
    );
    assert_eq!(eff["grants"].as_array().unwrap().len(), 1, "{eff}");
    // bob은 남의 유효 권한을 못 본다.
    assert_eq!(
        bob.get("/permissions/effective?user=alice&object=vm/a1")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, who) = w.alice.get("/permissions/who?object=vm/a1").await;
    assert_eq!(status, StatusCode::OK, "{who}");
    assert!(who["grants"].as_array().unwrap().len() >= 3, "{who}");
    // 뺀다.
    assert_eq!(
        w.alice.delete(&format!("/grants/{given}")).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(names(&bob.get("/vms").await.1), set(&[]));
    // 쓰이는 역할은 지우지 못한다.
    let (status, body) = w.admin.delete("/access-roles/lead").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "role_in_use");
    // 내장 역할은 바꾸지 못하고 목록에 보인다.
    assert_eq!(
        w.admin.delete("/access-roles/admin").await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, roles) = w.alice.get("/access-roles").await;
    assert_eq!(names(&roles), set(&["admin", "operator", "viewer", "lead"]));
}

/// 내 권한 표, 범위 토큰(주인 ∩ 범위), 범위 토큰은 토큰을 만들지 못한다.
#[tokio::test]
async fn my_permissions_and_scoped_tokens() {
    let w = world().await;
    let (status, mine) = w.alice.get("/permissions/mine").await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    assert_eq!(mine["all"], false);
    assert_eq!(mine["root"], json!([]));
    assert!(
        mine["contents"][&w.a]
            .as_array()
            .unwrap()
            .contains(&json!("vm.power"))
    );
    let (_, admin_mine) = w.admin.get("/permissions/mine").await;
    assert_eq!(admin_mine["all"], true);

    let (status, token) = w
        .alice
        .post(
            "/tokens",
            json!({ "name": "ci", "scope": { "privileges": ["vm.power"], "targets": [{ "type": "folder", "id": w.a }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{token}");
    assert_eq!(token["token"]["scope"]["privileges"], json!(["vm.power"]));
    let ci = Client {
        base: w.alice.base.clone(),
        http: reqwest::Client::new(),
        token: Some(token["secret"].as_str().unwrap().to_string()),
    };
    // 범위 안의 VM은 보이고(system.read를 품는다) 켤 수 있지만 설정은 못 바꾼다.
    assert_eq!(names(&ci.get("/vms").await.1), set(&["a1", "a2"]));
    assert_eq!(ci.delete("/vms/a1").await.0, StatusCode::FORBIDDEN);
    assert_eq!(ci.get("/storage-pools").await.1["items"], json!([]));
    let (status, body) = ci.post("/tokens", json!({ "name": "more" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 잘못된 범위는 400
    let (status, _) = w
        .alice
        .post(
            "/tokens",
            json!({ "name": "bad", "scope": { "privileges": ["vm.fly"], "targets": [{ "type": "root" }] } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 폴더 API: 형제 이름, 이름 바꾸기·할당량은 부모 쪽 권한, 순환은 409.
#[tokio::test]
async fn folder_api_checks_names_cycles_and_parent_privileges() {
    let w = world().await;
    let prod = folder(&w.admin, "prod", "vm", Some(&w.a)).await;
    let (status, body) = w
        .admin
        .post(
            "/folders",
            json!({ "name": "PROD", "kind": "vm", "parent": w.a }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    let (status, body) = w
        .admin
        .patch(
            &format!("/folders/{}", w.a),
            json!({ "parent": prod }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "folder_cycle");
    // alice(operator)는 폴더를 만들지 못한다. inventory 권한을 받으면 /teamA 아래에 만들고, 자기 할당량은 못 올린다.
    let (status, _) = w
        .alice
        .post(
            "/folders",
            json!({ "name": "dev", "kind": "vm", "parent": w.a }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "folder-admin", "privileges": ["inventory.folder", "inventory.move", "inventory.quota"] }),
        )
        .await;
    w.admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "group", "id": w.group },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "folder", "id": w.a },
            }),
        )
        .await;
    let (status, body) = w
        .alice
        .post(
            "/folders",
            json!({ "name": "dev", "kind": "vm", "parent": w.a }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let dev = body["id"].as_str().unwrap().to_string();
    // 자식(dev)의 할당량은 부모(/teamA)의 inventory.quota로 된다.
    let (status, body) = w
        .alice
        .patch(
            &format!("/folders/{dev}"),
            json!({ "quota": { "vms": 3 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // /teamA 자신의 할당량은 루트 쪽 권한이 있어야 한다.
    let (status, body) = w
        .alice
        .patch(
            &format!("/folders/{}", w.a),
            json!({ "quota": { "vms": 100 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 이름 바꾸기
    let (status, body) = w
        .alice
        .patch(&format!("/folders/{dev}"), json!({ "name": "devel" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["name"], "devel");
}

/// 기능 게이트: 모든 노드가 `folders`를 알기 전에는 폴더 칸과 grant 쓰기가 409 `cluster_upgrading`이다.
#[tokio::test]
async fn folder_writes_wait_for_every_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let a = folder(&admin, "teamA", "vm", None).await;
    let mut node = env
        .cluster
        .get::<opl_types::node::NodeSpec>(env.node)
        .unwrap()
        .unwrap();
    node.status
        .features
        .retain(|f| f != opl_types::node::FOLDERS);
    env.cluster
        .update_status::<opl_types::node::NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    for (path, body) in [
        ("/folders", json!({ "name": "x", "kind": "vm" })),
        ("/vms", vm_in("v", Some(&a))),
        (
            "/grants",
            json!({ "principal": { "type": "user", "id": Uuid::now_v7() }, "role": { "type": "builtin", "role": "viewer" }, "target": { "type": "root" } }),
        ),
        ("/groups", json!({ "name": "g", "spec": {} })),
        (
            "/access-roles",
            json!({ "name": "r", "privileges": ["vm.power"] }),
        ),
        (
            "/users",
            json!({ "name": "nobody", "password": "password-1", "role": "none" }),
        ),
    ] {
        let (status, resp) = admin.post(path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {resp}");
        assert_eq!(resp["code"], "cluster_upgrading", "{path}");
    }
    // 폴더 없는 쓰기는 그대로 된다.
    let (status, body) = admin.post("/vms", vm_in("v", None)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 지표: VM 지표는 VM을 따르고 클러스터·노드·Prometheus는 루트 `cluster.read`다.
#[tokio::test]
async fn metrics_follow_objects_and_the_root() {
    let w = world().await;
    assert_eq!(
        w.alice.get("/vms/b1/metrics/history").await.0,
        StatusCode::NOT_FOUND
    );
    assert_ne!(
        w.alice.get("/vms/a1/metrics/history").await.0,
        StatusCode::NOT_FOUND
    );
    assert_ne!(
        w.alice.get("/vms/a1/metrics/history").await.0,
        StatusCode::FORBIDDEN
    );
    for path in [
        "/metrics",
        "/metrics/cluster",
        &format!("/nodes/{}/metrics", w.env.node),
    ] {
        assert_eq!(w.alice.get(path).await.0, StatusCode::FORBIDDEN, "{path}");
    }
    // 루트 viewer(그룹)를 주면 클러스터 지표가 열리고 상위 VM은 거른다.
    let viewer = w.env.user("carol", "viewer").await;
    assert_eq!(viewer.get("/metrics/cluster").await.0, StatusCode::OK);
}

/// 외부 그룹 연결: 그룹의 `external`과 사용자의 `external_groups`(로그인 때 저장)가 맞으면 그 그룹의 grant를 받는다.
#[tokio::test]
async fn external_groups_join_local_groups() {
    let w = world().await;
    let c = &w.env.cluster;
    // 영역 사용자(로그인을 거친 것처럼 외부 그룹을 적어 둔다)
    let realm: Resource<opl_types::realm::RealmSpec, _> = Resource::new(
        "corp",
        serde_json::from_value(json!({
            "backend": { "kind": "ldap", "url": "ldap://127.0.0.1:1", "base_dn": "dc=x" }
        }))
        .unwrap(),
    );
    c.create(&realm).await.unwrap();
    let dave: Resource<opl_types::auth::UserSpec, _> = Resource::new(
        "dave@corp",
        opl_types::auth::UserSpec {
            password_hash: String::new(),
            role: opl_types::auth::Role::None,
            session_epoch: 0,
            realm: Some("corp".into()),
            subject: None,
            external_groups: vec!["CN=Team-A,DC=x".into()],
        },
    );
    c.create(&dave).await.unwrap();
    let (status, body) = w
        .admin
        .put(
            "/groups/team-a",
            json!({ "spec": { "members": [w.alice_id], "external": [{ "realm": "corp", "group": " cn=team-a,dc=x " }] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, eff) = w
        .admin
        .get("/permissions/effective?user=dave@corp&object=vm/a1")
        .await;
    assert_eq!(status, StatusCode::OK, "{eff}");
    assert!(
        eff["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("vm.power")),
        "{eff}"
    );
    let (_, eff) = w
        .admin
        .get("/permissions/effective?user=dave@corp&object=vm/b1")
        .await;
    assert_eq!(eff["privileges"], json!([]), "{eff}");
}

/// 파생 종류(Task 등)를 구독하는데 VM이 옮겨져 보임이 바뀌면 그 Task 이벤트가 없으므로 `permissions`로 닫는다. 새 VM은
/// 닫지 않는다.
#[tokio::test]
async fn watch_of_derived_kinds_closes_when_a_vm_moves_out() {
    let w = world().await;
    let (_, list) = w.alice.get("/tasks").await;
    let since = list["revision"].as_u64().unwrap();
    let mut ws = connect(&w.alice, &format!("/watch?kinds=vm,task&since={since}")).await;
    w.admin.post("/vms", vm_in("a3", Some(&w.a))).await;
    let ev = next_json(&mut ws, Duration::from_secs(10)).await.unwrap();
    assert_eq!(ev["type"], "put", "{ev}");
    w.admin
        .post(
            &format!("/folders/{}/members", w.b),
            json!({ "items": [{ "kind": "vm", "key": "a3" }] }),
        )
        .await;
    let ev = next_json(&mut ws, Duration::from_secs(10)).await.unwrap();
    assert_eq!(ev["code"], "permissions", "{ev}");
}
