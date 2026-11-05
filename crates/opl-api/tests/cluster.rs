//! 클러스터 정보, 조인 토큰, 노드 합류와 제거.

mod common;

use common::Env;
use opl_api::cluster::{JoinRequest, JoinUrl};
use opl_cluster::types::NodeId;
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
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
    assert_eq!(view["node"], env.node.to_string());
    assert_eq!(view["members"].as_array().unwrap().len(), 1);
    assert_eq!(view["members"][0]["name"], "node1");
    assert_eq!(view["members"][0]["voter"], true);
    assert_eq!(view["members"][0]["online"], true);
    // 펜싱이 꺼진 노드는 null이다.
    assert_eq!(view["members"][0]["fencing"], json!(null), "{view}");
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.ha = Some(NodeHa {
        fencing: Fencing::Watchdog,
        recover_after_secs: 65,
    });
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let (_, view) = viewer.get("/cluster").await;
    assert_eq!(view["members"][0]["fencing"], "watchdog", "{view}");

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
            witness: false,
            version: Some(BUILD.into()),
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
    // 첫 핑 전에 제거되지 않도록 온라인으로 기록한다.
    assert!(joined.status.online);

    // 같은 토큰은 다시 쓸 수 없다.
    let (req, _) = join_request(&token, "node3");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "invalid_token");
    // 합류하다 멈춘 노드(오프라인 learner)는 새 토큰으로 다시 합류할 수 있다.
    env.cluster
        .update_status::<NodeSpec>(node, &NodeStatus::default())
        .await
        .unwrap();
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

/// 오프라인으로 기록한다(합류하다 멈춘 노드)
async fn mark_offline(env: &Env, node: Uuid) {
    env.cluster
        .update_status::<NodeSpec>(node, &NodeStatus::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_rejoining_learner_gets_its_new_cluster_address() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
    mark_offline(&env, node).await;

    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    req.address = "10.0.0.8:8006".into();
    req.cluster_address = "10.0.0.8:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = env.cluster.members();
    assert_eq!(members.learners.len(), 1);
    assert_eq!(members.addr(NodeId(node)), Some("10.0.0.8:8007"));
    let joined = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    assert_eq!(joined.spec.address, "10.0.0.8:8006");
    assert!(joined.status.online);
}

/// 합류 요청의 `witness`가 Node의 역할이 된다. 적지 않으면 일반 노드다. 역할은 노드를 고쳐도
/// 바뀌지 않는다.
#[tokio::test]
async fn a_witness_joins_as_a_witness() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (mut req, witness) = join_request(&new_token(&env).await, "w");
    req.witness = true;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let joined = env.cluster.get::<NodeSpec>(witness).unwrap().unwrap();
    assert!(joined.spec.witness);
    assert!(joined.status.online);
    // 클러스터 멤버 목록에도 위트니스인지가 보인다.
    let (_, view) = env.admin().await.get("/cluster").await;
    let witness_of = |id: Uuid| {
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()["witness"]
            .clone()
    };
    assert_eq!(witness_of(witness), true, "{view}");
    assert_eq!(witness_of(env.node), false, "{view}");

    let (req, node) = join_request(&new_token(&env).await, "node2");
    let mut body = json!(req);
    body.as_object_mut().unwrap().remove("witness");
    let (status, body) = anon.post("/cluster/join", body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !env.cluster
            .get::<NodeSpec>(node)
            .unwrap()
            .unwrap()
            .spec
            .witness
    );

    let admin = env.admin().await;
    for spec in [
        json!({ "address": "10.0.0.7:8006", "maintenance": true }),
        json!({ "address": "10.0.0.7:8006", "witness": false }),
    ] {
        let (status, body) = admin
            .put(&format!("/nodes/{witness}"), json!({ "spec": spec }), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["spec"]["witness"], true, "{body}");
    }
    let (status, body) = admin
        .put(
            &format!("/nodes/{node}"),
            json!({ "spec": { "address": "10.0.0.7:8006", "witness": true } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["witness"], false, "{body}");
}

/// 빌드 버전은 로그인 없이 읽는다.
#[tokio::test]
async fn version_is_served_without_login() {
    let env = Env::start().await;
    let (status, body) = env.anonymous().get("/version").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "version": BUILD }));
}

/// 이 노드의 기록된 버전을 바꾼다.
async fn set_version(env: &Env, node: Uuid, version: &str) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.version = Some(version.into());
    env.cluster
        .update_status::<NodeSpec>(node, &status)
        .await
        .unwrap();
}

/// 클러스터의 가장 낮은 노드 버전(위트니스·오프라인 노드도)보다 낮은 노드와 버전을 보내지 않는
/// 옛 노드는 토큰을 쓰지 않고 거절한다. 같거나 높은 버전은 합류한다. 멤버 목록에 버전이 보인다.
#[tokio::test]
async fn a_node_older_than_the_cluster_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    set_version(&env, env.node, "1.2.0").await;
    let mut witness: Resource<NodeSpec, NodeStatus> = Resource::new(
        "w",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    witness.status.version = Some("1.1.0".into());
    env.cluster.create(&witness).await.unwrap();

    let token = new_token(&env).await;
    for old in ["1.0.5", "1.1.0~rc1"] {
        let (mut req, _) = join_request(&token, "node2");
        req.version = Some(old.into());
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "version_too_old");
        assert_eq!(
            body["message"],
            format!("this node runs {old}; the cluster runs 1.1.0 or newer; upgrade it first")
        );
    }
    // 거절은 토큰을 쓰지 않는다.
    let (mut req, node) = join_request(&token, "node2");
    req.version = Some("1.1.0".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (mut req, node3) = join_request(&new_token(&env).await, "node3");
    req.version = Some("1:0.1".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (mut req, _) = join_request(&new_token(&env).await, "node4");
    req.version = None;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "version_too_old");
    assert_eq!(
        body["message"],
        "this node runs an unknown version; the cluster runs 1.1.0 or newer; upgrade it first"
    );

    // 다시 합류하는 노드 자신의 옛 기록은 세지 않는다.
    mark_offline(&env, node).await;
    set_version(&env, node, "0.1.0").await;
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.node = node;
    req.version = Some("1.1.0".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, view) = env.admin().await.get("/cluster").await;
    let version_of = |id: Uuid| {
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()["version"]
            .clone()
    };
    assert_eq!(version_of(env.node), "1.2.0", "{view}");
    // 합류 때 보낸 버전을 등록 전부터 적는다. 다시 합류하면 옛 기록을 바꾼다.
    assert_eq!(version_of(node3), "1:0.1", "{view}");
    assert_eq!(version_of(node), "1.1.0", "{view}");
}

/// 노드 버전을 하나도 적지 않은 옛 클러스터는 버전을 보내지 않는 노드도 받는다.
#[tokio::test]
async fn a_cluster_without_versions_takes_a_node_without_one() {
    let env = Env::start().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.version = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.version = None;
    let (status, body) = env.anonymous().post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn only_the_same_offline_node_rejoins() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );

    // 온라인 learner(승격을 기다리는 노드)의 id는 같은 이름으로도 가로챌 수 없다.
    let token = new_token(&env).await;
    let mut req = join_request(&token, "node2").0;
    req.node = node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");
    // 오프라인이어도 이름이 다르면 거부하고, 토큰은 쓰지 않는다.
    mark_offline(&env, node).await;
    let mut req = join_request(&token, "node3").0;
    req.node = node;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");
    assert_eq!(
        env.cluster.get::<NodeSpec>(node).unwrap().unwrap().name,
        "node2"
    );
    let (req, _) = join_request(&token, "node3");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn hosts_must_be_ips_or_host_names() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let token = new_token(&env).await;
    for (address, cluster_address) in [
        ("*.corp.example:8006", "10.0.0.7:8007"),
        ("10.0.0.7:8006", "evil host/x:8007"),
        ("0.0.0.0:8006", "10.0.0.7:8007"),
    ] {
        let (mut req, _) = join_request(&token, "node2");
        req.address = address.into();
        req.cluster_address = cluster_address.into();
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{address} {cluster_address}: {body}"
        );
        assert_eq!(body["code"], "invalid_request");
    }
    let (req, _) = join_request(&token, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
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
