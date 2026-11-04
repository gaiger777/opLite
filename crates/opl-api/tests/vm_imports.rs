//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 실행 요청의 검사, 지우기와
//! 강제 지우기, 노드 시작 때의 정리. ESXi 연결(browse)과 ESXi에서 가져오기 만들기.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::esxi::MockEsxi;
use common::ustar::Tar;
use common::{Client, Env, vm_request};
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm_import::{
    EsxiSource, ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const VSPHERE: &str = include_str!("fixtures/vsphere.ovf");

/// vSphere 견본 OVF(두 번째 네트워크 이름을 `lan`으로 바꿈)와 그 파일들
fn vsphere_ova() -> Vec<u8> {
    let ovf = VSPHERE.replace("Backup Net", "lan");
    let disk = b"KDMV fake disk";
    Tar::new()
        .file("web01.ovf", ovf.as_bytes())
        .file("web01-disk1.vmdk", disk)
        .file("web01-disk2.vmdk", disk)
        .file("web01-disk3.vmdk", disk)
        .file("web01-file1.nvram", b"nvram")
        .finish()
}

async fn network(env: &Env, name: &str) {
    let network = Resource::new(
        name,
        NetworkSpec {
            bridge: Some("br0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&network).await.unwrap();
}

fn imports_dir(env: &Env) -> PathBuf {
    env.dir.path().join("uploads").join("imports")
}

/// `imports` 아래의 디렉터리 이름
fn import_dirs(env: &Env) -> Vec<String> {
    match std::fs::read_dir(imports_dir(env)) {
        Ok(entries) => entries
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

async fn check(client: &Client, name: &str, size: u64) -> (StatusCode, Value) {
    client
        .post(
            &format!("/vm-imports/ova/check?name={name}&size={size}"),
            Value::Null,
        )
        .await
}

/// 저장소에 가져오기 리소스를 직접 만든다.
async fn import_on(env: &Env, name: &str, node: Uuid, phase: ImportPhase) -> VmImport {
    let mut import: VmImport = Resource::new(
        name,
        VmImportSpec {
            node,
            source: ImportSource::Ova(OvaSource {
                file_name: format!("{name}.ova"),
                size_bytes: 1,
            }),
        },
    );
    import.status = VmImportStatus {
        phase,
        ..Default::default()
    };
    env.cluster.create(&import).await.unwrap();
    import
}

#[tokio::test]
async fn uploads_an_ova_and_proposes_a_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    network(&env, "lan").await;
    let ova = vsphere_ova();

    let (status, body) = check(&admin, "web01", ova.len() as u64).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let (status, body) = admin
        .upload(
            "/vm-imports/ova?name=web01&file_name=web01.ova",
            ova.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["name"], "web01");
    assert_eq!(body["spec"]["node"], env.node.to_string());
    assert_eq!(
        body["spec"]["source"],
        json!({ "ova": { "file_name": "web01.ova", "size_bytes": ova.len() } })
    );
    let st = &body["status"];
    assert_eq!(st["phase"], "ready");
    assert_eq!(st["source_vm"]["name"], "web01");
    assert_eq!(st["source_vm"]["disks"].as_array().unwrap().len(), 3);
    assert_eq!(
        st["proposal"],
        json!({
            "sockets": 2, "cores": 2, "memory_mib": 8192, "firmware": "uefi",
            "disk_bus": "virtio-blk", "nic_model": "virtio",
            // 이름이 같은 네트워크만 매핑한다.
            "networks": { "VM Network": null, "lan": "lan" },
        })
    );
    // 해석 경고 뒤에 제안 경고
    let warnings: Vec<&str> = st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(warnings[0], "CD-ROM drive is not imported");
    assert!(warnings.iter().any(|w| w.starts_with("UEFI boot entries")));
    assert!(
        warnings
            .iter()
            .any(|w| w.starts_with("power off the VMware VM"))
    );
    assert!(st.get("error").is_none() && st.get("vm").is_none(), "{st}");

    // 원본은 `imports/{id}/source.ova`에 남는다.
    let id = body["id"].as_str().unwrap();
    let source = imports_dir(&env).join(id).join("source.ova");
    assert_eq!(std::fs::read(&source).unwrap(), ova);

    let (status, list) = admin.get("/vm-imports").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["items"][0]["id"], id);
    let (status, got) = admin.get("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"], body["status"]);
    let (status, got) = admin.get(&format!("/vm-imports/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{got}");

    // 파일 이름이 없으면 `{이름}.ova`
    let (status, body) = admin.upload("/vm-imports/ova?name=web02", ova).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["source"]["ova"]["file_name"], "web02.ova");
}

#[tokio::test]
async fn invalid_ova_is_rejected_and_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // OVF가 가리키는 디스크가 tar에 없다.
    let ova = Tar::new()
        .file("web01.ovf", VSPHERE.as_bytes())
        .file("web01-disk1.vmdk", b"disk")
        .finish();
    let (status, body) = admin.upload("/vm-imports/ova?name=web01", ova).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_ova");
    assert_eq!(
        body["message"],
        "file \"web01-disk2.vmdk\" is not in the OVA"
    );

    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", b"not a tar".repeat(100))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_ova");

    let (_, list) = admin.get("/vm-imports").await;
    assert_eq!(list["items"], json!([]));
    // 지우기는 응답 뒤에 끝날 수 있다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while !import_dirs(&env).is_empty() {
        assert!(std::time::Instant::now() < deadline, "upload left behind");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn check_and_upload_reject_with_the_same_codes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    import_on(&env, "taken", env.node, ImportPhase::Ready).await;

    for (name, size, want, code) in [
        (
            "Bad_Name",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        // `/vm-imports/ova` 경로와 겹친다.
        ("ova", 1 << 20, StatusCode::BAD_REQUEST, "invalid_request"),
        ("taken", 1 << 20, StatusCode::CONFLICT, "name_taken"),
        (
            "web01",
            u64::MAX,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
    ] {
        let (status, body) = check(&admin, name, size).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (want, Some(code)),
            "{name} {body}"
        );
    }

    // 올리기도 같은 확인을 거친다. 받기 전에 거절하므로 남는 파일이 없다.
    let (status, body) = admin
        .upload("/vm-imports/ova?name=taken", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    assert_eq!(body["message"], "vm-import \"taken\" already exists");
    assert!(import_dirs(&env).is_empty());

    // 크기가 없으면 확인할 수 없다.
    let (status, body) = admin
        .post("/vm-imports/ova/check?name=web01", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn operators_upload_and_viewers_only_read() {
    let env = Env::start().await;
    let operator = env.user("op", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = check(&viewer, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = viewer
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = check(&operator, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, body) = operator
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, list) = viewer.get("/vm-imports").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (status, body) = viewer.get("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = viewer.get("/vm-imports/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn witness_nodes_do_not_take_uploads() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let spec = NodeSpec {
        witness: true,
        ..me.spec.clone()
    };
    env.cluster
        .update_spec(env.node, None, me.labels.clone(), &spec)
        .await
        .unwrap();

    let (status, body) = check(&admin, "web01", 1 << 20).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("witness"),
        "{body}"
    );
    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(import_dirs(&env).is_empty());
}

/// 노드가 시작할 때: 리소스가 없는 디렉터리는 지우고, 원본이 없는 이 노드의 `ready` 가져오기는
/// `failed`로 바꾼다. 다른 노드의 가져오기와 원본이 있는 가져오기는 그대로 둔다.
#[tokio::test]
async fn startup_sweeps_import_directories() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let kept = import_on(&env, "kept", env.node, ImportPhase::Ready).await;
    let gone = import_on(&env, "gone", env.node, ImportPhase::Ready).await;
    let done = import_on(&env, "done", env.node, ImportPhase::Done).await;
    let other = import_on(&env, "other", Uuid::now_v7(), ImportPhase::Ready).await;
    let kept_dir = imports_dir(&env).join(kept.id.to_string());
    std::fs::create_dir_all(&kept_dir).unwrap();
    std::fs::write(kept_dir.join("source.ova"), b"ova").unwrap();
    let orphan = imports_dir(&env).join(Uuid::now_v7().to_string());
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("source.ova"), b"ova").unwrap();
    // 원본 없는 디렉터리(올리기 도중 죽었다)도 리소스가 있으면 남는다.
    let empty = imports_dir(&env).join(gone.id.to_string());
    std::fs::create_dir_all(&empty).unwrap();
    // 볼륨 업로드 정리는 그대로 한다.
    let stray = env.dir.path().join("uploads").join("stray.upload");
    std::fs::write(&stray, b"x").unwrap();

    env.start_runner().await;

    assert!(!orphan.exists());
    assert!(kept_dir.join("source.ova").exists());
    assert!(empty.exists());
    assert!(!stray.exists());
    let phase = |id: Uuid| {
        let import = env.cluster.get::<VmImportSpec>(id).unwrap().unwrap();
        (import.status.phase, import.status.error)
    };
    assert_eq!(phase(kept.id), (ImportPhase::Ready, None));
    assert_eq!(
        phase(gone.id),
        (ImportPhase::Failed, Some("source files are gone".into()))
    );
    assert_eq!(phase(done.id), (ImportPhase::Done, None));
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}

/// 이 노드에서 준비된 dir 풀. `nodes`가 비어 있지 않으면 그 노드에서만 쓸 수 있다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let dir = env.dir.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: dir },
            nodes,
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 네트워크 `lan`, 풀 `local`, 올린 가져오기 `web01`(vSphere 견본: 디스크 셋, NIC 둘)
async fn uploaded(env: &Env, admin: &Client) -> Value {
    network(env, "lan").await;
    pool(env, "local", vec![]).await;
    let (status, body) = admin
        .upload("/vm-imports/ova?name=web01", vsphere_ova())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

/// 원본 네트워크를 모두 `lan`에 매핑한 실행 요청
fn run_request(name: &str) -> Value {
    json!({
        "name": name,
        "pool": "local",
        "networks": { "VM Network": "lan", "lan": "lan" },
    })
}

async fn run(client: &Client, import: &str, body: Value) -> (StatusCode, Value) {
    client
        .post(&format!("/vm-imports/{import}/run"), body)
        .await
}

fn import_named(env: &Env, name: &str) -> Option<VmImport> {
    env.cluster.get_by_name::<VmImportSpec>(name).unwrap()
}

async fn set_phase(env: &Env, name: &str, phase: ImportPhase) {
    let import = import_named(env, name).unwrap();
    let status = VmImportStatus {
        phase,
        ..import.status
    };
    env.cluster
        .update_status::<VmImportSpec>(import.id, &status)
        .await
        .unwrap();
}

/// Task를 저장소에 직접 만든다(API의 검사 없이).
async fn put_task(env: &Env, op: TaskOp, target: Uuid, state: TaskState) -> Value {
    let mut task: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    if state != TaskState::Pending {
        let status = TaskStatus { state, error: None };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    json!({ "id": task.id })
}

/// 실행 요청은 Task를 만들기 전에 매핑·이름·풀·MAC과 결과 VM 스펙을 확인한다.
#[tokio::test]
async fn run_checks_the_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let (status, vm) = admin.post("/vms", vm_request("taken")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let volume = Resource::new(
        "vol-disk1",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: opl_types::runtime::ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    pool(&env, "far", vec![Uuid::now_v7()]).await;
    // 원본의 첫 NIC과 같은 MAC을 쓰는 VM
    let mut mac = vm_request("mac");
    mac["spec"]["nics"][0]["mac"] = json!("00:50:56:8a:12:34");
    let (status, vm) = admin.post("/vms", mac).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let with = |patch: Value| {
        let mut body = run_request("web01");
        for (k, v) in patch.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    for (body, want, code, message) in [
        (
            with(json!({ "networks": { "lan": "lan" } })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("no network mapping for \"VM Network\""),
        ),
        (
            with(json!({ "networks": { "VM Network": "nope", "lan": "lan" } })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("network \"nope\" not found"),
        ),
        (
            with(json!({ "name": "Bad_Name" })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            None,
        ),
        (
            with(json!({ "name": "taken" })),
            StatusCode::CONFLICT,
            "name_taken",
            Some("VM \"taken\" already exists"),
        ),
        (
            with(json!({ "name": "vol" })),
            StatusCode::CONFLICT,
            "name_taken",
            Some("volume \"vol-disk1\" already exists"),
        ),
        (
            with(json!({ "pool": "nope" })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("storage pool \"nope\" not found"),
        ),
        (
            with(json!({ "pool": "far" })),
            StatusCode::CONFLICT,
            "pool_not_on_node",
            None,
        ),
        (
            with(json!({ "sockets": 0 })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("cpu.sockets must be at least 1"),
        ),
        (
            run_request("web01"),
            StatusCode::CONFLICT,
            "mac_in_use",
            Some("MAC address 00:50:56:8a:12:34 is already in use"),
        ),
        (
            with(json!({ "surprise": 1 })),
            StatusCode::BAD_REQUEST,
            "invalid_request",
            None,
        ),
    ] {
        let (status, got) = run(&admin, "web01", body.clone()).await;
        assert_eq!(
            (status, got["code"].as_str()),
            (want, Some(code)),
            "{body} {got}"
        );
        if let Some(message) = message {
            assert_eq!(got["message"], message, "{body}");
        }
    }
    // 거절한 요청은 가져오기를 바꾸지 않는다.
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(import.status.phase, ImportPhase::Ready);
    assert_eq!(import.status.error, None);

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = run(&viewer, "web01", run_request("web01")).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = run(&admin, "nope", run_request("web01")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

/// 받은 요청은 가져오기를 `importing`으로 바꾸고 새 VM id를 대상으로 한 Task를 돌려준다. 그동안 다시
/// 실행하거나 지울 수 없다. Task가 실패하면 `ready`로 돌아가 이유를 남긴다(여기서는 가짜 디스크라 실패한다).
#[tokio::test]
async fn run_starts_a_task_and_failure_returns_to_ready() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let mut body = run_request("web01");
    // 새 MAC을 받으면 원본 MAC이 겹쳐도 된다.
    body["keep_macs"] = json!(false);
    // 요청에 없는 원본 네트워크 `lan`은 제안의 매핑(이름이 같은 네트워크)을 쓴다.
    body["networks"] = json!({ "VM Network": "lan" });
    let mut mac = vm_request("mac");
    mac["spec"]["nics"][0]["mac"] = json!("00:50:56:8a:12:34");
    let (status, vm) = admin.post("/vms", mac).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");

    let (status, task) = run(&admin, "web01", body.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(task["spec"]["op"]["kind"], "vm-import");
    assert_eq!(task["spec"]["op"]["import"], import.id.to_string());
    assert_eq!(task["spec"]["op"]["run"]["keep_macs"], false);
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let target = task["spec"]["target"].as_str().unwrap();
    assert_ne!(target, import.id.to_string());

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let import = import_named(&env, "web01").unwrap();
    assert_eq!(import.status.phase, ImportPhase::Ready);
    let error = import.status.error.unwrap();
    assert!(error.starts_with("cannot import disk \"disk0\""), "{error}");
    assert_eq!(task["status"]["error"], error.as_str());
    // 추출한 파일은 지우고 원본은 남긴다.
    let dir = imports_dir(&env).join(import.id.to_string());
    assert!(!dir.join("disk.vmdk").exists());
    assert!(dir.join("source.ova").exists());
    let (_, volumes) = env.cluster.list::<VolumeSpec>().unwrap();
    assert!(volumes.is_empty(), "{volumes:?}");
}

/// 실행 Task가 끝나지 않은 가져오기와 끝난·쓸 수 없는 가져오기는 실행하지 않는다. 실행·지우기 Task가
/// 끝나지 않았으면 지우지도 않는다(`force`여도).
#[tokio::test]
async fn run_and_delete_need_an_idle_import() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let id = import_named(&env, "web01").unwrap().id;
    env.stop_runner().await;
    let op = TaskOp::VmImport {
        import: id,
        run: serde_json::from_value(run_request("web01")).unwrap(),
    };
    put_task(&env, op, Uuid::now_v7(), TaskState::Running).await;
    for (phase, code) in [
        (ImportPhase::Importing, "import_busy"),
        (ImportPhase::Done, "import_done"),
        (ImportPhase::Failed, "import_failed"),
    ] {
        set_phase(&env, "web01", phase).await;
        let (status, body) = run(&admin, "web01", run_request("web01")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some(code)),
            "{body}"
        );
    }
    set_phase(&env, "web01", ImportPhase::Importing).await;
    for path in ["/vm-imports/web01", "/vm-imports/web01?force=true"] {
        let (status, body) = admin.delete(path).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some("import_busy")),
            "{body}"
        );
    }

    // 끝나지 않은 지우기 Task가 있어도 바쁘다.
    set_phase(&env, "web01", ImportPhase::Ready).await;
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    for task in tasks {
        let status = TaskStatus {
            state: TaskState::Failed,
            error: None,
        };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    put_task(&env, TaskOp::VmImportDelete, id, TaskState::Pending).await;
    let (status, body) = run(&admin, "web01", run_request("web01")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_busy")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/web01").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("import_busy")),
        "{body}"
    );
}

/// 원본 노드가 없어져 실행 Task가 실패로 끝나면(정리가 돌지 않아) 가져오기는 `importing`에 남는다. 끝나지
/// 않은 Task가 없으면 쉬는 것으로 보아 다시 실행하거나 지울 수 있고, 노드가 없으면 `force`로 지운다.
#[tokio::test]
async fn importing_without_a_running_task_is_idle() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let web01 = import_named(&env, "web01").unwrap();
    let failed = |import: Uuid| TaskOp::VmImport {
        import,
        run: serde_json::from_value(run_request("web01")).unwrap(),
    };
    put_task(&env, failed(web01.id), Uuid::now_v7(), TaskState::Failed).await;
    set_phase(&env, "web01", ImportPhase::Importing).await;

    // 다시 실행할 수 있다(가짜 디스크라 실패해 `ready`로 돌아간다).
    let (status, task) = run(&admin, "web01", run_request("web01")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        import_named(&env, "web01").unwrap().status.phase,
        ImportPhase::Ready
    );

    // 지우기 Task도 지운다.
    set_phase(&env, "web01", ImportPhase::Importing).await;
    let (status, task) = admin.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import_named(&env, "web01").is_none());
    assert!(!imports_dir(&env).join(web01.id.to_string()).exists());

    // 클러스터에서 없어진 노드의 가져오기는 `force`로 지운다.
    let far = import_on(&env, "far", Uuid::now_v7(), ImportPhase::Importing).await;
    put_task(&env, failed(far.id), Uuid::now_v7(), TaskState::Failed).await;
    let (status, body) = admin.delete("/vm-imports/far").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/far?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(import_named(&env, "far").is_none());
}

/// 끝난 실행 Task가 대상 VM을 이미 만들었는데 가져오기가 `done`이 되지 못했으면(VM을 만든 뒤 opld가
/// 죽었다), 실행 요청이 가져오기를 `done`으로 끝내고 409 `import_done`이다. 이 노드의 원본 디렉터리는 지우고
/// VM은 둔다. `importing`이든 `ready`든 같다.
#[tokio::test]
async fn run_finishes_an_import_whose_vm_was_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    uploaded(&env, &admin).await;
    let web01 = import_named(&env, "web01").unwrap();
    let other = import_on(&env, "other", env.node, ImportPhase::Ready).await;
    for (import, phase) in [
        (&web01, ImportPhase::Importing),
        (&other, ImportPhase::Ready),
    ] {
        set_phase(&env, &import.name, phase).await;
        let mut vm: opl_types::vm::Vm = Resource::new(
            format!("{}-vm", import.name),
            serde_json::from_value(vm_request("x")["spec"].clone()).unwrap(),
        );
        vm.id = Uuid::now_v7();
        env.cluster.create(&vm).await.unwrap();
        let op = TaskOp::VmImport {
            import: import.id,
            run: serde_json::from_value(run_request(&import.name)).unwrap(),
        };
        put_task(&env, op, vm.id, TaskState::Failed).await;

        let (status, body) = run(&admin, &import.name, run_request("again")).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (StatusCode::CONFLICT, Some("import_done")),
            "{body}"
        );
        let done = import_named(&env, &import.name).unwrap();
        assert_eq!(done.status.phase, ImportPhase::Done);
        assert_eq!(done.status.vm, Some(vm.id));
        assert_eq!(done.status.error, None);
        assert!(!imports_dir(&env).join(import.id.to_string()).exists());
        assert!(
            env.cluster
                .get::<opl_types::vm::VmSpec>(vm.id)
                .unwrap()
                .is_some()
        );
    }
}

/// 지우기 Task는 원본 디렉터리와 리소스를 지운다. 원본 노드가 오프라인이거나 없으면 409 `node_offline`이고,
/// `force`면 리소스만 지운다(200).
#[tokio::test]
async fn delete_removes_files_and_force_forgets_an_unreachable_import() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let body = uploaded(&env, &admin).await;
    let dir = imports_dir(&env).join(body["id"].as_str().unwrap());
    assert!(dir.exists());

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, task) = admin.delete("/vm-imports/web01").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-import-delete");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(import_named(&env, "web01").is_none());
    assert!(!dir.exists());

    // 클러스터에 없는 노드의 가져오기
    import_on(&env, "far", Uuid::now_v7(), ImportPhase::Ready).await;
    let (status, body) = admin.delete("/vm-imports/far").await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = run(&admin, "far", run_request("far")).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("node_offline")),
        "{body}"
    );
    let (status, body) = admin.delete("/vm-imports/far?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("vm-import \"far\" deleted; its files are left on node"),
        "{body}"
    );
    assert!(import_named(&env, "far").is_none());
}

/// 노드가 시작할 때 실행할 Task가 없는 `importing` 가져오기는 `ready`로 되돌리고 이유를 남긴다. 이전
/// opld가 돌리던 가져오기 Task는 실패로 끝나고, 그 정리가 가져오기를 `ready`로 되돌린다.
#[tokio::test]
async fn startup_returns_interrupted_imports_to_ready() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let lost = import_on(&env, "lost", env.node, ImportPhase::Importing).await;
    let cut = import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    for import in [&lost, &cut] {
        let dir = imports_dir(&env).join(import.id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.ova"), b"ova").unwrap();
    }
    let run = serde_json::from_value(run_request("cut")).unwrap();
    let op = TaskOp::VmImport {
        import: cut.id,
        run,
    };
    let task = put_task(&env, op, Uuid::now_v7(), TaskState::Running).await;

    env.start_runner().await;

    let lost = env.cluster.get::<VmImportSpec>(lost.id).unwrap().unwrap();
    assert_eq!(lost.status.phase, ImportPhase::Ready);
    assert_eq!(
        lost.status.error.as_deref(),
        Some("the import was interrupted")
    );
    let admin = env.admin().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let cut = env.cluster.get::<VmImportSpec>(cut.id).unwrap().unwrap();
    assert_eq!(cut.status.phase, ImportPhase::Ready);
    assert!(
        cut.status
            .error
            .as_deref()
            .unwrap()
            .starts_with("interrupted"),
        "{cut:?}"
    );
}

/// VM을 만든 뒤 가져오기를 `done`으로 바꾸기 전에 opld가 죽었으면, 노드가 시작할 때 대상 VM이 있는
/// 가져오기를 `done`으로 끝내고 원본 디렉터리를 지운다. 끝난 Task(시작 정리)와 끝나지 않은 Task(실행기가
/// 실패로 끝내는 정리) 둘 다 그렇다. VM과 볼륨은 지우지 않는다.
#[tokio::test]
async fn startup_finishes_imports_whose_vm_was_made() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let ended = import_on(&env, "ended", env.node, ImportPhase::Importing).await;
    let cut = import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    let mut tasks = vec![];
    for (import, state) in [(&ended, TaskState::Failed), (&cut, TaskState::Running)] {
        let dir = imports_dir(&env).join(import.id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("source.ova"), b"ova").unwrap();
        let mut vm: opl_types::vm::Vm = Resource::new(
            import.name.as_str(),
            serde_json::from_value(vm_request("x")["spec"].clone()).unwrap(),
        );
        vm.id = Uuid::now_v7();
        env.cluster.create(&vm).await.unwrap();
        let op = TaskOp::VmImport {
            import: import.id,
            run: serde_json::from_value(run_request(&import.name)).unwrap(),
        };
        tasks.push((put_task(&env, op, vm.id, state).await, vm.id));
    }

    env.start_runner().await;

    let ended_done = env.cluster.get::<VmImportSpec>(ended.id).unwrap().unwrap();
    assert_eq!(ended_done.status.phase, ImportPhase::Done);
    assert_eq!(ended_done.status.vm, Some(tasks[0].1));
    assert_eq!(ended_done.status.error, None);
    assert!(!imports_dir(&env).join(ended.id.to_string()).exists());
    let admin = env.admin().await;
    let task = admin.wait_task(&tasks[1].0).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let cut_done = env.cluster.get::<VmImportSpec>(cut.id).unwrap().unwrap();
    assert_eq!(cut_done.status.phase, ImportPhase::Done);
    assert_eq!(cut_done.status.vm, Some(tasks[1].1));
    assert!(!imports_dir(&env).join(cut.id.to_string()).exists());
    for (_, vm) in tasks {
        assert!(
            env.cluster
                .get::<opl_types::vm::VmSpec>(vm)
                .unwrap()
                .is_some()
        );
    }
}

// --- ESXi에서 바로 가져오기 ---

const ESXI_USER: &str = "root";
const ESXI_PASSWORD: &str = "S3cret-esxi-pw";
/// 형식은 맞지만 가짜 ESXi의 것이 아닌 지문
const OTHER_FINGERPRINT: &str = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF";

const WEB01_VMX: &str = r#".encoding = "UTF-8"
displayName = "web01"
guestOS = "ubuntu-64"
numvcpus = "2"
cpuid.coresPerSocket = "2"
memSize = "1024"
firmware = "efi"
scsi0.present = "TRUE"
scsi0.virtualDev = "pvscsi"
scsi0:0.present = "TRUE"
scsi0:0.fileName = "web01.vmdk"
sata0.present = "TRUE"
sata0:0.present = "TRUE"
sata0:0.deviceType = "cdrom-image"
sata0:0.fileName = "/vmfs/volumes/datastore1/iso/ubuntu.iso"
ethernet0.present = "TRUE"
ethernet0.networkName = "VM Network"
ethernet0.virtualDev = "vmxnet3"
ethernet0.addressType = "static"
ethernet0.address = "00:50:56:aa:bb:cc"
"#;

fn descriptor(create_type: &str, extent: &str, extra: &str) -> Vec<u8> {
    format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n{extra}\
         createType=\"{create_type}\"\n\n# Extent description\n{extent}\n\n\
         # The Disk Data Base\nddb.adapterType = \"lsilogic\"\n"
    )
    .into_bytes()
}

/// 가짜 ESXi의 파일: 데이터스토어 둘, 켜진 VM(`web01`, 잠금 파일), 스냅샷이 있는 VM, `.vmx`를 읽을 수
/// 없는 VM, 다른 데이터스토어에 디스크가 있는 VM, VM이 아닌 디렉터리.
fn esxi_files() -> BTreeMap<String, Vec<u8>> {
    let minimal = |name: &str, disk: &str| {
        format!(
            "displayName = \"{name}\"\nguestOS = \"windows2019srv-64\"\nmemSize = \"2048\"\n\
             numvcpus = \"4\"\nscsi0:0.present = \"TRUE\"\nscsi0:0.fileName = \"{disk}\"\n"
        )
        .into_bytes()
    };
    BTreeMap::from([
        ("datastore1/web01/web01.vmx".into(), WEB01_VMX.into()),
        (
            "datastore1/web01/web01.vmdk".into(),
            descriptor("vmfs", "RW 8192 VMFS \"web01-flat.vmdk\"", ""),
        ),
        ("datastore1/web01/web01-flat.vmdk".into(), vec![0; 4096]),
        ("datastore1/web01/web01.vmx.lck".into(), vec![]),
        ("datastore1/web01/vmware.log".into(), b"log".to_vec()),
        (
            "datastore1/snap/snap.vmx".into(),
            minimal("snap", "snap-000001.vmdk"),
        ),
        (
            "datastore1/snap/snap-000001.vmdk".into(),
            descriptor(
                "vmfsSparse",
                "RW 8192 VMFSSPARSE \"snap-000001-delta.vmdk\"",
                "parentFileNameHint=\"snap.vmdk\"\n",
            ),
        ),
        (
            "datastore1/broken/broken.vmx".into(),
            b"numvcpus = \"2\"\n".to_vec(),
        ),
        (
            "datastore1/far/far.vmx".into(),
            minimal("far", "/vmfs/volumes/datastore2/far/far.vmdk"),
        ),
        ("datastore1/iso/ubuntu.iso".into(), b"iso".to_vec()),
        (
            "datastore2/db01/db01.vmx".into(),
            minimal("db01", "db01.vmdk"),
        ),
        (
            "datastore2/db01/db01.vmdk".into(),
            descriptor("monolithicFlat", "RW 2048 FLAT \"db01-flat.vmdk\" 0", ""),
        ),
    ])
}

async fn mock_esxi() -> MockEsxi {
    MockEsxi::start(esxi_files(), ESXI_USER, ESXI_PASSWORD).await
}

/// 요청 본문의 연결(`EsxiConnection`은 직렬화하지 않으므로 JSON으로 만든다)
fn connection(esxi: &MockEsxi, password: &str, fingerprint: Option<&str>) -> Value {
    let mut conn = json!({
        "host": esxi.addr.ip().to_string(),
        "port": esxi.addr.port(),
        "user": ESXI_USER,
        "password": password,
    });
    if let Some(f) = fingerprint {
        conn["fingerprint"] = json!(f);
    }
    conn
}

fn esxi_import(name: &str, conn: Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": conn, "datastore": datastore, "vmx": vmx })
}

/// 지문이 없으면 409 `untrusted_certificate`와 서버 지문, 다르면 409 `fingerprint_mismatch`와 실제 지문.
/// 둘 다 자격 증명을 보내기 전에 끊는다. 비밀번호가 틀리면 400 `esxi_auth`, 연결할 수 없으면 502
/// `esxi_unreachable`.
#[tokio::test]
async fn browse_needs_a_trusted_fingerprint() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, None),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "untrusted_certificate");
    assert_eq!(body["message"], "verify the ESXi certificate fingerprint");
    assert_eq!(body["details"], json!({ "fingerprint": esxi.fingerprint }));

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(OTHER_FINGERPRINT)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "fingerprint_mismatch");
    assert_eq!(body["details"], json!({ "fingerprint": esxi.fingerprint }));
    assert!(esxi.requests().is_empty(), "{:?}", esxi.requests());

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, "wrong", Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "esxi_auth");

    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    conn["port"] = json!(port);
    let (status, body) = admin.post("/vm-imports/esxi/browse", conn).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["code"], "esxi_unreachable");

    // 알 수 없는 필드는 받지 않는다.
    let mut conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    conn["extra"] = json!(1);
    let (status, body) = admin.post("/vm-imports/esxi/browse", conn).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // 모양이 틀린 지문은 연결하기 전에 400
    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some("AB:CD")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid fingerprint \"AB:CD\""),
        "{body}"
    );
}

/// 데이터스토어마다 맨 위 디렉터리의 `.vmx`를 읽어 요약한다. 잠금 파일이 있으면 `locked`, 읽을 수 없는
/// `.vmx`는 그 VM의 `error`, `.vmx`가 없는 디렉터리는 뺀다. 지문은 대소문자·콜론 없이 적어도 된다.
#[tokio::test]
async fn browse_lists_vms_on_every_datastore() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;
    let loose = esxi.fingerprint.replace(':', "").to_lowercase();

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&loose)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let vm = |vmx: &str, name: &str, guest: &str, cpus: u32, mem: u64, locked: bool| {
        json!({
            "vmx": vmx, "name": name, "guest_os": guest, "vcpus": cpus, "memory_mib": mem,
            "disks": 1, "locked": locked, "error": null,
        })
    };
    assert_eq!(
        body,
        json!({ "datastores": [
            { "name": "datastore1", "vms": [
                {
                    "vmx": "broken/broken.vmx", "name": "broken", "guest_os": "", "vcpus": 0,
                    "memory_mib": 0, "disks": 0, "locked": false,
                    "error": "the .vmx has no memSize",
                },
                vm("far/far.vmx", "far", "windows2019srv-64", 4, 2048, false),
                vm("snap/snap.vmx", "snap", "windows2019srv-64", 4, 2048, false),
                vm("web01/web01.vmx", "web01", "ubuntu-64", 2, 1024, true),
            ]},
            { "name": "datastore2", "vms": [
                vm("db01/db01.vmx", "db01", "windows2019srv-64", 4, 2048, false),
            ]},
        ]})
    );

    // 읽기 사용자는 ESXi에 연결할 수 없다.
    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// `.vmx`와 디스크립터를 읽어 원본 VM·제안·경고를 담은 `ready` 가져오기를 만든다. 원본에는 비밀번호가
/// 없고(저장소 어디에도 없다), 자격 증명은 이 노드의 메모리에만 둔다.
#[tokio::test]
async fn creates_an_import_from_esxi() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;

    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", conn, "datastore1", "web01/web01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["node"], env.node.to_string());
    assert_eq!(
        body["spec"]["source"],
        json!({ "esxi": {
            "host": "127.0.0.1", "port": esxi.addr.port(), "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": ESXI_USER,
            "fingerprint": esxi.fingerprint,
        }})
    );
    let st = &body["status"];
    assert_eq!(st["phase"], "ready");
    assert_eq!(
        st["source_vm"],
        json!({
            "name": "web01", "guest_os": "ubuntu-64", "firmware": "uefi", "vcpus": 2,
            "cores_per_socket": 2, "memory_mib": 1024,
            "disks": [{
                "name": "disk0", "file": "web01/web01-flat.vmdk",
                "capacity_bytes": 8192 * 512, "controller": "scsi",
            }],
            "nics": [{ "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" }],
        })
    );
    assert_eq!(
        st["proposal"],
        json!({
            "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
            "disk_bus": "virtio-blk", "nic_model": "virtio",
            "networks": { "VM Network": null },
        })
    );
    let warnings: Vec<&str> = st["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert_eq!(
        warnings[..2],
        [
            "the VM looks powered on (lock files found); power it off before importing",
            "CD-ROM drive is not imported",
        ]
    );

    // 비밀번호는 저장소의 어떤 리소스에도 없다.
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let (_, imports) = env.cluster.list::<VmImportSpec>().unwrap();
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let stored = serde_json::to_string(&(imports, tasks)).unwrap();
    assert!(!stored.contains(ESXI_PASSWORD), "{stored}");
    let (_, read) = admin.get("/vm-imports/web01").await;
    assert!(!read.to_string().contains(ESXI_PASSWORD), "{read}");
    // 자격 증명은 이 노드의 메모리에(정규화한 지문과 함께)
    let stored = env.state.import_credentials.get(id).unwrap();
    assert_eq!(stored.password, ESXI_PASSWORD);
    assert_eq!(
        stored.fingerprint.as_deref(),
        Some(esxi.fingerprint.as_str())
    );
    // ESXi 가져오기에는 원본 디렉터리가 없다(실행할 때 만든다).
    assert!(import_dirs(&env).is_empty());

    // 같은 이름은 409
    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", conn, "datastore2", "db01/db01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");

    // vCenter 모양(monolithicFlat)도 된다. 잠금 파일이 없으면 경고도 없다.
    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("db01", conn, "datastore2", "db01/db01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["status"]["source_vm"]["disks"][0]["file"],
        "db01/db01-flat.vmdk"
    );
    assert_eq!(
        body["status"]["source_vm"]["disks"][0]["capacity_bytes"],
        2048 * 512
    );
    assert!(
        !body["status"]["warnings"]
            .to_string()
            .contains("lock files found")
    );
}

/// 켜진 VM은 잠금 파일로 짐작한다: `.lck`로 끝나는 항목, 스왑 파일(`.vswp`, 켜진 VM만 가진다), NFS 잠금
/// (`.lck-`로 시작). 일시 중단된 VM은 `.vmss`를 가진다. browse는 `locked`, 만들기는 경고. 디스크립터가 따로
/// 없는 VMDK(크기 제한을 넘는 바이너리)는 400 "not a VMDK descriptor".
#[tokio::test]
async fn detects_powered_on_vms_and_disks_without_descriptors() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = |name: &str| {
        let vmx = WEB01_VMX.replace("web01", name).into_bytes();
        let descriptor = descriptor("vmfs", &format!("RW 8 VMFS \"{name}-flat.vmdk\""), "");
        [
            (format!("datastore1/{name}/{name}.vmx"), vmx),
            (format!("datastore1/{name}/{name}.vmdk"), descriptor),
            (format!("datastore1/{name}/{name}-flat.vmdk"), vec![0; 4096]),
        ]
    };
    let mut files = BTreeMap::new();
    for name in ["off", "swap", "nfs", "lockdir", "suspended", "sparse"] {
        files.extend(vm(name));
    }
    files.insert("datastore1/swap/swap-3f1a2b.vswp".into(), vec![]);
    files.insert("datastore1/nfs/.lck-0011223344556677".into(), vec![]);
    files.insert("datastore1/lockdir/lockdir.vmx.lck/lock".into(), vec![]);
    files.insert("datastore1/suspended/suspended-5d3c.vmss".into(), vec![]);
    // 디스크립터 없이 데이터가 들어 있는 VMDK(호스트형 스파스)
    files.insert(
        "datastore1/sparse/sparse.vmdk".into(),
        vec![b'K'; 100 << 10],
    );
    let esxi = MockEsxi::start(files, ESXI_USER, ESXI_PASSWORD).await;

    let (status, body) = admin
        .post(
            "/vm-imports/esxi/browse",
            connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let locked: Vec<(&str, bool)> = body["datastores"][0]["vms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|vm| {
            (
                vm["name"].as_str().unwrap(),
                vm["locked"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        locked,
        [
            ("lockdir", true),
            ("nfs", true),
            ("off", false),
            ("sparse", false),
            ("suspended", true),
            ("swap", true),
        ]
    );

    let powered_on = "the VM looks powered on (lock files found); power it off before importing";
    let suspended = "the VM is suspended; power it off in VMware before importing (its memory state is not imported)";
    for (name, warning) in [
        ("off", None),
        ("swap", Some(powered_on)),
        ("nfs", Some(powered_on)),
        ("lockdir", Some(powered_on)),
        ("suspended", Some(suspended)),
    ] {
        let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
        let (status, body) = admin
            .post(
                "/vm-imports/esxi",
                esxi_import(name, conn, "datastore1", &format!("{name}/{name}.vmx")),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{name}: {body}");
        let warnings = body["status"]["warnings"].as_array().unwrap();
        for w in [powered_on, suspended] {
            assert_eq!(
                warnings.iter().any(|x| x == w),
                warning == Some(w),
                "{name}: {body}"
            );
        }
        if let Some(w) = warning {
            assert_eq!(warnings[0], w, "{name}: {body}");
        }
    }

    let conn = connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import("sparse", conn, "datastore1", "sparse/sparse.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "[datastore1] sparse/sparse.vmdk is not a VMDK descriptor; only VMFS and flat disks can be imported"
    );
}

/// 가져올 수 없는 VM과 잘못된 요청은 가져오기를 만들지 않고, 자격 증명도 남기지 않는다.
#[tokio::test]
async fn esxi_import_rejects_what_it_cannot_import() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let esxi = mock_esxi().await;
    let trusted = || connection(&esxi, ESXI_PASSWORD, Some(&esxi.fingerprint));

    // 지문이 없으면 browse와 같은 409
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import(
                "web01",
                connection(&esxi, ESXI_PASSWORD, None),
                "datastore1",
                "web01/web01.vmx",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "untrusted_certificate");
    assert_eq!(body["details"]["fingerprint"], esxi.fingerprint);

    // 모양이 틀린 지문은 연결하기 전에 400
    let (status, body) = admin
        .post(
            "/vm-imports/esxi",
            esxi_import(
                "web01",
                connection(&esxi, ESXI_PASSWORD, Some("not-a-fingerprint")),
                "datastore1",
                "web01/web01.vmx",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid fingerprint"),
        "{body}"
    );

    for (name, vmx, code, want) in [
        (
            "snap",
            "snap/snap.vmx",
            "invalid_request",
            "the VM has snapshots; consolidate or delete them in VMware first",
        ),
        (
            "far",
            "far/far.vmx",
            "invalid_request",
            "is on another datastore",
        ),
        (
            "broken",
            "broken/broken.vmx",
            "invalid_request",
            "the .vmx has no memSize",
        ),
        (
            "gone",
            "gone/gone.vmx",
            "invalid_request",
            "[datastore1] gone/gone.vmx was not found",
        ),
        ("up", "../x.vmx", "invalid_request", "invalid .vmx path"),
        (
            "notvmx",
            "web01/web01.vmdk",
            "invalid_request",
            "invalid .vmx path",
        ),
        ("esxi", "web01/web01.vmx", "invalid_request", "reserved"),
        ("Bad Name", "web01/web01.vmx", "invalid_request", "name"),
    ] {
        let (status, body) = admin
            .post(
                "/vm-imports/esxi",
                esxi_import(name, trusted(), "datastore1", vmx),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{vmx}: {body}");
        assert_eq!(body["code"], code, "{vmx}: {body}");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains(want), "{vmx}: {message}");
    }
    let (_, imports) = env.cluster.list::<VmImportSpec>().unwrap();
    assert!(imports.is_empty());
    assert!(env.state.import_credentials.is_empty());

    // 읽기 사용자는 만들 수 없다.
    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = viewer
        .post(
            "/vm-imports/esxi",
            esxi_import("web01", trusted(), "datastore1", "web01/web01.vmx"),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// 저장소에 ESXi 가져오기를 직접 만든다.
async fn esxi_import_on(env: &Env, name: &str, node: Uuid, phase: ImportPhase) -> VmImport {
    let mut import: VmImport = Resource::new(
        name,
        VmImportSpec {
            node,
            source: ImportSource::Esxi(EsxiSource {
                host: "esxi.example".into(),
                port: 443,
                datacenter: "ha-datacenter".into(),
                datastore: "datastore1".into(),
                vmx: format!("{name}/{name}.vmx"),
                user: "root".into(),
                fingerprint: OTHER_FINGERPRINT.into(),
            }),
        },
    );
    import.status = VmImportStatus {
        phase,
        ..Default::default()
    };
    env.cluster.create(&import).await.unwrap();
    import
}

/// 노드가 시작할 때 이 노드의 `ready`·(Task 없는) `importing` ESXi 가져오기는 자격 증명을 잃었으므로
/// `failed`가 된다. 원본 디렉터리가 없어도 "source files are gone"이 아니다. 다른 노드의 것과 끝난 것은
/// 그대로 둔다.
#[tokio::test]
async fn startup_fails_esxi_imports_whose_credentials_are_lost() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let ready = esxi_import_on(&env, "ready", env.node, ImportPhase::Ready).await;
    let cut = esxi_import_on(&env, "cut", env.node, ImportPhase::Importing).await;
    let done = esxi_import_on(&env, "done", env.node, ImportPhase::Done).await;
    let other = esxi_import_on(&env, "other", Uuid::now_v7(), ImportPhase::Ready).await;

    env.start_runner().await;

    let phase = |id: Uuid| {
        let import = env.cluster.get::<VmImportSpec>(id).unwrap().unwrap();
        (import.status.phase, import.status.error)
    };
    let lost = Some("the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again".into());
    assert_eq!(phase(ready.id), (ImportPhase::Failed, lost.clone()));
    assert_eq!(phase(cut.id), (ImportPhase::Failed, lost));
    assert_eq!(phase(done.id), (ImportPhase::Done, None));
    assert_eq!(phase(other.id), (ImportPhase::Ready, None));
}
