# opLite 계획 7: Linux/KVM과 브리지 네트워크 (M7) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** opld가 Linux에서 KVM(없으면 TCG)으로 VM을 돌리고, VM NIC를 관리자가 미리 만든 Linux 브리지에 tap으로(선택적으로 VLAN 태그와 함께) 잇는다. `Network` 리소스와 API·CLI, 브리지를 보는 스케줄러, VM마다 systemd 스코프를 더하고, Linux E2E와 CI의 KVM 작업으로 고정한다(M7).

**Architecture:** `opl-qemu`의 `HostProfile`에 Debian/Ubuntu 프로필과 KVM 감지를 더하고, `opl-driver`에 호스트 네트워크 트레이트 `HostNet`과 iproute2 구현을 둔다. QEMU 드라이버는 QEMU를 띄우기 전에 tap을 만들고 QEMU가 끝나면 지운다(tap 이름이 VM id에서 정해지므로 opld가 다시 시작해도 찾는다). reconciler가 네트워크 이름을 브리지·VLAN으로 풀고, 노드는 자기 브리지 목록을 알리며, 스케줄러는 브리지가 있는 노드만 고른다. opld는 `[hypervisor]` 설정으로 가속기와 systemd 스코프를 정한다.

**Tech Stack:** Rust 2024(MSRV 1.88), QEMU(Linux: `qemu-system-*`, OVMF/AAVMF; macOS: Homebrew), iproute2(`ip`, `bridge` JSON 출력), systemd-run, tokio 1.54.1

**설계 문서:** `docs/superpowers/specs/2026-10-20-opl-node-core-design.md` (2.2, 3.2, 3.3, 5.2, 5.4, 7, 10, 11절)
**선행 계획:** 계획 1~6 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| Linux QEMU 프로필(x86_64 OVMF, aarch64 AAVMF), KVM 감지와 TCG 대체 | 브리지 생성·관리, SDN, 방화벽 — 하위 프로젝트 6 |
| `Network` 리소스(브리지, VLAN), `/networks` API, `oplctl network` | 네트워크 수정(PUT). 지우고 다시 만든다 |
| tap 생성·VLAN(VLAN-aware 브리지, pvid untagged)·삭제, 재시작 후에도 VM의 tap 찾기 | 게스트 안에서의 통신 확인(게스트 이미지) — 나중에 |
| 노드의 브리지 목록 알리기(시작, 1분마다), 브리지를 보는 스케줄러 | cgroup 자원 제한(CPU/메모리) — 나중에 |
| QEMU를 `systemd-run --scope`로 감싸기(설정으로 끔) | QEMU 권한 낮추기(root가 아닌 사용자로 실행) — 나중에 |
| Linux에서 QMP 연결 `EAGAIN` 재시도 | Linux aarch64 KVM(GIC 등) 검증 — CI는 x86_64만 |
| E2E: 브리지 VM의 tap·VLAN 확인, CI `linux-kvm`(실제 KVM, systemd 스코프) | |
| tokio 1.54.1 (RUSTSEC-2026-0384) | |

## 이전 계획에서 이어받는 사항

계획 6의 "다음 계획으로 넘길 사항"에서 "QemuKvm, bridge 네트워크(계획 7)"를 다룬다. 계획 1에서 미룬 "Linux `systemd-run --scope`"도 여기서 한다.

## 실행 환경

개발 Mac(Apple M1)은 중첩 가상화가 없어 KVM을 쓸 수 없다. Linux 테스트는 Docker(Rancher Desktop 등)의 privileged 컨테이너에서 root로, TCG로 돌린다. 실제 KVM과 systemd 스코프는 CI의 `linux-kvm` 작업에서 확인한다(Task 11).

```bash
# 이미지(한 번)
cat > /tmp/opl-lxdev.Dockerfile <<'EOF'
FROM rust:bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    qemu-system-arm qemu-system-x86 qemu-efi-aarch64 ovmf qemu-utils iproute2 libssl-dev pkg-config procps \
 && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
EOF
docker build -t opl-lxdev -f /tmp/opl-lxdev.Dockerfile /tmp
# --init: 강제로 죽인 QEMU(데몬)를 거둬 좀비로 남지 않게 한다. 없으면 kill 테스트가 실패한다.
docker run -d --init --privileged --name opl-lx -v opl-target:/target -e CARGO_TARGET_DIR=/target \
    -v "$PWD":/ws -w /ws opl-lxdev sleep infinity
docker exec opl-lx cargo test -p opl-driver --features qemu-it,net-it
```

작업 디렉터리를 바인드 마운트할 수 없는 환경(예: Docker VM에 공유되지 않은 경로)에서는 `tar`로 복사해 넣는다. 컨테이너에는 systemd가 없으므로 `OPL_TEST_SYSTEMD`를 주지 않는다.

## 설계 결정

- **VLAN은 VLAN-aware 브리지로 한다.** 관리자가 `vlan_filtering 1`로 만든 브리지 하나에 여러 VLAN을 둔다(Proxmox의 VLAN-aware 모드). tap은 브리지 기본 pvid를 빼고 그 VLAN만 태그 없이 주고받는다(`bridge vlan add vid N pvid untagged`). VLAN을 쓰는 네트워크인데 브리지가 VLAN-aware가 아니면 시작하지 않는다(필터링이 꺼진 브리지에서는 VLAN이 격리되지 않는다). VLAN마다 브리지를 만드는 방식은 브리지 관리라 하위 프로젝트 6이다.
- **tap은 iproute2 명령으로 다룬다.** 의존성이 늘지 않고 운영자가 같은 명령으로 확인·복구할 수 있다. 명령 순서는 순수 함수(`tap_commands`)로 만들어 단위 테스트로 고정하고, 출력은 `-j`(JSON)로 읽는다.
- **tap 이름은 VM id와 NIC 순번으로 정한다.** `op` + VM id(UUIDv7)의 무작위 뒷부분 10자리 + `n` + 순번. 앞부분은 생성 시각이라 겹칠 수 있어 쓰지 않는다. 이름만으로 VM의 tap을 모두 찾을 수 있으므로 상태를 따로 저장하지 않는다. 15자 제한 때문에 VM의 NIC는 16개까지다.
- **tap은 QEMU 앞뒤로 드라이버가 만들고 지운다.** QEMU의 `-netdev tap,ifname=...,script=no`가 미리 만든 tap을 연다. 지우는 곳: 모니터가 본 종료(정상·크래시), 기동 실패 정리, `new()`가 지우는 죽은 VM. 지우기 실패는 기록만 하고, 다음 시작이 같은 이름을 지우고 다시 만든다. opld가 다시 시작해 살아 있는 VM에 다시 붙을 때는 tap을 건드리지 않는다.
- **opld는 Linux에서 root로 돈다.** tap을 만들려면 CAP_NET_ADMIN이 필요하다. QEMU도 root로 돈다(권한 낮추기는 나중에).
- **`user`는 예약된 네트워크 이름이다.** 리소스 없이 QEMU user-mode NAT로 연결한다. 같은 이름의 네트워크는 만들 수 없다. 네트워크를 고치는 API는 두지 않는다(실행 중인 VM의 tap과 어긋나므로 지우고 다시 만든다).
- **노드가 브리지를 알리고 스케줄러가 본다.** 노드는 시작할 때와 1분마다 브리지 목록(이름, VLAN-aware 여부)을 Node status에 기록한다(바뀔 때만 쓴다). 스케줄러는 VM의 네트워크가 쓰는 브리지가 모두 있는 노드만 고른다. `spec.node`를 직접 정한 VM은 지금처럼 확인하지 않는다(그 노드에 브리지가 없으면 시작이 영구 오류로 실패한다).
- **가속기는 `auto`가 기본이다.** macOS는 hvf, Linux는 `/dev/kvm`을 열 수 있으면 KVM, 아니면 TCG(경고). `accel = "kvm"`을 명시했는데 열 수 없으면 opld가 시작하지 않는다. `NodeStatus.hypervisor`는 `qemu-kvm`/`qemu-tcg`/`qemu-hvf`다.
- **QEMU는 `systemd-run --scope`로 감싼다.** `--unit=opl-vm-<id> --collect`. `-daemonize`한 QEMU가 스코프에 남아 opld와 다른 cgroup에서 돈다(opld를 다시 시작해도 VM이 서비스 cgroup과 함께 죽지 않는다). systemd가 없는 환경(컨테이너)을 위해 `[hypervisor] systemd_scope = false`로 끈다. 자원 제한은 아직 걸지 않는다.
- **QMP 연결은 `EAGAIN`이면 다시 시도한다.** QEMU의 QMP 소켓 대기열은 1이다. 응답하지 않던 QEMU(SIGSTOP 등)에 쌓인 연결 때문에 Linux의 비차단 `connect`가 `EAGAIN`으로 곧바로 실패한다. 50ms 간격으로 5초까지 다시 시도한다(호출자의 제한 시간 안에서).

## 파일 구조

```
Cargo.toml                                    # 수정: tokio 1.54.1
.github/workflows/ci.yml                      # 수정: linux-kvm 작업
README.md, docs/.../2026-10-20-opl-node-core-design.md   # 수정: M7
crates/opl-types/src/
├── network.rs                                # 새로: Network, USER_NETWORK, tap_name, valid_ifname
├── node.rs                                   # 수정: NodeStatus.bridges, BridgeInfo
├── runtime.rs                                # 수정: NicBackend::Tap { ifname, bridge, vlan }
├── resource.rs, lib.rs                       # 수정: Kind::Network
└── vm.rs                                     # 수정: MAX_NICS
crates/opl-qemu/src/{host,qmp,cmdline}.rs     # 수정: debian/native 프로필, kvm_available, QMP EAGAIN
crates/opl-driver/
├── src/net.rs                                # 새로: HostNet, IpRoute, Tap
├── src/qemu.rs                               # 수정: tap 수명, systemd 스코프
└── tests/{host_net,qemu_bridge}.rs           # 새로: net-it (Linux, root)
crates/opl-reconciler/src/resolve.rs          # 수정: 브리지 네트워크 → Tap
crates/opl-api/src/{resources,scheduler,vms,watch,lib}.rs   # 수정: /networks, 스케줄러
crates/opl-api/tests/networks.rs              # 새로
crates/oplctl/src/cli.rs                      # 수정: network 명령
crates/opld/src/{config,host,bootstrap,member}.rs          # 수정: [hypervisor], Host, 브리지 알리기
crates/opld/tests/bridge.rs                   # 새로: E2E (Linux, root)
```

---


### Task 1: tokio 1.54.1로 올리기 (RUSTSEC-2026-0384)

tokio 1.54.0의 `Interval`에 런타임 시간 드라이버와의 경쟁 조건(use-after-free)이 있어 1.54.0이 회수되었다. `NodeMonitor`가 `Interval`을 쓰고 CI의 `cargo deny`가 막으므로 가장 먼저 올린다.

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: 버전 올리기**

`Cargo.toml`에서

```toml
thiserror = "2.0.21"
tokio = { version = "1.54.0", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time", "process", "fs", "signal"] }
uuid = { version = "1.28.0", features = ["v4", "v7", "serde"] }
```

을 다음으로 바꾼다:

```toml
thiserror = "2.0.21"
tokio = { version = "1.54.1", features = ["rt-multi-thread", "macros", "net", "io-util", "sync", "time", "process", "fs", "signal"] }
uuid = { version = "1.28.0", features = ["v4", "v7", "serde"] }
```

- [ ] **Step 2: 확인**

Run: `cargo update -p tokio --precise 1.54.1`
Expected: `Updating tokio v1.54.0 -> v1.54.1`

Run: `cargo deny check`
Expected: `advisories ok, bans ok, licenses ok, sources ok`

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add Cargo.toml Cargo.lock
git commit -m "fix(deps): tokio 1.54.1 (RUSTSEC-2026-0384, Interval 경쟁 조건)"
```

---

### Task 2: 리소스 타입: 네트워크, 노드의 브리지, tap 백엔드

`Network` 리소스(브리지 이름과 VLAN 태그)와 `Kind::Network`를 더한다. 예약된 이름 `user`는 리소스 없이 QEMU NAT로 남는다. 노드 status에 브리지 목록(`BridgeInfo`)을 두고, 런타임 NIC의 `NicBackend::Tap`에 브리지와 VLAN을 싣는다. 다른 크레이트의 구조체 리터럴은 새 필드만 채운다(뒤 Task에서 실제 값을 넣는다).

**Files:**
- Create: `crates/opl-types/src/network.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-qemu/src/cmdline.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/opl-types/src/runtime.rs`
- Modify: `crates/opld/src/bootstrap.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
            hypervisor: "mock".into(),
        };
```

을 다음으로 바꾼다:

```rust
            hypervisor: "mock".into(),
            bridges: vec![],
        };
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            hypervisor: "mock".into(),
        };
```

을 다음으로 바꾼다:

```rust
            hypervisor: "mock".into(),
            bridges: vec![],
        };
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            hypervisor: "mock".into(),
        };
```

을 다음으로 바꾼다:

```rust
            hypervisor: "mock".into(),
            bridges: vec![],
        };
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
```

을 다음으로 바꾼다:

```rust
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname, .. } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
                        ifname: "tap0i0".into(),
                    },
```

을 다음으로 바꾼다:

```rust
                        ifname: "tap0i0".into(),
                        bridge: "vmbr0".into(),
                        vlan: Some(10),
                    },
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod cluster;
pub mod node;
```

을 다음으로 바꾼다:

```rust
pub mod cluster;
pub mod network;
pub mod node;
```

`crates/opl-types/src/network.rs`를 만든다:

```rust
//! VM NIC가 연결하는 네트워크. 지금은 관리자가 미리 만든 Linux 브리지만 있다.

use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 예약된 네트워크 이름. 리소스가 없고 QEMU user-mode NAT로 연결한다(macOS 개발·데모용).
pub const USER_NETWORK: &str = "user";

pub type Network = Resource<NetworkSpec, NetworkStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NetworkSpec {
    /// 노드에 미리 만들어 둔 Linux 브리지 이름(예: `vmbr0`)
    pub bridge: String,
    /// VLAN 태그(1~4094). 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다.
    #[serde(default)]
    pub vlan: Option<u16>,
}

impl ResourceSpec for NetworkSpec {
    const KIND: Kind = Kind::Network;
    type Status = NetworkStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NetworkStatus {}

impl NetworkSpec {
    pub fn validate(&self) -> Result<(), NetworkError> {
        if !valid_ifname(&self.bridge) {
            return Err(NetworkError::BadBridge(self.bridge.clone()));
        }
        if let Some(vlan) = self.vlan
            && !(1..=4094).contains(&vlan)
        {
            return Err(NetworkError::BadVlan(vlan));
        }
        Ok(())
    }
}

/// Linux 인터페이스 이름 규칙: 1~15바이트, `/`·공백·`:` 없음, `.`·`..`가 아님.
pub fn valid_ifname(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'/' && c != b':')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkError {
    #[error("invalid bridge name {0:?}: use 1-15 characters without '/', ':' or spaces")]
    BadBridge(String),
    #[error("invalid VLAN tag {0}: use 1-4094")]
    BadVlan(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(bridge: &str, vlan: Option<u16>) -> NetworkSpec {
        NetworkSpec {
            bridge: bridge.into(),
            vlan,
        }
    }

    #[test]
    fn validates_bridge_and_vlan() {
        assert!(spec("vmbr0", None).validate().is_ok());
        assert!(spec("br-lan.10", Some(4094)).validate().is_ok());
        for bad in ["", "a/b", "a b", "a:b", ".", "..", "abcdefghijklmnop"] {
            assert_eq!(
                spec(bad, None).validate(),
                Err(NetworkError::BadBridge(bad.into()))
            );
        }
        assert_eq!(
            spec("vmbr0", Some(0)).validate(),
            Err(NetworkError::BadVlan(0))
        );
        assert_eq!(
            spec("vmbr0", Some(4095)).validate(),
            Err(NetworkError::BadVlan(4095))
        );
    }

    #[test]
    fn vlan_is_optional() {
        let s: NetworkSpec =
            serde_json::from_value(serde_json::json!({ "bridge": "vmbr0" })).unwrap();
        assert_eq!(s, spec("vmbr0", None));
    }
}
```

`crates/opl-types/src/node.rs`에서

```rust
    pub memory_mib: u64,
    /// 예: `qemu-hvf`, `qemu-kvm`
    pub hypervisor: String,
}
```

을 다음으로 바꾼다:

```rust
    pub memory_mib: u64,
    /// 예: `qemu-hvf`, `qemu-kvm`, `qemu-tcg`
    pub hypervisor: String,
    /// 이 노드에 있는 Linux 브리지. 스케줄러가 브리지 네트워크를 쓰는 VM을 배치할 때 본다.
    pub bridges: Vec<BridgeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BridgeInfo {
    pub name: String,
    /// VLAN-aware 브리지(`vlan_filtering 1`)인지. VLAN 태그를 쓰는 네트워크에 필요하다.
    pub vlan_filtering: bool,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
    JoinToken,
}
```

을 다음으로 바꾼다:

```rust
    JoinToken,
    Network,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::JoinToken => "join-token",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::JoinToken => "join-token",
            Kind::Network => "network",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::JoinToken,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::JoinToken,
            Kind::Network,
        ] {
```

`crates/opl-types/src/runtime.rs`에서

```rust
    User,
    /// 미리 만들어 둔 tap 장치
    Tap { ifname: String },
}
```

을 다음으로 바꾼다:

```rust
    User,
    /// 드라이버가 만들어 브리지에 붙이는 tap 장치
    Tap {
        ifname: String,
        bridge: String,
        vlan: Option<u16>,
    },
}
```

`crates/opld/src/bootstrap.rs`에서

```rust
        hypervisor: "qemu-hvf".into(),
    };
```

을 다음으로 바꾼다:

```rust
        hypervisor: "qemu-hvf".into(),
        bridges: vec![],
    };
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-types && cargo build --workspace --all-targets`
Expected: PASS (`validates_bridge_and_vlan`, `vlan_is_optional`, `kind_serializes_as_its_str` 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(types): Network 리소스, 노드 브리지 목록, tap 백엔드의 브리지·VLAN"
```

---

### Task 3: QEMU 호스트 프로필: Debian/Ubuntu, KVM 감지

`HostProfile::debian(arch, accel)`(OVMF/AAVMF 경로)과 `HostProfile::native(share_dir)`(macOS는 Homebrew hvf, Linux는 `/dev/kvm`을 열 수 있으면 KVM, 아니면 TCG)를 더한다. `Arch::native()`와 `Arch::machine()`으로 실제 QEMU를 띄우는 테스트가 호스트에 맞는 머신 타입을 쓰게 해, 같은 `qemu-it` 테스트가 macOS와 Linux에서 돈다.

**Files:**
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-qemu/Cargo.toml`
- Modify: `crates/opl-qemu/src/host.rs`
- Modify: `crates/opl-qemu/src/lib.rs`
- Modify: `crates/opl-qemu/tests/qemu_boot.rs`
- Modify: `crates/opl-reconciler/Cargo.toml`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/qemu_restart.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/Cargo.toml`에서

```toml
storage-it = []
# 실제 VM을 띄우는 통합 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []
```

을 다음으로 바꾼다:

```toml
storage-it = []
# 실제 VM을 띄우는 통합 테스트. macOS는 brew install qemu, Linux는 apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils가 필요하다(KVM이 없으면 TCG).
qemu-it = []
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]
```

을 다음으로 바꾼다:

```rust
//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
};
use opl_qemu::HostProfile;
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};
use tokio::sync::broadcast;
```

을 다음으로 바꾼다:

```rust
};
use opl_qemu::{Arch, HostProfile};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{CpuModel, CpuSpec, Firmware};
use tokio::sync::broadcast;
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
    QemuConfig {
        host: HostProfile::homebrew_hvf(Path::new(&share)),
        run_dir: root.join("run"),
```

을 다음으로 바꾼다:

```rust
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
        firmware: Firmware::Uefi,
        machine: MachineType::Virt,
        disks: vec![],
```

을 다음으로 바꾼다:

```rust
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![],
```

`crates/opl-qemu/Cargo.toml`에서

```toml
[features]
# 실제 QEMU를 실행하는 통합 테스트. macOS(hvf)에서는 `brew install qemu`가 필요하다.
qemu-it = []
```

을 다음으로 바꾼다:

```toml
[features]
# 실제 QEMU를 실행하는 통합 테스트. macOS는 brew install qemu, Linux는 apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils가 필요하다(KVM이 없으면 TCG).
qemu-it = []
```

`crates/opl-qemu/src/host.rs`에서

```rust
use std::path::{Path, PathBuf};
```

을 다음으로 바꾼다:

```rust
use std::path::{Path, PathBuf};

use opl_types::vm::MachineType;
```

`crates/opl-qemu/src/host.rs`에서

```rust
    Tcg,
}
```

을 다음으로 바꾼다:

```rust
    Tcg,
}

impl Arch {
    /// 이 프로그램이 컴파일된 아키텍처
    pub fn native() -> Self {
        if cfg!(target_arch = "x86_64") {
            Arch::X86_64
        } else {
            Arch::Aarch64
        }
    }

    /// 이 아키텍처에서 쓸 수 있는 머신 타입
    pub fn machine(self) -> MachineType {
        match self {
            Arch::X86_64 => MachineType::Q35,
            Arch::Aarch64 => MachineType::Virt,
        }
    }
}
```

`crates/opl-qemu/src/host.rs`에서

```rust
            uefi_vars_template: share_dir.join("edk2-arm-vars.fd"),
        }
    }
}
```

을 다음으로 바꾼다:

```rust
            uefi_vars_template: share_dir.join("edk2-arm-vars.fd"),
        }
    }

    /// Debian/Ubuntu의 QEMU와 UEFI 펌웨어(`ovmf`, `qemu-efi-aarch64` 패키지).
    pub fn debian(arch: Arch, accel: Accel) -> Self {
        match arch {
            Arch::X86_64 => Self {
                arch,
                accel,
                qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd"),
                uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd"),
            },
            Arch::Aarch64 => Self {
                arch,
                accel,
                qemu_binary: PathBuf::from("/usr/bin/qemu-system-aarch64"),
                uefi_code: PathBuf::from("/usr/share/AAVMF/AAVMF_CODE.fd"),
                uefi_vars_template: PathBuf::from("/usr/share/AAVMF/AAVMF_VARS.fd"),
            },
        }
    }

    /// 이 호스트의 기본 프로필. macOS는 Homebrew QEMU와 hvf, Linux는 Debian 경로와
    /// KVM(`/dev/kvm`을 열 수 없으면 TCG)이다.
    pub fn native(share_dir: &Path) -> Self {
        if cfg!(target_os = "macos") {
            Self::homebrew_hvf(share_dir)
        } else {
            let accel = if kvm_available() {
                Accel::Kvm
            } else {
                Accel::Tcg
            };
            Self::debian(Arch::native(), accel)
        }
    }
}

/// `/dev/kvm`을 읽고 쓸 수 있는지. QEMU의 `accel=kvm`이 여는 장치다.
pub fn kvm_available() -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debian_profiles_match_the_arch() {
        let x86 = HostProfile::debian(Arch::X86_64, Accel::Kvm);
        assert_eq!(x86.qemu_binary, Path::new("/usr/bin/qemu-system-x86_64"));
        assert!(x86.uefi_code.ends_with("OVMF_CODE_4M.fd"));
        let arm = HostProfile::debian(Arch::Aarch64, Accel::Tcg);
        assert_eq!(arm.accel, Accel::Tcg);
        assert!(arm.uefi_vars_template.ends_with("AAVMF_VARS.fd"));
    }

    #[test]
    fn native_profile_uses_this_arch() {
        let p = HostProfile::native(Path::new("/opt/homebrew/share/qemu"));
        assert_eq!(p.arch, Arch::native());
        if cfg!(target_os = "macos") {
            assert_eq!(p.accel, Accel::Hvf);
        } else {
            assert_ne!(p.accel, Accel::Hvf);
        }
    }
}
```

`crates/opl-qemu/src/lib.rs`에서

```rust
pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, Arch, HostProfile};
pub use paths::{PathError, RuntimePaths};
```

을 다음으로 바꾼다:

```rust
pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, Arch, HostProfile, kvm_available};
pub use paths::{PathError, RuntimePaths};
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
//! 실제 QEMU를 띄우는 통합 테스트. `cargo test -p opl-qemu --features qemu-it`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]
```

을 다음으로 바꾼다:

```rust
//! 실제 QEMU를 띄우는 통합 테스트. `cargo test -p opl-qemu --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust

use opl_qemu::{HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware, MachineType, NicModel};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust

use opl_qemu::{Arch, HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware, NicModel};
use uuid::Uuid;
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::homebrew_hvf(Path::new(&share))
}
```

을 다음으로 바꾼다:

```rust
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::native(Path::new(&share))
}
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
        firmware: Firmware::Uefi,
        machine: MachineType::Virt,
        disks: vec![RuntimeDisk {
```

을 다음으로 바꾼다:

```rust
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![RuntimeDisk {
```

`crates/opl-reconciler/Cargo.toml`에서

```toml
[features]
# 실제 QEMU로 opld 재시작 후 재연결을 확인하는 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []
```

을 다음으로 바꾼다:

```toml
[features]
# 실제 QEMU로 opld 재시작 후 재연결을 확인하는 테스트. macOS는 brew install qemu, Linux는 apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils가 필요하다(KVM이 없으면 TCG).
qemu-it = []
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
use opl_types::resource::Resource;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType, PowerState, Vm, VmSpec};
use tempfile::TempDir;
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, PowerState, Vm, VmSpec};
use tempfile::TempDir;
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust

/// 디스크와 NIC 없이 UEFI로 뜨는 작은 aarch64 VM.
pub fn vm(name: &str, node: Uuid) -> Vm {
```

을 다음으로 바꾼다:

```rust

/// 디스크와 NIC 없이 UEFI로 뜨는 작은 VM(이 호스트의 머신 타입).
pub fn vm(name: &str, node: Uuid) -> Vm {
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
```

을 다음으로 바꾼다:

```rust
            firmware: Firmware::Uefi,
            machine: opl_qemu::Arch::native().machine(),
            disks: vec![],
```

`crates/opl-reconciler/tests/qemu_restart.rs`에서

```rust
//! `cargo test -p opl-reconciler --features qemu-it --test qemu_restart`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]
```

을 다음으로 바꾼다:

```rust
//! `cargo test -p opl-reconciler --features qemu-it --test qemu_restart`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]
```

`crates/opl-reconciler/tests/qemu_restart.rs`에서

```rust
    QemuConfig {
        host: HostProfile::homebrew_hvf(Path::new(&share)),
        run_dir: root.join("run"),
```

을 다음으로 바꾼다:

```rust
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-qemu`
Expected: PASS (`debian_profiles_match_the_arch`, `native_profile_uses_this_arch` 포함)

Run: `cargo test -p opl-qemu -p opl-driver -p opl-reconciler --features qemu-it`
Expected: macOS(hvf)와 Linux 컨테이너(아래 '실행 환경')에서 PASS. 단, Linux에서는 `start_does_not_destroy_unresponsive_running_vm`이 Task 5(QMP 연결 재시도) 전까지 실패할 수 있다(특히 단독으로 돌릴 때).

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(qemu): Debian/Ubuntu 호스트 프로필과 KVM 감지, 통합 테스트를 호스트에 맞게"
```

---

### Task 4: 호스트 네트워크: tap을 만들어 브리지에 잇기

`opl-driver`의 `HostNet` 트레이트와 iproute2 구현 `IpRoute`를 더한다. `create_tap`은 브리지가 있는지, VLAN이면 VLAN-aware인지 확인하고, 남은 같은 이름을 지운 뒤 `ip tuntap add` → `master` → (VLAN이면 브리지 기본 pvid 제거, `vid N pvid untagged`) → `up` 순으로 만든다. 실패하면 만든 tap을 지운다. tap 이름은 VM id의 무작위 뒷부분 10자리와 NIC 순번(`op<10자리>n<순번>`)이라 opld가 다시 시작해도 `delete_vm_taps(vm)`이 접두사로 찾는다. 이름이 15자를 넘지 않도록 VM의 NIC는 16개까지다. 실제 tap·브리지 테스트는 `net-it` 기능(Linux, root)이다.

**Files:**
- Create: `crates/opl-driver/src/net.rs`
- Create: `crates/opl-driver/tests/host_net.rs`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-types/src/network.rs`
- Modify: `crates/opl-types/src/vm.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/Cargo.toml`에서

```toml
qemu-it = []
```

을 다음으로 바꾼다:

```toml
qemu-it = []
# 실제 tap과 브리지를 만드는 통합 테스트. Linux에서 root(CAP_NET_ADMIN)와 iproute2가 필요하다.
net-it = []
```

`crates/opl-driver/Cargo.toml`에서

```toml
rustix.workspace = true
serde_json.workspace = true
```

을 다음으로 바꾼다:

```toml
rustix.workspace = true
serde.workspace = true
serde_json.workspace = true
```

`crates/opl-driver/src/lib.rs`에서

```rust
//! 하이퍼바이저와 스토리지 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.
```

을 다음으로 바꾼다:

```rust
//! 하이퍼바이저, 스토리지, 호스트 네트워크 드라이버. reconciler와 API는 트레이트만 보고, 구현은 여기서 고른다.
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub mod mock;
pub mod qemu;
```

을 다음으로 바꾼다:

```rust
pub mod mock;
pub mod net;
pub mod qemu;
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use mock::MockHypervisor;
pub use opl_types::storage::SourceFormat;
```

을 다음으로 바꾼다:

```rust
pub use mock::MockHypervisor;
pub use net::{HostNet, IpRoute, Tap};
pub use opl_types::storage::SourceFormat;
```

`crates/opl-driver/src/net.rs`를 만든다:

```rust
//! VM NIC를 Linux 브리지에 잇는 tap 장치. iproute2(`ip`, `bridge`) 명령을 부르므로 opld가
//! root(또는 CAP_NET_ADMIN)로 돌아야 한다. 브리지 자체는 관리자가 미리 만든다.

use async_trait::async_trait;
use opl_types::network::tap_prefix;
use opl_types::node::BridgeInfo;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::DriverError;

/// 드라이버가 만들어 브리지에 붙이는 tap 장치
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tap {
    pub ifname: String,
    pub bridge: String,
    pub vlan: Option<u16>,
}

/// 호스트 네트워크 조작. 테스트에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait HostNet: Send + Sync + std::fmt::Debug {
    /// 이 호스트의 Linux 브리지
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError>;
    /// tap을 만들어 브리지에 붙이고 올린다. 같은 이름이 남아 있으면 지우고 다시 만든다.
    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError>;
    /// VM의 tap을 모두 지운다. 없으면 아무것도 하지 않는다.
    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError>;
}

/// iproute2로 tap을 다룬다.
#[derive(Debug, Default)]
pub struct IpRoute;

#[async_trait]
impl HostNet for IpRoute {
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
        let out = run("ip", &["-j", "-d", "link", "show", "type", "bridge"]).await?;
        Ok(parse_bridges(&out)?.into_iter().map(|b| b.info).collect())
    }

    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError> {
        let out = run("ip", &["-j", "-d", "link", "show", "type", "bridge"]).await?;
        let bridges = parse_bridges(&out)?;
        let Some(bridge) = bridges.iter().find(|b| b.info.name == tap.bridge) else {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} not found on this node",
                tap.bridge
            )));
        };
        if tap.vlan.is_some() && !bridge.info.vlan_filtering {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} is not VLAN-aware (vlan_filtering 1)",
                tap.bridge
            )));
        }
        // 이전 VM 인스턴스가 남긴 같은 이름의 tap
        delete_link(&tap.ifname).await?;
        for (program, args) in tap_commands(tap, bridge.default_pvid) {
            if let Err(e) = run(program, &args).await {
                let _ = delete_link(&tap.ifname).await;
                return Err(e);
            }
        }
        Ok(())
    }

    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError> {
        let out = run("ip", &["-j", "link", "show"]).await?;
        let links: Vec<Link> = serde_json::from_str(&out).map_err(bad_output)?;
        let prefix = tap_prefix(vm);
        for link in links.iter().filter(|l| l.ifname.starts_with(&prefix)) {
            delete_link(&link.ifname).await?;
        }
        Ok(())
    }
}

/// tap을 만들고 브리지에 붙여 올리는 명령. VLAN이면 브리지 기본 VLAN 대신 그 VLAN만
/// 태그 없이 주고받게 한다.
fn tap_commands(tap: &Tap, default_pvid: u16) -> Vec<(&'static str, Vec<String>)> {
    let dev = tap.ifname.as_str();
    let mut cmds = vec![
        ("ip", args(&["tuntap", "add", "dev", dev, "mode", "tap"])),
        (
            "ip",
            args(&["link", "set", "dev", dev, "master", &tap.bridge]),
        ),
    ];
    if let Some(vlan) = tap.vlan {
        if default_pvid != 0 && default_pvid != vlan {
            let pvid = default_pvid.to_string();
            cmds.push(("bridge", args(&["vlan", "del", "dev", dev, "vid", &pvid])));
        }
        let vid = vlan.to_string();
        cmds.push((
            "bridge",
            args(&["vlan", "add", "dev", dev, "vid", &vid, "pvid", "untagged"]),
        ));
    }
    cmds.push(("ip", args(&["link", "set", "dev", dev, "up"])));
    cmds
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

async fn delete_link(ifname: &str) -> Result<(), DriverError> {
    match run("ip", &["link", "del", "dev", ifname]).await {
        Err(DriverError::ToolFailed { stderr, .. }) if stderr.contains("Cannot find device") => {
            Ok(())
        }
        other => other.map(|_| ()),
    }
}

async fn run(program: &'static str, args: &[impl AsRef<str>]) -> Result<String, DriverError> {
    let output = tokio::process::Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .output()
        .await?;
    if !output.status.success() {
        return Err(DriverError::ToolFailed {
            tool: program,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, Deserialize)]
struct Link {
    ifname: String,
    #[serde(default)]
    linkinfo: Option<LinkInfo>,
}

#[derive(Debug, Deserialize)]
struct LinkInfo {
    #[serde(default)]
    info_data: Option<BridgeData>,
}

#[derive(Debug, Deserialize)]
struct BridgeData {
    #[serde(default)]
    vlan_filtering: u8,
    #[serde(default)]
    vlan_default_pvid: u16,
}

#[derive(Debug, PartialEq, Eq)]
struct Bridge {
    info: BridgeInfo,
    default_pvid: u16,
}

/// `ip -j -d link show type bridge`의 출력
fn parse_bridges(json: &str) -> Result<Vec<Bridge>, DriverError> {
    let links: Vec<Link> = serde_json::from_str(json).map_err(bad_output)?;
    Ok(links
        .into_iter()
        .map(|l| {
            let data = l.linkinfo.and_then(|i| i.info_data);
            Bridge {
                info: BridgeInfo {
                    name: l.ifname,
                    vlan_filtering: data.as_ref().is_some_and(|d| d.vlan_filtering == 1),
                },
                default_pvid: data.map_or(1, |d| d.vlan_default_pvid),
            }
        })
        .collect())
}

fn bad_output(e: serde_json::Error) -> DriverError {
    DriverError::ToolFailed {
        tool: "ip",
        stderr: format!("unexpected JSON output: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(vlan: Option<u16>) -> Tap {
        Tap {
            ifname: "op0123456789n0".into(),
            bridge: "vmbr0".into(),
            vlan,
        }
    }

    fn joined(cmds: Vec<(&'static str, Vec<String>)>) -> Vec<String> {
        cmds.into_iter()
            .map(|(p, a)| format!("{p} {}", a.join(" ")))
            .collect()
    }

    #[test]
    fn plain_tap_joins_the_bridge() {
        assert_eq!(
            joined(tap_commands(&tap(None), 1)),
            [
                "ip tuntap add dev op0123456789n0 mode tap",
                "ip link set dev op0123456789n0 master vmbr0",
                "ip link set dev op0123456789n0 up",
            ]
        );
    }

    #[test]
    fn vlan_tap_replaces_the_default_pvid() {
        assert_eq!(
            joined(tap_commands(&tap(Some(10)), 1)),
            [
                "ip tuntap add dev op0123456789n0 mode tap",
                "ip link set dev op0123456789n0 master vmbr0",
                "bridge vlan del dev op0123456789n0 vid 1",
                "bridge vlan add dev op0123456789n0 vid 10 pvid untagged",
                "ip link set dev op0123456789n0 up",
            ]
        );
        // 브리지에 기본 VLAN이 없거나(0) 같은 VLAN이면 지울 것이 없다.
        for pvid in [0, 10] {
            let cmds = joined(tap_commands(&tap(Some(10)), pvid));
            assert!(!cmds.iter().any(|c| c.contains("vlan del")), "{cmds:?}");
        }
    }

    #[test]
    fn parses_bridges() {
        let json = r#"[
            {"ifindex":2,"ifname":"vmbr0","linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":1,"vlan_default_pvid":1}}},
            {"ifindex":3,"ifname":"br-plain","linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0,"vlan_default_pvid":1}}}
        ]"#;
        assert_eq!(
            parse_bridges(json).unwrap(),
            [
                Bridge {
                    info: BridgeInfo {
                        name: "vmbr0".into(),
                        vlan_filtering: true
                    },
                    default_pvid: 1
                },
                Bridge {
                    info: BridgeInfo {
                        name: "br-plain".into(),
                        vlan_filtering: false
                    },
                    default_pvid: 1
                },
            ]
        );
        assert!(parse_bridges("[]").unwrap().is_empty());
        assert!(parse_bridges("not json").is_err());
    }
}
```

`crates/opl-driver/tests/host_net.rs`를 만든다:

```rust
//! 실제 브리지와 tap을 만드는 통합 테스트. `cargo test -p opl-driver --features net-it`
//! Linux에서 root(CAP_NET_ADMIN)와 iproute2가 필요하다.
#![cfg(all(feature = "net-it", target_os = "linux"))]

use std::process::Command;

use opl_driver::{DriverError, HostNet, IpRoute, Tap};
use opl_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 테스트가 만든 브리지. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new(vlan_filtering: bool) -> Self {
        // 테스트마다 다른 이름(15자 이하)
        let name = format!("oplt{}", &Uuid::now_v7().simple().to_string()[22..]);
        let filtering = if vlan_filtering { "1" } else { "0" };
        ip(&[
            "link",
            "add",
            &name,
            "type",
            "bridge",
            "vlan_filtering",
            filtering,
        ]);
        ip(&["link", "set", &name, "up"]);
        Self(name)
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn ip(args: &[&str]) -> String {
    let out = Command::new("ip").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "ip {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn master_of(ifname: &str) -> Option<String> {
    let out = Command::new("ip")
        .args(["-j", "link", "show", "dev", ifname])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let links: Value = serde_json::from_slice(&out.stdout).unwrap();
    links[0]["master"].as_str().map(str::to_string)
}

/// `bridge -j vlan show dev <tap>`의 (vid, pvid이면서 untagged인지)
fn vlans(ifname: &str) -> Vec<(u64, bool)> {
    let out = Command::new("bridge")
        .args(["-j", "vlan", "show", "dev", ifname])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["vlans"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let flags: Vec<&str> = e["flags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            (
                e["vlan"].as_u64().unwrap(),
                flags.contains(&"PVID") && flags.contains(&"Egress Untagged"),
            )
        })
        .collect()
}

#[tokio::test]
async fn taps_join_bridges_with_vlans_and_are_removed_per_vm() {
    let aware = TestBridge::new(true);
    let plain = TestBridge::new(false);
    let net = IpRoute;

    let bridges = net.bridges().await.unwrap();
    let find = |name: &str| bridges.iter().find(|b| b.name == name).cloned();
    assert!(find(&aware.0).unwrap().vlan_filtering);
    assert!(!find(&plain.0).unwrap().vlan_filtering);

    let vm = Uuid::now_v7();
    let tagged = Tap {
        ifname: tap_name(vm, 0),
        bridge: aware.0.clone(),
        vlan: Some(10),
    };
    let untagged = Tap {
        ifname: tap_name(vm, 1),
        bridge: plain.0.clone(),
        vlan: None,
    };
    net.create_tap(&tagged).await.unwrap();
    net.create_tap(&untagged).await.unwrap();
    assert_eq!(master_of(&tagged.ifname), Some(aware.0.clone()));
    assert_eq!(master_of(&untagged.ifname), Some(plain.0.clone()));
    // 기본 VLAN 1은 빠지고 10만 태그 없이 주고받는다.
    assert_eq!(vlans(&tagged.ifname), vec![(10, true)]);

    // 이전 인스턴스가 남긴 tap이 있어도 다시 만든다.
    net.create_tap(&tagged).await.unwrap();
    assert_eq!(vlans(&tagged.ifname), vec![(10, true)]);

    // 다른 VM의 tap은 건드리지 않는다.
    let other = Uuid::now_v7();
    let other_tap = Tap {
        ifname: tap_name(other, 0),
        bridge: plain.0.clone(),
        vlan: None,
    };
    net.create_tap(&other_tap).await.unwrap();
    net.delete_vm_taps(vm).await.unwrap();
    assert_eq!(master_of(&tagged.ifname), None);
    assert_eq!(master_of(&untagged.ifname), None);
    assert_eq!(master_of(&other_tap.ifname), Some(plain.0.clone()));
    // 없는 tap을 지워도 성공이다.
    net.delete_vm_taps(vm).await.unwrap();
    net.delete_vm_taps(other).await.unwrap();
    assert_eq!(master_of(&other_tap.ifname), None);
}

#[tokio::test]
async fn missing_or_vlan_unaware_bridges_are_refused() {
    let plain = TestBridge::new(false);
    let net = IpRoute;
    let vm = Uuid::now_v7();

    let missing = Tap {
        ifname: tap_name(vm, 0),
        bridge: "oplt-missing".into(),
        vlan: None,
    };
    let err = net.create_tap(&missing).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    let vlan_on_plain = Tap {
        ifname: tap_name(vm, 0),
        bridge: plain.0.clone(),
        vlan: Some(10),
    };
    let err = net.create_tap(&vlan_on_plain).await.unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    // 거부하면 tap을 남기지 않는다.
    assert_eq!(master_of(&missing.ifname), None);
}
```

`crates/opl-types/src/network.rs`에서

```rust
use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
```

`crates/opl-types/src/network.rs`에서

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

/// VM의 `index`번째 NIC가 쓰는 tap 장치 이름. 노드 안에서 VM마다 겹치지 않고 15자를 넘지 않는다.
/// UUIDv7의 앞부분은 생성 시각이므로 무작위인 뒷부분 10자리를 쓴다.
pub fn tap_name(vm: Uuid, index: usize) -> String {
    format!("{}{index}", tap_prefix(vm))
}

/// 한 VM의 tap 장치 이름이 모두 이것으로 시작한다(`op<10자리>n`).
pub fn tap_prefix(vm: Uuid) -> String {
    let hex = vm.simple().to_string();
    format!("op{}n", &hex[hex.len() - 10..])
}
```

`crates/opl-types/src/network.rs`에서

```rust
    #[test]
    fn vlan_is_optional() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn tap_names_fit_and_share_the_vm_prefix() {
        let vm = Uuid::now_v7();
        let last = tap_name(vm, crate::vm::MAX_NICS - 1);
        assert!(valid_ifname(&last), "{last}");
        assert_eq!(last.len(), 15);
        assert!(tap_name(vm, 0).starts_with(&tap_prefix(vm)));
        assert_ne!(tap_prefix(vm), tap_prefix(Uuid::now_v7()));
        // 같은 VM의 1번과 10번 NIC가 접두사로 겹치지 않는다(`...n1`과 `...n10`은 다른 이름).
        assert_ne!(tap_name(vm, 1), tap_name(vm, 10));
    }

    #[test]
    fn vlan_is_optional() {
```

`crates/opl-types/src/vm.rs`에서

```rust

impl VmSpec {
```

을 다음으로 바꾼다:

```rust

/// VM 하나의 최대 NIC 수. tap 장치 이름(15자)에 순번 두 자리를 넣는다.
pub const MAX_NICS: usize = 16;

impl VmSpec {
```

`crates/opl-types/src/vm.rs`에서

```rust
                return Err(ValidationError::DuplicateBootOrder(order));
            }
        }
        let mut macs = BTreeSet::new();
```

을 다음으로 바꾼다:

```rust
                return Err(ValidationError::DuplicateBootOrder(order));
            }
        }
        if self.nics.len() > MAX_NICS {
            return Err(ValidationError::TooManyNics);
        }
        let mut macs = BTreeSet::new();
```

`crates/opl-types/src/vm.rs`에서

```rust
    DuplicateMac(MacAddr),
}
```

을 다음으로 바꾼다:

```rust
    DuplicateMac(MacAddr),
    #[error("a VM can have at most {MAX_NICS} NICs")]
    TooManyNics,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
        assert_eq!(s.validate(), Err(ValidationError::DuplicateMac(mac)));
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(s.validate(), Err(ValidationError::DuplicateMac(mac)));

        let mut s = sample();
        s.nics = vec![s.nics[0].clone(); MAX_NICS + 1];
        for nic in &mut s.nics {
            nic.mac = None;
        }
        assert_eq!(s.validate(), Err(ValidationError::TooManyNics));
    }
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-types -p opl-driver`
Expected: PASS (`tap_names_fit_and_share_the_vm_prefix`, `plain_tap_joins_the_bridge`, `vlan_tap_replaces_the_default_pvid`, `parses_bridges` 포함)

Run: `cargo test -p opl-driver --features net-it --test host_net`
Expected: Linux 컨테이너(아래 '실행 환경')에서 root로 PASS(2개)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): iproute2로 tap을 만들어 브리지에 잇기(VLAN-aware)"
```

---

### Task 5: QEMU 드라이버: tap 수명, systemd 스코프, QMP 연결 재시도

`QemuConfig`에 `net: Option<Arc<dyn HostNet>>`과 `systemd_scope`를 더한다. QEMU를 띄우기 전에 브리지 NIC마다 tap을 만들고(호스트 네트워크가 없으면 `InvalidSpec`), QEMU가 끝나면(정상 종료·크래시·강제 종료·기동 실패, 그리고 `new()`가 지우는 죽은 VM) tap을 지운다. `systemd_scope`면 `systemd-run --scope --unit=opl-vm-<id>`로 감싼다.

Linux에서 찾은 문제도 고친다: QEMU의 QMP 소켓은 대기열(backlog)이 1이라, 멈췄다 깨어난 QEMU처럼 받지 않은 연결이 쌓여 있으면 Linux의 `connect`가 곧바로 `EAGAIN`으로 실패한다(macOS는 기다린다). `QmpClient::connect`가 50ms 간격으로 최대 100번 다시 시도한다. 이것이 없으면 `start_does_not_destroy_unresponsive_running_vm`이 Linux에서 실패한다.

**Files:**
- Create: `crates/opl-driver/tests/qemu_bridge.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-qemu/src/qmp.rs`
- Modify: `crates/opl-reconciler/tests/qemu_restart.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/qemu.rs` 전체:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{HostProfile, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{NicBackend, VmRuntimeSpec};
use opl_types::vm::Firmware;
use rustix::process::{Flock, FlockOffsetType, FlockType, Pid, Signal, fcntl_getlk, kill_process};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::net::{HostNet, Tap};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 명령 하나의 응답을 기다리는 한도. 멈춘(SIGSTOP 등) QEMU에서 호출자가 묶이지 않게 한다.
const QMP_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGKILL을 보낸 뒤 프로세스가 사라지기를 기다리는 한도
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// SHUTDOWN 없이 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 기다리는 한도.
/// 넘기면 QMP만 끊긴 것으로 본다.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// SHUTDOWN 뒤 연결이 끊겼을 때 런타임 디렉터리를 지우려고 프로세스 종료를 잠깐 기다리는 한도.
/// QEMU는 소켓을 닫고 곧 끝나지만, 플러시가 길면 디렉터리는 다음 launch/new()에 맡기고 종료를 바로 알린다.
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub host: HostProfile,
    /// 소켓과 pidfile을 두는 휘발성 디렉터리 (Linux: `/run/opl`)
    pub run_dir: PathBuf,
    /// UEFI 변수처럼 재부팅 후에도 남아야 하는 VM별 파일 (Linux: `/var/lib/opl`)
    pub state_dir: PathBuf,
    /// 브리지 NIC의 tap을 만들고 지운다. 없으면(macOS) 브리지 NIC를 가진 VM은 시작하지 않는다.
    pub net: Option<Arc<dyn HostNet>>,
    /// QEMU를 `systemd-run --scope`로 감싸 VM마다 cgroup(`opl-vm-<id>.scope`)으로 분리한다.
    /// systemd가 있는 Linux에서만 켠다.
    pub systemd_scope: bool,
}

/// QEMU 프로세스를 `-daemonize`로 띄워 opld와 분리한다. opld가 재시작해도 VM은 계속 돈다.
///
/// QEMU의 생존 여부는 QMP 응답이 아니라 pidfile의 fcntl 잠금으로 판정한다([`liveness`]).
/// 잠금이 걸렸거나 판정할 수 없는 런타임 디렉터리는 절대 지우지 않고, 잠금 소유자로 확인한
/// pid만 죽인다.
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
            match liveness(&paths.pidfile) {
                Liveness::Alive(_) => {}
                Liveness::Dead => {
                    tracing::warn!(%id, "removing stale runtime dir");
                    if let Err(e) = std::fs::remove_dir_all(&paths.dir) {
                        tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                    }
                    delete_taps(hv.config.net.as_deref(), id).await;
                    continue;
                }
                Liveness::Unknown => {
                    tracing::error!(%id, "cannot tell whether QEMU is running; leaving runtime dir");
                    continue;
                }
            }
            // 살아 있는 VM이므로 붙지 못해도 아무것도 지우지 않는다. 이후 start()가 다시 시도한다.
            match tokio::time::timeout(ATTACH_TIMEOUT, hv.attach(id, paths, false)).await {
                Ok(Ok(())) => tracing::info!(%id, "reattached to running VM"),
                Ok(Err(e)) => tracing::error!(%id, error = %e, "could not reattach to running VM"),
                Err(_) => tracing::error!(%id, "timed out reattaching to running VM"),
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

    /// QMP에 연결하고 모니터 태스크를 붙여 등록한다. `launched`면 `Started`를 알린다.
    async fn attach(
        &self,
        id: Uuid,
        paths: RuntimePaths,
        launched: bool,
    ) -> Result<(), DriverError> {
        let qmp = QmpClient::connect(&paths.qmp_socket).await?;
        let pid = read_pid(&paths.pidfile)?;
        let rx = qmp.subscribe();
        // 잠금을 쥔 채로 모니터를 띄우고 등록한다. 모니터가 곧바로 연결 끊김을 보더라도
        // 등록이 끝난 뒤에야 해제할 수 있으므로 죽은 핸들이 남지 않는다.
        // 같은 이유로 Started도 이 인스턴스의 Stopped/Crashed보다 항상 먼저 나간다.
        let mut vms = self.vms.lock().unwrap();
        let monitor = tokio::spawn(monitor(
            id,
            rx,
            Arc::downgrade(&self.vms),
            self.events.clone(),
            paths.clone(),
            self.config.net.clone(),
        ));
        vms.insert(
            id,
            Arc::new(VmHandle {
                qmp,
                pid,
                paths,
                monitor,
            }),
        );
        if launched {
            let _ = self.events.send(HypervisorEvent {
                id,
                kind: EventKind::Started,
            });
        }
        Ok(())
    }

    async fn launch(&self, vm: &VmRuntimeSpec, paths: &RuntimePaths) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            std::fs::copy(&self.config.host.uefi_vars_template, &paths.uefi_vars)?;
        }
        // 호출자가 pidfile 잠금이 풀린 것(= QEMU 없음)을 확인했으므로 남은 소켓은 지워도 된다.
        // 이전 인스턴스가 남긴 pidfile은 새 QEMU가 잠그고 다시 쓴다.
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            remove_if_exists(sock)?;
        }

        let mut args = build_cmdline(vm, &self.config.host, paths)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        self.create_taps(vm).await?;
        let mut cmd = if self.config.systemd_scope {
            // 스코프는 QEMU가 끝날 때까지 남는다. --collect: 실패로 끝나도 남기지 않는다.
            let mut cmd = tokio::process::Command::new("systemd-run");
            cmd.args(["--scope", "--quiet", "--collect"])
                .arg(format!("--unit=opl-vm-{}", vm.id))
                .arg("--")
                .arg(&self.config.host.qemu_binary);
            cmd
        } else {
            tokio::process::Command::new(&self.config.host.qemu_binary)
        };
        cmd.args(&args)
            .stdin(Stdio::null())
            // 타임아웃으로 기다림을 그만두면 부모 프로세스도 끝낸다. 이미 분리된 자식은
            // 실패 정리(kill_and_cleanup)가 pidfile 잠금으로 찾아 끝낸다.
            .kill_on_drop(true);
        if cfg!(target_os = "macos") {
            // macOS에서는 -daemonize의 fork 뒤 자식이 Objective-C 런타임을 초기화하다
            // "fork() was called ... Crashing instead"로 죽는다. 디스플레이/VNC 장치가 있으면
            // 우연히 피하지만, 이 변수를 켜야 장치 구성과 무관하게 안정적으로 뜬다.
            cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
        }
        // -daemonize: 초기화가 끝나면 부모 프로세스가 종료 코드로 성공 여부를 알려 주고, 자식은 세션에서 분리된다.
        let output = tokio::time::timeout(START_TIMEOUT, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout("QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
        }
        tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths.clone(), true))
            .await
            .map_err(|_| DriverError::Timeout("QMP of new QEMU"))??;
        let Some(handle) = self.handle(vm.id) else {
            // 그새 끝났다. 모니터가 종료 이벤트를 보냈다.
            return Ok(());
        };
        let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        if let Err(e) = flatten(cont, "QMP cont") {
            let _ = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await;
            return Err(e);
        }
        Ok(())
    }

    /// 브리지 NIC마다 tap을 만들어 브리지에 붙인다. 하나라도 실패하면 만든 것을 지운다.
    async fn create_taps(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        let taps: Vec<Tap> = vm
            .nics
            .iter()
            .filter_map(|nic| match &nic.backend {
                NicBackend::Tap {
                    ifname,
                    bridge,
                    vlan,
                } => Some(Tap {
                    ifname: ifname.clone(),
                    bridge: bridge.clone(),
                    vlan: *vlan,
                }),
                NicBackend::User => None,
            })
            .collect();
        if taps.is_empty() {
            return Ok(());
        }
        let Some(net) = &self.config.net else {
            return Err(DriverError::InvalidSpec(
                "bridge networks are not supported on this node".into(),
            ));
        };
        for tap in &taps {
            if let Err(e) = net.create_tap(tap).await {
                delete_taps(Some(net.as_ref()), vm.id).await;
                return Err(e);
            }
        }
        Ok(())
    }

    /// 등록되지 않은 QEMU(기동 실패로 남은 데몬, 붙지 못한 VM)를 SIGKILL로 끝내고 런타임
    /// 디렉터리를 지운다. 끝냈음을 확인하지 못하면 살아 있는 VM의 파일이므로 아무것도 지우지
    /// 않고 false를 돌려준다.
    async fn kill_and_cleanup(&self, id: Uuid, paths: &RuntimePaths) -> bool {
        if liveness(&paths.pidfile) != Liveness::Dead {
            let pid = match read_pid(&paths.pidfile) {
                Ok(pid) => pid,
                Err(e) => {
                    tracing::error!(%id, error = %e, "QEMU has no readable pid; keeping runtime dir");
                    return false;
                }
            };
            kill_if_owner(&paths.pidfile, pid);
            if !wait_until(KILL_TIMEOUT, || liveness(&paths.pidfile) == Liveness::Dead).await {
                tracing::error!(%id, pid, "QEMU did not exit; keeping runtime dir");
                return false;
            }
        }
        if let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
        delete_taps(self.config.net.as_deref(), id).await;
        true
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 이어서 실행한다.
    async fn resume_if_prelaunch(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? == RunState::Prelaunch {
            tracing::info!(%id, "continuing VM left in prelaunch");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        Ok(())
    }

    async fn query_state(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let Some(handle) = self.handle(id) else {
            // 살아 있지만 QMP에 붙지 못한 VM도 드러내야 reconciler가 놓치지 않는다.
            return match liveness(&self.paths(id)?.pidfile) {
                Liveness::Dead => Ok(None),
                Liveness::Alive(pid) => Ok(Some(ActualState {
                    id,
                    pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                Liveness::Unknown => Err(DriverError::Io(std::io::Error::other(format!(
                    "cannot tell whether QEMU of VM {id} is running"
                )))),
            };
        };
        let status = match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await {
            Err(_) => return Err(DriverError::Timeout("QMP query-status")),
            Ok(Ok(RunState::Running)) => RuntimeStatus::Running,
            Ok(Ok(RunState::Prelaunch | RunState::Paused)) => RuntimeStatus::Paused,
            Ok(Ok(RunState::Shutdown)) => RuntimeStatus::Other("shutdown".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        if self.handle(vm.id).is_some() {
            return self.resume_if_prelaunch(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    return self.resume_if_prelaunch(vm.id).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(id = %vm.id, error = %e, "could not reattach to running VM")
                }
                Err(_) => tracing::error!(id = %vm.id, "timed out reattaching to running VM"),
            }
            return Err(DriverError::Timeout("reattach to running QEMU"));
        }
        if let Err(e) = self.launch(vm, &paths).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            }
            return Err(e);
        }
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            let paths = self.paths(id)?;
            if liveness(&paths.pidfile) == Liveness::Dead {
                return Ok(());
            }
            // 살아 있지만 QMP로 다룰 수 없는 VM
            return match mode {
                StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                StopMode::Force if self.kill_and_cleanup(id, &paths).await => Ok(()),
                StopMode::Force => Err(DriverError::Timeout("QEMU to exit")),
            };
        };
        match mode {
            StopMode::Acpi => {
                let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.system_powerdown()).await;
                flatten(r, "QMP system_powerdown")?;
            }
            StopMode::Force => {
                let pid = handle.pid;
                let pidfile = handle.paths.pidfile.clone();
                match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(%id, error = %e, "QMP quit failed"),
                    Err(_) => tracing::warn!(%id, "QMP quit timed out"),
                }
                drop(handle);
                // 등록이 풀렸어도 QMP만 끊기고 프로세스는 살아 있을 수 있으므로 pidfile도 확인한다.
                let gone = || self.handle(id).is_none() && liveness(&pidfile) == Liveness::Dead;
                if !wait_until(FORCE_STOP_TIMEOUT, gone).await {
                    tracing::warn!(%id, pid, "QEMU did not quit; sending SIGKILL");
                    kill_if_owner(&pidfile, pid);
                    if !wait_until(KILL_TIMEOUT, gone).await {
                        return Err(DriverError::Timeout("QEMU to exit"));
                    }
                }
            }
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        match self.query_state(id).await {
            // 멈춘 QEMU도 list()처럼 살아 있는 VM으로 드러내야 reconciler가 강제 종료로 넘어갈 수 있다.
            Err(DriverError::Timeout(what)) => match self.handle(id) {
                Some(handle) => Ok(Some(ActualState {
                    id,
                    pid: handle.pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                None => Err(DriverError::Timeout(what)),
            },
            other => other,
        }
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let mut ids: Vec<Uuid> = self.vms.lock().unwrap().keys().copied().collect();
        // 등록되지 않았지만 살아 있는 VM도 포함하도록 런타임 디렉터리를 함께 훑는다.
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            if let Some(id) = entry?
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.query_state(id).await {
                Ok(Some(st)) => out.push(st),
                Ok(None) => {}
                Err(DriverError::Io(e)) if self.handle(id).is_none() => {
                    tracing::error!(%id, error = %e, "skipping VM whose state is unknown");
                }
                // VM 하나가 응답하지 않는다고 전체 조회가 실패하면 안 된다.
                Err(DriverError::Timeout(_)) => {
                    if let Some(handle) = self.handle(id) {
                        out.push(ActualState {
                            id,
                            pid: handle.pid,
                            status: RuntimeStatus::Other("unresponsive".into()),
                        });
                    }
                }
                Err(e) => return Err(e),
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
    paths: RuntimePaths,
    net: Option<Arc<dyn HostNet>>,
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
    let unregister = || {
        if let Some(vms) = vms.upgrade() {
            let handle = vms.lock().unwrap().remove(&id);
            drop(handle);
        }
    };
    let dead = || liveness(&paths.pidfile) == Liveness::Dead;
    let kind = match guest_initiated {
        // SHUTDOWN을 받았으면 종료는 확정이다. QEMU는 소켓을 닫고 곧 끝나므로 잠깐 기다려
        // 디렉터리를 지우되, 플러시 등으로 늦어지면 잠금이 걸린 디렉터리는 남겨 두고
        // (다음 launch/new()가 잠금 해제를 확인하고 정리한다) 종료는 바로 알린다.
        Some(guest_initiated) => {
            if !wait_until(SHUTDOWN_EXIT_WAIT, dead).await {
                tracing::info!(%id, "QEMU is still exiting; leaving runtime dir");
            } else if let Err(e) = std::fs::remove_dir_all(&paths.dir) {
                tracing::warn!(%id, error = %e, "failed to remove runtime dir");
            }
            EventKind::Stopped { guest_initiated }
        }
        None => {
            if !wait_until(EXIT_GRACE, dead).await {
                // SHUTDOWN 없이 QMP만 끊기고 QEMU는 살아 있다. 파일을 지우거나 종료로 알리지
                // 않고 등록만 푼다. inspect/list에 "unresponsive"로 드러나고, 이후 start()가
                // 다시 붙거나 stop(Force)가 끝낸다.
                tracing::error!(%id, "lost QMP connection to running QEMU");
                unregister();
                return;
            }
            if let Err(e) = std::fs::remove_dir_all(&paths.dir) {
                tracing::warn!(%id, error = %e, "failed to remove runtime dir");
            }
            EventKind::Crashed
        }
    };
    // QEMU가 끝났으므로 tap을 지운다(아직 끝나는 중이어도 마지막 fd가 닫히면 사라진다).
    delete_taps(net.as_deref(), id).await;
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// VM의 tap을 지운다. 실패는 기록만 한다(다음 시작이 같은 이름을 다시 만든다).
async fn delete_taps(net: Option<&dyn HostNet>, id: Uuid) {
    if let Some(net) = net
        && let Err(e) = net.delete_vm_taps(id).await
    {
        tracing::warn!(%id, error = %e, "failed to delete tap devices");
    }
}

/// pidfile로 본 QEMU의 생존 여부
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// 이 pid가 pidfile 잠금을 쥐고 있다.
    Alive(u32),
    Dead,
    /// 판정할 수 없다. 살아 있을 수 있으므로 파일을 지우거나 프로세스를 죽이지 않는다.
    Unknown,
}

/// QEMU는 수명 내내 `-pidfile`에 POSIX 잠금(lockf)을 쥐고 있다(SIGSTOP 상태에서도).
/// `F_GETLK`로 잠금을 잡지 않고 소유자를 조회한다. 잠금이 없으면 QEMU는 없다.
/// 같은 프로세스가 쥔 잠금은 보이지 않지만, opld는 pidfile을 잠그지 않는다.
fn liveness(pidfile: &Path) -> Liveness {
    let file = match std::fs::File::open(pidfile) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Liveness::Dead,
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot open pidfile");
            return Liveness::Unknown;
        }
    };
    // 파일 전체(길이 0 = 끝까지)에 대한 쓰기 잠금을 막는 잠금이 있는지 묻는다.
    let probe = Flock {
        start: 0,
        length: 0,
        pid: None,
        typ: FlockType::WriteLock,
        offset_type: FlockOffsetType::Set,
    };
    match fcntl_getlk(&file, &probe) {
        Ok(None) => Liveness::Dead,
        Ok(Some(lock)) => match lock.pid {
            Some(pid) => Liveness::Alive(pid.as_raw_nonzero().get() as u32),
            None => {
                tracing::warn!(?pidfile, "pidfile lock has no owner pid");
                Liveness::Unknown
            }
        },
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot query pidfile lock");
            Liveness::Unknown
        }
    }
}

/// `pid`가 지금 pidfile 잠금의 소유자일 때만 SIGKILL을 보낸다. pid 재사용이나 잘못된 pidfile로
/// 엉뚱한 프로세스를 죽이지 않기 위해서다.
fn kill_if_owner(pidfile: &Path, pid: u32) {
    match liveness(pidfile) {
        Liveness::Alive(owner) if owner == pid => {}
        Liveness::Dead => return,
        other => {
            tracing::error!(
                ?pidfile,
                pid,
                ?other,
                "pid does not own the pidfile lock; not killing"
            );
            return;
        }
    }
    let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    match kill_process(target, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => tracing::warn!(pid, error = %e, "failed to kill QEMU"),
    }
}

/// `cond`가 참이 될 때까지 짧게 쉬며 확인한다. 한도 안에 참이 되면 true.
async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(limit, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// QMP_TIMEOUT으로 감싼 명령의 결과를 풀어 낸다.
fn flatten<T>(
    r: Result<Result<T, QmpError>, tokio::time::error::Elapsed>,
    what: &'static str,
) -> Result<T, DriverError> {
    Ok(r.map_err(|_| DriverError::Timeout(what))??)
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use opl_qemu::{Accel, Arch};
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{CpuModel, CpuSpec, MachineType, NicModel};

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
            net: None,
            systemd_scope: false,
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
        // pidfile이 없다.
        let stale = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("qmp.sock"), b"not a socket").unwrap();
        // pidfile은 있지만 아무도 잠그고 있지 않다(kill -9 뒤의 모습).
        let unlocked = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&unlocked).unwrap();
        std::fs::write(unlocked.join("qemu.pid"), b"4242\n").unwrap();
        let foreign = cfg.run_dir.join("vm").join("not-a-uuid");
        std::fs::create_dir_all(&foreign).unwrap();

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!stale.exists());
        assert!(!unlocked.exists());
        assert!(foreign.exists());
        assert!(hv.list().await.unwrap().is_empty());
    }

    /// 살아 있는 QEMU처럼 `dir/qemu.pid`에 fcntl 잠금을 걸고 자기 pid를 써 두는 자식 프로세스.
    /// fcntl 잠금은 프로세스 단위라 테스트 프로세스가 직접 잠그면 드라이버가 잠금을 볼 수 없다.
    /// 반환값을 drop하면 자식이 죽어 잠금이 풀린다.
    async fn hold_pidfile(dir: &Path) -> tokio::process::Child {
        use tokio::io::{AsyncBufReadExt, BufReader};

        const SCRIPT: &str = "import fcntl, os, sys, time
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o644)
fcntl.lockf(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, b'%d\\n' % os.getpid())
print('locked', flush=True)
time.sleep(60)
";
        std::fs::create_dir_all(dir).unwrap();
        let mut child = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(SCRIPT)
            .arg(dir.join("qemu.pid"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line.trim(), "locked");
        child
    }

    struct FakeQmp {
        /// 클라이언트가 연결을 닫으면 완료된다.
        closed: tokio::sync::oneshot::Receiver<()>,
        /// 받은 명령 이름
        commands: Arc<Mutex<Vec<String>>>,
        /// 보내면 서버가 (주어진 이벤트를 먼저 보낸 뒤) 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            loop {
                let line = tokio::select! {
                    ev = &mut hangup_rx => {
                        if let Ok(Some(ev)) = ev {
                            let _ = write.write_all(format!("{ev}\n").as_bytes()).await;
                        }
                        break;
                    }
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        _ => break,
                    },
                };
                let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                let name = cmd["execute"].as_str().unwrap_or_default().to_string();
                let ret = match name.as_str() {
                    "query-status" => {
                        serde_json::json!({ "running": status == "running", "status": status })
                    }
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                let reply = serde_json::json!({ "return": ret, "id": cmd["id"] });
                if write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = closed_tx.send(());
        });
        FakeQmp {
            closed,
            commands,
            hangup,
        }
    }

    #[tokio::test]
    async fn dropping_the_driver_releases_qmp_connections() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(hv.list().await.unwrap().len(), 1);
        drop(hv);

        // QEMU는 QMP 클라이언트를 하나만 받으므로, 연결이 남아 있으면 재시작한 opld가 붙을 수 없다.
        tokio::time::timeout(Duration::from_secs(2), fake.closed)
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
        let id = Uuid::now_v7();
        let dir = cfg.run_dir.join("vm").join(id.to_string());
        // pidfile이 잠겨 있으니 QEMU는 살아 있다. 소켓은 QMP가 아니어서 붙을 수 없지만 지우면 안 된다.
        let _qemu = hold_pidfile(&dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.handle(id).is_none());
        // 등록하지 않지만 살아 있는 VM으로 드러난다.
        let listed = hv.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].status,
            RuntimeStatus::Other("unresponsive".into())
        );
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn start_continues_vm_left_in_prelaunch() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // opld가 -daemonize와 cont 사이에 죽어 -S 상태로 남은 VM
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "prelaunch").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        hv.start(&vm).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn start_does_not_touch_live_vm_it_cannot_attach_to() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는다.
        let qemu = unresponsive_vm(&dir).await;

        let err = hv.start(&vm).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(dir.join("qmp.sock").exists());
        // 등록되지 않았어도 살아 있는 VM은 조회에 드러나야 reconciler가 놓치지 않는다.
        let expected = ActualState {
            id: vm.id,
            pid: qemu.id().unwrap(),
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(expected.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![expected]);
    }

    /// QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는 런타임 디렉터리를 만든다.
    async fn unresponsive_vm(dir: &Path) -> tokio::process::Child {
        let qemu = hold_pidfile(dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let _conn = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        qemu
    }

    #[tokio::test]
    async fn liveness_reports_lock_owner() {
        let tmp = short_tempdir();
        let dir = tmp.path().join("vm");
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
        let mut qemu = hold_pidfile(&dir).await;
        assert_eq!(
            liveness(&dir.join("qemu.pid")),
            Liveness::Alive(qemu.id().unwrap())
        );
        qemu.kill().await.unwrap();
        // kill -9 뒤에도 pidfile은 남지만 잠금은 풀린다.
        assert!(dir.join("qemu.pid").exists());
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
    }

    #[tokio::test]
    async fn force_stop_kills_live_unregistered_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let mut qemu = unresponsive_vm(&dir).await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        // ACPI 요청은 QMP 없이 보낼 수 없다. 다시 시도할 수 있는 오류다.
        let err = hv.stop(vm.id, StopMode::Acpi).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.exists());

        hv.stop(vm.id, StopMode::Force).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(2), qemu.wait())
            .await
            .expect("QEMU was not killed")
            .unwrap();
        assert!(!status.success());
        assert!(!dir.exists());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn force_stop_never_kills_a_pid_that_does_not_own_the_lock() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        // pidfile에 적힌 pid가 잠금 소유자와 다르다(pid 재사용, 다른 프로세스가 덮어씀 등).
        let mut bystander = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let bystander_pid = bystander.id().unwrap();
        std::fs::write(dir.join("qemu.pid"), format!("{bystander_pid}\n")).unwrap();
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let err = hv.stop(vm.id, StopMode::Force).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "bystander was killed"
        );
        assert!(dir.join("qemu.pid").exists());
    }

    #[tokio::test]
    async fn guest_shutdown_is_reported_even_if_qemu_exits_slowly() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU가 SHUTDOWN을 보내고 QMP를 닫았지만 아직 플러시 중이라 잠금을 쥐고 있다.
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        let shutdown = serde_json::json!({
            "event": "SHUTDOWN",
            "data": { "guest": true, "reason": "guest-shutdown" },
            "timestamp": { "seconds": 0, "microseconds": 0 }
        });
        fake.hangup.send(Some(shutdown)).unwrap();
        let ev = tokio::time::timeout(EXIT_GRACE - Duration::from_secs(2), rx.recv())
            .await
            .expect("Stopped was not reported promptly")
            .unwrap();
        assert_eq!(
            ev,
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.handle(vm.id).is_none());
        // 잠금이 남아 있으니 파일은 지우지 않는다. 다음 launch/new()가 정리한다.
        assert!(dir.join("qemu.pid").exists());
    }

    #[tokio::test]
    async fn losing_qmp_while_qemu_lives_unregisters_without_cleanup() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        fake.hangup.send(None).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hv.handle(vm.id).is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("VM was not unregistered");
        // 살아 있는 VM이므로 파일을 지우거나 종료로 알리면 안 된다. 이후 start()가 다시 붙는다.
        assert!(dir.join("qemu.pid").exists());
        assert!(rx.try_recv().is_err());
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
    async fn bridge_nic_without_host_net_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "op0123456789n0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        let err = hv.start(&vm).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
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

`crates/opl-driver/tests/qemu_bridge.rs`를 만든다:

```rust
//! 실제 QEMU의 NIC를 tap으로 브리지에 잇는 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it,net-it --test qemu_bridge`
//! Linux에서 root, iproute2, QEMU가 필요하다.
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use opl_driver::{EventKind, Hypervisor, IpRoute, QemuConfig, QemuHypervisor, StopMode};
use opl_qemu::{Arch, HostProfile};
use opl_types::network::tap_name;
use opl_types::runtime::{NicBackend, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, Firmware, NicModel};
use uuid::Uuid;

struct TestBridge(String);

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn bridge() -> TestBridge {
    let name = format!("oplq{}", &Uuid::now_v7().simple().to_string()[22..]);
    for args in [
        vec![
            "link",
            "add",
            &name,
            "type",
            "bridge",
            "vlan_filtering",
            "1",
        ],
        vec!["link", "set", &name, "up"],
    ] {
        assert!(Command::new("ip").args(&args).status().unwrap().success());
    }
    TestBridge(name)
}

fn master_of(ifname: &str) -> Option<String> {
    let out = Command::new("ip")
        .args(["-j", "link", "show", "dev", ifname])
        .output()
        .unwrap();
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    v[0]["master"].as_str().map(str::to_string)
}

fn config(root: &Path) -> QemuConfig {
    QemuConfig {
        host: HostProfile::native(Path::new("/opt/homebrew/share/qemu")),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: Some(Arc::new(IpRoute)),
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
}

fn spec(bridge: &str) -> VmRuntimeSpec {
    let id = Uuid::now_v7();
    VmRuntimeSpec {
        id,
        name: "it-bridge".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: tap_name(id, 0),
                bridge: bridge.into(),
                vlan: Some(20),
            },
        }],
    }
}

async fn wait_gone(ifname: &str) {
    for _ in 0..100 {
        if master_of(ifname).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("tap {ifname} is still there");
}

#[tokio::test]
async fn vm_nic_is_on_the_bridge_while_qemu_runs() {
    let br = bridge();
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec(&br.0);
    let tap = tap_name(vm.id, 0);

    hv.start(&vm).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    wait_gone(&tap).await;

    // QEMU가 죽어도 tap을 지운다.
    let mut rx = hv.subscribe();
    hv.start(&vm).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    assert!(
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no event")
            .unwrap();
        if ev.id == vm.id && ev.kind == EventKind::Crashed {
            break;
        }
    }
    wait_gone(&tap).await;
}

#[tokio::test]
async fn missing_bridge_fails_the_start_without_leftovers() {
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec("oplq-missing");
    let err = hv.start(&vm).await.unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert_eq!(master_of(&tap_name(vm.id, 0)), None);
}
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
        state_dir: root.join("state"),
    }
```

을 다음으로 바꾼다:

```rust
        state_dir: root.join("state"),
        net: None,
        // systemd가 있는 Linux(CI)에서 `OPL_TEST_SYSTEMD=1`로 스코프 실행까지 확인한다.
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
use std::sync::{Arc, Mutex};
```

을 다음으로 바꾼다:

```rust
use std::sync::{Arc, Mutex};
use std::time::Duration;
```

`crates/opl-qemu/src/qmp.rs`에서

```rust

impl QmpClient {
    /// 소켓에 연결하고 greeting을 확인한 뒤 `qmp_capabilities`로 명령 모드에 진입한다.
    pub async fn connect(path: &Path) -> Result<Self, QmpError> {
        let stream = UnixStream::connect(path).await?;
        let (read, write) = stream.into_split();
```

을 다음으로 바꾼다:

```rust

/// QEMU의 QMP 소켓은 대기열(backlog)이 1이다. Linux에서는 대기열이 차 있으면(앞선 클라이언트가
/// 아직 받아들여지지 않았으면, 예: 멈췄다 깨어난 QEMU) 연결이 곧바로 `EAGAIN`으로 실패하므로
/// 잠시 뒤 다시 시도한다. macOS는 이때 기다려 준다.
async fn connect_socket(path: &Path) -> std::io::Result<UnixStream> {
    const RETRY: Duration = Duration::from_millis(50);
    const ATTEMPTS: u32 = 100;
    let mut attempt = 1;
    loop {
        match UnixStream::connect(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && attempt < ATTEMPTS => {
                attempt += 1;
                tokio::time::sleep(RETRY).await;
            }
            other => return other,
        }
    }
}

impl QmpClient {
    /// 소켓에 연결하고 greeting을 확인한 뒤 `qmp_capabilities`로 명령 모드에 진입한다.
    pub async fn connect(path: &Path) -> Result<Self, QmpError> {
        let stream = connect_socket(path).await?;
        let (read, write) = stream.into_split();
```

`crates/opl-reconciler/tests/qemu_restart.rs`에서

```rust
        state_dir: root.join("state"),
    }
```

을 다음으로 바꾼다:

```rust
        state_dir: root.join("state"),
        net: None,
        // systemd가 있는 Linux(CI)에서 `OPL_TEST_SYSTEMD=1`로 스코프 실행까지 확인한다.
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
```

`crates/opld/src/member.rs`에서

```rust
            state_dir: config.data_dir.clone(),
        })
```

을 다음으로 바꾼다:

```rust
            state_dir: config.data_dir.clone(),
            net: None,
            systemd_scope: false,
        })
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-qemu -p opl-driver`
Expected: PASS (`bridge_nic_without_host_net_is_invalid` 포함)

Run: `cargo test -p opl-driver --features qemu-it`
Expected: macOS에서 PASS

Run: `cargo test -p opl-driver --features qemu-it,net-it`
Expected: Linux 컨테이너(아래 '실행 환경')에서 root로 PASS(`qemu_bridge` 2개, `qemu_driver` 6개, `host_net` 2개)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): VM NIC의 tap을 QEMU 앞뒤로 만들고 지우기, systemd 스코프, QMP 연결 EAGAIN 재시도"
```

---

### Task 6: reconciler: 브리지 네트워크를 tap으로 풀기

`runtime_spec`이 `user`가 아닌 NIC의 네트워크를 저장소에서 찾아 `NicBackend::Tap { ifname: tap_name(vm, i), bridge, vlan }`으로 만든다. 예약 이름 상수는 `opl_types::network::USER_NETWORK`로 옮긴다(API도 그것을 쓴다).

**Files:**
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/vms.rs`에서

```rust
use opl_cluster::types::Rejection;
use opl_reconciler::resolve::USER_NETWORK;
use opl_types::resource::{Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::Rejection;
use opl_types::network::USER_NETWORK;
use opl_types::resource::{Resource, ResourceSpec};
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use opl_driver::DirStorage;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_driver::DirStorage;
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use uuid::Uuid;

/// `Network` 리소스가 생기기 전까지 쓸 수 있는 유일한 네트워크 이름. QEMU user-mode NAT다.
pub const USER_NETWORK: &str = "user";
```

을 다음으로 바꾼다:

```rust
use uuid::Uuid;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    PoolNotOnNode(String),
    #[error("network {0:?} not found (only {USER_NETWORK:?} is supported for now)")]
    NetworkNotFound(String),
```

을 다음으로 바꾼다:

```rust
    PoolNotOnNode(String),
    #[error("network {0:?} not found")]
    NetworkNotFound(String),
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    for (i, nic) in vm.spec.nics.iter().enumerate() {
        if nic.network != USER_NETWORK {
            return Err(ResolveError::NetworkNotFound(nic.network.clone()));
        }
        nics.push(RuntimeNic {
            mac: nic.mac.ok_or(ResolveError::MissingMac(i))?,
            model: nic.model,
            backend: NicBackend::User,
        });
```

을 다음으로 바꾼다:

```rust
    for (i, nic) in vm.spec.nics.iter().enumerate() {
        let backend = if nic.network == USER_NETWORK {
            NicBackend::User
        } else {
            let network = cluster
                .get_by_name::<NetworkSpec>(&nic.network)?
                .ok_or_else(|| ResolveError::NetworkNotFound(nic.network.clone()))?;
            NicBackend::Tap {
                ifname: tap_name(vm.id, i),
                bridge: network.spec.bridge,
                vlan: network.spec.vlan,
            }
        };
        nics.push(RuntimeNic {
            mac: nic.mac.ok_or(ResolveError::MissingMac(i))?,
            model: nic.model,
            backend,
        });
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::network::{Network, NetworkSpec, tap_name};
use opl_types::resource::Resource;
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
#[tokio::test]
async fn reports_unresolvable_references() {
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn resolves_bridge_networks_to_taps() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let lan: Network = Resource::new(
        "lan",
        NetworkSpec {
            bridge: "vmbr0".into(),
            vlan: Some(10),
        },
    );
    cluster.create(&lan).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.nics.push(nic("user", Some(MacAddr::generate())));
    vm.spec.nics.push(nic("lan", Some(MacAddr::generate())));
    let spec = runtime_spec(&cluster, &vm, node).unwrap();
    assert_eq!(spec.nics[0].backend, NicBackend::User);
    // tap 이름은 NIC 순번으로 정한다.
    assert_eq!(
        spec.nics[1].backend,
        NicBackend::Tap {
            ifname: tap_name(vm.id, 1),
            bridge: "vmbr0".into(),
            vlan: Some(10),
        }
    );
}

#[tokio::test]
async fn reports_unresolvable_references() {
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-reconciler --test resolve && cargo build --workspace --all-targets`
Expected: PASS (`resolves_bridge_networks_to_taps` 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(reconciler): 브리지 네트워크를 tap 백엔드로 풀기"
```

---

### Task 7: API: 네트워크와 브리지를 보는 스케줄러

`/networks`(목록·조회는 모든 역할, 생성·삭제는 관리자)를 더한다. 이름 `user`는 예약, 브리지 이름과 VLAN을 검증하고, VM이 쓰는 네트워크는 지울 수 없다(409 `network_in_use`). VM의 NIC는 있는 네트워크만 쓸 수 있다. 스케줄러는 VM의 네트워크가 쓰는 브리지가 모두 있는(VLAN이면 VLAN-aware인) 노드만 고른다. watch에 `network`를 더한다.

**Files:**
- Create: `crates/opl-api/tests/networks.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/src/watch.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_tasks))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_networks, r::create_network))
        .routes(routes!(r::get_network, r::delete_network))
        .routes(routes!(r::list_tasks))
```

`crates/opl-api/src/resources.rs`에서

```rust
//! 종류에 상관없는 조회 핸들러와 공통 요청 형식, 풀과 노드 핸들러.
```

을 다음으로 바꾼다:

```rust
//! 종류에 상관없는 조회 핸들러와 공통 요청 형식, 풀·네트워크·노드 핸들러.
```

`crates/opl-api/src/resources.rs`에서

```rust
use opl_cluster::{Cluster, Typed};
use opl_types::node::{Node, NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::{Cluster, Typed};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
use opl_types::node::{Node, NodeSpec, NodeStatus};
```

`crates/opl-api/src/resources.rs`에서

```rust
list_and_get!(
    list_tasks,
```

을 다음으로 바꾼다:

```rust
list_and_get!(
    list_networks,
    get_network,
    NetworkSpec,
    NetworkStatus,
    "/networks",
    "/networks/{key}",
    "networks"
);
list_and_get!(
    list_tasks,
```

`crates/opl-api/src/resources.rs`에서

```rust

/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
```

을 다음으로 바꾼다:

```rust

/// 노드에 미리 만든 Linux 브리지를 가리키는 네트워크. 이름 `user`는 예약되어 있다.
#[utoipa::path(post, path = "/networks", tag = "networks",
    request_body = CreateRequest<NetworkSpec>,
    responses((status = 201, body = Resource<NetworkSpec, NetworkStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_network(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<NetworkSpec>>,
) -> Result<(StatusCode, Json<Network>), ApiError> {
    state.authorize(&principal, Action::Write, NetworkSpec::KIND)?;
    check_name(&req.name)?;
    if req.name == USER_NETWORK {
        return Err(ApiError::bad_request(format!(
            "network name {USER_NETWORK:?} is reserved for user-mode NAT"
        )));
    }
    req.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let mut network = Resource::new(req.name, req.spec);
    network.labels = req.labels;
    network.revision = state.cluster.create(&network).await?;
    Ok((StatusCode::CREATED, Json(network)))
}

/// VM이 쓰고 있으면 409
#[utoipa::path(delete, path = "/networks/{key}", tag = "networks",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_network(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NetworkSpec::KIND)?;
    let network = lookup::<NetworkSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.nics.iter().any(|n| n.network == network.name))
    {
        return Err(ApiError::conflict(
            "network_in_use",
            format!("network {:?} is used by VM {:?}", network.name, vm.name),
        ));
    }
    state
        .cluster
        .delete::<NetworkSpec>(network.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
```

`crates/opl-api/src/scheduler.rs` 전체:

```rust
//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, can reach its storage pools and has the bridges of its networks)"
)]
pub struct NoNode;

/// 스케줄러가 보는 클러스터 상태
#[derive(Debug, Default)]
pub struct Inventory {
    pub nodes: Vec<Node>,
    pub vms: Vec<Vm>,
    pub volumes: Vec<Volume>,
    pub pools: Vec<StoragePool>,
    pub networks: Vec<Network>,
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        Ok(Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
            networks: cluster.list::<NetworkSpec>()?.1,
        })
    }

    /// 이 VM의 브리지 네트워크가 쓰는 브리지가 모두 이 노드에 있는지. VLAN을 쓰면 VLAN-aware여야 한다.
    fn has_bridges(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.nics
            .iter()
            .filter(|nic| nic.network != USER_NETWORK)
            .all(|nic| {
                let Some(network) = self.networks.iter().find(|n| n.name == nic.network) else {
                    return false;
                };
                node.status.bridges.iter().any(|b| {
                    b.name == network.spec.bridge
                        && (network.spec.vlan.is_none() || b.vlan_filtering)
                })
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

/// 온라인이고 유지보수 중이 아니며 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드 가운데 미할당 메모리가 가장
/// 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은 범위 밖).
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
    inv.nodes
        .iter()
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| inv.reaches_disks(spec, n.id))
        .filter(|n| inv.has_bridges(spec, n))
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
        .ok_or(NoNode)
}

#[cfg(test)]
mod tests {
    use opl_types::node::{BridgeInfo, NodeStatus};
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec};

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
            bridges: vec![],
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
            networks: vec![],
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

    #[test]
    fn needs_the_bridges_of_its_networks() {
        let bridge = |name: &str, vlan_filtering| BridgeInfo {
            name: name.into(),
            vlan_filtering,
        };
        let mut none = node(64000);
        none.status.bridges = vec![bridge("vmbr1", true)];
        let mut plain = node(32000);
        plain.status.bridges = vec![bridge("vmbr0", false)];
        let mut aware = node(1024);
        aware.status.bridges = vec![bridge("vmbr0", true)];
        let network = |name: &str, vlan| -> Network {
            Resource::new(
                name,
                NetworkSpec {
                    bridge: "vmbr0".into(),
                    vlan,
                },
            )
        };
        let inv = Inventory {
            nodes: vec![none, plain.clone(), aware.clone()],
            networks: vec![network("lan", None), network("dmz", Some(20))],
            ..Default::default()
        };
        let nic = |network: &str| NicSpec {
            network: network.into(),
            model: NicModel::Virtio,
            mac: None,
        };
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![nic("user"), nic("lan")];
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(plain.id));
        // VLAN을 쓰려면 VLAN-aware 브리지가 있어야 한다.
        me.nics.push(nic("dmz"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(aware.id));
        // 없는 네트워크는 어디서도 돌 수 없다.
        me.nics.push(nic("gone"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }
}
```

`crates/opl-api/src/vms.rs`에서

```rust

    if let Some(nic) = spec.nics.iter().find(|n| n.network != USER_NETWORK) {
        return Err(ApiError::bad_request(format!(
            "network {:?} not found (only {USER_NETWORK:?} is supported for now)",
            nic.network
        )));
    }
```

을 다음으로 바꾼다:

```rust

    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if !inv.networks.iter().any(|n| n.name == nic.network) {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 5] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 6] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::Task,
];
```

을 다음으로 바꾼다:

```rust
    Kind::Task,
    Kind::Network,
];
```

`crates/opl-api/tests/networks.rs`를 만든다:

```rust
//! 네트워크 API: 관리자만 만들고 지우며, VM의 NIC가 쓰고, 브리지가 있는 노드에만 배치된다.

mod common;

use common::{Env, vm_request};
use opl_types::node::{BridgeInfo, NodeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn network(name: &str, bridge: &str, vlan: Option<u16>) -> Value {
    json!({ "name": name, "spec": { "bridge": bridge, "vlan": vlan } })
}

#[tokio::test]
async fn admins_manage_networks_and_others_read() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let operator = env.user("operator", "operator").await;
    let viewer = env.user("viewer", "viewer").await;

    let (status, body) = admin
        .post("/networks", network("lan", "vmbr0", Some(10)))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["vlan"], 10);
    assert_eq!(
        operator
            .post("/networks", network("dmz", "vmbr0", None))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, list) = viewer.get("/networks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["items"][0]["name"], "lan");
    assert_eq!(viewer.get("/networks/lan").await.0, StatusCode::OK);

    for (bad, why) in [
        (network("user", "vmbr0", None), "reserved name"),
        (network("bad-bridge", "a/b", None), "bridge name"),
        (network("bad-vlan", "vmbr0", Some(4095)), "VLAN tag"),
    ] {
        let (status, body) = admin.post("/networks", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, body) = admin.post("/networks", network("lan", "vmbr1", None)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "name_taken");
}

#[tokio::test]
async fn vms_use_existing_networks_which_cannot_be_deleted_while_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut vm = vm_request("web");
    vm["spec"]["nics"][0]["network"] = json!("lan");
    let (status, body) = admin.post("/vms", vm.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("lan"), "{body}");

    admin.post("/networks", network("lan", "vmbr0", None)).await;
    assert_eq!(admin.post("/vms", vm).await.0, StatusCode::CREATED);
    let (status, body) = admin.delete("/networks/lan").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "network_in_use");

    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    assert_eq!(
        admin.delete("/networks/lan").await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(admin.get("/networks/lan").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn vms_are_placed_on_nodes_with_the_bridge() {
    let env = Env::start().await;
    let admin = env.admin().await;
    admin
        .post("/networks", network("dmz", "vmbr0", Some(20)))
        .await;
    let mut vm = vm_request("web");
    vm["spec"]["nics"][0]["network"] = json!("dmz");
    admin.post("/vms", vm).await;

    // 이 노드에는 브리지가 없다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // VLAN-aware 브리지가 생기면 배치된다.
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status.clone();
    status.bridges = vec![BridgeInfo {
        name: "vmbr0".into(),
        vlan_filtering: true,
    }];
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
}
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opl-api --all-features`
Expected: PASS (`needs_the_bridges_of_its_networks`, `tests/networks.rs` 3개 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 네트워크 API와 브리지를 보는 스케줄러"
```

---

### Task 8: oplctl: network 명령

`oplctl network list|get|create|delete`를 더한다. `create NAME --bridge BR [--vlan N]`.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: PoolCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
    Delete {
        pool: String,
    },
}
```

을 다음으로 바꾼다:

```rust
    Delete {
        pool: String,
    },
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크를 만든다.
    Create {
        name: String,
        /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
        #[arg(long)]
        bridge: String,
        /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
        #[arg(long)]
        vlan: Option<u16>,
    },
    Delete {
        network: String,
    },
}
```

`crates/oplctl/src/cli.rs`에서

```rust
    disks: Vec<String>,
    /// 연결할 네트워크 이름(지금은 user만). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
```

을 다음으로 바꾼다:

```rust
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
```

`crates/oplctl/src/cli.rs`에서

```rust
                writeln!(out, "storage pool {pool} deleted")?;
            }
```

을 다음으로 바꾼다:

```rust
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, &["NAME", "BRIDGE", "VLAN"], |n| {
                    vec![
                        cell(&n["name"]),
                        cell(&n["spec"]["bridge"]),
                        cell(&n["spec"]["vlan"]),
                    ]
                })?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create { name, bridge, vlan } => {
                let body = json!({ "name": name, "spec": { "bridge": bridge, "vlan": vlan } });
                client.post("/networks", &body).await?;
                writeln!(out, "network {name} created")?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
```

- [ ] **Step 2: 확인**

Run: `cargo test -p oplctl`
Expected: PASS (`cli_definition_is_valid` 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): network 명령"
```

---

### Task 9: opld: [hypervisor] 설정, 노드의 브리지 알리기

설정에 `[hypervisor]`(`accel = auto|kvm|tcg|hvf`, `qemu_binary`, `uefi_code`, `uefi_vars`, `systemd_scope` — Linux 기본 켬)를 더하고, 다른 OS의 가속기와 macOS의 `systemd_scope`를 거부한다. `Host`가 프로필, `IpRoute`(Linux), 스코프 여부를 묶어 `QemuConfig`를 만든다. 노드 등록은 하이퍼바이저 이름(`qemu-kvm`/`qemu-tcg`/`qemu-hvf`)과 브리지 목록을 기록하고, 1분마다 브리지가 바뀌었으면 다시 기록한다. 테스트 하네스는 `OPL_TEST_SYSTEMD`가 있을 때만 스코프를 켠다.

**Files:**
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/host.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/src/bootstrap.rs`에서

```rust
use crate::config::Config;
```

을 다음으로 바꾼다:

```rust
use crate::config::Config;
use crate::host::Host;
```

`crates/opld/src/bootstrap.rs`에서

```rust
    config: &Config,
    name: &str,
```

을 다음으로 바꾼다:

```rust
    config: &Config,
    host: &Host,
    name: &str,
```

`crates/opld/src/bootstrap.rs`에서

```rust
    cluster.create(&info).await?;
    register_node(cluster, config).await?;
    let password = Uuid::new_v4().simple().to_string();
```

을 다음으로 바꾼다:

```rust
    cluster.create(&info).await?;
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
```

`crates/opld/src/bootstrap.rs`에서

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량을 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
```

을 다음으로 바꾼다:

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
```

`crates/opld/src/bootstrap.rs`에서

```rust
        memory_mib: crate::host::memory_mib(),
        hypervisor: "qemu-hvf".into(),
        bridges: vec![],
    };
    cluster.update_status::<NodeSpec>(id, &status).await?;
    Ok(())
```

을 다음으로 바꾼다:

```rust
        memory_mib: crate::host::memory_mib(),
        hypervisor: host.hypervisor_name(),
        bridges: host.bridges().await,
    };
    cluster.update_status::<NodeSpec>(id, &status).await?;
    Ok(())
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았으면 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    let bridges = host.bridges().await;
    if node.status.bridges != bridges {
        tracing::info!(?bridges, "bridges changed");
        let status = NodeStatus {
            bridges,
            ..node.status
        };
        cluster.update_status::<NodeSpec>(id, &status).await?;
    }
    Ok(())
```

`crates/opld/src/config.rs` 전체:

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_qemu::{Accel, HostProfile, kvm_available};
use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// 다른 노드와 클라이언트가 이 노드에 닿는 호스트(IP나 DNS 이름). 노드 인증서에 들어가고,
    /// 클러스터에 알리는 주소(`address:포트`)가 된다. 나중에 바꿀 수 없다.
    pub address: String,
    /// API(HTTPS)를 받을 소켓 주소
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// 노드 간 통신(gRPC over mTLS)을 받을 소켓 주소
    #[serde(default = "default_cluster_listen")]
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
    /// opld를 실행한 사용자만 쓸 수 있다(0600).
    pub control_socket: Option<PathBuf>,
    /// 클러스터 저장소, 노드 인증서, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리(macOS). Linux는 Debian 경로를 쓴다(`[hypervisor]`로 바꾼다).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub hypervisor: HypervisorSection,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HypervisorSection {
    /// `auto`(macOS는 hvf, Linux는 KVM이 있으면 kvm, 없으면 tcg), `kvm`, `tcg`, `hvf`
    #[serde(default)]
    pub accel: AccelSetting,
    /// QEMU 실행 파일. 기본은 호스트에 맞는 `qemu-system-*`이다.
    pub qemu_binary: Option<PathBuf>,
    /// UEFI 펌웨어 코드와 VM마다 복사하는 변수 저장소 원본
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
    /// QEMU를 `systemd-run --scope`로 감싼다(VM마다 cgroup). Linux에서 기본으로 켠다.
    #[serde(default = "default_systemd_scope")]
    pub systemd_scope: bool,
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            accel: AccelSetting::default(),
            qemu_binary: None,
            uefi_code: None,
            uefi_vars: None,
            systemd_scope: default_systemd_scope(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccelSetting {
    #[default]
    Auto,
    Kvm,
    Tcg,
    Hvf,
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

fn default_listen() -> SocketAddr {
    "0.0.0.0:8006".parse().unwrap()
}
fn default_cluster_listen() -> SocketAddr {
    "0.0.0.0:8007".parse().unwrap()
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
fn default_systemd_scope() -> bool {
    cfg!(target_os = "linux")
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
fn with_port(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
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
        for (key, addr) in [
            ("listen", self.listen),
            ("cluster_listen", self.cluster_listen),
        ] {
            if addr.port() == 0 {
                anyhow::bail!("{key} must have a port other than 0 (got {addr})");
            }
        }
        let linux = cfg!(target_os = "linux");
        match self.hypervisor.accel {
            AccelSetting::Kvm if !linux => anyhow::bail!("accel = \"kvm\" needs Linux"),
            AccelSetting::Hvf if linux => anyhow::bail!("accel = \"hvf\" needs macOS"),
            _ => {}
        }
        if self.hypervisor.systemd_scope && !linux {
            anyhow::bail!("systemd_scope needs Linux; set it to false");
        }
        // 알리는 주소는 `address`에 포트만 붙이므로 포트가 같으면 둘을 구별할 수 없다.
        if self.listen.port() == self.cluster_listen.port() {
            anyhow::bail!(
                "listen and cluster_listen must use different ports (got {} for both)",
                self.listen.port()
            );
        }
        Ok(())
    }

    /// 이 호스트에서 QEMU를 실행하는 방법. `accel = "kvm"`인데 `/dev/kvm`을 열 수 없으면 오류다.
    pub fn host_profile(&self) -> anyhow::Result<HostProfile> {
        let h = &self.hypervisor;
        let mut profile = HostProfile::native(&self.qemu_share_dir);
        match h.accel {
            AccelSetting::Auto => {}
            AccelSetting::Kvm if !kvm_available() => {
                anyhow::bail!("accel = \"kvm\" but /dev/kvm cannot be opened")
            }
            AccelSetting::Kvm => profile.accel = Accel::Kvm,
            AccelSetting::Tcg => profile.accel = Accel::Tcg,
            AccelSetting::Hvf => profile.accel = Accel::Hvf,
        }
        if let Some(binary) = &h.qemu_binary {
            profile.qemu_binary = binary.clone();
        }
        if let Some(code) = &h.uefi_code {
            profile.uefi_code = code.clone();
        }
        if let Some(vars) = &h.uefi_vars {
            profile.uefi_vars_template = vars.clone();
        }
        Ok(profile)
    }

    /// 클러스터에 알리는 API 주소
    pub fn api_address(&self) -> String {
        with_port(&self.address, self.listen.port())
    }

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
        }
        names
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.run_dir.join("opld.sock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn fills_defaults() {
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
        assert_eq!(c.api_address(), "10.0.0.5:8006");
        assert_eq!(c.cluster_address(), "10.0.0.5:8007");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.control_socket(), Path::new("/run/opl/opld.sock"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
        assert_eq!(c.hypervisor.systemd_scope, cfg!(target_os = "linux"));
        assert_eq!(
            c.cert_names(),
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn rejects_unknown_keys_and_missing_address() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\nlisten_addr = \"x\"").is_err()
        );
        assert!(toml::from_str::<Config>("name = \"n\"").is_err());
        let c = parse("name = \"n\"\naddress = \"h\"\n[reconciler]\nacpi_timeout_secs = 5");
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn validates_name_and_address() {
        let with = |name: &str, address: &str| Config {
            name: name.into(),
            address: address.into(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with("node1", "10.0.0.5").validate().is_ok());
        assert!(with("node1", "node1.lan").validate().is_ok());
        assert!(with("node1", "::1").validate().is_ok());
        assert!(with("Node_1", "10.0.0.5").validate().is_err());
        for bad in ["", "10.0.0.5:8006", "a b", "[::1]"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
        assert_eq!(with("n", "::1").api_address(), "[::1]:8006");
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_ports() {
        let ok = parse("name = \"n\"\naddress = \"h\"");
        assert!(ok.validate().is_ok());
        let same = Config {
            cluster_listen: "127.0.0.1:8006".parse().unwrap(),
            ..ok.clone()
        };
        assert!(same.validate().is_err());
        let zero = Config {
            listen: "0.0.0.0:0".parse().unwrap(),
            ..ok.clone()
        };
        assert!(zero.validate().is_err());
        let zero = Config {
            cluster_listen: "0.0.0.0:0".parse().unwrap(),
            ..ok
        };
        assert!(zero.validate().is_err());
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\n\
             qemu_binary = \"/opt/qemu/bin/qemu-system-x86_64\"\nsystemd_scope = false",
        );
        assert!(c.validate().is_ok());
        let p = c.host_profile().unwrap();
        assert_eq!(p.accel, Accel::Tcg);
        assert_eq!(p.qemu_binary, Path::new("/opt/qemu/bin/qemu-system-x86_64"));

        // 다른 OS의 가속기와 macOS의 systemd 스코프는 거부한다.
        let other = if cfg!(target_os = "linux") {
            "hvf"
        } else {
            "kvm"
        };
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"{other}\"\nsystemd_scope = false"
        ));
        assert!(c.validate().is_err());
        if !cfg!(target_os = "linux") {
            let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\nsystemd_scope = true");
            assert!(c.validate().is_err());
        }
        assert!(
            toml::from_str::<Config>(
                "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"xen\""
            )
            .is_err()
        );
    }
}
```

`crates/opld/src/host.rs`에서

```rust
//! 호스트 용량. 스케줄러가 노드를 고를 때 쓴다.
```

을 다음으로 바꾼다:

```rust
//! 호스트 용량과 가상화 구성. 스케줄러가 노드를 고를 때 쓴다.

use std::sync::Arc;

use opl_driver::{HostNet, IpRoute, QemuConfig};
use opl_qemu::{Accel, HostProfile};
use opl_types::node::BridgeInfo;

use crate::config::Config;

/// 이 노드가 VM을 실행하는 방법
#[derive(Debug, Clone)]
pub struct Host {
    pub profile: HostProfile,
    /// 브리지 NIC의 tap을 다룬다. Linux에서만 있다.
    pub net: Option<Arc<dyn HostNet>>,
    pub systemd_scope: bool,
}

impl Host {
    pub fn from_config(config: &Config) -> anyhow::Result<Self> {
        let profile = config.host_profile()?;
        if cfg!(target_os = "linux") && profile.accel == Accel::Tcg {
            tracing::warn!("KVM is not available; VMs run with TCG emulation (slow)");
        }
        let net: Option<Arc<dyn HostNet>> = if cfg!(target_os = "linux") {
            Some(Arc::new(IpRoute))
        } else {
            None
        };
        Ok(Self {
            profile,
            net,
            systemd_scope: config.hypervisor.systemd_scope,
        })
    }

    pub fn qemu_config(&self, config: &Config) -> QemuConfig {
        QemuConfig {
            host: self.profile.clone(),
            run_dir: config.run_dir.clone(),
            state_dir: config.data_dir.clone(),
            net: self.net.clone(),
            systemd_scope: self.systemd_scope,
        }
    }

    /// `NodeStatus.hypervisor`: `qemu-kvm`, `qemu-tcg`, `qemu-hvf`
    pub fn hypervisor_name(&self) -> String {
        format!("qemu-{}", self.profile.accel.as_str())
    }

    /// 이 노드의 Linux 브리지. 알 수 없으면 비어 있다.
    pub async fn bridges(&self) -> Vec<BridgeInfo> {
        let Some(net) = &self.net else {
            return vec![];
        };
        match net.bridges().await {
            Ok(bridges) => bridges,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list bridges");
                vec![]
            }
        }
    }
}
```

`crates/opld/src/host.rs`에서

```rust
        assert!(super::memory_mib() > 0);
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(super::memory_mib() > 0);
    }

    #[test]
    fn names_the_hypervisor_by_accelerator() {
        let config: crate::config::Config = toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap();
        let host = super::Host::from_config(&config).unwrap();
        assert_eq!(host.hypervisor_name(), "qemu-tcg");
        assert_eq!(host.net.is_some(), cfg!(target_os = "linux"));
    }
}
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError};
use opl_driver::{DirStorage, Hypervisor, QemuHypervisor, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::host::Host;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록을 다시 확인하는 간격. 관리자가 브리지를 만들면 이만큼 뒤에 스케줄러가 안다.
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

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config() -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
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

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config();
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
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
        let host = match Host::from_config(config) {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(host.qemu_config(config)).await {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        let (stop, rx) = oneshot::channel::<()>();
        let service = PeerService::new(cluster.clone(), Some(hv.clone()));
        let peer_tls = tls.clone();
        let tx = exited_tx.clone();
        let task = tokio::spawn(async move {
            let r = opl_peer::serve(peer_listener, &peer_tls, service, async {
                let _ = rx.await;
            })
            .await;
            let _ = tx.send(format!("cluster server stopped: {r:?}")).await;
            r
        });
        let peer = Server { stop, task };

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
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
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
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = if first_time {
            tokio::spawn(async {})
        } else {
            let (cluster, peers) = (cluster.clone(), peers.clone());
            tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
        };
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
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
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node);
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv, reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        let runner =
            TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config).await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let app = opl_api::router(Arc::new(state));
        let task = tokio::spawn(async move {
            let r = axum::serve(listener, app)
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
            api: Server { stop, task },
            reconciler,
            runner,
            monitor,
            register,
            promotion,
            exited,
        };
        Ok((member, created))
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let members = self.cluster.members();
        if members.voters.len() == 1 && members.voters.contains_key(&me) {
            anyhow::bail!(
                "this node is the only voting member; stop opld and remove its data directory instead"
            );
        }
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
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        self.register.abort();
        self.promotion.abort();
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

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
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
```

`crates/opld/tests/common/mod.rs`에서

```rust
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
            "name = \"{name}\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{port}\"\n\
             cluster_listen = \"127.0.0.1:{cluster_port}\"\ncontrol_socket = \"{sock}\"\n\
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}",
            sock = dir.path().join("opld.sock").display(),
```

을 다음으로 바꾼다:

```rust
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        // systemd가 있는 Linux(CI)에서만 `OPL_TEST_SYSTEMD=1`로 QEMU를 스코프로 감싼다.
        let systemd_scope = std::env::var_os("OPL_TEST_SYSTEMD").is_some();
        let config = format!(
            "name = \"{name}\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{port}\"\n\
             cluster_listen = \"127.0.0.1:{cluster_port}\"\ncontrol_socket = \"{sock}\"\n\
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\nsystemd_scope = {systemd_scope}\n",
            sock = dir.path().join("opld.sock").display(),
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opld --all-features`
Expected: macOS와 Linux 컨테이너(아래 '실행 환경')에서 PASS (실제 QEMU `e2e`, 프로세스 3개 `cluster` 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): [hypervisor] 설정(KVM/TCG, systemd 스코프), 노드 브리지 알리기"
```

---

### Task 10: E2E: 브리지 네트워크 (M7 완료 기준)

opld 하나로 클러스터를 만들고, 노드가 테스트 브리지를 알리는지, `network create --vlan 30`으로 만든 네트워크를 쓰는 VM의 tap이 그 브리지에 VLAN 30(pvid untagged)으로 붙는지, 쓰는 네트워크는 지울 수 없는지, VM을 끄면 tap이 사라지는지 본다.

**Files:**
- Create: `crates/opld/tests/bridge.rs`
- Modify: `crates/opld/Cargo.toml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/Cargo.toml`에서

```toml
[features]
# 실제 QEMU로 CLI부터 콘솔, kill -9 뒤 재연결까지 확인하는 E2E 테스트. Apple Silicon Mac + brew install qemu가 필요하다.
qemu-it = []
```

을 다음으로 바꾼다:

```toml
[features]
# 실제 QEMU로 CLI부터 콘솔, kill -9 뒤 재연결까지 확인하는 E2E 테스트.
# macOS는 brew install qemu, Linux는 apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils가 필요하다.
qemu-it = []
# VM NIC를 Linux 브리지에 잇는 E2E 테스트(qemu-it와 함께). Linux에서 root와 iproute2가 필요하다.
net-it = []
```

`crates/opld/tests/bridge.rs`를 만든다:

```rust
//! M7 완료 기준: Linux에서 브리지 네트워크를 만들고, 그 네트워크를 쓰는 VM의 NIC가 VLAN 태그와
//! 함께 브리지에 붙었다가 VM을 끄면 떨어진다.
//! `cargo test -p opld --features qemu-it,net-it --test bridge`
//! Linux에서 root, iproute2, QEMU가 필요하다(KVM이 없으면 TCG).
#![cfg(all(feature = "qemu-it", feature = "net-it", target_os = "linux"))]

mod common;

use std::process::Command;
use std::time::Duration;

use common::{Daemon, WAIT};
use opl_types::network::tap_name;
use serde_json::Value;
use uuid::Uuid;

/// 관리자가 미리 만든 VLAN-aware 브리지. 끝나면 지운다.
struct TestBridge(String);

impl TestBridge {
    fn new() -> Self {
        let name = format!("oplb{}", &Uuid::now_v7().simple().to_string()[22..]);
        for args in [
            vec![
                "link",
                "add",
                &name,
                "type",
                "bridge",
                "vlan_filtering",
                "1",
            ],
            vec!["link", "set", &name, "up"],
        ] {
            assert!(Command::new("ip").args(&args).status().unwrap().success());
        }
        Self(name)
    }
}

impl Drop for TestBridge {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

fn json(program: &str, args: &[&str]) -> Option<Value> {
    let out = Command::new(program).args(args).output().unwrap();
    out.status
        .success()
        .then(|| serde_json::from_slice(&out.stdout).unwrap())
}

fn master_of(ifname: &str) -> Option<String> {
    let v = json("ip", &["-j", "link", "show", "dev", ifname])?;
    v[0]["master"].as_str().map(str::to_string)
}

#[tokio::test]
async fn vm_nics_join_the_bridge_with_their_vlan() {
    let br = TestBridge::new();
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("m7").await;
    d.login().await;

    // 노드가 브리지를 알린다.
    let node = d.ctl_json(&["node", "get", "node1"]).await;
    let bridges = node["status"]["bridges"].as_array().unwrap();
    assert!(
        bridges
            .iter()
            .any(|b| b["name"] == br.0.as_str() && b["vlan_filtering"] == true),
        "{node}"
    );
    assert!(
        node["status"]["hypervisor"]
            .as_str()
            .is_some_and(|h| h == "qemu-kvm" || h == "qemu-tcg")
    );

    d.ctl(&[
        "network", "create", "lan", "--bridge", &br.0, "--vlan", "30",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--nic", "lan", "--nic", "user", "--start",
        ])
        .await
        .unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    let vm = d.ctl_json(&["vm", "get", "web"]).await;
    assert_eq!(vm["status"]["phase"], "running");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let tap = tap_name(id, 0);
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    let vlans = json("bridge", &["-j", "vlan", "show", "dev", &tap]).unwrap();
    let vlans = &vlans[0]["vlans"];
    assert_eq!(vlans.as_array().unwrap().len(), 1, "{vlans}");
    assert_eq!(vlans[0]["vlan"], 30);

    // 쓰고 있는 네트워크는 지울 수 없다.
    let err = d.ctl(&["network", "delete", "lan"]).await.unwrap_err();
    assert!(err.to_string().contains("network_in_use"), "{err}");

    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    let deadline = tokio::time::Instant::now() + WAIT;
    while master_of(&tap).is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "tap {tap} is still there"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["network", "delete", "lan"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo test -p opld --features qemu-it,net-it --test bridge`
Expected: Linux 컨테이너(아래 '실행 환경')에서 root로 PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "test(opld): 브리지 네트워크 E2E (M7)"
```

---

### Task 11: CI(linux-kvm), 문서

CI에 `linux-kvm` 작업을 더한다: GitHub Linux 러너에서 `/dev/kvm`을 열고 QEMU·OVMF·iproute2를 설치한 뒤, root로 `OPL_TEST_SYSTEMD=1`과 함께 QEMU·브리지·E2E 테스트를 돌린다(root 빌드는 캐시하지 않는다). systemd 스코프 테스트(`qemu_runs_in_its_own_systemd_scope`)는 `OPL_TEST_SYSTEMD`가 있을 때만 확인한다. README에 Linux 실행 방법, 설계 문서에 M7을 반영한다.

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml

  deny:
```

을 다음으로 바꾼다:

```yaml

  # M7: 실제 KVM으로 VM을 띄우고 NIC를 브리지에 잇는다. tap과 브리지를 만들려면 root가 필요하고,
  # QEMU는 systemd 스코프로 감싼다(OPL_TEST_SYSTEMD).
  linux-kvm:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: KVM, QEMU, UEFI firmware, iproute2
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' \
            | sudo tee /etc/udev/rules.d/99-kvm.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config qemu-system-x86 ovmf qemu-utils iproute2
          ls -l /dev/kvm
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
      - name: QEMU, bridge and E2E tests as root
        run: |
          sudo -E env "PATH=$PATH" OPL_TEST_SYSTEMD=1 CARGO_TARGET_DIR="$RUNNER_TEMP/target" \
            cargo test --features "$FEATURES" -p opl-qemu -p opl-driver -p opl-reconciler -p opld
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it

  deny:
```

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M6)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (M7)
```

`README.md`에서

```markdown

## 개발
```

을 다음으로 바꾼다:

````markdown

## 실행 (Linux, KVM)

opld는 root로 돈다(VM NIC의 tap 장치를 만든다). QEMU는 VM마다 `opl-vm-<id>.scope`로 감싼다.
KVM(`/dev/kvm`)이 없으면 TCG로 돌고 경고를 남긴다.

```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2
# 브리지는 관리자가 미리 만든다. VLAN 태그를 쓰려면 VLAN-aware여야 한다.
sudo ip link add vmbr0 type bridge vlan_filtering 1
sudo ip link set eth1 master vmbr0 && sudo ip link set vmbr0 up
sudo mkdir -p /etc/opl
sudo tee /etc/opl/opld.toml >/dev/null <<'TOML'
name = "node1"
address = "10.0.0.11"
# [hypervisor]
# accel = "auto"          # auto | kvm | tcg
# systemd_scope = true
TOML
sudo opld --config /etc/opl/opld.toml &
sudo oplctl cluster init --name prod
oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
oplctl network create lan --bridge vmbr0 --vlan 10   # VLAN 10을 태그 없이 주고받는 NIC
# 풀과 볼륨은 macOS와 같다(/var/lib/opl/pool 등).
oplctl vm create web --memory 1024 --disk root --nic lan --start
```

브리지를 쓰는 VM은 그 브리지가 있는 노드에만 배치된다. 노드는 브리지 목록을 시작할 때와
1분마다 알린다.

## 개발
````

`README.md`에서

```markdown
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: apt install qemu-utils)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (Apple Silicon)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
```

을 다음으로 바꾼다:

```markdown
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: 위 apt 패키지)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (macOS hvf, Linux KVM/TCG)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
```

`README.md`에서

````markdown
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```
````

을 다음으로 바꾼다:

````markdown
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
# Linux, root: 실제 tap과 브리지
sudo -E env "PATH=$PATH" cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN
sudo -E env "PATH=$PATH" cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
```

systemd가 있는 Linux에서는 `OPL_TEST_SYSTEMD=1`을 주면 QEMU 테스트가 systemd 스코프로 돈다(CI의
`linux-kvm` 작업). macOS에서는 Docker의 privileged Linux 컨테이너(`--init`)로 Linux 테스트를 TCG로
돌릴 수 있다.
````

`README.md`에서

```markdown
- [계획 6: 클러스터 부트스트랩, 노드 간 통신](docs/superpowers/plans/2026-10-22-plan6-cluster-bootstrap.md)
```

을 다음으로 바꾼다:

```markdown
- [계획 6: 클러스터 부트스트랩, 노드 간 통신](docs/superpowers/plans/2026-10-22-plan6-cluster-bootstrap.md)
- [계획 7: Linux/KVM, 브리지 네트워크](docs/superpowers/plans/2026-10-22-plan7-kvm-bridge.md)
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
    assert!(b.list().await.unwrap().is_empty());
}
```

을 다음으로 바꾼다:

```rust
    assert!(b.list().await.unwrap().is_empty());
}

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돈다.
#[tokio::test]
async fn qemu_runs_in_its_own_systemd_scope() {
    if std::env::var_os("OPL_TEST_SYSTEMD").is_none() {
        return;
    }
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        cgroup.contains(&format!("opl-vm-{}.scope", vm.id)),
        "{cgroup}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    // 스코프는 QEMU가 끝나면 사라지므로 같은 VM을 다시 시작할 수 있다.
    hv.start(&vm).await.unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md` 전체:

````markdown
# opLite 1차 하위 프로젝트 설계: 노드 코어 (클러스터 상태 + VM 라이프사이클)

- 작성일: 2026-10-20
- 상태: 설계 승인됨, 구현 계획 작성 전
- 범위: 전체 제품 중 첫 번째 하위 프로젝트

## 1. 제품 맥락

opLite는 Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼이다. Proxmox VE, VMware vSphere와 같은 영역의 제품이다.

| 항목 | 결정 |
|---|---|
| 1차 목표 시장 | VMware 이탈 기업(중소·중견), 3~32노드 온프레미스 클러스터 |
| 사업 모델 | 오픈코어: 코어 공개 + 엔터프라이즈 플러그인과 기술지원 구독 |
| 팀과 일정 | 소규모 팀(3~8명), 12개월 안에 파일럿 고객 확보 |
| 개발 환경 | 당분간 macOS(Apple Silicon)만 사용. Linux/KVM 환경은 나중에 확보 |
| 아키텍처 | 대칭형 노드 + 내장 Raft (별도 관리 서버 없음) |
| 하이퍼바이저 | QEMU/KVM을 QMP로 직접 제어 (libvirt 사용 안 함) |

### 1.1 전체 제품 분해

| # | 하위 프로젝트 | 대응 기능 (Proxmox / VMware) |
|---|---|---|
| 1 | **노드 코어: 클러스터 상태 + VM 라이프사이클 (이 문서)** | qemu-server, pmxcfs / hostd |
| 2 | 웹 UI | PVE GUI / vSphere Client |
| 3 | 공유 스토리지와 스냅샷 (NFS, iSCSI, Ceph, LVM, ZFS) | pve-storage / VMFS, vSAN |
| 4 | 라이브 마이그레이션 | qm migrate / vMotion |
| 5 | HA와 펜싱, 2노드 위트니스 | ha-manager / vSphere HA |
| 6 | 네트워크와 SDN (브리지 관리, VXLAN, 방화벽) | pve-network / NSX |
| 7 | 백업 | PBS / VADP |
| 8 | VMware 마이그레이션 마법사 | Import Wizard |
| 9 | 설치 ISO, 업데이트 채널, 라이선스, 텔레메트리 | 구독 저장소 / vLCM |
| 10 | 엔터프라이즈 플러그인 (LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR) | — / vCenter 기능 |

하위 프로젝트마다 별도로 설계 → 계획 → 구현 사이클을 거친다. 위 순서가 구현 권장 순서다.

### 1.2 검토했지만 채택하지 않은 대안

- **중앙 관리 서버 + 경량 에이전트 (vCenter 방식):** 관리 서버 자체의 HA를 따로 풀어야 하고 설치 단계가 늘어난다. 이 복잡함이 VMware를 떠나는 이유 중 하나다.
- **Kubernetes 기반 (KubeVirt 방식):** 운영 복잡도가 목표 고객과 맞지 않는다.
- **Cloud Hypervisor (Rust VMM):** Windows와 레거시 장치 지원이 부족하다. 대신 `Hypervisor` 트레이트를 통해 나중에 경량 VM 백엔드로 추가할 수 있게 열어 둔다.

## 2. 아키텍처와 프로세스 모델

```
┌──────────────────── 노드 (Linux 호스트, Debian 기반 어플라이언스) ────────────────────┐
│                                                                                      │
│  opld (단일 바이너리, tokio)                                                          │
│  ├─ api        : HTTPS REST + WebSocket (axum)   ← UI/CLI/Terraform이 접속             │
│  ├─ cluster    : openraft 상태 머신 (redb 저장)  ← 클러스터 설정의 단일 진실 원천      │
│  ├─ reconciler : 원하는 상태와 실제 상태를 맞추는 조정 루프 (노드별)                   │
│  └─ drivers    : Hypervisor / Storage / Network 트레이트 구현체                        │
│                                                                                      │
│  qemu-system-* (VM 1개당 프로세스 1개, systemd scope로 격리, QMP 소켓으로 제어)       │
└──────────────────────────────────────────────────────────────────────────────────────┘
          ▲ 노드 간 통신: gRPC(tonic) over mTLS — Raft 메시지 + 노드 간 작업 전달
```

### 2.1 핵심 원칙

1. **선언형 + 조정 루프.** API는 원하는 상태(spec)를 Raft에 기록하기만 한다. 각 노드의 reconciler가 자기 노드에 배정된 리소스를 실제 상태로 맞춘다.
2. **작업(Task) 모델.** 오래 걸리는 작업은 모두 Task ID를 반환하고, 진행률과 로그를 WebSocket으로 스트리밍한다.
3. **Raft에는 설정만 넣는다.** 메트릭 같은 고빈도 데이터는 노드 로컬에 저장하고, 노드 간 조회는 gRPC로 한다.
4. **단일 노드도 1인 클러스터다.** 단일 노드 모드라는 별도 코드 경로를 두지 않는다.

### 2.2 크레이트 구조 (Cargo 워크스페이스)

| 크레이트 | 역할 | 공개 여부 |
|---|---|---|
| `opl-types` | 리소스 모델(VmSpec, VolumeSpec 등), serde 스키마 | 공개 |
| `opl-qemu` | QMP 클라이언트 + QEMU 커맨드라인 생성기 | 공개 |
| `opl-driver` | `Hypervisor`/`Storage`/`HostNet`(tap·브리지) 트레이트, Mock·QEMU(hvf/KVM/TCG)·iproute2 구현 | 공개 |
| `opl-cluster` | openraft 상태 머신, 리소스 저장소, watch 스트림, 리더 전달 | 공개 |
| `opl-peer` | 노드 간 통신(gRPC over mTLS: Raft RPC, 리더 전달, 생존 확인, 콘솔 중계), 클러스터 PKI | 공개 |
| `opl-reconciler` | 조정 루프, VM별 작업 큐 | 공개 |
| `opl-api` | REST/WebSocket, 인증, `Authorizer` 훅 | 공개 |
| `opld` | 데몬 바이너리 조립 | 공개 |
| `oplctl` | CLI | 공개 |
| `opl-ent-*` | 엔터프라이즈 플러그인 | 비공개 (별도 저장소) |

오픈코어 경계는 크레이트 단위로 나눈다. 엔터프라이즈 기능은 공개 크레이트가 정의한 트레이트(`Authorizer`, `AuthProvider` 등)를 구현하는 비공개 크레이트로 제공하고, `opld` 빌드 시 기능 플래그로 연결한다. 공개 크레이트는 비공개 크레이트에 의존하지 않는다.

## 3. 리소스 데이터 모델

### 3.1 공통 봉투

```rust
struct Resource<S, T> {
    id: Uuid,                          // UUIDv7, 변경 불가
    name: String,                      // 종류별로 클러스터 내 유일
    revision: u64,                     // 마지막 수정의 Raft 커밋 인덱스
    labels: BTreeMap<String, String>,
    spec: S,                           // 원하는 상태: API로만 수정
    status: T,                         // 실제 상태: reconciler만 기록
}
```

- spec과 status를 분리해 사용자 쓰기와 시스템 쓰기가 충돌하지 않게 한다.
- `revision`은 낙관적 동시성 제어에 쓴다. 클라이언트가 `If-Match: <revision>`을 보냈는데 값이 다르면 412를 반환한다.
- 저장 형식 버전(`schema_version`)은 봉투가 아니라 저장 계층에 둔다. 노드의 redb 파일(`meta` 테이블)과 Raft 스냅샷 본문에 기록하고, 지원하지 않는 버전의 파일이나 스냅샷은 열지 않는다. 형식이 바뀌면 버전을 올리고 마이그레이션을 추가한다.
- 식별자는 숫자 VMID가 아니라 UUID를 쓴다. 다중 클러스터를 병합해도 충돌하지 않게 하기 위해서다.
- **저장 형식 호환성 규칙:** Raft에 저장되는 타입(spec/status)에 필드를 추가할 때는 반드시 `#[serde(default)]`를 붙인다. 새 필드는 클러스터의 모든 노드가 업그레이드되어 클러스터 기능 레벨이 올라간 뒤에만 기록할 수 있다. 이 규칙 덕분에 구 버전 노드는 새 필드를 만나지 않으므로, spec 타입의 `deny_unknown_fields`(API 오타 거부)를 유지할 수 있다.
- Raft 로그 엔트리(`Command`)는 버전 필드 없이 serde 태그로 구분한다. 모르는 명령은 무시하지 않고 오류로 처리하므로, 새 명령도 클러스터 기능 레벨이 올라간 뒤에만 기록한다.
- Raft 노드 id는 `Node` 리소스 id와 같은 UUID다(`opl_cluster::types::NodeId`).

### 3.2 리소스 목록

| 리소스 | spec | status |
|---|---|---|
| `Node` | 이름, 주소, 유지보수 모드 | 온라인 여부, CPU/메모리 용량, 하이퍼바이저 종류(`qemu-kvm`/`qemu-tcg`/`qemu-hvf`), Linux 브리지 목록(VLAN-aware 여부) |
| `Vm` | vCPU, 메모리, 펌웨어(bios/uefi), 머신 타입, 디스크·NIC, `power`, `node`(선택), `restart` 정책 | 실제 전원 상태, 실행 노드, QEMU PID, 오류 조건 |
| `StoragePool` | 종류(`dir`), 경로, 사용할 노드, `shared: bool` | 용량, 사용량 |
| `Volume` | 소속 풀, 크기, 형식(qcow2/raw) | 준비 상태(Pending/Ready), 할당 크기, 파일을 만드는 노드 |
| `Network` | 브리지 이름, VLAN 태그(선택). 이름 `user`는 예약(QEMU NAT, 리소스 없음) | — |
| `Task` | 작업 종류, 대상 리소스, 실행 노드 | 상태(Pending/Running/Succeeded/Failed), 오류 |
| `User`, `ApiToken` | 사용자 이름, 비밀번호 해시(argon2), 역할 / 주인, 토큰 비밀 값의 SHA-256 | — |
| `Cluster` | 클러스터 이름(리소스 이름), CA 인증서와 개인키(API로 내보내지 않는다) | — |
| `JoinToken` | 비밀 값의 SHA-256, 만료 시각 | — |

VM이 붙인 볼륨과 NIC의 MAC 주소는 클러스터 안에서 VM 하나만 쓸 수 있다. 여러 노드의 API가 동시에 써도 겹치지 않도록 상태 머신이 커밋 순서대로 확인한다(`Rejection::InUse`, API는 409).

`Vm` spec 예시:

```yaml
name: web-01
spec:
  cpu: { sockets: 1, cores: 4, model: host }
  memory_mib: 8192
  firmware: uefi
  machine: q35            # macOS(hvf)에서는 virt
  disks:
    - { volume: web-01-root, bus: virtio-blk, boot_order: 1 }
  nics:
    - { network: lan, model: virtio }   # mac을 생략하면 자동 할당
  power: running          # running | stopped
  restart: on-failure     # never | on-failure
  node: null              # null이면 스케줄러가 배치
```

### 3.3 스케줄러 (1차)

`node`가 비어 있는 VM을 시작하면 스케줄러가 노드를 고른다. 대상은 온라인 상태이고, 유지보수 모드가 아니며, 디스크가 있는 풀에 접근할 수 있고, NIC가 쓰는 네트워크의 브리지가 있는(VLAN을 쓰면 VLAN-aware인) 노드다. 이 중 미할당 메모리가 가장 많은 노드를 선택한다. 선택 결과는 `spec.node`에 기록한다. 리소스 오버커밋 정책과 DRS급 재배치는 범위 밖이다.

## 4. API

- 경로는 `/api/v1/{resource}`이며, 목록·조회·생성·수정·삭제를 제공한다. 경로의 `{id}` 자리에는 id나 이름을 쓸 수 있다(UUID 모양의 이름은 받지 않는다). 목록은 `{revision, items}`를 돌려준다.
- 동작은 `POST /api/v1/vms/{id}/actions/{start|stop|reboot}`이다. 응답은 `202 Accepted`와 `Task`다.
  - `start`/`stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 만든다. 대상 노드의 Task 실행기가 VM이 원하는 상태에 도달하면 Task를 끝낸다.
  - `stop?mode=force`는 reconciler의 VM별 작업자 큐로 강제 종료를 보낸다. `reboot`은 원하는 상태를 stopped로 바꿨다가 꺼지면 running으로 되돌린다. 그 사이 같은 VM에 새 요청이 오면 그것을 따른다.
- 볼륨 생성·삭제와 이미지 가져오기(`POST /api/v1/volumes/import`, 본문이 이미지)도 Task다. 파일이 준비되기 전의 볼륨은 VM에 붙일 수 없다.
- 감시: `GET /api/v1/watch?kinds=vm,task&since=<revision>` (WebSocket). 목록의 revision 다음 변경부터 받는다. 이어 받을 수 없으면 오류 메시지를 보내고 닫으며, 클라이언트는 목록을 다시 읽는다. 사용자와 토큰은 감시할 수 없다.
- 콘솔: `GET /api/v1/vms/{id}/console` (WebSocket). 실행 노드의 QEMU VNC로 프록시한다. 다른 노드에서 요청하면 노드 간 gRPC로 중계한다.
- 클러스터: `GET /api/v1/cluster`(이름, CA 지문, 리더, 멤버), `GET /api/v1/cluster/ca`(인증 없음), `POST /api/v1/cluster/join-tokens`(관리자), `POST /api/v1/cluster/join`(조인 토큰으로 인증), `DELETE /api/v1/nodes/{id}`(오프라인 노드만).
- 팔로워가 받은 쓰기는 리더로 전달된다. 응답은 그 쓰기가 이 노드에도 적용된 뒤에 오므로, 같은 노드에서 곧바로 읽으면 결과가 보인다.
- OpenAPI 스펙은 `utoipa`로 코드에서 생성해 `GET /api/v1/openapi.json`으로 내보낸다. 라우터와 같은 정의에서 만든다.
- 인증: 로컬 사용자(세션 쿠키, 웹 UI용)와 API 토큰(Bearer, CLI용)을 지원한다. 토큰은 `opl_<id>_<비밀 값>`이고 비밀 값의 SHA-256만 저장한다. 쿠키로 인증한 요청에 `Origin`이 있으면 서버와 같아야 한다. 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고, 바꾸면 그 사용자의 세션이 끝난다. 모든 핸들러는 `Authorizer::check(principal, action, resource)`를 거친다. 1차 구현은 `admin`/`operator`/`viewer` 고정 역할이다.
- 오류 응답은 `{ "code": "...", "message": "...", "details": {...} }` 형식으로 통일한다. Raft 과반수가 없으면 `503 cluster_no_quorum`, revision이 다르면 `412 revision_mismatch`, 이름이 겹치면 `409 name_taken`을 반환한다.

## 5. VM 라이프사이클

### 5.1 시작 흐름

```
클라이언트 ──POST /vms/web-01/actions/start──▶ opl-api (임의의 노드)
  1. 인증·권한 확인 → spec.power = running, node가 비어 있으면 스케줄러가 선택
  2. spec 변경을 Raft에 커밋 → Task(Pending, node = 대상 노드)를 따로 커밋 → 202 + task
                                │ 커밋 스트림
                                ▼
대상 노드의 reconciler
  3. desired(running) ≠ actual(stopped) → Hypervisor::start
  4. 커맨드라인 생성 → QEMU 실행(-S, -daemonize) → QMP 연결 → qmp_capabilities → cont
  5. status(running, pid)를 Raft에 기록
대상 노드의 Task 실행기
  6. VM의 power와 phase가 모두 running이 되면 Task Succeeded (reconciler가 포기하면 Failed)
```

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
| `QemuHypervisor` | Linux(KVM, 없으면 TCG), macOS(hvf) — `HostProfile`로 구분 | 제품, 개발·통합 테스트 |
| `MockHypervisor` | 전체 | 단위·컴포넌트 테스트, 장애 주입(`fail_next_start`, `crash`, `guest_shutdown`) |

QEMU 커맨드라인 생성은 순수 함수 `fn build_cmdline(&VmRuntimeSpec, &HostProfile, &RuntimePaths) -> Result<Vec<String>, CmdlineError>`로 만들고, `insta` 스냅샷으로 검증한다.

### 5.3 reconciler 규칙

- **트리거:** Raft 커밋 이벤트, QMP 이벤트, 30초 주기 전체 재동기화.
- **VM별 직렬화:** VM마다 작업 큐를 하나 두고, 서로 다른 VM은 병렬로 처리한다.
- **멱등성:** 모든 단계는 다시 실행해도 안전하다. 이미 원하는 상태면 아무것도 하지 않는다.
- **종료:** 원하는 상태가 `stopped`면 ACPI 종료를 요청하고, `acpi_timeout`(기본 60초) 안에 꺼지지 않으면 강제 종료한다.
- **게스트 내부 종료:** reconciler가 요청하지 않은 종료 이벤트가 오면(원하는 상태가 `running`인데 꺼짐) `spec.power`를 `stopped`로 바꾼다. QMP의 `guest_initiated`는 opld가 요청한 ACPI 종료에서도 true이므로 그것만으로 판단하지 않는다. 우리가 요청하지 않은 `guest_initiated: false` 종료(호스트의 SIGTERM 등)는 게스트의 결정이 아니므로 원하는 상태를 바꾸지 않는다. 방금까지 돌던 VM이 사라졌는데 이벤트가 아직 없으면 잠시 기다려, 게스트가 끈 VM을 다시 켜지 않게 한다.
- **크래시와 기동 실패:** `restart: on-failure`이면 지수 백오프로 재시작하고, 10분 안에 5회 실패하면 포기한다. `restart: never`의 크래시, 영구 기동 오류(이미지 없음, 잘못된 참조 등), 포기는 모두 `spec.power`를 `stopped`로 바꾸고 status를 `Failed`와 오류 메시지로 남긴다. 사용자가 다시 시작하면 처음부터 센다.
- **응답 없는 VM:** 살아 있지만 QMP로 다룰 수 없는 VM은 끄지 않고 `start()`로 다시 붙기를 계속 시도하며 status에 그 사실을 남긴다. 원하는 상태가 stopped면 ACPI 요청이 실패해도 `acpi_timeout` 뒤 강제 종료한다.
- **배정되지 않은 VM:** 이 노드에서 돌지만 이 노드에 배정되지 않은(삭제되었거나 다른 노드로 옮겨진) VM은 끄지 않고 경고만 남긴다. 저장소가 비거나 뒤처져도 VM을 죽이지 않기 위해서다. 실행 중인 VM의 삭제와 노드 변경은 API가 거부한다.
- **status 쓰기:** reconciler는 자기 노드의 VM status만 기록한다. Raft 과반수가 없어 쓰지 못하면 실제 상태에서 status를 다시 계산해 회복될 때까지 다시 쓴다(디스크 버퍼는 두지 않는다).

### 5.4 opld 재시작과 크래시 복구

- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
- `opld`가 시작되면 런타임 디렉터리를 훑어 살아 있는 QEMU에 QMP로 다시 연결한다. 그 뒤 Raft의 원하는 상태와 비교해 조정한다.
- QEMU의 생존 여부는 QEMU가 수명 내내 잡고 있는 pidfile의 fcntl 잠금으로 판정한다(QMP 응답 여부와 무관하며, SIGSTOP 상태에서도 정확하다). 잠금이 풀린 런타임 디렉터리만 지우고, 잠금 소유자 pid(F_GETLK)를 확인한 뒤에만 강제 종료한다. 살아 있지만 QMP에 붙지 못한 VM은 등록하지 않고 오류를 기록하며, 이후 `start()`가 재부착을 시도한다.
- macOS에서는 `-daemonize`의 fork가 Objective-C 런타임 검사에 걸려 크래시할 수 있으므로 `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES`로 실행한다.
- 이 설계 덕분에 `opld`를 업그레이드하거나 재시작해도 실행 중인 VM은 중단되지 않는다.

### 5.5 오류 처리

- `DriverError`는 `thiserror` 열거형이다. `is_transient()`로 재시도 가능 여부를 구분한다.
  - 일시적 오류(QMP 타임아웃, 소켓 일시 오류): 지수 백오프로 재시도한다.
  - 영구 오류(이미지 없음, 메모리 부족, 잘못된 spec): status 조건에 기록하고 Task를 Failed로 끝낸다.
- Raft 과반수를 잃으면 API 쓰기는 503이 되지만, 실행 중인 VM은 유지된다. 클러스터 장애가 워크로드 장애로 번지지 않게 하기 위해서다.

## 6. 스토리지 (1차: `dir` 풀)

```rust
#[async_trait]
trait Storage: Send + Sync {
    async fn create_volume(&self, pool: &PoolSpec, vol: &VolumeSpec) -> Result<VolumePath, DriverError>;
    async fn delete_volume(&self, pool: &PoolSpec, id: Uuid) -> Result<(), DriverError>;
    async fn import(&self, pool: &PoolSpec, src: &Path, fmt: ImageFormat) -> Result<VolumePath, DriverError>;
    async fn capacity(&self, pool: &PoolSpec) -> Result<Capacity, DriverError>;
}
```

- `dir` 풀은 로컬 디렉터리에 qcow2/raw 파일을 둔다. 작업은 `qemu-img`로 처리한다.
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다.
- 사용 중인 Volume은 삭제할 수 없다(409).

## 7. 네트워크 (1차)

- `user`: QEMU user-mode NAT. macOS 개발과 데모용이다. 리소스 없이 예약된 이름으로 쓴다.
- 브리지 네트워크(`Network` 리소스): 관리자가 미리 만든 Linux 브리지에 tap 장치를 연결한다. VLAN 태그는 선택 사항이고, 쓰려면 브리지가 VLAN-aware(`vlan_filtering 1`)여야 한다. tap은 그 VLAN만 태그 없이 주고받는다(pvid untagged, 브리지 기본 VLAN 제거).
- tap은 opld가 QEMU를 띄우기 전에 iproute2(`ip`, `bridge`)로 만들고, QEMU가 끝나면(정상 종료, 크래시, 강제 종료, 기동 실패) 지운다. 이름은 VM id의 뒷부분과 NIC 순번(`op<10자리>n<순번>`, 15자 이하)이라 opld가 다시 시작해도 VM의 tap을 찾을 수 있다. 그래서 Linux의 opld는 root(CAP_NET_ADMIN)로 돈다. VM 하나의 NIC는 16개까지다.
- 노드는 브리지 목록을 시작할 때와 1분마다 알리고(바뀔 때만 기록), 스케줄러가 쓴다.
- 브리지 생성과 관리, SDN, 방화벽은 하위 프로젝트 6에서 다룬다.

## 8. 클러스터 부트스트랩과 보안

```
node1$ oplctl cluster init --name prod          # 이 노드의 opld에 제어 소켓으로 요청한다
       → 클러스터 CA 생성, node1 인증서 발급, 1인 Raft 클러스터 시작, admin 계정 생성
       → admin 비밀번호(한 번만), CA 지문, 조인 URL 출력: opl-join://node1:8006?token=…&ca=sha256:…

node2$ oplctl cluster join 'opl-join://…'
       → CA 지문 검증 → 토큰으로 CSR 서명 받기 → learner로 합류 → 로그 따라잡은 뒤 voter로 승격
```

- 노드 인증서(`{data_dir}/pki`)가 없는 opld는 클러스터에 속하지 않은 노드다. 제어 소켓(`{run_dir}/opld.sock`, 0600)만 열고 `init`이나 `join`을 기다린다. init은 인증서를 마지막에 저장하므로, 도중에 실패하면 노드는 미초기화로 남고 남은 클러스터 저장소는 다음 시도에서 지운다. join은 learner가 되자마자(승격 전에) 저장하므로, 그 뒤에 실패한 노드는 멤버로 남고 다시 시작할 때마다 승격을 요청한다.
- 조인 토큰은 일회용이고 유효 시간이 정해져 있다(기본 1시간, 최대 7일). Raft에는 해시만 저장한다. 더 필요하면 관리자가 `oplctl cluster join-token`으로 만든다. 합류를 받는 노드는 토큰을 지워 한 번만 쓰게 하고, CSR에서 공개키만 써서 서명한다(이름, 용도, 기간은 서명하는 쪽이 정한다). 노드 인증서의 CN은 노드 id다.
- CA 개인키는 Raft에 복제한다(`Cluster` 리소스). 어느 노드든 합류를 받을 수 있고, 첫 노드가 죽어도 노드를 더할 수 있다. Proxmox(pmxcfs)와 같은 방식이다.
- 노드 간 통신(기본 8007)은 gRPC(tonic) over mTLS(rustls)다. `.proto` 대신 Rust 타입을 serde JSON으로 보내므로 `protoc`가 필요 없다. 서비스는 Raft RPC, 리더 전달, 생존 확인(Ping), 콘솔 중계다. 서버는 상대 인증서의 노드 id가 지금 멤버인지 확인해 제거된 노드를 막는다.
- 외부 API(기본 8006)는 클러스터 CA가 서명한 노드 인증서로 HTTPS를 연다. `oplctl`은 처음 로그인할 때 CA 지문(`--ca-fingerprint`)으로 서버를 확인하고 CA를 저장한다. 클라이언트 쪽 TLS는 OS 구현(native-tls)이다. macOS는 유효 기간이 825일을 넘는 서버 인증서를 거부하므로 노드 인증서는 824일짜리이고, 만료 전에 갱신해야 한다. 사용자 인증서로 교체하는 것은 나중에 한다. 세션 쿠키에는 `Secure`를 붙인다.
- 리더는 2초마다 다른 노드에 Ping을 보내 10초 동안 응답이 없으면 `NodeStatus.online`을 false로 기록한다(바뀔 때만 기록한다). 스케줄러가 이 값을 본다.
- `oplctl cluster leave`(정상 탈퇴)는 이 노드에 배정된 VM이 있거나 이 노드가 유일한 투표 멤버면 거부한다. Node 리소스를 지우고 멤버십에서 빠진 뒤, 클러스터 저장소와 노드 인증서를 지우고 미초기화 상태로 돌아간다. `oplctl cluster remove-node --force`(죽은 노드 제거)는 오프라인 노드만 뺀다. 멤버십 변경은 openraft의 joint consensus로 처리한다.
- 2노드 클러스터는 만들 수 있지만 `oplctl cluster status`가 경고를 표시한다. 위트니스는 하위 프로젝트 5에서 다룬다.

## 9. 범위 밖 (1차)

HA와 펜싱, 라이브 마이그레이션, 스냅샷과 백업, 공유 스토리지, 브리지 관리와 SDN, 웹 UI, LDAP/SSO, 세분화된 RBAC, 라이선스 시스템, 설치 ISO, 메트릭 수집과 저장.

## 10. 테스트 전략

| 계층 | 대상 | 도구 | 환경 |
|---|---|---|---|
| 단위 | 커맨드라인 생성, QMP 파싱, 스케줄러, 상태 머신 적용 | `cargo test`, `insta` | macOS, CI |
| 컴포넌트 | reconciler + Mock 드라이버: 이벤트 누락·중복·순서 바뀜 | tokio 테스트, 짧게 줄인 시간 설정 | macOS, CI |
| 클러스터 | 한 프로세스 안의 3노드(인메모리 네트워크), 리더 장애, 네트워크 분할 | openraft 테스트 하네스 | macOS, CI |
| 통합 | 실제 QEMU: 기동, QMP, 종료, opld 재시작 후 재연결 | `--features qemu-it` | macOS(hvf), Linux(KVM/TCG) |
| 통합 | 실제 tap·브리지·VLAN, QEMU NIC 연결과 정리, systemd 스코프 | `--features qemu-it,net-it`(root), `OPL_TEST_SYSTEMD=1` | Linux |
| E2E | oplctl로 VM 생성 → 콘솔 → opld kill -9 → 재연결 (opld 프로세스 + oplctl 라이브러리) | `--features qemu-it` | macOS(hvf) (M5) |
| E2E | oplctl로 클러스터 구성 → 아무 노드로 쓰기 → 리더 kill -9 → 노드 제거·탈퇴·재합류 (opld 프로세스 3개) | `cargo test` | macOS, CI (M6) |
| E2E | oplctl로 네트워크 생성 → 브리지 NIC VM 시작 → tap·VLAN 확인 → 끄면 tap 제거, 위 E2E를 Linux에서 | `--features qemu-it,net-it`(root) | Linux: Docker 컨테이너(TCG), CI(KVM) (M7) |

반드시 회귀 테스트로 고정할 불변식:

1. API가 성공(2xx)을 반환했다면 해당 변경은 Raft에 커밋된 것이다.
2. `opld`를 재시작해도(`kill -9` 포함) 실행 중인 VM은 중단되지 않고, 재시작 후 다시 연결된다.
3. 같은 VM에 대한 드라이버 호출은 동시에 실행되지 않는다.
4. Raft 과반수를 잃어도 실행 중인 VM은 중지되지 않는다.

## 11. 마일스톤

| M | 결과물 | 완료 기준 |
|---|---|---|
| M0 | 워크스페이스 뼈대, CI(fmt/clippy/test), `opl-types` | macOS에서 `cargo test` 통과 |
| M1 | `opl-qemu`: QMP 클라이언트 + 커맨드라인 생성기 | macOS에서 aarch64 게스트 기동, 상태 조회, 종료 |
| M2 | `opl-driver`: 트레이트 + Mock + QemuHvf + `dir` 풀 | Volume 생성, VMDK 가져오기 통합 테스트 통과 |
| M3 | `opl-cluster`: openraft + redb, 리소스 CRUD, watch | 3노드 인메모리 클러스터에서 리더 장애 후에도 쓰기 지속 |
| M4 | `opl-reconciler`: 조정 루프, VM별 큐, 재연결 | `opld` kill -9 후 VM 유지와 재연결 확인 |
| M5 | `opl-api` + `oplctl`: REST, Task, 콘솔, 토큰 인증 | CLI로 VM 생성부터 콘솔 접속까지 수행 |
| M6 | 클러스터 부트스트랩: CA, join, leave, mTLS | macOS에서 프로세스 3개로 클러스터 구성 |
| M7 | `QemuKvm` + bridge 네트워크 | Linux에서 E2E 통과 |

M0부터 M6까지는 macOS에서 진행한다. M7은 Linux에서 확인한다. 개발 Mac(M1)은 중첩 가상화가 없어 로컬은 privileged Docker 컨테이너에서 TCG로, KVM은 CI(GitHub Linux 러너)에서 확인한다.

## 12. 주요 의존성

| 용도 | 크레이트 |
|---|---|
| 비동기 런타임 | `tokio` |
| HTTP/WebSocket | `axum`, `tower` |
| 노드 간 RPC | `tonic`(수동 서비스 정의, serde JSON 메시지) |
| 합의 | `openraft` |
| 내장 저장소 | `redb` |
| TLS/PKI | `rustls`(ring), `rcgen`, 클라이언트는 `native-tls` |
| 직렬화 | `serde`, `serde_json`, `serde_yaml` |
| OpenAPI | `utoipa` |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) |
| CLI | `clap` |
| 로깅 | `tracing`, `tracing-subscriber` |
| 비밀번호 해시 | `argon2` |
| 테스트 | `insta`, `proptest` |

라이선스 정책: 공개 코어는 **Apache-2.0**으로 배포한다(2026-10-20 결정). 의존성은 Apache-2.0과 호환되는 라이선스(MIT, BSD, ISC 등)만 허용하고, CI에서 `cargo-deny`로 검사한다. QEMU(GPL)는 별도 프로세스로 실행하므로 링크하지 않는다.
````

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace --all-features && cargo deny check`
Expected: macOS에서 PASS(`net-it` 테스트는 Linux에서만 컴파일된다)

Run: `cargo test --workspace --all-features`
Expected: Linux 컨테이너(아래 '실행 환경')에서 root로 PASS

- [ ] **Step 3: 커밋**

```bash
git add .github README.md docs crates
git commit -m "ci, docs: Linux KVM 작업과 M7 문서"
```

---

## 완료 기준 (M7)

- Linux에서 opld가 KVM(없으면 TCG)으로 VM을 돌리고, 브리지 네트워크를 쓰는 VM의 tap이 VLAN과 함께 브리지에 붙었다가 VM을 끄면 사라진다 (`cargo test -p opld --features qemu-it,net-it --test bridge`, root).
- CI `linux-kvm` 작업이 실제 KVM과 systemd 스코프로 QEMU·브리지·E2E 테스트를 통과한다.
- macOS에서 기존 테스트(실제 QEMU hvf 포함)가 그대로 통과한다.
- `cargo fmt`, `clippy -D warnings`, `cargo test --workspace --all-features`, `cargo deny`가 통과한다.

## 알고 받아들인 제한

- **권한:** opld와 QEMU가 root로 돈다. QEMU 권한 낮추기는 나중에 한다.
- **브리지 관리:** 브리지는 관리자가 만든다. 브리지를 지우거나 VLAN-aware를 끄면 실행 중인 VM은 그대로이고, 1분 안에 노드 status만 바뀐다. tap의 MTU는 맞추지 않는다(브리지 MTU가 1500보다 크면 직접 맞춘다).
- **네트워크 수정 없음:** 네트워크의 브리지나 VLAN을 바꾸려면 쓰는 VM을 지운 뒤 다시 만든다. 네트워크 삭제와 다른 노드의 VM 생성 사이 경합은 막지 않는다(풀과 같다).
- **노드를 직접 정한 VM:** `spec.node`를 준 VM은 브리지를 확인하지 않고 배치된다. 그 노드에 브리지가 없으면 시작이 영구 오류(`InvalidSpec`)로 실패한다.
- **머신 타입 기본값:** `oplctl vm create`는 `--machine`이 없으면 oplctl이 도는 머신의 아키텍처로 정한다. Mac에서 x86_64 노드용 VM을 만들 때는 `--machine q35`를 준다.
- **Linux aarch64 KVM:** CI는 x86_64만 돌린다. aarch64 Linux는 TCG(컨테이너)로만 확인했다.
- **systemd 스코프:** 자원 제한을 걸지 않는다. 같은 이름의 스코프가 남아 있으면(QEMU 프로세스가 남은 경우) 시작이 실패한다.
- **tap 정리:** 지우기 실패는 기록만 한다. VM을 끌 때마다 `ip -j link show`로 그 VM의 tap을 찾는다.
- **로컬 검증:** 개발 Mac에서는 KVM을 쓸 수 없어 Linux 테스트를 Docker 컨테이너의 TCG로 돌린다.
