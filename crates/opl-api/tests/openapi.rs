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
        "/watch",
    ] {
        assert!(paths.contains_key(path), "{path} is not documented");
    }
    // 리소스 종류마다 따로 스키마가 생긴다.
    assert_eq!(
        doc["paths"]["/vms"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Resource_VmSpec_VmStatus"
    );
    assert!(doc["components"]["schemas"]["Resource_TaskSpec_TaskStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
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
