//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. 볼륨 크기 늘리기와 그 차례. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{PowerState, VmPhase, VmSpec};
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
fn qemu_io(path: &Path, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 파일에 있는 스냅샷 태그
fn tags(path: &Path) -> Vec<String> {
    let info: Value =
        serde_json::from_str(&qemu_img(&["info", "-U", "--output=json", path_str(path)])).unwrap();
    info["snapshots"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 파일의 가상 크기
fn virtual_size(path: &Path) -> u64 {
    let info: Value =
        serde_json::from_str(&qemu_img(&["info", "-U", "--output=json", path_str(path)])).unwrap();
    info["virtual-size"].as_u64().unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn pool_dir(env: &Env) -> PathBuf {
    env.dir.path().join("pool")
}

/// 풀 `local`을 저장소에 직접 만든다.
async fn create_pool(env: &Env) {
    std::fs::create_dir_all(pool_dir(env)).unwrap();
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 4 MiB qcow2 볼륨을 API로 만들고 파일 경로를 돌려준다.
async fn create_volume(env: &Env, admin: &Client, name: &str) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": name,
                "spec": { "pool": "local", "size_bytes": 4 * MIB, "format": "qcow2" }
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
    pool_dir(env).join(format!("{}.qcow2", volume.id))
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

fn get(env: &Env, vm: Uuid, name: &str) -> Option<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
}

fn vm(env: &Env, id: Uuid) -> opl_types::vm::Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

#[tokio::test]
async fn stopped_vm_snapshot_reverts_disk_size_and_uefi_vars() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-1").unwrap();
    assert!(qemu_io(&file, "write -P 0xaa 0 64k"));

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "s1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let s1 = get(&env, id, "s1").unwrap();
    assert!(!s1.spec.memory);
    assert_eq!(tags(&file), vec![s1.id.to_string()]);
    assert_eq!(vm(&env, id).status.snapshot, Some(s1.id));

    // 스냅샷 뒤에 내용, 크기, 변수, 설정이 바뀐다.
    assert!(qemu_io(&file, "write -P 0xbb 0 64k"));
    qemu_img(&["resize", "-q", path_str(&file), "8M"]);
    std::fs::write(&vars, b"vars-2").unwrap();
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(1024);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await,
    )
    .await;
    succeeded(&task);
    assert!(qemu_io(&file, "read -P 0xaa 0 64k"));
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    assert_eq!(volume.spec.size_bytes, 4 * MIB);
    assert_eq!(std::fs::read(&vars).unwrap(), b"vars-1");
    let got = vm(&env, id);
    assert_eq!(got.spec.memory_mib, 256);
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.snapshot, Some(s1.id));

    let task = wait(&admin, admin.delete("/vms/web/snapshots/s1").await).await;
    succeeded(&task);
    assert!(tags(&file).is_empty());
    assert!(get(&env, id, "s1").is_none());
    assert_eq!(vm(&env, id).status.snapshot, None);
    let copy = vars
        .parent()
        .unwrap()
        .join(format!("snapshots/{}.vars", s1.id));
    assert!(!copy.exists());
}

#[tokio::test]
async fn memory_snapshot_revert_starts_from_the_saved_state() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "mem" }))
            .await,
    )
    .await;
    succeeded(&task);
    let mem = get(&env, id, "mem").unwrap();
    assert!(mem.spec.memory);
    // Mock 하이퍼바이저는 파일에 쓰지 않으므로 QEMU가 만들었을 태그를 대신 만든다.
    qemu_img(&["snapshot", "-c", &mem.id.to_string(), path_str(&file)]);

    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["cpu"]["cores"] = json!(2);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;
    let pid = env.pid(id).await.unwrap();

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots/mem/revert", Value::Null)
            .await,
    )
    .await;
    succeeded(&task);
    // 강제로 끈 뒤 스냅샷의 스펙으로, 그 메모리를 불러오며 다시 시작했다.
    let got = vm(&env, id);
    assert_eq!(got.spec.cpu.cores, 1);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.snapshot, Some(mem.id));
    assert_ne!(env.pid(id).await, Some(pid));
    assert_eq!(
        env.hv.launches().last().unwrap(),
        &(id, Some(mem.id.to_string()))
    );
}

#[tokio::test]
async fn failed_disk_snapshot_removes_tags_already_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let root = create_volume(&env, &admin, "root").await;
    let data = create_volume(&env, &admin, "data").await;
    let id = create_vm(&admin, "web", &["root", "data"]).await;
    // 두 번째 디스크 파일이 없어 그 태그를 만들지 못한다.
    std::fs::remove_file(&data).unwrap();

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "s1" }))
            .await,
    )
    .await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(tags(&root).is_empty());
    assert!(get(&env, id, "s1").is_none());
    assert_eq!(vm(&env, id).status.snapshot, None);
}

#[tokio::test]
async fn unfinished_snapshot_tags_are_removed_at_startup() {
    let dir = tempfile::TempDir::new().unwrap();
    let pool_path = dir.path().to_path_buf();
    let file = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
    let slot = file.clone();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            let pool = Resource::new(
                "local",
                StoragePoolSpec {
                    backend: PoolBackend::Dir {
                        path: pool_path.clone(),
                    },
                    nodes: vec![],
                    shared: false,
                },
            );
            cluster.create(&pool).await.unwrap();
            let mut volume: Volume = Resource::new(
                "root",
                VolumeSpec {
                    pool: "local".into(),
                    size_bytes: MIB,
                    format: ImageFormat::Qcow2,
                },
            );
            volume.status.phase = VolumePhase::Ready;
            cluster.create(&volume).await.unwrap();
            let path = pool_path.join(format!("{}.qcow2", volume.id));
            qemu_img(&["create", "-q", "-f", "qcow2", path_str(&path), "1M"]);

            let mut spec: VmSpec =
                serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
            spec.disks =
                serde_json::from_value(json!([{ "volume": "root", "bus": "virtio-blk" }])).unwrap();
            spec.node = Some(me);
            let vm = Resource::new("web", spec.clone());
            cluster.create(&vm).await.unwrap();
            // 정지한 VM의 태그를 만든 뒤, 끝내기 전에 opld가 죽었다(Task는 실패로 끝났다).
            let mut s: Snapshot = Resource::new(
                "",
                SnapshotSpec {
                    vm: vm.id,
                    name: "half".into(),
                    parent: None,
                    memory: false,
                    description: String::new(),
                    scheduled: false,
                    vm_spec: spec,
                },
            );
            s.name = s.id.to_string();
            cluster.create(&s).await.unwrap();
            qemu_img(&["snapshot", "-c", &s.id.to_string(), path_str(&path)]);
            *slot.lock().unwrap() = path;
        },
    )
    .await;
    let file = file.lock().unwrap().clone();
    assert!(tags(&file).is_empty());
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    assert!(items.is_empty());
}

fn volume_size(env: &Env, name: &str) -> u64 {
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap();
    volume.spec.size_bytes
}

async fn resize(admin: &Client, size: u64) -> (StatusCode, Value) {
    admin
        .post("/volumes/root/resize", json!({ "size_bytes": size }))
        .await
}

#[tokio::test]
async fn stopped_volume_grows_and_task_rechecks_the_size() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;

    succeeded(&wait(&admin, resize(&admin, 8 * MIB).await).await);
    assert_eq!(virtual_size(&file), 8 * MIB);
    assert_eq!(volume_size(&env, "root"), 8 * MIB);
    assert_eq!(env.hv.disk_size(id, 0), None);

    // 둘 다 API 검사를 지나지만, 앞의 것이 늘린 뒤에는 뒤의 것이 더 크지 않다.
    env.stop_runner().await;
    let first = resize(&admin, 12 * MIB).await;
    let second = resize(&admin, 12 * MIB).await;
    env.start_runner().await;
    succeeded(&wait(&admin, first).await);
    let task = wait(&admin, second).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"].as_str().unwrap().contains("larger"),
        "{task}"
    );
    assert_eq!(virtual_size(&file), 12 * MIB);
    assert_eq!(volume_size(&env, "root"), 12 * MIB);
}

#[tokio::test]
async fn running_vm_disk_grows_in_qemu_and_task_rechecks_the_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    // Mock 하이퍼바이저는 파일을 늘리지 않으므로 QEMU가 늘렸을 크기를 대신 만든다.
    qemu_img(&["resize", "-q", path_str(&file), "8M"]);
    succeeded(&wait(&admin, resize(&admin, 8 * MIB).await).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
    assert_eq!(volume_size(&env, "root"), 8 * MIB);

    // API는 실행 중일 때 받았지만 실행할 때는 꺼졌다. 파일을 늘린다.
    env.stop_runner().await;
    let task = resize(&admin, 12 * MIB).await;
    let current = vm(&env, id);
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    env.cluster
        .update_spec(id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while vm(&env, id).status.phase != VmPhase::Stopped {
        assert!(Instant::now() < deadline, "VM did not stop");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    env.start_runner().await;
    succeeded(&wait(&admin, task).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
    assert_eq!(virtual_size(&file), 12 * MIB);
    assert_eq!(volume_size(&env, "root"), 12 * MIB);
}

#[tokio::test]
async fn resize_waits_for_a_revert_of_the_same_disk() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "mem" }))
            .await,
    )
    .await;
    succeeded(&task);
    let mem = get(&env, id, "mem").unwrap();
    qemu_img(&["snapshot", "-c", &mem.id.to_string(), path_str(&file)]);

    // 되돌리기(VM의 줄)를 먼저, 크기 늘리기(볼륨 Task)를 뒤에 맡긴다. 되돌리기는 시작이 늦어
    // 오래 걸린다. 크기 늘리기는 그동안 기다렸다가 다시 켜진 VM에서 한다.
    env.stop_runner().await;
    let (status, revert) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let (status, grow) = resize(&admin, 8 * MIB).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{grow}");
    env.hv.delay_starts(Duration::from_millis(500));
    env.start_runner().await;
    let state = |task: &Value| {
        let path = format!("/tasks/{}", task["id"].as_str().unwrap());
        let admin = admin.clone();
        async move { admin.get(&path).await.1["status"]["state"].clone() }
    };
    let deadline = Instant::now() + WAIT;
    loop {
        // 크기 늘리기를 먼저 읽는다. 되돌리기를 먼저 읽으면 두 읽기 사이에 되돌리기가 끝나고
        // 크기 늘리기가 시작될 수 있다.
        let grown = state(&grow).await;
        let reverted = state(&revert).await;
        if reverted == "succeeded" {
            break;
        }
        assert_ne!(reverted, "failed");
        assert_eq!(grown, "pending", "resize ran during the revert");
        assert!(Instant::now() < deadline, "revert did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    succeeded(&admin.wait_task(&grow).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
}
