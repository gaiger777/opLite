//! 매니페스트 내보내기(계획 73 §8.4): 지금 클러스터를 같은 매니페스트로 — id 대신 이름·폴더 경로, 비밀은 `secretRef`,
//! status는 뺀다. 차례는 적용 차례이고 볼 수 있는 것만이다. 내보낸 것을 그대로 같은 클러스터에 적용하면 모든 단계가
//! `unchanged`다(서버가 채운 MAC·주소도 내보낸다 — `minimal`이면 뺀다).

use std::collections::BTreeSet;

use orbivisor_types::apply_set::ApplySetSpec;
use orbivisor_types::manifest::{Manifest, ManifestKind, folder_path};
use orbivisor_types::resource::Kind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::diff::strip_empty;
use super::kinds::{Live, ORDER, World, readable};
use crate::auth::Principal;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ManifestQuery {
    /// 쉼표로 구분한 종류(`vm,volume` 또는 `Vm,Volume`). 없으면 다루는 종류 모두
    pub kinds: Option<String>,
    /// 이 폴더 경로와 그 아래만(`/prod/web` — 트리마다)
    pub folder: Option<String>,
    /// 이 적용 묶음이 기억하는 것만
    pub apply_set: Option<String>,
    /// 기본값과 같은 칸(빈 목록·`false`·`null`)과 서버가 만든 MAC·주소를 뺀다(사람이 읽을 파일).
    #[serde(default)]
    pub minimal: bool,
}

/// 내보낸 매니페스트와 그때의 revision
#[derive(Debug, Serialize, ToSchema)]
pub struct ManifestList {
    pub revision: u64,
    pub documents: Vec<Manifest>,
}

/// 종류 이름(`vm`·`floating-ip` 또는 `Vm`·`FloatingIp`)
pub(crate) fn parse_kind(s: &str) -> Result<ManifestKind, ApiError> {
    let s = s.trim();
    let kind = ManifestKind::ALL
        .into_iter()
        .find(|k| k.as_str() == s)
        .or_else(|| Kind::parse(s).and_then(ManifestKind::from_kind));
    match kind {
        Some(k) if ORDER.contains(&k) => Ok(k),
        Some(k) => Err(ApiError::bad_request(format!(
            "{} documents are not supported yet",
            k.as_str()
        ))
        .with_code("manifest_unsupported")),
        None => Err(ApiError::bad_request(format!(
            "unknown manifest kind {s:?}"
        ))),
    }
}

/// 내보내는 매니페스트 하나
fn document(world: &World, live: &Live, minimal: bool) -> Manifest {
    let mut m = world.manifest_of(live);
    if minimal {
        if live.kind == ManifestKind::Vm
            && let Some(nics) = m.spec.get_mut("nics").and_then(Value::as_array_mut)
        {
            for nic in nics.iter_mut().filter_map(Value::as_object_mut) {
                nic.remove("mac");
                nic.remove("ips");
            }
        }
        if live.kind == ManifestKind::FloatingIp
            && let Some(obj) = m.spec.as_object_mut()
        {
            obj.remove("address");
        }
        strip_empty(&mut m.spec);
    }
    m
}

/// `GET /manifests`
pub(crate) fn list(
    state: &AppState,
    principal: &Principal,
    q: &ManifestQuery,
) -> Result<ManifestList, ApiError> {
    let kinds: Vec<ManifestKind> = match &q.kinds {
        Some(list) => list
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(parse_kind)
            .collect::<Result<_, _>>()?,
        None => ORDER.to_vec(),
    };
    if let Some(path) = &q.folder {
        folder_path(path).map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    let members: Option<BTreeSet<(Kind, Uuid)>> = match &q.apply_set {
        Some(name) => {
            let set = state
                .cluster
                .get_by_name::<ApplySetSpec>(name)?
                .ok_or_else(|| ApiError::not_found(format!("apply set {name:?} not found")))?;
            Some(set.status.objects.iter().map(|o| (o.kind, o.id)).collect())
        }
        None => None,
    };
    let world = World::load(&state.cluster)?;
    let mut documents = Vec::new();
    for kind in ORDER.into_iter().filter(|k| kinds.contains(k)) {
        for ((k, _), live) in world.objects.iter().filter(|((k, _), _)| *k == kind) {
            if members
                .as_ref()
                .is_some_and(|m| !m.contains(&(k.kind(), live.id)))
            {
                continue;
            }
            if !readable(state, principal, live) {
                continue;
            }
            let m = document(&world, live, q.minimal);
            if let Some(path) = q.folder.as_deref().filter(|p| *p != "/") {
                // 폴더 문서는 자기 경로, 그 밖은 든 폴더
                let at = if kind == ManifestKind::Folder {
                    m.metadata.folder.as_deref().map_or_else(
                        || format!("/{}", m.metadata.name),
                        |p| super::kinds::join_path(p, &m.metadata.name),
                    )
                } else {
                    m.metadata.folder.clone().unwrap_or_else(|| "/".into())
                };
                if at != path && !at.starts_with(&format!("{path}/")) {
                    continue;
                }
            }
            documents.push(m);
        }
    }
    Ok(ManifestList {
        revision: world.revision,
        documents,
    })
}

/// 종류 `kind`의 `key`(id 또는 이름 — 폴더는 id 또는 `트리:경로`)인 볼 수 있는 객체
pub(crate) fn find(
    state: &AppState,
    principal: &Principal,
    world: &World,
    kind: ManifestKind,
    key: &str,
) -> Result<Live, ApiError> {
    let found = match key.parse::<Uuid>() {
        Ok(id) => world.by_id(kind, id),
        Err(_) => world.objects.get(&(kind, key.to_string())),
    };
    found
        .filter(|l| readable(state, principal, l))
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("{} {key:?} not found", kind.as_str())))
}

/// `GET /manifests/{kind}/{key}`
pub(crate) fn one(
    state: &AppState,
    principal: &Principal,
    kind: &str,
    key: &str,
) -> Result<Manifest, ApiError> {
    let kind = parse_kind(kind)?;
    let world = World::load(&state.cluster)?;
    let live = find(state, principal, &world, kind, key)?;
    Ok(document(&world, &live, false))
}
