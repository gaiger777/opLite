//! 보안 그룹을 건 NIC가 있는 VM을 켜기 직전의 방화벽 확인([`FirewallGate`]).

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_reconciler::{FirewallGate, ReconcilerConfig};
use opl_types::network::NetworkSpec;
use opl_types::resource::Resource;
use opl_types::vm::{MacAddr, NicModel, NicSpec, PowerState, Vm, VmPhase};

/// 부른 횟수를 세고, `fail`에 남은 오류를 앞에서부터 하나씩 돌려준다.
#[derive(Default)]
struct Gate {
    calls: AtomicUsize,
    fail: Mutex<Vec<String>>,
}

#[async_trait]
impl FirewallGate for Gate {
    async fn sync(&self) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut fail = self.fail.lock().unwrap();
        if fail.is_empty() {
            Ok(())
        } else {
            Err(fail.remove(0))
        }
    }
}

async fn env_with(gate: Option<Arc<Gate>>) -> Env {
    env_with_config(gate, |_| {}).await
}

async fn env_with_config(
    gate: Option<Arc<Gate>>,
    tweak: impl FnOnce(&mut ReconcilerConfig),
) -> Env {
    let env = Env::start_with(|c| {
        c.firewall = gate.map(|g| g as Arc<dyn FirewallGate>);
        tweak(c);
    })
    .await;
    let lan = Resource::new(
        "lan",
        NetworkSpec {
            bridge: Some("vmbr0".into()),
            vlan: None,
            vxlan: None,
            mtu: None,
        },
    );
    env.cluster.create(&lan).await.unwrap();
    env
}

/// 브리지 네트워크 NIC 하나짜리 VM. `groups`가 있으면 방화벽이 켜진다.
fn vm_with_groups(env: &Env, groups: &[&str]) -> Vm {
    let mut vm = vm("web", env.node);
    vm.spec.nics = vec![NicSpec {
        network: "lan".into(),
        model: NicModel::Virtio,
        mac: Some(MacAddr([0x52, 0x54, 0, 1, 2, 3])),
        security_groups: groups.iter().map(|g| g.to_string()).collect(),
    }];
    vm
}

#[tokio::test]
async fn starts_after_the_firewall_rules_are_in() {
    let gate = Arc::new(Gate::default());
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    assert_eq!(env.hv.launches().len(), 1);
}

/// 규칙을 넣지 못하면 켜지 않고 시작 실패로 센다(일시 실패처럼 다시 시도하고, 계속 실패하면 포기).
#[tokio::test]
async fn a_failing_firewall_keeps_the_vm_off() {
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into()];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    // 한 번 실패한 뒤 다시 시도해 켠다.
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 2);
    assert_eq!(env.hv.launches().len(), 1);

    // 하네스는 3번 실패하면 포기한다.
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 10];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: firewall: nft: boom; gave up after 3 failures")
    );
    assert_eq!(gate.calls.load(Ordering::SeqCst), 3);
    assert!(env.hv.launches().is_empty());
}

/// 방화벽이 없는 노드는 보안 그룹 NIC가 있는 VM을 켜지 않는다(다시 시도하지 않는다).
#[tokio::test]
async fn a_node_without_a_firewall_does_not_start_the_vm() {
    let env = env_with(None).await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: firewall: this node has no firewall (nftables)")
    );
    assert!(env.hv.launches().is_empty());
}

/// 보안 그룹이 없는 VM은 방화벽 확인 없이 켠다(방화벽이 없는 노드에서도).
#[tokio::test]
async fn vms_without_groups_skip_the_gate() {
    let gate = Arc::new(Gate::default());
    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 10];
    let env = env_with(Some(gate.clone())).await;
    let vm = vm_with_groups(&env, &[]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 0);

    let env = env_with(None).await;
    let vm = vm_with_groups(&env, &[]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
}

/// 살아 있는 VM에 다시 붙을 때(opld를 다시 시작한 뒤 등)도 이어서 실행하기 전에 규칙을 넣고, 넣지
/// 못하면 이어서 실행하지 않고 다시 본다.
#[tokio::test]
async fn reattaching_applies_the_firewall_rules_first() {
    let gate = Arc::new(Gate::default());
    // 응답하지 않는 VM은 재동기화로 알아채고 다시 붙는다.
    let env = env_with_config(Some(gate.clone()), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);

    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 1000];
    env.hv.set_unresponsive(vm.id, true);
    let got = env
        .wait(vm.id, "reported the firewall", |v| {
            v.status.last_error.is_some()
        })
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM is unresponsive: firewall: nft: boom")
    );
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(env.hv.launches().len(), 1);
}

/// 이전 opld가 멈춘 채 남긴 VM을 이어서 실행할 때도 먼저 규칙을 넣고, 넣지 못하면 멈춘 채 두고 다시 본다.
#[tokio::test]
async fn resuming_a_vm_left_paused_applies_the_firewall_rules_first() {
    let gate = Arc::new(Gate::default());
    let env = env_with_config(Some(gate.clone()), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    let vm = vm_with_groups(&env, &["web"]);
    env.create(&vm).await;
    env.wait(vm.id, "running", |v| v.status.phase == VmPhase::Running)
        .await;
    assert_eq!(gate.calls.load(Ordering::SeqCst), 1);

    *gate.fail.lock().unwrap() = vec!["nft: boom".into(); 1000];
    env.hv.leave_paused(vm.id);
    let got = env
        .wait(vm.id, "reported the firewall", |v| {
            v.status.last_error.is_some()
        })
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM is unresponsive: firewall: nft: boom")
    );
    assert!(env.hv.is_paused(vm.id), "must stay paused");

    // 규칙을 넣을 수 있게 되면 이어서 실행한다.
    gate.fail.lock().unwrap().clear();
    env.wait(vm.id, "resumed", |v| v.status.last_error.is_none())
        .await;
    assert!(!env.hv.is_paused(vm.id));
    assert_eq!(env.hv.launches().len(), 1);
}
