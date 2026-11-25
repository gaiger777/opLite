//! 이 노드가 공개 리스너에 끼울 ACME 인증서 고르기(계획 73 §11.5). `install: api`이고 이 노드를 덮는 살아 있는 인증서
//! — 노드마다면 `nodes[이 노드]`(키는 이 노드 디스크), 공유면 `shared`(키는 Raft status의 비밀). 노드마다 인증서가 공유보다
//! 먼저 기본 인증서가 되고, 모두 SNI 이름으로도 고를 수 있다. [`crate::ApiCertAgent`]가 쓴다.

use std::path::Path;

use orbivisor_types::acme::{Certificate, CertificateInstall, CertificateScope};
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
                let path = super::order::key_path(dir, c.id, &issued.serial);
                let key_pem = match std::fs::read_to_string(&path) {
                    Ok(k) => k,
                    Err(e) => {
                        // status가 이 노드의 인증서를 가리키는데 키가 없다(리뷰 2 — 조용히 내부 CA로 돌아가지 않게 알린다).
                        tracing::warn!(certificate = %c.name, serial = %issued.serial, file = %path.display(), error = %e,
                            "the key of this node's ACME certificate is missing; serving the next certificate (renew it to fix)");
                        continue;
                    }
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

/// 이만큼 지난 키 파일만 치운다(막 만든 인증서의 주문이 키를 쓴 뒤 status를 적기 전일 수 있다)
const STALE_KEY_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// 지운 인증서의 노드 키 파일(`{id}.{serial}.key`)을 치운다. 한 시간 넘게 지난 것만.
pub fn remove_deleted_keys(dir: &Path, certs: &[Certificate]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(id) = name
            .to_str()
            .filter(|n| n.ends_with(".key"))
            .and_then(|n| n.split('.').next())
            .and_then(|id| id.parse::<Uuid>().ok())
        else {
            continue;
        };
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age >= STALE_KEY_AGE);
        if old && !certs.iter().any(|c| c.id == id) {
            match std::fs::remove_file(e.path()) {
                Ok(()) => {
                    tracing::info!(file = %e.path().display(), "removed the key of a deleted ACME certificate")
                }
                Err(err) => {
                    tracing::debug!(file = %e.path().display(), error = %err, "cannot remove an ACME key")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::acme::{
        CertificateKey, CertificateSpec, ChallengeKind, IssuedCert, RenewPolicy, Secret,
    };
    use orbivisor_types::resource::Resource;

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
            ari_checked_at: None,
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
        // 다른 일련번호의 키(막 받은 새 키 — status는 아직 옛 사슬)는 고르지 않는다.
        std::fs::write(
            super::super::order::key_path(dir.path(), per.id, "2"),
            "next-key",
        )
        .unwrap();
        assert_eq!(installable(&certs, me, Some(dir.path()), 50).len(), 1);
        std::fs::write(
            super::super::order::key_path(dir.path(), per.id, "1"),
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

    #[test]
    fn removes_only_old_keys_of_deleted_certificates() {
        let dir = tempfile::TempDir::new().unwrap();
        let live = cert(1, CertificateScope::PerNode { nodes: vec![] });
        let gone = Uuid::from_u128(2);
        let keep = super::super::order::key_path(dir.path(), live.id, "1");
        let old = super::super::order::key_path(dir.path(), gone, "1");
        let fresh = super::super::order::key_path(dir.path(), Uuid::from_u128(3), "1");
        for p in [&keep, &old, &fresh] {
            std::fs::write(p, "k").unwrap();
        }
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        for p in [&keep, &old] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(hour_ago)
                .unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        remove_deleted_keys(dir.path(), std::slice::from_ref(&live));
        assert!(keep.exists(), "a live certificate's key stays");
        assert!(!old.exists(), "a deleted certificate's old key goes");
        assert!(
            fresh.exists(),
            "a fresh key may belong to an order in flight"
        );
        assert!(dir.path().join("notes.txt").exists());
    }
}
