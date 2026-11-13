//! Redfish 요청은 프록시 환경 변수(`HTTP_PROXY` 등)를 따르지 않는다. 환경 변수는 프로세스 전역이므로 다른 시험과
//! 나눈 시험 바이너리에서 본다.

use std::time::Duration;

use axum::routing::get;
use opl_api::power_fence::{Fencer, PowerFence};
use opl_types::fence::{FenceMethod, Power, RedfishFence};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use serde_json::json;

#[tokio::test]
async fn redfish_ignores_proxy_environment_variables() {
    // 닿지 않는 프록시. 따르면 연결이 거절된다.
    for var in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        // SAFETY: 이 시험 바이너리의 유일한 시험이고, 환경을 읽는 다른 스레드를 띄우기 전이다.
        unsafe { std::env::set_var(var, "http://127.0.0.1:9") };
    }
    for var in ["NO_PROXY", "no_proxy"] {
        // SAFETY: 위와 같다.
        unsafe { std::env::remove_var(var) };
    }
    let app = axum::Router::new().route(
        "/redfish/v1/Systems/1",
        get(|| async { axum::Json(json!({ "PowerState": "Off" })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let node: Node = Resource::new(
        "node-b",
        NodeSpec {
            address: "10.1.2.3:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    let fencer = Fencer {
        timeout: Duration::from_secs(5),
        ..Fencer::default()
    };
    let method = FenceMethod::Redfish(RedfishFence {
        url,
        user: "admin".into(),
        password: Some("pw".into()),
        system_id: Some("1".into()),
        ca_pem: None,
        insecure: false,
    });
    assert_eq!(fencer.status(&node, &method).await, Ok(Power::Off));
}
