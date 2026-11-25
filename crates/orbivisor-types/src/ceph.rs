//! Ceph 자동 구성(계획 56): Orbivisor 클러스터 하나에 Orbivisor가 노드에 설치·구성하는 Ceph 클러스터 하나.
//! `CephCluster` 리소스(이름 `ceph` 하나뿐)가 fsid·네트워크·키와 mon·mgr·OSD 노드를 담는다. OSD 하나하나와
//! health·PG는 Raft에 두지 않고 Ceph에서 그때그때 읽는다([`CephReport`]).

use std::fmt;
use std::net::IpAddr;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::Node;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::security_group::Cidr;

pub type CephCluster = Resource<CephSpec, CephStatus>;

/// `CephCluster` 리소스의 이름(클러스터에 하나뿐)
pub const CEPH_NAME: &str = "ceph";
/// 노드가 RBD 풀에 붙는 Ceph 사용자(`client.orbivisor`)의 id
pub const CLIENT_ID: &str = "orbivisor";
/// mon 수의 상한
pub const MAX_MONS: usize = 5;
/// OSD를 둘 수 있는 디스크의 최소 크기(5 GiB)
pub const MIN_OSD_BYTES: u64 = 5 << 30;
/// 구성할 수 있는 Ceph의 가장 낮은 판(18 = reef)
pub const MIN_MAJOR: u32 = 18;

/// 만든 뒤 바뀌지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CephSpec {
    pub fsid: Uuid,
    /// mon과 클라이언트가 쓰는 망
    pub public_network: Cidr,
    /// OSD 복제 트래픽 망(ceph.conf에만)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_network: Option<Cidr>,
    /// API 응답·watch에 내보내지 않는다.
    pub keys: CephKeys,
}

impl ResourceSpec for CephSpec {
    const KIND: Kind = Kind::Ceph;
    type Status = CephStatus;
}

/// Ceph 비밀 키(base64, `ceph-authtool --gen-print-key` 모양)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CephKeys {
    /// `mon.`
    pub mon: String,
    /// `client.admin`
    pub admin: String,
    /// `client.bootstrap-osd`
    pub bootstrap_osd: String,
    /// `client.orbivisor`
    pub client: String,
}

// 키가 로그에 나가지 않게 가린다.
impl fmt::Debug for CephKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CephKeys")
            .field("mon", &"***")
            .field("admin", &"***")
            .field("bootstrap_osd", &"***")
            .field("client", &"***")
            .finish()
    }
}

/// Task가 데몬이 실제로 섰음을 확인한 뒤에만 mon·mgr·OSD 노드를 더한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CephStatus {
    pub phase: CephPhase,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mons: Vec<CephMon>,
    /// mgr을 돌리는 노드
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mgrs: Vec<Uuid>,
    /// OSD가 하나 이상 있는 노드
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub osd_nodes: Vec<Uuid>,
    /// 설치한 Ceph의 판(예: `19.2.1 squid`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// 부트스트랩하는(한) 노드
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_node: Option<Uuid>,
    /// 부트스트랩이 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 도는(돌) 부트스트랩·mon·mgr Task의 자리(이 Task들은 클러스터 전체에서 한 번에 하나다)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<CephPending>,
}

/// 클러스터 전체에서 한 번에 하나만 도는 Ceph Task(부트스트랩·mon·mgr 더하기)의 자리. API가 Task를 만들기 전에
/// 리비전 확인 쓰기로 잡아 여러 노드의 API가 함께 받은 요청도 하나만 지나가게 하고, Task가 끝나면 지운다. 그
/// Task가 끝났거나 끝내 만들어지지 않았으면 다음 요청이 넘겨받는다. 이것을 모르는 옛 노드는 버린다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CephPending {
    pub op: crate::task::TaskOp,
    /// Task를 돌리는 노드
    pub node: Uuid,
    pub task: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CephPhase {
    /// 첫 mon·mgr을 세우는 중(실패했으면 다시 할 수 있다)
    #[default]
    Bootstrapping,
    /// mon 정족수와 mgr이 있다. mon·mgr·OSD 더하기와 풀 만들기를 할 수 있다.
    Ready,
}

/// mon 하나. 이름은 노드 이름이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CephMon {
    pub node: Uuid,
    pub name: String,
    /// Ceph public 망 안의 주소
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub addr: IpAddr,
}

/// Ceph 비밀 키: base64(`u16 le 1`(AES) + `u32 le secs` + `u32 le nanos` + `u16 le 16` + 비밀 16바이트).
/// `ceph-authtool --gen-print-key`와 같은 모양(28바이트, `AQ`로 시작하는 40자)이다.
pub fn encode_key(secret: &[u8; 16], secs: u32, nanos: u32) -> String {
    let mut bytes = Vec::with_capacity(28);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&secs.to_le_bytes());
    bytes.extend_from_slice(&nanos.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(secret);
    STANDARD.encode(bytes)
}

/// [`encode_key`] 모양의 키인지(28바이트, 종류 AES, 비밀 16바이트)
pub fn valid_key(s: &str) -> bool {
    STANDARD
        .decode(s)
        .is_ok_and(|b| b.len() == 28 && b[0..2] == [1, 0] && b[10..12] == [16, 0])
}

/// 노드 주소(`host:port`·`[v6]:port`)의 호스트가 IP면 그 IP
pub(crate) fn address_ip(address: &str) -> Option<IpAddr> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse().ok()
}

/// 인터페이스 주소(`10.0.0.5/24`)의 IP와 접두사
fn interface_address(s: &str) -> Option<(IpAddr, u8)> {
    let (ip, prefix) = s.split_once('/')?;
    Some((ip.parse().ok()?, prefix.parse().ok()?))
}

/// 노드의 mon 주소: 노드 주소의 IP가 망 `net` 안이면 그것, 아니면 인터페이스(이름 순)의 주소 가운데 망 안의 첫 것.
pub fn mon_addr(node: &Node, net: &Cidr) -> Option<IpAddr> {
    if let Some(ip) = address_ip(&node.spec.address).filter(|ip| net.contains(*ip)) {
        return Some(ip);
    }
    let mut interfaces: Vec<_> = node.status.interfaces.iter().collect();
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
        .iter()
        .flat_map(|i| &i.addresses)
        .filter_map(|a| interface_address(a))
        .map(|(ip, _)| ip)
        .find(|ip| net.contains(*ip))
}

/// `public_network`를 주지 않았을 때의 망: 노드 주소의 호스트가 IP면 그 IP를 가진 인터페이스의 망. 호스트가
/// 이름이거나 그 IP를 가진 인터페이스가 없으면 None.
pub fn default_public_network(node: &Node) -> Option<Cidr> {
    let ip = address_ip(&node.spec.address)?;
    node.status
        .interfaces
        .iter()
        .flat_map(|i| &i.addresses)
        .filter_map(|a| interface_address(a))
        .find(|(a, _)| *a == ip)
        .and_then(|(a, prefix)| Cidr::network(a, prefix))
}

/// 노드의 디스크 하나(`lsblk`의 최상위 디스크)와 OSD로 쓸 수 있는지
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeDisk {
    /// 예: `/dev/sdb`
    pub path: String,
    /// `/dev/disk/by-id/…`(재부팅해도 같은 디스크)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub by_id: Option<String>,
    pub size_bytes: u64,
    pub rotational: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub serial: Option<String>,
    #[serde(flatten)]
    pub state: DiskState,
}

/// 디스크를 OSD로 쓸 수 있는지
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum DiskState {
    /// 비어 있다.
    Available,
    /// 지워야 쓸 수 있다(파티션 표·파일 시스템 서명·다른 VG·다른 Ceph의 흔적).
    Wipeable { reasons: Vec<String> },
    /// 지우기로도 쓸 수 없다(시스템 디스크·마운트·스왑·풀 VG·이 Ceph의 OSD 등).
    InUse { reason: String },
}

/// Ceph에서 읽은 클러스터 상태
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct CephReport {
    pub health: CephHealth,
    pub mons: Vec<MonReport>,
    /// 정족수에 든 mon 이름
    pub quorum: Vec<String>,
    pub mgr: MgrReport,
    pub osds: Vec<OsdReport>,
    /// up OSD가 하나 이상 있는 호스트 수
    pub osd_hosts_up: u32,
    pub pgs: PgReport,
    pub usage: Usage,
    pub pools: Vec<PoolReport>,
    /// 읽은 시각(유닉스 초)
    pub read_at: u64,
    /// 읽은 노드
    pub via: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct CephHealth {
    /// `HEALTH_OK`, `HEALTH_WARN`, `HEALTH_ERR`
    pub status: String,
    pub checks: Vec<CephHealthCheck>,
}

/// health 검사 하나(예: `TOO_FEW_OSDS`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct CephHealthCheck {
    pub code: String,
    /// `HEALTH_WARN`, `HEALTH_ERR`
    pub severity: String,
    pub summary: String,
    pub count: u64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MonReport {
    pub name: String,
    /// v2 주소(`10.0.0.1:3300`)
    pub addr: String,
    pub in_quorum: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct MgrReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub active: Option<String>,
    pub standbys: Vec<String>,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct OsdReport {
    pub id: u32,
    pub host: String,
    pub up: bool,
    #[serde(rename = "in")]
    pub in_: bool,
    /// 장치 클래스(`hdd`, `ssd`, `nvme`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub class: Option<String>,
    /// CRUSH 무게
    pub weight: f64,
    /// 노드의 장치(예: `/dev/sdb`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub device: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct PgReport {
    pub total: u64,
    /// 상태(`active+clean` 등)와 그 PG 수
    pub states: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct Usage {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct PoolReport {
    pub name: String,
    pub id: u64,
    /// 복제 수
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub size: Option<u32>,
    pub stored_bytes: u64,
    pub max_available_bytes: u64,
    /// 0~1
    pub percent_used: f64,
    /// 이 Ceph 풀을 쓰는 Orbivisor 스토리지 풀
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub orbivisor_pool: Option<String>,
}

/// `GET /ceph`의 응답. 키는 담지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CephView {
    pub fsid: Uuid,
    #[cfg_attr(feature = "openapi", schema(value_type = String, example = "10.0.0.0/24"))]
    pub public_network: Cidr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, nullable = false, default, example = "10.1.0.0/24")
    )]
    pub cluster_network: Option<Cidr>,
    pub phase: CephPhase,
    pub mons: Vec<CephMon>,
    pub mgrs: Vec<Uuid>,
    pub osd_nodes: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub init_node: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_error: Option<String>,
    /// 노드가 관리 Ceph에 붙는 클라이언트 설정(`/etc/opl/ceph/ceph.conf`)
    pub managed_conf: String,
    /// 끝나지 않은 Ceph Task
    pub tasks: Vec<Uuid>,
}

impl CephView {
    pub fn new(ceph: &CephCluster, managed_conf: String, tasks: Vec<Uuid>) -> Self {
        let (spec, status) = (&ceph.spec, &ceph.status);
        Self {
            fsid: spec.fsid,
            public_network: spec.public_network,
            cluster_network: spec.cluster_network,
            phase: status.phase,
            mons: status.mons.clone(),
            mgrs: status.mgrs.clone(),
            osd_nodes: status.osd_nodes.clone(),
            release: status.release.clone(),
            init_node: status.init_node,
            last_error: status.last_error.clone(),
            managed_conf,
            tasks,
        }
    }
}

/// `GET /ceph/status`의 응답: 보고서와, 지금 읽지 못해 마지막으로 읽은 보고서를 돌려줄 때의 표시
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CephStatusView {
    #[serde(flatten)]
    pub report: CephReport,
    /// 참이면 `report`는 `read_at`에 읽은 마지막 보고서다(지금은 읽지 못함).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub stale: bool,
    /// 지금 읽지 못한 이유(`stale`일 때)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// `POST /ceph`: 노드 `node`에 첫 mon·mgr을 세운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CephInitRequest {
    /// 이름이나 id
    pub node: String,
    /// mon과 클라이언트의 망(CIDR). 없으면 노드 주소가 든 인터페이스의 망
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub public_network: Option<String>,
    /// OSD 복제 트래픽 망(CIDR)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub cluster_network: Option<String>,
}

/// `POST /ceph/mons`·`/ceph/mgrs`·`/ceph/clients`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CephNodeRequest {
    /// 이름이나 id
    pub node: String,
}

/// `POST /ceph/osds`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CephOsdRequest {
    /// 이름이나 id
    pub node: String,
    /// `/dev/sdb`나 `/dev/disk/by-id/…`
    pub device: String,
    /// 디스크의 파티션·파일 시스템·LVM을 지우고 쓴다.
    #[serde(default)]
    pub wipe: bool,
}

/// `POST /ceph/pools`: Ceph 풀을 만들어 Orbivisor RBD 풀 `name`으로 등록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CephPoolRequest {
    /// Orbivisor 스토리지 풀 이름
    pub name: String,
    /// Ceph 풀 이름. 없으면 `name`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub pool: Option<String>,
    /// 복제 수(1~3). 없으면 min(3, OSD가 있는 호스트 수)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub size: Option<u8>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{InterfaceKind, NodeInterface, NodeSpec, NodeStatus};

    /// Ceph 문서의 키 예
    const DOC_KEY: &str = "AQBHCbtT6APDHhAA5W00cBchwkQjh3dkKsyPjw==";

    fn keys() -> CephKeys {
        CephKeys {
            mon: "AQAAAAAAAAAAABAAAQIDBAUGBwgJCgsMDQ4PEA==".into(),
            admin: DOC_KEY.into(),
            bootstrap_osd: "AQBHCbtT6APDHhAAAAAAAAAAAAAAAAAAAAAAAA==".into(),
            client: "AQBHCbtT6APDHhAA//////////////////////8=".into(),
        }
    }

    fn node(address: &str, interfaces: &[(&str, &[&str])]) -> Node {
        let mut node = Node::new(
            "node1",
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: address.into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.status = NodeStatus {
            interfaces: interfaces
                .iter()
                .map(|(name, addresses)| NodeInterface {
                    name: name.to_string(),
                    kind: InterfaceKind::Ethernet,
                    state: "up".into(),
                    carrier: Some(true),
                    speed_mbps: None,
                    mac: None,
                    master: None,
                    addresses: addresses.iter().map(|a| a.to_string()).collect(),
                    bond: None,
                })
                .collect(),
            ..Default::default()
        };
        node
    }

    fn cidr(s: &str) -> Cidr {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// 알려진 키를 풀어 그 값으로 다시 만들면 같다.
    #[test]
    fn encodes_keys_like_ceph_authtool() {
        let b = STANDARD.decode(DOC_KEY).unwrap();
        assert_eq!(b.len(), 28);
        let secs = u32::from_le_bytes(b[2..6].try_into().unwrap());
        let nanos = u32::from_le_bytes(b[6..10].try_into().unwrap());
        let secret: [u8; 16] = b[12..28].try_into().unwrap();
        assert_eq!(encode_key(&secret, secs, nanos), DOC_KEY);
        assert!(valid_key(DOC_KEY));

        let key = encode_key(&[7; 16], 1_700_000_000, 123_456_789);
        assert!(key.starts_with("AQ"), "{key}");
        assert_eq!(key.len(), 40);
        assert!(valid_key(&key));
    }

    #[test]
    fn rejects_keys_of_another_shape() {
        for bad in [
            "",
            "not base64!",
            // 27바이트
            &STANDARD.encode([1u8; 27]),
            // 종류가 AES(1)가 아님
            &STANDARD.encode([[2u8, 0].as_slice(), &[0; 8], &[16, 0], &[0; 16]].concat()),
            // 비밀 길이가 16이 아님
            &STANDARD.encode([[1u8, 0].as_slice(), &[0; 8], &[32, 0], &[0; 16]].concat()),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn spec_round_trips_and_debug_hides_the_keys() {
        let spec = CephSpec {
            fsid: Uuid::now_v7(),
            public_network: cidr("10.0.0.0/24"),
            cluster_network: None,
            keys: keys(),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("cluster_network").is_none(), "{json}");
        assert_eq!(json["public_network"], "10.0.0.0/24");
        assert_eq!(json["keys"]["bootstrap_osd"], keys().bootstrap_osd);
        assert_eq!(serde_json::from_value::<CephSpec>(json).unwrap(), spec);

        let spec = CephSpec {
            cluster_network: Some(cidr("10.1.0.0/24")),
            ..spec
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["cluster_network"], "10.1.0.0/24");
        assert_eq!(serde_json::from_value::<CephSpec>(json).unwrap(), spec);

        let debug = format!("{spec:?}");
        let k = keys();
        for key in [&k.mon, &k.admin, &k.bootstrap_osd, &k.client] {
            assert!(!debug.contains(key.as_str()), "{debug}");
        }
        assert!(debug.contains("***"), "{debug}");
        assert_eq!(CephSpec::KIND, Kind::Ceph);
        assert_eq!(Kind::Ceph.as_str(), "ceph");
        assert_eq!(serde_json::to_value(Kind::Ceph).unwrap(), "ceph");
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let mut json = serde_json::to_value(CephSpec {
            fsid: Uuid::nil(),
            public_network: cidr("10.0.0.0/24"),
            cluster_network: None,
            keys: keys(),
        })
        .unwrap();
        json["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<CephSpec>(json).is_err());
    }

    #[test]
    fn empty_status_is_only_the_phase_and_round_trips() {
        let json = serde_json::to_value(CephStatus::default()).unwrap();
        assert_eq!(json, serde_json::json!({ "phase": "bootstrapping" }));
        assert_eq!(
            serde_json::from_value::<CephStatus>(serde_json::json!({})).unwrap(),
            CephStatus::default()
        );

        let n = Uuid::now_v7();
        let status = CephStatus {
            phase: CephPhase::Ready,
            mons: vec![CephMon {
                node: n,
                name: "node1".into(),
                addr: ip("10.0.0.1"),
            }],
            mgrs: vec![n],
            osd_nodes: vec![n],
            release: Some("19.2.1 squid".into()),
            init_node: Some(n),
            last_error: Some("x".into()),
            pending: Some(CephPending {
                op: crate::task::TaskOp::CephMonAdd,
                node: n,
                task: n,
            }),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["phase"], "ready");
        assert_eq!(
            json["mons"][0],
            serde_json::json!({ "node": n, "name": "node1", "addr": "10.0.0.1" })
        );
        assert_eq!(
            json["pending"],
            serde_json::json!({ "op": { "kind": "ceph-mon-add" }, "node": n, "task": n })
        );
        assert_eq!(serde_json::from_value::<CephStatus>(json).unwrap(), status);
    }

    #[test]
    fn mon_address_is_the_node_address_when_it_is_in_the_network() {
        let n = node("10.0.0.5:8006", &[("eth0", &["10.0.0.5/24"])]);
        assert_eq!(mon_addr(&n, &cidr("10.0.0.0/24")), Some(ip("10.0.0.5")));
        let n = node("[fd00::5]:8006", &[]);
        assert_eq!(mon_addr(&n, &cidr("fd00::/64")), Some(ip("fd00::5")));
    }

    /// 노드 주소가 망 밖이면 인터페이스 이름 순으로 망 안의 첫 주소다.
    #[test]
    fn mon_address_falls_back_to_interfaces_in_name_order() {
        let n = node(
            "10.0.0.5:8006",
            &[
                ("eth2", &["10.9.0.2/24"]),
                ("eth1", &["192.168.1.1/24", "10.9.0.1/24", "fd09::1/64"]),
                ("eth0", &["10.0.0.5/24"]),
            ],
        );
        assert_eq!(mon_addr(&n, &cidr("10.9.0.0/24")), Some(ip("10.9.0.1")));
        assert_eq!(mon_addr(&n, &cidr("fd09::/64")), Some(ip("fd09::1")));
        let named = node("node1.example:8006", &[("eth0", &["10.9.0.7/24"])]);
        assert_eq!(mon_addr(&named, &cidr("10.9.0.0/24")), Some(ip("10.9.0.7")));
    }

    #[test]
    fn mon_address_is_none_without_an_address_in_the_network() {
        let n = node("10.0.0.5:8006", &[("eth0", &["10.0.0.5/24", "bad"])]);
        assert_eq!(mon_addr(&n, &cidr("10.9.0.0/24")), None);
        assert_eq!(mon_addr(&n, &cidr("fd00::/64")), None);
    }

    #[test]
    fn default_public_network_is_the_network_of_the_node_address() {
        let n = node(
            "10.0.0.5:8006",
            &[("eth1", &["10.9.0.1/24"]), ("eth0", &["10.0.0.5/22"])],
        );
        assert_eq!(default_public_network(&n), Some(cidr("10.0.0.0/22")));
        let v6 = node("[fd00::1:5]:8006", &[("eth0", &["fd00::1:5/64"])]);
        assert_eq!(default_public_network(&v6), Some(cidr("fd00::/64")));
    }

    /// 호스트가 이름이거나 그 IP를 가진 인터페이스가 없으면(루프백 등) 정할 수 없다.
    #[test]
    fn default_public_network_is_none_for_names_and_unknown_addresses() {
        let named = node("node1.example:8006", &[("eth0", &["10.0.0.5/24"])]);
        assert_eq!(default_public_network(&named), None);
        let lo = node("127.0.0.1:8006", &[("eth0", &["10.0.0.5/24"])]);
        assert_eq!(default_public_network(&lo), None);
    }

    #[test]
    fn disk_state_is_tagged_by_state_and_flattened_into_the_disk() {
        assert_eq!(
            serde_json::to_value(DiskState::Wipeable {
                reasons: vec!["gpt partition table".into(), "ext4 filesystem".into()],
            })
            .unwrap(),
            serde_json::json!({ "state": "wipeable",
                                "reasons": ["gpt partition table", "ext4 filesystem"] })
        );
        assert_eq!(
            serde_json::to_value(DiskState::InUse {
                reason: "mounted at /srv".into()
            })
            .unwrap(),
            serde_json::json!({ "state": "in-use", "reason": "mounted at /srv" })
        );
        let disk = NodeDisk {
            path: "/dev/sdb".into(),
            by_id: None,
            size_bytes: 10 << 30,
            rotational: true,
            model: None,
            serial: Some("S1".into()),
            state: DiskState::Available,
        };
        let json = serde_json::to_value(&disk).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "path": "/dev/sdb", "size_bytes": 10u64 << 30, "rotational": true,
                                "serial": "S1", "state": "available" })
        );
        assert_eq!(serde_json::from_value::<NodeDisk>(json).unwrap(), disk);
    }

    /// 뒤 버전이 더한 필드·빠진 필드가 있어도 보고서를 읽는다. OSD의 `in`은 JSON 이름 그대로다.
    #[test]
    fn report_reads_with_defaults_and_names_in() {
        let report: CephReport = serde_json::from_value(serde_json::json!({
            "health": { "status": "HEALTH_OK", "new_field": 1 },
            "osds": [{ "id": 3, "host": "node2", "up": true, "in": true, "weight": 0.5 }],
            "pgs": { "total": 33, "states": [["active+clean", 33]] }
        }))
        .unwrap();
        assert_eq!(report.health.status, "HEALTH_OK");
        assert!(report.osds[0].in_ && report.osds[0].up);
        assert_eq!(report.pgs.states, [("active+clean".to_string(), 33)]);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["osds"][0]["in"], true);
        assert!(json["osds"][0].get("in_").is_none());
        assert_eq!(
            json["pgs"]["states"][0],
            serde_json::json!(["active+clean", 33])
        );
        assert_eq!(serde_json::from_value::<CephReport>(json).unwrap(), report);
        let empty: CephReport = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(empty, CephReport::default());
    }

    #[test]
    fn view_has_no_keys() {
        let n = Uuid::now_v7();
        let mut ceph = CephCluster::new(
            CEPH_NAME,
            CephSpec {
                fsid: Uuid::now_v7(),
                public_network: cidr("10.0.0.0/24"),
                cluster_network: None,
                keys: keys(),
            },
        );
        ceph.status.init_node = Some(n);
        let task = Uuid::now_v7();
        let view = CephView::new(&ceph, "/etc/opl/ceph/ceph.conf".into(), vec![task]);
        let text = serde_json::to_string(&view).unwrap();
        let k = keys();
        for key in [&k.mon, &k.admin, &k.bootstrap_osd, &k.client] {
            assert!(!text.contains(key.as_str()), "{text}");
        }
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["phase"], "bootstrapping");
        assert_eq!(json["init_node"], serde_json::json!(n));
        assert_eq!(json["tasks"], serde_json::json!([task]));
        assert_eq!(json["mons"], serde_json::json!([]));
        assert!(json.get("cluster_network").is_none());
    }

    #[test]
    fn requests_default_and_reject_unknown_fields() {
        let osd: CephOsdRequest =
            serde_json::from_value(serde_json::json!({ "node": "node2", "device": "/dev/sdb" }))
                .unwrap();
        assert!(!osd.wipe);
        let pool: CephPoolRequest =
            serde_json::from_value(serde_json::json!({ "name": "vmpool" })).unwrap();
        assert_eq!((pool.pool, pool.size), (None, None));
        assert!(
            serde_json::from_value::<CephNodeRequest>(serde_json::json!({ "node": "a", "x": 1 }))
                .is_err()
        );
        let init: CephInitRequest =
            serde_json::from_value(serde_json::json!({ "node": "node1" })).unwrap();
        assert_eq!(init.public_network, None);
    }

    /// 상태 응답은 보고서 필드를 그대로 펴고, 지금 읽은 것이면 `stale`·`error`를 넣지 않는다.
    #[test]
    fn status_view_flattens_the_report() {
        let report = CephReport {
            quorum: vec!["node1".into()],
            osd_hosts_up: 1,
            ..CephReport::default()
        };
        let fresh = CephStatusView {
            report: report.clone(),
            stale: false,
            error: None,
        };
        let v = serde_json::to_value(&fresh).unwrap();
        assert_eq!(v["quorum"], serde_json::json!(["node1"]));
        assert_eq!(v["osd_hosts_up"], 1);
        assert!(v.get("stale").is_none() && v.get("error").is_none(), "{v}");
        let stale = CephStatusView {
            report,
            stale: true,
            error: Some("timed out".into()),
        };
        let v = serde_json::to_value(&stale).unwrap();
        assert_eq!(
            (v["stale"].clone(), v["error"].clone()),
            (true.into(), "timed out".into())
        );
        let back: CephStatusView = serde_json::from_value(v).unwrap();
        assert_eq!(back, stale);
    }
}
