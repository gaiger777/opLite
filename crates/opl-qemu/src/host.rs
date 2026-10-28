use std::path::{Path, PathBuf};

use opl_types::vm::Arch;

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

/// 이 호스트에서 QEMU를 어떻게 실행할지에 대한 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProfile {
    pub arch: Arch,
    pub accel: Accel,
    pub qemu_binary: PathBuf,
    pub uefi_code: PathBuf,
    /// VM마다 복사해서 쓰는 UEFI 변수 저장소 원본
    pub uefi_vars_template: PathBuf,
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
            },
            Arch::Aarch64 => Self {
                arch,
                accel,
                qemu_binary: PathBuf::from("/usr/bin/qemu-system-aarch64"),
                uefi_code: PathBuf::from("/usr/share/AAVMF/AAVMF_CODE.fd"),
                uefi_vars_template: PathBuf::from("/usr/share/AAVMF/AAVMF_VARS.fd"),
            },
        }
    }

    /// 이 호스트의 기본 프로필. macOS는 Homebrew QEMU와 hvf, Linux는 Debian 경로와
    /// KVM(`/dev/kvm`을 열 수 없으면 TCG)이다.
    pub fn native(share_dir: &Path) -> Self {
        if cfg!(target_os = "macos") {
            Self::homebrew_hvf(share_dir)
        } else {
            let accel = if kvm_available() {
                Accel::Kvm
            } else {
                Accel::Tcg
            };
            Self::debian(Arch::native(), accel)
        }
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
