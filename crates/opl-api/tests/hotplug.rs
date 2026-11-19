//! 핫플러그 Task(`vm-hotplug`, 계획 54): 실행 중인 VM의 vCPU·메모리·디스크·NIC를 Mock 하이퍼바이저로 바꾸고 실행 중
//! 하드웨어 기록(`status.hardware`)과 보류를 남기며, 실패하면 스펙의 장치 목록을 되돌린다. `actions/hotplug`의 거절,
//! 마이그레이션과의 배타, VM 줄. `PUT /vms/{key}`의 실행 중 적용(헤더 `orbivisor-task`, 409들, 미할당 메모리)과 메모리
//! 스냅샷 제한.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, WAIT, vm_request};
use opl_types::node::{HOTPLUG, NodeHotplug, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePoolSpec, Volume, VolumeContent, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{
    DiskBus, DiskSpec, MacAddr, NicModel, NicSpec, PendingReason, PowerState, RunningHardware, Vm,
    VmPhase, VmSpec,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const ALL: NodeHotplug = NodeHotplug {
    cpu: true,
    memory: true,
    memory_prealloc: true,
};

/// 노드 `node`의 핫플러그 능력을 기록한다(opld의 `record_node` 대신).
async fn set_hotplug(env: &Env, node: Uuid, caps: NodeHotplug) {
    let mut n = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.hotplug = Some(caps);
    env.cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

/// 이 노드의 능력이 `caps`이고 Task 실행기가 가짜 저장소를 쓰는 환경과 dir 풀 `local`의 Ready 볼륨 `boot`·`data`·
/// `extra`(qcow2)
async fn setup(caps: NodeHotplug) -> (Env, Arc<FakeStorage>) {
    setup_with(caps, |_| {}).await
}

/// [`setup`]과 같되 `tweak`으로 reconciler 설정을 바꾼다.
async fn setup_with(
    caps: NodeHotplug,
    tweak: impl FnOnce(&mut opl_reconciler::ReconcilerConfig),
) -> (Env, Arc<FakeStorage>) {
    let mut env = Env::start_with(tweak, |_| async {}).await;
    set_hotplug(&env, env.node, caps).await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes: vec![],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    for name in ["boot", "data", "extra"] {
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Qcow2,
                content: Default::default(),
                base: None,
            },
        );
        volume.status.phase = VolumePhase::Ready;
        volume.status.node = Some(env.node);
        env.cluster.create(&volume).await.unwrap();
    }
    let storage = Arc::new(FakeStorage::default());
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    (env, storage)
}

/// 켜서 기록이 생긴 VM `web`: 소켓 1·코어 2(최대 8), 메모리 1024 MiB(최대 4096), 디스크 `boot`, user NIC 하나
async fn running_web(env: &Env, admin: &Client) -> Uuid {
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "cores": 2, "max_vcpus": 8 });
    req["spec"]["memory_mib"] = json!(1024);
    req["spec"]["max_memory_mib"] = json!(4096);
    req["spec"]["disks"] = json!([{ "volume": "boot", "bus": "virtio-blk" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    succeeded(&admin.act("web", "start").await);
    let id = vm["id"].as_str().unwrap().parse().unwrap();
    hardware(env, id);
    id
}

fn vm(env: &Env, id: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

fn hardware(env: &Env, id: Uuid) -> RunningHardware {
    vm(env, id).status.hardware.expect("running hardware")
}

fn volume_id(env: &Env, name: &str) -> Uuid {
    env.cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap()
        .id
}

/// 스펙을 `f`로 고쳐 저장소에 바로 쓴다(실행 중 하드웨어는 그대로).
async fn change(env: &Env, id: Uuid, f: impl Fn(&mut VmSpec)) {
    loop {
        let vm = vm(env, id);
        let mut spec = vm.spec.clone();
        f(&mut spec);
        if env
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
            .is_ok()
        {
            return;
        }
    }
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn failed(task: &Value) -> String {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    task["status"]["error"].as_str().unwrap().to_string()
}

fn disk(volume: &str) -> DiskSpec {
    DiskSpec {
        volume: volume.into(),
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    }
}

fn user_nic(last: u8) -> NicSpec {
    NicSpec {
        network: "user".into(),
        model: NicModel::Virtio,
        mac: Some(MacAddr([0x52, 0x54, 0, 0, 0, last])),
        security_groups: vec![],
    }
}

/// vCPU는 상한 안에서 늘고, 게스트가 내려 주지 않은 줄이기는 닿은 수를 기록하고 `failed` 보류로 남긴다.
#[tokio::test]
async fn vcpus_are_added_and_a_held_removal_is_recorded() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    assert_eq!(env.hv.vcpus(id), Some(2));

    change(&env, id, |s| s.cpu.cores = 4).await;
    let task = admin.act("web", "hotplug").await;
    succeeded(&task);
    assert_eq!(task["status"]["error"], Value::Null, "{task}");
    assert_eq!(env.hv.vcpus(id), Some(4));
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().vcpus, 4);
    assert!(hw.pending.is_empty(), "{hw:?}");

    env.hv.hold_unplug(id, true);
    change(&env, id, |s| s.cpu.cores = 2).await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert!(
        error.contains("the guest did not release vCPU 3 within 200ms"),
        "{error}"
    );
    assert_eq!(env.hv.vcpus(id), Some(4));
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().vcpus, 4);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    let p = &hw.pending[0];
    assert_eq!(
        (p.field.as_str(), p.running.as_str(), p.wanted.as_str()),
        ("vcpus", "4", "2")
    );
    assert_eq!(p.reason, PendingReason::Failed);
    assert!(p.detail.as_ref().unwrap().contains("vCPU 3"), "{p:?}");
    // 남은 vCPU는 떼는 중이다(마이그레이션을 막는다).
    assert_eq!(hw.unplugging, ["cpu2", "cpu3"]);

    // 게스트가 내려 주면 다시 해 보기가 끝내고 보류를 지운다.
    env.hv.hold_unplug(id, false);
    succeeded(&admin.act("web", "hotplug").await);
    assert_eq!(env.hv.vcpus(id), Some(2));
    let hw = hardware(&env, id);
    assert!(hw.pending.is_empty());
    assert!(hw.unplugging.is_empty());
}

/// QEMU가 vCPU 떼기를 거절하면(게스트가 vCPU 떼기를 켜지 않은 q35) 수는 그대로이고 `failed` 보류로 남되, 진행 중인
/// 떼기가 없으므로 `unplugging`에 두지 않는다.
#[tokio::test]
async fn a_refused_vcpu_removal_is_left_pending_without_unplugging() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    change(&env, id, |s| s.cpu.cores = 4).await;
    succeeded(&admin.act("web", "hotplug").await);

    env.hv.refuse_vcpu_unplug(id, true);
    change(&env, id, |s| s.cpu.cores = 2).await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert!(
        error.ends_with("the guest does not support vCPU removal yet; restart the VM to apply"),
        "{error}"
    );
    assert_eq!(env.hv.vcpus(id), Some(4));
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().vcpus, 4);
    assert!(hw.unplugging.is_empty(), "{hw:?}");
    let p = &hw.pending[0];
    assert_eq!(
        (
            p.field.as_str(),
            p.running.as_str(),
            p.wanted.as_str(),
            p.reason
        ),
        ("vcpus", "4", "2", PendingReason::Failed)
    );
    assert_eq!(
        p.detail.as_deref(),
        Some("the guest does not support vCPU removal yet; restart the VM to apply")
    );
}

/// 메모리는 virtio-mem에 꽂은 크기로 늘고 준다. 게스트가 내려 주지 않은 줄이기는 `failed` 보류다.
#[tokio::test]
async fn memory_is_plugged_and_unplugged() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    change(&env, id, |s| s.memory_mib = 2048).await;
    succeeded(&admin.act("web", "hotplug").await);
    assert_eq!(env.hv.plugged_mib(id), Some(1024));
    let hw = hardware(&env, id);
    assert_eq!(hw.memory_now(), 2048);
    assert!(hw.pending.is_empty(), "{hw:?}");

    change(&env, id, |s| s.memory_mib = 1536).await;
    succeeded(&admin.act("web", "hotplug").await);
    assert_eq!(env.hv.plugged_mib(id), Some(512));
    assert_eq!(hardware(&env, id).layout.unwrap().plugged_mib, 512);

    env.hv.hold_unplug(id, true);
    change(&env, id, |s| s.memory_mib = 1024).await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert!(
        error.contains("the guest released only 0 of 512 MiB within 200ms"),
        "{error}"
    );
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().plugged_mib, 512);
    let p = &hw.pending[0];
    assert_eq!(
        (p.field.as_str(), p.running.as_str(), p.wanted.as_str()),
        ("memory_mib", "1536", "1024")
    );
    assert_eq!(p.reason, PendingReason::Failed);
}

/// virtio-mem이 없는 노드에서 켠 VM의 메모리 변경은 성공한 Task의 경고와 `unsupported` 보류다.
#[tokio::test]
async fn memory_without_virtio_mem_is_left_pending() {
    let (env, _) = setup(NodeHotplug {
        cpu: true,
        memory: false,
        memory_prealloc: false,
    })
    .await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    change(&env, id, |s| s.memory_mib = 2048).await;
    let task = admin.act("web", "hotplug").await;
    succeeded(&task);
    assert_eq!(
        task["status"]["error"],
        "pending until restart: memory_mib (unsupported: this node has no virtio-mem)"
    );
    let hw = hardware(&env, id);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    assert_eq!(hw.pending[0].reason, PendingReason::Unsupported);
    assert!(env.hv.hotplug_calls().is_empty());
}

/// 끝에 붙인 디스크는 볼륨을 켜고 빈 루트 포트에 붙고, 떼면 포트가 비고 볼륨을 끈다.
#[tokio::test]
async fn a_disk_is_added_and_removed() {
    let (env, storage) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let data = volume_id(&env, "data");

    change(&env, id, |s| s.disks.push(disk("data"))).await;
    succeeded(&admin.act("web", "hotplug").await);
    assert!(storage.activations().contains(&data));
    assert!(env.hv.devices(id).contains(&"disk1-dev".to_string()));
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().disk_ports, [0]);
    assert_eq!(hw.disks, vm(&env, id).spec.disks);

    change(&env, id, |s| {
        s.disks.pop();
    })
    .await;
    succeeded(&admin.act("web", "hotplug").await);
    assert!(!env.hv.devices(id).contains(&"disk1-dev".to_string()));
    assert!(storage.deactivations().contains(&("local".into(), data)));
    let hw = hardware(&env, id);
    assert!(hw.layout.as_ref().unwrap().disk_ports.is_empty());
    assert_eq!(hw.disks, [disk("boot")]);
}

/// 붙이지 못한 디스크는 스펙에서 빠지고(스펙 = 도는 장치) 켠 볼륨을 끈다.
#[tokio::test]
async fn a_failed_disk_add_is_undone_in_the_spec() {
    let (env, storage) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let data = volume_id(&env, "data");

    env.hv.fail_next_hotplug("boom");
    change(&env, id, |s| s.disks.push(disk("data"))).await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert!(
        error.starts_with("disk \"data\" was not added: "),
        "{error}"
    );
    assert!(error.ends_with("; the change was undone"), "{error}");
    assert_eq!(vm(&env, id).spec.disks, [disk("boot")]);
    assert!(!env.hv.devices(id).contains(&"disk1-dev".to_string()));
    assert!(storage.deactivations().contains(&("local".into(), data)));
    let hw = hardware(&env, id);
    assert!(hw.layout.as_ref().unwrap().disk_ports.is_empty());
}

/// 게스트가 내려 주지 않은 떼기는 스펙을 되돌리지 않는다(사용자가 뗀 그대로): 장치는 기록에 남아 `unplugging`과
/// `failed` 보류가 되고, 하지 않은 붙이기만 스펙에서 빠진다. 그동안 마이그레이션은 막히고, 게스트가 내려 준 뒤의
/// 다음 Task가 떼기를 마치고 볼륨을 끈다.
#[tokio::test]
async fn a_disk_the_guest_keeps_is_removed_by_the_next_task() {
    let (env, storage) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    second_node(&env, ALL).await;
    change(&env, id, |s| s.disks.push(disk("data"))).await;
    succeeded(&admin.act("web", "hotplug").await);
    let data = volume_id(&env, "data");

    env.hv.hold_unplug(id, true);
    // `data`를 떼고 `extra`를 붙인다: 떼기가 먼저 시간을 넘는다.
    change(&env, id, |s| s.disks[1] = disk("extra")).await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert_eq!(
        error,
        "disk \"data\" was not removed: the guest did not release it within 200ms; the change was undone"
    );
    assert_eq!(vm(&env, id).spec.disks, [disk("boot")]);
    assert!(env.hv.devices(id).contains(&"disk1-dev".to_string()));
    let extra = volume_id(&env, "extra");
    assert!(!storage.activations().contains(&extra));
    assert!(!storage.deactivations().contains(&("local".into(), data)));
    let hw = hardware(&env, id);
    assert_eq!(hw.disks, [disk("boot"), disk("data")]);
    assert_eq!(hw.unplugging, ["disk1-dev"]);
    assert_eq!(hw.layout.as_ref().unwrap().disk_ports, [0]);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    let p = &hw.pending[0];
    assert_eq!(
        (p.field.as_str(), p.running.as_str(), p.wanted.as_str()),
        ("disk1-dev", "disk \"data\"", "removed")
    );
    assert_eq!(p.reason, PendingReason::Failed);
    assert_eq!(
        p.detail.as_deref(),
        Some("the guest has not released disk \"data\" yet; run `vm hotplug` again")
    );
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    // 게스트가 내려 주면 다음 Task가 떼기를 마친다.
    env.hv.hold_unplug(id, false);
    succeeded(&admin.act("web", "hotplug").await);
    assert!(!env.hv.devices(id).contains(&"disk1-dev".to_string()));
    assert!(storage.deactivations().contains(&("local".into(), data)));
    let hw = hardware(&env, id);
    assert_eq!(hw.disks, [disk("boot")]);
    assert!(hw.unplugging.is_empty());
    assert!(hw.pending.is_empty(), "{hw:?}");
    assert!(hw.layout.as_ref().unwrap().disk_ports.is_empty());
}

/// Task가 실행 중에 맞출 수 없는 스펙(부팅 디스크 떼기 — API를 거치지 않고 바뀜)을 만나면 스펙의 장치 목록을
/// 되돌리고 보류를 다시 셈한다.
#[tokio::test]
async fn a_device_change_the_running_vm_cannot_take_is_undone() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    change(&env, id, |s| {
        s.disks.clear();
        s.firmware = opl_types::vm::Firmware::Bios;
        s.cpu.cores = 4;
    })
    .await;
    let error = failed(&admin.act("web", "hotplug").await);
    assert_eq!(
        error,
        "VM \"web\": cannot remove disk \"boot\" while the VM is running; the change was undone"
    );
    assert_eq!(vm(&env, id).spec.disks, [disk("boot")]);
    // 함께 바뀐 vCPU는 하지 않았으므로 다시 해 보기를 기다리는 `failed` 보류다.
    let hw = hardware(&env, id);
    assert_eq!(hw.pending.len(), 2, "{hw:?}");
    assert_eq!(hw.pending[0].field, "firmware");
    let p = &hw.pending[1];
    assert_eq!(
        (
            p.field.as_str(),
            p.running.as_str(),
            p.wanted.as_str(),
            p.reason
        ),
        ("vcpus", "2", "4", PendingReason::Failed)
    );
    assert_eq!(
        p.detail.as_deref(),
        Some("not tried because an earlier change failed")
    );
    assert!(env.hv.hotplug_calls().is_empty());
}

/// 스펙의 디스크·NIC가 도는 QEMU의 장치와 다르면(API를 거치지 않고 바뀜) 옮기지 않는다.
#[tokio::test]
async fn a_vm_whose_devices_differ_from_its_spec_does_not_migrate() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    second_node(&env, ALL).await;
    change(&env, id, |s| s.disks.push(disk("data"))).await;
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    assert_eq!(
        body["message"],
        "cannot migrate VM \"web\": the VM's devices do not match the running hardware; run `vm hotplug` or restart it"
    );
}

/// Task 도중 VM이 다시 켜지면(pid가 바뀜) 그 QEMU의 기록을 고치거나 스펙을 되돌리지 않고 멈춘다.
#[tokio::test]
async fn a_restart_during_the_task_undoes_nothing() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let before = vm(&env, id).status.pid;

    env.hv.hold_next_hotplug();
    change(&env, id, |s| {
        s.cpu.cores = 4;
        s.disks.push(disk("data"));
    })
    .await;
    let (status, task) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    tokio::time::timeout(WAIT, env.hv.hotplug_reached())
        .await
        .expect("set_vcpus");
    // QEMU가 끝나고 다시 켜진다(새 pid, 새 기록).
    env.hv.crash(id);
    let deadline = tokio::time::Instant::now() + WAIT;
    while vm(&env, id).status.phase != VmPhase::Failed {
        assert!(tokio::time::Instant::now() < deadline, "not stopped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    succeeded(&admin.act("web", "start").await);
    assert_ne!(vm(&env, id).status.pid, before);
    let restarted = hardware(&env, id);
    env.hv.release_hotplug();
    let error = failed(&admin.wait_task(&task).await);
    assert_eq!(
        error,
        "VM \"web\": the VM was restarted or moved while its hardware was being changed; nothing was undone"
    );
    assert_eq!(vm(&env, id).spec.disks, [disk("boot"), disk("data")]);
    assert_eq!(hardware(&env, id), restarted);
}

/// NIC도 끝에 붙고 실행 중 붙인 것은 뗀다. 디스크가 쓰는 포트를 건너뛴다.
#[tokio::test]
async fn nics_are_added_and_removed() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    change(&env, id, |s| s.disks.push(disk("data"))).await;
    succeeded(&admin.act("web", "hotplug").await);

    change(&env, id, |s| s.nics.push(user_nic(9))).await;
    succeeded(&admin.act("web", "hotplug").await);
    assert!(env.hv.devices(id).contains(&"nic1".to_string()));
    let hw = hardware(&env, id);
    assert_eq!(hw.layout.as_ref().unwrap().nic_ports, [1]);
    assert_eq!(hw.nics.len(), 2);

    change(&env, id, |s| {
        s.nics.pop();
    })
    .await;
    succeeded(&admin.act("web", "hotplug").await);
    assert!(!env.hv.devices(id).contains(&"nic1".to_string()));
    let hw = hardware(&env, id);
    assert!(hw.layout.as_ref().unwrap().nic_ports.is_empty());
    assert_eq!(hw.nics.len(), 1);
}

/// 실행 중 하드웨어 기록을 `f`로 고쳐 쓴다(Task가 기록하기 전에 멈춘 것처럼).
async fn set_hardware(env: &Env, id: Uuid, f: impl Fn(&mut RunningHardware)) {
    loop {
        let vm = vm(env, id);
        let mut status = vm.status.clone();
        f(status.hardware.as_mut().unwrap());
        if env
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
            .is_ok()
        {
            return;
        }
    }
}

/// 앞서 중단된 Task가 붙였지만 기록하지 못한 장치는 다음 Task가 그대로 기록한다(되돌리거나 볼륨을 끄지 않는다).
#[tokio::test]
async fn a_device_an_interrupted_task_added_is_recorded() {
    let (env, storage) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    change(&env, id, |s| {
        s.disks.push(disk("data"));
        s.nics.push(user_nic(9));
    })
    .await;
    succeeded(&admin.act("web", "hotplug").await);
    set_hardware(&env, id, |hw| {
        hw.disks.truncate(1);
        hw.nics.truncate(1);
        let layout = hw.layout.as_mut().unwrap();
        layout.disk_ports.clear();
        layout.nic_ports.clear();
    })
    .await;

    succeeded(&admin.act("web", "hotplug").await);
    let hw = hardware(&env, id);
    assert_eq!(hw.disks, [disk("boot"), disk("data")]);
    assert_eq!(hw.nics.len(), 2);
    let layout = hw.layout.unwrap();
    assert_eq!((layout.disk_ports, layout.nic_ports), (vec![0], vec![1]));
    assert_eq!(vm(&env, id).spec.disks, [disk("boot"), disk("data")]);
    let data = volume_id(&env, "data");
    assert!(!storage.deactivations().contains(&("local".into(), data)));
}

/// 떼는 중(게스트가 아직 내려 주지 않음)인 디스크의 볼륨은 스펙에서 빠졌어도 쓰는 중이다: 지우기·다른 VM에 붙이기·
/// 크기 늘리기는 409 `volume_in_use`이고 유휴 볼륨 정리가 끄지 않는다.
#[tokio::test]
async fn a_volume_still_being_released_stays_in_use() {
    let (mut env, storage) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    change(&env, id, |s| s.disks.push(disk("data"))).await;
    succeeded(&admin.act("web", "hotplug").await);
    env.hv.hold_unplug(id, true);
    change(&env, id, |s| {
        s.disks.pop();
    })
    .await;
    failed(&admin.act("web", "hotplug").await);
    assert_eq!(hardware(&env, id).unplugging, ["disk1-dev"]);

    let releasing =
        "volume \"data\" is still being released by VM \"web\"; run `vm hotplug` or restart the VM";
    let (status, body) = admin.delete("/volumes/data").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        (body["code"].as_str(), body["message"].as_str()),
        (Some("volume_in_use"), Some(releasing))
    );
    let mut req = vm_request("other");
    req["spec"]["disks"] = json!([{ "volume": "data", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["message"], releasing);
    let (status, body) = admin
        .post("/volumes/data/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["message"], releasing);

    // 유휴 볼륨 정리(실행기가 시작할 때)는 떼는 중인 볼륨을 끄지 않는다. 다른 남은 볼륨을 끄는 것으로 돈 것을 안다.
    let data = volume_id(&env, "data");
    let orphan = Uuid::now_v7();
    storage.set_active("local", data);
    storage.set_active("local", orphan);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while storage.active().contains(&orphan) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "orphan still active"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(storage.active().contains(&data));
}

/// 다시 적용 동작도 메모리를 바로 늘리면 노드의 미할당 메모리를 보고, 끝나지 않은 스냅샷·백업 Task가 있으면 거절한다.
#[tokio::test]
async fn the_hotplug_action_checks_memory_and_busy_tasks() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status.memory_mib = 2048;
    env.cluster
        .update_status::<NodeSpec>(env.node, &n.status)
        .await
        .unwrap();
    change(&env, id, |s| s.memory_mib = 4096).await;
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert!(env.hv.hotplug_calls().is_empty());

    change(&env, id, |s| s.memory_mib = 1024).await;
    let backup = stuck_task(
        &env,
        TaskOp::VmBackup {
            backup: Uuid::now_v7(),
        },
        id,
    )
    .await;
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    env.cluster
        .delete::<TaskSpec>(backup.id, None)
        .await
        .unwrap();
    succeeded(&admin.act("web", "hotplug").await);
}

/// 계획 64 리뷰 2 N2: 호스트 노드에 묶는 VM의 늘린 메모리(virtio-mem)는 같은 호스트 노드에 묶인다. 노드 전체(Raft
/// 장부의 묶인 메모리 6144)로는 남아도 그 호스트 노드(3072)에 들지 않으면 409 `no_capacity`다(꽂지 않음).
#[tokio::test]
async fn bound_vms_grow_memory_only_within_their_host_node() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status.memory_mib = 65536;
    n.status.topology = Some(
        serde_json::from_value(json!({
            "sockets": 1, "threads_per_core": 1, "cpus": "0-3", "pinning": true, "cpuset": true,
            "probed_at": 1, "nodes": [{"id": 0, "cpus": "0-1", "memory_mib": 3072},
                                      {"id": 1, "cpus": "2-3", "memory_mib": 3072}],
        }))
        .unwrap(),
    );
    env.cluster
        .update_status::<NodeSpec>(env.node, &n.status)
        .await
        .unwrap();
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "cores": 2 });
    req["spec"]["memory_mib"] = json!(1024);
    req["spec"]["max_memory_mib"] = json!(4096);
    req["spec"]["numa"] = json!({ "nodes": 1, "memory": "strict" });
    req["spec"]["disks"] = json!([{ "volume": "boot", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    succeeded(&admin.act("web", "start").await);
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let hw = hardware(&env, id);
    assert_eq!(hw.host_placement.as_ref().unwrap().host_nodes, [0]);
    change(&env, id, |s| s.memory_mib = 4096).await;
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("cannot grow its memory to 4096 MiB"),
        "{body}"
    );
    assert!(env.hv.hotplug_calls().is_empty());
    // 호스트 노드에 드는 만큼(3072)은 꽂는다.
    change(&env, id, |s| s.memory_mib = 3072).await;
    succeeded(&admin.act("web", "hotplug").await);
}

/// 다시 적용 동작은 핫플러그 기록이 있는 실행 중 VM에만, 모든 노드가 `hotplug`를 알 때만 받는다.
#[tokio::test]
async fn the_hotplug_action_needs_a_running_vm_and_the_feature() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/vms", vm_request("off")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin.post("/vms/off/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_not_running", "{body}");

    running_web(&env, &admin).await;
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status.features.retain(|f| f != HOTPLUG);
    env.cluster
        .update_status::<NodeSpec>(env.node, &n.status)
        .await
        .unwrap();
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

/// 오프라인 노드에 배정되어 끝나지 않는 Task
async fn stuck_task(env: &Env, op: TaskOp, target: Uuid) -> Task {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    task.status.state = TaskState::Pending;
    env.cluster.create(&task).await.unwrap();
    task
}

/// 다른 노드 `node2`(온라인, 모든 기능)를 기록한다.
async fn second_node(env: &Env, caps: NodeHotplug) -> Uuid {
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut other: opl_types::node::Node = Resource::new("node2", me.spec.clone());
    me.status.hotplug = Some(caps);
    other.status = me.status;
    env.cluster.create(&other).await.unwrap();
    other.id
}

/// 핫플러그 Task가 끝나지 않은 VM은 옮기지 않고, 옮기는 중인 VM은 핫플러그를 받지 않는다.
#[tokio::test]
async fn migration_and_hotplug_exclude_each_other() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    second_node(&env, ALL).await;

    let hot = stuck_task(&env, TaskOp::VmHotplug, id).await;
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&format!("is still being applied (task {})", hot.id)),
        "{body}"
    );
    env.cluster.delete::<TaskSpec>(hot.id, None).await.unwrap();

    let op = TaskOp::VmMigrate {
        node: Uuid::now_v7(),
        pool: None,
    };
    stuck_task(&env, op, id).await;
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
}

/// virtio-mem으로 켠 VM은 virtio-mem이 없는 노드로 옮길 수 없다(대상은 같은 장치로 켠다).
#[tokio::test]
async fn a_vm_with_virtio_mem_does_not_move_to_a_node_without_it() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    running_web(&env, &admin).await;
    second_node(
        &env,
        NodeHotplug {
            cpu: true,
            memory: false,
            memory_prealloc: false,
        },
    )
    .await;
    let (status, body) = admin
        .post("/vms/web/migrate", json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "migration_unsupported", "{body}");
    assert_eq!(
        body["message"],
        "cannot migrate VM \"web\": node \"node2\" has no virtio-mem"
    );
}

/// 핫플러그 Task는 VM의 줄에 서서 앞의 스냅샷 Task가 끝난 뒤 돈다.
#[tokio::test]
async fn a_hotplug_task_waits_for_the_snapshot_before_it() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    env.hv.hang_next_snapshot_save();
    let (status, snapshot) = admin
        .post("/vms/web/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{snapshot}");
    tokio::time::timeout(WAIT, env.hv.snapshot_save_reached())
        .await
        .expect("snapshot save");
    change(&env, id, |s| s.cpu.cores = 4).await;
    // 다시 적용 동작은 스냅샷 Task가 끝나지 않았으면 거절한다.
    let (status, body) = admin.post("/vms/web/actions/hotplug", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    // 그래도 줄에 선 핫플러그 Task(그 사이 만들어진 것)는 스냅샷이 끝나지 않은 동안 시작하지 않는다.
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmHotplug,
            target: id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.status.state = TaskState::Pending;
    env.cluster.create(&task).await.unwrap();
    let hot = json!({ "id": task.id });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, now) = admin
        .get(&format!("/tasks/{}", hot["id"].as_str().unwrap()))
        .await;
    assert_eq!(now["status"]["state"], "pending", "{now}");
    assert!(env.hv.hotplug_calls().is_empty());

    // 강제로 끄면 스냅샷이 실패하고, 그 뒤에 돈 핫플러그는 꺼진 VM이라 실패한다.
    succeeded(&admin.act("web", "stop?mode=force").await);
    assert_eq!(
        admin.wait_task(&snapshot).await["status"]["state"],
        "failed"
    );
    let error = failed(&admin.wait_task(&hot).await);
    assert_eq!(error, "VM \"web\" is not running on this node");
    assert!(env.hv.hotplug_calls().is_empty());
    assert_eq!(vm(&env, id).status.phase, VmPhase::Stopped);
    assert_eq!(vm(&env, id).spec.power, PowerState::Stopped);
}

/// 저장소의 스펙을 `f`로 고쳐 `PUT /vms/{id}`로 보낸다: (상태, 응답 헤더 `orbivisor-task`, 본문)
async fn put(
    env: &Env,
    admin: &Client,
    id: Uuid,
    f: impl Fn(&mut VmSpec),
) -> (StatusCode, Option<String>, Value) {
    let current = vm(env, id);
    let mut spec = current.spec.clone();
    f(&mut spec);
    admin
        .put_task(
            &format!("/vms/{id}"),
            json!({ "labels": current.labels, "spec": spec }),
        )
        .await
}

/// 하드웨어를 바꾼 PUT: 200과 핫플러그 Task(끝까지 기다린 모습)
async fn put_hot(env: &Env, admin: &Client, id: Uuid, f: impl Fn(&mut VmSpec)) -> Value {
    let (status, task, body) = put(env, admin, id, f).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = task.expect("no orbivisor-task header");
    admin.wait_task(&json!({ "id": task })).await
}

/// 409와 그 코드·메시지
async fn put_conflict(
    env: &Env,
    admin: &Client,
    id: Uuid,
    f: impl Fn(&mut VmSpec),
) -> (String, String) {
    let (status, task, body) = put(env, admin, id, f).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(task, None);
    (
        body["code"].as_str().unwrap().to_string(),
        body["message"].as_str().unwrap().to_string(),
    )
}

/// 이 노드가 `hotplug`를 알리는지 정한다(옛 노드 흉내).
async fn set_feature(env: &Env, on: bool) {
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status.features.retain(|f| f != HOTPLUG);
    if on {
        n.status.features.push(HOTPLUG.into());
    }
    env.cluster
        .update_status::<NodeSpec>(env.node, &n.status)
        .await
        .unwrap();
}

/// 실행 중인 VM의 cores를 상한 안에서 바꾸면 PUT이 핫플러그 Task를 만들고 그 id를 헤더로 준다. 상한 밖은 검증
/// 400이거나(상한을 넘는 cores) 보류다.
#[tokio::test]
async fn put_applies_vcpus_while_running() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    let task = put_hot(&env, &admin, id, |s| s.cpu.cores = 4).await;
    succeeded(&task);
    assert_eq!(task["spec"]["op"]["kind"], "vm-hotplug", "{task}");
    assert_eq!(env.hv.vcpus(id), Some(4));
    assert!(hardware(&env, id).pending.is_empty());

    let (status, _, body) = put(&env, &admin, id, |s| s.cpu.cores = 9).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["message"],
        "cpu.max_vcpus must be at least cpu.sockets × cpu.cores (9)"
    );

    // 상한을 올리면 켤 때 상한 밖의 vCPU와 새 상한이 다음 재시작 때 적용된다.
    let task = put_hot(&env, &admin, id, |s| {
        s.cpu.cores = 9;
        s.cpu.max_vcpus = Some(16);
    })
    .await;
    succeeded(&task);
    assert_eq!(env.hv.vcpus(id), Some(4));
    let pending = hardware(&env, id).pending;
    let reason = |field: &str| pending.iter().find(|p| p.field == field).map(|p| p.reason);
    assert_eq!(
        reason("cpu.max_vcpus"),
        Some(PendingReason::Restart),
        "{pending:?}"
    );
    assert_eq!(reason("vcpus"), Some(PendingReason::Limit), "{pending:?}");
    let warning = task["status"]["error"].as_str().unwrap();
    assert!(warning.starts_with("pending until restart: "), "{warning}");
}

/// 소켓·펌웨어처럼 켤 때만 정해지는 것은 Task가 성공하고 경고와 `restart` 보류로 남는다.
#[tokio::test]
async fn put_leaves_restart_changes_pending() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    let task = put_hot(&env, &admin, id, |s| s.cpu.sockets = 2).await;
    succeeded(&task);
    let hw = hardware(&env, id);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    assert_eq!(hw.pending[0].field, "cpu");
    assert_eq!(hw.pending[0].reason, PendingReason::Restart);

    let task = put_hot(&env, &admin, id, |s| {
        s.cpu.sockets = 1;
        s.firmware = opl_types::vm::Firmware::Bios;
    })
    .await;
    succeeded(&task);
    assert_eq!(
        task["status"]["error"],
        "pending until restart: firmware (restart)"
    );
    let hw = hardware(&env, id);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    assert_eq!(hw.pending[0].field, "firmware");
}

/// 메모리를 바로 늘리려면 VM이 도는 노드에 그만큼 미할당 메모리가 있어야 한다.
#[tokio::test]
async fn put_needs_unallocated_memory_to_grow_memory() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let mut n = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    n.status.memory_mib = 2048;
    env.cluster
        .update_status::<NodeSpec>(env.node, &n.status)
        .await
        .unwrap();

    let (code, message) = put_conflict(&env, &admin, id, |s| s.memory_mib = 4096).await;
    assert_eq!(code, "no_capacity");
    assert!(
        message.contains("has only 1024 MiB of unallocated memory"),
        "{message}"
    );
    assert_eq!(vm(&env, id).spec.memory_mib, 1024);

    succeeded(&put_hot(&env, &admin, id, |s| s.memory_mib = 2048).await);
    assert_eq!(env.hv.plugged_mib(id), Some(1024));
}

/// 디스크는 끝에 붙이고 실행 중 붙인 것만 뗀다. 나머지 디스크 변경은 409 `vm_running`이다.
#[tokio::test]
async fn put_adds_and_removes_disks_at_the_end_only() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    succeeded(&put_hot(&env, &admin, id, |s| s.disks.push(disk("data"))).await);
    assert!(env.hv.devices(id).contains(&"disk1-dev".to_string()));
    succeeded(&put_hot(&env, &admin, id, |s| s.disks.push(disk("extra"))).await);

    let refused = [
        (
            Box::new(|s: &mut VmSpec| s.disks.clear()) as Box<dyn Fn(&mut VmSpec)>,
            "cannot remove disk \"boot\" of VM \"web\" while it is running; only disks and NICs added at the end can be removed while it runs; stop it first",
        ),
        (
            Box::new(|s: &mut VmSpec| {
                s.disks.remove(1);
            }),
            "cannot remove disk \"data\" from the middle of VM \"web\" while it is running; only disks and NICs added at the end can be removed while it runs; stop it first",
        ),
        (
            Box::new(|s: &mut VmSpec| s.disks.swap(1, 2)),
            "cannot reorder the disks of VM \"web\" while it is running; only disks and NICs added at the end can be removed while it runs; stop it first",
        ),
    ];
    for (f, message) in refused {
        let (code, got) = put_conflict(&env, &admin, id, f).await;
        assert_eq!((code.as_str(), got.as_str()), ("vm_running", message));
    }
    // SATA 디스크는 루트 포트에 붙이지 않는다.
    succeeded(
        &put_hot(&env, &admin, id, |s| {
            s.disks.pop();
        })
        .await,
    );
    let (code, message) = put_conflict(&env, &admin, id, |s| {
        s.disks.push(DiskSpec {
            bus: DiskBus::Sata,
            ..disk("extra")
        })
    })
    .await;
    assert_eq!(code, "vm_running");
    assert!(
        message.starts_with("SATA disks cannot be added while the VM is running"),
        "{message}"
    );

    // 실행 중 붙인 것은 뗀다.
    succeeded(
        &put_hot(&env, &admin, id, |s| {
            s.disks.pop();
        })
        .await,
    );
    assert!(!env.hv.devices(id).contains(&"disk1-dev".to_string()));
    assert_eq!(vm(&env, id).spec.disks, [disk("boot")]);
}

/// 루트 포트 8개를 다 쓰면 더 붙이지 못한다. NIC의 모델·네트워크 바꾸기는 409 `vm_running`(전에는 다음 시작부터)이고,
/// 장치가 아닌 것(레이블·재시작 정책)만 바꾸면 Task가 없다.
#[tokio::test]
async fn put_refuses_nic_changes_and_a_ninth_device() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    let (code, message) = put_conflict(&env, &admin, id, |s| {
        s.nics[0].model = NicModel::E1000e;
    })
    .await;
    assert_eq!(code, "vm_running");
    assert!(
        message.starts_with("cannot change NIC 0 of VM \"web\" while it is running"),
        "{message}"
    );

    succeeded(
        &put_hot(&env, &admin, id, |s| {
            s.nics.extend((10..18).map(user_nic));
        })
        .await,
    );
    assert_eq!(hardware(&env, id).layout.unwrap().nic_ports.len(), 8);
    let (code, message) = put_conflict(&env, &admin, id, |s| s.disks.push(disk("data"))).await;
    assert_eq!(code, "hotplug_unavailable");
    assert_eq!(
        message,
        "no free hotplug port (8 in use); restart VM \"web\" to add more disks or NICs while it runs"
    );

    let (status, task, body) = put(&env, &admin, id, |s| {
        s.restart = opl_types::vm::RestartPolicy::OnFailure;
    })
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
}

/// 핫플러그 없이 켠 VM(기능이 모자랄 때 켬)은 디스크·NIC를 실행 중에 바꾸지 못한다. 기능이 모자란 동안은 지금처럼
/// 실행 중 cores 변경을 스펙만 받고(Task 없음) 디스크 변경은 409다.
#[tokio::test]
async fn put_without_hotplug_keeps_the_old_rules() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    set_feature(&env, false).await;
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "boot", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    succeeded(&admin.act("web", "start").await);
    let id = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(hardware(&env, id).layout, None);

    let (status, task, body) = put(&env, &admin, id, |s| s.cpu.cores = 2).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
    let (code, message) = put_conflict(&env, &admin, id, |s| s.disks.push(disk("data"))).await;
    assert_eq!(code, "vm_running");
    assert_eq!(
        message,
        "cannot change the disks of VM \"web\" while it is running; stop it first"
    );
    // 도는 하드웨어 기록이 있으면 그룹 없는 NIC의 모델·네트워크도 바꾸지 않는다(마이그레이션 대상이 그 NIC로 켠다).
    let (code, message) =
        put_conflict(&env, &admin, id, |s| s.nics[0].model = NicModel::E1000e).await;
    assert_eq!(code, "vm_running");
    assert_eq!(
        message,
        "cannot change the NICs of VM \"web\" while it is running; only security groups can change"
    );

    set_feature(&env, true).await;
    let (code, message) = put_conflict(&env, &admin, id, |s| s.disks.push(disk("data"))).await;
    assert_eq!(code, "hotplug_unavailable");
    assert_eq!(
        message,
        "VM \"web\" was started without hotplug support; restart it once to add or remove disks and NICs while it runs"
    );
}

/// 켜는 중인 VM의 하드웨어는 바꾸지 못하고(409 `vm_busy`), 꺼진 VM은 지금처럼 Task 없이 바뀐다.
#[tokio::test]
async fn put_waits_for_a_settled_vm() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "cores": 2, "max_vcpus": 8 });
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().parse().unwrap();

    let (status, task, body) = put(&env, &admin, id, |s| {
        s.cpu.cores = 3;
        s.disks.push(disk("data"));
    })
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);

    env.hv.hold_next_start();
    let (status, start) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{start}");
    tokio::time::timeout(WAIT, env.hv.start_held())
        .await
        .expect("start reached");
    let (code, message) = put_conflict(&env, &admin, id, |s| s.cpu.cores = 4).await;
    assert_eq!(code, "vm_busy");
    assert_eq!(
        message,
        "VM \"web\" is starting or stopping; change its hardware when it is running or stopped"
    );
    env.hv.release_start();
    succeeded(&admin.wait_task(&start).await);
}

/// 끝나지 않은 핫플러그 Task가 있으면 하드웨어 PUT은 409 `vm_busy`다(실패한 Task의 되돌리기가 이 변경을 지우지 않게).
/// 하드웨어가 아닌 변경은 받는다.
#[tokio::test]
async fn put_waits_for_the_hotplug_task() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let hot = stuck_task(&env, TaskOp::VmHotplug, id).await;

    let (code, message) = put_conflict(&env, &admin, id, |s| s.cpu.cores = 4).await;
    assert_eq!(code, "vm_busy");
    assert_eq!(
        message,
        format!(
            "a hardware change of VM \"web\" is still being applied (task {}); try again when it finishes",
            hot.id
        )
    );
    let (status, task, body) = put(&env, &admin, id, |s| s.restart = Default::default()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
}

/// 같은 PUT으로 끄는 VM의 하드웨어 변경은 Task 없이 다음 시작부터다(끄는 중인 QEMU를 바꾸지 않는다).
#[tokio::test]
async fn put_that_stops_the_vm_changes_hardware_for_the_next_start() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;

    let (status, task, body) = put(&env, &admin, id, |s| {
        s.power = PowerState::Stopped;
        s.cpu.cores = 4;
        s.disks.push(disk("data"));
    })
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
    let spec = vm(&env, id).spec;
    assert_eq!((spec.cpu.cores, spec.disks.len()), (4, 2));
    assert!(env.hv.hotplug_calls().is_empty());
}

/// 켜져 있어야 하지만 QEMU가 없는 VM(배정된 노드가 아직 켜지 않음 — 실패·다시 켜기 대기와 같음)은 지금 규칙대로
/// CPU·메모리 변경을 Task 없이 받고 디스크 변경은 409 `vm_running`이다.
#[tokio::test]
async fn put_on_a_vm_without_qemu_keeps_the_old_rules() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let node2 = second_node(&env, ALL).await;
    let mut req = vm_request("web");
    req["spec"]["cpu"] = json!({ "cores": 2, "max_vcpus": 8 });
    req["spec"]["disks"] = json!([{ "volume": "boot", "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(node2);
    req["spec"]["power"] = json!("running");
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(vm(&env, id).status.phase, VmPhase::Stopped);

    let (status, task, body) = put(&env, &admin, id, |s| {
        s.cpu.cores = 4;
        s.memory_mib = 512;
    })
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
    let (code, _) = put_conflict(&env, &admin, id, |s| s.disks.push(disk("data"))).await;
    assert_eq!(code, "vm_running");
}

/// `hotplug`를 모르는 노드가 있는 동안 핫플러그 배치로 도는 VM은 스냅샷에 하드웨어를 적지 못하므로 메모리 스냅샷이
/// 409이고, 메모리를 주지 않으면 디스크만 담는다.
#[tokio::test]
async fn memory_snapshots_need_every_node_to_record_the_layout() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    running_web(&env, &admin).await;
    set_feature(&env, false).await;

    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert_eq!(
        body["message"],
        "VM \"web\" runs with hotplug support, which cannot be recorded in a snapshot while a node lacks it; take a snapshot without memory or upgrade every node first"
    );
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "d" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let (_, snapshot) = admin.get("/vms/web/snapshots/d").await;
    assert_eq!(snapshot["spec"]["memory"], false, "{snapshot}");
}

/// 실행 중에 바꾼 하드웨어가 있으면 메모리를 주지 않은 스냅샷은 디스크만 담고 메모리 스냅샷은 409다. 다시 켜면
/// 메모리를 담고 그때의 하드웨어를 스냅샷에 적는다.
#[tokio::test]
async fn memory_snapshots_need_the_hardware_as_booted() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    succeeded(&put_hot(&env, &admin, id, |s| s.cpu.cores = 4).await);

    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert_eq!(
        body["message"],
        "VM \"web\" has hardware changed while running or pending changes; restart it to take a snapshot with memory"
    );
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "d" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let (_, snapshot) = admin.get("/vms/web/snapshots/d").await;
    assert_eq!(snapshot["spec"]["memory"], false, "{snapshot}");

    succeeded(&admin.act("web", "stop?mode=force").await);
    succeeded(&admin.act("web", "start").await);
    let booted = hardware(&env, id);
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "after" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let (_, snapshot) = admin.get("/vms/web/snapshots/after").await;
    assert_eq!(snapshot["spec"]["memory"], true, "{snapshot}");
    assert_eq!(
        snapshot["spec"]["hardware"],
        serde_json::to_value(&booted).unwrap()
    );
}

/// 게스트가 아직 내려 주지 않은 디스크·NIC가 있는 동안 디스크·NIC 목록을 바꾸는 PUT은 409 `vm_busy`다(CPU·메모리만
/// 바꾸는 PUT은 받는다).
#[tokio::test]
async fn put_waits_for_devices_the_guest_still_holds() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    succeeded(&put_hot(&env, &admin, id, |s| s.disks.push(disk("data"))).await);
    env.hv.hold_unplug(id, true);
    failed(
        &put_hot(&env, &admin, id, |s| {
            s.disks.pop();
        })
        .await,
    );
    assert_eq!(hardware(&env, id).unplugging, ["disk1-dev"]);

    let (code, message) = put_conflict(&env, &admin, id, |s| s.disks.push(disk("data"))).await;
    assert_eq!(code, "vm_busy");
    assert_eq!(
        message,
        "the guest is still releasing disk \"data\" from VM \"web\"; run `vm hotplug` first"
    );
    let (status, task, body) = put(&env, &admin, id, |s| s.cpu.cores = 4).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(task.is_some());
}

/// 끄면서 하드웨어를 바꾸는 PUT도 끝나지 않은 핫플러그 Task를 기다린다.
#[tokio::test]
async fn put_that_stops_the_vm_waits_for_the_hotplug_task() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    stuck_task(&env, TaskOp::VmHotplug, id).await;
    let (code, _) = put_conflict(&env, &admin, id, |s| {
        s.power = PowerState::Stopped;
        s.cpu.cores = 4;
    })
    .await;
    assert_eq!(code, "vm_busy");
    assert_eq!(vm(&env, id).spec.power, PowerState::Running);
}

/// 디스크·NIC를 바꾸며 끈 VM은 QEMU가 끝나기 전에 다시 켜지 않는다(도는 QEMU를 이어 쓰면 스펙과 장치가 어긋난다).
#[tokio::test]
async fn a_vm_stopping_with_changed_devices_starts_after_it_stops() {
    // 게스트가 ACPI 끄기를 무시하는 동안 QEMU가 남는다.
    let (env, _) = setup_with(ALL, |c| c.acpi_timeout = Duration::from_secs(3600)).await;
    let admin = env.admin().await;
    let id = running_web(&env, &admin).await;
    let (status, task, body) = put(&env, &admin, id, |s| {
        s.power = PowerState::Stopped;
        s.disks.push(disk("data"));
    })
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(task, None);
    assert!(vm(&env, id).status.hardware.is_some());

    let busy = "VM \"web\" is stopping with changed devices; start it after it stops";
    let (code, message) = put_conflict(&env, &admin, id, |s| s.power = PowerState::Running).await;
    assert_eq!((code.as_str(), message.as_str()), ("vm_busy", busy));
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["message"], busy);

    env.hv.guest_shutdown(id);
    let deadline = tokio::time::Instant::now() + WAIT;
    while vm(&env, id).status.hardware.is_some() {
        assert!(tokio::time::Instant::now() < deadline, "not stopped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    succeeded(&admin.act("web", "start").await);
    assert_eq!(hardware(&env, id).disks, [disk("boot"), disk("data")]);
}

/// 풀 `local`의 Ready ISO 볼륨(raw)을 만들고 id를 돌려준다.
async fn iso(env: &Env, name: &str) -> Uuid {
    let mut volume: Volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
            content: VolumeContent::Iso,
            base: None,
        },
    );
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster.create(&volume).await.unwrap();
    volume.id
}

/// 도는 QEMU의 CD 매체마다 그 파일 이름(빈 드라이브는 None)
fn media(env: &Env, id: Uuid) -> Vec<Option<String>> {
    env.hv
        .media(id)
        .into_iter()
        .map(|m| m.map(|p| p.file_name().unwrap().to_string_lossy().into_owned()))
        .collect()
}

/// `POST /vms/web/cdrom`과 응답 헤더 `orbivisor-task`
async fn cdrom(admin: &Client, body: Value) -> (StatusCode, Option<String>, Value) {
    let (status, headers, body) = admin
        .send_headers(reqwest::Method::POST, "/vms/web/cdrom", body)
        .await;
    let task = headers
        .get("orbivisor-task")
        .map(|v| v.to_str().unwrap().to_string());
    (status, task, body)
}

/// 켠 VM `web`: 디스크 `boot`, CD 드라이브 둘(`ubuntu`와 빈 드라이브)
async fn running_with_cdroms(admin: &Client) -> Uuid {
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "boot", "bus": "virtio-blk" }]);
    req["spec"]["cdroms"] = json!([{ "iso": "ubuntu" }, {}]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    succeeded(&admin.act("web", "start").await);
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 실행 중에도 CD 매체를 넣고 뺀다(vCPU·메모리 핫플러그가 없는 노드에서도): `POST /vms/{key}/cdrom`이 스펙의 매체만 바꾸고 핫플러그
/// Task를 만들며(헤더 `orbivisor-task`), Task가 QEMU의 매체와 기록을 바꾼다. 감사 설명은 `vm cdrom …`이다.
#[tokio::test]
async fn cdrom_media_change_while_running() {
    let (env, _) = setup(NodeHotplug::default()).await;
    let admin = env.admin().await;
    let ubuntu = iso(&env, "ubuntu").await;
    let alpine = iso(&env, "alpine").await;
    let id = running_with_cdroms(&admin).await;
    assert_eq!(media(&env, id), [Some(format!("{ubuntu}.raw")), None]);

    let (status, task, vm) = cdrom(&admin, json!({ "drive": 1, "iso": "alpine" })).await;
    assert_eq!(status, StatusCode::OK, "{vm}");
    assert_eq!(
        vm["spec"]["cdroms"],
        json!([{ "iso": "ubuntu" }, { "iso": "alpine" }])
    );
    succeeded(&admin.wait_task(&json!({ "id": task.unwrap() })).await);
    assert_eq!(
        media(&env, id),
        [Some(format!("{ubuntu}.raw")), Some(format!("{alpine}.raw"))]
    );
    let hw = hardware(&env, id);
    assert_eq!(hw.cdroms[1].iso.as_deref(), Some("alpine"));
    assert!(hw.pending.is_empty(), "{hw:?}");

    // 빼기(드라이브 번호는 기본 0)
    let (status, task, _) = cdrom(&admin, json!({ "iso": null })).await;
    assert_eq!(status, StatusCode::OK);
    succeeded(&admin.wait_task(&json!({ "id": task.unwrap() })).await);
    assert_eq!(media(&env, id), [None, Some(format!("{alpine}.raw"))]);
    assert_eq!(hardware(&env, id).cdroms[0].iso, None);
    let (status, body) = admin.post("/vms/web/cdrom", json!({ "drive": 5 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "VM \"web\" has no CD-ROM drive 5");

    // 감사 기록은 쓰기 스레드가 곧 적는다(새것부터 읽힌다).
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let entries = env
            .state
            .audit
            .read(opl_types::audit::AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        let details: Vec<String> = entries
            .iter()
            .filter(|e| e.action == "POST /vms/{key}/cdrom" && e.status == 200)
            .filter_map(|e| e.detail.clone())
            .collect();
        if details.len() == 2 || std::time::Instant::now() > deadline {
            assert_eq!(
                details,
                [
                    "vm cdrom web drive 0: eject",
                    "vm cdrom web drive 1: insert alpine"
                ]
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 실행 중에는 CD 드라이브를 더하거나 빼지 못하고(409 `vm_running`), CD 부팅 순서는 다음 재시작 때 적용된다. 매체를
/// 넣지 못하면 Task가 실패하고 기록은 그대로이며 보류 `cdroms[i]`(failed)로 남아 다시 해 보기가 마저 넣는다.
#[tokio::test]
async fn cdrom_drives_and_boot_order_wait_for_a_restart() {
    let (env, _) = setup(NodeHotplug::default()).await;
    let admin = env.admin().await;
    iso(&env, "ubuntu").await;
    let alpine = iso(&env, "alpine").await;
    let id = running_with_cdroms(&admin).await;
    let (_, vm) = admin.get("/vms/web").await;

    let mut spec = vm["spec"].clone();
    spec["cdroms"] = json!([{ "iso": "ubuntu" }, {}, {}]);
    let (status, _, body) = admin.put_task("/vms/web", json!({ "spec": spec })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_running");
    assert_eq!(
        body["message"],
        "cannot add or remove CD-ROM drives of VM \"web\" while it is running; change the media instead or stop it first"
    );

    let mut spec = vm["spec"].clone();
    spec["cdroms"][1]["boot_order"] = json!(5);
    let (status, task, body) = admin.put_task("/vms/web", json!({ "spec": spec })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    succeeded(&admin.wait_task(&json!({ "id": task.unwrap() })).await);
    let hw = hardware(&env, id);
    assert_eq!(hw.pending.len(), 1, "{hw:?}");
    assert_eq!(hw.pending[0].field, "cdroms[1].boot_order");
    assert_eq!(hw.pending[0].reason, PendingReason::Restart);

    env.hv.fail_next_hotplug("tray is stuck");
    let (status, task, _) = cdrom(&admin, json!({ "drive": 1, "iso": "alpine" })).await;
    assert_eq!(status, StatusCode::OK);
    let error = failed(&admin.wait_task(&json!({ "id": task.unwrap() })).await);
    assert!(error.contains("CD-ROM drive 1 of VM \"web\""), "{error}");
    assert!(error.contains("tray is stuck"), "{error}");
    assert_eq!(media(&env, id)[1], None);
    let hw = hardware(&env, id);
    assert_eq!(hw.cdroms[1].iso, None);
    // 스펙은 바꾼 그대로고, 매체 바꾸기 중에는 옮기지 않는다(기록과 다름).
    assert_eq!(vm_spec_iso(&env, id, 1).as_deref(), Some("alpine"));
    let failed_change = hw
        .pending
        .iter()
        .find(|p| p.field == "cdroms[1]")
        .unwrap_or_else(|| panic!("{hw:?}"));
    assert_eq!(
        (
            failed_change.running.as_str(),
            failed_change.wanted.as_str()
        ),
        ("empty", "alpine")
    );
    assert_eq!(failed_change.reason, PendingReason::Failed);
    // 부팅 순서 보류는 남는다(매체만 바꿨다).
    assert!(hw.pending.iter().any(|p| p.field == "cdroms[1].boot_order"));

    succeeded(&admin.act("web", "hotplug").await);
    assert_eq!(media(&env, id)[1], Some(format!("{alpine}.raw")));
    let hw = hardware(&env, id);
    assert_eq!(hw.cdroms[1].iso.as_deref(), Some("alpine"));
    assert_eq!(hw.cdroms[1].boot_order, None);
    assert!(hw.pending.iter().all(|p| p.field != "cdroms[1]"), "{hw:?}");
}

fn vm_spec_iso(env: &Env, id: Uuid, drive: usize) -> Option<String> {
    vm(env, id).spec.cdroms[drive].iso.clone()
}

/// 실행 중 붙이는 디스크의 `boot_order`가 도는 QEMU의 CD `bootindex`(값을 주지 않아 디스크 뒤에 매겨진 것)와 겹치면
/// 409 `vm_running`이다(QEMU `device_add`가 겹치는 `bootindex`를 거절한다). 겹치지 않으면 붙는다.
#[tokio::test]
async fn a_hot_disk_boot_order_must_not_take_a_running_cdrom_bootindex() {
    let (env, _) = setup(ALL).await;
    let admin = env.admin().await;
    iso(&env, "ubuntu").await;
    let id = running_with_cdroms(&admin).await;

    // 켤 때 실제 순서: disk0 1, cdrom0 2, cdrom1 3
    let (code, message) = put_conflict(&env, &admin, id, |s| {
        s.disks.push(DiskSpec {
            boot_order: Some(2),
            ..disk("data")
        })
    })
    .await;
    assert_eq!(code, "vm_running");
    assert_eq!(
        message,
        "boot_order 2 is already used by cdrom0 in the running VM \"web\"; pick another or set it after a restart"
    );
    succeeded(
        &put_hot(&env, &admin, id, |s| {
            s.disks.push(DiskSpec {
                boot_order: Some(9),
                ..disk("data")
            })
        })
        .await,
    );
    assert!(env.hv.devices(id).contains(&"disk1-dev".to_string()));
}
