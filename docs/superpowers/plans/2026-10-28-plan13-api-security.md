# opLite 계획 13: API·보안 보완 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 계획 5·8·9에서 넘긴 API·보안 사항을 마친다. 로그인과 현재 비밀번호 확인에 시도 제한을 두고, 비밀번호가 바뀌면 모든 노드의 옛 세션을 끊으며, 업로드 전에 거절 이유를 알려 주는 확인 API를 더하고, 노드 아키텍처를 API로 알려 머신 기본값과 배치에 쓴다.

**Architecture:** `opl-api/src/limit.rs`의 `AttemptLimiter`가 노드마다 메모리에서 (사용자 이름, IP)와 IP 단위로 실패를 센다(세션도 노드별이다). `begin`이 막힘 확인과 미리 세기를 한 번에 해서 동시 요청이 빠져나가지 못한다. 세션은 만들 때의 `UserSpec.session_epoch`를 기억하고, 요청마다 Raft에서 다시 읽는 사용자의 세대와 비교해 비밀번호 변경 뒤의 옛 세션을 모든 노드에서 끊는다. 업로드 확인은 `check_import` 하나를 확인 API와 실제 업로드가 함께 쓴다. `Arch`를 `opl-types`로 옮겨 `NodeStatus.arch`로 알리고, 스케줄러·VM 검증(`check_node_arch`)·oplctl·웹이 머신 종류와 맞춘다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft, axum 0.8, utoipa, clap, argon2; 웹은 계획 8~12와 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW). 새 의존성은 없다(`opl-api`의 dev-dependency `tokio`에 `test-util` 기능만 켠다).

**설계 문서:** `docs/superpowers/specs/2026-10-28-api-security-design.md` (전체가 이 계획)
**선행 계획:** 계획 1~12 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 로그인·현재 비밀번호 확인의 시도 제한(노드별, 사용자+IP 5번·IP 20번, 429 `too_many_attempts`와 `Retry-After`) | 클러스터 전체에서 세는 시도 제한(실패마다 Raft에 쓰면 공격이 곧 쓰기 부하다) |
| 비밀번호가 바뀌면 모든 노드의 옛 세션 끊기(`session_epoch`) | 프록시 헤더(`X-Forwarded-For`) 신뢰, 비밀번호 변경과 함께 API 토큰·열린 콘솔 끊기, 감사 로그 |
| 업로드 사전 확인 API `POST /volumes/import/check`, oplctl·웹이 먼저 확인, 웹의 2분 처리 대기 | 업로드를 풀이 있는 노드로 대신 전달하기 |
| `NodeStatus.arch`, 스케줄러와 지정 노드의 아키텍처 검증(`machine_arch_mismatch`), 웹·oplctl 머신 기본값, 노드 목록의 아키텍처 열 | 작업 목록 페이지 나누기, `index.html` ETag, VM 폼 나누기, 번역 키 `common.*`, 대화상자 뒤 포커스, 남은 전원 Task로 버튼 미리 막기 — 계획 14 |
| | 공유 스토리지(NFS·LVM·ZFS·iSCSI·Ceph) — 계획 14 뒤 |

## 이전 계획에서 이어받는 사항

- 계획 5·6: 로그인 시도 제한, 비밀번호 변경 시 세션 처리(Task 2·3).
- 계획 8·9: 노드 아키텍처를 API로 알리기(머신 기본값, Task 1·5·6·7), 업로드 사전 확인 API와 처리 중 응답 제한 시간(Task 4·7), 현재 비밀번호 확인의 시도 제한(Task 2).
- 계획 12에서 넘긴 웹 정리 항목은 계획 14다.

## 실행 환경

계획 12와 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 실제 QEMU 테스트는 기능으로 켠다: `opld`의 `qemu-it`(`smoke`·`e2e`), `opl-api`의 `storage-it`, `opld`의 `web-it`(브라우저 E2E). Linux CI(`linux-kvm`)는 QEMU 8.2다. 이 계획은 새 QEMU 옵션을 쓰지 않는다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1–4). 커밋한다. `Cargo.lock`은 바뀌지 않는다.

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **시도 제한.** `AttemptLimiter`는 `AppState`에 노드마다 하나다. 노드가 N개면 공격자의 시도도 N배지만 이 규모(노드 3–5개)에서 받아들인다. 키는 (소문자 사용자 이름, IP)와 IP. 연속 5번·20번 실패하면 30초 잠그고, 잠긴 뒤에도 실패가 이어지면 30초 × 2^(실패 − 한도)로 늘려 최대 15분. 마지막 실패 뒤 15분이 지난 기록은 지운다. 기록은 1만 개를 넘지 않는다(잠기지 않은 것 가운데 오래된 것부터). IPv6는 /64, IPv4-mapped는 IPv4로 센다. 값은 상수다.
- **미리 세기.** `begin(user, ip)`가 한 잠금 안에서 막힘을 확인하고 이번 시도를 실패로 센다. argon2를 기다리는 동안 들어온 동시 요청도 한도를 넘으면 429다. 성공하면 (사용자, IP) 기록을 지우고 IP 기록에서는 이번 시도 하나만 뺀다(자기 계정으로 로그인해 IP 기록을 지우는 우회를 막는다). 막혀 있으면 해시 계산 없이 429 `too_many_attempts`, `Retry-After`, 메시지 "try again in N seconds"이고, 429는 기록하지 않는다.
- **적용 대상.** `POST /auth/login`(없는 사용자도 실패로 센다, 이름 형식이 아닌 이름은 해시 없이 IP 키만 세고 401)과 `PUT /users/{key}`에서 자기 비밀번호를 바꿀 때의 현재 비밀번호 확인(같은 기록, 새 비밀번호 길이 확인 뒤).
- **클라이언트 IP.** `ClientIp` 추출기가 `ConnectInfo<SocketAddr>`를 읽는다(axum 0.8은 `Option<ConnectInfo>`를 handler 인자로 받지 못한다). opld는 `TlsListener`를 `tap_io(|_| {})`로 감싸 axum이 연결 주소를 넘기게 한다. 주소가 없으면(테스트 라우터를 `oneshot`으로 부를 때) `0.0.0.0` 하나로 세고 처음 한 번 경고한다. 프록시 헤더는 믿지 않는다.
- **세션 세대.** `UserSpec.session_epoch: u64`(0이면 쓰지 않는다 — `UserSpec`는 모르는 필드를 거절한다). 비밀번호가 바뀌는 쓰기에서만 1 올린다. 세션은 만들 때의 세대를 저장하고, 쿠키 인증에서 다르면 세션을 지우고 401 `unauthorized`. API 토큰과 열린 WebSocket 콘솔은 그대로다.
- **업로드 확인.** `check_import`가 권한 → 이름 → 풀 → 이 노드의 풀 → 이름 중복 → 공간 순서로 확인한다. 확인 API는 204, 업로드는 `Content-Length`가 먼저(없으면 411) 필요하다. 확인과 업로드 사이의 경쟁은 Raft 만들기의 `name_taken`이 막는다. 웹은 확인 거절이면 XHR을 만들지 않고, 업로드 본문을 다 보낸 뒤 2분 동안 응답이 없으면 알림만 바꾼다(XHR을 끊으면 서버의 `UploadGuard`가 볼륨을 지운다).
- **아키텍처.** `NodeStatus.arch: Option<Arch>`. 스케줄러는 arch가 머신과 같은 노드만 고르고 모르는 노드는 빼며, 지정 노드는 arch를 알고 다를 때만 400 `machine_arch_mismatch`(`check_node_arch`: `prepare`와 시작). 메모리 스냅샷 되돌리기가 이 노드에 고정할 때 arch가 다르면 Task를 실패로 끝낸다. oplctl과 웹은 지정 노드의 arch → 아는 노드가 모두 같은 arch 순으로 기본값을 정하고, 정할 수 없으면 oplctl은 `--machine`을 요구하고 웹은 `q35`를 고른다.
- **섞인 클러스터.** 옛 버전과 섞인 클러스터는 지원하지 않는다(모든 노드를 함께 올린다).

## 파일 구조

```
crates/opl-types/src/{vm.rs, node.rs, auth.rs}        # Arch, MachineType::arch, NodeStatus.arch, UserSpec.session_epoch
crates/opl-qemu/src/{host.rs, lib.rs, cmdline.rs}     # Arch를 opl-types에서
crates/opl-api/src/limit.rs(새로)                     # AttemptLimiter, ClientIp
crates/opl-api/src/{auth.rs, error.rs, lib.rs, state.rs, users.rs}   # 세션 세대, Retry-After, 시도 제한 적용
crates/opl-api/src/{volumes.rs, scheduler.rs, vms.rs, tasks/snapshot.rs}   # check_import, 아키텍처 배치·검증
crates/opl-api/tests/{auth.rs, import_check.rs(새로), vms.rs, snapshot_tasks.rs, openapi.rs, two_nodes.rs, common/mod.rs}
crates/opld/src/{bootstrap.rs, member.rs}             # arch 등록, 연결 주소 넘기기
crates/oplctl/src/cli.rs                              # 업로드 먼저 확인, 머신 기본값
web/src/storage/upload.ts, web/src/vms/{form.ts, VmFormPage.tsx}, web/src/nodes/NodesPage.tsx, web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/2026-10-28-api-security-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입: 아키텍처, 노드 arch, 세션 세대

`Arch { X86_64, Aarch64 }`(serde `x86_64`/`aarch64`, OpenAPI 스키마)와 `native()`(컴파일된 아키텍처)·`machine()`을 `opl-qemu`의 `host.rs`에서 `opl-types::vm`으로 옮기고, `opl-qemu`와 드라이버·reconciler 테스트는 `opl_types::vm::Arch`를 쓴다. `MachineType::arch()`(`q35` → `x86_64`, `virt` → `aarch64`)를 둔다. `NodeStatus.arch: Option<Arch>`(구조체 전체가 `#[serde(default)]`라 옛 노드는 `None`, `NodeStatus`는 모르는 필드를 받는다)를 더하고 노드를 만드는 곳은 모두 `None`으로 둔다(채우기는 Task 5). `UserSpec.session_epoch: u64`(`#[serde(default)]`, `UserSpec`는 `deny_unknown_fields`라 0이면 쓰지 않는다 — `skip_serializing_if`)를 더하고 쓰는 곳은 0. 직렬화·옛 형식 읽기 테스트. 웹 타입을 다시 만들고 테스트 노드 픽스처에 `arch: null`을 넣는다.

**Files:**
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/tests/qemu_bridge.rs`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-qemu/src/cmdline.rs`
- Modify: `crates/opl-qemu/src/host.rs`
- Modify: `crates/opl-qemu/src/lib.rs`
- Modify: `crates/opl-qemu/tests/qemu_boot.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-types/src/auth.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
            bridges: vec![],
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            arch: None,
        };
```

`crates/opl-api/src/users.rs`에서

```rust
            role: req.role,
        },
```

을 다음으로 바꾼다:

```rust
            role: req.role,
            session_epoch: 0,
        },
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            bridges: vec![],
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            arch: None,
        };
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
                role: Role::Admin,
            },
```

을 다음으로 바꾼다:

```rust
                role: Role::Admin,
                session_epoch: 0,
            },
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            bridges: vec![],
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            arch: None,
        };
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            role: Role::Admin,
        },
```

을 다음으로 바꾼다:

```rust
            role: Role::Admin,
            session_epoch: 0,
        },
```

`crates/opl-driver/src/qemu.rs`에서

```rust

    use opl_qemu::{Accel, Arch};
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{CpuModel, CpuSpec, MachineType, NicModel};
```

을 다음으로 바꾼다:

```rust

    use opl_qemu::Accel;
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{Arch, CpuModel, CpuSpec, MachineType, NicModel};
```

`crates/opl-driver/tests/qemu_bridge.rs`에서

```rust
use opl_driver::{EventKind, Hypervisor, IpRoute, QemuConfig, QemuHypervisor, StopMode};
use opl_qemu::{Arch, HostProfile};
use opl_types::network::tap_name;
use opl_types::runtime::{NicBackend, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, Firmware, NicModel};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_driver::{EventKind, Hypervisor, IpRoute, QemuConfig, QemuHypervisor, StopMode};
use opl_qemu::HostProfile;
use opl_types::network::tap_name;
use opl_types::runtime::{NicBackend, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, Firmware, NicModel};
use uuid::Uuid;
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
};
use opl_qemu::{Arch, HostProfile};
use opl_types::runtime::{ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware};
use tokio::sync::broadcast;
```

을 다음으로 바꾼다:

```rust
};
use opl_qemu::HostProfile;
use opl_types::runtime::{ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use tokio::sync::broadcast;
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
use opl_types::runtime::{ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, Arch, HostProfile};
use crate::paths::RuntimePaths;
```

을 다음으로 바꾼다:

```rust
use opl_types::runtime::{ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, HostProfile};
use crate::paths::RuntimePaths;
```

`crates/opl-qemu/src/host.rs`에서

```rust

use opl_types::vm::MachineType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}
```

을 다음으로 바꾼다:

```rust

use opl_types::vm::Arch;
```

`crates/opl-qemu/src/host.rs`에서

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

을 다음으로 바꾼다:

```rust
    Tcg,
}
```

`crates/opl-qemu/src/lib.rs`에서

```rust
pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, Arch, HostProfile, kvm_available};
pub use paths::{PathError, RuntimePaths};
```

을 다음으로 바꾼다:

```rust
pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, HostProfile, kvm_available};
pub use paths::{PathError, RuntimePaths};
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust

use opl_qemu::{Arch, HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware, NicModel};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust

use opl_qemu::{HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware, NicModel};
use uuid::Uuid;
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
            firmware: Firmware::Uefi,
            machine: opl_qemu::Arch::native().machine(),
            disks: vec![],
```

을 다음으로 바꾼다:

```rust
            firmware: Firmware::Uefi,
            machine: opl_types::vm::Arch::native().machine(),
            disks: vec![],
```

`crates/opl-types/src/auth.rs`에서

```rust
    pub role: Role,
}
```

을 다음으로 바꾼다:

```rust
    pub role: Role,
    /// 비밀번호가 바뀔 때마다 1 올린다. 세션은 만들 때의 값과 다르면 끊긴다.
    /// 0이면 쓰지 않는다(이 필드를 모르는 옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_epoch: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
```

`crates/opl-types/src/auth.rs`에서

```rust
pub struct ApiTokenStatus {}
```

을 다음으로 바꾼다:

```rust
pub struct ApiTokenStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_session_epoch_defaults_to_zero() {
        // 옛 사용자에는 session_epoch가 없다.
        let old: UserSpec = serde_json::from_value(serde_json::json!({
            "password_hash": "h",
            "role": "admin"
        }))
        .unwrap();
        assert_eq!(old.session_epoch, 0);
        let user = UserSpec {
            session_epoch: 3,
            ..old
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(json["session_epoch"], 3);
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn zero_session_epoch_is_not_written() {
        // 옛 노드의 UserSpec은 모르는 필드를 거절하므로, 세대가 0이면 쓰지 않는다.
        let user = UserSpec {
            password_hash: "h".into(),
            role: Role::Admin,
            session_epoch: 0,
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "h", "role": "admin" })
        );
    }
}
```

`crates/opl-types/src/node.rs`에서

```rust
use crate::resource::{Kind, Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::Arch;
```

`crates/opl-types/src/node.rs`에서

```rust
    pub bridges: Vec<BridgeInfo>,
}
```

을 다음으로 바꾼다:

```rust
    pub bridges: Vec<BridgeInfo>,
    /// 이 노드의 CPU 아키텍처. 옛 버전 노드는 기록하지 않아 None이다.
    pub arch: Option<Arch>,
}
```

`crates/opl-types/src/node.rs`에서

```rust
    }
}
```

을 다음으로 바꾼다:

```rust
    }

    #[test]
    fn status_arch_is_optional() {
        // 옛 노드는 arch를 기록하지 않았다.
        let old: NodeStatus =
            serde_json::from_value(serde_json::json!({ "online": true, "cpus": 2 })).unwrap();
        assert_eq!(old.arch, None);
        let status = NodeStatus {
            arch: Some(Arch::X86_64),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["arch"], "x86_64");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }
}
```

`crates/opl-types/src/vm.rs`에서

```rust
    Virt,
}
```

을 다음으로 바꾼다:

```rust
    Virt,
}

impl MachineType {
    /// 이 머신 타입을 쓸 수 있는 아키텍처
    pub fn arch(self) -> Arch {
        match self {
            MachineType::Q35 => Arch::X86_64,
            MachineType::Virt => Arch::Aarch64,
        }
    }
}

/// CPU 아키텍처. 노드가 등록할 때 기록하고, 머신 타입마다 하나로 정해진다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    X86_64,
    Aarch64,
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

`crates/opl-types/src/vm.rs`에서

```rust
    #[test]
    fn deserializes_with_defaults() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn arch_serializes_as_lowercase_and_pairs_with_machine() {
        assert_eq!(serde_json::to_value(Arch::X86_64).unwrap(), "x86_64");
        assert_eq!(serde_json::to_value(Arch::Aarch64).unwrap(), "aarch64");
        assert_eq!(
            serde_json::from_value::<Arch>(serde_json::json!("aarch64")).unwrap(),
            Arch::Aarch64
        );
        for arch in [Arch::X86_64, Arch::Aarch64] {
            assert_eq!(arch.machine().arch(), arch);
        }
        assert_eq!(MachineType::Q35.arch(), Arch::X86_64);
        assert_eq!(MachineType::Virt.arch(), Arch::Aarch64);
    }

    #[test]
    fn deserializes_with_defaults() {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            role: Role::Admin,
        },
```

을 다음으로 바꾼다:

```rust
            role: Role::Admin,
            session_epoch: 0,
        },
```

`crates/opld/src/bootstrap.rs`에서

```rust
        bridges: host.bridges().await.unwrap_or(recorded),
    };
```

을 다음으로 바꾼다:

```rust
        bridges: host.bridges().await.unwrap_or(recorded),
        arch: None,
    };
```

`web/src/test/fixtures.ts`에서

```ts
  spec: { address: "10.0.0.1", maintenance: false },
  status: { online: true, cpus: 8, memory_mib: 16384, hypervisor: "qemu-kvm", bridges: [] },
};
```

을 다음으로 바꾼다:

```ts
  spec: { address: "10.0.0.1", maintenance: false },
  status: {
    online: true,
    cpus: 8,
    memory_mib: 16384,
    hypervisor: "qemu-kvm",
    bridges: [],
    arch: null,
  },
};
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 362개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS 37개

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 229개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): Arch를 opl-types로 옮기고 노드 arch·사용자 세션 세대 필드 추가"
```

---

### Task 2: 로그인과 현재 비밀번호 확인의 시도 제한

새 `opl-api/src/limit.rs`의 `AttemptLimiter`(노드마다 `AppState.attempts` 하나): (소문자 사용자 이름, IP) 키는 연속 5번, IP 키는 연속 20번 실패하면 30초 잠그고, 이어진 실패마다 두 배(30초 × 2^(실패 − 한도), 최대 15분). 마지막 실패 뒤 15분이 지난 기록은 지우고, 기록은 1만 개를 넘지 않는다(넘치면 잠기지 않은 것 가운데 마지막 시도가 가장 오래된 것부터, 그다음 잠긴 것). IPv6는 /64로, IPv4-mapped는 IPv4로 센다. `begin(user, ip)`가 막힘 확인과 실패로 미리 세기를 한 번에 한다(동시 요청이 argon2를 기다리는 동안 모두 통과하지 못하게). 막혀 있으면 해시 계산 없이 429 `too_many_attempts`(`ApiError.retry_after` → `Retry-After` 헤더, 메시지 "try again in N seconds")이고 기록하지 않는다. `success(user, ip)`는 (사용자, IP) 기록을 지우고 IP 기록에서는 그 시도 하나만 뺀다(자기 계정 로그인으로 IP 기록을 지우지 못하게). 이름 형식이 아닌 사용자 이름은 해시 없이 `begin_ip`로 IP 키만 세고 401. `POST /auth/login`(없는 사용자도 실패)과 자기 비밀번호 변경의 현재 비밀번호 확인(같은 기록, 새 비밀번호 길이 확인 뒤)에 적용한다. 클라이언트 IP는 `ClientIp` 추출기(axum 0.8은 `Option<ConnectInfo>`를 받지 못한다)가 `ConnectInfo<SocketAddr>`에서 읽고 없으면 `0.0.0.0`(처음 한 번 경고). opld는 TLS 리스너를 `tap_io`로 감싸 `into_make_service_with_connect_info::<SocketAddr>()`로 서빙하고, API 테스트 서버도 연결 주소를 넘긴다. 단위 테스트는 멈춘 시계(`start_paused`, dev-dependency `tokio` `test-util`), API 테스트는 6번째 429와 헤더, 막힌 동안 맞는 비밀번호도 429, 동시 요청 20개, 성공이 IP 기록에서 자기 시도만 빼기, 이름 형식이 아닌 이름, 현재 비밀번호 확인.

**Files:**
- Create: `crates/opl-api/src/limit.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/error.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/auth.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
tempfile.workspace = true
tokio-tungstenite.workspace = true
```

을 다음으로 바꾼다:

```toml
tempfile.workspace = true
tokio = { workspace = true, features = ["test-util"] }
tokio-tungstenite.workspace = true
```

`crates/opl-api/src/error.rs`에서

```rust
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
```

을 다음으로 바꾼다:

```rust
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
```

`crates/opl-api/src/error.rs`에서

```rust
    pub message: String,
    pub details: Option<Value>,
}
```

을 다음으로 바꾼다:

```rust
    pub message: String,
    pub details: Option<Value>,
    /// `Retry-After` 헤더로 보낼 초(429)
    pub retry_after: Option<u64>,
}
```

`crates/opl-api/src/error.rs`에서

```rust
            details: None,
        }
```

을 다음으로 바꾼다:

```rust
            details: None,
            retry_after: None,
        }
```

`crates/opl-api/src/error.rs`에서

```rust
        };
        (self.status, axum::Json(body)).into_response()
    }
```

을 다음으로 바꾼다:

```rust
        };
        let mut response = (self.status, axum::Json(body)).into_response();
        if let Some(secs) = self.retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
```

`crates/opl-api/src/lib.rs`에서

```rust
mod janitor;
pub mod resources;
```

을 다음으로 바꾼다:

```rust
mod janitor;
pub mod limit;
pub mod resources;
```

`crates/opl-api/src/limit.rs`를 만든다:

```rust
//! 로그인과 현재 비밀번호 확인의 시도 제한(설계 2절). 노드 메모리에만 두고 노드마다 따로 센다.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex, Once};
use std::time::Duration;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use tokio::time::Instant;

use crate::error::ApiError;

/// (사용자 이름, IP) 키가 막히기까지의 연속 실패 수
pub const USER_FAILURES: u32 = 5;
/// IP 키가 막히기까지의 연속 실패 수
pub const IP_FAILURES: u32 = 20;
/// 처음 막는 시간. 이어진 실패마다 두 배가 된다.
pub const FIRST_LOCK: Duration = Duration::from_secs(30);
pub const MAX_LOCK: Duration = Duration::from_secs(15 * 60);
/// 마지막 시도 뒤 이만큼이 넘게 지난 기록은 지운다. 최대 잠금과 같아서, 막 풀린 뒤의 실패는 이어서 센다.
pub const IDLE: Duration = Duration::from_secs(15 * 60);
pub const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    User(String, IpAddr),
    Ip(IpAddr),
}

impl Key {
    fn limit(&self) -> u32 {
        match self {
            Key::User(..) => USER_FAILURES,
            Key::Ip(_) => IP_FAILURES,
        }
    }
}

struct Entry {
    failures: u32,
    locked_until: Option<Instant>,
    last: Instant,
}

/// 실패한 시도를 세어 막는다. 시계는 `tokio::time::Instant`라 테스트가 시간을 멈추고 당길 수 있다.
#[derive(Default)]
pub struct AttemptLimiter {
    entries: Mutex<HashMap<Key, Entry>>,
}

fn keys(user: &str, ip: IpAddr) -> [Key; 2] {
    let ip = counted_ip(ip);
    [Key::User(user.to_lowercase(), ip), Key::Ip(ip)]
}

/// 세는 단위의 IP. IPv4-mapped IPv6는 IPv4로 보고, IPv6는 /64 네트워크 하나로 센다(한 사용자가
/// 쉽게 바꿔 쓸 수 있는 주소 묶음이다).
fn counted_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        v4 => v4,
    }
}

/// 막힌 키가 있으면 풀릴 때까지 남은 시간(가장 긴 것)
fn wait(map: &HashMap<Key, Entry>, keys: &[Key], now: Instant) -> Option<Duration> {
    keys.iter()
        .filter_map(|k| map.get(k)?.locked_until)
        .filter(|until| *until > now)
        .map(|until| until - now)
        .max()
}

impl AttemptLimiter {
    /// 막혀 있으면 풀릴 때까지 남은 시간
    pub fn blocked(&self, user: &str, ip: IpAddr) -> Option<Duration> {
        wait(
            &self.entries.lock().unwrap(),
            &keys(user, ip),
            Instant::now(),
        )
    }

    /// 시도를 시작한다. 막혀 있으면 기록하지 않고 429 `too_many_attempts`. 아니면 이 시도를 미리
    /// 실패로 센다(한도에 닿으면 잠근다). 확인을 기다리는 동안 들어온 다른 시도가 이 실패를 보고
    /// 막히도록 확인과 세기를 한 번에 한다. 실패하면 따로 기록하지 않고, 성공하면 `success`를 부른다.
    pub fn begin(&self, user: &str, ip: IpAddr) -> Result<(), ApiError> {
        self.begin_keys(&keys(user, ip))
    }

    /// 사용자 이름 없이 IP 키만 세는 `begin`. 있을 수 없는 이름의 로그인에 쓴다(기록에 그 이름을 넣지 않는다).
    pub fn begin_ip(&self, ip: IpAddr) -> Result<(), ApiError> {
        self.begin_keys(&[Key::Ip(counted_ip(ip))])
    }

    fn begin_keys(&self, keys: &[Key]) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut map = self.entries.lock().unwrap();
        if let Some(wait) = wait(&map, keys, now) {
            return Err(too_many_attempts(wait));
        }
        map.retain(|_, e| now.duration_since(e.last) <= IDLE);
        for key in keys {
            let limit = key.limit();
            let e = map.entry(key.clone()).or_insert(Entry {
                failures: 0,
                locked_until: None,
                last: now,
            });
            e.failures += 1;
            e.last = now;
            if e.failures >= limit {
                // 한도를 넘은 실패마다 두 배. 2^6 * 30초가 이미 15분을 넘는다.
                let lock = FIRST_LOCK * 2u32.pow((e.failures - limit).min(6));
                e.locked_until = Some(now + lock.min(MAX_LOCK));
            }
        }
        // 넘치면 잠기지 않은 것 가운데 마지막 시도가 가장 오래된 것부터 버린다. 잠긴 것은 그 다음이다
        // (기록을 채워 잠금을 풀지 못하게).
        while map.len() > MAX_ENTRIES {
            let oldest = map
                .iter()
                .min_by_key(|(_, e)| (e.locked_until.is_some_and(|until| until > now), e.last))
                .map(|(k, _)| k.clone())
                .expect("map is not empty");
            map.remove(&oldest);
        }
        Ok(())
    }

    /// 성공한 시도. 그 (사용자 이름, IP) 기록은 지우고, IP 기록에서는 `begin`이 미리 센 이 시도 하나만
    /// 뺀다. IP 기록을 지우지 않는 것은 자기 계정 로그인으로 다른 이름의 실패를 지우지 못하게 하려는 것이다.
    pub fn success(&self, user: &str, ip: IpAddr) {
        let [user_key, ip_key] = keys(user, ip);
        let mut map = self.entries.lock().unwrap();
        map.remove(&user_key);
        if let Some(e) = map.get_mut(&ip_key) {
            e.failures = e.failures.saturating_sub(1);
            // 이 시도로 한도에 닿아 걸린 잠금은 푼다. 한도 아래에서는 잠금이 생기지 않는다.
            if e.failures < IP_FAILURES {
                e.locked_until = None;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

fn too_many_attempts(wait: Duration) -> ApiError {
    // 남은 시간을 올림한 초. 0초로 알려 주지 않는다.
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_attempts",
        format!("too many failed attempts; try again in {secs} seconds"),
    );
    e.retry_after = Some(secs);
    e
}

/// 요청한 클라이언트의 IP(연결 주소). 연결 주소가 없으면(라우터를 바로 부를 때) `0.0.0.0` 하나로 센다.
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip())
                .unwrap_or_else(|| {
                    // 테스트 라우터를 바로 부를 때는 괜찮다. opld에서 보이면 리스너 설정이 빠진 것이다.
                    static WARNED: Once = Once::new();
                    WARNED.call_once(|| {
                        tracing::warn!("no client address; counting login attempts as 0.0.0.0")
                    });
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

    /// 실패한 시도 n번(막혀 있으면 안 된다)
    fn fail(l: &AttemptLimiter, user: &str, ip: IpAddr, n: u32) {
        for _ in 0..n {
            l.begin(user, ip).unwrap();
        }
    }

    /// i마다 다른 IPv4 주소
    fn nth_ip(i: usize) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + i as u32))
    }

    #[tokio::test(start_paused = true)]
    async fn locks_after_five_failures_for_thirty_seconds() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK));
        // 사용자 이름은 대소문자를 가리지 않고, 다른 사용자·다른 IP는 막히지 않는다.
        assert_eq!(l.blocked("ALICE", IP), Some(FIRST_LOCK));
        assert_eq!(l.blocked("bob", IP), None);
        assert_eq!(l.blocked("alice", OTHER_IP), None);

        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(l.blocked("alice", IP), Some(Duration::from_secs(1)));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(l.blocked("alice", IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn attempts_count_as_failures_when_they_begin() {
        // 확인이 끝나기 전의 시도도 센다. 동시에 들어온 시도는 다섯 번째까지만 통과한다.
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        let e = l.begin("alice", IP).unwrap_err();
        assert_eq!(e.code, "too_many_attempts");
    }

    #[tokio::test(start_paused = true)]
    async fn continued_failures_double_the_lock_up_to_fifteen_minutes() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        let mut expected = FIRST_LOCK;
        for _ in 0..8 {
            assert_eq!(l.blocked("alice", IP), Some(expected));
            tokio::time::advance(expected).await;
            assert_eq!(l.blocked("alice", IP), None);
            fail(&l, "alice", IP, 1);
            expected = (expected * 2).min(MAX_LOCK);
        }
        assert_eq!(l.blocked("alice", IP), Some(MAX_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_attempts_are_not_recorded() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 5);
        tokio::time::advance(Duration::from_secs(10)).await;
        // 막힌 동안 두드려도 잠금이 늘지 않는다.
        for _ in 0..10 {
            let e = l.begin("alice", IP).unwrap_err();
            assert_eq!(e.retry_after, Some(20));
        }
        tokio::time::advance(Duration::from_secs(20)).await;
        // 풀린 뒤의 실패는 여섯 번째다.
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK * 2));
    }

    #[tokio::test(start_paused = true)]
    async fn success_resets_the_user_record() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        fail(&l, "alice", IP, 1);
        l.success("alice", IP);
        // 처음부터 다시 센다.
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("alice", IP), Some(FIRST_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn success_takes_back_only_its_own_attempt_from_the_ip_record() {
        let l = AttemptLimiter::default();
        for i in 0..IP_FAILURES - 1 {
            fail(&l, &format!("user{i}"), IP, 1);
        }
        // 스무 번째 시도는 성공한다. 미리 센 실패와 그로 걸린 잠금을 되돌린다.
        fail(&l, "alice", IP, 1);
        l.success("alice", IP);
        assert_eq!(l.blocked("new-user", IP), None);
        // 앞의 실패 19번은 남아 있어 한 번 더 실패하면 막힌다.
        fail(&l, "other", IP, 1);
        assert_eq!(l.blocked("new-user", IP), Some(FIRST_LOCK));
    }

    #[tokio::test(start_paused = true)]
    async fn ip_is_locked_after_twenty_failures_across_users() {
        let l = AttemptLimiter::default();
        for i in 0..IP_FAILURES {
            assert_eq!(l.blocked("new-user", IP), None, "{i}");
            fail(&l, &format!("user{i}"), IP, 1);
        }
        assert_eq!(l.blocked("new-user", IP), Some(FIRST_LOCK));
        assert_eq!(l.blocked("new-user", OTHER_IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn begin_ip_counts_only_the_ip() {
        let l = AttemptLimiter::default();
        for _ in 0..IP_FAILURES - 1 {
            l.begin_ip(IP).unwrap();
        }
        assert_eq!(l.len(), 1);
        fail(&l, "alice", IP, 1);
        assert_eq!(l.blocked("bob", IP), Some(FIRST_LOCK));
        assert!(l.begin_ip(IP).is_err());
    }

    #[test]
    fn ipv6_is_counted_per_slash_64_and_mapped_ipv4_as_ipv4() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::2".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(counted_ip(a), "2001:db8:1:2::".parse::<IpAddr>().unwrap());
        assert_eq!(counted_ip(a), counted_ip(b));
        assert_ne!(counted_ip(a), counted_ip(c));
        let mapped: IpAddr = "::ffff:10.0.0.1".parse().unwrap();
        assert_eq!(counted_ip(mapped), IP);
        assert_eq!(counted_ip(IP), IP);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_records_are_dropped() {
        let l = AttemptLimiter::default();
        fail(&l, "alice", IP, 4);
        tokio::time::advance(IDLE + Duration::from_millis(1)).await;
        // 기록할 때 오래된 기록을 지우므로 alice의 실패는 1번부터 다시 센다.
        fail(&l, "bob", OTHER_IP, 1);
        assert_eq!(l.len(), 2);
        fail(&l, "alice", IP, 4);
        assert_eq!(l.blocked("alice", IP), None);
    }

    #[tokio::test(start_paused = true)]
    async fn record_count_is_capped_by_dropping_the_oldest_unlocked() {
        let l = AttemptLimiter::default();
        // (locked, IP)는 잠기고 IP 키는 실패 5번으로 잠기지 않았다. 둘 다 가장 오래된 기록이다.
        fail(&l, "locked", IP, USER_FAILURES);
        tokio::time::advance(Duration::from_millis(1)).await;
        // 사용자마다 IP가 달라 한 시도가 기록 두 개를 만든다.
        for i in 0..MAX_ENTRIES / 2 - 1 {
            fail(&l, &format!("user{i}"), nth_ip(i), 1);
        }
        assert_eq!(l.len(), MAX_ENTRIES);
        tokio::time::advance(Duration::from_millis(1)).await;
        fail(&l, "last", OTHER_IP, 1);
        assert_eq!(l.len(), MAX_ENTRIES);
        let map = l.entries.lock().unwrap();
        // 잠긴 기록은 남고, 잠기지 않은 것 가운데 가장 오래된 IP 키가 빠졌다.
        assert!(map.contains_key(&Key::User("locked".into(), IP)));
        assert!(!map.contains_key(&Key::Ip(IP)));
        assert!(map.contains_key(&Key::Ip(OTHER_IP)));
    }

    #[test]
    fn error_carries_retry_after_in_whole_seconds() {
        let e = too_many_attempts(Duration::from_millis(29_500));
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(e.code, "too_many_attempts");
        assert_eq!(e.retry_after, Some(30));
        assert!(
            e.message.contains("try again in 30 seconds"),
            "{}",
            e.message
        );
    }
}
```

`crates/opl-api/src/state.rs`에서

```rust
use crate::auth::{Authorizer, RoleAuthorizer, Sessions};
```

을 다음으로 바꾼다:

```rust
use crate::auth::{Authorizer, RoleAuthorizer, Sessions};
use crate::limit::AttemptLimiter;
```

`crates/opl-api/src/state.rs`에서

```rust
    pub(crate) sessions: Sessions,
    /// 이 노드에서 VM spec의 검증과 쓰기를 한 번에 하나씩 한다(MAC·볼륨 중복 확인이 경합하지 않게).
```

을 다음으로 바꾼다:

```rust
    pub(crate) sessions: Sessions,
    /// 로그인과 현재 비밀번호 확인의 실패 기록(노드마다 따로)
    pub(crate) attempts: AttemptLimiter,
    /// 이 노드에서 VM spec의 검증과 쓰기를 한 번에 하나씩 한다(MAC·볼륨 중복 확인이 경합하지 않게).
```

`crates/opl-api/src/state.rs`에서

```rust
            sessions: Sessions::default(),
            vm_lock: tokio::sync::Mutex::new(()),
```

을 다음으로 바꾼다:

```rust
            sessions: Sessions::default(),
            attempts: AttemptLimiter::default(),
            vm_lock: tokio::sync::Mutex::new(()),
```

`crates/opl-api/src/users.rs` 전체:

```rust
//! 로그인, 사용자, API 토큰. 비밀번호 해시와 토큰 해시는 응답에 넣지 않는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{
    Action, Principal, SESSION_COOKIE, SESSION_TTL, dummy_hash, hash_password, new_token,
    session_cookie, verify_password,
};
use crate::error::{ApiError, Json};
use crate::limit::ClientIp;
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
/// 실패가 이어지면 잠시 막는다(429 `too_many_attempts`, `Retry-After`).
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn login(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    // 이름 형식이 아니면 그런 사용자는 있을 수 없다. 해시를 계산하지 않고 IP만 실패로 센다(긴 이름을
    // 기록에 넣지 않는다). 있을 수 없는 이름이라 응답 시간으로 드러나는 것도 없다.
    if validate_name(&req.username).is_err() {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    }
    // 막혀 있으면 해시를 계산하지 않는다. 이 시도는 미리 실패로 센다.
    state.attempts.begin(&req.username, ip)?;
    let user = state.cluster.get_by_name::<UserSpec>(&req.username)?;
    // 사용자가 없어도 해시를 비교해 응답 시간으로 사용자 존재 여부를 알 수 없게 한다.
    let hash = match &user {
        Some(u) => u.spec.password_hash.clone(),
        None => dummy_hash().await,
    };
    let ok = verify_password(req.password, hash).await;
    // 없는 사용자도 실패다(begin이 이미 셌다).
    let Some(user) = user.filter(|_| ok) else {
        return Err(wrong_credentials());
    };
    state.attempts.success(&req.username, ip);
    let session = state.sessions.create(user.id);
    let mut cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    let mut response = Json(UserView::from(user)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok(response)
}

fn wrong_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "wrong username or password",
    )
}

/// 세션을 끝내고 브라우저의 세션 쿠키를 지운다.
#[utoipa::path(post, path = "/auth/logout", tag = "auth", security(()),
    responses((status = 204, description = "Logged out; the session cookie is cleared",
        headers(("set-cookie" = String)))))]
pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(id) = session_cookie(&headers) {
        state.sessions.remove(id);
    }
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    (
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("ASCII cookie"),
        )],
    )
        .into_response()
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
            session_epoch: 0,
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
/// 현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
#[utoipa::path(put, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = UpdateUserRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    ClientIp(ip): ClientIp,
    Path(key): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserView>, ApiError> {
    // 관리 권한이 없으면 다른 사용자가 있는지조차 알려 주지 않는다.
    let may_write = state
        .authorizer
        .check(&principal, Action::Write, UserSpec::KIND);
    if !may_write && key != principal.name && key != principal.user.to_string() {
        return Err(ApiError::forbidden());
    }
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
            state.attempts.begin(&principal.name, ip)?;
            let current = req.current_password.unwrap_or_default();
            if !verify_password(current, user.spec.password_hash.clone()).await {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_credentials",
                    "the current password is wrong",
                ));
            }
            state.attempts.success(&principal.name, ip);
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
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
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
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
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

`crates/opl-api/tests/auth.rs`에서

```rust
        assert_eq!(body["code"], "forbidden", "{key}");
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(body["code"], "forbidden", "{key}");
    }
}

/// 로그인을 한 번 시도하고 (상태, Retry-After, 본문)을 돌려준다.
async fn try_login(
    env: &Env,
    user: &str,
    password: &str,
) -> (StatusCode, Option<String>, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_string());
    (status, retry_after, resp.json().await.unwrap())
}

#[tokio::test]
async fn repeated_login_failures_are_throttled() {
    let env = Env::start().await;
    for i in 0..5 {
        let (status, retry_after, _) = try_login(&env, "admin", "wrong-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{i}");
        assert_eq!(retry_after, None);
    }
    let (status, retry_after, body) = try_login(&env, "admin", "wrong-password").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "too_many_attempts");
    let secs: u64 = retry_after.unwrap().parse().unwrap();
    assert!((1..=30).contains(&secs), "{secs}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(&format!("try again in {secs} seconds")),
        "{body}"
    );
    // 막힌 동안에는 맞는 비밀번호도 받지 않는다.
    let (status, _, _) = try_login(&env, "admin", ADMIN_PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // 없는 사용자도 실패로 센다.
    for _ in 0..5 {
        assert_eq!(
            try_login(&env, "nobody", "wrong-password").await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        try_login(&env, "nobody", "wrong-password").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn successful_login_resets_failures() {
    let env = Env::start().await;
    for _ in 0..4 {
        try_login(&env, "admin", "wrong-password").await;
    }
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::OK
    );
    for _ in 0..4 {
        try_login(&env, "admin", "wrong-password").await;
    }
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn concurrent_logins_do_not_pass_the_limit() {
    let env = Env::start().await;
    // 비밀번호 확인을 기다리는 동안 들어온 시도도 앞의 시도를 실패로 보고 막힌다.
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let url = format!("{}/api/v1/auth/login", env.base);
        set.spawn(async move {
            reqwest::Client::new()
                .post(url)
                .json(&json!({ "username": "admin", "password": "wrong-password" }))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    let statuses = set.join_all().await;
    let throttled = statuses
        .iter()
        .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!(throttled, 15, "{statuses:?}");
}

#[tokio::test]
async fn impossible_user_names_count_only_against_the_ip() {
    let env = Env::start().await;
    let long = "a".repeat(10_000);
    for name in ["Admin", "no such user", long.as_str()] {
        let (status, _, body) = try_login(&env, name, "wrong-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
        assert_eq!(body["code"], "invalid_credentials");
    }
    for _ in 3..20 {
        try_login(&env, &long, "wrong-password").await;
    }
    // IP 키는 20번 실패로 막혀 맞는 비밀번호도 받지 않는다.
    assert_eq!(
        try_login(&env, "admin", ADMIN_PASSWORD).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn current_password_check_is_throttled_with_login() {
    let env = Env::start().await;
    let alice = env.user("alice", "viewer").await;
    let change = json!({ "password": "new-password", "current_password": "wrong-password" });
    for i in 0..5 {
        let (status, body) = alice.put("/users/alice", change.clone(), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{i}");
        assert_eq!(body["code"], "invalid_credentials");
    }
    let (status, body) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "too_many_attempts");
    // 로그인과 같은 기록을 쓴다.
    assert_eq!(
        try_login(&env, "alice", "password-1").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
```

을 다음으로 바꾼다:

```rust
        let server = tokio::spawn(async move {
            // opld처럼 연결 주소를 넘겨 시도 제한이 클라이언트 IP로 센다.
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
```

`crates/opld/src/member.rs`에서

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
```

을 다음으로 바꾼다:

```rust
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
```

`crates/opld/src/member.rs`에서

```rust
use anyhow::Context;
use openraft::BasicNode;
```

을 다음으로 바꾼다:

```rust
use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
```

`crates/opld/src/member.rs`에서

```rust
        let task = tokio::spawn(async move {
            let r = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
```

을 다음으로 바꾼다:

```rust
        let task = tokio::spawn(async move {
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
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 379개, 실패 0

Run: `cargo test -p opl-api --lib limit`
Expected: PASS

Run: `cargo test -p opl-api --test auth`
Expected: PASS 14개

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 229개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 로그인과 현재 비밀번호 확인에 시도 제한 추가"
```

---

### Task 3: 비밀번호가 바뀌면 모든 노드의 옛 세션 끊기

`Sessions::create(user, epoch)`가 만들 때의 `session_epoch`를 함께 저장하고 `get`은 (사용자, 세대)를 돌려준다. `update_user`는 비밀번호를 바꾸는 쓰기(본인·관리자)에서 세대를 1 올린다(역할만 바꾸면 그대로). 쿠키로 인증할 때 Raft에서 다시 읽은 사용자의 세대와 다르면 그 세션을 지우고 401 `unauthorized`. API 토큰은 그대로다. 테스트는 같은 클러스터의 `AppState` 둘(노드 둘 역할)로, 한쪽에서 비밀번호를 바꾸면 다른 쪽의 옛 쿠키가 401이고 토큰은 살아 있으며, 역할만 바꾸면 세션이 남는지 본다.

**Files:**
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/auth.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs` 전체:

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
use opl_types::auth::{ApiTokenSpec, Role, User, UserSpec};
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
        let account = matches!(kind, Kind::User | Kind::ApiToken | Kind::JoinToken);
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
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 노드 메모리에만 두는 세션. opld가 재시작하면 다시 로그인한다.
/// 만들 때 사용자의 세션 세대를 함께 둔다(다른 노드에서 비밀번호가 바뀌면 끊으려고).
#[derive(Default)]
pub struct Sessions {
    inner: Mutex<HashMap<String, (Uuid, u64, Instant)>>,
}

impl Sessions {
    pub fn create(&self, user: Uuid, epoch: u64) -> String {
        let id = random_hex(32);
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, (_, _, expires)| *expires > now);
        map.insert(id.clone(), (user, epoch, now + SESSION_TTL));
        id
    }

    /// (사용자, 만들 때의 세션 세대)
    pub fn get(&self, id: &str) -> Option<(Uuid, u64)> {
        let map = self.inner.lock().unwrap();
        map.get(id)
            .filter(|(_, _, expires)| *expires > Instant::now())
            .map(|(user, epoch, _)| (*user, *epoch))
    }

    pub fn remove(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }

    /// 사용자의 세션을 모두 끝낸다(비밀번호를 바꿨을 때).
    pub fn remove_user(&self, user: Uuid) {
        self.inner.lock().unwrap().retain(|_, (u, _, _)| *u != user);
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

fn principal_of(u: User) -> Principal {
    Principal {
        user: u.id,
        name: u.name,
        role: u.spec.role,
    }
}

impl AppState {
    fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(principal_of))
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
        let session = session_cookie(&parts.headers).ok_or_else(ApiError::unauthorized)?;
        let (user, epoch) = self
            .sessions
            .get(session)
            .ok_or_else(ApiError::unauthorized)?;
        // 쿠키는 브라우저가 알아서 붙이므로, 다른 출처의 페이지가 보낸 요청(WebSocket 포함)은 받지 않는다.
        if !same_origin(&parts.headers) {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "cross_origin",
                "cookie-authenticated requests must come from the same origin",
            ));
        }
        let user = self
            .cluster
            .get::<UserSpec>(user)?
            .ok_or_else(ApiError::unauthorized)?;
        // 세션을 만든 뒤 비밀번호가 바뀌었다(다른 노드에서 바뀌었을 수도 있다).
        if user.spec.session_epoch != epoch {
            self.sessions.remove(session);
            return Err(ApiError::unauthorized());
        }
        Ok(principal_of(user))
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
        let id = s.create(user, 7);
        assert_eq!(s.get(&id), Some((user, 7)));
        s.remove(&id);
        assert_eq!(s.get(&id), None);
        assert_eq!(s.get("unknown"), None);

        let (a, b) = (s.create(user, 0), s.create(user, 0));
        let other = s.create(Uuid::now_v7(), 0);
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

`crates/opl-api/src/users.rs`에서

```rust
    state.attempts.success(&req.username, ip);
    let session = state.sessions.create(user.id);
    let mut cookie = format!(
```

을 다음으로 바꾼다:

```rust
    state.attempts.success(&req.username, ip);
    let session = state.sessions.create(user.id, user.spec.session_epoch);
    let mut cookie = format!(
```

`crates/opl-api/src/users.rs`에서

```rust
/// 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
/// 바꾸면 그 사용자의 세션이 모두 끝난다. 마지막 관리자는 역할을 내려놓을 수 없다.
/// 현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
```

을 다음으로 바꾼다:

```rust
/// 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
/// 바꾸면 모든 노드에서 그 사용자의 세션이 끝난다(API 토큰은 남는다). 마지막 관리자는 역할을 내려놓을 수 없다.
/// 현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
```

`crates/opl-api/src/users.rs`에서

```rust
        spec.password_hash = hash_password(password).await?;
    }
```

을 다음으로 바꾼다:

```rust
        spec.password_hash = hash_password(password).await?;
        // 모든 노드의 옛 세션을 끊는다(쿠키 인증이 세대를 비교한다).
        spec.session_epoch += 1;
    }
```

`crates/opl-api/tests/auth.rs`에서

```rust

use common::{ADMIN_PASSWORD, Env};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust

use std::sync::Arc;

use common::{ADMIN_PASSWORD, Env};
use opl_api::AppState;
use reqwest::StatusCode;
```

`crates/opl-api/tests/auth.rs`에서

```rust
        try_login(&env, "alice", "password-1").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}
```

을 다음으로 바꾼다:

```rust
        try_login(&env, "alice", "password-1").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// 같은 클러스터를 쓰는 두 번째 API 서버(다른 노드 역할, 세션 저장소가 따로다)를 띄우고 주소를 돌려준다.
async fn serve_another_node(env: &Env) -> String {
    let state = Arc::new(AppState::new(
        env.cluster.clone(),
        env.node,
        env.hv.clone(),
        env.dir.path().join("uploads-b"),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = opl_api::router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// `base`에 로그인해 세션 쿠키(`opl_session=...`)를 돌려준다.
async fn cookie_login(base: &str, user: &str, password: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn whoami_with_cookie(base: &str, cookie: &str) -> (StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/auth/whoami"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn password_change_ends_sessions_on_other_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let alice = env.user("alice", "viewer").await;
    let b = serve_another_node(&env).await;

    // 본인이 노드 A에서 비밀번호를 바꾸면 노드 B의 옛 세션도 끊긴다.
    let cookie = cookie_login(&b, "alice", "password-1").await;
    assert_eq!(whoami_with_cookie(&b, &cookie).await.0, StatusCode::OK);
    let (status, body) = alice
        .put(
            "/users/alice",
            json!({ "password": "new-password", "current_password": "password-1" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = whoami_with_cookie(&b, &cookie).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "unauthorized");
    // API 토큰은 그대로다.
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::OK);

    // 관리자가 바꿔도 같다.
    let cookie = cookie_login(&b, "alice", "new-password").await;
    let (status, body) = admin
        .put(
            "/users/alice",
            json!({ "password": "reset-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        whoami_with_cookie(&b, &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );

    // 비밀번호가 아닌 것(역할)을 바꾸면 세션은 남는다.
    let cookie = cookie_login(&b, "alice", "reset-password").await;
    let (status, body) = admin
        .put("/users/alice", json!({ "role": "operator" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = whoami_with_cookie(&b, &cookie).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["role"], "operator");
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 380개, 실패 0

Run: `cargo test -p opl-api --test auth`
Expected: PASS 15개

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 229개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 비밀번호가 바뀌면 모든 노드의 옛 세션 끊기"
```

---

### Task 4: 업로드 사전 확인 API

`import_volume` 앞부분의 확인을 `pub async fn check_import(state, principal, q, size) -> Result<StoragePool, ApiError>`로 뽑고 이름 중복 조회를 더한다(권한 → 이름 형식·예약어 → 풀 → 이 노드의 풀 `pool_not_on_node` → 이름 중복 `name_taken`(`volume "x" already exists`) → 공간 `insufficient_storage`). 새 `POST /volumes/import/check?name&pool&format&source_format&size`(`ImportCheckQuery`, `deny_unknown_fields`와 `flatten`을 함께 쓸 수 없어 필드를 따로 둔다)는 확인만 하고 204. 업로드도 같은 함수를 부르며 크기는 `Content-Length`라 없으면 다른 확인보다 먼저 411이다. Raft 만들기의 `name_taken`은 그대로 둔다. oplctl `volume import`는 파일 크기로 먼저 확인하고 거절되면 파일을 열지 않는다. 테스트: 각 거절 code와 204, 업로드 이름 중복이 볼륨을 만들기 전에 409, 길이 없는 업로드가 먼저 411, OpenAPI 경로 목록, oplctl smoke.

**Files:**
- Create: `crates/opl-api/tests/import_check.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/tests/smoke.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(volumes::import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(volumes::import_volume))
        .routes(routes!(volumes::check_import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 업로드 본문의 문서용 스키마: 바이너리 파일
```

을 다음으로 바꾼다:

```rust

/// 업로드 사전 확인의 매개변수. 업로드의 것에 올릴 파일 크기(바이트)를 더한다.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ImportCheckQuery {
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (기본 qcow2)
    #[serde(default = "qcow2")]
    #[param(inline)]
    pub format: ImageFormat,
    #[param(inline)]
    pub source_format: SourceFormat,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// 업로드를 받기 전의 확인. 사전 확인과 업로드가 함께 쓴다. 문제가 없으면 풀을 돌려준다.
/// 확인 뒤에 다른 요청이 이름을 가져가면 저장소에 만들 때 `name_taken`이 난다.
pub async fn check_import(
    state: &AppState,
    principal: &Principal,
    q: &ImportQuery,
    size: u64,
) -> Result<StoragePool, ApiError> {
    state.authorize(principal, Action::Write, VolumeSpec::KIND)?;
    check_volume_name(&q.name)?;
    let pool = find_pool(state, &q.pool)?;
    if pool_node(state, &pool) != state.node {
        return Err(ApiError::conflict(
            "pool_not_on_node",
            format!(
                "storage pool {:?} is not available on this node; upload to one of its nodes",
                pool.name
            ),
        ));
    }
    if state.cluster.get_by_name::<VolumeSpec>(&q.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("volume {:?} already exists", q.name),
        ));
    }
    tokio::fs::create_dir_all(&state.upload_dir)
        .await
        .map_err(ApiError::internal)?;
    let available = fs4::available_space(&state.upload_dir).map_err(ApiError::internal)?;
    if size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("not enough free space for a {size}-byte upload"),
        ));
    }
    Ok(pool)
}

/// 업로드 전에 받을 수 있는지 확인한다(204). 거절 이유는 업로드와 같다. 본문은 받지 않는다.
#[utoipa::path(post, path = "/volumes/import/check", tag = "storage",
    params(ImportCheckQuery),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_import_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ImportCheckQuery>,
) -> Result<StatusCode, ApiError> {
    let size = q.size;
    let q = ImportQuery {
        name: q.name,
        pool: q.pool,
        format: q.format,
        source_format: q.source_format,
    };
    check_import(&state, &principal, &q, size).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 업로드 본문의 문서용 스키마: 바이너리 파일
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
/// 풀은 이 노드에서 쓸 수 있어야 한다.
#[utoipa::path(post, path = "/volumes/import", tag = "storage",
```

을 다음으로 바꾼다:

```rust
/// 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
/// 풀은 이 노드에서 쓸 수 있어야 한다. 받기 전에 `check_import`로 확인한다.
#[utoipa::path(post, path = "/volumes/import", tag = "storage",
```

`crates/opl-api/src/volumes.rs`에서

```rust
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_volume_name(&q.name)?;
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
```

을 다음으로 바꾼다:

```rust
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let length = content_length(&headers)?;
    let pool = check_import(&state, &principal, &q, length).await?;
    // 크기는 가져온 뒤 실제 이미지 크기로 바뀐다.
```

`crates/opl-api/src/volumes.rs`에서

```rust
    volume.status.node = Some(state.node);
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
```

을 다음으로 바꾼다:

```rust
    volume.status.node = Some(state.node);
    state.cluster.create(&volume).await?;
```

`crates/opl-api/tests/import_check.rs`를 만든다:

```rust
//! 업로드 사전 확인(`POST /volumes/import/check`)과 업로드가 같은 확인을 쓰는지.
//! 확인에서 거절되면 Task를 만들지 않으므로 qemu-img 없이 돈다.

mod common;

use common::{Client, Env};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use reqwest::StatusCode;
use serde_json::Value;
use uuid::Uuid;

/// 풀(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 풀 `local`에 볼륨을 저장소에 직접 만든다.
async fn volume(env: &Env, name: &str) -> Uuid {
    let volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.id
}

async fn check(client: &Client, name: &str, pool: &str, size: u64) -> (StatusCode, Value) {
    client
        .post(
            &format!(
                "/volumes/import/check?name={name}&pool={pool}&source_format=vmdk&size={size}"
            ),
            Value::Null,
        )
        .await
}

#[tokio::test]
async fn check_rejects_with_the_upload_codes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    volume(&env, "taken").await;

    let (status, body) = check(&admin, "disk", "local", 1 << 20).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    // 확인만 하고 볼륨은 만들지 않는다.
    assert_eq!(admin.get("/volumes/disk").await.0, StatusCode::NOT_FOUND);

    let viewer = env.user("viewer", "viewer").await;
    let (status, body) = check(&viewer, "disk", "local", 1 << 20).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    for (name, pool, size, want, code) in [
        (
            "Bad_Name",
            "local",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "import",
            "local",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "disk",
            "missing",
            1 << 20,
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
        (
            "disk",
            "remote",
            1 << 20,
            StatusCode::CONFLICT,
            "pool_not_on_node",
        ),
        (
            "taken",
            "local",
            1 << 20,
            StatusCode::CONFLICT,
            "name_taken",
        ),
        // 이름 중복은 공간보다 먼저 본다.
        (
            "taken",
            "local",
            u64::MAX,
            StatusCode::CONFLICT,
            "name_taken",
        ),
        (
            "disk",
            "local",
            u64::MAX,
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
        ),
    ] {
        let (status, body) = check(&admin, name, pool, size).await;
        assert_eq!(
            (status, body["code"].as_str()),
            (want, Some(code)),
            "{name} {pool} {body}"
        );
    }

    // 크기가 없으면 확인할 수 없다.
    let (status, body) = admin
        .post(
            "/volumes/import/check?name=disk&pool=local&source_format=vmdk",
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn upload_with_a_taken_name_is_rejected_before_creating_a_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let id = volume(&env, "taken").await;

    let (status, body) = admin
        .upload(
            "/volumes/import?name=taken&pool=local&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    // 저장소에 만들기 전에 거절하므로 받은 파일도 없다.
    assert_eq!(body["message"], "volume \"taken\" already exists");
    let (_, volumes) = admin.get("/volumes").await;
    let volumes = volumes["items"].as_array().unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0]["id"], id.to_string());
    let uploads = env.dir.path().join("uploads");
    assert!(!uploads.exists() || std::fs::read_dir(&uploads).unwrap().count() == 0);
}

#[tokio::test]
async fn upload_without_content_length_is_rejected_before_other_checks() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 크기를 모르면 확인할 수 없으므로 없는 풀이나 잘못된 이름보다 411이 먼저다.
    for query in ["name=x&pool=missing", "name=Bad&pool=missing"] {
        let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![0u8; 512])]);
        let r = admin
            .http
            .post(format!(
                "{}/api/v1/volumes/import?{query}&source_format=raw",
                admin.base
            ))
            .bearer_auth(admin.token.as_ref().unwrap())
            .body(reqwest::Body::wrap_stream(chunks));
        let (status, body) = admin.send(r).await;
        assert_eq!(status, StatusCode::LENGTH_REQUIRED, "{query}: {body}");
        assert_eq!(body["code"], "length_required");
    }
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/volumes/import",
        "/volumes/{key}/resize",
```

을 다음으로 바꾼다:

```rust
        "/volumes/import",
        "/volumes/import/check",
        "/volumes/{key}/resize",
```

`crates/oplctl/src/cli.rs`에서

```rust
                ];
                let task = client.upload("/volumes/import", &query, &file).await?;
```

을 다음으로 바꾼다:

```rust
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
```

`crates/opld/tests/smoke.rs`에서

```rust
    assert!(err.to_string().contains("name_taken"), "{err}");
```

을 다음으로 바꾼다:

```rust
    assert!(err.to_string().contains("name_taken"), "{err}");
    // 가져오기는 파일을 보내기 전에 확인한다. 거절되면 파일을 열지 않는다(읽을 수 없는 파일이어도
    // 서버의 거절 이유가 보인다). root는 권한과 상관없이 읽으므로 root로 돌리면 이 단언은 거절 이유만 본다.
    let image = d.path("disk.vmdk");
    std::fs::write(&image, [0u8; 512]).unwrap();
    std::fs::set_permissions(&image, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    let err = d
        .ctl(&[
            "volume",
            "import",
            "disk",
            "--pool",
            "missing",
            "--file",
            image.to_str().unwrap(),
        ])
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 383개, 실패 0

Run: `cargo test -p opl-api --test import_check`
Expected: PASS 3개

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 229개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 업로드 사전 확인 API와 oplctl 가져오기의 먼저 확인"
```

---

### Task 5: 노드 아키텍처 등록과 머신에 맞는 노드 배치

opld `register_node`가 `HostProfile.arch`를 `NodeStatus.arch`에 기록한다(설정으로 바꾼 프로필도 따른다). 스케줄러는 `n.status.arch == Some(spec.machine.arch())`인 노드만 고르고(arch를 모르는 노드는 후보가 아니다) `NoNode` 메시지에 아키텍처를 넣는다. `vms::check_node_arch`가 지정 노드의 arch를 알고 머신과 다르면 400 `machine_arch_mismatch`를 돌려주고(모르면 통과), VM 만들기·고치기·되돌리기의 `prepare`와 VM 시작(고정된 노드)이 이를 쓴다. 메모리 스냅샷 되돌리기가 배치되지 않은 VM을 이 노드에 고정할 때 arch가 맞지 않으면 고정하지 않고 Task를 실패로 끝낸다(메모리 상태는 이 노드에서만 이어진다). 테스트 노드는 테스트 VM(`virt`)에 맞춰 `aarch64`. 테스트: 스케줄러 단위, API 400·통과·시작, 메모리 되돌리기, opld 등록.

**Files:**
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opld/src/bootstrap.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, can reach its storage pools and has the bridges of its networks)"
)]
```

을 다음으로 바꾼다:

```rust
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, has the architecture of its machine type, can reach its storage pools and has the bridges of its networks)"
)]
```

`crates/opl-api/src/scheduler.rs`에서

```rust

/// 온라인이고 유지보수 중이 아니며 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드 가운데 미할당 메모리가 가장
/// 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은 범위 밖).
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
```

을 다음으로 바꾼다:

```rust

/// 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
/// 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다.
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| inv.reaches_disks(spec, n.id))
```

을 다음으로 바꾼다:

```rust
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| n.status.arch == Some(spec.machine.arch()))
        .filter(|n| inv.reaches_disks(spec, n.id))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec};
```

을 다음으로 바꾼다:

```rust
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{Arch, CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec};
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            bridges: vec![],
            arch: None,
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            arch: Some(Arch::Aarch64),
        };
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }

    #[test]
    fn needs_the_architecture_of_its_machine() {
        let x86 = {
            let mut n = node(64000);
            n.status.arch = Some(Arch::X86_64);
            n
        };
        let unknown = {
            let mut n = node(32000);
            n.status.arch = None;
            n
        };
        let arm = node(1024);
        let inv = Inventory {
            nodes: vec![x86.clone(), unknown, arm.clone()],
            ..Default::default()
        };
        // virt는 aarch64 노드에서만 돈다. arch를 모르는 노드(옛 버전)는 후보가 아니다.
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(arm.id));
        me.machine = MachineType::Q35;
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(x86.id));
        let inv = Inventory {
            nodes: vec![arm],
            ..Default::default()
        };
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }
}
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_driver::{DriverError, uefi_vars_path, vm_state_dir};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DriverError, uefi_vars_path, vm_state_dir};
use opl_types::node::NodeSpec;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use crate::schedule::{recorded_vm, set_schedule_error};
```

을 다음으로 바꾼다:

```rust
use crate::schedule::{recorded_vm, set_schedule_error};
use crate::vms::check_node_arch;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        if snapshot.spec.memory && spec.node.is_none() {
            spec.node = Some(ctx.config.node);
```

을 다음으로 바꾼다:

```rust
        if snapshot.spec.memory && spec.node.is_none() {
            // 이 노드의 arch가 머신과 다르면 여기서 시작할 수 없다. 다른 노드로 보내면 메모리 상태를
            // 이어 쓸 수 없으므로 고정하지 않고 Task를 실패로 끝낸다(디스크를 건드리기 전이다).
            if let Some(node) = ctx
                .cluster
                .get::<NodeSpec>(ctx.config.node)
                .map_err(|e| e.to_string())?
            {
                check_node_arch(&node, &spec).map_err(|e| e.message)?;
            }
            spec.node = Some(ctx.config.node);
```

`crates/opl-api/src/vms.rs`에서

```rust
use opl_types::network::USER_NETWORK;
use opl_types::resource::{Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use opl_types::network::USER_NETWORK;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
```

`crates/opl-api/src/vms.rs`에서

```rust
    }
    if let Some(node) = spec.node
        && !inv.nodes.iter().any(|n| n.id == node)
    {
        return Err(ApiError::bad_request(format!("node {node} not found")));
    }
```

을 다음으로 바꾼다:

```rust
    }
    if let Some(id) = spec.node {
        let Some(node) = inv.nodes.iter().find(|n| n.id == id) else {
            return Err(ApiError::bad_request(format!("node {id} not found")));
        };
        check_node_arch(node, spec)?;
    }
```

`crates/opl-api/src/vms.rs`에서

```rust
        );
    }
```

을 다음으로 바꾼다:

```rust
        );
    }
    Ok(())
}

/// 노드의 arch가 머신과 다르면 400 `machine_arch_mismatch`. arch를 모르는 노드(옛 버전)는 막지 않는다.
/// 업그레이드 중에도 VM을 고칠 수 있게.
pub(crate) fn check_node_arch(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if let Some(arch) = node.status.arch
        && arch != spec.machine.arch()
    {
        // Debug 이름을 소문자로 바꾸면 API의 값(q35, x86_64 등)과 같다.
        let name = |v: &dyn std::fmt::Debug| format!("{v:?}").to_lowercase();
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "machine_arch_mismatch",
            format!(
                "machine type {} needs an {} node, but node {:?} is {}",
                name(&spec.machine),
                name(&spec.machine.arch()),
                node.name,
                name(&arch)
            ),
        ));
    }
```

`crates/opl-api/src/vms.rs`에서

```rust
            let vm = modify_spec(&state, id, |spec| {
                if spec.node.is_none() {
```

을 다음으로 바꾼다:

```rust
            let vm = modify_spec(&state, id, |spec| {
                // 고정된 노드는 VM을 고칠 때 확인했지만, 그때 arch를 모르던 노드일 수 있다.
                if let Some(node) = spec.node
                    && let Some(node) = state.cluster.get::<NodeSpec>(node)?
                {
                    check_node_arch(&node, spec)?;
                }
                if spec.node.is_none() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use opl_types::storage::{SourceFormat, StoragePoolSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{SourceFormat, StoragePoolSpec};
use opl_types::vm::Arch;
use reqwest::StatusCode;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            bridges: vec![],
            arch: None,
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
        };
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
use opl_qemu::QmpError;
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
    assert_eq!(env.pid(vm).await, None);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.pid(vm).await, None);
}

#[tokio::test]
async fn memory_revert_does_not_place_the_vm_on_a_node_of_another_architecture() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "mem" })).await);
    assert!(get(&env, vm, "mem").spec.memory);
    // 끄고 배치를 푼다. 그 뒤 이 노드가 다른 arch로 기록됐다.
    succeeded(&admin.act("web", "stop").await);
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = Value::Null;
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.arch = Some(Arch::X86_64);
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();

    let (status, task) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("machine type"),
        "{task}"
    );
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.node, None);
    assert_eq!(got.spec.power, PowerState::Stopped);
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
use opl_types::resource::Resource;
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::vm::Arch;
use reqwest::StatusCode;
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            bridges: vec![],
            arch: None,
        };
```

을 다음으로 바꾼다:

```rust
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
        };
```

`crates/opl-api/tests/vms.rs`에서

```rust
use opl_driver::DriverError;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{VolumePhase, VolumeSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_driver::DriverError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{VolumePhase, VolumeSpec};
use opl_types::vm::Arch;
use reqwest::StatusCode;
```

`crates/opl-api/tests/vms.rs`에서

```rust
#[tokio::test]
async fn update_checks_if_match_revision() {
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn machine_must_match_the_node_architecture() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node = env.node.to_string();
    // 테스트 노드는 aarch64다. q35(x86_64)를 이 노드에 두면 거절한다.
    let mut x86 = vm_request("x86");
    x86["spec"]["machine"] = json!("q35");
    x86["spec"]["node"] = json!(node);
    let (status, body) = admin.post("/vms", x86.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");

    // 고칠 때도 같은 검증이다.
    let mut arm = vm_request("arm");
    arm["spec"]["node"] = json!(node);
    let (status, vm) = admin.post("/vms", arm).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["machine"] = json!("q35");
    let (status, body) = admin.put("/vms/arm", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");

    // 노드를 정하지 않은 q35 VM은 만들 수 있지만 맞는 노드가 없어 시작하지 못한다.
    let mut free = vm_request("free");
    free["spec"]["machine"] = json!("q35");
    assert_eq!(admin.post("/vms", free).await.0, StatusCode::CREATED);
    let (status, body) = admin.post("/vms/free/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "no_node");

    // arch를 모르는 노드(옛 버전)는 지정을 막지 않는다.
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.arch = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (code, body) = admin.post("/vms", x86).await;
    assert_eq!(code, StatusCode::CREATED, "{body}");

    // 그 노드가 arch를 기록한 뒤 시작하면 지정 노드 검증과 같은 400이다.
    status.arch = Some(Arch::Aarch64);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (code, body) = admin.post("/vms/x86/actions/start", Value::Null).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");
}

#[tokio::test]
async fn update_checks_if_match_revision() {
```

`crates/opld/src/bootstrap.rs`에서

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

을 다음으로 바꾼다:

```rust

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

`crates/opld/src/bootstrap.rs`에서

```rust
        bridges: host.bridges().await.unwrap_or(recorded),
        arch: None,
    };
```

을 다음으로 바꾼다:

```rust
        bridges: host.bridges().await.unwrap_or(recorded),
        arch: Some(host.profile.arch),
    };
```

`crates/opld/src/bootstrap.rs`에서

```rust
    use opl_types::node::BridgeInfo;
```

을 다음으로 바꾼다:

```rust
    use opl_types::node::BridgeInfo;
    use opl_types::vm::Arch;
```

`crates/opld/src/bootstrap.rs`에서

```rust

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir.path()), router.network(id))
            .await
```

을 다음으로 바꾼다:

```rust

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
```

`crates/opld/src/bootstrap.rs`에서

```rust
            .unwrap();

        let config: Config = toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
```

을 다음으로 바꾼다:

```rust
            .unwrap();
        (cluster, id)
    }

    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 387개, 실패 0

Run: `cargo test -p opl-api --test vms`
Expected: PASS 16개

Run: `cargo test -p opl-api --test snapshot_tasks`
Expected: PASS 15개

Run: `cargo test -p opld --features qemu-it --test e2e`
Expected: PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 노드 아키텍처 등록과 머신에 맞는 노드 배치"
```

---

### Task 6: oplctl: 머신 기본값을 노드 아키텍처로

`vm create`에 `--machine`이 없으면 `GET /nodes`를 읽어 비공개 `default_machine(node, nodes)`로 정한다: `--node`로 준 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch일 때 그것(`x86_64` → `q35`, `aarch64` → `virt`), 그 밖에는 `--machine virt` 또는 `--machine q35`를 달라는 오류. `--node`는 머신을 정하기 전에 찾는다. 클라이언트의 컴파일 아키텍처로 정하던 코드를 지운다. 순수 함수 단위 테스트.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust

#[derive(Clone, Copy, ValueEnum)]
enum Machine {
```

을 다음으로 바꾼다:

```rust

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
```

`crates/oplctl/src/cli.rs`에서

```rust
    memory: u32,
    /// 기본값은 호스트에 맞춘다(Apple Silicon: virt, x86_64: q35).
    #[arg(long, value_enum)]
```

을 다음으로 바꾼다:

```rust
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
```

`crates/oplctl/src/cli.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
```

`crates/oplctl/src/cli.rs`에서

```rust
        }
    }
    Ok(())
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
```

을 다음으로 바꾼다:

```rust
        }
    }
    Ok(())
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
```

`crates/oplctl/src/cli.rs`에서

```rust
    #[test]
    fn parses_clone_and_schedule_commands() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 388개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS 18개

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e`
Expected: PASS 5개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): vm create의 머신 기본값을 노드 아키텍처로 정하기"
```

---

### Task 7: 웹: 업로드 사전 확인·처리 대기 제한, 머신 기본값, 아키텍처 열

`storage/upload.ts`: 업로드 전에 `checkUpload(size, params, signal)`로 `POST /volumes/import/check`를 부르고, 거절이면 XHR 없이 오류 code 문구로 실패 알림(확인 단계의 연결 실패는 `errors.network`). 취소는 확인 요청도 끊는다. `uploadImage`의 `onSent`(XHR `upload`의 `load`, 빈 파일도)에서 "처리 중"과 2분 타이머(`NO_RESPONSE_MS`)를 시작하고, 시간이 지나면 알림을 `uploads.noResponse`({{name}})로 바꿔 닫을 수 있게 한다. XHR은 끊지 않고(끊으면 서버가 받은 볼륨을 지운다) 늦은 응답은 평소처럼 알린다. `vms/form.ts`의 `defaultMachine(nodes, selectedNode)`: 지정 노드의 arch → arch를 아는 노드가 모두 같으면 그 arch → `q35`(hvf로 추측하던 코드를 지운다). VM 만들기 화면에서 노드를 바꾸면 사용자가 머신을 고르지 않았을 때만 다시 정한다(편집 화면은 바꾸지 않는다). 노드 목록에 "아키텍처" 열(모르면 —). ko/en 문구 `errors.too_many_attempts`·`errors.machine_arch_mismatch`·`uploads.noResponse`. 테스트의 `FakeXhr.last`는 공용 setup에서 비운다. 테스트: 확인 거절·연결 실패·확인 중 취소, 2분 제한(가짜 타이머), 기본값 규칙, 노드 변경 재계산과 편집 화면 유지, 아키텍처 열, 로그인 429 문구.

**Files:**
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/auth/AuthProvider.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/storage/UploadDialog.test.tsx`
- Modify: `web/src/storage/upload.test.tsx`
- Modify: `web/src/storage/upload.ts`
- Modify: `web/src/test/fakeXhr.ts`
- Modify: `web/src/test/setup.ts`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx`에서

```tsx

it("does not show the sign-in form while checking the session", async () => {
```

을 다음으로 바꾼다:

```tsx

it("says when sign-in attempts are throttled", async () => {
  signedOut();
  server.use(
    http.post(apiUrl("/auth/login"), () =>
      HttpResponse.json(
        { code: "too_many_attempts", message: "try again in 30 seconds" },
        { status: 429, headers: { "Retry-After": "30" } },
      ),
    ),
  );
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  await user.type(await screen.findByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts. Try again later.");
});

it("does not show the sign-in form while checking the session", async () => {
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
  });
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/auth/logout"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const params = { name: "img", pool: "local", format: "qcow2", sourceFormat: "raw" } as const;
```

을 다음으로 바꾼다:

```tsx
  });
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/auth/logout"), () => new HttpResponse(null, { status: 204 })),
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const params = { name: "img", pool: "local", format: "qcow2", sourceFormat: "raw" } as const;
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
  expect(await screen.findByText(/^Uploading img/)).toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByText(/^Uploading img/)).toBeInTheDocument();
  // 확인을 통과해 파일을 보내는 중이다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
      return HttpResponse.json(ADMIN);
    }),
    http.post(apiUrl("/auth/login"), () => HttpResponse.json(ADMIN)),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
      return HttpResponse.json(ADMIN);
    }),
    http.post(apiUrl("/auth/login"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
```

`web/src/auth/AuthProvider.test.tsx`에서

```tsx
  act(() => startUpload(new File(["x"], "img.raw"), params, i18n.t));
  // 다시 로그인해 세대가 바뀐 뒤에 앞 세대의 업로드가 401을 받는다.
```

을 다음으로 바꾼다:

```tsx
  act(() => startUpload(new File(["x"], "img.raw"), params, i18n.t));
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  // 다시 로그인해 세대가 바뀐 뒤에 앞 세대의 업로드가 401을 받는다.
```

`web/src/i18n/en.json`에서

```json
    "node_offline": "The node holding this VM is offline.",
    "task_failed": "Task error: {{message}}",
```

을 다음으로 바꾼다:

```json
    "node_offline": "The node holding this VM is offline.",
    "too_many_attempts": "Too many attempts. Try again later.",
    "machine_arch_mismatch": "The machine type doesn't match the selected node's architecture.",
    "task_failed": "Task error: {{message}}",
```

`web/src/i18n/en.json`에서

```json
    "processing": "Processing {{name}}…",
    "dropped": "The server rejected the upload or the connection was lost.",
```

을 다음으로 바꾼다:

```json
    "processing": "Processing {{name}}…",
    "noResponse": "{{name}}: no response from the server. Check the volume list for the result.",
    "dropped": "The server rejected the upload or the connection was lost.",
```

`web/src/i18n/en.json`에서

```json
    "hypervisor": "Hypervisor",
    "cpus": "CPUs",
```

을 다음으로 바꾼다:

```json
    "hypervisor": "Hypervisor",
    "arch": "Architecture",
    "cpus": "CPUs",
```

`web/src/i18n/i18n.test.ts`에서

```ts
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

을 다음으로 바꾼다:

```ts
  );
  expect(errorMessage(t, new ApiError(429, "too_many_attempts", "try again in 30 seconds"))).toBe(
    "Too many attempts. Try again later.",
  );
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
    "The machine type doesn't match the selected node's architecture.",
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "task_failed": "작업 오류: {{message}}",
```

을 다음으로 바꾼다:

```json
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "too_many_attempts": "시도가 너무 많습니다. 잠시 뒤에 다시 하세요.",
    "machine_arch_mismatch": "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다.",
    "task_failed": "작업 오류: {{message}}",
```

`web/src/i18n/ko.json`에서

```json
    "processing": "{{name}} 처리 중…",
    "dropped": "서버가 업로드를 거절했거나 연결이 끊겼습니다.",
```

을 다음으로 바꾼다:

```json
    "processing": "{{name}} 처리 중…",
    "noResponse": "{{name}}: 서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요.",
    "dropped": "서버가 업로드를 거절했거나 연결이 끊겼습니다.",
```

`web/src/i18n/ko.json`에서

```json
    "hypervisor": "하이퍼바이저",
    "cpus": "CPU",
```

을 다음으로 바꾼다:

```json
    "hypervisor": "하이퍼바이저",
    "arch": "아키텍처",
    "cpus": "CPU",
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  revision: 7,
  status: { ...NODE.status, online: false, bridges: [{ name: "vmbr0", vlan_filtering: true }] },
};
```

을 다음으로 바꾼다:

```tsx
  revision: 7,
  status: {
    ...NODE.status,
    online: false,
    bridges: [{ name: "vmbr0", vlan_filtering: true }],
    arch: "aarch64" as const,
  },
};
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  expect(b).toHaveTextContent("vmbr0");
});
```

을 다음으로 바꾼다:

```tsx
  expect(b).toHaveTextContent("vmbr0");
  const archColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Architecture");
  expect(within(b).getAllByRole("cell")[archColumn]).toHaveTextContent("aarch64");
  // 옛 버전으로 등록한 노드는 아키텍처를 모른다.
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
});
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

을 다음으로 바꾼다:

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 10 : 9} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
              <TableCell>{node.status.hypervisor}</TableCell>
              <TableCell>{node.status.cpus}</TableCell>
```

을 다음으로 바꾼다:

```tsx
              <TableCell>{node.status.hypervisor}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              <TableCell>{node.status.cpus}</TableCell>
```

`web/src/storage/UploadDialog.test.tsx`에서

```tsx
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
```

`web/src/storage/UploadDialog.test.tsx`에서

```tsx
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(FakeXhr.last!.url).toContain(
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // 확인을 통과한 뒤에 보낸다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain(
```

`web/src/storage/upload.test.tsx` 전체:

```tsx
import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { delay, http, HttpResponse } from "msw";

import { ApiError, NetworkError, setUnauthorizedHandler } from "@/api/client";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import {
  nameFromFile,
  NO_RESPONSE_MS,
  resetUploads,
  sourceFormatOf,
  startUpload,
  uploadImage,
} from "@/storage/upload";
import * as tasks from "@/tasks/tasks";
import { resetTasks } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { task } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

function toasts() {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
}

const PARAMS = { name: "img", pool: "local", format: "qcow2", sourceFormat: "vmdk" } as const;

/** 확인을 통과한 뒤 만들어지는 XHR을 기다린다. */
async function sending(): Promise<FakeXhr> {
  return vi.waitFor(() => {
    expect(FakeXhr.last).toBeDefined();
    return FakeXhr.last!;
  });
}

/** 업로드 확인 요청의 쿼리. 기본으로 통과(204)시킨다. */
let checks: URLSearchParams[] = [];

beforeEach(() => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  checks = [];
  server.use(
    http.post(apiUrl("/volumes/import/check"), ({ request }) => {
      checks.push(new URL(request.url).searchParams);
      return new HttpResponse(null, { status: 204 });
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
  resetTasks();
});

it("suggests a volume name and format from the file name", () => {
  expect(nameFromFile("Ubuntu 24.04.vmdk")).toBe("ubuntu-24-04");
  expect(nameFromFile("--disk--.qcow2")).toBe("disk");
  expect(nameFromFile(`${"a".repeat(62)}-b.img`)).toBe("a".repeat(62));
  expect(sourceFormatOf("disk.VMDK")).toBe("vmdk");
  expect(sourceFormatOf("disk.qcow2")).toBe("qcow2");
  expect(sourceFormatOf("disk.qcow")).toBe("raw");
  expect(sourceFormatOf("disk.img")).toBe("raw");
});

it("posts the file to the import API and reports progress", async () => {
  const progress = vi.fn();
  const sent = vi.fn();
  const file = new Blob(["x".repeat(1024)]);
  const done = uploadImage(file, PARAMS, { onProgress: progress, onSent: sent });
  const xhr = FakeXhr.last!;
  expect(xhr.method).toBe("POST");
  expect(xhr.url).toBe(
    `${window.location.origin}/api/v1/volumes/import?name=img&pool=local&format=qcow2&source_format=vmdk`,
  );
  expect(xhr.headers["Content-Type"]).toBe("application/octet-stream");
  expect(xhr.body).toBe(file);
  xhr.progress(512, 1024);
  expect(progress).toHaveBeenCalledWith(512, 1024);
  expect(sent).not.toHaveBeenCalled();
  xhr.uploaded();
  expect(sent).toHaveBeenCalledOnce();
  xhr.respond(202, task("t1", "volume-import", "v1"));
  await expect(done).resolves.toMatchObject({ id: "t1" });
});

it("turns failures into API, network and abort errors", async () => {
  const conflict = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(409, { code: "pool_not_on_node", message: "elsewhere" });
  await expect(conflict).rejects.toEqual(new ApiError(409, "pool_not_on_node", "elsewhere"));

  const offline = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.fail();
  await expect(offline).rejects.toBeInstanceOf(NetworkError);

  const controller = new AbortController();
  const cancelled = uploadImage(new Blob(["x"]), PARAMS, { signal: controller.signal });
  controller.abort(new Error("stop"));
  await expect(cancelled).rejects.toThrow("stop");
  expect(FakeXhr.last!.aborted).toBe(true);

  // Task가 아닌 202 본문
  const odd = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(202, { ok: true });
  await expect(odd).rejects.toBeInstanceOf(ApiError);
});

it("reports a 401 like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  onTestFinished(() =>
    setUnauthorizedHandler(
      () => {},
      () => 0,
    ),
  );
  const done = uploadImage(new Blob(["x"]), PARAMS, {});
  FakeXhr.last!.respond(401, { code: "unauthorized", message: "no session" });
  await expect(done).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
});

it("shows progress in a toast, guards the page and hands the task over", async () => {
  toasts();
  act(() => startUpload(new File(["x".repeat(2048)], "img.vmdk"), PARAMS, i18n.t));
  expect(await screen.findByText("Uploading img: 0% (0 B / 2 KiB)")).toBeInTheDocument();
  const xhr = await sending();
  // 먼저 파일 크기로 확인했다.
  expect(Object.fromEntries(checks[0]!)).toEqual({
    name: "img",
    pool: "local",
    format: "qcow2",
    source_format: "vmdk",
    size: "2048",
  });
  const leave = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(leave);
  expect(leave.defaultPrevented).toBe(true);

  act(() => xhr.progress(1024, 2048));
  expect(await screen.findByText("Uploading img: 50% (1 KiB / 2 KiB)")).toBeInTheDocument();
  // 5% 단위로 내림한다(56%가 아니라 55%). sonner는 나중에 바꾸므로 기다린다.
  act(() => xhr.progress(1150, 2048));
  expect(await screen.findByText("Uploading img: 55% (1.1 KiB / 2 KiB)")).toBeInTheDocument();
  // 다 보내면 서버가 처리하는 동안 취소할 수 없다.
  act(() => {
    xhr.progress(2048, 2048);
    xhr.uploaded();
  });
  expect(await screen.findByText("Processing img…")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
  const after = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(after);
  expect(after.defaultPrevented).toBe(false);
});

it("cancels from the toast", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  const xhr = await sending();
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(xhr.aborted).toBe(true);
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});

it("cancels while the check is pending without sending the file", async () => {
  server.use(http.post(apiUrl("/volumes/import/check"), () => delay("infinite")));
  toasts();
  act(() => startUpload(new File(["x"], "img.vmdk"), PARAMS, i18n.t));
  await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
  expect(await screen.findByText("Cancelled the upload of img.")).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
});

it("keeps guarding the page until the last upload ends", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "a.raw"), { ...PARAMS, name: "a" }, i18n.t));
  const first = await sending();
  FakeXhr.last = undefined;
  act(() => startUpload(new File(["x"], "b.raw"), { ...PARAMS, name: "b" }, i18n.t));
  const second = await sending();
  const leaving = () => {
    const e = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(e);
    return e.defaultPrevented;
  };

  act(() => first.respond(202, task("ta", "volume-import", "va")));
  expect(await screen.findByText("Import volume a")).toBeInTheDocument();
  expect(leaving()).toBe(true);
  act(() => second.respond(202, task("tb", "volume-import", "vb")));
  expect(await screen.findByText("Import volume b")).toBeInTheDocument();
  expect(leaving()).toBe(false);
});

it("says the server may have dropped the connection", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.fail());
  expect(
    await screen.findByText(
      "Upload of img failed. The server rejected the upload or the connection was lost.",
    ),
  ).toBeInTheDocument();
});

it("fails instead of hanging when the answer is not a task", async () => {
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.respond(202, "<html>"));
  expect(
    await screen.findByText("Upload of img failed. The server failed: HTTP 202"),
  ).toBeInTheDocument();
  expect(screen.queryByText(/^Uploading img/)).not.toBeInTheDocument();
});

// 넘기다 던져도 "올리는 중" 알림에 남지 않고 실패를 알린다.
it("says the upload failed when handing over the task throws", async () => {
  vi.spyOn(tasks, "trackTask").mockImplementation(() => {
    throw new Error("boom");
  });
  onTestFinished(() => {
    vi.restoreAllMocks();
  });
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Upload of img failed. boom")).toBeInTheDocument();
  expect(screen.queryByText(/^Uploading img/)).not.toBeInTheDocument();
});

it("does not send the file when the check rejects it", async () => {
  server.use(
    http.post(apiUrl("/volumes/import/check"), () =>
      HttpResponse.json({ code: "insufficient_storage", message: "needs 2 GiB" }, { status: 507 }),
    ),
  );
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  expect(
    await screen.findByText(
      "Upload of img failed. The node does not have enough free space for this upload.",
    ),
  ).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
  expect(screen.queryByRole("button", { name: "Cancel" })).not.toBeInTheDocument();
});

it("says when the check cannot reach the server", async () => {
  server.use(http.post(apiUrl("/volumes/import/check"), () => HttpResponse.error()));
  toasts();
  act(() => startUpload(new File(["x"], "img.raw"), PARAMS, i18n.t));
  expect(
    await screen.findByText("Upload of img failed. Cannot reach the server."),
  ).toBeInTheDocument();
  expect(FakeXhr.last).toBeUndefined();
});

it("stops waiting after two minutes without cancelling, and still reports a late answer", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  onTestFinished(() => {
    vi.useRealTimers();
  });
  toasts();
  // 빈 파일은 진행률 없이 다 보낸다.
  act(() => startUpload(new File([], "img.raw"), PARAMS, i18n.t));
  const xhr = await sending();
  act(() => xhr.uploaded());
  expect(await screen.findByText("Processing img…")).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(NO_RESPONSE_MS - 1000));
  expect(screen.getByText("Processing img…")).toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(1000));
  const waiting = await screen.findByText(
    "img: no response from the server. Check the volume list for the result.",
  );
  expect(within(waiting.closest("li")!).getByRole("button", { name: "Close toast" })).toBeVisible();
  // 요청은 끊지 않는다(끊으면 서버가 받은 볼륨을 지운다).
  expect(xhr.aborted).toBe(false);
  act(() => xhr.respond(202, task("t9", "volume-import", "v1")));
  expect(await screen.findByText("Import volume img")).toBeInTheDocument();
});
```

`web/src/storage/upload.ts` 전체:

```ts
import type { TFunction } from "i18next";
import { toast } from "sonner";

import {
  api,
  call,
  currentAuthEpoch,
  NetworkError,
  reportUnauthorized,
  toApiError,
} from "@/api/client";
import type { components } from "@/api/schema";
import type { Task } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

type Schemas = components["schemas"];

export interface UploadParams {
  name: string;
  pool: string;
  format: Schemas["ImageFormat"];
  sourceFormat: Schemas["SourceFormat"];
}

/** 파일 이름으로 볼륨 이름을 제안한다: `Ubuntu 24.04.vmdk` → `ubuntu-24-04` */
export function nameFromFile(file: string): string {
  return file
    .replace(/\.[^.]*$/, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, 63)
    .replace(/-+$/, "");
}

/** 확장자로 원본 형식을 짐작한다. 모르면 raw다(`.img`, `.raw`, `.iso`). */
export function sourceFormatOf(file: string): Schemas["SourceFormat"] {
  const ext = file.toLowerCase().split(".").pop();
  return ext === "vmdk" ? "vmdk" : ext === "qcow2" ? "qcow2" : "raw";
}

/** 다 보낸 뒤 이만큼 응답이 없으면 알림을 바꾼다. 요청은 끊지 않는다(끊으면 서버가 받은 볼륨을 지운다). */
export const NO_RESPONSE_MS = 2 * 60 * 1000;

/**
 * 본문을 보내기 전에 서버가 받을 수 있는지 묻는다(`/volumes/import/check`, 204). 권한·이름·풀·공간 따위로
 * 거절하면 `ApiError`를 던진다.
 */
export async function checkUpload(size: number, params: UploadParams, signal?: AbortSignal) {
  await call(
    api.POST("/volumes/import/check", {
      params: {
        query: {
          name: params.name,
          pool: params.pool,
          format: params.format,
          source_format: params.sourceFormat,
          size,
        },
      },
      signal,
    }),
  );
}

/**
 * 이미지를 `/volumes/import`로 올린다. fetch는 올리는 진행률을 알려 주지 않아 `XMLHttpRequest`를 쓴다.
 * 본문을 다 보내면 `onSent`를 부른다(빈 파일이어도). 받은 Task(202)로 끝나고, 오류 응답은 `ApiError`, 연결 실패는 `NetworkError`, 취소는 `signal`의 이유로
 * 거부한다. 중간에 끊기면 서버가 만들던 볼륨과 받은 파일을 지운다.
 */
export function uploadImage(
  file: Blob,
  params: UploadParams,
  {
    onProgress,
    onSent,
    signal,
  }: {
    onProgress?(loaded: number, total: number): void;
    onSent?(): void;
    signal?: AbortSignal;
  },
): Promise<Task> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const query = new URLSearchParams({
      name: params.name,
      pool: params.pool,
      format: params.format,
      source_format: params.sourceFormat,
    });
    const xhr = new XMLHttpRequest();
    const sentAt = currentAuthEpoch();
    const onAbort = () => xhr.abort();
    signal?.addEventListener("abort", onAbort);
    const done = () => signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", `${window.location.origin}/api/v1/volumes/import?${query}`);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress?.(e.loaded, e.total || file.size);
    xhr.upload.onload = () => onSent?.();
    xhr.onload = () => {
      done();
      let body: unknown = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        // 프록시의 HTML 오류 페이지 따위
      }
      if (xhr.status === 202) {
        // Task가 아닌 본문(프록시 따위)은 실패로 본다.
        if (typeof (body as Partial<Task> | null)?.id === "string") {
          resolve(body as Task);
        } else {
          reject(toApiError(xhr.status, body));
        }
        return;
      }
      if (xhr.status === 401) {
        reportUnauthorized(sentAt);
      }
      reject(toApiError(xhr.status, body));
    };
    xhr.onerror = () => {
      done();
      reject(new NetworkError(new Error("upload failed")));
    };
    xhr.onabort = () => {
      done();
      reject(signal?.reason ?? new DOMException("upload aborted", "AbortError"));
    };
    xhr.send(file);
  });
}

/** 진행 중인 업로드: 알림 id → 취소. `silent`면 취소를 알리지 않는다. */
const active = new Map<string, { controller: AbortController; silent: boolean }>();

/** 올리는 중에 페이지를 떠나려 하면 브라우저가 확인한다. */
function guard(e: BeforeUnloadEvent) {
  e.preventDefault();
  // 옛 브라우저
  e.returnValue = "";
}

/** 업로드를 모두 취소한다. 로그아웃하거나 세션이 끝나면 부른다(다음 사용자에게 이어지지 않게). */
export function resetUploads() {
  for (const upload of active.values()) {
    upload.silent = true;
    upload.controller.abort();
  }
}

/**
 * 업로드를 시작하고 진행률과 취소 버튼을 알림으로 보여 준다. 화면을 옮겨도 계속된다. 다 올리면 볼륨을
 * 채우는 Task를 `trackTask`로 넘긴다.
 */
export function startUpload(file: File, params: UploadParams, t: TFunction) {
  const id = `upload-${crypto.randomUUID()}`;
  const controller = new AbortController();
  const cancel = { label: t("confirm.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
    window.addEventListener("beforeunload", guard);
  }
  const upload = { controller, silent: false };
  active.set(id, upload);
  let shown = -1;
  let processing = false;
  let waiting: ReturnType<typeof setTimeout> | undefined;
  // 다 보냈다. 서버가 파일을 쓰고 Task를 만드는 동안은 취소하지 않는다.
  const sentAll = () => {
    processing = true;
    toast.loading(t("uploads.processing", { name: params.name }), {
      id,
      action: undefined,
      duration: Infinity,
    });
    // 오래 답이 없으면 닫을 수 있는 알림으로 바꾼다. 늦게 오는 응답은 평소처럼 알린다.
    waiting = setTimeout(() => {
      toast.warning(t("uploads.noResponse", { name: params.name }), {
        id,
        duration: Infinity,
        closeButton: true,
      });
    }, NO_RESPONSE_MS);
  };
  const show = (loaded: number, total: number) => {
    if (processing) {
      return;
    }
    // 화면 낭독기가 덜 읽도록 5% 단위로만 바꾼다.
    const percent = total > 0 ? Math.floor((loaded / total) * 20) * 5 : 0;
    if (percent === shown) {
      return;
    }
    shown = percent;
    toast.loading(
      t("uploads.progress", {
        name: params.name,
        percent,
        loaded: formatBytes(loaded),
        total: formatBytes(total),
      }),
      { id, action: cancel, duration: Infinity },
    );
  };
  show(0, file.size);
  // 확인에서 거절되면 본문을 보내지 않는다.
  let sent = false;
  void checkUpload(file.size, params, controller.signal)
    .then(() => {
      sent = true;
      return uploadImage(file, params, {
        onProgress: show,
        onSent: sentAll,
        signal: controller.signal,
      });
    })
    .then((task) => {
      toast.dismiss(id);
      trackTask(task, t("tasks.op.volume-import", { target: params.name }));
    })
    // 넘기다 던져도 알림이 "올리는 중"에 남지 않게 여기서 함께 받는다.
    // `trackTask`에서 던졌다면 서버의 가져오기 Task는 이미 돌고 있을 수 있다.
    .catch((err: unknown) => {
      // 같은 id로 바꾸는 알림은 이전 옵션을 물려받으므로 취소 버튼을 지운다.
      if (upload.silent) {
        toast.dismiss(id);
      } else if (controller.signal.aborted) {
        toast.info(t("uploads.cancelled", { name: params.name }), {
          id,
          action: undefined,
          duration: 4000,
        });
      } else {
        // 서버가 본문을 다 받기 전에 거절해도(401·409·507 따위) 브라우저는 연결 끊김으로만 알린다.
        const error =
          sent && err instanceof NetworkError ? t("uploads.dropped") : errorMessage(t, err);
        toast.error(t("uploads.failed", { name: params.name, error }), {
          id,
          action: undefined,
          duration: Infinity,
          closeButton: true,
        });
      }
    })
    .finally(() => {
      clearTimeout(waiting);
      active.delete(id);
      if (active.size === 0) {
        window.removeEventListener("beforeunload", guard);
      }
    });
}
```

`web/src/test/fakeXhr.ts`에서

```ts
  aborted = false;
  upload: { onprogress: ((e: { loaded: number; total: number }) => void) | null } = {
    onprogress: null,
  };
  onload: (() => void) | null = null;
```

을 다음으로 바꾼다:

```ts
  aborted = false;
  upload: {
    onprogress: ((e: { loaded: number; total: number }) => void) | null;
    onload: (() => void) | null;
  } = { onprogress: null, onload: null };
  onload: (() => void) | null = null;
```

`web/src/test/fakeXhr.ts`에서

```ts

  respond(status: number, body: unknown) {
```

을 다음으로 바꾼다:

```ts

  /** 본문을 다 보냈다(`xhr.upload`의 load). */
  uploaded() {
    this.upload.onload?.();
  }

  respond(status: number, body: unknown) {
```

`web/src/test/setup.ts`에서

```ts
import i18n from "@/i18n";
import { server } from "@/test/server";
```

을 다음으로 바꾼다:

```ts
import i18n from "@/i18n";
import { FakeXhr } from "@/test/fakeXhr";
import { server } from "@/test/server";
```

`web/src/test/setup.ts`에서

```ts
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
beforeEach(() => i18n.changeLanguage("en"));
afterEach(() => {
```

을 다음으로 바꾼다:

```ts
beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
beforeEach(async () => {
  await i18n.changeLanguage("en");
  // 앞 테스트의 요청이 남아 있으면 "XHR을 보내지 않았다"를 확인할 수 없다.
  FakeXhr.last = undefined;
});
afterEach(() => {
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  expect(screen.getByText("At least 64 MiB.")).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("At least 64 MiB.")).toBeInTheDocument();
});

it("follows the chosen node's architecture until a machine is picked", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = {
    ...NODE,
    id: "n2",
    name: "node-b",
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ node: [arm, x86] });
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  const machine = await screen.findByLabelText("Machine type");
  // 아키텍처가 섞여 있으면 q35다.
  expect(machine).toHaveValue("q35");
  await user.selectOptions(screen.getByLabelText("Node"), "node-a");
  expect(machine).toHaveValue("virt");
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("q35");
  // 직접 고르면 노드를 바꿔도 그대로다.
  await user.selectOptions(machine, "virt");
  await user.selectOptions(screen.getByLabelText("Node"), "");
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(machine).toHaveValue("virt");
});

it("keeps the machine of an edited VM when its node changes", async () => {
  const arm = { ...NODE, status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = {
    ...NODE,
    id: "n2",
    name: "node-b",
    status: { ...NODE.status, arch: "x86_64" as const },
  };
  mockLists({ vm: [vm("web", "stopped")], node: [arm, x86] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const machine = await screen.findByLabelText("Machine type");
  expect(machine).toHaveValue("virt");
  await screen.findByRole("option", { name: "node-b" });
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  expect(screen.getByLabelText("Node")).toHaveValue("n2");
  expect(machine).toHaveValue("virt");
});
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  const [busy, setBusy] = useState(false);
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
```

을 다음으로 바꾼다:

```tsx
  const [busy, setBusy] = useState(false);
  // 사용자가 머신을 직접 고르기 전에는 노드를 바꾸면 그 노드에 맞게 기본값을 다시 정한다(만들 때만).
  const [machineChosen, setMachineChosen] = useState(false);
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
                value={values.machine}
                onChange={(e) => set("machine", e.target.value as VmFormValues["machine"])}
              >
```

을 다음으로 바꾼다:

```tsx
                value={values.machine}
                onChange={(e) => {
                  setMachineChosen(true);
                  set("machine", e.target.value as VmFormValues["machine"]);
                }}
              >
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
                value={values.node}
                onChange={(e) => set("node", e.target.value)}
              >
```

을 다음으로 바꾼다:

```tsx
                value={values.node}
                onChange={(e) => {
                  const node = e.target.value;
                  setValues((v) => ({
                    ...v,
                    node,
                    machine:
                      creating && !machineChosen
                        ? defaultMachine(nodes.data ?? [], node)
                        : v.machine,
                  }));
                }}
              >
```

`web/src/vms/form.test.ts`에서

```ts

it("picks virt only when every node is Apple Silicon", () => {
  const hvf = { ...NODE, status: { ...NODE.status, hypervisor: "qemu-hvf" } };
  const kvm = NODE;
  expect(defaultMachine([hvf])).toBe("virt");
  expect(defaultMachine([hvf, kvm])).toBe("q35");
  expect(defaultMachine([])).toBe("q35");
```

을 다음으로 바꾼다:

```ts

it("picks the machine from the node architecture", () => {
  const arm = { ...NODE, id: "n2", status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = { ...NODE, id: "n3", status: { ...NODE.status, arch: "x86_64" as const } };
  const unknown = NODE;
  // 지정 노드의 아키텍처가 먼저다.
  expect(defaultMachine([arm, x86], "n2")).toBe("virt");
  expect(defaultMachine([arm, x86], "n3")).toBe("q35");
  // 자동이거나 지정 노드의 아키텍처를 모르면, 아는 노드가 모두 같을 때 그것이다.
  expect(defaultMachine([arm, unknown])).toBe("virt");
  expect(defaultMachine([arm, unknown], "n1")).toBe("virt");
  expect(defaultMachine([arm, x86])).toBe("q35");
  expect(defaultMachine([unknown])).toBe("q35");
  expect(defaultMachine([])).toBe("q35");
```

`web/src/vms/form.ts`에서

```ts

/**
 * 노드의 아키텍처를 API가 알려 주지 않으므로, 모든 노드가 hvf(Apple Silicon)면 `virt`, 아니면 `q35`를
 * 기본값으로 한다. 다른 경우는 사용자가 고른다.
 */
export function defaultMachine(nodes: Node[]): Schemas["MachineType"] {
  return nodes.length > 0 && nodes.every((n) => n.status.hypervisor === "qemu-hvf")
    ? "virt"
    : "q35";
}
```

을 다음으로 바꾼다:

```ts

const MACHINE_OF: Record<Schemas["Arch"], Schemas["MachineType"]> = {
  x86_64: "q35",
  aarch64: "virt",
};

/**
 * 머신 기본값: 지정 노드(`selectedNode`, 빈 문자열이면 자동)의 아키텍처, 아니면 아키텍처를 아는 노드가 모두
 * 같을 때 그 아키텍처의 머신, 그 밖에는 `q35`다.
 */
export function defaultMachine(nodes: Node[], selectedNode = ""): Schemas["MachineType"] {
  const chosen = nodes.find((n) => n.id === selectedNode)?.status.arch;
  if (chosen) {
    return MACHINE_OF[chosen];
  }
  const [only, ...others] = new Set(nodes.flatMap((n) => (n.status.arch ? [n.status.arch] : [])));
  return only && others.length === 0 ? MACHINE_OF[only] : "q35";
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 236개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 업로드 사전 확인·처리 대기 제한과 노드 아키텍처로 머신 기본값"
```

---

### Task 8: 문서

README에 "로그인과 세션"(시도 제한과 429, 프록시 뒤의 IP, IPv6 /64, 비밀번호 변경이 모든 노드의 세션을 끊지만 API 토큰과 열린 콘솔은 남음, 버전이 섞인 클러스터는 지원하지 않음)과 "이미지 업로드와 머신 종류"(사전 확인, 2분 처리 대기, 노드 아키텍처로 정하는 머신 기본값과 섞인 클러스터의 오류, `machine_arch_mismatch`, arch를 모르는 노드는 다시 등록할 때까지 자동 배치 후보가 아님)를 더한다. 설계 문서 §2–§8에 구현하며 정한 것(`ClientIp`와 `tap_io`, `begin`과 잠금 식, 성공 때 IP 기록에서 그 시도만 빼기, 넘칠 때의 순서, 세대는 비밀번호를 바꿀 때만, 업로드의 411 순서와 메시지, `check_node_arch`를 쓰는 곳, oplctl은 `--machine`이 없을 때만 노드 목록을 읽음, 웹의 아키텍처 열과 재계산)을 적는다.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-28-api-security-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

## 개발
```

을 다음으로 바꾼다:

```markdown

## 로그인과 세션

- **시도 제한.** 같은 사용자 이름·IP로 5번 연속 실패하면 30초, 같은 IP에서 (사용자 이름과 상관없이) 20번
  연속 실패하면 30초 동안 막힌다. 풀린 뒤에도 실패가 이어지면 실패할 때마다 막는 시간이 두 배가 된다(최대 15분).
  막힌 동안에는 맞는 비밀번호도 **429** `too_many_attempts`("try again in N seconds", `Retry-After` 헤더)를
  받는다. 로그인에 성공하면 그 사용자 이름·IP의 기록은 지워지지만, IP의 기록에서는 그 시도 하나만 빠진다(자기
  계정으로 로그인해 IP의 실패 수를 지울 수 없다). 마지막 실패 뒤 15분이 넘게 지나면 기록이 지워진다. IPv6 주소는
  /64 단위로 센다. 자기 비밀번호를 바꿀 때의 현재
  비밀번호 확인도 같은 기록으로 센다. 노드마다 따로 세고, opld를 다시 시작하면 기록이 사라진다.
- **프록시 뒤.** IP는 TLS 연결의 주소다(`X-Forwarded-For`는 보지 않는다). 리버스 프록시 뒤에 두면 모든 요청이
  프록시 IP로 보이므로, 한 사람이 20번 틀리면 프록시를 거치는 모든 사용자가 함께 막힌다.
- **비밀번호 변경.** 비밀번호를 바꾸면(본인이든 admin이든) 모든 노드에서 그 사용자의 로그인 세션이 끝나 다시
  로그인해야 한다. 역할만 바꾸면 세션은 그대로다. **API 토큰은 끊기지 않는다.** 토큰이 새었을 수 있으면
  `oplctl token list`·`oplctl token delete`(웹: API 토큰 화면)로 따로 지운다. 열려 있는 콘솔(WebSocket)은 연결할
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.

## 이미지 업로드와 머신 종류

- **업로드 사전 확인.** 웹과 `oplctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에 있는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
- **처리 대기.** 웹에서 파일을 다 보낸 뒤 서버가 2분 동안 응답하지 않으면 "처리 중" 알림이 "<이름>: 서버
  응답이 없습니다. 볼륨 목록에서 결과를 확인하세요."로 바뀌고 닫을 수 있다. 업로드는 끊지 않고 서버에서 계속된다.
  나중에 응답이 오면 평소처럼 결과를 알린다.
- **머신 기본값.** 노드는 시작할 때 자기 아키텍처(x86_64·aarch64)를 등록한다(노드 목록의 "아키텍처" 열).
  머신 종류를 정하지 않으면 웹과 `oplctl vm create`는 지정한 노드의 아키텍처를, 노드를 정하지 않았으면 클러스터
  노드가 모두 같은 아키텍처일 때 그것을 쓴다(x86_64 → `q35`, aarch64 → `virt`). 아키텍처가 섞인 클러스터에서
  노드를 정하지 않으면 oplctl은 `--machine q35` 또는 `--machine virt`를 달라는 오류를 내고, 웹은 `q35`를 고른다.
- **배치.** 자동 배치는 머신 종류와 아키텍처가 같은 노드만 고른다. 지정한 노드와 머신이 맞지 않으면 **400**
  `machine_arch_mismatch`다(VM을 시작할 때도 확인한다). 버전이 섞인 클러스터는 지원하지 않으므로 모든 노드를 함께
  올린다. 새 opld로 다시 시작해 아키텍처를 등록하기 전의 노드는 자동 배치 후보가 아니다(직접 지정하면 쓸 수 있다).

## 개발
```

`docs/superpowers/specs/2026-10-28-api-security-design.md` 전체:

```markdown
# opLite API·보안 보완 설계 (계획 13)

작성일: 2026-10-28
상위 문서: `2026-10-20-opl-node-core-design.md`, `2026-10-27-robustness-design.md`

## 1. 목표와 범위

계획 5·8·9에서 넘긴 API·보안 사항을 마친다.

| 항목 | 내용 |
|---|---|
| 시도 제한 | 로그인과 현재 비밀번호 확인의 실패 횟수를 노드마다 세어 막는다 |
| 세션 세대 | 비밀번호가 바뀌면 모든 노드의 옛 세션을 끊는다 |
| 업로드 사전 확인 | 본문을 보내기 전에 거절 이유를 알려 주는 API, 웹의 처리 대기 제한 시간 |
| 노드 아키텍처 | `NodeStatus.arch`, 머신 기본값(웹·oplctl), 스케줄러와 지정 노드 검증 |

넣지 않는 것: 클러스터 전체에서 세는 시도 제한(실패마다 Raft에 쓰면 공격이 곧 쓰기 부하다), 프록시 헤더(`X-Forwarded-For`) 신뢰, API 토큰을 비밀번호 변경과 함께 끊기, 감사 로그. 계획 14(웹 정리)와 공유 스토리지는 그 뒤다.

## 2. 시도 제한

`crates/opl-api/src/limit.rs`의 `AttemptLimiter`. `AppState`가 노드마다 하나 가진다(세션도 노드별이다). 노드가 N개면 공격자가 시도할 수 있는 횟수도 N배지만, 이 플랫폼의 규모(노드 3–5개)에서는 받아들인다.

- **키 두 종류.**
  - (소문자로 바꾼 사용자 이름, IP): 연속 5번 실패하면 30초 막는다.
  - IP만: 연속 20번 실패하면 30초 막는다. 한 IP에서 사용자 이름을 바꿔 가며 시도하는 것과 argon2 계산으로 CPU를 쓰게 하는 것을 막는다.
  - IP는 IPv4-mapped IPv6면 IPv4로 바꾸고(`to_canonical()`), IPv6는 /64 네트워크 하나로 센다(한 사용자가 쉽게 바꿔 쓸 수 있는 주소 묶음이다).
- **늘어나는 잠금.** 막힌 뒤 풀린 다음에도 실패가 이어지면 실패할 때마다 막는 시간을 두 배로 늘린다. 최대 15분이다. 곧 잠금 시간은 30초 × 2^(실패 수 − 한도)이고 15분을 넘지 않는다(5번째 실패에 30초, 6번째에 1분, …).
- **성공하면** 그 (사용자 이름, IP) 기록을 지우고, IP 기록에서는 이번 시도로 센 1만 뺀다(그 시도로 IP 키가 한도에 닿아 걸린 잠금도 푼다). IP 기록을 지우지 않는 것은 자기 계정으로 로그인해 IP의 실패 수를 처음으로 돌리는 우회를 막기 위해서다.
- **기록 정리.** 마지막 시도 뒤 15분이 **넘게** 지난 기록은 지운다(시도를 셀 때 함께). 성공은 시도 시각을 바꾸지 않으므로 사실상 마지막 실패 기준이다. 최대 잠금과 같은 길이라, 15분 잠금이 막 풀린 뒤의 실패는 이어서 세어 다시 15분 막는다. 기록 수는 1만 개를 넘지 않는다. 넘치면 잠기지 않은 기록 가운데 마지막 시도가 가장 오래된 것부터 버리고, 그래도 넘치면 잠긴 기록 가운데 가장 오래된 것을 버린다(기록을 채워 잠금을 풀지 못하게).
- **판단 순서.** 막힌 상태면 해시를 계산하지 않고 바로 **429** `too_many_attempts`를 돌려준다. `Retry-After` 헤더에 남은 초를 넣고, 메시지에 "try again in N seconds"를 넣는다. 막히지 않았으면 **확인하기 전에 이 시도를 실패로 먼저 센다**(`AttemptLimiter::begin`: 두 키의 실패 수를 올리고 한도에 닿으면 잠근다). 막힘 확인과 세기를 한 잠금 안에서 하므로, argon2 확인을 기다리는 동안 들어온 동시 요청도 앞의 시도를 실패로 보고 막힌다(같은 사용자·IP로 동시에 20번 틀려도 5번만 비밀번호 확인까지 간다). 실패하면 따로 기록하지 않고, 성공하면 위처럼 되돌린다(`success`). 429로 돌려보낸 시도는 기록하지 않는다(마지막 시도 시각도 바꾸지 않는다). 그래서 막힌 동안 계속 두드려도 잠금이 늘거나 기록이 오래 남지 않는다.
- **적용 대상.**
  - `POST /auth/login`. 없는 사용자도 실패로 센다. 사용자 이름이 이름 형식(`opl_types`의 `validate_name`: 1–63자의 소문자·숫자·`-`)이 아니면 그런 사용자는 있을 수 없으므로 해시를 계산하지 않고 IP 키만 실패로 세어(`begin_ip`) 401 `invalid_credentials`를 돌려준다. 긴 이름으로 기록의 메모리를 늘리지 못하게 기록에는 형식을 통과한 이름만 들어간다. 있을 수 없는 이름이므로 응답 시간으로 드러나는 것도 없다.
  - 자기 비밀번호를 바꿀 때의 현재 비밀번호 확인(`PUT /users/{key}`의 `current_password`). 키는 (요청한 사용자 이름, IP)이고 로그인과 같은 기록을 쓴다. 새 비밀번호의 길이 확인(400) 뒤에 한다. 새 비밀번호가 너무 짧으면 시도 제한을 보지도 세지도 않는다.
- **값**은 상수다. 설정에 노출하지 않는다.
- **클라이언트 IP**는 TLS 리스너의 연결 주소(`into_make_service_with_connect_info::<SocketAddr>()`)다. 프록시 뒤에서는 모든 요청이 프록시 IP로 보인다(README에 적는다). IP를 얻을 수 없는 경우(테스트 라우터를 `oneshot`으로 부를 때)는 미지정 주소(`0.0.0.0`) 하나로 센다.
  - handler는 `limit.rs`의 `ClientIp` 추출기로 IP를 받는다. axum 0.8은 `Option<ConnectInfo<SocketAddr>>`를 추출기로 받을 수 없어서(`ConnectInfo`가 선택 추출을 구현하지 않는다), `ClientIp`가 요청 확장에서 `ConnectInfo<SocketAddr>`를 직접 찾고 없으면 `0.0.0.0`을 쓴다. 없을 때는 처음 한 번 `warn` 로그를 남긴다(테스트 라우터에서는 괜찮고, opld에서 보이면 리스너 설정이 빠진 것이다).
  - opld의 TLS 리스너는 axum이 연결 주소를 알 수 있는 타입이 아니다. 아무 일도 하지 않는 `tap_io(|_| {})`로 감싸면 axum이 `TapIo` 리스너의 `ConnectInfo<SocketAddr>`를 만들어 준다.
- 시계는 `tokio::time::Instant`를 써서 테스트가 시간을 멈추고 당길 수 있게 한다.

## 3. 세션 세대

- `UserSpec.session_epoch: u64`, `#[serde(default)]`(옛 데이터는 0). 0이면 쓰지 않는다(`skip_serializing_if`). `UserSpec`은 모르는 필드를 거절하므로, 세대가 0인 사용자를 옛 버전이 읽지 못하게 만들지 않으려는 것이다(버전이 섞인 클러스터를 지원한다는 뜻은 아니다 — 8절).
- 비밀번호가 바뀌면(본인이든 관리자든) 같은 쓰기에서 1을 올린다. 세대는 요청에 비밀번호가 있을 때만 올린다. 역할만 바꾸면 세션은 그대로다(역할은 요청마다 다시 읽으므로 바로 반영된다).
- 세션은 만들 때의 세대를 함께 저장한다.
- 쿠키로 인증할 때 Raft에서 다시 읽은 사용자의 세대와 비교한다(지금도 요청마다 사용자를 다시 읽는다). 다르면 그 세션을 지우고 401 `unauthorized`를 돌려준다. 그래서 다른 노드의 옛 세션도 다음 요청에서 끊긴다.
- 요청을 처리한 노드의 `sessions.remove_user`는 그대로 둔다.
- API 토큰은 끊지 않는다. 토큰은 사용자가 따로 지운다(문서에 적는다).

## 4. 업로드 사전 확인

- `import_volume` 앞부분의 확인을 `check_import(state, principal, params, size)`로 뽑는다. 문제가 없으면 풀을 돌려준다. 순서:
  1. 권한(Volume 쓰기)
  2. 이름 형식과 예약어 `import`
  3. 풀이 있는지
  4. 풀이 이 노드에 있는지(`pool_not_on_node`)
  5. 이름 중복(409 `name_taken`, 이름으로 조회, 메시지 `volume "x" already exists`)
  6. 공간: 크기 + 1 GiB를 남길 수 있는지(`insufficient_storage`)
- **`POST /volumes/import/check?name&pool&format&source_format&size`**: `check_import`만 부르고 문제가 없으면 204. 본문은 받지 않는다. OpenAPI에 등록한다. 쿼리는 업로드의 `ImportQuery`에 `size`를 더한 별도 구조체 `ImportCheckQuery`다(업로드 쿼리에 `size`를 받지 않게 하려고 나눈다. 둘 다 모르는 매개변수는 거절한다).
- 실제 업로드도 `check_import`를 부른다(크기는 `Content-Length`, 없으면 411 `length_required`). `Content-Length`가 없으면 크기를 알 수 없으므로 411을 권한·이름·풀 확인보다 **먼저** 돌려준다(전에는 풀 확인 뒤였다). 확인과 업로드 사이에 다른 요청이 이름을 가져갈 수 있으므로 Raft에 만들 때의 `name_taken`도 그대로 둔다.
- **웹**(`web/src/storage/upload.ts`): XHR 전에 확인 API를 부르고, 거절이면 업로드하지 않고 오류 코드 문구로 실패 토스트를 보인다. 확인 요청이 서버에 닿지 못하면(네트워크 오류) "업로드 거절 또는 끊김"이 아니라 `errors.network`("Cannot reach the server.")를 보인다.
- **처리 대기 제한 시간(웹).** 모든 바이트를 보낸 뒤(`xhr.upload`의 `load`. 진행률 이벤트가 없는 빈 파일도) "처리 중" 토스트로 바꾸고, 2분 안에 응답이 없으면 `uploads.noResponse`("{{name}}: 서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요.")로 바꾸고 닫을 수 있게 한다. XHR은 끊지 않는다(끊으면 서버의 `UploadGuard`가 받은 볼륨을 지운다). 나중에 응답이 오면 평소처럼 결과 토스트를 띄운다.
- **oplctl**(`volume import`): 파일 크기로 확인 API를 먼저 부르고, 거절이면 파일을 보내지 않는다.

## 5. 노드 아키텍처

- **타입.** `Arch { X86_64, Aarch64 }`(serde `x86_64`/`aarch64`)를 opl-types로 옮긴다. `native()`(컴파일된 아키텍처)와 `machine()`도 함께 옮기고, opl-qemu는 opl-types 것을 쓴다. `MachineType::arch()`(`q35` → `x86_64`, `virt` → `aarch64`)를 둔다. `NodeStatus.arch: Option<Arch>`, `#[serde(default)]`(옛 노드는 `None`).
- **opld.** `register_node`가 이 노드의 `HostProfile.arch`로 채운다.
- **스케줄러.** 후보 조건에 "노드 arch == VM 머신의 arch"를 더한다. arch가 `None`인 노드는 후보가 아니다. 맞는 노드가 없으면 지금처럼 409 `no_node`다. 그 메시지에 조건으로 "머신 타입의 아키텍처"를 더한다("… has the architecture of its machine type, …").
- **지정 노드 검증.** VM을 만들거나 고칠 때(같은 검증 함수 `vms::prepare`) 지정 노드의 arch가 있고 머신의 arch와 다르면 400 `machine_arch_mismatch`. arch를 모르는 노드는 통과시킨다(새 opld가 시작해 arch를 등록하기 전에도 VM을 고칠 수 있게. 버전이 섞인 클러스터는 지원하지 않는다 — 8절). 판단은 `vms::check_node_arch` 하나로 한다. 스냅샷 되돌리기도 되돌린 스펙을 같은 함수로 검증하므로(노드는 지금 것) 같은 400을 받는다.
  - VM을 시작할 때(`POST /vms/{key}/actions/start`) 고정된 노드도 같은 함수로 확인한다. arch를 모르던 때 고정했을 수 있다.
  - 메모리 스냅샷 되돌리기가 배치되지 않은 VM을 이 노드에 고정할 때(`tasks/snapshot.rs`의 `write_reverted_spec`) 이 노드의 arch가 머신과 다르면 고정하지 않고 Task를 실패로 끝낸다. 메모리 상태는 이 노드에서만 이어 쓸 수 있으므로 다른 노드로 보내지 않는다. 디스크를 건드리기 전이다.
- **oplctl.** `--machine`이 없으면 노드 목록(`GET /nodes`)을 읽어 정한다. `--machine`을 주면 노드 목록을 읽지 않는다. 지정 노드에 arch가 있으면 그것, 아니면 arch를 아는 노드가 모두 같은 arch일 때 그것. 그 밖에는 오류로 `--machine`을 달라고 한다. 클라이언트의 컴파일 아키텍처로 정하던 코드는 지운다.
- **웹.** `defaultMachine(nodes, selectedNode)`: 지정 노드의 arch → arch를 아는 노드가 모두 같으면 그 arch → `q35`. hvf로 추측하던 코드는 지운다. 노드를 바꾸면 사용자가 머신을 직접 고르지 않았을 때만 기본값을 다시 정한다. 다시 정하는 것은 만들기 화면뿐이다(편집 화면은 VM의 지금 머신을 그대로 둔다). 웹에는 노드 상세 화면이 없으므로 "아키텍처"는 노드 목록의 열로만 보인다(모르면 —).

## 6. 클라이언트 문구

- 웹 `errors.too_many_attempts`: ko "시도가 너무 많습니다. 잠시 뒤에 다시 하세요." / en "Too many attempts. Try again later."
- 웹 `errors.machine_arch_mismatch`: ko "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다." / en "The machine type doesn't match the selected node's architecture."
- 웹 `uploads.noResponse`: ko "{{name}}: 서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요." / en "{{name}}: no response from the server. Check the volume list for the result."
- oplctl은 서버 메시지를 그대로 보인다.

## 7. 테스트

- `AttemptLimiter` 단위 테스트(멈춘 시계): 5번째 실패 뒤 막힘, 30초 뒤 풀림, 이어진 실패로 두 배, 최대 15분, 성공하면 초기화, IP 키 20번, 정리와 상한.
- API: 로그인 6번째 시도가 429와 `Retry-After`; 막힌 동안 맞는 비밀번호도 429; 현재 비밀번호 확인도 막힘.
- 세션 세대: 노드 둘(같은 클러스터의 `AppState` 둘)에서 한쪽이 비밀번호를 바꾸면 다른 쪽의 옛 쿠키가 401.
- 업로드 확인: 각 거절 코드, 통과 204, 업로드의 이름 중복이 볼륨을 만들기 전에 409.
- 아키텍처: 스케줄러가 arch가 다르거나 `None`인 노드를 빼는지, 지정 노드가 맞지 않으면 400, 모르면 통과, opld 등록이 arch를 채우는지.
- oplctl과 웹: 기본값 함수, 웹 업로드의 확인 거절(XHR을 보내지 않음)과 2분 제한 시간(가짜 타이머).

## 8. 알려진 한계

- 시도 제한은 노드마다 따로 센다. opld가 다시 시작하면 기록이 사라진다.
- 프록시 뒤에서는 모든 사용자가 한 IP로 보여, IP 키가 정상 사용자를 함께 막을 수 있다.
- API 토큰은 비밀번호를 바꿔도 살아 있다.
- 열린 WebSocket 콘솔은 비밀번호를 바꿔도 끊기지 않는다(연결할 때 한 번 인증한다).
- 버전이 섞인 클러스터는 지원하지 않는다. 모든 노드를 함께 올린다(`robustness-design.md`와 같다).
- arch를 모르는 노드(옛 버전)는 자동 배치 후보가 아니다. 모든 노드가 새 버전으로 다시 등록하면 풀린다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 388개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e`
Expected: PASS 5개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 로그인 시도 제한·세션·업로드 확인·머신 기본값 안내와 구현하며 정한 것"
```

---

## 완료 기준 (계획 13)

- 같은 사용자·IP의 6번째 로그인이 429와 `Retry-After`로 거절되고, 동시 요청 20개 가운데 15개가 429이며, 성공이 IP 기록에서 자기 시도만 빼고, 현재 비밀번호 확인도 같은 기록으로 막힌다(`cargo test -p opl-api --test auth`와 `limit` 단위 테스트).
- 한 노드에서 비밀번호를 바꾸면 다른 노드의 옛 쿠키가 401이고 API 토큰은 살아 있다.
- `POST /volumes/import/check`가 업로드와 같은 거절 code를 돌려주고 통과하면 204이며, oplctl과 웹은 거절이면 파일을 보내지 않는다.
- 노드가 아키텍처를 등록하고, 스케줄러와 VM 검증이 머신과 맞지 않는 노드를 거절하며, oplctl·웹의 머신 기본값이 노드 아키텍처를 따른다. 실제 QEMU(`cargo test -p opld --features qemu-it --test smoke --test e2e`)에서 `--machine` 없이 만든 VM이 뜬다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **노드별 기록:** 시도 제한은 노드마다 따로 세고 opld가 다시 시작하면 사라진다. 노드가 N개면 시도할 수 있는 횟수도 N배다.
- **프록시 뒤:** 모든 요청이 프록시 IP로 보여 IP 키(20번)가 프록시 뒤의 모든 사용자를 함께 막을 수 있다.
- **API 토큰과 콘솔:** 비밀번호를 바꿔도 API 토큰과 이미 열린 WebSocket 콘솔은 끊기지 않는다.
- **arch를 모르는 노드:** 옛 버전으로 등록한 노드는 새 opld로 다시 시작해 등록할 때까지 자동 배치 후보가 아니다(지정은 된다).
- **섞인 클러스터:** 옛 버전과 섞인 클러스터는 지원하지 않는다.
- **웹 처리 대기:** 2분 뒤에도 서버 응답이 없으면 알림만 바뀌고 업로드 결과는 볼륨 목록에서 확인한다.
- 그 밖의 제한은 설계 문서 §8에 있다.
