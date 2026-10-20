use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

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
}
