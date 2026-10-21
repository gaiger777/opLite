//! Raft 과반수를 잃어도 실행 중인 VM은 그대로 돌고(설계 10절 불변식 4),
//! 못 쓴 status는 과반수가 돌아오면 기록된다.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{WAIT, open_node, raft_config, vm, wait_leader, wait_vm};
use openraft::BasicNode;
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::Reconciler;
use opl_types::vm::{RestartPolicy, VmPhase, VmSpec};
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn vm_is_restarted_without_quorum_and_status_catches_up() {
    let dir = TempDir::new().unwrap();
    let router = MemRouter::default();
    let write_timeout = Duration::from_millis(300);
    let (n1, n2) = (NodeId(Uuid::now_v7()), NodeId(Uuid::now_v7()));
    // n2는 선거를 시작하지 않는다. 그래서 n2가 내려갔다 돌아와도 n1이 그대로 리더다.
    let follower = openraft::Config {
        election_timeout_min: 60_000,
        election_timeout_max: 120_000,
        ..raft_config()
    };
    let c1 = open_node(
        &router,
        n1,
        &dir.path().join("n1"),
        raft_config(),
        write_timeout,
    )
    .await;
    let c2 = open_node(
        &router,
        n2,
        &dir.path().join("n2"),
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, BasicNode::new("n1")), (n2, BasicNode::new("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let hv = Arc::new(MockHypervisor::default());
    let dyn_hv: Arc<dyn Hypervisor> = hv.clone();
    let reconciler = Reconciler::spawn(c1.clone(), dyn_hv, common::config(n1.0));
    let mut vm = vm("web", n1.0);
    vm.spec.restart = RestartPolicy::OnFailure;
    c1.create(&vm).await.unwrap();
    let before = wait_vm(&c1, vm.id, "running", |v| {
        v.status.phase == VmPhase::Running
    })
    .await
    .status
    .pid;

    // n2를 내려 과반수를 없앤다.
    router.unregister(n2);
    c2.shutdown().await.unwrap();
    drop(c2);
    hv.crash(vm.id);

    // status를 쓰지 못해도 VM은 다시 시작된다.
    let deadline = Instant::now() + WAIT;
    let after = loop {
        let pid = hv.inspect(vm.id).await.unwrap().map(|s| s.pid);
        if pid.is_some() && pid != before {
            break pid;
        }
        assert!(Instant::now() < deadline, "VM was not restarted");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::sleep(write_timeout * 2).await;
    assert_eq!(
        c1.get::<VmSpec>(vm.id).unwrap().unwrap().status.pid,
        before,
        "status must not change without quorum"
    );

    // n2가 돌아오면 미뤄 둔 status가 기록된다.
    let _c2 = open_node(&router, n2, &dir.path().join("n2"), follower, write_timeout).await;
    wait_vm(&c1, vm.id, "status caught up", |v| {
        v.status.phase == VmPhase::Running && v.status.pid == after
    })
    .await;
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), after);
    reconciler.shutdown().await;
}
