# opLite 계획 1: 워크스페이스, 리소스 모델, QEMU 제어 (M0 + M1) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cargo 워크스페이스와 CI를 세우고, 리소스 모델(`opl-types`)과 QEMU 제어 크레이트(`opl-qemu`: 커맨드라인 생성 + QMP 클라이언트)를 만들어, Apple Silicon Mac에서 실제 QEMU VM을 일시정지 상태로 띄우고 → 실행 재개 → 재연결 → 종료하는 데까지 검증한다.

**Architecture:** `opl-types`는 serde 기반의 순수 데이터 타입만 담는다(spec/status 분리, `Resource<S, T>` 봉투). `opl-qemu`는 `VmRuntimeSpec`(이름 참조가 모두 풀린 VM 정의)을 QEMU 인자 목록으로 바꾸는 순수 함수와, tokio 기반 비동기 QMP 클라이언트(명령/응답 id 매칭 + 이벤트 브로드캐스트)를 제공한다. 프로세스 실행 관리(systemd scope, setsid)는 다음 계획의 `opl-driver`가 맡는다.

**Tech Stack:** Rust 1.85+ (edition 2024), tokio, serde/serde_json, thiserror, uuid(v7), insta(스냅샷 테스트), tempfile, QEMU 11.x(Homebrew, `-accel hvf`, 11.1.2에서 검증), GitHub Actions, cargo-deny

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`

---

## 전체 로드맵에서의 위치

| 계획 | 마일스톤 | 내용 | 상태 |
|---|---|---|---|
| **계획 1 (이 문서)** | M0 + M1 | 워크스페이스, CI, `opl-types`, `opl-qemu` | 작성됨 |
| 계획 2 | M2 | `opl-driver`: `Hypervisor`/`Storage` 트레이트, Mock, QemuHvf(프로세스 분리 실행, 재부착), `dir` 풀, VMDK 가져오기 | 계획 1 완료 후 작성 |
| 계획 3 | M3 | `opl-cluster`: openraft + redb 상태 머신, 리소스 CRUD, watch 스트림, 인메모리 3노드 테스트 | 계획 2 완료 후 작성 |
| 계획 4 | M4 | `opl-reconciler`: 조정 루프, VM별 작업 큐, 재시작 정책, kill -9 복구 | 계획 3 완료 후 작성 |
| 계획 5 | M5 | `opl-api` + `oplctl` + `opld`: REST, Task, 콘솔 WebSocket, 토큰 인증 | 계획 4 완료 후 작성 |
| 계획 6 | M6 | 클러스터 부트스트랩: CA, join/leave, mTLS | 계획 5 완료 후 작성 |
| 계획 7 | M7 | Linux `QemuKvm` + bridge 네트워크 + E2E | Linux 환경 확보 후 작성 |

뒤쪽 계획은 앞 계획에서 확정된 타입과 인터페이스를 기준으로 작성해야 정확하므로, 한 번에 하나씩 작성한다.

## 파일 구조

```
opLite/
├── Cargo.toml                         # 워크스페이스, 공통 의존성 버전
├── LICENSE                            # Apache-2.0 전문
├── deny.toml                          # 의존성 라이선스 허용 목록
├── .gitignore
├── .github/workflows/ci.yml           # fmt, clippy, test (Linux + macOS), cargo-deny
└── crates/
    ├── opl-types/
    │   ├── Cargo.toml
    │   └── src/
    │       ├── lib.rs                 # 모듈 선언만
    │       ├── resource.rs            # Resource<S, T> 봉투
    │       ├── vm.rs                  # VmSpec, VmStatus, MacAddr, 검증
    │       └── runtime.rs             # VmRuntimeSpec (드라이버 입력)
    └── opl-qemu/
        ├── Cargo.toml                 # feature `qemu-it`
        ├── src/
        │   ├── lib.rs                 # 모듈 선언 + 재노출
        │   ├── host.rs                # HostProfile, Arch, Accel
        │   ├── paths.rs               # RuntimePaths (소켓 경로 길이 검사)
        │   ├── cmdline.rs             # build_cmdline (순수 함수)
        │   ├── snapshots/             # insta 스냅샷
        │   └── qmp.rs                 # QmpClient
        └── tests/
            └── qemu_boot.rs           # 실제 QEMU 통합 테스트 (qemu-it)
```

---

### Task 1: 개발 환경과 워크스페이스 뼈대

**Files:**
- Create: `Cargo.toml`, `LICENSE`, `crates/opl-types/Cargo.toml`, `crates/opl-types/src/lib.rs`
- Modify: `.gitignore`

- [ ] **Step 1: 도구 확인과 QEMU 설치**

```bash
rustc --version          # 1.85 이상이어야 한다 (edition 2024)
brew install qemu
qemu-system-aarch64 --version
ls /opt/homebrew/share/qemu/edk2-aarch64-code.fd /opt/homebrew/share/qemu/edk2-arm-vars.fd
```

Expected: QEMU 버전이 출력되고 두 펌웨어 파일이 존재한다.

- [ ] **Step 2: 워크스페이스 매니페스트 작성**

`Cargo.toml`:

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
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.154"
thiserror = "2.0.21"
tokio = { version = "1.54.0", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time", "process"] }
uuid = { version = "1.28.0", features = ["v7", "serde"] }
insta = "1.49.0"
tempfile = "3.27.0"
```

- [ ] **Step 3: `opl-types` 크레이트 뼈대 작성**

`crates/opl-types/Cargo.toml`:

```toml
[package]
name = "opl-types"
description = "opLite resource model"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
serde.workspace = true
thiserror.workspace = true
uuid.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

`crates/opl-types/src/lib.rs`:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.
```

- [ ] **Step 4: `.gitignore` 교체와 LICENSE 추가**

`.gitignore`:

```
/target
.DS_Store
*.pending-snap
*.snap.new
```

```bash
curl -fsSL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE
head -3 LICENSE
```

Expected: `Apache License` / `Version 2.0, January 2004`가 보인다.

- [ ] **Step 5: 빌드 확인**

Run: `cargo build --workspace`
Expected: `Finished` (경고 없음)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock LICENSE .gitignore crates/opl-types
git commit -m "chore: Cargo 워크스페이스와 Apache-2.0 라이선스 추가"
```

---

### Task 2: `Resource<S, T>` 봉투

**Files:**
- Create: `crates/opl-types/src/resource.rs`
- Modify: `crates/opl-types/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-types/src/lib.rs`에 모듈을 선언한다:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.

pub mod resource;
```

`crates/opl-types/src/resource.rs`를 만들고 테스트만 먼저 넣는다:

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
    fn empty_labels_are_omitted_and_round_trip() {
        let r: Resource<u32, DummyStatus> = Resource::new("a", 1);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("labels").is_none());
        let back: Resource<u32, DummyStatus> = serde_json::from_value(json).unwrap();
        assert_eq!(back, r);
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-types`
Expected: 컴파일 실패 — `cannot find type 'Resource' in this scope` 등

- [ ] **Step 3: 구현 작성**

`crates/opl-types/src/resource.rs` 맨 위(테스트 모듈 앞)에 추가한다:

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 모든 리소스가 공유하는 봉투. spec은 API가, status는 reconciler가 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-types`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-types
git commit -m "feat(types): Resource 봉투 타입 추가"
```

---

### Task 3: `VmSpec`, `VmStatus`, `MacAddr`와 검증

**Files:**
- Create: `crates/opl-types/src/vm.rs`
- Modify: `crates/opl-types/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-types/src/lib.rs`:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.

pub mod resource;
pub mod vm;
```

`crates/opl-types/src/vm.rs`를 만들고 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> serde_json::Value {
        serde_json::json!({
            "cpu": { "cores": 4 },
            "memory_mib": 8192,
            "machine": "q35",
            "disks": [ { "volume": "web-01-root", "bus": "virtio-blk", "boot_order": 1 } ],
            "nics": [ { "network": "lan", "model": "virtio" } ],
            "power": "running",
            "restart": "on-failure"
        })
    }

    fn sample() -> VmSpec {
        serde_json::from_value(sample_json()).unwrap()
    }

    #[test]
    fn deserializes_with_defaults() {
        let spec = sample();
        assert_eq!(spec.cpu.sockets, 1);
        assert_eq!(spec.cpu.model, CpuModel::Host);
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert_eq!(spec.disks[0].bus, DiskBus::VirtioBlk);
        assert_eq!(spec.nics[0].mac, None);
        assert_eq!(spec.power, PowerState::Running);
        assert_eq!(spec.restart, RestartPolicy::OnFailure);
        assert_eq!(spec.node, None);
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut json = sample_json();
        json["memroy_mib"] = 1.into();
        assert!(serde_json::from_value::<VmSpec>(json).is_err());
    }

    #[test]
    fn round_trips_through_json() {
        let spec = sample();
        let back: VmSpec = serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn validate_accepts_sample() {
        assert_eq!(sample().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_bad_values() {
        let mut s = sample();
        s.cpu.cores = 0;
        assert_eq!(s.validate(), Err(ValidationError::ZeroCores));

        let mut s = sample();
        s.cpu.sockets = 0;
        assert_eq!(s.validate(), Err(ValidationError::ZeroSockets));

        let mut s = sample();
        s.memory_mib = MIN_MEMORY_MIB - 1;
        assert_eq!(s.validate(), Err(ValidationError::TooLittleMemory));

        let mut s = sample();
        let mut second = s.disks[0].clone();
        second.volume = "data".into();
        s.disks.push(second);
        assert_eq!(s.validate(), Err(ValidationError::DuplicateBootOrder(1)));
    }

    #[test]
    fn mac_parses_and_formats() {
        let mac: MacAddr = "52:54:00:AB:cd:0f".parse().unwrap();
        assert_eq!(mac.0, [0x52, 0x54, 0x00, 0xab, 0xcd, 0x0f]);
        assert_eq!(mac.to_string(), "52:54:00:ab:cd:0f");
        assert_eq!(serde_json::to_value(mac).unwrap(), "52:54:00:ab:cd:0f");
    }

    #[test]
    fn mac_rejects_malformed() {
        for bad in [
            "",
            "52:54:00:ab:cd",
            "52:54:00:ab:cd:0f:11",
            "5:54:00:ab:cd:0f",
            "zz:54:00:ab:cd:0f",
        ] {
            assert!(bad.parse::<MacAddr>().is_err(), "{bad} should be rejected");
        }
        assert!(serde_json::from_value::<MacAddr>("nope".into()).is_err());
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-types`
Expected: 컴파일 실패 — `cannot find type 'VmSpec'` 등

- [ ] **Step 3: 구현 작성**

`crates/opl-types/src/vm.rs` 맨 위에 추가한다. 핵심 결정:
- `deny_unknown_fields`: 오타 필드(`memroy_mib`)를 조용히 무시하지 않고 거부한다.
- `mac`은 `Option<MacAddr>`이다. 생략하면 VM 생성 시 자동 할당한다(계획 2).
- `node`는 Node 리소스의 UUID다. 이름 → UUID 변환은 API 계층이 맡는다.

```rust
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::Resource;

pub type Vm = Resource<VmSpec, VmStatus>;

pub const MIN_MEMORY_MIB: u32 = 64;

/// VM의 원하는 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSpec {
    pub cpu: CpuSpec,
    pub memory_mib: u32,
    #[serde(default)]
    pub firmware: Firmware,
    pub machine: MachineType,
    #[serde(default)]
    pub disks: Vec<DiskSpec>,
    #[serde(default)]
    pub nics: Vec<NicSpec>,
    #[serde(default)]
    pub power: PowerState,
    #[serde(default)]
    pub restart: RestartPolicy,
    /// 배치된 노드의 id. None이면 시작할 때 스케줄러가 고른다.
    #[serde(default)]
    pub node: Option<Uuid>,
}

impl VmSpec {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.cpu.sockets == 0 {
            return Err(ValidationError::ZeroSockets);
        }
        if self.cpu.cores == 0 {
            return Err(ValidationError::ZeroCores);
        }
        if self.memory_mib < MIN_MEMORY_MIB {
            return Err(ValidationError::TooLittleMemory);
        }
        let mut seen = BTreeSet::new();
        for order in self.disks.iter().filter_map(|d| d.boot_order) {
            if !seen.insert(order) {
                return Err(ValidationError::DuplicateBootOrder(order));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("cpu.sockets must be at least 1")]
    ZeroSockets,
    #[error("cpu.cores must be at least 1")]
    ZeroCores,
    #[error("memory_mib must be at least {MIN_MEMORY_MIB}")]
    TooLittleMemory,
    #[error("boot_order {0} is used by more than one disk")]
    DuplicateBootOrder(u8),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuSpec {
    #[serde(default = "one")]
    pub sockets: u16,
    pub cores: u16,
    #[serde(default)]
    pub model: CpuModel,
}

fn one() -> u16 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CpuModel {
    /// 호스트 CPU를 그대로 노출한다. TCG에서는 max로 대체된다.
    #[default]
    Host,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    Bios,
    #[default]
    Uefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineType {
    /// x86_64 전용
    Q35,
    /// aarch64 전용
    Virt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskSpec {
    /// Volume 리소스 이름
    pub volume: String,
    pub bus: DiskBus,
    #[serde(default)]
    pub boot_order: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiskBus {
    VirtioBlk,
    /// 드라이버 없이 설치하는 Windows 게스트용. q35에서만 쓸 수 있다.
    Sata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NicSpec {
    /// Network 리소스 이름
    pub network: String,
    pub model: NicModel,
    /// None이면 생성 시 자동 할당한다.
    #[serde(default)]
    pub mac: Option<MacAddr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NicModel {
    Virtio,
    E1000e,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PowerState {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
}

/// VM의 실제 상태. reconciler만 기록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VmStatus {
    pub phase: VmPhase,
    pub node: Option<Uuid>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum VmPhase {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

/// `52:54:00:12:34:56` 형식의 MAC 주소.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MacAddr(pub [u8; 6]);

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid MAC address: {0}")]
pub struct MacParseError(String);

impl FromStr for MacAddr {
    type Err = MacParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || MacParseError(s.to_string());
        let mut bytes = [0u8; 6];
        let mut parts = s.split(':');
        for b in &mut bytes {
            let part = parts.next().ok_or_else(err)?;
            if part.len() != 2 {
                return Err(err());
            }
            *b = u8::from_str_radix(part, 16).map_err(|_| err())?;
        }
        if parts.next().is_some() {
            return Err(err());
        }
        Ok(MacAddr(bytes))
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl TryFrom<String> for MacAddr {
    type Error = MacParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<MacAddr> for String {
    fn from(m: MacAddr) -> String {
        m.to_string()
    }
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-types`
Expected: `test result: ok. 9 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-types
git commit -m "feat(types): VmSpec/VmStatus, MAC 주소, spec 검증 추가"
```

---

### Task 4: `VmRuntimeSpec` (드라이버 입력 타입)

`VmSpec`은 Volume/Network를 **이름**으로 참조한다. 드라이버는 이름이 모두 풀린 형태(파일 경로, tap 이름, 실제 MAC)를 받아야 하므로 별도 타입을 둔다. Mock 드라이버도 이 타입을 쓰므로 `opl-qemu`가 아니라 `opl-types`에 둔다. 동작 로직이 없는 타입 정의이므로 이 Task에는 테스트가 없고, Task 7의 커맨드라인 테스트가 이 타입을 검증한다.

**Files:**
- Create: `crates/opl-types/src/runtime.rs`
- Modify: `crates/opl-types/src/lib.rs`

- [ ] **Step 1: 타입 작성**

`crates/opl-types/src/lib.rs`:

```rust
//! opLite 리소스 모델. 모든 크레이트가 공유하는 타입만 둔다.

pub mod resource;
pub mod runtime;
pub mod vm;
```

`crates/opl-types/src/runtime.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::vm::{CpuSpec, DiskBus, Firmware, MacAddr, MachineType, NicModel};

/// 하이퍼바이저 드라이버가 받는, 이름 참조가 모두 풀린 VM 정의.
/// Volume 이름은 파일 경로로, Network 이름은 백엔드로, MAC은 실제 값으로 바뀌어 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRuntimeSpec {
    pub id: Uuid,
    pub name: String,
    pub cpu: CpuSpec,
    pub memory_mib: u32,
    pub firmware: Firmware,
    pub machine: MachineType,
    pub disks: Vec<RuntimeDisk>,
    pub nics: Vec<RuntimeNic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDisk {
    pub path: PathBuf,
    pub format: ImageFormat,
    pub bus: DiskBus,
    pub boot_order: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageFormat {
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeNic {
    pub mac: MacAddr,
    pub model: NicModel,
    pub backend: NicBackend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum NicBackend {
    /// QEMU user-mode NAT
    User,
    /// 미리 만들어 둔 tap 장치
    Tap { ifname: String },
}
```

- [ ] **Step 2: 빌드와 lint 확인**

Run: `cargo clippy -p opl-types --all-targets -- -D warnings && cargo test -p opl-types`
Expected: 경고 없음, `test result: ok. 9 passed`

- [ ] **Step 3: Commit**

```bash
git add crates/opl-types
git commit -m "feat(types): 드라이버 입력용 VmRuntimeSpec 추가"
```

---

### Task 5: CI와 라이선스 검사

**Files:**
- Create: `.github/workflows/ci.yml`, `deny.toml`

- [ ] **Step 1: 워크플로 작성**

`.github/workflows/ci.yml`:

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

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
```

참고: GitHub의 macOS 러너는 중첩 가상화(hvf)를 지원하지 않으므로 `qemu-it` 통합 테스트는 CI에서 컴파일만 하고(clippy `--all-features`) 실행하지 않는다.

- [ ] **Step 2: 라이선스 정책 작성**

`deny.toml`:

```toml
# 공개 코어는 Apache-2.0이다. 호환되지 않는 라이선스의 의존성이 들어오면 CI가 실패한다.
[licenses]
allow = [
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MIT",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
]
confidence-threshold = 0.9

[bans]
multiple-versions = "warn"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

- [ ] **Step 3: 로컬에서 같은 검사 실행**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo install cargo-deny --locked   # 처음 한 번만
cargo deny check
```

Expected: 네 명령 모두 오류 없이 끝난다. `cargo deny check`가 허용 목록 밖의 라이선스를 보고하면, 해당 크레이트의 라이선스가 Apache-2.0과 호환되는지 확인한 뒤에만 `allow`에 추가한다.

- [ ] **Step 4: Commit**

```bash
git add .github deny.toml
git commit -m "ci: fmt/clippy/test(Linux, macOS)와 cargo-deny 라이선스 검사 추가"
```

---

### Task 6: `opl-qemu` 뼈대, `HostProfile`, `RuntimePaths`

**Files:**
- Create: `crates/opl-qemu/Cargo.toml`, `crates/opl-qemu/src/lib.rs`, `crates/opl-qemu/src/host.rs`, `crates/opl-qemu/src/paths.rs`

- [ ] **Step 1: 크레이트 매니페스트 작성**

`crates/opl-qemu/Cargo.toml`:

```toml
[package]
name = "opl-qemu"
description = "QEMU command line builder and QMP client for opLite"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# 실제 QEMU를 실행하는 통합 테스트. macOS(hvf)에서는 `brew install qemu`가 필요하다.
qemu-it = []

[dependencies]
opl-types.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
uuid.workspace = true

[dev-dependencies]
insta.workspace = true
tempfile.workspace = true
```

`crates/opl-qemu/src/lib.rs`:

```rust
//! QEMU 커맨드라인 생성과 QMP 클라이언트.

pub mod host;
pub mod paths;

pub use host::{Accel, Arch, HostProfile};
pub use paths::{PathError, RuntimePaths};
```

- [ ] **Step 2: `HostProfile` 작성**

동작 로직이 거의 없는 설정 타입이다. Task 7 테스트가 사용한다.

`crates/opl-qemu/src/host.rs`:

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    Kvm,
    Hvf,
    Tcg,
}

impl Accel {
    pub fn as_str(self) -> &'static str {
        match self {
            Accel::Kvm => "kvm",
            Accel::Hvf => "hvf",
            Accel::Tcg => "tcg",
        }
    }
}

/// 이 호스트에서 QEMU를 어떻게 실행할지에 대한 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProfile {
    pub arch: Arch,
    pub accel: Accel,
    pub qemu_binary: PathBuf,
    pub uefi_code: PathBuf,
    /// VM마다 복사해서 쓰는 UEFI 변수 저장소 원본
    pub uefi_vars_template: PathBuf,
}

impl HostProfile {
    /// Homebrew QEMU가 설치된 Apple Silicon Mac. `share_dir`는 보통 `/opt/homebrew/share/qemu`.
    pub fn homebrew_hvf(share_dir: &Path) -> Self {
        Self {
            arch: Arch::Aarch64,
            accel: Accel::Hvf,
            qemu_binary: PathBuf::from("qemu-system-aarch64"),
            uefi_code: share_dir.join("edk2-aarch64-code.fd"),
            uefi_vars_template: share_dir.join("edk2-arm-vars.fd"),
        }
    }
}
```

- [ ] **Step 3: `RuntimePaths` 실패하는 테스트 작성**

유닉스 소켓 경로는 macOS에서 103바이트를 넘으면 `bind`가 실패한다. 실패 원인을 알기 어려운 오류이므로 경로를 만들 때 미리 검사한다.

`crates/opl-qemu/src/paths.rs`에 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_files_under_vm_dir() {
        let id = Uuid::nil();
        let p = RuntimePaths::new(Path::new("/run/opl"), id).unwrap();
        let dir = "/run/opl/vm/00000000-0000-0000-0000-000000000000";
        assert_eq!(p.dir, Path::new(dir));
        assert_eq!(p.qmp_socket, Path::new(dir).join("qmp.sock"));
        assert_eq!(p.vnc_socket, Path::new(dir).join("vnc.sock"));
        assert_eq!(p.pidfile, Path::new(dir).join("qemu.pid"));
        assert_eq!(p.uefi_vars, Path::new(dir).join("uefi-vars.fd"));
    }

    #[test]
    fn rejects_socket_paths_over_the_limit() {
        let long = Path::new("/").join("x".repeat(60));
        let err = RuntimePaths::new(&long, Uuid::nil()).unwrap_err();
        assert!(err.len > MAX_SOCKET_PATH);
    }
}
```

- [ ] **Step 4: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-qemu`
Expected: 컴파일 실패 — `cannot find type 'RuntimePaths'`

- [ ] **Step 5: 구현 작성**

`crates/opl-qemu/src/paths.rs` 맨 위에 추가한다:

```rust
use std::path::{Path, PathBuf};

use thiserror::Error;
use uuid::Uuid;

/// macOS의 sun_path 한도(104바이트)에서 NUL 종단 1바이트를 뺀 값. Linux(108)보다 작아 이쪽에 맞춘다.
pub const MAX_SOCKET_PATH: usize = 103;

/// VM 하나의 런타임 파일 위치: `{run_dir}/vm/{uuid}/...`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub dir: PathBuf,
    pub qmp_socket: PathBuf,
    pub vnc_socket: PathBuf,
    pub pidfile: PathBuf,
    pub uefi_vars: PathBuf,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("socket path {path:?} is {len} bytes; the limit is {MAX_SOCKET_PATH}")]
pub struct PathError {
    pub path: PathBuf,
    pub len: usize,
}

impl RuntimePaths {
    pub fn new(run_dir: &Path, id: Uuid) -> Result<Self, PathError> {
        let dir = run_dir.join("vm").join(id.to_string());
        let paths = Self {
            qmp_socket: dir.join("qmp.sock"),
            vnc_socket: dir.join("vnc.sock"),
            pidfile: dir.join("qemu.pid"),
            uefi_vars: dir.join("uefi-vars.fd"),
            dir,
        };
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            let len = sock.as_os_str().len();
            if len > MAX_SOCKET_PATH {
                return Err(PathError {
                    path: sock.clone(),
                    len,
                });
            }
        }
        Ok(paths)
    }
}
```

- [ ] **Step 6: 테스트 통과 확인**

Run: `cargo test -p opl-qemu`
Expected: `test result: ok. 2 passed`

- [ ] **Step 7: Commit**

```bash
git add crates/opl-qemu Cargo.lock
git commit -m "feat(qemu): HostProfile과 소켓 경로 길이를 검사하는 RuntimePaths 추가"
```

---

### Task 7: QEMU 커맨드라인 생성기

`build_cmdline`은 부수 효과가 없는 순수 함수다. 생성 결과 전체를 insta 스냅샷으로 고정하고, 호환성 오류와 이스케이프는 개별 테스트로 검증한다.

**Files:**
- Create: `crates/opl-qemu/src/cmdline.rs`, `crates/opl-qemu/src/snapshots/opl_qemu__cmdline__tests__x86_kvm_uefi_full.snap`, `crates/opl-qemu/src/snapshots/opl_qemu__cmdline__tests__aarch64_hvf_minimal.snap`
- Modify: `crates/opl-qemu/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트와 기대 스냅샷 작성**

`crates/opl-qemu/src/lib.rs`:

```rust
//! QEMU 커맨드라인 생성과 QMP 클라이언트.

pub mod cmdline;
pub mod host;
pub mod paths;

pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, Arch, HostProfile};
pub use paths::{PathError, RuntimePaths};
```

`crates/opl-qemu/src/cmdline.rs`에 테스트만 먼저 넣는다:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use opl_types::runtime::{RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
    use uuid::Uuid;

    use super::*;

    fn x86_host() -> HostProfile {
        HostProfile {
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
            uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
        }
    }

    fn mac_host() -> HostProfile {
        HostProfile::homebrew_hvf(Path::new("/opt/homebrew/share/qemu"))
    }

    fn paths() -> RuntimePaths {
        RuntimePaths::new(Path::new("/run/opl"), Uuid::nil()).unwrap()
    }

    fn x86_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::nil(),
            name: "web-01".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 4,
                model: CpuModel::Host,
            },
            memory_mib: 8192,
            firmware: Firmware::Uefi,
            machine: MachineType::Q35,
            disks: vec![
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-root.qcow2".into(),
                    format: ImageFormat::Qcow2,
                    bus: DiskBus::VirtioBlk,
                    boot_order: Some(1),
                },
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-data.raw".into(),
                    format: ImageFormat::Raw,
                    bus: DiskBus::Sata,
                    boot_order: None,
                },
            ],
            nics: vec![
                RuntimeNic {
                    mac: "52:54:00:12:34:56".parse().unwrap(),
                    model: NicModel::Virtio,
                    backend: NicBackend::Tap {
                        ifname: "tap0i0".into(),
                    },
                },
                RuntimeNic {
                    mac: "52:54:00:12:34:57".parse().unwrap(),
                    model: NicModel::E1000e,
                    backend: NicBackend::User,
                },
            ],
        }
    }

    fn mac_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            machine: MachineType::Virt,
            disks: vec![RuntimeDisk {
                path: "/tmp/opl/root.qcow2".into(),
                format: ImageFormat::Qcow2,
                bus: DiskBus::VirtioBlk,
                boot_order: Some(1),
            }],
            nics: vec![RuntimeNic {
                mac: "52:54:00:12:34:56".parse().unwrap(),
                model: NicModel::Virtio,
                backend: NicBackend::User,
            }],
            ..x86_spec()
        }
    }

    #[test]
    fn x86_kvm_uefi_full() {
        let args = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn aarch64_hvf_minimal() {
        let args = build_cmdline(&mac_spec(), &mac_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn bios_omits_pflash() {
        let spec = VmRuntimeSpec {
            firmware: Firmware::Bios,
            ..x86_spec()
        };
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(!args.iter().any(|a| a.contains("pflash")));
    }

    #[test]
    fn host_cpu_falls_back_to_max_under_tcg() {
        let host = HostProfile {
            accel: Accel::Tcg,
            ..x86_host()
        };
        let args = build_cmdline(&x86_spec(), &host, &paths()).unwrap();
        let i = args.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(args[i + 1], "max");
        assert!(args.contains(&"q35,accel=tcg".to_string()));
    }

    #[test]
    fn escapes_commas_in_names_and_paths() {
        let mut spec = x86_spec();
        spec.name = "a,b".into();
        spec.disks[0].path = "/pool/x,y.qcow2".into();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"guest=a,,b,debug-threads=on".to_string()));
        assert!(
            args.contains(
                &"driver=file,filename=/pool/x,,y.qcow2,node-name=disk0-file".to_string()
            )
        );
    }

    #[test]
    fn rejects_incompatible_combinations() {
        let err = |spec: &VmRuntimeSpec, host: &HostProfile| {
            build_cmdline(spec, host, &paths()).unwrap_err()
        };
        assert_eq!(
            err(&x86_spec(), &mac_host()),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
        assert_eq!(
            err(
                &VmRuntimeSpec {
                    firmware: Firmware::Bios,
                    ..mac_spec()
                },
                &mac_host()
            ),
            CmdlineError::BiosOnAarch64
        );
        let mut sata_on_virt = mac_spec();
        sata_on_virt.disks[0].bus = DiskBus::Sata;
        assert_eq!(
            err(&sata_on_virt, &mac_host()),
            CmdlineError::SataRequiresQ35
        );
        let mut many = x86_spec();
        many.disks = (0..7)
            .map(|i| RuntimeDisk {
                path: format!("/d{i}.raw").into(),
                format: ImageFormat::Raw,
                bus: DiskBus::Sata,
                boot_order: None,
            })
            .collect();
        assert_eq!(err(&many, &x86_host()), CmdlineError::TooManySataDisks);
    }
}
```

기대 스냅샷 `crates/opl-qemu/src/snapshots/opl_qemu__cmdline__tests__x86_kvm_uefi_full.snap`:

```
---
source: crates/opl-qemu/src/cmdline.rs
expression: "args.join(\"\\n\")"
---
-name
guest=web-01,debug-threads=on
-uuid
00000000-0000-0000-0000-000000000000
-no-user-config
-nodefaults
-S
-machine
q35,accel=kvm
-cpu
host
-smp
sockets=1,cores=4,threads=1
-m
8192M
-drive
if=pflash,unit=0,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE.fd
-drive
if=pflash,unit=1,format=raw,file=/run/opl/vm/00000000-0000-0000-0000-000000000000/uefi-vars.fd
-device
VGA,id=video0
-device
qemu-xhci,id=usb
-device
usb-tablet,bus=usb.0
-device
usb-kbd,bus=usb.0
-blockdev
driver=file,filename=/var/lib/opl/pools/local/web-01-root.qcow2,node-name=disk0-file
-blockdev
driver=qcow2,file=disk0-file,node-name=disk0
-device
virtio-blk-pci,drive=disk0,id=disk0-dev,bootindex=1
-blockdev
driver=file,filename=/var/lib/opl/pools/local/web-01-data.raw,node-name=disk1-file
-blockdev
driver=raw,file=disk1-file,node-name=disk1
-device
ide-hd,drive=disk1,bus=ide.0,id=disk1-dev
-netdev
tap,id=net0,ifname=tap0i0,script=no,downscript=no
-device
virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56,id=nic0
-netdev
user,id=net1
-device
e1000e,netdev=net1,mac=52:54:00:12:34:57,id=nic1
-vnc
unix:/run/opl/vm/00000000-0000-0000-0000-000000000000/vnc.sock
-qmp
unix:/run/opl/vm/00000000-0000-0000-0000-000000000000/qmp.sock,server=on,wait=off
-pidfile
/run/opl/vm/00000000-0000-0000-0000-000000000000/qemu.pid
```

기대 스냅샷 `crates/opl-qemu/src/snapshots/opl_qemu__cmdline__tests__aarch64_hvf_minimal.snap`:

```
---
source: crates/opl-qemu/src/cmdline.rs
expression: "args.join(\"\\n\")"
---
-name
guest=web-01,debug-threads=on
-uuid
00000000-0000-0000-0000-000000000000
-no-user-config
-nodefaults
-S
-machine
virt,accel=hvf
-cpu
host
-smp
sockets=1,cores=4,threads=1
-m
8192M
-drive
if=pflash,unit=0,format=raw,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd
-drive
if=pflash,unit=1,format=raw,file=/run/opl/vm/00000000-0000-0000-0000-000000000000/uefi-vars.fd
-device
virtio-gpu-pci,id=video0
-device
qemu-xhci,id=usb
-device
usb-tablet,bus=usb.0
-device
usb-kbd,bus=usb.0
-blockdev
driver=file,filename=/tmp/opl/root.qcow2,node-name=disk0-file
-blockdev
driver=qcow2,file=disk0-file,node-name=disk0
-device
virtio-blk-pci,drive=disk0,id=disk0-dev,bootindex=1
-netdev
user,id=net0
-device
virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56,id=nic0
-vnc
unix:/run/opl/vm/00000000-0000-0000-0000-000000000000/vnc.sock
-qmp
unix:/run/opl/vm/00000000-0000-0000-0000-000000000000/qmp.sock,server=on,wait=off
-pidfile
/run/opl/vm/00000000-0000-0000-0000-000000000000/qemu.pid
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-qemu`
Expected: 컴파일 실패 — `cannot find function 'build_cmdline'`

- [ ] **Step 3: 구현 작성**

`crates/opl-qemu/src/cmdline.rs` 맨 위에 추가한다. 핵심 결정:
- 항상 `-S`(일시정지)로 시작한다. 호출자가 QMP 연결을 확보한 뒤 `cont`로 재개하므로 부팅 초기 이벤트를 놓치지 않는다.
- `-nodefaults -no-user-config`: QEMU 기본 장치와 설정 파일을 끄고, 필요한 장치를 모두 명시한다. 버전이 바뀌어도 게스트가 보는 하드웨어가 달라지지 않게 하기 위해서다.
- 디스크는 `-blockdev`(파일 노드 + 포맷 노드)로 정의한다. 나중에 라이브 마이그레이션과 스냅샷에서 노드 이름으로 조작하기 위해서다.
- 옵션 값 안의 쉼표는 `,,`로 이스케이프한다.

```rust
use std::path::Path;

use opl_types::runtime::{ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, Arch, HostProfile};
use crate::paths::RuntimePaths;

/// q35의 내장 AHCI 컨트롤러 포트 수
const Q35_SATA_PORTS: usize = 6;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CmdlineError {
    #[error("machine type {machine:?} is not supported on {arch:?} hosts")]
    MachineArchMismatch { machine: MachineType, arch: Arch },
    #[error("BIOS firmware is not supported on aarch64; use UEFI")]
    BiosOnAarch64,
    #[error("SATA disks require the q35 machine type")]
    SataRequiresQ35,
    #[error("at most {Q35_SATA_PORTS} SATA disks are supported")]
    TooManySataDisks,
}

/// QEMU 인자 목록(바이너리 제외)을 만든다. 부수 효과가 없는 순수 함수다.
///
/// VM은 항상 `-S`(일시정지) 상태로 시작한다. 호출자는 QMP 연결 뒤 `cont`로 실행을 재개한다.
pub fn build_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    check_compat(spec, host)?;

    let mut a = Args::default();
    a.opt(
        "-name",
        format!("guest={},debug-threads=on", esc(&spec.name)),
    );
    a.opt("-uuid", spec.id.to_string());
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    a.flag("-S");
    let machine = match spec.machine {
        MachineType::Q35 => "q35",
        MachineType::Virt => "virt",
    };
    a.opt(
        "-machine",
        format!("{machine},accel={}", host.accel.as_str()),
    );
    let cpu = match (spec.cpu.model, host.accel) {
        (CpuModel::Host, Accel::Tcg) | (CpuModel::Max, _) => "max",
        (CpuModel::Host, _) => "host",
    };
    a.opt("-cpu", cpu.to_string());
    a.opt(
        "-smp",
        format!(
            "sockets={},cores={},threads=1",
            spec.cpu.sockets, spec.cpu.cores
        ),
    );
    a.opt("-m", format!("{}M", spec.memory_mib));

    if spec.firmware == Firmware::Uefi {
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=0,format=raw,readonly=on,file={}",
                esc_path(&host.uefi_code)
            ),
        );
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=1,format=raw,file={}",
                esc_path(&paths.uefi_vars)
            ),
        );
    }

    let video = match host.arch {
        Arch::X86_64 => "VGA,id=video0",
        Arch::Aarch64 => "virtio-gpu-pci,id=video0",
    };
    a.opt("-device", video.to_string());
    a.opt("-device", "qemu-xhci,id=usb".to_string());
    a.opt("-device", "usb-tablet,bus=usb.0".to_string());
    a.opt("-device", "usb-kbd,bus=usb.0".to_string());

    let mut sata_port = 0;
    for (i, disk) in spec.disks.iter().enumerate() {
        let fmt = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        a.opt(
            "-blockdev",
            format!(
                "driver=file,filename={},node-name=disk{i}-file",
                esc_path(&disk.path)
            ),
        );
        a.opt(
            "-blockdev",
            format!("driver={fmt},file=disk{i}-file,node-name=disk{i}"),
        );
        let mut dev = match disk.bus {
            DiskBus::VirtioBlk => format!("virtio-blk-pci,drive=disk{i},id=disk{i}-dev"),
            DiskBus::Sata => {
                let d = format!("ide-hd,drive=disk{i},bus=ide.{sata_port},id=disk{i}-dev");
                sata_port += 1;
                d
            }
        };
        if let Some(order) = disk.boot_order {
            dev.push_str(&format!(",bootindex={order}"));
        }
        a.opt("-device", dev);
    }

    for (i, nic) in spec.nics.iter().enumerate() {
        let netdev = match &nic.backend {
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
                esc(ifname)
            ),
        };
        a.opt("-netdev", netdev);
        let model = match nic.model {
            NicModel::Virtio => "virtio-net-pci",
            NicModel::E1000e => "e1000e",
        };
        a.opt(
            "-device",
            format!("{model},netdev=net{i},mac={},id=nic{i}", nic.mac),
        );
    }

    a.opt("-vnc", format!("unix:{}", esc_path(&paths.vnc_socket)));
    a.opt(
        "-qmp",
        format!("unix:{},server=on,wait=off", esc_path(&paths.qmp_socket)),
    );
    a.opt("-pidfile", paths.pidfile.display().to_string());
    Ok(a.0)
}

fn check_compat(spec: &VmRuntimeSpec, host: &HostProfile) -> Result<(), CmdlineError> {
    match (spec.machine, host.arch) {
        (MachineType::Q35, Arch::X86_64) | (MachineType::Virt, Arch::Aarch64) => {}
        (machine, arch) => return Err(CmdlineError::MachineArchMismatch { machine, arch }),
    }
    if host.arch == Arch::Aarch64 && spec.firmware == Firmware::Bios {
        return Err(CmdlineError::BiosOnAarch64);
    }
    let sata = spec.disks.iter().filter(|d| d.bus == DiskBus::Sata).count();
    if sata > 0 && spec.machine != MachineType::Q35 {
        return Err(CmdlineError::SataRequiresQ35);
    }
    if sata > Q35_SATA_PORTS {
        return Err(CmdlineError::TooManySataDisks);
    }
    Ok(())
}

#[derive(Default)]
struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, flag: &str) {
        self.0.push(flag.to_string());
    }

    fn opt(&mut self, flag: &str, value: String) {
        self.0.push(flag.to_string());
        self.0.push(value);
    }
}

/// QEMU 옵션 값 안의 쉼표는 두 번 써서 이스케이프한다.
fn esc(s: &str) -> String {
    s.replace(',', ",,")
}

fn esc_path(p: &Path) -> String {
    esc(&p.display().to_string())
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-qemu`
Expected: `test result: ok. 8 passed`. 스냅샷 불일치가 나면 `.snap.new` 파일과 Step 1의 기대 스냅샷을 비교해 구현을 고친다. 스냅샷을 의도적으로 바꾸는 경우에만 `cargo install cargo-insta && cargo insta review`로 승인한다.

- [ ] **Step 5: Commit**

```bash
git add crates/opl-qemu
git commit -m "feat(qemu): VmRuntimeSpec을 QEMU 인자로 바꾸는 커맨드라인 생성기 추가"
```

---

### Task 8: QMP 클라이언트

QMP는 줄 단위 JSON 프로토콜이다. 연결하면 QEMU가 greeting을 보내고, 클라이언트가 `qmp_capabilities`를 보내야 명령 모드에 들어간다. 명령 응답과 비동기 이벤트가 같은 스트림에 섞여 오므로, 리더 태스크 하나가 `id`로 응답을 대기 중인 호출자에게 돌려주고 이벤트는 broadcast 채널로 내보낸다.

**Files:**
- Create: `crates/opl-qemu/src/qmp.rs`
- Modify: `crates/opl-qemu/src/lib.rs`

- [ ] **Step 1: 실패하는 테스트 작성**

`crates/opl-qemu/src/lib.rs`:

```rust
//! QEMU 커맨드라인 생성과 QMP 클라이언트.

pub mod cmdline;
pub mod host;
pub mod paths;
pub mod qmp;

pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, Arch, HostProfile};
pub use paths::{PathError, RuntimePaths};
pub use qmp::{QmpClient, QmpError, QmpEvent, RunState};
```

`crates/opl-qemu/src/qmp.rs`에 테스트만 먼저 넣는다. 테스트는 임시 디렉터리에 유닉스 소켓을 열어 QEMU 쪽을 흉내 내는 `FakeQemu`를 쓴다:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::net::UnixListener;

    use super::*;

    const GREETING: &str = r#"{"QMP":{"version":{"qemu":{"major":10,"minor":1,"micro":0},"package":""},"capabilities":[]}}"#;

    /// 테스트용 가짜 QEMU 쪽 연결
    struct FakeQemu {
        lines: Lines<BufReader<OwnedReadHalf>>,
        write: OwnedWriteHalf,
    }

    impl FakeQemu {
        async fn send(&mut self, v: Value) {
            let mut line = v.to_string().into_bytes();
            line.push(b'\n');
            self.write.write_all(&line).await.unwrap();
        }

        async fn recv(&mut self) -> Value {
            let line = self.lines.next_line().await.unwrap().unwrap();
            serde_json::from_str(&line).unwrap()
        }

        /// 명령 하나를 받아 이름을 확인하고 `reply`를 `id`와 함께 돌려준다.
        async fn answer(&mut self, expect: &str, reply: Value) {
            let cmd = self.recv().await;
            assert_eq!(cmd["execute"], expect);
            let mut reply = reply;
            reply["id"] = cmd["id"].clone();
            self.send(reply).await;
        }
    }

    /// 리스너를 열고, 클라이언트 연결을 받아 greeting과 capabilities 협상까지 처리한다.
    async fn setup() -> (QmpClient, FakeQemu, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("qmp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, write) = stream.into_split();
            let mut fake = FakeQemu {
                lines: BufReader::new(read).lines(),
                write,
            };
            fake.write
                .write_all(format!("{GREETING}\n").as_bytes())
                .await
                .unwrap();
            fake.answer("qmp_capabilities", json!({ "return": {} }))
                .await;
            fake
        });
        let client = QmpClient::connect(&path).await.unwrap();
        (client, server.await.unwrap(), dir)
    }

    #[tokio::test]
    async fn negotiates_and_queries_status() {
        let (client, mut fake, _dir) = setup().await;
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "prelaunch" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::Prelaunch);
    }

    #[tokio::test]
    async fn command_errors_carry_class_and_desc() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.cont(), async {
            fake.answer(
                "cont",
                json!({ "error": { "class": "GenericError", "desc": "boom" } }),
            )
            .await;
        });
        match res.unwrap_err() {
            QmpError::Command { class, desc } => {
                assert_eq!(class, "GenericError");
                assert_eq!(desc, "boom");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_are_broadcast_to_subscribers() {
        let (client, mut fake, _dir) = setup().await;
        let mut rx = client.subscribe();
        fake.send(json!({ "event": "SHUTDOWN", "data": { "guest": true }, "timestamp": {} }))
            .await;
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.event, "SHUTDOWN");
        assert_eq!(ev.data["guest"], true);
    }

    #[tokio::test]
    async fn pending_and_later_commands_fail_after_disconnect() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.query_status(), async {
            let _ = fake.recv().await;
            drop(fake);
        });
        assert!(matches!(res, Err(QmpError::Disconnected)));
        assert!(matches!(client.cont().await, Err(QmpError::Disconnected)));
    }

    #[tokio::test]
    async fn quit_succeeds_when_qemu_closes_without_reply() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.quit(), async {
            let _ = fake.recv().await;
            drop(fake);
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn rejects_non_qmp_greeting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("qmp.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            // 클라이언트가 결과를 읽을 때까지 연결을 유지한다.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        });
        assert!(matches!(
            QmpClient::connect(&path).await,
            Err(QmpError::Protocol(_))
        ));
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인**

Run: `cargo test -p opl-qemu qmp`
Expected: 컴파일 실패 — `cannot find type 'QmpClient'`

- [ ] **Step 3: 구현 작성**

`crates/opl-qemu/src/qmp.rs` 맨 위에 추가한다. 핵심 결정:
- `pending`은 `Mutex<Option<HashMap>>`이다. 리더가 끝나면 `None`으로 바꾸고, 대기 중인 호출을 모두 `Disconnected`로 끝낸다. 이후 호출도 즉시 `Disconnected`를 받으므로 영원히 기다리는 호출이 생기지 않는다.
- `quit`은 QEMU가 응답 전에 연결을 닫는 경우가 있으므로 `Disconnected`도 성공으로 본다.
- 타임아웃은 클라이언트가 아니라 호출자가 `tokio::time::timeout`으로 건다. 명령마다 적절한 시간이 다르기 때문이다(마이그레이션은 수 분 걸린다).

```rust
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("QMP i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid QMP message: {0}")]
    Protocol(String),
    #[error("QMP command failed: {class}: {desc}")]
    Command { class: String, desc: String },
    #[error("QMP connection closed")]
    Disconnected,
}

/// QEMU가 보낸 비동기 이벤트 (SHUTDOWN, STOP, RESUME 등)
#[derive(Debug, Clone, PartialEq)]
pub struct QmpEvent {
    pub event: String,
    pub data: Value,
}

/// `query-status`의 `status` 값
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    /// `-S`로 시작해 아직 `cont`를 받지 않은 상태
    Prelaunch,
    Running,
    Paused,
    Shutdown,
    Other(String),
}

impl RunState {
    fn parse(s: &str) -> Self {
        match s {
            "prelaunch" => RunState::Prelaunch,
            "running" => RunState::Running,
            "paused" => RunState::Paused,
            "shutdown" => RunState::Shutdown,
            other => RunState::Other(other.to_string()),
        }
    }
}

type Reply = Result<Value, QmpError>;
/// None이면 연결이 끊긴 상태다. 이후의 명령은 바로 Disconnected로 실패한다.
type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Reply>>>>>;

/// QMP 유닉스 소켓 클라이언트. 명령은 동시에 여러 개 보낼 수 있고, 응답은 `id`로 짝을 맞춘다.
pub struct QmpClient {
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
    pending: Pending,
    next_id: AtomicU64,
    events: broadcast::Sender<QmpEvent>,
    reader: JoinHandle<()>,
}

impl QmpClient {
    /// 소켓에 연결하고 greeting을 확인한 뒤 `qmp_capabilities`로 명령 모드에 진입한다.
    pub async fn connect(path: &Path) -> Result<Self, QmpError> {
        let stream = UnixStream::connect(path).await?;
        let (read, write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let greeting = lines.next_line().await?.ok_or(QmpError::Disconnected)?;
        let greeting: Value = serde_json::from_str(&greeting)
            .map_err(|e| QmpError::Protocol(format!("greeting is not JSON: {e}")))?;
        if greeting.get("QMP").is_none() {
            return Err(QmpError::Protocol(format!(
                "unexpected greeting: {greeting}"
            )));
        }

        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (events, _) = broadcast::channel(64);
        let reader = tokio::spawn(read_loop(lines, pending.clone(), events.clone()));
        let client = Self {
            writer: tokio::sync::Mutex::new(write),
            pending,
            next_id: AtomicU64::new(1),
            events,
            reader,
        };
        client.execute("qmp_capabilities", None).await?;
        Ok(client)
    }

    pub async fn execute(
        &self,
        command: &str,
        arguments: Option<Value>,
    ) -> Result<Value, QmpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        match self.pending.lock().unwrap().as_mut() {
            Some(map) => {
                map.insert(id, tx);
            }
            None => return Err(QmpError::Disconnected),
        }

        let mut msg = json!({ "execute": command, "id": id });
        if let Some(args) = arguments {
            msg["arguments"] = args;
        }
        let mut line = serde_json::to_vec(&msg).expect("json! value always serializes");
        line.push(b'\n');
        if let Err(e) = self.writer.lock().await.write_all(&line).await {
            if let Some(map) = self.pending.lock().unwrap().as_mut() {
                map.remove(&id);
            }
            return Err(e.into());
        }
        rx.await.unwrap_or(Err(QmpError::Disconnected))
    }

    /// 이 시점 이후에 도착하는 이벤트를 받는다.
    pub fn subscribe(&self) -> broadcast::Receiver<QmpEvent> {
        self.events.subscribe()
    }

    pub async fn query_status(&self) -> Result<RunState, QmpError> {
        let ret = self.execute("query-status", None).await?;
        let status = ret
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| QmpError::Protocol(format!("query-status without status: {ret}")))?;
        Ok(RunState::parse(status))
    }

    pub async fn cont(&self) -> Result<(), QmpError> {
        self.execute("cont", None).await.map(drop)
    }

    /// 게스트에 ACPI 전원 버튼 이벤트를 보낸다. 게스트가 응답하지 않을 수 있다.
    pub async fn system_powerdown(&self) -> Result<(), QmpError> {
        self.execute("system_powerdown", None).await.map(drop)
    }

    /// QEMU를 즉시 종료한다. 응답 전에 연결이 끊겨도 성공으로 본다.
    pub async fn quit(&self) -> Result<(), QmpError> {
        match self.execute("quit", None).await {
            Ok(_) | Err(QmpError::Disconnected) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for QmpClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_loop(
    mut lines: Lines<BufReader<OwnedReadHalf>>,
    pending: Pending,
    events: broadcast::Sender<QmpEvent>,
) {
    while let Ok(Some(line)) = lines.next_line().await {
        dispatch(&line, &pending, &events);
    }
    if let Some(map) = pending.lock().unwrap().take() {
        for (_, tx) in map {
            let _ = tx.send(Err(QmpError::Disconnected));
        }
    }
}

fn dispatch(line: &str, pending: &Pending, events: &broadcast::Sender<QmpEvent>) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if let Some(event) = msg.get("event").and_then(Value::as_str) {
        let data = msg.get("data").cloned().unwrap_or(Value::Null);
        let _ = events.send(QmpEvent {
            event: event.to_string(),
            data,
        });
        return;
    }
    let Some(id) = msg.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Some(tx) = pending.lock().unwrap().as_mut().and_then(|m| m.remove(&id)) else {
        return;
    };
    let reply = if let Some(ret) = msg.get("return") {
        Ok(ret.clone())
    } else if let Some(err) = msg.get("error") {
        let field = |k: &str| {
            err.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Err(QmpError::Command {
            class: field("class"),
            desc: field("desc"),
        })
    } else {
        Err(QmpError::Protocol(format!(
            "reply without return or error: {msg}"
        )))
    };
    let _ = tx.send(reply);
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cargo test -p opl-qemu`
Expected: `test result: ok. 14 passed`

- [ ] **Step 5: Commit**

```bash
git add crates/opl-qemu
git commit -m "feat(qemu): 비동기 QMP 클라이언트 추가 (id 매칭, 이벤트 브로드캐스트)"
```

---

### Task 9: 실제 QEMU 통합 테스트 (M1 완료 기준)

이 테스트는 설계 문서의 불변식 2번("opld를 재시작해도 VM은 중단되지 않고 다시 연결된다")을 QEMU 수준에서 처음 확인한다. QMP 연결을 끊었다가 다시 붙어도 VM이 계속 실행 중이어야 한다.

**Files:**
- Create: `crates/opl-qemu/tests/qemu_boot.rs`

- [ ] **Step 1: 통합 테스트 작성**

`crates/opl-qemu/tests/qemu_boot.rs`:

```rust
//! 실제 QEMU를 띄우는 통합 테스트. `cargo test -p opl-qemu --features qemu-it`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_qemu::{HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware, MachineType, NicModel};
use uuid::Uuid;

fn host() -> HostProfile {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::homebrew_hvf(Path::new(&share))
}

fn spec(id: Uuid, disk: PathBuf) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id,
        name: "it-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: MachineType::Virt,
        disks: vec![RuntimeDisk {
            path: disk,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }],
    }
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path:?} did not appear within 10s");
}

#[tokio::test]
async fn boots_paused_resumes_reconnects_and_quits() {
    let host = host();
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let id = Uuid::now_v7();
    let paths = RuntimePaths::new(tmp.path(), id).unwrap();
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::copy(&host.uefi_vars_template, &paths.uefi_vars).unwrap();

    let disk = tmp.path().join("root.qcow2");
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());

    let args = build_cmdline(&spec(id, disk), &host, &paths).unwrap();
    let mut child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for(&paths.qmp_socket).await;

    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Prelaunch);
    qmp.cont().await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);

    // opld 재시작을 흉내 낸다: 연결을 끊고 다시 붙어도 VM은 계속 실행 중이어야 한다.
    drop(qmp);
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}
```

- [ ] **Step 2: 기본 테스트에서는 제외되는지 확인**

Run: `cargo test -p opl-qemu`
Expected: `qemu_boot` 테스트 바이너리가 `0 passed`로 끝난다(기능 플래그가 꺼져 있음).

- [ ] **Step 3: 실제 QEMU로 실행**

Run: `cargo test -p opl-qemu --features qemu-it --test qemu_boot -- --nocapture`
Expected: `test boots_paused_resumes_reconnects_and_quits ... ok`

실패 시 확인할 것:
- `qemu-system-aarch64: -accel hvf: ...` → 다른 가상화 앱이 hvf를 점유하고 있는지 확인한다.
- `failed to find romfile` / pflash 크기 오류 → `OPL_QEMU_SHARE`가 Homebrew QEMU의 `share/qemu`를 가리키는지 확인한다.
- QMP 소켓이 생기지 않음 → `child`의 stderr를 보려면 `.stderr(Stdio::inherit())`를 잠시 추가해 QEMU 오류 메시지를 확인한다.

- [ ] **Step 4: 전체 검사**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Expected: 모두 통과. `opl-types` 9개, `opl-qemu` 14개.

- [ ] **Step 5: Commit**

```bash
git add crates/opl-qemu/tests
git commit -m "test(qemu): hvf에서 실제 QEMU 기동, 재개, 재연결, 종료 통합 테스트 추가"
```

---

### Task 10: README 갱신과 푸시

**Files:**
- Modify: `README.md`

- [ ] **Step 1: README에 개발 방법 추가**

`README.md`를 다음 내용으로 교체한다:

````markdown
# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M1)

## 개발

```bash
cargo test --workspace                                    # 단위 테스트
brew install qemu                                         # macOS 통합 테스트용
cargo test -p opl-qemu --features qemu-it --test qemu_boot  # 실제 QEMU 기동 테스트
```

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
````

- [ ] **Step 2: Commit과 푸시**

```bash
git add README.md
git commit -m "docs: README에 개발 방법 추가"
git push origin main
```

- [ ] **Step 3: CI 확인**

GitHub Actions의 `CI` 워크플로에서 `check (ubuntu-latest)`, `check (macos-latest)`, `deny` 세 작업이 모두 녹색인지 확인한다.

---

## 완료 기준 (M0 + M1)

- [ ] `cargo test --workspace`가 macOS와 Linux CI에서 통과한다.
- [ ] `cargo deny check`가 통과한다.
- [ ] macOS에서 `qemu-it` 통합 테스트가 통과한다: VM이 일시정지 상태로 뜨고, 실행이 재개되고, QMP 재연결 후에도 실행 중이며, `quit`으로 정상 종료된다.

## 다음 계획으로 넘길 사항

- MAC 자동 할당(`52:54:00` 접두사 + 난수)과 VmSpec → VmRuntimeSpec 변환은 계획 2(`opl-driver`)에서 구현한다.
- QEMU 프로세스 분리 실행(macOS `setsid`, Linux `systemd-run --scope`)과 pidfile 기반 재부착도 계획 2에서 구현한다.
- UEFI 변수 파일은 이 계획에서는 테스트가 직접 복사한다. 계획 2에서는 VM 생성 시 드라이버가 원본을 복사하고, VM과 같은 스토리지에 보관한다.

## 실행 후 기록 (2026-10-20)

코드 리뷰를 거치며 계획의 코드에서 아래가 바뀌었다. 저장소의 코드가 기준이다.

- `opl-types`: MAC 파싱이 `+` 부호를 받아들이던 버그 수정, `validate()`에 멀티캐스트 MAC·중복 MAC 검사 추가, `VmStatus`에 `#[serde(default)]` 추가 (테스트 10개)
- `opl-qemu` `QmpClient`: 쓰기를 전용 writer 태스크로 분리해 명령 취소 시 스트림이 손상되던 버그 수정, 취소된 명령의 pending 항목 제거, 연결이 끊기면 이벤트 구독자가 `Closed`를 받도록 수정, 동시 명령 id 매칭 테스트 추가 (테스트 17개)
- 설계 문서: 저장 형식 호환성 규칙(새 필드는 `#[serde(default)]` + 클러스터 기능 레벨 이후 기록), 런타임 파일 이름 갱신

다음 계획에 넘기는 사항:
- `QmpClient::execute`를 타임아웃으로 취소해도 명령은 나중에 실행될 수 있다. reconciler는 결과를 `query_status`로 다시 확인해야 한다.
- QMP writer 큐에는 상한이 없다. 재시도는 반드시 백오프를 거친다.
- Sata+virt, Bios+virt처럼 spec만으로 판정할 수 있는 조합은 API 단계(계획 5)에서 미리 거부하는 것을 검토한다.
