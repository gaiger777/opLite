use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use uuid::Uuid;

use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
use crate::types::{Command, LeaderRequest, NodeId, RawResource, Rejection, TypeConfig};
use crate::vip::{VipLeases, VipTable};
use crate::watch::{Watch, WatchError};

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
}

impl ClusterConfig {
    pub fn new(node_id: NodeId, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            node_id,
            data_dir: data_dir.into(),
            raft: openraft::Config::default(),
            write_timeout: Duration::from_secs(5),
            in_memory: false,
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

impl From<RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>> for ClusterError {
    fn from(e: RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>) -> Self {
        match e {
            RaftError::APIError(ClientWriteError::ForwardToLeader(f)) => ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr),
            },
            other => ClusterError::Raft(other.to_string()),
        }
    }
}

/// 노드 하나의 클러스터 저장소. 쓰기는 Raft로 복제하고, 읽기는 로컬 상태 머신에서 한다.
pub struct Cluster {
    id: NodeId,
    store: Store,
    node: NodeHandle,
    write_timeout: Duration,
    forwarder: Option<Arc<dyn Forwarder>>,
    /// 헬스 체크가 보는 적용 지연
    apply_lag: std::sync::Mutex<ApplyLag>,
}

/// 이 노드가 클러스터에 무엇으로 참여하는지
#[derive(Clone)]
pub(crate) enum Backend {
    /// Raft 구성원(투표 멤버·learner). `confirm`은 두 리스 표가 함께 쓰는 리더 확인이다.
    Member {
        raft: Raft<TypeConfig>,
        confirm: ConfirmBatch,
    },
}

/// 노드의 지금 백엔드와 리더로서 받은 리스 표. memnet은 이것을 등록해 두고 RPC를 보낼 때마다 지금 백엔드를
/// 읽는다(백엔드가 바뀌어도 맞는 곳으로 간다).
#[derive(Clone)]
pub(crate) struct NodeHandle {
    backend: Arc<RwLock<Backend>>,
    /// 리더일 때 받은 HA 리스 갱신(메모리에만 둔다)
    leases: Arc<LeaseTable>,
    /// 리더일 때 받은 VIP 리스 갱신(메모리에만 둔다)
    vips: Arc<VipTable>,
}

impl NodeHandle {
    fn backend(&self) -> Backend {
        self.backend.read().unwrap().clone()
    }

    /// Raft 구성원이면 그 Raft
    pub(crate) fn raft(&self) -> Option<Raft<TypeConfig>> {
        match self.backend() {
            Backend::Member { raft, .. } => Some(raft),
        }
    }

    pub(crate) fn leader_ctx(&self) -> LeaderCtx {
        let Backend::Member { raft, confirm } = self.backend();
        LeaderCtx {
            raft,
            confirm,
            leases: self.leases.clone(),
            vips: self.vips.clone(),
        }
    }
}

/// 지금 알고 있는 멤버십. 값은 노드의 Raft 주소다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub leader: Option<NodeId>,
    pub voters: BTreeMap<NodeId, String>,
    pub learners: BTreeMap<NodeId, String>,
}

impl Members {
    pub fn contains(&self, id: NodeId) -> bool {
        self.voters.contains_key(&id) || self.learners.contains_key(&id)
    }

    pub fn addr(&self, id: NodeId) -> Option<&str> {
        self.voters
            .get(&id)
            .or_else(|| self.learners.get(&id))
            .map(String::as_str)
    }
}

impl Cluster {
    /// 저장소를 열고 Raft를 시작한다. 처음 만든 클러스터는 `initialize`나
    /// 다른 노드의 `add_learner`로 멤버십을 받기 전까지 아무것도 하지 않는다.
    pub async fn open<N>(config: ClusterConfig, network: N) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig>,
    {
        let store = if config.in_memory {
            tracing::warn!(
                "the cluster store is in memory (test only): the Raft log and votes are lost on restart"
            );
            Store::open_in_memory()?
        } else {
            std::fs::create_dir_all(&config.data_dir)?;
            Store::open(&config.data_dir.join(DB_FILE))?
        };
        let raft_config = Arc::new(
            config
                .raft
                .validate()
                .map_err(|e| ClusterError::Raft(e.to_string()))?,
        );
        let raft = Raft::new(
            config.node_id,
            raft_config,
            network,
            store.log_store(),
            store.state_machine(),
        )
        .await
        .map_err(|e| ClusterError::Raft(e.to_string()))?;
        Ok(Self {
            id: config.node_id,
            store,
            node: NodeHandle {
                backend: Arc::new(RwLock::new(Backend::Member {
                    confirm: ConfirmBatch::for_raft(raft.clone()),
                    raft,
                })),
                leases: Arc::default(),
                vips: Arc::default(),
            },
            write_timeout: config.write_timeout,
            forwarder: None,
            apply_lag: Default::default(),
        })
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

    /// 이 노드의 Raft(구성원인 백엔드)
    fn member(&self) -> Raft<TypeConfig> {
        let Backend::Member { raft, .. } = self.node.backend();
        raft
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

    /// 새 클러스터를 만든다. 첫 노드에서 한 번만 부른다.
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), ClusterError> {
        self.member()
            .initialize(members)
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// 노드를 learner로 추가하고 로그를 따라잡을 때까지 기다린다. 리더에서 부른다.
    /// `write_timeout` 안에 끝나지 않으면 `Timeout`이다. 이때도 learner 추가는 나중에
    /// 커밋될 수 있고, 커밋되면 리더는 계속 따라잡게 한다.
    pub async fn add_learner(&self, id: NodeId, node: BasicNode) -> Result<(), ClusterError> {
        tokio::time::timeout(
            self.write_timeout,
            self.member().add_learner(id, node, true),
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
            self.member().change_membership(voters, true),
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
            let result = match leader::execute(&self.leader_ctx(), req.clone(), remaining).await {
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
                            let _ = self
                                .wait_revision(
                                    index,
                                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                                )
                                .await;
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
        leader::execute(&self.leader_ctx(), req, self.write_timeout).await
    }

    pub(crate) fn leader_ctx(&self) -> LeaderCtx {
        self.node.leader_ctx()
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
        self.node.vips.leases(&self.member())
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
        self.node.leases.leases(&self.member())
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
    pub fn members(&self) -> Members {
        let m = self.member().metrics().borrow().clone();
        let membership = m.membership_config.membership();
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        let mut out = Members {
            leader: m.current_leader,
            voters: BTreeMap::new(),
            learners: BTreeMap::new(),
        };
        for (id, node) in membership.nodes() {
            let list = if voters.contains(id) {
                &mut out.voters
            } else {
                &mut out.learners
            };
            list.insert(*id, node.addr.clone());
        }
        out
    }

    /// 헬스 체크가 보는 이 노드의 Raft 상태. 부를 때마다 적용 지연을 다시 잰다([`RaftHealth`]).
    pub fn health(&self) -> RaftHealth {
        let m = self.member().metrics().borrow().clone();
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
        match self.member().ensure_linearizable().await {
            Ok(_) => Ok(()),
            Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f))) => {
                Err(ClusterError::NotLeader {
                    leader_id: f.leader_id,
                    leader_addr: f.leader_node.map(|n| n.addr),
                })
            }
            Err(e) => Err(ClusterError::Raft(e.to_string())),
        }
    }

    /// `since` 이후에 커밋된 변경을 구독한다.
    pub fn watch(&self, since: u64) -> Result<Watch, WatchError> {
        self.store.watch_hub().subscribe(since)
    }

    pub async fn shutdown(&self) -> Result<(), ClusterError> {
        self.member()
            .shutdown()
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// Raft가 멈추면 그 이유와 함께 끝나는 future(저장소를 붙잡지 않는다). 저장소 쓰기가 실패하거나
    /// 저장소 스레드가 멈추면 openraft가 이 노드의 Raft를 멈춘다(`running_state`가 `Err`). `shutdown`으로
    /// 멈춰도 끝난다.
    pub fn stopped(&self) -> impl Future<Output = String> + Send + 'static {
        let mut rx = self.member().metrics();
        async move {
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
        }
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
            .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
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
            .initialize(BTreeMap::from([(id, BasicNode::new("node-1"))]))
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
        node.initialize(BTreeMap::from([(node.id(), BasicNode::new("node-1"))]))
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

    /// 피드는 멤버십 항목처럼 이벤트가 없는 항목도 적용 표시(와 멤버십)로 싣는다. learner의 적용 리비전도
    /// 이벤트 없이 리더를 따라온다.
    #[tokio::test]
    async fn feed_marks_membership_entries_without_events() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let leader = open_node(&router, root.path(), 1).await;
        leader
            .initialize(BTreeMap::from([(leader.id(), BasicNode::new("node-1"))]))
            .await
            .unwrap();
        leader
            .wait_revision(1, Duration::from_secs(10))
            .await
            .unwrap();
        let learner = open_node(&router, root.path(), 2).await;
        let mut feed = leader.store.watch_hub().feed(leader.revision()).unwrap();
        leader
            .add_learner(learner.id(), BasicNode::new("node-2"))
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
