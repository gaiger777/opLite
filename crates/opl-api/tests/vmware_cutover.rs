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

/// 루트에 `vm.create`만 가진 사용자(계획을 만들고 돌릴 수 있지만 대상 폴더의 `vm.power`가 없다)
async fn migrator(env: &Env, admin: &Client) -> Client {
    let (status, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "migrator", "privileges": ["vm.create"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{role}");
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "mig", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/grants",
            json!({
                "principal": { "type": "user", "id": body["id"] },
                "role": { "type": "custom", "id": role["id"] },
                "target": { "type": "root" },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.login("mig", "password-1").await
}

/// 항목이 `pred`를 만족할 때까지 기다린다(10초).
async fn wait_item(env: &Env, id: Uuid, pred: impl Fn(&VmwareVmStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = item(env, id).status;
        if pred(&s) {
            return;
        }
        assert!(Instant::now() < deadline, "{s:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
    let (env, admin, fake, storage) = setup().await;
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
    // VM 켜기 실패 → 만든 VM을 지우고 되돌림. 켠 VM의 게스트가 대상에 썼을 수 있으므로 대상 내용을 버리고 처음부터 다시
    // 복사한다(리뷰 1 — 두 갈래가 섞이지 않게): `copying`, 볼륨 파일 없음, 그새 전환은 409.
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    let task = act(&admin, "f1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Copying, "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("cutover failed at starting-vm"),
        "{s:?}"
    );
    assert_eq!(s.rounds, 0, "{s:?}");
    assert!(
        s.disks
            .iter()
            .all(|d| d.change_id.is_none() && d.rounds == 0),
        "{s:?}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    for d in &s.disks {
        assert!(storage.contents(d.volume.unwrap()).is_none(), "{s:?}");
    }
    let (status, body) = admin.post("/vmware-vms/f1-web01/cutover", json!({})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("not_ready")),
        "{body}"
    );
    // 다시 전체 복사한 뒤 전환하면 된다.
    src.write(&fake, 0, 2 * BLOCK, b"after rollback");
    sync(&env, &admin, it.id, true).await;
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Syncing);
    let task = act(&admin, "f1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done);
    for (i, d) in s.disks.iter().enumerate() {
        assert!(storage.contents(d.volume.unwrap()).unwrap() == src.disks[i]);
    }
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
    // 리뷰 1: 루트 `vm.create`만으로는 원본 전원을 다루는 다시 하기·취소를 할 수 없다(전환과 같은 권한).
    let mig = migrator(&env, &admin).await;
    for path in [
        "/vmware-vms/r1-web01/retry",
        "/vmware-vms/r1-web01/cancel",
        "/vmware-migrations/r1/actions/cancel",
    ] {
        let (status, body) = mig.post(path, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
    }
    // 다시 하기가 요청하지 않은 전환 Task(겹친 요청 등)는 아무것도 하지 않는다.
    let mut stray: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmwareCutover,
            target: it.id,
            node: env.node,
        },
    );
    stray.name = stray.id.to_string();
    env.cluster.create(&stray).await.unwrap();
    let task = admin.wait_task(&json!({ "id": stray.id })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Failed);
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    // 다시 하기 둘이 겹쳐도 Task는 하나다(하나는 409 `migration_busy`).
    let (a, b) = tokio::join!(
        admin.post("/vmware-vms/r1-web01/retry", json!({})),
        admin.post("/vmware-vms/r1-web01/retry", json!({})),
    );
    let mut ok = Vec::new();
    for (status, body) in [a, b] {
        if status == StatusCode::OK {
            ok.push(body);
        } else {
            assert_eq!(
                (status, body["code"].as_str()),
                (StatusCode::CONFLICT, Some("migration_busy")),
                "{body}"
            );
        }
    }
    assert_eq!(ok.len(), 1, "{ok:?}");
    let task = admin.wait_task(&json!({ "id": ok[0]["task"] })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let cutovers = env
        .cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .into_iter()
        .filter(|t| t.spec.target == it.id && t.spec.op == TaskOp::VmwareCutover)
        .count();
    assert_eq!(
        cutovers, 3,
        "the first cutover, the stray task and one retry"
    );
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

/// 리뷰 1: 계획을 취소해도 전환한(`done`) VM은 그대로다 — 대상 VM이 남아 돌고 원본은 꺼진 채, 마무리·되돌리기로 끝낸다.
/// 마무리가 실패하면 `done`에 오류만 남기고(취소로 대상을 지울 수 없음) 다시 마무리하면 된다.
#[tokio::test]
async fn cancelling_a_migration_keeps_cut_over_vms() {
    let (env, admin, fake, storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "k1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    let task = act(&admin, "k1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = admin
        .post("/vmware-migrations/k1/actions/cancel", json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let scheduler = VmwareScheduler::spawn(
        env.state.clone(),
        VmwareSchedulerConfig {
            tick: Duration::from_millis(100),
        },
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    scheduler.shutdown().await;
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    let vm = vm_named(&env, "web01").expect("the cut over VM is kept");
    assert_eq!(Some(vm.id), s.target_vm);
    assert_eq!(vm.spec.power, opl_types::vm::PowerState::Running);
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    for (i, d) in s.disks.iter().enumerate() {
        assert!(storage.contents(d.volume.unwrap()).unwrap() == src.disks[i]);
    }
    let tasks = env.cluster.list::<TaskSpec>().unwrap().1;
    assert!(
        !tasks
            .iter()
            .any(|t| t.spec.target == it.id && matches!(t.spec.op, TaskOp::VmwareCleanup { .. })),
        "no cleanup for a cut over VM"
    );
    // 마무리 실패(CBT 끄기) → `done`에 오류, 취소는 409, 다시 마무리
    fake.fail_next("ReconfigVM_Task", "GenericVmConfigFault");
    let task = act(&admin, "k1-web01", "finalize", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("changed block tracking"),
        "{s:?}"
    );
    let (status, body) = admin.post("/vmware-vms/k1-web01/cancel", json!({})).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (StatusCode::CONFLICT, Some("vmware_vm_state")),
        "{body}"
    );
    let task = act(&admin, "k1-web01", "finalize", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Finalized);
    assert!(vm_named(&env, "web01").is_some());
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    src.check(&fake);
}

/// 리뷰 1: 원본 끄기 중에 사용자 스냅샷(백업 제품)이 생기고 게스트가 마지막으로 쓰면, 마지막 델타가 그 쓰기를 놓치지 않게
/// 회차가 스냅샷 나무를 다시 보고 멈춘다 — 전환은 되돌려지고(원본 다시 켬) 대상은 `done`이 되지 않는다.
#[tokio::test]
async fn a_snapshot_taken_during_the_cutover_rolls_it_back() {
    let (env, admin, fake, _storage) = setup().await;
    let mut src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "u1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    fake.ignore_shutdown(true);
    let (status, got) = admin.post("/vmware-vms/u1-web01/cutover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    // 원본 보호를 마치고 게스트 종료를 보낼 참이다.
    wait_item(&env, it.id, |s| s.source_changes.powered_off_by_us).await;
    fake.add_user_snapshot("web01", "veeam-temp");
    src.user_snapshots = user_snapshots(&fake, "web01");
    src.write(&fake, 0, 5 * BLOCK + 3, b"LAST WRITE DURING SHUTDOWN");
    fake.set_power("web01", VmwarePower::PoweredOff);
    fake.ignore_shutdown(false);
    let task = admin.wait_task(&json!({ "id": got["task"] })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::RolledBack, "{s:?}");
    let error = s.error.clone().unwrap();
    assert!(
        error.contains("cutover failed at final-sync") && error.contains("veeam-temp"),
        "{error}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    src.check(&fake);
}

/// 리뷰 1: 게스트 종료가 시간 한도를 넘긴 뒤 곧 꺼지면(느린 게스트) 되돌림이 원본을 다시 켠다.
#[tokio::test]
async fn a_slow_guest_shutdown_is_powered_on_again() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "g1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    fake.ignore_shutdown(true);
    let (status, got) = admin.post("/vmware-vms/g1-web01/cutover", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    // 원본 보호를 마치고 게스트 종료를 보낼 참이다.
    wait_item(&env, it.id, |s| s.source_changes.powered_off_by_us).await;
    // 한도(1초)를 넘긴 뒤, 더 지켜보는 동안 꺼진다.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    fake.set_power("web01", VmwarePower::PoweredOff);
    fake.ignore_shutdown(false);
    let task = admin.wait_task(&json!({ "id": got["task"] })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Syncing, "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("vmware_shutdown_timeout")
    );
    assert!(
        s.history.iter().any(|h| h
            .event
            .starts_with("the guest shut down after the time limit")),
        "{s:?}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    src.check(&fake);
}

/// 리뷰 1: 이미 내용이 있는 대상에 다시 전체 복사(`"*"`)하면 원본에서 쓰이지 않은 곳을 0으로 쓴다(남은 옛 내용이 없다).
#[tokio::test]
async fn a_full_copy_over_an_old_target_zeroes_the_gaps() {
    let (env, admin, fake, storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "z1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    let vol = item(&env, it.id).status.disks[0].volume.unwrap();
    // 블록 2는 원본에서 쓰이지 않았다(가짜 vSphere의 처음 내용).
    assert!(
        src.disks[0][2 * BLOCK as usize..3 * BLOCK as usize]
            .iter()
            .all(|b| *b == 0)
    );
    storage.poke(vol, 2 * BLOCK as usize + 5, b"stale target data");
    sync(&env, &admin, it.id, true).await;
    assert!(
        storage.contents(vol).unwrap() == src.disks[0],
        "stale data is left"
    );
    src.check(&fake);
}

/// 리뷰 1: 취소 정리는 다른 노드의 로컬 풀에 있는 대상 볼륨의 기록을 지우지 않는다(파일이 그 노드에 남으므로 그 노드가
/// 시작할 때 고아 정리가 지운다). 우리 스냅샷은 지운다.
#[tokio::test]
async fn cancel_leaves_volumes_on_another_nodes_local_pool() {
    let (env, admin, fake, storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "o1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    let vol = item(&env, it.id).status.disks[0].volume.unwrap();
    let other = item(&env, it.id).status.disks[1].volume.unwrap();
    let v = env.cluster.get::<VolumeSpec>(vol).unwrap().unwrap();
    let mut status = v.status.clone();
    status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(vol, &status)
        .await
        .unwrap();
    let task = act(&admin, "o1-web01", "cancel", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Cancelled, "{s:?}");
    assert!(env.cluster.get::<VolumeSpec>(vol).unwrap().is_some());
    assert!(
        storage.contents(vol).is_some(),
        "the file is left for its node"
    );
    assert!(
        s.history
            .iter()
            .any(|h| h.event.contains("that node removes it")),
        "{s:?}"
    );
    // 다른 디스크(이 노드)는 지웠다.
    assert!(s.disks[1].volume.is_none());
    assert!(env.cluster.get::<VolumeSpec>(other).unwrap().is_none());
    assert!(fake.snapshots("web01").is_empty());
    src.check(&fake);
}

/// 항목 상태를 손으로 고쳐 쓴다(opld가 걸음 중에 죽은 것처럼).
async fn set_status(env: &Env, id: Uuid, change: impl Fn(&mut VmwareVmStatus)) {
    let mut s = item(env, id).status;
    change(&mut s);
    env.cluster
        .update_status::<VmwareVmSpec>(id, &s)
        .await
        .unwrap();
}

/// 끊긴 전환을 잇는 Task(opld가 다시 떠서 잇는 것과 같다)를 만들고 끝을 기다린다.
async fn resume_cutover(env: &Env, admin: &Client, id: Uuid) -> Value {
    let mut t: opl_types::task::Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmwareCutover,
            target: id,
            node: env.node,
        },
    );
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    admin.wait_task(&json!({ "id": t.id })).await
}

/// 리뷰 2: `starting-vm`에서 잇는 전환은 vSphere가 필요 없다 — 원본 연결이 실패해도 켜진 대상 VM을 되돌리지 않고(대상
/// 디스크에 쓴 것이 남음) `done`을 적는다.
#[tokio::test]
async fn a_resumed_start_never_rolls_back_a_running_vm() {
    let (env, admin, fake, storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "r1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    let task = act(&admin, "r1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vol = item(&env, it.id).status.disks[0].volume.unwrap();
    let written = b"WRITTEN ON THE ORBIVISOR VM";
    storage.poke(vol, 100, written);
    // `done`을 적기 전에 opld가 죽었다.
    set_status(&env, it.id, |s| {
        s.state = VmwareVmState::CuttingOver;
        s.cutover_step = Some(opl_types::vmware::VmwareCutoverStep::StartingVm);
    })
    .await;
    fake.fail_next("RetrievePropertiesEx", "SystemError");
    let task = resume_cutover(&env, &admin, it.id).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    assert!(s.error.is_none(), "{s:?}");
    let vm = vm_named(&env, "web01").expect("the target VM is kept");
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(
        &storage.contents(vol).unwrap()[100..100 + written.len()],
        written
    );
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);

    // 이어 갈 때 VM이 켜지지 않아도(앞 opld가 켰을 수 있다) 되돌리지 않는다: `done`에 오류, VM·디스크는 둔다.
    set_status(&env, it.id, |s| {
        s.state = VmwareVmState::CuttingOver;
        s.cutover_step = Some(opl_types::vmware::VmwareCutoverStep::StartingVm);
    })
    .await;
    let mut spec = vm.spec.clone();
    spec.power = opl_types::vm::PowerState::Stopped;
    env.cluster
        .update_spec::<VmSpec>(vm.id, None, vm.labels.clone(), &spec)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while vm_named(&env, "web01").unwrap().status.phase != VmPhase::Stopped {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    let task = resume_cutover(&env, &admin, it.id).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Done, "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("it and its disks were kept"),
        "{s:?}"
    );
    assert_eq!(s.target_vm, Some(vm.id));
    assert!(vm_named(&env, "web01").is_some());
    assert_eq!(
        &storage.contents(vol).unwrap()[100..100 + written.len()],
        written
    );
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    // 돌아가는 길은 되돌리기다. 두 번 이어 간 전환은 vSphere를 부르지 않았다: 넣은 오류가 아직 남아 첫 되돌리기가 맞는다.
    let task = act(&admin, "r1-web01", "revert", json!({ "confirm": "web01" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("injected SystemError"),
        "{task}"
    );
    let task = act(&admin, "r1-web01", "revert", json!({ "confirm": "web01" })).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOn);
    src.check(&fake);
}

/// 리뷰 2: 원본 보호를 적은 뒤 `ReconfigVM` 전에 opld가 죽었으면, 이어 가는 전환이 NIC·설명을 원본의 지금 값과 맞춘다.
#[tokio::test]
async fn a_resumed_stop_applies_the_recorded_protection() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "p1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    // 가짜 vSphere의 NIC 장치 키
    let keys = vec![4000, 4001];
    set_status(&env, it.id, |s| {
        s.state = VmwareVmState::CuttingOver;
        s.cutover_step = Some(opl_types::vmware::VmwareCutoverStep::StoppingSource);
        s.source_changes.nics_disconnected = keys.clone();
        s.source_changes.annotation_added = true;
        s.source_changes.old_annotation = Some(NOTE.into());
    })
    .await;
    let task = resume_cutover(&env, &admin, it.id).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(item(&env, it.id).status.state, VmwareVmState::Done);
    assert_eq!(fake.nics_start_connected("web01"), vec![false, false]);
    let note = fake.annotation("web01");
    assert!(note.starts_with("Migrated to Orbivisor VM"), "{note}");
    assert!(note.ends_with(NOTE), "{note}");
    src.check(&fake);
}

/// 리뷰 2: 시간 한도와 더 지켜보는 시간을 넘긴 뒤에 게스트가 꺼지면 다음 동기화가 원본을 다시 켠다.
#[tokio::test]
async fn a_shutdown_finishing_after_the_watch_is_powered_on_by_the_next_sync() {
    let (env, admin, fake, _storage) = setup().await;
    let src = Source::new(&fake, "web01", 2);
    let it = start_plan(&env, &admin, "l1", "DC1/vm/Prod/web01", "warm", json!({})).await;
    sync(&env, &admin, it.id, true).await;
    fake.ignore_shutdown(true);
    let task = act(&admin, "l1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    fake.ignore_shutdown(false);
    let s = item(&env, it.id).status;
    assert_eq!(s.state, VmwareVmState::Syncing, "{s:?}");
    assert!(s.shutdown_sent_at.is_some(), "{s:?}");
    assert!(
        s.error
            .as_deref()
            .unwrap()
            .contains("may still finish shutting down"),
        "{s:?}"
    );
    assert_source_restored(&env, &fake, "web01", &s);
    // 보낸 종료가 늦게 끝났다.
    fake.set_power("web01", VmwarePower::PoweredOff);
    sync(&env, &admin, it.id, false).await;
    let s = item(&env, it.id).status;
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOn, "{s:?}");
    assert!(s.shutdown_sent_at.is_none(), "{s:?}");
    assert!(
        s.history
            .iter()
            .any(|h| h.event.contains("the source VM was powered on again")),
        "{s:?}"
    );
    src.check(&fake);

    // 표시가 남은 채 다시 전환할 때 원본이 꺼져 있으면 우리가 끈 것으로 적는다(되돌림·되돌리기가 켠다).
    set_status(&env, it.id, |s| s.shutdown_sent_at = Some(1)).await;
    fake.set_power("web01", VmwarePower::PoweredOff);
    let task = act(&admin, "l1-web01", "cutover", json!({})).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let s = item(&env, it.id).status;
    assert!(
        s.shutdown_sent_at.is_none() && s.source_changes.powered_off_by_us,
        "{s:?}"
    );
    src.check(&fake);
}
