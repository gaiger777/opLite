//! 계획 72 스테이지 1: VMware 이전·VM 내보내기의 새 Task는 옛 노드가 남았으면 만들 수 없고(409 `cluster_upgrading`),
//! 실행 스테이지 전에는 실패로 끝난다. 새 권한 `vm.export`을 역할·토큰 범위에 넣는 쓰기도 모든 노드가 `vm-export`를 알
//! 때만이고 내장 admin만 가진다. 저장한 VMware 연결의 watch 이벤트에는 비밀번호가 없다.

mod common;

use std::time::Duration;

use common::{Client, Env};
use futures_util::StreamExt;
use opl_types::node::{NodeSpec, VM_EXPORT, VMWARE_MIGRATE};
use opl_types::resource::Resource;
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vmware::{Secret, VmwareSource, VmwareSourceSpec, VmwareTransport};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

/// 이 노드가 `feature`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_feature(env: &Env, feature: &str) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != feature);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

fn upgrading(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
}

#[tokio::test]
async fn vm_export_privilege_needs_every_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let role = json!({ "name": "exporter", "privileges": ["system.read", "vm.export"] });
    let token = json!({ "name": "ci", "scope": { "privileges": ["vm.export"], "targets": [{ "type": "root" }] } });
    let (status, body) = admin.post("/access-roles", role).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("vm.export")),
        "{body}"
    );
    let (status, body) = admin.post("/tokens", token.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // 내장 admin은 새 권한을 가지고 operator는 가지지 않는다.
    let (_, body) = admin.get("/access-roles/admin").await;
    assert!(
        body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("vm.export")),
        "{body}"
    );
    let (_, body) = admin.get("/access-roles/operator").await;
    assert!(
        !body["privileges"]
            .as_array()
            .unwrap()
            .contains(&json!("vm.export")),
        "{body}"
    );

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_feature(&env, VM_EXPORT).await;
    let (status, body) = admin
        .post(
            "/access-roles",
            json!({ "name": "exporter2", "privileges": ["vm.export", "no.such"] }),
        )
        .await;
    upgrading(status, &body);
    let (status, body) = admin
        .put(
            "/access-roles/exporter",
            json!({ "privileges": ["vm.export"] }),
            None,
        )
        .await;
    upgrading(status, &body);
    let (status, body) = admin.post("/tokens", token).await;
    upgrading(status, &body);
    // 새 권한을 쓰지 않는 쓰기는 그대로다.
    let (status, body) = admin
        .put(
            "/access-roles/exporter",
            json!({ "privileges": ["system.read"] }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// (Task, 기능, 없는 대상에 대한 결과 — `None`이면 성공)
fn ops() -> Vec<(TaskOp, &'static str, Option<&'static str>)> {
    let vmware = Some("VMware migration tasks are not available yet");
    vec![
        (TaskOp::VmwareSync { full: true }, VMWARE_MIGRATE, vmware),
        (TaskOp::VmwareCutover, VMWARE_MIGRATE, vmware),
        (TaskOp::VmwareRevert, VMWARE_MIGRATE, vmware),
        (
            TaskOp::VmwareCleanup {
                delete_target: false,
            },
            VMWARE_MIGRATE,
            vmware,
        ),
        // 스테이지 6: 내보내기 Task는 돈다 — 없는 내보내기는 쓸 수 없고, 지우기는 할 일이 없다.
        (
            TaskOp::VmExport {
                export: Uuid::now_v7(),
            },
            VM_EXPORT,
            Some("export was deleted"),
        ),
        (TaskOp::VmExportDelete, VM_EXPORT, None),
    ]
}

/// 실행 스테이지 전의 새 Task(Raft에 직접 쓴 것)는 실패로 끝난다(스테이지 6부터 내보내기 Task는 없는 대상을 그대로 알린다).
#[tokio::test]
async fn vmware_and_export_tasks_are_not_available_yet() {
    let env = Env::start().await;
    let admin = env.admin().await;
    for (op, _, error) in ops() {
        let mut t: opl_types::task::Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node: env.node,
            },
        );
        t.name = t.id.to_string();
        env.cluster.create(&t).await.unwrap();
        let done = admin.wait_task(&json!({ "id": t.id })).await;
        match error {
            Some(error) => {
                assert_eq!(done["status"]["state"], "failed", "{done}");
                assert_eq!(done["status"]["error"], error, "{done}");
            }
            None => assert_eq!(done["status"]["state"], "succeeded", "{done}"),
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(client: &Client, path: &str) -> Socket {
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_text(ws: &mut Socket) -> String {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return text.to_string();
        }
    }
}

/// 저장한 연결의 watch 이벤트(cluster.read — 보는 사람도)에는 비밀번호가 없다.
#[tokio::test]
async fn vmware_source_events_hide_the_password() {
    let env = Env::start().await;
    let viewer = env.user("ann", "viewer").await;
    let since = viewer.get("/vms").await.1["revision"].as_u64().unwrap();
    let mut ws = connect(
        &viewer,
        &format!("/watch?kinds=vmware-source&since={since}"),
    )
    .await;
    let source: VmwareSource = Resource::new(
        "vc1",
        VmwareSourceSpec {
            host: "vcenter.example.com".into(),
            port: 443,
            user: "migr@vsphere.local".into(),
            password: Secret::new("Sup3r-s3cret"),
            fingerprint: ["AB"; 32].join(":"),
            transport: VmwareTransport::Folder,
            datacenter_hint: None,
        },
    );
    env.cluster.create(&source).await.unwrap();
    let event = next_text(&mut ws).await;
    assert!(event.contains("vcenter.example.com"), "{event}");
    assert!(event.contains("migr@vsphere.local"), "{event}");
    assert!(!event.contains("Sup3r-s3cret"), "{event}");
    assert!(!event.contains("password"), "{event}");
    env.cluster
        .delete::<VmwareSourceSpec>(source.id, None)
        .await
        .unwrap();
    let event = next_text(&mut ws).await;
    assert!(event.contains("deleted"), "{event}");
    assert!(!event.contains("Sup3r-s3cret"), "{event}");
}
