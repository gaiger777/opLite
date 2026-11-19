//! 켤 때의 NUMA 배치와 기록(계획 64 스테이지 4): 노드 할당기가 전용 pCPU·호스트 노드·hugepages를 고르고 예약하며,
//! 작업자가 `hardware.numa`·`host_placement`를 기록하고 드라이버에 고정을 넘긴다. 검사 루프의 다시 고정도 본다.

mod common;

use std::sync::Arc;

use common::{Env, vm};
use opl_driver::Hypervisor;
use opl_reconciler::HostAllocator;
use opl_reconciler::cpu::CpuRefusal;
use opl_reconciler::host_alloc::{Shape, repin};
use opl_types::node::{Node, NodeSpec, PLACEMENT};
use opl_types::numa::{
    CpuPolicy, CpuSet, HostNode, HostResources, HugepagePool, NodeTopology, PageKind, PageSize,
};
use opl_types::resource::Resource;
use opl_types::vm::{PowerState, Vm, VmPhase};
use uuid::Uuid;

fn set(s: &str) -> CpuSet {
    s.parse().unwrap()
}

/// GitHub 러너 모양: CPU 4개·NUMA 노드 하나·2M hugepages 256개, cgroup cpuset 있음
fn runner() -> NodeTopology {
    NodeTopology {
        sockets: 1,
        threads_per_core: 1,
        cpus: set("0-3"),
        nodes: vec![HostNode {
            id: 0,
            cpus: set("0-3"),
            memory_mib: 16384,
            hugepages: vec![HugepagePool {
                size_kib: 2048,
                total: 256,
            }],
        }],
        page_sizes_kib: vec![2048],
        pinning: true,
        cpuset: true,
        ..NodeTopology::default()
    }
}

/// 이 노드 기록(예약 0, 전용 2-3 → 공유 1)
async fn add_node(env: &Env, resources: Option<HostResources>) {
    let mut node: Node = Resource::new(
        "n1",
        NodeSpec {
            host_resources: resources,
            address: "a".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = env.node;
    node.status.features = vec![PLACEMENT.to_string()];
    node.status.topology = Some(runner());
    env.cluster.create(&node).await.unwrap();
}

fn resources() -> HostResources {
    HostResources {
        reserved_cpus: set("0"),
        dedicated_cpus: set("2-3"),
        hugepages: Vec::new(),
    }
}

fn dedicated(env: &Env, name: &str, vcpus: u16) -> Vm {
    let mut vm = vm(name, env.node);
    vm.spec.cpu.cores = vcpus;
    vm.spec.cpu.policy = CpuPolicy::Dedicated;
    vm
}

async fn running(env: &Env, id: Uuid) -> Vm {
    env.wait(id, "running", |v| {
        v.status.phase == VmPhase::Running && v.status.hardware.is_some()
    })
    .await
}

fn launched(env: &Env, id: Uuid) -> opl_types::runtime::VmRuntimeSpec {
    env.hv
        .launch_specs()
        .into_iter()
        .rfind(|s| s.id == id)
        .expect("VM was not launched")
}

/// 전용 VM: 빈 전용 pCPU에 vCPU마다 고정하고 에뮬레이터는 공유 집합, 스코프는 둘의 합이다. 기록에 모양과 이 노드의
/// 배치가 남는다. 할당기는 기록된 pCPU를 빼고 고르고, 앞 VM이 꺼지면(예약도 풀림) 다음 VM을 켠다.
#[tokio::test]
async fn dedicated_vms_are_placed_pinned_and_recorded() {
    let env = Env::start().await;
    add_node(&env, Some(resources())).await;
    let first = dedicated(&env, "db", 2);
    env.create(&first).await;
    let got = running(&env, first.id).await;
    let hw = got.status.hardware.unwrap();
    let numa = hw.numa.clone().unwrap();
    assert_eq!(numa.policy, CpuPolicy::Dedicated);
    assert_eq!(numa.nodes.len(), 1);
    let place = hw.host_placement.clone().unwrap();
    assert_eq!(place.node, env.node);
    assert_eq!(place.vcpus, [2, 3]);
    assert_eq!(place.host_nodes, [0]);
    assert!(hw.pending.is_empty(), "{:?}", hw.pending);
    let spec = launched(&env, first.id);
    assert_eq!(spec.numa.as_ref(), Some(&numa));
    assert_eq!(spec.host_placement.as_ref(), Some(&place));
    let pin = spec.pinning.unwrap();
    assert_eq!(pin.emulator, set("1"));
    assert_eq!(pin.vcpus, [set("2"), set("3")]);
    assert_eq!(pin.scope, Some(set("1-3")));

    // 둘째 VM은 Raft 장부가 켜기를 막는다. 할당기도 기록된 pCPU를 빼고 고른다.
    let mut second = dedicated(&env, "cache", 1);
    second.spec.power = PowerState::Stopped;
    env.create(&second).await;
    let shape = Shape::Spec(&second.spec, &second.spec.cpu);
    match env.config.host_alloc.claim(&env.cluster, second.id, shape) {
        Err(CpuRefusal::Cannot(m)) => assert_eq!(
            m,
            "node \"n1\" has 0 free dedicated CPUs on host NUMA node 0; the VM needs 1 on one"
        ),
        other => panic!("{other:?}"),
    }
    env.set_power(first.id, PowerState::Stopped).await;
    env.wait(first.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(second.id, PowerState::Running).await;
    let got = running(&env, second.id).await;
    assert_eq!(
        got.status.hardware.unwrap().host_placement.unwrap().vcpus,
        [2]
    );
}

/// NUMA 설정이 없는 VM: 노드에 호스트 자원 설정이 없으면 기록·런타임 스펙이 지금 그대로(고정 없음)이고, 예약 CPU가
/// 있으면 공유 집합에 둔다(opld의 친화를 물려받지 않게).
#[tokio::test]
async fn plain_vms_stay_as_before_or_run_on_the_shared_cpus() {
    let env = Env::start().await;
    add_node(&env, None).await;
    let plain = vm("web", env.node);
    env.create(&plain).await;
    let got = running(&env, plain.id).await;
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.numa, None);
    assert_eq!(hw.host_placement, None);
    let json = serde_json::to_value(&hw).unwrap();
    assert!(json.get("numa").is_none() && json.get("host_placement").is_none());
    let spec = launched(&env, plain.id);
    assert_eq!(
        (spec.numa, spec.host_placement, spec.pinning),
        (None, None, None)
    );

    let env = Env::start().await;
    add_node(&env, Some(resources())).await;
    let plain = vm("web", env.node);
    env.create(&plain).await;
    let got = running(&env, plain.id).await;
    assert_eq!(got.status.hardware.unwrap().numa, None);
    let pin = launched(&env, plain.id).pinning.unwrap();
    assert_eq!(pin.emulator, set("1"));
    assert!(pin.vcpus.is_empty());
    assert_eq!(pin.scope, Some(set("1")));
}

/// hugepages VM은 `/sys`의 실제 빈 수까지만 받는다(다른 프로세스가 쓴 페이지).
#[tokio::test]
async fn hugepages_are_limited_by_the_real_free_pages() {
    let sys = tempfile::tempdir().unwrap();
    let pool = sys
        .path()
        .join("devices/system/node/node0/hugepages/hugepages-2048kB");
    std::fs::create_dir_all(&pool).unwrap();
    std::fs::write(pool.join("free_hugepages"), "100\n").unwrap();
    let root = sys.path().to_path_buf();
    let env = Env::start_with(|c| {
        c.host_alloc = Arc::new(HostAllocator::new(c.node, Some(root)));
    })
    .await;
    add_node(&env, None).await;
    let mut big = vm("big", env.node);
    big.spec.hugepages = Some(PageSize::Size2M);
    big.spec.memory_mib = 256;
    env.create(&big).await;
    let got = env
        .wait(big.id, "refused", |v| v.status.last_error.is_some())
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some(
            "failed to start: the VM's node has 100 free 2M hugepages on host NUMA node 0; the VM needs 128 on one"
        )
    );
    std::fs::write(pool.join("free_hugepages"), "256\n").unwrap();
    env.set_power(big.id, PowerState::Running).await;
    let got = running(&env, big.id).await;
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.numa.unwrap().page, Some(PageKind::Size2M));
    assert_eq!(hw.host_placement.unwrap().host_nodes, [0]);
}

/// 켜는 중인 VM의 예약 때문에 들어가지 않으면 기다린다(실패로 세지 않음). 예약을 풀면 들어간다.
#[tokio::test]
async fn a_reservation_of_a_starting_vm_makes_others_wait() {
    let mut env = Env::start().await;
    env.stop_reconciler().await;
    add_node(&env, Some(resources())).await;
    let mut a = dedicated(&env, "a", 2);
    a.spec.power = PowerState::Stopped;
    let mut b = dedicated(&env, "b", 1);
    b.spec.power = PowerState::Stopped;
    env.create(&a).await;
    env.create(&b).await;
    let alloc = &env.config.host_alloc;
    let claim = alloc
        .claim(&env.cluster, a.id, Shape::Spec(&a.spec, &a.spec.cpu))
        .unwrap();
    assert_eq!(claim.placement.unwrap().vcpus, [2, 3]);
    assert_eq!(alloc.held(), [a.id]);
    match alloc.claim(&env.cluster, b.id, Shape::Spec(&b.spec, &b.spec.cpu)) {
        Err(CpuRefusal::Waiting(m)) => assert!(m.contains("waiting for other VMs starting"), "{m}"),
        other => panic!("{other:?}"),
    }
    alloc.release(a.id);
    let claim = alloc
        .claim(&env.cluster, b.id, Shape::Spec(&b.spec, &b.spec.cpu))
        .unwrap();
    assert_eq!(claim.placement.unwrap().vcpus, [2]);
    // 예약한 VM이 지워지면 예약도 사라진다.
    env.cluster
        .delete::<opl_types::vm::VmSpec>(b.id, None)
        .await
        .unwrap();
    let c = dedicated(&env, "c", 2);
    env.create(&c).await;
    assert!(
        alloc
            .claim(&env.cluster, c.id, Shape::Spec(&c.spec, &c.spec.cpu))
            .is_ok()
    );
}

/// 검사 루프: 기록과 지금 세 집합으로 셈한 고정이 드라이버의 것과 다르면(누가 `taskset`을 씀, 공유 집합이 바뀜) 다시
/// 건다. 같으면 아무것도 하지 않는다.
#[tokio::test]
async fn repin_fixes_drift_and_follows_the_shared_set() {
    let env = Env::start().await;
    add_node(&env, Some(resources())).await;
    let db = dedicated(&env, "db", 1);
    env.create(&db).await;
    running(&env, db.id).await;
    assert_eq!(repin(&env.cluster, &*env.hv, env.node).await.unwrap(), 0);
    let mut drifted = env.hv.saved_pinning(db.id).unwrap();
    drifted.emulator = set("0-3");
    env.hv.drift_pinning(db.id, drifted);
    assert_eq!(repin(&env.cluster, &*env.hv, env.node).await.unwrap(), 1);
    assert_eq!(env.hv.saved_pinning(db.id).unwrap().emulator, set("1"));
    assert_eq!(repin(&env.cluster, &*env.hv, env.node).await.unwrap(), 0);

    // 예약을 지우면 공유 집합이 넓어진다(0-1): 도는 VM의 에뮬레이터를 다시 건다.
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut spec = node.spec.clone();
    spec.host_resources = Some(HostResources {
        reserved_cpus: CpuSet::new(),
        ..resources()
    });
    env.cluster
        .update_spec(node.id, Some(node.revision), node.labels, &spec)
        .await
        .unwrap();
    assert_eq!(repin(&env.cluster, &*env.hv, env.node).await.unwrap(), 1);
    let pin = env.hv.saved_pinning(db.id).unwrap();
    assert_eq!(pin.emulator, set("0-1"));
    assert_eq!(pin.vcpus.len(), 1);
}
