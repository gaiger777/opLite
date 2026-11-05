# opLite 계획 33: 구독과 텔레메트리 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ed25519로 서명한 오프라인 구독 키로 구독 상태(노드 수·만료)를 보이고, 구독이 있으면 `enterprise` 업데이트 채널(APT 인증)을 쓰게 한다. 기능은 막지 않는다. 키 발급 도구를 둔다. 옵트인 텔레메트리(기본 꺼짐, 보낼 내용 미리 보기, 리더가 하루 한 번)를 더한다. API·oplctl·웹. 하위 프로젝트 9(설치·업데이트·구독)의 마지막 계획이다.

**Architecture:**
- **구독 키:** `opl1.<payload>.<signature>`. 검증은 내장 공개 키 목록(빌드 값 `OPL_LICENSE_KEYS`, 없으면 저장소의 개발 키)과 `ring`의 Ed25519. 서버 없이 오프라인으로 검증한다. 키는 계획 32의 `settings` 리소스(`spec.subscription`)에 두고, API로는 키 대신 상태만 내보낸다.
- **옛 노드 호환:** 계획 32 노드는 `settings`의 모르는 필드를 풀지 못한다. 새 필드는 기본값이면 직렬화하지 않고, 새 필드를 쓰는 요청은 모든 노드가 이 빌드 이상일 때만 받는다(409 `cluster_upgrading`).
- **enterprise 채널:** 계획 32의 소스 파일 관리에 APT 인증 파일(`/etc/apt/auth.conf.d/oplite.conf`)을 더한다.
- **텔레메트리:** 리더의 `TelemetryReporter`가 수만 담은 JSON을 HTTPS로 보낸다. 마지막 전송 시각은 Raft의 `settings.status`에 있어 리더가 바뀌어도 다시 보내지 않는다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, openraft 클러스터 상태, tokio, clap, `ring`(Ed25519)·`base64`·`reqwest`(이미 `Cargo.lock`에 있는 판). 웹은 React·TypeScript·react-query·vitest·msw. 셸은 POSIX sh와 bash(시험). CI는 GitHub Actions(ubuntu-latest, Docker `debian:13`, KVM). 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md` (§4와 §4.6 "구현(계획 33)", §5)
**선행 계획:** 계획 1~32 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 구독 키 형식·서명 검증·상태, 개발 키, 발급 도구 `opl-license` | 온라인 활성화 서버, 클러스터 묶기(하드웨어 지문) |
| 구독 API, `enterprise` 채널과 APT 인증 파일 | enterprise 저장소 쪽 인증 확인, 저장소 호스팅 |
| 텔레메트리 설정·미리 보기·리더 리포터, opld `[telemetry]` | 텔레메트리 수집 서버 |
| oplctl `subscription`·`telemetry`, 웹 설정·대시보드 배너 | 기능 막기(구독이 없어도 모든 기능이 돈다), 감사 로그(코드베이스에 없음) |

## 실행 환경

- **이 Mac에서 하는 것:** Rust·웹 변경과 시험(opld 프로세스 E2E 포함), 셸 시험(`packaging/tests/*-test.sh` 가운데 `deb-test.sh`·`repo-test.sh` 밖의 것). 텔레메트리 시험은 127.0.0.1의 시험 HTTPS 서버로 한다(밖으로 나가는 연결 없음).
- **이 Mac에서 못 하는 것:** Docker 컨테이너 시험(`deb-test.sh`·`repo-test.sh`)과 ISO E2E는 CI(`ci.yml`의 `package`, `iso.yml`)가 한다.
- **shellcheck:** 없으면 CI `shell` 작업이 본다.

## 설계 결정

- **`require_current` 게이트:** 구독 넣기·지우기, enterprise 채널, 텔레메트리 바꾸기·id 다시 만들기, 리포터의 상태 쓰기는 위트니스를 포함한 모든 노드의 버전이 이 빌드 이상일 때만 한다. 계획 32의 `require_upgraded`(버전이 있기만 하면 됨)는 그대로 둔다 — 업그레이드 경로는 낮은 노드가 있어도 돌아야 한다.
- **키는 밖으로 나가지 않는다:** `GET`·`PATCH /settings`, `reset-id` 응답, watch 이벤트에서 `spec.subscription`을 지운다. 로그에는 구독 id만.
- **정식 빌드 강제:** 개발 키의 비밀 키는 시험 견본으로 저장소에 있다. 그래서 `build-deb.sh`는 정식 공개 키(정규 base64, 개발 키가 아님, opld 바이너리에 실제로 들어 있음) 또는 `OPL_DEV_LICENSE=1`이 있어야 패키지를 만든다.
- **텔레메트리 끝점:** https만, 사용자 정보 금지, 리다이렉트를 따르지 않는다. 사설 수집 서버의 CA는 opld `[telemetry] ca_file`(설계와 다른 점).
- **웹:** 텔레메트리를 켜기 전에 보낼 JSON을 대화상자로 보인다. 대시보드 배너는 닫으면 그 안내를 7일 숨긴다(localStorage).

## 파일 구조

```
crates/opl-types/src/{subscription.rs, settings.rs, version.rs, lib.rs}, crates/opl-types/{keys/dev-license.pub, tests/fixtures/dev-license.key}
crates/opl-api/src/{subscription.rs, telemetry.rs, settings.rs, cluster.rs, updates.rs, watch.rs, lib.rs, bin/opl-license.rs}
crates/opl-api/tests/{subscription.rs, telemetry.rs, telemetry_reporter.rs, opl_license.rs, common/collector.rs, …}
crates/opl-peer/src/pki.rs, crates/opld/src/{config.rs, member.rs}, crates/opld/tests/subscription.rs, crates/oplctl/src/cli.rs
packaging/deb/build-deb.sh, packaging/tests/build-deb-test.sh, .github/actions/build-deb/action.yml
web/src/settings/{SubscriptionSection.tsx, TelemetrySection.tsx, SubscriptionBanner.tsx, save.ts, …}, web/src/{api/live.ts, pages/DashboardPage.tsx, …}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. API가 바뀌는 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다. `Cargo.lock`도 싣지 않는다. Task 1의 "생성 파일" 단계에서 `cargo check`가 의존 목록 줄을 고친다. 셸 스크립트의 들여쓰기 탭은 그대로 옮긴다.

---

### Task 1: 구독 키와 발급 도구

`opl_types::subscription`: 구독 키 `opl1.<payload>.<signature>`(payload는 JSON의 base64url, 서명은 `opl1.<payload>` 바이트에 대한 Ed25519)를 풀고 검증한다(`verify`는 형식·서명만, `parse`는 만료까지). 공개 키 목록은 빌드 값 `OPL_LICENSE_KEYS`, 없으면 저장소의 개발 키(상태에 development 표시). 상태 계산 `status`(none·active·expired·invalid·over_limit). 개발 키 쌍(공개 키와 시험 견본 비밀 키), 발급 도구 `opl-license keygen|pubkey|issue`(패키지에 넣지 않음). `build-deb.sh`는 정식 공개 키(`OPL_LICENSE_KEYS` — 정규 base64, 개발 키가 아님, opld 바이너리에 들어 있음) 또는 `OPL_DEV_LICENSE=1`이 있어야 패키지를 만들고, CI는 `OPL_DEV_LICENSE=1`로 만든다. 새 직접 의존 `ring`·`base64`(이미 `Cargo.lock`에 있는 판).

**Files:**
- Create: `crates/opl-api/src/bin/opl-license.rs`
- Create: `crates/opl-api/tests/opl_license.rs`
- Create: `crates/opl-types/keys/dev-license.pub`
- Create: `crates/opl-types/src/subscription.rs`
- Create: `crates/opl-types/tests/fixtures/dev-license.key`
- Modify: `.github/actions/build-deb/action.yml`
- Modify: `Cargo.toml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-types/Cargo.toml`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `packaging/deb/build-deb.sh`
- Modify: `packaging/tests/build-deb-test.sh`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/actions/build-deb/action.yml`에서

```yaml
      run: cargo build --release -p opld --features web-ui -p oplctl
    - id: deb
      shell: bash
      run: |
```

을 다음으로 바꾼다:

```yaml
      run: cargo build --release -p opld --features web-ui -p oplctl
    # CI 패키지는 저장소의 개발 구독 키를 믿는 개발 패키지다(정식 빌드는 OPL_LICENSE_KEYS를 준다).
    - id: deb
      shell: bash
      env:
        OPL_DEV_LICENSE: "1"
      run: |
```

`Cargo.toml`에서

```toml
native-tls = "0.2.18"
rust-embed = { version = "8.13.0", features = ["mime-guess", "interpolate-folder-path"] }
```

을 다음으로 바꾼다:

```toml
native-tls = "0.2.18"
ring = "0.17.14"
base64 = "0.22.1"
rust-embed = { version = "8.13.0", features = ["mime-guess", "interpolate-folder-path"] }
```

`crates/opl-api/Cargo.toml`에서

```toml
axum.workspace = true
fs4.workspace = true
```

을 다음으로 바꾼다:

```toml
axum.workspace = true
clap.workspace = true
fs4.workspace = true
```

`crates/opl-api/src/bin/opl-license.rs`를 만든다:

```rust
//! 구독 키 발급 도구. 패키지에는 넣지 않는다.
//! `opl-license keygen --out <파일>`: 발급용 키 쌍을 만들어 비밀 키를 파일에, 공개 키를 표준 출력에.
//! `opl-license issue --key <파일> --id … --customer … --level … --nodes … --days …`: 키 문자열을 표준 출력에.
//! `opl-license pubkey --key <파일>`: 비밀 키 파일의 공개 키를 표준 출력에.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use opl_types::subscription::{self, KeyError, Level, LicensePayload};

#[derive(Parser)]
#[command(name = "opl-license", version = opl_types::version::BUILD, about = "Issue opLite subscription keys")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 발급용 Ed25519 키 쌍을 만든다. 비밀 키(PKCS#8 base64)는 --out 파일(0600)에, 공개 키
    /// (OPL_LICENSE_KEYS의 항목)는 표준 출력에.
    Keygen {
        /// 비밀 키 파일. 이미 있으면 거절한다.
        #[arg(long)]
        out: PathBuf,
    },
    /// 구독 키를 발급해 표준 출력에 한 줄로 낸다.
    Issue {
        /// keygen이 만든 비밀 키 파일
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        customer: String,
        #[arg(long, value_enum)]
        level: LevelArg,
        /// 위트니스가 아닌 노드 수 한도
        #[arg(long)]
        nodes: u32,
        /// 유효 기간(일)
        #[arg(long)]
        days: u64,
        /// 발급 시각(유닉스 초). 기본은 지금.
        #[arg(long)]
        issued_at: Option<u64>,
    },
    /// 비밀 키 파일의 공개 키(OPL_LICENSE_KEYS의 항목)를 표준 출력에 낸다.
    Pubkey {
        /// keygen이 만든 비밀 키 파일
        #[arg(long)]
        key: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum LevelArg {
    Standard,
    Premium,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("opl-license: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.cmd {
        Cmd::Keygen { out } => {
            let key = subscription::generate_key().map_err(|e| e.to_string())?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&out)
                .map_err(|e| format!("create {}: {e}", out.display()))?;
            if let Err(e) = writeln!(file, "{}", key.private_key).and_then(|()| file.sync_all()) {
                // 반쯤 쓴 파일을 남기지 않는다(다음 keygen이 거절하지 않게).
                let _ = std::fs::remove_file(&out);
                return Err(format!("write {}: {e}", out.display()));
            }
            println!("{}", key.public_key);
        }
        Cmd::Issue {
            key,
            id,
            customer,
            level,
            nodes,
            days,
            issued_at,
        } => {
            if nodes < 1 {
                return Err("--nodes must be at least 1".into());
            }
            if days < 1 {
                return Err("--days must be at least 1".into());
            }
            let pkcs8 = private_key(&key)?;
            let issued_at = issued_at.unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            });
            let payload = LicensePayload {
                id,
                customer,
                level: match level {
                    LevelArg::Standard => Level::Standard,
                    LevelArg::Premium => Level::Premium,
                },
                nodes,
                issued_at,
                expires_at: issued_at.saturating_add(days.saturating_mul(86_400)),
            };
            // 키는 이미 풀렸으므로 남는 실패는 발급할 내용의 검사다.
            let key = subscription::sign(&pkcs8, &payload).map_err(|e| match e {
                KeyError::Format(m) => format!("invalid license: {m}"),
                e => e.to_string(),
            })?;
            println!("{key}");
        }
        Cmd::Pubkey { key } => {
            let pkcs8 = private_key(&key)?;
            println!(
                "{}",
                subscription::public_key(&pkcs8).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(())
}

/// 비밀 키 파일을 읽어 Ed25519 키인지까지 확인한 PKCS#8 바이트
fn private_key(path: &Path) -> Result<Vec<u8>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bad = |e: KeyError| match e {
        KeyError::Format(m) => format!("{}: {m}", path.display()),
        e => e.to_string(),
    };
    let pkcs8 = subscription::decode_private_key(&text).map_err(bad)?;
    subscription::public_key(&pkcs8).map_err(bad)?;
    Ok(pkcs8)
}
```

`crates/opl-api/tests/opl_license.rs`를 만든다:

```rust
//! 구독 키 발급 도구 opl-license: keygen → issue → 키 풀기 왕복, 비밀 키 파일 권한, 잘못된 인자.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

use opl_types::subscription::{self, KeyError, Level, PublicKey};

fn opl_license(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_opl-license"))
        .args(args)
        .output()
        .expect("run opl-license")
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn keygen_then_issue_makes_a_key_this_public_key_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("issuer.key");
    let key_path = key_file.to_str().unwrap();

    let public = stdout(&opl_license(&["keygen", "--out", key_path]));
    let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // pubkey는 비밀 키 파일에서 같은 공개 키를 낸다.
    assert_eq!(stdout(&opl_license(&["pubkey", "--key", key_path])), public);
    let public = PublicKey::from_base64(public.trim(), false).expect("public key on stdout");

    let issued_at = 1_800_000_000;
    let out = stdout(&opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "lic-42",
        "--customer",
        "Example Corp",
        "--level",
        "standard",
        "--nodes",
        "4",
        "--days",
        "365",
        "--issued-at",
        &issued_at.to_string(),
    ]));
    assert_eq!(out.lines().count(), 1, "{out}");
    let license = subscription::parse(&out, std::slice::from_ref(&public), issued_at).unwrap();
    assert_eq!(license.id, "lic-42");
    assert_eq!(license.customer, "Example Corp");
    assert_eq!(license.level, Level::Standard);
    assert_eq!(license.nodes, 4);
    assert_eq!(license.issued_at, issued_at);
    assert_eq!(license.expires_at, issued_at + 365 * 86_400);
    assert!(!license.development);
    // 저장소의 개발 키로는 검증되지 않는다.
    assert_eq!(
        subscription::verify(&out, &subscription::license_keys()),
        Err(KeyError::Signature)
    );

    // 이미 있는 파일은 덮어쓰지 않는다.
    let before = std::fs::read(&key_file).unwrap();
    let again = opl_license(&["keygen", "--out", key_path]);
    assert!(!again.status.success());
    assert_eq!(std::fs::read(&key_file).unwrap(), before);
}

#[test]
fn issue_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("issuer.key");
    let key_path = key_file.to_str().unwrap();
    stdout(&opl_license(&["keygen", "--out", key_path]));

    let issue = |key: &str, nodes: &str, days: &str| {
        opl_license(&[
            "issue",
            "--key",
            key,
            "--id",
            "lic-1",
            "--customer",
            "c",
            "--level",
            "premium",
            "--nodes",
            nodes,
            "--days",
            days,
        ])
    };
    // 노드 0, 기간 0은 키를 만들기 전에 거절한다.
    for (out, want) in [
        (
            issue(key_path, "0", "30"),
            "opl-license: --nodes must be at least 1",
        ),
        (
            issue(key_path, "3", "0"),
            "opl-license: --days must be at least 1",
        ),
    ] {
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(err.trim(), want);
    }
    // 내용이 틀리면 "키를 받았다"가 아니라 발급할 내용이 틀렸다고 한다.
    let out = opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "Acme 2026",
        "--customer",
        "c",
        "--level",
        "premium",
        "--nodes",
        "1",
        "--days",
        "1",
    ]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.starts_with("opl-license: invalid license: id must be"),
        "{err}"
    );
    // 비밀 키 파일이 없거나 키가 아님
    let missing = dir.path().join("missing.key");
    assert!(!issue(missing.to_str().unwrap(), "3", "30").status.success());
    let junk = dir.path().join("junk.key");
    std::fs::write(&junk, "AAAA\n").unwrap();
    assert!(!issue(junk.to_str().unwrap(), "3", "30").status.success());
    let out = opl_license(&["pubkey", "--key", junk.to_str().unwrap()]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a PKCS#8 Ed25519 private key"), "{err}");
    // 모르는 수준
    let out = opl_license(&[
        "issue",
        "--key",
        key_path,
        "--id",
        "x",
        "--customer",
        "c",
        "--level",
        "gold",
        "--nodes",
        "1",
        "--days",
        "1",
    ]);
    assert!(!out.status.success());
}
```

`crates/opl-types/Cargo.toml`에서

```toml
[dependencies]
serde.workspace = true
thiserror.workspace = true
```

을 다음으로 바꾼다:

```toml
[dependencies]
base64.workspace = true
ring.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
```

`crates/opl-types/Cargo.toml`에서

```toml
uuid.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

을 다음으로 바꾼다:

```toml
uuid.workspace = true
```

`crates/opl-types/keys/dev-license.pub`를 만든다:

```
agJBkv/4s2B9yquPvRhRIfR/c5MpSEvc5h65eeYLsJo=
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod storage;
pub mod task;
```

을 다음으로 바꾼다:

```rust
pub mod storage;
pub mod subscription;
pub mod task;
```

`crates/opl-types/src/subscription.rs`를 만든다:

```rust
//! 구독 키. 오프라인으로 검증하는 서명된 문자열 `opl1.<payload>.<signature>`이다: payload는 JSON의
//! base64url(패딩 없음), signature는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url.
//! 구독은 기능을 막지 않는다. 상태([`status`])는 읽을 때 지금 시각과 노드 수로 계산한다.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 키 문자열의 접두사(형식 판)
const PREFIX: &str = "opl1";

/// 만료 시각의 상한(9999-12-31T23:59:59Z)
pub const MAX_EXPIRES_AT: u64 = 253_402_300_799;

/// 저장소의 개발 공개 키. 비밀 키는 시험 견본(`tests/fixtures/dev-license.key`)으로 공개되어 있다.
const DEV_KEY: &str = include_str!("../keys/dev-license.pub");

/// 구독 수준
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Standard,
    Premium,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Standard => "standard",
            Level::Premium => "premium",
        }
    }
}

/// 키에 서명해 담는 내용. 모르는 필드는 무시한다(나중 판이 더할 수 있게).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicensePayload {
    /// 키 id(1~64자, 영문자·숫자·`.`·`_`·`-`, 영문자나 숫자로 시작)
    pub id: String,
    /// 고객 이름(1~200자)
    pub customer: String,
    pub level: Level,
    /// 위트니스가 아닌 노드 수 한도(1 이상)
    pub nodes: u32,
    /// 발급 시각(유닉스 초)
    pub issued_at: u64,
    /// 만료 시각(유닉스 초). 발급 시각보다 뒤다.
    pub expires_at: u64,
}

impl LicensePayload {
    fn validate(&self) -> Result<(), KeyError> {
        // id는 인증 파일·명령 출력에 그대로 들어가므로 안전한 문자만 받는다.
        let id = self.id.as_bytes();
        let safe = id.first().is_some_and(u8::is_ascii_alphanumeric)
            && id.len() <= 64
            && id
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if !safe {
            return Err(KeyError::Format(
                "id must be 1-64 letters, digits, '.', '_' or '-' and start with a letter or digit"
                    .into(),
            ));
        }
        let len = self.customer.chars().count();
        if !(1..=200).contains(&len) {
            return Err(KeyError::Format("customer must be 1-200 characters".into()));
        }
        if self.nodes == 0 {
            return Err(KeyError::Format("nodes must be at least 1".into()));
        }
        if self.expires_at > MAX_EXPIRES_AT {
            return Err(KeyError::Format(
                "expires_at must not be after 9999-12-31T23:59:59Z".into(),
            ));
        }
        if self.expires_at <= self.issued_at {
            return Err(KeyError::Format(
                "expires_at must be after issued_at".into(),
            ));
        }
        Ok(())
    }
}

/// 서명이 맞는 키의 내용
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct License {
    pub id: String,
    pub customer: String,
    pub level: Level,
    pub nodes: u32,
    pub issued_at: u64,
    pub expires_at: u64,
    /// 저장소의 개발 키로 검증됨
    pub development: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("malformed subscription key: {0}")]
    Format(String),
    #[error("the subscription key signature does not match this build")]
    Signature,
    #[error("the subscription key expired on {}", date(*.0))]
    Expired(u64),
}

/// 검증에 쓰는 Ed25519 공개 키
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    pub bytes: [u8; 32],
    /// 저장소의 개발 키
    pub development: bool,
}

impl PublicKey {
    /// base64(표준) 32바이트 공개 키
    pub fn from_base64(text: &str, development: bool) -> Option<PublicKey> {
        let bytes = STANDARD.decode(text.trim()).ok()?.try_into().ok()?;
        Some(PublicKey { bytes, development })
    }
}

/// 이 빌드가 믿는 공개 키. 빌드 값 `OPL_LICENSE_KEYS`(base64 표준 공개 키를 쉼표로)가 있으면 그것만,
/// 없거나 공백뿐이면 저장소의 개발 키 하나다. 그 밖에 풀리는 키가 없으면(`","` 같은 값) 빈 목록이다.
pub fn license_keys() -> Vec<PublicKey> {
    keys_from(option_env!("OPL_LICENSE_KEYS"))
}

/// [`license_keys`]의 본체. 풀리지 않는 항목은 건너뛴다(그 키로 서명한 키는 invalid가 된다).
/// 목록에 든 개발 공개 키도 개발 키로 표시한다.
fn keys_from(list: Option<&str>) -> Vec<PublicKey> {
    let dev = PublicKey::from_base64(DEV_KEY, true);
    match list {
        Some(list) if !list.trim().is_empty() => list
            .split(',')
            .filter(|k| !k.trim().is_empty())
            .filter_map(|k| PublicKey::from_base64(k, false))
            .map(|mut k| {
                k.development = dev.as_ref().is_some_and(|d| d.bytes == k.bytes);
                k
            })
            .collect(),
        _ => dev.into_iter().collect(),
    }
}

/// 서명만 확인하고 내용을 푼다(만료는 보지 않는다). 공개 키 중 하나라도 맞으면 된다.
pub fn verify(key: &str, keys: &[PublicKey]) -> Result<License, KeyError> {
    let key = key.trim();
    let parts: Vec<&str> = key.split('.').collect();
    let [prefix, payload, sig] = parts[..] else {
        return Err(KeyError::Format(
            "expected opl1.<payload>.<signature>".into(),
        ));
    };
    if prefix != PREFIX {
        return Err(KeyError::Format(format!("unknown prefix {prefix:?}")));
    }
    let json = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| KeyError::Format("payload is not base64url".into()))?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig)
        .map_err(|_| KeyError::Format("signature is not base64url".into()))?;
    let signed = &key[..PREFIX.len() + 1 + payload.len()];
    let signer = keys
        .iter()
        .find(|k| {
            UnparsedPublicKey::new(&ED25519, &k.bytes)
                .verify(signed.as_bytes(), &sig)
                .is_ok()
        })
        .ok_or(KeyError::Signature)?;
    let p: LicensePayload = serde_json::from_slice(&json)
        .map_err(|e| KeyError::Format(format!("payload is not valid JSON: {e}")))?;
    p.validate()?;
    Ok(License {
        id: p.id,
        customer: p.customer,
        level: p.level,
        nodes: p.nodes,
        issued_at: p.issued_at,
        expires_at: p.expires_at,
        development: signer.development,
    })
}

/// 키를 풀고 만료(`now >= expires_at`)까지 확인한다. 새로 넣는 키에 쓴다.
pub fn parse(key: &str, keys: &[PublicKey], now: u64) -> Result<License, KeyError> {
    let license = verify(key, keys)?;
    if now >= license.expires_at {
        return Err(KeyError::Expired(license.expires_at));
    }
    Ok(license)
}

/// PKCS#8(v2) Ed25519 비밀 키로 키 문자열을 만든다. 내용 검사는 [`verify`]와 같다.
pub fn sign(pkcs8: &[u8], license: &LicensePayload) -> Result<String, KeyError> {
    license.validate()?;
    let json = serde_json::to_vec(license).map_err(|e| KeyError::Format(e.to_string()))?;
    sign_bytes(pkcs8, &json)
}

fn sign_bytes(pkcs8: &[u8], json: &[u8]) -> Result<String, KeyError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| KeyError::Format("not a PKCS#8 Ed25519 private key".into()))?;
    let signed = format!("{PREFIX}.{}", URL_SAFE_NO_PAD.encode(json));
    let sig = pair.sign(signed.as_bytes());
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
}

/// 새로 만든 키 쌍(둘 다 base64 표준 한 줄)
pub struct GeneratedKey {
    /// PKCS#8(v2) 비밀 키
    pub private_key: String,
    /// 32바이트 공개 키(`OPL_LICENSE_KEYS`의 항목)
    pub public_key: String,
}

/// 발급용 Ed25519 키 쌍을 만든다.
pub fn generate_key() -> Result<GeneratedKey, KeyError> {
    let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| KeyError::Format("generate a key pair".into()))?;
    let pair = Ed25519KeyPair::from_pkcs8(doc.as_ref())
        .map_err(|_| KeyError::Format("generate a key pair".into()))?;
    Ok(GeneratedKey {
        private_key: STANDARD.encode(doc.as_ref()),
        public_key: STANDARD.encode(pair.public_key().as_ref()),
    })
}

/// PKCS#8(v2) Ed25519 비밀 키의 공개 키(base64 표준, `OPL_LICENSE_KEYS`의 항목)
pub fn public_key(pkcs8: &[u8]) -> Result<String, KeyError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
        .map_err(|_| KeyError::Format("not a PKCS#8 Ed25519 private key".into()))?;
    Ok(STANDARD.encode(pair.public_key().as_ref()))
}

/// 비밀 키 파일(base64 표준 한 줄)의 PKCS#8 바이트
pub fn decode_private_key(text: &str) -> Result<Vec<u8>, KeyError> {
    STANDARD
        .decode(text.trim())
        .map_err(|_| KeyError::Format("the private key file is not base64".into()))
}

/// 구독 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    /// 키가 없다.
    None,
    Active,
    Expired,
    /// 저장된 키가 이 빌드의 공개 키로 검증되지 않는다.
    Invalid,
    /// 위트니스가 아닌 노드가 한도보다 많다.
    OverLimit,
}

/// 읽을 때 계산한 구독 상태. 키 문자열은 담지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubscriptionInfo {
    pub state: SubscriptionState,
    // 아래 Option들은 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub customer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub level: Option<Level>,
    /// 노드 수 한도
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub nodes: Option<u32>,
    /// 위트니스가 아닌 노드 수
    pub used_nodes: u32,
    /// 만료 시각(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub expires_at: Option<u64>,
    /// 개발 키로 검증됨
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub development: Option<bool>,
}

/// 저장된 키의 상태. 키 없음 → none, 서명·형식 실패 → invalid, 만료 → expired,
/// 노드 수 초과 → over_limit, 그 밖 active.
pub fn status(
    key: Option<&str>,
    keys: &[PublicKey],
    now: u64,
    used_nodes: u32,
) -> SubscriptionInfo {
    let mut info = SubscriptionInfo {
        state: SubscriptionState::None,
        id: None,
        customer: None,
        level: None,
        nodes: None,
        used_nodes,
        expires_at: None,
        development: None,
    };
    let Some(key) = key else {
        return info;
    };
    let Ok(license) = verify(key, keys) else {
        info.state = SubscriptionState::Invalid;
        return info;
    };
    info.state = if now >= license.expires_at {
        SubscriptionState::Expired
    } else if used_nodes > license.nodes {
        SubscriptionState::OverLimit
    } else {
        SubscriptionState::Active
    };
    info.id = Some(license.id);
    info.customer = Some(license.customer);
    info.level = Some(license.level);
    info.nodes = Some(license.nodes);
    info.expires_at = Some(license.expires_at);
    info.development = Some(license.development);
    info
}

/// 유닉스 초의 UTC 날짜(RFC 3339 full-date, `YYYY-MM-DD`)
pub fn date(at: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at.min(i64::MAX as u64) as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}", t.year(), u8::from(t.month()), t.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 저장소 개발 키의 비밀 키 견본
    const DEV_PRIVATE: &str = include_str!("../tests/fixtures/dev-license.key");
    const DAY: u64 = 86_400;
    const NOW: u64 = 1_800_000_000;

    fn dev_keys() -> Vec<PublicKey> {
        keys_from(None)
    }

    fn payload() -> LicensePayload {
        LicensePayload {
            id: "lic-0001".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 3,
            issued_at: NOW - DAY,
            expires_at: NOW + 30 * DAY,
        }
    }

    fn dev_sign(p: &LicensePayload) -> String {
        sign(&decode_private_key(DEV_PRIVATE).unwrap(), p).unwrap()
    }

    fn dev_sign_json(json: &str) -> String {
        sign_bytes(&decode_private_key(DEV_PRIVATE).unwrap(), json.as_bytes()).unwrap()
    }

    fn is_format(r: Result<License, KeyError>) -> bool {
        matches!(r, Err(KeyError::Format(_)))
    }

    #[test]
    fn a_key_signed_with_the_dev_fixture_parses() {
        let key = dev_sign(&payload());
        assert!(key.starts_with("opl1."));
        let license = parse(&format!("  {key}\n"), &dev_keys(), NOW).unwrap();
        assert_eq!(
            license,
            License {
                id: "lic-0001".into(),
                customer: "Example Corp".into(),
                level: Level::Premium,
                nodes: 3,
                issued_at: NOW - DAY,
                expires_at: NOW + 30 * DAY,
                development: true,
            }
        );
    }

    #[test]
    fn the_dev_fixture_matches_the_dev_public_key() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();
        assert_eq!(dev_keys().len(), 1);
        assert_eq!(pair.public_key().as_ref(), dev_keys()[0].bytes);
        assert!(dev_keys()[0].development);
        assert_eq!(public_key(&pkcs8).unwrap(), DEV_KEY.trim());
        assert!(public_key(b"not a key").is_err());
    }

    #[test]
    fn tampered_keys_are_rejected() {
        let key = dev_sign(&payload());
        let (signed, sig) = key.rsplit_once('.').unwrap();
        // 다른 내용(노드 수 99)에 원래 서명
        let other = dev_sign(&LicensePayload {
            nodes: 99,
            ..payload()
        });
        let other_payload = other.split('.').nth(1).unwrap();
        let swapped = format!("opl1.{other_payload}.{sig}");
        assert_eq!(verify(&swapped, &dev_keys()), Err(KeyError::Signature));
        // 서명 바이트 하나를 바꿈
        let mut bytes = URL_SAFE_NO_PAD.decode(sig).unwrap();
        bytes[10] ^= 1;
        let flipped = format!("{signed}.{}", URL_SAFE_NO_PAD.encode(&bytes));
        assert_eq!(verify(&flipped, &dev_keys()), Err(KeyError::Signature));
        // 다른 공개 키
        let stranger = generate_key().unwrap();
        let keys = vec![PublicKey::from_base64(&stranger.public_key, false).unwrap()];
        assert_eq!(verify(&key, &keys), Err(KeyError::Signature));
        assert_eq!(verify(&key, &[]), Err(KeyError::Signature));
        assert_eq!(
            KeyError::Signature.to_string(),
            "the subscription key signature does not match this build"
        );
    }

    #[test]
    fn any_listed_key_may_match() {
        let stranger = generate_key().unwrap();
        let mut keys = vec![PublicKey::from_base64(&stranger.public_key, false).unwrap()];
        keys.extend(dev_keys());
        let license = verify(&dev_sign(&payload()), &keys).unwrap();
        assert!(license.development);
        // 다른 키로 서명한 키는 development가 아니다.
        let pkcs8 = decode_private_key(&stranger.private_key).unwrap();
        let key = sign(&pkcs8, &payload()).unwrap();
        assert!(!verify(&key, &keys).unwrap().development);
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let key = dev_sign(&payload());
        let (prefix_payload, sig) = key.rsplit_once('.').unwrap();
        let payload_part = prefix_payload.strip_prefix("opl1.").unwrap();
        for bad in [
            String::new(),
            "opl1".into(),
            prefix_payload.to_string(),
            format!("{key}.x"),
            format!("opl2.{payload_part}.{sig}"),
            format!("opl1.{payload_part}!.{sig}"),
            format!("opl1.{payload_part}.{sig}="),
        ] {
            assert!(is_format(verify(&bad, &dev_keys())), "{bad:?}");
        }
        // 서명은 맞지만 내용이 틀림
        for json in [
            "not json",
            r#"{"id":"a","customer":"c","level":"gold","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"","customer":"c","level":"standard","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"","level":"standard","nodes":1,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":0,"issued_at":1,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":1,"issued_at":2,"expires_at":2}"#,
            r#"{"id":"a","customer":"c","level":"standard","nodes":1,"issued_at":1}"#,
        ] {
            assert!(
                is_format(verify(&dev_sign_json(json), &dev_keys())),
                "{json}"
            );
        }
        let long_id = "x".repeat(65);
        let json = format!(
            r#"{{"id":"{long_id}","customer":"c","level":"standard","nodes":1,"issued_at":1,"expires_at":2}}"#
        );
        assert!(is_format(verify(&dev_sign_json(&json), &dev_keys())));
        let err = verify("x.y", &dev_keys()).unwrap_err().to_string();
        assert!(err.starts_with("malformed subscription key: "), "{err}");
    }

    #[test]
    fn ids_are_limited_to_safe_characters() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        for id in ["Acme 2026", "a\nb", "-a", ".a", "a/b", "가"] {
            let p = LicensePayload {
                id: id.into(),
                ..payload()
            };
            assert!(
                matches!(sign(&pkcs8, &p), Err(KeyError::Format(_))),
                "{id:?}"
            );
            let json = serde_json::to_string(&p).unwrap();
            assert!(
                is_format(verify(&dev_sign_json(&json), &dev_keys())),
                "{id:?}"
            );
        }
        for id in ["a", "Acme-2026_01.x", "0", &"x".repeat(64)] {
            let p = LicensePayload {
                id: id.to_string(),
                ..payload()
            };
            assert!(verify(&dev_sign(&p), &dev_keys()).is_ok(), "{id:?}");
        }
    }

    #[test]
    fn expiry_is_capped_at_the_year_9999() {
        let max = LicensePayload {
            expires_at: MAX_EXPIRES_AT,
            ..payload()
        };
        assert!(verify(&dev_sign(&max), &dev_keys()).is_ok());
        let json = serde_json::to_string(&LicensePayload {
            expires_at: MAX_EXPIRES_AT + 1,
            ..payload()
        })
        .unwrap();
        assert!(is_format(verify(&dev_sign_json(&json), &dev_keys())));
        assert_eq!(date(MAX_EXPIRES_AT), "9999-12-31");
    }

    #[test]
    fn sign_checks_the_payload_too() {
        let pkcs8 = decode_private_key(DEV_PRIVATE).unwrap();
        let bad = LicensePayload {
            nodes: 0,
            ..payload()
        };
        assert!(matches!(sign(&pkcs8, &bad), Err(KeyError::Format(_))));
        assert!(matches!(
            sign(b"not a key", &payload()),
            Err(KeyError::Format(_))
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = r#"{"id":"a","customer":"c","level":"standard","nodes":2,"issued_at":1,"expires_at":2,"features":["x"]}"#;
        let license = verify(&dev_sign_json(json), &dev_keys()).unwrap();
        assert_eq!(license.level, Level::Standard);
        assert_eq!(license.nodes, 2);
    }

    #[test]
    fn expired_keys_fail_to_parse_but_verify() {
        let key = dev_sign(&payload());
        let expires = NOW + 30 * DAY;
        assert!(parse(&key, &dev_keys(), expires - 1).is_ok());
        assert_eq!(
            parse(&key, &dev_keys(), expires),
            Err(KeyError::Expired(expires))
        );
        assert!(verify(&key, &dev_keys()).is_ok());
        assert_eq!(
            KeyError::Expired(1_767_225_600).to_string(),
            "the subscription key expired on 2026-01-01"
        );
    }

    #[test]
    fn status_is_computed_from_time_and_nodes() {
        let key = dev_sign(&payload());
        let keys = dev_keys();

        let none = status(None, &keys, NOW, 2);
        assert_eq!(none.state, SubscriptionState::None);
        assert_eq!(none.used_nodes, 2);
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            serde_json::json!({ "state": "none", "used_nodes": 2 })
        );

        let active = status(Some(&key), &keys, NOW, 2);
        assert_eq!(
            serde_json::to_value(&active).unwrap(),
            serde_json::json!({
                "state": "active", "id": "lic-0001", "customer": "Example Corp",
                "level": "premium", "nodes": 3, "used_nodes": 2,
                "expires_at": NOW + 30 * DAY, "development": true,
            })
        );
        // 경계: 한도와 같으면 active
        assert_eq!(
            status(Some(&key), &keys, NOW, 3).state,
            SubscriptionState::Active
        );
        let over = status(Some(&key), &keys, NOW, 4);
        assert_eq!(over.state, SubscriptionState::OverLimit);
        assert_eq!(over.id.as_deref(), Some("lic-0001"));
        // 만료가 노드 수 초과보다 먼저다. 만료돼도 내용은 보인다.
        let expired = status(Some(&key), &keys, NOW + 30 * DAY, 4);
        assert_eq!(expired.state, SubscriptionState::Expired);
        assert_eq!(expired.level, Some(Level::Premium));

        let invalid = status(Some(&key), &[], NOW, 1);
        assert_eq!(
            serde_json::to_value(&invalid).unwrap(),
            serde_json::json!({ "state": "invalid", "used_nodes": 1 })
        );
        assert_eq!(
            status(Some("garbage"), &keys, NOW, 1).state,
            SubscriptionState::Invalid
        );
        assert_eq!(
            serde_json::to_value(SubscriptionState::OverLimit).unwrap(),
            "over_limit"
        );
    }

    #[test]
    fn build_keys_replace_the_dev_key() {
        let a = generate_key().unwrap();
        let b = generate_key().unwrap();
        let keys = keys_from(Some(&format!(" {} ,,{}", a.public_key, b.public_key)));
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|k| !k.development));
        assert_eq!(
            keys[0],
            PublicKey::from_base64(&a.public_key, false).unwrap()
        );
        // 빈 값은 없는 것과 같다.
        assert_eq!(keys_from(Some("")), dev_keys());
        assert_eq!(keys_from(Some(" ")), dev_keys());
        // 빈 항목만 있으면 개발 키로 떨어지지 않고 어떤 키도 믿지 않는다.
        assert_eq!(keys_from(Some(",")), vec![]);
        assert_eq!(keys_from(Some(" , ,")), vec![]);
        // 개발 공개 키를 목록에 넣어도 개발 키다.
        let keys = keys_from(Some(&format!("{},{}", a.public_key, DEV_KEY.trim())));
        assert_eq!(keys.len(), 2);
        assert!(!keys[0].development);
        assert!(keys[1].development);
        // 풀리지 않는 항목은 건너뛴다.
        assert_eq!(keys_from(Some("not-base64,AAAA")), vec![]);
        let want = match option_env!("OPL_LICENSE_KEYS") {
            Some(v) if !v.trim().is_empty() => keys_from(Some(v)),
            _ => dev_keys(),
        };
        assert_eq!(license_keys(), want);
    }
}
```

`crates/opl-types/tests/fixtures/dev-license.key`를 만든다:

```
MFECAQEwBQYDK2VwBCIEIFoJY/2xpWkg/PKvQjpX9bNbwjvivnVgZbaospRwsMoEgSEAagJBkv/4s2B9yquPvRhRIfR/c5MpSEvc5h65eeYLsJo=
```

`packaging/deb/build-deb.sh`에서

```sh
# /usr/share/keyrings/oplite-archive-keyring.gpg로 싣는다.
set -eu
```

을 다음으로 바꾼다:

```sh
# /usr/share/keyrings/oplite-archive-keyring.gpg로 싣는다.
# 구독 키 공개 키: 바이너리를 OPL_LICENSE_KEYS로 빌드한 정식 패키지는 그 값을 여기에도 준다. 없으면
# 저장소의 개발 키를 믿는 바이너리이므로 OPL_DEV_LICENSE=1(개발 패키지)일 때만 만든다.
# OPL_LICENSE_KEYS에는 정규 base64(44자, 32바이트) 항목이 하나 이상 있어야 하고, 개발 키가 들어 있으면
# 안 되며, opld에 그 값이 그대로 들어 있어야 한다(그 값으로 빌드했는지).
set -eu
```

`packaging/deb/build-deb.sh`에서

```sh

DEB_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEB_DIR/../.." && pwd)
BIN_DIR=${OPL_BIN_DIR:-$ROOT/target/release}
```

을 다음으로 바꾼다:

```sh

if [ -z "${OPL_LICENSE_KEYS:-}" ] && [ "${OPL_DEV_LICENSE:-}" != 1 ]; then
	echo "build-deb: OPL_LICENSE_KEYS is not set; set it for a release build or OPL_DEV_LICENSE=1 for a development package" >&2
	exit 1
fi

DEB_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEB_DIR/../.." && pwd)
BIN_DIR=${OPL_BIN_DIR:-$ROOT/target/release}

if [ -n "${OPL_LICENSE_KEYS:-}" ]; then
	DEV_KEY=$(cat "$ROOT/crates/opl-types/keys/dev-license.pub")
	set -f
	old_ifs=$IFS
	IFS=,
	n=0
	for k in $OPL_LICENSE_KEYS; do
		k=$(printf %s "$k" | tr -d ' \t\n')
		[ -n "$k" ] || continue
		n=$((n + 1))
		# 표준 base64 정규형(44자, = 끝, 풀었다 다시 만들면 같음)이고 32바이트여야 한다.
		# GNU base64는 패딩 없는 값·끝 비트가 0이 아닌 값도 풀어 주므로 길이와 왕복을 함께 본다.
		if [ "${#k}" != 44 ] || [ "${k%=}" = "$k" ] ||
			[ "$(printf %s "$k" | base64 -d 2>/dev/null | base64)" != "$k" ] ||
			[ "$(printf %s "$k" | base64 -d 2>/dev/null | wc -c | tr -d ' ')" != 32 ]; then
			echo "build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: $k" >&2
			exit 1
		fi
		if [ "$k" = "$DEV_KEY" ]; then
			echo "build-deb: OPL_LICENSE_KEYS contains the development key; use OPL_DEV_LICENSE=1 for a development package" >&2
			exit 1
		fi
	done
	IFS=$old_ifs
	set +f
	if [ "$n" -eq 0 ]; then
		echo "build-deb: OPL_LICENSE_KEYS has no keys" >&2
		exit 1
	fi
fi
```

`packaging/deb/build-deb.sh`에서

```sh
	fi
done

APT_KEY=${OPL_APT_KEY:-}
```

을 다음으로 바꾼다:

```sh
	fi
done
# 정식 패키지: opld가 이 OPL_LICENSE_KEYS로 빌드됐는지(빌드 값은 바이너리에 그대로 들어간다)
if [ -n "${OPL_LICENSE_KEYS:-}" ] && ! grep -qaF -- "$OPL_LICENSE_KEYS" "$BIN_DIR/opld"; then
	echo "build-deb: opld was not built with this OPL_LICENSE_KEYS" >&2
	exit 1
fi

APT_KEY=${OPL_APT_KEY:-}
```

`packaging/tests/build-deb-test.sh`에서

```sh
#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지, OPL_APT_KEY의 키링을 싣는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu
```

을 다음으로 바꾼다:

```sh
#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지, OPL_APT_KEY의 키링을 싣는지,
# OPL_LICENSE_KEYS 없이는 OPL_DEV_LICENSE=1일 때만 만드는지, OPL_LICENSE_KEYS의 항목·개발 키·opld에 든 값을
# 확인하는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
set -eu

# 사용자 셸의 값에 결과가 흔들리지 않게 한다(아래 run이 OPL_DEV_LICENSE 기본 1을 준다).
unset OPL_DEV_LICENSE OPL_LICENSE_KEYS
```

`packaging/tests/build-deb-test.sh`에서

```sh

# fake_bins <opld 버전> <oplctl 버전>: <bin> --version이 "<bin> <버전>"을 찍는 가짜 바이너리
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "opld %s"\n' "$1" >"$WORK/bin/opld"
	printf '#!/bin/sh\necho "oplctl %s"\n' "$2" >"$WORK/bin/oplctl"
```

을 다음으로 바꾼다:

```sh

# fake_bins <opld 버전> <oplctl 버전> [opld에 넣을 문자열]: <bin> --version이 "<bin> <버전>"을 찍는
# 가짜 바이너리. 세 번째 인자는 OPL_LICENSE_KEYS로 빌드한 opld처럼 바이너리 안에 둔다.
fake_bins() {
	rm -rf "${WORK:?}/bin"
	mkdir -p "$WORK/bin"
	printf '#!/bin/sh\necho "opld %s"\n# %s\n' "$1" "${3-}" >"$WORK/bin/opld"
	printf '#!/bin/sh\necho "oplctl %s"\n' "$2" >"$WORK/bin/oplctl"
```

`packaging/tests/build-deb-test.sh`에서

```sh
# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" \
		sh "$DEB_DIR/build-deb.sh" "$1" "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
```

을 다음으로 바꾼다:

```sh
# run <버전>: build-deb.sh를 돌려 종료 코드(code)·stdout·stderr를 남긴다.
# OPL_DEV_LICENSE는 따로 주지 않으면 1(개발 패키지)이다.
run() {
	rm -rf "$WORK/out"
	code=0
	PATH="$WORK/fakebin:$PATH" OPL_BIN_DIR="$WORK/bin" OPL_DEV_LICENSE="${OPL_DEV_LICENSE-1}" \
		sh "$DEB_DIR/build-deb.sh" "$1" "$WORK/out" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
```

`packaging/tests/build-deb-test.sh`에서

```sh

if [ "$FAILED" -ne 0 ]; then
```

을 다음으로 바꾼다:

```sh

# 구독 키 공개 키: OPL_LICENSE_KEYS도 OPL_DEV_LICENSE=1도 없으면 만들지 않는다.
fake_bins 0.1.0 0.1.0
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'build-deb: OPL_LICENSE_KEYS is not set; set it for a release build or OPL_DEV_LICENSE=1 for a development package' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "OPL_LICENSE_KEYS 없이 개발 패키지가 아니면 실패"
else
	fail "OPL_LICENSE_KEYS 없음: 종료 코드 $code"
fi
OPL_LICENSE_KEYS='' OPL_DEV_LICENSE=0 run 0.1.0
if [ "$code" -eq 1 ] && [ ! -e "$WORK/out" ]; then
	ok "OPL_DEV_LICENSE가 1이 아니면 실패"
else
	fail "OPL_DEV_LICENSE=0: 종료 코드 $code"
fi
# 정식 키: opl-license keygen과 같은 형식(base64 32바이트)의 개발 키가 아닌 키 둘
RELEASE_KEYS='Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM=,mR8vT2wX4yZ6aB8cD0eF2gH4iJ6kL8mN0oP2qR4sT6U='
DEV_KEY=$(cat "$DEB_DIR/../../crates/opl-types/keys/dev-license.pub")
fake_bins 0.1.0 0.1.0 "$RELEASE_KEYS"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
if [ "$code" -eq 0 ] && [ -f "$WORK/out/oplite_0.1.0_amd64.deb" ]; then
	ok "OPL_LICENSE_KEYS가 있으면 만든다"
else
	fail "OPL_LICENSE_KEYS 있음: 종료 코드 $code"
	cat "$WORK/stderr"
fi
# check_key_error <이름> <문구>: 종료 코드 1, 그 문구, 패키지 없음
check_key_error() {
	if [ "$code" -eq 1 ] && grep -qF -- "$2" "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
fake_bins 0.1.0 0.1.0 "$DEV_KEY"
OPL_LICENSE_KEYS=$DEV_KEY OPL_DEV_LICENSE='' run 0.1.0
check_key_error "OPL_LICENSE_KEYS의 개발 키는 거절" \
	'build-deb: OPL_LICENSE_KEYS contains the development key; use OPL_DEV_LICENSE=1 for a development package'
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$DEV_KEY"
OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$DEV_KEY" OPL_DEV_LICENSE='' run 0.1.0
check_key_error "목록에 섞인 개발 키도 거절" 'OPL_LICENSE_KEYS contains the development key'
# 패딩 없는 키, 끝 비트가 0이 아닌(정규형이 아닌) 키도 거절한다.
for bad in 'not-base64!' 'AAAA' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyMA' \
	'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyM' 'Hk3pQ0m2u8Yx1bVd7cNf4sLw9gTzR6aJe5iKoP3qUyN='; do
	fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*},$bad"
	OPL_LICENSE_KEYS="${RELEASE_KEYS%%,*},$bad" OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "32바이트 base64가 아닌 항목 거절: $bad" \
		"build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: $bad"
done
# 공백·쉼표뿐이면 키가 하나도 없다.
for none in ' ' ','; do
	fake_bins 0.1.0 0.1.0 "$none"
	OPL_LICENSE_KEYS=$none OPL_DEV_LICENSE='' run 0.1.0
	check_key_error "키가 없는 OPL_LICENSE_KEYS 거절: '$none'" 'build-deb: OPL_LICENSE_KEYS has no keys'
done
fake_bins 0.1.0 0.1.0 "${RELEASE_KEYS%%,*}"
OPL_LICENSE_KEYS=$RELEASE_KEYS OPL_DEV_LICENSE='' run 0.1.0
check_key_error "opld에 OPL_LICENSE_KEYS가 그대로 없으면 거절" \
	'build-deb: opld was not built with this OPL_LICENSE_KEYS'

if [ "$FAILED" -ne 0 ]; then
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check --workspace`
Expected: `Cargo.lock`의 opl-types·opl-api 의존 목록 줄만 바뀐다(새 패키지 없음, 이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1209개, 실패 0

Run: `bash packaging/tests/maint-test.sh && bash packaging/tests/build-deb-test.sh && bash packaging/tests/build-repo-test.sh && bash packaging/tests/setup-test.sh && bash packaging/tests/iso-test.sh`
Expected: 모두 통과

Run: `shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm`
Expected: 경고 없음 (bash는 `shopt -s globstar` 뒤에, shellcheck가 없으면 CI `shell` 작업이 본다)

- [ ] **Step 4: 커밋**

```bash
git add Cargo.toml Cargo.lock crates packaging .github
git commit -m "feat: 구독 키와 발급 도구 — opl_types::subscription(opl1.<payload>.<sig> 형식, base64url 패딩 없음, opl1.<payload> 바이트에 대한 Ed25519 서명, verify(서명·형식만)와 parse(만료까지), 모르는 필드 무시, id 1~64자·customer 1~200자·nodes ≥ 1·expires_at > issued_at, KeyError Format·Signature·Expired, sign·generate_key·decode_private_key, license_keys: 빌드 값 OPL_LICENSE_KEYS가 있으면 그것만 아니면 저장소 개발 키(development), status → SubscriptionInfo{state none|active|expired|invalid|over_limit, id, customer, level, nodes, used_nodes, expires_at, development}), 개발 키 쌍(crates/opl-types/keys/dev-license.pub, 시험 견본 tests/fixtures/dev-license.key), 발급 도구 opl-license keygen --out(0600, 있으면 거절)·issue(패키지에 넣지 않음), build-deb.sh는 OPL_LICENSE_KEYS가 없으면 OPL_DEV_LICENSE=1일 때만 만들고 CI build-deb action은 OPL_DEV_LICENSE=1, 새 직접 의존 ring·base64(opl-types)·clap(opl-api)"
```

---

### Task 2: 구독 API와 oplctl subscription

`Settings.spec.subscription`(키 문자열 — 없으면 직렬화하지 않아 계획 33 노드가 계속 풀 수 있다). `GET /api/v1/subscription`(로그인한 누구나, 상태는 읽을 때 위트니스가 아닌 노드 수로 계산), `PUT`(admin, 400 `invalid_subscription_key`), `DELETE`(admin). 새 게이트 `require_current`: 위트니스를 포함한 모든 노드가 이 빌드 이상이 아니면 새 필드를 쓰는 요청은 409 `cluster_upgrading`(계획 33 노드는 모르는 필드를 풀지 못한다). 키 문자열은 `GET`·`PATCH /settings`와 watch 이벤트로 나가지 않는다. oplctl `subscription show|set <KEY>|set --file <파일|->|remove`.

**Files:**
- Create: `crates/opl-api/src/subscription.rs`
- Create: `crates/opl-api/tests/subscription.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/settings.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-types/src/settings.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
        .collect();
    if old.is_empty() {
```

을 다음으로 바꾼다:

```rust
        .collect();
    upgrading(&mut old)
}

/// 이 빌드보다 낮은 노드나 버전을 적지 않은 노드(위트니스 포함)가 남아 있으면 409
/// `cluster_upgrading`으로 거절한다. 설정의 새 필드(구독 등)를 기본값이 아닌 값으로 쓰기 전에 부른다.
/// 옛 노드는 모르는 필드가 든 설정을 풀지 못한다.
pub(crate) fn require_current(cluster: &opl_cluster::Cluster) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| {
            n.status
                .version
                .as_deref()
                .is_none_or(|v| version::compare(v, version::BUILD).is_lt())
        })
        .map(|n| n.name)
        .collect();
    upgrading(&mut old)
}

/// 옛 노드 이름들(이름 순)로 `cluster_upgrading`을 만든다. 없으면 통과.
fn upgrading(old: &mut [String]) -> Result<(), ApiError> {
    if old.is_empty() {
```

`crates/opl-api/src/lib.rs`에서

```rust
mod state;
mod tasks;
```

을 다음으로 바꾼다:

```rust
mod state;
mod subscription;
mod tasks;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(r::list_pools, r::create_pool))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(
            subscription::get_subscription,
            subscription::put_subscription,
            subscription::delete_subscription
        ))
        .routes(routes!(r::list_pools, r::create_pool))
```

`crates/opl-api/src/settings.rs` 전체:

```rust
//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.
//! 구독 키 문자열은 API로 내보내지 않는다([`public`]).

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::if_match;
use crate::state::AppState;

/// 저장된 설정. 없으면 기본값을 담은 리소스(id는 nil UUID, revision 0)다.
pub fn current(cluster: &Cluster) -> Result<Settings, ClusterError> {
    Ok(cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
        .unwrap_or_else(|| Resource {
            id: Uuid::nil(),
            ..Resource::new(SETTINGS_NAME, SettingsSpec::default())
        }))
}

/// API로 내보내는 설정: 구독 키 문자열을 뺀다.
pub(crate) fn public(mut settings: Settings) -> Settings {
    settings.spec.subscription = None;
    settings
}

/// 설정에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub updates: Option<UpdateSettingsPatch>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub channel: Option<Channel>,
}

/// 클러스터 설정. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    Ok(Json(public(current(&state.cluster)?)))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
    params(("If-Match" = Option<u64>, Header)),
    request_body = SettingsPatch,
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Json(req): Json<SettingsPatch>,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let Some(updates) = req.updates else {
        return Err(ApiError::bad_request("give updates"));
    };
    crate::cluster::require_upgraded(&state.cluster)?;
    let expected = if_match(&headers)?;
    let written = write(&state.cluster, expected, |spec| {
        if let Some(channel) = updates.channel {
            spec.updates.channel = channel;
        }
        Ok(())
    })
    .await?;
    Ok(Json(public(written)))
}

/// 설정을 읽어 `change`로 바꿔 쓴다. 설정이 아직 없으면 만든다(`expected`는 0). `expected`가
/// 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지 않는다.
/// 돌려주는 설정은 구독 키를 담고 있다.
pub(crate) async fn write(
    cluster: &Cluster,
    expected: Option<u64>,
    change: impl Fn(&mut SettingsSpec) -> Result<(), ApiError>,
) -> Result<Settings, ApiError> {
    for _ in 0..10 {
        let settings = current(cluster)?;
        if let Some(expected) = expected
            && expected != settings.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: settings.revision,
            })
            .into());
        }
        let mut spec = settings.spec.clone();
        change(&mut spec)?;
        if spec == settings.spec {
            return Ok(settings);
        }
        let written = if settings.revision == 0 {
            // 아직 없다. 다른 요청이 먼저 만들었으면 다시 읽는다.
            let mut created = Resource::new(SETTINGS_NAME, spec);
            match cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(created);
                }
                Err(e) => e,
            }
        } else {
            match cluster
                .update_spec(
                    settings.id,
                    Some(settings.revision),
                    settings.labels.clone(),
                    &spec,
                )
                .await
            {
                Ok(_) => return Ok(current(cluster)?),
                Err(e) => e,
            }
        };
        // If-Match 0으로 만들려 했는데 다른 요청이 먼저 만들었다.
        if let (ClusterError::Rejected(Rejection::NameTaken { .. }), Some(expected)) =
            (&written, expected)
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current(cluster)?.revision,
            })
            .into());
        }
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. })
                if expected.is_none() =>
            {
                continue;
            }
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/src/subscription.rs`를 만든다:

```rust
//! 구독 `GET/PUT/DELETE /api/v1/subscription`. 키 문자열은 설정(`Settings.spec.subscription`)에
//! 두고, API는 읽을 때 계산한 상태([`SubscriptionInfo`])만 보인다.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::settings::{Settings, SettingsSpec};
use opl_types::subscription::{self, SubscriptionInfo};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::settings::{current, write};
use crate::state::AppState;

/// 넣을 구독 키
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionKey {
    /// `opl1.<payload>.<signature>`
    pub key: String,
}

/// 설정에 저장된 키의 지금 상태. 노드 수는 위트니스가 아닌 노드만 센다.
fn info(cluster: &Cluster, settings: &Settings) -> Result<SubscriptionInfo, ClusterError> {
    let used = cluster
        .list::<NodeSpec>()?
        .1
        .iter()
        .filter(|n| !n.spec.witness)
        .count();
    Ok(subscription::status(
        settings.spec.subscription.as_deref(),
        &subscription::license_keys(),
        now(),
        u32::try_from(used).unwrap_or(u32::MAX),
    ))
}

/// 구독 상태. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/subscription", tag = "subscription",
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    let settings = current(&state.cluster)?;
    Ok(Json(info(&state.cluster, &settings)?))
}

/// 구독 키를 넣는다(있던 키는 바꾼다). 형식·서명이 맞지 않거나 만료된 키는 400
/// `invalid_subscription_key`.
#[utoipa::path(put, path = "/subscription", tag = "subscription",
    request_body = SubscriptionKey,
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<SubscriptionKey>,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let key = req.key.trim().to_string();
    let license = subscription::parse(&key, &subscription::license_keys(), now()).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_subscription_key",
            e.to_string(),
        )
    })?;
    crate::cluster::require_current(&state.cluster)?;
    let settings = write(&state.cluster, None, |spec| {
        spec.subscription = Some(key.clone());
        Ok(())
    })
    .await?;
    tracing::info!(id = %license.id, "subscription key set");
    Ok(Json(info(&state.cluster, &settings)?))
}

/// 구독 키를 지운다. 없으면 그대로 200이다.
#[utoipa::path(delete, path = "/subscription", tag = "subscription",
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    crate::cluster::require_current(&state.cluster)?;
    let settings = write(&state.cluster, None, |spec| {
        spec.subscription = None;
        Ok(())
    })
    .await?;
    tracing::info!("subscription key removed");
    Ok(Json(info(&state.cluster, &settings)?))
}
```

`crates/opl-api/src/watch.rs`에서

```rust

async fn stream(mut socket: WebSocket, mut watch: Watch, kinds: BTreeSet<Kind>) {
```

을 다음으로 바꾼다:

```rust

/// 설정 이벤트에서 구독 키 문자열을 뺀다(`settings::public`과 같이).
fn public(mut event: WatchEvent) -> WatchEvent {
    if let WatchEvent::Put { kind, resource } | WatchEvent::Deleted { kind, resource, .. } =
        &mut event
        && *kind == Kind::Settings
        && let Some(spec) = resource.spec.as_object_mut()
    {
        spec.remove("subscription");
    }
    event
}

async fn stream(mut socket: WebSocket, mut watch: Watch, kinds: BTreeSet<Kind>) {
```

`crates/opl-api/src/watch.rs`에서

```rust
                let text = match ev {
                    Ok(ev) if wanted(&ev, &kinds) => serde_json::to_string(&ev),
                    Ok(_) => continue,
```

을 다음으로 바꾼다:

```rust
                let text = match ev {
                    Ok(ev) if wanted(&ev, &kinds) => serde_json::to_string(&public(ev)),
                    Ok(_) => continue,
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/settings",
        "/nodes/{key}/updates/refresh",
```

을 다음으로 바꾼다:

```rust
        "/settings",
        "/subscription",
        "/nodes/{key}/updates/refresh",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "#/components/schemas/SettingsPatch"
    );
```

을 다음으로 바꾼다:

```rust
        "#/components/schemas/SettingsPatch"
    );
    // 구독 키 문자열은 설정 스키마에 없고, 상태만 따로 보인다.
    assert!(
        doc["components"]["schemas"]["SettingsSpec"]["properties"]["subscription"].is_null(),
        "{}",
        doc["components"]["schemas"]["SettingsSpec"]
    );
    assert_eq!(
        doc["paths"]["/subscription"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SubscriptionInfo"
    );
    let info = &doc["components"]["schemas"]["SubscriptionInfo"];
    assert_eq!(
        info["required"],
        serde_json::json!(["state", "used_nodes"]),
        "{info}"
    );
```

`crates/opl-api/tests/subscription.rs`를 만든다:

```rust
//! 구독 API: 넣기·지우기, 권한, 상태 계산(노드 수, 위트니스 제외), 키를 내보내지 않음.

mod common;

use common::Env;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use opl_types::version::BUILD;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const DAY: u64 = 86_400;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 개발 키로 서명한 키. 노드 `nodes`대까지, 지금부터 `days`일 뒤 만료.
fn key(nodes: u32, days: u64) -> String {
    key_at(nodes, now() - DAY, now() + days * DAY)
}

fn key_at(nodes: u32, issued_at: u64, expires_at: u64) -> String {
    sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes,
            issued_at,
            expires_at,
        },
    )
    .unwrap()
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 지금 빌드를 돌리는 노드를 더한다.
async fn add_node(env: &Env, name: &str, witness: bool, version: Option<&str>) {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness,
        },
    );
    node.status.version = version.map(Into::into);
    env.cluster.create(&node).await.unwrap();
}

fn stored_key(env: &Env) -> Option<String> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .and_then(|s| s.spec.subscription)
}

#[tokio::test]
async fn subscription_is_set_shown_and_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, got) = viewer.get("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, json!({ "state": "none", "used_nodes": 1 }));

    // 앞뒤 공백은 다듬어 저장한다.
    let k = key(3, 30);
    let (status, got) = admin
        .put("/subscription", json!({ "key": format!("  {k}\n") }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["state"], "active");
    assert_eq!(got["id"], "sub-1");
    assert_eq!(got["customer"], "Example Corp");
    assert_eq!(got["level"], "premium");
    assert_eq!(got["nodes"], 3);
    assert_eq!(got["used_nodes"], 1);
    assert_eq!(got["development"], true);
    assert!(got["expires_at"].as_u64().unwrap() > now());
    assert_eq!(stored_key(&env).as_deref(), Some(k.as_str()));
    let (status, shown) = viewer.get("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown, got);

    // 설정 응답은 키 문자열을 담지 않는다.
    let (_, settings) = viewer.get("/settings").await;
    assert_eq!(
        settings["spec"],
        json!({ "updates": { "channel": "stable" } })
    );
    let (status, patched) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["spec"],
        json!({ "updates": { "channel": "testing" } })
    );
    assert!(!patched.to_string().contains(&k));
    assert_eq!(stored_key(&env).as_deref(), Some(k.as_str()));

    let (status, got) = admin.delete("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, json!({ "state": "none", "used_nodes": 1 }));
    assert_eq!(stored_key(&env), None);
    let revision = admin.get("/settings").await.1["revision"].clone();
    // 이미 없으면 쓰지 않는다.
    let (status, got) = admin.delete("/subscription").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(admin.get("/settings").await.1["revision"], revision);
}

#[tokio::test]
async fn only_admins_change_the_subscription() {
    let env = Env::start().await;
    let k = key(3, 30);
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_eq!(client.get("/subscription").await.0, StatusCode::OK);
        assert_error(
            &client.put("/subscription", json!({ "key": k }), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.delete("/subscription").await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert_error(
        &env.anonymous().get("/subscription").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert_eq!(stored_key(&env), None);
}

#[tokio::test]
async fn bad_keys_are_rejected_with_the_reason() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let k = key(3, 30);
    // 서명 가운데 글자 하나를 바꾼다(마지막 글자는 남는 비트가 있어 형식 오류가 될 수 있다).
    let at = k.len() - 10;
    let swapped = if &k[at..=at] == "A" { "B" } else { "A" };
    let tampered = format!("{}{swapped}{}", &k[..at], &k[at + 1..]);
    // 2023-11-14T22:13:20Z에 발급, 하루 뒤 만료
    let expired = key_at(3, 1_700_000_000, 1_700_000_000 + DAY);
    for (body, message) in [
        (
            json!({ "key": "opl1.x" }),
            "malformed subscription key: expected opl1.<payload>.<signature>".to_string(),
        ),
        (
            json!({ "key": tampered }),
            "the subscription key signature does not match this build".to_string(),
        ),
        (
            json!({ "key": expired }),
            "the subscription key expired on 2023-11-15".to_string(),
        ),
    ] {
        let got = admin.put("/subscription", body, None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_subscription_key");
        assert_eq!(got.1["message"], message);
    }
    assert_error(
        &admin
            .put("/subscription", json!({ "key": k, "x": 1 }), None)
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_eq!(stored_key(&env), None);
}

/// 노드 수는 위트니스를 빼고 센다. 한도와 같으면 active, 넘으면 over_limit.
#[tokio::test]
async fn nodes_over_the_limit_are_reported() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, got) = admin
        .put("/subscription", json!({ "key": key(2, 30) }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["state"], "active");

    add_node(&env, "node2", false, Some(BUILD)).await;
    add_node(&env, "witness", true, Some(BUILD)).await;
    let (_, got) = admin.get("/subscription").await;
    assert_eq!(got["state"], "active", "{got}");
    assert_eq!(got["used_nodes"], 2);

    add_node(&env, "node3", false, Some(BUILD)).await;
    let (_, got) = admin.get("/subscription").await;
    assert_eq!(got["state"], "over_limit", "{got}");
    assert_eq!(got["used_nodes"], 3);
    assert_eq!(got["nodes"], 2);
}

/// 이 빌드보다 낮거나 버전을 적지 않은 노드(위트니스 포함)가 있으면 키를 쓰지 않는다.
#[tokio::test]
async fn subscription_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    add_node(&env, "old", true, Some("0.0.1~old")).await;
    add_node(&env, "ancient", false, None).await;
    let k = key(3, 30);
    for got in [
        admin.put("/subscription", json!({ "key": k }), None).await,
        admin.delete("/subscription").await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
        assert_eq!(
            got.1["message"],
            "some nodes run an older opLite (ancient, old); finish the upgrade on every node first"
        );
    }
    assert_eq!(stored_key(&env), None);
    // 읽기는 된다.
    assert_eq!(admin.get("/subscription").await.0, StatusCode::OK);
}

/// 설정 변경 구독(watch)도 키 문자열을 내보내지 않는다.
#[tokio::test]
async fn watch_does_not_send_the_key() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let since = viewer.get("/vms").await.1["revision"].as_u64().unwrap();
    let url = format!(
        "{}/api/v1/watch?kinds=settings&since={since}",
        viewer.base.replace("http://", "ws://")
    );
    let mut req = url.into_client_request().unwrap();
    let token = viewer.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let mut ws = tokio_tungstenite::connect_async(req).await.unwrap().0;

    let k = key(3, 30);
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let ev: Value = loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            break serde_json::from_str(&text).unwrap();
        }
    };
    assert_eq!(ev["kind"], "settings", "{ev}");
    assert_eq!(
        ev["resource"]["spec"],
        json!({ "updates": { "channel": "stable" } })
    );
    assert!(!ev.to_string().contains(&k));
}
```

`crates/opl-types/src/settings.rs`에서

```rust
    pub updates: UpdateSettings,
}
```

을 다음으로 바꾼다:

```rust
    pub updates: UpdateSettings,
    /// 구독 키 문자열. API는 내보내지 않고 `GET /subscription`이 상태만 보인다. 없으면 쓰지 않아
    /// 구독을 모르는 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(ignore))]
    pub subscription: Option<String>,
}
```

`crates/opl-types/src/settings.rs`에서

```rust
    }

    #[test]
    fn unknown_fields_are_rejected() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 구독이 없으면 계획 32 노드가 쓰던 모양 그대로 쓰고, 있으면 함께 쓰고 푼다.
    #[test]
    fn subscription_is_written_only_when_set() {
        let spec = SettingsSpec {
            updates: UpdateSettings {
                channel: Channel::Testing,
            },
            subscription: None,
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::json!({ "updates": { "channel": "testing" } })
        );
        let spec = SettingsSpec {
            subscription: Some("opl1.a.b".into()),
            ..spec
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "updates": { "channel": "testing" }, "subscription": "opl1.a.b" })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
    }

    #[test]
    fn unknown_fields_are_rejected() {
```

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: SettingsCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

을 다음으로 바꾼다:

```rust
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

`crates/oplctl/src/cli.rs`에서

```rust
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}
```

을 다음으로 바꾼다:

```rust
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
}
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
    }
```

을 다음으로 바꾼다:

```rust
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn cli_definition_is_valid() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1219개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 구독 API와 oplctl subscription — Settings.spec.subscription(키 문자열, 없으면 직렬화하지 않아 계획 32 노드가 계속 품, OpenAPI 스키마에서 숨김), GET /subscription(로그인한 누구나, 상태는 읽을 때 위트니스가 아닌 노드 수로 계산)·PUT {key}(admin, 형식·서명·만료 실패 400 invalid_subscription_key, 다듬어 저장)·DELETE(admin, 없으면 쓰지 않음), 새 게이트 require_current(위트니스 포함 모든 노드가 이 빌드 이상이어야, 아니면 409 cluster_upgrading), 설정 쓰기 다시 하기를 settings::write로 빼 함께 씀, GET·PATCH /settings와 watch의 설정 이벤트가 키 문자열을 내보내지 않음(settings::public), oplctl subscription show|set <KEY>|set --file|remove, schema.d.ts"
```

---

### Task 3: enterprise 채널과 APT 인증 파일

채널 `enterprise`: `PATCH /settings`가 구독이 active가 아니면 400 `subscription_required`(옛 노드가 있으면 409). 노드는 enterprise 채널이고 저장된 키의 서명이 맞으면 `/etc/apt/auth.conf.d/oplite.conf`(`machine <저장소 호스트> login <구독 id> password <키의 SHA-256>`, 0600, 관리 표시)를 쓰고, 채널을 바꾸거나 구독을 지우면 opld가 쓴 파일만 지운다. 설정 watch 이벤트에 소스 파일과 함께 맞춘다.

**Files:**
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/settings.rs`
- Modify: `crates/opl-api/src/subscription.rs`
- Modify: `crates/opl-api/src/updates.rs`
- Modify: `crates/opl-api/tests/common/fake_apt.rs`
- Modify: `crates/opl-api/tests/settings.rs`
- Modify: `crates/opl-api/tests/subscription.rs`
- Modify: `crates/opl-api/tests/updates.rs`
- Modify: `crates/opl-types/src/settings.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/config.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use updates::{KEYRING, UpdateAgent, Updates, UpdatesConfig};
```

을 다음으로 바꾼다:

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use updates::{AUTH_FILE, KEYRING, UpdateAgent, Updates, UpdatesConfig};
```

`crates/opl-api/src/settings.rs`에서

```rust
use axum::extract::State;
use axum::http::HeaderMap;
use opl_cluster::types::Rejection;
```

을 다음으로 바꾼다:

```rust
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
```

`crates/opl-api/src/settings.rs`에서

```rust
use opl_types::settings::{Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus};
use serde::Deserialize;
```

을 다음으로 바꾼다:

```rust
use opl_types::settings::{Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus};
use opl_types::subscription::SubscriptionState;
use serde::Deserialize;
```

`crates/opl-api/src/settings.rs`에서

```rust
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
```

을 다음으로 바꾼다:

```rust
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다. enterprise 채널은 모든 노드가 이 빌드 이상이고 구독이 active일 때만 고른다
/// (이미 enterprise여도 다시 보내면 검사한다).
#[utoipa::path(patch, path = "/settings", tag = "settings",
```

`crates/opl-api/src/settings.rs`에서

```rust
    crate::cluster::require_upgraded(&state.cluster)?;
    let expected = if_match(&headers)?;
    let written = write(&state.cluster, expected, |spec| {
        if let Some(channel) = updates.channel {
```

을 다음으로 바꾼다:

```rust
    crate::cluster::require_upgraded(&state.cluster)?;
    let enterprise = updates.channel == Some(Channel::Enterprise);
    // 옛 노드는 enterprise 채널을 풀지 못한다.
    let used = if enterprise {
        crate::cluster::require_current(&state.cluster)?;
        crate::subscription::used_nodes(&state.cluster)?
    } else {
        0
    };
    let expected = if_match(&headers)?;
    let written = write(&state.cluster, expected, |spec| {
        if enterprise
            && crate::subscription::state_of(spec, used).state != SubscriptionState::Active
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "subscription_required",
                "an active subscription is required for the enterprise channel",
            ));
        }
        if let Some(channel) = updates.channel {
```

`crates/opl-api/src/subscription.rs`에서

```rust

/// 설정에 저장된 키의 지금 상태. 노드 수는 위트니스가 아닌 노드만 센다.
fn info(cluster: &Cluster, settings: &Settings) -> Result<SubscriptionInfo, ClusterError> {
    let used = cluster
```

을 다음으로 바꾼다:

```rust

/// 설정에 저장된 키의 지금 상태
fn info(cluster: &Cluster, settings: &Settings) -> Result<SubscriptionInfo, ClusterError> {
    Ok(state_of(&settings.spec, used_nodes(cluster)?))
}

/// 구독이 세는 노드 수: 위트니스가 아닌 노드
pub(crate) fn used_nodes(cluster: &Cluster) -> Result<u32, ClusterError> {
    let used = cluster
```

`crates/opl-api/src/subscription.rs`에서

```rust
        .count();
    Ok(subscription::status(
        settings.spec.subscription.as_deref(),
        &subscription::license_keys(),
        now(),
        u32::try_from(used).unwrap_or(u32::MAX),
    ))
}
```

을 다음으로 바꾼다:

```rust
        .count();
    Ok(u32::try_from(used).unwrap_or(u32::MAX))
}

/// 설정 `spec`에 저장된 키의 지금 상태(노드 `used`대)
pub(crate) fn state_of(spec: &SettingsSpec, used: u32) -> SubscriptionInfo {
    subscription::status(
        spec.subscription.as_deref(),
        &subscription::license_keys(),
        now(),
        used,
    )
}
```

`crates/opl-api/src/updates.rs` 전체:

```rust
//! 노드의 APT 업데이트: opLite 저장소 소스 파일과 enterprise 채널 인증 파일 관리, 업데이트 확인(`apt-get -s dist-upgrade`),
//! 주기 확인 에이전트, 다시 확인 API(`POST /nodes/{key}/updates/refresh`), 업그레이드(`POST
//! /nodes/{key}/upgrade`, `systemd-run`의 일시 유닛에서 `apt-get dist-upgrade`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec, NodeStatus, NodeUpdates, PackageUpdate};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::settings::{Channel, SettingsSpec};
use opl_types::subscription;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::version::{BUILD, SUITE};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, node_name, now};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// opld가 쓴 소스·인증 파일의 첫 줄. 이 줄이 없는 파일은 관리자가 직접 관리하는 것으로 보고 건드리지 않는다.
pub const SOURCES_MARK: &str = "# managed by opld; edits are overwritten";
/// 패키지가 싣는 저장소 공개 키(`UpdatesConfig.keyring`의 기본값)
pub const KEYRING: &str = "/usr/share/keyrings/oplite-archive-keyring.gpg";
/// enterprise 채널의 APT 인증 파일(`UpdatesConfig.auth_file`의 기본값)
pub const AUTH_FILE: &str = "/etc/apt/auth.conf.d/oplite.conf";
/// `NodeStatus.updates`에 담는 패키지 수의 상한
pub const MAX_PACKAGES: usize = 500;
/// 다른 확인이 도는 중일 때의 오류
const CHECK_BUSY: &str = "another update check is running";
/// 확인 명령이 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 60초까지 기다리게 하는 apt 설정
const CHECK_LOCK_TIMEOUT: &str = "DPkg::Lock::Timeout=60";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// 설정 구독을 하지 못했을 때 다시 해 보는 간격
const RESUBSCRIBE: Duration = Duration::from_secs(10);
/// 업그레이드 셸이 로그 끝에 남기는 종료 상태 줄의 앞부분. `--collect`로 유닛이 사라진 뒤에도 결과를 안다.
const EXIT_MARK: &str = "opl-upgrade: exit ";
/// 업그레이드 셸이 맨 처음 로그에 남기는 줄. 없으면 유닛이 셸을 시작하지 못했다.
const START_MARK: &str = "opl-upgrade: start";
/// apt가 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 기다리는 시간(초)
const LOCK_TIMEOUT: u32 = 600;
/// 실패한 업그레이드의 오류에 담는 로그 끝 줄 수
const LOG_LINES: usize = 20;

/// 노드 업데이트 설정(opld `[updates]`를 푼 값). 업데이트가 꺼진 노드에는 없다.
#[derive(Debug, Clone)]
pub struct UpdatesConfig {
    /// 주기 확인 간격
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    pub first_check: Duration,
    /// opLite APT 저장소 주소. 비면 소스 파일을 쓰지 않는다.
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    pub sources_file: PathBuf,
    /// 저장소 공개 키. 없으면 소스 파일을 쓰지 않는다(apt가 서명을 확인하지 못한다).
    pub keyring: PathBuf,
    /// opld가 관리하는 APT 인증 파일(enterprise 채널에서 구독 키로 쓴다)
    pub auth_file: PathBuf,
    pub apt_get: PathBuf,
    pub systemd_run: PathBuf,
    pub systemctl: PathBuf,
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    pub poll: Duration,
}

/// 이 노드의 업데이트 확인. 확인은 한 번에 하나만 돈다.
#[derive(Debug)]
pub struct Updates {
    pub config: UpdatesConfig,
    lock: tokio::sync::Mutex<()>,
}

impl Updates {
    pub fn new(config: UpdatesConfig) -> Self {
        Self {
            config,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `apt-get update -q` 뒤 `apt-get -s -q dist-upgrade`의 `Inst` 줄로 올릴 패키지를 찾는다. 명령이
    /// 실패하거나 다른 확인이 도는 중이면(기다리지 않는다) `error`에 이유를 담는다.
    pub async fn check(&self) -> NodeUpdates {
        let Ok(_guard) = self.lock.try_lock() else {
            return failed(CHECK_BUSY.into());
        };
        self.check_locked().await
    }

    /// 다른 확인이 끝나기를 기다렸다가 확인한다. 잠금 줄에 서지 않고 `poll`마다 다시 해 본다. 잠금이
    /// 잡혀 있고 `give_up()`이면(업그레이드가 잠금을 잡았다) 확인하지 않고 `None`이다.
    async fn check_waiting(&self, give_up: impl Fn() -> bool) -> Option<NodeUpdates> {
        loop {
            if let Ok(_guard) = self.lock.try_lock() {
                return Some(self.check_locked().await);
            }
            if give_up() {
                return None;
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    async fn check_locked(&self) -> NodeUpdates {
        if let Err(e) = self.apt(&["update", "-q", "-o", CHECK_LOCK_TIMEOUT]).await {
            return failed(e);
        }
        match self
            .apt(&["-s", "-q", "-o", CHECK_LOCK_TIMEOUT, "dist-upgrade"])
            .await
        {
            Ok(out) => found(parse_simulate(&out), now()),
            Err(e) => failed(e),
        }
    }

    /// 확인해 결과를 노드 `node`의 `status.updates`에 쓴다. 확인이 실패했으면(결과를 쓴 뒤) 그 이유가
    /// 오류다. 다른 확인이 도는 중이면 쓰지 않는다(그 확인이 쓴다).
    pub(crate) async fn refresh(&self, cluster: &Cluster, node: Uuid) -> Result<(), String> {
        let updates = self.check().await;
        if updates.error.as_deref() == Some(CHECK_BUSY) {
            return Err(CHECK_BUSY.into());
        }
        self.record_check(cluster, node, updates).await
    }

    /// [`Self::refresh`]와 같지만 다른 확인이 도는 중이면 끝나기를 기다린다(사용자가 요청한 확인). 이
    /// 노드를 업그레이드하는 중이면 확인하지 않고 성공이다(업그레이드가 끝나면 확인한다).
    pub(crate) async fn refresh_waiting(
        &self,
        cluster: &Cluster,
        node: Uuid,
    ) -> Result<(), String> {
        match self.check_waiting(|| upgrading(cluster, node)).await {
            Some(updates) => self.record_check(cluster, node, updates).await,
            None => Ok(()),
        }
    }

    async fn record_check(
        &self,
        cluster: &Cluster,
        node: Uuid,
        updates: NodeUpdates,
    ) -> Result<(), String> {
        record(cluster, node, &updates)
            .await
            .map_err(|e| format!("cannot record the update check: {e}"))?;
        match updates.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// `apt-get <args>`(LANG=C)의 표준 출력. 실패하면 명령과 stderr 끝 줄이 오류다.
    async fn apt(&self, args: &[&str]) -> Result<String, String> {
        let apt_get = &self.config.apt_get;
        let output = tokio::process::Command::new(apt_get)
            .args(args)
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            // 에이전트가 멈추면(opld 종료) 확인도 끝낸다.
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", apt_get.display()))?;
        if !output.status.success() {
            return Err(format!(
                "apt-get {} failed ({}): {}",
                args.join(" "),
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// 업그레이드 Task `task`를 실행한다: 일시 유닛에서 apt를 돌리고(`resume`이면 이전 opld가 띄운
    /// 유닛을 이어서 본다) 유닛이 끝날 때까지 지켜본다. 성공이든 실패든 끝나면 업데이트를 다시 확인한다.
    /// 클러스터 어디든 다른 업그레이드가 돌고 있으면(두 요청이 함께 지나갔다) 시작하지 않는다.
    /// 업그레이드하는 동안 확인 잠금을 잡아 apt 잠금을 다투지 않고, 잠금을 잡은 채로 마지막 확인을 한다.
    pub(crate) async fn upgrade(
        &self,
        cluster: &Cluster,
        node: Uuid,
        task: Uuid,
        resume: bool,
    ) -> Result<(), String> {
        if !resume && let Some(other) = other_upgrade(cluster, task) {
            return Err(format!(
                "upgrade_in_progress: node {other:?} is being upgraded"
            ));
        }
        let (result, updates) = {
            let _guard = self.lock.lock().await;
            let result = self.run_upgrade(task, resume).await;
            (result, self.check_locked().await)
        };
        if let Err(e) = self.record_check(cluster, node, updates).await {
            tracing::warn!(error = %e, "update check after the upgrade failed");
        }
        result
    }

    async fn run_upgrade(&self, task: Uuid, resume: bool) -> Result<(), String> {
        let unit = upgrade_unit(task);
        let log = self.config.log_dir.join(format!("upgrade-{task}.log"));
        // 이 opld가 유닛을 띄웠는지. 이어 볼 때는 이전 opld가 띄우지 못했을 수 있다.
        let mut started = !resume;
        if started {
            self.start_unit(&unit, &log).await?;
        }
        loop {
            match self.unit_state(&unit).await? {
                Some(UnitState::Running) | None => {}
                Some(UnitState::Succeeded) => return Ok(()),
                Some(UnitState::Failed) => return Err(upgrade_failed(&log).await),
                Some(UnitState::Gone) => {
                    let text = read_log(&log).await;
                    if text.lines().any(|l| l == START_MARK) {
                        return self.judge_gone(&log, &text).await;
                    }
                    if started {
                        return Err("the upgrade did not start; try again".into());
                    }
                    // 이전 opld가 유닛을 띄우기 전에 멈췄다. apt는 다시 돌려도 된다.
                    self.start_unit(&unit, &log).await?;
                    started = true;
                    continue;
                }
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    /// `systemd-run`으로 apt를 opld 밖의 일시 유닛에서 띄운다. 패키지의 postinst가 opld를 다시
    /// 시작해도 apt는 끊기지 않는다. 출력은 로그 파일에 더한다.
    async fn start_unit(&self, unit: &str, log: &Path) -> Result<(), String> {
        let c = &self.config;
        tokio::fs::create_dir_all(&c.log_dir)
            .await
            .map_err(|e| format!("cannot create {}: {e}", c.log_dir.display()))?;
        let output = tokio::process::Command::new(&c.systemd_run)
            .arg(format!("--unit={unit}"))
            .arg("--collect")
            .arg("--property=Type=exec")
            .arg(format!(
                "--property=StandardOutput=append:{}",
                log.display()
            ))
            .arg(format!("--property=StandardError=append:{}", log.display()))
            .arg("--setenv=DEBIAN_FRONTEND=noninteractive")
            .arg("--setenv=LANG=C")
            .args(["/bin/sh", "-c", &upgrade_script(&c.apt_get)])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", c.systemd_run.display()))?;
        if !output.status.success() {
            return Err(format!(
                "systemd-run failed ({}): {}",
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(())
    }

    /// `systemctl show`로 읽은 유닛 상태. `systemctl`이 실패하면(systemd 자신을 올리는 중 등) `None`이다
    /// (다음에 다시 본다).
    async fn unit_state(&self, unit: &str) -> Result<Option<UnitState>, String> {
        let systemctl = &self.config.systemctl;
        let output = tokio::process::Command::new(systemctl)
            .args([
                "show",
                "-p",
                "ActiveState",
                "-p",
                "Result",
                "-p",
                "LoadState",
            ])
            .arg(format!("{unit}.service"))
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", systemctl.display()))?;
        if !output.status.success() {
            tracing::warn!(
                unit,
                status = %output.status,
                stderr = %tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES),
                "cannot read the upgrade unit; will retry"
            );
            return Ok(None);
        }
        Ok(Some(unit_state(&String::from_utf8_lossy(&output.stdout))))
    }

    /// 셸을 시작한 유닛이 사라졌다(`--collect`가 치웠다). 로그 `text`의 종료 상태 줄이 있으면 그것으로
    /// 판단한다. 없으면 apt가 도중에 끊겼다(전원이 꺼진 경우 등): `dpkg --audit`가 무언가 알리면 dpkg를
    /// 마저 설정하라고, 아니면 다시 시작하라고 실패한다.
    async fn judge_gone(&self, log: &Path, text: &str) -> Result<(), String> {
        match exit_status(text) {
            Some(0) => return Ok(()),
            Some(_) => return Err(upgrade_failed(log).await),
            None => {}
        }
        let dpkg = &self.config.dpkg;
        let output = tokio::process::Command::new(dpkg)
            .arg("--audit")
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", dpkg.display()))?;
        if output.status.success() && output.stdout.iter().all(u8::is_ascii_whitespace) {
            Err("the upgrade was interrupted; start it again".into())
        } else {
            Err("the upgrade was interrupted; run \"dpkg --configure -a\" on the node".into())
        }
    }

    /// 소스 파일(`apt_url`이 있을 때만)과 인증 파일을 지금 설정으로 맞춘다.
    /// 둘 중 하나라도 새로 썼으면 true다.
    fn sync_sources(&self, cluster: &Cluster) -> bool {
        let spec = match crate::settings::current(cluster) {
            Ok(settings) => settings.spec,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the settings for the APT sources");
                return false;
            }
        };
        let auth = self.sync_auth(&spec);
        if self.config.apt_url.is_empty() {
            return auth;
        }
        let channel = spec.updates.channel;
        let c = &self.config;
        let path = &c.sources_file;
        let sources = match write_sources(path, &c.apt_url, channel, &c.keyring) {
            Ok(SourcesOutcome::Written) => {
                tracing::info!(file = %path.display(), channel = channel.as_str(), "wrote the APT sources");
                true
            }
            Ok(
                SourcesOutcome::Unchanged | SourcesOutcome::Unmanaged | SourcesOutcome::NoKeyring,
            ) => false,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot write the APT sources");
                false
            }
        };
        sources || auth
    }

    /// 인증 파일을 설정 `spec`에 맞춘다. 새로 썼으면 true다.
    fn sync_auth(&self, spec: &SettingsSpec) -> bool {
        let path = &self.config.auth_file;
        let contents = auth_contents(&self.config.apt_url, spec);
        match write_auth(path, contents.as_deref()) {
            Ok(AuthOutcome::Written) => {
                tracing::info!(file = %path.display(), "wrote the APT credentials");
                true
            }
            Ok(AuthOutcome::Removed) => {
                tracing::info!(file = %path.display(), "removed the APT credentials");
                false
            }
            Ok(AuthOutcome::Unchanged | AuthOutcome::Unmanaged) => false,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot write the APT credentials");
                false
            }
        }
    }
}

fn failed(error: String) -> NodeUpdates {
    NodeUpdates {
        checked_at: Some(now()),
        error: Some(error),
        ..NodeUpdates::default()
    }
}

/// 찾은 패키지의 확인 결과. [`MAX_PACKAGES`]가 넘으면 앞의 것만 담는다.
fn found(mut packages: Vec<PackageUpdate>, checked_at: u64) -> NodeUpdates {
    let truncated = packages.len() > MAX_PACKAGES;
    packages.truncate(MAX_PACKAGES);
    NodeUpdates {
        checked_at: Some(checked_at),
        packages,
        truncated,
        error: None,
    }
}

/// 비지 않은 마지막 `n`줄
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 업그레이드 Task `task`의 일시 유닛 이름(Task id 전체). opld가 다시 시작해도 Task id로 다시 만든다.
fn upgrade_unit(task: Uuid) -> String {
    format!("opl-upgrade-{}", task.simple())
}

/// 업그레이드 유닛이 돌리는 셸 명령. 처음에 시작 줄([`START_MARK`]), 끝에 종료 상태 줄([`EXIT_MARK`])을
/// 로그에 남긴다. apt는 다른 apt의 잠금을 [`LOCK_TIMEOUT`]초까지 기다린다.
fn upgrade_script(apt_get: &Path) -> String {
    let apt = sh_quote(&apt_get.display().to_string());
    let lock = format!("-o DPkg::Lock::Timeout={LOCK_TIMEOUT}");
    format!(
        "echo \"{START_MARK}\"; {apt} update -q {lock} && {apt} -y -q {lock} \
         -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
         s=$?; echo \"{EXIT_MARK}$s\"; exit $s"
    )
}

/// 셸의 작은따옴표 인용
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 업그레이드 유닛의 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitState {
    Running,
    Succeeded,
    Failed,
    /// 유닛이 없다(끝나고 치워졌거나 노드가 다시 켜졌다).
    Gone,
}

/// `systemctl show -p ActiveState -p Result -p LoadState`의 `KEY=VALUE` 줄
fn unit_state(show: &str) -> UnitState {
    let value = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
    };
    let result = value("Result");
    match value("ActiveState") {
        _ if value("LoadState") == "not-found" => UnitState::Gone,
        "failed" => UnitState::Failed,
        "inactive" if result.is_empty() => UnitState::Gone,
        "inactive" if result == "success" => UnitState::Succeeded,
        "inactive" => UnitState::Failed,
        _ => UnitState::Running,
    }
}

/// 로그 끝의 종료 상태 줄([`EXIT_MARK`])에 적힌 값
fn exit_status(log: &str) -> Option<i32> {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix(EXIT_MARK))?
        .trim()
        .parse()
        .ok()
}

/// 업그레이드 로그(없으면 빈 문자열)
async fn read_log(log: &Path) -> String {
    tokio::fs::read(log)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// 실패한 업그레이드의 오류: 로그 끝 줄
async fn upgrade_failed(log: &Path) -> String {
    format!(
        "the upgrade failed; last log lines:\n{}",
        tail(&read_log(log).await, LOG_LINES)
    )
}

/// `apt-get -s dist-upgrade` 출력의 `Inst <pkg> [<cur>] (<new> <suite…> [<arch>])` 줄. `[<cur>]`가
/// 없으면 새로 설치할 패키지다. `Conf`·`Remv`와 모양이 다른 줄은 건너뛴다.
pub(crate) fn parse_simulate(text: &str) -> Vec<PackageUpdate> {
    text.lines().filter_map(parse_inst).collect()
}

fn parse_inst(line: &str) -> Option<PackageUpdate> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    let mut rest = rest.trim_start();
    let mut current = None;
    if let Some(r) = rest.strip_prefix('[') {
        let (cur, r) = r.split_once(']')?;
        current = Some(cur.to_string());
        rest = r.trim_start();
    }
    let candidate = rest.strip_prefix('(')?.split_whitespace().next()?;
    Some(PackageUpdate {
        name: name.to_string(),
        current,
        candidate: candidate.trim_end_matches(')').to_string(),
    })
}

/// opLite 저장소의 deb822 소스 파일 내용
pub(crate) fn sources_contents(url: &str, channel: Channel, keyring: &Path) -> String {
    format!(
        "{SOURCES_MARK}\nTypes: deb\nURIs: {url}\nSuites: {SUITE}\nComponents: {}\nSigned-By: {}\n",
        channel.as_str(),
        keyring.display()
    )
}

/// [`write_sources`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcesOutcome {
    Written,
    /// 이미 같은 내용이다.
    Unchanged,
    /// 관리 표시가 없는 파일이다(관리자가 직접 관리한다). 건드리지 않았다.
    Unmanaged,
    /// 저장소 공개 키가 없다. 쓰지 않았고 opld가 쓴 파일이 있으면 지웠다.
    NoKeyring,
}

/// 소스 파일을 쓴다. 파일이 있는데 첫 줄이 [`SOURCES_MARK`]가 아니면(비었거나 공백뿐이면 opld 것으로
/// 본다) 건드리지 않고 경고한다. 공개 키 `keyring`이 없으면 apt가 서명을 확인하지 못해 `apt-get
/// update`가 실패하므로 쓰지 않고, opld가 쓴 파일은 지운다. 임시 파일(apt가 무시하는 `.dpkg-tmp`)에
/// 쓰고 디스크에 내린 뒤 바꿔 넣어 apt가 반쯤 쓴 파일을 읽지 않게 한다.
pub(crate) fn write_sources(
    path: &Path,
    url: &str,
    channel: Channel,
    keyring: &Path,
) -> std::io::Result<SourcesOutcome> {
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old
        && !old.trim().is_empty()
        && old.lines().next() != Some(SOURCES_MARK)
    {
        tracing::warn!(
            file = %path.display(),
            "the APT sources file has no opld mark; leaving it as it is"
        );
        return Ok(SourcesOutcome::Unmanaged);
    }
    if !keyring.exists() {
        tracing::warn!(
            keyring = %keyring.display(),
            "the opLite archive key is missing; not writing the APT sources"
        );
        if old.is_some() {
            std::fs::remove_file(path)?;
        }
        return Ok(SourcesOutcome::NoKeyring);
    }
    let contents = sources_contents(url, channel, keyring);
    if old.as_deref() == Some(contents.as_str()) {
        return Ok(SourcesOutcome::Unchanged);
    }
    // 누구나 읽게 한다.
    replace_file(path, &contents, 0o644)?;
    Ok(SourcesOutcome::Written)
}

/// 파일을 임시 파일(apt가 무시하는 `.dpkg-tmp`)에 권한 `mode`로 쓰고 디스크에 내린 뒤 바꿔 넣는다.
/// apt가 반쯤 쓴 파일을 읽지 않는다.
fn replace_file(path: &Path, contents: &str, mode: u32) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".dpkg-tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        // 남은 임시 파일이나 umask와 상관없이 권한을 맞춘다.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // 바꿔 넣은 것도 디스크에 내린다. 실패해도 파일은 이미 바뀌었다.
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// 저장소 주소의 `호스트[:포트]`(apt 인증 파일의 `machine`). 모양이 이상하면 None이다.
fn apt_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    (!host.is_empty() && !host.contains(char::is_whitespace)).then_some(host)
}

/// 설정 `spec`에 맞는 인증 파일 내용. enterprise 채널이고 저장된 구독 키의 서명이 맞을 때만 있다(만료는
/// 보지 않는다 — 저장소가 거절하면 업데이트 확인 오류에 보인다). 로그인은 구독 id, 비밀번호는 키
/// 문자열의 SHA-256 16진수다.
pub(crate) fn auth_contents(url: &str, spec: &SettingsSpec) -> Option<String> {
    if url.is_empty() || spec.updates.channel != Channel::Enterprise {
        return None;
    }
    let key = spec.subscription.as_deref()?;
    let license = match subscription::verify(key, &subscription::license_keys()) {
        Ok(license) => license,
        Err(e) => {
            tracing::warn!(error = %e, "cannot use the subscription key for the APT credentials");
            return None;
        }
    };
    let Some(host) = apt_host(url) else {
        tracing::warn!(
            url,
            "cannot find the host in the APT URL; not writing the APT credentials"
        );
        return None;
    };
    // 키 검사가 id를 이미 막지만, 한 줄 형식이 깨지지 않게 다시 확인한다.
    if !auth_field_ok(host) || !auth_field_ok(&license.id) {
        tracing::warn!(
            url,
            id = license.id,
            "the APT host or subscription id has spaces or control characters; not writing the APT credentials"
        );
        return None;
    }
    Some(format!(
        "{SOURCES_MARK}\nmachine {host} login {} password {}\n",
        license.id,
        hex::encode(Sha256::digest(key.as_bytes()))
    ))
}

/// 인증 파일(netrc 모양)의 한 토큰으로 쓸 수 있는 값: 비어 있지 않고 공백·제어 문자가 없다.
fn auth_field_ok(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// [`write_auth`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthOutcome {
    Written,
    /// 이미 같은 내용이다(쓸 것이 없으면 파일도 없다).
    Unchanged,
    /// opld가 쓴 파일을 지웠다.
    Removed,
    /// 관리 표시가 없는 파일이다(관리자가 직접 관리한다). 건드리지 않았다.
    Unmanaged,
}

/// 인증 파일을 `contents`로 맞춘다(0600). None이면 opld가 쓴 파일을 지운다. 관리 표시가 없는 파일은
/// [`write_sources`]처럼 건드리지 않고 경고한다.
pub(crate) fn write_auth(path: &Path, contents: Option<&str>) -> std::io::Result<AuthOutcome> {
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old
        && !old.trim().is_empty()
        && old.lines().next() != Some(SOURCES_MARK)
    {
        tracing::warn!(
            file = %path.display(),
            "the APT credentials file has no opld mark; leaving it as it is"
        );
        return Ok(AuthOutcome::Unmanaged);
    }
    let Some(contents) = contents else {
        if old.is_none() {
            return Ok(AuthOutcome::Unchanged);
        }
        std::fs::remove_file(path)?;
        return Ok(AuthOutcome::Removed);
    };
    if old.as_deref() == Some(contents) {
        return Ok(AuthOutcome::Unchanged);
    }
    // 키에서 나온 비밀번호를 담으므로 root만 읽는다.
    replace_file(path, contents, 0o600)?;
    Ok(AuthOutcome::Written)
}

/// 노드 `node`의 `status.updates`와 이 노드의 버전을 쓴다.
async fn record(cluster: &Cluster, node: Uuid, updates: &NodeUpdates) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        status.updates = Some(updates.clone());
        status.version = Some(BUILD.into());
        true
    })
    .await
}

/// 이 노드 status의 `version`과 `updates`가 비었으면(이 필드를 모르는 옛 opld가 리더일 때 status를
/// 다시 쓰며 지웠다) 다시 쓴다. 확인 결과는 남긴다.
async fn restore(cluster: &Cluster, node: Uuid) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        let stale = status.version.as_deref() != Some(BUILD) || status.updates.is_none();
        status.version = Some(BUILD.into());
        status.updates.get_or_insert_with(NodeUpdates::default);
        stale
    })
    .await
}

/// 노드 `node`의 status를 `change`로 고쳐 쓴다(false면 쓰지 않는다). status의 다른 필드는 opld도
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn change_status(
    cluster: &Cluster,
    node: Uuid,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<NodeSpec>(node)? else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(node, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 소스 파일을 시작할 때와 설정이 바뀔 때 맞추고, `first_check` 뒤 처음, 그 뒤 `check_interval`마다
/// 업데이트를 확인하는 에이전트. 업데이트가 켜진 노드에서 돈다.
pub struct UpdateAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl UpdateAgent {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, updates: Arc<Updates>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_agent(cluster, node, updates, rx)),
        }
    }

    /// 멈춘다. 도는 확인은 기다리지 않는다(apt 명령은 혼자 끝난다).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_agent(
    cluster: Arc<Cluster>,
    node: Uuid,
    updates: Arc<Updates>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut next = Instant::now() + updates.config.first_check;
    let mut watch = None;
    let mut sync = true;
    let mut started = false;
    if let Err(e) = restore(&cluster, node).await {
        tracing::warn!(error = %e, "cannot restore the node status");
    }
    loop {
        if sync {
            // 이번에 읽을 설정 뒤의 변경을 놓치지 않게 먼저 구독한다.
            if watch.is_none() {
                watch = cluster
                    .list::<SettingsSpec>()
                    .ok()
                    .and_then(|(revision, _)| cluster.watch(revision).ok());
            }
            // 채널을 바꿔 소스 파일을 새로 썼으면 곧 확인해 새 채널의 업데이트를 보인다.
            if updates.sync_sources(&cluster) && started {
                next = Instant::now();
            }
            sync = false;
            started = true;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = tokio::time::sleep_until(next) => {
                // 업그레이드 중에는 apt 잠금을 다투지 않는다. 업그레이드가 끝나면 스스로 확인한다.
                if upgrading(&cluster, node) {
                    tracing::debug!("skipping the update check during an upgrade");
                } else {
                    tokio::select! {
                        _ = &mut shutdown => return,
                        result = updates.refresh(&cluster, node) => {
                            if let Err(e) = result {
                                tracing::warn!(error = %e, "update check failed");
                            }
                        }
                    }
                }
                next = Instant::now() + updates.config.check_interval;
            }
            () = settings_changed(&mut watch) => sync = true,
        }
    }
}

/// 노드 `node`에서 업그레이드 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn upgrading(cluster: &Cluster, node: Uuid) -> bool {
    let Ok((_, tasks)) = cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node && !t.status.state.is_finished()
    })
}

/// 업그레이드 Task `task` 말고 running인 업그레이드가 있으면 그 노드 이름(없는 노드면 id). 두 요청이
/// 함께 지나가 두 Task가 생겨도 running 쓰기는 Raft 순서대로 적용되므로 나중 것이 앞의 것을 본다.
fn other_upgrade(cluster: &Cluster, task: Uuid) -> Option<String> {
    let (_, tasks) = cluster.list::<TaskSpec>().ok()?;
    let other = tasks.into_iter().find(|t| {
        t.id != task && t.spec.op == TaskOp::NodeUpgrade && t.status.state == TaskState::Running
    })?;
    Some(match cluster.get::<NodeSpec>(other.spec.node) {
        Ok(Some(node)) => node.name,
        _ => other.spec.node.to_string(),
    })
}

/// 설정이 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 다시
/// 맞추게 한다. 구독이 없으면 잠시 뒤 돌아가 다시 구독하게 한다.
async fn settings_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Settings,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Settings,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

/// 업데이트를 확인하지 않는 노드(위트니스, 업데이트가 꺼진 노드, 옛 노드)면 400 `updates_unsupported`
pub(crate) fn check_updates_supported(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness || node.status.updates.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "updates_unsupported",
            format!("node {:?} does not check for updates", node.name),
        ));
    }
    Ok(())
}

/// 노드의 업데이트를 다시 확인할 Task(`node-update-check`, 그 노드에서 돈다)를 돌려준다(202). 결과는
/// Node `status.updates`에 쓴다. 그 노드에 끝나지 않은 확인 Task가 있으면 새로 만들지 않고 그것을
/// 돌려준다. 업데이트를 확인하지 않는 노드면 400 `updates_unsupported`, 오프라인이면 409 `node_offline`,
/// 옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다), 그 노드를
/// 업그레이드하는 중이면 409 `upgrade_in_progress`.
#[utoipa::path(post, path = "/nodes/{key}/updates/refresh", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_updates(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    // 업그레이드가 apt 잠금을 잡고 있다. 끝나면 실행기가 스스로 확인한다.
    if tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is being upgraded; it checks for updates when the upgrade ends",
                node.name
            ),
        ));
    }
    if let Some(task) = tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::NodeUpdateCheck
            && t.spec.node == node.id
            && !t.status.state.is_finished()
    }) {
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpdateCheck, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드의 패키지를 올릴 Task(`node-upgrade`, 그 노드에서 돈다)를 돌려준다(202). apt는 opld 밖의 일시
/// 유닛에서 돌므로 VM은 계속 돌고 opld만 잠깐 다시 시작한다. 업데이트를 확인하지 않는 노드면 400
/// `updates_unsupported`, 오프라인이면 409 `node_offline`, 클러스터 어디든 끝나지 않은 업그레이드가
/// 있으면 409 `upgrade_in_progress`(한 번에 한 노드 — 나머지 노드가 과반수와 서비스를 지킨다), 옛 노드가
/// 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/upgrade", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upgrade_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    // 이 프로세스에 함께 온 요청이 둘 다 검사를 지나 업그레이드 둘을 만들지 않게 한다. 다른 노드로 온
    // 요청과의 경합은 실행기가 막는다(`other_upgrade`).
    static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REQUESTS.lock().await;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(task) = tasks
        .iter()
        .find(|t| t.spec.op == TaskOp::NodeUpgrade && !t.status.state.is_finished())
    {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is still being upgraded; upgrade one node at a time",
                node_name(&state, task.spec.node)?
            ),
        ));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpgrade, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debian 13에서 `LANG=C apt-get -s -q dist-upgrade`가 내는 모양(보안 업데이트, 새 커널 의존성,
    /// 지울 패키지, opLite 저장소 패키지가 섞임)
    const SIMULATE: &str = "\
Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following packages will be REMOVED:
  libfoo1
The following NEW packages will be installed:
  linux-image-6.12.48+deb13-amd64
The following packages will be upgraded:
  base-files libc-bin libc6 linux-image-amd64 oplite
5 upgraded, 1 newly installed, 1 to remove and 0 not upgraded.
Remv libfoo1 [1.2-3]
Inst base-files [13.8+deb13u1] (13.8+deb13u2 Debian:13.2/stable [amd64])
Conf base-files (13.8+deb13u2 Debian:13.2/stable [amd64])
Inst libc6 [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64]) [libc-bin:amd64 ]
Inst libc-bin [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc6 (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc-bin (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Inst linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Inst linux-image-amd64 [6.12.41-1] (6.12.48-1 Debian:13.2/stable [amd64])
Inst oplite [0.2.0] (1:0.2.1~rc1 opLite:trixie [amd64])
Conf linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf linux-image-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf oplite (1:0.2.1~rc1 opLite:trixie [amd64])
";

    fn pkg(name: &str, current: Option<&str>, candidate: &str) -> PackageUpdate {
        PackageUpdate {
            name: name.into(),
            current: current.map(Into::into),
            candidate: candidate.into(),
        }
    }

    #[test]
    fn parse_simulate_reads_inst_lines() {
        assert_eq!(
            parse_simulate(SIMULATE),
            [
                pkg("base-files", Some("13.8+deb13u1"), "13.8+deb13u2"),
                pkg("libc6", Some("2.41-12"), "2.41-12+deb13u1"),
                pkg("libc-bin", Some("2.41-12"), "2.41-12+deb13u1"),
                // 새 커널 의존성: 설치된 버전이 없다.
                pkg("linux-image-6.12.48+deb13-amd64", None, "6.12.48-1"),
                pkg("linux-image-amd64", Some("6.12.41-1"), "6.12.48-1"),
                pkg("oplite", Some("0.2.0"), "1:0.2.1~rc1"),
            ]
        );
        assert!(parse_simulate("").is_empty());
        // 모양이 다른 줄은 건너뛴다.
        assert!(parse_simulate("Inst\nInst x [1.0\nInst y [1.0]\nInst z").is_empty());
    }

    #[test]
    fn found_keeps_at_most_max_packages() {
        let many: String = (0..600)
            .map(|i| format!("Inst p{i} [1.0] (1.1 Debian:13.2/stable [amd64])\n"))
            .collect();
        let packages = parse_simulate(&many);
        assert_eq!(packages.len(), 600);
        let updates = found(packages, 7);
        assert_eq!(updates.checked_at, Some(7));
        assert_eq!(updates.packages.len(), MAX_PACKAGES);
        assert_eq!(updates.packages[0].name, "p0");
        assert!(updates.truncated);
        assert_eq!(updates.error, None);
        let few = found(parse_simulate(SIMULATE), 7);
        assert_eq!(few.packages.len(), 6);
        assert!(!few.truncated);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\n\nc\nd\ne\nf\n", 5), "b\nc\nd\ne\nf");
        assert_eq!(tail("only\n", 5), "only");
        assert_eq!(tail("", 5), "");
    }

    fn missing_tools() -> Updates {
        let dir = PathBuf::from("/nonexistent/opl-updates");
        Updates::new(UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: dir.join("oplite.sources"),
            keyring: dir.join("keyring.gpg"),
            auth_file: dir.join("oplite.conf"),
            apt_get: dir.join("apt-get"),
            systemd_run: dir.join("systemd-run"),
            systemctl: dir.join("systemctl"),
            dpkg: dir.join("dpkg"),
            log_dir: dir.join("log"),
            poll: Duration::from_millis(20),
        })
    }

    #[tokio::test]
    async fn check_reports_a_missing_apt_get_and_does_not_wait_for_another_check() {
        let updates = missing_tools();
        let got = updates.check().await;
        assert_eq!(
            got.error.as_deref(),
            Some(
                "cannot run /nonexistent/opl-updates/apt-get: No such file or directory (os error 2)"
            )
        );
        assert!(got.checked_at.is_some());
        let _running = updates.lock.try_lock().unwrap();
        let got = updates.check().await;
        assert_eq!(got.error.as_deref(), Some(CHECK_BUSY));
        assert!(got.packages.is_empty());
    }

    /// 사용자가 요청한 확인은 도는 확인을 기다렸다가 확인한다.
    #[tokio::test]
    async fn a_waiting_check_waits_for_the_running_one() {
        let updates = missing_tools();
        let running = updates.lock.try_lock().unwrap();
        let waiting = updates.check_waiting(|| false);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        drop(running);
        let got = waiting.await.unwrap();
        assert!(got.error.unwrap().starts_with("cannot run "));
    }

    /// 업그레이드가 잠금을 잡았으면 기다리지 않고 확인하지 않는다.
    #[tokio::test]
    async fn a_waiting_check_gives_up_during_an_upgrade() {
        let updates = missing_tools();
        let _upgrading = updates.lock.try_lock().unwrap();
        assert!(updates.check_waiting(|| true).await.is_none());
    }

    #[test]
    fn unit_state_reads_systemctl_show() {
        let show = |active: &str, result: &str, load: &str| {
            unit_state(&format!(
                "ActiveState={active}\nResult={result}\nLoadState={load}\n"
            ))
        };
        assert_eq!(show("active", "success", "loaded"), UnitState::Running);
        assert_eq!(show("activating", "success", "loaded"), UnitState::Running);
        assert_eq!(
            show("deactivating", "success", "loaded"),
            UnitState::Running
        );
        assert_eq!(show("inactive", "success", "loaded"), UnitState::Succeeded);
        assert_eq!(show("failed", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "", "loaded"), UnitState::Gone);
        assert_eq!(show("inactive", "success", "not-found"), UnitState::Gone);
        // 줄 순서는 상관없다(systemctl은 자기 순서로 낸다).
        assert_eq!(
            unit_state("LoadState=loaded\nResult=success\nActiveState=inactive\n"),
            UnitState::Succeeded
        );
        // 알 수 없는 상태는 계속 지켜본다.
        assert_eq!(unit_state(""), UnitState::Running);
        assert_eq!(show("reloading", "success", "loaded"), UnitState::Running);
    }

    #[test]
    fn exit_status_reads_the_last_mark() {
        assert_eq!(
            exit_status("Setting up oplite (0.2.1) ...\nopl-upgrade: exit 0\n"),
            Some(0)
        );
        assert_eq!(
            exit_status("opl-upgrade: exit 0\nopl-upgrade: exit 100\n"),
            Some(100)
        );
        assert_eq!(exit_status("Unpacking oplite (0.2.1) ...\n"), None);
        assert_eq!(exit_status(""), None);
    }

    #[test]
    fn upgrade_unit_and_script() {
        let id: Uuid = "0193a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".parse().unwrap();
        assert_eq!(
            upgrade_unit(id),
            "opl-upgrade-0193a1b2c3d47e5f8a9b0c1d2e3f4a5b"
        );
        assert_eq!(
            upgrade_script(Path::new("/usr/bin/apt-get")),
            "echo \"opl-upgrade: start\"; \
             '/usr/bin/apt-get' update -q -o DPkg::Lock::Timeout=600 && \
             '/usr/bin/apt-get' -y -q -o DPkg::Lock::Timeout=600 \
             -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
             s=$?; echo \"opl-upgrade: exit $s\"; exit $s"
        );
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    /// 셸이 정말 돌면 로그는 시작 줄로 시작해 apt의 종료 상태 줄로 끝난다(따옴표가 든 경로도).
    #[test]
    fn upgrade_script_runs_in_sh() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("it's tools");
        std::fs::create_dir(&tools).unwrap();
        let apt_get = tools.join("apt-get");
        let run = |exit: u8| {
            std::fs::write(
                &apt_get,
                format!("#!/bin/sh\necho \"apt-get $*\"\nexit {exit}\n"),
            )
            .unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("chmod 755 \"$1\" && {}", upgrade_script(&apt_get)),
                    "sh",
                ])
                .arg(&apt_get)
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8(output.stdout).unwrap(),
            )
        };
        let (code, log) = run(0);
        assert_eq!(code, Some(0));
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines[0], START_MARK);
        assert_eq!(lines[1], "apt-get update -q -o DPkg::Lock::Timeout=600");
        assert!(lines[2].ends_with(" dist-upgrade"), "{log}");
        assert_eq!(lines[3], "opl-upgrade: exit 0");
        assert_eq!(exit_status(&log), Some(0));
        let (code, log) = run(100);
        assert_eq!(code, Some(100));
        assert_eq!(log.lines().last(), Some("opl-upgrade: exit 100"));
        assert_eq!(exit_status(&log), Some(100));
    }

    #[test]
    fn sources_are_deb822_with_the_mark() {
        assert_eq!(
            sources_contents(
                "https://apt.example.com/oplite",
                Channel::Testing,
                Path::new(KEYRING)
            ),
            "# managed by opld; edits are overwritten\n\
             Types: deb\n\
             URIs: https://apt.example.com/oplite\n\
             Suites: trixie\n\
             Components: testing\n\
             Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg\n"
        );
    }

    #[test]
    fn write_sources_writes_replaces_and_leaves_unmanaged_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        std::fs::write(&keyring, "").unwrap();
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sources_contents(url, Channel::Stable, &keyring)
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unchanged
        );
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Components: testing\n")
        );
        // 관리 표시가 없는 파일(관리자가 직접 쓴 것)은 그대로 둔다.
        let own =
            "Types: deb\nURIs: http://mirror.lan/oplite\nSuites: trixie\nComponents: stable\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
        // 비었거나 공백뿐인 파일은 opld 것으로 본다.
        for empty in ["", " \n\n"] {
            std::fs::write(&path, empty).unwrap();
            assert_eq!(
                write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
                SourcesOutcome::Written
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                sources_contents(url, Channel::Stable, &keyring)
            );
        }
        assert!(!dir.path().join("oplite.sources.dpkg-tmp").exists());
    }

    /// 공개 키가 없으면 쓰지 않고, opld가 쓴 파일은 지우고, 관리자의 파일은 그대로 둔다.
    #[test]
    fn write_sources_needs_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        std::fs::write(&path, sources_contents(url, Channel::Stable, &keyring)).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        let own = "Types: deb\nURIs: http://mirror.lan/oplite\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
    }

    /// 개발 키로 서명한 구독 키(`expires_at`에 만료)
    fn dev_key(expires_at: u64) -> String {
        use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
        sign(
            &decode_private_key(include_str!(
                "../../opl-types/tests/fixtures/dev-license.key"
            ))
            .unwrap(),
            &LicensePayload {
                id: "sub-1".into(),
                customer: "Example Corp".into(),
                level: Level::Standard,
                nodes: 3,
                issued_at: 1_700_000_000,
                expires_at,
            },
        )
        .unwrap()
    }

    fn spec(channel: Channel, key: Option<&str>) -> SettingsSpec {
        SettingsSpec {
            updates: opl_types::settings::UpdateSettings { channel },
            subscription: key.map(Into::into),
        }
    }

    #[test]
    fn sources_name_the_enterprise_channel() {
        assert!(
            sources_contents(
                "https://apt.example.com",
                Channel::Enterprise,
                Path::new(KEYRING)
            )
            .contains("Components: enterprise\n")
        );
    }

    /// enterprise 채널이고 서명이 맞는 키가 있을 때만 쓴다. 만료된 키도 쓴다(저장소가 판단한다).
    #[test]
    fn auth_contents_need_the_enterprise_channel_and_a_signed_key() {
        let key = dev_key(1_700_000_000 + 86_400);
        let url = "https://user@apt.example.com:8443/oplite?x#y";
        let password = hex::encode(Sha256::digest(key.as_bytes()));
        assert_eq!(
            auth_contents(url, &spec(Channel::Enterprise, Some(&key))).unwrap(),
            format!(
                "# managed by opld; edits are overwritten\n\
                 machine apt.example.com:8443 login sub-1 password {password}\n"
            )
        );
        assert_eq!(
            auth_contents(
                "http://apt.example.com",
                &spec(Channel::Enterprise, Some(&key))
            )
            .unwrap()
            .lines()
            .nth(1)
            .unwrap(),
            format!("machine apt.example.com login sub-1 password {password}")
        );
        assert_eq!(auth_contents(url, &spec(Channel::Stable, Some(&key))), None);
        assert_eq!(
            auth_contents(url, &spec(Channel::Testing, Some(&key))),
            None
        );
        assert_eq!(auth_contents(url, &spec(Channel::Enterprise, None)), None);
        let tampered = format!("{key}x");
        assert_eq!(
            auth_contents(url, &spec(Channel::Enterprise, Some(&tampered))),
            None
        );
        for bad in [
            "",
            "apt.example.com",
            "https://",
            "https:///oplite",
            "https://apt.exam\u{7}ple.com",
        ] {
            assert_eq!(
                auth_contents(bad, &spec(Channel::Enterprise, Some(&key))),
                None,
                "{bad}"
            );
        }
    }

    /// 인증 파일 한 줄에 들어가는 값은 공백·제어 문자가 없어야 한다(키 검사를 지나도 두 번째 방어).
    #[test]
    fn auth_fields_must_be_single_tokens() {
        assert!(auth_field_ok("sub-1"));
        assert!(auth_field_ok("apt.example.com:8443"));
        for bad in ["", "Acme 2026", "a\nb", "a\tb", "a\u{0}b", "a\u{85}b"] {
            assert!(!auth_field_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn write_auth_writes_0600_replaces_removes_and_leaves_unmanaged_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.conf");
        let a = "# managed by opld; edits are overwritten\nmachine h login a password 1\n";
        let b = "# managed by opld; edits are overwritten\nmachine h login b password 2\n";
        assert_eq!(write_auth(&path, None).unwrap(), AuthOutcome::Unchanged);
        assert!(!path.exists());
        assert_eq!(write_auth(&path, Some(a)).unwrap(), AuthOutcome::Written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), a);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(write_auth(&path, Some(a)).unwrap(), AuthOutcome::Unchanged);
        assert_eq!(write_auth(&path, Some(b)).unwrap(), AuthOutcome::Written);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), b);
        assert_eq!(write_auth(&path, None).unwrap(), AuthOutcome::Removed);
        assert!(!path.exists());
        assert!(!dir.path().join("oplite.conf.dpkg-tmp").exists());
        // 관리 표시가 없는 파일(관리자가 직접 쓴 것)은 쓰지도 지우지도 않는다.
        let own = "machine mirror.lan login me password secret\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(write_auth(&path, Some(a)).unwrap(), AuthOutcome::Unmanaged);
        assert_eq!(write_auth(&path, None).unwrap(), AuthOutcome::Unmanaged);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
    }
}
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust
            keyring: self.path("keyring.gpg"),
            apt_get: self.path("apt-get"),
```

을 다음으로 바꾼다:

```rust
            keyring: self.path("keyring.gpg"),
            auth_file: self.path("oplite.conf"),
            apt_get: self.path("apt-get"),
```

`crates/opl-api/tests/settings.rs`에서

```rust
                "/settings",
                json!({ "updates": { "channel": "enterprise" } }),
                None,
```

을 다음으로 바꾼다:

```rust
                "/settings",
                json!({ "updates": { "channel": "nightly" } }),
                None,
```

`crates/opl-api/tests/subscription.rs`에서

```rust
//! 구독 API: 넣기·지우기, 권한, 상태 계산(노드 수, 위트니스 제외), 키를 내보내지 않음.
```

을 다음으로 바꾼다:

```rust
//! 구독 API: 넣기·지우기, 권한, 상태 계산(노드 수, 위트니스 제외), 키를 내보내지 않음, enterprise 채널 조건.
```

`crates/opl-api/tests/subscription.rs`에서

```rust
    assert!(!ev.to_string().contains(&k));
}
```

을 다음으로 바꾼다:

```rust
    assert!(!ev.to_string().contains(&k));
}

fn stored_channel(env: &Env) -> Option<String> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .map(|s| s.spec.updates.channel.as_str().to_string())
}

/// enterprise 채널은 구독이 active일 때만 고른다. 이미 enterprise여도 다시 보내면 같이 검사한다.
#[tokio::test]
async fn the_enterprise_channel_needs_an_active_subscription() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let enterprise = json!({ "updates": { "channel": "enterprise" } });
    let got = admin.patch("/settings", enterprise.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
    assert_eq!(
        got.1["message"],
        "an active subscription is required for the enterprise channel"
    );
    assert_eq!(stored_channel(&env), None);

    let k = key(1, 30);
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": k }), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, got) = admin.patch("/settings", enterprise.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"],
        json!({ "updates": { "channel": "enterprise" } })
    );
    assert_eq!(stored_channel(&env).as_deref(), Some("enterprise"));

    // 노드가 한도를 넘으면(over_limit) 다시 enterprise를 보내도 거절한다. 다른 채널은 고른다.
    add_node(&env, "node2", false, Some(BUILD)).await;
    let got = admin.patch("/settings", enterprise.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");

    // 구독을 지워도 채널은 그대로 두지만, enterprise로는 바꾸지 못한다.
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key(2, 30) }), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        admin.patch("/settings", enterprise.clone(), None).await.0,
        StatusCode::OK
    );
    assert_eq!(admin.delete("/subscription").await.0, StatusCode::OK);
    assert_eq!(stored_channel(&env).as_deref(), Some("enterprise"));
    let got = admin.patch("/settings", enterprise, None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "subscription_required");
}

/// 이 빌드보다 낮은 노드가 있으면 구독이 있어도 enterprise를 쓰지 않는다(옛 노드는 풀지 못한다).
/// 구독을 넣은 뒤 옛 노드가 생기는 이 조합은 합류 버전 검사 때문에 실제로는 생기지 않는 인위적
/// 상황이다. 여기서는 PATCH의 `require_current` 게이트만 확인한다.
#[tokio::test]
async fn the_enterprise_channel_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key(3, 30) }), None)
            .await
            .0,
        StatusCode::OK
    );
    add_node(&env, "old", true, Some("0.0.1~old")).await;
    let got = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "enterprise" } }),
            None,
        )
        .await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert_eq!(stored_channel(&env).as_deref(), Some("stable"));
    // 옛 노드가 푸는 채널은 그대로 고른다.
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
}
```

`crates/opl-api/tests/updates.rs`에서

```rust
        (apt.calls().len() == 4).then_some(())
    })
```

을 다음으로 바꾼다:

```rust
        (apt.calls().len() == 4).then_some(())
    })
    .await;
    agent.shutdown().await;
}

/// 개발 키로 서명한 구독 키(노드 3대, 30일)
fn subscription_key() -> String {
    use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sign(
        &decode_private_key(include_str!(
            "../../opl-types/tests/fixtures/dev-license.key"
        ))
        .unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now - 60,
            expires_at: now + 30 * 86_400,
        },
    )
    .unwrap()
}

/// enterprise 채널이고 구독이 있으면 APT 인증 파일을 쓰고, 구독을 지우거나 채널을 바꾸면 지운다.
#[tokio::test]
async fn the_agent_writes_the_apt_credentials_for_the_enterprise_channel() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    let apt = FakeApt::new();
    let env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    let admin = env.admin().await;
    let mut config = apt.config();
    config.apt_url = "https://apt.example.com:8443/oplite".into();
    let sources = config.sources_file.clone();
    let auth = config.auth_file.clone();
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    let read_sources = || std::fs::read_to_string(&sources).ok();
    let read_auth = || std::fs::read_to_string(&auth).ok();
    eventually("the sources file", read_sources).await;
    assert_eq!(read_auth(), None);

    let k = subscription_key();
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "enterprise" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    eventually("the enterprise channel", || {
        read_sources().filter(|t| t.contains("Components: enterprise\n"))
    })
    .await;
    let expected = format!(
        "# managed by opld; edits are overwritten\n\
         machine apt.example.com:8443 login sub-1 password {}\n",
        hex::encode(Sha256::digest(k.as_bytes()))
    );
    let text = eventually("the APT credentials", read_auth).await;
    assert_eq!(text, expected);
    let mode = std::fs::metadata(&auth).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // 구독을 지우면 지우고(채널은 그대로), 다시 넣으면 다시 쓴다.
    assert_eq!(admin.delete("/subscription").await.0, StatusCode::OK);
    eventually("the removed credentials", || {
        read_auth().is_none().then_some(())
    })
    .await;
    let (status, got) = admin.put("/subscription", json!({ "key": k }), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        eventually("the credentials again", read_auth).await,
        expected
    );

    // 다른 채널로 바꾸면 지운다.
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "stable" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    eventually("the stable channel", || {
        read_sources().filter(|t| t.contains("Components: stable\n"))
    })
    .await;
    eventually("the removed credentials", || {
        read_auth().is_none().then_some(())
    })
```

`crates/opl-types/src/settings.rs`에서

```rust
    Testing,
}
```

을 다음으로 바꾼다:

```rust
    Testing,
    /// 구독한 클러스터용 릴리스. 고르려면 구독이 active여야 하고, 저장소는 구독 키로 인증한다.
    Enterprise,
}
```

`crates/opl-types/src/settings.rs`에서

```rust
            Channel::Testing => "testing",
        }
```

을 다음으로 바꾼다:

```rust
            Channel::Testing => "testing",
            Channel::Enterprise => "enterprise",
        }
```

`crates/opl-types/src/settings.rs`에서

```rust
    fn channels_are_lowercase() {
        for channel in [Channel::Stable, Channel::Testing] {
            assert_eq!(serde_json::to_value(channel).unwrap(), channel.as_str());
```

을 다음으로 바꾼다:

```rust
    fn channels_are_lowercase() {
        for channel in [Channel::Stable, Channel::Testing, Channel::Enterprise] {
            assert_eq!(serde_json::to_value(channel).unwrap(), channel.as_str());
```

`crates/opl-types/src/settings.rs`에서

```rust
        assert_eq!(spec.updates.channel, Channel::Testing);
        assert!(serde_json::from_str::<Channel>(r#""Testing""#).is_err());
        assert!(serde_json::from_str::<Channel>(r#""enterprise""#).is_err());
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(spec.updates.channel, Channel::Testing);
        let spec: SettingsSpec =
            serde_json::from_str(r#"{"updates":{"channel":"enterprise"}}"#).unwrap();
        assert_eq!(spec.updates.channel, Channel::Enterprise);
        assert_eq!(Channel::Enterprise.as_str(), "enterprise");
        assert!(serde_json::from_str::<Channel>(r#""Testing""#).is_err());
        assert!(serde_json::from_str::<Channel>(r#""nightly""#).is_err());
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing)
    Set {
```

을 다음으로 바꾼다:

```rust
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
```

`crates/opld/src/config.rs`에서

```rust
            keyring: opl_api::KEYRING.into(),
            apt_get: self.apt_get.clone(),
```

을 다음으로 바꾼다:

```rust
            keyring: opl_api::KEYRING.into(),
            auth_file: opl_api::AUTH_FILE.into(),
            apt_get: self.apt_get.clone(),
```

`crates/opld/src/config.rs`에서

```rust
        assert_eq!(config.sources_file, Path::new("/tmp/x.sources"));
        assert_eq!(config.apt_get, Path::new("/opt/apt-get"));
```

을 다음으로 바꾼다:

```rust
        assert_eq!(config.sources_file, Path::new("/tmp/x.sources"));
        assert_eq!(
            config.auth_file,
            Path::new("/etc/apt/auth.conf.d/oplite.conf")
        );
        assert_eq!(config.apt_get, Path::new("/opt/apt-get"));
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1226개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: enterprise 채널과 APT 인증 파일 — Channel::Enterprise, PATCH /settings가 enterprise로 바꿀 때(이미 enterprise여도) require_current 뒤 구독이 active가 아니면 400 subscription_required, 노드는 enterprise 채널이고 저장된 키의 서명이 맞으면 /etc/apt/auth.conf.d/oplite.conf(UpdatesConfig.auth_file, 상수 AUTH_FILE)에 machine <apt_url의 호스트[:포트]> login <구독 id> password <키 SHA-256>을 0600으로 쓰고(.dpkg-tmp에 써서 바꿔 넣음, 같으면 쓰지 않음) 아니면 opld가 쓴 파일만 지움(관리 표시 없는 파일은 그대로), 설정 watch 이벤트에 소스 파일과 함께 맞춤, 인증 파일을 새로 쓰면 곧 업데이트 확인, schema.d.ts"
```

---

### Task 4: 텔레메트리 설정·미리 보기와 oplctl telemetry

`Settings.spec.telemetry{enabled, endpoint}`와 `status.telemetry{id, last_sent_at, last_error}`(기본값이면 직렬화하지 않음), 빌드 값 `OPL_TELEMETRY_URL`. `PATCH /settings`의 `telemetry`(https 끝점만, 사용자 정보 금지, 끝점이 없으면 400 `telemetry_endpoint_required`, 처음 켜면 무작위 id), `POST /api/v1/telemetry/reset-id`, 보낼 내용 `TelemetryReport`(수만 — 이름·주소·MAC·고객·키 없음)와 `GET /api/v1/telemetry/preview`(admin). oplctl `telemetry show|enable [--endpoint]|disable|preview|reset-id`.

**Files:**
- Create: `crates/opl-api/src/telemetry.rs`
- Create: `crates/opl-api/tests/telemetry.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/settings.rs`
- Modify: `crates/opl-api/src/updates.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/settings.rs`
- Modify: `crates/opl-types/src/settings.rs`
- Modify: `crates/opl-types/src/version.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
mod tasks;
mod updates;
```

을 다음으로 바꾼다:

```rust
mod tasks;
mod telemetry;
mod updates;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(settings::reset_telemetry_id))
        .routes(routes!(telemetry::preview_telemetry))
        .routes(routes!(
```

`crates/opl-api/src/settings.rs` 전체:

```rust
//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.
//! 구독 키 문자열은 API로 내보내지 않는다([`public`]).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{
    Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus, TelemetrySettings,
};
use opl_types::subscription::SubscriptionState;
use opl_types::version::TELEMETRY_URL;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::if_match;
use crate::state::AppState;

/// 저장된 설정. 없으면 기본값을 담은 리소스(id는 nil UUID, revision 0)다.
pub fn current(cluster: &Cluster) -> Result<Settings, ClusterError> {
    Ok(cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
        .unwrap_or_else(|| Resource {
            id: Uuid::nil(),
            ..Resource::new(SETTINGS_NAME, SettingsSpec::default())
        }))
}

/// API로 내보내는 설정: 구독 키 문자열을 뺀다.
pub(crate) fn public(mut settings: Settings) -> Settings {
    settings.spec.subscription = None;
    settings
}

/// 설정에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub updates: Option<UpdateSettingsPatch>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub telemetry: Option<TelemetryPatch>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub channel: Option<Channel>,
}

/// 텔레메트리에서 바꿀 값
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TelemetryPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
    /// 보낼 `https://` 주소. `""`는 지워 빌드 기본 끝점을 쓴다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub endpoint: Option<String>,
}

/// 텔레메트리 끝점은 호스트가 있는 `https://` 주소여야 한다.
fn check_endpoint(endpoint: &str) -> Result<(), ApiError> {
    // 설정은 로그인한 누구나 읽으므로 끝점에 사용자 정보를 두지 않는다.
    let authority = endpoint.split_once("://").map_or("", |(_, rest)| {
        rest.split(['/', '?', '#']).next().unwrap_or("")
    });
    if authority.contains('@') {
        return Err(ApiError::bad_request(
            "telemetry.endpoint must not contain credentials",
        ));
    }
    let ok = endpoint.starts_with("https://")
        && endpoint.parse::<Uri>().is_ok_and(|uri| {
            uri.scheme_str() == Some("https") && uri.host().is_some_and(|h| !h.is_empty())
        });
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "telemetry.endpoint must be an https:// URL",
        ))
    }
}

/// 텔레메트리 패치를 `telemetry`에 적용한다. 켜져 있는데 끝점도 빌드 기본 끝점도 없으면 400.
fn apply_telemetry(
    telemetry: &mut TelemetrySettings,
    patch: &TelemetryPatch,
) -> Result<(), ApiError> {
    if let Some(enabled) = patch.enabled {
        telemetry.enabled = enabled;
    }
    match patch.endpoint.as_deref() {
        Some("") => telemetry.endpoint = None,
        Some(endpoint) => telemetry.endpoint = Some(endpoint.to_string()),
        None => {}
    }
    if telemetry.enabled && telemetry.endpoint.is_none() && TELEMETRY_URL.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "telemetry_endpoint_required",
            "set telemetry.endpoint; this build has no default endpoint",
        ));
    }
    Ok(())
}

/// 클러스터 설정. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    Ok(Json(public(current(&state.cluster)?)))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다. enterprise 채널은 모든 노드가 이 빌드 이상이고 구독이 active일 때만 고른다
/// (이미 enterprise여도 다시 보내면 검사한다). 텔레메트리도 모든 노드가 이 빌드 이상일 때만 바꾸고,
/// 켤 때 id가 없으면 만든다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
    params(("If-Match" = Option<u64>, Header)),
    request_body = SettingsPatch,
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Json(req): Json<SettingsPatch>,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    if req.updates.is_none() && req.telemetry.is_none() {
        return Err(ApiError::bad_request("give updates or telemetry"));
    }
    if let Some(endpoint) = req.telemetry.as_ref().and_then(|t| t.endpoint.as_deref())
        && !endpoint.is_empty()
    {
        check_endpoint(endpoint)?;
    }
    crate::cluster::require_upgraded(&state.cluster)?;
    let channel = req.updates.and_then(|u| u.channel);
    let enterprise = channel == Some(Channel::Enterprise);
    // 옛 노드는 enterprise 채널과 텔레메트리 필드를 풀지 못한다.
    if enterprise || req.telemetry.is_some() {
        crate::cluster::require_current(&state.cluster)?;
    }
    let used = if enterprise {
        crate::subscription::used_nodes(&state.cluster)?
    } else {
        0
    };
    let expected = if_match(&headers)?;
    let written = write(&state.cluster, expected, |spec| {
        if enterprise
            && crate::subscription::state_of(spec, used).state != SubscriptionState::Active
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "subscription_required",
                "an active subscription is required for the enterprise channel",
            ));
        }
        if let Some(channel) = channel {
            spec.updates.channel = channel;
        }
        if let Some(telemetry) = &req.telemetry {
            apply_telemetry(&mut spec.telemetry, telemetry)?;
        }
        Ok(())
    })
    .await?;
    let has_id = written
        .status
        .telemetry
        .as_ref()
        .is_some_and(|t| t.id.is_some());
    if !written.spec.telemetry.enabled || has_id {
        return Ok(Json(public(written)));
    }
    // 처음 켰다: 보고에 붙일 id를 만든다(다른 요청이 먼저 만들었으면 그대로).
    let written = write_status(&state.cluster, |status| {
        let telemetry = status.telemetry.get_or_insert_default();
        if telemetry.id.is_none() {
            telemetry.id = Some(Uuid::new_v4());
        }
    })
    .await?;
    Ok(Json(public(written)))
}

/// 텔레메트리 id를 새로 만든다. 설정이 아직 없으면 만든다.
#[utoipa::path(post, path = "/telemetry/reset-id", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn reset_telemetry_id(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    crate::cluster::require_current(&state.cluster)?;
    let written = write_status(&state.cluster, |status| {
        status.telemetry.get_or_insert_default().id = Some(Uuid::new_v4());
    })
    .await?;
    tracing::info!("telemetry id reset");
    Ok(Json(public(written)))
}

/// 설정 status를 읽어 `change`로 바꿔 쓴다. 설정이 아직 없으면 기본 spec으로 만든다. 읽은 뒤 다른
/// 쪽이 썼으면(revision) 다시 읽어 바꾼다. 같은 값이면 쓰지 않는다. 돌려주는 설정은 구독 키를 담고 있다.
pub(crate) async fn write_status(
    cluster: &Cluster,
    change: impl Fn(&mut SettingsStatus),
) -> Result<Settings, ApiError> {
    for _ in 0..10 {
        let settings = current(cluster)?;
        let mut status = settings.status.clone();
        change(&mut status);
        if status == settings.status {
            return Ok(settings);
        }
        let written = if settings.revision == 0 {
            let mut created = Resource::new(SETTINGS_NAME, settings.spec);
            created.status = status;
            match cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(created);
                }
                Err(e) => e,
            }
        } else {
            match cluster
                .update_status_checked::<SettingsSpec>(settings.id, settings.revision, &status)
                .await
            {
                Ok(_) => return Ok(current(cluster)?),
                Err(e) => e,
            }
        };
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. }) => continue,
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}

/// 설정을 읽어 `change`로 바꿔 쓴다. 설정이 아직 없으면 만든다(`expected`는 0). `expected`가
/// 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지 않는다.
/// 돌려주는 설정은 구독 키를 담고 있다.
pub(crate) async fn write(
    cluster: &Cluster,
    expected: Option<u64>,
    change: impl Fn(&mut SettingsSpec) -> Result<(), ApiError>,
) -> Result<Settings, ApiError> {
    for _ in 0..10 {
        let settings = current(cluster)?;
        if let Some(expected) = expected
            && expected != settings.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: settings.revision,
            })
            .into());
        }
        let mut spec = settings.spec.clone();
        change(&mut spec)?;
        if spec == settings.spec {
            return Ok(settings);
        }
        let written = if settings.revision == 0 {
            // 아직 없다. 다른 요청이 먼저 만들었으면 다시 읽는다.
            let mut created = Resource::new(SETTINGS_NAME, spec);
            match cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(created);
                }
                Err(e) => e,
            }
        } else {
            match cluster
                .update_spec(
                    settings.id,
                    Some(settings.revision),
                    settings.labels.clone(),
                    &spec,
                )
                .await
            {
                Ok(_) => return Ok(current(cluster)?),
                Err(e) => e,
            }
        };
        // If-Match 0으로 만들려 했는데 다른 요청이 먼저 만들었다.
        if let (ClusterError::Rejected(Rejection::NameTaken { .. }), Some(expected)) =
            (&written, expected)
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current(cluster)?.revision,
            })
            .into());
        }
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. })
                if expected.is_none() =>
            {
                continue;
            }
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/src/telemetry.rs`를 만든다:

```rust
//! 텔레메트리 보고 내용과 `GET /api/v1/telemetry/preview`. 수만 보내고 이름·주소·MAC·키·사용자·고객
//! 이름은 넣지 않는다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use opl_cluster::{Cluster, ClusterError};
use opl_types::backup_job::BackupJobSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::security_group::SecurityGroupSpec;
use opl_types::settings::SettingsSpec;
use opl_types::storage::StoragePoolSpec;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::{ImportSource, VmImportSpec};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::state::AppState;

/// 보고 형식 판
const SCHEMA: u32 = 1;

/// 텔레메트리 보고. 맵은 종류 → 수다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TelemetryReport {
    /// 보고 형식 판(지금 1)
    pub schema: u32,
    /// 설정 상태의 텔레메트리 id(아직 없으면 nil UUID)
    pub id: Uuid,
    /// 보낸 시각(유닉스 초)
    pub sent_at: u64,
    /// 버전별 노드 수(위트니스 포함, 버전을 적지 않은 노드는 `unknown`)
    pub versions: BTreeMap<String, u64>,
    pub nodes: NodeCounts,
    /// 아키텍처별 노드 수(위트니스 제외, 모르면 `unknown`)
    pub arch: BTreeMap<String, u64>,
    /// 하이퍼바이저별 노드 수(위트니스 제외)
    pub hypervisor: BTreeMap<String, u64>,
    /// CPU 합(위트니스 제외)
    pub cpus: u64,
    /// 메모리 합(위트니스 제외, GiB 내림)
    pub memory_gib: u64,
    pub vms: VmCounts,
    /// 백엔드 종류별 스토리지 풀 수
    pub pools: BTreeMap<String, u64>,
    /// 종류별 네트워크 수: `vxlan`, `vlan`(브리지와 VLAN), `bridge`
    pub networks: BTreeMap<String, u64>,
    pub security_groups: u64,
    pub backup_jobs: u64,
    /// 원본 종류별 VM 가져오기 수(`ova`, `esxi`)
    pub imports: BTreeMap<String, u64>,
    /// 구독 수준(`standard`, `premium`). 서명이 맞으면 만료돼도 수준이고, 없으면 `none`.
    pub subscription: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NodeCounts {
    pub total: u64,
    pub witness: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct VmCounts {
    pub total: u64,
    /// 지금 도는 VM(phase running)
    pub running: u64,
    /// HA를 켠 VM
    pub ha: u64,
}

fn count(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

fn len<T>(items: &[T]) -> u64 {
    items.len() as u64
}

/// 클러스터의 지금 상태로 보고를 만든다.
pub(crate) async fn build_report(
    cluster: &Cluster,
    id: Uuid,
    now: u64,
) -> Result<TelemetryReport, ClusterError> {
    let nodes = cluster.list::<NodeSpec>()?.1;
    let mut versions = BTreeMap::new();
    let (mut arch, mut hypervisor) = (BTreeMap::new(), BTreeMap::new());
    let (mut cpus, mut memory_mib, mut witness) = (0u64, 0u64, 0u64);
    for node in &nodes {
        count(
            &mut versions,
            node.status.version.as_deref().unwrap_or("unknown"),
        );
        if node.spec.witness {
            witness += 1;
            continue;
        }
        count(
            &mut arch,
            match node.status.arch {
                Some(Arch::X86_64) => "x86_64",
                Some(Arch::Aarch64) => "aarch64",
                None => "unknown",
            },
        );
        count(&mut hypervisor, &node.status.hypervisor);
        cpus += u64::from(node.status.cpus);
        memory_mib += node.status.memory_mib;
    }

    let vms = cluster.list::<VmSpec>()?.1;
    let mut pools = BTreeMap::new();
    for pool in cluster.list::<StoragePoolSpec>()?.1 {
        // 백엔드의 태그(`kind`)만 쓴다. 경로·서버 주소는 넣지 않는다.
        let kind = serde_json::to_value(&pool.spec.backend)?["kind"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        count(&mut pools, &kind);
    }
    let mut networks = BTreeMap::new();
    for network in cluster.list::<NetworkSpec>()?.1 {
        let kind = if network.spec.vxlan.is_some() {
            "vxlan"
        } else if network.spec.vlan.is_some() {
            "vlan"
        } else {
            "bridge"
        };
        count(&mut networks, kind);
    }
    let mut imports = BTreeMap::new();
    for import in cluster.list::<VmImportSpec>()?.1 {
        let kind = match import.spec.source {
            ImportSource::Ova(_) => "ova",
            ImportSource::Esxi(_) => "esxi",
        };
        count(&mut imports, kind);
    }
    let settings = crate::settings::current(cluster)?;
    let used = crate::subscription::used_nodes(cluster)?;
    let subscription = crate::subscription::state_of(&settings.spec, used)
        .level
        .map_or("none", |level| level.as_str())
        .to_string();

    Ok(TelemetryReport {
        schema: SCHEMA,
        id,
        sent_at: now,
        versions,
        nodes: NodeCounts {
            total: len(&nodes),
            witness,
        },
        arch,
        hypervisor,
        cpus,
        memory_gib: memory_mib / 1024,
        vms: VmCounts {
            total: len(&vms),
            running: vms
                .iter()
                .filter(|v| v.status.phase == VmPhase::Running)
                .count() as u64,
            ha: vms.iter().filter(|v| v.spec.ha).count() as u64,
        },
        pools,
        networks,
        security_groups: len(&cluster.list::<SecurityGroupSpec>()?.1),
        backup_jobs: len(&cluster.list::<BackupJobSpec>()?.1),
        imports,
        subscription,
    })
}

/// 지금 보낼 보고 그대로(꺼져 있어도). admin만 본다.
#[utoipa::path(get, path = "/telemetry/preview", tag = "settings",
    responses((status = 200, body = TelemetryReport), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn preview_telemetry(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<TelemetryReport>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let settings = crate::settings::current(&state.cluster)?;
    let id = settings
        .status
        .telemetry
        .and_then(|t| t.id)
        .unwrap_or_else(Uuid::nil);
    Ok(Json(build_report(&state.cluster, id, now()).await?))
}
```

`crates/opl-api/src/updates.rs`에서

```rust
            subscription: key.map(Into::into),
        }
```

을 다음으로 바꾼다:

```rust
            subscription: key.map(Into::into),
            ..SettingsSpec::default()
        }
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/subscription",
        "/nodes/{key}/updates/refresh",
```

을 다음으로 바꾼다:

```rust
        "/subscription",
        "/telemetry/preview",
        "/telemetry/reset-id",
        "/nodes/{key}/updates/refresh",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "{info}"
    );
```

을 다음으로 바꾼다:

```rust
        "{info}"
    );
    // 텔레메트리 설정·상태와 미리 보기 보고
    assert_eq!(
        doc["components"]["schemas"]["SettingsSpec"]["properties"]["telemetry"]["$ref"],
        "#/components/schemas/TelemetrySettings"
    );
    assert_eq!(
        doc["components"]["schemas"]["SettingsStatus"]["properties"]["telemetry"]["$ref"],
        "#/components/schemas/TelemetryStatus"
    );
    assert_eq!(
        doc["paths"]["/telemetry/preview"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/TelemetryReport"
    );
    assert!(
        doc["components"]["schemas"]["SettingsPatch"]["properties"]["telemetry"].is_object(),
        "{}",
        doc["components"]["schemas"]["SettingsPatch"]
    );
```

`crates/opl-api/tests/settings.rs`에서

```rust
        &admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true } }),
                None,
            )
            .await,
```

을 다음으로 바꾼다:

```rust
        &admin
            .patch("/settings", json!({ "telemetry": { "interval": 1 } }), None)
            .await,
```

`crates/opl-api/tests/telemetry.rs`를 만든다:

```rust
//! 텔레메트리 설정·id·미리 보기: 끝점 검사, 켤 때 id, reset-id, 권한, 옛 노드, 보낼 내용(이름·주소가 없음).

mod common;

use common::Env;
use opl_types::backup_job::BackupJobSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use opl_types::version::{BUILD, TELEMETRY_URL};
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::VmImportSpec;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const ENDPOINT: &str = "https://telemetry.example.com/v1/report";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn stored(env: &Env) -> Option<opl_types::settings::Settings> {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
}

fn telemetry_id(settings: &Value) -> Uuid {
    settings["status"]["telemetry"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id: {settings}"))
        .parse()
        .unwrap()
}

async fn add_node(
    env: &Env,
    name: &str,
    witness: bool,
    version: Option<&str>,
    tweak: impl FnOnce(&mut NodeStatus),
) -> Uuid {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "10.9.8.7:8006".into(),
            maintenance: false,
            witness,
        },
    );
    node.status.version = version.map(Into::into);
    tweak(&mut node.status);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// JSON spec으로 리소스를 바로 만든다.
async fn create<S>(env: &Env, name: &str, spec: Value)
where
    S: ResourceSpec + DeserializeOwned,
    S::Status: Default,
{
    let resource: Resource<S, S::Status> =
        Resource::new(name, serde_json::from_value::<S>(spec).unwrap());
    env.cluster.create(&resource).await.unwrap();
}

/// 끝점은 `https://` 주소여야 하고, 빌드 기본 끝점이 없으면 끝점 없이 켜지 않는다.
#[tokio::test]
async fn telemetry_needs_an_https_endpoint() {
    let env = Env::start().await;
    let admin = env.admin().await;
    if TELEMETRY_URL.is_empty() {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "telemetry_endpoint_required");
        assert_eq!(
            got.1["message"],
            "set telemetry.endpoint; this build has no default endpoint"
        );
    }
    for endpoint in [
        "http://telemetry.example.com/r",
        "https://",
        "https://bad host/r",
        "telemetry.example.com",
    ] {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true, "endpoint": endpoint } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    // 끝점에 사용자 정보(비밀번호)를 두지 않는다 — 설정은 누구나 읽는다.
    for endpoint in [
        "https://user:secret@telemetry.example.com/r",
        "https://user@telemetry.example.com/r",
    ] {
        let got = admin
            .patch(
                "/settings",
                json!({ "telemetry": { "endpoint": endpoint } }),
                None,
            )
            .await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "telemetry.endpoint must not contain credentials"
        );
    }
    assert!(stored(&env).is_none());

    // 끝점만 두는 것도 된다(켜지 않음).
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"]["telemetry"],
        json!({ "enabled": false, "endpoint": ENDPOINT })
    );
    assert_eq!(got["status"], json!({}));
}

/// 처음 켜면 id가 생기고, 꺼도 남고, reset-id가 바꾼다. 끝점 `""`는 지운다.
#[tokio::test]
async fn enabling_makes_an_id_that_stays_until_reset() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got["spec"],
        json!({
            "updates": { "channel": "stable" },
            "telemetry": { "enabled": true, "endpoint": ENDPOINT }
        })
    );
    let id = telemetry_id(&got);
    assert_eq!(id.get_version_num(), 4);
    assert_eq!(got["status"], json!({ "telemetry": { "id": id } }));
    assert_eq!(admin.get("/settings").await.1, got);
    let stored = stored(&env).unwrap();
    assert!(stored.spec.telemetry.enabled);
    assert_eq!(stored.status.telemetry.unwrap().id, Some(id));

    // 다시 켜도 id는 그대로다.
    let (_, again) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true } }),
            None,
        )
        .await;
    assert_eq!(telemetry_id(&again), id);
    assert_eq!(again["revision"], got["revision"]);

    let (status, off) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": false } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{off}");
    assert_eq!(off["spec"]["telemetry"]["enabled"], false);
    assert_eq!(off["spec"]["telemetry"]["endpoint"], ENDPOINT);
    assert_eq!(telemetry_id(&off), id);

    // 끝점을 지우면 기본값이 되어 spec에서 빠진다.
    let (status, cleared) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "endpoint": "" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(
        cleared["spec"],
        json!({ "updates": { "channel": "stable" } })
    );

    let (status, reset) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    let new_id = telemetry_id(&reset);
    assert_ne!(new_id, id);
    assert_eq!(new_id.get_version_num(), 4);
    assert_eq!(reset["name"], SETTINGS_NAME);
    assert_eq!(admin.get("/settings").await.1, reset);
}

/// 설정이 아직 없어도 reset-id가 만들고, 그 응답은 구독 키를 담지 않는다.
#[tokio::test]
async fn reset_id_creates_the_settings_and_hides_the_key() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-1".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now() - 60,
            expires_at: now() + 86_400,
        },
    )
    .unwrap();
    let (status, got) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"], json!({ "updates": { "channel": "stable" } }));
    let first = telemetry_id(&got);
    assert_eq!(
        stored(&env).unwrap().status.telemetry.unwrap().id,
        Some(first)
    );

    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key }), None)
            .await
            .0,
        StatusCode::OK
    );
    let (status, got) = admin.post("/telemetry/reset-id", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_ne!(telemetry_id(&got), first);
    assert!(!got.to_string().contains(&key), "{got}");
    assert_eq!(stored(&env).unwrap().spec.subscription, Some(key));
}

#[tokio::test]
async fn only_admins_change_telemetry_or_preview_it() {
    let env = Env::start().await;
    let body = json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } });
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        assert_error(
            &client.patch("/settings", body.clone(), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.post("/telemetry/reset-id", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
        assert_error(
            &client.get("/telemetry/preview").await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert!(stored(&env).is_none());

    // 켠 뒤에는 누구나 상태를 읽는다.
    let admin = env.admin().await;
    let (_, got) = admin.patch("/settings", body, None).await;
    let viewer = env.user("viewer2", "viewer").await;
    let (status, seen) = viewer.get("/settings").await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen["status"], got["status"]);
}

/// 이 빌드보다 낮은 노드가 있으면 텔레메트리를 바꾸지 않는다(옛 노드는 새 필드를 풀지 못한다).
#[tokio::test]
async fn telemetry_waits_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    add_node(&env, "old", true, Some("0.0.1~old"), |_| {}).await;
    for got in [
        admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
                None,
            )
            .await,
        admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": false } }),
                None,
            )
            .await,
        admin.post("/telemetry/reset-id", json!({})).await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
        assert_eq!(
            got.1["message"],
            "some nodes run an older opLite (old); finish the upgrade on every node first"
        );
    }
    assert!(stored(&env).is_none());
    // 채널은 그대로 바꾼다. 미리 보기도 된다.
    assert_eq!(
        admin
            .patch(
                "/settings",
                json!({ "updates": { "channel": "testing" } }),
                None
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(admin.get("/telemetry/preview").await.0, StatusCode::OK);
}

/// 미리 보기는 지금 보낼 그대로다: 수만 있고 이름·주소·MAC·고객·키는 없다.
#[tokio::test]
async fn preview_counts_resources_without_names_or_addresses() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "secret-sub".into(),
            customer: "Example Corp".into(),
            level: Level::Premium,
            nodes: 10,
            issued_at: now() - 60,
            expires_at: now() + 86_400,
        },
    )
    .unwrap();
    assert_eq!(
        admin
            .put("/subscription", json!({ "key": key }), None)
            .await
            .0,
        StatusCode::OK
    );

    // node1(Env): 이 빌드, aarch64, mock, CPU 4, 8192 MiB
    let node2 = add_node(&env, "secret-node2", false, Some("0.0.1~old"), |s| {
        s.arch = Some(Arch::X86_64);
        s.cpus = 8;
        s.memory_mib = 16_896;
        s.hypervisor = "qemu-kvm".into();
    })
    .await;
    add_node(&env, "secret-node3", false, None, |s| {
        s.cpus = 2;
        s.memory_mib = 1024;
        s.hypervisor = "qemu-kvm".into();
    })
    .await;
    // 위트니스는 버전과 노드 수에만 센다.
    add_node(&env, "secret-witness", true, Some(BUILD), |s| {
        s.arch = Some(Arch::X86_64);
        s.cpus = 64;
        s.memory_mib = 1 << 20;
        s.hypervisor = "none".into();
    })
    .await;

    // VM은 다른 노드에 둔다(이 노드의 reconciler가 건드리지 않게).
    for (name, mac, ha, running) in [
        ("secret-vm-a", "52:54:00:ab:cd:01", true, true),
        ("secret-vm-b", "52:54:00:ab:cd:02", false, true),
        ("secret-vm-c", "52:54:00:ab:cd:03", false, false),
    ] {
        let mut vm: opl_types::vm::Vm = Resource::new(
            name,
            serde_json::from_value::<VmSpec>(json!({
                "cpu": { "cores": 1 },
                "memory_mib": 256,
                "machine": "q35",
                "nics": [ { "network": "secret-net", "model": "virtio", "mac": mac } ],
                "node": node2,
                "ha": ha
            }))
            .unwrap(),
        );
        if running {
            vm.status.phase = VmPhase::Running;
        }
        env.cluster.create(&vm).await.unwrap();
    }
    for (name, backend) in [
        (
            "secret-pool-a",
            json!({ "kind": "dir", "path": "/srv/secret" }),
        ),
        (
            "secret-pool-b",
            json!({ "kind": "dir", "path": "/srv/secret2" }),
        ),
        (
            "secret-pool-c",
            json!({ "kind": "nfs", "server": "10.9.8.7", "export": "/secret" }),
        ),
    ] {
        create::<StoragePoolSpec>(&env, name, json!({ "backend": backend, "nodes": [node2] }))
            .await;
    }
    for (name, spec) in [
        ("secret-net", json!({ "bridge": "vmbr-secret" })),
        (
            "secret-vlan",
            json!({ "bridge": "vmbr-secret", "vlan": 42 }),
        ),
        ("secret-vx", json!({ "vxlan": { "vni": 4242 } })),
    ] {
        create::<NetworkSpec>(&env, name, spec).await;
    }
    for name in ["secret-sg-a", "secret-sg-b"] {
        create::<SecurityGroupSpec>(&env, name, json!({ "description": "secret-desc" })).await;
    }
    create::<BackupJobSpec>(
        &env,
        "secret-job",
        json!({ "vms": { "all": true }, "pool": "secret-pool-a", "schedule": { "at": "03:00" } }),
    )
    .await;
    for (name, source) in [
        (
            "secret-imp-a",
            json!({ "ova": { "file_name": "secret.ova", "size_bytes": 1 } }),
        ),
        (
            "secret-imp-b",
            json!({ "ova": { "file_name": "secret2.ova", "size_bytes": 1 } }),
        ),
        (
            "secret-imp-c",
            json!({ "esxi": {
                "host": "esxi.secret.example", "port": 443, "datacenter": "ha-datacenter",
                "datastore": "secret-ds", "vmx": "secret/secret.vmx", "user": "root",
                "fingerprint": "AB:CD"
            } }),
        ),
    ] {
        create::<VmImportSpec>(&env, name, json!({ "node": node2, "source": source })).await;
    }

    let before = now();
    let (status, report) = admin.get("/telemetry/preview").await;
    assert_eq!(status, StatusCode::OK, "{report}");
    let sent_at = report["sent_at"].as_u64().unwrap();
    assert!(sent_at >= before && sent_at <= now(), "{report}");
    let mut versions = serde_json::Map::new();
    versions.insert(BUILD.into(), json!(2));
    versions.insert("0.0.1~old".into(), json!(1));
    versions.insert("unknown".into(), json!(1));
    assert_eq!(
        report,
        json!({
            "schema": 1,
            "id": Uuid::nil(),
            "sent_at": sent_at,
            "versions": versions,
            "nodes": { "total": 4, "witness": 1 },
            "arch": { "aarch64": 1, "x86_64": 1, "unknown": 1 },
            "hypervisor": { "mock": 1, "qemu-kvm": 2 },
            "cpus": 14,
            // (8192 + 16896 + 1024) MiB = 25.5 GiB
            "memory_gib": 25,
            "vms": { "total": 3, "running": 2, "ha": 1 },
            "pools": { "dir": 2, "nfs": 1 },
            "networks": { "bridge": 1, "vlan": 1, "vxlan": 1 },
            "security_groups": 2,
            "backup_jobs": 1,
            "imports": { "ova": 2, "esxi": 1 },
            "subscription": "premium"
        })
    );
    let text = report.to_string();
    for secret in [
        "secret",
        "node1",
        "10.9.8.7",
        "127.0.0.1",
        "52:54:00",
        "Example Corp",
        "root",
        "admin",
        key.as_str(),
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

/// 켠 뒤 미리 보기의 id는 상태의 id다. 구독이 없으면 `none`.
#[tokio::test]
async fn preview_uses_the_telemetry_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, preview) = admin.get("/telemetry/preview").await;
    assert_eq!(preview["id"], json!(Uuid::nil()));
    assert_eq!(preview["subscription"], "none");
    let (status, got) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": true, "endpoint": ENDPOINT } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (_, preview) = admin.get("/telemetry/preview").await;
    assert_eq!(preview["id"], json!(telemetry_id(&got)));
}
```

`crates/opl-types/src/settings.rs`에서

```rust
use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust
use serde::{Deserialize, Serialize};

use uuid::Uuid;
```

`crates/opl-types/src/settings.rs`에서

```rust
    pub subscription: Option<String>,
}
```

을 다음으로 바꾼다:

```rust
    pub subscription: Option<String>,
    /// 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "TelemetrySettings::is_default")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub telemetry: TelemetrySettings,
}
```

`crates/opl-types/src/settings.rs`에서

```rust

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingsStatus {}
```

을 다음으로 바꾼다:

```rust

/// 텔레메트리 설정. 켜면 리더가 하루 한 번 클러스터 규모(이름·주소 없음)를 끝점에 보낸다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TelemetrySettings {
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub enabled: bool,
    /// 보낼 `https://` 주소. 없으면 빌드 값(`version::TELEMETRY_URL`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub endpoint: Option<String>,
}

impl TelemetrySettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingsStatus {
    /// 텔레메트리 상태. 처음 켤 때 생기고, 없으면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub telemetry: Option<TelemetryStatus>,
}

/// 텔레메트리 상태
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TelemetryStatus {
    /// 보고에 붙는 무작위 id(UUID v4). 처음 켤 때 만들고, 꺼도 남는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub id: Option<Uuid>,
    /// 마지막으로 보낸 시각(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_sent_at: Option<u64>,
    /// 마지막 보내기가 실패한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_error: Option<String>,
}
```

`crates/opl-types/src/settings.rs`에서

```rust
            subscription: None,
        };
```

을 다음으로 바꾼다:

```rust
            subscription: None,
            telemetry: TelemetrySettings::default(),
        };
```

`crates/opl-types/src/settings.rs`에서

```rust

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"telemetry":{}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"updates":{"mirror":"x"}}"#).is_err());
```

을 다음으로 바꾼다:

```rust

    /// 텔레메트리도 기본값(꺼짐, 끝점 없음, 상태 없음)이면 계획 32 모양 그대로 쓴다.
    #[test]
    fn telemetry_is_written_only_when_set() {
        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: false,
                endpoint: None,
            },
            ..SettingsSpec::default()
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::json!({ "updates": { "channel": "stable" } })
        );
        assert_eq!(
            serde_json::to_value(SettingsStatus::default()).unwrap(),
            serde_json::json!({})
        );

        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: true,
                endpoint: None,
            },
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "updates": { "channel": "stable" }, "telemetry": { "enabled": true } })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: false,
                endpoint: Some("https://t.example/r".into()),
            },
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["telemetry"],
            serde_json::json!({ "enabled": false, "endpoint": "https://t.example/r" })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);

        let status = SettingsStatus {
            telemetry: Some(TelemetryStatus {
                id: Some(Uuid::nil()),
                last_sent_at: Some(5),
                last_error: None,
            }),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "telemetry": { "id": Uuid::nil(), "last_sent_at": 5 } })
        );
        assert_eq!(
            serde_json::from_value::<SettingsStatus>(json).unwrap(),
            status
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"metrics":{}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"telemetry":{"interval":1}}"#).is_err());
        assert!(serde_json::from_str::<SettingsStatus>(r#"{"telemetry":{"sent":1}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"updates":{"mirror":"x"}}"#).is_err());
```

`crates/opl-types/src/version.rs`에서

```rust
pub const APT_URL: &str = match option_env!("OPL_APT_URL") {
    Some(v) => v,
```

을 다음으로 바꾼다:

```rust
pub const APT_URL: &str = match option_env!("OPL_APT_URL") {
    Some(v) => v,
    None => "",
};

/// 이 빌드의 텔레메트리 기본 끝점(빌드 때 환경 변수 `OPL_TELEMETRY_URL`). 없거나 비었으면 빈 문자열이고,
/// 그러면 설정에 끝점을 주어야 텔레메트리를 켠다.
pub const TELEMETRY_URL: &str = match option_env!("OPL_TELEMETRY_URL") {
    Some(v) => v,
```

`crates/opl-types/src/version.rs`에서

```rust
        assert_eq!(SUITE, "trixie");
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(SUITE, "trixie");
        assert_eq!(
            TELEMETRY_URL,
            option_env!("OPL_TELEMETRY_URL").unwrap_or("")
        );
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: SubscriptionCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

을 다음으로 바꾼다:

```rust
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
    }
```

을 다음으로 바꾼다:

```rust
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    #[test]
    fn cli_definition_is_valid() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1237개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 텔레메트리 설정·미리 보기와 oplctl telemetry — Settings.spec.telemetry{enabled, endpoint}(기본값이면 직렬화하지 않음)와 status.telemetry{id, last_sent_at, last_error}(없으면 쓰지 않음), 빌드 값 version::TELEMETRY_URL(OPL_TELEMETRY_URL), PATCH /settings의 telemetry{enabled, endpoint}(endpoint \"\"는 지움, https:// 주소가 아니면 400 invalid_request, 켰는데 끝점도 빌드 기본 끝점도 없으면 400 telemetry_endpoint_required, require_current 뒤 씀, 처음 켜면 status에 UUID v4 id — 꺼도 남음, 빈 패치는 \"give updates or telemetry\"), POST /telemetry/reset-id(admin, 새 id, 설정이 없으면 만듦), status 쓰기 다시 하기 settings::write_status, telemetry.rs TelemetryReport{schema 1, id, sent_at, versions, nodes, arch, hypervisor, cpus, memory_gib, vms, pools, networks, security_groups, backup_jobs, imports, subscription}·build_report(수만, 이름·주소·MAC·고객·키 없음), GET /telemetry/preview(admin, 아직 id가 없으면 nil UUID), OpenAPI·schema.d.ts, oplctl telemetry show|enable [--endpoint]|disable|preview|reset-id"
```

---

### Task 5: 텔레메트리 리포터와 opld [telemetry]

리더에서 도는 `TelemetryReporter`(백업 작업 스케줄러 모양): 켜져 있고 모든 노드가 이 빌드 이상이면, 마지막 전송에서 `interval`(기본 24시간)이 지나면 끝점에 JSON을 POST한다(리다이렉트를 따르지 않고 https만, `HTTPS_PROXY`를 따름, 시간 제한 30초). 실패하면 `retry`(1시간) 뒤 다시. `last_sent_at`이 Raft에 있어 리더가 바뀌어도 다시 보내지 않는다. opld `[telemetry]` `tick`·`interval`·`retry`·`ca_file`(사설 수집 서버의 CA). 시험은 시험 CA가 서명한 HTTPS 수집 서버로 하고, opld E2E는 노드 둘과 위트니스로 리더를 꺼도 다시 보내지 않음을 본다.

**Files:**
- Create: `crates/opl-api/tests/common/collector.rs`
- Create: `crates/opl-api/tests/telemetry_reporter.rs`
- Create: `crates/opld/tests/subscription.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/telemetry.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-peer/src/pki.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
opl-types = { workspace = true, features = ["openapi"] }
roxmltree.workspace = true
```

을 다음으로 바꾼다:

```toml
opl-types = { workspace = true, features = ["openapi"] }
reqwest.workspace = true
roxmltree.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
rcgen.workspace = true
reqwest.workspace = true
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
rcgen.workspace = true
tempfile.workspace = true
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use updates::{AUTH_FILE, KEYRING, UpdateAgent, Updates, UpdatesConfig};
```

을 다음으로 바꾼다:

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use telemetry::{TelemetryConfig, TelemetryReporter};
pub use updates::{AUTH_FILE, KEYRING, UpdateAgent, Updates, UpdatesConfig};
```

`crates/opl-api/src/telemetry.rs`에서

```rust
//! 텔레메트리 보고 내용과 `GET /api/v1/telemetry/preview`. 수만 보내고 이름·주소·MAC·키·사용자·고객
//! 이름은 넣지 않는다.

use std::collections::BTreeMap;
use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
//! 텔레메트리 보고 내용과 `GET /api/v1/telemetry/preview`, 그리고 리더가 보고를 보내는
//! [`TelemetryReporter`]. 수만 보내고 이름·주소·MAC·키·사용자·고객 이름은 넣지 않는다.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
```

`crates/opl-api/src/telemetry.rs`에서

```rust
use opl_types::storage::StoragePoolSpec;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::{ImportSource, VmImportSpec};
use serde::Serialize;
use utoipa::ToSchema;
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::StoragePoolSpec;
use opl_types::version::TELEMETRY_URL;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::{ImportSource, VmImportSpec};
use serde::Serialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
```

`crates/opl-api/src/telemetry.rs`에서

```rust
    Ok(Json(build_report(&state.cluster, id, now()).await?))
}
```

을 다음으로 바꾼다:

```rust
    Ok(Json(build_report(&state.cluster, id, now()).await?))
}

/// 텔레메트리 리포터 설정
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// 보낼 때가 됐는지 확인하는 간격
    pub tick: Duration,
    /// 보내는 간격(마지막으로 보낸 뒤)
    pub interval: Duration,
    /// 실패한 뒤 다시 보내기까지 기다리는 시간
    pub retry: Duration,
    /// 요청 하나의 시간 제한
    pub timeout: Duration,
    /// 시스템 루트에 더해 믿는 CA 인증서(DER). 사설 수집 서버와 시험에 쓴다.
    pub extra_roots: Vec<Vec<u8>>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            interval: Duration::from_secs(24 * 3600),
            retry: Duration::from_secs(3600),
            timeout: Duration::from_secs(30),
            extra_roots: Vec::new(),
        }
    }
}

/// 텔레메트리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 보낸다.
pub struct TelemetryReporter {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TelemetryReporter {
    pub fn spawn(state: Arc<AppState>, config: TelemetryConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(state: Arc<AppState>, config: TelemetryConfig, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut memory = Memory::default();
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 보내기와 선거 중의 기록은 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut memory) => {}
        }
    }
}

/// 리더의 메모리에만 있는 보내기 기록
#[derive(Debug, Default)]
struct Memory {
    /// 실패한 뒤 다시 보낼 시각
    next_retry: Option<u64>,
    /// 마지막으로 보낸 시각. 보낸 뒤 상태 쓰기가 실패해도 다시 보내지 않게 한다.
    last_sent: Option<u64>,
}

/// 지금 보낼 때인지. 마지막으로 보낸 시각은 설정(`recorded`)과 메모리 중 늦은 것이다.
fn due(now: u64, recorded: Option<u64>, memory: &Memory, interval: Duration) -> bool {
    let last_sent = recorded.max(memory.last_sent);
    last_sent.is_none_or(|t| now >= t.saturating_add(interval.as_secs()))
        && memory.next_retry.is_none_or(|t| now >= t)
}

/// 켜져 있는데 쓸 끝점이 없을 때 적는 오류
const NO_ENDPOINT: &str = "no telemetry endpoint: set telemetry.endpoint";

async fn tick(state: &AppState, config: &TelemetryConfig, memory: &mut Memory) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 옛 노드는 텔레메트리 상태가 든 설정을 풀지 못한다.
    if let Err(e) = crate::cluster::require_current(cluster) {
        tracing::debug!(reason = %e.message, "telemetry waits for the upgrade");
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 설정을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok(settings) = crate::settings::current(cluster) else {
        return;
    };
    let telemetry = &settings.spec.telemetry;
    if !telemetry.enabled {
        return;
    }
    let endpoint = telemetry
        .endpoint
        .clone()
        .unwrap_or_else(|| TELEMETRY_URL.to_string());
    let status = settings.status.telemetry.unwrap_or_default();
    if endpoint.is_empty() {
        // 같은 오류가 이미 적혀 있으면 쓰지 않는다.
        if status.last_error.as_deref() != Some(NO_ENDPOINT) {
            let written = crate::settings::write_status(cluster, |status| {
                status.telemetry.get_or_insert_default().last_error = Some(NO_ENDPOINT.into());
            })
            .await;
            if let Err(e) = written {
                tracing::warn!(error = %e.message, "cannot record the telemetry result");
            }
        }
        return;
    }
    let now = now();
    if !due(now, status.last_sent_at, memory, config.interval) {
        return;
    }
    // id는 켤 때 만들지만, 없으면 여기서 만들어 보고와 함께 적는다.
    let id = status.id.unwrap_or_else(Uuid::new_v4);
    let report = match build_report(cluster, id, now).await {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "cannot build the telemetry report");
            return;
        }
    };
    let error = send(config, &endpoint, &report).await.err();
    match &error {
        None => {
            memory.next_retry = None;
            memory.last_sent = Some(now);
            tracing::info!(%endpoint, "telemetry report sent");
        }
        Some(e) => {
            // 보내기가 오래 걸렸을 수 있으므로 retry는 실패한 때부터 센다.
            memory.next_retry = Some(self::now().saturating_add(config.retry.as_secs()));
            tracing::warn!(%endpoint, error = %e, "cannot send the telemetry report");
        }
    }
    let written = crate::settings::write_status(cluster, |status| {
        let telemetry = status.telemetry.get_or_insert_default();
        telemetry.id.get_or_insert(id);
        match &error {
            None => {
                telemetry.last_sent_at = Some(now);
                telemetry.last_error = None;
            }
            Some(e) => telemetry.last_error = Some(e.clone()),
        }
    })
    .await;
    if let Err(e) = written {
        tracing::warn!(error = %e.message, "cannot record the telemetry result");
    }
}

/// 보고를 JSON으로 `endpoint`에 보낸다. 2xx가 아니면 상태 코드가 오류다(3xx도 따르지 않고 오류다 —
/// 응답이 보고를 다른 곳으로 보내지 못하게). https만 쓴다. 프록시는 `HTTPS_PROXY`를 따른다.
async fn send(
    config: &TelemetryConfig,
    endpoint: &str,
    report: &TelemetryReport,
) -> Result<(), String> {
    let mut roots = Vec::new();
    for der in &config.extra_roots {
        roots.push(reqwest::Certificate::from_der(der).map_err(|e| e.to_string())?);
    }
    let client = reqwest::Client::builder()
        .timeout(config.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .tls_certs_merge(roots)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(endpoint)
        .json(report)
        .send()
        .await
        .map_err(|e| chain(&e))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

/// 오류와 그 원인들을 `: `로 잇는다(reqwest의 겉 오류만으로는 이유가 보이지 않는다).
fn chain(e: &dyn Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn due_waits_for_the_interval_and_the_retry() {
        let none = Memory::default();
        assert!(due(1_000, None, &none, DAY));
        assert!(!due(1_000, Some(900), &none, DAY));
        assert!(due(900 + 86_400, Some(900), &none, DAY));
        let retry = Memory {
            next_retry: Some(1_100),
            last_sent: None,
        };
        assert!(!due(1_000, None, &retry, DAY));
        assert!(due(1_100, None, &retry, DAY));
    }

    /// 보낸 뒤 상태 쓰기가 실패해 설정에 기록이 없거나 옛 기록만 있어도 메모리의 시각으로 막는다.
    #[test]
    fn a_send_this_leader_remembers_counts_too() {
        let sent = Memory {
            next_retry: None,
            last_sent: Some(1_000),
        };
        assert!(!due(1_500, None, &sent, DAY));
        assert!(!due(1_500, Some(10), &sent, DAY));
        assert!(due(1_000 + 86_400, Some(10), &sent, DAY));
        // 설정이 더 늦으면 그것을 쓴다(다른 리더가 보냄).
        assert!(!due(1_000 + 86_400, Some(2_000), &sent, DAY));
    }
}
```

`crates/opl-api/tests/common/collector.rs`를 만든다:

```rust
//! 시험용 텔레메트리 수집 서버. 시험 CA가 서명한 `localhost`·`127.0.0.1` 인증서의 HTTPS로
//! `POST /report`를 받아 본문(JSON)을 기록하고, 정한 상태 코드(와 `Location`)로 답한다(기본 204). 다른 크레이트의
//! 시험도 `#[path = ".../opl-api/tests/common/collector.rs"] mod collector;`로 가져다 쓸 수 있다
//! (axum·opl-peer·serde_json·tokio·uuid가 필요하다).
#![allow(dead_code)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use opl_peer::pki::{ClusterCa, NodeTls, first_cert, node_key_and_csr};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[derive(Clone)]
struct Site {
    reports: Arc<Mutex<Vec<Value>>>,
    status: Arc<AtomicU16>,
    location: Arc<Mutex<Option<String>>>,
}

pub struct Collector {
    /// 보고를 보낼 주소(`https://127.0.0.1:<port>/report`)
    pub url: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    /// 같은 CA(DER)
    pub ca_der: Vec<u8>,
    site: Site,
    server: JoinHandle<()>,
}

impl Collector {
    pub async fn start() -> Collector {
        let ca = ClusterCa::generate("telemetry").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["localhost".to_string(), "127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let listener = opl_peer::tls::TlsListener::new(tcp, config).unwrap();
        let site = Site {
            reports: Arc::default(),
            status: Arc::new(AtomicU16::new(204)),
            location: Arc::default(),
        };
        let app = axum::Router::new()
            .route("/report", axum::routing::post(report))
            .with_state(site.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Collector {
            url: format!("https://{addr}/report"),
            ca_der: first_cert(&ca.cert_pem).unwrap().to_vec(),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    /// 지금까지 받은 보고(받은 순서)
    pub fn reports(&self) -> Vec<Value> {
        self.site.reports.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.site.reports.lock().unwrap().len()
    }

    /// 앞으로 `status`로 답한다(기록은 그대로 한다).
    pub fn respond_with(&self, status: u16) {
        self.site.status.store(status, Ordering::SeqCst);
    }

    /// 앞으로 307과 `Location: location`으로 답한다(기록은 그대로 한다).
    pub fn redirect_to(&self, location: &str) {
        *self.site.location.lock().unwrap() = Some(location.to_string());
        self.respond_with(307);
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn report(State(site): State<Site>, body: Bytes) -> Response {
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    site.reports.lock().unwrap().push(value);
    let status = StatusCode::from_u16(site.status.load(Ordering::SeqCst)).unwrap();
    match site.location.lock().unwrap().clone() {
        Some(location) => (status, [(header::LOCATION, location)]).into_response(),
        None => status.into_response(),
    }
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust

pub mod esxi;
```

을 다음으로 바꾼다:

```rust

pub mod collector;
pub mod esxi;
```

`crates/opl-api/tests/telemetry_reporter.rs`를 만든다:

```rust
//! 텔레메트리 리포터: 시험 HTTPS 수집 서버(시험 CA)로 한 번 보내고 `last_sent_at`을 적는지, 간격 전에는
//! 다시 보내지 않고 간격이 지나면 다시 보내는지, 실패하면 `last_error`를 적고 retry 뒤 다시 보내는지,
//! 꺼져 있거나 옛 노드가 남아 있으면 보내지 않는지, 리다이렉트를 따르지 않는지, 끝점이 없으면 오류를
//! 적는지. 짧은 tick으로 돌린다.

mod common;

use std::time::{Duration, Instant};

use common::Env;
use common::collector::Collector;
use opl_api::{TelemetryConfig, TelemetryReporter};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{
    SETTINGS_NAME, Settings, SettingsSpec, TelemetrySettings, TelemetryStatus,
};
use opl_types::version::TELEMETRY_URL;
use reqwest::StatusCode;
use serde_json::json;

/// 기다리는 한도. macOS에서는 새 CA로 하는 첫 TLS 연결의 인증서 검증이 15초쯤 걸리기도 한다.
const REPORT_WAIT: Duration = Duration::from_secs(60);

/// 50ms마다 보는 리포터. 수집 서버의 CA를 믿는다. 시각은 초 단위라 retry 3초는 적어도 2초 뒤다.
fn reporter(env: &Env, collector: &Collector, interval: Duration) -> TelemetryReporter {
    TelemetryReporter::spawn(
        env.state.clone(),
        TelemetryConfig {
            tick: Duration::from_millis(50),
            interval,
            retry: Duration::from_secs(3),
            timeout: Duration::from_secs(30),
            extra_roots: vec![collector.ca_der.clone()],
        },
    )
}

/// 수집 서버로 텔레메트리를 켠다(끄면 끝점만 둔다).
async fn configure(env: &Env, collector: &Collector, enabled: bool) {
    let admin = env.admin().await;
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "telemetry": { "enabled": enabled, "endpoint": collector.url } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

fn telemetry(env: &Env) -> TelemetryStatus {
    env.cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .and_then(|s| s.status.telemetry)
        .unwrap_or_default()
}

async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + REPORT_WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 켜면 한 번 보내고 `last_sent_at`을 적는다. 간격(24시간) 전에는 다시 보내지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_once_and_records_the_time() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let id = telemetry(&env).id.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("a report", || collector.count() == 1).await;
    until("last_sent_at", || telemetry(&env).last_sent_at.is_some()).await;
    let report = &collector.reports()[0];
    assert_eq!(report["schema"], 1, "{report}");
    assert_eq!(report["id"], id.to_string(), "{report}");
    assert_eq!(report["nodes"]["total"], 1, "{report}");
    let status = telemetry(&env);
    assert_eq!(status.id, Some(id));
    assert_eq!(status.last_sent_at, report["sent_at"].as_u64());
    assert_eq!(status.last_error, None);

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 1);
    reporter.shutdown().await;
}

/// 간격이 지나면 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_again_after_the_interval() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(1));

    until("two reports", || collector.count() >= 2).await;
    let reports = collector.reports();
    assert!(
        reports[1]["sent_at"].as_u64() > reports[0]["sent_at"].as_u64(),
        "{reports:?}"
    );
    reporter.shutdown().await;
}

/// 수집 서버가 실패하면 `last_error`를 적고 retry 전에는 다시 보내지 않으며, 그 뒤 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_is_recorded_and_retried() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    collector.respond_with(500);
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    let status = telemetry(&env);
    let error = status.last_error.unwrap();
    assert!(error.contains("500"), "{error}");
    assert_eq!(status.last_sent_at, None);
    assert_eq!(collector.count(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(collector.count(), 1);

    collector.respond_with(200);
    until("a retried report", || {
        telemetry(&env).last_sent_at.is_some()
    })
    .await;
    assert_eq!(collector.count(), 2);
    assert_eq!(telemetry(&env).last_error, None);
    reporter.shutdown().await;
}

/// 꺼져 있으면 보내지 않고, 켜면 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_while_disabled() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, false).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 0);
    assert_eq!(telemetry(&env).last_sent_at, None);

    configure(&env, &collector, true).await;
    until("a report", || collector.count() == 1).await;
    reporter.shutdown().await;
}

/// 이 빌드보다 낮은 노드가 남아 있으면(설정의 새 필드를 풀지 못한다) 보내지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_while_an_old_node_remains() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    configure(&env, &collector, true).await;
    let mut old: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.9.8.7:8006".into(),
            maintenance: false,
            witness: false,
        },
    );
    old.status.version = Some("0.0.1".into());
    env.cluster.create(&old).await.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(collector.count(), 0);
    assert_eq!(telemetry(&env).last_sent_at, None);

    env.cluster.delete::<NodeSpec>(old.id, None).await.unwrap();
    until("a report", || collector.count() == 1).await;
    reporter.shutdown().await;
}

/// 3xx는 따르지 않고 실패로 적는다(끝점을 바꾼 응답이 보고를 다른 곳으로 보내지 못하게).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn redirects_are_not_followed() {
    let env = Env::start().await;
    let collector = Collector::start().await;
    // 따른다면 같은 수집 서버로 다시 보내 요청 수가 늘어난다.
    collector.redirect_to(&collector.url);
    configure(&env, &collector, true).await;
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    let error = telemetry(&env).last_error.unwrap();
    assert!(error.starts_with("HTTP 307"), "{error}");
    assert_eq!(telemetry(&env).last_sent_at, None);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(collector.count(), 1);
    reporter.shutdown().await;
}

/// 켜져 있는데 쓸 끝점이 없으면(설정도 빌드 값도 없음) 그 이유를 last_error에 한 번 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_endpoint_is_recorded() {
    if !TELEMETRY_URL.is_empty() {
        return;
    }
    let env = Env::start().await;
    let collector = Collector::start().await;
    // API는 이런 설정을 받지 않는다. 다른 빌드(빌드 기본 끝점이 있는)가 쓴 설정을 흉내 낸다.
    let settings: Settings = Settings::new(
        SETTINGS_NAME,
        SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: true,
                endpoint: None,
            },
            ..SettingsSpec::default()
        },
    );
    env.cluster.create(&settings).await.unwrap();
    let reporter = reporter(&env, &collector, Duration::from_secs(24 * 3600));

    until("last_error", || telemetry(&env).last_error.is_some()).await;
    assert_eq!(
        telemetry(&env).last_error.as_deref(),
        Some("no telemetry endpoint: set telemetry.endpoint")
    );
    let revision = |env: &Env| {
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .unwrap()
            .revision
    };
    let written = revision(&env);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        revision(&env),
        written,
        "the same error is not written again"
    );
    assert_eq!(collector.count(), 0);
    reporter.shutdown().await;
}
```

`crates/opl-peer/src/pki.rs`에서

```rust
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| PkiError::Pem(e.to_string()))
}
```

을 다음으로 바꾼다:

```rust
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| PkiError::Pem(e.to_string()))
}

/// PEM의 모든 인증서. 하나도 없으면 오류다.
pub fn certs(pem: &str) -> Result<Vec<CertificateDer<'static>>, PkiError> {
    let certs = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PkiError::Pem(e.to_string()))?;
    if certs.is_empty() {
        return Err(PkiError::Pem("no certificate".into()));
    }
    Ok(certs)
}
```

`crates/opl-peer/src/pki.rs`에서

```rust
    #[test]
    fn same_csr_signed_twice_gets_different_serials() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn certs_reads_every_certificate_of_a_bundle() {
        let a = ClusterCa::generate("a").unwrap().cert_pem;
        let b = ClusterCa::generate("b").unwrap().cert_pem;
        let both = certs(&format!("{a}{b}")).unwrap();
        assert_eq!(both, vec![first_cert(&a).unwrap(), first_cert(&b).unwrap()]);
        assert!(certs("not a cert").is_err());
        assert!(certs("").is_err());
    }

    #[test]
    fn same_csr_signed_twice_gets_different_serials() {
```

`crates/opld/src/config.rs`에서

```rust
use opl_api::{
    BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig, UpdatesConfig,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig, TelemetryConfig,
    UpdatesConfig,
};
```

`crates/opld/src/config.rs`에서

```rust
    pub updates: UpdatesSection,
}
```

을 다음으로 바꾼다:

```rust
    pub updates: UpdatesSection,
    #[serde(default)]
    pub telemetry: TelemetrySection,
}
```

`crates/opld/src/config.rs`에서

```rust
            poll: self.poll,
        })
    }
}

fn default_check_interval() -> Duration {
```

을 다음으로 바꾼다:

```rust
            poll: self.poll,
        })
    }
}

/// 텔레메트리 리포터(리더). 켜고 끄기와 끝점은 클러스터 설정(`PATCH /settings`)이 정한다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySection {
    /// 보낼 때가 됐는지 확인하는 간격
    #[serde(
        default = "default_telemetry_tick",
        deserialize_with = "deserialize_duration"
    )]
    pub tick: Duration,
    /// 보고를 보내는 간격
    #[serde(
        default = "default_telemetry_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Duration,
    /// 보내지 못한 뒤 다시 보내기까지 기다리는 시간
    #[serde(
        default = "default_telemetry_retry",
        deserialize_with = "deserialize_duration"
    )]
    pub retry: Duration,
    /// 시스템 루트에 더해 믿는 CA 인증서(PEM, 여럿이어도 된다). 사설 수집 서버에 쓴다.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
}

impl Default for TelemetrySection {
    fn default() -> Self {
        Self {
            tick: default_telemetry_tick(),
            interval: default_telemetry_interval(),
            retry: default_telemetry_retry(),
            ca_file: None,
        }
    }
}

impl TelemetrySection {
    /// API의 리포터 설정. `ca_file`을 읽지 못하거나 인증서가 없으면 오류다.
    pub fn telemetry_config(&self) -> anyhow::Result<TelemetryConfig> {
        let mut extra_roots = Vec::new();
        if let Some(path) = &self.ca_file {
            let pem = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
            let certs = opl_peer::pki::certs(&pem)
                .map_err(|e| anyhow::anyhow!("telemetry.ca_file {}: {e}", path.display()))?;
            extra_roots = certs.into_iter().map(|c| c.to_vec()).collect();
        }
        Ok(TelemetryConfig {
            tick: self.tick,
            interval: self.interval,
            retry: self.retry,
            extra_roots,
            ..TelemetryConfig::default()
        })
    }
}

fn default_telemetry_tick() -> Duration {
    Duration::from_secs(60)
}
fn default_telemetry_interval() -> Duration {
    Duration::from_secs(24 * 3600)
}
fn default_telemetry_retry() -> Duration {
    Duration::from_secs(3600)
}

fn default_check_interval() -> Duration {
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn telemetry_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.telemetry, TelemetrySection::default());
        let t = c.telemetry.telemetry_config().unwrap();
        assert_eq!(t.tick, Duration::from_secs(60));
        assert_eq!(t.interval, Duration::from_secs(24 * 3600));
        assert_eq!(t.retry, Duration::from_secs(3600));
        assert_eq!(t.timeout, Duration::from_secs(30));
        assert!(t.extra_roots.is_empty());

        // ca_file의 인증서를 모두(DER로) 믿는다.
        let dir = tempfile::tempdir().unwrap();
        let ca_file = dir.path().join("ca.pem");
        let a = opl_peer::pki::ClusterCa::generate("a").unwrap().cert_pem;
        let b = opl_peer::pki::ClusterCa::generate("b").unwrap().cert_pem;
        std::fs::write(&ca_file, format!("{a}{b}")).unwrap();
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[telemetry]\ntick = \"1s\"\ninterval = \"6h\"\n\
             retry = \"10m\"\nca_file = \"{}\"",
            ca_file.display()
        ));
        let t = c.telemetry.telemetry_config().unwrap();
        assert_eq!(t.tick, Duration::from_secs(1));
        assert_eq!(t.interval, Duration::from_secs(6 * 3600));
        assert_eq!(t.retry, Duration::from_secs(600));
        assert_eq!(
            t.extra_roots,
            vec![
                opl_peer::pki::first_cert(&a).unwrap().to_vec(),
                opl_peer::pki::first_cert(&b).unwrap().to_vec()
            ]
        );

        // 읽지 못하거나 인증서가 없는 파일은 시작 오류다.
        let with_ca = |path: &Path| {
            parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[telemetry]\nca_file = \"{}\"",
                path.display()
            ))
        };
        let missing = dir.path().join("missing.pem");
        let err = with_ca(&missing).telemetry.telemetry_config().unwrap_err();
        assert!(err.to_string().contains("missing.pem"), "{err}");
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        assert!(with_ca(&empty).telemetry.telemetry_config().is_err());

        for bad in ["interval = \"0s\"", "retry = \"1d\"", "endpoint = \"x\""] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[telemetry]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

`crates/opld/src/member.rs`에서

```rust
    AppState, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig,
    IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
    UpdateAgent, Updates,
};
```

을 다음으로 바꾼다:

```rust
    AppState, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig,
    IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig, TelemetryReporter,
    UefiVarsFetcher, UpdateAgent, Updates,
};
```

`crates/opld/src/member.rs`에서

```rust
    updates: Option<UpdateAgent>,
}
```

을 다음으로 바꾼다:

```rust
    updates: Option<UpdateAgent>,
    /// 리더가 텔레메트리 보고를 보낸다(설정에서 켰을 때).
    telemetry: TelemetryReporter,
}
```

`crates/opld/src/member.rs`에서

```rust
        self.backups.shutdown().await;
        self.recovery.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.backups.shutdown().await;
        self.telemetry.shutdown().await;
        self.recovery.shutdown().await;
```

`crates/opld/src/member.rs`에서

```rust
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
```

을 다음으로 바꾼다:

```rust
    ) -> anyhow::Result<(Self, Option<Created>)> {
        // `[telemetry] ca_file`을 읽지 못하면 아무것도 열기 전에 실패한다.
        let telemetry_config = config.telemetry.telemetry_config()?;
        let id = NodeId(node);
```

`crates/opld/src/member.rs`에서

```rust
        let backups = BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

을 다음으로 바꾼다:

```rust
        let backups = BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config());
        let telemetry = TelemetryReporter::spawn(state.clone(), telemetry_config);
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

`crates/opld/src/member.rs`에서

```rust
                backups,
                recovery,
```

을 다음으로 바꾼다:

```rust
                backups,
                telemetry,
                recovery,
```

`crates/opld/tests/subscription.rs`를 만든다:

```rust
//! 계획 33: 일반 노드 둘과 위트니스로 만든 클러스터에 구독 키를 넣고, 텔레메트리를 켜면 리더가 시험
//! HTTPS 수집 서버(`[telemetry] ca_file`로 믿는 시험 CA)로 한 번만 보낸다. 리더를 kill -9 해도 새 리더는
//! 간격(24시간) 전에 다시 보내지 않는다. 남은 노드를 짧은 간격으로 다시 시작하면 새 리더가 보낸다.
//! QEMU는 쓰지 않는다.

mod common;

#[path = "../../opl-api/tests/common/collector.rs"]
mod collector;

use std::time::Duration;

use collector::Collector;
use common::{Daemon, eventually};
use opl_types::subscription::{Level, LicensePayload, decode_private_key, sign};
use serde_json::Value;

/// 저장소 개발 키의 비밀 키 견본
const DEV_PRIVATE: &str = include_str!("../../opl-types/tests/fixtures/dev-license.key");
const DAY: u64 = 24 * 3600;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn leader_name(d: &Daemon) -> Option<String> {
    let out = d.ctl(&["-o", "json", "cluster", "status"]).await.ok()?;
    let view: Value = serde_json::from_str(&out).ok()?;
    view["members"]
        .as_array()?
        .iter()
        .find(|m| m["id"] == view["leader"])?["name"]
        .as_str()
        .map(str::to_string)
}

/// `oplctl -o json telemetry show`의 `status.telemetry`
async fn telemetry_status(d: &Daemon) -> Value {
    d.ctl_json(&["telemetry", "show"]).await["status"]["telemetry"].clone()
}

#[tokio::test]
async fn the_leader_reports_once_even_after_a_failover() {
    let collector = Collector::start().await;
    let ca = tempfile::tempdir().unwrap();
    let ca_file = ca.path().join("collector-ca.pem");
    std::fs::write(&ca_file, &collector.ca_pem).unwrap();
    // 틱은 짧게, 간격은 길게: 한 번 보낸 뒤로는 리더가 바뀌어도 다시 보내지 않아야 한다.
    let extra = format!(
        "[telemetry]\ntick = \"1s\"\ninterval = \"24h\"\nca_file = \"{}\"",
        ca_file.display()
    );
    let mut nodes = vec![Daemon::new("node-a", &extra), Daemon::new("node-b", &extra)];
    let mut w = Daemon::witness("wit");
    for d in &mut nodes {
        d.start().await;
    }
    w.start().await;
    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    nodes[1].login_with(&password, &fp).await;
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    w.join(url.trim()).await.unwrap();

    // 개발 키로 서명한 구독 키: 위트니스는 세지 않는다.
    let key = sign(
        &decode_private_key(DEV_PRIVATE).unwrap(),
        &LicensePayload {
            id: "sub-e2e".into(),
            customer: "Example Corp".into(),
            level: Level::Standard,
            nodes: 3,
            issued_at: now() - DAY,
            expires_at: now() + 30 * DAY,
        },
    )
    .unwrap();
    // 모든 노드(위트니스 포함)가 버전을 적어야 새 필드를 쓴다.
    eventually("the subscription is accepted", || async {
        nodes[0].ctl(&["subscription", "set", &key]).await.is_ok()
    })
    .await;
    // 다른 노드는 커밋을 조금 늦게 적용할 수 있다.
    eventually("the other node sees the subscription", || async {
        let shown = nodes[1].ctl(&["subscription", "show"]).await.unwrap();
        shown.contains("state: active (development key)") && shown.contains("nodes: 2/3")
    })
    .await;

    nodes[0]
        .ctl(&["telemetry", "enable", "--endpoint", &collector.url])
        .await
        .unwrap();
    eventually("a telemetry report", || async { collector.count() == 1 }).await;
    eventually("last_sent_at", || async {
        telemetry_status(&nodes[1]).await["last_sent_at"].is_u64()
    })
    .await;
    let report = &collector.reports()[0];
    assert_eq!(report["nodes"]["total"], 3, "{report}");
    assert_eq!(report["nodes"]["witness"], 1, "{report}");
    assert_eq!(report["subscription"], "standard", "{report}");
    let sent = telemetry_status(&nodes[1]).await;

    // 리더(일반 노드)를 kill -9 하면 남은 일반 노드가 리더가 된다. 몇 틱이 지나도 다시 보내지 않는다.
    let leader = leader_name(&nodes[0]).await.unwrap();
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive = &nodes[1 - dead];
    eventually("the other node leads", || async {
        leader_name(alive).await.as_deref() == Some(alive.name.as_str())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(collector.count(), 1, "{:?}", collector.reports());
    assert_eq!(telemetry_status(alive).await, sent);

    // 남은 노드를 간격 2초로 다시 시작하면(위트니스와 정족수) 새 리더가 다시 보낸다.
    let alive = &mut nodes[1 - dead];
    alive.stop();
    let config = alive.path("opld.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("interval = \"24h\""), "{text}");
    std::fs::write(
        &config,
        text.replace("interval = \"24h\"", "interval = \"2s\""),
    )
    .unwrap();
    alive.start().await;
    eventually("a report from the new leader", || async {
        collector.count() >= 2
    })
    .await;
    let reports = collector.reports();
    assert!(
        reports[1]["sent_at"].as_u64() > reports[0]["sent_at"].as_u64(),
        "{reports:?}"
    );
    assert_eq!(reports[1]["id"], reports[0]["id"], "{reports:?}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1249개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat: 텔레메트리 리포터와 opld [telemetry] — opl-api TelemetryReporter::spawn(AppState, TelemetryConfig{tick, interval, retry, timeout, extra_roots(DER)})·shutdown(모든 노드에서 띄우고 리더만 보냄, require_current가 아니면 그 틱을 건너뜀, read_barrier 뒤 설정을 읽어 꺼졌거나 last_sent_at + interval 전이거나 실패 뒤 retry 전이면 보내지 않음, reqwest로 끝점(설정 또는 OPL_TELEMETRY_URL)에 JSON POST — HTTPS_PROXY를 따름, 2xx면 last_sent_at = sent_at·last_error 지움, 아니면 last_error(HTTP 상태 또는 원인까지 이은 오류)와 리더 메모리의 다시 보낼 시각, id가 없으면 이때 만들어 함께 적음), opl-api의 reqwest를 일반 의존으로, opl_peer::pki::certs(PEM의 모든 인증서), opld [telemetry] tick(기본 60s)·interval(24h)·retry(1h)·ca_file(PEM, 읽지 못하면 시작 오류)과 member Services의 리포터, 시험 HTTPS 수집 서버(tests/common/collector.rs, 시험 CA가 서명한 잎 인증서), opld E2E(노드 둘 + 위트니스, 구독 넣기, 한 번 보낸 뒤 리더를 kill -9 해도 다시 보내지 않음)"
```

---

### Task 6: 웹: 구독·enterprise 채널·텔레메트리 설정과 구독 배너

설정 페이지의 구독 카드(상태, 고객, 수준, 노드 사용/한도, 만료일, 개발용 키; admin은 키 넣기·지우기), 업데이트 채널의 enterprise(구독이 active일 때만), 텔레메트리(켜기 전 미리 보기 대화상자, 끄기, 끝점, id 다시 만들기, 마지막 전송·오류). 대시보드 위 구독 안내 배너(active가 아니거나 만료 30일 전부터, 닫으면 그 안내를 7일 숨김). 오류 문구(ko·en).

**Files:**
- Create: `web/src/settings/SubscriptionBanner.test.tsx`
- Create: `web/src/settings/SubscriptionBanner.tsx`
- Create: `web/src/settings/SubscriptionSection.tsx`
- Create: `web/src/settings/TelemetrySection.tsx`
- Create: `web/src/settings/save.ts`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/LiveProvider.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/lib/format.test.ts`
- Modify: `web/src/lib/format.ts`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`
- Modify: `web/src/settings/SettingsPage.test.tsx`
- Modify: `web/src/settings/SettingsPage.tsx`
- Modify: `web/src/test/server.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/api/LiveProvider.test.tsx`에서

```tsx

import { listKey, useList, useSettings } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
```

을 다음으로 바꾼다:

```tsx

import { listKey, useList, useSettings, useSubscription } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx

it("reads the lists again when the watch cannot continue", async () => {
```

을 다음으로 바꾼다:

```tsx

function SubscriptionProbe() {
  const status = useLiveStatus();
  const subscription = useSubscription();
  return (
    <p>
      {status}: {subscription.data?.state}
    </p>
  );
}

it("reads the subscription again when the settings change", async () => {
  let state = "none";
  let reads = 0;
  server.use(
    ...lists([], 7),
    http.get(apiUrl("/subscription"), () => {
      reads += 1;
      return HttpResponse.json({ state, used_nodes: 1 });
    }),
    watch.addEventListener("connection", ({ client }) => {
      // 다른 admin이 키를 넣었다. 이벤트의 설정에는 키가 없다.
      state = "active";
      client.send(
        JSON.stringify({ type: "put", kind: "settings", resource: settings(9, "stable") }),
      );
    }),
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <SubscriptionProbe />
      </LiveProvider>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("live: active")).toBeInTheDocument();
  expect(reads).toBeGreaterThanOrEqual(2);
});

it("reads the subscription again when a node joins or leaves", async () => {
  const node = (id: string, revision: number) => ({ id, name: id, revision, spec: {}, status: {} });
  let state = "active";
  let send: ((event: object) => void) | undefined;
  server.use(
    http.get(apiUrl("/nodes"), () => HttpResponse.json({ revision: 7, items: [node("a", 3)] })),
    ...lists([], 7),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state, used_nodes: 1 })),
    watch.addEventListener("connection", ({ client }) => {
      send = (event) => client.send(JSON.stringify(event));
    }),
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <SubscriptionProbe />
      </LiveProvider>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("live: active")).toBeInTheDocument();
  // 새 노드가 합류해 한도를 넘었다.
  state = "over_limit";
  send!({ type: "put", kind: "node", resource: node("b", 9) });
  expect(await screen.findByText("live: over_limit")).toBeInTheDocument();
  // 그 노드가 떠났다.
  state = "active";
  send!({ type: "deleted", kind: "node", revision: 10, resource: node("b", 9) });
  expect(await screen.findByText("live: active")).toBeInTheDocument();
});

it("reads the lists again when the watch cannot continue", async () => {
```

`web/src/api/LiveProvider.tsx`에서

```tsx
  SETTINGS_KIND,
  type WatchEvent,
```

을 다음으로 바꾼다:

```tsx
  SETTINGS_KIND,
  SUBSCRIPTION_KEY,
  type WatchEvent,
```

`web/src/api/LiveProvider.tsx`에서

```tsx
        since = Math.min(...lists.map((list) => list.revision));
        // 설정은 목록이 없으므로 watch를 열기 전에 다시 읽는다(보는 화면이 있을 때만).
        await queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
      } catch {
```

을 다음으로 바꾼다:

```tsx
        since = Math.min(...lists.map((list) => list.revision));
        // 설정과 구독 상태는 목록이 없으므로 watch를 열기 전에 다시 읽는다(보는 화면이 있을 때만).
        await queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
        await queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
      } catch {
```

`web/src/api/LiveProvider.tsx`에서

```tsx
                );
              }
              break;
            }
```

을 다음으로 바꾼다:

```tsx
                );
                // 구독 키가 바뀌었을 수 있다(이벤트에는 키가 없으므로 상태를 다시 읽는다).
                void queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
              }
              break;
            }
            // 노드가 합류하거나 떠나면 구독의 노드 수가 바뀐다.
            if (
              event.kind === "node" &&
              (event.type === "deleted" ||
                !queryClient
                  .getQueryData<List<{ id: string }>>(listKey("node"))
                  ?.items.some((node) => node.id === event.resource.id))
            ) {
              void queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
            }
```

`web/src/api/live.ts`에서

```ts
export const SETTINGS_KEY = ["settings"] as const;
```

을 다음으로 바꾼다:

```ts
export const SETTINGS_KEY = ["settings"] as const;
/** 구독 상태(`GET /subscription`). 키는 설정에 있으므로 설정이 바뀌거나 다시 연결하면 다시 읽는다. */
export const SUBSCRIPTION_KEY = ["subscription"] as const;
/** 구독 상태를 다시 읽는 주기(만료는 시각으로 정해진다) */
export const SUBSCRIPTION_REFRESH_MS = 60 * 60 * 1000;
```

`web/src/api/live.ts`에서

```ts
    staleTime: Infinity,
  });
}
```

을 다음으로 바꾼다:

```ts
    staleTime: Infinity,
  });
}

/** 구독 상태. 노드 수와 시각으로 서버가 계산한다. 시각에 따라 만료되므로 한 시간마다 다시 읽는다. */
export function useSubscription() {
  return useQuery({
    queryKey: SUBSCRIPTION_KEY,
    queryFn: () => call(api.GET("/subscription")),
    staleTime: Infinity,
    refetchInterval: SUBSCRIPTION_REFRESH_MS,
  });
}
```

`web/src/api/types.ts`에서

```ts
export type Channel = Schemas["Channel"];
```

을 다음으로 바꾼다:

```ts
export type Channel = Schemas["Channel"];
export type SettingsPatch = Schemas["SettingsPatch"];
export type SubscriptionInfo = Schemas["SubscriptionInfo"];
export type SubscriptionState = Schemas["SubscriptionState"];
export type TelemetryReport = Schemas["TelemetryReport"];
```

`web/src/i18n/en.json`에서

```json
    "upgrade_in_progress": "An upgrade is in progress: {{message}}",
    "cluster_upgrading": "The cluster is being upgraded: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "upgrade_in_progress": "An upgrade is in progress: {{message}}",
    "cluster_upgrading": "The cluster is being upgraded: {{message}}",
    "subscription_required": "An active subscription is required for the enterprise channel.",
    "invalid_subscription_key": "The subscription key is not valid: {{message}}",
    "telemetry_endpoint_required": "Set a telemetry endpoint first. This build has no default endpoint."
  },
```

`web/src/i18n/en.json`에서

```json
    "noNodes": "No nodes.",
    "mixedVersions": "Nodes run different opLite versions ({{versions}}). Finish the upgrade on every node."
  },
```

을 다음으로 바꾼다:

```json
    "noNodes": "No nodes.",
    "mixedVersions": "Nodes run different opLite versions ({{versions}}). Finish the upgrade on every node.",
    "subscription": {
      "none": "This cluster has no subscription.",
      "invalid": "The subscription key does not match this build.",
      "expired": "The subscription expired on {{date}}.",
      "over_limit": "The cluster has {{used}} nodes, but the subscription covers {{limit}}.",
      "expiring": "The subscription expires on {{date}}.",
      "manage": "Subscription settings",
      "dismiss": "Hide for 7 days"
    }
  },
```

`web/src/i18n/en.json`에서

```json
      "stable": "Stable",
      "testing": "Testing"
    },
    "saved": "Settings saved.",
    "adminOnly": "Only administrators can change the settings."
  }
```

을 다음으로 바꾼다:

```json
      "stable": "Stable",
      "testing": "Testing",
      "enterprise": "Enterprise"
    },
    "saved": "Settings saved.",
    "adminOnly": "Only administrators can change the settings.",
    "errors": {
      "busy": "Another settings change is in progress. Try again."
    },
    "enterpriseHint": "Enterprise needs an active subscription.",
    "subscription": {
      "title": "Subscription",
      "states": {
        "none": "No subscription",
        "active": "Active",
        "expired": "Expired",
        "invalid": "Invalid key",
        "over_limit": "Over the node limit"
      },
      "development": "Development key",
      "customer": "Customer",
      "level": "Level",
      "levels": {
        "standard": "Standard",
        "premium": "Premium"
      },
      "nodes": "Nodes",
      "expires": "Expires",
      "key": "Subscription key",
      "keyHint": "Paste the key you received. It starts with opl1.",
      "apply": "Apply key",
      "applied": "Subscription key applied.",
      "remove": "Remove key",
      "removeTitle": "Remove the subscription key?",
      "removeDescription": "The cluster will have no subscription. Nodes on the enterprise channel stop getting updates.",
      "removed": "Subscription key removed."
    },
    "telemetry": {
      "title": "Telemetry",
      "intro": "When on, the leader sends a usage report once a day: counts and versions only, without names, addresses or keys.",
      "on": "On",
      "off": "Off",
      "endpoint": "Endpoint",
      "buildDefault": "Not set (the build's default endpoint is used if it has one)",
      "endpointHint": "An https:// address. Leave it empty to use the build default.",
      "saveEndpoint": "Save endpoint",
      "id": "Report ID",
      "lastSent": "Last sent",
      "never": "Never",
      "lastError": "Last error",
      "enable": "Turn on…",
      "disable": "Turn off",
      "resetId": "New report ID",
      "previewTitle": "Turn on telemetry",
      "previewDescription": "This is the report the cluster would send now. Nothing else is sent.",
      "confirmEnable": "Turn on",
      "enabled": "Telemetry turned on.",
      "disabled": "Telemetry turned off.",
      "idReset": "New report ID made."
    }
  }
```

`web/src/i18n/ko.json`에서

```json
    "upgrade_in_progress": "업그레이드가 진행 중입니다: {{message}}",
    "cluster_upgrading": "클러스터를 업그레이드하는 중입니다: {{message}}"
  },
```

을 다음으로 바꾼다:

```json
    "upgrade_in_progress": "업그레이드가 진행 중입니다: {{message}}",
    "cluster_upgrading": "클러스터를 업그레이드하는 중입니다: {{message}}",
    "subscription_required": "enterprise 채널에는 유효한(active) 구독이 필요합니다.",
    "invalid_subscription_key": "구독 키가 올바르지 않습니다: {{message}}",
    "telemetry_endpoint_required": "먼저 텔레메트리 끝점을 정하세요. 이 빌드에는 기본 끝점이 없습니다."
  },
```

`web/src/i18n/ko.json`에서

```json
    "noNodes": "노드가 없습니다.",
    "mixedVersions": "노드들이 서로 다른 opLite 버전({{versions}})을 돌립니다. 모든 노드의 업그레이드를 끝내세요."
  },
```

을 다음으로 바꾼다:

```json
    "noNodes": "노드가 없습니다.",
    "mixedVersions": "노드들이 서로 다른 opLite 버전({{versions}})을 돌립니다. 모든 노드의 업그레이드를 끝내세요.",
    "subscription": {
      "none": "이 클러스터에는 구독이 없습니다.",
      "invalid": "구독 키가 이 빌드와 맞지 않습니다.",
      "expired": "구독이 {{date}}에 만료되었습니다.",
      "over_limit": "클러스터의 노드는 {{used}}대인데 구독은 {{limit}}대까지입니다.",
      "expiring": "구독이 {{date}}에 만료됩니다.",
      "manage": "구독 설정",
      "dismiss": "7일 동안 숨기기"
    }
  },
```

`web/src/i18n/ko.json`에서

```json
      "stable": "stable(안정)",
      "testing": "testing(시험)"
    },
    "saved": "설정을 저장했습니다.",
    "adminOnly": "설정은 관리자만 바꿀 수 있습니다."
  }
```

을 다음으로 바꾼다:

```json
      "stable": "stable(안정)",
      "testing": "testing(시험)",
      "enterprise": "enterprise(구독)"
    },
    "saved": "설정을 저장했습니다.",
    "adminOnly": "설정은 관리자만 바꿀 수 있습니다.",
    "errors": {
      "busy": "다른 설정 변경이 진행 중입니다. 다시 시도하세요."
    },
    "enterpriseHint": "enterprise는 유효한 구독이 있어야 고를 수 있습니다.",
    "subscription": {
      "title": "구독",
      "states": {
        "none": "구독 없음",
        "active": "유효",
        "expired": "만료됨",
        "invalid": "잘못된 키",
        "over_limit": "노드 수 초과"
      },
      "development": "개발용 키",
      "customer": "고객",
      "level": "수준",
      "levels": {
        "standard": "Standard",
        "premium": "Premium"
      },
      "nodes": "노드",
      "expires": "만료일",
      "key": "구독 키",
      "keyHint": "받은 키를 붙여 넣으세요. opl1.로 시작합니다.",
      "apply": "키 넣기",
      "applied": "구독 키를 넣었습니다.",
      "remove": "키 지우기",
      "removeTitle": "구독 키를 지울까요?",
      "removeDescription": "클러스터에 구독이 없어집니다. enterprise 채널의 노드는 업데이트를 받지 못합니다.",
      "removed": "구독 키를 지웠습니다."
    },
    "telemetry": {
      "title": "텔레메트리",
      "intro": "켜면 리더가 하루에 한 번 사용 보고를 보냅니다. 수와 버전만 보내고 이름·주소·키는 보내지 않습니다.",
      "on": "켜짐",
      "off": "꺼짐",
      "endpoint": "끝점",
      "buildDefault": "설정 안 됨 — 빌드에 기본 끝점이 있으면 그것을 씀",
      "endpointHint": "https:// 주소. 비우면 빌드 기본 끝점을 씁니다.",
      "saveEndpoint": "끝점 저장",
      "id": "보고 ID",
      "lastSent": "마지막 전송",
      "never": "없음",
      "lastError": "마지막 오류",
      "enable": "켜기…",
      "disable": "끄기",
      "resetId": "보고 ID 새로 만들기",
      "previewTitle": "텔레메트리 켜기",
      "previewDescription": "지금 보낼 보고 그대로입니다. 이것 말고는 보내지 않습니다.",
      "confirmEnable": "켜기",
      "enabled": "텔레메트리를 켰습니다.",
      "disabled": "텔레메트리를 껐습니다.",
      "idReset": "보고 ID를 새로 만들었습니다."
    }
  }
```

`web/src/lib/format.test.ts`에서

```ts
import { formatBytes, formatCapacity, formatMib, formatTime, uuidTime } from "@/lib/format";
```

을 다음으로 바꾼다:

```ts
import {
  formatBytes,
  formatCapacity,
  formatDate,
  formatMib,
  formatTime,
  uuidTime,
} from "@/lib/format";
```

`web/src/lib/format.test.ts`에서

```ts
  expect(formatTime(time, "ko")).toContain("2025");
});
```

을 다음으로 바꾼다:

```ts
  expect(formatTime(time, "ko")).toContain("2025");
  expect(formatDate(time, "en")).toContain("2025");
  expect(formatDate(time, "en")).not.toContain(":");
});

it("shows dates in UTC like the server and oplctl", () => {
  // 어느 시간대의 브라우저에서도 같은 날이다(둘 중 하나는 로컬 시간대에서 다른 날이 된다).
  expect(formatDate(new Date("2026-01-01T00:30:00Z"), "en")).toBe("Jan 1, 2026");
  expect(formatDate(new Date("2025-12-31T23:30:00Z"), "en")).toBe("Dec 31, 2025");
});
```

`web/src/lib/format.ts`에서

```ts
  return date.toLocaleString(language, { dateStyle: "medium", timeStyle: "medium" });
}
```

을 다음으로 바꾼다:

```ts
  return date.toLocaleString(language, { dateStyle: "medium", timeStyle: "medium" });
}

/** 날짜만(만료일 등). 서버·oplctl처럼 UTC 날짜다. */
export function formatDate(date: Date, language: string): string {
  return date.toLocaleDateString(language, { dateStyle: "medium", timeZone: "UTC" });
}
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("shows the subscription notice above the dashboard", async () => {
  mockLists({ node: [NODE], vm: [] });
  server.use(
    cluster("n1"),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
  );
  renderPage(<DashboardPage />);
  expect(await screen.findByRole("status")).toHaveTextContent("This cluster has no subscription.");
});
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
import { compareVersions } from "@/lib/version";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
```

을 다음으로 바꾼다:

```tsx
import { compareVersions } from "@/lib/version";
import { SubscriptionBanner } from "@/settings/SubscriptionBanner";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      {/* 롤링 업그레이드 중이거나 멈춘 업그레이드가 있다. */}
```

을 다음으로 바꾼다:

```tsx
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      <SubscriptionBanner />
      {/* 롤링 업그레이드 중이거나 멈춘 업그레이드가 있다. */}
```

`web/src/settings/SettingsPage.test.tsx`에서

```tsx
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
```

을 다음으로 바꾼다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
```

`web/src/settings/SettingsPage.test.tsx`에서

```tsx
  expect(screen.getByText("Only administrators can change the settings.")).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("Only administrators can change the settings.")).toBeInTheDocument();
});

const ACTIVE = {
  state: "active",
  id: "sub-1",
  customer: "Example Corp",
  level: "premium",
  nodes: 4,
  used_nodes: 3,
  expires_at: 4102444800,
};

it.each([
  [
    { ...ACTIVE, development: true },
    ["Active", "Development key", "Example Corp", "Premium", "3 / 4"],
  ],
  [{ ...ACTIVE, state: "expired", expires_at: 1767225600 }, ["Expired", "Example Corp"]],
  [{ ...ACTIVE, state: "over_limit", used_nodes: 5 }, ["Over the node limit", "5 / 4"]],
  [{ state: "none", used_nodes: 2 }, ["No subscription", "2"]],
  [{ state: "invalid", used_nodes: 2 }, ["Invalid key"]],
])("shows the subscription state %#", async (info, texts) => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json(info)),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  for (const text of texts) {
    expect(await within(section).findByText(text)).toBeInTheDocument();
  }
  if (info.state === "none" || info.state === "invalid") {
    expect(within(section).queryByText("Customer")).not.toBeInTheDocument();
    expect(within(section).queryByText("Expires")).not.toBeInTheDocument();
  } else {
    expect(within(section).getByText("Expires")).toBeInTheDocument();
  }
  expect(within(section).queryByText("Development key") !== null).toBe("development" in info);
});

it("applies and removes a subscription key", async () => {
  const sent: { method: string; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 3 })),
    http.put(apiUrl("/subscription"), async ({ request }) => {
      sent.push({ method: "PUT", body: await request.json() });
      return HttpResponse.json(ACTIVE);
    }),
    http.delete(apiUrl("/subscription"), () => {
      sent.push({ method: "DELETE", body: null });
      return HttpResponse.json({ state: "none", used_nodes: 3 });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  expect(await within(section).findByText("No subscription")).toBeInTheDocument();
  // 지울 키가 없다.
  expect(within(section).queryByRole("button", { name: "Remove key" })).not.toBeInTheDocument();
  const apply = within(section).getByRole("button", { name: "Apply key" });
  expect(apply).toBeDisabled();
  await userEvent.type(within(section).getByLabelText("Subscription key"), "  opl1.abc.def\n");
  await userEvent.click(apply);
  expect(await screen.findByText("Subscription key applied.")).toBeInTheDocument();
  expect(sent).toEqual([{ method: "PUT", body: { key: "opl1.abc.def" } }]);
  expect(within(section).getByText("Active")).toBeInTheDocument();
  expect(within(section).getByLabelText("Subscription key")).toHaveValue("");

  await userEvent.click(within(section).getByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(dialog).toHaveTextContent("Remove the subscription key?");
  // 확인하기 전에는 지우지 않는다.
  expect(sent).toHaveLength(1);
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  expect(await screen.findByText("Subscription key removed.")).toBeInTheDocument();
  expect(sent[1]).toEqual({ method: "DELETE", body: null });
  expect(await within(section).findByText("No subscription")).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(within(section).getByRole("heading", { name: "Subscription" })).toHaveFocus(),
  );
});

it("says why a subscription key was rejected", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.put(apiUrl("/subscription"), () =>
      HttpResponse.json(
        {
          code: "invalid_subscription_key",
          message: "the subscription key signature does not match this build",
        },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await userEvent.type(within(section).getByLabelText("Subscription key"), "opl1.x.y");
  await userEvent.click(within(section).getByRole("button", { name: "Apply key" }));
  expect(await within(section).findByRole("alert")).toHaveTextContent(
    "The subscription key is not valid: the subscription key signature does not match this build",
  );
});

it("lets admins pick enterprise only with an active subscription", async () => {
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ ...ACTIVE, state: "expired" })),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { updates: { channel: string } };
      sent.push(body);
      return HttpResponse.json({ ...DEFAULT, revision: 2, spec: { updates: body.updates } });
    }),
  );
  const { unmount } = renderPage(<SettingsPage />);
  await screen.findByText("Expired");
  expect(screen.getByRole("option", { name: "Enterprise" })).toBeDisabled();
  expect(screen.getByLabelText("Update channel")).toHaveAccessibleDescription(
    /Enterprise needs an active subscription\./,
  );
  unmount();

  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(ACTIVE)));
  renderPage(<SettingsPage />);
  await screen.findByText("Active");
  expect(screen.getByRole("option", { name: "Enterprise" })).toBeEnabled();
  expect(screen.getByLabelText("Update channel")).not.toHaveAccessibleDescription(
    /Enterprise needs/,
  );
  await userEvent.selectOptions(screen.getByLabelText("Update channel"), "enterprise");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(sent).toEqual([{ updates: { channel: "enterprise" } }]));
});

it("explains when the enterprise channel needs a subscription", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "subscription_required", message: "an active subscription is required" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  await screen.findByText("Active");
  await userEvent.selectOptions(screen.getByLabelText("Update channel"), "enterprise");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "An active subscription is required for the enterprise channel.",
  );
});

const REPORT = {
  schema: 1,
  id: "00000000-0000-0000-0000-000000000000",
  sent_at: 1767225600,
  versions: { "0.9.0": 2 },
  nodes: { total: 2, witness: 0 },
  arch: { x86_64: 2 },
  hypervisor: { kvm: 2 },
  cpus: 16,
  memory_gib: 64,
  vms: { total: 3, running: 2, ha: 1 },
  pools: { dir: 1 },
  networks: { bridge: 1 },
  security_groups: 0,
  backup_jobs: 0,
  imports: {},
  subscription: "premium",
};

it("shows the report before turning telemetry on", async () => {
  const patches: unknown[] = [];
  let previews = 0;
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => {
      previews += 1;
      return HttpResponse.json(REPORT);
    }),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { telemetry: { enabled: boolean } };
      patches.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({
        ...DEFAULT,
        revision: 5,
        spec: { ...DEFAULT.spec, telemetry: body.telemetry },
        status: { telemetry: { id: "0a1b2c3d-0000-4000-8000-000000000001" } },
      });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  expect(within(section).getByText("Off")).toBeInTheDocument();
  expect(
    within(section).getByText("Not set (the build's default endpoint is used if it has one)"),
  ).toBeInTheDocument();
  expect(within(section).getByText("Never")).toBeInTheDocument();

  // 처음에는 취소한다: 켜지 않는다.
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  let dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  expect(dialog).toHaveTextContent('"memory_gib": 64');
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(patches).toEqual([]);

  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  expect(await screen.findByText("Telemetry turned on.")).toBeInTheDocument();
  expect(previews).toBe(2);
  expect(patches).toEqual([{ ifMatch: "0", body: { telemetry: { enabled: true } } }]);
  expect(within(section).getByText("On")).toBeInTheDocument();
  expect(within(section).getByText("0a1b2c3d-0000-4000-8000-000000000001")).toBeInTheDocument();
  expect(within(section).getByRole("button", { name: "Turn off" })).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(within(section).getByRole("heading", { name: "Telemetry" })).toHaveFocus(),
  );
});

it("turns telemetry off, sets the endpoint and makes a new report id", async () => {
  const sent: unknown[] = [];
  let settings = {
    ...DEFAULT,
    revision: 4,
    spec: { ...DEFAULT.spec, telemetry: { enabled: true, endpoint: "https://t.example/r" } },
    status: {
      telemetry: {
        id: "0a1b2c3d-0000-4000-8000-000000000001",
        last_sent_at: 1767225600,
        last_error: "HTTP 503",
      },
    },
  };
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(settings)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as {
        telemetry: { enabled?: boolean; endpoint?: string };
      };
      sent.push(body);
      settings = {
        ...settings,
        revision: settings.revision + 1,
        spec: { ...settings.spec, telemetry: { ...settings.spec.telemetry, ...body.telemetry } },
      };
      return HttpResponse.json(settings);
    }),
    http.post(apiUrl("/telemetry/reset-id"), () => {
      sent.push("reset-id");
      settings = {
        ...settings,
        revision: settings.revision + 1,
        status: {
          telemetry: { ...settings.status.telemetry, id: "0a1b2c3d-0000-4000-8000-000000000002" },
        },
      };
      return HttpResponse.json(settings);
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  expect(within(section).getByText("On")).toBeInTheDocument();
  expect(within(section).getByText("https://t.example/r")).toBeInTheDocument();
  expect(within(section).getByText("HTTP 503")).toBeInTheDocument();
  expect(within(section).queryByText("Never")).not.toBeInTheDocument();

  await userEvent.click(within(section).getByRole("button", { name: "Turn off" }));
  expect(await screen.findByText("Telemetry turned off.")).toBeInTheDocument();
  expect(within(section).getByText("Off")).toBeInTheDocument();
  // 누른 버튼이 사라졌으므로 포커스는 그 부분의 제목으로 간다.
  expect(within(section).getByRole("heading", { name: "Telemetry" })).toHaveFocus();

  const endpoint = within(section).getByLabelText("Endpoint");
  expect(endpoint).toHaveValue("https://t.example/r");
  await userEvent.clear(endpoint);
  await userEvent.click(within(section).getByRole("button", { name: "Save endpoint" }));
  await vi.waitFor(() =>
    expect(
      within(section).getByText("Not set (the build's default endpoint is used if it has one)"),
    ).toBeInTheDocument(),
  );

  await userEvent.click(within(section).getByRole("button", { name: "New report ID" }));
  expect(
    await within(section).findByText("0a1b2c3d-0000-4000-8000-000000000002"),
  ).toBeInTheDocument();
  expect(sent).toEqual([
    { telemetry: { enabled: false } },
    { telemetry: { endpoint: "" } },
    "reset-id",
  ]);
});

it("says when telemetry needs an endpoint", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => HttpResponse.json(REPORT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "telemetry_endpoint_required", message: "set telemetry.endpoint" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Set a telemetry endpoint first. This build has no default endpoint.",
  );
});

it("shows subscription and telemetry read-only to everyone but admins", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({
        ...DEFAULT,
        spec: { ...DEFAULT.spec, telemetry: { enabled: true } },
        status: { telemetry: { id: "0a1b2c3d-0000-4000-8000-000000000001" } },
      }),
    ),
  );
  renderPage(<SettingsPage />, { as: { ...ADMIN, role: "operator" } });
  const subscription = await screen.findByRole("region", { name: "Subscription" });
  expect(await within(subscription).findByText("Active")).toBeInTheDocument();
  expect(within(subscription).queryByLabelText("Subscription key")).not.toBeInTheDocument();
  expect(within(subscription).queryByRole("button")).not.toBeInTheDocument();
  const telemetry = screen.getByRole("region", { name: "Telemetry" });
  expect(within(telemetry).getByText("On")).toBeInTheDocument();
  expect(within(telemetry).queryByLabelText("Endpoint")).not.toBeInTheDocument();
  expect(within(telemetry).queryByRole("button")).not.toBeInTheDocument();
});

it("sends the endpoint typed but not saved when turning telemetry on", async () => {
  const patches: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => HttpResponse.json(REPORT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { telemetry: object };
      patches.push(body);
      return HttpResponse.json({
        ...DEFAULT,
        revision: 5,
        spec: { ...DEFAULT.spec, telemetry: body.telemetry },
      });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.type(within(section).getByLabelText("Endpoint"), " https://t.example/r ");
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  await vi.waitFor(() =>
    expect(patches).toEqual([{ telemetry: { enabled: true, endpoint: "https://t.example/r" } }]),
  );
  expect(await within(section).findByText("https://t.example/r")).toBeInTheDocument();
});

it("reads the settings again after the subscription key changes", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/settings"), () => {
      reads += 1;
      return HttpResponse.json(DEFAULT);
    }),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
    http.put(apiUrl("/subscription"), () => HttpResponse.json(ACTIVE)),
    http.delete(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await within(section).findByText("No subscription");
  expect(reads).toBe(1);
  await userEvent.type(within(section).getByLabelText("Subscription key"), "opl1.a.b");
  await userEvent.click(within(section).getByRole("button", { name: "Apply key" }));
  await vi.waitFor(() => expect(reads).toBe(2));

  await userEvent.click(within(section).getByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  await vi.waitFor(() => expect(reads).toBe(3));
});

it("says when another settings change is in progress", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "busy", message: "the settings are being changed; try again" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Update channel"), "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Another settings change is in progress. Try again.",
  );
});

it("shows why the subscription key could not be removed", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.delete(apiUrl("/subscription"), () =>
      HttpResponse.json(
        {
          code: "cluster_upgrading",
          message: "some nodes run an older opLite (node2); finish the upgrade on every node first",
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await userEvent.click(await within(section).findByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: some nodes run an older opLite (node2)",
  );
  // 지우지 못했으니 그대로다.
  expect(within(section).getByText("Active")).toBeInTheDocument();
});

it("says why the telemetry report could not be shown", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () =>
      HttpResponse.json({ code: "forbidden", message: "admin only" }, { status: 403 }),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  expect(await within(section).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});
```

`web/src/settings/SettingsPage.tsx` 전체:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useSettings, useSubscription } from "@/api/live";
import type { Channel } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { patchSettings, settingsError } from "@/settings/save";
import { SubscriptionSection } from "@/settings/SubscriptionSection";
import { TelemetrySection } from "@/settings/TelemetrySection";

const CHANNELS: Channel[] = ["stable", "testing", "enterprise"];

/** 클러스터 설정. 누구나 읽고 admin만 바꾼다. */
export function SettingsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const subscription = useSubscription();
  const write = can(me.role, "write", "settings");
  // 고르기 전에는 서버의 값을 보인다(watch로 바뀌어도 따라간다).
  const [chosen, setChosen] = useState<Channel | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!settings.data) {
    return <ListPending error={settings.error} />;
  }
  const current = settings.data.spec.updates?.channel ?? "stable";
  const channel = chosen ?? current;
  const revision = settings.data.revision;
  // enterprise 저장소는 구독 키로 인증하므로 구독이 active일 때만 고른다(서버도 확인한다).
  const subscribed = subscription.data?.state === "active";

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, { updates: { channel } });
      setChosen(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex max-w-xl flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.settings")}</h1>
      <SubscriptionSection write={write} />
      <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
        <h2 className="font-medium">{t("settings.updates")}</h2>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <Field
          id="settings-channel"
          label={t("settings.channel")}
          hint={
            subscribed
              ? t("settings.channelHint")
              : `${t("settings.channelHint")} ${t("settings.enterpriseHint")}`
          }
        >
          {(a11y) => (
            <NativeSelect
              id="settings-channel"
              value={channel}
              disabled={!write || busy}
              onChange={(e) => setChosen(e.target.value as Channel)}
              {...a11y}
            >
              {CHANNELS.map((c) => (
                <NativeSelectOption key={c} value={c} disabled={c === "enterprise" && !subscribed}>
                  {t(`settings.channels.${c}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        {write ? (
          <div>
            <Button type="submit" disabled={busy || channel === current}>
              {t("common.save")}
            </Button>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">{t("settings.adminOnly")}</p>
        )}
      </form>
      <TelemetrySection write={write} />
    </div>
  );
}
```

`web/src/settings/SubscriptionBanner.test.tsx`를 만든다:

```tsx
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { readPref, writePref } from "@/lib/prefs";
import {
  HIDDEN_UNTIL_PREF,
  SubscriptionBanner,
  subscriptionNotice,
} from "@/settings/SubscriptionBanner";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const DAY = 24 * 60 * 60;
const now = () => Math.floor(Date.now() / 1000);

const ACTIVE = {
  state: "active" as const,
  id: "sub-1",
  customer: "Example Corp",
  level: "standard" as const,
  nodes: 3,
  used_nodes: 2,
  expires_at: now() + 365 * DAY,
};

beforeEach(() => window.localStorage.clear());

function subscription(info: object) {
  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(info)));
}

it("picks the notice from the state and the days left", () => {
  const at = Date.UTC(2026, 0, 1);
  const expires = at / 1000 + 30 * DAY;
  expect(subscriptionNotice({ ...ACTIVE, expires_at: expires + 1 }, at)).toBeNull();
  // 30일 남은 날부터 곧 만료다.
  expect(subscriptionNotice({ ...ACTIVE, expires_at: expires }, at)).toBe("expiring");
  expect(subscriptionNotice({ state: "none", used_nodes: 1 }, at)).toBe("none");
  expect(subscriptionNotice({ state: "invalid", used_nodes: 1 }, at)).toBe("invalid");
  expect(subscriptionNotice({ ...ACTIVE, state: "expired" }, at)).toBe("expired");
  expect(subscriptionNotice({ ...ACTIVE, state: "over_limit" }, at)).toBe("over_limit");
});

it.each([
  [{ state: "none", used_nodes: 2 }, "status", "This cluster has no subscription."],
  [
    { state: "invalid", used_nodes: 2 },
    "status",
    "The subscription key does not match this build.",
  ],
  [
    { ...ACTIVE, state: "expired", expires_at: now() - DAY },
    "alert",
    "The subscription expired on",
  ],
  [
    { ...ACTIVE, state: "over_limit", used_nodes: 5 },
    "alert",
    "The cluster has 5 nodes, but the subscription covers 3.",
  ],
  [{ ...ACTIVE, expires_at: now() + 10 * DAY }, "status", "The subscription expires on"],
])("shows a notice %#", async (info, role, text) => {
  subscription(info);
  renderPage(<SubscriptionBanner />);
  const banner = await screen.findByRole(role);
  expect(banner).toHaveTextContent(text);
  expect(screen.getByRole("link", { name: "Subscription settings" })).toHaveAttribute(
    "href",
    "/settings",
  );
});

it("shows nothing for an active subscription far from expiry", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/subscription"), () => {
      reads += 1;
      return HttpResponse.json(ACTIVE);
    }),
  );
  renderPage(<SubscriptionBanner />);
  await vi.waitFor(() => expect(reads).toBe(1));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("hides the notice for seven days in this browser", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/subscription"), () => {
      reads += 1;
      return HttpResponse.json({ ...ACTIVE, state: "expired", expires_at: now() - DAY });
    }),
  );
  const first = renderPage(<SubscriptionBanner />);
  await screen.findByRole("alert");
  const before = Date.now();
  await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  // 닫은 안내와 시각을 함께 둔다.
  const [notice, at] = readPref(HIDDEN_UNTIL_PREF)!.split(":");
  expect(notice).toBe("expired");
  const until = Number(at);
  expect(until).toBeGreaterThanOrEqual(before + 7 * DAY * 1000);
  expect(until).toBeLessThanOrEqual(Date.now() + 7 * DAY * 1000);
  first.unmount();

  // 다시 열어도 숨긴다.
  const second = renderPage(<SubscriptionBanner />);
  await vi.waitFor(() => expect(reads).toBe(2));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  second.unmount();

  // 7일이 지나면 다시 보인다.
  writePref(HIDDEN_UNTIL_PREF, `expired:${Date.now() - 1}`);
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("alert")).toBeInTheDocument();
});

it("hides only the notice that was dismissed", async () => {
  let info: object = { state: "none", used_nodes: 2 };
  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(info)));
  const first = renderPage(<SubscriptionBanner />);
  await screen.findByRole("status");
  await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
  first.unmount();

  // 구독을 넣었다가 만료됐다: 다른 안내이므로 보인다.
  info = { ...ACTIVE, state: "expired", expires_at: now() - DAY };
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("alert")).toHaveTextContent("The subscription expired on");
});

it("ignores a hidden-until value from an older version", async () => {
  writePref(HIDDEN_UNTIL_PREF, String(Date.now() + DAY * 1000));
  subscription({ state: "none", used_nodes: 2 });
  renderPage(<SubscriptionBanner />);
  expect(await screen.findByRole("status")).toBeInTheDocument();
});

it("shows the notice when the browser storage cannot be used", async () => {
  const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("denied");
  });
  const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("denied");
  });
  try {
    subscription({ state: "none", used_nodes: 2 });
    renderPage(<SubscriptionBanner />);
    expect(await screen.findByRole("status")).toBeInTheDocument();
    // 닫기는 이번 화면에서는 된다.
    await userEvent.click(screen.getByRole("button", { name: "Hide for 7 days" }));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  } finally {
    getItem.mockRestore();
    setItem.mockRestore();
  }
});
```

`web/src/settings/SubscriptionBanner.tsx`를 만든다:

```tsx
import { XIcon } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useSubscription } from "@/api/live";
import type { SubscriptionInfo } from "@/api/types";
import { Button } from "@/components/ui/button";
import { formatDate } from "@/lib/format";
import { readPref, writePref } from "@/lib/prefs";

const DAY_MS = 24 * 60 * 60 * 1000;
/** 만료가 이만큼 남으면 "곧 만료"를 보인다. */
const EXPIRING_MS = 30 * DAY_MS;
/** 닫으면 이 브라우저에서 이만큼 숨긴다. */
const HIDE_MS = 7 * DAY_MS;
export const HIDDEN_UNTIL_PREF = "subscription-banner-hidden-until";

type Notice = "none" | "invalid" | "expired" | "over_limit" | "expiring";

/** 보일 안내. active이고 만료가 30일보다 많이 남았으면 없다. */
export function subscriptionNotice(info: SubscriptionInfo, now: number): Notice | null {
  if (info.state !== "active") {
    return info.state;
  }
  return info.expires_at !== undefined && info.expires_at * 1000 - now <= EXPIRING_MS
    ? "expiring"
    : null;
}

/** 닫은 안내와 그 안내를 숨길 시각(`"<안내>:<ms>"`). 옛 판의 숫자만 있는 값은 없는 것으로 본다. */
function dismissed(): { notice: string; until: number } | null {
  const [notice, until] = (readPref(HIDDEN_UNTIL_PREF) ?? "").split(":");
  return notice && until ? { notice, until: Number(until) || 0 } : null;
}

/**
 * 대시보드 위의 구독 안내. 닫으면 그 안내만 7일 동안 숨긴다(다른 안내가 되면 보인다). 상태를 읽지 못하면
 * 아무것도 보이지 않는다.
 */
export function SubscriptionBanner() {
  const { t, i18n } = useTranslation();
  const subscription = useSubscription();
  // 지금 시각은 구독 상태를 다시 읽을 때(한 시간마다) 함께 바뀐다.
  const now = subscription.dataUpdatedAt;
  const [hidden, setHidden] = useState(dismissed);

  const notice = subscription.data && subscriptionNotice(subscription.data, now);
  if (!notice || (hidden?.notice === notice && hidden.until > now)) {
    return null;
  }
  const info = subscription.data!;
  const warning = notice === "expired" || notice === "over_limit";
  const date =
    info.expires_at === undefined
      ? ""
      : formatDate(new Date(info.expires_at * 1000), i18n.language);

  return (
    <div
      role={warning ? "alert" : "status"}
      className={
        warning
          ? "flex items-center gap-3 rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
          : "flex items-center gap-3 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
      }
    >
      <p className="flex-1">
        {t(`dashboard.subscription.${notice}`, {
          date,
          used: info.used_nodes,
          limit: info.nodes,
        })}{" "}
        <Link to="/settings" className="underline underline-offset-4">
          {t("dashboard.subscription.manage")}
        </Link>
      </p>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={t("dashboard.subscription.dismiss")}
        onClick={() => {
          const until = Date.now() + HIDE_MS;
          writePref(HIDDEN_UNTIL_PREF, `${notice}:${until}`);
          setHidden({ notice, until });
        }}
      >
        <XIcon />
      </Button>
    </div>
  );
}
```

`web/src/settings/SubscriptionSection.tsx`를 만든다:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { SETTINGS_KEY, SUBSCRIPTION_KEY, useSubscription } from "@/api/live";
import type { SubscriptionInfo, SubscriptionState } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatDate } from "@/lib/format";
import { settingsError, withSettingsError } from "@/settings/save";

const BADGE: Record<SubscriptionState, "default" | "destructive" | "outline"> = {
  active: "default",
  expired: "destructive",
  over_limit: "destructive",
  none: "outline",
  invalid: "outline",
};

/** 구독 상태 카드. admin은 키를 넣고 지운다. 키 문자열은 서버가 돌려주지 않는다. */
export function SubscriptionSection({ write }: { write: boolean }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const subscription = useSubscription();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  // 대화상자를 닫으면 포커스를 돌려줄 곳: 지우기 버튼, 지워서 버튼이 사라지면 이 부분의 제목
  const returnFocus = useRef<HTMLElement | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  const info = subscription.data;
  const store = (updated: SubscriptionInfo) => {
    queryClient.setQueryData(SUBSCRIPTION_KEY, updated);
    // 구독 키는 설정에 있다(watch 이벤트를 기다리지 않고 다시 읽는다).
    void queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
  };

  async function apply(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      store(await call(api.PUT("/subscription", { body: { key: key.trim() } })));
      setKey("");
      toast.success(t("settings.subscription.applied"));
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="settings-subscription" className="flex flex-col gap-4">
      <h2 id="settings-subscription" ref={heading} tabIndex={-1} className="font-medium">
        {t("settings.subscription.title")}
      </h2>
      {!info ? (
        <ListPending error={subscription.error} />
      ) : (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("common.state")}</dt>
          <dd className="flex items-center gap-1">
            <Badge variant={BADGE[info.state]}>
              {t(`settings.subscription.states.${info.state}`)}
            </Badge>
            {info.development && (
              <Badge variant="outline">{t("settings.subscription.development")}</Badge>
            )}
          </dd>
          {info.customer && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.customer")}</dt>
              <dd>{info.customer}</dd>
            </>
          )}
          {info.level && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.level")}</dt>
              <dd>{t(`settings.subscription.levels.${info.level}`)}</dd>
            </>
          )}
          <dt className="text-muted-foreground">{t("settings.subscription.nodes")}</dt>
          <dd>
            {info.nodes === undefined ? info.used_nodes : `${info.used_nodes} / ${info.nodes}`}
          </dd>
          {info.expires_at !== undefined && (
            <>
              <dt className="text-muted-foreground">{t("settings.subscription.expires")}</dt>
              <dd>{formatDate(new Date(info.expires_at * 1000), i18n.language)}</dd>
            </>
          )}
        </dl>
      )}
      {write && (
        <form noValidate onSubmit={(e) => void apply(e)} className="flex flex-col gap-4">
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <Field
            id="settings-subscription-key"
            label={t("settings.subscription.key")}
            hint={t("settings.subscription.keyHint")}
          >
            {(a11y) => (
              <textarea
                id="settings-subscription-key"
                rows={3}
                spellCheck={false}
                autoComplete="off"
                value={key}
                disabled={busy}
                onChange={(e) => setKey(e.target.value)}
                className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
                {...a11y}
              />
            )}
          </Field>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy || key.trim() === ""}>
              {t("settings.subscription.apply")}
            </Button>
            {info && info.state !== "none" && (
              <Button
                type="button"
                variant="outline"
                disabled={busy}
                onClick={(e) => {
                  returnFocus.current = e.currentTarget;
                  setRemoving(true);
                }}
              >
                {t("settings.subscription.remove")}
              </Button>
            )}
          </div>
        </form>
      )}
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t("settings.subscription.removeTitle")}
        description={t("settings.subscription.removeDescription")}
        confirmLabel={t("settings.subscription.remove")}
        returnFocus={returnFocus}
        onConfirm={() =>
          withSettingsError(t, async () => {
            store(await call(api.DELETE("/subscription")));
            returnFocus.current = heading.current;
            toast.success(t("settings.subscription.removed"));
          })
        }
      />
    </section>
  );
}
```

`web/src/settings/TelemetrySection.tsx`를 만든다:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useSettings } from "@/api/live";
import type { TelemetryReport } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { formatTime } from "@/lib/format";
import { patchSettings, settingsError, storeSettings, withSettingsError } from "@/settings/save";

/**
 * 옵트인 텔레메트리. 켜기 전에는 지금 보낼 보고를 그대로 보여 주고 확인을 받는다. 상태(id, 마지막 전송,
 * 오류)는 리더가 설정 status에 적는다.
 */
export function TelemetrySection({ write }: { write: boolean }) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  // 입력하기 전에는 서버의 값을 보인다.
  const [endpoint, setEndpoint] = useState<string | null>(null);
  const [preview, setPreview] = useState<TelemetryReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 대화상자를 닫으면 포커스를 돌려줄 곳: 켜기 버튼, 켜서 버튼이 사라지면 이 부분의 제목
  const returnFocus = useRef<HTMLElement | null>(null);
  const heading = useRef<HTMLHeadingElement>(null);

  if (!settings.data) {
    return null;
  }
  const revision = settings.data.revision;
  const spec = settings.data.spec.telemetry;
  const status = settings.data.status.telemetry;
  const enabled = spec?.enabled ?? false;
  const saved = spec?.endpoint ?? "";
  const typed = endpoint ?? saved;

  async function run(action: () => Promise<void>, done?: string) {
    setBusy(true);
    setError(null);
    try {
      await action();
      if (done) {
        toast.success(done);
      }
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  const saveEndpoint = (e: React.FormEvent) => {
    e.preventDefault();
    void run(async () => {
      await patchSettings(queryClient, revision, { telemetry: { endpoint: typed.trim() } });
      setEndpoint(null);
    }, t("settings.saved"));
  };

  return (
    <section aria-labelledby="settings-telemetry" className="flex flex-col gap-4">
      <h2 id="settings-telemetry" ref={heading} tabIndex={-1} className="font-medium">
        {t("settings.telemetry.title")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("settings.telemetry.intro")}</p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("common.state")}</dt>
        <dd>
          <Badge variant={enabled ? "default" : "outline"}>
            {enabled ? t("settings.telemetry.on") : t("settings.telemetry.off")}
          </Badge>
        </dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.endpoint")}</dt>
        <dd className="break-all">{saved || t("settings.telemetry.buildDefault")}</dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.id")}</dt>
        <dd className="font-mono text-xs">{status?.id ?? "—"}</dd>
        <dt className="text-muted-foreground">{t("settings.telemetry.lastSent")}</dt>
        <dd>
          {status?.last_sent_at
            ? formatTime(new Date(status.last_sent_at * 1000), i18n.language)
            : t("settings.telemetry.never")}
        </dd>
        {status?.last_error && (
          <>
            <dt className="text-muted-foreground">{t("settings.telemetry.lastError")}</dt>
            <dd className="text-destructive">{status.last_error}</dd>
          </>
        )}
      </dl>
      {write && (
        <>
          <form noValidate onSubmit={saveEndpoint} className="flex flex-col gap-4">
            <Field
              id="settings-telemetry-endpoint"
              label={t("settings.telemetry.endpoint")}
              hint={t("settings.telemetry.endpointHint")}
            >
              {(a11y) => (
                <Input
                  id="settings-telemetry-endpoint"
                  type="url"
                  placeholder="https://"
                  value={typed}
                  disabled={busy}
                  onChange={(e) => setEndpoint(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
            <div>
              <Button type="submit" variant="outline" disabled={busy || typed.trim() === saved}>
                {t("settings.telemetry.saveEndpoint")}
              </Button>
            </div>
          </form>
          <div className="flex flex-wrap gap-2">
            {enabled ? (
              <Button
                variant="outline"
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await patchSettings(queryClient, revision, { telemetry: { enabled: false } });
                    // 누른 버튼이 "켜기"로 바뀌어 사라진다.
                    heading.current?.focus();
                  }, t("settings.telemetry.disabled"))
                }
              >
                {t("settings.telemetry.disable")}
              </Button>
            ) : (
              <Button
                disabled={busy}
                onClick={(e) => {
                  returnFocus.current = e.currentTarget;
                  // 켜기 전에 보낼 내용을 보여 준다.
                  void run(async () => setPreview(await call(api.GET("/telemetry/preview"))));
                }}
              >
                {t("settings.telemetry.enable")}
              </Button>
            )}
            <Button
              variant="outline"
              disabled={busy}
              onClick={() =>
                void run(async () => {
                  storeSettings(queryClient, await call(api.POST("/telemetry/reset-id")));
                }, t("settings.telemetry.idReset"))
              }
            >
              {t("settings.telemetry.resetId")}
            </Button>
          </div>
        </>
      )}
      {preview && (
        <FormDialog
          title={t("settings.telemetry.previewTitle")}
          description={t("settings.telemetry.previewDescription")}
          submitLabel={t("settings.telemetry.confirmEnable")}
          returnFocus={returnFocus}
          onClose={() => setPreview(null)}
          onSubmit={() =>
            withSettingsError(t, async () => {
              // 입력했지만 저장하지 않은 끝점도 함께 보낸다(켜기는 그 끝점으로 보내야 한다).
              const changed = typed.trim() !== saved;
              await patchSettings(queryClient, revision, {
                telemetry: changed ? { enabled: true, endpoint: typed.trim() } : { enabled: true },
              });
              if (changed) {
                setEndpoint(null);
              }
              returnFocus.current = heading.current;
              toast.success(t("settings.telemetry.enabled"));
            })
          }
        >
          <pre className="max-h-80 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
            {JSON.stringify(preview, null, 2)}
          </pre>
        </FormDialog>
      )}
    </section>
  );
}
```

`web/src/settings/save.ts`를 만든다:

```ts
import type { QueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, ApiError, call } from "@/api/client";
import { SETTINGS_KEY } from "@/api/live";
import type { Settings, SettingsPatch } from "@/api/types";
import { codeMessage, errorMessage } from "@/i18n/errors";

/** 설정·구독·텔레메트리 오류 문구. `settings.errors.<code>`(설정 쓰기가 겹친 busy 등)를 먼저 찾는다. */
export function settingsError(t: TFunction, err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "settings.errors")
    : errorMessage(t, err);
}

/** 대화상자가 보여 줄 수 있게 실패를 설정 오류 문구로 바꿔 던진다. */
export async function withSettingsError(t: TFunction, action: () => Promise<void>) {
  try {
    await action();
  } catch (err) {
    throw new Error(settingsError(t, err), { cause: err });
  }
}

/** 서버가 돌려준 설정을 캐시에 넣는다. 응답을 기다리는 사이 watch가 더 새 설정을 넣었으면 그것을 남긴다. */
export function storeSettings(queryClient: QueryClient, updated: Settings) {
  queryClient.setQueryData<Settings>(SETTINGS_KEY, (old) =>
    old && old.revision > updated.revision ? old : updated,
  );
}

/** 읽은 revision으로(`If-Match`) 설정을 바꾼다. */
export async function patchSettings(
  queryClient: QueryClient,
  revision: number,
  body: SettingsPatch,
) {
  const updated = await call(
    api.PATCH("/settings", { params: { header: { "If-Match": revision } }, body }),
  );
  storeSettings(queryClient, updated);
}
```

`web/src/test/server.ts`에서

```ts
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 */
export const server = setupServer(
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
);
```

을 다음으로 바꾼다:

```ts
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 * 구독 상태도 대시보드 배너가 읽으므로 기본으로 배너가 없는 상태(오래 남은 active)다.
 */
export const server = setupServer(
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
  http.get(apiUrl("/subscription"), () =>
    HttpResponse.json({
      state: "active",
      id: "sub-1",
      customer: "Example",
      level: "standard",
      nodes: 10,
      used_nodes: 1,
      expires_at: 4102444800,
    }),
  ),
);
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 구독·enterprise 채널·텔레메트리 설정과 대시보드 구독 배너 — 설정 페이지에 구독 상태 카드(상태 배지, 개발용 키, 고객, 수준, 노드 used/limit, 만료일; admin은 키 textarea로 PUT /subscription(다듬어 보냄), 확인 대화상자 뒤 DELETE), 업데이트 채널에 enterprise(구독이 active일 때만 고를 수 있고 아니면 비활성과 안내), 텔레메트리 부분(켜짐·끝점(없으면 빌드 기본값)·보고 ID·마지막 전송·마지막 오류; admin은 \"켜기…\"가 GET /telemetry/preview의 JSON을 대화상자로 보이고 확인해야 PATCH telemetry.enabled=true, 끄기, 끝점 저장(비우면 \"\"), POST /telemetry/reset-id), 설정 PATCH 공통 도우미 settings/save.ts, useSubscription(SUBSCRIPTION_KEY — 연결 때와 settings watch 이벤트에 다시 읽음), 대시보드 위 구독 안내(none·invalid·만료 30일 전부터 곧 만료는 정보 색 status, expired·over_limit는 경고 색 alert, 설정 링크, 닫으면 localStorage opl.subscription-banner-hidden-until에 7일 뒤 시각), formatDate, 시험 서버의 기본 구독 응답(배너 없는 active), 오류 문구 subscription_required·invalid_subscription_key·telemetry_endpoint_required, ko·en"
```

---

### Task 7: 문서

README 새 절 "구독과 텔레메트리"(키 형식, 상태, enterprise 저장소 인증, 발급 도구와 정식 빌드, 보내는 내용, `[telemetry]`), 설치·저장소 예의 `build-deb.sh`에 `OPL_DEV_LICENSE`·`OPL_LICENSE_KEYS`, 설계 §4.6 "구현(계획 33)"(설계와 다른 점과 알려진 틈)과 §5 오류, 처음 분해 문서의 하위 프로젝트 9 메모.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리)
```

`README.md`에서

```markdown
pnpm -C web install --frozen-lockfile && pnpm -C web build
OPL_VERSION=0.1.0 cargo build --release -p opld --features web-ui -p oplctl
packaging/deb/build-deb.sh 0.1.0 dist              # dist/oplite_0.1.0_amd64.deb
# 2. netinst. 판과 SHA-256은 packaging/iso/debian.lock에 있고, build-iso.sh가 해시를 확인한다.
```

을 다음으로 바꾼다:

```markdown
pnpm -C web install --frozen-lockfile && pnpm -C web build
#    구독 공개 키(OPL_LICENSE_KEYS) 없이 빌드하면 저장소의 개발 키를 믿는 개발 패키지다("구독과 텔레메트리").
OPL_VERSION=0.1.0 cargo build --release -p opld --features web-ui -p oplctl
OPL_DEV_LICENSE=1 packaging/deb/build-deb.sh 0.1.0 dist   # dist/oplite_0.1.0_amd64.deb
# 2. netinst. 판과 SHA-256은 packaging/iso/debian.lock에 있고, build-iso.sh가 해시를 확인한다.
```

`README.md`에서

```markdown
패키지로 설치한 노드는 opLite APT 저장소에서 업데이트를 확인하고, 관리자가 노드를 **하나씩** 업그레이드한다.
클러스터 설정의 **채널**(`stable`·`testing`)이 어느 저장소 구성 요소를 따를지 정한다.
```

을 다음으로 바꾼다:

```markdown
패키지로 설치한 노드는 opLite APT 저장소에서 업데이트를 확인하고, 관리자가 노드를 **하나씩** 업그레이드한다.
클러스터 설정의 **채널**(`stable`·`testing`, 구독이 있으면 `enterprise`)이 어느 저장소 구성 요소를 따를지 정한다.
```

`README.md`에서

```markdown
gpg --export "$KEY_ID" > oplite-archive-keyring.gpg
# 패키지: 저장소 주소(바이너리에)와 공개 키(/usr/share/keyrings/oplite-archive-keyring.gpg)
OPL_VERSION=0.2.0 OPL_APT_URL=https://apt.example.com/oplite \
    cargo build --release -p opld --features web-ui -p oplctl
OPL_APT_KEY=oplite-archive-keyring.gpg packaging/deb/build-deb.sh 0.2.0 dist
# 저장소: 채널(stable|testing|enterprise)에 패키지를 더하고 색인을 다시 만들어 서명한다(apt-ftparchive, apt-utils)
```

을 다음으로 바꾼다:

```markdown
gpg --export "$KEY_ID" > oplite-archive-keyring.gpg
# 패키지: 저장소 주소(바이너리에)와 공개 키(/usr/share/keyrings/oplite-archive-keyring.gpg).
# 구독 공개 키 OPL_LICENSE_KEYS는 바이너리와 build-deb.sh에 함께 준다("구독과 텔레메트리"). export하지 않고
# 두 명령 앞에만 준다: 같은 셸의 cargo test가 개발 키로 서명한 시험 키를 풀지 못해 실패한다.
KEYS=<opl-license keygen이 출력한 공개 키>
OPL_LICENSE_KEYS=$KEYS OPL_VERSION=0.2.0 OPL_APT_URL=https://apt.example.com/oplite \
    cargo build --release -p opld --features web-ui -p oplctl
OPL_LICENSE_KEYS=$KEYS OPL_APT_KEY=oplite-archive-keyring.gpg packaging/deb/build-deb.sh 0.2.0 dist
# 저장소: 채널(stable|testing|enterprise)에 패키지를 더하고 색인을 다시 만들어 서명한다(apt-ftparchive, apt-utils)
```

`README.md`에서

```markdown
넣으면 저장소를 주기적으로 다시 서명해야 하고, 그러지 못하면 모든 노드의 확인이 실패한다. `enterprise` 채널은
계획 33(구독)이 쓴다.
```

을 다음으로 바꾼다:

````markdown
넣으면 저장소를 주기적으로 다시 서명해야 하고, 그러지 못하면 모든 노드의 확인이 실패한다. `enterprise` 채널은
구독이 있는 클러스터가 쓴다(아래).

## 구독과 텔레메트리

구독은 기능을 막지 않는다. 키가 없거나 만료돼도 모든 기능이 돈다. 구독 키는 지원과 `enterprise` 업데이트 채널에
대한 권리이고, 노드가 빌드에 들어 있는 공개 키로 오프라인 검증한다(어떤 서버에도 묻지 않는다).

```bash
oplctl subscription show                         # state: active, customer, level, nodes: 2/3(사용/한도), expires: 2027-11-05
oplctl subscription set opl1.eyJpZCI6…           # admin. 키를 넣거나 바꾸고 상태를 보인다
oplctl subscription set --file key.txt           # 파일에서(끝의 줄바꿈은 다듬는다)
oplctl subscription set --file - < key.txt       # 표준 입력에서. 키를 명령줄(셸 기록, ps)에 두지 않으려면 --file을 쓴다
oplctl subscription remove                       # admin
oplctl settings set updates.channel=enterprise   # admin. 구독이 active일 때만
```

- **키.** `opl1.<payload>.<signature>` 한 줄이다. `payload`는 JSON
  `{"id","customer","level":"standard"|"premium","nodes","issued_at","expires_at"}`(시각은 유닉스 초, 모르는 필드는
  무시)의 base64url(패딩 없음), `signature`는 `opl1.<payload>` 바이트에 대한 Ed25519 서명의 base64url이다. `id`는
  영문자·숫자·`.`·`_`·`-`로 된 1~64자(영문자나 숫자로 시작, APT 인증 파일의 로그인이 된다), `customer`는 1~200자,
  `nodes`(위트니스가 아닌 노드 수 한도)는 1 이상, `expires_at`은 `issued_at`보다 뒤이고 9999-12-31T23:59:59Z
  (`253402300799`)를 넘지 않는다.
- **상태.** `GET /api/v1/subscription`(로그인한 누구나)이 읽을 때마다 계산한다: 키가 없으면 `none`, 형식이나 서명이
  맞지 않으면 `invalid`(공개 키가 다른 빌드로 바꾼 경우), 만료됐으면 `expired`, 위트니스가 아닌 노드가 한도보다 많으면
  `over_limit`(같으면 괜찮다), 그 밖은 `active`다. 저장소의 개발 키로 검증된 키는 `development: true`이고 oplctl은
  `state: active (development key)`, 웹은 "개발용 키"로 보인다. `PUT /api/v1/subscription`(`{"key":"opl1.…"}`)과
  `DELETE /api/v1/subscription`은 admin만 한다. 형식·서명이 틀리거나 이미 만료된 키는 **400**
  `invalid_subscription_key`이고 이유가 붙는다(`malformed subscription key: …`, `the subscription key signature does
  not match this build`, `the subscription key expired on 2026-01-01`). 키 문자열은 클러스터 설정
  (`Settings.spec.subscription`)에 저장하지만 API(`GET`·`PATCH /api/v1/settings`, watch)로는 내보내지 않는다. 감사
  로그는 없고 opld 로그에 키 id만 남는다.
- **웹.** "설정" 화면의 구독 카드가 상태·고객·수준·노드(사용/한도)·만료일을 보이고, admin은 키를 붙여 넣거나 지운다.
  상태가 `active`가 아니거나 만료 30일 전부터 대시보드 제목 아래에 안내가 보인다(`expired`·`over_limit`는 경고 색,
  그 밖은 정보 색). "7일 동안 숨기기"를 누르면 그 브라우저에서 그 안내만 7일 동안 숨긴다(예: "구독 없음"을 닫은 뒤
  구독이 만료되면 만료 안내는 보인다). 웹은 설정이 바뀌거나, 노드가 합류하거나 떠나거나, 다시 연결할 때와 한
  시간마다 상태를 다시 읽는다. 만료일은 UTC 날짜다.
- **enterprise 채널.** `updates.channel=enterprise`는 구독이 `active`일 때만 바꿀 수 있다(아니면 **400**
  `subscription_required`, 이미 enterprise여도 다시 보내면 검사한다). 채널이 enterprise이고 저장된 키의 서명이 맞으면
  노드마다 opld가 소스 파일(`Components: enterprise`)과 함께 APT 인증 파일 `/etc/apt/auth.conf.d/oplite.conf`(0600)를
  쓴다:

  ```
  # managed by opld; edits are overwritten
  machine apt.example.com login <구독 id> password <키 문자열의 SHA-256 16진수>
  ```

  `machine`은 `apt_url`의 호스트(포트가 있으면 `호스트:포트`)다. 프로토콜 없는 `machine` 항목을 apt는 https에만 쓰므로
  `apt_url`이 `http://`면 인증이 가지 않는다. 다른 채널로 바꾸거나 키를 지우면 opld가 쓴 파일을 지운다(첫 줄의 표시가
  없는 파일은 소스 파일처럼 건드리지 않고 경고만). 구독이 만료되거나 노드 수를 넘어도 채널과 인증 파일은 그대로다.
  저장소가 거절하면(401 등) `node updates`의 확인 오류에 보인다. 저장소 쪽에서 인증을 확인하는 일(예: 웹 서버가
  사용자 이름을 구독 id로, 비밀번호를 발급한 키의 SHA-256으로 확인하고 만료된 구독을 거절)은 opLite 밖이다.
- **옛 노드.** 구독 넣기·지우기, enterprise 채널, 텔레메트리 바꾸기(`reset-id` 포함)는 모든 노드(위트니스 포함)가 이 opld 빌드와
  같거나 새 버전일 때만 된다. 아니면 **409** `cluster_upgrading`("some nodes run an older opLite (node3); …"):
  옛 opld는 이 새 값이 든 설정을 풀지 못한다. 롤링 업그레이드를 끝낸 뒤 쓴다.

**키 발급.** 발급 도구 `opl-license`는 패키지에 넣지 않는다. 발급하는 쪽에서 저장소를 빌드해 쓴다.

```bash
cargo build --release -p opl-api --bin opl-license
# 발급 키 쌍(한 번). 비밀 키(PKCS#8 base64 한 줄)는 --out 파일(0600, 있으면 거절)에, 공개 키 base64는 표준 출력에
target/release/opl-license keygen --out license.key
# 키 발급: 표준 출력에 opl1.… 한 줄(--issued-at <유닉스 초>로 발급 시각을 정할 수 있다. 기본은 지금)
target/release/opl-license issue --key license.key --id acme-2026 --customer 'Acme Corp' \
    --level premium --nodes 3 --days 365
# 비밀 키 파일의 공개 키(OPL_LICENSE_KEYS의 항목)를 다시 보기
target/release/opl-license pubkey --key license.key
```

`issue`는 `--nodes`·`--days`가 1 이상인지 먼저 보고("opl-license: --days must be at least 1"), 내용이 키 규칙에 맞지
않으면 "opl-license: invalid license: id must be …"처럼 멈춘다. `keygen`은 쓰다가 실패하면 만든 파일을 지운다.

빌드 값 `OPL_LICENSE_KEYS`(base64 공개 키를 쉼표로)를 주고 빌드한 opld만 그 키로 서명한 구독 키를 믿는다. 주지
않으면 저장소의 개발 키(`crates/opl-types/keys/dev-license.pub`) 하나를 믿는데, 그 비밀 키가 시험 견본
(`crates/opl-types/tests/fixtures/dev-license.key`)으로 저장소에 있어 누구나 키를 만들 수 있다. 그래서 정식 패키지는
반드시 `OPL_LICENSE_KEYS`를 주고(위 "저장소 만들기"처럼 `cargo build`와 `build-deb.sh`에 함께), `build-deb.sh`는 이 값이
없으면 `OPL_DEV_LICENSE=1`(개발 패키지, CI의 패키지가 이렇다)이 아닌 한 "build-deb: OPL_LICENSE_KEYS is not set; …"로
멈춘다. 값이 있으면 세 가지를 더 본다: 쉼표로 나눈 항목마다 정규 base64(44자, `=` 끝)로 푼 32바이트 Ed25519 공개
키인지("build-deb: OPL_LICENSE_KEYS entry is not a base64 Ed25519 public key: …")와 그런 항목이 하나 이상인지(공백·쉼표뿐이면
"build-deb: OPL_LICENSE_KEYS has no keys"), 개발 키가 들어 있지 않은지("… contains the development
key; use OPL_DEV_LICENSE=1 for a development package"), opld 바이너리에 그 값이 그대로 들어 있는지(같은 값으로
빌드했는지, "build-deb: opld was not built with this OPL_LICENSE_KEYS"). 그래도 개발 키를 넣고 빌드한 바이너리는 그
키로 검증한 구독을 개발용으로 보인다. 빌드 값이 공백뿐이면 없는 것과 같지만(개발 키), 쉼표처럼 공백이 아닌 글자가 있는데
풀리는 키가 없으면 개발 키로 떨어지지 않고 어떤 구독 키도 믿지 않는다. 발급 키를
바꿀 때는 옛 공개 키와 새 공개 키를 함께 넣으면 이미 낸 키도 계속 풀린다. 이 키를 믿지 않는 빌드로 올리면 저장된 키는
`invalid`가 된다.

**텔레메트리.** 기본으로 꺼져 있다. 켜면 클러스터 리더가 하루에 한 번 사용 보고(수와 버전만)를 보낸다.

```bash
oplctl telemetry preview                           # admin. 지금 보낼 JSON 그대로(꺼져 있어도)
oplctl telemetry enable                            # admin. 저장된 끝점, 없으면 빌드 기본 끝점(OPL_TELEMETRY_URL)으로
oplctl telemetry enable --endpoint https://telemetry.example.com/v1/report
oplctl telemetry show                              # enabled, endpoint, id, last sent, last error
oplctl telemetry disable
oplctl telemetry reset-id                          # 보고 id를 새로 만든다
```

- **설정.** `PATCH /api/v1/settings`의 `{"telemetry":{"enabled":true,"endpoint":"https://…"}}`(admin)다. 끝점은
  호스트가 있는 `https://` 주소여야 하고 사용자 정보(`user:pass@`)를 넣을 수 없다(설정은 로그인한 누구나 읽는다.
  아니면 **400** `invalid_request`). `""`를 보내면 지워 빌드 값
  `OPL_TELEMETRY_URL`을 쓴다. 켰는데 끝점도 빌드 값도 없으면 **400** `telemetry_endpoint_required`. 처음 켤 때 무작위
  UUID v4 보고 id를 만든다(클러스터 이름·CA와 관계없다). 꺼도 id는 남고, `POST /api/v1/telemetry/reset-id`(admin)가
  새로 만든다. 웹 "설정" 화면은 "켜기…"를 누르면 보낼 JSON(`GET /api/v1/telemetry/preview`, admin)을 대화상자로
  보이고 확인해야 켠다(끝점 입력란에 저장하지 않은 값이 있으면 함께 보낸다).
- **보내기.** 리더가 `[telemetry] tick`마다 확인해, 켜져 있고 마지막 전송(`status.telemetry.last_sent_at`)에서
  `interval`이 지났으면 끝점에 JSON을 `POST`한다(시간 제한 30초, https만, 리다이렉트는 따르지 않는다). 2xx면
  `last_sent_at`을 적고 `last_error`를 지운다. 아니면 `last_error`("HTTP 500 Internal Server Error", 3xx면 "HTTP 307
  Temporary Redirect" 같은 상태, 또는 연결 오류)를 적고 `retry` 뒤에 다시 보낸다. 켜져 있는데 설정 끝점도 빌드 기본
  끝점도 없으면(다른 빌드가 켠 경우) `last_error`가 "no telemetry endpoint: set telemetry.endpoint"다. 마지막 전송
  시각은 Raft에 있어 리더가 바뀌어도 `interval` 전에는 다시 보내지 않는다. 보낸 뒤 그 기록을 쓰지 못해도 같은 리더는
  메모리의 전송 시각으로 다시 보내지 않는다. 다만 다시 보낼 시각과 그 전송 시각은 리더의 메모리에 있어, 실패 뒤
  리더가 바뀌면 새 리더가 곧 다시 보낼 수 있고, 보낸 뒤 기록하기 전에 리더가 바뀌면 새 리더가 한 번 더 보낼 수 있다.
  이 빌드보다 오래된 노드가 남아 있으면 보내지 않는다.
- **프록시.** reqwest가 `HTTPS_PROXY` 환경 변수를 따른다. 패키지의 opld는 systemd 서비스이므로
  `systemctl edit opld`로 덮어쓰기 파일에 다음을 넣고 `systemctl restart opld` 한다:

  ```ini
  [Service]
  Environment=HTTPS_PROXY=http://proxy.example.com:3128
  ```

- **보내는 내용**(`schema` 1). 이름, 주소, MAC, 키, 사용자, 고객 이름은 넣지 않는다:

  ```json
  {
    "schema": 1,
    "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
    "sent_at": 1793923200,
    "versions": { "0.2.0": 3 },
    "nodes": { "total": 3, "witness": 1 },
    "arch": { "x86_64": 2 },
    "hypervisor": { "qemu-kvm": 2 },
    "cpus": 64,
    "memory_gib": 251,
    "vms": { "total": 40, "running": 31, "ha": 6 },
    "pools": { "dir": 1, "nfs": 1, "rbd": 1 },
    "networks": { "bridge": 1, "vlan": 2, "vxlan": 1 },
    "security_groups": 3,
    "backup_jobs": 2,
    "imports": { "esxi": 5, "ova": 1 },
    "subscription": "premium"
  }
  ```

  `versions`는 버전별 노드 수(위트니스 포함, 버전을 모르면 `unknown`), `arch`·`hypervisor`·`cpus`·`memory_gib`(MiB 합을
  GiB로 내림)는 위트니스를 뺀 노드, `vms.running`은 지금 도는 VM, `vms.ha`는 HA를 켠 VM이다. `pools`는 백엔드 종류
  (`dir`, `nfs`, `lvm-thin`, `zfs`, `rbd`, `lvm-shared`), `networks`는 VXLAN이면 `vxlan`, 아니고 VLAN 태그가 있으면
  `vlan`, 그 밖은 `bridge`, `imports`는 가져오기의 원본 종류(`ova`, `esxi`)별 수다. `subscription`은 서명이 맞는
  구독 키의 수준(만료돼도), 키가 없거나 서명이 맞지 않으면 `none`이다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[telemetry]
tick = "60s"         # 리더가 보낼 때가 됐는지 보는 간격
interval = "24h"     # 보고 간격
retry = "1h"         # 보내지 못한 뒤 다시 보내기까지
# ca_file = "/etc/opl/telemetry-ca.pem"   # 시스템 루트에 더해 믿는 CA 인증서(PEM, 여럿 가능). 사설 수집 서버에 쓴다.
#                                         # 읽지 못하거나 인증서가 없으면 opld가 시작하지 않는다
```

수집 서버는 opLite 밖이다. 끝점의 기본값은 빌드할 때 `OPL_TELEMETRY_URL`로 넣는다(없으면 기본 끝점이 없다).
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료)과 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)부터 했다(`2026-11-05-install-update-subscription-design.md`). 계획 33(구독과 텔레메트리)이 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료), 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)과 계획 33(구독과 텔레메트리: Ed25519로 서명한 오프라인 구독 키와 발급 도구 `opl-license`, 빌드 값 `OPL_LICENSE_KEYS`(없으면 개발 키, 정식 패키지는 반드시), 읽을 때 계산하는 구독 상태(none·active·expired·invalid·over_limit), 구독이 있어야 하는 `enterprise` 채널과 노드의 APT 인증 파일, 이 빌드보다 낮은 노드가 있으면 새 설정 값을 쓰지 않는 게이트 `require_current`, 옵트인 텔레메트리(미리 보기, 리더의 `TelemetryReporter`가 하루 한 번, 수만 보냄), 웹 설정 화면과 대시보드 구독 안내, oplctl, 완료)으로 나눴다(`2026-11-05-install-update-subscription-design.md`). 하위 프로젝트 9는 끝났다. 오프라인 설치, ZFS 루트·RAID 설치, arm64 ISO, 기능 잠금, 온라인 구독 확인, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동·롤링 업그레이드, 노드 비우기(유지보수 모드), 커널·펌웨어 관리, 업데이트 되돌리기는 넣지 않았다.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown

## 5. 오류
```

을 다음으로 바꾼다:

```markdown

### 4.6 구현(계획 33)

- 키(`opl_types::subscription`, 새 직접 의존 `ring`·`base64`): `verify(key, keys)`는 형식·서명만, `parse(key, keys, now)`는 만료까지 본다(`PUT`이 쓰고, 상태 계산과 APT 인증 파일은 만료된 키도 풀어야 하므로 `verify`). 키는 앞뒤 공백을 다듬어 풀고 저장한다. `KeyError{Format, Signature, Expired}`의 문구가 400의 이유다(`malformed subscription key: …`, `the subscription key signature does not match this build`, `the subscription key expired on YYYY-MM-DD`). `license_keys()`는 빌드 값 `OPL_LICENSE_KEYS`(base64 표준 32바이트 키를 쉼표로)가 있으면 그것만, 없거나 공백뿐이면 개발 키 하나(`development: true`). 공백이 아닌 글자가 있는데 풀리는 키가 없으면(`","` 등) 개발 키로 떨어지지 않고 빈 목록(어떤 키도 믿지 않음)이다. 목록에 개발 공개 키가 들어 있어도 그 항목은 `development: true`다. 내용 검사(`sign`·`verify` 모두): `id`는 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`(APT 인증 파일의 한 토큰이 되고 명령 출력에 그대로 나간다), `customer` 1~200자, `nodes` ≥ 1, `issued_at` < `expires_at` ≤ `253402300799`(9999-12-31T23:59:59Z, 날짜로 바꿀 수 있는 상한). 발급 함수 `sign(pkcs8, &LicensePayload)`·`generate_key()`·`decode_private_key(text)`·`public_key(pkcs8)`는 opl-types에 있어 `opl-license`와 시험이 함께 쓴다. 비밀 키 파일은 PKCS#8 v2 DER의 base64 한 줄이다. `opl-license`는 `keygen`·`issue`에 더해 `pubkey --key <파일>`(비밀 키의 공개 키)을 두었고, `issue`는 `--nodes`·`--days` ≥ 1을 먼저 보고 내용 오류를 "invalid license: …"로 보이며, `keygen`은 쓰기에 실패하면 만든 파일을 지운다.
- 키 형식의 앞 호환: 옛 빌드는 모르는 `level` 값이 든 키 전체를 `invalid`로 본다(모르는 필드는 무시하지만 `level`은 enum이다). 그래서 새 수준을 더할 때는 새 접두(`opl2`)로 내고, 옛 빌드가 그 키를 받지 못함을 알린다. 지금 코드는 바꾸지 않는다.
- 상태(`status`/`SubscriptionInfo`): 판정 순서는 키 없음 `none` → 형식·서명 `invalid`(id 등은 비움) → 만료(`now ≥ expires_at`) `expired` → 위트니스가 아닌 노드가 한도보다 많음 `over_limit`(같으면 active) → `active`. `development`는 서명이 맞을 때만 있다. 이 응답에는 30일 "곧 만료"가 없고 웹이 `expires_at`으로 계산한다.
- 정식 빌드 강제: `build-deb.sh`가 `OPL_LICENSE_KEYS`가 비었고 `OPL_DEV_LICENSE=1`도 아니면 종료 1. 값이 있으면 (1) 쉼표로 나눈 항목마다 표준 base64 정규형(44자, `=` 끝, 풀었다 다시 만들면 같음)으로 푼 32바이트인지, 그리고 그런 항목이 하나 이상인지(공백·쉼표뿐이면 `build-deb: OPL_LICENSE_KEYS has no keys`), (2) 개발 공개 키(`crates/opl-types/keys/dev-license.pub`)가 들어 있지 않은지(있으면 `OPL_DEV_LICENSE=1`을 쓰라고), (3) opld 바이너리에 그 문자열이 그대로 들어 있는지(`option_env!` 값은 바이너리의 문자열 상수로 남는다 — 같은 값으로 빌드했는지)를 본다. CI의 패키지는 `OPL_DEV_LICENSE=1`이다.
- **옛 노드 게이트 `require_current`**(설계에 없음): 계획 32 노드의 `SettingsSpec`·`SettingsStatus`는 `deny_unknown_fields`이고 `Channel`에 `enterprise`가 없어, 새 필드가 든 설정 Raft 항목을 풀지 못한다. 그래서 (1) 새 필드는 기본값이면 직렬화하지 않고(`subscription` None, `telemetry` 기본값, `status.telemetry` 빈 값) 채널만 바꾸는 쓰기는 옛 노드도 계속 풀며, (2) 새 값을 쓰는 모든 곳(구독 `PUT`·`DELETE`, `enterprise`로 바꾸는 `PATCH`, `telemetry`가 든 `PATCH`(끄기도), `reset-id`)은 쓰기 전에 위트니스를 포함한 모든 노드의 `status.version`이 있고 이 빌드(`BUILD`) 이상인지 보고 아니면 409 `cluster_upgrading`(`require_upgraded`와 같은 문구, 이름 순)이다. 리포터도 이 조건이 아니면 그 틱을 건너뛴다. `require_upgraded`(버전 없는 노드만 봄)는 업그레이드 경로가 낮은 노드가 있어도 돌아야 하므로 그대로 둔다. 두 struct의 `deny_unknown_fields`도 그대로다.
- 구독 API: `PUT` 거절 순서 403 → 400 `invalid_subscription_key` → 409 `cluster_upgrading`, `DELETE`는 403 → 409(없으면 쓰지 않고 200 `none`). 설정 쓰기의 다시 하기 루프를 `settings::write`로 빼 `PATCH /settings`와 함께 쓴다. 키 문자열은 `GET`·`PATCH /settings`, `reset-id`의 응답과 watch의 설정 이벤트에서 지우고(`settings::public`), OpenAPI 스키마에서도 숨긴다. 설계와 다른 점: 감사 로그가 코드베이스에 없으므로 남기지 않고 `tracing::info!`로 키 id만 남긴다(텔레메트리 바꾸기도 감사 로그 없음).
- APT 인증 파일의 두 번째 방어: 키 검사가 id를 막지만, `auth_contents`는 호스트나 id에 공백·제어 문자가 있으면 쓰지 않는다(관리 파일은 지우고 `warn!`).
- enterprise 채널: `PATCH`가 `enterprise`를 보내면(이미 enterprise여도) `require_upgraded` → `require_current` 뒤, 쓸 spec의 구독이 `active`가 아니면 400 `subscription_required`. 노드(`updates.rs`)는 소스 파일과 함께 인증 파일(`UpdatesConfig.auth_file`, 상수 `AUTH_FILE` — opld 설정 키는 더하지 않음)을 맞춘다: 채널이 enterprise이고 `apt_url`이 있고 키의 서명이 맞으면 `machine <호스트[:포트]> login <id> password <키 SHA-256 16진수>`를 `.dpkg-tmp`·0600·fsync·rename으로(같으면 쓰지 않음, 소스 파일과 같은 `replace_file`), 아니면 관리 표시가 있는 파일만 지운다(표시 없는 파일은 그대로 두고 `warn!`). 소스 파일을 쓰지 않는 경우(키링 없음·Unmanaged)에도 같은 규칙이다. 인증 파일을 새로 쓰면 소스 파일처럼 곧 업데이트를 확인한다. 만료·`over_limit`인 키도 서명이 맞으면 파일을 둔다(설계 §4.2의 "구독이 끝나도 채널은 그대로"). apt는 프로토콜 없는 `machine` 항목을 https에만 쓰므로 `http://` 저장소에는 인증이 가지 않는다.
- 텔레메트리 설정: `PATCH /settings`의 `telemetry{enabled, endpoint}`(설계의 merge patch가 아니라 계획 32의 Patch 관례, endpoint `""`는 지움). 끝점은 호스트가 있는 `https://` 주소가 아니면 400 `invalid_request`("telemetry.endpoint must be an https:// URL"), authority에 사용자 정보(`@`)가 있으면 400 `invalid_request`("telemetry.endpoint must not contain credentials" — 설정은 로그인한 누구나 읽는다). 빈 패치는 "give updates or telemetry". 처음 켤 때 id는 spec 쓰기 뒤 두 번째 쓰기(`settings::write_status`, status에 10번 다시)로 만든다. `reset-id`는 설정이 없으면 만든다. 빌드 값 `opl_types::version::TELEMETRY_URL`(`OPL_TELEMETRY_URL`, 빈 값은 없음).
- 보고(`telemetry.rs` `TelemetryReport`, `build_report`): `versions`는 위트니스 포함(버전 없으면 `unknown`), `arch`(없으면 `unknown`)·`hypervisor`·`cpus`·`memory_gib`(MiB 합을 1024로 내림)는 위트니스 제외, `vms.running`은 phase `running`, `vms.ha`는 `spec.ha`. 네트워크는 종류 필드가 없으므로 `vxlan`이 있으면 `vxlan`, 아니고 `vlan`이 있으면 `vlan`, 그 밖은 `bridge`로 나눈다. 풀은 backend의 `kind` 태그만, 가져오기는 원본 종류(`ova`·`esxi`)만. `subscription`은 서명이 맞는 키의 수준(만료·`over_limit`여도)이고 아니면 `none`. 설계와 다른 점: `GET /telemetry/preview`는 보고 내용(클러스터 규모)을 보이므로 설정 쓰기와 같은 admin 권한이다. 아직 id가 없으면 nil UUID다.
- 리포터(`TelemetryReporter::spawn(AppState, TelemetryConfig{tick, interval, retry, timeout, extra_roots})`·`shutdown`, `BackupJobScheduler` 모양, 모든 일반 노드에서 띄우고 리더만 보냄): 틱 순서는 리더인가 → `require_current` → `read_barrier` → 켜짐 → 끝점(설정 또는 빌드 값; 둘 다 없으면 `last_error = "no telemetry endpoint: set telemetry.endpoint"`를 같은 값이 아닐 때만 적고 끝) → `max(last_sent_at, 메모리의 last_sent) + interval`·메모리의 다시 보낼 시각(판단은 함수 `due`) → 보고 만들기 → `POST`(reqwest, opl-api의 일반 의존이 됨, 시간 제한 30초, 리다이렉트를 따르지 않음(`Policy::none`, 3xx는 "HTTP 307 …" 오류)·https만(`https_only`), 프록시는 reqwest 기본이라 `HTTPS_PROXY`, `extra_roots`는 시스템 루트에 더함) → 성공이면 메모리의 `last_sent`를 지금으로 → `write_status`(2xx면 `last_sent_at = sent_at`·`last_error` 지움, 아니면 `last_error`는 "HTTP <상태>" 또는 원인까지 `: `로 이은 오류). id가 없으면 이때 만들어 함께 적는다. 메모리의 `last_sent`는 보낸 뒤 상태 쓰기가 실패해도 같은 리더가 다시 보내지 않게 한다.
- opld `[telemetry]`: 설계의 `interval`(기본 "24h")에 더해 `retry`("1h"), `tick`("60s" — 설계 밖, E2E가 짧게 쓰려고 둠), `ca_file`(PEM, 여러 인증서, 읽지 못하거나 인증서가 없으면 opld 시작 오류)을 두었다. 설계 §4.5는 추가 루트 인증서를 시험만 넘긴다고 했지만, 사설 수집 서버와 opld E2E를 위해 opld 설정 `ca_file`로 둔다. 인증서 PEM 읽기는 `opl_peer::pki::certs`.
- oplctl: `subscription show|set <KEY>|set --file <파일>|set --file -(표준 입력, 키를 명령줄에 두지 않음)|remove`(`state: active (development key)`, `customer`, `level`, `nodes: <used>/<limit>`, `expires: YYYY-MM-DD`; `set`도 결과 상태를 보임), `telemetry show|enable [--endpoint]|disable|preview|reset-id`(show는 `enabled`·`endpoint`(없으면 oplctl 빌드의 기본값에 `(build default)`, 그것도 없으면 `none`)·`id`·`last sent`·`last error`, preview는 JSON). `settings set updates.channel=enterprise`는 값 검사를 서버에 맡긴다.
- 웹: `/settings` 한 화면에 구독(`SubscriptionSection`)·업데이트 채널·텔레메트리(`TelemetrySection`) 부분. enterprise는 구독이 `active`일 때만 고를 수 있고 아니면 막힌 항목과 안내. 텔레메트리 "켜기…"는 `GET /telemetry/preview`의 JSON을 대화상자로 보이고 확인해야 `PATCH`한다. 구독 배너(`SubscriptionBanner`)는 대시보드 제목 아래, 닫으면 localStorage `opl.subscription-banner-hidden-until`에 `"<안내>:<ms>"`를 두고 그 안내일 때만 7일 숨긴다(다른 안내가 되면 보임, 옛 숫자 값은 무시). `useSubscription()`(`SUBSCRIPTION_KEY`)은 연결 때, 설정 watch 이벤트, 목록에 없던 노드의 put과 노드 delete 이벤트에, 그리고 한 시간마다(`refetchInterval`) 다시 읽고, 배너의 "지금"은 마지막으로 읽은 시각이다. 구독 PUT·DELETE 뒤에는 설정도 다시 읽는다. 텔레메트리 "켜기"는 입력란에 저장하지 않은 끝점이 있으면 함께 보낸다. 끝점이 없으면 "설정 안 됨 — 빌드에 기본 끝점이 있으면 그것을 씀". 설정 쓰기가 겹친 409 `busy`는 VM용 문구가 아니라 `settings.errors.busy`. 성공해서 누른 버튼이 사라지면(켜기·끄기·지우기) 포커스는 그 부분의 제목으로 간다. 만료일은 UTC 날짜다.
- 시험: opl-types 단위(키 풀기·바뀐 payload·바뀐 서명·다른 키·형식·모르는 필드·만료, 상태 표와 경계, 옛 설정 JSON 모양), `opl-api/tests/opl_license.rs`(바이너리 keygen → issue → 풀기), `tests/subscription.rs`(넣기·지우기·권한·이유·over_limit·위트니스 제외·옛 노드 409·키가 설정과 watch에 없음·enterprise 거절), `tests/updates.rs`(인증 파일 내용·0600·같으면 안 씀·지움·표시 없는 파일 그대로·에이전트가 구독을 넣으면 파일), `tests/telemetry.rs`(끝점 검사·id·reset-id·권한·옛 노드·보고 내용에 이름·주소·MAC·고객·키 없음), `tests/telemetry_reporter.rs`(시험 CA가 서명한 HTTPS 수집 서버 `tests/common/collector.rs`: 한 번 보내고 기록, 간격 뒤 다시, 500이면 `last_error`와 다시, 꺼져 있으면·옛 노드가 있으면 안 보냄, 307을 따르지 않음, 끝점이 없으면 `last_error`를 한 번), `telemetry.rs` 단위(`due`: 메모리의 전송 시각), opld 설정 단위, opld E2E `crates/opld/tests/subscription.rs`(일반 노드 둘 + 위트니스, 구독 active, 텔레메트리를 켜면 한 번 보내고 리더를 kill -9 해도 새 리더가 다시 보내지 않음, 남은 노드를 `interval = "2s"`로 다시 시작하면 위트니스와 정족수를 이뤄 두 번째 보고를 보냄), oplctl `fake_api`, 웹 vitest(상태 카드·키 넣기·지우기·읽기 전용·enterprise 조건·미리 보기 대화상자·켜기가 입력한 끝점을 보냄·배너와 그 안내만 7일 숨김·localStorage 예외·노드 이벤트에 다시 읽음·busy 문구·지우기 409·미리 보기 실패·UTC 날짜), `build-deb-test.sh`(구독 공개 키 없음, 32바이트가 아닌 항목·패딩 없는·정규형이 아닌 항목·공백이나 쉼표뿐인 값·개발 키·opld에 없는 값 거절; 사용자 셸의 `OPL_DEV_LICENSE`·`OPL_LICENSE_KEYS`를 지우고 시작).
- 알려진 틈:
  - 실패 뒤 다시 보낼 시각과 보낸 시각은 리더의 메모리에 있어, 리더가 바뀌면 새 리더가 `retry`를 기다리지 않고 곧 다시 보낼 수 있다. 보낸 뒤 `last_sent_at`을 적지 못하면 같은 리더는 메모리로 막지만, 그 사이 리더가 바뀌면 새 리더가 한 번 더 보낼 수 있다(수집 서버가 `id`·`sent_at`으로 거를 수 있다).
  - 이 빌드보다 낮은 노드가 하나라도 있으면(롤링 업그레이드 중) 구독·텔레메트리를 바꿀 수 없고 보고도 보내지 않는다.

## 5. 오류
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음(그 노드를 업그레이드하는 중의 다시 확인도) |
| `cluster_upgrading` | 409 | 버전을 적지 않은 옛 노드가 남은 클러스터에서 설정 바꾸기·다시 확인·업그레이드(계획 32 구현에서 더함) |
| `subscription_required` | 400 | 구독 없이 enterprise 채널 |
| `invalid_subscription_key` | 400 | 형식·서명·만료 |
| `telemetry_endpoint_required` | 400 | 끝점 없이 텔레메트리 켜기 |
```

을 다음으로 바꾼다:

```markdown
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음(그 노드를 업그레이드하는 중의 다시 확인도) |
| `cluster_upgrading` | 409 | 버전을 적지 않은 옛 노드가 남은 클러스터에서 설정 바꾸기·다시 확인·업그레이드(계획 32 구현에서 더함). 이 빌드보다 낮은 노드(위트니스 포함)가 남은 클러스터에서 구독 넣기·지우기, enterprise 채널, 텔레메트리 바꾸기와 `reset-id`(계획 33 구현에서 더함) |
| `subscription_required` | 400 | 구독 없이 enterprise 채널 |
| `invalid_subscription_key` | 400 | 형식·서명·만료(메시지가 이유) |
| `telemetry_endpoint_required` | 400 | 끝점 없이 텔레메트리 켜기 |
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 구독과 텔레메트리 — README 새 절 \"구독과 텔레메트리\"(기능을 막지 않는 오프라인 구독, oplctl subscription show|set|remove, 키 형식 opl1.<payload>.<signature>와 JSON 필드, 상태 none·active·expired·invalid·over_limit와 개발용 키, PUT·DELETE 권한과 invalid_subscription_key 이유, 키가 API로 나가지 않음, 웹 구독 카드와 대시보드 안내·7일 숨김, enterprise 채널의 subscription_required와 /etc/apt/auth.conf.d/oplite.conf(login=구독 id, password=키 SHA-256, https에만), 옛 노드 409 cluster_upgrading, 발급 도구 opl-license keygen|issue, OPL_LICENSE_KEYS·OPL_DEV_LICENSE와 개발 키, 텔레메트리 설정·끝점 검사·telemetry_endpoint_required·id·미리 보기, 리더의 보내기·다시 시도·HTTPS_PROXY, 보내는 내용 예와 분류 규칙, [telemetry] tick·interval·retry·ca_file, OPL_TELEMETRY_URL), 설치·저장소 예의 build-deb에 OPL_DEV_LICENSE=1·OPL_LICENSE_KEYS, 설계 §4.6 \"구현(계획 33)\"(verify·parse 나눔, 판정 순서, require_current 게이트와 이유, 새 필드는 기본값이면 직렬화하지 않음, 감사 로그 없음, enterprise·인증 파일 규칙, 텔레메트리 Patch 관례, 네트워크 분류, preview admin, 리포터 틱 순서, opld [telemetry] tick·ca_file, oplctl·웹, 시험, 알려진 틈)와 §5 cluster_upgrading 줄, 노드 코어 설계 §1.1에 계획 33 완료와 하위 프로젝트 9 끝"
```

---

## 완료 기준 (계획 33)

- `opl-license keygen`·`issue`로 만든 키를 `oplctl subscription set --file -`로 넣으면 `oplctl subscription show`가 `active`와 고객·수준·노드 수·만료일을 보이고, 바뀐 키·다른 키·만료된 키는 400 `invalid_subscription_key`다.
- 구독이 active가 아니면 `oplctl settings set updates.channel=enterprise`가 400 `subscription_required`이고, active면 각 노드가 `Components: enterprise` 소스 파일과 0600 APT 인증 파일을 쓴다. 채널을 바꾸거나 구독을 지우면 인증 파일이 지워진다.
- `oplctl telemetry preview`가 이름·주소·키 없이 수만 담은 JSON을 보이고, 켜면 리더가 한 번 보내며 리더가 바뀌어도 `interval` 전에는 다시 보내지 않는다(opld E2E).
- 계획 32 노드가 남은 클러스터에서는 구독·enterprise·텔레메트리 쓰기가 409 `cluster_upgrading`이고, 채널만 바꾸는 쓰기는 그대로 된다.
- 웹 설정 페이지의 구독·채널·텔레메트리 부분과 대시보드 구독 배너.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`, 셸 시험과 shellcheck가 통과한다.

## 알고 받아들인 제한

- 구독 키를 여러 클러스터에 써도 막지 못한다(오프라인 검증). 소스에서 빌드하면 자기 공개 키를 넣을 수 있다 — 구독은 기능 잠금이 아니므로 의도한 것이다.
- 텔레메트리 실패 뒤 다시 보낼 시각과 보낸 기록은 리더 메모리에도 있다. 리더가 바뀌거나 보낸 뒤 상태를 쓰기 전에 리더가 죽으면 한 번 더 보낼 수 있다.
- 웹의 구독 상태는 설정 이벤트·노드 목록 변화·1시간 주기로 다시 읽는다.
- 새 구독 수준(level)은 옛 빌드에서 키 전체가 invalid가 되므로 새 접두(`opl2`)로 낸다.
- 그 밖의 제한은 설계 문서 §4.6과 README "구독과 텔레메트리" 절에 있다.

## 실행 후 기록 (2026-11-06)

7개 Task를 계획대로 적용했다. Task마다 검증된 단계와 트리가 같았다. 생성 파일 `Cargo.lock`·`web/src/api/schema.d.ts`도 같았다. 실행 전에 계획 문서를 처음부터 끝까지 main 복제본에 적용해, 각 단계의 트리와 같음을 확인했다. 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 두 차례 돌렸고, CI 프로브는 첫 번째에 모두 통과했다. 고친 내용은 위 Task의 코드에 이미 들어 있다.

- **1차 리뷰에서 고친 것 (27가지):**
  - 구독 키:
    - id는 영문자·숫자·`.`·`_`·`-`만 쓴다. APT 인증 파일의 login 줄이 깨지지 않게 하려는 것이다.
    - 만료 시각의 상한은 9999년이다.
    - `OPL_LICENSE_KEYS`에 든 개발 키도 개발용으로 표시한다.
    - `opl-license`의 인자 검사와 문구를 고치고 `pubkey`를 더했다.
  - 패키지: `build-deb.sh`가 정식 공개 키를 셋 다 확인한다 — 32바이트 base64인지, 개발 키가 아닌지, opld 바이너리에 실제로 들었는지.
  - 텔레메트리:
    - 리다이렉트를 따르지 않고 https만 쓴다.
    - 끝점에 사용자 정보를 넣지 못한다.
    - 끝점이 없으면 `last_error`를 적는다.
    - 보낸 뒤 상태 쓰기가 실패해도 다시 보내지 않게, 리더 메모리에 보낸 시각을 둔다.
    - opld E2E가 새 리더가 실제로 보내는지도 본다.
  - oplctl `subscription set --file -`(표준 입력). 키를 명령줄에 두지 않게 하려는 것이다.
  - 웹:
    - 배너는 닫은 그 안내만 숨긴다.
    - 구독 상태를 노드 변화와 1시간 주기로 다시 읽는다.
    - 구독을 바꾸면 설정도 다시 읽는다.
    - 설정 `busy` 문구, 포커스, UTC 날짜, 오류 경로 시험을 고쳤다.
  - 문서를 고쳤다.
- **2차 리뷰에서 고친 것:**
  - `build-deb.sh`가 정규 base64(44자, `=`로 끝남)만 받고, 받아들인 키가 없으면(공백·쉼표뿐) 거절한다. GNU `base64 -d`는 패딩 없는 값도 풀기 때문이다.
  - opld E2E가 바꿀 설정 문자열이 있는지 먼저 확인한다.
- **실행 중에 고친 것 (Task 5):**
  - 이 Mac의 저장소 경로에서는 시험 CA로 하는 첫 TLS 연결이 15초쯤 걸렸다(스크래치 복제본에서는 그렇지 않았다). 그래서 리포터 시험 6개가 시간 안에 끝나지 않았다.
  - 시험을 다중 스레드 런타임으로 바꿨다. 보내기 시간 제한은 운영과 같은 30초, 기다리는 한도는 60초다.
  - 실패 뒤 다시 보낼 시각을 보내기를 시작한 때가 아니라 실패한 때부터 세도록 코드를 고쳤다. 보내기가 오래 걸리면 곧바로 다시 보내던 문제다.
  - 계획 문서도 같이 고쳤다.
- **불안정한 시험(부하 때, 다시 돌리면 통과):**
  - opl-api `updates.rs` a_failed_upgrade_fails_the_task_with_the_last_log_lines
  - opld `cluster.rs`의 두 시험("API did not start")

**테스트:**
- macOS `cargo test --workspace` 1249개
- 웹 시험 494개와 `typecheck`·`lint`·`build`
- 셸 시험 `maint-test.sh`·`build-deb-test.sh`·`build-repo-test.sh`·`setup-test.sh`·`iso-test.sh`와 shellcheck. `build-deb-test.sh`는 GNU `base64`로도 돌렸다.
- `fmt`·`clippy -D warnings`(모든 기능)
- 정식 공개 키를 넣은 release opld에 그 문자열이 들어 있음을 확인했다. `build-deb.sh`가 바이너리를 확인하는 근거다.

Linux CI(프로브)에서는 다음이 통과했다.
- 위 시험 전부. 리포터 시험과 opld E2E도 Linux에서 통과했다.
- Debian 13 컨테이너 패키지 시험
- BIOS·UEFI ISO 무인 설치 E2E

## 다음 계획으로 넘길 사항

- 하위 프로젝트 9(설치·업데이트·구독)가 이 계획으로 끝났다. 처음 분해의 하위 프로젝트는 모두 끝났다. 다음 일은 노드 코어 설계 §1.1과 각 설계 문서의 "알려진 틈"에서 고른다.
- 정식 빌드를 하려면 다음이 필요하다.
  - `opl-license keygen`으로 키 쌍을 만들어 비밀 키를 따로 보관한다.
  - `OPL_LICENSE_KEYS`를 `cargo build`와 `build-deb.sh`에 같은 값으로 준다.
  - 필요하면 `OPL_APT_URL`·`OPL_APT_KEY`·`OPL_TELEMETRY_URL`도 준다.
- enterprise 저장소의 인증 확인(auth.conf의 id·키 SHA-256)과 텔레메트리 수집 서버는 opLite 밖이다.
