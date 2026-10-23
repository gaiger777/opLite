//! M4 완료 기준: opld가 죽었다 다시 떠도 VM은 계속 돌고, 새 opld가 다시 붙어 제어한다.
//! `cargo test -p opl-reconciler --features qemu-it --test qemu_restart`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{open_node, raft_config, set_power, single_node, vm, wait_leader, wait_vm};
use opl_cluster::memnet::MemRouter;
use opl_driver::{Hypervisor, QemuConfig, QemuHypervisor};
use opl_qemu::HostProfile;
use opl_reconciler::Reconciler;
use opl_types::vm::{PowerState, VmPhase, VmSpec};

fn qemu_config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // systemd가 있는 Linux(CI)에서 `OPL_TEST_SYSTEMD=1`로 스코프 실행까지 확인한다.
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
}

#[tokio::test]
async fn vm_survives_opld_restart_and_is_controlled_again() {
    // 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let router = MemRouter::default();
    let cluster = single_node(&router, &tmp.path().join("cluster")).await;
    let node = cluster.id();
    let mut config = common::config(node.0);
    // 디스크 없는 UEFI 펌웨어는 ACPI 종료에 응하지 않을 수 있다. 강제 종료까지 확인한다.
    config.acpi_timeout = Duration::from_secs(3);

    let hv: Arc<dyn Hypervisor> =
        Arc::new(QemuHypervisor::new(qemu_config(tmp.path())).await.unwrap());
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), config.clone());
    let vm = vm("it-vm", node.0);
    cluster.create(&vm).await.unwrap();
    let running = wait_vm(&cluster, vm.id, "running", |v| {
        v.status.phase == VmPhase::Running
    })
    .await;
    let pid = running.status.pid.expect("pid recorded");

    // opld가 죽은 것처럼 reconciler, 드라이버, 클러스터를 모두 내린다.
    // QEMU는 -daemonize로 떠 있어 opld와 함께 죽지 않는다.
    reconciler.shutdown().await;
    drop(hv);
    router.unregister(node);
    cluster.shutdown().await.unwrap();
    drop(cluster);

    // 새 opld: 저장소를 다시 열고, 드라이버가 살아 있는 QEMU에 다시 붙는다.
    let cluster = open_node(
        &router,
        node,
        &tmp.path().join("cluster"),
        raft_config(),
        Duration::from_secs(5),
    )
    .await;
    wait_leader(&cluster).await;
    let hv: Arc<dyn Hypervisor> =
        Arc::new(QemuHypervisor::new(qemu_config(tmp.path())).await.unwrap());
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), Some(pid));
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), config);

    // 새 reconciler는 VM을 다시 시작하지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), Some(pid));
    let got = cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(
        (got.status.phase, got.status.pid),
        (VmPhase::Running, Some(pid))
    );

    // 다시 붙은 VM을 끌 수 있다.
    set_power(&cluster, vm.id, PowerState::Stopped).await;
    wait_vm(&cluster, vm.id, "stopped", |v| {
        v.status.phase == VmPhase::Stopped
    })
    .await;
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    reconciler.shutdown().await;
}
