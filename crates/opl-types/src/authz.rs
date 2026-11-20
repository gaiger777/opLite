//! 권한 모델(계획 62): 권한 목록과 비트 집합, 내장 역할, 사용자 정의 역할·grant·그룹, API 토큰 범위.
//!
//! grant = (주체, 역할, 대상, propagate)이고 유효 권한은 전역 역할(루트 암묵 grant)과 grant의 합집합이다. 거부
//! 규칙은 없다. 평가의 참조 구현은 [`crate::authz_eval`]에 있다.

use std::collections::BTreeSet;
use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::auth::Role;
use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

/// 사용자 정의 역할. 리소스 이름이 역할 이름이고 내장 역할 이름([`Role::as_str`])은 쓸 수 없다.
pub type AccessRole = Resource<AccessRoleSpec, AccessRoleStatus>;
/// 권한 하나. 리소스 이름은 id 문자열이다.
pub type Grant = Resource<GrantSpec, GrantStatus>;
/// 로컬 그룹. 리소스 이름이 그룹 이름이다.
pub type Group = Resource<GroupSpec, GroupStatus>;

/// 핸들러가 지금(계획 61까지) 확인하는 동작. `Authorizer::check(principal, Action, Kind)`의 인자다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Read,
    /// 생성, 수정, 삭제
    Write,
    /// 시작·정지 같은 동작과 콘솔
    Operate,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Read, Action::Write, Action::Operate];
}

macro_rules! privileges {
    ($($(#[$doc:meta])* $variant:ident = $name:literal,)*) => {
        /// 권한(설계 §6.2). 저장·API에서는 `"vm.power"` 같은 문자열이다.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
        pub enum Privilege {
            $($(#[$doc])* #[serde(rename = $name)] $variant,)*
        }

        impl Privilege {
            /// 모든 권한(비트 순서)
            pub const ALL: &'static [Privilege] = &[$(Privilege::$variant,)*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Privilege::$variant => $name,)*
                }
            }

            /// 이름으로 찾는다. 모르는 이름(뒤 버전이 더한 권한 포함)은 None이다.
            pub fn parse(name: &str) -> Option<Privilege> {
                match name {
                    $($name => Some(Privilege::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

privileges! {
    /// 보기: 목록·상세·watch·지표·Task·경보. 다른 어떤 권한이든 그 객체의 이것을 품는다.
    SystemRead = "system.read",
    /// 폴더에 VM 만들기(새로·가져오기·복원·배포 받기)
    VmCreate = "vm.create",
    VmDelete = "vm.delete",
    /// 스펙 바꾸기, CD, cloud-init, 펌웨어 초기화, 핫플러그, 템플릿으로 바꾸기·되돌리기
    VmConfig = "vm.config",
    /// 켜기·끄기·재시작·에이전트 종료
    VmPower = "vm.power",
    VmConsole = "vm.console",
    VmSnapshot = "vm.snapshot",
    /// 라이브·저장소 옮기기
    VmMigrate = "vm.migrate",
    /// 원본(템플릿) 쪽 복제·배포
    VmClone = "vm.clone",
    /// 전용 CPU·hugepages VM(계획 64): 스펙에 `cpu.policy: dedicated`나 `hugepages`가 있는 VM을 만들기·그 칸 바꾸기·
    /// 복제·배포·가져오기·복원. 드문 노드 자원이라 operator에 넣지 않는다.
    VmDedicated = "vm.dedicated",
    BackupCreate = "backup.create",
    BackupRestore = "backup.restore",
    BackupDelete = "backup.delete",
    /// 백업 작업(루트)
    BackupJob = "backup.job",
    /// 풀에 볼륨 만들기·늘리기·지우기
    StorageAllocate = "storage.allocate",
    /// 풀 만들기·바꾸기
    StorageConfig = "storage.config",
    /// NIC를 그 네트워크에 붙이기
    NetworkAssign = "network.assign",
    /// 네트워크·보안 그룹 쓰기
    NetworkConfig = "network.config",
    /// 하위 폴더 만들기·이름 바꾸기·지우기
    InventoryFolder = "inventory.folder",
    /// 옮기기(원본·대상 둘 다)
    InventoryMove = "inventory.move",
    /// 자식 폴더 할당량
    InventoryQuota = "inventory.quota",
    /// 이 대상 아래 grant 주기·빼기(자기 권한 안에서)
    PermissionModify = "permission.modify",
    AlertRead = "alert.read",
    /// 경보 규칙 쓰기(루트)
    AlertManage = "alert.manage",
    /// 노드·클러스터·설정·HA·배치·Ceph·SDN·원격 읽기, 노드·클러스터 지표, Prometheus
    ClusterRead = "cluster.read",
    /// 계정·영역·남의 토큰·알림 대상·감사 로그·설정·노드·HA·배치·Ceph·SDN·원격 쓰기, 역할·그룹·루트 grant
    ClusterAdmin = "cluster.admin",
}

impl Privilege {
    fn bit(self) -> u64 {
        1 << (self as u32)
    }
}

impl fmt::Display for Privilege {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 권한 비트 집합. JSON은 권한 이름 목록이고, 읽을 때 모르는 이름은 버린다(뒤 버전이 더한 권한).
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PrivSet(u64);

impl PrivSet {
    pub const EMPTY: PrivSet = PrivSet(0);

    /// 모든 권한
    pub fn all() -> PrivSet {
        Privilege::ALL.iter().copied().collect()
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, p: Privilege) -> bool {
        self.0 & p.bit() != 0
    }

    pub fn insert(&mut self, p: Privilege) {
        self.0 |= p.bit();
    }

    pub fn is_subset(self, other: PrivSet) -> bool {
        self.0 & !other.0 == 0
    }

    /// `self`에는 있고 `other`에는 없는 권한
    pub fn difference(self, other: PrivSet) -> PrivSet {
        PrivSet(self.0 & !other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Privilege> {
        Privilege::ALL
            .iter()
            .copied()
            .filter(move |p| self.contains(*p))
    }

    /// 권한이 하나라도 있으면 `system.read`를 더한다(모든 권한은 그 객체 보기를 품는다).
    pub fn with_implied(self) -> PrivSet {
        if self.is_empty() {
            self
        } else {
            self | Privilege::SystemRead.into()
        }
    }

    /// 이름 목록을 푼다. 모르는 이름은 집합에 넣지 않고 둘째 값으로 돌려준다(부르는 쪽이 경고를 남긴다).
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> (PrivSet, Vec<String>) {
        let mut set = PrivSet::EMPTY;
        let mut unknown = Vec::new();
        for n in names {
            match Privilege::parse(n.as_ref()) {
                Some(p) => set.insert(p),
                None => unknown.push(n.as_ref().to_string()),
            }
        }
        (set, unknown)
    }

    /// 이름 목록(비트 순서)
    pub fn names(self) -> Vec<&'static str> {
        self.iter().map(Privilege::as_str).collect()
    }
}

impl From<Privilege> for PrivSet {
    fn from(p: Privilege) -> Self {
        PrivSet(p.bit())
    }
}

impl FromIterator<Privilege> for PrivSet {
    fn from_iter<I: IntoIterator<Item = Privilege>>(iter: I) -> Self {
        let mut set = PrivSet::EMPTY;
        for p in iter {
            set.insert(p);
        }
        set
    }
}

impl BitOr for PrivSet {
    type Output = PrivSet;
    fn bitor(self, rhs: PrivSet) -> PrivSet {
        PrivSet(self.0 | rhs.0)
    }
}

impl BitOrAssign for PrivSet {
    fn bitor_assign(&mut self, rhs: PrivSet) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for PrivSet {
    type Output = PrivSet;
    fn bitand(self, rhs: PrivSet) -> PrivSet {
        PrivSet(self.0 & rhs.0)
    }
}

impl fmt::Debug for PrivSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.names()).finish()
    }
}

impl Serialize for PrivSet {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.names())
    }
}

impl<'de> Deserialize<'de> for PrivSet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let names = Vec::<String>::deserialize(d)?;
        Ok(PrivSet::from_names(&names).0)
    }
}

/// 내장 역할의 권한(설계 §6.3). 지금 `RoleAuthorizer`의 뜻 그대로다: [`kind_privilege`]와 함께 쓰면 모든
/// (역할, 동작, 종류)에서 같은 답을 낸다. 새 권한(`inventory.*`·`permission.modify`·`network.config`·
/// `storage.config`)은 operator에 넣지 않는다.
pub fn builtin_privileges(role: Role) -> PrivSet {
    use Privilege::*;
    let viewer: PrivSet = [SystemRead, ClusterRead, AlertRead].into_iter().collect();
    match role {
        Role::Admin => PrivSet::all(),
        Role::Operator => {
            viewer
                | [
                    VmCreate,
                    VmDelete,
                    VmConfig,
                    VmPower,
                    VmConsole,
                    VmSnapshot,
                    VmMigrate,
                    VmClone,
                    BackupCreate,
                    BackupRestore,
                    BackupDelete,
                    BackupJob,
                    StorageAllocate,
                    NetworkAssign,
                ]
                .into_iter()
                .collect()
        }
        Role::Viewer => viewer,
        Role::None => PrivSet::EMPTY,
    }
}

/// 계정과 비밀을 담은 종류(전원 끄기 펜싱의 BMC 비밀번호, 알림 대상의 SMTP 비밀번호·webhook 토큰, 2FA 비밀,
/// 누가 무엇을 받았는지 담은 grant·그룹). 읽기도 `cluster.admin`이다.
pub fn is_account_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::User
            | Kind::ApiToken
            | Kind::JoinToken
            | Kind::Realm
            | Kind::Session
            | Kind::NodeFence
            | Kind::NotificationTarget
            | Kind::Grant
            | Kind::Group
            | Kind::TwoFactor
    )
}

/// 지금(종류 단위) 확인 `(Action, Kind)`이 요구하는 권한. 루트 확인 shim과 [`builtin_privileges`]의 동치 시험이
/// 쓴다. 객체를 따르는 확인은 계획 62 경로표가 더 좁은 권한(`vm.delete`·`vm.snapshot` 등)을 고른다.
pub fn kind_privilege(action: Action, kind: Kind) -> Privilege {
    use Privilege::*;
    if is_account_kind(kind) {
        return ClusterAdmin;
    }
    match action {
        Action::Read => match kind {
            Kind::AlertRule => AlertRead,
            Kind::Vm
            | Kind::Volume
            | Kind::Snapshot
            | Kind::Backup
            | Kind::VmImport
            | Kind::Network
            | Kind::Router
            | Kind::FloatingIp
            | Kind::LoadBalancer
            | Kind::NetworkMigration
            | Kind::StoragePool
            | Kind::Task
            | Kind::Folder => SystemRead,
            _ => ClusterRead,
        },
        Action::Write => match kind {
            Kind::Vm => VmConfig,
            Kind::VmImport => VmCreate,
            Kind::Volume => StorageAllocate,
            Kind::Backup => BackupCreate,
            Kind::BackupJob => BackupJob,
            Kind::StoragePool => StorageConfig,
            Kind::Network
            | Kind::SecurityGroup
            | Kind::Router
            | Kind::FloatingIp
            | Kind::LoadBalancer
            | Kind::NetworkMigration => NetworkConfig,
            Kind::AlertRule => AlertManage,
            Kind::Folder => InventoryFolder,
            _ => ClusterAdmin,
        },
        Action::Operate => match kind {
            Kind::Vm => VmPower,
            Kind::VmImport => VmCreate,
            Kind::Volume => StorageAllocate,
            Kind::Backup => BackupRestore,
            Kind::BackupJob => BackupJob,
            _ => ClusterAdmin,
        },
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthzError {
    #[error("unknown privilege {0:?}")]
    UnknownPrivilege(String),
    #[error("a role needs at least one privilege")]
    NoPrivileges,
    #[error("description is longer than {MAX_DESCRIPTION} characters")]
    LongDescription,
    #[error("{0:?} is a built-in role name")]
    ReservedRoleName(String),
    #[error(transparent)]
    BadName(#[from] NameError),
    #[error("role \"none\" grants nothing")]
    GrantNone,
    #[error(
        "permissions can be set on a vm, network, storage-pool, router or floating-ip object, not {0:?}"
    )]
    BadObjectKind(Kind),
    #[error("external group needs a realm and a group name of 1-{MAX_EXTERNAL_GROUP} characters")]
    BadExternalGroup,
    #[error("a token scope needs at least one privilege and one target")]
    EmptyScope,
}

/// 역할·그룹 설명의 최대 길이(문자 수)
pub const MAX_DESCRIPTION: usize = 256;
/// 외부 그룹 이름(LDAP DN·OIDC 그룹 값)의 최대 길이(바이트)
pub const MAX_EXTERNAL_GROUP: usize = 1024;
/// 클러스터 전체 grant 수 상한(리뷰 2 N6)
pub const MAX_GRANTS: usize = 10_000;
/// 사용자 한 명의 API 토큰 수 상한(리뷰 2 N6)
pub const MAX_USER_TOKENS: usize = 100;

/// 사용자 정의 역할 이름 규칙: 리소스 이름 규칙이고 내장 역할 이름(`admin`·`operator`·`viewer`·`none`)이 아니다.
pub fn validate_role_name(name: &str) -> Result<(), AuthzError> {
    validate_name(name)?;
    if Role::ALL.iter().any(|r| r.as_str() == name) {
        return Err(AuthzError::ReservedRoleName(name.to_string()));
    }
    Ok(())
}

/// 쓰기 검증: 이름이 모두 알려진 권한이어야 한다.
fn check_known(names: &[String]) -> Result<PrivSet, AuthzError> {
    let (set, unknown) = PrivSet::from_names(names);
    match unknown.into_iter().next() {
        Some(u) => Err(AuthzError::UnknownPrivilege(u)),
        None => Ok(set),
    }
}

/// 사용자 정의 역할(`Kind::AccessRole`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AccessRoleSpec {
    /// 권한 이름 목록. 이 빌드가 모르는 이름은 읽을 때 버린다(뒤 버전이 더한 권한).
    pub privileges: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub description: String,
}

impl ResourceSpec for AccessRoleSpec {
    const KIND: Kind = Kind::AccessRole;
    type Status = AccessRoleStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct AccessRoleStatus {}

impl AccessRoleSpec {
    /// 쓰기 검증: 권한이 하나 이상이고 모두 알려진 이름이다.
    pub fn validate(&self) -> Result<(), AuthzError> {
        if check_known(&self.privileges)?.is_empty() {
            return Err(AuthzError::NoPrivileges);
        }
        check_description(&self.description)
    }

    /// 권한 집합과 모르는 이름(버린 것)
    pub fn privset(&self) -> (PrivSet, Vec<String>) {
        PrivSet::from_names(&self.privileges)
    }
}

fn check_description(d: &str) -> Result<(), AuthzError> {
    if d.chars().count() > MAX_DESCRIPTION {
        Err(AuthzError::LongDescription)
    } else {
        Ok(())
    }
}

/// grant를 받는 주체
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrincipalRef {
    /// 사용자(로컬·외부)
    User { id: Uuid },
    /// 로컬 그룹
    Group { id: Uuid },
}

/// grant가 주는 역할
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoleRef {
    /// 내장 역할(`none`은 쓸 수 없다)
    Builtin { role: Role },
    /// 사용자 정의 역할(`AccessRole` id)
    Custom { id: Uuid },
}

/// grant를 건 곳
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum GrantTarget {
    /// 세 트리와 클러스터 객체 모두의 조상
    Root,
    /// 폴더(`Folder` id)
    Folder { id: Uuid },
    /// 단일 객체(VM·네트워크·스토리지 풀·라우터·유동 IP)
    Object { kind: Kind, id: Uuid },
}

impl GrantTarget {
    pub fn validate(&self) -> Result<(), AuthzError> {
        match self {
            // 폴더에 드는 종류(VM·네트워크·스토리지 풀, 계획 66 라우터·유동 IP)
            GrantTarget::Object { kind, .. } if crate::folder::FolderKind::of(*kind).is_none() => {
                Err(AuthzError::BadObjectKind(*kind))
            }
            _ => Ok(()),
        }
    }
}

/// 권한 하나(`Kind::Grant`): 주체에게 대상에서 역할을 준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub principal: PrincipalRef,
    pub role: RoleRef,
    pub target: GrantTarget,
    /// 하위 폴더와 그 안의 객체로 내려가는지. 거짓이면 대상 자체(폴더 이름 바꾸기·지우기·그 폴더의 권한 보기)에만
    /// 닿는다. 기본은 참이고 참이면 쓰지 않는다.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub propagate: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl ResourceSpec for GrantSpec {
    const KIND: Kind = Kind::Grant;
    type Status = GrantStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct GrantStatus {}

impl GrantSpec {
    /// 스펙만으로 보는 검사. 주체·역할·대상이 있는지는 API가 본다.
    pub fn validate(&self) -> Result<(), AuthzError> {
        if self.role == (RoleRef::Builtin { role: Role::None }) {
            return Err(AuthzError::GrantNone);
        }
        self.target.validate()
    }
}

/// 로컬 그룹(`Kind::Group`). 외부 사용자는 그 영역의 `external_groups`에 `external`의 그룹이 있으면 든다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    /// 멤버 사용자 id(로컬·외부)
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub members: Vec<Uuid>,
    /// 연결한 디렉터리 그룹
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub external: Vec<ExternalGroup>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub description: String,
}

impl ResourceSpec for GroupSpec {
    const KIND: Kind = Kind::Group;
    type Status = GroupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct GroupStatus {}

/// 영역의 디렉터리 그룹(LDAP 그룹 DN 또는 OIDC `groups` 클레임 값)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ExternalGroup {
    pub realm: String,
    pub group: String,
}

impl GroupSpec {
    pub fn validate(&self) -> Result<(), AuthzError> {
        for e in &self.external {
            let g = e.group.trim();
            if validate_name(&e.realm).is_err() || g.is_empty() || g.len() > MAX_EXTERNAL_GROUP {
                return Err(AuthzError::BadExternalGroup);
            }
        }
        check_description(&self.description)
    }

    /// 중복 없는 멤버
    pub fn member_set(&self) -> BTreeSet<Uuid> {
        self.members.iter().copied().collect()
    }
}

/// API 토큰의 권한 범위(설계 §6.6). `targets`마다 `privileges`를 건 propagate grant로 읽은 천장이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TokenScope {
    /// 권한 이름 목록. 모르는 이름은 읽을 때 버린다.
    pub privileges: Vec<String>,
    pub targets: Vec<GrantTarget>,
}

impl TokenScope {
    pub fn validate(&self) -> Result<(), AuthzError> {
        if check_known(&self.privileges)?.is_empty() || self.targets.is_empty() {
            return Err(AuthzError::EmptyScope);
        }
        self.targets.iter().try_for_each(GrantTarget::validate)
    }

    pub fn privset(&self) -> PrivSet {
        PrivSet::from_names(&self.privileges).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privilege_names_round_trip() {
        assert_eq!(Privilege::ALL.len(), 26);
        assert!(Privilege::ALL.len() <= 64);
        for (i, p) in Privilege::ALL.iter().enumerate() {
            assert_eq!(*p as usize, i);
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
            assert_eq!(Privilege::parse(p.as_str()), Some(*p));
            assert_eq!(p.to_string(), p.as_str());
        }
        assert_eq!(Privilege::VmPower.as_str(), "vm.power");
        assert_eq!(Privilege::parse("vm.teleport"), None);
        assert!(serde_json::from_value::<Privilege>("vm.teleport".into()).is_err());
    }

    #[test]
    fn privset_operations() {
        let a: PrivSet = [Privilege::VmPower, Privilege::VmConsole]
            .into_iter()
            .collect();
        let b: PrivSet = Privilege::VmPower.into();
        assert!(b.is_subset(a));
        assert!(!a.is_subset(b));
        assert_eq!(a & b, b);
        assert_eq!((a | b), a);
        assert_eq!(a.difference(b), Privilege::VmConsole.into());
        assert_eq!(a.names(), vec!["vm.power", "vm.console"]);
        assert!(PrivSet::EMPTY.is_empty());
        assert_eq!(PrivSet::EMPTY.with_implied(), PrivSet::EMPTY);
        assert!(b.with_implied().contains(Privilege::SystemRead));
        assert_eq!(PrivSet::all().iter().count(), Privilege::ALL.len());
        assert_eq!(format!("{b:?}"), r#"{"vm.power"}"#);
    }

    #[test]
    fn privset_json_drops_unknown_names() {
        let set: PrivSet = serde_json::from_value(serde_json::json!([
            "vm.power",
            "vm.teleport",
            "system.read"
        ]))
        .unwrap();
        assert_eq!(
            set,
            [Privilege::SystemRead, Privilege::VmPower]
                .into_iter()
                .collect()
        );
        assert_eq!(
            serde_json::to_value(set).unwrap(),
            serde_json::json!(["system.read", "vm.power"])
        );
        let (set, unknown) = PrivSet::from_names(&["vm.power", "x.y"]);
        assert_eq!(set, Privilege::VmPower.into());
        assert_eq!(unknown, vec!["x.y".to_string()]);
    }

    #[test]
    fn builtin_roles_nest() {
        let admin = builtin_privileges(Role::Admin);
        let op = builtin_privileges(Role::Operator);
        let viewer = builtin_privileges(Role::Viewer);
        assert_eq!(admin, PrivSet::all());
        assert!(viewer.is_subset(op) && op.is_subset(admin));
        assert!(builtin_privileges(Role::None).is_empty());
        // 새 권한은 operator에 없다.
        for p in [
            Privilege::InventoryFolder,
            Privilege::InventoryMove,
            Privilege::InventoryQuota,
            Privilege::PermissionModify,
            Privilege::NetworkConfig,
            Privilege::StorageConfig,
            Privilege::AlertManage,
            Privilege::ClusterAdmin,
            Privilege::VmDedicated,
        ] {
            assert!(!op.contains(p), "{p}");
        }
        assert_eq!(
            viewer.names(),
            vec!["system.read", "alert.read", "cluster.read"]
        );
    }

    /// 지금 `RoleAuthorizer::check`(opl-api, 계획 61 끝)의 규칙을 여기 그대로 옮긴 것. opl-api도 진짜 구현과
    /// 같은 시험을 한다.
    fn old_check(role: Role, action: Action, kind: Kind) -> bool {
        let account = matches!(
            kind,
            Kind::User
                | Kind::ApiToken
                | Kind::JoinToken
                | Kind::Realm
                | Kind::Session
                | Kind::NodeFence
                | Kind::NotificationTarget
                | Kind::Grant
                | Kind::Group
                | Kind::TwoFactor
        );
        match role {
            Role::Admin => true,
            Role::Operator => match action {
                Action::Read => !account,
                Action::Write | Action::Operate => matches!(
                    kind,
                    Kind::Vm | Kind::Volume | Kind::Backup | Kind::BackupJob | Kind::VmImport
                ),
            },
            Role::Viewer => action == Action::Read && !account,
            Role::None => false,
        }
    }

    #[test]
    fn builtin_roles_match_the_kind_rules() {
        for role in Role::ALL {
            for action in Action::ALL {
                for kind in Kind::ALL {
                    assert_eq!(
                        builtin_privileges(role).contains(kind_privilege(action, kind)),
                        old_check(role, action, kind),
                        "{role:?} {action:?} {kind:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn access_role_storage_and_validation() {
        let r = AccessRoleSpec {
            privileges: vec!["vm.power".into(), "vm.console".into()],
            description: String::new(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "privileges": ["vm.power", "vm.console"] })
        );
        assert_eq!(serde_json::from_value::<AccessRoleSpec>(json).unwrap(), r);
        assert_eq!(r.validate(), Ok(()));
        // 뒤 버전이 더한 권한은 읽을 때 버리고, 쓰기는 막는다.
        let newer = AccessRoleSpec {
            privileges: vec!["vm.power".into(), "vm.teleport".into()],
            description: "ops".into(),
        };
        assert_eq!(
            newer.privset(),
            (Privilege::VmPower.into(), vec!["vm.teleport".to_string()])
        );
        assert_eq!(
            newer.validate(),
            Err(AuthzError::UnknownPrivilege("vm.teleport".into()))
        );
        let empty = AccessRoleSpec {
            privileges: vec![],
            description: String::new(),
        };
        assert_eq!(empty.validate(), Err(AuthzError::NoPrivileges));
        let long = AccessRoleSpec {
            description: "x".repeat(257),
            ..r.clone()
        };
        assert_eq!(long.validate(), Err(AuthzError::LongDescription));
        assert_eq!(AccessRoleSpec::KIND.as_str(), "access-role");
    }

    #[test]
    fn role_names_are_reserved() {
        assert_eq!(validate_role_name("vm-operator"), Ok(()));
        for r in ["admin", "operator", "viewer", "none"] {
            assert_eq!(
                validate_role_name(r),
                Err(AuthzError::ReservedRoleName(r.into()))
            );
        }
        assert!(matches!(
            validate_role_name("Bad Name"),
            Err(AuthzError::BadName(_))
        ));
    }

    #[test]
    fn grant_storage_format() {
        let g = GrantSpec {
            principal: PrincipalRef::Group {
                id: Uuid::from_u128(1),
            },
            role: RoleRef::Builtin {
                role: Role::Operator,
            },
            target: GrantTarget::Folder {
                id: Uuid::from_u128(2),
            },
            propagate: true,
        };
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "principal": { "type": "group", "id": Uuid::from_u128(1).to_string() },
                "role": { "type": "builtin", "role": "operator" },
                "target": { "type": "folder", "id": Uuid::from_u128(2).to_string() }
            })
        );
        assert_eq!(serde_json::from_value::<GrantSpec>(json).unwrap(), g);
        let g2 = GrantSpec {
            principal: PrincipalRef::User {
                id: Uuid::from_u128(3),
            },
            role: RoleRef::Custom {
                id: Uuid::from_u128(4),
            },
            target: GrantTarget::Object {
                kind: Kind::Vm,
                id: Uuid::from_u128(5),
            },
            propagate: false,
        };
        let json = serde_json::to_value(&g2).unwrap();
        assert_eq!(json["propagate"], false);
        assert_eq!(json["target"]["kind"], "vm");
        assert_eq!(json["role"]["type"], "custom");
        assert_eq!(serde_json::from_value::<GrantSpec>(json).unwrap(), g2);
        let root: GrantTarget =
            serde_json::from_value(serde_json::json!({"type": "root"})).unwrap();
        assert_eq!(root, GrantTarget::Root);
        assert_eq!(GrantSpec::KIND.as_str(), "grant");
    }

    #[test]
    fn validates_grants() {
        let mut g = GrantSpec {
            principal: PrincipalRef::User {
                id: Uuid::from_u128(1),
            },
            role: RoleRef::Builtin { role: Role::Viewer },
            target: GrantTarget::Root,
            propagate: true,
        };
        assert_eq!(g.validate(), Ok(()));
        g.role = RoleRef::Builtin { role: Role::None };
        assert_eq!(g.validate(), Err(AuthzError::GrantNone));
        g.role = RoleRef::Builtin { role: Role::Admin };
        for kind in [
            Kind::Vm,
            Kind::Network,
            Kind::StoragePool,
            Kind::Router,
            Kind::FloatingIp,
        ] {
            g.target = GrantTarget::Object {
                kind,
                id: Uuid::from_u128(2),
            };
            assert_eq!(g.validate(), Ok(()));
        }
        for kind in [Kind::Volume, Kind::Folder, Kind::Node, Kind::Backup] {
            g.target = GrantTarget::Object {
                kind,
                id: Uuid::from_u128(2),
            };
            assert_eq!(g.validate(), Err(AuthzError::BadObjectKind(kind)));
        }
    }

    #[test]
    fn group_storage_and_validation() {
        let g = GroupSpec {
            members: vec![Uuid::from_u128(1), Uuid::from_u128(1)],
            external: vec![ExternalGroup {
                realm: "corp".into(),
                group: "cn=team-a,dc=corp".into(),
            }],
            description: String::new(),
        };
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "members": [Uuid::from_u128(1).to_string(), Uuid::from_u128(1).to_string()],
                "external": [{ "realm": "corp", "group": "cn=team-a,dc=corp" }]
            })
        );
        assert_eq!(serde_json::from_value::<GroupSpec>(json).unwrap(), g);
        assert_eq!(g.validate(), Ok(()));
        assert_eq!(g.member_set().len(), 1);
        let empty: GroupSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, GroupSpec::default());
        for (realm, group) in [("Corp", "g"), ("corp", "  "), ("corp", &"x".repeat(1025))] {
            let bad = GroupSpec {
                external: vec![ExternalGroup {
                    realm: realm.into(),
                    group: group.to_string(),
                }],
                ..GroupSpec::default()
            };
            assert_eq!(bad.validate(), Err(AuthzError::BadExternalGroup));
        }
        assert_eq!(GroupSpec::KIND.as_str(), "group");
    }

    #[test]
    fn token_scope_validation() {
        let s = TokenScope {
            privileges: vec!["vm.power".into(), "vm.console".into()],
            targets: vec![GrantTarget::Folder {
                id: Uuid::from_u128(1),
            }],
        };
        assert_eq!(s.validate(), Ok(()));
        assert!(s.privset().contains(Privilege::VmConsole));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(serde_json::from_value::<TokenScope>(json).unwrap(), s);
        let no_targets = TokenScope {
            targets: vec![],
            ..s.clone()
        };
        assert_eq!(no_targets.validate(), Err(AuthzError::EmptyScope));
        let no_privs = TokenScope {
            privileges: vec![],
            ..s.clone()
        };
        assert_eq!(no_privs.validate(), Err(AuthzError::EmptyScope));
        let unknown = TokenScope {
            privileges: vec!["vm.teleport".into()],
            ..s
        };
        assert_eq!(
            unknown.validate(),
            Err(AuthzError::UnknownPrivilege("vm.teleport".into()))
        );
    }
}
