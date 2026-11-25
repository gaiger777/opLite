//! 계획 72 스테이지 4: VMware 전체 복사와 CBT 델타 동기화를 가짜 vSphere(기능 `fake-vsphere`)와 가짜 스토리지(같은 프로세스
//! NBD 서버 — qemu-img·qemu-nbd 없음)로 본다 — 따뜻한 회차와 게스트 쓰기(대상 = 회차 스냅샷 시점의 원본 바이트, 바뀐 블록만
//! 읽음), CBT 초기화 → 그 디스크만 전체, 스냅샷 지우기 실패 → 두 스냅샷 → 다시 하기가 정리, opld 다시 시작 → 체크포인트부터
//! (노드 시작 sweep이 대상 볼륨을 지우지 않음), 사용자 스냅샷 → 회차 건너뜀, 차가운 이전, 무료 ESXi, 항목 취소(정리 Task), 스케줄러
//! (전체 → 주기 델타 → 계획 취소 → 정리). 모든 시험 끝에 원본 디스크와 사용자 스냅샷은 그대로다.
//! `cargo test -p orbivisor-api --features fake-vsphere --test vmware_sync`
//! 실제 `qemu-nbd` 쓰기 길은 `--features fake-vsphere,storage-it`(qemu-img·qemu-nbd 필요).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Client, Env, FakeStorage};
use orbivisor_api::vmware::fake::{BLOCK, FakeConfig, FakeVsphere};
use orbivisor_api::{VmwareScheduler, VmwareSchedulerConfig};
use orbivisor_types::resource::Resource;
use orbivisor_types::storage::{VolumePhase, VolumeSpec};
use orbivisor_types::task::{TaskOp, TaskSpec};
use orbivisor_types::vmware::{
    VMWARE_VM_LABEL, VmwareSourceSpec, VmwareVm, VmwareVmSpec, VmwareVmState,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 원본 연결 `vc1`과 풀 `local`(가짜 스토리지 — 체크포인트 64 KiB, 멈춤 한도 5초)
async fn setup() -> (Env, Client, FakeVsphere, Arc<FakeStorage>) {
    setup_with(FakeConfig::default()).await
}

async fn setup_with(config: FakeConfig) -> (Env, Client, FakeVsphere, Arc<FakeStorage>) {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let fake = FakeVsphere::start(config).await;
    let spec = fake.source_spec();
    let (status, body) = admin
        .post(
            "/vmware-sources",
            json!({
                "name": "vc1", "host": spec.host, "port": spec.port, "user": spec.user,
                "password": spec.password.expose(), "fingerprint": spec.fingerprint,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let dir = env.dir.path().join("local");
    std::fs::create_dir_all(&dir).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": dir } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let storage = Arc::new(FakeStorage::default());
    storage.enable_sources();
    env.stop_runner().await;
    start_runner(&mut env, &storage).await;
    (env, admin, fake, storage)
}

async fn start_runner(env: &mut Env, storage: &Arc<FakeStorage>) {
    env.start_runner_config(storage.clone(), |c| {
        c.vmware_checkpoint_bytes = 64 << 10;
        c.vmware_stall = Duration::from_secs(5);
    })
    .await;
}

/// 계획 `name`(VM 하나 `vm`, `mode`)을 만들고 시작한다. 항목을 돌려준다.
async fn start_plan(env: &Env, admin: &Client, name: &str, vm: &str, mode: &str) -> VmwareVm {
    let (status, body) = admin
        .post(
            "/vmware-migrations",
            json!({
                "name": name, "source": "vc1", "mode": mode,
                "rules": {
                    "networks": [
                        { "from": "VM Network", "to": "user" },
                        { "from": "dvpg-web", "to": "user" },
                    ],
                    "default_pool": "local",
                    "start_after": false,
                },
                "limits": { "streams_per_disk": 2 },
                "vms": [{ "ref": vm, "overrides": { "disk_bus": "virtio-blk", "nic_model": "virtio" } }],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            &format!("/vmware-migrations/{name}/actions/start"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id: Uuid = body["vms"][0]["id"].as_str().unwrap().parse().unwrap();
    let item = item(env, id);
    assert_eq!(
        item.status.state,
        VmwareVmState::Pending,
        "{:?}",
        item.status
    );
    item
}

fn item(env: &Env, id: Uuid) -> VmwareVm {
    env.cluster.get::<VmwareVmSpec>(id).unwrap().unwrap()
}

/// 항목에 Task를 직접 만들고(스케줄러 없이) 끝날 때까지 기다린다.
async fn run(env: &Env, admin: &Client, item: Uuid, op: TaskOp) -> Value {
    let mut t: orbivisor_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op,
            target: item,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    admin.wait_task(&json!({ "id": t.id })).await
}

async fn sync(env: &Env, admin: &Client, item: Uuid, full: bool) -> Value {
    run(env, admin, item, TaskOp::VmwareSync { full }).await
}

fn ok(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 대상 볼륨 내용 = 원본 기반(회차 스냅샷 시점)
fn same_as_base(env: &Env, storage: &FakeStorage, fake: &FakeVsphere, vm: &str, id: Uuid) {
    let it = item(env, id);
    for (i, d) in it.status.disks.iter().enumerate() {
        let got = storage.contents(d.volume.unwrap()).unwrap();
        assert!(got == fake.base_bytes(vm, i), "disk {i} differs");
    }
}

/// flat 파일 `/folder` GET이 읽은 바이트 합(Range 머리에서 — 읽개를 열 때 읽는 디스크립터는 빼고)
fn read_bytes(fake: &FakeVsphere) -> u64 {
    fake.requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path.contains("-flat.vmdk"))
        .filter_map(|r| r.range.as_deref())
        .filter_map(|r| {
            let (a, b) = r.strip_prefix("bytes=")?.split_once('-')?;
            Some(b.parse::<u64>().ok()? - a.parse::<u64>().ok()? + 1)
        })
        .sum()
}

/// 따뜻한 이전: 첫 회차는 할당된 구역 전체(씬 디스크의 빈 곳은 읽지 않음), 다음 회차는 바뀐 블록만 읽고, 대상은 늘 회차
/// 스냅샷 시점의 원본 바이트다. 우리 스냅샷은 늘 하나이고, 볼륨은 레이블을 단 `pending`으로 남는다.
#[tokio::test]
async fn warm_rounds_copy_changed_blocks_and_match_the_source() {
    let (env, admin, fake, storage) = setup().await;
    let it = start_plan(&env, &admin, "w1", "DC1/vm/Prod/web01", "warm").await;
    let before = fake.disk_bytes("web01", 0);
    fake.clear_requests();
    ok(&sync(&env, &admin, it.id, true).await);
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Syncing, "{s:?}");
    assert_eq!(s.rounds, 1);
    assert!(s.cbt_enabled_by_us && fake.cbt_enabled("web01"));
    assert!(s.next_sync_at.is_some() && s.started_at.is_some());
    assert_eq!(s.disks.len(), 2);
    assert!(
        s.disks
            .iter()
            .all(|d| d.change_id.is_some() && d.rounds == 1)
    );
    let snaps = fake.snapshots("web01");
    assert_eq!(snaps.len(), 1);
    assert_eq!(s.snapshot.as_ref().unwrap().moref, snaps[0].0);
    // 할당된 구역(사이가 256 KiB 이하면 이어 읽는다 — 가짜 디스크의 빈 블록은 하나씩이라 모두 이어진다)
    let full: u64 = s.disks.iter().map(|d| d.capacity_bytes).sum();
    let first = read_bytes(&fake);
    assert!(first > 0 && first <= full, "read {first} of {full}");
    same_as_base(&env, &storage, &fake, "web01", it.id);
    for d in &s.disks {
        let v = env
            .cluster
            .get::<VolumeSpec>(d.volume.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(v.status.phase, VolumePhase::Pending);
        assert_eq!(v.labels[VMWARE_VM_LABEL], it.id.to_string());
        assert_eq!(v.spec.pool, "local");
    }
    assert!(v_names(&env).contains(&"web01-disk0".to_string()));

    // 게스트 쓰기 → 델타 둘: 바뀐 블록만, 대상 = 새 스냅샷 시점
    for round in 2..=3u32 {
        fake.write(
            "web01",
            0,
            u64::from(round) * BLOCK + 7,
            &[round as u8; 100],
        );
        fake.write("web01", 1, 5, b"delta");
        fake.clear_requests();
        ok(&sync(&env, &admin, it.id, false).await);
        let s = item(&env, it.id).status;
        assert_eq!(s.rounds, round);
        let read = read_bytes(&fake);
        assert!(read > 0 && read <= 2 * BLOCK, "round {round} read {read}");
        // vCenter: 호스트로 티켓을 들고 바로
        assert!(
            fake.requests()
                .iter()
                .filter(|r| r.method == "GET")
                .all(|g| g.server > 0 && g.ticket.is_some())
        );
        assert_eq!(fake.snapshots("web01").len(), 1);
        assert!(s.estimate_s.is_some());
        same_as_base(&env, &storage, &fake, "web01", it.id);
    }
    // 원본: 게스트가 보는 내용은 우리 쓰기 없이 게스트 쓰기만
    let mut want = before;
    for round in 2..=3u64 {
        let at = (round * BLOCK + 7) as usize;
        want[at..at + 100].fill(round as u8);
    }
    assert!(fake.disk_bytes("web01", 0) == want);
}

fn v_names(env: &Env) -> Vec<String> {
    env.cluster
        .list::<VolumeSpec>()
        .unwrap()
        .1
        .into_iter()
        .map(|v| v.name)
        .collect()
}

/// CBT 초기화 → 그 VM의 디스크를 `"*"`로 다시(경고), 스냅샷 지우기 실패 → `failed`와 두 스냅샷, 다시 하기가 옛 것을 먼저
/// 지우고 잇는다. 사용자 스냅샷이 생기면 회차를 건너뛰고 그대로 둔다.
#[tokio::test]
async fn cbt_reset_snapshot_failures_and_user_snapshots() {
    let (env, admin, fake, storage) = setup().await;
    let it = start_plan(&env, &admin, "w2", "DC1/vm/Prod/web01", "warm").await;
    ok(&sync(&env, &admin, it.id, true).await);

    fake.reset_cbt("web01");
    fake.write("web01", 0, 9, b"after reset");
    let done = sync(&env, &admin, it.id, false).await;
    ok(&done);
    assert!(
        done["status"]["error"]
            .as_str()
            .unwrap()
            .contains("changed block tracking was reset on disk 0"),
        "{done}"
    );
    same_as_base(&env, &storage, &fake, "web01", it.id);

    fake.fail_next("RemoveSnapshot_Task", "GenericVmConfigFault");
    let failed = sync(&env, &admin, it.id, false).await;
    assert_eq!(failed["status"]["state"], "failed", "{failed}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Failed);
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("keeps two Orbivisor snapshots"),
        "{s:?}"
    );
    assert!(s.pending_snapshot.is_some());
    assert_eq!(fake.snapshots("web01").len(), 2);
    // 항목 취소: 정리 Task가 두 스냅샷·CBT·대상 볼륨을 치운다. 취소한 항목은 다시 할 수 없다(409).
    let (status, body) = admin.post("/vmware-vms/w2-web01/cancel", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cleanup = admin.wait_task(&json!({ "id": body["task"] })).await;
    ok(&cleanup);
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Cancelled);
    assert!(fake.snapshots("web01").is_empty());
    assert!(!fake.cbt_enabled("web01"));
    assert!(s.disks.iter().all(|d| d.volume.is_none()));
    assert!(!v_names(&env).iter().any(|n| n.starts_with("web01-disk")));
    let (status, body) = admin.post("/vmware-vms/w2-web01/retry", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 다시: 새 계획, 회차 하나 → 지우기 실패 → 다시 하기가 두 스냅샷을 정리한다.
    let it = start_plan(&env, &admin, "w3", "DC1/vm/Prod/web01", "warm").await;
    ok(&sync(&env, &admin, it.id, true).await);
    fake.fail_next("RemoveSnapshot_Task", "GenericVmConfigFault");
    let failed = sync(&env, &admin, it.id, false).await;
    assert_eq!(failed["status"]["state"], "failed", "{failed}");
    assert_eq!(fake.snapshots("web01").len(), 2);
    let (status, body) = admin.post("/vmware-vms/w3-web01/retry", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["status"]["state"], "syncing");
    ok(&sync(&env, &admin, it.id, false).await);
    let s = item(&env, it.id).status;
    assert_eq!((s.rounds, s.sync_failures), (2, 0));
    assert!(s.pending_snapshot.is_none() && s.error.is_none());
    assert_eq!(fake.snapshots("web01").len(), 1);
    same_as_base(&env, &storage, &fake, "web01", it.id);

    // 사용자 스냅샷: 회차를 건너뛰고(경고) 손대지 않는다.
    let user = fake.add_user_snapshot("web01", "backup-job");
    let skipped = sync(&env, &admin, it.id, false).await;
    ok(&skipped);
    assert!(
        skipped["status"]["error"]
            .as_str()
            .unwrap()
            .contains("not Orbivisor's (backup-job)"),
        "{skipped}"
    );
    let s = item(&env, it.id).status;
    assert_eq!(s.rounds, 2);
    assert!(s.next_sync_at.unwrap() > opl_api_now());
    assert!(fake.snapshots("web01").iter().any(|(m, _)| *m == user));
}

fn opl_api_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// opld 다시 시작: 복사 중 실행기를 멈추고 다시 띄우면 Task가 체크포인트부터 잇는다(처음 구역을 다시 읽지 않는다). 노드 시작
/// sweep은 살아 있는 항목의 `pending` 볼륨을 지우지 않는다. 범위 읽기가 끊기면(다시 해도 안 될 만큼) 회차가 실패하고 항목은
/// `copying`으로 남아 다시 한다.
#[tokio::test]
async fn a_restart_resumes_from_the_checkpoint_and_keeps_the_volumes() {
    // 디스크 0은 16 MiB(4 MiB 조각 넷 — 조각마다 체크포인트)
    let mut config = FakeConfig::default();
    config.vms[0].disks = vec![16 << 20, 4 << 20];
    let (mut env, admin, fake, storage) = setup_with(config).await;
    let it = start_plan(&env, &admin, "w4", "DC1/vm/Prod/web01", "warm").await;
    // 끊긴 읽기: 세 번 다시 해도 끊기면 실패 → 항목은 copying, 실패 1
    fake.cut_reads(1000);
    let failed = sync(&env, &admin, it.id, true).await;
    assert_eq!(failed["status"]["state"], "failed", "{failed}");
    let s = item(&env, it.id).status;
    assert_eq!((s.state, s.sync_failures), (VmwareVmState::Copying, 1));
    fake.cut_reads(0);

    // 느린 원본에서 복사하다 멈춘다.
    fake.set_delay(Duration::from_millis(30));
    let mut t: orbivisor_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmwareSync { full: true },
            target: it.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let checkpoint = loop {
        let s = item(&env, it.id).status;
        if let Some(d) = s.disks.first()
            && d.copied_bytes > 0
        {
            break d.copied_bytes;
        }
        assert!(Instant::now() < deadline, "no checkpoint: {s:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    env.stop_runner().await;
    let volumes: Vec<Uuid> = item(&env, it.id)
        .status
        .disks
        .iter()
        .filter_map(|d| d.volume)
        .collect();
    fake.set_delay(Duration::ZERO);
    fake.clear_requests();
    start_runner(&mut env, &storage).await;
    let done = admin.wait_task(&json!({ "id": t.id })).await;
    ok(&done);
    // 시작 sweep이 대상 볼륨을 남겼다.
    assert!(volumes.iter().all(|v| !storage.deleted().contains(v)));
    // 디스크 0은 체크포인트 앞을 다시 읽지 않았다.
    let disk0 = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "GET" && r.path.contains("web01-flat.vmdk"))
        .filter_map(|r| r.range)
        .filter_map(|r| {
            r.strip_prefix("bytes=")?
                .split_once('-')?
                .0
                .parse::<u64>()
                .ok()
        })
        .min()
        .unwrap();
    assert!(
        disk0 >= checkpoint && checkpoint >= 4 << 20,
        "re-read from {disk0} (checkpoint {checkpoint})"
    );
    let s = item(&env, it.id).status;
    assert_eq!(
        (s.state, s.rounds, s.sync_failures),
        (VmwareVmState::Syncing, 1, 0)
    );
    same_as_base(&env, &storage, &fake, "web01", it.id);
}

/// 차가운 이전(꺼진 VM): 스냅샷·CBT 없이 기반 전체를 옮기고(0 조각은 0으로), flat 파일의 수정 시각을 적고 `ready`다. 켜진 VM의
/// 차가운 동기화는 곧 `failed`다.
#[tokio::test]
async fn cold_copies_a_powered_off_vm_without_snapshots() {
    let (env, admin, fake, storage) = setup().await;
    let it = start_plan(&env, &admin, "c1", "DC1/vm/db01", "cold").await;
    ok(&sync(&env, &admin, it.id, true).await);
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Ready, "{s:?}");
    assert!(s.snapshot.is_none() && !s.cbt_enabled_by_us);
    assert!(s.disks[0].source_mtime.is_some(), "{s:?}");
    assert!(fake.snapshots("db01").is_empty() && !fake.cbt_enabled("db01"));
    let got = storage.contents(s.disks[0].volume.unwrap()).unwrap();
    assert!(got == fake.disk_bytes("db01", 0));

    // 켜진 VM: 곧 실패(다시 해도 같다)
    let item_id = it.id;
    let (status, body) = admin.post("/vmware-vms/c1-db01/cancel", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    ok(&admin.wait_task(&json!({ "id": body["task"] })).await);
    assert_eq!(item(&env, item_id).status.state, VmwareVmState::Cancelled);
    let it = start_plan(&env, &admin, "c2", "DC1/vm/db01", "cold").await;
    fake.set_power("db01", orbivisor_types::vmware::VmwarePower::PoweredOn);
    let failed = sync(&env, &admin, it.id, true).await;
    assert_eq!(failed["status"]["state"], "failed", "{failed}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Failed);
    assert!(s.error.unwrap().contains("vmware_powered_on"));
}

/// 무료 ESXi: 스냅샷이 거절되면(`RestrictedVersion`) 항목은 곧 `failed`이고 원본 연결에 `restricted`를 적는다.
#[tokio::test]
async fn a_restricted_host_fails_the_item_and_marks_the_source() {
    let (env, admin, fake, _storage) = setup().await;
    let it = start_plan(&env, &admin, "r1", "DC1/vm/Prod/web01", "warm").await;
    fake.set_restricted(true);
    let failed = sync(&env, &admin, it.id, true).await;
    assert_eq!(failed["status"]["state"], "failed", "{failed}");
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Failed);
    let source = env
        .cluster
        .get_by_name::<VmwareSourceSpec>("vc1")
        .unwrap()
        .unwrap();
    assert!(source.status.restricted);
    assert!(fake.snapshots("web01").is_empty());
}

/// 스케줄러: 시작한 계획의 `pending` 항목에 전체 복사 Task를 만들고, 주기가 지나면 델타를, 계획을 취소하면 정리 Task를 만든다.
#[tokio::test]
async fn the_scheduler_copies_syncs_and_cleans_up_cancelled_plans() {
    let (env, admin, fake, storage) = setup().await;
    let scheduler = VmwareScheduler::spawn(
        env.state.clone(),
        VmwareSchedulerConfig {
            tick: Duration::from_millis(100),
        },
    );
    let it = start_plan(&env, &admin, "s1", "DC1/vm/Prod/web01", "warm").await;
    let s = wait_item(&env, it.id, |s| s.rounds >= 1).await;
    assert_eq!(s.state, VmwareVmState::Syncing);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let m = admin.get("/vmware-migrations/s1").await.1;
        if m["status"]["counts"]["syncing"] == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "{m}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 다음 주기를 앞당긴다 → 델타
    fake.write("web01", 0, 3, b"tick");
    let mut cur = item(&env, it.id);
    cur.status.next_sync_at = Some(1);
    env.cluster
        .update_status::<VmwareVmSpec>(it.id, &cur.status)
        .await
        .unwrap();
    wait_item(&env, it.id, |s| s.rounds >= 2).await;
    same_as_base(&env, &storage, &fake, "web01", it.id);
    // 계획 취소 → 정리
    let (status, body) = admin
        .post("/vmware-migrations/s1/actions/cancel", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let s = wait_item(&env, it.id, |s| s.state == VmwareVmState::Cancelled).await;
    assert!(s.snapshot.is_none() && !s.cbt_enabled_by_us);
    assert!(fake.snapshots("web01").is_empty() && !fake.cbt_enabled("web01"));
    let tasks = env.cluster.list::<TaskSpec>().unwrap().1;
    let ops: Vec<&TaskOp> = tasks
        .iter()
        .filter(|t| t.spec.target == it.id)
        .map(|t| &t.spec.op)
        .collect();
    assert!(ops.contains(&&TaskOp::VmwareSync { full: true }), "{ops:?}");
    assert!(
        ops.contains(&&TaskOp::VmwareSync { full: false }),
        "{ops:?}"
    );
    assert!(
        ops.contains(&&TaskOp::VmwareCleanup {
            delete_target: true
        }),
        "{ops:?}"
    );
    scheduler.shutdown().await;
}

async fn wait_item(
    env: &Env,
    id: Uuid,
    done: impl Fn(&orbivisor_types::vmware::VmwareVmStatus) -> bool,
) -> orbivisor_types::vmware::VmwareVmStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = item(env, id).status;
        if done(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "item did not get there: {s:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 실제 `qemu-nbd` 쓰기 길(디렉터리 풀 qcow2): 전체 → 델타 뒤 볼륨 파일(`qemu-img convert -O raw`) = 원본 기반 바이트.
#[cfg(feature = "storage-it")]
#[tokio::test]
async fn qemu_nbd_writes_the_target_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let spec = fake.source_spec();
    let (status, body) = admin
        .post(
            "/vmware-sources",
            json!({
                "name": "vc1", "host": spec.host, "port": spec.port, "user": spec.user,
                "password": spec.password.expose(), "fingerprint": spec.fingerprint,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let dir = env.dir.path().join("local");
    std::fs::create_dir_all(&dir).unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": dir } } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let it = start_plan(&env, &admin, "q1", "DC1/vm/Prod/web01", "warm").await;
    ok(&sync(&env, &admin, it.id, true).await);
    fake.write("web01", 1, 2 * BLOCK + 1, &[0xAB; 5000]);
    // 0으로 덮인 블록(델타가 0 조각을 `WRITE_ZEROES`로)
    fake.write("web01", 0, 0, &vec![0; BLOCK as usize]);
    ok(&sync(&env, &admin, it.id, false).await);
    let s = item(&env, it.id).status;
    for (i, d) in s.disks.iter().enumerate() {
        let v = env
            .cluster
            .get::<VolumeSpec>(d.volume.unwrap())
            .unwrap()
            .unwrap();
        let file = dir.join(format!("{}.qcow2", v.id));
        let raw = env.dir.path().join(format!("disk{i}.raw"));
        let out = std::process::Command::new("qemu-img")
            .args(["convert", "-O", "raw"])
            .arg(&file)
            .arg(&raw)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            std::fs::read(&raw).unwrap() == fake.base_bytes("web01", i),
            "disk {i}"
        );
    }
}

/// 잰 것(§18): 256 MiB 디스크(블록 셋 중 둘 할당) 하나의 전체 복사와 1 MiB 델타의 속도 — 루프백·가짜 스토리지·디버그 빌드.
/// `cargo test -p orbivisor-api --features fake-vsphere --test vmware_sync -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn measure_copy_speed() {
    let mut config = FakeConfig::default();
    config.vms[0].disks = vec![256 << 20];
    let (mut env, admin, fake, storage) = setup_with(config).await;
    // 체크포인트는 기본(512 MiB·30초)
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |_| {}).await;
    let it = start_plan(&env, &admin, "m1", "DC1/vm/Prod/web01", "warm").await;
    let t = Instant::now();
    ok(&sync(&env, &admin, it.id, true).await);
    let full = t.elapsed();
    let copied = read_bytes(&fake);
    for i in 0..16u64 {
        fake.write("web01", 0, i * 16 * BLOCK, &vec![7u8; BLOCK as usize]);
    }
    fake.clear_requests();
    let t = Instant::now();
    ok(&sync(&env, &admin, it.id, false).await);
    let delta = t.elapsed();
    let s = item(&env, it.id).status;
    println!(
        "full copy: {} MiB in {:.2}s ({:.0} MiB/s); delta: {} KiB in {:.2}s; estimate_s {:?}",
        copied >> 20,
        full.as_secs_f64(),
        (copied >> 20) as f64 / full.as_secs_f64(),
        read_bytes(&fake) >> 10,
        delta.as_secs_f64(),
        s.estimate_s
    );
}
