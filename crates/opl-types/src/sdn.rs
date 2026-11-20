//! SDN(OVN) 설치·구성(계획 65): Orbivisor 클러스터 하나에 Orbivisor가 노드에 설치·구성하는 OVN 배포 하나.
//! `Sdn` 리소스(이름 `sdn` 하나뿐)가 중앙 노드(NB·SB ovsdb 클러스터와 northd)와 리더 `OvnSync`의 동기화 기록을
//! 담는다. 노드마다의 chassis 상태는 [`NodeSdn`](`NodeStatus.sdn`)이다. 원하는 논리 스위치·포트는 네트워크·VM
//! 리소스에 있고 NB는 그 파생물이다.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::Node;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::MacAddr;

pub type Sdn = Resource<SdnSpec, SdnStatus>;

/// `Sdn` 리소스의 이름(클러스터에 하나뿐)
pub const SDN_NAME: &str = "sdn";
/// 중앙 노드 수의 상한
pub const MAX_CENTRALS: usize = 5;
/// NB ovsdb의 TLS 포트
pub const NB_PORT: u16 = 6641;
/// SB ovsdb의 TLS 포트
pub const SB_PORT: u16 = 6642;
/// NB ovsdb 클러스터의 Raft 포트
pub const NB_RAFT_PORT: u16 = 6643;
/// SB ovsdb 클러스터의 Raft 포트
pub const SB_RAFT_PORT: u16 = 6644;
/// Geneve UDP 포트(IANA)
pub const GENEVE_PORT: u16 = 6081;
/// SDN 네트워크의 기본 MTU: underlay 1500에서 OVN Geneve 머리(`max_header_size` 38 + 바깥 IPv4 20)를 뺀 값
pub const DEFAULT_SDN_MTU: u16 = 1442;
/// 구성할 수 있는 OVN의 가장 낮은 판(24.03 LTS)
pub const MIN_OVN: (u32, u32) = (24, 3);

/// VM `vm`의 NIC(MAC `mac`) 논리 포트 이름 `opl-<VM id>-<MAC 12자리 16진>`. MAC은 클러스터에서 하나라 겹치지 않고 NIC
/// 순서가 바뀌어도 같은 포트다. 노드의 OVS 포트 `external_ids:iface-id`가 이 이름이다(reconciler가 실행 정의에 넣는다).
pub fn port_name(vm: Uuid, mac: MacAddr) -> String {
    let hex: String = mac.0.iter().map(|b| format!("{b:02x}")).collect();
    format!("opl-{vm}-{hex}")
}

/// 라우터 하나의 게이트웨이 chassis 수 상한(계획 66 §9.3, `HA_Chassis` 행)
pub const MAX_GATEWAYS: usize = 5;
/// 가장 높은 `HA_Chassis.priority`(OVN 0~32767)
pub const TOP_GATEWAY_PRIORITY: u16 = 32767;
/// 노드 하나의 physnet 수 상한
pub const MAX_PHYSNETS: usize = 8;
/// 노드의 게이트웨이 우선순위 범위(관리자가 고름)
pub const GATEWAY_PRIORITIES: std::ops::RangeInclusive<u16> = 1..=1000;

/// SHA-256
fn sha256(parts: &[&[u8]]) -> ring::digest::Digest {
    let mut c = ring::digest::Context::new(&ring::digest::SHA256);
    for p in parts {
        c.update(p);
    }
    c.finish()
}

/// physnet 이름 규칙 `[a-z][a-z0-9-]{0,30}`(OVN `ovn-bridge-mappings`의 `:`·`,`가 들지 않는다)
pub fn valid_physnet(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=31).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// physnet 연결 장치 이름(계획 66 §7): `(OVS 쪽 veth, Linux 브리지 쪽 veth, OVS 브리지)` =
/// `oplp<h8>o`·`oplp<h8>b`·`oplp<h8>x`(h8 = SHA-256(`<태그>/<physnet>`) 앞 8자 16진, 13자). 같은 클러스터·physnet은 노드마다
/// 같은 이름이다. VXLAN 장치 `oplx<vni>`와 겹치지 않게 접두사를 `oplp`로 같이 쓴다.
pub fn physnet_ifnames(tag: &str, physnet: &str) -> (String, String, String) {
    let d = sha256(&[tag.as_bytes(), b"/", physnet.as_bytes()]);
    let h8: String = d.as_ref()[..4].iter().map(|b| format!("{b:02x}")).collect();
    (
        format!("oplp{h8}o"),
        format!("oplp{h8}b"),
        format!("oplp{h8}x"),
    )
}

/// 네트워크 옮기기(계획 67 §9.5) 다리의 임시 physnet 이름 `mig<h8>`(h8 = SHA-256(`<태그>/migration/<옮기기 id>`) 앞
/// 8자 16진). [`valid_physnet`]에 맞고, 관리자 physnet과 겹치지 않게 이 모양은 노드 설정이 받지 않는다
/// ([`NodeSdnConfig::validate`]). 다리 장치 이름은 이 physnet의 [`physnet_ifnames`]다.
pub fn migration_physnet(tag: &str, migration: Uuid) -> String {
    let d = sha256(&[
        tag.as_bytes(),
        b"/migration/",
        migration.to_string().as_bytes(),
    ]);
    let h8: String = d.as_ref()[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("mig{h8}")
}

/// [`migration_physnet`]의 이름 모양(`^mig[0-9a-f]{8}$`)인지
pub fn is_migration_physnet(name: &str) -> bool {
    name.strip_prefix("mig").is_some_and(|rest| {
        rest.len() == 8
            && rest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// OVN 접근 잠금의 주소 허용 집합(계획 67 §10.3, 이름 순·겹치지 않게)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowSets {
    /// 멤버 집합(NB 6641·SB 역할 6642): 위트니스·macOS가 아닌 노드의 클러스터 주소와 underlay 주소
    pub members: Vec<IpAddr>,
    /// 중앙 집합(Raft 6643·6644·SB 특권 6645): 중앙 주소와 그 노드들의 underlay
    pub centrals: Vec<IpAddr>,
    /// Geneve(UDP 6081)를 받는 출발 주소: 멤버 노드의 `encap_ip`
    pub encaps: Vec<IpAddr>,
}

/// 노드 목록과 SDN 리소스에서 잠금의 허용 집합을 셈한다. 지운 노드는 목록에 없으므로 빠진다. macOS 노드(`qemu-hvf`)는
/// chassis가 될 수 없어 멤버가 아니다.
pub fn allow_sets(nodes: &[Node], sdn: &Sdn) -> AllowSets {
    let mut sets = AllowSets::default();
    for n in nodes {
        if n.spec.witness || n.status.hypervisor == "qemu-hvf" {
            continue;
        }
        sets.members
            .extend(crate::ceph::address_ip(&n.spec.address));
        sets.members.extend(n.status.underlay);
        sets.encaps
            .extend(n.status.sdn.as_ref().and_then(|s| s.encap_ip));
    }
    for c in &sdn.status.centrals {
        sets.centrals.push(c.address);
        if let Some(n) = nodes.iter().find(|n| n.id == c.node) {
            sets.centrals.extend(n.status.underlay);
        }
    }
    for set in [&mut sets.members, &mut sets.centrals, &mut sets.encaps] {
        for ip in set.iter_mut() {
            *ip = ip.to_canonical();
        }
        set.sort();
        set.dedup();
    }
    sets
}

/// [`physnet_ifnames`]의 이름 모양(`^oplp[0-9a-f]{8}[obx]$`)인지
pub fn is_physnet_ifname(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("oplp") else {
        return false;
    };
    let b = rest.as_bytes();
    b.len() == 9
        && b[..8]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        && matches!(b[8], b'o' | b'b' | b'x')
}

/// 라우터 `router`가 네트워크 `network`에 둔 포트의 MAC: `02:` + SHA-256(`router` ‖ 라우터 id ‖ 네트워크 id) 앞 5바이트.
/// [`crate::ipam::server_mac`](네트워크 id만)과 입력 모양이 달라 겹치지 않는다. 리더가 바뀌어도 같다.
pub fn router_mac(router: Uuid, network: Uuid) -> MacAddr {
    let d = sha256(&[b"router", router.as_bytes(), network.as_bytes()]);
    let d = d.as_ref();
    MacAddr([0x02, d[0], d[1], d[2], d[3], d[4]])
}

/// 라우터 `router`의 게이트웨이 chassis 순위(계획 66 §9.3): 후보 `(노드, 관리자 우선순위)`를 관리자 우선순위 내림, 같으면
/// HRW 해시 SHA-256(라우터 id ‖ 노드 id) 내림으로 세워 앞 [`MAX_GATEWAYS`]개에 `HA_Chassis.priority` 32767, 32766, …을
/// 준다. 같은 우선순위의 노드에 라우터들이 고르게 흩어지고, 노드를 더하거나 빼도 그 노드가 이기거나 지던 라우터만
/// 주인이 바뀐다.
pub fn gateway_ranking(router: Uuid, candidates: &[(Uuid, u16)]) -> Vec<(Uuid, u16)> {
    let mut ranked: Vec<(u16, [u8; 32], Uuid)> = candidates
        .iter()
        .map(|(node, priority)| {
            let mut h = [0u8; 32];
            h.copy_from_slice(sha256(&[router.as_bytes(), node.as_bytes()]).as_ref());
            (*priority, h, *node)
        })
        .collect();
    ranked.sort_by(|a, b| b.cmp(a));
    ranked
        .into_iter()
        .take(MAX_GATEWAYS)
        .zip((0..).map(|i: u16| TOP_GATEWAY_PRIORITY - i))
        .map(|((_, _, node), priority)| (node, priority))
        .collect()
}

/// 노드의 SDN 설정(`NodeSpec.sdn`, 계획 66 §7). 관리자가 노드 쓰기(`cluster.admin`)로 바꾼다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeSdnConfig {
    /// physnet 이름 → 이 노드의 Linux 브리지 이름(관리자가 만든 것, 8개까지). SDN 에이전트가 veth로 OVS 브리지를 잇는다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub physnets: BTreeMap<String, String>,
    /// 게이트웨이 노드 후보의 우선순위(1~1000, 클수록 먼저). 없으면 게이트웨이가 아니다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub gateway: Option<u16>,
}

impl NodeSdnConfig {
    pub fn validate(&self) -> Result<(), NodeSdnError> {
        if self.physnets.len() > MAX_PHYSNETS {
            return Err(NodeSdnError::TooManyPhysnets);
        }
        for (physnet, bridge) in &self.physnets {
            if !valid_physnet(physnet) {
                return Err(NodeSdnError::BadPhysnet(physnet.clone()));
            }
            if is_migration_physnet(physnet) {
                return Err(NodeSdnError::ReservedPhysnet(physnet.clone()));
            }
            if !crate::network::valid_ifname(bridge)
                || bridge == crate::network::OVN_BRIDGE
                || crate::network::is_managed_ifname(bridge)
            {
                return Err(NodeSdnError::BadBridge(physnet.clone(), bridge.clone()));
            }
        }
        if let Some(p) = self.gateway
            && !GATEWAY_PRIORITIES.contains(&p)
        {
            return Err(NodeSdnError::BadGateway(p));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NodeSdnError {
    #[error("a node can have at most {MAX_PHYSNETS} physnets")]
    TooManyPhysnets,
    #[error(
        "invalid physnet {0:?}: use 1-31 lowercase letters, digits and '-', starting with a letter"
    )]
    BadPhysnet(String),
    #[error(
        "physnet {0:?}: bridge {1:?} must be an existing Linux bridge name (1-15 characters), not br-int or an Orbivisor device"
    )]
    BadBridge(String, String),
    #[error("invalid gateway priority {0}: use 1-1000")]
    BadGateway(u16),
    #[error(
        "physnet {0:?} is reserved for network migration bridges (mig followed by 8 hex digits)"
    )]
    ReservedPhysnet(String),
}

/// 노드의 physnet 연결 상태(`NodeSdn.physnets`, 계획 66 §7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PhysnetState {
    pub name: String,
    /// 이 노드의 Linux 브리지
    pub bridge: String,
    /// veth·OVS 브리지·`ovn-bridge-mappings`가 다 있다.
    #[serde(default)]
    pub ready: bool,
    /// 준비되지 않은 까닭(브리지 없음, 매핑 충돌, 명령 실패)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 클러스터 SDN의 원하는 상태(게이트웨이 노드는 `NodeSpec.sdn`에 둔다 — 계획 66).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdnSpec {
    /// OVN 접근 잠금(계획 67 §10): SB RBAC 역할 포트·northd 전용 포트·주소 허용 목록. 거짓이면 쓰지 않아 옛 노드도 푼다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lockdown: bool,
}

impl ResourceSpec for SdnSpec {
    const KIND: Kind = Kind::Sdn;
    type Status = SdnStatus;
}

/// Task가 중앙을 실제로 세웠음을 확인한 뒤에만 중앙을 더한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SdnStatus {
    pub phase: SdnPhase,
    /// NB·SB ovsdb 클러스터와 northd를 돌리는 노드
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub centrals: Vec<SdnCentral>,
    /// 설치한 OVN의 판(예: `24.03.4`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// 처음 세우는(세운) 노드
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_node: Option<Uuid>,
    /// 처음 세우기의 중앙 주소(API가 정한 것). 주소 없이 다시 하면 이 주소로 이어서 한다(리뷰 1 I-7 — 이미 만든 DB의
    /// Raft 주소와 단위가 어긋나지 않게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_address: Option<IpAddr>,
    /// 처음 세우기가 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 도는(돌) 중앙 Task(`sdn-init`·`sdn-central-add`·`sdn-central-remove`)의 자리. 클러스터 전체에서 한 번에 하나다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<SdnPending>,
    /// 리더 `OvnSync`의 마지막 NB 동기화. 바뀔 때만 쓴다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SdnSync>,
    /// OVN NB Raft 클러스터 id(`sdn-init`이 DB를 만들며 적는다). 노드에 남은 DB가 이 클러스터 것인지 본다(§7.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nb_cid: Option<Uuid>,
    /// OVN SB Raft 클러스터 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sb_cid: Option<Uuid>,
    /// 클러스터에서 지웠지만 OVN Raft에 멤버로 남았을 수 있는 노드(중앙 목록 밖의 DB를 가진 채 꺼진 노드 — 리뷰 2 N-2).
    /// 살아 있는 중앙의 `sdn-central-kick` Task가 내쫓은 뒤 지운다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub departed: Vec<SdnDeparted>,
    /// OVN 접근 잠금을 맞춘 기록(계획 67, `sdn-lockdown` Task가 쓴다). 잠금을 켠 적이 없으면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lockdown: Option<LockdownState>,
}

/// 중앙의 OVN 접근 잠금 상태(`SdnStatus.lockdown`, 계획 67 §10.4)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct LockdownState {
    /// SB 설정 파일·단위를 `SdnSpec.lockdown`대로 바꾼 중앙
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub applied: Vec<Uuid>,
    /// 중앙을 바꾸는 중이다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 클러스터에서 지운 노드 가운데 OVN Raft에서 아직 내쫓지 않은 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnDeparted {
    pub node: Uuid,
    /// 지울 때의 노드 이름(`sdn central remove NAME`으로 부른다)
    pub name: String,
    /// 그 노드의 Raft 주소(NB·SB가 이 IP의 6643·6644)
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub address: IpAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SdnPhase {
    /// 첫 중앙을 세우는 중(실패했으면 다시 할 수 있다)
    #[default]
    Bootstrapping,
    /// NB·SB에 리더가 있다. 중앙·chassis 더하기와 SDN 네트워크 만들기를 할 수 있다.
    Ready,
}

/// 중앙 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnCentral {
    pub node: Uuid,
    /// NB·SB·Raft 포트가 듣는 주소(노드 클러스터 주소 하나)
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub address: IpAddr,
}

/// 클러스터 전체에서 한 번에 하나만 도는 중앙 Task의 자리. API가 Task를 만들기 전에 리비전 확인 쓰기로 잡고, Task가
/// 끝나면 지운다(`CephPending`과 같다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdnPending {
    pub op: crate::task::TaskOp,
    /// Task를 돌리는 노드
    pub node: Uuid,
    pub task: Uuid,
}

/// 리더 `OvnSync`가 NB에 쓴 마지막 기록
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SdnSync {
    /// 쓴 리더의 Raft 임기(펜싱 토큰의 앞)
    pub term: u64,
    /// 읽은 Raft 로그 인덱스(펜싱 토큰의 뒤)
    pub index: u64,
    /// 마지막으로 맞춘 시각(유닉스 초). 성공이 이어지면 60초 넘게 낡을 때만 바꾼다.
    pub at: u64,
    /// 마지막 차례가 실패한 이유
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// NB `NB_Global.nb_cfg`(쓸 때마다 1 올림)
    pub nb_cfg: i64,
    /// NB `NB_Global.hv_cfg`: 모든 chassis가 반영한 `nb_cfg`
    pub hv_cfg: i64,
    /// 마지막 차례에서 실패한 묶음(`network tenant-a`, `port-group web,ssh` 따위, 계획 67). 비면 쓰지 않는다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub failed: Vec<String>,
}

/// 노드의 SDN 상태(`NodeStatus.sdn`). 노드의 SDN 에이전트가 바뀔 때만, 모든 노드가 `sdn`을 알릴 때만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeSdn {
    pub chassis: ChassisState,
    /// `ovn-controller --version`의 판
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ovn_version: Option<String>,
    /// `ovs-vsctl --version`의 판
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ovs_version: Option<String>,
    /// Geneve 터널의 이 노드 주소(`ovn-encap-ip`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub encap_ip: Option<IpAddr>,
    /// 이 노드가 중앙이면 NB·SB Raft 역할과 northd
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub central: Option<CentralHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// physnet 연결(계획 66, `NodeSpec.sdn.physnets`마다 이름 순). 비면 쓰지 않는다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub physnets: Vec<PhysnetState>,
    /// `ovn-cms-options`에 `enable-chassis-as-gw`를 적었다(계획 66). 거짓이면 쓰지 않는다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub gateway: bool,
    /// 이 노드의 OVN 주소 허용 목록(nftables `inet opl_ovn_<태그>`, 계획 67 §10.3). 잠금이 꺼져 있으면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub lockdown: Option<LockdownNodeState>,
}

/// 노드의 OVN 주소 허용 목록 상태(`NodeSdn.lockdown`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct LockdownNodeState {
    /// 표가 원하는 집합과 맞다.
    pub firewall: bool,
    /// 허용 집합의 주소 수
    pub allowed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

impl NodeSdn {
    /// physnet `name`이 이 노드에서 준비됐다.
    pub fn physnet_ready(&self, name: &str) -> bool {
        self.physnets.iter().any(|p| p.name == name && p.ready)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ChassisState {
    /// chassis로 구성하지 않았다.
    #[default]
    Missing,
    /// 구성했지만 SB에 붙지 않았거나 SB에 이 노드의 `Chassis` 행이 아직 없다.
    Configuring,
    /// SB에 붙었고 `Chassis` 행이 있다. SDN NIC가 있는 VM을 돌릴 수 있다.
    Ready,
    /// ovn-controller가 돌지 않는다.
    Down,
    /// 이 빌드가 모르는 값(뒤 버전이 더한 상태)
    #[serde(other)]
    Unknown,
}

/// 중앙 노드의 ovsdb Raft 역할과 northd
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CentralHealth {
    pub nb: RaftRole,
    pub sb: RaftRole,
    /// 이 노드의 northd가 SB 잠금을 가져 일하고 있다(나머지는 대기).
    #[serde(default)]
    pub northd_active: bool,
}

/// ovsdb 클러스터에서 이 서버의 역할(`cluster/status`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RaftRole {
    Leader,
    Follower,
    Candidate,
    /// 클러스터에 붙는 중(`join-cluster` 뒤 따라잡기 전)
    Joining,
    /// 서버가 돌지 않거나 답하지 않는다.
    Down,
    /// 이 빌드가 모르는 값
    #[serde(other)]
    Unknown,
}

/// `POST /sdn`: 노드 `node`에 첫 중앙을 세운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnInitRequest {
    /// 이름이나 id
    pub node: String,
    /// NB·SB가 들을 주소. 없으면 노드 주소의 IP
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = String, format = "ip", nullable = false, default)
    )]
    pub address: Option<IpAddr>,
}

/// `POST /sdn/centrals`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnNodeRequest {
    /// 이름이나 id
    pub node: String,
}

/// `POST /sdn/chassis`: `nodes`의 노드나(`all`이면) chassis가 아닌 모든 Linux 노드에 chassis를 설치한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SdnChassisRequest {
    /// 이름이나 id
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: Vec<String>,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub all: bool,
}

impl SdnChassisRequest {
    /// `nodes`와 `all` 가운데 정확히 하나
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.nodes.is_empty() == self.all {
            Ok(())
        } else {
            Err("give either nodes or all")
        }
    }
}

/// `GET /sdn`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnView {
    /// 없으면 SDN을 아직 설정하지 않았다(`sdn init` 전).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub phase: Option<SdnPhase>,
    pub centrals: Vec<SdnCentralView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub release: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub init_node: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sync: Option<SdnSync>,
    /// 위트니스가 아닌 노드마다(이름 순)의 chassis 상태
    pub chassis: Vec<SdnChassisView>,
    /// 끝나지 않은 SDN Task
    pub tasks: Vec<Uuid>,
    /// 중앙 목록에 없지만 OVN Raft에 남았을 수 있는 노드(이 SDN의 DB를 가진 노드와 클러스터에서 지운 노드 — 리뷰 2
    /// N-2·웹 I-3). `DELETE /sdn/centrals/{node}`로 뺀다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub strays: Vec<SdnStrayView>,
}

/// 중앙 목록 밖의 OVN Raft 멤버(일 수 있는 노드)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnStrayView {
    pub node: Uuid,
    pub name: String,
    /// 클러스터에서 이미 지운 노드다(살아 있는 중앙이 Raft에서 내쫓는다).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub gone: bool,
}

/// 중앙 하나와 그 노드가 알린 Raft 역할
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnCentralView {
    pub node: Uuid,
    /// 노드 이름(노드가 없어졌으면 id)
    pub name: String,
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub address: IpAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub health: Option<CentralHealth>,
}

/// 노드 하나의 chassis 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SdnChassisView {
    pub node: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sdn: Option<NodeSdn>,
}

/// 노드 `node`의 OVN 중앙 주소(NB·SB가 듣고 Raft가 쓰는 주소): 노드 주소(`NodeSpec.address`)의 호스트가 IP면 그것 —
/// 노드 인증서의 SAN이다 — 아니면 underlay(`NodeStatus.underlay`). 둘 다 없으면 None
pub fn central_address(node: &Node) -> Option<IpAddr> {
    crate::ceph::address_ip(&node.spec.address).or(node.status.underlay)
}

impl SdnView {
    /// `sdn`이 없으면 설정 전 모양(중앙 없음)이다. 노드 표는 설정 전에도 채운다.
    pub fn new(sdn: Option<&Sdn>, nodes: &[Node], tasks: Vec<Uuid>) -> Self {
        let node = |id: Uuid| nodes.iter().find(|n| n.id == id);
        let centrals = sdn
            .map(|s| s.status.centrals.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|c| SdnCentralView {
                node: c.node,
                name: node(c.node).map_or_else(|| c.node.to_string(), |n| n.name.clone()),
                address: c.address,
                health: node(c.node)
                    .and_then(|n| n.status.sdn.as_ref())
                    .and_then(|s| s.central),
            })
            .collect();
        let mut chassis: Vec<SdnChassisView> = nodes
            .iter()
            .filter(|n| !n.spec.witness)
            .map(|n| SdnChassisView {
                node: n.id,
                name: n.name.clone(),
                sdn: n.status.sdn.clone(),
            })
            .collect();
        chassis.sort_by(|a, b| a.name.cmp(&b.name));
        let status = sdn.map(|s| &s.status);
        let mut strays: Vec<SdnStrayView> = Vec::new();
        if let Some(s) = status.filter(|s| s.phase == SdnPhase::Ready) {
            let busy = s.pending.as_ref().map(|p| p.node);
            strays.extend(
                nodes
                    .iter()
                    .filter(|n| !s.centrals.iter().any(|c| c.node == n.id) && busy != Some(n.id))
                    .filter(|n| n.status.sdn.as_ref().is_some_and(|x| x.central.is_some()))
                    .map(|n| SdnStrayView {
                        node: n.id,
                        name: n.name.clone(),
                        gone: false,
                    }),
            );
            strays.extend(s.departed.iter().map(|d| SdnStrayView {
                node: d.node,
                name: d.name.clone(),
                gone: true,
            }));
            strays.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Self {
            phase: status.map(|s| s.phase),
            centrals,
            release: status.and_then(|s| s.release.clone()),
            init_node: status.and_then(|s| s.init_node),
            last_error: status.and_then(|s| s.last_error.clone()),
            sync: status.and_then(|s| s.sync.clone()),
            chassis,
            tasks,
            strays,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeSpec, NodeStatus};
    use serde_json::json;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn kind_and_empty_shapes() {
        assert_eq!(SdnSpec::KIND, Kind::Sdn);
        assert_eq!(Kind::Sdn.as_str(), "sdn");
        assert_eq!(serde_json::to_value(Kind::Sdn).unwrap(), "sdn");
        assert_eq!(serde_json::to_value(SdnSpec::default()).unwrap(), json!({}));
        assert!(serde_json::from_value::<SdnSpec>(json!({ "gateways": [] })).is_err());
        let json = serde_json::to_value(SdnStatus::default()).unwrap();
        assert_eq!(json, json!({ "phase": "bootstrapping" }));
        assert_eq!(
            serde_json::from_value::<SdnStatus>(json!({})).unwrap(),
            SdnStatus::default()
        );
    }

    #[test]
    fn status_round_trips() {
        let n = Uuid::now_v7();
        let status = SdnStatus {
            phase: SdnPhase::Ready,
            centrals: vec![SdnCentral {
                node: n,
                address: ip("10.0.0.1"),
            }],
            release: Some("24.03.4".into()),
            init_node: Some(n),
            init_address: Some(ip("10.0.0.1")),
            last_error: None,
            pending: Some(SdnPending {
                op: crate::task::TaskOp::SdnCentralAdd,
                node: n,
                task: Uuid::now_v7(),
            }),
            sync: Some(SdnSync {
                term: 3,
                index: 120,
                at: 1_700_000_000,
                error: None,
                nb_cfg: 7,
                hv_cfg: 6,
                failed: Vec::new(),
            }),
            nb_cid: Some(Uuid::now_v7()),
            sb_cid: None,
            departed: vec![SdnDeparted {
                node: n,
                name: "n9".into(),
                address: ip("10.0.0.9"),
            }],
            lockdown: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["departed"][0]["name"], "n9");
        assert!(
            serde_json::to_value(SdnStatus::default())
                .unwrap()
                .get("departed")
                .is_none()
        );
        assert_eq!(json["centrals"][0]["address"], "10.0.0.1");
        assert!(json.get("sb_cid").is_none(), "{json}");
        assert_eq!(json["init_address"], "10.0.0.1");
        assert_eq!(json["pending"]["op"], json!({ "kind": "sdn-central-add" }));
        assert!(json["sync"].get("error").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<SdnStatus>(json).unwrap(), status);
    }

    #[test]
    fn node_sdn_writes_only_what_it_has_and_reads_unknown_states() {
        assert_eq!(
            serde_json::to_value(NodeSdn::default()).unwrap(),
            json!({ "chassis": "missing" })
        );
        let full = NodeSdn {
            physnets: Vec::new(),
            gateway: false,
            lockdown: None,
            chassis: ChassisState::Ready,
            ovn_version: Some("24.03.4".into()),
            ovs_version: Some("3.3.0".into()),
            encap_ip: Some(ip("10.0.0.2")),
            central: Some(CentralHealth {
                nb: RaftRole::Leader,
                sb: RaftRole::Follower,
                northd_active: true,
            }),
            error: None,
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["central"]["nb"], "leader");
        assert_eq!(serde_json::from_value::<NodeSdn>(json).unwrap(), full);
        // 뒤 버전이 더한 값은 모름으로 읽는다(노드 상태 전체를 못 읽게 되지 않게).
        let later: NodeSdn = serde_json::from_value(json!({
            "chassis": "draining",
            "central": { "nb": "learner", "sb": "leader" }
        }))
        .unwrap();
        assert_eq!(later.chassis, ChassisState::Unknown);
        let central = later.central.unwrap();
        assert_eq!(central.nb, RaftRole::Unknown);
        assert!(!central.northd_active);
    }

    #[test]
    fn requests_parse() {
        let r: SdnInitRequest = serde_json::from_value(json!({ "node": "n1" })).unwrap();
        assert_eq!(r.address, None);
        let r: SdnInitRequest =
            serde_json::from_value(json!({ "node": "n1", "address": "10.0.0.1" })).unwrap();
        assert_eq!(r.address, Some(ip("10.0.0.1")));
        assert!(serde_json::from_value::<SdnNodeRequest>(json!({ "node": "n1", "x": 1 })).is_err());
        let ok = |v| {
            serde_json::from_value::<SdnChassisRequest>(v)
                .unwrap()
                .validate()
        };
        assert_eq!(ok(json!({ "nodes": ["n1", "n2"] })), Ok(()));
        assert_eq!(ok(json!({ "all": true })), Ok(()));
        assert!(ok(json!({})).is_err());
        assert!(ok(json!({ "nodes": ["n1"], "all": true })).is_err());
    }

    fn node(name: &str, witness: bool, sdn: Option<NodeSdn>) -> Node {
        let mut n = Resource::new(
            name,
            NodeSpec {
                sdn: None,
                address: format!("{name}:8006"),
                maintenance: false,
                witness,
                roles: None,
                host_resources: None,
            },
        );
        n.status = NodeStatus {
            sdn,
            ..NodeStatus::default()
        };
        n
    }

    /// 중앙 주소: 노드 주소의 IP, 이름이면 underlay, 둘 다 없으면 None
    #[test]
    fn central_addresses() {
        let mut n = node("n1", false, None);
        assert_eq!(central_address(&n), None);
        n.status.underlay = Some(ip("10.1.0.5"));
        assert_eq!(central_address(&n), Some(ip("10.1.0.5")));
        n.spec.address = "10.0.0.1:8006".into();
        assert_eq!(central_address(&n), Some(ip("10.0.0.1")));
        n.spec.address = "[fd00::1]:8006".into();
        assert_eq!(central_address(&n), Some(ip("fd00::1")));
    }

    #[test]
    fn view_before_and_after_init() {
        let central = NodeSdn {
            chassis: ChassisState::Ready,
            central: Some(CentralHealth {
                nb: RaftRole::Leader,
                sb: RaftRole::Leader,
                northd_active: true,
            }),
            ..NodeSdn::default()
        };
        let nodes = vec![
            node("n2", false, None),
            node("n1", false, Some(central.clone())),
            node("w", true, None),
        ];
        let before = SdnView::new(None, &nodes, Vec::new());
        assert_eq!(before.phase, None);
        assert!(before.centrals.is_empty());
        let names: Vec<_> = before.chassis.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["n1", "n2"]);
        let json = serde_json::to_value(&before).unwrap();
        assert!(json.get("phase").is_none(), "{json}");

        let gone = Uuid::now_v7();
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status.phase = SdnPhase::Ready;
        sdn.status.centrals = vec![
            SdnCentral {
                node: nodes[1].id,
                address: ip("10.0.0.1"),
            },
            SdnCentral {
                node: gone,
                address: ip("10.0.0.9"),
            },
        ];
        let after = SdnView::new(Some(&sdn), &nodes, vec![gone]);
        assert_eq!(after.phase, Some(SdnPhase::Ready));
        assert_eq!(after.centrals[0].name, "n1");
        assert_eq!(after.centrals[0].health, central.central);
        assert_eq!(after.centrals[1].name, gone.to_string());
        assert_eq!(after.centrals[1].health, None);
        assert_eq!(after.chassis[0].sdn, Some(central.clone()));
        assert!(after.strays.is_empty());
        assert!(
            serde_json::to_value(&after)
                .unwrap()
                .get("strays")
                .is_none()
        );
    }

    /// 리뷰 2 N-2·웹 I-3: 중앙 목록 밖에서 DB를 알린 노드와 지운 노드는 `strays`다. 준비 전과 그 노드에서 중앙 Task가
    /// 도는 동안(합류 중)은 아니다.
    #[test]
    fn view_lists_stray_central_databases() {
        let health = NodeSdn {
            central: Some(CentralHealth {
                nb: RaftRole::Follower,
                sb: RaftRole::Joining,
                northd_active: false,
            }),
            ..NodeSdn::default()
        };
        let nodes = vec![
            node("n1", false, Some(health.clone())),
            node("n2", false, Some(health)),
        ];
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status.centrals = vec![SdnCentral {
            node: nodes[0].id,
            address: ip("10.0.0.1"),
        }];
        assert!(
            SdnView::new(Some(&sdn), &nodes, Vec::new())
                .strays
                .is_empty()
        );
        sdn.status.phase = SdnPhase::Ready;
        let gone = Uuid::now_v7();
        sdn.status.departed = vec![SdnDeparted {
            node: gone,
            name: "n0".into(),
            address: ip("10.0.0.9"),
        }];
        let view = SdnView::new(Some(&sdn), &nodes, Vec::new());
        let strays: Vec<_> = view
            .strays
            .iter()
            .map(|s| (s.name.as_str(), s.gone))
            .collect();
        assert_eq!(strays, [("n0", true), ("n2", false)]);
        let json = serde_json::to_value(&view).unwrap();
        assert!(json["strays"][1].get("gone").is_none(), "{json}");
        sdn.status.pending = Some(SdnPending {
            op: crate::task::TaskOp::SdnCentralAdd,
            node: nodes[1].id,
            task: Uuid::now_v7(),
        });
        let view = SdnView::new(Some(&sdn), &nodes, Vec::new());
        assert_eq!(view.strays.len(), 1);
    }

    /// 계획 66: 노드 상태의 physnet·게이트웨이는 비면 쓰지 않아 계획 65 JSON이 바이트 그대로다.
    #[test]
    fn node_sdn_routing_fields_are_written_only_when_set() {
        let old = r#"{"chassis":"ready","ovn_version":"24.03.8","encap_ip":"10.0.0.2"}"#;
        let s: NodeSdn = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&s).unwrap(), old);
        let s = NodeSdn {
            physnets: vec![
                PhysnetState {
                    name: "ext".into(),
                    bridge: "vmbr1".into(),
                    ready: true,
                    error: None,
                },
                PhysnetState {
                    name: "dmz".into(),
                    bridge: "vmbr2".into(),
                    ready: false,
                    error: Some("bridge vmbr2 does not exist".into()),
                },
            ],
            gateway: true,
            ..s
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["gateway"], true);
        assert_eq!(
            json["physnets"][0],
            json!({ "name": "ext", "bridge": "vmbr1", "ready": true })
        );
        assert_eq!(serde_json::from_value::<NodeSdn>(json).unwrap(), s);
        assert!(s.physnet_ready("ext"));
        assert!(!s.physnet_ready("dmz"));
        assert!(!s.physnet_ready("lan"));
    }

    #[test]
    fn node_sdn_config_validates_and_round_trips() {
        assert_eq!(
            serde_json::to_string(&NodeSdnConfig::default()).unwrap(),
            "{}"
        );
        let c: NodeSdnConfig = serde_json::from_value(json!({
            "physnets": { "ext": "vmbr1", "dmz": "br-dmz" },
            "gateway": 200
        }))
        .unwrap();
        assert_eq!(c.validate(), Ok(()));
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({ "physnets": { "dmz": "br-dmz", "ext": "vmbr1" }, "gateway": 200 })
        );
        assert!(serde_json::from_value::<NodeSdnConfig>(json!({ "gw": 1 })).is_err());
        let with = |physnets: &[(&str, &str)], gateway: Option<u16>| NodeSdnConfig {
            physnets: physnets
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            gateway,
        };
        assert_eq!(
            with(&[("Ext", "vmbr1")], None).validate(),
            Err(NodeSdnError::BadPhysnet("Ext".into()))
        );
        for bad in ["", "br-int", "oplv10", "a/b", "abcdefghijklmnop"] {
            assert_eq!(
                with(&[("ext", bad)], None).validate(),
                Err(NodeSdnError::BadBridge("ext".into(), bad.into())),
                "{bad}"
            );
        }
        let (_, veth, _) = physnet_ifnames("0123456789ab", "ext");
        assert!(with(&[("ext", &veth)], None).validate().is_err());
        for (p, ok) in [(0, false), (1, true), (1000, true), (1001, false)] {
            assert_eq!(with(&[], Some(p)).validate().is_ok(), ok, "{p}");
        }
        let many: Vec<(String, String)> =
            (0..9).map(|i| (format!("p{i}"), "vmbr1".into())).collect();
        let many: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            with(&many, None).validate(),
            Err(NodeSdnError::TooManyPhysnets)
        );
    }

    #[test]
    fn physnet_device_names() {
        let (o, b, x) = physnet_ifnames("0123456789ab", "ext");
        let h8 = &o[4..12];
        assert_eq!((o.len(), b.len(), x.len()), (13, 13, 13));
        assert_eq!(o, format!("oplp{h8}o"));
        assert_eq!(b, format!("oplp{h8}b"));
        assert_eq!(x, format!("oplp{h8}x"));
        for n in [&o, &b, &x] {
            assert!(is_physnet_ifname(n), "{n}");
        }
        // 노드마다 같고, 클러스터·physnet이 다르면 다르다.
        assert_eq!(physnet_ifnames("0123456789ab", "ext"), (o.clone(), b, x));
        assert_ne!(physnet_ifnames("0123456789ab", "dmz").0, o);
        assert_ne!(physnet_ifnames("ba9876543210", "ext").0, o);
        // VXLAN 장치(`oplx<vni>`)와 겹치지 않는다.
        assert!(!is_physnet_ifname("oplx12345678"));
        assert!(!is_physnet_ifname("oplp1234567o"));
        assert!(!is_physnet_ifname("oplp12345678oo"));
    }

    #[test]
    fn valid_physnet_names() {
        for ok in ["ext", "a", "physnet-1", &"a".repeat(31)] {
            assert!(valid_physnet(ok), "{ok}");
        }
        for bad in ["", "1a", "-a", "A", "a_b", "a:b", "a,b", &"a".repeat(32)] {
            assert!(!valid_physnet(bad), "{bad}");
        }
    }

    #[test]
    fn router_macs_are_stable_local_and_distinct() {
        let (r1, r2, n1, n2) = (
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(11),
            Uuid::from_u128(12),
        );
        let m = router_mac(r1, n1);
        assert_eq!(m, router_mac(r1, n1));
        assert_eq!(m.0[0], 0x02);
        let all = [
            m,
            router_mac(r1, n2),
            router_mac(r2, n1),
            router_mac(r2, n2),
        ];
        let set: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(set.len(), 4);
        // DHCP 서버 MAC(네트워크 id만)과 겹치지 않는다.
        assert_ne!(router_mac(r1, n1), crate::ipam::server_mac(n1));
    }

    #[test]
    fn gateway_ranking_orders_by_priority_then_hash() {
        let router = Uuid::from_u128(77);
        let nodes: Vec<Uuid> = (1..=7).map(Uuid::from_u128).collect();
        let candidates: Vec<(Uuid, u16)> = nodes.iter().map(|n| (*n, 100)).collect();
        let ranked = gateway_ranking(router, &candidates);
        assert_eq!(ranked.len(), MAX_GATEWAYS);
        assert_eq!(
            ranked.iter().map(|r| r.1).collect::<Vec<_>>(),
            vec![32767, 32766, 32765, 32764, 32763]
        );
        // 같은 입력은 같은 답(순서와 상관없이)
        let mut reversed = candidates.clone();
        reversed.reverse();
        assert_eq!(gateway_ranking(router, &reversed), ranked);
        // 관리자 우선순위가 해시보다 먼저다.
        let mut favoured = candidates.clone();
        favoured[6].1 = 200;
        assert_eq!(gateway_ranking(router, &favoured)[0], (nodes[6], 32767));
        assert!(gateway_ranking(router, &[]).is_empty());
        assert_eq!(
            gateway_ranking(router, &[(nodes[0], 1)]),
            vec![(nodes[0], 32767)]
        );
    }

    /// HRW: 같은 우선순위의 노드 넷에 라우터 1000개가 고르게 흩어지고, 노드 하나를 더하면 그 노드가 이긴 라우터만
    /// 주인이 바뀐다.
    #[test]
    fn gateway_ranking_spreads_routers_and_moves_few() {
        let nodes: Vec<(Uuid, u16)> = (1..=4).map(|i| (Uuid::from_u128(i), 100)).collect();
        let routers: Vec<Uuid> = (0..1000u128)
            .map(|i| Uuid::from_u128(1_000_000 + i))
            .collect();
        let owner = |r: Uuid, c: &[(Uuid, u16)]| gateway_ranking(r, c)[0].0;
        let mut count = std::collections::BTreeMap::new();
        for r in &routers {
            *count.entry(owner(*r, &nodes)).or_insert(0) += 1;
        }
        for (node, n) in &count {
            assert!((150..=350).contains(n), "{node}: {n}");
        }
        let new = (Uuid::from_u128(5), 100);
        let mut more = nodes.clone();
        more.push(new);
        let mut moved = 0;
        for r in &routers {
            let (before, after) = (owner(*r, &nodes), owner(*r, &more));
            if before != after {
                assert_eq!(after, new.0, "only the new node takes routers");
                moved += 1;
            }
        }
        assert!((100..=300).contains(&moved), "{moved}");
    }

    /// 계획 67: 잠금·실패 묶음 칸이 없으면 옛 SDN·노드 SDN JSON이 바이트까지 같고, 있으면 왕복한다.
    #[test]
    fn sdn_services_fields_are_written_only_when_set() {
        let old = r#"{"phase":"ready","centrals":[{"node":"00000000-0000-0000-0000-000000000001","address":"10.0.0.1"}],"sync":{"term":3,"index":9,"at":100,"nb_cfg":2,"hv_cfg":2}}"#;
        let st: SdnStatus = serde_json::from_str(old).unwrap();
        assert_eq!(st.lockdown, None);
        assert!(st.sync.as_ref().unwrap().failed.is_empty());
        assert_eq!(serde_json::to_string(&st).unwrap(), old);
        let old = r#"{}"#;
        let spec: SdnSpec = serde_json::from_str(old).unwrap();
        assert!(!spec.lockdown);
        assert_eq!(serde_json::to_string(&spec).unwrap(), old);
        let spec = SdnSpec { lockdown: true };
        assert_eq!(
            serde_json::to_string(&spec).unwrap(),
            r#"{"lockdown":true}"#
        );
        assert_eq!(
            serde_json::from_str::<SdnSpec>(r#"{"lockdown":true}"#).unwrap(),
            spec
        );
        let mut st = st;
        st.lockdown = Some(LockdownState {
            applied: vec![Uuid::from_u128(1)],
            pending: true,
            error: None,
        });
        st.sync.as_mut().unwrap().failed = vec!["network tenant-a".into()];
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(
            json["lockdown"],
            json!({ "applied": ["00000000-0000-0000-0000-000000000001"], "pending": true })
        );
        assert_eq!(json["sync"]["failed"], json!(["network tenant-a"]));
        assert_eq!(serde_json::from_value::<SdnStatus>(json).unwrap(), st);
        let old = r#"{"chassis":"ready","encap_ip":"10.0.0.2","physnets":[{"name":"ext","bridge":"vmbr1","ready":true}],"gateway":true}"#;
        let ns: NodeSdn = serde_json::from_str(old).unwrap();
        assert_eq!(ns.lockdown, None);
        assert_eq!(serde_json::to_string(&ns).unwrap(), old);
        let ns = NodeSdn {
            lockdown: Some(LockdownNodeState {
                firewall: true,
                allowed: 5,
                error: None,
            }),
            ..ns
        };
        let json = serde_json::to_value(&ns).unwrap();
        assert_eq!(json["lockdown"], json!({ "firewall": true, "allowed": 5 }));
        assert_eq!(serde_json::from_value::<NodeSdn>(json).unwrap(), ns);
    }

    #[test]
    fn migration_physnets_are_valid_and_reserved() {
        let a = migration_physnet("t1", Uuid::from_u128(1));
        assert_eq!(a.len(), 11);
        assert!(a.starts_with("mig"));
        assert!(valid_physnet(&a));
        assert!(is_migration_physnet(&a));
        assert_eq!(a, migration_physnet("t1", Uuid::from_u128(1)));
        assert_ne!(a, migration_physnet("t2", Uuid::from_u128(1)));
        assert_ne!(a, migration_physnet("t1", Uuid::from_u128(2)));
        for other in [
            "migration",
            "mig1234567",
            "mig123456789",
            "migABCDEF12",
            "ext",
        ] {
            assert!(!is_migration_physnet(other), "{other}");
        }
        // 관리자 physnet으로 쓸 수 없다(같은 모양만 — `migration` 같은 이름은 된다).
        let cfg = NodeSdnConfig {
            physnets: [(a.clone(), "vmbr1".to_string())].into(),
            gateway: None,
        };
        assert_eq!(cfg.validate(), Err(NodeSdnError::ReservedPhysnet(a)));
        let cfg = NodeSdnConfig {
            physnets: [("migration".to_string(), "vmbr1".to_string())].into(),
            gateway: None,
        };
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn allow_sets_follow_members_and_centrals() {
        let mk = |n: u128,
                  addr: &str,
                  underlay: Option<&str>,
                  witness: bool,
                  hv: &str,
                  encap: Option<&str>| {
            let mut node: Node = Resource::new(
                format!("n{n}"),
                serde_json::from_value(json!({ "address": addr, "witness": witness })).unwrap(),
            );
            node.id = Uuid::from_u128(n);
            node.status.underlay = underlay.map(ip);
            node.status.hypervisor = hv.into();
            node.status.sdn = encap.map(|e| NodeSdn {
                encap_ip: Some(ip(e)),
                ..Default::default()
            });
            node
        };
        let nodes = vec![
            mk(
                1,
                "10.0.0.1:8006",
                Some("192.168.0.1"),
                false,
                "qemu-kvm",
                Some("192.168.0.1"),
            ),
            mk(
                2,
                "10.0.0.2:8006",
                None,
                false,
                "qemu-kvm",
                Some("10.0.0.2"),
            ),
            // 이름 주소(IP 아님)는 underlay만
            mk(
                3,
                "n3.lab:8006",
                Some("192.168.0.3"),
                false,
                "qemu-kvm",
                None,
            ),
            mk(4, "10.0.0.4:8006", None, true, "", None),
            mk(5, "10.0.0.5:8006", None, false, "qemu-hvf", None),
        ];
        let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec::default());
        sdn.status.centrals = vec![
            SdnCentral {
                node: Uuid::from_u128(1),
                address: ip("10.0.0.1"),
            },
            SdnCentral {
                node: Uuid::from_u128(2),
                address: ip("10.0.0.2"),
            },
        ];
        let sets = allow_sets(&nodes, &sdn);
        let ips = |v: &[&str]| v.iter().map(|s| ip(s)).collect::<Vec<_>>();
        assert_eq!(
            sets.members,
            ips(&["10.0.0.1", "10.0.0.2", "192.168.0.1", "192.168.0.3"])
        );
        assert_eq!(sets.centrals, ips(&["10.0.0.1", "10.0.0.2", "192.168.0.1"]));
        assert_eq!(sets.encaps, ips(&["10.0.0.2", "192.168.0.1"]));
        // 지운 노드는 목록에 없으므로 빠진다.
        let sets = allow_sets(&nodes[1..], &sdn);
        assert!(!sets.members.contains(&ip("10.0.0.1")));
        assert_eq!(sets.centrals, ips(&["10.0.0.1", "10.0.0.2"]));
    }
}
