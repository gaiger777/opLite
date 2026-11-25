//! ACME 시험의 도움(계획 73): 시험 CA로 사슬 만들기, SNI 손맞춤으로 받은 인증서의 일련번호 보기, 노드 기능 더하기,
//! 계정 리소스 바로 만들기.

use std::net::SocketAddr;
use std::sync::Arc;

use orbivisor_api::acme::jws::AccountKey;
use orbivisor_cluster::Cluster;
use orbivisor_types::acme::{AcmeAccount, AcmeAccountSpec, AcmeAccountStatus, Secret};
use orbivisor_types::node::NodeSpec;
use orbivisor_types::resource::Resource;
use rustls_pki_types::ServerName;
use uuid::Uuid;

/// 시험 CA(루트 PEM과 발급자)
pub struct TestCa {
    pub pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl TestCa {
    pub fn new(cn: &str) -> Self {
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        p.distinguished_name.push(rcgen::DnType::CommonName, cn);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.self_signed(&key).unwrap();
        Self {
            pem: cert.pem(),
            issuer: rcgen::Issuer::new(p, key),
        }
    }

    /// 이름들의 서버 인증서 (사슬 PEM, 키 PEM)
    pub fn issue(&self, names: &[&str]) -> (String, String) {
        let mut p =
            rcgen::CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
                .unwrap();
        // macOS의 TLS(native-tls)는 825일 넘는 서버 인증서와 서버 용도 없는 인증서를 믿지 않는다.
        let now = time::OffsetDateTime::now_utc();
        p.not_before = now - time::Duration::days(1);
        p.not_after = now + time::Duration::days(30);
        p.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        (cert.pem(), key.serialize_pem())
    }
}

/// `name`(SNI)으로 손맞춤해 `root`로 확인되면 서버 인증서의 일련번호(16진)
pub async fn served_serial(addr: SocketAddr, name: &str, root: &str) -> Option<String> {
    let mut roots = rustls::RootCertStore::empty();
    for c in orbivisor_peer::pki::certs(root).ok()? {
        roots.add(c).ok()?;
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.ok()?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from(name.to_string()).unwrap(), tcp)
        .await
        .ok()?;
    let leaf = tls.get_ref().1.peer_certificates()?.first()?.to_vec();
    orbivisor_api::acme::ari::leaf_of_der(&leaf)
        .ok()
        .map(|l| l.serial)
}

/// 노드 기능에 `extra`를 더한다(`http01` 등).
pub async fn add_feature(cluster: &Cluster, node: Uuid, extra: &str) {
    let mut me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
    if !me.status.features.iter().any(|f| f == extra) {
        me.status.features.push(extra.into());
    }
    cluster
        .update_status::<NodeSpec>(node, &me.status)
        .await
        .unwrap();
}

/// 노드 기능에서 `feature`를 뺀다.
pub async fn drop_feature(cluster: &Cluster, node: Uuid, feature: &str) {
    let mut me = cluster.get::<NodeSpec>(node).unwrap().unwrap();
    me.status.features.retain(|f| f != feature);
    cluster
        .update_status::<NodeSpec>(node, &me.status)
        .await
        .unwrap();
}

/// CA에 묻지 않고 계정 리소스를 바로 만든다(API 시험).
pub async fn seed_account(cluster: &Cluster, name: &str, directory: &str) -> AcmeAccount {
    let mut a: AcmeAccount = Resource::new(
        name,
        AcmeAccountSpec {
            directory: directory.into(),
            email: None,
            agree_tos: true,
            eab: None,
            ca_bundle: None,
            key: Secret::new(AccountKey::generate().unwrap().to_pem()),
        },
    );
    a.status = AcmeAccountStatus {
        url: Some(format!("{directory}/acct/1")),
        ..Default::default()
    };
    cluster.create(&a).await.unwrap();
    a
}
