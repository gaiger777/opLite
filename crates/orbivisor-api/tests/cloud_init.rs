//! VM의 cloud-init 설정(계획 57): 비밀번호는 SHA-512 crypt 해시로만 저장하고 VM·스냅샷·백업 응답과 watch 이벤트에서
//! 가리며(`********`), 가린 값을 되보내면 저장된 해시를 둔다. `user_data`가 만든 칸을 덮으면 경고 헤더, 실행 중에
//! 바꾸면 다음 재시작 때 적용(보류), 옛 노드가 있으면 409.

mod common;

use std::time::Duration;

use common::{Client, Env, vm_request};
use futures_util::StreamExt;
use orbivisor_types::backup::{Backup, BackupSpec};
use orbivisor_types::cloud_init::PASSWORD_MASK;
use orbivisor_types::crypt::sha512_crypt;
use orbivisor_types::node::{INSTALL_MEDIA, NodeSpec};
use orbivisor_types::resource::Resource;
use orbivisor_types::snapshot::{Snapshot, SnapshotSpec};
use orbivisor_types::vm::{PendingReason, VmSpec};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

const WARNING: &str = "cloud_init.user_data replaces the generated user-data; user, password and ssh_keys are not used";

/// 저장소에 있는 VM `name`의 cloud-init 비밀번호(해시)
fn stored_password(env: &Env, name: &str) -> Option<String> {
    let vm = env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap();
    vm.spec.cloud_init.and_then(|c| c.password)
}

/// cloud-init 비밀번호를 둔 VM `web`을 만들고 응답을 돌려준다.
async fn create_web(admin: &Client, password: &str) -> (StatusCode, Value) {
    let mut req = vm_request("web");
    req["spec"]["cloud_init"] = json!({ "user": "orbi", "password": password });
    admin.post("/vms", req).await
}

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

async fn next_text(ws: &mut Socket) -> String {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return text.to_string();
        }
    }
}

/// 평문 비밀번호는 `$6$`(무작위 솔트 16자, 기본 5000회)로 바꿔 Raft에 저장하고, VM 응답(만들기·고치기·조회·목록)과
/// watch 이벤트, 그 스펙을 담은 스냅샷·백업 응답 어디에도 해시가 없다. 받은 스펙(MASK)을 고쳐 되보내면 해시는
/// 그대로다.
#[tokio::test]
async fn passwords_are_stored_as_sha512_crypt_and_never_returned() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = create_web(&admin, "correct horse").await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["cloud_init"]["password"], PASSWORD_MASK);
    assert_eq!(vm["spec"]["cloud_init"]["user"], "orbi");

    let hash = stored_password(&env, "web").unwrap();
    let salt = hash.strip_prefix("$6$").unwrap().split('$').next().unwrap();
    assert_eq!(salt.len(), 16, "{hash}");
    assert_eq!(sha512_crypt(b"correct horse", salt), hash);
    // 같은 평문도 솔트가 달라 해시가 다르다.
    let mut other = vm_request("db");
    other["spec"]["cloud_init"] = json!({ "password": "correct horse" });
    assert_eq!(admin.post("/vms", other).await.0, StatusCode::CREATED);
    assert_ne!(stored_password(&env, "db").unwrap(), hash);

    for path in ["/vms/web", "/vms"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.to_string().contains(&hash), "{path}: {body}");
    }
    let (_, got) = admin.get("/vms/web").await;
    assert_eq!(got["spec"]["cloud_init"]["password"], PASSWORD_MASK);

    // 받은 스펙을 고쳐 되보내면(MASK) 해시를 둔다. 응답과 watch 이벤트에도 해시가 없다.
    let since = admin.get("/vms").await.1["revision"].as_u64().unwrap();
    let mut ws = connect(&admin, &format!("/watch?kinds=vm&since={since}")).await;
    let mut spec = got["spec"].clone();
    spec["cloud_init"]["hostname"] = json!("web-1");
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["cloud_init"]["password"], PASSWORD_MASK);
    assert!(!body.to_string().contains(&hash), "{body}");
    assert_eq!(stored_password(&env, "web").unwrap(), hash);
    let event = next_text(&mut ws).await;
    assert!(event.contains("web-1"), "{event}");
    assert!(event.contains(PASSWORD_MASK), "{event}");
    assert!(!event.contains(&hash), "{event}");

    // 저장된 스펙을 담은 스냅샷과 백업의 응답도 가린다.
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    let snapshot: Snapshot = Resource::new(
        format!("{}-s1", web.id),
        SnapshotSpec {
            vm: web.id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: web.spec.clone(),
            hardware: None,
        },
    );
    env.cluster.create(&snapshot).await.unwrap();
    let backup: Backup = Resource::new(
        "b1",
        BackupSpec {
            vm: web.id,
            vm_name: "web".into(),
            pool: "bk".into(),
            vm_spec: web.spec.clone(),
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
            source: None,
            store: None,
        },
    );
    env.cluster.create(&backup).await.unwrap();
    for path in [
        "/snapshots",
        "/vms/web/snapshots",
        "/vms/web/snapshots/s1",
        "/backups",
        "/backups?vm=web",
        "/backups/b1",
    ] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert!(body.to_string().contains(PASSWORD_MASK), "{path}: {body}");
        assert!(!body.to_string().contains(&hash), "{path}: {body}");
    }
}

/// 저장된 비밀번호가 없는데 MASK면 400이다. crypt 해시 입력은 그대로 저장하고, 비밀번호를 빼면 지운다.
#[tokio::test]
async fn a_mask_needs_a_stored_password_and_hashes_are_kept() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = create_web(&admin, PASSWORD_MASK).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "cloud_init.password: give a new password");

    let hash = sha512_crypt(b"pw", "saltsaltsaltsalt");
    let (status, vm) = create_web(&admin, &hash).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(stored_password(&env, "web").unwrap(), hash);

    let mut spec = vm["spec"].clone();
    spec["cloud_init"]
        .as_object_mut()
        .unwrap()
        .remove("password");
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored_password(&env, "web"), None);
    // 지운 뒤의 MASK는 되보낼 해시가 없다.
    spec["cloud_init"]["password"] = json!(PASSWORD_MASK);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "cloud_init.password: give a new password");
}

/// `user_data`가 만든 user-data를 대신하면서 `user`·`password`·`ssh_keys`가 있으면 `orbivisor-warning`으로 알린다.
#[tokio::test]
async fn user_data_that_replaces_generated_fields_is_warned() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["cloud_init"] = json!({ "user": "orbi", "user_data": "#cloud-config\n{}\n" });
    let (status, headers, vm) = admin.send_headers(Method::POST, "/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(headers["orbivisor-warning"], WARNING);

    // user_data만이면(덮을 칸이 없음) 알리지 않는다. 고치기 응답도 같은 규칙이다.
    let mut spec = vm["spec"].clone();
    spec["cloud_init"].as_object_mut().unwrap().remove("user");
    let (status, headers, body) = admin
        .send_headers(Method::PUT, "/vms/web", json!({ "spec": spec.clone() }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("orbivisor-warning").is_none(), "{headers:?}");
    spec["cloud_init"]["ssh_keys"] = json!(["ssh-ed25519 AAAAC3Nza orbi@host"]);
    let (status, headers, body) = admin
        .send_headers(Method::PUT, "/vms/web", json!({ "spec": spec }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["orbivisor-warning"], WARNING);

    let mut req = vm_request("db");
    req["spec"]["cloud_init"] = json!({ "user": "orbi" });
    let (_, headers, _) = admin.send_headers(Method::POST, "/vms", req).await;
    assert!(headers.get("orbivisor-warning").is_none(), "{headers:?}");
}

/// user-data·vendor-data는 저장되고 VM 쓰기 권한자에게는 그대로 보이지만(고쳐 보내야 한다), 읽기만 하는 주체(viewer)
/// 에게는 VM·목록·스냅샷·백업 응답에서 가려진다. 가린 값을 되보내면 저장된 값이 남고, 저장된 것이 없으면 400이다.
#[tokio::test]
async fn user_data_is_hidden_from_readers() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("vic", "viewer").await;
    let secret = "#cloud-config\nchpasswd: {list: 'root:s3cret'}\n";
    let mut req = vm_request("web");
    req["spec"]["cloud_init"] =
        json!({ "user_data": secret, "vendor_data": "#cloud-config\n{}\n" });
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["spec"]["cloud_init"]["user_data"], secret);
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: id,
            name: "s1".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: env.cluster.get::<VmSpec>(id).unwrap().unwrap().spec,
            hardware: None,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();

    let (_, got) = admin.get("/vms/web").await;
    assert_eq!(got["spec"]["cloud_init"]["user_data"], secret);
    for path in [
        "/vms/web",
        "/vms",
        "/vms/web/snapshots",
        "/vms/web/snapshots/s1",
    ] {
        let (status, body) = viewer.get(path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let text = body.to_string();
        assert!(!text.contains("s3cret"), "{path}: {text}");
        assert!(text.contains(PASSWORD_MASK), "{path}: {text}");
    }

    // 가린 값을 되보내면(웹·CLI가 받은 스펙을 고쳐 보냄) 저장된 값이 남는다.
    let mut spec = vm["spec"].clone();
    spec["cloud_init"]["user_data"] = json!(PASSWORD_MASK);
    spec["cloud_init"]["vendor_data"] = json!(PASSWORD_MASK);
    spec["cloud_init"]["hostname"] = json!("web-01");
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = env.cluster.get::<VmSpec>(id).unwrap().unwrap().spec;
    let ci = stored.cloud_init.unwrap();
    assert_eq!(ci.user_data.as_deref(), Some(secret));
    assert_eq!(ci.vendor_data.as_deref(), Some("#cloud-config\n{}\n"));
    assert_eq!(ci.hostname.as_deref(), Some("web-01"));

    let mut req = vm_request("db");
    req["spec"]["cloud_init"] = json!({ "user_data": PASSWORD_MASK });
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "cloud_init.user_data: give the user_data itself"
    );
}

/// cloud-init이 해시로 보지 않는 yescrypt·bcrypt-2b 해시는 평문으로 설정되므로 400이다. 해시 모양을 엄격히 보아 모양이
/// 어긋난 값은 평문으로 해시한다.
#[tokio::test]
async fn unsupported_crypt_hashes_are_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for hash in [
        "$y$j9T$abc$def",
        "$2b$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy",
    ] {
        let (status, body) = create_web(&admin, hash).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            body["message"],
            "cloud_init.password: cloud-init cannot use yescrypt/bcrypt-2b hashes; use a SHA-512 crypt hash ($6$, e.g. `mkpasswd -m sha-512`) or a plain password; plain passwords may not start with $y$ or $2b$"
        );
    }
    let (status, body) = create_web(&admin, "$6$salt$short").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let stored = stored_password(&env, "web").unwrap();
    assert_ne!(stored, "$6$salt$short");
    let salt = stored
        .strip_prefix("$6$")
        .unwrap()
        .split('$')
        .next()
        .unwrap();
    assert_eq!(sha512_crypt(b"$6$salt$short", salt), stored);
}

/// 실행 중에 바꾼 cloud-init은 다음 QEMU 시작에 적용된다: 핫플러그 Task는 보류 `cloud_init`(Restart)만 남긴다.
#[tokio::test]
async fn cloud_init_changed_while_running_is_pending() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["cloud_init"] = json!({ "user": "orbi" });
    assert_eq!(admin.post("/vms", req).await.0, StatusCode::CREATED);
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let id: Uuid = task["spec"]["target"].as_str().unwrap().parse().unwrap();
    let started = env.hv.seed(id).expect("seed");
    let hw = env
        .cluster
        .get::<VmSpec>(id)
        .unwrap()
        .unwrap()
        .status
        .hardware
        .unwrap();
    assert_eq!(hw.cloud_init.as_deref(), Some(started.instance_id.as_str()));

    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["cloud_init"]["hostname"] = json!("web-2");
    let (status, task, body) = admin.put_task("/vms/web", json!({ "spec": spec })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = admin.wait_task(&json!({ "id": task.unwrap() })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(
        task["status"]["error"],
        "pending until restart: cloud_init (restart)"
    );
    let hw = env
        .cluster
        .get::<VmSpec>(id)
        .unwrap()
        .unwrap()
        .status
        .hardware
        .unwrap();
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    let p = &hw.pending[0];
    assert_eq!(p.field, "cloud_init");
    assert_eq!(p.reason, PendingReason::Restart);
    assert_eq!(p.running, started.instance_id);
    assert_ne!(p.wanted, started.instance_id);
    // 도는 QEMU의 씨앗은 그대로다.
    assert_eq!(env.hv.seed(id), Some(started));
    // 대상은 씨앗을 새 스펙으로 만들므로 옮기지 않는다.
    let me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut other: orbivisor_types::node::Node = Resource::new("node2", me.spec.clone());
    other.status = me.status;
    env.cluster.create(&other).await.unwrap();
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "restart_required");
    assert_eq!(
        body["message"],
        "cannot migrate VM \"web\": cloud-init changed while running; restart the VM before migrating it"
    );
}

/// `install-media`를 모르는 노드가 있으면 CD-ROM 드라이브·cloud-init을 쓰는 스펙과 매체 바꾸기는 409다(옛 노드는
/// 새 필드가 든 VM을 풀지 못한다). 두 필드가 없는 VM은 그대로 된다.
#[tokio::test]
async fn old_nodes_refuse_cdroms_and_cloud_init() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != INSTALL_MEDIA);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();

    let (status, body) = create_web(&admin, "pw").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    let mut req = vm_request("web");
    req["spec"]["cdroms"] = json!([{}]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");

    let (status, body) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["spec"].get("cdroms").is_none(), "{body}");
    assert!(body["spec"].get("cloud_init").is_none(), "{body}");
    let (status, body) = admin.post("/vms/web/cdrom", json!({ "iso": "x" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}
