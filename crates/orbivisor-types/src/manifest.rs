//! 선언형 매니페스트(계획 73 §6·§9): `orbivisorctl apply`·Terraform 공급자·내보내기가 주고받는 꼴. 저장하지 않는다 —
//! 서버가 지금 상태와 견주어 계획을 세운다. YAML은 CLI만 읽고 서버는 이 꼴의 JSON만 받는다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::resource::{Kind, validate_name};
use crate::settings::SETTINGS_NAME;

/// 매니페스트 판
pub const API_VERSION: &str = "orbivisor.io/v1";
/// 적용 묶음에 든 리소스에 붙이는 레이블(값은 묶음 이름)
pub const APPLY_SET_LABEL: &str = "orbivisor.io/apply-set";
/// 리소스를 관리하는 도구(`terraform`)를 적는 레이블
pub const MANAGED_BY_LABEL: &str = "orbivisor.io/managed-by";
/// 값이 `"false"`면 가지치기에서 건너뛰는 주석
pub const PRUNE_ANNOTATION: &str = "orbivisor.io/prune";
/// 한 요청의 문서 수 한도
pub const MAX_DOCUMENTS: usize = 1000;
/// 폴더 경로 한 칸의 최대 깊이(폴더 트리 한도와 같다)
const MAX_FOLDER_DEPTH: usize = 16;

/// 매니페스트로 다루는 종류(스물다섯). `Kind`와 [`ManifestKind::kind`]로 잇는다. 노드·Task·스냅샷·백업·가져오기·이전·
/// 내보내기·SDN 설치·Ceph·사용자 비밀번호·토큰·2FA는 없다(§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ManifestKind {
    Vm,
    Volume,
    Network,
    Router,
    FloatingIp,
    SecurityGroup,
    LoadBalancer,
    StoragePool,
    Folder,
    PlacementRule,
    HaGroup,
    HaCheck,
    BackupJob,
    BackupStore,
    Remote,
    Grant,
    AccessRole,
    Group,
    AlertRule,
    NotificationTarget,
    Realm,
    Settings,
    Certificate,
    AcmeAccount,
    AcmeDns,
}

impl ManifestKind {
    pub const ALL: [ManifestKind; 25] = [
        Self::Vm,
        Self::Volume,
        Self::Network,
        Self::Router,
        Self::FloatingIp,
        Self::SecurityGroup,
        Self::LoadBalancer,
        Self::StoragePool,
        Self::Folder,
        Self::PlacementRule,
        Self::HaGroup,
        Self::HaCheck,
        Self::BackupJob,
        Self::BackupStore,
        Self::Remote,
        Self::Grant,
        Self::AccessRole,
        Self::Group,
        Self::AlertRule,
        Self::NotificationTarget,
        Self::Realm,
        Self::Settings,
        Self::Certificate,
        Self::AcmeAccount,
        Self::AcmeDns,
    ];

    /// 저장소 종류
    pub fn kind(self) -> Kind {
        match self {
            Self::Vm => Kind::Vm,
            Self::Volume => Kind::Volume,
            Self::Network => Kind::Network,
            Self::Router => Kind::Router,
            Self::FloatingIp => Kind::FloatingIp,
            Self::SecurityGroup => Kind::SecurityGroup,
            Self::LoadBalancer => Kind::LoadBalancer,
            Self::StoragePool => Kind::StoragePool,
            Self::Folder => Kind::Folder,
            Self::PlacementRule => Kind::PlacementRule,
            Self::HaGroup => Kind::HaGroup,
            Self::HaCheck => Kind::HaCheck,
            Self::BackupJob => Kind::BackupJob,
            Self::BackupStore => Kind::BackupStore,
            Self::Remote => Kind::Remote,
            Self::Grant => Kind::Grant,
            Self::AccessRole => Kind::AccessRole,
            Self::Group => Kind::Group,
            Self::AlertRule => Kind::AlertRule,
            Self::NotificationTarget => Kind::NotificationTarget,
            Self::Realm => Kind::Realm,
            Self::Settings => Kind::Settings,
            Self::Certificate => Kind::Certificate,
            Self::AcmeAccount => Kind::AcmeAccount,
            Self::AcmeDns => Kind::AcmeDns,
        }
    }

    /// 저장소 종류의 매니페스트 종류(매니페스트로 다루지 않으면 `None`)
    pub fn from_kind(kind: Kind) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.kind() == kind)
    }

    /// `kind` 칸의 값(`Vm`·`FloatingIp`…)
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vm => "Vm",
            Self::Volume => "Volume",
            Self::Network => "Network",
            Self::Router => "Router",
            Self::FloatingIp => "FloatingIp",
            Self::SecurityGroup => "SecurityGroup",
            Self::LoadBalancer => "LoadBalancer",
            Self::StoragePool => "StoragePool",
            Self::Folder => "Folder",
            Self::PlacementRule => "PlacementRule",
            Self::HaGroup => "HaGroup",
            Self::HaCheck => "HaCheck",
            Self::BackupJob => "BackupJob",
            Self::BackupStore => "BackupStore",
            Self::Remote => "Remote",
            Self::Grant => "Grant",
            Self::AccessRole => "AccessRole",
            Self::Group => "Group",
            Self::AlertRule => "AlertRule",
            Self::NotificationTarget => "NotificationTarget",
            Self::Realm => "Realm",
            Self::Settings => "Settings",
            Self::Certificate => "Certificate",
            Self::AcmeAccount => "AcmeAccount",
            Self::AcmeDns => "AcmeDns",
        }
    }

    /// 데이터를 든 종류(가지치기는 `--prune-data` 없이 지우지 않는다)
    pub fn holds_data(self) -> bool {
        matches!(self, Self::Vm | Self::Volume | Self::BackupStore)
    }
}

/// VM의 원하는 전원 상태(`state` — VM만)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    Running,
    Stopped,
}

/// 매니페스트의 이름표
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ManifestMetadata {
    pub name: String,
    /// 폴더 경로(`/prod/web`). 없으면 뿌리. 폴더를 따르는 종류만 쓴다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// 서버가 저장하지 않는 표시(`orbivisor.io/prune: "false"` 등)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

/// 매니페스트 문서 하나. `spec`은 그 종류의 만들기 본문 spec과 같은 꼴이고 참조는 id가 아니라 이름이다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: ManifestKind,
    pub metadata: ManifestMetadata,
    #[serde(default)]
    pub spec: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub state: Option<ManifestState>,
}

/// 매니페스트 꼴이 틀림(문서 번호와 칸 경로는 부른 쪽이 붙인다)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("apiVersion must be {API_VERSION:?}, not {0:?}")]
    ApiVersion(String),
    #[error("metadata.name: {0}")]
    Name(String),
    #[error("the Settings document must be named {SETTINGS_NAME:?}")]
    SettingsName,
    #[error("metadata.folder {0:?} must be an absolute path of folder names like /prod/web")]
    Folder(String),
    #[error("state is only for Vm documents")]
    State,
    #[error("spec must be an object")]
    Spec,
}

impl Manifest {
    pub fn new(kind: ManifestKind, name: impl Into<String>, spec: Value) -> Self {
        Self {
            api_version: API_VERSION.into(),
            kind,
            metadata: ManifestMetadata {
                name: name.into(),
                ..ManifestMetadata::default()
            },
            spec,
            state: None,
        }
    }

    /// 종류에 매이지 않는 검사(판·이름·폴더 경로·`state`·spec 꼴). 종류의 spec 꼴은 서버 어댑터가 본다.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.api_version != API_VERSION {
            return Err(ManifestError::ApiVersion(self.api_version.clone()));
        }
        if self.kind == ManifestKind::Settings {
            if self.metadata.name != SETTINGS_NAME {
                return Err(ManifestError::SettingsName);
            }
        } else {
            validate_name(&self.metadata.name).map_err(|e| ManifestError::Name(e.to_string()))?;
        }
        if let Some(folder) = &self.metadata.folder {
            folder_path(folder)?;
        }
        if self.state.is_some() && self.kind != ManifestKind::Vm {
            return Err(ManifestError::State);
        }
        if !(self.spec.is_object() || self.spec.is_null()) {
            return Err(ManifestError::Spec);
        }
        Ok(())
    }

    /// 가지치기에서 건너뛰라는 주석이 있는지
    pub fn keeps_on_prune(&self) -> bool {
        self.metadata
            .annotations
            .get(PRUNE_ANNOTATION)
            .is_some_and(|v| v == "false")
    }
}

/// 폴더 경로(`/prod/web`)를 폴더 이름들로 나눈다. `/`는 뿌리(빈 목록)다.
pub fn folder_path(path: &str) -> Result<Vec<&str>, ManifestError> {
    let bad = || ManifestError::Folder(path.to_string());
    let rest = path.strip_prefix('/').ok_or_else(bad)?;
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() > MAX_FOLDER_DEPTH || parts.iter().any(|p| validate_name(p).is_err()) {
        return Err(bad());
    }
    Ok(parts)
}

/// 비밀 칸의 값: 평문 또는 "그대로 둠" 참조(`secretRef: "<종류>/<이름>/<칸>"` — 내보낸 파일을 다시 적용해도 비밀이
/// 바뀌지 않는다). 서버는 평문만 쓴다. CLI는 `secretFrom: {env|file}`을 보내기 전에 평문으로 푼다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(untagged)]
pub enum SecretValue {
    Plain(String),
    Ref {
        #[serde(rename = "secretRef")]
        secret_ref: String,
    },
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain(_) => f.write_str("Plain(<redacted>)"),
            Self::Ref { secret_ref } => f
                .debug_struct("Ref")
                .field("secret_ref", secret_ref)
                .finish(),
        }
    }
}

impl SecretValue {
    /// 내보내기가 쓰는 참조
    pub fn reference(kind: ManifestKind, name: &str, field: &str) -> Self {
        Self::Ref {
            secret_ref: format!("{}/{name}/{field}", kind.as_str()),
        }
    }
}

/// 비밀 칸(`.`로 잇는 spec 안 경로). 서버 어댑터의 비밀 칸과 같다(orbivisor-api 시험이 본다).
pub fn secret_fields(kind: ManifestKind) -> &'static [&'static str] {
    match kind {
        ManifestKind::Vm => &["cloud_init.password"],
        ManifestKind::BackupStore => &["encryption.key", "backend.s3.secret_access_key"],
        ManifestKind::Remote => &["token"],
        ManifestKind::NotificationTarget => &["target.password", "target.token"],
        ManifestKind::Realm => &["backend.bind_password", "backend.client_secret"],
        ManifestKind::AcmeAccount => &["eab.hmac_key"],
        ManifestKind::AcmeDns => &[
            "provider.api_token",
            "provider.secret_access_key",
            "provider.secret",
            "provider.api_key",
        ],
        _ => &[],
    }
}

/// CLI `secretFrom`을 받는 칸: 비밀 칸과 보기가 가리는 칸(webhook URL·헤더 값 — `*`는 아무 이름). 그 밖의 칸(설명·레이블·
/// user_data)은 평문으로 저장되고 보이므로 받지 않는다(저장소의 매니페스트가 운영자 비밀을 끌어오지 못하게 — 계획 73 리뷰 1).
pub fn secret_from_allowed(kind: ManifestKind, path: &[&str]) -> bool {
    let matches = |pattern: &str| {
        let segs: Vec<&str> = pattern.split('.').collect();
        segs.len() == path.len() && segs.iter().zip(path).all(|(s, p)| *s == "*" || s == p)
    };
    secret_fields(kind).iter().any(|f| matches(f))
        || (kind == ManifestKind::NotificationTarget
            && (matches("target.url") || matches("target.headers.*")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn secret_from_is_only_for_secret_fields() {
        assert!(secret_from_allowed(
            ManifestKind::Vm,
            &["cloud_init", "password"]
        ));
        assert!(!secret_from_allowed(
            ManifestKind::Vm,
            &["cloud_init", "user_data"]
        ));
        assert!(!secret_from_allowed(ManifestKind::Vm, &["description"]));
        assert!(!secret_from_allowed(ManifestKind::Vm, &["cloud_init"]));
        let t = ManifestKind::NotificationTarget;
        assert!(secret_from_allowed(t, &["target", "token"]));
        assert!(secret_from_allowed(t, &["target", "url"]));
        assert!(secret_from_allowed(
            t,
            &["target", "headers", "Authorization"]
        ));
        assert!(!secret_from_allowed(t, &["target", "headers"]));
        assert!(!secret_from_allowed(ManifestKind::Folder, &["description"]));
    }

    #[test]
    fn manifest_kinds_map_to_store_kinds() {
        let kinds: std::collections::BTreeSet<Kind> = ManifestKind::ALL
            .into_iter()
            .map(ManifestKind::kind)
            .collect();
        assert_eq!(kinds.len(), 25);
        for m in ManifestKind::ALL {
            assert_eq!(ManifestKind::from_kind(m.kind()), Some(m));
            assert_eq!(serde_json::to_value(m).unwrap(), m.as_str());
            // 매니페스트 종류는 모두 이력 종류다(되돌리기가 매니페스트로 간다).
            assert!(m.kind().history_tracked(), "{m:?}");
        }
        for k in [
            Kind::Node,
            Kind::Task,
            Kind::User,
            Kind::ApplySet,
            Kind::Snapshot,
        ] {
            assert_eq!(ManifestKind::from_kind(k), None, "{k:?}");
        }
        assert!(ManifestKind::Vm.holds_data() && !ManifestKind::Network.holds_data());
    }

    #[test]
    fn manifest_round_trips_and_rejects_unknown_fields() {
        let doc = json!({
            "apiVersion": "orbivisor.io/v1",
            "kind": "Vm",
            "metadata": { "name": "web01", "folder": "/prod/web", "labels": { "tier": "web" } },
            "state": "running",
            "spec": { "vcpus": 4 }
        });
        let m: Manifest = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(m.kind, ManifestKind::Vm);
        assert_eq!(m.state, Some(ManifestState::Running));
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(serde_json::to_value(&m).unwrap(), doc);
        let mut bad = doc.clone();
        bad["metadata"]["nmae"] = json!("x");
        assert!(serde_json::from_value::<Manifest>(bad).is_err());
        let mut bad = doc;
        bad["kind"] = json!("Node");
        assert!(serde_json::from_value::<Manifest>(bad).is_err());
    }

    #[test]
    fn validates_common_fields() {
        let mut m = Manifest::new(ManifestKind::Network, "web-net", json!({}));
        assert_eq!(m.validate(), Ok(()));
        m.api_version = "v2".into();
        assert!(matches!(m.validate(), Err(ManifestError::ApiVersion(_))));
        m.api_version = API_VERSION.into();
        m.state = Some(ManifestState::Stopped);
        assert_eq!(m.validate(), Err(ManifestError::State));
        m.state = None;
        m.metadata.folder = Some("prod".into());
        assert!(matches!(m.validate(), Err(ManifestError::Folder(_))));
        m.metadata.folder = Some("/prod/Web".into());
        assert!(matches!(m.validate(), Err(ManifestError::Folder(_))));
        m.metadata.name = "Bad".into();
        assert!(matches!(m.validate(), Err(ManifestError::Name(_))));
        let s = Manifest::new(ManifestKind::Settings, "other", json!({}));
        assert_eq!(s.validate(), Err(ManifestError::SettingsName));
        assert_eq!(
            Manifest::new(ManifestKind::Settings, "cluster", json!({})).validate(),
            Ok(())
        );
        assert_eq!(
            Manifest::new(ManifestKind::Folder, "a", json!([1])).validate(),
            Err(ManifestError::Spec)
        );
        assert_eq!(folder_path("/").unwrap(), Vec::<&str>::new());
        assert_eq!(folder_path("/prod/web").unwrap(), vec!["prod", "web"]);
        assert!(folder_path("/prod//web").is_err());
        assert!(folder_path(&"/a".repeat(17)).is_err());
    }

    #[test]
    fn prune_annotation_and_secret_values() {
        let mut m = Manifest::new(ManifestKind::Volume, "v", json!({}));
        assert!(!m.keeps_on_prune());
        m.metadata
            .annotations
            .insert(PRUNE_ANNOTATION.into(), "false".into());
        assert!(m.keeps_on_prune());

        let plain: SecretValue = serde_json::from_value(json!("hunter2")).unwrap();
        assert_eq!(plain, SecretValue::Plain("hunter2".into()));
        assert!(!format!("{plain:?}").contains("hunter2"));
        let r = SecretValue::reference(ManifestKind::BackupStore, "s3", "secret_access_key");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            json!({ "secretRef": "BackupStore/s3/secret_access_key" })
        );
        assert_eq!(serde_json::from_value::<SecretValue>(v).unwrap(), r);
    }
}
