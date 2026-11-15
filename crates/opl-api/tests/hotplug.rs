//! 핫플러그 Task(`vm-hotplug`, 계획 54): 실행 중인 VM의 vCPU·메모리·디스크·NIC를 Mock 하이퍼바이저로 바꾸고 실행 중
//! 하드웨어 기록(`status.hardware`)과 보류를 남기며, 실패하면 스펙의 장치 목록을 되돌린다. `actions/hotplug`의 거절,
//! 마이그레이션과의 배타, VM 줄. 스펙은 저장소에 바로 쓴다(`PUT`의 실행 중 적용은 스테이지 6).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, WAIT, vm_request};
use opl_types::node::{HOTPLUG, NodeHotplug, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
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
    let mut env = Env::start().await;
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
            },
        );
        volume.status.phase = VolumePhase::Ready;
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
