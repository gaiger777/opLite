//! 스토리지 풀 API: 백엔드별 값 검사, 볼륨 파일 작업을 할 노드.

mod common;

use common::Env;
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn pool(name: &str, backend: Value) -> Value {
    json!({ "name": name, "spec": { "backend": backend } })
}

#[tokio::test]
async fn rejects_bad_backend_values() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let nfs =
        |server: &str, export: &str| json!({ "kind": "nfs", "server": server, "export": export });
    for (bad, why) in [
        (
            pool("rel", json!({ "kind": "dir", "path": "pool" })),
            "relative path",
        ),
        (pool("srv", nfs("nas:2049", "/srv/vm")), "server with ':'"),
        (pool("exp", nfs("nas", "srv/vm")), "relative export"),
        (
            pool(
                "opt",
                json!({ "kind": "nfs", "server": "nas", "export": "/srv/vm", "options": "vers=4.2, hard" }),
            ),
            "options with space",
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}");
    }
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("dir", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn nfs_pools_are_always_shared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let nfs = json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" });
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin.post("/storage-pools", pool("nas", nfs.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "nfs", "{body}");
    // 공유가 아니라고 적으면 거절한다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "nas2", "spec": { "backend": nfs, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("shared"),
        "{body}"
    );
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "nas2", "spec": { "backend": nfs, "shared": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // dir 풀은 `shared`를 적은 대로 둔다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("dir", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    // 모르는 필드는 전처럼 거절한다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "typo", "spec": { "backend": nfs, "shard": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
}

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
async fn other_node(env: &Env, name: &str, online: bool, pools: &[&str]) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    node.status.online = online;
    for pool in pools {
        let id = env
            .cluster
            .get_by_name::<StoragePoolSpec>(pool)
            .unwrap()
            .unwrap()
            .id;
        let state = PoolState {
            pool_id: id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert((*pool).into(), state);
    }
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 볼륨 파일 작업을 맡은 노드는 이 노드에서 풀이 준비됐으면 이 노드, 아니면 풀이 준비된 온라인
/// 노드다(디렉터리 풀은 상태를 아직 알리지 않은 노드도 쓸 수 있다고 본다). 공유가 아닌 풀의 볼륨은 파일이 있는 노드에서 지운다.
#[tokio::test]
async fn volume_tasks_go_to_a_node_where_the_pool_is_ready() {
    let mut env = Env::start().await;
    // Task는 실행하지 않고 맡은 노드만 본다.
    env.stop_runner().await;
    let admin = env.admin().await;
    for (name, backend) in [
        ("local", json!({ "kind": "dir", "path": env.dir.path() })),
        (
            "nas",
            json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", pool(name, backend)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let create = |name: &str, pool: &str| {
        let body = json!({
            "name": name,
            "spec": { "pool": pool, "size_bytes": 1 << 20, "format": "qcow2" }
        });
        let admin = &admin;
        async move { admin.post("/volumes", body).await }
    };
    let task_node = |task: &Value| task["spec"]["node"].as_str().unwrap().to_string();

    // 어느 노드도 풀 상태를 알리지 않았다. NFS 풀은 쓸 수 없고, 디렉터리 풀은 쓸 수 있다고 본다.
    let (status, body) = create("x", "nas").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_unavailable");
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);
    let (status, task) = create("d", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), env.node.to_string());

    // 이 노드가 디렉터리 풀이 준비되지 않았다고 알리면 준비된 다른 노드다. 오프라인 노드는
    // 준비됐다고 알렸어도 고르지 않는다.
    let down = PoolState {
        ready: false,
        error: Some("no such directory".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "local", down).await;
    other_node(&env, "off", false, &["local", "nas"]).await;
    let other = other_node(&env, "n2", true, &["local", "nas"]).await;
    let (status, task) = create("a", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
    let (_, volume) = admin.get("/volumes/a").await;
    assert_eq!(volume["status"]["node"], other.to_string());

    // 이 노드에서 준비되면 이 노드다.
    env.pool_ready("local").await;
    let (status, task) = create("b", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), env.node.to_string());

    // 공유 풀은 이 노드에서 준비되지 않았으면 준비된 노드다.
    let (status, task) = create("c", "nas").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());

    // 지우기: 공유가 아닌 풀의 볼륨은 파일이 있는 노드, 공유 풀의 볼륨은 풀이 준비된 노드다.
    for (name, node) in [("a", other), ("c", env.node)] {
        let mut v = env
            .cluster
            .get_by_name::<VolumeSpec>(name)
            .unwrap()
            .unwrap();
        v.status.phase = VolumePhase::Ready;
        v.status.node = Some(node);
        env.cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
    }
    let (status, task) = admin.delete("/volumes/a").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
    let (status, task) = admin.delete("/volumes/c").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
}
