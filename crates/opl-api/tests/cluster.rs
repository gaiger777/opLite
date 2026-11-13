//! 클러스터 정보, 조인 토큰, 노드 합류와 제거.

mod common;

use common::Env;
use opl_api::cluster::{JoinRequest, JoinUrl};
use opl_cluster::types::NodeId;
use opl_peer::pki::{cert_names, fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::{ClusterSpec, ClusterStatus, JoinTokenSpec};
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
        fence_after_secs: None,
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
            cluster_addresses: vec![],
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

/// 관찰 노드(Raft 밖, 클러스터 정보의 목록에 있음)도 learner처럼 다시 합류한다: 온라인이면 id를 가로챌 수 없고,
/// 오프라인이면 새 주소로 learner가 되며 목록에서 빠진다.
#[tokio::test]
async fn a_rejoining_observer_is_treated_like_a_learner() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
    // 키퍼가 관찰 노드로 내린 것처럼: 목록에 쓰고 멤버십에서 뺀다.
    let (_, infos) = env.cluster.list::<ClusterSpec>().unwrap();
    let info = &infos[0];
    let status = ClusterStatus {
        observers: [(node, "10.0.0.7:8007".to_string())].into(),
        ..ClusterStatus::default()
    };
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    env.cluster
        .request(opl_cluster::types::LeaderRequest::Remove { id: NodeId(node) })
        .await
        .unwrap();
    // `Remove`는 목록에서도 뺀다. 다시 목록에 둔다(멤버십 밖의 관찰 노드).
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    assert!(env.cluster.members().observers.contains_key(&NodeId(node)));

    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    req.address = "10.0.0.8:8006".into();
    req.cluster_address = "10.0.0.8:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");

    mark_offline(&env, node).await;
    req.token = new_token(&env).await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = env.cluster.members();
    assert_eq!(
        members.learners.get(&NodeId(node)).map(String::as_str),
        Some("10.0.0.8:8007")
    );
    assert!(members.observers.is_empty());
    let (_, infos) = env.cluster.list::<ClusterSpec>().unwrap();
    assert!(infos[0].status.observers.is_empty());
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

    let (mut req, node) = join_request(&new_token(&env).await, "node2");
    // 다른 노드의 링크로는 합류하지 못한다.
    req.cluster_address = "10.0.0.8:8007".into();
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
            roles: None,
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
    req.cluster_address = "10.0.0.8:8007".into();
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
    let (mut req, _) = join_request(&token, "node3");
    req.cluster_address = "10.0.0.8:8007".into();
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

/// 노드의 기록된 기능에서 `feature`를 더하거나(`on`) 뺀다(옛 노드).
async fn set_feature(env: &Env, node: Uuid, feature: &str, on: bool) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.features.retain(|f| f != feature);
    if on {
        status.features.push(feature.into());
    }
    env.cluster
        .update_status::<NodeSpec>(node, &status)
        .await
        .unwrap();
}

/// 여러 링크로 합류: 모든 노드가 `multi-link`를 적었을 때만 받고(아니면 토큰을 쓰지 않고 409), 링크 목록 전체가
/// 멤버십과 인증서 이름에 들어간다. 다시 합류할 때 링크 목록이 바뀌면(첫 링크가 같아도) 그 목록으로 다시 더한다.
#[tokio::test]
async fn a_node_joins_with_several_links() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let links = vec!["10.0.0.7:8007".to_string(), "10.1.0.7:8007".into()];
    let (mut req, node) = join_request(&new_token(&env).await, "node2");
    req.cluster_addresses = links.clone();
    set_feature(&env, env.node, "multi-link", false).await;
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert!(env.cluster.members().learners.is_empty());

    set_feature(&env, env.node, "multi-link", true).await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        cert_names(body["cert_pem"].as_str().unwrap()).unwrap(),
        ["10.0.0.7", "10.1.0.7", "localhost", "127.0.0.1", "::1"]
    );
    let members = env.cluster.members();
    assert_eq!(members.addr(NodeId(node)), Some("10.0.0.7:8007"));
    assert_eq!(members.links[&NodeId(node)], links);
    let (status, view) = env.admin().await.get("/cluster").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let me = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == node.to_string())
        .unwrap_or_else(|| panic!("the node is not listed: {view}"))
        .clone();
    assert_eq!(me["cluster_address"], "10.0.0.7:8007");
    assert_eq!(me["cluster_addresses"], json!(links));

    // 첫 링크는 그대로, 둘째 링크만 바꿔 다시 합류한다. 등록 전의 자기 기록(기능 없음)은 세지 않는다.
    mark_offline(&env, node).await;
    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    req.cluster_addresses = vec!["10.0.0.7:8007".into(), "10.2.0.7:8007".into()];
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = env.cluster.members();
    assert_eq!(members.learners.len(), 1);
    assert_eq!(members.links[&NodeId(node)], req.cluster_addresses);
    // 기록된 링크도 이번 것이다(옛 링크가 남으면 리더의 링크 맞추기가 멤버십을 되돌린다).
    let recorded = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    assert_eq!(recorded.status.cluster_addresses, req.cluster_addresses);
}

/// 다른 노드의 링크(같은 주소의 다른 표기 포함)로는 합류하지 못한다(그 노드에 갈 호출을 받게 된다).
#[tokio::test]
async fn a_link_of_another_node_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.cluster_address = "[fd00::7]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let token = new_token(&env).await;
    let (mut req, _) = join_request(&token, "node3");
    req.address = "10.0.0.8:8006".into();
    req.cluster_address = "[fd00:0::7]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("already a link"),
        "{body}"
    );
    assert_eq!(env.cluster.members().learners.len(), 1);
    // 토큰은 쓰지 않았다.
    let (mut req, _) = join_request(&token, "node3");
    req.cluster_address = "[fd00::8]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 다른 노드가 원하는(Node 기록의 `cluster_addresses`, 아직 멤버십에 없는) 링크로도 합류하지 못한다.
#[tokio::test]
async fn a_link_another_node_wants_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mine = env.cluster.members().voters[&NodeId(env.node)].clone();
    node.status.cluster_addresses = vec![mine, "10.9.0.1:8007".into()];
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();

    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.cluster_address = "10.9.0.1:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("already a link"),
        "{body}"
    );
    assert!(env.cluster.members().learners.is_empty());
}

/// 링크 목록은 첫 것이 `cluster_address`이고 겹치지 않으며 모두 SAN에 넣을 수 있는 호스트여야 한다(기능 검사보다
/// 먼저 본다).
#[tokio::test]
async fn links_must_start_with_the_cluster_address() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let token = new_token(&env).await;
    for links in [
        ["10.1.0.7:8007", "10.0.0.7:8007"],
        ["10.0.0.7:8007", "10.0.0.7:8007"],
        ["10.0.0.7:8007", "evil host/x:8007"],
        ["10.0.0.7:8007", "10.1.0.7"],
    ] {
        let (mut req, _) = join_request(&token, "node2");
        req.cluster_addresses = links.iter().map(|l| l.to_string()).collect();
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{links:?}: {body}");
        assert_eq!(body["code"], "invalid_request");
    }
    // 겹침은 값으로 본다(IP는 같은 주소, 이름은 대소문자 없이).
    for links in [
        ["[fd00::7]:8007", "[fd00:0::7]:8007"],
        ["node2.lan:8007", "NODE2.lan:8007"],
    ] {
        let (mut req, _) = join_request(&token, "node2");
        req.cluster_address = links[0].into();
        req.cluster_addresses = links.iter().map(|l| l.to_string()).collect();
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{links:?}: {body}");
    }
    // 링크 하나만 적으면 옛 요청과 같다(기능이 필요 없다).
    let (mut req, _) = join_request(&token, "node2");
    req.cluster_addresses = vec!["10.0.0.7:8007".into()];
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
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

/// 모든 응답에 답한 노드의 적용 리비전(`Orbivisor-Revision`)이 붙는다. `Orbivisor-Min-Revision`을 주면 그 리비전까지
/// 적용한 뒤 답하고, 5초 안에 안 되면 503 `revision_unavailable`이다.
#[tokio::test]
async fn reads_wait_for_the_min_revision_header() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let get = |min: Option<String>| {
        let mut r = admin
            .http
            .get(format!("{}/api/v1/volumes", admin.base))
            .bearer_auth(admin.token.as_deref().unwrap());
        if let Some(min) = min {
            r = r.header("Orbivisor-Min-Revision", min);
        }
        r.send()
    };
    let revision = |resp: &reqwest::Response| -> u64 {
        resp.headers()["orbivisor-revision"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let resp = get(None).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let now = revision(&resp);
    assert!(now >= 1 && now <= env.cluster.revision());

    // 아직 적용하지 않은 리비전: 쓰기가 적용되면 답한다.
    let want = env.cluster.revision() + 1;
    let cluster = env.cluster.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cluster
            .create(&Resource::new(
                "late",
                opl_types::storage::VolumeSpec {
                    pool: "local".into(),
                    size_bytes: 1 << 20,
                    format: opl_types::runtime::ImageFormat::Qcow2,
                },
            ))
            .await
            .unwrap()
    });
    let resp = get(Some(want.to_string())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(revision(&resp) >= want);
    writer.await.unwrap();

    let resp = get(Some("soon".into())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let far = env.cluster.revision() + 1_000_000;
    let started = std::time::Instant::now();
    let resp = get(Some(far.to_string())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() >= std::time::Duration::from_secs(4));
    assert!(revision(&resp) < far);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "revision_unavailable", "{body}");
    // 헬스도 헤더의 오류를 짧은 응답으로 바꾸지 않고 그대로 돌려준다.
    let resp = admin
        .http
        .get(format!("{}/api/v1/health", admin.base))
        .bearer_auth(admin.token.as_deref().unwrap())
        .header("Orbivisor-Min-Revision", "soon")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 인증되지 않은 요청은 기다리지 않고(곧바로 401) 리비전도 알려 주지 않는다.
    let started = std::time::Instant::now();
    let resp = admin
        .http
        .get(format!("{}/api/v1/volumes", admin.base))
        .header("Orbivisor-Min-Revision", far.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(resp.headers().get("orbivisor-revision").is_none());
    let resp = admin
        .http
        .get(format!("{}/api/v1/openapi.json", admin.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("orbivisor-revision").is_none());
}
