//! 노드 CPU 탐침(계획 63 §6). QEMU를 VM과 같은 머신·가속기로 `-S`로 한 번 띄워 QMP로 CPU 모델과 플래그를 묻고
//! ([`probe_replies`]) 그 응답으로 카탈로그를 만든다([`build_catalog`]). 만들기는 순수 함수라 기록한 응답
//! (`tests/fixtures/cpu/`)으로 시험한다.
//!
//! - x86_64: `query-cpu-definitions`, 별칭이 아닌 모델마다 `query-cpu-model-expansion static`, `full max`, `full host`
//!   (KVM만 된다), 플래그 이름은 `-cpu help`의 "Recognized CPUID flags".
//! - aarch64: `query-cpu-definitions`와 모델마다 `full` 확장. 버전·`unavailable-features`·정적 확장이 없다. 플래그는
//!   `full max`의 불 속성(`sve`·`pauth`·`pmu` …)이다. `aarch64` 속성이 참이 아닌 모델(32비트·M 프로필)은 `virt`
//!   aarch64 게스트로 켤 수 없어 뺀다. KVM·HVF에서는 명명 모델을 켤 수 없다(`missing_other = ["accel"]`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opl_types::cpu::{Bits, CatalogModel, CpuCatalogSpec, CpuModel, normalize_flag};
use opl_types::vm::{Arch, MachineType};
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::io::AsyncReadExt;

use crate::host::Accel;
use crate::qmp::{QmpClient, QmpError};

/// 탐침 하나를 기다리는 시간(x86 정적 확장 188번을 포함해 보통 1초 안쪽)
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// 카탈로그에 넣지 않는 모델. `host`·`max`는 API가 의사 항목으로 보이고, `base`는 정적 확장의 기반이다.
const SKIPPED_MODELS: [&str; 3] = ["host", "max", "base"];

#[derive(Debug, Error)]
pub enum CpuProbeError {
    #[error("cannot run QEMU: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Qmp(#[from] QmpError),
    #[error("QEMU exited before the QMP socket was ready: {0}")]
    Exited(String),
    #[error("the CPU probe timed out")]
    Timeout,
    #[error("unexpected QEMU reply: {0}")]
    Invalid(String),
}

/// 탐침이 QEMU에서 받은 것. 카탈로그에 쓰지 않는 값(거짓인 불, `vendor` 밖의 값)은 받을 때 버린다 — 고정물이 작고
/// 실제 탐침과 같은 모양이다.
#[derive(Debug, Clone, PartialEq)]
pub struct CpuProbeReplies {
    /// `query-version`(`major.minor.micro`)
    pub version: String,
    /// `query-cpu-definitions`의 `return`(배열)
    pub definitions: Value,
    /// `query-cpu-model-expansion full max`의 `props`(거짓인 불도 둔다 — aarch64 플래그 이름이다)
    pub max: Map<String, Value>,
    /// `full host`의 `props`(KVM만 된다)
    pub host: Option<Map<String, Value>>,
    /// 모델마다의 확장(x86 `static`, aarch64 `full`) 가운데 참인 불과 `vendor`
    pub expansions: BTreeMap<String, Map<String, Value>>,
    /// `-cpu help`의 출력(x86만)
    pub help: String,
}

impl CpuProbeReplies {
    /// 고정물 JSON
    pub fn to_json(&self) -> Value {
        json!({
            "version": self.version,
            "definitions": self.definitions,
            "max": self.max,
            "host": self.host,
            "expansions": self.expansions,
            "help": self.help,
        })
    }

    pub fn from_json(v: &Value) -> Result<Self, String> {
        let object = |key: &str| -> Result<Map<String, Value>, String> {
            v.get(key)
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| format!("{key} is not an object"))
        };
        Ok(Self {
            version: v["version"]
                .as_str()
                .ok_or("version is not a string")?
                .to_string(),
            definitions: v["definitions"].clone(),
            max: object("max")?,
            host: v["host"].as_object().cloned(),
            expansions: object("expansions")?
                .into_iter()
                .map(|(k, v)| match v {
                    Value::Object(m) => Ok((k, m)),
                    _ => Err(format!("expansion {k} is not an object")),
                })
                .collect::<Result<_, _>>()?,
            help: v["help"].as_str().unwrap_or_default().to_string(),
        })
    }
}

/// 탐침 결과: 카탈로그와 호스트 CPU(`full host`, KVM만)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuProbe {
    pub catalog: CpuCatalogSpec,
    /// 호스트 CPU 벤더(`GenuineIntel` …)
    pub vendor: Option<String>,
    /// 호스트 CPU 이름(`model-id`)
    pub model_id: Option<String>,
}

/// `-cpu help`의 "Recognized CPUID flags:" 아래 들여 쓴 줄들의 이름
pub fn help_flags(help: &str) -> Vec<String> {
    let mut lines = help
        .lines()
        .skip_while(|l| !l.contains("Recognized CPUID flags"));
    if lines.next().is_none() {
        return Vec::new();
    }
    lines
        .take_while(|l| l.starts_with(char::is_whitespace) && !l.trim().is_empty())
        .flat_map(|l| l.split_whitespace())
        .map(String::from)
        .collect()
}

/// 확장 `props`에서 카탈로그가 쓰는 것만 남긴다: 참인 불과 `vendor`.
fn trim_expansion(props: &Map<String, Value>) -> Map<String, Value> {
    props
        .iter()
        .filter(|(k, v)| *v == &Value::Bool(true) || (k.as_str() == "vendor" && v.is_string()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn str_prop(props: Option<&Map<String, Value>>, key: &str) -> Option<String> {
    props
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// 탐침 응답으로 카탈로그를 만든다(순수). `flags`는 [`normalize_flag`]로 맞춰 정렬하고, `models`는 이름순이며,
/// 비트 집합은 정렬된 `flags`의 순번이므로 응답의 순서와 상관없이 같은 카탈로그(같은 해시)가 나온다. `accel`은
/// `kvm`·`hvf`·`tcg`다.
pub fn build_catalog(
    arch: Arch,
    accel: &str,
    replies: &CpuProbeReplies,
) -> Result<CpuProbe, String> {
    let names: Vec<String> = match arch {
        Arch::X86_64 => help_flags(&replies.help),
        Arch::Aarch64 => replies
            .max
            .iter()
            .filter(|(_, v)| v.is_boolean())
            .map(|(k, _)| k.clone())
            .collect(),
    };
    let flags: Vec<String> = names
        .iter()
        .filter_map(|n| normalize_flag(n).ok())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if flags.is_empty() {
        return Err(match arch {
            Arch::X86_64 => "`-cpu help` lists no CPU flags".into(),
            Arch::Aarch64 => "the `max` CPU model has no properties".into(),
        });
    }
    let index = |name: &str| {
        normalize_flag(name)
            .ok()
            .and_then(|n| flags.binary_search(&n).ok())
    };
    let trues = |props: &Map<String, Value>| {
        Bits::from_indices(
            props
                .iter()
                .filter(|(_, v)| **v == Value::Bool(true))
                .filter_map(|(k, _)| index(k)),
        )
    };
    let definitions = replies
        .definitions
        .as_array()
        .ok_or("query-cpu-definitions did not return a list")?;
    let mut models = Vec::new();
    for def in definitions {
        let name = def["name"]
            .as_str()
            .ok_or_else(|| format!("CPU definition without a name: {def}"))?;
        // `host`·`max`·`base`·`cluster`·수준 이름과 문법에 맞지 않는 이름은 VM이 쓸 수 없다.
        if SKIPPED_MODELS.contains(&name)
            || !matches!(name.parse::<CpuModel>(), Ok(CpuModel::Named(_)))
        {
            continue;
        }
        let expansion = replies.expansions.get(name);
        if arch == Arch::Aarch64 && expansion.and_then(|e| e.get("aarch64")) != Some(&json!(true)) {
            continue;
        }
        let alias_of = def["alias-of"].as_str().map(String::from);
        let mut missing = Vec::new();
        let mut missing_other = BTreeSet::new();
        for u in def["unavailable-features"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            match index(u) {
                Some(i) => missing.push(i),
                None => {
                    missing_other.insert(u.to_string());
                }
            }
        }
        if arch == Arch::Aarch64 && accel != Accel::Tcg.as_str() {
            missing_other.insert("accel".to_string());
        }
        let (vendor, features) = match arch {
            Arch::X86_64 => {
                let own = alias_of.as_deref().unwrap_or(name);
                (
                    str_prop(replies.expansions.get(own), "vendor"),
                    // 별칭의 특징은 가리키는 버전 모델의 것이다.
                    alias_of.is_none().then(|| expansion.map(trues)).flatten(),
                )
            }
            Arch::Aarch64 => (None, None),
        };
        models.push(CatalogModel {
            name: name.to_string(),
            alias_of,
            vendor,
            missing: Bits::from_indices(missing),
            missing_other: missing_other.into_iter().collect(),
            features,
            migration_safe: def["migration-safe"].as_bool().unwrap_or(false),
            deprecated: def["deprecated"].as_bool().unwrap_or(false),
        });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models.dedup_by(|a, b| a.name == b.name);
    let catalog = CpuCatalogSpec {
        arch,
        accel: accel.to_string(),
        qemu: replies.version.clone(),
        machine: arch.machine(),
        supported: trues(&replies.max),
        vendor: str_prop(Some(&replies.max), "vendor"),
        flags,
        models,
    };
    Ok(CpuProbe {
        catalog,
        vendor: str_prop(replies.host.as_ref(), "vendor"),
        model_id: str_prop(replies.host.as_ref(), "model-id"),
    })
}

/// QEMU를 한 번 띄워 CPU 모델·플래그를 묻고 카탈로그를 만든다. `dir`에 QMP 소켓을 잠깐 만든다.
pub async fn probe(
    binary: &Path,
    arch: Arch,
    accel: Accel,
    dir: &Path,
    timeout: Duration,
) -> Result<CpuProbe, CpuProbeError> {
    let replies = probe_replies(binary, arch, accel, dir, timeout).await?;
    build_catalog(arch, accel.as_str(), &replies).map_err(CpuProbeError::Invalid)
}

/// 탐침 소켓 이름이 겹치지 않게 하는 번호
static PROBES: AtomicU64 = AtomicU64::new(0);

/// QEMU를 `-S -nodefaults -display none`으로 띄워 CPU 질의의 응답을 모은다(x86은 `-cpu help`도 부른다).
pub async fn probe_replies(
    binary: &Path,
    arch: Arch,
    accel: Accel,
    dir: &Path,
    timeout: Duration,
) -> Result<CpuProbeReplies, CpuProbeError> {
    std::fs::create_dir_all(dir)?;
    let socket = dir.join(format!(
        "cpu-probe-{}-{}.qmp",
        std::process::id(),
        PROBES.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&socket);
    let result = tokio::time::timeout(timeout, ask(binary, arch, accel, &socket)).await;
    let _ = std::fs::remove_file(&socket);
    result.map_err(|_| CpuProbeError::Timeout)?
}

async fn ask(
    binary: &Path,
    arch: Arch,
    accel: Accel,
    socket: &Path,
) -> Result<CpuProbeReplies, CpuProbeError> {
    let help = match arch {
        // QEMU 버전에 따라 끝난 상태가 다를 수 있어 보지 않는다(`-tpmdev help`처럼).
        Arch::X86_64 => {
            let out = tokio::process::Command::new(binary)
                .args(["-cpu", "help"])
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await?;
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        Arch::Aarch64 => String::new(),
    };
    let machine = match arch.machine() {
        MachineType::Q35 => "q35",
        MachineType::Virt => "virt",
    };
    let mut child = tokio::process::Command::new(binary)
        .args(["-S", "-nodefaults", "-display", "none", "-machine"])
        .arg(format!("{machine},accel={}", accel.as_str()))
        .arg("-qmp")
        .arg(format!("unix:{},server=on,wait=off", socket.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    while !socket.exists() {
        if child.try_wait()?.is_some() {
            return Err(exited(&mut child).await);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // 가속기를 켜지 못한 QEMU는 소켓을 만든 뒤 끝날 수 있다.
    let qmp = match QmpClient::connect(socket).await {
        Ok(qmp) => qmp,
        Err(e) => {
            return match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(_) => Err(exited(&mut child).await),
                Err(_) => Err(e.into()),
            };
        }
    };
    let replies = query(&qmp, arch, help).await;
    let _ = qmp.quit().await;
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    replies
}

/// 끝난 QEMU의 표준 오류
async fn exited(child: &mut tokio::process::Child) -> CpuProbeError {
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut stderr).await;
    }
    CpuProbeError::Exited(stderr.trim().to_string())
}

async fn query(
    qmp: &QmpClient,
    arch: Arch,
    help: String,
) -> Result<CpuProbeReplies, CpuProbeError> {
    let version = qmp.query_version().await?;
    let definitions = qmp.execute("query-cpu-definitions", None).await?;
    let list = definitions
        .as_array()
        .ok_or_else(|| CpuProbeError::Invalid("query-cpu-definitions is not a list".into()))?;
    let max = expand(qmp, "full", "max")
        .await?
        .ok_or_else(|| CpuProbeError::Invalid("cannot expand the max CPU model".into()))?;
    // `host`는 KVM에서만 된다.
    let host = expand(qmp, "full", "host").await?;
    let kind = match arch {
        Arch::X86_64 => "static",
        Arch::Aarch64 => "full",
    };
    let mut expansions = BTreeMap::new();
    for def in list {
        let Some(name) = def["name"].as_str() else {
            continue;
        };
        if SKIPPED_MODELS.contains(&name) || def.get("alias-of").is_some() {
            continue;
        }
        if let Some(props) = expand(qmp, kind, name).await? {
            expansions.insert(name.to_string(), trim_expansion(&props));
        }
    }
    Ok(CpuProbeReplies {
        version,
        definitions,
        max,
        host: host.map(|h| trim_host(&h)),
        expansions,
        help,
    })
}

/// `full host`에서 쓰는 것(`vendor`·`model-id`)
fn trim_host(props: &Map<String, Value>) -> Map<String, Value> {
    props
        .iter()
        .filter(|(k, v)| matches!(k.as_str(), "vendor" | "model-id") && v.is_string())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `query-cpu-model-expansion`의 `props`. QEMU가 그 모델·종류를 펼치지 못하면(명령 오류) None이다.
async fn expand(
    qmp: &QmpClient,
    kind: &str,
    model: &str,
) -> Result<Option<Map<String, Value>>, CpuProbeError> {
    let args = json!({ "type": kind, "model": { "name": model } });
    match qmp.execute("query-cpu-model-expansion", Some(args)).await {
        Ok(ret) => Ok(Some(
            ret["model"]["props"]
                .as_object()
                .cloned()
                .unwrap_or_default(),
        )),
        Err(QmpError::Command { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> CpuProbeReplies {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cpu")
            .join(name);
        let text = std::fs::read_to_string(&path).unwrap();
        CpuProbeReplies::from_json(&serde_json::from_str(&text).unwrap()).unwrap()
    }

    fn names<'a>(c: &'a CpuCatalogSpec, bits: &'a Bits) -> Vec<&'a str> {
        c.names(bits).collect()
    }

    /// 응답의 순서를 뒤집는다(정의 목록, `-cpu help`의 플래그 줄, props 순서).
    fn shuffled(r: &CpuProbeReplies) -> CpuProbeReplies {
        let mut r = r.clone();
        if let Some(list) = r.definitions.as_array_mut() {
            list.reverse();
        }
        let rev = |m: &Map<String, Value>| -> Map<String, Value> {
            let mut items: Vec<_> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            items.reverse();
            items.into_iter().collect()
        };
        r.max = rev(&r.max);
        r.expansions = r
            .expansions
            .iter()
            .map(|(k, v)| (k.clone(), rev(v)))
            .collect();
        let flags = help_flags(&r.help);
        let mut reversed: Vec<_> = flags.iter().rev().cloned().collect();
        // 같은 이름을 `_`로 한 번 더(QEMU는 둘을 같게 받는다): 맞춘 뒤 하나다.
        reversed.push("lahf_lm".into());
        r.help = format!(
            "Available CPUs:\n  max\n\nRecognized CPUID flags:\n  {}\n\n",
            reversed.join(" ")
        );
        r
    }

    #[test]
    fn help_flags_read_the_indented_block() {
        let help = "Available CPUs:\n  max   Enables all features\n\nRecognized CPUID flags:\n  3dnow abm\n  sse4.1 lahf-lm\n\nmore text\n";
        assert_eq!(help_flags(help), ["3dnow", "abm", "sse4.1", "lahf-lm"]);
        assert!(help_flags("Available CPUs:\n  a53\n").is_empty());
    }

    /// 이 Mac의 Homebrew QEMU 11.1.2, `-machine q35,accel=tcg`
    #[test]
    fn x86_tcg_catalog() {
        let r = fixture("x86_64-tcg.json");
        let p = build_catalog(Arch::X86_64, "tcg", &r).unwrap();
        let c = &p.catalog;
        assert_eq!(
            (c.arch, c.accel.as_str(), c.qemu.as_str(), c.machine),
            (Arch::X86_64, "tcg", "11.1.2", MachineType::Q35)
        );
        // TCG에는 `full host`가 없다.
        assert_eq!((p.vendor, p.model_id), (None, None));
        assert_eq!(c.vendor.as_deref(), Some("AuthenticAMD"));
        // 정렬되고 맞춘 이름이다.
        assert!(c.flags.windows(2).all(|w| w[0] < w[1]));
        assert!(
            c.flags
                .iter()
                .all(|f| normalize_flag(f).as_deref() == Ok(f))
        );
        for f in ["avx512vbmi", "avx512-vpopcntdq", "sse4.1", "lahf-lm", "pni"] {
            assert!(c.flag_index(f).is_some(), "{f}");
        }
        assert!(c.flags.len() > 400, "{}", c.flags.len());
        // TCG `max`: AVX-512는 없고 SSE4.1·AES는 있다.
        let supported = names(c, &c.supported);
        assert!(supported.contains(&"sse4.1") && supported.contains(&"aes"));
        assert!(!supported.contains(&"avx512vbmi"));
        // host·max·base는 모델이 아니다.
        for m in ["host", "max", "base"] {
            assert!(c.model(m).is_none(), "{m}");
        }
        assert!(c.models.windows(2).all(|w| w[0].name < w[1].name));
        assert!(c.models.len() > 150, "{}", c.models.len());

        let v7 = c.model("Icelake-Server-v7").unwrap();
        assert_eq!(v7.alias_of, None);
        assert_eq!(v7.vendor.as_deref(), Some("GenuineIntel"));
        assert!(v7.migration_safe && !v7.deprecated);
        let missing = names(c, &v7.missing);
        assert!(missing.contains(&"avx512vbmi") && missing.contains(&"gfni"));
        assert!(v7.missing_other.is_empty());
        let features = names(c, v7.features.as_ref().unwrap());
        assert!(features.contains(&"avx512vbmi") && features.contains(&"sse4.2"));

        let alias = c.model("Icelake-Server").unwrap();
        assert_eq!(alias.alias_of.as_deref(), Some("Icelake-Server-v1"));
        assert_eq!(alias.vendor.as_deref(), Some("GenuineIntel"));
        assert_eq!(alias.features, None);
        assert_eq!(
            c.model("Icelake-Server-noTSX").unwrap().alias_of.as_deref(),
            Some("Icelake-Server-v2")
        );
        // 수준의 기반 모델이 있고 TCG에서 켤 수 있다.
        let base = c.model("qemu64-v1").unwrap();
        assert!(base.missing.is_empty() && base.missing_other.is_empty());
    }

    /// 탐침 결과가 opl-types의 `can_run`과 맞는다: 빠진 것을 끄면 켤 수 있다.
    #[test]
    fn x86_catalog_feeds_can_run() {
        use opl_types::cpu::{CpuSource, ResolvedCpu, can_run};
        let c = build_catalog(Arch::X86_64, "tcg", &fixture("x86_64-tcg.json"))
            .unwrap()
            .catalog;
        let mut cpu = ResolvedCpu {
            model: "Icelake-Server-v7".into(),
            flags: BTreeMap::new(),
            enforce: true,
            source: CpuSource::Spec,
            dropped_mask: Vec::new(),
        };
        let err = can_run(&cpu, &c).unwrap_err();
        assert!(err.missing.contains(&"avx512vbmi".to_string()), "{err}");
        let m = c.model("Icelake-Server-v7").unwrap();
        cpu.flags = c
            .names(&m.missing)
            .map(|f| (f.to_string(), false))
            .collect();
        can_run(&cpu, &c).unwrap();
        cpu.flags.insert("avx512vbmi".into(), true);
        assert!(can_run(&cpu, &c).is_err());
    }

    /// 해시는 응답의 순서에 매이지 않는다(정렬·정규화). 바뀌면 같은 노드가 새 카탈로그를 올리므로 고정해 둔다.
    #[test]
    fn catalog_hash_is_deterministic() {
        let r = fixture("x86_64-tcg.json");
        let a = build_catalog(Arch::X86_64, "tcg", &r).unwrap().catalog;
        let b = build_catalog(Arch::X86_64, "tcg", &shuffled(&r))
            .unwrap()
            .catalog;
        assert_eq!(a, b);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 64);
        // JSON으로 왕복해도 같다(노드가 Raft에서 읽은 카탈로그의 해시가 이름과 같다).
        let back: CpuCatalogSpec =
            serde_json::from_slice(&serde_json::to_vec(&a).unwrap()).unwrap();
        assert_eq!(back.hash(), a.hash());
        // 가속기가 다르면 다른 카탈로그다.
        let kvm = build_catalog(Arch::X86_64, "kvm", &r).unwrap().catalog;
        assert_ne!(kvm.hash(), a.hash());
        insta::assert_snapshot!(a.hash(), @"c82d2c57a6dce732e9793f8fa3f1d8549db568d892e97bc4e77614c1296e67de");
    }

    /// 크기(설계 §7, 보고). 실제 탐침은 `tests/cpu_probe.rs`가 같은 것을 찍는다.
    #[test]
    fn catalog_sizes() {
        for (file, arch, accel, max) in [
            ("x86_64-tcg.json", Arch::X86_64, "tcg", 64 * 1024),
            ("aarch64-tcg.json", Arch::Aarch64, "tcg", 8 * 1024),
            ("aarch64-hvf.json", Arch::Aarch64, "hvf", 8 * 1024),
        ] {
            let c = build_catalog(arch, accel, &fixture(file)).unwrap().catalog;
            let size = serde_json::to_vec(&c).unwrap().len();
            println!(
                "{file}: {} models, {} flags, {size} bytes",
                c.models.len(),
                c.flags.len()
            );
            assert!(size < max, "{file}: {size}");
        }
    }

    /// 이 Mac의 Homebrew QEMU 11.1.2, `-machine virt,accel=tcg`
    #[test]
    fn aarch64_tcg_catalog() {
        let p = build_catalog(Arch::Aarch64, "tcg", &fixture("aarch64-tcg.json")).unwrap();
        let c = &p.catalog;
        assert_eq!((c.machine, c.vendor.as_deref()), (MachineType::Virt, None));
        assert_eq!((p.vendor, p.model_id), (None, None));
        // 플래그는 `full max`의 불 속성이다(거짓인 `pauth-impdef`도 이름이다).
        for f in ["sve", "sve128", "pauth", "pauth-impdef", "pmu", "aarch64"] {
            assert!(c.flag_index(f).is_some(), "{f}");
        }
        let supported = names(c, &c.supported);
        assert!(supported.contains(&"sve") && !supported.contains(&"pauth-impdef"));
        // 64비트 모델만, 정적 확장·벤더 없음, TCG라 켤 수 있다.
        let n1 = c.model("neoverse-n1").unwrap();
        assert_eq!((n1.features.as_ref(), n1.vendor.as_ref()), (None, None));
        assert!(n1.missing.is_empty() && n1.missing_other.is_empty());
        assert!(c.model("cortex-a57").is_some());
        for m in ["cortex-m3", "cortex-a15", "arm926", "host", "max"] {
            assert!(c.model(m).is_none(), "{m}");
        }
    }

    /// 이 Mac의 HVF: `max`는 `pauth`·`pmu`·`aarch64`뿐이고 명명 모델은 켤 수 없다.
    #[test]
    fn aarch64_hvf_catalog() {
        use opl_types::cpu::{CpuSource, ResolvedCpu, can_run};
        let c = build_catalog(Arch::Aarch64, "hvf", &fixture("aarch64-hvf.json"))
            .unwrap()
            .catalog;
        assert_eq!(names(&c, &c.supported), ["aarch64", "pauth", "pmu"]);
        assert!(!c.models.is_empty());
        assert!(c.models.iter().all(|m| m.missing_other == ["accel"]));
        let cpu = ResolvedCpu {
            model: "neoverse-n1".into(),
            flags: BTreeMap::new(),
            enforce: true,
            source: CpuSource::Spec,
            dropped_mask: Vec::new(),
        };
        assert_eq!(can_run(&cpu, &c).unwrap_err().other, ["accel"]);
        // 같은 QEMU라도 TCG와 다른 카탈로그다.
        let tcg = build_catalog(Arch::Aarch64, "tcg", &fixture("aarch64-tcg.json"))
            .unwrap()
            .catalog;
        assert_ne!(tcg.hash(), c.hash());
    }

    /// KVM은 `full host`로 호스트 벤더·이름을 알린다(합성: 기록한 TCG 응답에 `host`를 더함).
    #[test]
    fn kvm_host_vendor_and_name() {
        let mut r = fixture("x86_64-tcg.json");
        r.host = Some(trim_host(
            json!({ "vendor": "GenuineIntel", "model-id": "Intel(R) Xeon(R) Gold 6338", "family": 6, "vmx": true })
                .as_object()
                .unwrap(),
        ));
        let p = build_catalog(Arch::X86_64, "kvm", &r).unwrap();
        assert_eq!(p.vendor.as_deref(), Some("GenuineIntel"));
        assert_eq!(p.model_id.as_deref(), Some("Intel(R) Xeon(R) Gold 6338"));
        assert_eq!(r.host.unwrap().len(), 2);
    }

    #[test]
    fn unknown_unavailable_features_are_not_flags() {
        let mut r = fixture("x86_64-tcg.json");
        let list = r.definitions.as_array_mut().unwrap();
        let def = list
            .iter_mut()
            .find(|d| d["name"] == "Skylake-Server-v5")
            .unwrap();
        def["unavailable-features"] = json!(["type", "avx512f", "avx512_vpopcntdq"]);
        let c = build_catalog(Arch::X86_64, "kvm", &r).unwrap().catalog;
        let m = c.model("Skylake-Server-v5").unwrap();
        assert_eq!(names(&c, &m.missing), ["avx512-vpopcntdq", "avx512f"]);
        assert_eq!(m.missing_other, ["type"]);
    }

    #[test]
    fn empty_flag_lists_are_errors() {
        let mut r = fixture("x86_64-tcg.json");
        r.help.clear();
        assert!(build_catalog(Arch::X86_64, "tcg", &r).is_err());
        let mut r = fixture("aarch64-tcg.json");
        r.max.clear();
        assert!(build_catalog(Arch::Aarch64, "tcg", &r).is_err());
    }

    #[test]
    fn fixture_json_round_trips() {
        let r = fixture("aarch64-hvf.json");
        assert_eq!(CpuProbeReplies::from_json(&r.to_json()).unwrap(), r);
    }
}
