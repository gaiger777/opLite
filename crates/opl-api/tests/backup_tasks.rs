//! 백업 Task와 백업 지우기 Task: Mock 하이퍼바이저와 dir 풀로 꺼진 VM과 실행 중인 VM을 백업하고(압축
//! qcow2, UEFI 변수, 매니페스트), 실패하면 디렉터리를 지우고 `failed`로 남기며, 지우기는 파일과
//! 리소스를 지운다. 복원은 백업에서 꺼진 새 VM과 볼륨을 만들고, 실패하면 만든 볼륨을 지운다.
//! qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, MOUNT_ROOT, vm_request};
use opl_driver::{DirStorage, uefi_vars_path};
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE, MANIFEST_VERSION,
};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
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

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

/// 꺼진 VM `web`(`setup`)을 백업 `b1`로 백업한다.
async fn backed_up(env: &Env, admin: &Client) -> Backup {
    let task = wait(
        admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    backup(env, "b1").unwrap()
}

async fn restore(admin: &Client, name: &str, pool: &str) -> Value {
    wait(
        admin,
        admin
            .post("/backups/b1/restore", json!({ "name": name, "pool": pool }))
            .await,
    )
    .await
}

/// 복원은 백업의 VM 스펙으로 꺼진 새 VM을 만든다: 디스크마다 대상 풀에 `{새 이름}-disk{i}` 볼륨(dir
/// 풀이면 qcow2)을 만들어 백업 내용을 쓰고, UEFI 변수를 복사하고, MAC은 새로 받는다. 새 VM의 id는
/// Task의 대상이다. 복원은 디스크 크기만큼 걸리므로 Task 시간 제한을 받지 않는다.
#[tokio::test]
async fn restore_makes_a_stopped_vm_from_the_backup() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (vm, root, _) = setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    // 백업 뒤에 바뀐 원본 디스크는 복원에 들어가지 않는다.
    assert!(qemu_io(&root, "qcow2", "write -P 0xcc 0 64k"));
    create_pool(&env, "other", false).await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.timeout = Duration::ZERO
    })
    .await;

    let task = restore(&admin, "web-2", "other").await;
    succeeded(&task);
    let new = get_vm(&env, "web-2").unwrap();
    assert_eq!(task["spec"]["target"], new.id.to_string());
    let old = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(new.spec.power, PowerState::Stopped);
    assert_eq!(new.spec.node, None);
    assert!(!new.spec.ha);
    assert_eq!(new.spec.memory_mib, old.spec.memory_mib);
    let disks: Vec<&str> = new.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-2-disk0", "web-2-disk1"]);
    let (old_mac, new_mac) = (old.spec.nics[0].mac, new.spec.nics[0].mac);
    assert!(new_mac.is_some());
    assert_ne!(new_mac, old_mac);

    for (name, size, pattern) in [
        ("web-2-disk0", 4 * MIB, "read -P 0xaa 0 64k"),
        ("web-2-disk1", 2 * MIB, "read -P 0x5a 64k 64k"),
    ] {
        let volume = get_volume(&env, name).unwrap();
        assert_eq!(volume.spec.pool, "other");
        assert_eq!(volume.spec.format, ImageFormat::Qcow2);
        assert_eq!(volume.spec.size_bytes, size);
        assert_eq!(volume.status.phase, VolumePhase::Ready);
        assert_eq!(volume.status.node, Some(env.node));
        let path = pool_dir(&env, "other").join(format!("{}.qcow2", volume.id));
        assert!(qemu_io(&path, "qcow2", pattern), "{name}");
    }
    let vars = uefi_vars_path(&env.dir.path().join("state"), new.id);
    assert_eq!(std::fs::read(vars).unwrap(), b"uefi variables");
    assert_eq!(new.status.firmware_node, Some(env.node));
    // 백업은 그대로 남는다.
    assert_eq!(backup(&env, "b1").unwrap().status.phase, BackupPhase::Ready);
    assert!(backup_dir(&env, b.id).join(MANIFEST_FILE).exists());
}

/// 블록 풀(여기서는 LVM-thin)로 복원하면 볼륨은 raw다.
#[tokio::test]
async fn restore_into_a_block_pool_writes_raw_volumes() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backed_up(&env, &admin).await;
    let thin = Resource::new(
        "thin",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&thin).await.unwrap();
    env.pool_ready("thin").await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_with(storage.clone()).await;

    succeeded(&restore(&admin, "web-2", "thin").await);
    let volumes: Vec<Volume> = ["web-2-disk0", "web-2-disk1"]
        .iter()
        .map(|n| get_volume(&env, n).unwrap())
        .collect();
    for v in &volumes {
        assert_eq!(
            (v.spec.pool.as_str(), v.spec.format),
            ("thin", ImageFormat::Raw)
        );
    }
    assert_eq!(
        storage.import_formats(),
        [
            (volumes[0].id, ImageFormat::Raw),
            (volumes[1].id, ImageFormat::Raw)
        ]
    );
    assert!(get_vm(&env, "web-2").is_some());
}

/// 다른 클러스터에서 받은 사본은 보통 백업처럼 복원한다: 원본 백업의 매니페스트와 파일을 받기 API로
/// 보내 받은 사본(매니페스트의 백업 id는 사본의 id)에서 같은 내용의 VM을 만든다.
#[tokio::test]
async fn a_received_copy_restores_like_its_original() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "dr", true).await;
    create_pool(&env, "other", false).await;
    let source = backup_dir(&env, b.id);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(source.join(MANIFEST_FILE)).unwrap()).unwrap();
    let body = json!({
        "pool": "dr",
        "manifest": manifest,
        "origin": { "cluster": "main", "ca_fingerprint": "sha256:00", "backup": b.id }
    });
    let (status, got) = admin.post("/backups/receive", body).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    let copy: Uuid = got["id"].as_str().unwrap().parse().unwrap();
    for file in ["disk0.qcow2", "disk1.qcow2", "vars.fd"] {
        let r = admin
            .http
            .put(format!("{}/api/v1/backups/{copy}/files/{file}", admin.base))
            .bearer_auth(admin.token.as_ref().unwrap())
            .body(std::fs::read(source.join(file)).unwrap());
        let (status, got) = admin.send(r).await;
        assert_eq!(status, StatusCode::OK, "{file}: {got}");
    }
    let (status, got) = admin
        .post(&format!("/backups/{copy}/actions/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");

    let task = wait(
        &admin,
        admin
            .post(
                &format!("/backups/{copy}/restore"),
                json!({ "name": "web-dr", "pool": "other" }),
            )
            .await,
    )
    .await;
    succeeded(&task);
    let new = get_vm(&env, "web-dr").unwrap();
    assert_eq!(new.spec.memory_mib, b.spec.vm_spec.memory_mib);
    for (name, pattern) in [
        ("web-dr-disk0", "read -P 0xaa 0 64k"),
        ("web-dr-disk1", "read -P 0x5a 64k 64k"),
    ] {
        let volume = get_volume(&env, name).unwrap();
        let path = pool_dir(&env, "other").join(format!("{}.qcow2", volume.id));
        assert!(qemu_io(&path, "qcow2", pattern), "{name}");
    }
    let vars = uefi_vars_path(&env.dir.path().join("state"), new.id);
    assert_eq!(std::fs::read(vars).unwrap(), b"uefi variables");
}

/// 디스크 하나를 복원하지 못하면 그때까지 만든 볼륨(파일과 리소스)을 지우고 VM을 만들지 않는다.
#[tokio::test]
async fn failed_restore_removes_the_volumes_it_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    std::fs::write(backup_dir(&env, b.id).join("disk1.qcow2"), b"not an image").unwrap();

    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot restore disk \"disk1\""), "{error}");
    assert!(get_vm(&env, "web-2").is_none());
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_volume(&env, "web-2-disk1").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env, "other"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // 같은 이름으로 다시 복원할 수 있다(여기서는 같은 이유로 다시 실패한다).
    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
}

/// 다른 형식 버전의 매니페스트는 읽지 않는다. 볼륨을 만들기 전에 실패한다.
#[tokio::test]
async fn restore_refuses_another_manifest_version() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let path = backup_dir(&env, b.id).join(MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["version"] = json!(MANIFEST_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("manifest version 2"), "{error}");
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_vm(&env, "web-2").is_none());
}

/// 복원은 백업 리소스에 적힌 VM 스펙으로 VM을 만든다. 매니페스트가 그 기록과 다르면(VM 스펙이나 디스크
/// 수) 볼륨을 만들기 전에 실패한다. 기록된 스펙이 VM 스펙 검사를 통과하지 못하면 만든 볼륨을 지운다.
#[tokio::test]
async fn restore_checks_the_manifest_against_the_backup_record() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let path = backup_dir(&env, b.id).join(MANIFEST_FILE);
    let original = std::fs::read(&path).unwrap();
    let manifest: Value = serde_json::from_slice(&original).unwrap();

    let mut memory = manifest.clone();
    memory["vm_spec"]["memory_mib"] = json!(4096);
    let mut disks = manifest.clone();
    disks["disks"].as_array_mut().unwrap().pop();
    for changed in [memory, disks] {
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let task = restore(&admin, "web-2", "other").await;
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(
            error.contains("manifest does not match the backup record"),
            "{error}"
        );
        assert!(get_volume(&env, "web-2-disk0").is_none());
        assert!(get_vm(&env, "web-2").is_none());
    }

    // 기록과 매니페스트가 함께 틀린 스펙이면 VM을 만들지 않고 볼륨을 지운다.
    let mut spec = b.spec.clone();
    spec.vm_spec.memory_mib = 1;
    env.cluster
        .update_spec(b.id, Some(b.revision), b.labels.clone(), &spec)
        .await
        .unwrap();
    let mut bad = manifest.clone();
    bad["vm_spec"]["memory_mib"] = json!(1);
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("invalid VM spec in backup"), "{error}");
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_vm(&env, "web-2").is_none());
}

/// 백업 디렉터리의 디스크·UEFI 변수 파일은 일반 파일이어야 한다. 심볼릭 링크는 따라가지 않는다.
#[tokio::test]
async fn restore_refuses_symlinks_in_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let dir = backup_dir(&env, b.id);
    let outside = env.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    for file in ["disk1.qcow2", "vars.fd"] {
        let moved = outside.join(file);
        std::fs::rename(dir.join(file), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, dir.join(file)).unwrap();
        let task = restore(&admin, "web-2", "other").await;
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(
            error.contains(&format!("{file} is not a regular file")),
            "{error}"
        );
        assert!(get_volume(&env, "web-2-disk0").is_none());
        assert!(get_vm(&env, "web-2").is_none());
        std::fs::remove_file(dir.join(file)).unwrap();
        std::fs::rename(&moved, dir.join(file)).unwrap();
    }
    succeeded(&restore(&admin, "web-2", "other").await);
}

/// 공유가 아닌 풀에서 다른 노드가 쓴 백업은 이 노드에서 되살리지 않는다(API는 그 노드에 Task를 둔다).
#[tokio::test]
async fn restore_runs_only_on_the_node_holding_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let n2 = other_node(&env, "n2").await;
    let status = opl_types::backup::BackupStatus {
        node: Some(n2),
        ..b.status.clone()
    };
    env.cluster
        .update_status::<BackupSpec>(b.id, &status)
        .await
        .unwrap();

    let op = opl_types::task::TaskOp::BackupRestore {
        backup: b.id,
        name: "web-2".into(),
        pool: "other".into(),
    };
    let task = put_task(&env, op, Uuid::now_v7(), env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        format!("backup is stored on node {n2}")
    );
    assert!(get_volume(&env, "web-2-disk0").is_none());
}
