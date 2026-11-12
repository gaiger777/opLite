//! API 문서(OpenAPI)

mod common;

use common::Env;
use reqwest::StatusCode;

#[tokio::test]
async fn openapi_document_is_served_without_login() {
    let env = Env::start().await;
    let (status, doc) = env.anonymous().get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let paths = doc["paths"].as_object().unwrap();
    for path in [
        "/auth/login",
        "/auth/realms",
        "/realms",
        "/realms/{key}",
        "/realms/{key}/actions/test",
        "/remotes",
        "/remotes/probe",
        "/remotes/{key}",
        "/remotes/{key}/actions/refresh",
        "/version",
        "/health",
        "/cluster/api-endpoints",
        "/settings",
        "/subscription",
        "/telemetry/preview",
        "/telemetry/reset-id",
        "/nodes/{key}/updates/refresh",
        "/nodes/{key}/upgrade",
        "/nodes/{key}/drain",
        "/nodes/{key}/metrics",
        "/metrics/nodes",
        "/vms/{key}/metrics",
        "/vms",
        "/vms/{key}",
        "/vms/{key}/actions/{action}",
        "/vms/{key}/console",
        "/volumes/import",
        "/volumes/import/check",
        "/volumes/{key}/resize",
        "/snapshots",
        "/vms/{key}/snapshots",
        "/vms/{key}/snapshots/{name}",
        "/vms/{key}/snapshots/{name}/revert",
        "/vms/{key}/clone",
        "/vms/{key}/migrate",
        "/vms/{key}/backups",
        "/backups",
        "/backups/{key}",
        "/backups/{key}/restore",
        "/backups/receive",
        "/backups/{key}/files/{file}",
        "/backups/{key}/actions/complete",
        "/backups/{key}/actions/replicate",
        "/backup-jobs",
        "/backup-jobs/{key}",
        "/backup-jobs/{key}/run",
        "/vm-imports",
        "/vm-imports/{key}",
        "/vm-imports/{key}/run",
        "/vm-imports/ova",
        "/vm-imports/ova/check",
        "/vm-imports/esxi",
        "/vm-imports/esxi/browse",
        "/watch",
    ] {
        assert!(paths.contains_key(path), "{path} is not documented");
    }
    // 풀은 `shared`만 바꾼다.
    assert_eq!(
        doc["paths"]["/storage-pools/{key}"]["patch"]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/PoolPatch"
    );
    // 리소스 종류마다 따로 스키마가 생긴다.
    assert_eq!(
        doc["paths"]["/vms"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Resource_VmSpec_VmStatus"
    );
    assert!(doc["components"]["schemas"]["Resource_TaskSpec_TaskStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_BackupSpec_BackupStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_BackupJobSpec_BackupJobStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_VmImportSpec_VmImportStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_SettingsSpec_SettingsStatus"].is_object());
    assert_eq!(
        doc["paths"]["/settings"]["patch"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SettingsPatch"
    );
    // 영역 응답에는 서비스 계정 비밀번호가 없고 있는지만 보인다. 영역 목록은 로그인 전에 읽는다.
    let ldap_view = &doc["components"]["schemas"]["LdapRealmView"]["properties"];
    assert!(ldap_view["bind_password"].is_null(), "{ldap_view}");
    assert!(ldap_view["has_bind_password"].is_object(), "{ldap_view}");
    let oidc_view = &doc["components"]["schemas"]["OidcRealmView"]["properties"];
    assert!(oidc_view["client_secret"].is_null(), "{oidc_view}");
    assert!(oidc_view["has_client_secret"].is_object(), "{oidc_view}");
    assert_eq!(
        doc["paths"]["/auth/realms"]["get"]["security"],
        serde_json::json!([{}])
    );
    // 구독 키 문자열은 설정 스키마에 없고, 상태만 따로 보인다.
    assert!(
        doc["components"]["schemas"]["SettingsSpec"]["properties"]["subscription"].is_null(),
        "{}",
        doc["components"]["schemas"]["SettingsSpec"]
    );
    assert_eq!(
        doc["paths"]["/subscription"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SubscriptionInfo"
    );
    let info = &doc["components"]["schemas"]["SubscriptionInfo"];
    assert_eq!(
        info["required"],
        serde_json::json!(["state", "used_nodes"]),
        "{info}"
    );
    // 텔레메트리 설정·상태와 미리 보기 보고
    assert_eq!(
        doc["components"]["schemas"]["SettingsSpec"]["properties"]["telemetry"]["$ref"],
        "#/components/schemas/TelemetrySettings"
    );
    assert_eq!(
        doc["components"]["schemas"]["SettingsStatus"]["properties"]["telemetry"]["$ref"],
        "#/components/schemas/TelemetryStatus"
    );
    assert_eq!(
        doc["paths"]["/telemetry/preview"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/TelemetryReport"
    );
    assert!(
        doc["components"]["schemas"]["SettingsPatch"]["properties"]["telemetry"].is_object(),
        "{}",
        doc["components"]["schemas"]["SettingsPatch"]
    );
    // 원격 상태의 노드·VM 수는 텔레메트리 보고의 것과 이름이 겹치지 않는다.
    let schemas = &doc["components"]["schemas"];
    let remote_status = &schemas["RemoteStatus"]["properties"];
    assert!(
        remote_status["nodes"]
            .to_string()
            .contains("RemoteNodeCounts"),
        "{remote_status}"
    );
    assert!(
        remote_status["vms"].to_string().contains("RemoteVmCounts"),
        "{remote_status}"
    );
    assert!(schemas["RemoteNodeCounts"]["properties"]["online"].is_object());
    assert!(schemas["RemoteVmCounts"]["properties"]["ha"].is_null());
    assert!(schemas["NodeCounts"]["properties"]["witness"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
    let selector = &doc["components"]["schemas"]["VmSelector"]["oneOf"];
    assert_eq!(selector.as_array().map(Vec::len), Some(3), "{selector}");
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
    let vm_status = &doc["components"]["schemas"]["VmStatus"];
    assert_eq!(
        vm_status["properties"]["migration"],
        serde_json::json!({
            "$ref": "#/components/schemas/MigrationStatus",
            "description": "진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다."
        })
    );
    assert!(
        !vm_status["required"]
            .as_array()
            .is_some_and(|r| r.contains(&"migration".into())),
        "{vm_status}"
    );
    // 변수 파일을 가진 노드도 없으면 빼고 쓰는 선택 필드다.
    assert_eq!(
        vm_status["properties"]["firmware_node"]["format"], "uuid",
        "{vm_status}"
    );
    assert!(
        !vm_status["required"]
            .as_array()
            .is_some_and(|r| r.contains(&"firmware_node".into())),
        "{vm_status}"
    );
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["token"]["scheme"], "bearer");
    assert_eq!(schemes["session"]["name"], "opl_session");
    // 경로는 /api/v1 아래에 있다.
    assert_eq!(doc["servers"][0]["url"], "/api/v1");
    // 업로드 본문은 바이트 배열이 아니라 바이너리 파일이다.
    let body = &doc["paths"]["/volumes/import"]["post"]["requestBody"]["content"]["application/octet-stream"]
        ["schema"];
    let schema = match body["$ref"].as_str() {
        Some(r) => &doc["components"]["schemas"][r.trim_start_matches("#/components/schemas/")],
        None => body,
    };
    assert_eq!(schema["format"], "binary", "{body}");
    // 헬스 체크는 인증 없이 부르고 503도 같은 모양이다.
    let health = &doc["paths"]["/health"]["get"];
    assert_eq!(health["security"], serde_json::json!([{}]));
    assert_eq!(
        health["responses"]["503"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Health"
    );
    // 로그인하지 않으면 `status`·`failed`만 오므로 나머지는 선택 필드다.
    assert_eq!(
        doc["components"]["schemas"]["Health"]["required"],
        serde_json::json!(["status", "failed"])
    );
    // 로그인은 인증 없이 부른다.
    assert_eq!(
        doc["paths"]["/auth/login"]["post"]["security"],
        serde_json::json!([{}])
    );
    // 로그아웃도 인증 없이 부를 수 있고, 쿠키를 지우는 헤더를 알린다.
    let logout = &doc["paths"]["/auth/logout"]["post"];
    assert_eq!(logout["security"], serde_json::json!([{}]));
    assert!(logout["responses"]["204"]["headers"]["set-cookie"].is_object());
    // 모든 참조(`$ref`)가 문서 안에 있다(웹 UI의 타입 생성이 실패하지 않게).
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for r in refs {
        let target = r
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("external ref {r}"));
        assert!(doc.pointer(target).is_some(), "dangling ref {r}");
    }
    // 웹 UI가 타입을 만드는 문서(`opl-api-openapi`)와 서버가 주는 문서가 같다.
    assert_eq!(doc, serde_json::to_value(opl_api::openapi()).unwrap());
}

fn collect_refs<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                match (key.as_str(), v.as_str()) {
                    ("$ref", Some(r)) => out.push(r),
                    _ => collect_refs(v, out),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}
