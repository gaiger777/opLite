//! `GET /api/v1/watch?kinds=vm,task&since=<revision>`: 커밋된 변경을 WebSocket으로 보낸다.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use orbivisor_cluster::watch::{Watch, WatchError, WatchEvent};
use orbivisor_types::cloud_init::{PASSWORD_MASK, redact_vm_json};
use orbivisor_types::resource::Kind;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::authz::{EffectiveIndex, Owned, Owners, Subject};
use crate::error::{ApiError, Query};
use crate::state::AppState;
use orbivisor_cluster::types::RawResource;
use orbivisor_types::authz::Privilege;

/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 31] = [
    Kind::Vm,
    Kind::Volume,
    Kind::StoragePool,
    Kind::Node,
    Kind::Task,
    Kind::Network,
    Kind::Snapshot,
    Kind::SecurityGroup,
    Kind::Backup,
    Kind::BackupJob,
    Kind::VmImport,
    Kind::Settings,
    Kind::HaGroup,
    Kind::PlacementRule,
    // 경보 규칙(리더 경보 포함, 비밀 없음). 알림 대상은 비밀을 담아 넣지 않는다.
    Kind::AlertRule,
    // 계획 62: 폴더(볼 수 있는 것만), 역할(누구나), grant(목록과 같은 것만), 그룹(cluster.admin)
    Kind::Folder,
    Kind::AccessRole,
    Kind::Grant,
    Kind::Group,
    // 계획 67: 네트워크 폴더의 라우터·유동 IP·부하 분산기·네트워크 옮기기(볼 수 있는 것만 — 폴더를 따른다). 라우터·부하
    // 분산기 보기의 OVN 상태(게이트웨이 주인·멤버 건강)는 사건이 아니라 보기를 다시 읽어 얻는다.
    Kind::Router,
    Kind::FloatingIp,
    Kind::LoadBalancer,
    Kind::NetworkMigration,
    // 계획 68: 백업 저장소(루트 — system.read). S3 비밀 키·주 키는 이벤트에서 뺀다.
    Kind::BackupStore,
    // 계획 72: 저장한 vCenter·ESXi 연결(루트 — cluster.read). 비밀번호는 이벤트에서 뺀다.
    Kind::VmwareSource,
    // 계획 72: 이전 계획과 항목(루트 — system.read, 비밀 없음)
    Kind::VmwareMigration,
    Kind::VmwareVm,
    // 계획 73: 적용 묶음(루트 — system.read, 비밀 없음 — 선언의 비밀은 `$secret` 표시뿐)
    Kind::ApplySet,
    // 계획 73: ACME 계정·DNS 공급자·인증서(루트 — cluster.read). 계정 키·EAB·DNS 비밀·공유 키는 이벤트에서 뺀다.
    Kind::AcmeAccount,
    Kind::AcmeDns,
    Kind::Certificate,
];

/// 이벤트마다 거르는 종류(폴더를 따르는 것과 grant). 나머지는 구독할 때 종류 단위로 루트에서 본다.
fn per_event(kind: Kind) -> bool {
    crate::auth::folder_kind(kind) || matches!(kind, Kind::Grant | Kind::AccessRole)
}

/// 주인이 바뀌면(VM·풀의 이동) 보임이 함께 바뀌는 파생 종류
fn derived(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Volume | Kind::Snapshot | Kind::Backup | Kind::Task
    )
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct WatchQuery {
    /// 쉼표로 구분한 종류 이름. 예: `vm,task`
    pub kinds: String,
    /// 목록 응답의 `revision`. 그 뒤의 변경부터 받는다.
    pub since: u64,
}

fn parse_kinds(s: &str) -> Result<BTreeSet<Kind>, ApiError> {
    s.split(',')
        .map(|name| {
            WATCHABLE
                .into_iter()
                .find(|k| k.as_str() == name.trim())
                .ok_or_else(|| ApiError::bad_request(format!("cannot watch kind {name:?}")))
        })
        .collect()
}

/// 이벤트는 `orbivisor_cluster::watch::WatchEvent`의 JSON이다. 이어 받을 수 없게 되면
/// `{"type":"error","code":"expired"|"lagged"|"closed"}`를 보내고 닫는다. 클라이언트는 목록을
/// 다시 읽고 새로 구독한다.
///
/// 계획 62: 폴더를 따르는 종류(VM·볼륨·풀·네트워크·스냅샷·백업·가져오기·Task·폴더)와 grant는 이벤트마다 볼 수 있는
/// 것만 보낸다. 보이던 객체가 옮겨져 안 보이게 되면 그 객체의 `deleted` 이벤트(이름과 id만)를 보낸다. 구독자의 권한이
/// 바뀌거나(grant·역할·그룹·폴더 트리·사용자·토큰), 파생 종류(볼륨·스냅샷·백업·Task)를 구독하는데 VM·풀이 옮겨져
/// 보임이 바뀌면 `{"type":"error","code":"permissions"}`를 보내고 닫는다(목록을 다시 읽는다). 그 밖의 종류는 구독할 때
/// 루트에서 읽기 권한을 본다(403).
#[utoipa::path(get, path = "/watch", tag = "watch",
    params(WatchQuery),
    responses((status = 101, description = "WebSocket stream of change events"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn watch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<WatchQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let kinds = parse_kinds(&q.kinds)?;
    for kind in kinds.iter().filter(|k| !per_event(**k)) {
        state.authorize(&principal, Action::Read, *kind)?;
    }
    // 권한의 세대와 주인 표는 구독 전에 뜬다(이벤트를 놓치지 않게 구독보다 먼저).
    let generation = state.authz.generation();
    let index = state.authz.index(Subject::from(&principal))?;
    let owners = Owners::load(&state.cluster)?;
    let watch = state.cluster.watch(q.since).map_err(|e| match e {
        WatchError::Expired { .. } => ApiError::conflict("expired", e.to_string()),
        other => ApiError::internal(other),
    })?;
    let filter = Filter {
        state: state.clone(),
        subject: Subject::from(&principal),
        principal,
        generation,
        index,
        owners,
        derived: kinds.iter().any(|k| derived(*k)),
    };
    Ok(ws?.on_upgrade(move |socket| stream(socket, watch, kinds, filter)))
}

/// 연결 하나의 거르기 상태
struct Filter {
    state: Arc<AppState>,
    principal: Principal,
    subject: Subject,
    generation: u64,
    index: Arc<EffectiveIndex>,
    /// 이벤트 순서대로 고친 주인 표
    owners: Owners,
    /// 파생 종류를 구독한다
    derived: bool,
}

/// 이벤트 하나를 거른 결과
enum Verdict {
    Send(WatchEvent, bool),
    Skip,
    /// 목록을 다시 읽어야 한다(`permissions`).
    Close,
}

impl Filter {
    fn visible(&self, owned: Option<Owned>) -> bool {
        owned.is_some_and(|o| self.index.get(&o.target).contains(o.read))
    }

    /// 이벤트의 리소스를 VM 설정 권한으로 볼 수 있는지(cloud-init user-data를 그대로 낸다)
    fn writer(&self, owned: Option<Owned>) -> bool {
        owned.is_some_and(|o| self.index.get(&o.target).contains(Privilege::VmConfig))
    }

    /// 구독자의 권한이 바뀌었으면 거짓. 세대만 바뀌었으면(남의 grant·토큰 등) 새 색인으로 계속한다.
    fn still_valid(&mut self) -> bool {
        let generation = self.state.authz.generation();
        if generation == self.generation {
            return true;
        }
        match self.state.authz.index(self.subject) {
            Ok(index) if index.same_rights(&self.index) => {
                self.index = index;
                self.generation = generation;
                true
            }
            _ => false,
        }
    }

    /// 옮기기 이벤트에서 구독자가 읽을 수 없는 VM의 기록을 뺀다(리뷰 1 I3 — `GET`과 같다).
    fn redact(&self, mut event: WatchEvent) -> WatchEvent {
        if let WatchEvent::Put {
            kind: Kind::NetworkMigration,
            resource,
        }
        | WatchEvent::Deleted {
            kind: Kind::NetworkMigration,
            resource,
            ..
        } = &mut event
        {
            crate::network_migrations::redact_json(
                &self.state,
                &self.principal,
                &mut resource.spec,
                &mut resource.status,
            );
        }
        if let WatchEvent::Put {
            kind: Kind::BackupStore,
            resource,
        }
        | WatchEvent::Deleted {
            kind: Kind::BackupStore,
            resource,
            ..
        } = &mut event
        {
            crate::backup_stores::redact_json(&mut resource.spec);
        }
        if let WatchEvent::Put {
            kind: Kind::VmwareSource,
            resource,
        }
        | WatchEvent::Deleted {
            kind: Kind::VmwareSource,
            resource,
            ..
        } = &mut event
        {
            orbivisor_types::vmware::redact_source_json(&mut resource.spec);
        }
        if let WatchEvent::Put { kind, resource } | WatchEvent::Deleted { kind, resource, .. } =
            &mut event
        {
            match kind {
                Kind::AcmeAccount => orbivisor_types::acme::redact_account_json(&mut resource.spec),
                Kind::AcmeDns => orbivisor_types::acme::redact_dns_json(&mut resource.spec),
                Kind::Certificate => {
                    orbivisor_types::acme::redact_certificate_status_json(&mut resource.status)
                }
                _ => {}
            }
        }
        event
    }

    fn judge(&mut self, event: WatchEvent, kinds: &BTreeSet<Kind>) -> Verdict {
        if !self.still_valid() {
            return Verdict::Close;
        }
        let (kind, resource) = match &event {
            WatchEvent::Reset { .. } => {
                // 스냅샷 설치: 클라이언트가 목록을 다시 읽는다. 주인 표도 새로 뜬다.
                if let Ok(owners) = Owners::load(&self.state.cluster) {
                    self.owners = owners;
                }
                return Verdict::Send(event, false);
            }
            WatchEvent::Put { kind, resource } | WatchEvent::Deleted { kind, resource, .. } => {
                (*kind, resource)
            }
        };
        let scoped = per_event(kind);
        // 이벤트 앞의 보임: VM·네트워크·풀은 표가 아는 폴더, 나머지는 이벤트의 주인
        let owned_before = if scoped && kind != Kind::Grant && kind != Kind::AccessRole {
            match self.owners.stored(kind, resource.id) {
                Some(target) => Some(Owned::object(target)),
                None if orbivisor_types::folder::FolderKind::of(kind).is_some() => None,
                None => self.owners.of_event(kind, resource),
            }
        } else {
            None
        };
        let before = self.visible(owned_before);
        let moved_owner = self.owners.apply(&event);
        let deleted = matches!(event, WatchEvent::Deleted { .. });
        let owned_after = if deleted {
            owned_before
        } else {
            self.owners.of_event(kind, resource)
        };
        let after = match kind {
            Kind::Grant => crate::permissions::grant_visible_raw(
                &self.state,
                &self.principal,
                &self.index,
                resource,
            ),
            Kind::AccessRole => true,
            _ => self.visible(owned_after),
        };
        // VM·풀이 옮겨져 보임이 바뀌었으면 그 볼륨·스냅샷·백업·Task의 보임도 바뀐다.
        // 지우기는 파생 객체가 자기 이벤트로 지워지므로 닫지 않는다. 새로 생긴 VM·풀(앞의 주인이 없음)에는 아직 파생
        // 객체가 없다.
        if moved_owner
            && self.derived
            && !deleted
            && owned_before.is_some()
            && matches!(kind, Kind::Vm | Kind::StoragePool)
            && before != after
        {
            return Verdict::Close;
        }
        if !kinds.contains(&kind) {
            return Verdict::Skip;
        }
        if !scoped {
            let writer = self.writer(self.owners.of_event(kind, resource));
            return Verdict::Send(self.redact(event), writer);
        }
        let writer = self.writer(owned_after);
        match event {
            WatchEvent::Put { .. } if after => Verdict::Send(self.redact(event), writer),
            // 옮겨져 안 보이게 됐다: 이름과 id만 담은 지우기
            WatchEvent::Put { kind, resource } if before => {
                let mut gone = RawResource::new(resource.name.clone(), serde_json::Value::Null);
                gone.id = resource.id;
                gone.revision = resource.revision;
                gone.status = serde_json::Value::Null;
                Verdict::Send(
                    WatchEvent::Deleted {
                        revision: resource.revision,
                        kind,
                        resource: gone,
                    },
                    false,
                )
            }
            WatchEvent::Deleted { .. } if before || after => {
                Verdict::Send(self.redact(event), writer)
            }
            _ => Verdict::Skip,
        }
    }
}

/// 설정 이벤트에서 구독 키 문자열을 빼고(`settings::public`과 같이) 이미지 카탈로그 URL을 가리며, VM·스냅샷·백업의 cloud-init 비밀번호(해시)를
/// 가린다(목록·조회 응답과 같이). VM 쓰기 권한이 없는 구독자(`writer`가 거짓)에게는 user-data·vendor-data도 가린다.
fn public(mut event: WatchEvent, writer: bool) -> WatchEvent {
    if let WatchEvent::Put { kind, resource } | WatchEvent::Deleted { kind, resource, .. } =
        &mut event
    {
        if *kind == Kind::Settings
            && let Some(spec) = resource.spec.as_object_mut()
        {
            spec.remove("subscription");
            // 이미지 카탈로그의 URL은 받기 Task의 URL처럼 늘 가린다(관리자는 `GET /image-catalog`로 본다).
            if let Some(images) = spec.get_mut("image_catalog").and_then(|v| v.as_array_mut()) {
                for image in images {
                    for path in ["/url", "/checksum/url"] {
                        if let Some(url) = image.pointer_mut(path)
                            && let Some(text) = url.as_str()
                        {
                            *url = crate::volumes::redact_url(text).into();
                        }
                    }
                }
            }
        }
        if matches!(*kind, Kind::Vm | Kind::Snapshot | Kind::Backup) {
            let mut value = serde_json::json!({ "spec": std::mem::take(&mut resource.spec) });
            redact_vm_json(*kind, &mut value);
            let spec = match *kind {
                Kind::Vm => value.get_mut("spec"),
                _ => value.get_mut("spec").and_then(|s| s.get_mut("vm_spec")),
            };
            if !writer && let Some(ci) = spec.and_then(|s| s.get_mut("cloud_init")) {
                for field in ["user_data", "vendor_data"] {
                    if let Some(data) = ci.get_mut(field) {
                        *data = PASSWORD_MASK.into();
                    }
                }
            }
            resource.spec = value["spec"].take();
        }
        // 받기 Task의 URL(사용자 정보·쿼리)은 목록·조회 응답처럼 가린다.
        if *kind == Kind::Task {
            redact_task_json(&mut resource.spec, writer);
        }
    }
    event
}

/// `volumes::public_task`의 JSON 판(watch 이벤트의 Task spec): 받기 Task의 URL·체크섬 파일 URL을 가리고, 템플릿
/// 초안(받기)과 배포 덮어쓰기(복제)의 cloud-init 비밀번호를 가린다(계획 58). VM 쓰기 권한이 없는 구독자(`writer`가
/// 거짓)에게는 user-data·vendor-data도 가린다.
fn redact_task_json(spec: &mut serde_json::Value, writer: bool) {
    let op = &mut spec["op"];
    let ci = match op["kind"].as_str() {
        Some("volume-download") => {
            for field in ["url", "checksum_url"] {
                if let Some(url) = op[field].as_str() {
                    op[field] = crate::volumes::redact_url(url).into();
                }
            }
            op.get_mut("template")
        }
        Some("vm-clone") => op.get_mut("overrides"),
        _ => None,
    };
    let Some(ci) = ci.and_then(|v| v.get_mut("cloud_init")) else {
        return;
    };
    let fields: &[&str] = if writer {
        &["password"]
    } else {
        &["password", "user_data", "vendor_data"]
    };
    for field in fields {
        if let Some(value) = ci.get_mut(*field).filter(|v| v.is_string()) {
            *value = PASSWORD_MASK.into();
        }
    }
}

async fn stream(
    mut socket: WebSocket,
    mut watch: Watch,
    kinds: BTreeSet<Kind>,
    mut filter: Filter,
) {
    loop {
        tokio::select! {
            ev = watch.next() => {
                let text = match ev {
                    Ok(ev) => match filter.judge(ev, &kinds) {
                        Verdict::Send(ev, writer) => serde_json::to_string(&public(ev, writer)),
                        Verdict::Skip => continue,
                        Verdict::Close => {
                            let msg = serde_json::json!({
                                "type": "error",
                                "code": "permissions",
                                "message": "your permissions or the objects you can see changed; list again",
                            });
                            let _ = socket.send(Message::text(msg.to_string())).await;
                            let _ = socket.send(Message::Close(None)).await;
                            return;
                        }
                    },
                    Err(e) => {
                        let code = match e {
                            WatchError::Expired { .. } => "expired",
                            WatchError::Lagged => "lagged",
                            WatchError::Closed => "closed",
                        };
                        let msg = serde_json::json!({
                            "type": "error",
                            "code": code,
                            "message": e.to_string(),
                        });
                        let _ = socket.send(Message::text(msg.to_string())).await;
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                };
                let Ok(text) = text else { return };
                if socket.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 계획 67: 라우터·유동 IP·부하 분산기·네트워크 옮기기의 실시간 갱신(폴더를 따라 사건마다 거른다)
    #[test]
    fn sdn_objects_are_watchable_per_folder() {
        let kinds = parse_kinds("router,floating-ip,load-balancer,network-migration").unwrap();
        assert_eq!(kinds.len(), 4);
        assert!(kinds.iter().all(|k| per_event(*k)));
    }

    /// 계획 72: 저장한 VMware 연결·이전 계획·항목은 루트에서 본다(사건마다 거르지 않음). 내보내기는 아직 아니다.
    #[test]
    fn vmware_kinds_are_watchable_at_the_root() {
        let kinds = parse_kinds("vmware-source,vmware-migration,vmware-vm").unwrap();
        assert_eq!(kinds.len(), 3);
        assert!(kinds.iter().all(|k| !per_event(*k)));
        assert!(parse_kinds("vm-export").is_err());
    }

    #[test]
    fn snapshots_are_watchable() {
        let kinds = parse_kinds("vm, snapshot").unwrap();
        assert!(kinds.contains(&Kind::Snapshot));
        assert!(parse_kinds("user").is_err());
    }

    #[test]
    fn backups_are_watchable() {
        let kinds = parse_kinds("backup").unwrap();
        assert!(kinds.contains(&Kind::Backup));
    }

    #[test]
    fn backup_jobs_are_watchable() {
        let kinds = parse_kinds("backup-job").unwrap();
        assert!(kinds.contains(&Kind::BackupJob));
    }

    #[test]
    fn vm_imports_are_watchable() {
        let kinds = parse_kinds("vm-import").unwrap();
        assert!(kinds.contains(&Kind::VmImport));
    }

    #[test]
    fn settings_are_watchable() {
        let kinds = parse_kinds("settings").unwrap();
        assert!(kinds.contains(&Kind::Settings));
    }

    #[test]
    fn ha_groups_and_placement_rules_are_watchable() {
        let kinds = parse_kinds("ha-group,placement-rule").unwrap();
        assert!(kinds.contains(&Kind::HaGroup));
        assert!(kinds.contains(&Kind::PlacementRule));
    }

    /// VM·스냅샷·백업 이벤트의 cloud-init 비밀번호(해시)는 가려 보낸다. 다른 칸과 다른 종류는 그대로다.
    #[test]
    fn events_hide_the_cloud_init_password() {
        use orbivisor_cluster::types::RawResource;
        use orbivisor_types::resource::Resource;
        use serde_json::json;

        let hash = "$6$salt$hash";
        let vm: RawResource = Resource::new(
            "web",
            json!({ "cloud_init": { "user": "orbi", "password": hash } }),
        );
        let event = public(
            WatchEvent::Put {
                kind: Kind::Vm,
                resource: vm,
            },
            true,
        );
        let WatchEvent::Put { resource, .. } = &event else {
            panic!("{event:?}");
        };
        assert_eq!(resource.spec["cloud_init"]["password"], PASSWORD_MASK);
        assert_eq!(resource.spec["cloud_init"]["user"], "orbi");

        for kind in [Kind::Snapshot, Kind::Backup] {
            let r: RawResource = Resource::new(
                "s1",
                json!({ "vm_spec": { "cloud_init": { "password": hash } } }),
            );
            let event = public(
                WatchEvent::Deleted {
                    revision: 7,
                    kind,
                    resource: r,
                },
                true,
            );
            assert!(!serde_json::to_string(&event).unwrap().contains(hash));
        }
        let task: RawResource = Resource::new("t", json!({ "password": hash }));
        let event = public(
            WatchEvent::Put {
                kind: Kind::Task,
                resource: task,
            },
            true,
        );
        assert!(serde_json::to_string(&event).unwrap().contains(hash));
    }

    /// VM 쓰기 권한이 없는 구독자에게는 VM·스냅샷·백업의 user-data·vendor-data도 가려 보낸다. 쓰기 권한이 있으면 그대로다.
    #[test]
    fn events_hide_user_data_from_readers() {
        use orbivisor_cluster::types::RawResource;
        use orbivisor_types::resource::Resource;
        use serde_json::json;

        let ci = json!({ "user": "orbi", "user_data": "#cloud-config\n{}", "vendor_data": "x" });
        for (kind, spec) in [
            (Kind::Vm, json!({ "cloud_init": ci })),
            (Kind::Snapshot, json!({ "vm_spec": { "cloud_init": ci } })),
            (Kind::Backup, json!({ "vm_spec": { "cloud_init": ci } })),
        ] {
            for writer in [false, true] {
                let r: RawResource = Resource::new("r", spec.clone());
                let event = public(WatchEvent::Put { kind, resource: r }, writer);
                let WatchEvent::Put { resource, .. } = &event else {
                    panic!("{event:?}");
                };
                let got = match kind {
                    Kind::Vm => &resource.spec["cloud_init"],
                    _ => &resource.spec["vm_spec"]["cloud_init"],
                };
                let want = if writer {
                    ci.clone()
                } else {
                    json!({ "user": "orbi", "user_data": PASSWORD_MASK, "vendor_data": PASSWORD_MASK })
                };
                assert_eq!(*got, want, "{kind:?} writer={writer}");
            }
        }
        // cloud-init 없는 VM에는 칸을 더하지 않는다.
        let r: RawResource = Resource::new("r", json!({ "cpu": 1 }));
        let event = public(
            WatchEvent::Put {
                kind: Kind::Vm,
                resource: r,
            },
            false,
        );
        let WatchEvent::Put { resource, .. } = &event else {
            panic!("{event:?}");
        };
        assert_eq!(resource.spec, json!({ "cpu": 1 }));
    }

    /// 설정 이벤트는 구독 키를 빼고 이미지 카탈로그의 이미지·체크섬 파일 URL의 쿼리를 가려 보낸다(쓰기 권한과 상관없이).
    #[test]
    fn settings_events_hide_catalog_url_queries() {
        use orbivisor_cluster::types::RawResource;
        use orbivisor_types::resource::Resource;
        use serde_json::json;

        let settings: RawResource = Resource::new(
            "cluster",
            json!({
                "subscription": "key",
                "image_catalog": [
                    { "id": "a", "url": "https://m.example/a.img?token=t0k",
                      "checksum": { "kind": "sums", "url": "https://m.example/SUMS?sig=s1g" } },
                    { "id": "b", "url": "https://m.example/b.img",
                      "checksum": { "kind": "sha256", "value": "00" } },
                ],
            }),
        );
        let event = public(
            WatchEvent::Put {
                kind: Kind::Settings,
                resource: settings,
            },
            true,
        );
        let WatchEvent::Put { resource, .. } = &event else {
            panic!("{event:?}");
        };
        let images = &resource.spec["image_catalog"];
        assert_eq!(images[0]["url"], "https://m.example/a.img?***");
        assert_eq!(images[0]["checksum"]["url"], "https://m.example/SUMS?***");
        assert_eq!(images[1]["url"], "https://m.example/b.img");
        assert_eq!(images[1]["checksum"]["value"], "00");
        assert!(resource.spec.get("subscription").is_none());
    }

    /// 받기 Task의 URL은 사용자 정보와 쿼리를 가려 보낸다. 다른 Task는 그대로다.
    #[test]
    fn events_hide_download_credentials() {
        use orbivisor_cluster::types::RawResource;
        use orbivisor_types::resource::Resource;
        use serde_json::json;

        let url = "https://u:secret@a.example/x.iso?token=t0k";
        let task: RawResource = Resource::new(
            "t",
            json!({ "op": { "kind": "volume-download", "url": url, "source_format": "raw" } }),
        );
        let event = public(
            WatchEvent::Put {
                kind: Kind::Task,
                resource: task,
            },
            true,
        );
        let WatchEvent::Put { resource, .. } = &event else {
            panic!("{event:?}");
        };
        assert_eq!(
            resource.spec["op"]["url"],
            "https://***@a.example/x.iso?***"
        );
        let other: RawResource =
            Resource::new("t", json!({ "op": { "kind": "vm-start", "url": url } }));
        let event = public(
            WatchEvent::Put {
                kind: Kind::Task,
                resource: other,
            },
            true,
        );
        assert!(serde_json::to_string(&event).unwrap().contains("secret"));
    }

    #[test]
    fn security_groups_are_watchable() {
        let kinds = parse_kinds("security-group").unwrap();
        assert!(kinds.contains(&Kind::SecurityGroup));
    }

    #[test]
    fn task_events_hide_template_passwords_and_checksum_urls() {
        let mut spec = serde_json::json!({ "op": {
            "kind": "volume-download",
            "url": "https://u:p@a.example/x.img",
            "source_format": "qcow2",
            "checksum_url": "https://a.example/SUMS?token=t",
            "template": { "name": "t", "network": "lan", "cloud_init": { "password": "$6$s$h" } },
        }});
        redact_task_json(&mut spec, true);
        assert_eq!(spec["op"]["url"], "https://***@a.example/x.img");
        assert_eq!(spec["op"]["checksum_url"], "https://a.example/SUMS?***");
        assert_eq!(
            spec["op"]["template"]["cloud_init"]["password"],
            PASSWORD_MASK
        );

        let mut spec = serde_json::json!({ "op": {
            "kind": "vm-clone", "name": "web-1", "snapshot": null, "pool": null,
            "overrides": { "cloud_init": { "password": "$6$s$h", "user": "u" } },
        }});
        redact_task_json(&mut spec, true);
        assert_eq!(
            spec["op"]["overrides"]["cloud_init"]["password"],
            PASSWORD_MASK
        );
        assert_eq!(spec["op"]["overrides"]["cloud_init"]["user"], "u");
        // 비밀번호가 없는 Task는 그대로다.
        let mut spec = serde_json::json!({ "op": { "kind": "vm-clone", "name": "web-1" } });
        let before = spec.clone();
        redact_task_json(&mut spec, true);
        assert_eq!(spec, before);

        // VM 쓰기 권한이 없으면 user-data·vendor-data도 가린다(VM 스펙처럼).
        let clone = serde_json::json!({ "op": {
            "kind": "vm-clone", "name": "web-1",
            "overrides": { "cloud_init": { "user_data": "#cloud-config\npassword: x", "vendor_data": "v", "user": "u" } },
        }});
        let mut spec = clone.clone();
        redact_task_json(&mut spec, true);
        assert_eq!(spec, clone);
        redact_task_json(&mut spec, false);
        let ci = &spec["op"]["overrides"]["cloud_init"];
        assert_eq!(ci["user_data"], PASSWORD_MASK);
        assert_eq!(ci["vendor_data"], PASSWORD_MASK);
        assert_eq!(ci["user"], "u");
        let mut spec = serde_json::json!({ "op": {
            "kind": "volume-download", "url": "https://a.example/x.img",
            "template": { "name": "t", "cloud_init": { "user_data": "secret" } },
        }});
        redact_task_json(&mut spec, false);
        assert_eq!(
            spec["op"]["template"]["cloud_init"]["user_data"],
            PASSWORD_MASK
        );
    }
}
