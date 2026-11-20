//! VM의 cloud-init 설정(계획 57). Orbivisor가 이것으로 NoCloud 씨앗(meta-data·user-data·network-config·
//! vendor-data)을 만들어 VM에 읽기 전용 CD로 넣는다.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::host::valid_host;
use crate::network::{Ipv6Mode, NetworkSpec};
use crate::resource::Kind;
use crate::vm::NicSpec;

/// SDN 네트워크(이름)마다 그 스펙과 라우터 인터페이스인지(계획 65·66). network-config가 SDN NIC의 주소 설정을 IPAM
/// 주소(`NicSpec.ips`)와 서브넷으로 만든다. 단순 네트워크는 넣지 않는다.
pub type SdnSubnets = BTreeMap<String, SdnNicNetwork>;

/// SDN NIC 하나가 붙은 네트워크(계획 66): 스펙(서브넷·공급자)과 라우터 인터페이스인지(게이트웨이·RA가 있음)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SdnNicNetwork {
    pub spec: NetworkSpec,
    #[serde(default)]
    pub routed: bool,
}

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

/// Orbivisor가 만든 NoCloud 씨앗(파일 내용). 드라이버가 ISO 9660(라벨 `cidata`)으로 써서 VM에 넣는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudInitSeed {
    /// meta-data의 `instance-id`(`iid-` + 내용 해시 16자). 내용이 바뀌면 바뀌어 cloud-init이 다시 적용한다.
    pub instance_id: String,
    pub meta_data: String,
    pub user_data: String,
    pub vendor_data: Option<String>,
    pub network_config: Option<String>,
}

/// meta-data 파일
#[derive(Serialize)]
struct MetaData<'a> {
    #[serde(rename = "instance-id")]
    instance_id: &'a str,
    #[serde(rename = "local-hostname")]
    local_hostname: &'a str,
}

/// 만든 user-data(`#cloud-config`)
#[derive(Serialize)]
struct UserData<'a> {
    hostname: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    fqdn: Option<&'a str>,
    manage_etc_hosts: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chpasswd: Option<Chpasswd>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_pwauth: Option<bool>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    ssh_authorized_keys: &'a [String],
}

#[derive(Serialize)]
struct Chpasswd {
    expire: bool,
}

/// network-config 버전 1
#[derive(Serialize)]
struct NetworkConfig<'a> {
    version: u8,
    config: Vec<NetEntry<'a>>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum NetEntry<'a> {
    Physical {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mac_address: Option<String>,
        subnets: Vec<Subnet<'a>>,
    },
    Nameserver {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        address: Vec<String>,
        #[serde(skip_serializing_if = "<[String]>::is_empty")]
        search: &'a [String],
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Subnet<'a> {
    Dhcp,
    Dhcp6,
    #[serde(rename = "ipv6_slaac")]
    Ipv6Slaac,
    /// 계획 66: 주소는 RA(SLAAC), 나머지는 DHCPv6
    #[serde(rename = "ipv6_dhcpv6-stateless")]
    Ipv6Dhcpv6Stateless,
    /// 계획 66: 주소도 DHCPv6
    #[serde(rename = "ipv6_dhcpv6-stateful")]
    Ipv6Dhcpv6Stateful,
    Static {
        address: std::borrow::Cow<'a, str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gateway: Option<std::borrow::Cow<'a, str>>,
        /// SDN 서브넷의 DNS 서버(계획 65, DHCP가 없는 서브넷). 비면 쓰지 않는다.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dns_nameservers: Vec<String>,
    },
    Static6 {
        address: std::borrow::Cow<'a, str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        gateway: Option<std::borrow::Cow<'a, str>>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        dns_nameservers: Vec<String>,
    },
}

/// VM `vm_id`(이름 `vm_name`, NIC `nics`)의 cloud-init 설정 `ci`로 NoCloud 씨앗을 만든다. 같은 입력이면 같은
/// 바이트다. YAML 파일은 JSON으로 쓴다(JSON은 YAML이고 cloud-init이 그대로 읽는다). SDN NIC의 서브넷을 모를 때(핫플러그의
/// instance-id 견주기)의 꼴이다 — instance-id는 [`render_sdn`]과 같다.
pub fn render(vm_id: Uuid, vm_name: &str, ci: &CloudInit, nics: &[NicSpec]) -> CloudInitSeed {
    render_sdn(vm_id, vm_name, ci, nics, &SdnSubnets::new())
}

/// [`render`]에 SDN 네트워크의 서브넷 `sdn`을 더한 것(계획 65 §9): SDN NIC의 network-config를 IPAM 주소로 만든다.
/// instance-id는 서브넷이 아니라 NIC 주소(`ips`)로 셈한다 — 그래서 서브넷을 모르는 핫플러그의 견주기
/// (`hotplug::seed_matches`)와 같은 값이고, 주소가 바뀌면 바뀐다. 서브넷의 DHCP·DNS만 바꾸면 instance-id는 그대로다
/// (게스트가 이미 적용한 설정을 다시 적용하지 않는다). 주소가 없는 VM은 계획 64와 바이트까지 같다.
pub fn render_sdn(
    vm_id: Uuid,
    vm_name: &str,
    ci: &CloudInit,
    nics: &[NicSpec],
    sdn: &SdnSubnets,
) -> CloudInitSeed {
    let host = ci.hostname.as_deref().unwrap_or(vm_name);
    let label = host.split('.').next().unwrap_or(host);
    let user_data = match &ci.user_data {
        Some(custom) => custom.clone(),
        None => {
            let password = ci.password.as_deref();
            let data = UserData {
                hostname: label,
                fqdn: host.contains('.').then_some(host),
                manage_etc_hosts: true,
                user: ci.user.as_deref(),
                password,
                chpasswd: password.map(|_| Chpasswd { expire: false }),
                ssh_pwauth: password.map(|_| true),
                ssh_authorized_keys: &ci.ssh_keys,
            };
            format!("#cloud-config\n{}\n", pretty(&data))
        }
    };
    let network_config = network_config(ci, nics, sdn);
    let key = seed_network_key(ci, nics);
    let instance_id = instance_id(
        vm_id,
        label,
        &user_data,
        ci.vendor_data.as_deref(),
        key.as_deref(),
    );
    let meta_data = pretty(&MetaData {
        instance_id: &instance_id,
        local_hostname: label,
    }) + "\n";
    CloudInitSeed {
        instance_id,
        meta_data,
        user_data,
        vendor_data: ci.vendor_data.clone(),
        network_config,
    }
}

/// 씨앗의 instance-id: VM id와 파일 내용(호스트 이름·user-data·vendor-data·network-config)의 SHA-256 앞 16자.
/// 내용이 같으면 같아 다시 켜도 cloud-init이 인스턴스마다 하는 일을 다시 하지 않는다.
pub fn instance_id(
    vm_id: Uuid,
    local_hostname: &str,
    user_data: &str,
    vendor_data: Option<&str>,
    network_config: Option<&str>,
) -> String {
    let mut c = ring::digest::Context::new(&ring::digest::SHA256);
    c.update(vm_id.as_bytes());
    // 칸마다 있음 표시와 길이를 앞에 둬 경계가 겹치지 않게 한다.
    for part in [
        Some(local_hostname),
        Some(user_data),
        vendor_data,
        network_config,
    ] {
        match part {
            Some(p) => {
                c.update(&[1]);
                c.update(&(p.len() as u64).to_be_bytes());
                c.update(p.as_bytes());
            }
            None => c.update(&[0]),
        }
    }
    let hex: String = c.finish().as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("iid-{hex}")
}

/// instance-id에 넣는 network-config 자리: SDN 서브넷 없이 만든 network-config에, NIC 주소(`ips`)가 하나라도 있으면
/// NIC마다의 주소 줄을 붙인 것. 주소가 없으면 계획 64의 network-config 그대로다.
fn seed_network_key(ci: &CloudInit, nics: &[NicSpec]) -> Option<String> {
    let base = network_config(ci, nics, &SdnSubnets::new());
    if nics.iter().all(|n| n.ips.is_empty()) {
        return base;
    }
    let mut key = base.unwrap_or_default();
    for (i, nic) in nics.iter().enumerate() {
        let ips: Vec<String> = nic.ips.iter().map(ToString::to_string).collect();
        key.push_str(&format!("# nic{i} ips {}\n", ips.join(" ")));
    }
    Some(key)
}

/// SDN NIC 하나의 주소 설정(계획 65 §9, 계획 66 §10): IPv4 주소의 서브넷이 DHCP([`NetworkSpec::subnet_dhcp`])면 `dhcp`,
/// 아니면 `static`(주소/접두사, 서브넷 DNS), IPv6는 `static6`이다. 라우터 인터페이스인 네트워크(RA가 있음)의 `ipv6_mode`
/// 서브넷은 `ipv6_slaac`·`ipv6_dhcpv6-*`다. 고정 주소의 `gateway`는 라우터 인터페이스이거나 공급자 네트워크(물리 라우터)일
/// 때만 적는다(라우터가 없는 오버레이는 계획 65 그대로 바이트까지 같다). 주소가 없으면 IPv4 DHCP다. 서브넷에 들지 않는
/// 주소(서브넷을 뺀 뒤)는 적지 않는다.
fn sdn_subnets<'a>(nic: &NicSpec, net: &SdnNicNetwork) -> Vec<Subnet<'a>> {
    let mut ips = nic.ips.clone();
    ips.sort_by_key(|ip| !ip.is_ipv4());
    if ips.is_empty() {
        return vec![Subnet::Dhcp];
    }
    let spec = &net.spec;
    let with_gateway = net.routed || spec.is_provider();
    ips.iter()
        .filter_map(|ip| {
            let subnet = spec.subnets.iter().find(|s| s.cidr.contains(*ip))?;
            let address = format!("{ip}/{}", subnet.cidr.prefix).into();
            let dns = || subnet.dns_servers.iter().map(ToString::to_string).collect();
            let gateway = with_gateway.then(|| subnet.gateway().to_string().into());
            let mode = subnet.ipv6_mode.filter(|_| net.routed);
            Some(match (ip.is_ipv4(), spec.subnet_dhcp(subnet), mode) {
                (true, true, _) => Subnet::Dhcp,
                (true, false, _) => Subnet::Static {
                    address,
                    gateway,
                    dns_nameservers: dns(),
                },
                (false, _, Some(Ipv6Mode::Slaac)) => Subnet::Ipv6Slaac,
                (false, _, Some(Ipv6Mode::Dhcpv6Stateless)) => Subnet::Ipv6Dhcpv6Stateless,
                (false, _, Some(Ipv6Mode::Dhcpv6Stateful)) => Subnet::Ipv6Dhcpv6Stateful,
                (false, _, None) => Subnet::Static6 {
                    address,
                    gateway,
                    dns_nameservers: dns(),
                },
            })
        })
        .collect()
}

/// network-config. 주소 설정도 DNS도 SDN NIC(`sdn`의 네트워크)도 없으면 파일을 두지 않는다(cloud-init 기본 — 첫 NIC
/// DHCP). 있으면 모든 NIC를 적고, 설정이 없는(항목이 없거나 빈) NIC는 IPv4 DHCP다. IPv6만 설정한 NIC에는 IPv4를 두지
/// 않는다(IPv6 전용 망에 DHCPv4를 기다리지 않게). SDN NIC는 `cloud_init.networks` 대신 IPAM 주소로 적는다
/// ([`sdn_subnets`] — SDN NIC가 있으면 늘 파일이 있다: 첫 NIC DHCP 기본이면 둘째 NIC의 고정 IPv6가 빠진다).
fn network_config(ci: &CloudInit, nics: &[NicSpec], sdn: &SdnSubnets) -> Option<String> {
    let dns = ci
        .dns
        .as_ref()
        .filter(|d| !d.servers.is_empty() || !d.search.is_empty());
    let has_sdn = nics.iter().any(|n| sdn.contains_key(&n.network));
    if ci.networks.is_empty() && dns.is_none() && !has_sdn {
        return None;
    }
    let mut config: Vec<NetEntry> = nics
        .iter()
        .enumerate()
        .map(|(i, nic)| {
            if let Some(net) = sdn.get(&nic.network) {
                return NetEntry::Physical {
                    name: format!("eth{i}"),
                    mac_address: nic.mac.map(|m| m.to_string()),
                    subnets: sdn_subnets(nic, net),
                };
            }
            let net = ci.networks.get(i);
            let ipv6_only = net.is_some_and(|n| n.ipv4.is_none() && n.ipv6.is_some());
            let mut subnets = Vec::new();
            if !ipv6_only {
                subnets.push(match net.and_then(|n| n.ipv4.as_ref()) {
                    None | Some(IpConfig::Dhcp) | Some(IpConfig::Slaac) => Subnet::Dhcp,
                    Some(IpConfig::Static { address, gateway }) => Subnet::Static {
                        address: address.as_str().into(),
                        gateway: gateway.as_deref().map(Into::into),
                        dns_nameservers: Vec::new(),
                    },
                });
            }
            match net.and_then(|n| n.ipv6.as_ref()) {
                None => {}
                Some(IpConfig::Dhcp) => subnets.push(Subnet::Dhcp6),
                Some(IpConfig::Slaac) => subnets.push(Subnet::Ipv6Slaac),
                Some(IpConfig::Static { address, gateway }) => subnets.push(Subnet::Static6 {
                    address: address.as_str().into(),
                    gateway: gateway.as_deref().map(Into::into),
                    dns_nameservers: Vec::new(),
                }),
            }
            NetEntry::Physical {
                name: format!("eth{i}"),
                mac_address: nic.mac.map(|m| m.to_string()),
                subnets,
            }
        })
        .collect();
    if let Some(dns) = dns {
        config.push(NetEntry::Nameserver {
            address: dns.servers.iter().map(ToString::to_string).collect(),
            search: &dns.search,
        });
    }
    Some(pretty(&NetworkConfig { version: 1, config }) + "\n")
}

fn pretty(v: &impl Serialize) -> String {
    serde_json::to_string_pretty(v).expect("cloud-init files serialize")
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

    /// 두 NIC VM(MAC 고정)
    fn nics() -> Vec<NicSpec> {
        serde_json::from_value(serde_json::json!([
            { "network": "lan", "model": "virtio", "mac": "52:54:00:12:34:56" },
            { "network": "dmz", "model": "virtio", "mac": "52:54:00:12:34:57" }
        ]))
        .unwrap()
    }

    const HASH: &str = "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";

    fn hashed() -> CloudInit {
        CloudInit {
            password: Some(HASH.into()),
            ..ci()
        }
    }

    #[test]
    fn renders_the_four_nocloud_files() {
        let seed = render(Uuid::nil(), "web", &hashed(), &nics());
        assert_eq!(seed.instance_id, "iid-0cd5141d80a257cf");
        assert_eq!(
            seed.meta_data,
            "{\n  \"instance-id\": \"iid-0cd5141d80a257cf\",\n  \"local-hostname\": \"web-01\"\n}\n"
        );
        let want_user = format!(
            r#"#cloud-config
{{
  "hostname": "web-01",
  "fqdn": "web-01.example.com",
  "manage_etc_hosts": true,
  "user": "orbi",
  "password": "{HASH}",
  "chpasswd": {{
    "expire": false
  }},
  "ssh_pwauth": true,
  "ssh_authorized_keys": [
    "ssh-ed25519 AAAAC3Nza orbi@laptop"
  ]
}}
"#
        );
        assert_eq!(seed.user_data, want_user);
        assert_eq!(
            seed.vendor_data.as_deref(),
            Some("#cloud-config\nruncmd: [true]\n")
        );
        let want_net = r#"{
  "version": 1,
  "config": [
    {
      "type": "physical",
      "name": "eth0",
      "mac_address": "52:54:00:12:34:56",
      "subnets": [
        {
          "type": "static",
          "address": "10.0.0.5/24",
          "gateway": "10.0.0.1"
        },
        {
          "type": "ipv6_slaac"
        }
      ]
    },
    {
      "type": "physical",
      "name": "eth1",
      "mac_address": "52:54:00:12:34:57",
      "subnets": [
        {
          "type": "dhcp"
        }
      ]
    },
    {
      "type": "nameserver",
      "address": [
        "10.0.0.2"
      ],
      "search": [
        "example.com"
      ]
    }
  ]
}
"#;
        assert_eq!(seed.network_config.as_deref(), Some(want_net));
        // user-data는 첫 줄 `#cloud-config` 뒤가 JSON(= YAML)이다.
        let (head, body) = seed.user_data.split_once('\n').unwrap();
        assert_eq!(head, "#cloud-config");
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["password"], HASH);
        let net: serde_json::Value =
            serde_json::from_str(seed.network_config.as_deref().unwrap()).unwrap();
        assert_eq!(net["config"][0]["subnets"][0]["address"], "10.0.0.5/24");
    }

    #[test]
    fn minimal_settings_use_the_vm_name_and_distribution_defaults() {
        let seed = render(Uuid::nil(), "web", &CloudInit::default(), &nics());
        assert_eq!(
            seed.user_data,
            "#cloud-config\n{\n  \"hostname\": \"web\",\n  \"manage_etc_hosts\": true\n}\n"
        );
        assert!(seed.meta_data.contains("\"local-hostname\": \"web\""));
        // 비밀번호가 없으면 ssh_pwauth를 적지 않는다. 주소 설정·DNS가 없으면 network-config도 없다.
        assert!(!seed.user_data.contains("ssh_pwauth"));
        assert_eq!(seed.network_config, None);
        assert_eq!(seed.vendor_data, None);
        let no_password = CloudInit {
            password: None,
            ..ci()
        };
        let seed = render(Uuid::nil(), "web", &no_password, &nics());
        assert!(!seed.user_data.contains("ssh_pwauth"));
        assert!(!seed.user_data.contains("chpasswd"));
    }

    #[test]
    fn custom_user_data_replaces_the_generated_one() {
        let custom = "#cloud-config\npackages: [nginx]\n";
        let c = CloudInit {
            user_data: Some(custom.into()),
            ..hashed()
        };
        let seed = render(Uuid::nil(), "web", &c, &nics());
        assert_eq!(seed.user_data, custom);
        // 호스트 이름은 meta-data에 남는다.
        assert!(seed.meta_data.contains("\"local-hostname\": \"web-01\""));
    }

    #[test]
    fn every_nic_is_written_once_any_address_is_set() {
        // 둘째 NIC만 정적 IPv6(IPv4 없음 — DHCPv4를 넣지 않음), 첫 NIC는 항목이 비어 DHCP
        let c = CloudInit {
            networks: vec![
                CloudInitNet::default(),
                CloudInitNet {
                    ipv4: None,
                    ipv6: Some(IpConfig::Static {
                        address: "2001:db8::5/64".into(),
                        gateway: None,
                    }),
                },
            ],
            dns: None,
            ..CloudInit::default()
        };
        let seed = render(Uuid::nil(), "web", &c, &nics());
        let net: serde_json::Value =
            serde_json::from_str(seed.network_config.as_deref().unwrap()).unwrap();
        assert_eq!(
            net["config"],
            serde_json::json!([
                { "type": "physical", "name": "eth0", "mac_address": "52:54:00:12:34:56",
                  "subnets": [{ "type": "dhcp" }] },
                { "type": "physical", "name": "eth1", "mac_address": "52:54:00:12:34:57",
                  "subnets": [{ "type": "static6", "address": "2001:db8::5/64" }] }
            ])
        );
        // DHCPv4와 DHCPv6, 그리고 DNS만 있어도 파일을 쓴다(모든 NIC IPv4 DHCP).
        let c = CloudInit {
            networks: vec![CloudInitNet {
                ipv4: Some(IpConfig::Dhcp),
                ipv6: Some(IpConfig::Dhcp),
            }],
            ..CloudInit::default()
        };
        let net: serde_json::Value = serde_json::from_str(
            &render(Uuid::nil(), "web", &c, &nics())
                .network_config
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            net["config"][0]["subnets"],
            serde_json::json!([{ "type": "dhcp" }, { "type": "dhcp6" }])
        );
        let c = CloudInit {
            dns: Some(CloudInitDns {
                servers: vec!["2001:db8::53".parse().unwrap()],
                search: vec![],
            }),
            ..CloudInit::default()
        };
        let net: serde_json::Value = serde_json::from_str(
            &render(Uuid::nil(), "web", &c, &nics())
                .network_config
                .unwrap(),
        )
        .unwrap();
        assert_eq!(net["config"].as_array().unwrap().len(), 3);
        assert_eq!(
            net["config"][2],
            serde_json::json!({ "type": "nameserver", "address": ["2001:db8::53"] })
        );
    }

    #[test]
    fn sdn_nics_take_their_addresses_from_ipam() {
        // eth0: SDN(DHCP 서브넷 + 고정 IPv6), eth1: 단순, eth2: SDN(DHCP 없는 서브넷), eth3: 주소 없는 SDN
        let nics: Vec<NicSpec> = serde_json::from_value(serde_json::json!([
            { "network": "tenant", "model": "virtio", "mac": "52:54:00:00:00:01",
              "ips": ["2001:db8::5", "10.0.0.5"] },
            { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:02" },
            { "network": "static", "model": "virtio", "mac": "52:54:00:00:00:03",
              "ips": ["10.9.0.7"] },
            { "network": "bare", "model": "virtio", "mac": "52:54:00:00:00:04" }
        ]))
        .unwrap();
        let sdn: SdnSubnets = serde_json::from_value(serde_json::json!({
            "tenant": { "spec": { "sdn": {}, "subnets": [
                { "cidr": "10.0.0.0/24" }, { "cidr": "2001:db8::/64", "dns_servers": ["2001:db8::53"] }
            ] } },
            "static": { "spec": { "sdn": {}, "subnets": [
                { "cidr": "10.9.0.0/16", "dhcp": false, "dns_servers": ["10.9.0.53"] }
            ] } },
            "bare": { "spec": { "sdn": {} } }
        }))
        .unwrap();
        // 주소 설정이 없어도 SDN NIC가 있으면 파일이 있다. 단순 NIC는 지금처럼 DHCP다.
        let c = CloudInit::default();
        let seed = render_sdn(Uuid::nil(), "web", &c, &nics, &sdn);
        let net: serde_json::Value =
            serde_json::from_str(seed.network_config.as_deref().unwrap()).unwrap();
        let subnets: Vec<_> = (0..4)
            .map(|i| net["config"][i]["subnets"].clone())
            .collect();
        assert_eq!(
            subnets,
            [
                serde_json::json!([
                    { "type": "dhcp" },
                    { "type": "static6", "address": "2001:db8::5/64", "dns_nameservers": ["2001:db8::53"] }
                ]),
                serde_json::json!([{ "type": "dhcp" }]),
                serde_json::json!([
                    { "type": "static", "address": "10.9.0.7/16", "dns_nameservers": ["10.9.0.53"] }
                ]),
                serde_json::json!([{ "type": "dhcp" }]),
            ]
        );
        // instance-id는 서브넷을 모르는 견주기(핫플러그)와 같고, 주소가 바뀌면 바뀐다.
        assert_eq!(
            seed.instance_id,
            render(Uuid::nil(), "web", &c, &nics).instance_id
        );
        let mut moved = nics.clone();
        moved[0].ips[1] = "10.0.0.6".parse().unwrap();
        assert_ne!(
            seed.instance_id,
            render_sdn(Uuid::nil(), "web", &c, &moved, &sdn).instance_id
        );
        // 주소가 없는 VM의 씨앗은 SDN 서브넷과 상관없이 계획 64와 같다(SDN NIC가 없으면 바이트까지).
        let plain = render(Uuid::nil(), "web", &hashed(), &super::tests::nics());
        assert_eq!(
            plain,
            render_sdn(Uuid::nil(), "web", &hashed(), &super::tests::nics(), &sdn)
        );
    }

    /// 계획 66 §10: 라우터 인터페이스인 네트워크의 고정 주소에는 게이트웨이, `ipv6_mode`는 RA·DHCPv6 항목. 라우터가 없으면
    /// `ipv6_mode`가 있어도 고정 주소(RA가 없다), 공급자 네트워크는 물리 라우터를 게이트웨이로 적고 DHCP는 기본 꺼짐이다.
    #[test]
    fn routed_and_provider_nics_get_gateways_and_ipv6_modes() {
        let nics: Vec<NicSpec> = serde_json::from_value(serde_json::json!([
            { "network": "routed", "model": "virtio", "mac": "52:54:00:00:00:01",
              "ips": ["10.9.0.7", "2001:db8::5054:ff:fe00:1"] },
            { "network": "stateful", "model": "virtio", "mac": "52:54:00:00:00:02",
              "ips": ["2001:db8:1::10"] },
            { "network": "stateless", "model": "virtio", "mac": "52:54:00:00:00:03",
              "ips": ["2001:db8:2::5054:ff:fe00:3"] },
            { "network": "unrouted", "model": "virtio", "mac": "52:54:00:00:00:04",
              "ips": ["10.8.0.7", "2001:db8:3::5054:ff:fe00:4"] },
            { "network": "ext", "model": "virtio", "mac": "52:54:00:00:00:05",
              "ips": ["203.0.113.10"] }
        ]))
        .unwrap();
        let sdn: SdnSubnets = serde_json::from_value(serde_json::json!({
            "routed": { "routed": true, "spec": { "sdn": {}, "subnets": [
                { "cidr": "10.9.0.0/24", "dhcp": false },
                { "cidr": "2001:db8::/64", "ipv6_mode": "slaac" }
            ] } },
            "stateful": { "routed": true, "spec": { "sdn": {}, "subnets": [
                { "cidr": "2001:db8:1::/64", "ipv6_mode": "dhcpv6-stateful" }
            ] } },
            "stateless": { "routed": true, "spec": { "sdn": {}, "subnets": [
                { "cidr": "2001:db8:2::/64", "ipv6_mode": "dhcpv6-stateless" }
            ] } },
            "unrouted": { "spec": { "sdn": {}, "subnets": [
                { "cidr": "10.8.0.0/24", "dhcp": false },
                { "cidr": "2001:db8:3::/64", "ipv6_mode": "slaac" }
            ] } },
            "ext": { "spec": { "sdn": { "provider": { "physnet": "ext" } }, "subnets": [
                { "cidr": "203.0.113.0/24" }
            ] } }
        }))
        .unwrap();
        let seed = render_sdn(Uuid::nil(), "web", &CloudInit::default(), &nics, &sdn);
        let net: serde_json::Value =
            serde_json::from_str(seed.network_config.as_deref().unwrap()).unwrap();
        let subnets: Vec<_> = (0..5)
            .map(|i| net["config"][i]["subnets"].clone())
            .collect();
        assert_eq!(
            subnets,
            [
                serde_json::json!([
                    { "type": "static", "address": "10.9.0.7/24", "gateway": "10.9.0.1" },
                    { "type": "ipv6_slaac" }
                ]),
                serde_json::json!([{ "type": "ipv6_dhcpv6-stateful" }]),
                serde_json::json!([{ "type": "ipv6_dhcpv6-stateless" }]),
                serde_json::json!([
                    { "type": "static", "address": "10.8.0.7/24" },
                    { "type": "static6", "address": "2001:db8:3:0:5054:ff:fe00:4/64" }
                ]),
                serde_json::json!([
                    { "type": "static", "address": "203.0.113.10/24", "gateway": "203.0.113.1" }
                ]),
            ]
        );
        // instance-id는 NIC 주소로만 센다(라우터를 붙여도 그대로).
        let mut unrouted = sdn.clone();
        for n in unrouted.values_mut() {
            n.routed = false;
        }
        assert_eq!(
            seed.instance_id,
            render_sdn(Uuid::nil(), "web", &CloudInit::default(), &nics, &unrouted).instance_id
        );
        assert_ne!(
            seed.network_config,
            render_sdn(Uuid::nil(), "web", &CloudInit::default(), &nics, &unrouted).network_config
        );
    }

    #[test]
    fn a_plain_hostname_has_no_fqdn() {
        let c = CloudInit {
            hostname: Some("db".into()),
            ..CloudInit::default()
        };
        let seed = render(Uuid::nil(), "web", &c, &[]);
        assert!(seed.user_data.contains("\"hostname\": \"db\""));
        assert!(!seed.user_data.contains("fqdn"));
    }

    #[test]
    fn instance_id_follows_the_content() {
        let id = |vm: Uuid, name: &str, c: &CloudInit| render(vm, name, c, &nics()).instance_id;
        let a = id(Uuid::nil(), "web", &hashed());
        assert_eq!(a.len(), "iid-".len() + 16);
        assert!(a.starts_with("iid-"));
        // 같은 입력이면 같다.
        assert_eq!(a, id(Uuid::nil(), "web", &hashed()));
        // 호스트 이름, VM id, vendor-data, 주소가 바뀌면 바뀐다.
        let host = CloudInit {
            hostname: Some("web-02".into()),
            ..hashed()
        };
        assert_ne!(a, id(Uuid::nil(), "web", &host));
        assert_ne!(a, id(Uuid::from_u128(1), "web", &hashed()));
        let vendor = CloudInit {
            vendor_data: None,
            ..hashed()
        };
        assert_ne!(a, id(Uuid::nil(), "web", &vendor));
        // 호스트 이름이 없으면 VM 이름이 내용이다.
        let bare = CloudInit::default();
        assert_ne!(id(Uuid::nil(), "web", &bare), id(Uuid::nil(), "db", &bare));
        // 사용자 user-data만 쓸 때도 호스트 이름이 바뀌면 바뀐다(meta-data에만 있음).
        let custom = |h: &str| CloudInit {
            hostname: Some(h.into()),
            user_data: Some("#cloud-config\n".into()),
            ..CloudInit::default()
        };
        assert_ne!(
            id(Uuid::nil(), "web", &custom("a")),
            id(Uuid::nil(), "web", &custom("b"))
        );
        // 칸 경계가 섞이지 않는다.
        assert_ne!(
            instance_id(Uuid::nil(), "a", "b", None, None),
            instance_id(Uuid::nil(), "ab", "", None, None)
        );
        assert_ne!(
            instance_id(Uuid::nil(), "a", "b", Some(""), None),
            instance_id(Uuid::nil(), "a", "b", None, Some(""))
        );
    }
}
