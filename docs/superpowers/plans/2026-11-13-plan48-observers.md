# Orbivisor 계획 48: 규모 2 — 관찰 노드 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드가 수백 대여도 리더의 Raft 일이 투표 멤버와 대기 learner 수에만 비례하게 한다. 그 밖의 노드는 Raft 구성원이 아닌 관찰 노드로, 투표 멤버 하나를 따라 자기 복제본을 유지한다.

**Architecture:**
- **백엔드:** `Cluster`가 Raft 멤버(`Backend::Member`)와 관찰 노드(`Backend::Observer`) 가운데 하나로 돈다. 읽기·감시는 같은 redb 표와 `WatchHub`를 쓰므로 API·reconciler·HA는 그대로다.
- **Follow 피드:** 관찰 노드는 리더가 아닌 투표 멤버에 피어 RPC `follow`(서버 스트리밍)로 붙어 스냅샷(처음·뒤처졌을 때)과 변경 묶음(적용한 `LogId` 포함)을 받는다. 쓰기와 HA·VIP 리스 갱신은 리더로 전달하고 그 리비전까지 기다린다.
- **전환:** 리더의 `VoterKeeper`가 투표 멤버 + 대기 learner 2대 밖의 learner를 `ClusterStatus.observers`에 올리고 멤버십에서 뺀다. 각 노드의 `RoleKeeper`가 그에 맞춰 관찰 모드(META `node_mode`)로 바꾸거나 되돌린다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9.25, redb, tonic(JSON 코덱 서버 스트리밍), axum·utoipa, React(웹 역할 열). 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-11-availability-scale-design.md` (§6, §15 "구현(계획 48)")
**선행 계획:** 계획 1~47 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 백엔드 틈, `revision`·`wait_revision`, 적용 표시를 담는 피드 | 리스 갱신을 투표 멤버가 묶어 전달하기 |
| Follow 피드(스냅샷·변경·진행), 관찰 노드 복제본, 상류 바꾸기 | 바로 관찰 노드로 가입하기 |
| gRPC `follow`, `ClusterStatus.observers`, 역할 표시, `Orbivisor-Min-Revision` | 대기 learner 수 설정 |
| learner ↔ 관찰 노드 전환, `RoleKeeper`, `VoterKeeper` 확장, opld 모드 열기 | 클러스터 통신 다중 링크(계획 49) |
| 관찰 노드 규모 시험, 웹 역할 열, README, 설계 §15 | |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 변경과 시험(memnet으로 관찰 노드·전환, 규모 시험 30·100·300노드), opld `ha-it`, 웹 시험.
- **Linux CI:** 같은 시험과 규모 시험(노드 100).
- **테스트 VM:** 노드 둘이라 관찰 노드가 생기지 않는다(투표 멤버 상한 5). 배포 뒤 새 빌드의 정상 동작과 역할 표시만 본다.

## 설계 결정

- **관찰 노드의 리비전은 늘 리더 로그의 앞부분이다:** 피드의 모든 묶음이 적용한 `LogId`를 싣고, 관찰 노드는 자기보다 낮은 스냅샷·이미 적용한 이벤트를 버리며, 투표 멤버는 자기보다 앞선 관찰 노드를 받지 않는다. 그래서 관찰 노드 상태는 뒤로 가지 않는다.
- **펜싱은 리더를 거친다:** 관찰 노드의 HA·VIP 리스 갱신은 리더로 가고, 리더가 돌려준 인덱스까지 적용해야 성공이다. 리더가 옮긴 VM을 관찰 노드가 계속 돌리지 않는다. 그래서 리더 일에 노드당 초당 약 0.5 RPC가 남는다(예외로 문서에 적음).
- **투표 멤버는 관찰 노드가 되지 않는다:** 노드는 joint 두 구성 어디에도 투표 멤버가 아니고 그 멤버십이 적용된 뒤에만 Raft를 끄고, 끈 뒤 다시 확인한다.
- **기능 확인으로 롤링 업그레이드를 지킨다:** 모든 노드가 `observer`를 보고할 때만 내린다.


### Task 1: 백엔드 틈과 공통 적용 리비전

opl-cluster `Cluster`가 Raft를 `NodeHandle { backend: Arc<RwLock<Backend>>, .. }`(`Backend::Member { raft, confirm }`) 뒤에 두고 `raft()`는 `Option<Raft<TypeConfig>>`를 돌려준다(부르는 곳은 `None`을 처리: 피어 서버의 Raft RPC는 `failed_precondition`). `Cluster::revision()`·`wait_revision(rev, timeout)`이 저장소의 적용 리비전을 기다리고(`request_within`·`wait_local`·`renew_lease`·`renew_vip`이 `raft.wait()` 대신 씀), `WatchHub`는 이벤트 없는 Raft 항목(blank·membership)도 적용 표시로 담는 `FeedBatch { events, applied, membership }`를 쌓는다. memnet은 RPC마다 노드의 현재 backend를 찾는다. 동작은 바뀌지 않는다.

**Files:**
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/remotes.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/voters.rs`
- Modify: `crates/opl-cluster/src/watch.rs`
- Modify: `crates/opl-cluster/tests/common/mod.rs`
- Modify: `crates/opl-cluster/tests/health.rs`
- Modify: `crates/opl-cluster/tests/lease.rs`
- Modify: `crates/opl-cluster/tests/scale.rs`
- Modify: `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`
- Modify: `crates/opl-cluster/tests/voters.rs`
- Modify: `crates/opl-cluster/tests/witness.rs`
- Modify: `crates/opl-peer/src/monitor.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs`에서

```rust
            .raft()
            .wait(Some(Duration::from_secs(10)))
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
```

`crates/opl-api/src/remotes.rs`에서

```rust
            .raft()
            .wait(Some(Duration::from_secs(10)))
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            .raft()
            .wait(Some(WAIT))
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .wait(Some(WAIT))
```

`crates/opl-cluster/src/cluster.rs` 전체:

```rust
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
```

`crates/opl-cluster/src/memnet.rs`에서

```rust

use crate::cluster::{Cluster, ClusterError};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::types::{LeaderRequest, NodeId, TypeConfig};
```

을 다음으로 바꾼다:

```rust

use crate::cluster::{Cluster, ClusterError, NodeHandle};
use crate::leader::{self, Forwarder};
use crate::types::{LeaderRequest, NodeId, TypeConfig};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
struct Routes {
    nodes: HashMap<NodeId, LeaderCtx>,
    isolated: HashSet<NodeId>,
```

을 다음으로 바꾼다:

```rust
struct Routes {
    /// 노드의 백엔드. RPC를 보낼 때마다 지금 백엔드를 읽는다.
    nodes: HashMap<NodeId, NodeHandle>,
    isolated: HashSet<NodeId>,
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
            .nodes
            .insert(id, cluster.leader_ctx());
    }
```

을 다음으로 바꾼다:

```rust
            .nodes
            .insert(id, cluster.node_handle());
    }
```

`crates/opl-cluster/src/memnet.rs`에서

```rust

    fn route(&self, from: NodeId, to: NodeId) -> Result<LeaderCtx, Unreachable> {
        let routes = self.routes.lock().unwrap();
```

을 다음으로 바꾼다:

```rust

    fn route(&self, from: NodeId, to: NodeId) -> Result<NodeHandle, Unreachable> {
        let routes = self.routes.lock().unwrap();
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
    ) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let ctx = self.route(from, to)?;
        let routes = self.routes.lock().unwrap();
        if routes.raft_blocked.contains(&from) || routes.raft_blocked.contains(&to) {
            return Err(unreachable("raft blocked"));
        }
        Ok((ctx.raft, routes.replies_lost.contains(&from)))
    }
```

을 다음으로 바꾼다:

```rust
    ) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let node = self.route(from, to)?;
        let lost = {
            let routes = self.routes.lock().unwrap();
            if routes.raft_blocked.contains(&from) || routes.raft_blocked.contains(&to) {
                return Err(unreachable("raft blocked"));
            }
            routes.replies_lost.contains(&from)
        };
        let raft = node
            .raft()
            .ok_or_else(|| unreachable("not a Raft member"))?;
        Ok((raft, lost))
    }
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
    ) -> Result<u64, ClusterError> {
        let ctx = self
            .router
            .route(self.from, leader)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&ctx, req, FORWARD_TIMEOUT).await
    }
```

을 다음으로 바꾼다:

```rust
    ) -> Result<u64, ClusterError> {
        let node = self
            .router
            .route(self.from, leader)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&node.leader_ctx(), req, FORWARD_TIMEOUT).await
    }
```

`crates/opl-cluster/src/state_machine.rs` 전체:

```rust
use std::io::Cursor;
use std::sync::Arc;

use openraft::storage::RaftStateMachine;
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exclusive::exclusive_values;
use crate::store::{
    KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA, KEY_SNAPSHOT_META, META, NAMES,
    OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;

/// 스냅샷 본문의 머리. 본문 전체를 읽기 전에 형식 버전부터 확인한다.
#[derive(Deserialize)]
struct SnapshotHeader {
    schema_version: u32,
}

/// 스냅샷 본문. 상태 머신의 리소스 전체를 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
}

/// 리소스 상태 머신. 엔트리 묶음 하나를 redb 트랜잭션 하나로 적용하므로
/// 리소스와 `last_applied`는 항상 함께 기록된다. 쓰기는 저장소 스레드에서 돈다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>, writer: StoreWriter) -> Self {
        Self { db, watch, writer }
    }

    fn read_applied(&self) -> Result<(Option<LogId<NodeId>>, Membership), StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok((
            get_json(&meta, KEY_LAST_APPLIED)?,
            get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default(),
        ))
    }

    fn apply_entries(
        db: &Database,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Option<FeedBatch>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let mut applied = None;
        let mut membership = None;
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        let m = Membership::new(Some(entry.log_id), m);
                        put_json(&mut meta, KEY_LAST_MEMBERSHIP, &m)?;
                        membership = Some(m);
                        Ok(index)
                    }
                    EntryPayload::Normal(cmd) => {
                        apply_command(&mut objects, &mut names, cmd, index)?.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                };
                put_json(&mut meta, KEY_LAST_APPLIED, &entry.log_id)?;
                applied = Some(entry.log_id);
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        // 이벤트가 없는 묶음도 적용 위치를 알린다.
        let batch = applied.map(|applied| FeedBatch {
            events,
            applied,
            membership,
        });
        Ok((responses, batch))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷 본문을 만든다. 저장은 부른 쪽이 한다.
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let txn = db.begin_read()?;
        let meta_table = txn.open_table(META)?;
        let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
        let last_membership: Membership =
            get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
        let mut objects = Vec::new();
        for item in txn.open_table(OBJECTS)?.iter()? {
            let (key, value) = item?;
            let kind: Kind = serde_json::from_value(key.value().0.into())?;
            objects.push((kind, serde_json::from_slice(value.value())?));
        }
        drop(txn);

        let data = serde_json::to_vec(&SnapshotData {
            schema_version: SCHEMA_VERSION,
            objects,
        })?;
        let meta = Meta {
            snapshot_id: format!(
                "{}-{}",
                last_log_id.map_or(0, |l| l.index),
                Uuid::new_v4().simple()
            ),
            last_log_id,
            last_membership,
        };
        Ok((meta, data))
    }

    /// 스냅샷을 저장한다. 이미 저장된 스냅샷이 같거나 더 새로우면 그대로 둔다.
    /// openraft는 스냅샷 빌드를 별도 태스크에서 돌리므로, 느린 빌드가 끝나기 전에
    /// 더 새 스냅샷이 설치될 수 있다.
    fn save_snapshot(db: &Database, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
            if let Some(existing) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)?
                && existing.last_log_id >= meta.last_log_id
            {
                return Ok(());
            }
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn current_snapshot(&self) -> Result<Option<Snapshot<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        let Some(meta) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)? else {
            return Ok(None);
        };
        let data = table
            .get(KEY_SNAPSHOT_DATA)?
            .map(|v| v.value().to_vec())
            .unwrap_or_default();
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }

    /// 리소스 전체를 스냅샷 내용으로 바꾼다. 리소스, 적용 위치, 스냅샷을 한 트랜잭션에 기록한다.
    fn install(db: &Database, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let header: SnapshotHeader = serde_json::from_slice(&data)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: header.schema_version,
            });
        }
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        let txn = db.begin_write()?;
        {
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            objects.retain(|_, _| false)?;
            names.retain(|_, _| false)?;
            for (kind, resource) in &snapshot.objects {
                objects.insert(
                    (kind.as_str(), resource.id.as_u128()),
                    serde_json::to_vec(resource)?.as_slice(),
                )?;
                names.insert(
                    (kind.as_str(), resource.name.as_str()),
                    resource.id.as_u128(),
                )?;
            }
            let mut table = txn.open_table(META)?;
            put_json(&mut table, KEY_LAST_APPLIED, &meta.last_log_id)?;
            put_json(&mut table, KEY_LAST_MEMBERSHIP, &meta.last_membership)?;
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// 명령 하나를 적용한다. 거부는 결정적이므로 모든 노드가 같은 결과를 낸다.
/// 바깥 `Err`는 저장소 오류이고, 안쪽 `Err`는 명령 거부다.
fn apply_command(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
) -> Result<Result<WatchEvent, Rejection>, StoreError> {
    let load = |objects: &Table<(&'static str, u128), &'static [u8]>,
                kind: Kind,
                id: Uuid|
     -> Result<Option<RawResource>, StoreError> {
        match objects.get((kind.as_str(), id.as_u128()))? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    };
    let check = |expected: Option<u64>, actual: u64| match expected {
        Some(expected) if expected != actual => {
            Err(Rejection::RevisionMismatch { expected, actual })
        }
        _ => Ok(()),
    };

    let (kind, resource) = match cmd {
        Command::Create { kind, mut resource } => {
            if load(objects, kind, resource.id)?.is_some() {
                return Ok(Err(Rejection::AlreadyExists {
                    kind,
                    id: resource.id,
                }));
            }
            if names
                .get((kind.as_str(), resource.name.as_str()))?
                .is_some()
            {
                return Ok(Err(Rejection::NameTaken {
                    kind,
                    name: resource.name,
                }));
            }
            if let Err(r) = check_exclusive(objects, kind, resource.id, &resource.spec)? {
                return Ok(Err(r));
            }
            names.insert(
                (kind.as_str(), resource.name.as_str()),
                resource.id.as_u128(),
            )?;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateSpec {
            kind,
            id,
            expected_revision,
            labels,
            spec,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            if let Err(r) = check_exclusive(objects, kind, id, &spec)? {
                return Ok(Err(r));
            }
            resource.labels = labels;
            resource.spec = spec;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatus { kind, id, status } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatusIf {
            kind,
            id,
            expected_revision,
            status,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(Some(expected_revision), resource.revision) {
                return Ok(Err(r));
            }
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::Delete {
            kind,
            id,
            expected_revision,
        } => {
            let Some(resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            objects.remove((kind.as_str(), id.as_u128()))?;
            names.remove((kind.as_str(), resource.name.as_str()))?;
            return Ok(Ok(WatchEvent::Deleted {
                revision: index,
                kind,
                resource,
            }));
        }
    };
    objects.insert(
        (kind.as_str(), resource.id.as_u128()),
        serde_json::to_vec(&resource)?.as_slice(),
    )?;
    Ok(Ok(WatchEvent::Put { kind, resource }))
}

/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다.
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec);
    if wanted.is_empty() {
        return Ok(Ok(()));
    }
    let in_use = |field: &str, value: &str, holder: Uuid| Rejection::InUse {
        kind,
        field: field.to_string(),
        value: value.to_string(),
        holder,
    };
    for (i, (field, value)) in wanted.iter().enumerate() {
        if wanted[..i].contains(&(field, value.clone())) {
            return Ok(Err(in_use(field, value, id)));
        }
    }
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (key, v) = item?;
        let other = Uuid::from_u128(key.value().1);
        if other == id {
            continue;
        }
        let other_spec: RawResource = serde_json::from_slice(v.value())?;
        for (field, value) in exclusive_values(kind, &other_spec.spec) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    Ok(Ok(()))
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    /// 큰 읽기는 `spawn_blocking`에서, 저장은 저장소 스레드에서 한다.
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let db = self.db.clone();
        let (meta, data) = tokio::task::spawn_blocking(move || Self::snapshot_data(&db))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        let (meta, data) = self
            .writer
            .run(move |db| Self::save_snapshot(db, &meta, &data).map(|()| (meta, data)))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, Membership), StorageError<NodeId>> {
        self.read_applied()
            .map_err(|e| StorageIOError::read_state_machine(&e).into())
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let (responses, batch) = self
            .writer
            .run(move |db| Self::apply_entries(db, entries))
            .await
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        if let Some(batch) = batch {
            self.watch.publish(batch);
        }
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &Meta,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let (m, data) = (meta.clone(), snapshot.into_inner());
        self.writer
            .run(move |db| Self::install(db, &m, data))
            .await
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;
        self.watch.reset(meta.last_log_id.map_or(0, |l| l.index));
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        self.current_snapshot()
            .map_err(|e| StorageIOError::read_snapshot(None, &e).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use openraft::CommittedLeaderId;

    fn meta_at(index: u64) -> Meta {
        Meta {
            last_log_id: Some(LogId::new(
                CommittedLeaderId::new(1, NodeId::from(1)),
                index,
            )),
            last_membership: Membership::default(),
            snapshot_id: format!("{index}"),
        }
    }

    /// openraft는 스냅샷을 다른 태스크에서 만들므로, 느린 빌드가 그 사이 설치된
    /// 더 새 스냅샷을 덮어쓰면 안 된다.
    #[test]
    fn older_snapshot_does_not_replace_newer_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        StateMachine::save_snapshot(&sm.db, &meta_at(10), b"new").unwrap();
        StateMachine::save_snapshot(&sm.db, &meta_at(5), b"old").unwrap();

        let snap = sm.current_snapshot().unwrap().unwrap();
        assert_eq!(snap.meta, meta_at(10));
        assert_eq!(snap.snapshot.into_inner(), b"new");
    }

    #[test]
    fn snapshot_with_newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        // 미래 형식은 본문 구조도 다를 수 있다. 헤더만 보고 거부해야 한다.
        let data = br#"{"schema_version": 99, "objects": {"format": "v99"}}"#.to_vec();
        assert!(matches!(
            StateMachine::install(&sm.db, &meta_at(3), data),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
```

`crates/opl-cluster/src/voters.rs`에서

```rust
async fn tick(cluster: &Cluster, config: &VoterKeeperConfig, state: &mut State) {
    let m = cluster.raft().metrics().borrow().clone();
    if m.state != ServerState::Leader {
```

을 다음으로 바꾼다:

```rust
async fn tick(cluster: &Cluster, config: &VoterKeeperConfig, state: &mut State) {
    // Raft 구성원이 아니면 리더일 수 없다.
    let Some(raft) = cluster.raft() else {
        *state = State::default();
        return;
    };
    let m = raft.metrics().borrow().clone();
    if m.state != ServerState::Leader {
```

`crates/opl-cluster/src/voters.rs`에서

```rust
    state.unfinished = true;
    match change_members(cluster.raft(), changes, config.change_timeout).await {
        Outcome::Done => {
```

을 다음으로 바꾼다:

```rust
    state.unfinished = true;
    match change_members(&raft, changes, config.change_timeout).await {
        Outcome::Done => {
```

`crates/opl-cluster/src/voters.rs`에서

```rust
    let finish = ChangeMembers::AddVoterIds(BTreeSet::new());
    match change_members(cluster.raft(), finish, config.change_timeout).await {
        Outcome::Done => {
```

을 다음으로 바꾼다:

```rust
    let finish = ChangeMembers::AddVoterIds(BTreeSet::new());
    let Some(raft) = cluster.raft() else {
        return;
    };
    match change_members(&raft, finish, config.change_timeout).await {
        Outcome::Done => {
```

`crates/opl-cluster/src/voters.rs`에서

```rust
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, BasicNode>) -> bool {
    let now = cluster.raft().metrics().borrow().clone();
    now.state == ServerState::Leader
```

을 다음으로 바꾼다:

```rust
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, BasicNode>) -> bool {
    let Some(raft) = cluster.raft() else {
        return false;
    };
    let now = raft.metrics().borrow().clone();
    now.state == ServerState::Leader
```

`crates/opl-cluster/src/watch.rs` 전체:

```rust
//! 커밋된 리소스 변경을 구독한다. 최근 적용 묶음은 메모리에 보관해 `since` 이후부터 이어 받을 수 있다.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use openraft::{BasicNode, LogId, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};

use crate::types::{NodeId, RawResource};

/// 메모리에 보관하는 최근 적용 묶음 수. 이보다 오래된 `since`는 `Expired`가 된다.
pub const HISTORY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WatchEvent {
    /// 리소스가 생성되거나 바뀌었다. 이 이벤트의 revision은 `resource.revision`이다.
    Put { kind: Kind, resource: RawResource },
    /// 리소스가 삭제되었다. 삭제 직전의 리소스를 담는다.
    Deleted {
        revision: u64,
        kind: Kind,
        resource: RawResource,
    },
    /// 스냅샷 설치로 상태가 통째로 바뀌었다. 구독자는 목록을 다시 읽어야 한다.
    Reset { revision: u64 },
}

impl WatchEvent {
    pub fn revision(&self) -> u64 {
        match self {
            WatchEvent::Put { resource, .. } => resource.revision,
            WatchEvent::Deleted { revision, .. } | WatchEvent::Reset { revision } => *revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// 요청한 revision 이후의 변경 일부를 더는 보관하지 않는다. 목록을 다시 읽고 새로 구독한다.
    #[error("revision {since} is too old (history starts after {earliest})")]
    Expired { since: u64, earliest: u64 },
    /// 구독자가 너무 느려 변경을 놓쳤다. 목록을 다시 읽고 새로 구독한다.
    #[error("watcher lagged behind")]
    Lagged,
    #[error("cluster store closed")]
    Closed,
}

/// 상태 머신이 한 번에 적용한 항목 묶음. 이벤트가 없는 항목(blank, 멤버십, 거부된 명령)도 `applied`로
/// 적용 위치를 알린다(리비전에 틈이 있어 이벤트만으로는 어디까지 적용했는지 모른다).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedBatch {
    /// 묶음의 리소스 변경(revision 순서)
    pub events: Vec<WatchEvent>,
    /// 묶음의 마지막 항목
    pub applied: LogId<NodeId>,
    /// 묶음에 든 마지막 멤버십 변경
    pub membership: Option<StoredMembership<NodeId, BasicNode>>,
}

/// 피드 구독이 받는 것
#[derive(Debug, Clone)]
pub(crate) enum FeedItem {
    Batch(Arc<FeedBatch>),
    /// 스냅샷 설치로 상태가 통째로 바뀌었다.
    Reset {
        revision: u64,
    },
}

impl FeedItem {
    fn revision(&self) -> u64 {
        match self {
            FeedItem::Batch(b) => b.applied.index,
            FeedItem::Reset { revision } => *revision,
        }
    }
}

struct History {
    batches: VecDeque<Arc<FeedBatch>>,
    /// 이 revision보다 뒤의 묶음은 모두 `batches`에 있다.
    earliest: u64,
}

pub(crate) struct WatchHub {
    history: Mutex<History>,
    tx: broadcast::Sender<FeedItem>,
    /// 저장소에 적용한 마지막 인덱스(revision). 묶음 적용과 스냅샷 설치가 올린다.
    applied: watch::Sender<u64>,
}

impl WatchHub {
    /// `applied`: 저장소를 연 시점의 마지막 적용 인덱스. 그 이전 변경은 보관하지 않는다.
    pub(crate) fn new(applied: u64) -> Self {
        Self {
            history: Mutex::new(History {
                batches: VecDeque::new(),
                earliest: applied,
            }),
            tx: broadcast::channel(HISTORY).0,
            applied: watch::channel(applied).0,
        }
    }

    /// 커밋된 묶음을 알린다. 상태 머신이 redb 커밋 뒤에 부른다.
    pub(crate) fn publish(&self, batch: FeedBatch) {
        let batch = Arc::new(batch);
        let mut h = self.history.lock().unwrap();
        if h.batches.len() == HISTORY {
            let old = h.batches.pop_front().expect("history is full");
            h.earliest = old.applied.index;
        }
        h.batches.push_back(batch.clone());
        let _ = self.tx.send(FeedItem::Batch(batch.clone()));
        self.applied.send_replace(batch.applied.index);
    }

    /// 스냅샷 설치 뒤에 부른다. 보관한 변경을 버리고 `Reset`을 알린다.
    pub(crate) fn reset(&self, revision: u64) {
        let mut h = self.history.lock().unwrap();
        h.batches.clear();
        h.earliest = revision;
        let _ = self.tx.send(FeedItem::Reset { revision });
        self.applied.send_replace(revision);
    }

    /// 저장소에 적용한 마지막 인덱스
    pub(crate) fn revision(&self) -> u64 {
        *self.applied.borrow()
    }

    /// 적용 인덱스가 바뀔 때마다 깨어나는 수신자
    pub(crate) fn applied(&self) -> watch::Receiver<u64> {
        self.applied.subscribe()
    }

    /// `since` 뒤의 적용 묶음을 구독한다.
    pub(crate) fn feed(&self, since: u64) -> Result<Feed, WatchError> {
        let h = self.history.lock().unwrap();
        if since < h.earliest {
            return Err(WatchError::Expired {
                since,
                earliest: h.earliest,
            });
        }
        let backlog = h
            .batches
            .iter()
            .filter(|b| b.applied.index > since)
            .map(|b| FeedItem::Batch(b.clone()))
            .collect();
        // 잠금을 쥔 채 구독해야 backlog와 이후 묶음 사이에 빈틈이 없다.
        Ok(Feed {
            backlog,
            rx: self.tx.subscribe(),
            seen: since,
        })
    }

    pub(crate) fn subscribe(&self, since: u64) -> Result<Watch, WatchError> {
        Ok(Watch {
            feed: self.feed(since)?,
            pending: VecDeque::new(),
            since,
        })
    }
}

/// 적용 묶음 구독. `next()`는 적용 순서대로, `since` 뒤의 묶음(과 `Reset`)만 돌려준다.
pub(crate) struct Feed {
    backlog: VecDeque<FeedItem>,
    rx: broadcast::Receiver<FeedItem>,
    seen: u64,
}

impl Feed {
    pub(crate) async fn next(&mut self) -> Result<FeedItem, WatchError> {
        if let Some(item) = self.backlog.pop_front() {
            self.seen = item.revision();
            return Ok(item);
        }
        loop {
            match self.rx.recv().await {
                Ok(item @ FeedItem::Reset { .. }) => {
                    self.seen = item.revision();
                    return Ok(item);
                }
                // 목록을 읽은 직후 구독하면 그 목록에 이미 반영된 묶음이 올 수 있다.
                Ok(item) if item.revision() <= self.seen => continue,
                Ok(item) => {
                    self.seen = item.revision();
                    return Ok(item);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(WatchError::Lagged),
                Err(broadcast::error::RecvError::Closed) => return Err(WatchError::Closed),
            }
        }
    }
}

/// 변경 구독. `next()`는 revision 순서대로, `since` 이후의 변경만 돌려준다. 적용 묶음을 이벤트로 펼친다.
pub struct Watch {
    feed: Feed,
    pending: VecDeque<WatchEvent>,
    since: u64,
}

impl Watch {
    pub async fn next(&mut self) -> Result<WatchEvent, WatchError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            match self.feed.next().await? {
                FeedItem::Reset { revision } => return Ok(WatchEvent::Reset { revision }),
                // `since`가 묶음 가운데면 그 앞의 변경은 건너뛴다.
                FeedItem::Batch(batch) => self.pending.extend(
                    batch
                        .events
                        .iter()
                        .filter(|e| e.revision() > self.since)
                        .cloned(),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    fn log_id(index: u64) -> LogId<NodeId> {
        LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `applied`까지 적용한 묶음
    fn batch(applied: u64, events: Vec<WatchEvent>) -> FeedBatch {
        FeedBatch {
            events,
            applied: log_id(applied),
            membership: None,
        }
    }

    fn put(revision: u64) -> WatchEvent {
        let mut resource: RawResource = Resource::new("a", json!({}));
        resource.revision = revision;
        WatchEvent::Put {
            kind: Kind::Vm,
            resource,
        }
    }

    #[tokio::test]
    async fn replays_history_after_since_then_follows_live() {
        let hub = WatchHub::new(0);
        hub.publish(batch(3, vec![put(1), put(2), put(3)]));
        let mut w = hub.subscribe(1).unwrap();
        assert_eq!(w.next().await.unwrap().revision(), 2);
        assert_eq!(w.next().await.unwrap().revision(), 3);
        hub.publish(batch(4, vec![put(4)]));
        assert_eq!(w.next().await.unwrap().revision(), 4);
    }

    #[tokio::test]
    async fn skips_live_events_at_or_before_since() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(5).unwrap();
        hub.publish(batch(6, vec![put(5), put(6)]));
        assert_eq!(w.next().await.unwrap().revision(), 6);
    }

    #[test]
    fn since_before_history_is_expired() {
        let hub = WatchHub::new(10);
        assert_eq!(
            hub.subscribe(9).err(),
            Some(WatchError::Expired {
                since: 9,
                earliest: 10
            })
        );
        assert!(hub.subscribe(10).is_ok());
    }

    #[test]
    fn evicting_old_events_moves_earliest() {
        let hub = WatchHub::new(0);
        for i in 1..=HISTORY as u64 + 1 {
            hub.publish(batch(i, vec![put(i)]));
        }
        assert!(matches!(
            hub.subscribe(0),
            Err(WatchError::Expired { earliest: 1, .. })
        ));
        assert!(hub.subscribe(1).is_ok());
    }

    #[tokio::test]
    async fn reset_clears_history_and_is_always_delivered() {
        let hub = WatchHub::new(0);
        hub.publish(batch(2, vec![put(1), put(2)]));
        let mut w = hub.subscribe(2).unwrap();
        hub.reset(2);
        assert_eq!(w.next().await.unwrap(), WatchEvent::Reset { revision: 2 });
        assert!(matches!(hub.subscribe(1), Err(WatchError::Expired { .. })));
    }

    #[tokio::test]
    async fn slow_watcher_gets_lagged() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(0).unwrap();
        for i in 1..=HISTORY as u64 + 1 {
            hub.publish(batch(i, vec![put(i)]));
        }
        assert_eq!(w.next().await, Err(WatchError::Lagged));
    }

    /// 이벤트가 없는 묶음(blank·멤버십)도 피드에 적용 위치로 온다. 변경 구독은 그것을 건너뛴다.
    #[tokio::test]
    async fn feed_carries_batches_without_events_and_watch_skips_them() {
        let hub = WatchHub::new(0);
        let mut feed = hub.feed(0).unwrap();
        let mut w = hub.subscribe(0).unwrap();
        hub.publish(batch(1, vec![]));
        hub.publish(batch(3, vec![put(2)]));
        let FeedItem::Batch(first) = feed.next().await.unwrap() else {
            panic!("expected a batch");
        };
        assert_eq!((first.applied.index, first.events.len()), (1, 0));
        let FeedItem::Batch(second) = feed.next().await.unwrap() else {
            panic!("expected a batch");
        };
        assert_eq!(second.applied.index, 3);
        assert_eq!(w.next().await.unwrap().revision(), 2);
        // 이벤트 없는 묶음 뒤에서 이어 구독해도 그 뒤 묶음만 온다.
        let mut late = hub.feed(1).unwrap();
        assert_eq!(late.next().await.unwrap().revision(), 3);
    }

    #[test]
    fn publish_and_reset_move_the_applied_revision() {
        let hub = WatchHub::new(4);
        let rx = hub.applied();
        assert_eq!(hub.revision(), 4);
        hub.publish(batch(5, vec![]));
        assert_eq!((hub.revision(), *rx.borrow()), (5, 5));
        hub.reset(9);
        assert_eq!((hub.revision(), *rx.borrow()), (9, 9));
    }
}
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
            .raft()
            .metrics()
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .metrics()
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
        for id in ids {
            if self.nodes[id].raft().metrics().borrow().current_leader != Some(leader) {
                return None;
            }
        }
        let m = self.nodes[&leader].raft().metrics().borrow().clone();
        let applied = m.last_applied.map(|l| l.index);
```

을 다음으로 바꾼다:

```rust
        for id in ids {
            if self.nodes[id]
                .raft()
                .unwrap()
                .metrics()
                .borrow()
                .current_leader
                != Some(leader)
            {
                return None;
            }
        }
        let m = self.nodes[&leader]
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let applied = m.last_applied.map(|l| l.index);
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust
            .raft()
            .wait(Some(WAIT))
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .wait(Some(WAIT))
```

`crates/opl-cluster/tests/health.rs`에서

```rust
    assert_eq!(
        tc.get(leader).raft().metrics().borrow().state,
        ServerState::Leader
```

을 다음으로 바꾼다:

```rust
    assert_eq!(
        tc.get(leader).raft().unwrap().metrics().borrow().state,
        ServerState::Leader
```

`crates/opl-cluster/tests/lease.rs`에서

```rust
fn term(tc: &TestCluster, id: NodeId) -> u64 {
    tc.get(id).raft().metrics().borrow().current_term
}
```

을 다음으로 바꾼다:

```rust
fn term(tc: &TestCluster, id: NodeId) -> u64 {
    tc.get(id).raft().unwrap().metrics().borrow().current_term
}
```

`crates/opl-cluster/tests/lease.rs`에서

```rust
    assert_eq!(
        tc.get(leader).raft().metrics().borrow().state,
        ServerState::Leader
```

을 다음으로 바꾼다:

```rust
    assert_eq!(
        tc.get(leader).raft().unwrap().metrics().borrow().state,
        ServerState::Leader
```

`crates/opl-cluster/tests/lease.rs`에서

```rust
    // 선거에 나설 수 있는 노드를 하나씩만 두어 리더를 a → b → a로 정해서 바꾼다.
    tc.get(c).raft().runtime_config().elect(false);
    tc.router.isolate(a);
    assert_eq!(tc.leader_among(&[b, c]).await, b);
    tc.get(b).raft().runtime_config().elect(false);
    tc.router.heal(a);
    // a가 b의 로그를 받아야 c가 a에게 투표한다.
    assert_eq!(tc.leader().await, b);
    let last = tc.get(b).raft().metrics().borrow().last_log_index.unwrap();
    tc.wait_applied(a, last).await;
```

을 다음으로 바꾼다:

```rust
    // 선거에 나설 수 있는 노드를 하나씩만 두어 리더를 a → b → a로 정해서 바꾼다.
    tc.get(c).raft().unwrap().runtime_config().elect(false);
    tc.router.isolate(a);
    assert_eq!(tc.leader_among(&[b, c]).await, b);
    tc.get(b).raft().unwrap().runtime_config().elect(false);
    tc.router.heal(a);
    // a가 b의 로그를 받아야 c가 a에게 투표한다.
    assert_eq!(tc.leader().await, b);
    let last = tc
        .get(b)
        .raft()
        .unwrap()
        .metrics()
        .borrow()
        .last_log_index
        .unwrap();
    tc.wait_applied(a, last).await;
```

`crates/opl-cluster/tests/scale.rs`에서

```rust
        ids.iter().copied().find(|id| {
            let m = self.get(*id).raft().metrics().borrow().clone();
            let applied = m.last_applied.map(|l| l.index);
```

을 다음으로 바꾼다:

```rust
        ids.iter().copied().find(|id| {
            let m = self.get(*id).raft().unwrap().metrics().borrow().clone();
            let applied = m.last_applied.map(|l| l.index);
```

`crates/opl-cluster/tests/scale.rs`에서

```rust
            .iter()
            .any(|id| h.get(*id).raft().metrics().borrow().state == ServerState::Leader);
        if leading {
```

을 다음으로 바꾼다:

```rust
            .iter()
            .any(|id| h.get(*id).raft().unwrap().metrics().borrow().state == ServerState::Leader);
        if leading {
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    c.raft()
        .wait(Some(WAIT))
```

을 다음으로 바꾼다:

```rust
    c.raft()
        .unwrap()
        .wait(Some(WAIT))
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust

    let last_log_index = |id| tc.get(id).raft().metrics().borrow().last_log_index;
    let before = last_log_index(old);
```

을 다음으로 바꾼다:

```rust

    let last_log_index = |id| tc.get(id).raft().unwrap().metrics().borrow().last_log_index;
    let before = last_log_index(old);
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
        .raft()
        .wait(Some(common::WAIT))
```

을 다음으로 바꾼다:

```rust
        .raft()
        .unwrap()
        .wait(Some(common::WAIT))
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
    // 지워진 로그는 스냅샷으로만 받을 수 있다.
    let snapshot = tc.get(n4).raft().metrics().borrow().snapshot;
    assert!(
```

을 다음으로 바꾼다:

```rust
    // 지워진 로그는 스냅샷으로만 받을 수 있다.
    let snapshot = tc.get(n4).raft().unwrap().metrics().borrow().snapshot;
    assert!(
```

`crates/opl-cluster/tests/voters.rs` 전체:

```rust
//! 투표 멤버 상한(`VoterKeeper`): 노드 일곱·상한 다섯에서 zone을 나눠 내리고, 죽은 투표 멤버를 learner로
//! 바꾸고, 과반이 없으면 바꾸지 않는다. 옛 노드(기능 없음)가 섞이면 움직이지 않는다. 끊긴 두 단계 변경을
//! 마무리하고(API로 지운 노드는 learner로 남기지 않는다), 바꿀 learner가 없으면 오프라인 투표 멤버를 그대로
//! 두고, 새 리더는 살아 있는 투표 멤버를 내리지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use openraft::{BasicNode, ChangeMembers};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{
    Cluster, ClusterConfig, Members, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 리스 갱신 간격
const RENEW: Duration = Duration::from_millis(200);

fn keeper_config() -> VoterKeeperConfig {
    VoterKeeperConfig {
        interval: Duration::from_millis(100),
        healthy_within: Duration::from_secs(1),
        alive_within: Duration::from_millis(600),
        offline_after: Duration::from_secs(2),
        change_timeout: Duration::from_secs(5),
    }
}

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    keeper: VoterKeeper,
}

struct Harness {
    router: MemRouter,
    nodes: BTreeMap<NodeId, Running>,
    _root: TempDir,
}

impl Harness {
    /// 노드 `1..=n`을 모두 투표 멤버로 띄우고 리더를 기다린다.
    async fn start(n: u64) -> Self {
        Self::start_with(n, keeper_config()).await
    }

    async fn start_with(n: u64, keeper: VoterKeeperConfig) -> Self {
        let root = TempDir::new().unwrap();
        let router = MemRouter::default();
        let mut nodes = BTreeMap::new();
        for i in 1..=n {
            let id = NodeId::from(i);
            let mut config = ClusterConfig::new(id, root.path().join(i.to_string()));
            config.raft = openraft::Config {
                heartbeat_interval: 50,
                election_timeout_min: 1000,
                election_timeout_max: 2000,
                ..Default::default()
            };
            config.write_timeout = Duration::from_secs(10);
            let cluster = Cluster::open(config, router.network(id))
                .await
                .unwrap()
                .with_forwarder(router.forwarder(id));
            router.register(id, &cluster);
            let cluster = Arc::new(cluster);
            let renew = {
                let cluster = cluster.clone();
                tokio::spawn(async move {
                    loop {
                        let _ = cluster.renew_lease(RENEW * 2).await;
                        tokio::time::sleep(RENEW).await;
                    }
                })
            };
            let keeper = VoterKeeper::spawn(cluster.clone(), keeper.clone());
            nodes.insert(
                id,
                Running {
                    cluster,
                    renew,
                    keeper,
                },
            );
        }
        let members = (1..=n)
            .map(|i| {
                (
                    NodeId::from(i),
                    openraft::BasicNode::new(format!("node-{i}")),
                )
            })
            .collect();
        nodes[&NodeId::from(1)]
            .cluster
            .initialize(members)
            .await
            .unwrap();
        let h = Harness {
            router,
            nodes,
            _root: root,
        };
        eventually("a leader", || async { h.leader().is_some() }).await;
        h
    }

    fn any(&self) -> &Cluster {
        &self.nodes.values().next().unwrap().cluster
    }

    /// 모든 노드가 같은 리더를 알면 그 리더(멈춘 노드를 아직 리더로 알면 None)
    fn leader(&self) -> Option<NodeId> {
        let leader = self.any().members().leader?;
        (self.nodes.contains_key(&leader)
            && self
                .nodes
                .values()
                .all(|n| n.cluster.members().leader == Some(leader)))
        .then_some(leader)
    }

    /// 리더가 아는 투표 멤버(두 단계 변경 중이면 None)
    fn voters(&self) -> Option<BTreeSet<NodeId>> {
        let leader = self.leader()?;
        let m = self.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        (membership.get_joint_config().len() == 1).then(|| membership.voter_ids().collect())
    }

    /// 리더가 아는 멤버십(다른 노드는 아직 적용하지 않았을 수 있다)
    fn leader_members(&self) -> Option<Members> {
        Some(self.nodes[&self.leader()?].cluster.members())
    }

    /// 클러스터 설정 `voters`
    async fn set_voters(&self, n: u8) {
        let spec = SettingsSpec {
            voters: Some(n),
            ..SettingsSpec::default()
        };
        match self
            .any()
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
        {
            None => {
                self.any()
                    .create(&Resource::new(SETTINGS_NAME, spec))
                    .await
                    .unwrap();
            }
            Some(s) => {
                self.any()
                    .update_spec(s.id, None, s.labels, &spec)
                    .await
                    .unwrap();
            }
        }
    }

    /// 노드 `i`의 Node 리소스(zone 레이블, 기능)
    async fn register(&self, i: u64, zone: &str, feature: bool) {
        let mut node = Resource::new(
            format!("n{i}"),
            NodeSpec {
                address: format!("node-{i}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = NodeId::from(i).0;
        node.labels.insert(ZONE_LABEL.into(), zone.into());
        node.status = status(feature);
        self.any().create(&node).await.unwrap();
    }

    /// 노드를 멈춘다(프로세스 종료에 해당).
    async fn stop(&mut self, id: NodeId) {
        let node = self.nodes.remove(&id).unwrap();
        node.renew.abort();
        node.keeper.shutdown().await;
        self.router.unregister(id);
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.stop(id).await;
        }
    }
}

fn status(feature: bool) -> NodeStatus {
    NodeStatus {
        online: true,
        features: if feature {
            vec![VOTER_LIMIT.into()]
        } else {
            vec![]
        },
        ..NodeStatus::default()
    }
}

/// `cond`가 참이 될 때까지 20초까지 기다린다.
async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn zones(voters: &BTreeSet<NodeId>, zone: &BTreeMap<NodeId, &str>) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for v in voters {
        *out.entry(zone[v].to_string()).or_default() += 1;
    }
    out
}

/// 노드 일곱(zone a 셋, b 둘, c 둘)이 모두 투표 멤버로 시작한다. 옛 노드가 섞여 있으면 그대로 두고, 모두
/// 기능을 알면 zone이 고르게 다섯으로 줄인다. 투표 멤버 하나를 죽이면 learner로 내리고 다른 노드를 올린다.
/// 상한을 3으로 바꾸면 zone마다 하나씩 남는다.
#[tokio::test]
async fn seven_nodes_keep_five_voters_spread_over_zones() {
    let mut h = Harness::start(7).await;
    let zone: BTreeMap<NodeId, &str> = [
        (1, "a"),
        (2, "a"),
        (3, "a"),
        (4, "b"),
        (5, "b"),
        (6, "c"),
        (7, "c"),
    ]
    .into_iter()
    .map(|(i, z)| (NodeId::from(i), z))
    .collect();
    // 노드 7은 옛 노드다(기능을 모른다).
    for i in 1..=7 {
        h.register(i, zone[&NodeId::from(i)], i != 7).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.voters().map(|v| v.len()), Some(7));

    // 노드 7도 업그레이드했다.
    h.any()
        .update_status::<NodeSpec>(NodeId::from(7).0, &status(true))
        .await
        .unwrap();
    eventually("five voters", || async {
        h.voters().is_some_and(|v| v.len() == 5)
    })
    .await;
    let voters = h.voters().unwrap();
    let spread = zones(&voters, &zone);
    assert_eq!(spread.len(), 3, "{spread:?}");
    assert!(spread.values().all(|n| *n <= 2), "{spread:?}");
    // 내린 노드는 learner로 남는다(다른 노드는 아직 적용하지 않았을 수 있어 리더의 멤버십으로 본다).
    assert_eq!(h.leader_members().unwrap().learners.len(), 2);

    // 리더가 아닌 투표 멤버 하나를 죽이면 오프라인이 된 뒤 learner로 내리고, 다른 노드가 올라온다.
    let leader = h.leader().unwrap();
    let dead = *voters.iter().find(|v| **v != leader).unwrap();
    h.stop(dead).await;
    eventually("the dead voter replaced", || async {
        h.voters()
            .is_some_and(|v| v.len() == 5 && !v.contains(&dead))
    })
    .await;
    let members = h.leader_members().unwrap();
    assert!(members.learners.contains_key(&dead), "{members:?}");
    let spread = zones(&h.voters().unwrap(), &zone);
    assert_eq!(spread.len(), 3, "{spread:?}");

    // 상한을 셋으로 바꾸면 zone마다 하나가 남는다.
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    let spread = zones(&h.voters().unwrap(), &zone);
    assert_eq!(spread.values().copied().collect::<Vec<_>>(), [1, 1, 1]);
    h.close().await;
}

/// 투표 멤버 다섯 가운데 셋이 죽어 과반이 없으면 오프라인이 되어도 멤버십을 바꾸지 않는다(바꿀 수도 없다).
#[tokio::test]
async fn nothing_changes_without_a_majority() {
    let mut h = Harness::start(5).await;
    for i in 1..=5 {
        h.register(i, "a", true).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let leader = h.leader().unwrap();
    let before = h.voters().unwrap();
    assert_eq!(before.len(), 5);
    let doomed: Vec<NodeId> = before
        .iter()
        .copied()
        .filter(|v| *v != leader)
        .take(3)
        .collect();
    for id in doomed {
        h.stop(id).await;
    }
    // 오프라인(2초)이 지나도 리더가 아는 멤버십은 그대로다(두 단계 변경도 시작하지 않는다).
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let m = h.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        assert_eq!(membership.get_joint_config().len(), 1);
        assert_eq!(membership.voter_ids().collect::<BTreeSet<_>>(), before);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.close().await;
}

/// 노드가 상한 이하면 오프라인 투표 멤버도 내리지 않는다(바꿀 learner가 없다): 노드 셋에서 하나가 오래 꺼져도
/// 셋 모두 투표 멤버로 남는다(내리면 그 뒤 하나만 더 죽어도 정족수가 깨진다).
#[tokio::test]
async fn an_offline_voter_without_a_replacement_stays_a_voter() {
    let mut h = Harness::start(3).await;
    for i in 1..=3 {
        h.register(i, "a", true).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let leader = h.leader().unwrap();
    let before = h.voters().unwrap();
    assert_eq!(before.len(), 3);
    let dead = *before.iter().find(|v| **v != leader).unwrap();
    h.stop(dead).await;
    // 오프라인(2초)이 지나도 그대로다.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        let m = h.nodes[&leader]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        assert_eq!(membership.get_joint_config().len(), 1);
        assert_eq!(membership.voter_ids().collect::<BTreeSet<_>>(), before);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.close().await;
}

/// 두 단계 변경의 첫 단계(joint)를 팔로워가 받은 뒤 리더가 죽으면 새 리더는 joint를 커밋하지만 두 번째 단계는
/// 오지 않는다. 새 리더의 키퍼가 그것을 uniform으로 마무리한다(그 뒤 상한을 넘는 죽은 옛 리더를 내린다).
#[tokio::test]
async fn a_new_leader_finishes_a_joint_change_left_by_the_old_one() {
    let mut h = Harness::start(4).await;
    for i in 1..=4 {
        h.register(i, "a", true).await;
    }
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    let leader = h.leader().unwrap();
    let voters = h.voters().unwrap();
    let learner = *h.nodes.keys().find(|id| !voters.contains(id)).unwrap();
    let followers: Vec<NodeId> = voters.iter().copied().filter(|v| *v != leader).collect();
    // 앞선 변경(내리기)이 모든 노드에 적용되기를 기다린다. 아니면 리더가 그 커밋을 모른 채 다음 변경을 거절한다.
    let config_index = |id: &NodeId| {
        let m = h.nodes[id]
            .cluster
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .clone();
        let index = m.membership_config.log_id().map(|l| l.index);
        (index, m.last_applied.map(|l| l.index) >= index)
    };
    let before = config_index(&leader).0;
    eventually("the last change applied everywhere", || async {
        h.nodes.keys().all(|id| config_index(id) == (before, true))
    })
    .await;
    // 리더가 보낸 항목은 닿지만 답이 사라져 리더는 joint의 커밋을 모르고 두 번째 단계를 내지 않는다.
    h.router.lose_replies(leader);
    let raft = h.nodes[&leader].cluster.raft().unwrap().clone();
    let change = tokio::spawn(async move {
        let add = ChangeMembers::AddVoterIds(BTreeSet::from([learner]));
        let _ = raft.change_membership(add, true).await;
    });
    eventually("the followers have the joint config", || async {
        followers.iter().all(|f| {
            let m = h.nodes[f]
                .cluster
                .raft()
                .unwrap()
                .metrics()
                .borrow()
                .clone();
            m.membership_config.log_id().map(|l| l.index) > before
                && m.membership_config.membership().get_joint_config().len() == 2
        })
    })
    .await;
    change.abort();
    h.stop(leader).await;
    // 마무리한 구성은 joint의 새 구성(옛 리더도 든다)이다. 새 임기의 처음에는 내리지 않으므로 잠시 보인다.
    let finished: BTreeSet<NodeId> = voters.iter().copied().chain([learner]).collect();
    eventually("the joint config finished", || async {
        h.voters().is_some_and(|v| v == finished)
    })
    .await;
    eventually("a uniform config without the old leader", || async {
        h.voters()
            .is_some_and(|v| v.len() == 3 && !v.contains(&leader) && v.contains(&learner))
    })
    .await;
    h.close().await;
}

/// 노드를 지우는 변경(`LeaderRequest::Remove`, learner로 남기지 않음)의 두 번째 단계가 늦어 키퍼가 joint를 먼저
/// 마무리하면(learner로 남긴다) 늦은 두 번째 단계는 할 일이 없다. 지우는 요청은 그 뒤 멤버십을 다시 읽어 learner로
/// 남은 노드를 지운다.
#[tokio::test]
async fn a_removed_node_does_not_stay_a_learner_when_the_keeper_finishes_first() {
    let config = VoterKeeperConfig {
        change_timeout: Duration::from_secs(1),
        ..keeper_config()
    };
    let mut h = Harness::start_with(4, config).await;
    let leader = h.leader().unwrap();
    let gone = *h.nodes.keys().find(|id| **id != leader).unwrap();
    let rest: BTreeSet<NodeId> = h.nodes.keys().copied().filter(|id| *id != gone).collect();
    h.stop(gone).await;
    // 지우는 요청은 따로 된 실행기에서 한다. 첫 단계를 보낸 뒤 그 실행기의 하나뿐인 스레드를 3초 막아 두 번째
    // 단계를 늦춘다(Raft와 키퍼는 이 시험의 실행기에서 돈다).
    let cluster = h.nodes[&leader].cluster.clone();
    let (tx, mut removed) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let remove =
                tokio::spawn(
                    async move { cluster.request(LeaderRequest::Remove { id: gone }).await },
                );
            tokio::spawn(async { std::thread::sleep(Duration::from_secs(3)) });
            let _ = tx.send(remove.await.unwrap());
        });
    });
    // 키퍼가 먼저 마무리해 지운 노드가 learner로 보인다.
    eventually("the keeper finished the joint config", || async {
        h.voters().is_some_and(|v| v == rest)
    })
    .await;
    assert!(
        h.leader_members().unwrap().learners.contains_key(&gone),
        "{:?}",
        h.leader_members()
    );
    assert!(removed.try_recv().is_err(), "the removal was not delayed");
    removed.await.unwrap().unwrap();
    eventually("the removed node gone", || async {
        h.leader_members()
            .is_some_and(|m| !m.voters.contains_key(&gone) && !m.learners.contains_key(&gone))
    })
    .await;
    h.close().await;
}

/// 새 리더는 리스 표가 비어 죽은 투표 멤버도 한동안 건강하다고 본다. 그때 붐비는 zone의 살아 있는 투표 멤버를
/// 내리면 바꾼 뒤의 구성에서 살아 있는 것이 과반이 아니라 joint가 커밋되지 못하고 클러스터가 멈춘다. 키퍼는
/// 실제로 받은 갱신으로 과반을 세고, 새 임기의 처음에는 개수 때문에 내리지 않는다.
#[tokio::test]
async fn a_new_leader_does_not_demote_live_voters_while_dead_ones_look_healthy() {
    let config = VoterKeeperConfig {
        healthy_within: Duration::from_secs(3),
        offline_after: Duration::from_secs(6),
        ..keeper_config()
    };
    let mut h = Harness::start_with(5, config).await;
    let leader = h.leader().unwrap();
    let other = *h.nodes.keys().find(|id| **id != leader).unwrap();
    let survivors: BTreeSet<NodeId> = h
        .nodes
        .keys()
        .copied()
        .filter(|id| *id != leader && *id != other)
        .collect();
    // 죽을 둘(리더와 하나)은 혼자인 zone에, 남을 셋은 붐비는 zone a에 둔다. 남을 하나는 옛 노드라 그동안 키퍼가
    // 쉰다(상한 3이어도 다섯 그대로).
    let old = *survivors.iter().next().unwrap();
    for i in 1..=5 {
        let id = NodeId::from(i);
        let zone = if id == leader {
            "b"
        } else if id == other {
            "c"
        } else {
            "a"
        };
        h.register(i, zone, id != old).await;
    }
    h.set_voters(3).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.voters().map(|v| v.len()), Some(5));
    h.stop(leader).await;
    h.stop(other).await;
    // 옛 노드도 업그레이드했다. 새 리더의 키퍼가 움직이기 시작한다.
    let via = *survivors.iter().last().unwrap();
    h.nodes[&via]
        .cluster
        .update_status::<NodeSpec>(old.0, &status(true))
        .await
        .unwrap();
    eventually("only the live voters left", || async {
        h.voters().is_some_and(|v| v == survivors)
    })
    .await;
    h.close().await;
}

/// 합류하자마자 죽어 `voter-limit`를 보고하지 못한 learner(기능·버전 없는 Node 기록, 리스 없음)가 있어도 키퍼는
/// 움직인다.
#[tokio::test]
async fn a_learner_that_never_registered_does_not_stop_the_keeper() {
    let h = Harness::start(4).await;
    for i in 1..=4 {
        h.register(i, "a", true).await;
    }
    h.set_voters(3).await;
    eventually("three voters", || async {
        h.voters().is_some_and(|v| v.len() == 3)
    })
    .await;
    // 합류 처리(Node 기록, learner 추가)만 되고 노드는 뜨지 못했다.
    let ghost = NodeId::from(9);
    let mut node = Resource::new(
        "n9",
        NodeSpec {
            address: "node-9:8006".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = ghost.0;
    h.any().create(&node).await.unwrap();
    let leader = h.leader().unwrap();
    h.nodes[&leader]
        .cluster
        .raft()
        .unwrap()
        .add_learner(ghost, BasicNode::new("node-9"), false)
        .await
        .unwrap();
    // 상한을 다섯으로 올리면 살아 있는 learner를 다시 올린다.
    h.set_voters(5).await;
    eventually("four voters", || async {
        h.voters()
            .is_some_and(|v| v.len() == 4 && !v.contains(&ghost))
    })
    .await;
    h.close().await;
}
```

`crates/opl-cluster/tests/witness.rs`에서

```rust
        for id in [node(2), node(3)] {
            let state = tc.get(id).raft().metrics().borrow().state;
            assert_eq!(state, ServerState::Follower, "{id} stood for election");
```

을 다음으로 바꾼다:

```rust
        for id in [node(2), node(3)] {
            let state = tc.get(id).raft().unwrap().metrics().borrow().state;
            assert_eq!(state, ServerState::Follower, "{id} stood for election");
```

`crates/opl-peer/src/monitor.rs`에서

```rust
    let by_lease = old.is_empty();
    let term = cluster.raft().metrics().borrow().current_term;
    if !by_lease && seen.warned != Some(term) {
```

을 다음으로 바꾼다:

```rust
    let by_lease = old.is_empty();
    let term = cluster
        .raft()
        .map_or(0, |raft| raft.metrics().borrow().current_term);
    if !by_lease && seen.warned != Some(term) {
```

`crates/opl-peer/src/server.rs`에서

```rust
use bytes::Bytes;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
```

을 다음으로 바꾼다:

```rust
use bytes::Bytes;
use openraft::Raft;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
```

`crates/opl-peer/src/server.rs`에서

```rust

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
```

을 다음으로 바꾼다:

```rust

    /// 이 노드의 Raft. Raft 구성원이 아니면 Raft RPC를 받지 않는다.
    fn raft(&self) -> Result<Raft<TypeConfig>, Status> {
        self.cluster
            .raft()
            .ok_or_else(|| Status::failed_precondition("this node is not a Raft member"))
    }

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
```

`crates/opl-peer/src/server.rs`에서

```rust
        self.authorize(&req)?;
        let reply = self.cluster.raft().append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
```

을 다음으로 바꾼다:

```rust
        self.authorize(&req)?;
        let reply = self.raft()?.append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
```

`crates/opl-peer/src/server.rs`에서

```rust
        self.authorize(&req)?;
        let reply = self.cluster.raft().install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
```

을 다음으로 바꾼다:

```rust
        self.authorize(&req)?;
        let reply = self.raft()?.install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
```

`crates/opl-peer/src/server.rs`에서

```rust
        self.authorize(&req)?;
        let reply = self.cluster.raft().vote(req.into_inner()).await;
        Ok(Response::new(reply))
```

을 다음으로 바꾼다:

```rust
        self.authorize(&req)?;
        let reply = self.raft()?.vote(req.into_inner()).await;
        Ok(Response::new(reply))
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            .iter()
            .map(|n| n.cluster.raft().metrics().borrow().current_leader)
            .collect();
```

을 다음으로 바꾼다:

```rust
            .iter()
            .map(|n| n.cluster.raft().unwrap().metrics().borrow().current_leader)
            .collect();
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().metrics().borrow().state == ServerState::Leader
        {
```

을 다음으로 바꾼다:

```rust
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().unwrap().metrics().borrow().state == ServerState::Leader
        {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .raft()
        .wait(Some(WAIT))
```

을 다음으로 바꾼다:

```rust
        .raft()
        .unwrap()
        .wait(Some(WAIT))
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
        .raft()
        .wait(Some(WAIT))
```

을 다음으로 바꾼다:

```rust
        .raft()
        .unwrap()
        .wait(Some(WAIT))
```

`crates/opld/src/bootstrap.rs`에서

```rust
            .raft()
            .wait(Some(Duration::from_secs(10)))
```

을 다음으로 바꾼다:

```rust
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
```

`crates/opld/src/member.rs`에서

```rust
    let id = cluster.id();
    let applied = cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
```

을 다음으로 바꾼다:

```rust
    let id = cluster.id();
    let Some(raft) = cluster.raft() else {
        return false;
    };
    let applied = raft
        .wait(Some(Duration::from_secs(10)))
```

`crates/opld/src/member.rs`에서

```rust
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되고 저장소에 적용되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지
    // 모른다(적용해야 다른 노드의 Node 리소스로 리더가 투표 멤버를 고르는지 안다).
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
```

을 다음으로 바꾼다:

```rust
    let id = cluster.id();
    // Raft 구성원이 아니면 올릴 것이 없다.
    let Some(raft) = cluster.raft() else {
        return;
    };
    // 저장한 멤버십이 지표에 반영되고 저장소에 적용되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지
    // 모른다(적용해야 다른 노드의 Node 리소스로 리더가 투표 멤버를 고르는지 안다).
    while raft
        .wait(Some(Duration::from_secs(10)))
```

`crates/opld/src/member.rs`에서

```rust
        .raft()
        .wait(Some(Duration::from_secs(30)))
```

을 다음으로 바꾼다:

```rust
        .raft()
        .context("this node is not a Raft member")?
        .wait(Some(Duration::from_secs(30)))
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1838개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 백엔드 틈과 공통 적용 리비전 — Backend, revision·wait_revision, FeedBatch"
```

---

### Task 2: Follow 피드와 복제본 백엔드

새 `opl-cluster/src/follow.rs`(`FollowFrame { Hello, SnapshotPart, SnapshotEnd, Batch, Progress }`, `NodeRole`, `FollowClient`, `Cluster::follow(caller, since)` — 기록이 지났으면 스냅샷 뒤 변경, 1초마다 `Progress`, 자기보다 앞선 호출자는 `Hello`만 보내고 닫음)와 `replica.rs`(관찰 노드 백엔드: 리더가 아닌 투표 멤버를 rendezvous 해시로 골라 따르고, 3초 진행이 없거나 리더가 알려 준 인덱스를 늦게 따라가면 다른 투표 멤버로 옮기며, 이미 적용한 리비전 이하 이벤트와 자기보다 낮은 스냅샷은 버린다). `state_machine::apply_feed`로 한 트랜잭션에 적용하고 같은 묶음을 로컬 감시에 낸다. `Cluster::open_observer(config, follow)`, 관찰 노드에서 쓰기·리스 갱신은 리더로 전달 뒤 `wait_revision`, `health()`는 붙은 투표 멤버의 진행으로. memnet `MemRouter::follower`.

**Files:**
- Create: `crates/opl-cluster/src/follow.rs`
- Create: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-cluster/Cargo.toml`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/store.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-cluster/Cargo.toml`에서

```toml
async-trait.workspace = true
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
async-trait.workspace = true
futures-util.workspace = true
openraft.workspace = true
```

`crates/opl-cluster/src/cluster.rs` 전체:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures_util::future::Either;
use futures_util::stream::BoxStream;
use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::follow::{self, FollowClient, FollowFrame};
use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::replica::Replica;
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
use crate::types::{Command, LeaderRequest, NodeId, RawResource, Rejection, TypeConfig};
use crate::vip::{VipLeases, VipTable};
use crate::watch::{Watch, WatchError, WatchHub};

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
            Backend::Member { raft, confirm } => Some(LeaderCtx {
                raft,
                confirm,
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
        let store = open_store(&config)?;
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
        let backend = Backend::Member {
            confirm: ConfirmBatch::for_raft(raft.clone()),
            raft,
        };
        Ok(Self::assemble(
            config.node_id,
            config.write_timeout,
            store,
            backend,
        ))
    }

    /// 저장소를 열고 관찰 노드로 시작한다(Raft를 띄우지 않음). 저장소의 멤버십에서 투표 멤버를 골라 `follow`로
    /// 붙고, 저장소의 적용 위치부터 이어 받는다. 쓰기와 리스 갱신은 `with_forwarder`의 전달자로 리더에 보낸다.
    pub async fn open_observer(
        config: ClusterConfig,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, ClusterError> {
        let store = open_store(&config)?;
        let replica = Replica::start(config.node_id, &store, follow)?;
        Ok(Self::assemble(
            config.node_id,
            config.write_timeout,
            store,
            Backend::Observer(replica),
        ))
    }

    fn assemble(id: NodeId, write_timeout: Duration, store: Store, backend: Backend) -> Self {
        Self {
            id,
            node: NodeHandle {
                id,
                db: store.db().clone(),
                hub: store.watch_hub().clone(),
                backend: Arc::new(RwLock::new(backend)),
                leases: Arc::default(),
                vips: Arc::default(),
            },
            store,
            write_timeout,
            forwarder: None,
            apply_lag: Default::default(),
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
        self.member()?
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

    pub async fn shutdown(&self) -> Result<(), ClusterError> {
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
    /// 관찰 노드는 `shutdown`하면 끝난다.
    pub fn stopped(&self) -> impl Future<Output = String> + Send + 'static {
        let mut rx = match self.node.backend() {
            Backend::Member { raft, .. } => raft.metrics(),
            Backend::Observer(replica) => {
                let mut stopped = replica.stopped();
                return Either::Right(async move {
                    let _ = stopped.wait_for(|s| *s).await;
                    "the replica stopped".to_owned()
                });
            }
        };
        Either::Left(async move {
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
```

`crates/opl-cluster/src/follow.rs`를 만든다:

```rust
//! 관찰 노드가 투표 멤버 하나에서 받는 스트림(설계 §6): 스냅샷(처음·뒤처졌을 때) + 적용 묶음 + 1초마다 진행.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use openraft::{BasicNode, LogId, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::cluster::{Backend, ClusterError, NodeHandle};
use crate::state_machine::snapshot_objects;
use crate::types::{NodeId, RawResource};
use crate::watch::{Feed, FeedBatch, FeedItem, WatchError, WatchEvent};

/// 서버가 이만큼마다 `Progress`를 보낸다. 관찰 노드는 이것으로 연결이 살아 있는지 안다.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// 스냅샷 조각 하나에 담는 리소스 수
const SNAPSHOT_PART_OBJECTS: usize = 256;

/// 서버가 보내고 아직 읽히지 않은 프레임 수. 받는 쪽이 느리면 서버의 피드가 `Lagged`로 끝난다.
const FRAME_BUFFER: usize = 16;

/// 노드가 클러스터에 무엇으로 참여하는지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    Voter,
    Learner,
    /// Raft 구성원이 아니다(관찰 노드).
    Observer,
}

/// Follow 스트림의 프레임
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
pub enum FollowFrame {
    /// 처음 프레임. `role`은 서버가 보는 부른 노드의 역할이다. 서버가 Raft 구성원이 아니면 이것만 보내고
    /// 닫는다(다른 투표 멤버로 가라는 뜻).
    Hello {
        role: NodeRole,
        leader: Option<(NodeId, String)>,
        voters: BTreeMap<NodeId, String>,
    },
    /// 스냅샷의 리소스 일부. `SnapshotEnd`까지 모아 한 번에 설치한다.
    SnapshotPart { objects: Vec<(Kind, RawResource)> },
    /// 스냅샷 끝. 상태 전체를 앞 조각들로 바꾼다(구독자에게는 `Reset`).
    SnapshotEnd {
        applied: Option<LogId<NodeId>>,
        membership: StoredMembership<NodeId, BasicNode>,
    },
    /// 서버가 적용한 묶음([`FeedBatch`]). 이벤트가 없는 묶음도 적용 위치로 온다.
    Batch {
        events: Vec<WatchEvent>,
        applied: LogId<NodeId>,
        membership: Option<StoredMembership<NodeId, BasicNode>>,
    },
    /// 서버가 적용한 위치와 서버가 아는 리더
    Progress {
        applied: Option<LogId<NodeId>>,
        leader: Option<NodeId>,
        leader_addr: Option<String>,
    },
}

impl FollowFrame {
    fn batch(b: &FeedBatch) -> Self {
        FollowFrame::Batch {
            events: b.events.clone(),
            applied: b.applied,
            membership: b.membership.clone(),
        }
    }
}

/// 관찰 노드가 투표 멤버에 Follow를 거는 통로. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait FollowClient: Send + Sync + 'static {
    /// `addr`(클러스터 주소)의 노드 `to`에서 `since` 뒤의 상태를 받는다. 스트림의 `Err`나 끝은 연결이 끊긴 것이다.
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError>;
}

/// 노드 `caller`에게 보낼 Follow 스트림. 프레임은 따로 띄운 작업이 만들고, 스트림을 버리면 그 작업도 끝난다.
pub(crate) fn serve(
    node: NodeHandle,
    caller: NodeId,
    since: u64,
) -> Result<BoxStream<'static, FollowFrame>, ClusterError> {
    let (hello, member) = hello(&node, caller)?;
    let (tx, rx) = mpsc::channel(FRAME_BUFFER);
    // 부른 쪽이 이 노드보다 앞섰다(다른 투표 멤버에서 옮겨 왔다). 받으면 이 노드가 그 사이 스냅샷을 설치할 때 부른
    // 쪽이 뒤로 가므로 `Hello`만 보내고 닫아 다른 투표 멤버로 가게 한다.
    let ahead = since > node.hub().revision();
    tokio::spawn(async move {
        if tx.send(hello).await.is_err() || !member {
            return;
        }
        if ahead {
            tracing::debug!(%caller, since, "the follower is ahead of this node; turning it away");
            return;
        }
        if let Err(why) = stream_changes(&node, since, &tx).await {
            tracing::debug!(%caller, %why, "follow stream ended");
        }
    });
    Ok(Box::pin(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (frame, rx))
    })))
}

/// 처음 프레임과, 이 노드가 Raft 구성원(스트림을 이어 보냄)인지
fn hello(node: &NodeHandle, caller: NodeId) -> Result<(FollowFrame, bool), ClusterError> {
    let (membership, leader, member) = match node.backend() {
        Backend::Member { raft, .. } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership().clone();
            let leader = m
                .current_leader
                .and_then(|l| membership.get_node(&l).map(|n| (l, n.addr.clone())));
            let member = membership.get_node(&node.id()).is_some();
            (membership, leader, member)
        }
        Backend::Observer(replica) => (
            replica.membership()?.membership().clone(),
            replica.leader(),
            false,
        ),
    };
    let voters: BTreeMap<NodeId, String> = membership
        .voter_ids()
        .filter_map(|id| membership.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect();
    let role = if voters.contains_key(&caller) {
        NodeRole::Voter
    } else if membership.get_node(&caller).is_some() {
        NodeRole::Learner
    } else {
        NodeRole::Observer
    };
    Ok((
        FollowFrame::Hello {
            role,
            leader,
            voters,
        },
        member,
    ))
}

/// `since` 뒤의 묶음을 보낸다(기록보다 오래면 스냅샷부터). 받는 쪽이 끊거나, 피드가 끊기거나, 이 노드의 Raft가
/// 멈추거나 구성원에서 빠지면 끝난다.
async fn stream_changes(
    node: &NodeHandle,
    since: u64,
    tx: &mpsc::Sender<FollowFrame>,
) -> Result<(), String> {
    let Some(raft) = node.raft() else {
        return Err("not a Raft member".into());
    };
    let mut metrics = raft.metrics();
    let mut feed = match node.hub().feed(since) {
        Ok(feed) => feed,
        Err(WatchError::Expired { .. }) => send_snapshot(node, tx).await?,
        Err(e) => return Err(e.to_string()),
    };
    let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
    loop {
        let frame = tokio::select! {
            item = feed.next() => match item.map_err(|e| e.to_string())? {
                FeedItem::Batch(b) => FollowFrame::batch(&b),
                // 이 노드가 스냅샷을 설치해 상태가 통째로 바뀌었다. 받는 쪽에도 스냅샷을 보낸다.
                FeedItem::Reset { .. } => {
                    feed = send_snapshot(node, tx).await?;
                    continue;
                }
            },
            _ = tick.tick() => {
                if metrics.has_changed().is_err() {
                    return Err("the Raft task ended".into());
                }
                let m = metrics.borrow_and_update().clone();
                if let Err(e) = &m.running_state {
                    return Err(e.to_string());
                }
                let membership = m.membership_config.membership();
                if membership.get_node(&node.id()).is_none() {
                    return Err("no longer a Raft member".into());
                }
                FollowFrame::Progress {
                    applied: m.last_applied,
                    leader: m.current_leader,
                    leader_addr: m
                        .current_leader
                        .and_then(|l| membership.get_node(&l).map(|n| n.addr.clone())),
                }
            }
        };
        tx.send(frame)
            .await
            .map_err(|_| "the follower went away".to_string())?;
    }
}

/// 지금 상태를 스냅샷으로 보내고, 그 리비전 뒤의 피드를 돌려준다.
async fn send_snapshot(node: &NodeHandle, tx: &mpsc::Sender<FollowFrame>) -> Result<Feed, String> {
    let (applied, membership, objects, feed) = loop {
        let db = node.db().clone();
        let (applied, membership, objects) =
            tokio::task::spawn_blocking(move || snapshot_objects(&db))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        // 읽은 뒤 기록이 그 리비전을 지나쳤으면(아주 드묾) 다시 뜬다.
        match node.hub().feed(applied.map_or(0, |l| l.index)) {
            Ok(feed) => break (applied, membership, objects, feed),
            Err(WatchError::Expired { .. }) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let gone = |_| "the follower went away".to_string();
    let mut objects = objects.into_iter().peekable();
    while objects.peek().is_some() {
        let part = objects.by_ref().take(SNAPSHOT_PART_OBJECTS).collect();
        tx.send(FollowFrame::SnapshotPart { objects: part })
            .await
            .map_err(gone)?;
    }
    tx.send(FollowFrame::SnapshotEnd {
        applied,
        membership,
    })
    .await
    .map_err(gone)?;
    Ok(feed)
}

#[cfg(test)]
mod tests {
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    use super::*;

    /// 프레임은 노드 사이로 JSON으로 오간다(gRPC). 리비전 틈을 메우는 적용 위치와 멤버십까지 그대로 돌아온다.
    #[test]
    fn frames_round_trip_through_json() {
        let applied = LogId::new(CommittedLeaderId::new(3, NodeId::from(1)), 42);
        let mut resource: RawResource = Resource::new("a", json!({"x": 1}));
        resource.revision = 41;
        let frames = [
            FollowFrame::Hello {
                role: NodeRole::Observer,
                leader: Some((NodeId::from(1), "node-1".into())),
                voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
            },
            FollowFrame::SnapshotPart {
                objects: vec![(Kind::Vm, resource.clone())],
            },
            FollowFrame::SnapshotEnd {
                applied: Some(applied),
                membership: StoredMembership::default(),
            },
            FollowFrame::Batch {
                events: vec![WatchEvent::Put {
                    kind: Kind::Vm,
                    resource,
                }],
                applied,
                membership: Some(StoredMembership::default()),
            },
            FollowFrame::Progress {
                applied: Some(applied),
                leader: None,
                leader_addr: None,
            },
        ];
        for frame in frames {
            let json = serde_json::to_vec(&frame).unwrap();
            assert_eq!(serde_json::from_slice::<FollowFrame>(&json).unwrap(), frame);
        }
    }
}
```

`crates/opl-cluster/src/lib.rs`에서

```rust
mod exclusive;
mod health;
```

을 다음으로 바꾼다:

```rust
mod exclusive;
mod follow;
mod health;
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub mod memnet;
mod state_machine;
```

을 다음으로 바꾼다:

```rust
pub mod memnet;
mod replica;
mod state_machine;
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use health::{QUORUM_ACK_MAX, RaftHealth};
```

을 다음으로 바꾼다:

```rust
pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use follow::{FollowClient, FollowFrame, NodeRole, PROGRESS_INTERVAL};
pub use health::{QUORUM_ACK_MAX, RaftHealth};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
use async_trait::async_trait;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
```

을 다음으로 바꾼다:

```rust
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
use crate::cluster::{Cluster, ClusterError, NodeHandle};
use crate::leader::{self, Forwarder};
```

을 다음으로 바꾼다:

```rust
use crate::cluster::{Cluster, ClusterError, NodeHandle};
use crate::follow::{FollowClient, FollowFrame};
use crate::leader::{self, Forwarder};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
    replies_lost: HashSet<NodeId>,
}
```

을 다음으로 바꾼다:

```rust
    replies_lost: HashSet<NodeId>,
    /// 노드에 건 Follow 수
    follows: HashMap<NodeId, usize>,
}
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
        Arc::new(MemForwarder {
            from,
```

을 다음으로 바꾼다:

```rust
        Arc::new(MemForwarder {
            from,
            router: self.clone(),
        })
    }

    /// `from` 노드(관찰 노드)가 쓸 Follow 통로. `Cluster::open_observer`에 넘긴다.
    pub fn follower(&self, from: NodeId) -> Arc<MemFollow> {
        Arc::new(MemFollow {
            from,
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
        self.routes.lock().unwrap().replies_lost.insert(id);
    }
```

을 다음으로 바꾼다:

```rust
        self.routes.lock().unwrap().replies_lost.insert(id);
    }

    /// 노드 `to`에 건 Follow 수
    pub fn follows(&self, to: NodeId) -> usize {
        self.routes
            .lock()
            .unwrap()
            .follows
            .get(&to)
            .copied()
            .unwrap_or(0)
    }
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&node.leader_ctx(), req, FORWARD_TIMEOUT).await
    }
```

을 다음으로 바꾼다:

```rust
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        match node.leader_ctx() {
            Some(ctx) => leader::execute(&ctx, req, FORWARD_TIMEOUT).await,
            None => Err(node.not_leader()),
        }
    }
}

pub struct MemFollow {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl FollowClient for MemFollow {
    async fn follow(
        &self,
        to: NodeId,
        _addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        *self
            .router
            .routes
            .lock()
            .unwrap()
            .follows
            .entry(to)
            .or_default() += 1;
        let node = self
            .router
            .route(self.from, to)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        let frames = node.follow(self.from, since)?;
        // 프레임마다 길을 다시 본다. 두 노드 가운데 하나가 격리되거나 등록이 풀리면 연결이 끊긴 것처럼 끝난다.
        let (router, from) = (self.router.clone(), self.from);
        Ok(frames
            .take_while(move |_| std::future::ready(router.route(from, to).is_ok()))
            .map(Ok)
            .boxed())
    }
```

`crates/opl-cluster/src/replica.rs`를 만든다:

```rust
//! 관찰 노드의 복제본(설계 §6). Raft 구성원이 아니고, 투표 멤버 하나에 Follow로 붙어 받은 스냅샷과 적용 묶음을
//! 자기 저장소에 쓴다. 쓰기와 리스 갱신은 `Cluster`가 리더로 전달한다.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::{BasicNode, StoredMembership};
use redb::{Database, ReadableDatabase};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cluster::ClusterError;
use crate::follow::{FollowClient, FollowFrame};
use crate::health::ApplyLag;
use crate::state_machine::{Objects, apply_feed, install_objects};
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::NodeId;
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, BasicNode>;

/// 붙은 노드에서 이만큼 아무 프레임도 오지 않으면 끊고 다시 붙는다(서버는 1초마다 `Progress`를 보낸다).
pub(crate) const FOLLOW_STALL: Duration = Duration::from_secs(3);

/// 다시 붙기 전에 쉬는 시간
const RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// 리더가 돌려준 인덱스(전달한 쓰기·리스 갱신)를 이보다 오래 적용하지 못하면 붙은 노드가 뒤처진 것으로 보고 다른
/// 투표 멤버로 옮긴다(살아 있어 `Progress`는 오지만 로그를 받지 못하는 노드).
pub(crate) const UPSTREAM_LAG_MAX: Duration = Duration::from_secs(3);

/// 연결 하나가 끝난 까닭
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// 붙지 못했거나 진행을 하나도 받지 못하고 끊겼다. 다음에는 다른 노드를 고른다.
    Failed,
    /// 진행을 받다가 끊겼다.
    Progressed,
    /// 붙은 노드가 리더다(리더를 모르고 골랐거나 그 노드가 리더가 됐다). 다른 투표 멤버로 옮긴다.
    Move,
    /// 붙은 노드가 리더보다 뒤처졌다. 다음에는 다른 노드를 고른다.
    Lagging,
}

/// 관찰 노드의 복제본. 복제 작업은 `start`에서 띄우고 `shutdown`으로 멈춘다.
#[derive(Clone)]
pub(crate) struct Replica {
    inner: Arc<Inner>,
}

struct Inner {
    id: NodeId,
    db: Arc<Database>,
    hub: Arc<WatchHub>,
    writer: StoreWriter,
    follow: Arc<dyn FollowClient>,
    status: Mutex<Status>,
    /// `shutdown`이 `true`로 바꾼다.
    stopped: watch::Sender<bool>,
    /// `follow_another`가 올린다. 붙은 노드를 버리고 다른 노드로 옮긴다.
    leave_upstream: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// 붙은 노드에게서 들은 것
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    pub(crate) leader: Option<(NodeId, String)>,
    /// 붙을 후보(투표 멤버와 클러스터 주소)
    pub(crate) voters: BTreeMap<NodeId, String>,
    /// 지금 붙어 있는 노드
    pub(crate) upstream: Option<NodeId>,
    /// 붙은 노드가 마지막으로 알린 적용 인덱스
    pub(crate) upstream_applied: u64,
    /// 리더가 돌려준 가장 큰 인덱스(전달한 쓰기·리스 갱신). 이만큼 적용하지 못한 시간이 적용 지연이다.
    pub(crate) leader_applied: u64,
    /// 붙은 노드에서 마지막으로 진행(스냅샷·묶음·`Progress`)을 받은 때
    pub(crate) last_progress: Option<Instant>,
    /// 연결이 끝나 다시 붙은 횟수
    pub(crate) reconnects: u64,
}

impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다. 저장소의 적용 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
        id: NodeId,
        store: &Store,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let voters = voter_addrs(&read_membership(&db)?);
        let inner = Arc::new(Inner {
            id,
            db,
            hub: store.watch_hub().clone(),
            writer: store.writer().clone(),
            follow,
            status: Mutex::new(Status {
                voters,
                ..Default::default()
            }),
            stopped: watch::channel(false).0,
            leave_upstream: watch::channel(0).0,
            task: Mutex::new(None),
        });
        let task = tokio::spawn(inner.clone().run());
        *inner.task.lock().unwrap() = Some(task);
        Ok(Self { inner })
    }

    pub(crate) fn status(&self) -> Status {
        self.inner.status.lock().unwrap().clone()
    }

    pub(crate) fn leader(&self) -> Option<(NodeId, String)> {
        self.inner.status.lock().unwrap().leader.clone()
    }

    /// 저장소에 적용된 마지막 멤버십
    pub(crate) fn membership(&self) -> Result<Membership, StoreError> {
        read_membership(&self.inner.db)
    }

    /// 리더를 묻는 쪽에 돌려줄 `NotLeader`
    pub(crate) fn not_leader(&self) -> ClusterError {
        let leader = self.leader();
        ClusterError::NotLeader {
            leader_id: leader.as_ref().map(|(id, _)| *id),
            leader_addr: leader.map(|(_, addr)| addr),
        }
    }

    /// 리더가 돌려준 인덱스(그때 리더가 적용했거나 커밋한 것)를 적는다. 붙은 노드가 이것을 오래 보내지 않으면
    /// 옮긴다.
    pub(crate) fn heard_from_leader(&self, index: u64) {
        let mut status = self.inner.status.lock().unwrap();
        status.leader_applied = status.leader_applied.max(index);
    }

    /// 붙은 노드를 버리고 다른 투표 멤버로 옮긴다(리더가 돌려준 인덱스를 기다리다 시간이 지났을 때).
    pub(crate) fn follow_another(&self) {
        self.inner.leave_upstream.send_modify(|n| *n += 1);
    }

    /// 복제 작업을 멈춘다.
    pub(crate) async fn shutdown(&self) {
        let task = self.inner.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.inner.stopped.send_replace(true);
    }

    /// `shutdown`하면 `true`가 되는 수신자
    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.inner.stopped.subscribe()
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        // 이번 차례에 붙지 못했거나 아무것도 받지 못한 노드. 진행을 받으면 비운다.
        let mut skip = BTreeSet::new();
        loop {
            let Some((to, addr)) = self.pick(&skip) else {
                skip.clear();
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            };
            let since = self.hub.revision();
            // 다른 노드에 붙지 못해 리더까지 왔으면 리더에 머문다.
            let may_move = skip.is_empty();
            let outcome = match self.follow.follow(to, &addr, since).await {
                Ok(frames) => self.consume(to, frames, may_move).await,
                Err(e) => {
                    tracing::debug!(upstream = %to, error = %e, "cannot follow");
                    Outcome::Failed
                }
            };
            {
                let mut status = self.status.lock().unwrap();
                status.upstream = None;
                status.reconnects += 1;
            }
            match outcome {
                Outcome::Failed | Outcome::Lagging => {
                    skip.insert(to);
                }
                Outcome::Progressed => skip.clear(),
                Outcome::Move => {}
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// 붙을 노드: 리더가 아닌 투표 멤버 가운데 이 노드와의 rendezvous 해시가 가장 큰 것(관찰 노드가 고르게
    /// 퍼진다), 없으면 리더. `skip`에 든 노드는 건너뛴다.
    fn pick(&self, skip: &BTreeSet<NodeId>) -> Option<(NodeId, String)> {
        let status = self.status.lock().unwrap();
        let leader = status.leader.as_ref().map(|(id, _)| *id);
        let mut ranked: Vec<(&NodeId, &String)> = status
            .voters
            .iter()
            .filter(|(id, _)| Some(**id) != leader && **id != self.id)
            .collect();
        ranked.sort_by_key(|(id, _)| std::cmp::Reverse(rendezvous(self.id, **id)));
        ranked
            .into_iter()
            .map(|(id, addr)| (*id, addr.clone()))
            .chain(status.leader.clone())
            .find(|(id, _)| !skip.contains(id))
    }

    /// 스트림이 끝나거나 `FOLLOW_STALL` 동안 멈출 때까지 프레임을 적용한다. `may_move`면 붙은 노드가 리더일 때
    /// 다른 투표 멤버가 있으면 끊는다.
    async fn consume(
        &self,
        to: NodeId,
        mut frames: BoxStream<'static, Result<FollowFrame, ClusterError>>,
        may_move: bool,
    ) -> Outcome {
        let mut progressed = false;
        let ended = |progressed| {
            if progressed {
                Outcome::Progressed
            } else {
                Outcome::Failed
            }
        };
        let mut parts = Vec::new();
        // 리더가 돌려준 인덱스를 이 노드에서 적용하지 못한 시간(붙을 때마다 새로 잰다)
        let mut lag = ApplyLag::default();
        let mut leave = self.leave_upstream.subscribe();
        leave.borrow_and_update();
        loop {
            if may_move && self.upstream_is_leader(to) {
                return Outcome::Move;
            }
            let leader_applied = self.status.lock().unwrap().leader_applied;
            if lag.observe(
                Some(leader_applied),
                Some(self.hub.revision()),
                Instant::now(),
            ) > UPSTREAM_LAG_MAX
            {
                tracing::warn!(upstream = %to, leader_applied, "the followed node lags behind the leader; following another");
                return Outcome::Lagging;
            }
            let next = tokio::select! {
                next = tokio::time::timeout(FOLLOW_STALL, frames.next()) => next,
                _ = leave.changed() => {
                    tracing::warn!(upstream = %to, "a forwarded request was not applied in time; following another");
                    return Outcome::Lagging;
                }
            };
            let frame = match next {
                Ok(Some(Ok(frame))) => frame,
                Ok(Some(Err(e))) => {
                    tracing::debug!(upstream = %to, error = %e, "follow stream failed");
                    return ended(progressed);
                }
                Ok(None) => return ended(progressed),
                Err(_) => {
                    tracing::warn!(upstream = %to, "no progress from the followed node; following another");
                    return ended(progressed);
                }
            };
            let result = match frame {
                FollowFrame::Hello { leader, voters, .. } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream = Some(to);
                    status.leader = leader;
                    if !voters.is_empty() {
                        status.voters = voters;
                    }
                    continue;
                }
                FollowFrame::SnapshotPart { objects } => {
                    parts.extend(objects);
                    continue;
                }
                FollowFrame::SnapshotEnd {
                    applied,
                    membership,
                } => {
                    self.install(applied, membership, std::mem::take(&mut parts))
                        .await
                }
                FollowFrame::Batch {
                    events,
                    applied,
                    membership,
                } => {
                    self.apply(FeedBatch {
                        events,
                        applied,
                        membership,
                    })
                    .await
                }
                FollowFrame::Progress {
                    applied,
                    leader,
                    leader_addr,
                } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream_applied = applied.map_or(0, |l| l.index);
                    status.leader = leader.zip(leader_addr);
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::error!(upstream = %to, error = %e, "cannot write the followed state");
                return ended(progressed);
            }
            progressed = true;
            self.status.lock().unwrap().last_progress = Some(Instant::now());
        }
    }

    /// 붙은 노드가 리더이고 붙을 다른 투표 멤버가 있다.
    fn upstream_is_leader(&self, to: NodeId) -> bool {
        let status = self.status.lock().unwrap();
        status.leader.as_ref().is_some_and(|(l, _)| *l == to)
            && status.voters.keys().any(|v| *v != to && *v != self.id)
    }

    async fn install(
        &self,
        applied: Option<openraft::LogId<NodeId>>,
        membership: Membership,
        objects: Objects,
    ) -> Result<(), StoreError> {
        let voters = voter_addrs(&membership);
        let revision = applied.map_or(0, |l| l.index);
        // 붙은 노드가 이 노드보다 뒤다(뒤처진 노드에 붙은 뒤 그 노드가 스냅샷을 설치했다). 설치하면 이미 돌려준 리비전
        // 읽기·리스 갱신이 거짓이 되므로 두고, 이어 오는 묶음에서 이미 적용한 것은 건너뛴다. 같은 리비전이면 상태가
        // 같으므로 설치하지 않는다(구독자에게 헛된 `Reset`을 내지 않게).
        if revision <= self.hub.revision() {
            tracing::warn!(
                revision,
                applied = self.hub.revision(),
                "ignoring a snapshot not newer than this node's state"
            );
            return Ok(());
        }
        self.writer
            .run(move |db| install_objects(db, applied, membership, objects))
            .await?;
        self.hub.reset(revision);
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(revision);
        if !voters.is_empty() {
            status.voters = voters;
        }
        Ok(())
    }

    async fn apply(&self, mut batch: FeedBatch) -> Result<(), StoreError> {
        // 다시 붙은 뒤 이미 적용한 묶음이 올 수 있다.
        if batch.applied.index <= self.hub.revision() {
            return Ok(());
        }
        // 투표 멤버마다 묶음 경계가 달라, 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 변경을 품을 수 있다. 그것은 쓰지도
        // 내지도 않는다(옛 값으로 되돌리거나 감시에 두 번 보이지 않게). 적용 위치는 묶음의 것이다.
        let applied_here = self.hub.revision();
        batch.events.retain(|e| e.revision() > applied_here);
        let batch = self
            .writer
            .run(move |db| apply_feed(db, &batch).map(|()| batch))
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(batch.applied.index);
        if let Some(m) = &batch.membership {
            let voters = voter_addrs(m);
            if !voters.is_empty() {
                status.voters = voters;
            }
        }
        drop(status);
        self.hub.publish(batch);
        Ok(())
    }
}

/// 같은 관찰 노드는 늘 같은 순서로, 관찰 노드마다는 다른 순서로 투표 멤버를 고르게 하는 점수
fn rendezvous(me: NodeId, voter: NodeId) -> u64 {
    let mut h = DefaultHasher::new();
    (me, voter).hash(&mut h);
    h.finish()
}

fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default())
}

fn voter_addrs(m: &Membership) -> BTreeMap<NodeId, String> {
    let m = m.membership();
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use openraft::BasicNode;
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tempfile::TempDir;

    use super::*;
    use crate::cluster::{Cluster, ClusterConfig};
    use crate::memnet::MemRouter;
    use crate::types::LeaderRequest;
    use crate::watch::{HISTORY, WatchError, WatchEvent};

    const WAIT: Duration = Duration::from_secs(10);

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

    fn config(root: &TempDir, i: u64, in_memory: bool) -> ClusterConfig {
        let mut config = ClusterConfig::new(NodeId::from(i), root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.in_memory = in_memory;
        config
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !cond() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 투표 멤버 1..=3(메모리 저장소)과 관찰 노드 4..(파일 저장소). 관찰 노드는 실제 길처럼 learner로 가입해
    /// 따라잡은 뒤 멤버십에서 빠지고, 같은 저장소로 관찰 노드로 다시 열린다.
    struct Net {
        router: MemRouter,
        root: TempDir,
        voters: BTreeMap<NodeId, Cluster>,
        observers: BTreeMap<NodeId, Cluster>,
    }

    impl Net {
        async fn start(observers: u64) -> Self {
            Self::start_with(observers, |_| {}).await
        }

        /// 투표 멤버의 Raft 설정을 `raft`로 고쳐 띄운다.
        async fn start_with(observers: u64, raft: impl Fn(&mut openraft::Config)) -> Self {
            let mut net = Net {
                router: MemRouter::default(),
                root: TempDir::new().unwrap(),
                voters: BTreeMap::new(),
                observers: BTreeMap::new(),
            };
            for i in 1..=3 {
                let id = NodeId::from(i);
                let mut config = config(&net.root, i, true);
                raft(&mut config.raft);
                let c = Cluster::open(config, net.router.network(id))
                    .await
                    .unwrap()
                    .with_forwarder(net.router.forwarder(id));
                net.router.register(id, &c);
                net.voters.insert(id, c);
            }
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
                .collect();
            net.voters[&NodeId::from(1)]
                .initialize(members)
                .await
                .unwrap();
            for i in 4..4 + observers {
                net.add_observer(i).await;
            }
            net
        }

        fn leader(&self) -> &Cluster {
            self.voters
                .values()
                .find(|c| c.members().leader == Some(c.id()))
                .expect("a leader")
        }

        async fn settled_leader(&self) -> NodeId {
            wait_until("a leader", || {
                self.voters
                    .values()
                    .any(|c| c.members().leader == Some(c.id()))
            })
            .await;
            self.leader().id()
        }

        async fn add_observer(&mut self, i: u64) {
            let id = NodeId::from(i);
            self.settled_leader().await;
            let learner = Cluster::open(config(&self.root, i, false), self.router.network(id))
                .await
                .unwrap();
            self.router.register(id, &learner);
            self.leader()
                .add_learner(id, BasicNode::new(format!("node-{i}")))
                .await
                .unwrap();
            self.leader()
                .request(LeaderRequest::Remove { id })
                .await
                .unwrap();
            self.router.unregister(id);
            learner.shutdown().await.unwrap();
            drop(learner);
            let observer = loop {
                match Cluster::open_observer(config(&self.root, i, false), self.router.follower(id))
                    .await
                {
                    Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    other => break other.unwrap(),
                }
            }
            .with_forwarder(self.router.forwarder(id));
            self.router.register(id, &observer);
            self.observers.insert(id, observer);
        }

        fn observer(&self, n: usize) -> &Cluster {
            self.observers.values().nth(n).unwrap()
        }

        /// 모든 관찰 노드가 리더의 지금 리비전까지 적용하기를 기다린다.
        async fn sync(&self) -> u64 {
            let revision = self.leader().revision();
            for o in self.observers.values() {
                o.wait_revision(revision, WAIT).await.unwrap();
            }
            revision
        }

        async fn stop_voter(&mut self, id: NodeId) {
            self.router.unregister(id);
            self.voters.remove(&id).unwrap().shutdown().await.unwrap();
        }

        /// 모든 투표 멤버의 기록이 `revision`을 지나칠 때까지 리더에 쓴다.
        async fn write_past_history(&self, revision: u64) {
            let mut n = 0;
            while self.voters.values().any(|v| {
                !matches!(
                    v.node_handle().hub().feed(revision),
                    Err(WatchError::Expired { .. })
                )
            }) {
                for _ in 0..64 {
                    self.leader()
                        .create(&volume(&format!("bulk-{n}")))
                        .await
                        .unwrap();
                    n += 1;
                }
                assert!(n < 4 * HISTORY, "history never expired");
            }
        }

        async fn shutdown(self) {
            for c in self.observers.values().chain(self.voters.values()) {
                c.shutdown().await.unwrap();
            }
        }
    }

    fn replica(c: &Cluster) -> Replica {
        match c.node_handle().backend() {
            crate::cluster::Backend::Observer(r) => r,
            crate::cluster::Backend::Member { .. } => panic!("not an observer"),
        }
    }

    async fn next_events(w: &mut crate::watch::Watch, n: usize) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap());
        }
        out
    }

    /// 관찰 노드는 Raft 밖에서 투표 멤버와 같은 객체를 같은 `list` 리비전으로 보인다.
    #[tokio::test]
    async fn observers_serve_the_same_objects_at_the_same_list_revision() {
        let net = Net::start(2).await;
        let vols: Vec<Volume> = (0..3).map(|i| volume(&format!("v{i}"))).collect();
        for v in &vols {
            net.leader().create(v).await.unwrap();
        }
        let gone = vols[0].id;
        net.leader().delete::<VolumeSpec>(gone, None).await.unwrap();
        net.sync().await;
        let expected = net.leader().list::<VolumeSpec>().unwrap();
        for o in net.observers.values() {
            assert!(o.raft().is_none());
            assert_eq!(o.list::<VolumeSpec>().unwrap(), expected);
            assert!(o.get::<VolumeSpec>(gone).unwrap().is_none());
            assert!(o.get_by_name::<VolumeSpec>("v0").unwrap().is_none());
            assert_eq!(
                o.get_by_name::<VolumeSpec>("v1").unwrap().map(|v| v.id),
                Some(vols[1].id)
            );
            // 멤버십은 저장소에 적용된 것, 리더는 붙은 노드가 알린 것이다.
            let members = o.members();
            assert_eq!(members.leader, Some(net.leader().id()));
            assert_eq!(members.voters.len(), 3);
            assert!(!members.contains(o.id()));
        }
        net.shutdown().await;
    }

    /// 관찰 노드에서 쓴 것은 돌아오자마자 그 노드에서 읽히고, 리스 갱신은 리더로 전달되어 성공한다.
    /// 리더로서 할 일은 아는 리더를 담은 `NotLeader`다.
    #[tokio::test]
    async fn an_observer_reads_its_own_writes_and_renews_its_lease() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let v = volume("mine");
        let revision = o.create(&v).await.unwrap();
        assert!(o.revision() >= revision);
        assert_eq!(
            o.get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
            revision
        );

        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        assert!(net.leader().leases().renewed.contains_key(&o.id()));
        assert!(o.leases().renewed.is_empty());
        let leader = net.leader().id();
        assert!(matches!(
            o.read_barrier().await,
            Err(ClusterError::NotLeader { leader_id: Some(l), leader_addr: Some(_) }) if l == leader
        ));
        assert!(matches!(
            o.execute_as_leader(LeaderRequest::RenewLease { node: o.id() }).await,
            Err(ClusterError::NotLeader { leader_id: Some(l), .. }) if l == leader
        ));

        // 헬스: 붙은 노드의 진행이 오고 리더를 안다.
        let health = o.health();
        assert_eq!(health.leader, Some(leader));
        assert!(health.quorum);

        // Raft 구성원이 아닌 노드에 건 Follow는 `Hello`(부른 쪽의 역할)만 오고 닫힌다.
        let frames: Vec<FollowFrame> = o.follow(NodeId::from(99), 0).unwrap().collect().await;
        assert!(
            matches!(
                frames.as_slice(),
                [FollowFrame::Hello { role: crate::follow::NodeRole::Observer, voters, .. }] if voters.len() == 3
            ),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 관찰 노드는 리더가 아닌 투표 멤버에 붙는다. 시작할 때 리더를 몰라 리더에 붙었으면 `Hello`를 보고 옮긴다.
    #[tokio::test]
    async fn observers_follow_voters_other_than_the_leader() {
        let net = Net::start(4).await;
        let leader = net.settled_leader().await;
        for o in net.observers.values() {
            let r = replica(o);
            wait_until("following a non-leader voter", || {
                r.status().upstream.is_some_and(|u| u != leader)
            })
            .await;
        }
        net.shutdown().await;
    }

    /// 붙은 투표 멤버가 죽으면 다른 투표 멤버로 자기 리비전부터 다시 붙는다. 기록 안이라 `Reset`이 없다.
    #[tokio::test]
    async fn an_observer_moves_to_another_voter_without_reset() {
        let mut net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        // 리더가 아닌 투표 멤버에 붙는다.
        assert_ne!(upstream, leader);
        let before = r.status().reconnects;
        let mut w = net.observer(0).watch(net.sync().await).unwrap();

        net.stop_voter(upstream).await;
        let mut written = Vec::new();
        for i in 0..3 {
            let v = volume(&format!("after-{i}"));
            net.leader().create(&v).await.unwrap();
            written.push(v.id);
        }
        let events = next_events(&mut w, 3).await;
        let ids: Vec<_> = events
            .iter()
            .map(|e| match e {
                WatchEvent::Put { resource, .. } => resource.id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, written);
        let status = r.status();
        assert!(status.reconnects > before);
        assert!(status.upstream.is_some_and(|u| u != upstream));
        net.shutdown().await;
    }

    /// 끊긴 동안 투표 멤버의 기록이 지나가면 다시 붙을 때 스냅샷을 받고 구독자는 `Reset`을 본다.
    #[tokio::test]
    async fn an_observer_past_the_history_gets_a_snapshot_and_reset() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        let mut w = o.watch(revision).unwrap();
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        assert_eq!(o.revision(), revision);
        net.router.heal(o.id());

        let reset = tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap();
        assert!(matches!(reset, WatchEvent::Reset { .. }), "{reset:?}");
        net.sync().await;
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 적용이 막혀 뒤처진 관찰 노드는 서버 쪽 피드가 `Lagged`로 끝나 다시 붙고 따라잡는다.
    #[tokio::test]
    async fn a_lagging_observer_reconnects_and_catches_up() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        let revision = net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let before = r.status().reconnects;
        // 관찰 노드의 저장소 스레드를 붙잡는다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        net.write_past_history(revision + 64).await;
        release.send(()).unwrap();

        net.sync().await;
        assert!(r.status().reconnects > before);
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
    #[tokio::test]
    async fn observer_watch_events_match_the_voter() {
        let net = Net::start(2).await;
        let revision = net.sync().await;
        let mut watches: Vec<_> = std::iter::once(net.leader())
            .chain(net.observers.values())
            .map(|c| c.watch(revision).unwrap())
            .collect();
        let v = volume("w");
        net.leader().create(&v).await.unwrap();
        net.observer(1)
            .update_spec(
                v.id,
                None,
                BTreeMap::from([("a".into(), "b".into())]),
                &v.spec,
            )
            .await
            .unwrap();
        net.observer(0)
            .update_status::<VolumeSpec>(v.id, &Default::default())
            .await
            .unwrap();
        net.leader().delete::<VolumeSpec>(v.id, None).await.unwrap();

        let expected = next_events(&mut watches[0], 4).await;
        assert!(matches!(expected[3], WatchEvent::Deleted { .. }));
        for w in &mut watches[1..] {
            assert_eq!(next_events(w, 4).await, expected);
        }
        net.shutdown().await;
    }

    fn log_id(index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `revision`에 쓰인 볼륨
    fn raw(name: &str, revision: u64) -> crate::types::RawResource {
        let mut r: crate::types::RawResource =
            serde_json::from_value(serde_json::to_value(volume(name)).unwrap()).unwrap();
        r.revision = revision;
        r
    }

    fn put(r: &crate::types::RawResource) -> WatchEvent {
        WatchEvent::Put {
            kind: opl_types::resource::Kind::Volume,
            resource: r.clone(),
        }
    }

    fn hello() -> FollowFrame {
        FollowFrame::Hello {
            role: crate::follow::NodeRole::Observer,
            leader: Some((NodeId::from(1), "node-1".into())),
            voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
        }
    }

    /// 정해 둔 프레임을 돌려주는 Follow 통로(투표 멤버마다 묶음 경계·스냅샷이 다른 경우를 만든다). 부를 때마다 다음
    /// 대본을 쓰고, `hold`인 대본은 끝에서 끊지 않고 머문다. 대본이 떨어지면 붙지 못한다.
    #[derive(Default)]
    struct Scripted {
        scripts: Mutex<std::collections::VecDeque<(Vec<FollowFrame>, bool)>>,
    }

    impl Scripted {
        fn then(self, frames: Vec<FollowFrame>, hold: bool) -> Self {
            self.scripts.lock().unwrap().push_back((frames, hold));
            self
        }
    }

    #[async_trait::async_trait]
    impl FollowClient for Scripted {
        async fn follow(
            &self,
            _to: NodeId,
            _addr: &str,
            _since: u64,
        ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
            let Some((frames, hold)) = self.scripts.lock().unwrap().pop_front() else {
                return Err(ClusterError::Unreachable("no more scripts".into()));
            };
            let frames = futures_util::stream::iter(frames.into_iter().map(Ok));
            Ok(if hold {
                frames.chain(futures_util::stream::pending()).boxed()
            } else {
                frames.boxed()
            })
        }
    }

    /// 대본대로 받는 관찰 노드(투표 멤버 1만 든 멤버십을 둔 빈 저장소라 그 노드를 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let config = config(root, 9, false);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let store = Store::open(&config.data_dir.join(crate::cluster::DB_FILE)).unwrap();
        let membership = Membership::new(
            None,
            openraft::Membership::new(
                vec![[NodeId::from(1)].into()],
                BTreeMap::from([(NodeId::from(1), BasicNode::new("node-1"))]),
            ),
        );
        let txn = store.db().begin_write().unwrap();
        {
            let mut meta = txn.open_table(META).unwrap();
            crate::store::put_json(&mut meta, KEY_LAST_MEMBERSHIP, &membership).unwrap();
        }
        txn.commit().unwrap();
        drop(store);
        let script = Arc::new(script);
        loop {
            match Cluster::open_observer(config.clone(), script.clone()).await {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        }
    }

    /// 감시에서 더 오지 않을 때까지 받은 이벤트의 리비전
    async fn drain(w: &mut crate::watch::Watch) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        while let Ok(e) = tokio::time::timeout(Duration::from_millis(300), w.next()).await {
            out.push(e.unwrap());
        }
        out
    }

    /// 다른 투표 멤버로 옮기면 묶음 경계가 달라 새 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 관찰 노드는
    /// 그 이벤트를 다시 내지 않는다(감시 리비전이 엄격히 오른다).
    #[tokio::test]
    async fn events_already_applied_are_not_replayed_after_moving() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                false,
            )
            // 다른 투표 멤버는 11..=13을 한 묶음으로 적용했다.
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b), put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(13, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        o.shutdown().await.unwrap();
    }

    /// 자기보다 뒤인 상류가 보낸 스냅샷(더 낮은 리비전)은 설치하지 않는다: 상태와 리비전이 뒤로 가지 않고 `Reset`도
    /// 없다. 그 뒤 묶음은 이어 받는다.
    #[tokio::test]
    async fn an_older_snapshot_from_the_upstream_is_not_installed() {
        let root = TempDir::new().unwrap();
        let (x, y, z, w) = (raw("x", 5), raw("y", 20), raw("z", 14), raw("w", 21));
        let snapshot = |objects: Vec<&crate::types::RawResource>, applied| {
            vec![
                FollowFrame::SnapshotPart {
                    objects: objects
                        .into_iter()
                        .map(|r| (opl_types::resource::Kind::Volume, r.clone()))
                        .collect(),
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(applied)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot(vec![&x], 10));
        first.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(20),
            membership: None,
        });
        let mut second = vec![hello()];
        second.extend(snapshot(vec![&x, &z], 15));
        second.push(FollowFrame::Batch {
            events: vec![put(&w)],
            applied: log_id(21),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(20, WAIT).await.unwrap();
        let mut watch = o.watch(20).unwrap();
        o.wait_revision(21, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == w.id),
            "{events:?}"
        );
        assert!(o.get::<VolumeSpec>(y.id).unwrap().is_some());
        assert!(o.get::<VolumeSpec>(z.id).unwrap().is_none());
        assert_eq!(o.revision(), 21);
        o.shutdown().await.unwrap();
    }

    /// 자기와 같은 리비전의 스냅샷도 설치하지 않는다(상태가 같다): 구독자는 헛된 `Reset` 없이 다음 묶음만 본다.
    #[tokio::test]
    async fn a_snapshot_at_the_same_revision_is_not_installed_again() {
        let root = TempDir::new().unwrap();
        let (x, y) = (raw("x", 10), raw("y", 11));
        let snapshot = || {
            vec![
                FollowFrame::SnapshotPart {
                    objects: vec![(opl_types::resource::Kind::Volume, x.clone())],
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(10)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot());
        let mut second = vec![hello()];
        second.extend(snapshot());
        second.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(11),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut watch = o.watch(10).unwrap();
        o.wait_revision(11, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == y.id),
            "{events:?}"
        );
        o.shutdown().await.unwrap();
    }

    /// 투표 멤버는 자기보다 앞선 관찰 노드(`since`가 자기 리비전보다 큼)를 받지 않는다: `Hello`만 보내고 닫아 다른
    /// 투표 멤버로 가게 한다(뒤처진 노드가 그 뒤 스냅샷을 설치하면 관찰 노드가 뒤로 가므로).
    #[tokio::test]
    async fn a_voter_turns_away_an_observer_ahead_of_it() {
        let net = Net::start(0).await;
        let leader = net.settled_leader().await;
        let voter = &net.voters[&leader];
        let frames: Vec<FollowFrame> = tokio::time::timeout(
            Duration::from_secs(3),
            voter
                .follow(NodeId::from(9), voter.revision() + 100)
                .unwrap()
                .collect(),
        )
        .await
        .expect("the stream was not closed");
        assert!(
            matches!(frames.as_slice(), [FollowFrame::Hello { .. }]),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 뒤처진 투표 멤버(Raft가 끊김)가 리더의 스냅샷을 설치해도, 그보다 앞선 관찰 노드는 뒤로 가지 않는다(그런 투표
    /// 멤버에 붙지 않는다).
    #[tokio::test]
    async fn an_observer_ahead_of_a_lagging_voter_never_goes_back() {
        let mut net = Net::start_with(1, |raft| {
            raft.max_in_snapshot_log_to_keep = 0;
            raft.purge_batch_size = 1;
        })
        .await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        let lagging = *net
            .voters
            .keys()
            .find(|id| **id != leader && **id != upstream)
            .unwrap();
        net.router.block_raft(lagging);
        for i in 0..5 {
            net.leader()
                .create(&volume(&format!("old-{i}")))
                .await
                .unwrap();
        }
        // 리더가 스냅샷을 만들고 로그를 지워 뒤처진 노드는 스냅샷으로만 따라잡는다.
        let raft = net.leader().raft().unwrap();
        raft.trigger().snapshot().await.unwrap();
        let purged = raft
            .wait(Some(WAIT))
            .metrics(|m| m.purged.is_some(), "leader purged logs")
            .await
            .unwrap()
            .purged
            .unwrap()
            .index;
        for i in 0..20 {
            net.leader()
                .create(&volume(&format!("new-{i}")))
                .await
                .unwrap();
        }
        let ahead = net.sync().await;
        assert!(ahead > purged);
        let mut w = net.observer(0).watch(ahead).unwrap();

        // 붙은 노드를 잃은 관찰 노드는 리더가 아닌 투표 멤버인 뒤처진 노드를 먼저 시도한다(그 노드는 받지 않는다).
        let tried = net.router.follows(lagging);
        net.stop_voter(upstream).await;
        wait_until("the observer tried the lagging voter", || {
            net.router.follows(lagging) > tried
        })
        .await;
        net.router.heal(lagging);
        net.voters[&lagging]
            .wait_revision(ahead, WAIT)
            .await
            .unwrap();
        let v = volume("after");
        net.leader().create(&v).await.unwrap();
        net.sync().await;
        let o = net.observer(0);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        let events = drain(&mut w).await;
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, WatchEvent::Reset { .. })),
            "{events:?}"
        );
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 살아 있지만 뒤처진 투표 멤버(Raft가 끊겨 `Progress`만 보냄)에 붙은 관찰 노드는 리스 갱신의 적용 기다림이 시간
    /// 초과하거나 리더가 돌려준 인덱스보다 오래 뒤처지면 다른 투표 멤버로 옮겨, 갱신이 다시 성공한다.
    #[tokio::test]
    async fn an_observer_leaves_a_lagging_voter_and_renews_its_lease() {
        let net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        assert_ne!(upstream, leader);
        net.router.block_raft(upstream);
        net.leader().create(&volume("unseen")).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut failed = 0;
        while let Err(e) = o.renew_lease(Duration::from_secs(2)).await {
            failed += 1;
            assert!(
                tokio::time::Instant::now() < deadline,
                "renewals kept failing ({failed}): {e}"
            );
        }
        assert_ne!(r.status().upstream, Some(upstream));
        net.router.heal(upstream);
        net.shutdown().await;
    }
}
```

`crates/opl-cluster/src/state_machine.rs`에서

```rust
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;
```

을 다음으로 바꾼다:

```rust
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;
/// 상태 전체의 리소스(스냅샷 본문)
pub(crate) type Objects = Vec<(Kind, RawResource)>;
```

`crates/opl-cluster/src/state_machine.rs`에서

```rust
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let txn = db.begin_read()?;
        let meta_table = txn.open_table(META)?;
        let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
        let last_membership: Membership =
            get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
        let mut objects = Vec::new();
        for item in txn.open_table(OBJECTS)?.iter()? {
            let (key, value) = item?;
            let kind: Kind = serde_json::from_value(key.value().0.into())?;
            objects.push((kind, serde_json::from_slice(value.value())?));
        }
        drop(txn);

        let data = serde_json::to_vec(&SnapshotData {
```

을 다음으로 바꾼다:

```rust
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let (last_log_id, last_membership, objects) = snapshot_objects(db)?;
        let data = serde_json::to_vec(&SnapshotData {
```

`crates/opl-cluster/src/state_machine.rs`에서

```rust
        Ok(())
    }
}
```

을 다음으로 바꾼다:

```rust
        Ok(())
    }
}

/// 한 읽기 트랜잭션에서 뜬 상태 전체: 적용 위치, 멤버십, 리소스.
pub(crate) fn snapshot_objects(
    db: &Database,
) -> Result<(Option<LogId<NodeId>>, Membership, Objects), StoreError> {
    let txn = db.begin_read()?;
    let meta_table = txn.open_table(META)?;
    let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
    let last_membership: Membership =
        get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
    let mut objects = Vec::new();
    for item in txn.open_table(OBJECTS)?.iter()? {
        let (key, value) = item?;
        let kind: Kind = serde_json::from_value(key.value().0.into())?;
        objects.push((kind, serde_json::from_slice(value.value())?));
    }
    Ok((last_log_id, last_membership, objects))
}

/// 관찰 노드가 Follow로 받은 스냅샷(리소스 전체)을 `StateMachine::install`로 설치한다.
pub(crate) fn install_objects(
    db: &Database,
    applied: Option<LogId<NodeId>>,
    membership: Membership,
    objects: Objects,
) -> Result<(), StoreError> {
    let data = serde_json::to_vec(&SnapshotData {
        schema_version: SCHEMA_VERSION,
        objects,
    })?;
    let meta = Meta {
        snapshot_id: format!(
            "{}-{}",
            applied.map_or(0, |l| l.index),
            Uuid::new_v4().simple()
        ),
        last_log_id: applied,
        last_membership: membership,
    };
    StateMachine::install(db, &meta, data)
}

/// 관찰 노드가 Follow로 받은 적용 묶음을 쓴다. 리소스·이름, `last_applied`, 멤버십을 한 쓰기 트랜잭션에 기록한다.
/// 같은 묶음을 로컬 구독자에게 내는 것(`WatchHub::publish`)은 부른 쪽이 커밋 뒤에 한다.
pub(crate) fn apply_feed(db: &Database, batch: &FeedBatch) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    {
        let mut objects = txn.open_table(OBJECTS)?;
        let mut names = txn.open_table(NAMES)?;
        for event in &batch.events {
            match event {
                WatchEvent::Put { kind, resource } => {
                    let key = (kind.as_str(), resource.id.as_u128());
                    // 이름이 바뀐 리소스면 옛 이름 색인을 지운다.
                    let old: Option<RawResource> = match objects.get(key)? {
                        Some(v) => Some(serde_json::from_slice(v.value())?),
                        None => None,
                    };
                    if let Some(old) = old
                        && old.name != resource.name
                    {
                        names.remove((kind.as_str(), old.name.as_str()))?;
                    }
                    objects.insert(key, serde_json::to_vec(resource)?.as_slice())?;
                    names.insert(
                        (kind.as_str(), resource.name.as_str()),
                        resource.id.as_u128(),
                    )?;
                }
                WatchEvent::Deleted { kind, resource, .. } => {
                    objects.remove((kind.as_str(), resource.id.as_u128()))?;
                    names.remove((kind.as_str(), resource.name.as_str()))?;
                }
                // 피드 묶음에는 오지 않는다(스냅샷 설치는 `install_objects`).
                WatchEvent::Reset { .. } => {}
            }
        }
        let mut meta = txn.open_table(META)?;
        put_json(&mut meta, KEY_LAST_APPLIED, &batch.applied)?;
        if let Some(m) = &batch.membership {
            put_json(&mut meta, KEY_LAST_MEMBERSHIP, m)?;
        }
    }
    txn.commit()?;
    Ok(())
}
```

`crates/opl-cluster/src/store.rs`에서

```rust

    #[cfg(test)]
    pub(crate) fn writer(&self) -> &StoreWriter {
```

을 다음으로 바꾼다:

```rust

    pub(crate) fn writer(&self) -> &StoreWriter {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1852개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(cluster): Follow 피드와 복제본 백엔드 — 관찰 노드가 투표 멤버 하나를 따름"
```

---

### Task 3: gRPC Follow·관찰 노드 목록·API

opl-types `ClusterStatus.observers`(노드 id → 클러스터 주소)와 노드 기능 `observer`, `Members.observers`(캐시). opl-peer 서버 스트리밍 `follow`(JSON, 제어 채널; 프레임마다 호출자가 아직 멤버·관찰 노드인지 `FollowGate`로 다시 확인), 스냅샷·큰 묶음은 약 4 MiB로 나눔(`BatchPart`), 관찰 노드에 온 Raft RPC는 `failed_precondition`. opl-api: 노드 목록 `role`(voter/learner/observer), 가입 `via`는 투표 멤버, 관찰 노드 헬스, 인증 뒤 `Orbivisor-Min-Revision` 대기(다른 노드에서 만든 자격 증명은 기다린 뒤 다시 인증)와 `Orbivisor-Revision` 응답 헤더, 감사·지표 모으기와 janitor가 관찰 노드도 센다. `orbivisorctl cluster status`의 ROLE. OpenAPI와 `schema.d.ts`.

**Files:**
- Create: `crates/opl-api/src/revision.rs`
- Modify: `crates/opl-api/src/audit.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/health.rs`
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/metrics.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/health.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/follow.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-peer/build.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/src/wire.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-types/src/cluster.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/audit.rs`에서

```rust
        .chain(&members.learners)
        .filter_map(|(id, addr)| {
```

을 다음으로 바꾼다:

```rust
        .chain(&members.learners)
        .chain(&members.observers)
        .filter_map(|(id, addr)| {
```

`crates/opl-api/src/auth.rs`에서

```rust
    ) -> Result<Self, Self::Rejection> {
        let principal = state.authenticate(parts).await;
        crate::audit::note_auth(parts, principal.as_ref().ok());
        principal
    }
```

을 다음으로 바꾼다:

```rust
    ) -> Result<Self, Self::Rejection> {
        let mut principal = state.authenticate(parts).await;
        // `Orbivisor-Min-Revision`의 리비전까지 기다렸으면 다시 인증한다(다른 노드에서 막 만든 자격 증명은 받고,
        // 기다리는 사이 폐기된 것은 받지 않는다).
        let waited = crate::revision::wait_for(state, parts, principal.is_ok()).await;
        if let Ok(true) = waited {
            principal = state.authenticate(parts).await;
        }
        crate::audit::note_auth(parts, principal.as_ref().ok());
        let principal = principal?;
        crate::revision::authenticated(parts);
        waited?;
        Ok(principal)
    }
```

`crates/opl-api/src/cluster.rs` 전체:

```rust
//! 클러스터 정보, 노드 합류(조인 토큰, CSR 서명), 노드 제거.
//!
//! 새 노드는 조인 URL을 받아 이 노드의 `POST /cluster/join`을 부른다. 토큰을 지워 한 번만 쓰게 하고,
//! 노드의 CSR에 클러스터 CA로 서명하고, Node 리소스를 만들고, 노드를 learner로 더한다. 새 노드는
//! 받은 인증서로 노드 간 서버를 띄우고 로그를 따라잡은 뒤 스스로 투표 멤버 승격을 요청한다(모든 노드가
//! `voter-limit`를 알면 위트니스만 요청하고, 다른 노드는 리더의 `VoterKeeper`가 상한까지 올린다).

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_peer::pki::{ClusterCa, fingerprint};
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{Fencing, NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::version;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal, constant_time_eq, random_hex, sha256_hex};
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;

/// 조인 토큰의 기본 유효 시간
pub const DEFAULT_JOIN_TTL: Duration = Duration::from_secs(3600);
/// 조인 토큰의 최대 유효 시간
const MAX_JOIN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const JOIN_TOKEN_PREFIX: &str = "oplj_";
const JOIN_SCHEME: &str = "opl-join://";

/// `opl-join://<API 주소>?token=<토큰>&ca=<CA 지문>`. `orbivisorctl cluster join`에 그대로 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinUrl {
    /// 합류 요청을 받을 노드의 API 주소(`host:port`)
    pub address: String,
    pub token: String,
    /// 클러스터 CA 인증서의 지문(`sha256:<16진수>`). 새 노드는 이것으로 상대를 확인한다.
    pub ca_fingerprint: String,
}

impl fmt::Display for JoinUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{JOIN_SCHEME}{}?token={}&ca={}",
            self.address, self.token, self.ca_fingerprint
        )
    }
}

impl FromStr for JoinUrl {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "invalid join URL {s:?}: expected {JOIN_SCHEME}<host:port>?token=...&ca=sha256:..."
            )
        };
        let rest = s.trim().strip_prefix(JOIN_SCHEME).ok_or_else(bad)?;
        let (address, query) = rest.split_once('?').ok_or_else(bad)?;
        let (mut token, mut ca) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("token", v)) => token = Some(v.to_string()),
                Some(("ca", v)) => ca = Some(v.to_string()),
                _ => return Err(bad()),
            }
        }
        if host_of(address).is_none() {
            return Err(bad());
        }
        Ok(JoinUrl {
            address: address.to_string(),
            token: token.ok_or_else(bad)?,
            ca_fingerprint: ca.filter(|c| c.starts_with("sha256:")).ok_or_else(bad)?,
        })
    }
}

/// `host:port`나 `[v6]:port`의 호스트. 포트가 숫자가 아니면 `None`이다.
pub fn host_of(address: &str) -> Option<&str> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// 인증서 SAN에 넣을 수 있는 호스트인지. 특수 주소가 아닌 IP나 RFC 1123 호스트 이름(와일드카드, 끝의 점 없이)
fn valid_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
        return !ip.is_unspecified() && !ip.is_multicast() && !broadcast;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
pub(crate) fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
        .cluster
        .list::<ClusterSpec>()?
        .1
        .into_iter()
        .next()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_not_initialized",
                "the cluster has not been initialized",
            )
        })
}

fn ca_fingerprint(info: &ClusterInfo) -> Result<String, ApiError> {
    fingerprint(&info.spec.ca_cert_pem).map_err(ApiError::internal)
}

/// 노드가 클러스터에 무엇으로 참여하는지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    /// Raft 투표 멤버
    Voter,
    /// Raft learner(투표하지 않음, 대기·따라잡는 중)
    Learner,
    /// Raft 밖에서 투표 멤버 하나를 따르는 관찰 노드
    Observer,
}

/// 클러스터 멤버 한 노드
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MemberView {
    pub id: Uuid,
    /// Node 리소스 이름. 아직 없으면(합류 중) 비어 있다.
    pub name: Option<String>,
    /// API 주소
    pub address: Option<String>,
    /// 노드 간 통신 주소
    pub cluster_address: String,
    /// 투표 멤버인지(`role`이 `voter`)
    pub voter: bool,
    pub role: MemberRole,
    pub online: bool,
    /// 위트니스 노드인지(VM을 돌리지 않는 투표 멤버)
    pub witness: bool,
    /// 노드의 HA 펜싱. 펜싱이 꺼졌거나 Node가 아직 없으면 null이다.
    pub fencing: Option<Fencing>,
    /// 노드의 orbivisord 빌드 버전. 옛 버전 노드이거나 Node가 아직 없으면 null이다.
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterView {
    pub name: String,
    /// 클러스터 CA 인증서의 지문. `orbivisorctl login --ca-fingerprint`에 쓴다.
    pub ca_fingerprint: String,
    pub leader: Option<Uuid>,
    /// 이 응답을 보낸 노드의 id. 볼륨 업로드처럼 받은 노드에서 처리하는 요청에 쓴다.
    pub node: Uuid,
    pub members: Vec<MemberView>,
}

/// 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십과 관찰 노드)
#[utoipa::path(get, path = "/cluster", tag = "cluster",
    responses((status = 200, body = ClusterView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cluster(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<ClusterView>, ApiError> {
    state.authorize(&principal, Action::Read, ClusterSpec::KIND)?;
    let info = cluster_info(&state)?;
    let members = state.cluster.members();
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let view = |id: NodeId, addr: &String, role: MemberRole| {
        let node = nodes.iter().find(|n| n.id == id.0);
        MemberView {
            id: id.0,
            name: node.map(|n| n.name.clone()),
            address: node.map(|n| n.spec.address.clone()),
            cluster_address: addr.clone(),
            voter: role == MemberRole::Voter,
            role,
            online: node.is_some_and(|n| n.status.online),
            witness: node.is_some_and(|n| n.spec.witness),
            fencing: node.and_then(|n| n.status.ha).map(|ha| ha.fencing),
            version: node.and_then(|n| n.status.version.clone()),
        }
    };
    let mut list: Vec<MemberView> = members
        .voters
        .iter()
        .map(|(id, addr)| view(*id, addr, MemberRole::Voter))
        .collect();
    list.extend(
        members
            .learners
            .iter()
            .map(|(id, addr)| view(*id, addr, MemberRole::Learner)),
    );
    list.extend(
        members
            .observers
            .iter()
            .map(|(id, addr)| view(*id, addr, MemberRole::Observer)),
    );
    Ok(Json(ClusterView {
        name: info.name.clone(),
        ca_fingerprint: ca_fingerprint(&info)?,
        leader: members.leader.map(|id| id.0),
        node: state.node,
        members: list,
    }))
}

/// 앞단(로드 밸런서)이 부를 API 노드 하나
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiEndpoint {
    /// Node 리소스 이름
    pub name: String,
    /// API 주소(`host:port`)
    pub address: String,
    /// 장애 영역(노드 레이블 `zone`). 없으면 null이다.
    pub zone: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiEndpoints {
    /// 이름 순
    pub items: Vec<ApiEndpoint>,
}

/// `api` 역할 노드(기본: 위트니스가 아닌 노드)의 API 주소. 앞단은 이 노드들을 부르고
/// `GET /health`로 준비되지 않은 노드를 뺀다.
#[utoipa::path(get, path = "/cluster/api-endpoints", tag = "cluster",
    responses((status = 200, body = ApiEndpoints), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_api_endpoints(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<ApiEndpoints>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let mut items: Vec<ApiEndpoint> = state
        .cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| n.spec.is_api())
        .map(|n| ApiEndpoint {
            zone: n.labels.get("zone").cloned(),
            name: n.name,
            address: n.spec.address,
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(ApiEndpoints { items }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VersionInfo {
    /// 이 노드의 orbivisord 빌드 버전
    pub version: String,
}

/// 이 노드의 빌드 버전. 인증 없이 읽는다(로그인 화면과 orbivisorctl이 서버 버전을 본다).
#[utoipa::path(get, path = "/version", tag = "cluster", security(()),
    responses((status = 200, body = VersionInfo)))]
pub async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: version::BUILD.to_string(),
    })
}

/// 클러스터 CA 인증서(PEM). 인증 없이 읽는다. 받는 쪽은 반드시 지문을 확인해야 한다.
#[utoipa::path(get, path = "/cluster/ca", tag = "cluster", security(()),
    responses((status = 200, description = "CA certificate", content_type = "application/x-pem-file", body = String),
              (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn get_ca(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let info = cluster_info(&state)?;
    Ok((
        [(CONTENT_TYPE, "application/x-pem-file")],
        info.spec.ca_cert_pem,
    ))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenRequest {
    /// 유효 시간(초). 기본 1시간, 최대 7일
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedJoinToken {
    /// 새 노드에서 `orbivisorctl cluster join`에 넘길 URL. 다시 볼 수 없다.
    pub url: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

/// 새 토큰과 그 조인 URL(이 노드의 API 주소). 토큰은 한 번만 쓸 수 있다.
pub async fn issue_join_token(
    state: &AppState,
    ttl: Duration,
) -> Result<CreatedJoinToken, ApiError> {
    let info = cluster_info(state)?;
    let me = state
        .cluster
        .get::<NodeSpec>(state.node)?
        .ok_or_else(|| ApiError::internal("this node has no Node resource"))?;
    let secret = random_hex(32);
    let expires_at = now_secs() + ttl.as_secs();
    let mut token: JoinToken = Resource::new(
        "",
        JoinTokenSpec {
            secret_hash: sha256_hex(&secret),
            expires_at,
        },
    );
    token.name = token.id.to_string();
    state.cluster.create(&token).await?;
    let url = JoinUrl {
        address: me.spec.address,
        token: format!("{JOIN_TOKEN_PREFIX}{}_{secret}", token.id.simple()),
        ca_fingerprint: ca_fingerprint(&info)?,
    };
    Ok(CreatedJoinToken {
        url: url.to_string(),
        expires_at,
    })
}

#[utoipa::path(post, path = "/cluster/join-tokens", tag = "cluster",
    request_body = JoinTokenRequest,
    responses((status = 201, body = CreatedJoinToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_join_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<JoinTokenRequest>,
) -> Result<(StatusCode, Json<CreatedJoinToken>), ApiError> {
    state.authorize(&principal, Action::Write, JoinTokenSpec::KIND)?;
    let ttl = req.ttl_secs.map_or(DEFAULT_JOIN_TTL, Duration::from_secs);
    if ttl.is_zero() || ttl > MAX_JOIN_TTL {
        return Err(ApiError::bad_request(
            "ttl_secs must be between 1 and 604800",
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(issue_join_token(&state, ttl).await?),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    /// 조인 URL의 토큰
    pub token: String,
    /// 새 노드의 id(Raft 노드 id, Node 리소스 id)
    pub node: Uuid,
    /// 새 노드의 이름
    pub name: String,
    /// 새 노드의 API 주소(`host:port`)
    pub address: String,
    /// 새 노드의 노드 간 통신 주소(`host:port`)
    pub cluster_address: String,
    /// 새 노드가 만든 키의 CSR(PEM)
    pub csr_pem: String,
    /// 새 노드가 위트니스(투표만 하고 VM을 돌리지 않는 노드)인지
    #[serde(default)]
    pub witness: bool,
    /// 새 노드의 orbivisord 빌드 버전. 옛 버전 노드는 보내지 않는다.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JoinResponse {
    pub cluster: String,
    pub ca_pem: String,
    /// 새 노드의 인증서(PEM)
    pub cert_pem: String,
    /// 투표 멤버의 노드 간 통신 주소(이 노드가 투표 멤버면 이 노드, 아니면 리더나 다른 투표 멤버). 새 노드는
    /// 여기로 승격을 요청한다.
    pub via: String,
}

/// 합류하는 노드가 클러스터의 가장 낮은 노드 버전(위트니스 포함, 버전을 적은 노드만, 합류하는 노드
/// 자신의 옛 기록은 빼고)보다 낮으면 거절한다. 버전을 보내지 않는 옛 노드도 버전을 적은 노드가
/// 있으면 거절한다. 새 노드가 옛 노드가 모르는 상태를 쓰지 않게 한다.
fn check_version(state: &AppState, joining: Uuid, version: Option<&str>) -> Result<(), ApiError> {
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let oldest = nodes
        .iter()
        .filter(|n| n.id != joining)
        .filter_map(|n| n.status.version.as_deref())
        .min_by(|a, b| version::compare(a, b));
    let Some(oldest) = oldest else {
        return Ok(());
    };
    let runs = match version {
        Some(version) if version::compare(version, oldest).is_lt() => version,
        Some(_) => return Ok(()),
        None => "an unknown version",
    };
    Err(ApiError::conflict(
        "version_too_old",
        format!("this node runs {runs}; the cluster runs {oldest} or newer; upgrade it first"),
    ))
}

/// 버전을 적지 않은 옛 노드(위트니스 포함)가 남아 있으면 새 종류의 상태를 쓰지 않고 409
/// `cluster_upgrading`으로 거절한다. 버전을 적지 않는 옛 opld는 새 종류의 상태를 풀지 못한다.
pub(crate) fn require_upgraded(cluster: &opl_cluster::Cluster) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| n.status.version.is_none())
        .map(|n| n.name)
        .collect();
    upgrading(&mut old)
}

/// 이 빌드보다 낮은 노드나 버전을 적지 않은 노드(위트니스 포함)가 남아 있으면 409
/// `cluster_upgrading`으로 거절한다. 설정의 새 필드(구독 등)를 기본값이 아닌 값으로 쓰기 전에 부른다.
/// 옛 노드는 모르는 필드가 든 설정을 풀지 못한다.
pub(crate) fn require_current(cluster: &opl_cluster::Cluster) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| {
            n.status
                .version
                .as_deref()
                .is_none_or(|v| version::compare(v, version::BUILD).is_lt())
        })
        .map(|n| n.name)
        .collect();
    upgrading(&mut old)
}

/// 기능 `feature`([`opl_types::node::FEATURES`])를 적지 않은 노드(위트니스 포함)가 남아 있으면 409
/// `cluster_upgrading`으로 거절한다. 버전을 비교하지 않으므로 롤링 업그레이드 중(버전이 섞여도)
/// 모든 노드가 그 기능을 알면 통과한다.
pub(crate) fn require_feature(
    cluster: &opl_cluster::Cluster,
    feature: &str,
) -> Result<(), ApiError> {
    let old = missing_feature(cluster, feature)?;
    if old.is_empty() {
        return Ok(());
    }
    Err(ApiError::conflict(
        "cluster_upgrading",
        format!(
            "some nodes do not support {} yet ({}); upgrade them first",
            feature.replace('-', " "),
            old.join(", ")
        ),
    ))
}

/// 기능 `feature`를 적지 않은 노드(위트니스 포함)의 이름(이름 순). [`require_feature`]와 같은 판단을
/// Task처럼 API 오류가 아닌 곳에서 쓴다.
pub(crate) fn missing_feature(
    cluster: &opl_cluster::Cluster,
    feature: &str,
) -> Result<Vec<String>, ClusterError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| !n.status.features.iter().any(|f| f == feature))
        .map(|n| n.name)
        .collect();
    old.sort();
    Ok(old)
}

/// 옛 노드 이름들(이름 순)로 `cluster_upgrading`을 만든다. 없으면 통과.
fn upgrading(old: &mut [String]) -> Result<(), ApiError> {
    if old.is_empty() {
        return Ok(());
    }
    old.sort();
    Err(ApiError::conflict(
        "cluster_upgrading",
        format!(
            "some nodes run an older Orbivisor ({}); finish the upgrade on every node first",
            old.join(", ")
        ),
    ))
}

fn invalid_token() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_token",
        "invalid or expired join token",
    )
}

/// 새 노드의 합류 요청. 조인 토큰으로 인증한다(사용자 인증 없음).
#[utoipa::path(post, path = "/cluster/join", tag = "cluster", security(()),
    request_body = JoinRequest,
    responses((status = 200, body = JoinResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn join(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    // 토큰부터 확인해, 토큰 없는 요청에는 클러스터에 대해 아무것도 알려 주지 않는다.
    let (id, secret) = req
        .token
        .strip_prefix(JOIN_TOKEN_PREFIX)
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(id, secret)| Some((Uuid::try_parse(id).ok()?, secret)))
        .ok_or_else(invalid_token)?;
    let token = state
        .cluster
        .get::<JoinTokenSpec>(id)?
        .ok_or_else(invalid_token)?;
    if !constant_time_eq(&sha256_hex(secret), &token.spec.secret_hash)
        || token.spec.expires_at <= now_secs()
    {
        return Err(invalid_token());
    }

    check_version(&state, req.node, req.version.as_deref())?;
    check_name(&req.name)?;
    let (Some(api_host), Some(cluster_host)) = (
        host_of(&req.address).filter(|h| valid_host(h)),
        host_of(&req.cluster_address).filter(|h| valid_host(h)),
    ) else {
        return Err(ApiError::bad_request(
            "address and cluster_address must be host:port with an IP address or a host name",
        ));
    };
    if req.node.is_nil() {
        return Err(ApiError::bad_request("node id must not be nil"));
    }
    // learner이면 합류하다 멈춘 노드다. 같은 이름의 오프라인 노드만 새 토큰으로 다시 합류할 수 있다.
    // 그래야 토큰을 가진 쪽이 승격을 기다리는 살아 있는 노드의 id를 가로채지 못한다.
    let members = state.cluster.members();
    let learner_addr = members.learners.get(&NodeId(req.node)).cloned();
    let rejoin_ok = || -> Result<bool, ApiError> {
        Ok(state
            .cluster
            .get::<NodeSpec>(req.node)?
            .is_some_and(|n| n.name == req.name && !n.status.online))
    };
    if members.voters.contains_key(&NodeId(req.node)) || (learner_addr.is_some() && !rejoin_ok()?) {
        return Err(ApiError::conflict(
            "already_member",
            format!("node {} is already a cluster member", req.node),
        ));
    }
    if let Some(other) = state.cluster.get_by_name::<NodeSpec>(&req.name)?
        && other.id != req.node
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("node name {:?} is already taken", req.name),
        ));
    }
    let info = cluster_info(&state)?;
    let via = via_voter(&state.cluster.members(), NodeId(state.node))
        .ok_or_else(|| ApiError::internal("this node knows no voter"))?;

    let mut sans = vec![api_host.to_string()];
    if cluster_host != api_host {
        sans.push(cluster_host.to_string());
    }
    sans.extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
    let ca = ClusterCa {
        cert_pem: info.spec.ca_cert_pem.clone(),
        key_pem: info.spec.ca_key_pem.clone(),
    };
    let cert_pem = ca
        .sign_node(&req.csr_pem, req.node, &sans)
        .map_err(|e| ApiError::bad_request(format!("cannot sign the CSR: {e}")))?;

    // 토큰을 지운다. 동시에 같은 토큰을 쓴 요청은 하나만 성공한다.
    match state
        .cluster
        .delete::<JoinTokenSpec>(token.id, Some(token.revision))
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(_)) => return Err(invalid_token()),
        Err(e) => return Err(e.into()),
    }

    // 앞서 합류하다 실패한 같은 노드면 Node 리소스를 다시 쓴다.
    // 온라인으로 기록해 첫 핑 전에 노드가 제거되지 않게 한다. 핑이 계속 실패하면 리더가 오프라인으로 바꾼다.
    // 버전은 보낸 값으로 적어, 등록 전에도 버전 검사가 이 노드를 센다.
    let spec = NodeSpec {
        address: req.address.clone(),
        maintenance: false,
        witness: req.witness,
        roles: None,
    };
    match state.cluster.get::<NodeSpec>(req.node)? {
        Some(node) => {
            state
                .cluster
                .update_spec(node.id, None, node.labels, &spec)
                .await?;
            let status = NodeStatus {
                online: true,
                version: req.version.clone(),
                ..node.status
            };
            state
                .cluster
                .update_status::<NodeSpec>(node.id, &status)
                .await?;
        }
        None => {
            let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(req.name.clone(), spec);
            node.id = req.node;
            node.status.online = true;
            node.status.version = req.version.clone();
            state.cluster.create(&node).await?;
        }
    }
    // add_learner는 이미 있는 노드의 주소를 바꾸지 않으므로, 주소가 바뀐 learner는 먼저 뺀다.
    if learner_addr.is_some_and(|addr| addr != req.cluster_address) {
        state
            .cluster
            .request(LeaderRequest::Remove {
                id: NodeId(req.node),
            })
            .await?;
    }
    state
        .cluster
        .request(LeaderRequest::AddLearner {
            id: NodeId(req.node),
            addr: req.cluster_address.clone(),
        })
        .await?;
    tracing::info!(node = %req.name, id = %req.node, "node joined as learner");
    Ok(Json(JoinResponse {
        cluster: info.name,
        ca_pem: info.spec.ca_cert_pem,
        cert_pem,
        via,
    }))
}

/// 새 노드가 승격을 요청할 투표 멤버의 주소: 이 노드가 투표 멤버면 이 노드, 아니면(learner·관찰 노드) 아는 리더,
/// 모르면 아무 투표 멤버
fn via_voter(members: &opl_cluster::Members, me: NodeId) -> Option<String> {
    let leader = members.leader.and_then(|l| members.voters.get(&l));
    members
        .voters
        .get(&me)
        .or(leader)
        .or_else(|| members.voters.values().next())
        .cloned()
}

/// 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `orbivisorctl cluster leave`로 뺀다.
/// 그 노드에 배정된 VM은 그대로 남는다.
#[utoipa::path(delete, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Removed"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.id == state.node || node.status.online {
        return Err(ApiError::conflict(
            "node_online",
            format!(
                "node {:?} is online; run `orbivisorctl cluster leave` on it instead",
                node.name
            ),
        ));
    }
    state
        .cluster
        .request(LeaderRequest::Remove {
            id: NodeId(node.id),
        })
        .await?;
    match state.cluster.delete::<NodeSpec>(node.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => {}
        Err(e) => return Err(e.into()),
    }
    tracing::info!(node = %node.name, "node removed from the cluster");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_via_is_a_voter() {
        let id = |n: u64| NodeId::from(n);
        let addrs = |ids: &[u64]| -> std::collections::BTreeMap<NodeId, String> {
            ids.iter().map(|n| (id(*n), format!("node-{n}"))).collect()
        };
        let members = opl_cluster::Members {
            leader: Some(id(2)),
            voters: addrs(&[1, 2, 3]),
            learners: addrs(&[4]),
            observers: addrs(&[5]),
        };
        assert_eq!(via_voter(&members, id(3)).as_deref(), Some("node-3"));
        assert_eq!(via_voter(&members, id(4)).as_deref(), Some("node-2"));
        assert_eq!(via_voter(&members, id(5)).as_deref(), Some("node-2"));
        let leaderless = opl_cluster::Members {
            leader: None,
            ..members
        };
        assert_eq!(via_voter(&leaderless, id(5)).as_deref(), Some("node-1"));
    }

    #[test]
    fn join_url_round_trips() {
        let url = JoinUrl {
            address: "[::1]:8006".into(),
            token: "oplj_abc_def".into(),
            ca_fingerprint: "sha256:00ff".into(),
        };
        let s = url.to_string();
        assert_eq!(s, "opl-join://[::1]:8006?token=oplj_abc_def&ca=sha256:00ff");
        assert_eq!(s.parse::<JoinUrl>().unwrap(), url);
        for bad in [
            "https://h:1?token=a&ca=sha256:00",
            "opl-join://h?token=a&ca=sha256:00",
            "opl-join://h:1?token=a",
            "opl-join://h:1?token=a&ca=md5:00",
            "opl-join://h:1?token=a&ca=sha256:00&x=1",
        ] {
            assert!(bad.parse::<JoinUrl>().is_err(), "{bad}");
        }
    }

    #[test]
    fn host_of_handles_v6_and_names() {
        assert_eq!(host_of("10.0.0.5:8006"), Some("10.0.0.5"));
        assert_eq!(host_of("[::1]:8006"), Some("::1"));
        assert_eq!(host_of("node1.lan:8007"), Some("node1.lan"));
        assert_eq!(host_of("node1.lan"), None);
        assert_eq!(host_of(":8006"), None);
        assert_eq!(host_of("h:port"), None);
    }

    #[test]
    fn valid_host_accepts_ips_and_host_names_only() {
        for good in ["node1.example", "localhost", "10.0.0.5", "::1", "a-b.c1"] {
            assert!(valid_host(good), "{good}");
        }
        let long_label = format!("{}.example", "a".repeat(64));
        let long_name = vec!["a".repeat(63); 4].join(".");
        for bad in [
            "*.corp.example",
            "evil host/x",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            &long_label,
            &long_name,
            "-a.example",
            "a-.example",
            "node1.example.",
            "a..b",
            "노드.example",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
        assert_eq!(host_of("[::1]:8006").filter(|h| valid_host(h)), Some("::1"));
    }
}
```

`crates/opl-api/src/health.rs`에서

```rust
) -> Result<(StatusCode, Json<Health>), ApiError> {
    let live = q.live.is_some() && q.ready.is_none();
```

을 다음으로 바꾼다:

```rust
) -> Result<(StatusCode, Json<Health>), ApiError> {
    // `Orbivisor-Min-Revision`의 오류(400·503)는 짧은 응답으로 바꾸지 않고 그대로 돌려준다.
    let principal = match principal {
        Err(e) if crate::revision::rejected(&e) => return Err(e),
        principal => principal,
    };
    let live = q.live.is_some() && q.ready.is_none();
```

`crates/opl-api/src/janitor.rs`에서

```rust

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십의 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
```

을 다음으로 바꾼다:

```rust

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십과 관찰 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
```

`crates/opl-api/src/janitor.rs`에서

```rust
        .chain(members.learners.keys())
        .map(|id| id.0)
```

을 다음으로 바꾼다:

```rust
        .chain(members.learners.keys())
        .chain(members.observers.keys())
        .map(|id| id.0)
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod resources;
mod schedule;
```

을 다음으로 바꾼다:

```rust
pub mod resources;
pub mod revision;
mod schedule;
```

`crates/opl-api/src/lib.rs`에서

```rust
#[openapi(
    info(title = "Orbivisor API", description = "Orbivisor REST API (`/api/v1`)"),
    servers((url = "/api/v1")),
```

을 다음으로 바꾼다:

```rust
#[openapi(
    info(
        title = "Orbivisor API",
        description = "Orbivisor REST API (`/api/v1`). Every authenticated response carries `Orbivisor-Revision`, \
            the revision the answering node had applied when it answered. An authenticated request with \
            `Orbivisor-Min-Revision: <revision>` is answered once the node has applied that revision (503 \
            `revision_unavailable` after 5 seconds); pass the `revision` of a resource written elsewhere to read it."
    ),
    servers((url = "/api/v1")),
```

`crates/opl-api/src/lib.rs`에서

```rust
            }),
        );
    // 믿는 프록시의 헤더와 허용 출처(CORS)는 라우트를 찾기 전에 본다(preflight는 라우트가 없다).
```

을 다음으로 바꾼다:

```rust
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            revision::layer,
        ));
    // 믿는 프록시의 헤더와 허용 출처(CORS)는 라우트를 찾기 전에 본다(preflight는 라우트가 없다).
```

`crates/opl-api/src/metrics.rs`에서

```rust
        .chain(&members.learners)
        .filter_map(|(id, addr)| {
```

을 다음으로 바꾼다:

```rust
        .chain(&members.learners)
        .chain(&members.observers)
        .filter_map(|(id, addr)| {
```

`crates/opl-api/src/metrics.rs`에서

```rust
        .chain(&members.learners)
        .find(|(id, _)| id.0 == node.id)
```

을 다음으로 바꾼다:

```rust
        .chain(&members.learners)
        .chain(&members.observers)
        .find(|(id, _)| id.0 == node.id)
```

`crates/opl-api/src/revision.rs`를 만든다:

```rust
//! 리비전 읽기(설계 §6). 인증된 요청에 헤더 `Orbivisor-Min-Revision`이 있으면 이 노드의 저장소가 그 리비전까지
//! 적용하기를 [`MIN_REVISION_WAIT`]까지 기다린 뒤 처리하고, 넘으면 503 `revision_unavailable`이다. 없으면 지금처럼
//! 로컬 상태로 답한다. 인증된 요청의 응답에는 답할 때 이 노드가 적용한 리비전 `Orbivisor-Revision`을 붙인다. 한
//! 노드에서 쓴 리소스의 리비전을 다른 노드(관찰 노드 등)에 넘기면 자기 쓰기를 읽는다.
//!
//! 기다린 뒤에는 다시 인증한다(`Principal` 추출): 다른 노드에서 막 만든 자격 증명(토큰·사용자·비밀번호)도 그
//! 리비전으로 받고, 기다리는 사이 폐기된 것은 받지 않는다. 인증되지 않은 요청은 이 노드가 있다고 아는 리비전
//! ([`opl_cluster::Cluster::known_revision`])일 때만 기다리고 응답에 리비전을 붙이지 않는다: 없는 리비전으로 연결을
//! 붙잡거나 쓰기 활동을 엿보지 못하게.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;
use crate::state::AppState;

/// 요청 헤더: 이 리비전 이후의 상태로 답하라
pub const MIN_REVISION: HeaderName = HeaderName::from_static("orbivisor-min-revision");

/// 응답 헤더: 답한 노드가 적용한 리비전
pub const REVISION: HeaderName = HeaderName::from_static("orbivisor-revision");

/// `Orbivisor-Min-Revision`을 기다리는 최대 시간
pub const MIN_REVISION_WAIT: Duration = Duration::from_secs(5);

/// 잘못된 `Orbivisor-Min-Revision`(400)의 메시지
const BAD_HEADER: &str = "Orbivisor-Min-Revision must be a revision number";

/// 시간 안에 리비전을 따라잡지 못한 503의 코드
const UNAVAILABLE: &str = "revision_unavailable";

/// 요청이 인증됐다는 표시. 층이 요청에 넣고 인증이 세운다.
#[derive(Clone, Default)]
struct Authenticated(Arc<AtomicBool>);

pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let authenticated = Authenticated::default();
    req.extensions_mut().insert(authenticated.clone());
    let mut response = next.run(req).await;
    if authenticated.0.load(Ordering::SeqCst) {
        response
            .headers_mut()
            .insert(REVISION, HeaderValue::from(state.cluster.revision()));
    }
    response
}

/// 인증된 요청: 응답에 리비전을 붙이게 표시한다.
pub(crate) fn authenticated(parts: &Parts) {
    if let Some(mark) = parts.extensions.get::<Authenticated>() {
        mark.0.store(true, Ordering::SeqCst);
    }
}

/// `Orbivisor-Min-Revision`의 리비전을 아직 적용하지 않았으면 그때까지 기다린다. 기다렸으면 `true`다(부른 쪽은 다시
/// 인증한다). 인증된 요청(`authenticated`)은 헤더가 잘못됐거나(400) 시간 안에 따라잡지 못하면(503) 오류다. 인증되지
/// 않은 요청은 이 노드가 있다고 아는 리비전일 때만 기다리고, 그 밖에는 기다리지 않는다(`false`, 인증 실패 그대로).
pub(crate) async fn wait_for(
    state: &AppState,
    parts: &Parts,
    authenticated: bool,
) -> Result<bool, ApiError> {
    let revision = match min_revision(parts) {
        Ok(Some(revision)) => revision,
        Ok(None) => return Ok(false),
        Err(e) if authenticated => return Err(e),
        Err(_) => return Ok(false),
    };
    if revision <= state.cluster.revision()
        || (!authenticated && revision > state.cluster.known_revision())
    {
        return Ok(false);
    }
    match wait(state, revision).await {
        Ok(()) => Ok(true),
        Err(e) if authenticated => Err(e),
        Err(_) => Ok(false),
    }
}

/// `Orbivisor-Min-Revision` 때문에 난 오류(400·503)인지
pub(crate) fn rejected(e: &ApiError) -> bool {
    e.code == UNAVAILABLE || e.message == BAD_HEADER
}

fn min_revision(parts: &Parts) -> Result<Option<u64>, ApiError> {
    let Some(value) = parts.headers.get(&MIN_REVISION) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| ApiError::bad_request(BAD_HEADER))
}

async fn wait(state: &AppState, revision: u64) -> Result<(), ApiError> {
    state
        .cluster
        .wait_revision(revision, MIN_REVISION_WAIT)
        .await
        .map(|_| ())
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                UNAVAILABLE,
                format!(
                    "this node has not applied revision {revision} yet; try again or ask another node"
                ),
            )
        })
}
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert_eq!(admin.get("/nodes/node2").await.0, StatusCode::NOT_FOUND);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(admin.get("/nodes/node2").await.0, StatusCode::NOT_FOUND);
}

/// 모든 응답에 답한 노드의 적용 리비전(`Orbivisor-Revision`)이 붙는다. `Orbivisor-Min-Revision`을 주면 그 리비전까지
/// 적용한 뒤 답하고, 5초 안에 안 되면 503 `revision_unavailable`이다.
#[tokio::test]
async fn reads_wait_for_the_min_revision_header() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let get = |min: Option<String>| {
        let mut r = admin
            .http
            .get(format!("{}/api/v1/volumes", admin.base))
            .bearer_auth(admin.token.as_deref().unwrap());
        if let Some(min) = min {
            r = r.header("Orbivisor-Min-Revision", min);
        }
        r.send()
    };
    let revision = |resp: &reqwest::Response| -> u64 {
        resp.headers()["orbivisor-revision"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    let resp = get(None).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let now = revision(&resp);
    assert!(now >= 1 && now <= env.cluster.revision());

    // 아직 적용하지 않은 리비전: 쓰기가 적용되면 답한다.
    let want = env.cluster.revision() + 1;
    let cluster = env.cluster.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        cluster
            .create(&Resource::new(
                "late",
                opl_types::storage::VolumeSpec {
                    pool: "local".into(),
                    size_bytes: 1 << 20,
                    format: opl_types::runtime::ImageFormat::Qcow2,
                },
            ))
            .await
            .unwrap()
    });
    let resp = get(Some(want.to_string())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(revision(&resp) >= want);
    writer.await.unwrap();

    let resp = get(Some("soon".into())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let far = env.cluster.revision() + 1_000_000;
    let started = std::time::Instant::now();
    let resp = get(Some(far.to_string())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() >= std::time::Duration::from_secs(4));
    assert!(revision(&resp) < far);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "revision_unavailable", "{body}");
    // 헬스도 헤더의 오류를 짧은 응답으로 바꾸지 않고 그대로 돌려준다.
    let resp = admin
        .http
        .get(format!("{}/api/v1/health", admin.base))
        .bearer_auth(admin.token.as_deref().unwrap())
        .header("Orbivisor-Min-Revision", "soon")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 인증되지 않은 요청은 기다리지 않고(곧바로 401) 리비전도 알려 주지 않는다.
    let started = std::time::Instant::now();
    let resp = admin
        .http
        .get(format!("{}/api/v1/volumes", admin.base))
        .header("Orbivisor-Min-Revision", far.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(resp.headers().get("orbivisor-revision").is_none());
    let resp = admin
        .http
        .get(format!("{}/api/v1/openapi.json", admin.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("orbivisor-revision").is_none());
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    pub state: Arc<AppState>,
    reconciler: Option<Reconciler>,
```

을 다음으로 바꾼다:

```rust
    pub state: Arc<AppState>,
    /// 클러스터의 시험망. 노드(관찰 노드 등)를 더할 때 쓴다.
    pub router: MemRouter,
    reconciler: Option<Reconciler>,
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            state,
            reconciler: Some(reconciler),
```

을 다음으로 바꾼다:

```rust
            state,
            router,
            reconciler: Some(reconciler),
```

`crates/opl-api/tests/health.rs`에서

```rust
use common::{Client, Env, WAIT};
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::ClusterError;
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::node::{NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust
use common::{Client, Env, WAIT};
use openraft::BasicNode;
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_reconciler::ha::LeaseRenewer;
use opl_reconciler::{HaConfig, HaGuard};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
```

`crates/opl-api/tests/health.rs`에서

```rust
        StatusCode::FORBIDDEN
    );
}
```

을 다음으로 바꾼다:

```rust
        StatusCode::FORBIDDEN
    );
}

/// 관찰 노드를 더한다. 실제 길처럼 learner로 가입해 따라잡은 뒤 클러스터 정보의 관찰 노드 목록에 적히고 멤버십에서
/// 빠진다. 같은 저장소로 관찰 노드로 다시 열려 투표 멤버(`env`의 노드)를 따르고, Node 리소스(`api` 역할)가 있다.
async fn start_observer(env: &Env) -> Arc<Cluster> {
    let id = NodeId(Uuid::now_v7());
    let addr = format!("observer-{id}");
    let mut config = ClusterConfig::new(id, env.dir.path().join("observer"));
    config.raft = openraft::Config {
        heartbeat_interval: 50,
        election_timeout_min: 300,
        election_timeout_max: 600,
        ..Default::default()
    };
    let learner = Cluster::open(config.clone(), env.router.network(id))
        .await
        .unwrap();
    env.router.register(id, &learner);
    env.cluster
        .add_learner(id, BasicNode::new(&addr))
        .await
        .unwrap();
    let mut info = env.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
    info.status.observers.insert(id.0, addr);
    env.cluster
        .update_status::<ClusterSpec>(info.id, &info.status)
        .await
        .unwrap();
    env.cluster
        .request(LeaderRequest::Remove { id })
        .await
        .unwrap();
    env.router.unregister(id);
    learner.shutdown().await.unwrap();
    drop(learner);
    let deadline = std::time::Instant::now() + WAIT;
    let observer = loop {
        match Cluster::open_observer(config.clone(), env.router.follower(id)).await {
            Err(ClusterError::Store(_)) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => break other.unwrap(),
        }
    };
    let observer = Arc::new(observer.with_forwarder(env.router.forwarder(id)));
    env.router.register(id, &observer);
    let mut node = Resource::new(
        "observer",
        NodeSpec {
            address: "127.0.0.1:0".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    node.id = id.0;
    let revision = env.cluster.create(&node).await.unwrap();
    observer.wait_revision(revision, WAIT).await.unwrap();
    observer
}

/// `cluster`(다른 노드)로 API를 따로 서비스한다(관리자 토큰을 쓰는 클라이언트).
async fn serve_node(env: &Env, cluster: Arc<Cluster>) -> Client {
    let node = cluster.id().0;
    let state = AppState::new(
        cluster,
        node,
        env.hv.clone(),
        env.dir.path().join(format!("uploads-{node}")),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(Arc::new(state));
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Client {
        base,
        http: reqwest::Client::new(),
        token: env.admin().await.token,
    }
}

/// 관찰 노드는 붙은 투표 멤버의 진행이 5초 안에 오고 리더를 알면 준비됐고(200), 끊기면 `quorum`으로 503이다.
/// 관찰 노드도 클러스터 멤버로(`role: observer`) 보인다.
#[tokio::test]
async fn an_observer_is_ready_while_it_follows_a_voter() {
    let env = Env::start().await;
    let observer = start_observer(&env).await;
    let id = observer.id();
    let client = serve_node(&env, observer.clone()).await;
    // 관리자 토큰이 관찰 노드에 복제될 때까지
    observer
        .wait_revision(env.cluster.revision(), WAIT)
        .await
        .unwrap();
    let (status, body) = client.get("/health").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["node"], id.0.to_string());
    assert_eq!(body["leader"], env.node.to_string());
    assert_eq!(body["quorum"], true);

    for c in [&client, &env.admin().await] {
        let (status, view) = c.get("/cluster").await;
        assert_eq!(status, StatusCode::OK, "{view}");
        let me = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.0.to_string())
            .unwrap_or_else(|| panic!("the observer is not listed: {view}"))
            .clone();
        assert_eq!(me["role"], "observer", "{view}");
        assert_eq!(me["voter"], false);
        assert_eq!(me["name"], "observer");
        assert_eq!(me["cluster_address"], format!("observer-{id}"));
        assert_eq!(view["members"][0]["role"], "voter", "{view}");
    }

    env.router.isolate(id);
    let anon = Client {
        token: None,
        ..client
    };
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let (status, body) = anon.get("/health").await;
        if status == StatusCode::SERVICE_UNAVAILABLE {
            assert_eq!(failed(&body), ["quorum"], "{body}");
            break;
        }
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(std::time::Instant::now() < deadline, "still ready: {body}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    observer.shutdown().await.unwrap();
}

/// `Orbivisor-Min-Revision`은 다른 노드에서 만든 자격 증명에도 쓰인다: 이 노드가 아직 적용하지 않은 토큰도 그
/// 리비전을 주면 적용한 뒤 다시 인증해 받는다(관찰 노드는 리더가 돌려준 인덱스로 그 리비전이 있음을 안다). 기다리는
/// 사이 지운 토큰은 받지 않는다. 이 노드가 있다고 모르는 리비전이면 인증되지 않은 요청은 기다리지 않는다.
#[tokio::test]
async fn min_revision_covers_credentials_made_on_another_node() {
    let env = Env::start().await;
    let observer = start_observer(&env).await;
    let client = serve_node(&env, observer.clone()).await;
    let doomed = env.admin().await.token.unwrap();
    observer
        .wait_revision(env.cluster.revision(), WAIT)
        .await
        .unwrap();
    let get = |token: &str, min: Option<u64>| {
        let mut r = client
            .http
            .get(format!("{}/api/v1/volumes", client.base))
            .bearer_auth(token);
        if let Some(min) = min {
            r = r.header("Orbivisor-Min-Revision", min.to_string());
        }
        r.send()
    };
    assert_eq!(get(&doomed, None).await.unwrap().status(), StatusCode::OK);

    // 관찰 노드가 프레임을 받지 못하는 동안 리더에서 토큰 하나를 만들고 하나를 지운다.
    env.router.pause_follow(observer.id());
    let fresh = env.admin().await;
    let doomed_id = doomed
        .strip_prefix("opl_")
        .and_then(|t| t.split('_').next())
        .unwrap();
    let (status, body) = fresh.delete(&format!("/tokens/{doomed_id}")).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let fresh = fresh.token.unwrap();
    let revision = env.cluster.revision();
    assert_eq!(
        get(&fresh, None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(get(&doomed, None).await.unwrap().status(), StatusCode::OK);
    // 관찰 노드가 그 리비전을 모르면 인증되지 않은 요청은 기다리지 않는다.
    let started = std::time::Instant::now();
    let resp = get(&fresh, Some(revision)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(started.elapsed() < Duration::from_secs(2));

    // 리스 갱신이 리더의 인덱스를 알려 관찰 노드가 그 리비전이 있음을 안다.
    let renewal = {
        let observer = observer.clone();
        tokio::spawn(async move { observer.renew_lease(WAIT).await })
    };
    let deadline = std::time::Instant::now() + WAIT;
    while observer.known_revision() < revision {
        assert!(
            std::time::Instant::now() < deadline,
            "the revision stayed unknown"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(observer.revision() < revision);
    let resume = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        env.router.heal(observer.id());
    };
    let (made, deleted, ()) = tokio::join!(
        get(&fresh, Some(revision)),
        get(&doomed, Some(revision)),
        resume
    );
    assert_eq!(made.unwrap().status(), StatusCode::OK);
    assert_eq!(deleted.unwrap().status(), StatusCode::UNAUTHORIZED);
    renewal.await.unwrap().unwrap();
    observer.shutdown().await.unwrap();
}
```

`crates/opl-cluster/src/cluster.rs` 전체:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use futures_util::future::Either;
use futures_util::stream::BoxStream;
use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::follow::{self, FollowClient, FollowFrame};
use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::replica::Replica;
use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
use crate::types::{Command, LeaderRequest, NodeId, RawResource, Rejection, TypeConfig};
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
    /// 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`). `Kind::Cluster` 변경으로 갱신한다.
    observers: Arc<RwLock<BTreeMap<NodeId, String>>>,
}

/// 이 노드가 클러스터에 무엇으로 참여하는지
#[derive(Clone)]
pub(crate) enum Backend {
    /// Raft 구성원(투표 멤버·learner). `confirm`은 두 리스 표가 함께 쓰는 리더 확인이다.
    Member {
        raft: Raft<TypeConfig>,
        confirm: ConfirmBatch,
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
            Backend::Member { raft, confirm } => Some(LeaderCtx {
                raft,
                confirm,
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
    pub async fn open<N>(config: ClusterConfig, network: N) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig>,
    {
        let store = open_store(&config)?;
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
        let backend = Backend::Member {
            confirm: ConfirmBatch::for_raft(raft.clone()),
            raft,
        };
        Ok(Self::assemble(
            config.node_id,
            config.write_timeout,
            store,
            backend,
        ))
    }

    /// 저장소를 열고 관찰 노드로 시작한다(Raft를 띄우지 않음). 저장소의 멤버십에서 투표 멤버를 골라 `follow`로
    /// 붙고, 저장소의 적용 위치부터 이어 받는다. 쓰기와 리스 갱신은 `with_forwarder`의 전달자로 리더에 보낸다.
    pub async fn open_observer(
        config: ClusterConfig,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, ClusterError> {
        let store = open_store(&config)?;
        let replica = Replica::start(config.node_id, &store, follow)?;
        Ok(Self::assemble(
            config.node_id,
            config.write_timeout,
            store,
            Backend::Observer(replica),
        ))
    }

    fn assemble(id: NodeId, write_timeout: Duration, store: Store, backend: Backend) -> Self {
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
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), ClusterError> {
        self.member()?
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
            list.insert(*id, node.addr.clone());
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

    pub async fn shutdown(&self) -> Result<(), ClusterError> {
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
    /// 관찰 노드는 `shutdown`하면 끝난다.
    pub fn stopped(&self) -> impl Future<Output = String> + Send + 'static {
        let mut rx = match self.node.backend() {
            Backend::Member { raft, .. } => raft.metrics(),
            Backend::Observer(replica) => {
                let mut stopped = replica.stopped();
                return Either::Right(async move {
                    let _ = stopped.wait_for(|s| *s).await;
                    "the replica stopped".to_owned()
                });
            }
        };
        Either::Left(async move {
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
```

`crates/opl-cluster/src/follow.rs` 전체:

```rust
//! 관찰 노드가 투표 멤버 하나에서 받는 스트림(설계 §6): 스냅샷(처음·뒤처졌을 때) + 적용 묶음 + 1초마다 진행.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use openraft::{BasicNode, LogId, RaftMetrics, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::cluster::{Backend, Cluster, ClusterError, NodeHandle, observers_of};
use crate::state_machine::snapshot_objects;
use crate::types::{NodeId, RawResource};
use crate::watch::{Feed, FeedBatch, FeedItem, WatchError, WatchEvent};

/// 서버가 이만큼마다 `Progress`를 보낸다. 관찰 노드는 이것으로 연결이 살아 있는지 안다.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// 스냅샷 조각 하나에 담는 리소스 수
const SNAPSHOT_PART_OBJECTS: usize = 256;

/// 스냅샷 조각 하나에 담는 리소스의 JSON 크기(이를 넘기 전에 자른다. 리소스 하나가 이보다 크면 그것만 담는다).
/// 조각은 gRPC 메시지 하나로 가므로 노드 간 메시지 한도(opl-peer `MAX_MESSAGE_BYTES`, 64 MiB)보다 넉넉히 작게 둔다.
/// 큰 적용 묶음도 이 크기로 나눠 보낸다(`BatchPart`).
const SNAPSHOT_PART_BYTES: usize = 4 << 20;

/// 서버가 보내고 아직 읽히지 않은 프레임 수. 이만큼 쌓이면 서버는 보내기를 멈추고, 그동안 서버의 피드가 기록
/// (`HISTORY` 묶음)보다 뒤처지면 `Lagged`로 끝난다(받는 쪽은 다시 붙는다).
const FRAME_BUFFER: usize = 16;

/// 노드가 클러스터에 무엇으로 참여하는지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    Voter,
    Learner,
    /// Raft 구성원이 아니다(관찰 노드).
    Observer,
}

/// Follow 스트림의 프레임
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "kebab-case")]
pub enum FollowFrame {
    /// 처음 프레임. `role`은 서버가 보는 부른 노드의 역할이다. 서버가 Raft 구성원이 아니면 이것만 보내고
    /// 닫는다(다른 투표 멤버로 가라는 뜻).
    Hello {
        role: NodeRole,
        leader: Option<(NodeId, String)>,
        voters: BTreeMap<NodeId, String>,
    },
    /// 스냅샷의 리소스 일부. `SnapshotEnd`까지 모아 한 번에 설치한다.
    SnapshotPart { objects: Vec<(Kind, RawResource)> },
    /// 스냅샷 끝. 상태 전체를 앞 조각들로 바꾼다(구독자에게는 `Reset`).
    SnapshotEnd {
        applied: Option<LogId<NodeId>>,
        membership: StoredMembership<NodeId, BasicNode>,
    },
    /// 큰 묶음의 앞부분 이벤트(메시지 한도 때문에 나눔). 뒤따르는 `Batch`와 함께 한 번에 적용한다.
    BatchPart { events: Vec<WatchEvent> },
    /// 서버가 적용한 묶음([`FeedBatch`])(앞서 온 `BatchPart`의 이벤트가 그 앞에 붙는다). 이벤트가 없는 묶음도 적용
    /// 위치로 온다.
    Batch {
        events: Vec<WatchEvent>,
        applied: LogId<NodeId>,
        membership: Option<StoredMembership<NodeId, BasicNode>>,
    },
    /// 서버가 적용한 위치와 서버가 아는 리더
    Progress {
        applied: Option<LogId<NodeId>>,
        leader: Option<NodeId>,
        leader_addr: Option<String>,
    },
}

impl FollowFrame {
    /// 묶음 하나를 보낼 프레임들: 이벤트의 JSON이 `max_bytes`를 넘으면 앞부분을 `BatchPart`로 나누고 마지막 `Batch`가
    /// 적용 위치와 멤버십을 싣는다.
    fn batch(b: &FeedBatch, max_bytes: usize) -> Vec<Self> {
        let mut events = b.events.clone().into_iter().peekable();
        let mut frames = Vec::new();
        loop {
            let part = take_part(&mut events, usize::MAX, max_bytes);
            if events.peek().is_none() {
                frames.push(FollowFrame::Batch {
                    events: part,
                    applied: b.applied,
                    membership: b.membership.clone(),
                });
                return frames;
            }
            frames.push(FollowFrame::BatchPart { events: part });
        }
    }

    /// 이 프레임이 바꾼 클러스터 정보의 관찰 노드 목록(프레임 안의 마지막 것). 클러스터 정보를 싣지 않으면 `None`이다.
    /// 지운 관찰 노드의 열린 스트림이 목록에서 빠지는 프레임부터 받지 않게 한다(목록 캐시의 갱신을 기다리지 않고).
    pub fn observers(&self) -> Option<BTreeMap<NodeId, String>> {
        let events = match self {
            FollowFrame::Batch { events, .. } | FollowFrame::BatchPart { events } => events,
            FollowFrame::SnapshotPart { objects } => {
                return objects
                    .iter()
                    .rev()
                    .find(|(kind, _)| *kind == Kind::Cluster)
                    .map(|(_, info)| observers_of(info));
            }
            _ => return None,
        };
        events.iter().rev().find_map(|e| match e {
            WatchEvent::Put {
                kind: Kind::Cluster,
                resource,
            } => Some(observers_of(resource)),
            WatchEvent::Deleted {
                kind: Kind::Cluster,
                ..
            } => Some(BTreeMap::new()),
            _ => None,
        })
    }
}

/// 열린 Follow 스트림을 프레임마다 다시 허가한다(노드 간 네트워크가 쓴다). 부른 쪽이 이 노드의 Raft 멤버십이나
/// 관찰 노드 목록에 있어야 한다. 목록은 이 스트림의 프레임에 실린 것을 먼저 본다(이 노드의 목록 캐시는 따로 갱신되어
/// 늦을 수 있다): 목록에서 부른 쪽을 빼는 프레임부터 허가하지 않는다(지운 관찰 노드가 그 뒤의 쓰기를 받지 않게).
pub struct FollowGate {
    caller: NodeId,
    /// 이 스트림이 마지막으로 실은 관찰 노드 목록에 부른 쪽이 있는지(아직 싣지 않았으면 `None`)
    listed: Option<bool>,
}

impl FollowGate {
    pub fn new(caller: NodeId) -> Self {
        Self {
            caller,
            listed: None,
        }
    }

    /// `frame`을 부른 쪽에 보내도 되는지
    pub fn admits(&mut self, cluster: &Cluster, frame: &FollowFrame) -> bool {
        if let Some(observers) = frame.observers() {
            self.listed = Some(observers.contains_key(&self.caller));
        }
        cluster.in_membership(self.caller)
            || self
                .listed
                .unwrap_or_else(|| cluster.knows_node(self.caller))
    }
}

/// 마지막으로 프레임으로 나눈 묶음과 그 프레임. 관찰 노드마다의 스트림이 같은 묶음(`Arc`)을 받으므로 한 번만
/// 나눈다(이벤트를 복제하고 JSON 크기를 재는 일을 관찰 노드마다 하지 않게).
#[derive(Default)]
pub(crate) struct BatchFrames(Mutex<Option<SplitBatch>>);

/// 나눈 묶음과 그 프레임
type SplitBatch = (Weak<FeedBatch>, Arc<[FollowFrame]>);

impl BatchFrames {
    fn get(&self, batch: &Arc<FeedBatch>) -> Arc<[FollowFrame]> {
        let mut last = self.0.lock().unwrap();
        // `Weak`을 쥐는 동안 그 자리는 다른 묶음에 쓰이지 않으므로 주소로 같은 묶음인지 안다.
        if let Some((seen, frames)) = &*last
            && std::ptr::eq(seen.as_ptr(), Arc::as_ptr(batch))
        {
            return frames.clone();
        }
        let frames: Arc<[FollowFrame]> = FollowFrame::batch(batch, SNAPSHOT_PART_BYTES).into();
        *last = Some((Arc::downgrade(batch), frames.clone()));
        frames
    }
}

/// 관찰 노드가 투표 멤버에 Follow를 거는 통로. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait FollowClient: Send + Sync + 'static {
    /// `addr`(클러스터 주소)의 노드 `to`에서 `since` 뒤의 상태를 받는다. 스트림의 `Err`나 끝은 연결이 끊긴 것이다.
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError>;
}

/// 노드 `caller`에게 보낼 Follow 스트림. 프레임은 따로 띄운 작업이 만들고, 스트림을 버리면 그 작업도 끝난다.
pub(crate) fn serve(
    node: NodeHandle,
    caller: NodeId,
    since: u64,
) -> Result<BoxStream<'static, FollowFrame>, ClusterError> {
    let (hello, member) = hello(&node, caller)?;
    let (tx, rx) = mpsc::channel(FRAME_BUFFER);
    // 부른 쪽이 이 노드보다 앞섰다(다른 투표 멤버에서 옮겨 왔다). 받으면 이 노드가 그 사이 스냅샷을 설치할 때 부른
    // 쪽이 뒤로 가므로 `Hello`만 보내고 닫아 다른 투표 멤버로 가게 한다.
    let ahead = since > node.hub().revision();
    tokio::spawn(async move {
        if tx.send(hello).await.is_err() || !member {
            return;
        }
        if ahead {
            tracing::debug!(%caller, since, "the follower is ahead of this node; turning it away");
            return;
        }
        if let Err(why) = stream_changes(&node, since, &tx).await {
            tracing::debug!(%caller, %why, "follow stream ended");
        }
    });
    Ok(Box::pin(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (frame, rx))
    })))
}

/// 처음 프레임과, 이 노드가 Raft 구성원(스트림을 이어 보냄)인지
fn hello(node: &NodeHandle, caller: NodeId) -> Result<(FollowFrame, bool), ClusterError> {
    let (membership, leader, member) = match node.backend() {
        Backend::Member { raft, .. } => {
            let m = raft.metrics().borrow().clone();
            let membership = m.membership_config.membership().clone();
            let leader = m
                .current_leader
                .and_then(|l| membership.get_node(&l).map(|n| (l, n.addr.clone())));
            let member = membership.get_node(&node.id()).is_some();
            (membership, leader, member)
        }
        Backend::Observer(replica) => (
            replica.membership()?.membership().clone(),
            replica.leader(),
            false,
        ),
    };
    let voters: BTreeMap<NodeId, String> = membership
        .voter_ids()
        .filter_map(|id| membership.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect();
    let role = if voters.contains_key(&caller) {
        NodeRole::Voter
    } else if membership.get_node(&caller).is_some() {
        NodeRole::Learner
    } else {
        NodeRole::Observer
    };
    Ok((
        FollowFrame::Hello {
            role,
            leader,
            voters,
        },
        member,
    ))
}

/// `since` 뒤의 묶음을 보낸다(기록보다 오래면 스냅샷부터). 받는 쪽이 끊거나, 피드가 끊기거나, 이 노드의 Raft가
/// 멈추거나 구성원에서 빠지면 끝난다.
async fn stream_changes(
    node: &NodeHandle,
    since: u64,
    tx: &mpsc::Sender<FollowFrame>,
) -> Result<(), String> {
    let Some(raft) = node.raft() else {
        return Err("not a Raft member".into());
    };
    let mut metrics = raft.metrics();
    let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
    let mut feed = match node.hub().feed(since) {
        Ok(feed) => feed,
        Err(WatchError::Expired { .. }) => send_snapshot(node, tx, &mut tick, &mut metrics).await?,
        Err(e) => return Err(e.to_string()),
    };
    loop {
        let frame = tokio::select! {
            item = feed.next() => match item.map_err(|e| e.to_string())? {
                FeedItem::Batch(b) => {
                    for frame in node.batch_frames().get(&b).iter() {
                        tx.send(frame.clone())
                            .await
                            .map_err(|_| "the follower went away".to_string())?;
                    }
                    continue;
                }
                // 이 노드가 스냅샷을 설치해 상태가 통째로 바뀌었다. 받는 쪽에도 스냅샷을 보낸다.
                FeedItem::Reset { .. } => {
                    feed = send_snapshot(node, tx, &mut tick, &mut metrics).await?;
                    continue;
                }
            },
            _ = tick.tick() => progress(node, &mut metrics)?,
        };
        tx.send(frame)
            .await
            .map_err(|_| "the follower went away".to_string())?;
    }
}

type Metrics = tokio::sync::watch::Receiver<RaftMetrics<NodeId, BasicNode>>;

/// 이 노드가 적용한 위치와 아는 리더. 이 노드의 Raft가 멈췄거나 구성원에서 빠졌으면 오류다.
fn progress(node: &NodeHandle, metrics: &mut Metrics) -> Result<FollowFrame, String> {
    if metrics.has_changed().is_err() {
        return Err("the Raft task ended".into());
    }
    let m = metrics.borrow_and_update().clone();
    if let Err(e) = &m.running_state {
        return Err(e.to_string());
    }
    let membership = m.membership_config.membership();
    if membership.get_node(&node.id()).is_none() {
        return Err("no longer a Raft member".into());
    }
    Ok(FollowFrame::Progress {
        applied: m.last_applied,
        leader: m.current_leader,
        leader_addr: m
            .current_leader
            .and_then(|l| membership.get_node(&l).map(|n| n.addr.clone())),
    })
}

/// 지금 상태를 스냅샷으로 보내고, 그 리비전 뒤의 피드를 돌려준다. 스냅샷을 뜨는 동안에도 `tick`마다 `Progress`를
/// 보낸다(큰 상태를 읽는 동안 받는 쪽이 진행이 없다고 끊지 않게).
async fn send_snapshot(
    node: &NodeHandle,
    tx: &mpsc::Sender<FollowFrame>,
    tick: &mut tokio::time::Interval,
    metrics: &mut Metrics,
) -> Result<Feed, String> {
    let gone = |_| "the follower went away".to_string();
    let (applied, membership, objects, feed) = loop {
        let db = node.db().clone();
        let mut read = tokio::task::spawn_blocking(move || snapshot_objects(&db));
        let read = loop {
            tokio::select! {
                read = &mut read => break read,
                _ = tick.tick() => tx.send(progress(node, metrics)?).await.map_err(gone)?,
            }
        };
        let (applied, membership, objects) = read
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        // 읽은 뒤 기록이 그 리비전을 지나쳤으면(아주 드묾) 다시 뜬다.
        match node.hub().feed(applied.map_or(0, |l| l.index)) {
            Ok(feed) => break (applied, membership, objects, feed),
            Err(WatchError::Expired { .. }) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut objects = objects.into_iter().peekable();
    while objects.peek().is_some() {
        let part = take_part(&mut objects, SNAPSHOT_PART_OBJECTS, SNAPSHOT_PART_BYTES);
        tx.send(FollowFrame::SnapshotPart { objects: part })
            .await
            .map_err(gone)?;
    }
    tx.send(FollowFrame::SnapshotEnd {
        applied,
        membership,
    })
    .await
    .map_err(gone)?;
    Ok(feed)
}

/// 다음 조각(스냅샷의 리소스나 묶음의 이벤트): `max_objects`개까지, JSON 크기가 `max_bytes`를 넘기 전까지(적어도
/// 하나, 남은 것이 없으면 빈 조각)
fn take_part<T: Serialize>(
    objects: &mut std::iter::Peekable<std::vec::IntoIter<T>>,
    max_objects: usize,
    max_bytes: usize,
) -> Vec<T> {
    let mut part = Vec::new();
    let mut bytes = 0;
    while part.len() < max_objects {
        let Some(next) = objects.peek() else { break };
        let size = json_len(next);
        if !part.is_empty() && bytes + size > max_bytes {
            break;
        }
        bytes += size;
        part.extend(objects.next());
    }
    part
}

/// 값을 JSON으로 쓴 길이(쓰지 않고 센다)
fn json_len(value: &impl Serialize) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // 리소스는 이미 JSON에서 읽은 값이라 다시 쓰는 데 실패하지 않는다.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

#[cfg(test)]
mod tests {
    use openraft::CommittedLeaderId;
    use opl_types::resource::Resource;
    use serde_json::json;

    use super::*;
    use crate::state_machine::Objects;

    /// 큰 묶음은 이벤트를 `BatchPart`로 나눠 보내고 마지막 `Batch`만 적용 위치와 멤버십을 싣는다. 작은 묶음은 그대로다.
    #[test]
    fn large_batches_are_split_and_the_last_frame_carries_the_applied_index() {
        let applied = LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), 9);
        let event = |i: u64| {
            let mut resource: RawResource = Resource::new("a", json!({ "x": "y".repeat(1000) }));
            resource.revision = i;
            WatchEvent::Put {
                kind: Kind::Vm,
                resource,
            }
        };
        let batch = FeedBatch {
            events: (1..=5).map(event).collect(),
            applied,
            membership: Some(StoredMembership::default()),
        };
        let frames = FollowFrame::batch(&batch, 2500);
        let shape: Vec<(bool, Vec<u64>)> = frames
            .iter()
            .map(|f| match f {
                FollowFrame::BatchPart { events } => {
                    (false, events.iter().map(WatchEvent::revision).collect())
                }
                FollowFrame::Batch { events, .. } => {
                    (true, events.iter().map(WatchEvent::revision).collect())
                }
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            shape,
            [(false, vec![1, 2]), (false, vec![3, 4]), (true, vec![5])]
        );
        assert!(matches!(
            frames.last(),
            Some(FollowFrame::Batch { applied: a, membership: Some(_), .. }) if *a == applied
        ));
        assert_eq!(
            FollowFrame::batch(&batch, usize::MAX),
            [FollowFrame::Batch {
                events: batch.events.clone(),
                applied,
                membership: batch.membership.clone(),
            }]
        );
        // 이벤트 없는 묶음도 `Batch` 하나다.
        let empty = FeedBatch {
            events: vec![],
            applied,
            membership: None,
        };
        assert!(matches!(
            FollowFrame::batch(&empty, 10).as_slice(),
            [FollowFrame::Batch { events, .. }] if events.is_empty()
        ));
    }

    /// 같은 묶음(`Arc`)은 스트림이 몇이든 한 번만 프레임으로 나눈다. 다른 묶음이 오면 다시 나눈다.
    #[test]
    fn batch_frames_are_made_once_per_batch() {
        let applied = |i| LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), i);
        let batch = |i| {
            Arc::new(FeedBatch {
                events: vec![],
                applied: applied(i),
                membership: None,
            })
        };
        let cache = BatchFrames::default();
        let (first, second) = (batch(1), batch(2));
        let made = cache.get(&first);
        assert!(Arc::ptr_eq(&made, &cache.get(&first)));
        let other = cache.get(&second);
        assert!(!Arc::ptr_eq(&made, &other));
        assert!(matches!(&other[..], [FollowFrame::Batch { applied: a, .. }] if *a == applied(2)));
    }

    /// 클러스터 정보를 바꾸는 프레임은 그 관찰 노드 목록을 알린다(지운 관찰 노드의 스트림을 그 프레임에서 끊는다).
    #[test]
    fn frames_tell_the_observers_they_list() {
        let applied = LogId::new(CommittedLeaderId::new(1, NodeId::from(1)), 9);
        let observer = NodeId::from(7);
        let info = |observers: &[NodeId]| -> RawResource {
            let mut r: RawResource = Resource::new("c", json!({}));
            r.status = serde_json::to_value(opl_types::cluster::ClusterStatus {
                observers: observers
                    .iter()
                    .map(|id| (id.0, format!("node-{id}")))
                    .collect(),
            })
            .unwrap();
            r
        };
        let put = |r: RawResource| WatchEvent::Put {
            kind: Kind::Cluster,
            resource: r,
        };
        let vm = WatchEvent::Put {
            kind: Kind::Vm,
            resource: Resource::new("a", json!({})),
        };
        let batch = |events| FollowFrame::Batch {
            events,
            applied,
            membership: None,
        };
        assert_eq!(batch(vec![vm.clone()]).observers(), None);
        // 마지막 클러스터 정보가 이긴다.
        let listed = batch(vec![put(info(&[])), vm.clone(), put(info(&[observer]))]);
        assert!(listed.observers().unwrap().contains_key(&observer));
        let unlisted = FollowFrame::BatchPart {
            events: vec![put(info(&[observer])), put(info(&[]))],
        };
        assert_eq!(unlisted.observers(), Some(BTreeMap::new()));
        let snapshot = FollowFrame::SnapshotPart {
            objects: vec![(Kind::Cluster, info(&[observer]))],
        };
        assert!(snapshot.observers().unwrap().contains_key(&observer));
        let progress = FollowFrame::Progress {
            applied: None,
            leader: None,
            leader_addr: None,
        };
        assert_eq!(progress.observers(), None);
    }

    /// 열린 스트림의 허가는 목록 캐시보다 스트림이 실은 목록을 먼저 본다: 캐시가 아직 관찰 노드를 적고 있어도
    /// 그것을 목록에서 빼는 프레임부터 막는다. Raft 멤버는 목록과 관계없이 받는다.
    #[tokio::test]
    async fn the_gate_closes_at_the_frame_that_unlists_the_caller() {
        use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};

        let router = crate::memnet::MemRouter::default();
        let id = NodeId::from(1u64);
        let mut config = crate::cluster::ClusterConfig::new(id, "unused");
        config.in_memory = true;
        let c = Cluster::open(config, router.network(id)).await.unwrap();
        router.register(id, &c);
        c.initialize(BTreeMap::from([(id, BasicNode::new("node-1"))]))
            .await
            .unwrap();
        let observer = NodeId::from(9u64);
        let status = |observers: &[NodeId]| ClusterStatus {
            observers: observers
                .iter()
                .map(|id| (id.0, format!("node-{id}")))
                .collect(),
        };
        let mut info: ClusterInfo = Resource::new(
            "test",
            ClusterSpec {
                ca_cert_pem: String::new(),
                ca_key_pem: String::new(),
            },
        );
        info.status = status(&[observer]);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match c.create(&info).await {
                Ok(_) => break,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e}"),
            }
        }
        while !c.knows_node(observer) {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let progress = FollowFrame::Progress {
            applied: None,
            leader: None,
            leader_addr: None,
        };
        let mut unlisted: RawResource =
            serde_json::from_value(serde_json::to_value(&info).unwrap()).unwrap();
        unlisted.status = serde_json::to_value(status(&[])).unwrap();
        let removal = FollowFrame::Batch {
            events: vec![WatchEvent::Put {
                kind: Kind::Cluster,
                resource: unlisted,
            }],
            applied: LogId::new(CommittedLeaderId::new(1, id), 99),
            membership: None,
        };

        let mut gate = FollowGate::new(observer);
        assert!(gate.admits(&c, &progress));
        // 캐시는 아직 관찰 노드를 적고 있다(이 프레임은 이 노드에 적용되지 않았다).
        assert!(!gate.admits(&c, &removal));
        assert!(c.knows_node(observer));
        assert!(!gate.admits(&c, &progress));
        // Raft 멤버는 목록에 없어도 받는다.
        let mut member = FollowGate::new(id);
        assert!(member.admits(&c, &removal));
        c.shutdown().await.unwrap();
    }

    /// 스냅샷 조각은 리소스 수와 JSON 크기로 자른다(gRPC 메시지 한도). 한도보다 큰 리소스도 혼자 간다.
    #[test]
    fn snapshot_parts_are_cut_by_count_and_bytes() {
        let object = |kb: usize| -> (Kind, RawResource) {
            (
                Kind::Vm,
                Resource::new("a", json!({ "x": "y".repeat(kb << 10) })),
            )
        };
        let sizes = |objects: Objects, max_objects, max_bytes| -> Vec<usize> {
            let mut it = objects.into_iter().peekable();
            let mut out = Vec::new();
            while it.peek().is_some() {
                let part = take_part(&mut it, max_objects, max_bytes);
                assert!(!part.is_empty());
                out.push(part.len());
            }
            out
        };
        let small: Objects = (0..10).map(|_| object(1)).collect();
        assert_eq!(sizes(small.clone(), 4, usize::MAX), [4, 4, 2]);
        // 1 KiB 남짓한 리소스는 2.5 KiB에 둘씩 든다.
        assert_eq!(sizes(small, 100, 2560), [2, 2, 2, 2, 2]);
        let mixed = vec![object(1), object(8), object(1), object(1)];
        assert_eq!(sizes(mixed, 100, 4096), [1, 1, 2]);
        // 기본 한도: 4 MiB를 넘지 않게 자른다.
        let big: Objects = (0..10).map(|_| object(1024)).collect();
        let parts = sizes(big, SNAPSHOT_PART_OBJECTS, SNAPSHOT_PART_BYTES);
        assert_eq!(parts, [3, 3, 3, 1]);
    }

    /// 프레임은 노드 사이로 JSON으로 오간다(gRPC). 리비전 틈을 메우는 적용 위치와 멤버십까지 그대로 돌아온다.
    #[test]
    fn frames_round_trip_through_json() {
        let applied = LogId::new(CommittedLeaderId::new(3, NodeId::from(1)), 42);
        let mut resource: RawResource = Resource::new("a", json!({"x": 1}));
        resource.revision = 41;
        let frames = [
            FollowFrame::Hello {
                role: NodeRole::Observer,
                leader: Some((NodeId::from(1), "node-1".into())),
                voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
            },
            FollowFrame::SnapshotPart {
                objects: vec![(Kind::Vm, resource.clone())],
            },
            FollowFrame::SnapshotEnd {
                applied: Some(applied),
                membership: StoredMembership::default(),
            },
            FollowFrame::BatchPart {
                events: vec![WatchEvent::Put {
                    kind: Kind::Vm,
                    resource: resource.clone(),
                }],
            },
            FollowFrame::Batch {
                events: vec![WatchEvent::Put {
                    kind: Kind::Vm,
                    resource,
                }],
                applied,
                membership: Some(StoredMembership::default()),
            },
            FollowFrame::Progress {
                applied: Some(applied),
                leader: None,
                leader_addr: None,
            },
        ];
        for frame in frames {
            let json = serde_json::to_vec(&frame).unwrap();
            assert_eq!(serde_json::from_slice::<FollowFrame>(&json).unwrap(), frame);
        }
    }
}
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use follow::{FollowClient, FollowFrame, NodeRole, PROGRESS_INTERVAL};
pub use health::{QUORUM_ACK_MAX, RaftHealth};
```

을 다음으로 바꾼다:

```rust
pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use follow::{FollowClient, FollowFrame, FollowGate, NodeRole, PROGRESS_INTERVAL};
pub use health::{QUORUM_ACK_MAX, RaftHealth};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
    follows: HashMap<NodeId, usize>,
}
```

을 다음으로 바꾼다:

```rust
    follows: HashMap<NodeId, usize>,
    /// Follow 프레임을 받지 못하고 멈춘 노드(풀면 이어 받는다)
    follow_paused: HashSet<NodeId>,
    /// 스냅샷 조각을 하나마다 이만큼 늦게 받는 노드
    slow_snapshots: HashMap<NodeId, Duration>,
}
```

`crates/opl-cluster/src/memnet.rs`에서

```rust

    /// 노드 `to`에 건 Follow 수
```

을 다음으로 바꾼다:

```rust

    /// 그 노드가 받는 Follow 프레임을 멈춘다(연결은 그대로, 풀면 이어 받는다). 프레임이 늦게 오는 관찰 노드.
    pub fn pause_follow(&self, id: NodeId) {
        self.routes.lock().unwrap().follow_paused.insert(id);
    }

    /// 그 노드가 받는 스냅샷 조각을 하나마다 `delay`씩 늦춘다(다른 프레임은 그대로). 스냅샷이 오래 걸리는 관찰 노드.
    pub fn slow_snapshots(&self, id: NodeId, delay: Duration) {
        self.routes.lock().unwrap().slow_snapshots.insert(id, delay);
    }

    /// 노드 `to`에 건 Follow 수
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
        routes.replies_lost.remove(&id);
    }
```

을 다음으로 바꾼다:

```rust
        routes.replies_lost.remove(&id);
        routes.follow_paused.remove(&id);
        routes.slow_snapshots.remove(&id);
    }

    /// `to`가 받을 Follow 프레임을 멈춘 동안, 또는 늦춘 스냅샷 조각이면 그만큼 붙잡는다.
    async fn hold_frame(&self, to: NodeId, frame: &FollowFrame) {
        let started = tokio::time::Instant::now();
        loop {
            let hold = {
                let routes = self.routes.lock().unwrap();
                routes.follow_paused.contains(&to)
                    || (matches!(frame, FollowFrame::SnapshotPart { .. })
                        && routes
                            .slow_snapshots
                            .get(&to)
                            .is_some_and(|d| started.elapsed() < *d))
            };
            if !hold {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
        let (router, from) = (self.router.clone(), self.from);
        Ok(frames
            .take_while(move |_| std::future::ready(router.route(from, to).is_ok()))
```

을 다음으로 바꾼다:

```rust
        let (router, from) = (self.router.clone(), self.from);
        let held = router.clone();
        Ok(frames
            .then(move |frame| {
                let router = held.clone();
                async move {
                    router.hold_frame(from, &frame).await;
                    frame
                }
            })
            .take_while(move |_| std::future::ready(router.route(from, to).is_ok()))
```

`crates/opl-cluster/src/replica.rs` 전체:

```rust
//! 관찰 노드의 복제본(설계 §6). Raft 구성원이 아니고, 투표 멤버 하나에 Follow로 붙어 받은 스냅샷과 적용 묶음을
//! 자기 저장소에 쓴다. 쓰기와 리스 갱신은 `Cluster`가 리더로 전달한다.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::{BasicNode, StoredMembership};
use redb::{Database, ReadableDatabase};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cluster::ClusterError;
use crate::follow::{FollowClient, FollowFrame};
use crate::health::ApplyLag;
use crate::state_machine::{Objects, apply_feed, install_objects};
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::NodeId;
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, BasicNode>;

/// 붙은 노드에서 이만큼 아무 프레임도 오지 않으면 끊고 다시 붙는다(서버는 1초마다 `Progress`를 보낸다).
pub(crate) const FOLLOW_STALL: Duration = Duration::from_secs(3);

/// 다시 붙기 전에 쉬는 시간
const RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// 리더가 돌려준 인덱스(전달한 쓰기·리스 갱신)를 붙은 노드가 이보다 오래 알리지 않으면(`Progress`·묶음의 적용 위치)
/// 뒤처진 것으로 보고 다른 투표 멤버로 옮긴다(살아 있어 `Progress`는 오지만 로그를 받지 못하는 노드).
pub(crate) const UPSTREAM_LAG_MAX: Duration = Duration::from_secs(3);

/// 연결 하나가 끝난 까닭
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// 붙지 못했거나 진행을 하나도 받지 못하고 끊겼다. 다음에는 다른 노드를 고른다.
    Failed,
    /// 진행을 받다가 끊겼다.
    Progressed,
    /// 붙은 노드가 리더다(리더를 모르고 골랐거나 그 노드가 리더가 됐다). 다른 투표 멤버로 옮긴다.
    Move,
    /// 붙은 노드가 리더보다 뒤처졌다. 다음에는 다른 노드를 고른다.
    Lagging,
}

/// 관찰 노드의 복제본. 복제 작업은 `start`에서 띄우고 `shutdown`으로 멈춘다.
#[derive(Clone)]
pub(crate) struct Replica {
    inner: Arc<Inner>,
}

struct Inner {
    id: NodeId,
    db: Arc<Database>,
    hub: Arc<WatchHub>,
    writer: StoreWriter,
    follow: Arc<dyn FollowClient>,
    status: Mutex<Status>,
    /// `shutdown`이 `true`로 바꾼다.
    stopped: watch::Sender<bool>,
    /// `follow_another`가 올린다. 붙은 노드를 버리고 다른 노드로 옮긴다.
    leave_upstream: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// 붙은 노드에게서 들은 것
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    pub(crate) leader: Option<(NodeId, String)>,
    /// 붙을 후보(투표 멤버와 클러스터 주소)
    pub(crate) voters: BTreeMap<NodeId, String>,
    /// 지금 붙어 있는 노드
    pub(crate) upstream: Option<NodeId>,
    /// 붙은 노드가 마지막으로 알린 적용 인덱스
    pub(crate) upstream_applied: u64,
    /// 리더가 돌려준 가장 큰 인덱스(전달한 쓰기·리스 갱신). 이만큼 적용하지 못한 시간이 적용 지연이다.
    pub(crate) leader_applied: u64,
    /// 붙은 노드에서 마지막으로 진행(스냅샷·묶음·`Progress`)을 받은 때
    pub(crate) last_progress: Option<Instant>,
    /// 연결이 끝나 다시 붙은 횟수
    pub(crate) reconnects: u64,
}

impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다. 저장소의 적용 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
        id: NodeId,
        store: &Store,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let voters = voter_addrs(&read_membership(&db)?);
        let inner = Arc::new(Inner {
            id,
            db,
            hub: store.watch_hub().clone(),
            writer: store.writer().clone(),
            follow,
            status: Mutex::new(Status {
                voters,
                ..Default::default()
            }),
            stopped: watch::channel(false).0,
            leave_upstream: watch::channel(0).0,
            task: Mutex::new(None),
        });
        let task = tokio::spawn(inner.clone().run());
        *inner.task.lock().unwrap() = Some(task);
        Ok(Self { inner })
    }

    pub(crate) fn status(&self) -> Status {
        self.inner.status.lock().unwrap().clone()
    }

    pub(crate) fn leader(&self) -> Option<(NodeId, String)> {
        self.inner.status.lock().unwrap().leader.clone()
    }

    /// 저장소에 적용된 마지막 멤버십
    pub(crate) fn membership(&self) -> Result<Membership, StoreError> {
        read_membership(&self.inner.db)
    }

    /// 리더를 묻는 쪽에 돌려줄 `NotLeader`
    pub(crate) fn not_leader(&self) -> ClusterError {
        let leader = self.leader();
        ClusterError::NotLeader {
            leader_id: leader.as_ref().map(|(id, _)| *id),
            leader_addr: leader.map(|(_, addr)| addr),
        }
    }

    /// 리더가 돌려준 인덱스(그때 리더가 적용했거나 커밋한 것)를 적는다. 붙은 노드가 이것을 오래 보내지 않으면
    /// 옮긴다.
    pub(crate) fn heard_from_leader(&self, index: u64) {
        let mut status = self.inner.status.lock().unwrap();
        status.leader_applied = status.leader_applied.max(index);
    }

    /// 붙은 노드를 버리고 다른 투표 멤버로 옮긴다(리더가 돌려준 인덱스를 기다리다 시간이 지났을 때).
    pub(crate) fn follow_another(&self) {
        self.inner.leave_upstream.send_modify(|n| *n += 1);
    }

    /// 복제 작업을 멈춘다.
    pub(crate) async fn shutdown(&self) {
        let task = self.inner.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.inner.stopped.send_replace(true);
    }

    /// `shutdown`하면 `true`가 되는 수신자
    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.inner.stopped.subscribe()
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        // 이번 차례에 붙지 못했거나 아무것도 받지 못한 노드. 진행을 받으면 비운다.
        let mut skip = BTreeSet::new();
        loop {
            let Some((to, addr)) = self.pick(&skip) else {
                skip.clear();
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            };
            let since = self.hub.revision();
            // 다른 노드에 붙지 못해 리더까지 왔으면 리더에 머문다.
            let may_move = skip.is_empty();
            let outcome = match self.follow.follow(to, &addr, since).await {
                Ok(frames) => self.consume(to, frames, may_move).await,
                Err(e) => {
                    tracing::debug!(upstream = %to, error = %e, "cannot follow");
                    Outcome::Failed
                }
            };
            {
                let mut status = self.status.lock().unwrap();
                status.upstream = None;
                status.reconnects += 1;
            }
            match outcome {
                Outcome::Failed | Outcome::Lagging => {
                    skip.insert(to);
                }
                Outcome::Progressed => skip.clear(),
                Outcome::Move => {}
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// 붙을 노드: 리더가 아닌 투표 멤버 가운데 이 노드와의 rendezvous 해시가 가장 큰 것(관찰 노드가 고르게
    /// 퍼진다), 없으면 리더. `skip`에 든 노드는 건너뛴다.
    fn pick(&self, skip: &BTreeSet<NodeId>) -> Option<(NodeId, String)> {
        let status = self.status.lock().unwrap();
        let leader = status.leader.as_ref().map(|(id, _)| *id);
        let mut ranked: Vec<(&NodeId, &String)> = status
            .voters
            .iter()
            .filter(|(id, _)| Some(**id) != leader && **id != self.id)
            .collect();
        ranked.sort_by_key(|(id, _)| std::cmp::Reverse(rendezvous(self.id, **id)));
        ranked
            .into_iter()
            .map(|(id, addr)| (*id, addr.clone()))
            .chain(status.leader.clone())
            .find(|(id, _)| !skip.contains(id))
    }

    /// 스트림이 끝나거나 `FOLLOW_STALL` 동안 멈출 때까지 프레임을 적용한다. `may_move`면 붙은 노드가 리더일 때
    /// 다른 투표 멤버가 있으면 끊는다.
    async fn consume(
        &self,
        to: NodeId,
        mut frames: BoxStream<'static, Result<FollowFrame, ClusterError>>,
        may_move: bool,
    ) -> Outcome {
        let mut progressed = false;
        let ended = |progressed| {
            if progressed {
                Outcome::Progressed
            } else {
                Outcome::Failed
            }
        };
        let mut parts = Vec::new();
        // 스냅샷 조각을 받는 중이다(첫 `SnapshotPart`부터 `SnapshotEnd`까지).
        let mut in_snapshot = false;
        // 다음 `Batch`와 한 번에 적용할 나눠 온 묶음의 앞부분
        let mut batch_parts = Vec::new();
        // 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(붙을 때마다, 스냅샷을 설치할 때마다 새로 잰다)
        let mut lag = ApplyLag::default();
        let mut leave = self.leave_upstream.subscribe();
        leave.borrow_and_update();
        loop {
            if may_move && self.upstream_is_leader(to) {
                return Outcome::Move;
            }
            // 스냅샷이나 나눈 묶음을 받는 동안은 붙은 노드가 알린 적용이 움직이지 않는다(스냅샷이 오래 걸려도 끊지 않게).
            let receiving = in_snapshot || !batch_parts.is_empty();
            if !receiving && self.upstream_lag(&mut lag) > UPSTREAM_LAG_MAX {
                tracing::warn!(upstream = %to, "the followed node lags behind the leader; following another");
                return Outcome::Lagging;
            }
            let next = tokio::select! {
                next = tokio::time::timeout(FOLLOW_STALL, frames.next()) => next,
                _ = leave.changed(), if !receiving => {
                    // 붙은 노드는 리더의 인덱스를 알렸는데 이 노드가 늦게 적용하는 것이면 머문다.
                    if self.upstream_behind() {
                        tracing::warn!(upstream = %to, "a forwarded request was not applied in time; following another");
                        return Outcome::Lagging;
                    }
                    continue;
                }
            };
            let frame = match next {
                Ok(Some(Ok(frame))) => frame,
                Ok(Some(Err(e))) => {
                    tracing::debug!(upstream = %to, error = %e, "follow stream failed");
                    return ended(progressed);
                }
                Ok(None) => return ended(progressed),
                Err(_) => {
                    tracing::warn!(upstream = %to, "no progress from the followed node; following another");
                    return ended(progressed);
                }
            };
            // 스냅샷 조각은 `SnapshotEnd`까지, 나눈 묶음은 `Batch`까지 이어 와야 한다(서버는 그 사이에 다른 프레임을
            // 넣지 않는다). 아니면 모은 것을 버리고 다시 붙는다.
            let in_order = match frame {
                FollowFrame::SnapshotPart { .. } | FollowFrame::SnapshotEnd { .. } => {
                    batch_parts.is_empty()
                }
                FollowFrame::BatchPart { .. } | FollowFrame::Batch { .. } => !in_snapshot,
                FollowFrame::Hello { .. } | FollowFrame::Progress { .. } => !receiving,
            };
            if !in_order {
                tracing::warn!(upstream = %to, "the followed node sent frames out of order; following again");
                return ended(progressed);
            }
            let result = match frame {
                FollowFrame::Hello { leader, voters, .. } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream = Some(to);
                    status.leader = leader;
                    if !voters.is_empty() {
                        status.voters = voters;
                    }
                    continue;
                }
                FollowFrame::SnapshotPart { objects } => {
                    in_snapshot = true;
                    parts.extend(objects);
                    Ok(())
                }
                FollowFrame::BatchPart { events } => {
                    batch_parts.extend(events);
                    Ok(())
                }
                FollowFrame::SnapshotEnd {
                    applied,
                    membership,
                } => {
                    in_snapshot = false;
                    let result = self
                        .install(applied, membership, std::mem::take(&mut parts))
                        .await;
                    // 설치하는 동안 늦은 전달 기다림이나 지연은 붙은 노드 탓이 아니다. 새로 잰다.
                    lag = ApplyLag::default();
                    leave.borrow_and_update();
                    result
                }
                FollowFrame::Batch {
                    events,
                    applied,
                    membership,
                } => {
                    batch_parts.extend(events);
                    self.apply(FeedBatch {
                        events: std::mem::take(&mut batch_parts),
                        applied,
                        membership,
                    })
                    .await
                }
                FollowFrame::Progress {
                    applied,
                    leader,
                    leader_addr,
                } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream_applied = applied.map_or(0, |l| l.index);
                    status.leader = leader.zip(leader_addr);
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::error!(upstream = %to, error = %e, "cannot write the followed state");
                return ended(progressed);
            }
            progressed = true;
            self.status.lock().unwrap().last_progress = Some(Instant::now());
        }
    }

    /// 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(이 노드의 적용이 아니라 붙은 노드가 알린 적용으로 잰다:
    /// 이 노드의 디스크가 느린 것을 붙은 노드 탓으로 돌리지 않게)
    fn upstream_lag(&self, lag: &mut ApplyLag) -> Duration {
        let status = self.status.lock().unwrap();
        lag.observe(
            Some(status.leader_applied),
            Some(status.upstream_applied),
            Instant::now(),
        )
    }

    /// 붙은 노드가 알린 적용이 리더가 돌려준 인덱스보다 뒤다.
    fn upstream_behind(&self) -> bool {
        let status = self.status.lock().unwrap();
        status.upstream_applied < status.leader_applied
    }

    /// 붙은 노드가 리더이고 붙을 다른 투표 멤버가 있다.
    fn upstream_is_leader(&self, to: NodeId) -> bool {
        let status = self.status.lock().unwrap();
        status.leader.as_ref().is_some_and(|(l, _)| *l == to)
            && status.voters.keys().any(|v| *v != to && *v != self.id)
    }

    async fn install(
        &self,
        applied: Option<openraft::LogId<NodeId>>,
        membership: Membership,
        objects: Objects,
    ) -> Result<(), StoreError> {
        let voters = voter_addrs(&membership);
        let revision = applied.map_or(0, |l| l.index);
        // 붙은 노드가 이 노드보다 뒤다(뒤처진 노드에 붙은 뒤 그 노드가 스냅샷을 설치했다). 설치하면 이미 돌려준 리비전
        // 읽기·리스 갱신이 거짓이 되므로 두고, 이어 오는 묶음에서 이미 적용한 것은 건너뛴다. 같은 리비전이면 상태가
        // 같으므로 설치하지 않는다(구독자에게 헛된 `Reset`을 내지 않게).
        if revision <= self.hub.revision() {
            tracing::warn!(
                revision,
                applied = self.hub.revision(),
                "ignoring a snapshot not newer than this node's state"
            );
            return Ok(());
        }
        self.writer
            .run(move |db| install_objects(db, applied, membership, objects))
            .await?;
        self.hub.reset(revision);
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(revision);
        if !voters.is_empty() {
            status.voters = voters;
        }
        Ok(())
    }

    async fn apply(&self, mut batch: FeedBatch) -> Result<(), StoreError> {
        // 다시 붙은 뒤 이미 적용한 묶음이 올 수 있다.
        if batch.applied.index <= self.hub.revision() {
            return Ok(());
        }
        // 투표 멤버마다 묶음 경계가 달라, 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 변경을 품을 수 있다. 그것은 쓰지도
        // 내지도 않는다(옛 값으로 되돌리거나 감시에 두 번 보이지 않게). 적용 위치는 묶음의 것이다.
        let applied_here = self.hub.revision();
        batch.events.retain(|e| e.revision() > applied_here);
        let batch = self
            .writer
            .run(move |db| apply_feed(db, &batch).map(|()| batch))
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(batch.applied.index);
        if let Some(m) = &batch.membership {
            let voters = voter_addrs(m);
            if !voters.is_empty() {
                status.voters = voters;
            }
        }
        drop(status);
        self.hub.publish(batch);
        Ok(())
    }
}

/// 같은 관찰 노드는 늘 같은 순서로, 관찰 노드마다는 다른 순서로 투표 멤버를 고르게 하는 점수
fn rendezvous(me: NodeId, voter: NodeId) -> u64 {
    let mut h = DefaultHasher::new();
    (me, voter).hash(&mut h);
    h.finish()
}

fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default())
}

fn voter_addrs(m: &Membership) -> BTreeMap<NodeId, String> {
    let m = m.membership();
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use openraft::BasicNode;
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tempfile::TempDir;

    use super::*;
    use crate::cluster::{Cluster, ClusterConfig};
    use crate::memnet::MemRouter;
    use crate::types::LeaderRequest;
    use crate::watch::{HISTORY, WatchError, WatchEvent};

    const WAIT: Duration = Duration::from_secs(10);

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

    fn config(root: &TempDir, i: u64, in_memory: bool) -> ClusterConfig {
        let mut config = ClusterConfig::new(NodeId::from(i), root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.in_memory = in_memory;
        config
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !cond() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 투표 멤버 1..=3(메모리 저장소)과 관찰 노드 4..(파일 저장소). 관찰 노드는 실제 길처럼 learner로 가입해
    /// 따라잡은 뒤 멤버십에서 빠지고, 같은 저장소로 관찰 노드로 다시 열린다.
    struct Net {
        router: MemRouter,
        root: TempDir,
        voters: BTreeMap<NodeId, Cluster>,
        observers: BTreeMap<NodeId, Cluster>,
    }

    impl Net {
        async fn start(observers: u64) -> Self {
            Self::start_with(observers, |_| {}).await
        }

        /// 투표 멤버의 Raft 설정을 `raft`로 고쳐 띄운다.
        async fn start_with(observers: u64, raft: impl Fn(&mut openraft::Config)) -> Self {
            let mut net = Net {
                router: MemRouter::default(),
                root: TempDir::new().unwrap(),
                voters: BTreeMap::new(),
                observers: BTreeMap::new(),
            };
            for i in 1..=3 {
                let id = NodeId::from(i);
                let mut config = config(&net.root, i, true);
                raft(&mut config.raft);
                let c = Cluster::open(config, net.router.network(id))
                    .await
                    .unwrap()
                    .with_forwarder(net.router.forwarder(id));
                net.router.register(id, &c);
                net.voters.insert(id, c);
            }
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
                .collect();
            net.voters[&NodeId::from(1)]
                .initialize(members)
                .await
                .unwrap();
            for i in 4..4 + observers {
                net.add_observer(i).await;
            }
            net
        }

        fn leader(&self) -> &Cluster {
            self.voters
                .values()
                .find(|c| c.members().leader == Some(c.id()))
                .expect("a leader")
        }

        async fn settled_leader(&self) -> NodeId {
            wait_until("a leader", || {
                self.voters
                    .values()
                    .any(|c| c.members().leader == Some(c.id()))
            })
            .await;
            self.leader().id()
        }

        async fn add_observer(&mut self, i: u64) {
            let id = NodeId::from(i);
            self.settled_leader().await;
            let learner = Cluster::open(config(&self.root, i, false), self.router.network(id))
                .await
                .unwrap();
            self.router.register(id, &learner);
            self.leader()
                .add_learner(id, BasicNode::new(format!("node-{i}")))
                .await
                .unwrap();
            self.leader()
                .request(LeaderRequest::Remove { id })
                .await
                .unwrap();
            self.router.unregister(id);
            learner.shutdown().await.unwrap();
            drop(learner);
            let observer = loop {
                match Cluster::open_observer(config(&self.root, i, false), self.router.follower(id))
                    .await
                {
                    Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    other => break other.unwrap(),
                }
            }
            .with_forwarder(self.router.forwarder(id));
            self.router.register(id, &observer);
            self.observers.insert(id, observer);
        }

        fn observer(&self, n: usize) -> &Cluster {
            self.observers.values().nth(n).unwrap()
        }

        /// 모든 관찰 노드가 리더의 지금 리비전까지 적용하기를 기다린다.
        async fn sync(&self) -> u64 {
            let revision = self.leader().revision();
            for o in self.observers.values() {
                o.wait_revision(revision, WAIT).await.unwrap();
            }
            revision
        }

        async fn stop_voter(&mut self, id: NodeId) {
            self.router.unregister(id);
            self.voters.remove(&id).unwrap().shutdown().await.unwrap();
        }

        /// 모든 투표 멤버의 기록이 `revision`을 지나칠 때까지 리더에 쓴다.
        async fn write_past_history(&self, revision: u64) {
            let mut n = 0;
            while self.voters.values().any(|v| {
                !matches!(
                    v.node_handle().hub().feed(revision),
                    Err(WatchError::Expired { .. })
                )
            }) {
                for _ in 0..64 {
                    self.leader()
                        .create(&volume(&format!("bulk-{n}")))
                        .await
                        .unwrap();
                    n += 1;
                }
                assert!(n < 4 * HISTORY, "history never expired");
            }
        }

        async fn shutdown(self) {
            for c in self.observers.values().chain(self.voters.values()) {
                c.shutdown().await.unwrap();
            }
        }
    }

    fn replica(c: &Cluster) -> Replica {
        match c.node_handle().backend() {
            crate::cluster::Backend::Observer(r) => r,
            crate::cluster::Backend::Member { .. } => panic!("not an observer"),
        }
    }

    async fn next_events(w: &mut crate::watch::Watch, n: usize) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap());
        }
        out
    }

    /// 관찰 노드는 Raft 밖에서 투표 멤버와 같은 객체를 같은 `list` 리비전으로 보인다.
    #[tokio::test]
    async fn observers_serve_the_same_objects_at_the_same_list_revision() {
        let net = Net::start(2).await;
        let vols: Vec<Volume> = (0..3).map(|i| volume(&format!("v{i}"))).collect();
        for v in &vols {
            net.leader().create(v).await.unwrap();
        }
        let gone = vols[0].id;
        net.leader().delete::<VolumeSpec>(gone, None).await.unwrap();
        net.sync().await;
        let expected = net.leader().list::<VolumeSpec>().unwrap();
        for o in net.observers.values() {
            assert!(o.raft().is_none());
            assert_eq!(o.list::<VolumeSpec>().unwrap(), expected);
            assert!(o.get::<VolumeSpec>(gone).unwrap().is_none());
            assert!(o.get_by_name::<VolumeSpec>("v0").unwrap().is_none());
            assert_eq!(
                o.get_by_name::<VolumeSpec>("v1").unwrap().map(|v| v.id),
                Some(vols[1].id)
            );
            // 멤버십은 저장소에 적용된 것, 리더는 붙은 노드가 알린 것이다.
            let members = o.members();
            assert_eq!(members.leader, Some(net.leader().id()));
            assert_eq!(members.voters.len(), 3);
            assert!(!members.contains(o.id()));
        }
        net.shutdown().await;
    }

    /// 관찰 노드에서 쓴 것은 돌아오자마자 그 노드에서 읽히고, 리스 갱신은 리더로 전달되어 성공한다.
    /// 리더로서 할 일은 아는 리더를 담은 `NotLeader`다.
    #[tokio::test]
    async fn an_observer_reads_its_own_writes_and_renews_its_lease() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let v = volume("mine");
        let revision = o.create(&v).await.unwrap();
        assert!(o.revision() >= revision);
        assert_eq!(
            o.get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
            revision
        );

        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        assert!(net.leader().leases().renewed.contains_key(&o.id()));
        assert!(o.leases().renewed.is_empty());
        let leader = net.leader().id();
        assert!(matches!(
            o.read_barrier().await,
            Err(ClusterError::NotLeader { leader_id: Some(l), leader_addr: Some(_) }) if l == leader
        ));
        assert!(matches!(
            o.execute_as_leader(LeaderRequest::RenewLease { node: o.id() }).await,
            Err(ClusterError::NotLeader { leader_id: Some(l), .. }) if l == leader
        ));

        // 헬스: 붙은 노드의 진행이 오고 리더를 안다.
        let health = o.health();
        assert_eq!(health.leader, Some(leader));
        assert!(health.quorum);

        // Raft 구성원이 아닌 노드에 건 Follow는 `Hello`(부른 쪽의 역할)만 오고 닫힌다.
        let frames: Vec<FollowFrame> = o.follow(NodeId::from(99), 0).unwrap().collect().await;
        assert!(
            matches!(
                frames.as_slice(),
                [FollowFrame::Hello { role: crate::follow::NodeRole::Observer, voters, .. }] if voters.len() == 3
            ),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 관찰 노드는 리더가 아닌 투표 멤버에 붙는다. 시작할 때 리더를 몰라 리더에 붙었으면 `Hello`를 보고 옮긴다.
    #[tokio::test]
    async fn observers_follow_voters_other_than_the_leader() {
        let net = Net::start(4).await;
        let leader = net.settled_leader().await;
        for o in net.observers.values() {
            let r = replica(o);
            wait_until("following a non-leader voter", || {
                r.status().upstream.is_some_and(|u| u != leader)
            })
            .await;
        }
        net.shutdown().await;
    }

    /// 붙은 투표 멤버가 죽으면 다른 투표 멤버로 자기 리비전부터 다시 붙는다. 기록 안이라 `Reset`이 없다.
    #[tokio::test]
    async fn an_observer_moves_to_another_voter_without_reset() {
        let mut net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        // 리더가 아닌 투표 멤버에 붙는다.
        assert_ne!(upstream, leader);
        let before = r.status().reconnects;
        let mut w = net.observer(0).watch(net.sync().await).unwrap();

        net.stop_voter(upstream).await;
        let mut written = Vec::new();
        for i in 0..3 {
            let v = volume(&format!("after-{i}"));
            net.leader().create(&v).await.unwrap();
            written.push(v.id);
        }
        let events = next_events(&mut w, 3).await;
        let ids: Vec<_> = events
            .iter()
            .map(|e| match e {
                WatchEvent::Put { resource, .. } => resource.id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, written);
        let status = r.status();
        assert!(status.reconnects > before);
        assert!(status.upstream.is_some_and(|u| u != upstream));
        net.shutdown().await;
    }

    /// 끊긴 동안 투표 멤버의 기록이 지나가면 다시 붙을 때 스냅샷을 받고 구독자는 `Reset`을 본다.
    #[tokio::test]
    async fn an_observer_past_the_history_gets_a_snapshot_and_reset() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        let mut w = o.watch(revision).unwrap();
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        assert_eq!(o.revision(), revision);
        net.router.heal(o.id());

        let reset = tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap();
        assert!(matches!(reset, WatchEvent::Reset { .. }), "{reset:?}");
        net.sync().await;
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 적용이 막혀 뒤처진 관찰 노드는 서버 쪽 피드가 `Lagged`로 끝나 다시 붙고 따라잡는다.
    #[tokio::test]
    async fn a_lagging_observer_reconnects_and_catches_up() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        let revision = net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let before = r.status().reconnects;
        // 관찰 노드의 저장소 스레드를 붙잡는다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        net.write_past_history(revision + 64).await;
        release.send(()).unwrap();

        net.sync().await;
        assert!(r.status().reconnects > before);
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
    #[tokio::test]
    async fn observer_watch_events_match_the_voter() {
        let net = Net::start(2).await;
        let revision = net.sync().await;
        let mut watches: Vec<_> = std::iter::once(net.leader())
            .chain(net.observers.values())
            .map(|c| c.watch(revision).unwrap())
            .collect();
        let v = volume("w");
        net.leader().create(&v).await.unwrap();
        net.observer(1)
            .update_spec(
                v.id,
                None,
                BTreeMap::from([("a".into(), "b".into())]),
                &v.spec,
            )
            .await
            .unwrap();
        net.observer(0)
            .update_status::<VolumeSpec>(v.id, &Default::default())
            .await
            .unwrap();
        net.leader().delete::<VolumeSpec>(v.id, None).await.unwrap();

        let expected = next_events(&mut watches[0], 4).await;
        assert!(matches!(expected[3], WatchEvent::Deleted { .. }));
        for w in &mut watches[1..] {
            assert_eq!(next_events(w, 4).await, expected);
        }
        net.shutdown().await;
    }

    fn log_id(index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `revision`에 쓰인 볼륨
    fn raw(name: &str, revision: u64) -> crate::types::RawResource {
        let mut r: crate::types::RawResource =
            serde_json::from_value(serde_json::to_value(volume(name)).unwrap()).unwrap();
        r.revision = revision;
        r
    }

    fn put(r: &crate::types::RawResource) -> WatchEvent {
        WatchEvent::Put {
            kind: opl_types::resource::Kind::Volume,
            resource: r.clone(),
        }
    }

    fn hello() -> FollowFrame {
        FollowFrame::Hello {
            role: crate::follow::NodeRole::Observer,
            leader: Some((NodeId::from(1), "node-1".into())),
            voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
        }
    }

    /// 정해 둔 프레임을 돌려주는 Follow 통로(투표 멤버마다 묶음 경계·스냅샷이 다른 경우를 만든다). 부를 때마다 다음
    /// 대본을 쓰고, `hold`인 대본은 끝에서 끊지 않고 머문다. 대본이 떨어지면 붙지 못한다.
    #[derive(Default)]
    struct Scripted {
        scripts: Mutex<std::collections::VecDeque<(Vec<FollowFrame>, bool)>>,
    }

    impl Scripted {
        fn then(self, frames: Vec<FollowFrame>, hold: bool) -> Self {
            self.scripts.lock().unwrap().push_back((frames, hold));
            self
        }
    }

    #[async_trait::async_trait]
    impl FollowClient for Scripted {
        async fn follow(
            &self,
            _to: NodeId,
            _addr: &str,
            _since: u64,
        ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
            let Some((frames, hold)) = self.scripts.lock().unwrap().pop_front() else {
                return Err(ClusterError::Unreachable("no more scripts".into()));
            };
            let frames = futures_util::stream::iter(frames.into_iter().map(Ok));
            Ok(if hold {
                frames.chain(futures_util::stream::pending()).boxed()
            } else {
                frames.boxed()
            })
        }
    }

    /// 대본대로 받는 관찰 노드(투표 멤버 1만 든 멤버십을 둔 빈 저장소라 그 노드를 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let config = config(root, 9, false);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let store = Store::open(&config.data_dir.join(crate::cluster::DB_FILE)).unwrap();
        let membership = Membership::new(
            None,
            openraft::Membership::new(
                vec![[NodeId::from(1)].into()],
                BTreeMap::from([(NodeId::from(1), BasicNode::new("node-1"))]),
            ),
        );
        let txn = store.db().begin_write().unwrap();
        {
            let mut meta = txn.open_table(META).unwrap();
            crate::store::put_json(&mut meta, KEY_LAST_MEMBERSHIP, &membership).unwrap();
        }
        txn.commit().unwrap();
        drop(store);
        let script = Arc::new(script);
        loop {
            match Cluster::open_observer(config.clone(), script.clone()).await {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        }
    }

    /// 감시에서 더 오지 않을 때까지 받은 이벤트의 리비전
    async fn drain(w: &mut crate::watch::Watch) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        while let Ok(e) = tokio::time::timeout(Duration::from_millis(300), w.next()).await {
            out.push(e.unwrap());
        }
        out
    }

    /// 다른 투표 멤버로 옮기면 묶음 경계가 달라 새 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 관찰 노드는
    /// 그 이벤트를 다시 내지 않는다(감시 리비전이 엄격히 오른다).
    #[tokio::test]
    async fn events_already_applied_are_not_replayed_after_moving() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                false,
            )
            // 다른 투표 멤버는 11..=13을 한 묶음으로 적용했다.
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b), put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(13, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        o.shutdown().await.unwrap();
    }

    /// 자기보다 뒤인 상류가 보낸 스냅샷(더 낮은 리비전)은 설치하지 않는다: 상태와 리비전이 뒤로 가지 않고 `Reset`도
    /// 없다. 그 뒤 묶음은 이어 받는다.
    #[tokio::test]
    async fn an_older_snapshot_from_the_upstream_is_not_installed() {
        let root = TempDir::new().unwrap();
        let (x, y, z, w) = (raw("x", 5), raw("y", 20), raw("z", 14), raw("w", 21));
        let snapshot = |objects: Vec<&crate::types::RawResource>, applied| {
            vec![
                FollowFrame::SnapshotPart {
                    objects: objects
                        .into_iter()
                        .map(|r| (opl_types::resource::Kind::Volume, r.clone()))
                        .collect(),
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(applied)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot(vec![&x], 10));
        first.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(20),
            membership: None,
        });
        let mut second = vec![hello()];
        second.extend(snapshot(vec![&x, &z], 15));
        second.push(FollowFrame::Batch {
            events: vec![put(&w)],
            applied: log_id(21),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(20, WAIT).await.unwrap();
        let mut watch = o.watch(20).unwrap();
        o.wait_revision(21, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == w.id),
            "{events:?}"
        );
        assert!(o.get::<VolumeSpec>(y.id).unwrap().is_some());
        assert!(o.get::<VolumeSpec>(z.id).unwrap().is_none());
        assert_eq!(o.revision(), 21);
        o.shutdown().await.unwrap();
    }

    /// 자기와 같은 리비전의 스냅샷도 설치하지 않는다(상태가 같다): 구독자는 헛된 `Reset` 없이 다음 묶음만 본다.
    #[tokio::test]
    async fn a_snapshot_at_the_same_revision_is_not_installed_again() {
        let root = TempDir::new().unwrap();
        let (x, y) = (raw("x", 10), raw("y", 11));
        let snapshot = || {
            vec![
                FollowFrame::SnapshotPart {
                    objects: vec![(opl_types::resource::Kind::Volume, x.clone())],
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(10)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot());
        let mut second = vec![hello()];
        second.extend(snapshot());
        second.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(11),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut watch = o.watch(10).unwrap();
        o.wait_revision(11, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == y.id),
            "{events:?}"
        );
        o.shutdown().await.unwrap();
    }

    /// 투표 멤버는 자기보다 앞선 관찰 노드(`since`가 자기 리비전보다 큼)를 받지 않는다: `Hello`만 보내고 닫아 다른
    /// 투표 멤버로 가게 한다(뒤처진 노드가 그 뒤 스냅샷을 설치하면 관찰 노드가 뒤로 가므로).
    #[tokio::test]
    async fn a_voter_turns_away_an_observer_ahead_of_it() {
        let net = Net::start(0).await;
        let leader = net.settled_leader().await;
        let voter = &net.voters[&leader];
        let frames: Vec<FollowFrame> = tokio::time::timeout(
            Duration::from_secs(3),
            voter
                .follow(NodeId::from(9), voter.revision() + 100)
                .unwrap()
                .collect(),
        )
        .await
        .expect("the stream was not closed");
        assert!(
            matches!(frames.as_slice(), [FollowFrame::Hello { .. }]),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 뒤처진 투표 멤버(Raft가 끊김)가 리더의 스냅샷을 설치해도, 그보다 앞선 관찰 노드는 뒤로 가지 않는다(그런 투표
    /// 멤버에 붙지 않는다).
    #[tokio::test]
    async fn an_observer_ahead_of_a_lagging_voter_never_goes_back() {
        let mut net = Net::start_with(1, |raft| {
            raft.max_in_snapshot_log_to_keep = 0;
            raft.purge_batch_size = 1;
        })
        .await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        let lagging = *net
            .voters
            .keys()
            .find(|id| **id != leader && **id != upstream)
            .unwrap();
        net.router.block_raft(lagging);
        for i in 0..5 {
            net.leader()
                .create(&volume(&format!("old-{i}")))
                .await
                .unwrap();
        }
        // 리더가 스냅샷을 만들고 로그를 지워 뒤처진 노드는 스냅샷으로만 따라잡는다.
        let raft = net.leader().raft().unwrap();
        raft.trigger().snapshot().await.unwrap();
        let purged = raft
            .wait(Some(WAIT))
            .metrics(|m| m.purged.is_some(), "leader purged logs")
            .await
            .unwrap()
            .purged
            .unwrap()
            .index;
        for i in 0..20 {
            net.leader()
                .create(&volume(&format!("new-{i}")))
                .await
                .unwrap();
        }
        let ahead = net.sync().await;
        assert!(ahead > purged);
        let mut w = net.observer(0).watch(ahead).unwrap();

        // 붙은 노드를 잃은 관찰 노드는 리더가 아닌 투표 멤버인 뒤처진 노드를 먼저 시도한다(그 노드는 받지 않는다).
        let tried = net.router.follows(lagging);
        net.stop_voter(upstream).await;
        wait_until("the observer tried the lagging voter", || {
            net.router.follows(lagging) > tried
        })
        .await;
        net.router.heal(lagging);
        net.voters[&lagging]
            .wait_revision(ahead, WAIT)
            .await
            .unwrap();
        let v = volume("after");
        net.leader().create(&v).await.unwrap();
        net.sync().await;
        let o = net.observer(0);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        let events = drain(&mut w).await;
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, WatchEvent::Reset { .. })),
            "{events:?}"
        );
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 살아 있지만 뒤처진 투표 멤버(Raft가 끊겨 `Progress`만 보냄)에 붙은 관찰 노드는 리스 갱신의 적용 기다림이 시간
    /// 초과하거나 리더가 돌려준 인덱스보다 오래 뒤처지면 다른 투표 멤버로 옮겨, 갱신이 다시 성공한다.
    #[tokio::test]
    async fn an_observer_leaves_a_lagging_voter_and_renews_its_lease() {
        let net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        assert_ne!(upstream, leader);
        net.router.block_raft(upstream);
        net.leader().create(&volume("unseen")).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut failed = 0;
        while let Err(e) = o.renew_lease(Duration::from_secs(2)).await {
            failed += 1;
            assert!(
                tokio::time::Instant::now() < deadline,
                "renewals kept failing ({failed}): {e}"
            );
        }
        assert_ne!(r.status().upstream, Some(upstream));
        net.router.heal(upstream);
        net.shutdown().await;
    }

    /// 나눠 온 묶음(`BatchPart`… `Batch`)은 마지막 프레임에서 한 번에 적용한다. 그 전에 끊기면 앞부분은 버린다.
    #[tokio::test]
    async fn split_batches_are_applied_at_once() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let snapshot = FollowFrame::SnapshotEnd {
            applied: Some(log_id(10)),
            membership: Default::default(),
        };
        let script = Scripted::default()
            // 앞부분만 오고 멈춘 뒤(진행 없음으로) 끊긴다.
            .then(
                vec![
                    hello(),
                    snapshot,
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&b)],
                    },
                    FollowFrame::Batch {
                        events: vec![put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        // 앞부분만으로는 적용하지 않는다.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(o.revision(), 10);
        assert!(o.get::<VolumeSpec>(a.id).unwrap().is_none());
        o.wait_revision(13, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        for r in [&a, &b, &c] {
            assert!(o.get::<VolumeSpec>(r.id).unwrap().is_some());
        }
        o.shutdown().await.unwrap();
    }

    /// 나눈 묶음 사이에 다른 프레임이 오면(서버는 그러지 않는다) 모은 앞부분을 버리고 다시 붙는다.
    #[tokio::test]
    async fn frames_out_of_order_drop_the_parts_and_reconnect() {
        let root = TempDir::new().unwrap();
        let (a, b) = (raw("a", 11), raw("b", 12));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        o.wait_revision(12, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12]);
        assert!(replica(&o).status().reconnects >= 1);
        o.shutdown().await.unwrap();
    }

    /// 스냅샷으로 따라잡는 관찰 노드는 스냅샷이 `UPSTREAM_LAG_MAX`보다 오래 걸려도(조각마다 1초) 리스 갱신이 도는
    /// 동안(리더가 돌려준 인덱스, 시간 초과한 적용 기다림) 붙은 노드를 버리지 않고 끝까지 받아 따라잡는다.
    #[tokio::test]
    async fn a_slow_snapshot_catch_up_is_not_cut_off_by_lag_checks() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        let target = net.leader().revision();
        net.router.heal(o.id());
        net.router.slow_snapshots(o.id(), Duration::from_secs(1));
        let renew = async {
            loop {
                let _ = o.renew_lease(Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = renew => unreachable!(),
            caught_up = o.wait_revision(target, Duration::from_secs(40)) => {
                caught_up.expect("the observer never finished the snapshot");
            }
        }
        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        net.shutdown().await;
    }
}
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
use common::{TestCluster, WAIT, node, volume};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_types::resource::Kind;
use opl_types::storage::{VolumeSpec, VolumeStatus};
```

을 다음으로 바꾼다:

```rust
use common::{TestCluster, WAIT, node, volume};
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{ClusterError, Members};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::resource::{Kind, Resource};
use opl_types::storage::{VolumeSpec, VolumeStatus};
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    c.shutdown().await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    c.shutdown().await.unwrap();
}

async fn wait_members(c: &opl_cluster::Cluster, what: &str, cond: impl Fn(&Members) -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !cond(&c.members()) {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 클러스터 정보의 관찰 노드(`ClusterStatus.observers`)는 멤버 목록에 들어가 주소를 찾을 수 있다. Raft 구성원과
/// 겹치면(내리는 중) 구성원으로 센다. 다시 연 노드는 저장소에서 바로 읽는다.
#[tokio::test]
async fn members_include_the_observers_in_the_cluster_info() {
    let mut tc = TestCluster::start(1).await;
    let me = node(1);
    let observer = node(9);
    let c = tc.get(me);
    let mut info: ClusterInfo = Resource::new(
        "test",
        ClusterSpec {
            ca_cert_pem: String::new(),
            ca_key_pem: String::new(),
        },
    );
    c.create(&info).await.unwrap();
    assert!(!c.members().contains(observer));

    info.status.observers = BTreeMap::from([(observer.0, "node-9".into()), (me.0, "stale".into())]);
    c.update_status::<ClusterSpec>(info.id, &info.status)
        .await
        .unwrap();
    wait_members(c, "the observer is listed", |m| m.contains(observer)).await;
    let members = c.members();
    assert_eq!(members.addr(observer), Some("node-9"));
    assert_eq!(members.observers.len(), 1);
    assert_eq!(
        members.addr(me),
        members.voters.get(&me).map(String::as_str)
    );

    tc.stop(me).await;
    tc.open(me).await;
    let c = tc.get(me);
    assert_eq!(c.members().addr(observer), Some("node-9"));

    c.update_status::<ClusterSpec>(info.id, &ClusterStatus::default())
        .await
        .unwrap();
    wait_members(c, "the observer is gone", |m| !m.contains(observer)).await;
    assert_eq!(c.members().addr(observer), None);
}
```

`crates/opl-peer/build.rs`에서

```rust
//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림은 바이트를 그대로, UEFI 변수 조각은 이진 형식으로 보낸다.
```

을 다음으로 바꾼다:

```rust
//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림은 바이트를 그대로, UEFI 변수 조각은 이진 형식으로 보낸다. 관찰 노드의 Follow는 JSON 서버 스트림이다.
```

`crates/opl-peer/build.rs`에서

```rust
            Method::builder()
                .name("console")
```

을 다음으로 바꾼다:

```rust
            Method::builder()
                .name("follow")
                .route_name("Follow")
                .input_type("crate::wire::FollowRequest")
                .output_type("opl_cluster::FollowFrame")
                .codec_path("crate::codec::JsonCodec")
                .server_streaming()
                .build(),
        )
        .method(
            Method::builder()
                .name("console")
```

`crates/opl-peer/src/client.rs`에서

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.
```

을 다음으로 바꾼다:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 관찰 노드의 Follow, 콘솔,
//! 마이그레이션 받기, UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.
```

`crates/opl-peer/src/client.rs`에서

```rust
use bytes::Bytes;
use openraft::BasicNode;
```

을 다음으로 바꾼다:

```rust
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::BasicNode;
```

`crates/opl-peer/src/client.rs`에서

```rust
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, Forwarder};
use opl_types::audit::{AuditEntry, AuditQuery};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, FollowClient, FollowFrame, Forwarder};
use opl_types::audit::{AuditEntry, AuditQuery};
```

`crates/opl-peer/src/client.rs`에서

```rust
use crate::proto::peer_client::PeerClient;
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};
```

을 다음으로 바꾼다:

```rust
use crate::proto::peer_client::PeerClient;
use crate::wire::FollowRequest;
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};
```

`crates/opl-peer/src/client.rs`에서

```rust
///
/// 실행기를 주면(`with_runtimes`) 주소마다 채널을 둘 둔다. Raft·리더 전달(리스 갱신과 쓰기 전달)·ping은
/// control 채널, 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk 채널이고, 채널의 연결
```

을 다음으로 바꾼다:

```rust
///
/// 실행기를 주면(`with_runtimes`) 주소마다 채널을 둘 둔다. Raft·리더 전달(리스 갱신과 쓰기 전달)·ping·Follow는
/// control 채널, 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk 채널이고, 채널의 연결
```

`crates/opl-peer/src/client.rs`에서

```rust

#[async_trait]
```

을 다음으로 바꾼다:

```rust

/// 관찰 노드가 투표 멤버에 거는 Follow(control 채널). 제한 시간 없이 이어지고, 끊긴 연결은 keepalive가, 멈춘
/// 스트림은 복제본이(진행 3초 없음) 알아챈다.
#[async_trait]
impl FollowClient for Peers {
    async fn follow(
        &self,
        _to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        let unreachable = |s: Status| ClusterError::Unreachable(s.message().to_string());
        let frames = self
            .client(addr)
            .map_err(unreachable)?
            .follow(FollowRequest { since })
            .await
            .map_err(unreachable)?
            .into_inner();
        Ok(frames.map(move |f| f.map_err(unreachable)).boxed())
    }
}

#[async_trait]
```

`crates/opl-peer/src/lib.rs`에서

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기, 감사 로그 읽기, 지표 읽기다.
```

을 다음으로 바꾼다:

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, 관찰 노드의 Follow, VM 콘솔 중계, 라이브 마이그레이션
//! 받기, UEFI 변수 파일 보내기, 감사 로그 읽기, 지표 읽기다.
```

`crates/opl-peer/src/server.rs`에서

```rust
use bytes::Bytes;
use openraft::Raft;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
```

을 다음으로 바꾼다:

```rust
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::Raft;
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{Cluster, FollowFrame, FollowGate};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
```

`crates/opl-peer/src/server.rs`에서

```rust
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, IncomingReply, InstallSnapshotReply,
    InstallSnapshotRequest, LeaderReply, MigrateIncomingReply, VoteReply, VoteRequest,
```

을 다음으로 바꾼다:

```rust
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, FollowRequest, IncomingReply, InstallSnapshotReply,
    InstallSnapshotRequest, LeaderReply, MigrateIncomingReply, VoteReply, VoteRequest,
```

`crates/opl-peer/src/server.rs`에서

```rust

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버여야 한다. 아직 멤버십을
/// 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다.
pub struct PeerService {
```

을 다음으로 바꾼다:

```rust

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버(투표 멤버·learner·관찰 노드)여야
/// 한다. 아직 멤버십을 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다. 관찰 노드는
/// Raft RPC를 받지 않는다(`failed_precondition`).
pub struct PeerService {
```

`crates/opl-peer/src/server.rs`에서

```rust

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
        let certs = req
```

을 다음으로 바꾼다:

```rust

    /// 부른 노드의 id. 클러스터에서 빠진 노드는 `permission_denied`다.
    fn authorize<T>(&self, req: &Request<T>) -> Result<NodeId, Status> {
        let certs = req
```

`crates/opl-peer/src/server.rs`에서

```rust
        if uninitialized || members.contains(NodeId(id)) {
            Ok(())
        } else {
```

을 다음으로 바꾼다:

```rust
        if uninitialized || members.contains(NodeId(id)) {
            Ok(NodeId(id))
        } else {
```

`crates/opl-peer/src/server.rs`에서

```rust

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;
```

을 다음으로 바꾼다:

```rust

    type FollowStream = BoxStream<'static, Result<FollowFrame, Status>>;

    /// 관찰 노드에 이 노드의 상태를 스트림으로 보낸다. 리스 갱신처럼 Raft 쪽 일이라 `work`로 넘기지 않는다.
    async fn follow(
        &self,
        req: Request<FollowRequest>,
    ) -> Result<Response<Self::FollowStream>, Status> {
        let caller = self.authorize(&req)?;
        let frames = self
            .cluster
            .follow(caller, req.into_inner().since)
            .map_err(|e| Status::internal(e.to_string()))?;
        // 열린 스트림도 프레임마다 다시 허가한다: 지운 노드(멤버십과 관찰 노드 목록 모두에서 빠짐)는 그 뒤의 쓰기를
        // 받지 못하고 스트림이 끝난다(목록에서 빼는 프레임부터, `FollowGate`).
        let cluster = Arc::downgrade(&self.cluster);
        let mut gate = FollowGate::new(caller);
        let frames = frames.take_while(move |frame| {
            let admitted = cluster.upgrade().is_some_and(|c| gate.admits(&c, frame));
            if !admitted {
                tracing::info!(%caller, "closing the follow stream of a node removed from the cluster");
            }
            std::future::ready(admitted)
        });
        Ok(Response::new(frames.map(Ok).boxed()))
    }

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;
```

`crates/opl-peer/src/wire.rs`에서

```rust
use opl_cluster::types::{NodeId, TypeConfig};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::{NodeId, TypeConfig};
use serde::{Deserialize, Serialize};
```

`crates/opl-peer/src/wire.rs`에서

```rust
pub type IncomingReply = Result<(), IncomingError>;
```

을 다음으로 바꾼다:

```rust
pub type IncomingReply = Result<(), IncomingError>;

/// 관찰 노드가 투표 멤버에 거는 Follow. `since` 뒤의 상태를 받는다(`opl_cluster::FollowFrame` 스트림).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowRequest {
    pub since: u64,
}
```

`crates/opl-peer/tests/grpc.rs`에서

```rust

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
```

을 다음으로 바꾼다:

```rust

use futures_util::StreamExt;
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::VoteRequest;
use openraft::{BasicNode, ServerState, Vote};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig, FollowClient, FollowFrame, NodeRole};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
```

을 다음으로 바꾼다:

```rust
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}

/// 리더가 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`)을 `observers`(id, 클러스터 주소)로 바꾼다(없으면
/// 정보를 만든다).
async fn set_observers(leader: &Node, observers: &[(NodeId, String)]) {
    let status = ClusterStatus {
        observers: observers
            .iter()
            .map(|(id, addr)| (id.0, addr.clone()))
            .collect(),
    };
    match leader.cluster.list::<ClusterSpec>().unwrap().1.first() {
        Some(info) => {
            leader
                .cluster
                .update_status::<ClusterSpec>(info.id, &status)
                .await
                .unwrap();
        }
        None => {
            let mut info: ClusterInfo = Resource::new(
                "test",
                ClusterSpec {
                    ca_cert_pem: String::new(),
                    ca_key_pem: String::new(),
                },
            );
            info.status = status;
            leader.cluster.create(&info).await.unwrap();
        }
    }
}

/// 관찰 노드를 띄운다. 실제 길처럼 learner로 가입해 따라잡은 뒤 관찰 노드 목록에 적히고 멤버십에서 빠진다. 같은
/// 저장소로 관찰 노드로 다시 열려 gRPC Follow로 투표 멤버를 따르고, 쓰기는 gRPC로 리더에 전달한다.
async fn start_observer(ca: &ClusterCa, root: &TempDir, leader: &Node) -> Node {
    let mut learner = Node::start(ca, root).await;
    let id = learner.id();
    let joined = leader
        .cluster
        .request(LeaderRequest::AddLearner {
            id,
            addr: learner.addr.clone(),
        })
        .await
        .unwrap();
    // `AddLearner`는 learner가 받기를 기다리지 않는다. 자기를 더한 멤버십까지 적용하기 전에 멈추면 관찰 노드의
    // 저장소에 따를 투표 멤버가 없다(느린 CI에서 그랬다).
    learner.cluster.wait_revision(joined, WAIT).await.unwrap();
    learner.stop().await;
    drop(learner);

    let tls = tls_for(ca, id);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    // 목록에 적은 뒤 멤버십에서 뺀다(그 사이에도 거절되지 않게).
    set_observers(leader, &[(id, addr.clone())]).await;
    leader
        .cluster
        .request(LeaderRequest::Remove { id })
        .await
        .unwrap();

    let peers = Arc::new(Peers::new(&tls));
    let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
    config.write_timeout = WAIT;
    // learner의 저장소 작업이 아직 파일을 쥐고 있으면 잠시 뒤 다시 연다.
    let deadline = Instant::now() + WAIT;
    let cluster = loop {
        match Cluster::open_observer(config.clone(), peers.clone()).await {
            Err(opl_cluster::ClusterError::Store(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => break other.unwrap(),
        }
    };
    let cluster = Arc::new(cluster.with_forwarder(peers.clone()));
    let hv = Arc::new(MockHypervisor::default());
    let service = PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
    let (stop, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    Node {
        addr,
        cluster,
        peers,
        hv,
        state_dir: root.path().join(format!("{id}-state")),
        stop: Some(stop),
        server: Some(server),
    }
}

async fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 관찰 노드는 mTLS gRPC Follow로 투표 멤버의 상태를 받아 읽기에 답하고, 쓰기는 리더로 전달해 자기 쓰기를 바로 읽는다.
#[tokio::test]
async fn an_observer_follows_a_voter_over_grpc() {
    let (ca, root, nodes) = start(1).await;
    let before = volume("before");
    nodes[0].cluster.create(&before).await.unwrap();
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let o = &observer.cluster;
    assert!(o.raft().is_none());
    o.wait_revision(nodes[0].cluster.revision(), WAIT)
        .await
        .unwrap();
    assert!(o.get::<VolumeSpec>(before.id).unwrap().is_some());

    let after = volume("after");
    let revision = nodes[0].cluster.create(&after).await.unwrap();
    o.wait_revision(revision, WAIT).await.unwrap();
    assert!(o.get::<VolumeSpec>(after.id).unwrap().is_some());

    let mine = volume("mine");
    let revision = o.create(&mine).await.unwrap();
    assert!(o.revision() >= revision);
    assert!(o.get::<VolumeSpec>(mine.id).unwrap().is_some());
    assert!(
        nodes[0]
            .cluster
            .get::<VolumeSpec>(mine.id)
            .unwrap()
            .is_some()
    );
    o.renew_lease(WAIT).await.unwrap();
    wait_for("the observer knows the leader", || o.health().quorum).await;
    assert_eq!(o.members().leader, Some(nodes[0].id()));
    // 관찰 노드끼리도, 투표 멤버도 관찰 노드를 클러스터 노드로 안다(주소 찾기).
    wait_for("the leader lists the observer", || {
        nodes[0].cluster.members().addr(observer.id()) == Some(observer.addr.as_str())
    })
    .await;
    assert_eq!(
        o.members().addr(observer.id()),
        Some(observer.addr.as_str())
    );
}

/// 관찰 노드 목록에 든 노드는 피어로 받고, 목록에서도 빠진 노드는 거절한다. 관찰 노드는 Raft RPC를 받지 않고,
/// 관찰 노드에 건 Follow는 `Hello`만 오고 닫힌다.
#[tokio::test]
async fn observers_are_peers_but_not_raft_members() {
    let (ca, root, nodes) = start(1).await;
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let leader_addr = nodes[0].addr.clone();
    assert_eq!(
        observer.peers.ping(&leader_addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
    assert_eq!(
        nodes[0].peers.ping(&observer.addr, WAIT).await.unwrap(),
        observer.id()
    );

    // 관찰 노드에 온 Raft RPC
    let mut raft = GrpcNetwork::new(nodes[0].peers.clone())
        .new_client(observer.id(), &BasicNode::new(&observer.addr))
        .await;
    let err = raft
        .vote(
            VoteRequest::new(Vote::new(1, nodes[0].id()), None),
            RPCOption::new(WAIT),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a Raft member"), "{err}");

    // 관찰 노드에 건 Follow: 부른 쪽(투표 멤버)의 역할만 알리고 닫는다.
    let frames: Vec<FollowFrame> =
        FollowClient::follow(nodes[0].peers.as_ref(), observer.id(), &observer.addr, 0)
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
    assert!(
        matches!(
            frames.as_slice(),
            [FollowFrame::Hello {
                role: NodeRole::Voter,
                ..
            }]
        ),
        "{frames:?}"
    );

    // 목록에서 빼면(지운 노드) 거절한다.
    set_observers(&nodes[0], &[]).await;
    wait_for("the observer is unlisted", || {
        !nodes[0].cluster.members().contains(observer.id())
    })
    .await;
    let err = observer.peers.ping(&leader_addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err =
        match FollowClient::follow(observer.peers.as_ref(), nodes[0].id(), &leader_addr, 0).await {
            Ok(_) => panic!("a removed node may follow"),
            Err(e) => e,
        };
    assert!(err.to_string().contains("not a member"), "{err}");
}

/// 열린 Follow 스트림도 다시 허가한다: 관찰 노드 목록과 멤버십 모두에서 빠진 노드(지운 노드)의 스트림은 끊긴다(그
/// 뒤의 쓰기, CA 키 같은 비밀을 받지 못한다). 목록 캐시가 갱신되기를 기다리지 않고 곧바로 써도, 목록에서 빼는 묶음부터
/// 받지 못한다.
#[tokio::test]
async fn a_removed_observer_stops_receiving_the_feed() {
    let (ca, root, nodes) = start(1).await;
    let observer = start_observer(&ca, &root, &nodes[0]).await;
    let mut frames = FollowClient::follow(
        observer.peers.as_ref(),
        nodes[0].id(),
        &nodes[0].addr,
        nodes[0].cluster.revision(),
    )
    .await
    .unwrap();
    assert!(matches!(
        frames.next().await,
        Some(Ok(FollowFrame::Hello { .. }))
    ));
    assert!(matches!(
        tokio::time::timeout(WAIT, frames.next()).await.unwrap(),
        Some(Ok(FollowFrame::Progress { .. }))
    ));

    set_observers(&nodes[0], &[]).await;
    // 목록에서 뺀 쓰기와 그 뒤에 쓴 것은 받지 못하고 스트림이 끝난다.
    let secret = volume("after-removal");
    nodes[0].cluster.create(&secret).await.unwrap();
    let rest: Vec<_> = tokio::time::timeout(Duration::from_secs(5), frames.collect::<Vec<_>>())
        .await
        .expect("the stream of a removed observer was not closed");
    for frame in rest.into_iter().flatten() {
        assert!(
            frame
                .observers()
                .is_none_or(|o| o.contains_key(&observer.id())),
            "a removed observer received its removal: {frame:?}"
        );
        if let FollowFrame::Batch { events, .. } = frame {
            assert!(
                events.iter().all(|e| !matches!(
                    e,
                    opl_cluster::watch::WatchEvent::Put { resource, .. } if resource.id == secret.id
                )),
                "a removed observer received a later write"
            );
        }
    }
}
```

`crates/opl-types/src/cluster.rs`에서

```rust

use std::fmt;

use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use uuid::Uuid;
```

`crates/opl-types/src/cluster.rs`에서

```rust
#[serde(default)]
pub struct ClusterStatus {}
```

을 다음으로 바꾼다:

```rust
#[serde(default)]
pub struct ClusterStatus {
    /// Raft 밖에서 투표 멤버 하나를 따르는 관찰 노드(노드 id → 클러스터 주소). 리더의 `VoterKeeper`만 쓴다.
    /// 옛 노드는 모르는 필드를 무시한다.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub observers: BTreeMap<Uuid, String>,
}
```

`crates/opl-types/src/cluster.rs`에서

```rust

    #[test]
```

을 다음으로 바꾼다:

```rust

    /// 관찰 노드가 없으면 옛 모양(`{}`) 그대로 쓰고, 옛 모양도 읽는다.
    #[test]
    fn observers_are_optional_in_the_status() {
        assert_eq!(
            serde_json::to_string(&ClusterStatus::default()).unwrap(),
            "{}"
        );
        assert_eq!(
            serde_json::from_str::<ClusterStatus>("{}").unwrap(),
            ClusterStatus::default()
        );
        let id = Uuid::from_u128(7);
        let status = ClusterStatus {
            observers: BTreeMap::from([(id, "node-7:8007".to_string())]),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<ClusterStatus>(&json).unwrap(),
            status
        );
    }

    #[test]
```

`crates/opl-types/src/node.rs`에서

```rust
    "voter-limit",
];
```

을 다음으로 바꾼다:

```rust
    "voter-limit",
    "observer",
];
```

`crates/opl-types/src/node.rs`에서

```rust
                "lease-liveness",
                "voter-limit"
            ])
```

을 다음으로 바꾼다:

```rust
                "lease-liveness",
                "voter-limit",
                "observer"
            ])
```

`crates/oplctl/src/cli.rs`에서

```rust
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
```

을 다음으로 바꾼다:

```rust
                cell(&m["address"]),
                // 옛 서버는 `role`을 보내지 않는다(투표 멤버가 아니면 learner).
                m["role"]
                    .as_str()
                    .unwrap_or(if m["voter"] == true {
                        "voter"
                    } else {
                        "learner"
                    })
                    .into(),
                cell(&m["online"]),
```

`crates/oplctl/src/cli.rs`에서

```rust
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
```

을 다음으로 바꾼다:

```rust
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// ROLE은 서버가 보낸 `role`(관찰 노드 포함)이고, `role`이 없는 옛 서버는 `voter`로 voter·learner를 가린다.
    #[test]
    fn cluster_status_shows_observers() {
        let member = |name: &str, voter: bool, role: Option<&str>| {
            let mut m = json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": false,
            });
            if let Some(role) = role {
                m["role"] = json!(role);
            }
            m
        };
        let view = json!({
            "name": "lab",
            "ca_fingerprint": "ab:cd",
            "leader": "id-a",
            "node": "id-a",
            "members": [
                member("a", true, Some("voter")),
                member("b", true, Some("voter")),
                member("c", true, Some("voter")),
                member("l", false, Some("learner")),
                member("o", false, Some("observer")),
                member("x", false, None),
            ],
        });
        let mut out = Vec::new();
        print_cluster_status(&mut out, &view).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("o     id-o  o:8006   observer  true"), "{out}");
        assert!(out.contains("l     id-l  l:8006   learner   true"), "{out}");
        assert!(out.contains("x     id-x  x:8006   learner   true"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
```

`crates/opld/src/bootstrap.rs`에서

```rust
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit"
            ]
        );
        update_node_status(&cluster, |status| {
```

을 다음으로 바꾼다:

```rust
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer"
            ]
        );
        update_node_status(&cluster, |status| {
```

`crates/opld/src/bootstrap.rs`에서

```rust
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 옛 버전 리더가 status를 다시 쓰며 지운 기능 표시를 주기 갱신이 되살린다. 같으면 쓰지 않는다.
```

을 다음으로 바꾼다:

```rust
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 옛 버전 리더가 status를 다시 쓰며 지운 기능 표시를 주기 갱신이 되살린다. 같으면 쓰지 않는다.
```

`crates/opld/src/bootstrap.rs`에서

```rust
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
```

을 다음으로 바꾼다:

```rust
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
```

`crates/opld/src/bootstrap.rs`에서

```rust
                    "voter-limit".into(),
                ],
```

을 다음으로 바꾼다:

```rust
                    "voter-limit".into(),
                    "observer".into(),
                ],
```

`crates/opld/src/member.rs`에서

```rust
            learners: learners.iter().map(|n| (id(*n), String::new())).collect(),
        };
```

을 다음으로 바꾼다:

```rust
            learners: learners.iter().map(|n| (id(*n), String::new())).collect(),
            observers: Default::default(),
        };
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1870개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(cluster): gRPC Follow·관찰 노드 목록·API — 관찰 노드를 피어로 받고 리비전 읽기"
```

---

### Task 4: 관찰 노드 전환과 VoterKeeper

`Cluster::become_observer()`(Raft 끄기 → 한 트랜잭션에 META `node_mode = observer`·LOG 비우기·`dropped_log` 기록, vote는 남김 → 복제본)와 `become_member()`(복제본 멈춤 → `dropped_log`까지 적용했는지 확인 → 스냅샷 저장 → Raft 다시 시작), `open_node`(디스크의 모드로 열기), 전환 중 늦은 적용을 막는 `retired`. 모든 노드의 `RoleKeeper`(1초; 목록에 있는 learner는 관찰 노드로, 목록에 없는 관찰 노드는 멤버로, 30초 적용 진행이 없으면 Follow `Hello.role`로 확인). `VoterKeeper`에 `ToObserver`·`ToLearner`와 `STANDBY_LEARNERS = 2`(투표 멤버는 내리지 않음, 모든 노드가 `observer` 기능일 때만, 올리기는 AddLearner 뒤 목록에서 뺌), 리더는 목록에 있는 노드의 `AddVoter`를 거절, `LeaderRequest::Remove`는 관찰 목록에서도 뺀다. opld는 저장된 모드로 연다. 시험 `opl-cluster/tests/observers.rs`.

**Files:**
- Create: `crates/opl-cluster/src/roles.rs`
- Create: `crates/opl-cluster/tests/observers.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/health.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/store.rs`
- Modify: `crates/opl-cluster/src/voters.rs`
- Modify: `crates/opl-cluster/tests/scale.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
    let members = state.cluster.members();
    let learner_addr = members.learners.get(&NodeId(req.node)).cloned();
    let rejoin_ok = || -> Result<bool, ApiError> {
```

을 다음으로 바꾼다:

```rust
    let members = state.cluster.members();
    // 관찰 노드도 learner처럼 본다(같은 이름의 오프라인 노드만 다시 합류하고, 주소가 바뀌었으면 먼저 뺀다).
    let learner_addr = members
        .learners
        .get(&NodeId(req.node))
        .or_else(|| members.observers.get(&NodeId(req.node)))
        .cloned();
    let rejoin_ok = || -> Result<bool, ApiError> {
```

`crates/opl-api/tests/cluster.rs`에서

```rust
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::JoinTokenSpec;
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::{ClusterSpec, ClusterStatus, JoinTokenSpec};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert!(joined.status.online);
}
```

을 다음으로 바꾼다:

```rust
    assert!(joined.status.online);
}

/// 관찰 노드(Raft 밖, 클러스터 정보의 목록에 있음)도 learner처럼 다시 합류한다: 온라인이면 id를 가로챌 수 없고,
/// 오프라인이면 새 주소로 learner가 되며 목록에서 빠진다.
#[tokio::test]
async fn a_rejoining_observer_is_treated_like_a_learner() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (req, node) = join_request(&new_token(&env).await, "node2");
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
    // 키퍼가 관찰 노드로 내린 것처럼: 목록에 쓰고 멤버십에서 뺀다.
    let (_, infos) = env.cluster.list::<ClusterSpec>().unwrap();
    let info = &infos[0];
    let status = ClusterStatus {
        observers: [(node, "10.0.0.7:8007".to_string())].into(),
    };
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    env.cluster
        .request(opl_cluster::types::LeaderRequest::Remove { id: NodeId(node) })
        .await
        .unwrap();
    // `Remove`는 목록에서도 뺀다. 다시 목록에 둔다(멤버십 밖의 관찰 노드).
    env.cluster
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    assert!(env.cluster.members().observers.contains_key(&NodeId(node)));

    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    req.address = "10.0.0.8:8006".into();
    req.cluster_address = "10.0.0.8:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "already_member");

    mark_offline(&env, node).await;
    req.token = new_token(&env).await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = env.cluster.members();
    assert_eq!(
        members.learners.get(&NodeId(node)).map(String::as_str),
        Some("10.0.0.8:8007")
    );
    assert!(members.observers.is_empty());
    let (_, infos) = env.cluster.list::<ClusterSpec>().unwrap();
    assert!(infos[0].status.observers.is_empty());
}
```

`crates/opl-api/tests/health.rs`에서

```rust
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

을 다음으로 바꾼다:

```rust
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

`crates/opl-api/tests/health.rs`에서

```rust
        .unwrap();
    env.cluster
        .request(LeaderRequest::Remove { id })
        .await
```

을 다음으로 바꾼다:

```rust
        .unwrap();
    // 리더의 키퍼처럼 목록에 적은 뒤 `RemoveNodes`로 뺀다(`LeaderRequest::Remove`는 노드를 지우는 것이라 목록에서도
    // 뺀다).
    env.cluster
        .raft()
        .unwrap()
        .change_membership(openraft::ChangeMembers::RemoveNodes([id].into()), false)
        .await
```

`crates/opl-api/tests/health.rs`에서

```rust
    let observer = loop {
        match Cluster::open_observer(config.clone(), env.router.follower(id)).await {
            Err(ClusterError::Store(_)) if std::time::Instant::now() < deadline => {
```

을 다음으로 바꾼다:

```rust
    let observer = loop {
        match Cluster::open_observer(
            config.clone(),
            env.router.network(id),
            env.router.follower(id),
        )
        .await
        {
            Err(ClusterError::Store(_)) if std::time::Instant::now() < deadline => {
```

`crates/opl-cluster/src/cluster.rs` 전체:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftMetrics, RaftNetworkFactory};
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
use crate::types::{Command, LeaderRequest, NodeId, RawResource, Rejection, TypeConfig};
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
                Backend::Observer(Replica::start(config.node_id, &store, follow.clone())?)
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
        let replica = Replica::start(self.id, &self.store, follow)?;
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
                let replica = Replica::start(self.id, &self.store, follow)?;
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
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), ClusterError> {
        self.member()?
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
            list.insert(*id, node.addr.clone());
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
fn may_leave_raft(m: &RaftMetrics<NodeId, BasicNode>, id: NodeId) -> Result<(), String> {
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
            .initialize(BTreeMap::from([(leader.id(), BasicNode::new("node-1"))]))
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
            .add_learner(id, BasicNode::new("node-2"))
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
            .initialize(BTreeMap::from([(leader.id(), BasicNode::new("node-1"))]))
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
            .add_learner(id, BasicNode::new("node-2"))
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
            .initialize(BTreeMap::from([(leader.id(), BasicNode::new("node-1"))]))
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
            .add_learner(id, BasicNode::new("node-2"))
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
```

`crates/opl-cluster/src/leader.rs`에서

```rust
use openraft::{BasicNode, ChangeMembers, Raft, RaftMetrics};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust
use openraft::{BasicNode, ChangeMembers, Raft, RaftMetrics};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::Kind;
use redb::Database;
use serde::{Deserialize, Serialize};
```

`crates/opl-cluster/src/leader.rs`에서

```rust
use crate::lease::{ConfirmBatch, LeaseTable};
use crate::types::{LeaderRequest, NodeId, Rejection, Response, TypeConfig};
use crate::vip::VipTable;
```

을 다음으로 바꾼다:

```rust
use crate::lease::{ConfirmBatch, LeaseTable};
use crate::store::list_objects;
use crate::types::{Command, LeaderRequest, NodeId, Rejection, Response, TypeConfig};
use crate::vip::VipTable;
```

`crates/opl-cluster/src/leader.rs`에서

```rust
    pub(crate) confirm: ConfirmBatch,
    pub(crate) leases: Arc<LeaseTable>,
```

을 다음으로 바꾼다:

```rust
    pub(crate) confirm: ConfirmBatch,
    /// 이 노드의 저장소(클러스터 정보의 관찰 노드 목록을 읽는다)
    pub(crate) db: Arc<Database>,
    pub(crate) leases: Arc<LeaseTable>,
```

`crates/opl-cluster/src/leader.rs`에서

```rust
            }
            // learner가 아니면(빠졌거나 더한 적이 없으면) 따라잡기를 기다리지 않고 바로 거부한다.
            if membership.get_node(&id).is_none() {
                return Err(ClusterError::Rejected(Rejection::NotFound {
```

을 다음으로 바꾼다:

```rust
            }
            // learner가 아니면(빠졌거나 더한 적이 없으면) 따라잡기를 기다리지 않고 바로 거부한다. 관찰 노드 목록에
            // 있는 learner(관찰 노드로 내려가는 중)도 올리지 않는다(내려가며 로그를 지우는 노드가 투표 멤버가 되면 안
            // 된다).
            if membership.get_node(&id).is_none() || observers(&ctx.db)?.contains_key(&id) {
                return Err(ClusterError::Rejected(Rejection::NotFound {
```

`crates/opl-cluster/src/leader.rs`에서

```rust
                .map_err(|_| ClusterError::Timeout)?;
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
```

을 다음으로 바꾼다:

```rust
                .map_err(|_| ClusterError::Timeout)?;
            // 기다리는 사이 키퍼가 이 노드를 관찰 노드 목록에 적었을 수 있다(내려가며 로그를 지운다). 바로 앞에서 다시 본다.
            if observers(&ctx.db)?.contains_key(&id) {
                return Err(ClusterError::Rejected(Rejection::NotFound {
                    kind: Kind::Node,
                    id: id.0,
                }));
            }
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
```

`crates/opl-cluster/src/leader.rs`에서

```rust
            }
            let membership = m.membership_config.membership();
            let change = if membership.voter_ids().any(|v| v == id) {
                ChangeMembers::RemoveVoters(BTreeSet::from([id]))
            } else if membership.learner_ids().any(|v| v == id) {
                ChangeMembers::RemoveNodes(BTreeSet::from([id]))
            } else {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            };
            let deadline = tokio::time::Instant::now() + timeout;
            let index =
                retry_in_progress(timeout, || raft.change_membership(change.clone(), false))
                    .await?;
            // 두 단계 변경의 두 번째 단계보다 VoterKeeper가 joint를 먼저 마무리했으면(learner로 남긴다) 두 번째
            // 단계는 할 일이 없어 이 노드가 learner로 남는다. 다시 읽어 지운다.
            let m = raft.metrics().borrow().clone();
            if !m
                .membership_config
                .membership()
                .learner_ids()
                .any(|v| v == id)
            {
                return Ok(index);
            }
            let change = ChangeMembers::RemoveNodes(BTreeSet::from([id]));
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            retry_in_progress(remaining, || raft.change_membership(change.clone(), false)).await
        }
```

을 다음으로 바꾼다:

```rust
            }
            let deadline = tokio::time::Instant::now() + timeout;
            let index = remove_member(raft, &m, id, deadline).await?;
            // 관찰 노드 목록에서도 뺀다(멤버십에서 먼저 빼야 키퍼가 목록에서만 빠진 learner를 보지 않는다).
            let unlisted = unlist_observer(ctx, id, deadline).await?;
            Ok(unlisted
                .or(index)
                .unwrap_or_else(|| m.last_applied.map_or(0, |l| l.index)))
        }
    }
}

/// 노드를 멤버십에서 뺀다(learner로 남기지 않는다). 멤버십에 없으면 `None`이다.
async fn remove_member(
    raft: &Raft<TypeConfig>,
    m: &RaftMetrics<NodeId, BasicNode>,
    id: NodeId,
    deadline: tokio::time::Instant,
) -> Result<Option<u64>, ClusterError> {
    let membership = m.membership_config.membership();
    let change = if membership.voter_ids().any(|v| v == id) {
        ChangeMembers::RemoveVoters(BTreeSet::from([id]))
    } else if membership.learner_ids().any(|v| v == id) {
        ChangeMembers::RemoveNodes(BTreeSet::from([id]))
    } else {
        return Ok(None);
    };
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let index =
        retry_in_progress(remaining, || raft.change_membership(change.clone(), false)).await?;
    // 두 단계 변경의 두 번째 단계보다 VoterKeeper가 joint를 먼저 마무리했으면(learner로 남긴다) 두 번째
    // 단계는 할 일이 없어 이 노드가 learner로 남는다. 다시 읽어 지운다.
    let m = raft.metrics().borrow().clone();
    if !m
        .membership_config
        .membership()
        .learner_ids()
        .any(|v| v == id)
    {
        return Ok(Some(index));
    }
    let change = ChangeMembers::RemoveNodes(BTreeSet::from([id]));
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    retry_in_progress(remaining, || raft.change_membership(change.clone(), false))
        .await
        .map(Some)
}

/// 이 노드의 저장소에 있는 클러스터 정보와 그 관찰 노드 목록(클러스터 정보가 없으면 `None`)
fn cluster_info(
    db: &Database,
) -> Result<Option<(crate::types::RawResource, ClusterStatus)>, ClusterError> {
    let Some(info) = list_objects(db, Kind::Cluster)?.1.into_iter().next() else {
        return Ok(None);
    };
    let status = serde_json::from_value(info.status.clone())?;
    Ok(Some((info, status)))
}

/// 클러스터 정보의 관찰 노드 목록
fn observers(db: &Database) -> Result<std::collections::BTreeMap<NodeId, String>, ClusterError> {
    Ok(cluster_info(db)?
        .map(|(_, status)| {
            status
                .observers
                .into_iter()
                .map(|(id, addr)| (NodeId(id), addr))
                .collect()
        })
        .unwrap_or_default())
}

/// 클러스터 정보의 관찰 노드 목록에서 `id`를 뺀다. 목록에 없으면 `None`, 뺐으면 그 쓰기의 인덱스다. 다른 쓰기와
/// 겹치면(revision 불일치, 리더가 아직 적용하지 않은 쓰기) 다시 읽어 `deadline`까지 다시 한다.
async fn unlist_observer(
    ctx: &LeaderCtx,
    id: NodeId,
    deadline: tokio::time::Instant,
) -> Result<Option<u64>, ClusterError> {
    loop {
        let Some((info, mut status)) = cluster_info(&ctx.db)? else {
            return Ok(None);
        };
        if status.observers.remove(&id.0).is_none() {
            return Ok(None);
        }
        let command = Command::UpdateStatusIf {
            kind: Kind::Cluster,
            id: info.id,
            expected_revision: info.revision,
            status: serde_json::to_value(&status)?,
        };
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let resp = tokio::time::timeout(remaining, ctx.raft.client_write(command))
            .await
            .map_err(|_| ClusterError::Timeout)??;
        match resp.data.0 {
            Ok(index) => return Ok(Some(index)),
            Err(Rejection::RevisionMismatch { .. })
                if tokio::time::Instant::now() + IN_PROGRESS_RETRY_INTERVAL < deadline =>
            {
                tokio::time::sleep(IN_PROGRESS_RETRY_INTERVAL).await;
            }
            Err(rejection) => return Err(rejection.into()),
        }
```

`crates/opl-cluster/src/lib.rs`에서

```rust
mod replica;
mod state_machine;
```

을 다음으로 바꾼다:

```rust
mod replica;
mod roles;
mod state_machine;
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use log_store::LogStore;
pub use state_machine::StateMachine;
pub use vip::{VipLeases, VipRenewal};
pub use voters::{VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL, leader_manages_voters};
```

을 다음으로 바꾼다:

```rust
pub use log_store::LogStore;
pub use roles::{RoleKeeper, RoleKeeperConfig};
pub use state_machine::StateMachine;
pub use vip::{VipLeases, VipRenewal};
pub use voters::{
    OBSERVER, STANDBY_LEARNERS, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
    leader_manages_voters,
};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust

pub struct MemNetwork {
```

을 다음으로 바꾼다:

```rust

#[derive(Clone)]
pub struct MemNetwork {
```

`crates/opl-cluster/src/replica.rs` 전체:

```rust
//! 관찰 노드의 복제본(설계 §6). Raft 구성원이 아니고, 투표 멤버 하나에 Follow로 붙어 받은 스냅샷과 적용 묶음을
//! 자기 저장소에 쓴다. 쓰기와 리스 갱신은 `Cluster`가 리더로 전달한다.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::{BasicNode, StoredMembership};
use redb::{Database, ReadableDatabase};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cluster::ClusterError;
use crate::follow::{FollowClient, FollowFrame};
use crate::health::ApplyLag;
use crate::state_machine::{Objects, apply_feed, install_objects};
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::NodeId;
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, BasicNode>;

/// 붙은 노드에서 이만큼 아무 프레임도 오지 않으면 끊고 다시 붙는다(서버는 1초마다 `Progress`를 보낸다).
pub(crate) const FOLLOW_STALL: Duration = Duration::from_secs(3);

/// 다시 붙기 전에 쉬는 시간
const RECONNECT_DELAY: Duration = Duration::from_millis(100);

/// 리더가 돌려준 인덱스(전달한 쓰기·리스 갱신)를 붙은 노드가 이보다 오래 알리지 않으면(`Progress`·묶음의 적용 위치)
/// 뒤처진 것으로 보고 다른 투표 멤버로 옮긴다(살아 있어 `Progress`는 오지만 로그를 받지 못하는 노드).
pub(crate) const UPSTREAM_LAG_MAX: Duration = Duration::from_secs(3);

/// 연결 하나가 끝난 까닭
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// 붙지 못했거나 진행을 하나도 받지 못하고 끊겼다. 다음에는 다른 노드를 고른다.
    Failed,
    /// 진행을 받다가 끊겼다.
    Progressed,
    /// 붙은 노드가 리더다(리더를 모르고 골랐거나 그 노드가 리더가 됐다). 다른 투표 멤버로 옮긴다.
    Move,
    /// 붙은 노드가 리더보다 뒤처졌다. 다음에는 다른 노드를 고른다.
    Lagging,
}

/// 관찰 노드의 복제본. 복제 작업은 `start`에서 띄우고 `shutdown`으로 멈춘다.
#[derive(Clone)]
pub(crate) struct Replica {
    inner: Arc<Inner>,
}

struct Inner {
    id: NodeId,
    db: Arc<Database>,
    hub: Arc<WatchHub>,
    writer: StoreWriter,
    follow: Arc<dyn FollowClient>,
    status: Mutex<Status>,
    /// `shutdown`이 `true`로 바꾼다.
    stopped: watch::Sender<bool>,
    /// `follow_another`가 올린다. 붙은 노드를 버리고 다른 노드로 옮긴다.
    leave_upstream: watch::Sender<u64>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// 붙은 노드에게서 들은 것
#[derive(Debug, Clone, Default)]
pub(crate) struct Status {
    pub(crate) leader: Option<(NodeId, String)>,
    /// 붙을 후보(투표 멤버와 클러스터 주소)
    pub(crate) voters: BTreeMap<NodeId, String>,
    /// 지금 붙어 있는 노드
    pub(crate) upstream: Option<NodeId>,
    /// 붙은 노드가 마지막으로 알린 적용 인덱스
    pub(crate) upstream_applied: u64,
    /// 리더가 돌려준 가장 큰 인덱스(전달한 쓰기·리스 갱신). 이만큼 적용하지 못한 시간이 적용 지연이다.
    pub(crate) leader_applied: u64,
    /// 붙은 노드에서 마지막으로 진행(스냅샷·묶음·`Progress`)을 받은 때
    pub(crate) last_progress: Option<Instant>,
    /// 연결이 끝나 다시 붙은 횟수
    pub(crate) reconnects: u64,
}

impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다. 저장소의 적용 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
        id: NodeId,
        store: &Store,
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let voters = voter_addrs(&read_membership(&db)?);
        let inner = Arc::new(Inner {
            id,
            db,
            hub: store.watch_hub().clone(),
            writer: store.writer().clone(),
            follow,
            status: Mutex::new(Status {
                voters,
                ..Default::default()
            }),
            stopped: watch::channel(false).0,
            leave_upstream: watch::channel(0).0,
            task: Mutex::new(None),
        });
        let task = tokio::spawn(inner.clone().run());
        *inner.task.lock().unwrap() = Some(task);
        Ok(Self { inner })
    }

    pub(crate) fn status(&self) -> Status {
        self.inner.status.lock().unwrap().clone()
    }

    pub(crate) fn leader(&self) -> Option<(NodeId, String)> {
        self.inner.status.lock().unwrap().leader.clone()
    }

    /// 저장소에 적용된 마지막 멤버십
    pub(crate) fn membership(&self) -> Result<Membership, StoreError> {
        read_membership(&self.inner.db)
    }

    /// 리더를 묻는 쪽에 돌려줄 `NotLeader`
    pub(crate) fn not_leader(&self) -> ClusterError {
        let leader = self.leader();
        ClusterError::NotLeader {
            leader_id: leader.as_ref().map(|(id, _)| *id),
            leader_addr: leader.map(|(_, addr)| addr),
        }
    }

    /// 리더가 돌려준 인덱스(그때 리더가 적용했거나 커밋한 것)를 적는다. 붙은 노드가 이것을 오래 보내지 않으면
    /// 옮긴다.
    pub(crate) fn heard_from_leader(&self, index: u64) {
        let mut status = self.inner.status.lock().unwrap();
        status.leader_applied = status.leader_applied.max(index);
    }

    /// 붙은 노드를 버리고 다른 투표 멤버로 옮긴다(리더가 돌려준 인덱스를 기다리다 시간이 지났을 때).
    pub(crate) fn follow_another(&self) {
        self.inner.leave_upstream.send_modify(|n| *n += 1);
    }

    /// 복제 작업을 멈춘다. 작업이 저장소 스레드에 넣어 둔 적용(작업을 멈춰도 저장소 스레드는 한다)까지 끝난 뒤
    /// 돌아오므로, 그 뒤에 읽은 리비전은 저장소와 같고 더 움직이지 않는다.
    pub(crate) async fn shutdown(&self) {
        let task = self.inner.task.lock().unwrap().take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        // 저장소 스레드는 넣은 차례대로 하므로 빈 쓰기가 끝나면 앞서 넣은 적용도 끝났다(스레드가 멈췄으면 남은 것이 없다).
        let _ = self.inner.writer.run(|_| Ok(())).await;
        self.inner.stopped.send_replace(true);
    }

    /// `shutdown`하면 `true`가 되는 수신자
    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.inner.stopped.subscribe()
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        // 이번 차례에 붙지 못했거나 아무것도 받지 못한 노드. 진행을 받으면 비운다.
        let mut skip = BTreeSet::new();
        loop {
            let Some((to, addr)) = self.pick(&skip) else {
                skip.clear();
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            };
            let since = self.hub.revision();
            // 다른 노드에 붙지 못해 리더까지 왔으면 리더에 머문다.
            let may_move = skip.is_empty();
            let outcome = match self.follow.follow(to, &addr, since).await {
                Ok(frames) => self.consume(to, frames, may_move).await,
                Err(e) => {
                    tracing::debug!(upstream = %to, error = %e, "cannot follow");
                    Outcome::Failed
                }
            };
            {
                let mut status = self.status.lock().unwrap();
                status.upstream = None;
                status.reconnects += 1;
            }
            match outcome {
                Outcome::Failed | Outcome::Lagging => {
                    skip.insert(to);
                }
                Outcome::Progressed => skip.clear(),
                Outcome::Move => {}
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    }

    /// 붙을 노드: 리더가 아닌 투표 멤버 가운데 이 노드와의 rendezvous 해시가 가장 큰 것(관찰 노드가 고르게
    /// 퍼진다), 없으면 리더. `skip`에 든 노드는 건너뛴다.
    fn pick(&self, skip: &BTreeSet<NodeId>) -> Option<(NodeId, String)> {
        let status = self.status.lock().unwrap();
        let leader = status.leader.as_ref().map(|(id, _)| *id);
        let mut ranked: Vec<(&NodeId, &String)> = status
            .voters
            .iter()
            .filter(|(id, _)| Some(**id) != leader && **id != self.id)
            .collect();
        ranked.sort_by_key(|(id, _)| std::cmp::Reverse(rendezvous(self.id, **id)));
        ranked
            .into_iter()
            .map(|(id, addr)| (*id, addr.clone()))
            .chain(status.leader.clone())
            .find(|(id, _)| !skip.contains(id))
    }

    /// 스트림이 끝나거나 `FOLLOW_STALL` 동안 멈출 때까지 프레임을 적용한다. `may_move`면 붙은 노드가 리더일 때
    /// 다른 투표 멤버가 있으면 끊는다.
    async fn consume(
        &self,
        to: NodeId,
        mut frames: BoxStream<'static, Result<FollowFrame, ClusterError>>,
        may_move: bool,
    ) -> Outcome {
        let mut progressed = false;
        let ended = |progressed| {
            if progressed {
                Outcome::Progressed
            } else {
                Outcome::Failed
            }
        };
        let mut parts = Vec::new();
        // 스냅샷 조각을 받는 중이다(첫 `SnapshotPart`부터 `SnapshotEnd`까지).
        let mut in_snapshot = false;
        // 다음 `Batch`와 한 번에 적용할 나눠 온 묶음의 앞부분
        let mut batch_parts = Vec::new();
        // 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(붙을 때마다, 스냅샷을 설치할 때마다 새로 잰다)
        let mut lag = ApplyLag::default();
        let mut leave = self.leave_upstream.subscribe();
        leave.borrow_and_update();
        loop {
            if may_move && self.upstream_is_leader(to) {
                return Outcome::Move;
            }
            // 스냅샷이나 나눈 묶음을 받는 동안은 붙은 노드가 알린 적용이 움직이지 않는다(스냅샷이 오래 걸려도 끊지 않게).
            let receiving = in_snapshot || !batch_parts.is_empty();
            if !receiving && self.upstream_lag(&mut lag) > UPSTREAM_LAG_MAX {
                tracing::warn!(upstream = %to, "the followed node lags behind the leader; following another");
                return Outcome::Lagging;
            }
            let next = tokio::select! {
                next = tokio::time::timeout(FOLLOW_STALL, frames.next()) => next,
                _ = leave.changed(), if !receiving => {
                    // 붙은 노드는 리더의 인덱스를 알렸는데 이 노드가 늦게 적용하는 것이면 머문다.
                    if self.upstream_behind() {
                        tracing::warn!(upstream = %to, "a forwarded request was not applied in time; following another");
                        return Outcome::Lagging;
                    }
                    continue;
                }
            };
            let frame = match next {
                Ok(Some(Ok(frame))) => frame,
                Ok(Some(Err(e))) => {
                    tracing::debug!(upstream = %to, error = %e, "follow stream failed");
                    return ended(progressed);
                }
                Ok(None) => return ended(progressed),
                Err(_) => {
                    tracing::warn!(upstream = %to, "no progress from the followed node; following another");
                    return ended(progressed);
                }
            };
            // 스냅샷 조각은 `SnapshotEnd`까지, 나눈 묶음은 `Batch`까지 이어 와야 한다(서버는 그 사이에 다른 프레임을
            // 넣지 않는다). 아니면 모은 것을 버리고 다시 붙는다.
            let in_order = match frame {
                FollowFrame::SnapshotPart { .. } | FollowFrame::SnapshotEnd { .. } => {
                    batch_parts.is_empty()
                }
                FollowFrame::BatchPart { .. } | FollowFrame::Batch { .. } => !in_snapshot,
                FollowFrame::Hello { .. } | FollowFrame::Progress { .. } => !receiving,
            };
            if !in_order {
                tracing::warn!(upstream = %to, "the followed node sent frames out of order; following again");
                return ended(progressed);
            }
            let result = match frame {
                FollowFrame::Hello { leader, voters, .. } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream = Some(to);
                    status.leader = leader;
                    if !voters.is_empty() {
                        status.voters = voters;
                    }
                    continue;
                }
                FollowFrame::SnapshotPart { objects } => {
                    in_snapshot = true;
                    parts.extend(objects);
                    Ok(())
                }
                FollowFrame::BatchPart { events } => {
                    batch_parts.extend(events);
                    Ok(())
                }
                FollowFrame::SnapshotEnd {
                    applied,
                    membership,
                } => {
                    in_snapshot = false;
                    let result = self
                        .install(applied, membership, std::mem::take(&mut parts))
                        .await;
                    // 설치하는 동안 늦은 전달 기다림이나 지연은 붙은 노드 탓이 아니다. 새로 잰다.
                    lag = ApplyLag::default();
                    leave.borrow_and_update();
                    result
                }
                FollowFrame::Batch {
                    events,
                    applied,
                    membership,
                } => {
                    batch_parts.extend(events);
                    self.apply(FeedBatch {
                        events: std::mem::take(&mut batch_parts),
                        applied,
                        membership,
                    })
                    .await
                }
                FollowFrame::Progress {
                    applied,
                    leader,
                    leader_addr,
                } => {
                    let mut status = self.status.lock().unwrap();
                    status.upstream_applied = applied.map_or(0, |l| l.index);
                    status.leader = leader.zip(leader_addr);
                    Ok(())
                }
            };
            if let Err(e) = result {
                tracing::error!(upstream = %to, error = %e, "cannot write the followed state");
                return ended(progressed);
            }
            progressed = true;
            self.status.lock().unwrap().last_progress = Some(Instant::now());
        }
    }

    /// 리더가 돌려준 인덱스를 붙은 노드가 알리지 않은 시간(이 노드의 적용이 아니라 붙은 노드가 알린 적용으로 잰다:
    /// 이 노드의 디스크가 느린 것을 붙은 노드 탓으로 돌리지 않게)
    fn upstream_lag(&self, lag: &mut ApplyLag) -> Duration {
        let status = self.status.lock().unwrap();
        lag.observe(
            Some(status.leader_applied),
            Some(status.upstream_applied),
            Instant::now(),
        )
    }

    /// 붙은 노드가 알린 적용이 리더가 돌려준 인덱스보다 뒤다.
    fn upstream_behind(&self) -> bool {
        let status = self.status.lock().unwrap();
        status.upstream_applied < status.leader_applied
    }

    /// 붙은 노드가 리더이고 붙을 다른 투표 멤버가 있다.
    fn upstream_is_leader(&self, to: NodeId) -> bool {
        let status = self.status.lock().unwrap();
        status.leader.as_ref().is_some_and(|(l, _)| *l == to)
            && status.voters.keys().any(|v| *v != to && *v != self.id)
    }

    async fn install(
        &self,
        applied: Option<openraft::LogId<NodeId>>,
        membership: Membership,
        objects: Objects,
    ) -> Result<(), StoreError> {
        let voters = voter_addrs(&membership);
        let revision = applied.map_or(0, |l| l.index);
        // 붙은 노드가 이 노드보다 뒤다(뒤처진 노드에 붙은 뒤 그 노드가 스냅샷을 설치했다). 설치하면 이미 돌려준 리비전
        // 읽기·리스 갱신이 거짓이 되므로 두고, 이어 오는 묶음에서 이미 적용한 것은 건너뛴다. 같은 리비전이면 상태가
        // 같으므로 설치하지 않는다(구독자에게 헛된 `Reset`을 내지 않게).
        if revision <= self.hub.revision() {
            tracing::warn!(
                revision,
                applied = self.hub.revision(),
                "ignoring a snapshot not newer than this node's state"
            );
            return Ok(());
        }
        // 구독자에게 알리는 것도 저장소 스레드의 쓰기 안에서 한다(전환의 쓰기와 차례가 어긋나지 않게).
        let hub = self.hub.clone();
        self.writer
            .run(move |db| {
                install_objects(db, applied, membership, objects)?;
                hub.reset(revision);
                Ok(())
            })
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(revision);
        if !voters.is_empty() {
            status.voters = voters;
        }
        Ok(())
    }

    async fn apply(&self, mut batch: FeedBatch) -> Result<(), StoreError> {
        // 다시 붙은 뒤 이미 적용한 묶음이 올 수 있다.
        if batch.applied.index <= self.hub.revision() {
            return Ok(());
        }
        // 투표 멤버마다 묶음 경계가 달라, 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 변경을 품을 수 있다. 그것은 쓰지도
        // 내지도 않는다(옛 값으로 되돌리거나 감시에 두 번 보이지 않게). 적용 위치는 묶음의 것이다.
        let applied_here = self.hub.revision();
        batch.events.retain(|e| e.revision() > applied_here);
        let (applied, voters) = (
            batch.applied.index,
            batch.membership.as_ref().map(voter_addrs),
        );
        let hub = self.hub.clone();
        self.writer
            .run(move |db| {
                apply_feed(db, &batch)?;
                hub.publish(batch);
                Ok(())
            })
            .await?;
        let mut status = self.status.lock().unwrap();
        status.upstream_applied = status.upstream_applied.max(applied);
        if let Some(voters) = voters.filter(|v| !v.is_empty()) {
            status.voters = voters;
        }
        Ok(())
    }
}

/// 같은 관찰 노드는 늘 같은 순서로, 관찰 노드마다는 다른 순서로 투표 멤버를 고르게 하는 점수
fn rendezvous(me: NodeId, voter: NodeId) -> u64 {
    let mut h = DefaultHasher::new();
    (me, voter).hash(&mut h);
    h.finish()
}

fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default())
}

fn voter_addrs(m: &Membership) -> BTreeMap<NodeId, String> {
    let m = m.membership();
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use openraft::BasicNode;
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tempfile::TempDir;

    use super::*;
    use crate::cluster::{Cluster, ClusterConfig};
    use crate::memnet::MemRouter;
    use crate::types::LeaderRequest;
    use crate::watch::{HISTORY, WatchError, WatchEvent};

    const WAIT: Duration = Duration::from_secs(10);

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

    fn config(root: &TempDir, i: u64, in_memory: bool) -> ClusterConfig {
        let mut config = ClusterConfig::new(NodeId::from(i), root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.in_memory = in_memory;
        config
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !cond() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 투표 멤버 1..=3(메모리 저장소)과 관찰 노드 4..(파일 저장소). 관찰 노드는 실제 길처럼 learner로 가입해
    /// 따라잡은 뒤 멤버십에서 빠지고, 같은 저장소로 관찰 노드로 다시 열린다.
    struct Net {
        router: MemRouter,
        root: TempDir,
        voters: BTreeMap<NodeId, Cluster>,
        observers: BTreeMap<NodeId, Cluster>,
    }

    impl Net {
        async fn start(observers: u64) -> Self {
            Self::start_with(observers, |_| {}).await
        }

        /// 투표 멤버의 Raft 설정을 `raft`로 고쳐 띄운다.
        async fn start_with(observers: u64, raft: impl Fn(&mut openraft::Config)) -> Self {
            let mut net = Net {
                router: MemRouter::default(),
                root: TempDir::new().unwrap(),
                voters: BTreeMap::new(),
                observers: BTreeMap::new(),
            };
            for i in 1..=3 {
                let id = NodeId::from(i);
                let mut config = config(&net.root, i, true);
                raft(&mut config.raft);
                let c = Cluster::open(config, net.router.network(id))
                    .await
                    .unwrap()
                    .with_forwarder(net.router.forwarder(id));
                net.router.register(id, &c);
                net.voters.insert(id, c);
            }
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
                .collect();
            net.voters[&NodeId::from(1)]
                .initialize(members)
                .await
                .unwrap();
            for i in 4..4 + observers {
                net.add_observer(i).await;
            }
            net
        }

        fn leader(&self) -> &Cluster {
            self.voters
                .values()
                .find(|c| c.members().leader == Some(c.id()))
                .expect("a leader")
        }

        async fn settled_leader(&self) -> NodeId {
            wait_until("a leader", || {
                self.voters
                    .values()
                    .any(|c| c.members().leader == Some(c.id()))
            })
            .await;
            self.leader().id()
        }

        async fn add_observer(&mut self, i: u64) {
            let id = NodeId::from(i);
            self.settled_leader().await;
            let learner = Cluster::open(config(&self.root, i, false), self.router.network(id))
                .await
                .unwrap();
            self.router.register(id, &learner);
            self.leader()
                .add_learner(id, BasicNode::new(format!("node-{i}")))
                .await
                .unwrap();
            self.leader()
                .request(LeaderRequest::Remove { id })
                .await
                .unwrap();
            self.router.unregister(id);
            learner.shutdown().await.unwrap();
            drop(learner);
            let observer = loop {
                match Cluster::open_observer(
                    config(&self.root, i, false),
                    self.router.network(id),
                    self.router.follower(id),
                )
                .await
                {
                    Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    other => break other.unwrap(),
                }
            }
            .with_forwarder(self.router.forwarder(id));
            self.router.register(id, &observer);
            self.observers.insert(id, observer);
        }

        fn observer(&self, n: usize) -> &Cluster {
            self.observers.values().nth(n).unwrap()
        }

        /// 모든 관찰 노드가 리더의 지금 리비전까지 적용하기를 기다린다.
        async fn sync(&self) -> u64 {
            let revision = self.leader().revision();
            for o in self.observers.values() {
                o.wait_revision(revision, WAIT).await.unwrap();
            }
            revision
        }

        async fn stop_voter(&mut self, id: NodeId) {
            self.router.unregister(id);
            self.voters.remove(&id).unwrap().shutdown().await.unwrap();
        }

        /// 모든 투표 멤버의 기록이 `revision`을 지나칠 때까지 리더에 쓴다.
        async fn write_past_history(&self, revision: u64) {
            let mut n = 0;
            while self.voters.values().any(|v| {
                !matches!(
                    v.node_handle().hub().feed(revision),
                    Err(WatchError::Expired { .. })
                )
            }) {
                for _ in 0..64 {
                    self.leader()
                        .create(&volume(&format!("bulk-{n}")))
                        .await
                        .unwrap();
                    n += 1;
                }
                assert!(n < 4 * HISTORY, "history never expired");
            }
        }

        async fn shutdown(self) {
            for c in self.observers.values().chain(self.voters.values()) {
                c.shutdown().await.unwrap();
            }
        }
    }

    fn replica(c: &Cluster) -> Replica {
        match c.node_handle().backend() {
            crate::cluster::Backend::Observer(r) => r,
            crate::cluster::Backend::Member { .. } => panic!("not an observer"),
        }
    }

    async fn next_events(w: &mut crate::watch::Watch, n: usize) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap());
        }
        out
    }

    /// 관찰 노드는 Raft 밖에서 투표 멤버와 같은 객체를 같은 `list` 리비전으로 보인다.
    #[tokio::test]
    async fn observers_serve_the_same_objects_at_the_same_list_revision() {
        let net = Net::start(2).await;
        let vols: Vec<Volume> = (0..3).map(|i| volume(&format!("v{i}"))).collect();
        for v in &vols {
            net.leader().create(v).await.unwrap();
        }
        let gone = vols[0].id;
        net.leader().delete::<VolumeSpec>(gone, None).await.unwrap();
        net.sync().await;
        let expected = net.leader().list::<VolumeSpec>().unwrap();
        for o in net.observers.values() {
            assert!(o.raft().is_none());
            assert_eq!(o.list::<VolumeSpec>().unwrap(), expected);
            assert!(o.get::<VolumeSpec>(gone).unwrap().is_none());
            assert!(o.get_by_name::<VolumeSpec>("v0").unwrap().is_none());
            assert_eq!(
                o.get_by_name::<VolumeSpec>("v1").unwrap().map(|v| v.id),
                Some(vols[1].id)
            );
            // 멤버십은 저장소에 적용된 것, 리더는 붙은 노드가 알린 것이다.
            let members = o.members();
            assert_eq!(members.leader, Some(net.leader().id()));
            assert_eq!(members.voters.len(), 3);
            assert!(!members.contains(o.id()));
        }
        net.shutdown().await;
    }

    /// 관찰 노드에서 쓴 것은 돌아오자마자 그 노드에서 읽히고, 리스 갱신은 리더로 전달되어 성공한다.
    /// 리더로서 할 일은 아는 리더를 담은 `NotLeader`다.
    #[tokio::test]
    async fn an_observer_reads_its_own_writes_and_renews_its_lease() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let v = volume("mine");
        let revision = o.create(&v).await.unwrap();
        assert!(o.revision() >= revision);
        assert_eq!(
            o.get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
            revision
        );

        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        assert!(net.leader().leases().renewed.contains_key(&o.id()));
        assert!(o.leases().renewed.is_empty());
        let leader = net.leader().id();
        assert!(matches!(
            o.read_barrier().await,
            Err(ClusterError::NotLeader { leader_id: Some(l), leader_addr: Some(_) }) if l == leader
        ));
        assert!(matches!(
            o.execute_as_leader(LeaderRequest::RenewLease { node: o.id() }).await,
            Err(ClusterError::NotLeader { leader_id: Some(l), .. }) if l == leader
        ));

        // 헬스: 붙은 노드의 진행이 오고 리더를 안다.
        let health = o.health();
        assert_eq!(health.leader, Some(leader));
        assert!(health.quorum);

        // Raft 구성원이 아닌 노드에 건 Follow는 `Hello`(부른 쪽의 역할)만 오고 닫힌다.
        let frames: Vec<FollowFrame> = o.follow(NodeId::from(99), 0).unwrap().collect().await;
        assert!(
            matches!(
                frames.as_slice(),
                [FollowFrame::Hello { role: crate::follow::NodeRole::Observer, voters, .. }] if voters.len() == 3
            ),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// `shutdown`은 복제 작업이 저장소 스레드에 넣어 둔 적용까지 끝난 뒤 돌아온다: 멈춘 뒤 읽은 리비전이 저장소와 같고
    /// 그 뒤로 움직이지 않는다.
    #[tokio::test]
    async fn shutdown_waits_for_a_queued_apply() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        // 저장소 스레드를 붙잡아 다음 적용이 줄을 서게 한다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        let written = net.leader().create(&volume("queued")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            release.send(()).unwrap();
        });
        r.shutdown().await;
        let stopped_at = o.revision();
        releaser.await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(o.revision(), stopped_at);
        assert!(stopped_at >= written);
        let stored: Option<openraft::LogId<NodeId>> = {
            let txn = o.store().db().begin_read().unwrap();
            get_json(
                &txn.open_table(META).unwrap(),
                crate::store::KEY_LAST_APPLIED,
            )
            .unwrap()
        };
        assert_eq!(stored.map(|l| l.index), Some(stopped_at));
        net.shutdown().await;
    }

    /// 관찰 노드는 리더가 아닌 투표 멤버에 붙는다. 시작할 때 리더를 몰라 리더에 붙었으면 `Hello`를 보고 옮긴다.
    #[tokio::test]
    async fn observers_follow_voters_other_than_the_leader() {
        let net = Net::start(4).await;
        let leader = net.settled_leader().await;
        for o in net.observers.values() {
            let r = replica(o);
            wait_until("following a non-leader voter", || {
                r.status().upstream.is_some_and(|u| u != leader)
            })
            .await;
        }
        net.shutdown().await;
    }

    /// 붙은 투표 멤버가 죽으면 다른 투표 멤버로 자기 리비전부터 다시 붙는다. 기록 안이라 `Reset`이 없다.
    #[tokio::test]
    async fn an_observer_moves_to_another_voter_without_reset() {
        let mut net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        // 리더가 아닌 투표 멤버에 붙는다.
        assert_ne!(upstream, leader);
        let before = r.status().reconnects;
        let mut w = net.observer(0).watch(net.sync().await).unwrap();

        net.stop_voter(upstream).await;
        let mut written = Vec::new();
        for i in 0..3 {
            let v = volume(&format!("after-{i}"));
            net.leader().create(&v).await.unwrap();
            written.push(v.id);
        }
        let events = next_events(&mut w, 3).await;
        let ids: Vec<_> = events
            .iter()
            .map(|e| match e {
                WatchEvent::Put { resource, .. } => resource.id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, written);
        let status = r.status();
        assert!(status.reconnects > before);
        assert!(status.upstream.is_some_and(|u| u != upstream));
        net.shutdown().await;
    }

    /// 끊긴 동안 투표 멤버의 기록이 지나가면 다시 붙을 때 스냅샷을 받고 구독자는 `Reset`을 본다.
    #[tokio::test]
    async fn an_observer_past_the_history_gets_a_snapshot_and_reset() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        let mut w = o.watch(revision).unwrap();
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        assert_eq!(o.revision(), revision);
        net.router.heal(o.id());

        let reset = tokio::time::timeout(WAIT, w.next()).await.unwrap().unwrap();
        assert!(matches!(reset, WatchEvent::Reset { .. }), "{reset:?}");
        net.sync().await;
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 적용이 막혀 뒤처진 관찰 노드는 서버 쪽 피드가 `Lagged`로 끝나 다시 붙고 따라잡는다.
    #[tokio::test]
    async fn a_lagging_observer_reconnects_and_catches_up() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let r = replica(o);
        let revision = net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let before = r.status().reconnects;
        // 관찰 노드의 저장소 스레드를 붙잡는다.
        let (release, held) = std::sync::mpsc::channel::<()>();
        o.store()
            .writer()
            .submit(move |_| {
                let _ = held.recv();
            })
            .unwrap();
        net.write_past_history(revision + 64).await;
        release.send(()).unwrap();

        net.sync().await;
        assert!(r.status().reconnects > before);
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
    #[tokio::test]
    async fn observer_watch_events_match_the_voter() {
        let net = Net::start(2).await;
        let revision = net.sync().await;
        let mut watches: Vec<_> = std::iter::once(net.leader())
            .chain(net.observers.values())
            .map(|c| c.watch(revision).unwrap())
            .collect();
        let v = volume("w");
        net.leader().create(&v).await.unwrap();
        net.observer(1)
            .update_spec(
                v.id,
                None,
                BTreeMap::from([("a".into(), "b".into())]),
                &v.spec,
            )
            .await
            .unwrap();
        net.observer(0)
            .update_status::<VolumeSpec>(v.id, &Default::default())
            .await
            .unwrap();
        net.leader().delete::<VolumeSpec>(v.id, None).await.unwrap();

        let expected = next_events(&mut watches[0], 4).await;
        assert!(matches!(expected[3], WatchEvent::Deleted { .. }));
        for w in &mut watches[1..] {
            assert_eq!(next_events(w, 4).await, expected);
        }
        net.shutdown().await;
    }

    fn log_id(index: u64) -> openraft::LogId<NodeId> {
        openraft::LogId::new(openraft::CommittedLeaderId::new(1, NodeId::from(1)), index)
    }

    /// `revision`에 쓰인 볼륨
    fn raw(name: &str, revision: u64) -> crate::types::RawResource {
        let mut r: crate::types::RawResource =
            serde_json::from_value(serde_json::to_value(volume(name)).unwrap()).unwrap();
        r.revision = revision;
        r
    }

    fn put(r: &crate::types::RawResource) -> WatchEvent {
        WatchEvent::Put {
            kind: opl_types::resource::Kind::Volume,
            resource: r.clone(),
        }
    }

    fn hello() -> FollowFrame {
        FollowFrame::Hello {
            role: crate::follow::NodeRole::Observer,
            leader: Some((NodeId::from(1), "node-1".into())),
            voters: BTreeMap::from([(NodeId::from(1), "node-1".into())]),
        }
    }

    /// 정해 둔 프레임을 돌려주는 Follow 통로(투표 멤버마다 묶음 경계·스냅샷이 다른 경우를 만든다). 부를 때마다 다음
    /// 대본을 쓰고, `hold`인 대본은 끝에서 끊지 않고 머문다. 대본이 떨어지면 붙지 못한다.
    #[derive(Default)]
    struct Scripted {
        scripts: Mutex<std::collections::VecDeque<(Vec<FollowFrame>, bool)>>,
    }

    impl Scripted {
        fn then(self, frames: Vec<FollowFrame>, hold: bool) -> Self {
            self.scripts.lock().unwrap().push_back((frames, hold));
            self
        }
    }

    #[async_trait::async_trait]
    impl FollowClient for Scripted {
        async fn follow(
            &self,
            _to: NodeId,
            _addr: &str,
            _since: u64,
        ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
            let Some((frames, hold)) = self.scripts.lock().unwrap().pop_front() else {
                return Err(ClusterError::Unreachable("no more scripts".into()));
            };
            let frames = futures_util::stream::iter(frames.into_iter().map(Ok));
            Ok(if hold {
                frames.chain(futures_util::stream::pending()).boxed()
            } else {
                frames.boxed()
            })
        }
    }

    /// 대본대로 받는 관찰 노드(투표 멤버 1만 든 멤버십을 둔 빈 저장소라 그 노드를 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let config = config(root, 9, false);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let store = Store::open(&config.data_dir.join(crate::cluster::DB_FILE)).unwrap();
        let membership = Membership::new(
            None,
            openraft::Membership::new(
                vec![[NodeId::from(1)].into()],
                BTreeMap::from([(NodeId::from(1), BasicNode::new("node-1"))]),
            ),
        );
        let txn = store.db().begin_write().unwrap();
        {
            let mut meta = txn.open_table(META).unwrap();
            crate::store::put_json(&mut meta, KEY_LAST_MEMBERSHIP, &membership).unwrap();
        }
        txn.commit().unwrap();
        drop(store);
        let router = MemRouter::default();
        let script = Arc::new(script);
        loop {
            match Cluster::open_observer(
                config.clone(),
                router.network(NodeId::from(9)),
                script.clone(),
            )
            .await
            {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        }
    }

    /// 감시에서 더 오지 않을 때까지 받은 이벤트의 리비전
    async fn drain(w: &mut crate::watch::Watch) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        while let Ok(e) = tokio::time::timeout(Duration::from_millis(300), w.next()).await {
            out.push(e.unwrap());
        }
        out
    }

    /// 다른 투표 멤버로 옮기면 묶음 경계가 달라 새 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 관찰 노드는
    /// 그 이벤트를 다시 내지 않는다(감시 리비전이 엄격히 오른다).
    #[tokio::test]
    async fn events_already_applied_are_not_replayed_after_moving() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                false,
            )
            // 다른 투표 멤버는 11..=13을 한 묶음으로 적용했다.
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b), put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(13, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        o.shutdown().await.unwrap();
    }

    /// 자기보다 뒤인 상류가 보낸 스냅샷(더 낮은 리비전)은 설치하지 않는다: 상태와 리비전이 뒤로 가지 않고 `Reset`도
    /// 없다. 그 뒤 묶음은 이어 받는다.
    #[tokio::test]
    async fn an_older_snapshot_from_the_upstream_is_not_installed() {
        let root = TempDir::new().unwrap();
        let (x, y, z, w) = (raw("x", 5), raw("y", 20), raw("z", 14), raw("w", 21));
        let snapshot = |objects: Vec<&crate::types::RawResource>, applied| {
            vec![
                FollowFrame::SnapshotPart {
                    objects: objects
                        .into_iter()
                        .map(|r| (opl_types::resource::Kind::Volume, r.clone()))
                        .collect(),
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(applied)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot(vec![&x], 10));
        first.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(20),
            membership: None,
        });
        let mut second = vec![hello()];
        second.extend(snapshot(vec![&x, &z], 15));
        second.push(FollowFrame::Batch {
            events: vec![put(&w)],
            applied: log_id(21),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(20, WAIT).await.unwrap();
        let mut watch = o.watch(20).unwrap();
        o.wait_revision(21, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == w.id),
            "{events:?}"
        );
        assert!(o.get::<VolumeSpec>(y.id).unwrap().is_some());
        assert!(o.get::<VolumeSpec>(z.id).unwrap().is_none());
        assert_eq!(o.revision(), 21);
        o.shutdown().await.unwrap();
    }

    /// 자기와 같은 리비전의 스냅샷도 설치하지 않는다(상태가 같다): 구독자는 헛된 `Reset` 없이 다음 묶음만 본다.
    #[tokio::test]
    async fn a_snapshot_at_the_same_revision_is_not_installed_again() {
        let root = TempDir::new().unwrap();
        let (x, y) = (raw("x", 10), raw("y", 11));
        let snapshot = || {
            vec![
                FollowFrame::SnapshotPart {
                    objects: vec![(opl_types::resource::Kind::Volume, x.clone())],
                },
                FollowFrame::SnapshotEnd {
                    applied: Some(log_id(10)),
                    membership: Default::default(),
                },
            ]
        };
        let mut first = vec![hello()];
        first.extend(snapshot());
        let mut second = vec![hello()];
        second.extend(snapshot());
        second.push(FollowFrame::Batch {
            events: vec![put(&y)],
            applied: log_id(11),
            membership: None,
        });
        let script = Scripted::default().then(first, false).then(second, true);
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut watch = o.watch(10).unwrap();
        o.wait_revision(11, WAIT).await.unwrap();
        let events = drain(&mut watch).await;
        assert!(
            matches!(events.as_slice(), [WatchEvent::Put { resource, .. }] if resource.id == y.id),
            "{events:?}"
        );
        o.shutdown().await.unwrap();
    }

    /// 투표 멤버는 자기보다 앞선 관찰 노드(`since`가 자기 리비전보다 큼)를 받지 않는다: `Hello`만 보내고 닫아 다른
    /// 투표 멤버로 가게 한다(뒤처진 노드가 그 뒤 스냅샷을 설치하면 관찰 노드가 뒤로 가므로).
    #[tokio::test]
    async fn a_voter_turns_away_an_observer_ahead_of_it() {
        let net = Net::start(0).await;
        let leader = net.settled_leader().await;
        let voter = &net.voters[&leader];
        let frames: Vec<FollowFrame> = tokio::time::timeout(
            Duration::from_secs(3),
            voter
                .follow(NodeId::from(9), voter.revision() + 100)
                .unwrap()
                .collect(),
        )
        .await
        .expect("the stream was not closed");
        assert!(
            matches!(frames.as_slice(), [FollowFrame::Hello { .. }]),
            "{frames:?}"
        );
        net.shutdown().await;
    }

    /// 뒤처진 투표 멤버(Raft가 끊김)가 리더의 스냅샷을 설치해도, 그보다 앞선 관찰 노드는 뒤로 가지 않는다(그런 투표
    /// 멤버에 붙지 않는다).
    #[tokio::test]
    async fn an_observer_ahead_of_a_lagging_voter_never_goes_back() {
        let mut net = Net::start_with(1, |raft| {
            raft.max_in_snapshot_log_to_keep = 0;
            raft.purge_batch_size = 1;
        })
        .await;
        let leader = net.settled_leader().await;
        let r = replica(net.observer(0));
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        let lagging = *net
            .voters
            .keys()
            .find(|id| **id != leader && **id != upstream)
            .unwrap();
        net.router.block_raft(lagging);
        for i in 0..5 {
            net.leader()
                .create(&volume(&format!("old-{i}")))
                .await
                .unwrap();
        }
        // 리더가 스냅샷을 만들고 로그를 지워 뒤처진 노드는 스냅샷으로만 따라잡는다.
        let raft = net.leader().raft().unwrap();
        raft.trigger().snapshot().await.unwrap();
        let purged = raft
            .wait(Some(WAIT))
            .metrics(|m| m.purged.is_some(), "leader purged logs")
            .await
            .unwrap()
            .purged
            .unwrap()
            .index;
        for i in 0..20 {
            net.leader()
                .create(&volume(&format!("new-{i}")))
                .await
                .unwrap();
        }
        let ahead = net.sync().await;
        assert!(ahead > purged);
        let mut w = net.observer(0).watch(ahead).unwrap();

        // 붙은 노드를 잃은 관찰 노드는 리더가 아닌 투표 멤버인 뒤처진 노드를 먼저 시도한다(그 노드는 받지 않는다).
        let tried = net.router.follows(lagging);
        net.stop_voter(upstream).await;
        wait_until("the observer tried the lagging voter", || {
            net.router.follows(lagging) > tried
        })
        .await;
        net.router.heal(lagging);
        net.voters[&lagging]
            .wait_revision(ahead, WAIT)
            .await
            .unwrap();
        let v = volume("after");
        net.leader().create(&v).await.unwrap();
        net.sync().await;
        let o = net.observer(0);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        let events = drain(&mut w).await;
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, WatchEvent::Reset { .. })),
            "{events:?}"
        );
        assert_eq!(
            o.list::<VolumeSpec>().unwrap(),
            net.leader().list::<VolumeSpec>().unwrap()
        );
        net.shutdown().await;
    }

    /// 살아 있지만 뒤처진 투표 멤버(Raft가 끊겨 `Progress`만 보냄)에 붙은 관찰 노드는 리스 갱신의 적용 기다림이 시간
    /// 초과하거나 리더가 돌려준 인덱스보다 오래 뒤처지면 다른 투표 멤버로 옮겨, 갱신이 다시 성공한다.
    #[tokio::test]
    async fn an_observer_leaves_a_lagging_voter_and_renews_its_lease() {
        let net = Net::start(1).await;
        let leader = net.settled_leader().await;
        let o = net.observer(0);
        let r = replica(o);
        net.sync().await;
        wait_until("attached", || r.status().upstream.is_some()).await;
        let upstream = r.status().upstream.unwrap();
        assert_ne!(upstream, leader);
        net.router.block_raft(upstream);
        net.leader().create(&volume("unseen")).await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut failed = 0;
        while let Err(e) = o.renew_lease(Duration::from_secs(2)).await {
            failed += 1;
            assert!(
                tokio::time::Instant::now() < deadline,
                "renewals kept failing ({failed}): {e}"
            );
        }
        assert_ne!(r.status().upstream, Some(upstream));
        net.router.heal(upstream);
        net.shutdown().await;
    }

    /// 나눠 온 묶음(`BatchPart`… `Batch`)은 마지막 프레임에서 한 번에 적용한다. 그 전에 끊기면 앞부분은 버린다.
    #[tokio::test]
    async fn split_batches_are_applied_at_once() {
        let root = TempDir::new().unwrap();
        let (a, b, c) = (raw("a", 11), raw("b", 12), raw("c", 13));
        let snapshot = FollowFrame::SnapshotEnd {
            applied: Some(log_id(10)),
            membership: Default::default(),
        };
        let script = Scripted::default()
            // 앞부분만 오고 멈춘 뒤(진행 없음으로) 끊긴다.
            .then(
                vec![
                    hello(),
                    snapshot,
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&b)],
                    },
                    FollowFrame::Batch {
                        events: vec![put(&c)],
                        applied: log_id(13),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        // 앞부분만으로는 적용하지 않는다.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(o.revision(), 10);
        assert!(o.get::<VolumeSpec>(a.id).unwrap().is_none());
        o.wait_revision(13, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12, 13]);
        for r in [&a, &b, &c] {
            assert!(o.get::<VolumeSpec>(r.id).unwrap().is_some());
        }
        o.shutdown().await.unwrap();
    }

    /// 나눈 묶음 사이에 다른 프레임이 오면(서버는 그러지 않는다) 모은 앞부분을 버리고 다시 붙는다.
    #[tokio::test]
    async fn frames_out_of_order_drop_the_parts_and_reconnect() {
        let root = TempDir::new().unwrap();
        let (a, b) = (raw("a", 11), raw("b", 12));
        let script = Scripted::default()
            .then(
                vec![
                    hello(),
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::BatchPart {
                        events: vec![put(&a)],
                    },
                    FollowFrame::SnapshotEnd {
                        applied: Some(log_id(10)),
                        membership: Default::default(),
                    },
                    FollowFrame::Batch {
                        events: vec![put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            )
            .then(
                vec![
                    hello(),
                    FollowFrame::Batch {
                        events: vec![put(&a), put(&b)],
                        applied: log_id(12),
                        membership: None,
                    },
                ],
                true,
            );
        let o = scripted_observer(&root, script).await;
        o.wait_revision(10, WAIT).await.unwrap();
        let mut w = o.watch(10).unwrap();
        o.wait_revision(12, WAIT).await.unwrap();
        let revisions: Vec<u64> = drain(&mut w).await.iter().map(|e| e.revision()).collect();
        assert_eq!(revisions, [11, 12]);
        assert!(replica(&o).status().reconnects >= 1);
        o.shutdown().await.unwrap();
    }

    /// 스냅샷으로 따라잡는 관찰 노드는 스냅샷이 `UPSTREAM_LAG_MAX`보다 오래 걸려도(조각마다 1초) 리스 갱신이 도는
    /// 동안(리더가 돌려준 인덱스, 시간 초과한 적용 기다림) 붙은 노드를 버리지 않고 끝까지 받아 따라잡는다.
    #[tokio::test]
    async fn a_slow_snapshot_catch_up_is_not_cut_off_by_lag_checks() {
        let net = Net::start(1).await;
        let o = net.observer(0);
        let revision = net.sync().await;
        net.router.isolate(o.id());
        net.write_past_history(revision).await;
        let target = net.leader().revision();
        net.router.heal(o.id());
        net.router.slow_snapshots(o.id(), Duration::from_secs(1));
        let renew = async {
            loop {
                let _ = o.renew_lease(Duration::from_secs(1)).await;
            }
        };
        tokio::select! {
            _ = renew => unreachable!(),
            caught_up = o.wait_revision(target, Duration::from_secs(40)) => {
                caught_up.expect("the observer never finished the snapshot");
            }
        }
        o.renew_lease(Duration::from_secs(5)).await.unwrap();
        net.shutdown().await;
    }
}
```

`crates/opl-cluster/src/roles.rs`를 만든다:

```rust
//! 노드 자신의 역할을 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`, 리더의 `VoterKeeper`가 씀)에
//! 맞춘다(설계 §6). 모든 노드가 띄운다.
//!
//! - learner인데 목록에 있으면 관찰 노드가 된다([`Cluster::become_observer`]).
//! - 관찰 노드인데 목록에 없고 저장된 멤버십에 있으면(리더가 learner로 더함) Raft 구성원이 된다
//!   ([`Cluster::become_member`]).
//! - 투표 멤버는 아무것도 하지 않는다.
//! - 대비책: 멤버 모드의 비투표 노드의 적용이 `stalled_after` 넘게 멈추면(리더를 모르는 것도) 투표 멤버에게
//!   Follow로 자기 역할을 묻고(`Hello.role`), 관찰 노드라 하면 관찰 노드가 된다(꺼진 동안 내려져 목록을 적용하지
//!   못한 노드). openraft 0.9.25는 커밋된 투표를 가진 learner가 다시 시작하면 리더 소식이 없어도 그 투표의 리더를
//!   `current_leader`로 계속 알리고 팔로워의 마지막 하트비트 시각을 지표에 내지 않으므로, "리더를 모름" 대신
//!   적용이 멈춘 것으로 본다. 쓰기가 없는 클러스터에서도 묻게 되지만 묻는 것은 Follow 하나(첫 프레임만)다.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::Cluster;
use crate::follow::NodeRole;

#[derive(Debug, Clone)]
pub struct RoleKeeperConfig {
    /// 살펴보는 간격
    pub interval: Duration,
    /// 멤버 모드의 비투표 노드의 적용이 이만큼 멈추면 투표 멤버에게 역할을 묻는다.
    pub stalled_after: Duration,
}

impl Default for RoleKeeperConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            stalled_after: Duration::from_secs(30),
        }
    }
}

/// 이 노드의 역할(관찰 노드 ↔ 멤버)을 목록에 맞춘다.
pub struct RoleKeeper {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RoleKeeper {
    pub fn spawn(cluster: Arc<Cluster>, config: RoleKeeperConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, config, rx)),
        }
    }

    /// 멈춘다. 진행 중인 전환은 끝까지 기다린다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 이번 틱에 할 전환
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    BecomeObserver,
    BecomeMember,
}

async fn run(cluster: Arc<Cluster>, config: RoleKeeperConfig, mut shutdown: oneshot::Receiver<()>) {
    // 멤버 모드의 비투표 노드의 적용 위치와 그것이 그대로인 첫 시각
    let mut stall = None;
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let step = tokio::select! {
            _ = &mut shutdown => return,
            step = decide(&cluster, &config, &mut stall) => step,
        };
        let Some(step) = step else {
            continue;
        };
        // 전환은 도중에 취소되면 안 되므로 따로 띄운 작업에서 하고 끝을 기다린다(종료 요청이 와도 끝까지 간다).
        let task = {
            let cluster = cluster.clone();
            tokio::spawn(async move {
                match step {
                    Step::BecomeObserver => cluster.become_observer().await,
                    Step::BecomeMember => cluster.become_member().await,
                }
            })
        };
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::info!(error = %e, ?step, "cannot change this node's role yet"),
            Err(e) => tracing::warn!(error = %e, ?step, "the role change task failed"),
        }
    }
}

async fn decide(
    cluster: &Cluster,
    config: &RoleKeeperConfig,
    stall: &mut Option<(u64, Instant)>,
) -> Option<Step> {
    let me = cluster.id();
    let listed = cluster.listed_as_observer();
    let Some(raft) = cluster.raft() else {
        *stall = None;
        let members = cluster.members();
        let in_raft = members.voters.contains_key(&me) || members.learners.contains_key(&me);
        return (!listed && in_raft).then_some(Step::BecomeMember);
    };
    let m = raft.metrics().borrow().clone();
    if m.membership_config
        .membership()
        .voter_ids()
        .any(|v| v == me)
    {
        *stall = None;
        return None;
    }
    if listed {
        *stall = None;
        return Some(Step::BecomeObserver);
    }
    let revision = cluster.revision();
    let leader_known = m.current_leader.is_some();
    let since = match *stall {
        Some((seen, since)) if seen == revision => since,
        _ => {
            *stall = Some((revision, Instant::now()));
            return None;
        }
    };
    if since.elapsed() < config.stalled_after {
        return None;
    }
    // 다음 물음은 다시 `stalled_after` 뒤에 한다.
    *stall = Some((revision, Instant::now()));
    match cluster.role_from_voters().await {
        Some(NodeRole::Observer) => {
            tracing::info!(
                leader_known,
                "this node's log has stalled and a voting member says it is an observer; following it"
            );
            Some(Step::BecomeObserver)
        }
        _ => None,
    }
}
```

`crates/opl-cluster/src/state_machine.rs` 전체:

```rust
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openraft::storage::RaftStateMachine;
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exclusive::exclusive_values;
use crate::store::{
    KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA, KEY_SNAPSHOT_META, META, NAMES,
    OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;
/// 상태 전체의 리소스(스냅샷 본문)
pub(crate) type Objects = Vec<(Kind, RawResource)>;

/// 스냅샷 본문의 머리. 본문 전체를 읽기 전에 형식 버전부터 확인한다.
#[derive(Deserialize)]
struct SnapshotHeader {
    schema_version: u32,
}

/// 스냅샷 본문. 상태 머신의 리소스 전체를 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
}

/// 리소스 상태 머신. 엔트리 묶음 하나를 redb 트랜잭션 하나로 적용하므로
/// 리소스와 `last_applied`는 항상 함께 기록된다. 쓰기는 저장소 스레드에서 돈다. 적용 묶음을 구독자에게 내는
/// 것도 같은 저장소 스레드의 쓰기 안에서 해, 저장소와 적용 리비전의 차례가 다른 쓰기(전환)와 어긋나지 않는다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
    /// 이 상태 머신을 쓰는 Raft가 계획된 전환으로 물러났다. openraft는 `shutdown` 뒤에도 상태 머신 작업이 남은
    /// 적용을 마저 할 수 있어, 전환이 저장소 스레드에서 이것을 세운 뒤의 적용·설치는 쓰지 않고 `Retired`다.
    retired: Arc<AtomicBool>,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>, writer: StoreWriter) -> Self {
        Self {
            db,
            watch,
            writer,
            retired: Arc::default(),
        }
    }

    /// 전환이 저장소 스레드의 쓰기 안에서 세우는 표시
    pub(crate) fn retired_flag(&self) -> Arc<AtomicBool> {
        self.retired.clone()
    }

    fn read_applied(&self) -> Result<(Option<LogId<NodeId>>, Membership), StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok((
            get_json(&meta, KEY_LAST_APPLIED)?,
            get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default(),
        ))
    }

    fn apply_entries(
        db: &Database,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Option<FeedBatch>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let mut applied = None;
        let mut membership = None;
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        let m = Membership::new(Some(entry.log_id), m);
                        put_json(&mut meta, KEY_LAST_MEMBERSHIP, &m)?;
                        membership = Some(m);
                        Ok(index)
                    }
                    EntryPayload::Normal(cmd) => {
                        apply_command(&mut objects, &mut names, cmd, index)?.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                };
                put_json(&mut meta, KEY_LAST_APPLIED, &entry.log_id)?;
                applied = Some(entry.log_id);
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        // 이벤트가 없는 묶음도 적용 위치를 알린다.
        let batch = applied.map(|applied| FeedBatch {
            events,
            applied,
            membership,
        });
        Ok((responses, batch))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷 본문을 만든다. 저장은 부른 쪽이 한다.
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let (last_log_id, last_membership, objects) = snapshot_objects(db)?;
        let data = serde_json::to_vec(&SnapshotData {
            schema_version: SCHEMA_VERSION,
            objects,
        })?;
        let meta = Meta {
            snapshot_id: format!(
                "{}-{}",
                last_log_id.map_or(0, |l| l.index),
                Uuid::new_v4().simple()
            ),
            last_log_id,
            last_membership,
        };
        Ok((meta, data))
    }

    /// 스냅샷을 저장한다. 이미 저장된 스냅샷이 같거나 더 새로우면 그대로 둔다.
    /// openraft는 스냅샷 빌드를 별도 태스크에서 돌리므로, 느린 빌드가 끝나기 전에
    /// 더 새 스냅샷이 설치될 수 있다.
    fn save_snapshot(db: &Database, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
            if let Some(existing) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)?
                && existing.last_log_id >= meta.last_log_id
            {
                return Ok(());
            }
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn current_snapshot(&self) -> Result<Option<Snapshot<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        let Some(meta) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)? else {
            return Ok(None);
        };
        let data = table
            .get(KEY_SNAPSHOT_DATA)?
            .map(|v| v.value().to_vec())
            .unwrap_or_default();
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }

    /// 리소스 전체를 스냅샷 내용으로 바꾼다. 리소스, 적용 위치, 스냅샷을 한 트랜잭션에 기록한다.
    fn install(db: &Database, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let header: SnapshotHeader = serde_json::from_slice(&data)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: header.schema_version,
            });
        }
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        let txn = db.begin_write()?;
        {
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            objects.retain(|_, _| false)?;
            names.retain(|_, _| false)?;
            for (kind, resource) in &snapshot.objects {
                objects.insert(
                    (kind.as_str(), resource.id.as_u128()),
                    serde_json::to_vec(resource)?.as_slice(),
                )?;
                names.insert(
                    (kind.as_str(), resource.name.as_str()),
                    resource.id.as_u128(),
                )?;
            }
            let mut table = txn.open_table(META)?;
            put_json(&mut table, KEY_LAST_APPLIED, &meta.last_log_id)?;
            put_json(&mut table, KEY_LAST_MEMBERSHIP, &meta.last_membership)?;
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// 한 읽기 트랜잭션에서 뜬 상태 전체: 적용 위치, 멤버십, 리소스.
pub(crate) fn snapshot_objects(
    db: &Database,
) -> Result<(Option<LogId<NodeId>>, Membership, Objects), StoreError> {
    let txn = db.begin_read()?;
    let meta_table = txn.open_table(META)?;
    let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
    let last_membership: Membership =
        get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
    let mut objects = Vec::new();
    for item in txn.open_table(OBJECTS)?.iter()? {
        let (key, value) = item?;
        let kind: Kind = serde_json::from_value(key.value().0.into())?;
        objects.push((kind, serde_json::from_slice(value.value())?));
    }
    Ok((last_log_id, last_membership, objects))
}

/// 지금 상태 전체를 스냅샷으로 저장한다(이미 저장된 스냅샷이 같거나 더 새로우면 그대로). 관찰 노드가 다시 Raft를
/// 띄우기 전에 부른다: 지운 로그 대신 openraft가 뒤처진 노드에 보낼 스냅샷이 적용 위치까지 있어야 한다.
pub(crate) fn save_current_snapshot(db: &Database) -> Result<(), StoreError> {
    let (meta, data) = StateMachine::snapshot_data(db)?;
    StateMachine::save_snapshot(db, &meta, &data)
}

/// 관찰 노드가 Follow로 받은 스냅샷(리소스 전체)을 `StateMachine::install`로 설치한다.
pub(crate) fn install_objects(
    db: &Database,
    applied: Option<LogId<NodeId>>,
    membership: Membership,
    objects: Objects,
) -> Result<(), StoreError> {
    let data = serde_json::to_vec(&SnapshotData {
        schema_version: SCHEMA_VERSION,
        objects,
    })?;
    let meta = Meta {
        snapshot_id: format!(
            "{}-{}",
            applied.map_or(0, |l| l.index),
            Uuid::new_v4().simple()
        ),
        last_log_id: applied,
        last_membership: membership,
    };
    StateMachine::install(db, &meta, data)
}

/// 관찰 노드가 Follow로 받은 적용 묶음을 쓴다. 리소스·이름, `last_applied`, 멤버십을 한 쓰기 트랜잭션에 기록한다.
/// 같은 묶음을 로컬 구독자에게 내는 것(`WatchHub::publish`)은 부른 쪽이 커밋 뒤에 한다.
pub(crate) fn apply_feed(db: &Database, batch: &FeedBatch) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    {
        let mut objects = txn.open_table(OBJECTS)?;
        let mut names = txn.open_table(NAMES)?;
        for event in &batch.events {
            match event {
                WatchEvent::Put { kind, resource } => {
                    let key = (kind.as_str(), resource.id.as_u128());
                    // 이름이 바뀐 리소스면 옛 이름 색인을 지운다.
                    let old: Option<RawResource> = match objects.get(key)? {
                        Some(v) => Some(serde_json::from_slice(v.value())?),
                        None => None,
                    };
                    if let Some(old) = old
                        && old.name != resource.name
                    {
                        names.remove((kind.as_str(), old.name.as_str()))?;
                    }
                    objects.insert(key, serde_json::to_vec(resource)?.as_slice())?;
                    names.insert(
                        (kind.as_str(), resource.name.as_str()),
                        resource.id.as_u128(),
                    )?;
                }
                WatchEvent::Deleted { kind, resource, .. } => {
                    objects.remove((kind.as_str(), resource.id.as_u128()))?;
                    names.remove((kind.as_str(), resource.name.as_str()))?;
                }
                // 피드 묶음에는 오지 않는다(스냅샷 설치는 `install_objects`).
                WatchEvent::Reset { .. } => {}
            }
        }
        let mut meta = txn.open_table(META)?;
        put_json(&mut meta, KEY_LAST_APPLIED, &batch.applied)?;
        if let Some(m) = &batch.membership {
            put_json(&mut meta, KEY_LAST_MEMBERSHIP, m)?;
        }
    }
    txn.commit()?;
    Ok(())
}

/// 명령 하나를 적용한다. 거부는 결정적이므로 모든 노드가 같은 결과를 낸다.
/// 바깥 `Err`는 저장소 오류이고, 안쪽 `Err`는 명령 거부다.
fn apply_command(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
) -> Result<Result<WatchEvent, Rejection>, StoreError> {
    let load = |objects: &Table<(&'static str, u128), &'static [u8]>,
                kind: Kind,
                id: Uuid|
     -> Result<Option<RawResource>, StoreError> {
        match objects.get((kind.as_str(), id.as_u128()))? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    };
    let check = |expected: Option<u64>, actual: u64| match expected {
        Some(expected) if expected != actual => {
            Err(Rejection::RevisionMismatch { expected, actual })
        }
        _ => Ok(()),
    };

    let (kind, resource) = match cmd {
        Command::Create { kind, mut resource } => {
            if load(objects, kind, resource.id)?.is_some() {
                return Ok(Err(Rejection::AlreadyExists {
                    kind,
                    id: resource.id,
                }));
            }
            if names
                .get((kind.as_str(), resource.name.as_str()))?
                .is_some()
            {
                return Ok(Err(Rejection::NameTaken {
                    kind,
                    name: resource.name,
                }));
            }
            if let Err(r) = check_exclusive(objects, kind, resource.id, &resource.spec)? {
                return Ok(Err(r));
            }
            names.insert(
                (kind.as_str(), resource.name.as_str()),
                resource.id.as_u128(),
            )?;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateSpec {
            kind,
            id,
            expected_revision,
            labels,
            spec,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            if let Err(r) = check_exclusive(objects, kind, id, &spec)? {
                return Ok(Err(r));
            }
            resource.labels = labels;
            resource.spec = spec;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatus { kind, id, status } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatusIf {
            kind,
            id,
            expected_revision,
            status,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(Some(expected_revision), resource.revision) {
                return Ok(Err(r));
            }
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::Delete {
            kind,
            id,
            expected_revision,
        } => {
            let Some(resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            objects.remove((kind.as_str(), id.as_u128()))?;
            names.remove((kind.as_str(), resource.name.as_str()))?;
            return Ok(Ok(WatchEvent::Deleted {
                revision: index,
                kind,
                resource,
            }));
        }
    };
    objects.insert(
        (kind.as_str(), resource.id.as_u128()),
        serde_json::to_vec(&resource)?.as_slice(),
    )?;
    Ok(Ok(WatchEvent::Put { kind, resource }))
}

/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다.
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec);
    if wanted.is_empty() {
        return Ok(Ok(()));
    }
    let in_use = |field: &str, value: &str, holder: Uuid| Rejection::InUse {
        kind,
        field: field.to_string(),
        value: value.to_string(),
        holder,
    };
    for (i, (field, value)) in wanted.iter().enumerate() {
        if wanted[..i].contains(&(field, value.clone())) {
            return Ok(Err(in_use(field, value, id)));
        }
    }
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (key, v) = item?;
        let other = Uuid::from_u128(key.value().1);
        if other == id {
            continue;
        }
        let other_spec: RawResource = serde_json::from_slice(v.value())?;
        for (field, value) in exclusive_values(kind, &other_spec.spec) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    Ok(Ok(()))
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    /// 큰 읽기는 `spawn_blocking`에서, 저장은 저장소 스레드에서 한다.
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let db = self.db.clone();
        let (meta, data) = tokio::task::spawn_blocking(move || Self::snapshot_data(&db))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        let (meta, data) = self
            .writer
            .run(move |db| Self::save_snapshot(db, &meta, &data).map(|()| (meta, data)))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, Membership), StorageError<NodeId>> {
        self.read_applied()
            .map_err(|e| StorageIOError::read_state_machine(&e).into())
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let (watch, retired) = (self.watch.clone(), self.retired.clone());
        let responses = self
            .writer
            .run(move |db| {
                if retired.load(Ordering::SeqCst) {
                    return Err(StoreError::Retired);
                }
                let (responses, batch) = Self::apply_entries(db, entries)?;
                if let Some(batch) = batch {
                    watch.publish(batch);
                }
                Ok(responses)
            })
            .await
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &Meta,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let (m, data) = (meta.clone(), snapshot.into_inner());
        let (watch, retired) = (self.watch.clone(), self.retired.clone());
        self.writer
            .run(move |db| {
                if retired.load(Ordering::SeqCst) {
                    return Err(StoreError::Retired);
                }
                Self::install(db, &m, data)?;
                watch.reset(m.last_log_id.map_or(0, |l| l.index));
                Ok(())
            })
            .await
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        self.current_snapshot()
            .map_err(|e| StorageIOError::read_snapshot(None, &e).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use openraft::CommittedLeaderId;

    fn meta_at(index: u64) -> Meta {
        Meta {
            last_log_id: Some(LogId::new(
                CommittedLeaderId::new(1, NodeId::from(1)),
                index,
            )),
            last_membership: Membership::default(),
            snapshot_id: format!("{index}"),
        }
    }

    /// openraft는 스냅샷을 다른 태스크에서 만들므로, 느린 빌드가 그 사이 설치된
    /// 더 새 스냅샷을 덮어쓰면 안 된다.
    #[test]
    fn older_snapshot_does_not_replace_newer_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        StateMachine::save_snapshot(&sm.db, &meta_at(10), b"new").unwrap();
        StateMachine::save_snapshot(&sm.db, &meta_at(5), b"old").unwrap();

        let snap = sm.current_snapshot().unwrap().unwrap();
        assert_eq!(snap.meta, meta_at(10));
        assert_eq!(snap.snapshot.into_inner(), b"new");
    }

    #[test]
    fn snapshot_with_newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        // 미래 형식은 본문 구조도 다를 수 있다. 헤더만 보고 거부해야 한다.
        let data = br#"{"schema_version": 99, "objects": {"format": "v99"}}"#.to_vec();
        assert!(matches!(
            StateMachine::install(&sm.db, &meta_at(3), data),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
```

`crates/opl-cluster/src/store.rs`에서

```rust
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
```

`crates/opl-cluster/src/store.rs`에서

```rust
pub(crate) const KEY_SNAPSHOT_DATA: &str = "snapshot_data";
```

을 다음으로 바꾼다:

```rust
pub(crate) const KEY_SNAPSHOT_DATA: &str = "snapshot_data";
/// 이 노드가 무엇으로 참여하는지([`NodeMode`]). 없으면 `Member`다(계획 48 전의 저장소).
pub(crate) const KEY_NODE_MODE: &str = "node_mode";
/// 관찰 노드가 되며 지운 Raft 로그의 마지막 인덱스(지금까지 가장 큰 것). 다시 Raft를 띄우기 전에 여기까지 적용한다
/// ([`enter_observer_mode`]).
pub(crate) const KEY_DROPPED_LOG: &str = "dropped_log";

/// 저장소를 어떤 백엔드로 여는지(설계 §6)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeMode {
    /// Raft 구성원(투표 멤버·learner)으로 Raft를 띄운다.
    #[default]
    Member,
    /// Raft 밖의 관찰 노드로 투표 멤버 하나를 따른다. Raft 로그는 비어 있다.
    Observer,
}
```

`crates/opl-cluster/src/store.rs`에서

```rust
    WriterPanicked,
}
```

을 다음으로 바꾼다:

```rust
    WriterPanicked,
    /// 계획된 전환(관찰 노드 ↔ 멤버)으로 물러난 Raft의 상태 머신이 쓰려 했다.
    #[error("the state machine was retired by a mode change")]
    Retired,
}
```

`crates/opl-cluster/src/store.rs`에서

```rust

pub(crate) fn get_object(
```

을 다음으로 바꾼다:

```rust

/// 저장된 모드. 없으면 `Member`다.
pub(crate) fn read_node_mode(db: &Database) -> Result<NodeMode, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_NODE_MODE)?.unwrap_or_default())
}

/// 관찰 노드가 된다: 한 트랜잭션에서 모드를 `Observer`로 적고 Raft 로그와 저장된 커밋 위치를 지운다. 투표(vote)는
/// 남긴다(다시 투표 멤버가 되어도 같은 임기에 두 번 투표하지 않게). 상태 머신(리소스·`last_applied`·멤버십·스냅샷)은
/// 그대로라 복제본이 그 적용 위치부터 이어 받는다. 커밋 위치를 지우는 것은 다시 Raft를 띄울 때 openraft가 지운
/// 로그를 다시 적용하려 하지 않게 하려는 것이다(없으면 적용 위치를 커밋으로 본다).
///
/// 지운 로그의 마지막 인덱스는 `KEY_DROPPED_LOG`에 남긴다. 리더는 이 노드가 받았다고 답한 로그까지를 진행
/// (`matching`)으로 쥐고, 이 노드가 멤버십에서 빠지기 전에 관찰 노드가 되면 그 진행을 그대로 쥔다. 다시 Raft를 띄운
/// 이 노드의 로그(적용 위치까지)가 그보다 짧으면 openraft 0.9.25 리더가 그것을 로그가 뒤로 간 것으로 본다(디버그
/// 빌드는 `follower log reversion` panic, 그 밖에는 이 노드가 따라잡지 못할 수 있다).
pub(crate) fn enter_observer_mode(db: &Database) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    {
        let mut meta = txn.open_table(META)?;
        put_json(&mut meta, KEY_NODE_MODE, &NodeMode::Observer)?;
        meta.remove(KEY_COMMITTED)?;
        let mut log = txn.open_table(LOG)?;
        let last = log.last()?.map_or(0, |(index, _)| index.value());
        let dropped = get_json::<u64>(&meta, KEY_DROPPED_LOG)?
            .unwrap_or(0)
            .max(last);
        put_json(&mut meta, KEY_DROPPED_LOG, &dropped)?;
        log.retain(|_, _| false)?;
    }
    txn.commit()?;
    Ok(())
}

/// 관찰 노드가 되며 지운 Raft 로그의 마지막 인덱스. 지운 적이 없으면 0이다.
pub(crate) fn read_dropped_log(db: &Database) -> Result<u64, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    Ok(get_json(&meta, KEY_DROPPED_LOG)?.unwrap_or(0))
}

/// 모드를 적는다.
pub(crate) fn write_node_mode(db: &Database, mode: NodeMode) -> Result<(), StoreError> {
    let txn = db.begin_write()?;
    put_json(&mut txn.open_table(META)?, KEY_NODE_MODE, &mode)?;
    txn.commit()?;
    Ok(())
}

pub(crate) fn get_object(
```

`crates/opl-cluster/src/voters.rs` 전체:

```rust
//! 투표 멤버 상한. 리더의 [`VoterKeeper`]가 투표 멤버를 클러스터 설정 `voters`(3·5·7, 기본 5)로 맞춘다.
//! 나머지 노드는 learner로 남는다(쓰기 확정과 선거는 투표 멤버 과반만 기다린다).
//!
//! 한 번에 하나만 바꾼다(죽은 투표 멤버의 교체는 변경 한 번). 바꾸기 전과 뒤의 구성 모두에서 살아 있는(최근
//! `alive_within` 안에 이 리더에 리스를 갱신한) 투표 멤버가 과반일 때만 바꾼다: 두 단계 변경의 중간(joint)
//! 구성은 두 구성의 과반이 모두 있어야 커밋된다. 고르는 순서는 [`plan`]에 있다. 모든 노드가 [`VOTER_LIMIT`]를
//! 알 때만 움직인다: 그 전에는 지금처럼 노드가 스스로 투표 멤버 승격을 요청한다(옛 노드, 롤링 업그레이드).
//!
//! openraft의 멤버십 변경은 joint를 커밋한 뒤 uniform을 제안하는 두 단계라, 그 사이에 끊기면(시간 초과, 리더
//! 교체) joint가 남는다. 키퍼는 남은 joint를 마무리한다.
//!
//! 관찰 노드(설계 §6): 투표 멤버와 대기 learner([`STANDBY_LEARNERS`]) 밖의 노드는 Raft 밖의 관찰 노드다. 키퍼는
//! 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`)을 쓴다. 내리기는 목록에 더한 뒤(노드의
//! `RoleKeeper`가 보고 관찰 노드가 된다) 다음 틱에 멤버십에서 뺀다(`RemoveNodes`). 올리기는 learner로 더한 뒤
//! 목록에서 뺀다(그 사이 끊기면 "목록에 있는 learner"라 다시 내려간다: 어느 쪽이든 목록과 멤버십 가운데 한 곳에는
//! 남는다). 투표 멤버는 목록에 두지 않는다(있으면 목록에서 뺀다).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{BasicNode, ChangeMembers, LogId, Raft, RaftMetrics, ServerState};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::node::{Node, NodeSpec};
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::{Cluster, ClusterError};
use crate::types::{NodeId, TypeConfig};

/// 이 기능을 아는 노드는 투표 멤버 승격을 리더의 `VoterKeeper`에 맡긴다. 모든 노드가 알면 리더가 상한을 지킨다.
pub const VOTER_LIMIT: &str = "voter-limit";

/// 이 기능을 아는 노드는 관찰 노드가 될 수 있다(설계 §6). 모든 노드가 알 때만 키퍼가 learner를 관찰 노드로 내린다.
pub const OBSERVER: &str = "observer";

/// 투표 멤버 밖에 learner로 남기는 건강한 노드 수(위트니스 제외). 나머지는 관찰 노드다. 투표 멤버가 죽으면 이것이
/// 먼저 올라가고, 줄면 관찰 노드를 learner로 올린다.
pub const STANDBY_LEARNERS: usize = 2;

/// 장애 영역을 적는 노드 레이블(노드 설정 `[node] zone`)
pub const ZONE_LABEL: &str = "zone";

/// learner가 리더의 마지막 로그에서 이만큼 안쪽이면 따라잡았다고 보고 올린다.
const CATCH_UP_SLACK: u64 = 64;

#[derive(Debug, Clone)]
pub struct VoterKeeperConfig {
    /// 살펴보는 간격
    pub interval: Duration,
    /// 마지막 리스 갱신이 이만큼 안이면 건강하다(올릴 수 있다). 새 임기의 처음 이만큼은 개수 때문에 내리지 않는다.
    pub healthy_within: Duration,
    /// 마지막 리스 갱신이 이만큼 안이면 살아 있다(과반을 셀 때). openraft 0.9.25의 지표에는 팔로워별 최근
    /// 응답 시각이 없어 리스 갱신(2초마다)으로 본다.
    pub alive_within: Duration,
    /// 투표 멤버의 마지막 리스 갱신이 이만큼 지나면 오프라인으로 보고 올릴 수 있는 learner로 바꾼다.
    pub offline_after: Duration,
    /// 멤버십 변경 하나를 기다리는 시간
    pub change_timeout: Duration,
}

impl Default for VoterKeeperConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            healthy_within: Duration::from_secs(10),
            alive_within: Duration::from_secs(3),
            offline_after: Duration::from_secs(30),
            change_timeout: Duration::from_secs(10),
        }
    }
}

/// 리더에서 투표 멤버를 상한에 맞춘다. 모든 노드가 띄우고, 리더일 때만 일한다.
pub struct VoterKeeper {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl VoterKeeper {
    pub fn spawn(cluster: Arc<Cluster>, config: VoterKeeperConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 멤버 하나에 대해 `plan`이 보는 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) id: NodeId,
    pub(crate) voter: bool,
    pub(crate) witness: bool,
    /// 노드 레이블 `zone`. 없는 노드끼리는 같은 영역으로 본다.
    pub(crate) zone: Option<String>,
    /// 최근(`healthy_within`)에 리스를 갱신했다.
    pub(crate) healthy: bool,
    /// 아주 최근(`alive_within`)에 이 리더에 리스를 갱신했다(리더 자신은 늘 참). 과반은 이것으로 센다.
    pub(crate) alive: bool,
    /// `offline_after` 넘게 리스를 갱신하지 않았다.
    pub(crate) offline: bool,
    /// learner가 로그를 따라잡았다(투표 멤버는 늘 참).
    pub(crate) caught_up: bool,
    /// Raft 밖의 관찰 노드(클러스터 정보의 목록에 있다)
    pub(crate) observer: bool,
}

/// 멤버십 변경 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// learner를 투표 멤버로 올린다.
    Promote(NodeId),
    /// 투표 멤버를 learner로 내린다.
    Demote(NodeId),
    /// 투표 멤버 `out`을 learner로 내리고 learner `by`를 올린다(변경 한 번).
    Replace { out: NodeId, by: NodeId },
    /// learner를 관찰 노드로 내린다(목록에 더한다. 멤버십에서 빼는 것은 다음 틱).
    ToObserver(NodeId),
    /// 관찰 노드를 learner로 올린다(learner로 더한 뒤 목록에서 뺀다).
    ToLearner(NodeId),
}

impl Change {
    /// 바꾼 뒤의 투표 멤버
    fn voters_after(self, before: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
        let mut after = before.clone();
        match self {
            Change::Promote(id) => {
                after.insert(id);
            }
            Change::Demote(id) => {
                after.remove(&id);
            }
            Change::Replace { out, by } => {
                after.remove(&out);
                after.insert(by);
            }
            Change::ToObserver(_) | Change::ToLearner(_) => {}
        }
        after
    }
}

/// 다음에 할 멤버십 변경 하나. 순서:
/// 1. 위트니스 learner를 올린다(위트니스는 늘 투표 멤버이고 상한에 센다).
/// 2. 투표 멤버가 `limit`보다 적으면 건강하고 따라잡은 learner를 올린다. 투표 멤버가 적은 zone의 것 먼저.
/// 3. `limit`보다 많으면 하나를 내린다: 오프라인, 살아 있지 않은 것, 투표 멤버가 많은 zone의 것, 건강하지 않은 것,
///    나중에 들어온 것 순. 새 임기의 처음(`settling`)에는 내리지 않는다(건강 정보가 아직 없다).
/// 4. `limit`만큼이면 오프라인 투표 멤버를 올릴 수 있는 learner로 바꾼다(변경 한 번). 그런 learner가 없으면
///    그대로 둔다: 내리기만 하면 투표 멤버가 줄어 하나만 더 죽어도 정족수가 깨진다(노드 둘과 위트니스에서 하나가
///    오래 꺼진 뒤 다른 하나가 죽는 경우). 노드가 상한 이하면 모두 투표 멤버로 남는다.
/// 5. 관찰 노드(설계 §6, 새 임기의 처음에는 하지 않음): 투표 멤버가 상한보다 적은데 올릴 learner가 없거나, 건강한
///    대기 learner(위트니스가 아닌 비투표 구성원)가 [`STANDBY_LEARNERS`]보다 적으면 건강한 관찰 노드 하나를
///    learner로 올린다(투표 멤버가 적은 zone의 것 먼저). 대기 learner가 그보다 많고 모든 노드가 [`OBSERVER`]를
///    알면(`observers_ready`) 건강한 대기 learner 하나를 관찰 노드로 내린다(대기 learner가 많은 zone의 것, 따라잡지
///    못한 것, 나중에 들어온 것 순). 오프라인 투표 멤버가 있으면 내리지 않는다.
///
/// 위트니스와 리더는 내리지 않는다. 투표 멤버는 관찰 노드로 내리지 않는다. 바꾸기 전이나 뒤의 투표 멤버에서 살아
/// 있는 것이 과반이 아니면 바꾸지 않는다.
pub(crate) fn plan(
    leader: NodeId,
    members: &[Member],
    limit: usize,
    settling: bool,
    observers_ready: bool,
) -> Option<Change> {
    let voters: Vec<&Member> = members.iter().filter(|m| m.voter).collect();
    let change = pick(leader, members, &voters, limit, settling, observers_ready)?;
    let before: BTreeSet<NodeId> = voters.iter().map(|m| m.id).collect();
    let after = change.voters_after(&before);
    (alive_majority(members, &before) && alive_majority(members, &after)).then_some(change)
}

/// `voters` 가운데 살아 있는 것이 과반이다.
fn alive_majority(members: &[Member], voters: &BTreeSet<NodeId>) -> bool {
    let alive = members
        .iter()
        .filter(|m| m.alive && voters.contains(&m.id))
        .count();
    alive * 2 > voters.len()
}

fn pick(
    leader: NodeId,
    members: &[Member],
    voters: &[&Member],
    limit: usize,
    settling: bool,
    observers_ready: bool,
) -> Option<Change> {
    let demotable = |m: &&Member| !m.witness && m.id != leader;
    let promotable = |m: &&Member| !m.voter && !m.observer && m.healthy && m.caught_up;
    if let Some(m) = members.iter().filter(promotable).find(|m| m.witness) {
        return Some(Change::Promote(m.id));
    }
    // `except`를 뺀 투표 멤버 가운데 `zone`의 것의 수
    let in_zone = |zone: &Option<String>, except: Option<NodeId>| {
        voters
            .iter()
            .filter(|v| v.zone == *zone && Some(v.id) != except)
            .count()
    };
    // 올릴 learner: 투표 멤버가 적은 zone의 것, 같으면 id가 작은 것
    let best = |except: Option<NodeId>| {
        members
            .iter()
            .filter(promotable)
            .min_by_key(|m| (in_zone(&m.zone, except), m.id))
            .map(|m| m.id)
    };
    // learner로 올릴 관찰 노드: 건강한 것 가운데 투표 멤버가 적은 zone의 것, 같으면 id가 작은 것. 새 임기의
    // 처음에는 건강 정보가 없어 올리지 않는다.
    let returning = members
        .iter()
        .filter(|m| m.observer && m.healthy && !settling)
        .min_by_key(|m| (in_zone(&m.zone, None), m.id))
        .map(|m| Change::ToLearner(m.id));
    if voters.len() < limit {
        return best(None).map(Change::Promote).or(returning);
    }
    if voters.len() > limit {
        if settling {
            return None;
        }
        let m = voters.iter().copied().filter(demotable).max_by_key(|m| {
            (
                m.offline,
                !m.alive,
                in_zone(&m.zone, None),
                !m.healthy,
                m.id,
            )
        })?;
        return Some(Change::Demote(m.id));
    }
    let dead = voters.iter().copied().filter(demotable).find(|m| m.offline);
    if let Some(dead) = dead
        && let Some(by) = best(Some(dead.id))
    {
        return Some(Change::Replace { out: dead.id, by });
    }
    if settling {
        return None;
    }
    // 대기 learner: 건강한 비투표 Raft 구성원(위트니스는 늘 투표 멤버로 올라가므로 세지 않는다)
    let standby: Vec<&Member> = members
        .iter()
        .filter(|m| !m.voter && !m.observer && !m.witness && m.healthy)
        .collect();
    if standby.len() < STANDBY_LEARNERS {
        return returning;
    }
    if standby.len() > STANDBY_LEARNERS && observers_ready && dead.is_none() {
        let in_standby_zone =
            |zone: &Option<String>| standby.iter().filter(|s| s.zone == *zone).count();
        return standby
            .iter()
            .max_by_key(|m| (in_standby_zone(&m.zone), !m.caught_up, m.id))
            .map(|m| Change::ToObserver(m.id));
    }
    None
}

/// [`VOTER_LIMIT`]를 보고하지 않아 키퍼를 막는 멤버. 합류하자마자 죽어 등록하지 못한 learner(Node 기록이 없거나
/// 기능·버전이 비었고, 이 임기에 리스도 없다)는 뺀다: 건강하지 않아 올리지 않고, 다시 떠서 등록하면 기능을
/// 보고한다.
fn blocking_members(
    members: impl Iterator<Item = NodeId>,
    voters: &BTreeSet<NodeId>,
    nodes: &BTreeMap<NodeId, Node>,
    renewed: &BTreeMap<NodeId, Instant>,
) -> Vec<NodeId> {
    members
        .filter(|id| {
            let node = nodes.get(id);
            if node.is_some_and(|n| n.status.features.iter().any(|f| f == VOTER_LIMIT)) {
                return false;
            }
            let unregistered =
                node.is_none_or(|n| n.status.features.is_empty() && n.status.version.is_none());
            voters.contains(id) || !unregistered || renewed.contains_key(id)
        })
        .collect()
}

/// 리더의 [`VoterKeeper`]가 투표 멤버를 고르는지: [`VOTER_LIMIT`]를 보고하지 않아 막는 멤버가 없다. 키퍼와 노드
/// (가입·재시작 때 스스로 승격을 요청할지, opld)가 이 규칙 하나를 쓴다. 규칙이 다르면 키퍼가 움직이는데 노드가
/// 스스로 승격해 승격·강등이 반복된다. `renewed`는 이 임기에 리스를 갱신한 멤버(리더의 리스 표)이고, 그것을
/// 모르는 노드는 비워 넘긴다(그러면 키퍼보다 덜 막지는 않는다).
pub fn leader_manages_voters(
    members: impl IntoIterator<Item = NodeId>,
    voters: &BTreeSet<NodeId>,
    nodes: &BTreeMap<NodeId, Node>,
    renewed: &BTreeMap<NodeId, Instant>,
) -> bool {
    blocking_members(members.into_iter(), voters, nodes, renewed).is_empty()
}

/// 키퍼가 틱 사이에 기억하는 것. 리더가 아니게 되면 비운다.
#[derive(Default)]
struct State {
    /// 리더로 본 임기와 그 임기에 살펴보기 시작한 시각(기준점). 새 리더의 리스 표는 비어 있으므로 투표 멤버는
    /// 기준점에 갱신한 것으로 친다(곧바로 오프라인으로 보지 않는다).
    base: Option<(u64, Instant)>,
    /// 이 키퍼가 시작했지만 끝을 보지 못한 변경(실패, 시간 초과)이 있다. 첫 단계(joint)만 커밋됐을 수 있다.
    unfinished: bool,
    /// 지금 보이는 joint 구성의 로그와 그것을 처음 본 시각
    joint: Option<(LogId<NodeId>, Instant)>,
    /// 마무리하지 못했다고 경고한 joint 구성의 로그(joint마다 한 번)
    finish_warned: Option<LogId<NodeId>>,
    /// 키퍼를 막는 멤버를 경고한 임기(임기마다 한 번)
    warned: Option<u64>,
    /// 관찰 노드 목록에 있는 learner(내려가는 중)를 처음 본 시각
    listed_since: BTreeMap<NodeId, Instant>,
}

async fn run(
    cluster: Arc<Cluster>,
    config: VoterKeeperConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut state = State::default();
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 과반이 없으면 변경이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&cluster, &config, &mut state) => {}
        }
    }
}

async fn tick(cluster: &Cluster, config: &VoterKeeperConfig, state: &mut State) {
    // Raft 구성원이 아니면 리더일 수 없다.
    let Some(raft) = cluster.raft() else {
        *state = State::default();
        return;
    };
    let m = raft.metrics().borrow().clone();
    if m.state != ServerState::Leader {
        *state = State::default();
        return;
    }
    if m.membership_config.membership().get_joint_config().len() > 1 {
        finish_joint(cluster, config, state, &m).await;
        return;
    }
    state.joint = None;
    let leases = cluster.leases();
    let since = match state.base {
        Some((term, at)) if term == leases.term => at,
        _ => {
            // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
            if cluster.read_barrier().await.is_err() {
                return;
            }
            let at = Instant::now();
            state.base = Some((leases.term, at));
            at
        }
    };
    let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
        return;
    };
    let nodes: BTreeMap<NodeId, _> = nodes.into_iter().map(|n| (NodeId(n.id), n)).collect();
    let name = |id: NodeId| {
        nodes
            .get(&id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    };
    let membership = m.membership_config.membership();
    let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
    // 옛 노드는 스스로 투표 멤버가 되려 하고 펜싱을 끄면 리스를 갱신하지 않는다. 모든 멤버가 기능을 알 때까지
    // 둔다(투표 멤버의 Node 리소스가 아직 없으면 합류 중이라 어느 버전인지 모른다).
    let blocking = blocking_members(
        membership.nodes().map(|(id, _)| *id),
        &voters,
        &nodes,
        &leases.renewed,
    );
    if !blocking.is_empty() {
        if state.warned != Some(leases.term) {
            state.warned = Some(leases.term);
            let names: Vec<String> = blocking.into_iter().map(name).collect();
            tracing::warn!(
                nodes = %names.join(","),
                "not managing the voting members: these members have not reported {VOTER_LIMIT}"
            );
        }
        return;
    }
    let limit = match cluster.get_by_name::<SettingsSpec>(SETTINGS_NAME) {
        Ok(settings) => settings.map(|s| s.spec).unwrap_or_default().voter_limit(),
        Err(_) => return,
    };
    // 클러스터 정보의 관찰 노드 목록(클러스터 정보가 없으면 관찰 노드를 둘 곳이 없다)
    let Ok((_, infos)) = cluster.list::<ClusterSpec>() else {
        return;
    };
    let info = infos.into_iter().next();
    let listed: BTreeMap<NodeId, String> = info
        .as_ref()
        .map(|i| {
            i.status
                .observers
                .iter()
                .map(|(id, addr)| (NodeId(*id), addr.clone()))
                .collect()
        })
        .unwrap_or_default();
    state.listed_since.retain(|id, _| listed.contains_key(id));
    if let Some(info) = &info
        && fix_listed(
            cluster, config, state, &raft, &m, info, &listed, &voters, &name,
        )
        .await
    {
        return;
    }
    let now = Instant::now();
    let age = |at: Instant| now.saturating_duration_since(at);
    let observers = listed
        .keys()
        .copied()
        .filter(|id| membership.get_node(id).is_none());
    let members: Vec<Member> = membership
        .nodes()
        .map(|(&id, _)| id)
        .chain(observers)
        .map(|id| {
            let node = nodes.get(&id);
            let voter = voters.contains(&id);
            let observer = membership.get_node(&id).is_none();
            let me = id == cluster.id();
            let renewed = leases.renewed.get(&id).copied();
            let last = if me {
                Some(now)
            } else if voter {
                Some(renewed.map_or(since, |r| r.max(since)))
            } else {
                renewed
            };
            Member {
                id,
                voter,
                witness: node.is_some_and(|n| n.spec.witness),
                zone: node.and_then(|n| n.labels.get(ZONE_LABEL).cloned()),
                healthy: last.is_some_and(|at| age(at) < config.healthy_within),
                // 기준점의 유예 없이 이 임기에 실제로 받은 갱신으로만 본다.
                alive: me || renewed.is_some_and(|at| age(at) < config.alive_within),
                offline: last.is_none_or(|at| age(at) >= config.offline_after),
                caught_up: voter || (!observer && caught_up(&m, id)),
                observer,
            }
        })
        .collect();
    // 모든 멤버와 관찰 노드가 관찰 노드 기능을 알 때만 learner를 내린다(옛 노드는 목록을 모른다).
    let observers_ready = members.iter().all(|member| {
        nodes
            .get(&member.id)
            .is_some_and(|n| n.status.features.iter().any(|f| f == OBSERVER))
    });
    let settling = age(since) < config.healthy_within;
    let Some(change) = plan(cluster.id(), &members, limit, settling, observers_ready) else {
        return;
    };
    // 계획은 장벽·목록 읽기 전의 지표로 세웠다. 그 사이 구성이 바뀌었으면(다른 변경, 리더 교체) 다음 틱에 다시 세운다.
    if !same_config(cluster, &m) {
        return;
    }
    let (changes, done) = match change {
        Change::Promote(id) => (
            ChangeMembers::AddVoterIds(BTreeSet::from([id])),
            format!("promoted {} to a voting member", name(id)),
        ),
        // learner로 남긴다(`retain`).
        Change::Demote(id) => (
            ChangeMembers::RemoveVoters(BTreeSet::from([id])),
            format!("demoted {} to a learner", name(id)),
        ),
        Change::Replace { out, by } => (
            ChangeMembers::ReplaceAllVoters(change.voters_after(&voters)),
            format!(
                "replaced the offline voting member {} with {}",
                name(out),
                name(by)
            ),
        ),
        Change::ToObserver(id) => {
            let (Some(info), Some(node)) = (&info, membership.get_node(&id)) else {
                return;
            };
            let mut next = listed.clone();
            next.insert(id, node.addr.clone());
            match write_observers(cluster, info, next).await {
                Ok(_) => tracing::info!("moving {} out of Raft as an observer", name(id)),
                Err(e) => tracing::warn!(error = %e, ?change, "cannot list an observer"),
            }
            return;
        }
        Change::ToLearner(id) => {
            return_observer(cluster, config, &raft, id, &listed, &name).await;
            return;
        }
    };
    state.unfinished = true;
    match change_members(&raft, changes, config.change_timeout).await {
        Outcome::Done => {
            state.unfinished = false;
            tracing::info!(limit, "{done}");
        }
        // 다른 변경(위트니스 승격, API의 노드 제거)이 커밋 중이다. 아무것도 쓰지 않았으므로 다음 틱에 그 뒤의
        // 구성으로 계획을 다시 세운다.
        Outcome::Busy => state.unfinished = false,
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, ?change, "cannot change the voting members")
        }
    }
}

/// 목록과 멤버십이 겹친 것을 고친다. 무언가 했거나 기다려야 하면 `true`(이번 틱에는 다른 것을 하지 않는다).
/// - 목록에 있는 투표 멤버는 목록에서 뺀다(투표 멤버는 관찰 노드로 내리지 않는다).
/// - 목록에 있는 learner(내려가는 중)는 그 노드가 목록을 받을 시간(간격 두 번, 리더가 목록 쓰기까지 복제했거나
///   `change_timeout`이 지남)을 준 뒤 멤버십에서 뺀다(`RemoveNodes`, openraft는 투표 멤버를 이것으로 빼지 않는다).
///   꺼져 있어 목록을 받지 못한 노드는 다시 켜졌을 때 `RoleKeeper`의 대비책이 관찰 노드로 만든다.
#[allow(clippy::too_many_arguments)]
async fn fix_listed(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    state: &mut State,
    raft: &Raft<TypeConfig>,
    m: &RaftMetrics<NodeId, BasicNode>,
    info: &ClusterInfo,
    listed: &BTreeMap<NodeId, String>,
    voters: &BTreeSet<NodeId>,
    name: &impl Fn(NodeId) -> String,
) -> bool {
    let stray: Vec<NodeId> = listed
        .keys()
        .copied()
        .filter(|id| voters.contains(id))
        .collect();
    if !stray.is_empty() {
        let next = listed
            .iter()
            .filter(|(id, _)| !voters.contains(id))
            .map(|(id, addr)| (*id, addr.clone()))
            .collect();
        match write_observers(cluster, info, next).await {
            Ok(_) => tracing::warn!(
                nodes = %stray.iter().map(|id| name(*id)).collect::<Vec<_>>().join(","),
                "voting members were listed as observers; unlisted them"
            ),
            Err(e) => tracing::warn!(error = %e, "cannot unlist voting members from the observers"),
        }
        return true;
    }
    let membership = m.membership_config.membership();
    let pending: Vec<NodeId> = listed
        .keys()
        .copied()
        .filter(|id| membership.get_node(id).is_some())
        .collect();
    if pending.is_empty() {
        return false;
    }
    let now = Instant::now();
    let ready: BTreeSet<NodeId> = pending
        .into_iter()
        .filter(|id| {
            let waited =
                now.saturating_duration_since(*state.listed_since.entry(*id).or_insert(now));
            waited >= config.interval * 2
                && (matched(m, *id) >= Some(info.revision) || waited >= config.change_timeout)
        })
        .collect();
    if ready.is_empty() || !same_config(cluster, m) {
        return true;
    }
    let names: Vec<String> = ready.iter().map(|id| name(*id)).collect();
    match change_members(
        raft,
        ChangeMembers::RemoveNodes(ready),
        config.change_timeout,
    )
    .await
    {
        Outcome::Done => {
            tracing::info!(nodes = %names.join(","), "moved learners out of Raft as observers")
        }
        Outcome::Busy => {}
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, nodes = %names.join(","), "cannot move learners out of Raft")
        }
    }
    true
}

/// 관찰 노드를 learner로 더한 뒤 목록에서 뺀다. 목록에서 빼지 못하면 다음 틱에 "목록에 있는 learner"로 보여 다시
/// 관찰 노드가 된다(목록과 멤버십 어느 쪽에도 없는 노드는 생기지 않는다).
async fn return_observer(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    raft: &Raft<TypeConfig>,
    id: NodeId,
    listed: &BTreeMap<NodeId, String>,
    name: &impl Fn(NodeId) -> String,
) {
    let Some(addr) = listed.get(&id) else {
        return;
    };
    let added = tokio::time::timeout(
        config.change_timeout,
        raft.add_learner(id, BasicNode::new(addr.clone()), false),
    )
    .await;
    match outcome(added) {
        Outcome::Done => {}
        Outcome::Busy => return,
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, node = %name(id), "cannot add an observer back as a learner");
            return;
        }
    }
    // learner를 더한 쓰기는 이 노드(리더)에 적용된 뒤 돌아온다. 목록은 그 뒤에 다시 읽는다.
    let Ok((_, infos)) = cluster.list::<ClusterSpec>() else {
        return;
    };
    let Some(info) = infos.into_iter().next() else {
        return;
    };
    let mut next: BTreeMap<NodeId, String> = info
        .status
        .observers
        .iter()
        .map(|(id, addr)| (NodeId(*id), addr.clone()))
        .collect();
    next.remove(&id);
    match write_observers(cluster, &info, next).await {
        Ok(_) => tracing::info!("added the observer {} back as a learner", name(id)),
        Err(e) => tracing::warn!(error = %e, node = %name(id), "cannot unlist an observer"),
    }
}

/// 클러스터 정보의 관찰 노드 목록을 `observers`로 바꾼다(읽은 뒤 다른 쪽이 썼으면 `RevisionMismatch`).
async fn write_observers(
    cluster: &Cluster,
    info: &ClusterInfo,
    observers: BTreeMap<NodeId, String>,
) -> Result<u64, ClusterError> {
    let status = ClusterStatus {
        observers: observers
            .into_iter()
            .map(|(id, addr)| (id.0, addr))
            .collect(),
    };
    cluster
        .update_status_checked::<ClusterSpec>(info.id, info.revision, &status)
        .await
}

/// 리더가 그 노드에 복제했다고 아는 마지막 로그 인덱스
fn matched(m: &RaftMetrics<NodeId, BasicNode>, id: NodeId) -> Option<u64> {
    m.replication
        .as_ref()
        .and_then(|r| r.get(&id).copied().flatten())
        .map(|l| l.index)
}

/// 두 단계 변경의 첫 단계(joint)까지만 된 구성을 uniform으로 마무리한다. 다른 쪽의 변경이 곧 두 번째 단계를
/// 제안할 수 있으므로 이 키퍼의 변경이 끊겼거나, joint가 이전 임기의 것이거나(그 리더의 두 번째 단계는 오지
/// 않는다), `change_timeout` 넘게 그대로일 때만 한다. 어느 때든 joint가 바뀌지 않은 채 간격 두 번 넘게 보였어야
/// 한다(살아 있는 두 번째 단계가 먼저 끝나게).
///
/// 마무리는 빠지는 투표 멤버를 learner로 남긴다. 노드를 지우는 변경(`LeaderRequest::Remove`)의 joint를 마무리했으면
/// 그 요청이 끝난 뒤 멤버십을 다시 읽어 learner로 남은 노드를 지운다(`leader::execute`).
async fn finish_joint(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    state: &mut State,
    m: &RaftMetrics<NodeId, BasicNode>,
) {
    let log_id = *m.membership_config.log_id();
    let now = Instant::now();
    let seen = match state.joint {
        Some((id, at)) if Some(id) == log_id => at,
        _ => {
            state.joint = log_id.map(|id| (id, now));
            now
        }
    };
    let earlier_term = log_id.is_some_and(|l| l.leader_id.term < m.current_term);
    let unchanged = now.saturating_duration_since(seen);
    if unchanged < config.interval * 2
        || (!state.unfinished && !earlier_term && unchanged < config.change_timeout)
    {
        return;
    }
    // 아무도 더하지 않는 승격은 joint를 그 마지막(새) 구성 하나로 만든다(openraft 0.9.25 `Membership::change`의
    // `AddVoterIds`: 새 투표 멤버 = 마지막 구성 ∪ {}, `find_coherent`는 그것을 품은 joint에서 uniform을 준다).
    let finish = ChangeMembers::AddVoterIds(BTreeSet::new());
    let Some(raft) = cluster.raft() else {
        return;
    };
    match change_members(&raft, finish, config.change_timeout).await {
        Outcome::Done => {
            state.unfinished = false;
            state.joint = None;
            tracing::info!("finished an interrupted change of the voting members");
        }
        // joint가 아직 커밋되지 않았다(새 리더의 빈 항목이 커밋하면 다음 틱에 마무리한다).
        Outcome::Busy => {}
        Outcome::Failed(e) => {
            if state.finish_warned != log_id {
                state.finish_warned = log_id;
                tracing::warn!(error = %e, "cannot finish an interrupted change of the voting members");
            }
        }
    }
}

/// 지금도 리더이고 멤버십 구성이 `m`(계획을 세운 지표)의 것 그대로다.
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, BasicNode>) -> bool {
    let Some(raft) = cluster.raft() else {
        return false;
    };
    let now = raft.metrics().borrow().clone();
    now.state == ServerState::Leader
        && now.current_term == m.current_term
        && now.membership_config.log_id() == m.membership_config.log_id()
}

/// 멤버십 변경 한 번의 결과
enum Outcome {
    Done,
    /// 앞선 멤버십 변경이 아직 커밋 중이라 거절됐다(아무것도 쓰지 않았다).
    Busy,
    /// 실패했거나 시간 안에 끝나지 않았다. 첫 단계(joint)만 커밋됐을 수 있다.
    Failed(String),
}

/// 투표 멤버를 바꾼다. 빠진 투표 멤버는 learner로 남긴다(`retain`).
async fn change_members(
    raft: &Raft<TypeConfig>,
    changes: ChangeMembers<NodeId, BasicNode>,
    timeout: Duration,
) -> Outcome {
    outcome(tokio::time::timeout(timeout, raft.change_membership(changes, true)).await)
}

type MembershipResult = Result<
    Result<ClientWriteResponse<TypeConfig>, RaftError<NodeId, ClientWriteError<NodeId, BasicNode>>>,
    tokio::time::error::Elapsed,
>;

fn outcome(result: MembershipResult) -> Outcome {
    match result {
        Err(_) => Outcome::Failed("timed out".into()),
        Ok(Ok(_)) => Outcome::Done,
        Ok(Err(RaftError::APIError(ClientWriteError::ChangeMembershipError(
            ChangeMembershipError::InProgress(_),
        )))) => Outcome::Busy,
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
    }
}

/// learner가 리더의 마지막 로그를 거의 따라잡았는지(리더의 복제 지표)
fn caught_up(m: &RaftMetrics<NodeId, openraft::BasicNode>, id: NodeId) -> bool {
    let matched = m
        .replication
        .as_ref()
        .and_then(|r| r.get(&id).copied().flatten())
        .map(|l| l.index);
    let last = m.last_log_index.unwrap_or(0);
    matched.is_some_and(|i| i + CATCH_UP_SLACK >= last)
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;

    use super::*;

    fn member(n: u64, voter: bool, zone: Option<&str>) -> Member {
        Member {
            id: NodeId::from(n),
            voter,
            witness: false,
            zone: zone.map(str::to_string),
            healthy: true,
            alive: true,
            offline: false,
            caught_up: true,
            observer: false,
        }
    }

    /// 꺼진 지 `offline_after`가 지난 멤버
    fn dead(m: &mut Member) {
        m.healthy = false;
        m.alive = false;
        m.offline = true;
    }

    fn id(n: u64) -> NodeId {
        NodeId::from(n)
    }

    #[test]
    fn learners_are_promoted_up_to_the_limit_preferring_other_zones() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, false, Some("a")),
            member(4, false, Some("b")),
            member(5, false, None),
        ];
        // zone b·없음에 투표 멤버가 없다. 같으면 id가 작은 것.
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(4)))
        );
        members[3].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(5)))
        );
        members[4].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(3)))
        );
        members[2].voter = true;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        // 노드가 상한보다 적으면 모두 투표 멤버다.
        assert_eq!(plan(id(1), &members, 7, false, false), None);
    }

    #[test]
    fn only_healthy_caught_up_learners_are_promoted() {
        let mut members = vec![member(1, true, None), member(2, false, None)];
        members[1].healthy = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        members[1].healthy = true;
        members[1].caught_up = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        members[1].caught_up = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(2)))
        );
    }

    #[test]
    fn extra_voters_are_demoted_from_crowded_zones_but_never_the_leader_or_a_witness() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, true, Some("a")),
            member(4, true, Some("b")),
            member(5, true, Some("b")),
            member(6, true, Some("c")),
            member(7, true, Some("c")),
        ];
        // zone a가 셋으로 가장 많다. 리더(3)는 빼고 id가 큰 것.
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(2)))
        );
        members[1].voter = false;
        // 이제 모든 zone이 둘이다. 건강하지 않은 것 먼저.
        members[3].healthy = false;
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(4)))
        );
        members[3].healthy = true;
        // 위트니스는 내리지 않는다.
        members[6].witness = true;
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(6)))
        );
        // 새 임기의 처음에는 개수 때문에 내리지 않는다.
        assert_eq!(plan(id(3), &members, 5, true, false), None);
    }

    #[test]
    fn witnesses_are_always_voters_and_count_toward_the_limit() {
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, false, None),
        ];
        members[3].witness = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Promote(id(4)))
        );
        members[3].voter = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(3)))
        );
    }

    /// 상한만큼의 투표 멤버 가운데 하나가 오프라인이면 learner 하나로 한 번에 바꾼다(빠진 zone의 것 먼저).
    #[test]
    fn an_offline_voter_is_replaced_by_a_learner_in_one_change() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("b")),
            member(3, true, Some("c")),
            member(4, false, Some("a")),
            member(5, false, Some("c")),
        ];
        dead(&mut members[2]);
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Replace {
                out: id(3),
                by: id(5)
            })
        );
        // 오프라인 위트니스는 바꾸지 않는다.
        members[2].witness = true;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 올릴 수 있는 learner가 없으면 그대로 둔다.
        members[2].witness = false;
        members[3].healthy = false;
        members[4].caught_up = false;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    /// 노드 여섯·상한 다섯에서 죽은 투표 멤버는 남은 learner로 한 번에 바뀐다.
    #[test]
    fn six_nodes_replace_a_dead_voter_in_one_change() {
        let mut members: Vec<Member> = (1..=6).map(|i| member(i, i <= 5, None)).collect();
        dead(&mut members[4]);
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Replace {
                out: id(5),
                by: id(6)
            })
        );
    }

    /// 노드가 상한 이하면 오프라인이어도 투표 멤버로 남는다: 노드 둘과 위트니스에서 B를 내리면 그 뒤 A가 죽었을 때
    /// B와 위트니스로 정족수가 안 된다.
    #[test]
    fn an_offline_voter_without_a_replacement_stays_a_voter() {
        // 노드 둘(1, 2)과 위트니스(3)
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
        ];
        members[2].witness = true;
        dead(&mut members[1]);
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 노드 셋
        members[2].witness = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    #[test]
    fn nothing_changes_without_a_live_majority() {
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, true, None),
            member(5, true, None),
            member(6, false, None),
        ];
        for m in &mut members[2..5] {
            dead(m);
        }
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        // 셋이 살아 있으면(과반) 오프라인 하나를 learner로 바꿀 수 있다(바꾼 뒤 다섯 중 넷이 살아 있다).
        members[2] = member(3, true, None);
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Replace {
                out: id(4),
                by: id(6)
            })
        );
        // 상한보다 많아도 내린 뒤 과반이 살아 있지 않으면 내리지 않는다: 다섯 중 살아 있는 셋(리더, 2, 3) 가운데
        // 하나를 내리면 넷 중 둘만 살아 있다(살아 있지 않은 둘은 위트니스라 내리지 않는다).
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, true, None),
            member(5, true, None),
        ];
        for m in &mut members[3..] {
            m.witness = true;
            m.healthy = false;
            m.alive = false;
        }
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 위트니스 하나가 살아 있으면 내릴 수 있다.
        members[3].healthy = true;
        members[3].alive = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(3)))
        );
    }

    /// 새 리더는 리스 표가 비어 투표 멤버를 기준점에 갱신한 것으로 쳐 건강하다고 본다. 과반은 그 유예 없이
    /// 실제로 받은 갱신(살아 있음)으로 세므로, 죽은 투표 멤버가 있으면 살아 있는 투표 멤버를 내리지 않는다.
    #[test]
    fn a_new_leader_does_not_demote_a_live_voter_on_stale_health() {
        // 다섯 가운데 4·5가 죽었지만 아직 건강하다고 보인다. 살아 있는 셋은 붐비는 zone a에 있다.
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, true, Some("a")),
            member(4, true, Some("b")),
            member(5, true, Some("c")),
        ];
        for m in &mut members[3..] {
            m.alive = false;
        }
        // 새 임기의 처음에는 내리지 않는다.
        assert_eq!(plan(id(1), &members, 3, true, false), None);
        // 그 뒤에는 살아 있지 않은 것을 먼저 내린다(살아 있는 2·3을 내리면 넷 중 둘만 살아 있다).
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(5)))
        );
        // 살아 있는 투표 멤버만 남은 후보라면 과반이 깨지므로 내리지 않는다.
        members[3].witness = true;
        members[4].witness = true;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    /// Raft 밖의 관찰 노드
    fn observer(n: u64, zone: Option<&str>) -> Member {
        Member {
            observer: true,
            caught_up: false,
            ..member(n, false, zone)
        }
    }

    /// 투표 멤버 다섯(1..=5)과 learner `learners`
    fn five_voters_and(learners: &[u64]) -> Vec<Member> {
        (1..=5)
            .map(|i| member(i, true, None))
            .chain(learners.iter().map(|&i| member(i, false, None)))
            .collect()
    }

    /// 대기 learner가 둘보다 많으면 모든 노드가 기능을 알 때만 하나를 관찰 노드로 내린다(대기 learner가 많은 zone의
    /// 것, 나중에 들어온 것). 건강하지 않은 learner와 위트니스는 세지도 내리지도 않고, 새 임기의 처음에는 하지 않는다.
    #[test]
    fn extra_standby_learners_become_observers() {
        let mut members = five_voters_and(&[6, 7, 8]);
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToObserver(id(8)))
        );
        assert_eq!(plan(id(1), &members, 5, true, true), None);
        // zone a에 대기 learner가 둘이면 그 가운데 나중 것
        members[5].zone = Some("a".into());
        members[6].zone = Some("a".into());
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToObserver(id(7)))
        );
        // 건강하지 않은 learner는 대기로 세지 않는다(둘만 남아 아무것도 하지 않는다).
        members[7].healthy = false;
        assert_eq!(plan(id(1), &members, 5, false, true), None);
        // 위트니스 learner는 먼저 투표 멤버로 올린다(관찰 노드로 내리지 않는다).
        members[7].healthy = true;
        members[7].witness = true;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Promote(id(8)))
        );
        // 오프라인 투표 멤버가 있으면(바꿀 learner가 따라잡는 중) 내리지 않는다.
        let mut members = five_voters_and(&[6, 7, 8]);
        dead(&mut members[4]);
        for m in &mut members[5..] {
            m.caught_up = false;
        }
        assert_eq!(plan(id(1), &members, 5, false, true), None);
    }

    /// 대기 learner가 둘보다 적거나 투표 멤버가 모자란데 올릴 learner가 없으면 건강한 관찰 노드를 learner로 올린다
    /// (투표 멤버가 적은 zone의 것 먼저). 새 임기의 처음에는 하지 않는다.
    #[test]
    fn observers_come_back_as_learners_when_standbys_run_short() {
        let mut members = five_voters_and(&[6]);
        members.push(observer(7, None));
        members.push(observer(8, Some("b")));
        // 투표 멤버는 모두 zone 없음이다. zone b의 관찰 노드가 먼저.
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(8)))
        );
        assert_eq!(plan(id(1), &members, 5, true, true), None);
        // 건강하지 않은 관찰 노드는 올리지 않는다.
        members[7].healthy = false;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(7)))
        );
        // 대기 learner가 둘이면 그대로다.
        members[6].observer = false;
        members[6].caught_up = true;
        assert_eq!(plan(id(1), &members, 5, false, true), None);
        // 투표 멤버가 상한보다 적은데 올릴 learner가 없다(따라잡는 중이 아니라 아예 없다).
        let members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            observer(4, None),
        ];
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::ToLearner(id(4)))
        );
    }

    /// 죽은 투표 멤버를 대기 learner로 바꾸면 대기가 줄어 관찰 노드가 learner로 올라온다(그 뒤 다시 죽은 투표
    /// 멤버가 생기면 그것이 올라간다).
    #[test]
    fn a_dead_voter_pulls_an_observer_through_learner_to_voter() {
        let mut members = five_voters_and(&[6, 7]);
        members.push(observer(8, Some("z")));
        dead(&mut members[4]);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Replace {
                out: id(5),
                by: id(6)
            })
        );
        // 바꾼 뒤: 5는 죽은 learner, 6은 투표 멤버. 건강한 대기는 7 하나.
        members[4].voter = false;
        members[5].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(8)))
        );
        // 8이 learner로 따라잡은 뒤 투표 멤버 하나가 더 죽으면 zone이 다른 8이 올라간다.
        members[7].observer = false;
        members[7].caught_up = true;
        dead(&mut members[3]);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Replace {
                out: id(4),
                by: id(8)
            })
        );
    }

    /// 어떤 경우에도 투표 멤버를 관찰 노드로 내리지 않는다.
    #[test]
    fn voters_never_become_observers() {
        for n in 1..=9u64 {
            for voters in 1..=n {
                for limit in [3, 5, 7] {
                    for settling in [false, true] {
                        let members: Vec<Member> =
                            (1..=n).map(|i| member(i, i <= voters, None)).collect();
                        if let Some(Change::ToObserver(id)) =
                            plan(NodeId::from(1), &members, limit, settling, true)
                        {
                            assert!(!members.iter().any(|m| m.id == id && m.voter));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_observer_feature_is_listed() {
        assert!(opl_types::node::FEATURES.contains(&OBSERVER));
    }

    fn node(n: u64, features: &[&str], version: Option<&str>) -> (NodeId, Node) {
        let mut node = Resource::new(
            format!("n{n}"),
            NodeSpec {
                address: format!("node-{n}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = id(n).0;
        node.status = NodeStatus {
            features: features.iter().map(|f| f.to_string()).collect(),
            version: version.map(str::to_string),
            ..NodeStatus::default()
        };
        (id(n), node)
    }

    /// 합류하자마자 죽어 등록하지 못한 learner(기능·버전 없음, 리스 없음)는 키퍼를 막지 않는다. 옛 노드(버전이
    /// 있거나 리스를 갱신한다)와 기능을 모르는 투표 멤버는 막는다.
    #[test]
    fn a_learner_that_never_registered_does_not_block() {
        let nodes: BTreeMap<NodeId, Node> = [
            node(1, &[VOTER_LIMIT], Some("0.3.0")),
            node(2, &[], None),
            node(3, &[], Some("0.2.0")),
            node(4, &[], None),
            node(6, &[], None),
        ]
        .into_iter()
        .collect();
        let voters = BTreeSet::from([id(1), id(6)]);
        let renewed = BTreeMap::from([(id(4), Instant::now())]);
        // 2: 등록 못 한 learner, 5: Node 기록도 없는 learner — 빼고 본다.
        // 3: 옛 learner, 4: 리스를 갱신하는 learner, 6: 기능 없는 투표 멤버 — 막는다.
        let members = (1..=6).map(id);
        assert_eq!(
            blocking_members(members, &voters, &nodes, &renewed),
            vec![id(3), id(4), id(6)]
        );
        // 노드 쪽이 쓰는 공용 판단도 같다: 등록 못 한 learner만 있으면 리더가 고른다.
        assert!(!leader_manages_voters(
            (1..=6).map(id),
            &voters,
            &nodes,
            &renewed
        ));
        let fine = BTreeSet::from([id(1)]);
        assert!(leader_manages_voters(
            [1, 2, 5].map(id),
            &fine,
            &nodes,
            &BTreeMap::new()
        ));
        // 이 임기에 리스를 갱신한 learner는 등록 전이어도 막는다(리스 표를 아는 리더만).
        let renewing = BTreeMap::from([(id(2), Instant::now())]);
        assert!(!leader_manages_voters(
            [1, 2].map(id),
            &fine,
            &nodes,
            &renewing
        ));
    }

    #[test]
    fn the_feature_is_listed() {
        assert!(opl_types::node::FEATURES.contains(&VOTER_LIMIT));
    }
}
```

`crates/opl-cluster/tests/observers.rs`를 만든다:

```rust
//! 관찰 노드 전환(설계 §6): 노드 여덟이 투표 멤버 다섯·대기 learner 둘·관찰 노드 하나로 나뉘고, 투표 멤버가 죽으면
//! 관찰 노드가 learner를 거쳐 투표 멤버가 된다. 내려가는 노드는 그동안 읽고 쓰고, 다시 시작한 관찰 노드는 자기
//! 리비전부터 이어 가며, 꺼진 동안 내려진 노드는 대비책으로 관찰 노드가 된다. 관찰 노드 기능을 모르는 노드가
//! 있으면 아무도 내리지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::NodeId;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, OBSERVER, RoleKeeper, RoleKeeperConfig, VOTER_LIMIT,
    VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 리스 갱신 간격
const RENEW: Duration = Duration::from_millis(200);

fn keeper_config() -> VoterKeeperConfig {
    VoterKeeperConfig {
        interval: Duration::from_millis(100),
        healthy_within: Duration::from_secs(1),
        alive_within: Duration::from_millis(600),
        offline_after: Duration::from_secs(2),
        change_timeout: Duration::from_secs(5),
    }
}

fn role_config() -> RoleKeeperConfig {
    RoleKeeperConfig {
        interval: Duration::from_millis(100),
        stalled_after: Duration::from_secs(2),
    }
}

fn id(n: u64) -> NodeId {
    NodeId::from(n)
}

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

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper, RoleKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

struct Harness {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Running>,
}

impl Harness {
    /// 노드 `1..=5`를 투표 멤버로 초기화하고 클러스터 정보와 Node 기록(`zones`의 zone, 기능 `voter-limit`)을 만든
    /// 뒤 나머지를 learner로 더한다. learner가 모두 따라잡은 뒤 `without_observer`가 아닌 노드가 기능 `observer`를
    /// 알린다(그 전에는 아무도 내리지 않으므로, 따라잡는 중인 learner가 먼저 내려가지 않는다).
    async fn start(zones: &[&str], without_observer: &[u64]) -> Self {
        let mut h = Harness {
            router: MemRouter::default(),
            root: TempDir::new().unwrap(),
            nodes: BTreeMap::new(),
        };
        let n = zones.len() as u64;
        for i in 1..=n {
            h.open(i).await;
        }
        let voters = (1..=5)
            .map(|i| (id(i), openraft::BasicNode::new(format!("node-{i}"))))
            .collect();
        h.get(1).initialize(voters).await.unwrap();
        eventually("a leader", || async { h.leader().is_some() }).await;
        let mut info = Resource::new(
            "c",
            ClusterSpec {
                ca_cert_pem: "CERT".into(),
                ca_key_pem: "KEY".into(),
            },
        );
        info.status = ClusterStatus::default();
        h.get(1).create(&info).await.unwrap();
        for (i, zone) in (1..=n).zip(zones) {
            h.register(i, zone).await;
        }
        for i in 6..=n {
            let leader = h.leader_cluster();
            leader
                .add_learner(id(i), openraft::BasicNode::new(format!("node-{i}")))
                .await
                .unwrap();
        }
        eventually("learners caught up", || async {
            let Some(raft) = h.leader().and_then(|l| h.nodes[&l].cluster.raft()) else {
                return false;
            };
            let m = raft.metrics().borrow().clone();
            let last = m.last_log_index;
            m.replication.as_ref().is_some_and(|r| {
                (6..=n).all(|i| r.get(&id(i)).is_some_and(|l| l.map(|l| l.index) == last))
            })
        })
        .await;
        for i in (1..=n).filter(|i| !without_observer.contains(i)) {
            h.enable_observer(i).await;
        }
        h
    }

    /// 노드를 (다시) 연다(저장소의 모드대로). 데이터 디렉터리는 노드마다 고정이다.
    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = Duration::from_secs(10);
        let deadline = Instant::now() + Duration::from_secs(5);
        let cluster = loop {
            match Cluster::open_node(
                config.clone(),
                self.router.network(node),
                self.router.follower(node),
            )
            .await
            {
                // openraft의 상태 머신 작업이 아직 저장소를 쥐고 있다.
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        };
        let cluster = Arc::new(cluster.with_forwarder(self.router.forwarder(node)));
        self.router.register(node, &cluster);
        let renew = {
            let cluster = cluster.clone();
            tokio::spawn(async move {
                loop {
                    let _ = cluster.renew_lease(RENEW * 2).await;
                    tokio::time::sleep(RENEW).await;
                }
            })
        };
        self.nodes.insert(
            node,
            Running {
                voters: VoterKeeper::spawn(cluster.clone(), keeper_config()),
                roles: RoleKeeper::spawn(cluster.clone(), role_config()),
                cluster,
                renew,
            },
        );
    }

    fn get(&self, i: u64) -> &Arc<Cluster> {
        &self.nodes[&id(i)].cluster
    }

    /// 투표 멤버 가운데 스스로 리더라고 보는 노드
    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|n| {
            let raft = n.cluster.raft()?;
            let m = raft.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(n.cluster.id())
        })
    }

    fn leader_cluster(&self) -> &Arc<Cluster> {
        &self.nodes[&self.leader().expect("a leader")].cluster
    }

    /// 리더가 아는 (투표 멤버, learner), joint면 None
    fn membership(&self) -> Option<(BTreeSet<NodeId>, BTreeSet<NodeId>)> {
        let m = self.nodes[&self.leader()?]
            .cluster
            .raft()?
            .metrics()
            .borrow()
            .clone();
        let membership = m.membership_config.membership();
        if membership.get_joint_config().len() > 1 {
            return None;
        }
        let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
        let learners = membership.learner_ids().collect();
        Some((voters, learners))
    }

    /// 클러스터 정보의 관찰 노드 목록(리더의 저장소)
    fn listed(&self) -> BTreeSet<NodeId> {
        let Some(leader) = self.leader() else {
            return BTreeSet::new();
        };
        let (_, infos) = self.nodes[&leader].cluster.list::<ClusterSpec>().unwrap();
        infos[0]
            .status
            .observers
            .keys()
            .map(|u| NodeId(*u))
            .collect()
    }

    /// 투표 멤버 `voters`, learner `learners`, 나머지가 관찰 노드(목록에 있고 관찰 노드 모드)
    fn settled(&self, voters: &[u64], learners: &[u64], observers: &[u64]) -> bool {
        let ids = |v: &[u64]| v.iter().map(|&i| id(i)).collect::<BTreeSet<_>>();
        self.membership() == Some((ids(voters), ids(learners)))
            && self.listed() == ids(observers)
            && observers
                .iter()
                .all(|&i| self.get(i).mode() == NodeMode::Observer)
            && learners
                .iter()
                .chain(voters)
                .all(|&i| self.get(i).mode() == NodeMode::Member)
    }

    /// `settled`가 될 때까지 30초까지 기다린다. 안 되면 지금 모양을 보인다.
    async fn settle(&self, voters: &[u64], learners: &[u64], observers: &[u64]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.settled(voters, learners, observers) {
            if Instant::now() >= deadline {
                let modes: Vec<_> = self
                    .nodes
                    .iter()
                    .map(|(id, n)| (id.0.as_u128(), n.cluster.mode()))
                    .collect();
                panic!(
                    "not settled: leader {:?} membership {:?} listed {:?} modes {modes:?}",
                    self.leader(),
                    self.membership(),
                    self.listed()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 노드 `i`의 Node 기록(zone 레이블, 기능)
    async fn register(&self, i: u64, zone: &str) {
        let mut node = Resource::new(
            format!("n{i}"),
            NodeSpec {
                address: format!("node-{i}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = id(i).0;
        node.labels.insert(ZONE_LABEL.into(), zone.into());
        node.status = features(false);
        self.get(1).create(&node).await.unwrap();
    }

    /// 노드 `i`가 관찰 노드 기능을 알린다.
    async fn enable_observer(&self, i: u64) {
        let node = self.get(1).get::<NodeSpec>(id(i).0).unwrap().unwrap();
        self.get(1)
            .update_status::<NodeSpec>(node.id, &features(true))
            .await
            .unwrap();
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 데이터 디렉터리는 남는다.
    async fn stop(&mut self, i: u64) {
        let node = self.nodes.remove(&id(i)).unwrap();
        node.renew.abort();
        node.roles.shutdown().await;
        node.voters.shutdown().await;
        self.router.unregister(id(i));
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for i in self
            .nodes
            .keys()
            .map(|n| n.0.as_u128() as u64)
            .collect::<Vec<_>>()
        {
            self.stop(i).await;
        }
    }
}

fn features(observer: bool) -> NodeStatus {
    let mut features = vec![VOTER_LIMIT.to_string()];
    if observer {
        features.push(OBSERVER.into());
    }
    NodeStatus {
        online: true,
        features,
        ..NodeStatus::default()
    }
}

/// `cond`가 참이 될 때까지 30초까지 기다린다.
async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 노드 여덟은 투표 멤버 다섯·대기 learner 둘·관찰 노드 하나가 된다. 내려가는 노드(8)는 그동안 쓰기를 리더로
/// 보내고 곧바로 자기 저장소에서 읽는다. 관찰 노드가 되면 Raft가 없고 로그는 비어 있다.
#[tokio::test]
async fn eight_nodes_split_into_voters_standbys_and_an_observer_without_interrupting_writes() {
    let h = Harness::start(&["a"; 8], &[]).await;
    let eight = h.get(8).clone();
    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicU64::new(0));
    let writer = {
        let (stop, written) = (stop.clone(), written.clone());
        tokio::spawn(async move {
            let mut i = 0;
            while !stop.load(Ordering::SeqCst) {
                let v = volume(&format!("w{i}"));
                let revision = eight.create(&v).await.expect("a write from node 8");
                let read = eight
                    .get::<VolumeSpec>(v.id)
                    .unwrap()
                    .expect("read own write");
                assert_eq!(read.revision, revision);
                written.fetch_add(1, Ordering::SeqCst);
                i += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let before = written.load(Ordering::SeqCst);
    eventually("more writes from the observer", || async {
        written.load(Ordering::SeqCst) > before + 5
    })
    .await;
    stop.store(true, Ordering::SeqCst);
    writer.await.unwrap();

    let eight = h.get(8);
    assert!(eight.raft().is_none());
    assert!(eight.members().observers.contains_key(&id(8)));
    // 관찰 노드도 리더와 같은 것을 읽는다.
    let revision = h.leader_cluster().revision();
    eight
        .wait_revision(revision, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        eight.list::<VolumeSpec>().unwrap().1,
        h.leader_cluster().list::<VolumeSpec>().unwrap().1
    );
    h.close().await;
}

/// 투표 멤버가 죽으면 대기 learner가 올라가고, 관찰 노드가 learner로 돌아와(로그를 적용 위치까지 지운 것으로 메운
/// 뒤 리더에게서 받는다) 다음 장애에 투표 멤버가 된다.
#[tokio::test]
async fn an_observer_returns_through_learner_to_voter_when_voters_die() {
    let mut h = Harness::start(&["a", "a", "b", "b", "c", "a", "b", "d"], &[]).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let observed = h.get(8).revision();
    h.stop(5).await;
    // 5(zone c)는 6으로 바뀌고(zone a·b가 같아 id가 작은 것) 대기가 하나라 8이 learner로 돌아온다.
    eventually("node 8 back as a learner", || async {
        h.membership()
            .is_some_and(|(v, l)| v.contains(&id(6)) && l.contains(&id(8)))
            && h.get(8).mode() == NodeMode::Member
            && !h.listed().contains(&id(8))
    })
    .await;
    // 다시 띄운 Raft는 지운 로그를 적용 위치까지 지운 것(purge)으로 메웠다.
    let m = h.get(8).raft().unwrap().metrics().borrow().clone();
    assert!(m.purged.map(|l| l.index) >= Some(observed), "{m:?}");
    // 따라잡은 뒤 4(zone b)가 죽으면 투표 멤버가 없는 zone d의 8이 올라간다.
    eventually("a leader", || async { h.leader().is_some() }).await;
    let target = h.leader_cluster().revision();
    h.get(8)
        .wait_revision(target, Duration::from_secs(10))
        .await
        .unwrap();
    h.stop(4).await;
    eventually("node 8 a voter", || async {
        h.membership().is_some_and(|(v, _)| v.contains(&id(8)))
    })
    .await;
    let v = volume("after");
    h.get(8).create(&v).await.unwrap();
    h.close().await;
}

/// 다시 시작한 관찰 노드(파일 저장소)는 관찰 노드로 뜨고 자기 리비전부터 이어 받는다(스냅샷·`Reset` 없이).
#[tokio::test]
async fn a_restarted_observer_resumes_from_its_revision() {
    let mut h = Harness::start(&["a"; 8], &[]).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let old = h.get(8).clone();
    h.stop(8).await;
    // 멈춘 뒤의 적용 위치(저장소에 남은 것)
    let revision = old.revision();
    drop(old);
    let mut written = Vec::new();
    for i in 0..3 {
        let v = volume(&format!("while-down-{i}"));
        h.leader_cluster().create(&v).await.unwrap();
        written.push(v.id);
    }
    h.open(8).await;
    let eight = h.get(8).clone();
    assert_eq!(eight.mode(), NodeMode::Observer);
    assert!(eight.raft().is_none());
    assert_eq!(eight.revision(), revision);
    let mut w = eight.watch(revision).unwrap();
    let mut seen = Vec::new();
    while seen.len() < written.len() {
        match tokio::time::timeout(Duration::from_secs(10), w.next())
            .await
            .unwrap()
            .unwrap()
        {
            WatchEvent::Put {
                kind: opl_types::resource::Kind::Volume,
                resource,
            } => seen.push(resource.id),
            WatchEvent::Put { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, written);
    h.close().await;
}

/// 내려가는 동안 목록을 받지 못한 노드(Raft가 끊긴 채 멤버십에서 빠지고 그 뒤 다시 시작함)는 적용이 멈춘 것을 보고
/// 투표 멤버에게 역할을 물어 관찰 노드가 된다.
#[tokio::test]
async fn a_node_demoted_while_cut_off_follows_the_fallback() {
    // 8이 내려가기 전에 Raft를 끊는다: 기능을 모두 알기 전에는 아무도 내리지 않는다.
    let mut h = Harness::start(&["a"; 8], &[8]).await;
    eventually("standbys caught up", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 3)
    })
    .await;
    h.router.block_raft(id(8));
    h.enable_observer(8).await;
    // 목록에 오르지만 8은 그 쓰기를 받지 못한다. 그 사이 꺼지고, `change_timeout` 뒤 멤버십에서 빠진다.
    eventually("node 8 listed", || async { h.listed().contains(&id(8)) }).await;
    assert_eq!(h.get(8).mode(), NodeMode::Member);
    h.stop(8).await;
    eventually("node 8 out of Raft", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 2 && !l.contains(&id(8)))
    })
    .await;
    h.router.heal(id(8));
    h.open(8).await;
    assert_eq!(h.get(8).mode(), NodeMode::Member);
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    let v = volume("after-fallback");
    let revision = h.get(8).create(&v).await.unwrap();
    assert_eq!(
        h.get(8).get::<VolumeSpec>(v.id).unwrap().unwrap().revision,
        revision
    );
    h.close().await;
}

/// 관찰 노드 기능을 모르는 노드가 하나라도 있으면 아무도 내리지 않는다. 그 노드가 기능을 알리면 내린다.
#[tokio::test]
async fn nobody_becomes_an_observer_until_every_node_knows_the_feature() {
    let h = Harness::start(&["a"; 8], &[3]).await;
    eventually("standbys caught up", || async {
        h.membership()
            .is_some_and(|(v, l)| v.len() == 5 && l.len() == 3)
    })
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(h.listed().is_empty());
    assert_eq!(h.membership().map(|(_, l)| l.len()), Some(3));
    h.enable_observer(3).await;
    h.settle(&[1, 2, 3, 4, 5], &[6, 7], &[8]).await;
    h.close().await;
}
```

`crates/opl-cluster/tests/scale.rs`에서

```rust
/// 보낸 Raft RPC를 세는 네트워크
struct Counting {
```

을 다음으로 바꾼다:

```rust
/// 보낸 Raft RPC를 세는 네트워크
#[derive(Clone)]
struct Counting {
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
    assert!(members.voters.contains_key(&n4) && members.voters.contains_key(&n5));
}
```

을 다음으로 바꾼다:

```rust
    assert!(members.voters.contains_key(&n4) && members.voters.contains_key(&n5));
}

/// 승격(`AddVoter`)은 learner가 따라잡기를 기다린 뒤 바로 앞에서 관찰 노드 목록을 다시 본다: 기다리는 사이 목록에
/// 적힌 노드(관찰 노드로 내려가며 로그를 지운다)는 올리지 않는다.
#[tokio::test]
async fn a_learner_listed_as_an_observer_while_catching_up_is_not_promoted() {
    use opl_types::cluster::{ClusterSpec, ClusterStatus};
    use opl_types::resource::Resource;

    let mut tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    let n4 = node(4);
    tc.open(n4).await;
    tc.get(leader).add_learner(n4, addr(4)).await.unwrap();
    let info = Resource::new(
        "c",
        ClusterSpec {
            ca_cert_pem: "CERT".into(),
            ca_key_pem: "KEY".into(),
        },
    );
    tc.get(leader).create(&info).await.unwrap();

    // learner가 따라잡지 못하는 동안 승격을 요청하고, 그 사이 목록에 적는다.
    tc.router.block_raft(n4);
    tc.get(leader).create(&volume("unseen")).await.unwrap();
    let c = tc.get(leader);
    let list_then_heal = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let status = ClusterStatus {
            observers: [(n4.0, "node-4".to_string())].into(),
        };
        c.update_status::<ClusterSpec>(info.id, &status)
            .await
            .unwrap();
        tc.router.heal(n4);
    };
    let (promoted, ()) = tokio::join!(
        c.execute_as_leader(LeaderRequest::AddVoter { id: n4 }),
        list_then_heal
    );
    assert!(
        matches!(
            promoted,
            Err(ClusterError::Rejected(Rejection::NotFound { .. }))
        ),
        "{promoted:?}"
    );
    assert!(!c.members().voters.contains_key(&n4));
}
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    let addr = listener.local_addr().unwrap().to_string();
    // 목록에 적은 뒤 멤버십에서 뺀다(그 사이에도 거절되지 않게).
    set_observers(leader, &[(id, addr.clone())]).await;
    leader
        .cluster
        .request(LeaderRequest::Remove { id })
        .await
```

을 다음으로 바꾼다:

```rust
    let addr = listener.local_addr().unwrap().to_string();
    // 목록에 적은 뒤 멤버십에서 뺀다(그 사이에도 거절되지 않게). 리더의 키퍼처럼 `RemoveNodes`로 뺀다
    // (`LeaderRequest::Remove`는 노드를 지우는 것이라 목록에서도 뺀다).
    set_observers(leader, &[(id, addr.clone())]).await;
    leader
        .cluster
        .raft()
        .unwrap()
        .change_membership(openraft::ChangeMembers::RemoveNodes([id].into()), false)
        .await
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    let cluster = loop {
        match Cluster::open_observer(config.clone(), peers.clone()).await {
            Err(opl_cluster::ClusterError::Store(_)) if Instant::now() < deadline => {
```

을 다음으로 바꾼다:

```rust
    let cluster = loop {
        match Cluster::open_observer(
            config.clone(),
            GrpcNetwork::new(peers.clone()),
            peers.clone(),
        )
        .await
        {
            Err(opl_cluster::ClusterError::Store(_)) if Instant::now() < deadline => {
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, 사용량 지표, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    ApiCertAgent, AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager,
    HaManagerConfig, IncomingConfig, IncomingMigrations, RemotePoller, RuntimeProbe,
    SnapshotScheduler, TaskRunner, TaskRunnerConfig, TelemetryReporter, UefiVarsFetcher,
    UpdateAgent, Updates, VipAgent, VipManager, VipManagerConfig,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{
    CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Members, RoleKeeper, RoleKeeperConfig,
    Typed, VoterKeeper, VoterKeeperConfig, leader_manages_voters,
};
use opl_driver::{
    DirStorage, Hypervisor, IpVip, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage,
    Storage, Storages, SystemLvm, SystemRbd, SystemZfs, VipRecord, ZfsStorage, migration_tls_dir,
    write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{ApiCert, TlsListener, api_server_config_with};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers, renew_liveness};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::metrics::host as metrics_host;
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::notify::Notifier;
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;
use crate::runtime::{Handles, on, run_on};

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 데이터 디렉터리 아래 마이그레이션 디스크 기록 파일을 두는 디렉터리(로컬 디스크 마이그레이션 설계 §3.6)
const MIGRATION_DISKS_DIR: &str = "migration-disks";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// HA 펜싱을 쓰지 않는 노드(펜싱을 끈 노드, 위트니스)가 생존 표시로 리스를 갱신하는 간격. `HaGuard`의
/// 가장 긴 갱신 간격과 같다(리더는 10초 넘게 갱신이 없으면 오프라인으로 본다).
const LIVENESS_RENEW: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록과 기능 표시(위트니스는 기능 표시만)를 다시 확인하는 간격. 관리자가 브리지를 만들면
/// 이만큼 뒤에 스케줄러가 알고, 옛 버전 리더가 지운 기능 표시는 이만큼 뒤에 되살아난다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    /// API 이름(클러스터 설정 `api_names`)을 API 인증서에 더한다(관리자 인증서 `[api] tls_cert`를 쓰면
    /// 없다).
    api_cert: Option<ApiCertAgent>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    /// 리더가 투표 멤버를 상한(클러스터 설정 `voters`)에 맞춘다.
    voter_keeper: VoterKeeper,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    /// 리더가 내장 VIP의 주인을 정한다(이 노드의 `[vip]`과 상관없다).
    vip_manager: VipManager,
    /// 이 노드의 VIP 리스를 갱신하고 VIP를 붙인다(`[vip]`이 없으면 없다).
    vip: Option<VipAgent>,
    pools: PoolMounter,
    /// 이 노드와 그 VM의 사용량 지표를 잰다.
    metrics: MetricsSampler,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
    /// APT 소스 파일과 주기 업데이트 확인(업데이트가 꺼졌으면 없다)
    updates: Option<UpdateAgent>,
    /// 리더가 텔레메트리 보고를 보낸다(설정에서 켰을 때).
    telemetry: TelemetryReporter,
    /// 리더가 원격 클러스터의 상태를 모은다.
    remotes: RemotePoller,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
    audit: Arc<AuditLog>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        // VIP를 먼저 떼어 앞단이 다른 노드로 옮겨 가게 한다.
        if let Some(vip) = self.vip {
            vip.shutdown().await;
        }
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.audit.shutdown().await;
        if let Some(api_cert) = self.api_cert {
            api_cert.shutdown().await;
        }
        self.monitor.shutdown().await;
        self.voter_keeper.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.telemetry.shutdown().await;
        self.remotes.shutdown().await;
        self.recovery.shutdown().await;
        self.vip_manager.shutdown().await;
        self.pools.shutdown().await;
        self.metrics.shutdown().await;
        if let Some(updates) = self.updates {
            updates.shutdown().await;
        }
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 이 노드의 역할(관찰 노드 ↔ 멤버)을 클러스터 정보의 관찰 노드 목록에 맞춘다.
    roles: RoleKeeper,
    /// 생존 표시로만 리스를 갱신한다(HA 펜싱을 쓰는 노드는 `HaGuard`가 갱신하므로 없다).
    liveness: Option<JoinHandle<()>>,
    /// Raft가 멈추면 `exited`로 알린다. 정상 종료는 저장소를 닫기 전에 이것부터 멈춘다.
    raft_watch: JoinHandle<()>,
    /// 서버 태스크가 끝나거나 Raft가 멈추면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    ///
    /// 서비스를 실행기 셋에 나눠 띄운다(설계 §3.1). Raft(openraft 작업은 `Raft::new`를 부른 실행기에
    /// 생긴다), 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시, 승격은 `rt.core`, API는 `rt.api`, VM·풀을
    /// 다스리는 일과 스케줄러는 `rt.work`에서 돈다. 노드 간 서버가 받은 Raft·리스가 아닌 요청(마이그레이션
    /// 받기, 콘솔 중계, UEFI 변수·감사 로그·지표 읽기)도 `rt.work`에서 돈다.
    ///
    /// `runtimes`는 실행기 확인(`Liveness`)의 마지막 결과로, 헬스 체크(`GET /api/v1/health`)가 본다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
        runtimes: Option<Arc<dyn RuntimeProbe>>,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        // `[telemetry] ca_file`을 읽지 못하면 아무것도 열기 전에 실패한다.
        let telemetry_config = config.telemetry.telemetry_config()?;
        let id = NodeId(node);
        // Raft·리스 전달·ping 채널의 연결 작업은 core, 콘솔·마이그레이션·감사·지표 채널의 것은 work에서
        // 돈다(처음 연 쪽 실행기와 관계없이).
        let peers = Arc::new(Peers::new(&tls).with_runtimes(rt.core.clone(), rt.work.clone()));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let network = GrpcNetwork::new(peers.clone());
        // 저장소에 적힌 모드로 연다: 관찰 노드로 내려간 노드는 Raft 없이 투표 멤버 하나를 따른다(설계 §6).
        let cluster = Arc::new(
            run_on(
                &rt.core,
                Cluster::open_node(cluster_config, network, peers.clone()),
            )
            .await?
            .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap, rt).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        // 살아 있는 VM에 다시 붙으며 띄우는 QMP·모니터 작업이 `work`에 생긴다.
        let hv: Arc<dyn Hypervisor> =
            match run_on(&rt.work, QemuHypervisor::new(host.qemu_config(config))).await {
                Ok(hv) => Arc::new(hv),
                Err(e) => {
                    let _ = cluster.shutdown().await;
                    return Err(e.into());
                }
            };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        // API 서버, 노드 간 서버, Raft 감시가 보낸다.
        let (exited_tx, exited) = mpsc::channel(3);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
                migration_disks: config.data_dir.join(MIGRATION_DISKS_DIR),
            },
        );
        // API가 적고, 다른 노드가 노드 간 서버로 읽는다.
        let audit = match config.audit.audit_config(&config.data_dir, &config.name) {
            Some(audit) => {
                let dir = audit.dir.clone();
                match AuditLog::open(audit) {
                    Ok(audit) => Arc::new(audit),
                    Err(e) => {
                        let _ = cluster.shutdown().await;
                        return Err(anyhow::Error::new(e)
                            .context(format!("cannot open the audit log in {}", dir.display())));
                    }
                }
            }
            None => Arc::new(AuditLog::disabled()),
        };
        // 측정기가 적고, API와 다른 노드(노드 간 서버)가 읽는다.
        let metrics_store = Arc::new(MetricsStore::default());
        let metrics_source = Arc::new(StoreSource::new(node, metrics_store.clone()));
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone())
            .with_metrics(metrics_source.clone())
            .with_work(rt.work.clone());
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    if !leader_picks_voters_after_join(&cluster).await {
                        promote(&cluster, &peers, Some(&via)).await?;
                    }
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.mount_root = mount_root.clone();
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            state.trusted_proxies = config.api.trusted_proxies.clone();
            state.audit = audit;
            state.metrics = Some(metrics_source);
            // 헬스 체크가 실행기 확인과 HA 리스(아래에서 띄운 뒤)를 본다.
            state.runtimes = runtimes;
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            // API 리스너는 관리자 인증서(`[api] tls_cert`)나 노드 인증서로 시작한다. 노드 인증서면 API
            // 이름(클러스터 설정 `api_names`)이 있을 때 아래 `ApiCertAgent`가 열기 전에 바꿔 끼운다.
            let api_cert = match config.api.read_tls()? {
                Some((cert_pem, key_pem)) => ApiCert::new(&cert_pem, &key_pem)
                    .map_err(|e| anyhow::anyhow!("api.tls_cert/api.tls_key: {e}"))?,
                None => ApiCert::new(&tls.cert_pem, &tls.key_pem)?,
            };
            let api_tls = api_server_config_with(api_cert.clone())?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_cert, api_tls))
        };
        let (mut state, created, api_cert, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = on(&rt.core, || {
            spawn_promotion(first_time, false, &cluster, &peers)
        });
        // 전환은 Raft를 다시 띄우므로 core에서 돈다.
        let roles = on(&rt.core, || {
            RoleKeeper::spawn(cluster.clone(), RoleKeeperConfig::default())
        });
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx.clone()));
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = on(&rt.work, || {
            OverlayAgent::spawn(cluster.clone(), config, &host, tag)
        });
        let firewall = on(&rt.work, || firewall.map(FirewallAgent::spawn));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| on(&rt.core, || HaGuard::spawn(cluster.clone(), ha)));
        // 펜싱이 꺼져 있어도 리더가 생존을 리스로 보도록 갱신만 한다.
        let liveness = ha
            .is_none()
            .then(|| on(&rt.core, || renew_liveness(cluster.clone(), LIVENESS_RENEW)));
        reconciler_config.ha = ha.clone();
        state.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = on(&rt.work, || {
            Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config)
        });
        let pools = on(&rt.work, || {
            PoolMounter::spawn(
                cluster.clone(),
                mount_root.clone(),
                qemu_binary,
                config.pools.refresh(),
            )
        });
        // 지표는 메모리에만 둔다(Raft에 쓰지 않는다).
        let metrics = on(&rt.work, || {
            MetricsSampler::spawn(
                hv.clone(),
                metrics_host::native(),
                cluster.clone(),
                config.data_dir.clone(),
                metrics_store,
            )
        });
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        runner_config.migration_disk_timeout = config.migration.disk_timeout;
        // 받는 쪽(IncomingConfig)과 같은 디렉터리다.
        runner_config.migration_disks = Some(config.data_dir.join(MIGRATION_DISKS_DIR));
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        // 노드 비우기 Task가 이 노드의 API와 같은 확인으로 마이그레이션 Task를 만든다.
        let state = Arc::new(state);
        runner_config.api = Some(state.clone());
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
        let updates = config
            .updates
            .updates_config()
            .map(|c| Arc::new(Updates::new(c)));
        runner_config.updates = updates.clone();
        // Task 실행기의 고아 정리는 쓰기마다 10초까지 기다리므로 정족수 없이 다시 켤 때 systemd의 시작
        // 제한(90초)을 넘길 수 있다. 정리는 API보다 먼저 끝나야 하므로(API가 막 받는 업로드 파일을
        // 고아로 지우지 않게) 뒤로 미루지 않고 READY=1을 그 전에 보낸다(API가 뜬 뒤 다시 보낸다).
        Notifier::from_env().notify("READY=1");
        let runner = run_on(
            &rt.work,
            TaskRunner::spawn(
                cluster.clone(),
                storage,
                hv,
                reconciler.handle(),
                runner_config,
            ),
        )
        .await;
        let monitor = on(&rt.core, || {
            NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default())
        });
        let voter_keeper = on(&rt.core, || {
            VoterKeeper::spawn(cluster.clone(), VoterKeeperConfig::default())
        });
        let updates = on(&rt.work, || {
            updates.map(|u| UpdateAgent::spawn(cluster.clone(), node, u))
        });

        let api_cert = config.api.tls_cert.is_none().then(|| {
            on(&rt.work, || {
                ApiCertAgent::spawn(cluster.clone(), tls.clone(), api_cert)
            })
        });
        // TLS 핸드셰이크 작업이 `api`에 생긴다.
        let listener = on(&rt.api, || TlsListener::new(api_listener, api_tls))?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "orbivisord is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let (schedule, janitor, backups, telemetry, remotes) = on(&rt.work, || {
            (
                SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config()),
                ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config()),
                BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config()),
                TelemetryReporter::spawn(state.clone(), telemetry_config),
                RemotePoller::spawn(state.clone(), config.remotes.poller_config()),
            )
        });
        let recovery = on(&rt.core, || {
            HaManager::spawn(state.clone(), HaManagerConfig::default())
        });
        // VIP 리스 갱신과 떼기는 HA 리스처럼 core에서 돈다(api·work가 바빠도 제때 뗀다).
        let vip_manager = on(&rt.core, || {
            VipManager::spawn(cluster.clone(), VipManagerConfig::default())
        });
        let vip = config.vip.agent_config().map(|mut c| {
            // 붙인 VIP를 적어 두면 이 프로세스가 죽어도 다음 시작과 `--release-vips`가 뗀다.
            c.record = Some(VipRecord::in_dir(&config.data_dir));
            on(&rt.core, || {
                VipAgent::spawn(state.clone(), Arc::new(IpVip::default()), c)
            })
        });
        let audit = state.audit.clone();
        let app = opl_api::router(state);
        // 내장 UI를 끄면 API만 연다(UI는 따로 둔 포털이 준다).
        let app = if config.web.enabled {
            app.merge(crate::web::router())
        } else {
            app
        };
        let task = rt.api.spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                api_cert,
                reconciler,
                runner,
                monitor,
                voter_keeper,
                schedule,
                janitor,
                backups,
                telemetry,
                remotes,
                audit,
                recovery,
                vip_manager,
                vip,
                pools,
                metrics,
                overlay,
                firewall,
                ha,
                updates,
            }),
            register,
            promotion,
            roles,
            liveness,
            raft_watch,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = on(&rt.core, || {
            spawn_promotion(first_time, true, &cluster, &peers)
        });
        let roles = on(&rt.core, || {
            RoleKeeper::spawn(cluster.clone(), RoleKeeperConfig::default())
        });
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx));
        // 위트니스는 HA 펜싱이 없으므로 생존 표시로만 리스를 갱신한다.
        let liveness = on(&rt.core, || renew_liveness(cluster.clone(), LIVENESS_RENEW));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "orbivisord is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            roles,
            liveness: Some(liveness),
            raft_watch,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// 시험 고리가 쥐는 클러스터 저장소(약한 참조라 닫힌 저장소를 붙잡지 않는다)
    #[cfg(feature = "starve-it")]
    pub fn cluster_weak(&self) -> std::sync::Weak<Cluster> {
        Arc::downgrade(&self.cluster)
    }

    /// API 서버나 노드 간 서버가 스스로 멈추거나 Raft가 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        // 아래에서 저장소를 닫아 Raft가 멈춰도 실패로 알리지 않는다.
        self.raft_watch.abort();
        // 역할 전환을 먼저 멈춘다(진행 중인 전환은 끝까지 간다).
        self.roles.shutdown().await;
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        // 오프라인으로 기록한 뒤의 갱신이 다시 온라인으로 만들지 않게 먼저 멈춘다.
        if let Some(liveness) = &self.liveness {
            liveness.abort();
        }
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop orbivisord and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// Raft가 멈추면(저장소 쓰기 실패나 저장소 스레드 멈춤으로 openraft가 멈춘 경우) 그 이유를 `exited`로
/// 보낸다. 주 루프가 0이 아닌 코드로 끝나고 systemd(`Restart=on-failure`)가 다시 띄운다. 그러지 않으면
/// HA VM이 없는 노드는 API만 답하는 채로 남는다.
fn watch_raft(cluster: &Cluster, exited: mpsc::Sender<String>) -> JoinHandle<()> {
    let stopped = cluster.stopped();
    tokio::spawn(async move {
        let why = stopped.await;
        let _ = exited.send(format!("Raft stopped: {why}")).await;
    })
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다(리더가 투표 멤버를 고르면 위트니스만).
/// 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(
    first_time: bool,
    witness: bool,
    cluster: &Arc<Cluster>,
    peers: &Arc<Peers>,
) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers, witness).await })
}

/// 리더의 `VoterKeeper`가 투표 멤버를 고르는지(이 노드의 저장소로 본다). 그러면 위트니스가 아닌 노드는 승격을
/// 요청하지 않고 learner로 기다린다. 옛 노드가 남았으면 지금처럼 스스로 승격을 요청한다(옛 리더에는
/// `VoterKeeper`가 없다).
fn leader_picks_voters(cluster: &Cluster) -> bool {
    let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
        return false;
    };
    picks_voters(cluster.id(), &cluster.members(), nodes)
}

/// 이 노드(`me`) 말고 다른 멤버가 있고, 키퍼와 같은 규칙([`leader_manages_voters`])으로 막는 멤버가 없다. 노드는
/// 리더의 리스 표를 모르므로 이 임기에 갱신한 멤버가 없다고 본다: 키퍼보다 덜 막지 않아, 키퍼가 움직이는데 이
/// 노드가 스스로 승격하는 일(승격·강등 반복)은 없다.
fn picks_voters(me: NodeId, members: &Members, nodes: Vec<Typed<NodeSpec>>) -> bool {
    let voters: BTreeSet<NodeId> = members.voters.keys().copied().collect();
    let others: Vec<NodeId> = members
        .voters
        .keys()
        .chain(members.learners.keys())
        .copied()
        .filter(|id| *id != me)
        .collect();
    let nodes = nodes.into_iter().map(|n| (NodeId(n.id), n)).collect();
    !others.is_empty() && leader_manages_voters(others, &voters, &nodes, &BTreeMap::new())
}

/// 합류한 노드가 승격을 요청하지 않아도 되는지. 이 노드를 더한 멤버십까지 저장소에 적용되기를(앞서 합류한
/// 노드들의 Node 리소스가 보이기를) 잠시 기다린 뒤 [`leader_picks_voters`]로 본다. 기다리다 말면 지금처럼
/// 요청한다(상한을 넘으면 리더가 다시 내린다).
async fn leader_picks_voters_after_join(cluster: &Cluster) -> bool {
    let id = cluster.id();
    // 관찰 노드는 리더의 키퍼만 올린다(스스로 요청하지 않는다).
    let Some(raft) = cluster.raft() else {
        tracing::info!("this node is an observer; the leader picks the voting members");
        return true;
    };
    let applied = raft
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| membership_applied(m, id),
            "this node's membership applied",
        )
        .await
        .is_ok();
    let picks = applied && leader_picks_voters(cluster);
    if picks {
        tracing::info!(
            "the leader picks the voting members; staying a learner until it promotes this node"
        );
    }
    picks
}

/// 멤버십에 `id`가 있고 그 멤버십까지 저장소에 적용했다(그 전에 더한 노드들의 Node 리소스가 보인다).
fn membership_applied(m: &openraft::RaftMetrics<NodeId, BasicNode>, id: NodeId) -> bool {
    m.membership_config
        .membership()
        .nodes()
        .any(|(n, _)| *n == id)
        && m.last_applied.map(|l| l.index) >= m.membership_config.log_id().map(|l| l.index)
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다. 리더가 투표 멤버를 고르면
/// ([`leader_picks_voters`]) 위트니스가 아닌 노드는 요청하지 않고 learner로 남는다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers, witness: bool) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되고 저장소에 적용되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지
    // 모른다(적용해야 다른 노드의 Node 리소스로 리더가 투표 멤버를 고르는지 안다). 그 사이 관찰 노드로 바뀌면
    // Raft가 바뀌므로 매번 다시 읽는다.
    loop {
        // 관찰 노드(Raft 구성원이 아님)는 리더의 키퍼가 learner로, 그 뒤 투표 멤버로 올린다.
        let Some(raft) = cluster.raft() else {
            tracing::info!("this node is an observer; the leader picks the voting members");
            return;
        };
        match raft
            .wait(Some(Duration::from_secs(10)))
            .metrics(
                |m| membership_applied(m, id),
                "this node's membership applied",
            )
            .await
        {
            Ok(_) => break,
            // 전환으로 꺼진 Raft다. 새 백엔드를 다시 읽는다.
            Err(openraft::metrics::WaitError::ShuttingDown) => {
                tokio::time::sleep(PROMOTE_RETRY_MIN).await;
            }
            Err(openraft::metrics::WaitError::Timeout(..)) => {
                tracing::info!(
                    "this node is not in the cluster membership it knows yet; still waiting"
                );
            }
        }
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        // 위트니스는 늘 투표 멤버라 스스로 요청한다.
        if !witness && leader_picks_voters(cluster) {
            tracing::info!(
                "the leader picks the voting members; staying a learner until it promotes this node"
            );
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .context("this node is not a Raft member")?
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;
    use opl_types::resource::Resource;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// `dir` 아래에 데이터를 두는 단일 노드 멤버의 설정
    fn member_config(dir: &std::path::Path) -> Config {
        let mut config: Config = toml::from_str(&format!(
            "name = \"n1\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{}\"\n\
             cluster_listen = \"127.0.0.1:{}\"\n[hypervisor]\naccel = \"tcg\"\n\
             systemd_scope = false\n[ha]\nfencing = \"process\"\n[updates]\nenabled = false",
            free_port(),
            free_port()
        ))
        .unwrap();
        config.data_dir = dir.join("data");
        config.run_dir = dir.join("run");
        config
    }

    /// 새 클러스터를 만든 멤버의 서비스가 실행기 셋에 나뉘어 돈다. 부른 쪽의 실행기에는 아무 작업도
    /// 남지 않고(고르지 않고 띄운 서비스가 없다), 닫으면 세 실행기의 작업이 모두 끝난다.
    #[test]
    fn services_run_on_the_three_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let alive = |h: &tokio::runtime::Handle| h.metrics().num_alive_tasks();
        // 주 루프 대신 부르는 쪽
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (member, created) = Member::start(&config, node, tls, bootstrap, &rt, None)
                .await
                .unwrap();
            assert!(created.is_some());
            let here = tokio::runtime::Handle::current();
            assert_eq!(alive(&here), 0, "tasks left on the caller's runtime");
            for (name, h) in [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)] {
                assert!(alive(h) > 0, "no task on {name}");
            }
            // 빠진 것처럼 닫아 root로 도는 Linux CI에서 만든 방화벽 테이블도 지운다.
            member.close(true).await.unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while [&rt.core, &rt.api, &rt.work].iter().any(|h| alive(h) > 0) {
            assert!(
                std::time::Instant::now() < deadline,
                "tasks left after close: core {} api {} work {}",
                alive(&rt.core),
                alive(&rt.api),
                alive(&rt.work)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Raft가 멈추면 API와 노드 간 서버가 돌아도 `failed`가 그 이유와 함께 끝난다(여기서는 저장소를
    /// 바깥에서 닫는다. 저장소 쓰기가 panic해 openraft가 멈추는 경우는 opl-cluster 시험이 본다).
    #[test]
    fn failed_ends_when_raft_stops() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (mut member, _) = Member::start(&config, node, tls, bootstrap, &rt, None)
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(300), member.failed())
                    .await
                    .is_err()
            );
            member.cluster().shutdown().await.unwrap();
            let e = tokio::time::timeout(Duration::from_secs(10), member.failed())
                .await
                .unwrap();
            assert!(e.to_string().starts_with("Raft stopped: "), "{e}");
            let _ = member.close(true).await;
        });
    }

    /// 가입·재시작한 노드가 스스로 승격을 요청할지는 키퍼와 같은 규칙이다: 합류하자마자 죽어 등록하지 못한
    /// learner(기능·버전 없는 Node 기록이나 Node 기록 없음)가 있어도 리더가 고른다고 본다(키퍼가 움직이는데 이
    /// 노드가 스스로 올라가면 키퍼가 다시 내려 반복한다). 기능을 모르는 투표 멤버·옛 노드가 있으면 스스로 요청한다.
    #[test]
    fn a_joining_node_follows_the_keepers_rule() {
        let id = |n: u128| NodeId(Uuid::from_u128(n));
        let node = |n: u128, features: &[&str], version: Option<&str>| {
            let mut node: Typed<NodeSpec> = Resource::new(
                format!("n{n}"),
                NodeSpec {
                    address: format!("h{n}:8006"),
                    maintenance: false,
                    witness: false,
                    roles: None,
                },
            );
            node.id = id(n).0;
            node.status.features = features.iter().map(|f| f.to_string()).collect();
            node.status.version = version.map(str::to_string);
            node
        };
        let members = |voters: &[u128], learners: &[u128]| Members {
            leader: Some(id(1)),
            voters: voters.iter().map(|n| (id(*n), String::new())).collect(),
            learners: learners.iter().map(|n| (id(*n), String::new())).collect(),
            observers: Default::default(),
        };
        let limit = opl_cluster::VOTER_LIMIT;
        let ok = || node(1, &[limit], Some("0.3.0"));
        // 나(4)와 등록 못 한 learner(5: 기능·버전 없음, 6: Node 기록 없음)
        let nodes = vec![ok(), node(2, &[limit], None), node(5, &[], None)];
        assert!(picks_voters(
            id(4),
            &members(&[1, 2], &[4, 5, 6]),
            nodes.clone()
        ));
        // 옛 learner(버전이 있다)는 막는다.
        let old = vec![ok(), node(2, &[limit], None), node(5, &[], Some("0.2.0"))];
        assert!(!picks_voters(id(4), &members(&[1, 2], &[4, 5]), old));
        // 기능을 모르는 투표 멤버는 등록하지 않았어도 막는다.
        assert!(!picks_voters(id(4), &members(&[1, 2, 5], &[4]), nodes));
        // 혼자면 스스로 요청한다.
        assert!(!picks_voters(id(4), &members(&[], &[4]), vec![]));
    }

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                    roles: None,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1886개, 실패 0

Run: `cargo test -p opld --features ha-it --test ha`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 관찰 노드 전환과 VoterKeeper — learner ↔ 관찰 노드, RoleKeeper, opld 모드 열기"
```

---

### Task 5: 관찰 노드 규모 시험과 문서

규모 시험을 투표 5 + 대기 learner 2 + 나머지 관찰 노드로(바로 관찰 노드로 열고 목록에 넣음): 비투표 노드에 가는 Raft RPC < 15/s, 리더 RPC < 35 + 노드당 4.5, 관찰 노드 지연 p99, 투표 멤버 장애 뒤 5초 안에 모두 다시 붙음. 투표 멤버 RPC 지연은 2초/n(최소 20 ms, 최소 노드 20). `ClusterConfig::follow_seed`(시험 전용). 웹 ClusterPage의 역할 열, CORS `Access-Control-Expose-Headers: Orbivisor-Revision`, README(대규모 클러스터), 설계 문서 §15 "구현(계획 48)".

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `crates/opl-api/src/proxy.rs`
- Modify: `crates/opl-api/tests/proxy.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-cluster/tests/scale.rs`
- Modify: `docs/superpowers/specs/2026-11-11-availability-scale-design.md`
- Modify: `web/src/cluster/ClusterPage.test.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
      - run: cargo test -p opld --features starve-it --test starve
      # 노드 100개(투표 5, 나머지 learner)를 한 프로세스에 띄워 리더 RPC(확인 묶기 비율 포함)·쓰기 p99·노드 추가·
      # 리더 장애를 잰다(설계 §5·§14, 개발 Mac에서 15초쯤)
      - name: Scale test (100 nodes in one process)
```

을 다음으로 바꾼다:

```yaml
      - run: cargo test -p opld --features starve-it --test starve
      # 노드 100개(투표 5, 대기 learner 2, 나머지 관찰 노드)를 한 프로세스에 띄워 리더 RPC(확인 묶기 비율 포함)·쓰기
      # p99·관찰 노드 뒤처짐·노드 추가·투표 멤버와 리더 장애를 잰다(설계 §5·§6·§14·§15, 개발 Mac에서 16초쯤)
      - name: Scale test (100 nodes in one process)
```

`README.md`에서

````markdown
노드를 수백 대 더해도 리더가 느려지지 않게 하는 것들이다([설계 문서](docs/superpowers/specs/2026-11-11-availability-scale-design.md)
§5·§14).

```bash
orbivisorctl settings set voters=5   # 투표 멤버 상한 3|5|7(기본 5, 위트니스도 센다)
orbivisorctl node list               # VOTER 열: true 투표 멤버, false learner, - 멤버십에 없음
orbivisorctl cluster status          # ROLE 열: voter | learner
```
````

을 다음으로 바꾼다:

````markdown
노드를 수백 대 더해도 리더가 느려지지 않게 하는 것들이다([설계 문서](docs/superpowers/specs/2026-11-11-availability-scale-design.md)
§5·§6·§14·§15).

```bash
orbivisorctl settings set voters=5   # 투표 멤버 상한 3|5|7(기본 5, 위트니스도 센다)
orbivisorctl node list               # VOTER 열: true 투표 멤버, false learner·관찰 노드, - 멤버십에 없음
orbivisorctl cluster status          # ROLE 열: voter | learner | observer
```
````

`README.md`에서

```markdown
  `VoterKeeper`가 마무리한다(중간 구성이 바뀌지 않은 채 2초 넘게 보일 때만; 노드 삭제·`cluster leave`가 지운 노드는 그
  요청이 learner로 남지 않게 다시 확인해 지운다). 리더와 위트니스는 내리지 않는다(위트니스는 늘 투표 멤버). 나머지 노드는 learner로 모든 것을
  복제받으며 VM·API 일은 투표 멤버와 같다. 모든 멤버가 새 버전이 되기 전(롤링 업그레이드)에는 예전처럼 모든 노드가 투표
  멤버로 올라온다(합류하자마자 죽어 등록하지 못한 learner는 기다리지 않는다; 기다리는 동안 막는 노드를 임기마다 한 번
```

을 다음으로 바꾼다:

```markdown
  `VoterKeeper`가 마무리한다(중간 구성이 바뀌지 않은 채 2초 넘게 보일 때만; 노드 삭제·`cluster leave`가 지운 노드는 그
  요청이 learner로 남지 않게 다시 확인해 지운다). 리더와 위트니스는 내리지 않는다(위트니스는 늘 투표 멤버). 나머지 노드는 대기 learner 둘과
  관찰 노드(아래)이고 VM·API 일은 투표 멤버와 같다. 모든 멤버가 새 버전이 되기 전(롤링 업그레이드)에는 예전처럼 모든 노드가 투표
  멤버로 올라온다(합류하자마자 죽어 등록하지 못한 learner는 기다리지 않는다; 기다리는 동안 막는 노드를 임기마다 한 번
```

`README.md`에서

```markdown
  자리부터 다음 노드를 물어 느린 노드 하나가 나머지를 막지 않는다.
- **남은 것.** learner도 리더가 하트비트와 로그를 보내므로 리더의 RPC는 노드 수에 비례한다(노드 300대에서 초당 약
  1,500~2,100). 투표·learner가 아닌 관찰 노드는 계획 48이다. 리더 장애 뒤 새 리더까지는 노드 수와 관계없이 3~4초다
  (openraft가 마지막 하트비트 뒤 리더 리스 2초와 선거 제한 시간 1~2초를 기다린다).
- **규모 시험.** `cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`가 한 프로세스에 노드
  300개(투표 5, 나머지 learner, `OPL_SCALE_NODES`로 바꿈, CI는 100)를 띄우고 모든 노드가 리스를 갱신하는 채로 잰다.
  저장소는 메모리에 둔다(한 디스크에 수백 노드의 fsync가 몰리지 않게). 기준과 잰 값(Apple Silicon 10코어, 시험 빌드):

  | 항목 | 기준 | 노드 100 | 노드 300 |
  |---|---|---|---|
  | 리더의 초당 RPC(하트비트·리스 갱신과 그 확인, 아래 지연을 넣고) | 노드당 10 미만 | 570 | 1,320 |
  | 그 가운데 투표 멤버에게(묶지 않은 최악 = 하트비트 + 갱신 수 × 2 × 투표 멤버 수) | 최악의 0.6 미만 | 196(최악 520, 0.38) | 160(최악 1,600, 0.10) |
  | 쓰기 지연 p99(리더에서 하나씩) | 50 ms 미만(`OPL_SCALE_P99_MS`, CI 200 ms) | 8~11 ms | 31~33 ms |
  | 노드 하나 추가(learner로 따라잡기까지) | 2초 미만 | 9~21 ms | 65~150 ms |
  | 리더 장애 뒤 새 리더 | 5초 미만(설계 3초, `OPL_SCALE_FAILOVER_MS`, §14; 10초면 그 자리에서 실패) | 3.3~3.4초 | 3.2~3.7초 |

  CI 러너(코어 3~4개)는 노드 100개를 한 프로세스에서 돌리므로 쓰기 지연이 CPU에 묶여 흔들린다. 그래서 CI는 쓰기 p99
  기준만 `OPL_SCALE_P99_MS=200`으로 둔다(쓰기가 멈추거나 노드 수에 따라 크게 느려지는 것만 잡는다). 다른 기준(리더 RPC와
  확인 묶기 비율, 노드 추가, 리더 장애)은 CI의 노드 100에서도 같게 검사한다.

  리더 RPC를 재는 5초 동안은 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 20 ms 지연을 넣는다. 한 컴퓨터
  안에서는 확인 왕복이 갱신 간격보다 짧아 묶이는 정도가 기계·빌드 속도에 따라 달랐다. 지연을 넣으면 확인은 초당 많아야
  50번이라 노드 100에서도 비율이 0.42를 넘을 수 없고(잰 값 0.38, 배포 빌드도 0.38), 묶기를 빼면 노드 100·300 모두
  0.79~0.80으로 실패한다. 쓰기 지연·노드 추가·리더 장애는 지연 없이 잰다.
```

을 다음으로 바꾼다:

```markdown
  자리부터 다음 노드를 물어 느린 노드 하나가 나머지를 막지 않는다.
- **관찰 노드(계획 48).** 투표 멤버와 대기 learner 둘(`STANDBY_LEARNERS`) 말고 나머지 노드는 Raft 구성원이 아니다.
  learner에게도 리더가 하트비트(200 ms)와 로그를 보내므로 계획 47까지는 리더의 RPC가 노드 수에 비례했다(노드 300대에서
  초당 약 1,500~2,100). 관찰 노드는 리더가 아닌 투표 멤버 하나(노드마다 해시로 골라 고르게 퍼진다)에 노드 간 gRPC
  `Follow`로 붙어 스냅샷(처음·뒤처졌을 때)과 적용 묶음을 받아 자기 저장소에 쓴다. 그래서 리더의 Raft 일은 투표 멤버와
  learner 수에만 비례한다.
  - 읽기는 자기 복제본에서 한다(팔로워처럼 조금 늦을 수 있다). 쓰기와 HA·VIP 리스 갱신은 리더로 전달하고, 쓰기는 그
    리비전이 자기 복제본에 적용되기를 쓰기 제한 시간 가까이까지 기다린 뒤 돌려주므로 자기가 쓴 것은 보통 곧바로 읽힌다(적용이
    늦어도 쓰기는 성공으로 돌려주므로 잠깐 옛 값이 읽힐 수 있다). 늦은 적용이나 다른 노드에서 쓴 것을 확실히 읽으려면 쓴
    리소스의 `revision`을 요청 헤더 `Orbivisor-Min-Revision: <리비전>`으로 보낸다(그 리비전까지 5초 기다리고, 못
    따라잡으면 503). 응답 헤더 `Orbivisor-Revision`은 답할 때 그 노드가 적용한 리비전이다(쓰기 응답이라도 적용이 늦으면
    그 쓰기의 리비전보다 낮다). 기다린 뒤에는 다시 인증하므로 다른 노드에서 막 만든 토큰·사용자·비밀번호도 그
    리비전을 주면 쓸 수 있고, 기다리는 사이 폐기된 것은 거절된다. 인증되지 않은 요청은 그 노드가 있다고 아는 리비전일
    때만 기다리고 응답 헤더도 없다. 허용 출처(`web_origins`)의 웹 포털도 이 헤더를 읽을 수 있다
    (`Access-Control-Expose-Headers`).
  - 붙은 투표 멤버가 죽거나 3초 넘게 진행(1초마다 오는 `Progress`)이 없으면 다른 투표 멤버로 마지막 리비전부터 다시
    붙는다. 살아 있지만 리더를 따라가지 못하는 투표 멤버(전달한 쓰기·리스 갱신에 리더가 돌려준 리비전을 3초 넘게 알리지
    않는 노드)도 떠난다. 적용 기다림이 제한 시간보다 조금 일찍 끝나면 곧바로 다시 본다. 스냅샷을 받는 동안은 이 검사를
    하지 않아 오래 걸리는 스냅샷도 끝까지 받는다. 자기보다 뒤인 투표 멤버에는 붙지 않는다(상태가 뒤로 가지 않는다).
    기록(묶음 1024개)보다 뒤처졌으면 스냅샷을 다시 받는다. 지운 관찰 노드는 열린 스트림도 자기를 목록에서 빼는 묶음부터
    받지 못하고 끊긴다.
  - 리더의 `VoterKeeper`가 정한다: learner가 대기 둘보다 많고 모든 노드가 기능 `observer`를 알리면 건강한 learner를
    관찰 노드 목록(클러스터 정보)에 적고 멤버십에서 뺀다. 투표 멤버가 죽어 대기 learner가 모자라면 관찰 노드를 learner로
    되돌리고, 따라잡으면 보통의 승격으로 투표 멤버가 된다. 노드마다 `RoleKeeper`가 목록을 보고 Raft를 끄거나 다시 띄우며
    (관찰 노드가 되면 Raft 로그를 지우고 투표 기록은 남긴다. 다시 멤버가 될 때는 지운 로그의 마지막 인덱스까지 따라잡은
    뒤 Raft를 띄운다), 모드는 `cluster.redb`에 적혀 다시 시작해도 그대로다. 꺼진
    동안 내려진 노드는 적용이 30초 멈춘 것을 보고 투표 멤버에게 역할을 물어 관찰 노드가 된다. 새 노드는 지금처럼
    learner로 들어와 따라잡은 뒤 내려간다.
  - 헬스 체크(`GET /api/v1/health`)에서 관찰 노드는 붙은 투표 멤버의 진행이 5초 안에 왔고 리더를 알면 `quorum`이다.
    웹의 클러스터 화면과 `orbivisorctl cluster status`의 역할에 `observer`로 보인다.
  - 예외: HA·VIP 리스 갱신은 펜싱 안전 때문에 관찰 노드도 리더로 보낸다. 리더에는 노드당 초당 약 0.5 RPC(2초마다
    갱신)와 그 리더 확인(묶임)이 남는다.
  - 내리기 주의: 관찰 노드의 `cluster.redb`는 Raft 로그가 비어 있다(모드 `observer`). 계획 48 전 orbivisord는 이 모드를
    몰라 빈 로그로 Raft를 시작하므로 관찰 노드를 옛 버전으로 내리지 않는다. 내려야 하면 먼저 그 노드를 클러스터에서
    빼고(`cluster leave`) 옛 버전으로 다시 합류한다.
- **남은 것.** 리더 장애 뒤 새 리더까지는 노드 수와 관계없이 3~4초다(openraft가 마지막 하트비트 뒤 리더 리스 2초와 선거
  제한 시간 1~2초를 기다린다). 리스 갱신을 투표 멤버가 묶어 리더에 전달하는 것과 처음부터 관찰 노드로 합류하는 것은
  나중이다.
- **규모 시험.** `cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`가 한 프로세스에 노드
  300개(투표 5, 대기 learner 2, 나머지 관찰 노드, `OPL_SCALE_NODES`로 바꿈(20 이상), CI는 100)를 띄우고 모든 노드가 리스를
  갱신하며 `VoterKeeper`·`RoleKeeper`를 띄운 채로 잰다. 관찰 노드는 learner로 따라잡지 않고 빈 저장소로 바로 관찰 노드로
  연다(시작 시간을 줄임). 저장소는 메모리에 둔다(한 디스크에 수백 노드의 fsync가 몰리지 않게). 기준과 잰 값(Apple
  Silicon 10코어, 시험 빌드, 두 번):

  | 항목 | 기준 | 노드 30 | 노드 100 | 노드 300 |
  |---|---|---|---|---|
  | 리더의 초당 RPC(하트비트·리스 갱신과 그 확인, 아래 지연을 넣고) | 35 + 노드당 4.5 미만(계획 47은 노드당 10) | 93(노드당 3.1) | 254~255(2.5) | 352~355(1.2) |
  | 그 가운데 비투표 구성원(대기 learner 둘)에게 | 15 미만(노드 수와 관계없이) | 7 | 7 | 7 |
  | 그 가운데 투표 멤버에게(묶지 않은 최악 = 하트비트 + 갱신 수 × 2 × 투표 멤버 수) | 최악의 0.6 미만 | 72(최악 171, 0.42) | 198(최악 523~525, 0.38) | 200~201(최악 1,479~1,499, 0.13~0.14) |
  | 쓰기 지연 p99(리더에서 하나씩) | 50 ms 미만(`OPL_SCALE_P99_MS`, CI 200 ms) | 1.7~1.8 ms | 4.2~4.4 ms | 11.7~12.6 ms |
  | 관찰 노드의 뒤처짐 p99(리더가 쓰기를 마친 뒤 관찰 노드마다 적용하기까지) | 500 ms 미만 | 0.8~0.9 ms | 2.3~2.4 ms | 6.8~7.1 ms |
  | 노드 하나 추가(learner로 따라잡기까지) | 2초 미만 | 1 ms | 3 ms | 5~8 ms |
  | 투표 멤버 장애 뒤 모든 관찰 노드가 다시 붙어 새 쓰기를 받기까지 | 5초 미만 | 0.2초(10개가 붙어 있었음) | 1.2초(24개) | 1.2초(82개) |
  | 리더 장애 뒤 새 리더 | 5초 미만(설계 3초, `OPL_SCALE_FAILOVER_MS`, §14; 10초면 그 자리에서 실패) | 3.2초 | 3.3~3.6초 | 3.4~3.6초 |

  계획 47(나머지가 learner)의 같은 구간 리더 RPC는 노드 100에서 570, 노드 300에서 1,320이었고 그 3분의 2가 learner
  하트비트였다. 지금 노드 수에 따라 느는 것은 전달받은 리스 갱신(노드당 0.5)과 그 리더 확인뿐이다.

  CI 러너(코어 3~4개)는 노드 100개를 한 프로세스에서 돌리므로 쓰기 지연이 CPU에 묶여 흔들린다. 그래서 CI는 쓰기 p99
  기준만 `OPL_SCALE_P99_MS=200`으로 둔다(쓰기가 멈추거나 노드 수에 따라 크게 느려지는 것만 잡는다). 다른 기준은 CI의
  노드 100에서도 같게 검사한다.

  리더 RPC를 재는 5초 동안은 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 지연(2초/노드 수, 20 ms 이상 —
  노드 30에서 67 ms, 100부터 20 ms)을 넣는다. 한 컴퓨터 안에서는 확인 왕복이 갱신 간격보다 짧아 묶이는 정도가 기계·빌드
  속도에 따라 달랐다. 지연을 넣으면 확인은 초당 많아야 1/지연번이라 확인 요청(초당 노드 수)의 절반 이하로 묶이고, 묶기를
  빼면 노드 30·100 모두 0.79~0.80으로 실패한다. 계획 47은 노드 수와 관계없이 20 ms였고, 그러면 확인 요청이 초당 50번이
  안 되는 노드 50 미만에서는 묶일 까닭이 없어 노드 30에서 이 검사가 실패했다. 지연이 하트비트 간격(200 ms)에 가까워지면
  선거가 나므로 노드는 20 이상이어야 한다. 쓰기 지연·뒤처짐·노드 추가·장애는 지연 없이 잰다.

  투표 멤버 장애는 시험의 memnet이 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 연결을 끊어 1초 안에 알아챈다. 실제
  gRPC에서 연결이 끊기지 않고 멈추면 3초 동안 진행이 없는 것으로 알아채므로 다시 붙기까지 3초 남짓이다.
```

`crates/opl-api/src/proxy.rs`에서

```rust
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_MAX_AGE, ACCESS_CONTROL_REQUEST_HEADERS,
    ACCESS_CONTROL_REQUEST_METHOD, HOST, ORIGIN, UPGRADE, VARY,
};
```

을 다음으로 바꾼다:

```rust
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, HOST, ORIGIN, UPGRADE, VARY,
};
```

`crates/opl-api/src/proxy.rs`에서

```rust
use crate::limit::ClientIp;
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
use crate::limit::ClientIp;
use crate::revision::REVISION;
use crate::state::AppState;
```

`crates/opl-api/src/proxy.rs`에서

```rust
    let mut response = next.run(req).await;
    cors_headers(response.headers_mut(), origin);
    response
```

을 다음으로 바꾼다:

```rust
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    cors_headers(headers, origin);
    // 다른 출처의 스크립트는 노출한 헤더만 읽는다. 응답의 리비전을 다음 읽기의 최소 리비전으로 쓰게 한다.
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_name(REVISION),
    );
    response
```

`crates/opl-api/tests/proxy.rs`에서

```rust
    assert_eq!(resp.headers()["access-control-allow-credentials"], "true");
    let resp = whoami(&env.base, &cookie, &evil).await;
```

을 다음으로 바꾼다:

```rust
    assert_eq!(resp.headers()["access-control-allow-credentials"], "true");
    // 포털의 스크립트도 응답의 리비전을 읽어 다음 읽기의 `Orbivisor-Min-Revision`으로 보낼 수 있다.
    assert_eq!(
        resp.headers()["access-control-expose-headers"],
        "orbivisor-revision"
    );
    let resp = whoami(&env.base, &cookie, &evil).await;
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    pub in_memory: bool,
}
```

을 다음으로 바꾼다:

```rust
    pub in_memory: bool,
    /// 저장소의 멤버십에 투표 멤버가 없을 때 관찰 노드가 붙을 투표 멤버(노드 id → 클러스터 주소). 시험용(규모
    /// 시험이 노드를 learner로 따라잡게 하지 않고 빈 저장소로 바로 관찰 노드로 연다)이다.
    #[doc(hidden)]
    pub follow_seed: BTreeMap<NodeId, String>,
}
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            in_memory: false,
        }
```

을 다음으로 바꾼다:

```rust
            in_memory: false,
            follow_seed: BTreeMap::new(),
        }
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
                store.writer().run(enter_observer_mode).await?;
                Backend::Observer(Replica::start(config.node_id, &store, follow.clone())?)
            }
```

을 다음으로 바꾼다:

```rust
                store.writer().run(enter_observer_mode).await?;
                Backend::Observer(Replica::start(
                    config.node_id,
                    &store,
                    follow.clone(),
                    &config.follow_seed,
                )?)
            }
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            .await?;
        let replica = Replica::start(self.id, &self.store, follow)?;
        // 적용 지연은 백엔드마다 다른 것을 잰다(Raft 로그 대 붙은 노드·리더의 인덱스).
```

을 다음으로 바꾼다:

```rust
            .await?;
        let replica = Replica::start(self.id, &self.store, follow, &BTreeMap::new())?;
        // 적용 지연은 백엔드마다 다른 것을 잰다(Raft 로그 대 붙은 노드·리더의 인덱스).
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
                })?;
                let replica = Replica::start(self.id, &self.store, follow)?;
                *self.node.backend.write().unwrap() = Backend::Observer(replica);
```

을 다음으로 바꾼다:

```rust
                })?;
                let replica = Replica::start(self.id, &self.store, follow, &BTreeMap::new())?;
                *self.node.backend.write().unwrap() = Backend::Observer(replica);
```

`crates/opl-cluster/src/replica.rs`에서

```rust
impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다. 저장소의 적용 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
```

을 다음으로 바꾼다:

```rust
impl Replica {
    /// 저장소의 멤버십에서 후보를 읽고 복제 작업을 띄운다(멤버십에 투표 멤버가 없으면 `seed`). 저장소의 적용
    /// 위치(`revision`)부터 이어 받는다.
    pub(crate) fn start(
```

`crates/opl-cluster/src/replica.rs`에서

```rust
        follow: Arc<dyn FollowClient>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let voters = voter_addrs(&read_membership(&db)?);
        let inner = Arc::new(Inner {
```

을 다음으로 바꾼다:

```rust
        follow: Arc<dyn FollowClient>,
        seed: &BTreeMap<NodeId, String>,
    ) -> Result<Self, StoreError> {
        let db = store.db().clone();
        let mut voters = voter_addrs(&read_membership(&db)?);
        if voters.is_empty() {
            voters.clone_from(seed);
        }
        let inner = Arc::new(Inner {
```

`crates/opl-cluster/src/replica.rs`에서

```rust

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
```

을 다음으로 바꾼다:

```rust

    /// 빈 저장소로 바로 연 관찰 노드는 설정의 `follow_seed`(투표 멤버)에 붙어 스냅샷부터 받는다(규모 시험).
    #[tokio::test]
    async fn a_fresh_observer_follows_the_seed_voters() {
        let mut net = Net::start(0).await;
        net.settled_leader().await;
        let v = volume("before");
        net.leader().create(&v).await.unwrap();
        let id = NodeId::from(9);
        let mut config = config(&net.root, 9, true);
        config.follow_seed = (1..=3u64)
            .map(|i| (NodeId::from(i), format!("node-{i}")))
            .collect();
        let o = Cluster::open_observer(config, net.router.network(id), net.router.follower(id))
            .await
            .unwrap()
            .with_forwarder(net.router.forwarder(id));
        net.router.register(id, &o);
        net.observers.insert(id, o);
        let revision = net.sync().await;
        let o = net.observer(0);
        assert_eq!(o.revision(), revision);
        assert!(o.get::<VolumeSpec>(v.id).unwrap().is_some());
        assert_eq!(o.members().voters.len(), 3);
        net.shutdown().await;
    }

    /// 관찰 노드의 감시 이벤트는 투표 멤버의 것과 같다(같은 묶음을 그대로 낸다).
```

`crates/opl-cluster/src/replica.rs`에서

```rust

    /// 대본대로 받는 관찰 노드(투표 멤버 1만 든 멤버십을 둔 빈 저장소라 그 노드를 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let config = config(root, 9, false);
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let store = Store::open(&config.data_dir.join(crate::cluster::DB_FILE)).unwrap();
        let membership = Membership::new(
            None,
            openraft::Membership::new(
                vec![[NodeId::from(1)].into()],
                BTreeMap::from([(NodeId::from(1), BasicNode::new("node-1"))]),
            ),
        );
        let txn = store.db().begin_write().unwrap();
        {
            let mut meta = txn.open_table(META).unwrap();
            crate::store::put_json(&mut meta, KEY_LAST_MEMBERSHIP, &membership).unwrap();
        }
        txn.commit().unwrap();
        drop(store);
        let router = MemRouter::default();
        let script = Arc::new(script);
        loop {
            match Cluster::open_observer(
                config.clone(),
                router.network(NodeId::from(9)),
                script.clone(),
            )
            .await
            {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen))) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        }
    }
```

을 다음으로 바꾼다:

```rust

    /// 대본대로 받는 관찰 노드(빈 메모리 저장소, 투표 멤버 1을 따른다)
    async fn scripted_observer(root: &TempDir, script: Scripted) -> Cluster {
        let router = MemRouter::default();
        let id = NodeId::from(9);
        let mut config = config(root, 9, true);
        config.follow_seed = BTreeMap::from([(NodeId::from(1), "node-1".into())]);
        Cluster::open_observer(config, router.network(id), Arc::new(script))
            .await
            .unwrap()
    }
```

`crates/opl-cluster/tests/scale.rs` 전체:

```rust
//! 규모 시험(설계 §5·§6): 한 프로세스에 노드 여럿(기본 300, `OPL_SCALE_NODES`)을 memnet으로 띄운다. 투표 멤버
//! 다섯과 대기 learner 둘이 Raft 구성원이고 나머지는 관찰 노드다(투표 멤버 하나를 Follow로 따름). 모든 노드가
//! 2초마다 리스를 갱신하고 `VoterKeeper`·`RoleKeeper`를 띄운 채로 리더의 초당 RPC 수, 쓰기 지연 p99, 관찰 노드의
//! 뒤처짐 p99, 노드 하나 추가, 투표 멤버 장애 뒤 관찰 노드가 다시 붙기까지, 리더 장애 뒤 새 리더까지를 재고
//! 기준을 넘으면 실패한다.
//!
//! 관찰 노드는 실제 노드처럼 learner로 들어와 따라잡은 뒤 내려가지 않고 빈 저장소로 바로 관찰 노드로 연다(시작
//! 시간을 줄인다, `ClusterConfig::follow_seed`). 클러스터 정보의 관찰 노드 목록에는 넣는다(키퍼가 보는 모양을
//! 실제와 같게).
//!
//! 저장소는 모두 메모리에 둔다. 한 컴퓨터의 디스크 하나에 노드 수백 개(투표 멤버 다섯만이어도)의 fsync가
//! 몰리면 그 줄서기가 잰 값을 차지한다(실제 노드는 디스크가 따로다). 그래서 쓰기 지연은 Raft와 리더의 일만
//! 잰다. 디스크를 넣은 값은 설계 §14에 적었다.
//!
//! `cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`

#![cfg(feature = "scale-it")]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use openraft::error::{InstallSnapshotError, RPCError, RaftError};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::{MemConnection, MemFollow, MemForwarder, MemNetwork, MemRouter};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, FollowClient, FollowFrame, Forwarder, OBSERVER,
    RoleKeeper, RoleKeeperConfig, STANDBY_LEARNERS, Typed, VOTER_LIMIT, VoterKeeper,
    VoterKeeperConfig, ZONE_LABEL,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::VolumeSpec;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 투표 멤버 수(클러스터 설정 `voters`의 기본값)
const VOTERS: u64 = 5;
/// Raft 구성원 수(투표 멤버와 대기 learner). 나머지는 관찰 노드다.
const MEMBERS: u64 = VOTERS + STANDBY_LEARNERS as u64;
/// 리스 갱신 간격(opld `LIVENESS_RENEW`, `HaGuard`)
const RENEW: Duration = Duration::from_secs(2);
/// 쓰기 지연을 재는 쓰기 수
const WRITES: usize = 200;
/// 관찰 노드의 뒤처짐을 재는 쓰기 수와 그 간격
const STALENESS_WRITES: usize = 100;
const STALENESS_GAP: Duration = Duration::from_millis(20);
/// 리더가 쓰기를 적용한 뒤 모든 관찰 노드가 적용하기까지의 p99 기준. 관찰 노드는 리더가 아닌 투표 멤버를
/// 따르므로 그 투표 멤버가 커밋을 알기까지(다음 복제나 하트비트 200 ms)와 Follow 한 번이 더해진다.
const FOLLOW_STALENESS_P99: Duration = Duration::from_millis(500);
/// 투표 멤버 장애 뒤 모든 관찰 노드가 다른 투표 멤버로 다시 붙어 새 쓰기를 받기까지(관찰 노드는 진행이 3초
/// 없으면 다시 붙고, memnet은 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 끊는다).
const REATTACH_MAX: Duration = Duration::from_secs(5);

/// 리더 장애 뒤 새 리더가 자기 임기의 항목을 적용하기까지. 설계는 3초였지만 openraft 0.9.25는 리더가 있던
/// 팔로워가 마지막 하트비트 뒤 리더 리스(`election_timeout_max` 2초)와 선거 제한 시간(1~2초)을 더 기다린 뒤
/// 선거를 시작해 노드 수와 관계없이 3~4초다(노드 여섯에서도 같다). 그 위에 투표·빈 항목 확정과 바쁜 CI의
/// 여유로 1초를 둔다(설계 §14). `OPL_SCALE_FAILOVER_MS`로 바꾼다.
const FAILOVER_MAX_MS: u64 = 5000;
/// 리더 장애 뒤 새 리더를 기다리는 한도. 넘으면 기준과 관계없이 그 자리에서 실패한다.
const FAILOVER_GIVE_UP: Duration = Duration::from_secs(10);
/// 쓰기 지연 p99 기준(ms). 코어가 적은 CI 러너는 `OPL_SCALE_P99_MS`로 바꾼다(설계 §14, README).
const WRITE_P99_MAX_MS: u64 = 50;
const ADD_NODE_MAX: Duration = Duration::from_secs(2);
/// 리더가 비투표 구성원(대기 learner 둘)에게 보내는 초당 Raft RPC 상한: 하트비트 5(200 ms) × 둘 = 10에 쓰기
/// 복제의 여유. 관찰 노드에게는 보내지 않으므로 노드 수와 관계없다.
const NON_VOTER_RPCS_MAX: f64 = 15.0;
/// 노드 수와 관계없는 리더 RPC: 투표 멤버 넷에게 하트비트 20과 비투표 구성원 상한
const FIXED_RPCS_MAX: f64 = 20.0 + NON_VOTER_RPCS_MAX;
/// 노드(관찰 노드) 하나가 늘리는 리더의 초당 RPC 상한: 리스 갱신 전달 0.5, 그 갱신의 리더 확인을 묶지 못했을 때
/// 2번 × 투표 멤버 넷 = 4. 합 4.5(계획 47은 learner 하트비트 5를 더해 10이었다).
const RPCS_PER_NODE_MAX: f64 = 4.5;
/// 하트비트 간격(`raft_config`)
const HEARTBEAT: Duration = Duration::from_millis(200);
/// RPC를 재는 동안 투표 멤버로 가는 `append_entries`(하트비트·리더 확인)에 넣는 지연의 바닥.
/// [`voter_rpc_delay`]를 본다.
const VOTER_RPC_DELAY: Duration = Duration::from_millis(20);
/// 그때 투표 멤버에게 간 RPC가 묶지 않은 최악의 이 비율 아래여야 한다(노드 수와 관계없이, CI의 노드 100에서도).
/// 묶지 않게 고치면 노드 100·300 모두 0.79~0.80이었다(설계 §14).
const BATCHED_RATIO_MAX: f64 = 0.6;

/// 노드 수의 하한. 그보다 적으면 [`voter_rpc_delay`]가 하트비트 간격(200 ms)에 가까워져 선거가 나고 리스 갱신이
/// 실패한다(노드 8에서 지연 250 ms로 갱신 14번 실패, 노드 추가 실패).
const MIN_NODES: u64 = 20;

/// 투표 멤버로 가는 `append_entries`에 넣는 지연. 확인 왕복이 늘 갱신이 오는 간격보다 길어 기계·빌드와 관계없이
/// 확인이 묶이게 한다. 확인은 한 번에 하나라 초당 많아야 1/지연번이고, 확인 요청은 갱신(초당 n/2)마다 2번이라
/// 초당 n번이다. 지연을 2초/n(바닥 20 ms, 노드 20에서 100 ms)으로 두면 확인은 요청의 절반 이하로 묶이고, 투표
/// 멤버 RPC는 많아야 하트비트 20 + 확인 4 × n/2 = 20 + 2n, 묶지 않은 최악(20 + 10 × (n/2 + 0.5))의
/// (20 + 2n) / (25 + 5n)이다(노드 20에서 0.48, 30에서 0.46, 100에서 0.42). 노드 100부터는 20 ms라 확인이 초당
/// 50번까지다(노드 300에서 0.14). 잰 값: 노드 30 0.42, 100 0.37, 300 0.13. 확인을 묶지 않게 고친 빌드는 노드 30
/// 0.79, 100 0.80으로 실패했다. 계획 47처럼 노드 수와 관계없이 20 ms로 두면 확인 요청이 초당 50번이 안 되는
/// 노드 50 미만에서는 묶일 까닭이 없어 노드 30에서 0.79로 실패했다.
fn voter_rpc_delay(n: u64) -> Duration {
    VOTER_RPC_DELAY.max(Duration::from_secs(2) / u32::try_from(n).unwrap())
}

/// opld와 같은 Raft 설정(`member.rs` `raft_config`)
fn raft_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

/// 노드별 RPC 수: 그 노드가 투표 멤버·learner에게 보낸 Raft RPC와 리더로서 전달받은 요청(리스 갱신)
#[derive(Default)]
struct Counter {
    to_voters: AtomicU64,
    to_learners: AtomicU64,
    forwarded: AtomicU64,
}

type Counters = Arc<HashMap<NodeId, Counter>>;

/// 보낸 Raft RPC를 세는 네트워크
#[derive(Clone)]
struct Counting {
    inner: MemNetwork,
    from: NodeId,
    counters: Counters,
    /// 0이 아니면 투표 멤버로 가는 `append_entries`가 이만큼(마이크로초) 늦다.
    slow_voters: Arc<AtomicU64>,
}

impl RaftNetworkFactory<TypeConfig> for Counting {
    type Network = CountingConnection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> CountingConnection {
        CountingConnection {
            inner: self.inner.new_client(target, node).await,
            from: self.from,
            // 재는 동안 투표 멤버는 처음의 `1..=VOTERS` 그대로다.
            to_voter: (1..=VOTERS).any(|i| NodeId::from(i) == target),
            counters: self.counters.clone(),
            slow_voters: self.slow_voters.clone(),
        }
    }
}

struct CountingConnection {
    inner: MemConnection,
    from: NodeId,
    to_voter: bool,
    counters: Counters,
    slow_voters: Arc<AtomicU64>,
}

impl CountingConnection {
    fn count(&self) {
        let c = &self.counters[&self.from];
        if self.to_voter {
            &c.to_voters
        } else {
            &c.to_learners
        }
        .fetch_add(1, Ordering::Relaxed);
    }
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for CountingConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        self.count();
        let delay = self.slow_voters.load(Ordering::Relaxed);
        if self.to_voter && delay > 0 {
            tokio::time::sleep(Duration::from_micros(delay)).await;
        }
        self.inner.append_entries(rpc, option).await
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        self.count();
        self.inner.install_snapshot(rpc, option).await
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        self.count();
        self.inner.vote(rpc, option).await
    }
}

/// 리더가 전달받은 요청(리스 갱신)을 세는 전달자
struct CountingForwarder {
    inner: Arc<MemForwarder>,
    counters: Counters,
}

#[async_trait]
impl Forwarder for CountingForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        if let Some(c) = self.counters.get(&leader) {
            c.forwarded.fetch_add(1, Ordering::Relaxed);
        }
        self.inner.forward(leader, addr, req).await
    }
}

/// 관찰 노드가 마지막으로 붙은 투표 멤버(관찰 노드 → 투표 멤버)
type Upstreams = Arc<Mutex<HashMap<NodeId, NodeId>>>;

/// 관찰 노드가 어느 투표 멤버에 붙었는지 적는 Follow 통로
struct TrackingFollow {
    inner: Arc<MemFollow>,
    from: NodeId,
    upstreams: Upstreams,
}

#[async_trait]
impl FollowClient for TrackingFollow {
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        let frames = self.inner.follow(to, addr, since).await?;
        self.upstreams.lock().unwrap().insert(self.from, to);
        Ok(frames)
    }
}

/// 노드 하나: 저장소, 리스 갱신, VoterKeeper, RoleKeeper
struct Running {
    cluster: Arc<Cluster>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

struct Harness {
    router: MemRouter,
    counters: Counters,
    nodes: BTreeMap<NodeId, Running>,
    /// 실패한 리스 갱신 수
    renew_failures: Arc<AtomicU64>,
    /// 투표 멤버로 가는 RPC의 지연(마이크로초, RPC를 잴 때만)
    slow_voters: Arc<AtomicU64>,
    upstreams: Upstreams,
    /// 노드 수(리스 갱신 시작을 고르게 나눈다)
    total: u64,
}

impl Harness {
    /// 노드 `i`를 연다. 관찰 노드면 빈 저장소로 바로 관찰 노드로 열어 투표 멤버 `1..=VOTERS`를 따르고, 아니면
    /// 멤버로 연다(아직 멤버십에 없다).
    async fn open(&mut self, i: u64, observer: bool) {
        let id = NodeId::from(i);
        let mut config = ClusterConfig::new(id, format!("node-{i}"));
        config.raft = raft_config();
        config.write_timeout = Duration::from_secs(10);
        config.in_memory = true;
        let network = Counting {
            inner: self.router.network(id),
            from: id,
            counters: self.counters.clone(),
            slow_voters: self.slow_voters.clone(),
        };
        let follow = Arc::new(TrackingFollow {
            inner: self.router.follower(id),
            from: id,
            upstreams: self.upstreams.clone(),
        });
        let cluster = if observer {
            config.follow_seed = (1..=VOTERS)
                .map(|v| (NodeId::from(v), format!("node-{v}")))
                .collect();
            Cluster::open_observer(config, network, follow).await
        } else {
            Cluster::open_node(config, network, follow).await
        }
        .unwrap()
        .with_forwarder(Arc::new(CountingForwarder {
            inner: self.router.forwarder(id),
            counters: self.counters.clone(),
        }));
        self.router.register(id, &cluster);
        let cluster = Arc::new(cluster);
        // 노드마다 갱신 시작을 간격 안에서 고르게 늦춘다(실제 노드는 서로 맞춰 갱신하지 않는다).
        let offset =
            RENEW * u32::try_from(i % self.total).unwrap() / u32::try_from(self.total).unwrap();
        let renew = {
            let cluster = cluster.clone();
            let failures = self.renew_failures.clone();
            tokio::spawn(async move {
                tokio::time::sleep(offset).await;
                loop {
                    let sent = Instant::now();
                    if cluster.renew_lease(RENEW * 2).await.is_err() {
                        failures.fetch_add(1, Ordering::Relaxed);
                    }
                    tokio::time::sleep_until(sent + RENEW).await;
                }
            })
        };
        self.nodes.insert(
            id,
            Running {
                voters: VoterKeeper::spawn(cluster.clone(), VoterKeeperConfig::default()),
                roles: RoleKeeper::spawn(cluster.clone(), RoleKeeperConfig::default()),
                cluster,
                renew,
            },
        );
    }

    fn get(&self, id: NodeId) -> &Cluster {
        &self.nodes[&id].cluster
    }

    fn observers(&self) -> impl Iterator<Item = &Arc<Cluster>> {
        self.nodes
            .range(NodeId::from(MEMBERS + 1)..)
            .map(|(_, n)| &n.cluster)
            .filter(|c| c.raft().is_none())
    }

    /// `ids` 가운데 리더가 되어 자기 임기의 빈 항목까지 적용한 노드
    fn settled_leader(&self, ids: &[NodeId]) -> Option<NodeId> {
        ids.iter().copied().find(|id| {
            let m = self.get(*id).raft().unwrap().metrics().borrow().clone();
            let applied = m.last_applied.map(|l| l.index);
            m.state == ServerState::Leader && applied.is_some() && applied >= m.last_log_index
        })
    }

    async fn wait_leader(&self, ids: &[NodeId], within: Duration) -> NodeId {
        let deadline = Instant::now() + within;
        loop {
            if let Some(leader) = self.settled_leader(ids) {
                return leader;
            }
            assert!(Instant::now() < deadline, "no leader among {ids:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 노드 `i`의 Node 리소스(zone 셋에 고르게, 기능 `voter-limit`·`observer`)
    async fn register(&self, leader: NodeId, i: u64) {
        let mut node = Resource::new(
            format!("n{i}"),
            NodeSpec {
                address: format!("node-{i}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = NodeId::from(i).0;
        node.labels.insert(ZONE_LABEL.into(), format!("z{}", i % 3));
        node.status = NodeStatus {
            online: true,
            features: vec![VOTER_LIMIT.into(), OBSERVER.into()],
            ..NodeStatus::default()
        };
        self.get(leader).create(&node).await.unwrap();
    }

    /// 모든 관찰 노드가 `revision`까지 적용하기를 기다리고 가장 늦은 것까지 걸린 시간을 돌려준다.
    async fn observers_reach(&self, revision: u64, within: Duration) -> Duration {
        let t = Instant::now();
        for o in self.observers() {
            let left = within.saturating_sub(t.elapsed());
            if let Err(e) = o.wait_revision(revision, left).await {
                panic!(
                    "observer {} stayed at {} below {revision} for {within:?}: {e}",
                    o.id(),
                    o.revision()
                );
            }
        }
        t.elapsed()
    }

    /// 노드를 멈춘다(프로세스 종료에 해당). 네트워크에서 먼저 뺀다.
    async fn stop(&mut self, id: NodeId) {
        self.router.unregister(id);
        let node = self.nodes.remove(&id).unwrap();
        node.renew.abort();
        node.roles.shutdown().await;
        node.voters.shutdown().await;
        node.cluster.shutdown().await.unwrap();
    }

    async fn close(mut self) {
        for id in self.nodes.keys().copied().collect::<Vec<_>>() {
            self.stop(id).await;
        }
    }
}

fn volume(i: usize) -> Typed<VolumeSpec> {
    Resource::new(
        format!("disk-{i}"),
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

/// 리더 확인을 묶지 않았을 때(최악) 리더가 투표 멤버에게 보내는 초당 RPC: 팔로워마다 하트비트와, 리스 갱신(전달받은
/// 것과 리더 자신의 것)마다 확인 2번 × 투표 멤버 수. 확인 하나는 팔로워 넷에게 가지만 투표 멤버 다섯으로 세어 하트비트와
/// 겹친 것·재시도의 여유를 둔다(묶지 않게 고친 빌드가 팔로워 넷으로 센 값을 1% 넘었다).
fn unbatched_to_voters(forwarded: f64) -> f64 {
    let followers = (VOTERS - 1) as f64;
    let renewals = forwarded + 1.0 / RENEW.as_secs_f64();
    followers / HEARTBEAT.as_secs_f64() + renewals * 2.0 * VOTERS as f64
}

/// 쓰기 지연 p99 기준. 기본 50 ms, `OPL_SCALE_P99_MS`로 바꾼다.
fn write_p99_max() -> Duration {
    let ms = std::env::var("OPL_SCALE_P99_MS")
        .ok()
        .map_or(WRITE_P99_MAX_MS, |v| {
            v.parse().expect("OPL_SCALE_P99_MS must be a number")
        });
    Duration::from_millis(ms)
}

/// 리더 장애 뒤 새 리더까지의 기준. 기본 5초, `OPL_SCALE_FAILOVER_MS`로 바꾼다.
fn failover_max() -> Duration {
    let ms = std::env::var("OPL_SCALE_FAILOVER_MS")
        .ok()
        .map_or(FAILOVER_MAX_MS, |v| {
            v.parse().expect("OPL_SCALE_FAILOVER_MS must be a number")
        });
    Duration::from_millis(ms)
}

/// 노드 수. 기본 300, CI는 `OPL_SCALE_NODES=100`.
fn node_count() -> u64 {
    std::env::var("OPL_SCALE_NODES")
        .ok()
        .map(|v| v.parse().expect("OPL_SCALE_NODES must be a number"))
        .unwrap_or(300)
}

/// `window` 동안 리더가 쓴 초당 RPC 수(투표 멤버에게, learner에게, 전달받은 것)
async fn leader_rpcs(c: &Counter, window: Duration) -> [f64; 3] {
    let read = || {
        [
            c.to_voters.load(Ordering::Relaxed),
            c.to_learners.load(Ordering::Relaxed),
            c.forwarded.load(Ordering::Relaxed),
        ]
    };
    let before = read();
    tokio::time::sleep(window).await;
    let after = read();
    std::array::from_fn(|k| (after[k] - before[k]) as f64 / window.as_secs_f64())
}

/// 정렬된 값의 p50·p99
fn percentiles(sorted: &[Duration]) -> (Duration, Duration) {
    let len = sorted.len();
    (sorted[len / 2], sorted[(len * 99).div_ceil(100) - 1])
}

#[tokio::test(flavor = "multi_thread")]
async fn hundreds_of_nodes_do_not_slow_the_leader() {
    let n = node_count();
    assert!(
        n >= MIN_NODES,
        "OPL_SCALE_NODES must be at least {MIN_NODES}"
    );
    // 나중에 추가할 노드(n + 1)까지 센다.
    let counters: Counters = Arc::new(
        (1..=n + 1)
            .map(|i| (NodeId::from(i), Counter::default()))
            .collect(),
    );
    let mut h = Harness {
        router: MemRouter::default(),
        counters,
        nodes: BTreeMap::new(),
        renew_failures: Arc::default(),
        slow_voters: Arc::default(),
        upstreams: Arc::default(),
        total: n,
    };
    let started = Instant::now();
    for i in 1..=MEMBERS {
        h.open(i, false).await;
    }
    let voters: Vec<NodeId> = (1..=VOTERS).map(NodeId::from).collect();
    let members = (1..=VOTERS)
        .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
        .collect();
    h.get(voters[0]).initialize(members).await.unwrap();
    let leader = h.wait_leader(&voters, Duration::from_secs(10)).await;
    for i in VOTERS + 1..=MEMBERS {
        h.get(leader)
            .add_learner(NodeId::from(i), BasicNode::new(format!("node-{i}")))
            .await
            .unwrap();
    }
    // 클러스터 정보의 관찰 노드 목록(실제로는 리더의 VoterKeeper가 내리며 쓴다)
    let mut info = Resource::new(
        "c",
        ClusterSpec {
            ca_cert_pem: "CERT".into(),
            ca_key_pem: "KEY".into(),
        },
    );
    info.status = ClusterStatus {
        observers: (MEMBERS + 1..=n)
            .map(|i| (NodeId::from(i).0, format!("node-{i}")))
            .collect(),
    };
    h.get(leader).create(&info).await.unwrap();
    for i in 1..=n {
        h.register(leader, i).await;
    }
    for i in MEMBERS + 1..=n {
        h.open(i, true).await;
    }
    h.observers_reach(h.get(leader).revision(), Duration::from_secs(30))
        .await;
    assert_eq!(h.observers().count() as u64, n - MEMBERS);
    eprintln!("scale: {n} nodes up in {:?}", started.elapsed());

    // 안정된 뒤(모든 노드가 한 번 넘게 갱신) 리더의 초당 RPC 수: 하트비트, 리스 갱신과 그 확인. 투표 멤버로 가는
    // RPC를 늦춰 확인 왕복이 갱신 간격보다 길게 한다(묶기 검사가 기계 속도에 좌우되지 않게). 쓰기 지연·장애 조치는
    // 늦추지 않고 잰다.
    let delay = voter_rpc_delay(n);
    h.slow_voters
        .store(delay.as_micros() as u64, Ordering::Relaxed);
    tokio::time::sleep(RENEW + Duration::from_secs(1)).await;
    let failures_before = h.renew_failures.load(Ordering::Relaxed);
    let [to_voters, to_learners, forwarded] =
        leader_rpcs(&h.counters[&leader], Duration::from_secs(5)).await;
    let renew_failures = h.renew_failures.load(Ordering::Relaxed) - failures_before;
    h.slow_voters.store(0, Ordering::Relaxed);
    // 투표 멤버로 센 RPC는 처음의 `1..=VOTERS`로 가는 것이다. 재는 동안 키퍼가 바꾸지 않았는지 본다.
    let measured = h.get(leader).members();
    assert_eq!(
        measured.voters.keys().copied().collect::<Vec<_>>(),
        voters,
        "the voting members changed while measuring"
    );
    let standby: BTreeSet<NodeId> = (VOTERS + 1..=MEMBERS).map(NodeId::from).collect();
    assert_eq!(
        measured.learners.keys().copied().collect::<BTreeSet<_>>(),
        standby,
        "the standby learners changed while measuring"
    );
    let rpcs = to_voters + to_learners + forwarded;
    let unbatched = unbatched_to_voters(forwarded);
    eprintln!(
        "scale: leader RPCs/s {rpcs:.0} ({:.2} per node; to voters {to_voters:.0} of {unbatched:.0} \
         unbatched with {delay:?} delay, ratio {:.2}, to learners {to_learners:.0}, forwarded \
         renewals {forwarded:.0}), failed renewals {renew_failures}",
        rpcs / n as f64,
        to_voters / unbatched
    );

    // 쓰기 지연(리더에서 하나씩)
    let mut latencies = Vec::with_capacity(WRITES);
    for i in 0..WRITES {
        let t = Instant::now();
        h.get(leader).create(&volume(i)).await.unwrap();
        latencies.push(t.elapsed());
    }
    latencies.sort();
    let (p50, p99) = percentiles(&latencies);
    eprintln!(
        "scale: write p50 {p50:?}, p99 {p99:?}, max {:?}",
        latencies[WRITES - 1]
    );

    // 관찰 노드의 뒤처짐: 리더가 쓰기를 마친(적용한) 때부터 관찰 노드 하나하나가 그것을 적용하기까지
    let mut waits = Vec::new();
    for i in 0..STALENESS_WRITES {
        let revision = h.get(leader).create(&volume(WRITES + i)).await.unwrap();
        let t = Instant::now();
        for o in h.observers() {
            let o = o.clone();
            waits.push(tokio::spawn(async move {
                o.wait_revision(revision, Duration::from_secs(10))
                    .await
                    .map(|_| t.elapsed())
            }));
        }
        tokio::time::sleep(STALENESS_GAP).await;
    }
    let mut staleness = Vec::with_capacity(waits.len());
    for w in waits {
        staleness.push(w.await.unwrap().expect("an observer fell behind for 10s"));
    }
    staleness.sort();
    let (stale_p50, stale_p99) = percentiles(&staleness);
    eprintln!(
        "scale: observer staleness p50 {stale_p50:?}, p99 {stale_p99:?}, max {:?} ({} samples)",
        staleness[staleness.len() - 1],
        staleness.len()
    );

    // 노드 하나 추가(learner로 들어와 로그를 따라잡기까지)
    let added = n + 1;
    h.open(added, false).await;
    let t = Instant::now();
    h.get(leader)
        .add_learner(NodeId::from(added), BasicNode::new(format!("node-{added}")))
        .await
        .unwrap();
    let add = t.elapsed();
    eprintln!("scale: added a node in {add:?}");

    // 투표 멤버 장애: 관찰 노드가 가장 많이 붙은 투표 멤버(리더가 아닌 것)를 끊고 곧바로 쓴다. 끊긴 노드는 그
    // 쓰기를 받지 못하므로 그 노드를 따르던 관찰 노드는 다른 투표 멤버로 다시 붙어야 받는다.
    let followers = |v: NodeId| {
        h.upstreams
            .lock()
            .unwrap()
            .values()
            .filter(|u| **u == v)
            .count()
    };
    let failed = voters
        .iter()
        .copied()
        .filter(|v| *v != leader)
        .max_by_key(|v| followers(*v))
        .unwrap();
    let moved = followers(failed);
    assert!(
        moved > 0,
        "no observer follows a voter other than the leader"
    );
    let rest: Vec<NodeId> = voters
        .iter()
        .copied()
        .filter(|v| *v != leader && *v != failed)
        .collect();
    let t = Instant::now();
    h.router.isolate(failed);
    let revision = h.get(leader).create(&volume(usize::MAX)).await.unwrap();
    h.observers_reach(revision, FAILOVER_GIVE_UP).await;
    let reattach = t.elapsed();
    eprintln!(
        "scale: all observers past the failed voter {failed} ({moved} followed it) in {reattach:?}"
    );

    // 리더 장애: 네트워크에서 끊는 순간부터 남은 투표 멤버 가운데 새 리더가 자기 임기의 항목을 적용하기까지
    let t = Instant::now();
    h.router.isolate(leader);
    let elected = loop {
        let leading = rest
            .iter()
            .any(|id| h.get(*id).raft().unwrap().metrics().borrow().state == ServerState::Leader);
        if leading {
            break t.elapsed();
        }
        assert!(
            t.elapsed() < FAILOVER_GIVE_UP,
            "no new leader among {rest:?} within {FAILOVER_GIVE_UP:?} after isolating the leader {leader}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    h.wait_leader(&rest, FAILOVER_GIVE_UP.saturating_sub(t.elapsed()))
        .await;
    let failover = t.elapsed();
    eprintln!("scale: new leader elected in {elected:?}, ready in {failover:?}");

    h.close().await;

    assert_eq!(renew_failures, 0, "lease renewals failed");
    // 관찰 노드에게는 Raft RPC를 보내지 않는다.
    assert!(
        to_learners < NON_VOTER_RPCS_MAX,
        "leader Raft RPCs/s to non-voters {to_learners:.0} (max {NON_VOTER_RPCS_MAX})"
    );
    assert!(
        rpcs < FIXED_RPCS_MAX + RPCS_PER_NODE_MAX * n as f64,
        "leader RPCs/s {rpcs:.0} for {n} nodes"
    );
    // 확인 왕복이 갱신 간격보다 길면 묶기가 투표 멤버로 가는 RPC를 묶지 않은 최악보다 크게 줄인다.
    assert!(
        to_voters < unbatched * BATCHED_RATIO_MAX,
        "leader RPCs/s to voters {to_voters:.0}: confirmations are not batched \
         (unbatched {unbatched:.0})"
    );
    let p99_max = write_p99_max();
    assert!(p99 < p99_max, "write p99 {p99:?} (max {p99_max:?})");
    assert!(
        stale_p99 < FOLLOW_STALENESS_P99,
        "observer staleness p99 {stale_p99:?} (max {FOLLOW_STALENESS_P99:?})"
    );
    assert!(add < ADD_NODE_MAX, "adding a node took {add:?}");
    assert!(
        reattach < REATTACH_MAX,
        "observers took {reattach:?} to follow another voter (max {REATTACH_MAX:?})"
    );
    let failover_max = failover_max();
    assert!(
        failover < failover_max,
        "failover took {failover:?} (max {failover_max:?})"
    );
}
```

`docs/superpowers/specs/2026-11-11-availability-scale-design.md`에서

```markdown
  넘기는 규칙은 뒤 계획으로 미룬다.
```

을 다음으로 바꾼다:

```markdown
  넘기는 규칙은 뒤 계획으로 미룬다.

## 15. 구현(계획 48)

다섯 단계를 커밋 다섯으로 했다(1 백엔드 틈과 공통 적용 리비전, 2 Follow 피드와 복제본, 3 gRPC Follow·관찰 노드 목록·API,
4 전환·VoterKeeper·opld, 5 규모 시험과 문서). 설계 §6과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**역할과 수.**
- 대기 learner는 상수 `STANDBY_LEARNERS = 2`다(설정으로 두는 것은 나중). 위트니스는 투표 멤버로 센다. 투표 멤버와 대기
  learner가 아닌 노드가 관찰 노드다.
- 가입은 지금처럼 learner로 하고 따라잡은 뒤 키퍼가 관찰 노드로 내린다. 처음부터 관찰 노드로 가입하는 것은 나중이다.
- 바꿀 의도는 클러스터 정보의 `ClusterStatus.observers`(노드 id → 클러스터 주소)다. 리더의 `VoterKeeper`만 쓰고, 옛
  노드는 모르는 필드를 무시한다(비면 쓰지 않는다). `Members.observers`는 그 가운데 Raft 멤버십에 없는 것이다(내려가는
  중인 노드는 멤버로 센다). 노드 기능 `observer`를 모든 Raft 구성원과 관찰 노드가 알릴 때만 내린다.

**리비전과 적용 표시.**
- 리비전은 Raft 로그 인덱스(META `last_applied`)다. blank·멤버십·거절된 명령도 `last_applied`를 올리지만 감시 이벤트를
  내지 않아 리비전에 틈이 있다. 그래서 `WatchHub`는 적용한 항목마다 `FeedBatch { events, applied: LogId, membership }`를
  쌓는다(이벤트 없는 항목도 적용 표시로). 공개 `Watch`는 이벤트로 펼치고, 기록(`HISTORY` 1024)은 이제 묶음 수다.
- `Cluster::revision()`·`wait_revision(rev, timeout)`은 `WatchHub` 안의 적용 위치 채널을 본다. 쓰기 전달·`wait_local`·
  `renew_lease`·`renew_vip`의 적용 기다림(`raft.wait().applied_index_at_least`)을 이것으로 바꿔 관찰 노드에서도 같다.
  리스 기다림이 blank 항목에서 멈추지 않는다.
- `Cluster`는 저장소, Raft를 다시 만드는 팩토리, `NodeHandle`(백엔드 `Member { raft, confirm } | Observer(Replica)`와 리스
  표)을 갖는다. `Cluster::raft()`는 `Option`이고, 관찰 노드에서 `read_barrier`·리더 일·`initialize`·멤버십 변경은
  `NotLeader`(아는 리더를 담음), `leases()`·`vip_leases()`는 빈 값이다. memnet은 노드의 `NodeHandle`을 등록해 RPC마다
  지금 백엔드를 읽는다.

**Follow 피드(§6의 `Follow { since_revision }`).**
- 프레임은 `Hello { role, leader, voters }`, `SnapshotPart { objects }`, `SnapshotEnd { applied, membership }`,
  `Batch { events, applied, membership }`, `Progress { applied, leader, leader_addr }`(JSON, 태그 `frame`)다. 모든 `Batch`·
  `Progress`가 적용한 `LogId`를 싣는다(리비전 틈 때문에, 또 다시 Raft 구성원이 될 때 로그를 이어 쓰게). `/watch`의
  이벤트 형식을 그대로 쓴다.
- 서버(`Cluster::follow(caller, since)`)는 `since`가 기록보다 오래면 지금 상태의 스냅샷(리소스 256개 또는 JSON 약 4 MiB씩
  조각, gRPC 메시지 한도 64 MiB)부터, 자기가 스냅샷을 설치하면(`Reset`) 다시 스냅샷을 보낸다. 1초마다 `Progress`다
  (스냅샷을 뜨는 동안에도 보낸다 — 큰 상태를 읽는 동안 받는 쪽이 진행이 없다고 끊지 않게).
  이벤트 JSON이 약 4 MiB를 넘는 묶음은 앞부분을 `BatchPart { events }`로 나누고 마지막 `Batch`가 적용 위치를 실어,
  관찰 노드는 마지막 프레임에서 한 번에 적용한다(끊기면 앞부분을 버린다).
- `since`가 서버의 리비전보다 앞선 호출자(다른 투표 멤버에서 옮겨 온 관찰 노드)는 받지 않는다(`Hello`만 보내고 닫음).
  받으면 뒤처진 서버가 그 사이 리더의 스냅샷(관찰 노드보다 낮을 수 있음)을 설치할 때 관찰 노드가 그것으로 바뀌어
  리비전이 뒤로 가고, 이미 돌려준 리비전 읽기·리스 갱신이 거짓이 된다(옮긴 VM이 다시 자기 것으로 보여 이중 실행
  위험). 투표 멤버 자신은 커밋보다 낮은 스냅샷을 설치하지 않는다(openraft 0.9.25 `install_full_snapshot`). 복제본도
  자기보다 낮은 스냅샷은 설치하지 않는다(이중 방어).
  서버는 보내고 읽히지 않은 프레임이 16개면 보내기를 멈추고, 그동안 서버의 피드가 기록(`HISTORY` 1024 묶음)보다
  뒤처지면 `Lagged`로 끝나 느린 관찰 노드는 다시 붙는다. 같은 묶음은 스트림이 몇이든 한 번만 프레임으로 나눈다
  (이벤트 복제와 JSON 크기 재기를 관찰 노드마다 하지 않게). 구성원이 아닌 서버는
  `Hello`만 보내고 닫고(다른 데로 가라), Raft가 멈추거나 서버가 멤버십에서 빠지면 끝난다. `Hello.role`은 서버가 보는
  부른 쪽의 역할이다.
- gRPC는 서버 스트리밍 `Follow`(`FollowRequest { since }` → `FollowFrame` JSON, 제어 채널)이고 서버는 `core` 실행기에서
  답한다. `authorize`는 관찰 노드 목록의 노드도 받고 목록과 멤버십 모두에서 빠진 노드는 거절한다. 열린 Follow
  스트림도 프레임마다 다시 본다(`FollowGate`): 부른 쪽이 Raft 멤버십이나 관찰 노드 목록에 있어야 하고, 목록은 그
  스트림의 프레임에 실린 클러스터 정보의 것을 먼저 본다(노드의 목록 캐시는 따로 갱신되어 늦을 수 있다). 그래서 지운
  관찰 노드는 자기를 목록에서 빼는 묶음부터 받지 못하고 스트림이 끝나 그 뒤의 쓰기(CA 키 같은 비밀 포함)를 받지
  못한다. 관찰 노드에 온 Raft RPC는 `failed_precondition`이다.

**복제본.**
- 관찰 노드는 리더가 아닌 투표 멤버 가운데 자기와의 rendezvous 해시가 가장 큰 것에 붙는다(관찰 노드가 고르게 퍼진다,
  없으면 리더). 리더에 붙었고 다른 투표 멤버가 있으면 옮긴다(다른 투표 멤버가 모두 실패해 리더에 온 경우는 머문다).
  오류·끝·3초 진행 없음(`FOLLOW_STALL`)이면 자기 리비전부터 다시 붙고, 진행 없이 실패한 노드는 다음 고르기에서 건너뛴다.
- 살아 있어 `Progress`는 보내지만 리더를 따라가지 못하는 투표 멤버도 떠나 건너뛴다(리스 갱신이 계속 실패해 스스로
  펜싱하지 않게): 전달한 쓰기·리스 갱신이 돌려준 리더의 인덱스를 붙은 노드가 3초(`UPSTREAM_LAG_MAX`) 넘게 알리지
  않으면(`Progress`·묶음·스냅샷의 적용 위치 — 이 노드의 적용이 아니라 붙은 노드가 알린 것으로 재, 이 노드의 디스크가
  느린 것을 붙은 노드 탓으로 돌리지 않는다). 전달 경로의 적용 기다림이 시간 초과하면 곧바로 다시 본다(그때 붙은 노드가
  그 인덱스를 알렸으면 머문다). 이 기다림은 남은 시간의 90%에서 끝내, 같은 제한 시간을 건 리스 갱신의 바깥
  `timeout`이 future를 버리기 전에 닿는다. 스냅샷 조각을 받는 동안(첫 `SnapshotPart`부터 `SnapshotEnd`까지)과 나눈
  묶음을 받는 동안은 재지 않고, 스냅샷을 설치하면 새로 잰다(스냅샷이 3초 넘게 걸려도 끊고 다른 투표 멤버에서 처음부터
  다시 받기를 되풀이하지 않게). 헬스의 `apply_lag`도 그 인덱스를 본다.
- 스냅샷 조각은 `SnapshotEnd`까지, 나눈 묶음은 `Batch`까지 이어 와야 한다(서버는 그 사이에 다른 프레임을 넣지
  않는다). 아니면 모은 것을 버리고 다시 붙는다. 자기와 같은 리비전의 스냅샷은 설치하지 않는다(헛된 `Reset`이 없게).
- 투표 멤버마다 적용 묶음의 경계가 달라 옮긴 뒤 첫 묶음이 이미 적용한 리비전의 이벤트를 품을 수 있다. 그 이벤트는
  쓰지도 감시에 내지도 않는다(적용 위치는 묶음의 것).
- `apply_feed`는 리소스·이름·`last_applied`·멤버십을 한 쓰기 트랜잭션에 쓰고, 같은 묶음을 저장소 스레드에서 로컬
  `WatchHub`에 낸다(감시 이벤트가 투표 멤버와 같다). 스냅샷은 기존 상태 머신 설치와 같다.
- 쓰기는 리더로 바로 전달하고 돌려받은 리비전을 `wait_revision`으로 기다린다(보통 자기 쓰기를 곧바로 읽는다, 아래
  "쓰기 뒤 읽기").

**읽기와 헬스.**
- 기본 읽기는 지금 팔로워처럼 로컬이다. 요청 헤더 `Orbivisor-Min-Revision`이 있으면 그 리비전까지 5초 기다리고 넘으면
  503 `revision_unavailable`이다. `/api/v1`의 응답에 `Orbivisor-Revision`(답할 때 그 노드가 적용한 리비전)을 붙이고, 허용
  출처의 CORS 응답은 `Access-Control-Expose-Headers: orbivisor-revision`으로 그것을 포털 스크립트에 보인다. 기다림은
  인증(`Principal` 추출)에 들어 있다: 기다린 뒤 다시 인증하므로 다른 노드에서 막 만든 자격 증명(토큰·사용자·비밀번호)도
  그 리비전을 주면 받고, 기다리는 사이 폐기된 것은 거절한다. 인증되지 않은 요청은 이 노드가 있다고 아는 리비전
  (`Cluster::known_revision`: 멤버는 받은 로그의 마지막 인덱스, 관찰 노드는 붙은 노드가 알렸거나 리더가 돌려준
  인덱스)일 때만 기다리고(없는 리비전으로 연결을 붙잡지 못하게) 응답 헤더도 없다. 헬스는 헤더의 오류(400·503)를
  짧은 응답으로 바꾸지 않고 그대로 돌려준다.
- 쓰기 뒤 읽기: 전달한 쓰기는 그 리비전이 이 노드에 적용되기를 쓰기 제한 시간까지 기다리지만, 넘어도 쓰기는 됐으므로
  성공으로 돌려준다(그 뒤의 읽기가 잠깐 옛 값일 수 있다). 그래서 "보통 곧바로, 늦으면 쓴 리소스의 `revision`을
  `Orbivisor-Min-Revision`으로"다. 쓰기 응답의 `Orbivisor-Revision`도 그때 적용한 리비전이라 쓰기의 리비전보다 낮을 수 있다.
  리스 갱신은 다르다: 적용까지 확인되어야 성공이다.
- 관찰 노드의 헬스는 붙은 투표 멤버의 진행이 5초(`QUORUM_ACK_MAX`) 안에 왔고 리더를 알면 `quorum`이고, `apply_lag`는 붙은
  노드가 알린 적용 인덱스와 리더가 돌려준 인덱스 가운데 큰 것을 아직 적용하지 못한 시간이다.
- API `MemberView.role`(voter·learner·observer), `orbivisorctl cluster status`의 ROLE, 웹 클러스터 화면의 역할이 관찰
  노드를 따로 보인다. 가입의 `via`는 투표 멤버 주소다. 콘솔·펌웨어·마이그레이션의 노드 주소 찾기, 감사 로그·지표 모으기와
  janitor가 관찰 노드도 센다.

**전환.**
- 디스크는 `cluster.redb` 하나에 META `node_mode`(member|observer)다. `become_observer`는 이 노드가 joint의 어느 구성에서도
  투표 멤버가 아니고 그 멤버십이 적용됐을 때만 Raft를 끄고, 끈 뒤 다시 확인한다(그 사이 투표 멤버가 됐으면 Raft를 다시
  띄움). 그다음 한 트랜잭션에서 모드를 적고 LOG 표와 저장된 커밋 위치를 지우고 투표는 남긴 뒤 적용 위치부터 복제본으로
  따른다. 물러난 Raft의 늦은 적용은 저장소 스레드에서 세우는 표시(`StoreError::Retired`)로 막는다.
- `become_member`는 복제본을 멈추고 지금 상태 전체를 스냅샷으로 저장한 뒤 모드를 멤버로 적고 Raft를 띄운다. 로그가 비어
  있으므로 openraft 0.9.25가 시작할 때 마지막 로그를 적용 위치까지 지운 것(purge)으로 메우고(`StorageHelper::
  get_initial_state`) 리더가 그 뒤의 로그를 보낸다. 띄우지 못하면 관찰 노드로 돌아간다. `stopped()`는 계획된 전환을
  멈춤으로 보지 않는다.
- 다시 멤버가 되기 전에 관찰 노드가 되며 지운 로그의 마지막 인덱스(META `dropped_log`)까지 복제본으로 적용한다. 키퍼는
  목록에 쓴 뒤 `RemoveNodes`하므로 learner는 멤버십에서 빠지기 전에 관찰 노드가 될 수 있고, 빠지지 않은 채 다시 멤버가
  되면(내리던 중에 다시 올리기, `AddVoter`의 다시 보기와 멤버십 변경 사이의 목록 쓰기) 리더는 그 노드의 진행
  (`matching`, 노드가 받았다고 답한 로그)을 그대로 쥔다. 다시 띄운 Raft의 로그가 그보다 짧으면 openraft 0.9.25 리더가
  로그가 뒤로 간 것으로 본다(`ProgressEntry::update_conflicting`, 디버그 빌드는 `follower log reversion` panic). 노드가
  "멤버십에서 빠진 것을 본 뒤에야 관찰 노드가 되게" 할 수는 없다: openraft 리더는 빠진 노드의 진행을 그 멤버십을
  덧붙일 때 버리고(`rebuild_progresses`) 그 노드에 그 항목을 보내지 않는다. 지운 로그까지 적용하면 다시 띄운 로그
  (적용 위치까지 purge로 메우고 커밋으로 봄)가 리더의 `matching`을 덮는다.
- `RoleKeeper`(모든 노드, `core`, 1초)가 목록에 맞춘다: learner인데 목록에 있으면 관찰 노드, 관찰 노드인데 목록에 없고
  저장된 멤버십에 있으면 멤버, 투표 멤버는 그대로. 대비책은 설계가 생각한 "리더를 모름" 대신 "멤버 모드 비투표 노드의
  적용이 30초 멈춤"이다(openraft 0.9.25는 다시 시작한 learner가 옛 리더를 계속 알려 "리더 없음"이 생기지 않는다). 그러면
  투표 멤버에게 Follow로 `Hello.role`을 묻고 관찰 노드라면 따른다(꺼진 동안 내려진 노드).
- `VoterKeeper`: 대기 learner가 둘보다 많고 모든 노드가 `observer`를 알리면 건강한 learner 하나(대기가 많은 zone, 따라잡지
  못한 것, 나중에 들어온 것 순)를 목록에 쓰고(`UpdateStatusIf`) 두 틱 넘게 지나 그 노드에 복제됐거나 `change_timeout`이
  지난 뒤 `RemoveNodes`한다. 투표 멤버가 줄어 대기가 모자라면 건강한 관찰 노드를 `AddLearner` 뒤 목록에서 빼고(중간에
  끊기면 "목록에 있는 learner"가 되어 다시 내려간다 — 멤버십에도 목록에도 없는 노드가 생기지 않는다), 따라잡으면 보통의
  승격으로 투표 멤버가 된다. 새 임기의 처음 10초는 관찰 노드 전환을 하지 않는다. 리더는 목록에 있는 노드의 `AddVoter`를
  거절하고(따라잡기를 기다린 뒤 멤버십을 바꾸기 바로 앞에서 다시 본다), `LeaderRequest::Remove`는 목록에서도 뺀다. 고아(멤버십에도 목록에도 없는 Node 기록)는 고치지 않는다(지운
  노드가 되살아나지 않게).
- opld는 저장된 모드로 연다(`Cluster::open_node`). 관찰 노드는 승격을 요청하지 않고, `leave`·다시 가입이 관찰 모드를
  처리한다.
- 계획 48 전 바이너리는 `node_mode`를 몰라 관찰 노드의 저장소(LOG 비움)를 빈 로그로 연다. 옛 코드에 거절을 넣을 수
  없으므로 README에 "관찰 노드를 옛 버전으로 내리기 전에 클러스터에서 빼고 다시 합류"를 적었다.

**리더 일의 예외(§6 "리더의 일은 투표 멤버·learner 수에만 비례").**
- HA·VIP 리스 갱신은 관찰 노드도 지금처럼 리더로 전달한다. 펜싱의 안전이 리더의 리스 표와 갱신마다의 과반 확인에 기대기
  때문이다. 그래서 리더에는 노드당 초당 약 0.5 RPC(2초마다 갱신)와 그 리더 확인이 남는다. 확인은 `ConfirmBatch`로 묶여
  노드 수보다 천천히 는다(아래 잰 값). Raft RPC(하트비트·복제)는 관찰 노드 수와 관계없다. 갱신을 투표 멤버가 묶어 리더에
  전달하는 것은 나중이다.

**규모 시험(§5·§6).**
- `opl-cluster/tests/scale.rs`는 투표 멤버 다섯과 대기 learner 둘을 Raft로 띄우고 나머지를 관찰 노드로 연다. 관찰 노드는
  learner로 따라잡은 뒤 내리지 않고 빈 메모리 저장소로 바로 연다(시작 시간을 줄인다, 노드 300을 띄우는 데 계획 47의 40초 →
  2초). 그러려면 저장소에 멤버십이 없는 관찰 노드가 붙을 곳을 알아야 해서 시험용 `ClusterConfig::follow_seed`(문서에서
  숨김)를 더했다(저장된 멤버십에 투표 멤버가 없을 때만 쓴다). 클러스터 정보의 관찰 노드 목록에도 넣고 모든 노드가
  `VoterKeeper`·`RoleKeeper`를 띄운다. 재는 동안 투표 멤버와 대기 learner가 처음 그대로인지 확인한다.
- 기준: 리더가 비투표 구성원에게 보내는 Raft RPC 초당 15 미만(노드 수와 관계없이), 리더의 초당 RPC 합 35 + 노드당 4.5
  미만(리스 갱신 전달 0.5 + 그 확인을 묶지 못했을 때 2번 × 투표 멤버 넷 = 4; 계획 47은 learner 하트비트 5를 더해 노드당
  10), 확인 묶기 비율(§14) 0.6 미만, 쓰기 p99 50 ms 미만(`OPL_SCALE_P99_MS`, CI 200 ms), 관찰 노드의 뒤처짐 p99 500 ms
  미만(`FOLLOW_STALENESS_P99`, 리더가 쓰기를 마친 뒤 관찰 노드 하나하나가 적용하기까지, 쓰기 100개 × 관찰 노드), 노드 추가 2초
  미만, 투표 멤버 장애 뒤 모든 관찰 노드가 새 쓰기를 받기까지 5초 미만, 리더 장애 5초 미만.
- 투표 멤버 장애: 시험의 Follow 통로가 관찰 노드마다 붙은 투표 멤버를 적고, 관찰 노드가 가장 많이 붙은 리더 아닌 투표
  멤버를 끊은 뒤 곧바로 쓴다. 끊긴 노드는 그 쓰기를 받지 못하므로 모든 관찰 노드가 그것을 적용했다면 다시 붙은 것이다.
  memnet은 끊긴 노드의 다음 프레임(1초마다 `Progress`)에서 연결을 끊으므로 1초 안에 알아챈다. 실제 gRPC에서 연결이 끊기지
  않고 멈추면 3초(`FOLLOW_STALL`)에 알아채므로 3초 남짓이다(시험이 재지 않은 값).
- **작은 노드 수의 묶기 검사를 고쳤다.** 계획 47의 시험은 리더 RPC를 재는 동안 투표 멤버로 가는 `append_entries`에 20 ms
  지연을 넣었다. 확인은 한 번에 하나라 초당 많아야 50번인데 확인 요청은 초당 노드 수(갱신 n/2 × 2)라, 노드 50 미만에서는
  묶일 까닭이 없어 노드 30에서 비율 0.79(134 대 169)로 실패했다(계획 47의 바탕 `9b8de4b`에서도 같음). 지연을 2초/n(20 ms
  이상)으로 바꿨다: 확인은 초당 많아야 n/2번이라 확인 요청의 절반 이하로 묶이고, 투표 멤버 RPC는 많아야 하트비트 20 +
  확인 4 × n/2, 묶지 않은 최악(20 + 10 × (n/2 + 0.5))의 (20 + 2n) / (25 + 5n)이다(노드 20에서 0.48, 30에서 0.46, 100에서
  0.42, 노드 100부터 지연 20 ms라 300에서 0.14). 노드 100·300은 계획 47과 같은 지연이다. 기준(0.6)은 그대로다. 지연이
  하트비트 간격(200 ms)에 가까워지면 선거가 나므로(노드 8에서 지연 250 ms로 리스 갱신 14번 실패, 노드 추가 실패) 노드
  수는 20 이상이어야 한다(`MIN_NODES`, 전에는 투표 멤버보다 많기만 하면 됐다). 확인을 묶지 않게 고친 빌드(갱신마다
  `confirm_leader`)는 노드 30에서 0.79, 100에서 0.80으로 실패했다.
- 잰 값(개발 Mac, Apple Silicon 10코어, 시험 빌드, 두 번씩):

  | | 노드 30 | 노드 100 | 노드 300 |
  |---|---|---|---|
  | 리더 RPC 합(지연을 넣은 5초) | 93(노드당 3.1) | 254~255(2.5) | 352~355(1.2) |
  | 비투표 구성원(대기 learner 둘)에게 | 7 | 7 | 7 |
  | 투표 멤버에게(묶지 않은 최악, 비율) | 72(171, 0.42) | 198(523~525, 0.38) | 200~201(1,479~1,499, 0.13~0.14) |
  | 전달받은 리스 갱신 | 15 | 50 | 145~149 |
  | 쓰기 p99 | 1.7~1.8 ms | 4.2~4.4 ms | 11.7~12.6 ms |
  | 관찰 노드 뒤처짐 p50 / p99 | 0.45 / 0.8~0.85 ms | 1.2~1.4 / 2.3~2.4 ms | 4.1~4.3 / 6.8~7.1 ms |
  | 노드 추가 | 1 ms | 3 ms | 5~8 ms |
  | 투표 멤버 장애 뒤 모든 관찰 노드가 새 쓰기를 받기까지(붙어 있던 관찰 노드) | 0.2초(10) | 1.2초(24) | 1.2초(82) |
  | 리더 장애 뒤 새 리더 | 3.2초 | 3.3~3.6초 | 3.4~3.6초 |
  | 노드를 모두 띄우기 | 0.16초 | 0.3초 | 2.0초 |

  계획 47(나머지가 learner)의 같은 구간 리더 RPC 합은 노드 100에서 570, 노드 300에서 1,320(노드당 5.7·4.4)이었고 그
  3분의 2가 learner 하트비트였다. 쓰기 p99도 노드 300에서 31~33 ms였던 것이 12 ms 남짓이 되었다(리더가 쓰기마다 learner
  모두에 복제하고 한 프로세스의 learner 모두가 그것을 쓰던 일이 관찰 노드 몫의 적용만 남았다). 실패한 리스 갱신은 없었다.

**남은 것.**
- 죽어서 교체된 투표 멤버는 건강하지 않은 learner로 남는다(건강한 learner만 내린다). 막 된 리더가 목록 쓰기를 적용하기
  전이면 목록의 노드를 승격할 수 있다. `AddVoter`의 다시 보기와 멤버십 변경 사이에 목록에 오른 노드는 Raft를 끈
  관찰 노드인 채 투표 멤버가 될 수 있다(키퍼가 목록에서 빼면 다시 멤버가 된다 — 그동안 투표 멤버 하나가 응답하지
  않는다).
- 리스 갱신을 투표 멤버가 묶어 리더에 전달하기, 처음부터 관찰 노드로 가입하기, 대기 learner 수를 설정으로 두기.
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  voter: true,
  online: true,
```

을 다음으로 바꾼다:

```tsx
  voter: true,
  role: "voter",
  online: true,
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
          member("n2", "node-b", { fencing: null }),
          member("n3", null, { voter: false, online: false, address: null }),
          member("n4", "witness-w", { witness: true }),
        ],
```

을 다음으로 바꾼다:

```tsx
          member("n2", "node-b", { fencing: null }),
          member("n3", null, { voter: false, role: "learner", online: false, address: null }),
          member("n4", "witness-w", { witness: true }),
          member("n5", "node-o", { voter: false, role: "observer" }),
        ],
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  expect(joining).toHaveTextContent("Offline");
  // 위트니스는 역할 옆에 배지가 붙는다.
```

을 다음으로 바꾼다:

```tsx
  expect(joining).toHaveTextContent("Offline");
  // 관찰 노드(Raft 밖에서 투표 멤버를 따름)는 learner와 따로 보인다.
  const observer = within(table).getByText("node-o").closest("tr")!;
  expect(observer).toHaveTextContent("Observer");
  expect(observer).not.toHaveTextContent("Learner");
  // 위트니스는 역할 옆에 배지가 붙는다.
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
                <TableCell>
                  {m.id === leader
                    ? t("cluster.leader")
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                  {m.witness && (
```

을 다음으로 바꾼다:

```tsx
                <TableCell>
                  {m.id === leader ? t("cluster.leader") : t(`cluster.${m.role}`)}
                  {m.witness && (
```

`web/src/i18n/en.json`에서

```json
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining. A witness votes but runs no VMs.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
```

을 다음으로 바꾼다:

```json
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a standby or joining node with the full Raft log. An observer follows a voter outside Raft, so it adds no work to the leader. A witness votes but runs no VMs.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
```

`web/src/i18n/en.json`에서

```json
    "learner": "Learner",
    "clusterAddress": "Cluster address",
```

을 다음으로 바꾼다:

```json
    "learner": "Learner",
    "observer": "Observer",
    "clusterAddress": "Cluster address",
```

`web/src/i18n/ko.json`에서

```json
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다. 위트니스는 투표만 하고 VM을 돌리지 않습니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
```

을 다음으로 바꾼다:

```json
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 Raft 로그를 모두 받는 대기·합류 중인 노드입니다. 관찰 노드는 Raft 밖에서 투표 멤버 하나를 따르므로 리더의 일을 늘리지 않습니다. 위트니스는 투표만 하고 VM을 돌리지 않습니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
```

`web/src/i18n/ko.json`에서

```json
    "learner": "학습자",
    "clusterAddress": "클러스터 주소",
```

을 다음으로 바꾼다:

```json
    "learner": "학습자",
    "observer": "관찰 노드",
    "clusterAddress": "클러스터 주소",
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1887개, 실패 0

Run: `OPL_SCALE_NODES=100 cargo test -p opl-cluster --features scale-it --test scale -- --nocapture`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: 통과

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add .github README.md crates docs web
git commit -m "test(cluster): 관찰 노드 규모 시험과 문서 — 투표 5·대기 2·나머지 관찰 노드, 웹 역할, CORS 리비전 헤더"
```

---

## 완료 기준 (계획 48)

- 노드 8대 이상 클러스터에서 투표 멤버 5 + learner 2 + 나머지 관찰 노드가 되고, 투표 멤버가 죽으면 관찰 노드가 learner를 거쳐 투표 멤버로 올라간다(`observers.rs`).
- 관찰 노드에서 읽기·쓰기·리스 갱신이 되고, 붙은 투표 멤버가 죽으면 다른 투표 멤버로 다시 붙는다.
- 규모 시험이 노드 30·100(CI)·300에서 통과한다: 비투표 노드에 가는 Raft RPC < 15/s, 리더 RPC < 35 + 노드당 4.5, 확인 묶기 비율 < 0.6, 투표 멤버 장애 뒤 5초 안에 모두 다시 붙음.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, `ha-it`, 웹 시험이 통과한다. Linux CI가 통과한다.

## 알고 받아들인 제한

- 관찰 노드의 HA·VIP 리스 갱신은 여전히 리더로 간다(노드당 초당 약 0.5 RPC).
- 관찰 노드 읽기는 보통 곧바로 쓴 것을 보지만, 늦으면 `Orbivisor-Min-Revision`으로 기다려야 한다.
- 죽어서 교체된 투표 멤버는 건강하지 않은 learner로 남는다. 드문 경합에서 목록에 오른 노드가 잠시 응답하지 않는 투표 멤버가 될 수 있다(설계 §15).
- 관찰 노드를 계획 48 전 버전으로 내리기 전에는 `cluster leave` 뒤 다시 가입한다.
