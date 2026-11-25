//! `orbivisorctl cert …`(계획 73 §13): ACME 계정(`cert account`)·DNS-01 공급자(`cert dns`)·인증서(`cert add|list|get|renew|
//! delete`). 비밀(EAB HMAC 키·DNS 공급자 비밀)은 인자로 받지 않는다 — 파일·환경 변수·터미널에서.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Map, Value, json};

use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum CertCmd {
    /// ACME 계정(`cluster.admin`): 더하기(CA에 등록)·목록·보기·다시 확인·지우기
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
    /// DNS-01 공급자(`cluster.admin`): 더하기·목록·보기·지우기
    Dns {
        #[command(subcommand)]
        cmd: DnsCmd,
    },
    /// 인증서를 더한다. 리더가 다음 틱(60초)에 주문한다(`cert renew`로 곧바로).
    Add(Box<AddArgs>),
    /// 인증서 표(`NAME SCOPE CHALLENGE NODE SERIAL EXPIRES RENEW ERROR`)
    List,
    Get {
        cert: String,
    },
    /// 지금 주문한다(물러서기를 지운다).
    Renew {
        cert: String,
        /// 주문 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    Delete {
        cert: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccountCmd {
    /// 계정을 만들고 CA에 등록한다(계정 키는 서버가 만든다).
    Add {
        name: String,
        /// 디렉터리 URL 또는 `letsencrypt`·`letsencrypt-staging`·`zerossl`·`google`
        #[arg(long)]
        directory: String,
        #[arg(long)]
        email: Option<String>,
        /// CA 약관에 동의한다(필수).
        #[arg(long, required = true)]
        agree_tos: bool,
        /// 외부 계정 묶기 키 id(ZeroSSL·Google·사내 CA)
        #[arg(long, requires = "eab_hmac_file")]
        eab_kid: Option<String>,
        /// EAB HMAC 키(base64url)가 든 파일(`-`이면 `ORBIVISOR_ACME_EAB_HMAC`)
        #[arg(long, value_name = "FILE", requires = "eab_kid")]
        eab_hmac_file: Option<PathBuf>,
        /// 사내 ACME의 루트(PEM 파일)
        #[arg(long, value_name = "FILE")]
        ca_bundle: Option<PathBuf>,
    },
    List,
    Get {
        account: String,
    },
    /// CA에 다시 묻는다.
    Check {
        account: String,
    },
    Delete {
        account: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum ProviderKind {
    Cloudflare,
    Route53,
    Rfc2136,
    Pdns,
    Hook,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Tsig {
    HmacSha256,
    HmacSha512,
}

/// `cert dns add`의 인자
#[derive(Args)]
pub(crate) struct DnsAddArgs {
    name: String,
    #[arg(long, value_enum)]
    provider: ProviderKind,
    /// 비밀(Cloudflare API 토큰·Route 53 비밀 키·TSIG 비밀(base64)·PowerDNS API 키)이 든 파일. 없으면
    /// `ORBIVISOR_ACME_DNS_SECRET`, 그것도 없으면 터미널에서 묻는다(훅은 비밀이 없다).
    #[arg(long, value_name = "FILE")]
    secret_file: Option<PathBuf>,
    /// Cloudflare 영역 id(없으면 이름으로 찾는다)
    #[arg(long)]
    zone_id: Option<String>,
    /// Route 53 접근 키 id
    #[arg(long)]
    access_key_id: Option<String>,
    #[arg(long)]
    hosted_zone_id: Option<String>,
    #[arg(long)]
    region: Option<String>,
    /// RFC 2136 서버(`ip:53`)
    #[arg(long)]
    server: Option<String>,
    /// RFC 2136 TSIG 키 이름
    #[arg(long)]
    key_name: Option<String>,
    #[arg(long, value_enum)]
    algorithm: Option<Tsig>,
    /// RFC 2136 영역(없으면 SOA로 찾는다)
    #[arg(long)]
    zone: Option<String>,
    /// PowerDNS API 주소
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    server_id: Option<String>,
    /// 훅 실행 파일(`/etc/orbivisor/acme-hooks/` 아래)
    #[arg(long)]
    path: Option<String>,
    /// 이 공급자가 맡는 영역(여러 번)
    #[arg(long = "zones", value_name = "ZONE")]
    zones: Vec<String>,
    /// 전파를 기다리는 한도(초, 기본 180)
    #[arg(long)]
    propagation_timeout: Option<u32>,
    /// 권한 서버에 묻지 않는다.
    #[arg(long)]
    no_propagation_check: bool,
}

#[derive(Subcommand)]
pub(crate) enum DnsCmd {
    Add(Box<DnsAddArgs>),
    List,
    Get { dns: String },
    Delete { dns: String },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum KeyKind {
    P256,
    P384,
}

/// `cert add`의 인자
#[derive(Args)]
pub(crate) struct AddArgs {
    name: String,
    /// ACME 계정
    #[arg(long)]
    account: String,
    /// 인증서 이름(여러 번). 노드마다면 `{node}`·`{node_address}`를 쓸 수 있고, `*.`는 DNS-01만
    #[arg(long = "name", value_name = "NAME", required = true)]
    names: Vec<String>,
    /// 노드마다 따로 받는다(기본 — 키는 그 노드 디스크에만).
    #[arg(long, conflicts_with = "shared")]
    per_node: bool,
    /// 한 번 받아 모든 노드가 같은 인증서를 낸다(VIP 이름).
    #[arg(long)]
    shared: bool,
    /// 노드마다 인증서를 받을 노드(id, 여러 번 — 없으면 모두)
    #[arg(long = "node", value_name = "NODE", conflicts_with = "shared")]
    nodes: Vec<String>,
    /// 포트 80(`[api] http_listen`)으로 답한다(기본).
    #[arg(long, conflicts_with = "dns01")]
    http01: bool,
    /// TXT로 답한다 — DNS 공급자 이름
    #[arg(long, value_name = "DNS")]
    dns01: Option<String>,
    #[arg(long, value_enum)]
    key: Option<KeyKind>,
    /// ACME 프로필(`classic`·`tlsserver`·`shortlived`)
    #[arg(long)]
    profile: Option<String>,
    /// 만료 N일 전에 갱신한다(없으면 CA 갱신 창 — ARI, 없으면 유효 기간 1/3)
    #[arg(long, value_name = "DAYS")]
    days_before: Option<u16>,
    /// 고를 사슬의 루트 CN
    #[arg(long)]
    preferred_chain: Option<String>,
    /// 받기만 하고 API 리스너에 끼우지 않는다.
    #[arg(long)]
    no_install: bool,
    /// HTTP-01 자기 확인을 건너뛴다.
    #[arg(long)]
    no_precheck: bool,
}

/// 비밀: 파일 > 환경 변수(`ORBIVISOR_{env}`, 옛 이름 `OPL_{env}`) > 터미널
fn secret(file: Option<&PathBuf>, env: &str, prompt: &str) -> anyhow::Result<String> {
    if let Some(f) = file.filter(|f| f.as_os_str() != "-") {
        return Ok(std::fs::read_to_string(f)
            .with_context(|| format!("cannot read {}", f.display()))?
            .trim()
            .to_string());
    }
    match orbivisor_types::legacy::env(env) {
        Some(v) => Ok(v),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn need(v: &Option<String>, flag: &str) -> anyhow::Result<String> {
    v.clone()
        .ok_or_else(|| anyhow::anyhow!("--{flag} is required for this provider"))
}

/// `cert dns add`의 공급자 본문
pub(crate) fn provider_body(
    a: &DnsAddArgs,
    secret: impl Fn() -> anyhow::Result<String>,
) -> anyhow::Result<Value> {
    let mut p = Map::new();
    let put = |p: &mut Map<String, Value>, k: &str, v: &Option<String>| {
        if let Some(v) = v {
            p.insert(k.into(), json!(v));
        }
    };
    match a.provider {
        ProviderKind::Cloudflare => {
            p.insert("kind".into(), json!("cloudflare"));
            p.insert("api_token".into(), json!(secret()?));
            put(&mut p, "zone_id", &a.zone_id);
        }
        ProviderKind::Route53 => {
            p.insert("kind".into(), json!("route53"));
            p.insert(
                "access_key_id".into(),
                json!(need(&a.access_key_id, "access-key-id")?),
            );
            p.insert("secret_access_key".into(), json!(secret()?));
            put(&mut p, "hosted_zone_id", &a.hosted_zone_id);
            put(&mut p, "region", &a.region);
        }
        ProviderKind::Rfc2136 => {
            p.insert("kind".into(), json!("rfc2136"));
            p.insert("server".into(), json!(need(&a.server, "server")?));
            p.insert("key_name".into(), json!(need(&a.key_name, "key-name")?));
            if let Some(alg) = a.algorithm {
                p.insert(
                    "algorithm".into(),
                    json!(match alg {
                        Tsig::HmacSha256 => "hmac-sha256",
                        Tsig::HmacSha512 => "hmac-sha512",
                    }),
                );
            }
            p.insert("secret".into(), json!(secret()?));
            put(&mut p, "zone", &a.zone);
        }
        ProviderKind::Pdns => {
            p.insert("kind".into(), json!("power_dns"));
            p.insert("url".into(), json!(need(&a.url, "url")?));
            p.insert("api_key".into(), json!(secret()?));
            put(&mut p, "server_id", &a.server_id);
        }
        ProviderKind::Hook => {
            p.insert("kind".into(), json!("hook"));
            p.insert("path".into(), json!(need(&a.path, "path")?));
        }
    }
    let mut body = json!({ "name": a.name, "provider": p });
    if !a.zones.is_empty() {
        body["zones"] = json!(a.zones);
    }
    if a.propagation_timeout.is_some() || a.no_propagation_check {
        body["propagation"] = json!({
            "timeout_s": a.propagation_timeout.unwrap_or(180),
            "check": if a.no_propagation_check { "skip" } else { "authoritative" },
        });
    }
    Ok(body)
}

/// `cert add`의 본문(준 것만)
pub(crate) fn certificate_body(a: &AddArgs) -> Value {
    let mut b = json!({ "name": a.name, "account": a.account, "names": a.names });
    b["scope"] = if a.shared {
        json!({ "kind": "shared" })
    } else if a.nodes.is_empty() {
        json!({ "kind": "per_node" })
    } else {
        json!({ "kind": "per_node", "nodes": a.nodes })
    };
    b["challenge"] = match &a.dns01 {
        Some(dns) => json!({ "kind": "dns01", "dns": dns }),
        None => json!({ "kind": "http01" }),
    };
    if let Some(k) = a.key {
        b["key"] = json!(match k {
            KeyKind::P256 => "ecdsa-p256",
            KeyKind::P384 => "ecdsa-p384",
        });
    }
    if let Some(p) = &a.profile {
        b["profile"] = json!(p);
    }
    if let Some(d) = a.days_before {
        b["renew"] = json!({ "kind": "days_before", "days": d });
    }
    if let Some(c) = &a.preferred_chain {
        b["preferred_chain"] = json!(c);
    }
    if a.no_install {
        b["install"] = json!("none");
    }
    if a.no_precheck {
        b["precheck"] = json!(false);
    }
    b
}

fn when(v: &Value) -> String {
    v.as_i64()
        .and_then(|s| time::OffsetDateTime::from_unix_timestamp(s).ok())
        .map(|t| {
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                t.year(),
                u8::from(t.month()),
                t.day(),
                t.hour(),
                t.minute()
            )
        })
        .unwrap_or_else(|| "-".into())
}

/// 인증서 표의 줄들(노드마다면 노드마다 한 줄, 받은 것이 없으면 한 줄)
pub(crate) fn rows(c: &Value) -> Vec<Vec<String>> {
    let scope = cell(&c["spec"]["scope"]["kind"]).replace('_', "-");
    let challenge = match c["spec"]["challenge"]["kind"].as_str() {
        Some("dns01") => format!("dns-01 {}", cell(&c["spec"]["challenge"]["dns"])),
        _ => "http-01".into(),
    };
    let error = c["status"]["error"].as_str().unwrap_or("").to_string();
    let issued: Vec<(String, &Value)> = match c["status"]["nodes"].as_object() {
        Some(nodes) if !nodes.is_empty() => nodes.iter().map(|(n, i)| (n.clone(), i)).collect(),
        _ => c["status"]
            .get("shared")
            .map(|s| vec![("shared".to_string(), s)])
            .unwrap_or_default(),
    };
    let name = cell(&c["name"]);
    if issued.is_empty() {
        return vec![vec![
            name,
            scope,
            challenge,
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
            error,
        ]];
    }
    issued
        .into_iter()
        .map(|(node, i)| {
            vec![
                name.clone(),
                scope.clone(),
                challenge.clone(),
                node,
                cell(&i["serial"]),
                when(&i["not_after"]),
                when(&i["renew_at"]),
                i["error"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| error.clone()),
            ]
        })
        .collect()
}

pub(crate) async fn cert(
    client: &Client,
    cmd: CertCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        CertCmd::Account { cmd } => account(client, cmd, json_out, out).await,
        CertCmd::Dns { cmd } => dns(client, cmd, json_out, out).await,
        CertCmd::Add(a) => {
            let got = client.post("/certificates", &certificate_body(&a)).await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            writeln!(
                out,
                "certificate {} added; the leader orders it within a minute (or run `cert renew {}`)",
                cell(&got["name"]),
                cell(&got["name"])
            )?;
            Ok(())
        }
        CertCmd::List => {
            let got = client.get("/certificates").await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            let rows: Vec<Vec<String>> = got["items"]
                .as_array()
                .map(|a| a.iter().flat_map(rows).collect())
                .unwrap_or_default();
            Ok(table(
                out,
                &[
                    "NAME",
                    "SCOPE",
                    "CHALLENGE",
                    "NODE",
                    "SERIAL",
                    "EXPIRES",
                    "RENEW",
                    "ERROR",
                ],
                &rows,
            )?)
        }
        CertCmd::Get { cert } => {
            let got = client.get(&format!("/certificates/{}", seg(&cert))).await?;
            Ok(print_json(out, &got)?)
        }
        CertCmd::Renew { cert, wait } => {
            let got = client
                .post(
                    &format!("/certificates/{}/actions/renew", seg(&cert)),
                    &json!({}),
                )
                .await?;
            let tasks: Vec<Value> = got["tasks"].as_array().cloned().unwrap_or_default();
            if wait {
                for t in &tasks {
                    client.wait_task(t).await?;
                }
                let c = client.get(&format!("/certificates/{}", seg(&cert))).await?;
                if json_out {
                    return Ok(print_json(out, &c)?);
                }
                return Ok(table(
                    out,
                    &[
                        "NAME",
                        "SCOPE",
                        "CHALLENGE",
                        "NODE",
                        "SERIAL",
                        "EXPIRES",
                        "RENEW",
                        "ERROR",
                    ],
                    &rows(&c),
                )?);
            }
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            let ids: Vec<String> = tasks.iter().map(|t| cell(&t["id"])).collect();
            writeln!(
                out,
                "certificate {cert}: ordering (task {})",
                ids.join(", ")
            )?;
            Ok(())
        }
        CertCmd::Delete { cert } => {
            client
                .delete(&format!("/certificates/{}", seg(&cert)))
                .await?;
            writeln!(out, "certificate {cert} deleted")?;
            Ok(())
        }
    }
}

async fn account(
    client: &Client,
    cmd: AccountCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let show = |out: &mut (dyn Write + Send), got: &Value| -> anyhow::Result<()> {
        if json_out {
            return Ok(print_json(out, got)?);
        }
        writeln!(
            out,
            "acme-account {}: {} ({})",
            cell(&got["name"]),
            cell(&got["status"]["url"]),
            cell(&got["spec"]["directory"])
        )?;
        Ok(())
    };
    match cmd {
        AccountCmd::Add {
            name,
            directory,
            email,
            agree_tos,
            eab_kid,
            eab_hmac_file,
            ca_bundle,
        } => {
            let mut body = json!({ "name": name, "directory": directory, "agree_tos": agree_tos });
            if let Some(e) = email {
                body["email"] = json!(e);
            }
            if let Some(kid) = eab_kid {
                let hmac = secret(eab_hmac_file.as_ref(), "ACME_EAB_HMAC", "EAB HMAC key: ")?;
                body["eab"] = json!({ "kid": kid, "hmac_key": hmac });
            }
            if let Some(f) = ca_bundle {
                body["ca_bundle"] = json!(
                    std::fs::read_to_string(&f)
                        .with_context(|| format!("cannot read {}", f.display()))?
                );
            }
            show(out, &client.post("/acme-accounts", &body).await?)
        }
        AccountCmd::List => {
            let got = client.get("/acme-accounts").await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            let rows: Vec<Vec<String>> = got["items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            vec![
                                cell(&x["name"]),
                                cell(&x["spec"]["directory"]),
                                cell(&x["spec"]["email"]),
                                if x["status"]["url"].is_string() {
                                    "yes"
                                } else {
                                    "no"
                                }
                                .into(),
                                x["status"]["error"].as_str().unwrap_or("").into(),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(table(
                out,
                &["NAME", "DIRECTORY", "EMAIL", "REGISTERED", "ERROR"],
                &rows,
            )?)
        }
        AccountCmd::Get { account } => show(
            out,
            &client
                .get(&format!("/acme-accounts/{}", seg(&account)))
                .await?,
        ),
        AccountCmd::Check { account } => show(
            out,
            &client
                .post(
                    &format!("/acme-accounts/{}/actions/check", seg(&account)),
                    &json!({}),
                )
                .await?,
        ),
        AccountCmd::Delete { account } => {
            client
                .delete(&format!("/acme-accounts/{}", seg(&account)))
                .await?;
            writeln!(out, "acme-account {account} deleted")?;
            Ok(())
        }
    }
}

async fn dns(
    client: &Client,
    cmd: DnsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        DnsCmd::Add(a) => {
            let body = provider_body(&a, || {
                secret(
                    a.secret_file.as_ref(),
                    "ACME_DNS_SECRET",
                    "DNS provider secret: ",
                )
            })?;
            let got = client.post("/acme-dns", &body).await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            writeln!(
                out,
                "acme-dns {} added ({})",
                cell(&got["name"]),
                cell(&got["spec"]["provider"]["kind"])
            )?;
            Ok(())
        }
        DnsCmd::List => {
            let got = client.get("/acme-dns").await?;
            if json_out {
                return Ok(print_json(out, &got)?);
            }
            let rows: Vec<Vec<String>> = got["items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|x| {
                            let zones: Vec<String> = x["spec"]["zones"]
                                .as_array()
                                .map(|z| z.iter().map(cell).collect())
                                .unwrap_or_default();
                            vec![
                                cell(&x["name"]),
                                cell(&x["spec"]["provider"]["kind"]),
                                zones.join(","),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(table(out, &["NAME", "PROVIDER", "ZONES"], &rows)?)
        }
        DnsCmd::Get { dns } => Ok(print_json(
            out,
            &client.get(&format!("/acme-dns/{}", seg(&dns))).await?,
        )?),
        DnsCmd::Delete { dns } => {
            client.delete(&format!("/acme-dns/{}", seg(&dns))).await?;
            writeln!(out, "acme-dns {dns} deleted")?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(subcommand)]
        cmd: CertCmd,
    }

    #[test]
    fn bodies_carry_only_what_was_given() {
        let T {
            cmd: CertCmd::Add(a),
        } = T::try_parse_from([
            "t",
            "add",
            "api",
            "--account",
            "le",
            "--name",
            "api.example.com",
            "--shared",
            "--dns01",
            "cf",
            "--key",
            "p384",
            "--days-before",
            "20",
            "--no-precheck",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            certificate_body(&a),
            json!({ "name": "api", "account": "le", "names": ["api.example.com"], "scope": { "kind": "shared" },
                    "challenge": { "kind": "dns01", "dns": "cf" }, "key": "ecdsa-p384",
                    "renew": { "kind": "days_before", "days": 20 }, "precheck": false })
        );
        assert!(
            T::try_parse_from([
                "t",
                "add",
                "x",
                "--account",
                "le",
                "--name",
                "a",
                "--shared",
                "--per-node"
            ])
            .is_err()
        );
        assert!(T::try_parse_from(["t", "add", "x", "--account", "le"]).is_err());
        let T {
            cmd: CertCmd::Dns {
                cmd: DnsCmd::Add(d),
            },
        } = T::try_parse_from([
            "t",
            "dns",
            "add",
            "bind",
            "--provider",
            "rfc2136",
            "--server",
            "10.0.0.53:53",
            "--key-name",
            "acme",
            "--algorithm",
            "hmac-sha512",
            "--zones",
            "example.com",
            "--no-propagation-check",
        ])
        .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            provider_body(&d, || Ok("c2VjcmV0".into())).unwrap(),
            json!({ "name": "bind", "provider": { "kind": "rfc2136", "server": "10.0.0.53:53", "key_name": "acme",
                    "algorithm": "hmac-sha512", "secret": "c2VjcmV0" }, "zones": ["example.com"],
                    "propagation": { "timeout_s": 180, "check": "skip" } })
        );
        let T {
            cmd: CertCmd::Dns {
                cmd: DnsCmd::Add(d),
            },
        } = T::try_parse_from(["t", "dns", "add", "r", "--provider", "route53"]).unwrap()
        else {
            panic!()
        };
        assert!(
            provider_body(&d, || Ok("s".into()))
                .unwrap_err()
                .to_string()
                .contains("--access-key-id")
        );
    }

    #[test]
    fn rows_per_node() {
        let c = json!({ "name": "web", "spec": { "scope": { "kind": "per_node" }, "challenge": { "kind": "http01" } },
            "status": { "nodes": { "n1": { "serial": "ab", "not_after": 0, "renew_at": 60 } }, "error": "boom" } });
        assert_eq!(
            rows(&c),
            [[
                "web",
                "per-node",
                "http-01",
                "n1",
                "ab",
                "1970-01-01 00:00",
                "1970-01-01 00:01",
                "boom"
            ]]
        );
        let empty = json!({ "name": "x", "spec": { "scope": { "kind": "shared" }, "challenge": { "kind": "dns01", "dns": "cf" } }, "status": {} });
        assert_eq!(rows(&empty)[0][..4], ["x", "shared", "dns-01 cf", "-"]);
    }

    /// `cert renew`은 Task를 알린다.
    #[tokio::test]
    async fn renew_reports_the_tasks() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/certificates/web/actions/renew") => {
                (202, json!({ "tasks": [{ "id": "t1" }, { "id": "t2" }] }))
            }
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let mut out = Vec::new();
        cert(
            &client,
            CertCmd::Renew {
                cert: "web".into(),
                wait: false,
            },
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "certificate web: ordering (task t1, t2)\n"
        );
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
