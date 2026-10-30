use std::path::Path;

use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, HostProfile};
use crate::paths::RuntimePaths;

/// q35의 내장 AHCI 컨트롤러 포트 수
const Q35_SATA_PORTS: usize = 6;

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
    #[error("rbd disks are not supported yet")]
    RbdDisksUnsupported,
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
    a.opt(
        "-smp",
        format!(
            "sockets={},cores={},threads=1",
            spec.cpu.sockets, spec.cpu.cores
        ),
    );
    a.opt("-m", format!("{}M", spec.memory_mib));

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

    let mut sata_port = 0;
    for (i, disk) in spec.disks.iter().enumerate() {
        let fmt = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        let proto = match disk.kind {
            DiskKind::File => "file",
            DiskKind::Block => "host_device",
            DiskKind::Rbd => return Err(CmdlineError::RbdDisksUnsupported),
        };
        a.opt(
            "-blockdev",
            format!(
                "driver={proto},filename={},node-name=disk{i}-file",
                esc_path(&disk.path)
            ),
        );
        a.opt(
            "-blockdev",
            format!("driver={fmt},file=disk{i}-file,node-name=disk{i}"),
        );
        let mut dev = match disk.bus {
            DiskBus::VirtioBlk => format!("virtio-blk-pci,drive=disk{i},id=disk{i}-dev"),
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
        a.opt(
            "-device",
            format!("{model},netdev=net{i},mac={},id=nic{i}", nic.mac),
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

    use opl_types::runtime::{DiskKind, RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
    use uuid::Uuid;

    use super::*;

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
                    },
                },
                RuntimeNic {
                    mac: "52:54:00:12:34:57".parse().unwrap(),
                    model: NicModel::E1000e,
                    backend: NicBackend::User,
                },
            ],
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

    /// RBD 디스크는 아직 열지 않는다.
    #[test]
    fn rbd_disks_are_unsupported_for_now() {
        let mut spec = x86_spec();
        spec.disks[1].kind = DiskKind::Rbd;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::RbdDisksUnsupported
        );
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
