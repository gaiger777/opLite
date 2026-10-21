//! 한 프로세스 안에서 여러 노드를 띄우는 테스트 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(10);

pub fn node(n: u64) -> NodeId {
    NodeId::from(n)
}

pub fn raft_config() -> openraft::Config {
    openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 200,
        election_timeout_max: 400,
        ..Default::default()
    }
}

pub fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

pub struct TestCluster {
    pub router: MemRouter,
    pub nodes: BTreeMap<NodeId, Cluster>,
    pub raft: openraft::Config,
    pub write_timeout: Duration,
    root: TempDir,
}

impl TestCluster {
    /// 노드 `1..=n`을 띄우고 노드 1에서 클러스터를 초기화한 뒤 리더를 기다린다.
    pub async fn start(n: u64) -> Self {
        Self::start_with(n, raft_config(), Duration::from_secs(5)).await
    }

    pub async fn start_with(n: u64, raft: openraft::Config, write_timeout: Duration) -> Self {
        let mut tc = TestCluster {
            router: MemRouter::default(),
            nodes: BTreeMap::new(),
            raft,
            write_timeout,
            root: TempDir::new().unwrap(),
        };
        for i in 1..=n {
            tc.open(node(i)).await;
        }
        let members = (1..=n).map(|i| (node(i), addr(i))).collect();
        tc.nodes[&node(1)].initialize(members).await.unwrap();
        tc.leader().await;
        tc
    }

    /// 노드를 (다시) 연다. 데이터 디렉터리는 노드마다 고정이라 재시작하면 이전 상태를 이어받는다.
    pub async fn open(&mut self, id: NodeId) {
        let mut config = ClusterConfig::new(id, self.root.path().join(id.to_string()));
        config.raft = self.raft.clone();
        config.write_timeout = self.write_timeout;
        let cluster = Cluster::open(config, self.router.network(id))
            .await
            .unwrap();
        self.router.register(id, cluster.raft().clone());
        self.nodes.insert(id, cluster);
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 데이터 디렉터리는 남는다.
    pub async fn stop(&mut self, id: NodeId) {
        self.router.unregister(id);
        let cluster = self.nodes.remove(&id).unwrap();
        cluster.shutdown().await.unwrap();
    }

    pub fn get(&self, id: NodeId) -> &Cluster {
        &self.nodes[&id]
    }

    /// 실행 중인 노드 가운데 리더를 기다린다.
    pub async fn leader(&self) -> NodeId {
        self.leader_among(&self.nodes.keys().copied().collect::<Vec<_>>())
            .await
    }

    pub async fn leader_among(&self, ids: &[NodeId]) -> NodeId {
        let deadline = Instant::now() + WAIT;
        loop {
            for id in ids {
                let m = self.nodes[id].raft().metrics().borrow().clone();
                if m.state == ServerState::Leader {
                    return *id;
                }
            }
            assert!(Instant::now() < deadline, "no leader elected among {ids:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 노드가 `index`까지 적용하기를 기다린다.
    pub async fn wait_applied(&self, id: NodeId, index: u64) {
        self.nodes[&id]
            .raft()
            .wait(Some(WAIT))
            .applied_index_at_least(Some(index), "wait_applied")
            .await
            .unwrap();
    }
}

pub fn addr(n: u64) -> BasicNode {
    BasicNode::new(format!("node-{n}"))
}
