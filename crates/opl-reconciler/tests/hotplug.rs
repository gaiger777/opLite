//! 켤 때의 핫플러그 배치와 실행 중 하드웨어 기록(`status.hardware`, 계획 54).

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::Hypervisor;
use opl_types::node::{HOTPLUG, Node, NodeHotplug, NodeSpec, PLACEMENT};
use opl_types::resource::Resource;
use opl_types::runtime::{HOTPLUG_PORTS, VmRuntimeSpec};
use opl_types::vm::{
    MacAddr, NicModel, NicSpec, PendingChange, PendingReason, PowerState, RestartPolicy,
    RunningHardware, Vm, VmPhase, VmSpec,
};
use uuid::Uuid;

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

/// 노드 기록을 만든다(이 노드면 `id`가 env의 노드). `features`와 핫플러그 능력을 시험이 정한다.
async fn add_node(env: &Env, id: Uuid, features: &[&str], hotplug: Option<NodeHotplug>) {
    let mut node: Node = Resource::new(
        format!("n-{id}"),
        NodeSpec {
            address: "a".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = id;
    node.status.features = features.iter().map(|f| f.to_string()).collect();
    node.status.hotplug = hotplug;
    env.cluster.create(&node).await.unwrap();
}

const ALL: NodeHotplug = NodeHotplug {
    cpu: true,
    memory: true,
};

/// 상한이 있는 VM(1×2 vCPU, 최대 8 / 1024 MiB, 최대 4096)과 NIC 하나
fn hot_vm(node: Uuid) -> Vm {
    let mut vm = vm("web", node);
    vm.spec.cpu.cores = 2;
    vm.spec.cpu.max_vcpus = Some(8);
    vm.spec.memory_mib = 1024;
    vm.spec.max_memory_mib = Some(4096);
    vm.spec.nics.push(NicSpec {
        network: "user".into(),
        model: NicModel::Virtio,
        mac: Some(MacAddr::generate()),
        security_groups: Vec::new(),
    });
    vm
}

/// 이 VM을 마지막으로 띄운 런타임 스펙
fn last_launch(env: &Env, id: Uuid) -> VmRuntimeSpec {
    env.hv
        .launch_specs()
        .into_iter()
        .rfind(|s| s.id == id)
        .expect("VM was not launched")
}

/// 실행 중 하드웨어 기록을 바꾼다(핫플러그 Task 대신). 작업자의 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_hardware(
    env: &Env,
    id: Uuid,
    change: impl Fn(&mut RunningHardware),
) -> RunningHardware {
    loop {
        let vm = env.get(id);
        let mut status = vm.status.clone();
        change(status.hardware.as_mut().expect("no running hardware"));
        match env
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return status.hardware.unwrap(),
            Err(opl_cluster::ClusterError::Rejected(_)) => continue,
            Err(e) => panic!("{e}"),
        }
    }
}

/// 모든 노드가 `hotplug`면 켤 때 빈 루트 포트·vCPU 자리·virtio-mem을 둔 배치로 켜고, 그 모양을 `status.hardware`에
/// 남긴다. 끄면 기록이 없어진다.
#[tokio::test]
async fn starts_with_a_hotplug_layout_when_every_node_has_hotplug() {
    let env = Env::start().await;
    add_node(&env, env.node, &[PLACEMENT, HOTPLUG], Some(ALL)).await;
    let vm = hot_vm(env.node);
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "running with hardware", |v| {
            running(v) && v.status.hardware.is_some()
        })
        .await;
    let spec = last_launch(&env, vm.id);
    let layout = spec.hotplug.clone().expect("no hotplug layout");
    assert_eq!(layout.ports, HOTPLUG_PORTS);
    assert_eq!(layout.vcpus, 2);
    assert_eq!(layout.max_memory_mib, 4096);
    assert_eq!(layout.plugged_mib, 0);
    assert_eq!((layout.boot_disks, layout.boot_nics), (0, 1));
    assert_eq!(spec.cpu.max_vcpus, Some(8));
    assert_eq!(spec.memory_mib, 1024);
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.layout, Some(layout));
    assert_eq!(hw.cpu, vm.spec.cpu);
    assert_eq!(hw.memory_mib, 1024);
    assert_eq!(hw.nics, vm.spec.nics);
    assert!(hw.pending.is_empty());
    assert_eq!(env.hv.vcpus(vm.id), Some(2));

    env.set_power(vm.id, PowerState::Stopped).await;
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.hardware, None);
}

/// 한 노드라도 `hotplug`를 알리지 않으면(롤링 업그레이드 중) 배치 없이 옛 명령줄로 켠다(옛 노드로 옮길 수 있게).
#[tokio::test]
async fn starts_without_a_layout_while_a_node_lacks_hotplug() {
    let env = Env::start().await;
    add_node(&env, env.node, &[PLACEMENT, HOTPLUG], Some(ALL)).await;
    add_node(&env, Uuid::now_v7(), &[PLACEMENT], None).await;
    let vm = hot_vm(env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    let spec = last_launch(&env, vm.id);
    assert_eq!(spec.hotplug, None);
    assert_eq!(spec.cpu.max_vcpus, None);
    assert_eq!(got.status.hardware, Some(RunningHardware::legacy(&vm.spec)));
    assert_eq!(got.status.hardware.unwrap().layout, None);
}

/// 이 노드가 CPU 핫플러그를 못 하면 vCPU 상한을 두지 않고, virtio-mem이 없으면 메모리 상한이 켤 때 메모리다.
#[tokio::test]
async fn node_capabilities_limit_the_layout() {
    let env = Env::start().await;
    add_node(&env, env.node, &[HOTPLUG], Some(NodeHotplug::default())).await;
    let vm = hot_vm(env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    let spec = last_launch(&env, vm.id);
    assert_eq!(spec.cpu.max_vcpus, None);
    let layout = spec.hotplug.expect("no hotplug layout");
    assert_eq!(layout.max_memory_mib, 1024);
    assert_eq!(layout.ports, HOTPLUG_PORTS);
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.cpu.max_vcpus, None);
    assert_eq!(hw.max_memory_mib(), 1024);
}

/// 켜는 동안(QEMU를 띄운 뒤 기록하기 전) 바뀐 vCPU·메모리는 기록의 보류(`restart`)로 남는다 — 그동안 API는 QEMU가
/// 없는 VM으로 보고 핫플러그 Task 없이 받았다.
#[tokio::test]
async fn changes_made_while_starting_are_left_pending() {
    let env = Env::start().await;
    add_node(&env, env.node, &[PLACEMENT, HOTPLUG], Some(ALL)).await;
    let vm = hot_vm(env.node);
    env.hv.hold_next_start();
    env.create(&vm).await;
    tokio::time::timeout(common::WAIT, env.hv.start_held())
        .await
        .expect("start reached");
    loop {
        let now = env.get(vm.id);
        let mut spec = now.spec.clone();
        spec.cpu.cores = 4;
        if env
            .cluster
            .update_spec(vm.id, Some(now.revision), now.labels.clone(), &spec)
            .await
            .is_ok()
        {
            break;
        }
    }
    env.hv.release_start();
    let got = env
        .wait(vm.id, "running with hardware", |v| {
            running(v) && v.status.hardware.is_some()
        })
        .await;
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.layout.as_ref().unwrap().vcpus, 2);
    assert_eq!(
        hw.pending,
        [PendingChange {
            field: "vcpus".into(),
            running: "2".into(),
            wanted: "4".into(),
            reason: PendingReason::Restart,
            detail: Some(
                "changed while the VM was starting; restart it or run `vm hotplug`".into()
            ),
        }]
    );
}

/// 크래시 뒤 다시 켜면 하드웨어를 새로 정한다: 앞 QEMU의 보류·바꾼 vCPU는 사라진다(다시 켜며 적용됐다).
#[tokio::test]
async fn a_restart_after_a_crash_records_new_hardware() {
    let env = Env::start().await;
    add_node(&env, env.node, &[HOTPLUG], Some(ALL)).await;
    let mut vm = hot_vm(env.node);
    vm.spec.restart = RestartPolicy::OnFailure;
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await;

    set_hardware(&env, vm.id, |hw| {
        hw.layout.as_mut().unwrap().vcpus = 4;
        hw.pending.push(PendingChange {
            field: "firmware".into(),
            running: "uefi".into(),
            wanted: "bios".into(),
            reason: PendingReason::Restart,
            detail: None,
        });
    })
    .await;
    env.hv.crash(vm.id);

    let got = env
        .wait(vm.id, "restarted", |v| {
            running(v) && v.status.pid != before.status.pid && v.status.hardware.is_some()
        })
        .await;
    let hw = got.status.hardware.unwrap();
    assert!(hw.pending.is_empty(), "{hw:?}");
    assert_eq!(hw.layout.unwrap().vcpus, 2);
    assert_eq!(last_launch(&env, vm.id).hotplug.unwrap().vcpus, 2);
}

/// reconciler를 멈추고 VM의 실행 중 하드웨어 기록을 지운다(기록 없이 도는 VM).
async fn clear_hardware(env: &mut Env, id: Uuid) {
    env.stop_reconciler().await;
    let current = env.get(id);
    let mut status = current.status.clone();
    status.hardware = None;
    env.cluster
        .update_status_checked::<VmSpec>(id, current.revision, &status)
        .await
        .unwrap();
}

/// opld를 다시 띄워 도는 VM에 다시 붙으면 기록을 그대로 둔다(핫플러그 Task가 고친 값 포함). 기록이 없는 도는
/// VM은 켤 때 실행 디렉터리에 남긴 기록으로, 그것도 없으면(계획 54 전에 켬) 배치 없는 기록을 남긴다.
#[tokio::test]
async fn reattaching_keeps_the_record_or_writes_a_legacy_one() {
    let mut env = Env::start().await;
    add_node(&env, env.node, &[HOTPLUG], Some(ALL)).await;
    let vm = hot_vm(env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    let changed = set_hardware(&env, vm.id, |hw| {
        hw.layout.as_mut().unwrap().vcpus = 4;
    })
    .await;

    env.stop_reconciler().await;
    env.start_reconciler();
    // 새 reconciler가 첫 동기화를 마쳤는지는 새 VM이 뜨는 것으로 확인한다.
    let probe = common::vm("probe", env.node);
    env.create(&probe).await;
    env.wait(probe.id, "probe running", running).await;
    assert_eq!(env.get(vm.id).status.hardware, Some(changed));
    assert_eq!(
        env.hv
            .launch_specs()
            .iter()
            .filter(|s| s.id == vm.id)
            .count(),
        1
    );

    // 기록 없이 도는 VM은 켤 때 실행 디렉터리에 남긴 기록(배치 포함)으로 다시 붙는다.
    clear_hardware(&mut env, vm.id).await;
    env.start_reconciler();
    let got = env
        .wait(vm.id, "saved record", |v| v.status.hardware.is_some())
        .await;
    let saved = env.hv.saved_hardware(vm.id).expect("saved at start");
    assert_eq!(got.status.hardware, Some(saved.clone()));
    assert_eq!(saved.layout.unwrap().vcpus, 2);

    // 그 기록도 없으면(계획 54 전에 켬) 배치 없는 기록이다.
    clear_hardware(&mut env, vm.id).await;
    env.hv.forget_saved_hardware(vm.id);
    env.start_reconciler();
    let got = env
        .wait(vm.id, "legacy record", |v| v.status.hardware.is_some())
        .await;
    assert_eq!(got.status.hardware, Some(RunningHardware::legacy(&vm.spec)));
}

/// 작업자가 VM을 읽은 뒤 다른 이유로 status를 쓰는 사이 누가(핫플러그 Task) 기록을 바꾸면 그 값을 지킨다.
#[tokio::test]
async fn status_writes_keep_hardware_written_meanwhile() {
    let env = Env::start_with(|c| c.resync_interval = Duration::from_millis(200)).await;
    add_node(&env, env.node, &[HOTPLUG], Some(ALL)).await;
    let vm = hot_vm(env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // 응답하지 않는 VM에 다시 붙는 동안(멈춘 start) 기록을 바꾼다. 작업자는 오류를 쓴다.
    env.hv.hold_next_start();
    env.hv.set_unresponsive(vm.id, true);
    env.hv.start_held().await;
    let changed = set_hardware(&env, vm.id, |hw| {
        hw.layout.as_mut().unwrap().plugged_mib = 2048;
    })
    .await;
    env.hv.release_start();

    let got = env
        .wait(vm.id, "reported unresponsive", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("unresponsive"))
        })
        .await;
    assert_eq!(got.status.hardware, Some(changed));
    env.hv.set_unresponsive(vm.id, false);
}
