//! API 문서(OpenAPI)

mod common;

use common::Env;
use reqwest::StatusCode;

#[tokio::test]
async fn openapi_document_is_served_without_login() {
    let env = Env::start().await;
    let (status, doc) = env.anonymous().get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    std::fs::write(
        "/tmp/opl-openapi.json",
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for path in [
        "/auth/login",
        "/vms",
        "/vms/{key}",
        "/vms/{key}/actions/{action}",
        "/vms/{key}/console",
        "/volumes/import",
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
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["token"]["scheme"], "bearer");
    assert_eq!(schemes["session"]["name"], "opl_session");
    // 로그인은 인증 없이 부른다.
    assert_eq!(
        doc["paths"]["/auth/login"]["post"]["security"],
        serde_json::json!([{}])
    );
}
