//! 원격 클러스터. 다른 Orbivisor 클러스터의 주소, 그 클러스터 CA의 지문과 인증서, 그 클러스터에서 만든 API
//! 토큰을 두고, 리더가 상태를 모으며 API가 요청을 중계한다. 두 클러스터는 대등하다(원격 쪽 권한은 토큰 주인이 정한다).

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

pub type Remote = Resource<RemoteSpec, RemoteStatus>;

/// API 토큰의 접두(`opl_<id>_<비밀>`)
const TOKEN_PREFIX: &str = "opl_";
/// CA 지문의 접두
const FINGERPRINT_PREFIX: &str = "sha256:";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RemoteSpec {
    /// `https://<호스트>[:포트]`(경로 없음)
    pub url: String,
    /// 원격 클러스터 CA 인증서의 지문 `sha256:<소문자 16진수 64자>`. TLS는 이 CA 하나만 믿는다.
    pub ca_fingerprint: String,
    /// 원격 클러스터에서 만든 API 토큰. API는 내보내지 않는다.
    pub token: String,
    /// 등록하거나 바꿀 때 서버가 받아 `ca_fingerprint`와 맞는지 확인한 원격 클러스터 CA 인증서(PEM, 정확히
    /// 하나). 노드는 연결할 때 CA를 다시 받지 않고 이것만 믿는다.
    pub ca_pem: String,
}

impl ResourceSpec for RemoteSpec {
    const KIND: Kind = Kind::Remote;
    type Status = RemoteStatus;
}

// 토큰이 로그에 나가지 않게 가리고, 긴 CA PEM은 뺀다.
impl std::fmt::Debug for RemoteSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSpec")
            .field("url", &self.url)
            .field("ca_fingerprint", &self.ca_fingerprint)
            .field("token", &"***")
            .finish_non_exhaustive()
    }
}

/// 리더가 주기적으로 모은 원격 클러스터의 상태. 모으지 못하면 `reachable`이 false이고 `error`에 이유가 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct RemoteStatus {
    pub reachable: bool,
    /// 상태가 마지막으로 바뀐 시각(유닉스 초). 같은 상태면 다시 쓰지 않으므로 마지막 확인 시각이 아니다.
    pub checked_at: Option<u64>,
    /// 닿지 못한 이유
    pub error: Option<String>,
    pub cluster_name: Option<String>,
    /// 노드 버전 가운데 가장 낮은 것
    pub version: Option<String>,
    /// 위트니스를 뺀 노드 수
    pub nodes: Option<NodeCounts>,
    pub vms: Option<VmCounts>,
    /// 위트니스를 뺀 노드의 CPU 합
    pub cpus: Option<u64>,
    /// 위트니스를 뺀 노드의 메모리 합(GiB)
    pub memory_gib: Option<u64>,
}

// 텔레메트리 보고의 `NodeCounts`와 API 문서의 이름이 겹치지 않게 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = RemoteNodeCounts))]
pub struct NodeCounts {
    pub total: u64,
    pub online: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(as = RemoteVmCounts))]
pub struct VmCounts {
    pub total: u64,
    pub running: u64,
}

impl RemoteSpec {
    /// 검증하고 저장할 모양으로 바꾼다: url의 앞뒤 공백과 끝 `/`를 떼고, 지문을 `sha256:<소문자>`로.
    pub fn normalize(&mut self) -> Result<(), RemoteSpecError> {
        self.url = normalize_url(&self.url)?;
        self.ca_fingerprint =
            normalize_ca_fingerprint(&self.ca_fingerprint).ok_or(RemoteSpecError::Fingerprint)?;
        if token_id(&self.token).is_none() {
            return Err(RemoteSpecError::Token);
        }
        Ok(())
    }
}

/// `https://<호스트>[:포트]`(경로는 없거나 `/`, 쿼리·조각·사용자 정보 없음)를 끝 `/` 없이 돌려준다.
pub fn normalize_url(url: &str) -> Result<String, RemoteSpecError> {
    let url = url.trim();
    let rest = url.strip_prefix("https://").ok_or(RemoteSpecError::Url)?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@'])
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(RemoteSpecError::Url);
    }
    let (host_ok, port) = match authority.strip_prefix('[') {
        // [IPv6]:포트
        Some(v6) => {
            let (host, after) = v6.split_once(']').ok_or(RemoteSpecError::Url)?;
            let port = match after {
                "" => None,
                _ => Some(after.strip_prefix(':').ok_or(RemoteSpecError::Url)?),
            };
            (host.parse::<std::net::Ipv6Addr>().is_ok(), port)
        }
        None => {
            let (host, port) = match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            };
            (!host.is_empty() && !host.contains(['[', ']']), port)
        }
    };
    let port_ok = port.is_none_or(|p| {
        p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|p| p != 0)
    });
    if !host_ok || !port_ok {
        return Err(RemoteSpecError::Url);
    }
    Ok(format!("https://{authority}"))
}

/// CA 지문을 `sha256:<소문자 16진수 64자>`로 바꾼다. 접두 `sha256:`은 없어도 되고, 대문자와 `:` 구분
/// (`AB:CD:…`)도 받는다. 그 모양이 아니면 None.
pub fn normalize_ca_fingerprint(text: &str) -> Option<String> {
    let text = text.trim();
    let hex = match text.get(..FINGERPRINT_PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(FINGERPRINT_PREFIX) => {
            &text[FINGERPRINT_PREFIX.len()..]
        }
        _ => text,
    };
    let hex: String = hex.chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{FINGERPRINT_PREFIX}{}", hex.to_ascii_lowercase()))
}

/// API 토큰(`opl_<id>_<비밀>`)의 id. 그 모양이 아니면 None. 비밀은 HTTP 헤더에 넣을 수 있는
/// 공백 없는 ASCII여야 한다.
pub fn token_id(token: &str) -> Option<Uuid> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    if secret.is_empty() || !secret.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Uuid::try_parse(id).ok()
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RemoteSpecError {
    #[error(
        "url must be https://<host>[:port] without a path, query, fragment or user information"
    )]
    Url,
    #[error("ca_fingerprint must be sha256: followed by 64 hexadecimal digits")]
    Fingerprint,
    #[error("token must be an Orbivisor API token (opl_<id>_<secret>)")]
    Token,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const FP: &str = "sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const TOKEN: &str =
        "opl_0192f0c15d2e70008000000000000000_0123456789abcdef0123456789abcdef0123456789abcdef";
    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

    fn spec() -> RemoteSpec {
        RemoteSpec {
            url: "https://dr.example.com:8443".into(),
            ca_fingerprint: FP.into(),
            token: TOKEN.into(),
            ca_pem: CA_PEM.into(),
        }
    }

    #[test]
    fn kind_is_remote() {
        assert_eq!(RemoteSpec::KIND, Kind::Remote);
        assert_eq!(serde_json::to_value(Kind::Remote).unwrap(), "remote");
    }

    #[test]
    fn spec_round_trips_and_rejects_unknown_fields() {
        let full = json!({
            "url": "https://dr.example.com:8443",
            "ca_fingerprint": FP,
            "token": TOKEN,
            "ca_pem": CA_PEM
        });
        let s: RemoteSpec = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(s, spec());
        assert_eq!(serde_json::to_value(&s).unwrap(), full);

        let mut extra = full.clone();
        extra["bogus"] = json!(1);
        assert!(serde_json::from_value::<RemoteSpec>(extra).is_err());
        for field in ["token", "ca_pem"] {
            let mut missing = full.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<RemoteSpec>(missing).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn debug_hides_the_token() {
        let text = format!("{:?}", spec());
        assert!(!text.contains(TOKEN), "{text}");
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("dr.example.com"), "{text}");
    }

    #[test]
    fn status_defaults_and_round_trips() {
        let empty: RemoteStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, RemoteStatus::default());
        assert!(!empty.reachable);

        let full = json!({
            "reachable": true,
            "checked_at": 1_700_000_000u64,
            "error": null,
            "cluster_name": "dr",
            "version": "0.9.0",
            "nodes": { "total": 3, "online": 2 },
            "vms": { "total": 10, "running": 4 },
            "cpus": 48,
            "memory_gib": 192
        });
        let s: RemoteStatus = serde_json::from_value(full.clone()).unwrap();
        assert_eq!(
            s.nodes,
            Some(NodeCounts {
                total: 3,
                online: 2
            })
        );
        assert_eq!(
            s.vms,
            Some(VmCounts {
                total: 10,
                running: 4
            })
        );
        assert_eq!(serde_json::to_value(&s).unwrap(), full);
    }

    #[test]
    fn normalize_trims_the_url_and_lowers_the_fingerprint() {
        let mut s = RemoteSpec {
            url: " https://dr.example.com:8443/ ".into(),
            ca_fingerprint: format!("SHA256:{}", FP[7..].to_ascii_uppercase()),
            ..spec()
        };
        assert_eq!(s.normalize(), Ok(()));
        assert_eq!(s, spec());
    }

    #[test]
    fn validates_urls() {
        for (url, normal) in [
            ("https://h", "https://h"),
            ("https://h/", "https://h"),
            ("https://h:8443", "https://h:8443"),
            ("https://10.0.0.1:8443/", "https://10.0.0.1:8443"),
            ("https://[::1]", "https://[::1]"),
            ("https://[fe80::1]:8443/", "https://[fe80::1]:8443"),
        ] {
            assert_eq!(normalize_url(url).as_deref(), Ok(normal), "{url}");
        }
        for url in [
            "",
            "h",
            "http://h",
            "HTTPS://h",
            "https://",
            "https:///",
            "https://h/api",
            "https://h//",
            "https://h?x=1",
            "https://h/?x=1",
            "https://h#x",
            "https://u@h",
            "https://u:p@h",
            "https://h :8443",
            "https://:8443",
            "https://h:",
            "https://h:0",
            "https://h:65536",
            "https://h:+1",
            "https://h:x",
            "https://h:1:2",
            "https://[]:8443",
            "https://[::1",
            "https://[::1]x",
            "https://[h]:8443",
            "https://::1",
        ] {
            assert_eq!(normalize_url(url), Err(RemoteSpecError::Url), "{url}");
        }
    }

    #[test]
    fn normalizes_fingerprints() {
        let hex = &FP[7..];
        let colons = hex
            .as_bytes()
            .chunks(2)
            .map(|p| std::str::from_utf8(p).unwrap().to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(":");
        for text in [
            FP.to_string(),
            hex.to_string(),
            hex.to_ascii_uppercase(),
            colons.clone(),
            format!("sha256:{colons}"),
            format!(" SHA256:{hex} "),
        ] {
            assert_eq!(
                normalize_ca_fingerprint(&text).as_deref(),
                Some(FP),
                "{text}"
            );
        }
        for text in [
            "",
            "sha256:",
            &FP[..FP.len() - 1],
            &format!("{FP}0"),
            &format!("sha1:{hex}"),
            &format!("sha256:{}g", &hex[1..]),
            &format!("sha256:{} ", &hex[..32]),
        ] {
            assert_eq!(normalize_ca_fingerprint(text), None, "{text}");
        }
        let mut s = RemoteSpec {
            ca_fingerprint: "sha256:abc".into(),
            ..spec()
        };
        assert_eq!(s.normalize(), Err(RemoteSpecError::Fingerprint));
    }

    #[test]
    fn validates_tokens() {
        assert_eq!(
            token_id(TOKEN),
            Some(Uuid::try_parse("0192f0c15d2e70008000000000000000").unwrap())
        );
        for bad in [
            "",
            "opl_",
            "opl_0192f0c15d2e70008000000000000000",
            "opl_0192f0c15d2e70008000000000000000_",
            "opl_xyz_abc",
            "0192f0c15d2e70008000000000000000_abc",
            "OPL_0192f0c15d2e70008000000000000000_abc",
            "opl_0192f0c15d2e70008000000000000000_a b",
            "opl_0192f0c15d2e70008000000000000000_abc\n",
            "opl_0192f0c15d2e70008000000000000000_ab\u{e9}",
        ] {
            assert_eq!(token_id(bad), None, "{bad:?}");
            let mut s = RemoteSpec {
                token: bad.into(),
                ..spec()
            };
            assert_eq!(s.normalize(), Err(RemoteSpecError::Token), "{bad:?}");
        }
        // 검사 순서: url, 지문, 토큰
        let mut s = RemoteSpec {
            url: "http://h".into(),
            ca_fingerprint: "x".into(),
            token: "x".into(),
            ca_pem: String::new(),
        };
        assert_eq!(s.normalize(), Err(RemoteSpecError::Url));
    }
}
