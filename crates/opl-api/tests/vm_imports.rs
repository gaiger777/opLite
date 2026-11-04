//! VM 가져오기 API: OVA 올리기 사전 확인, 올리기(해석과 제안), 목록·읽기, 노드 시작 때의 정리.

mod common;

use std::path::PathBuf;

use common::ustar::Tar;
use common::{Client, Env};
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::vm_import::{
    ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus,
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
