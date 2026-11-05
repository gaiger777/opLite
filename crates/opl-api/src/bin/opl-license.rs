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
