# opLite 계획 2: 하이퍼바이저·스토리지 드라이버 (M2) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `opl-driver` 크레이트를 만든다. reconciler와 API가 의존할 `Hypervisor`/`Storage` 트레이트, 테스트용 `MockHypervisor`, 실제 QEMU를 opld와 분리해 띄우고 재시작 후 다시 붙는 `QemuHypervisor`, `qemu-img` 기반 `dir` 스토리지 풀(VMDK 가져오기 포함)을 구현한다.

**Architecture:** QEMU는 `-daemonize`로 띄워 opld와 세션을 분리한다. 기동 실패는 부모 프로세스의 종료 코드와 stderr로 즉시 받는다. 드라이버는 VM마다 `QmpClient`와 모니터 태스크를 둔다. 모니터는 QMP 연결이 끊기면 `SHUTDOWN` 이벤트 유무로 정상 종료와 크래시를 구분해 `HypervisorEvent`로 알린다. `QemuHypervisor::new()`는 런타임 디렉터리를 훑어 살아 있는 VM에 다시 연결한다. 이것이 "opld가 재시작해도 VM은 계속 돈다"는 불변식의 구현이다. 스토리지는 풀 디렉터리에 `{volume-id}.{qcow2|raw}` 파일을 두고 `qemu-img`로 다룬다.

**Tech Stack:** Rust 2024, tokio, async-trait, tracing, fs4(파일시스템 용량), QEMU 11.x/qemu-img, `opl-qemu`(계획 1), `opl-types`

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (5.2, 5.4, 5.5, 6절)
**선행 계획:** `docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md` (완료)

---

## 범위

| 포함 | 제외 (담당 계획) |
|---|---|
| `Hypervisor` 트레이트, `MockHypervisor`, `QemuHypervisor` | VmSpec → VmRuntimeSpec 변환(Volume/Network 이름 해석) — 계획 4 reconciler |
| `-daemonize` 기동, QMP 재부착, 크래시 감지 | ACPI 종료 타임아웃 후 강제 종료 승격 — 계획 4 |
| `Storage` 트레이트, `DirStorage`(생성·가져오기·삭제·정보·용량) | `systemd-run --scope` 자원 격리, `QemuKvm` 호스트 프로필 — 계획 7 |
| `MacAddr::generate()`, `StoragePool`/`Volume` 리소스 타입 | tap 장치 생성(Network 드라이버) — 계획 7 |
| CI에 Linux 스토리지 통합 테스트 추가 | Mock의 동시 호출 감지 — 계획 4에서 필요할 때 |

## 계획 1 실행 결과에서 이어받는 사항

- `QmpClient::execute`를 타임아웃으로 취소해도 명령은 나중에 실행될 수 있다. 드라이버의 `inspect`는 항상 `query_status`로 실제 상태를 다시 읽는다.
- `QmpClient::subscribe()`는 연결이 끊기면 `Closed`를 돌려준다. 모니터 태스크는 이것을 프로세스 종료 신호로 쓴다.
- QEMU QMP는 클라이언트를 **하나만** 받는다. 드라이버가 연결을 놓지 않으면 재시작한 opld가 VM에 붙을 수 없다(Task 6의 회귀 테스트가 이를 고정한다).

## 파일 구조

```
Cargo.toml                                 # 수정: opl-qemu/async-trait/fs4/tracing 추가, uuid v4
.github/workflows/ci.yml                   # 수정: Linux에서 storage-it 실행
README.md                                  # 수정: 개발 명령
docs/superpowers/specs/...-design.md       # 수정: 5.2, 5.4절을 구현에 맞춤
crates/opl-types/src/
├── lib.rs                                 # 수정: storage 모듈
├── vm.rs                                  # 수정: MacAddr::generate
└── storage.rs                             # 신규: StoragePool/Volume 리소스
crates/opl-driver/
├── Cargo.toml                             # features: storage-it, qemu-it
├── src/
│   ├── lib.rs                             # 모듈 선언 + 재노출
│   ├── error.rs                           # DriverError, is_transient
│   ├── hypervisor.rs                      # Hypervisor 트레이트와 상태·이벤트 타입
│   ├── mock.rs                            # MockHypervisor
│   ├── qemu.rs                            # QemuHypervisor, 모니터 태스크
│   └── storage.rs                         # Storage 트레이트, DirStorage
└── tests/
    ├── dir_storage.rs                     # storage-it: 실제 qemu-img
    └── qemu_driver.rs                     # qemu-it: 실제 QEMU (Apple Silicon)
```

---

### Task 1: `MacAddr::generate()`

VM 생성 시 MAC을 자동 할당하는 데 쓴다(계획 1에서 넘어온 항목). 난수는 이미 의존하는 `uuid`의 v4 생성기를 써서 의존성을 늘리지 않는다.

**Files:**
- Modify: `Cargo.toml` (uuid features), `crates/opl-types/src/vm.rs`

- [ ] **Step 1: uuid v4 기능 켜기**

`Cargo.toml`의 `[workspace.dependencies]`에서 uuid 줄을 바꾼다:

```toml
uuid = { version = "1.28.0", features = ["v4", "v7", "serde"] }
```

- [ ] **Step 2: 실패하는 테스트 작성**

`crates/opl-types/src/vm.rs`의 `mod tests` 안, `fn mac_rejects_malformed()` 바로 위에 추가한다:

```rust
    #[test]
    fn generated_mac_is_unicast_with_qemu_prefix() {
        let a = MacAddr::generate();
        let b = MacAddr::generate();
        assert_eq!(&a.0[..3], &[0x52, 0x54, 0x00]);
        assert_eq!(a.0[0] & 1, 0);
        assert_ne!(a, b);
    }
```

- [ ] **Step 3: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-types generated_mac`
Expected: 컴파일 실패 — `no function or associated item named 'generate' found for struct 'MacAddr'`

- [ ] **Step 4: 구현 작성**

`crates/opl-types/src/vm.rs`에서 `pub struct MacAddr(pub [u8; 6]);` 정의 바로 아래(`MacParseError` 앞)에 추가한다:

```rust
impl MacAddr {
    /// QEMU/KVM 관례 접두사 `52:54:00`에 난수 3바이트를 붙인 주소를 만든다.
    pub fn generate() -> Self {
        let r = Uuid::new_v4().into_bytes();
        MacAddr([0x52, 0x54, 0x00, r[0], r[1], r[2]])
    }
}
```

- [ ] **Step 5: 테스트 통과 확인**

Run: `cargo test -p opl-types`
Expected: `test result: ok. 11 passed`

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-types/src/vm.rs
git commit -m "feat(types): MAC 주소 자동 생성 추가"
```

---

### Task 2: `StoragePool` / `Volume` 리소스 타입

**Files:**
- Create: `crates/opl-types/src/storage.rs`
- Modify: `crates/opl-types/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-types/src/lib.rs`:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.

pub mod resource;
pub mod runtime;
pub mod storage;
pub mod vm;
```

`crates/opl-types/src/storage.rs`에 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_pool_deserializes_with_defaults() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "dir", "path": "/var/lib/opl/pools/local" }
        }))
        .unwrap();
        assert_eq!(
            spec.backend,
            PoolBackend::Dir {
                path: "/var/lib/opl/pools/local".into()
            }
        );
        assert!(spec.nodes.is_empty());
        assert!(!spec.shared);
    }

    #[test]
    fn rejects_unknown_backend_fields() {
        let res = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "dir", "path": "/p", "pth": "/typo" }
        }));
        assert!(res.is_err());
    }

    #[test]
    fn volume_spec_round_trips() {
        let spec = VolumeSpec {
            pool: "local".into(),
            size_bytes: 10 << 30,
            format: ImageFormat::Qcow2,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["format"], "qcow2");
        assert_eq!(serde_json::from_value::<VolumeSpec>(json).unwrap(), spec);
    }

    #[test]
    fn statuses_default_missing_fields() {
        let p: StoragePoolStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p, StoragePoolStatus::default());
        let v: VolumeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(v, VolumeStatus::default());
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-types`
Expected: 컴파일 실패 — `cannot find type 'StoragePoolSpec'`

- [ ] **Step 3: 구현 작성**

`crates/opl-types/src/storage.rs` 맨 위에 추가한다. 설계 문서 3.1의 저장 형식 호환성 규칙에 따라 spec에는 `deny_unknown_fields`, status에는 `#[serde(default)]`를 붙인다:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::Resource;
use crate::runtime::ImageFormat;

pub type StoragePool = Resource<StoragePoolSpec, StoragePoolStatus>;
pub type Volume = Resource<VolumeSpec, VolumeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoragePoolSpec {
    pub backend: PoolBackend,
    /// 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드.
    #[serde(default)]
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. 라이브 마이그레이션에서 쓴다.
    #[serde(default)]
    pub shared: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum PoolBackend {
    /// 로컬 디렉터리에 이미지 파일을 둔다.
    Dir { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct StoragePoolStatus {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// StoragePool 리소스 이름
    pub pool: String,
    pub size_bytes: u64,
    pub format: ImageFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VolumeStatus {
    /// 실제로 디스크에 할당된 크기
    pub allocated_bytes: Option<u64>,
    pub attached_to: Option<Uuid>,
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-types`
Expected: `test result: ok. 15 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-types
git commit -m "feat(types): StoragePool, Volume 리소스 타입 추가"
```

---

### Task 3: `opl-driver` 뼈대와 `DriverError`

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/opl-driver/Cargo.toml`, `crates/opl-driver/src/lib.rs`, `crates/opl-driver/src/error.rs`

- [ ] **Step 1: 워크스페이스 의존성 추가**

`Cargo.toml` 전체가 다음과 같아지도록 수정한다(`opl-qemu`, `async-trait`, `fs4`, `tracing` 추가):

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"
repository = "https://github.com/gaiger777/opLite"

[workspace.dependencies]
opl-types = { path = "crates/opl-types" }
opl-qemu = { path = "crates/opl-qemu" }
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.154"
thiserror = "2.0.21"
tokio = { version = "1.54.0", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time", "process"] }
uuid = { version = "1.28.0", features = ["v4", "v7", "serde"] }
insta = "1.49.0"
tempfile = "3.27.0"
async-trait = "0.1.92"
fs4 = "1.1.0"
tracing = "0.1.44"
```

- [ ] **Step 2: 크레이트 매니페스트와 lib.rs 작성**

`crates/opl-driver/Cargo.toml`:

```toml
[package]
name = "opl-driver"
description = "Hypervisor and storage drivers for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# qemu-img만 있으면 되는 스토리지 통합 테스트 (macOS: brew install qemu, Debian/Ubuntu: apt install qemu-utils)
storage-it = []
# 실제 VM을 띄우는 통합 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []

[dependencies]
async-trait.workspace = true
fs4.workspace = true
opl-qemu.workspace = true
opl-types.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/opl-driver/src/lib.rs`:

```rust
//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;

pub use error::DriverError;
```

- [ ] **Step 3: 실패하는 테스트 작성**

`crates/opl-driver/src/error.rs`에 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_errors() {
        assert!(DriverError::Timeout("qmp").is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
        assert!(DriverError::Io(std::io::Error::other("x")).is_transient());

        let cmd = QmpError::Command {
            class: "GenericError".into(),
            desc: "x".into(),
        };
        assert!(!DriverError::Qmp(cmd).is_transient());
        assert!(!DriverError::StartFailed("x".into()).is_transient());
        assert!(!DriverError::InvalidSpec("x".into()).is_transient());
        assert!(!DriverError::VolumeNotFound(Uuid::nil()).is_transient());
    }
}
```

- [ ] **Step 4: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-driver`
Expected: 컴파일 실패 — `cannot find type 'DriverError'`

- [ ] **Step 5: 구현 작성**

`crates/opl-driver/src/error.rs` 맨 위에 추가한다. `is_transient()`는 설계 문서 5.5절의 "일시적 오류는 백오프 재시도, 영구 오류는 status에 기록"을 판단하는 기준이다. `match`에 와일드카드를 쓰지 않는다. 새 variant를 추가할 때 분류를 빠뜨리면 컴파일 오류가 나게 하기 위해서다:

```rust
use opl_qemu::QmpError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("VM {0} is not running")]
    NotRunning(Uuid),
    #[error("invalid VM definition: {0}")]
    InvalidSpec(String),
    #[error("QEMU failed to start: {0}")]
    StartFailed(String),
    #[error("volume {0} not found")]
    VolumeNotFound(Uuid),
    #[error("volume {0} already exists")]
    VolumeExists(Uuid),
    #[error("{tool} failed: {stderr}")]
    ToolFailed { tool: &'static str, stderr: String },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error(transparent)]
    Qmp(#[from] QmpError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl DriverError {
    /// 같은 요청을 잠시 뒤 다시 시도하면 성공할 수 있는 오류인지.
    /// 영구 오류는 status에 기록하고 사용자에게 알려야 한다.
    pub fn is_transient(&self) -> bool {
        match self {
            DriverError::Qmp(QmpError::Command { .. }) => false,
            DriverError::Qmp(_) | DriverError::Timeout(_) | DriverError::Io(_) => true,
            DriverError::NotRunning(_)
            | DriverError::InvalidSpec(_)
            | DriverError::StartFailed(_)
            | DriverError::VolumeNotFound(_)
            | DriverError::VolumeExists(_)
            | DriverError::ToolFailed { .. } => false,
        }
    }
}
```

- [ ] **Step 6: 테스트 통과 확인**

Run: `cargo test -p opl-driver`
Expected: `test result: ok. 1 passed`

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-driver
git commit -m "feat(driver): opl-driver 크레이트와 DriverError 추가"
```

---

### Task 4: `Hypervisor` 트레이트와 `MockHypervisor`

**Files:**
- Create: `crates/opl-driver/src/hypervisor.rs`, `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/lib.rs`

- [ ] **Step 1: 트레이트와 타입 작성**

동작이 없는 인터페이스 정의다. 계약(멱등성, `Force`는 종료까지 대기)은 doc comment에 적고, Mock과 QEMU 구현의 테스트가 같은 계약을 검증한다.

`crates/opl-driver/src/hypervisor.rs`:

```rust
use std::path::PathBuf;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// 게스트에 ACPI 전원 버튼을 보낸다. 게스트가 무시할 수 있다.
    Acpi,
    /// 프로세스를 즉시 끝낸다.
    Force,
}

/// 프로세스가 살아 있는 VM의 상태
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualState {
    pub id: Uuid,
    pub pid: u32,
    pub status: RuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStatus {
    Running,
    /// 기동 직후 `cont` 전이거나 일시정지된 상태
    Paused,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypervisorEvent {
    pub id: Uuid,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Started,
    /// 정상 종료. `guest_initiated`는 게스트 OS 안에서 끈 경우 true다.
    Stopped {
        guest_initiated: bool,
    },
    /// 종료 이벤트 없이 프로세스가 사라졌다.
    Crashed,
}

/// VM 프로세스 수명을 관리한다. 같은 VM에 대한 호출을 직렬화하는 것은 호출자(reconciler)의 책임이다.
#[async_trait]
pub trait Hypervisor: Send + Sync {
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;

    /// `Acpi`는 요청만 보내고 바로 돌아온다. `Force`는 프로세스가 끝날 때까지 기다린다.
    /// 실행 중이 아니면 아무것도 하지 않고 성공한다.
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;

    /// 실행 중이 아니면 `None`
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;

    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;

    /// VNC 유닉스 소켓 경로
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다.
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;
}
```

`crates/opl-driver/src/lib.rs`:

```rust
//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;
pub mod hypervisor;
pub mod mock;

pub use error::DriverError;
pub use hypervisor::{ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode};
pub use mock::MockHypervisor;
```

- [ ] **Step 2: Mock 실패하는 테스트 작성**

`crates/opl-driver/src/mock.rs`에 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        hv.start(&vm).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm).await.unwrap();
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a).await.unwrap();
        hv.start(&b).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }
}
```

- [ ] **Step 3: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-driver mock`
Expected: 컴파일 실패 — `cannot find type 'MockHypervisor'`

- [ ] **Step 4: Mock 구현 작성**

`crates/opl-driver/src/mock.rs` 맨 위에 추가한다:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        if self.state.lock().unwrap().vms.remove(&id).is_some() {
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(id: Uuid, pid: u32) -> ActualState {
        ActualState {
            id,
            pid,
            status: RuntimeStatus::Running,
        }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            if st.vms.contains_key(&vm.id) {
                return Ok(());
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        // 실제 QEMU에서도 ACPI 종료는 게스트가 스스로 꺼진 것(guest: true)으로 보고된다.
        let guest_initiated = mode == StopMode::Acpi;
        self.remove(id, EventKind::Stopped { guest_initiated });
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        if self.state.lock().unwrap().vms.contains_key(&id) {
            Ok(PathBuf::from(format!("/mock/{id}/vnc.sock")))
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}
```

- [ ] **Step 5: 테스트 통과 확인**

Run: `cargo test -p opl-driver`
Expected: `test result: ok. 5 passed`

- [ ] **Step 6: Commit**

```bash
git add crates/opl-driver
git commit -m "feat(driver): Hypervisor 트레이트와 장애 주입용 MockHypervisor 추가"
```

---

### Task 5: `Storage` 트레이트와 `DirStorage`

**Files:**
- Create: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-driver/src/lib.rs`:

```rust
//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;
pub mod hypervisor;
pub mod mock;
pub mod storage;

pub use error::DriverError;
pub use hypervisor::{ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode};
pub use mock::MockHypervisor;
pub use storage::{Capacity, DirStorage, SourceFormat, Storage, VolumeInfo};
```

`crates/opl-driver/src/storage.rs`에 테스트만 먼저 넣는다. 이 단위 테스트는 `qemu-img` 없이 돈다. 실제 `qemu-img` 동작은 Task 7의 통합 테스트가 검증한다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn pool(path: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: path.into() },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn volume_path_uses_id_and_format() {
        let id = Uuid::nil();
        assert_eq!(
            DirStorage::volume_path(&pool("/pools/local"), id, ImageFormat::Qcow2),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.qcow2")
        );
        assert_eq!(
            DirStorage::volume_path(&pool("/pools/local"), id, ImageFormat::Raw),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.raw")
        );
    }

    #[tokio::test]
    async fn tool_failure_carries_stderr() {
        let storage = DirStorage::new("/bin/sh");
        // `sh create ...`는 파일이 없어 실패하고 stderr를 남긴다.
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let err = storage
            .create_volume(&p, Uuid::nil(), 1 << 20, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        match err {
            DriverError::ToolFailed { tool, stderr } => {
                assert_eq!(tool, "qemu-img");
                assert!(!stderr.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = DirStorage::new("qemu-img");
        let err = storage
            .delete_volume(
                &pool(tmp.path().to_str().unwrap()),
                Uuid::nil(),
                ImageFormat::Raw,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)));
    }

    #[tokio::test]
    async fn capacity_reports_filesystem_space() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = DirStorage::new("qemu-img")
            .capacity(&pool(tmp.path().to_str().unwrap()))
            .await
            .unwrap();
        assert!(cap.total_bytes > 0);
        assert!(cap.available_bytes <= cap.total_bytes);
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-driver storage`
Expected: 컴파일 실패 — `cannot find type 'DirStorage'`

- [ ] **Step 3: 구현 작성**

`crates/opl-driver/src/storage.rs` 맨 위에 추가한다. 핵심 결정:
- 볼륨 파일 이름은 `{id}.{형식}`이다. 사람이 읽는 이름은 Raft의 `Volume` 리소스에만 두고, 파일 시스템에는 바뀌지 않는 id만 쓴다.
- `import_volume`이 실패하면 만들다 만 파일을 지운다. 남겨 두면 같은 id로 다시 시도할 때 `VolumeExists`가 난다.
- `volume_info`는 `qemu-img info -U`를 쓴다. 실행 중인 VM이 이미지를 잠그고 있어도 읽을 수 있다.

```rust
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;

/// 가져올 원본 이미지 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeInfo {
    pub virtual_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// 볼륨 파일의 생성, 가져오기, 삭제. 볼륨은 풀 안에서 id로 식별한다.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn create_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// 외부 이미지(VMDK 등)를 변환해 새 볼륨으로 만든다. 실패하면 만들다 만 파일을 남기지 않는다.
    async fn import_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    async fn delete_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError>;

    /// VM이 실행 중이어도 조회할 수 있다.
    async fn volume_info(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError>;

    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError>;
}

/// `dir` 풀: 디렉터리 안에 `{id}.qcow2` / `{id}.raw` 파일을 두고 `qemu-img`로 다룬다.
pub struct DirStorage {
    qemu_img: PathBuf,
}

impl DirStorage {
    pub fn new(qemu_img: impl Into<PathBuf>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
        }
    }

    pub fn volume_path(pool: &StoragePoolSpec, id: Uuid, format: ImageFormat) -> PathBuf {
        let PoolBackend::Dir { path } = &pool.backend;
        path.join(format!("{id}.{}", fmt_name(format)))
    }

    async fn qemu_img(&self, args: &[&str]) -> Result<Vec<u8>, DriverError> {
        let out = tokio::process::Command::new(&self.qemu_img)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            return Err(DriverError::ToolFailed {
                tool: "qemu-img",
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(out.stdout)
    }
}

fn fmt_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    }
}

fn src_fmt_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Vmdk => "vmdk",
        SourceFormat::Qcow2 => "qcow2",
        SourceFormat::Raw => "raw",
    }
}

fn path_str(p: &Path) -> Result<&str, DriverError> {
    p.to_str()
        .ok_or_else(|| DriverError::InvalidSpec(format!("path is not UTF-8: {p:?}")))
}

#[async_trait]
impl Storage for DirStorage {
    async fn create_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.to_string();
        self.qemu_img(&[
            "create",
            "-q",
            "-f",
            fmt_name(format),
            path_str(&path)?,
            &size,
        ])
        .await?;
        Ok(path)
    }

    async fn import_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let args = [
            "convert",
            "-q",
            "-f",
            src_fmt_name(src_format),
            "-O",
            fmt_name(format),
            path_str(src)?,
            path_str(&path)?,
        ];
        if let Err(e) = self.qemu_img(&args).await {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        Ok(path)
    }

    async fn delete_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        match std::fs::remove_file(Self::volume_path(pool, id, format)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(DriverError::VolumeNotFound(id))
            }
            other => Ok(other?),
        }
    }

    async fn volume_info(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if !path.exists() {
            return Err(DriverError::VolumeNotFound(id));
        }
        // -U: 실행 중인 VM이 잡고 있는 이미지 잠금을 무시하고 읽기만 한다.
        let out = self
            .qemu_img(&[
                "info",
                "-U",
                "--output=json",
                "-f",
                fmt_name(format),
                path_str(&path)?,
            ])
            .await?;
        let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })?;
        let field = |k: &str| json.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(VolumeInfo {
            virtual_bytes: field("virtual-size"),
            allocated_bytes: field("actual-size"),
        })
    }

    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError> {
        let PoolBackend::Dir { path } = &pool.backend;
        Ok(Capacity {
            total_bytes: fs4::total_space(path)?,
            available_bytes: fs4::available_space(path)?,
        })
    }
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-driver`
Expected: `test result: ok. 9 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-driver
git commit -m "feat(driver): qemu-img 기반 dir 스토리지 풀 추가 (생성, VMDK 가져오기, 삭제, 정보, 용량)"
```

---

### Task 6: `QemuHypervisor`

드라이버의 핵심이다. 단위 테스트는 실제 QEMU 없이 돈다. 가짜 `qemu` 셸 스크립트로 기동 실패를 흉내 내고, 가짜 QMP 서버로 재부착을 흉내 낸다. 실제 QEMU 동작은 Task 7이 검증한다.

**Files:**
- Create: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-driver/src/lib.rs`:

```rust
//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.

pub mod error;
pub mod hypervisor;
pub mod mock;
pub mod qemu;
pub mod storage;

pub use error::DriverError;
pub use hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
pub use mock::MockHypervisor;
pub use qemu::{QemuConfig, QemuHypervisor};
pub use storage::{Capacity, DirStorage, SourceFormat, Storage, VolumeInfo};
```

`crates/opl-driver/src/qemu.rs`에 테스트만 먼저 넣는다. 특히 다음 두 테스트는 실제로 발생했던 버그를 고정한다:
- `dropping_the_driver_releases_qmp_connections`: 모니터 태스크가 VM 맵을 강한 참조(`Arc`)로 쥐면 순환 참조가 생긴다. 그러면 드라이버를 내려도 QMP 연결이 닫히지 않고, 재시작한 opld가 VM에 붙지 못한다.
- `new_keeps_runtime_dir_when_reattach_fails_for_other_reasons`: 재부착 타임아웃을 "죽은 VM"으로 판정하면 **살아 있는 VM의 소켓을 지워** 영영 제어할 수 없게 된다.

```rust
#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use opl_qemu::{Accel, Arch};
    use opl_types::vm::{CpuModel, CpuSpec, MachineType};

    use super::*;

    fn config(root: &Path, qemu_binary: PathBuf) -> QemuConfig {
        let template = root.join("vars-template.fd");
        std::fs::write(&template, b"vars").unwrap();
        QemuConfig {
            host: HostProfile {
                arch: Arch::Aarch64,
                accel: Accel::Tcg,
                qemu_binary,
                uefi_code: root.join("code.fd"),
                uefi_vars_template: template,
            },
            run_dir: root.join("run"),
            state_dir: root.join("state"),
        }
    }

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    fn short_tempdir() -> tempfile::TempDir {
        // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
        tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[tokio::test]
    async fn start_failure_reports_qemu_stderr_and_cleans_run_dir() {
        let tmp = short_tempdir();
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'qemu: something broke' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();
        let vm = spec();

        let err = hv.start(&vm).await.unwrap_err();
        match &err {
            DriverError::StartFailed(msg) => assert_eq!(msg, "qemu: something broke"),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!err.is_transient());
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        // UEFI 변수 파일은 state_dir에 복사되어 남는다.
        let vars = tmp
            .path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd");
        assert_eq!(std::fs::read(vars).unwrap(), b"vars");
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn new_removes_stale_runtime_dirs_and_ignores_foreign_entries() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let stale = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("qmp.sock"), b"not a socket").unwrap();
        let foreign = cfg.run_dir.join("vm").join("not-a-uuid");
        std::fs::create_dir_all(&foreign).unwrap();

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!stale.exists());
        assert!(foreign.exists());
        assert!(hv.list().await.unwrap().is_empty());
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// 클라이언트가 연결을 닫으면 `closed`로 알린다.
    async fn fake_qmp(dir: &Path) -> tokio::sync::oneshot::Receiver<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("qemu.pid"), "4242\n").unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                let ret = match cmd["execute"].as_str() {
                    Some("query-status") => {
                        serde_json::json!({ "running": true, "status": "running" })
                    }
                    _ => serde_json::json!({}),
                };
                let reply = serde_json::json!({ "return": ret, "id": cmd["id"] });
                write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
            let _ = closed_tx.send(());
        });
        closed_rx
    }

    #[tokio::test]
    async fn dropping_the_driver_releases_qmp_connections() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        let closed = fake_qmp(&dir).await;

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(hv.list().await.unwrap().len(), 1);
        drop(hv);

        // QEMU는 QMP 클라이언트를 하나만 받으므로, 연결이 남아 있으면 재시작한 opld가 붙을 수 없다.
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("QMP connection was not closed after dropping the driver")
            .unwrap();
        assert!(dir.exists(), "runtime dir of a live VM must not be removed");
    }

    #[tokio::test]
    async fn new_keeps_runtime_dir_when_reattach_fails_for_other_reasons() {
        use tokio::io::AsyncWriteExt;

        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // 무언가가 소켓을 열고 있지만 QMP가 아니다. 살아 있는 프로세스일 수 있으므로 지우면 안 된다.
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.list().await.unwrap().is_empty());
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn stop_and_console_on_unknown_vm() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let id = Uuid::now_v7();
        hv.stop(id, StopMode::Force).await.unwrap();
        hv.stop(id, StopMode::Acpi).await.unwrap();
        assert!(matches!(
            hv.console_endpoint(id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn incompatible_spec_is_invalid_not_start_failure() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let vm = VmRuntimeSpec {
            machine: MachineType::Q35,
            ..spec()
        };
        assert!(matches!(
            hv.start(&vm).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-driver qemu`
Expected: 컴파일 실패 — `cannot find type 'QemuHypervisor'`

- [ ] **Step 3: 구현 작성**

`crates/opl-driver/src/qemu.rs` 맨 위에 추가한다. 핵심 결정:
- **`-daemonize`:** QEMU가 스스로 세션에서 분리된다. 부모 프로세스는 초기화가 끝난 뒤에 종료하므로, 종료 코드와 stderr로 기동 실패를 바로 알 수 있다. `unsafe`(`pre_exec`/`setsid`)도 필요 없다.
- **모니터는 맵을 `Weak`로 참조:** 맵 → 핸들 → 모니터 태스크 → 맵으로 이어지는 순환을 끊는다. 핸들의 `Drop`은 모니터를 abort한다. 그래서 드라이버를 내릴 때 모니터가 "연결 끊김 = 크래시"로 오판해 살아 있는 VM의 파일을 지우는 일이 없다.
- **등록 해제 → 이벤트 순서:** 이벤트를 받은 reconciler가 곧바로 `start`를 불렀을 때, 아직 남아 있는 핸들 때문에 "이미 실행 중"으로 판단하고 아무것도 하지 않는 것을 막는다.
- **재부착 실패 시 삭제 조건:** 소켓 연결 자체가 거부될 때(`QmpError::Io`)만 지운다.
- **UEFI 변수:** 휘발성인 run_dir이 아니라 state_dir에 둔다. 재부팅해도 부트 항목이 유지된다.
- **std `Mutex`:** 맵 잠금을 `.await` 너머로 들고 있지 않는다. 같은 VM에 대한 호출을 직렬화하는 것은 reconciler의 책임이다(트레이트 doc 참고).

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{HostProfile, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::Firmware;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub host: HostProfile,
    /// 소켓과 pidfile을 두는 휘발성 디렉터리 (Linux: `/run/opl`)
    pub run_dir: PathBuf,
    /// UEFI 변수처럼 재부팅 후에도 남아야 하는 VM별 파일 (Linux: `/var/lib/opl`)
    pub state_dir: PathBuf,
}

/// QEMU 프로세스를 `-daemonize`로 띄워 opld와 분리한다. opld가 재시작해도 VM은 계속 돈다.
pub struct QemuHypervisor {
    config: QemuConfig,
    vms: Arc<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
}

struct VmHandle {
    qmp: QmpClient,
    pid: u32,
    paths: RuntimePaths,
    monitor: JoinHandle<()>,
}

impl Drop for VmHandle {
    fn drop(&mut self) {
        // 드라이버가 내려갈 때 모니터가 "연결 끊김 = 크래시"로 오판해 런타임 파일을 지우지 않게 한다.
        self.monitor.abort();
    }
}

impl QemuHypervisor {
    /// 런타임 디렉터리를 훑어 살아 있는 VM에 다시 연결하고, 죽은 VM의 흔적은 지운다.
    pub async fn new(config: QemuConfig) -> Result<Self, DriverError> {
        let vm_run_dir = config.run_dir.join("vm");
        std::fs::create_dir_all(&vm_run_dir)?;
        std::fs::create_dir_all(config.state_dir.join("vm"))?;
        let hv = Self {
            config,
            vms: Arc::new(Mutex::new(HashMap::new())),
            events: broadcast::channel(64).0,
        };
        for entry in std::fs::read_dir(&vm_run_dir)? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = hv.paths(id)?;
            match tokio::time::timeout(ATTACH_TIMEOUT, hv.attach(id, paths.clone())).await {
                Ok(Ok(())) => tracing::info!(%id, "reattached to running VM"),
                // 소켓에 연결조차 안 되면 QEMU는 이미 없다. 이때만 흔적을 지운다.
                Ok(Err(DriverError::Qmp(QmpError::Io(e)))) => {
                    tracing::warn!(%id, error = %e, "removing stale runtime dir");
                    std::fs::remove_dir_all(&paths.dir)?;
                }
                // 살아 있는 VM일 수 있으므로 아무것도 지우지 않는다.
                Ok(Err(e)) => tracing::error!(%id, error = %e, "could not reattach to VM"),
                Err(_) => tracing::error!(%id, "timed out reattaching to VM"),
            }
        }
        Ok(hv)
    }

    fn paths(&self, id: Uuid) -> Result<RuntimePaths, DriverError> {
        let mut paths = RuntimePaths::new(&self.config.run_dir, id)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        paths.uefi_vars = self.state_dir(id).join("uefi-vars.fd");
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        self.config.state_dir.join("vm").join(id.to_string())
    }

    fn handle(&self, id: Uuid) -> Option<Arc<VmHandle>> {
        self.vms.lock().unwrap().get(&id).cloned()
    }

    /// QMP에 연결하고 모니터 태스크를 붙여 등록한다.
    async fn attach(&self, id: Uuid, paths: RuntimePaths) -> Result<(), DriverError> {
        let qmp = QmpClient::connect(&paths.qmp_socket).await?;
        let pid = read_pid(&paths.pidfile)?;
        let monitor = tokio::spawn(monitor(
            id,
            qmp.subscribe(),
            Arc::downgrade(&self.vms),
            self.events.clone(),
            paths.dir.clone(),
        ));
        let handle = Arc::new(VmHandle {
            qmp,
            pid,
            paths,
            monitor,
        });
        self.vms.lock().unwrap().insert(id, handle);
        Ok(())
    }

    async fn launch(&self, vm: &VmRuntimeSpec, paths: &RuntimePaths) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            std::fs::copy(&self.config.host.uefi_vars_template, &paths.uefi_vars)?;
        }
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            remove_if_exists(sock)?;
        }

        let mut args = build_cmdline(vm, &self.config.host, paths)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        // -daemonize: 초기화가 끝나면 부모 프로세스가 종료 코드로 성공 여부를 알려 주고, 자식은 세션에서 분리된다.
        let output = tokio::time::timeout(
            START_TIMEOUT,
            tokio::process::Command::new(&self.config.host.qemu_binary)
                .args(&args)
                .stdin(Stdio::null())
                .output(),
        )
        .await
        .map_err(|_| DriverError::Timeout("QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
        }
        self.attach(vm.id, paths.clone()).await?;
        let handle = self.handle(vm.id).ok_or(DriverError::NotRunning(vm.id))?;
        if let Err(e) = handle.qmp.cont().await {
            let _ = handle.qmp.quit().await;
            return Err(e.into());
        }
        Ok(())
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        if self.handle(vm.id).is_some() {
            return Ok(());
        }
        let paths = self.paths(vm.id)?;
        if let Err(e) = self.launch(vm, &paths).await {
            if self.handle(vm.id).is_none() {
                let _ = std::fs::remove_dir_all(&paths.dir);
            }
            return Err(e);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        match mode {
            StopMode::Acpi => handle.qmp.system_powerdown().await?,
            StopMode::Force => {
                let mut rx = self.events.subscribe();
                handle.qmp.quit().await?;
                drop(handle);
                tokio::time::timeout(FORCE_STOP_TIMEOUT, async {
                    while self.handle(id).is_some() {
                        match rx.recv().await {
                            Ok(ev) if ev.id == id => break,
                            Ok(_) | Err(RecvError::Lagged(_)) => {}
                            Err(RecvError::Closed) => break,
                        }
                    }
                })
                .await
                .map_err(|_| DriverError::Timeout("QEMU to exit"))?;
            }
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(None);
        };
        let status = match handle.qmp.query_status().await {
            Ok(RunState::Running) => RuntimeStatus::Running,
            Ok(RunState::Prelaunch | RunState::Paused) => RuntimeStatus::Paused,
            Ok(RunState::Shutdown) => RuntimeStatus::Other("shutdown".into()),
            Ok(RunState::Other(s)) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Err(QmpError::Disconnected) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let ids: Vec<Uuid> = self.vms.lock().unwrap().keys().copied().collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(st) = self.inspect(id).await? {
                out.push(st);
            }
        }
        Ok(out)
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        self.handle(id)
            .map(|h| h.paths.vnc_socket.clone())
            .ok_or(DriverError::NotRunning(id))
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// QMP 연결이 끊길 때까지 이벤트를 지켜보다가, 종료 종류를 판정해 알리고 런타임 파일을 정리한다.
async fn monitor(
    id: Uuid,
    mut rx: broadcast::Receiver<QmpEvent>,
    // Weak: 맵이 핸들을, 핸들이 이 태스크를 소유하므로 강한 참조를 쥐면 순환이 생겨
    // 드라이버를 내려도 QMP 연결이 닫히지 않는다.
    vms: Weak<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
    run_dir: PathBuf,
) {
    let mut guest_initiated = None;
    loop {
        match rx.recv().await {
            Ok(ev) if ev.event == "SHUTDOWN" => {
                guest_initiated = Some(ev.data["guest"].as_bool().unwrap_or(false));
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
    let kind = match guest_initiated {
        Some(guest_initiated) => EventKind::Stopped { guest_initiated },
        None => EventKind::Crashed,
    };
    if let Err(e) = std::fs::remove_dir_all(&run_dir) {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    if let Some(vms) = vms.upgrade() {
        let handle = vms.lock().unwrap().remove(&id);
        drop(handle);
    }
    let _ = events.send(HypervisorEvent { id, kind });
}

fn read_pid(pidfile: &Path) -> Result<u32, DriverError> {
    let text = std::fs::read_to_string(pidfile)?;
    text.trim().parse().map_err(|_| {
        DriverError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad pidfile {pidfile:?}: {text:?}"),
        ))
    })
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-driver`
Expected: `test result: ok. 15 passed`

- [ ] **Step 5: 회귀 테스트가 버그를 잡는지 확인**

`attach()`의 `Arc::downgrade(&self.vms),` 줄을 잠시 `{ std::mem::forget(self.vms.clone()); Arc::downgrade(&self.vms) },`로 바꿔(맵 누수 = 순환 참조와 같은 증상) 실행한다.

Run: `cargo test -p opl-driver --lib dropping`
Expected: FAIL — `QMP connection was not closed after dropping the driver`

원래대로 되돌리고 다시 실행해 PASS를 확인한다. `git diff`에 남은 변경이 없어야 한다.

- [ ] **Step 6: Commit**

```bash
git add crates/opl-driver
git commit -m "feat(driver): QEMU를 분리 실행하고 재부착·크래시 감지하는 QemuHypervisor 추가"
```

---

### Task 7: 통합 테스트 (실제 qemu-img, 실제 QEMU)

**Files:**
- Create: `crates/opl-driver/tests/dir_storage.rs`, `crates/opl-driver/tests/qemu_driver.rs`

- [ ] **Step 1: 스토리지 통합 테스트 작성**

`crates/opl-driver/tests/dir_storage.rs`:

```rust
//! 실제 qemu-img를 쓰는 스토리지 통합 테스트. `cargo test -p opl-driver --features storage-it`
#![cfg(feature = "storage-it")]

use std::path::Path;

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage};
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn pool(dir: &Path) -> StoragePoolSpec {
    StoragePoolSpec {
        backend: PoolBackend::Dir {
            path: dir.to_path_buf(),
        },
        nodes: vec![],
        shared: false,
    }
}

fn qemu_img(args: &[&str]) {
    let status = std::process::Command::new("qemu-img")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

#[tokio::test]
async fn create_inspect_and_delete_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();

    let path = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(path, DirStorage::volume_path(&pool, id, ImageFormat::Qcow2));
    let info = storage
        .volume_info(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(
        info.allocated_bytes < 64 * MIB,
        "qcow2 should be thin: {info:?}"
    );

    let again = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(matches!(again, Err(DriverError::VolumeExists(_))));

    storage
        .delete_volume(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert!(!path.exists());
    let gone = storage.volume_info(&pool, id, ImageFormat::Qcow2).await;
    assert!(matches!(gone, Err(DriverError::VolumeNotFound(_))));
}

#[tokio::test]
async fn imports_vmdk_preserving_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let pool_dir = tmp.path().join("pool");
    std::fs::create_dir(&pool_dir).unwrap();
    let pool = pool(&pool_dir);

    // VMware에서 가져온 디스크를 흉내 낸다: 알려진 패턴의 raw → vmdk
    let pattern: Vec<u8> = (0..MIB as usize).map(|i| (i % 251) as u8).collect();
    let raw = tmp.path().join("src.raw");
    std::fs::write(&raw, &pattern).unwrap();
    let vmdk = tmp.path().join("src.vmdk");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw.to_str().unwrap(),
        vmdk.to_str().unwrap(),
    ]);

    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, ImageFormat::Qcow2)
        .await
        .unwrap();

    let back = tmp.path().join("back.raw");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "qcow2",
        "-O",
        "raw",
        path.to_str().unwrap(),
        back.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read(&back).unwrap(), pattern);
}

#[tokio::test]
async fn failed_import_leaves_no_partial_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();

    let err = storage
        .import_volume(
            &pool,
            id,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            ImageFormat::Qcow2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(!DirStorage::volume_path(&pool, id, ImageFormat::Qcow2).exists());
}
```

- [ ] **Step 2: 스토리지 통합 테스트 실행**

Run: `cargo test -p opl-driver --features storage-it --test dir_storage`
Expected: `test result: ok. 3 passed`

- [ ] **Step 3: QEMU 드라이버 통합 테스트 작성**

이 테스트는 설계 문서의 불변식 2번("opld를 재시작해도 VM은 중단되지 않고 다시 연결된다")을 드라이버 수준에서 고정한다.

`crates/opl-driver/tests/qemu_driver.rs`:

```rust
//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::Path;
use std::time::Duration;

use opl_driver::{
    EventKind, Hypervisor, HypervisorEvent, QemuConfig, QemuHypervisor, RuntimeStatus, StopMode,
};
use opl_qemu::HostProfile;
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};
use tokio::sync::broadcast;
use uuid::Uuid;

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::homebrew_hvf(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
    }
}

fn spec() -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "it-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: MachineType::Virt,
        disks: vec![],
        nics: vec![],
    }
}

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

async fn next_event_for(rx: &mut broadcast::Receiver<HypervisorEvent>, id: Uuid) -> EventKind {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = rx.recv().await.unwrap();
            if ev.id == id {
                return ev.kind;
            }
        }
    })
    .await
    .expect("no event within 10s")
}

#[tokio::test]
async fn start_inspect_and_force_stop() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let mut rx = hv.subscribe();
    let vm = spec();

    hv.start(&vm).await.unwrap();
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    hv.start(&vm).await.unwrap(); // 이미 실행 중이면 그대로 성공

    let st = hv.inspect(vm.id).await.unwrap().expect("running");
    assert_eq!(st.status, RuntimeStatus::Running);
    assert!(st.pid > 0);
    assert_eq!(hv.list().await.unwrap().len(), 1);
    assert!(hv.console_endpoint(vm.id).await.unwrap().exists());

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(
        next_event_for(&mut rx, vm.id).await,
        EventKind::Stopped {
            guest_initiated: false
        }
    );
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
    assert!(
        tmp.path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd")
            .exists()
    );
}

#[tokio::test]
async fn reattaches_after_driver_restart() {
    let tmp = short_tempdir();
    let vm = spec();
    let pid = {
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap().pid
        // 드라이버(= opld)가 내려간다. QEMU는 분리되어 있으므로 계속 돌아야 한다.
    };

    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let st = hv
        .inspect(vm.id)
        .await
        .unwrap()
        .expect("VM should survive driver restart");
    assert_eq!(st.pid, pid);
    assert_eq!(st.status, RuntimeStatus::Running);

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
}

#[tokio::test]
async fn reports_crash_when_qemu_is_killed() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let mut rx = hv.subscribe();

    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Crashed);
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
}
```

- [ ] **Step 4: QEMU 드라이버 통합 테스트 실행 (3회 반복)**

```bash
for i in 1 2 3; do cargo test -p opl-driver --features qemu-it --test qemu_driver -q || break; done
pgrep -fl qemu-system || echo "no qemu left"
```

Expected: 매번 `test result: ok. 3 passed`, 마지막 줄 `no qemu left`

실패 시 확인할 것:
- `reattaches_after_driver_restart`가 약 5초 걸린 뒤 실패 → 이전 드라이버의 QMP 연결이 닫히지 않은 것이다. Task 6 Step 5의 순환 참조를 의심한다.
- 테스트가 중간에 실패하면 QEMU가 남을 수 있다. `pkill -f "guest=it-vm"`으로 정리한다.

- [ ] **Step 5: Commit**

```bash
git add crates/opl-driver/tests
git commit -m "test(driver): qemu-img 스토리지와 실제 QEMU 드라이버 통합 테스트 추가"
```

---

### Task 8: CI, 설계 문서, README 갱신

**Files:**
- Modify: `.github/workflows/ci.yml`, `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`, `README.md`

- [ ] **Step 1: CI에 Linux 스토리지 통합 테스트 추가**

`.github/workflows/ci.yml` 전체가 다음과 같아지도록 수정한다(`check` 작업 끝에 qemu-utils 설치와 storage-it 실행 추가):

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets --all-features -- -D warnings
      - run: cargo test --workspace
      - name: Storage integration tests (qemu-img)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y qemu-utils
          cargo test -p opl-driver --features storage-it --test dir_storage

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
```

- [ ] **Step 2: 설계 문서 5.2절 교체**

설계 문서에서 `### 5.2 드라이버 트레이트`부터 `### 5.3 reconciler 규칙` 직전까지를 다음으로 바꾼다. 원래 설계와 달라진 점은 세 가지다. `start`가 핸들 대신 `()`를 반환하고, 콘솔이 VNC 유닉스 소켓 경로로 바뀌었고, 이벤트가 broadcast receiver로 전달된다. 그리고 두 QEMU 구현이 `HostProfile`로 구분되는 구현 하나로 합쳐졌다.

````markdown
### 5.2 드라이버 트레이트

```rust
#[async_trait]
trait Hypervisor: Send + Sync {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;       // 이미 실행 중이면 성공
    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError>;  // Acpi | Force, 실행 중이 아니면 성공
    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError>;
    async fn list(&self) -> Result<Vec<ActualState>, DriverError>;
    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError>; // VNC 유닉스 소켓
    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent>;                // Started | Stopped | Crashed
}
```

| 구현 | 플랫폼 | 용도 |
|---|---|---|
| `QemuHypervisor` | Linux(KVM), macOS(hvf) — `HostProfile`로 구분 | 제품, 개발·통합 테스트 |
| `MockHypervisor` | 전체 | 단위·컴포넌트 테스트, 장애 주입(`fail_next_start`, `crash`, `guest_shutdown`) |

QEMU 커맨드라인 생성은 순수 함수 `fn build_cmdline(&VmRuntimeSpec, &HostProfile, &RuntimePaths) -> Result<Vec<String>, CmdlineError>`로 만들고, `insta` 스냅샷으로 검증한다.
````

- [ ] **Step 3: 설계 문서 5.4절 교체**

`### 5.4 opld 재시작과 크래시 복구`부터 `### 5.5 오류 처리` 직전까지를 다음으로 바꾼다:

```markdown
### 5.4 opld 재시작과 크래시 복구

- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서 cgroup 자원 제어를 위해 `systemd-run --scope`로 감싸는 것은 M7에서 추가한다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
- `opld`가 시작되면 런타임 디렉터리를 훑어 살아 있는 QEMU에 QMP로 다시 연결한다. 그 뒤 Raft의 원하는 상태와 비교해 조정한다.
- 소켓 연결 자체가 거부될 때만 죽은 VM으로 보고 런타임 디렉터리를 지운다. 그 밖의 오류나 타임아웃에서는 살아 있는 VM일 수 있으므로 아무것도 지우지 않고 오류를 기록한다.
- 이 설계 덕분에 `opld`를 업그레이드하거나 재시작해도 실행 중인 VM은 중단되지 않는다.
```

- [ ] **Step 4: README 갱신**

`README.md` 전체를 다음으로 바꾼다:

````markdown
# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M2)

## 개발

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: apt install qemu-utils)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (Apple Silicon)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
```

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
- [계획 2: 하이퍼바이저·스토리지 드라이버](docs/superpowers/plans/2026-10-20-plan2-opl-driver.md)
````

- [ ] **Step 5: 전체 검사**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

Expected: 모두 통과한다. 테스트는 `opl-types` 15개, `opl-qemu` 17개, `opl-driver` 15개다.

- [ ] **Step 6: Commit**

```bash
git add .github docs README.md
git commit -m "docs: 드라이버 설계를 구현에 맞추고 CI에 Linux 스토리지 테스트 추가"
```

푸시는 GitHub 토큰에 `workflow` 권한이 생긴 뒤에 한다(이 계획은 `.github/workflows/ci.yml`을 수정한다).

---

## 완료 기준 (M2)

- [ ] `cargo test --workspace`가 통과한다 (macOS, Linux CI).
- [ ] `storage-it`: VMDK를 가져온 볼륨의 내용이 원본과 바이트 단위로 같다. 가져오기에 실패하면 파일이 남지 않는다.
- [ ] `qemu-it`: 기동 → 상태 조회 → 강제 종료, **드라이버 재시작 후 같은 pid의 VM에 재부착**, `kill -9` 시 `Crashed` 이벤트가 나온다.
- [ ] 테스트가 끝난 뒤 남은 QEMU 프로세스가 없다.

## 다음 계획(계획 3: `opl-cluster`)으로 넘길 사항

- 저장 형식 호환성 규칙(설계 3.1)과 `schema_version` 위치를 결정한다.
- 계획 4(reconciler)가 쓸 수 있도록, 리소스 변경 watch 스트림이 "변경된 리소스 + revision"을 내보내야 한다.

## 실행 후 기록 (2026-10-20)

코드 리뷰(실제 QEMU 11.1.2와 qemu-img로 재현)를 거쳐 계획의 코드에서 아래가 바뀌었다. 저장소의 코드가 기준이다.

**QemuHypervisor**
- QEMU 생존 판정을 pidfile의 fcntl 잠금(`F_GETLK`, 잠금 소유자 pid 확인)으로 바꿨다. 계획의 "QMP 연결 거부 = 죽은 VM" 판정은 틀렸다. QEMU가 잠시 멈추거나(SIGSTOP, I/O 정지) macOS의 QMP 대기열이 차면 살아 있는 VM의 런타임 디렉터리를 지워 영영 제어할 수 없게 만들었다.
- `Liveness::{Alive(owner), Dead, Unknown}`. 잠금 소유자가 맞을 때만 SIGKILL하고, 판정 불가면 아무것도 지우거나 죽이지 않는다.
- 살아 있지만 등록되지 않은 VM은 `inspect`/`list`에 `Other("unresponsive")`로 드러나고, `start()`는 재부착을 시도하며, `stop(Force)`는 SIGKILL 후 정리한다.
- `start()`는 `-S` 상태로 남은 VM에 `cont`를 보낸다. 모니터 등록과 `Started` 이벤트는 맵 잠금 안에서 처리한다(등록 전에 모니터가 끝나 죽은 핸들이 남던 경합 제거).
- 모든 QMP 호출에 타임아웃을 두었다. `stop(Force)`는 `quit`이 듣지 않으면 SIGKILL한다. 이렇게 죽은 VM은 `Crashed`로 보고된다.
- SHUTDOWN 이벤트를 받은 뒤 QEMU가 늦게 끝나도 `Stopped` 이벤트는 바로 보낸다.
- macOS: `-daemonize`의 fork가 Objective-C 런타임 검사로 크래시한다(VNC 장치가 없으면 40회 중 40회). `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES`로 실행한다.

**MockHypervisor / DriverError**
- Mock의 `stop(Acpi)`는 아무것도 하지 않는다(실제 QEMU처럼 요청만 보냄). 실제 종료는 `guest_shutdown()`으로 일으킨다.
- `fail_next_start`는 이미 실행 중인 VM에는 적용되지 않는다.
- `Io`의 `NotFound`/`PermissionDenied`/`InvalidInput`/`Unsupported`는 영구 오류다.

**DirStorage**
- `qemu-img create`는 기존 파일을 경고 없이 덮어쓴다. 고유한 임시 파일(`.{name}.{uuid}.part`)에 만든 뒤 `hard_link`로 게시하므로, 이미 있는 볼륨은 절대 덮어쓰지 않는다(풀은 hard link를 지원하는 파일시스템이어야 한다).
- `kill_on_drop`을 쓴다. 상대 경로(`json:`, `nbd:` 같은 프로토콜 문법)는 거부한다. `qemu-img info` 출력에 필드가 없으면 `ToolFailed`를 돌려준다.

테스트: `opl-driver` 단위 30개(python3 필요), storage-it 3개, qemu-it 6개.

## 다음 계획으로 넘기는 사항

계획 4 (reconciler):
- `Stopped{guest_initiated: true}`는 opld가 요청한 ACPI 종료에서도 나온다. 이벤트만 보고 spec을 바꾸지 말고 현재 원하는 상태와 비교한다.
- 원하는 상태가 `stopped`일 때 오는 `Crashed`(강제 종료의 결과)는 크래시 백오프 집계에서 뺀다.
- `Stopped` 직후 잠시 `unresponsive`로 보이는 VM(종료 중인 QEMU)을 바로 `stop(Force)`하지 않는다. 유예한 뒤 VM별 큐 안에서 `inspect`로 다시 확인하고 행동한다.
- `list()`는 VM을 순서대로 조회하므로 응답 없는 VM마다 최대 5초씩 늘어난다. 필요하면 병렬화한다.
- ACPI 종료 타임아웃 후 `Force`로 승격하는 규칙을 구현한다.

계획 5 (API):
- 업로드 이미지는 단일 파일 형식인지 검증한다(VMDK descriptor가 임의의 호스트 파일을 참조할 수 있다).
- `MacAddr::generate()`의 충돌을 클러스터 안에서 확인한다. 볼륨 크기 0은 거부한다. 가져온 볼륨의 `size_bytes`를 정의한다.

나중에 처리:
- opld가 크래시하면 남는 `.part` 임시 파일과, 늦게 종료된 VM의 런타임 디렉터리를 시작할 때 정리한다.
