//! 계획 64 API: NUMA·전용 CPU·hugepages의 검증(400 `invalid_numa`)과 기능 게이트, 권한 `vm.dedicated`, 할당량
//! `dedicated_vcpus`, 노드 `host_resources`(400 `invalid_host_resources`, 409 `host_cpus_in_use`·`hugepages_in_use`,
//! 칸이 없으면 그대로), `GET /vms/{key}/numa`·`/nodes/{key}/topology`·`/topology/usage`, 전용 CPU·hugepages의 배치
//! (스테이지 3 — 스케줄러의 NUMA 맞춤, 고정 노드 켜기, 껐다 켜기, Raft 장부). 토폴로지가 있는 노드는 저장소에만 있다
//! (Mock 노드는 토폴로지가 없다).

mod common;

use common::{Env, vm_request};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::node::{FEATURES, NUMA, Node, NodeSpec, NodeStatus};
use opl_types::numa::{
    CpuPolicy, GuestNode, HostPlacement, HostResource, HostResources, NodeTopology, PageKind,
    ResolvedNuma,
};
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

/// 새 칸은 모든 노드가 `numa`를 알 때 받고 검증한다(400 `invalid_numa`). 옛 노드가 있으면 검증보다 먼저 409다.
#[tokio::test]
async fn numa_fields_are_validated_and_gated() {
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
    for (i, mut req) in tries().into_iter().enumerate() {
        req["name"] = json!(format!("new{i}"));
        let (status, body) = admin.post("/vms", req.clone()).await;
        assert_eq!(status, StatusCode::CREATED, "{req}: {body}");
        for k in ["numa", "hugepages"] {
            assert_eq!(body["spec"].get(k), req["spec"].get(k), "{body}");
        }
    }
    let (_, db) = admin.get("/vms/new1").await;
    assert_eq!(db["spec"]["cpu"]["thread_policy"], "isolate", "{db}");
    // 맞지 않는 값은 400 invalid_numa
    for (edit, message) in [
        (
            json!({ "cpu": { "cores": 1, "thread_policy": "isolate" } }),
            "cpu.thread_policy needs cpu.policy dedicated",
        ),
        (
            json!({ "numa": { "nodes": 2 } }),
            "numa.nodes 2 must divide cpu.sockets 1: a guest NUMA node holds whole sockets",
        ),
        (
            json!({ "hugepages": "1G", "max_memory_mib": 2048, "memory_mib": 1024 }),
            "1G hugepages cannot be used with memory hotplug (max_memory_mib)",
        ),
    ] {
        let mut req = vm_request("bad");
        for (k, v) in edit.as_object().unwrap() {
            req["spec"][k] = v.clone();
        }
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{edit}: {body}");
        assert_eq!(body["code"], "invalid_numa", "{body}");
        assert_eq!(body["message"], message, "{body}");
    }
    // 바꾸기도 받는다(꺼진 VM).
    let (_, vm) = admin.get("/vms/plain").await;
    let mut spec = vm["spec"].clone();
    spec["hugepages"] = json!("2M");
    let (status, body) = admin.put("/vms/plain", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["hugepages"], "2M", "{body}");
    // 폴더 할당량 dedicated_vcpus
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
    let node = format!("/nodes/{}", env.node);
    let (_, n) = admin.get(&node).await;
    let mut nspec = n["spec"].clone();
    nspec["host_resources"] = json!({ "dedicated_cpus": "2-3" });
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

/// 권한 `vm.dedicated`: 운영자(operator)는 전용 CPU·hugepages VM을 만들거나 그 칸을 바꾸지 못하지만 vNUMA·메모리
/// 묶기만 쓰는 공유 VM은 만들고, 관리자가 만든 전용 VM의 다른 칸(vCPU 수)은 바꾼다. 할당량 `dedicated_vcpus`는
/// 전용 VM의 vCPU를 센다.
#[tokio::test]
async fn dedicated_vms_need_the_privilege_and_count_in_the_quota() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("op", "operator").await;
    let with = |name: &str, edit: Value| {
        let mut req = vm_request(name);
        for (k, v) in edit.as_object().unwrap() {
            if k == "cpu" {
                for (ck, cv) in v.as_object().unwrap() {
                    req["spec"]["cpu"][ck] = cv.clone();
                }
            } else {
                req["spec"][k] = v.clone();
            }
        }
        req
    };
    for edit in [
        json!({ "cpu": { "policy": "dedicated" } }),
        json!({ "hugepages": "2M" }),
    ] {
        let (status, body) = operator.post("/vms", with("x", edit.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{edit}: {body}");
        assert_eq!(
            body["message"], "dedicated CPUs and hugepages need the vm.dedicated privilege",
            "{body}"
        );
    }
    let (status, body) = operator
        .post(
            "/vms",
            with(
                "vnuma",
                json!({ "numa": { "nodes": 1, "memory": "strict" } }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .post(
            "/vms",
            with("db", json!({ "cpu": { "policy": "dedicated" } })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut spec = body["spec"].clone();
    spec["cpu"]["cores"] = json!(2);
    let (status, body) = operator
        .put("/vms/db", json!({ "spec": spec.clone() }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    spec = body["spec"].clone();
    spec["hugepages"] = json!("2M");
    let (status, body) = operator.put("/vms/db", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // 할당량: 전용 vCPU 2개 폴더에 3 vCPU 전용 VM은 409 quota_exceeded(공유 VM은 세지 않는다)
    let (status, f) = admin
        .post(
            "/folders",
            json!({ "name": "db", "kind": "vm", "quota": { "dedicated_vcpus": 2 } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{f}");
    let folder = f["id"].clone();
    let mut req = with(
        "big",
        json!({ "cpu": { "policy": "dedicated", "cores": 3 } }),
    );
    req["spec"]["folder"] = folder.clone();
    let (status, body) = admin.post("/vms", req.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "quota_exceeded", "{body}");
    req["spec"]["cpu"]["cores"] = json!(2);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut plain = with("plain", json!({ "cpu": { "cores": 4 } }));
    plain["spec"]["folder"] = folder;
    let (status, body) = admin.post("/vms", plain).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, u) = admin
        .get(&format!("/folders/{}/usage", f["id"].as_str().unwrap()))
        .await;
    assert_eq!(u["usage"]["dedicated_vcpus"], 2, "{u}");
}

/// 리뷰 2 N6: 전용 CPU·hugepages VM을 다른 폴더로 옮기려면 그 폴더에 `vm.dedicated`가 있어야 한다(복제·배포와 같다).
/// 두 폴더의 `inventory.move`만 있는 사용자는 전용 VM을 옮기지 못하고, 공유 VM은 옮긴다.
#[tokio::test]
async fn moving_a_dedicated_vm_needs_the_privilege_in_the_new_folder() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut folders = Vec::new();
    for name in ["a", "b"] {
        let (status, f) = admin
            .post("/folders", json!({ "name": name, "kind": "vm" }))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{f}");
        folders.push(f["id"].clone());
    }
    for (name, policy) in [("db", "dedicated"), ("web", "shared")] {
        let mut req = vm_request(name);
        req["spec"]["cpu"]["policy"] = json!(policy);
        req["spec"]["folder"] = folders[0].clone();
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let (_, role) = admin
        .post(
            "/access-roles",
            json!({ "name": "mover", "privileges": ["inventory.move"] }),
        )
        .await;
    let (status, user) = admin
        .post(
            "/users",
            json!({ "name": "mv", "password": "password-1", "role": "none" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{user}");
    for f in &folders {
        let (status, body) = admin
            .post(
                "/grants",
                json!({
                    "principal": { "type": "user", "id": user["id"] },
                    "role": { "type": "custom", "id": role["id"] },
                    "target": { "type": "folder", "id": f },
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let mover = env.login("mv", "password-1").await;
    let (status, body) = mover
        .post(
            &format!("/folders/{}/members", folders[1].as_str().unwrap()),
            json!({ "items": [{ "kind": "vm", "key": "db" }, { "kind": "vm", "key": "web" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["ok"], false, "{body}");
    assert_eq!(
        items[0]["error"]["message"],
        "dedicated CPUs and hugepages need the vm.dedicated privilege",
        "{body}"
    );
    assert_eq!(items[1]["ok"], true, "{body}");
    // 관리자(어디서나 vm.dedicated)는 옮긴다.
    let (status, body) = admin
        .post(
            &format!("/folders/{}/members", folders[1].as_str().unwrap()),
            json!({ "items": [{ "kind": "vm", "key": "db" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["ok"], true, "{body}");
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

/// n1(전용 4-7, 2M 풀 512)에서 도는 전용 hugepages VM `db`(vCPU 2 → CPU 4·5, 256 MiB → 2M 128페이지)
async fn running_db(env: &Env) -> Uuid {
    let n1 = add_numa_node(env, "n1", "4-7").await;
    let mut node = env.cluster.get::<NodeSpec>(n1).unwrap().unwrap();
    let topology = node.status.topology.as_mut().unwrap();
    topology.nodes[0].hugepages = vec![opl_types::numa::HugepagePool {
        size_kib: 2048,
        total: 512,
    }];
    env.cluster
        .update_status::<NodeSpec>(n1, &node.status)
        .await
        .unwrap();
    let mut spec = dedicated(2, Some(n1), PowerState::Running);
    spec.hugepages = Some(opl_types::numa::PageSize::Size2M);
    let mut vm = create(env, "db", spec).await;
    vm.status.phase = VmPhase::Running;
    vm.status.node = Some(n1);
    let mut hw = RunningHardware::legacy(&vm.spec);
    hw.numa = Some(ResolvedNuma {
        nodes: vec![GuestNode {
            sockets: vec![0],
            memory_mib: 256,
        }],
        distances: Vec::new(),
        page: Some(PageKind::Size2M),
        policy: CpuPolicy::Dedicated,
        thread_policy: Some(Default::default()),
        emulator: Some(Default::default()),
        memory: Some(opl_types::numa::MemoryPolicy::Strict),
    });
    hw.host_placement = Some(HostPlacement {
        node: n1,
        vcpus: vec![4, 5],
        siblings: Default::default(),
        emulator: Default::default(),
        host_nodes: vec![0],
    });
    vm.status.hardware = Some(hw);
    env.cluster
        .update_status::<VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();
    n1
}

/// `PUT /nodes/{key}`의 `host_resources`: 칸이 없으면 그대로, 토폴로지에 맞지 않으면 400, 도는 VM의 pCPU·페이지를
/// 빼앗으면 409(`force`면 경고와 함께 쓴다), `{}`는 지운다.
#[tokio::test]
async fn host_resources_are_checked_and_kept() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_db(&env).await;
    let (_, n) = admin.get("/nodes/n1").await;
    let base = {
        let mut spec = n["spec"].clone();
        spec.as_object_mut().unwrap().remove("host_resources");
        spec
    };
    let put = |resources: Option<Value>| {
        let mut spec = base.clone();
        if let Some(r) = resources {
            spec["host_resources"] = r;
        }
        json!({ "spec": spec })
    };
    // 칸이 없으면 저장된 값 그대로(이 칸을 모르는 클라이언트)
    let (status, body) = admin.put("/nodes/n1", put(None), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["host_resources"]["dedicated_cpus"], "4-7",
        "{body}"
    );
    for (r, message) in [
        (
            json!({ "dedicated_cpus": "4-9" }),
            "node \"n1\": dedicated_cpus 8-9 are not online CPUs of this node",
        ),
        (
            json!({ "reserved_cpus": "0-4", "dedicated_cpus": "4-7" }),
            "node \"n1\": reserved_cpus and dedicated_cpus overlap: 4",
        ),
        (
            json!({ "dedicated_cpus": "0-7" }),
            "node \"n1\": no shared CPUs left: reserved_cpus and dedicated_cpus take every online CPU",
        ),
        (
            json!({ "dedicated_cpus": "4-7", "hugepages": [{ "size": "2M", "count": 1, "host_node": 1 }] }),
            "node \"n1\": hugepages: this node has no host NUMA node 1",
        ),
    ] {
        let (status, body) = admin.put("/nodes/n1", put(Some(r.clone())), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{r}: {body}");
        assert_eq!(body["code"], "invalid_host_resources", "{body}");
        assert_eq!(body["message"], message, "{body}");
    }
    // 토폴로지가 없는 노드(이 Mock 노드)
    let me = format!("/nodes/{}", env.node);
    let (_, m) = admin.get(&me).await;
    let mut mspec = m["spec"].clone();
    mspec["host_resources"] = json!({ "dedicated_cpus": "1" });
    let (status, body) = admin.put(&me, json!({ "spec": mspec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_host_resources", "{body}");

    // 도는 db의 pCPU 4·5를 전용에서 빼면 409, force면 쓰고 경고
    let shrink = put(Some(json!({ "dedicated_cpus": "6-7" })));
    let (status, body) = admin.put("/nodes/n1", shrink.clone(), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "host_cpus_in_use", "{body}");
    assert_eq!(body["details"]["vms"], json!(["db"]), "{body}");
    let (status, headers, body) = admin
        .send_headers(reqwest::Method::PUT, "/nodes/n1?force=true", shrink)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["host_resources"]["dedicated_cpus"], "6-7",
        "{body}"
    );
    let warning = headers["orbivisor-warning"].to_str().unwrap();
    assert!(warning.contains("db (4-5)"), "{warning}");
    // db는 2M 128페이지를 쓴다: 그보다 적은 예약은 409
    let few = put(Some(
        json!({ "dedicated_cpus": "4-7", "hugepages": [{ "size": "2M", "count": 100, "host_node": 0 }] }),
    ));
    let (status, body) = admin.put("/nodes/n1", few, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "hugepages_in_use", "{body}");
    assert_eq!(
        body["details"]["short"],
        json!(["2M@0: 100 reserved, 128 used"]),
        "{body}"
    );
    let enough = put(Some(
        json!({ "dedicated_cpus": "4-7", "hugepages": [{ "size": "2M", "count": 512 }] }),
    ));
    let (status, body) = admin.put("/nodes/n1", enough, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 토폴로지·사용
    let (status, t) = admin.get("/nodes/n1/topology").await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(
        t["sets"],
        json!({ "reserved": "", "dedicated": "4-7", "shared": "0-3" }),
        "{t}"
    );
    assert_eq!(t["host_nodes"][0]["dedicated_used"], "4-5", "{t}");
    assert_eq!(
        t["host_nodes"][0]["hugepages"],
        json!([{ "size": "2M", "total": 512, "used": 128 }]),
        "{t}"
    );
    assert_eq!(t["vms"][0]["vm"], "db", "{t}");
    assert_eq!(t["vms"][0]["vcpus"], json!([4, 5]), "{t}");
    assert_eq!(t["vms"][0]["pages"], 128, "{t}");
    assert!(t.get("live").is_none(), "{t}");
    let (status, u) = admin.get("/topology/usage").await;
    assert_eq!(status, StatusCode::OK, "{u}");
    let n1 = u["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "n1")
        .unwrap();
    assert_eq!(n1["dedicated_total"], 4, "{u}");
    assert_eq!(n1["dedicated_used"], 2, "{u}");
    assert_eq!(
        n1["hugepages"],
        json!([{ "size": "2M", "total_mib": 1024, "used_mib": 256 }]),
        "{u}"
    );
    // VM의 NUMA: 기록과 노드마다 맞음(n1은 기록 모양 그대로 다시 맞춤)
    let (status, v) = admin.get("/vms/db/numa").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["wanted"], "auto, dedicated, hugepages 2M, memory strict",
        "{v}"
    );
    assert_eq!(v["running"]["page"], "2M", "{v}");
    assert_eq!(v["placement"]["vcpus"], json!([4, 5]), "{v}");
    let fit = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node"] == "n1")
        .unwrap();
    assert_eq!(fit["fits"], true, "{v}");
    assert_eq!(fit["plan"]["host_nodes"], json!([0]), "{v}");
    assert_eq!(fit["plan"]["page"], "2M", "{v}");

    // 권한 없는 사용자
    let nobody = env.user("nobody", "none").await;
    assert_eq!(
        nobody.get("/nodes/n1/topology").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(nobody.get("/topology/usage").await.0, StatusCode::FORBIDDEN);
    assert_eq!(nobody.get("/vms/db/numa").await.0, StatusCode::NOT_FOUND);

    // `{}`는 지운다. 리뷰 1 M1: 고정된 VM(db)이 있으면 지우기도 409 `host_cpus_in_use`(공유 VM이 그 pCPU로 퍼짐),
    // force면 지우고 경고한다.
    let (status, body) = admin.put("/nodes/n1", put(Some(json!({}))), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "host_cpus_in_use", "{body}");
    assert_eq!(body["details"]["vms"], json!(["db"]), "{body}");
    let (status, headers, body) = admin
        .send_headers(
            reqwest::Method::PUT,
            "/nodes/n1?force=true",
            put(Some(json!({}))),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("host_resources").is_none(), "{body}");
    let warning = headers["orbivisor-warning"].to_str().unwrap();
    assert!(warning.contains("db (4-5)"), "{warning}");
}
