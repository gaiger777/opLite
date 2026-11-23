//! 라이브 복원 Task(계획 68 스테이지 5, 가짜 하이퍼바이저): 저장소 백업을 새 VM으로 라이브 복원해 VM이 배선(NBD 백업 →
//! 오버레이)으로 켜지고, mirror가 다 옮기면 갈아탄 뒤 보통 VM이 되는지 본다. 그 사이 opld 다시 시작(Task 이어 감),
//! 백업을 읽지 못해 멈춤(`Paused` → `cont`), 강제 정지·다시 켜기, `vm_restoring` 409, 취소(VM·디스크 지움), 옛 풀 백업의
//! qcow2 체인 배선을 본다. 라이브 복원 API는 스테이지 6이므로 Task를 클러스터에 바로 쓴다. qemu-img·qemu-io·qemu-nbd가
//! 필요하다. `cargo test -p opl-api --features storage-it --test live_restore_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Client, Env, vm_request};
use opl_driver::{Hypervisor, LiveRestoreSource, RuntimeStatus};
use opl_store::format::StoreInfo;
use opl_store::nbd::NbdClient;
use opl_types::backup::{Backup, BackupSpec};
use opl_types::backup_store::{BackupStore, BackupStoreSpec, LiveRestorePhase, StorePhase};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn tool(name: &str, args: &[&str]) {
    let out = Command::new(name).args(args).output().unwrap();
    assert!(out.status.success(), "{name} {args:?}: {out:?}");
}

fn pool_dir(env: &Env, pool: &str) -> PathBuf {
    env.dir.path().join(pool)
}

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
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 내용이 든 디스크 `root`(12 MiB)를 가진 꺼진 VM `web`
async fn source_vm(env: &Env, admin: &Client) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({"name": "root", "spec": {"pool": "local", "size_bytes": 12 * MIB, "format": "qcow2"}}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let v = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let root = pool_dir(env, "local").join(format!("{}.qcow2", v.id));
    for cmd in ["write -P 0xaa 0 64k", "write -P 0x5b 9M 64k"] {
        tool(
            "qemu-io",
            &["-f", "qcow2", "-c", cmd, root.to_str().unwrap()],
        );
    }
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    root
}

fn raw_of(path: &Path) -> Vec<u8> {
    let out = path.with_extension("raw-check");
    tool(
        "qemu-img",
        &[
            "convert",
            "-U",
            "-O",
            "raw",
            path.to_str().unwrap(),
            out.to_str().unwrap(),
        ],
    );
    let b = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(out);
    b
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

async fn seed_store(env: &Env, name: &str) -> BackupStore {
    let spec: BackupStoreSpec = serde_json::from_value(json!({"backend": {"pool": "bk"}})).unwrap();
    let mut store: BackupStore = Resource::new(name, spec);
    store.status.phase = StorePhase::Ready;
    store.status.store_id = Some(store.id);
    store.status.node = Some(env.node);
    let dir = pool_dir(env, "bk").join("store").join(store.id.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let info = StoreInfo::new(store.id, None, 1);
    std::fs::write(dir.join("store.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    env.cluster.create(&store).await.unwrap();
    store
}

fn create_task(op: TaskOp, target: Uuid, node: Uuid) -> opl_types::task::Task {
    let mut t: opl_types::task::Task = Resource::new("", TaskSpec { op, target, node });
    t.name = t.id.to_string();
    t
}

/// VM `web`의 저장소 백업 `b1`(꺼진 VM — `qemu-nbd`)
async fn store_backup(env: &Env) -> Backup {
    let v = vm_named(env, "web").unwrap();
    let b: Backup = Resource::new(
        "b1",
        BackupSpec {
            vm: v.id,
            vm_name: v.name.clone(),
            pool: String::new(),
            store: Some("st".into()),
            source: None,
            vm_spec: v.spec.clone(),
            job: None,
            origin: None,
            incremental: false,
            max_chain: None,
        },
    );
    env.cluster.create(&b).await.unwrap();
    let t = create_task(TaskOp::VmBackup { backup: b.id }, v.id, env.node);
    env.cluster.create(&t).await.unwrap();
    succeeded(&env.admin().await.wait_task(&json!({"id": t.id})).await);
    b
}

fn vm_named(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn vm(env: &Env, id: Uuid) -> Option<Vm> {
    env.cluster.get::<VmSpec>(id).unwrap()
}

async fn wait_vm(env: &Env, id: Uuid, what: &str, pred: impl Fn(&Vm) -> bool) -> Vm {
    for _ in 0..500 {
        if let Some(v) = vm(env, id)
            && pred(&v)
        {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "timed out waiting for {what}: {:?}",
        vm(env, id).map(|v| v.status)
    );
}

fn phase(v: &Vm) -> Option<LiveRestorePhase> {
    v.status.live_restore.as_ref().map(|l| l.phase)
}

async fn start_live_restore(env: &Env, backup: Uuid, name: &str) -> (Uuid, Uuid) {
    let target = Uuid::now_v7();
    let op = TaskOp::BackupLiveRestore {
        backup,
        name: name.into(),
        pool: "local".into(),
        folder: None,
    };
    let t = create_task(op, target, env.node);
    env.cluster.create(&t).await.unwrap();
    (target, t.id)
}

/// 배선의 첫 디스크를 NBD로 다 읽는다(게스트가 보는 백업).
async fn read_source(socket: &Path) -> Vec<u8> {
    let mut c = NbdClient::connect(socket, "disk0", &[]).await.unwrap();
    let size = c.size();
    let mut out = Vec::new();
    while (out.len() as u64) < size {
        let len = (size - out.len() as u64).min(4 * MIB) as u32;
        out.extend(c.read(out.len() as u64, len).await.unwrap());
    }
    out
}

#[tokio::test]
async fn store_backup_live_restore_runs_survives_restarts_and_pivots() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    seed_store(&env, "st").await;
    let root = source_vm(&env, &admin).await;
    let b = store_backup(&env).await;

    let (id, task) = start_live_restore(&env, b.id, "web2").await;
    // VM이 배선(NBD 백업)으로 켜지고 mirror가 돈다.
    let v = wait_vm(&env, id, "copying", |v| {
        v.status.phase == VmPhase::Running && phase(v) == Some(LiveRestorePhase::Copying)
    })
    .await;
    assert_eq!(v.spec.node, Some(env.node));
    assert_eq!(v.spec.power, PowerState::Running);
    assert_eq!(env.hv.live_launches(id), 1);
    let wiring = env.hv.live_restore_wiring(id).unwrap();
    let LiveRestoreSource::Nbd { socket, export } = &wiring[0].source else {
        panic!("{wiring:?}");
    };
    assert_eq!(export, "disk0");
    assert!(wiring[0].overlay.exists(), "{wiring:?}");
    // 게스트가 보는 백업은 원본 디스크와 같다.
    assert_eq!(read_source(socket).await, raw_of(&root));
    let target = env
        .cluster
        .get_by_name::<VolumeSpec>(&v.spec.disks[0].volume)
        .unwrap()
        .unwrap();
    assert_eq!(target.spec.size_bytes, 12 * MIB);
    // 백업은 라이브 복원이 끝날 때까지 지우지 못한다.
    let (status, body) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 디스크를 다루는 동작은 409 `vm_restoring`
    let (status, body) = admin.post("/vms/web2/actions/stop", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_restoring", "{body}");
    let (status, body) = admin.delete("/vms/web2").await;
    assert_eq!(body["code"], "vm_restoring", "{status} {body}");
    let (status, body) = admin
        .post("/vms/web2/snapshots", json!({"name": "s1"}))
        .await;
    assert_eq!(body["code"], "vm_restoring", "{status} {body}");

    // 백업을 읽지 못해 VM이 멈추면 `Paused`로 적고 `cont`로 잇는다.
    env.hv
        .set_status(id, Some(RuntimeStatus::Other("io-error".into())));
    wait_vm(&env, id, "paused", |v| {
        phase(v) == Some(LiveRestorePhase::Paused)
    })
    .await;
    wait_vm(&env, id, "copying again", |v| {
        phase(v) == Some(LiveRestorePhase::Copying)
    })
    .await;
    assert!(env.hv.pause_calls(id).contains(&"resume"));

    // 강제 정지 → `Stopped`(배선 그대로), 다시 켜면 같은 배선으로
    succeeded(&admin.act("web2", "stop?mode=force").await);
    wait_vm(&env, id, "stopped", |v| {
        phase(v) == Some(LiveRestorePhase::Stopped)
    })
    .await;
    assert!(env.hv.live_restore_wired(id));
    succeeded(&admin.act("web2", "start").await);
    wait_vm(&env, id, "copying after start", |v| {
        phase(v) == Some(LiveRestorePhase::Copying) && v.status.phase == VmPhase::Running
    })
    .await;
    assert_eq!(env.hv.live_launches(id), 2);

    // opld가 다시 떠도 Task가 잇는다(헬퍼 — 여기서는 opld 안 서버 — 를 다시 띄운다).
    env.stop_runner().await;
    env.start_runner().await;
    let polls = env.hv.live_restore_polls(id);
    for _ in 0..500 {
        if env.hv.live_restore_polls(id) > polls {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(env.hv.live_restore_polls(id) > polls);
    assert_eq!(read_source(socket).await, raw_of(&root));

    // 첫 갈아타기는 실패한다(작업이 없어졌다): `Pivoting`에서 맴돌지 않고 `Copying`으로 돌아가 다시 옮긴 뒤 갈아탄다(리뷰 1 I3).
    env.hv.fail_next_pivot(opl_driver::DriverError::LiveRestore(
        "opl-lr-0: write error".into(),
    ));
    // 다 옮기면 갈아타고 보통 VM이 된다.
    env.hv.finish_live_restore_copy(id);
    let t = admin.wait_task(&json!({"id": task})).await;
    succeeded(&t);
    let v = vm(&env, id).unwrap();
    assert_eq!(v.status.live_restore, None);
    assert_eq!(env.hv.pivots(), vec![id]);
    assert!(!env.hv.live_restore_wired(id));
    assert!(!wiring[0].overlay.exists());
    assert!(!socket.exists());
    // 이제 보통 VM이라 막힌 동작이 풀린다.
    let (status, body) = admin.post("/vms/web2/actions/stop", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn cancel_deletes_the_vm_and_pool_backups_wire_their_chain() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    source_vm(&env, &admin).await;
    // 옛 풀 백업(qcow2 — 꺼진 VM)
    let (status, t) = admin
        .post("/vms/web/backups", json!({"pool": "bk", "name": "old"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    let old = env
        .cluster
        .get_by_name::<BackupSpec>("old")
        .unwrap()
        .unwrap();

    let (id, task) = start_live_restore(&env, old.id, "web3").await;
    let v = wait_vm(&env, id, "copying", |v| {
        v.status.phase == VmPhase::Running && phase(v) == Some(LiveRestorePhase::Copying)
    })
    .await;
    // 풀 백업은 헬퍼 없이 QEMU가 qcow2 파일을 곧바로 연다.
    let wiring = env.hv.live_restore_wiring(id).unwrap();
    let LiveRestoreSource::Qcow2 { chain } = &wiring[0].source else {
        panic!("{wiring:?}");
    };
    assert_eq!(chain.len(), 1);
    assert!(chain[0].starts_with(pool_dir(&env, "bk")), "{chain:?}");
    let volume = v.spec.disks[0].volume.clone();

    // 취소: VM을 끄고 VM·대상 볼륨·배선·오버레이를 지운다(Task는 취소로 끝남).
    opl_api::request_live_restore_cancel(&env.cluster, id)
        .await
        .unwrap();
    let t = admin.wait_task(&json!({"id": task})).await;
    assert_eq!(t["status"]["state"], "failed", "{t}");
    assert!(
        t["status"]["error"].as_str().unwrap().contains("cancelled"),
        "{t}"
    );
    assert!(vm(&env, id).is_none());
    assert!(env.pid(id).await.is_none());
    assert!(
        env.cluster
            .get_by_name::<VolumeSpec>(&volume)
            .unwrap()
            .is_none()
    );
    assert!(!env.hv.live_restore_wired(id));
    assert!(!wiring[0].overlay.exists());
    // 라이브 복원 중이 아닌 VM은 취소할 수 없다.
    let web = vm_named(&env, "web").unwrap();
    assert!(
        opl_api::request_live_restore_cancel(&env.cluster, web.id)
            .await
            .is_err()
    );
}
