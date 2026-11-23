//! 저장소 백업 Task(계획 68 스테이지 4): 백업 풀 안 저장소에 꺼진 VM(`qemu-nbd`)과 실행 중인 VM(가짜 하이퍼바이저의
//! fleecing NBD)을 백업하고, 증분(base·비트맵 문맥), 복원(`qemu-img convert` ← 이 노드 안 NBD 서버), 지우기(가운데 백업도),
//! 커밋 확인(쓸기가 지운 청크를 다시 올림), GC(남의 오래된 청크만 지움·잠금), 검증(깨진 청크 → `bad/`·실패·다음 백업이
//! 고침), 옛 풀 백업 옮기기, S3(가짜) 읽기 전용 저장소의 scan, 리더 예약(GC·검증·작업의 저장소)을 본다.
//! 저장소 API는 스테이지 6이므로 저장소·백업·Task는 클러스터에 바로 쓴다. qemu-img·qemu-io·qemu-nbd가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_store_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, vm_request};
use opl_store::Store;
use opl_store::backend::local::LocalBackend;
use opl_store::format::StoreInfo;
use opl_types::backup::{Backup, BackupPhase, BackupSpec, VerifyState};
use opl_types::backup_store::{BackupStore, BackupStoreSpec, StorePhase};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;
const CHUNK: usize = 4 << 20;

fn tool(name: &str, args: &[&str]) -> String {
    let out = Command::new(name).args(args).output().unwrap();
    assert!(out.status.success(), "{name} {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn qemu_io(path: &Path, format: &str, cmd: &str) {
    tool(
        "qemu-io",
        &["-f", format, "-c", cmd, path.to_str().unwrap()],
    );
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

async fn create_volume(env: &Env, admin: &Client, name: &str, size: u64) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({"name": name, "spec": {"pool": "local", "size_bytes": size, "format": "qcow2"}}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let v = env
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap();
    pool_dir(env, "local").join(format!("{}.qcow2", v.id))
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

/// 백업 풀 `bk` 안 저장소 `st`를 준비된 상태로 만든다(초기화 Task의 `chunks/` 디렉터리 65,536개는 따로 본다).
async fn seed_store(env: &Env, name: &str) -> BackupStore {
    let spec: BackupStoreSpec = serde_json::from_value(json!({"backend": {"pool": "bk"}})).unwrap();
    let mut store: BackupStore = Resource::new(name, spec);
    store.status.phase = StorePhase::Ready;
    store.status.store_id = Some(store.id);
    store.status.node = Some(env.node);
    let dir = store_dir(env, &store);
    std::fs::create_dir_all(&dir).unwrap();
    let info = StoreInfo::new(store.id, None, 1);
    std::fs::write(dir.join("store.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    env.cluster.create(&store).await.unwrap();
    store
}

fn store_dir(env: &Env, store: &BackupStore) -> PathBuf {
    pool_dir(env, "bk").join("store").join(store.id.to_string())
}

async fn open(env: &Env, store: &BackupStore) -> Store {
    Store::open(Arc::new(LocalBackend::new(store_dir(env, store))), None)
        .await
        .unwrap()
}

fn get_store(env: &Env, name: &str) -> BackupStore {
    env.cluster
        .get_by_name::<BackupStoreSpec>(name)
        .unwrap()
        .unwrap()
}

fn backup(env: &Env, name: &str) -> Backup {
    env.cluster
        .get_by_name::<BackupSpec>(name)
        .unwrap()
        .unwrap()
}

async fn task(env: &Env, op: TaskOp, target: Uuid) -> Value {
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    env.admin().await.wait_task(&json!({"id": t.id})).await
}

/// VM `vm`의 저장소 백업 `name`을 만들고 그 Task가 끝나기를 기다린다.
async fn store_backup(env: &Env, vm: &str, store: &str, name: &str, incremental: bool) -> Value {
    let v = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>(vm)
        .unwrap()
        .unwrap();
    let b: Backup = Resource::new(
        name,
        BackupSpec {
            vm: v.id,
            vm_name: v.name.clone(),
            pool: String::new(),
            store: Some(store.into()),
            source: None,
            vm_spec: v.spec.clone(),
            job: None,
            origin: None,
            incremental,
            max_chain: None,
        },
    );
    env.cluster.create(&b).await.unwrap();
    task(env, TaskOp::VmBackup { backup: b.id }, v.id).await
}

/// 볼륨 파일(qcow2)의 내용을 raw로
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

/// 백업 `name`을 새 VM `to`로 복원하고 첫 디스크 내용을 돌려준다.
async fn restore(env: &Env, name: &str, to: &str) -> Vec<u8> {
    let admin = env.admin().await;
    let (status, t) = admin
        .post(
            &format!("/backups/{name}/restore"),
            json!({"name": to, "pool": "local"}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    let vm = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>(to)
        .unwrap()
        .unwrap();
    let v = env
        .cluster
        .get_by_name::<VolumeSpec>(&vm.spec.disks[0].volume)
        .unwrap()
        .unwrap();
    raw_of(&pool_dir(env, "local").join(format!("{}.qcow2", v.id)))
}

fn chunk_files(env: &Env, store: &BackupStore) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let dir = store_dir(env, store).join("chunks");
    for sub in std::fs::read_dir(&dir).into_iter().flatten() {
        for f in std::fs::read_dir(sub.unwrap().path()).unwrap() {
            let p = f.unwrap().path();
            if !p.to_string_lossy().contains(".tmp.") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// 디스크 이미지: 청크 n마다 값 `fill[n]`(0이면 0 청크)
fn image(fill: &[u8]) -> Vec<u8> {
    fill.iter().flat_map(|b| vec![*b; CHUNK]).collect()
}

#[tokio::test]
async fn stopped_vm_backup_restores_and_deletes() {
    let mut env = Env::start().await;
    // 새 데이터 디렉터리처럼 소켓 디렉터리가 아직 없다(GitHub 탐침: qemu-nbd가 소켓을 못 만들었다).
    let tmp = tempfile::tempdir_in("/tmp").unwrap();
    let run = tmp.path().join("backup-run");
    env.stop_runner().await;
    let dir = run.clone();
    env.start_runner_config(
        Arc::new(opl_driver::DirStorage::new("qemu-img", common::MOUNT_ROOT)),
        move |c| c.backup_run_dir = dir,
    )
    .await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    let root = create_volume(&env, &admin, "root", 12 * MIB).await;
    qemu_io(&root, "qcow2", "write -P 0xaa 0 64k");
    qemu_io(&root, "qcow2", "write -P 0x5b 9M 64k");
    create_vm(&admin, "web", &["root"]).await;
    let vars = opl_driver::uefi_vars_path(&env.dir.path().join("state"), backup_vm(&env, "web"));
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"uefi vars").unwrap();

    succeeded(&store_backup(&env, "web", "st", "b1", false).await);
    let b = backup(&env, "b1");
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    assert!(b.status.uefi_vars);
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&run).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }
    let stats = b.status.store_stats.unwrap();
    // 청크 0(0xaa)·2(0x5b)만 내용이 있고 청크 1은 0(qcow2가 할당하지 않음 — 읽지도 않는다)
    assert_eq!((stats.new_chunks, stats.zero_chunks), (2, 1), "{stats:?}");
    assert_eq!(stats.logical_bytes, 12 * MIB);
    assert_eq!(b.status.disks[0].file, "disk0.fidx");
    assert_eq!(chunk_files(&env, &st).len(), 2);
    // 저장소에 매니페스트가 있고 커밋 잠금은 풀렸다
    let store = open(&env, &st).await;
    let m = store.read_manifest(b.spec.vm, b.id).await.unwrap();
    assert_eq!(m.disks[0].zero_chunks, 1);
    assert!(get_store(&env, "st").status.committing.is_empty());

    let restored = restore(&env, "b1", "web2").await;
    assert_eq!(restored, raw_of(&root));
    let web2 = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web2")
        .unwrap()
        .unwrap();
    let vars2 = opl_driver::uefi_vars_path(&env.dir.path().join("state"), web2.id);
    assert_eq!(std::fs::read(vars2).unwrap(), b"uefi vars");

    // 같은 내용의 두 번째 백업은 새 청크가 없다
    succeeded(&store_backup(&env, "web", "st", "b2", false).await);
    assert_eq!(backup(&env, "b2").status.store_stats.unwrap().new_chunks, 0);
    // 앞 백업을 지워도(체인 없음) 뒤 백업은 그대로 복원된다
    let (status, t) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    assert!(
        env.cluster
            .get_by_name::<BackupSpec>("b1")
            .unwrap()
            .is_none()
    );
    assert!(store.read_manifest(b.spec.vm, b.id).await.is_err());
    assert_eq!(restore(&env, "b2", "web3").await, raw_of(&root));
}

fn backup_vm(env: &Env, name: &str) -> Uuid {
    env.cluster
        .get_by_name::<opl_types::vm::VmSpec>(name)
        .unwrap()
        .unwrap()
        .id
}

#[tokio::test]
async fn running_vm_backups_are_incremental_from_the_dirty_bitmap() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 12 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let first = image(&[1, 0, 2]);
    env.hv.set_fleece_images(vm, vec![first.clone()]);

    // 증분을 요청해도 앞 백업이 없으면 전체로 읽는다.
    succeeded(&store_backup(&env, "web", "st", "f1", true).await);
    let f1 = backup(&env, "f1");
    assert_eq!(f1.status.base, None);
    assert!(
        f1.status
            .full_reason
            .as_deref()
            .unwrap()
            .contains("no earlier backup"),
        "{f1:?}"
    );
    assert_eq!(f1.status.store_stats.unwrap().new_chunks, 2);
    assert_eq!(chunk_files(&env, &st).len(), 2);

    // 청크 1만 바뀜(비트맵 문맥) → 그 청크만 읽고 나머지는 base 인덱스에서 옮긴다.
    let mut second = first.clone();
    second[CHUNK + 10..CHUNK + 20].fill(7);
    env.hv.set_fleece_images(vm, vec![second.clone()]);
    env.hv
        .set_fleece_dirty(vm, vec![vec![(CHUNK as u64 + 10, 10)]]);
    succeeded(&store_backup(&env, "web", "st", "i1", true).await);
    let i1 = backup(&env, "i1");
    assert_eq!(i1.status.base, Some(f1.id), "{i1:?}");
    let stats = i1.status.store_stats.unwrap();
    assert_eq!(
        (stats.read_bytes, stats.new_chunks),
        (CHUNK as u64, 1),
        "{stats:?}"
    );
    assert_eq!(stats.reused_chunks, 2);
    // fleecing은 열고 닫혔고, 성공이면 부모 비트맵을 지운다
    assert!(env.hv.open_fleeces().is_empty());
    let calls = env.hv.fleece_calls();
    let last = calls.last().unwrap();
    assert_eq!(last.2, Some(true));
    assert_eq!(last.1.parent, Some(opl_types::backup::bitmap_name(f1.id)));

    // 복원은 완전한 인덱스로(체인 없음) — base를 지워도 된다.
    let (status, t) = admin.delete("/backups/f1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    let restored = restore(&env, "i1", "web2").await;
    assert_eq!(restored, second);
}

/// 전체 백업도 성공하면 다른 백업 비트맵을 지운다(리뷰 1 I1): 전체 → 전체 → 증분은 둘째 전체를 base로 쓰고, 디스크엔
/// 비트맵이 하나만 남는다.
#[tokio::test]
async fn full_backups_drop_older_bitmaps_so_incrementals_continue() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    env.hv.set_fleece_images(vm, vec![image(&[1, 2])]);
    succeeded(&store_backup(&env, "web", "st", "f1", false).await);
    succeeded(&store_backup(&env, "web", "st", "f2", false).await);
    let f2 = backup(&env, "f2");
    let names = |env: &Env| -> Vec<String> {
        env.hv.bitmaps(vm)[0]
            .iter()
            .map(|b| b.name.clone())
            .collect()
    };
    assert_eq!(names(&env), vec![opl_types::backup::bitmap_name(f2.id)]);
    env.hv.set_fleece_dirty(vm, vec![vec![(10, 10)]]);
    succeeded(&store_backup(&env, "web", "st", "i1", true).await);
    let i1 = backup(&env, "i1");
    assert_eq!(i1.status.base, Some(f2.id), "{i1:?}");
    assert_eq!(names(&env), vec![opl_types::backup::bitmap_name(i1.id)]);
}

#[tokio::test]
async fn base_of_a_pending_backup_cannot_be_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 4 * MIB).await;
    create_vm(&admin, "web", &["root"]).await;
    succeeded(&store_backup(&env, "web", "st", "f1", false).await);
    let f1 = backup(&env, "f1");
    // 만드는 중인 자식이 f1을 base로 가리킴
    let mut child: Backup = Resource::new(
        "c1",
        BackupSpec {
            store: Some("st".into()),
            ..f1.spec.clone()
        },
    );
    child.status.base = Some(f1.id);
    env.cluster.create(&child).await.unwrap();
    let (status, body) = admin.delete("/backups/f1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "backup_in_use", "{body}");
}

#[tokio::test]
async fn a_commit_after_a_sweep_reuploads_chunks_that_were_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let img = image(&[3, 4]);
    env.hv.set_fleece_images(vm, vec![img.clone()]);
    // 남의 청크(같은 내용)가 이미 저장소에 있다 → 백업은 "있음"으로 건너뛴다.
    let store = open(&env, &st).await;
    let c = store.encode_chunk(&img[..CHUNK]);
    store.put_chunk(&c).await.unwrap();
    // 쓸기가 진행 중일 때 시작한 백업
    let mut s = get_store(&env, "st");
    s.status.sweeping = true;
    env.cluster
        .update_status::<BackupStoreSpec>(s.id, &s.status)
        .await
        .unwrap();
    let env2 = &env;
    let backup = async { store_backup(env2, "web", "st", "b1", false).await };
    let sweep = async {
        // 백업이 커밋 구간을 기다리는 동안 쓸기가 그 청크를 지우고 잠금을 푼다.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        store
            .backend()
            .delete(&opl_store::format::chunk_key(&c.id))
            .await
            .unwrap();
        let mut s = get_store(env2, "st");
        s.status.sweeping = false;
        s.status.sweep_epoch += 1;
        env2.cluster
            .update_status::<BackupStoreSpec>(s.id, &s.status)
            .await
            .unwrap();
    };
    let (t, ()) = tokio::join!(backup, sweep);
    succeeded(&t);
    assert!(
        store.has_chunk(&c.id).await.unwrap(),
        "the removed chunk was uploaded again"
    );
    let stats = backup_of(&env, "b1").status.store_stats.unwrap();
    assert_eq!(stats.new_chunks, 2, "{stats:?}");
    assert_eq!(restore(&env, "b1", "web2").await, img);
}

fn backup_of(env: &Env, name: &str) -> Backup {
    backup(env, name)
}

#[tokio::test]
async fn gc_removes_only_old_unreferenced_chunks_and_records_usage() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    env.hv.set_fleece_images(vm, vec![image(&[5, 6])]);
    succeeded(&store_backup(&env, "web", "st", "b1", false).await);
    env.hv.set_fleece_images(vm, vec![image(&[5, 8])]);
    succeeded(&store_backup(&env, "web", "st", "b2", false).await);
    let store = open(&env, &st).await;
    // 오래된 쓰레기 청크 하나, 막 올라온 쓰레기 청크 하나
    let old = store.encode_chunk(&[9u8; 100]);
    let young = store.encode_chunk(&[10u8; 100]);
    for c in [&old, &young] {
        store.put_chunk(c).await.unwrap();
    }
    let old_path = store_dir(&env, &st).join(opl_store::format::chunk_key(&old.id));
    let f = std::fs::File::options()
        .write(true)
        .open(&old_path)
        .unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    // b1을 지우면 청크 6만 쓰레기가 된다(새로 올라온 것은 아니지만 b1의 시각 그대로 — 오래되게 맞춘다)
    let (status, t) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    let six = store.chunk_id(&vec![6u8; CHUNK]);
    let six_path = store_dir(&env, &st).join(opl_store::format::chunk_key(&six));
    std::fs::File::options()
        .write(true)
        .open(&six_path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        .unwrap();

    // 이 클러스터가 주인인 오래된 하트비트
    let (cluster, fp) = opl_api::stores::this_cluster(&env.cluster).unwrap();
    store
        .write_owner(&opl_store::format::OwnerFile {
            cluster,
            ca_fingerprint: fp,
            store_resource: st.id,
            heartbeat: 1,
        })
        .await
        .unwrap();

    let st_id = get_store(&env, "st").id;
    succeeded(&task(&env, TaskOp::BackupStoreGc, st_id).await);
    assert!(!store.has_chunk(&old.id).await.unwrap());
    assert!(!store.has_chunk(&six).await.unwrap());
    assert!(
        store.has_chunk(&young.id).await.unwrap(),
        "young chunks stay"
    );
    let s = get_store(&env, "st").status;
    assert_eq!(
        (s.gc.clone(), s.sweeping, s.sweep_epoch),
        (None, false, 1),
        "{s:?}"
    );
    let last = s.last_gc.unwrap();
    assert_eq!(
        (last.removed_chunks, last.kept_young, last.error),
        (2, 1, None)
    );
    let usage = s.usage.unwrap();
    assert_eq!(
        (usage.snapshots, usage.logical_bytes),
        (1, 8 * MIB),
        "{usage:?}"
    );
    // 남은 백업은 그대로 복원된다
    assert_eq!(restore(&env, "b2", "web2").await, image(&[5, 8]));
    // 주인 하트비트(아직 이 클러스터가 주인이므로 새로 쓴다)
    assert!(store.read_owner().await.unwrap().unwrap().heartbeat > 1);
}

/// 쓸기 동안 다른 클러스터가 (`--force`로) 넘겨받으면 GC가 끝날 때 주인 표식을 되돌리지 않고 저장소를 `failed`로 멈춘다
/// (리뷰 2 N1 — 앞에는 GC 끝의 하트비트가 넘겨받기를 덮어 두 클러스터가 같은 저장소에 썼다). 가짜 S3를 느리게 하고 한 쪽에
/// 객체 하나씩 나열해 쓸기를 몇 초 걸리게 한다.
#[tokio::test]
async fn a_take_over_during_the_sweep_is_not_undone() {
    let env = Env::start().await;
    let s3 = opl_store::fake_s3::FakeS3::start("bk", "AKID", "secret-key").await;
    let target = s3.target(Some("st"));
    let spec: BackupStoreSpec = serde_json::from_value(json!({
        "backend": {"s3": serde_json::to_value(&target).unwrap()}
    }))
    .unwrap();
    let res: BackupStore = Resource::new("st", spec);
    env.cluster.create(&res).await.unwrap();
    succeeded(&task(&env, TaskOp::BackupStoreInit, res.id).await);
    let backend = Arc::new(opl_store::backend::s3::S3Backend::new(&target, 4).unwrap());
    let store = Store::open(backend, None).await.unwrap();
    for i in 0..12u8 {
        store
            .put_chunk(&store.encode_chunk(&[i; 100]))
            .await
            .unwrap();
    }
    s3.set_page_size(1);
    s3.set_delay(Duration::from_millis(250));
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::BackupStoreGc,
            target: res.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    let phase = || get_store(&env, "st").status.gc.map(|g| g.phase);
    for _ in 0..1500 {
        if phase() == Some(opl_types::backup_store::GcPhase::Sweep) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(phase(), Some(opl_types::backup_store::GcPhase::Sweep));
    // 쓸기 잠금 뒤의 주인 확인은 지났고 쓸기는 아직이다.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(get_store(&env, "st").status.sweeping);
    store
        .write_owner(&opl_store::format::OwnerFile {
            cluster: "dr".into(),
            ca_fingerprint: "sha256:dd".into(),
            store_resource: Uuid::now_v7(),
            heartbeat: 1,
        })
        .await
        .unwrap();
    let done = env.admin().await.wait_task(&json!({"id": t.id})).await;
    s3.set_delay(Duration::ZERO);
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert!(
        done["status"]["error"]
            .as_str()
            .unwrap()
            .contains("taken over by cluster \"dr\""),
        "{done}"
    );
    assert_eq!(store.read_owner().await.unwrap().unwrap().cluster, "dr");
    let s = get_store(&env, "st").status;
    assert_eq!(s.phase, StorePhase::Failed, "{s:?}");
    assert!(!s.sweeping && s.gc.is_none(), "{s:?}");
}

#[tokio::test]
async fn gc_waits_for_backups_that_were_running_and_for_commits() {
    let env = Env::start().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    // 시작 때 pending인 백업(W)과 커밋 구간에 있는 백업
    let spec: BackupSpec = serde_json::from_value(json!({
        "vm": Uuid::now_v7(), "vm_name": "x", "store": "st",
        "vm_spec": {"cpu": {"cores": 1}, "memory_mib": 256, "machine": "virt"}
    }))
    .unwrap();
    let pending: Backup = Resource::new("p1", spec);
    env.cluster.create(&pending).await.unwrap();
    let mut s = get_store(&env, "st");
    s.status.committing = vec![pending.id];
    env.cluster
        .update_status::<BackupStoreSpec>(st.id, &s.status)
        .await
        .unwrap();
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::BackupStoreGc,
            target: st.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    // 기다림 단계에서 멈춘다
    let phase = || get_store(&env, "st").status.gc.map(|g| g.phase);
    for _ in 0..200 {
        if phase() == Some(opl_types::backup_store::GcPhase::Wait) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(phase(), Some(opl_types::backup_store::GcPhase::Wait));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(phase(), Some(opl_types::backup_store::GcPhase::Wait));
    // 백업이 끝남 → 커밋 구간이 비기 전에는 쓸지 않는다
    let mut b = env.cluster.get::<BackupSpec>(pending.id).unwrap().unwrap();
    b.status.phase = BackupPhase::Failed;
    env.cluster
        .update_status::<BackupSpec>(b.id, &b.status)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!get_store(&env, "st").status.sweeping);
    let mut s = get_store(&env, "st");
    s.status.committing.clear();
    env.cluster
        .update_status_checked::<BackupStoreSpec>(st.id, s.revision, &s.status)
        .await
        .unwrap();
    succeeded(&env.admin().await.wait_task(&json!({"id": t.id})).await);
    assert_eq!(get_store(&env, "st").status.sweep_epoch, 1);
}

#[tokio::test]
async fn verify_quarantines_bad_chunks_and_the_next_backup_heals_them() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let img = image(&[11, 12]);
    env.hv.set_fleece_images(vm, vec![img.clone()]);
    succeeded(&store_backup(&env, "web", "st", "b1", false).await);
    let b1 = backup(&env, "b1");
    let st_id = get_store(&env, "st").id;
    let verify = TaskOp::BackupStoreVerify {
        backups: vec![b1.id],
    };
    succeeded(&task(&env, verify.clone(), st_id).await);
    assert_eq!(
        backup(&env, "b1").status.verify.unwrap().state,
        VerifyState::Ok
    );

    // 청크 하나를 망가뜨린다
    let store = open(&env, &st).await;
    let id = store.chunk_id(&img[CHUNK..]);
    let path = store_dir(&env, &st).join(opl_store::format::chunk_key(&id));
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let t = task(&env, verify, st_id).await;
    succeeded(&t);
    assert!(
        t["status"]["error"]
            .as_str()
            .unwrap()
            .contains("failed verification"),
        "{t}"
    );
    let v = backup(&env, "b1").status.verify.unwrap();
    assert_eq!((v.state, v.bad_chunks), (VerifyState::Failed, 1));
    assert!(!path.exists(), "the bad chunk was moved aside");
    let bad: Vec<_> = std::fs::read_dir(store_dir(&env, &st).join("bad"))
        .unwrap()
        .collect();
    assert_eq!(bad.len(), 1);
    let s = get_store(&env, "st").status;
    assert_eq!(s.bad_chunks, 1);
    assert_eq!(s.last_verify.unwrap().bad, 1);
    // 다음 증분 백업은 base가 검증에 실패했으므로 전체로 읽고 그 청크를 다시 올린다
    succeeded(&store_backup(&env, "web", "st", "b2", true).await);
    let b2 = backup(&env, "b2");
    assert_eq!(b2.status.base, None);
    assert_eq!(
        b2.status.full_reason.as_deref(),
        Some("a chunk of the base backup failed verification")
    );
    assert!(path.exists(), "the next backup healed the chunk");
    assert_eq!(restore(&env, "b2", "web2").await, img);
}

/// 검증이 한 백업의 깨진 청크를 `bad/`로 옮기면, 아직 검증하지 않은 다른 백업을 base로 쓰는 증분도 그 청크(base에서 옮긴
/// id)를 원본에서 다시 올린다(리뷰 1 I5) — 망가짐이 새 증분으로 번지지 않는다.
#[tokio::test]
async fn an_incremental_reuploads_base_chunks_that_verify_moved_aside() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let img = image(&[21, 22]);
    env.hv.set_fleece_images(vm, vec![img.clone()]);
    succeeded(&store_backup(&env, "web", "st", "f1", false).await);
    succeeded(&store_backup(&env, "web", "st", "f2", false).await);
    // f1만 검증한다: 두 백업이 함께 쓰는 청크 1이 깨져 `bad/`로 간다(f2는 아직 검증하지 않음).
    let store = open(&env, &st).await;
    let id = store.chunk_id(&img[CHUNK..]);
    let path = store_dir(&env, &st).join(opl_store::format::chunk_key(&id));
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let verify = TaskOp::BackupStoreVerify {
        backups: vec![backup(&env, "f1").id],
    };
    succeeded(&task(&env, verify, get_store(&env, "st").id).await);
    assert!(!path.exists(), "the bad chunk was moved aside");
    assert_eq!(get_store(&env, "st").status.quarantining, None);
    // f2를 base로 한 증분: 청크 0만 바뀌었다 — 청크 1은 base 인덱스에서 옮기지만 `bad/`에 있으므로 다시 올린다.
    let mut next = img.clone();
    next[10..20].fill(9);
    env.hv.set_fleece_images(vm, vec![next.clone()]);
    env.hv.set_fleece_dirty(vm, vec![vec![(10, 10)]]);
    succeeded(&store_backup(&env, "web", "st", "i1", true).await);
    let i1 = backup(&env, "i1");
    assert_eq!(i1.status.base, Some(backup(&env, "f2").id), "{i1:?}");
    assert!(
        path.exists(),
        "the chunk taken from the base was uploaded again"
    );
    assert_eq!(restore(&env, "i1", "web2").await, next);
}

/// 검증이 앞서 놓지 못한 제 옮기기 잠금(`quarantining`)을 다시 쥐고 끝낸다(리뷰 2 N3 — 앞에는 쥔 Task를 적지 않아 제
/// 잠금을 끝없이 기다렸고 그동안 모든 커밋이 막혔다).
#[tokio::test]
async fn verify_takes_back_its_own_quarantine_lock() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 8 * MIB).await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let img = image(&[31, 32]);
    env.hv.set_fleece_images(vm, vec![img.clone()]);
    succeeded(&store_backup(&env, "web", "st", "b1", false).await);
    let store = open(&env, &st).await;
    let id = store.chunk_id(&img[CHUNK..]);
    let path = store_dir(&env, &st).join(opl_store::format::chunk_key(&id));
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::BackupStoreVerify {
                backups: vec![backup(&env, "b1").id],
            },
            target: st.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    // 이 Task가 쥔 채 남은 잠금
    let mut s = get_store(&env, "st");
    s.status.quarantining = Some(t.id);
    env.cluster
        .update_status::<BackupStoreSpec>(s.id, &s.status)
        .await
        .unwrap();
    env.cluster.create(&t).await.unwrap();
    let done = admin.wait_task(&json!({"id": t.id})).await;
    succeeded(&done);
    assert!(!path.exists(), "the bad chunk was moved aside");
    assert_eq!(get_store(&env, "st").status.quarantining, None);
}

#[tokio::test]
async fn an_old_pool_backup_moves_to_the_store() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    seed_store(&env, "st").await;
    let root = create_volume(&env, &admin, "root", 8 * MIB).await;
    qemu_io(&root, "qcow2", "write -P 0x3c 5M 64k");
    create_vm(&admin, "web", &["root"]).await;
    let (status, t) = admin
        .post("/vms/web/backups", json!({"pool": "bk", "name": "old"}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{t}");
    succeeded(&admin.wait_task(&t).await);
    let old = backup(&env, "old");
    let op = TaskOp::BackupConvert { store: "st".into() };
    succeeded(&task(&env, op.clone(), old.id).await);
    let moved = backup(&env, "old-st");
    assert_eq!(moved.spec.store.as_deref(), Some("st"));
    assert_eq!(moved.spec.source.as_ref().unwrap().backup, old.id);
    assert_eq!(moved.status.created_at, old.status.created_at);
    assert_eq!(moved.status.phase, BackupPhase::Ready);
    // 원본은 남는다. 두 번 옮기면 실패한다.
    assert_eq!(backup(&env, "old").status.phase, BackupPhase::Ready);
    let again = task(&env, op, old.id).await;
    assert_eq!(again["status"]["state"], "failed", "{again}");
    assert_eq!(restore(&env, "old-st", "web2").await, raw_of(&root));
}

#[tokio::test]
async fn a_read_only_s3_store_is_scanned_into_backups() {
    let env = Env::start().await;
    let s3 = opl_store::fake_s3::FakeS3::start("bk", "AKID", "secret-key").await;
    let target = s3.target(Some("dr"));
    // 다른 클러스터가 쓴 저장소
    let backend = Arc::new(opl_store::backend::s3::S3Backend::new(&target, 4).unwrap());
    let other = Uuid::now_v7();
    let store = Store::init(backend, other, None, 1).await.unwrap();
    store
        .write_owner(&opl_store::format::OwnerFile {
            cluster: "prod".into(),
            ca_fingerprint: "sha256:00".into(),
            store_resource: other,
            heartbeat: 1,
        })
        .await
        .unwrap();
    let (vm, id) = (Uuid::now_v7(), Uuid::now_v7());
    opl_store::testing::write_backup(&store, vm, id, &[image(&[1, 2])])
        .await
        .unwrap();
    let spec: BackupStoreSpec = serde_json::from_value(json!({
        "backend": {"s3": serde_json::to_value(&target).unwrap()}, "read_only": true
    }))
    .unwrap();
    let res: BackupStore = Resource::new("dr", spec);
    env.cluster.create(&res).await.unwrap();
    succeeded(&task(&env, TaskOp::BackupStoreInit, res.id).await);
    let s = get_store(&env, "dr").status;
    assert_eq!((s.phase, s.store_id), (StorePhase::Ready, Some(other)));
    assert_eq!(s.owner.unwrap().cluster, "prod");
    assert_eq!(s.next_gc, None, "read-only stores are not collected");
    succeeded(&task(&env, TaskOp::BackupStoreScan, res.id).await);
    let b = env.cluster.get::<BackupSpec>(id).unwrap().unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready);
    assert_eq!(b.spec.store.as_deref(), Some("dr"));
    assert_eq!(b.spec.origin.as_ref().unwrap().cluster, "prod");
    // 읽기 전용 저장소의 GC·백업 지우기는 거절
    let gc = task(&env, TaskOp::BackupStoreGc, res.id).await;
    assert_eq!(gc["status"]["state"], "failed", "{gc}");
    let (status, body) = env
        .admin()
        .await
        .delete(&format!("/backups/{}", b.name))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "store_read_only");
    // 저장소에서 사라지면 다시 scan이 failed로 표시
    store.delete_snapshot(vm, id).await.unwrap();
    succeeded(&task(&env, TaskOp::BackupStoreScan, res.id).await);
    let b = env.cluster.get::<BackupSpec>(id).unwrap().unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
}

/// 폴더 안에 있고 복제 대상이 있던 VM의 백업도 scan한 뒤 복원된다(리뷰 1 C1): `config` 블롭은 만든 클러스터의 원래
/// 스펙이고 리소스는 폴더·복제 대상을 버린 스펙이다 — 복원은 리소스의 스펙을 기준으로 한다.
#[tokio::test]
async fn a_scanned_backup_of_a_vm_in_a_folder_restores() {
    let env = Env::start().await;
    create_pool(&env, "local", false).await;
    let s3 = opl_store::fake_s3::FakeS3::start("bk", "AKID", "secret-key").await;
    let target = s3.target(Some("dr"));
    let backend = Arc::new(opl_store::backend::s3::S3Backend::new(&target, 4).unwrap());
    let other = Uuid::now_v7();
    let store = Store::init(backend, other, None, 1).await.unwrap();
    store
        .write_owner(&opl_store::format::OwnerFile {
            cluster: "prod".into(),
            ca_fingerprint: "sha256:00".into(),
            store_resource: other,
            heartbeat: 1,
        })
        .await
        .unwrap();
    let (vm, id) = (Uuid::now_v7(), Uuid::now_v7());
    let config = opl_store::format::manifest::ConfigBlob {
        vm_name: "web".into(),
        vm_spec: serde_json::from_value(json!({
            "cpu": {"cores": 1}, "memory_mib": 256, "machine": "virt",
            "disks": [{"volume": "root", "bus": "virtio-blk"}],
            "folder": Uuid::now_v7(),
            "replication": [{"node": Uuid::now_v7()}]
        }))
        .unwrap(),
        uefi_vars: false,
        tpm_state: false,
    };
    let img = image(&[5, 0, 6]);
    opl_store::testing::write_backup_with(&store, vm, id, std::slice::from_ref(&img), &config, 7)
        .await
        .unwrap();
    let spec: BackupStoreSpec = serde_json::from_value(json!({
        "backend": {"s3": serde_json::to_value(&target).unwrap()}, "read_only": true
    }))
    .unwrap();
    let res: BackupStore = Resource::new("dr", spec);
    env.cluster.create(&res).await.unwrap();
    succeeded(&task(&env, TaskOp::BackupStoreInit, res.id).await);
    succeeded(&task(&env, TaskOp::BackupStoreScan, res.id).await);
    let b = env.cluster.get::<BackupSpec>(id).unwrap().unwrap();
    assert_eq!(b.spec.vm_spec.folder, None);
    assert!(b.spec.vm_spec.replication.is_empty(), "{b:?}");
    assert_eq!(restore(&env, &b.name, "web2").await, img);
}

/// 초기화 Task는 `store.json`·`owner.json`을 쓰고 `ready`·다음 GC 시각을 적는다(로컬은 `chunks/` 디렉터리 65,536개를
/// 만들어 macOS에서 십여 초 걸린다).
#[tokio::test]
async fn init_writes_the_store_and_claims_ownership() {
    let env = Env::start().await;
    create_pool(&env, "bk", true).await;
    let spec: BackupStoreSpec = serde_json::from_value(json!({"backend": {"pool": "bk"}})).unwrap();
    let res: BackupStore = Resource::new("st", spec);
    env.cluster.create(&res).await.unwrap();
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::BackupStoreInit,
            target: res.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    let admin = env.admin().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let (_, got) = admin.get(&format!("/tasks/{}", t.id)).await;
        let state = got["status"]["state"].as_str().unwrap_or("").to_string();
        if state == "succeeded" || state == "failed" {
            succeeded(&got);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let s = get_store(&env, "st");
    assert_eq!(s.status.phase, StorePhase::Ready);
    assert_eq!(s.status.store_id, Some(res.id));
    assert_eq!(s.status.node, Some(env.node));
    assert!(s.status.next_gc.is_some());
    assert_eq!(s.status.owner.as_ref().unwrap().cluster, "test");
    let store = open(&env, &s).await;
    assert_eq!(
        store.read_owner().await.unwrap().unwrap().store_resource,
        res.id
    );
    // 풀에 저장소가 있으면 풀을 지울 수 없다
    let (status, body) = admin.delete("/storage-pools/bk").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");
}

#[tokio::test]
async fn the_scheduler_queues_gc_verify_and_store_jobs() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let st = seed_store(&env, "st").await;
    create_volume(&env, &admin, "root", 4 * MIB).await;
    create_vm(&admin, "web", &["root"]).await;
    // GC·검증 시각이 지났고 새 백업 검증을 켠 저장소, 저장소로 백업하는 작업
    let mut s = get_store(&env, "st");
    s.spec.verify = Some(
        serde_json::from_value(json!({"schedule": {"at": "06:00"}, "verify_new": true})).unwrap(),
    );
    env.cluster
        .update_spec(s.id, Some(s.revision), s.labels.clone(), &s.spec)
        .await
        .unwrap();
    let mut s = get_store(&env, "st");
    s.status.next_gc = Some(1);
    s.status.next_verify = Some(1);
    env.cluster
        .update_status::<BackupStoreSpec>(s.id, &s.status)
        .await
        .unwrap();
    let job: opl_types::backup_job::BackupJob = Resource::new(
        "nightly",
        serde_json::from_value(json!({
            "vms": {"names": ["web"]}, "store": "st", "schedule": {"at": "03:00"}, "keep": {"last": 1}
        }))
        .unwrap(),
    );
    let mut job = job;
    job.status.next_run = Some(1);
    env.cluster.create(&job).await.unwrap();
    let scheduler = opl_api::BackupJobScheduler::spawn(
        env.state.clone(),
        opl_api::BackupJobSchedulerConfig {
            tick: Duration::from_millis(100),
            max_concurrent: 2,
        },
    );
    let ops = || {
        env.cluster
            .list::<TaskSpec>()
            .unwrap()
            .1
            .into_iter()
            .filter(|t| t.status.state.is_finished())
            .map(|t| t.spec.op)
            .collect::<Vec<_>>()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let done = ops();
        let gc = done.contains(&TaskOp::BackupStoreGc);
        let verify_new = done
            .iter()
            .any(|o| matches!(o, TaskOp::BackupStoreVerify { backups } if !backups.is_empty()));
        let backed_up = done.iter().any(|o| matches!(o, TaskOp::VmBackup { .. }));
        if gc && verify_new && backed_up {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{done:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    scheduler.shutdown().await;
    let s = get_store(&env, "st").status;
    assert!(s.next_gc.unwrap() > 1 && s.next_verify.unwrap() > 1);
    let b: Vec<Backup> = env.cluster.list::<BackupSpec>().unwrap().1;
    let made = b.iter().find(|b| b.spec.job == Some(job.id)).unwrap();
    assert_eq!(made.spec.store.as_deref(), Some("st"));
    assert_eq!(made.status.verify.as_ref().unwrap().state, VerifyState::Ok);
    let _ = st;
}
