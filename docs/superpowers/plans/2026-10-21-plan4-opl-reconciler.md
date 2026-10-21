# opLite 계획 4: reconciler (M4) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `opl-reconciler` 크레이트를 만든다. 노드에 배정된 VM의 원하는 상태(Raft에 기록된 spec)와 실제 상태(하이퍼바이저)를 맞추는 조정 루프다. opld가 죽었다 다시 떠도 VM이 계속 돌고 다시 제어되는 것을 실제 QEMU 테스트로 고정한다(M4).

**Architecture:** 노드마다 디스패처 하나가 저장소 watch, 하이퍼바이저 이벤트, 주기적 재동기화를 받아 VM별 작업자 태스크에 나눠 준다. 같은 VM에 대한 드라이버 호출은 그 VM의 작업자에서만 일어난다(설계 10절 불변식 3). 작업자는 깨어날 때마다 로컬 저장소에서 VM을 다시 읽고 실제 상태를 조회해 한 걸음씩 맞추므로(level-triggered) 같은 트리거를 여러 번 받아도 안전하다. 이벤트는 "게스트가 스스로 꺼졌다", "크래시였다"처럼 실제 상태만 봐서는 알 수 없는 사실을 알릴 때만 쓴다. 클러스터 쓰기가 실패하면(과반수 없음) VM은 그대로 두고 status를 다시 계산해 계속 다시 쓴다.

**Tech Stack:** Rust 2024, tokio, `opl-cluster`(계획 3), `opl-driver`(계획 2), `opl-types`

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (5.3, 5.4, 5.5, 10, 11절)
**선행 계획:** 계획 1~3 (완료)

---

## 범위

| 포함 | 제외 (담당 계획) |
|---|---|
| VM별 작업자, 디스패처(watch + 하이퍼바이저 이벤트 + 30초 재동기화) | `opld` 바이너리, 설정 파일, 실제 `kill -9` 테스트 — 계획 5 |
| 시작, ACPI 종료 후 강제 종료, 게스트 내부 종료 반영 | 볼륨 파일 생성·삭제(Storage 호출) — 계획 5의 API Task |
| 크래시 재시작 지수 백오프와 포기, 기동 오류 분류 | 스케줄러(`spec.node` 고르기), MAC 자동 할당 — 계획 5 |
| Volume 이름 → 파일 경로, `user` 네트워크 해석 | `Network` 리소스, bridge — 계획 7 |
| 과반수가 없을 때 status 재시도, VM 유지 | 팔로워의 쓰기를 리더로 전달 — 계획 6 |
| 실제 QEMU로 opld 재시작 후 재연결 확인 | `stop?mode=force`, `reboot` 일회성 명령 — 계획 5 |

## 이전 계획에서 이어받는 사항

계획 2에서:
- `Stopped{guest_initiated: true}`는 opld가 요청한 ACPI 종료에서도 나온다. 이 플래그는 쓰지 않고, 작업자가 ACPI 종료를 요청했는지(`acpi_timeout` 안)로 가린다.
- 원하는 상태가 `stopped`일 때 오는 `Crashed`(강제 종료의 결과)는 실패로 세지 않는다.
- `Stopped` 직후 잠시 `unresponsive`로 보이는 VM을 바로 `stop(Force)`하지 않는다. 작업자는 실행 중이 아닌 상태(`inspect`가 `Some`)를 모두 "돌고 있음"으로 보고, 강제 종료는 `acpi_timeout`이 지난 뒤에만 한다.
- ACPI 종료 타임아웃 후 `Force`로 승격한다.

계획 3에서:
- 시작할 때 `list()`로 읽고 그 revision부터 `watch()`한다. `Expired`, `Lagged`, `Reset`을 받으면 목록을 다시 읽는다.
- status는 `update_status`로 쓴다. 과반수가 없으면 `Timeout`이 나므로 다시 쓴다.
- `Cluster`는 `Clone`이 아니므로 `Arc<Cluster>`로 나눠 쓴다.

## 설계 결정

- **포기하면 `spec.power`를 `stopped`로 바꾼다.** `restart: never`의 크래시, 영구 기동 오류(펌웨어 없음, 잘못된 Volume 참조 등), 백오프 포기는 모두 원하는 상태를 꺼짐으로 바꾸고 status를 `Failed`와 오류 메시지로 남긴다. 원하는 상태를 `running`으로 두면 30초 재동기화마다 다시 시작하게 되고, 메모리에만 둔 "포기" 표시는 opld 재시작으로 사라진다. 설계 5.3절이 게스트 내부 종료에 대해 이미 같은 규칙을 쓰므로 그것을 넓힌다. 사용자가 다시 시작하면(`power: running`) 실패 횟수를 처음부터 센다.
- **배정되지 않은 VM은 끄지 않는다.** 이 노드에서 도는데 저장소에서 삭제되었거나 다른 노드에 배정된 VM은 경고만 남긴다. 저장소가 비거나(새로 합류한 노드) 뒤처져도 VM을 죽이지 않기 위해서다. 실행 중인 VM의 삭제와 노드 변경은 API(계획 5)가 막는다.
- **종료 이벤트를 잠시 기다린다.** 방금까지 돌던 VM이 사라져 보이는데 종료 이벤트가 아직 안 왔으면 1초 기다린다. 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 되기 때문이다. 기다려도 오지 않으면(이벤트 유실, opld가 내려가 있던 동안의 종료) 원하는 상태대로 다시 켠다.
- **status 버퍼는 메모리에만 둔다.** 쓰기에 실패하면 `retry_interval`(기본 2초)마다 실제 상태에서 status를 다시 계산해 쓴다. 디스크에 버퍼를 두지 않아도 opld가 재시작하면 실제 상태에서 다시 계산하므로 잃는 정보가 없다.
- **네트워크는 `user`만 해석한다.** `Network` 리소스(계획 7)가 생기기 전까지 NIC의 `network`가 `"user"`가 아니면 영구 오류다. MAC이 없는 NIC도 영구 오류다(API가 생성할 때 채운다).
- **가짜 시계 대신 짧은 시간 설정을 쓴다.** 테스트는 `acpi_timeout` 300ms, 백오프 50ms처럼 줄인 `ReconcilerConfig`로 돌린다. tokio의 멈춘 시계는 openraft 타이머와 redb의 블로킹 호출과 섞이면 동작을 예측하기 어렵다. 재동기화 주기는 1시간으로 꺼 두어 이벤트만으로 동작하는지 확인한다.
- **M4의 "opld kill -9"는 프로세스 안에서 흉내 낸다.** `opld` 바이너리는 계획 5에서 생긴다. 이 계획은 reconciler, 드라이버, 클러스터 저장소를 모두 내렸다가 새로 여는 것으로 확인한다. QEMU는 `-daemonize`로 떠 있어 opld 프로세스가 `kill -9`로 죽어도 결과는 같다.

## 파일 구조

```
Cargo.toml                                   # 수정: opl-driver 워크스페이스 의존성
README.md                                    # 수정: 상태 M4, 테스트 명령, 문서 링크
docs/superpowers/specs/...-design.md         # 수정: 5.3절 규칙, 10절 테스트 도구
crates/opl-reconciler/
├── Cargo.toml
├── src/
│   ├── lib.rs                               # 모듈 선언 + 재노출
│   ├── config.rs                            # ReconcilerConfig, RestartBackoff
│   ├── backoff.rs                           # FailureTracker: 지수 백오프와 포기 판정
│   ├── resolve.rs                           # VmSpec → VmRuntimeSpec (Volume 경로, user 네트워크)
│   ├── worker.rs                            # VM 하나를 맡는 작업자: 조정 규칙
│   └── reconciler.rs                        # Reconciler, 디스패처: watch·이벤트·재동기화 분배
└── tests/
    ├── common/mod.rs                        # 단일·다중 노드 클러스터 + Mock 하이퍼바이저 하네스
    ├── resolve.rs                           # 참조 해석
    ├── reconcile.rs                         # 조정 규칙 (Mock)
    ├── no_quorum.rs                         # 과반수 상실 중 VM 유지와 status 재기록
    └── qemu_restart.rs                      # 실제 QEMU: opld 재시작 후 재연결 (qemu-it)
```

---

### Task 1: 크레이트 뼈대, 설정, 백오프

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/opl-reconciler/Cargo.toml`, `crates/opl-reconciler/src/lib.rs`, `crates/opl-reconciler/src/config.rs`, `crates/opl-reconciler/src/backoff.rs`

- [ ] **Step 1: 워크스페이스 의존성 추가**

루트 `Cargo.toml`의 `[workspace.dependencies]`에서 `opl-cluster = ...` 줄 바로 아래에 추가한다:

```toml
opl-driver = { path = "crates/opl-driver" }
```

- [ ] **Step 2: 크레이트 파일 작성**

`crates/opl-reconciler/Cargo.toml`:

```toml
[package]
name = "opl-reconciler"
description = "Reconciliation loop that drives VMs on a node toward their desired state"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# 실제 QEMU로 opld 재시작 후 재연결을 확인하는 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []

[dependencies]
opl-cluster.workspace = true
opl-driver.workspace = true
opl-types.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
uuid.workspace = true

[dev-dependencies]
openraft.workspace = true
opl-qemu.workspace = true
redb.workspace = true
tempfile.workspace = true
```

`crates/opl-reconciler/src/lib.rs` (Task 2, 3에서 모듈을 더한다):

```rust
//! 원하는 상태(Raft에 기록된 spec)와 실제 상태(하이퍼바이저)를 맞추는 조정 루프.
//!
//! 노드마다 하나씩 돈다. 이 노드에 배정된 VM마다 작업자 태스크를 하나 두어 같은 VM에 대한
//! 드라이버 호출을 직렬화하고, 서로 다른 VM은 병렬로 처리한다.

mod backoff;
mod config;

pub use config::{ReconcilerConfig, RestartBackoff};
```

`crates/opl-reconciler/src/config.rs`:

```rust
use std::time::Duration;

use uuid::Uuid;

/// reconciler 설정. 기본값은 설계 문서 5.3절을 따른다.
#[derive(Debug, Clone)]
pub struct ReconcilerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 VM만 다룬다.
    pub node: Uuid,
    /// 이벤트를 놓쳤더라도 원하는 상태와 실제 상태를 다시 맞추는 주기
    pub resync_interval: Duration,
    /// ACPI 종료를 요청한 뒤 강제 종료로 넘어가기까지 기다리는 시간
    pub acpi_timeout: Duration,
    /// 클러스터 쓰기나 드라이버 호출이 실패했을 때 다시 시도하기까지의 간격.
    /// 과반수가 없어 status를 못 쓰는 동안에도 이 간격으로 다시 쓴다.
    pub retry_interval: Duration,
    pub restart: RestartBackoff,
}

/// 크래시나 일시적 기동 실패 뒤 다시 시작하는 규칙.
/// `window` 안에 `max_failures`번 실패하면 포기한다.
#[derive(Debug, Clone)]
pub struct RestartBackoff {
    pub initial: Duration,
    pub max: Duration,
    pub max_failures: usize,
    pub window: Duration,
}

impl ReconcilerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            resync_interval: Duration::from_secs(30),
            acpi_timeout: Duration::from_secs(60),
            retry_interval: Duration::from_secs(2),
            restart: RestartBackoff {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(60),
                max_failures: 5,
                window: Duration::from_secs(600),
            },
        }
    }
}
```

- [ ] **Step 3: 백오프 테스트 작성**

`crates/opl-reconciler/src/backoff.rs`를 테스트만 담아 만든다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> RestartBackoff {
        RestartBackoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(5),
            max_failures: 5,
            window: Duration::from_secs(600),
        }
    }

    #[test]
    fn doubles_up_to_max_then_gives_up() {
        let mut t = FailureTracker::default();
        let now = Instant::now();
        let delays: Vec<_> = (0..4).map(|_| t.record(now, &rule())).collect();
        assert_eq!(
            delays,
            [1, 2, 4, 5].map(|s| Next::RetryAfter(Duration::from_secs(s)))
        );
        assert_eq!(t.record(now, &rule()), Next::GiveUp);
        // 포기하면 기록을 비우므로 다시 처음부터 센다.
        assert_eq!(
            t.record(now, &rule()),
            Next::RetryAfter(Duration::from_secs(1))
        );
    }

    #[test]
    fn forgets_failures_older_than_window() {
        let mut t = FailureTracker::default();
        let start = Instant::now();
        for _ in 0..4 {
            t.record(start, &rule());
        }
        let later = start + Duration::from_secs(600);
        assert_eq!(
            t.record(later, &rule()),
            Next::RetryAfter(Duration::from_secs(1))
        );
    }
}
```

- [ ] **Step 4: 실패 확인**

Run: `cargo test -p opl-reconciler`
Expected: 컴파일 실패 (`FailureTracker`, `Next`가 없음)

- [ ] **Step 5: 구현**

`crates/opl-reconciler/src/backoff.rs`의 맨 위(`#[cfg(test)]` 앞)에 추가한다:

```rust
use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use crate::config::RestartBackoff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    RetryAfter(Duration),
    GiveUp,
}

/// 최근 실패 시각. `window`보다 오래된 실패는 잊는다.
#[derive(Debug, Default)]
pub(crate) struct FailureTracker {
    failures: VecDeque<Instant>,
}

impl FailureTracker {
    /// 실패를 기록하고 다음 행동을 정한다. n번째 실패 뒤에는 `initial * 2^(n-1)`(최대 `max`)만큼
    /// 기다리고, `window` 안에서 `max_failures`번째 실패면 포기한다.
    pub(crate) fn record(&mut self, now: Instant, rule: &RestartBackoff) -> Next {
        while let Some(&first) = self.failures.front() {
            if now.duration_since(first) < rule.window {
                break;
            }
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        let n = self.failures.len();
        if n >= rule.max_failures {
            self.failures.clear();
            return Next::GiveUp;
        }
        let factor = 1u32 << (n - 1).min(16);
        Next::RetryAfter(rule.initial.saturating_mul(factor).min(rule.max))
    }

    pub(crate) fn clear(&mut self) {
        self.failures.clear();
    }
}
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-reconciler`
Expected: 2개 통과. `FailureTracker`를 아직 아무도 쓰지 않아 dead_code 경고가 나는데, Task 3에서 사라진다.

Run: `cargo fmt --all -- --check`
Expected: 출력 없음

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-reconciler
git commit -m "feat(reconciler): opl-reconciler 크레이트, 설정, 재시작 백오프"
```

---

### Task 2: 참조 해석 (VmSpec → VmRuntimeSpec)

드라이버는 이름 참조가 모두 풀린 `VmRuntimeSpec`을 받는다. Volume 이름은 풀 경로 아래 파일로(`DirStorage::volume_path`), NIC의 네트워크 이름 `user`는 `NicBackend::User`로 바꾼다. 테스트에 쓸 하네스(노드 하나짜리 클러스터)도 여기서 만든다.

**Files:**
- Create: `crates/opl-reconciler/tests/common/mod.rs`, `crates/opl-reconciler/tests/resolve.rs`, `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/src/lib.rs`

- [ ] **Step 1: 하네스 작성**

`crates/opl-reconciler/tests/common/mod.rs` (Task 3에서 `Env`를 더한다):

```rust
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
use opl_reconciler::{ReconcilerConfig, RestartBackoff};
use opl_types::resource::Resource;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType, PowerState, Vm, VmSpec};
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

/// 디스크와 NIC 없이 UEFI로 뜨는 작은 aarch64 VM.
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
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power: PowerState::Running,
            restart: Default::default(),
            node: Some(node),
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
```

- [ ] **Step 2: 실패하는 테스트 작성**

`crates/opl-reconciler/tests/resolve.rs`:

```rust
//! VM spec의 이름 참조를 드라이버용 경로와 백엔드로 푸는 규칙.

mod common;

use opl_cluster::memnet::MemRouter;
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{DiskBus, DiskSpec, MacAddr, NicModel, NicSpec};
use tempfile::TempDir;
use uuid::Uuid;

fn pool(name: &str, nodes: Vec<Uuid>) -> StoragePool {
    Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/pools/{name}").into(),
            },
            nodes,
            shared: false,
        },
    )
}

fn volume(name: &str, pool: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

fn disk(volume: &str) -> DiskSpec {
    DiskSpec {
        volume: volume.into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    }
}

fn nic(network: &str, mac: Option<MacAddr>) -> NicSpec {
    NicSpec {
        network: network.into(),
        model: NicModel::Virtio,
        mac,
    }
}

#[tokio::test]
async fn resolves_volumes_to_pool_paths_and_user_network() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let root = volume("root", "local");
    cluster.create(&root).await.unwrap();

    let mac: MacAddr = "52:54:00:00:00:01".parse().unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.nics.push(nic("user", Some(mac)));

    let spec = runtime_spec(&cluster, &vm, node).unwrap();
    assert_eq!(spec.id, vm.id);
    assert_eq!(
        spec.disks,
        vec![RuntimeDisk {
            path: format!("/pools/local/{}.qcow2", root.id).into(),
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }]
    );
    assert_eq!(
        spec.nics,
        vec![RuntimeNic {
            mac,
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }]
    );
}

#[tokio::test]
async fn reports_unresolvable_references() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster
        .create(&pool("elsewhere", vec![Uuid::now_v7()]))
        .await
        .unwrap();
    cluster
        .create(&volume("remote", "elsewhere"))
        .await
        .unwrap();
    cluster
        .create(&volume("orphan", "deleted-pool"))
        .await
        .unwrap();

    let check = |disks: Vec<DiskSpec>, nics: Vec<NicSpec>| {
        let mut vm = common::vm("web", node);
        vm.spec.disks = disks;
        vm.spec.nics = nics;
        runtime_spec(&cluster, &vm, node).unwrap_err()
    };
    assert!(matches!(
        check(vec![disk("nope")], vec![]),
        ResolveError::VolumeNotFound(n) if n == "nope"
    ));
    assert!(matches!(
        check(vec![disk("orphan")], vec![]),
        ResolveError::PoolNotFound(n) if n == "deleted-pool"
    ));
    assert!(matches!(
        check(vec![disk("remote")], vec![]),
        ResolveError::PoolNotOnNode(n) if n == "elsewhere"
    ));
    assert!(matches!(
        check(vec![], vec![nic("lan", Some(MacAddr::generate()))]),
        ResolveError::NetworkNotFound(n) if n == "lan"
    ));
    assert!(matches!(
        check(vec![], vec![nic("user", None)]),
        ResolveError::MissingMac(0)
    ));
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p opl-reconciler --test resolve`
Expected: 컴파일 실패 (`opl_reconciler::resolve`가 없음)

- [ ] **Step 4: 구현**

`crates/opl-reconciler/src/resolve.rs`:

```rust
//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use opl_cluster::{Cluster, ClusterError};
use opl_driver::DirStorage;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
use uuid::Uuid;

/// `Network` 리소스가 생기기 전까지 쓸 수 있는 유일한 네트워크 이름. QEMU user-mode NAT다.
pub const USER_NETWORK: &str = "user";

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("volume {0:?} not found")]
    VolumeNotFound(String),
    #[error("storage pool {0:?} not found")]
    PoolNotFound(String),
    #[error("storage pool {0:?} is not available on this node")]
    PoolNotOnNode(String),
    #[error("network {0:?} not found (only {USER_NETWORK:?} is supported for now)")]
    NetworkNotFound(String),
    #[error("nic {0} has no MAC address")]
    MissingMac(usize),
    #[error(transparent)]
    Cluster(#[from] ClusterError),
}

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id다.
pub fn runtime_spec(cluster: &Cluster, vm: &Vm, node: Uuid) -> Result<VmRuntimeSpec, ResolveError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        disks.push(RuntimeDisk {
            path: DirStorage::volume_path(&pool.spec, volume.id, volume.spec.format),
            format: volume.spec.format,
            bus: disk.bus,
            boot_order: disk.boot_order,
        });
    }
    let mut nics = Vec::with_capacity(vm.spec.nics.len());
    for (i, nic) in vm.spec.nics.iter().enumerate() {
        if nic.network != USER_NETWORK {
            return Err(ResolveError::NetworkNotFound(nic.network.clone()));
        }
        nics.push(RuntimeNic {
            mac: nic.mac.ok_or(ResolveError::MissingMac(i))?,
            model: nic.model,
            backend: NicBackend::User,
        });
    }
    Ok(VmRuntimeSpec {
        id: vm.id,
        name: vm.name.clone(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks,
        nics,
    })
}
```

`crates/opl-reconciler/src/lib.rs`의 `mod config;` 아래에 추가한다:

```rust
pub mod resolve;
```

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p opl-reconciler --test resolve`
Expected: 2개 통과

- [ ] **Step 6: Commit**

```bash
git add crates/opl-reconciler
git commit -m "feat(reconciler): Volume 경로와 user 네트워크로 VM 참조 해석"
```

---

### Task 3: 작업자와 디스패처

조정 규칙(설계 5.3)을 구현한다. 작업자 하나가 VM 하나를 맡는다.

| 원하는 상태 | 실제 상태 | 작업자가 하는 일 | status |
|---|---|---|---|
| running | 돌고 있음 | 없음(일시정지 상태면 `start`로 `cont`) | Running, pid |
| running | 없음 | 백오프 중이면 기다리고, 아니면 시작 | Running 또는 Starting(+이유) |
| running | 방금 사라짐, 이벤트 아직 없음 | 이벤트를 1초 기다림 | 그대로 |
| stopped | 돌고 있음 | ACPI 요청 → `acpi_timeout` 뒤 강제 종료 | Stopping → Stopped |
| stopped | 없음 | 없음 | Stopped(포기했으면 Failed 유지) |

| 이벤트 (원하는 상태 running) | 작업자가 하는 일 |
|---|---|
| `Stopped`, 우리가 `acpi_timeout` 안에 요청한 종료 | 다시 시작(요청 뒤 사용자가 다시 켰다) |
| `Stopped`, 그 밖 | 게스트가 스스로 껐다: `spec.power = stopped` |
| `Crashed`, `restart: on-failure` | 백오프 뒤 재시작, 10분 안에 5번째 실패면 포기 |
| `Crashed`, `restart: never` | 포기 |
| 영구 기동 오류, 참조 해석 실패 | 포기 |

포기: `spec.power = stopped`, status `Failed` + `last_error`. 원하는 상태가 `stopped`일 때의 종료·크래시 이벤트는 무시한다.

**Files:**
- Modify: `crates/opl-reconciler/tests/common/mod.rs`, `crates/opl-reconciler/src/lib.rs`
- Create: `crates/opl-reconciler/tests/reconcile.rs`, `crates/opl-reconciler/src/worker.rs`, `crates/opl-reconciler/src/reconciler.rs`

- [ ] **Step 1: 하네스에 `Env` 추가**

`crates/opl-reconciler/tests/common/mod.rs`의 import에서 `use opl_reconciler::{ReconcilerConfig, RestartBackoff};`를 다음으로 바꾼다:

```rust
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
```

`use opl_types::vm::...;` 줄 아래에 `use tempfile::TempDir;`를 추가하고, 파일 끝에 추가한다:

```rust
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

    /// Mock 하이퍼바이저에서 실행 중인 VM의 pid
    pub async fn pid(&self, id: Uuid) -> Option<u32> {
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
}
```

- [ ] **Step 2: 실패하는 테스트 작성**

`crates/opl-reconciler/tests/reconcile.rs`:

```rust
//! Mock 하이퍼바이저로 조정 규칙(설계 5.3)을 확인한다.

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::DriverError;
use opl_types::vm::{DiskBus, DiskSpec, PowerState, RestartPolicy, VmPhase, VmStatus};
use uuid::Uuid;

fn running(v: &opl_types::vm::Vm) -> bool {
    v.status.phase == VmPhase::Running
}

#[tokio::test]
async fn starts_assigned_vm_and_records_status() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    let pid = env.pid(vm.id).await.expect("VM was not started");
    assert_eq!(
        got.status,
        VmStatus {
            phase: VmPhase::Running,
            node: Some(env.node),
            pid: Some(pid),
            last_error: None,
        }
    );
}

#[tokio::test]
async fn ignores_vms_not_assigned_to_this_node() {
    let env = Env::start().await;
    let other = vm("other", Uuid::now_v7());
    let mut unplaced = vm("unplaced", env.node);
    unplaced.spec.node = None;
    let mine = vm("mine", env.node);
    for v in [&other, &unplaced, &mine] {
        env.create(v).await;
    }

    // 마지막에 만든 VM까지 처리했으니 앞의 둘은 이미 건너뛰었다.
    env.wait(mine.id, "running", running).await;
    for v in [&other, &unplaced] {
        assert_eq!(env.pid(v.id).await, None, "{} was started", v.name);
        assert_eq!(env.get(v.id).status, VmStatus::default());
    }
}

#[tokio::test]
async fn stop_escalates_to_force_when_guest_ignores_acpi() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // Mock 게스트는 ACPI 요청을 무시하므로 acpi_timeout(300ms) 뒤 강제 종료로만 꺼진다.
    env.set_power(vm.id, PowerState::Stopped).await;
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);

    // 다시 켤 수 있다.
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    assert!(env.pid(vm.id).await.is_some());
}

#[tokio::test]
async fn acpi_stop_completes_when_guest_shuts_down() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    env.hv.guest_shutdown(vm.id);

    // 강제 종료(60초 뒤)를 기다리지 않고 끝난다. 우리가 요청한 종료이므로 오류가 아니다.
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn cancelled_stop_restarts_vm_when_guest_finishes_shutdown() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    // 게스트가 꺼지기 전에 사용자가 마음을 바꿨다.
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    // 게스트는 앞서 받은 ACPI 요청대로 꺼진다. 게스트가 스스로 끈 것으로 보면 안 된다.
    env.hv.guest_shutdown(vm.id);

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
}

#[tokio::test]
async fn guest_shutdown_turns_power_off() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.guest_shutdown(vm.id);

    // 다시 켜지 않고 원하는 상태를 꺼짐으로 바꾼다.
    let got = env
        .wait(vm.id, "powered off", |v| {
            v.spec.power == PowerState::Stopped && v.status.phase == VmPhase::Stopped
        })
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn crash_restarts_vm_with_on_failure_policy() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.restart = RestartPolicy::OnFailure;
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.hv.crash(vm.id);

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, got.status.pid);
}

#[tokio::test]
async fn crash_without_restart_policy_marks_failed() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.crash(vm.id);

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error.as_deref(), Some("VM crashed"));
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn repeated_crashes_give_up() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.restart = RestartPolicy::OnFailure;
    env.create(&vm).await;

    // 하네스 설정은 1분 안에 3번 실패하면 포기한다.
    let mut last = None;
    for _ in 0..3 {
        let got = env
            .wait(vm.id, "running", |v| running(v) && v.status.pid != last)
            .await;
        last = got.status.pid;
        env.hv.crash(vm.id);
    }

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("VM crashed; gave up after 3 failures")
    );
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn permanent_start_failure_marks_failed() {
    let env = Env::start().await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to start: QEMU failed to start: no firmware")
    );
}

#[tokio::test]
async fn transient_start_failure_is_retried() {
    let env = Env::start().await;
    env.hv.fail_next_start(DriverError::Timeout("qmp"));
    let vm = vm("web", env.node);
    env.create(&vm).await;

    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(got.spec.power, PowerState::Running);
}

#[tokio::test]
async fn unresolvable_volume_marks_failed() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "missing".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("volume \"missing\" not found")
    );
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn restarted_reconciler_keeps_running_vms() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    env.start_reconciler();

    // 새 reconciler가 첫 동기화를 마쳤는지는 새 VM이 뜨는 것으로 확인한다.
    let probe = common::vm("probe", env.node);
    env.create(&probe).await;
    env.wait(probe.id, "probe running", running).await;

    assert_eq!(env.pid(vm.id).await, before.status.pid);
    assert_eq!(env.get(vm.id).status, before.status);
}
```

- [ ] **Step 3: 실패 확인**

Run: `cargo test -p opl-reconciler --test reconcile`
Expected: 컴파일 실패 (`opl_reconciler::Reconciler`가 없음)

- [ ] **Step 4: 작업자 구현**

`crates/opl-reconciler/src/worker.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::resolve::{ResolveError, runtime_spec};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(1);

/// 작업자를 깨우는 이유.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
}

#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// VM 하나를 맡는 작업자. 이 VM에 대한 드라이버 호출은 모두 이 태스크에서만 일어난다.
///
/// 매번 로컬 저장소에서 VM을 다시 읽고 실제 상태를 조회해 맞추므로(level-triggered),
/// 같은 트리거를 여러 번 받아도 안전하다. 이벤트는 "게스트가 스스로 꺼졌다"처럼 실제 상태만
/// 봐서는 알 수 없는 사실을 알릴 때만 쓴다.
pub(crate) struct Worker {
    id: Uuid,
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 이 작업자가 ACPI 종료를 요청한 시각. 뒤이은 종료 이벤트가 우리 요청의 결과인지 가리고,
    /// 강제 종료로 넘어갈 시점을 정한다. 요청 뒤 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안은 지우지 않는다.
    stop_requested: Option<Instant>,
    failures: FailureTracker,
    /// 백오프 중이면 이 시각까지 시작하지 않는다. status에 보일 이유를 함께 둔다.
    backoff: Option<(Instant, String)>,
    /// spec.power를 stopped로 바꿔야 한다(게스트 종료, 재시작 포기). 안쪽 값은 status에 남길 오류다.
    /// 쓰기에 실패하면 남겨 두었다가 다음 단계에서 다시 쓴다.
    power_off: Option<Option<String>>,
    /// 마지막으로 조회했을 때 VM이 실행 중이었는지
    was_running: bool,
    /// 사라진 VM의 종료 이벤트를 이 시각까지 기다린다.
    awaiting_event: Option<Instant>,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            stop_requested: None,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            wake_at: None,
        }
    }

    /// 보내는 쪽이 모두 사라지면 남은 트리거를 처리하고 끝난다. `previous`는 같은 VM을 맡았던
    /// 작업자다. 같은 VM에 대한 드라이버 호출이 겹치지 않도록 그것이 끝난 뒤에 시작한다.
    pub(crate) async fn run(
        mut self,
        previous: Option<JoinHandle<()>>,
        mut rx: mpsc::UnboundedReceiver<Trigger>,
    ) {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        loop {
            let trigger = match self.wake_at {
                Some(at) => tokio::select! {
                    t = rx.recv() => t,
                    () = tokio::time::sleep_until(at) => Some(Trigger::Sync),
                },
                None => rx.recv().await,
            };
            let Some(trigger) = trigger else { return };
            self.wake_at = None;
            if let Err(e) = self.step(trigger).await {
                tracing::warn!(vm = %self.id, error = %e, "reconcile failed; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    fn wake(&mut self, at: Instant) {
        self.wake_at = Some(self.wake_at.map_or(at, |w| w.min(at)));
    }

    async fn step(&mut self, trigger: Trigger) -> Result<(), StepError> {
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다.
            if self.hv.inspect(self.id).await?.is_some() {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            return Ok(());
        };
        if let Trigger::Event(kind) = trigger {
            self.on_event(&vm, kind);
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.cluster
                        .update_status::<VmSpec>(self.id, &status)
                        .await?;
                }
                return Ok(());
            }
        }
        self.write_power_off(vm).await
    }

    fn on_event(&mut self, vm: &Vm, kind: EventKind) {
        let now = Instant::now();
        if kind != EventKind::Started {
            self.was_running = false;
            self.awaiting_event = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stop_requested
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            EventKind::Stopped { .. } if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 보지 않고, 우리가 요청했는지로 가린다.
            EventKind::Stopped { .. } => self.power_off = Some(None),
            EventKind::Crashed => match vm.spec.restart {
                RestartPolicy::Never => self.power_off = Some(Some("VM crashed".into())),
                RestartPolicy::OnFailure => self.record_failure("VM crashed".into()),
            },
            EventKind::Started => {}
        }
    }

    fn record_failure(&mut self, reason: String) {
        let now = Instant::now();
        match self.failures.record(now, &self.config.restart) {
            Next::RetryAfter(delay) => {
                self.backoff = Some((now + delay, format!("{reason}; retrying in {delay:?}")));
            }
            Next::GiveUp => {
                let n = self.config.restart.max_failures;
                self.power_off = Some(Some(format!("{reason}; gave up after {n} failures")));
            }
        }
    }

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다.
    async fn reconcile(&mut self, vm: &Vm) -> Result<VmStatus, StepError> {
        let actual = self.hv.inspect(self.id).await?;
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                if state.status == RuntimeStatus::Paused {
                    // opld가 기동 도중(cont 전)에 죽었다. start()가 cont를 보낸다.
                    self.start(vm).await?;
                }
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
            }
            // 방금까지 돌던 VM이 사라졌다. 왜 꺼졌는지(게스트 종료, 크래시) 이벤트로 안 뒤에 움직인다.
            // 기다려도 오지 않으면(이벤트 유실) 원하는 상태대로 다시 시작한다.
            (PowerState::Running, None)
                if self.awaiting_event.is_some_and(|deadline| now < deadline) =>
            {
                self.wake(self.awaiting_event.unwrap());
            }
            (PowerState::Running, None) => {
                self.awaiting_event = None;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if self.backoff.is_none() && self.start(vm).await? {
                    match self.hv.inspect(self.id).await? {
                        Some(state) => {
                            status.phase = VmPhase::Running;
                            status.pid = Some(state.pid);
                            status.last_error = None;
                        }
                        // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
                        None => {
                            status.phase = VmPhase::Starting;
                            status.pid = None;
                        }
                    }
                }
                if let Some((at, reason)) = self.backoff.clone() {
                    self.wake(at);
                    status.phase = VmPhase::Starting;
                    status.pid = None;
                    status.last_error = Some(reason);
                }
            }
            (PowerState::Stopped, Some(state)) => {
                self.backoff = None;
                match self.stop_requested {
                    None => {
                        self.hv.stop(self.id, StopMode::Acpi).await?;
                        self.stop_requested = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    Some(at) => self.wake(at + self.config.acpi_timeout),
                }
                status.phase = VmPhase::Stopping;
                status.pid = Some(state.pid);
            }
            (PowerState::Stopped, None) => {
                self.stop_requested = None;
                self.awaiting_event = None;
                self.backoff = None;
                self.failures.clear();
                // 포기해서 멈춘 VM은 사용자가 다시 시작할 때까지 Failed로 남긴다.
                if status.phase != VmPhase::Failed {
                    status.phase = VmPhase::Stopped;
                }
                status.pid = None;
            }
        }
        Ok(status)
    }

    /// VM을 시작한다. 시작하지 못하고 실패를 기록했으면(백오프 또는 포기) false.
    async fn start(&mut self, vm: &Vm) -> Result<bool, StepError> {
        let spec = match runtime_spec(&self.cluster, vm, self.config.node) {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 참조가 틀렸다. spec을 고쳐야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.power_off = Some(Some(e.to_string()));
                return Ok(false);
            }
        };
        match self.hv.start(&spec).await {
            Ok(()) => Ok(true),
            Err(e) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            Err(e) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(false)
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 그 사이 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서 다시 읽어 쓴다.
            self.cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
                .await?;
        }
        let status = VmStatus {
            phase: if error.is_some() {
                VmPhase::Failed
            } else {
                VmPhase::Stopped
            },
            node: Some(self.config.node),
            pid: None,
            last_error: error,
        };
        self.cluster
            .update_status::<VmSpec>(self.id, &status)
            .await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}
```

- [ ] **Step 5: 디스패처 구현**

`crates/opl-reconciler/src/reconciler.rs`:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor};
use opl_types::resource::Kind;
use opl_types::vm::VmSpec;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::ReconcilerConfig;
use crate::worker::{Trigger, Worker};

/// 한 노드의 조정 루프. `spawn`으로 시작하고 `shutdown`으로 멈춘다.
///
/// 멈춰도 VM은 건드리지 않는다. 다시 시작하면(하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙은 뒤)
/// 현재 실제 상태에서부터 이어서 맞춘다.
pub struct Reconciler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Reconciler {
    pub fn spawn(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReconcilerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let dispatcher = Dispatcher {
            cluster,
            hv,
            config: Arc::new(config),
            workers: HashMap::new(),
            retired: HashMap::new(),
        };
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx)),
        }
    }

    /// 진행 중인 단계가 끝나기를 기다린 뒤 돌아온다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Watch(#[from] WatchError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

struct WorkerHandle {
    tx: mpsc::UnboundedSender<Trigger>,
    task: JoinHandle<()>,
}

/// 저장소 변경, 하이퍼바이저 이벤트, 주기적 재동기화를 VM별 작업자에게 나눠 준다.
struct Dispatcher {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 이 노드에 배정된 VM의 작업자
    workers: HashMap<Uuid, WorkerHandle>,
    /// 내보냈지만 아직 끝나지 않았을 수 있는 작업자. 같은 VM이 다시 배정되면 새 작업자가 기다린다.
    retired: HashMap<Uuid, JoinHandle<()>>,
}

impl Dispatcher {
    async fn run(mut self, mut shutdown: oneshot::Receiver<()>) {
        // 목록을 읽기 전에 구독해야 그 사이에 난 이벤트를 놓치지 않는다.
        let mut events = self.hv.subscribe();
        'sync: loop {
            let mut watch = match self.full_sync().await {
                Ok(watch) => watch,
                Err(e) => {
                    tracing::warn!(error = %e, "full sync failed; will retry");
                    tokio::select! {
                        _ = &mut shutdown => break,
                        () = tokio::time::sleep(self.config.retry_interval) => continue,
                    }
                }
            };
            let resync = tokio::time::sleep(self.config.resync_interval);
            tokio::pin!(resync);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break 'sync,
                    () = &mut resync => continue 'sync,
                    ev = watch.next() => match ev {
                        Ok(WatchEvent::Put { kind: Kind::Vm, resource })
                        | Ok(WatchEvent::Deleted { kind: Kind::Vm, resource, .. }) => {
                            self.vm_changed(resource.id);
                        }
                        Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                        Ok(_) => {}
                    },
                    ev = events.recv() => match ev {
                        Ok(ev) => {
                            if !self.notify(ev.id, Trigger::Event(ev.kind)) {
                                tracing::debug!(vm = %ev.id, "event for a VM not assigned to this node");
                            }
                        }
                        // 이벤트를 놓쳤다. 실제 상태를 전부 다시 맞춘다.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue 'sync,
                        Err(broadcast::error::RecvError::Closed) => break 'sync,
                    },
                }
            }
        }
        let workers = self.workers.drain().map(|(_, w)| w.task);
        let tasks: Vec<_> = workers
            .chain(self.retired.drain().map(|(_, t)| t))
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// 목록을 다시 읽어 작업자를 맞추고, 그 revision부터 구독한다.
    async fn full_sync(&mut self) -> Result<Watch, SyncError> {
        self.retired.retain(|_, task| !task.is_finished());
        let (revision, vms) = self.cluster.list::<VmSpec>()?;
        let mine: HashSet<Uuid> = vms
            .iter()
            .filter(|vm| vm.spec.node == Some(self.config.node))
            .map(|vm| vm.id)
            .collect();
        let gone: Vec<Uuid> = self
            .workers
            .keys()
            .filter(|id| !mine.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.retire(id);
        }
        for &id in &mine {
            self.sync(id);
        }
        for state in self.hv.list().await? {
            if !mine.contains(&state.id) {
                tracing::warn!(vm = %state.id, "VM is running on this node but not assigned to it; leaving it alone");
            }
        }
        Ok(self.cluster.watch(revision)?)
    }

    fn vm_changed(&mut self, id: Uuid) {
        match self.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => self.sync(id),
            Ok(_) => self.retire(id),
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                self.notify(id, Trigger::Sync);
            }
        }
    }

    fn notify(&self, id: Uuid, trigger: Trigger) -> bool {
        self.workers
            .get(&id)
            .is_some_and(|w| w.tx.send(trigger).is_ok())
    }

    fn sync(&mut self, id: Uuid) {
        if self.notify(id, Trigger::Sync) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker::new(
            id,
            self.cluster.clone(),
            self.hv.clone(),
            self.config.clone(),
        );
        let previous = self.retired.remove(&id);
        let task = tokio::spawn(worker.run(previous, rx));
        let _ = tx.send(Trigger::Sync);
        self.workers.insert(id, WorkerHandle { tx, task });
    }

    /// 삭제되었거나 다른 노드로 배정된 VM의 작업자를 내보낸다. 작업자는 마지막으로 한 번 확인하고 끝난다.
    fn retire(&mut self, id: Uuid) {
        if let Some(w) = self.workers.remove(&id) {
            let _ = w.tx.send(Trigger::Sync);
            self.retired.insert(id, w.task);
        }
    }
}
```

`crates/opl-reconciler/src/lib.rs` 전체:

```rust
//! 원하는 상태(Raft에 기록된 spec)와 실제 상태(하이퍼바이저)를 맞추는 조정 루프.
//!
//! 노드마다 하나씩 돈다. 이 노드에 배정된 VM마다 작업자 태스크를 하나 두어 같은 VM에 대한
//! 드라이버 호출을 직렬화하고, 서로 다른 VM은 병렬로 처리한다.

mod backoff;
mod config;
mod reconciler;
pub mod resolve;
mod worker;

pub use config::{ReconcilerConfig, RestartBackoff};
pub use reconciler::Reconciler;
```

- [ ] **Step 6: 통과 확인**

Run: `cargo test -p opl-reconciler`
Expected: 단위 2개, `reconcile` 13개, `resolve` 2개 통과

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: 경고 없음 (Task 1의 dead_code 경고도 사라진다)

- [ ] **Step 7: 부하 속 안정성 확인**

테스트 바이너리 10개를 동시에 3번 돌린다. CI 러너처럼 CPU가 바쁠 때 타이밍에 기대는 테스트를 잡아낸다.

```bash
cargo test -p opl-reconciler --test reconcile --no-run
B=$(ls -t target/debug/deps/reconcile-* | grep -v '\.d$' | head -1)
fails=0
for r in 1 2 3; do
  pids=()
  for i in $(seq 10); do "$B" -q >/dev/null 2>&1 & pids+=($!); done
  for p in "${pids[@]}"; do wait "$p" || fails=$((fails+1)); done
done
echo "failures: $fails"
```

Expected: `failures: 0`

- [ ] **Step 8: Commit**

```bash
git add crates/opl-reconciler
git commit -m "feat(reconciler): VM별 작업자와 디스패처로 시작·종료·크래시 재시작·게스트 종료 처리"
```

---

### Task 4: 과반수 상실 중 동작

설계 10절 불변식 4(과반수를 잃어도 실행 중인 VM은 중지되지 않는다)와 5.3절 status 재기록을 고정한다. 노드 2개짜리 클러스터에서 n2를 내리면 과반수가 없다. n2는 선거 제한 시간을 아주 길게 두어, 돌아와도 n1이 그대로 리더다(팔로워의 쓰기 전달은 계획 6).

**Files:**
- Create: `crates/opl-reconciler/tests/no_quorum.rs`

- [ ] **Step 1: 테스트 작성**

`crates/opl-reconciler/tests/no_quorum.rs`:

```rust
//! Raft 과반수를 잃어도 실행 중인 VM은 그대로 돌고(설계 10절 불변식 4),
//! 못 쓴 status는 과반수가 돌아오면 기록된다.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{WAIT, open_node, raft_config, vm, wait_leader, wait_vm};
use openraft::BasicNode;
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_driver::{Hypervisor, MockHypervisor};
use opl_reconciler::Reconciler;
use opl_types::vm::{RestartPolicy, VmPhase, VmSpec};
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn vm_is_restarted_without_quorum_and_status_catches_up() {
    let dir = TempDir::new().unwrap();
    let router = MemRouter::default();
    let write_timeout = Duration::from_millis(300);
    let (n1, n2) = (NodeId(Uuid::now_v7()), NodeId(Uuid::now_v7()));
    // n2는 선거를 시작하지 않는다. 그래서 n2가 내려갔다 돌아와도 n1이 그대로 리더다.
    let follower = openraft::Config {
        election_timeout_min: 60_000,
        election_timeout_max: 120_000,
        ..raft_config()
    };
    let c1 = open_node(
        &router,
        n1,
        &dir.path().join("n1"),
        raft_config(),
        write_timeout,
    )
    .await;
    let c2 = open_node(
        &router,
        n2,
        &dir.path().join("n2"),
        follower.clone(),
        write_timeout,
    )
    .await;
    let members = BTreeMap::from([(n1, BasicNode::new("n1")), (n2, BasicNode::new("n2"))]);
    c1.initialize(members).await.unwrap();
    wait_leader(&c1).await;

    let hv = Arc::new(MockHypervisor::default());
    let dyn_hv: Arc<dyn Hypervisor> = hv.clone();
    let reconciler = Reconciler::spawn(c1.clone(), dyn_hv, common::config(n1.0));
    let mut vm = vm("web", n1.0);
    vm.spec.restart = RestartPolicy::OnFailure;
    c1.create(&vm).await.unwrap();
    let before = wait_vm(&c1, vm.id, "running", |v| {
        v.status.phase == VmPhase::Running
    })
    .await
    .status
    .pid;

    // n2를 내려 과반수를 없앤다.
    router.unregister(n2);
    c2.shutdown().await.unwrap();
    drop(c2);
    hv.crash(vm.id);

    // status를 쓰지 못해도 VM은 다시 시작된다.
    let deadline = Instant::now() + WAIT;
    let after = loop {
        let pid = hv.inspect(vm.id).await.unwrap().map(|s| s.pid);
        if pid.is_some() && pid != before {
            break pid;
        }
        assert!(Instant::now() < deadline, "VM was not restarted");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::sleep(write_timeout * 2).await;
    assert_eq!(
        c1.get::<VmSpec>(vm.id).unwrap().unwrap().status.pid,
        before,
        "status must not change without quorum"
    );

    // n2가 돌아오면 미뤄 둔 status가 기록된다.
    let _c2 = open_node(&router, n2, &dir.path().join("n2"), follower, write_timeout).await;
    wait_vm(&c1, vm.id, "status caught up", |v| {
        v.status.phase == VmPhase::Running && v.status.pid == after
    })
    .await;
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), after);
    reconciler.shutdown().await;
}
```

- [ ] **Step 2: 통과 확인**

Run: `cargo test -p opl-reconciler --test no_quorum`
Expected: 1개 통과. Task 3의 작업자가 이미 쓰기 실패를 `retry_interval`마다 다시 시도하므로 구현 변경은 없다.

Step 7과 같은 명령에서 `reconcile`을 `no_quorum`으로 바꿔 부하 속에서도 돌린다.
Expected: `failures: 0`

- [ ] **Step 3: Commit**

```bash
git add crates/opl-reconciler/tests/no_quorum.rs
git commit -m "test(reconciler): 과반수가 없어도 VM을 다시 시작하고 status는 회복 뒤 기록"
```

---

### Task 5: 실제 QEMU로 opld 재시작 확인 (M4)

reconciler, 드라이버, 클러스터 저장소를 모두 내렸다가 새로 연다. QEMU는 `-daemonize`로 떠 있어 계속 돌고, 새 드라이버가 pidfile 잠금으로 살아 있는 것을 확인해 다시 붙는다(계획 2). 새 reconciler는 VM을 다시 시작하지 않고(같은 pid), 다시 붙은 VM을 끌 수 있어야 한다. 디스크 없는 UEFI 펌웨어는 ACPI 종료에 응하지 않을 수 있으므로 강제 종료 승격까지 실제 QEMU로 확인된다.

**Files:**
- Create: `crates/opl-reconciler/tests/qemu_restart.rs`
- Modify: `README.md`

- [ ] **Step 1: 테스트 작성**

`crates/opl-reconciler/tests/qemu_restart.rs`:

```rust
//! M4 완료 기준: opld가 죽었다 다시 떠도 VM은 계속 돌고, 새 opld가 다시 붙어 제어한다.
//! `cargo test -p opl-reconciler --features qemu-it --test qemu_restart`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{open_node, raft_config, set_power, single_node, vm, wait_leader, wait_vm};
use opl_cluster::memnet::MemRouter;
use opl_driver::{Hypervisor, QemuConfig, QemuHypervisor};
use opl_qemu::HostProfile;
use opl_reconciler::Reconciler;
use opl_types::vm::{PowerState, VmPhase, VmSpec};

fn qemu_config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::homebrew_hvf(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
    }
}

#[tokio::test]
async fn vm_survives_opld_restart_and_is_controlled_again() {
    // 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let router = MemRouter::default();
    let cluster = single_node(&router, &tmp.path().join("cluster")).await;
    let node = cluster.id();
    let mut config = common::config(node.0);
    // 디스크 없는 UEFI 펌웨어는 ACPI 종료에 응하지 않을 수 있다. 강제 종료까지 확인한다.
    config.acpi_timeout = Duration::from_secs(3);

    let hv: Arc<dyn Hypervisor> =
        Arc::new(QemuHypervisor::new(qemu_config(tmp.path())).await.unwrap());
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), config.clone());
    let vm = vm("it-vm", node.0);
    cluster.create(&vm).await.unwrap();
    let running = wait_vm(&cluster, vm.id, "running", |v| {
        v.status.phase == VmPhase::Running
    })
    .await;
    let pid = running.status.pid.expect("pid recorded");

    // opld가 죽은 것처럼 reconciler, 드라이버, 클러스터를 모두 내린다.
    // QEMU는 -daemonize로 떠 있어 opld와 함께 죽지 않는다.
    reconciler.shutdown().await;
    drop(hv);
    router.unregister(node);
    cluster.shutdown().await.unwrap();
    drop(cluster);

    // 새 opld: 저장소를 다시 열고, 드라이버가 살아 있는 QEMU에 다시 붙는다.
    let cluster = open_node(
        &router,
        node,
        &tmp.path().join("cluster"),
        raft_config(),
        Duration::from_secs(5),
    )
    .await;
    wait_leader(&cluster).await;
    let hv: Arc<dyn Hypervisor> =
        Arc::new(QemuHypervisor::new(qemu_config(tmp.path())).await.unwrap());
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), Some(pid));
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), config);

    // 새 reconciler는 VM을 다시 시작하지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(hv.inspect(vm.id).await.unwrap().map(|s| s.pid), Some(pid));
    let got = cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(
        (got.status.phase, got.status.pid),
        (VmPhase::Running, Some(pid))
    );

    // 다시 붙은 VM을 끌 수 있다.
    set_power(&cluster, vm.id, PowerState::Stopped).await;
    wait_vm(&cluster, vm.id, "stopped", |v| {
        v.status.phase == VmPhase::Stopped
    })
    .await;
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    reconciler.shutdown().await;
}
```

- [ ] **Step 2: 통과 확인** (Apple Silicon Mac, `brew install qemu`)

Run: `cargo test -p opl-reconciler --features qemu-it --test qemu_restart`
Expected: 1개 통과 (약 5초)

Run: `pgrep -fl qemu-system`
Expected: 출력 없음 (테스트가 띄운 QEMU가 남지 않는다)

- [ ] **Step 3: README에 명령 추가**

`README.md`의 개발 명령 블록 마지막 줄(`cargo test -p opl-driver --features qemu-it --test qemu_driver ...`) 아래에 추가한다:

```bash
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: opld 재시작 후 재연결
```

- [ ] **Step 4: Commit**

```bash
git add crates/opl-reconciler/tests/qemu_restart.rs README.md
git commit -m "test(reconciler): 실제 QEMU로 opld 재시작 뒤 VM 유지와 재연결 확인"
```

---

### Task 6: 설계 문서, README 갱신

**Files:**
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`, `README.md`

- [ ] **Step 1: 설계 5.3절 규칙 갱신**

다음 두 줄을:

```markdown
- **게스트 내부 종료:** QMP `SHUTDOWN` 이벤트가 게스트가 시작한 종료이면 `spec.power`를 `stopped`로 바꾼다.
- **크래시:** QEMU가 비정상 종료하면 status에 오류 조건을 기록한다. `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 중단한다.
```

다음으로 바꾼다:

```markdown
- **종료:** 원하는 상태가 `stopped`면 ACPI 종료를 요청하고, `acpi_timeout`(기본 60초) 안에 꺼지지 않으면 강제 종료한다.
- **게스트 내부 종료:** reconciler가 요청하지 않은 종료 이벤트가 오면(원하는 상태가 `running`인데 꺼짐) `spec.power`를 `stopped`로 바꾼다. QMP의 `guest_initiated`는 opld가 요청한 ACPI 종료에서도 true이므로 쓰지 않는다. 방금까지 돌던 VM이 사라졌는데 이벤트가 아직 없으면 잠시 기다려, 게스트가 끈 VM을 다시 켜지 않게 한다.
- **크래시와 기동 실패:** `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 포기한다. `restart: never`의 크래시, 영구 기동 오류(이미지 없음, 잘못된 참조 등), 포기는 모두 `spec.power`를 `stopped`로 바꾸고 status를 `Failed`와 오류 메시지로 남긴다. 사용자가 다시 시작하면 처음부터 센다.
- **배정되지 않은 VM:** 이 노드에서 돌지만 이 노드에 배정되지 않은(삭제되었거나 다른 노드로 옮겨진) VM은 끄지 않고 경고만 남긴다. 저장소가 비거나 뒤처져도 VM을 죽이지 않기 위해서다. 실행 중인 VM의 삭제와 노드 변경은 API가 거부한다.
```

같은 절의 status 쓰기 줄 끝에 문장을 더한다:

```markdown
- **status 쓰기:** reconciler는 자기 노드의 VM status만 기록한다. Raft 과반수가 없을 때는 로컬에 버퍼링했다가 회복되면 다시 기록한다. 버퍼는 메모리에만 두며, 재시작하면 실제 상태에서 다시 계산한다.
```

- [ ] **Step 2: 설계 10절 테스트 도구 갱신**

컴포넌트 행의 `tokio 테스트, 가짜 시계`를 `tokio 테스트, 짧게 줄인 시간 설정`으로 바꾼다.

- [ ] **Step 3: README 갱신**

`README.md`에서 `- 상태: 개발 초기 (M3)`을 `- 상태: 개발 초기 (M4)`로 바꾸고, 문서 목록 끝에 추가한다:

```markdown
- [계획 4: reconciler](docs/superpowers/plans/2026-10-21-plan4-opl-reconciler.md)
```

- [ ] **Step 4: 전체 확인**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace && cargo deny check`
Expected: 모두 통과

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/specs/2026-10-20-opl-node-core-design.md README.md
git commit -m "docs: reconciler 규칙(포기, 배정되지 않은 VM, 이벤트 대기)을 설계에 반영"
```

---

## 완료 기준 (M4)

- [ ] `cargo test --workspace`가 통과한다 (macOS, Linux CI).
- [ ] 원하는 상태에 따라 VM을 시작하고, ACPI로 끄고, 응답이 없으면 강제로 끈다.
- [ ] 게스트가 스스로 끄면 원하는 상태가 `stopped`가 되고, 크래시는 재시작 정책과 백오프를 따른다.
- [ ] 과반수가 없어도 VM은 계속 돌고(다시 시작도 하고), status는 회복 뒤 기록된다.
- [ ] 실제 QEMU에서 reconciler·드라이버·저장소를 다시 띄워도 VM은 같은 pid로 계속 돌고 다시 제어된다.

## 다음 계획으로 넘길 사항

계획 5 (API, opld):
- 실행 중인 VM의 삭제와 `spec.node` 변경을 거부한다(409). reconciler는 배정되지 않은 VM을 끄지 않는다.
- VM을 만들 때 MAC이 없는 NIC에 `MacAddr::generate()`로 채운다. `power: running`인데 `node`가 비면 스케줄러가 고른다.
- 볼륨 파일 생성·삭제·가져오기는 API Task에서 `Storage`로 한다.
- `stop?mode=force`와 `reboot`은 그 VM의 작업자 큐를 거쳐야 불변식 3이 유지된다. `Reconciler`에 VM별 명령을 넣는 통로를 더한다.
- `opld` 바이너리를 만들고 실제 `kill -9` 뒤 재연결을 E2E로 확인한다.

계획 6 (클러스터 부트스트랩):
- 팔로워 노드의 reconciler는 status를 쓰지 못한다(`NotLeader`를 받고 계속 다시 시도한다). `Cluster::write`가 리더로 전달해야 한다.
- 과반수가 없는 동안 status 쓰기가 `retry_interval`마다 미커밋 엔트리를 리더 로그에 남긴다. 회복되면 모두 커밋되고 마지막 값이 남는다. 장애가 길면 재시도 간격을 늘리는 것을 검토한다.

나중에:
- 하이퍼바이저 이벤트를 놓치면(`Lagged`, opld가 내려가 있던 동안의 게스트 종료) 원하는 상태대로 다시 켠다.
- 일시정지(`Paused`) VM은 시작 도중 opld가 죽은 경우로 보고 `cont`한다. 사용자 일시정지 기능이 생기면 구분한다.
