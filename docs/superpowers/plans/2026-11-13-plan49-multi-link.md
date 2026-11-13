# Orbivisor 계획 49: 클러스터 통신 다중 링크 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드 사이 클러스터 통신이 망 하나에 기대지 않게 한다. 노드마다 클러스터 주소를 여러 개 두고, 쓰던 링크가 끊기면 Raft 하트비트·리스 갱신·쓰기 전달이 선거 제한 시간 안에 다음 링크로 넘어간다.

**Architecture:**
- **노드 타입:** Raft 구성원 값이 `BasicNode { addr }`에서 `OrbNode { addrs }`로 바뀐다. 링크 하나면 저장 형식이 예전과 바이트까지 같아 롤링 업그레이드와 이미 쓴 로그가 그대로다.
- **링크 고르기:** `LinkBook`(노드 → 링크 목록)과 `LinkSelector`(노드마다 지금 링크)를 opl-peer `Peers`와 memnet이 함께 쓴다. 연결 실패는 곧바로, 시간 초과는 연속 두 번 또는 400 ms 뒤 넘어가고, 30초마다 우선 링크를 다시 시험한다. 보내지 않은 것이 확실한 요청만 같은 호출 안에서 다시 보낸다.
- **설정과 맞추기:** 노드 설정 `cluster_addresses`를 노드가 Node 기록에 적고, 인증서에 모든 링크 이름을 넣는다. 모든 노드가 `multi-link`를 알리면 리더가 멤버십(`SetNodes`)과 관찰 노드 목록을 그 값으로 맞춘다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9.25, tonic 0.14(gRPC 메타데이터), rcgen(같은 키로 다시 서명), axum·utoipa, React(웹 링크 표시). 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-11-availability-scale-design.md` (§7, §16 "구현(계획 49)")
**선행 계획:** 계획 1~48 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| `OrbNode`와 옛 형식 호환 | 링크 상태를 헬스·지표·웹에 보이기(계획 52) |
| `LinkBook`·`LinkSelector`, Peers·memnet 링크 넘어가기, 대상 노드 확인 메타데이터 | 링크별 백그라운드 ping |
| `cluster_addresses` 설정, 인증서 다시 서명, 여러 링크 합류 | 대량 전송 링크를 따로 고르기 |
| 리더의 링크 맞추기와 `multi-link` 기능, `observer_links` | 내장 VIP의 BGP 모드 |
| API·웹 표시, 죽은 우선 링크 실제 프로세스 시험, README, 설계 §16 | |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 변경과 시험(memnet 링크 끊기, gRPC 닫힌 포트·답 없는 리스너, opld 실제 프로세스 시험 — 127.0.0.2가 응답 없이 시간 초과), opld `ha-it`, 웹 시험.
- **배포 서버 svc01(Linux):** 같은 시험(127.0.0.2가 곧바로 거절되는 경로)과 규모 시험(노드 100).
- **GitHub:** 시스템을 바꾸는 작업(linux-kvm·ldap·package)만.
- **테스트 VM:** 노드 둘이 링크 하나로 돈다. 배포 뒤 업그레이드가 멤버십을 바꾸지 않고(링크 하나는 같은 형식), `multi-link` 기능 보고와 `cluster_addresses` 표시를 본다.

## 설계 결정

- **링크 하나는 예전과 같다:** `OrbNode`는 링크가 하나면 `{"addr": …}`만 쓴다. 옛 노드는 여러 링크 항목을 첫 링크로 읽는다. 여러 링크를 멤버십에 넣는 일은 모든 노드가 `multi-link`를 알릴 때만 한다.
- **넘어가기는 선거보다 빠르다:** 넘어가기 상한(400 ms + 하트비트 200 ms)이 `election_timeout_min − heartbeat_interval`(800 ms)보다 짧다.
- **요청을 두 번 보내지 않는다:** 같은 호출 안의 다시 보내기는 연결이 되지 않았거나 다른 노드가 거절했을 때만 한다. 이미 보낸 호출의 `Unavailable`(keepalive 만료)은 다시 보내지 않는다.
- **링크가 다른 노드를 가리키지 않는다:** 대상 id를 아는 호출은 `orbivisor-target`을 싣고 받는 노드가 확인한다. 합류와 링크 맞추기는 다른 노드와 겹치는 링크를 거절한다.
- **인증서는 같은 키로 이름만 더한다:** 다시 시작할 때 로컬 CA로 빠진 이름을 더해 다시 서명한다. 클라이언트 신원은 CA와 노드 id로 확인되므로 그대로다.


### Task 1: OrbNode 노드 타입

opl-cluster `types.rs`에 `OrbNode { addrs: Vec<String> }`(각 항목 `host:port`, 앞이 우선)를 두고 openraft TypeConfig의 `Node`를 `BasicNode`에서 이것으로 바꾼다. serde는 손으로 쓴다: 늘 `addr`(첫 링크)를 쓰고 링크가 둘 이상일 때만 `addrs`를 더 쓴다 — 링크 하나면 `BasicNode`와 바이트까지 같아 이미 쓴 로그·스냅샷·옛 노드가 그대로다. 읽을 때는 비지 않은 `addrs`, 아니면 `[addr]`. `OrbNode::addr()`가 이 계획 전의 "그 노드의 주소"이고 `.addr` 필드를 읽던 곳은 `.addr()`로 바꾼다. 계획 48 형식의 멤버십 로그 항목·`StoredMembership`·스냅샷 메타 고정 JSON을 읽는 시험. 동작은 바뀌지 않는다.

**Files:**
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/remotes.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/health.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/follow.rs`
- Modify: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/src/lease.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/types.rs`
- Modify: `crates/opl-cluster/src/voters.rs`
- Modify: `crates/opl-cluster/src/watch.rs`
- Modify: `crates/opl-cluster/tests/common/mod.rs`
- Modify: `crates/opl-cluster/tests/observers.rs`
- Modify: `crates/opl-cluster/tests/scale.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`
- Modify: `crates/opl-cluster/tests/voters.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/no_quorum.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs`에서

```rust
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;
```

을 다음으로 바꾼다:

```rust
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::{NodeId, OrbNode};
```

`crates/opl-api/src/janitor.rs`에서

```rust
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
```

을 다음으로 바꾼다:

```rust
                id,
                OrbNode::single(id.to_string()),
            )]))
```

`crates/opl-api/src/remotes.rs`에서

```rust
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;
```

을 다음으로 바꾼다:

```rust
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::{NodeId, OrbNode};
```

`crates/opl-api/src/remotes.rs`에서

```rust
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
```

을 다음으로 바꾼다:

```rust
                id,
                OrbNode::single(id.to_string()),
            )]))
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use axum::serve::ListenerExt;
use openraft::{BasicNode, ServerState};
use opl_api::vm_imports::ImportCredentials;
use opl_api::{AppState, AuditConfig, AuditLog, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
```

을 다음으로 바꾼다:

```rust
use axum::serve::ListenerExt;
use openraft::ServerState;
use opl_api::vm_imports::ImportCredentials;
use opl_api::{AppState, AuditConfig, AuditLog, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(id.to_string()))]))
            .await
```

을 다음으로 바꾼다:

```rust
        cluster
            .initialize(BTreeMap::from([(id, OrbNode::single(id.to_string()))]))
            .await
```

`crates/opl-api/tests/health.rs`에서

```rust
use common::{Client, Env, WAIT};
use openraft::BasicNode;
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

을 다음으로 바꾼다:

```rust
use common::{Client, Env, WAIT};
use opl_api::{AppState, RuntimeProbe};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

`crates/opl-api/tests/health.rs`에서

```rust
    env.cluster
        .add_learner(id, BasicNode::new(&addr))
        .await
```

을 다음으로 바꾼다:

```rust
    env.cluster
        .add_learner(id, OrbNode::single(&addr))
        .await
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{
```

을 다음으로 바꾼다:

```rust
use futures_util::{SinkExt, StreamExt};
use opl_api::{
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
```

을 다음으로 바꾼다:

```rust
};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig};
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
```

을 다음으로 바꾼다:

```rust
    let members = BTreeMap::from([
        (NodeId(a.id), OrbNode::single(&a.addr)),
        (NodeId(b.id), OrbNode::single(&b.addr)),
    ]);
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
```

`crates/opl-cluster/src/follow.rs` 전체:

```rust
//! 관찰 노드가 투표 멤버 하나에서 받는 스트림(설계 §6): 스냅샷(처음·뒤처졌을 때) + 적용 묶음 + 1초마다 진행.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use openraft::{LogId, RaftMetrics, StoredMembership};
use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::cluster::{Backend, Cluster, ClusterError, NodeHandle, observers_of};
use crate::state_machine::snapshot_objects;
use crate::types::{NodeId, OrbNode, RawResource};
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
        membership: StoredMembership<NodeId, OrbNode>,
    },
    /// 큰 묶음의 앞부분 이벤트(메시지 한도 때문에 나눔). 뒤따르는 `Batch`와 함께 한 번에 적용한다.
    BatchPart { events: Vec<WatchEvent> },
    /// 서버가 적용한 묶음([`FeedBatch`])(앞서 온 `BatchPart`의 이벤트가 그 앞에 붙는다). 이벤트가 없는 묶음도 적용
    /// 위치로 온다.
    Batch {
        events: Vec<WatchEvent>,
        applied: LogId<NodeId>,
        membership: Option<StoredMembership<NodeId, OrbNode>>,
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
                .and_then(|l| membership.get_node(&l).map(|n| (l, n.addr().to_string())));
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
        .filter_map(|id| membership.get_node(&id).map(|n| (id, n.addr().to_string())))
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

type Metrics = tokio::sync::watch::Receiver<RaftMetrics<NodeId, OrbNode>>;

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
            .and_then(|l| membership.get_node(&l).map(|n| n.addr().to_string())),
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
        c.initialize(BTreeMap::from([(id, OrbNode::single("node-1"))]))
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

`crates/opl-cluster/src/leader.rs` 전체:

```rust
//! 리더가 처리하는 요청(쓰기, 멤버십 변경)과, 팔로워가 그것을 리더에 전달하는 통로.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{ChangeMembers, Raft, RaftMetrics};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::Kind;
use redb::Database;
use serde::{Deserialize, Serialize};

use crate::cluster::ClusterError;
use crate::lease::{ConfirmBatch, LeaseTable};
use crate::store::list_objects;
use crate::types::{Command, LeaderRequest, NodeId, OrbNode, Rejection, Response, TypeConfig};
use crate::vip::VipTable;

/// learner가 로그를 따라잡기를 기다리는 최대 시간. 스냅샷을 받아야 하면 오래 걸린다.
pub const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(60);

/// 앞선 멤버십 변경이 아직 커밋 중일 때 다시 시도하는 간격
const IN_PROGRESS_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// 팔로워가 받은 요청을 리더에게 보낸다. 노드 간 네트워크가 구현한다.
#[async_trait]
pub trait Forwarder: Send + Sync + 'static {
    /// `addr`(리더의 Raft 주소)의 리더에게 요청을 보내고 리더의 결과를 돌려준다. 리더가 그 사이
    /// 바뀌었으면 `NotLeader`, 리더에 닿지 못했으면 `Unreachable`이다.
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError>;
}

/// 노드 사이로 오가는 리더의 오류
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "kebab-case")]
pub enum LeaderError {
    Rejected {
        rejection: Rejection,
    },
    NotLeader {
        leader_id: Option<NodeId>,
        leader_addr: Option<String>,
    },
    Timeout,
    Other {
        message: String,
    },
}

impl From<ClusterError> for LeaderError {
    fn from(e: ClusterError) -> Self {
        match e {
            ClusterError::Rejected(rejection) => LeaderError::Rejected { rejection },
            ClusterError::NotLeader {
                leader_id,
                leader_addr,
            } => LeaderError::NotLeader {
                leader_id,
                leader_addr,
            },
            ClusterError::Timeout => LeaderError::Timeout,
            other => LeaderError::Other {
                message: other.to_string(),
            },
        }
    }
}

impl From<LeaderError> for ClusterError {
    fn from(e: LeaderError) -> Self {
        match e {
            LeaderError::Rejected { rejection } => ClusterError::Rejected(rejection),
            LeaderError::NotLeader {
                leader_id,
                leader_addr,
            } => ClusterError::NotLeader {
                leader_id,
                leader_addr,
            },
            LeaderError::Timeout => ClusterError::Timeout,
            LeaderError::Other { message } => ClusterError::Raft(message),
        }
    }
}

/// 리더로서 요청을 처리하는 데 쓰는 것: Raft, 리스 표(HA·VIP)와 그 둘이 함께 쓰는 리더 확인
#[derive(Clone)]
pub(crate) struct LeaderCtx {
    pub(crate) raft: Raft<TypeConfig>,
    pub(crate) confirm: ConfirmBatch,
    /// 이 노드의 저장소(클러스터 정보의 관찰 노드 목록을 읽는다)
    pub(crate) db: Arc<Database>,
    pub(crate) leases: Arc<LeaseTable>,
    pub(crate) vips: Arc<VipTable>,
}

/// 이 노드가 리더라고 보고 요청을 처리한다. 리더가 아니면 `NotLeader`다. 성공하면 커밋된 로그 인덱스
/// (리스 갱신은 리더가 적용한 마지막 인덱스).
pub(crate) async fn execute(
    ctx: &LeaderCtx,
    req: LeaderRequest,
    timeout: Duration,
) -> Result<u64, ClusterError> {
    let raft = &ctx.raft;
    match req {
        LeaderRequest::RenewLease { node } => {
            ctx.leases.renew(raft, &ctx.confirm, node, timeout).await
        }
        LeaderRequest::RenewVip { node, vips } => {
            ctx.vips
                .renew(raft, &ctx.confirm, node, vips, timeout)
                .await
        }
        LeaderRequest::Write { command } => {
            let resp = tokio::time::timeout(timeout, raft.client_write(command))
                .await
                .map_err(|_| ClusterError::Timeout)??;
            let Response(result) = resp.data;
            Ok(result?)
        }
        LeaderRequest::AddLearner { id, addr } => {
            retry_in_progress(timeout, || {
                raft.add_learner(id, OrbNode::single(addr.clone()), false)
            })
            .await
        }
        LeaderRequest::AddVoter { id } => {
            let m = raft.metrics().borrow().clone();
            // 아래 지름길은 리더의 멤버십으로만 판단한다. 뒤처진 팔로워가 답하면 안 된다.
            if m.state != openraft::ServerState::Leader {
                return Err(not_leader(&m));
            }
            let membership = m.membership_config.membership();
            if membership.voter_ids().any(|v| v == id) {
                return Ok(m.last_applied.map_or(0, |l| l.index));
            }
            // learner가 아니면(빠졌거나 더한 적이 없으면) 따라잡기를 기다리지 않고 바로 거부한다. 관찰 노드 목록에
            // 있는 learner(관찰 노드로 내려가는 중)도 올리지 않는다(내려가며 로그를 지우는 노드가 투표 멤버가 되면 안
            // 된다).
            if membership.get_node(&id).is_none() || observers(&ctx.db)?.contains_key(&id) {
                return Err(ClusterError::Rejected(Rejection::NotFound {
                    kind: Kind::Node,
                    id: id.0,
                }));
            }
            // learner가 지금 리더의 마지막 로그까지 받으면 올린다.
            let target = m.last_log_index;
            raft.wait(Some(CATCH_UP_TIMEOUT))
                .metrics(
                    |m| {
                        m.state != openraft::ServerState::Leader
                            || m.replication.as_ref().is_some_and(|r| {
                                r.get(&id).is_some_and(|l| l.map(|l| l.index) >= target)
                            })
                    },
                    "learner caught up",
                )
                .await
                .map_err(|_| ClusterError::Timeout)?;
            // 기다리는 사이 키퍼가 이 노드를 관찰 노드 목록에 적었을 수 있다(내려가며 로그를 지운다). 바로 앞에서 다시 본다.
            if observers(&ctx.db)?.contains_key(&id) {
                return Err(ClusterError::Rejected(Rejection::NotFound {
                    kind: Kind::Node,
                    id: id.0,
                }));
            }
            let change = ChangeMembers::AddVoterIds(BTreeSet::from([id]));
            retry_in_progress(timeout, || raft.change_membership(change.clone(), false)).await
        }
        LeaderRequest::Remove { id } => {
            let m = raft.metrics().borrow().clone();
            if m.state != openraft::ServerState::Leader {
                return Err(not_leader(&m));
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
    m: &RaftMetrics<NodeId, OrbNode>,
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
    }
}

/// 멤버십을 바꾼다. 앞선 멤버십 변경이 아직 커밋 중이면(`InProgress`, 예: 다른 노드의 승격이
/// 두 단계 중 하나를 커밋하는 중) `timeout`까지 다시 시도하고, 그래도 안 되면 `Timeout`이다.
async fn retry_in_progress<F, Fut>(timeout: Duration, mut change: F) -> Result<u64, ClusterError>
where
    F: FnMut() -> Fut,
    Fut: Future<
        Output = Result<
            ClientWriteResponse<TypeConfig>,
            RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>,
        >,
    >,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, change())
            .await
            .map_err(|_| ClusterError::Timeout)?
        {
            Err(RaftError::APIError(ClientWriteError::ChangeMembershipError(
                ChangeMembershipError::InProgress(_),
            ))) => {
                if tokio::time::Instant::now() + IN_PROGRESS_RETRY_INTERVAL >= deadline {
                    return Err(ClusterError::Timeout);
                }
                tokio::time::sleep(IN_PROGRESS_RETRY_INTERVAL).await;
            }
            other => return Ok(other?.log_id.index),
        }
    }
}

/// 이 노드가 아는 리더와 그 주소로 `NotLeader`를 만든다. openraft의 `change_membership`은
/// 리더인지 보기 전에 변경을 검사하므로, 팔로워에서는 `ForwardToLeader` 대신 검사 오류가 날 수 있다.
pub(crate) fn not_leader(m: &RaftMetrics<NodeId, OrbNode>) -> ClusterError {
    let leader_addr = m.current_leader.and_then(|l| {
        m.membership_config
            .membership()
            .get_node(&l)
            .map(|n| n.addr().to_string())
    });
    ClusterError::NotLeader {
        leader_id: m.current_leader,
        leader_addr,
    }
}
```

`crates/opl-cluster/src/lease.rs`에서

```rust
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr),
            })
```

을 다음으로 바꾼다:

```rust
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr().to_string()),
            })
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use state_machine::StateMachine;
pub use vip::{VipLeases, VipRenewal};
```

을 다음으로 바꾼다:

```rust
pub use state_machine::StateMachine;
pub use types::OrbNode;
pub use vip::{VipLeases, VipRenewal};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
use futures_util::stream::BoxStream;
use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
```

을 다음으로 바꾼다:

```rust
use futures_util::stream::BoxStream;
use openraft::Raft;
use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
};
use openraft::{BasicNode, Raft};
```

을 다음으로 바꾼다:

```rust
};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
use crate::leader::{self, Forwarder};
use crate::types::{LeaderRequest, NodeId, TypeConfig};
```

을 다음으로 바꾼다:

```rust
use crate::leader::{self, Forwarder};
use crate::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
```

`crates/opl-cluster/src/memnet.rs`에서

```rust

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> MemConnection {
        MemConnection {
```

을 다음으로 바꾼다:

```rust

    async fn new_client(&mut self, target: NodeId, _node: &OrbNode) -> MemConnection {
        MemConnection {
```

`crates/opl-cluster/src/memnet.rs`에서

```rust
type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;
```

을 다음으로 바꾼다:

```rust
type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;
```

`crates/opl-cluster/src/replica.rs`에서

```rust
use futures_util::stream::BoxStream;
use openraft::{BasicNode, StoredMembership};
use redb::{Database, ReadableDatabase};
```

을 다음으로 바꾼다:

```rust
use futures_util::stream::BoxStream;
use openraft::StoredMembership;
use redb::{Database, ReadableDatabase};
```

`crates/opl-cluster/src/replica.rs`에서

```rust
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::NodeId;
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, BasicNode>;
```

을 다음으로 바꾼다:

```rust
use crate::store::{KEY_LAST_MEMBERSHIP, META, Store, StoreError, get_json};
use crate::types::{NodeId, OrbNode};
use crate::watch::{FeedBatch, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, OrbNode>;
```

`crates/opl-cluster/src/replica.rs`에서

```rust
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr.clone())))
        .collect()
```

을 다음으로 바꾼다:

```rust
    m.voter_ids()
        .filter_map(|id| m.get_node(&id).map(|n| (id, n.addr().to_string())))
        .collect()
```

`crates/opl-cluster/src/replica.rs`에서

```rust

    use openraft::BasicNode;
    use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust

    use opl_types::resource::Resource;
```

`crates/opl-cluster/src/replica.rs`에서

```rust
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
                .collect();
```

을 다음으로 바꾼다:

```rust
            let members = (1..=3u64)
                .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
                .collect();
```

`crates/opl-cluster/src/replica.rs`에서

```rust
            self.leader()
                .add_learner(id, BasicNode::new(format!("node-{i}")))
                .await
```

을 다음으로 바꾼다:

```rust
            self.leader()
                .add_learner(id, OrbNode::single(format!("node-{i}")))
                .await
```

`crates/opl-cluster/src/state_machine.rs`에서

```rust
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;
/// 상태 전체의 리소스(스냅샷 본문)
```

을 다음으로 바꾼다:

```rust
};
use crate::types::{Command, NodeId, OrbNode, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{FeedBatch, WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, OrbNode>;
type Meta = SnapshotMeta<NodeId, OrbNode>;
/// 상태 전체의 리소스(스냅샷 본문)
```

`crates/opl-cluster/src/types.rs`에서

```rust

openraft::declare_raft_types!(
```

을 다음으로 바꾼다:

```rust

/// Raft 구성원 노드 값. 노드에 닿는 클러스터 링크들(`host:port`)이고 앞이 우선이다(설계 §7).
///
/// 저장 형식이다(로그·스냅샷·노드 간 gRPC). 늘 `addr`(첫 링크)를 쓰고 링크가 둘 이상일 때만
/// `addrs`를 더 쓴다. 링크 하나면 `openraft::BasicNode`와 바이트까지 같고, 옛 노드는 여러 링크
/// 항목을 첫 링크 하나로 읽는다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrbNode {
    pub addrs: Vec<String>,
}

impl OrbNode {
    pub fn new<S: Into<String>>(addrs: impl IntoIterator<Item = S>) -> Self {
        Self {
            addrs: addrs.into_iter().map(Into::into).collect(),
        }
    }

    /// 링크 하나인 노드.
    pub fn single(addr: impl Into<String>) -> Self {
        Self {
            addrs: vec![addr.into()],
        }
    }

    /// 첫 링크. 이 계획 전의 "그 노드의 주소" 자리다. 링크가 없으면 `""`.
    pub fn addr(&self) -> &str {
        self.addrs.first().map_or("", String::as_str)
    }
}

impl fmt::Display for OrbNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.addrs.join(","))
    }
}

impl Serialize for OrbNode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let many = self.addrs.len() > 1;
        let mut st = serializer.serialize_struct("OrbNode", if many { 2 } else { 1 })?;
        st.serialize_field("addr", self.addr())?;
        if many {
            st.serialize_field("addrs", &self.addrs)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for OrbNode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            addr: String,
            #[serde(default)]
            addrs: Vec<String>,
        }
        let w = Wire::deserialize(deserializer)?;
        if !w.addrs.is_empty() {
            return Ok(Self { addrs: w.addrs });
        }
        // 빈 `addr`는 링크 없음(`Default`)으로 읽어 쓰고 읽기가 같은 값이 되게 한다.
        if w.addr.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self::single(w.addr))
    }
}

openraft::declare_raft_types!(
```

`crates/opl-cluster/src/types.rs`에서

```rust
        NodeId = NodeId,
        Node = openraft::BasicNode,
);
```

을 다음으로 바꾼다:

```rust
        NodeId = NodeId,
        Node = OrbNode,
);
```

`crates/opl-cluster/src/types.rs`에서

```rust
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    /// 확인하지 않는 status 쓰기는 예전 형식 그대로다. 확인하는 쓰기는 새 명령이다.
```

을 다음으로 바꾼다:

```rust
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    /// 노드 값은 저장 형식이다. 링크 하나는 `BasicNode`와 같고 옛 형식도 읽는다(계획 49).
    #[test]
    fn orb_node_storage_format() {
        let old: OrbNode = serde_json::from_value(json!({"addr": "a:8007"})).unwrap();
        assert_eq!(old, OrbNode::single("a:8007"));
        assert_eq!(old.addr(), "a:8007");
        assert_eq!(
            serde_json::to_string(&OrbNode::single("a:8007")).unwrap(),
            r#"{"addr":"a:8007"}"#
        );

        let two = OrbNode::new(["a", "b"]);
        let text = serde_json::to_string(&two).unwrap();
        assert_eq!(text, r#"{"addr":"a","addrs":["a","b"]}"#);
        assert_eq!(serde_json::from_str::<OrbNode>(&text).unwrap(), two);
        assert_eq!(two.to_string(), "a,b");
        // 옛 노드(openraft `BasicNode`)는 첫 링크 하나로 읽는다.
        let basic: openraft::BasicNode = serde_json::from_str(&text).unwrap();
        assert_eq!(basic.addr, "a");

        let empty: OrbNode = serde_json::from_value(json!({"addr": "a", "addrs": []})).unwrap();
        assert_eq!(empty, OrbNode::single("a"));
        assert_eq!(OrbNode::default().addr(), "");
        let none = serde_json::to_string(&OrbNode::default()).unwrap();
        assert_eq!(
            serde_json::from_str::<OrbNode>(&none).unwrap(),
            OrbNode::default()
        );
    }

    /// 계획 48(ebb8f75)까지 `BasicNode`로 쓴 멤버십 로그 항목·`StoredMembership`·스냅샷 메타를
    /// 새 노드 타입으로 읽는다.
    #[test]
    fn basic_node_membership_formats_still_read() {
        let n1 = NodeId::from(1);
        let n2 = NodeId::from(2);
        let want = BTreeMap::from([
            (n1, OrbNode::single("10.0.0.1:8007")),
            (n2, OrbNode::single("10.0.0.2:8007")),
        ]);
        let nodes_of = |m: &openraft::Membership<NodeId, OrbNode>| {
            m.nodes()
                .map(|(id, n)| (*id, n.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let membership = r#"{"configs":[["00000000-0000-0000-0000-000000000001"]],"nodes":{"00000000-0000-0000-0000-000000000001":{"addr":"10.0.0.1:8007"},"00000000-0000-0000-0000-000000000002":{"addr":"10.0.0.2:8007"}}}"#;
        let log_id = r#"{"leader_id":{"term":3,"node_id":"00000000-0000-0000-0000-000000000001"},"index":7}"#;

        let entry_text =
            format!(r#"{{"log_id":{log_id},"payload":{{"Membership":{membership}}}}}"#);
        let entry: openraft::Entry<TypeConfig> = serde_json::from_str(&entry_text).unwrap();
        assert_eq!(entry.log_id.index, 7);
        let openraft::EntryPayload::Membership(m) = &entry.payload else {
            panic!("not a membership entry: {entry:?}");
        };
        assert_eq!(nodes_of(m), want);
        // 링크 하나는 쓴 바이트도 같다.
        assert_eq!(serde_json::to_string(&entry).unwrap(), entry_text);

        let stored = format!(r#"{{"log_id":{log_id},"membership":{membership}}}"#);
        let stored: openraft::StoredMembership<NodeId, OrbNode> =
            serde_json::from_str(&stored).unwrap();
        assert_eq!(nodes_of(stored.membership()), want);

        let meta = format!(
            r#"{{"last_log_id":{log_id},"last_membership":{{"log_id":{log_id},"membership":{membership}}},"snapshot_id":"7-abc"}}"#
        );
        let meta: openraft::SnapshotMeta<NodeId, OrbNode> = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta.snapshot_id, "7-abc");
        assert_eq!(nodes_of(meta.last_membership.membership()), want);
    }

    /// 확인하지 않는 status 쓰기는 예전 형식 그대로다. 확인하는 쓰기는 새 명령이다.
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
use openraft::{ChangeMembers, LogId, Raft, RaftMetrics, ServerState};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::node::{Node, NodeSpec};
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::{Cluster, ClusterError};
use crate::types::{NodeId, OrbNode, TypeConfig};

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
            next.insert(id, node.addr().to_string());
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
    m: &RaftMetrics<NodeId, OrbNode>,
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
        raft.add_learner(id, OrbNode::single(addr.clone()), false),
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
fn matched(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> Option<u64> {
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
    m: &RaftMetrics<NodeId, OrbNode>,
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
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, OrbNode>) -> bool {
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
    changes: ChangeMembers<NodeId, OrbNode>,
    timeout: Duration,
) -> Outcome {
    outcome(tokio::time::timeout(timeout, raft.change_membership(changes, true)).await)
}

type MembershipResult = Result<
    Result<ClientWriteResponse<TypeConfig>, RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>>,
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
fn caught_up(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> bool {
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

`crates/opl-cluster/src/watch.rs`에서

```rust

use openraft::{BasicNode, LogId, StoredMembership};
use opl_types::resource::Kind;
```

을 다음으로 바꾼다:

```rust

use openraft::{LogId, StoredMembership};
use opl_types::resource::Kind;
```

`crates/opl-cluster/src/watch.rs`에서

```rust

use crate::types::{NodeId, RawResource};
```

을 다음으로 바꾼다:

```rust

use crate::types::{NodeId, OrbNode, RawResource};
```

`crates/opl-cluster/src/watch.rs`에서

```rust
    /// 묶음에 든 마지막 멤버십 변경
    pub membership: Option<StoredMembership<NodeId, BasicNode>>,
}
```

을 다음으로 바꾼다:

```rust
    /// 묶음에 든 마지막 멤버십 변경
    pub membership: Option<StoredMembership<NodeId, OrbNode>>,
}
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust

use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

을 다음으로 바꾼다:

```rust

use openraft::ServerState;
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

`crates/opl-cluster/tests/common/mod.rs`에서

```rust

pub fn addr(n: u64) -> BasicNode {
    BasicNode::new(format!("node-{n}"))
}
```

을 다음으로 바꾼다:

```rust

pub fn addr(n: u64) -> OrbNode {
    OrbNode::single(format!("node-{n}"))
}
```

`crates/opl-cluster/tests/observers.rs`에서

```rust
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::NodeId;
use opl_cluster::watch::WatchEvent;
```

을 다음으로 바꾼다:

```rust
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::watch::WatchEvent;
```

`crates/opl-cluster/tests/observers.rs`에서

```rust
        let voters = (1..=5)
            .map(|i| (id(i), openraft::BasicNode::new(format!("node-{i}"))))
            .collect();
```

을 다음으로 바꾼다:

```rust
        let voters = (1..=5)
            .map(|i| (id(i), OrbNode::single(format!("node-{i}"))))
            .collect();
```

`crates/opl-cluster/tests/observers.rs`에서

```rust
            leader
                .add_learner(id(i), openraft::BasicNode::new(format!("node-{i}")))
                .await
```

을 다음으로 바꾼다:

```rust
            leader
                .add_learner(id(i), OrbNode::single(format!("node-{i}")))
                .await
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
use openraft::ServerState;
use openraft::error::{InstallSnapshotError, RPCError, RaftError};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::memnet::{MemConnection, MemFollow, MemForwarder, MemNetwork, MemRouter};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
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

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> CountingConnection {
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
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

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
        .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
        .collect();
    h.get(voters[0]).initialize(members).await.unwrap();
    let leader = h.wait_leader(&voters, Duration::from_secs(10)).await;
    for i in VOTERS + 1..=MEMBERS {
        h.get(leader)
            .add_learner(NodeId::from(i), OrbNode::single(format!("node-{i}")))
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
        .add_learner(
            NodeId::from(added),
            OrbNode::single(format!("node-{added}")),
        )
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

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
            id: n4,
            addr: addr(4).addr,
        })
```

을 다음으로 바꾼다:

```rust
            id: n4,
            addr: addr(4).addr().to_string(),
        })
```

`crates/opl-cluster/tests/voters.rs`에서

```rust

use openraft::{BasicNode, ChangeMembers};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{
```

을 다음으로 바꾼다:

```rust

use openraft::ChangeMembers;
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{
```

`crates/opl-cluster/tests/voters.rs`에서

```rust
        let members = (1..=n)
            .map(|i| {
                (
                    NodeId::from(i),
                    openraft::BasicNode::new(format!("node-{i}")),
                )
            })
            .collect();
```

을 다음으로 바꾼다:

```rust
        let members = (1..=n)
            .map(|i| (NodeId::from(i), OrbNode::single(format!("node-{i}"))))
            .collect();
```

`crates/opl-cluster/tests/voters.rs`에서

```rust
        .unwrap()
        .add_learner(ghost, BasicNode::new("node-9"), false)
        .await
```

을 다음으로 바꾼다:

```rust
        .unwrap()
        .add_learner(ghost, OrbNode::single("node-9"), false)
        .await
```

`crates/opl-peer/src/client.rs`에서

```rust
use futures_util::stream::BoxStream;
use openraft::BasicNode;
use openraft::error::{
```

을 다음으로 바꾼다:

```rust
use futures_util::stream::BoxStream;
use openraft::error::{
```

`crates/opl-peer/src/client.rs`에서

```rust
};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, FollowClient, FollowFrame, Forwarder};
```

을 다음으로 바꾼다:

```rust
};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
use opl_cluster::{ClusterError, FollowClient, FollowFrame, Forwarder};
```

`crates/opl-peer/src/client.rs`에서

```rust

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `BasicNode.addr`는 노드의 클러스터 주소다.
#[derive(Clone)]
```

을 다음으로 바꾼다:

```rust

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `OrbNode::addr()`(첫 링크)는 노드의 클러스터 주소다.
#[derive(Clone)]
```

`crates/opl-peer/src/client.rs`에서

```rust

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr.clone(),
            peers: self.peers.clone(),
```

을 다음으로 바꾼다:

```rust

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr().to_string(),
            peers: self.peers.clone(),
```

`crates/opl-peer/src/client.rs`에서

```rust
type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, BasicNode, RaftError<NodeId, E>> {
    if matches!(
```

을 다음으로 바꾼다:

```rust
type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, OrbNode, RaftError<NodeId, E>> {
    if matches!(
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
use openraft::raft::VoteRequest;
use openraft::{BasicNode, ServerState, Vote};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig, FollowClient, FollowFrame, NodeRole};
```

을 다음으로 바꾼다:

```rust
use openraft::raft::VoteRequest;
use openraft::{ServerState, Vote};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, FollowClient, FollowFrame, NodeRole};
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
```

을 다음으로 바꾼다:

```rust
        .iter()
        .map(|n| (n.id(), OrbNode::single(&n.addr)))
        .collect();
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    .await;
    let members = BTreeMap::from([(node.id(), BasicNode::new(&node.addr))]);
    node.cluster.initialize(members).await.unwrap();
```

을 다음으로 바꾼다:

```rust
    .await;
    let members = BTreeMap::from([(node.id(), OrbNode::single(&node.addr))]);
    node.cluster.initialize(members).await.unwrap();
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    let mut raft = GrpcNetwork::new(nodes[0].peers.clone())
        .new_client(observer.id(), &BasicNode::new(&observer.addr))
        .await;
```

을 다음으로 바꾼다:

```rust
    let mut raft = GrpcNetwork::new(nodes[0].peers.clone())
        .new_client(observer.id(), &OrbNode::single(&observer.addr))
        .await;
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust

use openraft::{BasicNode, ServerState};
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

을 다음으로 바꾼다:

```rust

use openraft::ServerState;
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::StoreError;
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
    let cluster = open_node(router, id, dir, raft_config(), Duration::from_secs(5)).await;
    let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
    cluster.initialize(members).await.unwrap();
```

을 다음으로 바꾼다:

```rust
    let cluster = open_node(router, id, dir, raft_config(), Duration::from_secs(5)).await;
    let members = BTreeMap::from([(id, OrbNode::single(id.to_string()))]);
    cluster.initialize(members).await.unwrap();
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
use common::{WAIT, open_node, raft_config, vm, wait_leader, wait_vm};
use openraft::BasicNode;
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_driver::{Hypervisor, MockHypervisor};
```

을 다음으로 바꾼다:

```rust
use common::{WAIT, open_node, raft_config, vm, wait_leader, wait_vm};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::{NodeId, OrbNode};
use opl_driver::{Hypervisor, MockHypervisor};
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, BasicNode::new("n1")), (n2, BasicNode::new("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let hv = Arc::new(MockHypervisor::default());
```

을 다음으로 바꾼다:

```rust
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, OrbNode::single("n1")), (n2, OrbNode::single("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let hv = Arc::new(MockHypervisor::default());
```

`crates/opl-reconciler/tests/no_quorum.rs`에서

```rust
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, BasicNode::new("n1")), (n2, BasicNode::new("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let guard = HaGuard::spawn(
```

을 다음으로 바꾼다:

```rust
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, OrbNode::single("n1")), (n2, OrbNode::single("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let guard = HaGuard::spawn(
```

`crates/opld/src/bootstrap.rs`에서

```rust
    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
```

을 다음으로 바꾼다:

```rust
    use async_trait::async_trait;
    use openraft::ServerState;
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_cluster::types::OrbNode;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
```

`crates/opld/src/bootstrap.rs`에서

```rust
        router.register(id, &cluster);
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
```

을 다음으로 바꾼다:

```rust
        router.register(id, &cluster);
        let members = BTreeMap::from([(id, OrbNode::single(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
```

`crates/opld/src/member.rs`에서

```rust
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
```

을 다음으로 바꾼다:

```rust
use axum::serve::ListenerExt;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
```

`crates/opld/src/member.rs`에서

```rust
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{
```

을 다음으로 바꾼다:

```rust
};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{
```

`crates/opld/src/member.rs`에서

```rust
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
```

을 다음으로 바꾼다:

```rust
                Bootstrap::Init { name, ca } => {
                    let me = OrbNode::single(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
```

`crates/opld/src/member.rs`에서

```rust
/// 멤버십에 `id`가 있고 그 멤버십까지 저장소에 적용했다(그 전에 더한 노드들의 Node 리소스가 보인다).
fn membership_applied(m: &openraft::RaftMetrics<NodeId, BasicNode>, id: NodeId) -> bool {
    m.membership_config
```

을 다음으로 바꾼다:

```rust
/// 멤버십에 `id`가 있고 그 멤버십까지 저장소에 적용했다(그 전에 더한 노드들의 Node 리소스가 보인다).
fn membership_applied(m: &openraft::RaftMetrics<NodeId, OrbNode>, id: NodeId) -> bool {
    m.membership_config
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1889개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): OrbNode 노드 타입 — Raft 구성원 값을 링크 목록으로 (동작 변화 없음)"
```

---

### Task 2: 링크 고르기와 넘어가기

새 `opl-cluster/src/links.rs`: `LinkBook`(첫 링크 → 링크 목록, 적용된 멤버십으로 `Cluster`가 고침)과 `LinkSelector`(노드마다 지금 링크; 연결 실패면 곧바로, 제한 시간 초과면 연속 두 번이거나 마지막 성공에서 400 ms 뒤 다음 링크로; 우선 링크가 아니면 30초마다 control 채널의 짧은 호출 하나를 우선 링크로 시험; 넘어간 뒤 보낸 호출의 성공으로만 돌아옴). opl-peer `Peers`가 `(Lane, 링크)`마다 채널을 두고 호출마다 링크를 골라 결과를 알리며, 요청이 나가지 않았음이 확실한 실패(`tonic::ConnectError`, 채널 만들기 실패, 다른 노드 거절)만 같은 호출 안에서 다른 링크로 한 번 더 보낸다. 대상 id를 아는 호출은 gRPC 메타데이터 `orbivisor-target`을 싣고 서버는 자기 id와 다르면 `permission_denied`. 리스 갱신 gRPC 제한 시간 3초, ping·리더 요청의 Peers 안 제한 시간. memnet `cut_link`·`sent`. opld가 `peers.set_link_book(cluster.link_book())`.

**Files:**
- Create: `crates/opl-cluster/src/links.rs`
- Create: `crates/opl-cluster/tests/links.rs`
- Modify: `crates/opl-api/src/audit.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/firmware.rs`
- Modify: `crates/opl-api/src/metrics.rs`
- Modify: `crates/opl-cluster/Cargo.toml`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/memnet.rs`
- Modify: `crates/opl-cluster/src/replica.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/monitor.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/audit.rs`에서

```rust
        PEER_READ_TIMEOUT,
        peers.read_audit(addr, query, PEER_READ_TIMEOUT),
    )
```

을 다음으로 바꾼다:

```rust
        PEER_READ_TIMEOUT,
        peers.read_audit(id, addr, query, PEER_READ_TIMEOUT),
    )
```

`crates/opl-api/src/cluster.rs`에서

```rust
            observers: addrs(&[5]),
        };
```

을 다음으로 바꾼다:

```rust
            observers: addrs(&[5]),
            links: Default::default(),
        };
```

`crates/opl-api/src/firmware.rs`에서

```rust
        self.peers
            .uefi_vars(&addr, vm, &uefi_vars_path(&self.state_dir, vm), Some(len))
            .await
```

을 다음으로 바꾼다:

```rust
        self.peers
            .uefi_vars(
                NodeId(node),
                &addr,
                vm,
                &uefi_vars_path(&self.state_dir, vm),
                Some(len),
            )
            .await
```

`crates/opl-api/src/metrics.rs`에서

```rust
        PEER_READ_TIMEOUT,
        peers.read_metrics(addr, query, PEER_READ_TIMEOUT),
    )
```

을 다음으로 바꾼다:

```rust
        PEER_READ_TIMEOUT,
        peers.read_metrics(id, addr, query, PEER_READ_TIMEOUT),
    )
```

`crates/opl-cluster/Cargo.toml`에서

```toml
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
tempfile.workspace = true
tokio = { workspace = true, features = ["test-util"] }
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
use openraft::{Raft, RaftMetrics, RaftNetworkFactory};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::follow::{self, FollowClient, FollowFrame, NodeRole};
use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::links::{LINK_RETRY, LinkBook};
use crate::log_store::LogStore;
use crate::replica::{Replica, read_membership};
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
    /// 우선 링크가 아닌 동안 우선 링크를 다시 시도하는 간격(memnet이 쓴다, 기본 [`LINK_RETRY`]). 시험용이다.
    #[doc(hidden)]
    pub link_retry: Duration,
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
            link_retry: LINK_RETRY,
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
    /// 노드마다의 클러스터 링크(적용된 멤버십의 `OrbNode`). 관찰 노드 목록과 함께 고친다.
    link_book: Arc<LinkBook>,
    /// memnet이 이 노드의 링크 선택에 쓰는 우선 링크 재시도 간격
    link_retry: Duration,
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

/// 지금 알고 있는 멤버십. 값은 노드의 Raft 주소(첫 링크)다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub leader: Option<NodeId>,
    pub voters: BTreeMap<NodeId, String>,
    pub learners: BTreeMap<NodeId, String>,
    /// Raft 밖의 관찰 노드(클러스터 정보의 `observers` 가운데 Raft 구성원이 아닌 것)
    pub observers: BTreeMap<NodeId, String>,
    /// 위 모든 노드의 클러스터 링크(앞이 우선, 첫 것이 위의 주소)
    pub links: BTreeMap<NodeId, Vec<String>>,
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
        let mut cluster = Self::assemble(&config, store, backend, start_raft);
        cluster.follow = follow;
        Ok(cluster)
    }

    fn assemble(
        config: &ClusterConfig,
        store: Store,
        backend: Backend,
        start_raft: StartRaft,
    ) -> Self {
        let id = config.node_id;
        let link_book = Arc::new(LinkBook::default());
        let observers = track_observers(store.db(), store.watch_hub(), &link_book);
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
            write_timeout: config.write_timeout,
            forwarder: None,
            start_raft,
            follow: None,
            transition: tokio::sync::Mutex::new(false),
            epoch: tokio::sync::watch::channel(0).0,
            apply_lag: Default::default(),
            observers,
            link_book,
            link_retry: config.link_retry,
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

    /// 노드마다의 클러스터 링크. 노드 간 연결(`Peers::set_link_book`)이 노드의 첫 링크로 링크 목록을 찾는다.
    pub fn link_book(&self) -> Arc<LinkBook> {
        self.link_book.clone()
    }

    pub(crate) fn link_retry(&self) -> Duration {
        self.link_retry
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
            links: BTreeMap::new(),
        };
        for (id, node) in membership.nodes() {
            let list = if voters.contains(id) {
                &mut out.voters
            } else {
                &mut out.learners
            };
            list.insert(*id, node.addr().to_string());
            out.links.insert(*id, node.addrs.clone());
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
        for (id, addr) in &out.observers {
            out.links.insert(*id, vec![addr.clone()]);
        }
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

/// 클러스터 정보의 관찰 노드 목록과 적용된 멤버십의 링크(`book`)를 읽어 두고, `Kind::Cluster` 변경과 멤버십
/// 변경을 따라 고치는 작업을 띄운다. 작업은 저장소가 닫히거나(피드가 `Closed`) 목록을 쥔 `Cluster`가 사라지면
/// 끝난다.
fn track_observers(
    db: &Arc<Database>,
    hub: &Arc<WatchHub>,
    book: &Arc<LinkBook>,
) -> Arc<RwLock<BTreeMap<NodeId, String>>> {
    let cache = Arc::new(RwLock::new(BTreeMap::new()));
    // 구독한 뒤 읽어야 그 사이의 변경을 놓치지 않는다.
    let feed = hub.feed(hub.revision()).ok();
    *cache.write().unwrap() = read_observers(db);
    read_links(db, book);
    let (db, hub, weak, book) = (
        Arc::downgrade(db),
        Arc::downgrade(hub),
        Arc::downgrade(&cache),
        book.clone(),
    );
    tokio::spawn(async move {
        let mut feed = feed;
        loop {
            if let Some(mut f) = feed.take()
                && !follow_observers(&mut f, &weak, &book).await
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
            read_links(&db, &book);
            if feed.is_none() {
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    });
    cache
}

/// 피드의 `Kind::Cluster` 변경으로 목록을, 멤버십 변경으로 링크를 고친다. 다시 읽어야 하면(`Reset`·`Lagged`)
/// `true`, 끝나야 하면(저장소가 닫힘, 목록이 사라짐) `false`다.
async fn follow_observers(
    feed: &mut Feed,
    cache: &Weak<RwLock<BTreeMap<NodeId, String>>>,
    book: &LinkBook,
) -> bool {
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
        if let Some(m) = &batch.membership {
            book.replace(m.membership().nodes().map(|(_, node)| node));
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

/// 저장소에 적용된 멤버십의 링크로 `book`을 고친다.
fn read_links(db: &Database, book: &LinkBook) {
    match read_membership(db) {
        Ok(m) => book.replace(m.membership().nodes().map(|(_, node)| node)),
        Err(e) => tracing::warn!(error = %e, "cannot read the stored membership"),
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
```

`crates/opl-cluster/src/lib.rs`에서

```rust
mod lease;
mod log_store;
```

을 다음으로 바꾼다:

```rust
mod lease;
pub mod links;
mod log_store;
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use lease::Leases;
pub use log_store::LogStore;
```

을 다음으로 바꾼다:

```rust
pub use lease::Leases;
pub use links::{Failure, LINK_DEAD_AFTER, LINK_RETRY, LinkBook, LinkSelector, Pick};
pub use log_store::LogStore;
```

`crates/opl-cluster/src/links.rs`를 만든다:

```rust
//! 노드 하나에 닿는 클러스터 링크 여럿 가운데 쓸 것을 고른다(설계 §7). 노드 간 gRPC(`opl-peer`의 `Peers`)와
//! memnet이 같은 규칙을 쓴다.
//!
//! 노드는 첫 링크(이 계획 전의 "그 노드의 주소", `Members`의 값)로 가리킨다. 처음에는 첫 링크를 쓰고, 연결하지
//! 못하면 곧바로, 제한 시간을 넘기면 지금 링크에서 연속 두 번 넘겼거나 그 링크의 마지막 성공에서
//! [`LINK_DEAD_AFTER`]가 지났을 때 다음 링크로 넘어간다(모두 실패하면 돌아가며). 우선 링크가 아닌 동안
//! [`LINK_RETRY`]마다 다음 호출 하나를 우선 링크로 보내 보고(시험 호출), 성공하면 돌아간다.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use tokio::time::Instant;

use crate::types::OrbNode;

/// 우선 링크가 아닌 동안 우선 링크를 다시 시도하는 간격
pub const LINK_RETRY: Duration = Duration::from_secs(30);

/// 제한 시간을 넘긴 링크를 죽었다고 보는, 그 링크의 마지막 성공(또는 그 링크로 넘어온 때)부터의 시간. 이 안의
/// 초과 한 번(하트비트 하나가 늦은 살아 있는 링크)으로는 넘어가지 않는다. 조용히 죽은 링크에서 마지막 성공 뒤 첫
/// 하트비트가 다음 링크로 가기까지(이것 + 하트비트 간격 200 ms)가 선거 제한 시간의 최솟값(1초)에서 하트비트
/// 간격을 뺀 것보다 짧아야 따르는 노드가 선거를 시작하지 않는다.
pub const LINK_DEAD_AFTER: Duration = Duration::from_millis(400);

/// 지금 링크에서 연속으로 이만큼 제한 시간을 넘기면 [`LINK_DEAD_AFTER`] 전이어도 넘어간다.
const TIMEOUTS_TO_SWITCH: u32 = 2;

/// 노드마다의 링크 목록(첫 링크 → 링크 목록). 링크가 하나인 노드는 넣지 않는다.
#[derive(Debug, Default)]
pub struct LinkBook {
    links: RwLock<HashMap<String, Vec<String>>>,
}

impl LinkBook {
    /// 첫 링크가 `addr`인 노드의 링크 목록. 모르는 노드는 `[addr]`이다.
    pub fn links(&self, addr: &str) -> Vec<String> {
        self.links
            .read()
            .unwrap()
            .get(addr)
            .cloned()
            .unwrap_or_else(|| vec![addr.to_string()])
    }

    /// 목록을 `nodes`로 바꾼다.
    pub fn replace<'a>(&self, nodes: impl IntoIterator<Item = &'a OrbNode>) {
        let links = nodes
            .into_iter()
            .filter(|n| n.addrs.len() > 1)
            .map(|n| (n.addr().to_string(), n.addrs.clone()))
            .collect();
        *self.links.write().unwrap() = links;
    }
}

/// 링크 하나로 보낸 호출이 실패한 까닭
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 연결하지 못했다.
    Connect,
    /// 제한 시간 안에 답이 오지 않았다.
    Timeout,
}

/// 호출 하나가 쓸 링크
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub link: String,
    /// 지금 링크 대신 우선 링크로 보내 보는 시험 호출이다.
    pub trial: bool,
    /// 고른 때(호출을 보낸 때). 성공을 알릴 때 함께 준다([`LinkSelector::ok`]).
    pub sent: Instant,
}

/// 노드마다 지금 쓰는 링크. 링크가 둘 이상인 노드만 상태를 둔다.
#[derive(Debug)]
pub struct LinkSelector {
    retry: Duration,
    nodes: Mutex<HashMap<String, LinkState>>,
}

#[derive(Debug)]
struct LinkState {
    links: Vec<String>,
    /// 지금 쓰는 링크(`links`의 위치)
    active: usize,
    /// 지금 링크의 마지막 성공(그 링크로 넘어온 때부터 센다)
    last_ok: Instant,
    /// 지금 링크에서 마지막 성공 뒤 연속으로 넘긴 제한 시간
    timeouts: u32,
    /// 지금 링크로 넘어온 때, 또는 마지막 시험 호출을 보낸 때
    switched_at: Instant,
    /// 시험 호출이 우선 링크로 가 있다.
    trial: bool,
}

impl LinkState {
    fn new(links: &[String]) -> Self {
        let now = Instant::now();
        Self {
            links: links.to_vec(),
            active: 0,
            last_ok: now,
            timeouts: 0,
            switched_at: now,
            trial: false,
        }
    }
}

impl Default for LinkSelector {
    fn default() -> Self {
        Self::new(LINK_RETRY)
    }
}

impl LinkSelector {
    /// 우선 링크를 `retry`마다 다시 시도한다(시험은 짧게 준다).
    pub fn new(retry: Duration) -> Self {
        Self {
            retry,
            nodes: Mutex::new(HashMap::new()),
        }
    }

    /// 첫 링크가 `addr`이고 링크가 `links`인 노드로 보낼 호출 하나의 링크. 때가 되면 우선 링크로 보내는 시험
    /// 호출이다.
    pub fn pick(&self, addr: &str, links: &[String]) -> Pick {
        self.choose(addr, links, true)
    }

    /// [`pick`](Self::pick)과 같지만 시험 호출로 고르지 않는다(늘 지금 링크). 오래 걸리거나 큰 호출에 쓴다.
    pub fn current(&self, addr: &str, links: &[String]) -> Pick {
        self.choose(addr, links, false)
    }

    fn choose(&self, addr: &str, links: &[String], may_trial: bool) -> Pick {
        let mut nodes = self.nodes.lock().unwrap();
        let sent = Instant::now();
        if links.len() < 2 {
            nodes.remove(addr);
            return Pick {
                link: links.first().map_or(addr, String::as_str).to_string(),
                trial: false,
                sent,
            };
        }
        let state = nodes
            .entry(addr.to_string())
            .or_insert_with(|| LinkState::new(links));
        // 링크 목록이 바뀌었으면 처음부터 다시 센다.
        if state.links != links {
            *state = LinkState::new(links);
        }
        // 시험 호출을 보낸 뒤 결과 없이 `retry`가 지나면(버려진 호출) 다시 보낸다.
        if may_trial && state.active != 0 && state.switched_at.elapsed() >= self.retry {
            state.trial = true;
            state.switched_at = sent;
            return Pick {
                link: state.links[0].clone(),
                trial: true,
                sent,
            };
        }
        Pick {
            link: state.links[state.active].clone(),
            trial: false,
            sent,
        }
    }

    /// `sent`에 `link`로 보낸 호출에 상대가 답했다. 우선 링크면 그리로 돌아간다. 다만 지금 링크로 넘어오기(또는 마지막
    /// 시험 호출) 전에 보낸 호출의 늦은 성공으로는 돌아가지 않는다(그 뒤에 죽었을 수 있다).
    pub fn ok(&self, addr: &str, link: &str, sent: Instant) {
        let mut nodes = self.nodes.lock().unwrap();
        let Some(state) = nodes.get_mut(addr) else {
            return;
        };
        if state.active != 0 && link == state.links[0] {
            if sent < state.switched_at {
                return;
            }
            tracing::info!(node = addr, link, "back to the preferred cluster link");
            state.active = 0;
            state.trial = false;
            state.last_ok = Instant::now();
            state.timeouts = 0;
        } else if link == state.links[state.active] {
            state.last_ok = Instant::now();
            state.timeouts = 0;
        }
    }

    /// `link`로 보낸 호출이 `failure`로 실패했다. 지금 링크가 죽었다고 보면 다음 링크로 넘어간다.
    pub fn failed(&self, addr: &str, link: &str, failure: Failure) {
        let mut nodes = self.nodes.lock().unwrap();
        let Some(state) = nodes.get_mut(addr) else {
            return;
        };
        let active = state.links[state.active].clone();
        // 시험 호출이 실패했다. 지금 링크에 머물고 `retry`를 다시 잰다.
        if state.trial && state.active != 0 && link == state.links[0] {
            tracing::debug!(
                node = addr,
                link,
                ?failure,
                "the preferred cluster link is still down"
            );
            state.trial = false;
            state.switched_at = Instant::now();
            return;
        }
        // 넘어가기 전에 보낸 호출의 늦은 실패
        if link != active {
            return;
        }
        if failure == Failure::Timeout {
            state.timeouts += 1;
            if state.timeouts < TIMEOUTS_TO_SWITCH && state.last_ok.elapsed() < LINK_DEAD_AFTER {
                return;
            }
        }
        state.active = (state.active + 1) % state.links.len();
        let now = Instant::now();
        state.last_ok = now;
        state.timeouts = 0;
        state.switched_at = now;
        state.trial = false;
        tracing::warn!(
            node = addr,
            from = %active,
            to = %state.links[state.active],
            ?failure,
            "cluster link failed; switching to the next link"
        );
    }

    /// 첫 링크가 `addr`인 노드에 지금 쓰는 링크. 링크가 하나인(상태가 없는) 노드는 `None`이다.
    pub fn active(&self, addr: &str) -> Option<String> {
        let nodes = self.nodes.lock().unwrap();
        nodes.get(addr).map(|s| s.links[s.active].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links() -> Vec<String> {
        vec!["a".into(), "b".into(), "c".into()]
    }

    /// 고른 링크와 시험 호출인지
    fn chosen(p: Pick) -> (String, bool) {
        (p.link, p.trial)
    }

    fn pick(s: &LinkSelector) -> (String, bool) {
        chosen(s.pick("a", &links()))
    }

    fn on(link: &str, trial: bool) -> (String, bool) {
        (link.into(), trial)
    }

    /// 지금 보낸 호출의 성공
    fn ok(s: &LinkSelector, link: &str) {
        s.ok("a", link, Instant::now());
    }

    #[test]
    fn the_book_lists_only_nodes_with_several_links() {
        let book = LinkBook::default();
        book.replace(&[
            OrbNode::new(["a", "b"]),
            OrbNode::single("c"),
            OrbNode::default(),
        ]);
        assert_eq!(book.links("a"), vec!["a", "b"]);
        assert_eq!(book.links("c"), vec!["c"]);
        assert_eq!(book.links("x"), vec!["x"]);
        assert_eq!(book.links.read().unwrap().len(), 1);
        book.replace(&[]);
        assert_eq!(book.links("a"), vec!["a"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_link_keeps_no_state() {
        let s = LinkSelector::default();
        assert_eq!(chosen(s.pick("a", &["a".to_string()])), on("a", false));
        assert_eq!(chosen(s.pick("a", &[])), on("a", false));
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a"), None);
        // 링크가 하나로 줄면 상태를 버린다.
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        assert_eq!(chosen(s.pick("a", &["a".to_string()])), on("a", false));
        assert_eq!(s.active("a"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_failure_switches_at_once_and_all_failing_rotates() {
        let s = LinkSelector::default();
        assert_eq!(pick(&s), on("a", false));
        s.failed("a", "a", Failure::Connect);
        assert_eq!(pick(&s), on("b", false));
        // 넘어가기 전에 보낸 호출의 늦은 실패는 세지 않는다.
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        s.failed("a", "b", Failure::Connect);
        assert_eq!(pick(&s), on("c", false));
        s.failed("a", "c", Failure::Connect);
        assert_eq!(pick(&s), on("a", false));
    }

    #[tokio::test(start_paused = true)]
    async fn a_timeout_switches_after_two_in_a_row_or_400_ms_after_the_last_success() {
        let s = LinkSelector::default();
        pick(&s);
        // 넘어온(처음 고른) 때부터 400 ms 안의 초과 한 번은 넘기지 않는다.
        tokio::time::advance(Duration::from_millis(300)).await;
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("a"));
        // 그 사이 성공이 있으면 다시 센다.
        ok(&s, "a");
        tokio::time::advance(Duration::from_millis(300)).await;
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("a"));
        // 연속 두 번이면 400 ms 전이어도 넘어간다.
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        // 새 링크는 넘어온 때부터 센다: 400 ms가 지난 초과 한 번이면 넘어간다.
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        ok(&s, "b");
        tokio::time::advance(Duration::from_millis(399)).await;
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        ok(&s, "b");
        tokio::time::advance(Duration::from_millis(400)).await;
        s.failed("a", "b", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("c"));
    }

    /// 조용히 죽은 링크(하트비트가 200 ms마다 가고 200 ms에 끊김): 마지막 성공 뒤 첫 하트비트가 다음 링크로 가기까지
    /// 선거 제한 시간의 최솟값(1초)에서 하트비트 간격을 뺀 것보다 짧다.
    #[tokio::test(start_paused = true)]
    async fn a_silent_link_is_left_before_an_election() {
        let s = LinkSelector::default();
        pick(&s);
        ok(&s, "a");
        let last_ok = Instant::now();
        loop {
            // 200 ms마다 하트비트를 보내고, 200 ms에 제한 시간을 넘긴다.
            tokio::time::advance(Duration::from_millis(200)).await;
            let (link, _) = pick(&s);
            if link != "a" {
                break;
            }
            tokio::time::advance(Duration::from_millis(200)).await;
            s.failed("a", &link, Failure::Timeout);
        }
        assert!(
            last_ok.elapsed() < Duration::from_millis(1000 - 200),
            "{:?}",
            last_ok.elapsed()
        );
    }

    /// 넘어가기 전에 보낸 호출이 우선 링크에서 늦게 성공해도 돌아가지 않는다. 넘어간 뒤 보낸 시험 호출의 성공만
    /// 돌아가게 한다.
    #[tokio::test(start_paused = true)]
    async fn a_late_success_from_before_the_switch_does_not_switch_back() {
        let s = LinkSelector::new(Duration::from_secs(30));
        let early = s.pick("a", &links());
        tokio::time::advance(Duration::from_millis(10)).await;
        s.failed("a", "a", Failure::Connect);
        s.ok("a", &early.link, early.sent);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        tokio::time::advance(Duration::from_secs(30)).await;
        let trial = s.pick("a", &links());
        assert!(trial.trial);
        // 시험 호출 전에 보낸 것(버려진 옛 시험 호출 등)도 세지 않는다.
        s.ok("a", "a", early.sent);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        s.ok("a", &trial.link, trial.sent);
        assert_eq!(s.active("a").as_deref(), Some("a"));
    }

    /// 시험 호출로 고르지 않는 호출은 재시도 간격이 지나도 지금 링크로 간다.
    #[tokio::test(start_paused = true)]
    async fn current_never_tries_the_preferred_link() {
        let s = LinkSelector::new(Duration::from_secs(30));
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(chosen(s.current("a", &links())), on("b", false));
        assert_eq!(pick(&s), on("a", true));
    }

    #[tokio::test(start_paused = true)]
    async fn one_call_tries_the_preferred_link_after_the_retry_interval() {
        let s = LinkSelector::new(Duration::from_secs(30));
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(pick(&s), on("b", false));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pick(&s), on("a", true));
        // 시험 중인 동안 다른 호출은 지금 링크로 간다.
        assert_eq!(pick(&s), on("b", false));
        assert_eq!(pick(&s), on("b", false));

        // 시험이 실패하면 머물고 30초를 다시 잰다.
        s.failed("a", "a", Failure::Timeout);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(pick(&s), on("b", false));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(pick(&s), on("a", true));
        // 결과 없이 버려진 시험은 30초 뒤 다시 보낸다.
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(pick(&s), on("a", true));

        // 시험이 성공하면 돌아간다.
        ok(&s, "a");
        assert_eq!(s.active("a").as_deref(), Some("a"));
        assert_eq!(pick(&s), on("a", false));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(pick(&s), on("a", false));
    }

    #[tokio::test(start_paused = true)]
    async fn new_links_start_over() {
        let s = LinkSelector::default();
        pick(&s);
        s.failed("a", "a", Failure::Connect);
        assert_eq!(s.active("a").as_deref(), Some("b"));
        let fewer = vec!["a".to_string(), "c".to_string()];
        assert_eq!(chosen(s.pick("a", &fewer)), on("a", false));
    }
}
```

`crates/opl-cluster/src/memnet.rs` 전체:

```rust
//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크와 리더 전달자. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 `opl-peer`에 있다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::Raft;
use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};

use crate::cluster::{Cluster, ClusterError, NodeHandle};
use crate::follow::{FollowClient, FollowFrame};
use crate::leader::{self, Forwarder};
use crate::links::{Failure, LinkBook, LinkSelector};
use crate::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};

/// 전달받은 요청을 리더가 처리하는 최대 시간
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Routes {
    /// 노드의 백엔드. RPC를 보낼 때마다 지금 백엔드를 읽는다.
    nodes: HashMap<NodeId, NodeHandle>,
    isolated: HashSet<NodeId>,
    /// Raft RPC만 끊긴 노드. 리더 전달은 된다.
    raft_blocked: HashSet<NodeId>,
    /// 보낸 Raft RPC는 닿지만 답을 받지 못하는 노드
    replies_lost: HashSet<NodeId>,
    /// 노드에 건 Follow 수
    follows: HashMap<NodeId, usize>,
    /// Follow 프레임을 받지 못하고 멈춘 노드(풀면 이어 받는다)
    follow_paused: HashSet<NodeId>,
    /// 스냅샷 조각을 하나마다 이만큼 늦게 받는 노드
    slow_snapshots: HashMap<NodeId, Duration>,
    /// 보내는 노드의 링크 목록과 링크 선택(`Peers`처럼 노드마다 하나)
    senders: HashMap<NodeId, Sender>,
    /// 끊긴 (보내는 노드, 링크). 그 노드가 그 링크로 보내는 호출은 연결하지 못한다.
    cut: HashSet<(NodeId, String)>,
    /// 링크마다 보낸 호출 수(끊긴 링크로 보낸 것 포함)
    sent: HashMap<String, u64>,
}

/// 보내는 노드 하나가 링크를 고르는 것
#[derive(Clone)]
struct Sender {
    book: Arc<LinkBook>,
    links: Arc<LinkSelector>,
}

/// 호출 하나가 고른 링크. 결과를 보내는 노드의 링크 선택에 알린다.
struct LinkUse {
    links: Option<Arc<LinkSelector>>,
    addr: String,
    link: String,
    /// 고른 때
    sent: tokio::time::Instant,
}

impl LinkUse {
    fn report(&self, reached: bool) {
        if let Some(links) = &self.links {
            if reached {
                links.ok(&self.addr, &self.link, self.sent);
            } else {
                links.failed(&self.addr, &self.link, Failure::Connect);
            }
        }
    }
}

/// 노드 등록부. 등록되지 않았거나 격리된 노드로 가는 RPC는 `Unreachable`이 된다.
#[derive(Clone, Default)]
pub struct MemRouter {
    routes: Arc<Mutex<Routes>>,
}

impl MemRouter {
    /// `from` 노드가 쓸 네트워크. `Cluster::open`에 넘긴다.
    pub fn network(&self, from: NodeId) -> MemNetwork {
        MemNetwork {
            from,
            router: self.clone(),
        }
    }

    /// `from` 노드가 쓸 리더 전달자. `Cluster::with_forwarder`에 넘긴다.
    pub fn forwarder(&self, from: NodeId) -> Arc<MemForwarder> {
        Arc::new(MemForwarder {
            from,
            router: self.clone(),
        })
    }

    /// `from` 노드(관찰 노드)가 쓸 Follow 통로. `Cluster::open_observer`에 넘긴다.
    pub fn follower(&self, from: NodeId) -> Arc<MemFollow> {
        Arc::new(MemFollow {
            from,
            router: self.clone(),
        })
    }

    /// 노드를 등록한다. 그 노드가 보내는 호출은 노드의 링크 책(멤버십의 `OrbNode`)과 이 노드의 링크 선택으로
    /// 링크를 고른다.
    pub fn register(&self, id: NodeId, cluster: &Cluster) {
        let mut routes = self.routes.lock().unwrap();
        routes.nodes.insert(id, cluster.node_handle());
        routes.senders.insert(
            id,
            Sender {
                book: cluster.link_book(),
                links: Arc::new(LinkSelector::new(cluster.link_retry())),
            },
        );
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    /// 그 노드로 가거나 나가는 Raft RPC만 끊는다. 리더 전달은 그대로 된다(복제가 늦은 노드).
    pub fn block_raft(&self, id: NodeId) {
        self.routes.lock().unwrap().raft_blocked.insert(id);
    }

    /// 그 노드가 보낸 Raft RPC는 상대가 처리하지만 답은 사라진다(리더가 보낸 항목을 팔로워는 받았는데 리더는
    /// 커밋을 모른다).
    pub fn lose_replies(&self, id: NodeId) {
        self.routes.lock().unwrap().replies_lost.insert(id);
    }

    /// 그 노드가 받는 Follow 프레임을 멈춘다(연결은 그대로, 풀면 이어 받는다). 프레임이 늦게 오는 관찰 노드.
    pub fn pause_follow(&self, id: NodeId) {
        self.routes.lock().unwrap().follow_paused.insert(id);
    }

    /// 그 노드가 받는 스냅샷 조각을 하나마다 `delay`씩 늦춘다(다른 프레임은 그대로). 스냅샷이 오래 걸리는 관찰 노드.
    pub fn slow_snapshots(&self, id: NodeId, delay: Duration) {
        self.routes.lock().unwrap().slow_snapshots.insert(id, delay);
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

    /// `from`이 `link`로 보내는 호출을 끊는다(연결 실패). 다른 링크는 그대로다.
    pub fn cut_link(&self, from: NodeId, link: &str) {
        self.routes
            .lock()
            .unwrap()
            .cut
            .insert((from, link.to_string()));
    }

    /// 모든 노드가 `link`로 보낸 호출 수(끊긴 동안 보낸 것 포함)
    pub fn sent(&self, link: &str) -> u64 {
        self.routes
            .lock()
            .unwrap()
            .sent
            .get(link)
            .copied()
            .unwrap_or(0)
    }

    /// 노드의 격리·끊긴 Raft·끊긴 링크(`cut_link`) 등을 모두 푼다.
    pub fn heal(&self, id: NodeId) {
        let mut routes = self.routes.lock().unwrap();
        routes.cut.retain(|(from, _)| *from != id);
        routes.isolated.remove(&id);
        routes.raft_blocked.remove(&id);
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

    /// `from`이 첫 링크가 `addr`인 노드로 보낼 호출의 링크를 고른다. `links`가 없으면 `from`의 링크 책에서 찾는다.
    fn pick(&self, from: NodeId, addr: &str, links: Option<&[String]>) -> LinkUse {
        let sender = self.routes.lock().unwrap().senders.get(&from).cloned();
        let links = match (links, &sender) {
            (Some(links), _) => links.to_vec(),
            (None, Some(sender)) => sender.book.links(addr),
            (None, None) => vec![addr.to_string()],
        };
        let (link, sent) = match &sender {
            Some(sender) => {
                let pick = sender.links.pick(addr, &links);
                (pick.link, pick.sent)
            }
            None => (
                links.first().map_or(addr, String::as_str).to_string(),
                tokio::time::Instant::now(),
            ),
        };
        *self
            .routes
            .lock()
            .unwrap()
            .sent
            .entry(link.clone())
            .or_default() += 1;
        LinkUse {
            links: sender.map(|s| s.links),
            addr: addr.to_string(),
            link,
            sent,
        }
    }

    fn is_cut(&self, from: NodeId, link: &str) -> bool {
        self.routes
            .lock()
            .unwrap()
            .cut
            .contains(&(from, link.to_string()))
    }

    /// `from`이 고른 링크로 `to`에 닿는다. 끊긴 링크거나 닿지 않으면 연결 실패로 알린다.
    fn route_via(
        &self,
        from: NodeId,
        to: NodeId,
        link: &LinkUse,
    ) -> Result<NodeHandle, Unreachable> {
        let routed = if self.is_cut(from, &link.link) {
            Err(unreachable("link cut"))
        } else {
            self.route(from, to)
        };
        link.report(routed.is_ok());
        routed
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<NodeHandle, Unreachable> {
        let routes = self.routes.lock().unwrap();
        if routes.isolated.contains(&from) || routes.isolated.contains(&to) {
            return Err(unreachable("isolated"));
        }
        routes
            .nodes
            .get(&to)
            .cloned()
            .ok_or_else(|| unreachable("not registered"))
    }

    /// 받는 노드의 Raft와, 답을 버려야 하는지
    fn raft_route(
        &self,
        from: NodeId,
        to: NodeId,
        link: &LinkUse,
    ) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let node = self.route_via(from, to, link)?;
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
}

fn unreachable(why: &str) -> Unreachable {
    Unreachable::new(&std::io::Error::other(why.to_string()))
}

pub struct MemForwarder {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl Forwarder for MemForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let link = self.router.pick(self.from, addr, None);
        let node = self
            .router
            .route_via(self.from, leader, &link)
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
        addr: &str,
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
        let link = self.router.pick(self.from, addr, None);
        let node = self
            .router
            .route_via(self.from, to, &link)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        let frames = node.follow(self.from, since)?;
        // 프레임마다 길을 다시 본다. 두 노드 가운데 하나가 격리되거나 등록이 풀리거나 쓰는 링크가 끊기면 연결이 끊긴
        // 것처럼 끝난다.
        let (router, from, link) = (self.router.clone(), self.from, link.link);
        let held = router.clone();
        Ok(frames
            .then(move |frame| {
                let router = held.clone();
                async move {
                    router.hold_frame(from, &frame).await;
                    frame
                }
            })
            .take_while(move |_| {
                std::future::ready(router.route(from, to).is_ok() && !router.is_cut(from, &link))
            })
            .map(Ok)
            .boxed())
    }
}

#[derive(Clone)]
pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            node: node.clone(),
            router: self.router.clone(),
        }
    }
}

/// Raft RPC 연결. 링크는 `new_client`가 받은 멤버십의 노드 값에서 고른다.
pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    node: OrbNode,
    router: MemRouter,
}

impl MemConnection {
    fn route(&self) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let link = self
            .router
            .pick(self.from, self.node.addr(), Some(&self.node.addrs));
        self.router.raft_route(self.from, self.target, &link)
    }
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let (raft, lost) = self.route()?;
        let result = raft
            .append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let (raft, lost) = self.route()?;
        let result = raft
            .install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let (raft, lost) = self.route()?;
        let result = raft
            .vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }
}
```

`crates/opl-cluster/src/replica.rs`에서

```rust

fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
```

을 다음으로 바꾼다:

```rust

pub(crate) fn read_membership(db: &Database) -> Result<Membership, StoreError> {
    let txn = db.begin_read()?;
```

`crates/opl-cluster/tests/links.rs`를 만든다:

```rust
//! 클러스터 통신 다중 링크(설계 §7): 노드마다 링크 둘(`n{i}-a` 우선, `n{i}-b`)로 멤버십을 만들고, 모든 노드
//! 사이의 우선 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드의 Follow가 둘째 링크로 이어지며, 우선 링크를 풀면
//! 재시도 간격 뒤 다시 우선 링크를 쓴다.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::time::Instant;

const WAIT: Duration = Duration::from_secs(10);

/// 시험의 우선 링크 재시도 간격. 실패하는 시험 호출이 하트비트(openraft는 연결하지 못한 노드에 500 ms 쉬었다
/// 다시 보낸다)와 선거 투표(1~2초마다 한 번)를 거의 모두 먹지 않게 선거 제한 시간보다 길게 둔다.
const RETRY: Duration = Duration::from_millis(2500);

fn id(n: u64) -> NodeId {
    NodeId::from(n)
}

/// 노드 id의 번호
fn num(n: NodeId) -> u64 {
    (1..=4).find(|i| id(*i) == n).unwrap()
}

fn link(n: u64, which: &str) -> String {
    format!("n{n}-{which}")
}

fn links(n: u64) -> OrbNode {
    OrbNode::new([link(n, "a"), link(n, "b")])
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

async fn eventually<F, Fut>(what: &str, cond: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !cond().await {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct Harness {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Cluster>,
}

impl Harness {
    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.link_retry = RETRY;
        let deadline = Instant::now() + Duration::from_secs(5);
        let cluster = loop {
            match Cluster::open_node(
                config.clone(),
                self.router.network(node),
                self.router.follower(node),
            )
            .await
            {
                Err(ClusterError::Store(StoreError::Db(redb::Error::DatabaseAlreadyOpen)))
                    if Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => break other.unwrap(),
            }
        };
        let cluster = cluster.with_forwarder(self.router.forwarder(node));
        self.router.register(node, &cluster);
        self.nodes.insert(node, cluster);
    }

    fn get(&self, i: u64) -> &Cluster {
        &self.nodes[&id(i)]
    }

    /// 스스로 리더라고 보는 투표 멤버
    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|c| {
            let m = c.raft()?.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(c.id())
        })
    }

    fn term(&self, n: NodeId) -> u64 {
        self.nodes[&n]
            .raft()
            .unwrap()
            .metrics()
            .borrow()
            .current_term
    }
}

/// 노드 1~3은 링크 둘로 투표 멤버, 노드 4는 링크 둘로 learner로 더한 뒤 관찰 노드가 된다.
async fn start() -> Harness {
    let mut h = Harness {
        router: MemRouter::default(),
        root: TempDir::new().unwrap(),
        nodes: BTreeMap::new(),
    };
    for i in 1..=4 {
        h.open(i).await;
    }
    let voters = (1..=3).map(|i| (id(i), links(i))).collect();
    h.get(1).initialize(voters).await.unwrap();
    eventually("a leader", || async {
        h.leader().is_some_and(|l| {
            let m = h.nodes[&l].raft().unwrap().metrics().borrow().clone();
            m.last_applied.map(|l| l.index) >= m.last_log_index
        })
    })
    .await;
    let leader = h.leader().unwrap();
    h.nodes[&leader].add_learner(id(4), links(4)).await.unwrap();
    let written = h.nodes[&leader].create(&volume("before")).await.unwrap();
    h.get(4).wait_revision(written, WAIT).await.unwrap();
    // 자기를 learner로 둔 멤버십이 지표에 적용으로 보일 때까지 다시 시도한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = h.get(4).become_observer().await {
        assert!(Instant::now() < deadline, "{e}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.get(4).mode(), NodeMode::Observer);
    h
}

#[tokio::test]
async fn the_cluster_runs_on_the_second_link_and_returns_to_the_first() {
    let mut h = start().await;
    let leader = h.leader().unwrap();
    let term = h.term(leader);
    // 모든 노드가 링크를 알고, 링크 책은 첫 링크로 목록을 찾는다.
    let members = h.get(2).members();
    assert_eq!(members.links[&leader], links(num(leader)).addrs);
    assert_eq!(members.voters[&id(1)], link(1, "a"));
    assert_eq!(
        h.get(4).link_book().links(&link(1, "a")),
        vec![link(1, "a"), link(1, "b")]
    );

    // 모든 노드 사이의 우선 링크를 끊는다.
    for from in 1..=4 {
        for to in 1..=4 {
            h.router.cut_link(id(from), &link(to, "a"));
        }
    }
    // 리더는 그대로이고 팔로워·관찰 노드의 쓰기와 리스 갱신이 둘째 링크로 전달된다.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.leader(), Some(leader));
    assert_eq!(h.term(leader), term);
    let follower = (1..=3).map(id).find(|n| *n != leader).unwrap();
    let written = h.nodes[&follower].create(&volume("cut")).await.unwrap();
    h.nodes[&follower].renew_lease(WAIT).await.unwrap();
    let observer = h.get(4);
    observer.renew_lease(WAIT).await.unwrap();
    observer.create(&volume("from-observer")).await.unwrap();
    // 관찰 노드의 Follow는 우선 링크가 끊겨 닫히고 둘째 링크로 다시 붙는다.
    observer.wait_revision(written, WAIT).await.unwrap();
    let later = h.nodes[&leader].create(&volume("later")).await.unwrap();
    observer.wait_revision(later, WAIT).await.unwrap();
    assert!(
        observer
            .get_by_name::<VolumeSpec>("later")
            .unwrap()
            .is_some()
    );

    // 리더가 죽으면 남은 둘이 둘째 링크로 새 리더를 뽑는다.
    let stopped = h.nodes.remove(&leader).unwrap();
    h.router.unregister(leader);
    stopped.shutdown().await.unwrap();
    eventually("a new leader", || async {
        h.leader().is_some_and(|l| l != leader)
    })
    .await;
    let rest: Vec<NodeId> = (1..=3).map(id).filter(|n| *n != leader).collect();
    h.nodes[&rest[0]]
        .create(&volume("after-leader"))
        .await
        .unwrap();
    h.nodes[&rest[1]]
        .create(&volume("after-leader-2"))
        .await
        .unwrap();

    // 끊긴 동안에도 우선 링크로는 재시도 간격마다 시험 호출 하나씩만 간다.
    let a_links = |h: &Harness| -> u64 {
        rest.iter()
            .map(|n| h.router.sent(&link(num(*n), "a")))
            .sum()
    };
    let b_links = |h: &Harness| -> u64 {
        rest.iter()
            .map(|n| h.router.sent(&link(num(*n), "b")))
            .sum()
    };
    let (a, b) = (a_links(&h), b_links(&h));
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 보내는 쪽과 받는 쪽 짝(남은 둘 사이 둘, 관찰 노드에서 둘)마다 1초에 시험 하나쯤(넉넉히 둘)이다.
    assert!(a_links(&h) - a <= 4 * 2, "{}", a_links(&h) - a);
    assert!(b_links(&h) - b >= 5, "{}", b_links(&h) - b);

    // 우선 링크를 풀면 재시도 간격 뒤 모두 다시 우선 링크를 쓴다.
    for from in 1..=4 {
        h.router.heal(id(from));
    }
    tokio::time::sleep(RETRY * 2).await;
    let (a, b) = (a_links(&h), b_links(&h));
    tokio::time::sleep(Duration::from_secs(1)).await;
    // 리더가 남은 팔로워에 보내는 하트비트만으로도 1초에 여럿 간다.
    assert!(a_links(&h) - a >= 5, "{}", a_links(&h) - a);
    assert_eq!(b_links(&h), b);
    let written = h.nodes[&rest[0]].create(&volume("healed")).await.unwrap();
    h.get(4).wait_revision(written, WAIT).await.unwrap();

    for (_, c) in std::mem::take(&mut h.nodes) {
        c.shutdown().await.unwrap();
    }
}
```

`crates/opl-peer/src/client.rs` 전체:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 관찰 노드의 Follow, 콘솔,
//! 마이그레이션 받기, UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
use opl_cluster::{
    ClusterError, Failure, FollowClient, FollowFrame, Forwarder, LinkBook, LinkSelector, Pick,
};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::metrics::MetricsQuery;
use tokio::runtime::Handle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Status, Streaming};
use uuid::Uuid;

use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::metrics::MetricsReply;
use crate::migration::{
    IncomingError, IncomingMigration, MigrateIncomingRequest, MigrateIncomingResponse,
};
use crate::pki::NodeTls;
use crate::proto::peer_client::PeerClient;
use crate::wire::FollowRequest;
use crate::{
    KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, TARGET_METADATA, VM_METADATA,
};

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 리더 요청 하나를 기다리는 최대 시간. 리더가 AddVoter에서 새 노드를 따라잡게 기다리는 시간
/// (`opl_cluster::CATCH_UP_TIMEOUT`, 60초)에 쓰기 시간을 더한 것보다 길어야 한다.
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// 리스 갱신(`RenewLease`·`RenewVip`) 하나를 기다리는 최대 시간. 갱신하는 쪽이 future를 버리는 시간(HA 리스는
/// `fence_after`가 20초 이상일 때 4초)보다 짧으면 끊긴 링크를 이 제한 시간 초과로 보고, 바깥이 더 짧으면(짧은
/// `fence_after`) 버려진 호출을 가드([`Attempt`])가 초과로 센다.
const LEASE_RPC_TIMEOUT: Duration = Duration::from_secs(3);

/// Follow를 열고 응답 헤더를 받기까지 기다리는 최대 시간
const FOLLOW_OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// 받을 준비(`MigrateIncoming`)를 기다리는 최대 시간. 대상 노드가 VM 상태를 따라잡고(최대 10초),
/// 디스크를 켜고, QEMU를 띄우는 시간이다.
const MIGRATE_INCOMING_TIMEOUT: Duration = Duration::from_secs(120);

/// 받기 확인(`MigrateIncomingWait`)을 기다리는 최대 시간. 대상 노드의 드라이버가 받기를 최대 120초
/// 기다리므로(`incoming_wait`) 그보다 길어야 그 결과를 받는다.
const MIGRATE_INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

/// 받기 취소(`MigrateIncomingAbort`)를 기다리는 최대 시간. 받는 QEMU를 강제로 끄고 디스크를 끈다.
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);

/// UEFI 변수 파일 받기(`UefiVars`)를 기다리는 최대 시간. AAVMF 변수 파일은 64 MiB지만 대부분 0이라
/// 길이만 온다.
const UEFI_VARS_TIMEOUT: Duration = Duration::from_secs(60);

/// 다른 노드로 가는 연결. 주소마다 채널 하나를 만들어 다시 쓴다(끊기면 tonic이 다시 잇는다).
/// 연결할 때 이 노드의 인증서를 내밀고, 상대 인증서는 클러스터 CA로 확인한다.
///
/// 노드는 첫 링크(클러스터 주소)로 가리킨다. 링크가 둘 이상인 노드(`set_link_book`의 책, 책을 붙이기 전의 Raft는
/// 멤버십의 `OrbNode`)는 호출마다 [`LinkSelector`]로 링크를 고르고 그 링크의 채널로 보내며, 결과로 넘어가기를
/// 정한다(설계 §7). 요청이 나가지 않은 호출(연결하지 못함)은 다시 고른 다른 링크로 한 번 더 보낸다. 우선 링크로
/// 보내 보는 시험 호출은
/// control 채널의 짧은 호출만이다. 채널은 링크마다 따로다.
///
/// 실행기를 주면(`with_runtimes`) 주소마다 채널을 둘 둔다. Raft·리더 전달(리스 갱신과 쓰기 전달)·ping·Follow는
/// control 채널, 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk 채널이고, 채널의 연결
/// 작업은 부른 쪽 실행기와 관계없이 그 채널의 실행기에서 돈다. 주지 않으면 채널 하나를 처음 부른
/// 쪽 실행기에서 돌린다.
pub struct Peers {
    tls: ClientTlsConfig,
    channels: Mutex<HashMap<(Lane, String), Channel>>,
    /// control·bulk 채널의 연결 작업을 돌릴 실행기
    runtimes: Option<(Handle, Handle)>,
    /// 노드마다 지금 쓰는 링크
    links: LinkSelector,
    /// 노드마다의 링크 목록. 붙이기 전에는 모든 노드가 링크 하나다(Raft는 멤버십의 링크를 쓴다).
    book: OnceLock<Arc<LinkBook>>,
}

/// 호출 결과 가운데 링크 실패로 셀 것
#[derive(Clone, Copy, PartialEq, Eq)]
enum Counts {
    /// 연결 실패와 제한 시간 초과(짧은 호출)
    All,
    /// 연결 실패만(오래 걸릴 수 있는 호출)
    ConnectOnly,
}

/// 호출 하나가 고른 링크. 결과를 링크 선택에 알린다. 알리지 않고 버려지면(부른 쪽의 제한 시간) 제한 시간 초과로
/// 센다. openraft가 다른 까닭(복제 스트림을 다시 만듦)으로 버린 호출도 초과로 세지만, 넘어가기는 연속 두 번이나
/// 마지막 성공 뒤 400 ms(`LINK_DEAD_AFTER`)를 요구하므로 살아 있는 링크는 대개 버리지 않는다.
struct Attempt<'a> {
    links: &'a LinkSelector,
    addr: String,
    pick: Pick,
    counts: Counts,
    done: bool,
}

impl Attempt<'_> {
    /// 결과를 링크 선택에 알린다.
    fn finish<T>(mut self, result: &Result<T, Status>) {
        self.done = true;
        let failure = match result {
            Ok(_) => None,
            Err(s) => match s.code() {
                // 서버가 내려가는 중이라 답한 것("shutting down"), 이미 보낸 호출이 keepalive 만료로 끝난 것도 여기에
                // 든다. 곧 닿지 않을 노드·연결이다(다시 보낼지는 [`not_sent`]가 따로 본다).
                Code::Unavailable => Some(Failure::Connect),
                // 링크가 다른 노드를 가리킨다(그 노드는 처리하지 않았다).
                Code::PermissionDenied if s.metadata().contains_key(TARGET_METADATA) => {
                    Some(Failure::Connect)
                }
                // tonic은 자기 제한 시간을 `Cancelled`로 돌려준다.
                Code::DeadlineExceeded | Code::Cancelled => Some(Failure::Timeout),
                // 요청 중에 끊긴 연결이거나 처리 중의 오류다. 어느 쪽인지 몰라 세지 않는다.
                Code::Unknown | Code::Internal => return,
                // 상대가 답했다(권한·요청 오류).
                _ => None,
            },
        };
        match failure {
            None => self.links.ok(&self.addr, &self.pick.link, self.pick.sent),
            Some(Failure::Timeout) if self.counts == Counts::ConnectOnly => {}
            Some(failure) => self.links.failed(&self.addr, &self.pick.link, failure),
        }
    }
}

/// 요청이 상대에게 나가지 않았음이 확실한 실패: 연결하지 못함(TCP·TLS, 오류 원인 사슬의 `tonic::ConnectError`)이거나
/// 다른 노드가 처리하지 않고 거절함(대상 id). `Unavailable`이라도 이미 보낸 호출(keepalive ping 만료, 서버의
/// "shutting down")은 상대가 받았을 수 있다. `tonic::transport::Error`는 보낸 뒤의 오류도 감싸므로 보지 않는다.
fn not_sent(s: &Status) -> bool {
    if s.code() == Code::PermissionDenied && s.metadata().contains_key(TARGET_METADATA) {
        return true;
    }
    let mut source = std::error::Error::source(s);
    while let Some(e) = source {
        if e.is::<tonic::ConnectError>() {
            return true;
        }
        source = e.source();
    }
    false
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.done && self.counts == Counts::All {
            self.links
                .failed(&self.addr, &self.pick.link, Failure::Timeout);
        }
    }
}

/// 채널의 쓰임
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Control,
    Bulk,
}

/// tonic 채널의 작업(요청 버퍼, h2 연결)을 정한 실행기에 띄운다.
#[derive(Clone)]
struct OnRuntime(Handle);

impl<F> hyper::rt::Executor<F> for OnRuntime
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        self.0.spawn(fut);
    }
}

impl Peers {
    pub fn new(tls: &NodeTls) -> Self {
        Self {
            tls: ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca_pem))
                .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem)),
            channels: Mutex::new(HashMap::new()),
            runtimes: None,
            links: LinkSelector::default(),
            book: OnceLock::new(),
        }
    }

    /// 노드의 링크 목록을 이 책에서 찾는다(처음 한 번만 붙는다). 클러스터 저장소를 연 뒤 붙인다.
    pub fn set_link_book(&self, book: Arc<LinkBook>) {
        let _ = self.book.set(book);
    }

    /// 우선 링크를 `retry`마다 다시 시도한다(기본 30초). 시험용이다.
    pub fn with_link_retry(mut self, retry: Duration) -> Self {
        self.links = LinkSelector::new(retry);
        self
    }

    /// 첫 링크가 `addr`인 노드에 지금 쓰는 링크(`host:port`)
    pub fn active_link(&self, addr: &str) -> String {
        self.links.active(addr).unwrap_or_else(|| addr.to_string())
    }

    /// 첫 링크가 `addr`인 노드의 링크 목록. 책을 붙이기 전에는 `fallback`이다.
    fn node_links(&self, addr: &str, fallback: impl FnOnce() -> Vec<String>) -> Vec<String> {
        match self.book.get() {
            Some(book) => book.links(addr),
            None => fallback(),
        }
    }

    /// control 채널의 연결 작업은 `control`, bulk 채널의 것은 `bulk` 실행기에서 돌린다.
    pub fn with_runtimes(mut self, control: Handle, bulk: Handle) -> Self {
        self.runtimes = Some((control, bulk));
        self
    }

    /// 호출 하나의 링크. 우선 링크로 보내 보는 시험 호출은 control 채널의 짧은 호출만 한다(큰·오래 걸리는 호출은
    /// 늘 지금 링크).
    fn pick(&self, lane: Lane, addr: &str, links: &[String], counts: Counts) -> Pick {
        if lane == Lane::Control && counts == Counts::All {
            self.links.pick(addr, links)
        } else {
            self.links.current(addr, links)
        }
    }

    /// 고른 링크의 클라이언트와 결과를 알릴 것. 링크로 채널을 만들지 못하면(잘못된 주소) 연결 실패로 센다(시험
    /// 호출이면 시험을 끝낸다).
    fn attempt(
        &self,
        lane: Lane,
        addr: &str,
        pick: Pick,
        counts: Counts,
    ) -> Result<(PeerClient<Channel>, Attempt<'_>), Status> {
        match self.channel(lane, &pick.link) {
            Ok(client) => Ok((
                client,
                Attempt {
                    links: &self.links,
                    addr: addr.to_string(),
                    pick,
                    counts,
                    done: false,
                },
            )),
            Err(e) => {
                self.links.failed(addr, &pick.link, Failure::Connect);
                Err(e)
            }
        }
    }

    /// 첫 링크가 `addr`이고 링크가 `links`인 노드로 `call`을 보낸다. 요청이 나가지 않았으면([`not_sent`], 채널을 만들지
    /// 못함) 다시 고른 다른 링크로 한 번 더 보낸다. 제한 시간 초과와 이미 보낸 호출의 다른 실패는 다시 보내지
    /// 않는다(받았을 수 있다).
    async fn send<R, F, Fut>(
        &self,
        lane: Lane,
        addr: &str,
        links: &[String],
        counts: Counts,
        mut call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let mut pick = self.pick(lane, addr, links, counts);
        let mut again = true;
        loop {
            let link = pick.link.clone();
            let (reply, unsent) = match self.attempt(lane, addr, pick, counts) {
                Ok((client, attempt)) => {
                    let reply = call(client).await;
                    attempt.finish(&reply);
                    let unsent = reply.as_ref().err().is_some_and(not_sent);
                    (reply, unsent)
                }
                Err(e) => (Err(e), true),
            };
            if unsent && std::mem::take(&mut again) {
                let next = self.links.current(addr, links);
                if next.link != link {
                    pick = next;
                    continue;
                }
            }
            return reply;
        }
    }

    /// [`send`](Self::send)를 control 채널로, 링크는 책에서
    async fn send_control<R, F, Fut>(
        &self,
        addr: &str,
        counts: Counts,
        call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.node_links(addr, || vec![addr.to_string()]);
        self.send(Lane::Control, addr, &links, counts, call).await
    }

    /// 오래 걸릴 수 있는 bulk 호출이라 연결 실패만 센다.
    async fn send_bulk<R, F, Fut>(&self, addr: &str, call: F) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.node_links(addr, || vec![addr.to_string()]);
        self.send(Lane::Bulk, addr, &links, Counts::ConnectOnly, call)
            .await
    }

    /// 링크 `addr`로 가는 채널의 클라이언트
    fn channel(&self, lane: Lane, addr: &str) -> Result<PeerClient<Channel>, Status> {
        let (lane, executor) = match &self.runtimes {
            Some((control, bulk)) => match lane {
                Lane::Control => (lane, Some(control.clone())),
                Lane::Bulk => (lane, Some(bulk.clone())),
            },
            None => (Lane::Control, None),
        };
        let mut channels = self.channels.lock().unwrap();
        let channel = match channels.get(&(lane, addr.to_string())) {
            Some(channel) => channel.clone(),
            None => {
                let mut endpoint = Endpoint::from_shared(format!("https://{addr}"))
                    .and_then(|e| e.tls_config(self.tls.clone()))
                    .map_err(|e| Status::invalid_argument(format!("bad address {addr}: {e}")))?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
                    .keep_alive_timeout(KEEPALIVE_TIMEOUT)
                    .keep_alive_while_idle(true)
                    .tcp_keepalive(Some(KEEPALIVE_INTERVAL));
                if let Some(handle) = executor {
                    endpoint = endpoint.executor(OnRuntime(handle));
                }
                let channel = endpoint.connect_lazy();
                channels.insert((lane, addr.to_string()), channel.clone());
                channel
            }
        };
        Ok(PeerClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES))
    }

    /// 노드 `to`(첫 링크 `addr`)가 응답하면 그 노드의 id를 돌려준다. 링크가 다른 노드를 가리키면 그 노드가 거절해
    /// 다음 링크로 다시 보낸다.
    pub async fn ping(&self, to: NodeId, addr: &str, timeout: Duration) -> Result<NodeId, Status> {
        let reply = self
            .send_control(addr, Counts::All, |mut client| {
                let mut req = Request::new(());
                req.set_timeout(timeout);
                addressed(&mut req, Some(to));
                async move { within(timeout, client.ping(req)).await }
            })
            .await;
        Ok(reply?.into_inner())
    }

    /// `addr`의 노드에 리더 요청을 보낸다. 그 노드가 리더가 아니면 `NotLeader`다. 리스 갱신은 짧게 기다리고
    /// 제한 시간 초과도 링크 실패로 센다. 다른 요청(쓰기, 따라잡기를 기다리는 `AddVoter`)은 연결 실패만 센다.
    pub async fn leader_request(
        &self,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request_to(addr, None, req).await
    }

    /// [`leader_request`](Self::leader_request)와 같고, 리더의 id를 알면(`leader`) 실어 보내 다른 노드를 가리키는
    /// 링크면 다른 링크로 보낸다.
    async fn leader_request_to(
        &self,
        addr: &str,
        leader: Option<NodeId>,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let (timeout, counts) = match req {
            LeaderRequest::RenewLease { .. } | LeaderRequest::RenewVip { .. } => {
                (LEASE_RPC_TIMEOUT, Counts::All)
            }
            _ => (LEADER_REQUEST_TIMEOUT, Counts::ConnectOnly),
        };
        // 서버는 처리 결과를 늘 응답 본문에 담아 보내므로, `Status`는 전송 문제다. 새로 연결하지
        // 못하면 `Unavailable`이지만 요청 중에 끊기면 `Cancelled`, `Unknown`, `Internal` 등이 오므로
        // 권한·요청 오류가 아니면 모두 `Unreachable`로 돌려 다시 시도하게 한다.
        let reply = self
            .send_control(addr, counts, |mut client| {
                let mut req = Request::new(req.clone());
                req.set_timeout(timeout);
                addressed(&mut req, leader);
                async move { within(timeout, client.leader(req)).await }
            })
            .await;
        let reply = reply.map_err(|s| match s.code() {
            Code::PermissionDenied | Code::Unauthenticated | Code::InvalidArgument => {
                ClusterError::Raft(s.to_string())
            }
            _ => ClusterError::Unreachable(s.message().to_string()),
        })?;
        reply.into_inner().map_err(ClusterError::from)
    }

    /// `addr`의 노드에서 도는 VM의 콘솔(VNC 바이트 스트림)을 연다.
    pub async fn console(&self, addr: &str, vm: Uuid) -> Result<ConsoleStream, ConsoleError> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut req = Request::new(ReceiverStream::new(rx));
        let value = MetadataValue::try_from(vm.to_string())
            .map_err(|e| ConsoleError::Unavailable(e.to_string()))?;
        req.metadata_mut().insert(VM_METADATA, value);
        let unavailable = |s: Status| match s.code() {
            Code::FailedPrecondition => ConsoleError::NotRunning,
            _ => ConsoleError::Unavailable(s.message().to_string()),
        };
        // 요청 스트림은 한 번만 보낼 수 있어 다른 링크로 다시 보내지 않는다.
        let links = self.node_links(addr, || vec![addr.to_string()]);
        let pick = self.pick(Lane::Bulk, addr, &links, Counts::ConnectOnly);
        let (mut client, attempt) = self
            .attempt(Lane::Bulk, addr, pick, Counts::ConnectOnly)
            .map_err(unavailable)?;
        let reply = client.console(req).await;
        attempt.finish(&reply);
        let inbound = reply.map_err(unavailable)?.into_inner();
        Ok(ConsoleStream {
            sender: ConsoleSender { tx },
            receiver: ConsoleReceiver { inbound },
        })
    }
}

impl Peers {
    /// `addr`의 노드에 VM을 받을 준비를 시킨다. 받는 QEMU가 연 포트(디스크를 복사하면 NBD 포트도)를
    /// 돌려준다.
    pub async fn migrate_incoming(
        &self,
        addr: &str,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(req.clone());
                req.set_timeout(MIGRATE_INCOMING_TIMEOUT);
                async move { client.migrate_incoming(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }

    /// `addr`의 노드에서 받는 QEMU가 받기를 마칠 때까지 기다린다.
    pub async fn migrate_incoming_wait(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(migration);
                req.set_timeout(MIGRATE_INCOMING_WAIT_TIMEOUT);
                async move { client.migrate_incoming_wait(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }

    /// `addr`의 노드에 확정 전의 마이그레이션을 버리게 한다(받는 QEMU를 끄고 디스크를 끈다).
    pub async fn migrate_incoming_abort(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(migration);
                req.set_timeout(MIGRATE_INCOMING_ABORT_TIMEOUT);
                async move { client.migrate_incoming_abort(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에
    /// 받아 fsync한 뒤 이름을 바꾼다). 받은 크기가 `expect_len`과 다르거나 실패하면 `dest`는 그대로다.
    /// 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(
        &self,
        from: NodeId,
        addr: &str,
        vm: Uuid,
        dest: &Path,
        expect_len: Option<u64>,
    ) -> Result<u64, UefiVarsError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(UefiVarsRequest { vm });
                req.set_timeout(UEFI_VARS_TIMEOUT);
                addressed(&mut req, Some(from));
                async move { client.uefi_vars(req).await }
            })
            .await;
        let chunks = reply?.into_inner();
        crate::firmware::write_file(chunks, dest, expect_len).await
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)의 감사 로그에서 `query`에 맞는 항목을 읽는다(새 것부터, 최대 `limit + 1`개).
    /// 그 노드가 감사 로그를 껐으면 None이다.
    pub async fn read_audit(
        &self,
        from: NodeId,
        addr: &str,
        query: AuditQuery,
        timeout: Duration,
    ) -> Result<Option<Vec<AuditEntry>>, Status> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(query.clone());
                req.set_timeout(timeout);
                addressed(&mut req, Some(from));
                async move { client.read_audit(req).await }
            })
            .await;
        match reply {
            Ok(reply) => Ok(Some(reply.into_inner().entries)),
            Err(s) if s.code() == Code::FailedPrecondition => Ok(None),
            Err(s) => Err(s),
        }
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)의 메모리에 있는 지표를 읽는다. 그 노드가 지표를 모르면(옛 노드, 지표 소스가
    /// 없는 노드) None이다.
    pub async fn read_metrics(
        &self,
        from: NodeId,
        addr: &str,
        query: MetricsQuery,
        timeout: Duration,
    ) -> Result<Option<MetricsReply>, Status> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(query.clone());
                req.set_timeout(timeout);
                addressed(&mut req, Some(from));
                async move { client.read_metrics(req).await }
            })
            .await;
        match reply {
            Ok(reply) => Ok(Some(reply.into_inner())),
            Err(s) if s.code() == Code::Unimplemented => Ok(None),
            Err(s) => Err(s),
        }
    }
}

/// `fut`을 `timeout`까지만 기다린다. gRPC 제한 시간은 요청을 보낸 뒤부터 재고 tonic의 연결 제한 시간은 TCP
/// 연결까지라, 받고 답하지 않는 상대(TLS 핸드셰이크에서 멈춤)에 보낸 호출은 이것이 끝낸다.
async fn within<T>(
    timeout: Duration,
    fut: impl Future<Output = Result<T, Status>>,
) -> Result<T, Status> {
    tokio::time::timeout(timeout, fut)
        .await
        .unwrap_or_else(|_| Err(Status::deadline_exceeded("timed out")))
}

/// 가려는 노드의 id(`target`)를 요청에 싣는다(받는 노드가 다른 노드면 거절한다).
fn addressed<T>(req: &mut Request<T>, target: Option<NodeId>) {
    if let Some(value) = target.and_then(|t| MetadataValue::try_from(t.0.to_string()).ok()) {
        req.metadata_mut().insert(TARGET_METADATA, value);
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
fn unreachable(s: Status) -> IncomingError {
    IncomingError::Unavailable(s.message().to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    #[error("the VM is not running on that node")]
    NotRunning,
    #[error("cannot open the console: {0}")]
    Unavailable(String),
}

/// 다른 노드의 VM 콘솔 연결
pub struct ConsoleStream {
    sender: ConsoleSender,
    receiver: ConsoleReceiver,
}

impl ConsoleStream {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.sender.send(data).await
    }

    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.receiver.recv().await
    }

    /// 보내는 쪽과 받는 쪽으로 나눈다(두 방향을 따로 돌릴 때).
    pub fn into_split(self) -> (ConsoleSender, ConsoleReceiver) {
        (self.sender, self.receiver)
    }
}

/// 콘솔 연결의 보내는 쪽
pub struct ConsoleSender {
    tx: tokio::sync::mpsc::Sender<Bytes>,
}

impl ConsoleSender {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(data).await.is_ok()
    }
}

/// 콘솔 연결의 받는 쪽
pub struct ConsoleReceiver {
    inbound: Streaming<Bytes>,
}

impl ConsoleReceiver {
    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.inbound.message().await.ok().flatten()
    }
}

/// 관찰 노드가 투표 멤버에 거는 Follow(control 채널). 여는 데(응답 헤더까지) 3초를 넘기면 실패이고, 연 뒤에는
/// 제한 시간 없이 이어진다. 끊긴 연결은 keepalive가, 멈춘 스트림은 복제본이(진행 3초 없음) 알아챈다.
#[async_trait]
impl FollowClient for Peers {
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        let unreachable = |s: Status| ClusterError::Unreachable(s.message().to_string());
        let reply = self
            .send_control(addr, Counts::All, |mut client| {
                let mut req = Request::new(FollowRequest { since });
                addressed(&mut req, Some(to));
                async move {
                    tokio::time::timeout(FOLLOW_OPEN_TIMEOUT, client.follow(req))
                        .await
                        .unwrap_or_else(|_| {
                            Err(Status::deadline_exceeded(format!(
                                "the follow stream did not open within {FOLLOW_OPEN_TIMEOUT:?}"
                            )))
                        })
                }
            })
            .await;
        let frames = reply.map_err(unreachable)?.into_inner();
        Ok(frames.map(move |f| f.map_err(unreachable)).boxed())
    }
}

#[async_trait]
impl Forwarder for Peers {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request_to(addr, Some(leader), req).await
    }
}

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `OrbNode::addr()`(첫 링크)는 노드의 클러스터 주소이고,
/// 링크는 링크 책에서, 책을 붙이기 전에는 멤버십의 `OrbNode.addrs`에서 고른다(다른 호출과 같은 목록이어야 링크
/// 선택이 처음부터 다시 세지 않는다). openraft가 제한 시간(`hard_ttl`)에 버린 RPC는 링크의 제한 시간 초과로 센다.
#[derive(Clone)]
pub struct GrpcNetwork {
    peers: Arc<Peers>,
}

impl GrpcNetwork {
    pub fn new(peers: Arc<Peers>) -> Self {
        Self { peers }
    }
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetwork {
    type Network = GrpcConnection;

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr().to_string(),
            links: node.addrs.clone(),
            peers: self.peers.clone(),
        }
    }
}

pub struct GrpcConnection {
    target: NodeId,
    addr: String,
    links: Vec<String>,
    peers: Arc<Peers>,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, OrbNode, RaftError<NodeId, E>> {
    if matches!(
        s.code(),
        Code::Unavailable | Code::PermissionDenied | Code::Unauthenticated
    ) {
        RPCError::Unreachable(Unreachable::new(&s))
    } else {
        RPCError::Network(NetworkError::new(&s))
    }
}

impl GrpcConnection {
    fn request<T>(&self, message: T, option: &RPCOption) -> Request<T> {
        let mut req = Request::new(message);
        req.set_timeout(option.hard_ttl());
        addressed(&mut req, Some(self.target));
        req
    }

    /// 대상 노드의 링크 목록
    fn links(&self) -> Vec<String> {
        self.peers.node_links(&self.addr, || self.links.clone())
    }

    /// `rpc`를 보낸다. 연결하지 못하면 다른 링크로 다시 보내므로, 링크가 둘 이상이면 보낼 때마다 복사한다.
    async fn send<T: Clone, R, F, Fut>(
        &self,
        rpc: T,
        option: &RPCOption,
        mut call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>, Request<T>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.links();
        let again = links.len() > 1;
        let mut rpc = Some(rpc);
        self.peers
            .send(Lane::Control, &self.addr, &links, Counts::All, |client| {
                let message = if again { rpc.clone() } else { rpc.take() };
                let message = message.expect("a single link is tried once");
                call(client, self.request(message, option))
            })
            .await
    }
}

impl RaftNetwork<TypeConfig> for GrpcConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.append_entries(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.install_snapshot(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.vote(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

`crates/opl-peer/src/lib.rs`에서

```rust

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
```

을 다음으로 바꾼다:

```rust

/// 호출이 가려는 노드의 id를 담는 메타데이터 키. 받는 노드는 이것이 자기 id가 아니면(다른 노드를 가리키는 링크)
/// 처리하지 않고 `permission_denied`로 답하며 같은 키에 자기 id를 싣는다. 보내는 쪽은 그것을 링크 실패로 센다.
pub const TARGET_METADATA: &str = "orbivisor-target";

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
```

`crates/opl-peer/src/monitor.rs`에서

```rust
                    peers
                        .ping(&addr, timeout)
                        .await
```

을 다음으로 바꾼다:

```rust
                    peers
                        .ping(target, &addr, timeout)
                        .await
```

`crates/opl-peer/src/server.rs`에서

```rust
};
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};
```

을 다음으로 바꾼다:

```rust
};
use crate::{
    KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, TARGET_METADATA, VM_METADATA,
};
```

`crates/opl-peer/src/server.rs`에서

```rust

    /// 부른 노드의 id. 클러스터에서 빠진 노드는 `permission_denied`다.
    fn authorize<T>(&self, req: &Request<T>) -> Result<NodeId, Status> {
```

을 다음으로 바꾼다:

```rust

    /// 부른 노드의 id. 클러스터에서 빠진 노드는 `permission_denied`다. 다른 노드에 보낸 호출(대상 id
    /// [`TARGET_METADATA`]가 이 노드가 아님)도 `permission_denied`이고 이 노드의 id를 실어 답한다(옛 노드는 대상을
    /// 싣지 않으므로 받는다).
    fn authorize<T>(&self, req: &Request<T>) -> Result<NodeId, Status> {
```

`crates/opl-peer/src/server.rs`에서

```rust
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        let members = self.cluster.members();
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        if let Some(target) = req.metadata().get(TARGET_METADATA) {
            let me = self.cluster.id();
            if target.to_str().ok().and_then(|t| Uuid::try_parse(t).ok()) != Some(me.0) {
                let mut status = Status::permission_denied(format!(
                    "this is node {me}, not {target:?}; a cluster link points to the wrong node"
                ));
                if let Ok(value) = me.0.to_string().parse() {
                    status.metadata_mut().insert(TARGET_METADATA, value);
                }
                return Err(status);
            }
        }
        let members = self.cluster.members();
```

`crates/opl-peer/tests/grpc.rs` 전체:

```rust
//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::VoteRequest;
use openraft::{ServerState, Vote};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, FollowClient, FollowFrame, Forwarder, NodeRole};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, api_cert, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingDisk, IncomingError, IncomingMigration, LEASE_LIVENESS,
    MetricsReply, MetricsSource, MigrateIncomingRequest, MigrateIncomingResponse,
    MigrationReceiver, NodeMonitor, NodeMonitorConfig, PeerService, Peers, UefiVarsError,
    renew_liveness,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
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

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None, None, None, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받고, `audit`이 있으면 감사 로그를, `metrics`가
    /// 있으면 지표를 그것으로 읽는다. `work`가 있으면 Raft·리스가 아닌 처리를 그 실행기에서 돌린다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
        audit: Option<Arc<dyn AuditSource>>,
        metrics: Option<Arc<dyn MetricsSource>>,
        work: Option<tokio::runtime::Handle>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        if let Some(audit) = audit {
            service = service.with_audit(audit);
        }
        if let Some(metrics) = metrics {
            service = service.with_metrics(metrics);
        }
        if let Some(work) = work {
            service = service.with_work(work);
        }
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
            state_dir,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        // opld처럼 노드 간 서버가 끝난 뒤 Raft를 멈춘다. 먼저 멈추면 서버가 아직 받는 전달 요청에
        // 다시 시도하지 않는 "raft stopped"로 답한다.
        let _ = self.stop.take().unwrap().send(());
        self.server.take().unwrap().await.unwrap();
        self.cluster.shutdown().await.unwrap();
    }
}

/// 받은 요청을 적고 정해 둔 결과를 돌려주는 받는 쪽
#[derive(Default)]
struct FakeReceiver {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MigrationReceiver for FakeReceiver {
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {} {:?}",
            req.vm, req.task, req.qemu_version, req.disks
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        // 복사할 디스크가 있으면 NBD 포트도 연다.
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: (!req.disks.is_empty()).then_some(60008),
        })
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wait {} {}", req.vm, req.task));
        Err(IncomingError::Failed("the receiving QEMU exited".into()))
    }

    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("abort {} {}", req.vm, req.task));
        Ok(())
    }
}

/// 받은 조건을 적고 항목 하나를 돌려주는 감사 로그
#[derive(Default)]
struct FakeAudit {
    queries: std::sync::Mutex<Vec<AuditQuery>>,
}

#[async_trait::async_trait]
impl AuditSource for FakeAudit {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        if query.user.as_deref() == Some("broken") {
            return Err("cannot read the audit log".into());
        }
        self.queries.lock().unwrap().push(query);
        Ok(vec![AuditEntry {
            id: Uuid::now_v7(),
            time: 1_000,
            node: "n2".into(),
            user: Some("alice".into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: "POST /vms".into(),
            target: None,
            status: 201,
            duration_ms: 2,
            detail: None,
        }])
    }
}

/// 받은 조건을 적고 `since`보다 뒤의 점을 돌려주는 지표(노드 점은 `t` 1000·2000)
#[derive(Default)]
struct FakeMetrics {
    queries: std::sync::Mutex<Vec<MetricsQuery>>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        self.queries.lock().unwrap().push(q.clone());
        let samples = [1_000, 2_000]
            .into_iter()
            .filter(|t| q.since.is_none_or(|since| *t > since))
            .map(|t| NodeSample {
                t,
                cpu: Some(12.5),
                mem_used_mib: Some(100),
                mem_total_mib: Some(200),
                disk_read_bps: None,
                disk_write_bps: None,
                net_rx_bps: None,
                net_tx_bps: None,
                fs_used_mib: None,
                fs_total_mib: None,
            })
            .collect();
        MetricsReply {
            node: Some(NodeSeries {
                node: Uuid::nil(),
                samples,
            }),
            vms: vec![],
        }
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), OrbNode::single(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().unwrap().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().unwrap().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(
        stranger
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .is_err()
    );
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .ping(nodes[0].id(), &nodes[0].addr, WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0]
            .peers
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .unwrap(),
        nodes[0].id()
    );
    // 멤버의 API 인증서(서버 인증만, 노드 id 없음)로는 피어가 되지 못한다.
    let member = tls_for(&ca, nodes[0].id());
    let (api_pem, api_key) = api_cert(&ca, &member.cert_pem, &[]).unwrap();
    let api = Peers::new(&NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: api_pem,
        key_pem: api_key,
    });
    assert!(api.ping(nodes[0].id(), &nodes[0].addr, WAIT).await.is_err());
}

/// 시험이 실패해도 따로 만든 실행기를 끝낸다(async 문맥에서 그냥 놓으면 panic한다).
struct Background(Option<tokio::runtime::Runtime>);

impl Background {
    fn new() -> Self {
        Self::of(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        )
    }

    fn of(rt: tokio::runtime::Runtime) -> Self {
        Self(Some(rt))
    }

    fn handle(&self) -> &tokio::runtime::Handle {
        self.0.as_ref().unwrap().handle()
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            rt.shutdown_background();
        }
    }
}

/// 실행기를 받은 `Peers`의 control 채널은 처음 연 실행기가 끝나도 된다(채널의 연결 작업이 받은
/// control 실행기에서 돈다). 실행기를 주지 않으면 처음 연 실행기와 함께 채널이 멈춘다.
#[tokio::test]
async fn control_channels_outlive_the_runtime_that_opened_them() {
    let (ca, _root, nodes) = start(1).await;
    let control = tokio::runtime::Handle::current();
    let bulk = Background::new();
    let peers = Arc::new(
        Peers::new(&tls_for(&ca, nodes[0].id())).with_runtimes(control, bulk.handle().clone()),
    );
    let addr = nodes[0].addr.clone();

    // 따로 만든 실행기(work·api에 해당)가 먼저 채널을 열고 끝난다.
    let first = Background::new();
    let opened = {
        let (peers, addr, id) = (peers.clone(), addr.clone(), nodes[0].id());
        first
            .handle()
            .spawn(async move { peers.ping(id, &addr, WAIT).await })
    };
    assert_eq!(opened.await.unwrap().unwrap(), nodes[0].id());
    drop(first);

    assert_eq!(
        peers.ping(nodes[0].id(), &addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .unwrap()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}

#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn new_leader_does_not_revive_nodes_recorded_offline() {
    let (_ca, _root, mut nodes) = start(5).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let online = |cluster: &Cluster, id: NodeId| {
        cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };

    // 팔로워 하나를 멈추고 리더가 오프라인으로 기록할 때까지 기다린다.
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let mut dead = nodes.remove(follower);
    let dead_id = dead.id();
    dead.stop().await;
    let old = nodes.iter().position(|n| n.id() == leader).unwrap();
    let deadline = Instant::now() + WAIT;
    while online(&nodes[old].cluster, dead_id) {
        assert!(
            Instant::now() < deadline,
            "dead follower never went offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 리더도 멈추면 새 리더가 그 기록을 다시 온라인으로 바꾸지 않아야 한다.
    let mut old = nodes.remove(old);
    old.stop().await;
    let new_leader = wait_leader(&nodes).await;
    let new_leader = nodes.iter().find(|n| n.id() == new_leader).unwrap();
    // 새 리더가 이전 커밋을 모두 적용한 뒤부터 본다. openraft는 리더 확인 RPC를 하트비트 간격
    // (50ms)만 기다리고, 노드 둘이 죽어 남는 여유가 없어 CPU가 바쁘면 한 번에 실패할 수 있다.
    // 운영 코드(노드 감시, 예약 작업)처럼 다시 한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = new_leader.cluster.read_barrier().await {
        assert!(
            Instant::now() < deadline,
            "read barrier never succeeded: {e}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 감시 간격보다 길고 `offline_after`보다 짧게 지켜본다.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !online(&new_leader.cluster, dead_id),
            "new leader revived a node recorded offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for m in monitors {
        m.shutdown().await;
    }
}

/// 노드 리소스를 기록한다. `lease`면 그 노드가 [`LEASE_LIVENESS`]를 안다고 적는다.
async fn register_nodes(
    nodes: &[Node],
    online: impl Fn(NodeId) -> bool,
    lease: impl Fn(usize) -> bool,
) {
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = node.id().0;
        r.status.online = online(node.id());
        if lease(i) {
            r.status.features = vec![LEASE_LIVENESS.into()];
        }
        nodes[0].cluster.create(&r).await.unwrap();
    }
}

fn recorded_online(cluster: &Cluster, id: NodeId) -> bool {
    cluster
        .get::<NodeSpec>(id.0)
        .unwrap()
        .unwrap()
        .status
        .online
}

async fn wait_online(cluster: &Cluster, id: NodeId, want: bool) {
    let deadline = Instant::now() + WAIT;
    while recorded_online(cluster, id) != want {
        assert!(
            Instant::now() < deadline,
            "node {id} never became online={want}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const LIVENESS_RENEW: Duration = Duration::from_millis(200);

/// 모든 노드가 리스로 생존을 알리면 리더는 ping이 아니라 리스를 본다: 핑에 답하는 노드도 갱신을 멈추면
/// 오프라인, 다시 갱신하면 온라인이다. 펜싱 없이 생존 표시로만 갱신하는 노드도 온라인으로 남는다.
#[tokio::test]
async fn with_lease_liveness_the_leader_follows_renewals_not_pings() {
    let (_ca, _root, nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    // 팔로워 하나는 오프라인으로 시작해, 갱신하면 온라인이 되는지 본다.
    register_nodes(&nodes, |id| id == leader, |_| true).await;
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let mut renewers: Vec<_> = nodes
        .iter()
        .map(|n| renew_liveness(n.cluster.clone(), LIVENESS_RENEW))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    let other = nodes
        .iter()
        .position(|n| n.id() != leader && n.id() != follower_id)
        .unwrap();
    wait_online(&leader_cluster, follower_id, true).await;

    // 노드는 그대로 돌고(핑에 답한다) 갱신만 멈춘다.
    renewers[follower].abort();
    wait_online(&leader_cluster, follower_id, false).await;
    assert!(recorded_online(&leader_cluster, nodes[other].id()));
    renewers[follower] = renew_liveness(nodes[follower].cluster.clone(), LIVENESS_RENEW);
    wait_online(&leader_cluster, follower_id, true).await;

    // 정상 종료처럼 갱신을 멈추고 스스로 오프라인으로 기록하면, 리스가 아직 살아 있어도 다시 온라인이
    // 되지 않는다.
    renewers[follower].abort();
    let mut status = nodes[follower]
        .cluster
        .get::<NodeSpec>(follower_id.0)
        .unwrap()
        .unwrap()
        .status;
    status.online = false;
    nodes[follower]
        .cluster
        .update_status::<NodeSpec>(follower_id.0, &status)
        .await
        .unwrap();
    wait_online(&leader_cluster, follower_id, false).await;
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !recorded_online(&leader_cluster, follower_id),
            "a node that marked itself offline came back on an old renewal"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    renewers[follower] = renew_liveness(nodes[follower].cluster.clone(), LIVENESS_RENEW);
    wait_online(&leader_cluster, follower_id, true).await;
    assert!(recorded_online(&leader_cluster, leader));

    for r in renewers {
        r.abort();
    }
    for m in monitors {
        m.shutdown().await;
    }
}

/// 리스 생존을 모르는 옛 노드가 남았으면 지금처럼 ping으로 본다: 아무도 갱신하지 않아도 핑에 답하는
/// 노드는 온라인으로 남고, 멈춘 노드는 오프라인이 된다.
#[tokio::test]
async fn a_cluster_with_an_old_node_keeps_pinging() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let old = nodes.iter().position(|n| n.id() != leader).unwrap();
    register_nodes(&nodes, |_| true, |i| i != old).await;
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    // `offline_after`보다 오래 지켜본다(리스로 보면 갱신이 없어 오프라인이 된다).
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until {
        for n in &nodes {
            assert!(
                recorded_online(&leader_cluster, n.id()),
                "node {} went offline without a lease in a mixed cluster",
                n.id()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stopped = nodes.iter().position(|n| n.id() != leader).unwrap();
    let stopped_id = nodes[stopped].id();
    nodes[stopped].stop().await;
    wait_online(&leader_cluster, stopped_id, false).await;
    for m in monitors {
        m.shutdown().await;
    }
}

/// 멤버가 아닌 노드의 남은 기록(클러스터에서 빠진 옛 노드)은 리스 생존을 몰라도 리스 감시를 막지 않는다: 핑에
/// 답하는 노드도 갱신을 멈추면 오프라인이 된다.
#[tokio::test]
async fn a_leftover_record_of_a_removed_node_does_not_keep_pinging() {
    let (_ca, _root, nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    register_nodes(&nodes, |_| true, |_| true).await;
    let gone: Resource<NodeSpec, NodeStatus> = Resource::new(
        "gone",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
            roles: None,
        },
    );
    nodes[0].cluster.create(&gone).await.unwrap();
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let renewers: Vec<_> = nodes
        .iter()
        .map(|n| renew_liveness(n.cluster.clone(), LIVENESS_RENEW))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    // 노드는 그대로 돌고(핑에 답한다) 갱신만 멈춘다. ping으로 보면 온라인으로 남는다.
    renewers[follower].abort();
    wait_online(&leader_cluster, nodes[follower].id(), false).await;
    for r in renewers {
        r.abort();
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone()), None, None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (vm, task) = (Uuid::now_v7(), Uuid::now_v7());
    let peers = &nodes[0].peers;
    let req = |version: &str| MigrateIncomingRequest {
        vm,
        task,
        qemu_version: version.into(),
        disks: vec![],
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        MigrateIncomingResponse {
            port: 60007,
            nbd_port: None
        }
    );
    // 복사할 디스크는 요청에 실려 가고 NBD 포트가 돌아온다.
    let disk = IncomingDisk {
        volume: Uuid::now_v7(),
        size_bytes: 1 << 30,
    };
    let reply = peers
        .migrate_incoming(
            &target.addr,
            MigrateIncomingRequest {
                disks: vec![disk],
                ..req("8.2.2")
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.nbd_port, Some(60008));
    // 받는 쪽의 거절과 실패는 그 종류 그대로 온다.
    let err = peers
        .migrate_incoming(&target.addr, req("9.0.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Incompatible(_)), "{err:?}");
    let migration = IncomingMigration { vm, task };
    let err = peers
        .migrate_incoming_wait(&target.addr, migration)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed("the receiving QEMU exited".into())
    );
    peers
        .migrate_incoming_abort(&target.addr, migration)
        .await
        .unwrap();
    assert_eq!(
        *receiver.calls.lock().unwrap(),
        [
            format!("incoming {vm} {task} 8.2.2 []"),
            format!("incoming {vm} {task} 8.2.2 {:?}", [disk]),
            format!("incoming {vm} {task} 9.0.0 []"),
            format!("wait {vm} {task}"),
            format!("abort {vm} {task}"),
        ]
    );

    // 받는 처리가 없는 노드는 거부한다.
    let err = target
        .peers
        .migrate_incoming_abort(&nodes[0].addr, migration)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("does not receive migrations")),
        "{err:?}"
    );
    // 멤버가 아닌 노드의 요청은 받는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .migrate_incoming(&target.addr, req("8.2.2"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert_eq!(receiver.calls.lock().unwrap().len(), 5);
}

/// 받는 처리가 돈 스레드와, 그 처리가 띄운 작업(받은 VM의 QMP·모니터에 해당)이 돈 스레드를 적는다.
#[derive(Default)]
struct ThreadReceiver {
    threads: std::sync::Mutex<Vec<String>>,
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or_default()
        .to_string()
}

#[async_trait::async_trait]
impl MigrationReceiver for ThreadReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let spawned = tokio::spawn(async { thread_name() }).await.unwrap();
        self.threads
            .lock()
            .unwrap()
            .extend([thread_name(), spawned]);
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: None,
        })
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기를 받은 노드 간 서버는 마이그레이션 받기를 그 실행기에서 돌린다. 받는 처리가 띄운
/// 작업도 그 실행기에 생긴다(노드 간 서버의 실행기에는 Raft·리스만 남는다).
#[tokio::test]
async fn incoming_migrations_run_on_the_work_runtime() {
    let work = Background::of(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("peer-work")
            .enable_all()
            .build()
            .unwrap(),
    );
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(ThreadReceiver::default());
    let target = Node::start_with(
        &ca,
        &root,
        Some(receiver.clone()),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(nodes[0].id()) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    nodes[0]
        .peers
        .migrate_incoming(&target.addr, req)
        .await
        .unwrap();
    assert_eq!(
        *receiver.threads.lock().unwrap(),
        ["peer-work", "peer-work"]
    );
}

/// 받는 처리가 panic한다.
struct PanickingReceiver;

#[async_trait::async_trait]
impl MigrationReceiver for PanickingReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        panic!("receiver bug");
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기에서 돈 처리가 panic하면 그 요청만 `internal`로 답하고 서버는 계속 답한다.
#[tokio::test]
async fn a_panicking_handler_answers_internal() {
    let work = Background::new();
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let node = Node::start_with(
        &ca,
        &root,
        Some(Arc::new(PanickingReceiver)),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    let members = BTreeMap::from([(node.id(), OrbNode::single(&node.addr))]);
    node.cluster.initialize(members).await.unwrap();
    let nodes = [node];
    wait_leader(&nodes).await;

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    let err = nodes[0]
        .peers
        .migrate_incoming(&nodes[0].addr, req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("panicked")),
        "{err:?}"
    );
    assert_eq!(
        nodes[0]
            .peers
            .ping(nodes[0].id(), &nodes[0].addr, WAIT)
            .await
            .unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_a_member() {
    let (ca, root, nodes) = start(2).await;
    let (from, to) = (&nodes[1], &nodes[0]);
    let vm = Uuid::now_v7();
    // 데이터 조각, 0만 있는 조각 둘, 끝이 짧은 조각(마지막 바이트만 0이 아님)
    let chunk = opl_peer::firmware::CHUNK_BYTES;
    let mut vars: Vec<u8> = (0..chunk).map(|i| (i % 251) as u8 + 1).collect();
    vars.resize(3 * chunk + 100, 0);
    *vars.last_mut().unwrap() = 0xaa;
    let src = uefi_vars_path(&from.state_dir, vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &vars).unwrap();

    // 받는 노드의 옛 파일을 바꾼다.
    let dest = uefi_vars_path(&to.state_dir, vm);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(from.id(), &from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(from.id(), &from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(from.id(), &from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(from.id(), &from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(
            from.id(),
            &from.addr,
            vm,
            &root.path().join("outsider.fd"),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}

#[tokio::test]
async fn audit_logs_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let audit = Arc::new(FakeAudit::default());
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |limit, user: Option<&str>| AuditQuery {
        since: 10,
        until: 20_000,
        user: user.map(Into::into),
        action: Some("/vms".into()),
        limit,
    };
    let peers = &nodes[0].peers;
    let entries = peers
        .read_audit(target.id(), &target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(target.id(), &target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(target.id(), &target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(nodes[0].id(), &nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(target.id(), &target.addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(audit.queries.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn metrics_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let metrics = Arc::new(FakeMetrics::default());
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |since| MetricsQuery {
        scope: MetricsScope::Nodes,
        since,
    };
    let peers = &nodes[0].peers;
    let reply = peers
        .read_metrics(target.id(), &target.addr, query(Some(1_000)), WAIT)
        .await
        .unwrap()
        .unwrap();
    let samples = reply.node.unwrap().samples;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].t, 2_000);
    assert_eq!(samples[0].cpu, Some(12.5));
    assert_eq!(*metrics.queries.lock().unwrap(), [query(Some(1_000))]);

    // 지표 소스가 없는 노드(옛 노드처럼)는 None이다.
    let none = target
        .peers
        .read_metrics(nodes[0].id(), &nodes[0].addr, query(None), WAIT)
        .await
        .unwrap();
    assert_eq!(none, None);
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_metrics(target.id(), &target.addr, query(None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
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
    // 목록에 적은 뒤 멤버십에서 뺀다(그 사이에도 거절되지 않게). 리더의 키퍼처럼 `RemoveNodes`로 뺀다
    // (`LeaderRequest::Remove`는 노드를 지우는 것이라 목록에서도 뺀다).
    set_observers(leader, &[(id, addr.clone())]).await;
    leader
        .cluster
        .raft()
        .unwrap()
        .change_membership(openraft::ChangeMembers::RemoveNodes([id].into()), false)
        .await
        .unwrap();

    let peers = Arc::new(Peers::new(&tls));
    let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
    config.write_timeout = WAIT;
    // learner의 저장소 작업이 아직 파일을 쥐고 있으면 잠시 뒤 다시 연다.
    let deadline = Instant::now() + WAIT;
    let cluster = loop {
        match Cluster::open_observer(
            config.clone(),
            GrpcNetwork::new(peers.clone()),
            peers.clone(),
        )
        .await
        {
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
        observer
            .peers
            .ping(nodes[0].id(), &leader_addr, WAIT)
            .await
            .unwrap(),
        nodes[0].id()
    );
    assert_eq!(
        nodes[0]
            .peers
            .ping(observer.id(), &observer.addr, WAIT)
            .await
            .unwrap(),
        observer.id()
    );

    // 관찰 노드에 온 Raft RPC
    let mut raft = GrpcNetwork::new(nodes[0].peers.clone())
        .new_client(observer.id(), &OrbNode::single(&observer.addr))
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
    let err = observer
        .peers
        .ping(nodes[0].id(), &leader_addr, WAIT)
        .await
        .unwrap_err();
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

/// 닫힌 포트(연결은 곧바로 거절된다)
async fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().to_string()
}

/// 연결을 받고 아무것도 답하지 않는 리스너. 멈추면 받은 연결도 닫는다.
struct Blackhole(tokio::task::JoinHandle<()>);

impl Blackhole {
    fn start(listener: TcpListener) -> Self {
        Self(tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        }))
    }

    /// 리스너와 받은 연결을 닫는다(그 포트에 다시 bind할 수 있다).
    async fn stop(self) {
        self.0.abort();
        let _ = self.0.await;
    }
}

/// 받고 답하지 않는 첫 링크로 보낸 짧은 호출(Raft 하트비트처럼 200 ms 제한 시간)은 연속 두 번 넘기면 1초 안에
/// 둘째 링크로 간다(선거 제한 시간 전에).
#[tokio::test]
async fn short_calls_leave_a_silent_link_within_a_second() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let hole = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = hole.local_addr().unwrap().to_string();
    let hole = Blackhole::start(hole);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let peers = Peers::new(&tls_for(&ca, id));
    peers.set_link_book(book);

    let started = Instant::now();
    while peers.active_link(&first) == first {
        assert!(started.elapsed() < WAIT);
        let _ = peers.ping(id, &first, Duration::from_millis(200)).await;
    }
    let took = started.elapsed();
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert_eq!(peers.active_link(&first), server);
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    hole.stop().await;
}

/// 링크가 다른 노드를 가리키면(노드 A의 첫 링크가 B의 주소) 대상 id를 실은 호출(리더 전달·Raft RPC·ping·bulk)을 B가
/// 거절하고, 보내는 쪽은 그것을 링크 실패로 세어 A의 다음 링크로 다시 보낸다. 대상 id가 다른 호출은 처리하지
/// 않는다.
#[tokio::test]
async fn a_link_to_another_node_is_refused_and_skipped() {
    let (ca, _root, nodes) = start(2).await;
    let leader = wait_leader(&nodes).await;
    let a = nodes.iter().find(|n| n.id() == leader).unwrap();
    let b = nodes.iter().find(|n| n.id() != leader).unwrap();
    let wrong = OrbNode::new([b.addr.clone(), a.addr.clone()]);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&wrong]);
    let peers = Arc::new(Peers::new(&tls_for(&ca, b.id())));
    peers.set_link_book(book.clone());

    let renew = LeaderRequest::RenewLease { node: b.id() };
    Forwarder::forward(peers.as_ref(), a.id(), &b.addr, renew)
        .await
        .unwrap();
    assert_eq!(peers.active_link(&b.addr), a.addr);

    // ping과 bulk 호출(지표 읽기)도 대상 id를 싣는다.
    let pinging = Peers::new(&tls_for(&ca, b.id()));
    pinging.set_link_book(book.clone());
    assert_eq!(pinging.ping(a.id(), &b.addr, WAIT).await.unwrap(), a.id());
    assert_eq!(pinging.active_link(&b.addr), a.addr);
    let reading = Peers::new(&tls_for(&ca, b.id()));
    reading.set_link_book(book);
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: None,
    };
    let reply = reading.read_metrics(a.id(), &b.addr, query, WAIT).await;
    assert!(reply.unwrap().is_none());
    assert_eq!(reading.active_link(&b.addr), a.addr);

    // Raft RPC도 대상 id를 싣는다(링크 책이 없는 Peers).
    let raft_only = Arc::new(Peers::new(&tls_for(&ca, b.id())));
    let mut conn = GrpcNetwork::new(raft_only.clone())
        .new_client(a.id(), &wrong)
        .await;
    conn.vote(
        VoteRequest::new(Vote::new(0, b.id()), None),
        RPCOption::new(Duration::from_secs(3)),
    )
    .await
    .unwrap();
    assert_eq!(raft_only.active_link(&b.addr), a.addr);

    // 링크가 하나뿐이면 다시 보낼 곳이 없어 거절이 그대로 온다.
    let mut lone = GrpcNetwork::new(raft_only.clone())
        .new_client(a.id(), &OrbNode::single(&b.addr))
        .await;
    let err = lone
        .vote(
            VoteRequest::new(Vote::new(0, b.id()), None),
            RPCOption::new(Duration::from_secs(3)),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("wrong node"), "{err}");
}

/// 연결하지 못한 bulk 호출도 같은 호출 안에서 둘째 링크로 다시 보내고, 시험 호출은 control 채널의 짧은 호출만이라
/// 재시도 간격이 지나도 bulk 호출은 답 없는 우선 링크로 가지 않는다.
#[tokio::test]
async fn bulk_calls_retry_on_the_next_link_and_never_try_the_preferred_one() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = closed_port().await;
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_millis(300);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: None,
    };
    // 이 노드는 지표를 모른다(옛 노드처럼 None).
    let reply = peers
        .read_metrics(id, &first, query.clone(), WAIT)
        .await
        .unwrap();
    assert!(reply.is_none());
    assert_eq!(peers.active_link(&first), server);

    // 우선 링크가 받고 답하지 않게 되고 재시도 간격이 지나도, bulk 호출은 지금 링크로 곧바로 답을 받는다.
    let hole = Blackhole::start(TcpListener::bind(&first).await.unwrap());
    tokio::time::sleep(retry * 2).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(2),
        peers.read_metrics(id, &first, query, WAIT),
    )
    .await
    .expect("a bulk call went to the silent preferred link");
    assert!(reply.unwrap().is_none());
    hole.stop().await;
}

/// 주소로 쓸 수 없는 링크는 연결 실패로 세어 다음 링크로 넘어가고, 시험 호출도 거기서 끝난다.
#[tokio::test]
async fn a_link_that_is_not_an_address_is_skipped() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = "not an address:1".to_string();
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_millis(200);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
    tokio::time::sleep(retry * 2).await;
    // 시험 호출이 실패해도 다음 링크로 다시 보내 답을 받는다.
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
}

/// 받은 연결을 `to`로 잇는 TCP 중계. 연결 수를 세고, 얼리면(`freeze`) 보내는 쪽의 바이트는 계속 넘기고 받는 쪽으로
/// 오는 답은 버린다(받고 답하지 않는 서버, 연결은 열어 둠).
struct Relay {
    addr: String,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    frozen: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    async fn start(to: String) -> Self {
        use std::sync::atomic::Ordering;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let frozen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (count, freeze) = (accepted.clone(), frozen.clone());
        let task = tokio::spawn(async move {
            let mut conns = tokio::task::JoinSet::new();
            while let Ok((client, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let (to, freeze) = (to.clone(), freeze.clone());
                conns.spawn(async move {
                    let upstream = tokio::net::TcpStream::connect(&to).await.unwrap();
                    let (mut client_rx, mut client_tx) = client.into_split();
                    let (mut up_rx, mut up_tx) = upstream.into_split();
                    let sending = async move {
                        let _ = tokio::io::copy(&mut client_rx, &mut up_tx).await;
                    };
                    let answering = async move {
                        let mut buf = [0u8; 16 << 10];
                        loop {
                            match up_rx.read(&mut buf).await {
                                Ok(n) if n > 0 => {
                                    if !freeze.load(Ordering::SeqCst)
                                        && client_tx.write_all(&buf[..n]).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                // 얼린 뒤 서버가 닫아도 받는 쪽 연결은 열어 둔다.
                                _ if freeze.load(Ordering::SeqCst) => std::future::pending().await,
                                _ => return,
                            }
                        }
                    };
                    tokio::join!(sending, answering);
                });
            }
        });
        Self {
            addr,
            accepted,
            frozen,
            task,
        }
    }

    fn freeze(&self) {
        self.frozen.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 이미 보낸 쓰기가 연결에서 답을 받지 못하고 `Unavailable`로 끝나면(답하지 않는 연결의 keepalive ping 만료, 약 30초)
/// 받았을 수 있으므로 다른 링크로 다시 보내지 않는다. 링크 선택에는 연결 실패로 세어 다음 호출은 둘째 링크로 간다.
#[tokio::test]
async fn a_sent_write_that_loses_its_connection_is_not_sent_again() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = Relay::start(server.clone()).await;
    let second = Relay::start(server).await;
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.addr.clone(), second.addr.clone()])]);
    let peers = Peers::new(&tls_for(&ca, id));
    peers.set_link_book(book);
    assert_eq!(peers.ping(id, &first.addr, WAIT).await.unwrap(), id);

    first.freeze();
    let remove = LeaderRequest::Remove {
        id: NodeId(Uuid::now_v7()),
    };
    let started = Instant::now();
    let err = peers.leader_request(&first.addr, remove).await.unwrap_err();
    assert!(
        matches!(err, opl_cluster::ClusterError::Unreachable(_)),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(60), "{err:?}");
    assert_eq!(second.accepted(), 0, "the write was sent again");
    assert_eq!(first.accepted(), 1);
    assert_eq!(peers.active_link(&first.addr), second.addr);
    peers
        .leader_request(&first.addr, LeaderRequest::RenewLease { node: id })
        .await
        .unwrap();
}

/// 노드의 링크가 `[닫힌 포트, 서버]`면 첫 호출이 연결 실패로 같은 호출 안에서 둘째 링크로 다시 가고, 그 뒤의 ping·
/// 리더 요청·Follow·Raft RPC가 둘째 링크로 간다. Raft는 링크 책 없이도 멤버십의 링크로 넘어가고, 책이 있으면 책의
/// 목록을 쓴다(다른 호출과 같은 선택).
#[tokio::test]
async fn calls_fail_over_to_the_second_link() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let first = closed_port().await;
    let node = OrbNode::new([first.clone(), server.clone()]);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&node]);
    let peers = Arc::new(Peers::new(&tls_for(&ca, id)));
    peers.set_link_book(book);
    assert_eq!(peers.active_link(&first), first);

    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    assert_eq!(peers.active_link(&first), server);
    assert_eq!(peers.ping(id, &first, WAIT).await.unwrap(), id);
    peers
        .leader_request(&first, LeaderRequest::RenewLease { node: id })
        .await
        .unwrap();
    let mut frames = FollowClient::follow(peers.as_ref(), id, &first, 0)
        .await
        .unwrap();
    assert!(matches!(
        frames.next().await,
        Some(Ok(FollowFrame::Hello { .. }))
    ));

    let vote = || VoteRequest::new(Vote::new(0, id), None);
    let option = || RPCOption::new(Duration::from_secs(3));
    // 책이 있으면 Raft도 책의 링크를 쓴다(로그의 멤버십이 아직 링크 하나여도 지금 링크 그대로).
    let mut conn = GrpcNetwork::new(peers.clone())
        .new_client(id, &OrbNode::single(&first))
        .await;
    conn.vote(vote(), option()).await.unwrap();
    assert_eq!(peers.active_link(&first), server);

    // 링크 책이 없는 Peers의 Raft도 멤버십의 링크로 넘어간다.
    let raft_only = Arc::new(Peers::new(&tls_for(&ca, id)));
    let mut conn = GrpcNetwork::new(raft_only.clone())
        .new_client(id, &node)
        .await;
    conn.vote(vote(), option()).await.unwrap();
    assert_eq!(raft_only.active_link(&first), server);
    conn.vote(vote(), option()).await.unwrap();
}

/// 받고 답하지 않는 우선 링크로 보낸 리스 갱신은 3초 제한 시간에 넘어가고 다음 갱신은 둘째 링크로 성공한다. 우선
/// 링크에 서버가 다시 뜨면 재시도 간격 뒤의 시험 호출로 돌아온다.
#[tokio::test]
async fn a_silent_link_times_out_and_the_preferred_link_comes_back() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let hole = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first = hole.local_addr().unwrap().to_string();
    let hole = Blackhole::start(hole);
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let retry = Duration::from_secs(2);
    let peers = Peers::new(&tls_for(&ca, id)).with_link_retry(retry);
    peers.set_link_book(book);

    let started = Instant::now();
    let renew = || LeaderRequest::RenewLease { node: id };
    assert!(peers.leader_request(&first, renew()).await.is_err());
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2500) && took < Duration::from_secs(10),
        "{took:?}"
    );
    assert_eq!(peers.active_link(&first), server);
    peers.leader_request(&first, renew()).await.unwrap();

    // 우선 링크에 같은 노드의 서버를 띄운다.
    hole.stop().await;
    let listener = TcpListener::bind(&first).await.unwrap();
    let tls = tls_for(&ca, id);
    let service = PeerService::new(nodes[0].cluster.clone(), None);
    let (stop, rx) = oneshot::channel::<()>();
    let revived = tokio::spawn(async move {
        opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    // 재시도 간격 전에는 둘째 링크에 머문다.
    peers.leader_request(&first, renew()).await.unwrap();
    assert_eq!(peers.active_link(&first), server);
    let deadline = Instant::now() + WAIT;
    while peers.active_link(&first) != first {
        assert!(
            Instant::now() < deadline,
            "did not return to the preferred link"
        );
        let _ = peers.leader_request(&first, renew()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started.elapsed() >= retry);
    peers.leader_request(&first, renew()).await.unwrap();
    let _ = stop.send(());
    revived.await.unwrap();
}
```

`crates/opld/src/member.rs`에서

```rust
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
```

을 다음으로 바꾼다:

```rust
        );
        // 노드 간 호출이 멤버십의 링크로 넘어가기를 한다(관찰 노드·위트니스 포함).
        peers.set_link_book(cluster.link_book());
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
```

`crates/opld/src/member.rs`에서

```rust
            observers: Default::default(),
        };
```

을 다음으로 바꾼다:

```rust
            observers: Default::default(),
            links: Default::default(),
        };
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1906개, 실패 0

Run: `cargo test -p opld --features ha-it --test ha`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 링크 고르기와 넘어가기 — LinkBook·LinkSelector, memnet 링크 끊기, Peers 링크 넘어가기"
```

---

### Task 3: 노드 설정·인증서·합류의 여러 링크

opld 설정 `cluster_addresses`(호스트만, 앞이 우선, 없으면 `address` 하나; `Config::cluster_links()`·`cert_names()`), 호스트 규칙은 새 `opl_types::host`(`valid_host`·`same_host`·`same_link`)로 합류 API와 같게. 노드가 원하는 링크를 `NodeStatus.cluster_addresses`에 적는다(인증서 이름에 있는 링크만). init·합류의 인증서에 모든 링크 이름, 다시 시작할 때 빠진 이름이 있으면 로컬 CA로 같은 키에 다시 서명(`pki::csr_for_key`, `NodeTls::save_cert`). 합류 `JoinRequest.cluster_addresses`(둘 이상일 때만 보냄)와 `LeaderRequest::AddLearner.addrs`; API는 첫 링크·중복·호스트·다른 노드와의 겹침을 보고, 둘 이상이면 `multi-link` 기능을 요구한다(이 Task에서는 아직 409). 다시 합류는 기록의 링크를 새 값으로. 마이그레이션 QEMU 스트림 호스트는 받을 준비 요청이 성공한 뒤의 지금 링크. OpenAPI와 `schema.d.ts`.

**Files:**
- Create: `crates/opl-types/src/host.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks/migrate.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/drain.rs`
- Modify: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/src/types.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`
- Modify: `crates/opl-peer/src/pki.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/join.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs` 전체:

```rust
//! 클러스터 정보, 노드 합류(조인 토큰, CSR 서명), 노드 제거.
//!
//! 새 노드는 조인 URL을 받아 이 노드의 `POST /cluster/join`을 부른다. 토큰을 지워 한 번만 쓰게 하고,
//! 노드의 CSR에 클러스터 CA로 서명하고, Node 리소스를 만들고, 노드를 learner로 더한다. 새 노드는
//! 받은 인증서로 노드 간 서버를 띄우고 로그를 따라잡은 뒤 스스로 투표 멤버 승격을 요청한다(모든 노드가
//! `voter-limit`를 알면 위트니스만 요청하고, 다른 노드는 리더의 `VoterKeeper`가 상한까지 올린다).

use std::fmt;
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
use opl_types::host::{same_link, valid_host};
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
    /// 새 노드의 노드 간 통신 주소(`host:port`, 첫 링크)
    pub cluster_address: String,
    /// 새 노드의 모든 노드 간 통신 링크(`host:port`, 앞이 우선, 첫 것이 `cluster_address`). 링크가 둘 이상일
    /// 때만 보낸다(옛 노드는 이 필드를 모른다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cluster_addresses: Vec<String>,
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
    lacking_feature(feature, &missing_feature(cluster, feature)?)
}

/// `old`(기능 `feature`를 적지 않은 노드 이름)가 있으면 409 `cluster_upgrading`
fn lacking_feature(feature: &str, old: &[String]) -> Result<(), ApiError> {
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
    // 링크 목록: 보낸 것, 없으면(링크 하나, 옛 노드) `cluster_address` 하나
    let links = if req.cluster_addresses.is_empty() {
        vec![req.cluster_address.clone()]
    } else {
        req.cluster_addresses.clone()
    };
    let cluster_hosts: Option<Vec<&str>> = links
        .iter()
        .map(|link| host_of(link).filter(|h| valid_host(h)))
        .collect();
    let (Some(api_host), Some(cluster_hosts)) = (
        host_of(&req.address).filter(|h| valid_host(h)),
        cluster_hosts,
    ) else {
        return Err(ApiError::bad_request(
            "address, cluster_address and cluster_addresses must be host:port with an IP address or a host name",
        ));
    };
    if links[0] != req.cluster_address {
        return Err(ApiError::bad_request(
            "the first of cluster_addresses must be cluster_address",
        ));
    }
    if links
        .iter()
        .enumerate()
        .any(|(i, l)| links[..i].iter().any(|o| same_link(o, l)))
    {
        return Err(ApiError::bad_request(
            "cluster_addresses must not repeat an address",
        ));
    }
    // 옛 노드는 여러 링크의 멤버십을 첫 링크 하나로 읽으므로 모든 노드가 링크를 알 때만 받는다. 다시 합류하는
    // 노드 자신의 옛 기록(등록 전이면 기능이 비어 있다)은 세지 않는다.
    if links.len() > 1 {
        let mut old = missing_feature(&state.cluster, "multi-link")?;
        old.retain(|name| *name != req.name);
        lacking_feature("multi-link", &old)?;
    }
    if req.node.is_nil() {
        return Err(ApiError::bad_request("node id must not be nil"));
    }
    // learner이면 합류하다 멈춘 노드다. 같은 이름의 오프라인 노드만 새 토큰으로 다시 합류할 수 있다.
    // 그래야 토큰을 가진 쪽이 승격을 기다리는 살아 있는 노드의 id를 가로채지 못한다.
    let members = state.cluster.members();
    // 관찰 노드도 learner처럼 본다(같은 이름의 오프라인 노드만 다시 합류하고, 주소가 바뀌었으면 먼저 뺀다).
    let learner_addr = members
        .learners
        .get(&NodeId(req.node))
        .or_else(|| members.observers.get(&NodeId(req.node)))
        .cloned();
    // 그 learner·관찰 노드의 지금 링크 목록
    let learner_links = learner_addr.as_ref().map(|addr| {
        members
            .links
            .get(&NodeId(req.node))
            .cloned()
            .unwrap_or_else(|| vec![addr.clone()])
    });
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
    // 다른 노드(구성원·관찰 노드)의 링크는 받지 않는다(오타, 다시 쓴 IP): 그 노드에 갈 Raft RPC·리더 요청을 이
    // 노드가 받게 된다. 다른 노드가 원하는(Node 기록의, 링크 맞추기가 아직 멤버십에 넣지 않은) 링크도 본다.
    let (_, records) = state.cluster.list::<NodeSpec>()?;
    let wanted = records
        .into_iter()
        .map(|n| (NodeId(n.id), n.status.cluster_addresses));
    if let Some((other, link)) = members
        .links
        .clone()
        .into_iter()
        .chain(wanted)
        .filter(|(id, _)| *id != NodeId(req.node))
        .find_map(|(id, theirs)| {
            let link = links
                .iter()
                .find(|l| theirs.iter().any(|t| same_link(t, l)))?;
            Some((id, link))
        })
    {
        return Err(ApiError::bad_request(format!(
            "cluster link {link} is already a link of node {other}"
        )));
    }
    let info = cluster_info(&state)?;
    let via = via_voter(&state.cluster.members(), NodeId(state.node))
        .ok_or_else(|| ApiError::internal("this node knows no voter"))?;

    let mut sans = vec![api_host.to_string()];
    for host in cluster_hosts {
        if !sans.iter().any(|s| s == host) {
            sans.push(host.to_string());
        }
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
            // 링크는 이번 합류의 것으로 둔다. 옛 값이 남으면 리더의 링크 맞추기가 멤버십을 옛 링크로 되돌린다.
            let status = NodeStatus {
                online: true,
                version: req.version.clone(),
                cluster_addresses: links.clone(),
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
    // add_learner는 이미 있는 노드의 주소를 바꾸지 않으므로, 링크가 바뀐 learner는 먼저 뺀다.
    if learner_links.is_some_and(|old| old != links) {
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
            // 링크 하나면 옛 형식 그대로 보낸다.
            addrs: if links.len() > 1 { links } else { Vec::new() },
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
            links: Default::default(),
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

    /// 옛 노드가 보내는 합류 요청(`cluster_addresses` 없음)을 읽고, 링크가 하나면 그 필드 없이 쓴다(옛 API 노드는
    /// 모르는 필드를 거절한다).
    #[test]
    fn join_request_without_links_is_the_old_shape() {
        let node = Uuid::now_v7();
        let old = serde_json::json!({
            "token": "t", "node": node, "name": "n2", "address": "10.0.0.7:8006",
            "cluster_address": "10.0.0.7:8007", "csr_pem": "csr", "witness": false,
            "version": "1.0.0"
        });
        let req: JoinRequest = serde_json::from_value(old.clone()).unwrap();
        assert!(req.cluster_addresses.is_empty());
        assert_eq!(serde_json::to_value(&req).unwrap(), old);
        let two = JoinRequest {
            cluster_addresses: vec!["10.0.0.7:8007".into(), "10.1.0.7:8007".into()],
            ..req
        };
        let value = serde_json::to_value(&two).unwrap();
        assert_eq!(
            value["cluster_addresses"],
            serde_json::json!(["10.0.0.7:8007", "10.1.0.7:8007"])
        );
    }

    #[test]
    fn host_of_handles_v6_and_names() {
        assert_eq!(host_of("10.0.0.5:8006"), Some("10.0.0.5"));
        assert_eq!(host_of("[::1]:8006"), Some("::1"));
        assert_eq!(host_of("node1.lan:8007"), Some("node1.lan"));
        assert_eq!(host_of("node1.lan"), None);
        assert_eq!(host_of(":8006"), None);
        assert_eq!(host_of("h:port"), None);
        assert_eq!(host_of("[::1]:8006").filter(|h| valid_host(h)), Some("::1"));
    }
}
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            features: vec![],
        };
```

을 다음으로 바꾼다:

```rust
            features: vec![],
            cluster_addresses: vec![],
        };
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        .ok_or("this node cannot reach other nodes")?;
    // 대상 노드에는 클러스터 주소의 호스트(opld `address`)로 접속한다. 같은 이름이 노드 인증서에 있다.
    let addr = ctx
```

을 다음으로 바꾼다:

```rust
        .ok_or("this node cannot reach other nodes")?;
    let addr = ctx
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let host = addr_host(&addr)
        .ok_or_else(|| format!("cannot tell the host of node address {addr:?}"))?
        .to_string();
    let migration = MigrationStatus {
```

을 다음으로 바꾼다:

```rust
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let migration = MigrationStatus {
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        Ok(()) => match set_disks_shared(&ctx.cluster, &vm, &*ctx.storage, true).await {
            Ok(()) => send(ctx, &peers, &addr, &host, &vm, &migration).await,
            Err(e) => Err(format!("cannot share the disks with the target node: {e}")),
```

을 다음으로 바꾼다:

```rust
        Ok(()) => match set_disks_shared(&ctx.cluster, &vm, &*ctx.storage, true).await {
            Ok(()) => send(ctx, &peers, &addr, &vm, &migration).await,
            Err(e) => Err(format!("cannot share the disks with the target node: {e}")),
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
    addr: &str,
    host: &str,
    vm: &Vm,
```

을 다음으로 바꾼다:

```rust
    addr: &str,
    vm: &Vm,
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    let copied = if migration.disks.is_empty() {
```

을 다음으로 바꾼다:

```rust
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    // 대상 노드에는 받을 준비 요청이 닿은 클러스터 링크(그 노드에 지금 쓰는 링크; 첫 링크가 끊겼으면 다른 링크)의
    // 호스트로 접속한다. 요청 전에 읽으면 아직 모르는 죽은 첫 링크일 수 있다. 모든 링크의 이름이 노드 인증서에 있다.
    let link = peers.active_link(addr);
    let host =
        addr_host(&link).ok_or_else(|| format!("cannot tell the host of node address {link:?}"))?;
    let copied = if migration.disks.is_empty() {
```

`crates/opl-api/tests/cluster.rs`에서

```rust
use opl_cluster::types::NodeId;
use opl_peer::pki::{fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::{ClusterSpec, ClusterStatus, JoinTokenSpec};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::NodeId;
use opl_peer::pki::{cert_names, fingerprint, first_cert, node_id_of, node_key_and_csr};
use opl_types::cluster::{ClusterSpec, ClusterStatus, JoinTokenSpec};
```

`crates/opl-api/tests/cluster.rs`에서

```rust
            cluster_address: "10.0.0.7:8007".into(),
            csr_pem,
```

을 다음으로 바꾼다:

```rust
            cluster_address: "10.0.0.7:8007".into(),
            cluster_addresses: vec![],
            csr_pem,
```

`crates/opl-api/tests/cluster.rs`에서

```rust

    let (req, node) = join_request(&new_token(&env).await, "node2");
    let mut body = json!(req);
```

을 다음으로 바꾼다:

```rust

    let (mut req, node) = join_request(&new_token(&env).await, "node2");
    // 다른 노드의 링크로는 합류하지 못한다.
    req.cluster_address = "10.0.0.8:8007".into();
    let mut body = json!(req);
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    req.version = Some("1:0.1".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
```

을 다음으로 바꾼다:

```rust
    req.version = Some("1:0.1".into());
    req.cluster_address = "10.0.0.8:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    );
    let (req, _) = join_request(&token, "node3");
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
```

을 다음으로 바꾼다:

```rust
    );
    let (mut req, _) = join_request(&token, "node3");
    req.cluster_address = "10.0.0.8:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn bad_or_expired_tokens_are_refused() {
```

을 다음으로 바꾼다:

```rust
    assert_eq!(
        anon.post("/cluster/join", json!(req)).await.0,
        StatusCode::OK
    );
}

/// 노드의 기록된 기능에 `feature`를 더한다(이 빌드가 아직 적지 않는 기능).
async fn add_feature(env: &Env, node: Uuid, feature: &str) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.features.push(feature.into());
    env.cluster
        .update_status::<NodeSpec>(node, &status)
        .await
        .unwrap();
}

/// 여러 링크로 합류: 모든 노드가 `multi-link`를 적었을 때만 받고(아니면 토큰을 쓰지 않고 409), 링크 목록 전체가
/// 멤버십과 인증서 이름에 들어간다. 다시 합류할 때 링크 목록이 바뀌면(첫 링크가 같아도) 그 목록으로 다시 더한다.
#[tokio::test]
async fn a_node_joins_with_several_links() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let links = vec!["10.0.0.7:8007".to_string(), "10.1.0.7:8007".into()];
    let (mut req, node) = join_request(&new_token(&env).await, "node2");
    req.cluster_addresses = links.clone();
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "cluster_upgrading");
    assert!(env.cluster.members().learners.is_empty());

    add_feature(&env, env.node, "multi-link").await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        cert_names(body["cert_pem"].as_str().unwrap()).unwrap(),
        ["10.0.0.7", "10.1.0.7", "localhost", "127.0.0.1", "::1"]
    );
    let members = env.cluster.members();
    assert_eq!(members.addr(NodeId(node)), Some("10.0.0.7:8007"));
    assert_eq!(members.links[&NodeId(node)], links);

    // 첫 링크는 그대로, 둘째 링크만 바꿔 다시 합류한다. 등록 전의 자기 기록(기능 없음)은 세지 않는다.
    mark_offline(&env, node).await;
    let mut req = join_request(&new_token(&env).await, "node2").0;
    req.node = node;
    req.cluster_addresses = vec!["10.0.0.7:8007".into(), "10.2.0.7:8007".into()];
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let members = env.cluster.members();
    assert_eq!(members.learners.len(), 1);
    assert_eq!(members.links[&NodeId(node)], req.cluster_addresses);
    // 기록된 링크도 이번 것이다(옛 링크가 남으면 리더의 링크 맞추기가 멤버십을 되돌린다).
    let recorded = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    assert_eq!(recorded.status.cluster_addresses, req.cluster_addresses);
}

/// 다른 노드의 링크(같은 주소의 다른 표기 포함)로는 합류하지 못한다(그 노드에 갈 호출을 받게 된다).
#[tokio::test]
async fn a_link_of_another_node_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.cluster_address = "[fd00::7]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let token = new_token(&env).await;
    let (mut req, _) = join_request(&token, "node3");
    req.address = "10.0.0.8:8006".into();
    req.cluster_address = "[fd00:0::7]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("already a link"),
        "{body}"
    );
    assert_eq!(env.cluster.members().learners.len(), 1);
    // 토큰은 쓰지 않았다.
    let (mut req, _) = join_request(&token, "node3");
    req.cluster_address = "[fd00::8]:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// 다른 노드가 원하는(Node 기록의 `cluster_addresses`, 아직 멤버십에 없는) 링크로도 합류하지 못한다.
#[tokio::test]
async fn a_link_another_node_wants_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mine = env.cluster.members().voters[&NodeId(env.node)].clone();
    node.status.cluster_addresses = vec![mine, "10.9.0.1:8007".into()];
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();

    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.cluster_address = "10.9.0.1:8007".into();
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("already a link"),
        "{body}"
    );
    assert!(env.cluster.members().learners.is_empty());
}

/// 링크 목록은 첫 것이 `cluster_address`이고 겹치지 않으며 모두 SAN에 넣을 수 있는 호스트여야 한다(기능 검사보다
/// 먼저 본다).
#[tokio::test]
async fn links_must_start_with_the_cluster_address() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let token = new_token(&env).await;
    for links in [
        ["10.1.0.7:8007", "10.0.0.7:8007"],
        ["10.0.0.7:8007", "10.0.0.7:8007"],
        ["10.0.0.7:8007", "evil host/x:8007"],
        ["10.0.0.7:8007", "10.1.0.7"],
    ] {
        let (mut req, _) = join_request(&token, "node2");
        req.cluster_addresses = links.iter().map(|l| l.to_string()).collect();
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{links:?}: {body}");
        assert_eq!(body["code"], "invalid_request");
    }
    // 겹침은 값으로 본다(IP는 같은 주소, 이름은 대소문자 없이).
    for links in [
        ["[fd00::7]:8007", "[fd00:0::7]:8007"],
        ["node2.lan:8007", "NODE2.lan:8007"],
    ] {
        let (mut req, _) = join_request(&token, "node2");
        req.cluster_address = links[0].into();
        req.cluster_addresses = links.iter().map(|l| l.to_string()).collect();
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{links:?}: {body}");
    }
    // 링크 하나만 적으면 옛 요청과 같다(기능이 필요 없다).
    let (mut req, _) = join_request(&token, "node2");
    req.cluster_addresses = vec!["10.0.0.7:8007".into()];
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn bad_or_expired_tokens_are_refused() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            features: vec![],
        };
```

을 다음으로 바꾼다:

```rust
            features: vec![],
            cluster_addresses: vec![],
        };
```

`crates/opl-api/tests/drain.rs`에서

```rust
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    };
```

을 다음으로 바꾼다:

```rust
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        cluster_addresses: vec![],
    };
```

`crates/opl-api/tests/ha.rs`에서

```rust
        features: vec![],
    };
```

을 다음으로 바꾼다:

```rust
        features: vec![],
        cluster_addresses: vec![],
    };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        features: vec![],
    };
```

을 다음으로 바꾼다:

```rust
        features: vec![],
        cluster_addresses: vec![],
    };
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        };
```

을 다음으로 바꾼다:

```rust
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            cluster_addresses: vec![],
        };
```

`crates/opl-cluster/src/leader.rs`에서

```rust
        }
        LeaderRequest::AddLearner { id, addr } => {
            retry_in_progress(timeout, || {
                raft.add_learner(id, OrbNode::single(addr.clone()), false)
            })
            .await
        }
```

을 다음으로 바꾼다:

```rust
        }
        LeaderRequest::AddLearner { id, addr, addrs } => {
            let node = learner_node(addr, addrs);
            retry_in_progress(timeout, || raft.add_learner(id, node.clone(), false)).await
        }
```

`crates/opl-cluster/src/leader.rs`에서

```rust
        leader_addr,
    }
}
```

을 다음으로 바꾼다:

```rust
        leader_addr,
    }
}

/// `AddLearner`의 노드 값: 링크 목록이 있으면 그것, 없으면(링크 하나, 옛 노드) `addr` 하나
fn learner_node(addr: String, addrs: Vec<String>) -> OrbNode {
    if addrs.is_empty() {
        OrbNode::single(addr)
    } else {
        OrbNode::new(addrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learner_node_uses_every_link() {
        assert_eq!(
            learner_node("a:8007".into(), vec![]),
            OrbNode::single("a:8007")
        );
        let links = vec!["a:8007".to_string(), "b:8007".to_string()];
        assert_eq!(
            learner_node("a:8007".into(), links.clone()),
            OrbNode::new(links)
        );
    }
}
```

`crates/opl-cluster/src/types.rs`에서

```rust
        id: NodeId,
        addr: String,
    },
```

을 다음으로 바꾼다:

```rust
        id: NodeId,
        /// 첫 링크
        addr: String,
        /// 모든 링크(앞이 우선, 첫 것이 `addr`). 링크가 하나면 비운다(옛 형식 그대로). 옛 리더는 이것을
        /// 모르고 `addr` 하나로 더한다.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        addrs: Vec<String>,
    },
```

`crates/opl-cluster/src/types.rs`에서

```rust

    /// 노드 사이로 오가는 형식이다. VIP 리스 갱신.
```

을 다음으로 바꾼다:

```rust

    /// 노드 사이로 오가는 형식이다. learner 더하기: 링크가 하나면 옛 형식 그대로, 여럿이면 `addrs`를 더한다.
    #[test]
    fn add_learner_wire_format_is_stable() {
        let id = NodeId::from(4);
        let one = LeaderRequest::AddLearner {
            id,
            addr: "a:8007".into(),
            addrs: vec![],
        };
        let old = json!({"op": "add-learner", "id": id.to_string(), "addr": "a:8007"});
        assert_eq!(serde_json::to_value(&one).unwrap(), old);
        assert_eq!(serde_json::from_value::<LeaderRequest>(old).unwrap(), one);
        let two = LeaderRequest::AddLearner {
            id,
            addr: "a:8007".into(),
            addrs: vec!["a:8007".into(), "b:8007".into()],
        };
        let value = serde_json::to_value(&two).unwrap();
        assert_eq!(
            value,
            json!({"op": "add-learner", "id": id.to_string(), "addr": "a:8007",
                   "addrs": ["a:8007", "b:8007"]})
        );
        assert_eq!(serde_json::from_value::<LeaderRequest>(value).unwrap(), two);
    }

    /// 노드 사이로 오가는 형식이다. VIP 리스 갱신.
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
            addr: addr(4).addr().to_string(),
        })
```

을 다음으로 바꾼다:

```rust
            addr: addr(4).addr().to_string(),
            addrs: vec![],
        })
```

`crates/opl-peer/src/pki.rs`에서

```rust

/// PEM의 첫 인증서
```

을 다음으로 바꾼다:

```rust

/// 이미 있는 노드 키(PEM)의 CSR(PEM). 같은 키로 인증서를 다시 받을 때(이름을 더할 때) 쓴다.
pub fn csr_for_key(key_pem: &str) -> Result<String, PkiError> {
    let key = KeyPair::from_pem(key_pem)?;
    Ok(CertificateParams::default()
        .serialize_request(&key)?
        .pem()?)
}

/// PEM의 첫 인증서
```

`crates/opl-peer/src/pki.rs`에서

```rust
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
```

을 다음으로 바꾼다:

```rust
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    }

    /// `dir`(이미 저장한 노드 TLS 자료)의 노드 인증서만 `cert_pem`으로 바꾼다. 옆 임시 파일에 다 쓴 뒤 이름을
    /// 바꾸므로 도중에 죽어도 옛 인증서나 새 인증서 하나가 남는다.
    pub fn save_cert(dir: &Path, cert_pem: &str) -> Result<(), PkiError> {
        let tmp = dir.join(format!("{CERT_FILE}.tmp"));
        if tmp.exists() {
            std::fs::remove_file(&tmp)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&tmp)?;
        file.write_all(cert_pem.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(CERT_FILE))?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
```

`crates/opl-peer/src/pki.rs`에서

```rust
    }

    #[test]
    fn debug_does_not_show_keys() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 같은 키로 이름을 더해 다시 서명한다: 공개키와 CN(노드 id)은 그대로다.
    #[test]
    fn a_node_cert_is_signed_again_for_the_same_key() {
        let ca = ClusterCa::generate("prod").unwrap();
        let node = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let old = ca.sign_node(&csr, node, &["10.0.0.5".into()]).unwrap();
        let csr = csr_for_key(&key_pem).unwrap();
        let names = ["10.0.0.5".to_string(), "10.1.0.5".into()];
        let new = ca.sign_node(&csr, node, &names).unwrap();
        assert_eq!(cert_names(&new).unwrap(), names);
        let public_key = |pem: &str| {
            let der = first_cert(pem).unwrap();
            let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
            cert.public_key().raw.to_vec()
        };
        assert_eq!(public_key(&new), public_key(&old));
        assert_eq!(node_id_of(&first_cert(&new).unwrap()), Some(node));
        assert!(csr_for_key("not a key").is_err());
    }

    #[test]
    fn save_cert_replaces_only_the_node_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let pki = dir.path().join(PKI_DIR);
        let ca = ClusterCa::generate("prod").unwrap();
        let tls = node_tls(&ca, Uuid::now_v7());
        tls.save(&pki).unwrap();
        let other = node_tls(&ca, Uuid::now_v7());
        NodeTls::save_cert(&pki, &other.cert_pem).unwrap();
        let back = NodeTls::load(&pki).unwrap().unwrap();
        assert_eq!(back.cert_pem, other.cert_pem);
        assert_eq!(back.key_pem, tls.key_pem);
        assert_eq!(back.ca_pem, tls.ca_pem);
        assert!(!pki.join(format!("{CERT_FILE}.tmp")).exists());
    }

    #[test]
    fn debug_does_not_show_keys() {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            addr: new.addr.clone(),
        })
```

을 다음으로 바꾼다:

```rust
            addr: new.addr.clone(),
            addrs: vec![],
        })
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
```

을 다음으로 바꾼다:

```rust
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
            addrs: vec![],
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
```

을 다음으로 바꾼다:

```rust
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
            addrs: vec![],
        })
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
```

을 다음으로 바꾼다:

```rust
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
            addrs: vec![],
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
```

을 다음으로 바꾼다:

```rust
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
            addrs: vec![],
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            addr: learner.addr.clone(),
        })
```

을 다음으로 바꾼다:

```rust
            addr: learner.addr.clone(),
            addrs: vec![],
        })
```

`crates/opl-types/src/host.rs`를 만든다:

```rust
//! 노드 주소(클러스터 링크·API 주소)의 호스트 규칙. opld 설정 검사와 API 합류 검사가 함께 쓴다.

use std::net::IpAddr;

/// 다른 노드가 닿고 인증서 SAN에 넣을 수 있는 호스트인지: 특수 주소(0.0.0.0·::·멀티캐스트·브로드캐스트)가 아닌
/// IP나 RFC 1123 호스트 이름(와일드카드, 끝의 점 없이)
pub fn valid_host(host: &str) -> bool {
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

/// 같은 호스트인지: IP는 값으로(`fd00:0::5` = `fd00::5`), 이름은 대소문자 없이
pub fn same_host(a: &str, b: &str) -> bool {
    match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// 같은 링크(`host:port`, IPv6은 `[addr]:port`)인지: 포트가 같고 [`same_host`]. 모양이 아니면 문자열로 비교한다.
pub fn same_link(a: &str, b: &str) -> bool {
    match (split_link(a), split_link(b)) {
        (Some((ha, pa)), Some((hb, pb))) => pa == pb && same_host(ha, hb),
        _ => a == b,
    }
}

fn split_link(link: &str) -> Option<(&str, u16)> {
    let (host, port) = link.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    Some((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_host_accepts_ips_and_host_names_only() {
        for good in ["node1.example", "localhost", "10.0.0.5", "::1", "a-b.c1"] {
            assert!(valid_host(good), "{good}");
        }
        let long_label = format!("{}.example", "a".repeat(64));
        let long_name = vec!["a".repeat(63); 4].join(".");
        for bad in [
            "",
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
            "[::1]",
            "10.0.0.5:8007",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn hosts_and_links_compare_by_value() {
        assert!(same_host("fd00::5", "fd00:0::5"));
        assert!(same_host("Node1.LAN", "node1.lan"));
        assert!(!same_host("10.0.0.5", "10.0.0.6"));
        assert!(same_link("[fd00::5]:8007", "[fd00:0:0::5]:8007"));
        assert!(same_link("Node1.lan:8007", "node1.LAN:8007"));
        assert!(!same_link("[fd00::5]:8007", "[fd00::5]:9007"));
        assert!(!same_link("10.0.0.5:8007", "10.0.0.6:8007"));
        assert!(same_link("odd", "odd"));
    }
}
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod cluster;
pub mod metrics;
```

을 다음으로 바꾼다:

```rust
pub mod cluster;
pub mod host;
pub mod metrics;
```

`crates/opl-types/src/node.rs`에서

```rust
    pub features: Vec<String>,
}
```

을 다음으로 바꾼다:

```rust
    pub features: Vec<String>,
    /// 이 노드가 원하는 클러스터 링크(`host:port`, 앞이 우선). 노드가 등록할 때 설정의 `cluster_addresses`로
    /// 쓴다(옛 노드는 비어 있다). 리더가 Raft 멤버십의 링크를 이것에 맞춘다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub cluster_addresses: Vec<String>,
}
```

`crates/opl-types/src/node.rs`에서

```rust
    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn status_cluster_addresses_round_trip_and_are_omitted_when_empty() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("cluster_addresses").is_none(), "{none}");
        // 옛 노드가 쓴 status(필드 없음)도 읽는다.
        let old: NodeStatus = serde_json::from_value(serde_json::json!({"online": true})).unwrap();
        assert!(old.cluster_addresses.is_empty());
        let status = NodeStatus {
            cluster_addresses: vec!["10.0.0.5:8007".into(), "10.1.0.5:8007".into()],
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["cluster_addresses"],
            serde_json::json!(["10.0.0.5:8007", "10.1.0.5:8007"])
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

`crates/opld/src/bootstrap.rs` 전체:

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError, ZONE_LABEL};
use opl_peer::pki::{ClusterCa, NodeTls, cert_names};
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::host::same_host;
use opl_types::node::{FEATURES, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;
use crate::member::pki_dir;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            // 임시 파일에 쓰고 바꿔 넣어, 도중에 죽어도 반쯤 쓴 id가 남지 않게 한다.
            let tmp = data_dir.join(NODE_ID_TMP_FILE);
            let mut file = std::fs::File::create(&tmp)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 새 클러스터의 첫 기록: 클러스터 정보(이름, CA), 이 노드, `admin` 사용자. 돌려준 비밀번호는
/// 저장하지 않는다(해시만 남는다).
pub async fn create_cluster(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    name: &str,
    ca: &ClusterCa,
) -> anyhow::Result<String> {
    let info = Resource::new(
        name,
        ClusterSpec {
            ca_cert_pem: ca.cert_pem.clone(),
            ca_key_pem: ca.key_pem.clone(),
        },
    );
    cluster.create(&info).await?;
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 다시 시작한 노드의 역할(합류할 때 정해진 Node `spec.witness`)이 설정의 `witness`와 같은지 본다.
/// 로컬 저장소만 읽는다. Node 리소스가 아직 없으면 그대로 둔다.
pub fn check_role(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    match (node.spec.witness, config.witness) {
        (true, false) => anyhow::bail!(
            "this node joined as a witness but opld.toml says witness = false; leave the cluster \
             and join again to change the role"
        ),
        (false, true) => anyhow::bail!(
            "this node joined as a non-witness node but opld.toml says witness = true; leave the \
             cluster and join again to change the role"
        ),
        _ => Ok(()),
    }
}

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소와 zone 레이블(`[node] zone`이
/// 있을 때만)을 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    // 앞뒤 공백은 잘라 적는다(`" rack-a"`와 `"rack-a"`를 다른 zone으로 보지 않게).
    let zone = config.node.zone.as_deref().map(str::trim);
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
                    roles: None,
                },
            );
            node.id = id;
            if let Some(zone) = zone {
                node.labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            cluster.create(&node).await?;
        }
        Some(node)
            if node.spec.address != address
                || zone.is_some_and(|z| {
                    node.labels.get(ZONE_LABEL).map(String::as_str) != Some(z)
                }) =>
        {
            let mut labels = node.labels;
            if let Some(zone) = zone {
                labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, labels, &spec).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부, 버전, 업데이트 확인 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let underlay = config.underlay().await;
    record_node(cluster, config, host, underlay).await
}

/// `register_node`의 기록. `underlay`는 정한 underlay다(None이면 풀지 못한 것).
async fn record_node(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    underlay: Option<IpAddr>,
) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    let bridges = host.bridges().await;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let memory_mib = crate::host::memory_mib();
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            hypervisor: host.hypervisor_name(),
            // 브리지 목록을 얻지 못하면 기록된 목록을 그대로 둔다.
            bridges: bridges.clone().unwrap_or_else(|| status.bridges.clone()),
            arch: Some(host.profile.arch),
            // 풀 상태는 풀 새로 고침이 쓴다. 이미 적힌 것을 지우지 않는다.
            pools: std::mem::take(&mut status.pools),
            // 펜싱이 꺼져 있으면 None이라 리더가 이 노드의 HA VM을 옮기지 않는다.
            ha: config.ha.node_ha(),
            // 이름을 풀지 못하면(설정·IP 리터럴이 아님) 기록된 값을 지킨다(잠깐 DNS가 안 된다고
            // VXLAN 네트워크를 잃지 않게).
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
            // 업데이트 확인이 쓴 결과는 지우지 않는다. 꺼진 노드는 None이다(다시 확인을 받지 않는다).
            updates: config
                .updates
                .enabled()
                .then(|| status.updates.take().unwrap_or_default()),
            features: features(),
            cluster_addresses: announced_links(config),
        };
        true
    })
    .await
}

/// 이 빌드가 아는 기능([`FEATURES`]). 위트니스도 적는다(새 기능의 API는 모든 노드를 본다).
fn features() -> Vec<String> {
    FEATURES.iter().map(|f| f.to_string()).collect()
}

/// 알릴 클러스터 링크: 설정의 링크 가운데 호스트가 지금 노드 인증서의 이름에 있는 것. 다시 서명하지 못해 이름이
/// 빠진 링크로는 다른 노드가 TLS로 닿지 못하므로 알리지 않고 경고한다(빠진 링크가 바뀔 때만, `refresh_features`가
/// 주기적으로 부른다). 인증서를 읽지 못하면 모두 알린다.
pub(crate) fn announced_links(config: &Config) -> Vec<String> {
    let links = config.cluster_links();
    let names = match NodeTls::load(&pki_dir(config)) {
        Ok(Some(tls)) => cert_names(&tls.cert_pem).ok(),
        _ => None,
    };
    let Some(names) = names else {
        return links;
    };
    let (named, missing): (Vec<_>, Vec<_>) = config
        .cluster_hosts()
        .into_iter()
        .zip(links)
        .partition(|(host, _)| names.iter().any(|n| same_host(n, host)));
    let missing: Vec<_> = missing.into_iter().map(|(_, link)| link).collect();
    if newly_missing(&MISSING_LINKS, &missing) {
        tracing::warn!(
            ?missing,
            "the node certificate does not name these cluster links; not announcing them"
        );
    }
    named.into_iter().map(|(_, link)| link).collect()
}

/// 마지막으로 본 알리지 않는 링크([`announced_links`])
static MISSING_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 알리지 않는 링크 `missing`이 있고 `last`(지난번에 본 것)와 다르면 true다. `last`를 `missing`으로 바꾼다.
fn newly_missing(last: &Mutex<Vec<String>>, missing: &[String]) -> bool {
    let mut last = last.lock().unwrap();
    let changed = *last != missing;
    if changed {
        *last = missing.to_vec();
    }
    changed && !missing.is_empty()
}

/// 위트니스의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 아키텍처만 기록한다. VM을 돌리지
/// 않으므로 용량·하이퍼바이저·브리지·풀은 비운다.
pub async fn register_witness(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            arch: Some(Arch::native()),
            version: Some(BUILD.to_string()),
            features: features(),
            cluster_addresses: announced_links(config),
            ..NodeStatus::default()
        };
        true
    })
    .await
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았거나 목록을 얻지 못하면
/// 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let Some(bridges) = host.bridges().await else {
        return Ok(());
    };
    update_node_status(cluster, |status| {
        if status.bridges == bridges {
            return false;
        }
        tracing::info!(?bridges, "bridges changed");
        status.bridges = bridges.clone();
        true
    })
    .await
}

/// 기능 표시가 이 빌드의 목록과 다르면 고쳐 쓴다. 롤링 업그레이드 중 옛 버전 리더가 이 노드의 status를
/// 다시 쓰면(모르는 `features`를 빼고) 지워지므로 주기적으로 되살린다. 같으면 쓰지 않는다.
pub async fn refresh_features(cluster: &Cluster) -> anyhow::Result<()> {
    let features = features();
    update_node_status(cluster, |status| {
        if status.features == features {
            return false;
        }
        tracing::info!(?features, "restoring the recorded features");
        status.features = features.clone();
        true
    })
    .await
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let result = update_node_status(cluster, |status| {
        status.online = false;
        true
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

/// 이 노드의 status를 `change`로 고쳐 쓴다(`false`면 쓰지 않는다). status의 다른 필드는 다른
/// 작업(풀 새로 고침)도 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 다시 고친다.
async fn update_node_status(
    cluster: &Cluster,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        let mut status = node.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(id, node.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use async_trait::async_trait;
    use openraft::ServerState;
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_cluster::types::OrbNode;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeUpdates, PoolState};

    use crate::config::FencingSetting;

    /// 정해 둔 브리지를 돌려준다. `None`이면 `ip`가 실패한 것처럼 오류를 낸다.
    #[derive(Debug)]
    struct FakeNet(Option<Vec<BridgeInfo>>);

    #[async_trait]
    impl HostNet for FakeNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            self.0.clone().ok_or(DriverError::ToolFailed {
                tool: "ip",
                stderr: "boom".into(),
            })
        }
        async fn create_tap(&self, _: &Tap) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn delete_vm_taps(&self, _: Uuid) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn sync_overlays(
            &self,
            _: &str,
            _: &[Overlay],
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeFirewall;

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, _: &str) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        let members = BTreeMap::from([(id, OrbNode::single(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        (cluster, id)
    }

    /// address는 IP 리터럴이다(등록이 이름을 풀지 않게).
    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"10.0.0.1\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    /// 설정의 클러스터 링크(없으면 `address` 하나)를 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_cluster_links() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let links = || {
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            node.status.cluster_addresses
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.0.0.1:8007"]);
        config.cluster_addresses = vec!["10.1.0.1".into(), "10.0.0.1".into()];
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.1.0.1:8007", "10.0.0.1:8007"]);
        cluster.shutdown().await.unwrap();
    }

    /// 노드 인증서에 이름이 없는 링크(다시 서명 실패)는 알리지 않는다. 이름은 값으로 비교한다.
    #[tokio::test]
    async fn registration_announces_only_links_named_in_the_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        config.data_dir = dir.path().join("data");
        config.cluster_addresses = vec!["fd00::1".into(), "10.1.0.1".into(), "10.0.0.1".into()];
        let ca = ClusterCa::generate("t").unwrap();
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        let names = ["10.0.0.1".to_string(), "fd00:0::1".into()];
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id.0, &names).unwrap(),
            key_pem,
        }
        .save(&pki_dir(&config))
        .unwrap();
        assert_eq!(
            announced_links(&config),
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );

        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.cluster_addresses,
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 알리지 않는 링크의 경고는 그 집합이 바뀔 때만 한다(같은 것을 주기적으로 되풀이하지 않음).
    #[test]
    fn missing_links_are_warned_only_when_they_change() {
        let last = Mutex::new(Vec::new());
        let a = ["10.1.0.1:8007".to_string()];
        let ab = ["10.1.0.1:8007".to_string(), "10.2.0.1:8007".into()];
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
        assert!(!newly_missing(&last, &a));
        assert!(newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
    }

    /// `[ha]`의 펜싱을 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다. `off`면 없다.
    #[tokio::test]
    async fn registration_records_the_ha_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        config.ha.fencing = FencingSetting::Process;
        config.ha.fence_after_secs = 2;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        config.ha.fencing = FencingSetting::Off;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, None);
        cluster.shutdown().await.unwrap();
    }

    /// `[network] underlay`를, 없으면 `address`의 IP를 기록한다. 이름을 풀지 못하면(None) 기록된 값을
    /// 그대로 둔다(잠깐 DNS가 안 된다고 VM이 쓰는 VXLAN 장치를 지우지 않게).
    #[tokio::test]
    async fn registration_records_the_underlay() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let underlay = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .underlay
        };
        // 이름을 풀지 못한 새 노드에는 없다.
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), None);
        config.network.underlay = Some("10.1.0.5".parse().unwrap());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("10.1.0.5".parse().unwrap()));
        config.network.underlay = None;
        config.address = "fd00::5".into();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
    }

    /// `[node] zone`을 노드 레이블 `zone`으로 적는다. 다른 레이블은 그대로 두고, zone이 없으면 레이블을
    /// 건드리지 않는다(API로 둔 zone).
    #[tokio::test]
    async fn registration_records_the_zone_label() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let labels = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap().labels;
        config.node.zone = Some("rack-a".into());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels().get("zone").map(String::as_str), Some("rack-a"));
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        let mut other = node.labels.clone();
        other.insert("owner".into(), "ops".into());
        cluster
            .update_spec(id.0, None, other, &node.spec)
            .await
            .unwrap();
        // 앞뒤 공백은 잘라 적는다.
        config.node.zone = Some(" rack-b ".into());
        register_node(&cluster, &config, &host).await.unwrap();
        let got = labels();
        assert_eq!(got.get("zone").map(String::as_str), Some("rack-b"));
        assert_eq!(got.get("owner").map(String::as_str), Some("ops"));
        config.node.zone = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels(), got);
        cluster.shutdown().await.unwrap();
    }

    /// 방화벽(nftables)이 있는지 기록하고, 다시 등록하면 그때의 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let firewall = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .firewall
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        host.firewall = Some(Arc::new(FakeFirewall));
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(firewall());
        host.firewall = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        cluster.shutdown().await.unwrap();
    }

    /// 등록할 때 이 빌드의 버전과 기능을 쓴다(옛 버전으로 적힌 값도 바꾼다).
    #[tokio::test]
    async fn registration_records_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let version = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .version
        };
        let features = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .features
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
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
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            status.features.clear();
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
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
    #[tokio::test]
    async fn refresh_restores_features_erased_by_an_old_leader() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let node = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        let revision = node().revision;
        refresh_features(&cluster).await.unwrap();
        assert_eq!(node().revision, revision);
        // 옛 리더는 모르는 `features`를 빼고 status를 다시 쓴다.
        update_node_status(&cluster, |status| {
            status.features.clear();
            true
        })
        .await
        .unwrap();
        assert!(node().status.features.is_empty());
        refresh_features(&cluster).await.unwrap();
        assert_eq!(
            node().status.features,
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
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
    /// 꺼지면 지운다.
    #[tokio::test]
    async fn registration_records_whether_updates_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let updates = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .updates
        };
        config.updates.enabled = Some(true);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(NodeUpdates::default()));
        let checked = NodeUpdates {
            checked_at: Some(1_793_000_000),
            error: Some("apt-get update -q failed".into()),
            ..NodeUpdates::default()
        };
        update_node_status(&cluster, |status| {
            status.updates = Some(checked.clone());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(checked));
        config.updates.enabled = Some(false);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), None);
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        // underlay와 업데이트를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"\n\
             [updates]\nenabled = true",
        )
        .unwrap();
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.name, "w");
        assert!(node.spec.witness);
        assert_eq!(node.spec.address, "10.0.0.9:8006");
        assert_eq!(
            node.status,
            NodeStatus {
                online: true,
                arch: Some(Arch::native()),
                version: Some(BUILD.to_string()),
                features: vec![
                    "node-drain".into(),
                    "incremental-backup".into(),
                    "incremental-replication".into(),
                    "storage-migration".into(),
                    "metrics".into(),
                    "node-roles".into(),
                    "shared-sessions".into(),
                    "vip".into(),
                    "lease-liveness".into(),
                    "voter-limit".into(),
                    "observer".into(),
                ],
                cluster_addresses: vec!["10.0.0.9:8007".into()],
                ..NodeStatus::default()
            }
        );

        mark_offline(&cluster).await;
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.spec.witness);
        assert!(node.status.online);
        cluster.shutdown().await.unwrap();
    }

    /// 합류할 때 정한 역할과 설정의 `witness`가 다르면 시작하지 않는다. Node 리소스가 없으면 본다.
    #[tokio::test]
    async fn a_role_change_in_the_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let witness: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        let normal = test_config();
        check_role(&cluster, &witness).unwrap();
        check_role(&cluster, &normal).unwrap();

        register_witness(&cluster, &witness).await.unwrap();
        check_role(&cluster, &witness).unwrap();
        let err = check_role(&cluster, &normal).unwrap_err().to_string();
        assert!(
            err.contains("joined as a witness but opld.toml says witness = false"),
            "{err}"
        );

        let NodeId(id) = cluster.id();
        let node = cluster.get::<NodeSpec>(id).unwrap().unwrap();
        let spec = NodeSpec {
            witness: false,
            ..node.spec
        };
        cluster
            .update_spec(id, None, node.labels, &spec)
            .await
            .unwrap();
        check_role(&cluster, &normal).unwrap();
        let err = check_role(&cluster, &witness).unwrap_err().to_string();
        assert!(
            err.contains("joined as a non-witness node but opld.toml says witness = true"),
            "{err}"
        );
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config, "t").unwrap()
        };
        let br0 = vec![BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        let (ok, failing) = (host(Some(br0.clone())), host(None));
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .bridges
        };

        // 새 노드는 목록을 얻지 못하면 브리지가 없다.
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), vec![]);
        register_node(&cluster, &config, &ok).await.unwrap();
        assert_eq!(recorded(), br0);
        // 이미 기록된 브리지는 목록을 얻지 못해도 지우지 않는다.
        refresh_bridges(&cluster, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        refresh_bridges(&cluster, &host(Some(vec![])))
            .await
            .unwrap();
        assert_eq!(recorded(), vec![]);
        cluster.shutdown().await.unwrap();
    }

    /// 풀 새로 고침이 쓴 `pools`를 브리지 기록과 오프라인 표시가 지우지 않는다. 읽은 뒤 쓰기 전에 다른
    /// 쪽이 status를 써도 그 값을 지키며 다시 쓴다.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_status_writes_keep_pools_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let pools = BTreeMap::from([("local".to_string(), PoolState::default())]);
        // 처음 고칠 때 풀 새로 고침이 끼어든다.
        let mut first = true;
        update_node_status(&cluster, |status| {
            if std::mem::take(&mut first) {
                let mut meanwhile = status.clone();
                meanwhile.pools = pools.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(cluster.update_status::<NodeSpec>(id.0, &meanwhile))
                })
                .unwrap();
            }
            status.bridges = vec![BridgeInfo {
                name: "br0".into(),
                vlan_filtering: false,
            }];
            true
        })
        .await
        .unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.pools, pools);
        assert_eq!(node.status.bridges.len(), 1);

        mark_offline(&cluster).await;
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(!node.status.online);
        assert_eq!(node.status.pools, pools);

        // 다시 시작해 등록해도 풀 새로 고침이 먼저 쓴 pools를 지우지 않는다.
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.status.online);
        assert_eq!(node.status.pools, pools);
        cluster.shutdown().await.unwrap();
    }
}
```

`crates/opld/src/config.rs`에서

```rust
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
```

을 다음으로 바꾼다:

```rust
    pub cluster_listen: SocketAddr,
    /// 다른 노드가 이 노드의 노드 간 통신에 닿는 호스트들(IP나 DNS 이름, 포트 없이, 앞이 우선). 포트는
    /// `cluster_listen`의 포트다. 첫 링크가 끊기면 다음 링크로 넘어간다. 없거나 비면 `address` 하나다. 모두
    /// 노드 인증서에 들어간다.
    #[serde(default)]
    pub cluster_addresses: Vec<String>,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
```

`crates/opld/src/config.rs`에서

```rust

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
```

을 다음으로 바꾼다:

```rust

/// 다른 노드가 닿을 호스트(`key`의 값)인지: 포트 없는 IP나 호스트 이름. 0.0.0.0이나 ::은 안 된다. 규칙은 API 합류
/// 검사와 같다([`opl_types::host::valid_host`]).
fn check_host(key: &str, host: &str) -> anyhow::Result<()> {
    if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified()) {
        anyhow::bail!("{key} must not be an unspecified address (got {host:?})");
    }
    if !opl_types::host::valid_host(host) {
        anyhow::bail!("{key} must be an IP address or a host name without a port (got {host:?})");
    }
    Ok(())
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
```

`crates/opld/src/config.rs`에서

```rust
        opl_types::resource::validate_name(&self.name)?;
        let address = &self.address;
        let ok = !address.is_empty()
            && (address.parse::<IpAddr>().is_ok()
                || address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !ok {
            anyhow::bail!(
                "address must be an IP address or a host name without a port (got {address:?})"
            );
        }
        // 다른 노드가 닿을 주소이므로 0.0.0.0이나 ::은 안 된다.
        if address
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified())
        {
            anyhow::bail!("address must not be an unspecified address (got {address:?})");
        }
```

을 다음으로 바꾼다:

```rust
        opl_types::resource::validate_name(&self.name)?;
        check_host("address", &self.address)?;
        for (i, host) in self.cluster_addresses.iter().enumerate() {
            check_host("cluster_addresses", host)?;
            if self.cluster_addresses[..i]
                .iter()
                .any(|h| opl_types::host::same_host(h, host))
            {
                anyhow::bail!("cluster_addresses must not repeat a host (got {host:?} twice)");
            }
        }
```

`crates/opld/src/config.rs`에서

```rust

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }
```

을 다음으로 바꾼다:

```rust

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소): 첫 링크
    pub fn cluster_address(&self) -> String {
        with_port(self.cluster_hosts()[0], self.cluster_listen.port())
    }

    /// 노드 간 통신 링크(`host:port`, 앞이 우선): `cluster_addresses`, 없으면 `address` 하나
    pub fn cluster_links(&self) -> Vec<String> {
        self.cluster_hosts()
            .into_iter()
            .map(|host| with_port(host, self.cluster_listen.port()))
            .collect()
    }

    /// 링크의 호스트들(늘 하나 이상, [`cluster_links`](Self::cluster_links)와 같은 순서)
    pub fn cluster_hosts(&self) -> Vec<&str> {
        if self.cluster_addresses.is_empty() {
            vec![self.address.as_str()]
        } else {
            self.cluster_addresses.iter().map(String::as_str).collect()
        }
    }

    /// 링크가 여럿인데 `cluster_listen`이 한 주소만 들으면 시작할 때 낼 경고. 다른 링크로 오는 연결을 받지
    /// 못할 수 있다.
    pub fn links_warning(&self) -> Option<String> {
        (self.cluster_links().len() > 1 && !self.cluster_listen.ip().is_unspecified()).then(|| {
            format!(
                "cluster_addresses has several links but cluster_listen ({}) listens on one address; \
                 use 0.0.0.0 or :: to accept every link",
                self.cluster_listen
            )
        })
    }
```

`crates/opld/src/config.rs`에서

```rust

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
```

을 다음으로 바꾼다:

```rust

    /// 노드 인증서의 이름: 알리는 호스트, 모든 링크의 호스트, 같은 기계에서 쓰는 이름(겹친 것은 하나로)
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        let hosts = self.cluster_hosts();
        for name in hosts.into_iter().chain(["localhost", "127.0.0.1", "::1"]) {
            if !names.iter().any(|n| n == name) {
                names.push(name.into());
            }
```

`crates/opld/src/config.rs`에서

```rust
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }
```

을 다음으로 바꾼다:

```rust
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn cluster_links_default_to_the_address_and_keep_their_order() {
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.cluster_links(), ["10.0.0.5:8007"]);
        assert_eq!(c.links_warning(), None);
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"\ncluster_addresses = []");
        assert_eq!(c.cluster_links(), ["10.0.0.5:8007"]);
        // `address`가 목록에 없어도 된다. 첫 링크가 알리는 클러스터 주소다.
        let c = parse(
            "name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"0.0.0.0:9007\"\n\
             cluster_addresses = [\"10.1.0.5\", \"fd00::5\", \"10.0.0.5\"]",
        );
        c.validate().unwrap();
        assert_eq!(
            c.cluster_links(),
            ["10.1.0.5:9007", "[fd00::5]:9007", "10.0.0.5:9007"]
        );
        assert_eq!(c.cluster_address(), "10.1.0.5:9007");
        assert_eq!(c.api_address(), "node1.lan:8006");
        assert_eq!(c.links_warning(), None);
        assert_eq!(
            c.cert_names(),
            [
                "node1.lan",
                "10.1.0.5",
                "fd00::5",
                "10.0.0.5",
                "localhost",
                "127.0.0.1",
                "::1"
            ]
        );
        // 한 주소만 들으면 경고한다(막지는 않는다).
        let c = Config {
            cluster_listen: "10.1.0.5:9007".parse().unwrap(),
            ..c
        };
        c.validate().unwrap();
        assert!(c.links_warning().unwrap().contains("10.1.0.5:9007"));
    }

    #[test]
    fn cert_names_do_not_repeat_hosts() {
        let c = parse(
            "name = \"n\"\naddress = \"127.0.0.1\"\ncluster_addresses = [\"127.0.0.2\", \"127.0.0.1\"]",
        );
        assert_eq!(
            c.cert_names(),
            ["127.0.0.1", "127.0.0.2", "localhost", "::1"]
        );
    }

    #[test]
    fn validates_cluster_addresses() {
        let with = |hosts: &[&str]| Config {
            cluster_addresses: hosts.iter().map(|h| h.to_string()).collect(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with(&["10.0.0.5", "node1-b.lan", "::1"]).validate().is_ok());
        for bad in [
            &["10.0.0.5:8007"][..],
            &["[::1]"],
            &[""],
            &["a b"],
            &["0.0.0.0"],
            &["::"],
            &["10.0.0.5", "10.0.0.5"],
            // 같은 주소의 다른 표기, 대소문자만 다른 이름
            &["fd00::5", "fd00:0::5"],
            &["node1.lan", "NODE1.lan"],
            // API 합류가 받지 않는 호스트
            &["224.0.0.1"],
            &["node1.lan."],
            &["-a.lan"],
        ] {
            assert!(with(bad).validate().is_err(), "{bad:?}");
        }
    }
```

`crates/opld/src/join.rs`에서

```rust
const CA_MAX_BYTES: usize = 64 * 1024;
```

을 다음으로 바꾼다:

```rust
const CA_MAX_BYTES: usize = 64 * 1024;

/// 합류 요청에 실을 링크 목록: 링크가 둘 이상일 때만(링크 하나면 옛 요청 그대로라 옛 노드도 받는다)
fn join_links(config: &Config) -> Vec<String> {
    let links = config.cluster_links();
    if links.len() > 1 { links } else { Vec::new() }
}

/// 합류 거절 메시지. 옛 API 노드는 모르는 `cluster_addresses`(여러 링크로 합류)를 거절하므로 먼저 올리라고 알린다.
fn refusal(message: &str) -> String {
    if message.contains("unknown field") && message.contains("cluster_addresses") {
        format!(
            "the node at the join URL runs an older version that does not know several cluster links; \
             upgrade every node first ({message})"
        )
    } else {
        message.to_string()
    }
}
```

`crates/opld/src/join.rs`에서

```rust
            cluster_address: config.cluster_address(),
            csr_pem,
```

을 다음으로 바꾼다:

```rust
            cluster_address: config.cluster_address(),
            cluster_addresses: join_links(config),
            csr_pem,
```

`crates/opld/src/join.rs`에서

```rust
            "join refused: {}",
            body["message"].as_str().unwrap_or("unknown error")
        );
```

을 다음으로 바꾼다:

```rust
            "join refused: {}",
            refusal(body["message"].as_str().unwrap_or("unknown error"))
        );
```

`crates/opld/src/join.rs`에서

```rust
    })
}
```

을 다음으로 바꾼다:

```rust
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_sends_the_links_only_when_there_are_several() {
        let config: Config = toml::from_str("name = \"n\"\naddress = \"10.0.0.5\"").unwrap();
        assert!(join_links(&config).is_empty());
        let config = Config {
            cluster_addresses: vec!["10.0.0.5".into(), "10.1.0.5".into()],
            ..config
        };
        assert_eq!(join_links(&config), ["10.0.0.5:8007", "10.1.0.5:8007"]);
    }

    #[test]
    fn an_old_api_node_refusing_the_links_asks_to_upgrade() {
        let old =
            "invalid JSON body: unknown field `cluster_addresses`, expected one of `token`, `node`";
        assert!(
            refusal(old).contains("upgrade every node first"),
            "{}",
            refusal(old)
        );
        assert_eq!(
            refusal("invalid or expired join token"),
            "invalid or expired join token"
        );
    }
}
```

`crates/opld/src/main.rs`에서

```rust
) -> anyhow::Result<()> {
    prepare_data_dir(&config.data_dir)?;
```

을 다음으로 바꾼다:

```rust
) -> anyhow::Result<()> {
    if let Some(warning) = config.links_warning() {
        tracing::warn!("{warning}");
    }
    prepare_data_dir(&config.data_dir)?;
```

`crates/opld/src/member.rs`에서

```rust
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{ApiCert, TlsListener, api_server_config_with};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers, renew_liveness};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
```

을 다음으로 바꾼다:

```rust
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, cert_names, csr_for_key, fingerprint};
use opl_peer::tls::{ApiCert, TlsListener, api_server_config_with};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers, renew_liveness};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::cluster::ClusterSpec;
use opl_types::host::same_host;
use opl_types::node::NodeSpec;
```

`crates/opld/src/member.rs`에서

```rust
    config.data_dir.join(CLUSTER_DIR)
}
```

을 다음으로 바꾼다:

```rust
    config.data_dir.join(CLUSTER_DIR)
}

/// 노드 인증서에 설정의 이름([`Config::cert_names`], 모든 링크의 호스트)이 빠졌으면 저장소의 클러스터 CA로 같은
/// 키에 다시 서명해(옛 이름에 빠진 이름을 더해) `pki` 디렉터리에 저장하고 새 TLS 자료를 돌려준다. 빠진 것이
/// 없으면 `None`. 클러스터 CA를 아직 모르면(막 합류한 노드) 경고하고 `None`.
fn resign_node_cert(
    cluster: &Cluster,
    config: &Config,
    node: Uuid,
    tls: &NodeTls,
) -> anyhow::Result<Option<NodeTls>> {
    let mut names = cert_names(&tls.cert_pem)?;
    let missing: Vec<String> = config
        .cert_names()
        .into_iter()
        .filter(|want| !names.iter().any(|have| same_host(have, want)))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let Some(info) = cluster.list::<ClusterSpec>()?.1.into_iter().next() else {
        tracing::warn!(
            ?missing,
            "the node certificate lacks some cluster addresses but the cluster CA is not known yet"
        );
        return Ok(None);
    };
    let ca = ClusterCa {
        cert_pem: info.spec.ca_cert_pem,
        key_pem: info.spec.ca_key_pem,
    };
    names.extend(missing.iter().cloned());
    let cert_pem = ca.sign_node(&csr_for_key(&tls.key_pem)?, node, &names)?;
    NodeTls::save_cert(&pki_dir(config), &cert_pem)?;
    tracing::info!(added = ?missing, "node certificate signed again with the new names");
    Ok(Some(NodeTls {
        cert_pem,
        ..tls.clone()
    }))
}
```

`crates/opld/src/member.rs`에서

```rust
        peers.set_link_book(cluster.link_book());
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
```

을 다음으로 바꾼다:

```rust
        peers.set_link_book(cluster.link_book());
        // 설정에 더한 링크의 이름이 노드 인증서에 없으면 다시 서명해 그것으로 서버를 띄운다. 클라이언트 신원
        // (`peers`)은 옛 인증서 그대로다(이름이 아니라 CA와 CN의 노드 id로 확인된다).
        let tls = if matches!(bootstrap, Bootstrap::Restart) {
            match resign_node_cert(&cluster, config, node, &tls) {
                Ok(Some(new)) => new,
                Ok(None) => tls,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot sign the node certificate again for the new names");
                    tls
                }
            }
        } else {
            tls
        };
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
```

`crates/opld/src/member.rs`에서

```rust
                Bootstrap::Init { name, ca } => {
                    let me = OrbNode::single(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
```

을 다음으로 바꾼다:

```rust
                Bootstrap::Init { name, ca } => {
                    let me = OrbNode::new(config.cluster_links());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
```

`crates/opld/src/member.rs`에서

```rust

    /// 가입·재시작한 노드가 스스로 승격을 요청할지는 키퍼와 같은 규칙이다: 합류하자마자 죽어 등록하지 못한
```

을 다음으로 바꾼다:

```rust

    /// 링크를 더해 다시 시작하면 노드 인증서를 같은 키로 다시 서명해(옛 이름에 새 링크의 호스트를 더해) 저장하고,
    /// 그 뒤의 시작은 다시 서명하지 않는다.
    #[test]
    fn a_restart_with_new_links_signs_the_node_cert_again() {
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
            let old = tls.cert_pem.clone();
            let key_pem = tls.key_pem.clone();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (member, _) = Member::start(&config, node, tls, bootstrap, &rt, None)
                .await
                .unwrap();
            member.close(false).await.unwrap();
            assert_eq!(cert_names(&old).unwrap(), ["127.0.0.1", "localhost", "::1"]);

            let config = Config {
                cluster_addresses: vec!["127.0.0.2".into(), "127.0.0.1".into()],
                ..config
            };
            let restart = |config: Config| {
                let rt = rt.clone();
                async move {
                    let tls = NodeTls::load(&pki_dir(&config)).unwrap().unwrap();
                    let (member, _) =
                        Member::start(&config, node, tls, Bootstrap::Restart, &rt, None)
                            .await
                            .unwrap();
                    member.close(false).await.unwrap();
                    NodeTls::load(&pki_dir(&config)).unwrap().unwrap()
                }
            };
            let saved = restart(config.clone()).await;
            assert_eq!(
                cert_names(&saved.cert_pem).unwrap(),
                ["127.0.0.1", "localhost", "::1", "127.0.0.2"]
            );
            let der = opl_peer::pki::first_cert(&saved.cert_pem).unwrap();
            assert_eq!(opl_peer::pki::node_id_of(&der), Some(node));
            // 같은 키다(공개키가 같은 것은 opl-peer `pki` 시험이 본다).
            assert_eq!(saved.key_pem, key_pem);
            // 이름이 다 있으면 다시 서명하지 않는다.
            let again = restart(config.clone()).await;
            assert_eq!(again.cert_pem, saved.cert_pem);
            // 빠진 것처럼 닫아 root로 도는 Linux CI에서 만든 방화벽 테이블도 지운다.
            let tls = NodeTls::load(&pki_dir(&config)).unwrap().unwrap();
            let (member, _) = Member::start(&config, node, tls, Bootstrap::Restart, &rt, None)
                .await
                .unwrap();
            member.close(true).await.unwrap();
        });
    }

    /// 가입·재시작한 노드가 스스로 승격을 요청할지는 키퍼와 같은 규칙이다: 합류하자마자 죽어 등록하지 못한
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1926개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(cluster): 노드 설정·인증서·합류의 여러 링크 — cluster_addresses, 시작 때 인증서 다시 서명"
```

---

### Task 4: 링크 맞추기와 multi-link 기능

노드 기능 `multi-link`(`opl_cluster::MULTI_LINK`). 리더의 `VoterKeeper` 틱에서 바꿀 멤버십이 없고 살아 있는 투표 멤버가 과반이며 모든 구성원·관찰 노드가 `multi-link`를 알릴 때만, 한 틱에 한 노드씩: Raft 구성원의 `addrs`가 그 노드가 적은 `cluster_addresses`와 다르면 `change_membership(SetNodes, false)`(openraft 0.9.25에서 joint 없이 항목 하나), 관찰 노드면 새 `ClusterStatus.observer_links`를 고친다. 다른 노드와 겹치는 링크는 건너뛰고 경고. 관찰 노드 캐시·링크 책·`Members.links`에 관찰 노드의 실제 링크. `refresh_features`가 옛 리더가 지운 `cluster_addresses`도 되살린다. memnet 시험 `opl-cluster/tests/links.rs`.

**Files:**
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/follow.rs`
- Modify: `crates/opl-cluster/src/leader.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/voters.rs`
- Modify: `crates/opl-cluster/tests/links.rs`
- Modify: `crates/opl-cluster/tests/scale.rs`
- Modify: `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-types/src/cluster.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust
use axum::response::IntoResponse;
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_peer::pki::{ClusterCa, fingerprint};
```

을 다음으로 바꾼다:

```rust
use axum::response::IntoResponse;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{ClusterError, MULTI_LINK};
use opl_peer::pki::{ClusterCa, fingerprint};
```

`crates/opl-api/src/cluster.rs`에서

```rust
    if links.len() > 1 {
        let mut old = missing_feature(&state.cluster, "multi-link")?;
        old.retain(|name| *name != req.name);
        lacking_feature("multi-link", &old)?;
    }
```

을 다음으로 바꾼다:

```rust
    if links.len() > 1 {
        let mut old = missing_feature(&state.cluster, MULTI_LINK)?;
        old.retain(|name| *name != req.name);
        lacking_feature(MULTI_LINK, &old)?;
    }
```

`crates/opl-api/tests/cluster.rs`에서

```rust
        observers: [(node, "10.0.0.7:8007".to_string())].into(),
    };
```

을 다음으로 바꾼다:

```rust
        observers: [(node, "10.0.0.7:8007".to_string())].into(),
        ..ClusterStatus::default()
    };
```

`crates/opl-api/tests/cluster.rs`에서

```rust

/// 노드의 기록된 기능에 `feature`를 더한다(이 빌드가 아직 적지 않는 기능).
async fn add_feature(env: &Env, node: Uuid, feature: &str) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.features.push(feature.into());
    env.cluster
```

을 다음으로 바꾼다:

```rust

/// 노드의 기록된 기능에서 `feature`를 더하거나(`on`) 뺀다(옛 노드).
async fn set_feature(env: &Env, node: Uuid, feature: &str, on: bool) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.features.retain(|f| f != feature);
    if on {
        status.features.push(feature.into());
    }
    env.cluster
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    req.cluster_addresses = links.clone();
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
```

을 다음으로 바꾼다:

```rust
    req.cluster_addresses = links.clone();
    set_feature(&env, env.node, "multi-link", false).await;
    let (status, body) = anon.post("/cluster/join", json!(req.clone())).await;
```

`crates/opl-api/tests/cluster.rs`에서

```rust

    add_feature(&env, env.node, "multi-link").await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
```

을 다음으로 바꾼다:

```rust

    set_feature(&env, env.node, "multi-link", true).await;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
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
use openraft::{Raft, RaftMetrics, RaftNetworkFactory};
use opl_types::cluster::ClusterStatus;
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::follow::{self, FollowClient, FollowFrame, NodeRole};
use crate::health::{ApplyLag, QUORUM_ACK_MAX, RaftHealth};
use crate::leader::{self, Forwarder, LeaderCtx};
use crate::lease::{ConfirmBatch, LeaseTable, Leases};
use crate::links::{LINK_RETRY, LinkBook};
use crate::log_store::LogStore;
use crate::replica::{Replica, read_membership};
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
    /// 우선 링크가 아닌 동안 우선 링크를 다시 시도하는 간격(memnet이 쓴다, 기본 [`LINK_RETRY`]). 시험용이다.
    #[doc(hidden)]
    pub link_retry: Duration,
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
            link_retry: LINK_RETRY,
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
    /// 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`와 그 링크 `observer_links`). `Kind::Cluster`
    /// 변경으로 갱신한다.
    observers: Arc<RwLock<BTreeMap<NodeId, OrbNode>>>,
    /// 노드마다의 클러스터 링크(적용된 멤버십의 `OrbNode`와 관찰 노드 목록의 링크). 둘이 바뀔 때 함께 고친다.
    link_book: Arc<LinkBook>,
    /// memnet이 이 노드의 링크 선택에 쓰는 우선 링크 재시도 간격
    link_retry: Duration,
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

/// 지금 알고 있는 멤버십. 값은 노드의 Raft 주소(첫 링크)다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Members {
    pub leader: Option<NodeId>,
    pub voters: BTreeMap<NodeId, String>,
    pub learners: BTreeMap<NodeId, String>,
    /// Raft 밖의 관찰 노드(클러스터 정보의 `observers` 가운데 Raft 구성원이 아닌 것)
    pub observers: BTreeMap<NodeId, String>,
    /// 위 모든 노드의 클러스터 링크(앞이 우선, 첫 것이 위의 주소)
    pub links: BTreeMap<NodeId, Vec<String>>,
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
        let mut cluster = Self::assemble(&config, store, backend, start_raft);
        cluster.follow = follow;
        Ok(cluster)
    }

    fn assemble(
        config: &ClusterConfig,
        store: Store,
        backend: Backend,
        start_raft: StartRaft,
    ) -> Self {
        let id = config.node_id;
        let link_book = Arc::new(LinkBook::default());
        let observers = track_observers(store.db(), store.watch_hub(), &link_book);
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
            write_timeout: config.write_timeout,
            forwarder: None,
            start_raft,
            follow: None,
            transition: tokio::sync::Mutex::new(false),
            epoch: tokio::sync::watch::channel(0).0,
            apply_lag: Default::default(),
            observers,
            link_book,
            link_retry: config.link_retry,
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

    /// 노드마다의 클러스터 링크. 노드 간 연결(`Peers::set_link_book`)이 노드의 첫 링크로 링크 목록을 찾는다.
    pub fn link_book(&self) -> Arc<LinkBook> {
        self.link_book.clone()
    }

    pub(crate) fn link_retry(&self) -> Duration {
        self.link_retry
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
            links: BTreeMap::new(),
        };
        for (id, node) in membership.nodes() {
            let list = if voters.contains(id) {
                &mut out.voters
            } else {
                &mut out.learners
            };
            list.insert(*id, node.addr().to_string());
            out.links.insert(*id, node.addrs.clone());
        }
        // 내리는 중(목록에 쓰고 아직 멤버십에서 빼기 전)인 노드는 Raft 구성원으로 센다.
        for (id, node) in self.observers.read().unwrap().iter() {
            if membership.get_node(id).is_none() {
                out.observers.insert(*id, node.addr().to_string());
                out.links.insert(*id, node.addrs.clone());
            }
        }
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

/// 클러스터 정보의 관찰 노드 목록과 적용된 멤버십의 링크(`book`)를 읽어 두고, `Kind::Cluster` 변경과 멤버십
/// 변경을 따라 고치는 작업을 띄운다. 작업은 저장소가 닫히거나(피드가 `Closed`) 목록을 쥔 `Cluster`가 사라지면
/// 끝난다.
fn track_observers(
    db: &Arc<Database>,
    hub: &Arc<WatchHub>,
    book: &Arc<LinkBook>,
) -> Arc<RwLock<BTreeMap<NodeId, OrbNode>>> {
    let cache = Arc::new(RwLock::new(BTreeMap::new()));
    // 구독한 뒤 읽어야 그 사이의 변경을 놓치지 않는다.
    let feed = hub.feed(hub.revision()).ok();
    let mut members = Vec::new();
    read_links(db, &cache, book, &mut members);
    let (db, hub, weak, book) = (
        Arc::downgrade(db),
        Arc::downgrade(hub),
        Arc::downgrade(&cache),
        book.clone(),
    );
    tokio::spawn(async move {
        let mut feed = feed;
        loop {
            if let Some(mut f) = feed.take()
                && !follow_observers(&mut f, &weak, &book, &mut members).await
            {
                return;
            }
            // 처음이거나, 스냅샷 설치(`Reset`)·`Lagged` 뒤: 다시 구독하고 저장소에서 다시 읽는다.
            let (Some(db), Some(hub), Some(cache)) = (db.upgrade(), hub.upgrade(), weak.upgrade())
            else {
                return;
            };
            feed = hub.feed(hub.revision()).ok();
            read_links(&db, &cache, &book, &mut members);
            if feed.is_none() {
                tokio::time::sleep(RETRY_INTERVAL).await;
            }
        }
    });
    cache
}

/// 피드의 `Kind::Cluster` 변경으로 목록을, 멤버십 변경으로 멤버십의 노드(`members`)를 고치고 링크 책을 다시
/// 만든다. 다시 읽어야 하면(`Reset`·`Lagged`) `true`, 끝나야 하면(저장소가 닫힘, 목록이 사라짐) `false`다.
async fn follow_observers(
    feed: &mut Feed,
    cache: &Weak<RwLock<BTreeMap<NodeId, OrbNode>>>,
    book: &LinkBook,
    members: &mut Vec<OrbNode>,
) -> bool {
    loop {
        let batch = match feed.next().await {
            Ok(FeedItem::Batch(batch)) => batch,
            Ok(FeedItem::Reset { .. }) | Err(WatchError::Lagged | WatchError::Expired { .. }) => {
                return true;
            }
            Err(WatchError::Closed) => return false,
        };
        let Some(cache) = cache.upgrade() else {
            return false;
        };
        let mut changed = false;
        if let Some(m) = &batch.membership {
            *members = m
                .membership()
                .nodes()
                .map(|(_, node)| node.clone())
                .collect();
            changed = true;
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
            *cache.write().unwrap() = observers;
            changed = true;
        }
        if changed {
            fill_book(book, members, &cache.read().unwrap());
        }
    }
}

/// 저장소의 관찰 노드 목록과 적용된 멤버십으로 `cache`·`members`·`book`을 고친다. 멤버십을 읽지 못하면 앞의
/// `members`를 그대로 쓴다.
fn read_links(
    db: &Database,
    cache: &RwLock<BTreeMap<NodeId, OrbNode>>,
    book: &LinkBook,
    members: &mut Vec<OrbNode>,
) {
    match read_membership(db) {
        Ok(m) => {
            *members = m
                .membership()
                .nodes()
                .map(|(_, node)| node.clone())
                .collect()
        }
        Err(e) => tracing::warn!(error = %e, "cannot read the stored membership"),
    }
    let observers = read_observers(db);
    fill_book(book, members, &observers);
    *cache.write().unwrap() = observers;
}

/// 링크 책을 멤버십의 노드와 관찰 노드로 다시 만든다(둘에 다 있으면 멤버십의 것).
fn fill_book(book: &LinkBook, members: &[OrbNode], observers: &BTreeMap<NodeId, OrbNode>) {
    book.replace(observers.values().chain(members));
}

/// 저장소의 클러스터 정보에 적힌 관찰 노드
fn read_observers(db: &Database) -> BTreeMap<NodeId, OrbNode> {
    match list_objects(db, Kind::Cluster) {
        Ok((_, list)) => list.first().map(observers_of).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the cluster info");
            BTreeMap::new()
        }
    }
}

/// 클러스터 정보의 관찰 노드와 그 링크
pub(crate) fn observers_of(info: &RawResource) -> BTreeMap<NodeId, OrbNode> {
    match serde_json::from_value::<ClusterStatus>(info.status.clone()) {
        Ok(status) => observer_nodes(status),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the observers in the cluster info");
            BTreeMap::new()
        }
    }
}

/// 관찰 노드 목록의 노드 값: `observer_links`에 링크 목록이 있으면 그것, 없으면 `observers`의 주소 하나
pub(crate) fn observer_nodes(mut status: ClusterStatus) -> BTreeMap<NodeId, OrbNode> {
    status
        .observers
        .into_iter()
        .map(|(id, addr)| {
            let node = match status.observer_links.remove(&id) {
                Some(links) if !links.is_empty() => OrbNode::new(links),
                _ => OrbNode::single(addr),
            };
            (NodeId(id), node)
        })
        .collect()
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
```

`crates/opl-cluster/src/follow.rs`에서

```rust
    /// 지운 관찰 노드의 열린 스트림이 목록에서 빠지는 프레임부터 받지 않게 한다(목록 캐시의 갱신을 기다리지 않고).
    pub fn observers(&self) -> Option<BTreeMap<NodeId, String>> {
        let events = match self {
```

을 다음으로 바꾼다:

```rust
    /// 지운 관찰 노드의 열린 스트림이 목록에서 빠지는 프레임부터 받지 않게 한다(목록 캐시의 갱신을 기다리지 않고).
    pub fn observers(&self) -> Option<BTreeMap<NodeId, OrbNode>> {
        let events = match self {
```

`crates/opl-cluster/src/follow.rs`에서

```rust
                    .collect(),
            })
```

을 다음으로 바꾼다:

```rust
                    .collect(),
                ..Default::default()
            })
```

`crates/opl-cluster/src/follow.rs`에서

```rust
                .collect(),
        };
```

을 다음으로 바꾼다:

```rust
                .collect(),
            ..ClusterStatus::default()
        };
```

`crates/opl-cluster/src/leader.rs`에서

```rust

/// 클러스터 정보의 관찰 노드 목록에서 `id`를 뺀다. 목록에 없으면 `None`, 뺐으면 그 쓰기의 인덱스다. 다른 쓰기와
/// 겹치면(revision 불일치, 리더가 아직 적용하지 않은 쓰기) 다시 읽어 `deadline`까지 다시 한다.
```

을 다음으로 바꾼다:

```rust

/// 클러스터 정보의 관찰 노드 목록(과 그 링크)에서 `id`를 뺀다. 목록에 없으면 `None`, 뺐으면 그 쓰기의 인덱스다. 다른 쓰기와
/// 겹치면(revision 불일치, 리더가 아직 적용하지 않은 쓰기) 다시 읽어 `deadline`까지 다시 한다.
```

`crates/opl-cluster/src/leader.rs`에서

```rust
        }
        let command = Command::UpdateStatusIf {
```

을 다음으로 바꾼다:

```rust
        }
        status.observer_links.remove(&id.0);
        let command = Command::UpdateStatusIf {
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub use voters::{
    OBSERVER, STANDBY_LEARNERS, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
    leader_manages_voters,
};
```

을 다음으로 바꾼다:

```rust
pub use voters::{
    MULTI_LINK, OBSERVER, STANDBY_LEARNERS, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig,
    ZONE_LABEL, leader_manages_voters,
};
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
//!
//! 링크 맞추기(설계 §7): 바꿀 멤버십이 없으면 노드가 적은 클러스터 링크(`NodeStatus.cluster_addresses`)와 다른
//! 노드 하나를 고쳐 쓴다(Raft 구성원은 `SetNodes`, 관찰 노드는 목록의 `observer_links`). [`link_fix`]가 고른다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{ChangeMembers, LogId, Raft, RaftMetrics, ServerState};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::host::same_link;
use opl_types::node::{Node, NodeSpec};
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::{Cluster, ClusterError, observer_nodes};
use crate::types::{NodeId, OrbNode, TypeConfig};

/// 이 기능을 아는 노드는 투표 멤버 승격을 리더의 `VoterKeeper`에 맡긴다. 모든 노드가 알면 리더가 상한을 지킨다.
pub const VOTER_LIMIT: &str = "voter-limit";

/// 이 기능을 아는 노드는 관찰 노드가 될 수 있다(설계 §6). 모든 노드가 알 때만 키퍼가 learner를 관찰 노드로 내린다.
pub const OBSERVER: &str = "observer";

/// 이 기능을 아는 노드는 여러 클러스터 링크를 안다(설계 §7). 모든 노드가 알 때만 키퍼가 링크를 맞춘다.
pub const MULTI_LINK: &str = "multi-link";

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
    /// 다른 노드의 링크와 겹쳐 맞추지 않는다고 경고한 (노드, 그 링크를 가진 노드)
    link_clashes: BTreeSet<(NodeId, NodeId)>,
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
    let listed: BTreeMap<NodeId, OrbNode> = info
        .as_ref()
        .map(|i| observer_nodes(i.status.clone()))
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
        // 바꿀 멤버십이 없을 때만 링크를 맞춘다(살아 있는 과반이 없으면 쓰기가 끝나지 않는다).
        if alive_majority(&members, &voters) {
            sync_links(
                cluster,
                config,
                state,
                &m,
                info.as_ref(),
                &listed,
                &nodes,
                &name,
            )
            .await;
        }
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
            next.insert(id, node.clone());
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
    m: &RaftMetrics<NodeId, OrbNode>,
    info: &ClusterInfo,
    listed: &BTreeMap<NodeId, OrbNode>,
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
            .map(|(id, node)| (*id, node.clone()))
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

/// 관찰 노드를 목록의 링크 그대로 learner로 더한 뒤 목록에서 뺀다. 목록에서 빼지 못하면 다음 틱에 "목록에 있는
/// learner"로 보여 다시 관찰 노드가 된다(목록과 멤버십 어느 쪽에도 없는 노드는 생기지 않는다).
async fn return_observer(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    raft: &Raft<TypeConfig>,
    id: NodeId,
    listed: &BTreeMap<NodeId, OrbNode>,
    name: &impl Fn(NodeId) -> String,
) {
    let Some(node) = listed.get(&id) else {
        return;
    };
    let added = tokio::time::timeout(
        config.change_timeout,
        raft.add_learner(id, node.clone(), false),
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
    let mut next = observer_nodes(info.status.clone());
    next.remove(&id);
    match write_observers(cluster, &info, next).await {
        Ok(_) => tracing::info!("added the observer {} back as a learner", name(id)),
        Err(e) => tracing::warn!(error = %e, node = %name(id), "cannot unlist an observer"),
    }
}

/// 클러스터 정보의 관찰 노드 목록을 `observers`로 바꾼다(읽은 뒤 다른 쪽이 썼으면 `RevisionMismatch`). 링크가 둘
/// 이상인 관찰 노드만 `observer_links`에 링크 목록을 쓴다.
async fn write_observers(
    cluster: &Cluster,
    info: &ClusterInfo,
    observers: BTreeMap<NodeId, OrbNode>,
) -> Result<u64, ClusterError> {
    let status = ClusterStatus {
        observers: observers
            .iter()
            .map(|(id, node)| (id.0, node.addr().to_string()))
            .collect(),
        observer_links: observers
            .into_iter()
            .filter(|(_, node)| node.addrs.len() > 1)
            .map(|(id, node)| (id.0, node.addrs))
            .collect(),
    };
    cluster
        .update_status_checked::<ClusterSpec>(info.id, info.revision, &status)
        .await
}

/// 리더가 그 노드에 복제했다고 아는 마지막 로그 인덱스
fn matched(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> Option<u64> {
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
    m: &RaftMetrics<NodeId, OrbNode>,
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

/// 링크를 맞출 노드 하나(id 순으로 처음 것)와 그 노드의 새 값: 노드가 적은 링크(`NodeStatus.cluster_addresses`)가
/// `current`(멤버십과 관찰 노드 목록의 노드 값)와 다른 노드. 적은 링크가 비었으면(등록 전, 옛 기록) 그대로 둔다.
/// 모든 노드(위트니스·관찰 노드 포함)가 [`MULTI_LINK`]를 알릴 때만 고른다: 옛 노드는 링크 목록을 첫 링크 하나로
/// 읽고, 옛 리더는 멤버십을 다시 쓰며 링크를 떨어뜨린다. 첫 링크가 바뀌는 것(우선 링크를 새 망으로)도 고른다.
/// 적은 링크가 다른 노드의 링크와 겹치는 노드는 건너뛴다([`link_clashes`]).
pub(crate) fn link_fix(
    current: &BTreeMap<NodeId, OrbNode>,
    nodes: &BTreeMap<NodeId, Node>,
) -> Option<(NodeId, OrbNode)> {
    let ready = current.keys().all(|id| {
        nodes
            .get(id)
            .is_some_and(|n| n.status.features.iter().any(|f| f == MULTI_LINK))
    });
    if !ready {
        return None;
    }
    current.iter().find_map(|(id, node)| {
        let wanted = &nodes.get(id)?.status.cluster_addresses;
        (!wanted.is_empty() && *wanted != node.addrs && clash(*id, wanted, current).is_none())
            .then(|| (*id, OrbNode::new(wanted.clone())))
    })
}

/// 적은 링크가 다른 노드의 지금 링크와 겹치는 (노드, 그 링크를 가진 노드). 그대로 쓰면 그 다른 노드에 갈 Raft
/// RPC·리더 요청을 이 노드가 받게 된다(오타, 다시 쓴 IP).
pub(crate) fn link_clashes(
    current: &BTreeMap<NodeId, OrbNode>,
    nodes: &BTreeMap<NodeId, Node>,
) -> BTreeSet<(NodeId, NodeId)> {
    current
        .iter()
        .filter_map(|(id, node)| {
            let wanted = &nodes.get(id)?.status.cluster_addresses;
            if wanted.is_empty() || *wanted == node.addrs {
                return None;
            }
            Some((*id, clash(*id, wanted, current)?))
        })
        .collect()
}

/// `wanted`의 링크를 이미 쓰는 `id` 아닌 노드
fn clash(id: NodeId, wanted: &[String], current: &BTreeMap<NodeId, OrbNode>) -> Option<NodeId> {
    current.iter().find_map(|(other, node)| {
        (*other != id
            && node
                .addrs
                .iter()
                .any(|have| wanted.iter().any(|w| same_link(have, w))))
        .then_some(*other)
    })
}

/// [`link_fix`]가 고른 노드의 링크를 고쳐 쓴다: Raft 구성원은 노드 값만 바꾸는 `SetNodes`(투표 멤버가 그대로라
/// openraft 0.9.25는 joint 없이 한 항목으로 커밋한다), 관찰 노드는 목록(`observer_links`, 첫 링크가 바뀌면
/// `observers`의 값도). 앞선 멤버십 변경이 커밋 중이면(`Busy`) 다음 틱에 다시 본다.
#[allow(clippy::too_many_arguments)]
async fn sync_links(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    state: &mut State,
    m: &RaftMetrics<NodeId, OrbNode>,
    info: Option<&ClusterInfo>,
    listed: &BTreeMap<NodeId, OrbNode>,
    nodes: &BTreeMap<NodeId, Node>,
    name: &impl Fn(NodeId) -> String,
) {
    let membership = m.membership_config.membership();
    // 목록과 멤버십에 다 있으면(내려가는 중) 멤버십의 것
    let mut current = listed.clone();
    current.extend(membership.nodes().map(|(id, node)| (*id, node.clone())));
    // 겹치는 링크는 맞추지 않고 처음 볼 때 한 번 경고한다.
    let clashes = link_clashes(&current, nodes);
    for (id, other) in clashes.difference(&state.link_clashes) {
        tracing::warn!(
            node = %name(*id),
            other = %name(*other),
            "not updating the cluster links of a node: they include a link of another node"
        );
    }
    state.link_clashes = clashes;
    let Some((id, node)) = link_fix(&current, nodes) else {
        return;
    };
    let links = node.to_string();
    if membership.get_node(&id).is_none() {
        let Some(info) = info else {
            return;
        };
        let mut next = listed.clone();
        next.insert(id, node);
        match write_observers(cluster, info, next).await {
            Ok(_) => {
                tracing::info!(%links, "updated the cluster links of the observer {}", name(id))
            }
            Err(e) => tracing::warn!(error = %e, node = %name(id), "cannot update observer links"),
        }
        return;
    }
    // 계획은 장벽·목록 읽기 전의 지표로 세웠다(`tick`과 같이).
    let Some(raft) = cluster.raft() else {
        return;
    };
    if !same_config(cluster, m) {
        return;
    }
    let was_voter = membership.voter_ids().any(|v| v == id);
    let change = ChangeMembers::SetNodes(BTreeMap::from([(id, node)]));
    match change_members(&raft, change, config.change_timeout).await {
        Outcome::Done => {
            tracing::info!(%links, "updated the cluster links of {}", name(id));
            // `same_config`와 이 변경 사이에 노드 제거(`Remove`)가 커밋됐으면 openraft의 `SetNodes`(없는 노드는
            // 더한다)가 그 노드를 learner로 되살린다. 투표 멤버였는데 아니거나 Node 기록이 없으면 알리기만 한다.
            let now_voter = raft
                .metrics()
                .borrow()
                .membership_config
                .membership()
                .voter_ids()
                .any(|v| v == id);
            let recorded = matches!(cluster.get::<NodeSpec>(id.0), Ok(Some(_)));
            if (was_voter && !now_voter) || !recorded {
                tracing::warn!(
                    node = %name(id),
                    "updated the cluster links of a node that was removed meanwhile; it may be back as a learner"
                );
            }
        }
        Outcome::Busy => {}
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, node = %name(id), "cannot update the cluster links")
        }
    }
}

/// 지금도 리더이고 멤버십 구성이 `m`(계획을 세운 지표)의 것 그대로다.
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, OrbNode>) -> bool {
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
    changes: ChangeMembers<NodeId, OrbNode>,
    timeout: Duration,
) -> Outcome {
    outcome(tokio::time::timeout(timeout, raft.change_membership(changes, true)).await)
}

type MembershipResult = Result<
    Result<ClientWriteResponse<TypeConfig>, RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>>,
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
fn caught_up(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> bool {
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
        assert!(opl_types::node::FEATURES.contains(&MULTI_LINK));
    }

    /// 노드 `n`의 Node 기록: 적은 링크 `links`, 기능 `multi-link`
    fn linked(n: u64, links: &[&str]) -> (NodeId, Node) {
        let (id, mut node) = node(n, &[VOTER_LIMIT, MULTI_LINK], Some("0.3.0"));
        node.status.cluster_addresses = links.iter().map(|l| l.to_string()).collect();
        (id, node)
    }

    /// 적은 링크가 다른 노드의 링크(같은 주소의 다른 표기 포함)와 겹치는 노드는 맞추지 않고 다음 노드를 고른다.
    #[test]
    fn link_fix_skips_links_of_other_nodes() {
        let current: BTreeMap<NodeId, OrbNode> = BTreeMap::from([
            (id(1), OrbNode::single("10.0.0.1:8007")),
            (id(2), OrbNode::single("[fd00::2]:8007")),
            (id(3), OrbNode::single("10.0.0.3:8007")),
        ]);
        let nodes: BTreeMap<NodeId, Node> = [
            linked(1, &["10.0.0.1:8007", "[fd00:0::2]:8007"]),
            linked(2, &["[fd00::2]:8007"]),
            linked(3, &["10.0.0.3:8007", "10.1.0.3:8007"]),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            link_fix(&current, &nodes),
            Some((id(3), OrbNode::new(["10.0.0.3:8007", "10.1.0.3:8007"])))
        );
        assert_eq!(
            link_clashes(&current, &nodes),
            BTreeSet::from([(id(1), id(2))])
        );
    }

    /// 링크 맞추기는 적은 링크가 노드 값과 다른 노드를 한 번에 하나(id 순) 고르고, 적은 링크가 비었으면 그대로
    /// 두며, `multi-link`를 모르는 노드가 하나라도 있으면(관찰 노드 포함) 아무것도 고르지 않는다.
    #[test]
    fn link_fix_picks_one_node_at_a_time() {
        let mut current: BTreeMap<NodeId, OrbNode> = BTreeMap::from([
            (id(1), OrbNode::single("a1")),
            (id(2), OrbNode::single("a2")),
            (id(3), OrbNode::new(["a3", "b3"])),
        ]);
        let mut nodes: BTreeMap<NodeId, Node> = [
            linked(1, &["a1"]),
            linked(2, &["a2", "b2"]),
            linked(3, &["a3", "b3"]),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            link_fix(&current, &nodes),
            Some((id(2), OrbNode::new(["a2", "b2"])))
        );
        nodes.extend([linked(1, &["a1", "b1"])]);
        assert_eq!(
            link_fix(&current, &nodes),
            Some((id(1), OrbNode::new(["a1", "b1"])))
        );
        current.insert(id(1), OrbNode::new(["a1", "b1"]));
        current.insert(id(2), OrbNode::new(["a2", "b2"]));
        assert_eq!(link_fix(&current, &nodes), None);
        // 순서를 바꾸면 첫 링크도 바꾼다.
        nodes.extend([linked(3, &["b3", "a3"])]);
        assert_eq!(
            link_fix(&current, &nodes),
            Some((id(3), OrbNode::new(["b3", "a3"])))
        );
        // 적은 링크가 없으면(등록 전, 옛 기록) 그대로 둔다.
        nodes.extend([linked(3, &[])]);
        assert_eq!(link_fix(&current, &nodes), None);
        // 관찰 노드 4가 기능을 모르면(또는 Node 기록이 없으면) 아무것도 고르지 않는다.
        nodes.extend([linked(1, &["c1"])]);
        current.insert(id(4), OrbNode::single("a4"));
        assert_eq!(link_fix(&current, &nodes), None);
        nodes.extend([node(4, &[VOTER_LIMIT], Some("0.2.0"))]);
        assert_eq!(link_fix(&current, &nodes), None);
        nodes.extend([linked(4, &["a4"])]);
        assert_eq!(
            link_fix(&current, &nodes),
            Some((id(1), OrbNode::single("c1")))
        );
    }
}
```

`crates/opl-cluster/tests/links.rs`에서

```rust
//! 사이의 우선 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드의 Follow가 둘째 링크로 이어지며, 우선 링크를 풀면
//! 재시도 간격 뒤 다시 우선 링크를 쓴다.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{Cluster, ClusterConfig, ClusterError};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
//! 사이의 우선 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드의 Follow가 둘째 링크로 이어지며, 우선 링크를 풀면
//! 재시도 간격 뒤 다시 우선 링크를 쓴다. 리더의 키퍼는 노드가 적은 링크로 멤버십과 관찰 노드 목록의 링크를
//! 맞춘다.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openraft::ChangeMembers;
use opl_cluster::memnet::MemRouter;
use opl_cluster::store::{NodeMode, StoreError};
use opl_cluster::types::{NodeId, OrbNode};
use opl_cluster::{
    Cluster, ClusterConfig, ClusterError, MULTI_LINK, OBSERVER, RoleKeeper, RoleKeeperConfig,
    VOTER_LIMIT, VoterKeeper, VoterKeeperConfig,
};
use opl_types::cluster::{ClusterSpec, ClusterStatus};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

`crates/opl-cluster/tests/links.rs`에서

```rust
use tempfile::TempDir;
use tokio::time::Instant;
```

을 다음으로 바꾼다:

```rust
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tokio::time::Instant;
```

`crates/opl-cluster/tests/links.rs`에서

```rust
        c.shutdown().await.unwrap();
    }
}
```

을 다음으로 바꾼다:

```rust
        c.shutdown().await.unwrap();
    }
}

// ---- 링크 맞추기: 리더의 `VoterKeeper`가 노드가 적은 링크(`NodeStatus.cluster_addresses`)로 멤버십과 관찰 노드
// 목록의 링크를 고친다.

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

/// 노드 하나: 저장소, 리스 갱신(`renewing`이 참일 때), VoterKeeper, RoleKeeper
struct Kept {
    cluster: Arc<Cluster>,
    renewing: Arc<AtomicBool>,
    renew: JoinHandle<()>,
    voters: VoterKeeper,
    roles: RoleKeeper,
}

/// 노드 1~3은 첫 링크 하나(`n{i}-a`)로 만든 투표 멤버이고 모든 기능과 `cluster_addresses = [n{i}-a]`를 적는다.
struct Keepers {
    router: MemRouter,
    root: TempDir,
    nodes: BTreeMap<NodeId, Kept>,
}

impl Keepers {
    async fn start() -> Self {
        let mut h = Keepers {
            router: MemRouter::default(),
            root: TempDir::new().unwrap(),
            nodes: BTreeMap::new(),
        };
        for i in 1..=3 {
            h.open(i).await;
        }
        let voters = (1..=3)
            .map(|i| (id(i), OrbNode::single(link(i, "a"))))
            .collect();
        h.get(1).initialize(voters).await.unwrap();
        eventually("a leader", || async { h.leader().is_some() }).await;
        let info = Resource::new(
            "c",
            ClusterSpec {
                ca_cert_pem: "CERT".into(),
                ca_key_pem: "KEY".into(),
            },
        );
        h.leader_cluster().create(&info).await.unwrap();
        for i in 1..=3 {
            h.register(i, &[link(i, "a")], true).await;
        }
        h
    }

    async fn open(&mut self, i: u64) {
        let node = id(i);
        let mut config = ClusterConfig::new(node, self.root.path().join(i.to_string()));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        config.write_timeout = WAIT;
        config.link_retry = RETRY;
        let cluster = Cluster::open_node(
            config,
            self.router.network(node),
            self.router.follower(node),
        )
        .await
        .unwrap();
        let cluster = Arc::new(cluster.with_forwarder(self.router.forwarder(node)));
        self.router.register(node, &cluster);
        let renewing = Arc::new(AtomicBool::new(true));
        let renew = {
            let (cluster, renewing) = (cluster.clone(), renewing.clone());
            tokio::spawn(async move {
                loop {
                    if renewing.load(Ordering::SeqCst) {
                        let _ = cluster.renew_lease(RENEW * 2).await;
                    }
                    tokio::time::sleep(RENEW).await;
                }
            })
        };
        let roles = RoleKeeperConfig {
            interval: Duration::from_millis(100),
            stalled_after: Duration::from_secs(2),
        };
        self.nodes.insert(
            node,
            Kept {
                voters: VoterKeeper::spawn(cluster.clone(), keeper_config()),
                roles: RoleKeeper::spawn(cluster.clone(), roles),
                cluster,
                renewing,
                renew,
            },
        );
    }

    fn get(&self, i: u64) -> &Arc<Cluster> {
        &self.nodes[&id(i)].cluster
    }

    fn leader(&self) -> Option<NodeId> {
        self.nodes.values().find_map(|n| {
            let m = n.cluster.raft()?.metrics().borrow().clone();
            (m.state == openraft::ServerState::Leader).then_some(n.cluster.id())
        })
    }

    fn leader_cluster(&self) -> &Arc<Cluster> {
        &self.nodes[&self.leader().expect("a leader")].cluster
    }

    /// 리더의 멤버십에 있는 노드의 링크(없으면 빈 목록)
    fn links_of(&self, i: u64) -> Vec<String> {
        let Some(raft) = self.leader().and_then(|l| self.nodes[&l].cluster.raft()) else {
            return Vec::new();
        };
        let m = raft.metrics().borrow().clone();
        m.membership_config
            .membership()
            .get_node(&id(i))
            .map(|n| n.addrs.clone())
            .unwrap_or_default()
    }

    /// 리더 저장소의 클러스터 정보 status
    fn cluster_status(&self) -> ClusterStatus {
        self.leader_cluster().list::<ClusterSpec>().unwrap().1[0]
            .status
            .clone()
    }

    /// 노드 `i`의 Node 기록: 적은 링크 `links`, `multi_link`면 그 기능도
    async fn register(&self, i: u64, links: &[String], multi_link: bool) {
        let mut features = vec![VOTER_LIMIT.to_string(), OBSERVER.into()];
        if multi_link {
            features.push(MULTI_LINK.into());
        }
        let status = NodeStatus {
            online: true,
            features,
            cluster_addresses: links.to_vec(),
            ..NodeStatus::default()
        };
        let leader = self.leader_cluster();
        match leader.get::<NodeSpec>(id(i).0).unwrap() {
            Some(_) => {
                leader
                    .update_status::<NodeSpec>(id(i).0, &status)
                    .await
                    .unwrap();
            }
            None => {
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
                node.status = status;
                leader.create(&node).await.unwrap();
            }
        }
    }

    async fn close(mut self) {
        for (_, n) in std::mem::take(&mut self.nodes) {
            n.renew.abort();
            n.roles.shutdown().await;
            n.voters.shutdown().await;
            n.cluster.shutdown().await.unwrap();
        }
    }
}

/// 노드가 링크를 더하면 리더가 멤버십을 고치고(그 뒤 첫 링크를 끊어도 된다), 순서를 바꾸면 첫 링크도 바뀐다. 옛
/// 리더가 링크를 떨어뜨린 멤버십(`SetNodes`로 링크 하나)도 다시 맞춘다.
#[tokio::test]
async fn the_leader_adopts_the_links_the_nodes_report() {
    let h = Keepers::start().await;
    let two = |i: u64| vec![link(i, "a"), link(i, "b")];
    // 적은 링크가 멤버십과 같으면 아무것도 하지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    for i in 1..=3 {
        assert_eq!(h.links_of(i), vec![link(i, "a")]);
    }
    for i in 1..=3 {
        h.register(i, &two(i), true).await;
    }
    eventually("every member on two links", || async {
        (1..=3).all(|i| h.links_of(i) == two(i))
    })
    .await;
    // 모든 노드가 새 링크를 링크 책에서 찾는다.
    eventually("the link books", || async {
        (1..=3).all(|n| (1..=3).all(|i| h.get(n).link_book().links(&link(i, "a")) == two(i)))
    })
    .await;
    assert_eq!(h.get(2).members().links[&id(3)], two(3));

    // 이제 모든 노드 사이의 우선 링크를 끊어도 쓰기가 된다.
    for from in 1..=3 {
        for to in 1..=3 {
            h.router.cut_link(id(from), &link(to, "a"));
        }
    }
    let leader = h.leader().unwrap();
    let follower = (1..=3).find(|i| id(*i) != leader).unwrap();
    h.get(follower).create(&volume("cut")).await.unwrap();
    for from in 1..=3 {
        h.router.heal(id(from));
    }

    // 순서를 바꾸면 첫 링크(멤버 목록의 주소)도 바뀐다.
    h.register(3, &[link(3, "b"), link(3, "a")], true).await;
    eventually("node 3 prefers its second network", || async {
        h.links_of(3) == vec![link(3, "b"), link(3, "a")]
    })
    .await;
    eventually("the new address", || async {
        h.get(1).members().addr(id(3)) == Some(link(3, "b").as_str())
    })
    .await;

    // 옛 리더처럼 링크 하나로 다시 쓴 멤버십을 다시 맞춘다(앞선 변경이 아직 커밋 중이면 다시 한다).
    let raft = h.leader_cluster().raft().unwrap();
    let old = ChangeMembers::SetNodes(BTreeMap::from([(id(2), OrbNode::single(link(2, "a")))]));
    let deadline = Instant::now() + WAIT;
    while let Err(e) = raft.change_membership(old.clone(), false).await {
        assert!(Instant::now() < deadline, "{e}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    eventually("node 2 back on two links", || async {
        h.links_of(2) == two(2)
    })
    .await;
    h.get(2).create(&volume("after")).await.unwrap();
    h.close().await;
}

/// `multi-link`를 알리지 않는 노드가 하나라도 있으면 링크를 맞추지 않는다.
#[tokio::test]
async fn links_wait_until_every_node_knows_the_feature() {
    let h = Keepers::start().await;
    h.register(3, &[link(3, "a")], false).await;
    h.register(1, &[link(1, "a"), link(1, "b")], true).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.links_of(1), vec![link(1, "a")]);
    h.register(3, &[link(3, "a")], true).await;
    eventually("node 1 on two links", || async {
        h.links_of(1) == vec![link(1, "a"), link(1, "b")]
    })
    .await;
    h.close().await;
}

/// 관찰 노드의 링크는 클러스터 정보의 `observer_links`에 쓰고, 관찰 노드를 learner로 되돌리면 그 링크로 더한다.
#[tokio::test]
async fn observer_links_live_in_the_cluster_info() {
    let mut h = Keepers::start().await;
    h.open(4).await;
    let four = vec![link(4, "a"), link(4, "b")];
    // 4는 링크 하나로 learner가 된 뒤 목록에 올라 관찰 노드가 된다. 리스를 갱신하지 않는 동안은 건강하지 않아
    // learner로 되돌리지 않는다.
    h.nodes[&id(4)].renewing.store(false, Ordering::SeqCst);
    h.register(4, &[link(4, "a")], true).await;
    h.leader_cluster()
        .add_learner(id(4), OrbNode::single(link(4, "a")))
        .await
        .unwrap();
    let info = h.leader_cluster().list::<ClusterSpec>().unwrap().1[0].clone();
    let status = ClusterStatus {
        observers: BTreeMap::from([(id(4).0, link(4, "a"))]),
        ..ClusterStatus::default()
    };
    h.leader_cluster()
        .update_status::<ClusterSpec>(info.id, &status)
        .await
        .unwrap();
    eventually("node 4 an observer", || async {
        h.get(4).mode() == NodeMode::Observer && h.links_of(4).is_empty()
    })
    .await;
    // 관찰 노드가 링크를 더하면 키퍼가 목록에 쓰고(첫 링크는 그대로), 모든 노드가 그것을 안다.
    h.register(4, &four, true).await;
    eventually("the observer links", || async {
        h.cluster_status().observer_links.get(&id(4).0) == Some(&four)
    })
    .await;
    assert_eq!(h.cluster_status().observers[&id(4).0], link(4, "a"));
    eventually("the links of the observer", || async {
        (1..=4).all(|n| {
            h.get(n).members().links.get(&id(4)) == Some(&four)
                && h.get(n).link_book().links(&link(4, "a")) == four
        })
    })
    .await;

    // 리스를 갱신하면 투표 멤버가 상한보다 적어 learner로 돌아온다(목록의 링크 그대로). 목록의 링크도 빠진다.
    // 링크 맞추기가 고친 것이 아니게 4가 기능을 내린 채로 본다.
    h.register(4, &four, false).await;
    h.nodes[&id(4)].renewing.store(true, Ordering::SeqCst);
    eventually("node 4 back in Raft on two links", || async {
        h.links_of(4) == four && h.cluster_status() == ClusterStatus::default()
    })
    .await;
    h.close().await;
}
```

`crates/opl-cluster/tests/scale.rs`에서

```rust
            .collect(),
    };
```

을 다음으로 바꾼다:

```rust
            .collect(),
        ..ClusterStatus::default()
    };
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
/// 클러스터 정보의 관찰 노드(`ClusterStatus.observers`)는 멤버 목록에 들어가 주소를 찾을 수 있다. Raft 구성원과
/// 겹치면(내리는 중) 구성원으로 센다. 다시 연 노드는 저장소에서 바로 읽는다.
#[tokio::test]
```

을 다음으로 바꾼다:

```rust
/// 클러스터 정보의 관찰 노드(`ClusterStatus.observers`)는 멤버 목록에 들어가 주소를 찾을 수 있다. Raft 구성원과
/// 겹치면(내리는 중) 구성원으로 센다. 관찰 노드의 링크(`observer_links`)는 링크 책과 `Members.links`에 들어간다.
/// 다시 연 노드는 저장소에서 바로 읽는다.
#[tokio::test]
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    info.status.observers = BTreeMap::from([(observer.0, "node-9".into()), (me.0, "stale".into())]);
    c.update_status::<ClusterSpec>(info.id, &info.status)
```

을 다음으로 바꾼다:

```rust
    info.status.observers = BTreeMap::from([(observer.0, "node-9".into()), (me.0, "stale".into())]);
    let links = vec!["node-9".to_string(), "node-9b".to_string()];
    info.status.observer_links = BTreeMap::from([(observer.0, links.clone())]);
    c.update_status::<ClusterSpec>(info.id, &info.status)
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    assert_eq!(members.observers.len(), 1);
    assert_eq!(
```

을 다음으로 바꾼다:

```rust
    assert_eq!(members.observers.len(), 1);
    assert_eq!(members.links[&observer], links);
    assert_eq!(c.link_book().links("node-9"), links);
    assert_eq!(
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    assert_eq!(c.members().addr(observer), Some("node-9"));
```

을 다음으로 바꾼다:

```rust
    assert_eq!(c.members().addr(observer), Some("node-9"));
    assert_eq!(c.link_book().links("node-9"), links);
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    assert_eq!(c.members().addr(observer), None);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(c.members().addr(observer), None);
    assert_eq!(c.link_book().links("node-9"), ["node-9"]);
}
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
            observers: [(n4.0, "node-4".to_string())].into(),
        };
```

을 다음으로 바꾼다:

```rust
            observers: [(n4.0, "node-4".to_string())].into(),
            ..ClusterStatus::default()
        };
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            .collect(),
    };
```

을 다음으로 바꾼다:

```rust
            .collect(),
        ..ClusterStatus::default()
    };
```

`crates/opl-types/src/cluster.rs`에서

```rust
    pub observers: BTreeMap<Uuid, String>,
}
```

을 다음으로 바꾼다:

```rust
    pub observers: BTreeMap<Uuid, String>,
    /// 링크가 둘 이상인 관찰 노드의 클러스터 링크(앞이 우선, 첫 것이 `observers`의 값). 없는 관찰 노드는
    /// `observers`의 주소 하나다. 옛 노드는 모르는 필드를 무시하고 첫 링크만 쓴다.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub observer_links: BTreeMap<Uuid, Vec<String>>,
}
```

`crates/opl-types/src/cluster.rs`에서

```rust
        let id = Uuid::from_u128(7);
        let status = ClusterStatus {
            observers: BTreeMap::from([(id, "node-7:8007".to_string())]),
        };
        let json = serde_json::to_string(&status).unwrap();
```

을 다음으로 바꾼다:

```rust
        let id = Uuid::from_u128(7);
        let mut status = ClusterStatus {
            observers: BTreeMap::from([(id, "node-7:8007".to_string())]),
            ..ClusterStatus::default()
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("observer_links"), "{json}");
        assert_eq!(
            serde_json::from_str::<ClusterStatus>(&json).unwrap(),
            status
        );
        // 링크가 둘 이상인 관찰 노드만 링크 목록을 더 쓴다.
        status.observer_links = BTreeMap::from([(
            id,
            vec!["node-7:8007".to_string(), "node-7b:8007".to_string()],
        )]);
        let json = serde_json::to_string(&status).unwrap();
```

`crates/opl-types/src/node.rs`에서

```rust
    "observer",
];
```

을 다음으로 바꾼다:

```rust
    "observer",
    "multi-link",
];
```

`crates/opl-types/src/node.rs`에서

```rust
                "voter-limit",
                "observer"
            ])
```

을 다음으로 바꾼다:

```rust
                "voter-limit",
                "observer",
                "multi-link"
            ])
```

`crates/opld/src/bootstrap.rs` 전체:

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError, ZONE_LABEL};
use opl_peer::pki::{ClusterCa, NodeTls, cert_names};
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::host::same_host;
use opl_types::node::{FEATURES, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;
use crate::member::pki_dir;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            // 임시 파일에 쓰고 바꿔 넣어, 도중에 죽어도 반쯤 쓴 id가 남지 않게 한다.
            let tmp = data_dir.join(NODE_ID_TMP_FILE);
            let mut file = std::fs::File::create(&tmp)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 새 클러스터의 첫 기록: 클러스터 정보(이름, CA), 이 노드, `admin` 사용자. 돌려준 비밀번호는
/// 저장하지 않는다(해시만 남는다).
pub async fn create_cluster(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    name: &str,
    ca: &ClusterCa,
) -> anyhow::Result<String> {
    let info = Resource::new(
        name,
        ClusterSpec {
            ca_cert_pem: ca.cert_pem.clone(),
            ca_key_pem: ca.key_pem.clone(),
        },
    );
    cluster.create(&info).await?;
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 다시 시작한 노드의 역할(합류할 때 정해진 Node `spec.witness`)이 설정의 `witness`와 같은지 본다.
/// 로컬 저장소만 읽는다. Node 리소스가 아직 없으면 그대로 둔다.
pub fn check_role(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    match (node.spec.witness, config.witness) {
        (true, false) => anyhow::bail!(
            "this node joined as a witness but opld.toml says witness = false; leave the cluster \
             and join again to change the role"
        ),
        (false, true) => anyhow::bail!(
            "this node joined as a non-witness node but opld.toml says witness = true; leave the \
             cluster and join again to change the role"
        ),
        _ => Ok(()),
    }
}

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소와 zone 레이블(`[node] zone`이
/// 있을 때만)을 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    // 앞뒤 공백은 잘라 적는다(`" rack-a"`와 `"rack-a"`를 다른 zone으로 보지 않게).
    let zone = config.node.zone.as_deref().map(str::trim);
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
                    roles: None,
                },
            );
            node.id = id;
            if let Some(zone) = zone {
                node.labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            cluster.create(&node).await?;
        }
        Some(node)
            if node.spec.address != address
                || zone.is_some_and(|z| {
                    node.labels.get(ZONE_LABEL).map(String::as_str) != Some(z)
                }) =>
        {
            let mut labels = node.labels;
            if let Some(zone) = zone {
                labels.insert(ZONE_LABEL.into(), zone.to_string());
            }
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, labels, &spec).await?;
        }
        Some(_) => {}
    }
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부, 버전, 업데이트 확인 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let underlay = config.underlay().await;
    record_node(cluster, config, host, underlay).await
}

/// `register_node`의 기록. `underlay`는 정한 underlay다(None이면 풀지 못한 것).
async fn record_node(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    underlay: Option<IpAddr>,
) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    let bridges = host.bridges().await;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let memory_mib = crate::host::memory_mib();
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            hypervisor: host.hypervisor_name(),
            // 브리지 목록을 얻지 못하면 기록된 목록을 그대로 둔다.
            bridges: bridges.clone().unwrap_or_else(|| status.bridges.clone()),
            arch: Some(host.profile.arch),
            // 풀 상태는 풀 새로 고침이 쓴다. 이미 적힌 것을 지우지 않는다.
            pools: std::mem::take(&mut status.pools),
            // 펜싱이 꺼져 있으면 None이라 리더가 이 노드의 HA VM을 옮기지 않는다.
            ha: config.ha.node_ha(),
            // 이름을 풀지 못하면(설정·IP 리터럴이 아님) 기록된 값을 지킨다(잠깐 DNS가 안 된다고
            // VXLAN 네트워크를 잃지 않게).
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
            // 업데이트 확인이 쓴 결과는 지우지 않는다. 꺼진 노드는 None이다(다시 확인을 받지 않는다).
            updates: config
                .updates
                .enabled()
                .then(|| status.updates.take().unwrap_or_default()),
            features: features(),
            cluster_addresses: announced_links(config),
        };
        true
    })
    .await
}

/// 이 빌드가 아는 기능([`FEATURES`]). 위트니스도 적는다(새 기능의 API는 모든 노드를 본다).
fn features() -> Vec<String> {
    FEATURES.iter().map(|f| f.to_string()).collect()
}

/// 알릴 클러스터 링크: 설정의 링크 가운데 호스트가 지금 노드 인증서의 이름에 있는 것. 다시 서명하지 못해 이름이
/// 빠진 링크로는 다른 노드가 TLS로 닿지 못하므로 알리지 않고 경고한다(빠진 링크가 바뀔 때만, `refresh_features`가
/// 주기적으로 부른다). 인증서를 읽지 못하면 모두 알린다.
pub(crate) fn announced_links(config: &Config) -> Vec<String> {
    let links = config.cluster_links();
    let names = match NodeTls::load(&pki_dir(config)) {
        Ok(Some(tls)) => cert_names(&tls.cert_pem).ok(),
        _ => None,
    };
    let Some(names) = names else {
        return links;
    };
    let (named, missing): (Vec<_>, Vec<_>) = config
        .cluster_hosts()
        .into_iter()
        .zip(links)
        .partition(|(host, _)| names.iter().any(|n| same_host(n, host)));
    let missing: Vec<_> = missing.into_iter().map(|(_, link)| link).collect();
    if newly_missing(&MISSING_LINKS, &missing) {
        tracing::warn!(
            ?missing,
            "the node certificate does not name these cluster links; not announcing them"
        );
    }
    named.into_iter().map(|(_, link)| link).collect()
}

/// 마지막으로 본 알리지 않는 링크([`announced_links`])
static MISSING_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// 알리지 않는 링크 `missing`이 있고 `last`(지난번에 본 것)와 다르면 true다. `last`를 `missing`으로 바꾼다.
fn newly_missing(last: &Mutex<Vec<String>>, missing: &[String]) -> bool {
    let mut last = last.lock().unwrap();
    let changed = *last != missing;
    if changed {
        *last = missing.to_vec();
    }
    changed && !missing.is_empty()
}

/// 위트니스의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 아키텍처만 기록한다. VM을 돌리지
/// 않으므로 용량·하이퍼바이저·브리지·풀은 비운다.
pub async fn register_witness(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            arch: Some(Arch::native()),
            version: Some(BUILD.to_string()),
            features: features(),
            cluster_addresses: announced_links(config),
            ..NodeStatus::default()
        };
        true
    })
    .await
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았거나 목록을 얻지 못하면
/// 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let Some(bridges) = host.bridges().await else {
        return Ok(());
    };
    update_node_status(cluster, |status| {
        if status.bridges == bridges {
            return false;
        }
        tracing::info!(?bridges, "bridges changed");
        status.bridges = bridges.clone();
        true
    })
    .await
}

/// 기능 표시와 클러스터 링크가 이 빌드의 목록·설정과 다르면 고쳐 쓴다. 롤링 업그레이드 중 옛 버전 리더가 이
/// 노드의 status를 다시 쓰면(모르는 `features`·`cluster_addresses`를 빼고) 지워지므로 주기적으로 되살린다. 같으면
/// 쓰지 않는다.
pub async fn refresh_features(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let features = features();
    let links = announced_links(config);
    update_node_status(cluster, |status| {
        if status.features == features && status.cluster_addresses == links {
            return false;
        }
        tracing::info!(?features, ?links, "restoring the recorded features");
        status.features = features.clone();
        status.cluster_addresses = links.clone();
        true
    })
    .await
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let result = update_node_status(cluster, |status| {
        status.online = false;
        true
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

/// 이 노드의 status를 `change`로 고쳐 쓴다(`false`면 쓰지 않는다). status의 다른 필드는 다른
/// 작업(풀 새로 고침)도 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 다시 고친다.
async fn update_node_status(
    cluster: &Cluster,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        let mut status = node.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(id, node.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use async_trait::async_trait;
    use openraft::ServerState;
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_cluster::types::OrbNode;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeUpdates, PoolState};

    use crate::config::FencingSetting;

    /// 정해 둔 브리지를 돌려준다. `None`이면 `ip`가 실패한 것처럼 오류를 낸다.
    #[derive(Debug)]
    struct FakeNet(Option<Vec<BridgeInfo>>);

    #[async_trait]
    impl HostNet for FakeNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            self.0.clone().ok_or(DriverError::ToolFailed {
                tool: "ip",
                stderr: "boom".into(),
            })
        }
        async fn create_tap(&self, _: &Tap) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn delete_vm_taps(&self, _: Uuid) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn sync_overlays(
            &self,
            _: &str,
            _: &[Overlay],
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeFirewall;

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, _: &str) -> Result<(), DriverError> {
            Ok(())
        }
    }

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        let members = BTreeMap::from([(id, OrbNode::single(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .unwrap()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        (cluster, id)
    }

    /// address는 IP 리터럴이다(등록이 이름을 풀지 않게).
    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"10.0.0.1\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    /// 설정의 클러스터 링크(없으면 `address` 하나)를 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_cluster_links() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let links = || {
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            node.status.cluster_addresses
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.0.0.1:8007"]);
        config.cluster_addresses = vec!["10.1.0.1".into(), "10.0.0.1".into()];
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(links(), ["10.1.0.1:8007", "10.0.0.1:8007"]);
        cluster.shutdown().await.unwrap();
    }

    /// 노드 인증서에 이름이 없는 링크(다시 서명 실패)는 알리지 않는다. 이름은 값으로 비교한다.
    #[tokio::test]
    async fn registration_announces_only_links_named_in_the_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        config.data_dir = dir.path().join("data");
        config.cluster_addresses = vec!["fd00::1".into(), "10.1.0.1".into(), "10.0.0.1".into()];
        let ca = ClusterCa::generate("t").unwrap();
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        let names = ["10.0.0.1".to_string(), "fd00:0::1".into()];
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id.0, &names).unwrap(),
            key_pem,
        }
        .save(&pki_dir(&config))
        .unwrap();
        assert_eq!(
            announced_links(&config),
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );

        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.cluster_addresses,
            ["[fd00::1]:8007", "10.0.0.1:8007"]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 알리지 않는 링크의 경고는 그 집합이 바뀔 때만 한다(같은 것을 주기적으로 되풀이하지 않음).
    #[test]
    fn missing_links_are_warned_only_when_they_change() {
        let last = Mutex::new(Vec::new());
        let a = ["10.1.0.1:8007".to_string()];
        let ab = ["10.1.0.1:8007".to_string(), "10.2.0.1:8007".into()];
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
        assert!(!newly_missing(&last, &a));
        assert!(newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &ab));
        assert!(!newly_missing(&last, &[]));
        assert!(newly_missing(&last, &a));
    }

    /// `[ha]`의 펜싱을 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다. `off`면 없다.
    #[tokio::test]
    async fn registration_records_the_ha_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        config.ha.fencing = FencingSetting::Process;
        config.ha.fence_after_secs = 2;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        config.ha.fencing = FencingSetting::Off;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, None);
        cluster.shutdown().await.unwrap();
    }

    /// `[network] underlay`를, 없으면 `address`의 IP를 기록한다. 이름을 풀지 못하면(None) 기록된 값을
    /// 그대로 둔다(잠깐 DNS가 안 된다고 VM이 쓰는 VXLAN 장치를 지우지 않게).
    #[tokio::test]
    async fn registration_records_the_underlay() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let underlay = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .underlay
        };
        // 이름을 풀지 못한 새 노드에는 없다.
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), None);
        config.network.underlay = Some("10.1.0.5".parse().unwrap());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("10.1.0.5".parse().unwrap()));
        config.network.underlay = None;
        config.address = "fd00::5".into();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
    }

    /// `[node] zone`을 노드 레이블 `zone`으로 적는다. 다른 레이블은 그대로 두고, zone이 없으면 레이블을
    /// 건드리지 않는다(API로 둔 zone).
    #[tokio::test]
    async fn registration_records_the_zone_label() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let labels = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap().labels;
        config.node.zone = Some("rack-a".into());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels().get("zone").map(String::as_str), Some("rack-a"));
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        let mut other = node.labels.clone();
        other.insert("owner".into(), "ops".into());
        cluster
            .update_spec(id.0, None, other, &node.spec)
            .await
            .unwrap();
        // 앞뒤 공백은 잘라 적는다.
        config.node.zone = Some(" rack-b ".into());
        register_node(&cluster, &config, &host).await.unwrap();
        let got = labels();
        assert_eq!(got.get("zone").map(String::as_str), Some("rack-b"));
        assert_eq!(got.get("owner").map(String::as_str), Some("ops"));
        config.node.zone = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(labels(), got);
        cluster.shutdown().await.unwrap();
    }

    /// 방화벽(nftables)이 있는지 기록하고, 다시 등록하면 그때의 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let firewall = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .firewall
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        host.firewall = Some(Arc::new(FakeFirewall));
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(firewall());
        host.firewall = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        cluster.shutdown().await.unwrap();
    }

    /// 등록할 때 이 빌드의 버전과 기능을 쓴다(옛 버전으로 적힌 값도 바꾼다).
    #[tokio::test]
    async fn registration_records_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let version = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .version
        };
        let features = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .features
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link"
            ]
        );
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            status.features.clear();
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(
            features(),
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 옛 버전 리더가 status를 다시 쓰며 지운 기능 표시와 클러스터 링크를 주기 갱신이 되살린다. 같으면 쓰지 않는다.
    #[tokio::test]
    async fn refresh_restores_features_erased_by_an_old_leader() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let node = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        let revision = node().revision;
        refresh_features(&cluster, &config).await.unwrap();
        assert_eq!(node().revision, revision);
        // 옛 리더는 모르는 `features`·`cluster_addresses`를 빼고 status를 다시 쓴다.
        update_node_status(&cluster, |status| {
            status.features.clear();
            status.cluster_addresses.clear();
            true
        })
        .await
        .unwrap();
        assert!(node().status.features.is_empty());
        refresh_features(&cluster, &config).await.unwrap();
        assert_eq!(node().status.cluster_addresses, config.cluster_links());
        assert!(!node().status.cluster_addresses.is_empty());
        assert_eq!(
            node().status.features,
            [
                "node-drain",
                "incremental-backup",
                "incremental-replication",
                "storage-migration",
                "metrics",
                "node-roles",
                "shared-sessions",
                "vip",
                "lease-liveness",
                "voter-limit",
                "observer",
                "multi-link"
            ]
        );
        cluster.shutdown().await.unwrap();
    }

    /// 주기 갱신도 노드 인증서에 이름이 있는 링크만 되살린다.
    #[tokio::test]
    async fn refresh_restores_only_links_named_in_the_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        config.data_dir = dir.path().join("data");
        config.cluster_addresses = vec!["10.0.0.1".into(), "10.1.0.1".into()];
        let ca = ClusterCa::generate("t").unwrap();
        let (key_pem, csr) = opl_peer::pki::node_key_and_csr().unwrap();
        NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id.0, &["10.0.0.1".into()]).unwrap(),
            key_pem,
        }
        .save(&pki_dir(&config))
        .unwrap();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        update_node_status(&cluster, |status| {
            status.cluster_addresses = vec!["10.0.0.1:8007".into(), "10.1.0.1:8007".into()];
            true
        })
        .await
        .unwrap();
        refresh_features(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.cluster_addresses, ["10.0.0.1:8007"]);
        cluster.shutdown().await.unwrap();
    }

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
    /// 꺼지면 지운다.
    #[tokio::test]
    async fn registration_records_whether_updates_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let updates = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .updates
        };
        config.updates.enabled = Some(true);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(NodeUpdates::default()));
        let checked = NodeUpdates {
            checked_at: Some(1_793_000_000),
            error: Some("apt-get update -q failed".into()),
            ..NodeUpdates::default()
        };
        update_node_status(&cluster, |status| {
            status.updates = Some(checked.clone());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(checked));
        config.updates.enabled = Some(false);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), None);
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        // underlay와 업데이트를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"\n\
             [updates]\nenabled = true",
        )
        .unwrap();
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.name, "w");
        assert!(node.spec.witness);
        assert_eq!(node.spec.address, "10.0.0.9:8006");
        assert_eq!(
            node.status,
            NodeStatus {
                online: true,
                arch: Some(Arch::native()),
                version: Some(BUILD.to_string()),
                features: vec![
                    "node-drain".into(),
                    "incremental-backup".into(),
                    "incremental-replication".into(),
                    "storage-migration".into(),
                    "metrics".into(),
                    "node-roles".into(),
                    "shared-sessions".into(),
                    "vip".into(),
                    "lease-liveness".into(),
                    "voter-limit".into(),
                    "observer".into(),
                    "multi-link".into(),
                ],
                cluster_addresses: vec!["10.0.0.9:8007".into()],
                ..NodeStatus::default()
            }
        );

        mark_offline(&cluster).await;
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.spec.witness);
        assert!(node.status.online);
        cluster.shutdown().await.unwrap();
    }

    /// 합류할 때 정한 역할과 설정의 `witness`가 다르면 시작하지 않는다. Node 리소스가 없으면 본다.
    #[tokio::test]
    async fn a_role_change_in_the_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let witness: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        let normal = test_config();
        check_role(&cluster, &witness).unwrap();
        check_role(&cluster, &normal).unwrap();

        register_witness(&cluster, &witness).await.unwrap();
        check_role(&cluster, &witness).unwrap();
        let err = check_role(&cluster, &normal).unwrap_err().to_string();
        assert!(
            err.contains("joined as a witness but opld.toml says witness = false"),
            "{err}"
        );

        let NodeId(id) = cluster.id();
        let node = cluster.get::<NodeSpec>(id).unwrap().unwrap();
        let spec = NodeSpec {
            witness: false,
            ..node.spec
        };
        cluster
            .update_spec(id, None, node.labels, &spec)
            .await
            .unwrap();
        check_role(&cluster, &normal).unwrap();
        let err = check_role(&cluster, &witness).unwrap_err().to_string();
        assert!(
            err.contains("joined as a non-witness node but opld.toml says witness = true"),
            "{err}"
        );
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config, "t").unwrap()
        };
        let br0 = vec![BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        let (ok, failing) = (host(Some(br0.clone())), host(None));
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .bridges
        };

        // 새 노드는 목록을 얻지 못하면 브리지가 없다.
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), vec![]);
        register_node(&cluster, &config, &ok).await.unwrap();
        assert_eq!(recorded(), br0);
        // 이미 기록된 브리지는 목록을 얻지 못해도 지우지 않는다.
        refresh_bridges(&cluster, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        refresh_bridges(&cluster, &host(Some(vec![])))
            .await
            .unwrap();
        assert_eq!(recorded(), vec![]);
        cluster.shutdown().await.unwrap();
    }

    /// 풀 새로 고침이 쓴 `pools`를 브리지 기록과 오프라인 표시가 지우지 않는다. 읽은 뒤 쓰기 전에 다른
    /// 쪽이 status를 써도 그 값을 지키며 다시 쓴다.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_status_writes_keep_pools_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let pools = BTreeMap::from([("local".to_string(), PoolState::default())]);
        // 처음 고칠 때 풀 새로 고침이 끼어든다.
        let mut first = true;
        update_node_status(&cluster, |status| {
            if std::mem::take(&mut first) {
                let mut meanwhile = status.clone();
                meanwhile.pools = pools.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(cluster.update_status::<NodeSpec>(id.0, &meanwhile))
                })
                .unwrap();
            }
            status.bridges = vec![BridgeInfo {
                name: "br0".into(),
                vlan_filtering: false,
            }];
            true
        })
        .await
        .unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.pools, pools);
        assert_eq!(node.status.bridges.len(), 1);

        mark_offline(&cluster).await;
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(!node.status.online);
        assert_eq!(node.status.pools, pools);

        // 다시 시작해 등록해도 풀 새로 고침이 먼저 쓴 pools를 지우지 않는다.
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.status.online);
        assert_eq!(node.status.pools, pools);
        cluster.shutdown().await.unwrap();
    }
}
```

`crates/opld/src/member.rs`에서

```rust
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
```

을 다음으로 바꾼다:

```rust
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot record features");
```

`crates/opld/src/member.rs`에서

```rust
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
```

을 다음으로 바꾼다:

```rust
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot record features");
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1932개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 링크 맞추기와 multi-link 기능 — 리더가 노드가 적은 링크로 멤버십·관찰 노드 목록을 고침"
```

---

### Task 5: 다중 링크 API·웹·실제 프로세스 시험과 문서

API `MemberView.cluster_addresses`(모든 링크), 웹 클러스터 화면은 클러스터 주소 아래 둘째 링크부터. opld 실제 프로세스 시험: 세 노드가 우선 링크 127.0.0.2(macOS는 응답 없음, Linux는 거절)·둘째 127.0.0.1로 클러스터를 이루고 리더를 죽여도 쓰기가 이어짐, 링크를 더해 다시 시작하면 인증서와 멤버십이 바뀜. 연결 제한 시간 2초(쓰기 전달 시도 3초 안에 연결 실패가 나도록). README "클러스터 통신 다중 링크"(업그레이드 주의), 설계 §16 "구현(계획 49)".

**Files:**
- Modify: `README.md`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/health.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opld/tests/cluster.rs`
- Modify: `docs/superpowers/specs/2026-11-11-availability-scale-design.md`
- Modify: `web/src/cluster/ClusterPage.test.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/pages/DashboardPage.test.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
  노드로 가는 연결은 둘로 나눈다: Raft·리스 갱신·리더 전달·생존 확인은 `core`의 연결, 콘솔·마이그레이션 보내기·UEFI
  변수·감사 로그·지표 읽기는 `work`의 연결이다(처음 연 쪽 실행기와 관계없이). 대량 전송용 포트를 따로 두는 것은
  다중 링크(계획 49)에서 다룬다.
- **systemd watchdog.** 유닛(`opld.service`)은 `Type=notify`, `NotifyAccess=main`, `WatchdogSec=30s`,
```

을 다음으로 바꾼다:

```markdown
  노드로 가는 연결은 둘로 나눈다: Raft·리스 갱신·리더 전달·생존 확인은 `core`의 연결, 콘솔·마이그레이션 보내기·UEFI
  변수·감사 로그·지표 읽기는 `work`의 연결이다(처음 연 쪽 실행기와 관계없이). 노드 간 연결을 여러 망으로 잇는
  것은 아래 [클러스터 통신 다중 링크](#클러스터-통신-다중-링크)(계획 49)다. 대량 전송용 포트를 따로 두는 것은 아직 없다.
- **systemd watchdog.** 유닛(`opld.service`)은 `Type=notify`, `NotifyAccess=main`, `WatchdogSec=30s`,
```

`README.md`에서

```markdown
  줄 서기 때문, 노드 300에서 67 ms). 실제 노드의 쓰기 지연에는 그 노드 디스크의 fsync 시간이 더해진다.
```

을 다음으로 바꾼다:

````markdown
  줄 서기 때문, 노드 300에서 67 ms). 실제 노드의 쓰기 지연에는 그 노드 디스크의 fsync 시간이 더해진다.

## 클러스터 통신 다중 링크

노드 사이의 모든 통신(`cluster_listen`으로 가는 Raft·리스 갱신·쓰기 전달·관찰 노드의 Follow·콘솔·마이그레이션 준비)을
망 여럿으로 잇는다. 한 망(스위치·케이블·NIC)이 끊겨도 다른 망으로 클러스터가 그대로 돈다([설계 문서](docs/superpowers/specs/2026-11-11-availability-scale-design.md)
§7·§16).

```toml
# /etc/opl/opld.toml (노드마다)
address = "10.0.0.11"                           # API 주소와 인증서 이름(그대로)
cluster_listen = "0.0.0.0:8007"                 # 모든 링크에서 받게
cluster_addresses = ["10.0.0.11", "10.1.0.11"]  # 앞이 우선 링크. 호스트만(IP나 DNS 이름), 포트는 cluster_listen의 것
```

- **값.** 없거나 비면 `address` 하나다(계획 49 전과 같다). 호스트만 적고 포트·중복(같은 IP의 다른 표기, 대소문자만 다른
  이름 포함)은 거절한다. 호스트 규칙은 합류 API와 같다(특수 주소·끝의 점·와일드카드 안 됨). `address`도 같은 규칙이다.
  `address`가 목록에 없어도
  된다(예: API는 관리망, 노드 사이는 전용망 둘). `cluster_listen`은 모든 링크에서 받아야 하므로 `0.0.0.0`이나 `::`로
  둔다. 한 주소로 두고 링크가 둘 이상이면 시작할 때 경고만 남긴다.
- **넘어가기.** 노드마다 첫 링크(우선 링크)를 쓴다. 연결이 안 되면 곧바로, 답이 늦으면 그 링크에서 연속 두 번이거나 그
  링크의 마지막 성공에서 400 ms가 지났을 때 다음 링크로 넘어간다(하트비트 하나가 늦은 살아 있는 링크는 버리지 않는다.
  조용히 죽은 링크에서 마지막 성공 뒤 첫 하트비트가 다음 링크로 가기까지 약 600 ms로, 선거 제한 시간(1~2초)에 걸리지
  않는다; 리스 갱신은 3초 제한 시간 한 번). 연결하지 못한 호출은 같은 호출 안에서 다음 링크로 한 번 더 보낸다(보내지
  않았으므로 안전하다; 답이 늦은 호출, 이미 보낸 뒤 연결이 끊긴 호출은 상대가 받았을 수 있어 다시 보내지 않는다). 모두 안 되면 돌아가며 시도한다. 우선 링크가 아닌 동안
  30초마다 짧은 노드 간 호출(Raft·ping·리스 갱신·Follow 열기) 하나를 우선 링크로 보내 보고, 성공하면 돌아간다(그동안 다른
  호출은 지금 링크로; 콘솔·마이그레이션·쓰기 같은 크거나 긴 호출은 늘 지금 링크로). 넘어가면 `warn` 로그("cluster link failed;
  switching to the next link"), 돌아오면 `info` 로그를 남긴다. 링크 상태는 아직 헬스·지표·웹에 없다(나중에 관리망 검사와
  함께). 링크마다 따로 TCP 연결이라, 죽은 연결 위에서 이미 기다리던 긴 요청(투표 멤버 승격의 따라잡기 같은)은 keepalive
  (30초)까지 걸릴 수 있다(다음 호출은 새 링크로 간다).
- **인증서.** 노드 인증서에 `address`, 모든 `cluster_addresses` 호스트, localhost 셋이 들어간다(만들기·합류). 기존 노드에
  링크를 더하고 다시 시작하면 노드가 클러스터 CA로 같은 키에 인증서를 다시 서명해 쓴다("node certificate signed again
  with the new names" — 나갔다 다시 합류할 필요가 없다). 다시 서명하지 못해 인증서에 이름이 없는 링크는 클러스터에
  알리지 않는다(경고 "the node certificate does not name these cluster links").
- **잘못된 링크.** 다른 노드의 주소를 링크로 적으면(오타, 다시 쓴 IP) 그 노드에 갈 요청을 이 노드가 받게 된다. 그래서
  다른 노드의 링크와 겹치는 링크로는 합류하지 못하고(400), 리더도 그런 링크로 멤버십을 고치지 않는다(경고 "not updating
  the cluster links of a node"). 노드 간 호출은 가려는 노드의 id(`orbivisor-target` 메타데이터)를 실어 보내고, 받은 노드가
  다른 노드면 거절해 보내는 쪽이 다음 링크로 넘어간다(옛 노드는 싣지 않으므로 그대로 받는다).
- **보기.** `GET /api/v1/cluster`의 `members[].cluster_addresses`(모든 링크, 첫 것이 `cluster_address`)와
  `orbivisorctl -o json cluster status`에 보인다(표는 우선 링크만). 웹 클러스터 화면은 둘째 링크부터 클러스터 주소 아래
  작게 보인다.
- **마이그레이션.** VM 메모리·디스크 스트림은 대상 노드에 지금 쓰는 링크의 호스트로 간다(우선 링크가 죽었어도 옮긴다).
  그래서 `[migration] ports`를 모든 링크의 망에서 연다.
- **업그레이드.** 노드 기능 `multi-link`다. 모든 노드(위트니스·관찰 노드 포함)가 이 버전이 되기 전에는 링크가 멤버십에
  들어가지 않아 첫 링크 하나로 돈다(옛 노드는 여러 링크 항목을 첫 링크 하나로 읽는다). 모든 노드가 알리면 리더가 1초마다
  노드 하나씩 멤버십(관찰 노드는 관찰 노드 목록)의 링크를 그 노드가 적은 것으로 고친다. 여러 링크로 합류(`cluster
  join`)는 업그레이드가 끝난 뒤에 된다(그 전에는 409 `cluster_upgrading`; 새 노드는 첫 링크가 죽어 있어도 로그를 받도록
  합류 때 모든 링크를 싣는다). 조인 URL의 노드가 아직 옛 버전이면 모르는 필드(`cluster_addresses`)로 거절하고, 합류하는
  노드는 "upgrade every node first"로 알린다 — 모든 노드를 먼저 올리거나 링크 하나로 합류한 뒤 링크를 더한다. **주의:** 이때 설정의 링크가 멤버십의 주소와 다른 기존 노드(예: `address`를 바꾸고 다시
  시작한 노드)는 첫 링크도 그 노드의 설정 값으로 바뀐다. 업그레이드 전에 노드마다 `address`(또는 `cluster_addresses`)가
  다른 노드가 닿는 주소인지 본다. `address`도 이제 합류 API와 같은 호스트 규칙으로 검사하므로(끝의 점·멀티캐스트 등은
  거절되어 노드가 시작하지 않는다) 올리기 전에 확인한다.
- **시험.** `cargo test -p opld --test cluster`가 우선 링크를 아무도 듣지 않는 127.0.0.2로 둔 세 노드(`cluster_listen`은
  127.0.0.1)로 합류·쓰기 전달·리더 kill -9 뒤 새 리더를 확인하고(macOS는 연결이 응답 없이 시간 초과, Linux는 거절이라
  두 가지 모두 "죽은 우선 링크"다), 링크 하나로 만든 노드에 `cluster_addresses`를 더해 다시 시작하면 인증서와 멤버십이
  바뀌는지 본다. 링크를 도중에 끊고 되살리기는 memnet(`cargo test -p opl-cluster --test links`)과 gRPC
  (`cargo test -p opl-peer --test grpc`) 시험이 본다.
````

`crates/opl-api/src/cluster.rs`에서

```rust
    pub address: Option<String>,
    /// 노드 간 통신 주소
    pub cluster_address: String,
    /// 투표 멤버인지(`role`이 `voter`)
```

을 다음으로 바꾼다:

```rust
    pub address: Option<String>,
    /// 노드 간 통신 주소(우선 링크)
    pub cluster_address: String,
    /// 노드 간 통신 링크 모두(앞이 우선, 첫 것이 `cluster_address`)
    pub cluster_addresses: Vec<String>,
    /// 투표 멤버인지(`role`이 `voter`)
```

`crates/opl-api/src/cluster.rs`에서

```rust
            cluster_address: addr.clone(),
            voter: role == MemberRole::Voter,
```

을 다음으로 바꾼다:

```rust
            cluster_address: addr.clone(),
            cluster_addresses: members
                .links
                .get(&id)
                .cloned()
                .unwrap_or_else(|| vec![addr.clone()]),
            voter: role == MemberRole::Voter,
```

`crates/opl-api/tests/cluster.rs`에서

```rust
    assert_eq!(members.links[&NodeId(node)], links);
```

을 다음으로 바꾼다:

```rust
    assert_eq!(members.links[&NodeId(node)], links);
    let (status, view) = env.admin().await.get("/cluster").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let me = view["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == node.to_string())
        .unwrap_or_else(|| panic!("the node is not listed: {view}"))
        .clone();
    assert_eq!(me["cluster_address"], "10.0.0.7:8007");
    assert_eq!(me["cluster_addresses"], json!(links));
```

`crates/opl-api/tests/health.rs`에서

```rust
        assert_eq!(me["cluster_address"], format!("observer-{id}"));
        assert_eq!(view["members"][0]["role"], "voter", "{view}");
```

을 다음으로 바꾼다:

```rust
        assert_eq!(me["cluster_address"], format!("observer-{id}"));
        assert_eq!(me["cluster_addresses"], json!([format!("observer-{id}")]));
        assert_eq!(view["members"][0]["role"], "voter", "{view}");
```

`crates/opl-peer/src/client.rs`에서

```rust

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
```

을 다음으로 바꾼다:

```rust

/// 연결을 맺기까지 기다리는 최대 시간. 전달자가 쓰기 한 번을 기다리는 시간(`opl_cluster`의 3초)보다 짧아야
/// 답 없는 링크로 보낸 쓰기가 버려지기 전에 연결 실패로 끝나, 같은 시도 안에서 다음 링크로 다시 간다(쓰기는 연결
/// 실패만 센다).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    revived.await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    revived.await.unwrap();
}

/// 연결에 답하지 않는 우선 링크(127.0.0.2: Mac에서는 SYN에 답이 없고 Linux에서는 거절)로 전달한 쓰기는 연결 제한
/// 시간이 전달자의 시도 제한 시간(3초)보다 짧아 연결 실패로 끝나고, 같은 시도 안에서 둘째 링크로 다시 가 성공한다.
/// 쓰기처럼 연결 실패만 세는 요청밖에 보내지 않는 노드(막 합류해 등록하는 노드)도 넘어간다.
#[tokio::test]
async fn a_forwarded_write_fails_over_from_a_link_that_never_connects() {
    let (ca, _root, nodes) = start(1).await;
    let (id, server) = (nodes[0].id(), nodes[0].addr.clone());
    let port = closed_port().await.rsplit_once(':').unwrap().1.to_string();
    let first = format!("127.0.0.2:{port}");
    let book = Arc::new(opl_cluster::LinkBook::default());
    book.replace([&OrbNode::new([first.clone(), server.clone()])]);
    let peers = Peers::new(&tls_for(&ca, id));
    peers.set_link_book(book);

    let remove = LeaderRequest::Remove {
        id: NodeId(Uuid::now_v7()),
    };
    let attempt =
        tokio::time::timeout(Duration::from_secs(3), peers.leader_request(&first, remove)).await;
    assert!(matches!(attempt, Ok(Ok(_))), "{attempt:?}");
    assert_eq!(peers.active_link(&first), server);
    peers
        .leader_request(&first, LeaderRequest::RenewLease { node: id })
        .await
        .unwrap();
}
```

`crates/opld/tests/cluster.rs`에서

```rust
//! 위트니스는 다시 시작해도 위트니스이고, 위트니스만 남기는 탈퇴는 거부한다.
```

을 다음으로 바꾼다:

```rust
//! 위트니스는 다시 시작해도 위트니스이고, 위트니스만 남기는 탈퇴는 거부한다.
//! 계획 49: 우선 링크가 죽은 노드들도 둘째 링크로 클러스터를 이루고, 링크를 더해 다시 시작하면 인증서와
//! 멤버십이 따라온다.
```

`crates/opld/tests/cluster.rs`에서

```rust
use common::{Daemon, eventually};
use serde_json::Value;
```

을 다음으로 바꾼다:

```rust
use common::{Daemon, eventually};
use serde_json::{Value, json};
```

`crates/opld/tests/cluster.rs`에서

```rust
    off.stop();
}
```

을 다음으로 바꾼다:

```rust
    off.stop();
}

/// 우선 링크 127.0.0.2는 아무도 듣지 않는다(Mac에서는 응답 없이 시간 초과, Linux에서는 거절). 노드 간 서버는
/// 127.0.0.1에서만 들으므로 모든 노드 사이 통신이 둘째 링크로 넘어가야 한다.
const DEAD_FIRST_LINK: &str = "cluster_addresses = [\"127.0.0.2\", \"127.0.0.1\"]";

/// `DEAD_FIRST_LINK` 설정의 노드 간 링크(앞이 우선)
fn links(d: &Daemon) -> Vec<String> {
    vec![
        format!("127.0.0.2:{}", d.cluster_port),
        format!("127.0.0.1:{}", d.cluster_port),
    ]
}

/// 노드 인증서의 이름
fn node_cert_names(d: &Daemon) -> Vec<String> {
    let pem = std::fs::read_to_string(d.data_dir().join("pki").join("node.crt")).unwrap();
    opl_peer::pki::cert_names(&pem).unwrap()
}

/// 계획 49: 세 노드가 우선 링크가 죽은 채로 클러스터를 이루고(합류·승격·쓰기 전달), 리더를 kill -9 해도
/// 남은 둘이 둘째 링크로 새 리더를 뽑아 쓰기를 잇는다. 멤버마다 모든 링크가 보이고 인증서에 모든 링크 이름이 있다.
#[tokio::test]
async fn nodes_whose_first_link_is_dead_form_a_cluster_over_the_second() {
    let mut nodes = vec![
        Daemon::new("node1", DEAD_FIRST_LINK),
        Daemon::new("node2", DEAD_FIRST_LINK),
        Daemon::new("node3", DEAD_FIRST_LINK),
    ];
    for d in &mut nodes {
        d.start().await;
    }
    nodes[0].init("prod").await;
    let password = nodes[0].admin_password.clone().unwrap();
    let fp = nodes[0].ca_fingerprint.clone().unwrap();
    nodes[0].login().await;
    for i in 1..3 {
        let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
        nodes[i].join(url.trim()).await.unwrap();
        nodes[i].login_with(&password, &fp).await;
    }
    wait_voters(&nodes[0], 3).await;
    eventually("all nodes online", || async {
        members(&nodes[0]).await.iter().all(|m| m["online"] == true)
    })
    .await;
    let view = members(&nodes[0]).await;
    for d in &nodes {
        let m = view
            .iter()
            .find(|m| m["name"] == d.name.as_str())
            .unwrap_or_else(|| panic!("{} is not listed: {view:?}", d.name));
        assert_eq!(m["cluster_address"], links(d)[0], "{m}");
        assert_eq!(m["cluster_addresses"], json!(links(d)), "{m}");
        assert!(
            node_cert_names(d).iter().any(|n| n == "127.0.0.2"),
            "{}: {:?}",
            d.name,
            node_cert_names(d)
        );
    }

    // 팔로워로 쓰면 리더로 전달된다.
    for (i, d) in nodes.iter().enumerate() {
        let pool = d.path("pool");
        std::fs::create_dir_all(&pool).unwrap();
        d.ctl(&[
            "pool",
            "create",
            &format!("pool{i}"),
            "--path",
            pool.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }

    let leader = leader_name(&nodes[0]).await.unwrap();
    let dead = nodes.iter().position(|d| d.name == leader).unwrap();
    nodes[dead].kill();
    let alive: Vec<usize> = (0..3).filter(|i| *i != dead).collect();
    let (a, b) = (alive[0], alive[1]);
    eventually("a new leader", || async {
        leader_name(&nodes[a])
            .await
            .is_some_and(|name| name != leader)
    })
    .await;
    let pool = nodes[a].path("pool-after");
    std::fs::create_dir_all(&pool).unwrap();
    nodes[a]
        .ctl(&["pool", "create", "after", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    eventually("node b sees the pool", || async {
        nodes[b].ctl(&["pool", "get", "after"]).await.is_ok()
    })
    .await;

    nodes[a].stop();
    nodes[b].stop();
}

/// 계획 49: 링크 하나로 만든 노드에 `cluster_addresses`를 더해 다시 시작하면 같은 키로 인증서를 다시 받고, 리더가
/// 멤버십의 링크를 노드가 적은 목록으로 고친다.
#[tokio::test]
async fn adding_links_and_restarting_updates_the_certificate_and_the_membership() {
    let mut d = Daemon::new("node1", "");
    d.start().await;
    d.init("prod").await;
    d.login().await;
    assert!(!node_cert_names(&d).iter().any(|n| n == "127.0.0.2"));
    let key = std::fs::read_to_string(d.data_dir().join("pki").join("node.key")).unwrap();
    let single = vec![format!("127.0.0.1:{}", d.cluster_port)];
    assert_eq!(members(&d).await[0]["cluster_addresses"], json!(single));
    d.stop();

    let config = std::fs::read_to_string(d.path("opld.toml")).unwrap();
    std::fs::write(d.path("opld.toml"), format!("{DEAD_FIRST_LINK}\n{config}")).unwrap();
    d.start().await;
    assert!(node_cert_names(&d).iter().any(|n| n == "127.0.0.2"));
    assert_eq!(
        std::fs::read_to_string(d.data_dir().join("pki").join("node.key")).unwrap(),
        key
    );
    eventually("the membership has both links", || async {
        members(&d).await[0]["cluster_addresses"] == json!(links(&d))
    })
    .await;
    d.stop();
}
```

`docs/superpowers/specs/2026-11-11-availability-scale-design.md`에서

```markdown
- 리스 갱신을 투표 멤버가 묶어 리더에 전달하기, 처음부터 관찰 노드로 가입하기, 대기 learner 수를 설정으로 두기.
```

을 다음으로 바꾼다:

```markdown
- 리스 갱신을 투표 멤버가 묶어 리더에 전달하기, 처음부터 관찰 노드로 가입하기, 대기 learner 수를 설정으로 두기.

## 16. 구현(계획 49)

다섯 단계를 커밋 다섯으로 했다(1 `OrbNode`와 TypeConfig, 2 링크 고르기와 넘어가기, 3 노드 설정·인증서·합류, 4 링크
맞추기와 `multi-link`, 5 API·웹·실제 프로세스 시험·문서). 설계 §7과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**노드 타입(`OrbNode`).**
- `OrbNode { addrs: Vec<String> }`(각 항목 `host:port`, 앞이 우선)이고 serde는 손으로 썼다: 늘 `addr`(= `addrs[0]`)를
  쓰고 링크가 둘 이상일 때만 `addrs`를 더 쓴다. 링크 하나면 `BasicNode`와 바이트까지 같아 이미 쓴 로그·스냅샷과 옛
  노드가 그대로다. openraft 0.9.25의 `BasicNode`는 `deny_unknown_fields`가 없어 옛 노드는 여러 링크 항목을 첫 링크
  하나로 읽는다. 읽을 때는 `addrs`가 있고 비지 않으면 그것, 아니면 `[addr]`(빈 `addr`이고 `addrs`도 없으면 링크 없는
  `default()`). 계획 48 형식으로 쓴 멤버십 로그 항목·`StoredMembership`·스냅샷 메타의 고정 JSON을 읽는 시험이 있다.
- `OrbNode::addr()`(첫 링크)가 이 계획 전의 "그 노드의 주소"다. 주소 문자열을 받는 기존 API(`Members`의 값,
  `NotLeader.leader_addr`, `Forwarder`, `FollowClient`, `Peers`의 `addr: &str`, `ClusterStatus.observers`의 값)는 바꾸지
  않고 첫 링크를 노드의 열쇠로 쓴다. 링크 목록은 `Members.links`와 링크 책이 따로 싣는다.
- 설계는 "`multi-link` 기능으로 가린다"였다. 노드 타입 자체는 가리지 않고(링크 하나면 형식이 같으므로) 여러 링크를
  멤버십에 넣는 일(링크 맞추기, 여러 링크로 합류)만 가린다.

**링크 목록의 출처.**
- Raft 구성원은 멤버십의 `OrbNode.addrs`, 관찰 노드는 새 `ClusterStatus.observer_links: BTreeMap<Uuid, Vec<String>>`
  (링크가 둘 이상인 관찰 노드만, 비면 쓰지 않음, 옛 노드는 무시; OpenAPI에는 없음)다. 노드가 원하는 링크는 새
  `NodeStatus.cluster_addresses`(`host:port`, 등록 때 노드가 링크 하나여도 씀)다. `NodeSpec`은 `deny_unknown_fields`라
  건드리지 않았다. 옛 리더가 Node status를 다시 쓰며 `cluster_addresses`를 떨어뜨리면 `refresh_features`가 되살린다.
  등록·`refresh_features`는 호스트가 지금 노드 인증서의 이름에 있는 링크만 적는다(`announced_links`; 다시 서명이 실패해
  이름이 빠진 링크로는 다른 노드가 TLS로 닿지 못하므로 경고만, 리뷰; 경고는 빠진 링크 집합이 바뀔 때만 — `refresh_features`가
  60초마다 부르므로, 2차 리뷰). 다시 합류하면 API가 그 노드의 기록을 새 링크로
  둔다(옛 값이 남으면 링크 맞추기가 멤버십을 옛 링크로 되돌렸다, 리뷰).
- `initialize`(새 클러스터)와 `AddLearner`(합류)는 받은 링크를 그대로 쓴다(`LeaderRequest::AddLearner.addrs`, 링크
  하나면 생략해 옛 형식 그대로; 옛 리더는 모르는 필드를 무시하고 첫 링크만 쓴다). 첫 링크가 죽어 있으면 learner가
  로그를 받지 못해 등록·링크 맞추기까지 갈 수 없으므로 합류에서 바로 실어야 한다.
- `Cluster::link_book()`(첫 링크 → 링크 목록, 링크 하나인 노드는 넣지 않음)은 적용된 멤버십과 관찰 노드 목록으로
  `track_observers`가 고친다(`FeedBatch.membership`이 있을 때, Reset·Lagged 뒤 다시 읽을 때, 목록이 바뀔 때). opld는
  `open_node` 뒤 `peers.set_link_book(cluster.link_book())`을 부른다(관찰 노드·위트니스 포함). Raft의 `GrpcConnection`은
  책을 붙이면 책의 목록을, 붙이기 전에는 `new_client`가 받은 `OrbNode.addrs`를 쓴다(책을 붙이기 전에도 여러 링크).
  처음에는 늘 `OrbNode.addrs`를 썼는데, 멤버십이 바뀌는 사이 책(적용된 멤버십)과 목록이 달라 같은 노드의 선택 상태가
  호출마다 처음부터 다시 세어졌다(리뷰). 그래서 로그의 멤버십에만 있는 새 링크는 적용된 뒤에 쓰인다.

**넘어가기 규칙(`opl-cluster/src/links.rs`, Peers와 memnet이 함께 씀).**
- 첫 링크를 열쇠로 노드마다 `{ links, active, last_ok, timeouts, switched_at, trial }`이다(`LinkSelector`). 연결
  실패(`Connect`)면 곧바로, 제한 시간 초과(`Timeout`)면 지금 링크에서 연속 두 번이거나 그 링크의 마지막 성공(또는 그
  링크로 넘어온 때)에서 400 ms(`LINK_DEAD_AFTER`)가 지났을 때(먼저인 것) 다음 링크로 넘어가고, 모두 실패하면 돌아가며
  시도한다. 처음에는 "마지막 성공에서 1초"였는데, 하트비트 200 ms를 더하면 조용히 죽은 링크에서 1.2~1.4초 동안 따르는
  노드에 하트비트가 없어 선거 제한 시간(1~2초)에 걸렸다(pre-vote가 없어 리더가 물러남, 리뷰). **넘어가기 상한
  (`LINK_DEAD_AFTER` + 하트비트 간격)은 `election_timeout_min − heartbeat_interval`보다 짧아야 한다**(400 + 200 = 600 ms <
  1000 − 200 = 800 ms). 하트비트 하나가 늦은 살아 있는 링크(초과 한 번, 400 ms 안)는 버리지 않는다.
- 우선 링크가 아닌 동안 30초(`LINK_RETRY`)마다 다음 호출 **하나**를 우선 링크로 보내고(시험 호출) 성공하면 돌아오며
  실패하면 머물러 다시 잰다. 시험 호출은 control 채널의 짧은 호출(`Counts::All`: Raft RPC·ping·리스 갱신·Follow 열기)만
  보낸다(`LinkSelector::current`; 처음에는 콘솔·마이그레이션·쓰기도 시험 호출이 될 수 있어 죽은 우선 링크에서 오래
  걸렸다, 리뷰). 우선 링크로 돌아가는 것은 시험 호출의 성공이거나 지금 링크로 넘어온(마지막 시험을 보낸) 뒤 보낸 호출의
  성공일 때만이다(`Pick.sent`; 넘어가기 전에 보낸 호출의 늦은 성공이 죽은 링크로 되돌리던 것, 리뷰). 따로 도는 링크
  ping은 두지 않았다(memnet과 같은 코드로 시험하려고).
- **연결 실패는 같은 호출 안에서 다시 보낸다(리뷰).** Peers는 요청이 나가지 않았음이 확실한 호출만 다시 고른 다른
  링크로 한 번 더 보낸다(`not_sent`): 상태의 오류 원인 사슬에 `tonic::ConnectError`(TCP 연결·TLS 핸드셰이크 실패, 연결
  제한 시간 포함)가 있을 때, 링크로 채널을 만들지 못했을 때, 아래의 다른 노드 거절. 시험 호출이든 아니든 같다(시험
  호출이 실패해도 그 호출은 지금 링크로 성공한다). 처음에는 모든 `Unavailable`을 다시 보냈는데, tonic 0.14.6은 이미
  보낸 진행 중 호출도 keepalive ping 만료(`hyper::Error::is_timeout`)면 `Unavailable`로 끝내므로(`status.rs`
  `from_hyper_error`) 받은 쓰기가 두 번 갈 수 있었다(2차 리뷰). `tonic::transport::Error`는 채널의 모든 오류(보낸 뒤의 h2·IO
  오류 포함)를 감싸 쓰지 않았다. 링크 고르기에는 지금처럼 모든 `Unavailable`을 연결 실패로 센다(다음 호출은 다음 링크).
  시험: 쓰기를 받고 답을 버리며 연결은 열어 두는 중계 → keepalive 만료 뒤 오류, 둘째 링크로 연결 없음
  (`a_sent_write_that_loses_its_connection_is_not_sent_again`, 약 30초). 제한 시간 초과는 상대가 받았을 수 있어 다시
  보내지 않는다. 요청 스트림을 한 번만 보낼 수 있는 콘솔은 다시 보내지 않는다. 서버가 내려가는 중이라 답한
  `Unavailable`("shutting down")은 연결 실패로 세지만 다시 보내지 않는다. memnet은 다시 보내지 않는다(끊긴 링크는 연결
  실패로 넘기기만).
- 넘어가면 `warn`, 돌아오면 `info` 로그다. 헬스·지표에는 내지 않는다(계획 52의 관리망 검사와 함께 나중).
- 시간은 `tokio::time::Instant`(멈춘 시계 단위 시험). `ClusterConfig.link_retry`·`Peers::with_link_retry`는 시험 전용이다
  (memnet 시험 2.5초).

**Peers(opl-peer)가 실패를 보는 곳.**
- 채널 캐시는 `(Lane, 링크)`마다다(링크를 바꾸면 다른 TCP 연결). 호출마다 링크를 고르고 결과를 공통 도우미(`Attempt`)로
  알린다: `Unavailable`은 연결 실패, `DeadlineExceeded`·`Cancelled`(tonic 자기 제한 시간)는 초과, `Unknown`·`Internal`은
  세지 않음, 결과 없이 버려진 호출(부른 쪽의 `timeout`)은 가드가 초과로 센다. openraft가 다른 까닭(복제 스트림 다시
  만들기)으로 버린 호출도 초과로 세지만 "연속 두 번 또는 400 ms" 기준이 대개 가린다.
- **대상 노드 확인(리뷰).** 노드 A의 링크에 B의 주소가 들어가면(오타, 다시 쓴 IP) 리더가 A에게 보낼 append·vote를 B가
  받아 이중으로 셀 수 있었다. 대상 id를 아는 호출(Raft RPC의 `GrpcConnection`, 리더 전달·리스 갱신의 `Forwarder`, Follow)은
  gRPC 메타데이터 `orbivisor-target: <노드 uuid>`를 싣고, 서버는 있으면 자기 id와 비교해 다르면 처리하지 않고
  `permission_denied`로 답하며 같은 키에 자기 id를 싣는다(옛 노드는 싣지 않으므로 받는다). 클라이언트는 그 답을 연결
  실패로 세어 다음 링크로 다시 보낸다. ping(생존 확인 모니터)과 bulk 호출 가운데 부르는 곳이 id를 아는 `uefi_vars`·
  `read_audit`·`read_metrics`도 싣는다(2차 리뷰: 싣지 않은 ping이 다른 노드의 답으로 잘못된 링크를 성공으로 셌다). 싣지
  않는 것: opld의 `AddVoter` 요청(대상 id를 모름), 마이그레이션 준비·기다리기·취소와 콘솔(부르는 곳이 주소만 넘김).
  설정 쪽에서도 막는다: 합류는 다른 구성원·관찰 노드의 링크와 겹치는 링크를 400으로, 링크 맞추기는 그런 노드를 건너뛰고
  한 번 경고한다(`link_clashes`).
- 초과까지 세는 것: Raft RPC(openraft `hard_ttl`, `append_entries`는 하트비트 간격 200 ms), ping, `RenewLease`·`RenewVip`
  (gRPC 제한 시간을 90초에서 3초 `LEASE_RPC_TIMEOUT`로 — HA 리스 갱신의 바깥 `timeout`은 `fence_after`가 20초 이상이면
  4초라 Peers가 초과를 보고, 더 짧은 `fence_after`면 바깥이 먼저 버려 가드가 초과로 센다), Follow
  열기(응답 헤더까지 3초 `FOLLOW_OPEN_TIMEOUT`). 쓰기·`AddVoter` 같은 긴 리더 요청은 연결 실패만 센다(따라잡기 60초를
  실패로 보지 않게).
- ping과 리더 요청은 Peers 안에서도 제한 시간(`within`)으로 끝낸다: 연결은 받고 답하지 않는 리스너에서 TLS
  핸드셰이크가 tonic 제한 시간 밖에서 멈췄다.
- **연결 제한 시간 3초 → 2초(단계 5).** 실제 프로세스 시험에서 우선 링크가 SYN에 답하지 않는 macOS(127.0.0.2)의 막
  합류한 노드가 등록 쓰기를 전달하지 못해 합류가 실패했다. 쓰기 전달은 연결 실패만 세는데, 전달자의 시도 제한 시간
  (`FORWARD_ATTEMPT_TIMEOUT` 3초)과 연결 제한 시간(3초)이 같아 연결 실패가 나기 전에 시도가 버려졌고(버려진 긴 요청은
  세지 않음) 같은 링크로 다시 보내기만 되풀이했다. 합류 직후의 노드는 아직 리스 갱신·ping을 보내지 않아 다른 호출로도
  넘어가지 못했다. 연결 제한 시간을 2초로 줄여 답 없는 링크가 시도 안에서 연결 실패(`Unavailable`)로 끝나게 했고,
  리뷰 뒤로는 그 쓰기가 같은 시도 안에서 둘째 링크로 다시 가 성공한다(`opl-peer/tests/grpc.rs`의
  `a_forwarded_write_fails_over_from_a_link_that_never_connects`, 연결 제한 시간이 3초면 실패). 연결은 받지만
  TLS 핸드셰이크에서 멈추는 링크로 보낸 쓰기는 여전히 넘어가지 못한다(실제 끊긴 망은 연결을 받지 않고, 리스 갱신·ping이
  같은 노드의 링크를 넘기므로 남겨 둠). 그런 링크는 연결 실패만 세는 호출(쓰기·`AddVoter`·bulk 호출)로는 실패로
  세어지지 않는다(제한 시간 초과를 세지 않음) — 초과까지 세는 control 채널의 짧은 호출(Raft·ping·리스 갱신·Follow
  열기)이 넘긴다.
- 이미 기다리던 긴 요청은 죽은 연결에서 keepalive(10초 + 20초)까지 걸릴 수 있다(다음 호출은 새 링크).

**memnet.** 노드를 링크와 함께 등록하고(멤버십의 `OrbNode`로 링크 → 노드), `MemRouter::cut_link(from, link)`·`sent(link)`
(그 링크로 간 RPC 수), `heal(id)`이 끊긴 링크도 푼다. `MemForwarder`·`MemFollow`는 보내는 노드의 링크 책, `MemConnection`
(Raft)은 링크 책이 아니라 `new_client`가 받은 멤버십의 `OrbNode.addrs`에서 보내는 노드의 `LinkSelector`로 링크를 고르고
끊긴 링크면 연결 실패다(Peers의 `GrpcConnection`과 달리 책을 읽지 않음). memnet은 같은 호출 안에서 다시 보내지 않는다.

**링크 맞추기와 `multi-link`.**
- 노드 기능 `multi-link`(`FEATURES` 끝, `opl_cluster::MULTI_LINK`). 리더의 `VoterKeeper` 틱에서 joint가 아니고 `plan`이
  바꿀 것이 없고 살아 있는 투표 멤버가 과반일 때, 모든 Node(위트니스·관찰 노드 포함)가 Node 기록과 `multi-link`를 가질
  때만, NodeId 순으로 한 틱에 한 노드(`link_fix`): Raft 구성원의 `addrs`가 그 노드의 `status.cluster_addresses`(비지 않을
  때)와 다르면 `change_membership(SetNodes { id: OrbNode }, false)`(`change_timeout` 안, 앞선 변경이 커밋 중이면 다음 틱),
  관찰 노드면 `observer_links`를 `UpdateStatusIf`로 고친다(첫 링크가 바뀌면 `observers` 값도). 첫 링크를 바꾸는 것도
  허용한다 — 같은 노드 id이고 그 노드가 스스로 적은 값이라 openraft가 경고하는 "다른 노드를 가리키는 주소 바꾸기"가
  아니다. 그래서 업그레이드 뒤 설정의 링크가 멤버십 주소와 다른 기존 노드(예: `address`를 바꾸고 다시 시작한 노드)는 첫
  링크도 바뀐다(README의 업그레이드 주의).
- openraft 0.9.25의 `SetNodes`는 투표 멤버 집합이 그대로라 joint 없이 항목 하나로 커밋한다(`membership.rs`의
  `Membership::change` — `SetNodes`는 노드 값만 바꾸고 구성은 그대로 둔다). 그래서 `same_config`·`finish_joint`와 부딪히지 않는다
  (joint 구성이 생기지 않음). 멤버십이 바뀌면 리더가 복제 스트림을 다시 만들며 `new_client`를 새 노드 값으로 부른다.
- 좁은 경합(2차 리뷰): openraft의 `SetNodes`는 멤버십에 없는 노드를 더한다. 키퍼가 `same_config`를 본 뒤
  `change_membership`이 커밋되기 전에 노드 제거(`Remove`, API의 노드 삭제·`leave`)가 먼저 커밋되면 막 지운 노드가 learner로
  되살아날 수 있다. 키퍼는 `Done` 뒤 그 노드가 투표 멤버였는데 아니게 됐거나 Node 기록이 없으면 warn("updated the cluster
  links of a node that was removed meanwhile")만 한다. 되살아난 learner를 저절로 지우는 고아 정리는 없어 그 경고를 보고
  손으로 다시 뺀다(Node 기록 삭제가 `Remove` 뒤라 그 사이에 본 경우는 경고도 놓칠 수 있다).
- 키퍼의 `ToObserver`는 `addrs`를 `observer_links`에, learner로 되돌리기(`return_observer`)는 `observer_links`(없으면
  `observers` 값 하나)로 `OrbNode`를 만든다. `LeaderRequest::Remove`는 `observer_links`에서도 뺀다. `observer_nodes(status)`가
  관찰 노드 캐시·링크 책·`Members.links`에 관찰 노드의 실제 링크를 준다.

**설정·인증서·합류.**
- opld 최상위 `cluster_addresses`(호스트만, 포트·중복·잘못된 모양 거절; 호스트 규칙은 API 합류와 같은
  `opl_types::host::valid_host`이고 중복은 값으로 — IP는 같은 주소, 이름은 대소문자 없이, 리뷰. `address`도 같은 규칙이라
  끝의 점·멀티캐스트 같은 값은 이제 거절되어 그런 `address`로 돌던 노드는 업그레이드 뒤 시작하지 않는다 — README
  업그레이드 주의에 올리기 전에 확인하라고 적었다, 2차 리뷰), `Config::cluster_links()`(없거나 비면
  `[address:포트]`), `cluster_address()`는 첫 링크, `cert_names()`는 `address`·모든 링크 호스트·localhost 셋.
  `cluster_listen`이 `0.0.0.0`·`::`가 아닌데 링크가 둘 이상이면 시작 로그에 경고만(시험이 일부러 그렇게 쓴다).
- 합류: opld `join.rs`는 링크가 둘 이상일 때만 `JoinRequest.cluster_addresses`를 보낸다(옛 API 노드의
  `deny_unknown_fields`를 건드리지 않게). API는 첫 것이 `cluster_address`와 같고 겹치지 않으며 모두 SAN에 넣을 수 있는
  호스트인지 보고(아니면 400), 둘 이상이면 `multi-link`를 요구한다(409 `cluster_upgrading`; 다시 합류하는 노드 자신의 옛
  Node 기록은 세지 않음). 겹침은 값으로 보고(`same_link`), 다른 노드의 링크와 겹치면 400이다(이미 멤버·이름 검사 뒤).
  다른 노드의 Node 기록이 원하는(`status.cluster_addresses`, 링크 맞추기가 아직 멤버십에 넣지 않은) 링크와 겹쳐도 400이다(2차
  리뷰). 다시 합류 때 주소 비교는 링크 목록 전체다. 옛 API 노드는 `cluster_addresses`를 모르는 필드로 거절하므로, opld는 그
  거절을 "모든 노드를 먼저 올리라"는 메시지로 바꿔 보인다.
- 다시 서명: opld가 Restart에서 저장소를 연 뒤·노드 간 서버를 띄우기 전에(위트니스 포함) 노드 인증서에 빠진 이름이
  있으면 저장소의 클러스터 CA로 **같은 키**에 새로 서명해(`pki::csr_for_key`) `pki` 디렉터리에 저장하고 서버·마이그레이션
  TLS·API 인증서에 쓴다. CA가 아직 없거나 실패하면 경고만. 클라이언트 신원은 옛 인증서 그대로다(이름이 아니라 CA와 CN의
  노드 id로 확인됨).
- 마이그레이션의 QEMU 스트림 호스트는 대상 노드의 지금 링크(`Peers::active_link(addr)`)의 호스트다. 받을 준비 요청
  (`migrate_incoming`)이 성공한 뒤에 읽는다(그 전에는 아직 죽은 줄 모르는 첫 링크일 수 있었다, 리뷰; 그 요청은 연결
  실패면 다른 링크로 다시 가므로 성공한 뒤의 지금 링크는 닿은 링크다).

**보이기(단계 5).** API `MemberView.cluster_addresses`(모든 링크, 첫 것 = `cluster_address`; `Members.links`에 없으면
`[cluster_address]`), `orbivisorctl cluster status` 표는 그대로(`-o json`에 보임), 웹 클러스터 화면은 클러스터 주소 아래에
둘째 링크부터 작게.

**실제 프로세스 시험(`opld/tests/cluster.rs`).**
- 세 노드가 `cluster_addresses = ["127.0.0.2", "127.0.0.1"]`, `cluster_listen = "127.0.0.1:{port}"`로 우선 링크가 죽은 채
  init·합류·승격·쓰기 전달을 하고, 모든 멤버의 `cluster_addresses`가 두 링크이며 노드 인증서에 127.0.0.2가 있고, 리더 kill -9
  뒤 남은 둘이 새 리더를 뽑아 쓰기를 잇는다. macOS의 lo0에는 127.0.0.1만 있어 127.0.0.2로의 연결은 응답 없이 시간
  초과이고, Linux는 127/8 전체가 로컬이라 듣지 않는 포트는 곧바로 거절이다. 두 OS 모두 sudo 없이 "죽은 우선 링크"를
  본다(시험 약 25초, macOS). 합류 때 리더의 첫 `append_entries`들은 200 ms에 끊기다 1초 안팎에 둘째 링크로 넘어갔다(리뷰 뒤의 규칙으로는 연속
  두 번째 초과, 약 400 ms).
- 링크 하나로 만든 노드에 `cluster_addresses`를 더해 다시 시작하면 인증서가 같은 키로 다시 서명되고(127.0.0.2 포함)
  리더(그 노드 자신)가 멤버십을 두 링크로 고친다.
- 링크를 도중에 끊고 되살리는 것은 memnet(`opl-cluster/tests/links.rs`: 모든 노드 사이 첫 링크를 끊어도 리더·쓰기·리스
  갱신·관찰 노드 Follow가 계속, 리더를 죽이면 둘째 링크로 새 리더, 풀면 재시도 간격 뒤 첫 링크로, 링크 맞추기의 여러
  경우)과 gRPC(`opl-peer/tests/grpc.rs`: 닫힌 포트·받고 답하지 않는 리스너·SYN에 답 없는 주소) 시험이 본다.

**남은 것.**
- 링크 상태를 헬스·지표·웹에 보이기(계획 52의 관리망 검사와 함께), 링크별 백그라운드 ping, 대량 전송(콘솔·마이그레이션
  준비)용 링크를 따로 고르기.
- TLS 핸드셰이크에서 멈추는 링크로 보낸 긴 리더 요청은 다른 호출이 링크를 넘길 때까지 넘어가지 못한다.
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  cluster_address: `${name}:8007`,
  voter: true,
```

을 다음으로 바꾼다:

```tsx
  cluster_address: `${name}:8007`,
  cluster_addresses: [`${name}:8007`],
  voter: true,
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
        members: [
          member("n1", "node-a", { fencing: "watchdog" }),
          member("n2", "node-b", { fencing: null }),
```

을 다음으로 바꾼다:

```tsx
        members: [
          member("n1", "node-a", {
            fencing: "watchdog",
            cluster_addresses: ["node-a:8007", "10.1.0.11:8007"],
          }),
          member("n2", "node-b", { fencing: null }),
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  expect(fencing("witness-w")).toHaveTextContent("—");
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  expect(fencing("witness-w")).toHaveTextContent("—");
  // 클러스터 주소는 우선 링크이고 둘째 링크부터 그 아래 보인다.
  const links = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Cluster address");
  const linksOf = (name: string) =>
    within(within(table).getByText(name).closest("tr")!).getAllByRole("cell")[links];
  expect(linksOf("node-a")).toHaveTextContent("node-a:800710.1.0.11:8007");
  expect(linksOf("node-b")).toHaveTextContent(/^node-b:8007$/);
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.cluster_address}</TableCell>
              </TableRow>
```

을 다음으로 바꾼다:

```tsx
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">
                  {m.cluster_address}
                  {/* 둘째 링크부터(넘어갈 링크)는 우선 링크 아래 작게 */}
                  {m.cluster_addresses.slice(1).map((link) => (
                    <div key={link} className="text-muted-foreground">
                      {link}
                    </div>
                  ))}
                </TableCell>
              </TableRow>
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
      node: "n1",
      members: members.map((m) => ({ cluster_address: "a", online: true, voter: true, ...m })),
    }),
```

을 다음으로 바꾼다:

```tsx
      node: "n1",
      members: members.map((m) => ({
        cluster_address: "a",
        cluster_addresses: ["a"],
        online: true,
        voter: true,
        ...m,
      })),
    }),
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1935개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: 통과

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 4: 커밋**

```bash
git add README.md crates docs web
git commit -m "test(cluster): 다중 링크 API·웹·실제 프로세스 시험과 문서 — 죽은 우선 링크로 클러스터 이루기"
```

---

## 완료 기준 (계획 49)

- 우선 링크가 죽은 세 노드가 둘째 링크로 클러스터를 이루고, 리더를 죽여도 새 리더가 뽑혀 쓰기가 이어진다(macOS 시간 초과, Linux 거절 두 경로).
- 모든 노드 사이 첫 링크를 끊어도 리더·쓰기·리스 갱신·관찰 노드 Follow가 계속되고, 풀면 재시도 간격 뒤 첫 링크로 돌아온다(memnet).
- 조용히 죽은 링크에서 약 600 ms 안에 다음 링크로 넘어가고, 받고 답을 잃은 쓰기는 다시 보내지 않는다(gRPC 시험).
- 링크를 더해 다시 시작하면 같은 키로 인증서가 다시 서명되고 리더가 멤버십을 고친다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, `ha-it`, 웹 시험이 이 Mac과 svc01에서 통과한다.

## 알고 받아들인 제한

- 우선 링크가 죽어 있는 동안 30초마다 control 호출 하나가 우선 링크를 시험한다(연결 실패면 같은 호출이 지금 링크로 다시 가고, 조용한 링크면 그 호출 하나가 시간 초과).
- 연결은 받고 TLS 핸드셰이크에서 멈추는 링크는 연결 실패만 세는 긴 요청으로는 넘어가지 않는다(같은 노드의 Raft·ping·리스 갱신이 넘긴다).
- 마이그레이션 준비·기다리기·취소와 콘솔은 대상 id를 싣지 않는다.
- 링크 맞추기의 `SetNodes`와 노드 제거가 겹치는 좁은 경합에서 지운 노드가 learner로 되살아날 수 있다(경고를 보고 손으로 뺀다, 설계 §16).
- 업그레이드 뒤 설정의 링크가 멤버십 주소와 다른 기존 노드는 첫 링크가 바뀐다. `address` 호스트 규칙이 엄격해졌다(README 업그레이드 주의).
