//! 스토리지 풀 API: 백엔드별 값 검사, 볼륨 파일 작업을 할 노드.

mod common;

use common::{Env, vm_request};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState};
use opl_types::vm::VmSpec;
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
        (
            pool(
                "vg",
                json!({ "kind": "lvm-thin", "vg": "-vg", "thinpool": "tp" }),
            ),
            "vg starting with '-'",
        ),
        (
            pool(
                "tp",
                json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "a/b" }),
            ),
            "thin pool with '/'",
        ),
        (
            pool("z", json!({ "kind": "zfs", "dataset": "tank/-opl" })),
            "dataset component starting with '-'",
        ),
        (
            pool("z", json!({ "kind": "zfs", "dataset": "tank/opl@s" })),
            "dataset with '@'",
        ),
        (
            pool("r", json!({ "kind": "rbd", "pool": "-rbd" })),
            "rbd pool starting with '-'",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "namespace": "a/b" }),
            ),
            "rbd namespace with '/'",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "conf": "/etc/ceph/a,b.conf" }),
            ),
            "rbd conf with ','",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "user": "client.admin" }),
            ),
            "ceph user with client.",
        ),
        (
            pool("s", json!({ "kind": "lvm-shared", "vg": "-san" })),
            "shared vg starting with '-'",
        ),
        (
            pool(
                "s",
                json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "fd00::5", "target": "iqn.2003-01.org:opl" } }),
            ),
            "ipv6 portal without brackets",
        ),
        (
            pool(
                "s",
                json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "san", "target": "opl" } }),
            ),
            "target without iqn.",
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

/// LVM-thin 풀은 노드 로컬이고 raw 볼륨만 담는다.
#[tokio::test]
async fn lvm_thin_pools_are_node_local_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    // Task는 실행하지 않고 받아들였는지만 본다.
    env.stop_runner().await;
    let admin = env.admin().await;
    let lvm = json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "vg2", "spec": { "backend": lvm, "shared": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("node-local"),
        "{body}"
    );
    let (status, body) = admin.post("/storage-pools", pool("vg", lvm.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "lvm-thin", "{body}");
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "vg3", "spec": { "backend": lvm, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("vg").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "vg", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "lvm-thin pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // 가져오기도 같다. 형식을 적지 않으면 풀 종류를 따라 raw다.
    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=vg&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "lvm-thin pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=vg&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// ZFS 풀도 노드 로컬이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn zfs_pools_are_node_local_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let zfs = json!({ "kind": "zfs", "dataset": "tank/opl" });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "z2", "spec": { "backend": zfs, "shared": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"], "zfs pools are node-local; shared must be false",
        "{body}"
    );
    let (status, body) = admin.post("/storage-pools", pool("z", zfs.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "zfs", "{body}");
    assert_eq!(body["spec"]["backend"]["dataset"], "tank/opl", "{body}");
    env.pool_ready("z").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "z", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "zfs pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=z&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "zfs pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=z&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// RBD 풀은 늘 공유이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn rbd_pools_are_always_shared_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let rbd = json!({ "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl" });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "r2", "spec": { "backend": rbd, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "rbd pools are always shared", "{body}");
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin.post("/storage-pools", pool("r", rbd.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(
        body["spec"]["backend"],
        json!({ "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl", "conf": null }),
        "{body}"
    );
    env.pool_ready("r").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "r", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "rbd pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=r&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "rbd pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=r&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// RBD 풀의 볼륨 Task(만들기, 지우기, 쓰는 VM이 없는 크기 늘리기)는 이 노드에서 풀이 준비됐어도 풀이
/// 준비된 온라인 노드 가운데 id가 가장 작은 노드가 맡는다. 바꾸는 명령이 아직 돌고 있다는 표시가
/// 노드마다 따로이므로 한 볼륨의 Task가 늘 같은 노드에서 돌게 한다.
#[tokio::test]
async fn rbd_volume_tasks_go_to_the_lowest_ready_node() {
    volume_tasks_go_to_the_lowest_ready_node(json!({ "kind": "rbd", "pool": "rbd" })).await;
}

/// 공유 LVM 풀의 볼륨 Task도 RBD처럼 id가 가장 작은 준비된 온라인 노드가 맡는다.
#[tokio::test]
async fn lvm_shared_volume_tasks_go_to_the_lowest_ready_node() {
    volume_tasks_go_to_the_lowest_ready_node(json!({ "kind": "lvm-shared", "vg": "san" })).await;
}

/// 공유 블록 풀 `backend`(이름 `ceph`)의 볼륨 Task가 id가 가장 작은 준비된 온라인 노드로 가는지 본다.
async fn volume_tasks_go_to_the_lowest_ready_node(backend: Value) {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/storage-pools", pool("ceph", backend)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("ceph").await;
    let mut low: Node = Resource::new(
        "low",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    low.id = Uuid::nil();
    env.cluster.create(&low).await.unwrap();
    low.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(low.id, &low.status)
        .await
        .unwrap();
    common::mark_pool_ready(&env.cluster, low.id, "ceph").await;
    let low = low.id.to_string();

    let body = json!({
        "name": "r",
        "spec": { "pool": "ceph", "size_bytes": 1 << 20, "format": "raw" }
    });
    let (status, task) = admin.post("/volumes", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
    let mut v = env.cluster.get_by_name::<VolumeSpec>("r").unwrap().unwrap();
    v.status.phase = VolumePhase::Ready;
    v.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(v.id, &v.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/volumes/r/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
    let (status, task) = admin.delete("/volumes/r").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
}

/// 공유 LVM 풀은 늘 공유이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn lvm_shared_pools_are_always_shared_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let backend = json!({
        "kind": "lvm-shared", "vg": "san",
        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.linux-iscsi.san:opl" }
    });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "s2", "spec": { "backend": backend, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"], "lvm-shared pools are always shared",
        "{body}"
    );
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin
        .post("/storage-pools", pool("s", backend.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"], backend, "{body}");
    env.pool_ready("s").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "s", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "lvm-shared pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=s&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "lvm-shared pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=s&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
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

    // 파일이 있던 노드가 클러스터에서 빠졌으면 이 노드다(리소스만 지운다).
    let mut v = env.cluster.get_by_name::<VolumeSpec>("b").unwrap().unwrap();
    v.status.phase = VolumePhase::Ready;
    v.status.node = Some(Uuid::new_v4());
    env.cluster
        .update_status::<VolumeSpec>(v.id, &v.status)
        .await
        .unwrap();
    let (status, task) = admin.delete("/volumes/b").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), env.node.to_string());
}

/// `PATCH /storage-pools/{key}`는 디렉터리 풀의 `shared`만 바꾼다. 관리자만, 같은 값이면 그대로 200.
#[tokio::test]
async fn patch_changes_shared_of_dir_pools() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("local", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let operator = env.user("operator", "operator").await;
    let (status, _) = operator
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 로컬 → 공유는 언제나 된다. 라벨과 백엔드는 그대로다.
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "dir", "{body}");
    let (_, got) = admin.get("/storage-pools/local").await;
    assert_eq!(got["spec"]["shared"], true, "{got}");
    assert_eq!(got["revision"], body["revision"], "{got}");
    // 같은 값이면 쓰지 않고 그대로 200이다.
    let revision = body["revision"].as_u64().unwrap();
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], revision, "{body}");
    // revision이 다르면 412, 맞으면 바꾼다. id로도 찾는다.
    let (status, body) = admin
        .patch(
            "/storage-pools/local",
            json!({ "shared": false }),
            Some(revision + 1),
        )
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    let id = got["id"].as_str().unwrap();
    let (status, body) = admin
        .patch(
            &format!("/storage-pools/{id}"),
            json!({ "shared": false }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");

    // 모르는 필드, 빠진 필드, 없는 풀
    for bad in [json!({ "shared": true, "nodes": [] }), json!({})] {
        let (status, body) = admin.patch("/storage-pools/local", bad, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, _) = admin
        .patch("/storage-pools/nope", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 다른 백엔드는 바꿀 수 없다(같은 값이어도).
    for (name, backend, shared) in [
        (
            "nas",
            json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
            false,
        ),
        ("r", json!({ "kind": "rbd", "pool": "rbd" }), true),
        (
            "vg",
            json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }),
            true,
        ),
        ("z", json!({ "kind": "zfs", "dataset": "tank/opl" }), false),
    ] {
        let (status, body) = admin.post("/storage-pools", pool(name, backend)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, body) = admin
            .patch(
                &format!("/storage-pools/{name}"),
                json!({ "shared": shared }),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["code"], "invalid_request", "{name}");
        assert!(
            body["message"].as_str().unwrap().contains("dir pools"),
            "{body}"
        );
    }
}

/// 공유 → 로컬은 그 풀의 볼륨을 쓰는 VM이 볼륨을 만든 노드가 아닌 노드에 있거나 마이그레이션
/// Task가 끝나지 않았으면 409 `pool_in_use`다.
#[tokio::test]
async fn patch_to_local_needs_vms_on_their_volume_nodes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    for name in ["nas", "other"] {
        let backend = json!({ "kind": "dir", "path": env.dir.path().join(name) });
        let (status, body) = admin
            .post(
                "/storage-pools",
                json!({ "name": name, "spec": { "backend": backend, "shared": true } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        env.pool_ready(name).await;
    }
    let node2 = other_node(&env, "node2", true, &["nas", "other"]).await;
    // 이 노드가 만든 볼륨과 그 볼륨을 디스크로 쓰는 꺼진 VM
    for (volume, pool, vm) in [("root", "nas", "web"), ("data", "other", "db")] {
        let mut v: Volume = Resource::new(
            volume,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Qcow2,
            },
        );
        v.status.node = Some(env.node);
        env.cluster.create(&v).await.unwrap();
        v.status.phase = VolumePhase::Ready;
        env.cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
        let mut req = vm_request(vm);
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        req["spec"]["node"] = json!(env.node);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let set_node = |vm: &'static str, node: Uuid| {
        let cluster = env.cluster.clone();
        async move {
            let v = cluster.get_by_name::<VmSpec>(vm).unwrap().unwrap();
            let mut spec = v.spec.clone();
            spec.node = Some(node);
            cluster
                .update_spec(v.id, None, v.labels.clone(), &spec)
                .await
                .unwrap();
        }
    };
    // 다른 풀의 VM이 다른 노드로 옮겨 갔어도 상관없다.
    set_node("db", node2).await;
    let to_local = || admin.patch("/storage-pools/nas", json!({ "shared": false }), None);
    let to_shared = || admin.patch("/storage-pools/nas", json!({ "shared": true }), None);
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    let (status, body) = to_shared().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 공유 풀이라 옮겨 간 VM이 있으면 409(VM 이름)
    set_node("web", node2).await;
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"web\""),
        "{body}"
    );
    let (_, got) = admin.get("/storage-pools/nas").await;
    assert_eq!(got["spec"]["shared"], true, "{got}");

    // 볼륨을 만든 노드로 돌아오면 된다. 다시 공유로.
    set_node("web", env.node).await;
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = to_shared().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 끝나지 않은 마이그레이션 Task가 있어도 409
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    let mut task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmMigrate { node: node2 },
            target: web.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.revision = env.cluster.create(&task).await.unwrap();
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"web\""),
        "{body}"
    );
    // 끝난 Task는 상관없다.
    task.status.state = TaskState::Failed;
    env.cluster
        .update_status::<TaskSpec>(task.id, &task.status)
        .await
        .unwrap();
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
}
