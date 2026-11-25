//! 실제 QEMU에 CPU 모델·플래그를 묻는 탐침(계획 63 §6) 통합 테스트.
//! `cargo test -p orbivisor-qemu --features qemu-it --test cpu_probe -- --nocapture`
//!
//! 이 호스트의 기본 가속기와 TCG(x86_64·aarch64 QEMU가 있으면 둘 다)로 묻는다. `OPL_RECORD_CPU_FIXTURES=DIR`이면 받은
//! 응답을 `DIR/{arch}-{accel}.json`으로 남긴다(`tests/fixtures/cpu/`의 고정물을 이렇게 만들었다). KVM이면 응답을 로그에도
//! 찍는다(GitHub `linux-kvm`의 KVM 고정물을 옮기려고).
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};

use orbivisor_qemu::cpu::{PROBE_TIMEOUT, build_catalog, probe, probe_replies};
use orbivisor_qemu::{Accel, CpuProbeError, HostProfile};
use orbivisor_types::vm::Arch;

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

fn host() -> HostProfile {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::native(Path::new(&share))
}

/// 이 호스트에서 물을 (QEMU, 아키텍처, 가속기): 기본 프로필과, 있는 두 QEMU의 TCG
fn targets() -> Vec<(PathBuf, Arch, Accel)> {
    let native = host();
    let mut out = vec![(native.qemu_binary.clone(), native.arch, native.accel)];
    for (bin, arch) in [
        ("qemu-system-x86_64", Arch::X86_64),
        ("qemu-system-aarch64", Arch::Aarch64),
    ] {
        let found = std::process::Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if found && !(arch == native.arch && native.accel == Accel::Tcg) {
            out.push((PathBuf::from(bin), arch, Accel::Tcg));
        }
    }
    out
}

#[tokio::test]
async fn probes_the_local_qemu() {
    let tmp = short_tempdir();
    for (bin, arch, accel) in targets() {
        let replies = probe_replies(&bin, arch, accel, tmp.path(), PROBE_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{bin:?} {accel:?}: {e}"));
        let arch_name = match arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };
        let name = format!("{arch_name}-{}.json", accel.as_str());
        if let Some(dir) = std::env::var_os("OPL_RECORD_CPU_FIXTURES") {
            let json = serde_json::to_string(&replies.to_json()).unwrap();
            std::fs::write(Path::new(&dir).join(&name), json + "\n").unwrap();
        }
        if accel == Accel::Kvm {
            println!("KVM CPU probe replies ({name}): {}", replies.to_json());
        }
        let p = build_catalog(arch, accel.as_str(), &replies).unwrap();
        let c = &p.catalog;
        let size = serde_json::to_vec(c).unwrap().len();
        println!(
            "{name}: QEMU {}, {} models, {} flags ({} supported), {size} bytes, hash {}, host {:?} {:?}",
            c.qemu,
            c.models.len(),
            c.flags.len(),
            c.supported.count(),
            c.hash(),
            p.vendor,
            p.model_id
        );
        assert_eq!((c.arch, c.accel.as_str()), (arch, accel.as_str()));
        assert!(!c.flags.is_empty() && !c.supported.is_empty());
        assert!(!c.models.is_empty());
        if accel == Accel::Kvm && arch == Arch::X86_64 {
            assert!(p.vendor.is_some(), "KVM reports the host vendor");
        }
        if arch == Arch::Aarch64 && accel != Accel::Tcg {
            assert!(c.models.iter().all(|m| m.missing_other == ["accel"]));
        }
        // 두 번 물어도 같은 카탈로그다.
        let again = probe(&bin, arch, accel, tmp.path(), PROBE_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(again.catalog.hash(), c.hash());
    }
    // 소켓을 남기지 않는다.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_missing_binary_or_bad_accel_is_an_error() {
    let tmp = short_tempdir();
    let err = probe(
        Path::new("/nonexistent/qemu"),
        Arch::Aarch64,
        Accel::Tcg,
        tmp.path(),
        PROBE_TIMEOUT,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CpuProbeError::Io(_)), "{err}");
    // 이 호스트에 없는 가속기: QEMU가 소켓 전에 끝난다.
    let native = host();
    let other = if native.accel == Accel::Hvf {
        Accel::Kvm
    } else {
        Accel::Hvf
    };
    let err = probe(
        &native.qemu_binary,
        native.arch,
        other,
        tmp.path(),
        PROBE_TIMEOUT,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CpuProbeError::Exited(_)), "{err}");
}
