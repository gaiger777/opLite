//! 경로표(계획 62 설계 §8): API의 모든 (메서드, 경로)가 요구하는 권한과 그 대상. 미들웨어 [`layer`]가 이 표대로
//! 핸들러보다 먼저 확인한다(경로의 객체를 찾아 읽을 수 없으면 404, 읽을 수 있으나 권한이 없으면 403). 표에 없는 경로는
//! 거절한다(403). OpenAPI의 모든 경로가 표에 있는지 시험이 본다.
//!
//! 본문의 대상(만들기의 폴더·풀·네트워크 등)과 목록 거르기는 핸들러가 하고, 표는 그 경로를 [`Rule::Handler`]·
//! [`Rule::List`]로 적는다.

use std::sync::Arc;

use axum::extract::{FromRequestParts, MatchedPath, RawPathParams, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::authz_eval::AuthzTarget;
use opl_types::backup::BackupSpec;
use opl_types::folder::FolderSpec;
use opl_types::network::NetworkSpec;
use opl_types::resource::ResourceSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::TaskSpec;
use opl_types::vm::VmSpec;
use opl_types::vm_import::VmImportSpec;

use crate::auth::Principal;
use crate::authz::{Found, Owned, Owners};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::state::AppState;

/// 경로의 첫 인자(`{key}`·`{id}`)가 가리키는 객체의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Obj {
    Vm,
    Volume,
    Backup,
    VmImport,
    Network,
    StoragePool,
    Task,
    Folder,
}

/// 경로가 요구하는 것
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    /// 인증 없이(로그인·OIDC·합류·버전·헬스 — 핸들러가 따로 본다)
    Public,
    /// 인증만. 핸들러가 자기 것(토큰·whoami)이거나 본문의 대상(만들기의 폴더·풀·네트워크)을 확인한다.
    Handler,
    /// 인증만. 핸들러가 볼 수 있는 것만 돌려준다(목록 거르기).
    List,
    /// 루트(클러스터 객체)의 권한
    Root(Privilege),
    /// 경로 객체를 볼 권한(객체는 `system.read`, 객체 없는 Task는 `cluster.read`)
    Read(Obj),
    /// 경로 객체의 권한
    Object(Obj, Privilege),
}

use Obj::*;
use Privilege::*;
use Rule::*;

/// (메서드, `/api/v1` 아래 경로 템플릿, 규칙). `*`은 모든 메서드다(문서에 넣지 않는 원격 중계).
pub(crate) const ROUTES: &[(&str, &str, Rule)] = &[
    // 인증
    ("POST", "/auth/login", Public),
    ("POST", "/auth/logout", Public),
    ("GET", "/auth/oidc/callback", Public),
    ("GET", "/auth/oidc/{realm}/start", Public),
    ("GET", "/auth/realms", Public),
    ("GET", "/auth/whoami", Handler),
    ("GET", "/users", Root(ClusterAdmin)),
    ("POST", "/users", Root(ClusterAdmin)),
    // 자기 비밀번호는 누구나 바꾼다(핸들러가 본다).
    ("PUT", "/users/{key}", Handler),
    ("DELETE", "/users/{key}", Root(ClusterAdmin)),
    ("GET", "/tokens", Handler),
    ("POST", "/tokens", Handler),
    ("DELETE", "/tokens/{key}", Handler),
    ("GET", "/realms", Root(ClusterAdmin)),
    ("POST", "/realms", Root(ClusterAdmin)),
    ("GET", "/realms/{key}", Root(ClusterAdmin)),
    ("PUT", "/realms/{key}", Root(ClusterAdmin)),
    ("DELETE", "/realms/{key}", Root(ClusterAdmin)),
    ("POST", "/realms/{key}/actions/test", Root(ClusterAdmin)),
    // 원격
    ("GET", "/remotes", Root(ClusterRead)),
    ("POST", "/remotes", Root(ClusterAdmin)),
    ("POST", "/remotes/probe", Root(ClusterAdmin)),
    ("GET", "/remotes/{key}", Root(ClusterRead)),
    ("PUT", "/remotes/{key}", Root(ClusterAdmin)),
    ("DELETE", "/remotes/{key}", Root(ClusterAdmin)),
    ("POST", "/remotes/{key}/actions/refresh", Root(ClusterAdmin)),
    // 메서드에 따라 읽기·쓰기(핸들러가 루트에서 본다)
    ("*", "/remotes/{key}/api/{*path}", Handler),
    // 노드
    ("GET", "/nodes", Root(ClusterRead)),
    ("GET", "/nodes/{key}", Root(ClusterRead)),
    ("PUT", "/nodes/{key}", Root(ClusterAdmin)),
    ("DELETE", "/nodes/{key}", Root(ClusterAdmin)),
    ("GET", "/nodes/{key}/disks", Root(ClusterRead)),
    ("POST", "/nodes/{key}/updates/refresh", Root(ClusterAdmin)),
    ("POST", "/nodes/{key}/upgrade", Root(ClusterAdmin)),
    ("POST", "/nodes/{key}/drain", Root(ClusterAdmin)),
    ("POST", "/nodes/{key}/unfreeze", Root(ClusterAdmin)),
    ("GET", "/nodes/{key}/fence", Root(ClusterAdmin)),
    ("PUT", "/nodes/{key}/fence", Root(ClusterAdmin)),
    ("DELETE", "/nodes/{key}/fence", Root(ClusterAdmin)),
    ("POST", "/nodes/{key}/fence-test", Root(ClusterAdmin)),
    // 지표: 노드·클러스터는 루트 읽기, VM·풀은 그 객체
    ("GET", "/nodes/{key}/metrics", Root(ClusterRead)),
    ("GET", "/metrics/nodes", Root(ClusterRead)),
    ("GET", "/nodes/{key}/metrics/history", Root(ClusterRead)),
    ("GET", "/metrics/cluster", Root(ClusterRead)),
    ("GET", "/metrics", Root(ClusterRead)),
    ("GET", "/vms/{key}/metrics", Read(Vm)),
    ("GET", "/vms/{key}/metrics/history", Read(Vm)),
    (
        "GET",
        "/storage-pools/{key}/metrics/history",
        Read(StoragePool),
    ),
    // 경보
    ("GET", "/alert-rules", Root(AlertRead)),
    ("POST", "/alert-rules", Root(AlertManage)),
    ("GET", "/alert-rules/{key}", Root(AlertRead)),
    ("PUT", "/alert-rules/{key}", Root(AlertManage)),
    ("DELETE", "/alert-rules/{key}", Root(AlertManage)),
    ("GET", "/alerts", List),
    ("GET", "/alerts/history", List),
    ("GET", "/notification-targets", Root(ClusterAdmin)),
    ("POST", "/notification-targets", Root(ClusterAdmin)),
    ("GET", "/notification-targets/{key}", Root(ClusterAdmin)),
    ("PUT", "/notification-targets/{key}", Root(ClusterAdmin)),
    ("DELETE", "/notification-targets/{key}", Root(ClusterAdmin)),
    (
        "POST",
        "/notification-targets/{key}/test",
        Root(ClusterAdmin),
    ),
    // 클러스터·설정
    ("GET", "/cluster", Root(ClusterRead)),
    ("GET", "/cluster/ca", Public),
    ("GET", "/version", Public),
    ("GET", "/health", Public),
    ("GET", "/cluster/api-endpoints", Root(ClusterRead)),
    ("POST", "/cluster/join-tokens", Root(ClusterAdmin)),
    ("POST", "/cluster/join", Public),
    ("GET", "/settings", Root(ClusterRead)),
    ("PATCH", "/settings", Root(ClusterAdmin)),
    ("POST", "/telemetry/reset-id", Root(ClusterAdmin)),
    ("GET", "/telemetry/preview", Root(ClusterAdmin)),
    ("GET", "/subscription", Root(ClusterRead)),
    ("PUT", "/subscription", Root(ClusterAdmin)),
    ("DELETE", "/subscription", Root(ClusterAdmin)),
    // Ceph
    ("GET", "/ceph", Root(ClusterRead)),
    ("POST", "/ceph", Root(ClusterAdmin)),
    ("GET", "/ceph/status", Root(ClusterRead)),
    ("POST", "/ceph/mons", Root(ClusterAdmin)),
    ("POST", "/ceph/mgrs", Root(ClusterAdmin)),
    ("POST", "/ceph/osds", Root(ClusterAdmin)),
    ("POST", "/ceph/pools", Root(ClusterAdmin)),
    ("POST", "/ceph/clients", Root(ClusterAdmin)),
    // 풀·네트워크·보안 그룹
    ("GET", "/storage-pools", List),
    ("POST", "/storage-pools", Handler),
    ("GET", "/storage-pools/{key}", Read(StoragePool)),
    (
        "PATCH",
        "/storage-pools/{key}",
        Object(StoragePool, StorageConfig),
    ),
    (
        "DELETE",
        "/storage-pools/{key}",
        Object(StoragePool, StorageConfig),
    ),
    ("GET", "/networks", List),
    ("POST", "/networks", Handler),
    ("GET", "/networks/{key}", Read(Network)),
    ("DELETE", "/networks/{key}", Object(Network, NetworkConfig)),
    ("GET", "/security-groups", Root(ClusterRead)),
    ("POST", "/security-groups", Root(NetworkConfig)),
    ("GET", "/security-groups/{key}", Root(ClusterRead)),
    ("PATCH", "/security-groups/{key}", Root(NetworkConfig)),
    ("DELETE", "/security-groups/{key}", Root(NetworkConfig)),
    // HA·배치
    ("GET", "/ha-groups", Root(ClusterRead)),
    ("POST", "/ha-groups", Root(ClusterAdmin)),
    ("GET", "/ha-groups/{key}", Root(ClusterRead)),
    ("PUT", "/ha-groups/{key}", Root(ClusterAdmin)),
    ("DELETE", "/ha-groups/{key}", Root(ClusterAdmin)),
    ("GET", "/ha-checks", Root(ClusterRead)),
    ("POST", "/ha-checks", Root(ClusterAdmin)),
    ("GET", "/ha-checks/{key}", Root(ClusterRead)),
    ("PUT", "/ha-checks/{key}", Root(ClusterAdmin)),
    ("DELETE", "/ha-checks/{key}", Root(ClusterAdmin)),
    ("GET", "/placement", Root(ClusterRead)),
    ("GET", "/placement/recommendations", Root(ClusterRead)),
    // 추천의 VM에서 `vm.migrate`(핸들러가 본다)
    ("POST", "/placement/recommendations/{id}/apply", Handler),
    ("GET", "/placement-rules", Root(ClusterRead)),
    ("POST", "/placement-rules", Root(ClusterAdmin)),
    ("GET", "/placement-rules/{key}", Root(ClusterRead)),
    ("PUT", "/placement-rules/{key}", Root(ClusterAdmin)),
    ("DELETE", "/placement-rules/{key}", Root(ClusterAdmin)),
    // Task
    ("GET", "/tasks", List),
    ("GET", "/tasks/{key}", Read(Task)),
    // VM
    ("GET", "/vms", List),
    // 대상 폴더 `vm.create`, NIC마다 `network.assign`, 디스크 볼륨마다 `storage.allocate`
    ("POST", "/vms", Handler),
    ("GET", "/vms/{key}", Read(Vm)),
    // 새로 붙이는 네트워크·볼륨은 핸들러가 더 본다.
    ("PUT", "/vms/{key}", Object(Vm, VmConfig)),
    ("DELETE", "/vms/{key}", Object(Vm, VmDelete)),
    ("POST", "/vms/{key}/actions/{action}", Object(Vm, VmPower)),
    ("POST", "/vms/{key}/migrate", Object(Vm, VmMigrate)),
    ("POST", "/vms/{key}/cdrom", Object(Vm, VmConfig)),
    ("POST", "/vms/{key}/replicate", Object(Vm, VmMigrate)),
    ("POST", "/vms/{key}/template", Object(Vm, VmConfig)),
    ("DELETE", "/vms/{key}/template", Object(Vm, VmConfig)),
    ("POST", "/vms/{key}/firmware/reset", Object(Vm, VmConfig)),
    ("GET", "/vms/{key}/guest", Read(Vm)),
    ("POST", "/vms/{key}/guest/password", Object(Vm, VmConfig)),
    ("GET", "/vms/{key}/console", Object(Vm, VmConsole)),
    ("POST", "/vms/{key}/backups", Object(Vm, BackupCreate)),
    ("GET", "/snapshots", List),
    ("GET", "/vms/{key}/snapshots", Read(Vm)),
    ("POST", "/vms/{key}/snapshots", Object(Vm, VmSnapshot)),
    ("GET", "/vms/{key}/snapshots/{name}", Read(Vm)),
    (
        "DELETE",
        "/vms/{key}/snapshots/{name}",
        Object(Vm, VmSnapshot),
    ),
    (
        "POST",
        "/vms/{key}/snapshots/{name}/revert",
        Object(Vm, VmSnapshot),
    ),
    // 원본 `vm.clone` + 대상 폴더 `vm.create`·풀 `storage.allocate`(핸들러)
    ("POST", "/vms/{key}/clone", Object(Vm, VmClone)),
    // 백업
    ("GET", "/backups", List),
    ("GET", "/backups/{key}", Read(Backup)),
    ("DELETE", "/backups/{key}", Object(Backup, BackupDelete)),
    // 백업 `backup.restore` + 대상 폴더 `vm.create`·풀·네트워크(핸들러)
    (
        "POST",
        "/backups/{key}/restore",
        Object(Backup, BackupRestore),
    ),
    (
        "POST",
        "/backups/{key}/actions/replicate",
        Object(Backup, BackupCreate),
    ),
    // 다른 클러스터가 보내는 사본(루트에 둔다)
    ("POST", "/backups/receive", Root(BackupCreate)),
    ("PUT", "/backups/{key}/files/{file}", Root(BackupCreate)),
    (
        "POST",
        "/backups/{key}/actions/complete",
        Root(BackupCreate),
    ),
    ("GET", "/backup-jobs", Root(ClusterRead)),
    ("POST", "/backup-jobs", Root(BackupJob)),
    ("GET", "/backup-jobs/{key}", Root(ClusterRead)),
    ("PATCH", "/backup-jobs/{key}", Root(BackupJob)),
    ("DELETE", "/backup-jobs/{key}", Root(BackupJob)),
    ("POST", "/backup-jobs/{key}/run", Root(BackupJob)),
    // 가져오기(가져오기 객체는 루트에 있다)
    ("GET", "/vm-imports", List),
    ("GET", "/vm-imports/{key}", Read(VmImport)),
    ("DELETE", "/vm-imports/{key}", Object(VmImport, VmCreate)),
    // + 대상 폴더 `vm.create`·풀·네트워크(핸들러)
    ("POST", "/vm-imports/{key}/run", Object(VmImport, VmCreate)),
    ("POST", "/vm-imports/ova", Root(VmCreate)),
    ("POST", "/vm-imports/ova/check", Root(VmCreate)),
    ("POST", "/vm-imports/esxi/browse", Root(VmCreate)),
    ("POST", "/vm-imports/esxi", Root(VmCreate)),
    // 볼륨: 만들기는 풀 `storage.allocate`(핸들러), 볼륨은 붙은 VM 또는 풀을 따른다.
    ("GET", "/volumes", List),
    ("POST", "/volumes", Handler),
    ("POST", "/volumes/import", Handler),
    ("POST", "/volumes/import/check", Handler),
    ("POST", "/volumes/download", Handler),
    ("GET", "/volumes/{key}", Read(Volume)),
    ("DELETE", "/volumes/{key}", Object(Volume, StorageAllocate)),
    (
        "POST",
        "/volumes/{key}/resize",
        Object(Volume, StorageAllocate),
    ),
    (
        "POST",
        "/volumes/{key}/flatten",
        Object(Volume, StorageAllocate),
    ),
    // 이미지 카탈로그
    ("GET", "/image-catalog", Root(SystemRead)),
    ("POST", "/image-catalog", Root(ClusterAdmin)),
    ("DELETE", "/image-catalog/{id}", Root(ClusterAdmin)),
    // 받을 풀 `storage.allocate`(핸들러)
    ("POST", "/image-catalog/{id}/download", Handler),
    // 콘솔·watch·감사
    ("GET", "/watch", Handler),
    ("GET", "/audit", Root(ClusterAdmin)),
    // 폴더(계획 62)
    ("GET", "/folders", List),
    // 부모 폴더(없으면 루트)의 `inventory.folder`(핸들러)
    ("POST", "/folders", Handler),
    ("GET", "/folders/{id}", Read(Folder)),
    // 이름은 그 폴더 `inventory.folder`, 부모는 양쪽 `inventory.move`, 할당량은 부모 `inventory.quota`(핸들러)
    ("PATCH", "/folders/{id}", Read(Folder)),
    ("DELETE", "/folders/{id}", Object(Folder, InventoryFolder)),
    // 대상 폴더 `inventory.move`, 항목마다 원본 쪽 `inventory.move`(핸들러)
    (
        "POST",
        "/folders/{id}/members",
        Object(Folder, InventoryMove),
    ),
    ("GET", "/folders/{id}/usage", Read(Folder)),
    // 역할·그룹·grant·유효 권한(계획 62)
    ("GET", "/access-roles", Handler),
    ("POST", "/access-roles", Root(ClusterAdmin)),
    ("GET", "/access-roles/{key}", Handler),
    ("PUT", "/access-roles/{key}", Root(ClusterAdmin)),
    ("DELETE", "/access-roles/{key}", Root(ClusterAdmin)),
    ("GET", "/groups", Root(ClusterAdmin)),
    ("POST", "/groups", Root(ClusterAdmin)),
    ("GET", "/groups/{key}", Root(ClusterAdmin)),
    ("PUT", "/groups/{key}", Root(ClusterAdmin)),
    ("DELETE", "/groups/{key}", Root(ClusterAdmin)),
    // 대상의 `permission.modify`(위임·상승 막기는 핸들러)
    ("GET", "/grants", List),
    ("POST", "/grants", Handler),
    ("GET", "/grants/{id}", Handler),
    ("DELETE", "/grants/{id}", Handler),
    ("GET", "/permissions/mine", Handler),
    ("GET", "/permissions/effective", Handler),
    ("GET", "/permissions/who", Handler),
];

/// (메서드, 경로 템플릿)의 규칙. HEAD는 GET으로 본다.
pub(crate) fn rule(method: &Method, path: &str) -> Option<Rule> {
    let method = if method == Method::HEAD {
        "GET"
    } else {
        method.as_str()
    };
    ROUTES
        .iter()
        .find(|(m, p, _)| *p == path && (*m == method || *m == "*"))
        .map(|(_, _, r)| *r)
}

/// 경로의 `{key}`가 가리키는 객체와 그 주인. 없으면 None(핸들러가 404를 낸다).
pub(crate) fn resolve(cluster: &Cluster, obj: Obj, key: &str) -> Result<Option<Found>, ApiError> {
    fn missing<S: ResourceSpec>(key: &str) -> String {
        format!("{} {key:?} not found", S::KIND.as_str())
    }
    fn get<S: ResourceSpec>(
        cluster: &Cluster,
        key: &str,
    ) -> Result<Option<opl_cluster::Typed<S>>, ApiError> {
        match lookup::<S>(cluster, key) {
            Ok(found) => Ok(Some(found)),
            Err(e) if e.status == axum::http::StatusCode::NOT_FOUND => Ok(None),
            Err(e) => Err(e),
        }
    }
    let object = |target| Owned::object(target);
    Ok(match obj {
        Vm => get::<VmSpec>(cluster, key)?.map(|v| Found {
            owned: object(crate::authz::AuthzObject::authz_target(&v)),
            missing: missing::<VmSpec>(key),
        }),
        Network => get::<NetworkSpec>(cluster, key)?.map(|n| Found {
            owned: object(crate::authz::AuthzObject::authz_target(&n)),
            missing: missing::<NetworkSpec>(key),
        }),
        StoragePool => get::<StoragePoolSpec>(cluster, key)?.map(|p| Found {
            owned: object(crate::authz::AuthzObject::authz_target(&p)),
            missing: missing::<StoragePoolSpec>(key),
        }),
        Folder => get::<FolderSpec>(cluster, key)?.map(|f| Found {
            owned: object(AuthzTarget::Folder(f.id)),
            missing: missing::<FolderSpec>(key),
        }),
        VmImport => get::<VmImportSpec>(cluster, key)?.map(|i| Found {
            owned: object(Owners::import(i.id)),
            missing: missing::<VmImportSpec>(key),
        }),
        Volume => match get::<VolumeSpec>(cluster, key)? {
            None => None,
            Some(v) => Some(Found {
                owned: object(Owners::around_volume(cluster, &v.name, &v.spec.pool)?),
                missing: missing::<VolumeSpec>(key),
            }),
        },
        Backup => match get::<BackupSpec>(cluster, key)? {
            None => None,
            Some(b) => {
                let folder = match cluster.get::<VmSpec>(b.spec.vm)? {
                    Some(vm) => vm.spec.folder,
                    None => b.spec.vm_spec.folder,
                };
                Some(Found {
                    owned: object(AuthzTarget::Object {
                        kind: VmSpec::KIND,
                        id: b.spec.vm,
                        folder,
                    }),
                    missing: missing::<BackupSpec>(key),
                })
            }
        },
        Task => match get::<TaskSpec>(cluster, key)? {
            None => None,
            Some(t) => Some(Found {
                owned: Owners::around_task(cluster, &t.spec)?.task(&t.spec),
                missing: missing::<TaskSpec>(key),
            }),
        },
    })
}

/// 규칙을 확인한다.
pub(crate) fn check(
    state: &AppState,
    principal: &Principal,
    rule: Rule,
    key: Option<&str>,
) -> Result<(), ApiError> {
    let (obj, privilege) = match rule {
        Public | Handler | List => return Ok(()),
        // 플러그인 권한 검사(`Authorizer`)를 쓰면 루트 확인은 핸들러의 `AppState::authorize`가 플러그인으로 한다.
        Root(p) if state.authorizer.builtin() => return state.authz.require_root(principal, p),
        Root(_) => return Ok(()),
        Read(obj) => (obj, None),
        Object(obj, p) => (obj, Some(p)),
    };
    let Some(key) = key else {
        return Err(ApiError::internal(format!(
            "route rule {rule:?} without a path key"
        )));
    };
    let Some(found) = resolve(&state.cluster, obj, key)? else {
        // 없는 객체는 핸들러가 늘 같은 404를 낸다.
        return Ok(());
    };
    let privilege = privilege.unwrap_or(found.owned.read);
    state.authz.require(principal, privilege, &found)
}

/// 경로표 미들웨어. 라우트가 맞은 요청만 보고(`route_layer`), 인증한 주체를 요청에 넣어 핸들러가 다시 인증하지 않게 한다.
pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(template) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| crate::audit::api_path(m.as_str()).to_string())
    else {
        return next.run(req).await;
    };
    let Some(rule) = rule(req.method(), &template) else {
        tracing::error!(method = %req.method(), path = %template, "route without an authorization rule");
        return ApiError::forbidden().into_response();
    };
    if rule == Public {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    let principal = match Principal::from_request_parts(&mut parts, &state).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let key = match RawPathParams::from_request_parts(&mut parts, &state).await {
        Ok(params) => params.iter().next().map(|(_, v)| v.to_string()),
        Err(_) => None,
    };
    if let Err(e) = check(&state, &principal, rule, key.as_deref()) {
        return e.into_response();
    }
    parts.extensions.insert(principal);
    next.run(Request::from_parts(parts, body)).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// OpenAPI 문서의 (메서드, 경로) 모두
    fn documented() -> BTreeSet<(String, String)> {
        let doc = crate::openapi();
        let mut out = BTreeSet::new();
        for (path, item) in doc.paths.paths {
            for (method, op) in [
                ("GET", &item.get),
                ("POST", &item.post),
                ("PUT", &item.put),
                ("PATCH", &item.patch),
                ("DELETE", &item.delete),
                ("HEAD", &item.head),
                ("OPTIONS", &item.options),
                ("TRACE", &item.trace),
            ] {
                if op.is_some() {
                    out.insert((method.to_string(), path.clone()));
                }
            }
        }
        out
    }

    /// 설계 §8·§14: OpenAPI의 모든 경로에 규칙이 있다(없으면 미들웨어가 403으로 막지만 시험이 먼저 잡는다).
    #[test]
    fn every_openapi_path_has_a_rule() {
        let missing: Vec<_> = documented()
            .into_iter()
            .filter(|(m, p)| rule(&Method::from_bytes(m.as_bytes()).unwrap(), p).is_none())
            .collect();
        assert!(missing.is_empty(), "routes without a rule: {missing:?}");
    }

    /// 표에 문서에 없는 낡은 경로가 없고(문서에 넣지 않는 원격 중계만 `*`), 같은 (메서드, 경로)가 두 번 없다.
    #[test]
    fn every_rule_is_a_documented_route() {
        let documented = documented();
        let mut seen = BTreeSet::new();
        for (m, p, _) in ROUTES {
            assert!(seen.insert((*m, *p)), "duplicate rule {m} {p}");
            if *m == "*" {
                assert_eq!(*p, "/remotes/{key}/api/{*path}");
                continue;
            }
            assert!(
                documented.contains(&(m.to_string(), p.to_string())),
                "rule for an unknown route {m} {p}"
            );
        }
    }

    /// 객체 규칙은 경로에 객체 인자가 있다. 쓰기 경로는 `system.read`만으로 열리지 않는다.
    #[test]
    fn object_rules_have_a_path_key_and_writes_need_more_than_read() {
        for (m, p, r) in ROUTES {
            if matches!(r, Read(_) | Object(..)) {
                assert!(p.contains('{'), "{m} {p} has no key");
            }
            if matches!(
                r,
                Root(Privilege::SystemRead) | Object(_, Privilege::SystemRead)
            ) {
                assert_eq!(*m, "GET", "{m} {p} opens a write with system.read");
            }
            if *m != "GET" && *m != "*" {
                assert!(
                    !matches!(r, Read(_) | List | Root(Privilege::SystemRead))
                        || *p == "/folders/{id}",
                    "{m} {p} is a write guarded by read only"
                );
            }
        }
    }

    #[test]
    fn head_follows_get_and_any_matches_every_method() {
        assert_eq!(rule(&Method::HEAD, "/vms"), Some(List));
        assert_eq!(
            rule(&Method::PATCH, "/remotes/{key}/api/{*path}"),
            Some(Handler)
        );
        assert_eq!(rule(&Method::GET, "/no/such/route"), None);
    }
}
