//! 내장 VIP(설계 §4): 리더의 `VipManager`가 VIP 리스를 갱신하는 노드 가운데 VIP마다 다른 노드를 고르고,
//! 살아 있는 주인은 옮기지 않고, 리스가 끝난 주인의 VIP만 옮기는지. 리더가 된 뒤 리스 길이 동안은 갱신을
//! 받지 못한 주인도 쥐었다고 보는지. 노드의 `VipAgent`가 주인일 때만 VIP를 붙이고, 준비되지 않거나 멈추면
//! 떼는지. 다른 노드는 단일 노드 클러스터의 리더에게 직접 갱신을 보낸다. 옛 노드가 남았으면 VIP를 쓰지
//! 않는지, 늦게 깬 노드가 갱신보다 먼저 떼는지, 붙인 VIP를 기록하는지, 밖에서 지워진 VIP를 되살리는지, 뗀
//! VIP의 늦은 알림을 멈추는지.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, WAIT};
use opl_api::{VipAgent, VipAgentConfig, VipManager, VipManagerConfig};
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::VipRecord;
use opl_driver::{DriverError, VipAddress, VipNet};
use opl_types::node::{FEATURES, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::vip::VipHolderSpec;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// 바쁜 CI에서 100 ms 간격의 갱신이 잠깐 밀려도 "옮기지 않음"이 깨지지 않게 넉넉히 둔다.
const LEASE: Duration = Duration::from_secs(2);

fn manager(env: &Env) -> VipManager {
    VipManager::spawn(
        env.cluster.clone(),
        VipManagerConfig {
            tick: Duration::from_millis(50),
            lease: LEASE,
        },
    )
}

/// 지금 주인(VIP 주소 → 노드)
fn owners(cluster: &Cluster) -> BTreeMap<String, Uuid> {
    cluster
        .list::<VipHolderSpec>()
        .unwrap()
        .1
        .into_iter()
        .map(|h| (h.name, h.spec.node))
        .collect()
}

async fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 다른 노드 하나의 VIP 갱신을 100 ms마다 리더에게 직접 보낸다. `alive`를 끄면 멈춘다.
struct Renewer {
    alive: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Renewer {
    fn start(cluster: Arc<Cluster>, node: Uuid, vips: &[&str]) -> Self {
        let alive = Arc::new(AtomicBool::new(true));
        let vips: Vec<String> = vips.iter().map(|v| v.to_string()).collect();
        let flag = alive.clone();
        let task = tokio::spawn(async move {
            while flag.load(Ordering::SeqCst) {
                let _ = cluster
                    .execute_as_leader(LeaderRequest::RenewVip {
                        node: NodeId(node),
                        vips: vips.clone(),
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        Self { alive, task }
    }

    async fn stop(self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.task.await;
    }
}

const A: &str = "10.0.0.1";
const B: &str = "10.0.0.2";

#[tokio::test]
async fn gives_each_vip_to_a_different_node_and_moves_only_expired_holders() {
    let env = Env::start().await;
    let (n1, n2, n3) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let r1 = Renewer::start(env.cluster.clone(), n1, &[A, B]);
    let r2 = Renewer::start(env.cluster.clone(), n2, &[A, B]);
    // 둘 다 갱신한 뒤에 배정을 시작해야 처음부터 나눠 준다.
    wait_for("both renewals", || {
        env.cluster.vip_leases().renewed.len() == 2
    })
    .await;
    let m = manager(&env);

    wait_for("both VIPs assigned", || owners(&env.cluster).len() == 2).await;
    let first = owners(&env.cluster);
    assert_eq!(first[A], n1);
    assert_eq!(first[B], n2);

    // 노드가 더해져도 살아 있는 주인은 그대로다.
    let r3 = Renewer::start(env.cluster.clone(), n3, &[A, B]);
    tokio::time::sleep(LEASE * 2).await;
    assert_eq!(owners(&env.cluster), first);

    // node1이 멈추면 리스가 끝난 뒤 A만 node3(VIP가 없는 노드)으로 간다.
    let stopped = Instant::now();
    r1.stop().await;
    wait_for("A moved", || owners(&env.cluster)[A] != n1).await;
    assert!(stopped.elapsed() >= LEASE - Duration::from_millis(150));
    let moved = owners(&env.cluster);
    assert_eq!(moved[A], n3);
    assert_eq!(moved[B], n2);

    m.shutdown().await;
    r2.stop().await;
    r3.stop().await;
}

/// 리더가 된 뒤(여기서는 배정 작업을 띄운 뒤) 리스 길이 동안은 갱신이 없는 주인도 VIP를 쥐었다고 본다.
#[tokio::test]
async fn a_new_leader_waits_a_lease_before_moving_an_unknown_holder() {
    let env = Env::start().await;
    let ghost = Uuid::from_u128(9);
    let n1 = Uuid::from_u128(1);
    env.cluster
        .create(&Resource::new(A, VipHolderSpec { node: ghost }))
        .await
        .unwrap();
    let r1 = Renewer::start(env.cluster.clone(), n1, &[A]);
    let started = Instant::now();
    let m = manager(&env);

    wait_for("A moved from the silent holder", || {
        owners(&env.cluster)[A] == n1
    })
    .await;
    assert!(started.elapsed() >= LEASE, "{:?}", started.elapsed());

    m.shutdown().await;
    r1.stop().await;
}

/// VIP 조작을 남기는 가짜. `slow_add`가 있으면 첫 붙이기가 그만큼 걸린다(멈췄다 깬 노드). `present`는
/// 인터페이스에 붙어 있는 주소, `announce_delay`는 알림이 걸리는 시간이다.
#[derive(Default)]
struct FakeNet {
    calls: Mutex<Vec<(&'static str, String)>>,
    slow_add: Mutex<Option<Duration>>,
    present: Mutex<BTreeSet<String>>,
    announce_delay: Mutex<Option<Duration>>,
}

impl FakeNet {
    fn calls(&self) -> Vec<(&'static str, String)> {
        self.calls.lock().unwrap().clone()
    }

    fn held(&self) -> bool {
        // 마지막 add/remove가 add면 붙어 있다.
        self.calls()
            .iter()
            .rev()
            .find(|(op, _)| *op != "announce")
            .is_some_and(|(op, _)| *op == "add")
    }
}

#[async_trait]
impl VipNet for FakeNet {
    async fn add(&self, iface: &str, vip: VipAddress) -> Result<bool, DriverError> {
        assert_eq!(iface, "eth0");
        let slow = self.slow_add.lock().unwrap().take();
        if let Some(slow) = slow {
            tokio::time::sleep(slow).await;
        }
        self.calls.lock().unwrap().push(("add", vip.to_string()));
        Ok(self.present.lock().unwrap().insert(vip.to_string()))
    }

    async fn remove(&self, iface: &str, vip: VipAddress) -> Result<(), DriverError> {
        assert_eq!(iface, "eth0");
        self.calls.lock().unwrap().push(("remove", vip.to_string()));
        self.present.lock().unwrap().remove(&vip.to_string());
        Ok(())
    }

    async fn announce(&self, iface: &str, vip: VipAddress) -> Result<(), DriverError> {
        assert_eq!(iface, "eth0");
        let delay = *self.announce_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.calls
            .lock()
            .unwrap()
            .push(("announce", vip.to_string()));
        Ok(())
    }
}

fn agent(env: &Env, net: &Arc<FakeNet>) -> VipAgent {
    let mut config = VipAgentConfig::new("eth0", vec!["10.0.0.1/24".parse().unwrap()]);
    config.interval = Duration::from_millis(100);
    config.lease = LEASE;
    config.margin = Duration::from_millis(100);
    config.record = Some(VipRecord::in_dir(env.dir.path()));
    VipAgent::spawn(env.state.clone(), net.clone(), config)
}

fn recorded(env: &Env) -> Vec<String> {
    VipRecord::in_dir(env.dir.path())
        .load()
        .unwrap()
        .iter()
        .map(|(iface, vip)| format!("{iface} {vip}"))
        .collect()
}

async fn set_maintenance(env: &Env, on: bool) {
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let spec = NodeSpec {
        maintenance: on,
        ..node.spec
    };
    env.cluster
        .update_spec(env.node, None, node.labels, &spec)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_agent_holds_the_vip_only_while_it_is_the_ready_owner() {
    let env = Env::start().await;
    let net = Arc::new(FakeNet::default());
    let m = manager(&env);
    let a = agent(&env, &net);

    // 시작할 때 남은 VIP를 먼저 뗀다.
    wait_for("the VIP taken", || net.held()).await;
    let calls = net.calls();
    assert_eq!(calls[0], ("remove", "10.0.0.1/24".to_string()));
    assert!(calls.contains(&("add", "10.0.0.1/24".to_string())));
    wait_for("the announcement", || {
        net.calls().iter().any(|(op, _)| *op == "announce")
    })
    .await;
    assert_eq!(owners(&env.cluster)[A], env.node);
    // 붙인 VIP를 기록해 두고(죽어서 남으면 다음 시작이 뗀다) 떼면 지운다.
    assert_eq!(recorded(&env), ["eth0 10.0.0.1/24"]);

    // 유지보수 중(헬스 503)이면 곧바로 뗀다.
    set_maintenance(&env, true).await;
    wait_for("the VIP released", || !net.held()).await;
    // 준비되면 리스가 끝나기 전이라 같은 노드가 다시 갖는다.
    set_maintenance(&env, false).await;
    wait_for("the VIP taken again", || net.held()).await;

    // 멈추면 뗀다.
    a.shutdown().await;
    assert!(!net.held());
    assert!(recorded(&env).is_empty());
    m.shutdown().await;
}

fn announcements(net: &FakeNet) -> usize {
    net.calls()
        .iter()
        .filter(|(op, _)| *op == "announce")
        .count()
}

/// 밖에서(`ifreload` 등) 지워진 VIP는 몇 번의 갱신 안에 다시 붙이고 다시 알린다.
#[tokio::test]
async fn a_vip_removed_outside_is_added_again() {
    let env = Env::start().await;
    let net = Arc::new(FakeNet::default());
    let m = manager(&env);
    let a = agent(&env, &net);
    wait_for("the announcement", || announcements(&net) == 1).await;
    net.present.lock().unwrap().clear();
    wait_for("the VIP added again", || {
        net.present.lock().unwrap().contains("10.0.0.1/24")
    })
    .await;
    wait_for("the second announcement", || announcements(&net) == 2).await;
    // 붙어 있으면 다시 알리지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(announcements(&net), 2);
    a.shutdown().await;
    m.shutdown().await;
}

/// 떼면 아직 끝나지 않은 알림을 멈춘다(뗀 뒤에 알리면 이웃이 새 주인 대신 이 노드를 가리킨다).
#[tokio::test]
async fn releasing_a_vip_stops_its_announcement() {
    let env = Env::start().await;
    let net = Arc::new(FakeNet::default());
    *net.announce_delay.lock().unwrap() = Some(Duration::from_secs(1));
    let m = manager(&env);
    let a = agent(&env, &net);
    wait_for("the VIP taken", || net.held()).await;
    set_maintenance(&env, true).await;
    wait_for("the VIP released", || !net.held()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(announcements(&net), 0, "{:?}", net.calls());
    a.shutdown().await;
    m.shutdown().await;
}

/// 멈췄다 늦게 깬 노드(여기서는 붙이기가 리스보다 오래 걸림)는 끝난 리스로 VIP를 쥔 채 갱신하지 않고 먼저
/// 뗀 뒤 다시 갱신해 붙인다.
#[tokio::test]
async fn an_agent_that_wakes_up_late_releases_before_renewing() {
    let env = Env::start().await;
    let net = Arc::new(FakeNet::default());
    *net.slow_add.lock().unwrap() = Some(LEASE + Duration::from_millis(500));
    let m = manager(&env);
    let a = agent(&env, &net);
    wait_for("the VIP taken twice", || {
        net.calls().iter().filter(|(op, _)| *op == "add").count() >= 2
    })
    .await;
    let ops: Vec<&str> = net
        .calls()
        .iter()
        .map(|(op, _)| *op)
        .filter(|op| *op != "announce")
        .collect();
    assert_eq!(ops[..4], ["remove", "add", "remove", "add"], "{ops:?}");
    a.shutdown().await;
    m.shutdown().await;
}

/// `vip` 기능을 모르는 노드(위트니스 포함)가 남았으면 리더는 `VipHolder`를 쓰지 않고(옛 노드의 적용이
/// 멈춘다) 노드는 VIP 리스를 갱신하지 않는다. 모든 노드가 알면 시작한다.
#[tokio::test]
async fn vips_wait_until_every_node_knows_them() {
    let env = Env::start().await;
    let mut old = Resource::new(
        "old",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: true,
            roles: None,
        },
    );
    old.status = NodeStatus {
        online: true,
        features: FEATURES
            .iter()
            .filter(|f| **f != "vip")
            .map(|f| f.to_string())
            .collect(),
        ..Default::default()
    };
    env.cluster.create(&old).await.unwrap();
    let r = Renewer::start(env.cluster.clone(), Uuid::from_u128(2), &[A]);
    let net = Arc::new(FakeNet::default());
    let m = manager(&env);
    let a = agent(&env, &net);
    tokio::time::sleep(LEASE).await;
    assert!(owners(&env.cluster).is_empty());
    assert!(!net.held(), "{:?}", net.calls());
    assert!(
        !env.cluster
            .vip_leases()
            .renewed
            .contains_key(&NodeId(env.node))
    );

    // 옛 노드가 업그레이드되면 배정한다.
    let status = NodeStatus {
        online: true,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        ..Default::default()
    };
    env.cluster
        .update_status::<NodeSpec>(old.id, &status)
        .await
        .unwrap();
    wait_for("a VIP holder", || owners(&env.cluster).contains_key(A)).await;
    r.stop().await;
    a.shutdown().await;
    m.shutdown().await;
}

/// 리더가 다른 노드를 주인으로 정하면(이 노드가 쥐지 않을 때) 이 노드는 붙이지 않는다.
#[tokio::test]
async fn the_agent_does_not_take_a_vip_owned_by_another_node() {
    let env = Env::start().await;
    let other = Uuid::from_u128(2);
    env.cluster
        .create(&Resource::new(A, VipHolderSpec { node: other }))
        .await
        .unwrap();
    let r = Renewer::start(env.cluster.clone(), other, &[A]);
    let net = Arc::new(FakeNet::default());
    let m = manager(&env);
    let a = agent(&env, &net);
    tokio::time::sleep(LEASE * 2).await;
    assert!(!net.held(), "{:?}", net.calls());
    assert_eq!(owners(&env.cluster)[A], other);

    // 주인이 멈추면 리스가 끝난 뒤 이 노드가 갖는다.
    r.stop().await;
    wait_for("the VIP taken over", || net.held()).await;
    assert_eq!(owners(&env.cluster)[A], env.node);
    a.shutdown().await;
    m.shutdown().await;
}
