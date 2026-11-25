//! 이 노드가 공개 리스너에 끼울 ACME 인증서 고르기(계획 73 §11.5). `install: api`이고 이 노드를 덮는 살아 있는 인증서
//! — 노드마다면 `nodes[이 노드]`(키는 이 노드 디스크), 공유면 `shared`(키는 Raft status의 비밀). 노드마다 인증서가 공유보다
//! 먼저 기본 인증서가 되고, 모두 SNI 이름으로도 고를 수 있다. [`crate::ApiCertAgent`]가 쓴다.

use std::path::Path;

use opl_types::acme::{Certificate, CertificateInstall, CertificateScope};
use uuid::Uuid;

/// 끼울 인증서 하나
#[derive(Clone, PartialEq, Eq)]
pub struct Installable {
    pub certificate: Uuid,
    pub names: Vec<String>,
    pub chain_pem: String,
    pub key_pem: String,
    pub not_after: i64,
    pub per_node: bool,
}

impl std::fmt::Debug for Installable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installable")
            .field("certificate", &self.certificate)
            .field("names", &self.names)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

/// 이 노드(`me`)가 끼울 인증서들(기본이 될 것이 먼저 — 노드마다, 그다음 늦게 끝나는 것). 키를 읽지 못하면 뺀다.
pub fn installable(
    certs: &[Certificate],
    me: Uuid,
    key_dir: Option<&Path>,
    now: i64,
) -> Vec<Installable> {
    let mut out = Vec::new();
    for c in certs
        .iter()
        .filter(|c| c.spec.install == CertificateInstall::Api)
    {
        match &c.spec.scope {
            CertificateScope::PerNode { .. } => {
                let (Some(issued), Some(dir)) = (c.status.nodes.get(&me), key_dir) else {
                    continue;
                };
                if issued.not_after <= now {
                    continue;
                }
                let Ok(key_pem) = std::fs::read_to_string(super::order::key_path(dir, c.id)) else {
                    continue;
                };
                out.push(Installable {
                    certificate: c.id,
                    names: issued.names.clone(),
                    chain_pem: issued.chain_pem.clone(),
                    key_pem,
                    not_after: issued.not_after,
                    per_node: true,
                });
            }
            CertificateScope::Shared => {
                let Some(issued) = &c.status.shared else {
                    continue;
                };
                if issued.not_after <= now || c.status.shared_key.is_empty() {
                    continue;
                }
                out.push(Installable {
                    certificate: c.id,
                    names: issued.names.clone(),
                    chain_pem: issued.chain_pem.clone(),
                    key_pem: c.status.shared_key.expose().to_string(),
                    not_after: issued.not_after,
                    per_node: false,
                });
            }
        }
    }
    out.sort_by_key(|i| (!i.per_node, -i.not_after));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::acme::{
        CertificateKey, CertificateSpec, ChallengeKind, IssuedCert, RenewPolicy, Secret,
    };
    use opl_types::resource::Resource;

    fn cert(n: u128, scope: CertificateScope) -> Certificate {
        let mut c = Resource::new(
            format!("c{n}"),
            CertificateSpec {
                account: "le".into(),
                names: vec!["a.example.com".into()],
                scope,
                challenge: ChallengeKind::Http01,
                key: CertificateKey::EcdsaP256,
                profile: None,
                renew: RenewPolicy::Ari,
                preferred_chain: None,
                install: CertificateInstall::Api,
                precheck: true,
            },
        );
        c.id = Uuid::from_u128(n);
        c
    }

    fn issued(name: &str, not_after: i64) -> IssuedCert {
        IssuedCert {
            serial: "1".into(),
            not_before: 0,
            not_after,
            names: vec![name.into()],
            chain_pem: "chain".into(),
            ari: None,
            renew_at: 0,
            key: CertificateKey::EcdsaP256,
            error: None,
        }
    }

    #[test]
    fn per_node_first_and_only_live_ones() {
        let dir = tempfile::TempDir::new().unwrap();
        let me = Uuid::from_u128(7);
        let mut per = cert(1, CertificateScope::PerNode { nodes: vec![] });
        per.status.nodes.insert(me, issued("n7.example.com", 100));
        per.status
            .nodes
            .insert(Uuid::from_u128(8), issued("n8.example.com", 100));
        let mut shared = cert(2, CertificateScope::Shared);
        shared.status.shared = Some(issued("vip.example.com", 200));
        shared.status.shared_key = Secret::new("shared-key");
        let mut none = cert(3, CertificateScope::Shared);
        none.spec.install = CertificateInstall::None;
        none.status.shared = Some(issued("x.example.com", 200));
        none.status.shared_key = Secret::new("k");
        let certs = [shared.clone(), per.clone(), none];
        // 키 파일이 없으면 노드마다 인증서는 뺀다.
        let got = installable(&certs, me, Some(dir.path()), 50);
        assert_eq!(
            got.iter().map(|i| i.certificate).collect::<Vec<_>>(),
            [shared.id]
        );
        std::fs::write(
            super::super::order::key_path(dir.path(), per.id),
            "node-key",
        )
        .unwrap();
        let got = installable(&certs, me, Some(dir.path()), 50);
        assert_eq!(
            got.iter().map(|i| i.certificate).collect::<Vec<_>>(),
            [per.id, shared.id]
        );
        assert_eq!(got[0].key_pem, "node-key");
        assert_eq!(got[0].names, ["n7.example.com"]);
        assert!(!format!("{:?}", got[1]).contains("shared-key"));
        // 만료된 것은 끼우지 않는다.
        assert_eq!(installable(&certs, me, Some(dir.path()), 150).len(), 1);
        assert!(installable(&certs, me, Some(dir.path()), 250).is_empty());
    }
}
