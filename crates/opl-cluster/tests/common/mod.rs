//! 한 프로세스 안에서 여러 노드를 띄우는 테스트 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;

pub const WAIT: Duration = Duration::from_secs(10);

pub fn node(n: u64) -> NodeId {
    NodeId::from(n)
}

/// 테스트 바이너리 여러 개가 동시에 돌아 CPU가 바빠도 하트비트가 늦어 재선거가
/// 일어나지 않도록 선거 제한 시간을 하트비트 간격보다 넉넉히 길게 둔다.
pub fn raft_config() -> openraft::Config {
    openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
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
    /// 팔로워가 받은 요청을 리더로 전달한다.
    pub forward: bool,
    root: TempDir,
}

impl TestCluster {
    /// 노드 `1..=n`을 띄우고 노드 1에서 클러스터를 초기화한 뒤 리더를 기다린다.
    pub async fn start(n: u64) -> Self {
        Self::start_with(n, raft_config(), Duration::from_secs(5)).await
    }

    pub async fn start_with(n: u64, raft: openraft::Config, write_timeout: Duration) -> Self {
        Self::build(n, raft, write_timeout, false).await
    }

    /// 팔로워가 요청을 리더로 전달하는 클러스터
    pub async fn start_forwarding(n: u64) -> Self {
        Self::build(n, raft_config(), Duration::from_secs(10), true).await
    }

    async fn build(n: u64, raft: openraft::Config, write_timeout: Duration, forward: bool) -> Self {
        let mut tc = TestCluster {
            router: MemRouter::default(),
            nodes: BTreeMap::new(),
            raft,
            write_timeout,
            forward,
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
    ///
    /// openraft의 `shutdown`은 상태 머신 태스크가 끝나기를 기다리지 않는다. 그 태스크가
    /// 아직 redb 파일을 쥐고 있으면 `DatabaseAlreadyOpen`이 나므로 잠시 뒤 다시 연다.
    pub async fn open(&mut self, id: NodeId) {
        let mut config = ClusterConfig::new(id, self.root.path().join(id.to_string()));
        config.raft = self.raft.clone();
        config.write_timeout = self.write_timeout;
        let deadline = Instant::now() + Duration::from_secs(5);
        let cluster = loop {
            match Cluster::open(config.clone(), self.router.network(id)).await {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        };
        let cluster = if self.forward {
            cluster.with_forwarder(self.router.forwarder(id))
        } else {
            cluster
        };
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

    /// `ids`의 모든 노드가 같은 리더를 알고, 그 리더가 자기 임기의 빈 엔트리까지
    /// 적용했을 때 그 리더를 돌려준다. 그래야 바로 이어지는 쓰기가 리더 변경이나
    /// 팔로워의 `leader_id: None`에 걸리지 않는다.
    pub async fn leader_among(&self, ids: &[NodeId]) -> NodeId {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(leader) = self.settled_leader(ids) {
                return leader;
            }
            assert!(Instant::now() < deadline, "no leader elected among {ids:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn settled_leader(&self, ids: &[NodeId]) -> Option<NodeId> {
        let leader = self.nodes[&ids[0]]
            .raft()
            .metrics()
            .borrow()
            .current_leader?;
        if !ids.contains(&leader) {
            return None;
        }
        for id in ids {
            if self.nodes[id].raft().metrics().borrow().current_leader != Some(leader) {
                return None;
            }
        }
        let m = self.nodes[&leader].raft().metrics().borrow().clone();
        let applied = m.last_applied.map(|l| l.index);
        (m.state == ServerState::Leader && applied.is_some() && applied >= m.last_log_index)
            .then_some(leader)
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
