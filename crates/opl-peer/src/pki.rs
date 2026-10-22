//! 클러스터 CA와 노드 인증서.
//!
//! 노드는 자기 키를 직접 만들고 CSR만 보낸다. CA를 가진 노드는 CSR에서 공개키만 쓰고, 이름과 용도와
//! 기간은 스스로 정해 서명한다(CSR이 CA 권한을 요청해도 따르지 않는다). 노드 인증서의 CN은 노드 id다.

use std::fmt;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, SerialNumber,
};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// CA 인증서의 유효 기간(일)
const CA_DAYS: i64 = 365 * 20;
/// 노드 인증서의 유효 기간(일). macOS는 유효 기간이 825일을 넘는 TLS 서버 인증서를 거부한다
/// (시작 하루 전부터 유효하므로 하루를 뺀다). 만료 전에 갱신해야 한다.
const NODE_DAYS: i64 = 824;

/// 데이터 디렉터리 아래 노드 TLS 자료를 두는 디렉터리
pub const PKI_DIR: &str = "pki";
const CA_FILE: &str = "ca.crt";
const CERT_FILE: &str = "node.crt";
const KEY_FILE: &str = "node.key";

#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    #[error("certificate: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("invalid PEM: {0}")]
    Pem(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// 클러스터 CA
pub struct ClusterCa {
    pub cert_pem: String,
    pub key_pem: String,
}

impl fmt::Debug for ClusterCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterCa").finish_non_exhaustive()
    }
}

impl ClusterCa {
    /// 새 CA를 만든다(ECDSA P-256).
    pub fn generate(cluster_name: &str) -> Result<Self, PkiError> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.distinguished_name.push(
            DnType::CommonName,
            format!("opLite cluster {cluster_name} CA"),
        );
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        set_validity(&mut params, CA_DAYS);
        params.serial_number = Some(random_serial());
        let cert = params.self_signed(&key)?;
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key.serialize_pem(),
        })
    }

    /// 노드의 CSR에 서명한다. `sans`는 노드에 닿는 호스트 이름과 IP다.
    pub fn sign_node(
        &self,
        csr_pem: &str,
        node: Uuid,
        sans: &[String],
    ) -> Result<String, PkiError> {
        let csr = CertificateSigningRequestParams::from_pem(csr_pem)?;
        let mut params = CertificateParams::new(sans.to_vec())?;
        params
            .distinguished_name
            .push(DnType::CommonName, node.to_string());
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        set_validity(&mut params, NODE_DAYS);
        params.serial_number = Some(random_serial());
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, KeyPair::from_pem(&self.key_pem)?)?;
        let cert = CertificateSigningRequestParams {
            params,
            public_key: csr.public_key,
        }
        .signed_by(&issuer)?;
        Ok(cert.pem())
    }
}

/// 하루 전부터 `days`일 뒤까지 유효하다. 노드 시계가 조금 어긋나도 바로 쓸 수 있다.
fn set_validity(params: &mut CertificateParams, days: i64) {
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(days);
}

/// 무작위 128비트 일련번호. 같은 키에 두 번 서명해도 일련번호가 겹치지 않는다. 맨 윗비트를 지워 양수로
/// 만든다(RFC 5280).
fn random_serial() -> SerialNumber {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes[0] &= 0x7f;
    SerialNumber::from_slice(&bytes)
}

/// 노드 키와 CSR(PEM)을 만든다. 이름은 서명하는 쪽이 정하므로 CSR에는 넣지 않는다.
pub fn node_key_and_csr() -> Result<(String, String), PkiError> {
    let key = KeyPair::generate()?;
    let csr = CertificateParams::default().serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.pem()?))
}

/// PEM의 첫 인증서
pub fn first_cert(pem: &str) -> Result<CertificateDer<'static>, PkiError> {
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| PkiError::Pem(e.to_string()))
}

/// 인증서(PEM)의 SHA-256 지문. `sha256:<16진수>` 형식이다. 노드가 합류할 때 CA를 확인하는 데 쓴다.
pub fn fingerprint(cert_pem: &str) -> Result<String, PkiError> {
    let der = first_cert(cert_pem)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&der))))
}

/// 노드 인증서(DER)의 CN에 담긴 노드 id
pub fn node_id_of(cert_der: &[u8]) -> Option<Uuid> {
    let (_, cert) = x509_parser::parse_x509_certificate(cert_der).ok()?;
    let cn = cert.subject().iter_common_name().next()?;
    Uuid::try_parse(cn.as_str().ok()?).ok()
}

/// 노드가 디스크에 두는 TLS 자료: 클러스터 CA 인증서, 자기 인증서와 키
#[derive(Clone)]
pub struct NodeTls {
    pub ca_pem: String,
    pub cert_pem: String,
    pub key_pem: String,
}

impl fmt::Debug for NodeTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeTls").finish_non_exhaustive()
    }
}

impl NodeTls {
    /// `dir`이 없으면 `None`이다(아직 클러스터에 속하지 않은 노드).
    pub fn load(dir: &Path) -> Result<Option<Self>, PkiError> {
        if !dir.try_exists()? {
            return Ok(None);
        }
        let read = |name: &str| std::fs::read_to_string(dir.join(name));
        Ok(Some(Self {
            ca_pem: read(CA_FILE)?,
            cert_pem: read(CERT_FILE)?,
            key_pem: read(KEY_FILE)?,
        }))
    }

    /// `dir`에 쓴다. 옆의 임시 디렉터리에 다 쓴 뒤 이름을 바꾸므로, 도중에 죽으면 `dir`은 생기지
    /// 않는다. 키는 주인만 읽을 수 있다(0600).
    pub fn save(&self, dir: &Path) -> Result<(), PkiError> {
        let mut tmp = dir.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))?;
        for (name, content, mode) in [
            (CA_FILE, &self.ca_pem, 0o644),
            (CERT_FILE, &self.cert_pem, 0o644),
            (KEY_FILE, &self.key_pem, 0o600),
        ] {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(tmp.join(name))?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
        }
        // 이름을 바꾼 뒤 전원이 나가도 `dir`이 남도록 임시 디렉터리와 부모 디렉터리를 디스크에 쓴다.
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, dir)?;
        let parent = match dir.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_tls(ca: &ClusterCa, node: Uuid) -> NodeTls {
        let (key_pem, csr) = node_key_and_csr().unwrap();
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, node, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        }
    }

    #[test]
    fn signed_node_cert_carries_node_id_and_is_not_a_ca() {
        let ca = ClusterCa::generate("prod").unwrap();
        let node = Uuid::now_v7();
        // CA 권한을 요청하는 CSR이어도 서명한 인증서는 CA가 아니다.
        let key = KeyPair::generate().unwrap();
        let mut greedy = CertificateParams::new(vec!["evil.example".to_string()]).unwrap();
        greedy.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let csr = greedy.serialize_request(&key).unwrap().pem().unwrap();
        let pem = ca.sign_node(&csr, node, &["10.0.0.5".into()]).unwrap();

        let der = first_cert(&pem).unwrap();
        assert_eq!(node_id_of(&der), Some(node));
        let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
        assert!(!cert.is_ca());
        let validity = cert.validity();
        let days = (validity.not_after.timestamp() - validity.not_before.timestamp()) / 86400;
        assert!(days <= 825, "{days} days");
        let sans = cert.subject_alternative_name().unwrap().unwrap();
        let shown = format!("{:?}", sans.value.general_names);
        assert!(
            shown.contains("10.0.0.5") || shown.contains("[10, 0, 0, 5]"),
            "{shown}"
        );
        assert!(!shown.contains("evil"), "{shown}");
    }

    #[test]
    fn same_csr_signed_twice_gets_different_serials() {
        let ca = ClusterCa::generate("prod").unwrap();
        let (_, csr) = node_key_and_csr().unwrap();
        let node = Uuid::now_v7();
        let serial = |pem: &str| {
            let der = first_cert(pem).unwrap();
            let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
            cert.raw_serial().to_vec()
        };
        let a = serial(&ca.sign_node(&csr, node, &["127.0.0.1".into()]).unwrap());
        let b = serial(&ca.sign_node(&csr, node, &["127.0.0.1".into()]).unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_is_sha256_of_the_der() {
        let ca = ClusterCa::generate("prod").unwrap();
        let fp = fingerprint(&ca.cert_pem).unwrap();
        assert!(fp.starts_with("sha256:"));
        assert_eq!(fp.len(), "sha256:".len() + 64);
        assert_eq!(fingerprint(&ca.cert_pem).unwrap(), fp);
        assert_ne!(
            fingerprint(&ClusterCa::generate("prod").unwrap().cert_pem).unwrap(),
            fp
        );
        assert!(fingerprint("not a pem").is_err());
    }

    #[test]
    fn node_tls_round_trips_with_a_private_key() {
        let dir = tempfile::tempdir().unwrap();
        let pki = dir.path().join(PKI_DIR);
        assert!(NodeTls::load(&pki).unwrap().is_none());
        let ca = ClusterCa::generate("prod").unwrap();
        let tls = node_tls(&ca, Uuid::now_v7());
        tls.save(&pki).unwrap();
        let back = NodeTls::load(&pki).unwrap().unwrap();
        assert_eq!(back.cert_pem, tls.cert_pem);
        assert_eq!(back.key_pem, tls.key_pem);
        let mode = std::fs::metadata(pki.join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // 이미 있으면 덮어쓰지 않는다.
        assert!(tls.save(&pki).is_err());
    }

    #[test]
    fn debug_does_not_show_keys() {
        let ca = ClusterCa::generate("prod").unwrap();
        assert!(!format!("{ca:?}").contains("PRIVATE"));
        let tls = node_tls(&ca, Uuid::now_v7());
        assert!(!format!("{tls:?}").contains("PRIVATE"));
    }
}
