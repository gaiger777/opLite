//! 옛 이름 받기(계획 74). 실행 때 읽는 환경 변수는 `ORBIVISOR_*`이고, 한 릴리스 동안 옛 이름 `OPL_*`도 받아 경고한다.
//! 둘 다 있으면 새 것을 쓰고 옛 것은 말없이 무시한다. 다음 판에서 옛 이름 받기를 뺀다.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::sync::Mutex;

/// 새 이름 접두사
pub const NEW_PREFIX: &str = "ORBIVISOR_";
/// 옛 이름 접두사
pub const OLD_PREFIX: &str = "OPL_";

/// 실행 때 읽어 옛 이름도 받는 환경 변수(접두사 뺀 이름). orbivisorctl·orbivisor-devserver·시험이 읽고,
/// Terraform 공급자(Go)는 `SERVER`·`TOKEN`·`CA_PEM`·`CA_FINGERPRINT`를 같은 규칙으로 읽는다.
pub const RUNTIME_ENV: &[&str] = &[
    "CONFIG",
    "SOCKET",
    "SERVER",
    "TOKEN",
    "CA_PEM",
    "CA_FINGERPRINT",
    "ACME_DNS_SECRET",
    "ACME_EAB_HMAC",
    "ESXI_PASSWORD",
    "VMWARE_PASSWORD",
    "LOG",
    "QEMU_SHARE",
];

/// 옛 이름을 썼을 때의 경고 문장
pub fn deprecated(name: &str) -> String {
    format!("environment variable {OLD_PREFIX}{name} is deprecated; use {NEW_PREFIX}{name}")
}

/// 이미 경고한 이름(한 프로세스에서 이름마다 한 번)
static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// `ORBIVISOR_{name}`을 읽고, 없으면 `OPL_{name}`을 읽는다. 옛 이름을 쓰면 그 이름마다 한 번 표준 오류에
/// 경고한다(이 크레이트는 tracing을 쓰지 않는다). 값은 `std::env::var`처럼 그대로(빈 문자열도) 돌려주고,
/// UTF-8이 아니면 없는 것으로 본다. 새 이름이 있으면(UTF-8이 아니어도) 옛 이름은 보지 않는다.
pub fn env(name: &str) -> Option<String> {
    let (value, warning) = pick(name, |k| std::env::var_os(k));
    if let Some(w) = warning {
        let first = WARNED
            .lock()
            .map(|mut seen| seen.insert(name.to_string()))
            .unwrap_or(true);
        if first {
            eprintln!("warning: {w}");
        }
    }
    value.and_then(|v| v.into_string().ok())
}

/// [`env`]의 본체: 값과(옛 이름을 썼으면) 경고
fn pick(name: &str, get: impl Fn(&str) -> Option<OsString>) -> (Option<OsString>, Option<String>) {
    if let Some(v) = get(&format!("{NEW_PREFIX}{name}")) {
        return (Some(v), None);
    }
    match get(&format!("{OLD_PREFIX}{name}")) {
        Some(v) => (Some(v), Some(deprecated(name))),
        None => (None, None),
    }
}

/// orbivisorctl이 clap `env = "ORBIVISOR_X"`로 읽는 변수(나머지는 쓰는 곳에서 [`env`]로 읽어, 쓸 때만 경고한다)
pub const CLAP_ENV: &[&str] = &["CONFIG", "SERVER", "SOCKET"];

/// clap의 `env = "ORBIVISOR_X"`가 옛 이름도 받게, `names`(접두사 뺀 이름 — [`CLAP_ENV`]) 가운데 새 이름이 없고 옛 이름이
/// 있는 변수마다 (새 이름, 값, 경고)를 돌려준다. 부르는 쪽(main 맨 앞)이 프로세스 환경에 새 이름을 넣고 경고를 낸다.
/// 읽지 않는 변수까지 넣으면 쓰지도 않는 옛 이름마다 경고한다(리뷰 1).
pub fn promotions(
    names: &[&str],
    get: impl Fn(&str) -> Option<OsString>,
) -> Vec<(String, OsString, String)> {
    names
        .iter()
        .filter_map(|name| match pick(name, &get) {
            (Some(v), Some(w)) => Some((format!("{NEW_PREFIX}{name}"), v, w)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn new_name_only() {
        let (v, w) = pick("SERVER", vars(&[("ORBIVISOR_SERVER", "https://a")]));
        assert_eq!(v, Some("https://a".into()));
        assert_eq!(w, None);
    }

    #[test]
    fn old_name_only_warns() {
        let (v, w) = pick("SERVER", vars(&[("OPL_SERVER", "https://old")]));
        assert_eq!(v, Some("https://old".into()));
        assert_eq!(
            w.as_deref(),
            Some("environment variable OPL_SERVER is deprecated; use ORBIVISOR_SERVER")
        );
    }

    #[test]
    fn both_names_new_wins_without_warning() {
        let (v, w) = pick(
            "SERVER",
            vars(&[
                ("ORBIVISOR_SERVER", "https://new"),
                ("OPL_SERVER", "https://old"),
            ]),
        );
        assert_eq!(v, Some("https://new".into()));
        assert_eq!(w, None);
    }

    #[test]
    fn neither_name() {
        assert_eq!(pick("SERVER", vars(&[])), (None, None));
    }

    #[test]
    fn empty_value_is_kept() {
        // 빈 값도 값이다(std::env::var처럼) — 새 이름이 비어 있어도 옛 이름을 보지 않는다.
        let (v, w) = pick(
            "TOKEN",
            vars(&[("ORBIVISOR_TOKEN", ""), ("OPL_TOKEN", "old")]),
        );
        assert_eq!(v, Some("".into()));
        assert_eq!(w, None);
        let (v, _) = pick("TOKEN", vars(&[("OPL_TOKEN", "")]));
        assert_eq!(v, Some("".into()));
    }

    #[test]
    fn promotions_cover_only_old_names_of_runtime_vars() {
        assert!(CLAP_ENV.iter().all(|n| RUNTIME_ENV.contains(n)));
        let got = promotions(
            CLAP_ENV,
            vars(&[
                ("OPL_SERVER", "https://old"),
                ("ORBIVISOR_CONFIG", "/new.json"),
                ("OPL_CONFIG", "/old.json"),
                ("OPL_TEST_LVM", "vg/tp"),
                // orbivisorctl이 clap으로 읽지 않는 것(쓰는 곳에서 읽음)
                ("OPL_QEMU_SHARE", "/q"),
                ("OPL_ESXI_PASSWORD", "x"),
            ]),
        );
        assert_eq!(
            got,
            vec![(
                "ORBIVISOR_SERVER".to_string(),
                OsString::from("https://old"),
                deprecated("SERVER"),
            )]
        );
    }

    #[test]
    fn env_reads_the_process_environment() {
        // 다른 시험과 겹치지 않는 이름(RUNTIME_ENV 밖). 이 시험만 이 이름을 쓴다.
        // SAFETY: 이 이름은 이 시험만 읽고 쓴다. 다른 스레드가 환경을 읽는 동안 바꾸는 것은 플랫폼에 따라
        // 안전하지 않지만, 시험 바이너리 안의 다른 시험은 이 크레이트에서 환경을 읽지 않는다.
        unsafe {
            std::env::set_var("OPL_LEGACY_SELFTEST_1F3A", "old");
        }
        assert_eq!(env("LEGACY_SELFTEST_1F3A").as_deref(), Some("old"));
        // 두 번째도 같은 값(경고는 한 번)
        assert_eq!(env("LEGACY_SELFTEST_1F3A").as_deref(), Some("old"));
        assert!(WARNED.lock().unwrap().contains("LEGACY_SELFTEST_1F3A"));
        unsafe {
            std::env::set_var("ORBIVISOR_LEGACY_SELFTEST_1F3A", "new");
        }
        assert_eq!(env("LEGACY_SELFTEST_1F3A").as_deref(), Some("new"));
        assert_eq!(env("LEGACY_SELFTEST_ABSENT_9C2D"), None);
    }
}
