//! VM의 cloud-init 설정(계획 57). Orbivisor가 이것으로 NoCloud 씨앗(meta-data·user-data·network-config·
//! vendor-data)을 만들어 VM에 읽기 전용 CD로 넣는다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::host::valid_host;
use crate::resource::Kind;

/// 응답에서 비밀번호(해시) 대신 보이는 값. PUT 본문의 이 값은 "저장된 해시를 그대로 둠"이다.
pub const PASSWORD_MASK: &str = "********";

/// SSH 키 수·합 크기, 평문 비밀번호, user-data·vendor-data, DNS 서버·검색 도메인의 상한
pub const MAX_SSH_KEYS: usize = 32;
pub const MAX_SSH_KEYS_BYTES: usize = 16 * 1024;
pub const MAX_PASSWORD_BYTES: usize = 256;
pub const MAX_DATA_BYTES: usize = 64 * 1024;
pub const MAX_DNS_SERVERS: usize = 3;
pub const MAX_DNS_SEARCH: usize = 6;

/// 받는 SSH 공개 키 종류(줄 앞, 뒤에 공백)
const SSH_KEY_TYPES: [&str; 7] = [
    "ssh-ed25519 ",
    "ssh-rsa ",
    "ecdsa-sha2-nistp256 ",
    "ecdsa-sha2-nistp384 ",
    "ecdsa-sha2-nistp521 ",
    "sk-ssh-ed25519@openssh.com ",
    "sk-ecdsa-sha2-nistp256@openssh.com ",
];

/// VM의 cloud-init 설정. 비어 있는 칸은 cloud-init·배포판 기본을 따른다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CloudInit {
    /// 호스트 이름(FQDN도 됨). 없으면 VM 이름.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub hostname: Option<String>,
    /// 배포판 기본 사용자의 새 이름. 없으면 배포판 기본 이름.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub user: Option<String>,
    /// 기본 사용자의 비밀번호. 저장되는 것은 crypt 해시이고 응답에서는 [`PASSWORD_MASK`]로 가린다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub password: Option<String>,
    /// 기본 사용자의 SSH 공개 키(한 줄씩)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub ssh_keys: Vec<String>,
    /// NIC마다의 주소 설정(순번 = `nics` 순번). 비면 cloud-init 기본(첫 NIC DHCP).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub networks: Vec<CloudInitNet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub dns: Option<CloudInitDns>,
    /// 사용자 user-data. 있으면 만든 user-data 대신 그대로 쓴다(`user`·`password`·`ssh_keys`는 쓰이지 않음).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub user_data: Option<String>,
    /// vendor-data 파일. 만든 user-data와 함께 돈다(패키지·runcmd).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vendor_data: Option<String>,
}

/// NIC 하나의 주소 설정. 빈 항목(`{}`)은 IPv4 DHCP다. `ipv4` 없이 `ipv6`만 있으면 IPv4를 두지 않는다(IPv6 전용 —
/// DHCPv4도 쓰려면 `ipv4: {"mode": "dhcp"}`). `ipv6`이 없으면 IPv6 설정이 없다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CloudInitNet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ipv4: Option<IpConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ipv6: Option<IpConfig>,
}

/// 주소를 얻는 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "mode", deny_unknown_fields)]
pub enum IpConfig {
    Dhcp,
    /// IPv6 상태 없는 자동 설정(ipv6만)
    Slaac,
    Static {
        /// `주소/접두 길이`(예: `10.0.0.5/24`)
        address: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
        gateway: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CloudInitDns {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>, default))]
    pub servers: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub search: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CloudInitError {
    #[error("cloud_init.hostname {0:?} is not a valid host name")]
    Hostname(String),
    #[error(
        "cloud_init.user {0:?} must start with a lowercase letter or '_' and have at most 32 \
         lowercase letters, digits, '_' and '-'"
    )]
    User(String),
    #[error("cloud_init.user: use ssh_keys with the default user instead of root")]
    RootUser,
    #[error("cloud_init.password must not be empty")]
    EmptyPassword,
    #[error("cloud_init.password is longer than {MAX_PASSWORD_BYTES} bytes")]
    LongPassword,
    #[error(
        "cloud_init.password: cloud-init cannot use yescrypt/bcrypt-2b hashes; use a SHA-512 crypt \
         hash ($6$, e.g. `mkpasswd -m sha-512`) or a plain password; plain passwords may not start \
         with $y$ or $2b$"
    )]
    UnsupportedHash,
    #[error("cloud_init.ssh_keys: at most {MAX_SSH_KEYS} keys")]
    TooManyKeys,
    #[error(
        "cloud_init.ssh_keys[{0}] must be one line starting with ssh-ed25519, ssh-rsa, \
         ecdsa-sha2-nistp256/384/521, sk-ssh-ed25519@openssh.com or sk-ecdsa-sha2-nistp256@openssh.com"
    )]
    BadKey(usize),
    #[error("cloud_init.ssh_keys: at most {MAX_SSH_KEYS_BYTES} bytes in total")]
    KeysTooLarge,
    #[error("cloud_init.networks has {0} entries but the VM has {1} NICs")]
    TooManyNetworks(usize, usize),
    #[error("cloud_init.networks[{0}].{1}: {2:?} is not an {1} address with a prefix length")]
    BadAddress(usize, &'static str, String),
    #[error("cloud_init.networks[{0}].{1}: gateway {2:?} is not an {1} address")]
    BadGateway(usize, &'static str, String),
    #[error("cloud_init.networks[{0}].ipv4: slaac is only for ipv6")]
    SlaacIpv4(usize),
    #[error("cloud_init.dns.servers: at most {MAX_DNS_SERVERS} servers")]
    TooManyDnsServers,
    #[error("cloud_init.dns.search: at most {MAX_DNS_SEARCH} domains")]
    TooManySearch,
    #[error("cloud_init.dns.search: {0:?} is not a valid domain name")]
    BadSearch(String),
    #[error("cloud_init.{0} is larger than 64 KiB")]
    DataTooLarge(&'static str),
}

impl CloudInit {
    /// 값 검사. `nics`는 VM의 NIC 수다. 비밀번호는 평문·crypt 해시·[`PASSWORD_MASK`] 어느 것이든 받는다(API가
    /// 해시로 바꾸거나 저장된 해시로 되돌린다).
    pub fn validate(&self, nics: usize) -> Result<(), CloudInitError> {
        if let Some(h) = &self.hostname
            && !valid_name(h)
        {
            return Err(CloudInitError::Hostname(h.clone()));
        }
        if let Some(user) = &self.user {
            if !valid_user(user) {
                return Err(CloudInitError::User(user.clone()));
            }
            if user == "root" {
                return Err(CloudInitError::RootUser);
            }
        }
        if let Some(password) = &self.password {
            if password.is_empty() {
                return Err(CloudInitError::EmptyPassword);
            }
            if is_unsupported_hash(password) {
                return Err(CloudInitError::UnsupportedHash);
            }
            if password.len() > MAX_PASSWORD_BYTES && !is_crypt_hash(password) {
                return Err(CloudInitError::LongPassword);
            }
        }
        if self.ssh_keys.len() > MAX_SSH_KEYS {
            return Err(CloudInitError::TooManyKeys);
        }
        for (i, key) in self.ssh_keys.iter().enumerate() {
            let one_line = !key.contains(['\n', '\r']);
            if !one_line || !SSH_KEY_TYPES.iter().any(|t| key.starts_with(t)) {
                return Err(CloudInitError::BadKey(i));
            }
        }
        if self.ssh_keys.iter().map(String::len).sum::<usize>() > MAX_SSH_KEYS_BYTES {
            return Err(CloudInitError::KeysTooLarge);
        }
        if self.networks.len() > nics {
            return Err(CloudInitError::TooManyNetworks(self.networks.len(), nics));
        }
        for (i, net) in self.networks.iter().enumerate() {
            if let Some(ip) = &net.ipv4 {
                check_ip(i, ip, false)?;
            }
            if let Some(ip) = &net.ipv6 {
                check_ip(i, ip, true)?;
            }
        }
        if let Some(dns) = &self.dns {
            if dns.servers.len() > MAX_DNS_SERVERS {
                return Err(CloudInitError::TooManyDnsServers);
            }
            if dns.search.len() > MAX_DNS_SEARCH {
                return Err(CloudInitError::TooManySearch);
            }
            if let Some(bad) = dns.search.iter().find(|d| !valid_name(d)) {
                return Err(CloudInitError::BadSearch(bad.clone()));
            }
        }
        if self
            .user_data
            .as_ref()
            .is_some_and(|d| d.len() > MAX_DATA_BYTES)
        {
            return Err(CloudInitError::DataTooLarge("user_data"));
        }
        if self
            .vendor_data
            .as_ref()
            .is_some_and(|d| d.len() > MAX_DATA_BYTES)
        {
            return Err(CloudInitError::DataTooLarge("vendor_data"));
        }
        Ok(())
    }
}

/// RFC 1123 이름(점으로 이은 FQDN도 됨). IP 주소는 이름이 아니다.
fn valid_name(name: &str) -> bool {
    name.parse::<IpAddr>().is_err() && valid_host(name)
}

/// 리눅스 사용자 이름 `[a-z_][a-z0-9_-]{0,31}`
fn valid_user(user: &str) -> bool {
    let mut bytes = user.bytes();
    bytes
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && user.len() <= 32
        && bytes.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// 한 칸(`ipv6`이면 IPv6 칸)의 주소 설정 검사
fn check_ip(i: usize, ip: &IpConfig, ipv6: bool) -> Result<(), CloudInitError> {
    let family = if ipv6 { "ipv6" } else { "ipv4" };
    let same_family = |a: &IpAddr| a.is_ipv6() == ipv6;
    match ip {
        IpConfig::Dhcp => Ok(()),
        IpConfig::Slaac if ipv6 => Ok(()),
        IpConfig::Slaac => Err(CloudInitError::SlaacIpv4(i)),
        IpConfig::Static { address, gateway } => {
            if !parse_address(address).is_some_and(|a| same_family(&a)) {
                return Err(CloudInitError::BadAddress(i, family, address.clone()));
            }
            if let Some(gw) = gateway
                && !gw.parse::<IpAddr>().is_ok_and(|a| same_family(&a))
            {
                return Err(CloudInitError::BadGateway(i, family, gw.clone()));
            }
            Ok(())
        }
    }
}

/// `주소/접두 길이`의 주소. 보안 그룹의 `Cidr`와 달리 접두 뒤 호스트 비트가 있어야 한다(인터페이스 주소).
fn parse_address(s: &str) -> Option<IpAddr> {
    let (addr, prefix) = s.split_once('/')?;
    let addr: IpAddr = addr.parse().ok()?;
    let max = if addr.is_ipv6() { 128 } else { 32 };
    let digits =
        !prefix.is_empty() && prefix.len() <= 3 && prefix.bytes().all(|c| c.is_ascii_digit());
    (digits && prefix.parse::<u8>().is_ok_and(|p| p <= max)).then_some(addr)
}

/// cloud-init이 해시로 보는 crypt 해시(SHA-512 `$6$`, SHA-256 `$5$`, MD5 `$1$`, bcrypt `$2a$`·`$2y$`)의 모양인지.
/// 아니면 평문으로 본다. 모양을 엄격히 봐 평문이 우연히 해시로 저장되는 일과 너무 긴 값을 막는다.
pub fn is_crypt_hash(s: &str) -> bool {
    let b64 = |t: &str| {
        t.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'/')
    };
    // `[rounds=N$]솔트$해시` — 솔트 1~`salt_max`자, 해시 `hash_len`자
    let salted = |rest: &str, salt_max: usize, hash_len: usize, rounds: bool| {
        let rest = match rest.strip_prefix("rounds=") {
            Some(r) if rounds => match r.split_once('$') {
                Some((n, r))
                    if (1..=9).contains(&n.len()) && n.bytes().all(|c| c.is_ascii_digit()) =>
                {
                    r
                }
                _ => return false,
            },
            _ => rest,
        };
        rest.split_once('$').is_some_and(|(salt, hash)| {
            (1..=salt_max).contains(&salt.len()) && b64(salt) && hash.len() == hash_len && b64(hash)
        })
    };
    if let Some(rest) = s.strip_prefix("$6$") {
        salted(rest, 16, 86, true)
    } else if let Some(rest) = s.strip_prefix("$5$") {
        salted(rest, 16, 43, true)
    } else if let Some(rest) = s.strip_prefix("$1$") {
        salted(rest, 8, 22, false)
    } else if let Some(rest) = s.strip_prefix("$2a$").or_else(|| s.strip_prefix("$2y$")) {
        // `$2a$NN$` 뒤 솔트 22자 + 해시 31자
        rest.split_once('$').is_some_and(|(cost, tail)| {
            cost.len() == 2
                && cost.bytes().all(|c| c.is_ascii_digit())
                && tail.len() == 53
                && b64(tail)
        })
    } else {
        false
    }
}

/// cloud-init이 해시로 보지 않아 평문 비밀번호로 설정해 버리는 crypt 해시(yescrypt `$y$`, bcrypt `$2b$`)인지
fn is_unsupported_hash(s: &str) -> bool {
    s.starts_with("$y$") || s.starts_with("$2b$")
}

/// 응답 JSON에서 cloud-init 비밀번호(해시)를 [`PASSWORD_MASK`]로 가린다. VM은 `spec.cloud_init.password`,
/// 스냅샷·백업은 `spec.vm_spec.cloud_init.password`다. 다른 종류나 비밀번호가 없으면 그대로 둔다.
pub fn redact_vm_json(kind: Kind, value: &mut serde_json::Value) {
    let spec = match kind {
        Kind::Vm => value.get_mut("spec"),
        Kind::Snapshot | Kind::Backup => value.get_mut("spec").and_then(|s| s.get_mut("vm_spec")),
        _ => None,
    };
    if let Some(password) = spec
        .and_then(|s| s.get_mut("cloud_init"))
        .and_then(|c| c.get_mut("password"))
        .filter(|p| p.is_string())
    {
        *password = PASSWORD_MASK.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ci() -> CloudInit {
        CloudInit {
            hostname: Some("web-01.example.com".into()),
            user: Some("orbi".into()),
            password: Some("secret".into()),
            ssh_keys: vec!["ssh-ed25519 AAAAC3Nza orbi@laptop".into()],
            networks: vec![CloudInitNet {
                ipv4: Some(IpConfig::Static {
                    address: "10.0.0.5/24".into(),
                    gateway: Some("10.0.0.1".into()),
                }),
                ipv6: Some(IpConfig::Slaac),
            }],
            dns: Some(CloudInitDns {
                servers: vec!["10.0.0.2".parse().unwrap()],
                search: vec!["example.com".into()],
            }),
            user_data: None,
            vendor_data: Some("#cloud-config\nruncmd: [true]\n".into()),
        }
    }

    #[test]
    fn round_trips_and_omits_empty_fields() {
        let json = serde_json::json!({
            "hostname": "web-01.example.com",
            "user": "orbi",
            "password": "secret",
            "ssh_keys": ["ssh-ed25519 AAAAC3Nza orbi@laptop"],
            "networks": [{
                "ipv4": { "mode": "static", "address": "10.0.0.5/24", "gateway": "10.0.0.1" },
                "ipv6": { "mode": "slaac" }
            }],
            "dns": { "servers": ["10.0.0.2"], "search": ["example.com"] },
            "vendor_data": "#cloud-config\nruncmd: [true]\n"
        });
        let parsed: CloudInit = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(parsed, ci());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), json);
        assert_eq!(
            serde_json::to_value(CloudInit::default()).unwrap(),
            serde_json::json!({})
        );
        assert_eq!(
            serde_json::to_value(IpConfig::Dhcp).unwrap(),
            serde_json::json!({ "mode": "dhcp" })
        );
    }

    #[test]
    fn rejects_typos() {
        for bad in [
            serde_json::json!({ "hostnmae": "a" }),
            serde_json::json!({ "networks": [{ "ipv4": { "mode": "dhcp4" } }] }),
            serde_json::json!({ "networks": [{ "ipv4": { "mode": "static", "adress": "10.0.0.5/24" } }] }),
            serde_json::json!({ "networks": [{ "ip4": { "mode": "dhcp" } }] }),
            serde_json::json!({ "dns": { "server": ["10.0.0.2"] } }),
            serde_json::json!({ "dns": { "servers": ["dns.example.com"] } }),
        ] {
            assert!(
                serde_json::from_value::<CloudInit>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validate_accepts_full_settings() {
        assert_eq!(ci().validate(1), Ok(()));
        assert_eq!(CloudInit::default().validate(0), Ok(()));
        // crypt 해시는 그대로 받는다.
        let hash = format!("$6$salt${}", "x".repeat(86));
        let c = CloudInit {
            password: Some(hash),
            ..ci()
        };
        assert_eq!(c.validate(1), Ok(()));
    }

    #[test]
    fn validate_checks_names_and_users() {
        let with = |f: &dyn Fn(&mut CloudInit)| {
            let mut c = ci();
            f(&mut c);
            c.validate(1)
        };
        for bad in ["-web", "web_01", "a..b", "10.0.0.5", &"a".repeat(64), ""] {
            assert_eq!(
                with(&|c| c.hostname = Some(bad.into())),
                Err(CloudInitError::Hostname(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(with(&|c| c.hostname = Some("Web-01".into())), Ok(()));
        let long_fqdn = vec!["a".repeat(63); 4].join(".");
        assert_eq!(long_fqdn.len(), 255);
        assert_eq!(
            with(&|c| c.hostname = Some(long_fqdn.clone())),
            Err(CloudInitError::Hostname(long_fqdn.clone()))
        );
        for bad in ["Orbi", "1orbi", "", "or bi", &"a".repeat(33)] {
            assert_eq!(
                with(&|c| c.user = Some(bad.into())),
                Err(CloudInitError::User(bad.into())),
                "{bad}"
            );
        }
        assert_eq!(with(&|c| c.user = Some("_svc-1".into())), Ok(()));
        assert_eq!(
            with(&|c| c.user = Some("root".into())),
            Err(CloudInitError::RootUser)
        );
        assert_eq!(
            CloudInitError::RootUser.to_string(),
            "cloud_init.user: use ssh_keys with the default user instead of root"
        );
        assert_eq!(
            with(&|c| c.password = Some(String::new())),
            Err(CloudInitError::EmptyPassword)
        );
        assert_eq!(with(&|c| c.password = Some("p".repeat(256))), Ok(()));
        assert_eq!(
            with(&|c| c.password = Some("p".repeat(257))),
            Err(CloudInitError::LongPassword)
        );
        assert_eq!(
            CloudInitError::LongPassword.to_string(),
            "cloud_init.password is longer than 256 bytes"
        );
        // 해시 모양이어도 엄격한 모양이 아니면 평문이라 길이 상한을 받는다(씨앗이 너무 커지지 않게).
        assert_eq!(
            with(&|c| c.password = Some(format!("$6$salt${}", "a".repeat(600)))),
            Err(CloudInitError::LongPassword)
        );
        for hash in ["$y$j9T$salt$hash", "$2b$10$abc$def"] {
            assert_eq!(
                with(&|c| c.password = Some(hash.into())),
                Err(CloudInitError::UnsupportedHash),
                "{hash}"
            );
        }
        assert_eq!(
            CloudInitError::UnsupportedHash.to_string(),
            "cloud_init.password: cloud-init cannot use yescrypt/bcrypt-2b hashes; use a SHA-512 \
             crypt hash ($6$, e.g. `mkpasswd -m sha-512`) or a plain password; plain passwords may \
             not start with $y$ or $2b$"
        );
    }

    #[test]
    fn validate_checks_ssh_keys() {
        let with_keys = |keys: Vec<String>| {
            CloudInit {
                ssh_keys: keys,
                ..ci()
            }
            .validate(1)
        };
        for good in [
            "ssh-rsa AAAAB3 a@b",
            "ecdsa-sha2-nistp384 AAAA",
            "sk-ssh-ed25519@openssh.com AAAA",
            "sk-ecdsa-sha2-nistp256@openssh.com AAAA",
        ] {
            assert_eq!(with_keys(vec![good.into()]), Ok(()), "{good}");
        }
        for bad in [
            "ssh-dss AAAA",
            "ssh-ed25519",
            "ssh-ed25519 AAAA\nssh-rsa BBBB",
            " ssh-ed25519 AAAA",
        ] {
            assert_eq!(
                with_keys(vec!["ssh-rsa A".into(), bad.into()]),
                Err(CloudInitError::BadKey(1)),
                "{bad}"
            );
        }
        let key = "ssh-ed25519 AAAA".to_string();
        assert_eq!(with_keys(vec![key.clone(); 32]), Ok(()));
        assert_eq!(with_keys(vec![key; 33]), Err(CloudInitError::TooManyKeys));
        let big = format!("ssh-rsa {}", "A".repeat(8 * 1024));
        assert_eq!(
            with_keys(vec![big.clone(), big]),
            Err(CloudInitError::KeysTooLarge)
        );
    }

    #[test]
    fn validate_checks_networks() {
        let with_net = |net: CloudInitNet, nics: usize| {
            CloudInit {
                networks: vec![CloudInitNet::default(), net],
                ..ci()
            }
            .validate(nics)
        };
        let fixed = |address: &str, gateway: Option<&str>| {
            Some(IpConfig::Static {
                address: address.into(),
                gateway: gateway.map(Into::into),
            })
        };
        assert_eq!(with_net(CloudInitNet::default(), 2), Ok(()));
        assert_eq!(
            with_net(CloudInitNet::default(), 1),
            Err(CloudInitError::TooManyNetworks(2, 1))
        );
        assert_eq!(
            CloudInitError::TooManyNetworks(2, 1).to_string(),
            "cloud_init.networks has 2 entries but the VM has 1 NICs"
        );
        let v6 = CloudInitNet {
            ipv4: None,
            ipv6: fixed("2001:db8::5/64", Some("2001:db8::1")),
        };
        assert_eq!(with_net(v6, 2), Ok(()));
        // IPv4 칸에 IPv6 주소, 접두 없음, 접두가 너무 김
        for bad in [
            "2001:db8::5/64",
            "10.0.0.5",
            "10.0.0.5/33",
            "10.0.0.5/",
            "x/24",
        ] {
            let net = CloudInitNet {
                ipv4: fixed(bad, None),
                ipv6: None,
            };
            assert_eq!(
                with_net(net, 2),
                Err(CloudInitError::BadAddress(1, "ipv4", bad.into())),
                "{bad}"
            );
        }
        assert_eq!(
            CloudInitError::BadAddress(1, "ipv4", "10.0.0.5".into()).to_string(),
            "cloud_init.networks[1].ipv4: \"10.0.0.5\" is not an ipv4 address with a prefix length"
        );
        let net = CloudInitNet {
            ipv4: fixed("10.0.0.5/24", Some("2001:db8::1")),
            ipv6: None,
        };
        assert_eq!(
            with_net(net, 2),
            Err(CloudInitError::BadGateway(1, "ipv4", "2001:db8::1".into()))
        );
        let net = CloudInitNet {
            ipv4: None,
            ipv6: fixed("2001:db8::5/64", Some("10.0.0.1")),
        };
        assert_eq!(
            with_net(net, 2),
            Err(CloudInitError::BadGateway(1, "ipv6", "10.0.0.1".into()))
        );
        let net = CloudInitNet {
            ipv4: Some(IpConfig::Slaac),
            ipv6: None,
        };
        assert_eq!(with_net(net, 2), Err(CloudInitError::SlaacIpv4(1)));
    }

    #[test]
    fn validate_checks_dns_and_data() {
        let with = |f: &dyn Fn(&mut CloudInit)| {
            let mut c = ci();
            f(&mut c);
            c.validate(1)
        };
        assert_eq!(
            with(&|c| c.dns.as_mut().unwrap().servers = vec!["10.0.0.2".parse().unwrap(); 4]),
            Err(CloudInitError::TooManyDnsServers)
        );
        assert_eq!(
            with(&|c| c.dns.as_mut().unwrap().search = vec!["a.example".into(); 7]),
            Err(CloudInitError::TooManySearch)
        );
        assert_eq!(
            with(&|c| c.dns.as_mut().unwrap().search = vec!["bad_domain".into()]),
            Err(CloudInitError::BadSearch("bad_domain".into()))
        );
        assert_eq!(
            with(&|c| c.user_data = Some("x".repeat(MAX_DATA_BYTES))),
            Ok(())
        );
        assert_eq!(
            with(&|c| c.user_data = Some("x".repeat(MAX_DATA_BYTES + 1))),
            Err(CloudInitError::DataTooLarge("user_data"))
        );
        assert_eq!(
            with(&|c| c.vendor_data = Some("x".repeat(MAX_DATA_BYTES + 1))),
            Err(CloudInitError::DataTooLarge("vendor_data"))
        );
        assert_eq!(
            CloudInitError::DataTooLarge("user_data").to_string(),
            "cloud_init.user_data is larger than 64 KiB"
        );
    }

    #[test]
    fn crypt_hashes_are_recognised() {
        let sha512 = "svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";
        for hash in [
            format!("$6$saltstring${sha512}"),
            format!("$6$rounds=10000$saltstringsaltst${sha512}"),
            "$5$saltstring$C3o4O1TC6aRHF4FI.QSZMXtHbaj2gSXr4sUc/3NcUi.".into(),
            "$5$rounds=5000$toolongsaltstrin$Un/5jzAHMgOGZ5.mWJpuVolil07guHPvOW8mGRcvxa5".into(),
            "$1$saltsalt$9xy1btjgzLYfb7hivXtC//".into(),
            "$2a$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy".into(),
            "$2y$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy".into(),
        ] {
            assert!(is_crypt_hash(&hash), "{hash}");
        }
        for plain in [
            "secret".to_string(),
            "$6$nosalt".into(),
            "$1$salt$md5".into(),
            "6$a$b$c".into(),
            "$5$rounds=5000$salt$hash".into(),
            // cloud-init이 해시로 보지 않는 것
            "$y$j9T$salt$hash".into(),
            "$2b$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy".into(),
            // 솔트가 17자, 해시가 한 자 모자람·넘침, 해시에 쓰지 않는 문자
            format!("$6$saltstringsaltstr${sha512}"),
            format!("$6$saltstring${}", &sha512[1..]),
            format!("$6$saltstring${sha512}x"),
            format!("$6$salt string${sha512}"),
            format!("$6$rounds=x$saltstring${sha512}"),
            // 해시 모양이어도 아주 긴 값
            format!("$6$saltstring${}", "a".repeat(600)),
            PASSWORD_MASK.into(),
            String::new(),
        ] {
            assert!(!is_crypt_hash(&plain), "{plain}");
        }
    }

    #[test]
    fn redacts_passwords_in_vm_snapshot_and_backup_json() {
        let mut vm = serde_json::json!({
            "name": "web", "spec": { "cloud_init": { "user": "orbi", "password": "$6$s$h" } }
        });
        redact_vm_json(Kind::Vm, &mut vm);
        assert_eq!(vm["spec"]["cloud_init"]["password"], PASSWORD_MASK);
        assert_eq!(vm["spec"]["cloud_init"]["user"], "orbi");

        for kind in [Kind::Snapshot, Kind::Backup] {
            let mut v = serde_json::json!({
                "spec": { "vm_spec": { "cloud_init": { "password": "$6$s$h" } } }
            });
            redact_vm_json(kind, &mut v);
            assert_eq!(
                v["spec"]["vm_spec"]["cloud_init"]["password"],
                PASSWORD_MASK
            );
        }

        // 비밀번호가 없거나 cloud-init이 없거나 다른 종류면 그대로다.
        for (kind, json) in [
            (
                Kind::Vm,
                serde_json::json!({ "spec": { "cloud_init": { "user": "orbi" } } }),
            ),
            (
                Kind::Vm,
                serde_json::json!({ "spec": { "memory_mib": 512 } }),
            ),
            (
                Kind::Vm,
                serde_json::json!({ "spec": { "cloud_init": { "password": null } } }),
            ),
            (
                Kind::Snapshot,
                serde_json::json!({ "spec": { "vm_spec": {} } }),
            ),
            (
                Kind::Volume,
                serde_json::json!({ "spec": { "cloud_init": { "password": "x" } } }),
            ),
        ] {
            let mut v = json.clone();
            redact_vm_json(kind, &mut v);
            assert_eq!(v, json);
        }
    }
}
