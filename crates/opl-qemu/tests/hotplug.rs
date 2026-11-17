//! 핫플러그 배치로 띄운 실제 QEMU(OS 없음, `-S`)에 QMP 핫플러그 명령을 보내 보는 통합 테스트.
//! `cargo test -p opl-qemu --features qemu-it --test hotplug`
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_qemu::{
    Accel, HostProfile, QmpClient, QmpError, RuntimePaths, build_cmdline, hotplug_caps,
};
use opl_types::runtime::{
    DiskKind, HOTPLUG_PORTS, HotplugLayout, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic,
    VmRuntimeSpec,
};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware, NicModel};
use serde_json::{Value, json};
use uuid::Uuid;

fn host() -> HostProfile {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::native(Path::new(&share))
}

/// 디스크 하나·NIC 하나(user)와 빈 루트 포트 8개, 상한 없음
fn layout() -> HotplugLayout {
    HotplugLayout {
        vcpus: 1,
        max_memory_mib: 256,
        plugged_mib: 0,
        ports: HOTPLUG_PORTS,
        boot_disks: 1,
        boot_nics: 1,
        disk_ports: vec![],
        nic_ports: vec![],
    }
}

fn spec(host: &HostProfile, id: Uuid, disk: PathBuf, layout: HotplugLayout) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id,
        name: "it-hotplug".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
            max_vcpus: None,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: host.arch.machine(),
        disks: vec![RuntimeDisk {
            path: disk,
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }],
        guest_agent: false,
        hotplug: Some(layout),
        cdroms: vec![],
    }
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path:?} did not appear within 10s");
}

fn qcow2(path: &Path) {
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(path)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
}

/// 실행 중인 QEMU 하나. 테스트가 끝나면(`kill_on_drop`) 그 프로세스만 멈춘다.
struct Vm {
    tmp: tempfile::TempDir,
    child: tokio::process::Child,
    qmp: QmpClient,
}

impl Vm {
    async fn quit(mut self) {
        self.qmp.quit().await.unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("qemu did not exit within 10s")
            .unwrap();
        assert!(exit.success(), "qemu exited with {exit}");
    }
}

/// `make`가 디스크 경로·VM id로 만든 스펙으로 QEMU를 `-S`로 띄우고 QMP에 붙는다.
async fn launch(host: &HostProfile, make: impl FnOnce(Uuid, PathBuf) -> VmRuntimeSpec) -> Vm {
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let id = Uuid::now_v7();
    let paths = RuntimePaths::new(tmp.path(), id).unwrap();
    std::fs::create_dir_all(&paths.dir).unwrap();
    let disk = tmp.path().join("root.qcow2");
    qcow2(&disk);
    let spec = make(id, disk);
    if spec.firmware == Firmware::Uefi {
        std::fs::copy(&host.uefi_vars_template, &paths.uefi_vars).unwrap();
    }
    let args = build_cmdline(&spec, host, &paths).unwrap();
    let child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for(&paths.qmp_socket).await;
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    Vm { tmp, child, qmp }
}

/// id를 준 장치들(`/machine/peripheral`의 자식)
async fn peripherals(qmp: &QmpClient) -> Vec<String> {
    let ret = qmp
        .execute("qom-list", Some(json!({ "path": "/machine/peripheral" })))
        .await
        .unwrap();
    ret.as_array()
        .unwrap()
        .iter()
        .filter(|c| c["type"].as_str().is_some_and(|t| t.starts_with("child<")))
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

/// x86 q35를 TCG·BIOS로 띄우는 프로필. `qemu-system-x86_64`가 없으면 None
fn q35_tcg() -> Option<HostProfile> {
    let found = std::process::Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    found.then(|| HostProfile {
        arch: Arch::X86_64,
        accel: Accel::Tcg,
        qemu_binary: PathBuf::from("qemu-system-x86_64"),
        uefi_code: PathBuf::new(),
        uefi_vars_template: PathBuf::new(),
    })
}

/// 배치(루트 포트 8개)로 뜨고, x86은 CPU 자리 4개(켜진 것 1개)를, aarch64 `virt`는 CPU 핫플러그가 없다고 답한다.
#[tokio::test]
async fn the_layout_boots_and_reports_cpu_slots() {
    let host = host();
    let x86 = host.arch == Arch::X86_64;
    let vm = launch(&host, |id, disk| {
        let mut spec = spec(&host, id, disk, layout());
        if x86 {
            spec.cpu.max_vcpus = Some(4);
        }
        spec
    })
    .await;
    let devices = peripherals(&vm.qmp).await;
    for k in 0..HOTPLUG_PORTS {
        assert!(devices.contains(&format!("hp{k}")), "{devices:?}");
    }
    let cpus = vm.qmp.query_hotpluggable_cpus().await;
    if x86 {
        let cpus = cpus.unwrap();
        assert_eq!(cpus.len(), 4, "{cpus:?}");
        assert_eq!(cpus.iter().filter(|c| c.qom_path.is_some()).count(), 1);
    } else {
        match cpus {
            Err(QmpError::Command { class, desc }) => {
                assert_eq!(class, "GenericError");
                assert!(
                    desc.contains("does not support hot-plugging CPUs"),
                    "{desc}"
                );
            }
            other => panic!("aarch64 virt answered {other:?}"),
        }
    }
    vm.quit().await;
}

/// q35(TCG, BIOS)에서 빈 CPU 자리에 vCPU를 붙이고, 그 vCPU를 켠 채(`vcpus` 2) 다시 띄우면 같은 id로 뜬다.
#[tokio::test]
async fn a_cpu_is_added_on_q35() {
    let Some(host) = q35_tcg() else {
        eprintln!("qemu-system-x86_64 not found; skipping");
        return;
    };
    let make = |vcpus: u16| {
        let host = host.clone();
        move |id, disk| {
            let mut spec = spec(&host, id, disk, HotplugLayout { vcpus, ..layout() });
            spec.firmware = Firmware::Bios;
            spec.cpu.model = CpuModel::Max;
            spec.cpu.max_vcpus = Some(4);
            spec
        }
    };
    let vm = launch(&host, make(1)).await;
    assert_eq!(vm.qmp.query_cpus_fast().await.unwrap(), 1);
    let cpus = vm.qmp.query_hotpluggable_cpus().await.unwrap();
    let slot = cpus
        .iter()
        .find(|c| c.qom_path.is_none() && c.core_id == Some(1))
        .unwrap_or_else(|| panic!("no free slot at core 1 in {cpus:?}"));
    vm.qmp
        .device_add(json!({
            "driver": slot.kind, "id": "cpu1",
            "socket-id": slot.socket_id, "core-id": slot.core_id, "thread-id": slot.thread_id,
        }))
        .await
        .unwrap();
    assert_eq!(vm.qmp.query_cpus_fast().await.unwrap(), 2);
    vm.quit().await;

    let vm = launch(&host, make(2)).await;
    assert_eq!(vm.qmp.query_cpus_fast().await.unwrap(), 2);
    assert!(peripherals(&vm.qmp).await.contains(&"cpu1".to_string()));
    vm.quit().await;
}

/// 디스크(두 층 블록 노드)와 NIC(user)를 루트 포트에 붙인다. aarch64 `virt`는 게스트 없이도 곧바로 떼지고, q35는
/// 게스트가 eject해야 해서 시간 안에 떼지지 않는다.
#[tokio::test]
async fn disks_and_nics_go_on_root_ports() {
    let host = host();
    let vm = launch(&host, |id, disk| spec(&host, id, disk, layout())).await;
    let qmp = &vm.qmp;
    let hot = vm.tmp.path().join("hot.qcow2");
    qcow2(&hot);
    qmp.blockdev_add(json!({
        "driver": "file", "filename": hot.display().to_string(), "node-name": "disk1-file",
    }))
    .await
    .unwrap();
    qmp.blockdev_add(json!({ "driver": "qcow2", "file": "disk1-file", "node-name": "disk1" }))
        .await
        .unwrap();
    qmp.device_add(json!({
        "driver": "virtio-blk-pci", "drive": "disk1", "id": "disk1-dev", "bus": "hp0",
    }))
    .await
    .unwrap();
    qmp.netdev_add(json!({ "type": "user", "id": "net1" }))
        .await
        .unwrap();
    qmp.device_add(json!({
        "driver": "virtio-net-pci", "netdev": "net1", "mac": "52:54:00:12:34:57",
        "id": "nic1", "bus": "hp1",
    }))
    .await
    .unwrap();
    let devices = peripherals(qmp).await;
    assert!(devices.contains(&"disk1-dev".to_string()), "{devices:?}");
    assert!(devices.contains(&"nic1".to_string()), "{devices:?}");

    if host.arch == Arch::Aarch64 {
        let t = Duration::from_secs(5);
        assert!(qmp.device_del_wait("disk1-dev", t).await.unwrap());
        qmp.blockdev_del("disk1").await.unwrap();
        qmp.blockdev_del("disk1-file").await.unwrap();
        assert!(qmp.device_del_wait("nic1", t).await.unwrap());
        qmp.netdev_del("net1").await.unwrap();
        let devices = peripherals(qmp).await;
        assert!(!devices.contains(&"disk1-dev".to_string()), "{devices:?}");
        // 이미 뗀 장치는 뗀 것으로 본다.
        assert!(qmp.device_del_wait("nic1", t).await.unwrap());
    } else {
        let t = Duration::from_secs(2);
        assert!(!qmp.device_del_wait("disk1-dev", t).await.unwrap());
        assert!(peripherals(qmp).await.contains(&"disk1-dev".to_string()));
    }
    vm.quit().await;
}

/// virtio-mem 장치의 `requested-size`를 바꾼다(게스트가 없어 실제 크기는 그대로). QEMU에 `virtio-mem-pci`가 없으면
/// 건너뛰지만, Linux x86_64(Ubuntu 24.04 QEMU 8.2)에는 있어야 한다.
#[tokio::test]
async fn virtio_mem_takes_a_requested_size() {
    let host = host();
    let help = std::process::Command::new(&host.qemu_binary)
        .args(["-device", "help"])
        .output()
        .unwrap();
    let caps = hotplug_caps(
        host.arch,
        cfg!(target_os = "linux"),
        &String::from_utf8_lossy(&help.stdout),
    );
    if !caps.memory {
        assert!(
            !(cfg!(target_os = "linux") && host.arch == Arch::X86_64),
            "QEMU on Linux x86_64 must have virtio-mem-pci"
        );
        eprintln!("{:?} has no virtio-mem-pci; skipping", host.qemu_binary);
        return;
    }
    let vm = launch(&host, |id, disk| {
        spec(
            &host,
            id,
            disk,
            HotplugLayout {
                max_memory_mib: 512,
                ..layout()
            },
        )
    })
    .await;
    let path = "/machine/peripheral/vmem0-dev";
    assert_eq!(
        vm.qmp.qom_get(path, "requested-size").await.unwrap(),
        Value::from(0)
    );
    vm.qmp
        .qom_set(path, "requested-size", json!(256u64 << 20))
        .await
        .unwrap();
    assert_eq!(
        vm.qmp.qom_get(path, "requested-size").await.unwrap(),
        Value::from(268435456u64)
    );
    vm.quit().await;
}
