//! ACME 인증서(계획 73 §11): 손으로 짠 RFC 8555 클라이언트([`client`]·[`jws`]·[`ari`]), HTTP-01 리스너([`http01`] —
//! Raft의 도전 값), DNS-01 공급자([`dns`]), 주문 Task([`order`]), 리더 갱신 루프([`renewer`]), API 인증서 고르기
//! ([`install`]), API([`api`]).

pub mod api;
pub mod ari;
pub mod client;
pub mod dns;
pub mod http01;
pub mod install;
pub mod jws;
pub mod order;
pub mod renewer;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use orbivisor_types::acme::{CertificateScope, CertificateSpec};
use orbivisor_types::node::Node;
use uuid::Uuid;

pub use renewer::{AcmeRenewer, AcmeRenewerConfig};

/// 노드마다의 ACME 설정(opld가 채운다)
#[derive(Debug, Clone)]
pub struct AcmeNodeConfig {
    /// 노드마다 인증서의 키 디렉터리(`{data_dir}/pki/acme`, 0700). 없으면 이 노드는 노드마다 인증서를 받지 않는다.
    pub key_dir: Option<PathBuf>,
    /// DNS-01 전파 확인에 쓸 서버(opld `[acme] dns_resolver`). 있으면 이 서버만 권한 서버로 보고 묻는다. 없으면 영역의
    /// NS를 찾아 모두 묻는다.
    pub dns_resolver: Option<SocketAddr>,
    /// 훅 디렉터리의 실제 자리(기본 `/etc/orbivisor/acme-hooks/` — 시험이 바꾼다)
    pub hook_root: PathBuf,
    pub dns_endpoints: dns::DnsEndpoints,
    /// 인가·주문을 기다리는 한도(10분)
    pub poll_limit: Duration,
    /// 이 노드 공개 리스너의 인증서(만료 경보가 본다)
    pub api_cert: Option<Arc<orbivisor_peer::tls::ApiCert>>,
    /// 내부 노드 인증서(PEM — 만료 경보가 본다)
    pub node_cert_pem: Option<String>,
}

impl Default for AcmeNodeConfig {
    fn default() -> Self {
        Self {
            key_dir: None,
            dns_resolver: None,
            hook_root: PathBuf::from(orbivisor_types::acme::HOOK_DIR),
            dns_endpoints: dns::DnsEndpoints::default(),
            poll_limit: Duration::from_secs(600),
            api_cert: None,
            node_cert_pem: None,
        }
    }
}

/// 노드 스펙 주소의 호스트 부분(`{node_address}` 자리)
pub fn node_address(node: &Node) -> String {
    let a = node.spec.address.as_str();
    let host = match a.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => h,
        _ => a,
    };
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_string()
}

/// 인증서가 덮는 노드(노드마다면 `nodes` — 비면 위트니스가 아닌 모든 노드)
pub fn covered_nodes<'a>(cert: &CertificateSpec, nodes: &'a [Node]) -> Vec<&'a Node> {
    nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter(|n| match &cert.scope {
            CertificateScope::PerNode { nodes } => nodes.is_empty() || nodes.contains(&n.id),
            CertificateScope::Shared => true,
        })
        .collect()
}

/// 이 노드가 낼 수 있는 공개 인증서들과 내부 노드 인증서 가운데 가장 먼저 끝나는 때(유닉스 초). 읽을 것이 없으면 None.
pub fn soonest_expiry(config: &AcmeNodeConfig) -> Option<i64> {
    let mut ders = config
        .api_cert
        .as_ref()
        .map(|c| c.leaf_ders())
        .unwrap_or_default();
    if let Some(pem) = &config.node_cert_pem {
        ders.extend(ari::chain_ders(pem).into_iter().take(1));
    }
    ders.iter()
        .filter_map(|d| ari::leaf_of_der(d).ok())
        .map(|l| l.not_after)
        .min()
}

/// 유닉스 초를 RFC 3339(UTC)로
pub fn fmt_time(secs: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| secs.to_string())
}

/// status 칸의 열쇠(노드 id 또는 `shared`)
pub fn slot(node: Option<Uuid>) -> String {
    node.map_or_else(|| "shared".to_string(), |n| n.to_string())
}
