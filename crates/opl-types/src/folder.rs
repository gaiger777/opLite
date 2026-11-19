//! 인벤토리 폴더(계획 62). 종류마다(VM·네트워크·스토리지 풀) 따로 중첩 트리를 이루고, 권한(grant)이 아래로
//! 전파되며, VM 폴더에는 할당량을 건다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

/// 폴더. 리소스 이름은 폴더 id 문자열이다(이름 표는 종류 전체에서 하나라서, 형제마다 같은 화면 이름 `prod`를
/// 쓰려고 화면 이름을 스펙의 `name`에 둔다).
pub type Folder = Resource<FolderSpec, FolderStatus>;

/// 루트에서 가장 깊은 폴더까지의 단계 수 상한(루트 바로 아래가 1).
pub const MAX_FOLDER_DEPTH: usize = 16;
/// 폴더 화면 이름의 최대 길이(문자 수)
pub const MAX_FOLDER_NAME: usize = 64;
/// 클러스터 전체 폴더 수 상한(세 트리 합). 색인 다시 만들기가 폴더 수 × 깊이라서 둔다(리뷰 2 N6).
pub const MAX_FOLDERS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FolderSpec {
    /// 화면 이름. 형제 사이에서 대소문자 무시로 겹치지 않는다(상태 머신이 막는다).
    pub name: String,
    /// 이 폴더가 속한 트리
    pub kind: FolderKind,
    /// 부모 폴더. 없으면 그 종류의 루트 바로 아래다. 부모는 같은 종류여야 한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub parent: Option<Uuid>,
    /// 하위 트리 할당량. VM 폴더에만 둔다. 없으면 상한이 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub quota: Option<FolderQuota>,
}

impl ResourceSpec for FolderSpec {
    const KIND: Kind = Kind::Folder;
    type Status = FolderStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct FolderStatus {}

/// 폴더 트리의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum FolderKind {
    /// VM과 템플릿
    Vm,
    Network,
    /// 스토리지 풀
    Storage,
}

impl FolderKind {
    pub const ALL: [FolderKind; 3] = [FolderKind::Vm, FolderKind::Network, FolderKind::Storage];

    pub fn as_str(self) -> &'static str {
        match self {
            FolderKind::Vm => "vm",
            FolderKind::Network => "network",
            FolderKind::Storage => "storage",
        }
    }

    /// 이 종류의 리소스가 사는 트리. 폴더에 들지 않는 종류는 None이다.
    pub fn of(kind: Kind) -> Option<FolderKind> {
        match kind {
            Kind::Vm => Some(FolderKind::Vm),
            Kind::Network => Some(FolderKind::Network),
            Kind::StoragePool => Some(FolderKind::Storage),
            _ => None,
        }
    }

    /// 이 트리에 드는 리소스 종류
    pub fn member_kind(self) -> Kind {
        match self {
            FolderKind::Vm => Kind::Vm,
            FolderKind::Network => Kind::Network,
            FolderKind::Storage => Kind::StoragePool,
        }
    }
}

/// 폴더 할당량. 하위 트리 전체의 합에 건다. 칸마다 없으면 그 차원은 상한이 없다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct FolderQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vcpus: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub memory_mib: Option<u64>,
    /// 붙은 볼륨의 가상 크기 합(바이트)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub disk_bytes: Option<u64>,
    /// 템플릿 아닌 VM 수
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub vms: Option<u64>,
    /// 전용 VM의 vCPU 자리(핫플러그 상한 + 에뮬레이터 `isolate` 1, 계획 64)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub dedicated_vcpus: Option<u64>,
}

impl FolderQuota {
    /// 어느 차원에도 상한이 없다.
    pub fn is_empty(&self) -> bool {
        *self == FolderQuota::default()
    }
}

/// 폴더 하위 트리의 사용량(할당 기준, 전원과 무관). 할당량과 같은 칸이다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FolderUsage {
    pub vcpus: u64,
    pub memory_mib: u64,
    pub disk_bytes: u64,
    pub vms: u64,
    /// 전용 VM의 vCPU 자리(계획 64). 0이면 쓰지 않아 지금 응답 그대로다.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub dedicated_vcpus: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FolderError {
    #[error(
        "invalid folder name {0:?}: use 1-64 characters without '/', control characters or surrounding spaces"
    )]
    BadName(String),
    #[error("a quota can only be set on a vm folder")]
    QuotaOnNonVm,
    #[error("a folder cannot be its own parent")]
    OwnParent,
}

impl FolderSpec {
    /// 스펙만으로 보는 검사. 부모가 있는지·종류가 같은지·순환·깊이는 상태 머신이 본다.
    pub fn validate(&self, id: Uuid) -> Result<(), FolderError> {
        validate_folder_name(&self.name)?;
        if self.quota.is_some() && self.kind != FolderKind::Vm {
            return Err(FolderError::QuotaOnNonVm);
        }
        if self.parent == Some(id) {
            return Err(FolderError::OwnParent);
        }
        Ok(())
    }

    /// 형제 이름 중복을 막는 키 `"{부모 id 또는 root}/{소문자 이름}"`. 종류마다 따로 센다.
    pub fn sibling_key(&self) -> String {
        let parent = self
            .parent
            .map_or_else(|| format!("root-{}", self.kind.as_str()), |p| p.to_string());
        format!("{parent}/{}", self.name.to_lowercase())
    }
}

/// 폴더 화면 이름 규칙: 1~64자, `/`(경로 구분자)와 제어 문자가 없고 앞뒤 공백이 없다.
pub fn validate_folder_name(name: &str) -> Result<(), FolderError> {
    let ok = (1..=MAX_FOLDER_NAME).contains(&name.chars().count())
        && name.trim() == name
        && !name.chars().any(|c| c == '/' || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(FolderError::BadName(name.to_string()))
    }
}

/// 폴더 경로(`/teamA/prod`)를 이름 조각으로 나눈다. 루트는 `/`이고 빈 목록이다. 앞 `/`는 있어도 없어도 되고 끝
/// `/`는 무시한다. 조각마다 이름 규칙을 본다.
pub fn parse_folder_path(path: &str) -> Result<Vec<&str>, FolderError> {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let trimmed = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    for p in &parts {
        validate_folder_name(p)?;
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, kind: FolderKind) -> FolderSpec {
        FolderSpec {
            name: name.into(),
            kind,
            parent: None,
            quota: None,
        }
    }

    #[test]
    fn folder_storage_format() {
        let parent = Uuid::from_u128(7);
        let f = FolderSpec {
            parent: Some(parent),
            quota: Some(FolderQuota {
                vcpus: Some(32),
                vms: Some(20),
                ..FolderQuota::default()
            }),
            ..spec("prod", FolderKind::Vm)
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "name": "prod", "kind": "vm", "parent": parent.to_string(),
                                "quota": { "vcpus": 32, "vms": 20 } })
        );
        assert_eq!(serde_json::from_value::<FolderSpec>(json).unwrap(), f);
        // 루트 바로 아래, 할당량 없음
        let json = serde_json::to_value(spec("Team A", FolderKind::Storage)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "name": "Team A", "kind": "storage" })
        );
        assert!(
            serde_json::from_value::<FolderSpec>(
                serde_json::json!({ "name": "a", "kind": "vm", "x": 1 })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<FolderSpec>(
                serde_json::json!({ "name": "a", "kind": "volume" })
            )
            .is_err()
        );
        assert_eq!(FolderSpec::KIND, Kind::Folder);
        assert_eq!(serde_json::to_value(Kind::Folder).unwrap(), "folder");
    }

    #[test]
    fn validates_folder_specs() {
        let id = Uuid::from_u128(1);
        for ok in ["prod", "Team A", "팀-가", "a", &"x".repeat(64)] {
            assert_eq!(spec(ok, FolderKind::Vm).validate(id), Ok(()), "{ok}");
        }
        for bad in ["", " a", "a ", "a/b", "/", "a\nb", &"x".repeat(65)] {
            assert!(
                matches!(
                    spec(bad, FolderKind::Vm).validate(id),
                    Err(FolderError::BadName(_))
                ),
                "{bad:?}"
            );
        }
        let quota = Some(FolderQuota {
            vms: Some(1),
            ..FolderQuota::default()
        });
        for kind in [FolderKind::Network, FolderKind::Storage] {
            let f = FolderSpec {
                quota,
                ..spec("a", kind)
            };
            assert_eq!(f.validate(id), Err(FolderError::QuotaOnNonVm));
        }
        let f = FolderSpec {
            parent: Some(id),
            ..spec("a", FolderKind::Vm)
        };
        assert_eq!(f.validate(id), Err(FolderError::OwnParent));
    }

    #[test]
    fn sibling_key_ignores_case_and_separates_trees() {
        let p = Uuid::from_u128(9);
        let a = FolderSpec {
            parent: Some(p),
            ..spec("Prod", FolderKind::Vm)
        };
        assert_eq!(a.sibling_key(), format!("{p}/prod"));
        assert_eq!(spec("Prod", FolderKind::Vm).sibling_key(), "root-vm/prod");
        assert_ne!(
            spec("prod", FolderKind::Vm).sibling_key(),
            spec("prod", FolderKind::Network).sibling_key()
        );
    }

    #[test]
    fn folder_kind_maps_resource_kinds() {
        for fk in FolderKind::ALL {
            assert_eq!(FolderKind::of(fk.member_kind()), Some(fk));
            assert_eq!(serde_json::to_value(fk).unwrap(), fk.as_str());
        }
        assert_eq!(FolderKind::of(Kind::Volume), None);
        assert_eq!(FolderKind::of(Kind::Folder), None);
    }

    #[test]
    fn parses_folder_paths() {
        assert_eq!(parse_folder_path("/").unwrap(), Vec::<&str>::new());
        assert_eq!(parse_folder_path("").unwrap(), Vec::<&str>::new());
        assert_eq!(
            parse_folder_path("/teamA/prod").unwrap(),
            vec!["teamA", "prod"]
        );
        assert_eq!(parse_folder_path("teamA/").unwrap(), vec!["teamA"]);
        for bad in ["//a", "/a//b", "/ a", "/a /b"] {
            assert!(parse_folder_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_quota() {
        assert!(FolderQuota::default().is_empty());
        let q: FolderQuota = serde_json::from_str("{}").unwrap();
        assert!(q.is_empty());
        assert_eq!(serde_json::to_string(&q).unwrap(), "{}");
        assert!(serde_json::from_str::<FolderQuota>(r#"{"cpus":1}"#).is_err());
    }
}
