//! 폴더·grant 권한의 시행(계획 62 설계 §8·§14): 폴더 사용자의 모든 목록이 자기 폴더만 돌려주고, 남의 객체는 404,
//! 권한 밖 동작은 403, `/watch` 거르기·옮긴 뒤 가짜 지우기·권한이 바뀌면 닫기, 지표·경보·감사의 루트 요구, 위임과
//! 상승 막기, 기능 게이트, 폴더·역할·그룹·grant·유효 권한 API.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{Client, Env, vm_request};
use futures_util::StreamExt;
use orbivisor_types::backup::BackupSpec;
use orbivisor_types::resource::Resource;
use orbivisor_types::snapshot::SnapshotSpec;
use orbivisor_types::storage::VolumeSpec;
use orbivisor_types::task::{TaskOp, TaskSpec};
use orbivisor_types::vm::VmSpec;
use orbivisor_types::vm_import::{ImportSource, OvaSource, VmImportSpec};
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

/// 이 노드의 유지보수 표시를 저장소에 바로 쓴다.
async fn set_maintenance(env: &common::Env, on: bool) {
    let node = env
        .cluster
        .get::<orbivisor_types::node::NodeSpec>(env.node)
        .unwrap()
        .unwrap();
    let mut spec = node.spec.clone();
    spec.maintenance = on;
    env.cluster
        .update_spec(node.id, None, node.labels.clone(), &spec)
        .await
        .unwrap();
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
                    source: None,
                    store: None,
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
        "/cpu/baseline",
    ] {
        assert_eq!(alice.get(path).await.0, StatusCode::FORBIDDEN, "{path}");
    }
    // CPU 모델·플래그(계획 63): VM을 만들 수 있으면 읽되 노드 이름은 루트 읽기 권한이 있을 때만
    let (status, models) = alice.get("/cpu/models").await;
    assert_eq!(status, StatusCode::OK, "{models}");
    assert!(
        models["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m.get("nodes").is_none()),
        "{models}"
    );
    assert!(
        admin.get("/cpu/models").await.1["models"][0]["nodes"]
            .as_array()
            .is_some_and(|n| !n.is_empty())
    );
    assert_eq!(alice.get("/cpu/flags").await.0, StatusCode::OK);
    let me = admin.get("/nodes").await.1["items"][0]["name"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        alice.get(&format!("/nodes/{me}/cpu")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        alice
            .post(&format!("/nodes/{me}/cpu/refresh"), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // 노드 이름을 볼 수 없으면 노드로 거를 수도 없다(있는지 드러내지 않게, 리뷰 1 M2).
    assert_eq!(
        alice.get(&format!("/cpu/models?node={me}")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        alice.get("/cpu/models?node=nope").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin.get(&format!("/cpu/models?node={me}")).await.0,
        StatusCode::OK
    );
    // 아무 권한도 없는 사용자는 모델 목록도 못 본다.
    let nobody = w.env.user("nobody", "none").await;
    assert_eq!(nobody.get("/cpu/models").await.0, StatusCode::FORBIDDEN);
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
    for path in [
        "/vms/b1/metrics",
        "/vms/b1/snapshots",
        "/vms/b1/guest",
        "/vms/b1/cpu",
    ] {
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

    // VM의 CPU는 읽되 노드마다 내용(노드 이름)은 루트 읽기 권한이 있을 때만(리뷰 1 M2)
    let (status, cpu) = alice.get("/vms/a1/cpu").await;
    assert_eq!(status, StatusCode::OK, "{cpu}");
    assert_eq!(cpu["nodes"], serde_json::json!([]), "{cpu}");
    let (_, cpu) = w.admin.get("/vms/a1/cpu").await;
    assert!(
        cpu["nodes"].as_array().is_some_and(|n| !n.is_empty()),
        "{cpu}"
    );
    // 놓을 노드가 없는 409 no_node도 노드마다 까닭을 보이지 않는다(유일한 노드가 유지보수 중).
    set_maintenance(&w.env, true).await;
    let mut running = vm_in("busy", Some(&w.a));
    running["spec"]["power"] = "running".into();
    let (status, body) = alice.post("/vms", running.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node", "{body}");
    assert!(body.get("details").is_none_or(|d| d.is_null()), "{body}");
    assert!(
        !body["message"].as_str().unwrap().contains("maintenance\""),
        "{body}"
    );
    let (status, body) = w.admin.post("/vms", running).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["details"]["nodes"].is_object(), "{body}");
    set_maintenance(&w.env, false).await;

    // 자기 것은 읽는다.
    for path in [
        "/vms/a1",
        "/vms/a1/cpu",
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
        .get::<orbivisor_types::node::NodeSpec>(env.node)
        .unwrap()
        .unwrap();
    node.status
        .features
        .retain(|f| f != orbivisor_types::node::FOLDERS);
    env.cluster
        .update_status::<orbivisor_types::node::NodeSpec>(env.node, &node.status)
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
    let realm: Resource<orbivisor_types::realm::RealmSpec, _> = Resource::new(
        "corp",
        serde_json::from_value(json!({
            "backend": { "kind": "ldap", "url": "ldap://127.0.0.1:1", "base_dn": "dc=x" }
        }))
        .unwrap(),
    );
    c.create(&realm).await.unwrap();
    let dave: Resource<orbivisor_types::auth::UserSpec, _> = Resource::new(
        "dave@corp",
        orbivisor_types::auth::UserSpec {
            password_hash: String::new(),
            role: orbivisor_types::auth::Role::None,
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

/// 리뷰 1 C1: propagate가 아닌 폴더 grant로 받은 권한을 propagate grant로 넓히지 못한다(주기·빼기 모두 grant가 닿는
/// 모든 곳에서 역할의 권한과 `permission.modify`가 있어야 한다).
#[tokio::test]
async fn a_non_propagating_grant_cannot_be_widened() {
    let w = world().await;
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "escal", "privileges": ["permission.modify", "vm.delete", "vm.console", "vm.config"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let escal = json!({ "type": "custom", "id": role["id"] });
    let on_b = |user: &str, role: &Value, propagate: bool| {
        json!({
            "principal": { "type": "user", "id": user },
            "role": role,
            "target": { "type": "folder", "id": w.b },
            "propagate": propagate,
        })
    };
    let (status, body) = w
        .admin
        .post("/grants", on_b(&w.alice_id, &escal, false))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 자기에게 같은 역할을 propagate로: /teamB 안의 VM에는 권한이 없다.
    let (status, body) = w
        .alice
        .post("/grants", on_b(&w.alice_id, &escal, true))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.starts_with("cannot grant privileges you do not hold everywhere inside:"),
        "{body}"
    );
    assert!(message.contains("vm.delete"), "{body}");
    assert_eq!(w.alice.get("/vms/b1").await.0, StatusCode::NOT_FOUND);
    // 폴더 자체에만 닿는 grant는 줄 수 있다.
    let (status, own) = w
        .alice
        .post("/grants", on_b(&w.alice_id, &escal, false))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{own}");
    // 남의 propagate grant 빼기도 아래 모두의 권한이 있어야 한다.
    let (status, bob) = w
        .admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bob}");
    let viewer = json!({ "type": "builtin", "role": "viewer" });
    let (status, given) = w
        .admin
        .post("/grants", on_b(bob["id"].as_str().unwrap(), &viewer, true))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{given}");
    let (status, body) = w
        .alice
        .delete(&format!("/grants/{}", given["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .alice
        .delete(&format!("/grants/{}", own["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // /teamA는 그룹 operator(propagate)와 lead가 없으니 permission.modify가 없다: 기존 규칙 그대로 403.
    let (status, _) = w
        .alice
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": bob["id"] },
                "role": viewer,
                "target": { "type": "folder", "id": w.a },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// 리뷰 1 I7: 풀 만들기와 브리지 네트워크 만들기는 루트 `cluster.admin`이다. 폴더의 `storage.config`·
/// `network.config`로는 VXLAN 네트워크 만들기와 있는 풀·네트워크의 바꾸기·지우기만 한다.
#[tokio::test]
async fn host_pools_and_bridges_need_cluster_admin() {
    let w = world().await;
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "infra", "privileges": ["storage.config", "network.config"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    for target in [&w.st_a, &w.net_a] {
        let (status, body) = w
            .admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": w.alice_id },
                    "role": { "type": "custom", "id": role["id"] },
                    "target": { "type": "folder", "id": target },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = w
        .alice
        .post(
            "/storage-pools",
            json!({ "name": "etc", "spec": { "backend": { "kind": "dir", "path": "/etc" }, "folder": w.st_a } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("creating a storage pool needs cluster.admin"),
        "{body}"
    );
    let (status, body) = w
        .alice
        .post(
            "/networks",
            json!({ "name": "host-nic", "spec": { "bridge": "vmbr0", "vlan": 10, "folder": w.net_a } }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .alice
        .post(
            "/networks",
            json!({ "name": "overlay", "spec": { "vxlan": { "vni": 4242 }, "folder": w.net_a } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 리뷰 2 N11: `shared`는 관리자만, `backups`는 그 풀의 storage.config
    let (status, body) = w
        .alice
        .patch("/storage-pools/pool-a", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .alice
        .patch("/storage-pools/pool-a", json!({ "backups": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = w.alice.delete("/networks/overlay").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// 리뷰 1 I1·M1·M2: 옮기기의 대상 풀, CD-ROM의 ISO를 확인하고, 복제 Task에 확인한 폴더를 못 박는다.
#[tokio::test]
async fn migrate_pools_isos_and_clone_folders_are_checked() {
    let w = world().await;
    let (_, nodes) = w.admin.get("/nodes").await;
    let node = nodes["items"][0]["name"].as_str().unwrap().to_string();
    // 볼 수 없는 풀로 옮기기: 그 풀이 없는 것과 같은 404
    let (status, body) = w
        .alice
        .post(
            "/vms/a1/migrate",
            json!({ "node": node, "pool": "pool-root" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["message"], "storage pool \"pool-root\" not found");
    let (status, body) = w
        .admin
        .post(
            "/vms/a1/migrate",
            json!({ "node": node, "pool": "pool-root" }),
        )
        .await;
    assert_ne!(
        (status, body["message"].clone()),
        (
            StatusCode::NOT_FOUND,
            json!("storage pool \"pool-root\" not found")
        ),
        "{body}"
    );

    // 볼 수 없는 풀의 ISO는 CD-ROM에 넣지 못한다.
    let mut iso: Resource<VolumeSpec, _> = Resource::new(
        "iso-root",
        serde_json::from_value(
            json!({ "pool": "pool-root", "size_bytes": 1048576, "format": "raw", "content": "iso" }),
        )
        .unwrap(),
    );
    w.env.cluster.create(&iso).await.unwrap();
    iso.status.phase = orbivisor_types::storage::VolumePhase::Ready;
    w.env
        .cluster
        .update_status::<VolumeSpec>(iso.id, &iso.status)
        .await
        .unwrap();
    let mut req = vm_in("a-iso", Some(&w.a));
    req["spec"]["cdroms"] = json!([{ "iso": "iso-root" }]);
    let (status, body) = w.alice.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["message"], "volume \"iso-root\" not found");
    let (status, body) = w.admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 복제 Task는 확인한 폴더를 덮어쓰기로 갖는다(원본이 그사이 옮겨져도).
    let (status, task) = w
        .alice
        .post("/vms/a2/clone", json!({ "name": "a2-copy" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let op = task["spec"]["op"].to_string();
    assert!(op.contains(&format!("\"folder\":\"{}\"", w.a)), "{task}");
}

/// 리뷰 1 M7: 남의 유효 권한은 사용자를 찾기 전에 막고(이름 알아내기 없음), 전역 역할 사용자 목록은 관리자만, 볼 수
/// 없는 조상의 할당량 사용량은 보이지 않고, Prometheus는 루트 아래 모두를 읽는 주체만이다.
#[tokio::test]
async fn permission_views_do_not_leak() {
    let w = world().await;
    let object = format!("folder/{}", w.a);
    for user in ["admin", "nobody-here"] {
        let (status, body) = w
            .alice
            .get(&format!(
                "/permissions/effective?user={user}&object={object}"
            ))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{user}: {body}");
    }
    let (status, body) = w
        .alice
        .get(&format!(
            "/permissions/effective?user=alice&object={object}"
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // permission.modify를 /teamA에 주면 who를 보지만 전역 역할 사용자는 비어 있다.
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "lead", "privileges": ["permission.modify"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, body) = w
        .admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": w.alice_id },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "folder", "id": w.a },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = w
        .alice
        .get(&format!("/permissions/who?object={object}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["users"], json!([]), "{body}");
    let (_, body) = w
        .admin
        .get(&format!("/permissions/who?object={object}"))
        .await;
    assert!(
        body["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["name"] == "admin"),
        "{body}"
    );

    // 할당량: 볼 수 없는 조상(/top)은 사용량 보기에 없다.
    let (status, top) = w
        .admin
        .post(
            "/folders",
            json!({ "name": "top", "kind": "vm", "quota": { "vms": 10 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{top}");
    let (status, kid) = w
        .admin
        .post(
            "/folders",
            json!({ "name": "kid", "kind": "vm", "parent": top["id"], "quota": { "vms": 5 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{kid}");
    let kid = kid["id"].as_str().unwrap().to_string();
    let (status, body) = w
        .admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": w.alice_id },
                "role": { "type": "builtin", "role": "viewer" },
                "target": { "type": "folder", "id": kid },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = w.alice.get(&format!("/folders/{kid}/usage")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let limits: Vec<&str> = body["limits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["path"].as_str().unwrap())
        .collect();
    assert_eq!(limits, ["/top/kid"], "{body}");
    assert_eq!(body["binding"], json!({ "vms": kid }), "{body}");
    let (_, body) = w.admin.get(&format!("/folders/{kid}/usage")).await;
    assert_eq!(body["limits"].as_array().unwrap().len(), 2, "{body}");

    // Prometheus: propagate가 아닌 루트 cluster.read만으로는 403, 전역 viewer는 통과(꺼져 있어 404).
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "reader", "privileges": ["cluster.read"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, body) = w
        .admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": w.alice_id },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "root" },
                "propagate": false,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(w.alice.get("/metrics").await.0, StatusCode::FORBIDDEN);
    assert_eq!(w.alice.get("/metrics/cluster").await.0, StatusCode::OK);
    let viewer = w.env.user("carol", "viewer").await;
    assert_eq!(viewer.get("/metrics").await.0, StatusCode::NOT_FOUND);
}

/// 사용자에게 grant를 준다(관리자).
async fn grant(admin: &Client, user: &str, role: Value, target: Value, propagate: bool) -> String {
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": user },
                "role": role,
                "target": target,
                "propagate": propagate,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

/// 리뷰 2 N1: 폴더 자체에만 닿는(propagate가 아닌) `inventory.move`로 그 폴더를 자기가 다스리는 폴더 아래로 끌어와
/// 하위 트리 전체를 넘겨받지 못한다. 옛 부모와 새 부모 안 모두에서 propagate로 받은 `inventory.move`가 있어야 한다.
#[tokio::test]
async fn moving_a_folder_needs_move_inside_both_parents() {
    let w = world().await;
    let sandbox = folder(&w.admin, "sandbox", "vm", None).await;
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "organizer", "privileges": ["inventory.folder", "inventory.move"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let organizer = json!({ "type": "custom", "id": role["id"] });
    grant(
        &w.admin,
        &w.alice_id,
        organizer,
        json!({ "type": "folder", "id": w.b }),
        false,
    )
    .await;
    grant(
        &w.admin,
        &w.alice_id,
        json!({ "type": "builtin", "role": "admin" }),
        json!({ "type": "folder", "id": sandbox }),
        true,
    )
    .await;
    // /teamB를 /sandbox 아래로: 폴더 자체의 권한과 새 부모의 권한은 있지만 옛 부모(루트) 안에는 없다.
    let (status, body) = w
        .alice
        .patch(
            &format!("/folders/{}", w.b),
            json!({ "parent": sandbox }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("moving a folder needs inventory.move everywhere inside /"),
        "{body}"
    );
    assert_eq!(w.alice.get("/vms/b1").await.0, StatusCode::NOT_FOUND);
    // 이름 바꾸기(폴더 자체)는 된다.
    let (status, body) = w
        .alice
        .patch(
            &format!("/folders/{}", w.b),
            json!({ "name": "teamB2" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 자기 하위 트리 안의 옮기기는 된다.
    let x = folder(&w.admin, "x", "vm", Some(&sandbox)).await;
    let y = folder(&w.admin, "y", "vm", Some(&sandbox)).await;
    let (status, body) = w
        .alice
        .patch(&format!("/folders/{x}"), json!({ "parent": y }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 자기 하위 트리 밖(루트)으로 꺼내기는 루트 안의 권한이 있어야 한다.
    let (status, body) = w
        .alice
        .patch(&format!("/folders/{x}"), json!({ "parent": null }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 관리자는 된다.
    let (status, body) = w
        .admin
        .patch(
            &format!("/folders/{}", w.b),
            json!({ "parent": sandbox }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 리뷰 2 N2·N3·N11: 복제 대상 풀은 `storage.allocate`, 원격 클러스터로 백업 보내기와 풀의 `shared` 바꾸기는 루트
/// `cluster.admin`이다.
#[tokio::test]
async fn replica_pools_remote_backups_and_shared_pools_are_checked() {
    let w = world().await;
    w.seed_derived().await;
    let (_, nodes) = w.admin.get("/nodes").await;
    let node = nodes["items"][0]["id"].as_str().unwrap().to_string();
    let (status, a1) = w.alice.get("/vms/a1").await;
    assert_eq!(status, StatusCode::OK, "{a1}");
    let with_replica = |pool: &str| {
        let mut spec = a1["spec"].clone();
        spec["replication"] = json!([{ "node": node, "pool": pool }]);
        json!({ "spec": spec })
    };
    // 볼 수 없는 풀: 없는 것과 같은 404(풀 이름 알아내기 없음)
    let (status, body) = w
        .alice
        .put("/vms/a1", with_replica("pool-root"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["message"], "storage pool \"pool-root\" not found");
    // 자기 풀이면 권한은 지나가고 복제 검사가 답한다.
    let (status, body) = w.alice.put("/vms/a1", with_replica("pool-a"), None).await;
    assert_ne!(status, StatusCode::NOT_FOUND, "{body}");
    assert_ne!(status, StatusCode::FORBIDDEN, "{body}");

    // 원격으로 보내기: 백업을 볼 수 있어도 관리자만
    let (status, body) = w
        .alice
        .post(
            "/backups/bk-a1/actions/replicate",
            json!({ "remote": "dr", "pool": "dr-bk" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .admin
        .post(
            "/backups/bk-a1/actions/replicate",
            json!({ "remote": "dr", "pool": "dr-bk" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 풀의 shared는 관리자, backups는 그 풀의 storage.config
    let (status, role) = w
        .admin
        .post(
            "/access-roles",
            json!({ "name": "storage", "privileges": ["storage.config"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    grant(
        &w.admin,
        &w.alice_id,
        json!({ "type": "custom", "id": role["id"] }),
        json!({ "type": "folder", "id": w.st_a }),
        true,
    )
    .await;
    let (status, body) = w
        .alice
        .patch("/storage-pools/pool-a", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = w
        .alice
        .patch("/storage-pools/pool-a", json!({ "backups": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = w
        .admin
        .patch("/storage-pools/pool-a", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 리뷰 2 N4·N5: 사용자를 지우면 그 grant와 그룹 멤버 자리가, 폴더·VM을 지우면 그것에 건 grant가 함께 없어진다. 이미
/// 남은 지운 멤버는 그룹 고치기를 막지 않는다.
#[tokio::test]
async fn deleting_users_folders_and_objects_drops_their_grants() {
    let w = world().await;
    let (status, bob) = w
        .admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{bob}");
    let bob = bob["id"].as_str().unwrap().to_string();
    let (status, body) = w
        .admin
        .put(
            "/groups/team-a",
            json!({ "spec": { "members": [w.alice_id, bob] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let viewer = json!({ "type": "builtin", "role": "viewer" });
    let on_b = grant(
        &w.admin,
        &bob,
        viewer.clone(),
        json!({ "type": "folder", "id": w.b }),
        true,
    )
    .await;
    let empty = folder(&w.admin, "empty", "vm", None).await;
    let on_empty = grant(
        &w.admin,
        &w.alice_id,
        viewer.clone(),
        json!({ "type": "folder", "id": empty }),
        true,
    )
    .await;
    let r1 = w.vm_id("r1").await;
    let on_r1 = grant(
        &w.admin,
        &w.alice_id,
        viewer.clone(),
        json!({ "type": "object", "kind": "vm", "id": r1 }),
        true,
    )
    .await;
    // 이미 남은 고아(옛 상태): 지운 폴더를 가리키는 grant는 정리 작업이 지우고, 색인은 무시한다.
    let stale: Resource<orbivisor_types::authz::GrantSpec, _> = Resource::new(
        "stale",
        serde_json::from_value(json!({
            "principal": { "type": "user", "id": w.alice_id },
            "role": viewer,
            "target": { "type": "folder", "id": Uuid::now_v7() },
        }))
        .unwrap(),
    );
    w.env.cluster.create(&stale).await.unwrap();

    let (status, body) = w.admin.delete("/users/bob").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, group) = w.admin.get("/groups/team-a").await;
    assert_eq!(group["spec"]["members"], json!([w.alice_id]), "{group}");
    assert_eq!(
        w.admin.get(&format!("/grants/{on_b}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = w.admin.delete(&format!("/folders/{empty}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        w.admin.get(&format!("/grants/{on_empty}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, body) = w.admin.delete("/vms/r1").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        w.admin.get(&format!("/grants/{on_r1}")).await.0,
        StatusCode::NOT_FOUND
    );
    // 지우기 뒤의 정리가 옛 고아도 함께 지웠다.
    assert_eq!(
        w.admin.get(&format!("/grants/{}", stale.id)).await.0,
        StatusCode::NOT_FOUND
    );

    // 정리를 놓친 지운 멤버(옛 상태)가 남아 있어도 GET으로 받은 그대로 되보낼 수 있고, 저장할 때 빠진다.
    let gone = Uuid::now_v7();
    let mut stored = w
        .env
        .cluster
        .get_by_name::<orbivisor_types::authz::GroupSpec>("team-a")
        .unwrap()
        .unwrap();
    stored.spec.members.push(gone);
    w.env
        .cluster
        .update_spec(stored.id, None, stored.labels.clone(), &stored.spec)
        .await
        .unwrap();
    let (_, group) = w.admin.get("/groups/team-a").await;
    let (status, body) = w
        .admin
        .put(
            "/groups/team-a",
            json!({ "spec": { "members": group["spec"]["members"], "description": "x" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["members"], json!([w.alice_id]), "{body}");
    // 새로 넣는 없는 사용자는 여전히 400
    let (status, _) = w
        .admin
        .put(
            "/groups/team-a",
            json!({ "spec": { "members": [w.alice_id, Uuid::now_v7()] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// 리뷰 2 N6·N7: 사용자마다 토큰 수 상한, Task에 요청한 사용자, 권한을 잃은 콘솔은 닫힌다.
#[tokio::test]
async fn token_cap_task_requester_and_console_revocation() {
    let w = world().await;
    // 로그인한 클라이언트도 토큰 하나를 쓴다.
    let (_, mine) = w.alice.get("/tokens").await;
    let have = mine["items"].as_array().unwrap().len();
    for i in have..orbivisor_types::authz::MAX_USER_TOKENS {
        let (status, body) = w
            .alice
            .post("/tokens", json!({ "name": format!("t{i}") }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (status, body) = w.alice.post("/tokens", json!({ "name": "one-more" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "too_many_tokens");

    // Task는 요청한 사용자를 레이블로 갖는다.
    let (status, task) = w
        .alice
        .post("/vms/a2/clone", json!({ "name": "a2-copy" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["labels"]["requested-by"], w.alice_id, "{task}");

    // 콘솔: 열린 뒤 그룹 grant가 빠지면 닫힌다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    w.env.hv.set_console_dir(tmp.path());
    w.admin.act("a1", "start").await;
    let id = w.vm_id("a1").await;
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{id}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (conn, _) = listener.accept().await.unwrap();
        // 닫힐 때까지 붙잡고 있는다.
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(conn);
    });
    let mut ws = connect(&w.alice, "/vms/a1/console").await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .is_err(),
        "the console stays open while allowed"
    );
    let (_, grants) = w.admin.get("/grants").await;
    for g in grants["items"].as_array().unwrap() {
        if g["spec"]["target"]["id"] == w.a {
            let (status, body) = w
                .admin
                .delete(&format!("/grants/{}", g["id"].as_str().unwrap()))
                .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        }
    }
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(msg)) = ws.next().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "the console closes after the grant is removed"
    );
}

/// 리뷰 2(웹 I1): 노드 목록을 읽지 못하는 폴더 사용자도 `whoami`로 요청을 받은 노드를 안다.
#[tokio::test]
async fn whoami_names_the_serving_node() {
    let w = world().await;
    assert_eq!(w.alice.get("/nodes").await.0, StatusCode::FORBIDDEN);
    let (status, me) = w.alice.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "alice");
    assert_eq!(me["role"], "none");
    let node: Resource<orbivisor_types::node::NodeSpec, orbivisor_types::node::NodeStatus> =
        w.env.cluster.get(w.env.node).unwrap().unwrap();
    assert_eq!(
        me["node"],
        json!({ "id": node.id, "name": node.name }),
        "{me}"
    );
}

/// 리뷰 2 #3: 루트 `cluster.admin`이 아닌 사용자는 허용 목록의 플래그만 켠다(`+invtsc`는 403). 숨기기와 허용 목록의
/// 켜기는 이 검사를 지난다(그 뒤 검증은 카탈로그가 정한다). 관리자는 무엇이든 켤 수 있다.
#[tokio::test]
async fn only_cluster_admins_turn_on_restricted_cpu_flags() {
    let w = world().await;
    let with_flags = |name: &str, flags: &[&str]| {
        let mut req = vm_in(name, Some(&w.a));
        req["spec"]["cpu"]["flags"] = json!(flags);
        req
    };
    let (status, body) = w.alice.post("/vms", with_flags("tsc", &["+invtsc"])).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "cpu_flag_restricted", "{body}");
    assert_eq!(body["details"]["flags"], json!(["+invtsc"]), "{body}");
    for flags in [&["-invtsc"][..], &["+pcid"]] {
        let (_, body) = w.alice.post("/vms", with_flags("ok", flags)).await;
        assert_ne!(body["code"], "cpu_flag_restricted", "{flags:?}: {body}");
    }
    let (_, body) = w.admin.post("/vms", with_flags("tsc", &["+invtsc"])).await;
    assert_ne!(body["code"], "cpu_flag_restricted", "{body}");
    // 고치기도 같다(이미 저장된 플래그는 다시 보지 않는다).
    let (_, vm) = w.alice.get("/vms/a1").await;
    let mut spec = vm["spec"].clone();
    spec["cpu"]["flags"] = json!(["+vmx"]);
    let (status, body) = w
        .alice
        .put("/vms/a1", json!({ "spec": spec, "labels": {} }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 플래그 목록은 이 사용자가 제한된 플래그를 켤 수 있는지 알린다.
    let (_, flags) = w.alice.get("/cpu/flags").await;
    assert_eq!(flags["turn_on_restricted"], false, "{flags}");
    let (_, flags) = w.admin.get("/cpu/flags").await;
    assert_eq!(flags["turn_on_restricted"], true, "{flags}");
}
