use std::path::Path;

use opl_types::runtime::{
    DiskKind, HOTPLUG_PORTS, HotplugLayout, ImageFormat, NicBackend, RbdTarget, VmRuntimeSpec,
};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, HostProfile};
use crate::paths::RuntimePaths;

/// q35의 내장 AHCI 컨트롤러 포트 수
const Q35_SATA_PORTS: usize = 6;

/// 핫플러그 루트 포트를 모아 두는 `pcie.0` 슬롯. 멀티펑션 8개(`0x1e.0`~`0x1e.7`)라 자동 주소를 밀지 않는다.
pub const HOTPLUG_SLOT: u8 = 0x1e;

/// virtio-mem 블록 크기. 마이그레이션 양쪽이 같게 박아 둔다.
pub const VIRTIO_MEM_BLOCK: &str = "2M";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CmdlineError {
    #[error("machine type {machine:?} is not supported on {arch:?} hosts")]
    MachineArchMismatch { machine: MachineType, arch: Arch },
    #[error("BIOS firmware is not supported on aarch64; use UEFI")]
    BiosOnAarch64,
    #[error("SATA disks require the q35 machine type")]
    SataRequiresQ35,
    #[error("at most {Q35_SATA_PORTS} SATA disks are supported")]
    TooManySataDisks,
    #[error("disk {0} is an rbd disk without an rbd image")]
    MissingRbdImage(usize),
    #[error("vCPU hotplug is not supported on aarch64")]
    CpuHotplugUnsupported,
    #[error("at most {HOTPLUG_PORTS} hotplug ports are supported")]
    TooManyPorts,
    #[error("{0} {1} was added while running but has no hotplug port")]
    MissingPort(&'static str, usize),
}

/// QEMU 인자 목록(바이너리 제외)을 만든다. 부수 효과가 없는 순수 함수다.
///
/// VM은 항상 `-S`(일시정지) 상태로 시작한다. 호출자는 QMP 연결 뒤 `cont`로 실행을 재개한다.
pub fn build_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    check_compat(spec, host)?;

    let mut a = Args::default();
    a.opt(
        "-name",
        format!("guest={},debug-threads=on", esc(&spec.name)),
    );
    a.opt("-uuid", spec.id.to_string());
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    a.flag("-S");
    let machine = match spec.machine {
        MachineType::Q35 => "q35",
        MachineType::Virt => "virt",
    };
    a.opt(
        "-machine",
        format!("{machine},accel={}", host.accel.as_str()),
    );
    let cpu = match (spec.cpu.model, host.accel) {
        (CpuModel::Host, Accel::Tcg) | (CpuModel::Max, _) => "max",
        (CpuModel::Host, _) => "host",
    };
    a.opt("-cpu", cpu.to_string());
    let layout = spec.hotplug.as_ref();
    let boot_vcpus = spec.cpu.vcpus();
    // vCPU 자리: 상한이 켤 때 수보다 크면 소켓마다 `max / sockets`개 코어 자리를 두고 앞에서부터 켠다.
    let max_vcpus = match (layout, spec.cpu.max_vcpus) {
        (Some(_), Some(max)) if u32::from(max) > boot_vcpus => Some(u32::from(max)),
        _ => None,
    };
    match max_vcpus {
        Some(max) => a.opt(
            "-smp",
            format!(
                "{boot_vcpus},sockets={},cores={},threads=1,maxcpus={max}",
                spec.cpu.sockets,
                max / u32::from(spec.cpu.sockets)
            ),
        ),
        None => a.opt(
            "-smp",
            format!(
                "sockets={},cores={},threads=1",
                spec.cpu.sockets, spec.cpu.cores
            ),
        ),
    }
    // virtio-mem: 켤 때 메모리 위로 상한까지의 자리
    let virtio_mem = layout.filter(|l| l.max_memory_mib > spec.memory_mib);
    match virtio_mem {
        Some(l) => a.opt(
            "-m",
            format!("{}M,maxmem={}M", spec.memory_mib, l.max_memory_mib),
        ),
        None => a.opt("-m", format!("{}M", spec.memory_mib)),
    }
    // 실행 중 붙인 vCPU(원본과 같은 id·자리)
    if let (Some(l), Some(max)) = (layout, max_vcpus) {
        let per_socket = max / u32::from(spec.cpu.sockets);
        for i in boot_vcpus..u32::from(l.vcpus) {
            a.opt(
                "-device",
                format!(
                    "{cpu}-x86_64-cpu,id=cpu{i},socket-id={},core-id={},thread-id=0",
                    i / per_socket,
                    i % per_socket
                ),
            );
        }
    }

    if spec.firmware == Firmware::Uefi {
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=0,format=raw,readonly=on,file={}",
                esc_path(&host.uefi_code)
            ),
        );
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=1,format=raw,file={}",
                esc_path(&paths.uefi_vars)
            ),
        );
    }

    let video = match host.arch {
        Arch::X86_64 => "VGA,id=video0",
        Arch::Aarch64 => "virtio-gpu-pci,id=video0",
    };
    a.opt("-device", video.to_string());
    a.opt("-device", "qemu-xhci,id=usb".to_string());
    a.opt("-device", "usb-tablet,bus=usb.0".to_string());
    a.opt("-device", "usb-kbd,bus=usb.0".to_string());

    // 빈 루트 포트: 주소를 박아 다른 장치의 자동 주소가 그대로다. 실행 중 붙인 장치가 쓰므로 디스크·NIC보다 먼저 둔다.
    if let Some(l) = layout {
        for k in 0..l.ports {
            let mut port = format!(
                "pcie-root-port,id=hp{k},bus=pcie.0,chassis={},addr={HOTPLUG_SLOT:#x}.{k},io-reserve=0",
                u16::from(k) + 1
            );
            if k == 0 {
                port.push_str(",multifunction=on");
            }
            a.opt("-device", port);
        }
    }

    let mut sata_port = 0;
    for (i, disk) in spec.disks.iter().enumerate() {
        let fmt = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        let source = match disk.kind {
            DiskKind::File => format!("driver=file,filename={}", esc_path(&disk.path)),
            DiskKind::Block => format!("driver=host_device,filename={}", esc_path(&disk.path)),
            DiskKind::Rbd => {
                let rbd = disk.rbd.as_ref().ok_or(CmdlineError::MissingRbdImage(i))?;
                rbd_source(rbd)
            }
        };
        a.opt("-blockdev", format!("{source},node-name=disk{i}-file"));
        a.opt(
            "-blockdev",
            format!("driver={fmt},file=disk{i}-file,node-name=disk{i}"),
        );
        let mut dev = match disk.bus {
            DiskBus::VirtioBlk => format!(
                "virtio-blk-pci,drive=disk{i},id=disk{i}-dev{}",
                hot_bus(layout.map(|l| (l.boot_disks, &l.disk_ports[..])), i)
            ),
            DiskBus::Sata => {
                let d = format!("ide-hd,drive=disk{i},bus=ide.{sata_port},id=disk{i}-dev");
                sata_port += 1;
                d
            }
        };
        if let Some(order) = disk.boot_order {
            dev.push_str(&format!(",bootindex={order}"));
        }
        a.opt("-device", dev);
    }

    for (i, nic) in spec.nics.iter().enumerate() {
        let netdev = match &nic.backend {
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname, .. } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
                esc(ifname)
            ),
        };
        a.opt("-netdev", netdev);
        let model = match nic.model {
            NicModel::Virtio => "virtio-net-pci",
            NicModel::E1000e => "e1000e",
        };
        let mut dev = format!(
            "{model},netdev=net{i},mac={},id=nic{i}{}",
            nic.mac,
            hot_bus(layout.map(|l| (l.boot_nics, &l.nic_ports[..])), i)
        );
        // VXLAN 네트워크의 MTU를 게스트에 알린다(virtio만 받는다).
        if let (NicModel::Virtio, NicBackend::Tap { mtu: Some(mtu), .. }) =
            (nic.model, &nic.backend)
        {
            dev.push_str(&format!(",host_mtu={mtu}"));
        }
        a.opt("-device", dev);
    }

    // 게스트 에이전트 채널은 NIC 뒤에 단다(앞 장치들의 PCI 주소가 그대로다). 감시하는 VM만 단다.
    if spec.guest_agent {
        a.opt(
            "-chardev",
            format!(
                "socket,id=qga0,path={},server=on,wait=off",
                esc_path(&paths.qga_socket)
            ),
        );
        a.opt("-device", "virtio-serial-pci,id=qgaser".to_string());
        a.opt(
            "-device",
            "virtserialport,bus=qgaser.0,chardev=qga0,name=org.qemu.guest_agent.0".to_string(),
        );
    }

    // virtio-mem도 앞 장치들의 PCI 주소가 그대로이게 뒤에 단다. NUMA 노드는 `maxmem`이 있으면 QEMU가 만든다.
    if let Some(l) = virtio_mem {
        a.opt(
            "-object",
            format!(
                "memory-backend-ram,id=vmem0,size={}M",
                l.max_memory_mib - spec.memory_mib
            ),
        );
        a.opt(
            "-device",
            format!(
                "virtio-mem-pci,id=vmem0-dev,memdev=vmem0,block-size={VIRTIO_MEM_BLOCK},requested-size={}M",
                l.plugged_mib
            ),
        );
    }

    a.opt("-vnc", format!("unix:{}", esc_path(&paths.vnc_socket)));
    a.opt(
        "-qmp",
        format!("unix:{},server=on,wait=off", esc_path(&paths.qmp_socket)),
    );
    a.opt("-pidfile", paths.pidfile.display().to_string());
    Ok(a.0)
}

/// 라이브 마이그레이션을 받을 QEMU의 인자. [`build_cmdline`]과 같은 장치에 `-incoming defer`를 더한다.
/// QEMU는 QMP `migrate-incoming`을 받을 때까지 기다리고, 받은 뒤에도 `-S`라 `cont` 전까지 멈춰 있다.
pub fn build_incoming_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    let mut args = build_cmdline(spec, host, paths)?;
    args.push("-incoming".into());
    args.push("defer".into());
    Ok(args)
}

/// 복제본을 받는 QEMU의 인자. 게스트 장치 없이(`-machine none`) 멈춘 채(`-S`) QMP만 연다. 복제본 블록 노드와
/// NBD 서버는 QMP로 만든다.
pub fn build_replica_receiver_cmdline(qmp_socket: &Path, pidfile: &Path) -> Vec<String> {
    [
        "-machine",
        "none",
        "-nodefaults",
        "-display",
        "none",
        "-S",
        "-qmp",
    ]
    .map(String::from)
    .into_iter()
    .chain([
        format!("unix:{},server=on,wait=off", esc_path(qmp_socket)),
        "-pidfile".into(),
        pidfile.display().to_string(),
    ])
    .collect()
}

/// 실행 중 붙인 장치(순번이 켤 때 수 이상)의 루트 포트 버스 옵션. 켤 때 단 장치는 `pcie.0`이라 빈 문자열이다.
/// 포트가 있는지는 [`check_compat`]이 먼저 본다.
fn hot_bus(ports: Option<(u8, &[u8])>, i: usize) -> String {
    match ports {
        Some((boot, ports)) if i >= usize::from(boot) => ports
            .get(i - usize::from(boot))
            .map_or_else(String::new, |p| format!(",bus=hp{p}")),
        _ => String::new(),
    }
}

/// RBD 이미지를 여는 `rbd` 블록 드라이버 옵션. 네임스페이스와 conf는 있을 때만 넣는다.
fn rbd_source(rbd: &RbdTarget) -> String {
    let mut s = format!("driver=rbd,pool={}", esc(&rbd.pool));
    if let Some(ns) = &rbd.namespace {
        s.push_str(&format!(",namespace={}", esc(ns)));
    }
    s.push_str(&format!(
        ",image={},user={}",
        esc(&rbd.image),
        esc(&rbd.user)
    ));
    if let Some(conf) = &rbd.conf {
        s.push_str(&format!(",conf={}", esc(conf)));
    }
    s
}

fn check_compat(spec: &VmRuntimeSpec, host: &HostProfile) -> Result<(), CmdlineError> {
    match (spec.machine, host.arch) {
        (MachineType::Q35, Arch::X86_64) | (MachineType::Virt, Arch::Aarch64) => {}
        (machine, arch) => return Err(CmdlineError::MachineArchMismatch { machine, arch }),
    }
    if host.arch == Arch::Aarch64 && spec.firmware == Firmware::Bios {
        return Err(CmdlineError::BiosOnAarch64);
    }
    let sata = spec.disks.iter().filter(|d| d.bus == DiskBus::Sata).count();
    if sata > 0 && spec.machine != MachineType::Q35 {
        return Err(CmdlineError::SataRequiresQ35);
    }
    if sata > Q35_SATA_PORTS {
        return Err(CmdlineError::TooManySataDisks);
    }
    if let Some(layout) = &spec.hotplug {
        check_layout(spec, host, layout)?;
    }
    Ok(())
}

fn check_layout(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    layout: &HotplugLayout,
) -> Result<(), CmdlineError> {
    // aarch64 `virt`는 CPU 핫플러그가 없다(QEMU 11.1까지).
    let boot = spec.cpu.vcpus();
    if host.arch == Arch::Aarch64 && (spec.cpu.max() > boot || u32::from(layout.vcpus) > boot) {
        return Err(CmdlineError::CpuHotplugUnsupported);
    }
    if layout.ports > HOTPLUG_PORTS {
        return Err(CmdlineError::TooManyPorts);
    }
    let missing = |kind, count: usize, boot: u8, ports: &[u8]| {
        (usize::from(boot)..count)
            .find(|&i| ports.get(i - usize::from(boot)).is_none())
            .map(|i| CmdlineError::MissingPort(kind, i))
    };
    if let Some(e) = missing(
        "disk",
        spec.disks.len(),
        layout.boot_disks,
        &layout.disk_ports,
    )
    .or_else(|| missing("nic", spec.nics.len(), layout.boot_nics, &layout.nic_ports))
    {
        return Err(e);
    }
    Ok(())
}

#[derive(Default)]
struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, flag: &str) {
        self.0.push(flag.to_string());
    }

    fn opt(&mut self, flag: &str, value: String) {
        self.0.push(flag.to_string());
        self.0.push(value);
    }
}

/// QEMU 옵션 값 안의 쉼표는 두 번 써서 이스케이프한다.
fn esc(s: &str) -> String {
    s.replace(',', ",,")
}

fn esc_path(p: &Path) -> String {
    esc(&p.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use opl_types::runtime::{DiskKind, RbdTarget, RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn replica_receiver_has_no_guest_devices() {
        let args = build_replica_receiver_cmdline(
            Path::new("/run/opl/replica-x,y/qmp.sock"),
            Path::new("/run/opl/replica-x,y/qemu.pid"),
        );
        assert_eq!(
            args,
            [
                "-machine",
                "none",
                "-nodefaults",
                "-display",
                "none",
                "-S",
                "-qmp",
                "unix:/run/opl/replica-x,,y/qmp.sock,server=on,wait=off",
                "-pidfile",
                "/run/opl/replica-x,y/qemu.pid",
            ]
        );
    }

    fn x86_host() -> HostProfile {
        HostProfile {
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
            uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
        }
    }

    fn mac_host() -> HostProfile {
        HostProfile::homebrew_hvf(Path::new("/opt/homebrew/share/qemu"))
    }

    fn paths() -> RuntimePaths {
        RuntimePaths::new(Path::new("/run/opl"), Uuid::nil()).unwrap()
    }

    fn x86_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::nil(),
            name: "web-01".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 4,
                model: CpuModel::Host,
                max_vcpus: None,
            },
            memory_mib: 8192,
            firmware: Firmware::Uefi,
            machine: MachineType::Q35,
            disks: vec![
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-root.qcow2".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Qcow2,
                    bus: DiskBus::VirtioBlk,
                    boot_order: Some(1),
                },
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-data.raw".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Raw,
                    bus: DiskBus::Sata,
                    boot_order: None,
                },
            ],
            nics: vec![
                RuntimeNic {
                    mac: "52:54:00:12:34:56".parse().unwrap(),
                    model: NicModel::Virtio,
                    backend: NicBackend::Tap {
                        ifname: "tap0i0".into(),
                        bridge: "vmbr0".into(),
                        vlan: Some(10),
                        mtu: None,
                    },
                },
                RuntimeNic {
                    mac: "52:54:00:12:34:57".parse().unwrap(),
                    model: NicModel::E1000e,
                    backend: NicBackend::User,
                },
            ],
            guest_agent: false,
            hotplug: None,
            cdroms: vec![],
        }
    }

    fn mac_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            machine: MachineType::Virt,
            disks: vec![RuntimeDisk {
                path: "/tmp/opl/root.qcow2".into(),
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
            ..x86_spec()
        }
    }

    #[test]
    fn x86_kvm_uefi_full() {
        let args = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn aarch64_hvf_minimal() {
        let args = build_cmdline(&mac_spec(), &mac_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    /// 받는 쪽은 원본과 같은 장치(블록 노드 이름·장치 id)에 `-incoming defer`만 더한다.
    #[test]
    fn incoming_adds_defer_to_the_same_devices() {
        let normal = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        let incoming = build_incoming_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..normal.len()], normal[..]);
        assert_eq!(incoming[normal.len()..], ["-incoming", "defer"]);
        assert!(incoming.contains(&"-S".to_string()));
        assert!(!normal.contains(&"-incoming".to_string()));
        // 맞지 않는 조합은 똑같이 거절한다.
        assert_eq!(
            build_incoming_cmdline(&x86_spec(), &mac_host(), &paths()).unwrap_err(),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
    }

    /// VXLAN 네트워크의 MTU는 virtio NIC에만 `host_mtu`로 알린다.
    #[test]
    fn tap_mtu_sets_virtio_host_mtu() {
        let mut spec = x86_spec();
        let tap = NicBackend::Tap {
            ifname: "tap0i0".into(),
            bridge: "oplv1000".into(),
            vlan: None,
            mtu: Some(1450),
        };
        spec.nics[0].backend = tap.clone();
        spec.nics[1].backend = tap;
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56,id=nic0,host_mtu=1450".to_string()
        ));
        assert!(args.contains(&"e1000e,netdev=net1,mac=52:54:00:12:34:57,id=nic1".to_string()));
    }

    /// 게스트 감시를 켠 VM만 QGA 채널(소켓 서버, virtio-serial 포트)을 단다. 받는 쪽도 같다. aarch64(virt)도
    /// PCI 장치를 쓴다.
    #[test]
    fn guest_agent_adds_a_qga_channel() {
        let qga = [
            "-chardev",
            "socket,id=qga0,path=/run/opl/vm/00000000-0000-0000-0000-000000000000/qga.sock,server=on,wait=off",
            "-device",
            "virtio-serial-pci,id=qgaser",
            "-device",
            "virtserialport,bus=qgaser.0,chardev=qga0,name=org.qemu.guest_agent.0",
        ];
        for (spec, host) in [(x86_spec(), x86_host()), (mac_spec(), mac_host())] {
            let without = build_cmdline(&spec, &host, &paths()).unwrap();
            assert!(!without.iter().any(|a| a.contains("qga")), "{without:?}");
            let watched = VmRuntimeSpec {
                guest_agent: true,
                ..spec
            };
            let with = build_cmdline(&watched, &host, &paths()).unwrap();
            let at = with.iter().position(|a| a == "-chardev").unwrap();
            assert_eq!(with[at..at + qga.len()], qga);
            // 앞 장치(디스크·NIC)의 순서는 그대로이고 채널만 끼어든다.
            assert_eq!(with[..at], without[..at]);
            assert_eq!(with[at + qga.len()..], without[at..]);
            let incoming = build_incoming_cmdline(&watched, &host, &paths()).unwrap();
            assert_eq!(incoming[..with.len()], with[..]);
        }
    }

    #[test]
    fn bios_omits_pflash() {
        let spec = VmRuntimeSpec {
            firmware: Firmware::Bios,
            ..x86_spec()
        };
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(!args.iter().any(|a| a.contains("pflash")));
    }

    #[test]
    fn host_cpu_falls_back_to_max_under_tcg() {
        let host = HostProfile {
            accel: Accel::Tcg,
            ..x86_host()
        };
        let args = build_cmdline(&x86_spec(), &host, &paths()).unwrap();
        let i = args.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(args[i + 1], "max");
        assert!(args.contains(&"q35,accel=tcg".to_string()));
    }

    #[test]
    fn escapes_commas_in_names_and_paths() {
        let mut spec = x86_spec();
        spec.name = "a,b".into();
        spec.disks[0].path = "/pool/x,y.qcow2".into();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"guest=a,,b,debug-threads=on".to_string()));
        assert!(
            args.contains(
                &"driver=file,filename=/pool/x,,y.qcow2,node-name=disk0-file".to_string()
            )
        );
    }

    /// 블록 장치 디스크는 `host_device`로 연다. 포맷 노드는 파일 디스크와 같다.
    #[test]
    fn block_disks_use_host_device() {
        let mut spec = x86_spec();
        spec.disks[1].path = "/dev/vg0/opl-a".into();
        spec.disks[1].kind = DiskKind::Block;
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=host_device,filename=/dev/vg0/opl-a,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        assert!(args.contains(
            &"driver=file,filename=/var/lib/opl/pools/local/web-01-root.qcow2,node-name=disk0-file"
                .to_string()
        ));
    }

    /// RBD 디스크는 QEMU의 `rbd` 드라이버로 연다. 네임스페이스와 conf는 있을 때만 넘긴다.
    #[test]
    fn rbd_disks_use_rbd_driver() {
        let mut spec = x86_spec();
        spec.disks[0] = RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "opl".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            }),
            format: ImageFormat::Raw,
            ..spec.disks[0].clone()
        };
        spec.disks[1].path = PathBuf::new();
        spec.disks[1].kind = DiskKind::Rbd;
        spec.disks[1].rbd = Some(RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-b".into(),
            user: "admin".into(),
            conf: None,
        });
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=rbd,pool=rbd,namespace=opl,image=opl-a,user=opl,conf=/etc/ceph/opl.conf,node-name=disk0-file"
                .to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk0-file,node-name=disk0".to_string()));
        assert!(args.contains(
            &"driver=rbd,pool=rbd,image=opl-b,user=admin,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        // 이미지가 없는 RBD 디스크는 열 수 없다.
        spec.disks[1].rbd = None;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::MissingRbdImage(1)
        );
    }

    /// 핫플러그 배치: 켤 때 vCPU 2·상한 8·지금 4, virtio-mem 4096+12288(꽂은 2048), 포트 8, 실행 중 붙인 디스크(hp0)와
    /// NIC(hp1) 하나씩.
    fn hotplug_spec() -> VmRuntimeSpec {
        let mut spec = x86_spec();
        spec.cpu.cores = 2;
        spec.cpu.max_vcpus = Some(8);
        spec.memory_mib = 4096;
        spec.disks.push(RuntimeDisk {
            path: "/var/lib/opl/pools/local/web-01-hot.qcow2".into(),
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        });
        spec.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:58".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        });
        spec.hotplug = Some(HotplugLayout {
            vcpus: 4,
            max_memory_mib: 16384,
            plugged_mib: 2048,
            ports: HOTPLUG_PORTS,
            boot_disks: 2,
            boot_nics: 2,
            disk_ports: vec![0],
            nic_ports: vec![1],
        });
        spec
    }

    #[test]
    fn x86_kvm_hotplug_layout() {
        let args = build_cmdline(&hotplug_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    /// 배치가 없으면 상한이 있어도 옛 명령줄과 바이트까지 같다.
    #[test]
    fn without_a_layout_the_maximums_change_nothing() {
        let mut spec = x86_spec();
        let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        spec.cpu.max_vcpus = Some(16);
        assert_eq!(build_cmdline(&spec, &x86_host(), &paths()).unwrap(), before);
    }

    /// 상한이 없는 배치는 `-smp`·`-m`이 그대로이고 빈 루트 포트만 더한다.
    #[test]
    fn a_layout_without_maximums_adds_only_root_ports() {
        let mut spec = x86_spec();
        let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        spec.hotplug = Some(HotplugLayout {
            vcpus: 4,
            max_memory_mib: 8192,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 2,
            boot_nics: 2,
            disk_ports: vec![],
            nic_ports: vec![],
        });
        let after = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        let ports: Vec<String> = (0..8)
            .flat_map(|k| {
                let mut port = format!(
                    "pcie-root-port,id=hp{k},bus=pcie.0,chassis={},addr=0x1e.{k},io-reserve=0",
                    k + 1
                );
                if k == 0 {
                    port.push_str(",multifunction=on");
                }
                ["-device".to_string(), port]
            })
            .collect();
        let at = after
            .iter()
            .position(|a| a.starts_with("pcie-root-port"))
            .unwrap()
            - 1;
        assert_eq!(after[at..at + ports.len()], ports[..]);
        assert_eq!(after[..at], before[..at]);
        assert_eq!(after[at + ports.len()..], before[at..]);
        assert!(after.contains(&"sockets=1,cores=4,threads=1".to_string()));
        assert!(after.contains(&"8192M".to_string()));
    }

    /// 실행 중 붙인 디스크·NIC는 루트 포트에 놓여 `pcie.0` 슬롯을 먹지 않는다: 그것을 빼면 나머지 인자가 배치만 있는
    /// 명령줄과 같다. 받는 쪽도 같은 배치다.
    #[test]
    fn hot_added_devices_go_on_root_ports() {
        let spec = hotplug_spec();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"virtio-blk-pci,drive=disk2,id=disk2-dev,bus=hp0".to_string()));
        assert!(args.contains(
            &"virtio-net-pci,netdev=net2,mac=52:54:00:12:34:58,id=nic2,bus=hp1".to_string()
        ));
        // 켤 때 단 장치에는 버스가 없다.
        assert!(args.contains(&"virtio-blk-pci,drive=disk0,id=disk0-dev,bootindex=1".to_string()));
        let mut fewer = spec.clone();
        fewer.disks.pop();
        fewer.nics.pop();
        let layout = fewer.hotplug.as_mut().unwrap();
        layout.disk_ports.clear();
        layout.nic_ports.clear();
        let without = build_cmdline(&fewer, &x86_host(), &paths()).unwrap();
        let hot = |a: &String| a.contains("disk2") || a.contains("net2") || a.contains("nic2");
        let kept: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, a)| !hot(a) && !args.get(i + 1).is_some_and(hot))
            .map(|(_, a)| a)
            .collect();
        assert_eq!(kept, without.iter().collect::<Vec<_>>());
        let incoming = build_incoming_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..args.len()], args[..]);
    }

    /// aarch64(`virt`)는 CPU 핫플러그가 없으므로 vCPU 상한이나 붙인 vCPU가 있는 배치를 거절한다. 디스크·NIC 포트는 된다.
    #[test]
    fn aarch64_rejects_cpu_hotplug() {
        let layout = HotplugLayout {
            vcpus: 4,
            max_memory_mib: 8192,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 1,
            boot_nics: 1,
            disk_ports: vec![],
            nic_ports: vec![],
        };
        let spec = VmRuntimeSpec {
            hotplug: Some(layout.clone()),
            ..mac_spec()
        };
        let args = build_cmdline(&spec, &mac_host(), &paths()).unwrap();
        assert!(args.contains(&"sockets=1,cores=4,threads=1".to_string()));
        assert!(args.iter().any(|a| a.starts_with("pcie-root-port,id=hp7")));
        let mut max = spec.clone();
        max.cpu.max_vcpus = Some(8);
        assert_eq!(
            build_cmdline(&max, &mac_host(), &paths()).unwrap_err(),
            CmdlineError::CpuHotplugUnsupported
        );
        let mut added = spec;
        added.hotplug = Some(HotplugLayout { vcpus: 5, ..layout });
        assert_eq!(
            build_incoming_cmdline(&added, &mac_host(), &paths()).unwrap_err(),
            CmdlineError::CpuHotplugUnsupported
        );
    }

    #[test]
    fn rejects_bad_layouts() {
        let err = |spec: &VmRuntimeSpec| build_cmdline(spec, &x86_host(), &paths()).unwrap_err();
        let mut many = hotplug_spec();
        many.hotplug.as_mut().unwrap().ports = HOTPLUG_PORTS + 1;
        assert_eq!(err(&many), CmdlineError::TooManyPorts);
        let mut no_disk_port = hotplug_spec();
        no_disk_port.hotplug.as_mut().unwrap().disk_ports.clear();
        assert_eq!(err(&no_disk_port), CmdlineError::MissingPort("disk", 2));
        let mut no_nic_port = hotplug_spec();
        no_nic_port.hotplug.as_mut().unwrap().nic_ports.clear();
        assert_eq!(err(&no_nic_port), CmdlineError::MissingPort("nic", 2));
    }

    #[test]
    fn rejects_incompatible_combinations() {
        let err = |spec: &VmRuntimeSpec, host: &HostProfile| {
            build_cmdline(spec, host, &paths()).unwrap_err()
        };
        assert_eq!(
            err(&x86_spec(), &mac_host()),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
        assert_eq!(
            err(
                &VmRuntimeSpec {
                    firmware: Firmware::Bios,
                    ..mac_spec()
                },
                &mac_host()
            ),
            CmdlineError::BiosOnAarch64
        );
        let mut sata_on_virt = mac_spec();
        sata_on_virt.disks[0].bus = DiskBus::Sata;
        assert_eq!(
            err(&sata_on_virt, &mac_host()),
            CmdlineError::SataRequiresQ35
        );
        let mut many = x86_spec();
        many.disks = (0..7)
            .map(|i| RuntimeDisk {
                path: format!("/d{i}.raw").into(),
                kind: DiskKind::File,
                rbd: None,
                format: ImageFormat::Raw,
                bus: DiskBus::Sata,
                boot_order: None,
            })
            .collect();
        assert_eq!(err(&many, &x86_host()), CmdlineError::TooManySataDisks);
    }
}
