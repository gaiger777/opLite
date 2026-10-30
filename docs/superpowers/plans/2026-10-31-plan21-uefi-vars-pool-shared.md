# opLite 계획 21: UEFI 변수 따라가기와 풀의 공유 설정 바꾸기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 멈춘 채 다른 노드로 옮긴 UEFI VM이 새 노드에서 켜질 때 변수 파일을 가진 노드에서 받아 와 같은 UEFI 변수로 부팅하고, 디렉터리 풀의 `shared` 설정을 API·oplctl·웹으로 바꾼다.

**Architecture:** `VmStatus.firmware_node`가 지금 변수 파일을 가진 노드를 가리킨다(시작·마이그레이션 확정·되돌리기·복제가 쓴다). 노드 사이 RPC `UefiVars`가 그 노드의 `{data_dir}/vm/{id}/uefi-vars.fd`를 1 MiB 조각(0 조각은 길이만)으로 보내고, 받는 쪽은 임시 파일에 받아 fsync·이름 바꾸기로 바꾼다. 워커는 멈춘 VM을 새로 켤 때만, 스냅샷·복제 Task는 복사 전에 받아 오며, 실패하면 경고 뒤 로컬 파일·템플릿으로 계속한다. `PATCH /storage-pools/{key}`는 디렉터리 풀의 `shared`만 바꾸고, 공유→로컬은 볼륨이 만든 노드 밖에서 쓰이면 409다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8, utoipa, tonic(피어 서비스, 이 RPC는 이진 코덱 서버 스트림), clap; 웹은 계획 8~20과 같다(shadcn 스위치 하나 추가). 새 외부 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-10-30-live-migration-design.md` (§5·§6과 §7·§8의 계획 21 부분, "구현:" 메모 포함)
**선행 계획:** 계획 1~20 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `VmStatus.firmware_node`, RPC `UefiVars`, 받아 오기(시작·스냅샷·복제), 마이그레이션·되돌리기·복제의 기록 | 노드가 죽었을 때 변수 지키기(공유 풀의 변수 볼륨) — HA에서 다시 본다 |
| 크기가 다른 펌웨어 빌드의 변수 거절, 전송 크기 제한 | 다른 노드에 남은 변수 복사본 지우기 |
| `PATCH /storage-pools/{key}`(디렉터리 풀의 `shared`) | 풀의 `nodes`·백엔드 바꾸기 |
| oplctl `pool set`, 웹 풀 목록의 공유 스위치, 실제 시험 | 스냅샷의 변수 복사본 전송(다른 노드의 스냅샷에서 복제·되돌리기) |

## 이전 계획에서 이어받는 사항

- 계획 20: 노드 사이 RPC와 받는 쪽 처리의 모양(`IncomingMigrations`), 워커 보류와 `postmigrate` 규칙, 마이그레이션 `finish()`, 한 호스트 opld 둘의 실제 시험(`migration-it`, macOS는 TCG).
- 계획 10·12: 스냅샷·복제 Task가 UEFI 변수를 Task 노드의 로컬 파일로 복사한다(`uefi_vars_path`).
- 계획 15: 풀의 `shared`와 `is_shared`, 볼륨의 `status.node`(만든 노드), 스케줄러의 디스크 규칙.

## 실행 환경

계획 20과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 실제 시험(`opld`의 `migration-it`)은 이 Mac에서 TCG로 돈다. 새 CI 단계는 없다(`linux-kvm`이 이미 `migration-it`을 돌린다).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **변수는 VM을 따라 복사한다**(공유 볼륨이 아니라). 라이브 마이그레이션은 QEMU가 pflash로 옮기고(계획 20), 멈춘 채 옮기면 새 노드가 시작 때 받아 온다. 받아 오지 못해도 시작을 막지 않는다.
- **`firmware_node`는 QEMU가 뜬 뒤 쓴다.** 받기 전에 쓰면 그 사이 opld가 죽었을 때 다른 노드를 가리키던 값을 잃는다. 이 노드에서 UEFI VM이 돌면 언제나 자기로 고쳐 쓴다(넘겨받기가 원본의 마지막 기록보다 먼저인 경우).
- **실행 중인 QEMU의 파일은 바꾸지 않는다.** 워커는 새로 켤 때만, Task는 QEMU가 그 노드에서 돌지 않을 때만 받아 온다.
- **안전한 파일 바꾸기:** 임시 파일, `flush`·fsync, 이름 바꾸기, 디렉터리 fsync. 이 노드의 템플릿과 크기가 다르면(다른 펌웨어 빌드) 받은 파일을 쓰지 않는다.
- **`PATCH`는 기존 풀 경로**(`/storage-pools/{key}`). 공유→로컬은 쓴 뒤 다시 검사해 다른 API 노드의 VM 옮기기와 겹치면 되돌린다.

## 파일 구조

```
crates/opl-types/src/vm.rs                                         # firmware_node
crates/opl-peer/src/{firmware.rs, server.rs, client.rs, lib.rs}, build.rs   # UefiVars RPC
crates/opl-reconciler/src/{worker.rs, lib.rs, reconciler.rs}       # FirmwareFetcher, 시작 때 받아 오기
crates/opl-api/src/{firmware.rs, tasks.rs, tasks/{snapshot,clone,migrate}.rs, resources.rs, lib.rs}   # 받아 오기, PATCH
crates/opld/src/member.rs, crates/opld/tests/migration.rs          # 연결, 실제 시험
crates/oplctl/src/{cli.rs, client.rs}, web/src/{storage, components/ui}/..., web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/{2026-10-30-live-migration-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: UEFI 변수 파일 전송 RPC와 firmware_node

`VmStatus.firmware_node: Option<Uuid>`(지금 UEFI 변수 파일을 가진 노드, 없으면 빼고 쓰고 OpenAPI에서 필수 아님). opl-peer `UefiVars`: 요청은 VM id 16바이트, 응답은 1 MiB 조각 스트림(이진 코덱 — 태그 0 데이터, 태그 1 0만 있는 조각의 길이 u64 BE), 파일이 없으면 gRPC NotFound, 회원 검사. `PeerService::with_uefi_vars(state_dir)`(보내는 쪽 `uefi_vars_path`), `Peers::uefi_vars(addr, vm, dest, expect_len) -> Result<u64, UefiVarsError>`: 같은 디렉터리의 임시 파일에 받아 `flush`(tokio 쓰기 오류가 묻히지 않게)·`set_len`·fsync 뒤 이름을 바꾸고 디렉터리도 fsync, 0 조각 길이와 전체 크기(256 MiB)를 검사하고, 실패하면 원래 파일을 그대로 둔다. reconciler에 `FirmwareFetcher` trait(`ReconcilerConfig.firmware`), opl-api `UefiVarsFetcher`(오프라인으로 기록된 노드는 바로 거절), opld 연결.

**Files:**
- Create: `crates/opl-api/src/firmware.rs`
- Create: `crates/opl-peer/src/firmware.rs`
- Create: `crates/opl-reconciler/src/firmware.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-peer/build.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/codec.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/Cargo.toml`
- Modify: `crates/opl-reconciler/src/config.rs`
- Modify: `crates/opl-reconciler/src/lib.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/firmware.rs`를 만든다:

```rust
//! 다른 노드에서 UEFI 변수 파일 받아 오기(설계 §5). 변수 파일을 가진 노드(`firmware_node`)에 노드
//! 사이 RPC `UefiVars`로 받아 이 노드의 변수 파일을 바꾼다. opld가 reconciler에 넣는다.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_driver::uefi_vars_path;
use opl_peer::Peers;
use opl_reconciler::{FirmwareError, FirmwareFetcher};
use opl_types::node::NodeSpec;
use uuid::Uuid;

pub struct UefiVarsFetcher {
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    /// 이 노드의 드라이버 `state_dir`(받은 파일을 여기 둔다)
    state_dir: PathBuf,
}

impl UefiVarsFetcher {
    pub fn new(cluster: Arc<Cluster>, peers: Arc<Peers>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            cluster,
            peers,
            state_dir: state_dir.into(),
        }
    }
}

#[async_trait]
impl FirmwareFetcher for UefiVarsFetcher {
    /// 오프라인으로 기록된 노드에는 연결해 보지 않는다.
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        match self.cluster.get::<NodeSpec>(node) {
            Ok(Some(n)) if !n.status.online => {
                return Err(FirmwareError(format!("node {:?} is offline", n.name)));
            }
            Ok(_) => {}
            Err(e) => return Err(FirmwareError(format!("cannot read node {node}: {e}"))),
        }
        let addr = self
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
            .ok_or_else(|| FirmwareError(format!("node {node} is not a cluster member")))?;
        self.peers
            .uefi_vars(&addr, vm, &uefi_vars_path(&self.state_dir, vm))
            .await
            .map(|_| ())
            .map_err(|e| FirmwareError(e.to_string()))
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod error;
mod incoming;
```

을 다음으로 바꾼다:

```rust
pub mod error;
mod firmware;
mod incoming;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use incoming::{IncomingConfig, IncomingMigrations};
```

을 다음으로 바꾼다:

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use firmware::UefiVarsFetcher;
pub use incoming::{IncomingConfig, IncomingMigrations};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        },
    }
```

을 다음으로 바꾼다:

```rust
        },
        firmware: None,
    }
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    );
    let schemes = &doc["components"]["securitySchemes"];
```

을 다음으로 바꾼다:

```rust
    );
    // 변수 파일을 가진 노드도 없으면 빼고 쓰는 선택 필드다.
    assert_eq!(
        vm_status["properties"]["firmware_node"]["format"], "uuid",
        "{vm_status}"
    );
    assert!(
        !vm_status["required"]
            .as_array()
            .is_some_and(|r| r.contains(&"firmware_node".into())),
        "{vm_status}"
    );
    let schemes = &doc["components"]["securitySchemes"];
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
use openraft::BasicNode;
use opl_api::{AppState, IncomingConfig, IncomingMigrations, TaskRunner, hash_password};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::Reconciler;
use opl_types::auth::{Role, UserSpec};
```

을 다음으로 바꾼다:

```rust
use openraft::BasicNode;
use opl_api::{
    AppState, IncomingConfig, IncomingMigrations, TaskRunner, UefiVarsFetcher, hash_password,
};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage, uefi_vars_path,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, Reconciler};
use opl_types::auth::{Role, UserSpec};
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        );
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming));
        let listener = self.listener.take().unwrap();
```

을 다음으로 바꾼다:

```rust
        );
        let work = root.path().join(format!("{}-work", self.id));
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(work.join("state"));
        let listener = self.listener.take().unwrap();
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let work = root.path().join(format!("{}-work", self.id));
        let mut config = runner_config(self.id, &work);
```

을 다음으로 바꾼다:

```rust
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
```

을 다음으로 바꾼다:

```rust
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 노드의 드라이버 `state_dir`(Task 실행기 설정과 노드 간 서버가 같은 곳을 쓴다)
fn state_dir(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path().join(format!("{}-work", node.id)).join("state")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
    let (root, a, b) = two_nodes().await;
    let vm = Uuid::now_v7();
    let src = uefi_vars_path(&state_dir(&root, &b), vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars of b").unwrap();
    let fetcher = UefiVarsFetcher::new(a.cluster.clone(), a.peers.clone(), state_dir(&root, &a));
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");

    // 파일이 없거나 멤버가 아닌 노드면 실패하고 이 노드의 파일은 그대로다.
    let err = fetcher.fetch(Uuid::now_v7(), b.id).await.unwrap_err();
    assert!(err.0.contains("no UEFI variables"), "{err}");
    let err = fetcher.fetch(vm, Uuid::now_v7()).await.unwrap_err();
    assert!(err.0.contains("not a cluster member"), "{err}");

    // 오프라인으로 기록된 노드에서는 받지 않는다.
    std::fs::write(&src, b"newer").unwrap();
    let mut node = a.cluster.get::<NodeSpec>(b.id).unwrap().unwrap();
    node.status.online = false;
    a.cluster
        .update_status::<NodeSpec>(b.id, &node.status)
        .await
        .unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
```

`crates/opl-peer/build.rs`에서

```rust
//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림만 바이트를 그대로 보낸다.
```

을 다음으로 바꾼다:

```rust
//! 노드 간 gRPC 서비스 정의. `.proto` 대신 Rust 타입을 serde JSON으로 주고받는다(`protoc`가 필요 없다).
//! 콘솔 스트림은 바이트를 그대로, UEFI 변수 조각은 이진 형식으로 보낸다.
```

`crates/opl-peer/build.rs`에서

```rust
            Method::builder()
                .name("console")
```

을 다음으로 바꾼다:

```rust
            Method::builder()
                .name("uefi_vars")
                .route_name("UefiVars")
                .input_type("crate::firmware::UefiVarsRequest")
                .output_type("crate::firmware::UefiVarsChunk")
                .codec_path("crate::codec::BinaryCodec")
                .server_streaming()
                .build(),
        )
        .method(
            Method::builder()
                .name("console")
```

`crates/opl-peer/src/client.rs`에서

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
```

을 다음으로 바꾼다:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
```

`crates/opl-peer/src/client.rs`에서

```rust

use crate::migration::{IncomingError, IncomingMigration, MigrateIncomingRequest};
```

을 다음으로 바꾼다:

```rust

use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::migration::{IncomingError, IncomingMigration, MigrateIncomingRequest};
```

`crates/opl-peer/src/client.rs`에서

```rust
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);
```

을 다음으로 바꾼다:

```rust
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);

/// UEFI 변수 파일 받기(`UefiVars`)를 기다리는 최대 시간. AAVMF 변수 파일은 64 MiB지만 대부분 0이라
/// 길이만 온다.
const UEFI_VARS_TIMEOUT: Duration = Duration::from_secs(60);
```

`crates/opl-peer/src/client.rs`에서

```rust
            .map_err(unreachable)?
            .into_inner()
    }
}
```

을 다음으로 바꾼다:

```rust
            .map_err(unreachable)?
            .into_inner()
    }
}

impl Peers {
    /// `addr`의 노드에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에 받아
    /// fsync한 뒤 이름을 바꾼다). 실패하면 `dest`는 그대로다. 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(&self, addr: &str, vm: Uuid, dest: &Path) -> Result<u64, UefiVarsError> {
        let mut req = Request::new(UefiVarsRequest { vm });
        req.set_timeout(UEFI_VARS_TIMEOUT);
        let chunks = self.client(addr)?.uefi_vars(req).await?.into_inner();
        crate::firmware::write_file(chunks, dest).await
    }
}
```

`crates/opl-peer/src/codec.rs`에서

```rust
//! gRPC 메시지 형식. 보통 메시지는 JSON이고, 콘솔 스트림은 바이트 그대로다.
```

을 다음으로 바꾼다:

```rust
//! gRPC 메시지 형식. 보통 메시지는 JSON이고, 콘솔 스트림은 바이트 그대로, UEFI 변수 조각은 작은
//! 이진 형식([`Binary`])이다.
```

`crates/opl-peer/src/codec.rs`에서

```rust
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}
```

을 다음으로 바꾼다:

```rust
        Ok(Some(src.copy_to_bytes(src.remaining())))
    }
}

/// JSON 대신 이진 형식으로 보내는 메시지(큰 바이트 덩어리를 JSON 숫자 배열로 부풀리지 않는다)
pub trait Binary: Sized {
    fn encode(self, dst: &mut EncodeBuf<'_>);
    fn decode(src: Bytes) -> Result<Self, Status>;
}

pub struct BinaryCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for BinaryCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for BinaryCodec<T, U>
where
    T: Binary + Send + 'static,
    U: Binary + Send + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = BinaryEncoder<T>;
    type Decoder = BinaryDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        BinaryEncoder(PhantomData)
    }

    fn decoder(&mut self) -> Self::Decoder {
        BinaryDecoder(PhantomData)
    }
}

pub struct BinaryEncoder<T>(PhantomData<T>);

impl<T: Binary> Encoder for BinaryEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(&mut self, item: T, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst);
        Ok(())
    }
}

pub struct BinaryDecoder<U>(PhantomData<U>);

impl<U: Binary> Decoder for BinaryDecoder<U> {
    type Item = U;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<U>, Status> {
        U::decode(src.copy_to_bytes(src.remaining())).map(Some)
    }
}
```

`crates/opl-peer/src/firmware.rs`를 만든다:

```rust
//! UEFI 변수 파일 옮기기: VM을 다른 노드에서 켤 때 변수 파일을 가진 노드(`firmware_node`)에서
//! `UefiVars { vm }`로 받아 온다. 파일은 1 MiB 조각으로 보내고, 0만 있는 조각(AAVMF 변수 파일은
//! 64 MiB 대부분이 0이다)은 길이만 보낸다.

use std::io::SeekFrom;
use std::path::Path;

use bytes::{Buf, BufMut, Bytes};
use futures_util::{Stream, StreamExt};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tonic::codec::EncodeBuf;
use tonic::{Code, Status};
use uuid::Uuid;

use crate::codec::Binary;

/// 한 조각의 크기
pub const CHUNK_BYTES: usize = 1 << 20;

/// 받을 변수 파일의 최대 크기. AAVMF 변수 파일(64 MiB)보다 넉넉히 크고, 상대가 잘못된 길이를 보내도
/// 디스크를 채우지 않는다.
pub const MAX_BYTES: u64 = 256 << 20;

/// 변수 파일 요청: 어느 VM의 파일인지
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UefiVarsRequest {
    pub vm: Uuid,
}

impl Binary for UefiVarsRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(self.vm.as_bytes());
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        Uuid::from_slice(&src)
            .map(|vm| Self { vm })
            .map_err(|_| Status::invalid_argument("bad VM id"))
    }
}

/// 변수 파일의 한 조각. 순서대로 이으면 파일이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UefiVarsChunk {
    Data(Bytes),
    /// 이 길이만큼 0이다.
    Zero(u64),
}

const DATA_TAG: u8 = 0;
const ZERO_TAG: u8 = 1;

impl UefiVarsChunk {
    /// 파일에서 읽은 조각. 0만 있으면 길이만 남긴다.
    pub fn of(data: &[u8]) -> Self {
        if data.iter().all(|b| *b == 0) {
            Self::Zero(data.len() as u64)
        } else {
            Self::Data(Bytes::copy_from_slice(data))
        }
    }
}

impl Binary for UefiVarsChunk {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        match self {
            Self::Data(data) => {
                dst.put_u8(DATA_TAG);
                dst.put(data);
            }
            Self::Zero(len) => {
                dst.put_u8(ZERO_TAG);
                dst.put_u64(len);
            }
        }
    }

    fn decode(mut src: Bytes) -> Result<Self, Status> {
        let bad = || Status::invalid_argument("bad UEFI variables chunk");
        if src.is_empty() {
            return Err(bad());
        }
        match src.get_u8() {
            DATA_TAG => Ok(Self::Data(src)),
            ZERO_TAG if src.len() == 8 => Ok(Self::Zero(src.get_u64())),
            _ => Err(bad()),
        }
    }
}

/// 변수 파일을 받지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UefiVarsError {
    /// 그 노드에 이 VM의 변수 파일이 없다.
    #[error("the node has no UEFI variables for this VM")]
    NotFound,
    /// 그 노드에 닿지 못했거나 보내는 중에 끊겼다.
    #[error("cannot fetch the UEFI variables: {0}")]
    Unavailable(String),
    /// 받은 파일을 이 노드에 쓰지 못했다.
    #[error("cannot write the UEFI variables: {0}")]
    Io(String),
    /// 받은 파일을 쓸 수 없다(너무 크다 등).
    #[error("bad UEFI variables: {0}")]
    Invalid(String),
}

impl From<Status> for UefiVarsError {
    fn from(s: Status) -> Self {
        match s.code() {
            Code::NotFound => Self::NotFound,
            _ => Self::Unavailable(s.message().to_string()),
        }
    }
}

fn io(e: std::io::Error) -> UefiVarsError {
    UefiVarsError::Io(e.to_string())
}

/// 보내는 쪽: `file`을 끝까지 조각으로 보낸다. 받는 쪽이 끊으면 멈춘다.
pub(crate) async fn send_file(mut file: File, tx: mpsc::Sender<Result<UefiVarsChunk, Status>>) {
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = match read_full(&mut file, &mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tx
                    .send(Err(Status::internal(format!(
                        "cannot read the UEFI variables: {e}"
                    ))))
                    .await;
                break;
            }
        };
        if tx.send(Ok(UefiVarsChunk::of(&buf[..n]))).await.is_err() || n < buf.len() {
            break;
        }
    }
}

/// `buf`가 차거나 파일이 끝날 때까지 읽는다.
async fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]).await? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

/// 받는 쪽: 조각을 `dest`와 같은 디렉터리의 임시 파일에 쓰고 fsync한 뒤 `dest`로 이름을 바꾼다.
/// 받다가 실패하면 `dest`는 그대로다. 받은 파일의 크기를 돌려준다.
pub(crate) async fn write_file<S>(mut chunks: S, dest: &Path) -> Result<u64, UefiVarsError>
where
    S: Stream<Item = Result<UefiVarsChunk, Status>> + Unpin,
{
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(UefiVarsError::Io(format!("bad path {}", dest.display())));
    };
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    // 같은 VM을 동시에 받아도 임시 파일이 겹치지 않게 이름마다 다르게 둔다.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    let written = async {
        let mut file = File::create_new(&tmp).await.map_err(io)?;
        let mut len = 0u64;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let n = match &chunk {
                UefiVarsChunk::Data(data) => data.len() as u64,
                UefiVarsChunk::Zero(n) => *n,
            };
            len = len
                .checked_add(n)
                .filter(|len| *len <= MAX_BYTES)
                .ok_or_else(|| UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes")))?;
            match chunk {
                UefiVarsChunk::Data(data) => file.write_all(&data).await.map_err(io)?,
                // 건너뛴 자리는 0으로 읽힌다(끝의 0은 아래 `set_len`이 채운다).
                UefiVarsChunk::Zero(n) => {
                    let n = i64::try_from(n).map_err(|_| {
                        UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes"))
                    })?;
                    file.seek(SeekFrom::Current(n)).await.map_err(io)?;
                }
            }
        }
        // tokio `File`은 쓰기를 뒤에서 하므로, 그 오류는 flush가 돌려준다. `set_len`·`sync_all`은
        // 남은 쓰기를 기다리기만 하고 그 오류를 버린다.
        file.flush().await.map_err(io)?;
        file.set_len(len).await.map_err(io)?;
        file.sync_all().await.map_err(io)?;
        drop(file);
        tokio::fs::rename(&tmp, dest).await.map_err(io)?;
        // 이름 바꾸기가 전원이 나가도 남게 디렉터리도 fsync한다.
        File::open(dir)
            .await
            .map_err(io)?
            .sync_all()
            .await
            .map_err(io)?;
        Ok(len)
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;

    #[test]
    fn zero_chunks_carry_only_their_length() {
        assert_eq!(UefiVarsChunk::of(&[0u8; 4096]), UefiVarsChunk::Zero(4096));
        let mut data = vec![0u8; 4096];
        data[4095] = 7;
        assert_eq!(
            UefiVarsChunk::of(&data),
            UefiVarsChunk::Data(Bytes::from(data))
        );
    }

    #[test]
    fn chunks_decode_and_reject_garbage() {
        let mut data = BytesMut::new();
        data.put_u8(ZERO_TAG);
        data.put_u64(CHUNK_BYTES as u64);
        assert_eq!(
            UefiVarsChunk::decode(data.freeze()).unwrap(),
            UefiVarsChunk::Zero(CHUNK_BYTES as u64)
        );
        assert_eq!(
            UefiVarsChunk::decode(Bytes::from_static(b"\0abc")).unwrap(),
            UefiVarsChunk::Data(Bytes::from_static(b"abc"))
        );
        for bad in [&b""[..], b"\x01\0\0", b"\x02abc"] {
            assert!(UefiVarsChunk::decode(Bytes::copy_from_slice(bad)).is_err());
        }
        assert!(UefiVarsRequest::decode(Bytes::from_static(b"short")).is_err());
    }

    fn stream(
        items: Vec<Result<UefiVarsChunk, Status>>,
    ) -> impl Stream<Item = Result<UefiVarsChunk, Status>> + Unpin {
        futures_util::stream::iter(items)
    }

    #[tokio::test]
    async fn received_chunks_replace_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vm").join("uefi-vars.fd");
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"head"))),
            Ok(UefiVarsChunk::Zero(10)),
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"mid"))),
            // 끝의 0도 파일 크기에 든다.
            Ok(UefiVarsChunk::Zero(5)),
        ];
        assert_eq!(write_file(stream(chunks), &dest).await.unwrap(), 22);
        let mut want = b"head".to_vec();
        want.extend([0u8; 10]);
        want.extend(b"mid");
        want.extend([0u8; 5]);
        assert_eq!(std::fs::read(&dest).unwrap(), want);

        // 있던 파일을 바꾼다.
        let chunks = vec![Ok(UefiVarsChunk::Data(Bytes::from_static(b"new")))];
        assert_eq!(write_file(stream(chunks), &dest).await.unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(dest.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[tokio::test]
    async fn oversized_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        for chunks in [
            // 합이 최대 크기를 넘는다.
            vec![
                Ok(UefiVarsChunk::Data(Bytes::from_static(b"x"))),
                Ok(UefiVarsChunk::Zero(MAX_BYTES)),
            ],
            // i64로 seek할 수 없는 길이
            vec![Ok(UefiVarsChunk::Zero(u64::MAX))],
            // 합이 u64를 넘친다.
            vec![
                Ok(UefiVarsChunk::Zero(1)),
                Ok(UefiVarsChunk::Zero(u64::MAX)),
            ],
        ] {
            let err = write_file(stream(chunks), &dest).await.unwrap_err();
            assert!(matches!(err, UefiVarsError::Invalid(_)), "{err:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), b"old");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        // 최대 크기까지는 받는다.
        let chunks = vec![Ok(UefiVarsChunk::Zero(MAX_BYTES))];
        assert_eq!(write_file(stream(chunks), &dest).await.unwrap(), MAX_BYTES);
    }

    #[tokio::test]
    async fn broken_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"partial"))),
            Err(Status::unavailable("connection reset")),
        ];
        let err = write_file(stream(chunks), &dest).await.unwrap_err();
        assert_eq!(err, UefiVarsError::Unavailable("connection reset".into()));
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        // 임시 파일도 남기지 않는다.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
```

`crates/opl-peer/src/lib.rs`에서

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기다.

mod client;
mod codec;
pub mod migration;
```

을 다음으로 바꾼다:

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기, UEFI 변수
//! 파일 보내기다.

mod client;
mod codec;
pub mod firmware;
pub mod migration;
```

`crates/opl-peer/src/lib.rs`에서

```rust
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use migration::{
```

을 다음으로 바꾼다:

```rust
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use firmware::UefiVarsError;
pub use migration::{
```

`crates/opl-peer/src/server.rs`에서

```rust
use std::future::Future;
use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
```

`crates/opl-peer/src/server.rs`에서

```rust
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
```

`crates/opl-peer/src/server.rs`에서

```rust

use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
```

을 다음으로 바꾼다:

```rust

use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
```

`crates/opl-peer/src/server.rs`에서

```rust
    migrations: Option<Arc<dyn MigrationReceiver>>,
}
```

을 다음으로 바꾼다:

```rust
    migrations: Option<Arc<dyn MigrationReceiver>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
    state_dir: Option<PathBuf>,
}
```

`crates/opl-peer/src/server.rs`에서

```rust
            migrations: None,
        }
    }
```

을 다음으로 바꾼다:

```rust
            migrations: None,
            state_dir: None,
        }
    }

    /// 드라이버의 `state_dir`에 있는 VM의 UEFI 변수 파일을 보낸다. 없으면 보내기 요청을 거부한다.
    pub fn with_uefi_vars(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(state_dir.into());
        self
    }
```

`crates/opl-peer/src/server.rs`에서

```rust

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;
```

을 다음으로 바꾼다:

```rust

    type UefiVarsStream = ReceiverStream<Result<UefiVarsChunk, Status>>;

    async fn uefi_vars(
        &self,
        req: Request<UefiVarsRequest>,
    ) -> Result<Response<Self::UefiVarsStream>, Status> {
        self.authorize(&req)?;
        let dir = self
            .state_dir
            .as_ref()
            .ok_or_else(|| Status::unimplemented("this node does not serve UEFI variables"))?;
        let vm = req.into_inner().vm;
        let file = match tokio::fs::File::open(uefi_vars_path(dir, vm)).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Status::not_found(format!(
                    "no UEFI variables for VM {vm} on this node"
                )));
            }
            Err(e) => {
                return Err(Status::internal(format!(
                    "cannot open the UEFI variables of VM {vm}: {e}"
                )));
            }
        };
        let (tx, rx) = mpsc::channel(4);
        tokio::spawn(crate::firmware::send_file(file, tx));
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
use std::collections::BTreeMap;
use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    GrpcNetwork, IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver,
    NodeMonitor, NodeMonitorConfig, PeerService, Peers,
};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    GrpcNetwork, IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver,
    NodeMonitor, NodeMonitorConfig, PeerService, Peers, UefiVarsError,
};
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    hv: Arc<MockHypervisor>,
    stop: Option<oneshot::Sender<()>>,
```

을 다음으로 바꾼다:

```rust
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        let hv = Arc::new(MockHypervisor::default());
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
        if let Some(migrations) = migrations {
```

을 다음으로 바꾼다:

```rust
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
            hv,
            stop: Some(stop),
```

을 다음으로 바꾼다:

```rust
            hv,
            state_dir,
            stop: Some(stop),
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    assert_eq!(receiver.calls.lock().unwrap().len(), 4);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(receiver.calls.lock().unwrap().len(), 4);
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
    let len = to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap(), 0);
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}
```

`crates/opl-reconciler/Cargo.toml`에서

```toml
[dependencies]
opl-cluster.workspace = true
```

을 다음으로 바꾼다:

```toml
[dependencies]
async-trait.workspace = true
opl-cluster.workspace = true
```

`crates/opl-reconciler/Cargo.toml`에서

```toml
[dev-dependencies]
async-trait.workspace = true
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
[dev-dependencies]
openraft.workspace = true
```

`crates/opl-reconciler/src/config.rs`에서

```rust
use opl_driver::Storage;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_driver::Storage;

use crate::firmware::FirmwareFetcher;
use uuid::Uuid;
```

`crates/opl-reconciler/src/config.rs`에서

```rust
    pub restart: RestartBackoff,
}
```

을 다음으로 바꾼다:

```rust
    pub restart: RestartBackoff,
    /// 다른 노드에 있는 UEFI 변수 파일을 받아 온다. 없으면 받지 않고 이 노드의 파일로 켠다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}
```

`crates/opl-reconciler/src/config.rs`에서

```rust
            },
        }
```

을 다음으로 바꾼다:

```rust
            },
            firmware: None,
        }
```

`crates/opl-reconciler/src/firmware.rs`를 만든다:

```rust
//! 다른 노드에서 UEFI 변수 파일 받아 오기. 노드 사이 RPC는 opld가 [`FirmwareFetcher`]로 넣는다
//! (reconciler는 노드 사이 통신을 모른다).

use async_trait::async_trait;
use uuid::Uuid;

/// VM의 UEFI 변수 파일을 그 파일을 가진 노드(`status.firmware_node`)에서 받아 온다.
#[async_trait]
pub trait FirmwareFetcher: Send + Sync {
    /// `node`에 있는 `vm`의 변수 파일을 받아 이 노드의 변수 파일을 바꾼다. 실패하면 이 노드의
    /// 파일은 그대로다.
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError>;
}

/// 변수 파일을 받지 못한 이유(그 노드가 오프라인이다, 파일이 없다, 전송이 끊겼다 등)
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FirmwareError(pub String);
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
mod config;
mod reconciler;
```

을 다음으로 바꾼다:

```rust
mod config;
mod firmware;
mod reconciler;
```

`crates/opl-reconciler/src/lib.rs`에서

```rust
pub use config::{ReconcilerConfig, RestartBackoff};
pub use reconciler::{Reconciler, ReconcilerHandle};
```

을 다음으로 바꾼다:

```rust
pub use config::{ReconcilerConfig, RestartBackoff};
pub use firmware::{FirmwareError, FirmwareFetcher};
pub use reconciler::{Reconciler, ReconcilerHandle};
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            migration: None,
        };
```

을 다음으로 바꾼다:

```rust
            migration: None,
            firmware_node: vm.status.firmware_node,
        };
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
        },
    }
```

을 다음으로 바꾼다:

```rust
        },
        firmware: None,
    }
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
            migration: None,
        }
```

을 다음으로 바꾼다:

```rust
            migration: None,
            firmware_node: None,
        }
```

`crates/opl-types/src/vm.rs`에서

```rust
    pub migration: Option<MigrationStatus>,
}
```

을 다음으로 바꾼다:

```rust
    pub migration: Option<MigrationStatus>,
    /// 지금 UEFI 변수 파일을 가진 노드. UEFI VM을 켠 노드나 마이그레이션의 대상 노드가 쓴다.
    /// 다른 노드에서 켤 때 이 노드에서 변수를 받아 온다. BIOS VM은 늘 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub firmware_node: Option<Uuid>,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
        assert_eq!(st.migration, None);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(st.migration, None);
        assert_eq!(st.firmware_node, None);
    }

    #[test]
    fn firmware_node_is_written_only_when_known() {
        let json = serde_json::to_value(VmStatus::default()).unwrap();
        assert!(json.get("firmware_node").is_none(), "{json}");
        let node = Uuid::now_v7();
        let st = VmStatus {
            firmware_node: Some(node),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["firmware_node"], serde_json::json!(node));
        assert_eq!(serde_json::from_value::<VmStatus>(json).unwrap(), st);
    }
```

`crates/opld/src/member.rs`에서

```rust
    AppState, ClusterJanitor, IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig,
};
```

을 다음으로 바꾼다:

```rust
    AppState, ClusterJanitor, IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, UefiVarsFetcher,
};
```

`crates/opld/src/member.rs`에서

```rust
        );
        let service =
            PeerService::new(cluster.clone(), Some(hv.clone())).with_migrations(Arc::new(incoming));
        let peer_tls = tls.clone();
```

을 다음으로 바꾼다:

```rust
        );
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming))
            .with_uefi_vars(config.data_dir.clone());
        let peer_tls = tls.clone();
```

`crates/opld/src/member.rs`에서

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

을 다음으로 바꾼다:

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다.
        reconciler_config.firmware = Some(Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
        )));
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 735개, 실패 0

Run: `cargo test -p opl-peer`
Expected: PASS

Run: `cargo test -p opl-api --test two_nodes`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 307개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(peer): UEFI 변수 파일 전송 RPC와 VmStatus.firmware_node"
```

---

### Task 2: UEFI 변수가 시작·마이그레이션·스냅샷·복제를 따라간다

워커는 멈춘 UEFI VM을 새로 켤 때만(`start()`, 디스크 resolve 뒤 QEMU 띄우기 직전) `firmware_node`가 다른 노드면 받아 온다 — 다시 붙기·넘겨받기·재개·마이그레이션 보류에서는 받지 않는다. 받아 오기 실패(오프라인, 파일 없음, 이 노드 템플릿과 크기가 다름 — 펌웨어 빌드가 다른 노드)는 경고 뒤 로컬 파일이나 템플릿으로 켠다. QEMU가 뜬 뒤(그리고 이 노드에서 UEFI VM QEMU가 도는 동안) `firmware_node = 자기`, 남이 쓴 값은 상태 쓰기에서 보존한다. 마이그레이션 `finish()`는 대상으로 쓴다. 스냅샷·복제 Task는 복사 전에 받아 오고(`fetch_vars`, BIOS·이미 이 노드·QEMU가 여기서 돌면 안 함), 되돌리기는 자기로, 복제는 새 VM을 변수 복사본이 있는 노드로 쓴다. `TaskRunnerConfig.firmware`. 시험: reconciler(가짜 fetcher), two_nodes(실제 gRPC — 멈춘 VM 옮겨 켜기·스냅샷·복제, 크기가 다른 파일 거절, 대상 워커를 보류해 마이그레이션 `finish()`의 기록 확인).

**Files:**
- Create: `crates/opl-reconciler/tests/firmware.rs`
- Modify: `crates/opl-api/src/firmware.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/tasks/migrate.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/clones.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/firmware.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/firmware.rs`에서

```rust
    state_dir: PathBuf,
}

impl UefiVarsFetcher {
    pub fn new(cluster: Arc<Cluster>, peers: Arc<Peers>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
```

을 다음으로 바꾼다:

```rust
    state_dir: PathBuf,
    /// 이 노드의 UEFI 변수 템플릿. 받은 파일의 크기가 이와 다르면(다른 펌웨어 빌드) 쓰지 않는다.
    template: PathBuf,
}

impl UefiVarsFetcher {
    pub fn new(
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        state_dir: impl Into<PathBuf>,
        template: impl Into<PathBuf>,
    ) -> Self {
        Self {
```

`crates/opl-api/src/firmware.rs`에서

```rust
            state_dir: state_dir.into(),
        }
```

을 다음으로 바꾼다:

```rust
            state_dir: state_dir.into(),
            template: template.into(),
        }
```

`crates/opl-api/src/firmware.rs`에서

```rust
impl FirmwareFetcher for UefiVarsFetcher {
    /// 오프라인으로 기록된 노드에는 연결해 보지 않는다.
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
```

을 다음으로 바꾼다:

```rust
impl FirmwareFetcher for UefiVarsFetcher {
    /// 오프라인으로 기록된 노드에는 연결해 보지 않는다. 노드마다 펌웨어 빌드가 달라 변수 파일의 크기가
    /// 이 노드의 템플릿과 다르면 받은 것을 버린다(이 노드의 펌웨어가 그 파일로 켜지지 않는다).
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
```

`crates/opl-api/src/firmware.rs`에서

```rust
            .ok_or_else(|| FirmwareError(format!("node {node} is not a cluster member")))?;
        self.peers
            .uefi_vars(&addr, vm, &uefi_vars_path(&self.state_dir, vm))
            .await
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| FirmwareError(format!("node {node} is not a cluster member")))?;
        let len = tokio::fs::metadata(&self.template)
            .await
            .map_err(|e| {
                FirmwareError(format!(
                    "cannot read the UEFI variables template {}: {e}",
                    self.template.display()
                ))
            })?
            .len();
        self.peers
            .uefi_vars(&addr, vm, &uefi_vars_path(&self.state_dir, vm), Some(len))
            .await
```

`crates/opl-api/src/tasks.rs`에서

```rust
use opl_peer::Peers;
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
```

을 다음으로 바꾼다:

```rust
use opl_peer::Peers;
use opl_reconciler::{FirmwareFetcher, ReconcilerHandle};
use opl_types::resource::{Kind, Resource};
```

`crates/opl-api/src/tasks.rs`에서

```rust
    pub migration_timeout: Duration,
}
```

을 다음으로 바꾼다:

```rust
    pub migration_timeout: Duration,
    /// 스냅샷·복제가 UEFI 변수를 복사하기 전에, 변수 파일이 다른 노드에 있으면 받아 온다. 없으면
    /// 이 노드의 파일을 그대로 쓴다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}
```

`crates/opl-api/src/tasks.rs`에서

```rust
            migration_timeout: Duration::from_secs(30 * 60),
        }
```

을 다음으로 바꾼다:

```rust
            migration_timeout: Duration::from_secs(30 * 60),
            firmware: None,
        }
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
use super::snapshot::{
    check_local, check_settled, copy_file, get_snapshot, get_vm, is_down, ready_disks, vars_copy,
};
```

을 다음으로 바꾼다:

```rust
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_snapshot, get_vm, is_down, ready_disks,
    vars_copy,
};
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
            Some(id) => vars_copy(ctx, vm.id, id).expect("state_dir is set"),
            None => uefi_vars_path(dir, vm.id),
        };
```

을 다음으로 바꾼다:

```rust
            Some(id) => vars_copy(ctx, vm.id, id).expect("state_dir is set"),
            None => {
                fetch_vars(ctx, &vm).await;
                uefi_vars_path(dir, vm.id)
            }
        };
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        }
```

을 다음으로 바꾼다:

```rust
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
            // 새 VM의 변수는 이 노드에 있다. 다른 노드에서 켜면 여기서 받아 간다.
            new.status.firmware_node = Some(ctx.config.node);
        }
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{Firmware, MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
```

을 다음으로 바꾼다:

```rust

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로(UEFI 변수도 대상 노드에) 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
            status.pid = None;
        }
```

을 다음으로 바꾼다:

```rust
            status.pid = None;
        }
        // QEMU가 UEFI 변수(pflash)를 대상 노드의 변수 파일로 옮겼다(설계 §5).
        if vm.spec.firmware == Firmware::Uefi {
            status.firmware_node = Some(migration.node);
        }
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_types::task::Task;
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::Task;
use opl_types::vm::{Firmware, PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
    tokio::fs::copy(from, to).await.map(|_| ())
}
```

을 다음으로 바꾼다:

```rust
    tokio::fs::copy(from, to).await.map(|_| ())
}

/// UEFI VM의 변수 파일을 가진 노드(`firmware_node`)가 이 노드가 아니면 거기서 받아 와 이 노드의
/// 파일을 바꾼다(설계 §5). 받지 못하면 경고만 남기고 이 노드의 파일(없으면 복사하지 않는다)을 쓴다.
/// 이 노드에서 QEMU가 돌고 있으면(그 QEMU가 이 노드의 파일을 쓴다) 받지 않는다.
pub(super) async fn fetch_vars(ctx: &Ctx, vm: &Vm) {
    let (Some(fetcher), Some(node)) = (&ctx.config.firmware, vm.status.firmware_node) else {
        return;
    };
    if vm.spec.firmware != Firmware::Uefi || node == ctx.config.node {
        return;
    }
    if !matches!(ctx.hv.inspect(vm.id).await, Ok(None)) {
        return;
    }
    if let Err(e) = fetcher.fetch(vm.id, node).await {
        tracing::warn!(vm = %vm.id, from = %node, error = %e, "cannot fetch UEFI variables; copying this node's copy");
    }
}

/// UEFI 변수 파일을 이 노드에 썼다: 이 노드를 `firmware_node`로 삼는다. reconciler도 status를
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn set_firmware_node(ctx: &Ctx, vm: Uuid) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        if current.status.firmware_node == Some(ctx.config.node) {
            return Ok(());
        }
        let status = VmStatus {
            firmware_node: Some(ctx.config.node),
            ..current.status.clone()
        };
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        (Ok(()), Some(dir)) => {
            let vars = uefi_vars_path(dir, vm_id);
```

을 다음으로 바꾼다:

```rust
        (Ok(()), Some(dir)) => {
            fetch_vars(ctx, &vm).await;
            let vars = uefi_vars_path(dir, vm_id);
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
    }
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir)
        && tokio::fs::try_exists(&copy).await.unwrap_or(false)
    {
        copy_file(&copy, &uefi_vars_path(dir, vm_id))
            .await
            .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
    }
```

을 다음으로 바꾼다:

```rust
    }
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir) {
        if tokio::fs::try_exists(&copy).await.unwrap_or(false) {
            copy_file(&copy, &uefi_vars_path(dir, vm_id))
                .await
                .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
            // 되돌린 변수는 이 노드에 있다. 다음 시작이 다른 노드에서 받아 덮지 않게 한다.
            set_firmware_node(ctx, vm_id).await?;
        } else if get_vm(ctx, vm_id)?.spec.firmware == Firmware::Uefi {
            // 스냅샷을 다른 노드에서 찍었거나 한 번도 켜지 않은 VM이다. 변수 복사본은 노드 사이에
            // 받아 오지 않으므로 지금 변수를 그대로 둔다.
            tracing::warn!(vm = %vm_id, snapshot = %id, "this node has no UEFI variables copy of the snapshot; keeping the current variables");
        }
    }
```

`crates/opl-api/tests/clones.rs`에서

```rust
    assert_eq!(copy.status.snapshot, None);
    let mut copied = Vec::new();
```

을 다음으로 바꾼다:

```rust
    assert_eq!(copy.status.snapshot, None);
    // 새 VM의 UEFI 변수는 복제한 노드에 있다.
    assert_eq!(copy.status.firmware_node, Some(env.node));
    let mut copied = Vec::new();
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        });
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        let runner = TaskRunner::spawn(
```

을 다음으로 바꾼다:

```rust
        });
        // UEFI 변수 파일을 가진 노드에서 받아 온다(opld처럼 reconciler와 Task 실행기가 함께 쓴다).
        // 시험의 변수 파일은 모두 템플릿과 같은 6바이트다.
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(template(root, self), b"vars 0").unwrap();
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            self.cluster.clone(),
            self.peers.clone(),
            work.join("state"),
            template(root, self),
        ));
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        reconciler_config.firmware = Some(firmware.clone());
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        let runner = TaskRunner::spawn(
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
```

을 다음으로 바꾼다:

```rust
}

/// 노드의 UEFI 변수 템플릿(받은 변수 파일의 크기를 이와 견준다)
fn template(root: &TempDir, node: &Node) -> std::path::PathBuf {
    root.path()
        .join(format!("{}-work", node.id))
        .join("vars-template.fd")
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_the_node_that_has_them() {
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars of b").unwrap();
    let fetcher = UefiVarsFetcher::new(a.cluster.clone(), a.peers.clone(), state_dir(&root, &a));
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");
```

을 다음으로 바꾼다:

```rust
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, b"vars b").unwrap();
    let fetcher = UefiVarsFetcher::new(
        a.cluster.clone(),
        a.peers.clone(),
        state_dir(&root, &a),
        template(&root, &a),
    );
    let dest = uefi_vars_path(&state_dir(&root, &a), vm);

    fetcher.fetch(vm, b.id).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");

    // 크기가 이 노드의 템플릿과 다르면(다른 펌웨어 빌드) 받은 것을 버린다.
    std::fs::write(&src, b"other firmware").unwrap();
    let err = fetcher.fetch(vm, b.id).await.unwrap_err();
    assert!(err.0.contains("14 bytes"), "{err}");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars of b");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(err.0, "node \"node2\" is offline");
    assert_eq!(std::fs::read(&dest).unwrap(), b"vars b");
}

/// 노드의 VM `vm` UEFI 변수 파일에 쓴다.
fn write_vars(root: &TempDir, node: &Node, vm: Uuid, data: &[u8]) {
    let path = uefi_vars_path(&state_dir(root, node), vm);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

fn read_vars(root: &TempDir, node: &Node, vm: Uuid) -> Vec<u8> {
    std::fs::read(uefi_vars_path(&state_dir(root, node), vm)).unwrap()
}

/// 멈춘 UEFI VM을 다른 노드로 옮기면, 그 노드의 스냅샷·복제·시작이 앞 노드의 변수를 받아 쓴다.
/// 되돌린 변수는 그 노드의 것이 된다(다시 받지 않는다).
#[tokio::test]
async fn variables_follow_a_stopped_vm_to_another_node() {
    let (root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    write_vars(&root, &a, vm.id, b"vars 1");

    // 멈춘 채 B로 옮긴다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = serde_json::json!(b.id);
    let (status, body) = admin
        .put("/vms/web", serde_json::json!({ "spec": spec }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 스냅샷은 B에서 찍고 A의 변수를 받아 담는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", serde_json::json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    let s1 = b
        .cluster
        .list::<opl_types::snapshot::SnapshotSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|s| s.spec.vm == vm.id)
        .unwrap();
    let copy = state_dir(&root, &b).join(format!("vm/{}/snapshots/{}.vars", vm.id, s1.id));
    assert_eq!(std::fs::read(copy).unwrap(), b"vars 1");

    // 복제도 B에서 A의 변수를 받아 복사하고, 새 VM의 변수는 B에 있다.
    write_vars(&root, &a, vm.id, b"vars 2");
    let (status, task) = admin
        .post("/vms/web/clone", serde_json::json!({ "name": "web-2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = b.cluster.get_by_name::<VmSpec>("web-2").unwrap().unwrap();
    assert_eq!(read_vars(&root, &b, copy.id), b"vars 2");
    assert_eq!(copy.status.firmware_node, Some(b.id));

    // 켜면 B의 reconciler가 A의 변수를 받아 켜고 B를 변수 노드로 남긴다.
    write_vars(&root, &a, vm.id, b"vars 3");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 3");
    wait_vm(&b, vm.id, "variables on B", |v| {
        v.status.firmware_node == Some(b.id)
    })
    .await;
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    // 되돌리기는 스냅샷 때의 변수를 B에 쓰고 B를 변수 노드로 삼는다. 다음 시작은 A에서 받지 않는다.
    let mut moved = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    moved.status.firmware_node = Some(a.id);
    b.cluster
        .update_status::<VmSpec>(vm.id, &moved.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reverted = b.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(reverted.status.firmware_node, Some(b.id));
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(read_vars(&root, &b, vm.id), b"vars 1");
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
```

을 다음으로 바꾼다:

```rust
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;
    wait_vm(&a, vm.id, "variables on A", |v| {
        v.status.firmware_node == Some(a.id)
    })
    .await;
    // B의 reconciler가 넘겨받으며 `firmware_node`를 쓰기 전에, 확정 뒤 기록이 B를 쓰는지 본다.
    let b_reconciler = b._workers.as_ref().unwrap().0.handle();
    b_reconciler.begin_migration(vm.id).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
    assert_eq!(vm_now.status.migration, None);
    assert_eq!(runtime_status(&a, vm.id).await, None);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
```

을 다음으로 바꾼다:

```rust
    assert_eq!(vm_now.status.migration, None);
    // QEMU가 변수를 B로 옮겼다.
    assert_eq!(vm_now.status.firmware_node, Some(b.id));
    assert_eq!(runtime_status(&a, vm.id).await, None);
    b_reconciler.end_migration(vm.id);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
```

`crates/opl-peer/src/client.rs`에서

```rust
    /// `addr`의 노드에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에 받아
    /// fsync한 뒤 이름을 바꾼다). 실패하면 `dest`는 그대로다. 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(&self, addr: &str, vm: Uuid, dest: &Path) -> Result<u64, UefiVarsError> {
        let mut req = Request::new(UefiVarsRequest { vm });
        req.set_timeout(UEFI_VARS_TIMEOUT);
        let chunks = self.client(addr)?.uefi_vars(req).await?.into_inner();
        crate::firmware::write_file(chunks, dest).await
    }
```

을 다음으로 바꾼다:

```rust
    /// `addr`의 노드에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에 받아
    /// fsync한 뒤 이름을 바꾼다). 받은 크기가 `expect_len`과 다르거나 실패하면 `dest`는 그대로다.
    /// 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(
        &self,
        addr: &str,
        vm: Uuid,
        dest: &Path,
        expect_len: Option<u64>,
    ) -> Result<u64, UefiVarsError> {
        let mut req = Request::new(UefiVarsRequest { vm });
        req.set_timeout(UEFI_VARS_TIMEOUT);
        let chunks = self.client(addr)?.uefi_vars(req).await?.into_inner();
        crate::firmware::write_file(chunks, dest, expect_len).await
    }
```

`crates/opl-peer/src/firmware.rs` 전체:

```rust
//! UEFI 변수 파일 옮기기: VM을 다른 노드에서 켤 때 변수 파일을 가진 노드(`firmware_node`)에서
//! `UefiVars { vm }`로 받아 온다. 파일은 1 MiB 조각으로 보내고, 0만 있는 조각(AAVMF 변수 파일은
//! 64 MiB 대부분이 0이다)은 길이만 보낸다.

use std::io::SeekFrom;
use std::path::Path;

use bytes::{Buf, BufMut, Bytes};
use futures_util::{Stream, StreamExt};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tonic::codec::EncodeBuf;
use tonic::{Code, Status};
use uuid::Uuid;

use crate::codec::Binary;

/// 한 조각의 크기
pub const CHUNK_BYTES: usize = 1 << 20;

/// 받을 변수 파일의 최대 크기. AAVMF 변수 파일(64 MiB)보다 넉넉히 크고, 상대가 잘못된 길이를 보내도
/// 디스크를 채우지 않는다.
pub const MAX_BYTES: u64 = 256 << 20;

/// 변수 파일 요청: 어느 VM의 파일인지
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UefiVarsRequest {
    pub vm: Uuid,
}

impl Binary for UefiVarsRequest {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        dst.put_slice(self.vm.as_bytes());
    }

    fn decode(src: Bytes) -> Result<Self, Status> {
        Uuid::from_slice(&src)
            .map(|vm| Self { vm })
            .map_err(|_| Status::invalid_argument("bad VM id"))
    }
}

/// 변수 파일의 한 조각. 순서대로 이으면 파일이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UefiVarsChunk {
    Data(Bytes),
    /// 이 길이만큼 0이다.
    Zero(u64),
}

const DATA_TAG: u8 = 0;
const ZERO_TAG: u8 = 1;

impl UefiVarsChunk {
    /// 파일에서 읽은 조각. 0만 있으면 길이만 남긴다.
    pub fn of(data: &[u8]) -> Self {
        if data.iter().all(|b| *b == 0) {
            Self::Zero(data.len() as u64)
        } else {
            Self::Data(Bytes::copy_from_slice(data))
        }
    }
}

impl Binary for UefiVarsChunk {
    fn encode(self, dst: &mut EncodeBuf<'_>) {
        match self {
            Self::Data(data) => {
                dst.put_u8(DATA_TAG);
                dst.put(data);
            }
            Self::Zero(len) => {
                dst.put_u8(ZERO_TAG);
                dst.put_u64(len);
            }
        }
    }

    fn decode(mut src: Bytes) -> Result<Self, Status> {
        let bad = || Status::invalid_argument("bad UEFI variables chunk");
        if src.is_empty() {
            return Err(bad());
        }
        match src.get_u8() {
            DATA_TAG => Ok(Self::Data(src)),
            ZERO_TAG if src.len() == 8 => Ok(Self::Zero(src.get_u64())),
            _ => Err(bad()),
        }
    }
}

/// 변수 파일을 받지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UefiVarsError {
    /// 그 노드에 이 VM의 변수 파일이 없다.
    #[error("the node has no UEFI variables for this VM")]
    NotFound,
    /// 그 노드에 닿지 못했거나 보내는 중에 끊겼다.
    #[error("cannot fetch the UEFI variables: {0}")]
    Unavailable(String),
    /// 받은 파일을 이 노드에 쓰지 못했다.
    #[error("cannot write the UEFI variables: {0}")]
    Io(String),
    /// 받은 파일을 쓸 수 없다(너무 크다 등).
    #[error("bad UEFI variables: {0}")]
    Invalid(String),
}

impl From<Status> for UefiVarsError {
    fn from(s: Status) -> Self {
        match s.code() {
            Code::NotFound => Self::NotFound,
            _ => Self::Unavailable(s.message().to_string()),
        }
    }
}

fn io(e: std::io::Error) -> UefiVarsError {
    UefiVarsError::Io(e.to_string())
}

/// 보내는 쪽: `file`을 끝까지 조각으로 보낸다. 받는 쪽이 끊으면 멈춘다.
pub(crate) async fn send_file(mut file: File, tx: mpsc::Sender<Result<UefiVarsChunk, Status>>) {
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = match read_full(&mut file, &mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = tx
                    .send(Err(Status::internal(format!(
                        "cannot read the UEFI variables: {e}"
                    ))))
                    .await;
                break;
            }
        };
        if tx.send(Ok(UefiVarsChunk::of(&buf[..n]))).await.is_err() || n < buf.len() {
            break;
        }
    }
}

/// `buf`가 차거나 파일이 끝날 때까지 읽는다.
async fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read(&mut buf[n..]).await? {
            0 => break,
            m => n += m,
        }
    }
    Ok(n)
}

/// 받는 쪽: 조각을 `dest`와 같은 디렉터리의 임시 파일에 쓰고 fsync한 뒤 `dest`로 이름을 바꾼다.
/// `expect_len`을 주면 받은 파일의 크기가 그와 달라도 실패한다. 받다가 실패하면 `dest`는 그대로다.
/// 받은 파일의 크기를 돌려준다.
pub(crate) async fn write_file<S>(
    mut chunks: S,
    dest: &Path,
    expect_len: Option<u64>,
) -> Result<u64, UefiVarsError>
where
    S: Stream<Item = Result<UefiVarsChunk, Status>> + Unpin,
{
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(UefiVarsError::Io(format!("bad path {}", dest.display())));
    };
    tokio::fs::create_dir_all(dir).await.map_err(io)?;
    // 같은 VM을 동시에 받아도 임시 파일이 겹치지 않게 이름마다 다르게 둔다.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        Uuid::now_v7()
    ));
    let written = async {
        let mut file = File::create_new(&tmp).await.map_err(io)?;
        let mut len = 0u64;
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let n = match &chunk {
                UefiVarsChunk::Data(data) => data.len() as u64,
                UefiVarsChunk::Zero(n) => *n,
            };
            len = len
                .checked_add(n)
                .filter(|len| *len <= MAX_BYTES)
                .ok_or_else(|| UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes")))?;
            match chunk {
                UefiVarsChunk::Data(data) => file.write_all(&data).await.map_err(io)?,
                // 건너뛴 자리는 0으로 읽힌다(끝의 0은 아래 `set_len`이 채운다).
                UefiVarsChunk::Zero(n) => {
                    let n = i64::try_from(n).map_err(|_| {
                        UefiVarsError::Invalid(format!("larger than {MAX_BYTES} bytes"))
                    })?;
                    file.seek(SeekFrom::Current(n)).await.map_err(io)?;
                }
            }
        }
        if let Some(want) = expect_len
            && len != want
        {
            return Err(UefiVarsError::Invalid(format!(
                "{len} bytes, but this node's firmware uses {want} bytes"
            )));
        }
        // tokio `File`은 쓰기를 뒤에서 하므로, 그 오류는 flush가 돌려준다. `set_len`·`sync_all`은
        // 남은 쓰기를 기다리기만 하고 그 오류를 버린다.
        file.flush().await.map_err(io)?;
        file.set_len(len).await.map_err(io)?;
        file.sync_all().await.map_err(io)?;
        drop(file);
        tokio::fs::rename(&tmp, dest).await.map_err(io)?;
        // 이름 바꾸기가 전원이 나가도 남게 디렉터리도 fsync한다.
        File::open(dir)
            .await
            .map_err(io)?
            .sync_all()
            .await
            .map_err(io)?;
        Ok(len)
    }
    .await;
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;

    #[test]
    fn zero_chunks_carry_only_their_length() {
        assert_eq!(UefiVarsChunk::of(&[0u8; 4096]), UefiVarsChunk::Zero(4096));
        let mut data = vec![0u8; 4096];
        data[4095] = 7;
        assert_eq!(
            UefiVarsChunk::of(&data),
            UefiVarsChunk::Data(Bytes::from(data))
        );
    }

    #[test]
    fn chunks_decode_and_reject_garbage() {
        let mut data = BytesMut::new();
        data.put_u8(ZERO_TAG);
        data.put_u64(CHUNK_BYTES as u64);
        assert_eq!(
            UefiVarsChunk::decode(data.freeze()).unwrap(),
            UefiVarsChunk::Zero(CHUNK_BYTES as u64)
        );
        assert_eq!(
            UefiVarsChunk::decode(Bytes::from_static(b"\0abc")).unwrap(),
            UefiVarsChunk::Data(Bytes::from_static(b"abc"))
        );
        for bad in [&b""[..], b"\x01\0\0", b"\x02abc"] {
            assert!(UefiVarsChunk::decode(Bytes::copy_from_slice(bad)).is_err());
        }
        assert!(UefiVarsRequest::decode(Bytes::from_static(b"short")).is_err());
    }

    fn stream(
        items: Vec<Result<UefiVarsChunk, Status>>,
    ) -> impl Stream<Item = Result<UefiVarsChunk, Status>> + Unpin {
        futures_util::stream::iter(items)
    }

    #[tokio::test]
    async fn received_chunks_replace_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("vm").join("uefi-vars.fd");
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"head"))),
            Ok(UefiVarsChunk::Zero(10)),
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"mid"))),
            // 끝의 0도 파일 크기에 든다.
            Ok(UefiVarsChunk::Zero(5)),
        ];
        assert_eq!(write_file(stream(chunks), &dest, None).await.unwrap(), 22);
        let mut want = b"head".to_vec();
        want.extend([0u8; 10]);
        want.extend(b"mid");
        want.extend([0u8; 5]);
        assert_eq!(std::fs::read(&dest).unwrap(), want);

        // 있던 파일을 바꾼다.
        let chunks = vec![Ok(UefiVarsChunk::Data(Bytes::from_static(b"new")))];
        assert_eq!(write_file(stream(chunks), &dest, None).await.unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(dest.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[tokio::test]
    async fn oversized_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        for chunks in [
            // 합이 최대 크기를 넘는다.
            vec![
                Ok(UefiVarsChunk::Data(Bytes::from_static(b"x"))),
                Ok(UefiVarsChunk::Zero(MAX_BYTES)),
            ],
            // i64로 seek할 수 없는 길이
            vec![Ok(UefiVarsChunk::Zero(u64::MAX))],
            // 합이 u64를 넘친다.
            vec![
                Ok(UefiVarsChunk::Zero(1)),
                Ok(UefiVarsChunk::Zero(u64::MAX)),
            ],
        ] {
            let err = write_file(stream(chunks), &dest, None).await.unwrap_err();
            assert!(matches!(err, UefiVarsError::Invalid(_)), "{err:?}");
            assert_eq!(std::fs::read(&dest).unwrap(), b"old");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        // 최대 크기까지는 받는다.
        let chunks = vec![Ok(UefiVarsChunk::Zero(MAX_BYTES))];
        assert_eq!(
            write_file(stream(chunks), &dest, None).await.unwrap(),
            MAX_BYTES
        );
    }

    #[tokio::test]
    async fn transfer_of_another_size_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        let chunks = vec![Ok(UefiVarsChunk::Zero(4))];
        let err = write_file(stream(chunks), &dest, Some(3))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            UefiVarsError::Invalid("4 bytes, but this node's firmware uses 3 bytes".into())
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let chunks = vec![Ok(UefiVarsChunk::Data(Bytes::from_static(b"new")))];
        assert_eq!(write_file(stream(chunks), &dest, Some(3)).await.unwrap(), 3);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }

    #[tokio::test]
    async fn broken_transfer_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("uefi-vars.fd");
        std::fs::write(&dest, b"old").unwrap();
        let chunks = vec![
            Ok(UefiVarsChunk::Data(Bytes::from_static(b"partial"))),
            Err(Status::unavailable("connection reset")),
        ];
        let err = write_file(stream(chunks), &dest, None).await.unwrap_err();
        assert_eq!(err, UefiVarsError::Unavailable("connection reset".into()));
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        // 임시 파일도 남기지 않는다.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    std::fs::write(&dest, b"stale").unwrap();
    let len = to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap();
    assert_eq!(len, vars.len() as u64);
```

을 다음으로 바꾼다:

```rust
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(to.peers.uefi_vars(&from.addr, vm, &dest).await.unwrap(), 0);
    assert!(std::fs::read(&dest).unwrap().is_empty());
```

을 다음으로 바꾼다:

```rust
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(&from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest)
        .await
```

을 다음으로 바꾼다:

```rust
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest, None)
        .await
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"))
        .await
```

을 다음으로 바꾼다:

```rust
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"), None)
        .await
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{Firmware, PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            self.disks_active = true;
        }
```

을 다음으로 바꾼다:

```rust
            self.disks_active = true;
            // 이 노드의 QEMU가 이 노드의 변수 파일을 쓰고 있다(다시 붙음, 넘겨받은 마이그레이션 포함).
            self.own_firmware(vm, &mut status);
        }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
                    self.was_running = true;
                    match self.hv.inspect(self.id).await? {
```

을 다음으로 바꾼다:

```rust
                    self.was_running = true;
                    // 이 노드의 변수 파일(받아 왔거나 로컬, 없으면 템플릿)로 켰다.
                    self.own_firmware(vm, &mut status);
                    match self.hv.inspect(self.id).await? {
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        self.disks_shared = false;
        // 실패해도 다시 쓰지 않는다.
```

을 다음으로 바꾼다:

```rust
        self.disks_shared = false;
        self.fetch_firmware(vm).await;
        // 실패해도 다시 쓰지 않는다.
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
                Ok(false)
            }
        }
    }
```

을 다음으로 바꾼다:

```rust
                Ok(false)
            }
        }
    }

    /// 멈춘 UEFI VM을 새로 켜기 전에, 변수 파일을 가진 노드(`firmware_node`)가 다른 노드면 거기서
    /// 받아 와 이 노드의 파일을 바꾼다(설계 §5). 받지 못하면(그 노드가 오프라인이다, 파일이 없다 등)
    /// 경고만 남기고 이 노드의 파일(없으면 템플릿)로 켠다. 도는 VM에 다시 붙거나 마이그레이션을
    /// 넘겨받을 때는 부르지 않는다(QEMU가 이미 이 노드의 파일을 쓴다).
    async fn fetch_firmware(&self, vm: &Vm) {
        let (Some(fetcher), Some(node)) = (&self.config.firmware, vm.status.firmware_node) else {
            return;
        };
        if vm.spec.firmware != Firmware::Uefi || node == self.config.node {
            return;
        }
        match fetcher.fetch(self.id, node).await {
            Ok(()) => tracing::info!(vm = %self.id, from = %node, "fetched UEFI variables"),
            Err(e) => {
                tracing::warn!(vm = %self.id, from = %node, error = %e, "cannot fetch UEFI variables; starting with this node's copy");
            }
        }
    }

    /// UEFI VM이 이 노드의 변수 파일로 돈다: 이 노드를 `firmware_node`로 남긴다. BIOS VM은 그대로 둔다.
    fn own_firmware(&self, vm: &Vm, status: &mut VmStatus) {
        if vm.spec.firmware == Firmware::Uefi {
            status.firmware_node = Some(self.config.node);
        }
    }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            match self
```

을 다음으로 바꾼다:

```rust
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. `firmware_node`도 이 작업자가 바꾸지 않았으면
    /// 저장소의 것을 지킨다(스냅샷 되돌리기가 쓴다). 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        let keep_firmware = status.firmware_node == vm.status.firmware_node;
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            if keep_firmware {
                status.firmware_node = vm.status.firmware_node;
            }
            match self
```

`crates/opl-reconciler/tests/firmware.rs`를 만든다:

```rust
//! UEFI 변수가 VM을 따라간다(설계 §5): 멈춘 VM을 새로 켤 때만 변수 파일을 가진 노드에서 받아 오고,
//! 켠 노드를 `firmware_node`로 남긴다.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_driver::{Hypervisor, RuntimeStatus};
use opl_reconciler::{FirmwareError, FirmwareFetcher};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{Firmware, MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

/// 받아 오기 호출(VM, 노드)을 적는다. `fail`이면 실패한다.
#[derive(Default)]
struct Fetcher {
    calls: Mutex<Vec<(Uuid, Uuid)>>,
    fail: bool,
}

#[async_trait]
impl FirmwareFetcher for Fetcher {
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        self.calls.lock().unwrap().push((vm, node));
        if self.fail {
            return Err(FirmwareError("node \"node2\" is offline".into()));
        }
        Ok(())
    }
}

impl Fetcher {
    fn calls(&self) -> Vec<(Uuid, Uuid)> {
        self.calls.lock().unwrap().clone()
    }
}

async fn env_with(fetcher: &Arc<Fetcher>) -> Env {
    let f: Arc<dyn FirmwareFetcher> = fetcher.clone();
    Env::start_with(|c| c.firmware = Some(f)).await
}

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

/// `firmware_node`가 `node`인 멈춘 VM을 만들고 켠다. 켜진 VM을 돌려준다.
async fn start_vm(env: &Env, firmware: Firmware, node: Option<Uuid>) -> Vm {
    let mut vm = vm("web", env.node);
    vm.spec.firmware = firmware;
    vm.spec.power = PowerState::Stopped;
    vm.status.firmware_node = node;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await
}

#[tokio::test]
async fn variables_are_fetched_from_the_node_that_has_them() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let other = Uuid::now_v7();
    let got = start_vm(&env, Firmware::Uefi, Some(other)).await;

    assert_eq!(fetcher.calls(), vec![(got.id, other)]);
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert_eq!(env.hv.launches(), vec![(got.id, None)]);
}

#[tokio::test]
async fn failed_fetch_does_not_stop_the_start() {
    let fetcher = Arc::new(Fetcher {
        fail: true,
        ..Default::default()
    });
    let env = env_with(&fetcher).await;
    let other = Uuid::now_v7();
    let got = start_vm(&env, Firmware::Uefi, Some(other)).await;

    assert_eq!(fetcher.calls(), vec![(got.id, other)]);
    // 이 노드의 변수(없으면 템플릿)로 켰으므로 이제 이 노드가 가진다.
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.hv.launches(), vec![(got.id, None)]);
}

#[tokio::test]
async fn variables_on_this_node_are_not_fetched() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let got = start_vm(&env, Firmware::Uefi, Some(env.node)).await;
    assert_eq!(got.status.firmware_node, Some(env.node));

    // 한 번도 켠 적 없는 VM도 받을 곳이 없다.
    let mut fresh = vm("fresh", env.node);
    fresh.spec.firmware = Firmware::Uefi;
    env.create(&fresh).await;
    let got = env.wait(fresh.id, "running", running).await;
    assert_eq!(got.status.firmware_node, Some(env.node));
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
}

#[tokio::test]
async fn bios_vm_is_left_alone() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let got = start_vm(&env, Firmware::Bios, None).await;

    assert_eq!(got.status.firmware_node, None);
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
}

/// opld가 다시 떠서 도는 VM에 다시 붙는다: 그 QEMU는 이 노드의 변수를 쓰므로 받지 않고 이 노드를
/// 남긴다.
#[tokio::test]
async fn running_vm_is_not_fetched_when_reattached() {
    let fetcher = Arc::new(Fetcher::default());
    let mut env = env_with(&fetcher).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    let mut status = before.status.clone();
    status.firmware_node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    env.start_reconciler();

    let got = env
        .wait(vm.id, "firmware node recorded", |v| {
            v.status.firmware_node == Some(env.node)
        })
        .await;
    assert_eq!(got.status.pid, before.status.pid);
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
    assert_eq!(env.hv.launches(), vec![(vm.id, None)]);
}

/// 이 노드로 확정된 라이브 마이그레이션을 넘겨받는다: QEMU가 변수를 옮겨 왔으므로 받지 않고 이 노드를
/// 남긴다.
#[tokio::test]
async fn received_vm_is_not_fetched() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let source = Uuid::now_v7();
    let mut vm = vm("web", source);
    vm.status.firmware_node = Some(source);
    vm.status.phase = VmPhase::Migrating;
    vm.status.node = Some(source);
    vm.status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.create(&vm).await;
    let spec = VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();

    // 원본이 확정한다.
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.firmware_node == Some(env.node)
        })
        .await;
    assert_eq!(got.status.node, Some(env.node));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        env.hv.inspect(vm.id).await.unwrap().map(|s| s.status),
        Some(RuntimeStatus::Running)
    );
    assert!(fetcher.calls().is_empty(), "{:?}", fetcher.calls());
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// 작업자가 바꾸지 않는 `firmware_node`는 그 사이 다른 곳(스냅샷 되돌리기)이 쓴 값을 덮지 않는다.
#[tokio::test]
async fn status_writes_keep_firmware_node_written_meanwhile() {
    let fetcher = Arc::new(Fetcher::default());
    let env = env_with(&fetcher).await;
    let mut vm = vm("web", env.node);
    vm.spec.firmware = Firmware::Bios;
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;

    // 작업자가 VM을 읽고 시작하는 사이에 쓴다.
    env.hv.delay_starts(Duration::from_millis(300));
    env.set_power(vm.id, PowerState::Running).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let node = Uuid::now_v7();
    let mut status = current.status.clone();
    status.firmware_node = Some(node);
    env.cluster
        .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
        .await
        .unwrap();

    env.wait(vm.id, "running", running).await;
    assert_eq!(env.get(vm.id).status.firmware_node, Some(node));
}
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
            migration: None,
            firmware_node: None,
        }
```

을 다음으로 바꾼다:

```rust
            migration: None,
            // UEFI VM은 켠 노드가 변수 파일을 가진다.
            firmware_node: Some(env.node),
        }
```

`crates/opld/src/member.rs`에서

```rust
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
```

을 다음으로 바꾼다:

```rust
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirmwareFetcher, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
```

`crates/opld/src/member.rs`에서

```rust
        let qemu_binary = host.profile.qemu_binary.clone();
        let register = {
```

을 다음으로 바꾼다:

```rust
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        let register = {
```

`crates/opld/src/member.rs`에서

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다.
        reconciler_config.firmware = Some(Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
        )));
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

을 다음으로 바꾼다:

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.peers = Some(peers.clone());
        runner_config.migration_bandwidth =
```

을 다음으로 바꾼다:

```rust
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 744개, 실패 0

Run: `cargo test -p opl-reconciler --test firmware`
Expected: PASS

Run: `cargo test -p opl-api --test two_nodes --test clones`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(reconciler): UEFI 변수가 시작·마이그레이션·스냅샷·복제를 따라간다"
```

---

### Task 3: PATCH /storage-pools/{key}로 디렉터리 풀의 shared 바꾸기

`PATCH /api/v1/storage-pools/{key}` 본문 `PoolPatch { shared }`(`deny_unknown_fields`), `Write` 권한, `If-Match`(선택, 412), 200과 풀. 디렉터리 풀만(다른 종류는 같은 값이어도 400 `invalid_request`), 같은 값은 쓰지 않고 200, 로컬→공유는 언제나, 공유→로컬은 그 풀 볼륨을 쓰는 VM이 볼륨을 만든 노드와 다른 노드에 있거나 끝나지 않은 `VmMigrate` Task가 있으면 409 `pool_in_use`(VM 이름). `vm_lock` 안에서 검사·쓰기, 쓴 뒤 다시 검사해 실패하면 공유로 되돌리고 409(다른 API 노드의 VM 옮기기와의 경쟁).

**Files:**
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/pools.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_networks, r::create_network))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::patch_pool, r::delete_pool))
        .routes(routes!(r::list_networks, r::create_network))
```

`crates/opl-api/src/resources.rs`에서

```rust
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::{Cluster, Typed};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
```

을 다음으로 바꾼다:

```rust
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError, Typed};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
```

`crates/opl-api/src/resources.rs`에서

```rust
};
use opl_types::task::{TaskSpec, TaskStatus};
use opl_types::vm::{VmSpec, VmStatus};
```

을 다음으로 바꾼다:

```rust
};
use opl_types::task::{TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{VmSpec, VmStatus};
```

`crates/opl-api/src/resources.rs`에서

```rust

/// 노드에 미리 만든 Linux 브리지를 가리키는 네트워크. 이름 `user`는 예약되어 있다.
```

을 다음으로 바꾼다:

```rust

/// 풀에서 바꿀 수 있는 값. 지금은 디렉터리 풀의 `shared`만이다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolPatch {
    pub shared: bool,
}

/// 디렉터리 풀의 `shared`를 바꾼다(다른 백엔드는 400). 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의
/// 볼륨을 쓰는 VM이 그 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면
/// 409 `pool_in_use`다. 같은 값이면 쓰지 않고 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = PoolPatch,
    responses((status = 200, body = Resource<StoragePoolSpec, StoragePoolStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PoolPatch>,
) -> Result<Json<StoragePool>, ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    let expected = if_match(&headers)?;
    // VM 옮기기·마이그레이션 요청의 검사와 엇갈리지 않게 한다.
    let _lock = state.vm_lock.lock().await;
    for _ in 0..10 {
        let pool = lookup::<StoragePoolSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != pool.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: pool.revision,
            })
            .into());
        }
        let fixed = match pool.spec.backend {
            PoolBackend::Dir { .. } => None,
            PoolBackend::Nfs { .. } => Some("nfs pools are always shared"),
            PoolBackend::Rbd { .. } => Some("rbd pools are always shared"),
            PoolBackend::LvmShared { .. } => Some("lvm-shared pools are always shared"),
            PoolBackend::LvmThin { .. } => Some("lvm-thin pools are always node-local"),
            PoolBackend::Zfs { .. } => Some("zfs pools are always node-local"),
        };
        if let Some(why) = fixed {
            return Err(ApiError::bad_request(format!(
                "only dir pools can change shared; {why}"
            )));
        }
        if pool.spec.shared == req.shared {
            return Ok(Json(pool));
        }
        if !req.shared {
            check_pool_local(&state, &pool)?;
        }
        let mut spec = pool.spec.clone();
        spec.shared = req.shared;
        match state
            .cluster
            .update_spec(pool.id, Some(pool.revision), pool.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        let written = lookup::<StoragePoolSpec>(&state.cluster, &pool.id.to_string())?;
        // `vm_lock`은 이 노드의 요청만 막는다. 검사와 쓰기 사이에 다른 노드가 VM을 옮기거나
        // 마이그레이션을 시작했을 수 있으니 다시 보고, 이제 안 되면 공유로 되돌린다(그 사이 누가 풀을
        // 바꿨으면 그 값을 둔다).
        if !req.shared
            && let Err(e) = check_pool_local(&state, &written)
        {
            let mut spec = written.spec.clone();
            spec.shared = true;
            if let Err(revert) = state
                .cluster
                .update_spec(
                    written.id,
                    Some(written.revision),
                    written.labels.clone(),
                    &spec,
                )
                .await
            {
                tracing::warn!(pool = %written.name, error = %revert, "cannot make the pool shared again");
            }
            return Err(e);
        }
        return Ok(Json(written));
    }
    Err(ApiError::conflict(
        "busy",
        "the pool kept changing while being updated; try again",
    ))
}

/// 공유 풀을 로컬로 바꿔도 되는지. 볼륨은 만든 노드에 있는 것으로 돌아가므로, 그 볼륨을 쓰는 VM이
/// 다른 노드에 있거나 옮겨 가는 중이면 409 `pool_in_use`다.
fn check_pool_local(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    let volumes: Vec<_> = volumes
        .iter()
        .filter(|v| v.spec.pool == pool.name)
        .collect();
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    for vm in &vms {
        for disk in &vm.spec.disks {
            let Some(volume) = volumes.iter().find(|v| v.name == disk.volume) else {
                continue;
            };
            if let (Some(node), Some(home)) = (vm.spec.node, volume.status.node)
                && node != home
            {
                return Err(ApiError::conflict(
                    "pool_in_use",
                    format!(
                        "VM {:?} is on a different node from its volume {:?}; move it back before making pool {:?} local",
                        vm.name, volume.name, pool.name
                    ),
                ));
            }
            let migrating = tasks.iter().any(|t| {
                t.spec.target == vm.id
                    && !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::VmMigrate { .. })
            });
            if migrating {
                return Err(ApiError::conflict(
                    "pool_in_use",
                    format!(
                        "VM {:?} with a disk on pool {:?} is migrating; try again when the migration has finished",
                        vm.name, pool.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 노드에 미리 만든 Linux 브리지를 가리키는 네트워크. 이름 `user`는 예약되어 있다.
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let mut r = self.req(reqwest::Method::PUT, path).json(&body);
        if let Some(rev) = if_match {
```

을 다음으로 바꾼다:

```rust
        let mut r = self.req(reqwest::Method::PUT, path).json(&body);
        if let Some(rev) = if_match {
            r = r.header("if-match", rev.to_string());
        }
        self.send(r).await
    }

    pub async fn patch(
        &self,
        path: &str,
        body: Value,
        if_match: Option<u64>,
    ) -> (StatusCode, Value) {
        let mut r = self.req(reqwest::Method::PATCH, path).json(&body);
        if let Some(rev) = if_match {
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    }
    // 리소스 종류마다 따로 스키마가 생긴다.
```

을 다음으로 바꾼다:

```rust
    }
    // 풀은 `shared`만 바꾼다.
    assert_eq!(
        doc["paths"]["/storage-pools/{key}"]["patch"]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/PoolPatch"
    );
    // 리소스 종류마다 따로 스키마가 생긴다.
```

`crates/opl-api/tests/pools.rs`에서

```rust

use common::Env;
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust

use common::{Env, vm_request};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
```

`crates/opl-api/tests/pools.rs`에서

```rust
    assert_eq!(task_node(&task), env.node.to_string());
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(task_node(&task), env.node.to_string());
}

/// `PATCH /storage-pools/{key}`는 디렉터리 풀의 `shared`만 바꾼다. 관리자만, 같은 값이면 그대로 200.
#[tokio::test]
async fn patch_changes_shared_of_dir_pools() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("local", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let operator = env.user("operator", "operator").await;
    let (status, _) = operator
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 로컬 → 공유는 언제나 된다. 라벨과 백엔드는 그대로다.
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "dir", "{body}");
    let (_, got) = admin.get("/storage-pools/local").await;
    assert_eq!(got["spec"]["shared"], true, "{got}");
    assert_eq!(got["revision"], body["revision"], "{got}");
    // 같은 값이면 쓰지 않고 그대로 200이다.
    let revision = body["revision"].as_u64().unwrap();
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], revision, "{body}");
    // revision이 다르면 412, 맞으면 바꾼다. id로도 찾는다.
    let (status, body) = admin
        .patch(
            "/storage-pools/local",
            json!({ "shared": false }),
            Some(revision + 1),
        )
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    let id = got["id"].as_str().unwrap();
    let (status, body) = admin
        .patch(
            &format!("/storage-pools/{id}"),
            json!({ "shared": false }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");

    // 모르는 필드, 빠진 필드, 없는 풀
    for bad in [json!({ "shared": true, "nodes": [] }), json!({})] {
        let (status, body) = admin.patch("/storage-pools/local", bad, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, _) = admin
        .patch("/storage-pools/nope", json!({ "shared": true }), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 다른 백엔드는 바꿀 수 없다(같은 값이어도).
    for (name, backend, shared) in [
        (
            "nas",
            json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
            false,
        ),
        ("r", json!({ "kind": "rbd", "pool": "rbd" }), true),
        (
            "vg",
            json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }),
            true,
        ),
        ("z", json!({ "kind": "zfs", "dataset": "tank/opl" }), false),
    ] {
        let (status, body) = admin.post("/storage-pools", pool(name, backend)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, body) = admin
            .patch(
                &format!("/storage-pools/{name}"),
                json!({ "shared": shared }),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert_eq!(body["code"], "invalid_request", "{name}");
        assert!(
            body["message"].as_str().unwrap().contains("dir pools"),
            "{body}"
        );
    }
}

/// 공유 → 로컬은 그 풀의 볼륨을 쓰는 VM이 볼륨을 만든 노드가 아닌 노드에 있거나 마이그레이션
/// Task가 끝나지 않았으면 409 `pool_in_use`다.
#[tokio::test]
async fn patch_to_local_needs_vms_on_their_volume_nodes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    for name in ["nas", "other"] {
        let backend = json!({ "kind": "dir", "path": env.dir.path().join(name) });
        let (status, body) = admin
            .post(
                "/storage-pools",
                json!({ "name": name, "spec": { "backend": backend, "shared": true } }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        env.pool_ready(name).await;
    }
    let node2 = other_node(&env, "node2", true, &["nas", "other"]).await;
    // 이 노드가 만든 볼륨과 그 볼륨을 디스크로 쓰는 꺼진 VM
    for (volume, pool, vm) in [("root", "nas", "web"), ("data", "other", "db")] {
        let mut v: Volume = Resource::new(
            volume,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Qcow2,
            },
        );
        v.status.node = Some(env.node);
        env.cluster.create(&v).await.unwrap();
        v.status.phase = VolumePhase::Ready;
        env.cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
        let mut req = vm_request(vm);
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        req["spec"]["node"] = json!(env.node);
        let (status, body) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let set_node = |vm: &'static str, node: Uuid| {
        let cluster = env.cluster.clone();
        async move {
            let v = cluster.get_by_name::<VmSpec>(vm).unwrap().unwrap();
            let mut spec = v.spec.clone();
            spec.node = Some(node);
            cluster
                .update_spec(v.id, None, v.labels.clone(), &spec)
                .await
                .unwrap();
        }
    };
    // 다른 풀의 VM이 다른 노드로 옮겨 갔어도 상관없다.
    set_node("db", node2).await;
    let to_local = || admin.patch("/storage-pools/nas", json!({ "shared": false }), None);
    let to_shared = || admin.patch("/storage-pools/nas", json!({ "shared": true }), None);
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    let (status, body) = to_shared().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 공유 풀이라 옮겨 간 VM이 있으면 409(VM 이름)
    set_node("web", node2).await;
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"web\""),
        "{body}"
    );
    let (_, got) = admin.get("/storage-pools/nas").await;
    assert_eq!(got["spec"]["shared"], true, "{got}");

    // 볼륨을 만든 노드로 돌아오면 된다. 다시 공유로.
    set_node("web", env.node).await;
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = to_shared().await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 끝나지 않은 마이그레이션 Task가 있어도 409
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    let mut task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VmMigrate { node: node2 },
            target: web.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.revision = env.cluster.create(&task).await.unwrap();
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("\"web\""),
        "{body}"
    );
    // 끝난 Task는 상관없다.
    task.status.state = TaskState::Failed;
    env.cluster
        .update_status::<TaskSpec>(task.id, &task.status)
        .await
        .unwrap();
    let (status, body) = to_local().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 746개, 실패 0

Run: `cargo test -p opl-api --test pools --test openapi`
Expected: PASS

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): PATCH /storage-pools/{key}로 디렉터리 풀의 shared 바꾸기"
```

---

### Task 4: oplctl pool set과 웹 풀 목록의 공유 스위치

`oplctl pool set POOL --shared | --no-shared`(정확히 하나, `PATCH`, 출력 `storage pool X: shared|node-local`, `Client::patch`). 웹: `components/ui/switch.tsx`, 풀 목록의 "공유" 칸(관리자와 디렉터리 풀은 스위치, 그 밖은 글자), 확인 대화상자(닫히는 동안 문구가 바뀌지 않게 열림 상태를 따로 둔다), 409는 `pools.sharingErrors.<code>`와 서버 문구. en·ko.

**Files:**
- Create: `web/src/components/ui/switch.tsx`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/storage/PoolsPage.test.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
    Create(Box<PoolCreate>),
    Delete {
        pool: String,
    },
}
```

을 다음으로 바꾼다:

```rust
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
struct PoolSet {
    pool: String,
    #[command(flatten)]
    sharing: Sharing,
}

/// `pool set`의 공유 여부. 둘 중 하나를 꼭 준다.
#[derive(Args)]
#[group(required = true, multiple = false)]
struct Sharing {
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long)]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`)
fn pool_set_body(sharing: &Sharing) -> Value {
    json!({ "shared": sharing.shared })
}
```

`crates/oplctl/src/cli.rs`에서

```rust
                writeln!(out, "storage pool {name} created")?;
            }
```

을 다음으로 바꾼다:

```rust
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(PoolSet { pool, sharing }) => {
                client
                    .patch(&format!("/storage-pools/{pool}"), &pool_set_body(&sharing))
                    .await?;
                let state = if sharing.shared {
                    "shared"
                } else {
                    "node-local"
                };
                writeln!(out, "storage pool {pool}: {state}")?;
            }
```

`crates/oplctl/src/cli.rs`에서

```rust
    #[test]
    fn parses_pool_create_commands() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool, pool_set_body(&s.sharing)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }))
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            ("nas".into(), json!({ "shared": false }))
        );
        // 둘 중 하나만, 꼭 하나
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
    }

    #[test]
    fn parses_pool_create_commands() {
```

`crates/oplctl/src/client.rs`에서

```rust
        self.send(self.request(Method::PUT, path).json(body)).await
    }
```

을 다음으로 바꾼다:

```rust
        self.send(self.request(Method::PUT, path).json(body)).await
    }

    pub async fn patch(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::PATCH, path).json(body))
            .await
    }
```

`web/src/components/ui/switch.tsx`를 만든다:

```tsx
"use client"

import * as React from "react"
import { cn } from "cn"
import { Switch as SwitchPrimitive } from "radix-ui"

function Switch({
  className,
  ...props
}: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      className={cn(
        "peer inline-flex h-[1.15rem] w-8 shrink-0 items-center rounded-full border border-transparent shadow-xs transition-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 data-[state=checked]:bg-primary data-[state=unchecked]:bg-input dark:data-[state=unchecked]:bg-input/80",
        className
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        data-slot="switch-thumb"
        className="pointer-events-none block size-4 rounded-full bg-background ring-0 transition-transform data-[state=checked]:translate-x-[calc(100%-2px)] data-[state=unchecked]:translate-x-0 dark:data-[state=checked]:bg-primary-foreground dark:data-[state=unchecked]:bg-foreground"
      />
    </SwitchPrimitive.Root>
  )
}

export { Switch }
```

`web/src/i18n/en.json`에서

```json
    "sharedHint": "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
    "nodesHint": "Leave all unchecked to use it on every node.",
```

을 다음으로 바꾼다:

```json
    "sharedHint": "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
    "sharing": "Sharing",
    "sharedState": "Shared",
    "localState": "Node-local",
    "sharedNamed": "Shared storage: {{name}}",
    "makeSharedTitle": "Make {{name}} shared storage?",
    "makeSharedDescription": "Only do this when every node of the pool sees the same files at {{path}}, such as an NFS share mounted outside opLite. VMs with disks in the pool can then start on and migrate to any of its nodes.",
    "makeLocalTitle": "Make {{name}} node-local?",
    "makeLocalDescription": "Each volume goes back to the node that created it. VMs using the pool's volumes must be on that node and not migrating.",
    "makeShared": "Make shared",
    "makeLocal": "Make node-local",
    "madeShared": "{{name}} is now shared storage.",
    "madeLocal": "{{name}} is now node-local.",
    "sharingErrors": {
      "pool_in_use": "A VM uses this pool's volumes away from the node that created them, or is migrating: {{message}}"
    },
    "nodesHint": "Leave all unchecked to use it on every node.",
```

`web/src/i18n/ko.json`에서

```json
    "sharedHint": "모든 노드가 이 경로에서 같은 파일을 보면 고르세요(예: opLite 밖에서 마운트한 NFS 공유).",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
```

을 다음으로 바꾼다:

```json
    "sharedHint": "모든 노드가 이 경로에서 같은 파일을 보면 고르세요(예: opLite 밖에서 마운트한 NFS 공유).",
    "sharing": "공유",
    "sharedState": "공유",
    "localState": "노드 로컬",
    "sharedNamed": "공유 스토리지: {{name}}",
    "makeSharedTitle": "{{name}}을(를) 공유 스토리지로 바꿀까요?",
    "makeSharedDescription": "풀의 모든 노드가 {{path}}에서 같은 파일을 볼 때만 바꾸세요(예: opLite 밖에서 마운트한 NFS 공유). 그러면 이 풀에 디스크가 있는 VM을 풀의 어느 노드에서나 켜고 그 노드로 마이그레이션할 수 있습니다.",
    "makeLocalTitle": "{{name}}을(를) 노드 로컬로 바꿀까요?",
    "makeLocalDescription": "각 볼륨은 만든 노드에 있는 것으로 돌아갑니다. 이 풀의 볼륨을 쓰는 VM은 그 노드에 있어야 하고 마이그레이션 중이 아니어야 합니다.",
    "makeShared": "공유로 바꾸기",
    "makeLocal": "노드 로컬로 바꾸기",
    "madeShared": "{{name}}이(가) 공유 스토리지가 되었습니다.",
    "madeLocal": "{{name}}이(가) 노드 로컬이 되었습니다.",
    "sharingErrors": {
      "pool_in_use": "이 풀의 볼륨을 만든 노드가 아닌 곳에서 쓰거나 마이그레이션 중인 VM이 있습니다: {{message}}"
    },
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[4];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[4]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
```

을 다음으로 바꾼다:

```tsx
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[5];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[5]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("button", { name: "Delete local" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("button", { name: "Delete local" })).not.toBeInTheDocument();
  expect(screen.queryByRole("switch")).not.toBeInTheDocument();
  expect(screen.getByText("local").closest("tr")).toHaveTextContent("Node-local");
});

it("makes a directory pool shared after confirming", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/storage-pools/${local.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("local", { shared: true }));
    }),
  );
  renderPage(<PoolsPage />);
  const toggle = await screen.findByRole("switch", { name: "Shared storage: local" });
  expect(toggle).not.toBeChecked();
  await userEvent.click(toggle);
  const dialog = await screen.findByRole("alertdialog", {
    name: "Make local shared storage?",
  });
  expect(dialog).toHaveTextContent("/srv/local");
  await userEvent.click(within(dialog).getByRole("button", { name: "Make shared" }));
  expect(await screen.findByText("local is now shared storage.")).toBeInTheDocument();
  expect(body).toEqual({ shared: true });
});

it("shows why a shared pool cannot become node-local", async () => {
  const local = pool("local", { shared: true });
  mockLists({ "storage-pool": [local] });
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/storage-pools/${local.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        { code: "pool_in_use", message: 'VM "web" is on a different node' },
        { status: 409 },
      );
    }),
  );
  renderPage(<PoolsPage />);
  const toggle = await screen.findByRole("switch", { name: "Shared storage: local" });
  expect(toggle).toBeChecked();
  await userEvent.click(toggle);
  const dialog = await screen.findByRole("alertdialog", { name: "Make local node-local?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Make node-local" }));
  // 지우기의 pool_in_use 문구(볼륨이 남음)가 아니라 이 동작의 문구와 서버가 준 VM 이름
  const alert = await within(dialog).findByRole("alert");
  expect(alert).toHaveTextContent(
    'A VM uses this pool\'s volumes away from the node that created them, or is migrating: VM "web" is on a different node',
  );
  expect(dialog).toBeVisible();
  expect(body).toEqual({ shared: false });
});

// 디렉터리 풀만 공유 여부를 바꿀 수 있다. 다른 풀과 바꿀 수 없는 사용자에게는 글로만 보인다.
it("shows the sharing of other pools without a switch", async () => {
  mockLists({
    "storage-pool": [
      pool("local"),
      pool("nas", { backend: { kind: "nfs", server: "nas", export: "/srv/vm" } }),
      pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
    ],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const nas = (await within(table).findByText("nas")).closest("tr")!;
  expect(nas).toHaveTextContent("Shared");
  expect(within(nas).queryByRole("switch")).not.toBeInTheDocument();
  const fast = within(table).getByText("fast").closest("tr")!;
  expect(fast).toHaveTextContent("Node-local");
  expect(within(fast).queryByRole("switch")).not.toBeInTheDocument();
  expect(within(table).getAllByRole("switch")).toHaveLength(1);
});
```

`web/src/storage/PoolsPage.tsx` 전체:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { StoragePool } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import {
  cephNameError,
  cephUserError,
  iscsiPortalError,
  iscsiTargetError,
  isShared,
  lvmNameError,
  nfsExportError,
  nfsOptionsError,
  nfsServerError,
  poolCapacity,
  poolLocation,
  poolNodes,
  rbdConfError,
  zfsDatasetError,
} from "@/storage/pools";
import { PoolStatus } from "@/storage/PoolStatus";
import { validName } from "@/vms/form";

export function PoolsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<StoragePool | null>(null);
  // 공유 여부를 바꿀 디렉터리 풀과 바꿀 값. 닫히는 동안에도 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [sharing, setSharing] = useState<{ pool: StoragePool; shared: boolean } | null>(null);
  const [sharingOpen, setSharingOpen] = useState(false);
  const write = can(me.role, "write", "storage-pool");

  const nodeList = useMemo(() => nodes.data ?? [], [nodes.data]);
  // 볼륨 목록을 다 읽기 전에는 모른다(undefined).
  const volumeCount = (pool: StoragePool) =>
    volumes.isSuccess ? volumes.data.filter((v) => v.spec.pool === pool.name).length : undefined;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="pools-title" className="text-2xl font-semibold">
          {t("nav.pools")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <Table aria-labelledby="pools-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("pools.kind")}</TableHead>
            <TableHead>{t("pools.location")}</TableHead>
            <TableHead>{t("pools.sharing")}</TableHead>
            <TableHead>{t("pools.nodes")}</TableHead>
            <TableHead>{t("pools.free")}</TableHead>
            <TableHead>{t("pools.volumes")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(pools.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 8 : 7} query={pools} empty={t("pools.empty")} />
          )}
          {(pools.data ?? []).map((pool) => {
            const capacity = poolCapacity(pool, nodeList);
            const used = volumeCount(pool);
            return (
              <TableRow key={pool.id}>
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell>{t(`pools.kinds.${pool.spec.backend.kind}`)}</TableCell>
                <TableCell className="font-mono text-xs">{poolLocation(pool)}</TableCell>
                <TableCell>
                  {/* 디렉터리 풀만 바꿀 수 있다. 다른 풀은 종류가 정한다(NFS·RBD·공유 LVM은 늘 공유). */}
                  {write && pool.spec.backend.kind === "dir" ? (
                    <Switch
                      aria-label={t("pools.sharedNamed", { name: pool.name })}
                      checked={isShared(pool)}
                      onCheckedChange={(shared) => {
                        setSharing({ pool, shared });
                        setSharingOpen(true);
                      }}
                    />
                  ) : isShared(pool) ? (
                    t("pools.sharedState")
                  ) : (
                    t("pools.localState")
                  )}
                </TableCell>
                <TableCell>
                  {/* 노드를 정하지 않은 풀은 모든 노드에서 쓴다. */}
                  {(pool.spec.nodes ?? []).length === 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.allNodes")}</div>
                  )}
                  {poolNodes(pool, nodeList).map((n) => (
                    <PoolStatus key={n.id} name={n.name} state={n.state} offline={n.offline} />
                  ))}
                </TableCell>
                <TableCell>
                  {/* 공유 풀은 값 하나, 로컬 풀은 준비된 노드마다(노드가 둘 이상이면 이름을 붙인다). */}
                  {capacity.length === 0
                    ? "—"
                    : capacity.map((c) => (
                        <div key={c.node ?? ""}>
                          {capacity.length > 1 && `${c.node}: `}
                          {`${formatBytes(c.free)} / ${formatBytes(c.total)}`}
                        </div>
                      ))}
                </TableCell>
                <TableCell>
                  {used ?? "—"}
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(pool)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreatePoolDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={sharingOpen}
        onOpenChange={setSharingOpen}
        title={t(sharing?.shared ? "pools.makeSharedTitle" : "pools.makeLocalTitle", {
          name: sharing?.pool.name,
        })}
        description={
          sharing?.shared
            ? t("pools.makeSharedDescription", { path: sharing ? poolLocation(sharing.pool) : "" })
            : t("pools.makeLocalDescription")
        }
        confirmLabel={t(sharing?.shared ? "pools.makeShared" : "pools.makeLocal")}
        onConfirm={async () => {
          const { pool, shared } = sharing!;
          try {
            await call(
              api.PATCH("/storage-pools/{key}", {
                params: { path: { key: pool.id } },
                body: { shared },
              }),
            );
          } catch (err) {
            // pool_in_use는 지우기와 뜻이 달라 이 동작의 문구로 보인다.
            throw err instanceof ApiError
              ? new Error(codeMessage(t, err.code, err.message, "pools.sharingErrors"))
              : err;
          }
          toast.success(t(shared ? "pools.madeShared" : "pools.madeLocal", { name: pool.name }));
        }}
      />
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("pools.deleteTitle", { name: deleting?.name })}
        description={t("pools.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const pool = deleting!;
          await call(api.DELETE("/storage-pools/{key}", { params: { path: { key: pool.id } } }));
          toast.success(t("pools.deleted", { name: pool.name }));
        }}
      />
    </div>
  );
}

type Kind = StoragePool["spec"]["backend"]["kind"];

function CreatePoolDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<Kind>("dir");
  const [path, setPath] = useState("");
  const [shared, setShared] = useState(false);
  const [nfsServer, setNfsServer] = useState("");
  const [nfsExport, setNfsExport] = useState("");
  const [nfsOptions, setNfsOptions] = useState("");
  const [vg, setVg] = useState("");
  const [thinpool, setThinpool] = useState("");
  const [dataset, setDataset] = useState("");
  const [cephPool, setCephPool] = useState("");
  const [namespace, setNamespace] = useState("");
  const [cephUser, setCephUser] = useState("");
  const [cephConf, setCephConf] = useState("");
  const [iscsi, setIscsi] = useState(false);
  const [portal, setPortal] = useState("");
  const [target, setTarget] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (kind === "dir" && !path.startsWith("/")) {
      found.path = "pools.errors.path";
    }
    if (kind === "nfs") {
      const server = nfsServerError(nfsServer);
      if (server) {
        found.server = server;
      }
      const exportError = nfsExportError(nfsExport);
      if (exportError) {
        found.export = exportError;
      }
      const optionsError = nfsOptions === "" ? undefined : nfsOptionsError(nfsOptions);
      if (optionsError) {
        found.options = optionsError;
      }
    }
    if (kind === "lvm-thin") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      const thinpoolError = lvmNameError(thinpool);
      if (thinpoolError) {
        found.thinpool = thinpoolError;
      }
    }
    if (kind === "lvm-shared") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      // iSCSI를 고르지 않았으면 포털·타깃은 보지 않고 보내지도 않는다.
      const portalError = iscsi ? iscsiPortalError(portal) : undefined;
      if (portalError) {
        found.portal = portalError;
      }
      const targetError = iscsi ? iscsiTargetError(target) : undefined;
      if (targetError) {
        found.target = targetError;
      }
    }
    if (kind === "zfs") {
      const datasetError = zfsDatasetError(dataset);
      if (datasetError) {
        found.dataset = datasetError;
      }
    }
    if (kind === "rbd") {
      const poolError = cephNameError(cephPool);
      if (poolError) {
        found.cephPool = poolError;
      }
      // 네임스페이스·사용자·conf는 비우면 보내지 않는다(Ceph 기본값).
      const namespaceError = namespace === "" ? undefined : cephNameError(namespace);
      if (namespaceError) {
        found.namespace = namespaceError;
      }
      const userError = cephUser === "" ? undefined : cephUserError(cephUser);
      if (userError) {
        found.cephUser = userError;
      }
      const confError = cephConf === "" ? undefined : rbdConfError(cephConf);
      if (confError) {
        found.cephConf = confError;
      }
    }
    // 고른 뒤에 목록에서 빠진 노드는 보내지 않는다.
    const known = chosen.filter((id) => nodes.data?.some((n) => n.id === id));
    // 고른 노드가 모두 빠졌으면 빈 목록(모든 노드)으로 보내지 않는다.
    // 빠진 노드는 고른 목록에서도 지워, 다시 보내면 모든 노드로 만들 수 있게 한다.
    if (chosen.length > 0 && known.length === 0) {
      found.nodes = "pools.errors.nodesGone";
      setChosen(known);
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // NFS·RBD·공유 LVM 풀의 `shared`는 보내지 않는다(서버가 늘 공유로 둔다). 빈 옵션은 커널 기본값이다.
    // LVM-thin·ZFS 풀은 노드 로컬이라 `shared`를 보내지 않는다.
    const sharedDir = kind === "dir" && shared;
    const backend =
      kind === "dir"
        ? { kind, path }
        : kind === "lvm-thin"
          ? { kind, vg, thinpool }
          : kind === "zfs"
            ? { kind, dataset }
            : kind === "lvm-shared"
              ? { kind, vg, ...(iscsi && { iscsi: { portal, target } }) }
              : kind === "rbd"
                ? {
                    kind,
                    pool: cephPool,
                    ...(namespace !== "" && { namespace }),
                    ...(cephUser !== "" && { user: cephUser }),
                    ...(cephConf !== "" && { conf: cephConf }),
                  }
                : {
                    kind,
                    server: nfsServer,
                    export: nfsExport,
                    ...(nfsOptions !== "" && { options: nfsOptions }),
                  };
    const spec = { backend, nodes: known, ...(sharedDir && { shared: true }) };
    await call(api.POST("/storage-pools", { body: { name, spec } }));
    toast.success(t("pools.created", { name }));
  }

  return (
    <FormDialog
      title={t("pools.createTitle")}
      description={t("pools.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="pool-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field id="pool-kind" label={t("pools.kind")} hint={t(`pools.kindHint.${kind}`)}>
        {(a11y) => (
          <NativeSelect
            id="pool-kind"
            className="w-full"
            value={kind}
            onChange={(e) => setKind(e.target.value as Kind)}
            {...a11y}
          >
            <NativeSelectOption value="dir">{t("pools.kinds.dir")}</NativeSelectOption>
            <NativeSelectOption value="nfs">{t("pools.kinds.nfs")}</NativeSelectOption>
            <NativeSelectOption value="lvm-thin">{t("pools.kinds.lvm-thin")}</NativeSelectOption>
            <NativeSelectOption value="zfs">{t("pools.kinds.zfs")}</NativeSelectOption>
            <NativeSelectOption value="rbd">{t("pools.kinds.rbd")}</NativeSelectOption>
            <NativeSelectOption value="lvm-shared">
              {t("pools.kinds.lvm-shared")}
            </NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      {kind === "lvm-thin" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="vg0"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-thinpool" label={t("pools.thinpool")} error={errors.thinpool}>
            {(a11y) => (
              <Input
                id="pool-thinpool"
                placeholder="thinpool"
                value={thinpool}
                onChange={(e) => setThinpool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "lvm-shared" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="san"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* FC·멀티패스처럼 노드가 이미 LUN을 보면 로그인하지 않는다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-iscsi"
                checked={iscsi}
                onCheckedChange={(v) => {
                  setIscsi(v === true);
                  // 바꾸면 포털·타깃의 앞 오류는 맞지 않으므로 지운다.
                  setErrors((prev) => {
                    const next = { ...prev };
                    delete next.portal;
                    delete next.target;
                    return next;
                  });
                }}
                aria-describedby="pool-iscsi-hint"
              />
              <Label htmlFor="pool-iscsi">{t("pools.iscsi")}</Label>
            </div>
            <p id="pool-iscsi-hint" className="text-xs text-muted-foreground">
              {t("pools.iscsiHint")}
            </p>
          </div>
          {iscsi && (
            <>
              <Field
                id="pool-portal"
                label={t("pools.portal")}
                error={errors.portal}
                hint={t("pools.portalHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-portal"
                    placeholder="san.lan:3260"
                    value={portal}
                    onChange={(e) => setPortal(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <Field
                id="pool-target"
                label={t("pools.target")}
                error={errors.target}
                hint={t("pools.targetHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-target"
                    placeholder="iqn.2003-01.org.example:storage"
                    value={target}
                    onChange={(e) => setTarget(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
            </>
          )}
        </>
      )}
      {kind === "zfs" && (
        <Field id="pool-dataset" label={t("pools.dataset")} error={errors.dataset}>
          {(a11y) => (
            <Input
              id="pool-dataset"
              placeholder="tank/opl"
              value={dataset}
              onChange={(e) => setDataset(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      {kind === "rbd" && (
        <>
          <Field id="pool-ceph-pool" label={t("pools.cephPool")} error={errors.cephPool}>
            {(a11y) => (
              <Input
                id="pool-ceph-pool"
                placeholder="rbd"
                value={cephPool}
                onChange={(e) => setCephPool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-namespace" label={t("pools.namespace")} error={errors.namespace}>
            {(a11y) => (
              <Input
                id="pool-namespace"
                value={namespace}
                onChange={(e) => setNamespace(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-user"
            label={t("pools.cephUser")}
            error={errors.cephUser}
            hint={t("pools.cephUserHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-user"
                value={cephUser}
                onChange={(e) => setCephUser(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-conf"
            label={t("pools.cephConf")}
            error={errors.cephConf}
            hint={t("pools.cephConfHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-conf"
                value={cephConf}
                onChange={(e) => setCephConf(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "dir" && (
        <>
          <Field
            id="pool-path"
            label={t("pools.path")}
            error={errors.path}
            hint={t("pools.pathHint")}
          >
            {(a11y) => (
              <Input
                id="pool-path"
                placeholder="/var/lib/opl/pool"
                value={path}
                onChange={(e) => setPath(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* 밖에서 마운트한 공유 디렉터리. 공유가 아닌 풀의 VM은 볼륨이 있는 노드에서만 돈다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-shared"
                checked={shared}
                onCheckedChange={(v) => setShared(v === true)}
                aria-describedby="pool-shared-hint"
              />
              <Label htmlFor="pool-shared">{t("pools.shared")}</Label>
            </div>
            <p id="pool-shared-hint" className="text-xs text-muted-foreground">
              {t("pools.sharedHint")}
            </p>
          </div>
        </>
      )}
      {kind === "nfs" && (
        <>
          <Field
            id="pool-server"
            label={t("pools.server")}
            error={errors.server}
            hint={t("pools.serverHint")}
          >
            {(a11y) => (
              <Input
                id="pool-server"
                placeholder="nas.lan"
                value={nfsServer}
                onChange={(e) => setNfsServer(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-export"
            label={t("pools.export")}
            error={errors.export}
            hint={t("pools.exportHint")}
          >
            {(a11y) => (
              <Input
                id="pool-export"
                placeholder="/srv/vm"
                value={nfsExport}
                onChange={(e) => setNfsExport(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-options"
            label={t("pools.options")}
            error={errors.options}
            hint={t("pools.optionsHint")}
          >
            {(a11y) => (
              <Input
                id="pool-options"
                value={nfsOptions}
                onChange={(e) => setNfsOptions(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      <fieldset
        className="flex flex-col gap-2"
        aria-describedby={errors.nodes ? "pool-nodes-error" : undefined}
      >
        <legend className="mb-2 text-sm font-medium">{t("pools.nodes")}</legend>
        {!nodes.isSuccess && <ListPending error={nodes.error} />}
        {(nodes.data ?? []).map((node) => (
          <div key={node.id} className="flex items-center gap-2">
            <Checkbox
              id={`pool-node-${node.id}`}
              checked={chosen.includes(node.id)}
              onCheckedChange={(v) =>
                setChosen((c) => (v === true ? [...c, node.id] : c.filter((id) => id !== node.id)))
              }
            />
            <Label htmlFor={`pool-node-${node.id}`}>{node.name}</Label>
          </div>
        ))}
        <p className="text-xs text-muted-foreground">{t("pools.nodesHint")}</p>
        {errors.nodes && (
          <p id="pool-nodes-error" className="text-xs text-destructive">
            {t(errors.nodes)}
          </p>
        )}
      </fieldset>
    </FormDialog>
  );
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 747개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 310개

- [ ] **Step 3: 커밋**

```bash
git add crates web
git commit -m "feat(oplctl,web): pool set --shared|--no-shared와 웹 풀 목록의 공유 스위치"
```

---

### Task 5: 실제 시험: 멈춘 UEFI VM을 옮겨 켜기

`crates/opld/tests/migration.rs`에 `uefi_variables_follow_a_stopped_vm`: 로컬 디렉터리 풀을 `oplctl pool set --shared`로 공유로, node1에서 UEFI VM을 켜 `firmware_node` 확인, 멈춘 뒤 node1 변수 파일 끝에 표식, `PUT /vms/web`의 `node`로 node2에 옮김, 공유→로컬 `PATCH`는 409 `pool_in_use`, 켜면 node2 파일 끝에 표식·node1 파일은 남음·`firmware_node` = node2.

**Files:**
- Modify: `crates/opld/tests/migration.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/migration.rs`에서

```rust
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 게스트에는 OS가 없으므로(UEFI
//! 펌웨어만) 디스크 내용은 정지한 VM에서 `qemu-io`로 본다.
//! `cargo test -p opld --features migration-it --test migration`
```

을 다음으로 바꾼다:

```rust
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 멈춘 UEFI VM을 옮겨 켜면 변수
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다.
//! `cargo test -p opld --features migration-it --test migration`
```

`crates/opld/tests/migration.rs`에서

```rust

/// NFS 풀(두 노드가 각자 마운트한다)
```

을 다음으로 바꾼다:

```rust

/// 멈춘 UEFI VM을 다른 노드로 옮겨(`PUT`의 `node`) 켜면, 그 노드가 앞 노드의 변수 파일을 받아 켜고
/// 변수 노드가 된다. 로컬로 만든 디렉터리 풀을 공유로 바꾸고(`pool set`), VM이 볼륨과 다른 노드에
/// 있는 동안은 다시 로컬로 바꾸지 못하는지도 본다.
#[tokio::test]
async fn uefi_variables_follow_a_stopped_vm() {
    let p = Pair::new("mig-vars", ["60180-60189", "60190-60199"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let (node1, node2) = (p.node_id(0).await, p.node_id(1).await);

    let dir = tempfile::Builder::new()
        .prefix("oplvars")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "vars",
        "--path",
        dir.path().to_str().unwrap(),
    ])
    .await
    .unwrap();
    p.wait_pool_ready("vars").await;
    d.ctl(&[
        "volume",
        "create",
        "vars-root",
        "--pool",
        "vars",
        "--size",
        "64M",
    ])
    .await
    .unwrap();
    let out = d.ctl(&["pool", "set", "vars", "--shared"]).await.unwrap();
    assert!(out.contains("storage pool vars: shared"), "{out}");

    // 노드 1에서 켜면 그 노드가 변수 노드다. 끈 뒤 노드 1의 변수 파일 끝에 표식을 남긴다.
    d.ctl(&[
        "vm",
        "create",
        "web",
        "--memory",
        "256",
        "--disk",
        "vars-root",
        "--node",
        "node1",
        "--start",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let uuid = vm["id"].as_str().unwrap().parse().unwrap();
    eventually("node 1 has the UEFI variables", || async {
        let vm = c.get("/vms/web").await.unwrap();
        vm["status"]["firmware_node"].as_str() == Some(node1.as_str())
    })
    .await;
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let src_vars = opl_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = opl_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
    let mut content = std::fs::read(&src_vars).unwrap();
    let at = content.len() - MARKER.len();
    content[at..].copy_from_slice(MARKER);
    std::fs::write(&src_vars, &content).unwrap();
    assert!(!dst_vars.exists());

    // 멈춘 채 노드 2로 옮긴다. VM이 볼륨과 다른 노드에 있으므로 풀을 로컬로 되돌릴 수 없다.
    let mut spec = c.get("/vms/web").await.unwrap()["spec"].clone();
    spec["node"] = json!(node2);
    c.put("/vms/web", &json!({ "spec": spec })).await.unwrap();
    let (status, body) = error(
        c,
        Method::PATCH,
        "/storage-pools/vars",
        json!({ "shared": false }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_in_use", "{body}");

    // 노드 2가 노드 1의 변수 파일을 받아 켜고 변수 노드가 된다. 노드 1의 파일은 남는다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    assert!(std::fs::read(&dst_vars).unwrap().ends_with(MARKER));
    assert!(src_vars.exists());
    eventually("node 2 has the UEFI variables", || async {
        let vm = c.get("/vms/web").await.unwrap();
        vm["status"]["firmware_node"].as_str() == Some(node2.as_str())
    })
    .await;
    assert!(process_with(&vm_run_dir(
        &p.d[1],
        vm["id"].as_str().unwrap()
    )));

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "vars-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "vars"]).await.unwrap();
}

/// NFS 풀(두 노드가 각자 마운트한다)
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 747개, 실패 0

Run: `cargo test -p opld --features migration-it --test migration`
Expected: macOS에서 PASS 2개(TCG)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "test(opld): 멈춘 UEFI VM을 옮겨 켜면 변수 파일이 따라가는 실제 시험"
```

---

### Task 6: 문서

README(풀의 공유 여부 바꾸기 — `pool set`·웹 스위치·규칙, UEFI 변수가 멈춘 채 옮긴 VM을 따라감 — 노드가 죽으면 템플릿, 모든 노드가 같은 펌웨어 빌드, 남는 복사본, 다른 노드의 스냅샷에서 복제·되돌리기), 설계 문서 §5~§8의 "구현:" 메모, 처음 분해 문서의 UEFI 변수·`PoolSpec.shared` 줄.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-10-30-live-migration-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
oplctl pool create shared --path /mnt/vm --shared    # opLite 밖에서 마운트한 공유 디렉터리(모든 노드가 같은 파일을 본다)
oplctl pool list                                     # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
```

을 다음으로 바꾼다:

```markdown
oplctl pool create shared --path /mnt/vm --shared    # opLite 밖에서 마운트한 공유 디렉터리(모든 노드가 같은 파일을 본다)
oplctl pool set local --shared                       # 디렉터리 풀을 공유로(--no-shared는 노드 로컬로)
oplctl pool list                                     # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
```

`README.md`에서

```markdown
  (API `"shared": true`, 웹은 "공유 스토리지" 체크). `--shared`는 `--nfs`와 함께 쓰지 않는다.
- **노드별 풀 상태.** 노드는 자기가 쓸 수 있는 풀마다 준비 여부·용량·오류를 노드 상태(`status.pools`)에 알린다.
```

을 다음으로 바꾼다:

```markdown
  (API `"shared": true`, 웹은 "공유 스토리지" 체크). `--shared`는 `--nfs`와 함께 쓰지 않는다.
- **공유 여부 바꾸기.** 디렉터리 풀만 `oplctl pool set POOL --shared|--no-shared`(관리자)로 바꾼다(API
  `PATCH /api/v1/storage-pools/{key}` `{"shared": bool}`, `If-Match` 선택. 웹은 스토리지 풀 목록의 "공유" 스위치와
  확인 대화상자). 다른 풀은 같은 값이어도 **400** `invalid_request`다(NFS·RBD·공유 LVM은 늘 공유, LVM-thin·ZFS는
  늘 로컬). 같은 값이면 바꾸지 않고 200이다. 로컬 → 공유는 언제나 된다(그 경로가 풀의 모든 노드에서 같은 공유 파일
  시스템인지는 관리자가 보증한다). 공유 → 로컬은 그 풀의 볼륨을 쓰는 VM이 볼륨을 만든 노드가 아닌 노드에 있거나
  마이그레이션 중이면 **409** `pool_in_use`(VM 이름)다. VM을 볼륨의 노드로 되돌린 뒤 다시 한다. 로컬이 되면 볼륨은
  만든 노드에 있는 것으로 돌아간다.
- **노드별 풀 상태.** 노드는 자기가 쓸 수 있는 풀마다 준비 여부·용량·오류를 노드 상태(`status.pools`)에 알린다.
```

`README.md`에서

```markdown
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다. 공유가 아닌 풀의 VM은 이제 볼륨이 있는
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 `--shared`로 만들어야 VM이 다른 노드로 갈 수 있다. 풀을 바꾸는
  API는 없으므로 기존 풀을 공유로 바꾸려면 볼륨을 다른 풀로 옮기고(예: 복제) 풀을 다시 만든다.
```

을 다음으로 바꾼다:

```markdown
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다. 공유가 아닌 풀의 VM은 이제 볼륨이 있는
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 공유여야 VM이 다른 노드로 갈 수 있다. 기존 풀은
  `oplctl pool set POOL --shared`로 바꾼다.
```

`README.md`에서

```markdown
  끈다(확정 뒤, 시작 때와 30분마다).
- **UEFI 변수.** QEMU가 변수(pflash)도 함께 옮겨 대상 노드의 변수 파일에 쓴다.
- **공유 LVM.** 옮기는 동안 디스크 LV를 공유 잠금(`lvchange -asy`)으로 바꾸고, 대상이 넘겨받은 뒤 다시 배타로
```

을 다음으로 바꾼다:

```markdown
  끈다(확정 뒤, 시작 때와 30분마다).
- **UEFI 변수.** QEMU가 변수(pflash)도 함께 옮겨 대상 노드의 변수 파일에 쓴다. 변수 파일은 노드마다
  `{data_dir}/vm/{id}/uefi-vars.fd`에 있고, 클러스터 기록이 지금 파일을 가진 노드(`status.firmware_node`)를 안다.
  멈춘 채 다른 노드로 옮긴 VM(웹 VM 편집이나 API `PUT /api/v1/vms/{key}`의 `node`)은 켤 때 그 노드에서 노드 사이
  연결로 변수를 받아 온다. 스냅샷·복제도 변수를 복사하기 전에 받아 온다. 그 노드가 꺼졌거나 죽었거나 받지 못하면
  경고를 남기고 켜는 노드의 파일(없으면 템플릿)로 켠다 — 그 노드에만 있던 변수(부팅 순서 등)는 잃는다. 옛 노드의
  파일은 지우지 않는다. 모든 노드가 같은 펌웨어 빌드(같은 크기의 UEFI 변수 템플릿, `[hypervisor] uefi_vars`)를
  써야 한다 — 받은 변수 파일의 크기가 켜는 노드의 템플릿과 다르면 버리고 같은 경고 뒤 그 노드의 파일로 켠다.
  한계: 스냅샷의 변수 복사본은 받아 오지 않으므로, 스냅샷에서 복제하는 작업이 스냅샷을 찍은 노드가 아닌 곳에서
  돌면 새 VM은 템플릿으로 켜고, 되돌리기가 그런 노드에서 돌면 변수는 되돌리지 않고 지금 것을 둔다(경고). VM이
  거쳐 간 다른 노드의 변수 파일은 VM을 지워도 그 노드에 남는다.
- **공유 LVM.** 옮기는 동안 디스크 LV를 공유 잠금(`lvchange -asy`)으로 바꾸고, 대상이 넘겨받은 뒤 다시 배타로
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다. 구현: 변수 파일은 노드 로컬에 그대로 둔다. 라이브 마이그레이션에서는 QEMU가 pflash 내용을 옮겨 대상 노드의 파일에 쓰고(계획 20, CI에서 확인), 멈춘 채 옮긴 VM은 시작할 때 변수를 가진 노드에서 받아 온다(계획 21, 라이브 마이그레이션 설계 §5).
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
```

을 다음으로 바꾼다:

```markdown
- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다. 구현: 변수 파일은 노드 로컬에 그대로 둔다. 라이브 마이그레이션에서는 QEMU가 pflash 내용을 옮겨 대상 노드의 파일에 쓰고(계획 20, CI에서 확인), 멈춘 채 옮긴 VM은 시작할 때 변수를 가진 노드(`VmStatus.firmware_node`)에서 노드 사이 RPC로 받아 온다(계획 21에서 구현, 라이브 마이그레이션 설계 §5). 그 노드가 죽었으면 템플릿으로 켠다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다. 구현: 공유 스토리지 설계(계획 15~19)부터 배치와 Task 노드 선택에 쓰고(`is_shared`, NFS·RBD·공유 LVM은 늘 공유), 라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀일 때만 한다(아니면 409 `migration_unsupported`, 디스크 복사는 하지 않는다). 디렉터리 풀의 `shared` 바꾸기는 계획 21이다.
- 사용 중인 Volume은 삭제할 수 없다(409).
```

을 다음으로 바꾼다:

```markdown
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다. 구현: 공유 스토리지 설계(계획 15~19)부터 배치와 Task 노드 선택에 쓰고(`is_shared`, NFS·RBD·공유 LVM은 늘 공유), 라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀일 때만 한다(아니면 409 `migration_unsupported`, 디스크 복사는 하지 않는다). 디렉터리 풀의 `shared` 바꾸기는 계획 21에서 구현했다(`PATCH /api/v1/storage-pools/{key}`, `oplctl pool set`, 공유 → 로컬은 다른 노드의 VM이 쓰면 409 `pool_in_use`).
- 사용 중인 Volume은 삭제할 수 없다(409).
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown

## 6. 풀의 `shared` 바꾸기 (계획 21)
```

을 다음으로 바꾼다:

```markdown

구현: `firmware_node`는 `serde(default, skip_serializing_if)`인 선택 필드다(OpenAPI에서 필수 아님). RPC는 opl-peer의 이진 코덱(요청은 VM id 16바이트, 조각은 태그 0 데이터 / 태그 1 0 조각 길이 u64 BE, 오류는 gRPC 상태 코드 — 파일 없음은 `NotFound`)이고 기존 회원 검사를 거친다. 받는 쪽(`Peers::uefi_vars`, 60초)은 같은 디렉터리의 `.uefi-vars.fd.{uuid}.tmp`에 받아 flush·fsync 뒤 이름을 바꾸고(디렉터리도 fsync), 실패하면 기존 파일을 그대로 둔다. 받는 크기는 256 MiB까지이고, `UefiVarsFetcher`는 받은 크기가 이 노드의 UEFI 변수 템플릿(하이퍼바이저 설정)과 다르면 실패로 친다(다른 펌웨어 빌드). reconciler는 opl-peer에 의존하지 않고 `FirmwareFetcher` trait(`ReconcilerConfig.firmware`, Task 실행기는 `TaskRunnerConfig.firmware`)으로 받으며, opld가 opl-api의 `UefiVarsFetcher`(오프라인으로 기록된 노드는 연결해 보지 않고, 회원이 아니면 오류)를 넣는다. 워커는 QEMU를 새로 띄울 때(`start()`, 디스크 resolve 뒤)만 받고 다시 붙기·마이그레이션 넘겨받기에서는 받지 않는다. `firmware_node = 자기`는 QEMU가 뜬 뒤 `Running` 기록과 함께, 그리고 이 노드에서 UEFI VM의 QEMU가 도는 것을 볼 때마다 쓴다(넘겨받기가 원본의 마지막 기록보다 먼저 일어나는 경우). 워커의 status 쓰기는 자기가 바꾸지 않은 `firmware_node`를 덮지 않고, 상태를 새로 만드는 곳은 값을 이어받는다. 마이그레이션 확정 뒤의 status 쓰기(`finish()`)가 T를 쓴다. 스냅샷·복제 Task는 변수를 복사하기 전에 받고(BIOS, 이미 이 노드, 이 노드에서 QEMU가 돌면 받지 않는다), 되돌리기는 변수를 쓴 Task 노드를, 복제는 새 VM의 변수 복사본을 둔 Task 노드를 `firmware_node`로 쓴다. 실패는 모두 `tracing::warn` 뒤 이 노드의 파일(없으면 템플릿 또는 복사하지 않음)이다.

## 6. 풀의 `shared` 바꾸기 (계획 21)
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- revision 검사로 쓴다. oplctl `pool set POOL --shared|--no-shared`, 웹 풀 상세의 공유 스위치(디렉터리 풀만).
```

을 다음으로 바꾼다:

```markdown
- revision 검사로 쓴다. oplctl `pool set POOL --shared|--no-shared`, 웹 풀 상세의 공유 스위치(디렉터리 풀만).

구현: 경로는 기존 풀 경로인 `PATCH /api/v1/storage-pools/{key}`이고 본문 `PoolPatch { shared }`는 모르는 필드를 거절한다. 디렉터리가 아닌 풀은 같은 값이어도 400 "only dir pools can change shared; {kind} pools are always shared/node-local"이고, 같은 값이면 쓰지 않고 200이다. `If-Match`(선택, 다르면 412)가 없으면 revision 검사 쓰기를 10번까지 다시 해 보고 409 `busy`다. 검사는 VM 옮기기·마이그레이션 요청과 같은 `vm_lock` 안에서 한다. `vm_lock`은 이 노드의 요청만 막으므로 공유 → 로컬은 쓴 뒤 다시 검사해, 이제 안 되면 revision 검사로 공유로 되돌리고 409 `pool_in_use`다. 노드에 놓이지 않은 VM이나 만든 노드가 기록되지 않은 볼륨은 막지 않는다. oplctl `pool set`은 `--shared`·`--no-shared` 중 정확히 하나를 받고 `storage pool X: shared|node-local`을 찍는다. 웹은 풀 상세가 아니라 풀 목록의 "공유" 칸(쓰기 권한과 디렉터리 풀이면 스위치, 아니면 글자)이고 양쪽 다 확인 대화상자, 409는 `pools.sharingErrors.<code>` 문구에 서버 문구(VM 이름)를 붙인다.
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown

## 8. 알려진 한계
```

을 다음으로 바꾼다:

```markdown

구현(계획 21): `migration-it`의 `uefi_variables_follow_a_stopped_vm`은 로컬로 만든 디렉터리 풀을 `oplctl pool set --shared`로 공유로 바꾸고, node1에서 켠 VM의 `firmware_node`를 확인한 뒤 멈추고 node1 변수 파일 끝에 표식을 쓰고 `PUT`의 `node`로 node2에 옮긴다. 이때 공유 → 로컬 `PATCH`가 409 `pool_in_use`인지 보고, 켜면 node2 파일에 표식이 있고(node1 파일은 남는다) `firmware_node`가 node2인지 본다. 펌웨어가 켤 때 변수를 다시 쓰므로 해시가 아니라 표식으로 본다. 단위·in-process: opl-peer gRPC(조각·0 조각·없음·비회원), reconciler(가짜 fetcher — 받음, 실패해도 켬, 같은 노드면 안 받음, BIOS), `opl-api/tests/two_nodes.rs`(실제 gRPC로 멈춘 VM 옮겨 켜기), `tests/pools.rs`의 `PATCH`.

## 8. 알려진 한계
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- 취소가 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고, 시간 제한(30분)이 지나면 실패로 끝나 원본이 계속 돈다.
- 노드가 죽으면 그 노드에만 있던 UEFI 변수는 잃는다(템플릿으로 켠다).
- 마이그레이션 포트 범위를 노드 사이 방화벽에서 열어야 한다.
```

을 다음으로 바꾼다:

```markdown
- 취소가 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고, 시간 제한(30분)이 지나면 실패로 끝나 원본이 계속 돈다.
- 노드가 죽으면 그 노드에만 있던 UEFI 변수는 잃는다(템플릿으로 켠다). 구현: 계획 21대로 — 다른 노드가 켤 때 경고를 남기고 그 노드의 파일이나 템플릿으로 켠다.
- 스냅샷의 변수 복사본(`snapshots/{id}.vars`)은 RPC로 받지 않는다(변수 파일만). 스냅샷에서 복제하는 Task가 스냅샷을 찍은 노드가 아닌 곳에서 돌면 새 VM은 변수 없이(템플릿으로) 켜진다. 되돌리기 Task가 그런 노드에서 돌면 변수는 되돌리지 않고 지금 변수를 둔다(경고).
- 모든 노드가 같은 펌웨어 빌드(같은 크기의 변수 템플릿)를 써야 변수가 따라간다. 크기가 다르면 받은 파일을 버리고 켜는 노드의 파일(없으면 템플릿)로 켠다.
- VM이 거쳐 간 다른 노드의 변수 파일(받은 쪽은 옛 파일을 지우지 않는다)은 VM을 지워도 그 노드에 남는다.
- 마이그레이션 포트 범위를 노드 사이 방화벽에서 열어야 한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 747개, 실패 0

Run: `cargo test -p opld --features qemu-it,migration-it --test smoke --test e2e --test migration`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 310개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: UEFI 변수가 멈춘 채 옮긴 VM을 따라감과 pool set 안내, 설계에 계획 21 구현 메모"
```

---

## 완료 기준 (계획 21)

- 멈춘 UEFI VM을 다른 노드로 옮겨 켜면 앞 노드의 변수 파일로 부팅하고 `firmware_node`가 새 노드가 된다. 스냅샷·복제도 최신 변수를 복사한다. 변수를 가진 노드가 없거나 받지 못하면 경고 뒤 켜진다.
- 실행 중인 QEMU의 변수 파일을 바꾸지 않고, 받다가 실패하면 원래 파일이 그대로다. 다른 펌웨어 빌드의 변수는 쓰지 않는다.
- 디렉터리 풀의 `shared`를 API·oplctl·웹으로 바꿀 수 있고, 다른 종류는 400, 공유→로컬은 설계의 경우 409 `pool_in_use`다.
- 이 Mac에서 `migration-it`(멈춘 채 옮기기 포함)이, Linux CI(`linux-kvm`)에서 계획 20의 시험과 함께 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **변수를 가진 노드가 죽으면 그 변수를 잃는다**(템플릿으로 켠다).
- **모든 노드가 같은 펌웨어 빌드를 써야** 변수가 따라간다(크기가 다르면 받지 않는다).
- **VM이 거쳐 간 노드에 변수 복사본이 남는다**(VM을 지워도 다른 노드의 것은 남는다).
- **다른 노드에서 찍은 스냅샷**으로 복제하면 새 VM은 템플릿으로, 되돌리면 지금 변수를 그대로 쓴다(스냅샷의 변수 복사본은 찍은 노드에만 있다).
- 그 밖의 제한은 설계 문서 §8에 있다.

## 실행 후 기록 (2026-10-31)

6개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 한 차례 돌려 고쳤다(받은 파일의 쓰기 오류가 묻히지 않게 `flush`, 0 조각 길이·전체 크기 검사, 디렉터리 fsync, 다른 펌웨어 빌드의 변수 거절, `PATCH` 뒤 다시 검사해 되돌리기, 마이그레이션 `finish()`의 기록을 확인하는 시험, 스위치 대화상자 문구, 다른 노드의 스냅샷 되돌리기 경고 — 위 Task의 코드에 이미 들어 있다).

Task 6 뒤에 아래를 고쳤다(`b56efee`). 저장소의 코드가 기준이다. 위 Task 5의 코드 블록은 고치기 전 것이다.

- CI 확인용 브랜치의 `linux-kvm`(KVM)에서 `uefi_variables_follow_a_stopped_vm`이 노드 2 변수 파일 끝의 표식을 찾지 못했다. KVM에서는 OVMF가 빨리 시작하면서 변수 저장소의 빈 영역에 있는 표식을 정리한다(이 Mac의 TCG에서는 확인할 때까지 남아 있었다). 시험은 파일 내용 대신 노드 2의 opld 로그에 "fetched UEFI variables"가 있고 "cannot fetch UEFI variables"가 없는지 본다. 이 수정으로 CI 5개 작업이 모두 통과했다.
- 그 전에 `linux-kvm`이 snap(`core24`·`snapd`) 다운로드 연결 끊김으로 두 번 실패했다(코드와 무관, 다시 돌림).

테스트: macOS `cargo test --workspace` 747개, `opl-api` `storage-it` 255개, `opld` `qemu-it`·`migration-it`(opld 둘, TCG), 브라우저 E2E, 웹 Vitest 310개, `fmt`·`clippy -D warnings`(모든 기능). Linux CI `linux-kvm`(KVM)에서 계획 20의 마이그레이션 시험과 멈춘 채 옮기기 시험이 통과했다.

## 다음 계획으로 넘길 사항

- 하위 프로젝트 4(라이브 마이그레이션, UEFI 변수, 풀 `shared`)를 마쳤다. 다음은 처음 분해(`2026-10-20-opl-node-core-design.md` §1.1)의 다음 하위 프로젝트다.
- 노드가 죽으면 그 노드에만 있던 UEFI 변수를 잃는다 — HA에서 공유 풀의 변수 보관을 다시 본다.
- VM을 지워도 다른 노드에 남는 변수 복사본 정리, 스냅샷의 변수 복사본 전송.
- CI에서 `opl-api`의 `storage-it` 전체를 돌리는 것을 검토한다. CI의 snap 설치에 재시도를 두는 것도 검토한다.
