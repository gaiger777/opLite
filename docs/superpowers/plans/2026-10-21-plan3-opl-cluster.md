# opLite 계획 3: 클러스터 저장소 (M3) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `opl-cluster` 크레이트를 만든다. openraft로 복제하고 redb에 저장하는 리소스 저장소로, 타입 있는 CRUD와 낙관적 동시성 제어(revision), 커밋된 변경을 구독하는 watch를 제공한다. 한 프로세스 안의 3노드 클러스터에서 리더가 죽어도 쓰기가 이어지는 것을 테스트로 고정한다.

**Architecture:** 노드마다 redb 파일 하나(`{data_dir}/cluster.redb`)에 Raft 로그, 투표 기록, 상태 머신(리소스와 이름 색인), 스냅샷을 함께 둔다. 상태 머신은 엔트리 묶음을 redb 트랜잭션 하나로 적용하므로 리소스와 `last_applied`가 항상 함께 기록되고, 재시작해도 다시 적용할 필요가 없다. 리소스의 `revision`은 그 변경이 커밋된 로그 인덱스다. 명령 거부(이름 중복, revision 불일치)는 상태 머신 안에서 결정적으로 판정하므로 모든 노드가 같은 결과를 낸다. 쓰기는 리더에서만 받고, 읽기는 로컬 상태 머신에서 한다. 노드 간 실제 통신(gRPC + mTLS)은 계획 6이 만들고, 이 계획은 테스트용 인메모리 네트워크(`memnet`)만 둔다.

**Tech Stack:** Rust 2024, openraft 0.9.25(`serde`, `storage-v2`), redb 4.4, tokio, serde_json, `opl-types`

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (2.1, 3.1, 3.2, 10, 11절)
**선행 계획:** 계획 1, 계획 2 (완료)

---

## 범위

| 포함 | 제외 (담당 계획) |
|---|---|
| openraft 타입 설정, redb 로그 저장소·상태 머신·스냅샷 | 노드 간 gRPC + mTLS 네트워크, 조인 토큰, CA — 계획 6 |
| 리소스 CRUD(타입 있는 API), revision 검사, 이름 유일성 | 팔로워가 받은 쓰기를 리더로 전달 — 계획 5(API) |
| 커밋된 변경 watch(`since` 이어 받기, 스냅샷 설치 시 `Reset`) | `Node`/`Network`/`Task`/`User` 리소스 타입 — 필요한 계획에서 |
| 인메모리 네트워크(`memnet`)와 3노드 테스트 | Raft 과반수 상실 시 status 쓰기 로컬 버퍼링 — 계획 4 |
| `schema_version` 위치 결정(저장 계층) | 클러스터 기능 레벨, 스키마 마이그레이션 코드 — 첫 형식 변경 때 |

## 이전 계획에서 이어받는 사항

- 저장 형식 호환성 규칙(설계 3.1): 이 계획에서 `schema_version`을 **저장 계층**에 둔다. redb 파일의 `meta` 테이블과 스냅샷 본문에 기록하고, 봉투(`Resource`)에는 넣지 않는다. 지원하지 않는 버전의 파일이나 스냅샷은 열지 않는다.
- 계획 4(reconciler)를 위해 watch는 "변경된 리소스 전체 + revision"을 내보낸다(`WatchEvent::Put`/`Deleted`).

## 설계 결정

- **openraft 0.9.25(안정판)를 쓴다.** 0.10은 아직 alpha(0.10.0-alpha.37)라 API가 계속 바뀐다. 0.10이 안정되면 옮긴다. `storage-v2` 기능으로 로그 저장소(`RaftLogStorage`)와 상태 머신(`RaftStateMachine`)을 따로 구현한다.
- **Raft 노드 id는 UUID다(`opl_cluster::types::NodeId`).** 나중에 만들 `Node` 리소스의 id와 같은 값을 쓰므로 `VmSpec.node`(UUID)와 바로 맞물린다. `From<u64>`는 openraft 저장소 테스트 모음이 요구해서 둔다.
- **저장 형식은 JSON이다.** 로그 엔트리, 리소스, 스냅샷 모두 serde_json으로 직렬화한다. 설정 데이터 규모에서는 크기와 속도가 문제되지 않고, 디버깅과 형식 호환성 검토가 쉽다.
- **status만 바꿔도 revision이 오른다.** revision은 "마지막 수정의 커밋 인덱스"이기 때문이다. 그래서 `If-Match`로 spec을 고치는 클라이언트는 reconciler의 status 쓰기와 충돌(412)할 수 있다. 계획 5에서 다룬다.
- **쓰기에는 제한 시간(`write_timeout`, 기본 5초)이 있다.** openraft의 `client_write`는 과반수가 없으면 끝없이 기다린다. 시간이 지나면 `ClusterError::Timeout`을 돌려주지만, 명령은 나중에 커밋될 수도 있다(QMP 타임아웃과 같은 성질).
- **watch 이력은 메모리에만 있다(최근 1024건).** 재시작하거나 이력이 밀려나면 `since`가 `Expired`가 되고, 클라이언트는 목록을 다시 읽는다(Kubernetes의 list-then-watch와 같다). 스냅샷 설치로 상태가 통째로 바뀌면 `Reset`을 보낸다.
- **redb 호출은 async 함수 안에서 바로 한다.** 커밋마다 fsync가 일어나 tokio 워커가 수 밀리초 막힌다. 설정 데이터의 쓰기 빈도에서는 감수하고, 지연이 문제되면 `spawn_blocking`으로 옮긴다.

## 파일 구조

```
Cargo.toml                                 # 수정: opl-cluster, openraft, redb
README.md                                  # 수정: 상태 M3, 문서 링크
docs/superpowers/specs/...-design.md       # 수정: 3.1절 schema_version 결정, NodeId
crates/opl-types/src/
├── resource.rs                            # 수정: Kind, ResourceSpec
├── vm.rs                                  # 수정: impl ResourceSpec for VmSpec
└── storage.rs                             # 수정: impl ResourceSpec for StoragePoolSpec/VolumeSpec
crates/opl-cluster/
├── Cargo.toml
├── src/
│   ├── lib.rs                             # 모듈 선언 + 재노출
│   ├── types.rs                           # NodeId, Command, Rejection, Response, TypeConfig
│   ├── watch.rs                           # WatchHub(이력 + broadcast), Watch, WatchEvent
│   ├── store.rs                           # redb 테이블, Store::open, 스키마 버전, 읽기 함수
│   ├── log_store.rs                       # LogStore: RaftLogStorage
│   ├── state_machine.rs                   # StateMachine: RaftStateMachine, 명령 적용, 스냅샷
│   ├── cluster.rs                         # Cluster: 타입 있는 CRUD, 멤버십, watch
│   └── memnet.rs                          # 인메모리 Raft 네트워크(테스트용)
└── tests/
    ├── common/mod.rs                      # 여러 노드를 띄우는 하네스
    ├── storage_suite.rs                   # openraft 저장소 적합성 테스트 모음
    ├── single_node.rs                     # CRUD, 거부, 재시작, watch
    └── three_nodes.rs                     # 리더 장애, 분할, 스냅샷으로 따라잡기
```

---

### Task 1: `Kind`와 `ResourceSpec`

저장소는 리소스를 종류별로 나눠 저장하고, 타입 있는 API는 spec 타입만 보고 종류와 status 타입을 알아내야 한다. 이 둘을 `opl-types`에 둔다.

**Files:**
- Modify: `crates/opl-types/src/resource.rs`, `crates/opl-types/src/vm.rs`, `crates/opl-types/src/storage.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-types/src/resource.rs`의 `mod tests` 안, `empty_labels_are_omitted_and_round_trip` 위에 추가한다:

```rust
    #[test]
    fn kind_serializes_as_its_str() {
        for kind in [Kind::Vm, Kind::StoragePool, Kind::Volume] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p opl-types kind_serializes`
Expected: 컴파일 실패 (`Kind`가 없음)

- [ ] **Step 3: 구현**

`crates/opl-types/src/resource.rs`의 import를 다음으로 바꾼다:

```rust
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
```

같은 파일의 `#[cfg(test)]` 바로 위에 추가한다:

```rust
/// 클러스터 저장소가 구분하는 리소스 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Vm,
    StoragePool,
    Volume,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Vm => "vm",
            Kind::StoragePool => "storage-pool",
            Kind::Volume => "volume",
        }
    }
}

/// spec 타입과 리소스 종류, status 타입을 묶는다. 클러스터 저장소의 타입 있는 API가 쓴다.
pub trait ResourceSpec: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    const KIND: Kind;
    type Status: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static;
}
```

`crates/opl-types/src/vm.rs`: `use crate::resource::Resource;`를 `use crate::resource::{Kind, Resource, ResourceSpec};`로 바꾸고, `impl VmSpec {` 바로 위에 추가한다:

```rust
impl ResourceSpec for VmSpec {
    const KIND: Kind = Kind::Vm;
    type Status = VmStatus;
}
```

`crates/opl-types/src/storage.rs`: 같은 방식으로 import를 바꾸고, `pub enum PoolBackend` 위의 `#[derive(...)]` 바로 위와 `pub struct VolumeStatus` 위의 `#[derive(...)]` 바로 위에 각각 추가한다:

```rust
impl ResourceSpec for StoragePoolSpec {
    const KIND: Kind = Kind::StoragePool;
    type Status = StoragePoolStatus;
}
```

```rust
impl ResourceSpec for VolumeSpec {
    const KIND: Kind = Kind::Volume;
    type Status = VolumeStatus;
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-types`
Expected: 16개 통과

- [ ] **Step 5: Commit**

```bash
git add crates/opl-types
git commit -m "feat(types): 리소스 종류 Kind와 spec-status를 묶는 ResourceSpec 트레이트"
```

---

### Task 2: `opl-cluster` 뼈대와 Raft 타입

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/opl-cluster/Cargo.toml`, `crates/opl-cluster/src/lib.rs`, `crates/opl-cluster/src/types.rs`

- [ ] **Step 1: 워크스페이스 의존성 추가**

루트 `Cargo.toml`의 `[workspace.dependencies]`에서 `opl-qemu` 줄 아래에 `opl-cluster`를, `rustix` 줄 위에 openraft와 redb를 추가한다:

```toml
opl-cluster = { path = "crates/opl-cluster" }
```

```toml
openraft = { version = "0.9.25", features = ["serde", "storage-v2"] }
redb = "4.4.0"
```

- [ ] **Step 2: 크레이트 만들기**

`crates/opl-cluster/Cargo.toml`:

```toml
[package]
name = "opl-cluster"
description = "Raft-replicated resource store for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
openraft.workspace = true
opl-types.workspace = true
redb.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/opl-cluster/src/lib.rs`:

```rust
//! Raft로 복제되는 opLite 리소스 저장소.

pub mod types;
```

- [ ] **Step 3: 실패하는 테스트 작성**

`crates/opl-cluster/src/types.rs`를 테스트만으로 시작한다. `Command`의 JSON 모양은 Raft 로그에 남는 저장 형식이므로 테스트로 고정한다.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn node_id_is_a_plain_uuid_string() {
        let id = NodeId::from(7);
        assert_eq!(id.to_string(), "00000000-0000-0000-0000-000000000007");
        assert_eq!(serde_json::to_value(id).unwrap(), json!(id.to_string()));
    }

    /// Raft 로그에 남는 형식이다. 바뀌면 이미 기록된 로그를 읽지 못한다.
    #[test]
    fn command_storage_format_is_stable() {
        let id = Uuid::from_u128(1);
        let cmd = Command::Delete {
            kind: Kind::StoragePool,
            id,
            expected_revision: Some(3),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "delete",
                "kind": "storage-pool",
                "id": id.to_string(),
                "expected_revision": 3
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    #[test]
    fn unknown_command_is_an_error_not_ignored() {
        let value = json!({"op": "rename", "kind": "vm"});
        assert!(serde_json::from_value::<Command>(value).is_err());
    }
}
```

- [ ] **Step 4: 실패 확인**

Run: `cargo test -p opl-cluster`
Expected: 컴파일 실패 (`NodeId`, `Command`가 없음)

- [ ] **Step 5: 구현**

`crates/opl-cluster/src/types.rs`의 테스트 모듈 위에 다음을 둔다. `declare_raft_types!`는 기본 `SnapshotData`로 `Cursor<Vec<u8>>`를 쓰므로 `std::io::Cursor`를 가져와야 한다.

```rust
use std::collections::BTreeMap;
use std::fmt;
use std::io::Cursor;

use opl_types::resource::{Kind, Resource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 클러스터 노드 식별자. `Node` 리소스의 id와 같은 UUID다.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct NodeId(pub Uuid);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// openraft 저장소 테스트 모음(`openraft::testing::Suite`)이 숫자로 노드를 만들 때 쓴다.
impl From<u64> for NodeId {
    fn from(n: u64) -> Self {
        NodeId(Uuid::from_u64_pair(0, n))
    }
}

/// 저장소가 다루는 종류 없는 리소스. spec과 status는 JSON 그대로 둔다.
pub type RawResource = Resource<Value, Value>;

/// Raft 로그에 기록되는 명령. 상태 머신이 커밋 순서대로 적용한다.
///
/// 저장 형식이다. 변형이나 필드를 추가할 때는 설계 문서 3.1절의 호환성 규칙을 따른다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Command {
    /// 새 리소스. `resource.revision`은 무시하고 커밋 인덱스로 채운다.
    Create { kind: Kind, resource: RawResource },
    /// spec과 labels를 바꾼다. `expected_revision`이 있으면 현재 revision과 같아야 한다.
    UpdateSpec {
        kind: Kind,
        id: Uuid,
        expected_revision: Option<u64>,
        labels: BTreeMap<String, String>,
        spec: Value,
    },
    /// status만 바꾼다. reconciler가 쓰며 revision을 확인하지 않는다.
    UpdateStatus { kind: Kind, id: Uuid, status: Value },
    Delete {
        kind: Kind,
        id: Uuid,
        expected_revision: Option<u64>,
    },
}

/// 상태 머신이 명령을 거부한 이유. 모든 노드에서 똑같이 결정된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum Rejection {
    #[error("{kind:?} {id} already exists")]
    AlreadyExists { kind: Kind, id: Uuid },
    #[error("{kind:?} name {name:?} is already taken")]
    NameTaken { kind: Kind, name: String },
    #[error("{kind:?} {id} not found")]
    NotFound { kind: Kind, id: Uuid },
    #[error("revision mismatch: expected {expected}, actual {actual}")]
    RevisionMismatch { expected: u64, actual: u64 },
}

/// 명령 적용 결과. 성공하면 새 revision(커밋된 로그 인덱스)을 담는다.
/// 명령이 아닌 엔트리(빈 엔트리, 멤버십 변경)도 `Ok(인덱스)`를 돌려준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response(pub Result<u64, Rejection>);

openraft::declare_raft_types!(
    /// opLite의 openraft 타입 설정.
    pub TypeConfig:
        D = Command,
        R = Response,
        NodeId = NodeId,
        Node = openraft::BasicNode,
);
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-cluster`
Expected: 3개 통과

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-cluster
git commit -m "feat(cluster): opl-cluster 크레이트와 Raft 명령·응답 타입"
```

---

### Task 3: watch 허브

상태 머신이 커밋한 변경을 구독자에게 알린다. 최근 변경은 메모리에 보관해, 목록을 읽은 시점(`since`) 이후부터 빈틈과 중복 없이 이어 받게 한다.

핵심 불변식: 이력에 넣는 일과 broadcast로 보내는 일을 같은 잠금 안에서 하고, 구독도 같은 잠금 안에서 한다. 그래야 구독 시점의 이력(backlog)과 이후 실시간 변경 사이에 빈틈이 없다.

**Files:**
- Create: `crates/opl-cluster/src/watch.rs`
- Modify: `crates/opl-cluster/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-cluster/src/lib.rs`에 `pub mod watch;`를 추가하고, `crates/opl-cluster/src/watch.rs`를 테스트만으로 시작한다:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;
    use serde_json::json;

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
        hub.publish(vec![put(1), put(2), put(3)]);
        let mut w = hub.subscribe(1).unwrap();
        assert_eq!(w.next().await.unwrap().revision(), 2);
        assert_eq!(w.next().await.unwrap().revision(), 3);
        hub.publish(vec![put(4)]);
        assert_eq!(w.next().await.unwrap().revision(), 4);
    }

    #[tokio::test]
    async fn skips_live_events_at_or_before_since() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(5).unwrap();
        hub.publish(vec![put(5), put(6)]);
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
        hub.publish((1..=HISTORY as u64 + 1).map(put).collect());
        assert!(matches!(
            hub.subscribe(0),
            Err(WatchError::Expired { earliest: 1, .. })
        ));
        assert!(hub.subscribe(1).is_ok());
    }

    #[tokio::test]
    async fn reset_clears_history_and_is_always_delivered() {
        let hub = WatchHub::new(0);
        hub.publish(vec![put(1), put(2)]);
        let mut w = hub.subscribe(2).unwrap();
        hub.reset(2);
        assert_eq!(w.next().await.unwrap(), WatchEvent::Reset { revision: 2 });
        assert!(matches!(hub.subscribe(1), Err(WatchError::Expired { .. })));
    }

    #[tokio::test]
    async fn slow_watcher_gets_lagged() {
        let hub = WatchHub::new(0);
        let mut w = hub.subscribe(0).unwrap();
        hub.publish((1..=HISTORY as u64 + 1).map(put).collect());
        assert_eq!(w.next().await, Err(WatchError::Lagged));
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p opl-cluster watch`
Expected: 컴파일 실패 (`WatchHub`가 없음)

- [ ] **Step 3: 구현**

테스트 모듈 위에 둔다:

```rust
//! 커밋된 리소스 변경을 구독한다. 최근 변경은 메모리에 보관해 `since` 이후부터 이어 받을 수 있다.

use std::collections::VecDeque;
use std::sync::Mutex;

use opl_types::resource::Kind;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::types::RawResource;

/// 메모리에 보관하는 최근 변경 수. 이보다 오래된 `since`는 `Expired`가 된다.
pub const HISTORY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WatchEvent {
    /// 리소스가 생성되거나 바뀌었다. `resource.revision == revision`.
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

struct History {
    events: VecDeque<WatchEvent>,
    /// 이 revision보다 큰 변경은 모두 `events`에 있다.
    earliest: u64,
}

pub(crate) struct WatchHub {
    history: Mutex<History>,
    tx: broadcast::Sender<WatchEvent>,
}

impl WatchHub {
    /// `applied`: 저장소를 연 시점의 마지막 적용 인덱스. 그 이전 변경은 보관하지 않는다.
    pub(crate) fn new(applied: u64) -> Self {
        Self {
            history: Mutex::new(History {
                events: VecDeque::new(),
                earliest: applied,
            }),
            tx: broadcast::channel(HISTORY).0,
        }
    }

    /// 커밋된 변경을 알린다. 상태 머신이 redb 커밋 뒤에 부른다.
    pub(crate) fn publish(&self, events: Vec<WatchEvent>) {
        let mut h = self.history.lock().unwrap();
        for event in events {
            if h.events.len() == HISTORY {
                let old = h.events.pop_front().expect("history is full");
                h.earliest = old.revision();
            }
            h.events.push_back(event.clone());
            let _ = self.tx.send(event);
        }
    }

    /// 스냅샷 설치 뒤에 부른다. 보관한 변경을 버리고 `Reset`을 알린다.
    pub(crate) fn reset(&self, revision: u64) {
        let mut h = self.history.lock().unwrap();
        h.events.clear();
        h.earliest = revision;
        let _ = self.tx.send(WatchEvent::Reset { revision });
    }

    pub(crate) fn subscribe(&self, since: u64) -> Result<Watch, WatchError> {
        let h = self.history.lock().unwrap();
        if since < h.earliest {
            return Err(WatchError::Expired {
                since,
                earliest: h.earliest,
            });
        }
        let backlog = h
            .events
            .iter()
            .filter(|e| e.revision() > since)
            .cloned()
            .collect();
        // 잠금을 쥔 채 구독해야 backlog와 이후 변경 사이에 빈틈이 없다.
        Ok(Watch {
            backlog,
            rx: self.tx.subscribe(),
            seen: since,
        })
    }
}

/// 변경 구독. `next()`는 revision 순서대로, `since` 이후의 변경만 돌려준다.
pub struct Watch {
    backlog: VecDeque<WatchEvent>,
    rx: broadcast::Receiver<WatchEvent>,
    seen: u64,
}

impl Watch {
    pub async fn next(&mut self) -> Result<WatchEvent, WatchError> {
        if let Some(event) = self.backlog.pop_front() {
            self.seen = event.revision();
            return Ok(event);
        }
        loop {
            match self.rx.recv().await {
                Ok(event @ WatchEvent::Reset { .. }) => {
                    self.seen = event.revision();
                    return Ok(event);
                }
                // 목록을 읽은 직후 구독하면 그 목록에 이미 반영된 변경이 올 수 있다.
                Ok(event) if event.revision() <= self.seen => continue,
                Ok(event) => {
                    self.seen = event.revision();
                    return Ok(event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(WatchError::Lagged),
                Err(broadcast::error::RecvError::Closed) => return Err(WatchError::Closed),
            }
        }
    }
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-cluster watch`
Expected: 6개 통과. `WatchHub`를 아직 아무도 쓰지 않아 dead_code 경고가 나는데, Task 4~5에서 사라진다.

- [ ] **Step 5: Commit**

```bash
git add crates/opl-cluster
git commit -m "feat(cluster): 커밋된 변경을 since부터 이어 받는 watch 허브"
```

---

### Task 4: redb 저장소 (로그, 상태 머신, 스냅샷)

openraft가 요구하는 두 저장소를 redb 파일 하나로 구현한다. openraft가 제공하는 저장소 적합성 테스트 모음(`openraft::testing::Suite`, 35개 시나리오)으로 검증한다.

**Files:**
- Create: `crates/opl-cluster/src/store.rs`, `crates/opl-cluster/src/log_store.rs`, `crates/opl-cluster/src/state_machine.rs`, `crates/opl-cluster/tests/storage_suite.rs`
- Modify: `crates/opl-cluster/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-cluster/tests/storage_suite.rs`:

```rust
//! openraft가 제공하는 저장소 적합성 테스트 모음으로 redb 저장소를 검증한다.

use openraft::StorageError;
use openraft::testing::{StoreBuilder, Suite};
use opl_cluster::store::Store;
use opl_cluster::types::{NodeId, TypeConfig};
use opl_cluster::{LogStore, StateMachine};
use tempfile::TempDir;

struct RedbBuilder;

impl StoreBuilder<TypeConfig, LogStore, StateMachine, TempDir> for RedbBuilder {
    async fn build(&self) -> Result<(TempDir, LogStore, StateMachine), StorageError<NodeId>> {
        let dir = TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("opl.redb")).unwrap();
        Ok((dir, store.log_store(), store.state_machine()))
    }
}

#[test]
fn redb_store_passes_openraft_suite() {
    Suite::test_all(RedbBuilder).unwrap();
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p opl-cluster --test storage_suite`
Expected: 컴파일 실패 (`opl_cluster::store`가 없음)

- [ ] **Step 3: `store.rs` 작성**

테이블 정의, 스키마 버전 확인, 공용 읽기 함수를 둔다. `META` 테이블 하나에 로그 쪽 키(`vote`, `committed`, `last_purged`)와 상태 머신 쪽 키(`last_applied`, `last_membership`, 스냅샷)를 함께 둔다. redb 쓰기 트랜잭션은 한 번에 하나만 열리므로 둘이 섞이지 않는다.

`crates/opl-cluster/src/store.rs`:

```rust
//! redb 파일 하나에 Raft 로그와 상태 머신을 함께 저장한다.

use std::path::Path;
use std::sync::Arc;

use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::log_store::LogStore;
use crate::state_machine::StateMachine;
use crate::types::RawResource;
use crate::watch::WatchHub;

/// 저장 형식 버전. redb 파일과 스냅샷에 기록한다. 형식이 바뀌면 올리고 마이그레이션을 추가한다.
pub const SCHEMA_VERSION: u32 = 1;

/// Raft 로그: 인덱스 → 엔트리(JSON)
pub(crate) const LOG: TableDefinition<u64, &[u8]> = TableDefinition::new("raft_log");
/// 메타데이터: 키 → 값(JSON). 키 목록은 아래 `KEY_*` 상수.
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
/// 리소스: (종류, id) → RawResource(JSON)
pub(crate) const OBJECTS: TableDefinition<(&str, u128), &[u8]> = TableDefinition::new("objects");
/// 이름 색인: (종류, 이름) → id
pub(crate) const NAMES: TableDefinition<(&str, &str), u128> = TableDefinition::new("names");

pub(crate) const KEY_SCHEMA_VERSION: &str = "schema_version";
pub(crate) const KEY_VOTE: &str = "vote";
pub(crate) const KEY_COMMITTED: &str = "committed";
pub(crate) const KEY_LAST_PURGED: &str = "last_purged";
pub(crate) const KEY_LAST_APPLIED: &str = "last_applied";
pub(crate) const KEY_LAST_MEMBERSHIP: &str = "last_membership";
pub(crate) const KEY_SNAPSHOT_META: &str = "snapshot_meta";
pub(crate) const KEY_SNAPSHOT_DATA: &str = "snapshot_data";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] redb::Error),
    #[error("encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported schema version {found} (this build supports {SCHEMA_VERSION})")]
    UnsupportedSchema { found: u32 },
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for StoreError {
            fn from(e: $t) -> Self {
                StoreError::Db(e.into())
            }
        }
    )*};
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

/// 노드 하나의 저장소. `log_store()`와 `state_machine()`을 openraft에 넘긴다.
pub struct Store {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
}

impl Store {
    /// 파일이 없으면 만들고, 있으면 스키마 버전을 확인한다.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let db = Database::create(path)?;
        let txn = db.begin_write()?;
        {
            txn.open_table(LOG)?;
            txn.open_table(OBJECTS)?;
            txn.open_table(NAMES)?;
            let mut meta = txn.open_table(META)?;
            let found: Option<u32> = get_json(&meta, KEY_SCHEMA_VERSION)?;
            match found {
                None => put_json(&mut meta, KEY_SCHEMA_VERSION, &SCHEMA_VERSION)?,
                Some(SCHEMA_VERSION) => {}
                Some(found) => return Err(StoreError::UnsupportedSchema { found }),
            }
        }
        txn.commit()?;
        let db = Arc::new(db);
        let applied = last_applied_index(&db)?;
        Ok(Self {
            db,
            watch: Arc::new(WatchHub::new(applied)),
        })
    }

    pub fn log_store(&self) -> LogStore {
        LogStore::new(self.db.clone())
    }

    pub fn state_machine(&self) -> StateMachine {
        StateMachine::new(self.db.clone(), self.watch.clone())
    }

    pub(crate) fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub(crate) fn watch_hub(&self) -> &Arc<WatchHub> {
        &self.watch
    }
}

pub(crate) fn get_json<T: DeserializeOwned>(
    table: &impl ReadableTable<&'static str, &'static [u8]>,
    key: &str,
) -> Result<Option<T>, StoreError> {
    match table.get(key)? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

pub(crate) fn put_json<T: Serialize>(
    table: &mut redb::Table<&'static str, &'static [u8]>,
    key: &str,
    value: &T,
) -> Result<(), StoreError> {
    table.insert(key, serde_json::to_vec(value)?.as_slice())?;
    Ok(())
}

fn last_applied_index(db: &Database) -> Result<u64, StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    let applied: Option<openraft::LogId<crate::types::NodeId>> = get_json(&meta, KEY_LAST_APPLIED)?;
    Ok(applied.map_or(0, |l| l.index))
}

pub(crate) fn get_object(
    db: &Database,
    kind: Kind,
    id: Uuid,
) -> Result<Option<RawResource>, StoreError> {
    let txn = db.begin_read()?;
    let objects = txn.open_table(OBJECTS)?;
    match objects.get((kind.as_str(), id.as_u128()))? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

pub(crate) fn get_object_by_name(
    db: &Database,
    kind: Kind,
    name: &str,
) -> Result<Option<RawResource>, StoreError> {
    let txn = db.begin_read()?;
    let names = txn.open_table(NAMES)?;
    let Some(id) = names.get((kind.as_str(), name))? else {
        return Ok(None);
    };
    let objects = txn.open_table(OBJECTS)?;
    match objects.get((kind.as_str(), id.value()))? {
        Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
        None => Ok(None),
    }
}

/// 한 종류의 리소스 전체와, 그 목록이 반영하는 revision(마지막 적용 인덱스).
pub(crate) fn list_objects(
    db: &Database,
    kind: Kind,
) -> Result<(u64, Vec<RawResource>), StoreError> {
    let txn = db.begin_read()?;
    let meta = txn.open_table(META)?;
    let applied: Option<openraft::LogId<crate::types::NodeId>> = get_json(&meta, KEY_LAST_APPLIED)?;
    let objects = txn.open_table(OBJECTS)?;
    let mut out = Vec::new();
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (_, v) = item?;
        out.push(serde_json::from_slice(v.value())?);
    }
    Ok((applied.map_or(0, |l| l.index), out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_store_records_schema_version_and_reopens() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db");
        drop(Store::open(&path).unwrap());
        drop(Store::open(&path).unwrap());
    }

    #[test]
    fn newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("db");
        drop(Store::open(&path).unwrap());
        {
            let db = Database::create(&path).unwrap();
            let txn = db.begin_write().unwrap();
            put_json(
                &mut txn.open_table(META).unwrap(),
                KEY_SCHEMA_VERSION,
                &99u32,
            )
            .unwrap();
            txn.commit().unwrap();
        }
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
```

- [ ] **Step 4: `log_store.rs` 작성**

`append`는 redb 커밋(fsync)이 끝난 뒤에 `callback.log_io_completed`를 부른다. openraft는 이 콜백을 받아야 엔트리가 디스크에 있다고 보고 투표와 커밋을 진행한다.

`crates/opl-cluster/src/log_store.rs`:

```rust
use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use openraft::storage::{LogFlushed, RaftLogStorage};
use openraft::{
    Entry, LogId, LogState, OptionalSend, RaftLogReader, StorageError, StorageIOError, Vote,
};
use redb::{Database, ReadableDatabase, ReadableTable};

use crate::store::{
    KEY_COMMITTED, KEY_LAST_PURGED, KEY_VOTE, LOG, META, StoreError, get_json, put_json,
};
use crate::types::{NodeId, TypeConfig};

/// Raft 로그 저장소. 모든 쓰기는 커밋(fsync)이 끝난 뒤에 완료를 알린다.
#[derive(Clone)]
pub struct LogStore {
    db: Arc<Database>,
}

impl LogStore {
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    fn read_meta<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        get_json(&meta, key)
    }

    fn write_meta<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, key, value)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn read_entries(
        &self,
        range: (Bound<u64>, Bound<u64>),
    ) -> Result<Vec<Entry<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        let mut out = Vec::new();
        for item in log.range::<u64>(range)? {
            let (_, v) = item?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    fn last_entry_id(&self) -> Result<Option<LogId<NodeId>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        match log.last()? {
            Some((_, v)) => {
                let entry: Entry<TypeConfig> = serde_json::from_slice(v.value())?;
                Ok(Some(entry.log_id))
            }
            None => Ok(None),
        }
    }

    fn append_entries(&self, entries: Vec<Entry<TypeConfig>>) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            for entry in entries {
                log.insert(entry.log_id.index, serde_json::to_vec(&entry)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// `index` 이상의 엔트리를 지운다(충돌한 로그 잘라내기).
    fn truncate_from(&self, index: u64) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            log.retain_in(index.., |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// `log_id`까지의 엔트리를 지우고 마지막으로 지운 id를 기록한다.
    fn purge_upto(&self, log_id: &LogId<NodeId>) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, KEY_LAST_PURGED, log_id)?;
            let mut log = txn.open_table(LOG)?;
            log.retain_in(..=log_id.index, |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let range = (range.start_bound().cloned(), range.end_bound().cloned());
        self.read_entries(range)
            .map_err(|e| StorageIOError::read_logs(&e).into())
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged: Option<LogId<NodeId>> = self
            .read_meta(KEY_LAST_PURGED)
            .map_err(|e| StorageIOError::read_logs(&e))?;
        let last = self
            .last_entry_id()
            .map_err(|e| StorageIOError::read_logs(&e))?
            .or(last_purged);
        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.write_meta(KEY_VOTE, vote)
            .map_err(|e| StorageIOError::write_vote(&e).into())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        self.read_meta(KEY_VOTE)
            .map_err(|e| StorageIOError::read_vote(&e).into())
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        self.write_meta(KEY_COMMITTED, &committed)
            .map_err(|e| StorageIOError::write(&e).into())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        let committed: Option<Option<LogId<NodeId>>> = self
            .read_meta(KEY_COMMITTED)
            .map_err(|e| StorageIOError::read(&e))?;
        Ok(committed.flatten())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        self.append_entries(entries.into_iter().collect())
            .map_err(|e| StorageIOError::write_logs(&e))?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.truncate_from(log_id.index)
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.purge_upto(&log_id)
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}
```

- [ ] **Step 5: `state_machine.rs` 작성**

- `apply`는 엔트리 묶음을 트랜잭션 하나로 적용하고, 커밋한 **뒤에** watch로 알린다(커밋 전에 알리면 구독자가 아직 읽을 수 없는 변경을 보게 된다).
- 명령 거부는 `Response(Err(Rejection))`으로 돌려주고 `last_applied`는 그대로 전진한다. `StorageError`는 디스크나 직렬화 오류일 때만 쓴다. `StorageError`를 돌려주면 openraft가 노드를 멈추기 때문이다.
- 스냅샷은 읽기 트랜잭션 하나에서 상태 전체를 떠서 만들고, redb에 저장해 재시작 뒤에도 `get_current_snapshot`으로 내줄 수 있게 한다.
- `install_snapshot`은 리소스·이름 색인을 비우고 스냅샷 내용으로 채운 뒤, 같은 트랜잭션에서 `last_applied`와 멤버십을 기록한다. 그다음 watch에 `Reset`을 보낸다.

`crates/opl-cluster/src/state_machine.rs`:

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

use crate::store::{
    KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA, KEY_SNAPSHOT_META, META, NAMES,
    OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{WatchEvent, WatchHub};

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;

/// 스냅샷 본문. 상태 머신의 리소스 전체를 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
}

/// 리소스 상태 머신. 엔트리 묶음 하나를 redb 트랜잭션 하나로 적용하므로
/// 리소스와 `last_applied`는 항상 함께 기록된다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>) -> Self {
        Self { db, watch }
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
        &self,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Vec<WatchEvent>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let txn = self.db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        put_json(
                            &mut meta,
                            KEY_LAST_MEMBERSHIP,
                            &Membership::new(Some(entry.log_id), m),
                        )?;
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
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        Ok((responses, events))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷을 만들고 저장한다.
    fn build(&self) -> Result<Snapshot<TypeConfig>, StoreError> {
        let txn = self.db.begin_read()?;
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
        self.save_snapshot(&meta, &data)?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }

    fn save_snapshot(&self, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
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
    fn install(&self, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        if snapshot.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: snapshot.schema_version,
            });
        }
        let txn = self.db.begin_write()?;
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

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        self.build()
            .map_err(|e| StorageIOError::write_snapshot(None, &e).into())
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
        let (responses, events) = self
            .apply_entries(entries.into_iter().collect())
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        self.watch.publish(events);
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
        self.install(meta, snapshot.into_inner())
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
```

- [ ] **Step 6: `lib.rs` 갱신**

```rust
//! Raft로 복제되는 opLite 리소스 저장소.

mod log_store;
mod state_machine;
pub mod store;
pub mod types;
pub mod watch;

pub use log_store::LogStore;
pub use state_machine::StateMachine;
```

- [ ] **Step 7: 통과 확인**

Run: `cargo test -p opl-cluster`
Expected: 단위 테스트 11개, `storage_suite` 1개 통과. 테스트 모음은 커밋마다 fsync해서 macOS에서 10초 남짓 걸린다. `Store::db`, `get_object` 같은 읽기 함수는 Task 5에서 쓰기 전까지 dead_code 경고가 난다.

- [ ] **Step 8: Commit**

```bash
git add crates/opl-cluster
git commit -m "feat(cluster): redb 로그 저장소와 리소스 상태 머신, openraft 저장소 테스트 모음 통과"
```

---

### Task 5: `Cluster` API와 인메모리 네트워크

`Cluster`는 opld의 다른 부분(reconciler, API)이 쓰는 진입점이다. 쓰기는 Raft로 제안하고 커밋·적용을 기다린 뒤 새 revision을 돌려준다. 읽기는 로컬 상태 머신에서 한다.

**Files:**
- Create: `crates/opl-cluster/src/cluster.rs`, `crates/opl-cluster/src/memnet.rs`, `crates/opl-cluster/tests/common/mod.rs`, `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-cluster/src/lib.rs`

- [ ] **Step 1: 테스트 하네스 작성**

여러 노드를 한 프로세스에서 띄운다. 노드마다 데이터 디렉터리가 고정이라 `stop` 후 `open`하면 재시작과 같다. 선거 시간은 테스트를 빠르게 하려고 짧게 잡는다.

`crates/opl-cluster/tests/common/mod.rs`:

```rust
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
```

- [ ] **Step 2: 실패하는 테스트 작성**

`crates/opl-cluster/tests/single_node.rs`:

```rust
mod common;

use std::collections::BTreeMap;

use common::{TestCluster, node, volume};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{WatchError, WatchEvent};
use opl_types::resource::Kind;
use opl_types::storage::{VolumeSpec, VolumeStatus};

#[tokio::test]
async fn create_get_update_delete_round_trip() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));

    let mut v = volume("disk-a");
    let rev = c.create(&v).await.unwrap();
    v.revision = rev;
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap(), Some(v.clone()));
    assert_eq!(
        c.get_by_name::<VolumeSpec>("disk-a").unwrap(),
        Some(v.clone())
    );

    let mut spec = v.spec.clone();
    spec.size_bytes *= 2;
    let labels = BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    let rev2 = c
        .update_spec(v.id, Some(rev), labels.clone(), &spec)
        .await
        .unwrap();
    assert!(rev2 > rev);

    let status = VolumeStatus {
        allocated_bytes: Some(4096),
        attached_to: None,
    };
    let rev3 = c.update_status::<VolumeSpec>(v.id, &status).await.unwrap();
    let got = c.get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev3);
    assert_eq!(got.spec, spec);
    assert_eq!(got.labels, labels);
    assert_eq!(got.status, status);

    let (list_rev, items) = c.list::<VolumeSpec>().unwrap();
    assert_eq!(list_rev, rev3);
    assert_eq!(items, vec![got]);

    c.delete::<VolumeSpec>(v.id, Some(rev3)).await.unwrap();
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap(), None);
    assert_eq!(c.get_by_name::<VolumeSpec>("disk-a").unwrap(), None);
    // 삭제한 이름은 다시 쓸 수 있다.
    c.create(&volume("disk-a")).await.unwrap();
}

#[tokio::test]
async fn conflicting_writes_are_rejected() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let v = volume("disk-a");
    let rev = c.create(&v).await.unwrap();

    let err = c.create(&v).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::AlreadyExists { kind: Kind::Volume, id }) if id == v.id
    ));

    let err = c.create(&volume("disk-a")).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::NameTaken { .. })
    ));

    let err = c
        .update_spec(v.id, Some(rev + 100), BTreeMap::new(), &v.spec)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::RevisionMismatch { expected, actual })
            if expected == rev + 100 && actual == rev
    ));

    let err = c
        .delete::<VolumeSpec>(v.id, Some(rev + 100))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::RevisionMismatch { .. })
    ));

    let missing = volume("ghost").id;
    let err = c
        .update_status::<VolumeSpec>(missing, &VolumeStatus::default())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::NotFound { .. })
    ));

    // 거부된 명령은 아무것도 바꾸지 않는다.
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap().unwrap().revision, rev);
}

#[tokio::test]
async fn data_survives_restart() {
    let mut tc = TestCluster::start(1).await;
    let v = volume("disk-a");
    let rev = tc.get(node(1)).create(&v).await.unwrap();

    tc.stop(node(1)).await;
    tc.open(node(1)).await;
    tc.leader().await;

    let c = tc.get(node(1));
    let got = c.get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
    let rev2 = c.create(&volume("disk-b")).await.unwrap();
    assert!(rev2 > rev, "revisions keep increasing after restart");
}

#[tokio::test]
async fn watch_follows_list_without_gaps_or_duplicates() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let a = volume("disk-a");
    c.create(&a).await.unwrap();

    let (rev, items) = c.list::<VolumeSpec>().unwrap();
    assert_eq!(items.len(), 1);
    let mut w = c.watch(rev).unwrap();

    let b = volume("disk-b");
    let rev_b = c.create(&b).await.unwrap();
    let rev_del = c.delete::<VolumeSpec>(a.id, None).await.unwrap();

    match w.next().await.unwrap() {
        WatchEvent::Put { kind, resource } => {
            assert_eq!(kind, Kind::Volume);
            assert_eq!(resource.id, b.id);
            assert_eq!(resource.revision, rev_b);
        }
        other => panic!("unexpected {other:?}"),
    }
    match w.next().await.unwrap() {
        WatchEvent::Deleted {
            revision, resource, ..
        } => {
            assert_eq!(revision, rev_del);
            assert_eq!(resource.id, a.id);
        }
        other => panic!("unexpected {other:?}"),
    }

    // 지난 revision부터 다시 구독하면 보관한 변경을 다시 받는다.
    let mut again = c.watch(rev).unwrap();
    assert_eq!(again.next().await.unwrap().revision(), rev_b);
}

#[tokio::test]
async fn watch_history_does_not_survive_restart() {
    let mut tc = TestCluster::start(1).await;
    let rev = tc.get(node(1)).create(&volume("disk-a")).await.unwrap();
    tc.stop(node(1)).await;
    tc.open(node(1)).await;
    tc.leader().await;

    let c = tc.get(node(1));
    assert!(matches!(c.watch(rev - 1), Err(WatchError::Expired { .. })));
    let (list_rev, _) = c.list::<VolumeSpec>().unwrap();
    assert!(c.watch(list_rev).is_ok());
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p opl-cluster --test single_node`
Expected: 컴파일 실패 (`opl_cluster::Cluster`, `opl_cluster::memnet`이 없음)

- [ ] **Step 4: `memnet.rs` 작성**

RPC를 대상 노드의 `Raft` 핸들에 바로 넘긴다. `isolate`한 노드로 가거나 그 노드에서 나가는 RPC는 `Unreachable`이 되어 네트워크 분할을 흉내 낸다.

`crates/opl-cluster/src/memnet.rs`:

```rust
//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 계획 6에서 만든다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Raft};

use crate::types::{NodeId, TypeConfig};

#[derive(Default)]
struct Routes {
    nodes: HashMap<NodeId, Raft<TypeConfig>>,
    isolated: HashSet<NodeId>,
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

    pub fn register(&self, id: NodeId, raft: Raft<TypeConfig>) {
        self.routes.lock().unwrap().nodes.insert(id, raft);
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    pub fn heal(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.remove(&id);
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<Raft<TypeConfig>, Unreachable> {
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
}

fn unreachable(why: &str) -> Unreachable {
    Unreachable::new(&std::io::Error::other(why.to_string()))
}

pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            router: self.router.clone(),
        }
    }
}

pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    router: MemRouter,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let raft = self.router.route(self.from, self.target)?;
        raft.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

- [ ] **Step 5: `cluster.rs` 작성**

`crates/opl-cluster/src/cluster.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, ClientWriteError, RaftError};
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Resource, ResourceSpec};
use redb::Database;
use uuid::Uuid;

use crate::store::{Store, StoreError, get_object, get_object_by_name, list_objects};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{Watch, WatchError, WatchHub};

/// spec 타입 `S`의 리소스.
pub type Typed<S> = Resource<S, <S as ResourceSpec>::Status>;

/// 저장 파일 이름. `data_dir` 아래에 둔다.
pub const DB_FILE: &str = "cluster.redb";

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    pub node_id: NodeId,
    pub data_dir: PathBuf,
    pub raft: openraft::Config,
    /// 쓰기가 커밋되기를 기다리는 최대 시간. 과반수가 없으면 이 시간 뒤 `Timeout`이 된다.
    pub write_timeout: Duration,
}

impl ClusterConfig {
    pub fn new(node_id: NodeId, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            node_id,
            data_dir: data_dir.into(),
            raft: openraft::Config::default(),
            write_timeout: Duration::from_secs(5),
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
    raft: Raft<TypeConfig>,
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    write_timeout: Duration,
}

impl Cluster {
    /// 저장소를 열고 Raft를 시작한다. 처음 만든 클러스터는 `initialize`나
    /// 다른 노드의 `add_learner`로 멤버십을 받기 전까지 아무것도 하지 않는다.
    pub async fn open<N>(config: ClusterConfig, network: N) -> Result<Self, ClusterError>
    where
        N: RaftNetworkFactory<TypeConfig>,
    {
        std::fs::create_dir_all(&config.data_dir)?;
        let store = Store::open(&config.data_dir.join(DB_FILE))?;
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
            raft,
            db: store.db().clone(),
            watch: store.watch_hub().clone(),
            write_timeout: config.write_timeout,
        })
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    /// 네트워크 계층이 받은 RPC를 넘기고, 테스트가 메트릭을 볼 때 쓴다.
    pub fn raft(&self) -> &Raft<TypeConfig> {
        &self.raft
    }

    /// 새 클러스터를 만든다. 첫 노드에서 한 번만 부른다.
    pub async fn initialize(
        &self,
        members: BTreeMap<NodeId, BasicNode>,
    ) -> Result<(), ClusterError> {
        self.raft
            .initialize(members)
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// 노드를 learner로 추가하고 로그를 따라잡을 때까지 기다린다. 리더에서 부른다.
    pub async fn add_learner(&self, id: NodeId, node: BasicNode) -> Result<(), ClusterError> {
        self.raft.add_learner(id, node, true).await?;
        Ok(())
    }

    /// 투표 멤버를 `voters`로 바꾼다. 빠진 노드는 learner로 남는다. 리더에서 부른다.
    pub async fn change_membership(&self, voters: BTreeSet<NodeId>) -> Result<(), ClusterError> {
        self.raft.change_membership(voters, true).await?;
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
    pub async fn write(&self, command: Command) -> Result<u64, ClusterError> {
        let resp = tokio::time::timeout(self.write_timeout, self.raft.client_write(command))
            .await
            .map_err(|_| ClusterError::Timeout)??;
        let Response(result) = resp.data;
        Ok(result?)
    }

    /// 로컬 상태 머신에서 읽는다. 팔로워에서는 최신 커밋이 아직 반영되지 않았을 수 있다.
    /// 최신 값이 필요하면 먼저 `read_barrier`를 부른다.
    pub fn get<S: ResourceSpec>(&self, id: Uuid) -> Result<Option<Typed<S>>, ClusterError> {
        get_object(&self.db, S::KIND, id)?.map(from_raw).transpose()
    }

    pub fn get_by_name<S: ResourceSpec>(
        &self,
        name: &str,
    ) -> Result<Option<Typed<S>>, ClusterError> {
        get_object_by_name(&self.db, S::KIND, name)?
            .map(from_raw)
            .transpose()
    }

    /// 한 종류의 리소스 전체와 그 목록의 revision. `watch(revision)`으로 이어서 구독한다.
    pub fn list<S: ResourceSpec>(&self) -> Result<(u64, Vec<Typed<S>>), ClusterError> {
        let (revision, raw) = list_objects(&self.db, S::KIND)?;
        let items = raw.into_iter().map(from_raw).collect::<Result<_, _>>()?;
        Ok((revision, items))
    }

    /// 리더에게 확인한 최신 커밋까지 로컬에 적용될 때까지 기다린다(선형화 읽기).
    pub async fn read_barrier(&self) -> Result<(), ClusterError> {
        match self.raft.ensure_linearizable().await {
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
        self.watch.subscribe(since)
    }

    pub async fn shutdown(&self) -> Result<(), ClusterError> {
        self.raft
            .shutdown()
            .await
            .map_err(|e| ClusterError::Raft(e.to_string()))
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
```

- [ ] **Step 6: `lib.rs` 갱신**

```rust
//! Raft로 복제되는 opLite 리소스 저장소.

mod cluster;
mod log_store;
pub mod memnet;
mod state_machine;
pub mod store;
pub mod types;
pub mod watch;

pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Typed};
pub use log_store::LogStore;
pub use state_machine::StateMachine;
```

- [ ] **Step 7: 통과 확인**

Run: `cargo test -p opl-cluster --test single_node`
Expected: 5개 통과. dead_code 경고가 모두 사라진다.

- [ ] **Step 8: Commit**

```bash
git add crates/opl-cluster
git commit -m "feat(cluster): 타입 있는 CRUD·멤버십·watch를 제공하는 Cluster와 인메모리 네트워크"
```

---

### Task 6: 3노드 클러스터 테스트

M3 완료 기준("3노드 인메모리 클러스터에서 리더 장애 후에도 쓰기 지속")과 설계 10절의 불변식 1("성공을 돌려준 변경은 커밋된 것이다")을 테스트로 고정한다.

| 테스트 | 확인하는 것 |
|---|---|
| `writes_continue_after_leader_failure` | 리더를 멈추면 남은 두 노드가 새 리더를 뽑고 쓰기를 받는다. 예전 리더는 돌아와서 놓친 쓰기를 따라잡는다. |
| `follower_rejects_writes_with_leader_hint` | 팔로워의 쓰기와 `read_barrier`는 `NotLeader`(리더 id와 주소 포함)가 된다. 계획 5의 전달 로직이 이 정보를 쓴다. |
| `isolated_leader_cannot_commit_and_its_write_is_discarded` | 고립된 리더의 쓰기는 `Timeout`이 되고, 분할이 풀리면 버려진다. 성공을 돌려주지 않은 쓰기가 어느 노드에도 적용되지 않는다. |
| `new_node_catches_up_from_snapshot` | 로그가 지워진 뒤 합류한 노드는 스냅샷으로 따라잡고, watch로 `Reset`을 받는다. learner를 투표 멤버로 올린 뒤에도 쓰기가 이어진다. |

**Files:**
- Create: `crates/opl-cluster/tests/three_nodes.rs`

- [ ] **Step 1: 테스트 작성**

`crates/opl-cluster/tests/three_nodes.rs`:

```rust
//! 한 프로세스 안의 3노드 클러스터: 리더 장애, 네트워크 분할, 스냅샷으로 따라잡기.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{TestCluster, addr, node, raft_config, volume};
use openraft::SnapshotPolicy;
use opl_cluster::ClusterError;
use opl_cluster::watch::WatchEvent;
use opl_types::storage::VolumeSpec;

#[tokio::test]
async fn writes_continue_after_leader_failure() {
    let mut tc = TestCluster::start(3).await;
    let old = tc.leader().await;
    let a = volume("disk-a");
    tc.get(old).create(&a).await.unwrap();

    tc.stop(old).await;
    let new = tc.leader().await;
    assert_ne!(new, old);

    let b = volume("disk-b");
    let rev_b = tc.get(new).create(&b).await.unwrap();
    for id in tc.nodes.keys().copied().collect::<Vec<_>>() {
        tc.wait_applied(id, rev_b).await;
        let c = tc.get(id);
        assert!(c.get::<VolumeSpec>(a.id).unwrap().is_some(), "{id} lost a");
        assert!(
            c.get::<VolumeSpec>(b.id).unwrap().is_some(),
            "{id} missing b"
        );
    }

    // 예전 리더가 돌아오면 놓친 쓰기를 따라잡는다.
    tc.open(old).await;
    tc.wait_applied(old, rev_b).await;
    assert!(tc.get(old).get::<VolumeSpec>(b.id).unwrap().is_some());
}

#[tokio::test]
async fn follower_rejects_writes_with_leader_hint() {
    let tc = TestCluster::start(3).await;
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();

    let err = tc
        .get(follower)
        .create(&volume("disk-a"))
        .await
        .unwrap_err();
    match err {
        ClusterError::NotLeader {
            leader_id,
            leader_addr,
        } => {
            assert_eq!(leader_id, Some(leader));
            assert!(leader_addr.is_some());
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }
    assert!(matches!(
        tc.get(follower).read_barrier().await,
        Err(ClusterError::NotLeader { .. })
    ));
}

#[tokio::test]
async fn isolated_leader_cannot_commit_and_its_write_is_discarded() {
    let tc = TestCluster::start_with(3, raft_config(), Duration::from_secs(1)).await;
    let old = tc.leader().await;

    tc.router.isolate(old);
    let lost = volume("lost");
    let err = tc.get(old).create(&lost).await.unwrap_err();
    assert!(matches!(err, ClusterError::Timeout), "got {err:?}");

    let majority: Vec<_> = tc.nodes.keys().copied().filter(|id| *id != old).collect();
    let new = tc.leader_among(&majority).await;
    let kept = volume("kept");
    let rev = tc.get(new).create(&kept).await.unwrap();

    tc.router.heal(old);
    tc.wait_applied(old, rev).await;
    for id in tc.nodes.keys().copied().collect::<Vec<_>>() {
        tc.wait_applied(id, rev).await;
        let c = tc.get(id);
        assert!(c.get::<VolumeSpec>(kept.id).unwrap().is_some());
        assert!(
            c.get::<VolumeSpec>(lost.id).unwrap().is_none(),
            "{id} applied an uncommitted write"
        );
    }
}

#[tokio::test]
async fn new_node_catches_up_from_snapshot() {
    let raft = openraft::Config {
        snapshot_policy: SnapshotPolicy::LogsSinceLast(5),
        max_in_snapshot_log_to_keep: 0,
        purge_batch_size: 1,
        ..raft_config()
    };
    let mut tc = TestCluster::start_with(3, raft, Duration::from_secs(5)).await;
    let leader = tc.leader().await;
    let mut ids = Vec::new();
    let mut last = 0;
    for i in 0..20 {
        let v = volume(&format!("disk-{i}"));
        last = tc.get(leader).create(&v).await.unwrap();
        ids.push(v.id);
    }
    // 리더가 스냅샷을 만들고 로그를 지울 때까지 기다린다.
    tc.get(leader)
        .raft()
        .wait(Some(common::WAIT))
        .metrics(
            |m| m.purged.is_some_and(|p| p.index > 1),
            "leader purged logs",
        )
        .await
        .unwrap();

    let n4 = node(4);
    tc.open(n4).await;
    let mut w = tc.get(n4).watch(0).unwrap();
    tc.get(leader).add_learner(n4, addr(4)).await.unwrap();
    tc.wait_applied(n4, last).await;

    assert!(matches!(w.next().await.unwrap(), WatchEvent::Reset { .. }));
    let (_, items) = tc.get(n4).list::<VolumeSpec>().unwrap();
    assert_eq!(items.len(), 20);
    let snapshot = tc.get(n4).raft().metrics().borrow().snapshot;
    assert!(snapshot.is_some(), "learner installed a snapshot");

    // learner를 투표 멤버로 올려도 쓰기가 이어진다.
    let voters = BTreeSet::from([node(1), node(2), node(3), n4]);
    tc.get(leader).change_membership(voters).await.unwrap();
    let rev = tc.get(leader).create(&volume("after")).await.unwrap();
    tc.wait_applied(n4, rev).await;
}
```

- [ ] **Step 2: 실행**

Run: `cargo test -p opl-cluster --test three_nodes`
Expected: 4개 통과 (3초 안팎). Task 5까지의 구현으로 통과해야 한다. 실패하면 구현을 고친다. 테스트를 느슨하게 바꾸지 않는다.

- [ ] **Step 3: 안정성 확인**

선거 타이밍에 따라 결과가 달라지는 테스트가 없는지 반복 실행한다:

```bash
for i in $(seq 1 10); do cargo test -p opl-cluster --test three_nodes 2>&1 | grep "test result"; done
```

Expected: 10번 모두 `4 passed`

- [ ] **Step 4: Commit**

```bash
git add crates/opl-cluster
git commit -m "test(cluster): 3노드 리더 장애, 네트워크 분할, 스냅샷 합류 테스트"
```

---

### Task 7: 설계 문서, README 갱신

**Files:**
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`, `README.md`

- [ ] **Step 1: 설계 문서 3.1절 갱신**

설계 문서 3.1절에서 다음 두 줄을

```markdown
- 저장 형식에는 `schema_version`을 둔다. 업그레이드 시 Raft 스냅샷을 마이그레이션하기 위해서다.
```

```markdown
- `schema_version`을 봉투 안에 둘지 저장 계층(Raft 엔트리 헤더)에 둘지는 계획 3(opl-cluster)에서 결정한다.
```

각각 다음으로 바꾼다:

```markdown
- 저장 형식 버전(`schema_version`)은 봉투가 아니라 저장 계층에 둔다. 노드의 redb 파일(`meta` 테이블)과 Raft 스냅샷 본문에 기록하고, 지원하지 않는 버전의 파일이나 스냅샷은 열지 않는다. 형식이 바뀌면 버전을 올리고 마이그레이션을 추가한다.
```

```markdown
- Raft 로그 엔트리(`Command`)는 버전 필드 없이 serde 태그로 구분한다. 모르는 명령은 무시하지 않고 오류로 처리하므로, 새 명령도 클러스터 기능 레벨이 올라간 뒤에만 기록한다.
- Raft 노드 id는 `Node` 리소스 id와 같은 UUID다(`opl_cluster::types::NodeId`).
```

- [ ] **Step 2: README 갱신**

`README.md`에서 `- 상태: 개발 초기 (M2)`를 `- 상태: 개발 초기 (M3)`으로 바꾸고, `## 문서` 목록 끝에 추가한다:

```markdown
- [계획 3: 클러스터 저장소](docs/superpowers/plans/2026-10-21-plan3-opl-cluster.md)
```

- [ ] **Step 3: 전체 검사**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

Expected: 모두 통과한다. `opl-cluster` 테스트는 단위 11개, `single_node` 5개, `storage_suite` 1개, `three_nodes` 4개다. `cargo deny`는 중복 버전 경고(`warning[duplicate]`)만 내고 `advisories ok, bans ok, licenses ok, sources ok`로 끝난다.

- [ ] **Step 4: Commit**

```bash
git add docs README.md
git commit -m "docs: schema_version 위치와 Raft 노드 id 결정을 설계에 반영"
```

---

## 완료 기준 (M3)

- [ ] `cargo test --workspace`가 통과한다 (macOS, Linux CI).
- [ ] redb 저장소가 openraft 저장소 테스트 모음을 통과한다.
- [ ] 3노드 클러스터에서 리더를 멈춰도 쓰기가 이어지고, 돌아온 노드가 따라잡는다.
- [ ] 성공을 돌려주지 않은 쓰기(고립된 리더)는 어느 노드에도 적용되지 않는다.
- [ ] 재시작해도 리소스와 revision이 유지된다.

## 다음 계획으로 넘길 사항

계획 4 (reconciler):
- 시작할 때 `list()`로 읽고 그 revision부터 `watch()`한다. `Expired`, `Lagged`, `Reset`을 받으면 목록을 다시 읽는다.
- status는 `update_status`로 쓴다. 과반수가 없으면 `Timeout`이 나므로 로컬에 버퍼링했다가 다시 쓴다(설계 5.3).
- 팔로워의 로컬 읽기는 최신 커밋보다 늦을 수 있다. 조정은 watch로 받은 변경 기준으로 하면 충분하다.

계획 5 (API):
- 팔로워가 받은 쓰기는 `NotLeader`의 `leader_addr`로 전달한다.
- status 쓰기도 revision을 올린다. spec `If-Match`가 reconciler 쓰기와 부딪치지 않게 하려면 spec 전용 revision을 둘지 검토한다.
- 리소스 이름 형식 검증(빈 이름, 길이, 문자)은 API에서 한다. 저장소는 유일성만 본다.
- `Timeout`은 503(`cluster_no_quorum`)으로, `Rejected(RevisionMismatch)`는 412로, `NameTaken`/`AlreadyExists`는 409로 옮긴다.

계획 6 (클러스터 부트스트랩):
- `memnet`을 gRPC + mTLS 네트워크로 바꾼다. `Cluster::open`은 `RaftNetworkFactory`만 받으므로 그대로 쓴다.
- `initialize`는 첫 노드에서 한 번만 부른다. 이미 초기화된 노드에서 부르면 오류가 난다.
