//! 클러스터 정보, 조인 토큰, 노드 합류와 제거.

mod common;

use common::Env;
use opl_api::cluster::{JoinRequest, JoinUrl};
use opl_cluster::types::NodeId;
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn cluster_view_and_ca() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let (status, view) = viewer.get("/cluster").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["name"], "test");
    assert_eq!(view["leader"], env.node.to_string());
    assert_eq!(view["members"].as_array().unwrap().len(), 1);
    assert_eq!(view["members"][0]["name"], "node1");
    assert_eq!(view["members"][0]["voter"], true);
    assert_eq!(view["members"][0]["online"], true);

    // CA는 인증 없이 받고, 지문은 클러스터 정보와 같다.
    let anon = env.anonymous();
    assert_eq!(anon.get("/cluster").await.0, StatusCode::UNAUTHORIZED);
    let resp = anon
        .http
        .get(format!("{}/api/v1/cluster/ca", env.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let pem = resp.text().await.unwrap();
    assert_eq!(fingerprint(&pem).unwrap(), view["ca_fingerprint"]);
}

#[tokio::test]
async fn join_tokens_are_for_admins() {
    let env = Env::start().await;
    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator.post("/cluster/join-tokens", json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let admin = env.admin().await;
    let (status, body) = admin
        .post("/cluster/join-tokens", json!({ "ttl_secs": 0 }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, created) = admin.post("/cluster/join-tokens", json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let url: JoinUrl = created["url"].as_str().unwrap().parse().unwrap();
    assert_eq!(url.address, "127.0.0.1:0");
    let (_, view) = admin.get("/cluster").await;
    assert_eq!(url.ca_fingerprint, view["ca_fingerprint"]);
    // 토큰은 사용자 목록처럼 아무도 볼 수 없다(목록 경로가 없다). 저장소에는 해시만 있다.
    let (_, tokens) = env.cluster.list::<JoinTokenSpec>().unwrap();
    assert_eq!(tokens.len(), 1);
    assert!(!url.token.contains(&tokens[0].spec.secret_hash));
}

fn join_request(token: &str, name: &str) -> (JoinRequest, Uuid) {
    let node = Uuid::now_v7();
    let (_key, csr_pem) = node_key_and_csr().unwrap();
    (
        JoinRequest {
            token: token.to_string(),
            node,
            name: name.to_string(),
            address: "10.0.0.7:8006".into(),
            cluster_address: "10.0.0.7:8007".into(),
            csr_pem,
        },
        node,
    )
}

async fn new_token(env: &Env) -> String {
    let (status, created) = env
        .admin()
        .await
        .post("/cluster/join-tokens", json!({}))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["url"]
        .as_str()
        .unwrap()
        .parse::<JoinUrl>()
        .unwrap()
        .token
}

#[tokio::test]
async fn a_node_joins_once_per_token() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let token = new_token(&env).await;

    // 이름이 겹치면 토큰을 쓰지 않고 거부한다.
    let (req, _) = join_request(&token, "node1");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");

    let (req, node) = join_request(&token, "node2");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["cluster"], "test");
    let cert = first_cert(body["cert_pem"].as_str().unwrap()).unwrap();
    assert_eq!(node_id_of(&cert), Some(node));
    assert_eq!(body["via"], NodeId(env.node).to_string());
    let members = env.cluster.members();
    assert_eq!(
        members.learners.get(&NodeId(node)).unwrap(),
        "10.0.0.7:8007"
    );
    let joined = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    assert_eq!(joined.name, "node2");
    assert_eq!(joined.spec.address, "10.0.0.7:8006");

    // 같은 토큰은 다시 쓸 수 없다.
    let (req, _) = join_request(&token, "node3");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "invalid_token");
    // 합류하다 멈춘 노드(learner)는 새 토큰으로 다시 합류할 수 있다.
    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(env.cluster.members().learners.len(), 1);
    // 투표 멤버는 다시 합류할 수 없다.
    let mut req = join_request(&new_token(&env).await, "node9").0;
    req.node = env.node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");
}

#[tokio::test]
async fn bad_or_expired_tokens_are_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    for token in [
        "",
        "garbage",
        "oplj_nothex_x",
        &format!("oplj_{}_x", Uuid::now_v7().simple()),
    ] {
        let (req, _) = join_request(token, "node2");
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token}: {body}");
    }
    let token = new_token(&env).await;
    let (id, _) = token
        .strip_prefix("oplj_")
        .unwrap()
        .split_once('_')
        .unwrap();
    let id = Uuid::try_parse(id).unwrap();
    // 비밀 값이 틀리면 거부한다.
    let (req, _) = join_request(&format!("oplj_{}_wrong", id.simple()), "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // 만료된 토큰도 거부한다.
    let mut stored = env.cluster.get::<JoinTokenSpec>(id).unwrap().unwrap();
    stored.spec.expires_at = 1;
    env.cluster
        .update_spec(id, None, stored.labels.clone(), &stored.spec)
        .await
        .unwrap();
    let (req, _) = join_request(&token, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // CSR이 깨졌으면 토큰을 쓰지 않는다.
    let token = new_token(&env).await;
    let (mut req, _) = join_request(&token, "node2");
    req.csr_pem = "garbage".into();
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (req, _) = join_request(&token, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_offline_nodes_are_removed() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        env.anonymous().post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );

    // 이 노드와 온라인 노드는 뺄 수 없다.
    let (status, body) = admin.delete("/nodes/node1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_online");
    let online = NodeStatus {
        online: true,
        ..Default::default()
    };
    env.cluster
        .update_status::<NodeSpec>(node, &online)
        .await
        .unwrap();
    assert_eq!(admin.delete("/nodes/node2").await.0, StatusCode::CONFLICT);
    env.cluster
        .update_status::<NodeSpec>(node, &NodeStatus::default())
        .await
        .unwrap();

    let operator = env.user("op", "operator").await;
    assert_eq!(
        operator.delete("/nodes/node2").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = admin.delete("/nodes/node2").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert!(!env.cluster.members().contains(NodeId(node)));
    assert_eq!(admin.get("/nodes/node2").await.0, StatusCode::NOT_FOUND);
}
