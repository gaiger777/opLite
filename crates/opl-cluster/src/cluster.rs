use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{Raft, RaftMetrics, RaftNetworkFactory};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::follow::{self, FollowClient, FollowFrame, NodeRole};
use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::log_store::LogStore;
use crate::replica::Replica;
use crate::state_machine::{StateMachine, save_current_snapshot};
use crate::store::{
    NodeMode, Store, StoreError, enter_observer_mode, get_object, get_object_by_name, list_objects,
    read_dropped_log, read_node_mode, write_node_mode,
};
use crate::types::{Command, LeaderRequest, NodeId, OrbNode, RawResource, Rejection, TypeConfig};
use crate::vip::{VipLeases, VipTable};
use crate::watch::{Feed, FeedItem, Watch, WatchError, WatchEvent, WatchHub};

/// spec 타입 `S`의 리소스.
pub type Typed<S> = Resource<S, <S as ResourceSpec>::Status>;

/// 리더가 없거나 닿지 않을 때 다시 시도하는 간격
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// 리더에게 한 번 전달할 때 기다리는 최대 시간(`AddVoter`는 따라잡기를 기다리므로 제외).
/// 연결을 끊지 않고 죽은 리더 호스트가 `write_timeout` 전체를 잡아 두면 안 된다.
/// keepalive가 알아채기까지는 이보다 오래 걸린다.
const FORWARD_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);

/// 저장 파일 이름. `data_dir` 아래에 둔다.
pub const DB_FILE: &str = "cluster.redb";

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub node_id: NodeId,
    pub data_dir: PathBuf,
    pub raft: openraft::Config,
    /// 쓰기가 커밋되기를 기다리는 최대 시간. 과반수가 없으면 이 시간 뒤 `Timeout`이 된다.
    pub write_timeout: Duration,
    /// 저장소를 파일 대신 메모리에 둔다(`data_dir`을 쓰지 않음, 다시 열면 비어 있음). 시험용(규모 시험)이다:
    /// 껐다 켜면 Raft 로그와 투표가 사라져 실제 노드에서는 안전하지 않다.
    #[doc(hidden)]
    pub in_memory: bool,
    /// 저장소의 멤버십에 투표 멤버가 없을 때 관찰 노드가 붙을 투표 멤버(노드 id → 클러스터 주소). 시험용(규모
    /// 시험이 노드를 learner로 따라잡게 하지 않고 빈 저장소로 바로 관찰 노드로 연다)이다.
    #[doc(hidden)]
    pub follow_seed: BTreeMap<NodeId, String>,
}

impl ClusterConfig {
    pub fn new(node_id: NodeId, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            node_id,
            data_dir: data_dir.into(),
            raft: openraft::Config::default(),
            write_timeout: Duration::from_secs(5),
            in_memory: false,
            follow_seed: BTreeMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClusterError {
    /// 상태 머신이 명령을 거부했다(이름 중복, revision 불일치 등).
    #[error(transparent)]
    Rejected(#[from] Rejection),
    /// 이 노드는 리더가 아니다. 알려진 리더가 있으면 함께 돌려준다.
    #[error("not the leader (leader: {leader_id:?})")]
    NotLeader {
        leader_id: Option<NodeId>,
        leader_addr: Option<String>,
    },
    /// `write_timeout` 안에 커밋되지 않았다. 과반수가 없을 때 생긴다.
    /// 명령은 나중에 커밋될 수도 있다.
    #[error("write was not committed within the timeout")]
    Timeout,
    /// 리더에게 요청을 전달하지 못했다.
    #[error("cannot reach the leader: {0}")]
    Unreachable(String),
    #[error("raft: {0}")]
    Raft(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("resource encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}

impl From<RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>> for ClusterError {
    fn from(e: RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>) -> Self {
        match e {
            RaftError::APIError(ClientWriteError::ForwardToLeader(f)) => ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr().to_string()),
            },
            other => ClusterError::Raft(other.to_string()),
        }
    }
}

/// 저장소의 로그·상태 머신으로 이 노드의 Raft를 띄운다(`Cluster::open`에 넘긴 네트워크와 설정으로). 관찰 노드가
/// 다시 Raft 구성원이 될 때도 쓴다.
type StartRaft = Arc<
    dyn Fn(LogStore, StateMachine) -> BoxFuture<'static, Result<Raft<TypeConfig>, ClusterError>>
        + Send
        + Sync,
>;

/// 노드 하나의 클러스터 저장소. 쓰기는 Raft로 복제하고, 읽기는 로컬 상태 머신에서 한다.
pub struct Cluster {
    id: NodeId,
    store: Store,
    node: NodeHandle,
    write_timeout: Duration,
    forwarder: Option<Arc<dyn Forwarder>>,
    /// Raft를 (다시) 띄우는 것
    start_raft: StartRaft,
    /// 관찰 노드가 투표 멤버를 따르는 통로. 없으면 관찰 노드가 될 수 없다.
    follow: Option<Arc<dyn FollowClient>>,
    /// 전환(관찰 노드 ↔ 멤버)과 종료를 하나씩 한다. 값은 종료했는지다.
    transition: tokio::sync::Mutex<bool>,
    /// 전환마다 시작과 끝에 하나씩 오른다(홀수면 전환 중). `stopped`가 계획된 전환을 멈춤으로 보지 않게 한다.
    epoch: tokio::sync::watch::Sender<u64>,
    /// 헬스 체크가 보는 적용 지연
    apply_lag: std::sync::Mutex<ApplyLag>,
    /// 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`). `Kind::Cluster` 변경으로 갱신한다.
    observers: Arc<RwLock<BTreeMap<NodeId, String>>>,
}

/// 이 노드가 클러스터에 무엇으로 참여하는지
#[derive(Clone)]
pub(crate) enum Backend {
    /// Raft 구성원(투표 멤버·learner). `confirm`은 두 리스 표가 함께 쓰는 리더 확인이다. `retired`는 이 Raft의
    /// 상태 머신이 물러났다는 표시다(관찰 노드가 될 때 저장소 스레드에서 세운다).
    Member {
        raft: Raft<TypeConfig>,
        confirm: ConfirmBatch,
        retired: Arc<AtomicBool>,
    },
    /// Raft 밖에서 투표 멤버 하나를 따르는 관찰 노드(설계 §6)
    Observer(Replica),
}

/// 노드의 지금 백엔드와 리더로서 받은 리스 표. memnet은 이것을 등록해 두고 RPC를 보낼 때마다 지금 백엔드를
/// 읽는다(백엔드가 바뀌어도 맞는 곳으로 간다).
#[derive(Clone)]
pub(crate) struct NodeHandle {
    id: NodeId,
    /// Follow를 받을 때 읽는 저장소와 적용 묶음
    db: Arc<Database>,
    hub: Arc<WatchHub>,
    /// Follow 스트림들이 함께 쓰는, 마지막 묶음을 나눈 프레임
    batch_frames: Arc<follow::BatchFrames>,
    backend: Arc<RwLock<Backend>>,
    /// 리더일 때 받은 HA 리스 갱신(메모리에만 둔다)
    leases: Arc<LeaseTable>,
    /// 리더일 때 받은 VIP 리스 갱신(메모리에만 둔다)
    vips: Arc<VipTable>,
}

impl NodeHandle {
    pub(crate) fn id(&self) -> NodeId {
        self.id
    }

    pub(crate) fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub(crate) fn hub(&self) -> &Arc<WatchHub> {
        &self.hub
    }

    pub(crate) fn batch_frames(&self) -> &follow::BatchFrames {
        &self.batch_frames
    }

    pub(crate) fn backend(&self) -> Backend {
        self.backend.read().unwrap().clone()
    }

    /// Raft 구성원이면 그 Raft
    pub(crate) fn raft(&self) -> Option<Raft<TypeConfig>> {
        match self.backend() {
            Backend::Member { raft, .. } => Some(raft),
            Backend::Observer(_) => None,
        }
    }

    /// 리더로서 요청을 처리할 것. 관찰 노드는 리더가 될 수 없어 `None`이다.
    pub(crate) fn leader_ctx(&self) -> Option<LeaderCtx> {
        match self.backend() {
            Backend::Member { raft, confirm, .. } => Some(LeaderCtx {
                raft,
                confirm,
                db: self.db.clone(),
                leases: self.leases.clone(),
                vips: self.vips.clone(),
            }),
            Backend::Observer(_) => None,
        }
    }

    /// 이 노드가 아는 리더로 만든 `NotLeader`
    pub(crate) fn not_leader(&self) -> ClusterError {
        match self.backend() {
            Backend::Member { raft, .. } => leader::not_leader(&raft.metrics().borrow()),
            Backend::Observer(replica) => replica.not_leader(),
        }
    }

    pub(crate) fn follow(
        &self,
        caller: NodeId,
        since: u64,
    ) -> Result<BoxStream<'static, FollowFrame>, ClusterError> {
        follow::serve(self.clone(), caller, since)
    }
}

/// 지금 알고 있는 멤버십. 값은 노드의 Raft 주소다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub leader: Option<NodeId>,
    pub voters: BTreeMap<NodeId, String>,
    pub learners: BTreeMap<NodeId, String>,
    /// Raft 밖의 관찰 노드(클러스터 정보의 `observers` 가운데 Raft 구성원이 아닌 것)
    pub observers: BTreeMap<NodeId, String>,
}

impl Members {
    /// 클러스터의 노드인지(투표 멤버·learner·관찰 노드)
    pub fn contains(&self, id: NodeId) -> bool {
        self.voters.contains_key(&id)
            || self.learners.contains_key(&id)
            || self.observers.contains_key(&id)
    }

    /// 노드의 클러스터 주소(관찰 노드 포함)
    pub fn addr(&self, id: NodeId) -> Option<&str> {
        self.voters
            .get(&id)
            .or_else(|| self.learners.get(&id))
            .or_else(|| self.observers.get(&id))
            .map(String::as_str)
    }
}

impl Cluster {
    /// 저장소를 열고 Raft를 시작한다. 처음 만든 클러스터는 `initialize`나
    /// 다른 노드의 `add_learner`로 멤버십을 받기 전까지 아무것도 하지 않는다.
    /// 관찰 노드였던 저장소도 멤버로 연다([`Cluster::become_member`]와 같이 스냅샷을 맞춘 뒤). Follow 통로가 없어
    /// 관찰 노드가 될 수 없다([`Cluster::open_node`]는 된다).
    pub async fn open<N>(config: ClusterConfig, network: N) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig> + Clone,
    {
        Self::open_as(config, network, None, Some(NodeMode::Member)).await
    }

    /// 저장소를 열고 관찰 노드로 시작한다(Raft를 띄우지 않음). 저장소를 관찰 노드 모드로 바꾸고(Raft 로그를 지움,
    /// [`Cluster::become_observer`]와 같다) 저장소의 멤버십에서 투표 멤버를 골라 `follow`로 붙어, 저장소의 적용
    /// 위치부터 이어 받는다. 쓰기와 리스 갱신은 `with_forwarder`의 전달자로 리더에 보낸다. `network`는 다시 Raft
    /// 구성원이 될 때 쓴다.
    pub async fn open_observer<N>(
        config: ClusterConfig,
        network: N,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig> + Clone,
    {
        Self::open_as(config, network, Some(follow), Some(NodeMode::Observer)).await
    }

    /// 저장소에 적힌 모드(`node_mode`, 없으면 멤버)로 연다. 노드 프로세스가 쓴다: 관찰 노드로 내려간 노드는 다시
    /// 시작해도 관찰 노드로 뜨고, 둘 사이를 [`Cluster::become_observer`]·[`Cluster::become_member`]로 바꿀 수 있다.
    pub async fn open_node<N>(
        config: ClusterConfig,
        network: N,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig> + Clone,
    {
        Self::open_as(config, network, Some(follow), None).await
    }

    /// `mode`가 없으면 저장소에 적힌 모드로 연다.
    async fn open_as<N>(
        config: ClusterConfig,
        network: N,
        follow: Option<Arc<dyn FollowClient>>,
        mode: Option<NodeMode>,
    ) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig> + Clone,
    {
        let store = open_store(&config)?;
        let start_raft = raft_starter(&config, network)?;
        let mode = match mode {
            Some(mode) => mode,
            None => read_node_mode(store.db())?,
        };
        let backend = match (mode, &follow) {
            (NodeMode::Observer, Some(follow)) => {
                store.writer().run(enter_observer_mode).await?;
                Backend::Observer(Replica::start(
                    config.node_id,
                    &store,
                    follow.clone(),
                    &config.follow_seed,
                )?)
            }
            _ => {
                store.writer().run(prepare_member).await?;
                start_member(&start_raft, &store).await?
            }
        };
        let mut cluster = Self::assemble(
            config.node_id,
            config.write_timeout,
            store,
            backend,
            start_raft,
        );
        cluster.follow = follow;
        Ok(cluster)
    }

    fn assemble(
        id: NodeId,
        write_timeout: Duration,
        store: Store,
        backend: Backend,
        start_raft: StartRaft,
    ) -> Self {
        let observers = track_observers(store.db(), store.watch_hub());
        Self {
            id,
            node: NodeHandle {
                id,
                db: store.db().clone(),
                hub: store.watch_hub().clone(),
                batch_frames: Arc::default(),
                backend: Arc::new(RwLock::new(backend)),
                leases: Arc::default(),
                vips: Arc::default(),
            },
            store,
            write_timeout,
            forwarder: None,
            start_raft,
            follow: None,
            transition: tokio::sync::Mutex::new(false),
            epoch: tokio::sync::watch::channel(0).0,
            apply_lag: Default::default(),
            observers,
        }
    }

    /// 팔로워가 받은 쓰기와 멤버십 변경을 이것으로 리더에 전달한다. 없으면 `NotLeader`를 돌려준다.
    pub fn with_forwarder(mut self, forwarder: Arc<dyn Forwarder>) -> Self {
        self.forwarder = Some(forwarder);
        self
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    /// 네트워크 계층이 받은 RPC를 넘기고, 테스트가 메트릭을 볼 때 쓴다. Raft 구성원이 아니면 `None`이다.
    pub fn raft(&self) -> Option<Raft<TypeConfig>> {
        self.node.raft()
    }

    /// 이 노드의 Raft(구성원인 백엔드). 관찰 노드면 아는 리더를 담은 `NotLeader`다.
    fn member(&self) -> Result<Raft<TypeConfig>, ClusterError> {
        self.node.raft().ok_or_else(|| self.node.not_leader())
    }

    /// 관찰 노드면 그 복제본
    fn replica(&self) -> Option<Replica> {
        match self.node.backend() {
            Backend::Member { .. } => None,
            Backend::Observer(replica) => Some(replica),
        }
    }

    /// 지금 무엇으로 참여하는지(관찰 노드면 `Observer`)
    pub fn mode(&self) -> NodeMode {
        match self.node.backend() {
            Backend::Member { .. } => NodeMode::Member,
            Backend::Observer(_) => NodeMode::Observer,
        }
    }

    /// 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`)에 이 노드가 있다(내려가는 중인 learner 포함).
    pub(crate) fn listed_as_observer(&self) -> bool {
        self.observers.read().unwrap().contains_key(&self.id)
    }

    /// 전환 중이다(이 사이의 Raft 오류는 다시 시도한다).
    fn transitioning(&self) -> bool {
        !self.epoch.borrow().is_multiple_of(2)
    }

    /// Raft 구성원(learner)에서 관찰 노드가 된다(설계 §6): Raft를 끄고, 한 트랜잭션에서 모드를 관찰 노드로 적고
    /// Raft 로그를 지운 뒤(투표는 남김) 저장소의 적용 위치부터 투표 멤버 하나를 따른다. 이미 관찰 노드면 아무것도
    /// 하지 않는다.
    ///
    /// 투표 멤버는 내려가지 않는다: 이 노드가 어느 구성(joint의 두 구성)에서도 투표 멤버가 아니고 그 멤버십이
    /// 커밋·적용됐을 때만 끈다(로그를 지운 노드가 커밋에 센 응답을 잃지 않게). 끈 뒤의 마지막 지표로 다시 보고,
    /// 그 사이 투표 멤버가 됐으면 같은 저장소로 Raft를 다시 띄우고 오류를 돌려준다.
    ///
    /// 도중에 취소하면 백엔드가 꺼진 Raft로 남으므로 끝까지 기다린다(키퍼는 따로 띄운 작업에서 부른다).
    pub async fn become_observer(&self) -> Result<(), ClusterError> {
        let closed = self.transition.lock().await;
        if *closed {
            return Err(ClusterError::Raft("the cluster store is shut down".into()));
        }
        let Backend::Member { raft, retired, .. } = self.node.backend() else {
            return Ok(());
        };
        let Some(follow) = self.follow.clone() else {
            return Err(ClusterError::Raft(
                "this node cannot follow a voting member (no follow client)".into(),
            ));
        };
        let before = may_leave_raft(&raft.metrics().borrow(), self.id);
        before.map_err(ClusterError::Raft)?;
        let _epoch = Transition::start(&self.epoch);
        raft.shutdown()
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))?;
        let after = may_leave_raft(&raft.metrics().borrow(), self.id);
        if let Err(why) = after {
            // 끄는 사이 투표 멤버가 됐다. 로그는 그대로이므로 다시 띄운다(다시 시작한 노드와 같다). 끈 Raft의 상태
            // 머신은 정상 경로처럼 저장소 스레드에서 물러나게 한다(새 상태 머신과 함께 늦은 적용을 쓰지 않게).
            self.store
                .writer()
                .run(move |_| {
                    retired.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await?;
            let backend = start_member(&self.start_raft, &self.store).await?;
            *self.node.backend.write().unwrap() = backend;
            return Err(ClusterError::Raft(why));
        }
        // openraft의 상태 머신 작업은 `shutdown` 뒤에도 남은 적용을 할 수 있다. 저장소 스레드에서 차례대로 물러남을
        // 표시하므로 이 쓰기 뒤의 적용은 쓰이지 않고, 앞의 적용은 이미 쓰였다(적용 리비전도 함께 냈다).
        self.store
            .writer()
            .run(move |db| {
                retired.store(true, Ordering::SeqCst);
                enter_observer_mode(db)
            })
            .await?;
        let replica = Replica::start(self.id, &self.store, follow, &BTreeMap::new())?;
        // 적용 지연은 백엔드마다 다른 것을 잰다(Raft 로그 대 붙은 노드·리더의 인덱스).
        *self.apply_lag.lock().unwrap() = ApplyLag::default();
        *self.node.backend.write().unwrap() = Backend::Observer(replica);
        tracing::info!(revision = self.revision(), "this node is now an observer");
        Ok(())
    }

    /// 관찰 노드에서 Raft 구성원이 된다(설계 §6): 복제본을 멈추고 지금 상태 전체를 스냅샷으로 저장한 뒤 모드를
    /// 멤버로 적고 Raft를 띄운다. 로그는 비어 있으므로 openraft 0.9.25가 시작할 때 마지막 로그를 적용 위치까지
    /// 지운 것으로(purge) 메우고 그 위치를 커밋으로 본다(`StorageHelper::get_initial_state`). 리더는 그 뒤의 로그를
    /// 보낸다. 이미 멤버면 아무것도 하지 않는다. Raft를 띄우지 못하면 다시 관찰 노드로 돌아간다.
    ///
    /// 도중에 취소하면 백엔드가 멈춘 복제본으로 남으므로 끝까지 기다린다.
    pub async fn become_member(&self) -> Result<(), ClusterError> {
        let closed = self.transition.lock().await;
        if *closed {
            return Err(ClusterError::Raft("the cluster store is shut down".into()));
        }
        let Backend::Observer(replica) = self.node.backend() else {
            return Ok(());
        };
        // 관찰 노드가 되며 지운 로그까지는 복제본으로 적용한 뒤에 Raft를 띄운다. 리더가 이 노드의 진행으로 그만큼을
        // 쥐고 있을 수 있다(목록에 오른 learner가 멤버십에서 빠지기 전에 관찰 노드가 됐고, 빠지지 않은 채 다시 멤버가
        // 되는 경우 — 내리던 중에 올리기, 따라잡기를 기다린 승격과 겹친 목록 쓰기). 그보다 짧은 로그로 답하면 리더가
        // 로그가 뒤로 간 것으로 본다(`store::enter_observer_mode`).
        let dropped = read_dropped_log(self.store.db())?;
        if self.revision() < dropped {
            return Err(ClusterError::Raft(format!(
                "this node has applied {} but must reach {dropped}, the log it dropped as an observer",
                self.revision()
            )));
        }
        let _epoch = Transition::start(&self.epoch);
        replica.shutdown().await;
        // 멈춘 복제본이 넣어 둔 쓰기는 이 쓰기보다 먼저 돈다(저장소 스레드의 차례).
        let started = async {
            self.store.writer().run(prepare_member).await?;
            start_member(&self.start_raft, &self.store).await
        }
        .await;
        match started {
            Ok(backend) => {
                *self.apply_lag.lock().unwrap() = ApplyLag::default();
                *self.node.backend.write().unwrap() = backend;
                tracing::info!(revision = self.revision(), "this node is now a Raft member");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot start Raft; staying an observer");
                self.store.writer().run(enter_observer_mode).await?;
                let follow = self.follow.clone().ok_or_else(|| {
                    ClusterError::Raft("this node cannot follow a voting member".into())
                })?;
                let replica = Replica::start(self.id, &self.store, follow, &BTreeMap::new())?;
                *self.node.backend.write().unwrap() = Backend::Observer(replica);
                Err(e)
            }
        }
    }

    /// 투표 멤버에게 물은 이 노드의 역할(Follow의 `Hello.role`). 아는 투표 멤버(저장된 멤버십) 가운데 처음 답한
    /// 것이고, 아무도 답하지 않으면 `None`이다. 멤버 모드인데 오래 리더를 모르는 노드가 꺼진 동안 관찰 노드로
    /// 내려졌는지 본다.
    pub(crate) async fn role_from_voters(&self) -> Option<NodeRole> {
        use futures_util::StreamExt;
        let follow = self.follow.clone()?;
        let voters = self.members().voters;
        for (id, addr) in voters.into_iter().filter(|(id, _)| *id != self.id) {
            let Ok(mut frames) = follow.follow(id, &addr, self.revision()).await else {
                continue;
            };
            let first = tokio::time::timeout(Duration::from_secs(3), frames.next()).await;
            if let Ok(Some(Ok(FollowFrame::Hello { role, .. }))) = first {
                return Some(role);
            }
        }
        None
    }

    /// 이 노드의 저장소에 적용한 마지막 인덱스(revision). blank·멤버십·거부된 명령도 올린다.
    pub fn revision(&self) -> u64 {
        self.store.watch_hub().revision()
    }

    /// 이 노드의 저장소가 `revision`까지 적용하기를 기다린다. 적용한 인덱스를 돌려주고, `timeout` 안에
    /// 안 되면 `Timeout`이다.
    pub async fn wait_revision(
        &self,
        revision: u64,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let mut rx = self.store.watch_hub().applied();
        match tokio::time::timeout(timeout, rx.wait_for(|a| *a >= revision)).await {
            Ok(Ok(applied)) => Ok(*applied),
            Ok(Err(_)) | Err(_) => Err(ClusterError::Timeout),
        }
    }

    /// 이 노드가 있다고 아는 가장 큰 리비전(적용한 것보다 작지 않다): 멤버는 받은 로그의 마지막 인덱스, 관찰 노드는
    /// 붙은 노드가 알렸거나 리더가 돌려준 인덱스다. 곧 적용될 리비전이라 기다릴 만하다.
    pub fn known_revision(&self) -> u64 {
        let known = match self.node.backend() {
            Backend::Member { raft, .. } => raft.metrics().borrow().last_log_index.unwrap_or(0),
            Backend::Observer(replica) => {
                let status = replica.status();
                status.upstream_applied.max(status.leader_applied)
            }
        };
        known.max(self.revision())
    }

    /// 새 클러스터를 만든다. 첫 노드에서 한 번만 부른다.
    pub async fn initialize(&self, members: BTreeMap<NodeId, OrbNode>) -> Result<(), ClusterError> {
        self.member()?
            .initialize(members)
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// 노드를 learner로 추가하고 로그를 따라잡을 때까지 기다린다. 리더에서 부른다.
    /// `write_timeout` 안에 끝나지 않으면 `Timeout`이다. 이때도 learner 추가는 나중에
    /// 커밋될 수 있고, 커밋되면 리더는 계속 따라잡게 한다.
    pub async fn add_learner(&self, id: NodeId, node: OrbNode) -> Result<(), ClusterError> {
        tokio::time::timeout(
            self.write_timeout,
            self.member()?.add_learner(id, node, true),
        )
        .await
        .map_err(|_| ClusterError::Timeout)??;
        Ok(())
    }

    /// 투표 멤버를 `voters`로 바꾼다. 빠진 노드는 learner로 남는다. 리더에서 부른다.
    /// `write_timeout` 안에 커밋되지 않으면 `Timeout`이다. 변경은 나중에 커밋될 수도 있다.
    pub async fn change_membership(&self, voters: BTreeSet<NodeId>) -> Result<(), ClusterError> {
        tokio::time::timeout(
            self.write_timeout,
            self.member()?.change_membership(voters, true),
        )
        .await
        .map_err(|_| ClusterError::Timeout)??;
        Ok(())
    }

    pub async fn create<S: ResourceSpec>(&self, resource: &Typed<S>) -> Result<u64, ClusterError> {
        self.write(Command::Create {
            kind: S::KIND,
            resource: to_raw(resource)?,
        })
        .await
    }

    pub async fn update_spec<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: Option<u64>,
        labels: BTreeMap<String, String>,
        spec: &S,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateSpec {
            kind: S::KIND,
            id,
            expected_revision,
            labels,
            spec: serde_json::to_value(spec)?,
        })
        .await
    }

    pub async fn update_status<S: ResourceSpec>(
        &self,
        id: Uuid,
        status: &S::Status,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateStatus {
            kind: S::KIND,
            id,
            status: serde_json::to_value(status)?,
        })
        .await
    }

    /// 리소스의 revision이 `expected_revision`일 때만 status를 바꾼다. 아니면 `RevisionMismatch`.
    /// 같은 status를 여러 곳에서 나눠 쓸 때, 읽은 뒤 다른 쪽이 쓴 값을 덮지 않게 한다.
    pub async fn update_status_checked<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: u64,
        status: &S::Status,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateStatusIf {
            kind: S::KIND,
            id,
            expected_revision,
            status: serde_json::to_value(status)?,
        })
        .await
    }

    pub async fn delete<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: Option<u64>,
    ) -> Result<u64, ClusterError> {
        self.write(Command::Delete {
            kind: S::KIND,
            id,
            expected_revision,
        })
        .await
    }

    /// 명령을 리더에 제안하고 커밋·적용될 때까지 기다린다. 성공하면 새 revision을 돌려준다.
    /// 상태 머신은 spec/status JSON이 `kind`와 맞는지 보지 않으므로 타입 있는 메서드로만 부른다.
    pub(crate) async fn write(&self, command: Command) -> Result<u64, ClusterError> {
        self.request(LeaderRequest::Write { command }).await
    }

    /// 요청을 리더에서 처리한다. 이 노드가 리더가 아니면 전달자로 리더에 보내고, 리더가 커밋한
    /// 인덱스까지 이 노드에도 적용되기를 기다린다(이 노드에서 곧바로 읽어도 결과가 보인다).
    /// 리더가 없거나(선거 중) 닿지 않으면 `write_timeout`까지 다시 시도하고, 그래도 안 되면
    /// `Timeout`이다. 전달 시도가 응답 없이 끝나면(시도 제한 시간, 보낸 뒤 연결 끊김) 요청이 이미
    /// 적용됐을 수 있으므로, 그 뒤 다시 보낸 만들기·삭제의 거부는 `already_applied`로 판단한다.
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        self.request_within(req, self.write_timeout).await
    }

    /// `request`와 같되 `write_timeout` 대신 `timeout`까지만 기다린다.
    async fn request_within(
        &self,
        req: LeaderRequest,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + timeout;
        // 응답 없이 끝난 전달 시도가 있었다(리더가 처리했을 수 있다).
        let mut maybe_applied = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let result = match self.execute_local(req.clone(), remaining).await {
                Err(ClusterError::NotLeader {
                    leader_id: Some(leader),
                    leader_addr: Some(addr),
                }) if leader != self.id
                    && let Some(forwarder) = &self.forwarder =>
                {
                    let attempt = if matches!(req, LeaderRequest::AddVoter { .. }) {
                        remaining
                    } else {
                        remaining.min(FORWARD_ATTEMPT_TIMEOUT)
                    };
                    match tokio::time::timeout(
                        attempt,
                        forwarder.forward(leader, &addr, req.clone()),
                    )
                    .await
                    {
                        Err(_) if tokio::time::Instant::now() >= deadline => {
                            Err(ClusterError::Timeout)
                        }
                        // 이번 시도만 제한에 걸렸다. 리더를 다시 읽고 다시 보낸다.
                        Err(_) => {
                            maybe_applied = true;
                            Err(ClusterError::Unreachable(
                                "forward attempt timed out".into(),
                            ))
                        }
                        // 자기 자신을 뺀 요청이면 리더가 더는 이 노드에 복제하지 않으므로
                        // 적용을 기다리면 제한 시간까지 멈춘다.
                        Ok(Ok(index)) if matches!(req, LeaderRequest::Remove { id } if id == self.id) => {
                            Ok(index)
                        }
                        Ok(Ok(index)) => {
                            let replica = self.replica();
                            let mut wait =
                                deadline.saturating_duration_since(tokio::time::Instant::now());
                            if let Some(replica) = &replica {
                                replica.heard_from_leader(index);
                                // 남은 시간보다 조금 일찍 그만둔다: 부른 쪽이 같은 제한 시간으로 이 future를 버리기
                                // 전에(리스 갱신) 아래에서 다른 투표 멤버로 옮기게.
                                wait -= wait / 10;
                            }
                            let applied = self.wait_revision(index, wait).await;
                            // 관찰 노드가 붙은 투표 멤버가 리더를 따라가지 못한다. 다른 투표 멤버로 옮긴다.
                            if applied.is_err()
                                && let Some(replica) = &replica
                            {
                                replica.follow_another();
                            }
                            Ok(index)
                        }
                        // 연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 둘 다 적용됐을 수 있다고 본다.
                        Ok(Err(e)) => {
                            maybe_applied |= matches!(e, ClusterError::Unreachable(_));
                            Err(e)
                        }
                    }
                }
                other => other,
            };
            if maybe_applied && let Some(done) = self.already_applied(&req, &result, deadline).await
            {
                return done;
            }
            let retry = match &result {
                Err(ClusterError::NotLeader {
                    leader_id: None, ..
                })
                | Err(ClusterError::Unreachable(_)) => true,
                Err(ClusterError::NotLeader { .. }) => self.forwarder.is_some(),
                // 전환으로 꺼진 Raft에 닿았다. 새 백엔드로 다시 한다.
                Err(ClusterError::Raft(_)) => self.transitioning(),
                _ => false,
            };
            if !retry {
                return result;
            }
            if tokio::time::Instant::now() + RETRY_INTERVAL >= deadline {
                return Err(ClusterError::Timeout);
            }
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    /// 다시 보낸 요청의 거부가 앞서 응답 없이 끝난 같은 요청이 적용된 결과면, 이 노드에도 적용되기를
    /// 기다려 성공으로 돌려준다. 만들기의 id는 요청한 쪽이 만든 UUID v7이라 같은 id가 있으면 자기
    /// 요청이다. 삭제는 조건 없는 것만 판단한다. 조건부 수정·삭제는 거부가 자기 요청 때문인지 알 수
    /// 없어 판단하지 않는다(`None`, 다시 보낸 요청이 `RevisionMismatch`·`NotFound`로 끝날 수 있다).
    async fn already_applied(
        &self,
        req: &LeaderRequest,
        result: &Result<u64, ClusterError>,
        deadline: tokio::time::Instant,
    ) -> Option<Result<u64, ClusterError>> {
        let LeaderRequest::Write { command } = req else {
            return None;
        };
        match (command, result) {
            (
                Command::Create { kind, resource },
                Err(ClusterError::Rejected(Rejection::AlreadyExists { id, .. })),
            ) if *id == resource.id => Some(self.wait_local(*kind, *id, true, deadline).await),
            // 조건부 삭제는 판단하지 않는다. 합류 토큰처럼 "지운 쪽만 성공"을 뜻할 수 있어, 다른
            // 쪽이 지운 것을 자기 성공으로 볼 수 없다.
            (
                Command::Delete {
                    kind,
                    id,
                    expected_revision: None,
                },
                Err(ClusterError::Rejected(Rejection::NotFound { .. })),
            ) => Some(self.wait_local(*kind, *id, false, deadline).await),
            _ => None,
        }
    }

    /// 이 노드에서 리소스가 보이거나(`present`) 사라질 때까지 기다린다. 보이면 그 revision을,
    /// 사라지면 이 노드가 적용한 인덱스를 돌려준다. `deadline`까지 안 되면 `Timeout`이다.
    /// 한 번에 적용된 묶음에 다른 쪽의 삭제까지 들어 있으면 "있음"을 보지 못하고 `Timeout`
    /// (=적용됐을 수 있음)으로 끝난다.
    async fn wait_local(
        &self,
        kind: Kind,
        id: Uuid,
        present: bool,
        deadline: tokio::time::Instant,
    ) -> Result<u64, ClusterError> {
        let db = self.store.db();
        let mut rx = self.store.watch_hub().applied();
        tokio::time::timeout_at(
            deadline,
            rx.wait_for(|_| get_object(db, kind, id).is_ok_and(|r| r.is_some() == present)),
        )
        .await
        .map_err(|_| ClusterError::Timeout)?
        .map_err(|_| ClusterError::Timeout)?;
        match get_object(db, kind, id)? {
            Some(r) if present => Ok(r.revision),
            _ => Ok(self.revision()),
        }
    }

    /// 이 노드가 리더로서 요청을 처리한다. 노드 간 네트워크가 다른 노드에서 전달받은 요청을 넘긴다.
    /// 리더가 아니면 `NotLeader`다(다시 전달하지 않는다).
    pub async fn execute_as_leader(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        self.execute_local(req, self.write_timeout).await
    }

    /// 이 노드에서 리더로서 처리한다. 관찰 노드는 리더가 될 수 없어 아는 리더를 담은 `NotLeader`다(전달할 곳).
    async fn execute_local(
        &self,
        req: LeaderRequest,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        match self.node.leader_ctx() {
            Some(ctx) => leader::execute(&ctx, req, timeout).await,
            None => Err(self.node.not_leader()),
        }
    }

    #[cfg(test)]
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// memnet이 등록해 두는 이 노드의 백엔드와 리스 표
    pub(crate) fn node_handle(&self) -> NodeHandle {
        self.node.clone()
    }

    /// 이 노드의 HA 리스를 갱신한다. 리더가 과반수의 리더인지 확인하고 갱신을 적은 뒤, 리더가 돌려준
    /// 적용 인덱스까지 이 노드에도 적용되어야 성공이다(그 사이 리더가 옮긴 VM이 이미 보인다).
    /// `timeout` 안에 끝나지 않으면 `Timeout`이다. 호출자는 보내기 전에 시각을 재고 성공하면 그
    /// 시각을 마지막 갱신으로 삼는다(설계 §3.2).
    pub async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let index = self
            .request_within(LeaderRequest::RenewLease { node: self.id }, timeout)
            .await?;
        // 전달 경로의 적용 기다림은 제한 시간이 지나도 성공으로 돌려주므로 여기서 다시 확인한다.
        self.wait_revision(
            index,
            deadline.saturating_duration_since(tokio::time::Instant::now()),
        )
        .await?;
        Ok(())
    }

    /// 이 노드의 VIP 리스를 갱신한다(`vips`는 이 노드가 가질 수 있는 VIP 주소). 규칙은 `renew_lease`와
    /// 같다: 성공하면 리더가 돌려준 적용 인덱스까지 이 노드에도 적용되어 있어, 곧바로 읽은 `VipHolder`에
    /// 그때까지 리더가 정한 주인이 보인다. 호출자는 보내기 전에 시각을 재고 그 시각부터 리스를 센다.
    pub async fn renew_vip(
        &self,
        vips: Vec<String>,
        timeout: Duration,
    ) -> Result<(), ClusterError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let index = self
            .request_within(
                LeaderRequest::RenewVip {
                    node: self.id,
                    vips,
                },
                timeout,
            )
            .await?;
        self.wait_revision(
            index,
            deadline.saturating_duration_since(tokio::time::Instant::now()),
        )
        .await?;
        Ok(())
    }

    /// 리더가 지금 임기에 받은 VIP 리스 갱신. 리더에서만 뜻이 있다.
    pub fn vip_leases(&self) -> VipLeases {
        match self.node.raft() {
            Some(raft) => self.node.vips.leases(&raft),
            None => VipLeases::default(),
        }
    }

    /// `node`의 VIP 갱신을 `end_vip_fence`까지 `Rejection::Fenced`로 거부한다. 리더가 그 노드의 VIP를
    /// 다른 노드로 옮기기 전에 표시하고, 표시한 뒤 `vip_leases`를 다시 읽어 여전히 끝났는지 확인한다.
    pub fn begin_vip_fence(&self, node: NodeId) {
        self.node.vips.begin_fence(node);
    }

    /// VIP 펜싱 표시를 지운다. 옮기는 쓰기의 결과를 안 뒤에 부른다.
    pub fn end_vip_fence(&self, node: NodeId) {
        self.node.vips.end_fence(node);
    }

    /// 리더가 지금 임기에 받은 리스 갱신. 리더에서만 뜻이 있다(리더인지는 `read_barrier`로 확인한다).
    pub fn leases(&self) -> Leases {
        match self.node.raft() {
            Some(raft) => self.node.leases.leases(&raft),
            None => Leases::default(),
        }
    }

    /// `node`를 펜싱 중으로 표시한다. `end_fence`까지 그 노드의 갱신은 `Rejection::Fenced`로
    /// 거부된다. 표시하기 직전에 받은 갱신이 있을 수 있으므로, 표시한 뒤 `leases`를 다시 읽어
    /// 그 노드의 리스가 여전히 끝났는지 확인하고 나서 복구한다.
    pub fn begin_fence(&self, node: NodeId) {
        self.node.leases.begin_fence(node);
    }

    /// 펜싱 중 표시를 지운다. 복구 쓰기가 끝난(리더에 적용된) 뒤에 부른다.
    pub fn end_fence(&self, node: NodeId) {
        self.node.leases.end_fence(node);
    }

    /// 이 노드가 아는 멤버십과 리더
    /// 관찰 노드는 저장소에 적용된 멤버십과 붙은 노드가 알린 리더다.
    pub fn members(&self) -> Members {
        let (leader, membership) = match self.node.backend() {
            Backend::Member { raft, .. } => {
                let m = raft.metrics().borrow().clone();
                (m.current_leader, m.membership_config.membership().clone())
            }
            Backend::Observer(replica) => (
                replica.leader().map(|(id, _)| id),
                replica
                    .membership()
                    .map(|m| m.membership().clone())
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "cannot read the stored membership");
                        Default::default()
                    }),
            ),
        };
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        let mut out = Members {
            leader,
            voters: BTreeMap::new(),
            learners: BTreeMap::new(),
            observers: BTreeMap::new(),
        };
        for (id, node) in membership.nodes() {
            let list = if voters.contains(id) {
                &mut out.voters
            } else {
                &mut out.learners
            };
            list.insert(*id, node.addr().to_string());
        }
        // 내리는 중(목록에 쓰고 아직 멤버십에서 빼기 전)인 노드는 Raft 구성원으로 센다.
        out.observers = self
            .observers
            .read()
            .unwrap()
            .iter()
            .filter(|(id, _)| membership.get_node(id).is_none())
            .map(|(id, addr)| (*id, addr.clone()))
            .collect();
        out
    }

    /// `id`가 이 노드가 아는 클러스터 노드(멤버십이나 관찰 노드 목록)인지. [`Cluster::members`]의 `contains`와 같되
    /// 목록을 만들지 않아 열린 Follow 스트림이 프레임마다 볼 수 있다.
    pub fn knows_node(&self, id: NodeId) -> bool {
        self.in_membership(id) || self.observers.read().unwrap().contains_key(&id)
    }

    /// `id`가 이 노드가 아는 Raft 멤버십(투표 멤버·learner)에 있는지
    pub fn in_membership(&self, id: NodeId) -> bool {
        match self.node.backend() {
            Backend::Member { raft, .. } => raft
                .metrics()
                .borrow()
                .membership_config
                .membership()
                .get_node(&id)
                .is_some(),
            Backend::Observer(replica) => replica
                .membership()
                .is_ok_and(|m| m.membership().get_node(&id).is_some()),
        }
    }

    /// 헬스 체크가 보는 이 노드의 Raft 상태. 부를 때마다 적용 지연을 다시 잰다([`RaftHealth`]).
    /// 관찰 노드는 붙은 노드의 진행이 [`QUORUM_ACK_MAX`] 안에 왔고 리더를 알면 `quorum`이고, 적용 지연은 붙은
    /// 노드가 알린 적용 인덱스나 리더가 돌려준 인덱스(전달한 쓰기·리스 갱신) 가운데 큰 것을 아직 적용하지 못한 시간이다.
    pub fn health(&self) -> RaftHealth {
        let raft = match self.node.backend() {
            Backend::Member { raft, .. } => raft,
            Backend::Observer(replica) => {
                let status = replica.status();
                let apply_lag = self.apply_lag.lock().unwrap().observe(
                    Some(status.upstream_applied.max(status.leader_applied)),
                    Some(self.revision()),
                    std::time::Instant::now(),
                );
                let leader = status.leader.map(|(id, _)| id);
                return RaftHealth {
                    leader,
                    quorum: leader.is_some()
                        && status
                            .last_progress
                            .is_some_and(|t| t.elapsed() <= QUORUM_ACK_MAX),
                    apply_lag,
                };
            }
        };
        let m = raft.metrics().borrow().clone();
        let apply_lag = self.apply_lag.lock().unwrap().observe(
            m.last_log_index,
            m.last_applied.map(|l| l.index),
            std::time::Instant::now(),
        );
        let quorum = match m.current_leader {
            None => false,
            Some(leader) if leader == self.id => m
                .millis_since_quorum_ack
                .is_some_and(|ms| Duration::from_millis(ms) <= QUORUM_ACK_MAX),
            Some(_) => true,
        };
        RaftHealth {
            leader: m.current_leader,
            quorum,
            apply_lag,
        }
    }

    /// 로컬 상태 머신에서 읽는다. 팔로워에서는 최신 커밋이 아직 반영되지 않았을 수 있다.
    /// 최신 값이 필요하면 먼저 `read_barrier`를 부른다.
    pub fn get<S: ResourceSpec>(&self, id: Uuid) -> Result<Option<Typed<S>>, ClusterError> {
        get_object(self.store.db(), S::KIND, id)?
            .map(from_raw)
            .transpose()
    }

    pub fn get_by_name<S: ResourceSpec>(
        &self,
        name: &str,
    ) -> Result<Option<Typed<S>>, ClusterError> {
        get_object_by_name(self.store.db(), S::KIND, name)?
            .map(from_raw)
            .transpose()
    }

    /// 한 종류의 리소스 전체와 그 목록의 revision. `watch(revision)`으로 이어서 구독한다.
    pub fn list<S: ResourceSpec>(&self) -> Result<(u64, Vec<Typed<S>>), ClusterError> {
        let (revision, raw) = list_objects(self.store.db(), S::KIND)?;
        let items = raw.into_iter().map(from_raw).collect::<Result<_, _>>()?;
        Ok((revision, items))
    }

    /// 이 노드가 아직 리더인지 과반수에게 확인하고, 그 시점의 커밋까지 로컬에 적용될 때까지
    /// 기다린다(선형화 읽기). 리더에서만 쓸 수 있다. 팔로워와 learner에서는 리더 정보를 담은
    /// `NotLeader`를 돌려준다.
    pub async fn read_barrier(&self) -> Result<(), ClusterError> {
        match self.member()?.ensure_linearizable().await {
            Ok(_) => Ok(()),
            Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f))) => {
                Err(ClusterError::NotLeader {
                    leader_id: f.leader_id,
                    leader_addr: f.leader_node.map(|n| n.addr().to_string()),
                })
            }
            Err(e) => Err(ClusterError::Raft(e.to_string())),
        }
    }

    /// `since` 이후에 커밋된 변경을 구독한다.
    pub fn watch(&self, since: u64) -> Result<Watch, WatchError> {
        self.store.watch_hub().subscribe(since)
    }

    /// 관찰 노드가 투표 멤버에게서 받는 스트림을 이 노드가 만든다(설계 §6). `since` 뒤의 적용 묶음과 1초마다
    /// 진행을 보내고, `since`가 보관한 기록보다 오래면 스냅샷부터 보낸다. 이 노드가 Raft 구성원이 아니면 `Hello`만
    /// 보내고 닫는다.
    pub fn follow(
        &self,
        caller: NodeId,
        since: u64,
    ) -> Result<BoxStream<'static, FollowFrame>, ClusterError> {
        self.node.follow(caller, since)
    }

    /// 지금 백엔드(Raft나 복제본)를 멈춘다. 진행 중인 전환이 끝나기를 기다리고, 그 뒤에는 전환하지 않는다.
    pub async fn shutdown(&self) -> Result<(), ClusterError> {
        let mut closed = self.transition.lock().await;
        *closed = true;
        if let Some(replica) = self.replica() {
            replica.shutdown().await;
            return Ok(());
        }
        self.member()?
            .shutdown()
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// Raft가 멈추면 그 이유와 함께 끝나는 future(저장소를 붙잡지 않는다). 저장소 쓰기가 실패하거나
    /// 저장소 스레드가 멈추면 openraft가 이 노드의 Raft를 멈춘다(`running_state`가 `Err`). `shutdown`으로
    /// 멈춰도 끝난다.
    /// 관찰 노드는 `shutdown`하면 끝난다. 계획된 전환(관찰 노드 ↔ 멤버)으로 멈춘 백엔드는 멈춤으로 보지 않고 새
    /// 백엔드를 이어 본다.
    pub fn stopped(&self) -> impl Future<Output = String> + Send + 'static {
        let backend = Arc::downgrade(&self.node.backend);
        let mut epoch = self.epoch.subscribe();
        async move {
            loop {
                let seen = *epoch.borrow_and_update();
                let Some(current) = backend.upgrade().map(|b| b.read().unwrap().clone()) else {
                    return "the cluster store was dropped".to_owned();
                };
                let ended = stop_signal(&current);
                drop(current);
                tokio::select! {
                    why = ended => {
                        if seen.is_multiple_of(2) && *epoch.borrow() == seen {
                            return why;
                        }
                        // 전환이 이 백엔드를 멈췄다. 전환이 끝나면 새 백엔드를 본다.
                        if epoch.wait_for(|e| e.is_multiple_of(2)).await.is_err() {
                            return why;
                        }
                    }
                    changed = epoch.changed() => {
                        if changed.is_err() {
                            return "the cluster store was dropped".to_owned();
                        }
                    }
                }
            }
        }
    }
}

/// 백엔드가 멈추면 그 이유와 함께 끝나는 future. 수신자만 쥐어 저장소를 붙잡지 않는다.
fn stop_signal(backend: &Backend) -> BoxFuture<'static, String> {
    match backend {
        Backend::Member { raft, .. } => {
            let mut rx = raft.metrics();
            Box::pin(async move {
                loop {
                    let state = rx.borrow_and_update().running_state.clone();
                    if let Err(e) = state {
                        return e.to_string();
                    }
                    // 보내는 쪽이 사라졌다: Raft 작업이 끝났다(panic 포함).
                    if rx.changed().await.is_err() {
                        return "the Raft task ended".to_owned();
                    }
                }
            })
        }
        Backend::Observer(replica) => {
            let mut stopped = replica.stopped();
            Box::pin(async move {
                let _ = stopped.wait_for(|s| *s).await;
                "the replica stopped".to_owned()
            })
        }
    }
}

/// 전환 하나의 표시: 만들 때와 버릴 때 `epoch`를 하나씩 올린다(홀수면 전환 중).
struct Transition<'a>(&'a tokio::sync::watch::Sender<u64>);

impl<'a> Transition<'a> {
    fn start(epoch: &'a tokio::sync::watch::Sender<u64>) -> Self {
        epoch.send_modify(|e| *e += 1);
        Self(epoch)
    }
}

impl Drop for Transition<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|e| *e += 1);
    }
}

/// 이 노드가 Raft를 끄고 로그를 지워도 되는지: 어느 구성(joint의 두 구성)에서도 투표 멤버가 아니고, 그 멤버십이
/// 커밋·적용됐다(적용 위치 안). 커밋되지 않은 멤버십이 이 노드를 내린 것이면, 그것이 잘려 이 노드가 다시 투표
/// 멤버가 될 수 있다(그때 지운 로그로 투표하면 커밋된 항목을 잃을 수 있다).
fn may_leave_raft(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> Result<(), String> {
    if m.membership_config
        .membership()
        .voter_ids()
        .any(|v| v == id)
    {
        return Err("a voting member cannot become an observer".into());
    }
    let membership = m.membership_config.log_id().map(|l| l.index);
    if membership > m.last_applied.map(|l| l.index) {
        return Err("the membership that made this node a learner is not applied yet".into());
    }
    Ok(())
}

/// 저장소의 Raft를 띄워 멤버 백엔드를 만든다.
async fn start_member(start_raft: &StartRaft, store: &Store) -> Result<Backend, ClusterError> {
    let state_machine = store.state_machine();
    let retired = state_machine.retired_flag();
    let raft = start_raft(store.log_store(), state_machine).await?;
    Ok(Backend::Member {
        confirm: ConfirmBatch::for_raft(raft.clone()),
        raft,
        retired,
    })
}

/// 관찰 노드였던 저장소면 Raft를 띄우기 전에 지금 상태를 스냅샷으로 저장하고 모드를 멤버로 적는다. 지운 로그
/// 대신 openraft가 뒤처진 노드에 보낼 스냅샷이 적용 위치까지 있어야 한다.
fn prepare_member(db: &Database) -> Result<(), StoreError> {
    if read_node_mode(db)? == NodeMode::Observer {
        save_current_snapshot(db)?;
        write_node_mode(db, NodeMode::Member)?;
    }
    Ok(())
}

/// `network`와 설정으로 이 노드의 Raft를 띄우는 것
fn raft_starter<N>(config: &ClusterConfig, network: N) -> Result<StartRaft, ClusterError>
where
    N: RaftNetworkFactory<TypeConfig> + Clone,
{
    let raft_config = Arc::new(
        config
            .raft
            .clone()
            .validate()
            .map_err(|e| ClusterError::Raft(e.to_string()))?,
    );
    let id = config.node_id;
    Ok(Arc::new(move |log, state_machine| {
        let (network, raft_config) = (network.clone(), raft_config.clone());
        Box::pin(async move {
            Raft::new(id, raft_config, network, log, state_machine)
                .await
                .map_err(|e| ClusterError::Raft(e.to_string()))
        })
    }))
}

/// 클러스터 정보의 관찰 노드 목록을 읽어 두고, `Kind::Cluster` 변경을 따라 고치는 작업을 띄운다. 작업은
/// 저장소가 닫히거나(피드가 `Closed`) 목록을 쥔 `Cluster`가 사라지면 끝난다.
fn track_observers(
    db: &Arc<Database>,
    hub: &Arc<WatchHub>,
) -> Arc<RwLock<BTreeMap<NodeId, String>>> {
    let cache = Arc::new(RwLock::new(BTreeMap::new()));
    // 구독한 뒤 읽어야 그 사이의 변경을 놓치지 않는다.
    let feed = hub.feed(hub.revision()).ok();
    *cache.write().unwrap() = read_observers(db);
    let (db, hub, weak) = (
        Arc::downgrade(db),
        Arc::downgrade(hub),
        Arc::downgrade(&cache),
    );
    tokio::spawn(async move {
        let mut feed = feed;
        loop {
            if let Some(mut f) = feed.take()
                && !follow_observers(&mut f, &weak).await
            {
                return;
            }
            // 처음이거나, 스냅샷 설치(`Reset`)·`Lagged` 뒤: 다시 구독하고 저장소에서 다시 읽는다.
            let (Some(db), Some(hub), Some(cache)) = (db.upgrade(), hub.upgrade(), weak.upgrade())
            else {
                return;
            };
            feed = hub.feed(hub.revision()).ok();
            *cache.write().unwrap() = read_observers(&db);
            if feed.is_none() {
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    });
    cache
}

/// 피드의 `Kind::Cluster` 변경으로 목록을 고친다. 다시 읽어야 하면(`Reset`·`Lagged`) `true`, 끝나야 하면
/// (저장소가 닫힘, 목록이 사라짐) `false`다.
async fn follow_observers(feed: &mut Feed, cache: &Weak<RwLock<BTreeMap<NodeId, String>>>) -> bool {
    loop {
        let batch = match feed.next().await {
            Ok(FeedItem::Batch(batch)) => batch,
            Ok(FeedItem::Reset { .. }) | Err(WatchError::Lagged | WatchError::Expired { .. }) => {
                return true;
            }
            Err(WatchError::Closed) => return false,
        };
        if cache.strong_count() == 0 {
            return false;
        }
        for event in &batch.events {
            let observers = match event {
                WatchEvent::Put {
                    kind: Kind::Cluster,
                    resource,
                } => observers_of(resource),
                WatchEvent::Deleted {
                    kind: Kind::Cluster,
                    ..
                } => BTreeMap::new(),
                _ => continue,
            };
            let Some(cache) = cache.upgrade() else {
                return false;
            };
            *cache.write().unwrap() = observers;
        }
    }
}

/// 저장소의 클러스터 정보에 적힌 관찰 노드
fn read_observers(db: &Database) -> BTreeMap<NodeId, String> {
    match list_objects(db, Kind::Cluster) {
        Ok((_, list)) => list.first().map(observers_of).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the cluster info");
            BTreeMap::new()
        }
    }
}

pub(crate) fn observers_of(info: &RawResource) -> BTreeMap<NodeId, String> {
    match serde_json::from_value::<ClusterStatus>(info.status.clone()) {
        Ok(status) => status
            .observers
            .into_iter()
            .map(|(id, addr)| (NodeId(id), addr))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the observers in the cluster info");
            BTreeMap::new()
        }
    }
}

fn open_store(config: &ClusterConfig) -> Result<Store, ClusterError> {
    if config.in_memory {
        tracing::warn!(
            "the cluster store is in memory (test only): the Raft log and votes are lost on restart"
        );
        Ok(Store::open_in_memory()?)
    } else {
        std::fs::create_dir_all(&config.data_dir)?;
        Ok(Store::open(&config.data_dir.join(DB_FILE))?)
    }
}

fn to_raw<S: ResourceSpec>(r: &Typed<S>) -> Result<RawResource, serde_json::Error> {
    Ok(Resource {
        id: r.id,
        name: r.name.clone(),
        revision: r.revision,
        labels: r.labels.clone(),
        spec: serde_json::to_value(&r.spec)?,
        status: serde_json::to_value(&r.status)?,
    })
}

fn from_raw<S: ResourceSpec>(r: RawResource) -> Result<Typed<S>, ClusterError> {
    Ok(Resource {
        id: r.id,
        name: r.name,
        revision: r.revision,
        labels: r.labels,
        spec: serde_json::from_value(r.spec)?,
        status: serde_json::from_value(r.status)?,
    })
}

#[cfg(test)]
mod tests {
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tokio::time::Instant;

    use super::*;
    use crate::memnet::MemRouter;
    use crate::watch::FeedItem;

    const SLOW_WRITE: Duration = Duration::from_secs(3);

    fn volume(name: &str) -> Volume {
        Resource::new(
            name,
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1 << 30,
                format: ImageFormat::Qcow2,
            },
        )
    }

    /// 저장소 쓰기가 3초 걸리는 팔로워가 있어도 (작업자 스레드가 하나뿐인) 실행기는 막히지 않는다.
    /// 리더의 하트비트와 리스 갱신(`confirm_leader` 두 번)은 시간 안에 돌고 리더도 그대로다. 쓰기를
    /// 실행기 스레드에서 동기로 하면 3초 동안 모든 노드가 멈춘다.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_store_on_a_follower_does_not_stall_heartbeats_or_lease_checks() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let mut nodes = BTreeMap::new();
        for i in 1..=3u64 {
            let id = NodeId::from(i);
            let mut config = ClusterConfig::new(id, root.path().join(i.to_string()));
            config.raft = openraft::Config {
                heartbeat_interval: 50,
                election_timeout_min: 1000,
                election_timeout_max: 2000,
                ..Default::default()
            };
            let cluster = Cluster::open(config, router.network(id)).await.unwrap();
            router.register(id, &cluster);
            nodes.insert(id, cluster);
        }
        let members = (1..=3u64)
            .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
            .collect();
        let first = &nodes[&NodeId::from(1)];
        first.initialize(members).await.unwrap();
        let leader = loop {
            if let Some(l) = first.raft().unwrap().metrics().borrow().current_leader
                && nodes.values().all(|n| n.members().leader == Some(l))
            {
                break l;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let term = nodes[&leader]
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .current_term;
        let slow = *nodes.keys().find(|id| **id != leader).unwrap();
        // openraft 0.9의 Raft 루프는 자기 로그 쓰기가 끝나기를 기다리므로 느린 노드의 루프는 3초 멈추고,
        // 그 뒤 선거에 나설 수 있다. 여기서 보는 것은 실행기와 다른 노드라 선거는 끈다.
        nodes[&slow].raft().unwrap().runtime_config().elect(false);

        // 느린 팔로워의 저장소 스레드를 3초 붙잡은 뒤 쓴다. 리더와 다른 팔로워로 커밋된다.
        nodes[&slow]
            .store
            .writer()
            .submit(|_| std::thread::sleep(SLOW_WRITE))
            .unwrap();
        let stalled = Instant::now();
        let v = volume("v");
        let index = nodes[&leader].create(&v).await.unwrap();
        assert!(stalled.elapsed() < Duration::from_millis(500));
        let mut rounds = 0;
        while stalled.elapsed() < SLOW_WRITE {
            let started = Instant::now();
            nodes[&leader]
                .renew_lease(Duration::from_millis(500))
                .await
                .unwrap();
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{:?}",
                started.elapsed()
            );
            rounds += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(rounds >= 5, "{rounds}");
        let m = nodes[&leader].raft().unwrap().metrics().borrow().clone();
        assert_eq!((m.current_leader, m.current_term), (Some(leader), term));
        // 느린 팔로워도 붙잡힌 뒤에 차례대로 따라온다.
        nodes[&slow]
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .applied_index_at_least(Some(index), "slow follower caught up")
            .await
            .unwrap();
        assert!(nodes[&slow].get::<VolumeSpec>(v.id).unwrap().is_some());
        for node in nodes.values() {
            node.shutdown().await.unwrap();
        }
    }

    /// 저장소 쓰기가 panic해 저장소 스레드가 멈추면 다음 쓰기에서 openraft가 Raft를 멈추고
    /// `stopped`가 그 이유와 함께 끝난다.
    #[tokio::test]
    async fn stopped_ends_when_a_store_write_panics() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let id = NodeId::from(1u64);
        let cluster = Cluster::open(ClusterConfig::new(id, root.path()), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(BTreeMap::from([(id, OrbNode::single("node-1"))]))
            .await
            .unwrap();
        cluster.create(&volume("a")).await.unwrap();
        let stopped = cluster.stopped();
        tokio::pin!(stopped);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut stopped)
                .await
                .is_err()
        );

        cluster
            .store
            .writer()
            .submit(|_| panic!("store bug"))
            .unwrap();
        assert!(cluster.create(&volume("b")).await.is_err());
        let why = tokio::time::timeout(Duration::from_secs(10), stopped)
            .await
            .unwrap();
        assert!(why.contains("the store thread has stopped"), "{why}");
        let _ = cluster.shutdown().await;
    }

    /// learner가 된 멤버십이 지표에 적용으로 보일 때까지 기다린다.
    async fn wait_may_leave(c: &Cluster) {
        let id = c.id();
        c.raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .metrics(
                |m| may_leave_raft(m, id).is_ok(),
                "the learner membership applied",
            )
            .await
            .unwrap();
    }

    async fn open_node(router: &MemRouter, root: &std::path::Path, i: u64) -> Cluster {
        let id = NodeId::from(i);
        let cluster = Cluster::open(
            ClusterConfig::new(id, root.join(i.to_string())),
            router.network(id),
        )
        .await
        .unwrap();
        router.register(id, &cluster);
        cluster
    }

    /// 적용 리비전은 이벤트가 없는 항목(초기 멤버십, 리더의 blank)에서도 오르고, 그 리비전을 기다리는 쪽이
    /// 풀린다. 오지 않는 리비전은 제한 시간에 `Timeout`이다.
    #[tokio::test]
    async fn wait_revision_follows_entries_without_events_and_times_out() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let node = open_node(&router, root.path(), 1).await;
        assert_eq!(node.revision(), 0);
        node.initialize(BTreeMap::from([(node.id(), OrbNode::single("node-1"))]))
            .await
            .unwrap();
        // 멤버십(0)과 blank(1)뿐이라 리소스 변경은 없다.
        assert!(
            node.wait_revision(1, Duration::from_secs(10))
                .await
                .unwrap()
                >= 1
        );
        let (revision, volumes) = node.list::<VolumeSpec>().unwrap();
        assert!(volumes.is_empty());
        assert_eq!(revision, node.revision());

        let next = node.revision() + 1;
        let started = Instant::now();
        assert!(matches!(
            node.wait_revision(next, Duration::from_millis(200)).await,
            Err(ClusterError::Timeout)
        ));
        assert!(started.elapsed() >= Duration::from_millis(200));

        // 기다리는 중에 적용되면 풀린다.
        let v = volume("a");
        let (waited, written) = tokio::join!(
            node.wait_revision(next, Duration::from_secs(5)),
            node.create(&v)
        );
        assert_eq!(waited.unwrap(), written.unwrap());
        node.shutdown().await.unwrap();
    }

    /// 투표 멤버는 관찰 노드가 되지 않는다(Raft도 끄지 않는다). learner는 관찰 노드가 되며 Raft 로그와 저장된
    /// 커밋 위치를 지우고 투표와 상태 머신은 남긴다. 다시 멤버가 되면 openraft가 지운 로그를 적용 위치까지 지운
    /// 것(purge)으로 메우고 스냅샷이 그 위치에 있으며, 리더가 그 뒤의 로그를 보낸다.
    #[tokio::test]
    async fn a_learner_becomes_an_observer_and_back_but_a_voter_does_not() {
        use crate::store::{KEY_COMMITTED, KEY_LAST_PURGED, KEY_VOTE, LOG, META, get_json};
        use redb::{ReadableDatabase, ReadableTableMetadata};

        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let leader = open_node(&router, root.path(), 1).await;
        leader
            .initialize(BTreeMap::from([(leader.id(), OrbNode::single("node-1"))]))
            .await
            .unwrap();
        leader
            .wait_revision(1, Duration::from_secs(10))
            .await
            .unwrap();
        let id = NodeId::from(2u64);
        let mut learner = Cluster::open_node(
            ClusterConfig::new(id, root.path().join("2")),
            router.network(id),
            router.follower(id),
        )
        .await
        .unwrap()
        .with_forwarder(router.forwarder(id));
        learner.write_timeout = Duration::from_secs(10);
        router.register(id, &learner);
        leader
            .add_learner(id, OrbNode::single("node-2"))
            .await
            .unwrap();
        leader.create(&volume("a")).await.unwrap();
        learner
            .wait_revision(leader.revision(), Duration::from_secs(10))
            .await
            .unwrap();

        // 투표 멤버(리더)는 거절하고 Raft는 그대로 돈다.
        assert!(leader.become_observer().await.is_err());
        assert_eq!(leader.mode(), NodeMode::Member);
        leader.create(&volume("b")).await.unwrap();

        let meta = |c: &Cluster| {
            let txn = c.store.db().begin_read().unwrap();
            let meta = txn.open_table(META).unwrap();
            let vote: Option<openraft::Vote<NodeId>> = get_json(&meta, KEY_VOTE).unwrap();
            let committed: Option<Option<openraft::LogId<NodeId>>> =
                get_json(&meta, KEY_COMMITTED).unwrap();
            let logs = txn.open_table(LOG).unwrap().len().unwrap();
            (vote, committed, logs)
        };
        learner
            .wait_revision(leader.revision(), Duration::from_secs(10))
            .await
            .unwrap();
        wait_may_leave(&learner).await;
        let (vote, _, logs) = meta(&learner);
        assert!(vote.is_some() && logs > 0);
        learner.become_observer().await.unwrap();
        assert_eq!(learner.mode(), NodeMode::Observer);
        assert!(learner.raft().is_none());
        assert_eq!(
            read_node_mode(learner.store.db()).unwrap(),
            NodeMode::Observer
        );
        let (kept, committed, logs) = meta(&learner);
        assert_eq!((kept, committed, logs), (vote, None, 0));
        let observed = learner.revision();

        // 리더의 멤버십에 learner로 남아 있으므로 다시 멤버가 될 수 있다.
        learner.become_member().await.unwrap();
        assert_eq!(
            read_node_mode(learner.store.db()).unwrap(),
            NodeMode::Member
        );
        // 새 Raft가 첫 지표를 낼 때까지 기다린다.
        let m = learner
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.purged.is_some(), "the restarted Raft reports")
            .await
            .unwrap();
        assert_eq!(m.purged.map(|l| l.index), Some(observed));
        assert_eq!(m.snapshot.map(|l| l.index), Some(observed));
        let purged: Option<openraft::LogId<NodeId>> = {
            let txn = learner.store.db().begin_read().unwrap();
            get_json(&txn.open_table(META).unwrap(), KEY_LAST_PURGED).unwrap()
        };
        assert_eq!(purged.map(|l| l.index), Some(observed));
        let written = leader.create(&volume("c")).await.unwrap();
        learner
            .wait_revision(written, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            learner.list::<VolumeSpec>().unwrap().1,
            leader.list::<VolumeSpec>().unwrap().1
        );
        // 쓰기도 전달된다(관찰 노드였던 동안의 상태에 이어서).
        learner.create(&volume("d")).await.unwrap();
        learner.shutdown().await.unwrap();
        leader.shutdown().await.unwrap();
    }

    /// 관찰 노드가 되며 지운 Raft 로그의 마지막 인덱스를 남기고, 복제본이 거기까지 적용하기 전에는 다시 멤버가 되지
    /// 않는다. 리더가 그만큼을 이 노드의 진행으로 쥐고 있을 수 있어(멤버십에서 빠지지 않은 채 관찰 노드가 됐다),
    /// 그보다 짧은 로그로 Raft를 띄우면 리더가 로그가 뒤로 간 것으로 본다.
    #[tokio::test]
    async fn an_observer_rejoins_raft_only_after_applying_the_log_it_dropped() {
        use crate::store::{KEY_DROPPED_LOG, META, put_json};

        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let leader = open_node(&router, root.path(), 1).await;
        leader
            .initialize(BTreeMap::from([(leader.id(), OrbNode::single("node-1"))]))
            .await
            .unwrap();
        let id = NodeId::from(2u64);
        let learner = Cluster::open_node(
            ClusterConfig::new(id, root.path().join("2")),
            router.network(id),
            router.follower(id),
        )
        .await
        .unwrap()
        .with_forwarder(router.forwarder(id));
        router.register(id, &learner);
        leader
            .add_learner(id, OrbNode::single("node-2"))
            .await
            .unwrap();
        leader.create(&volume("a")).await.unwrap();
        learner
            .wait_revision(leader.revision(), Duration::from_secs(10))
            .await
            .unwrap();
        wait_may_leave(&learner).await;
        let last_log = learner.raft().unwrap().metrics().borrow().last_log_index;
        learner.become_observer().await.unwrap();
        assert_eq!(
            Some(read_dropped_log(learner.store.db()).unwrap()),
            last_log
        );

        // learner일 때 적용한 것보다 세 항목 더 받았던 것처럼 한다.
        let dropped = learner.revision() + 3;
        learner
            .store
            .writer()
            .run(move |db| {
                let txn = db.begin_write()?;
                put_json(&mut txn.open_table(META)?, KEY_DROPPED_LOG, &dropped)?;
                txn.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(learner.become_member().await.is_err());
        assert_eq!(learner.mode(), NodeMode::Observer);
        for i in 0..3 {
            leader.create(&volume(&format!("b{i}"))).await.unwrap();
        }
        learner
            .wait_revision(dropped, Duration::from_secs(10))
            .await
            .unwrap();
        learner.become_member().await.unwrap();
        assert_eq!(learner.mode(), NodeMode::Member);
        let written = leader.create(&volume("c")).await.unwrap();
        learner
            .wait_revision(written, Duration::from_secs(10))
            .await
            .unwrap();
        learner.shutdown().await.unwrap();
        leader.shutdown().await.unwrap();
    }

    /// 계획된 전환으로 Raft나 복제본이 멈춰도 `stopped`는 끝나지 않고 새 백엔드를 이어 본다. 종료하면 끝난다.
    #[tokio::test]
    async fn stopped_ignores_planned_transitions() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let leader = open_node(&router, root.path(), 1).await;
        leader
            .initialize(BTreeMap::from([(leader.id(), OrbNode::single("node-1"))]))
            .await
            .unwrap();
        let id = NodeId::from(2u64);
        let learner = Cluster::open_node(
            ClusterConfig::new(id, root.path().join("2")),
            router.network(id),
            router.follower(id),
        )
        .await
        .unwrap();
        router.register(id, &learner);
        leader
            .add_learner(id, OrbNode::single("node-2"))
            .await
            .unwrap();
        learner
            .wait_revision(leader.revision(), Duration::from_secs(10))
            .await
            .unwrap();
        wait_may_leave(&learner).await;
        let stopped = learner.stopped();
        tokio::pin!(stopped);
        learner.become_observer().await.unwrap();
        learner.become_member().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut stopped)
                .await
                .is_err()
        );
        learner.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), stopped)
            .await
            .unwrap();
        leader.shutdown().await.unwrap();
    }

    /// 피드는 멤버십 항목처럼 이벤트가 없는 항목도 적용 표시(와 멤버십)로 싣는다. learner의 적용 리비전도
    /// 이벤트 없이 리더를 따라온다.
    #[tokio::test]
    async fn feed_marks_membership_entries_without_events() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let leader = open_node(&router, root.path(), 1).await;
        leader
            .initialize(BTreeMap::from([(leader.id(), OrbNode::single("node-1"))]))
            .await
            .unwrap();
        leader
            .wait_revision(1, Duration::from_secs(10))
            .await
            .unwrap();
        let learner = open_node(&router, root.path(), 2).await;
        let mut feed = leader.store.watch_hub().feed(leader.revision()).unwrap();
        leader
            .add_learner(learner.id(), OrbNode::single("node-2"))
            .await
            .unwrap();

        let FeedItem::Batch(batch) = feed.next().await.unwrap() else {
            panic!("expected a batch");
        };
        assert!(batch.events.is_empty());
        assert_eq!(batch.applied.index, leader.revision());
        let membership = batch.membership.as_ref().expect("a membership entry");
        assert!(
            membership
                .membership()
                .learner_ids()
                .any(|id| id == learner.id())
        );
        assert_eq!(
            learner
                .wait_revision(leader.revision(), Duration::from_secs(10))
                .await
                .unwrap(),
            leader.revision()
        );
        learner.shutdown().await.unwrap();
        leader.shutdown().await.unwrap();
    }
}
