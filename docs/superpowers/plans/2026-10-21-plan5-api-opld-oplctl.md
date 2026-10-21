# opLite 계획 5: API, opld, oplctl (M5) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** REST API(`opl-api`), 노드 데몬(`opld`), 명령줄 클라이언트(`oplctl`)를 만든다. CLI로 로그인해 풀·볼륨·VM을 만들고, VM을 켜고 콘솔에 붙고, `opld`를 `kill -9` 해도 VM이 계속 돌다가 다시 제어되는 것을 실제 QEMU E2E 테스트로 고정한다(M5).

**Architecture:** API는 원하는 상태(spec)와 Task 리소스를 Raft에 기록하기만 한다(설계 2.1). VM을 움직이는 것은 계획 4의 reconciler이고, Task의 진행은 대상 노드의 Task 실행기가 reconciler처럼 목록을 읽고 그 revision부터 구독해 실행·기록한다. 강제 종료는 reconciler의 VM별 작업자 큐를 거치므로 같은 VM의 드라이버 호출이 겹치지 않는다(불변식 3). 인증은 로컬 사용자(argon2) + 세션 쿠키(웹 UI용) + API 토큰(CLI용, SHA-256 해시만 저장)이고, 모든 핸들러가 `Authorizer` 훅을 거친다. 라우터와 OpenAPI 문서는 `utoipa-axum`으로 한 정의에서 만든다. `opld`는 아직 1인 클러스터이고(노드 간 통신은 계획 6), 루프백 HTTP로만 연다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8(ws), utoipa 6 + utoipa-axum 0.3, argon2 0.6, sha2 0.11, reqwest 0.13, tokio-tungstenite 0.29, clap 4, toml, tracing-subscriber, `opl-cluster`·`opl-reconciler`·`opl-driver`·`opl-types`

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (2, 3.2, 3.3, 4, 5.1, 8, 10, 11절)
**선행 계획:** 계획 1~4 (완료)

---

## 범위

| 포함 | 제외 (담당 계획) |
|---|---|
| REST API: 풀·볼륨·VM·노드·Task CRUD, `If-Match`, 오류 형식, 이름 검증 | 팔로워가 받은 쓰기를 리더로 전달, 다른 노드의 콘솔 중계 — 계획 6 |
| VM 동작 start/stop/`stop?mode=force`/reboot와 Task, 스케줄러, MAC 자동 할당 | HTTPS, mTLS, 클러스터 CA, join/leave — 계획 6 |
| 볼륨 생성·가져오기(업로드)·삭제 Task, 다른 파일을 참조하는 이미지 거부 | 노드 heartbeat(온라인 판정), 풀 용량 status — 계획 6 |
| 로컬 사용자, 세션 쿠키, API 토큰, 고정 역할 3개, `Authorizer` 훅 | LDAP/SSO, 세분화된 RBAC — 엔터프라이즈 플러그인 |
| watch WebSocket, VNC 콘솔 WebSocket, OpenAPI(`/api/v1/openapi.json`) | 웹 UI — 하위 프로젝트 2 |
| `opld`(설정 파일, 첫 관리자, Node 등록, 정상 종료), `oplctl` | bridge 네트워크, `Network` 리소스, QemuKvm — 계획 7 |
| 실제 QEMU E2E: CLI → VM → 콘솔 → `kill -9` → 재연결 | 로그인 시도 제한, 감사 로그, Task 보존 기간 — 나중에 |

## 이전 계획에서 이어받는 사항

계획 3에서:
- `ClusterError`를 HTTP로 옮긴다: `Timeout` → 503 `cluster_no_quorum`, `Rejected(RevisionMismatch)` → 412, `NameTaken`/`AlreadyExists` → 409, `NotFound` → 404, `NotLeader` → 503 `not_leader`(리더 정보를 `details`에 담는다. 전달은 계획 6).
- 리소스 이름 형식은 API가 검증한다. 저장소는 유일성만 본다.
- status 쓰기도 revision을 올린다. spec 전용 revision은 두지 않는다. 대신 서버 안에서 spec을 바꾸는 쓰기(동작, Task 실행기)는 revision 불일치를 받으면 다시 읽어 쓰고, 사용자의 `PUT`만 `If-Match`를 그대로 검사한다.
- `read_barrier`는 쓰지 않는다. 이 계획은 1인 클러스터라 로컬 읽기가 곧 최신이다.

계획 4에서:
- 실행 중인 VM의 삭제와 `spec.node` 변경은 409로 거부한다. reconciler는 배정되지 않은 VM을 끄지 않는다.
- VM을 만들거나 바꿀 때 MAC이 없는 NIC에 `MacAddr::generate()`로 채운다(클러스터 안에서 중복이면 다시 뽑는다). `power: running`인데 `node`가 비면 스케줄러가 고른다.
- 볼륨 파일 생성·삭제·가져오기는 Task에서 `Storage`로 한다.
- `stop?mode=force`는 reconciler의 작업자 큐를 거친다(`ReconcilerHandle::force_stop`). `reboot`은 원하는 상태를 stopped로 바꿨다가 꺼지면 running으로 되돌리는 것으로 만들어, 드라이버 호출은 모두 작업자에 남긴다.
- `opld` 바이너리를 만들고 실제 `kill -9` 뒤 재연결을 E2E로 확인한다.

계획 2에서:
- `Storage::import_volume`의 원본은 믿을 수 있는 경로여야 한다. 올린 파일은 `check_import_source`로 다른 파일(VMDK extent, backing file)을 참조하지 않는지 먼저 확인한다.

## 설계 결정

- **API는 기록만 하고, 실행은 대상 노드의 Task 실행기가 한다.** 동작 요청은 spec을 바꾸고 Task(`spec.node` = VM이 배치된 노드)를 만든 뒤 202를 돌려준다. 실행기는 reconciler처럼 `list` 후 `watch`로 자기 노드의 Pending Task를 받아 실행하므로, 계획 6에서 다른 노드의 API가 만든 Task도 그대로 대상 노드에서 돈다. 같은 대상(VM, 볼륨)의 Task는 차례로 실행한다(`Storage`의 볼륨별 직렬화 요구).
- **Task 실행기는 VM을 직접 다루지 않는다.** start/stop Task는 reconciler가 원하는 상태에 도달하는 것을 기다려 결과를 기록할 뿐이다. 성공: `power`와 `phase`가 모두 running(또는 stopped). 실패: reconciler가 포기해 `power`를 stopped로 바꾼 경우(`last_error`를 옮긴다), 사용자가 반대로 바꾼 경우. 이전 실행의 `Failed` status는 `power`가 running인 동안 무시하므로 새 시작이 옛 실패로 끝나지 않는다.
- **opld가 실행 중에 죽은 Task는 실패로 기록한다.** 실행기가 처음 목록을 읽을 때 자기 노드의 Running Task는 이전 프로세스의 것이므로 "interrupted"로 끝내고, 만들다 만 볼륨을 지운다. Pending Task는 그대로 실행한다.
- **API 토큰은 `opl_<id>_<비밀 값>`이다.** id로 토큰 리소스를 찾고 비밀 값의 SHA-256을 상수 시간으로 비교한다. 토큰은 높은 엔트로피의 난수라 argon2가 필요 없고, 요청마다 argon2를 돌리지 않는다. 토큰과 세션은 요청마다 주인 User를 다시 읽으므로 사용자를 지우거나 역할을 바꾸면 곧바로 반영된다. 비밀 값은 만들 때 한 번만 보여 준다.
- **세션은 노드 메모리에만 둔다(12시간).** 웹 UI용이고, opld가 재시작하면 다시 로그인한다. CLI는 세션으로 로그인한 직후 API 토큰을 만들어 저장하고 세션은 닫는다.
- **첫 관리자 비밀번호는 파일로 남긴다.** 사용자가 하나도 없으면 opld가 `admin`을 만들고 비밀번호를 `{data_dir}/initial-admin-password`(0600)에 쓴다. `oplctl cluster init`(계획 6)이 생기면 그쪽으로 옮긴다.
- **HTTPS는 계획 6에서 한다.** 클러스터 CA가 생겨야 인증서를 발급할 수 있다. 그때까지 `opld`의 기본 주소는 `127.0.0.1:8006`이고, 세션 쿠키에는 `Secure`를 붙이지 않는다.
- **경로의 `{key}`는 id나 이름이다.** CLI가 이름으로 다루기 쉽게 한다. UUID 모양의 이름은 받지 않아 둘이 섞이지 않는다.
- **목록 응답은 `{revision, items}`이다.** 클라이언트는 그 revision으로 `GET /api/v1/watch?kinds=vm,task&since=<revision>`을 연다(Kubernetes의 list-then-watch). 사용자와 토큰은 해시를 담고 있어 watch로 내보내지 않는다.
- **이미지 업로드는 요청 본문을 그대로 스트리밍한다.** `POST /api/v1/volumes/import?name=…&pool=…&source_format=vmdk`. 본문을 이 노드의 `{data_dir}/uploads`에 받아 두고 가져오기 Task를 만든다. 풀은 이 노드에서 쓸 수 있어야 한다.
- **reboot은 끄고 다시 켜는 것이다.** QMP `system_reset`(하드 리셋) 대신 ACPI 종료 → (응답 없으면 `acpi_timeout` 뒤 강제 종료) → 시작으로, Proxmox의 `qm reboot`과 같다. 그 사이 사용자가 다시 켜면 그대로 둔다.
- **OpenAPI는 `utoipa-axum`으로 라우터와 함께 만든다.** 문서에 없는 경로나 경로 없는 문서가 생기지 않는다. 리소스 스키마는 `opl-types`의 `openapi` 기능으로 derive한다. utoipa 6이 Rust 1.88을 요구하고 이 계획의 코드도 let-chain을 쓰므로 MSRV를 1.88로 올린다.
- **E2E 테스트는 `opld` 프로세스를 띄우고 `oplctl`을 라이브러리로 부른다.** 테스트 크레이트가 다른 패키지의 바이너리를 빌드할 수 없어서, `oplctl`은 `run(args, out)`을 내보내는 라이브러리 + 얇은 바이너리로 만든다. 출력과 종료 코드를 빼면 바이너리와 같은 코드 경로다.

## 파일 구조

```
Cargo.toml                                   # 수정: MSRV 1.88, 새 크레이트와 의존성
README.md                                    # 수정: 상태 M5, 실행 방법, 테스트 명령, 문서 링크
.github/workflows/ci.yml                     # 수정: opl-api 볼륨 테스트(qemu-img)
docs/superpowers/specs/...-design.md         # 수정: 4절 API, 3.2절 Task, 5.1절 흐름, 8·10·11절
crates/opl-types/src/
├── resource.rs                              # 수정: Kind 추가, validate_name, openapi derive
├── node.rs, task.rs, auth.rs                # 새 리소스: Node, Task, User/ApiToken
├── storage.rs                               # 수정: SourceFormat을 opl-driver에서 옮김
└── vm.rs, runtime.rs                        # 수정: openapi derive
crates/opl-driver/src/
├── storage.rs                               # 수정: check_import_source
└── mock.rs                                  # 수정: set_console_dir
crates/opl-reconciler/src/
├── reconciler.rs                            # 수정: ReconcilerHandle::force_stop
└── worker.rs                                # 수정: Trigger::ForceStop
crates/opl-api/
├── src/
│   ├── lib.rs                               # 라우터 + OpenAPI 문서
│   ├── error.rs                             # ApiError, 오류 본문, JSON/쿼리 추출기
│   ├── state.rs                             # AppState
│   ├── auth.rs                              # Principal, Authorizer, 비밀번호, 토큰, 세션
│   ├── scheduler.rs                         # 노드 고르기(설계 3.3)
│   ├── tasks.rs                             # Task 실행기
│   ├── resources.rs                         # 공통 요청 형식, 목록·조회, 풀, 노드
│   ├── users.rs                             # 로그인, 사용자, 토큰
│   ├── vms.rs                               # VM CRUD와 동작
│   ├── volumes.rs                           # 볼륨 생성·가져오기·삭제
│   ├── watch.rs                             # 변경 구독 WebSocket
│   └── console.rs                           # VNC 콘솔 WebSocket
└── tests/
    ├── common/mod.rs                        # 1인 클러스터 + Mock + reconciler + 실행기 + 서버
    ├── auth.rs, vms.rs, tasks.rs, streams.rs, openapi.rs
    └── volumes.rs                           # qemu-img (storage-it)
crates/oplctl/src/
├── lib.rs, main.rs
├── cli.rs                                   # 명령 정의와 실행
├── client.rs                                # REST 클라이언트, Task 기다리기
├── config.rs                                # ~/.config/opl/oplctl.json (0600)
├── console.rs                               # 로컬 TCP → 콘솔 WebSocket
└── output.rs                                # 표, JSON
crates/opld/
├── src/
│   ├── main.rs                              # 조립, 신호, 정상 종료
│   ├── config.rs                            # TOML 설정
│   ├── bootstrap.rs                         # 노드 id, 1인 클러스터, 첫 관리자, Node 등록
│   └── host.rs                              # 호스트 메모리
└── tests/
    ├── common/mod.rs                        # opld 프로세스 + oplctl 라이브러리
    ├── smoke.rs                             # QEMU 없이: CLI, 재시작 후 상태 유지
    └── e2e.rs                               # 실제 QEMU (qemu-it): M5 완료 기준
```

---

### Task 1: 리소스 타입 (Node, Task, User, ApiToken)과 이름 규칙

API가 다룰 리소스를 `opl-types`에 더한다. API 문서용 스키마는 `openapi` 기능을 켰을 때만 derive한다(`opl-api`만 켠다). `SourceFormat`은 Task에 들어가야 하므로 `opl-driver`에서 `opl-types::storage`로 옮기고, `opl-driver`는 같은 이름으로 다시 내보낸다. 볼륨에는 준비 상태를 둔다. 파일을 만들거나 가져오는 중인 볼륨은 VM에 붙일 수 없다.

**Files:**
- Modify: `Cargo.toml`, `crates/opl-types/Cargo.toml`, `crates/opl-types/src/{lib,resource,storage,vm,runtime}.rs`, `crates/opl-driver/src/{lib,storage}.rs`, `crates/opl-cluster/tests/single_node.rs`
- Create: `crates/opl-types/src/{node,task,auth}.rs`

- [ ] **Step 1: 워크스페이스 설정**

루트 `Cargo.toml`에서 `rust-version = "1.85"`를 `rust-version = "1.88"`로 바꾼다(utoipa 6과 let-chain). `[workspace.dependencies]` 끝에 추가한다:

```toml
utoipa = { version = "6.0.0", features = ["uuid"] }
```

`crates/opl-types/Cargo.toml` 전체:

```toml
[package]
name = "opl-types"
description = "opLite resource model"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# API 문서(OpenAPI) 스키마. opl-api가 켠다.
openapi = ["dep:utoipa"]

[dependencies]
serde.workspace = true
thiserror.workspace = true
utoipa = { workspace = true, optional = true }
uuid.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

- [ ] **Step 2: 이름 규칙 테스트 작성**

`crates/opl-types/src/resource.rs`의 `#[cfg(test)] mod tests { ... }` 전체를 다음으로 바꾼다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
    struct DummyStatus {
        ready: bool,
    }

    #[test]
    fn new_resource_has_v7_id_zero_revision_and_default_status() {
        let r: Resource<u32, DummyStatus> = Resource::new("web-01", 7);
        assert_eq!(r.id.get_version_num(), 7);
        assert_eq!(r.name, "web-01");
        assert_eq!(r.revision, 0);
        assert!(r.labels.is_empty());
        assert_eq!(r.spec, 7);
        assert_eq!(r.status, DummyStatus::default());
    }

    #[test]
    fn kind_serializes_as_its_str() {
        for kind in [
            Kind::Vm,
            Kind::StoragePool,
            Kind::Volume,
            Kind::Node,
            Kind::Task,
            Kind::User,
            Kind::ApiToken,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
    }

    #[test]
    fn validates_names() {
        for ok in ["a", "web-01", "0", &"a".repeat(63)] {
            assert_eq!(validate_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "-a", "a-", "Web", "a_b", "a.b", "a b", &"a".repeat(64)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_labels_are_omitted_and_round_trip() {
        let r: Resource<u32, DummyStatus> = Resource::new("a", 1);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("labels").is_none());
        let back: Resource<u32, DummyStatus> = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
}
```

Run: `cargo test -p opl-types`
Expected: 컴파일 실패 (`validate_name`, `Kind::Node` 등이 없음)

- [ ] **Step 3: `resource.rs` 구현**

`crates/opl-types/src/resource.rs`의 테스트 모듈 앞부분 전체를 다음으로 바꾼다:

```rust
use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Resource<S, T> {
    pub id: Uuid,
    pub name: String,
    /// 마지막 수정이 커밋된 Raft 로그 인덱스. 아직 저장되지 않았으면 0.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
    pub status: T,
}

impl<S, T: Default> Resource<S, T> {
    pub fn new(name: impl Into<String>, spec: S) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            revision: 0,
            labels: BTreeMap::new(),
            spec,
            status: T::default(),
        }
    }
}

/// 클러스터 저장소가 구분하는 리소스 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Vm,
    StoragePool,
    Volume,
    Node,
    Task,
    User,
    ApiToken,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Vm => "vm",
            Kind::StoragePool => "storage-pool",
            Kind::Volume => "volume",
            Kind::Node => "node",
            Kind::Task => "task",
            Kind::User => "user",
            Kind::ApiToken => "api-token",
        }
    }
}

/// 리소스 이름 규칙: DNS 레이블처럼 1~63자의 영문 소문자, 숫자, `-`이고 `-`로 시작하거나 끝나지 않는다.
/// 저장소는 이름의 유일성만 보므로 API가 생성할 때 확인한다.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    let ok = (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(NameError(name.to_string()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "invalid name {0:?}: use 1-63 lowercase letters, digits and '-', not starting or ending with '-'"
)]
pub struct NameError(String);

/// spec 타입과 리소스 종류, status 타입을 묶는다. 클러스터 저장소의 타입 있는 API가 쓴다.
pub trait ResourceSpec: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    const KIND: Kind;
    type Status: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static;
}
```

- [ ] **Step 4: 새 리소스 모듈**

`crates/opl-types/src/node.rs`:

```rust
use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 클러스터 노드. 리소스 id는 그 노드의 Raft 노드 id와 같다.
pub type Node = Resource<NodeSpec, NodeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    /// API 주소 (`host:port`)
    pub address: String,
    /// 유지보수 중인 노드에는 스케줄러가 VM을 새로 배치하지 않는다.
    #[serde(default)]
    pub maintenance: bool,
}

impl ResourceSpec for NodeSpec {
    const KIND: Kind = Kind::Node;
    type Status = NodeStatus;
}

/// 노드가 시작할 때 스스로 기록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeStatus {
    pub online: bool,
    pub cpus: u32,
    pub memory_mib: u64,
    /// 예: `qemu-hvf`, `qemu-kvm`
    pub hypervisor: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_defaults_maintenance_and_status_defaults() {
        let spec: NodeSpec =
            serde_json::from_value(serde_json::json!({ "address": "10.0.0.1:8006" })).unwrap();
        assert!(!spec.maintenance);
        let status: NodeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(status, NodeStatus::default());
    }
}
```

`crates/opl-types/src/task.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::storage::SourceFormat;

/// 오래 걸리는 작업. API가 만들고 `node`의 작업 실행기가 실행해 status를 기록한다.
/// 리소스 이름은 id 문자열이다.
pub type Task = Resource<TaskSpec, TaskStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id
    pub target: Uuid,
    /// 작업을 실행할 노드
    pub node: Uuid,
}

impl ResourceSpec for TaskSpec {
    const KIND: Kind = Kind::Task;
    type Status = TaskStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum TaskOp {
    VmStart,
    VmStop,
    /// ACPI 없이 곧바로 끈다.
    VmForceStop,
    /// ACPI로 끈 뒤 다시 켠다.
    VmReboot,
    VolumeCreate,
    /// `source`(작업 노드의 로컬 파일)를 변환해 볼륨을 채우고, 끝나면 `source`를 지운다.
    VolumeImport {
        #[cfg_attr(feature = "openapi", schema(value_type = String))]
        source: PathBuf,
        source_format: SourceFormat,
    },
    VolumeDelete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct TaskStatus {
    pub state: TaskState,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl TaskState {
    pub fn is_finished(self) -> bool {
        matches!(self, TaskState::Succeeded | TaskState::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_is_tagged_by_kind() {
        let op = TaskOp::VolumeImport {
            source: "/var/lib/opl/uploads/x".into(),
            source_format: SourceFormat::Vmdk,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "volume-import",
                "source": "/var/lib/opl/uploads/x",
                "source_format": "vmdk"
            })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        assert_eq!(
            serde_json::to_value(TaskOp::VmForceStop).unwrap(),
            serde_json::json!({ "kind": "vm-force-stop" })
        );
    }

    #[test]
    fn new_task_is_pending() {
        assert_eq!(TaskStatus::default().state, TaskState::Pending);
        assert!(!TaskState::Running.is_finished());
        assert!(TaskState::Failed.is_finished());
    }
}
```

`crates/opl-types/src/auth.rs`:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

/// 로컬 사용자. 리소스 이름이 로그인 이름이다.
pub type User = Resource<UserSpec, UserStatus>;
/// API 토큰. 리소스 이름은 사용자가 붙인 이름이다. 비밀 값은 해시만 저장한다.
pub type ApiToken = Resource<ApiTokenSpec, ApiTokenStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// 모든 작업
    Admin,
    /// VM과 볼륨을 만들고 다룬다.
    Operator,
    /// 읽기만 한다.
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSpec {
    /// argon2 PHC 문자열
    pub password_hash: String,
    pub role: Role,
}

impl ResourceSpec for UserSpec {
    const KIND: Kind = Kind::User;
    type Status = UserStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserStatus {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiTokenSpec {
    /// 토큰 주인(User id). 권한은 주인의 역할을 따른다.
    pub user: Uuid,
    /// 비밀 값의 SHA-256 (16진수)
    pub secret_hash: String,
}

impl ResourceSpec for ApiTokenSpec {
    const KIND: Kind = Kind::ApiToken;
    type Status = ApiTokenStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApiTokenStatus {}
```

`crates/opl-types/src/storage.rs` 전체(`SourceFormat`과 `VolumePhase` 추가, 스키마 derive):

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::runtime::ImageFormat;

pub type StoragePool = Resource<StoragePoolSpec, StoragePoolStatus>;
pub type Volume = Resource<VolumeSpec, VolumeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

impl ResourceSpec for StoragePoolSpec {
    const KIND: Kind = Kind::StoragePool;
    type Status = StoragePoolStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum PoolBackend {
    /// 로컬 디렉터리에 이미지 파일을 둔다.
    Dir {
        #[cfg_attr(feature = "openapi", schema(value_type = String))]
        path: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct StoragePoolStatus {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// StoragePool 리소스 이름
    pub pool: String,
    pub size_bytes: u64,
    pub format: ImageFormat,
}

impl ResourceSpec for VolumeSpec {
    const KIND: Kind = Kind::Volume;
    type Status = VolumeStatus;
}

/// 가져올 원본 이미지 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VolumeStatus {
    pub phase: VolumePhase,
    /// 실제로 디스크에 할당된 크기
    pub allocated_bytes: Option<u64>,
    pub attached_to: Option<Uuid>,
}

/// 볼륨 파일이 준비됐는지. VM은 준비된 볼륨만 붙일 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VolumePhase {
    /// 파일을 만들거나 가져오는 중
    #[default]
    Pending,
    Ready,
}

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

`crates/opl-types/src/lib.rs` 전체:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.

pub mod auth;
pub mod node;
pub mod resource;
pub mod runtime;
pub mod storage;
pub mod task;
pub mod vm;
```

- [ ] **Step 5: 기존 타입에 스키마 derive**

`crates/opl-types/src/vm.rs`에서 다음 타입의 `#[derive(...)]` 줄 바로 아래에 `#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]`를 넣는다: `VmSpec`, `CpuSpec`, `CpuModel`, `Firmware`, `MachineType`, `DiskSpec`, `DiskBus`, `NicSpec`, `NicModel`, `PowerState`, `RestartPolicy`, `VmStatus`, `VmPhase`. `crates/opl-types/src/runtime.rs`의 `ImageFormat`에도 넣는다.

`MacAddr`는 문자열로 직렬화하므로 스키마도 문자열로 둔다. `#[derive(...)]` 아래에:

```rust
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(value_type = String))]
```

- [ ] **Step 6: `SourceFormat` 이동에 맞춰 드라이버와 클러스터 테스트 고치기**

`crates/opl-driver/src/storage.rs`에서 `/// 가져올 원본 이미지 형식` 주석과 `pub enum SourceFormat { ... }` 정의를 지우고, import를 바꾼다:

```rust
use opl_types::storage::{PoolBackend, SourceFormat, StoragePoolSpec};
```

`crates/opl-driver/src/lib.rs`의 마지막 `pub use storage::...` 줄을 다음 두 줄로 바꾼다(`pub use qemu::...` 앞에 `SourceFormat` 줄을 둔다):

```rust
pub use opl_types::storage::SourceFormat;
pub use qemu::{QemuConfig, QemuHypervisor};
pub use storage::{Capacity, DirStorage, Storage, VolumeInfo};
```

`crates/opl-cluster/tests/single_node.rs`의 `VolumeStatus { allocated_bytes: Some(4096), attached_to: None }`를 새 필드가 있어도 되게 바꾼다:

```rust
    let status = VolumeStatus {
        allocated_bytes: Some(4096),
        ..Default::default()
    };
```

- [ ] **Step 7: 통과 확인**

Run: `cargo test -p opl-types && cargo build -p opl-types --features openapi && cargo test --workspace`
Expected: `opl-types` 단위 20개 통과, `openapi` 기능으로도 빌드됨, 나머지 크레이트 그대로 통과

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: 경고 없음

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-types crates/opl-driver/src crates/opl-cluster/tests/single_node.rs
git commit -m "feat(types): Node·Task·User·ApiToken 리소스, 이름 규칙, 볼륨 준비 상태, OpenAPI 스키마"
```

---

### Task 2: 드라이버: 가져올 이미지 검사, Mock 콘솔 경로

사용자가 올린 VMDK 디스크립터는 호스트의 아무 파일이나 extent로 가리킬 수 있고, qcow2는 backing file이나 외부 데이터 파일을 가리킬 수 있다. `qemu-img convert`가 그 파일을 읽어 볼륨에 복사하면 운영자가 호스트 파일을 읽게 된다. 가져오기 전에 `qemu-img info`의 결과에서 이미지가 여는 파일이 모두 자기 자신인지 확인한다. 콘솔 테스트를 위해 Mock이 VNC 소켓 경로를 정할 수 있게 한다.

**Files:**
- Modify: `crates/opl-driver/src/storage.rs`, `crates/opl-driver/tests/dir_storage.rs`, `crates/opl-driver/src/mock.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-driver/tests/dir_storage.rs`의 `failed_import_leaves_no_partial_volume` 테스트 앞에 추가한다:

```rust
#[tokio::test]
async fn import_source_must_be_a_single_file() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = DirStorage::new("qemu-img");
    let raw = tmp.path().join("disk.raw");
    std::fs::write(&raw, vec![0u8; MIB as usize]).unwrap();
    let vmdk = tmp.path().join("disk.vmdk");
    let raw_s = raw.to_str().unwrap();
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw_s,
        vmdk.to_str().unwrap(),
    ]);
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    storage
        .check_import_source(&raw, SourceFormat::Raw)
        .await
        .unwrap();

    // monolithicFlat 디스크립터는 호스트의 임의 파일을 extent로 가리킬 수 있다.
    let desc = tmp.path().join("desc.vmdk");
    std::fs::write(
        &desc,
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
             createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"{raw_s}\" 0\n"
        ),
    )
    .unwrap();
    let err = storage
        .check_import_source(&desc, SourceFormat::Vmdk)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DriverError::InvalidSpec(ref m) if m.contains(raw_s)),
        "{err:?}"
    );

    // backing file도 다른 파일이다.
    let overlay = tmp.path().join("overlay.qcow2");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-b",
        raw_s,
        "-F",
        "raw",
        overlay.to_str().unwrap(),
    ]);
    let err = storage
        .check_import_source(&overlay, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    // qcow2 외부 데이터 파일도 마찬가지다.
    let external = tmp.path().join("external.qcow2");
    let data_file = format!("data_file={raw_s},data_file_raw=on");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-o",
        &data_file,
        external.to_str().unwrap(),
        "1M",
    ]);
    let err = storage
        .check_import_source(&external, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
}
```

`crates/opl-driver/src/mock.rs`의 테스트 모듈에서 `injected_failure_applies_once` 앞에 추가한다:

```rust
    #[tokio::test]
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }
```

Run: `cargo test -p opl-driver --features storage-it`
Expected: 컴파일 실패 (`check_import_source`, `set_console_dir`가 없음)

- [ ] **Step 2: `check_import_source` 구현**

`crates/opl-driver/src/storage.rs`의 `Storage` 트레이트에서 `import_volume` 선언 뒤(`delete_volume` 앞)에 추가한다:

```rust
    /// `src`가 다른 파일을 참조하지 않는 이미지 하나인지 확인한다(VMDK extent, backing file).
    /// 사용자가 올린 파일을 `import_volume`에 넘기기 전에 부른다.
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError>;
```

`fn path_str` 앞에 추가한다:

```rust
/// `qemu-img info` 출력에서 이미지가 여는(열 수 있는) 파일 이름을 모두 모은다.
fn referenced_files(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    (
                        "filename" | "backing-filename" | "full-backing-filename" | "data-file",
                        Value::String(f),
                    ) => out.push(f.clone()),
                    _ => referenced_files(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| referenced_files(v, out)),
        _ => {}
    }
}
```

`impl Storage for DirStorage`의 `import_volume` 뒤(`delete_volume` 앞)에 추가한다:

```rust
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        require_absolute(src)?;
        let src = path_str(src)?;
        let out = self
            .qemu_img(&["info", "--output=json", "-f", src_fmt_name(src_format), src])
            .await?;
        let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })?;
        // 이미지 자신과 그 하위 노드(extent 등)가 가리키는 파일이 모두 src여야 한다.
        let mut files = Vec::new();
        referenced_files(&json, &mut files);
        match files.into_iter().find(|f| f != src) {
            None => Ok(()),
            Some(other) => Err(DriverError::InvalidSpec(format!(
                "image refers to another file: {other}"
            ))),
        }
    }
```

- [ ] **Step 3: Mock 콘솔 경로**

`crates/opl-driver/src/mock.rs`의 `MockState`에 필드를 더한다(`unresponsive` 아래):

```rust
    console_dir: Option<PathBuf>,
```

`impl MockHypervisor`의 `fn remove` 앞에 추가한다:

```rust
    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }
```

`console_endpoint` 본문을 다음으로 바꾼다:

```rust
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-driver --features storage-it`
Expected: 단위 32개, `dir_storage` 4개 통과(`import_source_must_be_a_single_file` 포함)

- [ ] **Step 5: Commit**

```bash
git add crates/opl-driver
git commit -m "feat(driver): 다른 파일을 참조하는 가져오기 이미지 거부, Mock 콘솔 경로"
```

---

### Task 3: reconciler: 강제 종료 명령

`stop?mode=force`는 ACPI 응답을 기다리지 않는다. 드라이버 호출은 VM별 작업자에서만 일어나야 하므로(불변식 3) 디스패처에 명령 통로를 두고 작업자 큐로 전달한다. 원하는 상태가 stopped인데 아직 돌고 있으면 작업자가 곧바로 `stop(Force)`한다. 원하는 상태가 running이면(그 사이 사용자가 다시 켰으면) 무시한다.

**Files:**
- Modify: `crates/opl-reconciler/src/{worker,reconciler,lib}.rs`, `crates/opl-reconciler/tests/{common/mod,reconcile}.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-reconciler/tests/common/mod.rs`의 `impl Env`에서 `/// Mock 하이퍼바이저에서 실행 중인 VM의 pid` 주석 앞에 추가한다:

```rust
    pub fn force_stop(&self, id: Uuid) {
        self.reconciler.as_ref().unwrap().handle().force_stop(id);
    }

```

`crates/opl-reconciler/tests/reconcile.rs`의 `guest_shutdown_turns_power_off` 앞에 추가한다:

```rust
#[tokio::test]
async fn force_stop_does_not_wait_for_acpi() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    // Mock 게스트는 ACPI를 무시한다. 강제 종료가 없으면 60초 뒤에야 꺼진다.
    env.force_stop(vm.id);

    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn force_stop_is_ignored_while_power_is_running() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.force_stop(vm.id);
    // 명령이 처리될 시간을 준다.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(env.pid(vm.id).await, before);
    assert_eq!(env.get(vm.id).status.phase, VmPhase::Running);
}
```

Run: `cargo test -p opl-reconciler --test reconcile`
Expected: 컴파일 실패 (`Reconciler::handle`이 없음)

- [ ] **Step 2: 작업자**

`crates/opl-reconciler/src/worker.rs`:

1. `Trigger`에 변형을 더한다(`Event(EventKind),` 아래):

```rust
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
```

2. `Worker`에 필드를 더하고(`stopped_by_us` 아래) `Worker::new`에서 `force_requested: false`로 초기화한다:

```rust
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 쓴다.
    force_requested: bool,
```

3. `step`의 `if let Trigger::Event(kind) = trigger { self.on_event(&vm, kind); }`를 바꾼다:

```rust
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            Trigger::ForceStop => self.force_requested = true,
            Trigger::Sync => {}
        }
```

4. `reconcile`에서 원하는 상태가 running인 두 갈래(`(PowerState::Running, Some(state))`와 `(PowerState::Running, None)`)와 `(PowerState::Stopped, None)` 갈래의 첫 줄들 옆에 `self.force_requested = false;`를 넣는다. 각각 `self.stop_requested = None;`, `self.awaiting_event = None;`, `self.stop_requested = None;` 바로 아래다.

5. `(PowerState::Stopped, Some(state))` 갈래의 `match self.stop_requested {` 바로 아래, `None =>` 앞에 첫 갈래를 넣는다:

```rust
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
```

- [ ] **Step 3: 디스패처와 핸들**

`crates/opl-reconciler/src/reconciler.rs`:

1. `Reconciler`에 필드 `commands: mpsc::UnboundedSender<Uuid>,`를 더하고, 구조체 정의 뒤에 핸들을 둔다:

```rust
/// 실행 중인 reconciler에 VM별 일회성 명령을 보낸다. 명령은 그 VM의 작업자 큐를 거치므로
/// 같은 VM에 대한 드라이버 호출이 겹치지 않는다.
#[derive(Clone)]
pub struct ReconcilerHandle {
    commands: mpsc::UnboundedSender<Uuid>,
}

impl ReconcilerHandle {
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다. 원하는 상태를 먼저
    /// stopped로 기록한 뒤에 부른다. 이 노드에 배정되지 않은 VM이거나 원하는 상태가 running이면
    /// 아무것도 하지 않는다. 결과는 VM status로 확인한다.
    pub fn force_stop(&self, id: Uuid) {
        let _ = self.commands.send(id);
    }
}
```

2. `Reconciler::spawn`에서 `let (shutdown, rx) = oneshot::channel();` 아래에 `let (commands, command_rx) = mpsc::unbounded_channel();`을 넣고, 반환값을 바꾼다:

```rust
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx, command_rx)),
            commands,
        }
    }

    pub fn handle(&self) -> ReconcilerHandle {
        ReconcilerHandle {
            commands: self.commands.clone(),
        }
    }
```

3. `Dispatcher::run`의 시그니처를 바꾸고:

```rust
    async fn run(
        mut self,
        mut shutdown: oneshot::Receiver<()>,
        mut commands: mpsc::UnboundedReceiver<Uuid>,
    ) {
```

안쪽 `tokio::select!`의 하이퍼바이저 이벤트 갈래 뒤에 갈래를 더한다:

```rust
                    Some(id) = commands.recv() => {
                        if !self.notify(id, Trigger::ForceStop) {
                            tracing::warn!(vm = %id, "force stop for a VM not assigned to this node");
                        }
                    }
```

`crates/opl-reconciler/src/lib.rs`의 `pub use reconciler::Reconciler;`를 `pub use reconciler::{Reconciler, ReconcilerHandle};`로 바꾼다.

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-reconciler`
Expected: 단위 2개, `reconcile` 19개, `resolve` 2개, `no_quorum` 1개 통과

- [ ] **Step 5: Commit**

```bash
git add crates/opl-reconciler
git commit -m "feat(reconciler): 작업자 큐를 거치는 강제 종료 명령"
```

---

### Task 4: `opl-api` 핵심: 오류, 상태, 인증, 스케줄러

HTTP 라우트 없이 API의 바탕을 만든다.

- **오류(`error.rs`):** `ApiError`와 본문 `{code, message, details}`, `ClusterError` 변환(이어받은 매핑), 본문·쿼리 오류도 같은 형식으로 돌려주는 `Json`/`Query` 추출기.
- **인증(`auth.rs`):** 비밀번호(argon2id, 블로킹 스레드), API 토큰(`opl_<id>_<secret>`, SHA-256, 상수 시간 비교), 메모리 세션, 쿠키 요청의 `Origin` 확인, `Principal` 추출기, `Authorizer` 훅과 고정 역할.
- **스케줄러(`scheduler.rs`):** 설계 3.3을 순수 함수로 둔다.

| 역할 | 읽기 | 쓰기·동작 |
|---|---|---|
| admin | 전부 | 전부 |
| operator | 사용자·토큰 빼고 전부 | VM, 볼륨 |
| viewer | 사용자·토큰 빼고 전부 | 없음 |

자기 API 토큰은 역할과 관계없이 다룬다(Task 6의 토큰 핸들러가 확인한다).

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/opl-api/Cargo.toml`, `crates/opl-api/src/{lib,error,state,auth,scheduler}.rs`

- [ ] **Step 1: 의존성**

루트 `Cargo.toml`에서 tokio 기능에 `"fs", "signal"`을 더하고(`"process"` 뒤), `[workspace.dependencies]`의 `opl-driver = ...` 아래와 끝에 추가한다:

```toml
opl-reconciler = { path = "crates/opl-reconciler" }
opl-api = { path = "crates/opl-api" }
```

```toml
axum = { version = "0.8.9", features = ["ws", "macros"] }
argon2 = "0.6.0"
sha2 = "0.11.1"
getrandom = "0.4.3"
hex = "0.4.3"
futures-util = "0.3.34"
reqwest = { version = "0.13.5", default-features = false, features = ["json", "stream", "query"] }
tokio-tungstenite = "0.29.0"
utoipa-axum = "0.3.0"
```

`tokio-tungstenite`는 axum이 쓰는 0.29에 맞춘다(0.30을 쓰면 두 버전이 함께 빌드된다).

`crates/opl-api/Cargo.toml`:

```toml
[package]
name = "opl-api"
description = "REST API, tasks and authentication for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# qemu-img로 볼륨을 만드는 API 테스트 (macOS: brew install qemu, Debian/Ubuntu: apt install qemu-utils)
storage-it = []

[dependencies]
argon2.workspace = true
axum.workspace = true
fs4.workspace = true
futures-util.workspace = true
getrandom.workspace = true
hex.workspace = true
opl-cluster.workspace = true
opl-driver.workspace = true
opl-reconciler.workspace = true
opl-types = { workspace = true, features = ["openapi"] }
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
thiserror.workspace = true
tokio.workspace = true
tracing.workspace = true
utoipa.workspace = true
utoipa-axum.workspace = true
uuid.workspace = true

[dev-dependencies]
openraft.workspace = true
reqwest.workspace = true
tempfile.workspace = true
tokio-tungstenite.workspace = true
```

- [ ] **Step 2: 오류와 상태**

`crates/opl-api/src/error.rs`:

```rust
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

/// API 오류. 응답 본문은 `{ "code": "...", "message": "...", "details": {...} }`이다(설계 4절).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "permission denied")
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
    }
}

/// 오류 응답 본문
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// 기계가 읽는 오류 종류. 예: `not_found`, `revision_mismatch`, `cluster_no_quorum`
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code.to_string(),
            message: self.message,
            details: self.details,
        };
        (self.status, axum::Json(body)).into_response()
    }
}

impl From<ClusterError> for ApiError {
    fn from(e: ClusterError) -> Self {
        match e {
            ClusterError::Rejected(r) => {
                let message = r.to_string();
                match r {
                    Rejection::AlreadyExists { .. } => {
                        ApiError::conflict("already_exists", message)
                    }
                    Rejection::NameTaken { .. } => ApiError::conflict("name_taken", message),
                    Rejection::NotFound { .. } => ApiError::not_found(message),
                    Rejection::RevisionMismatch { .. } => ApiError::new(
                        StatusCode::PRECONDITION_FAILED,
                        "revision_mismatch",
                        message,
                    ),
                }
            }
            ClusterError::Timeout => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_no_quorum",
                "the cluster has no quorum; the change may still be applied later",
            ),
            ClusterError::NotLeader {
                leader_id,
                leader_addr,
            } => {
                let mut e = ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "not_leader",
                    "this node is not the cluster leader",
                );
                e.details = Some(serde_json::json!({
                    "leader_id": leader_id.map(|id| id.0),
                    "leader_addr": leader_addr,
                }));
                e
            }
            other => ApiError::internal(other),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

impl From<WebSocketUpgradeRejection> for ApiError {
    fn from(e: WebSocketUpgradeRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

/// 본문 오류도 API 오류 형식으로 돌려주는 JSON 추출기·응답.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// 쿼리 오류도 API 오류 형식으로 돌려주는 추출기.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);
```

`crates/opl-api/src/state.rs`:

```rust
use std::path::PathBuf;
use std::sync::Arc;

use opl_cluster::Cluster;
use opl_driver::Hypervisor;
use uuid::Uuid;

use crate::auth::{Authorizer, RoleAuthorizer, Sessions};

/// 모든 핸들러가 공유하는 노드의 상태.
pub struct AppState {
    pub cluster: Arc<Cluster>,
    /// 이 노드의 id
    pub node: Uuid,
    pub hv: Arc<dyn Hypervisor>,
    pub authorizer: Arc<dyn Authorizer>,
    /// 올린 이미지를 가져오기 전에 잠시 두는 디렉터리. 볼륨 풀과 같은 파일시스템이면 좋다.
    pub upload_dir: PathBuf,
    pub(crate) sessions: Sessions,
}

impl AppState {
    pub fn new(
        cluster: Arc<Cluster>,
        node: Uuid,
        hv: Arc<dyn Hypervisor>,
        upload_dir: PathBuf,
    ) -> Self {
        Self {
            cluster,
            node,
            hv,
            authorizer: Arc::new(RoleAuthorizer),
            upload_dir,
            sessions: Sessions::default(),
        }
    }
}
```

`crates/opl-api/src/lib.rs` (Task 5~9에서 모듈과 라우트를 더한다):

```rust
//! opLite REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod auth;
pub mod error;
pub mod scheduler;
mod state;

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
```

- [ ] **Step 3: 인증과 스케줄러 테스트 작성**

`crates/opl-api/src/auth.rs`와 `crates/opl-api/src/scheduler.rs`를 테스트 모듈만 담아 만든다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn principal(role: Role) -> Principal {
        Principal {
            user: Uuid::nil(),
            name: "u".into(),
            role,
        }
    }

    #[test]
    fn roles_grant_expected_actions() {
        let a = RoleAuthorizer;
        let viewer = principal(Role::Viewer);
        assert!(a.check(&viewer, Action::Read, Kind::Vm));
        assert!(!a.check(&viewer, Action::Operate, Kind::Vm));
        assert!(!a.check(&viewer, Action::Read, Kind::User));

        let op = principal(Role::Operator);
        assert!(a.check(&op, Action::Write, Kind::Vm));
        assert!(a.check(&op, Action::Operate, Kind::Vm));
        assert!(a.check(&op, Action::Write, Kind::Volume));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
        assert!(!a.check(&op, Action::Write, Kind::Node));
        assert!(!a.check(&op, Action::Read, Kind::ApiToken));

        let admin = principal(Role::Admin);
        assert!(a.check(&admin, Action::Write, Kind::User));
    }

    #[test]
    fn token_round_trips_and_rejects_garbage() {
        let id = Uuid::now_v7();
        let (hash, token) = new_token(id);
        let (parsed, secret) = parse_token(&token).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(sha256_hex(secret), hash);
        for bad in ["", "opl_", "opl_xyz_abc", "xyz", &token[4..]] {
            assert!(parse_token(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn compares_in_constant_time_shape() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }

    #[tokio::test]
    async fn verifies_passwords() {
        let hash = hash_password("correct horse".into()).await.unwrap();
        assert!(verify_password("correct horse".into(), hash.clone()).await);
        assert!(!verify_password("wrong".into(), hash).await);
        assert!(!verify_password("x".into(), "not a hash".into()).await);
    }

    #[test]
    fn sessions_can_be_removed() {
        let s = Sessions::default();
        let user = Uuid::now_v7();
        let id = s.create(user);
        assert_eq!(s.get(&id), Some(user));
        s.remove(&id);
        assert_eq!(s.get(&id), None);
        assert_eq!(s.get("unknown"), None);

        let (a, b) = (s.create(user), s.create(user));
        let other = s.create(Uuid::now_v7());
        s.remove_user(user);
        assert_eq!((s.get(&a), s.get(&b)), (None, None));
        assert!(s.get(&other).is_some());
    }

    #[test]
    fn checks_origin_against_host() {
        fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, v.parse().unwrap());
            }
            h
        }
        assert!(same_origin(&headers(&[("host", "127.0.0.1:8006")])));
        assert!(same_origin(&headers(&[
            ("host", "127.0.0.1:8006"),
            ("origin", "http://127.0.0.1:8006")
        ])));
        assert!(!same_origin(&headers(&[
            ("host", "127.0.0.1:8006"),
            ("origin", "http://127.0.0.1:9000")
        ])));
        assert!(!same_origin(&headers(&[("origin", "null")])));
    }
}
```

```rust
#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{CpuSpec, DiskBus, DiskSpec, MachineType};

    use super::*;

    fn node(memory_mib: u64) -> Node {
        let mut n = Resource::new(
            "n",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        n.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib,
            hypervisor: "mock".into(),
        };
        n
    }

    fn spec(memory_mib: u32, node: Option<Uuid>, power: PowerState) -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
            },
            memory_mib,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power,
            restart: Default::default(),
            node,
        }
    }

    #[test]
    fn picks_node_with_most_unassigned_memory() {
        let (a, b) = (node(8192), node(4096));
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        let me = spec(1024, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(a.id));

        // a에 6 GiB를 쓰는 VM이 돌면 b가 더 비어 있다. 꺼진 VM은 세지 않는다.
        inv.vms.push(Resource::new(
            "big",
            spec(6144, Some(a.id), PowerState::Running),
        ));
        inv.vms.push(Resource::new(
            "off",
            spec(6144, Some(b.id), PowerState::Stopped),
        ));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(b.id));
    }

    #[test]
    fn skips_offline_maintenance_and_unreachable_pools() {
        let mut offline = node(64000);
        offline.status.online = false;
        let mut maint = node(64000);
        maint.spec.maintenance = true;
        let far = node(32000);
        let near = node(1024);
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pool".into(),
                },
                nodes: vec![near.id],
                shared: false,
            },
        );
        let volume = Resource::new(
            "root",
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
            },
        );
        let inv = Inventory {
            nodes: vec![offline, maint, far, near.clone()],
            vms: vec![],
            volumes: vec![volume],
            pools: vec![pool],
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(near.id));

        let empty = Inventory::default();
        assert_eq!(schedule(Uuid::now_v7(), &me, &empty), Err(NoNode));
    }
}
```

Run: `cargo test -p opl-api`
Expected: 컴파일 실패 (`RoleAuthorizer`, `schedule` 등이 없음)

- [ ] **Step 4: 구현**

`crates/opl-api/src/auth.rs`의 테스트 모듈 앞에:

```rust
//! 인증(로컬 사용자, 세션 쿠키, API 토큰)과 권한 확인.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use opl_types::auth::{ApiTokenSpec, Role, UserSpec};
use opl_types::resource::Kind;
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "opl_session";
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
const TOKEN_PREFIX: &str = "opl_";

/// 인증된 요청의 주체. 요청마다 저장소의 User에서 다시 읽으므로 역할 변경과 삭제가 곧바로 반영된다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Principal {
    pub user: Uuid,
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    /// 생성, 수정, 삭제
    Write,
    /// 시작·정지 같은 동작과 콘솔
    Operate,
}

/// 모든 핸들러가 거치는 권한 확인 훅(설계 4절). 엔터프라이즈 플러그인이 세분화된 RBAC로 바꾼다.
pub trait Authorizer: Send + Sync {
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool;
}

/// 고정 역할 3개. 자기 API 토큰은 역할과 관계없이 누구나 다룬다(토큰 핸들러가 따로 확인한다).
pub struct RoleAuthorizer;

impl Authorizer for RoleAuthorizer {
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool {
        let account = matches!(kind, Kind::User | Kind::ApiToken);
        match principal.role {
            Role::Admin => true,
            Role::Operator => match action {
                Action::Read => !account,
                Action::Write | Action::Operate => matches!(kind, Kind::Vm | Kind::Volume),
            },
            Role::Viewer => action == Action::Read && !account,
        }
    }
}

/// argon2id로 해시한다. CPU를 오래 쓰므로 블로킹 스레드에서 돈다.
pub async fn hash_password(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::internal)
}

/// 해시가 깨졌으면 false다.
pub async fn verify_password(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}

/// 존재하지 않는 사용자로 로그인할 때도 같은 시간이 걸리도록 비교에 쓰는 해시.
pub(crate) async fn dummy_hash() -> String {
    static HASH: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    HASH.get_or_init(|| async {
        hash_password(random_hex(16))
            .await
            .expect("hashing a random password")
    })
    .await
    .clone()
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("system random source");
    hex::encode(buf)
}

pub(crate) fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// 새 토큰의 (비밀 값 해시, 사용자에게 한 번만 보여 줄 토큰 문자열)
pub(crate) fn new_token(id: Uuid) -> (String, String) {
    let secret = random_hex(32);
    let hash = sha256_hex(&secret);
    (hash, format!("{TOKEN_PREFIX}{}_{secret}", id.simple()))
}

/// `opl_<id>_<secret>`을 나눈다.
fn parse_token(token: &str) -> Option<(Uuid, &str)> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    Some((Uuid::try_parse(id).ok()?, secret))
}

/// 길이가 같은 두 문자열을 내용과 관계없이 같은 시간에 비교한다.
fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 노드 메모리에만 두는 세션. opld가 재시작하면 다시 로그인한다.
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, (Uuid, Instant)>>,
}

impl Sessions {
    pub fn create(&self, user: Uuid) -> String {
        let id = random_hex(32);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, (_, expires)| *expires > now);
        map.insert(id.clone(), (user, now + SESSION_TTL));
        id
    }

    pub fn get(&self, id: &str) -> Option<Uuid> {
        let map = self.inner.lock().unwrap();
        map.get(id)
            .filter(|(_, expires)| *expires > Instant::now())
            .map(|(user, _)| *user)
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    /// 사용자의 세션을 모두 끝낸다(비밀번호를 바꿨을 때).
    pub fn remove_user(&self, user: Uuid) {
        self.inner.lock().unwrap().retain(|_, (u, _)| *u != user);
    }
}

pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(SESSION_COOKIE)?.strip_prefix('='))
}

/// `Origin`이 없거나(브라우저 밖의 클라이언트) `Host`와 같다.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let origin = origin.to_str().unwrap_or_default();
    let origin_host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"));
    let host = headers.get(HOST).and_then(|h| h.to_str().ok());
    origin_host.is_some() && origin_host == host
}

impl AppState {
    fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(|u| Principal {
            user: u.id,
            name: u.name,
            role: u.spec.role,
        }))
    }

    async fn authenticate(&self, parts: &Parts) -> Result<Principal, ApiError> {
        if let Some(value) = parts.headers.get(AUTHORIZATION) {
            let token = value
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(ApiError::unauthorized)?;
            let (id, secret) = parse_token(token.trim()).ok_or_else(ApiError::unauthorized)?;
            let stored = self
                .cluster
                .get::<ApiTokenSpec>(id)?
                .ok_or_else(ApiError::unauthorized)?;
            if !constant_time_eq(&sha256_hex(secret), &stored.spec.secret_hash) {
                return Err(ApiError::unauthorized());
            }
            return self
                .principal_for(stored.spec.user)?
                .ok_or_else(ApiError::unauthorized);
        }
        let user = session_cookie(&parts.headers)
            .and_then(|id| self.sessions.get(id))
            .ok_or_else(ApiError::unauthorized)?;
        // 쿠키는 브라우저가 알아서 붙이므로, 다른 출처의 페이지가 보낸 요청(WebSocket 포함)은 받지 않는다.
        if !same_origin(&parts.headers) {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "cross_origin",
                "cookie-authenticated requests must come from the same origin",
            ));
        }
        self.principal_for(user)?.ok_or_else(ApiError::unauthorized)
    }

    /// 권한이 없으면 403
    pub fn authorize(
        &self,
        principal: &Principal,
        action: Action,
        kind: Kind,
    ) -> Result<(), ApiError> {
        if self.authorizer.check(principal, action, kind) {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }
}

impl FromRequestParts<Arc<AppState>> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        state.authenticate(parts).await
    }
}
```

`crates/opl-api/src/scheduler.rs`의 테스트 모듈 앞에:

```rust
//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "no node can run this VM (needs an online node that is not in maintenance and can reach its storage pools)"
)]
pub struct NoNode;

/// 스케줄러가 보는 클러스터 상태
#[derive(Debug, Default)]
pub struct Inventory {
    pub nodes: Vec<Node>,
    pub vms: Vec<Vm>,
    pub volumes: Vec<Volume>,
    pub pools: Vec<StoragePool>,
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        Ok(Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
        })
    }

    /// 이 VM의 디스크가 있는 풀을 모두 쓸 수 있는 노드인지
    fn reaches_disks(&self, spec: &VmSpec, node: Uuid) -> bool {
        spec.disks.iter().all(|disk| {
            let pool = self
                .volumes
                .iter()
                .find(|v| v.name == disk.volume)
                .and_then(|v| self.pools.iter().find(|p| p.name == v.spec.pool));
            pool.is_none_or(|p| p.spec.nodes.is_empty() || p.spec.nodes.contains(&node))
        })
    }

    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리
    fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
            .vms
            .iter()
            .filter(|v| v.id != vm && v.spec.node == Some(node.id))
            .filter(|v| v.spec.power == PowerState::Running)
            .map(|v| i64::from(v.spec.memory_mib))
            .sum();
        node.status.memory_mib as i64 - used
    }
}

/// 온라인이고 유지보수 중이 아니며 디스크의 풀을 쓸 수 있는 노드 가운데 미할당 메모리가 가장
/// 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은 범위 밖).
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
    inv.nodes
        .iter()
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| inv.reaches_disks(spec, n.id))
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
        .ok_or(NoNode)
}
```

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p opl-api`
Expected: 단위 8개 통과. `dummy_hash`와 `new_token`을 아직 쓰지 않아 dead_code 경고가 2개 나는데, Task 6에서 사라진다.

Run: `cargo fmt --all -- --check && cargo deny check`
Expected: 통과 (새 의존성의 라이선스 포함)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opl-api
git commit -m "feat(api): 오류 형식, 인증(argon2, API 토큰, 세션), 역할, 스케줄러"
```

---

### Task 5: Task 실행기

이 노드에 배정된 Task를 저장소에서 받아 실행하고 status를 기록한다(설계 결정 참고). 테스트는 HTTP 서버가 생기는 Task 6, 7에서 한다.

| Task | 하는 일 | 성공 | 실패 |
|---|---|---|---|
| `vm-start` | 기다린다 | power·phase가 running | reconciler가 포기해 power가 stopped |
| `vm-stop` | 기다린다 | power stopped, phase stopped/failed | 사용자가 다시 켬 |
| `vm-force-stop` | `ReconcilerHandle::force_stop` 후 기다린다 | 위와 같다 | 위와 같다 |
| `vm-reboot` | power를 stopped → 꺼지면 running | 다시 running | 그 사이 같은 VM에 새 요청(`superseded`) |
| `volume-create` | `Storage::create_volume` → Ready | 파일이 생김 | 볼륨 리소스도 지운다 |
| `volume-import` | 원본 검사 → `import_volume` → 원본 삭제 → Ready | 위와 같다 | 원본과 볼륨을 지운다 |
| `volume-delete` | VM이 쓰지 않으면 파일과 리소스 삭제 | 지워짐 | VM이 붙어 있음 |

- **차례와 제한 시간:** 볼륨 Task는 같은 볼륨의 앞 Task가 끝난 뒤에 시작한다. VM Task는 기다리기만 하므로 차례를 두지 않는다. 강제 종료가 진행 중인 reboot 뒤에 묶이면 안 된다. Task 하나는 `timeout`(기본 10분)을 넘기면 실패한다.
- **처음 목록을 읽을 때(opld 시작):**
  - 자기 노드의 Running Task는 이전 프로세스의 것이므로 "interrupted"로 끝낸다.
  - Task 없는 Pending 볼륨을 지운다. 볼륨을 만든 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽은 경우다.
  - Task 없는 업로드 파일도 지운다. 그래서 실행기는 API 서버보다 먼저 띄운다.

**Files:**
- Create: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/lib.rs`

- [ ] **Step 1: 구현**

`crates/opl-api/src/tasks.rs`:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Storage};
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Debug, Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
}

impl TaskRunner {
    pub fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            reconciler,
            config,
        });
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    /// 시작할 때 고아(Task 없는 Pending 볼륨, 올린 파일)를 지우므로 API 서버보다 먼저 띄운다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>) {
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 볼륨별 마지막 Task. 같은 볼륨의 Task는 앞의 것이 끝난 뒤에 시작한다.
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        if first {
            sweep_orphans(&ctx, &tasks).await;
        }
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

/// Task를 실행한다. 볼륨 Task는 같은 볼륨의 앞 Task가 끝난 뒤에 시작한다(`Storage`의 요구).
/// VM Task는 기다리기만 하므로 차례를 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let target = task.spec.target;
    let queued = is_volume_op(&task.spec.op);
    let previous = if queued {
        by_target.remove(&target)
    } else {
        None
    };
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error).await;
        } else {
            execute(&ctx, task).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if queued {
        by_target.insert(target, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    set_status(ctx, task.id, TaskState::Running, None).await;
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", ctx.config.timeout)),
    };
    match result {
        Ok(()) => set_status(ctx, task.id, TaskState::Succeeded, None).await,
        Err(e) => fail(ctx, &task, e).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨은 지운다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    cleanup(ctx, task).await;
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
async fn cleanup(ctx: &Ctx, task: &Task) {
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

async fn perform(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
    }
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM에 대한 동작 Task가 만들어졌는지. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks
        .iter()
        .any(|t| t.spec.target == task.spec.target && t.id > task.id && !is_volume_op(&t.spec.op)))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePoolSpec) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool.spec, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool.spec).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool.spec, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool.spec).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(format!("volume is attached to VM {:?}", vm.name));
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool.spec, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 실행할 Task가 없는 Pending 볼륨(볼륨을 만든 뒤 Task를 만들기
/// 전에, 또는 업로드 도중 opld가 죽었다)과, 가져오기 Task가 없는 올린 파일.
async fn sweep_orphans(ctx: &Ctx, tasks: &[Task]) {
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    if let Err(e) = remove_volume(ctx, volume.id).await {
                        tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume");
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let wanted = unfinished
            .iter()
            .any(|t| matches!(&t.spec.op, TaskOp::VolumeImport { source, .. } if *source == path));
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
}
```

`crates/opl-api/src/lib.rs`의 `mod state;` 아래에 `mod tasks;`를, `pub use state::AppState;` 아래에 다음을 추가한다:

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
```

- [ ] **Step 2: 빌드 확인**

Run: `cargo build -p opl-api`
Expected: 성공. `create_task`를 아직 쓰지 않아 dead_code 경고가 하나 더 나는데, Task 7에서 사라진다.

- [ ] **Step 3: Commit**

```bash
git add crates/opl-api
git commit -m "feat(api): 노드별 Task 실행기 (VM 동작 대기, 볼륨 작업, 재시작 처리)"
```

---

### Task 6: HTTP 서버: 로그인, 사용자, 토큰, 노드, 풀, Task 조회

라우터와 OpenAPI 문서를 `utoipa-axum`의 `OpenApiRouter`로 함께 만든다. 핸들러마다 `#[utoipa::path]`를 달고 `routes!`로 등록하면 문서에도 들어간다. 종류별 목록·조회 핸들러는 매크로(`list_and_get!`)로 만든다. 제네릭 핸들러는 경로마다 문서가 따로 있어야 하는 utoipa와 맞지 않기 때문이다.

- **경로:** `{key}`는 id나 이름이다.
- **수정:** `PUT`은 `If-Match: <revision>`이 있으면 확인한다.
- **로그인:** 세션 쿠키(`HttpOnly; SameSite=Strict`)를 준다.
- **사용자 관리:**
  - 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하다. 바꾸면 그 사용자의 세션이 모두 끝난다.
  - 마지막 관리자는 역할을 내려놓을 수 없다.
  - 사용자를 지우면 그 사용자의 토큰도 지운다.
- **응답:** 비밀번호 해시와 토큰 해시는 내보내지 않는다.

테스트 하네스는 노드 하나짜리 클러스터, Mock 하이퍼바이저, reconciler(짧은 시간 설정), Task 실행기를 띄우고 그 위에 실제 HTTP 서버(임의 포트)를 연다. 관리자는 저장소에 바로 만든다.

**Files:**
- Create: `crates/opl-api/src/{resources,users}.rs`, `crates/opl-api/tests/common/mod.rs`, `crates/opl-api/tests/auth.rs`
- Modify: `crates/opl-api/src/lib.rs`

- [ ] **Step 1: 하네스와 실패하는 테스트 작성**

`crates/opl-api/tests/common/mod.rs`:

```rust
//! 노드 하나짜리 클러스터, Mock 하이퍼바이저, reconciler, Task 실행기 위에 API 서버를 띄우는 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DirStorage, Hypervisor, MockHypervisor, Storage};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

pub const WAIT: Duration = Duration::from_secs(10);
pub const ADMIN_PASSWORD: &str = "admin-password";

pub struct Env {
    pub cluster: Arc<Cluster>,
    pub hv: Arc<MockHypervisor>,
    pub node: Uuid,
    pub base: String,
    pub dir: TempDir,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    server: tokio::task::JoinHandle<()>,
}

pub fn reconciler_config(node: Uuid) -> ReconcilerConfig {
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

/// API 서버와 같은 업로드 디렉터리를 쓰는 Task 실행기 설정
pub fn runner_config(node: Uuid, dir: &Path) -> TaskRunnerConfig {
    let mut config = TaskRunnerConfig::new(node);
    config.poll_interval = Duration::from_millis(20);
    config.upload_dir = Some(dir.join("uploads"));
    config
}

impl Env {
    pub async fn start() -> Self {
        Self::start_with(|_| {}, |_| async {}).await
    }

    /// `tweak`로 reconciler 설정을 바꾸고, `before_runner`로 Task 실행기가 뜨기 전에 저장소를 채운다.
    pub async fn start_with<F, Fut>(
        tweak: impl FnOnce(&mut ReconcilerConfig),
        before_runner: F,
    ) -> Self
    where
        F: FnOnce(Arc<Cluster>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let dir = TempDir::new().unwrap();
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let mut config = ClusterConfig::new(id, dir.path().join("cluster"));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 300,
            election_timeout_max: 600,
            ..Default::default()
        };
        let cluster = Arc::new(Cluster::open(config, router.network(id)).await.unwrap());
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(id.to_string()))]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(WAIT))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        let node = id.0;

        let mut me = Resource::new(
            "node1",
            NodeSpec {
                address: "127.0.0.1:0".into(),
                maintenance: false,
            },
        );
        me.id = node;
        me.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
        };
        cluster.create(&me).await.unwrap();
        let admin = Resource::new(
            "admin",
            UserSpec {
                password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
                role: Role::Admin,
            },
        );
        cluster.create(&admin).await.unwrap();
        before_runner(cluster.clone()).await;

        let hv = Arc::new(MockHypervisor::default());
        let mut rc = reconciler_config(node);
        tweak(&mut rc);
        let hv_dyn: Arc<dyn Hypervisor> = hv.clone();
        let reconciler = Reconciler::spawn(cluster.clone(), hv_dyn.clone(), rc);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            reconciler.handle(),
            runner_config(node, dir.path()),
        );
        let state = Arc::new(AppState::new(
            cluster.clone(),
            node,
            hv_dyn,
            dir.path().join("uploads"),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = opl_api::router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Env {
            cluster,
            hv,
            node,
            base,
            dir,
            reconciler: Some(reconciler),
            runner: Some(runner),
            server,
        }
    }

    /// 인증 없는 클라이언트
    pub fn anonymous(&self) -> Client {
        Client {
            base: self.base.clone(),
            http: reqwest::Client::new(),
            token: None,
        }
    }

    /// 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
    pub async fn login(&self, user: &str, password: &str) -> Client {
        let http = reqwest::Client::new();
        let resp = http
            .post(format!("{}/api/v1/auth/login", self.base))
            .json(&json!({ "username": user, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "login as {user}");
        let cookie = resp
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let resp = http
            .post(format!("{}/api/v1/tokens", self.base))
            .header("cookie", cookie)
            .json(&json!({ "name": format!("{user}-{}", Uuid::new_v4().simple()) }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body: Value = resp.json().await.unwrap();
        Client {
            base: self.base.clone(),
            http,
            token: Some(body["secret"].as_str().unwrap().to_string()),
        }
    }

    pub async fn admin(&self) -> Client {
        self.login("admin", ADMIN_PASSWORD).await
    }

    /// 관리자로 사용자를 만들고 그 사용자로 로그인한다.
    pub async fn user(&self, name: &str, role: &str) -> Client {
        let admin = self.admin().await;
        let (status, body) = admin
            .post(
                "/users",
                json!({ "name": name, "password": "password-1", "role": role }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        self.login(name, "password-1").await
    }

    pub async fn stop_runner(&mut self) {
        self.runner.take().unwrap().shutdown().await;
    }

    pub fn start_runner(&mut self) {
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
        self.runner = Some(TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            self.reconciler.as_ref().unwrap().handle(),
            runner_config(self.node, self.dir.path()),
        ));
    }

    pub async fn pid(&self, id: Uuid) -> Option<u32> {
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
    pub token: Option<String>,
}

impl Client {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    pub async fn send(&self, r: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let resp = r.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, body)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).json(&body))
            .await
    }

    pub async fn put(&self, path: &str, body: Value, if_match: Option<u64>) -> (StatusCode, Value) {
        let mut r = self.req(reqwest::Method::PUT, path).json(&body);
        if let Some(rev) = if_match {
            r = r.header("if-match", rev.to_string());
        }
        self.send(r).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::DELETE, path)).await
    }

    pub async fn upload(&self, path: &str, body: Vec<u8>) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).body(body))
            .await
    }

    /// Task가 끝날 때까지 기다려 마지막 모습을 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> Value {
        let id = task["id"].as_str().expect("task id");
        let deadline = Instant::now() + WAIT;
        loop {
            let (status, task) = self.get(&format!("/tasks/{id}")).await;
            assert_eq!(status, StatusCode::OK, "{task}");
            let state = task["status"]["state"].as_str().unwrap_or("pending");
            if state == "succeeded" || state == "failed" {
                return task;
            }
            assert!(Instant::now() < deadline, "task did not finish: {task}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 동작을 요청하고 Task가 끝날 때까지 기다린다.
    pub async fn act(&self, vm: &str, action: &str) -> Value {
        let (status, task) = self
            .post(&format!("/vms/{vm}/actions/{action}"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        self.wait_task(&task).await
    }
}

/// 디스크 없는 작은 VM 생성 요청
pub fn vm_request(name: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "virt",
            "nics": [ { "network": "user", "model": "virtio" } ]
        }
    })
}
```

`crates/opl-api/tests/auth.rs`:

```rust
//! 로그인, 세션, API 토큰, 역할 권한.

mod common;

use common::{ADMIN_PASSWORD, Env};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn requests_without_credentials_are_rejected() {
    let env = Env::start().await;
    let anon = env.anonymous();
    let (status, body) = anon.get("/nodes").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");

    let mut forged = anon.clone();
    forged.token = Some(format!("opl_{}_deadbeef", uuid::Uuid::now_v7().simple()));
    assert_eq!(forged.get("/nodes").await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_checks_password_and_session_cookie_works() {
    let env = Env::start().await;
    let http = reqwest::Client::new();
    let url = |p: &str| format!("{}/api/v1{p}", env.base);
    for (user, password) in [("admin", "wrong-password"), ("nobody", ADMIN_PASSWORD)] {
        let resp = http
            .post(url("/auth/login"))
            .json(&json!({ "username": user, "password": password }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "invalid_credentials");
    }

    let resp = http
        .post(url("/auth/login"))
        .json(&json!({ "username": "admin", "password": ADMIN_PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set_cookie = resp.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    let who: serde_json::Value = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(who["name"], "admin");
    assert_eq!(who["role"], "admin");
    // 다른 출처의 페이지가 쿠키로 보낸 요청은 거부한다.
    let resp = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .header("origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = http
        .post(url("/auth/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = http
        .get(url("/auth/whoami"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn roles_limit_what_users_can_do() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;

    let pool = json!({
        "name": "local",
        "spec": { "backend": { "kind": "dir", "path": "/tmp/pool" } }
    });
    assert_eq!(viewer.get("/storage-pools").await.0, StatusCode::OK);
    let (status, body) = viewer.post("/storage-pools", pool.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(viewer.get("/users").await.0, StatusCode::FORBIDDEN);

    // 운영자는 VM과 볼륨만 다룬다(VM 테스트에서 확인). 풀과 사용자는 관리자 몫이다.
    assert_eq!(operator.get("/nodes").await.0, StatusCode::OK);
    assert_eq!(
        operator.post("/storage-pools", pool).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(operator.get("/users").await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn users_manage_their_own_tokens_and_deleted_users_lose_access() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let alice = env.user("alice", "viewer").await;

    // 사용자 목록과 토큰 목록에 해시가 드러나지 않는다.
    let (_, users) = admin.get("/users").await;
    assert!(!users.to_string().contains("argon2"), "{users}");
    let (_, mine) = alice.get("/tokens").await;
    let items = mine["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].get("secret_hash").is_none());

    // 관리자 토큰은 alice에게 보이지 않고 지울 수도 없다.
    let (_, all) = admin.get("/tokens").await;
    let admin_token = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"].as_str().unwrap().starts_with("admin-"))
        .unwrap()
        .clone();
    let (status, _) = alice
        .delete(&format!("/tokens/{}", admin_token["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 자기 비밀번호는 현재 비밀번호를 대야 바꿀 수 있고, 역할은 못 바꾼다.
    let (status, body) = alice
        .put("/users/alice", json!({ "password": "new-password" }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "invalid_credentials");
    let (status, _) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = alice
        .put("/users/alice", json!({ "role": "admin" }), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = admin.delete("/users/admin").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // 마지막 관리자는 역할을 내려놓을 수 없다.
    let (status, body) = admin
        .put("/users/admin", json!({ "role": "viewer" }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "last_admin");
    assert_eq!(admin.delete("/users/alice").await.0, StatusCode::NO_CONTENT);
    assert_eq!(alice.get("/nodes").await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn weak_passwords_and_bad_names_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/users",
            json!({ "name": "bob", "password": "short", "role": "viewer" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    for name in ["Bob!", "0190a5c4-7c1e-7cc3-8a8e-2d1f0b8f4c11"] {
        let (status, _) = admin
            .post(
                "/users",
                json!({ "name": name, "password": "long-enough", "role": "viewer" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }
}
```

Run: `cargo test -p opl-api --test auth`
Expected: 컴파일 실패 (`opl_api::router`가 없음)

- [ ] **Step 2: 공통 핸들러와 사용자 핸들러**

`crates/opl-api/src/resources.rs`:

```rust
//! 종류에 상관없는 조회 핸들러와 공통 요청 형식, 풀과 노드 핸들러.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::IF_MATCH;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::{Cluster, Typed};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, StoragePoolStatus, VolumeSpec, VolumeStatus,
};
use opl_types::task::{TaskSpec, TaskStatus};
use opl_types::vm::{VmSpec, VmStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest<S> {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest<S> {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
}

/// 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다.
#[derive(Debug, Serialize, ToSchema)]
pub struct List<T> {
    pub revision: u64,
    pub items: Vec<T>,
}

/// 경로의 `{key}`는 id(UUID)나 이름이다.
pub fn lookup<S: ResourceSpec>(cluster: &Cluster, key: &str) -> Result<Typed<S>, ApiError> {
    let found = match Uuid::try_parse(key) {
        Ok(id) => cluster.get::<S>(id)?,
        Err(_) => cluster.get_by_name::<S>(key)?,
    };
    found.ok_or_else(|| ApiError::not_found(format!("{} {key:?} not found", S::KIND.as_str())))
}

/// `If-Match: <revision>` (따옴표로 감싸도 된다). 없으면 revision을 확인하지 않는다.
pub fn if_match(headers: &HeaderMap) -> Result<Option<u64>, ApiError> {
    let Some(value) = headers.get(IF_MATCH) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.trim().trim_matches('"').parse().ok())
        .map(Some)
        .ok_or_else(|| ApiError::bad_request("If-Match must be a resource revision"))
}

/// 경로에서 id와 이름을 구분할 수 있도록 UUID 모양의 이름은 받지 않는다.
pub fn check_name(name: &str) -> Result<(), ApiError> {
    validate_name(name).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if Uuid::try_parse(name).is_ok() {
        return Err(ApiError::bad_request(format!(
            "invalid name {name:?}: names must not look like an id"
        )));
    }
    Ok(())
}

fn list<S: ResourceSpec>(
    state: &AppState,
    principal: &Principal,
) -> Result<Json<List<Typed<S>>>, ApiError> {
    state.authorize(principal, Action::Read, S::KIND)?;
    let (revision, items) = state.cluster.list::<S>()?;
    Ok(Json(List { revision, items }))
}

fn get<S: ResourceSpec>(
    state: &AppState,
    principal: &Principal,
    key: &str,
) -> Result<Json<Typed<S>>, ApiError> {
    state.authorize(principal, Action::Read, S::KIND)?;
    Ok(Json(lookup::<S>(&state.cluster, key)?))
}

/// 종류별 목록·조회 핸들러. OpenAPI 문서에 종류마다 경로가 따로 필요해 제네릭 핸들러 대신 쓴다.
macro_rules! list_and_get {
    ($list:ident, $get:ident, $spec:ident, $status:ident, $path:literal, $item_path:literal, $tag:literal) => {
        #[utoipa::path(get, path = $path, tag = $tag, responses(
                                    (status = 200, body = List<Resource<$spec, $status>>),
                                    (status = "4XX", body = crate::error::ErrorBody),
                                ))]
        pub async fn $list(
            State(state): State<Arc<AppState>>,
            principal: Principal,
        ) -> Result<Json<List<Resource<$spec, $status>>>, ApiError> {
            list::<$spec>(&state, &principal)
        }

        #[utoipa::path(get, path = $item_path, tag = $tag,
                                    params(("key" = String, Path, description = "id or name")),
                                    responses(
                                        (status = 200, body = Resource<$spec, $status>),
                                        (status = "4XX", body = crate::error::ErrorBody),
                                    ))]
        pub async fn $get(
            State(state): State<Arc<AppState>>,
            principal: Principal,
            Path(key): Path<String>,
        ) -> Result<Json<Resource<$spec, $status>>, ApiError> {
            get::<$spec>(&state, &principal, &key)
        }
    };
}

list_and_get!(
    list_nodes,
    get_node,
    NodeSpec,
    NodeStatus,
    "/nodes",
    "/nodes/{key}",
    "nodes"
);
list_and_get!(
    list_pools,
    get_pool,
    StoragePoolSpec,
    StoragePoolStatus,
    "/storage-pools",
    "/storage-pools/{key}",
    "storage"
);
list_and_get!(
    list_volumes,
    get_volume,
    VolumeSpec,
    VolumeStatus,
    "/volumes",
    "/volumes/{key}",
    "storage"
);
list_and_get!(
    list_vms,
    get_vm,
    VmSpec,
    VmStatus,
    "/vms",
    "/vms/{key}",
    "vms"
);
list_and_get!(
    list_tasks,
    get_task,
    TaskSpec,
    TaskStatus,
    "/tasks",
    "/tasks/{key}",
    "tasks"
);

#[utoipa::path(post, path = "/storage-pools", tag = "storage",
    request_body = CreateRequest<StoragePoolSpec>,
    responses((status = 201, body = Resource<StoragePoolSpec, StoragePoolStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<StoragePoolSpec>>,
) -> Result<(StatusCode, Json<StoragePool>), ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    check_name(&req.name)?;
    let PoolBackend::Dir { path } = &req.spec.backend;
    if !path.is_absolute() {
        return Err(ApiError::bad_request("pool path must be absolute"));
    }
    for node in &req.spec.nodes {
        if state.cluster.get::<NodeSpec>(*node)?.is_none() {
            return Err(ApiError::bad_request(format!("node {node} not found")));
        }
    }
    let mut pool = Resource::new(req.name, req.spec);
    pool.labels = req.labels;
    pool.revision = state.cluster.create(&pool).await?;
    Ok((StatusCode::CREATED, Json(pool)))
}

/// 볼륨이 남아 있으면 409
#[utoipa::path(delete, path = "/storage-pools/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    let pool = lookup::<StoragePoolSpec>(&state.cluster, &key)?;
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    if let Some(v) = volumes.iter().find(|v| v.spec.pool == pool.name) {
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} still has volume {:?}", pool.name, v.name),
        ));
    }
    state
        .cluster
        .delete::<StoragePoolSpec>(pool.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
#[utoipa::path(put, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<NodeSpec>,
    responses((status = 200, body = Resource<NodeSpec, NodeStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<NodeSpec>>,
) -> Result<Json<Node>, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    state
        .cluster
        .update_spec(node.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<NodeSpec>(&state.cluster, &key)?))
}
```

`crates/opl-api/src/users.rs`:

```rust
//! 로그인, 사용자, API 토큰. 비밀번호 해시와 토큰 해시는 응답에 넣지 않는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{
    Action, Principal, SESSION_COOKIE, SESSION_TTL, dummy_hash, hash_password, new_token,
    session_cookie, verify_password,
};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;

pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct UserView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub role: Role,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            name: u.name,
            revision: u.revision,
            role: u.spec.role,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TokenView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub user: Uuid,
}

impl From<ApiToken> for TokenView {
    fn from(t: ApiToken) -> Self {
        Self {
            id: t.id,
            name: t.name,
            revision: t.revision,
            user: t.spec.user,
        }
    }
}

fn check_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// 세션 쿠키를 발급한다. 웹 UI가 쓴다. CLI는 로그인한 뒤 API 토큰을 만들어 쓴다.
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let user = state.cluster.get_by_name::<UserSpec>(&req.username)?;
    // 사용자가 없어도 해시를 비교해 응답 시간으로 사용자 존재 여부를 알 수 없게 한다.
    let hash = match &user {
        Some(u) => u.spec.password_hash.clone(),
        None => dummy_hash().await,
    };
    let ok = verify_password(req.password, hash).await;
    let Some(user) = user.filter(|_| ok) else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "wrong username or password",
        ));
    };
    let session = state.sessions.create(user.id);
    let cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    let mut response = Json(UserView::from(user)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok(response)
}

#[utoipa::path(post, path = "/auth/logout", tag = "auth", responses((status = 204)))]
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> StatusCode {
    if let Some(id) = session_cookie(&headers) {
        state.sessions.remove(id);
    }
    StatusCode::NO_CONTENT
}

#[utoipa::path(get, path = "/auth/whoami", tag = "auth",
    responses((status = 200, body = Principal), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn whoami(principal: Principal) -> Json<Principal> {
    Json(principal)
}

#[utoipa::path(get, path = "/users", tag = "auth",
    responses((status = 200, body = List<UserView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<UserView>>, ApiError> {
    state.authorize(&principal, Action::Read, UserSpec::KIND)?;
    let (revision, users) = state.cluster.list::<UserSpec>()?;
    Ok(Json(List {
        revision,
        items: users.into_iter().map(UserView::from).collect(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub name: String,
    pub password: String,
    pub role: Role,
}

#[utoipa::path(post, path = "/users", tag = "auth",
    request_body = CreateUserRequest,
    responses((status = 201, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    check_name(&req.name)?;
    check_password(&req.password)?;
    let mut user: User = Resource::new(
        req.name,
        UserSpec {
            password_hash: hash_password(req.password).await?,
            role: req.role,
        },
    );
    user.revision = state.cluster.create(&user).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub password: Option<String>,
    /// 자기 비밀번호를 바꿀 때 필요하다.
    #[serde(default)]
    pub current_password: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// 관리자는 누구의 비밀번호와 역할이든 바꾼다. 다른 사용자는 자기 비밀번호만 바꾼다.
/// 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
/// 바꾸면 그 사용자의 세션이 모두 끝난다. 마지막 관리자는 역할을 내려놓을 수 없다.
#[utoipa::path(put, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = UpdateUserRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserView>, ApiError> {
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    let is_self = user.id == principal.user;
    if !is_self || req.role.is_some() {
        state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    }
    let mut spec = user.spec.clone();
    let password_changed = req.password.is_some();
    if let Some(password) = req.password {
        check_password(&password)?;
        if is_self {
            let current = req.current_password.unwrap_or_default();
            if !verify_password(current, user.spec.password_hash.clone()).await {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_credentials",
                    "the current password is wrong",
                ));
            }
        }
        spec.password_hash = hash_password(password).await?;
    }
    if let Some(role) = req.role {
        if user.spec.role == Role::Admin && role != Role::Admin {
            let (_, users) = state.cluster.list::<UserSpec>()?;
            let admins = users.iter().filter(|u| u.spec.role == Role::Admin).count();
            if admins <= 1 {
                return Err(ApiError::conflict(
                    "last_admin",
                    "the last admin cannot give up the admin role",
                ));
            }
        }
        spec.role = role;
    }
    state
        .cluster
        .update_spec(user.id, Some(user.revision), user.labels, &spec)
        .await?;
    if password_changed {
        state.sessions.remove_user(user.id);
    }
    Ok(Json(
        lookup::<UserSpec>(&state.cluster, &user.id.to_string())?.into(),
    ))
}

/// 사용자와 그 사용자의 토큰을 지운다. 자기 자신은 지울 수 없다(마지막 관리자를 잃지 않게).
#[utoipa::path(delete, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    if user.id == principal.user {
        return Err(ApiError::conflict(
            "cannot_delete_self",
            "you cannot delete your own account",
        ));
    }
    // 토큰은 요청마다 주인을 다시 읽으므로, 사용자를 먼저 지우면 남은 토큰도 바로 쓸 수 없다.
    state.cluster.delete::<UserSpec>(user.id, None).await?;
    let (_, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    for token in tokens.into_iter().filter(|t| t.spec.user == user.id) {
        state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedToken {
    pub token: TokenView,
    /// `Authorization: Bearer <secret>`로 쓴다. 다시 볼 수 없다.
    pub secret: String,
}

fn may_manage(state: &AppState, principal: &Principal, token: &ApiToken) -> bool {
    token.spec.user == principal.user
        || state
            .authorizer
            .check(principal, Action::Write, ApiTokenSpec::KIND)
}

/// 요청한 사용자의 토큰을 만든다. 토큰의 권한은 주인의 역할을 따른다.
#[utoipa::path(post, path = "/tokens", tag = "auth",
    request_body = CreateTokenRequest,
    responses((status = 201, body = CreatedToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    check_name(&req.name)?;
    let mut token: ApiToken = Resource::new(
        req.name,
        ApiTokenSpec {
            user: principal.user,
            secret_hash: String::new(),
        },
    );
    let (hash, secret) = new_token(token.id);
    token.spec.secret_hash = hash;
    token.revision = state.cluster.create(&token).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token: token.into(),
            secret,
        }),
    ))
}

/// 자기 토큰. 관리자는 모든 토큰을 본다.
#[utoipa::path(get, path = "/tokens", tag = "auth",
    responses((status = 200, body = List<TokenView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_tokens(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<TokenView>>, ApiError> {
    let (revision, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    let items = tokens
        .into_iter()
        .filter(|t| may_manage(&state, &principal, t))
        .map(TokenView::from)
        .collect();
    Ok(Json(List { revision, items }))
}

#[utoipa::path(delete, path = "/tokens/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let token = lookup::<ApiTokenSpec>(&state.cluster, &key)?;
    // 남의 토큰은 있는지조차 알리지 않는다.
    if !may_manage(&state, &principal, &token) {
        return Err(ApiError::not_found(format!("api-token {key:?} not found")));
    }
    state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 3: 라우터**

`crates/opl-api/src/lib.rs` 전체 (Task 7~9에서 모듈과 라우트를 더한다):

```rust
//! opLite REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod auth;
pub mod error;
pub mod resources;
pub mod scheduler;
mod state;
mod tasks;
pub mod users;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
pub use tasks::{TaskRunner, TaskRunnerConfig};

use crate::resources as r;

#[derive(OpenApi)]
#[openapi(
    info(title = "opLite API", description = "opLite REST API (`/api/v1`)"),
    modifiers(&Security),
    security(("token" = []), ("session" = [])),
)]
struct ApiDoc;

/// API 토큰(`Authorization: Bearer opl_...`)과 세션 쿠키
struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "token",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(auth::SESSION_COOKIE))),
        );
    }
}

fn api() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(users::login))
        .routes(routes!(users::logout))
        .routes(routes!(users::whoami))
        .routes(routes!(users::list_users, users::create_user))
        .routes(routes!(users::update_user, users::delete_user))
        .routes(routes!(users::list_tokens, users::create_token))
        .routes(routes!(users::delete_token))
        .routes(routes!(r::list_nodes))
        .routes(routes!(r::get_node, r::update_node))
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_tasks))
        .routes(routes!(r::get_task))
}

/// API 문서. 라우터와 같은 정의에서 만들므로 경로가 어긋나지 않는다.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api().into_openapi()
}

/// `/api/v1` 아래의 API와 인증 없이 읽는 `/api/v1/openapi.json`
pub fn router(state: Arc<AppState>) -> Router {
    let (api, doc) = api().split_for_parts();
    let doc = Arc::new(doc);
    let api = api.route(
        "/openapi.json",
        get(move || {
            let doc = doc.clone();
            async move { axum::Json(doc.as_ref().clone()) }
        }),
    );
    Router::new().nest("/api/v1", api).with_state(state)
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-api`
Expected: 단위 8개, `auth` 5개 통과. 남은 경고는 `create_task` 하나(Task 7에서 사라진다).

- [ ] **Step 5: Commit**

```bash
git add crates/opl-api
git commit -m "feat(api): HTTP 서버, 로그인·사용자·토큰, 노드·풀·Task 조회, OpenAPI 라우터"
```

---

### Task 7: VM API와 동작

| 요청 | 하는 일 |
|---|---|
| `POST /vms` | 이름·spec 검증, 빠진 MAC 할당(클러스터 안에서 겹치지 않게), 디스크는 Ready 볼륨이고 다른 VM이 쓰지 않아야 함, 네트워크는 `user`만, 켜진 채로 만들면 노드 배치. 201 |
| `PUT /vms/{key}` | 같은 검증. 실행 중인 VM의 `node`는 바꿀 수 없다(409 `vm_running`). `If-Match` |
| `DELETE /vms/{key}` | 원하는 상태와 실제 상태가 모두 꺼져 있어야 한다(409 `vm_running`) |
| `POST /vms/{key}/actions/start` | 노드가 없으면 배치(못 하면 409 `no_node`), power = running, `vm-start` Task. 202 |
| `POST …/actions/stop[?mode=force]` | power = stopped, `vm-stop` 또는 `vm-force-stop` Task. 202 |
| `POST …/actions/reboot` | 실행 중이어야 한다(409 `vm_not_running`). `vm-reboot` Task. 202 |

동작이 spec을 바꿀 때는 revision을 확인하며 쓰고, reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다(`modify_spec`). Task는 VM이 배치된 노드에 만든다.

**Files:**
- Create: `crates/opl-api/src/vms.rs`, `crates/opl-api/tests/{vms,tasks}.rs`
- Modify: `crates/opl-api/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-api/tests/vms.rs`:

```rust
//! VM API: 생성 검증, 스케줄링, 동작과 Task, 오류 응답 형식.

mod common;

use std::time::Duration;

use common::{Env, vm_request};
use opl_driver::DriverError;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{VolumePhase, VolumeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn create_assigns_mac_and_validates_spec() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mac = vm["spec"]["nics"][0]["mac"].as_str().unwrap();
    assert!(mac.starts_with("52:54:00:"), "{mac}");
    // 꺼진 채로 만들면 노드를 고르지 않는다.
    assert_eq!(vm["spec"]["node"], Value::Null);
    assert_eq!(vm["spec"]["power"], "stopped");

    let mut bad = vm_request("bad");
    bad["spec"]["memory_mib"] = json!(1);
    let (status, body) = admin.post("/vms", bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");

    let mut typo = vm_request("typo");
    typo["spec"]["memroy_mib"] = json!(512);
    let (status, body) = admin.post("/vms", typo).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");

    let mut net = vm_request("net");
    net["spec"]["nics"][0]["network"] = json!("lan");
    assert_eq!(admin.post("/vms", net).await.0, StatusCode::BAD_REQUEST);

    let mut dup = vm_request("dup");
    dup["spec"]["nics"][0]["mac"] = json!(mac);
    let (status, body) = admin.post("/vms", dup).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "mac_in_use");

    let (status, body) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");

    let (status, body) = admin.get("/vms/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn operators_manage_vms_and_viewers_only_read() {
    let env = Env::start().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let (status, body) = viewer.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(
        operator.post("/vms", vm_request("web")).await.0,
        StatusCode::CREATED
    );
    assert_eq!(viewer.get("/vms/web").await.0, StatusCode::OK);
    let (status, _) = viewer.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let task = operator.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn disks_must_exist_and_belong_to_one_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let with_disk = |name: &str, volume: &str| {
        let mut req = vm_request(name);
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        req
    };
    // 파일이 아직 없는 볼륨은 붙일 수 없다.
    let (status, body) = admin.post("/vms", with_disk("a", "root")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_not_ready");
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    assert_eq!(
        admin.post("/vms", with_disk("a", "nope")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        admin.post("/vms", with_disk("a", "root")).await.0,
        StatusCode::CREATED
    );
    let (status, body) = admin.post("/vms", with_disk("b", "root")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_in_use");
}

#[tokio::test]
async fn start_schedules_and_stop_completes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-start");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");
    let id = vm["id"].as_str().unwrap().parse().unwrap();
    assert!(env.pid(id).await.is_some());

    // 실행 중인 VM은 지우거나 옮길 수 없다.
    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vm_running");
    let mut spec = vm["spec"].clone();
    spec["node"] = Value::Null;
    spec["power"] = json!("stopped");
    let (status, _) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Mock 게스트는 ACPI를 무시하므로 acpi_timeout(300ms) 뒤 강제로 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(env.pid(id).await, None);
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn force_stop_does_not_wait_for_acpi() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60), |_| async {}).await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;

    let (status, task) = admin
        .post("/vms/web/actions/stop?mode=force", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-force-stop");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");

    let (status, _) = admin
        .post("/vms/web/actions/start?mode=force", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn force_stop_during_reboot_wins() {
    // 게스트가 ACPI를 무시하므로 reboot은 60초 동안 꺼지기를 기다린다.
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60), |_| async {}).await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;
    let (_, reboot) = admin.post("/vms/web/actions/reboot", Value::Null).await;

    let task = admin.act("web", "stop?mode=force").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let reboot = admin.wait_task(&reboot).await;
    assert_eq!(reboot["status"]["state"], "failed");
    assert_eq!(
        reboot["status"]["error"],
        "superseded by a later request for this VM"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["power"], "stopped");
    assert_eq!(vm["status"]["phase"], "stopped");
}

#[tokio::test]
async fn reboot_restarts_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    admin.act("web", "start").await;
    let (_, before) = admin.get("/vms/web").await;

    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, after) = admin.get("/vms/web").await;
    assert_eq!(after["spec"]["power"], "running");
    assert_eq!(after["status"]["phase"], "running");
    assert_ne!(after["status"]["pid"], before["status"]["pid"]);

    admin.act("web", "stop").await;
    let (status, body) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "vm_not_running");
}

#[tokio::test]
async fn failed_start_fails_the_task() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        "failed to start: QEMU failed to start: no firmware"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["power"], "stopped");
}

#[tokio::test]
async fn start_fails_when_no_node_can_run_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    let (_, node) = admin.get(&format!("/nodes/{}", env.node)).await;
    let mut spec = node["spec"].clone();
    spec["maintenance"] = json!(true);
    let (status, _) = admin
        .put(
            &format!("/nodes/{}", env.node),
            json!({ "spec": spec }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "no_node");
}

#[tokio::test]
async fn update_checks_if_match_revision() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, vm) = admin.post("/vms", vm_request("web")).await;
    let revision = vm["revision"].as_u64().unwrap();
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(512);

    let (status, body) = admin
        .put("/vms/web", json!({ "spec": spec }), Some(revision + 100))
        .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(body["code"], "revision_mismatch");

    let (status, updated) = admin
        .put(
            "/vms/web",
            json!({ "spec": spec, "labels": { "tier": "web" } }),
            Some(revision),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["spec"]["memory_mib"], 512);
    assert_eq!(updated["labels"]["tier"], "web");
    // 기존 NIC의 MAC은 그대로 둔다.
    assert_eq!(updated["spec"]["nics"], vm["spec"]["nics"]);
}

#[tokio::test]
async fn list_returns_revision_and_items() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("a")).await;
    admin.post("/vms", vm_request("b")).await;
    let (status, list) = admin.get("/vms").await;
    assert_eq!(status, StatusCode::OK);
    assert!(list["revision"].as_u64().unwrap() > 0);
    let mut names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["a", "b"]);
}
```

`crates/opl-api/tests/tasks.rs` (Task 8에서 고아 정리 테스트를 더한다):

```rust
//! Task 실행기의 재시작 처리.

mod common;

use common::Env;
use opl_types::resource::Resource;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use uuid::Uuid;

#[tokio::test]
async fn tasks_left_running_by_a_previous_opld_are_failed() {
    let target = Uuid::now_v7();
    let id = std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = id.clone();
    let env = Env::start_with(
        |_| {},
        |cluster| async move {
            let node = cluster.id().0;
            let mut task: Task = Resource::new(
                "t",
                TaskSpec {
                    op: TaskOp::VmStop,
                    target,
                    node,
                },
            );
            task.status = TaskStatus {
                state: TaskState::Running,
                error: None,
            };
            cluster.create(&task).await.unwrap();
            *slot.lock().unwrap() = Some(task.id);
        },
    )
    .await;
    let admin = env.admin().await;
    let id = id.lock().unwrap().unwrap();

    let task = admin
        .wait_task(&serde_json::json!({ "id": id.to_string() }))
        .await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(
        task["status"]["error"],
        "interrupted: opld restarted while the task was running"
    );
}

#[tokio::test]
async fn tasks_created_while_runner_is_down_run_when_it_starts() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    admin.post("/vms", common::vm_request("web")).await;
    env.stop_runner().await;

    let (_, task) = admin
        .post("/vms/web/actions/start", serde_json::Value::Null)
        .await;
    assert_eq!(task["status"]["state"], "pending");
    env.start_runner();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}
```

Run: `cargo test -p opl-api --test vms --test tasks`
Expected: VM 경로가 없어 실패(404)

- [ ] **Step 2: 구현**

`crates/opl-api/src/vms.rs`:

```rust
//! VM 생성·수정·삭제와 동작(start, stop, reboot).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_reconciler::resolve::USER_NETWORK;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::VolumePhase;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{MacAddr, PowerState, Vm, VmPhase, VmSpec, VmStatus};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::scheduler::{Inventory, schedule};
use crate::state::AppState;
use crate::tasks::create_task;

/// 원하는 상태도 실제 상태도 꺼져 있다. 삭제와 노드 변경은 이때만 할 수 있다.
fn is_stopped(vm: &Vm) -> bool {
    vm.spec.power == PowerState::Stopped
        && matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

fn vm_running(vm: &Vm, what: &str) -> ApiError {
    ApiError::conflict(
        "vm_running",
        format!(
            "cannot {what} VM {:?} while it is running; stop it first",
            vm.name
        ),
    )
}

/// 저장하기 전에 spec을 검증하고 채운다: 빠진 MAC 할당, 참조 확인, 필요하면 노드 배치.
/// `id`는 이 spec을 가질 VM이다(자기 자신의 MAC과 디스크는 충돌로 보지 않는다).
fn prepare(state: &AppState, id: Uuid, spec: &mut VmSpec) -> Result<(), ApiError> {
    let inv = Inventory::load(&state.cluster)?;
    let others = inv.vms.iter().filter(|v| v.id != id);

    let mut used: BTreeSet<MacAddr> = others
        .clone()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in &spec.nics {
        if let Some(mac) = nic.mac
            && !used.insert(mac)
        {
            return Err(ApiError::conflict(
                "mac_in_use",
                format!("MAC address {mac} is already in use"),
            ));
        }
    }
    for nic in spec.nics.iter_mut().filter(|n| n.mac.is_none()) {
        let mac = std::iter::repeat_with(MacAddr::generate)
            .find(|m| !used.contains(m))
            .expect("random MACs eventually differ");
        used.insert(mac);
        nic.mac = Some(mac);
    }
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    if let Some(nic) = spec.nics.iter().find(|n| n.network != USER_NETWORK) {
        return Err(ApiError::bad_request(format!(
            "network {:?} not found (only {USER_NETWORK:?} is supported for now)",
            nic.network
        )));
    }
    let mut disks = BTreeSet::new();
    for disk in &spec.disks {
        if !disks.insert(&disk.volume) {
            return Err(ApiError::bad_request(format!(
                "volume {:?} is attached more than once",
                disk.volume
            )));
        }
        let Some(volume) = inv.volumes.iter().find(|v| v.name == disk.volume) else {
            return Err(ApiError::bad_request(format!(
                "volume {:?} not found",
                disk.volume
            )));
        };
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
        if let Some(other) = others
            .clone()
            .find(|v| v.spec.disks.iter().any(|d| d.volume == disk.volume))
        {
            return Err(ApiError::conflict(
                "volume_in_use",
                format!(
                    "volume {:?} is already attached to VM {:?}",
                    disk.volume, other.name
                ),
            ));
        }
    }
    if let Some(node) = spec.node
        && !inv.nodes.iter().any(|n| n.id == node)
    {
        return Err(ApiError::bad_request(format!("node {node} not found")));
    }
    if spec.power == PowerState::Running && spec.node.is_none() {
        spec.node = Some(
            schedule(id, spec, &inv).map_err(|e| ApiError::conflict("no_node", e.to_string()))?,
        );
    }
    Ok(())
}

/// 빠진 MAC을 채우고, 켜진 채로 만들면 노드를 고른다.
#[utoipa::path(post, path = "/vms", tag = "vms",
    request_body = CreateRequest<VmSpec>,
    responses((status = 201, body = Resource<VmSpec, VmStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<VmSpec>>,
) -> Result<(StatusCode, Json<Vm>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    check_name(&req.name)?;
    let mut vm: Vm = Resource::new(req.name, req.spec);
    vm.labels = req.labels;
    prepare(&state, vm.id, &mut vm.spec)?;
    vm.revision = state.cluster.create(&vm).await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`는 바꿀 수 없다. 다른 변경은 다음 시작부터 적용된다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<VmSpec>,
    responses((status = 200, body = Resource<VmSpec, VmStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<VmSpec>>,
) -> Result<Json<Vm>, ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let current = lookup::<VmSpec>(&state.cluster, &key)?;
    let mut spec = req.spec;
    prepare(&state, current.id, &mut spec)?;
    if spec.node != current.spec.node && !is_stopped(&current) {
        return Err(vm_running(&current, "move"));
    }
    state
        .cluster
        .update_spec(current.id, if_match(&headers)?, req.labels, &spec)
        .await?;
    Ok(Json(lookup::<VmSpec>(
        &state.cluster,
        &current.id.to_string(),
    )?))
}

/// 꺼진 VM만 지울 수 있다.
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    if !is_stopped(&vm) {
        return Err(vm_running(&vm, "delete"));
    }
    state
        .cluster
        .delete::<VmSpec>(vm.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ActionQuery {
    /// `stop`에만 쓴다. `force`면 ACPI 응답을 기다리지 않는다.
    #[serde(default)]
    pub mode: StopMode,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StopMode {
    #[default]
    Acpi,
    Force,
}

/// `start`와 `stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 돌려준다(202).
/// `stop?mode=force`와 `reboot`은 대상 노드의 Task 실행기가 수행한다.
#[utoipa::path(post, path = "/vms/{key}/actions/{action}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("action" = String, Path, description = "start, stop or reboot"), ActionQuery),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn vm_action(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, action)): Path<(String, String)>,
    Query(query): Query<ActionQuery>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    if query.mode == StopMode::Force && action != "stop" {
        return Err(ApiError::bad_request("mode=force applies only to stop"));
    }
    let id = lookup::<VmSpec>(&state.cluster, &key)?.id;
    let (op, vm) = match action.as_str() {
        "start" => {
            let vm = modify_spec(&state, id, |spec| {
                if spec.node.is_none() {
                    let inv = Inventory::load(&state.cluster)?;
                    let node = schedule(id, spec, &inv)
                        .map_err(|e| ApiError::conflict("no_node", e.to_string()))?;
                    spec.node = Some(node);
                }
                spec.power = PowerState::Running;
                Ok(())
            })
            .await?;
            (TaskOp::VmStart, vm)
        }
        "stop" => {
            let vm = modify_spec(&state, id, |spec| {
                spec.power = PowerState::Stopped;
                Ok(())
            })
            .await?;
            let op = match query.mode {
                StopMode::Acpi => TaskOp::VmStop,
                StopMode::Force => TaskOp::VmForceStop,
            };
            (op, vm)
        }
        "reboot" => {
            let vm = lookup::<VmSpec>(&state.cluster, &id.to_string())?;
            if vm.spec.power != PowerState::Running || vm.status.phase != VmPhase::Running {
                return Err(ApiError::conflict(
                    "vm_not_running",
                    format!("VM {:?} is not running", vm.name),
                ));
            }
            (TaskOp::VmReboot, vm)
        }
        _ => {
            return Err(ApiError::not_found(format!(
                "unknown action {action:?} (start, stop, reboot)"
            )));
        }
    };
    // 배치되지 않은 VM은 돌고 있지 않다. 정지 Task는 이 노드에서 곧바로 끝난다.
    let node = vm.spec.node.unwrap_or(state.node);
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// spec을 읽고 바꿔 revision을 확인하며 쓴다. reconciler의 status 쓰기와 부딪치면 다시 읽는다.
pub(crate) async fn modify_spec(
    state: &AppState,
    id: Uuid,
    f: impl Fn(&mut VmSpec) -> Result<(), ApiError>,
) -> Result<Vm, ApiError> {
    for _ in 0..10 {
        let vm = state
            .cluster
            .get::<VmSpec>(id)?
            .ok_or_else(|| ApiError::not_found(format!("vm {id} not found")))?;
        let mut spec = vm.spec.clone();
        f(&mut spec)?;
        if spec == vm.spec {
            return Ok(vm);
        }
        match state
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
        return state
            .cluster
            .get::<VmSpec>(id)?
            .ok_or_else(|| ApiError::not_found(format!("vm {id} not found")));
    }
    Err(ApiError::conflict(
        "busy",
        "the VM kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/src/lib.rs`의 `pub mod users;` 아래에 `mod vms;`를 넣고, `api()`의 `.routes(routes!(r::get_task))` 아래에 추가한다:

```rust
        .routes(routes!(r::list_vms, vms::create_vm))
        .routes(routes!(r::get_vm, vms::update_vm, vms::delete_vm))
        .routes(routes!(vms::vm_action))
```

- [ ] **Step 3: 통과 확인**

Run: `cargo test -p opl-api`
Expected: 단위 8개, `auth` 5개, `vms` 11개, `tasks` 2개 통과. 경고 없음.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

- [ ] **Step 4: 부하 속 안정성 확인**

```bash
cargo test -p opl-api --test vms --test tasks --no-run
fails=0
for t in vms tasks; do
  B=$(ls -t target/debug/deps/$t-* | grep -v '\.d$' | head -1)
  for r in 1 2 3; do
    pids=()
    for i in $(seq 5); do "$B" -q >/dev/null 2>&1 & pids+=($!); done
    for p in "${pids[@]}"; do wait "$p" || fails=$((fails+1)); done
  done
done
echo "failures: $fails"
```

Expected: `failures: 0`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-api
git commit -m "feat(api): VM CRUD, 시작·정지·강제 종료·재부팅 동작과 Task"
```

---

### Task 8: 볼륨 API

| 요청 | 하는 일 |
|---|---|
| `POST /volumes` | 이름·크기 검증, 풀이 있어야 함. 볼륨(Pending)과 `volume-create` Task를 만든다. 202 |
| `POST /volumes/import?name=&pool=&source_format=[&format=]` | 본문(이미지)을 받아 `volume-import` Task를 만든다. 202 |
| `DELETE /volumes/{key}` | VM이 쓰면 409 `volume_in_use`. 아니면 `volume-delete` Task. 202 |
| `DELETE /storage-pools/{key}` | 볼륨이 남아 있으면 409 `pool_in_use` (Task 6) |

업로드는 다음 순서로 받는다.

- **사전 확인:** `Content-Length`가 필요하다(없으면 411). 받기 전에 데이터 디렉터리에 1 GiB를 남길 공간이 있는지 확인한다(없으면 507). 데이터 디렉터리에는 Raft 저장소도 있기 때문이다.
- **받기:** 본문을 `Content-Length`만큼만 받는다.
- **중단 처리:** 끝내지 못하거나 클라이언트가 끊으면 볼륨과 파일을 지운다(`UploadGuard`).
- **풀 조건:** 풀은 이 노드에서 쓸 수 있어야 한다(409 `pool_not_on_node`). 볼륨 파일 작업은 풀을 쓸 수 있는 노드에서 한다.

**Files:**
- Create: `crates/opl-api/src/volumes.rs`, `crates/opl-api/tests/volumes.rs`
- Modify: `crates/opl-api/src/lib.rs`, `crates/opl-api/tests/tasks.rs`, `.github/workflows/ci.yml`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-api/tests/volumes.rs` (qemu-img가 필요하다):

```rust
//! 볼륨 생성·가져오기·삭제. qemu-img가 필요하다.
//! `cargo test -p opl-api --features storage-it --test volumes`
#![cfg(feature = "storage-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::{Client, Env};
use reqwest::StatusCode;
use serde_json::json;

const MIB: u64 = 1 << 20;

async fn create_pool(admin: &Client, dir: &Path) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "local",
                "spec": { "backend": { "kind": "dir", "path": dir } }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[tokio::test]
async fn creates_and_deletes_volume_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&admin, &pool).await;

    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": 10 * MIB, "format": "qcow2" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/root").await;
    assert_eq!(volume["status"]["phase"], "ready");
    assert!(volume["status"]["allocated_bytes"].as_u64().unwrap() > 0);
    let file = pool.join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(file.exists());

    let (status, body) = admin.delete("/storage-pools/local").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "pool_in_use");

    // VM이 쓰는 볼륨은 지울 수 없다.
    let mut vm = common::vm_request("web");
    vm["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    assert_eq!(admin.post("/vms", vm).await.0, StatusCode::CREATED);
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_in_use");
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);

    let (status, task) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(!file.exists());
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        admin.delete("/storage-pools/local").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn failed_create_removes_the_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 풀 디렉터리가 없으면 qemu-img가 실패한다.
    create_pool(&admin, &env.dir.path().join("missing")).await;
    let (_, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw" }
            }),
        )
        .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imports_uploaded_vmdk() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&admin, &pool).await;
    let raw = env.dir.path().join("disk.raw");
    std::fs::write(&raw, vec![7u8; 3 * MIB as usize]).unwrap();
    let vmdk = env.dir.path().join("disk.vmdk");
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

    let (status, task) = admin
        .upload(
            "/volumes/import?name=imported&pool=local&source_format=vmdk",
            std::fs::read(&vmdk).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/imported").await;
    assert_eq!(volume["spec"]["size_bytes"], 3 * MIB);
    assert_eq!(volume["spec"]["format"], "qcow2");
    // 올린 파일은 가져온 뒤 지운다.
    let uploads: Vec<_> = std::fs::read_dir(env.dir.path().join("uploads"))
        .unwrap()
        .collect();
    assert!(uploads.is_empty());
}

#[tokio::test]
async fn upload_needs_content_length() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&admin, &pool).await;
    // 스트림 본문은 길이를 모르므로 chunked로 간다.
    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![0u8; 512])]);
    let r = admin
        .http
        .post(format!(
            "{}/api/v1/volumes/import?name=x&pool=local&source_format=raw",
            admin.base
        ))
        .bearer_auth(admin.token.as_ref().unwrap())
        .body(reqwest::Body::wrap_stream(chunks));
    let (status, body) = admin.send(r).await;
    assert_eq!(status, StatusCode::LENGTH_REQUIRED, "{body}");
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_images_that_refer_to_other_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&admin, &pool).await;
    let descriptor = "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
                      createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"/etc/hosts\" 0\n";

    let (status, task) = admin
        .upload(
            "/volumes/import?name=evil&pool=local&source_format=vmdk",
            descriptor.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("/etc/hosts"), "{error}");
    assert_eq!(admin.get("/volumes/evil").await.0, StatusCode::NOT_FOUND);
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
}
```

`crates/opl-api/tests/tasks.rs`의 `tasks_created_while_runner_is_down_run_when_it_starts` 앞에 추가하고, import에 `use opl_types::runtime::ImageFormat;`와 `use opl_types::storage::VolumeSpec;`를 더한다:

```rust
#[tokio::test]
async fn orphans_of_a_previous_opld_are_removed() {
    let env = Env::start_with(
        |_| {},
        |cluster| async move {
            // 볼륨을 만든 뒤 Task를 만들기 전에(또는 업로드 도중) opld가 죽었다.
            let volume = Resource::new(
                "half",
                VolumeSpec {
                    pool: "local".into(),
                    size_bytes: 1,
                    format: ImageFormat::Raw,
                },
            );
            cluster.create(&volume).await.unwrap();
        },
    )
    .await;
    let admin = env.admin().await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while admin.get("/volumes/half").await.0 != reqwest::StatusCode::NOT_FOUND {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan volume was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}
```

Run: `cargo test -p opl-api --features storage-it --test volumes`
Expected: 볼륨 경로가 없어 실패

- [ ] **Step 2: 구현**

`crates/opl-api/src/volumes.rs`:

```rust
//! 볼륨 생성·가져오기·삭제. 파일 작업은 풀을 쓸 수 있는 노드의 Task 실행기가 한다.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, check_name, lookup};
use crate::state::AppState;
use crate::tasks::create_task;

/// 볼륨 파일 작업을 실행할 노드. 이 노드가 풀을 쓸 수 있으면 이 노드다.
fn pool_node(state: &AppState, pool: &StoragePool) -> Uuid {
    let nodes = &pool.spec.nodes;
    if nodes.is_empty() || nodes.contains(&state.node) {
        state.node
    } else {
        nodes[0]
    }
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// 볼륨 리소스를 만들고, 파일을 만들 Task를 돌려준다(202). Task가 실패하면 리소스도 지운다.
#[utoipa::path(post, path = "/volumes", tag = "storage",
    request_body = CreateRequest<VolumeSpec>,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<VolumeSpec>>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    if req.spec.size_bytes == 0 {
        return Err(ApiError::bad_request("size_bytes must be greater than 0"));
    }
    let pool = find_pool(&state, &req.spec.pool)?;
    let mut volume: Volume = Resource::new(req.name, req.spec);
    volume.labels = req.labels;
    state.cluster.create(&volume).await?;
    let task = create_task(
        &state.cluster,
        TaskOp::VolumeCreate,
        volume.id,
        pool_node(&state, &pool),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ImportQuery {
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (기본 qcow2)
    #[serde(default = "qcow2")]
    #[param(inline)]
    pub format: ImageFormat,
    #[param(inline)]
    pub source_format: SourceFormat,
}

fn qcow2() -> ImageFormat {
    ImageFormat::Qcow2
}

/// 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
/// 풀은 이 노드에서 쓸 수 있어야 한다.
#[utoipa::path(post, path = "/volumes/import", tag = "storage",
    params(ImportQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn import_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ImportQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&q.name)?;
    let pool = find_pool(&state, &q.pool)?;
    if pool_node(&state, &pool) != state.node {
        return Err(ApiError::conflict(
            "pool_not_on_node",
            format!(
                "storage pool {:?} is not available on this node; upload to one of its nodes",
                pool.name
            ),
        ));
    }
    // 크기는 가져온 뒤 실제 이미지 크기로 바뀐다.
    let volume: Volume = Resource::new(
        q.name,
        VolumeSpec {
            pool: pool.name.clone(),
            size_bytes: 0,
            format: q.format,
        },
    );
    let length = content_length(&headers)?;
    tokio::fs::create_dir_all(&state.upload_dir)
        .await
        .map_err(ApiError::internal)?;
    let available = fs4::available_space(&state.upload_dir).map_err(ApiError::internal)?;
    if length.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("not enough free space for a {length}-byte upload"),
        ));
    }
    state.cluster.create(&volume).await?;
    // 여기서부터 실패하거나 클라이언트가 끊어 이 future가 버려지면 볼륨과 파일을 지운다.
    let mut guard = UploadGuard {
        state: state.clone(),
        volume: volume.id,
        source: state.upload_dir.join(format!("{}.upload", volume.id)),
        armed: true,
    };
    receive(&guard.source, body, length).await?;
    let op = TaskOp::VolumeImport {
        source: guard.source.clone(),
        source_format: q.source_format,
    };
    let task = create_task(&state.cluster, op, volume.id, state.node).await?;
    guard.armed = false;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 올린 파일이 다 들어와도 데이터 디렉터리(Raft 저장소가 있다)에 남겨 둘 여유 공간
const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;

fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::LENGTH_REQUIRED,
                "length_required",
                "uploads need a Content-Length header",
            )
        })
}

/// 업로드를 끝내지 못한 볼륨과 파일을 지운다. 정상 경로에서는 `armed`를 내린다.
struct UploadGuard {
    state: Arc<AppState>,
    volume: Uuid,
    source: PathBuf,
    armed: bool,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (state, volume, source) = (self.state.clone(), self.volume, self.source.clone());
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(&source).await;
            if let Err(e) = state.cluster.delete::<VolumeSpec>(volume, None).await {
                tracing::warn!(%volume, error = %e, "cannot remove volume of a failed upload");
            }
        });
    }
}

/// 본문을 `length`바이트까지 받는다. 그보다 길거나 짧으면 실패다.
async fn receive(path: &FsPath, body: Body, length: u64) -> Result<(), ApiError> {
    let mut file = tokio::fs::File::create_new(path)
        .await
        .map_err(ApiError::internal)?;
    let mut received = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload failed: {e}")))?;
        received += chunk.len() as u64;
        if received > length {
            return Err(ApiError::bad_request(
                "upload is longer than Content-Length",
            ));
        }
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
    }
    if received != length {
        return Err(ApiError::bad_request("upload ended early"));
    }
    file.sync_all().await.map_err(ApiError::internal)?;
    Ok(())
}

/// VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202).
#[utoipa::path(delete, path = "/volumes/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(ApiError::conflict(
            "volume_in_use",
            format!("volume {:?} is attached to VM {:?}", volume.name, vm.name),
        ));
    }
    let node = match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
    {
        Some(pool) => pool_node(&state, &pool),
        None => state.node,
    };
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

`crates/opl-api/src/lib.rs`의 `mod vms;` 아래에 `mod volumes;`를 넣고, `api()`의 `.routes(routes!(vms::vm_action))` 아래에 추가한다:

```rust
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
```

- [ ] **Step 3: CI에 볼륨 테스트 추가**

`.github/workflows/ci.yml`의 "Storage integration tests (qemu-img)" 단계 끝에 줄을 더한다:

```yaml
          cargo test -p opl-api --features storage-it --test volumes
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p opl-api --features storage-it`
Expected: 단위 8개, `auth` 5개, `vms` 11개, `tasks` 3개, `volumes` 5개 통과

- [ ] **Step 5: Commit**

```bash
git add crates/opl-api .github/workflows/ci.yml
git commit -m "feat(api): 볼륨 생성·가져오기(업로드)·삭제, 고아 정리"
```

---

### Task 9: WebSocket(변경 구독, 콘솔)과 OpenAPI 문서

- **변경 구독:** `GET /api/v1/watch?kinds=vm,task&since=<revision>`은 `opl_cluster::watch::WatchEvent`를 JSON 텍스트 메시지로 보낸다. 이어 받을 수 없으면(`Expired`, `Lagged`) 오류 메시지를 보내고 닫는다. 사용자와 토큰은 구독할 수 없다.
- **콘솔:** `GET /api/v1/vms/{key}/console`은 바이너리 메시지를 이 노드에서 도는 VM의 VNC 유닉스 소켓으로 잇는다.
- **업그레이드 순서:** 두 엔드포인트 모두 인증·권한·입력을 먼저 검사한 뒤에 WebSocket으로 업그레이드한다. 그래서 일반 GET에도 API 오류 형식(400, 403, 409)으로 답한다.
- **OpenAPI:** `/api/v1/openapi.json`은 인증 없이 읽는다.

**Files:**
- Create: `crates/opl-api/src/{watch,console}.rs`, `crates/opl-api/tests/{streams,openapi}.rs`
- Modify: `crates/opl-api/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-api/tests/streams.rs`:

```rust
//! WebSocket: 변경 구독과 VNC 콘솔 중계.

mod common;

use std::time::Duration;

use common::{Client, Env, vm_request};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(client: &Client, path: &str) -> Socket {
    let url = format!("{}/api/v1{path}", client.base.replace("http://", "ws://"));
    let mut req = url.into_client_request().unwrap();
    let token = client.token.as_ref().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn next_json(ws: &mut Socket) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("no message")
            .unwrap()
            .unwrap();
        if let Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

#[tokio::test]
async fn watch_streams_changes_after_revision() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let (_, list) = viewer.get("/vms").await;
    let since = list["revision"].as_u64().unwrap();
    let mut ws = connect(&viewer, &format!("/watch?kinds=vm,task&since={since}")).await;

    admin.post("/vms", vm_request("web")).await;
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "put");
    assert_eq!(ev["kind"], "vm");
    assert_eq!(ev["resource"]["name"], "web");

    // 시작하면 Task와 VM 변경이 함께 온다.
    admin.post("/vms/web/actions/start", Value::Null).await;
    let mut kinds = Vec::new();
    while kinds.len() < 3 {
        kinds.push(
            next_json(&mut ws).await["kind"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    assert!(kinds.contains(&"task".to_string()), "{kinds:?}");
    assert!(kinds.contains(&"vm".to_string()), "{kinds:?}");
}

#[tokio::test]
async fn watch_rejects_secret_kinds_and_bad_queries() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin.get("/watch?kinds=user&since=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("user"), "{body}");
    let (status, body) = admin.get("/watch?kinds=vm").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"].as_str().unwrap().contains("since"),
        "{body}"
    );
    // 검증을 통과해도 WebSocket 요청이 아니면 거부한다.
    let (status, body) = admin.get("/watch?kinds=vm&since=0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn console_bridges_websocket_to_vnc_socket() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    // 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let consoles = tmp.path().to_path_buf();
    env.hv.set_console_dir(&consoles);

    admin.post("/vms", vm_request("web")).await;
    let (status, body) = admin.get("/vms/web/console").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_not_running");

    admin.act("web", "start").await;
    let (_, vm) = admin.get("/vms/web").await;
    let id = vm["id"].as_str().unwrap();
    // 가짜 VNC 서버: 인사말을 보내고, 받은 것을 그대로 돌려준다.
    let listener = tokio::net::UnixListener::bind(consoles.join(format!("{id}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    assert_eq!(
        viewer.get("/vms/web/console").await.0,
        StatusCode::FORBIDDEN
    );
    let mut ws = connect(&admin, "/vms/web/console").await;
    let greeting = ws.next().await.unwrap().unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap(),
        Message::binary(b"ping".to_vec())
    );
}
```

`crates/opl-api/tests/openapi.rs`:

```rust
//! API 문서(OpenAPI)

mod common;

use common::Env;
use reqwest::StatusCode;

#[tokio::test]
async fn openapi_document_is_served_without_login() {
    let env = Env::start().await;
    let (status, doc) = env.anonymous().get("/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    std::fs::write(
        "/tmp/opl-openapi.json",
        serde_json::to_string_pretty(&doc).unwrap(),
    )
    .unwrap();
    let paths = doc["paths"].as_object().unwrap();
    for path in [
        "/auth/login",
        "/vms",
        "/vms/{key}",
        "/vms/{key}/actions/{action}",
        "/vms/{key}/console",
        "/volumes/import",
        "/watch",
    ] {
        assert!(paths.contains_key(path), "{path} is not documented");
    }
    // 리소스 종류마다 따로 스키마가 생긴다.
    assert_eq!(
        doc["paths"]["/vms"]["post"]["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/Resource_VmSpec_VmStatus"
    );
    assert!(doc["components"]["schemas"]["Resource_TaskSpec_TaskStatus"].is_object());
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["token"]["scheme"], "bearer");
    assert_eq!(schemes["session"]["name"], "opl_session");
    // 로그인은 인증 없이 부른다.
    assert_eq!(
        doc["paths"]["/auth/login"]["post"]["security"],
        serde_json::json!([{}])
    );
}
```

Run: `cargo test -p opl-api --test streams --test openapi`
Expected: 실패 (`/watch`, `/vms/{key}/console`이 없음)

- [ ] **Step 2: 구현**

`crates/opl-api/src/watch.rs`:

```rust
//! `GET /api/v1/watch?kinds=vm,task&since=<revision>`: 커밋된 변경을 WebSocket으로 보낸다.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_types::resource::Kind;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Query};
use crate::state::AppState;

/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 5] = [
    Kind::Vm,
    Kind::Volume,
    Kind::StoragePool,
    Kind::Node,
    Kind::Task,
];

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct WatchQuery {
    /// 쉼표로 구분한 종류 이름. 예: `vm,task`
    pub kinds: String,
    /// 목록 응답의 `revision`. 그 뒤의 변경부터 받는다.
    pub since: u64,
}

fn parse_kinds(s: &str) -> Result<BTreeSet<Kind>, ApiError> {
    s.split(',')
        .map(|name| {
            WATCHABLE
                .into_iter()
                .find(|k| k.as_str() == name.trim())
                .ok_or_else(|| ApiError::bad_request(format!("cannot watch kind {name:?}")))
        })
        .collect()
}

/// 이벤트는 `opl_cluster::watch::WatchEvent`의 JSON이다. 이어 받을 수 없게 되면
/// `{"type":"error","code":"expired"|"lagged"|"closed"}`를 보내고 닫는다. 클라이언트는 목록을
/// 다시 읽고 새로 구독한다.
#[utoipa::path(get, path = "/watch", tag = "watch",
    params(WatchQuery),
    responses((status = 101, description = "WebSocket stream of change events"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn watch(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<WatchQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let kinds = parse_kinds(&q.kinds)?;
    for kind in &kinds {
        state.authorize(&principal, Action::Read, *kind)?;
    }
    let watch = state.cluster.watch(q.since).map_err(|e| match e {
        WatchError::Expired { .. } => ApiError::conflict("expired", e.to_string()),
        other => ApiError::internal(other),
    })?;
    Ok(ws?.on_upgrade(move |socket| stream(socket, watch, kinds)))
}

fn wanted(event: &WatchEvent, kinds: &BTreeSet<Kind>) -> bool {
    match event {
        WatchEvent::Put { kind, .. } | WatchEvent::Deleted { kind, .. } => kinds.contains(kind),
        WatchEvent::Reset { .. } => true,
    }
}

async fn stream(mut socket: WebSocket, mut watch: Watch, kinds: BTreeSet<Kind>) {
    loop {
        tokio::select! {
            ev = watch.next() => {
                let text = match ev {
                    Ok(ev) if wanted(&ev, &kinds) => serde_json::to_string(&ev),
                    Ok(_) => continue,
                    Err(e) => {
                        let code = match e {
                            WatchError::Expired { .. } => "expired",
                            WatchError::Lagged => "lagged",
                            WatchError::Closed => "closed",
                        };
                        let msg = serde_json::json!({
                            "type": "error",
                            "code": code,
                            "message": e.to_string(),
                        });
                        let _ = socket.send(Message::text(msg.to_string())).await;
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                };
                let Ok(text) = text else { return };
                if socket.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}
```

`crates/opl-api/src/console.rs`:

```rust
//! `GET /api/v1/vms/{id}/console`: WebSocket 바이너리 메시지를 VM의 VNC 유닉스 소켓으로 잇는다.

use std::sync::Arc;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::Response;
use opl_driver::DriverError;
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::resources::lookup;
use crate::state::AppState;

/// VM이 이 노드에서 돌고 있어야 한다. 다른 노드로 중계하는 것은 노드 간 통신이 생긴 뒤에 한다.
#[utoipa::path(get, path = "/vms/{key}/console", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 101, description = "WebSocket carrying the VNC (RFB) stream"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn console(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let not_running = || {
        ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running on this node", vm.name),
        )
    };
    if vm.status.node != Some(state.node) {
        return Err(not_running());
    }
    let path = match state.hv.console_endpoint(vm.id).await {
        Ok(path) => path,
        Err(DriverError::NotRunning(_)) => return Err(not_running()),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let ws = ws?;
    let vnc = UnixStream::connect(&path)
        .await
        .map_err(|_| not_running())?;
    Ok(ws.on_upgrade(move |socket| bridge(socket, vnc)))
}

async fn bridge(mut socket: WebSocket, mut vnc: UnixStream) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            n = vnc.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket.send(Message::binary(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Binary(data))) => {
                    if vnc.write_all(&data).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}
```

`crates/opl-api/src/lib.rs` 최종본:

```rust
//! opLite REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod auth;
mod console;
pub mod error;
pub mod resources;
pub mod scheduler;
mod state;
mod tasks;
pub mod users;
mod vms;
mod volumes;
mod watch;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
pub use tasks::{TaskRunner, TaskRunnerConfig};

use crate::resources as r;

#[derive(OpenApi)]
#[openapi(
    info(title = "opLite API", description = "opLite REST API (`/api/v1`)"),
    modifiers(&Security),
    security(("token" = []), ("session" = [])),
)]
struct ApiDoc;

/// API 토큰(`Authorization: Bearer opl_...`)과 세션 쿠키
struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "token",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(auth::SESSION_COOKIE))),
        );
    }
}

fn api() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(users::login))
        .routes(routes!(users::logout))
        .routes(routes!(users::whoami))
        .routes(routes!(users::list_users, users::create_user))
        .routes(routes!(users::update_user, users::delete_user))
        .routes(routes!(users::list_tokens, users::create_token))
        .routes(routes!(users::delete_token))
        .routes(routes!(r::list_nodes))
        .routes(routes!(r::get_node, r::update_node))
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_tasks))
        .routes(routes!(r::get_task))
        .routes(routes!(r::list_vms, vms::create_vm))
        .routes(routes!(r::get_vm, vms::update_vm, vms::delete_vm))
        .routes(routes!(vms::vm_action))
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
        .routes(routes!(console::console))
        .routes(routes!(watch::watch))
}

/// API 문서. 라우터와 같은 정의에서 만들므로 경로가 어긋나지 않는다.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api().into_openapi()
}

/// `/api/v1` 아래의 API와 인증 없이 읽는 `/api/v1/openapi.json`
pub fn router(state: Arc<AppState>) -> Router {
    let (api, doc) = api().split_for_parts();
    let doc = Arc::new(doc);
    let api = api.route(
        "/openapi.json",
        get(move || {
            let doc = doc.clone();
            async move { axum::Json(doc.as_ref().clone()) }
        }),
    );
    Router::new().nest("/api/v1", api).with_state(state)
}
```

- [ ] **Step 3: 통과 확인**

Run: `cargo test -p opl-api --features storage-it`
Expected: 단위 8개, `auth` 5개, `openapi` 1개, `streams` 3개, `tasks` 3개, `vms` 11개, `volumes` 5개 통과

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: 경고 없음

- [ ] **Step 4: Commit**

```bash
git add crates/opl-api
git commit -m "feat(api): 변경 구독·VNC 콘솔 WebSocket, OpenAPI 문서"
```

---

### Task 10: `oplctl`

E2E 테스트가 같은 코드를 부를 수 있도록 라이브러리(`oplctl::run(args, out)`) + 얇은 바이너리로 만든다. 인자 오류는 `clap::Error`로 돌려주고, 바이너리가 그것을 clap 방식(도움말, 종료 코드 2)으로 출력한다.

```
oplctl [--config PATH] [--server URL] [-o table|json] <명령>
  login --server URL [--user admin] [--password-file F]    세션으로 로그인 → API 토큰 저장(0600) → 세션 닫기
  logout
  node list | get N
  pool list | get P | create P --path DIR [--node ID]... | delete P
  volume list | get V | create V --pool P --size 10G [--format qcow2|raw]
         import V --pool P --file F [--source-format vmdk|qcow2|raw] | delete V
  vm list | get V | create V [--cores N] [--memory MIB] [--disk VOL]... [--nic user]... [--start]
     start V | stop V [--force] | reboot V | delete V | console V [--port N]
  task list | get T
  user list | create U --role R | passwd U | delete U
  token list | create T | delete T
```

- **Task를 만드는 명령:** 볼륨 생성·가져오기·삭제, VM 동작은 Task가 끝날 때까지 기다리고, 실패하면 Task의 오류로 끝난다.
- **콘솔:** `vm console`은 `127.0.0.1`의 로컬 포트(기본은 빈 포트)에서 VNC 뷰어의 연결을 받아 콘솔 WebSocket으로 잇는다.
- **다시 로그인:** 같은 서버에 다시 로그인하면 이전 토큰은 서버에서 지운다.
- **업로드:** 이미지는 파일을 읽어 가며 `Content-Length`와 함께 스트리밍한다.

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/oplctl/Cargo.toml`, `crates/oplctl/src/{lib,main,cli,client,config,console,output}.rs`

- [ ] **Step 1: 의존성**

루트 `Cargo.toml`의 `opl-api = ...` 아래에 `oplctl = { path = "crates/oplctl" }`를, 끝에 추가한다:

```toml
clap = { version = "4.6.7", features = ["derive", "env"] }
anyhow = "1.0.104"
rpassword = "7.5.4"
tokio-util = { version = "0.7.20", features = ["io"] }
```

`crates/oplctl/Cargo.toml`:

```toml
[package]
name = "oplctl"
description = "Command-line client for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
anyhow.workspace = true
clap.workspace = true
futures-util.workspace = true
reqwest.workspace = true
rpassword.workspace = true
serde.workspace = true
serde_json.workspace = true
tokio.workspace = true
tokio-tungstenite.workspace = true
tokio-util.workspace = true
uuid.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 2: 설정 파일과 출력**

`crates/oplctl/src/config.rs`:

```rust
//! 로그인 정보(서버 주소, API 토큰)를 담는 파일. 기본 위치는 `~/.config/opl/oplctl.json`이다.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliConfig {
    pub server: Option<String>,
    pub token: Option<String>,
}

pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    Path::new(&home).join(".config/opl/oplctl.json")
}

impl CliConfig {
    /// 파일이 없으면 빈 설정이다.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// 토큰이 들어 있으므로 주인만 읽을 수 있게(0600) 쓴다.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode`는 새 파일에만 쓰인다. 이미 있던 파일도 주인만 읽게 한다.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_privately_and_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/oplctl.json");
        assert_eq!(CliConfig::load(&path).unwrap(), CliConfig::default());
        let config = CliConfig {
            server: Some("http://127.0.0.1:8006".into()),
            token: Some("opl_x_y".into()),
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        config.save(&path).unwrap();
        assert_eq!(CliConfig::load(&path).unwrap(), config);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
```

`crates/oplctl/src/output.rs`:

```rust
//! 표와 JSON 출력.

use std::io::Write;

use serde_json::Value;

/// 열 너비를 맞춘 표. 마지막 열은 채우지 않는다.
pub fn table(out: &mut dyn Write, headers: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |out: &mut dyn Write, cells: Vec<&str>| -> std::io::Result<()> {
        let last = cells.len() - 1;
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i == last {
                s.push_str(cell);
            } else {
                s.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        writeln!(out, "{}", s.trim_end())
    };
    line(out, headers.to_vec())?;
    for row in rows {
        line(out, row.iter().map(String::as_str).collect())?;
    }
    Ok(())
}

pub fn json(out: &mut dyn Write, v: &Value) -> std::io::Result<()> {
    writeln!(out, "{}", serde_json::to_string_pretty(v).unwrap())
}

/// 값을 표의 칸으로. 없으면 `-`.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 바이트 수를 사람이 읽기 쉽게(1024 단위)
pub fn bytes(v: &Value) -> String {
    let Some(n) = v.as_u64() else {
        return "-".into();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n}B")
    } else {
        format!("{size:.1}{}", units[unit])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn aligns_columns() {
        let mut out = Vec::new();
        table(
            &mut out,
            &["NAME", "STATE"],
            &[
                vec!["a".into(), "running".into()],
                vec!["longer".into(), "-".into()],
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME    STATE\na       running\nlonger  -\n"
        );
    }

    #[test]
    fn formats_cells_and_sizes() {
        assert_eq!(cell(&Value::Null), "-");
        assert_eq!(cell(&json!("x")), "x");
        assert_eq!(cell(&json!(3)), "3");
        assert_eq!(bytes(&json!(512)), "512B");
        assert_eq!(bytes(&json!(10u64 << 30)), "10.0GiB");
        assert_eq!(bytes(&Value::Null), "-");
    }
}
```

- [ ] **Step 3: API 클라이언트와 콘솔 프록시**

`crates/oplctl/src/client.rs`:

```rust
//! opLite REST API 클라이언트.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    pub base: String,
    http: reqwest::Client,
    pub token: Option<String>,
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            token,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base)
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, self.url(path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    /// 성공하면 본문(JSON, 없으면 null), 실패하면 API 오류의 메시지와 코드
    pub async fn send(&self, r: RequestBuilder) -> anyhow::Result<Value> {
        let resp = r
            .send()
            .await
            .with_context(|| format!("cannot reach {}", self.base))?;
        let status = resp.status();
        let text = resp.text().await?;
        let body: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if status.is_success() {
            return Ok(body);
        }
        if status == StatusCode::UNAUTHORIZED && body["code"] == "unauthorized" {
            bail!("not logged in or the token is no longer valid; run `oplctl login`");
        }
        match (body["message"].as_str(), body["code"].as_str()) {
            (Some(message), Some(code)) => bail!("{message} ({code})"),
            _ => bail!("{status}: {body}"),
        }
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::POST, path).json(body)).await
    }

    pub async fn put(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        self.send(self.request(Method::PUT, path).json(body)).await
    }

    pub async fn delete(&self, path: &str) -> anyhow::Result<Value> {
        self.send(self.request(Method::DELETE, path)).await
    }

    /// 파일을 읽어 가며 본문으로 보낸다(이미지는 수 GB일 수 있다).
    pub async fn upload(
        &self,
        path: &str,
        query: &[(&str, &str)],
        file: &Path,
    ) -> anyhow::Result<Value> {
        let f = tokio::fs::File::open(file)
            .await
            .with_context(|| format!("cannot open {}", file.display()))?;
        let len = f.metadata().await?.len();
        let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(f));
        let r = self
            .request(Method::POST, path)
            .query(query)
            .header(reqwest::header::CONTENT_LENGTH, len)
            .body(body);
        self.send(r).await
    }

    /// Task가 끝날 때까지 기다린다. 실패했으면 그 오류를 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> anyhow::Result<Value> {
        let id = task["id"].as_str().context("response has no task id")?;
        loop {
            let task = self.get(&format!("/tasks/{id}")).await?;
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(task),
                Some("failed") => bail!(
                    "task {id} failed: {}",
                    task["status"]["error"].as_str().unwrap_or("unknown error")
                ),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }
}
```

`crates/oplctl/src/console.rs`:

```rust
//! 로컬 TCP 포트를 열어 VNC 뷰어의 연결을 API의 콘솔 WebSocket으로 잇는다.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::client::Client;

/// 연결을 받을 때마다 WebSocket을 하나 연다. 호출자가 멈출 때까지 돈다.
pub async fn serve(client: Client, vm: String, listener: TcpListener) -> anyhow::Result<()> {
    loop {
        let (tcp, _) = listener.accept().await?;
        let client = client.clone();
        let vm = vm.clone();
        tokio::spawn(async move {
            if let Err(e) = bridge(&client, &vm, tcp).await {
                eprintln!("console connection closed: {e:#}");
            }
        });
    }
}

async fn bridge(client: &Client, vm: &str, mut tcp: TcpStream) -> anyhow::Result<()> {
    let url = client
        .url(&format!("/vms/{vm}/console"))
        .replacen("http", "ws", 1);
    let mut req = url.into_client_request()?;
    if let Some(token) = &client.token {
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse()?);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .context("cannot open console")?;
    let (mut sink, mut stream) = ws.split();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        tokio::select! {
            n = tcp.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ok(());
                }
                sink.send(Message::binary(buf[..n].to_vec())).await?;
            }
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(data))) => tcp.write_all(&data).await?,
                Some(Ok(Message::Close(_))) | None => return Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            },
        }
    }
}
```

- [ ] **Step 4: 명령**

`crates/oplctl/src/cli.rs`:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::Client;
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long)]
        path: PathBuf,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    Delete {
        pool: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
    },
    Delete {
        volume: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    Delete {
        vm: String,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 호스트에 맞춘다(Apple Silicon: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(지금은 user만). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
    nics: Vec<String>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let token = login(&server, args).await?;
        // 같은 서버에 다시 로그인하면 이전 토큰은 서버에서도 지운다.
        if let (Some(old), Some(old_server)) = (&config.token, &config.server)
            && old_server == &server
            && let Some(id) = token_id(old)
        {
            let client = Client::new(&server, Some(token.clone()));
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        config.server = Some(server.clone());
        config.token = Some(token);
        config.save(&config_path)?;
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone());
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &[
                        "NAME",
                        "ID",
                        "ADDRESS",
                        "ONLINE",
                        "CPUS",
                        "MEMORY",
                        "MAINTENANCE",
                    ],
                    |n| {
                        vec![
                            cell(&n["name"]),
                            cell(&n["id"]),
                            cell(&n["spec"]["address"]),
                            cell(&n["status"]["online"]),
                            cell(&n["status"]["cpus"]),
                            bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
                            cell(&n["spec"]["maintenance"]),
                        ]
                    },
                )?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                print_list(out, json_out, &list, &["NAME", "PATH", "NODES"], |p| {
                    let nodes = p["spec"]["nodes"].as_array().map_or(0, Vec::len);
                    vec![
                        cell(&p["name"]),
                        cell(&p["spec"]["backend"]["path"]),
                        if nodes == 0 {
                            "all".into()
                        } else {
                            nodes.to_string()
                        },
                    ]
                })?;
            }
            PoolCmd::Get { pool } => {
                print_json(out, &client.get(&format!("/storage-pools/{pool}")).await?)?
            }
            PoolCmd::Create { name, path, nodes } => {
                let body = json!({
                    "name": name,
                    "spec": { "backend": { "kind": "dir", "path": path }, "nodes": nodes }
                });
                client.post("/storage-pools", &body).await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    |t| {
                        vec![
                            cell(&t["id"]),
                            cell(&t["spec"]["op"]["kind"]),
                            cell(&t["spec"]["target"]),
                            cell(&t["status"]["state"]),
                            cell(&t["status"]["error"]),
                        ]
                    },
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "PID", "ERROR"],
                |v| {
                    vec![
                        cell(&v["name"]),
                        cell(&v["spec"]["power"]),
                        cell(&v["status"]["phase"]),
                        cell(&v["status"]["node"]),
                        cell(&v["status"]["pid"]),
                        cell(&v["status"]["last_error"]),
                    ]
                },
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let machine = args.machine.unwrap_or(if cfg!(target_arch = "aarch64") {
                Machine::Virt
            } else {
                Machine::Q35
            });
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?["id"].clone()),
                None => None,
            };
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args
                .nics
                .iter()
                .map(|network| json!({ "network": network, "model": "virtio" }))
                .collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Delete { vm } => {
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None);
    let http = reqwest::Client::new();
    let resp = http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/oplctl/src/lib.rs`:

```rust
//! opLite 명령줄 클라이언트. `oplctl` 바이너리와 E2E 테스트가 함께 쓴다.

mod cli;
mod client;
mod config;
mod console;
mod output;

pub use cli::run;
pub use client::Client;
```

`crates/oplctl/src/main.rs`:

```rust
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let mut stdout = std::io::stdout();
    match oplctl::run(std::env::args_os(), &mut stdout).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast::<clap::Error>() {
            Ok(clap_err) => clap_err.exit(),
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}
```

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p oplctl && cargo run -q -p oplctl -- --help`
Expected: 단위 6개 통과(설정 파일 권한, 표 정렬, 크기 파싱, 토큰 id, clap 정의 검사), 명령 목록 출력. 서버와 함께 쓰는 테스트는 Task 11, 12에서 한다.

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo deny check`
Expected: 통과

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/oplctl
git commit -m "feat(oplctl): 로그인, 풀·볼륨·VM·Task·사용자·토큰 명령, VNC 콘솔 프록시"
```

---

### Task 11: `opld`

**시작 순서:**
1. `{data_dir}/node-id`: 처음 시작할 때 만든다.
2. 클러스터 저장소를 열고, 멤버십이 없으면 1인 클러스터로 초기화한 뒤 리더가 되기를 기다린다.
3. 사용자가 없으면 `admin`을 만들고 비밀번호를 `{data_dir}/initial-admin-password`(0600)에 쓴다.
4. Node 리소스를 만들거나 주소를 맞추고, 온라인·CPU·메모리를 기록한다.
5. `QemuHypervisor`를 띄운다(살아 있는 VM에 다시 붙는다).
6. reconciler와 Task 실행기를 띄운다. 실행기는 API보다 먼저 띄운다.
7. API를 연다.

**종료 순서:** SIGTERM/SIGINT를 받으면 다음 순서로 내린다. VM은 건드리지 않는다.
1. 서버. 열린 WebSocket은 5초 기다린 뒤 끊는다.
2. Task 실행기.
3. reconciler.
4. Node를 오프라인으로 표시한다.
5. 클러스터 저장소.

노드 간 네트워크가 아직 없으므로 `memnet` 라우터에 자기 자신만 등록한다(계획 6에서 gRPC로 바꾼다). 하이퍼바이저 프로필은 지금은 Apple Silicon(hvf)뿐이다. Linux에서도 API와 저장소는 돌지만 VM은 시작하지 못한다(M7).

설정 예:

```toml
name = "node1"                        # 필수
listen = "127.0.0.1:8006"             # 기본값
data_dir = "/var/lib/opl"             # 기본값
run_dir = "/run/opl"                  # 기본값. 유닉스 소켓 경로 길이 때문에 짧게
qemu_share_dir = "/opt/homebrew/share/qemu"
qemu_img = "qemu-img"
[reconciler]
acpi_timeout_secs = 60
```

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/opld/Cargo.toml`, `crates/opld/src/{main,config,bootstrap,host}.rs`, `crates/opld/tests/common/mod.rs`, `crates/opld/tests/smoke.rs`

- [ ] **Step 1: 의존성**

루트 `Cargo.toml`의 `[workspace.dependencies]` 끝에 추가한다:

```toml
toml = "1.1.8"
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }
```

`crates/opld/Cargo.toml`:

```toml
[package]
name = "opld"
description = "opLite node daemon"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# 실제 QEMU로 CLI부터 콘솔, kill -9 뒤 재연결까지 확인하는 E2E 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []

[dependencies]
anyhow.workspace = true
axum.workspace = true
clap.workspace = true
openraft.workspace = true
opl-api.workspace = true
opl-cluster.workspace = true
opl-driver.workspace = true
opl-qemu.workspace = true
opl-reconciler.workspace = true
opl-types.workspace = true
serde.workspace = true
tokio.workspace = true
toml.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
uuid.workspace = true

[dev-dependencies]
oplctl.workspace = true
reqwest.workspace = true
rustix.workspace = true
serde_json.workspace = true
tempfile.workspace = true
```

- [ ] **Step 2: 실패하는 테스트 작성**

하네스는 `opld` 프로세스를 임시 디렉터리(유닉스 소켓 경로 길이 때문에 `/tmp` 아래)와 빈 포트로 띄우고, `oplctl`을 라이브러리로 부른다.

`crates/opld/tests/common/mod.rs`:

```rust
//! opld 프로세스를 띄우고 oplctl(라이브러리)로 조작하는 하네스.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use tempfile::TempDir;

pub struct Daemon {
    pub dir: TempDir,
    pub port: u16,
    child: Option<Child>,
}

/// 비어 있는 포트를 하나 고른다.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Daemon {
    /// 유닉스 소켓 경로 길이 제한 때문에 /tmp 아래 짧은 경로를 쓴다.
    pub fn new(extra_config: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap();
        let port = free_port();
        let share =
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
            "name = \"node1\"\nlisten = \"127.0.0.1:{port}\"\ndata_dir = \"{data}\"\n\
             run_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}",
            data = dir.path().join("data").display(),
            run = dir.path().join("run").display(),
        );
        std::fs::write(dir.path().join("opld.toml"), config).unwrap();
        Self {
            dir,
            port,
            child: None,
        }
    }

    pub fn server(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// opld를 띄우고 API가 응답할 때까지 기다린다.
    pub async fn start(&mut self) {
        assert!(self.child.is_none());
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("opld.log"))
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_opld"))
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let url = format!("{}/api/v1/auth/whoami", self.server());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(resp) = reqwest::get(&url).await
                && resp.status() == reqwest::StatusCode::UNAUTHORIZED
            {
                return;
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("opld exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "opld did not start:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// SIGTERM을 보내고 끝나기를 기다린다.
    pub fn stop(&mut self) {
        let mut child = self.child.take().unwrap();
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&child),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let status = child.wait().unwrap();
        assert!(
            status.success(),
            "opld exited with {status}:\n{}",
            self.log()
        );
    }

    /// kill -9
    pub fn kill(&mut self) {
        let mut child = self.child.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(self.path("opld.log")).unwrap_or_default()
    }

    /// oplctl 명령을 이 데몬에 대고 실행한다. 로그인 정보는 임시 디렉터리에 둔다.
    pub async fn ctl(&self, args: &[&str]) -> anyhow::Result<String> {
        let config = self.path("oplctl.json");
        let mut argv = vec!["oplctl", "--config", config.to_str().unwrap()];
        argv.extend_from_slice(args);
        let mut out = Vec::new();
        oplctl::run(argv, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    pub async fn login(&self) {
        let password = self.data_dir().join("initial-admin-password");
        let server = self.server();
        self.ctl(&[
            "login",
            "--server",
            &server,
            "--user",
            "admin",
            "--password-file",
            password.to_str().unwrap(),
        ])
        .await
        .unwrap();
    }

    /// `oplctl -o json`의 결과
    pub async fn ctl_json(&self, args: &[&str]) -> serde_json::Value {
        let mut argv = vec!["-o", "json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
```

`crates/opld/tests/smoke.rs` (QEMU 없이 돈다):

```rust
//! QEMU 없이 opld를 띄워 CLI로 다루고, 다시 띄워도 상태가 남는지 본다.

mod common;

use common::Daemon;

#[tokio::test]
async fn opld_serves_cli_and_keeps_state_across_restarts() {
    let mut d = Daemon::new("");
    d.start().await;
    // 첫 시작에서 관리자 비밀번호를 파일로 남긴다(0600).
    let password_file = d.data_dir().join("initial-admin-password");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&password_file).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
    d.login().await;

    let nodes = d.ctl_json(&["node", "list"]).await;
    assert_eq!(nodes[0]["name"], "node1");
    assert_eq!(nodes[0]["status"]["online"], true);
    assert!(nodes[0]["status"]["memory_mib"].as_u64().unwrap() > 0);

    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&["vm", "create", "web", "--memory", "256", "--nic", "user"])
        .await
        .unwrap();
    let err = d
        .ctl(&["vm", "create", "web", "--memory", "256"])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("name_taken"), "{err}");

    let password = std::fs::read_to_string(&password_file).unwrap();
    d.stop();
    d.start().await;
    // 비밀번호는 다시 만들지 않고, 저장된 토큰도 계속 쓸 수 있다.
    assert_eq!(std::fs::read_to_string(&password_file).unwrap(), password);
    let vms = d.ctl_json(&["vm", "list"]).await;
    assert_eq!(vms[0]["name"], "web");
    let pools = d.ctl(&["pool", "list"]).await.unwrap();
    assert!(pools.contains("local"), "{pools}");

    d.ctl(&["logout"]).await.unwrap();
    let err = d.ctl(&["vm", "list"]).await.unwrap_err();
    assert!(err.to_string().contains("oplctl login"), "{err}");
    d.stop();
}
```

- [ ] **Step 3: 설정, 부트스트랩, 호스트 정보**

`crates/opld/src/config.rs`:

```rust
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// API를 받을 주소. TLS는 클러스터 부트스트랩(계획 6)에서 더하므로 그때까지는 루프백에만 연다.
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 클러스터 저장소, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리. 지금은 Apple Silicon(hvf)만 지원한다(Linux/KVM은 M7).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilerSection {
    /// ACPI 종료를 요청한 뒤 강제 종료하기까지 기다리는 시간(초)
    #[serde(default = "default_acpi_timeout")]
    pub acpi_timeout_secs: u64,
}

impl Default for ReconcilerSection {
    fn default() -> Self {
        Self {
            acpi_timeout_secs: default_acpi_timeout(),
        }
    }
}

impl ReconcilerSection {
    pub fn acpi_timeout(&self) -> Duration {
        Duration::from_secs(self.acpi_timeout_secs)
    }
}

fn default_listen() -> String {
    "127.0.0.1:8006".into()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/opl".into()
}
fn default_run_dir() -> PathBuf {
    "/run/opl".into()
}
fn default_qemu_share_dir() -> PathBuf {
    "/opt/homebrew/share/qemu".into()
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        Ok(toml::from_str(&text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_defaults() {
        let c: Config = toml::from_str(r#"name = "node1""#).unwrap();
        assert_eq!(c.listen, "127.0.0.1:8006");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("name = \"n\"\nlisten_addr = \"x\"").is_err());
        let c: Config =
            toml::from_str("name = \"n\"\n[reconciler]\nacpi_timeout_secs = 5").unwrap();
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }
}
```

`crates/opld/src/bootstrap.rs`:

```rust
//! 노드가 시작할 때 한 번 하는 일: 노드 id, 1인 클러스터 초기화, 첫 관리자, Node 리소스.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use openraft::{BasicNode, ServerState};
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use uuid::Uuid;

use crate::config::Config;

pub const NODE_ID_FILE: &str = "node-id";
pub const ADMIN_PASSWORD_FILE: &str = "initial-admin-password";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            std::fs::write(&path, format!("{id}\n"))?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 아직 멤버십이 없으면 이 노드 하나로 클러스터를 시작하고, 리더가 될 때까지 기다린다.
/// 다른 노드의 합류는 계획 6에서 다룬다.
pub async fn start_cluster(cluster: &Cluster, address: &str) -> anyhow::Result<()> {
    let id = cluster.id();
    if !cluster.raft().is_initialized().await? {
        tracing::info!(node = %id, "initializing a new single-node cluster");
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(address))]))
            .await?;
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| m.state == ServerState::Leader && m.current_leader == Some(id),
            "become leader",
        )
        .await?;
    Ok(())
}

/// 사용자가 하나도 없으면 `admin`을 만들고 비밀번호를 `{data_dir}/initial-admin-password`(0600)에 쓴다.
pub async fn ensure_admin(cluster: &Cluster, data_dir: &Path) -> anyhow::Result<()> {
    if !cluster.list::<UserSpec>()?.1.is_empty() {
        return Ok(());
    }
    let password = Uuid::new_v4().simple().to_string();
    let path = data_dir.join(ADMIN_PASSWORD_FILE);
    // 사용자를 만들기 전에 파일을 쓴다. 그 사이 죽으면 다음 시작에서 새 비밀번호로 덮어쓴다.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    writeln!(file, "{password}")?;
    file.sync_all()?;
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password)
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
        },
    );
    cluster.create(&admin).await?;
    tracing::info!(path = %path.display(), "created user admin; its password is in this file");
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량을 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address: config.listen.clone(),
                    maintenance: false,
                },
            );
            node.id = id;
            cluster.create(&node).await?;
        }
        Some(node) if node.spec.address != config.listen => {
            let spec = NodeSpec {
                address: config.listen.clone(),
                ..node.spec
            };
            cluster.update_spec(id, None, node.labels, &spec).await?;
        }
        Some(_) => {}
    }
    let status = NodeStatus {
        online: true,
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        memory_mib: crate::host::memory_mib(),
        hypervisor: "qemu-hvf".into(),
    };
    cluster.update_status::<NodeSpec>(id, &status).await?;
    Ok(())
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let NodeId(id) = cluster.id();
    let Ok(Some(node)) = cluster.get::<NodeSpec>(id) else {
        return;
    };
    let status = NodeStatus {
        online: false,
        ..node.status
    };
    if let Err(e) = cluster.update_status::<NodeSpec>(id, &status).await {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }
}
```

`crates/opld/src/host.rs`:

```rust
//! 호스트 용량. 스케줄러가 노드를 고를 때 쓴다.

/// 물리 메모리(MiB). 알 수 없으면 0이다.
pub fn memory_mib() -> u64 {
    bytes().map_or(0, |b| b >> 20)
}

#[cfg(target_os = "linux")]
fn bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(target_os = "macos")]
fn bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_host_memory() {
        assert!(super::memory_mib() > 0);
    }
}
```

- [ ] **Step 4: 조립**

`crates/opld/src/main.rs`:

```rust
//! opLite 노드 데몬. 클러스터 저장소, reconciler, Task 실행기, REST API를 한 프로세스로 묶는다.
//!
//! QEMU는 `-daemonize`로 떠 있으므로 opld가 멈추거나 죽어도 VM은 계속 돈다. 다시 시작하면
//! 하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙고 reconciler가 이어서 맞춘다.

mod bootstrap;
mod config;
mod host;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DirStorage, Hypervisor, QemuConfig, QemuHypervisor, Storage};
use opl_qemu::HostProfile;
use opl_reconciler::{Reconciler, ReconcilerConfig};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(version, about = "opLite node daemon")]
struct Args {
    #[arg(long, default_value = "/etc/opl/opld.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,openraft=warn")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    run(config).await
}

async fn run(config: Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.data_dir)?;
    let node = bootstrap::node_id(&config.data_dir)?;
    let id = NodeId(node);

    // 노드 간 네트워크(gRPC + mTLS)는 계획 6에서 만든다. 그때까지는 1인 클러스터다.
    let router = MemRouter::default();
    let cluster = Arc::new(
        Cluster::open(
            ClusterConfig::new(id, config.data_dir.join("cluster")),
            router.network(id),
        )
        .await?,
    );
    router.register(id, cluster.raft().clone());
    bootstrap::start_cluster(&cluster, &config.listen).await?;
    bootstrap::ensure_admin(&cluster, &config.data_dir).await?;
    bootstrap::register_node(&cluster, &config).await?;

    let hv: Arc<dyn Hypervisor> = Arc::new(
        QemuHypervisor::new(QemuConfig {
            host: HostProfile::homebrew_hvf(&config.qemu_share_dir),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
        })
        .await?,
    );
    let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
    let mut reconciler_config = ReconcilerConfig::new(node);
    reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
    let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
    // Task 실행기는 시작할 때 Task 없는 업로드를 지우므로 API 서버보다 먼저 띄운다.
    let mut runner_config = TaskRunnerConfig::new(node);
    runner_config.upload_dir = Some(config.data_dir.join("uploads"));
    let runner = TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config);
    let state = Arc::new(AppState::new(
        cluster.clone(),
        node,
        hv,
        config.data_dir.join("uploads"),
    ));

    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(node = %node, addr = %listener.local_addr()?, "opld is ready");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, opl_api::router(state))
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
    });

    tokio::select! {
        () = shutdown_signal() => {}
        result = &mut server => {
            // 서버가 스스로 멈췄다(포트 오류 등).
            result??;
        }
    }
    tracing::info!("shutting down; running VMs keep running");
    let _ = stop_tx.send(());
    if tokio::time::timeout(DRAIN_TIMEOUT, &mut server)
        .await
        .is_err()
    {
        server.abort();
    }
    runner.shutdown().await;
    reconciler.shutdown().await;
    bootstrap::mark_offline(&cluster).await;
    cluster.shutdown().await?;
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
```

- [ ] **Step 5: 통과 확인**

Run: `cargo test -p opld`
Expected: 단위 4개, `smoke` 1개 통과

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check && cargo deny check`
Expected: 통과

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/opld
git commit -m "feat(opld): 노드 데몬 (1인 클러스터 부트스트랩, 첫 관리자, Node 등록, 정상 종료)"
```

---

### Task 12: 실제 QEMU E2E (M5 완료 기준)

CLI로 풀·볼륨을 만들고 이미지를 가져오고, VM을 만들어 켜고, 콘솔 포트에서 VNC 인사말(`RFB 003.008`)을 받는다. 그다음 `opld`를 `kill -9` 한다. VM 프로세스가 살아 있고, 다시 띄운 `opld`가 같은 pid의 VM을 running으로 보고하며, 강제 종료할 수 있어야 한다. 디스크에 OS가 없는 UEFI 게스트는 ACPI에 응하지 않을 수 있어 `acpi_timeout_secs = 5`로 띄운다.

**Files:**
- Create: `crates/opld/tests/e2e.rs`

- [ ] **Step 1: 테스트 작성**

`crates/opld/tests/e2e.rs`:

```rust
//! M5 완료 기준: CLI로 VM을 만들어 콘솔에 붙고, opld를 kill -9 해도 VM이 계속 돌다가 다시 제어된다.
//! `cargo test -p opld --features qemu-it --test e2e`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::time::Duration;

use common::{Daemon, free_port};
use rustix::process::{Pid, test_kill_process};
use tokio::io::AsyncReadExt;

fn alive(pid: u64) -> bool {
    test_kill_process(Pid::from_raw(pid as i32).unwrap()).is_ok()
}

#[tokio::test]
async fn cli_creates_vm_opens_console_and_survives_kill_9() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.login().await;

    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    // 이미지 가져오기: oplctl이 파일을 스트리밍으로 올린다.
    let image = d.path("data.raw");
    std::fs::write(&image, vec![1u8; 4 << 20]).unwrap();
    let image_s = image.to_str().unwrap();
    d.ctl(&[
        "volume",
        "import",
        "data",
        "--pool",
        "local",
        "--file",
        image_s,
        "--source-format",
        "raw",
    ])
    .await
    .unwrap();
    let data = d.ctl_json(&["volume", "get", "data"]).await;
    assert_eq!(data["spec"]["size_bytes"], 4 << 20);
    assert_eq!(data["status"]["phase"], "ready");
    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let pid = vm["status"]["pid"].as_u64().unwrap();
    assert!(alive(pid));

    // 콘솔: oplctl이 연 로컬 포트로 VNC 서버의 인사말이 온다. 블록을 나가면 콘솔을 닫는다.
    {
        let port = free_port();
        let port_s = port.to_string();
        let args = ["vm", "console", "web", "--port", &port_s];
        let console = d.ctl(&args);
        tokio::pin!(console);
        let greeting = async {
            let mut tcp = loop {
                match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    Ok(tcp) => break tcp,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            let mut buf = [0u8; 12];
            tcp.read_exact(&mut buf).await.unwrap();
            buf
        };
        tokio::select! {
            r = &mut console => panic!("console exited: {r:?}"),
            buf = tokio::time::timeout(Duration::from_secs(10), greeting) => {
                assert_eq!(&buf.unwrap(), b"RFB 003.008\n");
            }
        }
    }

    // opld가 죽어도 VM은 그대로 돈다.
    d.kill();
    assert!(alive(pid));
    d.start().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    assert_eq!(vm["status"]["pid"].as_u64(), Some(pid));

    // 다시 붙은 VM을 끌 수 있다.
    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    assert!(!alive(pid));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["volume", "delete", "data"]).await.unwrap();
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
    d.stop();
}
```

- [ ] **Step 2: 실행**

Run: `cargo test -p opld --features qemu-it --test e2e`
Expected: 1개 통과 (Apple Silicon Mac + `brew install qemu`)

Run: `pgrep -fl qemu-system`
Expected: 출력 없음 (남은 QEMU 없음)

- [ ] **Step 3: 전체 통합 테스트**

Run: `cargo test --workspace --all-features`
Expected: 모두 통과 (계획 1~4의 qemu-it, storage-it 테스트 포함)

- [ ] **Step 4: Commit**

```bash
git add crates/opld/tests/e2e.rs
git commit -m "test(opld): 실제 QEMU E2E — CLI, 콘솔, kill -9 뒤 재연결 (M5)"
```

---

### Task 13: 설계 문서, README 갱신

**Files:**
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`, `README.md`

- [ ] **Step 1: 설계 문서**

3.2절 표의 세 줄을 바꾼다:

```markdown
| `Volume` | 소속 풀, 크기, 형식(qcow2/raw) | 준비 상태(Pending/Ready), 할당 크기 |
| `Task` | 작업 종류, 대상 리소스, 실행 노드 | 상태(Pending/Running/Succeeded/Failed), 오류 |
| `User`, `ApiToken` | 사용자 이름, 비밀번호 해시(argon2), 역할 / 주인, 토큰 비밀 값의 SHA-256 | — |
```

4절 전체(제목 아래 목록)를 다음으로 바꾼다:

```markdown
- 경로는 `/api/v1/{resource}`이며, 목록·조회·생성·수정·삭제를 제공한다. 경로의 `{id}` 자리에는 id나 이름을 쓸 수 있다(UUID 모양의 이름은 받지 않는다). 목록은 `{revision, items}`를 돌려준다.
- 동작은 `POST /api/v1/vms/{id}/actions/{start|stop|reboot}`이다. 응답은 `202 Accepted`와 `Task`다.
  - `start`/`stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 만든다. 대상 노드의 Task 실행기가 VM이 원하는 상태에 도달하면 Task를 끝낸다.
  - `stop?mode=force`는 reconciler의 VM별 작업자 큐로 강제 종료를 보낸다. `reboot`은 원하는 상태를 stopped로 바꿨다가 꺼지면 running으로 되돌린다. 그 사이 같은 VM에 새 요청이 오면 그것을 따른다.
- 볼륨 생성·삭제와 이미지 가져오기(`POST /api/v1/volumes/import`, 본문이 이미지)도 Task다. 파일이 준비되기 전의 볼륨은 VM에 붙일 수 없다.
- 감시: `GET /api/v1/watch?kinds=vm,task&since=<revision>` (WebSocket). 목록의 revision 다음 변경부터 받는다. 이어 받을 수 없으면 오류 메시지를 보내고 닫으며, 클라이언트는 목록을 다시 읽는다. 사용자와 토큰은 감시할 수 없다.
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다(M6).
- OpenAPI 스펙은 `utoipa`로 코드에서 생성해 `GET /api/v1/openapi.json`으로 내보낸다. 라우터와 같은 정의에서 만든다.
- 인증: 로컬 사용자(세션 쿠키, 웹 UI용)와 API 토큰(Bearer, CLI용)을 지원한다. 토큰은 `opl_<id>_<비밀 값>`이고 비밀 값의 SHA-256만 저장한다. 쿠키로 인증한 요청은 `Origin`이 서버와 같아야 한다. 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고, 바꾸면 그 사용자의 세션이 끝난다. 모든 핸들러는 `Authorizer::check(principal, action, resource)`를 거친다. 1차 구현은 `admin`/`operator`/`viewer` 고정 역할이다.
- 오류 응답은 `{ "code": "...", "message": "...", "details": {...} }` 형식으로 통일한다. Raft 과반수가 없으면 `503 cluster_no_quorum`, revision이 다르면 `412 revision_mismatch`, 이름이 겹치면 `409 name_taken`을 반환한다.
```

5.1절 코드 블록의 2~5단계를 다음으로 바꾼다:

```
  2. Raft에 제안 → 과반수 커밋 → Task(Pending, node = 대상 노드) 생성 → 202 + task
                                  │ 커밋 스트림
                                  ▼
대상 노드의 reconciler
  3. desired(running) ≠ actual(stopped) → Hypervisor::start
  4. 커맨드라인 생성 → QEMU 실행(-S, -daemonize) → QMP 연결 → qmp_capabilities → cont
  5. status(running, pid)를 Raft에 기록
대상 노드의 Task 실행기
  6. VM의 power와 phase가 모두 running이 되면 Task Succeeded (reconciler가 포기하면 Failed)
```

8절 마지막 줄(2노드 클러스터) 아래에 추가한다:

```markdown
- M5까지는 `opld`가 첫 시작에서 1인 클러스터를 만들고, 사용자가 없으면 `admin`을 만들어 비밀번호를 `{data_dir}/initial-admin-password`(0600)에 남긴다. API는 루프백 HTTP로만 연다. `cluster init`과 HTTPS가 생기면(M6) 그쪽으로 옮긴다.
```

10절 표의 E2E 줄 위에 줄을 더한다:

```markdown
| E2E | oplctl로 VM 생성 → 콘솔 → opld kill -9 → 재연결 (opld 프로세스 + oplctl 라이브러리) | `--features qemu-it` | macOS(hvf) (M5) |
```

- [ ] **Step 2: README**

`README.md` 전체:

````markdown
# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M5)

## 실행 (macOS, Apple Silicon)

```bash
brew install qemu
cat > opld.toml <<'TOML'
name = "node1"
data_dir = "/tmp/opl/data"
run_dir = "/tmp/opl/run"
TOML
cargo run -p opld -- --config opld.toml          # API: http://127.0.0.1:8006
cargo run -p oplctl -- login --server http://127.0.0.1:8006 \
    --password-file /tmp/opl/data/initial-admin-password
cargo run -p oplctl -- pool create local --path /tmp/opl/pool
cargo run -p oplctl -- volume create root --pool local --size 1G
cargo run -p oplctl -- vm create web --memory 512 --disk root --nic user --start
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
```

API 문서(OpenAPI)는 `http://127.0.0.1:8006/api/v1/openapi.json`에 있다.

## 개발

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: apt install qemu-utils)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (Apple Silicon)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: opld 재시작 후 재연결
cargo test -p opl-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
```

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
- [계획 2: 하이퍼바이저·스토리지 드라이버](docs/superpowers/plans/2026-10-20-plan2-opl-driver.md)
- [계획 3: 클러스터 저장소](docs/superpowers/plans/2026-10-21-plan3-opl-cluster.md)
- [계획 4: reconciler](docs/superpowers/plans/2026-10-21-plan4-opl-reconciler.md)
- [계획 5: API, opld, oplctl](docs/superpowers/plans/2026-10-21-plan5-api-opld-oplctl.md)
````

- [ ] **Step 3: 전체 확인**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace && cargo deny check`
Expected: 모두 통과

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-10-20-opl-node-core-design.md README.md
git commit -m "docs: API·Task·인증·E2E를 설계에 반영, README에 실행 방법"
```

---

## 완료 기준 (M5)

- [ ] `cargo test --workspace`가 통과한다 (macOS, Linux CI). CI는 opl-api 볼륨 테스트도 돌린다.
- [ ] REST API로 풀·볼륨·VM을 만들고 시작·정지·강제 종료·재부팅할 수 있고, 각 동작은 Task로 결과를 남긴다.
- [ ] 로그인, API 토큰, 고정 역할이 동작하고, 비밀번호·토큰 해시는 응답과 변경 구독에 나오지 않는다.
- [ ] 다른 파일을 참조하는 이미지는 가져오지 않는다.
- [ ] `/api/v1/openapi.json`이 모든 경로를 담는다.
- [ ] 실제 QEMU에서 CLI로 VM 생성 → 콘솔 접속 → `opld` `kill -9` → 같은 pid로 재연결 → 강제 종료가 된다.

## 알고 받아들인 제한

- 1인 클러스터만 된다. 팔로워의 쓰기 전달, 다른 노드의 콘솔 중계, 노드 heartbeat는 계획 6이다. 지금 Node의 `online`은 opld가 시작·정상 종료할 때만 바뀐다.
- API는 루프백 HTTP다. 세션 쿠키에 `Secure`가 없고, 토큰이 평문으로 오간다(HTTPS는 계획 6).
- 로그인 시도 횟수를 제한하지 않는다. 로그인마다 argon2를 돌리므로 많이 몰리면 CPU를 쓴다.
- API 토큰 이름은 모든 사용자에 걸쳐 유일하다(다른 사용자가 쓴 이름이면 409).
- `oplctl vm console`의 로컬 포트는 인증이 없다. 같은 호스트의 다른 사용자가 그 포트로 VNC에 붙을 수 있다.
- 비밀번호를 바꿔도 이미 만든 API 토큰은 그대로다(세션만 끝난다).
- 볼륨을 붙이는 검사(API)와 볼륨을 지우는 검사(Task 실행기)는 저장소 트랜잭션이 아니다. 아주 짧은 사이에 겹치면 지울 수 있다.
- 끝난 Task는 지우지 않는다(보존 기간은 나중에).
- 가져오기 원본 검사는 `qemu-img info`가 보고하는 파일 이름에 기댄다. 오래된 QEMU는 qcow2 외부 데이터 파일을 `data-file`로만 보고하므로 그 키도 확인한다.

## 실행 후 기록 (2026-10-21)

Task 리뷰와 최종 리뷰를 거쳐 계획의 코드에서 아래가 바뀌었다. 저장소의 코드가 기준이다.

Task 1~3 리뷰(`6279ee2`):
- **가져올 이미지는 헤더부터 본다.** `qemu-img info`는 보고하려고 VMDK extent와 qcow2 외부 데이터 파일을 실제로 연다. FIFO나 `nbd://`를 가리키는 업로드가 검사를 멈추게 하거나 연결을 열 수 있었다. 이제 텍스트 VMDK 디스크립터, backing file이 있는 qcow2, 외부 데이터 파일 비트가 켜진 qcow2를 qemu-img를 부르기 전에 거부한다. `qemu-img info`에는 60초 제한을 둔다(`INFO_TIMEOUT`).
- 강제 종료 플래그는 원하는 상태가 stopped일 때만 세운다. `UserStatus`, `ApiTokenStatus`에 `#[serde(default)]`.

Task 4~6 리뷰(`6efd71a`):
- **`TaskRunner::spawn`은 async다.** 고아 정리(Task 없는 Pending 볼륨, 업로드 파일)를 마친 뒤에 돌아오므로, opld는 그것을 기다린 뒤 API를 연다. 계획대로면 API가 막 만든 볼륨이나 받는 중인 파일을 정리가 지울 수 있었다. 정리는 Task 목록보다 나중 revision의 볼륨을 건드리지 않는다.
- **reboot은 끄기 전에도 나중 요청을 확인한다.** reboot 직후 start를 보내면 start는 성공으로 끝나고 VM은 꺼진 채 남았다. 회귀 테스트: `start_right_after_reboot_leaves_the_vm_running`.
- 관리자가 아닌 사용자의 `PUT /users/{key}`는 자기 자신이 아니면 조회 전에 403이다(사용자 존재 여부를 알리지 않는다). 내부 오류는 응답에 "internal error"만 담고 자세한 내용은 로그에 남긴다.

Task 7~9 리뷰(`a986bd8`):
- **실행 중인 VM의 디스크는 바꿀 수 없다(409 `vm_running`).** 계획대로면 실행 중에 디스크를 빼서 그 볼륨을 지우거나 다른 VM에 붙일 수 있었다.
- **VM 검증과 쓰기를 노드 안에서 직렬화한다(`AppState::vm_lock`).** 동시에 만든 VM이 같은 볼륨이나 MAC을 쓸 수 있었다. 노드 사이의 직렬화는 계획 6에서 다시 본다.
- **`PUT /vms`는 읽은 revision으로 쓴다.** `If-Match`가 없으면 충돌 시 다시 읽어 검증하고, 있으면 다를 때 곧바로 412다.
- OpenAPI에 `servers: /api/v1`, 업로드 본문은 binary 스키마. 볼륨 이름 `import`는 예약어. 아직 만드는 중인 볼륨의 삭제는 409 `volume_not_ready`.
- OpenAPI 테스트에 남아 있던 디버깅용 파일 쓰기를 지웠다(`929a62d`).

최종 리뷰(`a0217a4`):
- **opld는 루프백 주소에서만 연다.** 설정의 `listen`이 루프백이 아니면 시작하지 않는다(TLS가 생기는 계획 6까지).
- 포트를 reconciler·실행기보다 먼저 연다. `node-id`와 oplctl 설정 파일은 임시 파일 + rename으로 쓴다. 다시 로그인하면 새 설정을 먼저 저장한 뒤 이전 토큰을 지우고, 서버 주소는 끝의 `/`를 무시하고 비교한다.
- README 실행 절차에 풀 디렉터리 만들기를 더했다(`dir` 풀은 디렉터리를 만들지 않는다).

알고 받아들인 동작(위 제한 목록에 더해):
- start 동작은 `power = running`을 쓴 뒤에 Task를 만든다. 그 사이에 reboot이 나중 요청을 확인하면 놓쳐 VM을 끌 수 있다. 이때 start Task가 "VM was stopped"로 실패해 드러난다.
- 두 관리자가 동시에 스스로 역할을 내리면 관리자가 없어질 수 있다(마지막 관리자 검사가 트랜잭션이 아니다).
- 이전 opld가 실행하다 멈춘 Task는 실패로 기록하고 만들던 볼륨을 지운다. 실제로는 끝났던 생성도 지워지고, 중단된 reboot은 VM을 꺼진 채 둔다.
- 이미 열린 watch·콘솔 WebSocket은 사용자를 지우거나 역할을 바꿔도 끊지 않는다. watch에는 keepalive가 없다.
- 업로드 전 남은 공간 확인은 예약이 아니다. 동시에 큰 업로드 둘이 모두 통과할 수 있다. 느린 업로드에 제한 시간이 없다.
- 볼륨의 옛 저장값(`phase` 없음)은 Pending으로 읽힌다. 아직 저장된 클러스터가 없어 문제되지 않는다.
- `PUT /vms`로 `power`를 바꾸면 Task 없이 바뀐다. 명시한 `node`는 스케줄러의 조건(온라인, 유지보수, 풀)을 거치지 않는다.

테스트: `opl-api` 단위 9개, `auth` 9개, `vms` 15개, `tasks` 3개, `streams` 3개, `openapi` 1개, `volumes` 5개(storage-it). `oplctl` 6개. `opld` 단위 5개, `smoke` 2개, `e2e` 1개(qemu-it). `opl-types` 20개, `opl-driver` 단위 33개, `reconcile` 19개. 테스트 바이너리 8개를 16개씩 동시에 각 24회 돌려 실패 없음. 실제 QEMU E2E 통과, 남은 QEMU 프로세스 없음.

## 다음 계획으로 넘길 사항

계획 6 (클러스터 부트스트랩, 노드 간 통신):
- `memnet`을 gRPC + mTLS로 바꾸고 HTTPS를 연다. 그 뒤 opld의 루프백 제한을 풀고 세션 쿠키에 `Secure`를 붙인다.
- 팔로워가 받은 API 쓰기를 리더로 전달한다(지금은 503 `not_leader`, `details`에 리더). 다른 노드에서 도는 VM의 콘솔은 노드 간 중계가 필요하다.
- 노드 heartbeat로 `NodeStatus.online`을 판정한다. 스케줄러가 쓴다.
- Task 실행기의 고아 정리는 지금 클러스터 전체의 볼륨을 본다. 노드가 여럿이면 자기 노드에서 실행할 볼륨만 보게 한다.
- VM 검증·쓰기 직렬화(`vm_lock`)는 노드 안에서만 유효하다. 볼륨·MAC 중복을 클러스터 수준에서 막으려면 상태 머신에서 검사하거나 리더로 모은다.
- `opld`의 첫 관리자 비밀번호 파일과 1인 클러스터 초기화를 `oplctl cluster init`/`join`으로 옮긴다.
- `oplctl`의 Task 기다리기에는 제한 시간이 없다. 다른 노드의 실행기가 없으면 Pending에 머문다.

나중에:
- 로그인 시도 제한, 감사 로그, 끝난 Task의 보존 기간, 비밀번호 변경 시 토큰 처리, 사용자별 토큰 이름.
- 열린 WebSocket의 권한 재확인과 keepalive.
- `dir` 풀 디렉터리 만들기(풀 생성 Task)와 풀 용량 status.
