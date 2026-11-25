//! DNS-01 TXT 쓰기(계획 73 §11.4): 공급자 다섯 갈래(Cloudflare·Route 53·RFC 2136·PowerDNS·훅)와 전파 확인(영역의 권한
//! 서버에 직접 묻기). 같은 TXT 이름(와일드카드와 기본 이름)의 값들은 한 번에 넣는다.

pub mod cloudflare;
pub mod hook;
pub mod pdns;
pub mod query;
pub mod rfc2136;
pub mod route53;

use std::net::SocketAddr;
use std::time::Duration;

use opl_types::acme::{AcmeDnsSpec, DnsProvider, Propagation, PropagationCheck};
use serde_json::Value;

use super::AcmeNodeConfig;

/// 공급자 HTTP 요청의 시간 한도
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| crate::telemetry::chain(&e))
}

/// JSON 응답(본문이 JSON이 아니면 상태와 함께 실패)
pub(crate) async fn read_json(req: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = req
        .send()
        .await
        .map_err(|e| crate::telemetry::chain(&e.without_url()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|e| crate::telemetry::chain(&e.without_url()))?;
    serde_json::from_slice(&body).map_err(|_| format!("HTTP {status}: not a JSON response"))
}

/// `zones`(스펙) 가운데 `fqdn`을 담는 가장 긴 것
pub fn longest_zone(fqdn: &str, zones: &[String]) -> Option<String> {
    let fqdn = query::normalize(fqdn);
    zones
        .iter()
        .map(|z| query::normalize(z))
        .filter(|z| fqdn == *z || fqdn.ends_with(&format!(".{z}")))
        .max_by_key(String::len)
}

/// 영역을 찾을 때 물어볼 이름들: 스펙 `zones`에 맞는 것이 있으면 그것 하나, 없으면 레이블 둘 이상인 접미사(긴 것부터)
pub fn candidates(fqdn: &str, zones: &[String]) -> Vec<String> {
    if let Some(z) = longest_zone(fqdn, zones) {
        return vec![z];
    }
    let fqdn = query::normalize(fqdn);
    let labels: Vec<&str> = fqdn.split('.').collect();
    (0..labels.len().saturating_sub(1))
        .map(|i| labels[i..].join("."))
        .collect()
}

/// 공급자 엔드포인트(시험이 같은 프로세스의 가짜 서버로 바꾼다)
#[derive(Debug, Clone)]
pub struct DnsEndpoints {
    pub cloudflare: String,
    pub route53: String,
}

impl Default for DnsEndpoints {
    fn default() -> Self {
        Self {
            cloudflare: "https://api.cloudflare.com/client/v4".into(),
            route53: "https://route53.amazonaws.com".into(),
        }
    }
}

/// 넣은 TXT(지우기에 쓴다)
#[derive(Debug, Clone)]
pub enum Handle {
    Cloudflare {
        zone: String,
        ids: Vec<String>,
    },
    Route53 {
        zone: String,
    },
    PowerDns {
        zone: String,
    },
    /// RFC 2136·훅: 이름과 값만으로 지운다
    Plain,
}

/// 스펙 하나로 만든 공급자
pub enum Provider {
    Cloudflare(cloudflare::Cloudflare),
    Route53(route53::Route53),
    Rfc2136(rfc2136::Rfc2136),
    PowerDns(pdns::PowerDns),
    Hook(hook::Hook),
}

impl Provider {
    pub fn new(spec: &AcmeDnsSpec, node: &AcmeNodeConfig) -> Result<Self, String> {
        Ok(match &spec.provider {
            DnsProvider::Cloudflare { api_token, zone_id } => {
                Self::Cloudflare(cloudflare::Cloudflare {
                    base: node.dns_endpoints.cloudflare.clone(),
                    token: api_token.expose().to_string(),
                    zone_id: zone_id.clone(),
                })
            }
            DnsProvider::Route53 {
                access_key_id,
                secret_access_key,
                hosted_zone_id,
                region,
            } => Self::Route53(route53::Route53 {
                base: node.dns_endpoints.route53.clone(),
                access_key_id: access_key_id.clone(),
                secret_access_key: secret_access_key.expose().to_string(),
                hosted_zone_id: hosted_zone_id.clone(),
                region: region.clone(),
            }),
            DnsProvider::Rfc2136 {
                server,
                key_name,
                algorithm,
                secret,
                zone,
            } => {
                let server = server
                    .parse::<SocketAddr>()
                    .or_else(|_| format!("{server}:53").parse::<SocketAddr>())
                    .map_err(|_| format!("rfc2136 server {server:?} is not ip:port"))?;
                let secret = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    secret.expose().trim(),
                )
                .map_err(|_| "the rfc2136 TSIG secret is not base64".to_string())?;
                Self::Rfc2136(rfc2136::Rfc2136 {
                    server,
                    key_name: key_name.clone(),
                    algorithm: *algorithm,
                    secret,
                    zone: zone.clone(),
                })
            }
            DnsProvider::PowerDns {
                url,
                api_key,
                server_id,
            } => Self::PowerDns(pdns::PowerDns {
                url: url.clone(),
                api_key: api_key.expose().to_string(),
                server_id: server_id.clone(),
            }),
            DnsProvider::Hook { path } => Self::Hook(hook::Hook {
                path: hook::resolve(path, &node.hook_root)
                    .ok_or_else(|| format!("hook path {path:?} is outside the hook directory"))?,
            }),
        })
    }

    /// `fqdn`의 TXT로 `values`를 넣는다.
    pub async fn add(
        &self,
        fqdn: &str,
        values: &[String],
        zones: &[String],
    ) -> Result<Handle, String> {
        match self {
            Self::Cloudflare(p) => {
                let (zone, ids) = p.add(fqdn, values, zones).await?;
                Ok(Handle::Cloudflare { zone, ids })
            }
            Self::Route53(p) => Ok(Handle::Route53 {
                zone: p.add(fqdn, values, zones).await?,
            }),
            Self::PowerDns(p) => Ok(Handle::PowerDns {
                zone: p.add(fqdn, values, zones).await?,
            }),
            Self::Rfc2136(p) => {
                for v in values {
                    p.send(fqdn, v, true).await?;
                }
                Ok(Handle::Plain)
            }
            Self::Hook(p) => {
                for v in values {
                    p.run("add", fqdn, v).await?;
                }
                Ok(Handle::Plain)
            }
        }
    }

    /// 넣은 것을 지운다(실패는 경고만 — 인증서는 이미 받았거나 받지 못했다).
    pub async fn remove(
        &self,
        fqdn: &str,
        values: &[String],
        handle: &Handle,
    ) -> Result<(), String> {
        match (self, handle) {
            (Self::Cloudflare(p), Handle::Cloudflare { zone, ids }) => p.remove(zone, ids).await,
            (Self::Route53(p), Handle::Route53 { zone }) => p.remove(zone, fqdn, values).await,
            (Self::PowerDns(p), Handle::PowerDns { zone }) => p.remove(zone, fqdn, values).await,
            (Self::Rfc2136(p), _) => {
                for v in values {
                    p.send(fqdn, v, false).await?;
                }
                Ok(())
            }
            (Self::Hook(p), _) => {
                for v in values {
                    p.run("remove", fqdn, v).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// `_acme-challenge.{이름}`(와일드카드는 `*.`를 뗀다)
pub fn challenge_name(identifier: &str) -> String {
    format!(
        "_acme-challenge.{}",
        query::normalize(identifier.trim_start_matches("*."))
    )
}

/// TXT가 영역의 모든 권한 서버에 보일 때까지 기다린다. `resolver`(노드 설정 `[acme] dns_resolver`)가 있으면 그 서버만
/// 권한 서버로 보고 묻는다(시험·사내 DNS).
pub async fn wait_propagated(
    propagation: &Propagation,
    fqdn: &str,
    values: &[String],
    resolver: Option<SocketAddr>,
) -> Result<(), String> {
    if propagation.check == PropagationCheck::Skip {
        return Ok(());
    }
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(u64::from(propagation.timeout_s));
    let servers = match resolver {
        Some(r) => vec![r],
        None => {
            let sys = query::system_resolver();
            let zone = query::find_zone(sys, fqdn).await?;
            query::authoritative_servers(sys, &zone).await?
        }
    };
    let mut last = String::new();
    loop {
        let mut all = true;
        for server in &servers {
            for v in values {
                match query::txt_present(*server, fqdn, v).await {
                    Ok(true) => {}
                    Ok(false) => {
                        all = false;
                        last = format!("{server} does not answer the TXT yet");
                    }
                    Err(e) => {
                        all = false;
                        last = e;
                    }
                }
            }
        }
        if all {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the TXT record {fqdn} did not reach the authoritative servers in {}s ({last})",
                propagation.timeout_s
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_candidates() {
        assert_eq!(
            candidates("_acme-challenge.a.example.com", &[]),
            [
                "_acme-challenge.a.example.com",
                "a.example.com",
                "example.com"
            ]
        );
        assert_eq!(
            candidates(
                "_acme-challenge.a.example.com.",
                &["com".into(), "Example.com".into(), "other.org".into()]
            ),
            ["example.com"]
        );
        assert_eq!(
            challenge_name("*.Example.com"),
            "_acme-challenge.example.com"
        );
        assert_eq!(
            longest_zone("notexample.com", &["example.com".into()]),
            None
        );
    }
}
