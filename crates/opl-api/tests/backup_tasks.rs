//! 백업 Task와 백업 지우기 Task: Mock 하이퍼바이저와 dir 풀로 꺼진 VM과 실행 중인 VM을 백업하고(압축
//! qcow2, UEFI 변수, 매니페스트), 실패하면 디렉터리를 지우고 `failed`로 남기며, 지우기는 파일과
//! 리소스를 지운다. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Client, Env, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE, MANIFEST_VERSION,
};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn qemu_img(args: &[&str]) -> String {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn info(path: &Path) -> Value {
    serde_json::from_str(&qemu_img(&[
        "info",
        "-U",
        "--output=json",
        path.to_str().unwrap(),
    ]))
    .unwrap()
}

fn pool_dir(env: &Env, pool: &str) -> PathBuf {
    env.dir.path().join(pool)
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다.
async fn create_pool(env: &Env, name: &str, backups: bool) {
    std::fs::create_dir_all(pool_dir(env, name)).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env, name),
            },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 풀 `local`에 볼륨을 API로 만들고 파일 경로를 돌려준다.
async fn create_volume(env: &Env, admin: &Client, name: &str, size: u64, format: &str) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": name,
                "spec": { "pool": "local", "size_bytes": size, "format": format }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap();
    pool_dir(env, "local").join(format!("{}.{format}", volume.id))
}

async fn create_vm(admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

async fn wait(admin: &Client, (status, task): (StatusCode, Value)) -> Value {
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn backup(env: &Env, name: &str) -> Option<Backup> {
    env.cluster.get_by_name::<BackupSpec>(name).unwrap()
}

/// 백업 풀 `bk` 안의 백업 디렉터리
fn backup_dir(env: &Env, id: Uuid) -> PathBuf {
    opl_types::backup::backup_dir(&pool_dir(env, "bk"), id)
}

/// 두 디스크(qcow2, raw)에 표식을 쓴 VM `web`과 UEFI 변수 파일을 만든다.
async fn setup(env: &Env, admin: &Client) -> (Uuid, PathBuf, PathBuf) {
    create_pool(env, "local", false).await;
    create_pool(env, "bk", true).await;
    let root = create_volume(env, admin, "root", 4 * MIB, "qcow2").await;
    let data = create_volume(env, admin, "data", 2 * MIB, "raw").await;
    assert!(qemu_io(&root, "qcow2", "write -P 0xaa 0 64k"));
    assert!(qemu_io(&data, "raw", "write -P 0x5a 64k 64k"));
    let vm = create_vm(admin, "web", &["root", "data"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"uefi variables").unwrap();
    (vm, root, data)
}

#[tokio::test]
async fn stopped_vm_backup_writes_compressed_disks_vars_and_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (vm, _, _) = setup(&env, &admin).await;

    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let b = backup(&env, "b1").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    assert!(b.status.created_at.is_some());
    assert!(b.status.uefi_vars);
    assert_eq!(b.status.error, None);
    let names: Vec<(&str, &str, u64)> = b
        .status
        .disks
        .iter()
        .map(|d| (d.name.as_str(), d.file.as_str(), d.virtual_bytes))
        .collect();
    assert_eq!(
        names,
        [
            ("disk0", "disk0.qcow2", 4 * MIB),
            ("disk1", "disk1.qcow2", 2 * MIB)
        ]
    );

    let dir = backup_dir(&env, b.id);
    for disk in &b.status.disks {
        let path = dir.join(&disk.file);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), disk.stored_bytes);
        let info = info(&path);
        assert_eq!(info["format"], "qcow2");
        assert_eq!(info["virtual-size"], disk.virtual_bytes);
    }
    assert!(qemu_io(
        &dir.join("disk0.qcow2"),
        "qcow2",
        "read -P 0xaa 0 64k"
    ));
    assert!(qemu_io(
        &dir.join("disk1.qcow2"),
        "qcow2",
        "read -P 0x5a 64k 64k"
    ));
    assert_eq!(
        std::fs::read(dir.join("vars.fd")).unwrap(),
        b"uefi variables"
    );
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(manifest.version, MANIFEST_VERSION);
    assert_eq!(manifest.backup, b.id);
    assert_eq!((manifest.vm, manifest.vm_name.as_str()), (vm, "web"));
    assert_eq!(manifest.vm_spec, b.spec.vm_spec);
    assert_eq!(manifest.disks, b.status.disks);
    assert!(manifest.uefi_vars);
    assert_eq!(Some(manifest.created_at), b.status.created_at);
    // 공유가 아닌 풀이므로 파일을 쓴 이 노드를 적는다.
    assert_eq!(b.status.node, Some(env.node));
    // 다 쓴 파일만 남는다.
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["disk0.qcow2", "disk1.qcow2", "manifest.json", "vars.fd"]
    );

    // 지우기는 디렉터리와 리소스를 지운다.
    succeeded(&wait(&admin, admin.delete("/backups/b1").await).await);
    assert!(backup(&env, "b1").is_none());
    assert!(!dir.exists());
}

/// 실행 중인 VM은 디스크에 임시 스냅샷(백업 id의 simple 문자열, 메모리 없음)을 찍어 그때의 내용을
/// 담고, 끝나면 그 스냅샷을 지운다. Mock 하이퍼바이저는 파일에 쓰지 않으므로 QEMU가 만들었을 태그를
/// 대신 만들고, 그 뒤 게스트가 쓴 것처럼 디스크를 바꾼다.
#[tokio::test]
async fn running_vm_backup_reads_the_temporary_snapshot() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let root = create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    assert!(qemu_io(&root, "qcow2", "write -P 0xaa 0 64k"));
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"at snapshot").unwrap();

    env.stop_runner().await;
    let (status, task) = admin
        .post("/vms/web/backups", json!({ "pool": "bk", "name": "live" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = backup(&env, "live").unwrap().id;
    let tag = id.simple().to_string();
    qemu_img(&["snapshot", "-c", &tag, root.to_str().unwrap()]);
    assert!(qemu_io(&root, "qcow2", "write -P 0xbb 0 64k"));
    env.start_runner().await;
    succeeded(&admin.wait_task(&task).await);

    let b = backup(&env, "live").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    let dir = backup_dir(&env, id);
    let disk = dir.join("disk0.qcow2");
    assert!(qemu_io(&disk, "qcow2", "read -P 0xaa 0 64k"));
    assert!(info(&disk).get("snapshots").is_none());
    assert_eq!(std::fs::read(dir.join("vars.fd")).unwrap(), b"at snapshot");
    // 임시 스냅샷을 하이퍼바이저로 찍고 지웠다.
    assert!(
        env.hv.snapshots(vm).is_empty(),
        "{:?}",
        env.hv.snapshots(vm)
    );
    // VM은 계속 돈다.
    let vm = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(vm.status.phase, VmPhase::Running);
}

/// 디스크를 읽지 못하면(여기서는 실행 중인 VM의 임시 스냅샷이 디스크에 없다) 백업 디렉터리를 지우고
/// 오류와 함께 `failed`로 남긴다. 실패한 백업도 지울 수 있다.
#[tokio::test]
async fn failed_backup_removes_its_directory_and_records_the_error() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "bad" }))
            .await,
    )
    .await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot back up volume \"root\""), "{error}");
    let b = backup(&env, "bad").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
    assert_eq!(b.status.error.as_deref(), Some(error));
    assert!(!backup_dir(&env, b.id).exists());
    // 디렉터리를 만들기 전에 이 노드를 적었다. 지우기도 이 노드에서 한다.
    assert_eq!(b.status.node, Some(env.node));
    // 임시 스냅샷은 실패해도 지운다.
    assert!(env.hv.snapshots(vm).is_empty());

    succeeded(&wait(&admin, admin.delete("/backups/bad").await).await);
    assert!(backup(&env, "bad").is_none());
}

/// 온라인인 다른 노드를 저장소에 만든다. 그 노드의 실행기는 없다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: opl_types::node::Node = Resource::new(
        name,
        opl_types::node::NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// Task를 저장소에 직접 만든다(API의 검사 없이). `state`가 `Running`이면 이미 돌고 있는 것처럼 둔다.
async fn put_task(
    env: &Env,
    op: opl_types::task::TaskOp,
    target: Uuid,
    node: Uuid,
    state: TaskState,
) -> Value {
    let mut task: opl_types::task::Task = Resource::new("", TaskSpec { op, target, node });
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

/// 지우기 Task는 같은 백업을 되살리는 Task가 (다른 노드에서) 끝나지 않았으면 파일을 지우지 않고
/// 실패한다.
#[tokio::test]
async fn delete_waits_for_restores_of_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let b = backup(&env, "b1").unwrap();
    let n2 = other_node(&env, "n2").await;
    let restore = opl_types::task::TaskOp::BackupRestore {
        backup: b.id,
        name: "web-2".into(),
        pool: "local".into(),
    };
    put_task(&env, restore, Uuid::now_v7(), n2, TaskState::Running).await;

    let delete = opl_types::task::TaskOp::BackupDelete;
    let task = put_task(&env, delete, b.id, env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "backup is being restored");
    assert!(backup(&env, "b1").is_some());
    assert!(backup_dir(&env, b.id).join(MANIFEST_FILE).exists());
}

/// 공유가 아닌 풀에서 다른 노드가 쓴 백업의 파일은 이 노드에 없는 것이 당연하므로, 지우기가 이 노드에서
/// 돌면 이미 지운 것으로 보지 않고 실패한다. 풀이 없어진 백업은 리소스만 지운다.
#[tokio::test]
async fn delete_does_not_forget_files_on_another_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    create_vm(&admin, "web", &["root"]).await;
    for name in ["b1", "b2"] {
        let task = wait(
            &admin,
            admin
                .post("/vms/web/backups", json!({ "pool": "bk", "name": name }))
                .await,
        )
        .await;
        succeeded(&task);
    }
    let b1 = backup(&env, "b1").unwrap();
    let n2 = other_node(&env, "n2").await;
    let status = opl_types::backup::BackupStatus {
        node: Some(n2),
        ..b1.status.clone()
    };
    env.cluster
        .update_status::<BackupSpec>(b1.id, &status)
        .await
        .unwrap();
    std::fs::remove_dir_all(backup_dir(&env, b1.id)).unwrap();

    let delete = opl_types::task::TaskOp::BackupDelete;
    let task = put_task(&env, delete, b1.id, env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains(&format!("on node {n2}")), "{error}");
    assert!(backup(&env, "b1").is_some());

    // 풀이 없어지면 파일도 없다. 리소스만 지운다.
    env.cluster.delete::<BackupSpec>(b1.id, None).await.unwrap();
    let b2 = backup(&env, "b2").unwrap();
    let pool = env
        .cluster
        .get_by_name::<StoragePoolSpec>("bk")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<StoragePoolSpec>(pool.id, None)
        .await
        .unwrap();
    succeeded(&wait(&admin, admin.delete("/backups/b2").await).await);
    assert!(backup(&env, "b2").is_none());
    assert!(backup_dir(&env, b2.id).exists());
}

/// opld가 백업 도중 내려갔으면 다음 시작에 그 Task를 실패로 끝내고, 백업은 반쯤 쓴 디렉터리를 지우고
/// `failed`로 남긴다.
#[tokio::test]
async fn backup_interrupted_by_a_restart_is_failed_and_cleaned_up() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    create_vm(&admin, "web", &["root"]).await;

    env.stop_runner().await;
    let (status, task) = admin
        .post("/vms/web/backups", json!({ "pool": "bk", "name": "cut" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task_id, &running)
        .await
        .unwrap();
    let id = backup(&env, "cut").unwrap().id;
    let dir = backup_dir(&env, id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("disk0.qcow2.part"), b"half").unwrap();
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let b = backup(&env, "cut").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
    assert!(
        b.status
            .error
            .as_deref()
            .unwrap()
            .starts_with("interrupted"),
        "{b:?}"
    );
    assert!(!dir.exists());
}
