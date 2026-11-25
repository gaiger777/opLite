//! grant(`Grant` — 루트, 계획 73 §8.1 "전체가 열쇠"): 매니페스트 spec은 이름으로 적는다 —
//!
//! ```yaml
//! spec: { user: alice, role: operator, target: "vm:/prod" }          # 폴더(트리:경로 — 폴더 문서 열쇠와 같다)
//! spec: { group: ops, role: web-admin, target: "vm/web01" }         # 객체(종류/이름), 사용자 정의 역할
//! spec: { user: bob, role: viewer, target: "/", propagate: false }  # 루트
//! ```
//!
//! grant는 바꾸는 길이 없고(주거나 뺄 뿐) 리소스 이름이 id라서, 문서 열쇠는 이름이 아니라 내용 전체다
//! (`user:alice operator vm:/prod`, 내려가지 않으면 끝에 ` no-propagate`). 내용을 바꾼 문서는 다른 grant다 — 새로 주고,
//! 묶음의 가지치기가 옛 것을 뺀다(데이터가 없어 허용). `metadata.name`은 사람이 붙이는 이름이고 열쇠에 들지 않는다
//! (내보내기는 `grant-` + 열쇠 SHA-256 앞 12자).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::auth::Role;
use orbivisor_types::authz::{GrantSpec, GrantTarget, PrincipalRef, RoleRef};
use orbivisor_types::folder::FolderKind;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use orbivisor_types::resource::Kind;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    Adapter, Done, Fut, Live, Update, World, bad, folder_key, lives_of, roundtrip, typed,
    write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::permissions::{
    CreateGrantRequest, check_delegation, create_grant, delete_grant, role_set,
};
use crate::state::AppState;

pub(crate) struct GrantAdapter;

const BUILTIN: [Role; 3] = [Role::Admin, Role::Operator, Role::Viewer];

/// 객체 대상이 될 수 있는 종류(경로 앞머리 `vm/`·`network/`…)
const OBJECT_KINDS: [Kind; 7] = [
    Kind::Vm,
    Kind::Network,
    Kind::StoragePool,
    Kind::Router,
    Kind::FloatingIp,
    Kind::LoadBalancer,
    Kind::NetworkMigration,
];

fn tree_of(name: &str) -> Option<FolderKind> {
    [FolderKind::Vm, FolderKind::Network, FolderKind::Storage]
        .into_iter()
        .find(|t| super::tree_name(*t) == name)
}

/// 매니페스트의 대상 글
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Root,
    /// 폴더 문서 열쇠(`vm:/prod`)
    Folder(FolderKind, String),
    Object(Kind, String),
}

impl Target {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "/" {
            return Ok(Self::Root);
        }
        if let Some((tree, path)) = s.split_once(':')
            && let Some(tree) = tree_of(tree)
        {
            let path = path.trim_end_matches('/');
            orbivisor_types::manifest::folder_path(path).map_err(|e| e.to_string())?;
            return Ok(Self::Folder(tree, path.to_string()));
        }
        if let Some((kind, name)) = s.split_once('/')
            && let Some(kind) = OBJECT_KINDS.into_iter().find(|k| k.as_str() == kind)
            && !name.is_empty()
        {
            return Ok(Self::Object(kind, name.to_string()));
        }
        Err(format!(
            "target {s:?}: give \"/\", a folder \"vm:/path\" (vm, network or storage) or an object \"vm/name\""
        ))
    }

    fn text(&self) -> String {
        match self {
            Self::Root => "/".into(),
            Self::Folder(tree, path) => folder_key(*tree, path),
            Self::Object(kind, name) => format!("{}/{name}", kind.as_str()),
        }
    }
}

/// 매니페스트 spec을 읽은 것
#[derive(Debug, Clone, PartialEq, Eq)]
struct Doc {
    /// (그룹인지, 이름)
    who: (bool, String),
    /// 내장 역할 이름 또는 사용자 정의 역할 이름
    role: String,
    target: Target,
    propagate: bool,
}

fn parse(spec: &Value) -> Result<Doc, String> {
    let m = spec
        .as_object()
        .ok_or("a Grant spec is an object: { user|group, role, target, propagate? }")?;
    for k in m.keys() {
        if !["user", "group", "role", "target", "propagate"].contains(&k.as_str()) {
            return Err(format!(
                "unknown field `{k}` (user, group, role, target, propagate)"
            ));
        }
    }
    let text = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
    let who = match (text("user"), text("group")) {
        (Some(u), None) => (false, u),
        (None, Some(g)) => (true, g),
        _ => return Err("give exactly one of `user` and `group`".into()),
    };
    let role = text("role").ok_or("missing field `role`")?;
    let target = Target::parse(&text("target").ok_or("missing field `target`")?)?;
    let propagate = match m.get("propagate") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("propagate is true or false".into()),
    };
    Ok(Doc {
        who,
        role,
        target,
        propagate,
    })
}

fn key(d: &Doc) -> String {
    format!(
        "{}:{} {} {}{}",
        if d.who.0 { "group" } else { "user" },
        d.who.1,
        d.role,
        d.target.text(),
        if d.propagate { "" } else { " no-propagate" }
    )
}

fn builtin(name: &str) -> Option<Role> {
    BUILTIN.into_iter().find(|r| r.as_str() == name)
}

/// 같은 계획에서 먼저 만들 것(적용 때 다시 푼다)
fn id_or_nil(id: Option<Uuid>) -> Uuid {
    id.unwrap_or_else(Uuid::nil)
}

impl GrantAdapter {
    /// 저장 spec → 매니페스트 spec(id → 이름)
    fn manifest(spec: &GrantSpec, world: &World) -> Value {
        let mut out = Map::new();
        match spec.principal {
            PrincipalRef::User { id } => out.insert("user".into(), json!(world.user_name(id))),
            PrincipalRef::Group { id } => out.insert(
                "group".into(),
                json!(world.name_of(ManifestKind::Group, id)),
            ),
        };
        let role = match spec.role {
            RoleRef::Builtin { role } => role.as_str().to_string(),
            RoleRef::Custom { id } => world.name_of(ManifestKind::AccessRole, id),
        };
        out.insert("role".into(), json!(role));
        let target = match spec.target {
            GrantTarget::Root => Target::Root,
            GrantTarget::Folder { id } => match (world.folder_tree(id), world.folder_path(id)) {
                (Some(tree), Some(path)) => Target::Folder(tree, path),
                _ => Target::Object(Kind::Folder, id.to_string()),
            },
            GrantTarget::Object { kind, id } => {
                let name = match ManifestKind::from_kind(kind) {
                    Some(ManifestKind::Vm) => world.vm_name(id),
                    Some(mk) => world.name_of(mk, id),
                    None => id.to_string(),
                };
                Target::Object(kind, name)
            }
        };
        out.insert("target".into(), json!(target.text()));
        if !spec.propagate {
            out.insert("propagate".into(), json!(false));
        }
        Value::Object(out)
    }

    fn store(d: &Doc, world: &World) -> Result<GrantSpec, String> {
        let principal = if d.who.0 {
            PrincipalRef::Group {
                id: id_or_nil(world.id_of(ManifestKind::Group, &d.who.1)),
            }
        } else {
            PrincipalRef::User {
                id: world
                    .user_id(&d.who.1)
                    .ok_or_else(|| format!("user {:?} not found", d.who.1))?,
            }
        };
        let role = match builtin(&d.role) {
            Some(role) => RoleRef::Builtin { role },
            None => RoleRef::Custom {
                id: id_or_nil(world.id_of(ManifestKind::AccessRole, &d.role)),
            },
        };
        let target = match &d.target {
            Target::Root => GrantTarget::Root,
            Target::Folder(tree, path) => GrantTarget::Folder {
                id: id_or_nil(world.folder_id(*tree, path)),
            },
            Target::Object(kind, name) => {
                let id = match ManifestKind::from_kind(*kind) {
                    Some(ManifestKind::Vm) => world.vm_id(name),
                    Some(mk) => world.id_of(mk, name),
                    None => name.parse().ok(),
                };
                GrantTarget::Object {
                    kind: *kind,
                    id: id_or_nil(id),
                }
            }
        };
        Ok(GrantSpec {
            principal,
            role,
            target,
            propagate: d.propagate,
        })
    }

    /// 위임 규칙(처리기와 같다 — 대상의 `permission.modify`, 자기 권한 안). 같은 계획이 먼저 만들 대상·역할이면 건너뛴다
    /// (적용 때 처리기가 본다).
    fn delegation(
        state: &AppState,
        principal: &Principal,
        spec: &GrantSpec,
    ) -> Result<(), ApiError> {
        let planned = match spec.target {
            GrantTarget::Root => false,
            GrantTarget::Folder { id } | GrantTarget::Object { id, .. } => id.is_nil(),
        } || matches!(spec.role, RoleRef::Custom { id } if id.is_nil());
        if planned {
            return Ok(());
        }
        let (privileges, _) = role_set(state, &spec.role)?;
        check_delegation(state, principal, &spec.target, spec.propagate, privileges)
    }
}

impl Adapter for GrantAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<GrantSpec>(cluster, ManifestKind::Grant)
    }

    fn to_manifest(&self, spec: &Value, world: &World) -> Value {
        match typed::<GrantSpec>(spec) {
            Ok(g) => Self::manifest(&g, world),
            Err(_) => spec.clone(),
        }
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        let d = parse(spec)?;
        Ok(serde_json::to_value(Self::store(&d, world)?).expect("a grant serializes"))
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<GrantSpec>(store)
    }

    fn doc_key(&self, spec: &Value) -> Result<Option<String>, String> {
        parse(spec).map(|d| Some(key(&d)))
    }

    fn export_name(&self, _live: &Live, spec: &Value) -> String {
        let k = parse(spec).map(|d| key(&d)).unwrap_or_default();
        format!("grant-{}", &hex::encode(Sha256::digest(k.as_bytes()))[..12])
    }

    /// 바꾸는 길이 없다(열쇠가 내용 전체라 같은 열쇠의 spec은 같다 — 레이블만 쓴다)
    fn updatable(&self) -> Update {
        Update::Fields(&[])
    }

    fn create_carries_labels(&self) -> bool {
        false
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let Ok(d) = parse(spec) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if d.who.0 {
            out.push((ManifestKind::Group, d.who.1.clone()));
        }
        if builtin(&d.role).is_none() {
            out.push((ManifestKind::AccessRole, d.role.clone()));
        }
        match &d.target {
            Target::Root => {}
            Target::Folder(tree, path) => out.push((ManifestKind::Folder, folder_key(*tree, path))),
            Target::Object(kind, name) => {
                if let Some(mk) = ManifestKind::from_kind(*kind) {
                    out.push((mk, name.clone()));
                }
            }
        }
        out
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let spec: GrantSpec = typed(desired).map_err(bad)?;
        Self::delegation(state, principal, &spec)
    }

    /// 레이블만 바꿀 때도 빼기와 같은 규칙(경로표는 `Handler`)
    fn update_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/grants/{id}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/grants/{id}")
    }

    fn may_update_extra(
        &self,
        state: &AppState,
        principal: &Principal,
        live: &Live,
    ) -> Result<(), ApiError> {
        let spec: GrantSpec = typed(&live.spec).map_err(bad)?;
        Self::delegation(state, principal, &spec)
    }

    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        _name: String,
        _labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: GrantSpec = typed(&spec).map_err(bad)?;
            let (_, Json(grant)) = Box::pin(create_grant(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateGrantRequest {
                    principal: spec.principal,
                    role: spec.role,
                    target: spec.target,
                    propagate: spec.propagate,
                }),
            ))
            .await?;
            Ok(Done {
                id: grant.id,
                task: None,
            })
        })
    }

    fn update<'a>(
        &'a self,
        _state: &'a Arc<AppState>,
        _principal: &'a Principal,
        live: &'a Live,
        _labels: BTreeMap<String, String>,
        _desired: Value,
        _fields: Vec<String>,
    ) -> Fut<'a, Done> {
        // 계획이 spec 바뀜을 `immutable`로 막으므로 오지 않는다(레이블은 `write_labels`).
        Box::pin(async move {
            Err(ApiError::bad_request(format!(
                "grant {} cannot change; give a new grant and remove the old one",
                live.id
            )))
        })
    }

    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            Box::pin(delete_grant(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Path(live.id.to_string()),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<GrantSpec>(cluster, id, revision, labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_documents_key_on_their_whole_content() {
        let a =
            parse(&json!({ "user": "alice", "role": "operator", "target": "vm:/prod/" })).unwrap();
        assert_eq!(key(&a), "user:alice operator vm:/prod");
        let b = parse(
            &json!({ "group": "ops", "role": "web", "target": "vm/web01", "propagate": false }),
        )
        .unwrap();
        assert_eq!(key(&b), "group:ops web vm/web01 no-propagate");
        assert_eq!(
            parse(&json!({ "user": "a", "role": "viewer", "target": "/" }))
                .unwrap()
                .target,
            Target::Root
        );
        assert_eq!(
            parse(&json!({ "user": "a", "role": "viewer", "target": "storage-pool/fast" }))
                .unwrap()
                .target,
            Target::Object(Kind::StoragePool, "fast".into())
        );
        for bad in [
            json!({ "user": "a", "group": "b", "role": "viewer", "target": "/" }),
            json!({ "user": "a", "role": "viewer", "target": "disk/x" }),
            json!({ "user": "a", "role": "viewer", "target": "/", "extra": 1 }),
            json!({ "user": "a", "target": "/" }),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn refs_point_at_groups_roles_folders_and_objects() {
        let refs =
            GrantAdapter.refs(&json!({ "group": "ops", "role": "web", "target": "network:/dmz" }));
        assert_eq!(
            refs,
            vec![
                (ManifestKind::Group, "ops".into()),
                (ManifestKind::AccessRole, "web".into()),
                (ManifestKind::Folder, "network:/dmz".into()),
            ]
        );
        assert!(
            GrantAdapter
                .refs(&json!({ "user": "a", "role": "admin", "target": "/" }))
                .is_empty()
        );
    }
}
