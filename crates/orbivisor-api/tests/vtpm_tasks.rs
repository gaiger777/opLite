//! 계획 59 Task의 펌웨어 상태: 스냅샷·되돌리기·백업·복원·복제가 TPM 상태(`tpm2-00.permall`)를 다루는 방식, 초기화 Task
//! (`vm-firmware-reset`)와 회차, 게스트 에이전트 fs-freeze 경고, 지운 VM의 상태 디렉터리 정리. Mock 하이퍼바이저는 swtpm을
//! 띄우지 않으므로 상태 파일은 시험이 직접 쓴다.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, MOUNT_ROOT, vm_request};
use orbivisor_driver::mock::GuestAnswer;
use orbivisor_driver::{DirStorage, Hypervisor, tpm_state_path, uefi_vars_path};
use orbivisor_peer::sha256_hex;
use orbivisor_types::backup::{
    BackupManifest, BackupSpec, MANIFEST_FILE, TPM_STATE_FILE, backup_dir,
};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::snapshot::SnapshotSpec;
use orbivisor_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use orbivisor_types::task::{Task, TaskOp, TaskSpec};
use orbivisor_types::vm::{PowerState, Vm, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn state(env: &Env) -> PathBuf {
    env.dir.path().join("state")
}

fn get_vm(env: &Env, id: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn failed(task: &Value) -> String {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    task["status"]["error"].as_str().unwrap().to_string()
}

/// UEFI·TPM VM을 만든다(디스크 `disks`). `tweak`으로 스펙을 더 바꾼다.
async fn tpm_vm(
    admin: &Client,
    name: &str,
    disks: &[&str],
    tweak: impl FnOnce(&mut Value),
) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["firmware"] = json!("uefi");
    req["spec"]["tpm"] = json!(true);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    tweak(&mut req["spec"]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 드라이버가 쓰는 자리에 UEFI 변수와 TPM 상태 파일을 둔다.
fn write_firmware(env: &Env, vm: Uuid, vars: &[u8], tpm: &[u8]) {
    let vars_path = uefi_vars_path(&state(env), vm);
    std::fs::create_dir_all(vars_path.parent().unwrap()).unwrap();
    std::fs::write(&vars_path, vars).unwrap();
    let tpm_path = tpm_state_path(&state(env), vm);
    std::fs::create_dir_all(tpm_path.parent().unwrap()).unwrap();
    std::fs::write(&tpm_path, tpm).unwrap();
}

/// VM status를 바꾼다(작업자가 TPM 칸을 기록한 것처럼).
async fn set_status(env: &Env, vm: Uuid, f: impl FnOnce(&mut VmStatus)) {
    let mut status = get_vm(env, vm).status;
    f(&mut status);
    env.cluster
        .update_status::<VmSpec>(vm, &status)
        .await
        .unwrap();
}

/// 이 노드가 실행할 Task를 저장소에 직접 만들고 끝날 때까지 기다린다.
async fn run_task(env: &Env, admin: &Client, op: TaskOp, target: Uuid) -> Value {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    admin.wait_task(&json!({ "id": task.id })).await
}

/// 이 노드에서 준비된 dir 풀
async fn dir_pool(env: &Env, name: &str, backups: bool) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 풀 `local`과 바로 Ready인 qcow2 볼륨 `root`(파일은 없다. Mock 하이퍼바이저는 열지 않는다)
async fn root_volume(env: &Env) {
    dir_pool(env, "local", false).await;
    let mut volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
            content: Default::default(),
            base: None,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

fn snapshot_of(env: &Env, vm: Uuid, name: &str) -> orbivisor_types::snapshot::Snapshot {
    let (_, all) = env.cluster.list::<SnapshotSpec>().unwrap();
    all.into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
        .unwrap()
}

async fn snapshot(admin: &Client, vm: &str, body: Value) -> Value {
    let (status, task) = admin.post(&format!("/vms/{vm}/snapshots"), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

/// 스냅샷은 TPM 상태를 `{스냅샷}.tpm`(0600)으로 담고 그 노드를 적는다. 되돌리기는 그것을 돌려놓고 해시·`firmware_node`·
/// 이 노드의 회차를 적으며 사본을 비운다. 지우기는 사본도 지운다.
#[tokio::test]
async fn snapshots_copy_and_revert_the_tpm_state() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    succeeded(&admin.act("win", "start").await);
    write_firmware(&env, vm, b"vars-a", b"tpm-a");

    succeeded(&snapshot(&admin, "win", json!({ "name": "a" })).await);
    let a = snapshot_of(&env, vm, "a");
    assert_eq!(a.status.tpm_node, Some(env.node));
    let copy = state(&env).join(format!("vm/{vm}/snapshots/{}.tpm", a.id));
    assert_eq!(std::fs::read(&copy).unwrap(), b"tpm-a");
    assert_eq!(mode(&copy), 0o600);

    // 그 뒤 TPM이 바뀌고 기록되고 사본이 생겼고 한 번 초기화됐다.
    std::fs::write(tpm_state_path(&state(&env), vm), b"tpm-b").unwrap();
    set_status(&env, vm, |s| {
        s.tpm_sha256 = Some(sha256_hex(b"tpm-b"));
        s.firmware_copies = vec![Uuid::now_v7()];
        s.firmware_epoch = 3;
    })
    .await;

    let (status, task) = admin.post("/vms/win/snapshots/a/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(
        std::fs::read(tpm_state_path(&state(&env), vm)).unwrap(),
        b"tpm-a"
    );
    let after = get_vm(&env, vm);
    assert_eq!(after.status.tpm_sha256, Some(sha256_hex(b"tpm-a")));
    assert_eq!(after.status.firmware_node, Some(env.node));
    assert!(after.status.firmware_copies.is_empty());
    assert_eq!(
        orbivisor_driver::read_firmware_epoch(&state(&env), vm).unwrap(),
        3
    );

    let (status, task) = admin.delete("/vms/win/snapshots/a").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert!(!copy.exists());
}

/// TPM 상태 사본이 다른 노드에 있는 스냅샷은 되돌리지 않는다. VM을 끄기 전에 실패한다.
#[tokio::test]
async fn revert_fails_before_stopping_when_the_tpm_copy_is_elsewhere() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    succeeded(&admin.act("win", "start").await);
    write_firmware(&env, vm, b"vars", b"tpm");
    succeeded(&snapshot(&admin, "win", json!({ "name": "a" })).await);
    let mut a = snapshot_of(&env, vm, "a");
    a.status.tpm_node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<SnapshotSpec>(a.id, &a.status)
        .await
        .unwrap();

    let (status, task) = admin.post("/vms/win/snapshots/a/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let error = failed(&admin.wait_task(&task).await);
    assert!(
        error.contains("the TPM copy of snapshot \"a\" is on node"),
        "{error}"
    );
    let after = get_vm(&env, vm);
    assert_eq!(after.spec.power, PowerState::Running);
    assert!(env.hv.inspect(vm).await.unwrap().is_some());
}

/// 실행 중인 디스크 스냅샷은 VM의 `guest_agent.freeze_fs`대로 얼린다. 얼리지 못하면 Task는 성공하고 경고를 남긴다.
/// 게스트 에이전트를 두지 않은 VM은 얼리지 않는다.
#[tokio::test]
async fn disk_snapshots_freeze_the_guest_and_warn_when_they_cannot() {
    let env = Env::start().await;
    let admin = env.admin().await;
    root_volume(&env).await;
    let vm = tpm_vm(&admin, "win", &["root"], |spec| {
        spec["guest_agent"] = json!({});
    })
    .await;
    succeeded(&admin.act("win", "start").await);
    env.hv.set_guest_agent(vm, GuestAnswer::Ok);

    let task = snapshot(&admin, "win", json!({ "name": "a", "memory": false })).await;
    succeeded(&task);
    assert_eq!(task["status"]["error"], Value::Null, "{task}");
    assert_eq!(env.hv.quiesce_calls(vm), [true]);

    env.hv.fail_freeze(vm, Some("fsfreeze is busy"));
    let task = snapshot(&admin, "win", json!({ "name": "b", "memory": false })).await;
    succeeded(&task);
    assert_eq!(
        task["status"]["error"],
        "guest agent did not freeze file systems (fsfreeze is busy); the snapshot is crash-consistent",
        "{task}"
    );

    // freeze_fs를 끈 VM은 얼리지 않는다.
    let (status, mut body) = admin.get("/vms/win").await;
    assert_eq!(status, StatusCode::OK);
    body["spec"]["guest_agent"]["freeze_fs"] = json!(false);
    let (status, body) = admin
        .put("/vms/win", json!({ "spec": body["spec"] }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    succeeded(&snapshot(&admin, "win", json!({ "name": "c", "memory": false })).await);
    assert_eq!(env.hv.quiesce_calls(vm), [true, true, false]);
}

/// 백업은 TPM 상태를 `tpm2-00.permall`(0600)로 담고 `tpm_state`를 적는다. 복원은 새 VM의 TPM 디렉터리에 두고 해시와
/// `firmware_node`를 적는다.
#[tokio::test]
async fn backups_carry_the_tpm_state_to_the_restored_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "bk", true).await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    write_firmware(&env, vm, b"vars", b"tpm-state");
    set_status(&env, vm, |s| {
        s.firmware_node = Some(env.node);
        s.tpm_sha256 = Some(sha256_hex(b"tpm-state"));
    })
    .await;

    let (status, task) = admin
        .post("/vms/win/backups", json!({ "pool": "bk", "name": "b1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let b = env
        .cluster
        .get_by_name::<BackupSpec>("b1")
        .unwrap()
        .unwrap();
    assert!(b.status.tpm_state);
    let dir = backup_dir(&env.dir.path().join("bk"), b.id);
    assert_eq!(
        std::fs::read(dir.join(TPM_STATE_FILE)).unwrap(),
        b"tpm-state"
    );
    assert_eq!(mode(&dir.join(TPM_STATE_FILE)), 0o600);
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert!(manifest.tpm_state);

    let (status, task) = admin
        .post(
            "/backups/b1/restore",
            json!({ "name": "win-2", "pool": "bk" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let new = env.cluster.get_by_name::<VmSpec>("win-2").unwrap().unwrap();
    assert_eq!(new.status.tpm_sha256, Some(sha256_hex(b"tpm-state")));
    assert_eq!(new.status.firmware_node, Some(env.node));
    let restored = tpm_state_path(&state(&env), new.id);
    assert_eq!(std::fs::read(&restored).unwrap(), b"tpm-state");
    assert_eq!(mode(&restored), 0o600);
    assert_eq!(mode(restored.parent().unwrap()), 0o700);
}

/// 기록된 TPM 상태를 가진 노드에 닿지 못하면 백업은 TPM 없이 담지 않고 실패한다.
#[tokio::test]
async fn backup_fails_when_no_node_holds_the_tpm_state() {
    let env = Env::start().await;
    let admin = env.admin().await;
    dir_pool(&env, "bk", true).await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    set_status(&env, vm, |s| {
        s.firmware_node = Some(Uuid::now_v7());
        s.tpm_sha256 = Some(sha256_hex(b"elsewhere"));
    })
    .await;
    let (status, task) = admin
        .post("/vms/win/backups", json!({ "pool": "bk", "name": "b1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let error = failed(&admin.wait_task(&task).await);
    assert!(error.contains("vm firmware reset win --tpm"), "{error}");
}

/// 복제는 늘 새 TPM이다: 새 VM에는 TPM 해시가 없고 TPM 파일도 복사하지 않으며, 원본에 TPM 상태가 있었으면 경고를 남긴다.
/// UEFI 변수는 그대로 복사한다.
#[tokio::test]
async fn clones_get_a_new_tpm_with_a_warning() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "tmpl", &[], |_| {}).await;
    write_firmware(&env, vm, b"vars", b"tpm");
    set_status(&env, vm, |s| {
        s.firmware_node = Some(env.node);
        s.tpm_sha256 = Some(sha256_hex(b"tpm"));
    })
    .await;

    let (status, task) = admin
        .post("/vms/tmpl/clone", json!({ "name": "web-1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    succeeded(&task);
    assert!(
        task["status"]["error"].as_str().unwrap().contains(
            "VM \"web-1\" got a new TPM; secrets sealed to the TPM of \"tmpl\" do not carry over"
        ),
        "{task}"
    );
    let copy = env.cluster.get_by_name::<VmSpec>("web-1").unwrap().unwrap();
    assert!(copy.spec.tpm);
    assert_eq!(copy.status.tpm_sha256, None);
    assert_eq!(copy.status.firmware_node, Some(env.node));
    assert_eq!(
        std::fs::read(uefi_vars_path(&state(&env), copy.id)).unwrap(),
        b"vars"
    );
    assert!(!tpm_state_path(&state(&env), copy.id).exists());
}

/// 초기화 Task: 변수만이면 TPM 상태와 그 자리·사본을 지키고, TPM만이면 이 노드의 변수를 지키며, 둘 다면 자리도 지운다.
/// 매번 회차를 올리고 이 노드의 회차 파일을 적는다. 켜진 VM은 초기화하지 않는다.
#[tokio::test]
async fn firmware_reset_bumps_the_epoch_and_removes_this_nodes_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    write_firmware(&env, vm, b"vars", b"tpm");
    let copy = Uuid::now_v7();
    set_status(&env, vm, |s| {
        s.firmware_node = Some(env.node);
        s.tpm_sha256 = Some(sha256_hex(b"tpm"));
        s.firmware_copies = vec![copy];
        s.firmware_epoch = 1;
    })
    .await;
    let vars = uefi_vars_path(&state(&env), vm);
    let tpm = tpm_state_path(&state(&env), vm);
    let reset = |uefi_vars, tpm| TaskOp::VmFirmwareReset {
        uefi_vars,
        tpm,
        secure_boot: None,
    };

    succeeded(&run_task(&env, &admin, reset(true, false), vm).await);
    let after = get_vm(&env, vm);
    assert!(!vars.exists());
    assert!(tpm.exists());
    assert_eq!(after.status.firmware_epoch, 2);
    // 변수는 어디에도 지금 것이 없고, TPM 상태와 사본은 그대로다(리뷰 2 #3).
    assert_eq!(after.status.firmware_node, None);
    assert_eq!(after.status.tpm_sha256, Some(sha256_hex(b"tpm")));
    assert_eq!(after.status.firmware_copies, [copy]);
    assert_eq!(
        orbivisor_driver::read_firmware_epoch(&state(&env), vm).unwrap(),
        2
    );

    std::fs::write(&vars, b"vars-2").unwrap();
    succeeded(&run_task(&env, &admin, reset(false, true), vm).await);
    let after = get_vm(&env, vm);
    assert!(vars.exists());
    assert!(!tpm.parent().unwrap().exists());
    assert_eq!(after.status.firmware_epoch, 3);
    assert_eq!(after.status.firmware_node, None);
    assert_eq!(after.status.tpm_sha256, None);
    assert!(after.status.firmware_copies.is_empty());
    assert_eq!(
        orbivisor_driver::read_firmware_epoch(&state(&env), vm).unwrap(),
        3
    );

    succeeded(&run_task(&env, &admin, reset(true, true), vm).await);
    let after = get_vm(&env, vm);
    assert!(!vars.exists());
    assert_eq!(after.status.firmware_epoch, 4);
    assert_eq!(after.status.firmware_node, None);

    succeeded(&admin.act("win", "start").await);
    let error = failed(&run_task(&env, &admin, reset(true, true), vm).await);
    assert!(error.contains("is running"), "{error}");
    assert_eq!(get_vm(&env, vm).status.firmware_epoch, 4);
}

/// 지운 VM의 상태 디렉터리는 정리가 지운다. 있는 VM(다른 노드가 맡은 VM 포함)과 끝나지 않은 Task의 대상, VM id가 아닌
/// 디렉터리는 두고, 새로 바뀐 디렉터리도 시간이 지나기 전에는 둔다.
#[tokio::test]
async fn state_directories_of_deleted_vms_are_removed() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let kept = tpm_vm(&admin, "win", &[], |_| {}).await;
    // 다른 노드가 맡은 VM
    let mut elsewhere = get_vm(&env, kept).spec;
    elsewhere.node = Some(Uuid::now_v7());
    for nic in &mut elsewhere.nics {
        nic.mac = None;
    }
    let other: Vm = Resource::new("other", elsewhere);
    env.cluster.create(&other).await.unwrap();
    // 복원 Task(다른 노드에서 기다림)의 대상
    let restoring = Uuid::now_v7();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmStart,
            target: restoring,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    let deleted = Uuid::now_v7();
    for id in [kept, other.id, restoring, deleted] {
        write_firmware(&env, id, b"vars", b"tpm");
    }
    std::fs::create_dir_all(state(&env).join("vm/not-a-vm")).unwrap();
    let root = state(&env).join("vm");
    let dir = |id: Uuid| root.join(id.to_string());

    // 기본 한도(1시간)로는 막 바뀐 디렉터리를 지우지 않는다.
    env.stop_runner().await;
    env.start_runner().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(dir(deleted).exists());

    // 시작할 때는 후보만 모은다(다시 합류한 노드의 클러스터 상태가 늦을 수 있다, 리뷰 1 I3).
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.state_dir_min_age = Duration::ZERO;
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(dir(deleted).exists());

    // 다음 차례에도 후보이면 지운다.
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.state_dir_min_age = Duration::ZERO;
        c.tag_sweep = Duration::from_millis(50);
    })
    .await;
    let deadline = tokio::time::Instant::now() + common::WAIT;
    while dir(deleted).exists() {
        assert!(tokio::time::Instant::now() < deadline, "not removed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    for id in [kept, other.id, restoring] {
        assert!(tpm_state_path(&state(&env), id).exists(), "{id}");
    }
    assert!(state(&env).join("vm/not-a-vm").exists());
}

/// 메모리 스냅샷은 기록한 하드웨어의 TPM·Secure Boot가 스냅샷의 VM 스펙과 다르면 되돌리지 않는다(409 `snapshot_mismatch`,
/// 계획 59 §7). 스냅샷에 게스트 에이전트가 있으면 QGA 채널은 늘 있으므로 지금 게스트 감시와 상관없다.
#[tokio::test]
async fn memory_snapshots_need_the_devices_they_were_taken_with() {
    let env = Env::start().await;
    let admin = env.admin().await;
    root_volume(&env).await;
    let vm = tpm_vm(&admin, "win", &["root"], |spec| {
        spec["guest_agent"] = json!({});
    })
    .await;
    succeeded(&admin.act("win", "start").await);
    succeeded(&snapshot(&admin, "win", json!({ "name": "a" })).await);
    let mut a = snapshot_of(&env, vm, "a");
    assert!(a.spec.memory);
    let mut hw = a
        .spec
        .hardware
        .clone()
        .or(get_vm(&env, vm).status.hardware)
        .expect("running hardware");
    assert!(hw.tpm);
    hw.tpm = false;
    a.spec.hardware = Some(hw);
    env.cluster
        .update_spec(a.id, None, a.labels.clone(), &a.spec)
        .await
        .unwrap();

    let (status, body) = admin.post("/vms/win/snapshots/a/revert", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_mismatch", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("whose tpm differs"),
        "{body}"
    );
}

/// 리뷰 2 #1a: 이 노드의 회차가 초기화 전 VM의 회차와도 달랐으면(앞 초기화 때 오프라인) 초기화하지 않는 변수도 낡은
/// 것이라 TPM만 초기화해도 지운다. 회차가 같았으면 변수를 지킨다(위 시험).
#[tokio::test]
async fn a_reset_also_drops_files_left_from_an_older_reset() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    write_firmware(&env, vm, b"vars from epoch 0", b"tpm");
    set_status(&env, vm, |s| {
        s.firmware_node = Some(env.node);
        s.firmware_epoch = 1;
    })
    .await;
    let reset = TaskOp::VmFirmwareReset {
        uefi_vars: false,
        tpm: true,
        secure_boot: None,
    };
    succeeded(&run_task(&env, &admin, reset, vm).await);
    assert!(!uefi_vars_path(&state(&env), vm).exists());
    assert!(!tpm_state_path(&state(&env), vm).exists());
    assert_eq!(
        orbivisor_driver::read_firmware_epoch(&state(&env), vm).unwrap(),
        2
    );
}

/// 리뷰 2 #1c: TPM 사본이 없는 스냅샷을 되돌리면 이 노드의 회차가 달랐을 때 남은 낡은 TPM 상태를 지운다(회차만 적으면
/// 새 회차의 것으로 보인다). 되돌린 변수는 남는다.
#[tokio::test]
async fn reverting_a_snapshot_without_a_tpm_drops_a_stale_tpm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "web", &[], |spec| spec["tpm"] = json!(false)).await;
    succeeded(&admin.act("web", "start").await);
    let vars = uefi_vars_path(&state(&env), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-a").unwrap();
    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
    assert_eq!(snapshot_of(&env, vm, "a").status.tpm_node, None);

    // 초기화 전(회차 0)의 TPM 상태가 남았고 VM은 그 뒤 초기화됐다.
    let tpm = tpm_state_path(&state(&env), vm);
    std::fs::create_dir_all(tpm.parent().unwrap()).unwrap();
    std::fs::write(&tpm, b"old tpm").unwrap();
    std::fs::write(&vars, b"vars-b").unwrap();
    set_status(&env, vm, |s| s.firmware_epoch = 1).await;

    let (status, task) = admin.post("/vms/web/snapshots/a/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(std::fs::read(&vars).unwrap(), b"vars-a");
    assert!(!tpm.exists());
    assert_eq!(
        orbivisor_driver::read_firmware_epoch(&state(&env), vm).unwrap(),
        1
    );
}

/// 리뷰 2 #8: 기록된 TPM 상태가 없을 때(TPM 초기화 뒤) 이 노드의 회차가 다르면 남은 파일은 초기화 전의 것이라 스냅샷에
/// 담지 않는다.
#[tokio::test]
async fn snapshots_do_not_capture_a_tpm_from_before_a_reset() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = tpm_vm(&admin, "win", &[], |_| {}).await;
    write_firmware(&env, vm, b"vars", b"old tpm");
    set_status(&env, vm, |s| s.firmware_epoch = 1).await;
    succeeded(&snapshot(&admin, "win", json!({ "name": "a" })).await);
    assert_eq!(snapshot_of(&env, vm, "a").status.tpm_node, None);

    // 회차가 같으면(이 노드에서 새로 만든 상태) 담는다.
    orbivisor_driver::write_firmware_epoch(&state(&env), vm, 1).unwrap();
    succeeded(&snapshot(&admin, "win", json!({ "name": "b" })).await);
    assert_eq!(snapshot_of(&env, vm, "b").status.tpm_node, Some(env.node));
}

/// 리뷰 2 #7: 정리는 회차가 달라 남은 지금 것이 아닌 TPM 상태(오프라인 동안 초기화됨)를 두 차례 본 뒤 지운다. 이 노드가
/// `tpm_node`이거나 해시가 같은 TPM, 회차가 같은 TPM은 둔다.
#[tokio::test]
async fn the_sweep_removes_tpm_states_from_before_a_reset() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let stale = tpm_vm(&admin, "stale", &[], |_| {}).await;
    let holder = tpm_vm(&admin, "holder", &[], |_| {}).await;
    let copy = tpm_vm(&admin, "copy", &[], |_| {}).await;
    for id in [stale, holder, copy] {
        write_firmware(&env, id, b"vars", b"tpm");
    }
    set_status(&env, stale, |s| {
        s.firmware_epoch = 1;
        s.tpm_sha256 = Some(sha256_hex(b"newer"));
    })
    .await;
    set_status(&env, holder, |s| {
        s.firmware_epoch = 1;
        s.tpm_node = Some(env.node);
        s.tpm_sha256 = Some(sha256_hex(b"newer"));
    })
    .await;
    set_status(&env, copy, |s| {
        s.firmware_epoch = 1;
        s.tpm_sha256 = Some(sha256_hex(b"tpm"));
    })
    .await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.tag_sweep = Duration::from_millis(50);
    })
    .await;
    let deadline = tokio::time::Instant::now() + common::WAIT;
    while tpm_state_path(&state(&env), stale).exists() {
        assert!(tokio::time::Instant::now() < deadline, "not removed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 변수는 켤 때 지운다(정리는 비밀인 TPM 상태만 지운다).
    assert!(uefi_vars_path(&state(&env), stale).exists());
    tokio::time::sleep(Duration::from_millis(200)).await;
    for id in [holder, copy] {
        assert!(tpm_state_path(&state(&env), id).exists(), "{id}");
    }
}
