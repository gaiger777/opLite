//! 받은 사슬 읽기(x509-parser): 맨 앞 인증서의 일련번호·유효 기간·이름, ARI 인증서 id(RFC 9773 §4.1 —
//! base64url(AKI keyIdentifier) "." base64url(일련번호 DER 값)), 사슬 맨 위의 발급자 CN, 갱신 시각 고르기.

use opl_types::acme::{AriWindow, RenewPolicy};
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::{FromDer, X509Certificate};

use super::jws::b64;

/// 사슬 맨 앞(잎) 인증서에서 읽은 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    /// 16진수(소문자, 앞 0 뺌)
    pub serial: String,
    pub not_before: i64,
    pub not_after: i64,
    /// SAN의 DNS 이름·IP
    pub names: Vec<String>,
    /// ARI 인증서 id(AKI가 없으면 None)
    pub cert_id: Option<String>,
}

/// PEM 사슬의 인증서 DER들(차례 그대로)
pub fn chain_ders(pem: &str) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = pem;
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let block = &rest[start..];
        let Some(end) = block.find("-----END CERTIFICATE-----") else {
            break;
        };
        let end = end + "-----END CERTIFICATE-----".len();
        if let Some(der) = super::jws::pem_der(&block[..end], "CERTIFICATE") {
            out.push(der);
        }
        rest = &block[end..];
    }
    out
}

/// ARI 인증서 id(RFC 9773 §4.1)
pub fn cert_id_from(aki: &[u8], serial: &[u8]) -> String {
    format!("{}.{}", b64(aki), b64(serial))
}

/// 인증서 DER 하나를 읽는다.
pub fn leaf_of_der(der: &[u8]) -> Result<Leaf, String> {
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| format!("cannot parse the certificate: {e}"))?;
    let mut names = Vec::new();
    let mut aki = None;
    for ext in cert.extensions() {
        match ext.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(san) => {
                for n in &san.general_names {
                    match n {
                        GeneralName::DNSName(d) => names.push(d.to_string()),
                        GeneralName::IPAddress(ip) => {
                            let ip = match ip.len() {
                                4 => <[u8; 4]>::try_from(*ip)
                                    .ok()
                                    .map(|b| std::net::IpAddr::from(b).to_string()),
                                16 => <[u8; 16]>::try_from(*ip)
                                    .ok()
                                    .map(|b| std::net::IpAddr::from(b).to_string()),
                                _ => None,
                            };
                            names.extend(ip);
                        }
                        _ => {}
                    }
                }
            }
            ParsedExtension::AuthorityKeyIdentifier(a) => {
                aki = a.key_identifier.as_ref().map(|k| k.0.to_vec());
            }
            _ => {}
        }
    }
    let serial = cert.raw_serial();
    let mut hex = hex::encode(serial);
    while hex.len() > 1 && hex.starts_with('0') {
        hex.remove(0);
    }
    Ok(Leaf {
        serial: hex,
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
        names,
        cert_id: aki.map(|a| cert_id_from(&a, serial)),
    })
}

/// 사슬 PEM의 잎
pub fn leaf(pem: &str) -> Result<Leaf, String> {
    let ders = chain_ders(pem);
    leaf_of_der(ders.first().ok_or("the chain holds no certificate")?)
}

/// 사슬 맨 위 인증서의 발급자 CN(`preferred_chain`과 견준다)
pub fn chain_root_issuer(pem: &str) -> Option<String> {
    let ders = chain_ders(pem);
    let (_, top) = X509Certificate::from_der(ders.last()?).ok()?;
    top.issuer()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        .map(str::to_string)
}

/// `DaysBefore { days }`가 이 인증서의 수명보다 짧은가. 아니면(예: 6일짜리 `shortlived`에 7일, 45일 인증서에 60일) 받자마자
/// 갱신할 때가 되어 틱마다 다시 주문하므로 [`renew_at`]은 1/3 규칙으로 바꾼다.
pub fn days_before_fits(days: u16, not_before: i64, not_after: i64) -> bool {
    i64::from(days) * 86_400 < (not_after - not_before).max(0)
}

/// 갱신할 때(유닉스 초): `DaysBefore`면 만료 그 날 전(수명보다 길면 1/3 규칙), ARI 창이 있으면 창 안(`jitter`(0~1)로
/// 흩뜨림), 없으면 유효 기간의 1/3이 남았을 때.
pub fn renew_at(
    policy: RenewPolicy,
    not_before: i64,
    not_after: i64,
    ari: Option<&AriWindow>,
    jitter: f64,
) -> i64 {
    match (policy, ari) {
        (RenewPolicy::DaysBefore { days }, _) if days_before_fits(days, not_before, not_after) => {
            not_after - i64::from(days) * 86_400
        }
        (RenewPolicy::DaysBefore { .. }, _) => not_after - (not_after - not_before).max(0) / 3,
        (RenewPolicy::Ari, Some(w)) => {
            let span = (w.end - w.start).max(0) as f64;
            w.start + (span * jitter.clamp(0.0, 1.0)) as i64
        }
        (RenewPolicy::Ari, None) => not_after - (not_after - not_before).max(0) / 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9773_cert_id_example() {
        // RFC 9773 §4.1: AKI 69:88:5B:…:C8:D4, serial 00:87:65:43:21
        let aki = hex::decode("69885B6B87464041E1B37B847BA0AE2CDE01C8D4").unwrap();
        let serial = hex::decode("0087654321").unwrap();
        assert_eq!(
            cert_id_from(&aki, &serial),
            "aYhba4dGQEHhs3uEe6CuLN4ByNQ.AIdlQyE"
        );
    }

    #[test]
    fn renew_time_policies() {
        let (nb, na) = (0, 90 * 86_400);
        assert_eq!(renew_at(RenewPolicy::Ari, nb, na, None, 0.5), 60 * 86_400);
        assert_eq!(
            renew_at(RenewPolicy::DaysBefore { days: 10 }, nb, na, None, 0.5),
            80 * 86_400
        );
        let w = AriWindow {
            start: 100,
            end: 200,
            checked_at: 0,
        };
        assert_eq!(renew_at(RenewPolicy::Ari, nb, na, Some(&w), 0.0), 100);
        assert_eq!(renew_at(RenewPolicy::Ari, nb, na, Some(&w), 0.5), 150);
        assert_eq!(renew_at(RenewPolicy::Ari, nb, na, Some(&w), 7.0), 200);
        // 리뷰 1: 수명 이상의 days는 받자마자 갱신하지 않게 1/3 규칙으로
        assert_eq!(
            renew_at(RenewPolicy::DaysBefore { days: 90 }, nb, na, None, 0.5),
            60 * 86_400
        );
        assert_eq!(
            renew_at(
                RenewPolicy::DaysBefore { days: 7 },
                0,
                6 * 86_400,
                None,
                0.5
            ),
            4 * 86_400
        );
        assert_eq!(
            renew_at(RenewPolicy::DaysBefore { days: 89 }, nb, na, None, 0.5),
            86_400
        );
        assert!(!days_before_fits(90, nb, na) && days_before_fits(89, nb, na));
    }

    #[test]
    fn reads_a_generated_chain() {
        let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca.distinguished_name
            .push(rcgen::DnType::CommonName, "Test Root X1");
        ca.key_identifier_method = rcgen::KeyIdMethod::Sha256;
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca_cert = ca.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca, ca_key);
        let mut p =
            rcgen::CertificateParams::new(vec!["a.example.com".into(), "10.0.0.1".into()]).unwrap();
        p.use_authority_key_identifier_extension = true;
        p.serial_number = Some(rcgen::SerialNumber::from_slice(&[0x00, 0x87, 0x65]));
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &issuer).unwrap();
        let chain = format!("{}{}", cert.pem(), ca_cert.pem());
        let l = leaf(&chain).unwrap();
        assert_eq!(l.names, ["a.example.com", "10.0.0.1"]);
        assert_eq!(l.serial, "8765");
        assert!(l.not_after > l.not_before);
        assert!(l.cert_id.as_deref().unwrap().ends_with(".AIdl"));
        assert_eq!(chain_root_issuer(&chain).as_deref(), Some("Test Root X1"));
        assert_eq!(chain_ders(&chain).len(), 2);
    }
}
