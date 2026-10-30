//! UEFI 변수가 VM을 따라간다(설계 §5): 멈춘 VM을 새로 켤 때만 변수 파일을 가진 노드에서 받아 오고,
//! 켠 노드를 `firmware_node`로 남긴다.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_driver::{Hypervisor, RuntimeStatus};
use opl_reconciler::{FirmwareError, FirmwareFetcher};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{Firmware, MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

/// 받아 오기 호출(VM, 노드)을 적는다. `fail`이면 실패한다.
#[derive(Default)]
struct Fetcher {
    calls: Mutex<Vec<(Uuid, Uuid)>>,
    fail: bool,
}

#[async_trait]
impl FirmwareFetcher for Fetcher {
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        self.calls.lock().unwrap().push((vm, node));
        if self.fail {
            return Err(FirmwareError("node \"node2\" is offline".into()));
        }
        Ok(())
    }
}

impl Fetcher {
    fn calls(&self) -> Vec<(Uuid, Uuid)> {
        self.calls.lock().unwrap().clone()
    }
}

async fn env_with(fetcher: &Arc<Fetcher>) -> Env {
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    Env::start_with(|c| c.firmware = Some(f)).await
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

/// `firmware_node`가 `node`인 멈춘 VM을 만들고 켠다. 켜진 VM을 돌려준다.
async fn start_vm(env: &Env, firmware: Firmware, node: Option<Uuid>) -> Vm {
    let mut vm = vm("web", env.node);
    vm.spec.firmware = firmware;
    vm.spec.power = PowerState::Stopped;
    vm.status.firmware_node = node;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await
}

#[tokio::test]
async fn variables_are_fetched_from_the_node_that_has_them() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let other = Uuid::now_v7();
    let got = start_vm(&env, Firmware::Uefi, Some(other)).await;

    assert_eq!(fetcher.calls(), vec![(got.id, other)]);
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert_eq!(env.hv.launches(), vec![(got.id, None)]);
}

#[tokio::test]
async fn failed_fetch_does_not_stop_the_start() {
    let fetcher = Arc::new(Fetcher {
        fail: true,
        ..Default::default()
    });
    let env = env_with(&fetcher).await;
    let other = Uuid::now_v7();
    let got = start_vm(&env, Firmware::Uefi, Some(other)).await;

    assert_eq!(fetcher.calls(), vec![(got.id, other)]);
    // 이 노드의 변수(없으면 템플릿)로 켰으므로 이제 이 노드가 가진다.
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches(), vec![(got.id, None)]);
}

#[tokio::test]
async fn variables_on_this_node_are_not_fetched() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let got = start_vm(&env, Firmware::Uefi, Some(env.node)).await;
    assert_eq!(got.status.firmware_node, Some(env.node));

    // 한 번도 켠 적 없는 VM도 받을 곳이 없다.
    let mut fresh = vm("fresh", env.node);
    fresh.spec.firmware = Firmware::Uefi;
    env.create(&fresh).await;
    let got = env.wait(fresh.id, "running", running).await;
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
}

#[tokio::test]
async fn bios_vm_is_left_alone() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let got = start_vm(&env, Firmware::Bios, None).await;

    assert_eq!(got.status.firmware_node, None);
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
}

/// opld가 다시 떠서 도는 VM에 다시 붙는다: 그 QEMU는 이 노드의 변수를 쓰므로 받지 않고 이 노드를
/// 남긴다.
#[tokio::test]
async fn running_vm_is_not_fetched_when_reattached() {
    let fetcher = Arc::new(Fetcher::default());
    let mut env = env_with(&fetcher).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    let mut status = before.status.clone();
    status.firmware_node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    env.start_reconciler();

    let got = env
        .wait(vm.id, "firmware node recorded", |v| {
            v.status.firmware_node == Some(env.node)
        })
        .await;
    assert_eq!(got.status.pid, before.status.pid);
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
    assert_eq!(env.hv.launches(), vec![(vm.id, None)]);
}

/// 이 노드로 확정된 라이브 마이그레이션을 넘겨받는다: QEMU가 변수를 옮겨 왔으므로 받지 않고 이 노드를
/// 남긴다.
#[tokio::test]
async fn received_vm_is_not_fetched() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let source = Uuid::now_v7();
    let mut vm = vm("web", source);
    vm.status.firmware_node = Some(source);
    vm.status.phase = VmPhase::Migrating;
    vm.status.node = Some(source);
    vm.status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.create(&vm).await;
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();

    // 원본이 확정한다.
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.firmware_node == Some(env.node)
        })
        .await;
    assert_eq!(got.status.node, Some(env.node));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// 작업자가 바꾸지 않는 `firmware_node`는 그 사이 다른 곳(스냅샷 되돌리기)이 쓴 값을 덮지 않는다.
#[tokio::test]
async fn status_writes_keep_firmware_node_written_meanwhile() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let mut vm = vm("web", env.node);
    vm.spec.firmware = Firmware::Bios;
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;

    // 작업자가 VM을 읽고 시작하는 사이에 쓴다.
    env.hv.delay_starts(Duration::from_millis(300));
    env.set_power(vm.id, PowerState::Running).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let node = Uuid::now_v7();
    let mut status = current.status.clone();
    status.firmware_node = Some(node);
    env.cluster
        .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
        .await
        .unwrap();

    env.wait(vm.id, "running", running).await;
    assert_eq!(env.get(vm.id).status.firmware_node, Some(node));
}
