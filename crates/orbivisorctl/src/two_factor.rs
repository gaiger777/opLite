//! 2단계 인증(계획 62 설계 §10): `orbivisorctl 2fa …`(상태, TOTP 켜기·끄기, 복구 코드 다시 만들기)와 `login`의 둘째
//! 단계(TOTP·복구 코드, 정책이 요구하는데 요소가 없으면 TOTP 강제 등록). 패스키는 웹 전용이다.
//!
//! 코드는 `--code`로 받거나 터미널에서 묻는다. 터미널이 아니면 묻지 않고 실패한다(스크립트가 멈추지 않게).

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Subcommand};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{ApiError, Client};
use crate::output::{cell, json as print_json};

/// 터미널에서 한 줄을 묻는 함수(시험은 미리 정한 답을 준다)
pub(crate) type Ask<'a> = &'a mut (dyn FnMut(&str) -> anyhow::Result<String> + Send);

/// 표준 입력에서 한 줄을 묻는다. 터미널이 아니면 `--code`를 쓰라고 실패한다.
pub(crate) fn ask_terminal(prompt: &str) -> anyhow::Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        bail!("a two-factor code is needed; give --code");
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// 틀린 코드를 다시 묻는 횟수(터미널에서 물을 때만)
const TRIES: usize = 3;

/// 코드 하나를 둘째 단계 본문으로: 숫자 6~8자리는 TOTP, 그 밖은 복구 코드(`xxxx-xxxx-xxxx-xxxx`)
pub(crate) fn code_body(code: &str) -> Value {
    let compact: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if (6..=8).contains(&compact.len()) && compact.bytes().all(|b| b.is_ascii_digit()) {
        json!({ "totp": compact })
    } else {
        json!({ "recovery": code.trim() })
    }
}

fn is_code(e: &anyhow::Error, code: &str) -> bool {
    e.downcast_ref::<ApiError>().is_some_and(|a| a.code == code)
}

/// 대기 세션 쿠키를 붙여 보낸다.
async fn send_with_cookie(
    anon: &Client,
    cookie: &str,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> anyhow::Result<Value> {
    let mut r = anon
        .request(method, path)
        .header(reqwest::header::COOKIE, cookie);
    if let Some(body) = body {
        r = r.json(body);
    }
    anon.send(r).await
}

/// 복구 코드를 한 번 보인다.
fn print_recovery(err: &mut dyn Write, codes: &Value) -> std::io::Result<()> {
    let codes: Vec<String> = codes.as_array().into_iter().flatten().map(cell).collect();
    if codes.is_empty() {
        return Ok(());
    }
    writeln!(
        err,
        "recovery codes (shown only once; each signs you in once if you lose the authenticator):"
    )?;
    for c in codes {
        writeln!(err, "  {c}")?;
    }
    Ok(())
}

/// 로그인의 둘째 단계. `prompt`는 로그인 응답의 `second_factor`, `cookie`는 대기 세션이다. 끝나면 같은 쿠키가 정식
/// 세션이 된다.
pub(crate) async fn second_step(
    anon: &Client,
    cookie: &str,
    prompt: &Value,
    code: Option<&str>,
    ask: Ask<'_>,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if prompt["enroll"] == true {
        return enroll(anon, cookie, code, ask, err).await;
    }
    let methods: Vec<&str> = prompt["methods"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !methods.iter().any(|m| *m == "totp" || *m == "recovery") {
        bail!(
            "this account signs in with a passkey; sign in to the web UI, create an API token there and run `orbivisorctl login --token-file`"
        );
    }
    let tries = if code.is_some() { 1 } else { TRIES };
    for left in (0..tries).rev() {
        let code = match code {
            Some(c) => c.to_string(),
            None => ask("Authenticator code (or a recovery code): ")?,
        };
        let r = send_with_cookie(
            anon,
            cookie,
            Method::POST,
            "/auth/login/second-factor",
            Some(&code_body(&code)),
        )
        .await;
        match r {
            Ok(_) => return Ok(()),
            Err(e) if left > 0 && is_code(&e, "invalid_second_factor") => {
                writeln!(err, "wrong code, try again")?;
            }
            Err(e) => return Err(e.context("second sign-in step failed")),
        }
    }
    unreachable!("the last try returns")
}

/// 정책이 2단계 인증을 요구하는데 요소가 없다: TOTP를 등록하면 로그인이 끝난다.
async fn enroll(
    anon: &Client,
    cookie: &str,
    code: Option<&str>,
    ask: Ask<'_>,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if code.is_some() {
        bail!(
            "two-factor authentication is required but not set up for this account; run `orbivisorctl login` in a terminal (without --code) to set it up"
        );
    }
    writeln!(
        err,
        "two-factor authentication is required for this account; add it to an authenticator app"
    )?;
    let start = send_with_cookie(anon, cookie, Method::POST, "/auth/2fa/totp", None).await?;
    writeln!(err, "  secret: {}", cell(&start["secret"]))?;
    writeln!(err, "  URI:    {}", cell(&start["uri"]))?;
    for left in (0..TRIES).rev() {
        let code = ask("Code from the app: ")?;
        let r = send_with_cookie(
            anon,
            cookie,
            Method::POST,
            "/auth/2fa/totp/confirm",
            Some(&json!({ "code": code })),
        )
        .await;
        match r {
            Ok(done) => {
                print_recovery(err, &done["codes"])?;
                return Ok(());
            }
            Err(e) if left > 0 && is_code(&e, "invalid_second_factor") => {
                writeln!(err, "wrong code, try again")?;
            }
            Err(e) => return Err(e.context("two-factor setup failed")),
        }
    }
    unreachable!("the last try returns")
}

// ---- 2fa 명령 ----

/// 요소를 빼거나 복구 코드를 다시 만들 때의 본인 확인. 둘 다 없으면 코드를 묻는다.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("proof").args(["code", "password_file"])))]
pub(crate) struct ProofArgs {
    /// 인증 앱의 코드나 복구 코드
    #[arg(long)]
    code: Option<String>,
    /// 로컬 사용자의 비밀번호가 든 파일
    #[arg(long)]
    password_file: Option<PathBuf>,
}

impl ProofArgs {
    fn body(&self, ask: Ask<'_>) -> anyhow::Result<Value> {
        Ok(match (&self.code, &self.password_file) {
            (Some(code), _) => json!({ "code": code }),
            (_, Some(file)) => {
                let password = std::fs::read_to_string(file)
                    .with_context(|| format!("cannot read {}", file.display()))?;
                json!({ "password": password.trim_end_matches(['\r', '\n']) })
            }
            _ => json!({ "code": ask("Authenticator code (or a recovery code): ")? }),
        })
    }
}

#[derive(Subcommand)]
pub(crate) enum TwoFactorCmd {
    /// 내 2단계 인증 상태(TOTP, 남은 복구 코드, 패스키, 정책이 요구하는지)
    Status,
    Totp {
        #[command(subcommand)]
        cmd: TotpCmd,
    },
    Recovery {
        #[command(subcommand)]
        cmd: RecoveryCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum TotpCmd {
    /// TOTP를 켠다: 비밀과 `otpauth://` URI를 보이고 앱의 코드를 물어 확인한다. 첫 요소면 복구 코드를 한 번 보인다.
    Enable {
        /// 비밀과 URI만 보이고 확인은 `2fa totp confirm`으로 한다.
        #[arg(long)]
        no_confirm: bool,
    },
    /// `2fa totp enable --no-confirm` 뒤 앱의 코드로 확인한다.
    Confirm { code: String },
    /// TOTP를 끈다(코드·복구 코드나 비밀번호로 확인).
    Disable {
        #[command(flatten)]
        proof: ProofArgs,
    },
}

#[derive(Subcommand)]
pub(crate) enum RecoveryCmd {
    /// 복구 코드를 새로 만든다(남은 코드는 버린다).
    Regen {
        #[command(flatten)]
        proof: ProofArgs,
    },
}

fn status_lines(v: &Value) -> Vec<String> {
    let yes = |b: &Value| if *b == true { "yes" } else { "no" };
    let totp = if v["totp"] == true {
        "on"
    } else if v["totp_pending"] == true {
        "waiting for confirmation"
    } else {
        "off"
    };
    let mut lines = vec![
        format!("totp: {totp}"),
        format!("recovery codes left: {}", cell(&v["recovery_codes"])),
        format!("required by policy: {}", yes(&v["required"])),
    ];
    for p in v["passkeys"].as_array().into_iter().flatten() {
        lines.push(format!(
            "passkey: {} ({}, {})",
            cell(&p["name"]),
            cell(&p["rp_id"]),
            cell(&p["id"])
        ));
    }
    if v["available"] == false {
        lines.push("not available until every node is upgraded".into());
    }
    lines
}

pub(crate) async fn two_factor(
    client: &Client,
    cmd: TwoFactorCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    ask: Ask<'_>,
) -> anyhow::Result<()> {
    match cmd {
        TwoFactorCmd::Status => {
            let v = client.get("/auth/2fa").await?;
            if json_out {
                print_json(out, &v)?;
            } else {
                for line in status_lines(&v) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        TwoFactorCmd::Totp {
            cmd: TotpCmd::Enable { no_confirm },
        } => {
            let start = client.post("/auth/2fa/totp", &json!({})).await?;
            if json_out && no_confirm {
                return Ok(print_json(out, &start)?);
            }
            writeln!(out, "secret: {}", cell(&start["secret"]))?;
            writeln!(out, "URI:    {}", cell(&start["uri"]))?;
            if no_confirm {
                writeln!(out, "confirm with `orbivisorctl 2fa totp confirm CODE`")?;
                return Ok(());
            }
            let code = ask("Code from the app: ")?;
            totp_confirm(client, &code, json_out, out).await?;
        }
        TwoFactorCmd::Totp {
            cmd: TotpCmd::Confirm { code },
        } => totp_confirm(client, &code, json_out, out).await?,
        TwoFactorCmd::Totp {
            cmd: TotpCmd::Disable { proof },
        } => {
            let r = client
                .request(Method::DELETE, "/auth/2fa/totp")
                .json(&proof.body(ask)?);
            client.send(r).await?;
            writeln!(out, "totp turned off")?;
        }
        TwoFactorCmd::Recovery {
            cmd: RecoveryCmd::Regen { proof },
        } => {
            let done = client.post("/auth/2fa/recovery", &proof.body(ask)?).await?;
            if json_out {
                print_json(out, &done)?;
            } else {
                print_recovery(out, &done["codes"])?;
            }
        }
    }
    Ok(())
}

async fn totp_confirm(
    client: &Client,
    code: &str,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let done = client
        .post("/auth/2fa/totp/confirm", &json!({ "code": code }))
        .await?;
    if json_out {
        print_json(out, &done)?;
    } else {
        writeln!(out, "totp turned on")?;
        print_recovery(out, &done["codes"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn codes_pick_the_method() {
        assert_eq!(code_body("123 456"), json!({ "totp": "123456" }));
        assert_eq!(code_body("12345678"), json!({ "totp": "12345678" }));
        assert_eq!(
            code_body(" abcd-efgh-ijkl-mnop "),
            json!({ "recovery": "abcd-efgh-ijkl-mnop" })
        );
        assert_eq!(code_body("12345"), json!({ "recovery": "12345" }));
    }

    #[test]
    fn status_reads_the_view() {
        let v = json!({
            "totp": false, "totp_pending": true, "recovery_codes": 0, "required": true,
            "passkeys": [{ "id": "k1", "name": "laptop", "rp_id": "opl.example.com" }],
            "available": false
        });
        assert_eq!(
            status_lines(&v),
            [
                "totp: waiting for confirmation",
                "recovery codes left: 0",
                "required by policy: yes",
                "passkey: laptop (opl.example.com, k1)",
                "not available until every node is upgraded"
            ]
        );
    }

    /// 틀린 코드는 다시 묻고, 맞으면 끝난다. 쿠키는 매 요청에 붙는다.
    #[tokio::test]
    async fn second_step_retries_a_wrong_code() {
        let tries = Arc::new(Mutex::new(0));
        let t = tries.clone();
        let (client, seen) = fake_api(move |_, _| {
            let mut n = t.lock().unwrap();
            *n += 1;
            if *n == 1 {
                (
                    401,
                    json!({ "code": "invalid_second_factor", "message": "wrong code" }),
                )
            } else {
                (200, json!({ "name": "alice" }))
            }
        })
        .await;
        let mut answers = vec!["abcd-efgh-ijkl-mnop".to_string(), "111111".to_string()];
        let mut ask = |_: &str| -> anyhow::Result<String> { Ok(answers.pop().unwrap()) };
        let mut err = Vec::new();
        let prompt = json!({ "methods": ["totp", "recovery"], "enroll": false });
        second_step(&client, "opl_session=x", &prompt, None, &mut ask, &mut err)
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].body, json!({ "totp": "111111" }));
        assert_eq!(seen[1].body, json!({ "recovery": "abcd-efgh-ijkl-mnop" }));
        assert!(
            seen[0]
                .line
                .starts_with("POST /api/v1/auth/login/second-factor")
        );
        assert_eq!(String::from_utf8(err).unwrap(), "wrong code, try again\n");
    }

    /// `--code`로 준 코드는 한 번만 쓴다. 패스키만 있으면 웹으로 보낸다.
    #[tokio::test]
    async fn second_step_with_a_given_code_tries_once() {
        let (client, seen) = fake_api(|_, _| {
            (
                401,
                json!({ "code": "invalid_second_factor", "message": "wrong code" }),
            )
        })
        .await;
        let mut ask = |_: &str| -> anyhow::Result<String> { panic!("asked") };
        let prompt = json!({ "methods": ["totp"], "enroll": false });
        let e = second_step(
            &client,
            "c",
            &prompt,
            Some("123456"),
            &mut ask,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("wrong code (invalid_second_factor)"),
            "{e:#}"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
        let prompt = json!({ "methods": ["webauthn"], "enroll": false });
        let e = second_step(&client, "c", &prompt, None, &mut ask, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("--token-file"), "{e}");
    }

    /// 강제 등록: 비밀을 보이고 앱 코드로 확인한 뒤 복구 코드를 보인다.
    #[tokio::test]
    async fn enrollment_sets_up_totp() {
        let (client, seen) = fake_api(|_, path| {
            if path.ends_with("/confirm") {
                (200, json!({ "codes": ["aaaa-bbbb-cccc-dddd"] }))
            } else {
                (
                    200,
                    json!({ "secret": "SECRET", "uri": "otpauth://totp/x" }),
                )
            }
        })
        .await;
        let mut ask = |_: &str| -> anyhow::Result<String> { Ok("654321".to_string()) };
        let mut err = Vec::new();
        let prompt = json!({ "methods": [], "enroll": true });
        second_step(&client, "c", &prompt, None, &mut ask, &mut err)
            .await
            .unwrap();
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.contains("secret: SECRET\n  URI:    otpauth://totp/x\n"),
            "{err}"
        );
        assert!(err.ends_with("  aaaa-bbbb-cccc-dddd\n"), "{err}");
        {
            let seen = seen.lock().unwrap();
            assert!(seen[0].line.starts_with("POST /api/v1/auth/2fa/totp "));
            assert_eq!(seen[1].body, json!({ "code": "654321" }));
        }
        // `--code`로는 등록할 수 없다(비밀을 아직 모른다).
        let mut ask = |_: &str| -> anyhow::Result<String> { panic!("asked") };
        let e = second_step(&client, "c", &prompt, Some("1"), &mut ask, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("without --code"), "{e}");
    }

    #[tokio::test]
    async fn commands_send_the_proof() {
        let (client, seen) = fake_api(|_, path| {
            if path.ends_with("/recovery") {
                (200, json!({ "codes": ["a", "b"] }))
            } else {
                (
                    200,
                    json!({ "secret": "S", "uri": "otpauth://u", "codes": [] }),
                )
            }
        })
        .await;
        let mut ask = |_: &str| -> anyhow::Result<String> { Ok("222333".to_string()) };
        let mut out = Vec::new();
        two_factor(
            &client,
            TwoFactorCmd::Totp {
                cmd: TotpCmd::Disable {
                    proof: ProofArgs {
                        code: None,
                        password_file: None,
                    },
                },
            },
            false,
            &mut out,
            &mut ask,
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        two_factor(
            &client,
            TwoFactorCmd::Recovery {
                cmd: RecoveryCmd::Regen {
                    proof: ProofArgs {
                        code: Some("x".into()),
                        password_file: None,
                    },
                },
            },
            false,
            &mut out,
            &mut ask,
        )
        .await
        .unwrap();
        assert!(String::from_utf8(out).unwrap().ends_with("  a\n  b\n"));
        let mut out = Vec::new();
        two_factor(
            &client,
            TwoFactorCmd::Totp {
                cmd: TotpCmd::Enable { no_confirm: false },
            },
            false,
            &mut out,
            &mut ask,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "secret: S\nURI:    otpauth://u\ntotp turned on\n"
        );
        let seen = seen.lock().unwrap();
        assert!(seen[0].line.starts_with("DELETE /api/v1/auth/2fa/totp "));
        assert_eq!(seen[0].body, json!({ "code": "222333" }));
        assert_eq!(seen[1].body, json!({ "code": "x" }));
        assert_eq!(seen[3].body, json!({ "code": "222333" }));
    }
}
