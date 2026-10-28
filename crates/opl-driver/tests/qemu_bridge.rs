//! 실제 QEMU의 NIC를 tap으로 브리지에 잇는 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it,net-it --test qemu_bridge`
//! Linux에서 root, iproute2, QEMU가 필요하다.
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use opl_driver::{EventKind, Hypervisor, IpRoute, QemuConfig, QemuHypervisor, StopMode};
use opl_qemu::HostProfile;
use opl_types::network::tap_name;
use opl_types::runtime::{NicBackend, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, Firmware, NicModel};
use uuid::Uuid;

struct TestBridge(String);

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn bridge() -> TestBridge {
    let name = format!("oplq{}", &Uuid::now_v7().simple().to_string()[22..]);
    for args in [
        vec![
            "link",
            "add",
            &name,
            "type",
            "bridge",
            "vlan_filtering",
            "1",
        ],
        vec!["link", "set", &name, "up"],
    ] {
        assert!(Command::new("ip").args(&args).status().unwrap().success());
    }
    TestBridge(name)
}

fn master_of(ifname: &str) -> Option<String> {
    let out = Command::new("ip")
        .args(["-j", "link", "show", "dev", ifname])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["master"].as_str().map(str::to_string)
}

fn exists(ifname: &str) -> bool {
    Command::new("ip")
        .args(["link", "show", "dev", ifname])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn config(root: &Path) -> QemuConfig {
    QemuConfig {
        host: HostProfile::native(Path::new("/opt/homebrew/share/qemu")),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: Some(Arc::new(IpRoute)),
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
}

fn spec(bridge: &str) -> VmRuntimeSpec {
    let id = Uuid::now_v7();
    VmRuntimeSpec {
        id,
        name: "it-bridge".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: tap_name(id, 0),
                bridge: bridge.into(),
                vlan: Some(20),
            },
        }],
    }
}

async fn wait_gone(ifname: &str) {
    for _ in 0..100 {
        if !exists(ifname) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("tap {ifname} is still there");
}

#[tokio::test]
async fn vm_nic_is_on_the_bridge_while_qemu_runs() {
    let br = bridge();
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec(&br.0);
    let tap = tap_name(vm.id, 0);

    hv.start(&vm, None).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    wait_gone(&tap).await;

    // QEMU가 죽어도 tap을 지운다.
    let mut rx = hv.subscribe();
    hv.start(&vm, None).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    assert!(
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no event")
            .unwrap();
        if ev.id == vm.id && ev.kind == EventKind::Crashed {
            break;
        }
    }
    wait_gone(&tap).await;
}

#[tokio::test]
async fn missing_bridge_fails_the_start_without_leftovers() {
    let br = bridge();
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    // NIC 0의 tap은 만들어진 뒤 NIC 1에서 실패하므로 지워져야 한다.
    let mut vm = spec(&br.0);
    vm.nics.push(RuntimeNic {
        mac: "52:54:00:12:34:57".parse().unwrap(),
        model: NicModel::Virtio,
        backend: NicBackend::Tap {
            ifname: tap_name(vm.id, 1),
            bridge: "oplq-missing".into(),
            vlan: None,
        },
    });
    let err = hv.start(&vm, None).await.unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!exists(&tap_name(vm.id, 0)));
    assert!(!exists(&tap_name(vm.id, 1)));
}

#[tokio::test]
async fn tap_stays_on_the_bridge_across_driver_restart() {
    let br = bridge();
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let vm = spec(&br.0);
    let tap = tap_name(vm.id, 0);
    {
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(master_of(&tap), Some(br.0.clone()));
        // 드라이버(= opld)가 내려간다. QEMU는 분리되어 있으므로 tap도 그대로여야 한다.
    }

    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    assert!(hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    wait_gone(&tap).await;
}
