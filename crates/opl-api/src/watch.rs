//! `GET /api/v1/watch?kinds=vm,task&since=<revision>`: 커밋된 변경을 WebSocket으로 보낸다.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_types::cloud_init::{PASSWORD_MASK, redact_vm_json};
use opl_types::resource::Kind;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Query};
use crate::state::AppState;

/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 14] = [
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
];

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

/// 이벤트는 `opl_cluster::watch::WatchEvent`의 JSON이다. 이어 받을 수 없게 되면
/// `{"type":"error","code":"expired"|"lagged"|"closed"}`를 보내고 닫는다. 클라이언트는 목록을
/// 다시 읽고 새로 구독한다.
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
    for kind in &kinds {
        state.authorize(&principal, Action::Read, *kind)?;
    }
    let watch = state.cluster.watch(q.since).map_err(|e| match e {
        WatchError::Expired { .. } => ApiError::conflict("expired", e.to_string()),
        other => ApiError::internal(other),
    })?;
    let writer = crate::vms::writes_vms(&state, &principal);
    Ok(ws?.on_upgrade(move |socket| stream(socket, watch, kinds, writer)))
}

fn wanted(event: &WatchEvent, kinds: &BTreeSet<Kind>) -> bool {
    match event {
        WatchEvent::Put { kind, .. } | WatchEvent::Deleted { kind, .. } => kinds.contains(kind),
        WatchEvent::Reset { .. } => true,
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

async fn stream(mut socket: WebSocket, mut watch: Watch, kinds: BTreeSet<Kind>, writer: bool) {
    loop {
        tokio::select! {
            ev = watch.next() => {
                let text = match ev {
                    Ok(ev) if wanted(&ev, &kinds) => serde_json::to_string(&public(ev, writer)),
                    Ok(_) => continue,
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
        use opl_cluster::types::RawResource;
        use opl_types::resource::Resource;
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
        use opl_cluster::types::RawResource;
        use opl_types::resource::Resource;
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
        use opl_cluster::types::RawResource;
        use opl_types::resource::Resource;
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
        use opl_cluster::types::RawResource;
        use opl_types::resource::Resource;
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
