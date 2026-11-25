use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Resource<S, T> {
    pub id: Uuid,
    pub name: String,
    /// 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
    pub status: T,
}

impl<S, T: Default> Resource<S, T> {
    pub fn new(name: impl Into<String>, spec: S) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            revision: 0,
            labels: BTreeMap::new(),
            spec,
            status: T::default(),
        }
    }
}

/// 클러스터 저장소가 구분하는 리소스 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Vm,
    StoragePool,
    Volume,
    Node,
    Task,
    User,
    ApiToken,
    Cluster,
    JoinToken,
    Network,
    Snapshot,
    SecurityGroup,
    Backup,
    BackupJob,
    VmImport,
    Settings,
    Realm,
    Remote,
    Session,
    VipHolder,
    HaGroup,
    PlacementRule,
    NodeFence,
    HaCheck,
    Ceph,
    AlertRule,
    NotificationTarget,
    Folder,
    Grant,
    AccessRole,
    Group,
    TwoFactor,
    CpuCatalog,
    Sdn,
    Router,
    FloatingIp,
    /// OVN 부하 분산기(계획 67)
    LoadBalancer,
    /// 단순 → SDN 네트워크 옮기기(계획 67)
    NetworkMigration,
    /// 백업 저장소(계획 68)
    BackupStore,
    /// 저장한 vCenter·ESXi 연결(계획 72)
    VmwareSource,
    /// VMware 이전 계획(계획 72)
    VmwareMigration,
    /// VMware 이전 계획 안의 VM 하나(계획 72)
    VmwareVm,
    /// VM의 OVA/OVF 내보내기(계획 72)
    VmExport,
    /// 선언형 적용 묶음 — 마지막 선언·드리프트(계획 73)
    ApplySet,
    /// ACME 계정(계획 73)
    AcmeAccount,
    /// ACME DNS-01 공급자(계획 73)
    AcmeDns,
    /// ACME로 받는 공개 인증서(계획 73)
    Certificate,
}

impl Kind {
    /// 모든 종류(선언 순서). 종류마다 도는 시험과 권한 표가 쓴다.
    pub const ALL: [Kind; 47] = [
        Kind::Vm,
        Kind::StoragePool,
        Kind::Volume,
        Kind::Node,
        Kind::Task,
        Kind::User,
        Kind::ApiToken,
        Kind::Cluster,
        Kind::JoinToken,
        Kind::Network,
        Kind::Snapshot,
        Kind::SecurityGroup,
        Kind::Backup,
        Kind::BackupJob,
        Kind::VmImport,
        Kind::Settings,
        Kind::Realm,
        Kind::Remote,
        Kind::Session,
        Kind::VipHolder,
        Kind::HaGroup,
        Kind::PlacementRule,
        Kind::NodeFence,
        Kind::HaCheck,
        Kind::Ceph,
        Kind::AlertRule,
        Kind::NotificationTarget,
        Kind::Folder,
        Kind::Grant,
        Kind::AccessRole,
        Kind::Group,
        Kind::TwoFactor,
        Kind::CpuCatalog,
        Kind::Sdn,
        Kind::Router,
        Kind::FloatingIp,
        Kind::LoadBalancer,
        Kind::NetworkMigration,
        Kind::BackupStore,
        Kind::VmwareSource,
        Kind::VmwareMigration,
        Kind::VmwareVm,
        Kind::VmExport,
        Kind::ApplySet,
        Kind::AcmeAccount,
        Kind::AcmeDns,
        Kind::Certificate,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Vm => "vm",
            Kind::StoragePool => "storage-pool",
            Kind::Volume => "volume",
            Kind::Node => "node",
            Kind::Task => "task",
            Kind::User => "user",
            Kind::ApiToken => "api-token",
            Kind::Cluster => "cluster",
            Kind::JoinToken => "join-token",
            Kind::Network => "network",
            Kind::Snapshot => "snapshot",
            Kind::SecurityGroup => "security-group",
            Kind::Backup => "backup",
            Kind::BackupJob => "backup-job",
            Kind::VmImport => "vm-import",
            Kind::Settings => "settings",
            Kind::Realm => "realm",
            Kind::Remote => "remote",
            Kind::Session => "session",
            Kind::VipHolder => "vip-holder",
            Kind::HaGroup => "ha-group",
            Kind::PlacementRule => "placement-rule",
            Kind::NodeFence => "node-fence",
            Kind::HaCheck => "ha-check",
            Kind::Ceph => "ceph",
            Kind::AlertRule => "alert-rule",
            Kind::NotificationTarget => "notification-target",
            Kind::Folder => "folder",
            Kind::Grant => "grant",
            Kind::AccessRole => "access-role",
            Kind::Group => "group",
            Kind::TwoFactor => "two-factor",
            Kind::CpuCatalog => "cpu-catalog",
            Kind::Sdn => "sdn",
            Kind::Router => "router",
            Kind::FloatingIp => "floating-ip",
            Kind::LoadBalancer => "load-balancer",
            Kind::NetworkMigration => "network-migration",
            Kind::BackupStore => "backup-store",
            Kind::VmwareSource => "vmware-source",
            Kind::VmwareMigration => "vmware-migration",
            Kind::VmwareVm => "vmware-vm",
            Kind::VmExport => "vm-export",
            Kind::ApplySet => "apply-set",
            Kind::AcmeAccount => "acme-account",
            Kind::AcmeDns => "acme-dns",
            Kind::Certificate => "certificate",
        }
    }

    /// 설정 이력(계획 73 §7.2)을 남기는 종류인지. 설정 리소스(사람·apply·Terraform이 spec을 고치는 것)만 남기고, 작업 기록
    /// (자기 status가 이력)·비밀 덩어리·클러스터 설치 상태·노드가 스스로 쓰는 것은 뺀다. 새 종류는 여기서 넣거나 빼야
    /// 시험(`every_kind_is_classified_for_history`)이 통과한다.
    pub fn history_tracked(self) -> bool {
        match self {
            Kind::Vm
            | Kind::StoragePool
            | Kind::Volume
            | Kind::Network
            | Kind::SecurityGroup
            | Kind::BackupJob
            | Kind::Settings
            | Kind::Realm
            | Kind::Remote
            | Kind::HaGroup
            | Kind::PlacementRule
            | Kind::NodeFence
            | Kind::HaCheck
            | Kind::AlertRule
            | Kind::NotificationTarget
            | Kind::Folder
            | Kind::Grant
            | Kind::AccessRole
            | Kind::Group
            | Kind::Router
            | Kind::FloatingIp
            | Kind::LoadBalancer
            | Kind::BackupStore
            | Kind::VmwareSource
            | Kind::ApplySet
            | Kind::AcmeAccount
            | Kind::AcmeDns
            | Kind::Certificate
            | Kind::User => true,
            Kind::Node
            | Kind::Task
            | Kind::ApiToken
            | Kind::Cluster
            | Kind::JoinToken
            | Kind::Snapshot
            | Kind::Backup
            | Kind::VmImport
            | Kind::Session
            | Kind::VipHolder
            | Kind::Ceph
            | Kind::TwoFactor
            | Kind::CpuCatalog
            | Kind::Sdn
            | Kind::NetworkMigration
            | Kind::VmwareMigration
            | Kind::VmwareVm
            | Kind::VmExport => false,
        }
    }

    /// 이름(`as_str`)으로 찾는다.
    pub fn parse(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// 리소스 이름 규칙: DNS 레이블처럼 1~63자의 영문 소문자, 숫자, `-`이고 `-`로 시작하거나 끝나지 않는다.
/// 저장소는 이름의 유일성만 보므로 API가 생성할 때 확인한다.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    let ok = (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(NameError(name.to_string()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid name {0:?}: use 1-63 lowercase letters, digits and '-', not starting or ending with '-'"
)]
pub struct NameError(String);

impl NameError {
    pub(crate) fn new(name: &str) -> Self {
        Self(name.to_string())
    }
}

/// spec 타입과 리소스 종류, status 타입을 묶는다. 클러스터 저장소의 타입 있는 API가 쓴다.
pub trait ResourceSpec: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    const KIND: Kind;
    type Status: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
    struct DummyStatus {
        ready: bool,
    }

    #[test]
    fn new_resource_has_v7_id_zero_revision_and_default_status() {
        let r: Resource<u32, DummyStatus> = Resource::new("web-01", 7);
        assert_eq!(r.id.get_version_num(), 7);
        assert_eq!(r.name, "web-01");
        assert_eq!(r.revision, 0);
        assert!(r.labels.is_empty());
        assert_eq!(r.spec, 7);
        assert_eq!(r.status, DummyStatus::default());
    }

    #[test]
    fn kind_serializes_as_its_str() {
        for kind in Kind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
            assert_eq!(
                serde_json::from_value::<Kind>(kind.as_str().into()).unwrap(),
                kind
            );
        }
        // ALL은 빠짐없고 겹치지 않는다(새 종류를 더하면 길이와 함께 고친다).
        let set: std::collections::BTreeSet<Kind> = Kind::ALL.into_iter().collect();
        assert_eq!(set.len(), Kind::ALL.len());
        assert_eq!(
            Kind::ALL.last().map(|k| k.as_str()),
            Some("certificate"),
            "새 종류는 ALL 끝에도 더한다"
        );
        assert_eq!(Kind::parse("apply-set"), Some(Kind::ApplySet));
        assert_eq!(Kind::parse("nope"), None);
    }

    /// 계획 73 §7.2: 모든 종류가 이력에 넣을지 뺄지 정해져 있다(`history_tracked`의 match가 빠짐없음을 컴파일러가 보고, 이
    /// 시험은 설계 표의 수를 고정한다).
    #[test]
    fn every_kind_is_classified_for_history() {
        let tracked: Vec<&str> = Kind::ALL
            .into_iter()
            .filter(|k| k.history_tracked())
            .map(Kind::as_str)
            .collect();
        assert_eq!(tracked.len(), 29, "{tracked:?}");
        for k in [
            "task",
            "session",
            "node",
            "cluster",
            "api-token",
            "two-factor",
            "snapshot",
        ] {
            assert!(!tracked.contains(&k), "{k}");
        }
        for k in ["vm", "user", "apply-set", "certificate", "settings"] {
            assert!(tracked.contains(&k), "{k}");
        }
    }

    #[test]
    fn validates_names() {
        for ok in ["a", "web-01", "0", &"a".repeat(63)] {
            assert_eq!(validate_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "-a", "a-", "Web", "a_b", "a.b", "a b", &"a".repeat(64)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_labels_are_omitted_and_round_trip() {
        let r: Resource<u32, DummyStatus> = Resource::new("a", 1);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("labels").is_none());
        let back: Resource<u32, DummyStatus> = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
}
