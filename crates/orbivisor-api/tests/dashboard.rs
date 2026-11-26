//! 계획 75 운영자 대시보드 API: `GET /topology/physical`(포트 → 본딩 → 브리지 → 네트워크 → VM 사슬과 장애 영향)와
//! `GET /capacity`(노드별 CPU·메모리·스토리지 할당·사용). 권한(루트 `cluster.read`), 기능 게이트(`operator-dashboard`),
//! 입력이 같으면 같은 결과·바뀌면 바뀐 결과. 노드·네트워크·VM·볼륨은 저장소에 바로 쓴다(멤버가 아닌 노드 `svc01` —
//! 그 노드의 VM을 이 노드의 reconciler가 건드리지 않는다).

mod common;

use common::{Env, vm_request};
use orbivisor_types::network::Network;
use orbivisor_types::node::{
    FEATURES, NodeInterface, NodeSpec, NodeStatus, OPERATOR_DASHBOARD, PoolState,
};
use orbivisor_types::resource::Resource;
use orbivisor_types::storage::{VolumeSpec, VolumeStatus};
use orbivisor_types::vm::{VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// `eno1`·`eno2`를 묶은 `bond0`(eno2가 끊겨 저하) 위의 브리지 `vmbr0`
fn interfaces(eno2_up: bool) -> Vec<NodeInterface> {
    let eno2 = if eno2_up { "up" } else { "down" };
    serde_json::from_value(json!([
        { "name": "eno1", "kind": "ethernet", "state": "up", "carrier": true, "speed_mbps": 10000, "master": "bond0" },
        { "name": "eno2", "kind": "ethernet", "state": eno2, "carrier": eno2_up, "speed_mbps": 10000, "master": "bond0" },
        { "name": "bond0", "kind": "bond", "state": "up", "carrier": true, "master": "vmbr0",
          "bond": { "mode": "active-backup", "state": if eno2_up { "up" } else { "degraded" },
                    "ports": [ { "name": "eno1", "state": "up" }, { "name": "eno2", "state": eno2 } ] } },
        { "name": "vmbr0", "kind": "bridge", "state": "up", "carrier": true },
        // 쓰지 않는 포트(업링크가 아님)
        { "name": "eno3", "kind": "ethernet", "state": "down", "carrier": false }
    ]))
    .unwrap()
}

/// 멤버가 아닌 노드를 저장소에 만든다.
pub async fn add_node(env: &Env, name: &str, status: NodeStatus) -> Uuid {
    let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            sdn: None,
            host_resources: None,
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    r.status = status;
    env.cluster.create(&r).await.unwrap();
    r.id
}

/// 기능을 모두 아는 온라인 노드 상태(8스레드·16GiB)
pub fn node_status(interfaces: Vec<NodeInterface>) -> NodeStatus {
    NodeStatus {
        online: true,
        cpus: 8,
        memory_mib: 16_384,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        interfaces,
        ..NodeStatus::default()
    }
}

/// `node`에서 `phase`인 VM(코어 `cores`, 메모리 1024MiB, NIC는 `lan`)
async fn add_vm(env: &Env, name: &str, node: Uuid, cores: u32, phase: VmPhase) -> Uuid {
    let mut spec = vm_request(name)["spec"].clone();
    spec["cpu"]["cores"] = json!(cores);
    spec["memory_mib"] = json!(1024);
    spec["nics"] = json!([{ "network": "lan", "model": "virtio" }]);
    let mut vm: Resource<VmSpec, VmStatus> =
        Resource::new(name, serde_json::from_value(spec).unwrap());
    vm.status.phase = phase;
    vm.status.node = Some(node);
    env.cluster.create(&vm).await.unwrap();
    vm.id
}

async fn set_interfaces(env: &Env, node: Uuid, interfaces: Vec<NodeInterface>) {
    let mut n = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.interfaces = interfaces;
    env.cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

fn node_of<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == name)
        .unwrap_or_else(|| panic!("no node {name} in {body}"))
}

#[tokio::test]
async fn physical_topology_follows_the_chain_and_caches_until_the_input_changes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let svc01 = add_node(&env, "svc01", node_status(interfaces(false))).await;
    let lan: Network = Resource::new(
        "lan",
        serde_json::from_value(json!({ "bridge": "vmbr0" })).unwrap(),
    );
    env.cluster.create(&lan).await.unwrap();
    let web = add_vm(&env, "web", svc01, 2, VmPhase::Running).await;

    let (status, body) = admin.get("/topology/physical").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let n = node_of(&body, "svc01");
    assert_eq!(n["online"], true, "{n}");
    // 영향: eno2가 끊겨 bond0가 저하 — 위 브리지 vmbr0, 네트워크 lan, VM web
    assert_eq!(
        n["impact"],
        json!([{ "port": "eno2", "severity": "degraded", "bond": "bond0", "bridges": ["vmbr0"],
                 "networks": [lan.id], "vms": 1, "vm_ids": [web] }]),
        "{n}"
    );
    let roles: Vec<(&str, &str)> = n["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["name"].as_str().unwrap(), p["role"].as_str().unwrap()))
        .collect();
    assert!(roles.contains(&("eno2", "uplink")), "{roles:?}");
    assert!(roles.contains(&("eno3", "unused")), "{roles:?}");
    let bridge = n["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "vmbr0")
        .unwrap();
    assert_eq!(bridge["uplinks"], json!(["bond0"]), "{bridge}");
    assert_eq!(bridge["networks"], json!([lan.id]), "{bridge}");
    assert_eq!(bridge["vm_nics"], 1, "{bridge}");
    // 레일: svc01에서 lan은 저하, 브리지가 없는 이 노드(node1)는 없음
    let rail = &body["networks"][0];
    assert_eq!(rail["name"], "lan", "{body}");
    assert_eq!(rail["kind"], "bridge", "{rail}");
    assert_eq!(rail["carrier"], "vmbr0", "{rail}");
    assert!(
        rail["nodes"]
            .as_array()
            .unwrap()
            .contains(&json!({ "node": "svc01", "state": "degraded" })),
        "{rail}"
    );
    assert!(
        rail["nodes"]
            .as_array()
            .unwrap()
            .contains(&json!({ "node": "node1", "state": "absent" })),
        "{rail}"
    );

    // 입력이 같으면 같은 결과, 포트가 돌아오면 바뀐다.
    let (_, again) = admin.get("/topology/physical").await;
    assert_eq!(again, body);
    set_interfaces(&env, svc01, interfaces(true)).await;
    let (_, after) = admin.get("/topology/physical").await;
    // 장애가 없으면 `impact`를 빼고 보낸다.
    assert!(node_of(&after, "svc01").get("impact").is_none(), "{after}");
    assert!(
        after["networks"][0]["nodes"]
            .as_array()
            .unwrap()
            .contains(&json!({ "node": "svc01", "state": "up" })),
        "{after}"
    );

    // 권한: 루트 `cluster.read`가 없으면 403
    let nobody = env.user("nobody", "none").await;
    assert_eq!(
        nobody.get("/topology/physical").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(nobody.get("/capacity").await.0, StatusCode::FORBIDDEN);

    // 기능: 옛 노드가 있으면 409 `cluster_upgrading`
    let mut old = node_status(Vec::new());
    old.features.retain(|f| f != OPERATOR_DASHBOARD);
    add_node(&env, "old", old).await;
    for path in ["/topology/physical", "/capacity"] {
        let (status, body) = admin.get(path).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
}

#[tokio::test]
async fn capacity_counts_vcpus_memory_and_thin_provisioning() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let svc01 = add_node(&env, "svc01", node_status(Vec::new())).await;
    let lan: Network = Resource::new(
        "lan",
        serde_json::from_value(json!({ "bridge": "vmbr0" })).unwrap(),
    );
    env.cluster.create(&lan).await.unwrap();
    add_vm(&env, "web", svc01, 4, VmPhase::Running).await;
    add_vm(&env, "db", svc01, 8, VmPhase::Stopping).await;
    // 꺼진 VM은 세지 않는다.
    add_vm(&env, "off", svc01, 16, VmPhase::Stopped).await;
    // 로컬 풀: 100GiB 가운데 10GiB 남음, 볼륨 둘(씬 30GiB)
    let (status, pool) = admin
        .post(
            "/storage-pools",
            json!({ "name": "local", "spec": { "backend": { "kind": "dir", "path": "/srv/pool" }, "nodes": [svc01] } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{pool}");
    common::set_pool_state(
        &env.cluster,
        svc01,
        "local",
        PoolState {
            ready: true,
            total_bytes: 100 << 30,
            available_bytes: 10 << 30,
            ..Default::default()
        },
    )
    .await;
    for (name, gib) in [("d1", 20u64), ("d2", 10)] {
        let mut v: Resource<VolumeSpec, VolumeStatus> = Resource::new(
            name,
            serde_json::from_value(
                json!({ "pool": "local", "size_bytes": gib << 30, "format": "qcow2" }),
            )
            .unwrap(),
        );
        v.status.node = Some(svc01);
        env.cluster.create(&v).await.unwrap();
    }

    let (status, body) = admin.get("/capacity").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let n = node_of(&body, "svc01");
    assert_eq!(n["cpu"]["threads"], 8, "{n}");
    assert_eq!(n["cpu"]["vcpus_allocated"], 12, "{n}");
    assert_eq!(n["cpu"]["vcpus_running"], 4, "{n}");
    assert_eq!(n["cpu"]["overcommit"], 1.5, "{n}");
    assert_eq!(n["memory"]["total_mib"], 16_384, "{n}");
    assert_eq!(n["memory"]["allocated_mib"], 2048, "{n}");
    assert_eq!(
        n["storage"],
        json!([{ "pool": "local", "ready": true, "total_bytes": 100u64 << 30, "used_bytes": 90u64 << 30,
                 "provisioned_bytes": 30u64 << 30, "volumes": 2 }]),
        "{n}"
    );
    // 이 노드(node1)는 VM이 없다.
    assert_eq!(
        node_of(&body, "node1")["cpu"]["vcpus_allocated"],
        0,
        "{body}"
    );

    // 같은 입력은 같은 결과, VM이 늘면 바뀐다.
    let (_, again) = admin.get("/capacity").await;
    assert_eq!(again, body);
    add_vm(&env, "more", svc01, 2, VmPhase::Running).await;
    let (_, after) = admin.get("/capacity").await;
    assert_eq!(
        node_of(&after, "svc01")["cpu"]["vcpus_allocated"],
        14,
        "{after}"
    );
}
