//! 계획 72 스테이지 5: VMware 전환·되돌리기·마무리를 가짜 vSphere(기능 `fake-vsphere`)와 가짜 스토리지(qemu-img·qemu-nbd
//! 없음)로 본다 — 따뜻한 전환(켜진 채 델타 → 원본 보호·게스트 종료 → 마지막 델타 → VM 만들기·켜기, 대상 = 원본 마지막
//! 바이트, 내린 시간), 되돌리기(이름 확인 400, 원본 다시 켬·설명·NIC 되돌림), 마무리(CBT 끔·보호는 남김), 차가운 전환, 걸음마다
//! 실패 주입 → 원본이 다시 켜지고 NIC·설명이 되돌려지고 대상 VM이 없다, 게스트 종료 시간 넘김 → 원본 켜진 채, 할당량 넘음 →
//! 되돌림, 되돌림 실패 → 손으로 할 일과 다시 하기, 예약 전환. **모든 시험 끝에** 원본 디스크 바이트(게스트 쓰기만)와 사용자
//! 스냅샷이 그대로다.
//! `cargo test -p opl-api --features fake-vsphere --test vmware_cutover`

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Client, Env, FakeStorage, vm_request};
use opl_api::vmware::fake::{BLOCK, FakeConfig, FakeVsphere};
use opl_api::vmware::soap::MoRef;
use opl_api::{VmwareScheduler, VmwareSchedulerConfig};
use opl_driver::DriverError;
use opl_types::audit::AuditQuery;
use opl_types::resource::Resource;
use opl_types::storage::{VolumePhase, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{VmPhase, VmSpec};
use opl_types::vmware::{
    SNAPSHOT_PREFIX, VmwarePower, VmwareVm, VmwareVmSpec, VmwareVmState, VmwareVmStatus,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const NOTE: &str = "owner: ops team";

/// 원본 VM의 기대 내용: 시작 때 바이트와 사용자 스냅샷, 그 뒤의 게스트 쓰기만. [`Source::check`]가 원본이 우리 손을 타지
/// 않았는지 본다(디스크 바이트 = 시작 + 게스트 쓰기, 사용자 스냅샷이 모두 남음).
struct Source {
    vm: &'static str,
    disks: Vec<Vec<u8>>,
    user_snapshots: Vec<(String, String)>,
}

impl Source {
    fn new(fake: &FakeVsphere, vm: &'static str, disks: usize) -> Self {
        Source {
            vm,
            disks: (0..disks).map(|i| fake.disk_bytes(vm, i)).collect(),
            user_snapshots: user_snapshots(fake, vm),
        }
    }

    /// 게스트 쓰기(원본과 기대 내용 둘 다)
    fn write(&mut self, fake: &FakeVsphere, disk: usize, offset: u64, data: &[u8]) {
        fake.write(self.vm, disk, offset, data);
        let at = offset as usize;
        self.disks[disk][at..at + data.len()].copy_from_slice(data);
    }

    fn check(&self, fake: &FakeVsphere) {
        for (i, want) in self.disks.iter().enumerate() {
            assert!(
                fake.disk_bytes(self.vm, i) == *want,
                "source disk {i} of {} was changed",
                self.vm
            );
        }
        let now = user_snapshots(fake, self.vm);
        for s in &self.user_snapshots {
            assert!(now.contains(s), "user snapshot {s:?} was removed");
        }
    }
}

fn user_snapshots(fake: &FakeVsphere, vm: &str) -> Vec<(String, String)> {
    fake.snapshots(vm)
        .into_iter()
        .filter(|(_, name)| !name.starts_with(SNAPSHOT_PREFIX))
        .collect()
}

async fn setup() -> (Env, Client, FakeVsphere, Arc<FakeStorage>) {
    let mut env = Env::start().await;
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
    let storage = Arc::new(FakeStorage::default());
    storage.enable_sources();
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.vmware_checkpoint_bytes = 64 << 10;
        c.vmware_stall = Duration::from_secs(5);
        c.vmware_start_timeout = Duration::from_secs(10);
        c.vmware_shutdown_limit = Some(Duration::from_secs(1));
    })
    .await;
    // 원본 설명(되돌림이 그대로 되살려야 한다)
    let session = fake.login().await;
    for vm in ["web01", "db01"] {
        session
            .set_annotation(&MoRef::new("VirtualMachine", &fake.moref(vm)), NOTE)
            .await
            .unwrap();
    }
    (env, admin, fake, storage)
}

/// 계획 `name`을 만들고 시작한다(`extra`는 요청에 덧붙임). 항목을 돌려준다.
async fn start_plan(
    env: &Env,
    admin: &Client,
    name: &str,
    vm: &str,
    mode: &str,
    extra: Value,
) -> VmwareVm {
    let mut req = json!({
        "name": name, "source": "vc1", "mode": mode,
        "rules": {
            "networks": [
                { "from": "VM Network", "to": "user" },
                { "from": "dvpg-web", "to": "user" },
            ],
            "default_pool": "local",
        },
        "limits": { "streams_per_disk": 2 },
        "vms": [{ "ref": vm, "overrides": { "disk_bus": "virtio-blk", "nic_model": "virtio" } }],
    });
    for (k, v) in extra.as_object().unwrap() {
        if k == "rules" {
            for (rk, rv) in v.as_object().unwrap() {
                req["rules"][rk] = rv.clone();
            }
        } else {
            req[k] = v.clone();
        }
    }
    let (status, body) = admin.post("/vmware-migrations", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            &format!("/vmware-migrations/{name}/actions/start"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id: Uuid = body["vms"][0]["id"].as_str().unwrap().parse().unwrap();
    let it = item(env, id);
    assert_eq!(it.status.state, VmwareVmState::Pending, "{:?}", it.status);
    it
}

fn item(env: &Env, id: Uuid) -> VmwareVm {
    env.cluster.get::<VmwareVmSpec>(id).unwrap().unwrap()
}

async fn sync(env: &Env, admin: &Client, item: Uuid, full: bool) {
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmwareSync { full },
            target: item,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    let done = admin.wait_task(&json!({ "id": t.id })).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
}

/// 항목 동작을 보내고 그 Task가 끝날 때까지 기다린다(Task를 돌려준다).
async fn act(admin: &Client, item: &str, action: &str, body: Value) -> Value {
    let (status, got) = admin
        .post(&format!("/vmware-vms/{item}/{action}"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{action}: {got}");
    admin.wait_task(&json!({ "id": got["task"] })).await
}

fn vm_named(env: &Env, name: &str) -> Option<opl_types::vm::Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

/// 되돌린 원본: 켜짐, NIC가 켤 때 연결, 설명 그대로, 대상 VM 없음, 대상 볼륨은 `pending`으로 남음
fn assert_source_restored(env: &Env, fake: &FakeVsphere, vm: &str, s: &VmwareVmStatus) {
    assert_eq!(fake.power(vm), VmwarePower::PoweredOn, "{s:?}");
    assert!(fake.nics_start_connected(vm).iter().all(|c| *c), "{s:?}");
    assert_eq!(fake.annotation(vm), NOTE);
    assert!(s.source_changes.is_empty(), "{s:?}");
    assert!(s.target_vm.is_none() && s.cutover_step.is_none(), "{s:?}");
    assert!(vm_named(env, vm).is_none(), "the target VM is left");
    for d in &s.disks {
        let v = env
            .cluster
            .get::<VolumeSpec>(d.volume.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(v.status.phase, VolumePhase::Pending);
    }
}

/// 따뜻한 전환: 켜진 채 델타 → 원본 보호·게스트 종료 → 마지막 델타 → VM 만들기·켜기. 대상 볼륨 = 원본의 마지막 바이트(끄기
/// 바로 전의 쓰기까지), 원본은 꺼지고 NIC는 켤 때 끊김·설명이 달리고 우리 스냅샷은 없다. 내린 시간이 적히고 감사에 남는다.
/// 되돌리기는 이름이 틀리면 400, 맞으면 대상 VM을 끄고(남김) 원본을 다시 켜고 설명·NIC를 되돌린다.
#[tokio::test]
async fn warm_cutover_then_revert() {
    let (env, admin, fake, storage) = setup().await;
    let mut src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "w1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    src.write(&fake, 0, 3 * BLOCK + 9, &[0x5A; 300]);
    sync(&env, &admin, it.id, false).await;
    // 마지막 동기화 뒤의 쓰기도 옮겨진다(켜진 채 델타 + 꺼진 뒤 마지막 델타).
    src.write(&fake, 1, 11, b"just before the cutover");

    // 전환할 수 없는 상태·복사 전 항목
    let (status, body) = admin.post("/vmware-vms/w1-web01/finalize", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vmware_vm_state");

    let task = act(&admin, "w1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    assert!(s.cutover_step.is_none() && s.downtime_s.is_some(), "{s:?}");
    assert!(s.cutover_requested_at.is_some());
    assert!(
        s.history
            .iter()
            .any(|h| h.event.starts_with("cut over; downtime"))
    );
    let vm = vm_named(&env, "web01").expect("target VM");
    assert_eq!(Some(vm.id), s.target_vm);
    assert_eq!(vm.status.phase, VmPhase::Running);
    for (i, d) in s.disks.iter().enumerate() {
        let v = env
            .cluster
            .get::<VolumeSpec>(d.volume.unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(v.status.phase, VolumePhase::Ready);
        assert!(
            storage.contents(v.id).unwrap() == src.disks[i],
            "target disk {i} differs from the source"
        );
    }
    // 원본: 꺼짐, NIC 둘 다 켤 때 끊김, 설명(옛 설명은 뒤에), 우리 스냅샷 없음, CBT는 마무리 때까지 켜 둠
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    assert_eq!(fake.nics_start_connected("web01"), vec![false, false]);
    let note = fake.annotation("web01");
    assert!(
        note.starts_with("Migrated to Orbivisor VM \"web01\""),
        "{note}"
    );
    assert!(note.ends_with(NOTE), "{note}");
    assert!(fake.snapshots("web01").is_empty());
    assert!(s.source_changes.powered_off_by_us);
    assert_eq!(s.source_changes.old_annotation.as_deref(), Some(NOTE));
    // 감사: 내린 시간(노드의 일 — 쓰기 스레드가 곧 적는다)
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let entries = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: Some("vmware-vm.cutover".into()),
                limit: 100,
            })
            .await
            .unwrap();
        if entries.iter().any(|e| {
            e.detail
                .as_deref()
                .is_some_and(|d| d.starts_with("vmware-vm cutover w1-web01 (downtime"))
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "{entries:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 되돌리기: 이름 확인
    let (status, body) = admin
        .post("/vmware-vms/w1-web01/revert", json!({ "confirm": "web" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "confirm_mismatch");
    let task = act(&admin, "w1-web01", "revert", json!({ "confirm": "web01" })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Reverted, "{s:?}");
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOn);
    assert_eq!(fake.nics_start_connected("web01"), vec![true, true]);
    assert_eq!(fake.annotation("web01"), NOTE);
    assert!(!fake.cbt_enabled("web01"));
    // 대상 VM은 꺼진 채 남는다(사용자가 지운다).
    let vm = vm_named(&env, "web01").expect("target VM is kept");
    assert_eq!(vm.spec.power, opl_types::vm::PowerState::Stopped);
    // 되돌린 항목은 다시 되돌리거나 마무리할 수 없다.
    let (status, _) = admin
        .post("/vmware-vms/w1-web01/revert", json!({ "confirm": "web01" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    src.check(&fake);
}

/// 차가운 전환(꺼진 VM): 복사 뒤 flat 파일이 그대로면 원본을 끄지 않고(이미 꺼짐) VM을 만든다. 마무리는 우리가 켠 것을 치우고
/// 원본 보호(NIC 끊김·설명)는 남긴다. 복사 뒤 원본이 바뀌었으면(그새 켜짐) 전환하지 않고 다시 전체 복사로 돌린다.
#[tokio::test]
async fn cold_cutover_and_finalize() {
    let (env, admin, fake, storage) = setup().await;
    let mut src = Source::new(&fake, "db01", 1);
    let it = start_plan(&env, &admin, "c1", "DC1/vm/db01", "cold", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    // 복사 뒤 원본이 바뀌었다 → 다시 전체 복사
    src.write(&fake, 0, 5, b"changed after the copy");
    let task = act(&admin, "c1-db01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Copying, "{s:?}");
    assert!(s.error.unwrap().contains("changed after the copy"));
    assert_eq!(fake.annotation("db01"), NOTE);
    sync(&env, &admin, it.id, true).await;
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Ready);
    // 보기만 하는 사용자는 전환할 수 없다(대상 폴더의 vm.create·vm.power).
    let viewer = env.user("vi", "viewer").await;
    for action in ["cutover", "finalize"] {
        let (status, body) = viewer
            .post(&format!("/vmware-vms/c1-db01/{action}"), json!({}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{action}: {body}");
    }

    let task = act(&admin, "c1-db01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    assert!(!s.source_changes.powered_off_by_us);
    let d = &s.disks[0];
    assert!(storage.contents(d.volume.unwrap()).unwrap() == src.disks[0]);
    assert_eq!(fake.nics_start_connected("db01"), vec![false]);

    let task = act(&admin, "c1-db01", "finalize", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Finalized, "{s:?}");
    // 보호는 남는다(실수로 켜도 같은 MAC이 붙지 않게).
    assert_eq!(fake.nics_start_connected("db01"), vec![false]);
    assert!(fake.annotation("db01").contains("Orbivisor"));
    assert_eq!(fake.power("db01"), VmwarePower::PoweredOff);
    assert!(vm_named(&env, "db01").is_some());
    src.check(&fake);
}

/// 걸음마다 실패 주입: 원본을 건드리기 전(`preparing`)·원본 끄기(`stopping-source` — 설명 바꾸기 실패, 게스트 종료 시간
/// 넘김)·마지막 델타(`final-sync`)·VM 켜기(`starting-vm`). 어느 걸음이든 끝에 원본은 켜져 있고 NIC·설명이 되돌려지고 대상
/// VM이 없다. 원본을 끈 뒤의 실패는 `rolled-back`, 그 전은 `syncing`이다.
#[tokio::test]
async fn every_failing_step_leaves_the_source_running() {
    let (env, admin, fake, _storage) = setup().await;
    let mut src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "f1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    src.write(&fake, 0, 7, b"guest");
    // 사용자 스냅샷이 생겼다(백업 제품): 원본을 건드리기 전에 멈추고 그 스냅샷은 그대로 둔다.
    let user = fake.add_user_snapshot("web01", "nightly-backup");
    src.user_snapshots = user_snapshots(&fake, "web01");
    let (status, body) = admin.post("/vmware-vms/f1-web01/cutover", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "preflight_blocked", "{body}");
    assert_eq!(body["details"]["problems"][0]["code"], "vmware_snapshots");
    src.check(&fake);
    // 노드 캐시가 옛 인벤토리를 보여도 Task의 다시 확인이 막는다.
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmwareCutover,
            target: it.id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    let task = admin.wait_task(&json!({ "id": t.id })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("vmware_snapshots")
    );
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Syncing);
    assert_source_restored(&env, &fake, "web01", &s);
    src.check(&fake);
    // 사용자가 그 스냅샷을 지운다.
    fake.login()
        .await
        .remove_snapshot(&MoRef::new("VirtualMachineSnapshot", &user))
        .await
        .unwrap();
    src.user_snapshots.clear();

    let cases: [(&str, VmwareVmState, &str); 4] = [
        (
            "preparing",
            VmwareVmState::Syncing,
            "cutover stopped at preparing",
        ),
        (
            "annotate",
            VmwareVmState::Syncing,
            "cutover stopped at stopping-source: cannot annotate",
        ),
        ("timeout", VmwareVmState::Syncing, "vmware_shutdown_timeout"),
        (
            "final-sync",
            VmwareVmState::RolledBack,
            "cutover failed at final-sync",
        ),
    ];
    for (case, state, error) in cases {
        match case {
            "preparing" => fake.fail_next("CreateSnapshot_Task", "GenericVmConfigFault"),
            "annotate" => fake.fail_next("ReconfigVM_Task", "GenericVmConfigFault"),
            "timeout" => fake.ignore_shutdown(true),
            // 마지막 델타의 옛 스냅샷 지우기가 실패한다(켜진 채 델타의 지우기는 지나간다).
            _ => fake.fail_later("RemoveSnapshot_Task", 1, "GenericVmConfigFault"),
        }
        let task = act(&admin, "f1-web01", "cutover", json!({})).await;
        assert_eq!(task["status"]["state"], "failed", "{case}: {task}");
        fake.ignore_shutdown(false);
        let s = item(&env, it.id).status;
        assert_eq!(s.state, state, "{case}: {s:?}");
        assert!(s.error.as_deref().unwrap().contains(error), "{case}: {s:?}");
        assert_source_restored(&env, &fake, "web01", &s);
        src.check(&fake);
        // 다음 경우를 위해 다시 동기화(두 Orbivisor 스냅샷이 남았으면 정리된다)
        src.write(&fake, 1, 100, case.as_bytes());
        sync(&env, &admin, it.id, false).await;
        assert!(fake.snapshots("web01").len() <= 1);
    }
    // VM 켜기 실패 → 만든 VM을 지우고 되돌림
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    let task = act(&admin, "f1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::RolledBack, "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("cutover failed at starting-vm"),
        "{s:?}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    // 되돌린 뒤 다시 전환하면 된다.
    src.write(&fake, 0, 2 * BLOCK, b"after rollback");
    let task = act(&admin, "f1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Done);
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    src.check(&fake);
}

/// 할당량: VM을 만들 때 대상 폴더의 VM 수가 넘으면 자동 되돌림(원본 다시 켬, 대상 VM 없음, 볼륨은 남음).
#[tokio::test]
async fn a_quota_exceeded_at_creation_rolls_back() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "team", "kind": "vm", "quota": { "vms": 2 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let team = body["id"].as_str().unwrap().to_string();
    let it = start_plan(
        &env,
        &admin,
        "q1",
        "DC1/vm/Prod/web01",
        "warm",
        json!({ "rules": { "folders": { "kind": "flat", "into": team } } }),
    )
    .await;
    sync(&env, &admin, it.id, true).await;
    // 그새 폴더가 찼다.
    for name in ["a", "b"] {
        let mut req = vm_request(name);
        req["spec"]["folder"] = json!(team);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let task = act(&admin, "q1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::RolledBack, "{s:?}");
    let error = s.error.clone().unwrap();
    assert!(
        error.contains("cutover failed at creating-vm") && error.contains("would use 3 of 2 VMs"),
        "{error}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    src.check(&fake);
}

/// 되돌림 실패(원본을 켜지 못함): `failed`와 걸음, 손으로 할 일. 다시 하기가 되돌림을 다시 한다.
#[tokio::test]
async fn a_failed_rollback_reports_manual_steps_and_retry_finishes_it() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "r1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    fake.fail_later("RemoveSnapshot_Task", 1, "GenericVmConfigFault");
    fake.fail_next("PowerOnVM_Task", "GenericVmConfigFault");
    let task = act(&admin, "r1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Failed, "{s:?}");
    assert_eq!(
        s.cutover_step,
        Some(opl_types::vmware::VmwareCutoverStep::FinalSync)
    );
    let error = s.error.unwrap();
    assert!(
        error.contains("rollback failed") && error.contains("power on web01 in vCenter"),
        "{error}"
    );
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    // 다시 하기가 되돌림을 다시 한다.
    let task = act(&admin, "r1-web01", "retry", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::RolledBack, "{s:?}");
    assert!(!s.error.as_deref().unwrap().contains("rollback failed"));
    assert_source_restored(&env, &fake, "web01", &s);
    src.check(&fake);
}

/// 예약 전환: 시각이 지난 계획의 동기화된 항목을 스케줄러가 전환한다(한 번만 — 요청 시각을 적음).
#[tokio::test]
async fn the_scheduler_cuts_over_at_the_scheduled_time() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let it = start_plan(
        &env,
        &admin,
        "s1",
        "DC1/vm/Prod/web01",
        "warm",
        json!({ "cutover": { "at": at, "shutdown": "power-off" } }),
    )
    .await;
    let scheduler = VmwareScheduler::spawn(
        env.state.clone(),
        VmwareSchedulerConfig {
            tick: Duration::from_millis(100),
        },
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let s = loop {
        let s = item(&env, it.id).status;
        if s.state == VmwareVmState::Done {
            break s;
        }
        assert!(Instant::now() < deadline, "not cut over: {s:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(s.cutover_requested_at.unwrap() >= at);
    assert!(s.history.iter().any(|h| h.event == "scheduled cutover"));
    assert!(s.history.iter().any(|h| h.event == "source VM powered off"));
    let tasks = env.cluster.list::<TaskSpec>().unwrap().1;
    let cutovers = tasks
        .iter()
        .filter(|t| t.spec.target == it.id && t.spec.op == TaskOp::VmwareCutover)
        .count();
    assert_eq!(cutovers, 1);
    // 전환한 항목은 다시 건드리지 않는다.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Done);
    scheduler.shutdown().await;
    src.check(&fake);
}
