//! 새 노드가 클러스터에 합류를 요청한다.

use anyhow::{Context, bail};
use opl_api::cluster::{JoinRequest, JoinResponse, JoinUrl};
use opl_peer::pki::{NodeTls, fingerprint, node_key_and_csr};
use uuid::Uuid;

use crate::config::Config;

/// 받아들여진 합류
pub struct Joined {
    pub cluster: String,
    pub tls: NodeTls,
    /// 받아 준 노드의 노드 간 주소. 여기로 투표 멤버 승격을 요청한다.
    pub via: String,
}

/// 조인 URL의 노드에 합류를 요청한다. 그 노드의 CA는 인증서 검증 없이 받아 URL의 지문과 맞는지
/// 확인하고, 그 뒤로는 그 CA만 믿는다. 키는 이 노드에서 만들고 CSR만 보낸다.
pub async fn request(config: &Config, node: Uuid, url: &JoinUrl) -> anyhow::Result<Joined> {
    let base = format!("https://{}/api/v1", url.address);
    let insecure = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()?;
    let ca_pem = insecure
        .get(format!("{base}/cluster/ca"))
        .send()
        .await
        .with_context(|| format!("cannot reach {}", url.address))?
        .error_for_status()?
        .text()
        .await?;
    if fingerprint(&ca_pem)? != url.ca_fingerprint {
        bail!(
            "the cluster CA of {} does not match the join URL; check the URL",
            url.address
        );
    }
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes())?])
        .build()?;
    let (key_pem, csr_pem) = node_key_and_csr()?;
    let resp = client
        .post(format!("{base}/cluster/join"))
        .json(&JoinRequest {
            token: url.token.clone(),
            node,
            name: config.name.clone(),
            address: config.api_address(),
            cluster_address: config.cluster_address(),
            csr_pem,
        })
        .send()
        .await
        .with_context(|| format!("cannot reach {}", url.address))?;
    if !resp.status().is_success() {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        bail!(
            "join refused: {}",
            body["message"].as_str().unwrap_or("unknown error")
        );
    }
    let joined: JoinResponse = resp.json().await?;
    Ok(Joined {
        cluster: joined.cluster,
        tls: NodeTls {
            ca_pem: joined.ca_pem,
            cert_pem: joined.cert_pem,
            key_pem,
        },
        via: joined.via,
    })
}
