//! 클러스터 저장소와 Mock 하이퍼바이저 위에서 reconciler를 돌리는 테스트 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::resource::Resource;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, PowerState, Vm, VmSpec};
use tempfile::TempDir;
use uuid::Uuid;

pub const WAIT: Duration = Duration::from_secs(10);

pub fn raft_config() -> openraft::Config {
    openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        ..Default::default()
    }
}

/// 테스트가 빨리 끝나도록 시간을 줄인 설정. 재동기화는 꺼 두어(1시간) 이벤트만으로
/// 동작하는지 확인한다.
pub fn config(node: Uuid) -> ReconcilerConfig {
    ReconcilerConfig {
        node,
        // 테스트는 dir 풀만 쓰므로 NFS 마운트 디렉터리는 쓰이지 않는다.
        mount_root: "/nonexistent/opl-pools".into(),
        resync_interval: Duration::from_secs(3600),
        acpi_timeout: Duration::from_millis(300),
        retry_interval: Duration::from_millis(100),
        restart: RestartBackoff {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            max_failures: 3,
            window: Duration::from_secs(60),
        },
    }
}

/// 노드를 열고 네트워크에 등록한다. 같은 디렉터리를 방금 닫았다면 openraft의 상태 머신
/// 태스크가 redb 파일을 놓을 때까지 다시 시도한다.
pub async fn open_node(
    router: &MemRouter,
    id: NodeId,
    dir: &Path,
    raft: openraft::Config,
    write_timeout: Duration,
) -> Arc<Cluster> {
    let mut config = ClusterConfig::new(id, dir);
    config.raft = raft;
    config.write_timeout = write_timeout;
    let deadline = Instant::now() + Duration::from_secs(5);
    let cluster = loop {
        match Cluster::open(config.clone(), router.network(id)).await {
            Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                if Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => break other.unwrap(),
        }
    };
    router.register(id, cluster.raft().clone());
    Arc::new(cluster)
}

pub async fn wait_leader(cluster: &Cluster) {
    let id = cluster.id();
    cluster
        .raft()
        .wait(Some(WAIT))
        .metrics(
            |m| m.state == ServerState::Leader && m.current_leader == Some(id),
            "leader",
        )
        .await
        .unwrap();
}

/// 노드 하나짜리 클러스터를 만들고 리더가 되기를 기다린다.
pub async fn single_node(router: &MemRouter, dir: &Path) -> Arc<Cluster> {
    let id = NodeId(Uuid::now_v7());
    let cluster = open_node(router, id, dir, raft_config(), Duration::from_secs(5)).await;
    let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
    cluster.initialize(members).await.unwrap();
    wait_leader(&cluster).await;
    cluster
}

/// 디스크와 NIC 없이 UEFI로 뜨는 작은 VM(이 호스트의 머신 타입).
pub fn vm(name: &str, node: Uuid) -> Vm {
    Resource::new(
        name,
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: opl_types::vm::Arch::native().machine(),
            disks: vec![],
            nics: vec![],
            power: PowerState::Running,
            restart: Default::default(),
            node: Some(node),
            snapshot_schedule: None,
        },
    )
}

/// 로컬 저장소의 VM이 조건을 만족할 때까지 기다린다.
pub async fn wait_vm(cluster: &Cluster, id: Uuid, what: &str, pred: impl Fn(&Vm) -> bool) -> Vm {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(vm) = cluster.get::<VmSpec>(id).unwrap() {
            if pred(&vm) {
                return vm;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?} / {:?}",
                vm.spec.power,
                vm.status
            );
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 사용자가 시작·정지 버튼을 누른 것처럼 spec.power를 바꾼다. reconciler의 status 쓰기와
/// 부딪치면(revision 불일치) 다시 읽어 쓴다.
pub async fn set_power(cluster: &Cluster, id: Uuid, power: PowerState) {
    loop {
        let vm = cluster.get::<VmSpec>(id).unwrap().unwrap();
        let mut spec = vm.spec.clone();
        spec.power = power;
        match cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return,
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => panic!("{e}"),
        }
    }
}

/// 노드 하나짜리 클러스터 + Mock 하이퍼바이저 + reconciler.
pub struct Env {
    pub router: MemRouter,
    pub cluster: Arc<Cluster>,
    pub hv: Arc<MockHypervisor>,
    pub node: Uuid,
    pub config: ReconcilerConfig,
    reconciler: Option<Reconciler>,
    _dir: TempDir,
}

impl Env {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(tweak: impl FnOnce(&mut ReconcilerConfig)) -> Self {
        let dir = TempDir::new().unwrap();
        let router = MemRouter::default();
        let cluster = single_node(&router, dir.path()).await;
        let node = cluster.id().0;
        let mut config = config(node);
        tweak(&mut config);
        let mut env = Env {
            router,
            cluster,
            hv: Arc::new(MockHypervisor::default()),
            node,
            config,
            reconciler: None,
            _dir: dir,
        };
        env.start_reconciler();
        env
    }

    pub fn start_reconciler(&mut self) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        self.reconciler = Some(Reconciler::spawn(
            self.cluster.clone(),
            hv,
            self.config.clone(),
        ));
    }

    pub async fn stop_reconciler(&mut self) {
        self.reconciler.take().unwrap().shutdown().await;
    }

    pub async fn create(&self, vm: &Vm) {
        self.cluster.create(vm).await.unwrap();
    }

    pub fn get(&self, id: Uuid) -> Vm {
        self.cluster.get::<VmSpec>(id).unwrap().unwrap()
    }

    pub async fn wait(&self, id: Uuid, what: &str, pred: impl Fn(&Vm) -> bool) -> Vm {
        wait_vm(&self.cluster, id, what, pred).await
    }

    pub async fn set_power(&self, id: Uuid, power: PowerState) {
        set_power(&self.cluster, id, power).await
    }

    pub fn force_stop(&self, id: Uuid) {
        self.reconciler.as_ref().unwrap().handle().force_stop(id);
    }

    pub fn restore_on_start(&self, id: Uuid, tag: &str) {
        self.reconciler
            .as_ref()
            .unwrap()
            .handle()
            .restore_on_start(id, tag.into());
    }

    pub fn clear_restore(&self, id: Uuid) {
        self.reconciler.as_ref().unwrap().handle().clear_restore(id);
    }

    /// Mock 하이퍼바이저에서 실행 중인 VM의 pid
    pub async fn pid(&self, id: Uuid) -> Option<u32> {
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
}
