//! QEMU firmware descriptor(`docs/interop/firmware.json`)로 Secure Boot 펌웨어를 찾는다(계획 59).
//!
//! 디렉터리는 앞의 것이 우선이다. 같은 이름의 파일은 앞 디렉터리의 것이 뒤의 것을 가리고(빈 파일이면 그 이름을
//! 끈다), 남은 파일은 이름 순서가 우선순위다(QEMU 규칙). 고르는 조건은 `uefi` 인터페이스, `flash` 매핑(`mode`가
//! 없거나 `split`), 이 아키텍처와 머신의 대상, 기능 `secure-boot`·`enrolled-keys`(x86_64면 `requires-smm`도)다.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbivisor_types::vm::Arch;
use serde_json::Value;

/// Secure Boot 펌웨어 한 쌍: 읽기 전용 코드와 VM마다 복사하는 변수 저장소 원본(MS 키 등록)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwarePair {
    pub code: PathBuf,
    pub vars_template: PathBuf,
}

/// Debian·Ubuntu의 descriptor 디렉터리(QEMU가 읽는 차례)
pub const SYSTEM_DIRS: [&str; 2] = ["/etc/qemu/firmware", "/usr/share/qemu/firmware"];

/// `dirs`의 descriptor에서 `arch`의 Secure Boot 펌웨어를 찾는다. 없거나 읽지 못하면 None이다.
pub fn discover(dirs: &[PathBuf], arch: Arch) -> Option<FirmwarePair> {
    // 파일 이름 → 처음 본 디렉터리의 경로(앞 디렉터리가 가린다)
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.ends_with(".json") {
                files.entry(name).or_insert_with(|| entry.path());
            }
        }
    }
    files.values().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        // 빈 파일은 뒤 디렉터리의 같은 이름 파일을 끈다.
        if text.trim().is_empty() {
            return None;
        }
        let desc: Value = serde_json::from_str(&text).ok()?;
        secure_boot_pair(&desc, arch)
    })
}

/// descriptor 하나가 `arch`의 MS 키 등록 Secure Boot 펌웨어면 그 코드·변수 원본
pub fn secure_boot_pair(desc: &Value, arch: Arch) -> Option<FirmwarePair> {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    if !strings(&desc["interface-types"])
        .iter()
        .any(|t| t == "uefi")
    {
        return None;
    }
    let mapping = &desc["mapping"];
    if mapping["device"].as_str() != Some("flash") {
        return None;
    }
    if !matches!(mapping["mode"].as_str(), None | Some("split")) {
        return None;
    }
    let (arch_name, machine) = match arch {
        Arch::X86_64 => ("x86_64", "pc-q35-11.1"),
        Arch::Aarch64 => ("aarch64", "virt-11.1"),
    };
    let targets = desc["targets"].as_array()?;
    let target = targets.iter().any(|t| {
        t["architecture"].as_str() == Some(arch_name)
            && strings(&t["machines"]).iter().any(|m| glob(m, machine))
    });
    if !target {
        return None;
    }
    let features = strings(&desc["features"]);
    let has = |f: &str| features.iter().any(|x| x == f);
    if !has("secure-boot")
        || !has("enrolled-keys")
        || (arch == Arch::X86_64 && !has("requires-smm"))
    {
        return None;
    }
    let file = |v: &Value| v["filename"].as_str().map(PathBuf::from);
    // 원시(raw) 형식만 pflash로 연다.
    let raw = |v: &Value| matches!(v["format"].as_str(), None | Some("raw"));
    let (exec, vars) = (&mapping["executable"], &mapping["nvram-template"]);
    if !raw(exec) || !raw(vars) {
        return None;
    }
    Some(FirmwarePair {
        code: file(exec)?,
        vars_template: file(vars)?,
    })
}

/// `*`만 아는 glob
fn glob(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => {
            let Some(tail) = name.strip_prefix(head) else {
                return false;
            };
            (0..=tail.len())
                .filter(|&i| tail.is_char_boundary(i))
                .any(|i| glob(rest, &tail[i..]))
        }
    }
}

/// Homebrew QEMU의 descriptor 디렉터리(`{share_dir}/firmware`)
pub fn homebrew_dirs(share_dir: &Path) -> Vec<PathBuf> {
    vec![share_dir.join("firmware")]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Debian `ovmf`의 MS 키 등록 descriptor 모양
    fn debian_ms() -> Value {
        json!({
            "description": "OVMF with SB+SMM, SB enabled, MS certs enrolled",
            "interface-types": ["uefi"],
            "mapping": {
                "device": "flash",
                "mode": "split",
                "executable": {"filename": "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd", "format": "raw"},
                "nvram-template": {"filename": "/usr/share/OVMF/OVMF_VARS_4M.ms.fd", "format": "raw"}
            },
            "targets": [{"architecture": "x86_64", "machines": ["pc-q35-*"]}],
            "features": ["acpi-s3", "enrolled-keys", "requires-smm", "secure-boot", "verbose-dynamic"],
            "tags": []
        })
    }

    #[test]
    fn picks_the_debian_enrolled_firmware() {
        assert_eq!(
            secure_boot_pair(&debian_ms(), Arch::X86_64),
            Some(FirmwarePair {
                code: "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd".into(),
                vars_template: "/usr/share/OVMF/OVMF_VARS_4M.ms.fd".into(),
            })
        );
        assert_eq!(secure_boot_pair(&debian_ms(), Arch::Aarch64), None);
    }

    #[test]
    fn skips_descriptors_that_miss_a_condition() {
        let cases = [
            ("no enrolled keys", json!(["requires-smm", "secure-boot"])),
            ("no secure boot", json!(["enrolled-keys", "requires-smm"])),
            ("no smm on x86_64", json!(["enrolled-keys", "secure-boot"])),
        ];
        for (why, features) in &cases {
            let mut d = debian_ms();
            d["features"] = features.clone();
            assert_eq!(secure_boot_pair(&d, Arch::X86_64), None, "{why}");
        }
        let mut d = debian_ms();
        d["mapping"]["mode"] = json!("stateless");
        assert_eq!(secure_boot_pair(&d, Arch::X86_64), None);
        let mut d = debian_ms();
        d["mapping"]["device"] = json!("memory");
        assert_eq!(secure_boot_pair(&d, Arch::X86_64), None);
        let mut d = debian_ms();
        d["interface-types"] = json!(["bios"]);
        assert_eq!(secure_boot_pair(&d, Arch::X86_64), None);
        let mut d = debian_ms();
        d["targets"] = json!([{"architecture": "x86_64", "machines": ["pc-i440fx-*"]}]);
        assert_eq!(secure_boot_pair(&d, Arch::X86_64), None);
        let mut d = debian_ms();
        d["mapping"]["nvram-template"]["format"] = json!("qcow2");
        assert_eq!(secure_boot_pair(&d, Arch::X86_64), None);
        // aarch64는 SMM이 없어도 된다.
        let mut d = debian_ms();
        d["targets"] = json!([{"architecture": "aarch64", "machines": ["virt-*"]}]);
        d["features"] = json!(["enrolled-keys", "secure-boot"]);
        assert!(secure_boot_pair(&d, Arch::Aarch64).is_some());
    }

    #[test]
    fn glob_matches_star_patterns() {
        assert!(glob("pc-q35-*", "pc-q35-11.1"));
        assert!(glob("virt-*", "virt-11.1"));
        assert!(glob("pc-q35-11.1", "pc-q35-11.1"));
        assert!(!glob("pc-i440fx-*", "pc-q35-11.1"));
        assert!(glob("*q35*", "pc-q35-11.1"));
    }

    /// 앞 디렉터리의 같은 이름 파일(빈 파일이면 끄기)이 가리고, 이름 순서가 우선순위다.
    #[test]
    fn earlier_dirs_shadow_and_names_order() {
        let etc = tempfile::tempdir().unwrap();
        let usr = tempfile::tempdir().unwrap();
        let write = |dir: &Path, name: &str, code: &str| {
            let mut d = debian_ms();
            d["mapping"]["executable"]["filename"] = json!(code);
            std::fs::write(dir.join(name), d.to_string()).unwrap();
        };
        write(usr.path(), "40-ms.json", "/usr/40");
        write(usr.path(), "50-ms.json", "/usr/50");
        let dirs = [etc.path().to_path_buf(), usr.path().to_path_buf()];
        assert_eq!(
            discover(&dirs, Arch::X86_64).unwrap().code,
            Path::new("/usr/40")
        );
        // /etc의 같은 이름 파일이 가린다.
        write(etc.path(), "40-ms.json", "/etc/40");
        assert_eq!(
            discover(&dirs, Arch::X86_64).unwrap().code,
            Path::new("/etc/40")
        );
        // 빈 파일은 그 이름을 끈다.
        std::fs::write(etc.path().join("40-ms.json"), "").unwrap();
        assert_eq!(
            discover(&dirs, Arch::X86_64).unwrap().code,
            Path::new("/usr/50")
        );
        // 이름이 앞서는 파일이 이긴다.
        write(etc.path(), "10-ms.json", "/etc/10");
        assert_eq!(
            discover(&dirs, Arch::X86_64).unwrap().code,
            Path::new("/etc/10")
        );
        assert_eq!(discover(&[], Arch::X86_64), None);
    }

    /// 이 Mac의 실제 Homebrew descriptor: `50-edk2-x86_64-secure.json`에는 `enrolled-keys`가 없어 고르지 않는다
    /// (QEMU 11.1.2). descriptor가 없는 호스트(Linux CI 등)에서는 아무것도 보지 않는다.
    #[test]
    fn homebrew_descriptors_have_no_enrolled_keys() {
        let dir = Path::new("/opt/homebrew/share/qemu");
        let dirs = homebrew_dirs(dir);
        let secure = dirs[0].join("50-edk2-x86_64-secure.json");
        if !secure.exists() {
            return;
        }
        let desc: Value = serde_json::from_str(&std::fs::read_to_string(&secure).unwrap()).unwrap();
        let features = desc["features"].to_string();
        assert!(features.contains("secure-boot"), "{features}");
        assert!(!features.contains("enrolled-keys"), "{features}");
        assert_eq!(secure_boot_pair(&desc, Arch::X86_64), None);
        // enrolled-keys를 더하면 고른다(해석 자체는 맞다).
        let mut enrolled = desc.clone();
        enrolled["features"]
            .as_array_mut()
            .unwrap()
            .push(json!("enrolled-keys"));
        let pair = secure_boot_pair(&enrolled, Arch::X86_64).unwrap();
        assert!(
            pair.code.ends_with("edk2-x86_64-secure-code.fd"),
            "{pair:?}"
        );
        assert_eq!(discover(&dirs, Arch::X86_64), None);
        assert_eq!(discover(&dirs, Arch::Aarch64), None);
    }
}
