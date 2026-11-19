//! 계획 64 API: NUMA·전용 CPU·hugepages의 기능 게이트(스테이지 1 — API를 열기 전에는 VM·노드의 새 칸을 받지 않는다),
//! 전용 CPU·hugepages의 배치(스테이지 3 — 스케줄러의 NUMA 맞춤, 고정 노드 켜기, 껐다 켜기, Raft 장부). API가 새 칸을
//! 열기 전이라 VM·노드는 저장소에 바로 쓴다.

mod common;

use common::{Env, vm_request};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::node::{FEATURES, NUMA, Node, NodeSpec, NodeStatus};
use opl_types::numa::{CpuPolicy, HostResource, HostResources, NodeTopology};
use opl_types::resource::Resource;
use opl_types::vm::{Arch, PowerState, RunningHardware, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 이 노드가 `numa`를 모르는 것으로 적는다(옛 노드처럼).
async fn drop_numa(env: &Env) {
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.features.retain(|f| f != NUMA);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 새 칸 하나씩 든 VM 만들기 요청
fn tries() -> Vec<Value> {
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut req = vm_request("new");
        edit(&mut req["spec"]);
        req
    };
    vec![
        with(&|s| s["cpu"]["policy"] = json!("dedicated")),
        with(&|s| {
            s["cpu"]["policy"] = json!("dedicated");
            s["cpu"]["thread_policy"] = json!("isolate");
            s["cpu"]["emulator"] = json!("isolate");
        }),
        with(&|s| s["numa"] = json!({ "nodes": 1 })),
        with(&|s| s["numa"] = json!({ "nodes": "auto", "memory": "strict" })),
        with(&|s| s["hugepages"] = json!("2M")),
        with(&|s| s["hugepages"] = json!("any")),
    ]
}

#[tokio::test]
async fn numa_fields_are_gated() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 쓰지 않는 VM은 지금처럼 되고 새 칸이 없다.
    let (status, body) = admin.post("/vms", vm_request("plain")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    for k in ["numa", "hugepages"] {
        assert!(body["spec"].get(k).is_none(), "{body}");
    }
    for k in ["policy", "thread_policy", "emulator"] {
        assert!(body["spec"]["cpu"].get(k).is_none(), "{body}");
    }
    // 모든 노드가 알려도 스테이지 5 전에는 400이다.
    for req in tries() {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{req}: {body}");
    }
    // 바꾸기도 같다.
    let (_, vm) = admin.get("/vms/plain").await;
    let mut spec = vm["spec"].clone();
    spec["hugepages"] = json!("1G");
    let (status, body) = admin.put("/vms/plain", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 노드 호스트 자원 설정
    let node = format!("/nodes/{}", env.node);
    let (_, n) = admin.get(&node).await;
    let mut nspec = n["spec"].clone();
    let (status, body) = admin
        .put(&node, json!({ "spec": nspec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("host_resources").is_none(), "{body}");
    nspec["host_resources"] = json!({ "dedicated_cpus": "2-3" });
    let (status, body) = admin
        .put(&node, json!({ "spec": nspec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // 폴더 할당량 dedicated_vcpus는 기능만 본다(상태 머신이 이미 센다).
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "db", "kind": "vm", "quota": { "dedicated_vcpus": 8 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["quota"]["dedicated_vcpus"], 8, "{body}");
    let folder = body["id"].as_str().unwrap().to_string();

    // 옛 노드가 있으면 검증보다 먼저 409다.
    drop_numa(&env).await;
    for req in tries() {
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{req}: {body}");
        assert_eq!(body["code"], "cluster_upgrading", "{body}");
    }
    let (status, body) = admin.put(&node, json!({ "spec": nspec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading", "{body}");
    let (status, body) = admin
        .post(
            "/folders",
            json!({ "name": "db2", "kind": "vm", "quota": { "dedicated_vcpus": 8 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vcpus": 4, "dedicated_vcpus": 2 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // 옛 칸만이면 된다.
    let (status, body) = admin
        .patch(
            &format!("/folders/{folder}"),
            json!({ "quota": { "vcpus": 4 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = admin.post("/vms", vm_request("plain2")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // 문법이 틀린 칸은 JSON을 풀지 못한다.
    for bad in [
        json!({ "cpu": { "cores": 1, "policy": "exclusive" } }),
        json!({ "hugepages": "4K" }),
        json!({ "numa": { "nodes": "two" } }),
    ] {
        let mut req = vm_request("bad");
        for (k, v) in bad.as_object().unwrap() {
            req["spec"][k] = v.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert!(status.is_client_error(), "{bad}: {status} {body}");
    }
}

/// CPU 0-7(호스트 노드 하나)에 전용 `dedicated`인 aarch64 노드(다른 노드처럼 저장소에만 있다 — VM은 켜지지 않는다)
async fn add_numa_node(env: &Env, name: &str, dedicated: &str) -> Uuid {
    let topology: NodeTopology = serde_json::from_value(json!({
        "sockets": 1, "threads_per_core": 1, "cpus": "0-7", "pinning": true, "cpuset": true,
        "probed_at": 1, "nodes": [{"id": 0, "cpus": "0-7", "memory_mib": 16384}],
    }))
    .unwrap();
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
            host_resources: Some(HostResources {
                reserved_cpus: Default::default(),
                dedicated_cpus: dedicated.parse().unwrap(),
                hugepages: Vec::new(),
            }),
        },
    );
    node.status = NodeStatus {
        topology: Some(topology),
        online: true,
        cpus: 8,
        memory_mib: 16384,
        hypervisor: "qemu-kvm".into(),
        arch: Some(Arch::Aarch64),
        version: env
            .cluster
            .get::<NodeSpec>(env.node)
            .unwrap()
            .unwrap()
            .status
            .version,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        ..NodeStatus::default()
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// `cores`개 vCPU의 전용 VM 스펙
fn dedicated(cores: u16, node: Option<Uuid>, power: PowerState) -> VmSpec {
    let mut req = vm_request("x")["spec"].clone();
    req["cpu"]["cores"] = json!(cores);
    req["cpu"]["policy"] = json!("dedicated");
    req["node"] = json!(node);
    req["power"] = json!(power);
    serde_json::from_value(req).unwrap()
}

async fn create(env: &Env, name: &str, spec: VmSpec) -> Vm {
    let vm: Vm = Resource::new(name, spec);
    env.cluster.create(&vm).await.unwrap();
    vm
}

fn node_of(env: &Env, name: &str) -> Option<Uuid> {
    env.cluster
        .get_by_name::<VmSpec>(name)
        .unwrap()
        .unwrap()
        .spec
        .node
}

/// 켜기는 전용 CPU가 남은 노드에만 놓고(Mock 노드는 토폴로지가 없어 빠진다), 다 차면 409 `no_node`와 노드마다 까닭이다.
/// 꽉 찬 노드에 고정된 꺼진 VM은 자리가 있는 노드로 옮겨 켠다.
#[tokio::test]
async fn starts_place_dedicated_vms_where_cpus_are_free() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let n1 = add_numa_node(&env, "n1", "4-5").await;
    let n2 = add_numa_node(&env, "n2", "4-7").await;
    for name in ["a", "b", "c", "d"] {
        create(&env, name, dedicated(2, None, PowerState::Stopped)).await;
    }
    for name in ["a", "b", "c"] {
        let (status, body) = admin
            .post(&format!("/vms/{name}/actions/start"), json!({}))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{name}: {body}");
    }
    let placed: Vec<Option<Uuid>> = ["a", "b", "c"].iter().map(|n| node_of(&env, n)).collect();
    assert_eq!(
        placed.iter().filter(|n| **n == Some(n1)).count(),
        1,
        "{placed:?}"
    );
    assert_eq!(
        placed.iter().filter(|n| **n == Some(n2)).count(),
        2,
        "{placed:?}"
    );
    let (status, body) = admin.post("/vms/d/actions/start", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("has room for its dedicated CPUs, hugepages and NUMA layout"),
        "{body}"
    );
    assert_eq!(
        body["details"]["nodes"]["n1"],
        "node \"n1\" has 0 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one",
        "{body}"
    );
    assert!(
        body["details"]["nodes"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| v.as_str().unwrap().contains("(no topology)")),
        "{body}"
    );
    // 공유 VM은 상관없다.
    let (status, body) = admin.post("/vms", vm_request("plain")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // n2의 하나를 끄고, n1(꽉 참)에 고정된 꺼진 VM을 켜면 n2로 간다.
    let on_n2 = ["a", "b", "c"]
        .into_iter()
        .find(|n| node_of(&env, n) == Some(n2))
        .unwrap();
    let mut off = env.cluster.get_by_name::<VmSpec>(on_n2).unwrap().unwrap();
    off.spec.power = PowerState::Stopped;
    env.cluster
        .update_spec(off.id, None, off.labels.clone(), &off.spec)
        .await
        .unwrap();
    create(&env, "e", dedicated(2, Some(n1), PowerState::Stopped)).await;
    let (status, body) = admin.post("/vms/e/actions/start", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(node_of(&env, "e"), Some(n2));
}

/// 껐다 켜기는 같은 노드에서 새 스펙이 들어가야 한다: 전용 vCPU가 그 노드의 자리보다 많으면 끄기 전에 409
/// `no_capacity`다(계획 63의 CPU 확인과 같은 자리). Raft 장부는 늘리는 쓰기를 409 `no_capacity`(노드·자원·셈)로 막는다.
#[tokio::test]
async fn a_power_cycle_needs_room_for_the_new_dedicated_cpus() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let n1 = add_numa_node(&env, "n1", "4-7").await;
    let mut vm = create(&env, "db", dedicated(2, Some(n1), PowerState::Running)).await;
    vm.status.phase = VmPhase::Running;
    vm.status.node = Some(n1);
    vm.status.hardware = Some(RunningHardware::legacy(&vm.spec));
    env.cluster
        .update_status::<VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();
    let (status, body) = admin.post("/vms/db/actions/reboot", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let mut now = env.cluster.get_by_name::<VmSpec>("db").unwrap().unwrap();
    assert_eq!(now.spec.cpu.policy, CpuPolicy::Dedicated);
    now.spec.cpu.cores = 5;
    let err = env
        .cluster
        .update_spec(now.id, None, now.labels.clone(), &now.spec)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ClusterError::Rejected(Rejection::HostResourcesExceeded {
                resource: HostResource::DedicatedCpus,
                limit: 4,
                used: 5,
                ..
            })
        ),
        "{err:?}"
    );
    let api = opl_api::error::ApiError::from(err);
    assert_eq!(api.code, "no_capacity");
    assert_eq!(
        api.details,
        Some(json!({"node": n1, "resource": "dedicated_cpus", "limit": 4, "used": 5}))
    );
    now.spec.cpu.cores = 4;
    env.cluster
        .update_spec(now.id, None, now.labels.clone(), &now.spec)
        .await
        .unwrap();
    let (status, body) = admin.post("/vms/db/actions/reboot", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // 전용 집합이 줄면(스테이지 5의 force) 새 스펙이 들지 않아 끄지 않는다.
    let mut node = env.cluster.get::<NodeSpec>(n1).unwrap().unwrap();
    node.spec.host_resources.as_mut().unwrap().dedicated_cpus = "4-6".parse().unwrap();
    env.cluster
        .update_spec(n1, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    let (status, body) = admin.post("/vms/db/actions/reboot", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_capacity", "{body}");
    assert_eq!(
        body["message"],
        "node \"n1\" has 3 free dedicated CPUs on host NUMA node 0; the VM needs 4 on one; migrate VM \"db\" to a node that can run its CPU, then power cycle it",
        "{body}"
    );
    let db = env.cluster.get_by_name::<VmSpec>("db").unwrap().unwrap();
    assert_eq!(db.spec.power, PowerState::Running);
}
