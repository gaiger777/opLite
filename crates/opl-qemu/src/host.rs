use std::path::{Path, PathBuf};

use opl_types::node::NodeHotplug;
use opl_types::vm::Arch;

use crate::firmware::{self, FirmwarePair};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    Kvm,
    Hvf,
    Tcg,
}

impl Accel {
    pub fn as_str(self) -> &'static str {
        match self {
            Accel::Kvm => "kvm",
            Accel::Hvf => "hvf",
            Accel::Tcg => "tcg",
        }
    }
}

impl From<Accel> for opl_types::cpu::Accel {
    fn from(a: Accel) -> Self {
        match a {
            Accel::Kvm => Self::Kvm,
            Accel::Hvf => Self::Hvf,
            Accel::Tcg => Self::Tcg,
        }
    }
}

/// 이 호스트에서 QEMU를 어떻게 실행할지에 대한 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProfile {
    pub arch: Arch,
    pub accel: Accel,
    pub qemu_binary: PathBuf,
    pub uefi_code: PathBuf,
    /// VM마다 복사해서 쓰는 UEFI 변수 저장소 원본
    pub uefi_vars_template: PathBuf,
    /// Secure Boot 펌웨어(MS 키 등록, 계획 59). firmware descriptor로 찾고 없으면 None이다. 일반 UEFI VM은 쓰지 않는다.
    pub secure_boot: Option<FirmwarePair>,
}

impl HostProfile {
    /// Homebrew QEMU가 설치된 Apple Silicon Mac. `share_dir`는 보통 `/opt/homebrew/share/qemu`.
    pub fn homebrew_hvf(share_dir: &Path) -> Self {
        Self {
            arch: Arch::Aarch64,
            accel: Accel::Hvf,
            qemu_binary: PathBuf::from("qemu-system-aarch64"),
            uefi_code: share_dir.join("edk2-aarch64-code.fd"),
            uefi_vars_template: share_dir.join("edk2-arm-vars.fd"),
            secure_boot: None,
        }
    }

    /// Debian/Ubuntu의 QEMU와 UEFI 펌웨어(`ovmf`, `qemu-efi-aarch64` 패키지).
    pub fn debian(arch: Arch, accel: Accel) -> Self {
        match arch {
            Arch::X86_64 => Self {
                arch,
                accel,
                qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd"),
                uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd"),
                secure_boot: None,
            },
            Arch::Aarch64 => Self {
                arch,
                accel,
                qemu_binary: PathBuf::from("/usr/bin/qemu-system-aarch64"),
                uefi_code: PathBuf::from("/usr/share/AAVMF/AAVMF_CODE.fd"),
                uefi_vars_template: PathBuf::from("/usr/share/AAVMF/AAVMF_VARS.fd"),
                secure_boot: None,
            },
        }
    }

    /// 이 호스트의 기본 프로필. macOS는 Homebrew QEMU와 hvf, Linux는 Debian 경로와
    /// KVM(`/dev/kvm`을 열 수 없으면 TCG)이다. Secure Boot 펌웨어는 firmware descriptor로 찾는다(macOS는
    /// `{share_dir}/firmware`, Linux는 [`firmware::SYSTEM_DIRS`]).
    pub fn native(share_dir: &Path) -> Self {
        let (mut profile, dirs) = if cfg!(target_os = "macos") {
            (
                Self::homebrew_hvf(share_dir),
                firmware::homebrew_dirs(share_dir),
            )
        } else {
            let accel = if kvm_available() {
                Accel::Kvm
            } else {
                Accel::Tcg
            };
            let dirs = firmware::SYSTEM_DIRS.map(PathBuf::from).to_vec();
            (Self::debian(Arch::native(), accel), dirs)
        };
        profile.secure_boot = firmware::discover(&dirs, profile.arch);
        profile
    }
}

/// `/dev/kvm`을 읽고 쓸 수 있는지. QEMU의 `accel=kvm`이 여는 장치다.
pub fn kvm_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

/// 노드의 핫플러그 능력. vCPU는 Linux(`linux`)의 x86_64만(aarch64 `virt`는 QEMU 11.1까지 CPU 핫플러그가 없고,
/// macOS x86_64의 HVF도 아니다), 메모리는 `qemu-system-* -device help` 출력에 `virtio-mem-pci`가 있을 때다(macOS
/// Homebrew QEMU에는 없다). 장치가 꽂을 때 미리 잡기(`prealloc`, QEMU 7.1+)는 `-device virtio-mem-pci,help`
/// 출력(`mem_help`)에 `prealloc=`이 있을 때다(계획 64 리뷰 2 — 없으면 hugepages VM을 메모리 핫플러그 없이 켠다).
pub fn hotplug_caps(arch: Arch, linux: bool, device_help: &str, mem_help: &str) -> NodeHotplug {
    let memory = device_help.contains("\"virtio-mem-pci\"");
    NodeHotplug {
        cpu: linux && arch == Arch::X86_64,
        memory,
        memory_prealloc: memory && mem_help.contains("prealloc="),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_profiles_match_the_arch() {
        let x86 = HostProfile::debian(Arch::X86_64, Accel::Kvm);
        assert_eq!(x86.qemu_binary, Path::new("/usr/bin/qemu-system-x86_64"));
        assert!(x86.uefi_code.ends_with("OVMF_CODE_4M.fd"));
        let arm = HostProfile::debian(Arch::Aarch64, Accel::Tcg);
        assert_eq!(arm.accel, Accel::Tcg);
        assert!(arm.uefi_vars_template.ends_with("AAVMF_VARS.fd"));
    }

    /// 리뷰 2 N4: `-device virtio-mem-pci,help`에 `prealloc=<bool>`이 있는 QEMU(7.1+, svc01 8.2.2)만 미리 잡는다.
    #[test]
    fn virtio_mem_prealloc_is_read_from_the_device_help() {
        let with = "name \"virtio-mem-pci\", bus PCI\n";
        let q82 = "virtio-mem-pci options:\n  block-size=<size>\n  prealloc=<bool>        - (default: false)\n  requested-size=<size>\n";
        let q62 = "virtio-mem-pci options:\n  block-size=<size>\n  requested-size=<size>\n";
        assert!(hotplug_caps(Arch::X86_64, true, with, q82).memory_prealloc);
        let old = hotplug_caps(Arch::X86_64, true, with, q62);
        assert!(old.memory && !old.memory_prealloc);
        assert!(!hotplug_caps(Arch::X86_64, true, "", q82).memory_prealloc);
    }

    /// `-device help`의 한 줄 모양(QEMU 8.2·11.1): `name "virtio-mem-pci", bus PCI`
    #[test]
    fn hotplug_caps_read_the_arch_and_the_device_list() {
        let with = "Misc devices:\nname \"pxb\", bus PCI\nname \"virtio-mem-pci\", bus PCI\n";
        let without =
            "Misc devices:\nname \"virtio-balloon-pci\", bus PCI\nname \"virtio-mem-pci-base\"\n";
        assert_eq!(
            hotplug_caps(Arch::X86_64, true, with, ""),
            NodeHotplug {
                cpu: true,
                memory: true,
                memory_prealloc: false
            }
        );
        assert_eq!(
            hotplug_caps(Arch::X86_64, true, without, ""),
            NodeHotplug {
                cpu: true,
                memory: false,
                memory_prealloc: false
            }
        );
        assert_eq!(
            hotplug_caps(Arch::Aarch64, true, with, ""),
            NodeHotplug {
                cpu: false,
                memory: true,
                memory_prealloc: false
            }
        );
        assert_eq!(
            hotplug_caps(Arch::Aarch64, true, "", ""),
            NodeHotplug::default()
        );
        // macOS x86_64(HVF)는 vCPU 핫플러그를 하지 않는다.
        assert_eq!(
            hotplug_caps(Arch::X86_64, false, with, ""),
            NodeHotplug {
                cpu: false,
                memory: true,
                memory_prealloc: false
            }
        );
    }

    #[test]
    fn native_profile_uses_this_arch() {
        let p = HostProfile::native(Path::new("/opt/homebrew/share/qemu"));
        assert_eq!(p.arch, Arch::native());
        if cfg!(target_os = "macos") {
            assert_eq!(p.accel, Accel::Hvf);
        } else {
            assert_ne!(p.accel, Accel::Hvf);
        }
    }
}
