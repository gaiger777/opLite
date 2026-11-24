//! VM 내보내기(계획 72 §12): 꺼진 VM을 dir 풀에 OVF 패키지(서술자·가짜 VMDK·SHA-256 매니페스트)로 쓰고, 서명한 링크로 OVA를
//! 흘려 받는다. 받은 OVA를 계획 29의 OVA 올리기로 다시 읽어 같은 VM이 나오는지(왕복) 본다. qemu-img 없이 `FakeStorage`가
//! VMDK 자리에 가짜 바이트를 쓴다(실제 VMDK 왕복은 `storage-it`의 `vm_import_tasks`).

mod common;

use std::sync::Arc;

use common::{Client, Env, FakeStorage, mark_pool_ready, vm_request};
use opl_types::node::{NodeSpec, VM_EXPORT};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::vm_export::{VmExport, VmExportSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::Digest as _;
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// dir 풀 `name`을 저장소에 직접 만들고 이 노드에서 준비됐다고 적는다.
async fn pool(env: &Env, name: &str) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    mark_pool_ready(&env.cluster, env.node, name).await;
}

/// 풀 `local`의 Ready 볼륨(이 노드)
async fn volume(env: &Env, storage: &FakeStorage, name: &str) -> Uuid {
    let mut v = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 64 * MIB,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    env.cluster.create(&v).await.unwrap();
    v.status.phase = VolumePhase::Ready;
    v.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(v.id, &v.status)
        .await
        .unwrap();
    storage.place(v.id, 64 * MIB);
    v.id
}

/// 디스크 둘·MAC 고정 NIC 하나인 꺼진 VM `web`과 내보내기를 받을 dir 풀 `local`. Task 실행기는 `FakeStorage`를 쓴다.
async fn setup() -> (Env, Client, Arc<FakeStorage>, [Uuid; 2]) {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local").await;
    let storage = Arc::new(FakeStorage::default());
    storage.enable_sources();
    let disks = [
        volume(&env, &storage, "web-root").await,
        volume(&env, &storage, "web-data").await,
    ];
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "sockets": 1, "cores": 2 });
    req["spec"]["disks"] = json!([
        { "volume": "web-root", "bus": "virtio-blk" },
        { "volume": "web-data", "bus": "virtio-blk" },
    ]);
    req["spec"]["nics"] =
        json!([{ "network": "user", "model": "virtio", "mac": "52:54:00:12:34:56" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (env, admin, storage, disks)
}

fn export_dir(env: &Env, id: &str) -> std::path::PathBuf {
    env.dir.path().join("local/orbivisor-exports").join(id)
}

/// 내보내기를 시작하고 Task가 끝날 때까지 기다린 뒤 내보내기를 돌려준다.
async fn export(admin: &Client, body: Value) -> (Value, Value) {
    let (status, started) = admin.post("/vms/web/export", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    let task = admin.wait_task(&started["task"]).await;
    let name = started["export"]["name"].as_str().unwrap().to_string();
    let (status, export) = admin.get(&format!("/vm-exports/{name}")).await;
    assert_eq!(status, StatusCode::OK, "{export}");
    (task, export)
}

/// 링크를 받아 내려받는다: (상태, 머리, 본문)
async fn download(
    admin: &Client,
    export: &str,
    query: &str,
) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
    let (status, link) = admin
        .post(&format!("/vm-exports/{export}/download-link"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    assert_eq!(link["expires_in"], 600);
    fetch(admin, &format!("{}{query}", link["url"].as_str().unwrap())).await
}

async fn fetch(admin: &Client, url: &str) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
    let resp = admin
        .http
        .get(format!("{}/api/v1{url}", admin.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    (status, headers, resp.bytes().await.unwrap().to_vec())
}

fn sha(b: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(b))
}

/// 꺼진 VM을 내보내면 풀에 서술자·매니페스트·디스크가 생기고(`ready`, 파일마다 크기·해시, OVA 크기), 링크로 받은 OVA는
/// `Content-Length`가 정확하고 계획 29의 OVA 올리기가 같은 VM(디스크·NIC·MAC·CPU·메모리·guestId)으로 읽는다. 파일 하나도
/// 받는다(`format=ovf`). 지우면 디렉터리와 리소스가 없어진다.
#[tokio::test]
async fn exports_a_stopped_vm_and_streams_an_ova_that_imports_back() {
    let (env, admin, storage, disks) = setup().await;
    let (task, ex) = export(
        &admin,
        json!({ "pool": "local", "name": "web-ova", "guest_os": "ubuntu64Guest", "keep_macs": true }),
    )
    .await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-export");
    let st = &ex["status"];
    assert_eq!(st["phase"], "ready", "{ex}");
    assert_eq!(ex["spec"]["hw_version"], 14);
    assert_eq!(ex["spec"]["node"], env.node.to_string());
    let names: Vec<&str> = st["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["web.ovf", "web.mf", "web-disk0.vmdk", "web-disk1.vmdk"]
    );
    // 디스크마다 한 번(VM 디스크 차례)
    let calls: Vec<Uuid> = storage
        .calls()
        .into_iter()
        .filter(|(c, _)| *c == "export_vmdk")
        .map(|(_, id)| id)
        .collect();
    assert_eq!(calls, disks);
    let id = ex["id"].as_str().unwrap();
    let dir = export_dir(&env, id);
    for f in st["files"].as_array().unwrap() {
        let bytes = std::fs::read(dir.join(f["name"].as_str().unwrap())).unwrap();
        assert_eq!(f["size"], bytes.len() as u64);
        assert_eq!(f["sha256"], sha(&bytes));
    }
    assert_eq!(
        std::fs::read(dir.join("web-disk1.vmdk")).unwrap(),
        FakeStorage::vmdk_bytes(disks[1])
    );
    let mf = std::fs::read_to_string(dir.join("web.mf")).unwrap();
    assert!(mf.starts_with("SHA256(web.ovf)= "), "{mf}");
    assert_eq!(mf.lines().count(), 3);

    let (status, headers, ova) = download(&admin, "web-ova", "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/x-tar");
    assert_eq!(
        headers["content-length"],
        st["ova_bytes"].to_string().as_str()
    );
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("filename=\"web.ova\"")
    );
    assert_eq!(ova.len() as u64, st["ova_bytes"].as_u64().unwrap());
    // 첫 항목은 서술자(DSP0243)
    assert_eq!(&ova[..7], b"web.ovf");
    assert_eq!(&ova[257..263], b"ustar\0");

    // 왕복: 계획 29의 OVA 올리기가 읽는다(매니페스트가 디스크를 확인한다 — "not verified" 경고가 없다).
    let (status, imported) = admin.upload("/vm-imports/ova?name=web-back", ova).await;
    assert_eq!(status, StatusCode::CREATED, "{imported}");
    let source = &imported["status"]["source_vm"];
    assert_eq!(source["name"], "web");
    assert_eq!(source["guest_os"], "ubuntu64Guest");
    assert_eq!(source["vcpus"], 2);
    assert_eq!(source["cores_per_socket"], 2);
    assert_eq!(source["memory_mib"], 256);
    assert_eq!(
        source["disks"],
        json!([
            { "name": "disk0", "file": "web-disk0.vmdk", "capacity_bytes": 64 * MIB, "controller": "scsi" },
            { "name": "disk1", "file": "web-disk1.vmdk", "capacity_bytes": 64 * MIB, "controller": "scsi" },
        ])
    );
    assert_eq!(
        source["nics"],
        json!([{ "network": "user", "adapter": "vmxnet3", "mac": "52:54:00:12:34:56" }])
    );
    let warnings = imported["status"]["warnings"].as_array().unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("not verified")),
        "{warnings:?}"
    );

    // 감사: 내보내기·링크·내려받기(크기와 함께). 링크 토큰은 경로에 남기지 않는다.
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let (_, audit) = admin.get("/audit?limit=500").await;
        let text = audit.to_string();
        if text.contains("vm export web → pool local")
            && text.contains("vm-export download-link web-ova")
            && text.contains("vm-export download web-ova (OVA, ")
        {
            assert!(!text.contains(&format!("/download/{}", "1")), "{text}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "audit entries missing: {text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // 파일 하나(OVF 폴더로 받기)
    let (status, headers, ovf) = download(&admin, "web-ova", "?format=ovf&file=web.ovf").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/xml");
    assert_eq!(ovf, std::fs::read(dir.join("web.ovf")).unwrap());
    for (query, code) in [
        ("?format=ovf", StatusCode::BAD_REQUEST),
        ("?file=web.ovf", StatusCode::BAD_REQUEST),
        ("?format=ovf&file=nope.vmdk", StatusCode::NOT_FOUND),
    ] {
        let (status, _, _) = download(&admin, "web-ova", query).await;
        assert_eq!(status, code, "{query}");
    }

    // 지우기: 디렉터리와 리소스
    let (status, task) = admin.delete("/vm-exports/web-ova").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(!dir.exists());
    assert_eq!(
        admin.get("/vm-exports/web-ova").await.0,
        StatusCode::NOT_FOUND
    );
}

/// 링크는 만든 사용자의 것이다(남이면 403), 틀리거나 다른 내보내기의 링크는 410 `link_expired`. 기본 이름은
/// `{VM}-{시각}`이고, guestId를 모르면 `otherGuest64`와 경고를 남긴다.
#[tokio::test]
async fn links_belong_to_their_user() {
    let (env, admin, _storage, _) = setup().await;
    let (_, ex) = export(&admin, json!({ "pool": "local" })).await;
    let name = ex["name"].as_str().unwrap();
    assert!(name.starts_with("web-2"), "{name}");
    let warnings = ex["status"]["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("otherGuest64")),
        "{warnings:?}"
    );
    let ovf = std::fs::read_to_string(export_dir(&env, ex["id"].as_str().unwrap()).join("web.ovf"))
        .unwrap();
    assert!(ovf.contains(r#"vmw:osType="otherGuest64""#), "{ovf}");
    // MAC을 지키지 않으면 서술자에 주소가 없다.
    assert!(!ovf.contains("52:54:00:12:34:56"));

    let (_, link) = admin
        .post(&format!("/vm-exports/{name}/download-link"), json!({}))
        .await;
    let url = link["url"].as_str().unwrap();
    let other = env.user("bob", "admin").await;
    let (status, _, body) = fetch(&other, url).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let id = ex["id"].as_str().unwrap();
    for bad in [
        format!("/vm-exports/{id}/download/x"),
        format!(
            "/vm-exports/{id}/download/1.{}.AAAA",
            Uuid::now_v7().simple()
        ),
        url.replacen(id, &Uuid::now_v7().to_string(), 1),
    ] {
        let (status, _, body) = fetch(&admin, &bad).await;
        let want = if bad.contains(id) {
            StatusCode::GONE
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(status, want, "{bad}: {}", String::from_utf8_lossy(&body));
    }
    let (status, _, _) = fetch(&admin, url).await;
    assert_eq!(status, StatusCode::OK);
}

/// 막는 것: 켜진 VM(409 `vm_running`), 파일을 둘 수 없는 풀(400 `pool_not_file`), 없는 풀(400), 틀린 하드웨어 버전·guestId
/// (400), `vm.export`가 없는 operator(403), 옛 노드가 남았을 때(409 `cluster_upgrading`). 쓰는 동안 VM 켜기는 409
/// `vm_exporting`, 지우기는 409 `export_busy`. 내보내기가 있는 풀은 지울 수 없다(409 `pool_in_use`).
#[tokio::test]
async fn refuses_what_it_cannot_export() {
    let (env, admin, storage, _) = setup().await;
    let lvm = Resource::new(
        "thin",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&lvm).await.unwrap();
    let cases = [
        (
            json!({ "pool": "thin" }),
            StatusCode::BAD_REQUEST,
            "pool_not_file",
        ),
        (
            json!({ "pool": "nope" }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            json!({ "pool": "local", "hw_version": 6 }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            json!({ "pool": "local", "guest_os": "a b" }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            json!({ "pool": "local", "extra": 1 }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
    ];
    for (body, status, code) in cases {
        let (got, resp) = admin.post("/vms/web/export", body.clone()).await;
        assert_eq!(got, status, "{body}: {resp}");
        assert_eq!(resp["code"], code, "{body}: {resp}");
    }
    let operator = env.user("olga", "operator").await;
    let (status, body) = operator
        .post("/vms/web/export", json!({ "pool": "local" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // 보기는 system.read로 된다.
    assert_eq!(operator.get("/vm-exports").await.0, StatusCode::OK);

    // 쓰는 동안
    storage.pause();
    let (status, started) = admin
        .post(
            "/vms/web/export",
            json!({ "pool": "local", "name": "busy" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    storage.reached().await;
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_exporting");
    let (status, body) = admin.delete("/vm-exports/busy").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "export_busy");
    let (status, body) = admin
        .post("/vm-exports/busy/download-link", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "export_not_ready");
    storage.resume();
    let task = admin.wait_task(&started["task"]).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = admin.delete("/storage-pools/local").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use");

    // 켜진 VM
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = admin
        .post("/vms/web/export", json!({ "pool": "local" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");

    // 옛 노드
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.features.retain(|f| f != VM_EXPORT);
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/export", json!({ "pool": "local" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
}

/// 디스크를 쓰다 실패하면 디렉터리를 지우고 `failed`와 이유를 남긴다. 실패한 내보내기는 받을 수 없고(409
/// `export_not_ready`) 지울 수 있다.
#[tokio::test]
async fn a_failed_export_leaves_no_files() {
    let (env, admin, storage, _) = setup().await;
    storage.fail_exports(1);
    let (task, ex) = export(&admin, json!({ "pool": "local", "name": "broken" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("No space left"),
        "{task}"
    );
    assert_eq!(ex["status"]["phase"], "failed", "{ex}");
    assert!(ex["status"]["error"].as_str().is_some());
    assert!(!export_dir(&env, ex["id"].as_str().unwrap()).exists());
    let (status, body) = admin
        .post("/vm-exports/broken/download-link", json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "export_not_ready");
    let (status, task) = admin.delete("/vm-exports/broken").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(admin.wait_task(&task).await["status"]["state"], "succeeded");
    let left: Vec<VmExport> = env.cluster.list::<VmExportSpec>().unwrap().1;
    assert!(left.is_empty());
}
