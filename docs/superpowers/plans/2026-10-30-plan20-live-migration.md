# opLite 계획 20: 라이브 마이그레이션 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 디스크가 모두 공유 풀(공유 디렉터리·NFS·RBD·공유 LVM)에 있는 실행 중 VM을 다른 노드로 멈추지 않고 옮긴다. QEMU가 노드 인증서로 TLS 연결을 맺어 메모리를 직접 보내고, Raft에 VM의 노드를 바꾸는 순간이 확정이다.

**Architecture:** API(`POST /vms/{key}/migrate`)가 검사 뒤 원본 노드에 `VmMigrate` Task를 만든다. 원본 Task가 reconciler 워커를 보류하고 `Migrating`을 기록한 뒤, 노드 사이 RPC(`MigrateIncoming`)로 대상 노드에 `-incoming defer` QEMU를 띄우게 하고, QMP `migrate`로 보내고, 대상의 받기 완료(`MigrateIncomingWait`)를 확인한 다음 revision 검사로 `spec.node`를 대상으로 바꾼다. 대상 워커가 넘겨받아 `cont`하고, 원본은 `postmigrate` QEMU를 끈다. 확정 전 실패는 대상 QEMU를 끄고(`MigrateIncomingAbort`) 원본을 재개한다. 두 QEMU가 함께 도는 일은 없다(대상은 `-S`로 떠서 확정 뒤에만, 원본은 확정 전에만 재개). 공유 LVM은 마이그레이션 동안 LV 잠금을 공유로 바꾼다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8, utoipa, tonic(JSON 코덱 피어 서비스), clap, QEMU QMP(`migrate`·`migrate-incoming`·`tls-creds-x509`), LVM2 lvmlockd; 웹은 계획 8~19와 같다. 새 외부 의존성은 없다(rcgen은 opl-driver 시험에만). 실제 시험은 한 호스트의 opld 둘(이 Mac은 TCG, Linux CI는 KVM).

**설계 문서:** `docs/superpowers/specs/2026-10-30-live-migration-design.md` (§1~§4, §7·§8의 계획 20 부분, "구현:" 메모 포함 — §5·§6은 계획 21)
**선행 계획:** 계획 1~19 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `VmPhase::Migrating`, `VmStatus.migration`, `TaskOp::VmMigrate`, `POST /vms/{key}/migrate`와 검사, 마이그레이션 중 다른 동작 409 | 디스크 복사 마이그레이션(로컬 풀), 사후 복사, 취소 API |
| QEMU 마이그레이션 드라이버(받기·보내기·TLS·정리), 노드 사이 RPC 셋 | 다른 QEMU 버전·CPU 사이, macOS(HVF) 노드 |
| 원본 Task, reconciler 보류·넘겨받기·되돌리기·남은 QEMU 정리 | 멈춘 채 옮긴 VM의 UEFI 변수 따라가기 — 계획 21 |
| 공유 LVM의 잠금 공유↔배타 | 디렉터리 풀의 `shared` 바꾸기 — 계획 21 |
| 실제 시험(`migration-it`, 공유 디렉터리·NFS·RBD·공유 LVM), oplctl `vm migrate`, 웹 | HA·자동 재배치 |

## 이전 계획에서 이어받는 사항

- 계획 15~19: 공유 풀(`is_shared`), 마운터, 공유 블록 풀의 활성화 수명과 남은 활성화 정리(`deactivate_idle_volumes`, `tag_sweep`).
- 계획 19: `Storage::deactivate_volume`·`active_volumes`, 공유 LVM 배타 활성화(`-aey`), 크기 늘리기와 VM 시작의 규칙.
- 계획 3·10: 노드 사이 피어 서비스(mTLS, 회원 검사), 콘솔 중계, Task 실행기(VM 큐), 스냅샷·복제 Task.

## 실행 환경

계획 19와 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. **마이그레이션 실제 시험은 이 Mac에서 TCG로 돈다**(HVF에서는 보내는 QEMU가 가끔 `assert_hvf_ok`로 죽는다 — 그래서 API도 HVF 노드를 막는다). 한 호스트의 opld 둘은 systemd 스코프 이름과 탭 이름이 겹치므로 시험은 스코프를 끄고 브리지 NIC를 쓰지 않는다. NFS·RBD·공유 LVM 변형은 Linux CI(`linux-kvm`, QEMU 8.2, KVM)에서만 돈다(각 `*-it` 기능과 환경 변수).

CI에서 확인한 QEMU 8.2 동작: `tls-creds-x509`(dir에 `ca-cert.pem`·`server-cert.pem`·`server-key.pem`·`client-cert.pem`·`client-key.pem`)는 키 용도가 critical이면 keyEncipherment가 있어야 한다(노드 인증서에 더한다). 한 QEMU에 `migrate-incoming`은 한 번만 되므로 포트는 opld가 범위에서 미리 bind 해 보고 고른다. 완료 뒤 원본은 `postmigrate`, 대상은 `paused`(`-S`), 대상 `cont` 때 pflash(UEFI 변수)가 원본 내용으로 대상 파일에 쓰인다. 실패하면(연결 거부, TLS 불일치) 원본은 `running` 그대로다. lvmlockd는 쓰는 중인 LV도 `lvchange -asy`(배타→공유)·`-aey`(공유→배타)를 한다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`), `Cargo.lock`(빌드가 갱신).

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **원본 노드의 Task가 지휘한다.** 기존 Task 실행기(VM 큐, 재시작 때 "interrupted")를 쓰고, 원본 QEMU의 QMP가 그 노드에만 있다. 일반 Task 시간 제한 밖에서 돌고 단계마다 제한이 있다(보내기 `[migration] timeout`, RPC 120·180·60초).
- **확정은 revision 검사 쓰기 하나.** 확정(`spec.node = T`)과 되돌리기(`migration` 지우기)는 같은 revision을 검사하므로 하나만 적용된다. 원본은 되돌리기가 적용된 뒤에만, 대상은 확정 뒤 대상 워커만 재개한다. 원본의 `postmigrate` QEMU는 그 VM이 이 노드에서 돌던 중일 때만(상태 노드가 이 노드이고 Running·Migrating) 재개하고, 아니면 이미 확정된 마이그레이션의 옛 원본으로 보고 끈다.
- **QEMU가 직접 TLS로 보낸다.** 노드 인증서·키·클러스터 CA를 `{data_dir}/migration-tls/`에 쓰고 양쪽 `verify-peer`. 원본은 대상의 클러스터 회원 주소 호스트로 접속하고 그것을 `tls-hostname`으로, 대상은 `cluster_listen` IP에서 받는다.
- **받는 쪽 처리는 opl-api의 `IncomingMigrations`**(피어 서비스에 trait 객체로 주입). 받는 중 표시 파일에 Task id를 적어 남은 QEMU 정리가 대조한다.
- **남은 것 정리:** 원본 opld 재시작 때 워커가 `Migrating` VM의 진행 중 마이그레이션을 취소하고 되돌린다. Task 실행기가 시작 때와 `tag_sweep`마다 확정된 마이그레이션의 `postmigrate` 원본과 다른 Task의 받는 QEMU를 끈다.
- **공유 LVM:** 원본이 `Migrating` 기록 뒤 LV를 공유로, 대상은 공유로 켜고, 넘겨받은 뒤(그리고 opld 재시작·보류가 끝난 뒤) 워커가 배타로 되돌린다(성공할 때까지 다시 시도).
- **혼합 버전 클러스터**는 지원하지 않는다(새 RPC). 계획 20 전에 서명된 노드 인증서로는 QEMU 8.2의 마이그레이션 TLS가 실패한다(opld가 경고).

## 파일 구조

```
crates/opl-types/src/{vm.rs, task.rs}                              # Migrating, MigrationStatus, VmMigrate
crates/opl-api/src/{vms.rs, snapshots.rs, scheduler.rs, lib.rs}    # POST /vms/{key}/migrate, 마이그레이션 중 409
crates/opl-qemu/src/{qmp.rs, cmdline.rs}                           # 마이그레이션 QMP, -incoming defer
crates/opl-driver/src/{migration.rs, hypervisor.rs, qemu.rs, mock.rs}   # 받기·보내기·TLS 파일
crates/opl-peer/src/{migration.rs, server.rs, client.rs, pki.rs}   # RPC 셋, keyEncipherment
crates/opl-api/src/{incoming.rs, tasks.rs, tasks/migrate.rs}       # 받는 쪽, 원본 Task, 남은 QEMU 정리
crates/opl-reconciler/src/{reconciler.rs, worker.rs, resolve.rs}   # 보류, 넘겨받기, 되돌리기, 공유 켜기
crates/opl-driver/src/storage{.rs, /lvm.rs, /storages.rs}         # share_volume, exclusive_volume
crates/opld/src/{config.rs, member.rs}, crates/opld/tests/migration.rs (새로)   # [migration], 연결, migration-it
crates/oplctl/src/{cli.rs, client.rs}, web/src/{vms,pages}/..., web/src/i18n/{ko,en}.json
.github/workflows/ci.yml, README.md, docs/superpowers/specs/{2026-10-30-live-migration-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입과 API 규칙: 라이브 마이그레이션

`VmPhase::Migrating`(kebab `migrating`), `MigrationStatus { node, task }`, `VmStatus.migration`(없으면 빼고 쓰고 OpenAPI에서도 필수가 아니다), `TaskOp::VmMigrate { node }`(`vm-migrate`). `POST /vms/{key}/migrate` 본문 `{ "node": 이름 또는 id }`, `Operate` 권한, `vm_lock` 안에서 검사하고 원본 노드(VM의 `spec.node`)에 Task를 만든다. 검사 순서: 대상 없음 404, 같은 노드 400, 실행 중 아님 409 `vm_not_running`(마이그레이션 중이면 `vm_busy`), 바쁨 409 `vm_busy`(전원·스냅샷·되돌리기·크기 늘리기 Task), 공유 풀 아닌 디스크·HVF 노드(`hypervisor == "qemu-hvf"`) 409 `migration_unsupported`, 오프라인·유지보수 409 `node_unavailable`, 아키텍처 400 `machine_arch_mismatch`, 풀·브리지·메모리 409 `no_capacity`(`scheduler::node_fits`를 공개하고 메모리는 마이그레이션에서만 본다). 마이그레이션 중에는 VM의 다른 동작·`PUT`·지우기가 409 `vm_busy`(`check_no_migration`, `modify_spec`는 쓰기 직전에 다시 본다). Task 실행기의 `VmMigrate`는 아직 실패한다. opld 설정 `[migration]`(`ports = "60000-60099"`, `bandwidth_mib = 0`, `timeout = "30m"`). 웹은 문자열과 상태 배지.

**Files:**
- Create: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/tasks/tasks.test.tsx`
- Modify: `web/src/vms/VmPhaseBadge.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(vms::vm_action))
        .routes(routes!(snapshots::list_snapshots))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(vms::vm_action))
        .routes(routes!(vms::migrate_vm))
        .routes(routes!(snapshots::list_snapshots))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리
    fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
```

을 다음으로 바꾼다:

```rust
    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리
    pub fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        .iter()
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| n.status.arch == Some(spec.machine.arch()))
        .filter(|n| inv.reaches_disks(spec, n))
        .filter(|n| inv.has_bridges(spec, n))
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
```

을 다음으로 바꾼다:

```rust
        .iter()
        .filter(|n| node_fits(spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
        .ok_or(NoNode)
}
```

을 다음으로 바꾼다:

```rust
        .ok_or(NoNode)
}

/// [`schedule`]가 이 노드를 후보로 보는지. 아니면 그 이유다. 메모리는 보지 않는다.
pub fn node_fits(spec: &VmSpec, inv: &Inventory, node: &Node) -> Result<(), String> {
    let name = &node.name;
    if !node.status.online {
        return Err(format!("node {name:?} is offline"));
    }
    if node.spec.maintenance {
        return Err(format!("node {name:?} is in maintenance"));
    }
    if node.status.arch != Some(spec.machine.arch()) {
        let arch = format!("{:?}", spec.machine.arch()).to_lowercase();
        return Err(format!(
            "node {name:?} does not have the {arch} architecture of the VM's machine type"
        ));
    }
    if !inv.reaches_disks(spec, node) {
        return Err(format!(
            "node {name:?} cannot use the storage pools of the VM's disks"
        ));
    }
    if !inv.has_bridges(spec, node) {
        return Err(format!(
            "node {name:?} does not have the bridges of the VM's networks"
        ));
    }
    Ok(())
}
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
    fn node_fits_names_why_a_node_cannot_run_the_vm() {
        let pool = pool("nas", true);
        let ready = with_pool(node(8192), &pool, true);
        let inv = Inventory {
            nodes: vec![ready.clone()],
            volumes: vec![volume("root", "nas", None)],
            pools: vec![pool.clone()],
            networks: vec![Resource::new(
                "lan",
                NetworkSpec {
                    bridge: "vmbr0".into(),
                    vlan: None,
                },
            )],
            ..Default::default()
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(node_fits(&me, &inv, &ready), Ok(()));

        let mut offline = ready.clone();
        offline.status.online = false;
        assert_eq!(
            node_fits(&me, &inv, &offline),
            Err("node \"n\" is offline".into())
        );
        let mut maint = ready.clone();
        maint.spec.maintenance = true;
        assert_eq!(
            node_fits(&me, &inv, &maint),
            Err("node \"n\" is in maintenance".into())
        );
        let mut x86 = ready.clone();
        x86.status.arch = Some(Arch::X86_64);
        assert_eq!(
            node_fits(&me, &inv, &x86),
            Err(
                "node \"n\" does not have the aarch64 architecture of the VM's machine type".into()
            )
        );
        let not_ready = with_pool(node(8192), &pool, false);
        assert_eq!(
            node_fits(&me, &inv, &not_ready),
            Err("node \"n\" cannot use the storage pools of the VM's disks".into())
        );
        me.nics.push(NicSpec {
            network: "lan".into(),
            model: NicModel::Virtio,
            mac: None,
        });
        assert_eq!(
            node_fits(&me, &inv, &ready),
            Err("node \"n\" does not have the bridges of the VM's networks".into())
        );
    }
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다. 디스크 크기 늘리기도 같은 규칙이다.
pub(crate) fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
```

을 다음으로 바꾼다:

```rust
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다. 디스크 크기 늘리기도 같은 규칙이다. 마이그레이션 중인
/// VM도 같다([`check_no_migration`]).
pub(crate) fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_migration(state, vm)?;
    let settled = match vm.spec.power {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        )
    })
}
```

을 다음으로 바꾼다:

```rust
        )
    })
}

/// 마이그레이션 중인 VM(`migrating`이거나 끝나지 않은 마이그레이션 Task가 있다)은 다른 동작을 받지
/// 않는다: 409 `vm_busy`. 원본 QEMU를 끄거나 디스크·스펙을 바꾸면 옮기는 중인 VM이 어긋난다.
pub(crate) fn check_no_migration(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_not_migrating(vm)?;
    check_no_task(state, vm, "being migrated", |op| {
        matches!(op, TaskOp::VmMigrate { .. })
    })
}

/// 읽은 VM이 마이그레이션 중(`migrating`, 마이그레이션 표시)이면 409 `vm_busy`. 마이그레이션 Task는
/// 표시를 쓴 뒤에야 움직이므로, spec을 쓰기 직전에 다시 읽은 VM으로 보면 검사와 쓰기 사이에 시작한
/// 마이그레이션도 막는다([`check_no_migration`]의 Task 검사는 그 전에 지나갔을 수 있다).
pub(crate) fn check_not_migrating(vm: &Vm) -> Result<(), ApiError> {
    if vm.status.phase == VmPhase::Migrating || vm.status.migration.is_some() {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is migrating; try again when it has settled",
                vm.name
            ),
        ));
    }
    Ok(())
}
```

`crates/opl-api/src/tasks.rs`에서

```rust
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
    }
```

을 다음으로 바꾼다:

```rust
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
        TaskOp::VmMigrate { .. } => Err("VM migration is not supported yet".into()),
    }
```

`crates/opl-api/src/vms.rs` 전체:

```rust
//! VM 생성·수정·삭제와 동작(start, stop, reboot).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::network::USER_NETWORK;
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{VolumePhase, is_shared};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{MacAddr, PowerState, Vm, VmPhase, VmSpec, VmStatus};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::scheduler::{Inventory, node_fits, schedule};
use crate::snapshots::{
    check_no_migration, check_no_revert_task, check_no_snapshot_task, check_not_busy,
    check_not_migrating, count_snapshots,
};
use crate::state::AppState;
use crate::tasks::create_task;

/// 원하는 상태도 실제 상태도 꺼져 있다. 삭제와 노드·디스크 변경은 이때만 할 수 있다.
pub(crate) fn is_stopped(vm: &Vm) -> bool {
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
pub(crate) fn prepare(state: &AppState, id: Uuid, spec: &mut VmSpec) -> Result<(), ApiError> {
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

    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if !inv.networks.iter().any(|n| n.name == nic.network) {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
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
    if let Some(id) = spec.node {
        let Some(node) = inv.nodes.iter().find(|n| n.id == id) else {
            return Err(ApiError::bad_request(format!("node {id} not found")));
        };
        check_node_arch(node, spec)?;
    }
    if spec.power == PowerState::Running && spec.node.is_none() {
        spec.node = Some(
            schedule(id, spec, &inv).map_err(|e| ApiError::conflict("no_node", e.to_string()))?,
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
    let _lock = state.vm_lock.lock().await;
    prepare(&state, vm.id, &mut vm.spec)?;
    vm.revision = state.cluster.create(&vm).await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없다. 다른 변경은 다음 시작부터
/// 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
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
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let _lock = state.vm_lock.lock().await;
        let current = lookup::<VmSpec>(&state.cluster, &key)?;
        check_no_migration(&state, &current)?;
        check_no_snapshot_task(&state, &current)?;
        if let Some(expected) = expected
            && expected != current.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current.revision,
            })
            .into());
        }
        let mut spec = req.spec.clone();
        prepare(&state, current.id, &mut spec)?;
        if spec.node != current.spec.node && !is_stopped(&current) {
            return Err(vm_running(&current, "move"));
        }
        if spec.disks != current.spec.disks && !is_stopped(&current) {
            return Err(vm_running(&current, "change the disks of"));
        }
        match state
            .cluster
            .update_spec(
                current.id,
                Some(current.revision),
                req.labels.clone(),
                &spec,
            )
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
        return Ok(Json(lookup::<VmSpec>(
            &state.cluster,
            &current.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the VM kept changing while being updated; try again",
    ))
}

/// 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`, 끝나지 않은 스냅샷·복제
/// Task가 있으면 409 `vm_busy`(복제는 원본 VM과 그 스냅샷을 읽는다).
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_no_migration(&state, &vm)?;
    if !is_stopped(&vm) {
        return Err(vm_running(&vm, "delete"));
    }
    let snapshots = count_snapshots(&state, vm.id)?;
    if snapshots > 0 {
        return Err(ApiError::conflict(
            "vm_has_snapshots",
            format!(
                "VM {:?} has {snapshots} snapshot(s); delete them first",
                vm.name
            ),
        ));
    }
    check_no_snapshot_task(&state, &vm)?;
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
    #[param(inline)]
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
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 스냅샷 Task가 끝나지 않았으면 막는다. 시작한 스냅샷 Task는 실행기의 시간 제한으로 끝난다.
    // 강제 정지는 되돌리기만 막는다. 저장이 멈춘 만들기 중에도 VM을 끌 수 있어야 한다.
    // 마이그레이션 중에는 모두 막는다. 실패하면 원본이 계속 돌고, 시간 제한으로 끝난다.
    check_no_migration(&state, &vm)?;
    if query.mode == StopMode::Force {
        check_no_revert_task(&state, &vm)?;
    } else {
        check_no_snapshot_task(&state, &vm)?;
    }
    let id = vm.id;
    let (op, vm) = match action.as_str() {
        "start" => {
            let _lock = state.vm_lock.lock().await;
            let vm = modify_spec(&state, id, |spec| {
                // 고정된 노드는 VM을 고칠 때 확인했지만, 그때 arch를 모르던 노드일 수 있다.
                if let Some(node) = spec.node
                    && let Some(node) = state.cluster.get::<NodeSpec>(node)?
                {
                    check_node_arch(&node, spec)?;
                }
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

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateRequest {
    /// 옮겨 갈 노드(이름이나 id)
    pub node: String,
}

/// 실행 중인 VM을 다른 노드로 라이브 마이그레이션할 Task를 돌려준다(202). Task는 지금 노드의 실행기가
/// 한다. 디스크가 모두 공유 풀에 있어야 하고, HVF(macOS) 노드는 원본도 대상도 될 수 없다(409
/// `migration_unsupported`). 대상 노드가 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
#[utoipa::path(post, path = "/vms/{key}/migrate", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrateRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn migrate_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<MigrateRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    // 두 요청이 함께 검사를 지나 Task를 둘 만들지 않게 한다.
    let _lock = state.vm_lock.lock().await;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let target = lookup::<NodeSpec>(&state.cluster, &req.node)?;
    if vm.spec.node == Some(target.id) {
        return Err(ApiError::bad_request(format!(
            "VM {:?} is already on node {:?}",
            vm.name, target.name
        )));
    }
    // 이미 옮기는 중인 VM은 아래의 vm_busy로 거절한다.
    let source = vm.spec.node.filter(|_| {
        vm.spec.power == PowerState::Running
            && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating)
    });
    let Some(source) = source else {
        return Err(ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running", vm.name),
        ));
    };
    check_not_busy(&state, &vm)?;
    check_no_snapshot_task(&state, &vm)?;
    let inv = Inventory::load(&state.cluster)?;
    check_no_resize_task(&state, &inv, &vm)?;
    for disk in &vm.spec.disks {
        let pool = inv
            .volumes
            .iter()
            .find(|v| v.name == disk.volume)
            .and_then(|v| inv.pools.iter().find(|p| p.name == v.spec.pool));
        if !pool.is_some_and(is_shared) {
            return Err(ApiError::conflict(
                "migration_unsupported",
                format!(
                    "disk {:?} is not on a shared storage pool; only VMs whose disks are all on shared pools can be migrated",
                    disk.volume
                ),
            ));
        }
    }
    for node in [Some(&target), inv.nodes.iter().find(|n| n.id == source)]
        .into_iter()
        .flatten()
    {
        // HVF는 이 노드의 hypervisor 이름으로만 알 수 있다(macOS 노드).
        if node.status.hypervisor == "qemu-hvf" {
            return Err(ApiError::conflict(
                "migration_unsupported",
                format!(
                    "node {:?} runs macOS (HVF), which cannot migrate VMs",
                    node.name
                ),
            ));
        }
    }
    if !target.status.online || target.spec.maintenance {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is offline or in maintenance", target.name),
        ));
    }
    check_node_arch(&target, &vm.spec)?;
    node_fits(&vm.spec, &inv, &target).map_err(|e| ApiError::conflict("no_capacity", e))?;
    let free = inv.free_memory_mib(&target, vm.id);
    if free < i64::from(vm.spec.memory_mib) {
        return Err(ApiError::conflict(
            "no_capacity",
            format!(
                "node {:?} has {free} MiB of unassigned memory, but VM {:?} needs {} MiB",
                target.name, vm.name, vm.spec.memory_mib
            ),
        ));
    }
    let op = TaskOp::VmMigrate { node: target.id };
    let task = create_task(&state.cluster, op, vm.id, source).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM 디스크의 크기 늘리기 Task가 끝나지 않았으면 409 `vm_busy`. 실행 중인 VM의 디스크는 QMP로
/// 늘리므로 원본 QEMU가 사라지면 안 된다.
fn check_no_resize_task(state: &AppState, inv: &Inventory, vm: &Vm) -> Result<(), ApiError> {
    let disks: BTreeSet<Uuid> = inv
        .volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .map(|v| v.id)
        .collect();
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        disks.contains(&t.spec.target)
            && !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VolumeResize { .. })
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "a disk of VM {:?} is being resized; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// spec을 읽고 바꿔 revision을 확인하며 쓴다. reconciler의 status 쓰기와 부딪치면 다시 읽는다.
/// 읽은 VM이 마이그레이션 중이면 409 `vm_busy`다(호출자의 검사 뒤에 시작한 마이그레이션). 이 쓰기가
/// 먼저 적용되면 마이그레이션 Task가 바뀐 spec을 보고 그만둔다.
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
        check_not_migrating(&vm)?;
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

`crates/opl-api/tests/migrate.rs`를 만든다:

```rust
//! VM 라이브 마이그레이션 API: 요청 검사(설계 §4)와 마이그레이션 중 다른 동작 막기.
//! 실제 마이그레이션은 아직 없다. Task 실행기는 "not supported yet"으로 실패시킨다.

mod common;

use common::{Client, Env, mark_pool_ready, vm_request};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState};
use opl_types::vm::{Arch, MigrationStatus, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 실행기도 reconciler도 없는 두 번째 노드를 저장소에만 만든다(이 노드의 상태를 테스트가 바꾼다).
async fn add_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib: 8192,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
    };
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드의 상태를 바꾼다.
async fn edit_node(env: &Env, id: Uuid, f: impl FnOnce(&mut Node)) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    f(&mut node);
    env.cluster
        .update_spec(id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
    env.cluster
        .update_status::<NodeSpec>(id, &node.status)
        .await
        .unwrap();
}

/// 풀 `pool`을 만들고 이 노드에서 준비된 것으로 둔다.
async fn create_pool(env: &Env, admin: &Client, pool: &str, shared: bool) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": pool,
                "spec": {
                    "backend": { "kind": "dir", "path": env.dir.path().join(pool) },
                    "shared": shared
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready(pool).await;
}

/// 바로 Ready인 볼륨을 클러스터에 직접 만든다(파일은 만들지 않는다).
async fn ready_volume(env: &Env, pool: &str, name: &str) {
    let mut volume: Volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(env.node);
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// 이 노드에 고정된, 볼륨 `volume`을 디스크로 쓰는 VM을 만든다.
async fn create_vm(env: &Env, admin: &Client, name: &str, volume: &str) {
    let mut req = vm_request(name);
    req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// 공유 풀 `nas`의 디스크를 쓰는 VM `web`을 이 노드에서 실행하고, 그 풀을 쓸 수 있는 노드
/// `node2`를 더한다. `node2`의 id를 돌려준다.
async fn setup(env: &Env, admin: &Client) -> Uuid {
    let node2 = add_node(env, "node2").await;
    create_pool(env, admin, "nas", true).await;
    mark_pool_ready(&env.cluster, node2, "nas").await;
    ready_volume(env, "nas", "root").await;
    create_vm(env, admin, "web", "root").await;
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    node2
}

async fn migrate(client: &Client, vm: &str, node: &str) -> (StatusCode, Value) {
    client
        .post(&format!("/vms/{vm}/migrate"), json!({ "node": node }))
        .await
}

/// 409와 그 code를 확인한다.
fn assert_conflict((status, body): (StatusCode, Value), code: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], code, "{body}");
}

#[tokio::test]
async fn migrate_creates_a_task_on_the_source_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let (status, _) = migrate(&viewer, "web", "node2").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, task) = migrate(&operator, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-migrate", "node": node2 })
    );
    assert_eq!(task["spec"]["target"], vm["id"]);
    // 원본 노드(VM이 도는 노드)의 실행기가 한다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "VM migration is not supported yet");

    // 노드는 id로도 고른다. 이름이 없으면 404, 지금 노드면 400이다.
    let (status, task) = migrate(&admin, "web", &node2.to_string()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let (status, body) = migrate(&admin, "web", "nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, body) = migrate(&admin, "web", "node1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    let (status, _) = migrate(&admin, "nope", "node2").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn migrate_needs_a_running_vm_on_shared_pools() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;

    // 꺼진 VM은 옮길 것이 없다.
    create_pool(&env, &admin, "local", false).await;
    ready_volume(&env, "local", "data").await;
    create_vm(&env, &admin, "db", "data").await;
    assert_conflict(migrate(&admin, "db", "node2").await, "vm_not_running");

    // 공유가 아닌 풀의 디스크는 대상 노드에서 열 수 없다.
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (status, body) = migrate(&admin, "db", "node2").await;
    assert_conflict((status, body.clone()), "migration_unsupported");
    assert!(
        body["message"].as_str().unwrap().contains("\"data\""),
        "{body}"
    );
}

#[tokio::test]
async fn migrate_checks_the_nodes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;

    // HVF(macOS) 노드는 마이그레이션을 하지 못한다. 대상이든 원본이든.
    edit_node(&env, node2, |n| n.status.hypervisor = "qemu-hvf".into()).await;
    assert_conflict(
        migrate(&admin, "web", "node2").await,
        "migration_unsupported",
    );
    edit_node(&env, node2, |n| n.status.hypervisor = "qemu-kvm".into()).await;
    edit_node(&env, env.node, |n| n.status.hypervisor = "qemu-hvf".into()).await;
    assert_conflict(
        migrate(&admin, "web", "node2").await,
        "migration_unsupported",
    );
    edit_node(&env, env.node, |n| n.status.hypervisor = "mock".into()).await;

    edit_node(&env, node2, |n| n.status.online = false).await;
    assert_conflict(migrate(&admin, "web", "node2").await, "node_unavailable");
    edit_node(&env, node2, |n| {
        n.status.online = true;
        n.spec.maintenance = true;
    })
    .await;
    assert_conflict(migrate(&admin, "web", "node2").await, "node_unavailable");
    edit_node(&env, node2, |n| n.spec.maintenance = false).await;

    edit_node(&env, node2, |n| n.status.arch = Some(Arch::X86_64)).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "machine_arch_mismatch");
    edit_node(&env, node2, |n| n.status.arch = Some(Arch::Aarch64)).await;

    // 풀이 준비되지 않았거나 메모리가 모자라면 스케줄러가 고를 수 없는 노드다.
    edit_node(&env, node2, |n| {
        n.status.pools.get_mut("nas").unwrap().ready = false
    })
    .await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_conflict((status, body.clone()), "no_capacity");
    assert!(
        body["message"].as_str().unwrap().contains("storage pools"),
        "{body}"
    );
    mark_pool_ready(&env.cluster, node2, "nas").await;
    edit_node(&env, node2, |n| n.status.memory_mib = 128).await;
    let (status, body) = migrate(&admin, "web", "node2").await;
    assert_conflict((status, body.clone()), "no_capacity");
    assert!(
        body["message"].as_str().unwrap().contains("memory"),
        "{body}"
    );
    edit_node(&env, node2, |n| n.status.memory_mib = 8192).await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 끝나지 않은 마이그레이션 Task가 있으면 VM의 다른 동작이 모두 409 `vm_busy`다.
#[tokio::test]
async fn migrate_task_makes_the_vm_busy() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    // 실행기를 멈춰 Task가 끝나지 않게 한다.
    env.stop_runner().await;
    let (status, task) = migrate(&admin, "web", "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    assert_busy(&admin, "web", "root", "node2").await;
    // 마이그레이션 Task가 끝나면 풀린다.
    env.start_runner().await;
    admin.wait_task(&task).await;
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// `migrating`인 VM은 다른 동작이 모두 409 `vm_busy`다.
#[tokio::test]
async fn migrating_phase_makes_the_vm_busy() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = setup(&env, &admin).await;
    // 이 노드의 reconciler가 손대지 않도록 node2에서 도는 VM을 둔다.
    ready_volume(&env, "nas", "far-root").await;
    let mut req = vm_request("far");
    req["spec"]["disks"] = json!([{ "volume": "far-root", "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(node2);
    req["spec"]["power"] = json!("running");
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut vm = env.cluster.get_by_name::<VmSpec>("far").unwrap().unwrap();
    vm.status.phase = VmPhase::Migrating;
    vm.status.node = Some(node2);
    vm.status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &vm.status)
        .await
        .unwrap();

    assert_busy(&admin, "far", "far-root", "node1").await;
}

/// 켜기·끄기·강제 끄기·재시작·`PUT`·지우기·스냅샷·복제·디스크(`volume`) 크기 늘리기·`node`로
/// 마이그레이션이 모두 409 `vm_busy`다.
async fn assert_busy(admin: &Client, vm: &str, volume: &str, node: &str) {
    for action in ["start", "stop", "stop?mode=force", "reboot"] {
        let (status, body) = admin
            .post(&format!("/vms/{vm}/actions/{action}"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{action}: {body}");
        assert_eq!(body["code"], "vm_busy", "{action}: {body}");
    }
    let (_, current) = admin.get(&format!("/vms/{vm}")).await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(512);
    assert_conflict(
        admin
            .put(&format!("/vms/{vm}"), json!({ "spec": spec }), None)
            .await,
        "vm_busy",
    );
    assert_conflict(admin.delete(&format!("/vms/{vm}")).await, "vm_busy");
    assert_conflict(
        admin
            .post(&format!("/vms/{vm}/snapshots"), json!({ "name": "s1" }))
            .await,
        "vm_busy",
    );
    assert_conflict(
        admin
            .post(&format!("/vms/{vm}/clone"), json!({ "name": "copy" }))
            .await,
        "vm_busy",
    );
    assert_conflict(
        admin
            .post(
                &format!("/volumes/{volume}/resize"),
                json!({ "size_bytes": 2 << 20 }),
            )
            .await,
        "vm_busy",
    );
    assert_conflict(migrate(admin, vm, node).await, "vm_busy");
}

/// 끝나지 않은 스냅샷·크기 늘리기 Task가 있으면 마이그레이션을 시작하지 않는다.
#[tokio::test]
async fn migrate_waits_for_snapshot_and_resize_tasks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    env.stop_runner().await;

    let (status, resize) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resize}");
    assert_conflict(migrate(&admin, "web", "node2").await, "vm_busy");
    // 크기 늘리기 Task를 끝난 것으로 두고 스냅샷 Task를 본다.
    let id = resize["id"].as_str().unwrap().parse().unwrap();
    let mut task = env.cluster.get::<TaskSpec>(id).unwrap().unwrap();
    task.status.state = TaskState::Failed;
    env.cluster
        .update_status::<TaskSpec>(task.id, &task.status)
        .await
        .unwrap();

    let (status, snapshot) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{snapshot}");
    assert_conflict(migrate(&admin, "web", "node2").await, "vm_busy");
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/vms/{key}/clone",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/vms/{key}/clone",
        "/vms/{key}/migrate",
        "/watch",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    let schemes = &doc["components"]["securitySchemes"];
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
    let vm_status = &doc["components"]["schemas"]["VmStatus"];
    assert_eq!(
        vm_status["properties"]["migration"],
        serde_json::json!({
            "$ref": "#/components/schemas/MigrationStatus",
            "description": "진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다."
        })
    );
    assert!(
        !vm_status["required"]
            .as_array()
            .is_some_and(|r| r.contains(&"migration".into())),
        "{vm_status}"
    );
    let schemes = &doc["components"]["securitySchemes"];
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            schedule_error: vm.status.schedule_error.clone(),
        };
```

을 다음으로 바꾼다:

```rust
            schedule_error: vm.status.schedule_error.clone(),
            migration: None,
        };
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
            schedule_error: None,
        }
```

을 다음으로 바꾼다:

```rust
            schedule_error: None,
            migration: None,
        }
```

`crates/opl-types/src/task.rs`에서

```rust
        pool: Option<String>,
    },
```

을 다음으로 바꾼다:

```rust
        pool: Option<String>,
    },
    /// 실행 중인 대상 VM을 `node`로 라이브 마이그레이션한다. 원본 노드(VM의 `spec.node`)에서 돈다.
    VmMigrate {
        node: Uuid,
    },
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn migrate_op_is_tagged_by_kind() {
        let node = Uuid::now_v7();
        let op = TaskOp::VmMigrate { node };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "vm-migrate", "node": node })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/opl-types/src/vm.rs`에서

```rust
    pub schedule_error: Option<ScheduleError>,
}
```

을 다음으로 바꾼다:

```rust
    pub schedule_error: Option<ScheduleError>,
    /// 진행 중인 라이브 마이그레이션. 원본 노드의 마이그레이션 Task가 쓰고 지운다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    // 없으면 빼고 쓰므로 API 문서에서도 null·기본값이 아니라 선택 필드다.
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub migration: Option<MigrationStatus>,
}

/// 진행 중인 라이브 마이그레이션: 대상 노드와 그 마이그레이션 Task
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MigrationStatus {
    pub node: Uuid,
    pub task: Uuid,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
    Failed,
}
```

을 다음으로 바꾼다:

```rust
    Failed,
    /// 다른 노드로 라이브 마이그레이션 중이다(원본 QEMU가 아직 돈다).
    Migrating,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
        assert_eq!(st.schedule_error, None);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(st.schedule_error, None);
        assert_eq!(st.migration, None);
    }

    #[test]
    fn migration_status_is_written_only_while_migrating() {
        // 마이그레이션이 없으면 필드를 빼고 쓴다.
        let json = serde_json::to_value(VmStatus::default()).unwrap();
        assert!(json.get("migration").is_none(), "{json}");
        let st = VmStatus {
            phase: VmPhase::Migrating,
            migration: Some(MigrationStatus {
                node: Uuid::now_v7(),
                task: Uuid::now_v7(),
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["phase"], "migrating");
        assert_eq!(
            json["migration"],
            serde_json::json!({
                "node": st.migration.unwrap().node,
                "task": st.migration.unwrap().task
            })
        );
        assert_eq!(serde_json::from_value::<VmStatus>(json).unwrap(), st);
    }
```

`crates/opld/src/config.rs`에서

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
```

을 다음으로 바꾼다:

```rust
use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
```

`crates/opld/src/config.rs`에서

```rust
    pub pools: PoolsSection,
}
```

을 다음으로 바꾼다:

```rust
    pub pools: PoolsSection,
    #[serde(default)]
    pub migration: MigrationSection,
}
```

`crates/opld/src/config.rs`에서

```rust
        Duration::from_secs(self.refresh_secs)
    }
}
```

을 다음으로 바꾼다:

```rust
        Duration::from_secs(self.refresh_secs)
    }
}

/// 라이브 마이그레이션(설계 §4)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSection {
    /// 받는 쪽 QEMU가 마이그레이션을 받는 포트 범위(`"A-B"`, 둘 다 포함). 노드 사이 방화벽에서
    /// 열어야 한다.
    #[serde(
        default = "default_migration_ports",
        deserialize_with = "deserialize_port_range"
    )]
    pub ports: RangeInclusive<u16>,
    /// 보내는 대역폭 상한(MiB/s). 0이면 QEMU 기본값이다.
    #[serde(default)]
    pub bandwidth_mib: u64,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다(`"30m"`, `"1h30m"`, `"90s"`).
    #[serde(
        default = "default_migration_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub timeout: Duration,
}

impl Default for MigrationSection {
    fn default() -> Self {
        Self {
            ports: default_migration_ports(),
            bandwidth_mib: 0,
            timeout: default_migration_timeout(),
        }
    }
}

fn default_migration_ports() -> RangeInclusive<u16> {
    60000..=60099
}
fn default_migration_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}

/// `"A-B"`: 0이 아닌 포트 A부터 B까지(A ≤ B)
fn parse_port_range(s: &str) -> Result<RangeInclusive<u16>, String> {
    let err =
        || format!("invalid port range {s:?} (expected \"FIRST-LAST\", e.g. \"60000-60099\")");
    let (a, b) = s.split_once('-').ok_or_else(err)?;
    let a: u16 = a.trim().parse().map_err(|_| err())?;
    let b: u16 = b.trim().parse().map_err(|_| err())?;
    if a == 0 || a > b {
        return Err(err());
    }
    Ok(a..=b)
}

/// 숫자와 단위(`h`, `m`, `s`)를 이어 쓴 시간: `"30m"`, `"1h30m"`. 0보다 커야 한다.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let err = || format!("invalid duration {s:?} (e.g. \"30m\", \"1h30m\", \"90s\")");
    let mut rest = s.trim();
    let mut secs: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
        let n: u64 = rest[..digits].parse().map_err(|_| err())?;
        let unit = match rest.as_bytes()[digits] {
            b'h' => 3600,
            b'm' => 60,
            b's' => 1,
            _ => return Err(err()),
        };
        secs = n
            .checked_mul(unit)
            .and_then(|v| secs.checked_add(v))
            .ok_or_else(err)?;
        rest = &rest[digits + 1..];
    }
    if secs == 0 {
        return Err(err());
    }
    Ok(Duration::from_secs(secs))
}

fn deserialize_port_range<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<RangeInclusive<u16>, D::Error> {
    parse_port_range(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    parse_duration(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}
```

`crates/opld/src/config.rs`에서

```rust
        assert_eq!(c.pools.refresh(), Duration::from_secs(15));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
```

을 다음으로 바꾼다:

```rust
        assert_eq!(c.pools.refresh(), Duration::from_secs(15));
        assert_eq!(c.migration.ports, 60000..=60099);
        assert_eq!(c.migration.bandwidth_mib, 0);
        assert_eq!(c.migration.timeout, Duration::from_secs(30 * 60));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
```

`crates/opld/src/config.rs`에서

```rust
            }
        }
    }
}
```

을 다음으로 바꾼다:

```rust
            }
        }
    }

    #[test]
    fn migration_section_parses_ports_and_timeout() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"61000-61009\"\n\
             bandwidth_mib = 100\ntimeout = \"1h30m\"",
        );
        assert_eq!(c.migration.ports, 61000..=61009);
        assert_eq!(c.migration.bandwidth_mib, 100);
        assert_eq!(c.migration.timeout, Duration::from_secs(5400));
        let one = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"60000-60000\"\ntimeout = \"90s\"",
        );
        assert_eq!(one.migration.ports, 60000..=60000);
        assert_eq!(one.migration.timeout, Duration::from_secs(90));
        for bad in [
            "ports = \"60099-60000\"",
            "ports = \"0-10\"",
            "ports = \"60000\"",
            "ports = \"60000-70000\"",
            "ports = \"a-b\"",
            "timeout = \"0s\"",
            "timeout = \"30\"",
            "timeout = \"500ms\"",
            "timeout = \"m\"",
            "timeout = \"\"",
            "bandwidth = 1",
        ] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[migration]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }
}
```

`web/src/i18n/en.json`에서

```json
      "volume-resize": "Resize volume {{target}}",
      "vm-clone": "Clone {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "volume-resize": "Resize volume {{target}}",
      "vm-clone": "Clone {{target}}",
      "vm-migrate": "Migrate {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "stopping": "Stopping",
      "failed": "Failed"
    },
```

을 다음으로 바꾼다:

```json
      "stopping": "Stopping",
      "failed": "Failed",
      "migrating": "Migrating"
    },
```

`web/src/i18n/ko.json`에서

```json
      "volume-resize": "볼륨 {{target}} 크기 늘리기",
      "vm-clone": "{{target}} 복제"
    }
```

을 다음으로 바꾼다:

```json
      "volume-resize": "볼륨 {{target}} 크기 늘리기",
      "vm-clone": "{{target}} 복제",
      "vm-migrate": "{{target}} 마이그레이션"
    }
```

`web/src/i18n/ko.json`에서

```json
      "stopping": "정지 중",
      "failed": "실패"
    },
```

을 다음으로 바꾼다:

```json
      "stopping": "정지 중",
      "failed": "실패",
      "migrating": "마이그레이션 중"
    },
```

`web/src/tasks/tasks.test.tsx`에서

```tsx
it("describes unknown operations by their kind", () => {
  const unknown = { ...task("4", "pending"), spec: { op: { kind: "vm-migrate" }, target: "x" } };
  expect(describeTask(unknown as unknown as Task, i18n.t, "web")).toBe("vm-migrate web");
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
  const clone = { ...task("4", "pending"), spec: { op: { kind: "vm-clone" }, target: "x" } };
  expect(describeTask(clone as unknown as Task, i18n.t, "web")).toBe("Clone web");
});
```

을 다음으로 바꾼다:

```tsx
it("describes unknown operations by their kind", () => {
  const unknown = { ...task("4", "pending"), spec: { op: { kind: "vm-teleport" }, target: "x" } };
  expect(describeTask(unknown as unknown as Task, i18n.t, "web")).toBe("vm-teleport web");
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
  const clone = { ...task("4", "pending"), spec: { op: { kind: "vm-clone" }, target: "x" } };
  expect(describeTask(clone as unknown as Task, i18n.t, "web")).toBe("Clone web");
  const migrate = { ...task("4", "pending"), spec: { op: { kind: "vm-migrate" }, target: "x" } };
  expect(describeTask(migrate as unknown as Task, i18n.t, "web")).toBe("Migrate web");
});
```

`web/src/vms/VmPhaseBadge.tsx`에서

```tsx
  failed: "destructive",
} as const;
```

을 다음으로 바꾼다:

```tsx
  failed: "destructive",
  migrating: "secondary",
} as const;
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 663개, 실패 0

Run: `cargo test -p opl-api --test migrate`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 297개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 라이브 마이그레이션 타입과 POST /vms/{key}/migrate 검사"
```

---

### Task 2: QEMU 라이브 마이그레이션 드라이버

opl-qemu: `build_incoming_cmdline`(`-incoming defer`), `RunState::{InMigrate, PostMigrate}`, `MigrationState`·`MigrationInfo`·`TlsEndpoint`·`MigrationParameters`, QMP `query_version`·`add_tls_creds`·`object_del`·`migrate_set_capabilities`·`migrate_set_parameters`·`migrate`·`migrate_incoming`·`migrate_cancel`·`query_migrate`·`wait_migration`. opl-driver `migration.rs`: `MigrateTarget`·`MigrateOptions`, TLS 디렉터리 `migration_tls_dir`·`write_migration_tls`(디렉터리 0700, 키 0600), `tcp_uri`. `Hypervisor`에 `qemu_version`·`running_qemu_version`·`start_incoming`(범위에서 미리 bind 해 본 빈 포트, 받는 중 표시)·`incoming_wait`·`finish_incoming`·`list_incoming`·`migrate`(능력 `events`·`auto-converge`, TLS 객체는 매번 다시 만들고 양쪽 `verify-peer`, 시간 제한이 지나면 취소)·`stop_migrated`(`postmigrate`일 때만). `MockHypervisor`도. 노드 인증서에 keyEncipherment를 더한다(QEMU 8.2). 실제 시험 `qemu_migration.rs`(rcgen 인증서, macOS는 TCG).

**Files:**
- Create: `crates/opl-driver/src/migration.rs`
- Create: `crates/opl-driver/tests/qemu_migration.rs`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/hypervisor.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-peer/src/pki.rs`
- Modify: `crates/opl-qemu/src/cmdline.rs`
- Modify: `crates/opl-qemu/src/lib.rs`
- Modify: `crates/opl-qemu/src/qmp.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/Cargo.toml`에서

```toml
[dev-dependencies]
tempfile.workspace = true
```

을 다음으로 바꾼다:

```toml
[dev-dependencies]
rcgen.workspace = true
tempfile.workspace = true
```

`crates/opl-driver/src/error.rs`에서

```rust
    NotAttached(Uuid),
    #[error(transparent)]
```

을 다음으로 바꾼다:

```rust
    NotAttached(Uuid),
    /// 라이브 마이그레이션이 실패했거나 취소됐다. 내용은 QEMU의 `error-desc` 등이다.
    #[error("migration failed: {0}")]
    Migration(String),
    #[error(transparent)]
```

`crates/opl-driver/src/error.rs`에서

```rust
            | DriverError::Zfs(_)
            | DriverError::Rbd(_) => false,
        }
```

을 다음으로 바꾼다:

```rust
            | DriverError::Zfs(_)
            | DriverError::Rbd(_)
            | DriverError::Migration(_) => false,
        }
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(!DriverError::Rbd("rbd: error opening pool 'x'".into()).is_transient());
    }
```

을 다음으로 바꾼다:

```rust
        assert!(!DriverError::Rbd("rbd: error opening pool 'x'".into()).is_transient());
        // 마이그레이션 실패는 Task가 되돌린다. 같은 QEMU로 다시 받을 수 없다.
        assert!(!DriverError::Migration("TLS handshake failed".into()).is_transient());
    }
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust
use std::path::PathBuf;
```

을 다음으로 바꾼다:

```rust
use std::ops::RangeInclusive;
use std::path::PathBuf;
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust
use crate::error::DriverError;
```

을 다음으로 바꾼다:

```rust
use crate::error::DriverError;
use crate::migration::{MigrateOptions, MigrateTarget};
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

을 다음으로 바꾼다:

```rust

    /// 이 노드가 새로 띄울 QEMU의 버전(`major.minor.micro`). 받는 쪽이 원본과 같은지 본다.
    async fn qemu_version(&self) -> Result<String, DriverError>;

    /// 실행 중인 VM의 QEMU 버전(`major.minor.micro`, QMP `query-version`). 바이너리를 바꾼 뒤에도
    /// 이미 떠 있는 QEMU의 버전이다.
    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError>;

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시를 남기고, TLS
    /// 자격 증명(받는 쪽, 상대 인증서 확인)과 능력을 정한 뒤 `ports`에서 빈 포트로 `listen_host`에서
    /// 받기 시작한다. 연 포트를 돌려준다. 실패하면 띄운 QEMU를 끈다. 이미 이 노드에서 돌고 있는
    /// VM이면 실패한다. 받은 뒤에도 멈춰 있다([`finish_incoming`](Self::finish_incoming)이 이어서 실행).
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError>;

    /// 받는 중인 VM의 마이그레이션이 끝나기를 기다린다. 완료면 성공, 실패했거나 받는 QEMU가
    /// 사라졌으면 `Migration` 오류다. 받는 중 표시가 없는 VM이면 실패한다.
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError>;

    /// 받기를 마친 VM을 넘겨받는다: 멈춰 있으면 이어서 실행하고 받는 중 표시를 지운다. 표시가
    /// 없으면(이미 넘겨받음) 아무것도 하지 않는다. 아직 받는 중이면 `Migration` 오류다. 마이그레이션이
    /// 확정된 뒤(VM이 이 노드로 옮겨진 뒤)에만 부른다.
    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError>;

    /// 받는 중 표시가 있는 살아 있는 VM. 확정되지 않은 채 남은 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn list_incoming(&self) -> Result<Vec<Uuid>, DriverError>;

    /// 실행 중인 VM을 `target`으로 보낸다(TLS, 능력 `events`·`auto-converge`). 끝날 때까지 기다린다.
    /// 성공하면 원본은 `postmigrate`로 멈춰 있다([`stop_migrated`](Self::stop_migrated)로 끈다).
    /// 실패하거나 `opts.timeout`이 지나 취소하면 `Migration` 오류이고 원본은 대개 계속 돈다
    /// (멈췄으면 [`resume`](Self::resume)으로 이어서 실행한다).
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError>;

    /// 마이그레이션을 보내고 `postmigrate`로 멈춘 원본 QEMU를 끄고 true. 다른 상태이거나 실행 중이
    /// 아니면 아무것도 하지 않고 false.
    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub mod hypervisor;
pub mod mock;
```

을 다음으로 바꾼다:

```rust
pub mod hypervisor;
pub mod migration;
pub mod mock;
```

`crates/opl-driver/src/lib.rs`에서

```rust
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
```

을 다음으로 바꾼다:

```rust
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
pub use migration::{
    MIGRATION_TLS_ID, MigrateOptions, MigrateTarget, migration_tls_dir, tcp_uri,
    write_migration_tls,
};
```

`crates/opl-driver/src/migration.rs`를 만든다:

```rust
//! 라이브 마이그레이션의 하이퍼바이저 밖 준비물: QEMU TLS 자격 증명 파일, 주소, 포트.

use std::io::Write;
use std::ops::RangeInclusive;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 마이그레이션을 보낼 곳
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateTarget {
    /// 받는 노드의 주소(호스트 이름이나 IP)
    pub host: String,
    /// 받는 QEMU가 연 포트(`start_incoming`의 결과)
    pub port: u16,
    /// 받는 노드 인증서에서 확인할 이름(인증서의 SAN에 있어야 한다)
    pub tls_hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateOptions {
    /// 최대 대역폭(바이트/초). `None`이면 QEMU 기본값
    pub bandwidth: Option<u64>,
    /// 이 시간 안에 끝나지 않으면 취소한다(원본은 계속 돈다).
    pub timeout: Duration,
}

/// QEMU의 TLS 자격 증명 객체 id. 보내는 쪽과 받는 쪽 모두 이 이름으로 만든다.
pub const MIGRATION_TLS_ID: &str = "opl-mig-tls";

const CA_FILE: &str = "ca-cert.pem";
/// QEMU `tls-creds-x509`가 읽는 이름. 노드 인증서 하나를 받는 쪽(server)과 보내는 쪽(client)에 같이 쓴다.
const CERT_FILES: [&str; 2] = ["server-cert.pem", "client-cert.pem"];
const KEY_FILES: [&str; 2] = ["server-key.pem", "client-key.pem"];

/// QEMU에 줄 TLS 자격 증명 디렉터리 `{state_dir}/migration-tls`
pub fn migration_tls_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("migration-tls")
}

/// 클러스터 CA, 노드 인증서와 키를 QEMU `tls-creds-x509`의 파일 이름으로 `dir`에 쓴다. 디렉터리는
/// 주인만(0700), 키는 주인만 읽을 수 있다(0600). 이미 있으면 바꾼다(인증서 갱신). 파일마다 임시
/// 파일에 쓴 뒤 이름을 바꾸므로 QEMU가 읽는 도중에 반쪽 파일을 보지 않는다.
pub fn write_migration_tls(
    dir: &Path,
    ca_pem: &str,
    cert_pem: &str,
    key_pem: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let files = [(CA_FILE, ca_pem, 0o644)]
        .into_iter()
        .chain(CERT_FILES.map(|f| (f, cert_pem, 0o644)))
        .chain(KEY_FILES.map(|f| (f, key_pem, 0o600)));
    for (name, content, mode) in files {
        let tmp = dir.join(format!(".{name}.tmp"));
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, dir.join(name))?;
    }
    Ok(())
}

/// QEMU 마이그레이션 URI `tcp:host:port`. IPv6 주소는 괄호로 감싼다.
pub fn tcp_uri(host: &str, port: u16) -> String {
    format!("tcp:{}", socket_addr(host, port))
}

fn socket_addr(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// `ports`에서 `host`에 지금 열 수 있는 첫 포트. 잠깐 열어 보고 닫는다. QEMU의 `migrate-incoming`은
/// 한 번만 부를 수 있어 포트가 쓰는 중이면 그 QEMU를 버려야 하므로 미리 고른다.
pub(crate) fn pick_port(host: &str, ports: RangeInclusive<u16>) -> Option<u16> {
    ports
        .into_iter()
        .find(|&port| std::net::TcpListener::bind(socket_addr(host, port)).is_ok())
}

/// `qemu-system-* --version`의 첫 줄(`QEMU emulator version 8.2.2 (Debian ...)`)에서 버전을 읽는다.
/// QMP `query-version`의 `major.minor.micro`와 같은 모양이다.
pub(crate) fn parse_qemu_version(output: &str) -> Option<String> {
    let rest = output.lines().next()?.split("version ").nth(1)?;
    let version = rest.split_whitespace().next()?;
    version
        .split('.')
        .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn writes_tls_files_with_private_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = migration_tls_dir(tmp.path());
        assert_eq!(dir, tmp.path().join("migration-tls"));
        write_migration_tls(&dir, "CA", "CERT", "KEY").unwrap();

        let mode = |name: &str| std::fs::metadata(dir.join(name)).unwrap().mode() & 0o777;
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("ca-cert.pem"), "CA");
        for name in ["server-cert.pem", "client-cert.pem"] {
            assert_eq!(read(name), "CERT");
            assert_eq!(mode(name), 0o644);
        }
        for name in ["server-key.pem", "client-key.pem"] {
            assert_eq!(read(name), "KEY");
            assert_eq!(mode(name), 0o600, "{name}");
        }

        // 인증서가 바뀌면 다시 쓴다. 남은 임시 파일도 넘어간다.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join(".server-key.pem.tmp"), "half").unwrap();
        write_migration_tls(&dir, "CA2", "CERT2", "KEY2").unwrap();
        assert_eq!(read("server-key.pem"), "KEY2");
        assert_eq!(read("client-cert.pem"), "CERT2");
        assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "ca-cert.pem",
                "client-cert.pem",
                "client-key.pem",
                "server-cert.pem",
                "server-key.pem"
            ]
        );
    }

    #[test]
    fn uri_brackets_ipv6() {
        assert_eq!(tcp_uri("10.0.0.2", 60000), "tcp:10.0.0.2:60000");
        assert_eq!(tcp_uri("node-b.lan", 60001), "tcp:node-b.lan:60001");
        assert_eq!(tcp_uri("fd00::2", 60002), "tcp:[fd00::2]:60002");
        assert_eq!(tcp_uri("[fd00::2]", 60002), "tcp:[fd00::2]:60002");
    }

    #[test]
    fn picks_first_port_that_is_free() {
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        assert_eq!(pick_port("127.0.0.1", port..=port), None);
        drop(busy);
        assert_eq!(pick_port("127.0.0.1", port..=port), Some(port));
    }

    #[test]
    fn parses_qemu_version_output() {
        assert_eq!(
            parse_qemu_version(
                "QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.4)\nCopyright (c) 2003-2023 Fabrice Bellard\n"
            )
            .as_deref(),
            Some("8.2.2")
        );
        assert_eq!(
            parse_qemu_version("QEMU emulator version 11.1.2\nCopyright\n").as_deref(),
            Some("11.1.2")
        );
        assert_eq!(parse_qemu_version("qemu: unknown option"), None);
        assert_eq!(parse_qemu_version("version x.y"), None);
        assert_eq!(parse_qemu_version(""), None);
    }
}
```

`crates/opl-driver/src/mock.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::migration::{MigrateOptions, MigrateTarget};

/// 따로 정하지 않았을 때 `qemu_version`이 돌려주는 버전
pub const MOCK_QEMU_VERSION: &str = "8.2.2";

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
    /// `pause`로 멈추고 아직 `resume`하지 않은 VM
    paused: HashSet<Uuid>,
    /// 멈춘 채 남은 표시(`paused-by-opl`)가 있는 VM(이전 opld가 멈춘 채 죽었다)
    marked: HashSet<Uuid>,
    /// `pause`·`resume` 호출 (VM, 이름)
    pauses: Vec<(Uuid, &'static str)>,
    /// 조회에 드러낼 상태(QEMU가 스스로 멈춘 상태 등)
    statuses: HashMap<Uuid, RuntimeStatus>,
    /// `set_qemu_version`으로 정한 QEMU 버전
    qemu_version: Option<String>,
    /// 받는 중 표시가 있는 VM(`start_incoming`으로 띄우고 아직 `finish_incoming`하지 않음)
    incoming: HashSet<Uuid>,
    /// 마이그레이션이 만든 상태: 받는 쪽 `inmigrate`·받은 뒤 멈춤, 보낸 원본 `postmigrate`.
    /// VM이 꺼지면 사라진다.
    migration_status: HashMap<Uuid, RuntimeStatus>,
    /// 다음 `migrate` 한 번을 실패시킬 오류
    fail_next_migrate: Option<DriverError>,
    /// 다음 `incoming_wait` 한 번을 실패시킬 오류. 실제 QEMU처럼 받는 QEMU는 끝난다.
    fail_next_incoming: Option<DriverError>,
    /// `migrate` 호출 (VM, 보낸 곳). 실패한 호출 포함
    migrations: Vec<(Uuid, MigrateTarget)>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 다음 `snapshot_save` 한 번이 VM이 꺼질 때까지 끝나지 않게 한다. 실제 QEMU의 메모리 저장처럼
    /// 그동안 VM을 강제로 끄면 QMP가 끊긴 오류로 실패하고 태그를 남기지 않는다.
    pub fn hang_next_snapshot_save(&self) {
        self.state.lock().unwrap().hang_next_save = true;
    }

    /// 멈춘 `snapshot_save`가 들어올 때까지 기다린다.
    pub async fn snapshot_save_reached(&self) {
        self.save_reached.notified().await;
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
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

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// `pause`로 멈추고 아직 `resume`하지 않았는지
    pub fn is_paused(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().paused.contains(&id)
    }

    /// 이전 opld가 `pause`한 채 죽은 것처럼 VM을 표시와 함께 멈춘다. `resume_left_stopped`가
    /// 이어서 실행한다(지금 `pause`해 둔 VM은 표시가 없는 것으로 본다).
    pub fn leave_paused(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        st.paused.insert(id);
        st.marked.insert(id);
    }

    /// VM에 부른 `pause`·`resume`(실패한 호출 포함)을 순서대로 돌려준다.
    pub fn pause_calls(&self, id: Uuid) -> Vec<&'static str> {
        let st = self.state.lock().unwrap();
        st.pauses
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, call)| *call)
            .collect()
    }

    /// 조회(`inspect`·`list`)가 VM의 상태를 `status`로 알리게 한다(`None`이면 되돌린다). QEMU가 스스로
    /// 멈춘 상태(`io-error` 등)를 흉내 낸다. QMP로 다룰 수 없는 VM은 그대로 `unresponsive`다.
    pub fn set_status(&self, id: Uuid, status: Option<RuntimeStatus>) {
        let mut st = self.state.lock().unwrap();
        match status {
            Some(status) => st.statuses.insert(id, status),
            None => st.statuses.remove(&id),
        };
    }

    /// `qemu_version`·`running_qemu_version`이 돌려줄 버전(기본 [`MOCK_QEMU_VERSION`])
    pub fn set_qemu_version(&self, version: &str) {
        self.state.lock().unwrap().qemu_version = Some(version.to_string());
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. 원본은 그대로 돈다.
    pub fn fail_next_migrate(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate = Some(err);
    }

    /// 다음 `incoming_wait` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 크래시로 끝난다.
    pub fn fail_next_incoming_wait(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incoming = Some(err);
    }

    /// `migrate` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn migrations(&self) -> Vec<(Uuid, MigrateTarget)> {
        self.state.lock().unwrap().migrations.clone()
    }

    /// 받는 중 표시가 있는지
    pub fn is_incoming(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().incoming.contains(&id)
    }

    fn version(st: &MockState) -> String {
        st.qemu_version
            .clone()
            .unwrap_or_else(|| MOCK_QEMU_VERSION.to_string())
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        st.paused.remove(&id);
        st.marked.remove(&id);
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else if let Some(status) = st.statuses.get(&id) {
            status.clone()
        } else if let Some(status) = st.migration_status.get(&id) {
            status.clone()
        } else if st.paused.contains(&id) {
            RuntimeStatus::Paused
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().start_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
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
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.paused.remove(&id);
                        st.incoming.remove(&id);
                        st.migration_status.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let hang = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            std::mem::take(&mut st.hang_next_save)
        };
        if hang {
            self.save_reached.notify_one();
            while self.state.lock().unwrap().vms.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return Err(DriverError::Qmp(QmpError::Disconnected));
        }
        let mut st = self.state.lock().unwrap();
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "pause"));
        Self::controllable(&st, id)?;
        st.paused.insert(id);
        Ok(())
    }

    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "resume"));
        Self::controllable(&st, id)?;
        st.paused.remove(&id);
        st.marked.remove(&id);
        // 마이그레이션을 보낸 원본도 `cont`로 다시 돈다.
        if st.migration_status.get(&id) == Some(&postmigrate()) {
            st.migration_status.remove(&id);
        }
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        if Self::controllable(&st, id).is_err() {
            return Ok(false);
        }
        if st.marked.remove(&id) {
            st.paused.remove(&id);
        }
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        Ok(Self::version(&self.state.lock().unwrap()))
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(Self::version(&st))
    }

    /// 범위의 첫 포트를 돌려준다. `fail_next_start`로 주입한 오류로 실패할 수 있다.
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        _listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&vm.id) {
                return Err(DriverError::Migration(format!(
                    "VM {} is already running on this node",
                    vm.id
                )));
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            st.incoming.insert(vm.id);
            st.migration_status.insert(vm.id, inmigrate());
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(*ports.start())
    }

    /// 주입한 실패가 없으면 받기를 마친 것으로 본다(멈춘 채).
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains(&id) {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        if let Some(err) = st.fail_next_incoming.take() {
            drop(st);
            self.remove(id, EventKind::Crashed);
            return Err(err);
        }
        st.migration_status.insert(id, RuntimeStatus::Paused);
        Ok(())
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains(&id) {
            return Ok(());
        }
        if st.migration_status.get(&id) == Some(&inmigrate()) {
            return Err(DriverError::Migration(format!(
                "incoming migration of VM {id} has not completed"
            )));
        }
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<Uuid>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .incoming
            .iter()
            .filter(|id| st.vms.contains_key(id))
            .copied()
            .collect())
    }

    /// 주입한 실패가 없으면 원본을 `postmigrate`로 멈춘다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        _opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.migrations.push((id, target.clone()));
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_migrate.take() {
            return Err(err);
        }
        st.migration_status.insert(id, postmigrate());
        Ok(())
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let migrated = {
            let st = self.state.lock().unwrap();
            st.vms.contains_key(&id) && st.migration_status.get(&id) == Some(&postmigrate())
        };
        if migrated {
            self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            );
        }
        Ok(migrated)
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

fn inmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("inmigrate".into())
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

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
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
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
        hv.start(&vm, None).await.unwrap();
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
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
            hv.pause(vm.id).await,
            hv.resume(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);
        hv.pause(vm.id).await.unwrap();
        assert!(hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Paused);
        hv.resume(vm.id).await.unwrap();
        assert!(!hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60000,
            tls_hostname: "10.0.0.2".into(),
        }
    }

    fn opts() -> MigrateOptions {
        MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(60),
        }
    }

    /// 원본 노드와 대상 노드의 하이퍼바이저로 한 번의 마이그레이션을 흉내 낸다.
    #[tokio::test]
    async fn migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        assert_eq!(src.qemu_version().await.unwrap(), MOCK_QEMU_VERSION);
        assert!(matches!(
            src.running_qemu_version(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        src.start(&vm, None).await.unwrap();
        assert_eq!(
            src.running_qemu_version(vm.id).await.unwrap(),
            dst.qemu_version().await.unwrap()
        );

        let mut events = dst.subscribe();
        let port = dst
            .start_incoming(&vm, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        assert_eq!(port, 60000);
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Started);
        assert!(dst.is_incoming(vm.id));
        assert_eq!(dst.list_incoming().await.unwrap(), vec![vm.id]);
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            st.vms
                .get(&vm.id)
                .map(|&pid| MockHypervisor::state_of(&st, vm.id, pid).status)
        };
        assert_eq!(status(&dst), Some(inmigrate()));
        // 아직 받는 중이면 넘겨받을 수 없다.
        assert!(matches!(
            dst.finish_incoming(vm.id).await,
            Err(DriverError::Migration(_))
        ));
        // 이미 돌고 있는 VM은 받을 수 없다.
        assert!(matches!(
            src.start_incoming(&vm, "10.0.0.1", 60000..=60099).await,
            Err(DriverError::Migration(_))
        ));

        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(src.migrations(), vec![(vm.id, target())]);
        assert_eq!(status(&src), Some(postmigrate()));
        dst.incoming_wait(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Paused));
        dst.finish_incoming(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Running));
        assert!(!dst.is_incoming(vm.id));
        assert!(dst.list_incoming().await.unwrap().is_empty());
        // 두 번째 넘겨받기는 아무것도 하지 않는다.
        dst.finish_incoming(vm.id).await.unwrap();
        // 받는 중 표시가 없으면 기다릴 것이 없다.
        assert!(matches!(
            dst.incoming_wait(vm.id).await,
            Err(DriverError::Migration(_))
        ));

        // 돌고 있는 VM은 끄지 않는다.
        assert!(!dst.stop_migrated(vm.id).await.unwrap());
        assert!(src.stop_migrated(vm.id).await.unwrap());
        assert_eq!(src.inspect(vm.id).await.unwrap(), None);
        assert!(!src.stop_migrated(vm.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_migration_keeps_source_and_receiver_exits() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        src.start(&vm, None).await.unwrap();
        src.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(matches!(
            src.migrate(vm.id, &target(), &opts()).await,
            Err(DriverError::Migration(_))
        ));
        let state = src.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 받기에 실패한 QEMU는 크래시로 끝난다.
        dst.start_incoming(&vm, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        let mut events = dst.subscribe();
        dst.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
        assert!(dst.incoming_wait(vm.id).await.is_err());
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Crashed);
        assert!(dst.list_incoming().await.unwrap().is_empty());

        // 시작 실패를 주입하면 받기도 실패한다.
        dst.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            dst.start_incoming(&vm, "10.0.0.2", 60000..=60099).await,
            Err(DriverError::StartFailed(_))
        ));
        assert_eq!(dst.inspect(vm.id).await.unwrap(), None);

        // 보낸 뒤 확정 전에 실패하면 원본을 이어서 실행한다.
        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            postmigrate()
        );
        src.resume(vm.id).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!src.stop_migrated(vm.id).await.unwrap());

        // 받는 쪽에서 버전을 다르게 보이게 할 수 있다.
        dst.set_qemu_version("9.0.0");
        assert_eq!(dst.qemu_version().await.unwrap(), "9.0.0");
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
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

`crates/opl-driver/src/qemu.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{
    HostProfile, MigrationInfo, MigrationParameters, MigrationState, QmpClient, QmpError, QmpEvent,
    RunState, RuntimePaths, TlsEndpoint, build_cmdline, build_incoming_cmdline,
};
use opl_types::network::tap_prefix;
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
use crate::migration::{
    MIGRATION_TLS_ID, MigrateOptions, MigrateTarget, migration_tls_dir, parse_qemu_version,
    pick_port, tcp_uri,
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
/// 내부 스냅샷(저장·불러오기·삭제) 하나를 기다리는 한도. 메모리 스냅샷은 메모리 크기만큼 걸린다.
/// Task 한도(600초)보다 짧아야 Task가 먼저 끊기지 않고 이 오류(작업이 남아 있을 수 있음)를 받는다.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

/// opl이 스냅샷을 위해 VM을 멈춰 두었다는 표시 파일. VM 실행 디렉터리에 `pause`가 쓰고 `resume`이
/// 지운다. opld가 멈춘 채 죽었으면 다시 붙을 때 이 표시를 보고 이어서 실행한다.
const PAUSED_MARK: &str = "paused-by-opl";

/// 라이브 마이그레이션을 받는 QEMU라는 표시 파일. `start_incoming`이 QEMU를 띄우기 전에 실행
/// 디렉터리에 쓰고 `finish_incoming`이 지운다. QEMU가 끝나면 실행 디렉터리와 함께 사라진다.
const INCOMING_MARK: &str = "incoming";
/// 마이그레이션 상태를 다시 묻는 간격(`MIGRATION` 이벤트를 놓쳐도 이만큼 뒤에 안다)
const MIGRATION_POLL: Duration = Duration::from_secs(1);
/// 원본이 끝난 뒤 받는 쪽이 마지막 장치 상태를 다 읽기를 기다리는 한도
const INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
/// 시간 제한으로 `migrate_cancel`한 뒤 마이그레이션이 끝나기를 기다리는 한도
const CANCEL_WAIT: Duration = Duration::from_secs(30);

/// VM별 상태 디렉터리 `{state_dir}/vm/{id}`. `state_dir`은 [`QemuConfig::state_dir`]이다.
pub fn vm_state_dir(state_dir: &Path, id: Uuid) -> PathBuf {
    state_dir.join("vm").join(id.to_string())
}

/// VM의 UEFI 변수 파일. UEFI VM을 처음 띄울 때 템플릿을 복사해 만든다(BIOS VM에는 없다).
pub fn uefi_vars_path(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_state_dir(state_dir, id).join("uefi-vars.fd")
}

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
    /// `pause`로 멈추고 아직 `resume`을 부르지 않은 VM. 다시 붙어도 이어서 실행하지 않는다.
    pausing: Mutex<HashSet<Uuid>>,
    /// 받을 포트를 고르고 QEMU가 그 포트를 열 때까지 쥔다. 동시에 받는 두 VM이 같은 포트를 고르지 않게 한다.
    incoming_ports: tokio::sync::Mutex<()>,
}

/// QEMU를 띄우는 방식
#[derive(Clone, Copy)]
enum Launch<'a> {
    /// 띄우고(스냅샷 태그가 있으면 불러온 뒤) 이어서 실행한다.
    Start { restore: Option<&'a str> },
    /// `-incoming defer`로 띄우고 받는 중 표시를 남긴다. 멈춘 채 둔다.
    Incoming,
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
            pausing: Mutex::default(),
            incoming_ports: tokio::sync::Mutex::new(()),
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
                    tracing::warn!(%id, "cleaning up stale runtime dir");
                    // 런타임 디렉터리가 정리할 것이 남았다는 유일한 기록이므로 tap부터 지운다.
                    // 지우지 못하면 디렉터리를 남겨 다음 new()가 다시 시도한다.
                    if delete_taps(hv.config.net.as_deref(), id).await
                        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
                    {
                        tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                    }
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
        paths.uefi_vars = uefi_vars_path(&self.config.state_dir, id);
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        vm_state_dir(&self.config.state_dir, id)
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

    async fn launch(
        &self,
        vm: &VmRuntimeSpec,
        paths: &RuntimePaths,
        how: Launch<'_>,
    ) -> Result<(), DriverError> {
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

        let args = match how {
            Launch::Start { .. } => build_cmdline(vm, &self.config.host, paths),
            Launch::Incoming => build_incoming_cmdline(vm, &self.config.host, paths),
        };
        let mut args = args.map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        if let Launch::Incoming = how {
            // 띄우는 도중에 opld가 죽어도 남은 QEMU가 받는 쪽임을 알 수 있게 먼저 쓴다.
            std::fs::write(paths.dir.join(INCOMING_MARK), b"")?;
        }
        self.create_taps(vm).await?;
        let mut cmd = if self.config.systemd_scope {
            clear_stale_scope(vm.id).await;
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
        let restore = match how {
            Launch::Start { restore } => restore,
            // 받기를 마칠 때까지 멈춰 둔다.
            Launch::Incoming => return Ok(()),
        };
        if let Some(tag) = restore {
            // -S로 멈춘 채 불러온 뒤 cont로 이어서 실행한다. 메모리 상태는 첫 디스크(disk0)에 있다.
            let devices: Vec<String> = (0..vm.disks.len()).map(|i| format!("disk{i}")).collect();
            let load = job(
                handle
                    .qmp
                    .snapshot_load(tag, "disk0", &devices, SNAPSHOT_TIMEOUT),
                "QMP snapshot-load",
            )
            .await;
            if let Err(e) = load {
                drop(handle);
                // 작업이 끝나지 않았으면 QMP가 답하지 않을 수 있으므로 끝날 때까지 기다리고 필요하면 죽인다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop QEMU after snapshot load failed");
                }
                return Err(e);
            }
        }
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
        // create_tap은 같은 이름의 장치를 먼저 지우므로, 이 VM의 이름이 아니면 호스트 인터페이스를 지울 수 있다.
        let prefix = tap_prefix(vm.id);
        if let Some(tap) = taps.iter().find(|t| !t.ifname.starts_with(&prefix)) {
            return Err(DriverError::InvalidSpec(format!(
                "tap name {:?} does not start with {prefix:?}",
                tap.ifname
            )));
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
        // tap부터 지운다. 지우지 못하면 런타임 디렉터리를 남겨 다음 new()가 다시 시도한다.
        if delete_taps(self.config.net.as_deref(), id).await
            && let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
        true
    }

    /// QMP로 다룰 수 있는 실행 중인 VM의 핸들
    fn running_handle(&self, id: Uuid) -> Result<Arc<VmHandle>, DriverError> {
        if let Some(handle) = self.handle(id) {
            return Ok(handle);
        }
        match liveness(&self.paths(id)?.pidfile) {
            Liveness::Dead => Err(DriverError::NotRunning(id)),
            // 살아 있지만 QMP로 다룰 수 없다. 꺼졌다고 알리면 호출자가 쓰고 있는 디스크를 건드린다.
            // 요청을 보내지 않았으므로 시간 초과(작업이 남아 있을 수 있음)와 구별한다.
            Liveness::Alive(_) | Liveness::Unknown => Err(DriverError::NotAttached(id)),
        }
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 스냅샷을 위해
    /// 멈춘 채([`PAUSED_MARK`]) opld가 죽었거나 이어서 실행하지 못한 VM도 멈춘 채 남는다. 둘 다 이어서
    /// 실행한다. 지금 `pause`로 멈춰 둔 VM과 QEMU가 스스로 멈춘 VM(표시 없음)은 그대로 둔다.
    async fn continue_if_left_stopped(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let mark = handle.paths.dir.join(PAUSED_MARK);
        let ours = !self.pausing.lock().unwrap().contains(&id) && mark.exists();
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        let state = flatten(status, "QMP query-status")?;
        if state == RunState::Prelaunch || (ours && state == RunState::Paused) {
            tracing::info!(%id, ?state, "continuing VM left stopped");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        if ours {
            remove_if_exists(&mark)?;
        }
        Ok(())
    }

    /// 마이그레이션 TLS 자격 증명(`{state_dir}/migration-tls`)과 능력·매개변수를 정한다. 자격 증명
    /// 객체는 파일을 만들 때 읽으므로, 인증서가 바뀌었을 수 있어 매번 지우고 다시 만든다.
    async fn setup_migration(
        &self,
        qmp: &QmpClient,
        endpoint: TlsEndpoint,
        tls_hostname: Option<&str>,
        bandwidth: Option<u64>,
    ) -> Result<(), DriverError> {
        // 처음이면 없어서 실패한다.
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(MIGRATION_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        let add = qmp.add_tls_creds(MIGRATION_TLS_ID, &dir, endpoint);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, add).await,
            "QMP object-add",
        )?;
        let caps = qmp.migrate_set_capabilities(&[("events", true), ("auto-converge", true)]);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, caps).await,
            "QMP migrate-set-capabilities",
        )?;
        let params = MigrationParameters {
            tls_creds: Some(MIGRATION_TLS_ID.into()),
            tls_hostname: tls_hostname.map(str::to_string),
            max_bandwidth: bandwidth,
        };
        let set = qmp.migrate_set_parameters(&params);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, set).await,
            "QMP migrate-set-parameters",
        )
    }

    /// 띄운 받는 QEMU에 마이그레이션 설정을 하고 빈 포트에서 받기 시작한다.
    async fn listen_incoming(
        &self,
        id: Uuid,
        host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let handle = self.running_handle(id)?;
        self.setup_migration(&handle.qmp, TlsEndpoint::Server, None, None)
            .await?;
        let _ports = self.incoming_ports.lock().await;
        let Some(port) = pick_port(host, ports.clone()) else {
            return Err(DriverError::Migration(format!(
                "no free migration port in {}-{} on {host}",
                ports.start(),
                ports.end()
            )));
        };
        let uri = tcp_uri(host, port);
        let listen = handle.qmp.migrate_incoming(&uri);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, listen).await,
            "QMP migrate-incoming",
        )?;
        Ok(port)
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
            Ok(Ok(RunState::InMigrate)) => RuntimeStatus::Other("inmigrate".into()),
            Ok(Ok(RunState::PostMigrate)) => RuntimeStatus::Other("postmigrate".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        // 실행 중인데 지금 멈춰 두지 않았으면 표시는 남은 것이다(이어서 실행했지만 지우지 못했거나
        // 누가 `cont`함). 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 다시 붙을 때 이어서 실행한다.
        if status == RuntimeStatus::Running && !self.pausing.lock().unwrap().contains(&id) {
            let mark = handle.paths.dir.join(PAUSED_MARK);
            if let Err(e) = remove_if_exists(&mark) {
                tracing::warn!(%id, error = %e, "cannot remove paused mark");
            }
        }
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        // 이미 살아 있는 VM에는 restore를 쓰지 않는다. -S로 남은 VM도 그대로 이어서 실행한다.
        if self.handle(vm.id).is_some() {
            return self.continue_if_left_stopped(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    return self.continue_if_left_stopped(vm.id).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(id = %vm.id, error = %e, "could not reattach to running VM")
                }
                Err(_) => tracing::error!(id = %vm.id, "timed out reattaching to running VM"),
            }
            return Err(DriverError::Timeout("reattach to running QEMU"));
        }
        if restore.is_some() && vm.disks.is_empty() {
            return Err(DriverError::InvalidSpec(
                "a snapshot to restore needs a disk for its memory state".into(),
            ));
        }
        if let Err(e) = self.launch(vm, &paths, Launch::Start { restore }).await {
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

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        let Some(vmstate) = devices.first() else {
            return Err(DriverError::InvalidSpec(format!("VM {id} has no disks")));
        };
        if memory {
            let save = handle
                .qmp
                .snapshot_save(tag, vmstate, &devices, SNAPSHOT_TIMEOUT);
            job(save, "QMP snapshot-save").await
        } else {
            let r =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.snapshot_disks(tag, &devices))
                    .await;
            flatten(r, "QMP transaction")
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        if devices.is_empty() {
            return Ok(());
        }
        let delete = handle.qmp.snapshot_delete(tag, &devices, SNAPSHOT_TIMEOUT);
        job(delete, "QMP snapshot-delete").await
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let node = format!("disk{disk}");
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.block_resize(&node, size_bytes)).await;
        flatten(r, "QMP block_resize")
    }

    /// 멈추기 전에 표시([`PAUSED_MARK`])를 남긴다.
    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        self.pausing.lock().unwrap().insert(id);
        std::fs::write(handle.paths.dir.join(PAUSED_MARK), b"")?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.stop()).await;
        flatten(r, "QMP stop")
    }

    /// 이어서 실행한 뒤 표시를 지운다. 실패하면 표시가 남아 다시 붙을 때(`start`) 이어서 실행한다.
    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        self.pausing.lock().unwrap().remove(&id);
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        flatten(r, "QMP cont")?;
        remove_if_exists(&handle.paths.dir.join(PAUSED_MARK))?;
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        if self.handle(id).is_none() {
            return Ok(false);
        }
        self.continue_if_left_stopped(id).await?;
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        let output = tokio::process::Command::new(&self.config.host.qemu_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(QMP_TIMEOUT, output)
            .await
            .map_err(|_| DriverError::Timeout("qemu --version"))??;
        let stdout = String::from_utf8_lossy(&output.stdout);
        match parse_qemu_version(&stdout) {
            Some(v) if output.status.success() => Ok(v),
            _ => Err(DriverError::ToolFailed {
                tool: "qemu --version",
                stderr: format!(
                    "{}{}",
                    stdout.trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
        }
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_version()).await;
        flatten(r, "QMP query-version")
    }

    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let paths = self.paths(vm.id)?;
        if self.handle(vm.id).is_some() || liveness(&paths.pidfile) != Liveness::Dead {
            return Err(DriverError::Migration(format!(
                "VM {} is already running on this node",
                vm.id
            )));
        }
        if let Err(e) = self.launch(vm, &paths, Launch::Incoming).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            } else if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
            }
            return Err(e);
        }
        match self.listen_incoming(vm.id, listen_host, ports).await {
            Ok(port) => Ok(port),
            Err(e) => {
                // QEMU 하나는 한 번만 받을 수 있으므로 버린다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
                }
                Err(e)
            }
        }
    }

    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        if !handle.paths.dir.join(INCOMING_MARK).exists() {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        let wait = handle.qmp.wait_migration(MIGRATION_POLL);
        match tokio::time::timeout(INCOMING_WAIT_TIMEOUT, wait).await {
            Err(_) => Err(DriverError::Timeout("incoming migration to complete")),
            // 받기에 실패한 QEMU는 스스로 끝난다.
            Ok(Err(QmpError::Disconnected)) => {
                Err(DriverError::Migration("the receiving QEMU exited".into()))
            }
            Ok(Err(e)) => Err(e.into()),
            Ok(Ok(info)) => migration_result(info),
        }
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let mark = handle.paths.dir.join(INCOMING_MARK);
        if !mark.exists() {
            return Ok(());
        }
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        match flatten(status, "QMP query-status")? {
            RunState::Paused => {
                tracing::info!(%id, "continuing received VM");
                let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
                flatten(cont, "QMP cont")?;
            }
            RunState::Running => {}
            state => {
                return Err(DriverError::Migration(format!(
                    "incoming migration of VM {id} has not completed (QEMU is {state:?})"
                )));
            }
        }
        remove_if_exists(&mark)?;
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<Uuid>, DriverError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = self.paths(id)?;
            if paths.dir.join(INCOMING_MARK).exists() && liveness(&paths.pidfile) != Liveness::Dead
            {
                out.push(id);
            }
        }
        Ok(out)
    }

    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        self.setup_migration(
            qmp,
            TlsEndpoint::Client,
            Some(&target.tls_hostname),
            opts.bandwidth,
        )
        .await?;
        let uri = tcp_uri(&target.host, target.port);
        tracing::info!(%id, %uri, "migrating VM");
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, qmp.migrate(&uri)).await,
            "QMP migrate",
        )?;
        let info =
            match tokio::time::timeout(opts.timeout, qmp.wait_migration(MIGRATION_POLL)).await {
                Ok(r) => r?,
                Err(_) => {
                    tracing::warn!(%id, timeout = ?opts.timeout, "migration timed out; cancelling");
                    let cancel = tokio::time::timeout(QMP_TIMEOUT, qmp.migrate_cancel()).await;
                    flatten(cancel, "QMP migrate_cancel")?;
                    let wait = qmp.wait_migration(MIGRATION_POLL);
                    let info = flatten(
                        tokio::time::timeout(CANCEL_WAIT, wait).await,
                        "migration to cancel",
                    )?;
                    // 취소하는 사이에 끝났으면 그대로 성공이다.
                    if info.state != MigrationState::Completed {
                        return Err(DriverError::Migration(format!(
                            "did not complete within {:?}; cancelled",
                            opts.timeout
                        )));
                    }
                    info
                }
            };
        migration_result(info)
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(false);
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? != RunState::PostMigrate {
            return Ok(false);
        }
        drop(handle);
        tracing::info!(%id, "stopping migrated source QEMU");
        self.stop(id, StopMode::Force).await?;
        Ok(true)
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// 끝난 마이그레이션의 결과
fn migration_result(info: MigrationInfo) -> Result<(), DriverError> {
    match info.state {
        MigrationState::Completed => Ok(()),
        MigrationState::Cancelled => Err(DriverError::Migration("migration was cancelled".into())),
        state => Err(DriverError::Migration(
            info.error
                .unwrap_or_else(|| format!("migration ended as {state:?}")),
        )),
    }
}

/// 실행 중인 VM의 디스크 노드 이름(`disk{i}`)을 디스크 순서로 읽는다. 다시 붙은 VM은 스펙을
/// 모르므로 QEMU에 묻는다.
async fn query_disk_nodes(qmp: &QmpClient) -> Result<Vec<String>, DriverError> {
    let r = tokio::time::timeout(QMP_TIMEOUT, qmp.execute("query-named-block-nodes", None)).await;
    Ok(disk_nodes(&flatten(r, "QMP query-named-block-nodes")?))
}

/// 블록 노드 목록에서 cmdline이 디스크 포맷 노드에 붙인 `disk{i}`만 골라 i 순서로 돌려준다.
fn disk_nodes(nodes: &serde_json::Value) -> Vec<String> {
    let mut disks: Vec<(usize, String)> = nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["node-name"].as_str())
        .filter_map(|name| {
            let i = name.strip_prefix("disk")?.parse().ok()?;
            Some((i, name.to_string()))
        })
        .collect();
    disks.sort();
    disks.into_iter().map(|(_, name)| name).collect()
}

/// QMP 작업 명령을 기다린다. 작업의 제한 시간은 이벤트를 기다리는 동안에만 걸리므로, QEMU가
/// 명령 자체에 답하지 않아도(SIGSTOP 등) 묶이지 않게 한 번 더 감싼다.
async fn job(
    fut: impl Future<Output = Result<(), QmpError>>,
    what: &'static str,
) -> Result<(), DriverError> {
    flatten(
        tokio::time::timeout(SNAPSHOT_TIMEOUT + QMP_TIMEOUT, fut).await,
        what,
    )
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
    // (종료 종류, 프로세스가 끝나 런타임 디렉터리를 지워도 되는지)
    let (kind, exited) = match guest_initiated {
        // SHUTDOWN을 받았으면 종료는 확정이다. QEMU는 소켓을 닫고 곧 끝나므로 잠깐 기다려
        // 디렉터리를 지우되, 플러시 등으로 늦어지면 잠금이 걸린 디렉터리는 남겨 두고
        // (다음 launch/new()가 잠금 해제를 확인하고 정리한다) 종료는 바로 알린다.
        Some(guest_initiated) => {
            let exited = wait_until(SHUTDOWN_EXIT_WAIT, dead).await;
            if !exited {
                tracing::info!(%id, "QEMU is still exiting; leaving runtime dir");
            }
            (EventKind::Stopped { guest_initiated }, exited)
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
            (EventKind::Crashed, true)
        }
    };
    // 등록을 풀기 전에 tap을 지운다. QEMU가 아직 끝나는 중이어도 tap은 곧바로 지워진다(남은 fd는
    // 오류를 받는다). 게스트는 이미 꺼졌으므로 괜찮다. 런타임 디렉터리는 정리할 것이 남았다는
    // 유일한 기록이므로 tap을 지운 뒤에 지운다. 그 전에 끊기거나(드라이버가 내려가 이 태스크가
    // abort됨) tap을 지우지 못하면 디렉터리가 남아 다음 launch/new()가 다시 정리한다.
    if delete_taps(net.as_deref(), id).await
        && exited
        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
    {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// VM의 tap을 지운다. 지웠거나 지울 것이 없으면 true. 실패하면 기록하고 false를 돌려주며,
/// 호출자는 런타임 디렉터리를 남겨 다음 new()가 다시 지우게 한다.
async fn delete_taps(net: Option<&dyn HostNet>, id: Uuid) -> bool {
    if let Some(net) = net
        && let Err(e) = net.delete_vm_taps(id).await
    {
        tracing::warn!(%id, error = %e, "failed to delete tap devices");
        return false;
    }
    true
}

/// 끝난 QEMU의 `--collect` 스코프는 systemd가 비동기로 회수하므로, 곧바로 다시 띄우면
/// systemd-run이 "Unit ... already exists"로 실패한다. 호출자가 이전 QEMU가 없음을 확인했으므로
/// 남은 스코프를 멈추고 실패 기록을 지운다. 오류와 출력은 무시한다.
async fn clear_stale_scope(id: Uuid) {
    let unit = format!("opl-vm-{id}.scope");
    for verb in ["stop", "reset-failed"] {
        let status = tokio::process::Command::new("systemctl")
            .args([verb, "--quiet", &unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(KILL_TIMEOUT, status).await;
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

    use opl_qemu::Accel;
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{Arch, CpuModel, CpuSpec, MachineType, NicModel};

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

        let err = hv.start(&vm, None).await.unwrap_err();
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
        /// 받은 명령의 인자(`commands`와 같은 순서)
        arguments: Arc<Mutex<Vec<serde_json::Value>>>,
        /// 보내면 서버가 (주어진 이벤트를 먼저 보낸 뒤) 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        fake_qmp_with(dir, status, "completed").await
    }

    /// [`fake_qmp`]에 더해 `query-migrate`에 `migration` 상태로 답한다(`failed`면 `error-desc`도).
    /// `migrate_cancel`을 받은 뒤에는 `cancelled`로 답한다.
    async fn fake_qmp_with(dir: &Path, status: &'static str, migration: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let args = Arc::new(Mutex::new(Vec::new()));
        let arguments = args.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            let mut migration = migration;
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
                    "query-version" => serde_json::json!({
                        "qemu": { "major": 8, "minor": 2, "micro": 2 }, "package": ""
                    }),
                    "migrate_cancel" => {
                        migration = "cancelled";
                        serde_json::json!({})
                    }
                    "query-migrate" if migration == "failed" => serde_json::json!({
                        "status": "failed", "error-desc": "Connection refused"
                    }),
                    "query-migrate" => serde_json::json!({ "status": migration }),
                    // 디스크 두 개를 가진 VM의 블록 노드
                    "query-named-block-nodes" => serde_json::json!([
                        { "node-name": "disk1" },
                        { "node-name": "disk1-file" },
                        { "node-name": "disk0" },
                        { "node-name": "disk0-file" },
                    ]),
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                arguments.lock().unwrap().push(cmd["arguments"].clone());
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
            arguments: args,
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

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    /// 스냅샷을 위해 멈춘 채 opld가 죽은 VM은 다시 붙을 때 이어서 실행하고 표시를 지운다.
    #[tokio::test]
    async fn start_continues_vm_left_paused_for_snapshots() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        std::fs::write(dir.join(PAUSED_MARK), b"").unwrap();
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
        assert!(!dir.join(PAUSED_MARK).exists());
    }

    /// 표시 없이 멈춘 VM(QEMU가 스스로 멈춤)은 그대로 둔다.
    #[tokio::test]
    async fn start_leaves_vm_paused_by_qemu() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(!fake.commands.lock().unwrap().contains(&"cont".to_string()));
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60005,
            tls_hostname: "node-b".into(),
        }
    }

    /// 보내는 쪽: TLS 자격 증명(client)을 다시 만들고, 능력·매개변수를 정한 뒤 보내고 끝을 기다린다.
    #[tokio::test]
    async fn migrate_sets_up_tls_and_waits_for_completion() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let state_dir = cfg.state_dir.clone();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        assert_eq!(hv.running_qemu_version(vm.id).await.unwrap(), "8.2.2");
        let opts = MigrateOptions {
            bandwidth: Some(64 << 20),
            timeout: Duration::from_secs(10),
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();

        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("object-del") < at("object-add"));
        assert_eq!(
            arguments[at("object-del")],
            serde_json::json!({ "id": "opl-mig-tls" })
        );
        let add = &arguments[at("object-add")];
        assert_eq!(add["qom-type"], "tls-creds-x509");
        assert_eq!(add["endpoint"], "client");
        assert_eq!(
            add["dir"],
            state_dir.join("migration-tls").display().to_string()
        );
        assert_eq!(
            arguments[at("migrate-set-capabilities")]["capabilities"],
            serde_json::json!([
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true }
            ])
        );
        assert_eq!(
            arguments[at("migrate-set-parameters")],
            serde_json::json!({
                "tls-creds": "opl-mig-tls", "tls-hostname": "node-b", "max-bandwidth": 64u64 << 20
            })
        );
        assert!(at("migrate-set-parameters") < at("migrate"));
        assert_eq!(
            arguments[at("migrate")],
            serde_json::json!({ "uri": "tcp:10.0.0.2:60005" })
        );
        assert!(at("migrate") < at("query-migrate"));
    }

    #[tokio::test]
    async fn failed_migration_reports_qemu_error() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp_with(&dir, "running", "failed").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
        };
        match hv.migrate(vm.id, &target(), &opts).await.unwrap_err() {
            DriverError::Migration(msg) => assert_eq!(msg, "Connection refused"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 시간 제한이 지나면 취소하고 실패로 끝낸다.
    #[tokio::test]
    async fn migration_is_cancelled_after_timeout() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_millis(200),
        };
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::Migration(m) if m.contains("cancelled")),
            "{err:?}"
        );
        assert!(!err.is_transient());
        let commands = fake.commands.lock().unwrap().clone();
        assert!(commands.contains(&"migrate_cancel".to_string()));
        // 보내지 않은 매개변수는 빠진다.
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = commands
            .iter()
            .position(|c| c == "migrate-set-parameters")
            .unwrap();
        assert!(arguments[at].get("max-bandwidth").is_none());
    }

    /// 받은 VM은 멈춰 있으면 이어서 실행하고 표시를 지운다. 아직 받는 중이면 건드리지 않는다.
    #[tokio::test]
    async fn finish_incoming_continues_only_received_vms() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (received, receiving) = (spec(), spec());
        let dir_a = cfg.run_dir.join("vm").join(received.id.to_string());
        let dir_b = cfg.run_dir.join("vm").join(receiving.id.to_string());
        let _qa = hold_pidfile(&dir_a).await;
        let _qb = hold_pidfile(&dir_b).await;
        std::fs::write(dir_a.join(INCOMING_MARK), b"").unwrap();
        std::fs::write(dir_b.join(INCOMING_MARK), b"").unwrap();
        let fake_a = fake_qmp(&dir_a, "paused").await;
        let fake_b = fake_qmp_with(&dir_b, "inmigrate", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mut incoming = hv.list_incoming().await.unwrap();
        incoming.sort();
        let mut expected = vec![received.id, receiving.id];
        expected.sort();
        assert_eq!(incoming, expected);
        // 받는 중인 VM은 reconciler의 이어 가기에도 이어서 실행되지 않는다.
        assert_eq!(
            hv.inspect(receiving.id).await.unwrap().unwrap().status,
            RuntimeStatus::Other("inmigrate".into())
        );
        hv.resume_left_stopped(receiving.id).await.unwrap();
        hv.resume_left_stopped(received.id).await.unwrap();
        assert!(
            !fake_a
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );

        hv.incoming_wait(received.id).await.unwrap();
        hv.finish_incoming(received.id).await.unwrap();
        assert!(
            fake_a
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );
        assert!(!dir_a.join(INCOMING_MARK).exists());
        assert_eq!(hv.list_incoming().await.unwrap(), vec![receiving.id]);
        // 표시가 없으면 아무것도 하지 않고, 기다릴 것도 없다.
        hv.finish_incoming(received.id).await.unwrap();
        assert!(matches!(
            hv.incoming_wait(received.id).await,
            Err(DriverError::Migration(_))
        ));

        let err = hv.finish_incoming(receiving.id).await.unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir_b.join(INCOMING_MARK).exists());
        assert!(
            !fake_b
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );
    }

    /// 보낸 원본(`postmigrate`)이 아니면 끄지 않는다.
    #[tokio::test]
    async fn stop_migrated_leaves_running_vm_alone() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!hv.stop_migrated(vm.id).await.unwrap());
        assert!(!hv.stop_migrated(Uuid::now_v7()).await.unwrap());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    /// 이미 이 노드에서 도는 VM은 받지 않는다(그 QEMU를 건드리지 않는다).
    #[tokio::test]
    async fn start_incoming_refuses_a_running_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let err = hv
            .start_incoming(&vm, "127.0.0.1", 60000..=60099)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(!dir.join(INCOMING_MARK).exists());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    #[tokio::test]
    async fn start_incoming_failure_cleans_run_dir() {
        let tmp = short_tempdir();
        // 곧바로 실패로 끝나는 QEMU
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("false")))
            .await
            .unwrap();
        let vm = spec();
        let err = hv
            .start_incoming(&vm, "127.0.0.1", 60000..=60099)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::StartFailed(_)), "{err:?}");
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        assert!(hv.list_incoming().await.unwrap().is_empty());
    }

    #[test]
    fn disk_nodes_are_format_nodes_in_disk_order() {
        let nodes = serde_json::json!([
            { "node-name": "disk10" },
            { "node-name": "disk2-file" },
            { "node-name": "disk2" },
            { "node-name": "#block123" },
            { "node-name": "diskx" },
            { "node-name": "disk0" },
        ]);
        assert_eq!(disk_nodes(&nodes), ["disk0", "disk2", "disk10"]);
    }

    #[tokio::test]
    async fn snapshot_ops_use_disk_node_names() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        // 디스크만 스냅샷은 모든 디스크를 transaction 하나로 찍는다.
        hv.snapshot_save(vm.id, "s1", false).await.unwrap();
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        let actions = &arguments[at("transaction")]["actions"];
        assert_eq!(actions[0]["data"]["device"], "disk0");
        assert_eq!(actions[1]["data"]["device"], "disk1");
        assert_eq!(actions[1]["data"]["name"], "s1");
        assert_eq!(
            arguments[at("block_resize")],
            serde_json::json!({ "node-name": "disk1", "size": 2u64 << 30 })
        );
    }

    /// 실행 중인데 지금 멈춰 두지 않은 VM의 표시는 남은 것이다(이어서 실행했지만 지우지 못함, 누가
    /// `cont`함). 상태를 보면 지운다. 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 이어서 실행한다.
    #[tokio::test]
    async fn running_vm_drops_a_left_over_paused_mark() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mark = dir.join(PAUSED_MARK);

        // 멈춰 두는 동안에는 지우지 않는다(가짜 QMP는 늘 running이라고 답한다).
        hv.pause(vm.id).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap();
        assert!(mark.exists());
        hv.pausing.lock().unwrap().clear();
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);
        assert!(!mark.exists());
    }

    #[tokio::test]
    async fn pause_and_resume_send_stop_and_cont() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mark = dir.join(PAUSED_MARK);
        hv.pause(vm.id).await.unwrap();
        assert!(mark.exists());
        // 멈춰 둔 동안 reconciler가 다시 붙어도(start) 이어서 실행하지 않는다.
        hv.start(&vm, None).await.unwrap();
        assert!(mark.exists());
        hv.resume(vm.id).await.unwrap();
        assert!(!mark.exists());
        let commands = fake.commands.lock().unwrap().clone();
        let sent: Vec<&str> = commands
            .iter()
            .map(String::as_str)
            .filter(|c| ["stop", "cont"].contains(c))
            .collect();
        assert_eq!(sent, ["stop", "cont"]);

        let err = hv.pause(Uuid::now_v7()).await.unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");
    }

    #[tokio::test]
    async fn snapshot_ops_on_vm_that_is_not_attached() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let err = hv
            .snapshot_save(Uuid::now_v7(), "s", true)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");

        // 살아 있지만 QMP로 다룰 수 없는 VM을 꺼진 것으로 알리면 호출자가 쓰고 있는 디스크를
        // qemu-img로 건드린다. 다시 시도할 수 있는 오류로 끝낸다.
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        for err in [
            hv.snapshot_save(vm.id, "s", true).await.unwrap_err(),
            hv.snapshot_delete(vm.id, "s").await.unwrap_err(),
            hv.block_resize(vm.id, 0, 1 << 30).await.unwrap_err(),
            hv.pause(vm.id).await.unwrap_err(),
            hv.resume(vm.id).await.unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::NotAttached(_)), "{err:?}");
            assert!(err.is_transient());
        }
    }

    #[tokio::test]
    async fn restore_without_disks_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        // 메모리 상태는 첫 디스크에 있으므로 디스크가 없으면 띄우기 전에 거부한다.
        let err = hv.start(&spec(), Some("s1")).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
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

        let err = hv.start(&vm, None).await.unwrap_err();
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
                ifname: opl_types::network::tap_name(vm.id, 0),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn tap_name_of_another_vm_or_host_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        // create_tap이 같은 이름을 먼저 지우므로 호스트 인터페이스 이름은 받으면 안 된다.
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        match hv.start(&vm, None).await.unwrap_err() {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("eth0"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
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
            hv.start(&vm, None).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
```

`crates/opl-driver/tests/qemu_migration.rs`를 만든다:

```rust
//! 실제 QEMU 둘로 한 호스트에서 TLS 라이브 마이그레이션을 하는 드라이버 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it --test qemu_migration`
//! 두 하이퍼바이저는 실행·상태 디렉터리가 다른 두 노드를 흉내 내고, 디스크는 같은 파일(공유 풀)이다.
//! 인증서는 클러스터 PKI(`opl-peer`의 `pki`)와 같은 모양으로 rcgen이 만든다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_driver::{
    DriverError, Hypervisor, MigrateOptions, MigrateTarget, QemuConfig, QemuHypervisor,
    RuntimeStatus, StopMode, migration_tls_dir, write_migration_tls,
};
use opl_qemu::{Accel, HostProfile};
use opl_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use uuid::Uuid;

/// 받는 노드의 주소. 노드 인증서의 IP SAN이다.
const HOST: &str = "127.0.0.1";

fn short_tempdir() -> tempfile::TempDir {
    // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    let mut host = HostProfile::native(Path::new(&share));
    // HVF(macOS)에서는 보내는 QEMU가 메모리 변경 추적을 켜다 가끔 죽는다(QEMU 11.1, `assert_hvf_ok`
    // in `memory_global_dirty_log_start`). macOS 노드는 마이그레이션을 하지 않으므로(API가 거절)
    // 이 Mac에서는 TCG로 돌린다.
    if host.accel == Accel::Hvf {
        host.accel = Accel::Tcg;
    }
    QemuConfig {
        host,
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // systemd 스코프 이름은 VM id로 정해져(`opl-vm-{id}.scope`) 한 호스트의 두 QEMU가 겹친다.
        // 받는 쪽을 띄울 때 남은 스코프로 보고 원본 스코프를 멈추므로 쓰지 않는다.
        systemd_scope: false,
    }
}

/// 클러스터 CA(`ClusterCa::generate`와 같은 모양)
fn cluster_ca() -> (String, Issuer<'static, KeyPair>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params
        .distinguished_name
        .push(DnType::CommonName, "opLite cluster it CA");
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let pem = params.self_signed(&key).unwrap().pem();
    (pem, Issuer::new(params, key))
}

/// 노드 인증서와 키(`ClusterCa::sign_node`와 같은 모양: CN은 노드 id, 서버·클라이언트 겸용)
fn node_cert(ca: &Issuer<'static, KeyPair>, sans: &[&str]) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, Uuid::now_v7().to_string());
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, ca).unwrap();
    (cert.pem(), key.serialize_pem())
}

/// 한 호스트 위의 두 노드: 각자 실행·상태 디렉터리와 노드 인증서를 가진다.
struct Nodes {
    tmp: tempfile::TempDir,
    src: QemuHypervisor,
    dst: QemuHypervisor,
}

async fn nodes() -> Nodes {
    let tmp = short_tempdir();
    let (ca_pem, ca) = cluster_ca();
    let mut hvs = Vec::new();
    for name in ["a", "b"] {
        let cfg = config(&tmp.path().join(name));
        let (cert, key) = node_cert(&ca, &[HOST, "localhost"]);
        write_migration_tls(&migration_tls_dir(&cfg.state_dir), &ca_pem, &cert, &key).unwrap();
        hvs.push(QemuHypervisor::new(cfg).await.unwrap());
    }
    let dst = hvs.pop().unwrap();
    let src = hvs.pop().unwrap();
    Nodes { tmp, src, dst }
}

/// 공유 풀처럼 두 노드가 같은 경로로 여는 qcow2 디스크 하나를 가진 UEFI VM(OS 없음)
fn spec(disk: PathBuf) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "mig-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![RuntimeDisk {
            path: disk,
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![],
    }
}

fn create_disk(dir: &Path) -> PathBuf {
    let disk = dir.join("disk.qcow2");
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
    disk
}

fn vars(root: &Path, id: Uuid) -> PathBuf {
    opl_driver::uefi_vars_path(&root.join("state"), id)
}

async fn status(hv: &QemuHypervisor, id: Uuid) -> Option<RuntimeStatus> {
    hv.inspect(id).await.unwrap().map(|s| s.status)
}

fn opts() -> MigrateOptions {
    MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(120),
    }
}

#[tokio::test]
async fn migrates_a_running_vm_over_tls() {
    let n = nodes().await;
    let vm = spec(create_disk(n.tmp.path()));
    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
    // 넘겨받은 뒤 대상 파일이 원본과 같으면 QEMU가 pflash 내용을 옮겨 쓴 것이다.
    let src_vars = vars(&n.tmp.path().join("a"), vm.id);
    let dst_vars = vars(&n.tmp.path().join("b"), vm.id);
    let mut content = std::fs::read(&config(n.tmp.path()).host.uefi_vars_template).unwrap();
    let marker = b"opl-migration-it";
    let at = content.len() - marker.len();
    content[at..].copy_from_slice(marker);
    std::fs::create_dir_all(src_vars.parent().unwrap()).unwrap();
    std::fs::write(&src_vars, &content).unwrap();
    n.src.start(&vm, None).await.unwrap();

    // 같은 QEMU다.
    let version = n.src.running_qemu_version(vm.id).await.unwrap();
    assert_eq!(version, n.dst.qemu_version().await.unwrap());

    let port = n
        .dst
        .start_incoming(&vm, HOST, 61000..=61019)
        .await
        .unwrap();
    assert!((61000..=61019).contains(&port));
    assert_eq!(
        status(&n.dst, vm.id).await,
        Some(RuntimeStatus::Other("inmigrate".into()))
    );
    assert_eq!(n.dst.list_incoming().await.unwrap(), vec![vm.id]);
    // 그 포트는 이제 받는 QEMU가 쓴다.
    assert!(std::net::TcpListener::bind((HOST, port)).is_err());

    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    n.src.migrate(vm.id, &target, &opts()).await.unwrap();
    assert_eq!(
        status(&n.src, vm.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );
    n.dst.incoming_wait(vm.id).await.unwrap();
    // 받은 뒤에도 넘겨받기 전까지는 멈춰 있다.
    assert_eq!(status(&n.dst, vm.id).await, Some(RuntimeStatus::Paused));

    assert!(!std::fs::read(&dst_vars).unwrap().ends_with(marker));
    n.dst.finish_incoming(vm.id).await.unwrap();
    assert_eq!(status(&n.dst, vm.id).await, Some(RuntimeStatus::Running));
    assert!(n.dst.list_incoming().await.unwrap().is_empty());
    // 대상 QEMU가 이어서 실행하면서 pflash(UEFI 변수)를 자기 파일에 원본 내용으로 쓴다. 대상
    // 펌웨어가 곧바로 변수를 더 바꿀 수 있으므로(KVM) 파일 전체가 아니라 원본 표식만 본다.
    let same_vars = async {
        loop {
            let dst = std::fs::read(&dst_vars).unwrap();
            if dst.ends_with(marker) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), same_vars)
        .await
        .is_err()
    {
        let (src, dst) = (
            std::fs::read(&src_vars).unwrap(),
            std::fs::read(&dst_vars).unwrap(),
        );
        let diff = src.iter().zip(&dst).position(|(a, b)| a != b);
        panic!(
            "UEFI variables were not written on the target: src {} bytes (marker {}), dst {} bytes (marker {}), first difference at {diff:?}",
            src.len(),
            src.ends_with(marker),
            dst.len(),
            dst.ends_with(marker)
        );
    }

    // 원본 정리: postmigrate QEMU를 끈다.
    assert!(n.src.stop_migrated(vm.id).await.unwrap());
    assert_eq!(status(&n.src, vm.id).await, None);
    assert!(!n.dst.stop_migrated(vm.id).await.unwrap());
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
}

/// 확정 전에 실패하면 원본은 그대로 돈다. 받는 QEMU는 버린다.
#[tokio::test]
async fn failed_migration_keeps_the_source_running() {
    let n = nodes().await;
    let vm = spec(create_disk(n.tmp.path()));
    n.src.start(&vm, None).await.unwrap();

    // 아무도 받지 않는 포트
    let closed = {
        let l = std::net::TcpListener::bind((HOST, 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let target = MigrateTarget {
        host: HOST.into(),
        port: closed,
        tls_hostname: HOST.into(),
    };
    let err = n.src.migrate(vm.id, &target, &opts()).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));

    // 받는 노드 인증서에 없는 이름: TLS가 실패한다.
    let port = n
        .dst
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: "not-the-node".into(),
    };
    let err = n.src.migrate(vm.id, &target, &opts()).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));
    assert!(!n.src.stop_migrated(vm.id).await.unwrap());

    // 받는 쪽을 버린다(MigrateIncomingAbort가 할 일).
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(status(&n.dst, vm.id).await, None);
    assert!(n.dst.list_incoming().await.unwrap().is_empty());

    // 다시 받고 보내면 된다(원본 QEMU에 TLS 객체를 다시 만든다).
    let port = n
        .dst
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    n.src.migrate(vm.id, &target, &opts()).await.unwrap();
    n.dst.incoming_wait(vm.id).await.unwrap();

    // 확정하지 않고 되돌린다: 받은 QEMU를 끄고 원본을 이어서 실행한다.
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    n.src.resume(vm.id).await.unwrap();
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
}
```

`crates/opl-peer/src/pki.rs`에서

```rust
            .push(DnType::CommonName, node.to_string());
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
```

을 다음으로 바꾼다:

```rust
            .push(DnType::CommonName, node.to_string());
        // QEMU 8.2(GnuTLS)는 키 용도가 critical이면 마이그레이션 TLS에 keyEncipherment도 요구한다.
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![
```

`crates/opl-peer/src/pki.rs`에서

```rust
        assert!(!shown.contains("evil"), "{shown}");
    }
```

을 다음으로 바꾼다:

```rust
        assert!(!shown.contains("evil"), "{shown}");
        let usage = cert.key_usage().unwrap().unwrap().value;
        assert!(
            usage.digital_signature() && usage.key_encipherment(),
            "{usage:?}"
        );
    }
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
    Ok(a.0)
}
```

을 다음으로 바꾼다:

```rust
    Ok(a.0)
}

/// 라이브 마이그레이션을 받을 QEMU의 인자. [`build_cmdline`]과 같은 장치에 `-incoming defer`를 더한다.
/// QEMU는 QMP `migrate-incoming`을 받을 때까지 기다리고, 받은 뒤에도 `-S`라 `cont` 전까지 멈춰 있다.
pub fn build_incoming_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    let mut args = build_cmdline(spec, host, paths)?;
    args.push("-incoming".into());
    args.push("defer".into());
    Ok(args)
}
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
    }

    #[test]
    fn bios_omits_pflash() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 받는 쪽은 원본과 같은 장치(블록 노드 이름·장치 id)에 `-incoming defer`만 더한다.
    #[test]
    fn incoming_adds_defer_to_the_same_devices() {
        let normal = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        let incoming = build_incoming_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..normal.len()], normal[..]);
        assert_eq!(incoming[normal.len()..], ["-incoming", "defer"]);
        assert!(incoming.contains(&"-S".to_string()));
        assert!(!normal.contains(&"-incoming".to_string()));
        // 맞지 않는 조합은 똑같이 거절한다.
        assert_eq!(
            build_incoming_cmdline(&x86_spec(), &mac_host(), &paths()).unwrap_err(),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
    }

    #[test]
    fn bios_omits_pflash() {
```

`crates/opl-qemu/src/lib.rs`에서

```rust

pub use cmdline::{CmdlineError, build_cmdline};
pub use host::{Accel, HostProfile, kvm_available};
pub use paths::{PathError, RuntimePaths};
pub use qmp::{QmpClient, QmpError, QmpEvent, RunState};
```

을 다음으로 바꾼다:

```rust

pub use cmdline::{CmdlineError, build_cmdline, build_incoming_cmdline};
pub use host::{Accel, HostProfile, kvm_available};
pub use paths::{PathError, RuntimePaths};
pub use qmp::{
    MigrationInfo, MigrationParameters, MigrationState, QmpClient, QmpError, QmpEvent, RunState,
    TlsEndpoint,
};
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
    Shutdown,
    Other(String),
```

을 다음으로 바꾼다:

```rust
    Shutdown,
    /// `-incoming`로 떠서 마이그레이션을 받기 전이거나 받는 중
    InMigrate,
    /// 마이그레이션을 보내 끝낸 원본. `cont`하면 다시 돈다.
    PostMigrate,
    Other(String),
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
            "shutdown" => RunState::Shutdown,
            other => RunState::Other(other.to_string()),
        }
    }
}
```

을 다음으로 바꾼다:

```rust
            "shutdown" => RunState::Shutdown,
            "inmigrate" => RunState::InMigrate,
            "postmigrate" => RunState::PostMigrate,
            other => RunState::Other(other.to_string()),
        }
    }
}

/// `query-migrate`의 `status` 값
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationState {
    /// 마이그레이션을 한 적이 없다(`status`가 없다).
    None,
    Setup,
    Active,
    Completed,
    Failed,
    Cancelling,
    Cancelled,
    Other(String),
}

impl MigrationState {
    fn parse(s: Option<&str>) -> Self {
        match s {
            None | Some("none") => MigrationState::None,
            Some("setup") => MigrationState::Setup,
            Some("active") => MigrationState::Active,
            Some("completed") => MigrationState::Completed,
            Some("failed") => MigrationState::Failed,
            Some("cancelling") => MigrationState::Cancelling,
            Some("cancelled") => MigrationState::Cancelled,
            Some(other) => MigrationState::Other(other.to_string()),
        }
    }

    /// 끝난 상태(완료·실패·취소)인지
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            MigrationState::Completed | MigrationState::Failed | MigrationState::Cancelled
        )
    }
}

/// `query-migrate`의 결과 중 opl이 보는 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationInfo {
    pub state: MigrationState,
    /// 실패했을 때 QEMU가 남긴 까닭(`error-desc`)
    pub error: Option<String>,
}

impl MigrationInfo {
    fn parse(v: &Value) -> Self {
        Self {
            state: MigrationState::parse(v.get("status").and_then(Value::as_str)),
            error: v
                .get("error-desc")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

/// TLS 자격 증명 객체(`tls-creds-x509`)를 쓰는 쪽
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsEndpoint {
    /// 받는 쪽. 상대(보내는 쪽)의 인증서도 확인한다(`verify-peer`).
    Server,
    /// 보내는 쪽
    Client,
}

/// `migrate-set-parameters`로 바꿀 값. `None`인 것은 보내지 않는다(QEMU의 지금 값 그대로).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationParameters {
    /// TLS 자격 증명 객체 id
    pub tls_creds: Option<String>,
    /// 보내는 쪽이 받는 쪽 인증서에서 확인할 이름(IP SAN이나 DNS SAN)
    pub tls_hostname: Option<String>,
    /// 바이트/초
    pub max_bandwidth: Option<u64>,
}
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
        self.execute("block_resize", Some(args)).await.map(drop)
    }
```

을 다음으로 바꾼다:

```rust
        self.execute("block_resize", Some(args)).await.map(drop)
    }

    /// QEMU 버전 `major.minor.micro`
    pub async fn query_version(&self) -> Result<String, QmpError> {
        let ret = self.execute("query-version", None).await?;
        let q = &ret["qemu"];
        match (
            q["major"].as_u64(),
            q["minor"].as_u64(),
            q["micro"].as_u64(),
        ) {
            (Some(major), Some(minor), Some(micro)) => Ok(format!("{major}.{minor}.{micro}")),
            _ => Err(QmpError::Protocol(format!(
                "query-version without version: {ret}"
            ))),
        }
    }

    /// TLS 자격 증명 객체 `id`를 만든다. `dir`에 `ca-cert.pem`과 받는 쪽은 `server-cert.pem`·
    /// `server-key.pem`, 보내는 쪽은 `client-cert.pem`·`client-key.pem`이 있어야 한다(만들 때 읽는다).
    /// 늘 상대의 인증서를 확인한다(`verify-peer`).
    pub async fn add_tls_creds(
        &self,
        id: &str,
        dir: &Path,
        endpoint: TlsEndpoint,
    ) -> Result<(), QmpError> {
        let endpoint = match endpoint {
            TlsEndpoint::Server => "server",
            TlsEndpoint::Client => "client",
        };
        let args = json!({
            "qom-type": "tls-creds-x509",
            "id": id,
            "dir": dir.display().to_string(),
            "endpoint": endpoint,
            "verify-peer": true,
        });
        self.execute("object-add", Some(args)).await.map(drop)
    }

    /// `object-add`로 만든 객체 `id`를 지운다.
    pub async fn object_del(&self, id: &str) -> Result<(), QmpError> {
        self.execute("object-del", Some(json!({ "id": id })))
            .await
            .map(drop)
    }

    /// 마이그레이션 능력을 켜거나 끈다. 예: `("events", true)`
    pub async fn migrate_set_capabilities(
        &self,
        capabilities: &[(&str, bool)],
    ) -> Result<(), QmpError> {
        let caps: Vec<Value> = capabilities
            .iter()
            .map(|(name, state)| json!({ "capability": name, "state": state }))
            .collect();
        self.execute(
            "migrate-set-capabilities",
            Some(json!({ "capabilities": caps })),
        )
        .await
        .map(drop)
    }

    pub async fn migrate_set_parameters(
        &self,
        params: &MigrationParameters,
    ) -> Result<(), QmpError> {
        let mut args = serde_json::Map::new();
        if let Some(creds) = &params.tls_creds {
            args.insert("tls-creds".into(), json!(creds));
        }
        if let Some(host) = &params.tls_hostname {
            args.insert("tls-hostname".into(), json!(host));
        }
        if let Some(bw) = params.max_bandwidth {
            args.insert("max-bandwidth".into(), json!(bw));
        }
        self.execute("migrate-set-parameters", Some(Value::Object(args)))
            .await
            .map(drop)
    }

    /// 마이그레이션을 `uri`(`tcp:host:port`)로 보내기 시작한다. 곧바로 돌아오므로 끝은
    /// [`wait_migration`](Self::wait_migration)으로 기다린다.
    pub async fn migrate(&self, uri: &str) -> Result<(), QmpError> {
        self.execute("migrate", Some(json!({ "uri": uri })))
            .await
            .map(drop)
    }

    /// `-incoming defer`로 띄운 QEMU가 `uri`에서 마이그레이션을 받기 시작한다. 돌아올 때는 이미
    /// 포트를 열었다. QEMU 하나에 한 번만 부를 수 있다(실패해도 다시 부를 수 없다).
    pub async fn migrate_incoming(&self, uri: &str) -> Result<(), QmpError> {
        self.execute("migrate-incoming", Some(json!({ "uri": uri })))
            .await
            .map(drop)
    }

    /// 진행 중인 마이그레이션을 취소한다. 원본은 계속 돈다.
    pub async fn migrate_cancel(&self) -> Result<(), QmpError> {
        self.execute("migrate_cancel", None).await.map(drop)
    }

    pub async fn query_migrate(&self) -> Result<MigrationInfo, QmpError> {
        let ret = self.execute("query-migrate", None).await?;
        Ok(MigrationInfo::parse(&ret))
    }

    /// 마이그레이션이 끝난 상태(완료·실패·취소)가 될 때까지 기다린다. `MIGRATION` 이벤트(능력
    /// `events`)를 받거나 `poll`마다 `query-migrate`로 확인한다. 제한 시간은 호출자가 건다.
    pub async fn wait_migration(&self, poll: Duration) -> Result<MigrationInfo, QmpError> {
        // 처음 조회와 이벤트 사이에 끝나도 놓치지 않게 먼저 구독한다.
        let mut events = self.subscribe();
        loop {
            let info = self.query_migrate().await?;
            if info.state.is_finished() {
                return Ok(info);
            }
            tokio::select! {
                ev = events.recv() => match ev {
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                    // 이벤트 내용과 상관없이 다시 조회한다(놓쳤으면 Lagged).
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                },
                _ = tokio::time::sleep(poll) => {}
            }
        }
    }
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
```

을 다음으로 바꾼다:

```rust
    #[tokio::test]
    async fn migration_states_are_parsed() {
        let (client, mut fake, _dir) = setup().await;
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "postmigrate" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::PostMigrate);
        let (status, ()) = tokio::join!(client.query_status(), async {
            fake.answer(
                "query-status",
                json!({ "return": { "running": false, "status": "inmigrate" } }),
            )
            .await;
        });
        assert_eq!(status.unwrap(), RunState::InMigrate);

        // 마이그레이션을 한 적이 없으면 status가 없다.
        for (ret, state, error) in [
            (json!({}), MigrationState::None, None),
            (
                json!({ "status": "setup", "socket-address": [{ "port": "60000" }] }),
                MigrationState::Setup,
                None,
            ),
            (json!({ "status": "active" }), MigrationState::Active, None),
            (
                json!({ "status": "completed" }),
                MigrationState::Completed,
                None,
            ),
            (
                json!({
                    "status": "failed",
                    "error-desc": "Failed to connect to '127.0.0.1:60099': Connection refused"
                }),
                MigrationState::Failed,
                Some("Failed to connect to '127.0.0.1:60099': Connection refused"),
            ),
            (
                json!({ "status": "cancelled" }),
                MigrationState::Cancelled,
                None,
            ),
            (
                json!({ "status": "device" }),
                MigrationState::Other("device".into()),
                None,
            ),
        ] {
            let (info, ()) = tokio::join!(client.query_migrate(), async {
                fake.answer("query-migrate", json!({ "return": ret })).await;
            });
            let info = info.unwrap();
            assert_eq!(info.state, state);
            assert_eq!(info.error.as_deref(), error);
        }
        assert!(MigrationState::Completed.is_finished());
        assert!(MigrationState::Failed.is_finished());
        assert!(MigrationState::Cancelled.is_finished());
        assert!(!MigrationState::Cancelling.is_finished());
        assert!(!MigrationState::None.is_finished());
    }

    #[tokio::test]
    async fn query_version_formats_major_minor_micro() {
        let (client, mut fake, _dir) = setup().await;
        let (v, ()) = tokio::join!(client.query_version(), async {
            fake.answer(
                "query-version",
                json!({ "return": { "qemu": { "micro": 2, "minor": 2, "major": 8 }, "package": "Debian 1:8.2.2" } }),
            )
            .await;
        });
        assert_eq!(v.unwrap(), "8.2.2");
        let (v, ()) = tokio::join!(client.query_version(), async {
            fake.answer("query-version", json!({ "return": {} })).await;
        });
        assert!(matches!(v, Err(QmpError::Protocol(_))));
    }

    #[tokio::test]
    async fn migration_setup_commands_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let dir = Path::new("/var/lib/opl/migration-tls");
        let (r, cmd) = tokio::join!(
            client.add_tls_creds("opl-mig-tls", dir, TlsEndpoint::Server),
            async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            }
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "object-add");
        assert_eq!(
            cmd["arguments"],
            json!({
                "qom-type": "tls-creds-x509", "id": "opl-mig-tls",
                "dir": "/var/lib/opl/migration-tls", "endpoint": "server", "verify-peer": true
            })
        );
        let (r, cmd) = tokio::join!(client.add_tls_creds("c", dir, TlsEndpoint::Client), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["arguments"]["endpoint"], "client");

        let (r, cmd) = tokio::join!(client.object_del("opl-mig-tls"), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["execute"], "object-del");
        assert_eq!(cmd["arguments"], json!({ "id": "opl-mig-tls" }));

        let (r, cmd) = tokio::join!(
            client.migrate_set_capabilities(&[("events", true), ("auto-converge", true)]),
            async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            }
        );
        r.unwrap();
        assert_eq!(cmd["execute"], "migrate-set-capabilities");
        assert_eq!(
            cmd["arguments"],
            json!({ "capabilities": [
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true }
            ] })
        );

        // 정한 값만 보낸다.
        let params = MigrationParameters {
            tls_creds: Some("opl-mig-tls".into()),
            tls_hostname: Some("10.0.0.2".into()),
            max_bandwidth: Some(100 << 20),
        };
        let (r, cmd) = tokio::join!(client.migrate_set_parameters(&params), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["execute"], "migrate-set-parameters");
        assert_eq!(
            cmd["arguments"],
            json!({ "tls-creds": "opl-mig-tls", "tls-hostname": "10.0.0.2", "max-bandwidth": 100u64 << 20 })
        );
        let params = MigrationParameters {
            tls_creds: Some("opl-mig-tls".into()),
            ..Default::default()
        };
        let (r, cmd) = tokio::join!(client.migrate_set_parameters(&params), async {
            let cmd = fake.recv().await;
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            cmd
        });
        r.unwrap();
        assert_eq!(cmd["arguments"], json!({ "tls-creds": "opl-mig-tls" }));

        for (name, expect) in [("migrate", "migrate"), ("incoming", "migrate-incoming")] {
            let uri = "tcp:10.0.0.2:60000";
            let fut = async {
                match name {
                    "migrate" => client.migrate(uri).await,
                    _ => client.migrate_incoming(uri).await,
                }
            };
            let (r, cmd) = tokio::join!(fut, async {
                let cmd = fake.recv().await;
                fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
                cmd
            });
            r.unwrap();
            assert_eq!(cmd["execute"], expect);
            assert_eq!(cmd["arguments"], json!({ "uri": uri }));
        }
        let (r, ()) = tokio::join!(client.migrate_cancel(), async {
            fake.answer("migrate_cancel", json!({ "return": {} })).await;
        });
        r.unwrap();
    }

    /// 이벤트를 받으면 다시 조회하고, 끝난 상태에서 돌아온다.
    #[tokio::test]
    async fn wait_migration_requeries_on_events_until_finished() {
        let (client, mut fake, _dir) = setup().await;
        // 긴 poll: 이벤트로만 깨어나야 시간 안에 끝난다.
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_secs(3600)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            fake.send(
                json!({ "event": "MIGRATION", "data": { "status": "failed" }, "timestamp": {} }),
            )
            .await;
            fake.answer(
                "query-migrate",
                json!({ "return": { "status": "failed", "error-desc": "TLS handshake failed" } }),
            )
            .await;
        });
        let info = info.unwrap();
        assert_eq!(info.state, MigrationState::Failed);
        assert_eq!(info.error.as_deref(), Some("TLS handshake failed"));
    }

    /// 이벤트가 없어도 poll마다 조회한다(능력 `events`가 꺼진 QEMU, 놓친 이벤트).
    #[tokio::test]
    async fn wait_migration_polls_without_events() {
        let (client, mut fake, _dir) = setup().await;
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_millis(20)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "setup" } }))
                .await;
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            fake.answer(
                "query-migrate",
                json!({ "return": { "status": "completed" } }),
            )
            .await;
        });
        assert_eq!(info.unwrap().state, MigrationState::Completed);
    }

    #[tokio::test]
    async fn wait_migration_fails_when_qemu_goes_away() {
        let (client, mut fake, _dir) = setup().await;
        let (info, ()) = tokio::join!(client.wait_migration(Duration::from_secs(3600)), async {
            fake.answer("query-migrate", json!({ "return": { "status": "active" } }))
                .await;
            drop(fake);
        });
        assert!(matches!(info, Err(QmpError::Disconnected)), "{info:?}");
    }

    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 683개, 실패 0

Run: `cargo test -p opl-qemu -p opl-driver --features qemu-it`
Expected: PASS(실제 마이그레이션 2개 포함)

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(driver): QEMU 라이브 마이그레이션 드라이버 (TLS, -incoming defer)"
```

---

### Task 3: 노드 사이 RPC와 받는 쪽

opl-peer에 `MigrateIncoming { vm, task, qemu_version } -> { port }`·`MigrateIncomingWait`·`MigrateIncomingAbort`(`IncomingMigration { vm, task }`), `IncomingError { Incompatible, Rejected, Failed, Unavailable }`(응답 본문), `MigrationReceiver` trait과 `PeerService::with_migrations`, 클라이언트 `Peers::migrate_incoming`(120초)·`migrate_incoming_wait`(180초)·`migrate_incoming_abort`(60초). 받는 쪽은 opl-api `IncomingMigrations`: QEMU 버전 비교, VM이 `Migrating`이고 이 노드·Task를 가리키는지(Raft가 늦으면 10초까지 기다린다), 디스크 켜기, 받는 QEMU 띄우기, 실패하면 정리. Abort는 확정 뒤면 `Rejected`. opld가 시작 때 `{data_dir}/migration-tls`를 쓰고 받는 쪽을 피어 서비스에 연결한다(받는 QEMU는 `cluster_listen` IP에서 듣고, 노드 인증서에 keyEncipherment가 없으면 경고).

**Files:**
- Create: `crates/opl-api/src/incoming.rs`
- Create: `crates/opl-api/tests/incoming.rs`
- Create: `crates/opl-peer/src/migration.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-peer/build.rs`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/lib.rs`
- Modify: `crates/opl-peer/src/pki.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/src/wire.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/tests/smoke.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
argon2.workspace = true
axum.workspace = true
```

을 다음으로 바꾼다:

```toml
argon2.workspace = true
async-trait.workspace = true
axum.workspace = true
```

`crates/opl-api/Cargo.toml`에서

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

`crates/opl-api/src/incoming.rs`를 만든다:

```rust
//! 라이브 마이그레이션의 받는 쪽(대상 노드). 원본 노드의 `VmMigrate` Task가 노드 사이 RPC로 부른다
//! (설계 §2의 3·5단계와 실패 때의 취소).

use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_driver::{Hypervisor, StopMode, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::resolve::runtime_spec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use uuid::Uuid;

/// 원본 노드가 기록한 `Migrating` 상태가 이 노드에 적용되기를 기다리는 시간. 원본은 상태를 쓴 직후
/// 부르므로 이 노드의 저장소가 조금 늦을 수 있다.
const STATUS_WAIT: Duration = Duration::from_secs(10);
const STATUS_POLL: Duration = Duration::from_millis(50);

pub struct IncomingConfig {
    /// 이 노드의 id
    pub node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(reconciler와 같다)
    pub mount_root: PathBuf,
    /// 디스크 볼륨을 켜고 끄는 저장소(reconciler와 같다)
    pub storage: Arc<dyn Storage>,
    /// 받는 QEMU가 들을 IP(opld `cluster_listen`의 IP). 원본은 이 노드의 클러스터 주소의 호스트로
    /// 접속한다(노드 인증서에 있는 이름).
    pub listen_host: String,
    /// 받는 포트 범위(`[migration] ports`)
    pub ports: RangeInclusive<u16>,
}

/// 대상 노드에서 마이그레이션을 받는다. 받는 VM은 아직 원본 노드에 배정돼 있으므로(`spec.node`)
/// 이 노드의 reconciler는 손대지 않는다. 확정 뒤 넘겨받는 것은 reconciler다.
pub struct IncomingMigrations {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: IncomingConfig,
}

impl IncomingMigrations {
    pub fn new(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: IncomingConfig) -> Self {
        Self {
            cluster,
            hv,
            config,
        }
    }

    fn read_vm(&self, id: Uuid) -> Result<Option<Vm>, IncomingError> {
        self.cluster
            .get::<VmSpec>(id)
            .map_err(|e| IncomingError::Failed(format!("cannot read VM {id}: {e}")))
    }

    /// VM이 `task`로 이 노드에 오는 중이면 그 VM. 아니면 거절 이유.
    fn migrating_here(&self, vm: Option<Vm>, id: Uuid, task: Uuid) -> Result<Vm, String> {
        let me = self.config.node;
        let vm = vm.ok_or_else(|| format!("VM {id} not found"))?;
        if vm.spec.node == Some(me) {
            return Err(format!("VM {:?} is already on this node", vm.name));
        }
        let expected = Some(MigrationStatus { node: me, task });
        if vm.status.phase != VmPhase::Migrating || vm.status.migration != expected {
            return Err(format!(
                "VM {:?} is not migrating to this node by task {task}",
                vm.name
            ));
        }
        Ok(vm)
    }

    /// 원본이 막 쓴 `Migrating` 상태가 보일 때까지 잠시 기다린다.
    async fn wait_migrating_here(&self, id: Uuid, task: Uuid) -> Result<Vm, IncomingError> {
        let deadline = tokio::time::Instant::now() + STATUS_WAIT;
        loop {
            match self.migrating_here(self.read_vm(id)?, id, task) {
                Ok(vm) => return Ok(vm),
                Err(why) if tokio::time::Instant::now() >= deadline => {
                    return Err(IncomingError::Rejected(why));
                }
                Err(_) => tokio::time::sleep(STATUS_POLL).await,
            }
        }
    }

    /// 받으려고 켠 디스크 볼륨을 끈다. 끄지 못하면 경고만 남긴다(남은 볼륨은 Task 실행기의 정리가 끈다).
    async fn deactivate_disks(&self, vm: &Vm) {
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }
}

#[async_trait]
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 디스크를 켜고 받는 QEMU를
    /// 띄운다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
        let mine = self
            .hv
            .qemu_version()
            .await
            .map_err(|e| IncomingError::Failed(format!("cannot read the QEMU version: {e}")))?;
        if mine != req.qemu_version {
            return Err(IncomingError::Incompatible(format!(
                "the target node runs QEMU {mine} but the source runs {}",
                req.qemu_version
            )));
        }
        let vm = self.wait_migrating_here(req.vm, req.task).await?;
        let c = &self.config;
        // 실패하면 runtime_spec이 켠 볼륨을 끈다.
        let spec = runtime_spec(&self.cluster, &vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| IncomingError::Failed(e.to_string()))?;
        match self
            .hv
            .start_incoming(&spec, &c.listen_host, c.ports.clone())
            .await
        {
            Ok(port) => {
                tracing::info!(vm = %vm.id, task = %req.task, port, "receiving a live migration");
                Ok(port)
            }
            Err(e) => {
                self.deactivate_disks(&vm).await;
                Err(IncomingError::Failed(e.to_string()))
            }
        }
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.migrating_here(self.read_vm(req.vm)?, req.vm, req.task)
            .map_err(IncomingError::Rejected)?;
        self.hv
            .incoming_wait(req.vm)
            .await
            .map_err(|e| IncomingError::Failed(e.to_string()))
    }

    /// 확정 전이면(`spec.node`가 이 노드가 아니다) 받는 중 표시가 있는 QEMU를 끄고 디스크를 끈다.
    /// 받는 QEMU가 이미 끝났어도(받기 실패) 디스크는 끈다. 확정됐거나 VM이 다른 마이그레이션 중이면
    /// 거절하고 아무것도 하지 않는다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        let vm = self.read_vm(req.vm)?;
        if let Some(vm) = &vm {
            if vm.spec.node == Some(self.config.node) {
                return Err(IncomingError::Rejected(format!(
                    "the migration of VM {:?} has already been committed",
                    vm.name
                )));
            }
            if let Some(m) = vm.status.migration
                && m.task != req.task
            {
                return Err(IncomingError::Rejected(format!(
                    "VM {:?} is in another migration (task {})",
                    vm.name, m.task
                )));
            }
        }
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        if self
            .hv
            .list_incoming()
            .await
            .map_err(fail)?
            .contains(&req.vm)
        {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an aborted migration");
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        } else if self.hv.inspect(req.vm).await.map_err(fail)?.is_some() {
            // 받는 중 표시가 없는 QEMU는 받는 QEMU가 아니다. 그 디스크도 쓰는 중이다.
            return Ok(());
        }
        if let Some(vm) = &vm {
            self.deactivate_disks(vm).await;
        }
        Ok(())
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod error;
mod janitor;
```

을 다음으로 바꾼다:

```rust
pub mod error;
mod incoming;
mod janitor;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
```

을 다음으로 바꾼다:

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use incoming::{IncomingConfig, IncomingMigrations};
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
```

`crates/opl-api/tests/incoming.rs`를 만든다:

```rust
//! 라이브 마이그레이션의 받는 쪽(`IncomingMigrations`): 원본 노드가 RPC로 부르는 처리를 직접 부른다.
//! 이 노드(Env의 노드)가 대상이고, 원본 노드는 저장소에만 있다.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Env, FakeStorage, MOUNT_ROOT, vm_request};
use opl_api::{IncomingConfig, IncomingMigrations};
use opl_driver::mock::MOCK_QEMU_VERSION;
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

struct Setup {
    env: Env,
    storage: Arc<FakeStorage>,
    receiver: IncomingMigrations,
    /// 원본 노드
    source: Uuid,
    vm: Uuid,
    volume: Uuid,
}

/// 공유 풀 `nas`의 볼륨 `root`를 디스크로 쓰는 VM `web`을 원본 노드에 배정해 둔다(이 노드의
/// reconciler는 손대지 않는다).
async fn setup() -> Setup {
    let env = Env::start().await;
    let admin = env.admin().await;
    let source: Node = Resource::new(
        "node2",
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
        },
    );
    env.cluster.create(&source).await.unwrap();
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "nas",
                "spec": {
                    "backend": { "kind": "dir", "path": env.dir.path().join("nas") },
                    "shared": true
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("nas").await;
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(source.id);
    volume.status.phase = VolumePhase::Ready;
    env.cluster.create(&volume).await.unwrap();
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut vm = vm_named(&env, "web");
    vm.spec.node = Some(source.id);
    env.cluster
        .update_spec(vm.id, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();

    let storage = Arc::new(FakeStorage::default());
    let receiver = IncomingMigrations::new(
        env.cluster.clone(),
        env.hv.clone() as Arc<dyn Hypervisor>,
        IncomingConfig {
            node: env.node,
            mount_root: MOUNT_ROOT.into(),
            storage: storage.clone() as Arc<dyn Storage>,
            listen_host: "127.0.0.1".into(),
            ports: 61000..=61009,
        },
    );
    Setup {
        source: source.id,
        vm: vm.id,
        volume: volume.id,
        env,
        storage,
        receiver,
    }
}

fn vm_named(env: &Env, name: &str) -> Vm {
    env.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

impl Setup {
    /// 원본 노드가 하듯 VM을 `Migrating`으로, 대상을 `node`로 기록한다.
    async fn mark_migrating(&self, node: Uuid, task: Uuid) {
        let mut vm = self.env.cluster.get::<VmSpec>(self.vm).unwrap().unwrap();
        vm.status.phase = VmPhase::Migrating;
        vm.status.node = Some(self.source);
        vm.status.migration = Some(MigrationStatus { node, task });
        self.env
            .cluster
            .update_status::<VmSpec>(self.vm, &vm.status)
            .await
            .unwrap();
    }

    fn request(&self, task: Uuid) -> MigrateIncomingRequest {
        MigrateIncomingRequest {
            vm: self.vm,
            task,
            qemu_version: MOCK_QEMU_VERSION.into(),
        }
    }

    fn deactivations(&self) -> usize {
        self.storage
            .calls()
            .iter()
            .filter(|(op, id)| *op == "deactivate" && *id == self.volume)
            .count()
    }
}

#[tokio::test]
async fn receives_waits_and_aborts() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;

    let port = s.receiver.incoming(s.request(task)).await.unwrap();
    assert_eq!(port, 61000);
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![s.vm]);
    // 원본 노드에 배정된 VM이므로 이 노드의 reconciler는 받는 QEMU를 그대로 둔다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);

    let migration = IncomingMigration { vm: s.vm, task };
    s.receiver.wait(migration).await.unwrap();
    // 다른 Task의 확인은 거절한다.
    let other = IncomingMigration {
        vm: s.vm,
        task: Uuid::now_v7(),
    };
    let err = s.receiver.wait(other).await.unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    let err = s.receiver.abort(other).await.unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    assert!(s.env.hv.is_incoming(s.vm));

    // 확정 전 취소: 받는 QEMU를 끄고 디스크를 끈다. 다시 불러도 된다.
    s.receiver.abort(migration).await.unwrap();
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
    assert_eq!(s.deactivations(), 1);
    s.receiver.abort(migration).await.unwrap();
}

#[tokio::test]
async fn waits_for_the_migrating_status_to_arrive() {
    let s = setup().await;
    let task = Uuid::now_v7();
    // 원본이 상태를 쓰기 전에 요청이 먼저 와도 상태가 보이면 받는다.
    let incoming = s.receiver.incoming(s.request(task));
    let mark = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s.mark_migrating(s.env.node, task).await;
    };
    let (port, ()) = tokio::join!(incoming, mark);
    assert_eq!(port.unwrap(), 61000);
    assert!(s.env.hv.is_incoming(s.vm));
}

#[tokio::test]
async fn refuses_a_vm_not_migrating_here_or_another_qemu() {
    let s = setup().await;
    let task = Uuid::now_v7();

    // 다른 QEMU 버전
    s.mark_migrating(s.env.node, task).await;
    s.env.hv.set_qemu_version("9.0.0");
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Incompatible(m) if m.contains("9.0.0") && m.contains(MOCK_QEMU_VERSION)),
        "{err:?}"
    );
    s.env.hv.set_qemu_version(MOCK_QEMU_VERSION);

    // 다른 노드로 가는 중이다(기다려도 이 노드로 오는 상태가 보이지 않는다).
    s.mark_migrating(s.source, task).await;
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Rejected(m) if m.contains("not migrating to this node")),
        "{err:?}"
    );
    // 다른 Task로 오는 중이다.
    s.mark_migrating(s.env.node, Uuid::now_v7()).await;
    let err = s
        .receiver
        .wait(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Rejected(_)), "{err:?}");
    assert!(s.env.hv.launches().is_empty());
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 0);
}

#[tokio::test]
async fn failures_turn_disks_off_again() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;

    // QEMU를 띄우지 못하면 켠 디스크를 끈다.
    s.env
        .hv
        .fail_next_start(DriverError::StartFailed("no such machine".into()));
    let err = s.receiver.incoming(s.request(task)).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m) if m.contains("no such machine")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 1);

    // 받기에 실패하면 받는 QEMU는 스스로 끝난다. 취소가 디스크를 끈다.
    s.receiver.incoming(s.request(task)).await.unwrap();
    s.env
        .hv
        .fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let migration = IncomingMigration { vm: s.vm, task };
    let err = s.receiver.wait(migration).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Failed(m) if m.contains("TLS handshake failed")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    s.receiver.abort(migration).await.unwrap();
    assert_eq!(s.deactivations(), 2);
}

#[tokio::test]
async fn abort_leaves_a_committed_migration_alone() {
    let s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    let migration = IncomingMigration { vm: s.vm, task };
    s.receiver.wait(migration).await.unwrap();

    // 원본이 확정했다(spec.node가 이 노드). 늦게 온 취소는 받은 QEMU를 끄지 않는다.
    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.spec.node = Some(s.env.node);
    s.env
        .cluster
        .update_spec(s.vm, None, vm.labels.clone(), &vm.spec)
        .await
        .unwrap();
    let err = s.receiver.abort(migration).await.unwrap_err();
    assert!(
        matches!(&err, IncomingError::Rejected(m) if m.contains("committed")),
        "{err:?}"
    );
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);
}
```

`crates/opl-peer/build.rs`에서

```rust
        ))
        .method(
```

을 다음으로 바꾼다:

```rust
        ))
        .method(json(
            "migrate_incoming",
            "MigrateIncoming",
            "crate::migration::MigrateIncomingRequest",
            "crate::wire::MigrateIncomingReply",
        ))
        .method(json(
            "migrate_incoming_wait",
            "MigrateIncomingWait",
            "crate::migration::IncomingMigration",
            "crate::wire::IncomingReply",
        ))
        .method(json(
            "migrate_incoming_abort",
            "MigrateIncomingAbort",
            "crate::migration::IncomingMigration",
            "crate::wire::IncomingReply",
        ))
        .method(
```

`crates/opl-peer/src/client.rs`에서

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔.
```

을 다음으로 바꾼다:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기.
```

`crates/opl-peer/src/client.rs`에서

```rust

use crate::pki::NodeTls;
```

을 다음으로 바꾼다:

```rust

use crate::migration::{IncomingError, IncomingMigration, MigrateIncomingRequest};
use crate::pki::NodeTls;
```

`crates/opl-peer/src/client.rs`에서

```rust
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
```

을 다음으로 바꾼다:

```rust
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// 받을 준비(`MigrateIncoming`)를 기다리는 최대 시간. 대상 노드가 VM 상태를 따라잡고(최대 10초),
/// 디스크를 켜고, QEMU를 띄우는 시간이다.
const MIGRATE_INCOMING_TIMEOUT: Duration = Duration::from_secs(120);

/// 받기 확인(`MigrateIncomingWait`)을 기다리는 최대 시간. 대상 노드의 드라이버가 받기를 최대 120초
/// 기다리므로(`incoming_wait`) 그보다 길어야 그 결과를 받는다.
const MIGRATE_INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

/// 받기 취소(`MigrateIncomingAbort`)를 기다리는 최대 시간. 받는 QEMU를 강제로 끄고 디스크를 끈다.
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);
```

`crates/opl-peer/src/client.rs`에서

```rust
        })
    }
}
```

을 다음으로 바꾼다:

```rust
        })
    }
}

impl Peers {
    /// `addr`의 노드에 VM을 받을 준비를 시킨다. 받는 QEMU가 연 포트를 돌려준다.
    pub async fn migrate_incoming(
        &self,
        addr: &str,
        req: MigrateIncomingRequest,
    ) -> Result<u16, IncomingError> {
        let mut req = Request::new(req);
        req.set_timeout(MIGRATE_INCOMING_TIMEOUT);
        let reply = self
            .client(addr)
            .map_err(unreachable)?
            .migrate_incoming(req)
            .await
            .map_err(unreachable)?;
        reply.into_inner().map(|r| r.port)
    }

    /// `addr`의 노드에서 받는 QEMU가 받기를 마칠 때까지 기다린다.
    pub async fn migrate_incoming_wait(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let mut req = Request::new(migration);
        req.set_timeout(MIGRATE_INCOMING_WAIT_TIMEOUT);
        self.client(addr)
            .map_err(unreachable)?
            .migrate_incoming_wait(req)
            .await
            .map_err(unreachable)?
            .into_inner()
    }

    /// `addr`의 노드에 확정 전의 마이그레이션을 버리게 한다(받는 QEMU를 끄고 디스크를 끈다).
    pub async fn migrate_incoming_abort(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let mut req = Request::new(migration);
        req.set_timeout(MIGRATE_INCOMING_ABORT_TIMEOUT);
        self.client(addr)
            .map_err(unreachable)?
            .migrate_incoming_abort(req)
            .await
            .map_err(unreachable)?
            .into_inner()
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
fn unreachable(s: Status) -> IncomingError {
    IncomingError::Unavailable(s.message().to_string())
}
```

`crates/opl-peer/src/lib.rs`에서

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계다.

mod client;
mod codec;
pub mod monitor;
```

을 다음으로 바꾼다:

```rust
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기다.

mod client;
mod codec;
pub mod migration;
pub mod monitor;
```

`crates/opl-peer/src/lib.rs`에서

```rust
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use monitor::{NodeMonitor, NodeMonitorConfig};
```

을 다음으로 바꾼다:

```rust
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use migration::{
    IncomingError, IncomingMigration, MigrateIncomingRequest, MigrateIncomingResponse,
    MigrationReceiver,
};
pub use monitor::{NodeMonitor, NodeMonitorConfig};
```

`crates/opl-peer/src/migration.rs`를 만든다:

```rust
//! 라이브 마이그레이션의 노드 사이 RPC: 원본 노드가 대상 노드에 받을 준비(`MigrateIncoming`), 받기
//! 확인(`MigrateIncomingWait`), 취소(`MigrateIncomingAbort`)를 부른다. 받는 쪽 처리는 opld가
//! [`MigrationReceiver`]로 넣는다.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 받을 준비 요청
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateIncomingRequest {
    pub vm: Uuid,
    /// 원본 노드에서 도는 `VmMigrate` Task
    pub task: Uuid,
    /// 원본 QEMU의 버전(`major.minor.micro`). 대상 노드의 QEMU와 같아야 한다.
    pub qemu_version: String,
}

/// 받을 준비가 끝났다: 받는 QEMU가 연 포트
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrateIncomingResponse {
    pub port: u16,
}

/// 받기 확인과 취소 요청: 어느 VM의 어느 마이그레이션인지
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingMigration {
    pub vm: Uuid,
    pub task: Uuid,
}

/// 받는 쪽이 거절했거나 실패한 이유. 응답 본문에 담아 보낸다(gRPC 상태는 전송 문제만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum IncomingError {
    /// 두 노드의 QEMU 버전이 다르다(설계의 `migration_incompatible`).
    #[error("{0}")]
    Incompatible(String),
    /// 요청이 지금 상태와 맞지 않는다(VM이 이 노드로 오는 중이 아니다, 이미 확정됐다 등).
    #[error("{0}")]
    Rejected(String),
    /// 받는 노드에서 디스크나 QEMU가 실패했다.
    #[error("{0}")]
    Failed(String),
    /// 받는 노드에 닿지 못했거나 그 노드가 마이그레이션을 받지 않는다.
    #[error("cannot reach the target node: {0}")]
    Unavailable(String),
}

/// 대상 노드에서 마이그레이션을 받는 처리
#[async_trait]
pub trait MigrationReceiver: Send + Sync {
    /// VM을 받을 QEMU를 띄우고 받기 시작한다. 연 포트를 돌려준다.
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError>;

    /// 받는 QEMU가 받기를 마칠 때까지 기다린다.
    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError>;

    /// 확정 전의 마이그레이션을 버린다: 받는 QEMU를 끄고 켠 디스크를 끈다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError>;
}
```

`crates/opl-peer/src/pki.rs`에서

```rust

/// 노드 인증서(DER)의 CN에 담긴 노드 id
```

을 다음으로 바꾼다:

```rust

/// 인증서(PEM)의 키 용도가 keyEncipherment를 허용하는지(키 용도 확장이 없으면 허용). 계획 20 전에
/// 서명한 노드 인증서에는 없다. QEMU 8.2(GnuTLS)의 마이그레이션 TLS는 그것을 요구한다.
pub fn allows_key_encipherment(cert_pem: &str) -> Result<bool, PkiError> {
    let der = first_cert(cert_pem)?;
    let (_, cert) =
        x509_parser::parse_x509_certificate(&der).map_err(|e| PkiError::Pem(e.to_string()))?;
    match cert.key_usage() {
        Ok(Some(usage)) => Ok(usage.value.key_encipherment()),
        Ok(None) => Ok(true),
        Err(e) => Err(PkiError::Pem(e.to_string())),
    }
}

/// 노드 인증서(DER)의 CN에 담긴 노드 id
```

`crates/opl-peer/src/pki.rs`에서

```rust
    #[test]
    fn same_csr_signed_twice_gets_different_serials() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn key_encipherment_of_old_and_new_node_certs() {
        let ca = ClusterCa::generate("prod").unwrap();
        let tls = node_tls(&ca, Uuid::now_v7());
        assert!(allows_key_encipherment(&tls.cert_pem).unwrap());
        // 계획 20 전의 노드 인증서: digitalSignature만
        let key = KeyPair::generate().unwrap();
        let mut old = CertificateParams::new(vec!["10.0.0.5".to_string()]).unwrap();
        old.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let pem = old.self_signed(&key).unwrap().pem();
        assert!(!allows_key_encipherment(&pem).unwrap());
        // 키 용도 확장이 없으면 막지 않는다.
        let any = CertificateParams::new(vec!["10.0.0.5".to_string()]).unwrap();
        let pem = any.self_signed(&key).unwrap().pem();
        assert!(allows_key_encipherment(&pem).unwrap());
        assert!(allows_key_encipherment("not a cert").is_err());
    }

    #[test]
    fn same_csr_signed_twice_gets_different_serials() {
```

`crates/opl-peer/src/server.rs`에서

```rust

use crate::pki::{NodeTls, node_id_of};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, InstallSnapshotReply, InstallSnapshotRequest,
    LeaderReply, VoteReply, VoteRequest,
};
```

을 다음으로 바꾼다:

```rust

use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use crate::pki::{NodeTls, node_id_of};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, IncomingReply, InstallSnapshotReply,
    InstallSnapshotRequest, LeaderReply, MigrateIncomingReply, VoteReply, VoteRequest,
};
```

`crates/opl-peer/src/server.rs`에서

```rust
    hv: Option<Arc<dyn Hypervisor>>,
}
```

을 다음으로 바꾼다:

```rust
    hv: Option<Arc<dyn Hypervisor>>,
    migrations: Option<Arc<dyn MigrationReceiver>>,
}
```

`crates/opl-peer/src/server.rs`에서

```rust
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self { cluster, hv }
    }
```

을 다음으로 바꾼다:

```rust
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self {
            cluster,
            hv,
            migrations: None,
        }
    }

    /// 라이브 마이그레이션을 `receiver`로 받는다. 없으면 받기 요청을 거부한다.
    pub fn with_migrations(mut self, receiver: Arc<dyn MigrationReceiver>) -> Self {
        self.migrations = Some(receiver);
        self
    }

    fn migrations(&self) -> Result<&dyn MigrationReceiver, Status> {
        self.migrations
            .as_deref()
            .ok_or_else(|| Status::unimplemented("this node does not receive migrations"))
    }
```

`crates/opl-peer/src/server.rs`에서

```rust
        Ok(Response::new(self.cluster.id()))
    }
```

을 다음으로 바꾼다:

```rust
        Ok(Response::new(self.cluster.id()))
    }

    async fn migrate_incoming(
        &self,
        req: Request<MigrateIncomingRequest>,
    ) -> Result<Response<MigrateIncomingReply>, Status> {
        self.authorize(&req)?;
        let reply = self.migrations()?.incoming(req.into_inner()).await;
        Ok(Response::new(reply.map(|port| {
            crate::migration::MigrateIncomingResponse { port }
        })))
    }

    async fn migrate_incoming_wait(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        Ok(Response::new(
            self.migrations()?.wait(req.into_inner()).await,
        ))
    }

    async fn migrate_incoming_abort(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        Ok(Response::new(
            self.migrations()?.abort(req.into_inner()).await,
        ))
    }
```

`crates/opl-peer/src/wire.rs`에서

```rust
use opl_cluster::types::{NodeId, TypeConfig};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::{NodeId, TypeConfig};

use crate::migration::{IncomingError, MigrateIncomingResponse};
```

`crates/opl-peer/src/wire.rs`에서

```rust
pub type Empty = ();
```

을 다음으로 바꾼다:

```rust
pub type Empty = ();
/// 받을 준비의 결과. 받는 쪽의 거절·실패도 본문에 담는다.
pub type MigrateIncomingReply = Result<MigrateIncomingResponse, IncomingError>;
/// 받기 확인·취소의 결과
pub type IncomingReply = Result<(), IncomingError>;
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_types::node::{NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    GrpcNetwork, IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver,
    NodeMonitor, NodeMonitorConfig, PeerService, Peers,
};
use opl_types::node::{NodeSpec, NodeStatus};
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = NodeId(Uuid::now_v7());
```

을 다음으로 바꾼다:

```rust
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받는다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        let hv = Arc::new(MockHypervisor::default());
        let service = PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
        let (stop, rx) = oneshot::channel::<()>();
```

을 다음으로 바꾼다:

```rust
        let hv = Arc::new(MockHypervisor::default());
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>));
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        let (stop, rx) = oneshot::channel::<()>();
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
        self.cluster.shutdown().await.unwrap();
    }
```

을 다음으로 바꾼다:

```rust
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
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {}",
            req.vm, req.task, req.qemu_version
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        Ok(60007)
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
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    }
    for m in monitors {
        m.shutdown().await;
    }
}
```

을 다음으로 바꾼다:

```rust
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone())).await;
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
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        60007
    );
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
            format!("incoming {vm} {task} 8.2.2"),
            format!("incoming {vm} {task} 9.0.0"),
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
    assert_eq!(receiver.calls.lock().unwrap().len(), 4);
}
```

`crates/opld/src/config.rs`에서

```rust

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
```

을 다음으로 바꾼다:

```rust

    /// 마이그레이션을 받는 QEMU가 들을 호스트: 노드 간 통신을 받는 IP(`cluster_listen`, 기본
    /// `0.0.0.0`). `address`는 원본이 접속하는 이름일 뿐 이 기계의 주소로 풀리지 않을 수 있다(예:
    /// `/etc/hosts`의 `127.0.1.1`).
    pub fn migration_listen_host(&self) -> String {
        self.cluster_listen.ip().to_string()
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
```

`crates/opld/src/config.rs`에서

```rust
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }
```

을 다음으로 바꾼다:

```rust
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn migration_listens_on_the_cluster_listen_ip() {
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(c.migration_listen_host(), "0.0.0.0");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"10.0.0.5:8007\"");
        assert_eq!(c.migration_listen_host(), "10.0.0.5");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"[::]:8007\"");
        assert_eq!(c.migration_listen_host(), "::");
    }
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, ClusterJanitor, SnapshotScheduler, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
```

을 다음으로 바꾼다:

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, ClusterJanitor, IncomingConfig, IncomingMigrations, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig,
};
use opl_cluster::types::{LeaderRequest, NodeId};
```

`crates/opld/src/member.rs`에서

```rust
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage,
};
```

을 다음으로 바꾼다:

```rust
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
```

`crates/opld/src/member.rs`에서

```rust
        };
        let (exited_tx, exited) = mpsc::channel(2);
        let (stop, rx) = oneshot::channel::<()>();
        let service = PeerService::new(cluster.clone(), Some(hv.clone()));
        let peer_tls = tls.clone();
```

을 다음으로 바꾼다:

```rust
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
        let (exited_tx, exited) = mpsc::channel(2);
        let (stop, rx) = oneshot::channel::<()>();
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
            },
        );
        let service =
            PeerService::new(cluster.clone(), Some(hv.clone())).with_migrations(Arc::new(incoming));
        let peer_tls = tls.clone();
```

`crates/opld/src/member.rs`에서

```rust
        };
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
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
```

을 다음으로 바꾼다:

```rust
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
```

`crates/opld/tests/smoke.rs`에서

```rust
    assert_eq!(mode & 0o777, 0o600);
    // 저장소에 CA 개인 키가 있으므로 데이터 디렉터리도 주인만 들어간다. 제어 소켓은 주인만 쓴다.
```

을 다음으로 바꾼다:

```rust
    assert_eq!(mode & 0o777, 0o600);
    // 라이브 마이그레이션의 QEMU TLS 파일: 노드 인증서·키와 클러스터 CA, 키는 주인만 읽는다.
    let tls = d.data_dir().join("migration-tls");
    let read = |path: std::path::PathBuf| std::fs::read_to_string(path).unwrap();
    assert_eq!(
        read(tls.join("ca-cert.pem")),
        read(d.data_dir().join("pki/ca.crt"))
    );
    for (name, pki, want) in [
        ("server-cert.pem", "node.crt", 0o644),
        ("client-cert.pem", "node.crt", 0o644),
        ("server-key.pem", "node.key", 0o600),
        ("client-key.pem", "node.key", 0o600),
    ] {
        assert_eq!(
            read(tls.join(name)),
            read(d.data_dir().join("pki").join(pki))
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(tls.join(name)).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, want, "{name}");
    }
    // 저장소에 CA 개인 키가 있으므로 데이터 디렉터리도 주인만 들어간다. 제어 소켓은 주인만 쓴다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 691개, 실패 0

Run: `cargo test -p opl-peer --test grpc`
Expected: PASS

Run: `cargo test -p opl-api --test incoming`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(peer): 라이브 마이그레이션 노드 사이 RPC와 받는 쪽"
```

---

### Task 4: 원본 Task와 reconciler 보류·넘겨받기·정리

Task 실행기의 `VmMigrate`(`tasks/migrate.rs`, VM 큐, 일반 시간 제한 밖): 워커 보류(`ReconcilerHandle::begin_migration`/`end_migration`) → `Migrating` 기록 → 대상 준비 RPC → 보내기 → 받기 확인 → revision 검사로 확정(`spec.node = T`, `status { Running, T }`) → 원본 `postmigrate` QEMU 끄기·디스크 끄기. 확정 전 실패는 대상 Abort, `migration` 지우기(같은 revision 검사), 원본 재개. 확정 쓰기가 모호하면 60초 다시 시도한다. 대상 워커는 `spec.node`가 자기가 된 뒤 받는 중 표시가 있는 `paused` QEMU를 `finish_incoming`. 재시작한 워커는 `Migrating` VM을 되돌린다. 시작 때와 `tag_sweep`마다 확정된 원본·남은 받는 QEMU를 끄고, 남은 활성화 정리는 이 노드로 오는 중인 VM의 디스크를 쓰는 중으로 본다. in-process 2노드 시험(실제 gRPC mTLS, MockHypervisor).

**Files:**
- Create: `crates/opl-api/src/tasks/migrate.rs`
- Create: `crates/opl-reconciler/tests/migration.rs`
- Modify: `crates/opl-api/src/incoming.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/tests/incoming.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/tasks.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-reconciler/src/reconciler.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/incoming.rs`에서

```rust
    }

    /// 받으려고 켠 디스크 볼륨을 끈다. 끄지 못하면 경고만 남긴다(남은 볼륨은 Task 실행기의 정리가 끈다).
    async fn deactivate_disks(&self, vm: &Vm) {
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
```

을 다음으로 바꾼다:

```rust
    }
}

/// VM 디스크의 볼륨을 이 노드에서 끈다(받으려고 켰거나, 마이그레이션을 보낸 원본·남은 받는 QEMU의
/// 디스크). 끄지 못하면 경고만 남긴다(남은 볼륨은 Task 실행기의 정리가 끈다).
pub(crate) async fn deactivate_disks(cluster: &Cluster, storage: &dyn Storage, vm: &Vm) {
    for disk in &vm.spec.disks {
        let found = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .and_then(|volume| {
                let Some(volume) = volume else {
                    return Ok(None);
                };
                let pool = cluster.get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                Ok(pool.map(|pool| (volume, pool)))
            });
        let (volume, pool) = match found {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                continue;
            }
        };
        if let Err(e) = storage
            .deactivate_volume(&pool, volume.id, volume.spec.format)
            .await
        {
            tracing::warn!(vm = %vm.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
        }
```

`crates/opl-api/src/incoming.rs`에서

```rust
            Err(e) => {
                self.deactivate_disks(&vm).await;
                Err(IncomingError::Failed(e.to_string()))
```

을 다음으로 바꾼다:

```rust
            Err(e) => {
                deactivate_disks(&self.cluster, &*self.config.storage, &vm).await;
                Err(IncomingError::Failed(e.to_string()))
```

`crates/opl-api/src/incoming.rs`에서

```rust
        if let Some(vm) = &vm {
            self.deactivate_disks(vm).await;
        }
```

을 다음으로 바꾼다:

```rust
        if let Some(vm) = &vm {
            deactivate_disks(&self.cluster, &*self.config.storage, vm).await;
        }
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod clone;
mod migrate;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::Peers;
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared, pool_listed_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vms::is_stopped;
use crate::volumes::{block_pool_kind, usable_node};

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

#[derive(Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로). 확정되지 않았거나 확정 뒤 남은 마이그레이션 QEMU도 이 간격으로 정리한다.
    pub tag_sweep: Duration,
    /// 라이브 마이그레이션을 보낼 때 대상 노드를 부르는 노드 간 클라이언트. 없으면 마이그레이션 Task는
    /// 실패한다.
    pub peers: Option<Arc<Peers>>,
    /// 마이그레이션을 보내는 대역폭 상한(바이트/초). `None`이면 QEMU 기본값
    pub migration_bandwidth: Option<u64>,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다. 마이그레이션 Task는 `timeout` 대신 이것을 쓴다.
    pub migration_timeout: Duration,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
            peers: None,
            migration_bandwidth: None,
            migration_timeout: Duration::from_secs(30 * 60),
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
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
    /// 실행기가 멈췄는지. 뒤에서 도는 일(남은 태그 지우기)이 보고 함께 멈춘다.
    stopped: watch::Sender<bool>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
            stopped: watch::channel(false).0,
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    let tags = tokio::spawn(sweep_block_tags_every(ctx.clone()));
    let idle = tokio::spawn(deactivate_idle_volumes_every(ctx.clone()));
    let migrations = tokio::spawn(migrate::sweep_every(ctx.clone()));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
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
    retry.abort();
    tags.abort();
    idle.abort();
    migrations.abort();
    ctx.stopped.send_replace(true);
}

/// 공유 풀에서 남은 태그로 보고 지우려면 스냅샷이 만들어진 뒤 지나야 하는 시간. 다른 노드의 스냅샷
/// Task가 막 찍은 태그를 그 리소스보다 먼저 보고 지우지 않게 한다.
const SHARED_TAG_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// 남은 블록 스냅샷 태그를 시작할 때와 그 뒤 `tag_sweep`마다 지운다([`sweep_block_tags`]). 실행기가
/// 멈추면 함께 멈춘다.
async fn sweep_block_tags_every(ctx: Arc<Ctx>) {
    loop {
        sweep_block_tags(ctx.clone()).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS, RBD) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. opLite 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
///
/// 노드 로컬 풀의 볼륨은 그 볼륨을 만든 노드가 맡는다. 공유 풀(RBD)의 볼륨은 어느 노드에서나
/// 보이므로 노드마다 같은 답이 나오게 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가
/// 맡는다(그런 노드가 없으면 만든 노드). 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로
/// 만든 지 [`SHARED_TAG_MIN_AGE`]가 지난 태그만 지운다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready {
            continue;
        }
        let Ok(Some(pool)) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        else {
            continue;
        };
        if !uses_block_snapshots(&pool) {
            continue;
        }
        let shared = is_shared(&pool);
        let owner = if shared {
            usable_node(&ctx.cluster, &pool, None)
                .ok()
                .flatten()
                .or(volume.status.node)
        } else {
            volume.status.node
        };
        if owner.is_some_and(|n| n != ctx.config.node) {
            continue;
        }
        let format = volume.spec.format;
        let tags = if shared {
            ctx.storage
                .snapshot_tags_older_than(&pool, volume.id, format, SHARED_TAG_MIN_AGE)
                .await
        } else {
            ctx.storage.snapshot_tags(&pool, volume.id, format).await
        };
        let tags = match tags {
            Ok(tags) => tags,
            Err(e) => {
                tracing::debug!(volume = %volume.name, error = %e, "cannot read block snapshots");
                continue;
            }
        };
        for tag in tags {
            let Ok(id) = tag.parse::<Uuid>() else {
                continue;
            };
            if !matches!(ctx.cluster.get::<SnapshotSpec>(id), Ok(None)) {
                continue;
            }
            tracing::warn!(volume = %volume.name, tag, "removing a block snapshot left without its snapshot");
            if let Err(e) = ctx
                .storage
                .snapshot_delete(&pool, volume.id, format, &tag)
                .await
            {
                tracing::warn!(volume = %volume.name, tag, error = %e, "cannot remove block snapshot");
            }
        }
    }
}

/// 이 노드에서 켜진 채 남은 볼륨을 시작할 때와 그 뒤 `tag_sweep`마다 끈다
/// ([`deactivate_idle_volumes`]). 실행기가 멈추면 함께 멈춘다.
async fn deactivate_idle_volumes_every(ctx: Arc<Ctx>) {
    loop {
        deactivate_idle_volumes(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드에서 켜진 채 남은 볼륨(공유 LVM의 배타 활성화)을 끈다. VM이 멈춘 뒤 reconciler가 끄지
/// 못했거나 opld가 그 전에 내려갔을 때 남는다. 이 노드에 놓여 실행 중이거나 켜려는 VM(하이퍼바이저에서
/// 도는 VM 포함)의 디스크, 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직
/// 만드는 중인(pending) 볼륨은 그대로 둔다. 리소스가 없는 볼륨은 끈다.
///
/// 켜진 볼륨을 먼저 읽고 그 뒤에 VM·Task를 읽는다: 볼륨을 켜는 VM·Task는 켜기 전에 이미 저장소에
/// 있으므로 쓰는 중인 볼륨을 끄지 않는다(그 사이 켠 볼륨은 이번에 보지 않는다). 이미 켜져 있던 볼륨을
/// 그 사이 시작한 VM이 그대로 쓸 수도 있으므로 VM·Task는 볼륨마다 끄기 직전에 다시 읽는다.
async fn deactivate_idle_volumes(ctx: &Ctx) {
    let me = ctx.config.node;
    let pools = match ctx.cluster.list::<StoragePoolSpec>() {
        Ok((_, pools)) => pools,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list storage pools");
            return;
        }
    };
    let mut active = Vec::new();
    for pool in pools.into_iter().filter(|p| pool_listed_on(p, me)) {
        match ctx.storage.active_volumes(&pool).await {
            Ok(ids) if !ids.is_empty() => active.push((pool, ids)),
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(pool = %pool.name, error = %e, "cannot list active volumes")
            }
        }
    }
    for (pool, ids) in active {
        for id in ids {
            let (in_use, formats) = match volumes_in_use(ctx).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read which volumes are in use");
                    return;
                }
            };
            if in_use.contains(&id) {
                continue;
            }
            let format = formats.get(&id).copied().unwrap_or(ImageFormat::Raw);
            tracing::info!(pool = %pool.name, volume = %id, "deactivating a volume left active");
            if let Err(e) = ctx.storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(pool = %pool.name, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
}

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식. 이 노드로
/// 마이그레이션하는 중인 VM의 디스크도 쓰는 중이다(받는 QEMU를 띄우기 전에 켠다). 이 노드에서 보내는
/// 중인 VM은 원하는 상태가 running이고 마이그레이션 Task가 있다.
async fn volumes_in_use(ctx: &Ctx) -> Result<(HashSet<Uuid>, HashMap<Uuid, ImageFormat>), String> {
    let me = ctx.config.node;
    let running = ctx
        .hv
        .list()
        .await
        .map_err(|e| format!("cannot list running VMs: {e}"))?;
    let (volumes, tasks, vms) = ctx
        .cluster
        .list::<VolumeSpec>()
        .and_then(|(_, volumes)| {
            Ok((
                volumes,
                ctx.cluster.list::<TaskSpec>()?.1,
                ctx.cluster.list::<VmSpec>()?.1,
            ))
        })
        .map_err(|e| format!("cannot list volumes, tasks or VMs: {e}"))?;
    let mut in_use: HashSet<Uuid> = volumes
        .iter()
        .filter(|v| v.status.phase != VolumePhase::Ready)
        .map(|v| v.id)
        .collect();
    let mut task_vms = HashSet::new();
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == me && !t.status.state.is_finished())
    {
        in_use.insert(task.spec.target);
        task_vms.insert(task.spec.target);
    }
    let by_name: HashMap<&str, Uuid> = volumes.iter().map(|v| (v.name.as_str(), v.id)).collect();
    for vm in &vms {
        let used = task_vms.contains(&vm.id)
            || running.iter().any(|r| r.id == vm.id)
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running)
            || vm.status.migration.is_some_and(|m| m.node == me);
        if used {
            in_use.extend(
                vm.spec
                    .disks
                    .iter()
                    .filter_map(|d| by_name.get(d.volume.as_str())),
            );
        }
    }
    let formats = volumes.iter().map(|v| (v.id, v.spec.format)).collect();
    Ok((in_use, formats))
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// 마이그레이션 Task는 VM의 줄에 선다(앞의 스냅샷·복제가 끝난 뒤 옮긴다).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. } | TaskOp::VmMigrate { .. }
        );
        return (is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op) || vm_queue)
            .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| uses(&vm.spec))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
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
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    let migration = matches!(task.spec.op, TaskOp::VmMigrate { .. });
    let (result, unfinished) = if migration {
        (perform(ctx, &task).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        }
    };
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e, unfinished).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다. `unfinished`면 작업이 끝나지 않았다(시간 초과, 재시작으로 중단).
async fn fail(ctx: &Ctx, task: &Task, error: String, unfinished: bool) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]).
async fn cleanup(ctx: &Ctx, task: &Task, unfinished: bool) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op {
        if unfinished {
            snapshot::remove_unfinished_tags(ctx, snapshot);
        }
        if let Err(e) = snapshot::discard_pending(ctx, snapshot).await {
            tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
        }
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let TaskOp::VmClone { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
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
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
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
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
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

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
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

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
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
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
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
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    match user {
        // 꺼진 VM의 경로(저장소로 늘린다)는 원하는 상태도 꺼짐일 때만 탄다. 켜는 중에 늘리면 그동안
        // 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
        Some((vm, _)) if vm.status.phase != VmPhase::Running && !is_stopped(vm) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. 블록 볼륨은
        // QEMU가 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 볼륨의 실제 크기(LVM은 extent,
        // ZFS는 1 MiB 단위로 올린다)를 알린다. 그래야 게스트가 보는 크기가 아래에서 기록하는
        // 크기와 같다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            let mut size_bytes = size_bytes;
            if block_pool_kind(&pool).is_some() {
                ctx.storage
                    .resize_volume(&pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                size_bytes = ctx
                    .storage
                    .volume_info(&pool, id, volume.spec.format)
                    .await
                    .map_err(|e| format!("cannot read the new size: {e}"))?
                    .virtual_bytes;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => ctx
            .storage
            .resize_volume(&pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
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
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
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
    unmounted
}
```

`crates/opl-api/src/tasks/migrate.rs`를 만든다:

```rust
//! 라이브 마이그레이션의 원본 쪽 Task(설계 §2의 2~8단계와 실패 되돌리기)와 남은 마이그레이션 QEMU
//! 정리(설계 §2 "중간에 죽으면").
//!
//! 두 QEMU가 함께 도는 일은 없다: 대상 QEMU는 멈춘 채 받고 확정(`spec.node`를 대상으로 쓰기) 뒤에만
//! 대상 노드의 reconciler가 이어서 실행한다. 원본은 확정되지 않았다고 확인한 뒤에만 이어서 실행한다.
//! 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용된다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::ClusterError;
use opl_cluster::types::{NodeId, Rejection};
use opl_driver::{MigrateOptions, MigrateTarget, RuntimeStatus};
use opl_peer::{IncomingMigration, MigrateIncomingRequest, Peers};
use opl_reconciler::ReconcilerHandle;
use opl_types::task::Task;
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::incoming::deactivate_disks;

/// 확정 쓰기가 (과반수가 없는 등으로) 실패할 때 다시 시도하는 시간. 지나면 되돌린다.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 확정 뒤 status(`Running`, 대상 노드)를 쓰다 실패할 때 다시 시도하는 시간. 지나면 대상 노드의
/// reconciler가 넘겨받으며 마이그레이션 표시를 지우므로 그대로 둔다.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

/// 실행 중인 VM을 `target` 노드로 옮긴다. 확정 전에 실패하면 원본을 계속 돌리고 실패한다.
pub(super) async fn migrate(ctx: &Ctx, task: &Task, target: Uuid) -> Result<(), String> {
    let id = task.spec.target;
    let peers = ctx
        .config
        .peers
        .clone()
        .ok_or("this node cannot reach other nodes")?;
    // 대상 노드는 클러스터 주소의 호스트(opld `address`)에서 받는다. 같은 이름이 노드 인증서에 있다.
    let addr = ctx
        .cluster
        .members()
        .addr(NodeId(target))
        .map(str::to_string)
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let host = addr_host(&addr)
        .ok_or_else(|| format!("cannot tell the host of node address {addr:?}"))?
        .to_string();
    let migration = MigrationStatus {
        node: target,
        task: task.id,
    };
    let _hold = Hold::begin(&ctx.reconciler, id).await;
    match mark_migrating(ctx, id, migration).await {
        Ok(()) => {}
        Err(Mark::Refused(e)) => return Err(e),
        // 기록됐을 수도 있다.
        Err(Mark::Unknown(e)) => {
            roll_back(ctx, &peers, &addr, id, migration).await;
            return Err(e);
        }
    }
    let sent = send(ctx, &peers, &addr, &host, id, migration).await;
    let error = match sent {
        Ok(()) => commit(ctx, id, migration).await.err(),
        Err(e) => Some(e),
    };
    if let Some(error) = error {
        match roll_back(ctx, &peers, &addr, id, migration).await {
            Outcome::RolledBack(None) => return Err(error),
            Outcome::RolledBack(Some(resume)) => {
                return Err(format!(
                    "{error}; the source VM could not be resumed: {resume}"
                ));
            }
            // 실패로 본 확정 쓰기가 적용됐다.
            Outcome::Committed => {}
        }
    }
    finish(ctx, id, migration).await;
    Ok(())
}

/// `begin_migration`을 끝에(실패, 버려진 future 포함) 푼다. 버려져서 풀리면 작업자가 opld 재시작
/// 때처럼 남은 마이그레이션을 되돌린다.
struct Hold {
    reconciler: ReconcilerHandle,
    id: Uuid,
}

impl Hold {
    async fn begin(reconciler: &ReconcilerHandle, id: Uuid) -> Self {
        reconciler.begin_migration(id).await;
        Self {
            reconciler: reconciler.clone(),
            id,
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.reconciler.end_migration(self.id);
    }
}

/// `host:port`(IPv6는 `[addr]:port`)의 호스트
fn addr_host(addr: &str) -> Option<&str> {
    let (host, port) = addr.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

enum Mark {
    /// 기록하지 않았다.
    Refused(String),
    /// 기록됐는지 모른다.
    Unknown(String),
}

/// 2단계: 이 노드에서 실행 중인 VM인지 다시 보고 `Migrating`과 대상을 기록한다.
async fn mark_migrating(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<(), Mark> {
    let refused = |e: String| Mark::Refused(e);
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| refused(e.to_string()))?
            .ok_or_else(|| refused("VM was deleted".into()))?;
        if vm.spec.node != Some(ctx.config.node) {
            return Err(refused(format!(
                "VM {:?} is no longer on this node",
                vm.name
            )));
        }
        let running = vm.spec.power == PowerState::Running && vm.status.phase == VmPhase::Running;
        let state = ctx
            .hv
            .inspect(id)
            .await
            .map_err(|e| refused(e.to_string()))?;
        if !running || state.is_none_or(|s| s.status != RuntimeStatus::Running) {
            return Err(refused(format!("VM {:?} is not running", vm.name)));
        }
        let mut status = vm.status.clone();
        status.phase = VmPhase::Migrating;
        status.migration = Some(migration);
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(Mark::Unknown(format!("cannot record the migration: {e}"))),
        }
    }
}

/// 3~5단계: 대상 노드에 받을 준비를 시키고, 보내고, 대상이 다 받았는지 확인한다.
async fn send(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    host: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Result<(), String> {
    let qemu_version = ctx
        .hv
        .running_qemu_version(id)
        .await
        .map_err(|e| format!("cannot read the QEMU version: {e}"))?;
    let req = MigrateIncomingRequest {
        vm: id,
        task: migration.task,
        qemu_version,
    };
    let port = peers
        .migrate_incoming(addr, req)
        .await
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    let target = MigrateTarget {
        host: host.to_string(),
        port,
        tls_hostname: host.to_string(),
    };
    let opts = MigrateOptions {
        bandwidth: ctx.config.migration_bandwidth,
        timeout: ctx.config.migration_timeout,
    };
    ctx.hv
        .migrate(id, &target, &opts)
        .await
        .map_err(|e| e.to_string())?;
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    peers
        .migrate_incoming_wait(addr, incoming)
        .await
        .map_err(|e| format!("the target node did not receive the VM: {e}"))
}

/// 6단계: `spec.node`를 대상 노드로 쓴다(revision 확인). 이 쓰기가 적용되면 확정이다. 결과를 모르는
/// 오류는 `COMMIT_TIMEOUT` 동안 다시 시도한다(같은 revision 확인이므로 두 번 적용되지 않는다).
async fn commit(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<(), String> {
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return Err("VM was deleted".into()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if vm.spec.node == Some(migration.node) {
            return Ok(());
        }
        if vm.spec.node != Some(ctx.config.node) || vm.status.migration != Some(migration) {
            return Err(format!("VM {:?} changed during the migration", vm.name));
        }
        let mut spec = vm.spec.clone();
        spec.node = Some(migration.node);
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e @ ClusterError::Rejected(_)) => return Err(e.to_string()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => return Err(format!("cannot record the migration: {e}")),
        }
    }
}

enum Outcome {
    /// 확정되지 않았다. 원본을 이어서 실행하지 못했으면 그 이유
    RolledBack(Option<String>),
    /// 확정 쓰기가 적용돼 있었다.
    Committed,
}

/// 확정 전 실패: 마이그레이션 표시를 지워(읽은 revision 확인) 확정을 막은 뒤 대상의 받는 QEMU를
/// 끄게 하고, 원본이 멈춰 있으면(`postmigrate`, `paused`) 이어서 실행한다. 지우기 전에 확정이
/// 적용됐으면 `Committed`다. 결과를 알 수 없으면(과반수 없음) 알 때까지 다시 시도한다. 그동안 원본은
/// 멈춰 있을 수 있지만 두 QEMU가 함께 돌지는 않는다.
async fn roll_back(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Outcome {
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        if vm.spec.node == Some(migration.node) {
            return Outcome::Committed;
        }
        if vm.status.migration != Some(migration) {
            break;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot roll back the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    // 닿지 않으면 대상 노드의 정리가 끈다(마이그레이션 표시가 없다).
    if let Err(e) = peers.migrate_incoming_abort(addr, incoming).await {
        tracing::warn!(vm = %id, error = %e, "cannot abort the incoming migration on the target node");
    }
    let resumed = match ctx.hv.inspect(id).await {
        Ok(Some(state))
            if state.status == postmigrate() || state.status == RuntimeStatus::Paused =>
        {
            tracing::info!(vm = %id, "resuming the source of a failed migration");
            ctx.hv.resume(id).await.map_err(|e| e.to_string())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    Outcome::RolledBack(resumed.err())
}

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
/// 남긴다(남은 원본은 정리가 끈다).
async fn finish(ctx: &Ctx, id: Uuid, migration: MigrationStatus) {
    let deadline = Instant::now() + RECORD_TIMEOUT;
    let vm = loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                return;
            }
        };
        // 대상 노드의 reconciler가 이미 넘겨받아 표시를 지웠다.
        if vm.status.migration != Some(migration) {
            break vm;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        if status.node != Some(migration.node) {
            status.node = Some(migration.node);
            status.pid = None;
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => break vm,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM");
                break vm;
            }
        }
    };
    stop_source(ctx, &vm).await;
}

/// 마이그레이션을 보내고 멈춘 원본 QEMU(`postmigrate`)를 끄고(탭은 드라이버가 지운다) 이 노드의
/// 디스크를 끈다. 다른 상태의 QEMU는 끄지 않고, QEMU가 남아 있으면 디스크도 그대로 둔다.
async fn stop_source(ctx: &Ctx, vm: &Vm) {
    let stopped = match ctx.hv.stop_migrated(vm.id).await {
        Ok(true) => true,
        Ok(false) => matches!(ctx.hv.inspect(vm.id).await, Ok(None)),
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
            false
        }
    };
    if stopped {
        deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
    }
}

/// 남은 마이그레이션 QEMU를 시작할 때와 그 뒤 `tag_sweep`마다 끈다([`sweep`]). 실행기가 멈추면
/// 함께 멈춘다.
pub(super) async fn sweep_every(ctx: Arc<Ctx>) {
    loop {
        sweep(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 남은 마이그레이션 QEMU를 끄고 디스크를 끈다:
/// - 마이그레이션을 보내고 멈춘(`postmigrate`) 원본인데 VM이 다른 노드로 확정됐다(원본 opld가 확정
///   뒤 정리 전에 죽었다).
/// - 받는 중 표시가 있는 QEMU인데 VM이 이 노드로 확정되지 않았고 이 노드로 오는 중도 아니다(원본이
///   되돌렸거나 확정 전에 죽은 뒤 되돌려졌다). VM이 지워졌어도 끈다.
///
/// 확정 전의 원본(`spec.node`가 이 노드)은 마이그레이션 Task나 reconciler가 다룬다.
pub(super) async fn sweep(ctx: &Ctx) {
    let me = ctx.config.node;
    match ctx.hv.list().await {
        Ok(states) => {
            for state in states.into_iter().filter(|s| s.status == postmigrate()) {
                let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(state.id) else {
                    continue;
                };
                if vm.spec.node == Some(me) {
                    continue;
                }
                tracing::info!(vm = %vm.id, "stopping the source QEMU of a migration committed elsewhere");
                stop_source(ctx, &vm).await;
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
    }
    let incoming = match ctx.hv.list_incoming().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list incoming migrations");
            return;
        }
    };
    for id in incoming {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(vm) => vm,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                continue;
            }
        };
        if let Some(vm) = &vm
            && (vm.spec.node == Some(me) || vm.status.migration.is_some_and(|m| m.node == me))
        {
            continue;
        }
        tracing::info!(vm = %id, "stopping the receiving QEMU of a migration that did not complete");
        if let Err(e) = ctx.hv.stop(id, opl_driver::StopMode::Force).await {
            tracing::warn!(vm = %id, error = %e, "cannot stop the receiving QEMU");
            continue;
        }
        if let Some(vm) = &vm {
            deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::addr_host;

    #[test]
    fn host_of_a_cluster_address() {
        assert_eq!(addr_host("10.0.0.2:7443"), Some("10.0.0.2"));
        assert_eq!(addr_host("node-b.lan:7443"), Some("node-b.lan"));
        assert_eq!(addr_host("[fd00::2]:7443"), Some("fd00::2"));
        assert_eq!(addr_host("10.0.0.2"), None);
        assert_eq!(addr_host(":7443"), None);
    }
}
```

`crates/opl-api/tests/incoming.rs`에서

```rust
//! 라이브 마이그레이션의 받는 쪽(`IncomingMigrations`): 원본 노드가 RPC로 부르는 처리를 직접 부른다.
//! 이 노드(Env의 노드)가 대상이고, 원본 노드는 저장소에만 있다.
```

을 다음으로 바꾼다:

```rust
//! 라이브 마이그레이션의 받는 쪽(`IncomingMigrations`): 원본 노드가 RPC로 부르는 처리를 직접 부른다.
//! 이 노드(Env의 노드)가 대상이고, 원본 노드는 저장소에만 있다. Task 실행기의 남은 마이그레이션
//! QEMU 정리도 본다.
```

`crates/opl-api/tests/incoming.rs`에서

```rust
use opl_driver::mock::MOCK_QEMU_VERSION;
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_driver::mock::MOCK_QEMU_VERSION;
use opl_driver::{DriverError, Hypervisor, MigrateOptions, MigrateTarget, RuntimeStatus, Storage};
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
```

`crates/opl-api/tests/incoming.rs`에서

```rust
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);
}
```

을 다음으로 바꾼다:

```rust
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_some());
    assert_eq!(s.deactivations(), 0);
}

impl Setup {
    /// 남은 마이그레이션 QEMU를 자주 정리하는 Task 실행기로 바꾼다(디스크는 `storage`로 끈다).
    async fn sweep_often(&mut self) {
        self.env.stop_runner().await;
        let storage = self.storage.clone() as Arc<dyn Storage>;
        self.env
            .start_runner_config(storage, |c| c.tag_sweep = Duration::from_millis(50))
            .await;
    }

    async fn running(&self, id: Uuid) -> Option<RuntimeStatus> {
        self.env.hv.inspect(id).await.unwrap().map(|s| s.status)
    }

    async fn wait_stopped(&self, id: Uuid) {
        let deadline = std::time::Instant::now() + common::WAIT;
        while self.running(id).await.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "QEMU of {id} was not stopped"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// 원본이 마이그레이션을 되돌렸는데(이 노드로 오는 표시가 없다) 받는 QEMU가 남았다: 정리가 끄고
/// 디스크를 끈다. 이 노드로 오는 중이면 그대로 둔다.
#[tokio::test]
async fn sweep_stops_a_receiving_qemu_left_behind() {
    let mut s = setup().await;
    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    s.sweep_often().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.deactivations(), 0);

    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.wait_stopped(s.vm).await;
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
    assert_eq!(s.deactivations(), 1);
}

/// 이 노드에서 보낸 원본(`postmigrate`)이 남았는데 VM은 다른 노드로 확정됐다: 정리가 끄고 디스크를
/// 끈다. 이 노드에 배정된 VM의 원본(확정 전)은 그대로 둔다.
#[tokio::test]
async fn sweep_stops_a_source_qemu_committed_elsewhere() {
    let mut s = setup().await;
    let admin = s.env.admin().await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    // `web`은 원본 노드에 배정돼 있다(이 노드에서 보냈고 원본 노드로 확정된 것과 같다).
    let vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
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
    s.env.hv.start(&spec, None).await.unwrap();
    s.env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    // `db`는 이 노드에 배정돼 있고 확정 전이다.
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(s.env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let db = vm_named(&s.env, "db");
    s.env.hv.migrate(db.id, &target, &opts).await.unwrap();

    s.sweep_often().await;
    s.wait_stopped(s.vm).await;
    assert_eq!(s.deactivations(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        s.running(db.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );
}
```

`crates/opl-api/tests/migrate.rs`에서

```rust
//! VM 라이브 마이그레이션 API: 요청 검사(설계 §4)와 마이그레이션 중 다른 동작 막기.
//! 실제 마이그레이션은 아직 없다. Task 실행기는 "not supported yet"으로 실패시킨다.
```

을 다음으로 바꾼다:

```rust
//! VM 라이브 마이그레이션 API: 요청 검사(설계 §4)와 마이그레이션 중 다른 동작 막기.
//! 이 노드 하나만 실제로 돈다(노드 간 클라이언트가 없다). 실제 마이그레이션은 `two_nodes.rs`가 본다.
```

`crates/opl-api/tests/migrate.rs`에서

```rust
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "VM migration is not supported yet");
```

을 다음으로 바꾼다:

```rust
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "this node cannot reach other nodes"
    );
    // 원본 VM은 그대로 돈다.
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(env.node));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.migration, None);
```

`crates/opl-api/tests/tasks.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, Vm, VmSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{DiskBus, DiskSpec, MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
```

`crates/opl-api/tests/tasks.rs`에서

```rust
        ("making", "san", false),
    ] {
```

을 다음으로 바꾼다:

```rust
        ("making", "san", false),
        ("coming", "san", true),
    ] {
```

`crates/opl-api/tests/tasks.rs`에서

```rust
    let snap = vm_with_disks(&env, "snap", PowerState::Stopped, &["snapping"]).await;
    let running = TaskState::Running;
```

을 다음으로 바꾼다:

```rust
    let snap = vm_with_disks(&env, "snap", PowerState::Stopped, &["snapping"]).await;
    // 다른 노드에서 이 노드로 마이그레이션하는 중인 VM(받는 QEMU를 띄우기 전에 디스크를 켠다)
    let source = Uuid::now_v7();
    let mut spec: VmSpec =
        serde_json::from_value(common::vm_request("moving")["spec"].clone()).unwrap();
    spec.node = Some(source);
    spec.nics.clear();
    spec.disks = vec![DiskSpec {
        volume: "coming".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    }];
    let mut moving: Vm = Resource::new("moving", spec);
    moving.status.phase = VmPhase::Migrating;
    moving.status.node = Some(source);
    moving.status.migration = Some(MigrationStatus {
        node: env.node,
        task: Uuid::now_v7(),
    });
    env.cluster.create(&moving).await.unwrap();
    env.cluster
        .update_status::<VmSpec>(moving.id, &moving.status)
        .await
        .unwrap();
    let running = TaskState::Running;
```

`crates/opl-api/tests/tasks.rs`에서

```rust
    };
    let want = kept(&["boot", "resizing", "snapping", "making"]);
    let active = || {
```

을 다음으로 바꾼다:

```rust
    };
    let want = kept(&["boot", "resizing", "snapping", "making", "coming"]);
    let active = || {
```

`crates/opl-api/tests/two_nodes.rs` 전체:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{AppState, IncomingConfig, IncomingMigrations, TaskRunner, hash_password};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    DirStorage, DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage,
};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::Reconciler;
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config, vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    base: String,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
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
        Node {
            id,
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            tls,
            listener: Some(listener),
            base: String::new(),
            _workers: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT));
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
            },
        );
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming));
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        let reconciler =
            Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config(self.id));
        let work = root.path().join(format!("{}-work", self.id));
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root).await;
    let mut b = Node::open(&ca, &root).await;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for (i, node) in [&a, &b].into_iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node{}", i + 1),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    assert_eq!(runtime_status(&a, vm.id).await, None);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}
```

`crates/opl-reconciler/src/reconciler.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Hypervisor;
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
    commands: mpsc::UnboundedSender<Command>,
    restores: Restores,
    held: Held,
}

/// VM별로 다음 시작에 불러올 스냅샷 태그. 이 노드의 메모리에만 있어 opld가 재시작하면 사라진다.
pub(crate) type Restores = Arc<Mutex<HashMap<Uuid, String>>>;

/// 이 노드의 마이그레이션 Task가 다루는 중인 VM(`ReconcilerHandle::begin_migration`). 작업자는
/// 이 VM에 손대지 않는다. 메모리에만 있으므로 opld가 재시작하면 풀린다.
pub(crate) type Held = Arc<Mutex<HashSet<Uuid>>>;

/// 작업자에게 보내는 일회성 명령
enum Command {
    ForceStop(Uuid),
    /// 그 VM의 작업자가 지금 하던 단계를 마치면 알린다.
    Barrier(Uuid, oneshot::Sender<()>),
    Sync(Uuid),
}

/// 실행 중인 reconciler에 VM별 일회성 명령을 보낸다. 명령은 그 VM의 작업자 큐를 거치므로
/// 같은 VM에 대한 드라이버 호출이 겹치지 않는다.
#[derive(Clone)]
pub struct ReconcilerHandle {
    commands: mpsc::UnboundedSender<Command>,
    restores: Restores,
    held: Held,
}

impl ReconcilerHandle {
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다. 원하는 상태를 먼저
    /// stopped로 기록한 뒤에 부른다. 이 노드에 배정되지 않은 VM이거나 원하는 상태가 running이면
    /// 아무것도 하지 않는다. 결과는 VM status로 확인한다.
    pub fn force_stop(&self, id: Uuid) {
        let _ = self.commands.send(Command::ForceStop(id));
    }

    /// 이 노드에서 VM을 다음에 새로 띄울 때 한 번만 이 스냅샷 태그를 불러와 이어서 실행한다.
    /// 원하는 상태를 running으로 기록하기 전에 부른다. 시작이 실패해도 태그는 쓴 것으로 치고,
    /// 그 시작은 다시 시도하지 않는다(VM은 failed). 이미 맡긴 태그는 바꾼다.
    pub fn restore_on_start(&self, id: Uuid, tag: String) {
        self.restores.lock().unwrap().insert(id, tag);
    }

    /// 맡긴 태그를 쓰지 않고 버린다(시작하기 전에 되돌리기가 실패했을 때).
    pub fn clear_restore(&self, id: Uuid) {
        self.restores.lock().unwrap().remove(&id);
    }

    /// 라이브 마이그레이션을 보내는 동안 작업자가 이 VM에 손대지 않게 한다(status를 덮어쓰거나, 멈춘
    /// 원본을 이어서 실행하거나, 이벤트로 다시 시작하지 않는다). 작업자가 하던 단계를 마칠 때까지
    /// 기다린 뒤 돌아오므로, 돌아온 뒤에 쓴 status를 작업자가 덮지 않는다. 반드시
    /// [`end_migration`](Self::end_migration)으로 푼다.
    pub async fn begin_migration(&self, id: Uuid) {
        self.held.lock().unwrap().insert(id);
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Barrier(id, tx)).is_ok() {
            // 작업자가 없으면 보낸 쪽이 버려져 곧바로 돌아온다.
            let _ = rx.await;
        }
    }

    /// [`begin_migration`](Self::begin_migration)을 풀고 작업자가 VM을 다시 맞추게 한다.
    pub fn end_migration(&self, id: Uuid) {
        self.held.lock().unwrap().remove(&id);
        let _ = self.commands.send(Command::Sync(id));
    }
}

impl Reconciler {
    pub fn spawn(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReconcilerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let restores = Restores::default();
        let held = Held::default();
        let dispatcher = Dispatcher {
            cluster,
            hv,
            config: Arc::new(config),
            restores: restores.clone(),
            held: held.clone(),
            workers: HashMap::new(),
            retired: HashMap::new(),
        };
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx, command_rx)),
            commands,
            restores,
            held,
        }
    }

    pub fn handle(&self) -> ReconcilerHandle {
        ReconcilerHandle {
            commands: self.commands.clone(),
            restores: self.restores.clone(),
            held: self.held.clone(),
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
    restores: Restores,
    held: Held,
    /// 이 노드에 배정된 VM의 작업자
    workers: HashMap<Uuid, WorkerHandle>,
    /// 내보냈지만 아직 끝나지 않았을 수 있는 작업자. 같은 VM이 다시 배정되면 새 작업자가 기다린다.
    retired: HashMap<Uuid, JoinHandle<()>>,
}

impl Dispatcher {
    async fn run(
        mut self,
        mut shutdown: oneshot::Receiver<()>,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
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
                    Some(command) = commands.recv() => match command {
                        Command::ForceStop(id) => {
                            if !self.notify(id, Trigger::ForceStop) {
                                tracing::warn!(vm = %id, "force stop for a VM not assigned to this node");
                            }
                        }
                        // 작업자가 없으면 보낸 쪽이 버려진다(기다릴 단계가 없다).
                        Command::Barrier(id, done) => {
                            self.notify(id, Trigger::Barrier(done));
                        }
                        Command::Sync(id) => {
                            self.notify(id, Trigger::Sync);
                        }
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
        // 경고를 남기려는 것뿐이므로 실패해도 동기화는 이어 간다.
        match self.hv.list().await {
            Ok(states) => {
                for state in states {
                    if !mine.contains(&state.id) {
                        tracing::warn!(vm = %state.id, "VM is running on this node but not assigned to it; leaving it alone");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
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
            self.restores.clone(),
            self.held.clone(),
        );
        let previous = self.retired.remove(&id);
        let task = tokio::spawn(worker.run(previous, rx));
        let _ = tx.send(Trigger::Sync);
        self.workers.insert(id, WorkerHandle { tx, task });
    }

    /// 삭제되었거나 다른 노드로 배정된 VM의 작업자를 내보낸다. 작업자는 마지막으로 한 번 확인하고 끝난다.
    fn retire(&mut self, id: Uuid) {
        // 다시 배정되었을 때 묵은 태그로 시작하지 않게 한다.
        self.restores.lock().unwrap().remove(&id);
        if let Some(w) = self.workers.remove(&id) {
            let _ = w.tx.send(Trigger::Sync);
            self.retired.insert(id, w.task);
        }
    }
}
```

`crates/opl-reconciler/src/worker.rs` 전체:

```rust
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::reconciler::{Held, Restores};
use crate::resolve::{ResolveError, running_spec, runtime_spec};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// 작업자를 깨우는 이유.
#[derive(Debug)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
    /// 앞의 단계를 마쳤음을 알린다(`ReconcilerHandle::begin_migration`).
    Barrier(tokio::sync::oneshot::Sender<()>),
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
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 마이그레이션 Task가 다루는 중인 VM. 들어 있으면 이 작업자는 아무것도 하지 않는다.
    held: Held,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
    /// 원하는 상태가 다시 running으로 바뀌면 취소하므로, 새 종료는 늘 ACPI부터 다시 시작한다.
    stop_requested: Option<Instant>,
    /// 이 작업자가 마지막으로 종료를 요청한(ACPI 또는 강제) 시각. 뒤이은 종료 이벤트가 우리
    /// 요청의 결과인지 가린다. 요청을 취소했거나 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트(또는 강제 종료)는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안 유효하다.
    stopped_by_us: Option<Instant>,
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 켜고,
    /// 그 뒤 running으로 바뀌면 reconcile이 끈다.
    force_requested: bool,
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
    /// 디스크 볼륨이 켜져 있을 수 있다(시작하려고 켰거나 VM이 도는 것을 봤다). QEMU가 끝난 것을
    /// 확인하면 끈다([`Worker::deactivate_disks`]).
    disks_active: bool,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
        restores: Restores,
        held: Held,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            restores,
            held,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            disks_active: false,
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
            // 트리거는 차례로 처리하므로 앞의 단계는 이미 끝났다.
            if let Trigger::Barrier(done) = trigger {
                let _ = done.send(());
                continue;
            }
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
        // 마이그레이션 Task가 status와 QEMU를 다룬다. 끝나면(`end_migration`) 다시 깨운다.
        if self.held.lock().unwrap().contains(&self.id) {
            return Ok(());
        }
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(mut vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다.
            if self.hv.inspect(self.id).await?.is_some() {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            return Ok(());
        };
        if vm.status.migration.is_some() || vm.status.phase == VmPhase::Migrating {
            match self.clear_migration(&vm).await? {
                Some(current) => vm = current,
                // 그 사이 바뀌었다. 다시 읽어 맞춘다.
                None => {
                    self.wake(Instant::now());
                    return Ok(());
                }
            }
        }
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync | Trigger::Barrier(_) => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&mut vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.write_status(status, vm).await?;
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
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stopped_by_us
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            // 강제 종료의 결과로 Crashed가 와도 크래시로 보지 않는다.
            _ if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 우리가 요청했는지로 먼저 가린다.
            EventKind::Stopped {
                guest_initiated: true,
            } => self.power_off = Some(None),
            // 우리가 요청하지 않았고 게스트의 결정도 아니다(호스트의 SIGTERM 등). 원하는 상태는
            // 그대로 두므로 실패로 세지 않고 다시 시작한다.
            EventKind::Stopped {
                guest_initiated: false,
            } => {}
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

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다. 그 전에 status를 먼저
    /// 기록했으면 `vm`을 다시 읽은 것으로 바꾼다.
    async fn reconcile(&mut self, vm: &mut Vm) -> Result<VmStatus, StepError> {
        let actual = self.hv.inspect(self.id).await?;
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        if actual.is_some() {
            self.disks_active = true;
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                // 진행 중인 종료 요청을 취소한다. 게스트가 이미 받은 요청대로 꺼지면
                // `stopped_by_us`로 가려 다시 시작한다.
                self.stop_requested = None;
                self.force_requested = false;
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
                if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)이나 스냅샷으로 멈춘 채 죽었거나 QMP로 다룰 수 없다.
                    // 붙어 있으면 디스크를 다시 해석하지 않고(LVM이 응답하지 않을 수 있다) 이어서
                    // 실행하고, 아니면 start()가 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나
                    // 실패로 세지 않는다.
                    let mut continued = match self.hv.resume_left_stopped(self.id).await {
                        Ok(true) => Ok(()),
                        Ok(false) => self.reattach(vm).await,
                        Err(e) => Err(e.to_string()),
                    };
                    // 이 노드로 마이그레이션을 받아 멈춰 있는 QEMU다(확정되어 이 노드에 배정됐다).
                    // 받는 중 표시가 있을 때만 이어서 실행하고 표시를 지운다.
                    if continued.is_ok() && state.status == RuntimeStatus::Paused {
                        continued = self
                            .hv
                            .finish_incoming(self.id)
                            .await
                            .map_err(|e| e.to_string());
                    }
                    if let Err(e) = continued {
                        tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                        status.last_error = Some(format!("VM is unresponsive: {e}"));
                        self.wake(now + self.config.retry_interval);
                    }
                }
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
                self.force_requested = false;
                self.deactivate_disks(vm).await;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if self.backoff.is_none() && vm.status.phase == VmPhase::Running {
                    // 저장된 phase가 Running인 채 다시 켜면(QEMU가 스스로 끝났다) 그사이 API·Task
                    // 실행기가 실행 중 VM으로 보고 디스크를 늘린다(공유 LVM은 늘린 부분을 0으로 채우기
                    // 전에 QEMU가 연다). 디스크를 해석하기 전에 Starting을 먼저 기록해, 먼저 들어온
                    // 크기 늘리기 Task는 resolve가 보고 나중 것은 API·실행기가 거절하게 한다. 기록하지
                    // 못해도(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) VM은 다시 켠다(설계 10절
                    // 불변식 4). 기록이 시간 초과로 실패한 사이 과반수가 돌아오면 좁은 틈이 남는다.
                    let mut starting = status.clone();
                    starting.phase = VmPhase::Starting;
                    starting.pid = None;
                    match self.write_status(starting, vm.clone()).await {
                        Ok(()) => match self.cluster.get::<VmSpec>(self.id)? {
                            Some(current) => *vm = current,
                            None => return Ok(status),
                        },
                        Err(e) => {
                            tracing::warn!(vm = %self.id, error = %e, "cannot record starting phase; restarting anyway");
                        }
                    }
                }
                if self.backoff.is_none() && self.start(vm).await? {
                    // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
                    // 않고 이벤트를 기다리게 한다.
                    self.was_running = true;
                    match self.hv.inspect(self.id).await? {
                        Some(state) => {
                            status.phase = VmPhase::Running;
                            status.pid = Some(state.pid);
                            status.last_error = None;
                        }
                        // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
                        None => {
                            self.was_running = false;
                            let deadline = Instant::now() + EVENT_GRACE;
                            self.awaiting_event = Some(deadline);
                            self.wake(deadline);
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
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    None => {
                        // 요청이 실패해도(QMP로 다룰 수 없는 VM) acpi_timeout 뒤 강제 종료한다.
                        self.stop_requested = Some(now);
                        self.stopped_by_us = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                        if let Err(e) = self.hv.stop(self.id, StopMode::Acpi).await {
                            tracing::warn!(vm = %self.id, error = %e, "ACPI shutdown request failed; will force after timeout");
                        }
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
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
                self.force_requested = false;
                self.awaiting_event = None;
                self.deactivate_disks(vm).await;
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
        let spec = match self.resolve(vm).await {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았거나(NFS 서버가 멈춤, 마운트 전 등) 디스크를 늘리는
            // 중이다. 끝나면 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::VolumeResizing(_))) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(false);
            }
            // 참조가 틀렸거나, 이 노드가 지원하지 않는 풀(NFS on macOS)이거나, 저장소가 볼륨을
            // 내주지 못했다. spec을 고치거나 VM을 옮겨야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
                return Ok(false);
            }
        };
        self.disks_active = true;
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        let started = self.hv.start(&spec, restore.as_deref()).await;
        if started.is_err() {
            self.deactivate_disks(vm).await;
        }
        match (started, restore) {
            (Ok(()), _) => Ok(true),
            // 태그 없이 다시 시도하면 스냅샷이 아닌 처음부터 부팅하게 되므로 일시 오류라도 멈춘다.
            (Err(e), Some(tag)) => {
                self.power_off = Some(Some(format!("failed to restore snapshot {tag}: {e}")));
                Ok(false)
            }
            (Err(e), None) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            (Err(e), None) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(false)
            }
        }
    }

    /// 켜져 있을 수 있는 디스크 볼륨을 끈다(QEMU가 끝난 뒤). 끄지 못하면 경고만 남긴다(켜진 채 남은
    /// 볼륨은 Task 실행기의 정리가 다시 끈다).
    async fn deactivate_disks(&mut self, vm: &Vm) {
        if !std::mem::take(&mut self.disks_active) {
            return;
        }
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }

    /// 끝나지 않은 채 남은 마이그레이션 표시(`Migrating`, `status.migration`)를 지운다. 이 노드의
    /// 마이그레이션 Task가 다루지 않는 VM이므로, 원본 opld가 확정 전에 죽었거나(원본 노드) 확정 뒤
    /// 넘겨받는 중이다(대상 노드). 읽은 revision으로만 쓰므로 그 사이 확정되었으면(남은 확정 쓰기가
    /// 먼저 적용됨) 쓰지 않고 `None`이다. 쓰고 나면 그 확정은 더는 적용되지 않으므로, 이 노드의 QEMU가
    /// 마이그레이션을 보내고 멈춰 있으면(`postmigrate`) 이어서 실행한다. 대상의 QEMU는 확정 뒤에만
    /// 돌므로 두 QEMU가 함께 돌지 않는다. 다시 읽은 VM을 돌려준다.
    async fn clear_migration(&self, vm: &Vm) -> Result<Option<Vm>, StepError> {
        let mut status = vm.status.clone();
        status.migration = None;
        if status.phase == VmPhase::Migrating {
            status.phase = VmPhase::Running;
        }
        match self
            .cluster
            .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => return Ok(None),
            r => r?,
        };
        if let Some(state) = self.hv.inspect(self.id).await?
            && state.status == RuntimeStatus::Other("postmigrate".into())
        {
            tracing::warn!(vm = %self.id, "resuming the source of a migration that did not complete");
            self.hv.resume(self.id).await?;
        }
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        Ok(vm.filter(|vm| vm.spec.node == Some(self.config.node)))
    }

    /// VM의 런타임 정의. 디스크 볼륨을 활성화한다.
    async fn resolve(&self, vm: &Vm) -> Result<VmRuntimeSpec, ResolveError> {
        let c = &self.config;
        runtime_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage).await
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다.
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let c = &self.config;
        let spec = running_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
    }

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            match self
                .cluster
                .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
                .await
            {
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) => vm = current,
                        None => return Ok(()),
                    }
                }
                r => return r.map(|_| ()).map_err(Into::into),
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, mut vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
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
            snapshot: vm.status.snapshot,
            schedule_error: vm.status.schedule_error.clone(),
            migration: None,
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
use opl_driver::{DirStorage, Hypervisor, MockHypervisor};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DirStorage, Hypervisor, MockHypervisor};
use opl_reconciler::{Reconciler, ReconcilerConfig, ReconcilerHandle, RestartBackoff};
use opl_types::resource::Resource;
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust

    pub fn clear_restore(&self, id: Uuid) {
```

을 다음으로 바꾼다:

```rust

    pub fn handle(&self) -> ReconcilerHandle {
        self.reconciler.as_ref().unwrap().handle()
    }

    pub fn clear_restore(&self, id: Uuid) {
```

`crates/opl-reconciler/tests/migration.rs`를 만든다:

```rust
//! 라이브 마이그레이션과 reconciler: 원본 Task가 다루는 동안의 보류, 확정 전에 남은 마이그레이션의
//! 되돌리기, 받은 VM 넘겨받기(설계 §2).

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::{Hypervisor, MigrateOptions, MigrateTarget, RuntimeStatus};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use uuid::Uuid;

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running
}

/// 원본 Task처럼 `Migrating`과 대상 노드를 기록한다.
async fn mark_migrating(env: &Env, id: Uuid, node: Uuid) {
    let mut status = env.get(id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node,
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

fn target() -> MigrateTarget {
    MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    }
}

fn opts() -> MigrateOptions {
    MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    }
}

async fn status_of(env: &Env, id: Uuid) -> Option<RuntimeStatus> {
    env.hv.inspect(id).await.unwrap().map(|s| s.status)
}

#[tokio::test]
async fn held_vm_is_left_alone_until_the_migration_ends() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    // 원본이 보낸 뒤 멈췄다. 보류 중이면 작업자가 status를 덮거나 원본을 이어서 실행하지 않는다.
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = env.get(vm.id);
    assert_eq!(got.status.phase, VmPhase::Migrating);
    assert!(got.status.migration.is_some());
    assert_eq!(
        status_of(&env, vm.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );

    // Task 없이 끝나면(Task가 되돌리지 못함) 작업자가 표시를 지우고 원본을 이어서 실행한다.
    handle.end_migration(vm.id);
    let got = env
        .wait(vm.id, "migration cleared", |v| {
            running(v) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.spec.node, Some(env.node));
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}

/// opld가 마이그레이션을 보내다 확정 전에 죽고 다시 떴다: `begin_migration`이 없으므로 작업자가
/// `Migrating`을 되돌리고 `postmigrate`로 멈춘 원본을 이어서 실행한다.
#[tokio::test]
async fn migration_left_by_a_restart_is_rolled_back() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    env.start_reconciler();

    let got = env
        .wait(vm.id, "migration rolled back", |v| {
            running(v) && v.status.migration.is_none()
        })
        .await;
    assert_eq!(got.status.pid, pid);
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}

/// 마이그레이션이 이 노드로 확정됐다: 받는 중 표시가 있는 멈춘 QEMU를 이어서 실행하고 표시를 지운다.
/// 새로 띄우지 않는다.
#[tokio::test]
async fn received_vm_is_continued_after_the_commit() {
    let env = Env::start().await;
    let source = Uuid::now_v7();
    let mut vm = vm("web", source);
    env.create(&vm).await;
    mark_migrating(&env, vm.id, env.node).await;

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
        .start_incoming(&spec, "127.0.0.1", 60000..=60099)
        .await
        .unwrap();
    env.hv.incoming_wait(vm.id).await.unwrap();
    // 확정 전에는 원본 노드의 VM이므로 손대지 않는다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Paused));
    assert!(env.hv.is_incoming(vm.id));

    // 원본이 확정한다(spec.node만 먼저 바뀌고 status는 아직 원본의 것이다).
    let current = env.get(vm.id);
    vm.spec.node = Some(env.node);
    env.cluster
        .update_spec(vm.id, Some(current.revision), current.labels, &vm.spec)
        .await
        .unwrap();
    let got = env
        .wait(vm.id, "taken over", |v| {
            running(v) && v.status.migration.is_none() && v.status.node == Some(env.node)
        })
        .await;
    assert!(got.status.pid.is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_incoming(vm.id) {
        assert!(std::time::Instant::now() < deadline, "still incoming");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.state_dir = Some(config.data_dir.clone());
        let runner = TaskRunner::spawn(
```

을 다음으로 바꾼다:

```rust
        runner_config.state_dir = Some(config.data_dir.clone());
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        let runner = TaskRunner::spawn(
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 701개, 실패 0

Run: `cargo test -p opl-api --test two_nodes --test incoming`
Expected: PASS

Run: `cargo test -p opl-reconciler --test migration`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 라이브 마이그레이션 원본 Task와 reconciler 보류·넘겨받기·정리"
```

---

### Task 5: 공유 LVM 디스크의 마이그레이션과 안전한 되돌리기

`Storage::share_volume`·`exclusive_volume`(기본 Ok, 공유 LVM은 `lvchange -asy`·`-aey`), `LvmOps::activate_shared`, resolve의 `incoming_spec`(대상은 공유로 켠다)·`set_disks_shared`. 원본은 `Migrating` 기록 뒤 공유로 바꾸고, 워커는 자기가 켜지 않은 VM(재시작·넘겨받기·보류가 끝난 뒤)의 디스크를 배타로 되돌린다(성공할 때까지). 함께 다듬는 안전 규칙: `Hypervisor::cancel_migration`(되돌리기와 재시작 되돌리기가 진행 중 마이그레이션을 먼저 취소), 워커는 상태 노드가 이 노드이고 Running·Migrating일 때만 `postmigrate` 원본을 재개하고 그 밖의 `postmigrate`는 확정된 마이그레이션의 옛 원본으로 끈다, `Migrating` 기록 뒤 다른 Task를 다시 확인하고 `power`가 Running이 아니면 확정하지 않는다, 받는 중 표시에 Task id(`list_incoming`이 (vm, task))를 적어 정리와 `incoming()`이 대조한다, 보류 중 받은 이벤트는 보류가 끝난 뒤 처리한다(`restart = never`).

**Files:**
- Modify: `crates/opl-api/src/incoming.rs`
- Modify: `crates/opl-api/src/tasks/migrate.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/incoming.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-driver/src/hypervisor.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/tests/qemu_migration.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/migration.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/incoming.rs`에서

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::resolve::runtime_spec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_peer::{IncomingError, IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use opl_reconciler::resolve::incoming_spec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
```

`crates/opl-api/src/incoming.rs`에서

```rust
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 디스크를 켜고 받는 QEMU를
    /// 띄운다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
```

을 다음으로 바꾼다:

```rust
impl MigrationReceiver for IncomingMigrations {
    /// QEMU 버전을 견주고, VM이 이 Task로 이 노드에 오는 중인지 본 뒤 디스크를 공유로 켜고(원본이
    /// 아직 쓴다) 받는 QEMU를 띄운다. 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 먼저
    /// 끈다. 실패하면 켠 디스크를 끈다(띄운 QEMU는 드라이버가 끈다).
    async fn incoming(&self, req: MigrateIncomingRequest) -> Result<u16, IncomingError> {
```

`crates/opl-api/src/incoming.rs`에서

```rust
        let vm = self.wait_migrating_here(req.vm, req.task).await?;
        let c = &self.config;
        // 실패하면 runtime_spec이 켠 볼륨을 끈다.
        let spec = runtime_spec(&self.cluster, &vm, c.node, &c.mount_root, &*c.storage)
            .await
```

을 다음으로 바꾼다:

```rust
        let vm = self.wait_migrating_here(req.vm, req.task).await?;
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        let left = self.hv.list_incoming().await.map_err(fail)?;
        if left
            .iter()
            .any(|&(id, task)| id == req.vm && task != req.task)
        {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an earlier migration");
            self.hv.stop(req.vm, StopMode::Force).await.map_err(fail)?;
        }
        let c = &self.config;
        // 실패하면 incoming_spec이 켠 볼륨을 끈다.
        let spec = incoming_spec(&self.cluster, &vm, c.node, &c.mount_root, &*c.storage)
            .await
```

`crates/opl-api/src/incoming.rs`에서

```rust
            .hv
            .start_incoming(&spec, &c.listen_host, c.ports.clone())
            .await
```

을 다음으로 바꾼다:

```rust
            .hv
            .start_incoming(&spec, req.task, &c.listen_host, c.ports.clone())
            .await
```

`crates/opl-api/src/incoming.rs`에서

```rust

    /// 확정 전이면(`spec.node`가 이 노드가 아니다) 받는 중 표시가 있는 QEMU를 끄고 디스크를 끈다.
    /// 받는 QEMU가 이미 끝났어도(받기 실패) 디스크는 끈다. 확정됐거나 VM이 다른 마이그레이션 중이면
    /// 거절하고 아무것도 하지 않는다.
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
```

을 다음으로 바꾼다:

```rust

    /// 확정 전이면(`spec.node`가 이 노드가 아니다) 이 Task의 받는 중 표시가 있는 QEMU를 끄고 디스크를
    /// 끈다. 받는 QEMU가 이미 끝났어도(받기 실패) 디스크는 끈다. 확정됐거나 VM이 다른 마이그레이션
    /// 중이면 거절하고 아무것도 하지 않는다. 다른 Task의 받는 QEMU는 그대로 둔다(정리가 끈다).
    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
```

`crates/opl-api/src/incoming.rs`에서

```rust
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        if self
            .hv
            .list_incoming()
            .await
            .map_err(fail)?
            .contains(&req.vm)
        {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an aborted migration");
```

을 다음으로 바꾼다:

```rust
        let fail = |e: opl_driver::DriverError| IncomingError::Failed(e.to_string());
        let left = self.hv.list_incoming().await.map_err(fail)?;
        if left.contains(&(req.vm, req.task)) {
            tracing::info!(vm = %req.vm, task = %req.task, "stopping the receiving QEMU of an aborted migration");
```

`crates/opl-api/src/tasks/migrate.rs` 전체:

```rust
//! 라이브 마이그레이션의 원본 쪽 Task(설계 §2의 2~8단계와 실패 되돌리기)와 남은 마이그레이션 QEMU
//! 정리(설계 §2 "중간에 죽으면").
//!
//! 두 QEMU가 함께 도는 일은 없다: 대상 QEMU는 멈춘 채 받고 확정(`spec.node`를 대상으로 쓰기) 뒤에만
//! 대상 노드의 reconciler가 이어서 실행한다. 원본은 확정되지 않았다고 확인한 뒤에만 이어서 실행한다.
//! 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용된다.

use std::sync::Arc;
use std::time::Duration;

use opl_cluster::ClusterError;
use opl_cluster::types::{NodeId, Rejection};
use opl_driver::{MigrateOptions, MigrateTarget, RuntimeStatus};
use opl_peer::{IncomingMigration, MigrateIncomingRequest, Peers};
use opl_reconciler::ReconcilerHandle;
use opl_reconciler::resolve::set_disks_shared;
use opl_types::storage::VolumeSpec;
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::incoming::deactivate_disks;

/// 확정 쓰기가 (과반수가 없는 등으로) 실패할 때 다시 시도하는 시간. 지나면 되돌린다.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 확정 뒤 status(`Running`, 대상 노드)를 쓰다 실패할 때 다시 시도하는 시간. 지나면 대상 노드의
/// reconciler가 넘겨받으며 마이그레이션 표시를 지우므로 그대로 둔다.
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

/// 실행 중인 VM을 `target` 노드로 옮긴다. 확정 전에 실패하면 원본을 계속 돌리고 실패한다.
pub(super) async fn migrate(ctx: &Ctx, task: &Task, target: Uuid) -> Result<(), String> {
    let id = task.spec.target;
    let peers = ctx
        .config
        .peers
        .clone()
        .ok_or("this node cannot reach other nodes")?;
    // 대상 노드에는 클러스터 주소의 호스트(opld `address`)로 접속한다. 같은 이름이 노드 인증서에 있다.
    let addr = ctx
        .cluster
        .members()
        .addr(NodeId(target))
        .map(str::to_string)
        .ok_or_else(|| format!("node {target} is not a cluster member"))?;
    let host = addr_host(&addr)
        .ok_or_else(|| format!("cannot tell the host of node address {addr:?}"))?
        .to_string();
    let migration = MigrationStatus {
        node: target,
        task: task.id,
    };
    let _hold = Hold::begin(&ctx.reconciler, id).await;
    let vm = match mark_migrating(ctx, id, migration).await {
        Ok(vm) => vm,
        Err(Mark::Refused(e)) => return Err(e),
        // 기록됐을 수도 있다.
        Err(Mark::Unknown(e)) => {
            roll_back(ctx, &peers, &addr, id, migration).await;
            return Err(e);
        }
    };
    // API 검사 뒤 기록 전에 들어온 Task를 막는다. 기록 뒤의 요청은 API가 거절한다.
    let sent = match check_no_other_task(ctx, &vm) {
        Err(e) => Err(e),
        // 대상 노드가 받는 동안 디스크를 함께 연다(공유 LVM은 LV 잠금을 공유로).
        Ok(()) => match set_disks_shared(&ctx.cluster, &vm, &*ctx.storage, true).await {
            Ok(()) => send(ctx, &peers, &addr, &host, id, migration).await,
            Err(e) => Err(format!("cannot share the disks with the target node: {e}")),
        },
    };
    let error = match sent {
        Ok(()) => commit(ctx, id, migration).await.err(),
        Err(e) => Some(e),
    };
    if let Some(error) = error {
        match roll_back(ctx, &peers, &addr, id, migration).await {
            Outcome::RolledBack(None) => return Err(error),
            Outcome::RolledBack(Some(resume)) => {
                return Err(format!(
                    "{error}; the source VM could not be resumed: {resume}"
                ));
            }
            // 실패로 본 확정 쓰기가 적용됐다.
            Outcome::Committed => {}
        }
    }
    finish(ctx, id, migration).await;
    Ok(())
}

/// `begin_migration`을 끝에(실패, 버려진 future 포함) 푼다. 버려져서 풀리면 작업자가 opld 재시작
/// 때처럼 남은 마이그레이션을 되돌린다.
struct Hold {
    reconciler: ReconcilerHandle,
    id: Uuid,
}

impl Hold {
    async fn begin(reconciler: &ReconcilerHandle, id: Uuid) -> Self {
        reconciler.begin_migration(id).await;
        Self {
            reconciler: reconciler.clone(),
            id,
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.reconciler.end_migration(self.id);
    }
}

/// `host:port`(IPv6는 `[addr]:port`)의 호스트
fn addr_host(addr: &str) -> Option<&str> {
    let (host, port) = addr.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

enum Mark {
    /// 기록하지 않았다.
    Refused(String),
    /// 기록됐는지 모른다.
    Unknown(String),
}

/// 2단계: 이 노드에서 실행 중인 VM인지 다시 보고 `Migrating`과 대상을 기록한다. 기록한 VM(읽은 것)을
/// 돌려준다.
async fn mark_migrating(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<Vm, Mark> {
    let refused = |e: String| Mark::Refused(e);
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| refused(e.to_string()))?
            .ok_or_else(|| refused("VM was deleted".into()))?;
        if vm.spec.node != Some(ctx.config.node) {
            return Err(refused(format!(
                "VM {:?} is no longer on this node",
                vm.name
            )));
        }
        let running = vm.spec.power == PowerState::Running && vm.status.phase == VmPhase::Running;
        let state = ctx
            .hv
            .inspect(id)
            .await
            .map_err(|e| refused(e.to_string()))?;
        if !running || state.is_none_or(|s| s.status != RuntimeStatus::Running) {
            return Err(refused(format!("VM {:?} is not running", vm.name)));
        }
        let mut status = vm.status.clone();
        status.phase = VmPhase::Migrating;
        status.migration = Some(migration);
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(vm),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(Mark::Unknown(format!("cannot record the migration: {e}"))),
        }
    }
}

/// 마이그레이션을 기록한 뒤 다시 본다: 이 VM의 스냅샷 만들기·되돌리기·복제 Task나 디스크의 크기
/// 늘리기 Task가 끝나지 않았으면 실패다. API는 마이그레이션 Task와 이것들을 함께 받지 않지만, 검사와
/// 기록 사이에 들어온 요청은 지나갔을 수 있다. 같은 VM의 줄에 앞서 선 Task는 이미 끝났다.
fn check_no_other_task(ctx: &Ctx, vm: &Vm) -> Result<(), String> {
    let read = |e: ClusterError| format!("cannot read tasks: {e}");
    let mut disks = Vec::new();
    for disk in &vm.spec.disks {
        if let Some(volume) = ctx
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)
            .map_err(read)?
        {
            disks.push(volume.id);
        }
    }
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(read)?;
    for t in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let doing = match t.spec.op {
            TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::VmClone { .. }
                if t.spec.target == vm.id =>
            {
                "being snapshotted, reverted or cloned"
            }
            TaskOp::VolumeResize { .. } if disks.contains(&t.spec.target) => {
                "having a disk resized"
            }
            _ => continue,
        };
        return Err(format!(
            "VM {:?} is {doing} (task {}); try again when the task has finished",
            vm.name, t.id
        ));
    }
    Ok(())
}

/// 3~5단계: 대상 노드에 받을 준비를 시키고, 보내고, 대상이 다 받았는지 확인한다.
async fn send(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    host: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Result<(), String> {
    let qemu_version = ctx
        .hv
        .running_qemu_version(id)
        .await
        .map_err(|e| format!("cannot read the QEMU version: {e}"))?;
    let req = MigrateIncomingRequest {
        vm: id,
        task: migration.task,
        qemu_version,
    };
    let port = peers
        .migrate_incoming(addr, req)
        .await
        .map_err(|e| format!("the target node cannot receive the VM: {e}"))?;
    let target = MigrateTarget {
        host: host.to_string(),
        port,
        tls_hostname: host.to_string(),
    };
    let opts = MigrateOptions {
        bandwidth: ctx.config.migration_bandwidth,
        timeout: ctx.config.migration_timeout,
    };
    ctx.hv
        .migrate(id, &target, &opts)
        .await
        .map_err(|e| e.to_string())?;
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    peers
        .migrate_incoming_wait(addr, incoming)
        .await
        .map_err(|e| format!("the target node did not receive the VM: {e}"))
}

/// 6단계: `spec.node`를 대상 노드로 쓴다(revision 확인). 이 쓰기가 적용되면 확정이다. 결과를 모르는
/// 오류는 `COMMIT_TIMEOUT` 동안 다시 시도한다(같은 revision 확인이므로 두 번 적용되지 않는다).
async fn commit(ctx: &Ctx, id: Uuid, migration: MigrationStatus) -> Result<(), String> {
    let deadline = Instant::now() + COMMIT_TIMEOUT;
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return Err("VM was deleted".into()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if vm.spec.node == Some(migration.node) {
            return Ok(());
        }
        if vm.spec.node != Some(ctx.config.node) || vm.status.migration != Some(migration) {
            return Err(format!("VM {:?} changed during the migration", vm.name));
        }
        // 정지 요청이 검사를 지나 마이그레이션 중에 적용됐다. 대상에서 다시 켜지 않는다.
        if vm.spec.power != PowerState::Running {
            return Err(format!("VM {:?} was stopped during the migration", vm.name));
        }
        let mut spec = vm.spec.clone();
        spec.node = Some(migration.node);
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e @ ClusterError::Rejected(_)) => return Err(e.to_string()),
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => return Err(format!("cannot record the migration: {e}")),
        }
    }
}

enum Outcome {
    /// 확정되지 않았다. 원본을 이어서 실행하지 못했으면 그 이유
    RolledBack(Option<String>),
    /// 확정 쓰기가 적용돼 있었다.
    Committed,
}

/// 확정 전 실패: 마이그레이션 표시를 지워(읽은 revision 확인) 확정을 막은 뒤 대상의 받는 QEMU를
/// 끄게 하고, 원본이 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게), 원본이 멈춰
/// 있으면(`postmigrate`, `paused`) 이어서 실행한다(못 하면 보류가 풀린 뒤 reconciler가 한다). 디스크는
/// 보류가 풀린 뒤 reconciler가 다시 이 노드만 쓰게 한다(대상이 아직 끄지 못했으면 될 때까지 다시
/// 한다 — 그동안 공유로 남아도 원본은 돈다). 지우기 전에 확정이 적용됐으면 `Committed`다. 결과를 알 수 없으면(과반수 없음) 알 때까지 다시 시도한다. 그동안 원본은
/// 멈춰 있을 수 있지만 두 QEMU가 함께 돌지는 않는다.
async fn roll_back(
    ctx: &Ctx,
    peers: &Peers,
    addr: &str,
    id: Uuid,
    migration: MigrationStatus,
) -> Outcome {
    loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        if vm.spec.node == Some(migration.node) {
            return Outcome::Committed;
        }
        let current = vm.status.migration == Some(migration);
        let revision = vm.revision;
        let mut status = vm.status.clone();
        if !current {
            break;
        }
        status.migration = None;
        status.phase = VmPhase::Running;
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot roll back the migration; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    let incoming = IncomingMigration {
        vm: id,
        task: migration.task,
    };
    // 닿지 않으면 대상 노드의 정리가 끈다(마이그레이션 표시가 없다).
    if let Err(e) = peers.migrate_incoming_abort(addr, incoming).await {
        tracing::warn!(vm = %id, error = %e, "cannot abort the incoming migration on the target node");
    }
    if let Err(e) = ctx.hv.cancel_migration(id).await {
        tracing::warn!(vm = %id, error = %e, "cannot cancel the outgoing migration");
    }
    let resumed = match ctx.hv.inspect(id).await {
        Ok(Some(state))
            if state.status == postmigrate() || state.status == RuntimeStatus::Paused =>
        {
            tracing::info!(vm = %id, "resuming the source of a failed migration");
            ctx.hv.resume(id).await.map_err(|e| e.to_string())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    Outcome::RolledBack(resumed.err())
}

/// 7~8단계(확정 뒤): status를 대상 노드의 `Running`으로 쓰고, 원본 QEMU를 끄고, 원본 쪽 디스크를
/// 끈다. 대상 QEMU는 대상 노드의 reconciler가 이어서 실행한다. 실패해도 확정은 끝났으므로 경고만
/// 남긴다(남은 원본은 정리가 끈다).
async fn finish(ctx: &Ctx, id: Uuid, migration: MigrationStatus) {
    let deadline = Instant::now() + RECORD_TIMEOUT;
    let vm = loop {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) => vm,
            Ok(None) => return,
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
            // 원본 QEMU는 정리(`sweep`)가 끈다.
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM; leaving the source QEMU to the sweep");
                return;
            }
        };
        // 대상 노드의 reconciler가 이미 넘겨받아 표시를 지웠다.
        if vm.status.migration != Some(migration) {
            break vm;
        }
        let mut status = vm.status.clone();
        status.migration = None;
        status.phase = VmPhase::Running;
        if status.node != Some(migration.node) {
            status.node = Some(migration.node);
            status.pid = None;
        }
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => break vm,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) if Instant::now() < deadline => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the migrated VM");
                break vm;
            }
        }
    };
    stop_source(ctx, &vm).await;
}

/// 마이그레이션을 보내고 멈춘 원본 QEMU(`postmigrate`)를 끄고(탭은 드라이버가 지운다) 이 노드의
/// 디스크를 끈다. 다른 상태의 QEMU는 끄지 않고, QEMU가 남아 있으면 디스크도 그대로 둔다.
async fn stop_source(ctx: &Ctx, vm: &Vm) {
    let stopped = match ctx.hv.stop_migrated(vm.id).await {
        Ok(true) => true,
        Ok(false) => matches!(ctx.hv.inspect(vm.id).await, Ok(None)),
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
            false
        }
    };
    if stopped {
        deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
    }
}

/// 남은 마이그레이션 QEMU를 시작할 때와 그 뒤 `tag_sweep`마다 끈다([`sweep`]). 실행기가 멈추면
/// 함께 멈춘다.
pub(super) async fn sweep_every(ctx: Arc<Ctx>) {
    loop {
        sweep(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 남은 마이그레이션 QEMU를 끄고 디스크를 끈다:
/// - 마이그레이션을 보내고 멈춘(`postmigrate`) 원본인데 VM이 다른 노드로 확정됐거나 지워졌다(원본
///   opld가 확정 뒤 정리 전에 죽었거나 정리하지 못했다). VM이 이 노드에 배정돼 있어도 이 노드에서 돌던
///   VM이 아니면(`status.node`가 다른 노드이거나 phase가 `Running`·`Migrating`이 아니다) 확정 뒤
///   이 노드로 다시 옮겨진 것이므로 낡은 원본이다: QEMU만 끈다(디스크는 이 노드의 reconciler가 다룬다).
/// - 받는 중 표시가 있는 QEMU인데 VM이 이 노드로 확정되지 않았고 그 표시의 Task로 이 노드에 오는
///   중도 아니다(원본이 되돌렸거나 확정 전에 죽은 뒤 되돌려졌거나, 앞선 마이그레이션의 것이다). VM이
///   지워졌어도 끈다.
///
/// 확정 전의 원본(`spec.node`·`status.node`가 이 노드)은 마이그레이션 Task나 reconciler가 다룬다.
pub(super) async fn sweep(ctx: &Ctx) {
    let me = ctx.config.node;
    match ctx.hv.list().await {
        Ok(states) => {
            for state in states.into_iter().filter(|s| s.status == postmigrate()) {
                match ctx.cluster.get::<VmSpec>(state.id) {
                    Ok(Some(vm))
                        if vm.spec.node == Some(me)
                            && vm.status.node == Some(me)
                            && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating) => {
                    }
                    Ok(Some(vm)) if vm.spec.node == Some(me) => {
                        tracing::info!(vm = %vm.id, "stopping the stale source QEMU of a migration committed elsewhere");
                        if let Err(e) = ctx.hv.stop_migrated(vm.id).await {
                            tracing::warn!(vm = %vm.id, error = %e, "cannot stop the migrated source QEMU");
                        }
                    }
                    Ok(Some(vm)) => {
                        tracing::info!(vm = %vm.id, "stopping the source QEMU of a migration committed elsewhere");
                        stop_source(ctx, &vm).await;
                    }
                    // 옮긴 뒤 끄고 지웠다. 볼륨은 VM과 함께 지워지지 않지만 이 노드에서 켜진 채
                    // 남은 볼륨은 실행기의 정리가 끈다.
                    Ok(None) => {
                        tracing::info!(vm = %state.id, "stopping the source QEMU of a deleted VM");
                        if let Err(e) = ctx.hv.stop_migrated(state.id).await {
                            tracing::warn!(vm = %state.id, error = %e, "cannot stop the migrated source QEMU");
                        }
                    }
                    Err(e) => tracing::warn!(vm = %state.id, error = %e, "cannot read VM"),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
    }
    let incoming = match ctx.hv.list_incoming().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list incoming migrations");
            return;
        }
    };
    for (id, task) in incoming {
        let vm = match ctx.cluster.get::<VmSpec>(id) {
            Ok(vm) => vm,
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                continue;
            }
        };
        if let Some(vm) = &vm
            && (vm.spec.node == Some(me)
                || vm.status.migration == Some(MigrationStatus { node: me, task }))
        {
            continue;
        }
        // 그 사이 같은 VM의 새 마이그레이션을 받기 시작했으면(앞선 받는 QEMU를 끄고 새로 띄웠다) 새
        // QEMU를 끄지 않는다.
        match ctx.hv.list_incoming().await {
            Ok(now) if now.contains(&(id, task)) => {}
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list incoming migrations");
                continue;
            }
        }
        tracing::info!(vm = %id, "stopping the receiving QEMU of a migration that did not complete");
        if let Err(e) = ctx.hv.stop(id, opl_driver::StopMode::Force).await {
            tracing::warn!(vm = %id, error = %e, "cannot stop the receiving QEMU");
            continue;
        }
        if let Some(vm) = &vm {
            deactivate_disks(&ctx.cluster, &*ctx.storage, vm).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::addr_host;

    #[test]
    fn host_of_a_cluster_address() {
        assert_eq!(addr_host("10.0.0.2:7443"), Some("10.0.0.2"));
        assert_eq!(addr_host("node-b.lan:7443"), Some("node-b.lan"));
        assert_eq!(addr_host("[fd00::2]:7443"), Some("fd00::2"));
        assert_eq!(addr_host("10.0.0.2"), None);
        assert_eq!(addr_host(":7443"), None);
    }
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::auth::{Role, UserSpec};
```

을 다음으로 바꾼다:

```rust
};
use opl_reconciler::{Reconciler, ReconcilerConfig, ReconcilerHandle, RestartBackoff};
use opl_types::auth::{Role, UserSpec};
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.login(name, "password-1").await
    }
```

을 다음으로 바꾼다:

```rust
        self.login(name, "password-1").await
    }

    /// reconciler의 핸들(마이그레이션 보류 등)
    pub fn reconciler(&self) -> ReconcilerHandle {
        self.reconciler.as_ref().unwrap().handle()
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    slow_deactivates: std::sync::Mutex<std::collections::HashMap<Uuid, std::time::Duration>>,
}
```

을 다음으로 바꾼다:

```rust
    slow_deactivates: std::sync::Mutex<std::collections::HashMap<Uuid, std::time::Duration>>,
    /// 남은 횟수만큼 `exclusive_volume`이 일시 오류다(다른 노드가 아직 공유로 켜 두었다).
    held_exclusives: std::sync::Mutex<usize>,
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.active.lock().unwrap().push((pool.into(), id));
    }
```

을 다음으로 바꾼다:

```rust
        self.active.lock().unwrap().push((pool.into(), id));
    }

    /// 다음 `n`번의 `exclusive_volume`이 (호출을 기록한 뒤) 일시 오류다.
    pub fn hold_exclusives(&self, n: usize) {
        *self.held_exclusives.lock().unwrap() = n;
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.active.lock().unwrap().retain(|(_, v)| *v != id);
        Ok(())
```

을 다음으로 바꾼다:

```rust
        self.active.lock().unwrap().retain(|(_, v)| *v != id);
        Ok(())
    }
    /// 호출을 `share`로 적는다.
    async fn share_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("share", id));
        Ok(())
    }
    /// 호출을 `exclusive`로 적는다.
    async fn exclusive_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("exclusive", id));
        let mut held = self.held_exclusives.lock().unwrap();
        if *held > 0 {
            *held -= 1;
            return Err(DriverError::Timeout("another node to stop using the lv"));
        }
        Ok(())
```

`crates/opl-api/tests/incoming.rs`에서

```rust
    fn deactivations(&self) -> usize {
        self.storage
            .calls()
            .iter()
            .filter(|(op, id)| *op == "deactivate" && *id == self.volume)
            .count()
```

을 다음으로 바꾼다:

```rust
    fn deactivations(&self) -> usize {
        self.count("deactivate")
    }

    /// 볼륨에 대한 저장소 호출 `op`의 수
    fn count(&self, op: &str) -> usize {
        self.storage
            .calls()
            .iter()
            .filter(|(o, id)| *o == op && *id == self.volume)
            .count()
```

`crates/opl-api/tests/incoming.rs`에서

```rust
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![s.vm]);
    // 원본 노드에 배정된 VM이므로 이 노드의 reconciler는 받는 QEMU를 그대로 둔다.
```

을 다음으로 바꾼다:

```rust
    assert!(s.env.hv.is_incoming(s.vm));
    // 원본이 아직 쓰는 디스크를 공유로 켰다.
    assert_eq!(s.count("share"), 1);
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![(s.vm, task)]);
    // 원본 노드에 배정된 VM이므로 이 노드의 reconciler는 받는 QEMU를 그대로 둔다.
```

`crates/opl-api/tests/incoming.rs`에서

```rust
}

#[tokio::test]
async fn abort_leaves_a_committed_migration_alone() {
```

을 다음으로 바꾼다:

```rust
}

/// 앞선 마이그레이션(다른 Task)의 받는 QEMU가 남아 있으면 끄고 새로 받는다. 취소는 그 Task의 받는
/// QEMU만 끈다.
#[tokio::test]
async fn receiving_qemu_of_another_task_is_replaced() {
    let s = setup().await;
    let old = Uuid::now_v7();
    s.mark_migrating(s.env.node, old).await;
    s.receiver.incoming(s.request(old)).await.unwrap();

    let task = Uuid::now_v7();
    s.mark_migrating(s.env.node, task).await;
    s.receiver.incoming(s.request(task)).await.unwrap();
    assert_eq!(s.env.hv.list_incoming().await.unwrap(), vec![(s.vm, task)]);
    assert_eq!(s.env.hv.launches().len(), 0);

    // 마이그레이션 표시가 없어도 다른 Task의 취소는 이 받는 QEMU를 끄지 않는다(정리가 끈다).
    let mut vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    vm.status.phase = VmPhase::Running;
    vm.status.migration = None;
    s.env
        .cluster
        .update_status::<VmSpec>(s.vm, &vm.status)
        .await
        .unwrap();
    s.receiver
        .abort(IncomingMigration {
            vm: s.vm,
            task: old,
        })
        .await
        .unwrap();
    assert!(s.env.hv.is_incoming(s.vm));
    assert_eq!(s.deactivations(), 0);
    s.receiver
        .abort(IncomingMigration { vm: s.vm, task })
        .await
        .unwrap();
    assert!(s.env.hv.inspect(s.vm).await.unwrap().is_none());
    assert_eq!(s.deactivations(), 1);
}

#[tokio::test]
async fn abort_leaves_a_committed_migration_alone() {
```

`crates/opl-api/tests/incoming.rs`에서

```rust

/// 이 노드에서 보낸 원본(`postmigrate`)이 남았는데 VM은 다른 노드로 확정됐다: 정리가 끄고 디스크를
```

을 다음으로 바꾼다:

```rust

/// 이 노드로 오는 중이지만 다른 Task다(앞선 마이그레이션의 받는 QEMU가 남았다): 정리가 끈다.
#[tokio::test]
async fn sweep_stops_a_receiving_qemu_of_another_task() {
    let mut s = setup().await;
    let old = Uuid::now_v7();
    s.mark_migrating(s.env.node, old).await;
    s.receiver.incoming(s.request(old)).await.unwrap();
    s.mark_migrating(s.env.node, Uuid::now_v7()).await;
    s.sweep_often().await;
    s.wait_stopped(s.vm).await;
    assert!(s.env.hv.list_incoming().await.unwrap().is_empty());
}

/// 이 노드에서 보낸 원본(`postmigrate`)이 남았는데 VM은 다른 노드로 확정됐다: 정리가 끄고 디스크를
```

`crates/opl-api/tests/incoming.rs`에서

```rust
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        s.running(db.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );
}
```

을 다음으로 바꾼다:

```rust
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 정리는 끄지 않는다(이 노드의 reconciler가 이어서 실행할 수는 있다).
    assert!(s.running(db.id).await.is_some());
}

/// 보낸 원본(`postmigrate`)이 남았는데 VM이 지워졌다(다른 노드로 옮긴 뒤 끄고 지웠다): 정리가 끈다.
#[tokio::test]
async fn sweep_stops_a_source_qemu_of_a_deleted_vm() {
    let mut s = setup().await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    let vm = s.env.cluster.get::<VmSpec>(s.vm).unwrap().unwrap();
    let spec = VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "gone".into(),
        cpu: vm.spec.cpu.clone(),
        memory_mib: vm.spec.memory_mib,
        firmware: vm.spec.firmware,
        machine: vm.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    s.env.hv.start(&spec, None).await.unwrap();
    s.env.hv.migrate(spec.id, &target, &opts).await.unwrap();
    s.sweep_often().await;
    s.wait_stopped(spec.id).await;
}

/// 이 노드에 배정된 VM의 원본(`postmigrate`)이 남았는데 이 노드에서 돌던 VM이 아니다(다른 노드로
/// 확정된 뒤 거기서 꺼지고 이 노드로 다시 옮겨졌다): 낡은 원본이므로 정리가 끈다. 디스크는 이 노드의
/// reconciler가 다루므로 끄지 않는다.
#[tokio::test]
async fn sweep_stops_a_stale_source_qemu_assigned_here() {
    let mut s = setup().await;
    let admin = s.env.admin().await;
    let mut req = vm_request("db");
    req["spec"]["node"] = json!(s.env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let db = vm_named(&s.env, "db");
    // 정리만 보도록 reconciler는 손대지 않게 한다.
    s.env.reconciler().begin_migration(db.id).await;
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    s.env.hv.migrate(db.id, &target, &opts).await.unwrap();
    let mut status = db.status.clone();
    status.node = Some(s.source);
    status.phase = VmPhase::Stopped;
    status.pid = None;
    s.env
        .cluster
        .update_status::<VmSpec>(db.id, &status)
        .await
        .unwrap();

    s.sweep_often().await;
    s.wait_stopped(db.id).await;
    assert_eq!(s.deactivations(), 0);
}
```

`crates/opl-api/tests/two_nodes.rs` 전체:

```rust
//! 실제 gRPC(mTLS)로 이은 두 노드: 한 노드의 API로 다른 노드에 VM을 띄우고 콘솔을 중계한다.
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기, 공유 디스크의 잠금 바꾸기.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use openraft::BasicNode;
use opl_api::{AppState, IncomingConfig, IncomingMigrations, TaskRunner, hash_password};
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{DriverError, Hypervisor, MigrateTarget, MockHypervisor, RuntimeStatus, Storage};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{GrpcNetwork, PeerService, Peers};
use opl_reconciler::Reconciler;
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

use common::{
    ADMIN_PASSWORD, FakeStorage, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config,
    vm_request,
};

/// 받는 노드가 마이그레이션을 받는 포트 범위. Mock 하이퍼바이저는 첫 포트를 연다.
const MIGRATION_PORTS: std::ops::RangeInclusive<u16> = 61000..=61009;

struct Node {
    id: Uuid,
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 볼륨 호출을 적는 저장소(reconciler, Task 실행기, 마이그레이션 받기가 함께 쓴다)
    storage: Arc<FakeStorage>,
    tls: NodeTls,
    listener: Option<TcpListener>,
    base: String,
    _workers: Option<(Reconciler, TaskRunner)>,
}

impl Node {
    async fn open(ca: &ClusterCa, root: &TempDir) -> Node {
        let id = Uuid::now_v7();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, id, &["127.0.0.1".into()]).unwrap(),
            key_pem,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(NodeId(id), root.path().join(id.to_string()));
        // 테스트가 동시에 많이 돌면 첫 선거가 몇 초씩 걸린다.
        config.write_timeout = Duration::from_secs(30);
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
        Node {
            id,
            addr,
            cluster,
            peers,
            hv: Arc::new(MockHypervisor::default()),
            storage: Arc::new(FakeStorage::default()),
            tls,
            listener: Some(listener),
            base: String::new(),
            _workers: None,
        }
    }

    /// 노드 간 서버(마이그레이션 받기 포함), reconciler, Task 실행기, API 서버(HTTP)를 띄운다.
    async fn serve(&mut self, root: &TempDir) {
        let hv: Arc<dyn Hypervisor> = self.hv.clone();
        let storage: Arc<dyn Storage> = self.storage.clone();
        let incoming = IncomingMigrations::new(
            self.cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node: self.id,
                mount_root: MOUNT_ROOT.into(),
                storage: storage.clone(),
                listen_host: "127.0.0.1".into(),
                ports: MIGRATION_PORTS,
            },
        );
        let service = PeerService::new(self.cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming));
        let listener = self.listener.take().unwrap();
        let tls = self.tls.clone();
        tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, std::future::pending())
                .await
                .unwrap();
        });
        let mut reconciler_config = reconciler_config(self.id);
        reconciler_config.storage = storage.clone();
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let work = root.path().join(format!("{}-work", self.id));
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
        self._workers = Some((reconciler, runner));
    }
}

/// 두 노드를 띄워 클러스터를 만들고 Node 리소스와 관리자를 채운다. 노드 간 서버가 먼저 떠야
/// 선거가 끝나므로 서버부터 띄운다.
async fn two_nodes() -> (TempDir, Node, Node) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut a = Node::open(&ca, &root).await;
    let mut b = Node::open(&ca, &root).await;
    a.serve(&root).await;
    b.serve(&root).await;
    let members = BTreeMap::from([
        (NodeId(a.id), BasicNode::new(&a.addr)),
        (NodeId(b.id), BasicNode::new(&b.addr)),
    ]);
    a.cluster.initialize(members).await.unwrap();
    for (i, node) in [&a, &b].into_iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node{}", i + 1),
            NodeSpec {
                address: node.base.clone(),
                maintenance: false,
            },
        );
        r.id = node.id;
        r.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
        // 리더가 뽑힐 때까지 쓰기가 기다린다.
        a.cluster.create(&r).await.unwrap();
    }
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    a.cluster.create(&admin).await.unwrap();
    (root, a, b)
}

#[tokio::test]
async fn console_of_a_vm_on_another_node_is_relayed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    b.hv.set_console_dir(tmp.path());

    // 노드 A의 API로 노드 B에 VM을 만들고 켠다. Task는 B의 실행기가 끝낸다.
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(b.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = b
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    assert!(b.hv.inspect(vm.id).await.unwrap().is_some());
    assert!(a.hv.inspect(vm.id).await.unwrap().is_none());

    let listener =
        tokio::net::UnixListener::bind(tmp.path().join(format!("{}.vnc", vm.id))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });
    let url = format!("{}/api/v1/vms/web/console", a.base).replacen("http", "ws", 1);
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", admin.token.as_ref().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let greeting = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(greeting, Message::binary(b"RFB 003.008\n".to_vec()));
    ws.send(Message::binary(b"ping".to_vec())).await.unwrap();
    let echo = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echo, Message::binary(b"ping".to_vec()));

    // A의 API로 만든 VM은 B에서도 곧 보인다(같은 저장소).
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let deadline = Instant::now() + WAIT;
    while admin_b.get("/vms/web").await.0 != StatusCode::OK {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A에서 도는 VM `web`(디스크 없음)을 만든다.
async fn running_vm(a: &Node, admin: &common::Client) -> opl_types::vm::Vm {
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap()
}

async fn migrate(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": node }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

async fn runtime_status(node: &Node, id: Uuid) -> Option<RuntimeStatus> {
    node.hv.inspect(id).await.unwrap().map(|s| s.status)
}

/// 두 노드에서 VM이 원하는 대로 보일 때까지 기다린다.
async fn wait_vm(node: &Node, id: Uuid, what: &str, pred: impl Fn(&opl_types::vm::Vm) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        let vm = node.cluster.get::<VmSpec>(id).unwrap().unwrap();
        if pred(&vm) {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {:?}", vm.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 확정 전에 실패한 마이그레이션 뒤: VM은 A에 남아 돌고, B에는 받는 QEMU가 없다.
async fn assert_still_on_a(a: &Node, b: &Node, id: Uuid) {
    let vm = a.cluster.get::<VmSpec>(id).unwrap().unwrap();
    assert_eq!(vm.spec.node, Some(a.id));
    assert_eq!(vm.status.phase, VmPhase::Running);
    assert_eq!(vm.status.node, Some(a.id));
    assert_eq!(vm.status.migration, None);
    assert_eq!(runtime_status(a, id).await, Some(RuntimeStatus::Running));
    assert_eq!(runtime_status(b, id).await, None);
    assert!(!b.hv.is_incoming(id));
}

#[tokio::test]
async fn running_vm_migrates_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    // B의 API로 요청해도 Task는 원본 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = migrate(&admin_b, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], a.id.to_string());

    // A의 QEMU는 B의 클러스터 주소의 호스트, B가 연 포트로 보냈다.
    assert_eq!(
        a.hv.migrations(),
        vec![(
            vm.id,
            MigrateTarget {
                host: "127.0.0.1".into(),
                port: *MIGRATION_PORTS.start(),
                tls_hostname: "127.0.0.1".into(),
            }
        )]
    );
    let vm_now = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(vm_now.spec.node, Some(b.id));
    assert_eq!(vm_now.status.node, Some(b.id));
    assert_eq!(vm_now.status.migration, None);
    assert_eq!(runtime_status(&a, vm.id).await, None);
    // B의 reconciler가 받은 QEMU를 넘겨받아 이어서 실행한다(새로 띄우지 않는다).
    wait_vm(&b, vm.id, "running on B", |v| {
        v.status.phase == VmPhase::Running && v.status.node == Some(b.id) && v.status.pid.is_some()
    })
    .await;
    let deadline = Instant::now() + WAIT;
    while b.hv.is_incoming(vm.id) {
        assert!(Instant::now() < deadline, "B did not take over the VM");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    assert!(b.hv.launches().is_empty(), "{:?}", b.hv.launches());

    // 옮긴 VM은 B에서 다룬다: 끄면 B의 QEMU가 꺼진다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], b.id.to_string());
    assert_eq!(runtime_status(&b, vm.id).await, None);
}

#[tokio::test]
async fn failed_send_keeps_the_vm_running_on_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration(
        "Failed to connect to '127.0.0.1:61000': Connection refused".into(),
    ));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("Connection refused"), "{error}");
    assert_still_on_a(&a, &b, vm.id).await;
    // 다른 동작이 다시 된다.
    let task = admin.act("web", "reboot").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 원본은 다 보냈지만(`postmigrate`) 대상이 받기에 실패했다: 확정하지 않고 원본을 이어서 실행한다.
#[tokio::test]
async fn failed_receive_resumes_the_source() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("TLS handshake failed"), "{error}");
    assert_eq!(a.hv.migrations().len(), 1);
    assert_still_on_a(&a, &b, vm.id).await;
}

/// 원본 QEMU가 보내는 중인데 기다리기가 실패했다: 되돌리며 그 마이그레이션을 취소한다. 나중에 끝나
/// 원본을 멈추지 않는다.
#[tokio::test]
async fn unfinished_send_is_cancelled_by_the_rollback() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!a.hv.is_outgoing(vm.id));
    a.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_still_on_a(&a, &b, vm.id).await;
}

/// API 검사를 거치지 않고 Task를 만든다(검사와 마이그레이션 기록 사이에 들어온 요청).
async fn put_task(node: &Node, op: TaskOp, target: Uuid, on: Uuid) -> Uuid {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: on,
        },
    );
    task.name = task.id.to_string();
    node.cluster.create(&task).await.unwrap();
    task.id
}

/// Task가 끝날 때까지 기다린다.
async fn finished_task(node: &Node, id: Uuid) -> Task {
    let deadline = Instant::now() + WAIT;
    loop {
        let task = node.cluster.get::<TaskSpec>(id).unwrap().unwrap();
        if task.status.state.is_finished() {
            return task;
        }
        assert!(Instant::now() < deadline, "task {id}: {:?}", task.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// API가 마이그레이션을 받은 뒤 같은 VM의 스냅샷·디스크 크기 Task가 들어왔다: 원본 Task가 기록한 뒤
/// 다시 보고 되돌린다.
#[tokio::test]
async fn task_started_alongside_stops_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    // 실행하는 노드가 없어 끝나지 않는다.
    let elsewhere = Uuid::now_v7();
    let others = [
        (
            TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            vm.id,
            "snapshotted",
        ),
        (
            TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            volume,
            "resized",
        ),
    ];
    for (op, target, what) in others {
        let other = put_task(&a, op, target, elsewhere).await;
        let migrate = put_task(&a, TaskOp::VmMigrate { node: b.id }, vm.id, a.id).await;
        let task = finished_task(&a, migrate).await;
        assert_eq!(task.status.state, TaskState::Failed, "{:?}", task.status);
        let error = task.status.error.unwrap();
        assert!(error.contains(what), "{error}");
        assert!(a.hv.migrations().is_empty());
        assert_still_on_a(&a, &b, vm.id).await;
        a.cluster.delete::<TaskSpec>(other, None).await.unwrap();
    }
}

/// 보내는 동안 VM을 껐다(검사와 기록 사이에 들어온 정지): 확정하지 않는다.
#[tokio::test]
async fn vm_stopped_during_the_migration_is_not_committed() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let (status, task) = admin
        .post("/vms/web/migrate", serde_json::json!({ "node": "node2" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_vm(&a, vm.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let current = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    a.cluster
        .update_spec(vm.id, None, current.labels, &spec)
        .await
        .unwrap();

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("stopped"), "{error}");
    let got = a.cluster.get::<VmSpec>(vm.id).unwrap().unwrap();
    assert_eq!(got.spec.node, Some(a.id));
    assert_eq!(runtime_status(&b, vm.id).await, None);
    // A의 reconciler가 원하는 대로 끈다.
    wait_vm(&a, vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped).await;
}

#[tokio::test]
async fn different_qemu_versions_do_not_migrate() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let vm = running_vm(&a, &admin).await;

    b.hv.set_qemu_version("9.0.0");
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("QEMU 9.0.0"), "{error}");
    assert!(a.hv.migrations().is_empty());
    assert_still_on_a(&a, &b, vm.id).await;
}

/// A에서 도는 VM `web`. 디스크는 공유 dir 풀 `nas`의 볼륨 `root`다. 볼륨 id를 함께 돌려준다.
async fn running_vm_with_disk(a: &Node, admin: &common::Client) -> (opl_types::vm::Vm, Uuid) {
    let pool: StoragePool = Resource::new(
        "nas",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/nonexistent/opl-nas".into(),
            },
            nodes: vec![],
            shared: true,
        },
    );
    a.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "nas".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(a.id);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request("web");
    req["spec"]["node"] = serde_json::json!(a.id);
    req["spec"]["disks"] = serde_json::json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let vm = a.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    (vm, volume.id)
}

/// 노드의 볼륨 `id`에 대한 공유·배타·끄기 호출(차례대로)
fn lock_calls(node: &Node, id: Uuid) -> Vec<&'static str> {
    node.storage
        .calls()
        .into_iter()
        .filter(|(op, v)| *v == id && ["share", "exclusive", "deactivate"].contains(op))
        .map(|(op, _)| op)
        .collect()
}

/// 공유 디스크(공유 LVM이면 LV 잠금): 원본은 보내기 전에 공유로 바꾸고 확정 뒤 끈다. 대상은 공유로
/// 켜서 받고, 넘겨받은 뒤 원본이 끌 때까지(일시 오류) 다시 시도해 배타로 바꾼다.
#[tokio::test]
async fn shared_disks_are_shared_during_the_migration() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;
    assert!(lock_calls(&a, volume).is_empty());

    b.storage.hold_exclusives(1);
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(lock_calls(&a, volume), ["share", "deactivate"]);
    let deadline = Instant::now() + WAIT;
    while lock_calls(&b, volume).len() < 3 {
        assert!(
            Instant::now() < deadline,
            "B did not make the disk exclusive: {:?}",
            lock_calls(&b, volume)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(lock_calls(&b, volume), ["share", "exclusive", "exclusive"]);
    assert_eq!(
        runtime_status(&b, vm.id).await,
        Some(RuntimeStatus::Running)
    );
    // 한 번 배타로 바꾼 뒤에는 다시 하지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&b, volume).len(), 3);
}

/// 확정 전 실패: 대상은 받으려고 켠 디스크를 끄고, 원본은(보류가 풀린 reconciler가) 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn failed_migration_makes_shared_disks_exclusive_again() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    let (vm, volume) = running_vm_with_disk(&a, &admin).await;

    a.hv.fail_next_migrate(DriverError::Migration("Connection refused".into()));
    let task = migrate(&admin, "node2").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_still_on_a(&a, &b, vm.id).await;
    let deadline = Instant::now() + WAIT;
    while lock_calls(&a, volume).len() < 2 {
        assert!(Instant::now() < deadline, "{:?}", lock_calls(&a, volume));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lock_calls(&a, volume), ["share", "exclusive"]);
    assert_eq!(lock_calls(&b, volume), ["share", "deactivate"]);
}
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시를 남기고, TLS
    /// 자격 증명(받는 쪽, 상대 인증서 확인)과 능력을 정한 뒤 `ports`에서 빈 포트로 `listen_host`에서
```

을 다음으로 바꾼다:

```rust

    /// 라이브 마이그레이션을 받을 QEMU를 `-incoming defer`로 띄우고, 받는 중 표시(마이그레이션 Task
    /// `task`)를 남기고, TLS
    /// 자격 증명(받는 쪽, 상대 인증서 확인)과 능력을 정한 뒤 `ports`에서 빈 포트로 `listen_host`에서
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust
        vm: &VmRuntimeSpec,
        listen_host: &str,
```

을 다음으로 바꾼다:

```rust
        vm: &VmRuntimeSpec,
        task: Uuid,
        listen_host: &str,
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 받는 중 표시가 있는 살아 있는 VM. 확정되지 않은 채 남은 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn list_incoming(&self) -> Result<Vec<Uuid>, DriverError>;
```

을 다음으로 바꾼다:

```rust

    /// 받는 중 표시가 있는 살아 있는 VM과 그 표시의 마이그레이션 Task(읽을 수 없으면 nil). 확정되지
    /// 않은 채 남은 받는 QEMU를 찾아 끄는 데 쓴다.
    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError>;
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

을 다음으로 바꾼다:

```rust

    /// 이 VM이 보내는 중인 마이그레이션을 취소하고 끝날 때까지 기다린다(QMP `migrate_cancel`). 보내는
    /// 중이 아니면(끝났거나, 받는 쪽이거나, VM이 없다) 아무것도 하지 않는다. 취소하는 사이에 끝났으면
    /// 원본은 `postmigrate`로 멈춰 있다. opld가 기다리기를 그만둔 마이그레이션(재시작, 되돌리기)이
    /// 나중에 끝나 원본을 멈추지 않게 한다.
    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

`crates/opl-driver/src/mock.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::migration::{MigrateOptions, MigrateTarget};

/// 따로 정하지 않았을 때 `qemu_version`이 돌려주는 버전
pub const MOCK_QEMU_VERSION: &str = "8.2.2";

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
    /// `pause`로 멈추고 아직 `resume`하지 않은 VM
    paused: HashSet<Uuid>,
    /// 멈춘 채 남은 표시(`paused-by-opl`)가 있는 VM(이전 opld가 멈춘 채 죽었다)
    marked: HashSet<Uuid>,
    /// `pause`·`resume` 호출 (VM, 이름)
    pauses: Vec<(Uuid, &'static str)>,
    /// 조회에 드러낼 상태(QEMU가 스스로 멈춘 상태 등)
    statuses: HashMap<Uuid, RuntimeStatus>,
    /// `set_qemu_version`으로 정한 QEMU 버전
    qemu_version: Option<String>,
    /// 받는 중 표시가 있는 VM(`start_incoming`으로 띄우고 아직 `finish_incoming`하지 않음)과 그
    /// 마이그레이션 Task
    incoming: HashMap<Uuid, Uuid>,
    /// 마이그레이션이 만든 상태: 받는 쪽 `inmigrate`·받은 뒤 멈춤, 보낸 원본 `postmigrate`.
    /// VM이 꺼지면 사라진다.
    migration_status: HashMap<Uuid, RuntimeStatus>,
    /// 다음 `migrate` 한 번을 실패시킬 오류
    fail_next_migrate: Option<DriverError>,
    /// 다음 `incoming_wait` 한 번을 실패시킬 오류. 실제 QEMU처럼 받는 QEMU는 끝난다.
    fail_next_incoming: Option<DriverError>,
    /// `migrate` 호출 (VM, 보낸 곳). 실패한 호출 포함
    migrations: Vec<(Uuid, MigrateTarget)>,
    /// 보내는 중인 마이그레이션이 있는 VM(누구도 끝을 기다리지 않는다). 원본은 돈다.
    outgoing: HashSet<Uuid>,
    /// 다음 `migrate` 한 번을 이 오류로 실패시키되 마이그레이션은 보내는 중으로 남긴다.
    fail_next_migrate_unfinished: Option<DriverError>,
    /// `cancel_migration`이 취소하는 사이에 마이그레이션이 끝난다(원본은 `postmigrate`).
    complete_on_cancel: bool,
    /// `migrate`가 이만큼 기다린 뒤에 동작한다.
    migrate_delay: Option<Duration>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 다음 `snapshot_save` 한 번이 VM이 꺼질 때까지 끝나지 않게 한다. 실제 QEMU의 메모리 저장처럼
    /// 그동안 VM을 강제로 끄면 QMP가 끊긴 오류로 실패하고 태그를 남기지 않는다.
    pub fn hang_next_snapshot_save(&self) {
        self.state.lock().unwrap().hang_next_save = true;
    }

    /// 멈춘 `snapshot_save`가 들어올 때까지 기다린다.
    pub async fn snapshot_save_reached(&self) {
        self.save_reached.notified().await;
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
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

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// `pause`로 멈추고 아직 `resume`하지 않았는지
    pub fn is_paused(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().paused.contains(&id)
    }

    /// 이전 opld가 `pause`한 채 죽은 것처럼 VM을 표시와 함께 멈춘다. `resume_left_stopped`가
    /// 이어서 실행한다(지금 `pause`해 둔 VM은 표시가 없는 것으로 본다).
    pub fn leave_paused(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        st.paused.insert(id);
        st.marked.insert(id);
    }

    /// VM에 부른 `pause`·`resume`(실패한 호출 포함)을 순서대로 돌려준다.
    pub fn pause_calls(&self, id: Uuid) -> Vec<&'static str> {
        let st = self.state.lock().unwrap();
        st.pauses
            .iter()
            .filter(|(vm, _)| *vm == id)
            .map(|(_, call)| *call)
            .collect()
    }

    /// 조회(`inspect`·`list`)가 VM의 상태를 `status`로 알리게 한다(`None`이면 되돌린다). QEMU가 스스로
    /// 멈춘 상태(`io-error` 등)를 흉내 낸다. QMP로 다룰 수 없는 VM은 그대로 `unresponsive`다.
    pub fn set_status(&self, id: Uuid, status: Option<RuntimeStatus>) {
        let mut st = self.state.lock().unwrap();
        match status {
            Some(status) => st.statuses.insert(id, status),
            None => st.statuses.remove(&id),
        };
    }

    /// `qemu_version`·`running_qemu_version`이 돌려줄 버전(기본 [`MOCK_QEMU_VERSION`])
    pub fn set_qemu_version(&self, version: &str) {
        self.state.lock().unwrap().qemu_version = Some(version.to_string());
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. 원본은 그대로 돈다.
    pub fn fail_next_migrate(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate = Some(err);
    }

    /// 다음 `migrate` 한 번을 이 오류로 실패시킨다. QMP 연결이 끊긴 것처럼 마이그레이션은 보내는
    /// 중으로 남는다([`complete_outgoing`](Self::complete_outgoing)이 끝낸다).
    pub fn fail_next_migrate_unfinished(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_migrate_unfinished = Some(err);
    }

    /// 이후 `migrate`가 이만큼 기다린 뒤에 동작한다(보내는 동안 다른 일이 끼어드는 시험).
    pub fn delay_migrations(&self, delay: Duration) {
        self.state.lock().unwrap().migrate_delay = Some(delay);
    }

    /// 도는 VM이 보내는 중인 마이그레이션을 남긴다(opld가 보내다 죽었다).
    pub fn start_outgoing(&self, id: Uuid) {
        self.state.lock().unwrap().outgoing.insert(id);
    }

    /// 보내는 중인 마이그레이션이 있는지
    pub fn is_outgoing(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().outgoing.contains(&id)
    }

    /// 보내는 중인 마이그레이션이 있으면 끝낸다(원본은 `postmigrate`로 멈춘다).
    pub fn complete_outgoing(&self, id: Uuid) {
        let mut st = self.state.lock().unwrap();
        if st.outgoing.remove(&id) {
            st.migration_status.insert(id, postmigrate());
        }
    }

    /// 이후 `cancel_migration`은 취소하는 사이에 마이그레이션이 끝난 것으로 한다.
    pub fn complete_on_cancel(&self) {
        self.state.lock().unwrap().complete_on_cancel = true;
    }

    /// 다음 `incoming_wait` 한 번을 이 오류로 실패시킨다. 받는 QEMU는 크래시로 끝난다.
    pub fn fail_next_incoming_wait(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_incoming = Some(err);
    }

    /// `migrate` 호출(실패 포함)을 순서대로 돌려준다.
    pub fn migrations(&self) -> Vec<(Uuid, MigrateTarget)> {
        self.state.lock().unwrap().migrations.clone()
    }

    /// 받는 중 표시가 있는지
    pub fn is_incoming(&self, id: Uuid) -> bool {
        self.state.lock().unwrap().incoming.contains_key(&id)
    }

    fn version(st: &MockState) -> String {
        st.qemu_version
            .clone()
            .unwrap_or_else(|| MOCK_QEMU_VERSION.to_string())
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        st.paused.remove(&id);
        st.marked.remove(&id);
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        st.outgoing.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else if let Some(status) = st.statuses.get(&id) {
            status.clone()
        } else if let Some(status) = st.migration_status.get(&id) {
            status.clone()
        } else if st.paused.contains(&id) {
            RuntimeStatus::Paused
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().start_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
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
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.paused.remove(&id);
                        st.incoming.remove(&id);
                        st.migration_status.remove(&id);
                        st.outgoing.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let hang = {
            let mut st = self.state.lock().unwrap();
            Self::controllable(&st, id)?;
            std::mem::take(&mut st.hang_next_save)
        };
        if hang {
            self.save_reached.notify_one();
            while self.state.lock().unwrap().vms.contains_key(&id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            return Err(DriverError::Qmp(QmpError::Disconnected));
        }
        let mut st = self.state.lock().unwrap();
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "pause"));
        Self::controllable(&st, id)?;
        st.paused.insert(id);
        Ok(())
    }

    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        st.pauses.push((id, "resume"));
        Self::controllable(&st, id)?;
        st.paused.remove(&id);
        st.marked.remove(&id);
        // 마이그레이션을 보낸 원본도 `cont`로 다시 돈다.
        if st.migration_status.get(&id) == Some(&postmigrate()) {
            st.migration_status.remove(&id);
        }
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        let mut st = self.state.lock().unwrap();
        if Self::controllable(&st, id).is_err() {
            return Ok(false);
        }
        if st.marked.remove(&id) {
            st.paused.remove(&id);
        }
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        Ok(Self::version(&self.state.lock().unwrap()))
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        Ok(Self::version(&st))
    }

    /// 범위의 첫 포트를 돌려준다. `fail_next_start`로 주입한 오류로 실패할 수 있다.
    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        _listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&vm.id) {
                return Err(DriverError::Migration(format!(
                    "VM {} is already running on this node",
                    vm.id
                )));
            }
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
            st.incoming.insert(vm.id, task);
            st.migration_status.insert(vm.id, inmigrate());
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(*ports.start())
    }

    /// 주입한 실패가 없으면 받기를 마친 것으로 본다(멈춘 채).
    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains_key(&id) {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        if let Some(err) = st.fail_next_incoming.take() {
            drop(st);
            self.remove(id, EventKind::Crashed);
            return Err(err);
        }
        st.migration_status.insert(id, RuntimeStatus::Paused);
        Ok(())
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if !st.incoming.contains_key(&id) {
            return Ok(());
        }
        if st.migration_status.get(&id) == Some(&inmigrate()) {
            return Err(DriverError::Migration(format!(
                "incoming migration of VM {id} has not completed"
            )));
        }
        st.incoming.remove(&id);
        st.migration_status.remove(&id);
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .incoming
            .iter()
            .filter(|(id, _)| st.vms.contains_key(id))
            .map(|(&id, &task)| (id, task))
            .collect())
    }

    /// 주입한 실패가 없으면 원본을 `postmigrate`로 멈춘다.
    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        _opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().migrate_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        let mut st = self.state.lock().unwrap();
        st.migrations.push((id, target.clone()));
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_migrate.take() {
            return Err(err);
        }
        if let Some(err) = st.fail_next_migrate_unfinished.take() {
            st.outgoing.insert(id);
            return Err(err);
        }
        st.migration_status.insert(id, postmigrate());
        Ok(())
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let migrated = {
            let st = self.state.lock().unwrap();
            st.vms.contains_key(&id) && st.migration_status.get(&id) == Some(&postmigrate())
        };
        if migrated {
            self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            );
        }
        Ok(migrated)
    }

    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        if st.outgoing.remove(&id) && st.complete_on_cancel {
            st.migration_status.insert(id, postmigrate());
        }
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

fn inmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("inmigrate".into())
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}

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
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
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
        hv.start(&vm, None).await.unwrap();
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
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
            hv.pause(vm.id).await,
            hv.resume(vm.id).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);
        hv.pause(vm.id).await.unwrap();
        assert!(hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Paused);
        hv.resume(vm.id).await.unwrap();
        assert!(!hv.is_paused(vm.id));
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60000,
            tls_hostname: "10.0.0.2".into(),
        }
    }

    fn opts() -> MigrateOptions {
        MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(60),
        }
    }

    /// 원본 노드와 대상 노드의 하이퍼바이저로 한 번의 마이그레이션을 흉내 낸다.
    #[tokio::test]
    async fn migration_between_two_mocks() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        assert_eq!(src.qemu_version().await.unwrap(), MOCK_QEMU_VERSION);
        assert!(matches!(
            src.running_qemu_version(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
        src.start(&vm, None).await.unwrap();
        assert_eq!(
            src.running_qemu_version(vm.id).await.unwrap(),
            dst.qemu_version().await.unwrap()
        );

        let mut events = dst.subscribe();
        let port = dst
            .start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        assert_eq!(port, 60000);
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Started);
        assert!(dst.is_incoming(vm.id));
        assert_eq!(dst.list_incoming().await.unwrap(), vec![(vm.id, task)]);
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            st.vms
                .get(&vm.id)
                .map(|&pid| MockHypervisor::state_of(&st, vm.id, pid).status)
        };
        assert_eq!(status(&dst), Some(inmigrate()));
        // 아직 받는 중이면 넘겨받을 수 없다.
        assert!(matches!(
            dst.finish_incoming(vm.id).await,
            Err(DriverError::Migration(_))
        ));
        // 이미 돌고 있는 VM은 받을 수 없다.
        assert!(matches!(
            src.start_incoming(&vm, task, "10.0.0.1", 60000..=60099)
                .await,
            Err(DriverError::Migration(_))
        ));

        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(src.migrations(), vec![(vm.id, target())]);
        assert_eq!(status(&src), Some(postmigrate()));
        dst.incoming_wait(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Paused));
        dst.finish_incoming(vm.id).await.unwrap();
        assert_eq!(status(&dst), Some(RuntimeStatus::Running));
        assert!(!dst.is_incoming(vm.id));
        assert!(dst.list_incoming().await.unwrap().is_empty());
        // 두 번째 넘겨받기는 아무것도 하지 않는다.
        dst.finish_incoming(vm.id).await.unwrap();
        // 받는 중 표시가 없으면 기다릴 것이 없다.
        assert!(matches!(
            dst.incoming_wait(vm.id).await,
            Err(DriverError::Migration(_))
        ));

        // 돌고 있는 VM은 끄지 않는다.
        assert!(!dst.stop_migrated(vm.id).await.unwrap());
        assert!(src.stop_migrated(vm.id).await.unwrap());
        assert_eq!(src.inspect(vm.id).await.unwrap(), None);
        assert!(!src.stop_migrated(vm.id).await.unwrap());
    }

    #[tokio::test]
    async fn failed_migration_keeps_source_and_receiver_exits() {
        let (src, dst) = (MockHypervisor::default(), MockHypervisor::default());
        let vm = spec();
        let task = Uuid::now_v7();
        src.start(&vm, None).await.unwrap();
        src.fail_next_migrate(DriverError::Migration("Connection refused".into()));
        assert!(matches!(
            src.migrate(vm.id, &target(), &opts()).await,
            Err(DriverError::Migration(_))
        ));
        let state = src.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);

        // 받기에 실패한 QEMU는 크래시로 끝난다.
        dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
            .await
            .unwrap();
        let mut events = dst.subscribe();
        dst.fail_next_incoming_wait(DriverError::Migration("TLS handshake failed".into()));
        assert!(dst.incoming_wait(vm.id).await.is_err());
        assert_eq!(events.recv().await.unwrap().kind, EventKind::Crashed);
        assert!(dst.list_incoming().await.unwrap().is_empty());

        // 시작 실패를 주입하면 받기도 실패한다.
        dst.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            dst.start_incoming(&vm, task, "10.0.0.2", 60000..=60099)
                .await,
            Err(DriverError::StartFailed(_))
        ));
        assert_eq!(dst.inspect(vm.id).await.unwrap(), None);

        // 보낸 뒤 확정 전에 실패하면 원본을 이어서 실행한다.
        src.migrate(vm.id, &target(), &opts()).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            postmigrate()
        );
        src.resume(vm.id).await.unwrap();
        assert_eq!(
            src.inspect(vm.id).await.unwrap().unwrap().status,
            RuntimeStatus::Running
        );
        assert!(!src.stop_migrated(vm.id).await.unwrap());

        // 받는 쪽에서 버전을 다르게 보이게 할 수 있다.
        dst.set_qemu_version("9.0.0");
        assert_eq!(dst.qemu_version().await.unwrap(), "9.0.0");
    }

    #[tokio::test]
    async fn cancelled_migration_does_not_complete_later() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let status = |hv: &MockHypervisor| {
            let st = hv.state.lock().unwrap();
            MockHypervisor::state_of(&st, vm.id, st.vms[&vm.id]).status
        };
        // 끝을 기다리지 못한 마이그레이션은 보내는 중으로 남고, 나중에 끝나면 원본이 멈춘다.
        hv.fail_next_migrate_unfinished(DriverError::Timeout("QMP query-migrate"));
        assert!(hv.migrate(vm.id, &target(), &opts()).await.is_err());
        assert!(hv.is_outgoing(vm.id));
        assert_eq!(status(&hv), RuntimeStatus::Running);
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), postmigrate());
        hv.resume(vm.id).await.unwrap();

        // 취소하면 끝나지 않는다.
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert!(!hv.is_outgoing(vm.id));
        hv.complete_outgoing(vm.id);
        assert_eq!(status(&hv), RuntimeStatus::Running);
        // 보내는 중이 아니거나 없는 VM이면 아무것도 하지 않는다.
        hv.cancel_migration(vm.id).await.unwrap();
        hv.cancel_migration(Uuid::now_v7()).await.unwrap();

        // 취소하는 사이에 끝날 수 있다.
        hv.complete_on_cancel();
        hv.start_outgoing(vm.id);
        hv.cancel_migration(vm.id).await.unwrap();
        assert_eq!(status(&hv), postmigrate());
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
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

`crates/opl-driver/src/qemu.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{
    HostProfile, MigrationInfo, MigrationParameters, MigrationState, QmpClient, QmpError, QmpEvent,
    RunState, RuntimePaths, TlsEndpoint, build_cmdline, build_incoming_cmdline,
};
use opl_types::network::tap_prefix;
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
use crate::migration::{
    MIGRATION_TLS_ID, MigrateOptions, MigrateTarget, migration_tls_dir, parse_qemu_version,
    pick_port, tcp_uri,
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
/// 내부 스냅샷(저장·불러오기·삭제) 하나를 기다리는 한도. 메모리 스냅샷은 메모리 크기만큼 걸린다.
/// Task 한도(600초)보다 짧아야 Task가 먼저 끊기지 않고 이 오류(작업이 남아 있을 수 있음)를 받는다.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

/// opl이 스냅샷을 위해 VM을 멈춰 두었다는 표시 파일. VM 실행 디렉터리에 `pause`가 쓰고 `resume`이
/// 지운다. opld가 멈춘 채 죽었으면 다시 붙을 때 이 표시를 보고 이어서 실행한다.
const PAUSED_MARK: &str = "paused-by-opl";

/// 라이브 마이그레이션을 받는 QEMU라는 표시 파일(내용은 마이그레이션 Task id). `start_incoming`이
/// QEMU를 띄우기 전에 실행 디렉터리에 쓰고 `finish_incoming`이 지운다. QEMU가 끝나면 실행 디렉터리와
/// 함께 사라진다.
const INCOMING_MARK: &str = "incoming";
/// 마이그레이션 상태를 다시 묻는 간격(`MIGRATION` 이벤트를 놓쳐도 이만큼 뒤에 안다)
const MIGRATION_POLL: Duration = Duration::from_secs(1);
/// 원본이 끝난 뒤 받는 쪽이 마지막 장치 상태를 다 읽기를 기다리는 한도
const INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
/// 시간 제한으로 `migrate_cancel`한 뒤 마이그레이션이 끝나기를 기다리는 한도
const CANCEL_WAIT: Duration = Duration::from_secs(30);

/// VM별 상태 디렉터리 `{state_dir}/vm/{id}`. `state_dir`은 [`QemuConfig::state_dir`]이다.
pub fn vm_state_dir(state_dir: &Path, id: Uuid) -> PathBuf {
    state_dir.join("vm").join(id.to_string())
}

/// VM의 UEFI 변수 파일. UEFI VM을 처음 띄울 때 템플릿을 복사해 만든다(BIOS VM에는 없다).
pub fn uefi_vars_path(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_state_dir(state_dir, id).join("uefi-vars.fd")
}

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
    /// `pause`로 멈추고 아직 `resume`을 부르지 않은 VM. 다시 붙어도 이어서 실행하지 않는다.
    pausing: Mutex<HashSet<Uuid>>,
    /// 받을 포트를 고르고 QEMU가 그 포트를 열 때까지 쥔다. 동시에 받는 두 VM이 같은 포트를 고르지 않게 한다.
    incoming_ports: tokio::sync::Mutex<()>,
}

/// QEMU를 띄우는 방식
#[derive(Clone, Copy)]
enum Launch<'a> {
    /// 띄우고(스냅샷 태그가 있으면 불러온 뒤) 이어서 실행한다.
    Start { restore: Option<&'a str> },
    /// `-incoming defer`로 띄우고 받는 중 표시(마이그레이션 Task)를 남긴다. 멈춘 채 둔다.
    Incoming { task: Uuid },
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
            pausing: Mutex::default(),
            incoming_ports: tokio::sync::Mutex::new(()),
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
                    tracing::warn!(%id, "cleaning up stale runtime dir");
                    // 런타임 디렉터리가 정리할 것이 남았다는 유일한 기록이므로 tap부터 지운다.
                    // 지우지 못하면 디렉터리를 남겨 다음 new()가 다시 시도한다.
                    if delete_taps(hv.config.net.as_deref(), id).await
                        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
                    {
                        tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                    }
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
        paths.uefi_vars = uefi_vars_path(&self.config.state_dir, id);
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        vm_state_dir(&self.config.state_dir, id)
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

    async fn launch(
        &self,
        vm: &VmRuntimeSpec,
        paths: &RuntimePaths,
        how: Launch<'_>,
    ) -> Result<(), DriverError> {
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

        let args = match how {
            Launch::Start { .. } => build_cmdline(vm, &self.config.host, paths),
            Launch::Incoming { .. } => build_incoming_cmdline(vm, &self.config.host, paths),
        };
        let mut args = args.map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        if let Launch::Incoming { task } = how {
            // 띄우는 도중에 opld가 죽어도 남은 QEMU가 받는 쪽임을 알 수 있게 먼저 쓴다.
            std::fs::write(paths.dir.join(INCOMING_MARK), task.to_string())?;
        }
        self.create_taps(vm).await?;
        let mut cmd = if self.config.systemd_scope {
            clear_stale_scope(vm.id).await;
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
        let restore = match how {
            Launch::Start { restore } => restore,
            // 받기를 마칠 때까지 멈춰 둔다.
            Launch::Incoming { .. } => return Ok(()),
        };
        if let Some(tag) = restore {
            // -S로 멈춘 채 불러온 뒤 cont로 이어서 실행한다. 메모리 상태는 첫 디스크(disk0)에 있다.
            let devices: Vec<String> = (0..vm.disks.len()).map(|i| format!("disk{i}")).collect();
            let load = job(
                handle
                    .qmp
                    .snapshot_load(tag, "disk0", &devices, SNAPSHOT_TIMEOUT),
                "QMP snapshot-load",
            )
            .await;
            if let Err(e) = load {
                drop(handle);
                // 작업이 끝나지 않았으면 QMP가 답하지 않을 수 있으므로 끝날 때까지 기다리고 필요하면 죽인다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop QEMU after snapshot load failed");
                }
                return Err(e);
            }
        }
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
        // create_tap은 같은 이름의 장치를 먼저 지우므로, 이 VM의 이름이 아니면 호스트 인터페이스를 지울 수 있다.
        let prefix = tap_prefix(vm.id);
        if let Some(tap) = taps.iter().find(|t| !t.ifname.starts_with(&prefix)) {
            return Err(DriverError::InvalidSpec(format!(
                "tap name {:?} does not start with {prefix:?}",
                tap.ifname
            )));
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
        // tap부터 지운다. 지우지 못하면 런타임 디렉터리를 남겨 다음 new()가 다시 시도한다.
        if delete_taps(self.config.net.as_deref(), id).await
            && let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
        true
    }

    /// QMP로 다룰 수 있는 실행 중인 VM의 핸들
    fn running_handle(&self, id: Uuid) -> Result<Arc<VmHandle>, DriverError> {
        if let Some(handle) = self.handle(id) {
            return Ok(handle);
        }
        match liveness(&self.paths(id)?.pidfile) {
            Liveness::Dead => Err(DriverError::NotRunning(id)),
            // 살아 있지만 QMP로 다룰 수 없다. 꺼졌다고 알리면 호출자가 쓰고 있는 디스크를 건드린다.
            // 요청을 보내지 않았으므로 시간 초과(작업이 남아 있을 수 있음)와 구별한다.
            Liveness::Alive(_) | Liveness::Unknown => Err(DriverError::NotAttached(id)),
        }
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 스냅샷을 위해
    /// 멈춘 채([`PAUSED_MARK`]) opld가 죽었거나 이어서 실행하지 못한 VM도 멈춘 채 남는다. 둘 다 이어서
    /// 실행한다. 지금 `pause`로 멈춰 둔 VM과 QEMU가 스스로 멈춘 VM(표시 없음)은 그대로 둔다.
    async fn continue_if_left_stopped(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let mark = handle.paths.dir.join(PAUSED_MARK);
        let ours = !self.pausing.lock().unwrap().contains(&id) && mark.exists();
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        let state = flatten(status, "QMP query-status")?;
        if state == RunState::Prelaunch || (ours && state == RunState::Paused) {
            tracing::info!(%id, ?state, "continuing VM left stopped");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        if ours {
            remove_if_exists(&mark)?;
        }
        Ok(())
    }

    /// 마이그레이션 TLS 자격 증명(`{state_dir}/migration-tls`)과 능력·매개변수를 정한다. 자격 증명
    /// 객체는 파일을 만들 때 읽으므로, 인증서가 바뀌었을 수 있어 매번 지우고 다시 만든다.
    async fn setup_migration(
        &self,
        qmp: &QmpClient,
        endpoint: TlsEndpoint,
        tls_hostname: Option<&str>,
        bandwidth: Option<u64>,
    ) -> Result<(), DriverError> {
        // 처음이면 없어서 실패한다.
        let _ = tokio::time::timeout(QMP_TIMEOUT, qmp.object_del(MIGRATION_TLS_ID)).await;
        let dir = migration_tls_dir(&self.config.state_dir);
        let add = qmp.add_tls_creds(MIGRATION_TLS_ID, &dir, endpoint);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, add).await,
            "QMP object-add",
        )?;
        let caps = qmp.migrate_set_capabilities(&[("events", true), ("auto-converge", true)]);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, caps).await,
            "QMP migrate-set-capabilities",
        )?;
        let params = MigrationParameters {
            tls_creds: Some(MIGRATION_TLS_ID.into()),
            tls_hostname: tls_hostname.map(str::to_string),
            max_bandwidth: bandwidth,
        };
        let set = qmp.migrate_set_parameters(&params);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, set).await,
            "QMP migrate-set-parameters",
        )
    }

    /// 띄운 받는 QEMU에 마이그레이션 설정을 하고 빈 포트에서 받기 시작한다.
    async fn listen_incoming(
        &self,
        id: Uuid,
        host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let handle = self.running_handle(id)?;
        self.setup_migration(&handle.qmp, TlsEndpoint::Server, None, None)
            .await?;
        let _ports = self.incoming_ports.lock().await;
        let Some(port) = pick_port(host, ports.clone()) else {
            return Err(DriverError::Migration(format!(
                "no free migration port in {}-{} on {host}",
                ports.start(),
                ports.end()
            )));
        };
        let uri = tcp_uri(host, port);
        let listen = handle.qmp.migrate_incoming(&uri);
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, listen).await,
            "QMP migrate-incoming",
        )?;
        Ok(port)
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
            Ok(Ok(RunState::InMigrate)) => RuntimeStatus::Other("inmigrate".into()),
            Ok(Ok(RunState::PostMigrate)) => RuntimeStatus::Other("postmigrate".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        // 실행 중인데 지금 멈춰 두지 않았으면 표시는 남은 것이다(이어서 실행했지만 지우지 못했거나
        // 누가 `cont`함). 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 다시 붙을 때 이어서 실행한다.
        if status == RuntimeStatus::Running && !self.pausing.lock().unwrap().contains(&id) {
            let mark = handle.paths.dir.join(PAUSED_MARK);
            if let Err(e) = remove_if_exists(&mark) {
                tracing::warn!(%id, error = %e, "cannot remove paused mark");
            }
        }
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        // 이미 살아 있는 VM에는 restore를 쓰지 않는다. -S로 남은 VM도 그대로 이어서 실행한다.
        if self.handle(vm.id).is_some() {
            return self.continue_if_left_stopped(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    return self.continue_if_left_stopped(vm.id).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(id = %vm.id, error = %e, "could not reattach to running VM")
                }
                Err(_) => tracing::error!(id = %vm.id, "timed out reattaching to running VM"),
            }
            return Err(DriverError::Timeout("reattach to running QEMU"));
        }
        if restore.is_some() && vm.disks.is_empty() {
            return Err(DriverError::InvalidSpec(
                "a snapshot to restore needs a disk for its memory state".into(),
            ));
        }
        if let Err(e) = self.launch(vm, &paths, Launch::Start { restore }).await {
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

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        let Some(vmstate) = devices.first() else {
            return Err(DriverError::InvalidSpec(format!("VM {id} has no disks")));
        };
        if memory {
            let save = handle
                .qmp
                .snapshot_save(tag, vmstate, &devices, SNAPSHOT_TIMEOUT);
            job(save, "QMP snapshot-save").await
        } else {
            let r =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.snapshot_disks(tag, &devices))
                    .await;
            flatten(r, "QMP transaction")
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        if devices.is_empty() {
            return Ok(());
        }
        let delete = handle.qmp.snapshot_delete(tag, &devices, SNAPSHOT_TIMEOUT);
        job(delete, "QMP snapshot-delete").await
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let node = format!("disk{disk}");
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.block_resize(&node, size_bytes)).await;
        flatten(r, "QMP block_resize")
    }

    /// 멈추기 전에 표시([`PAUSED_MARK`])를 남긴다.
    async fn pause(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        self.pausing.lock().unwrap().insert(id);
        std::fs::write(handle.paths.dir.join(PAUSED_MARK), b"")?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.stop()).await;
        flatten(r, "QMP stop")
    }

    /// 이어서 실행한 뒤 표시를 지운다. 실패하면 표시가 남아 다시 붙을 때(`start`) 이어서 실행한다.
    async fn resume(&self, id: Uuid) -> Result<(), DriverError> {
        self.pausing.lock().unwrap().remove(&id);
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        flatten(r, "QMP cont")?;
        remove_if_exists(&handle.paths.dir.join(PAUSED_MARK))?;
        Ok(())
    }

    async fn resume_left_stopped(&self, id: Uuid) -> Result<bool, DriverError> {
        if self.handle(id).is_none() {
            return Ok(false);
        }
        self.continue_if_left_stopped(id).await?;
        Ok(true)
    }

    async fn qemu_version(&self) -> Result<String, DriverError> {
        let output = tokio::process::Command::new(&self.config.host.qemu_binary)
            .arg("--version")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(QMP_TIMEOUT, output)
            .await
            .map_err(|_| DriverError::Timeout("qemu --version"))??;
        let stdout = String::from_utf8_lossy(&output.stdout);
        match parse_qemu_version(&stdout) {
            Some(v) if output.status.success() => Ok(v),
            _ => Err(DriverError::ToolFailed {
                tool: "qemu --version",
                stderr: format!(
                    "{}{}",
                    stdout.trim(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
        }
    }

    async fn running_qemu_version(&self, id: Uuid) -> Result<String, DriverError> {
        let handle = self.running_handle(id)?;
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_version()).await;
        flatten(r, "QMP query-version")
    }

    async fn start_incoming(
        &self,
        vm: &VmRuntimeSpec,
        task: Uuid,
        listen_host: &str,
        ports: RangeInclusive<u16>,
    ) -> Result<u16, DriverError> {
        let paths = self.paths(vm.id)?;
        if self.handle(vm.id).is_some() || liveness(&paths.pidfile) != Liveness::Dead {
            return Err(DriverError::Migration(format!(
                "VM {} is already running on this node",
                vm.id
            )));
        }
        if let Err(e) = self.launch(vm, &paths, Launch::Incoming { task }).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            } else if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
            }
            return Err(e);
        }
        match self.listen_incoming(vm.id, listen_host, ports).await {
            Ok(port) => Ok(port),
            Err(e) => {
                // QEMU 하나는 한 번만 받을 수 있으므로 버린다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop incoming QEMU");
                }
                Err(e)
            }
        }
    }

    async fn incoming_wait(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        if !handle.paths.dir.join(INCOMING_MARK).exists() {
            return Err(DriverError::Migration(format!(
                "VM {id} is not receiving a migration"
            )));
        }
        let wait = handle.qmp.wait_migration(MIGRATION_POLL);
        match tokio::time::timeout(INCOMING_WAIT_TIMEOUT, wait).await {
            Err(_) => Err(DriverError::Timeout("incoming migration to complete")),
            // 받기에 실패한 QEMU는 스스로 끝난다.
            Ok(Err(QmpError::Disconnected)) => {
                Err(DriverError::Migration("the receiving QEMU exited".into()))
            }
            Ok(Err(e)) => Err(e.into()),
            Ok(Ok(info)) => migration_result(info),
        }
    }

    async fn finish_incoming(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let mark = handle.paths.dir.join(INCOMING_MARK);
        if !mark.exists() {
            return Ok(());
        }
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        match flatten(status, "QMP query-status")? {
            RunState::Paused => {
                tracing::info!(%id, "continuing received VM");
                let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
                flatten(cont, "QMP cont")?;
            }
            RunState::Running => {}
            state => {
                return Err(DriverError::Migration(format!(
                    "incoming migration of VM {id} has not completed (QEMU is {state:?})"
                )));
            }
        }
        remove_if_exists(&mark)?;
        Ok(())
    }

    async fn list_incoming(&self) -> Result<Vec<(Uuid, Uuid)>, DriverError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = self.paths(id)?;
            let mark = paths.dir.join(INCOMING_MARK);
            if mark.exists() && liveness(&paths.pidfile) != Liveness::Dead {
                // 읽을 수 없으면 어느 마이그레이션과도 맞지 않는 nil로 둔다(정리가 끈다).
                let task = std::fs::read_to_string(&mark)
                    .ok()
                    .and_then(|s| Uuid::parse_str(s.trim()).ok())
                    .unwrap_or_default();
                out.push((id, task));
            }
        }
        Ok(out)
    }

    async fn migrate(
        &self,
        id: Uuid,
        target: &MigrateTarget,
        opts: &MigrateOptions,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let qmp = &handle.qmp;
        self.setup_migration(
            qmp,
            TlsEndpoint::Client,
            Some(&target.tls_hostname),
            opts.bandwidth,
        )
        .await?;
        let uri = tcp_uri(&target.host, target.port);
        tracing::info!(%id, %uri, "migrating VM");
        flatten(
            tokio::time::timeout(QMP_TIMEOUT, qmp.migrate(&uri)).await,
            "QMP migrate",
        )?;
        let info =
            match tokio::time::timeout(opts.timeout, qmp.wait_migration(MIGRATION_POLL)).await {
                Ok(r) => r?,
                Err(_) => {
                    tracing::warn!(%id, timeout = ?opts.timeout, "migration timed out; cancelling");
                    let cancel = tokio::time::timeout(QMP_TIMEOUT, qmp.migrate_cancel()).await;
                    flatten(cancel, "QMP migrate_cancel")?;
                    let wait = qmp.wait_migration(MIGRATION_POLL);
                    let info = flatten(
                        tokio::time::timeout(CANCEL_WAIT, wait).await,
                        "migration to cancel",
                    )?;
                    // 취소하는 사이에 끝났으면 그대로 성공이다.
                    if info.state != MigrationState::Completed {
                        return Err(DriverError::Migration(format!(
                            "did not complete within {:?}; cancelled",
                            opts.timeout
                        )));
                    }
                    info
                }
            };
        migration_result(info)
    }

    async fn stop_migrated(&self, id: Uuid) -> Result<bool, DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(false);
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? != RunState::PostMigrate {
            return Ok(false);
        }
        drop(handle);
        tracing::info!(%id, "stopping migrated source QEMU");
        self.stop(id, StopMode::Force).await?;
        Ok(true)
    }

    async fn cancel_migration(&self, id: Uuid) -> Result<(), DriverError> {
        let handle = match self.running_handle(id) {
            Ok(handle) => handle,
            Err(DriverError::NotRunning(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        // 받는 QEMU의 마이그레이션은 보내는 쪽이 다룬다.
        if handle.paths.dir.join(INCOMING_MARK).exists() {
            return Ok(());
        }
        let info = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_migrate()).await;
        let info = flatten(info, "QMP query-migrate")?;
        if info.state == MigrationState::None || info.state.is_finished() {
            return Ok(());
        }
        tracing::warn!(%id, state = ?info.state, "cancelling an outgoing migration");
        let cancel = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.migrate_cancel()).await;
        flatten(cancel, "QMP migrate_cancel")?;
        let wait = handle.qmp.wait_migration(MIGRATION_POLL);
        flatten(
            tokio::time::timeout(CANCEL_WAIT, wait).await,
            "migration to cancel",
        )?;
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// 끝난 마이그레이션의 결과
fn migration_result(info: MigrationInfo) -> Result<(), DriverError> {
    match info.state {
        MigrationState::Completed => Ok(()),
        MigrationState::Cancelled => Err(DriverError::Migration("migration was cancelled".into())),
        state => Err(DriverError::Migration(
            info.error
                .unwrap_or_else(|| format!("migration ended as {state:?}")),
        )),
    }
}

/// 실행 중인 VM의 디스크 노드 이름(`disk{i}`)을 디스크 순서로 읽는다. 다시 붙은 VM은 스펙을
/// 모르므로 QEMU에 묻는다.
async fn query_disk_nodes(qmp: &QmpClient) -> Result<Vec<String>, DriverError> {
    let r = tokio::time::timeout(QMP_TIMEOUT, qmp.execute("query-named-block-nodes", None)).await;
    Ok(disk_nodes(&flatten(r, "QMP query-named-block-nodes")?))
}

/// 블록 노드 목록에서 cmdline이 디스크 포맷 노드에 붙인 `disk{i}`만 골라 i 순서로 돌려준다.
fn disk_nodes(nodes: &serde_json::Value) -> Vec<String> {
    let mut disks: Vec<(usize, String)> = nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["node-name"].as_str())
        .filter_map(|name| {
            let i = name.strip_prefix("disk")?.parse().ok()?;
            Some((i, name.to_string()))
        })
        .collect();
    disks.sort();
    disks.into_iter().map(|(_, name)| name).collect()
}

/// QMP 작업 명령을 기다린다. 작업의 제한 시간은 이벤트를 기다리는 동안에만 걸리므로, QEMU가
/// 명령 자체에 답하지 않아도(SIGSTOP 등) 묶이지 않게 한 번 더 감싼다.
async fn job(
    fut: impl Future<Output = Result<(), QmpError>>,
    what: &'static str,
) -> Result<(), DriverError> {
    flatten(
        tokio::time::timeout(SNAPSHOT_TIMEOUT + QMP_TIMEOUT, fut).await,
        what,
    )
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
    // (종료 종류, 프로세스가 끝나 런타임 디렉터리를 지워도 되는지)
    let (kind, exited) = match guest_initiated {
        // SHUTDOWN을 받았으면 종료는 확정이다. QEMU는 소켓을 닫고 곧 끝나므로 잠깐 기다려
        // 디렉터리를 지우되, 플러시 등으로 늦어지면 잠금이 걸린 디렉터리는 남겨 두고
        // (다음 launch/new()가 잠금 해제를 확인하고 정리한다) 종료는 바로 알린다.
        Some(guest_initiated) => {
            let exited = wait_until(SHUTDOWN_EXIT_WAIT, dead).await;
            if !exited {
                tracing::info!(%id, "QEMU is still exiting; leaving runtime dir");
            }
            (EventKind::Stopped { guest_initiated }, exited)
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
            (EventKind::Crashed, true)
        }
    };
    // 등록을 풀기 전에 tap을 지운다. QEMU가 아직 끝나는 중이어도 tap은 곧바로 지워진다(남은 fd는
    // 오류를 받는다). 게스트는 이미 꺼졌으므로 괜찮다. 런타임 디렉터리는 정리할 것이 남았다는
    // 유일한 기록이므로 tap을 지운 뒤에 지운다. 그 전에 끊기거나(드라이버가 내려가 이 태스크가
    // abort됨) tap을 지우지 못하면 디렉터리가 남아 다음 launch/new()가 다시 정리한다.
    if delete_taps(net.as_deref(), id).await
        && exited
        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
    {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// VM의 tap을 지운다. 지웠거나 지울 것이 없으면 true. 실패하면 기록하고 false를 돌려주며,
/// 호출자는 런타임 디렉터리를 남겨 다음 new()가 다시 지우게 한다.
async fn delete_taps(net: Option<&dyn HostNet>, id: Uuid) -> bool {
    if let Some(net) = net
        && let Err(e) = net.delete_vm_taps(id).await
    {
        tracing::warn!(%id, error = %e, "failed to delete tap devices");
        return false;
    }
    true
}

/// 끝난 QEMU의 `--collect` 스코프는 systemd가 비동기로 회수하므로, 곧바로 다시 띄우면
/// systemd-run이 "Unit ... already exists"로 실패한다. 호출자가 이전 QEMU가 없음을 확인했으므로
/// 남은 스코프를 멈추고 실패 기록을 지운다. 오류와 출력은 무시한다.
async fn clear_stale_scope(id: Uuid) {
    let unit = format!("opl-vm-{id}.scope");
    for verb in ["stop", "reset-failed"] {
        let status = tokio::process::Command::new("systemctl")
            .args([verb, "--quiet", &unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(KILL_TIMEOUT, status).await;
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

    use opl_qemu::Accel;
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{Arch, CpuModel, CpuSpec, MachineType, NicModel};

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

        let err = hv.start(&vm, None).await.unwrap_err();
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
        /// 받은 명령의 인자(`commands`와 같은 순서)
        arguments: Arc<Mutex<Vec<serde_json::Value>>>,
        /// 보내면 서버가 (주어진 이벤트를 먼저 보낸 뒤) 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        fake_qmp_with(dir, status, "completed").await
    }

    /// [`fake_qmp`]에 더해 `query-migrate`에 `migration` 상태로 답한다(`failed`면 `error-desc`도).
    /// `migrate_cancel`을 받은 뒤에는 `cancelled`로 답한다.
    async fn fake_qmp_with(dir: &Path, status: &'static str, migration: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let args = Arc::new(Mutex::new(Vec::new()));
        let arguments = args.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            let mut migration = migration;
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
                    "query-version" => serde_json::json!({
                        "qemu": { "major": 8, "minor": 2, "micro": 2 }, "package": ""
                    }),
                    "migrate_cancel" => {
                        migration = "cancelled";
                        serde_json::json!({})
                    }
                    "query-migrate" if migration == "failed" => serde_json::json!({
                        "status": "failed", "error-desc": "Connection refused"
                    }),
                    "query-migrate" => serde_json::json!({ "status": migration }),
                    // 디스크 두 개를 가진 VM의 블록 노드
                    "query-named-block-nodes" => serde_json::json!([
                        { "node-name": "disk1" },
                        { "node-name": "disk1-file" },
                        { "node-name": "disk0" },
                        { "node-name": "disk0-file" },
                    ]),
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                arguments.lock().unwrap().push(cmd["arguments"].clone());
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
            arguments: args,
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

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    /// 스냅샷을 위해 멈춘 채 opld가 죽은 VM은 다시 붙을 때 이어서 실행하고 표시를 지운다.
    #[tokio::test]
    async fn start_continues_vm_left_paused_for_snapshots() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        std::fs::write(dir.join(PAUSED_MARK), b"").unwrap();
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
        assert!(!dir.join(PAUSED_MARK).exists());
    }

    /// 표시 없이 멈춘 VM(QEMU가 스스로 멈춤)은 그대로 둔다.
    #[tokio::test]
    async fn start_leaves_vm_paused_by_qemu() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "paused").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.start(&vm, None).await.unwrap();
        assert!(!fake.commands.lock().unwrap().contains(&"cont".to_string()));
    }

    fn target() -> MigrateTarget {
        MigrateTarget {
            host: "10.0.0.2".into(),
            port: 60005,
            tls_hostname: "node-b".into(),
        }
    }

    /// 보내는 쪽: TLS 자격 증명(client)을 다시 만들고, 능력·매개변수를 정한 뒤 보내고 끝을 기다린다.
    #[tokio::test]
    async fn migrate_sets_up_tls_and_waits_for_completion() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let state_dir = cfg.state_dir.clone();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        assert_eq!(hv.running_qemu_version(vm.id).await.unwrap(), "8.2.2");
        let opts = MigrateOptions {
            bandwidth: Some(64 << 20),
            timeout: Duration::from_secs(10),
        };
        hv.migrate(vm.id, &target(), &opts).await.unwrap();

        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        assert!(at("object-del") < at("object-add"));
        assert_eq!(
            arguments[at("object-del")],
            serde_json::json!({ "id": "opl-mig-tls" })
        );
        let add = &arguments[at("object-add")];
        assert_eq!(add["qom-type"], "tls-creds-x509");
        assert_eq!(add["endpoint"], "client");
        assert_eq!(
            add["dir"],
            state_dir.join("migration-tls").display().to_string()
        );
        assert_eq!(
            arguments[at("migrate-set-capabilities")]["capabilities"],
            serde_json::json!([
                { "capability": "events", "state": true },
                { "capability": "auto-converge", "state": true }
            ])
        );
        assert_eq!(
            arguments[at("migrate-set-parameters")],
            serde_json::json!({
                "tls-creds": "opl-mig-tls", "tls-hostname": "node-b", "max-bandwidth": 64u64 << 20
            })
        );
        assert!(at("migrate-set-parameters") < at("migrate"));
        assert_eq!(
            arguments[at("migrate")],
            serde_json::json!({ "uri": "tcp:10.0.0.2:60005" })
        );
        assert!(at("migrate") < at("query-migrate"));
    }

    #[tokio::test]
    async fn failed_migration_reports_qemu_error() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp_with(&dir, "running", "failed").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_secs(10),
        };
        match hv.migrate(vm.id, &target(), &opts).await.unwrap_err() {
            DriverError::Migration(msg) => assert_eq!(msg, "Connection refused"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// 시간 제한이 지나면 취소하고 실패로 끝낸다.
    #[tokio::test]
    async fn migration_is_cancelled_after_timeout() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp_with(&dir, "running", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let opts = MigrateOptions {
            bandwidth: None,
            timeout: Duration::from_millis(200),
        };
        let err = hv.migrate(vm.id, &target(), &opts).await.unwrap_err();
        assert!(
            matches!(&err, DriverError::Migration(m) if m.contains("cancelled")),
            "{err:?}"
        );
        assert!(!err.is_transient());
        let commands = fake.commands.lock().unwrap().clone();
        assert!(commands.contains(&"migrate_cancel".to_string()));
        // 보내지 않은 매개변수는 빠진다.
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = commands
            .iter()
            .position(|c| c == "migrate-set-parameters")
            .unwrap();
        assert!(arguments[at].get("max-bandwidth").is_none());
    }

    /// 보내는 중인 마이그레이션만 취소하고 끝(`cancelled`)을 기다린다.
    #[tokio::test]
    async fn cancel_migration_cancels_only_an_outgoing_migration() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (sending, idle, receiving) = (spec(), spec(), spec());
        let dirs: Vec<PathBuf> = [&sending, &idle, &receiving]
            .iter()
            .map(|vm| cfg.run_dir.join("vm").join(vm.id.to_string()))
            .collect();
        let mut qemus = Vec::new();
        for dir in &dirs {
            qemus.push(hold_pidfile(dir).await);
        }
        std::fs::write(dirs[2].join(INCOMING_MARK), b"").unwrap();
        let fake_sending = fake_qmp_with(&dirs[0], "running", "active").await;
        let fake_idle = fake_qmp_with(&dirs[1], "running", "none").await;
        let fake_receiving = fake_qmp_with(&dirs[2], "inmigrate", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        hv.cancel_migration(sending.id).await.unwrap();
        let commands = fake_sending.commands.lock().unwrap().clone();
        let cancel = commands.iter().position(|c| c == "migrate_cancel").unwrap();
        assert!(commands[cancel + 1..].contains(&"query-migrate".to_string()));

        hv.cancel_migration(idle.id).await.unwrap();
        hv.cancel_migration(receiving.id).await.unwrap();
        hv.cancel_migration(Uuid::now_v7()).await.unwrap();
        for fake in [&fake_idle, &fake_receiving] {
            let commands = fake.commands.lock().unwrap().clone();
            assert!(
                !commands.contains(&"migrate_cancel".to_string()),
                "{commands:?}"
            );
        }
    }

    /// 받은 VM은 멈춰 있으면 이어서 실행하고 표시를 지운다. 아직 받는 중이면 건드리지 않는다.
    #[tokio::test]
    async fn finish_incoming_continues_only_received_vms() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let (received, receiving) = (spec(), spec());
        let dir_a = cfg.run_dir.join("vm").join(received.id.to_string());
        let dir_b = cfg.run_dir.join("vm").join(receiving.id.to_string());
        let _qa = hold_pidfile(&dir_a).await;
        let _qb = hold_pidfile(&dir_b).await;
        // 표시에는 마이그레이션 Task가 있다. 읽을 수 없으면 nil이다.
        let task = Uuid::now_v7();
        std::fs::write(dir_a.join(INCOMING_MARK), format!("{task}\n")).unwrap();
        std::fs::write(dir_b.join(INCOMING_MARK), b"").unwrap();
        let fake_a = fake_qmp(&dir_a, "paused").await;
        let fake_b = fake_qmp_with(&dir_b, "inmigrate", "active").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mut incoming = hv.list_incoming().await.unwrap();
        incoming.sort();
        let mut expected = vec![(received.id, task), (receiving.id, Uuid::nil())];
        expected.sort();
        assert_eq!(incoming, expected);
        // 받는 중인 VM은 reconciler의 이어 가기에도 이어서 실행되지 않는다.
        assert_eq!(
            hv.inspect(receiving.id).await.unwrap().unwrap().status,
            RuntimeStatus::Other("inmigrate".into())
        );
        hv.resume_left_stopped(receiving.id).await.unwrap();
        hv.resume_left_stopped(received.id).await.unwrap();
        assert!(
            !fake_a
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );

        hv.incoming_wait(received.id).await.unwrap();
        hv.finish_incoming(received.id).await.unwrap();
        assert!(
            fake_a
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );
        assert!(!dir_a.join(INCOMING_MARK).exists());
        assert_eq!(
            hv.list_incoming().await.unwrap(),
            vec![(receiving.id, Uuid::nil())]
        );
        // 표시가 없으면 아무것도 하지 않고, 기다릴 것도 없다.
        hv.finish_incoming(received.id).await.unwrap();
        assert!(matches!(
            hv.incoming_wait(received.id).await,
            Err(DriverError::Migration(_))
        ));

        let err = hv.finish_incoming(receiving.id).await.unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir_b.join(INCOMING_MARK).exists());
        assert!(
            !fake_b
                .commands
                .lock()
                .unwrap()
                .contains(&"cont".to_string())
        );
    }

    /// 보낸 원본(`postmigrate`)이 아니면 끄지 않는다.
    #[tokio::test]
    async fn stop_migrated_leaves_running_vm_alone() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!hv.stop_migrated(vm.id).await.unwrap());
        assert!(!hv.stop_migrated(Uuid::now_v7()).await.unwrap());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    /// 이미 이 노드에서 도는 VM은 받지 않는다(그 QEMU를 건드리지 않는다).
    #[tokio::test]
    async fn start_incoming_refuses_a_running_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(!dir.join(INCOMING_MARK).exists());
        assert!(!fake.commands.lock().unwrap().contains(&"quit".to_string()));
    }

    #[tokio::test]
    async fn start_incoming_failure_cleans_run_dir() {
        let tmp = short_tempdir();
        // 곧바로 실패로 끝나는 QEMU
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("false")))
            .await
            .unwrap();
        let vm = spec();
        let err = hv
            .start_incoming(&vm, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::StartFailed(_)), "{err:?}");
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        assert!(hv.list_incoming().await.unwrap().is_empty());
    }

    #[test]
    fn disk_nodes_are_format_nodes_in_disk_order() {
        let nodes = serde_json::json!([
            { "node-name": "disk10" },
            { "node-name": "disk2-file" },
            { "node-name": "disk2" },
            { "node-name": "#block123" },
            { "node-name": "diskx" },
            { "node-name": "disk0" },
        ]);
        assert_eq!(disk_nodes(&nodes), ["disk0", "disk2", "disk10"]);
    }

    #[tokio::test]
    async fn snapshot_ops_use_disk_node_names() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        // 디스크만 스냅샷은 모든 디스크를 transaction 하나로 찍는다.
        hv.snapshot_save(vm.id, "s1", false).await.unwrap();
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        let actions = &arguments[at("transaction")]["actions"];
        assert_eq!(actions[0]["data"]["device"], "disk0");
        assert_eq!(actions[1]["data"]["device"], "disk1");
        assert_eq!(actions[1]["data"]["name"], "s1");
        assert_eq!(
            arguments[at("block_resize")],
            serde_json::json!({ "node-name": "disk1", "size": 2u64 << 30 })
        );
    }

    /// 실행 중인데 지금 멈춰 두지 않은 VM의 표시는 남은 것이다(이어서 실행했지만 지우지 못함, 누가
    /// `cont`함). 상태를 보면 지운다. 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 이어서 실행한다.
    #[tokio::test]
    async fn running_vm_drops_a_left_over_paused_mark() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let _fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mark = dir.join(PAUSED_MARK);

        // 멈춰 두는 동안에는 지우지 않는다(가짜 QMP는 늘 running이라고 답한다).
        hv.pause(vm.id).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap();
        assert!(mark.exists());
        hv.pausing.lock().unwrap().clear();
        let state = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(state.status, RuntimeStatus::Running);
        assert!(!mark.exists());
    }

    #[tokio::test]
    async fn pause_and_resume_send_stop_and_cont() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let mark = dir.join(PAUSED_MARK);
        hv.pause(vm.id).await.unwrap();
        assert!(mark.exists());
        // 멈춰 둔 동안 reconciler가 다시 붙어도(start) 이어서 실행하지 않는다.
        hv.start(&vm, None).await.unwrap();
        assert!(mark.exists());
        hv.resume(vm.id).await.unwrap();
        assert!(!mark.exists());
        let commands = fake.commands.lock().unwrap().clone();
        let sent: Vec<&str> = commands
            .iter()
            .map(String::as_str)
            .filter(|c| ["stop", "cont"].contains(c))
            .collect();
        assert_eq!(sent, ["stop", "cont"]);

        let err = hv.pause(Uuid::now_v7()).await.unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");
    }

    #[tokio::test]
    async fn snapshot_ops_on_vm_that_is_not_attached() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let err = hv
            .snapshot_save(Uuid::now_v7(), "s", true)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");

        // 살아 있지만 QMP로 다룰 수 없는 VM을 꺼진 것으로 알리면 호출자가 쓰고 있는 디스크를
        // qemu-img로 건드린다. 다시 시도할 수 있는 오류로 끝낸다.
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        for err in [
            hv.snapshot_save(vm.id, "s", true).await.unwrap_err(),
            hv.snapshot_delete(vm.id, "s").await.unwrap_err(),
            hv.block_resize(vm.id, 0, 1 << 30).await.unwrap_err(),
            hv.pause(vm.id).await.unwrap_err(),
            hv.resume(vm.id).await.unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::NotAttached(_)), "{err:?}");
            assert!(err.is_transient());
        }
    }

    #[tokio::test]
    async fn restore_without_disks_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        // 메모리 상태는 첫 디스크에 있으므로 디스크가 없으면 띄우기 전에 거부한다.
        let err = hv.start(&spec(), Some("s1")).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
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

        let err = hv.start(&vm, None).await.unwrap_err();
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
                ifname: opl_types::network::tap_name(vm.id, 0),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn tap_name_of_another_vm_or_host_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        // create_tap이 같은 이름을 먼저 지우므로 호스트 인터페이스 이름은 받으면 안 된다.
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        match hv.start(&vm, None).await.unwrap_err() {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("eth0"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
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
            hv.start(&vm, None).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    async fn deactivate_volume(
        &self,
```

을 다음으로 바꾼다:

```rust
    async fn deactivate_volume(
        &self,
        _pool: &StoragePool,
        _id: Uuid,
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// 라이브 마이그레이션 동안 원본·대상 두 노드가 볼륨을 함께 쓸 수 있게 한다: 이 노드에서 켜져
    /// 있으면 다른 노드도 켤 수 있게 바꾸고, 꺼져 있으면 그렇게 켠다. 기본은 할 일이 없다(공유 LVM
    /// 풀만 LV 잠금을 공유로 바꾼다).
    async fn share_volume(
        &self,
        _pool: &StoragePool,
        _id: Uuid,
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// [`share_volume`](Self::share_volume)으로 함께 쓰게 한 볼륨을 다시 이 노드만 쓰게 한다. 이
    /// 노드에서 꺼져 있으면 할 일이 없다. 기본은 할 일이 없다.
    async fn exclusive_volume(
        &self,
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 켜 둔 LV의 내용을 `offset` 바이트부터 끝까지 0으로 채운다(`blkdiscard -z`). 크기에 비례해
```

을 다음으로 바꾼다:

```rust
    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 다른 노드도 함께 켤 수 있게 켠다(`lvchange -asy`). 이 노드에서 배타로 켜져 있으면 공유로
    /// 바꾼다. 다른 노드가 배타로 켜 두었으면 lvmlockd가 거절한다.
    async fn activate_shared(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 켜 둔 LV의 내용을 `offset` 바이트부터 끝까지 0으로 채운다(`blkdiscard -z`). 크기에 비례해
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
        Self::change("lvchange", &["-q", "-aey", &lv], vg, &[name]).await
    }
```

을 다음으로 바꾼다:

```rust
        Self::change("lvchange", &["-q", "-aey", &lv], vg, &[name]).await
    }

    async fn activate_shared(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvchange", &["-q", "-asy", &lv], vg, &[name]).await
    }
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
/// 배타 활성화한다(VM 디스크면 [`activate_volume`](Storage::activate_volume)부터
/// [`deactivate_volume`](Storage::deactivate_volume)까지). 스냅샷은 없다.
pub struct LvmSharedStorage {
```

을 다음으로 바꾼다:

```rust
/// 배타 활성화한다(VM 디스크면 [`activate_volume`](Storage::activate_volume)부터
/// [`deactivate_volume`](Storage::deactivate_volume)까지). 라이브 마이그레이션 동안만 원본·대상이
/// 공유로 켠다([`share_volume`](Storage::share_volume)). 스냅샷은 없다.
pub struct LvmSharedStorage {
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            Err(e) => Err(e),
        }
    }
```

을 다음으로 바꾼다:

```rust
            Err(e) => Err(e),
        }
    }

    /// LV 잠금을 공유로 바꾸거나(이 노드에서 켜져 있으면, 마이그레이션 원본) 공유로 켠다(꺼져
    /// 있으면, 받는 노드). 원본이 시작할 때 크기 늘리기가 남긴 0 채우기를 마쳤으므로 여기서는 채우지
    /// 않는다. 다른 노드가 배타로 켜 두었거나 이 노드가 0으로 채우는 중이거나 그 LV를 바꾸는 명령이
    /// 도는 중이면 일시 오류다.
    async fn share_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        self.existing(vg, id).await?;
        self.ops
            .activate_shared(vg, &name)
            .await
            .map_err(held_elsewhere)
    }

    /// 이 노드에서 켜져 있으면 LV 잠금을 다시 배타로 바꾼다(`lvchange -aey`). 꺼져 있으면 켜지 않는다.
    /// 다른 노드가 아직 공유로 켜 두었으면(마이그레이션의 다른 쪽이 아직 끄지 않았다) 일시 오류다.
    async fn exclusive_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        if !self.existing(vg, id).await?.is_active() {
            return Ok(());
        }
        self.ops
            .activate_exclusive(vg, &name)
            .await
            .map_err(held_elsewhere)
    }
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            self.call(format!("activate-ex {vg}/{name}"))?;
            self.check_held(name)?;
```

을 다음으로 바꾼다:

```rust
            self.call(format!("activate-ex {vg}/{name}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.active = true;
            self.sync_nodes(&lvs);
            Ok(())
        }

        async fn activate_shared(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("activate-sh {vg}/{name}"))?;
            self.check_held(name)?;
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`)만 알린다. 꺼진 볼륨 LV와 opLite 밖의 LV는 빼고 LVM
```

을 다음으로 바꾼다:

```rust

    /// 마이그레이션: 켜 둔 LV(원본)는 공유로 바꾸고 꺼진 LV(대상)는 공유로 켠다. 남은 0 채우기는
    /// 하지 않는다. 다른 노드가 배타로 켜 두었거나 이 노드가 채우는 중이거나 LV를 바꾸는 명령이 도는
    /// 중이면 일시 오류다.
    #[tokio::test]
    async fn shared_volume_for_a_migration() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let err = storage.share_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        lvm.taken();

        storage.share_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate-sh vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);

        // 받는 노드: 꺼져 있으면 공유로 켠다. 늘리기가 남긴 표시가 있어도 채우지 않는다.
        lvm.lvs.lock().unwrap().get_mut(&lv).unwrap().active = false;
        lvm.tags
            .lock()
            .unwrap()
            .insert(lv.clone(), vec![format!("{ZERO_FROM_TAG}4096")]);
        storage.share_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate-sh vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);

        lvm.held.lock().unwrap().push(lv.clone());
        held(storage.share_volume(&p, id, RAW).await.unwrap_err());
        lvm.held.lock().unwrap().clear();
        lvm.busy.lock().unwrap().push(lv.clone());
        let err = storage.share_volume(&p, id, RAW).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        lvm.busy.lock().unwrap().clear();
        let zeroing = format!("vg0/{lv}");
        storage.zeroing.lock().unwrap().insert(zeroing.clone());
        let err = storage.share_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(LV_ZEROING)), "{err:?}");
        storage.zeroing.lock().unwrap().remove(&zeroing);
        assert_eq!(lvm.taken(), [format!("activate-sh vg0/{lv}")]);
        invalid(
            storage
                .share_volume(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "lvm-shared pools hold raw volumes",
        );
    }

    /// 마이그레이션 뒤: 이 노드에서 켜져 있으면 다시 배타로 바꾸고, 꺼져 있으면 켜지 않는다. 다른
    /// 노드가 아직 공유로 켜 두었으면 일시 오류다.
    #[tokio::test]
    async fn exclusive_volume_after_a_migration() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();

        storage.exclusive_volume(&p, id, RAW).await.unwrap();
        assert!(lvm.taken().is_empty());
        assert!(!lvm.lv(&lv).unwrap().active);

        storage.share_volume(&p, id, RAW).await.unwrap();
        lvm.held.lock().unwrap().push(lv.clone());
        lvm.taken();
        held(storage.exclusive_volume(&p, id, RAW).await.unwrap_err());
        assert!(lvm.lv(&lv).unwrap().active);
        lvm.held.lock().unwrap().clear();

        storage.exclusive_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("activate-ex vg0/{lv}")
            ]
        );
        assert!(lvm.lv(&lv).unwrap().active);
    }

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`)만 알린다. 꺼진 볼륨 LV와 opLite 밖의 LV는 빼고 LVM
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            .deactivate_volume(pool, id, format)
            .await
    }
```

을 다음으로 바꾼다:

```rust
            .deactivate_volume(pool, id, format)
            .await
    }

    async fn share_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.share_volume(pool, id, format).await
    }

    async fn exclusive_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.exclusive_volume(pool, id, format).await
    }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        }
        async fn active_volumes(&self, _: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
```

을 다음으로 바꾼다:

```rust
        }
        async fn share_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("share_volume")
        }
        async fn exclusive_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("exclusive_volume")
        }
        async fn active_volumes(&self, _: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            storages.deactivate_volume(&p, id, f).await.unwrap();
            storages.active_volumes(&p).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            storages.deactivate_volume(&p, id, f).await.unwrap();
            storages.share_volume(&p, id, f).await.unwrap();
            storages.exclusive_volume(&p, id, f).await.unwrap();
            storages.active_volumes(&p).await.unwrap();
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                "deactivate_volume",
                "active_volumes",
```

을 다음으로 바꾼다:

```rust
                "deactivate_volume",
                "share_volume",
                "exclusive_volume",
                "active_volumes",
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
    let vm = spec(create_disk(n.tmp.path()));
    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
```

을 다음으로 바꾼다:

```rust
    let vm = spec(create_disk(n.tmp.path()));
    let task = Uuid::now_v7();
    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
        .dst
        .start_incoming(&vm, HOST, 61000..=61019)
        .await
```

을 다음으로 바꾼다:

```rust
        .dst
        .start_incoming(&vm, task, HOST, 61000..=61019)
        .await
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
    );
    assert_eq!(n.dst.list_incoming().await.unwrap(), vec![vm.id]);
    // 그 포트는 이제 받는 QEMU가 쓴다.
```

을 다음으로 바꾼다:

```rust
    );
    assert_eq!(n.dst.list_incoming().await.unwrap(), vec![(vm.id, task)]);
    // 그 포트는 이제 받는 QEMU가 쓴다.
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
    let vm = spec(create_disk(n.tmp.path()));
    n.src.start(&vm, None).await.unwrap();
```

을 다음으로 바꾼다:

```rust
    let vm = spec(create_disk(n.tmp.path()));
    let task = Uuid::now_v7();
    n.src.start(&vm, None).await.unwrap();
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
    // 받는 노드 인증서에 없는 이름: TLS가 실패한다.
    let port = n
        .dst
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
```

을 다음으로 바꾼다:

```rust
    // 받는 노드 인증서에 없는 이름: TLS가 실패한다.
    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
```

`crates/opl-driver/tests/qemu_migration.rs`에서

```rust
    // 다시 받고 보내면 된다(원본 QEMU에 TLS 객체를 다시 만든다).
    let port = n
        .dst
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
```

을 다음으로 바꾼다:

```rust
    // 다시 받고 보내면 된다(원본 QEMU에 TLS 객체를 다시 만든다).
    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61020..=61039)
        .await
        .unwrap();
    let target = MigrateTarget {
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, true).await
}
```

을 다음으로 바꾼다:

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Start).await
}
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, false).await
}

/// [`runtime_spec`]. `wait_resize`가 거짓이면 크기 늘리기 Task를 보지 않는다.
async fn resolve_activated(
```

을 다음으로 바꾼다:

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Running).await
}

/// 라이브 마이그레이션을 받을 때의 [`runtime_spec`]. 볼륨을 `activate_volume` 대신
/// `share_volume`으로 켠다(원본 노드가 아직 쓰고 있다).
pub async fn incoming_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, Activate::Incoming).await
}

/// VM 디스크의 볼륨을 이 노드에서 `share_volume`(`shared`) 또는 `exclusive_volume`으로 바꾼다(라이브
/// 마이그레이션). 처음 실패에서 멈춘다. 오류는 [`runtime_spec`]처럼 나눈다: 저장소의 일시 오류(다른
/// 노드가 아직 잠금을 쥐고 있다 등)는 `PoolUnavailable`이다.
pub async fn set_disks_shared(
    cluster: &Cluster,
    vm: &Vm,
    storage: &dyn Storage,
    shared: bool,
) -> Result<(), ResolveError> {
    for disk in &vm.spec.disks {
        let volume = cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
        let pool = cluster
            .get_by_name::<StoragePoolSpec>(pool_name)?
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        let format = volume.spec.format;
        let done = if shared {
            storage.share_volume(&pool, volume.id, format).await
        } else {
            storage.exclusive_volume(&pool, volume.id, format).await
        };
        match done {
            Ok(()) => {}
            Err(e @ (DriverError::VolumeNotFound(_) | DriverError::InvalidSpec(_))) => {
                return Err(ResolveError::VolumeUnusable(disk.volume.clone(), e));
            }
            Err(e) => {
                return Err(ResolveError::PoolUnavailable(
                    pool_name.clone(),
                    e.to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// 볼륨을 켜는 방식
#[derive(Clone, Copy, PartialEq)]
enum Activate {
    /// VM을 시작한다. 실행 중인 크기 늘리기 Task를 기다린다.
    Start,
    /// 이미 도는 VM에 다시 붙는다. 크기 늘리기 Task를 보지 않는다.
    Running,
    /// 마이그레이션을 받는다. 공유로 켜고(`share_volume`) 크기 늘리기 Task를 기다린다.
    Incoming,
}

/// [`runtime_spec`]. `how`에 따라 볼륨을 켠다.
async fn resolve_activated(
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    storage: &dyn Storage,
    wait_resize: bool,
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut activated = Vec::new();
    let result = resolve(
        cluster,
        vm,
        node,
        mount_root,
        storage,
        wait_resize,
        &mut activated,
    )
    .await;
    if result.is_err() {
```

을 다음으로 바꾼다:

```rust
    storage: &dyn Storage,
    how: Activate,
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut activated = Vec::new();
    let result = resolve(cluster, vm, node, mount_root, storage, how, &mut activated).await;
    if result.is_err() {
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    storage: &dyn Storage,
    wait_resize: bool,
    activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
```

을 다음으로 바꾼다:

```rust
    storage: &dyn Storage,
    how: Activate,
    activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        if wait_resize
            && tasks.iter().any(|t| {
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        if how != Activate::Running
            && tasks.iter().any(|t| {
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        let format = volume.spec.format;
        let located = match storage.activate_volume(&pool, volume.id, format).await {
            Ok(()) => {
```

을 다음으로 바꾼다:

```rust
        let format = volume.spec.format;
        let activation = match how {
            Activate::Incoming => storage.share_volume(&pool, volume.id, format).await,
            Activate::Start | Activate::Running => {
                storage.activate_volume(&pool, volume.id, format).await
            }
        };
        let located = match activation {
            Ok(()) => {
```

`crates/opl-reconciler/src/worker.rs` 전체:

```rust
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::reconciler::{Held, Restores};
use crate::resolve::{ResolveError, running_spec, runtime_spec, set_disks_shared};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// 작업자를 깨우는 이유.
#[derive(Debug)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
    /// 앞의 단계를 마쳤음을 알린다(`ReconcilerHandle::begin_migration`).
    Barrier(tokio::sync::oneshot::Sender<()>),
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
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 마이그레이션 Task가 다루는 중인 VM. 들어 있으면 이 작업자는 아무것도 하지 않는다.
    held: Held,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
    /// 원하는 상태가 다시 running으로 바뀌면 취소하므로, 새 종료는 늘 ACPI부터 다시 시작한다.
    stop_requested: Option<Instant>,
    /// 이 작업자가 마지막으로 종료를 요청한(ACPI 또는 강제) 시각. 뒤이은 종료 이벤트가 우리
    /// 요청의 결과인지 가린다. 요청을 취소했거나 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트(또는 강제 종료)는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안 유효하다.
    stopped_by_us: Option<Instant>,
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 켜고,
    /// 그 뒤 running으로 바뀌면 reconcile이 끈다.
    force_requested: bool,
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
    /// 디스크 볼륨이 켜져 있을 수 있다(시작하려고 켰거나 VM이 도는 것을 봤다). QEMU가 끝난 것을
    /// 확인하면 끈다([`Worker::deactivate_disks`]).
    disks_active: bool,
    /// 라이브 마이그레이션이 디스크 볼륨을 원본·대상이 함께 쓰게 해 두었을 수 있다(받은 VM을
    /// 넘겨받았거나, 남은 마이그레이션을 되돌렸거나, 마이그레이션 Task가 보류했다 풀었다). VM이 도는
    /// 동안 다시 이 노드만 쓰게 한다([`Worker::exclusive_disks`], 성공할 때까지). 저장소에 남지 않는
    /// 상태이므로 이 작업자가 직접 켜서 시작한 것이 아니면 참으로 시작한다(opld 재시작이나 넘겨받기
    /// 뒤 이미 도는 VM에 다시 붙으면 한 번 배타로 바꾼다, 멱등).
    disks_shared: bool,
    /// 보류 중에 받은 마지막 수명 이벤트(원본 QEMU의 크래시 등). 보류가 풀리면 처리한다.
    held_event: Option<EventKind>,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
        restores: Restores,
        held: Held,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            restores,
            held,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            disks_active: false,
            disks_shared: true,
            held_event: None,
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
            // 트리거는 차례로 처리하므로 앞의 단계는 이미 끝났다. 마이그레이션 Task가 디스크를
            // 공유로 바꿀 수 있으므로 보류가 풀린 뒤 다시 배타로 바꾼다.
            if let Trigger::Barrier(done) = trigger {
                self.disks_shared = true;
                let _ = done.send(());
                continue;
            }
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
        // 마이그레이션 Task가 status와 QEMU를 다룬다. 끝나면(`end_migration`) 다시 깨운다. 그동안
        // QEMU가 끝났으면(크래시 등) 보류가 풀린 뒤 그 이벤트대로 다룬다(재시작 정책).
        if self.held.lock().unwrap().contains(&self.id) {
            if let Trigger::Event(kind) = trigger {
                self.held_event = Some(kind);
            }
            return Ok(());
        }
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(mut vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 다른 노드로 옮겼다(보낸 원본이 꺼진 이벤트).
            self.held_event = None;
            // 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다.
            if self.hv.inspect(self.id).await?.is_some() {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            return Ok(());
        };
        if vm.status.migration.is_some() || vm.status.phase == VmPhase::Migrating {
            match self.clear_migration(&vm).await? {
                Some(current) => {
                    self.disks_shared = true;
                    vm = current;
                }
                // 그 사이 바뀌었다. 다시 읽어 맞춘다.
                None => {
                    self.wake(Instant::now());
                    return Ok(());
                }
            }
        }
        if let Some(kind) = self.held_event.take() {
            self.on_event(&vm, kind);
        }
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync | Trigger::Barrier(_) => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&mut vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.write_status(status, vm).await?;
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
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stopped_by_us
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            // 강제 종료의 결과로 Crashed가 와도 크래시로 보지 않는다.
            _ if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 우리가 요청했는지로 먼저 가린다.
            EventKind::Stopped {
                guest_initiated: true,
            } => self.power_off = Some(None),
            // 우리가 요청하지 않았고 게스트의 결정도 아니다(호스트의 SIGTERM 등). 원하는 상태는
            // 그대로 두므로 실패로 세지 않고 다시 시작한다.
            EventKind::Stopped {
                guest_initiated: false,
            } => {}
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

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다. 그 전에 status를 먼저
    /// 기록했으면 `vm`을 다시 읽은 것으로 바꾼다.
    async fn reconcile(&mut self, vm: &mut Vm) -> Result<VmStatus, StepError> {
        let mut actual = self.hv.inspect(self.id).await?;
        if let Some(state) = &actual
            && state.status == postmigrate()
            && !(vm.status.node == Some(self.config.node)
                && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating))
        {
            // 확정은 늘 status.node를 대상으로 바꾼다. 이 노드에서 돌던 VM이 아니면 다른 노드로 확정된
            // 마이그레이션의 원본이 정리되지 않고 남은 것이다(그 뒤 VM이 이 노드로 다시 옮겨졌다). 멈춘
            // 메모리는 그 사이 바뀐 디스크와 맞지 않으므로 이어서 실행하지 않고 끈 뒤 새로 시작한다.
            tracing::warn!(vm = %self.id, "stopping the stale source of a migration committed elsewhere");
            self.stopped_by_us = Some(Instant::now());
            if !self.hv.stop_migrated(self.id).await? {
                self.hv.stop(self.id, StopMode::Force).await?;
            }
            actual = self.hv.inspect(self.id).await?;
        }
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        if actual.is_some() {
            self.disks_active = true;
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                // 진행 중인 종료 요청을 취소한다. 게스트가 이미 받은 요청대로 꺼지면
                // `stopped_by_us`로 가려 다시 시작한다.
                self.stop_requested = None;
                self.force_requested = false;
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
                if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)이나 스냅샷으로 멈춘 채 죽었거나 QMP로 다룰 수 없다.
                    // 붙어 있으면 디스크를 다시 해석하지 않고(LVM이 응답하지 않을 수 있다) 이어서
                    // 실행하고, 아니면 start()가 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나
                    // 실패로 세지 않는다.
                    let mut continued = if state.status == postmigrate() {
                        // 마이그레이션을 보내고 멈춘 원본인데 이 노드에서 돌던 VM이고(위에서 가렸다)
                        // 표시가 없고(위에서 지웠다) 마이그레이션 Task도 다루지 않는다(보류가 아니다). 확정 쓰기는 표시가
                        // 있는 revision에서만 적용되므로 이제 확정될 수 없다: 이어서 실행한다(되돌리기의
                        // 재개 실패, 되돌린 뒤 끝난 마이그레이션).
                        tracing::warn!(vm = %self.id, "resuming the source of a migration that was not committed");
                        self.hv.resume(self.id).await.map_err(|e| e.to_string())
                    } else {
                        match self.hv.resume_left_stopped(self.id).await {
                            Ok(true) => Ok(()),
                            Ok(false) => self.reattach(vm).await,
                            Err(e) => Err(e.to_string()),
                        }
                    };
                    // 이 노드로 마이그레이션을 받아 멈춰 있는 QEMU다(확정되어 이 노드에 배정됐다).
                    // 받는 중 표시가 있을 때만 이어서 실행하고 표시를 지운다.
                    if continued.is_ok() && state.status == RuntimeStatus::Paused {
                        continued = self
                            .hv
                            .finish_incoming(self.id)
                            .await
                            .map_err(|e| e.to_string());
                        self.disks_shared |= continued.is_ok();
                    }
                    if let Err(e) = continued {
                        tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                        status.last_error = Some(format!("VM is unresponsive: {e}"));
                        self.wake(now + self.config.retry_interval);
                    }
                }
                if self.disks_shared && status.last_error.is_none() {
                    self.exclusive_disks(vm).await;
                }
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
                self.force_requested = false;
                self.deactivate_disks(vm).await;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if self.backoff.is_none() && vm.status.phase == VmPhase::Running {
                    // 저장된 phase가 Running인 채 다시 켜면(QEMU가 스스로 끝났다) 그사이 API·Task
                    // 실행기가 실행 중 VM으로 보고 디스크를 늘린다(공유 LVM은 늘린 부분을 0으로 채우기
                    // 전에 QEMU가 연다). 디스크를 해석하기 전에 Starting을 먼저 기록해, 먼저 들어온
                    // 크기 늘리기 Task는 resolve가 보고 나중 것은 API·실행기가 거절하게 한다. 기록하지
                    // 못해도(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) VM은 다시 켠다(설계 10절
                    // 불변식 4). 기록이 시간 초과로 실패한 사이 과반수가 돌아오면 좁은 틈이 남는다.
                    let mut starting = status.clone();
                    starting.phase = VmPhase::Starting;
                    starting.pid = None;
                    match self.write_status(starting, vm.clone()).await {
                        Ok(()) => match self.cluster.get::<VmSpec>(self.id)? {
                            Some(current) => *vm = current,
                            None => return Ok(status),
                        },
                        Err(e) => {
                            tracing::warn!(vm = %self.id, error = %e, "cannot record starting phase; restarting anyway");
                        }
                    }
                }
                if self.backoff.is_none() && self.start(vm).await? {
                    // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
                    // 않고 이벤트를 기다리게 한다.
                    self.was_running = true;
                    match self.hv.inspect(self.id).await? {
                        Some(state) => {
                            status.phase = VmPhase::Running;
                            status.pid = Some(state.pid);
                            status.last_error = None;
                        }
                        // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
                        None => {
                            self.was_running = false;
                            let deadline = Instant::now() + EVENT_GRACE;
                            self.awaiting_event = Some(deadline);
                            self.wake(deadline);
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
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    None => {
                        // 요청이 실패해도(QMP로 다룰 수 없는 VM) acpi_timeout 뒤 강제 종료한다.
                        self.stop_requested = Some(now);
                        self.stopped_by_us = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                        if let Err(e) = self.hv.stop(self.id, StopMode::Acpi).await {
                            tracing::warn!(vm = %self.id, error = %e, "ACPI shutdown request failed; will force after timeout");
                        }
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
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
                self.force_requested = false;
                self.awaiting_event = None;
                self.deactivate_disks(vm).await;
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
        let spec = match self.resolve(vm).await {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았거나(NFS 서버가 멈춤, 마운트 전 등) 디스크를 늘리는
            // 중이다. 끝나면 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::VolumeResizing(_))) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(false);
            }
            // 참조가 틀렸거나, 이 노드가 지원하지 않는 풀(NFS on macOS)이거나, 저장소가 볼륨을
            // 내주지 못했다. spec을 고치거나 VM을 옮겨야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
                return Ok(false);
            }
        };
        self.disks_active = true;
        // 방금 이 노드에서 켰다(배타).
        self.disks_shared = false;
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        let started = self.hv.start(&spec, restore.as_deref()).await;
        if started.is_err() {
            self.deactivate_disks(vm).await;
        }
        match (started, restore) {
            (Ok(()), _) => Ok(true),
            // 태그 없이 다시 시도하면 스냅샷이 아닌 처음부터 부팅하게 되므로 일시 오류라도 멈춘다.
            (Err(e), Some(tag)) => {
                self.power_off = Some(Some(format!("failed to restore snapshot {tag}: {e}")));
                Ok(false)
            }
            (Err(e), None) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            (Err(e), None) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(false)
            }
        }
    }

    /// 켜져 있을 수 있는 디스크 볼륨을 끈다(QEMU가 끝난 뒤). 끄지 못하면 경고만 남긴다(켜진 채 남은
    /// 볼륨은 Task 실행기의 정리가 다시 끈다).
    async fn deactivate_disks(&mut self, vm: &Vm) {
        // 끄면 공유도 풀린다.
        self.disks_shared = false;
        if !std::mem::take(&mut self.disks_active) {
            return;
        }
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }

    /// 도는 VM의 디스크 볼륨을 다시 이 노드만 쓰게 한다(공유 LVM은 배타 잠금). 마이그레이션의 다른
    /// 쪽 노드가 아직 볼륨을 끄지 않았으면(원본이 정리 전이다) 잠시 뒤 다시 한다. 그동안에도 VM은
    /// 돈다.
    async fn exclusive_disks(&mut self, vm: &Vm) {
        match set_disks_shared(&self.cluster, vm, &*self.config.storage, false).await {
            Ok(()) => self.disks_shared = false,
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::Cluster(_))) => {
                tracing::info!(vm = %self.id, error = %e, "disks are still shared with another node; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
            Err(e) => {
                tracing::warn!(vm = %self.id, error = %e, "cannot make disks exclusive to this node again");
                self.disks_shared = false;
            }
        }
    }

    /// 끝나지 않은 채 남은 마이그레이션 표시(`Migrating`, `status.migration`)를 지운다. 이 노드의
    /// 마이그레이션 Task가 다루지 않는 VM이므로, 원본 opld가 확정 전에 죽었거나(원본 노드) 확정 뒤
    /// 넘겨받는 중이다(대상 노드). 읽은 revision으로만 쓰므로 그 사이 확정되었으면(남은 확정 쓰기가
    /// 먼저 적용됨) 쓰지 않고 `None`이다. 쓰고 나면 그 확정은 더는 적용되지 않으므로, 이 노드의 QEMU가
    /// 아직 보내고 있으면 취소하고(나중에 끝나 원본이 멈추지 않게), 마이그레이션을 보내고 멈춰 있으면
    /// (`postmigrate`, 취소하는 사이에 끝난 경우 포함) 이어서 실행한다. 대상의 QEMU는 확정 뒤에만
    /// 돌므로 두 QEMU가 함께 돌지 않는다. 다시 읽은 VM을 돌려준다.
    async fn clear_migration(&self, vm: &Vm) -> Result<Option<Vm>, StepError> {
        let mut status = vm.status.clone();
        status.migration = None;
        if status.phase == VmPhase::Migrating {
            status.phase = VmPhase::Running;
        }
        match self
            .cluster
            .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => return Ok(None),
            r => r?,
        };
        self.hv.cancel_migration(self.id).await?;
        if let Some(state) = self.hv.inspect(self.id).await?
            && state.status == postmigrate()
        {
            tracing::warn!(vm = %self.id, "resuming the source of a migration that did not complete");
            self.hv.resume(self.id).await?;
        }
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        Ok(vm.filter(|vm| vm.spec.node == Some(self.config.node)))
    }

    /// VM의 런타임 정의. 디스크 볼륨을 활성화한다.
    async fn resolve(&self, vm: &Vm) -> Result<VmRuntimeSpec, ResolveError> {
        let c = &self.config;
        runtime_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage).await
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다.
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let c = &self.config;
        let spec = running_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
    }

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            match self
                .cluster
                .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
                .await
            {
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) => vm = current,
                        None => return Ok(()),
                    }
                }
                r => return r.map(|_| ()).map_err(Into::into),
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, mut vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
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
            snapshot: vm.status.snapshot,
            schedule_error: vm.status.schedule_error.clone(),
            migration: None,
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}

fn postmigrate() -> RuntimeStatus {
    RuntimeStatus::Other("postmigrate".into())
}
```

`crates/opl-reconciler/tests/migration.rs`에서

```rust
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{MigrationStatus, Vm, VmPhase, VmSpec};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{MigrationStatus, PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;
```

`crates/opl-reconciler/tests/migration.rs`에서

```rust
    env.hv
        .start_incoming(&spec, "127.0.0.1", 60000..=60099)
        .await
```

을 다음으로 바꾼다:

```rust
    env.hv
        .start_incoming(&spec, Uuid::now_v7(), "127.0.0.1", 60000..=60099)
        .await
```

`crates/opl-reconciler/tests/migration.rs`에서

```rust
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}
```

을 다음으로 바꾼다:

```rust
    assert!(env.hv.launches().is_empty(), "{:?}", env.hv.launches());
}

/// opld가 마이그레이션을 보내다 확정 전에 죽었는데 QEMU는 아직 보내는 중이다: 작업자가 `Migrating`을
/// 되돌린 뒤 그 마이그레이션을 취소한다. 나중에 끝나 원본을 멈추지 않는다.
#[tokio::test]
async fn active_migration_left_by_a_restart_is_cancelled() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    env.hv.start_outgoing(vm.id);
    env.start_reconciler();

    env.wait(vm.id, "migration rolled back", |v| {
        running(v) && v.status.migration.is_none()
    })
    .await;
    assert!(!env.hv.is_outgoing(vm.id));
    env.hv.complete_outgoing(vm.id);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}

/// 취소하는 사이에 마이그레이션이 끝났다(원본은 `postmigrate`): 확정될 수 없으므로 이어서 실행한다.
#[tokio::test]
async fn migration_completing_while_cancelled_resumes_the_source() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    env.hv.start_outgoing(vm.id);
    env.hv.complete_on_cancel();
    env.start_reconciler();

    env.wait(vm.id, "migration rolled back", |v| {
        running(v) && v.status.migration.is_none()
    })
    .await;
    // 취소하지 않았으면 여기서 끝난다.
    env.hv.complete_outgoing(vm.id);
    let deadline = std::time::Instant::now() + common::WAIT;
    while status_of(&env, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(std::time::Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 마이그레이션 Task가 표시를 지웠지만 멈춘 원본을 이어서 실행하지 못하고 끝났다: 표시가 없고 이 노드의
/// VM이므로 확정될 수 없다. 보류가 풀리면 작업자가 이어서 실행한다.
#[tokio::test]
async fn source_left_paused_after_a_rollback_is_resumed() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    mark_migrating(&env, vm.id, Uuid::now_v7()).await;
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    // 되돌리기: 표시만 지우고 원본은 멈춘 채 둔다(`cont` 실패).
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Running;
    status.migration = None;
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    handle.end_migration(vm.id);

    let deadline = std::time::Instant::now() + common::WAIT;
    while status_of(&env, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(std::time::Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let got = env.get(vm.id);
    assert!(running(&got));
    assert_eq!(got.status.pid, pid);
}

/// 보류 중에 원본 QEMU가 크래시했다: 보류가 풀린 뒤 그 이벤트대로 다룬다(`restart = never`면 다시
/// 켜지 않고 실패로 남긴다).
#[tokio::test]
async fn crash_while_held_follows_the_restart_policy() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    env.hv.crash(vm.id);
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.end_migration(vm.id);

    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.last_error.as_deref(), Some("VM crashed"));
    assert_eq!(env.hv.launches().len(), 1, "{:?}", env.hv.launches());
}

/// 다른 노드로 확정된 마이그레이션의 원본이 정리되지 않고 `postmigrate`로 남았는데, 그 뒤 VM이 그 노드에서
/// 꺼지고 이 노드로 다시 옮겨져 켜졌다: 멈춘 원본의 메모리는 낡았으므로 이어서 실행하지 않고 끈 뒤
/// 새로 시작한다.
#[tokio::test]
async fn stale_source_of_a_committed_migration_is_restarted_fresh() {
    let mut env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.stop_reconciler().await;
    env.hv.migrate(vm.id, &target(), &opts()).await.unwrap();
    // 다른 노드로 확정되어 거기서 꺼진 뒤 이 노드로 다시 옮겨졌다(spec.node는 이 노드, power는 running).
    let mut status = env.get(vm.id).status;
    status.node = Some(Uuid::now_v7());
    status.phase = VmPhase::Stopped;
    status.pid = None;
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    env.start_reconciler();

    let got = env
        .wait(vm.id, "started fresh", |v| {
            running(v) && v.status.node == Some(env.node)
        })
        .await;
    assert_ne!(got.status.pid, pid);
    assert_eq!(env.hv.launches().len(), 2, "{:?}", env.hv.launches());
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_driver::{
    Capacity, DriverError, Hypervisor, SourceFormat, StopMode, Storage, VolumeInfo, VolumeSource,
};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{
    Capacity, DriverError, Hypervisor, MigrateOptions, MigrateTarget, SourceFormat, StopMode,
    Storage, VolumeInfo, VolumeSource,
};
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_types::vm::{
    DiskBus, DiskSpec, PowerState, RestartPolicy, ScheduleError, VmPhase, VmSpec, VmStatus,
};
```

을 다음으로 바꾼다:

```rust
use opl_types::vm::{
    DiskBus, DiskSpec, MigrationStatus, PowerState, RestartPolicy, ScheduleError, VmPhase, VmSpec,
    VmStatus,
};
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    }
    async fn create_volume(
```

을 다음으로 바꾼다:

```rust
    }
    async fn share_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("share", id));
        Ok(())
    }
    async fn exclusive_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("exclusive", id));
        Ok(())
    }
    async fn create_volume(
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
        [("activate", root.id), ("deactivate", root.id)]
    );
}
```

을 다음으로 바꾼다:

```rust
        [("activate", root.id), ("deactivate", root.id)]
    );
}

/// opld가 마이그레이션을 보내다 확정 전에 죽고 다시 떴다: 작업자가 원본을 이어서 실행한 뒤 공유로
/// 바꿔 둔 디스크를 다시 배타로 바꾼다(한 번).
#[tokio::test]
async fn rolled_back_migration_makes_disks_exclusive_again() {
    let storage = Arc::new(Activations::default());
    let (mut env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.stop_reconciler().await;
    let mut status = env.get(vm.id).status;
    status.phase = VmPhase::Migrating;
    status.migration = Some(MigrationStatus {
        node: Uuid::now_v7(),
        task: Uuid::now_v7(),
    });
    env.cluster
        .update_status::<VmSpec>(vm.id, &status)
        .await
        .unwrap();
    let target = MigrateTarget {
        host: "127.0.0.2".into(),
        port: 60000,
        tls_hostname: "127.0.0.2".into(),
    };
    let opts = MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(60),
    };
    env.hv.migrate(vm.id, &target, &opts).await.unwrap();
    env.start_reconciler();

    env.wait(vm.id, "migration rolled back", |v| {
        running(v) && v.status.migration.is_none()
    })
    .await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage
        .calls
        .lock()
        .unwrap()
        .contains(&("exclusive", root.id))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "disk not made exclusive"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("activate", root.id), ("exclusive", root.id)]
    );
}

/// 디스크가 이 노드만 쓰는 상태인지는 메모리에 없다: opld가 다시 뜨면 작업자가 이미 도는 VM의
/// 디스크를 다시 배타로 바꾼다(공유 LVM에서 마이그레이션 중 죽었을 수 있다, 한 번).
#[tokio::test]
async fn restarted_reconciler_makes_running_disks_exclusive() {
    let storage = Arc::new(Activations::default());
    let (mut env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*storage.calls.lock().unwrap(), [("activate", root.id)]);

    env.stop_reconciler().await;
    env.start_reconciler();
    wait_exclusive(&storage, root.id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("activate", root.id), ("exclusive", root.id)]
    );
}

/// 마이그레이션 Task가 VM을 보류했다 풀었다(실패한 마이그레이션): 작업자가 디스크를 다시 배타로
/// 바꾼다.
#[tokio::test]
async fn disks_are_made_exclusive_after_a_migration_ends() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    let handle = env.handle();
    handle.begin_migration(vm.id).await;
    handle.end_migration(vm.id);
    wait_exclusive(&storage, root.id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("activate", root.id), ("exclusive", root.id)]
    );
}

async fn wait_exclusive(storage: &Activations, id: Uuid) {
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage.calls.lock().unwrap().contains(&("exclusive", id)) {
        assert!(
            std::time::Instant::now() < deadline,
            "disk not made exclusive"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_driver::{Capacity, DriverError, SourceFormat, Storage, VolumeInfo, VolumeSource};
use opl_reconciler::resolve::{ResolveError, runtime_spec};
use opl_types::network::{Network, NetworkSpec, tap_name};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{Capacity, DriverError, SourceFormat, Storage, VolumeInfo, VolumeSource};
use opl_reconciler::resolve::{ResolveError, incoming_spec, runtime_spec, set_disks_shared};
use opl_types::network::{Network, NetworkSpec, tap_name};
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
/// 활성화한 볼륨과 끈 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화와 (실패했을 때) 끄기만 부른다.
#[derive(Default)]
```

을 다음으로 바꾼다:

```rust
/// 활성화한 볼륨과 끈 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화와 (실패했을 때) 끄기만 부른다. 마이그레이션의 공유(`true`)·배타(`false`)
/// 바꾸기는 `shared`에 적고, `fail`이 있으면 그 오류로 실패한다.
#[derive(Default)]
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    deactivated: Mutex<Vec<Uuid>>,
    fail: Mutex<Option<fn() -> DriverError>>,
}
```

을 다음으로 바꾼다:

```rust
    deactivated: Mutex<Vec<Uuid>>,
    shared: Mutex<Vec<(Uuid, bool)>>,
    fail: Mutex<Option<fn() -> DriverError>>,
}

impl Activations {
    fn share(&self, id: Uuid, shared: bool) -> Result<(), DriverError> {
        self.shared.lock().unwrap().push((id, shared));
        match *self.fail.lock().unwrap() {
            Some(fail) => Err(fail()),
            None => Ok(()),
        }
    }
}
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
        Ok(())
    }
```

을 다음으로 바꾼다:

```rust
        Ok(())
    }
    async fn share_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.share(id, true)
    }
    async fn exclusive_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.share(id, false)
    }
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    assert!(storage.deactivated.lock().unwrap().is_empty());
}
```

을 다음으로 바꾼다:

```rust
    assert!(storage.deactivated.lock().unwrap().is_empty());
}

/// 마이그레이션을 받을 때는 볼륨을 활성화하지 않고 공유로 켠다(원본이 아직 쓴다). 실패하면 켠
/// 볼륨을 끈다.
#[tokio::test]
async fn incoming_spec_shares_the_volumes() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    cluster.create(&data).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));

    let storage = Activations::default();
    let spec = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(spec.disks.len(), 2);
    assert!(storage.calls.lock().unwrap().is_empty());
    assert_eq!(
        *storage.shared.lock().unwrap(),
        [(root.id, true), (data.id, true)]
    );
    assert!(storage.deactivated.lock().unwrap().is_empty());

    vm.spec.nics.push(nic("missing", None));
    let storage = Activations::default();
    let err = incoming_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::NetworkNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id, data.id]);
}

/// 디스크마다 공유·배타로 바꾼다. 처음 실패에서 멈추고, 저장소의 일시 오류는 `PoolUnavailable`,
/// 없는 볼륨은 `VolumeNotFound`다.
#[tokio::test]
async fn set_disks_shared_switches_every_disk() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    cluster.create(&data).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));

    let storage = Activations::default();
    set_disks_shared(&cluster, &vm, &storage, true)
        .await
        .unwrap();
    set_disks_shared(&cluster, &vm, &storage, false)
        .await
        .unwrap();
    assert_eq!(
        *storage.shared.lock().unwrap(),
        [
            (root.id, true),
            (data.id, true),
            (root.id, false),
            (data.id, false)
        ]
    );

    let storage = Activations::default();
    *storage.fail.lock().unwrap() = Some(|| DriverError::Timeout("another node"));
    let err = set_disks_shared(&cluster, &vm, &storage, false)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::PoolUnavailable(..)), "{err:?}");
    assert_eq!(*storage.shared.lock().unwrap(), [(root.id, false)]);

    vm.spec.disks.push(disk("missing"));
    let storage = Activations::default();
    let err = set_disks_shared(&cluster, &vm, &storage, true)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::VolumeNotFound(_)), "{err:?}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 724개, 실패 0

Run: `cargo test -p opl-driver --lib storage::lvm`
Expected: PASS

Run: `cargo test -p opl-reconciler --test migration`
Expected: PASS

Run: `cargo test -p opl-api --test two_nodes --test incoming`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): 공유 LVM 디스크의 라이브 마이그레이션 (LV 잠금 공유↔배타)"
```

---

### Task 6: 실제 마이그레이션 시험(migration-it)과 CI

기능 `migration-it`(opld)과 `crates/opld/tests/migration.rs`: 한 호스트의 opld 둘(`Daemon::with_hypervisor`, 스코프 끔, macOS는 TCG, 시험마다 다른 포트 범위)로 클러스터를 만들고 공유 디렉터리 풀의 UEFI VM을 옮긴다 — 확정 뒤 두 노드의 기록, 원본 QEMU 없음, 마이그레이션 중 409 `vm_busy`, UEFI 변수 표식이 대상 파일에, 포트를 막은 실패 뒤 원본 유지, 되돌아오기, 디스크 데이터. Linux에서 NFS·RBD·공유 LVM 변형(각 기능과 환경 변수, 공유 LVM은 `lvchange -aey` 세 시점 확인). CI `linux-kvm`의 FEATURES에 `opld/migration-it`.

**Files:**
- Create: `crates/opld/tests/migration.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
  # QEMU는 systemd 스코프로 감싼다(OPL_TEST_SYSTEMD). 웹 UI E2E는 브라우저로 같은 opld를 다룬다.
  linux-kvm:
```

을 다음으로 바꾼다:

```yaml
  # QEMU는 systemd 스코프로 감싼다(OPL_TEST_SYSTEMD). 웹 UI E2E는 브라우저로 같은 opld를 다룬다.
  # 라이브 마이그레이션 E2E는 한 호스트의 opld 둘로 공유 디렉터리·NFS·RBD·공유 LVM 풀 위의 VM을 옮긴다
  # (두 노드의 스코프 이름이 겹치므로 그 시험만 스코프를 끈다).
  linux-kvm:
```

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it,opl-driver/lvm-shared-it,opld/lvm-shared-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it,opl-driver/lvm-shared-it,opld/lvm-shared-it,opld/migration-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

`crates/opld/Cargo.toml`에서

```toml
web-it = ["qemu-it", "web-ui"]
```

을 다음으로 바꾼다:

```toml
web-it = ["qemu-it", "web-ui"]
# 한 호스트의 opld 둘로 실행 중인 VM을 공유 풀 위에서 옮기는 라이브 마이그레이션 E2E. qemu-it의 준비물과
# qemu-img, qemu-io가 필요하다. nfs-it·rbd-it·lvm-shared-it와 함께 켜면 그 풀로도 옮긴다(그 시험의 준비물).
migration-it = []
```

`crates/opld/tests/common/mod.rs`에서

```rust
    pub fn new(name: &str, extra_config: &str) -> Self {
        let dir = tempfile::Builder::new()
```

을 다음으로 바꾼다:

```rust
    pub fn new(name: &str, extra_config: &str) -> Self {
        // systemd가 있는 Linux(CI)에서만 `OPL_TEST_SYSTEMD=1`로 QEMU를 스코프로 감싼다.
        let systemd_scope = std::env::var_os("OPL_TEST_SYSTEMD").is_some();
        Self::with_hypervisor(
            name,
            extra_config,
            &format!("systemd_scope = {systemd_scope}"),
        )
    }

    /// `[hypervisor]` 절의 내용을 정해 만든다.
    pub fn with_hypervisor(name: &str, extra_config: &str, hypervisor: &str) -> Self {
        let dir = tempfile::Builder::new()
```

`crates/opld/tests/common/mod.rs`에서

```rust
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        // systemd가 있는 Linux(CI)에서만 `OPL_TEST_SYSTEMD=1`로 QEMU를 스코프로 감싼다.
        let systemd_scope = std::env::var_os("OPL_TEST_SYSTEMD").is_some();
        let config = format!(
```

을 다음으로 바꾼다:

```rust
            std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
        let config = format!(
```

`crates/opld/tests/common/mod.rs`에서

```rust
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\nsystemd_scope = {systemd_scope}\n",
            sock = dir.path().join("opld.sock").display(),
```

을 다음으로 바꾼다:

```rust
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\n{hypervisor}\n",
            sock = dir.path().join("opld.sock").display(),
```

`crates/opld/tests/migration.rs`를 만든다:

```rust
//! 실제 라이브 마이그레이션: 한 호스트의 opld 둘로 클러스터를 만들고, 공유 풀 디스크를 가진 UEFI VM을
//! 실행 중에 다른 노드로 옮긴다. 확정 뒤 상태, 원본 QEMU 정리, UEFI 변수 파일, 디스크 내용, 마이그레이션
//! 중의 `vm_busy`, 대상 포트가 막혀 실패하면 원본이 계속 도는지를 본다. 게스트에는 OS가 없으므로(UEFI
//! 펌웨어만) 디스크 내용은 정지한 VM에서 `qemu-io`로 본다.
//! `cargo test -p opld --features migration-it --test migration`
//! QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다. 이 Mac에서도 돈다(TCG).
//!
//! 공유 풀 시험은 그 풀의 기능을 함께 켜고 그 시험과 같은 환경 변수가 있어야 한다(Linux, root):
//! `nfs-it`(`OPL_TEST_NFS_EXPORT`), `rbd-it`(`OPL_TEST_RBD`, `client.admin`), `lvm-shared-it`
//! (`OPL_TEST_LVM_SHARED`, 있으면 `OPL_TEST_ISCSI`).
//!
//! 한 호스트의 두 노드는 systemd 스코프 이름(`opl-vm-{id}.scope`)과 브리지 NIC의 탭 이름이 겹치므로
//! 스코프를 끄고 NIC 없이 띄운다. 공유 LVM의 잠금(lvmlockd)은 호스트 단위이므로 두 노드가 같은 잠금을
//! 쓴다 — 원본이 확정 뒤 LV를 끄려 하면 대상 QEMU가 열고 있어 실패하고 경고만 남는다.
#![cfg(feature = "migration-it")]

mod common;

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{Daemon, eventually, free_port};
use oplctl::Client;
use reqwest::{Method, StatusCode};
use rustix::process::{Pid, test_kill_process};
use serde_json::{Value, json};

/// 원본 UEFI 변수 파일 끝에 남기는 표식
const MARKER: &[u8] = b"opl-migration-it";

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
fn client(d: &Daemon) -> Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}

/// 두 노드의 `[hypervisor]`. HVF(macOS)에서는 보내는 QEMU가 메모리 변경 추적을 켜다 가끔 죽고 API도
/// `qemu-hvf` 노드의 마이그레이션을 거절하므로 이 Mac에서는 TCG로 돌린다. Linux는 KVM(없으면 TCG)이다.
fn hypervisor() -> &'static str {
    if cfg!(target_os = "macos") {
        "systemd_scope = false\naccel = \"tcg\""
    } else {
        "systemd_scope = false"
    }
}

/// 한 클러스터의 두 노드. 0번이 처음 VM을 띄우는 노드다.
struct Pair {
    d: [Daemon; 2],
    c: [Client; 2],
}

impl Pair {
    /// 노드 `node1`·`node2`를 띄워 클러스터를 만들고 둘 다 로그인한다. `ports`는 각 노드의 마이그레이션
    /// 받기 포트 범위다(같은 파일의 시험이 함께 돌므로 시험마다 다르게 준다).
    async fn new(cluster: &str, ports: [&str; 2]) -> Self {
        // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
        let daemon = |name: &str, ports: &str| {
            Daemon::with_hypervisor(
                name,
                &format!("[reconciler]\nacpi_timeout_secs = 5\n[migration]\nports = \"{ports}\"\n"),
                hypervisor(),
            )
        };
        let mut d = [daemon("node1", ports[0]), daemon("node2", ports[1])];
        for d in &mut d {
            d.start().await;
        }
        d[0].init(cluster).await;
        d[0].login().await;
        let url = d[0].join_url.clone().unwrap();
        d[1].join(&url).await.unwrap();
        let (password, fp) = (
            d[0].admin_password.clone().unwrap(),
            d[0].ca_fingerprint.clone().unwrap(),
        );
        d[1].login_with(&password, &fp).await;
        let c = [client(&d[0]), client(&d[1])];
        eventually("both nodes online", || async {
            let nodes = c[0].get("/nodes").await.unwrap();
            let nodes = nodes["items"].as_array().unwrap();
            nodes.len() == 2 && nodes.iter().all(|n| n["status"]["online"] == true)
        })
        .await;
        Self { d, c }
    }

    async fn node_id(&self, i: usize) -> String {
        let node = self.c[0]
            .get(&format!("/nodes/{}", self.d[i].name))
            .await
            .unwrap();
        node["id"].as_str().unwrap().to_string()
    }

    /// 풀이 두 노드에서 준비될 때까지 기다린다.
    async fn wait_pool_ready(&self, pool: &str) {
        for d in &self.d {
            d.wait_pool_ready(pool).await;
        }
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        // NFS 풀이 다시 마운트되지 않게 opld를 먼저 멈추고, 임시 디렉터리를 지우기 전에 마운트를
        // 푼다(마운트된 채로 지우면 내보내기의 파일까지 지운다). 풀지 못하면 디렉터리를 남긴다.
        for d in &mut self.d {
            if d.is_running() {
                d.kill();
            }
            #[cfg(target_os = "linux")]
            unmount_nfs_pools(d);
        }
    }
}

/// 데몬의 `{data_dir}/pools` 아래 NFS 마운트를 푼다.
#[cfg(target_os = "linux")]
fn unmount_nfs_pools(d: &mut Daemon) {
    let Ok(pools) = std::fs::canonicalize(d.data_dir().join("pools")) else {
        return;
    };
    let info = std::fs::read_to_string(opl_driver::mounts::MOUNTINFO).unwrap();
    let mut left = false;
    for point in opl_driver::mounts::nfs_mount_points(&info)
        .into_iter()
        .filter(|p| p.starts_with(&pools))
    {
        if !Command::new("umount")
            .arg(&point)
            .status()
            .is_ok_and(|s| s.success())
        {
            eprintln!("cannot unmount {}", point.display());
            left = true;
        }
    }
    if left {
        let dir = std::mem::replace(&mut d.dir, tempfile::TempDir::new().unwrap());
        eprintln!(
            "left {} in place because nfs pools are still mounted under it",
            dir.keep().display()
        );
    }
}

/// 프로세스가 살아 있는지
fn alive(pid: u64) -> bool {
    test_kill_process(Pid::from_raw(pid as i32).unwrap()).is_ok()
}

/// 명령줄에 `pattern`이 든 프로세스가 있는지(그 노드의 VM 실행 디렉터리로 그 노드의 QEMU를 찾는다)
fn process_with(pattern: &Path) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(pattern)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run pgrep")
        .success()
}

/// 노드 `d`의 VM 실행 디렉터리(QEMU 명령줄에 든다)
fn vm_run_dir(d: &Daemon, id: &str) -> PathBuf {
    d.path("run").join("vm").join(id)
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// API 오류 응답의 상태와 코드
async fn error(c: &Client, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
    let resp = c.request(method, path).json(&body).send().await.unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap())
}

/// 실행 중인 VM `web`의 QEMU pid. 확정은 pid 없이 `running`을 쓰고 대상 워커가 넘겨받으며 채우므로
/// 기다린다.
async fn running_pid(c: &Client) -> u64 {
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let vm = c.get("/vms/web").await.unwrap();
        if vm["status"]["phase"] == "running"
            && let Some(pid) = vm["status"]["pid"].as_u64()
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "vm web is not running with a pid: {vm}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// 보낸 마이그레이션 Task와 원본의 VM id·QEMU pid
struct Started {
    task: Value,
    id: String,
    pid: u64,
}

/// 노드 `from`에서 도는 VM `web`을 노드 `to`로 옮기기 시작한다.
async fn start_migration(p: &Pair, from: usize, to: usize) -> Started {
    let c = &p.c[from];
    let pid = running_pid(c).await;
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(vm["spec"]["node"].as_str().unwrap(), p.node_id(from).await);
    let task = c
        .post("/vms/web/migrate", &json!({ "node": p.d[to].name }))
        .await
        .unwrap();
    assert_eq!(task["spec"]["op"]["kind"], "vm-migrate", "{task}");
    Started {
        task,
        id: vm["id"].as_str().unwrap().to_string(),
        pid,
    }
}

/// Task가 성공하기를 기다리고 확정 뒤 상태를 확인한다: 두 노드에서 같은 기록, 대상의 새 QEMU가
/// 계속 돌고 원본 QEMU는 없다. 대상 QEMU의 pid를 돌려준다.
async fn finish_migration(p: &Pair, from: usize, to: usize, started: Started) -> u64 {
    p.c[from].wait_task(&started.task).await.unwrap();
    let target = p.node_id(to).await;
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    // 넘겨받은 VM은 계속 돈다(워커가 QMP `query-status`로 본다).
    let pid = running_pid(&p.c[to]).await;
    assert_ne!(pid, started.pid);
    assert!(alive(pid));
    assert!(process_with(&vm_run_dir(&p.d[to], &started.id)));
    // 원본 opld가 멈춘(`postmigrate`) QEMU를 끈다.
    eventually("source QEMU stopped", || async {
        !alive(started.pid) && !process_with(&vm_run_dir(&p.d[from], &started.id))
    })
    .await;
    assert_eq!(running_pid(&p.c[to]).await, pid);
    pid
}

/// VM `web`을 노드 `from`에서 `to`로 옮긴다.
async fn migrate(p: &Pair, from: usize, to: usize) -> u64 {
    let started = start_migration(p, from, to).await;
    finish_migration(p, from, to, started).await
}

/// VM `web`을 노드 0에 만들어 켜고 0→1→0으로 옮긴 뒤 강제로 끈다. 공유 풀 시험이 쓴다.
#[cfg(all(any(feature = "nfs-it", feature = "rbd-it"), target_os = "linux"))]
async fn round_trip(p: &Pair, disk: &str) {
    p.d[0]
        .ctl(&[
            "vm", "create", "web", "--memory", "256", "--disk", disk, "--node", "node1", "--start",
        ])
        .await
        .unwrap();
    migrate(p, 0, 1).await;
    migrate(p, 1, 0).await;
    p.d[0].ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
}

/// 공유 디렉터리 풀(두 노드가 같은 경로로 연다)의 qcow2 디스크를 가진 UEFI VM을 옮긴다.
#[tokio::test]
async fn migrates_a_running_vm_between_two_nodes() {
    // 노드 1은 처음에는 원본이고, 나중에 받을 때 쓸 포트 하나를 시험이 막는다.
    let blocked = free_port();
    let p = Pair::new("mig", [&format!("{blocked}-{blocked}"), "60100-60119"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);

    let shared = tempfile::Builder::new()
        .prefix("oplshared")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    p.wait_pool_ready("shared").await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    let disk = shared
        .path()
        .join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(qemu_io(&disk, "qcow2", "write -P 0xab 0 64k"));

    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1",
    ])
    .await
    .unwrap();
    let vm = c.get("/vms/web").await.unwrap();
    let id = vm["id"].as_str().unwrap().to_string();
    let uuid = id.parse().unwrap();

    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
    // 옮긴 뒤 대상 파일에 표식이 있으면 QEMU가 pflash 내용을 옮겨 쓴 것이다.
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    let template = opl_qemu::HostProfile::native(Path::new(&share)).uefi_vars_template;
    let src_vars = opl_driver::uefi_vars_path(&p.d[0].data_dir(), uuid);
    let dst_vars = opl_driver::uefi_vars_path(&p.d[1].data_dir(), uuid);
    let mut content = std::fs::read(&template).unwrap();
    let at = content.len() - MARKER.len();
    content[at..].copy_from_slice(MARKER);
    std::fs::create_dir_all(src_vars.parent().unwrap()).unwrap();
    std::fs::write(&src_vars, &content).unwrap();
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");

    // 마이그레이션 중에는 다른 동작이 409 vm_busy다. 대상 QEMU를 띄우는 동안이므로 아직 끝나지 않았다.
    let started = start_migration(&p, 0, 1).await;
    let (status, body) = error(c, Method::POST, "/vms/web/actions/stop", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let (status, body) = error(
        c,
        Method::POST,
        "/vms/web/migrate",
        json!({ "node": "node2" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let pid = finish_migration(&p, 0, 1, started).await;
    let target = p.node_id(1).await;

    // 대상은 템플릿(표식 없음)으로 변수 파일을 만들어 받는다. 대상 QEMU가 이어서 실행하면서 pflash(UEFI
    // 변수)를 자기 파일에 원본 내용으로 쓰므로 표식이 생긴다. 게스트 펌웨어가 대상에서 계속 변수를 바꾸므로
    // 파일 전체를 원본과 견주지는 않는다.
    eventually("UEFI variables written on the target", || async {
        std::fs::read(&dst_vars).is_ok_and(|dst| dst.ends_with(MARKER))
    })
    .await;
    assert!(!std::fs::read(&template).unwrap().ends_with(MARKER));

    // 대상 포트가 막히면 확정 전에 실패하고 VM은 그 노드에서 그대로 돈다. 받으려고 띄운 QEMU는 버린다.
    let listener = TcpListener::bind(("127.0.0.1", blocked)).unwrap();
    let task = c
        .post("/vms/web/migrate", &json!({ "node": "node1" }))
        .await
        .unwrap();
    let err = c.wait_task(&task).await.unwrap_err();
    assert!(
        err.to_string()
            .contains(&format!("no free migration port in {blocked}-{blocked}")),
        "{err}"
    );
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["phase"], "running", "{vm}");
        assert_eq!(vm["status"]["pid"].as_u64(), Some(pid), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    assert!(alive(pid));
    eventually("receiving QEMU discarded", || async {
        !process_with(&vm_run_dir(&p.d[0], &id))
    })
    .await;

    // 포트가 풀리면 되돌아온다.
    drop(listener);
    migrate(&p, 1, 0).await;

    // 디스크 내용은 그대로다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&disk, "qcow2", "read -P 0xab 0 64k"));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!disk.exists());
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// NFS 풀(두 노드가 각자 마운트한다)
#[cfg(all(feature = "nfs-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_an_nfs_pool() {
    let export = std::env::var("OPL_TEST_NFS_EXPORT")
        .expect("OPL_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1");
    let p = Pair::new("mig-nfs", ["60120-60129", "60130-60139"]).await;
    let d = &p.d[0];
    let source = format!("127.0.0.1:{export}");
    d.ctl(&["pool", "create", "nas", "--nfs", &source])
        .await
        .unwrap();
    p.wait_pool_ready("nas").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "nas", "--size", "64M",
    ])
    .await
    .unwrap();
    round_trip(&p, "mig-root").await;
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "nas"]).await.unwrap();
}

/// Ceph RBD 풀(`client.admin`)
#[cfg(all(feature = "rbd-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_an_rbd_pool() {
    let pool = std::env::var("OPL_TEST_RBD").expect("OPL_TEST_RBD must name a Ceph pool");
    let p = Pair::new("mig-rbd", ["60140-60149", "60150-60159"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    c.post(
        "/storage-pools",
        &json!({ "name": "ceph", "spec": { "backend": { "kind": "rbd", "pool": pool } } }),
    )
    .await
    .unwrap();
    p.wait_pool_ready("ceph").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "ceph", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    round_trip(&p, "mig-root").await;
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "ceph"]).await.unwrap();
}

/// 공유 LVM 풀(lvmlockd 공유 VG). 이미 배타로 쥔 LV에 `lvchange -aey`를 다시 해도 성공해야 한다
/// (대상이 넘겨받은 뒤 배타로 바꿀 때 기대는 동작).
#[cfg(all(feature = "lvm-shared-it", target_os = "linux"))]
#[tokio::test]
async fn migrates_a_vm_on_a_shared_lvm_pool() {
    let vg =
        std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG");
    let must = |program: &str, args: &[&str]| {
        let out = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .env("LVM_SUPPRESS_FD_WARNINGS", "1")
            .output()
            .unwrap_or_else(|e| panic!("run {program}: {e}"));
        assert!(out.status.success(), "{program} {args:?}: {out:?}");
    };
    // 두 opld가 한 호스트에서 함께 로그인하고 잠금을 시작하지 않도록, 타깃 로그인과 잠금 시작은 시험이
    // 먼저 하고 풀에는 iSCSI를 적지 않는다(opld는 잠금 시작만 다시 한다 — 이미 했으면 곧바로 끝난다).
    if let Ok(iscsi) = std::env::var("OPL_TEST_ISCSI") {
        let (portal, target) = iscsi
            .split_once(',')
            .expect("OPL_TEST_ISCSI must look like PORTAL,TARGET");
        let sessions = Command::new("iscsiadm")
            .args(["-m", "session"])
            .output()
            .expect("run iscsiadm");
        if !String::from_utf8_lossy(&sessions.stdout).contains(target) {
            must(
                "iscsiadm",
                &["-m", "discovery", "-t", "sendtargets", "-p", portal],
            );
            must(
                "iscsiadm",
                &["-m", "node", "-T", target, "-p", portal, "--login"],
            );
            must("udevadm", &["settle"]);
        }
    }
    must("vgchange", &["--lockstart", &vg]);

    let p = Pair::new("mig-lvm", ["60160-60169", "60170-60179"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    c.post(
        "/storage-pools",
        &json!({ "name": "san", "spec": { "backend": { "kind": "lvm-shared", "vg": vg } } }),
    )
    .await
    .unwrap();
    p.wait_pool_ready("san").await;
    d.ctl(&[
        "volume", "create", "mig-root", "--pool", "san", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/mig-root").await.unwrap();
    let lv = format!("{vg}/opl-{}", volume["id"].as_str().unwrap());

    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "mig-root", "--node", "node1",
        "--start",
    ])
    .await
    .unwrap();
    // 원본 노드가 배타로 쥔 LV
    must("lvchange", &["-aey", &lv]);
    migrate(&p, 0, 1).await;
    must("lvchange", &["-aey", &lv]);
    migrate(&p, 1, 0).await;
    must("lvchange", &["-aey", &lv]);
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "mig-root"]).await.unwrap();
    d.ctl(&["pool", "delete", "san"]).await.unwrap();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 724개, 실패 0

Run: `cargo test -p opld --features migration-it --test migration`
Expected: macOS에서 PASS 1개(TCG)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "test(opld): 라이브 마이그레이션 실제 시험(migration-it)과 CI"
```

---

### Task 7: oplctl: vm migrate

`oplctl vm migrate VM NODE`(`POST /vms/{vm}/migrate`, `wait_task_for`로 24시간까지 기다리고 "vm X: migrated to NODE", 시간이 지나면 Task id와 함께 아직 돌 수 있다고 알린다), `vm list`의 NODE 칸에 `원본 -> 대상`.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust

use crate::client::{Client, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
```

을 다음으로 바꾼다:

```rust

use crate::client::{Client, MIGRATE_WAIT, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
```

`crates/oplctl/src/cli.rs`에서

```rust
    Reboot {
        vm: String,
    },
    Delete {
```

을 다음으로 바꾼다:

```rust
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
```

`crates/oplctl/src/cli.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
                &["NAME", "POWER", "PHASE", "NODE", "PID", "ERROR"],
                vm_row,
            )?;
```

`crates/oplctl/src/cli.rs`에서

```rust
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Delete { vm, with_snapshots } => {
```

을 다음으로 바꾼다:

```rust
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
```

`crates/oplctl/src/cli.rs`에서

```rust
        }
    }
    Ok(())
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
```

을 다음으로 바꾼다:

```rust
        }
    }
    Ok(())
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
```

`crates/oplctl/src/cli.rs`에서

```rust
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
```

을 다음으로 바꾼다:

```rust
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "migrating", "id-a -> id-b", "42", "-"]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "42", "-"]
        );
```

`crates/oplctl/src/client.rs`에서

```rust
const TASK_WAIT: Duration = Duration::from_secs(15 * 60);
```

을 다음으로 바꾼다:

```rust
const TASK_WAIT: Duration = Duration::from_secs(15 * 60);

/// 라이브 마이그레이션 Task를 기다리는 시간. 마이그레이션은 서버 설정(`[migration] timeout`)만큼 걸릴 수 있다.
pub const MIGRATE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

`crates/oplctl/src/client.rs`에서

```rust
        let id = task["id"].as_str().context("response has no task id")?;
        let deadline = Instant::now() + TASK_WAIT;
        loop {
```

을 다음으로 바꾼다:

```rust
        let id = task["id"].as_str().context("response has no task id")?;
        match self.poll_task(id, TASK_WAIT).await? {
            Ok(task) => Ok(task),
            Err(task) => bail!(
                "task {id} did not finish in {} minutes (state: {}); is opld running on node {}?",
                TASK_WAIT.as_secs() / 60,
                task["status"]["state"].as_str().unwrap_or("unknown"),
                task["spec"]["node"].as_str().unwrap_or("?")
            ),
        }
    }

    /// `wait_task`와 같지만 `wait`만큼 기다린다. 시간이 지나도 Task는 아직 돌고 있을 수 있다.
    pub async fn wait_task_for(&self, task: &Value, wait: Duration) -> anyhow::Result<Value> {
        let id = task["id"].as_str().context("response has no task id")?;
        match self.poll_task(id, wait).await? {
            Ok(task) => Ok(task),
            Err(task) => bail!(
                "stopped waiting for task {id} after {} minutes (state: {}); it may still be running, see `oplctl task get {id}`",
                wait.as_secs() / 60,
                task["status"]["state"].as_str().unwrap_or("unknown"),
            ),
        }
    }

    /// Task가 끝나면 `Ok(Task)`, 실패하면 오류, `wait`이 지나면 마지막으로 읽은 Task를 `Err`로.
    async fn poll_task(&self, id: &str, wait: Duration) -> anyhow::Result<Result<Value, Value>> {
        let deadline = Instant::now() + wait;
        loop {
```

`crates/oplctl/src/client.rs`에서

```rust
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(task),
                Some("failed") => bail!(
```

을 다음으로 바꾼다:

```rust
            match task["status"]["state"].as_str() {
                Some("succeeded") => return Ok(Ok(task)),
                Some("failed") => bail!(
```

`crates/oplctl/src/client.rs`에서

```rust
                ),
                state if Instant::now() > deadline => bail!(
                    "task {id} did not finish in {} minutes (state: {}); is opld running on node {}?",
                    TASK_WAIT.as_secs() / 60,
                    state.unwrap_or("unknown"),
                    task["spec"]["node"].as_str().unwrap_or("?")
                ),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
```

을 다음으로 바꾼다:

```rust
                ),
                _ if Instant::now() > deadline => return Ok(Err(task)),
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
```

`crates/oplctl/src/client.rs`에서

```rust
    use super::*;
```

을 다음으로 바꾼다:

```rust
    use super::*;
    use serde_json::json;
```

`crates/oplctl/src/client.rs`에서

```rust
        assert!(fingerprint("not a pem").is_err());
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(fingerprint("not a pem").is_err());
    }

    /// 어떤 요청에도 `body`(JSON)로 답하는 HTTP 서버의 주소
    async fn serve_json(body: Value) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let body = body.to_string();
                tokio::spawn(async move {
                    let mut buf = vec![0; 4096];
                    let mut req = Vec::new();
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = s.read(&mut buf).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    s.write_all(resp.as_bytes()).await.unwrap();
                });
            }
        });
        format!("http://{addr}")
    }

    /// 정한 시간이 지나면 기다리기를 그만두고, Task가 아직 돌 수 있다고 Task id와 함께 알린다.
    #[tokio::test]
    async fn wait_task_for_gives_up_after_the_given_time() {
        let id = "0b7f3a52-3f0e-4d5c-9a51-2b8f0c1d2e3f";
        let task = json!({ "id": id, "spec": { "node": "n1" }, "status": { "state": "running" } });
        let client = Client::new(&serve_json(task.clone()).await, None, None).unwrap();
        let err = client
            .wait_task_for(&task, Duration::ZERO)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "stopped waiting for task {id} after 0 minutes (state: running); it may still be running, see `oplctl task get {id}`"
            )
        );
        // 끝난 Task는 그대로 돌려준다.
        let done = json!({ "id": id, "status": { "state": "succeeded" } });
        let client = Client::new(&serve_json(done.clone()).await, None, None).unwrap();
        assert_eq!(
            client.wait_task_for(&done, MIGRATE_WAIT).await.unwrap(),
            done
        );
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 727개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): vm migrate와 vm list의 마이그레이션 표시"
```

---

### Task 8: 웹: VM 마이그레이션

VM 상세의 "Migrate"(운영 권한, 실행 중일 때, 전원·스냅샷·되돌리기·복제 Task가 있으면 막음), 대상 노드 대화상자 `VmMigrateDialog`(`migrationTargets`: 온라인·유지보수 아님·같은 아키텍처·지금 노드 제외, 서버의 409를 대화상자에), `isMigrating`으로 다른 동작을 이유와 함께 막고(콘솔은 허용), 머리에 `원본 → 대상`, 대시보드는 마이그레이션 중 VM을 실행 중·원본 노드로 센다. en·ko.

**Files:**
- Create: `web/src/vms/VmMigrateDialog.test.tsx`
- Create: `web/src/vms/VmMigrateDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`
- Modify: `web/src/vms/VmCloneDialog.test.tsx`
- Modify: `web/src/vms/VmConsole.test.tsx`
- Modify: `web/src/vms/VmConsole.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmMenu.tsx`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/actions.test.ts`
- Modify: `web/src/vms/actions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json` 전체:

```json
{
  "errors": {
    "network": "Cannot reach the server.",
    "unauthorized": "Your session has ended. Sign in again.",
    "invalid_credentials": "Wrong username or password.",
    "forbidden": "You do not have permission to do this.",
    "cross_origin": "The request came from another site and was rejected.",
    "not_found": "It no longer exists.",
    "already_exists": "It already exists.",
    "name_taken": "That name is already in use.",
    "revision_mismatch": "Someone else changed it first. Reload and try again.",
    "mac_in_use": "That MAC address is used by another VM.",
    "volume_in_use": "A volume is attached to another VM.",
    "volume_not_ready": "A volume is not ready yet.",
    "vm_running": "Stop the VM first.",
    "vm_not_running": "The VM is not running.",
    "busy": "Another operation on this VM is in progress. Try again shortly.",
    "no_node": "No node can run this VM: {{message}}",
    "console_unavailable": "The console is not available right now.",
    "cluster_no_quorum": "The cluster has no quorum. The change may still be applied later.",
    "not_leader": "The cluster leader is changing. Try again shortly.",
    "network_in_use": "A VM uses this network. Remove its interface first.",
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_not_on_node": "That storage pool is not available on the node you are connected to.",
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "last_admin": "The last administrator cannot give up the administrator role.",
    "cannot_delete_self": "You cannot delete your own account.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert, clone or migration) is in progress. Try again when it has finished.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. Snapshots of disks in LVM-thin, ZFS or Ceph RBD pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
    "vm_has_snapshots": "The VM has snapshots. Delete them first.",
    "volume_in_snapshot": "A VM snapshot still uses this volume. Delete that snapshot first.",
    "snapshot_not_ready": "The snapshot is still being taken.",
    "node_offline": "The node holding this VM is offline.",
    "too_many_attempts": "Too many attempts. Try again later.",
    "machine_arch_mismatch": "The machine type doesn't match the selected node's architecture.",
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The target node is offline or in maintenance.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}"
  },
  "nav": {
    "dashboard": "Dashboard",
    "vms": "Virtual machines",
    "volumes": "Volumes",
    "pools": "Storage pools",
    "networks": "Networks",
    "nodes": "Nodes",
    "tasks": "Tasks",
    "users": "Users",
    "tokens": "API tokens",
    "cluster": "Cluster"
  },
  "header": {
    "language": "Language",
    "theme": "Theme",
    "themeSystem": "System",
    "themeLight": "Light",
    "themeDark": "Dark",
    "account": "Account",
    "logout": "Sign out"
  },
  "roles": {
    "admin": "Administrator",
    "operator": "Operator",
    "viewer": "Viewer"
  },
  "login": {
    "username": "Username",
    "password": "Password",
    "submit": "Sign in"
  },
  "notFound": {
    "title": "Page not found",
    "home": "Go to the dashboard"
  },
  "live": {
    "live": "Live",
    "connecting": "Connecting…",
    "offline": "Disconnected · reconnecting"
  },
  "tasks": {
    "time": "Time",
    "operation": "Operation",
    "error": "Error",
    "empty": "No tasks.",
    "emptyFailed": "No failed tasks.",
    "failedOnly": "Failed only",
    "done": "{{label}}: done",
    "failed": "{{label}}: failed. {{error}}",
    "gone": "{{label}}: result is no longer available",
    "state": {
      "pending": "Pending",
      "running": "Running",
      "succeeded": "Succeeded",
      "failed": "Failed"
    },
    "op": {
      "vm-start": "Start {{target}}",
      "vm-stop": "Stop {{target}}",
      "vm-force-stop": "Force stop {{target}}",
      "vm-reboot": "Reboot {{target}}",
      "volume-create": "Create volume {{target}}",
      "volume-import": "Import volume {{target}}",
      "volume-delete": "Delete volume {{target}}",
      "snapshot-create": "Take snapshot of {{target}}",
      "snapshot-revert": "Revert {{target}} to a snapshot",
      "snapshot-delete": "Delete snapshot of {{target}}",
      "volume-resize": "Resize volume {{target}}",
      "vm-clone": "Clone {{target}}",
      "vm-migrate": "Migrate {{target}}"
    }
  },
  "pager": {
    "label": "Pages",
    "previous": "Previous",
    "next": "Next",
    "range": "{{from}}–{{to}} of {{total}}"
  },
  "common": {
    "loading": "Loading…",
    "copied": "Copied.",
    "copyFailed": "Could not copy. Select the text and copy it yourself.",
    "cancel": "Cancel",
    "save": "Save",
    "create": "Create",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "done": "Done",
    "name": "Name",
    "state": "State",
    "node": "Node"
  },
  "confirm": {
    "typeName": "Type {{name}} to confirm"
  },
  "vms": {
    "vcpus": "vCPUs",
    "memory": "Memory",
    "actions": "Actions",
    "actionsFor": "Actions for {{name}}",
    "search": "Search by name",
    "empty": "No virtual machines.",
    "selectAll": "Select all",
    "select": "Select {{name}}",
    "selected_one": "{{count}} selected",
    "selected_other": "{{count}} selected",
    "deleted": "Deleted {{name}}.",
    "forceStopTitle": "Force stop {{name}}?",
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "reason": {
      "powerPending": "A power action is still in progress.",
      "reverting": "Not available while a snapshot is being reverted.",
      "running": "The VM is running.",
      "stopped": "The VM is stopped.",
      "failed": "The VM has failed.",
      "busy": "The VM is starting or stopping.",
      "migrating": "Not available while the VM is being migrated.",
      "snapshotPending": "Not available while a snapshot or clone task is in progress."
    },
    "deleteTitle": "Delete {{name}}?",
    "deleteDescription": "The VM is deleted. Its volumes are kept.",
    "deleteSnapshots_one": "Also delete its snapshot",
    "deleteSnapshots_other": "Also delete all {{count}} of its snapshots",
    "hasSnapshots": "A VM with snapshots cannot be deleted. Delete its snapshots first, or check the box to delete them along with the VM.",
    "snapshotsPending": "A snapshot of this VM is still being taken. Delete the VM once it is done.",
    "deletingSnapshots": "Deleting the snapshots one at a time. This cannot be cancelled once started.",
    "phase": {
      "stopped": "Stopped",
      "starting": "Starting",
      "running": "Running",
      "stopping": "Stopping",
      "failed": "Failed",
      "migrating": "Migrating"
    },
    "migratingTo": "{{from}} → {{to}}",
    "action": {
      "start": "Start",
      "stop": "Stop",
      "force-stop": "Force stop",
      "reboot": "Reboot",
      "console": "Console",
      "delete": "Delete",
      "edit": "Edit",
      "clone": "Clone",
      "migrate": "Migrate"
    },
    "notFound": "There is no VM {{name}}",
    "backToList": "Back to virtual machines",
    "tab": {
      "summary": "Summary",
      "hardware": "Hardware",
      "console": "Console",
      "snapshots": "Snapshots",
      "tasks": "Tasks",
      "json": "JSON"
    },
    "cpu": "CPU",
    "cpuValue": "{{sockets}} × {{cores}} cores ({{model}})",
    "firmware": "Firmware",
    "machine": "Machine type",
    "restart": "Restart policy",
    "power": "Desired power",
    "pid": "QEMU PID",
    "created": "Created",
    "lastError": "Last error",
    "restartPolicy": {
      "never": "Never",
      "on-failure": "On failure"
    },
    "powerState": {
      "running": "Running",
      "stopped": "Stopped"
    },
    "disks": "Disks",
    "volume": "Volume",
    "bus": "Bus",
    "bootOrder": "Boot order",
    "nics": "Network interfaces",
    "network": "Network",
    "model": "Model",
    "mac": "MAC address",
    "openConsole": "Open console",
    "actionFailed": "{{name}}: {{error}}",
    "bulkSummary": "{{action}}: {{done}} requested, {{failed}} failed, {{skipped}} skipped.",
    "noDisks": "No disks.",
    "noNics": "No network interfaces."
  },
  "snapshots": {
    "created": "Created",
    "memory": "Memory",
    "description": "Description",
    "empty": "No snapshots.",
    "current": "Current",
    "after": "(after {{parent}})",
    "pending": "Creating",
    "memoryYes": "Included",
    "memoryNo": "No",
    "memorySize": "Included ({{size}})",
    "create": "Take snapshot",
    "createTitle": "Take a snapshot of {{name}}",
    "createDescription": "Saves the VM's disks, firmware variables and settings so you can return to this point later.",
    "includeMemory": "Include memory",
    "includeMemoryHint": "Also saves the running state. Reverting resumes the VM where it was.",
    "unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. {{volume}} is {{format}}.",
    "busy": "The VM is starting, stopping or migrating. Snapshots can be taken, reverted and deleted once it settles.",
    "revert": "Revert",
    "revertNamed": "Revert to {{name}}",
    "revertTitle": "Revert {{vm}} to {{name}}?",
    "revertDescription": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot. It stays off.",
    "revertDescriptionMemory": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot, and it resumes running from the saved memory.",
    "deleteNamed": "Delete snapshot {{name}}",
    "deleteTitle": "Delete snapshot {{name}}?",
    "deleteDescription": "The snapshot is deleted. Snapshots taken after it are kept, and the VM's current state does not change.",
    "op": {
      "create": "Take snapshot {{name}} of {{vm}}",
      "revert": "Revert {{vm}} to {{name}}",
      "delete": "Delete snapshot {{name}} of {{vm}}"
    },
    "errors": {
      "exists": "This VM already has a snapshot with this name.",
      "description": "Use at most 1024 characters."
    },
    "scheduled": "Scheduled",
    "every": {
      "hourly": "Hourly",
      "daily": "Daily",
      "weekly": "Weekly"
    },
    "schedule": "Snapshot schedule: {{every}}, keep {{keep}}",
    "scheduleMemory": "Snapshot schedule: {{every}}, keep {{keep}}, with memory",
    "noSchedule": "No snapshot schedule.",
    "scheduleEdit": "Change schedule",
    "scheduleFailed": "Last scheduled snapshot failed:",
    "scheduleError": {
      "vm_busy": "Skipped because the VM was busy; it will try again later.",
      "node_offline": "Skipped because the VM's node is offline ({{message}})."
    },
    "mixed": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. This VM mixes them.",
    "blockHint": "Disks in LVM-thin, ZFS and Ceph RBD pools are saved without memory. A running VM pauses briefly while they are saved.",
    "zfsLatest": "Disks in ZFS pools can only revert to their latest snapshot.",
    "sharedLvm": "Disks in shared LVM pools cannot be snapshotted. {{volume}} is in one."
  },
  "clone": {
    "title": "Clone {{name}}",
    "description": "A new stopped VM is made with full copies of the disks. It gets new MAC addresses and no snapshot schedule.",
    "name": "New VM name",
    "source": "Source",
    "current": "Current state (stopped VM only)",
    "snapshot": "Snapshot {{name}}",
    "running": "The VM is not stopped. Stop it first or choose a snapshot.",
    "busy": "The VM is starting, stopping or migrating. Try again when it has settled.",
    "gone": "The chosen snapshot no longer exists or is not ready. Choose another source.",
    "goneOption": "Chosen snapshot (no longer available)",
    "pool": "Storage pool for the disks",
    "samePool": "Same pool as each source disk",
    "submit": "Clone",
    "op": "Clone {{vm}} to {{name}}",
    "errors": {
      "exists": "A VM with this name already exists.",
      "poolNotOnNode": "That storage pool is not available on the node holding the VM.",
      "poolUnavailable": "That storage pool is not ready on the node holding the VM."
    }
  },
  "migrate": {
    "title": "Migrate {{name}}",
    "description": "The running VM moves to another node without stopping. Its disks must all be on shared storage pools, and both nodes need the same QEMU version and CPU.",
    "node": "Target node",
    "noTarget": "No other online node with the VM's architecture is available.",
    "submit": "Migrate",
    "op": "Migrate {{vm}} to {{node}}"
  },
  "forms": {
    "name": "Use 1–63 lowercase letters, digits and -, not starting or ending with -."
  },
  "volumes": {
    "pool": "Storage pool",
    "size": "Size",
    "allocated": "Allocated",
    "format": "Format",
    "vm": "Used by",
    "empty": "No volumes.",
    "phase": {
      "pending": "Creating",
      "ready": "Ready"
    },
    "createTitle": "Create an empty volume",
    "createDescription": "A new disk image in a storage pool. A qcow2 volume takes space only as it is written.",
    "deleteTitle": "Delete volume {{name}}?",
    "deleteDescription": "The disk image and all data on it are deleted.",
    "resize": "Resize",
    "resizeNamed": "Resize {{name}}",
    "resizeTitle": "Resize volume {{name}}",
    "resizeDescription": "Volumes can only grow. A running VM sees the new size right away; grow the partition and file system inside the guest yourself.",
    "currentSize": "Current size: {{size}}",
    "newSize": "New size (GiB)",
    "errors": {
      "reserved": "The name import is reserved.",
      "exists": "A volume with this name already exists.",
      "grow": "Enter a size larger than the current size."
    },
    "reason": {
      "inUse": "Remove it from the VM first.",
      "notReady": "Available once ready.",
      "vmsUnknown": "Cannot tell which VMs use it."
    }
  },
  "uploads": {
    "open": "Upload image",
    "title": "Upload a disk image",
    "description": "The file goes to the node you are connected to ({{node}}), so only storage pools on that node can be chosen. The image is converted into a new volume.",
    "file": "Image file",
    "sourceFormat": "File format",
    "submit": "Upload",
    "noPool": "No storage pool is available on this node.",
    "progress": "Uploading {{name}}: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "Cancelled the upload of {{name}}.",
    "failed": "Upload of {{name}} failed. {{error}}",
    "processing": "Processing {{name}}…",
    "noResponse": "{{name}}: no response from the server. Check the volume list for the result.",
    "dropped": "The server rejected the upload or the connection was lost.",
    "noNode": "Cannot tell which node you are connected to. {{error}}",
    "errors": {
      "file": "Choose a file."
    }
  },
  "pools": {
    "path": "Directory",
    "nodes": "Nodes",
    "free": "Free / total",
    "volumes": "Volumes",
    "empty": "No storage pools.",
    "allNodes": "All nodes",
    "createTitle": "Create a storage pool",
    "createDescription": "Where volumes are kept: a directory on each node, an NFS export that the nodes mount, an LVM thin pool or ZFS dataset on each node, a Ceph RBD pool, or a volume group shared by the nodes.",
    "pathHint": "An absolute path on the nodes.",
    "shared": "Shared storage",
    "sharedHint": "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export, images in its Ceph pool and logical volumes in its shared volume group are kept.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "kind": "Kind",
    "kinds": {
      "dir": "Directory",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD",
      "lvm-shared": "Shared LVM"
    },
    "location": "Location",
    "kindHint": {
      "dir": "Create the directory on each node first.",
      "nfs": "Each node mounts the export under its data directory. Mounting needs Linux nodes running as root. The pool is shared by its nodes.",
      "lvm-thin": "Create the volume group and thin pool on each node first. Volumes are raw thin volumes kept on the node that creates them, and their snapshots hold disks only. Needs Linux nodes running as root.",
      "zfs": "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root.",
      "rbd": "Create the Ceph pool first and put ceph.conf and the keyring on each node. Volumes are raw RBD images that every node of the pool can use, and their snapshots hold disks only. Needs Linux nodes with QEMU's rbd driver (qemu-block-extra) and the rbd command (ceph-common).",
      "lvm-shared": "Create the shared volume group (vgcreate --shared) on a LUN every node sees first, and run lvmlockd with sanlock or dlm on each node. Volumes are raw logical volumes that every node of the pool can use, without snapshots. Needs Linux nodes running as root."
    },
    "server": "NFS server",
    "serverHint": "Host name or IP address.",
    "export": "Export path",
    "exportHint": "The absolute path the server exports, such as /srv/vm.",
    "options": "Mount options (optional)",
    "optionsHint": "Comma-separated options passed to mount -o after nosuid,nodev,noexec,retry=0, such as vers=4.2,hard.",
    "state": {
      "ready": "Ready",
      "notReady": "Not ready",
      "unknown": "No status reported yet",
      "offline": "Offline"
    },
    "errors": {
      "path": "Enter an absolute path (starting with /).",
      "export": "Enter an absolute path (starting with /) without control characters.",
      "nodesGone": "The chosen nodes are no longer in the cluster. Choose again.",
      "server": "Enter a host name or IP address without spaces, commas, slashes or brackets.",
      "options": "Separate the options with commas, without spaces. bind, rbind, remount, move, suid, dev, exec, helper, bg and x-* options are not allowed.",
      "lvmName": "Use 1 to 127 letters, digits, +, _, . or -, not starting with -, snapshot or pvmove. LVM also reserves . and .. and names containing parts like _tdata or _tmeta.",
      "zfsDataset": "Use a dataset name such as tank/opl, at most 177 characters: parts separated by / with letters, digits, _, ., : or -, none empty, . or .. or starting with -. The pool name comes first and starts with a letter.",
      "rbdName": "Use 1 to 63 letters, digits, _, . or -, not starting with -.",
      "cephUser": "Write the Ceph user without the \"client.\" prefix, for example opl instead of client.opl.",
      "rbdConf": "Enter an absolute path (starting with /) without control characters, commas, colons or equals signs.",
      "portal": "Enter a host name or IP address with an optional :port (1–65535), without spaces, commas or slashes. Put IPv6 addresses in brackets, such as [fd00::5]:3260.",
      "target": "Use a target name starting with iqn., eui. or naa., at most 223 characters of lowercase letters, digits, ., : or -."
    },
    "vg": "Volume group",
    "thinpool": "Thin pool",
    "dataset": "Dataset",
    "cephPool": "Ceph pool",
    "namespace": "Namespace (optional)",
    "cephUser": "Ceph user (optional)",
    "cephUserHint": "The Ceph client ID without the \"client.\" prefix. Uses admin if empty.",
    "cephConf": "ceph.conf path (optional)",
    "cephConfHint": "An absolute path on the nodes, /etc/ceph/ceph.conf if empty.",
    "iscsi": "Log in to an iSCSI target",
    "iscsiHint": "Each node logs in to the target before using the volume group. Leave unchecked when the nodes already see the LUN, such as over Fibre Channel.",
    "portal": "iSCSI portal",
    "portalHint": "Host name or IP address with an optional port, such as san.lan:3260 or [fd00::5]:3260.",
    "target": "iSCSI target",
    "targetHint": "The target name, such as iqn.2003-01.org.example:storage."
  },
  "networks": {
    "bridge": "Bridge",
    "vlan": "VLAN",
    "vlanOptional": "VLAN tag (optional)",
    "vlanHint": "1–4094. The bridge must be VLAN-aware (vlan_filtering 1).",
    "nodes": "Nodes with the bridge",
    "vms": "VMs",
    "empty": "No networks.",
    "userNote": "The user network (QEMU NAT) is always available and is not listed.",
    "noNode": "No node has this bridge",
    "noVlanFiltering": "VLAN filtering is off on {{nodes}}",
    "createTitle": "Create a network",
    "createDescription": "Points VM interfaces at a Linux bridge that already exists on the nodes. VMs on this network run only on nodes with the bridge.",
    "created": "Created network {{name}}.",
    "deleteTitle": "Delete network {{name}}?",
    "deleteDescription": "Only the network is removed. The bridges on the nodes are not changed.",
    "deleted": "Deleted network {{name}}.",
    "inUse": "Remove the VM interfaces first.",
    "errors": {
      "reserved": "The name user is reserved for NAT.",
      "bridge": "Use 1–15 characters without /, : or spaces.",
      "vlan": "Use a number from 1 to 4094."
    }
  },
  "nodes": {
    "address": "API address",
    "hypervisor": "Hypervisor",
    "arch": "Architecture",
    "cpus": "CPUs",
    "memory": "Memory",
    "bridges": "Bridges",
    "pools": "Storage pools",
    "vms": "VMs",
    "thisNode": "(this node)",
    "joinNote": "To add a node, issue a join token on the Cluster page and run oplctl cluster join on the new node.",
    "maintenanceNote": "No new VMs are placed on a node in maintenance. VMs already running there keep running.",
    "startMaintenance": "Enter maintenance",
    "endMaintenance": "End maintenance",
    "maintenanceOn": "{{name}} is in maintenance.",
    "maintenanceOff": "{{name}} is out of maintenance.",
    "remove": "Remove from cluster",
    "removeTitle": "Remove {{name}} from the cluster?",
    "removeDescription": "Only for a node that is dead for good. It cannot rejoin with its current data, and VMs assigned to it stay assigned.",
    "removeNote": "Only offline nodes can be removed here. To remove a live node, run oplctl cluster leave on it.",
    "removed": "Removed {{name}} from the cluster.",
    "reason": {
      "thisNode": "You are connected through this node.",
      "online": "The node is online. Run oplctl cluster leave on it."
    }
  },
  "accounts": {
    "currentPassword": "Current password",
    "changePassword": "Change my password",
    "changePasswordDescription": "All your sessions end. Sign in again with the new password.",
    "passwordChanged": "Your password was changed. Sign in with the new password.",
    "newPassword": "New password",
    "confirmPassword": "Confirm the new password",
    "passwordHint": "At least 8 characters.",
    "errors": {
      "password": "Use at least 8 characters.",
      "mismatch": "The passwords do not match.",
      "current": "Enter your current password.",
      "wrongCurrent": "The current password is wrong."
    }
  },
  "users": {
    "role": "Role",
    "you": "(you)",
    "empty": "No users.",
    "roleHint": "Operators change VMs and volumes. Viewers only read. Neither can see accounts.",
    "createTitle": "Create a user",
    "created": "Created user {{name}}.",
    "changeRole": "Change role",
    "roleTitle": "Change the role of {{name}}",
    "roleChanged": "{{name}} is now {{role}}.",
    "resetPassword": "Reset password",
    "passwordTitle": "Reset the password of {{name}}",
    "passwordDescription": "The user's sessions end and they sign in with the new password.",
    "passwordReset": "Reset the password of {{name}}.",
    "deleteTitle": "Delete user {{name}}?",
    "deleteDescription": "The user and their API tokens are deleted.",
    "deleted": "Deleted user {{name}}.",
    "adminOnly": "Only administrators can manage users.",
    "selfNote": "Change your own password from the account menu. Only another administrator can change your role or delete your account.",
    "reason": {
      "selfRole": "Only another administrator can change your role.",
      "selfPassword": "Change your own password from the account menu.",
      "selfDelete": "Only another administrator can delete your account."
    }
  },
  "tokens": {
    "owner": "Owner",
    "created": "Created",
    "empty": "No API tokens.",
    "note": "Scripts and tools use API tokens. A token has the role of its owner. oplctl login makes one for the CLI.",
    "createTitle": "Create an API token",
    "createDescription": "The token acts as you, with your role.",
    "secretTitle": "Token {{name}} was created",
    "secretDescription": "Copy the token now. It cannot be shown again.",
    "secret": "Token",
    "copySecret": "Copy the token",
    "usage": "Send it in the header",
    "deleteTitle": "Delete API token {{name}}?",
    "deleteDescription": "Anything using the token stops working right away.",
    "deleted": "Deleted API token {{name}}."
  },
  "cluster": {
    "title": "Cluster {{name}}",
    "ca": "Certificate authority",
    "caNote": "Every node's HTTPS certificate is signed by the cluster CA. Import the CA into your browser to stop the certificate warning, and check that its fingerprint matches. Browsers show the SHA-256 fingerprint in uppercase hex, in pairs separated by colons or spaces; compare it with the hex after sha256: (letter case does not matter).",
    "copyFingerprint": "Copy the CA fingerprint",
    "downloadCa": "Download the CA",
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
    "role": "Role",
    "leader": "Leader",
    "voter": "Voter",
    "learner": "Learner",
    "clusterAddress": "Cluster address",
    "issue": "Issue a join token",
    "issueTitle": "Issue a join token",
    "issueDescription": "A new node joins with the token once. Start opld on the new node first.",
    "ttl": "Valid for",
    "ttlOption": {
      "3600": "1 hour",
      "86400": "1 day",
      "604800": "7 days"
    },
    "issuedTitle": "Join a new node",
    "issuedDescription": "Run this on the new node before {{time}}. It cannot be shown again.",
    "command": "Command",
    "copyCommand": "Copy the command",
    "commandNote": "Add --socket if opld on the new node does not use /run/opl/opld.sock."
  },
  "vmForm": {
    "createTitle": "Create a virtual machine",
    "editTitle": "Edit {{name}}",
    "runningNote": "The VM is running: its node and disks cannot change. Other changes apply from the next start.",
    "general": "General",
    "cores": "vCPU cores",
    "memory": "Memory (MiB)",
    "cpuModel": "CPU model",
    "nodeAuto": "Automatic",
    "diskN": "Disk {{n}}",
    "newVolume": "New volume",
    "pool": "Storage pool",
    "sizeGib": "Size (GiB)",
    "removeDisk": "Remove disk {{n}}",
    "addExisting": "Attach a volume",
    "addNew": "New empty volume",
    "nicN": "Network interface {{n}}",
    "userNetwork": "user (NAT)",
    "macAuto": "Automatic",
    "removeNic": "Remove interface {{n}}",
    "addNic": "Add an interface",
    "startAfter": "Start after creating",
    "reload": "Reload",
    "errors": {
      "name": "Use 1–63 lowercase letters, digits and -, not starting or ending with -.",
      "cores": "From 1 to 65535.",
      "memory": "At least 64 MiB.",
      "volume": "Choose or name a volume.",
      "pool": "Choose a storage pool.",
      "size": "Enter a size.",
      "bootOrder": "Boot orders must be unique numbers from 1 to 255.",
      "mac": "Use the form 52:54:00:12:34:56.",
      "volumeChoose": "Choose a volume.",
      "volumeDuplicate": "Each disk needs a different volume.",
      "volumeExists": "A volume with this name already exists.",
      "macMulticast": "A multicast address (odd first byte) cannot be used.",
      "macDuplicate": "Each interface needs a different MAC address.",
      "keep": "From 1 to 100."
    },
    "volumesCreated": "Volumes {{names}} were created; {{error}}",
    "schedule": "Scheduled snapshots",
    "scheduleNote": "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    "scheduleEvery": "Take snapshots",
    "scheduleOff": "Off",
    "scheduleKeep": "Snapshots to keep",
    "scheduleRaw": "{{volume}} is {{format}}, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    "scheduleMemoryHint": "Memory is saved only while the VM is running and has disks.",
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    "scheduleSharedLvm": "{{volume}} is in a shared LVM pool, so scheduled snapshots will fail."
  },
  "console": {
    "connecting": "Connecting…",
    "connected": "Connected",
    "failed": "Disconnected. The VM may have stopped or the console is not available.",
    "reconnect": "Reconnect",
    "paste": "Type clipboard",
    "pasteDenied": "The browser did not allow reading the clipboard.",
    "fullscreen": "Full screen",
    "pasteTruncated": "Only the first {{count}} characters were typed.",
    "screen": "VM screen",
    "hint": "Click the screen to type into the VM. Click outside it to release the keyboard.",
    "forbidden": "You do not have permission to use the console."
  },
  "dashboard": {
    "nodes": "Nodes",
    "online": "online",
    "vms": "Virtual machines",
    "vmCounts": "{{running}} running · {{stopped}} stopped · {{failed}} failed",
    "leader": "Leader",
    "perNode": "Nodes",
    "hypervisor": "Hypervisor",
    "activeVms": "Active VMs",
    "nodeOnline": "Online",
    "nodeOffline": "Offline",
    "maintenance": "Maintenance",
    "failedVms": "Failed virtual machines",
    "recentTasks": "Recent tasks",
    "allTasks": "All tasks",
    "noLeader": "No leader",
    "noNodes": "No nodes."
  }
}
```

`web/src/i18n/ko.json` 전체:

```json
{
  "errors": {
    "network": "서버에 연결할 수 없습니다.",
    "unauthorized": "세션이 끝났습니다. 다시 로그인하세요.",
    "invalid_credentials": "사용자 이름이나 비밀번호가 틀렸습니다.",
    "forbidden": "이 작업을 할 권한이 없습니다.",
    "cross_origin": "다른 사이트에서 온 요청이라 거부했습니다.",
    "not_found": "더 이상 존재하지 않습니다.",
    "already_exists": "이미 있습니다.",
    "name_taken": "이미 쓰는 이름입니다.",
    "revision_mismatch": "다른 사람이 먼저 바꿨습니다. 다시 불러온 뒤 시도하세요.",
    "mac_in_use": "다른 VM이 쓰는 MAC 주소입니다.",
    "volume_in_use": "다른 VM에 붙어 있는 볼륨입니다.",
    "volume_not_ready": "아직 준비되지 않은 볼륨입니다.",
    "vm_running": "먼저 VM을 정지하세요.",
    "vm_not_running": "VM이 실행 중이 아닙니다.",
    "busy": "이 VM에 다른 작업이 진행 중입니다. 잠시 뒤 다시 시도하세요.",
    "no_node": "이 VM을 실행할 노드가 없습니다: {{message}}",
    "console_unavailable": "지금은 콘솔을 열 수 없습니다.",
    "cluster_no_quorum": "클러스터 과반수가 없습니다. 변경은 나중에 적용될 수 있습니다.",
    "not_leader": "클러스터 리더가 바뀌는 중입니다. 잠시 뒤 다시 시도하세요.",
    "network_in_use": "이 네트워크를 쓰는 VM이 있습니다. 먼저 그 인터페이스를 빼세요.",
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_not_on_node": "지금 접속한 노드에서 쓸 수 없는 스토리지 풀입니다.",
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "last_admin": "마지막 관리자는 관리자 역할을 내려놓을 수 없습니다.",
    "cannot_delete_self": "자기 계정은 지울 수 없습니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제, 마이그레이션)이 진행 중입니다. 끝나면 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. LVM-thin·ZFS·Ceph RBD 풀의 디스크 스냅샷에는 메모리를 담을 수 없고, ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
    "vm_has_snapshots": "VM에 스냅샷이 있습니다. 먼저 스냅샷을 지우세요.",
    "volume_in_snapshot": "VM 스냅샷이 아직 이 볼륨을 씁니다. 먼저 그 스냅샷을 지우세요.",
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "too_many_attempts": "시도가 너무 많습니다. 잠시 뒤에 다시 하세요.",
    "machine_arch_mismatch": "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다.",
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "대상 노드가 오프라인이거나 유지보수 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}"
  },
  "nav": {
    "dashboard": "대시보드",
    "vms": "가상 머신",
    "volumes": "볼륨",
    "pools": "스토리지 풀",
    "networks": "네트워크",
    "nodes": "노드",
    "tasks": "작업",
    "users": "사용자",
    "tokens": "API 토큰",
    "cluster": "클러스터"
  },
  "header": {
    "language": "언어",
    "theme": "테마",
    "themeSystem": "시스템",
    "themeLight": "밝게",
    "themeDark": "어둡게",
    "account": "계정",
    "logout": "로그아웃"
  },
  "roles": {
    "admin": "관리자",
    "operator": "운영자",
    "viewer": "조회자"
  },
  "login": {
    "username": "사용자 이름",
    "password": "비밀번호",
    "submit": "로그인"
  },
  "notFound": {
    "title": "페이지를 찾을 수 없습니다",
    "home": "대시보드로 가기"
  },
  "live": {
    "live": "실시간",
    "connecting": "연결 중…",
    "offline": "실시간 연결 끊김 · 재연결 중"
  },
  "tasks": {
    "time": "시각",
    "operation": "작업",
    "error": "오류",
    "empty": "작업이 없습니다.",
    "emptyFailed": "실패한 작업이 없습니다.",
    "failedOnly": "실패만",
    "done": "{{label}}: 완료",
    "failed": "{{label}}: 실패. {{error}}",
    "gone": "{{label}}: 결과를 더 이상 볼 수 없습니다",
    "state": {
      "pending": "대기",
      "running": "진행 중",
      "succeeded": "성공",
      "failed": "실패"
    },
    "op": {
      "vm-start": "{{target}} 시작",
      "vm-stop": "{{target}} 정지",
      "vm-force-stop": "{{target}} 강제 정지",
      "vm-reboot": "{{target}} 재시작",
      "volume-create": "볼륨 {{target}} 만들기",
      "volume-import": "볼륨 {{target}} 가져오기",
      "volume-delete": "볼륨 {{target}} 삭제",
      "snapshot-create": "{{target}} 스냅샷 만들기",
      "snapshot-revert": "{{target}} 스냅샷으로 되돌리기",
      "snapshot-delete": "{{target}} 스냅샷 삭제",
      "volume-resize": "볼륨 {{target}} 크기 늘리기",
      "vm-clone": "{{target}} 복제",
      "vm-migrate": "{{target}} 마이그레이션"
    }
  },
  "pager": {
    "label": "페이지",
    "previous": "이전",
    "next": "다음",
    "range": "{{from}}–{{to}} / {{total}}"
  },
  "common": {
    "loading": "불러오는 중…",
    "copied": "복사했습니다.",
    "copyFailed": "복사하지 못했습니다. 글을 선택해 직접 복사하세요.",
    "cancel": "취소",
    "save": "저장",
    "create": "만들기",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "done": "완료",
    "name": "이름",
    "state": "상태",
    "node": "노드"
  },
  "confirm": {
    "typeName": "확인하려면 {{name}}을(를) 입력하세요"
  },
  "vms": {
    "vcpus": "vCPU",
    "memory": "메모리",
    "actions": "동작",
    "actionsFor": "{{name}} 동작",
    "search": "이름 검색",
    "empty": "가상 머신이 없습니다.",
    "selectAll": "모두 선택",
    "select": "{{name}} 선택",
    "selected_one": "{{count}}개 선택",
    "selected_other": "{{count}}개 선택",
    "deleted": "{{name}}을(를) 삭제했습니다.",
    "forceStopTitle": "{{name}}을(를) 강제 정지할까요?",
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "reason": {
      "powerPending": "전원 작업이 아직 처리 중입니다.",
      "reverting": "스냅샷을 되돌리는 중에는 할 수 없습니다.",
      "running": "VM이 실행 중입니다.",
      "stopped": "VM이 정지되어 있습니다.",
      "failed": "VM이 실패 상태입니다.",
      "busy": "VM이 시작하거나 정지하는 중입니다.",
      "migrating": "VM을 마이그레이션하는 중에는 할 수 없습니다.",
      "snapshotPending": "스냅샷이나 복제 작업 중에는 할 수 없습니다."
    },
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
    "deleteDescription": "VM을 삭제합니다. 볼륨은 남습니다.",
    "deleteSnapshots_one": "스냅샷 {{count}}개도 모두 지우기",
    "deleteSnapshots_other": "스냅샷 {{count}}개도 모두 지우기",
    "hasSnapshots": "스냅샷이 있는 VM은 지울 수 없습니다. 먼저 스냅샷을 지우거나, 위 칸을 골라 VM과 함께 지우세요.",
    "snapshotsPending": "이 VM의 스냅샷을 만드는 중입니다. 다 만든 뒤 VM을 지우세요.",
    "deletingSnapshots": "스냅샷을 하나씩 지우는 중입니다. 시작한 뒤에는 취소할 수 없습니다.",
    "phase": {
      "stopped": "정지",
      "starting": "시작 중",
      "running": "실행 중",
      "stopping": "정지 중",
      "failed": "실패",
      "migrating": "마이그레이션 중"
    },
    "migratingTo": "{{from}} → {{to}}",
    "action": {
      "start": "시작",
      "stop": "정지",
      "force-stop": "강제 정지",
      "reboot": "재시작",
      "console": "콘솔",
      "delete": "삭제",
      "edit": "편집",
      "clone": "복제",
      "migrate": "마이그레이션"
    },
    "notFound": "{{name}} VM이 없습니다",
    "backToList": "가상 머신 목록으로",
    "tab": {
      "summary": "요약",
      "hardware": "하드웨어",
      "console": "콘솔",
      "snapshots": "스냅샷",
      "tasks": "작업",
      "json": "JSON"
    },
    "cpu": "CPU",
    "cpuValue": "{{sockets}} × {{cores}}코어 ({{model}})",
    "firmware": "펌웨어",
    "machine": "머신 타입",
    "restart": "재시작 정책",
    "power": "원하는 전원 상태",
    "pid": "QEMU PID",
    "created": "만든 시각",
    "lastError": "마지막 오류",
    "restartPolicy": {
      "never": "하지 않음",
      "on-failure": "실패하면"
    },
    "powerState": {
      "running": "켜짐",
      "stopped": "꺼짐"
    },
    "disks": "디스크",
    "volume": "볼륨",
    "bus": "버스",
    "bootOrder": "부팅 순서",
    "nics": "네트워크 인터페이스",
    "network": "네트워크",
    "model": "모델",
    "mac": "MAC 주소",
    "openConsole": "콘솔 열기",
    "actionFailed": "{{name}}: {{error}}",
    "bulkSummary": "{{action}}: {{done}}개 요청, {{failed}}개 실패, {{skipped}}개 건너뜀.",
    "noDisks": "디스크가 없습니다.",
    "noNics": "네트워크 인터페이스가 없습니다."
  },
  "snapshots": {
    "created": "만든 시각",
    "memory": "메모리",
    "description": "설명",
    "empty": "스냅샷이 없습니다.",
    "current": "현재 위치",
    "after": "({{parent}} 다음)",
    "pending": "만드는 중",
    "memoryYes": "포함",
    "memoryNo": "없음",
    "memorySize": "포함 ({{size}})",
    "create": "스냅샷 만들기",
    "createTitle": "{{name}} 스냅샷 만들기",
    "createDescription": "VM의 디스크, 펌웨어 변수, 설정을 저장해 나중에 이 시점으로 돌아올 수 있게 합니다.",
    "includeMemory": "메모리 포함",
    "includeMemoryHint": "실행 상태도 저장합니다. 되돌리면 VM이 그때 상태에서 이어서 실행됩니다.",
    "unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. {{volume}}은(는) {{format}}입니다.",
    "busy": "VM이 시작·정지·마이그레이션하는 중입니다. 끝나면 스냅샷을 만들고 되돌리고 지울 수 있습니다.",
    "revert": "되돌리기",
    "revertNamed": "{{name}}(으)로 되돌리기",
    "revertTitle": "{{vm}}을(를) {{name}}(으)로 되돌릴까요?",
    "revertDescription": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌립니다. VM은 꺼진 채로 남습니다.",
    "revertDescriptionMemory": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌리고, 저장한 메모리에서 이어서 실행합니다.",
    "deleteNamed": "스냅샷 {{name}} 삭제",
    "deleteTitle": "스냅샷 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "스냅샷을 지웁니다. 그 뒤에 만든 스냅샷은 남고, VM의 지금 상태는 바뀌지 않습니다.",
    "op": {
      "create": "{{vm}} 스냅샷 {{name}} 만들기",
      "revert": "{{vm}}을(를) {{name}}(으)로 되돌리기",
      "delete": "{{vm}} 스냅샷 {{name}} 삭제"
    },
    "errors": {
      "exists": "이 VM에 같은 이름의 스냅샷이 이미 있습니다.",
      "description": "1024자까지 쓰세요."
    },
    "scheduled": "예약",
    "every": {
      "hourly": "매시간",
      "daily": "매일",
      "weekly": "매주"
    },
    "schedule": "예약 스냅샷: {{every}}, {{keep}}개 보관",
    "scheduleMemory": "예약 스냅샷: {{every}}, {{keep}}개 보관, 메모리 포함",
    "noSchedule": "스냅샷 예약이 없습니다.",
    "scheduleEdit": "예약 바꾸기",
    "scheduleFailed": "마지막 예약 스냅샷 실패:",
    "scheduleError": {
      "vm_busy": "VM이 다른 작업 중이라 건너뛰었습니다. 다음 기회에 다시 합니다.",
      "node_offline": "VM의 노드가 오프라인이라 건너뛰었습니다 ({{message}})."
    },
    "mixed": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. 이 VM에는 여러 방식이 섞여 있습니다.",
    "blockHint": "LVM-thin·ZFS·Ceph RBD 풀의 디스크는 메모리 없이 저장합니다. 실행 중인 VM은 저장하는 동안 잠깐 멈춥니다.",
    "zfsLatest": "ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
    "sharedLvm": "공유 LVM 풀의 디스크는 스냅샷을 만들 수 없습니다. {{volume}}이(가) 공유 LVM 풀에 있습니다."
  },
  "clone": {
    "title": "{{name}} 복제",
    "description": "디스크를 모두 복사해 꺼진 새 VM을 만듭니다. MAC 주소는 새로 정하고 예약 스냅샷은 없습니다.",
    "name": "새 VM 이름",
    "source": "원본",
    "current": "지금 상태(꺼진 VM만)",
    "snapshot": "스냅샷 {{name}}",
    "running": "VM이 꺼져 있지 않습니다. 먼저 정지하거나 스냅샷을 고르세요.",
    "busy": "VM이 시작·정지·마이그레이션하는 중입니다. 끝난 뒤 다시 시도하세요.",
    "gone": "고른 스냅샷이 없어졌거나 준비된 상태가 아닙니다. 원본을 다시 고르세요.",
    "goneOption": "고른 스냅샷(이제 없음)",
    "pool": "디스크를 둘 스토리지 풀",
    "samePool": "원본 디스크와 같은 풀",
    "submit": "복제",
    "op": "{{vm}}을(를) {{name}}(으)로 복제",
    "errors": {
      "exists": "같은 이름의 VM이 이미 있습니다.",
      "poolNotOnNode": "VM이 있는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "VM이 있는 노드에서 준비되지 않은 스토리지 풀입니다."
    }
  },
  "migrate": {
    "title": "{{name}} 마이그레이션",
    "description": "실행 중인 VM을 멈추지 않고 다른 노드로 옮깁니다. 디스크가 모두 공유 스토리지 풀에 있어야 하고, 두 노드의 QEMU 버전과 CPU가 같아야 합니다.",
    "node": "대상 노드",
    "noTarget": "VM의 아키텍처를 가진 다른 온라인 노드가 없습니다.",
    "submit": "마이그레이션",
    "op": "{{vm}}을(를) {{node}}(으)로 마이그레이션"
  },
  "forms": {
    "name": "소문자·숫자·-로 1–63자, -로 시작하거나 끝날 수 없습니다."
  },
  "volumes": {
    "pool": "스토리지 풀",
    "size": "크기",
    "allocated": "사용량",
    "format": "형식",
    "vm": "사용하는 VM",
    "empty": "볼륨이 없습니다.",
    "phase": {
      "pending": "만드는 중",
      "ready": "준비됨"
    },
    "createTitle": "빈 볼륨 만들기",
    "createDescription": "스토리지 풀에 새 디스크 이미지를 만듭니다. qcow2 볼륨은 쓴 만큼만 공간을 차지합니다.",
    "deleteTitle": "볼륨 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "디스크 이미지와 그 안의 데이터가 모두 지워집니다.",
    "resize": "크기 늘리기",
    "resizeNamed": "{{name}} 크기 늘리기",
    "resizeTitle": "볼륨 {{name}} 크기 늘리기",
    "resizeDescription": "볼륨은 늘리기만 할 수 있습니다. 실행 중인 VM은 바로 새 크기를 봅니다. 게스트 안의 파티션과 파일 시스템은 직접 늘리세요.",
    "currentSize": "지금 크기: {{size}}",
    "newSize": "새 크기(GiB)",
    "errors": {
      "reserved": "import는 예약된 이름입니다.",
      "exists": "같은 이름의 볼륨이 이미 있습니다.",
      "grow": "지금 크기보다 큰 값을 넣으세요."
    },
    "reason": {
      "inUse": "먼저 VM에서 빼세요.",
      "notReady": "준비되면 쓸 수 있습니다.",
      "vmsUnknown": "어느 VM이 쓰는지 알 수 없습니다."
    }
  },
  "uploads": {
    "open": "이미지 올리기",
    "title": "디스크 이미지 올리기",
    "description": "파일은 지금 접속한 노드({{node}})로 올라가므로 그 노드에서 쓸 수 있는 스토리지 풀만 고를 수 있습니다. 이미지는 새 볼륨으로 변환됩니다.",
    "file": "이미지 파일",
    "sourceFormat": "파일 형식",
    "submit": "올리기",
    "noPool": "이 노드에서 쓸 수 있는 스토리지 풀이 없습니다.",
    "progress": "{{name}} 올리는 중: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "{{name}} 업로드를 취소했습니다.",
    "failed": "{{name}} 업로드에 실패했습니다. {{error}}",
    "processing": "{{name}} 처리 중…",
    "noResponse": "{{name}}: 서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요.",
    "dropped": "서버가 업로드를 거절했거나 연결이 끊겼습니다.",
    "noNode": "접속한 노드를 알 수 없습니다. {{error}}",
    "errors": {
      "file": "파일을 고르세요."
    }
  },
  "pools": {
    "path": "디렉터리",
    "nodes": "노드",
    "free": "남은 공간 / 전체",
    "volumes": "볼륨",
    "empty": "스토리지 풀이 없습니다.",
    "allNodes": "모든 노드",
    "createTitle": "스토리지 풀 만들기",
    "createDescription": "볼륨을 두는 곳입니다: 각 노드의 디렉터리, 노드들이 마운트하는 NFS 내보내기, 각 노드의 LVM thin pool이나 ZFS 데이터셋, Ceph RBD 풀, 또는 노드들이 함께 쓰는 볼륨 그룹.",
    "pathHint": "노드의 절대 경로입니다.",
    "shared": "공유 스토리지",
    "sharedHint": "모든 노드가 이 경로에서 같은 파일을 보면 고르세요(예: opLite 밖에서 마운트한 NFS 공유).",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일, Ceph 풀의 이미지와 공유 볼륨 그룹의 논리 볼륨은 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "kind": "종류",
    "kinds": {
      "dir": "디렉터리",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD",
      "lvm-shared": "공유 LVM"
    },
    "location": "위치",
    "kindHint": {
      "dir": "먼저 각 노드에 디렉터리를 만드세요.",
      "nfs": "각 노드가 내보내기를 데이터 디렉터리 아래에 마운트합니다. 마운트는 root로 도는 Linux 노드에서만 됩니다. 풀의 노드들이 함께 쓰는 공유 풀입니다.",
      "lvm-thin": "먼저 각 노드에 볼륨 그룹과 thin pool을 만드세요. 볼륨은 만든 노드에 두는 raw thin 볼륨이고, 스냅샷은 디스크만 담습니다. root로 도는 Linux 노드에서만 됩니다.",
      "zfs": "먼저 각 노드에 데이터셋을 만드세요. 볼륨은 만든 노드에 두는 raw zvol이고, 스냅샷은 디스크만 담습니다. 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다. root로 도는 Linux 노드에서만 됩니다.",
      "rbd": "먼저 Ceph 풀을 만들고 각 노드에 ceph.conf와 키링을 두세요. 볼륨은 풀의 모든 노드가 쓰는 raw RBD 이미지이고, 스냅샷은 디스크만 담습니다. QEMU rbd 드라이버(qemu-block-extra)와 rbd 명령(ceph-common)이 있는 Linux 노드에서만 됩니다.",
      "lvm-shared": "먼저 모든 노드가 보는 LUN에 공유 볼륨 그룹(vgcreate --shared)을 만들고, 각 노드에서 sanlock이나 dlm과 함께 lvmlockd를 띄우세요. 볼륨은 풀의 모든 노드가 쓰는 raw 논리 볼륨이고, 스냅샷은 없습니다. root로 도는 Linux 노드에서만 됩니다."
    },
    "server": "NFS 서버",
    "serverHint": "호스트 이름 또는 IP 주소입니다.",
    "export": "내보내기 경로",
    "exportHint": "서버가 내보내는 절대 경로입니다(예: /srv/vm).",
    "options": "마운트 옵션(선택)",
    "optionsHint": "nosuid,nodev,noexec,retry=0 뒤에 붙여 mount -o에 넘길 쉼표 목록입니다(예: vers=4.2,hard).",
    "state": {
      "ready": "준비됨",
      "notReady": "준비 안 됨",
      "unknown": "아직 알린 상태 없음",
      "offline": "오프라인"
    },
    "errors": {
      "path": "절대 경로(/로 시작)를 입력하세요.",
      "export": "제어 문자 없는 절대 경로(/로 시작)를 입력하세요.",
      "nodesGone": "고른 노드가 더 이상 클러스터에 없습니다. 다시 고르세요.",
      "server": "공백·쉼표·빗금·대괄호 없이 호스트 이름이나 IP 주소를 입력하세요.",
      "options": "옵션은 공백 없이 쉼표로 나누세요. bind, rbind, remount, move, suid, dev, exec, helper, bg와 x-* 옵션은 쓸 수 없습니다.",
      "lvmName": "-, snapshot, pvmove로 시작하지 않는 1–127자의 영문자, 숫자, +, _, ., -를 쓰세요. LVM이 쓰는 .과 .., _tdata·_tmeta 같은 이름이 든 것도 쓸 수 없습니다.",
      "zfsDataset": "tank/opl처럼 177자 이하의 데이터셋 이름을 쓰세요: /로 나눈 각 부분은 영문자, 숫자, _, ., :, -로 되고 비어 있거나 . 또는 ..이거나 -로 시작하면 안 됩니다. 맨 앞은 풀 이름이고 영문자로 시작합니다.",
      "rbdName": "-로 시작하지 않는 1–63자의 영문자, 숫자, _, ., -를 쓰세요.",
      "cephUser": "Ceph 사용자는 \"client.\" 없이 쓰세요(client.opl이 아니라 opl).",
      "rbdConf": "제어 문자·쉼표·콜론·등호 없는 절대 경로(/로 시작)를 입력하세요.",
      "portal": "공백·쉼표·빗금 없이 호스트 이름이나 IP 주소를 입력하고, 필요하면 :포트(1–65535)를 붙이세요. IPv6 주소는 [fd00::5]:3260처럼 대괄호로 감싸세요.",
      "target": "iqn.·eui.·naa.로 시작하고 소문자, 숫자, ., :, -로만 된 223자 이하의 타깃 이름을 쓰세요."
    },
    "vg": "볼륨 그룹",
    "thinpool": "Thin pool",
    "dataset": "데이터셋",
    "cephPool": "Ceph 풀",
    "namespace": "네임스페이스(선택)",
    "cephUser": "Ceph 사용자(선택)",
    "cephUserHint": "client. 없는 Ceph 클라이언트 ID입니다. 비우면 admin입니다.",
    "cephConf": "ceph.conf 경로(선택)",
    "cephConfHint": "노드의 절대 경로입니다. 비우면 /etc/ceph/ceph.conf입니다.",
    "iscsi": "iSCSI 타깃에 로그인",
    "iscsiHint": "각 노드가 볼륨 그룹을 쓰기 전에 타깃에 로그인합니다. 파이버 채널처럼 노드가 이미 LUN을 보면 고르지 마세요.",
    "portal": "iSCSI 포털",
    "portalHint": "호스트 이름이나 IP 주소에 포트를 붙일 수 있습니다(예: san.lan:3260, [fd00::5]:3260).",
    "target": "iSCSI 타깃",
    "targetHint": "타깃 이름입니다(예: iqn.2003-01.org.example:storage)."
  },
  "networks": {
    "bridge": "브리지",
    "vlan": "VLAN",
    "vlanOptional": "VLAN 태그(선택)",
    "vlanHint": "1–4094. 브리지가 VLAN을 알아야 합니다(vlan_filtering 1).",
    "nodes": "브리지가 있는 노드",
    "vms": "VM",
    "empty": "네트워크가 없습니다.",
    "userNote": "user 네트워크(QEMU NAT)는 늘 쓸 수 있으며 목록에 나오지 않습니다.",
    "noNode": "이 브리지가 있는 노드가 없습니다",
    "noVlanFiltering": "{{nodes}}에서 VLAN 필터링이 꺼져 있습니다",
    "createTitle": "네트워크 만들기",
    "createDescription": "노드에 이미 있는 Linux 브리지에 VM 인터페이스를 연결합니다. 이 네트워크의 VM은 그 브리지가 있는 노드에서만 실행됩니다.",
    "created": "네트워크 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "네트워크 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "네트워크만 지웁니다. 노드의 브리지는 바꾸지 않습니다.",
    "deleted": "네트워크 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 VM 인터페이스를 빼세요.",
    "errors": {
      "reserved": "user는 NAT에 예약된 이름입니다.",
      "bridge": "/, :, 공백 없이 1–15자로 쓰세요.",
      "vlan": "1부터 4094까지의 숫자를 쓰세요."
    }
  },
  "nodes": {
    "address": "API 주소",
    "hypervisor": "하이퍼바이저",
    "arch": "아키텍처",
    "cpus": "CPU",
    "memory": "메모리",
    "bridges": "브리지",
    "pools": "스토리지 풀",
    "vms": "VM",
    "thisNode": "(이 노드)",
    "joinNote": "노드를 더하려면 클러스터 화면에서 조인 토큰을 발급한 뒤 새 노드에서 oplctl cluster join을 실행하세요.",
    "maintenanceNote": "유지보수 중인 노드에는 새 VM을 놓지 않습니다. 이미 실행 중인 VM은 그대로 둡니다.",
    "startMaintenance": "유지보수 시작",
    "endMaintenance": "유지보수 끝내기",
    "maintenanceOn": "{{name}}을(를) 유지보수 모드로 바꿨습니다.",
    "maintenanceOff": "{{name}}의 유지보수를 끝냈습니다.",
    "remove": "클러스터에서 제거",
    "removeTitle": "{{name}}을(를) 클러스터에서 제거할까요?",
    "removeDescription": "완전히 멈춘 노드에만 쓰세요. 지금 데이터로는 다시 합류할 수 없고, 이 노드에 배정된 VM은 그대로 남습니다.",
    "removeNote": "오프라인 노드만 여기서 제거할 수 있습니다. 살아 있는 노드는 그 노드에서 oplctl cluster leave를 실행하세요.",
    "removed": "{{name}}을(를) 클러스터에서 제거했습니다.",
    "reason": {
      "thisNode": "지금 이 노드로 접속해 있습니다.",
      "online": "노드가 온라인입니다. 그 노드에서 oplctl cluster leave를 실행하세요."
    }
  },
  "accounts": {
    "currentPassword": "현재 비밀번호",
    "changePassword": "내 비밀번호 변경",
    "changePasswordDescription": "내 세션이 모두 끝납니다. 새 비밀번호로 다시 로그인하세요.",
    "passwordChanged": "비밀번호를 바꿨습니다. 새 비밀번호로 로그인하세요.",
    "newPassword": "새 비밀번호",
    "confirmPassword": "새 비밀번호 확인",
    "passwordHint": "8자 이상입니다.",
    "errors": {
      "password": "8자 이상으로 쓰세요.",
      "mismatch": "두 비밀번호가 다릅니다.",
      "current": "현재 비밀번호를 입력하세요.",
      "wrongCurrent": "현재 비밀번호가 틀렸습니다."
    }
  },
  "users": {
    "role": "역할",
    "you": "(나)",
    "empty": "사용자가 없습니다.",
    "roleHint": "운영자는 VM과 볼륨을 바꾸고, 조회자는 읽기만 합니다. 둘 다 계정은 볼 수 없습니다.",
    "createTitle": "사용자 만들기",
    "created": "사용자 {{name}}을(를) 만들었습니다.",
    "changeRole": "역할 바꾸기",
    "roleTitle": "{{name}}의 역할 바꾸기",
    "roleChanged": "{{name}}의 역할을 {{role}}(으)로 바꿨습니다.",
    "resetPassword": "비밀번호 재설정",
    "passwordTitle": "{{name}}의 비밀번호 재설정",
    "passwordDescription": "그 사용자의 세션이 끝나고, 새 비밀번호로 다시 로그인합니다.",
    "passwordReset": "{{name}}의 비밀번호를 재설정했습니다.",
    "deleteTitle": "사용자 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "사용자와 그 사용자의 API 토큰이 지워집니다.",
    "deleted": "사용자 {{name}}을(를) 삭제했습니다.",
    "adminOnly": "사용자는 관리자만 관리할 수 있습니다.",
    "selfNote": "내 비밀번호는 계정 메뉴에서 바꿉니다. 내 역할 변경과 삭제는 다른 관리자가 합니다.",
    "reason": {
      "selfRole": "내 역할은 다른 관리자가 바꿉니다.",
      "selfPassword": "내 비밀번호는 계정 메뉴에서 바꿉니다.",
      "selfDelete": "내 계정은 다른 관리자가 삭제합니다."
    }
  },
  "tokens": {
    "owner": "주인",
    "created": "만든 시각",
    "empty": "API 토큰이 없습니다.",
    "note": "스크립트와 도구는 API 토큰을 씁니다. 토큰의 권한은 주인의 역할을 따릅니다. oplctl login은 CLI용 토큰을 만듭니다.",
    "createTitle": "API 토큰 만들기",
    "createDescription": "토큰은 내 역할로 나 대신 요청합니다.",
    "secretTitle": "토큰 {{name}}을(를) 만들었습니다",
    "secretDescription": "지금 토큰을 복사하세요. 다시 볼 수 없습니다.",
    "secret": "토큰",
    "copySecret": "토큰 복사",
    "usage": "요청 헤더에 넣어 보냅니다",
    "deleteTitle": "API 토큰 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 토큰을 쓰는 곳은 곧바로 동작하지 않습니다.",
    "deleted": "API 토큰 {{name}}을(를) 삭제했습니다."
  },
  "cluster": {
    "title": "클러스터 {{name}}",
    "ca": "인증 기관(CA)",
    "caNote": "모든 노드의 HTTPS 인증서는 클러스터 CA가 서명합니다. 인증서 경고를 없애려면 CA를 브라우저에 가져오고, 지문이 같은지 확인하세요. 브라우저는 SHA-256 지문을 두 자리씩 콜론이나 공백으로 나눈 대문자 16진수로 보여 줍니다. sha256: 뒤의 16진수와 비교하세요(대소문자는 상관없습니다).",
    "copyFingerprint": "CA 지문 복사",
    "downloadCa": "CA 내려받기",
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
    "role": "역할",
    "leader": "리더",
    "voter": "투표 멤버",
    "learner": "학습자",
    "clusterAddress": "클러스터 주소",
    "issue": "조인 토큰 발급",
    "issueTitle": "조인 토큰 발급",
    "issueDescription": "새 노드가 이 토큰으로 한 번 합류합니다. 먼저 새 노드에서 opld를 시작하세요.",
    "ttl": "유효 기간",
    "ttlOption": {
      "3600": "1시간",
      "86400": "1일",
      "604800": "7일"
    },
    "issuedTitle": "새 노드 합류",
    "issuedDescription": "{{time}} 전에 새 노드에서 실행하세요. 다시 볼 수 없습니다.",
    "command": "명령",
    "copyCommand": "명령 복사",
    "commandNote": "새 노드의 opld가 /run/opl/opld.sock을 쓰지 않으면 --socket을 붙이세요."
  },
  "vmForm": {
    "createTitle": "가상 머신 만들기",
    "editTitle": "{{name}} 편집",
    "runningNote": "VM이 실행 중이라 노드와 디스크는 바꿀 수 없습니다. 다른 변경은 다음 시작부터 적용됩니다.",
    "general": "일반",
    "cores": "vCPU 코어",
    "memory": "메모리 (MiB)",
    "cpuModel": "CPU 모델",
    "nodeAuto": "자동",
    "diskN": "디스크 {{n}}",
    "newVolume": "새 볼륨",
    "pool": "스토리지 풀",
    "sizeGib": "크기 (GiB)",
    "removeDisk": "디스크 {{n}} 빼기",
    "addExisting": "볼륨 붙이기",
    "addNew": "새 빈 볼륨",
    "nicN": "네트워크 인터페이스 {{n}}",
    "userNetwork": "user (NAT)",
    "macAuto": "자동",
    "removeNic": "인터페이스 {{n}} 빼기",
    "addNic": "인터페이스 추가",
    "startAfter": "만든 뒤 시작",
    "reload": "다시 불러오기",
    "errors": {
      "name": "소문자·숫자·-로 1~63자, -로 시작하거나 끝날 수 없습니다.",
      "cores": "1에서 65535 사이여야 합니다.",
      "memory": "64 MiB 이상이어야 합니다.",
      "volume": "볼륨을 고르거나 이름을 정하세요.",
      "pool": "스토리지 풀을 고르세요.",
      "size": "크기를 입력하세요.",
      "bootOrder": "부팅 순서는 1부터 255까지 겹치지 않는 숫자여야 합니다.",
      "mac": "52:54:00:12:34:56 형식으로 입력하세요.",
      "volumeChoose": "볼륨을 고르세요.",
      "volumeDuplicate": "디스크마다 다른 볼륨이어야 합니다.",
      "volumeExists": "같은 이름의 볼륨이 이미 있습니다.",
      "macMulticast": "멀티캐스트 주소(첫 바이트가 홀수)는 쓸 수 없습니다.",
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다.",
      "keep": "1에서 100 사이여야 합니다."
    },
    "volumesCreated": "볼륨 {{names}}은(는) 만들어졌습니다. {{error}}",
    "schedule": "예약 스냅샷",
    "scheduleNote": "주기마다 스냅샷을 만들고 예약으로 만든 것은 최근 것만 남깁니다. 직접 만든 스냅샷은 지우지 않습니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있어야 합니다.",
    "scheduleEvery": "스냅샷 주기",
    "scheduleOff": "끔",
    "scheduleKeep": "보관 개수",
    "scheduleRaw": "{{volume}}이(가) {{format}}이라 예약 스냅샷이 실패합니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있어야 합니다.",
    "scheduleMemoryHint": "VM이 실행 중이고 디스크가 있을 때만 메모리도 저장합니다.",
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀, Ceph RBD 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다.",
    "scheduleSharedLvm": "{{volume}}이(가) 공유 LVM 풀에 있어 예약 스냅샷이 실패합니다."
  },
  "console": {
    "connecting": "연결 중…",
    "connected": "연결됨",
    "failed": "연결이 끊겼습니다. VM이 정지했거나 콘솔을 열 수 없습니다.",
    "reconnect": "다시 연결",
    "paste": "클립보드 입력",
    "pasteDenied": "브라우저가 클립보드 읽기를 허락하지 않았습니다.",
    "fullscreen": "전체 화면",
    "pasteTruncated": "앞의 {{count}}자만 입력했습니다.",
    "screen": "VM 화면",
    "hint": "화면을 누르면 VM에 입력합니다. 화면 밖을 누르면 키보드를 돌려받습니다.",
    "forbidden": "콘솔을 쓸 권한이 없습니다."
  },
  "dashboard": {
    "nodes": "노드",
    "online": "온라인",
    "vms": "가상 머신",
    "vmCounts": "실행 {{running}} · 정지 {{stopped}} · 실패 {{failed}}",
    "leader": "리더",
    "perNode": "노드",
    "hypervisor": "하이퍼바이저",
    "activeVms": "활성 VM",
    "nodeOnline": "온라인",
    "nodeOffline": "오프라인",
    "maintenance": "유지보수",
    "failedVms": "실패한 가상 머신",
    "recentTasks": "최근 작업",
    "allTasks": "모든 작업",
    "noLeader": "리더 없음",
    "noNodes": "노드가 없습니다."
  }
}
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  });
  mockLists({ node: [NODE, down], vm: [vm("web", "running"), vm("app", "running"), broken] });
  server.use(
```

을 다음으로 바꾼다:

```tsx
  });
  // 옮기는 중인 VM도 실행 중으로 센다.
  const app = vm("app", "running");
  const migrating = { ...app, status: { ...app.status, phase: "migrating" as const } };
  mockLists({ node: [NODE, down], vm: [vm("web", "running"), migrating, broken] });
  server.use(
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  });
  expect(usage(NODE, [vm("web", "running"), failed, stopping, elsewhere])).toEqual({
    vms: 2,
    vcpus: 4,
    memoryMib: 4096,
  });
```

을 다음으로 바꾼다:

```tsx
  });
  // 옮기는 중인 VM은 확정할 때까지 원본 노드(status.node)에서 돈다.
  const migrating = vm("mig", "migrating", {
    status: {
      phase: "migrating",
      node: "n1",
      pid: 9,
      last_error: null,
      snapshot: null,
      schedule_error: null,
      migration: { node: "n2", task: "t1" },
    },
  });
  expect(usage(NODE, [vm("web", "running"), failed, stopping, elsewhere, migrating])).toEqual({
    vms: 3,
    vcpus: 6,
    memoryMib: 6144,
  });
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
/** QEMU가 떠 있는 단계. 실패한 VM도 `status.node`가 남으므로 단계로 거른다. */
const ACTIVE: Vm["status"]["phase"][] = ["starting", "running", "stopping"];

/** 노드에서 시작 중·실행 중·정지 중인 VM이 쓰는 vCPU와 메모리 */
export function usage(node: Node, vms: Vm[]) {
```

을 다음으로 바꾼다:

```tsx
/** QEMU가 떠 있는 단계. 실패한 VM도 `status.node`가 남으므로 단계로 거른다. */
const ACTIVE: Vm["status"]["phase"][] = ["starting", "running", "stopping", "migrating"];

/** 노드에서 시작 중·실행 중·정지 중·옮기는 중(원본 노드)인 VM이 쓰는 vCPU와 메모리 */
export function usage(node: Node, vms: Vm[]) {
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
            t("dashboard.vmCounts", {
              running: count("running"),
              stopped: count("stopped"),
```

을 다음으로 바꾼다:

```tsx
            t("dashboard.vmCounts", {
              // 옮기는 중인 VM은 원본에서 계속 돈다.
              running: count("running") + count("migrating"),
              stopped: count("stopped"),
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx
  openDetail(starting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText("The VM is starting or stopping. Try again when it has settled."),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});
```

을 다음으로 바꾼다:

```tsx
  openDetail(starting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText(
      "The VM is starting, stopping or migrating. Try again when it has settled.",
    ),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx
  openDetail(halting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText("The VM is starting or stopping. Try again when it has settled."),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});
```

을 다음으로 바꾼다:

```tsx
  openDetail(halting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText(
      "The VM is starting, stopping or migrating. Try again when it has settled.",
    ),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert, clone or migration) is in progress. Try again when it has finished.",
  );
```

`web/src/vms/VmConsole.test.tsx`에서

```tsx

it("does not connect to a stopped VM", () => {
```

을 다음으로 바꾼다:

```tsx

it("stays connected while the VM is being migrated", () => {
  const web = vm("web", "running");
  const { rerender } = render(<VmConsole vm={web} />);
  // 원본 QEMU가 계속 돈다. 상태가 바뀌어도 다시 연결하지 않는다.
  rerender(<VmConsole vm={{ ...web, status: { ...web.status, phase: "migrating" } }} />);
  expect(created).toHaveLength(1);
  expect(created[0]!.disconnected).toBe(false);
  render(<VmConsole vm={vm("app", "migrating")} />);
  expect(created).toHaveLength(2);
});

it("does not connect to a stopped VM", () => {
```

`web/src/vms/VmConsole.tsx`에서

```tsx
  const [attempt, setAttempt] = useState(0);
  const running = vm.status.phase === "running";
```

을 다음으로 바꾼다:

```tsx
  const [attempt, setAttempt] = useState(0);
  // 옮기는 중에도 원본 QEMU가 돈다. 확정하면 원본이 꺼져 끊기고, 다시 연결하면 대상에 붙는다.
  const running = vm.status.phase === "running" || vm.status.phase === "migrating";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { newestFirst, TASK_PAGE_SIZE, TaskTable } from "@/tasks/TasksPage";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
import { VmConsole } from "@/vms/lazyConsole";
```

을 다음으로 바꾼다:

```tsx
import { newestFirst, TASK_PAGE_SIZE, TaskTable } from "@/tasks/TasksPage";
import {
  ACTIONS,
  availability,
  useVmActions,
  type VmAction,
  type VmOperation,
} from "@/vms/actions";
import { VmConsole } from "@/vms/lazyConsole";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  // 고정한 노드가 있으면 그 노드, 아니면 스케줄러가 놓은 노드
  const nodeId = vm.spec.node ?? vm.status.node;
  const node = nodes.data?.find((n) => n.id === nodeId);
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmAction | "console" | "delete") => {
    const able = availability(vm, action, vmTasks);
```

을 다음으로 바꾼다:

```tsx
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  const migration = vm.status.migration;
  // 옮기는 중이면 QEMU가 도는 원본(확정하며 spec.node가 먼저 대상이 될 수 있다). 아니면 고정한 노드가
  // 있으면 그 노드, 없으면 스케줄러가 놓은 노드
  const nodeId = (migration ? vm.status.node : null) ?? vm.spec.node ?? vm.status.node;
  const node = nodes.data?.find((n) => n.id === nodeId);
  // 옮기는 중이면 옮겨 갈 노드(지금 노드면 보이지 않는다)
  const target =
    migration && migration.node !== nodeId
      ? nodes.data?.find((n) => n.id === migration.node)
      : undefined;
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmOperation) => {
    const able = availability(vm, action, vmTasks);
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const reasons = [
    ...(operate ? [...ACTIONS.map(reasonOf), consoleReason] : []),
    write ? reasonOf("delete") : null,
```

을 다음으로 바꾼다:

```tsx
  const reasons = [
    ...(operate ? [...ACTIONS.map(reasonOf), consoleReason, reasonOf("migrate")] : []),
    write ? reasonOf("delete") : null,
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
        <VmPhaseBadge vm={vm} />
        {node && <span className="text-sm text-muted-foreground">{node.name}</span>}
      </div>
```

을 다음으로 바꾼다:

```tsx
        <VmPhaseBadge vm={vm} />
        {node && (
          <span className="text-sm text-muted-foreground">
            {target ? t("vms.migratingTo", { from: node.name, to: target.name }) : node.name}
          </span>
        )}
      </div>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
            </Link>
          </ReasonButton>
```

을 다음으로 바꾼다:

```tsx
            </Link>
          </ReasonButton>
        )}
        {operate && (
          <ReasonButton
            variant="outline"
            size="sm"
            reason={reasonOf("migrate")}
            reasons={hint}
            onClick={() => setConfirm("migrate")}
          >
            {t("vms.action.migrate")}
          </ReasonButton>
```

`web/src/vms/VmMenu.tsx`에서

```tsx
} from "@/components/ui/dropdown-menu";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
```

을 다음으로 바꾼다:

```tsx
} from "@/components/ui/dropdown-menu";
import {
  ACTIONS,
  availability,
  useVmActions,
  type VmAction,
  type VmOperation,
} from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
import { VmMigrateDialog } from "@/vms/VmMigrateDialog";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmAction | "console" | "delete") => {
    const able = availability(vm, action, vmTasks);
```

을 다음으로 바꾼다:

```tsx
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmOperation) => {
    const able = availability(vm, action, vmTasks);
```

`web/src/vms/VmMenu.tsx`에서

```tsx
/** 메뉴·상세 버튼이 여는 대화상자 */
export type VmDialog = "force-stop" | "delete" | "clone";

/** 강제 정지·삭제의 확인과 복제 대화상자 */
export function VmConfirm({
```

을 다음으로 바꾼다:

```tsx
/** 메뉴·상세 버튼이 여는 대화상자 */
export type VmDialog = "force-stop" | "delete" | "clone" | "migrate";

/** 강제 정지·삭제의 확인과 복제·마이그레이션 대화상자 */
export function VmConfirm({
```

`web/src/vms/VmMenu.tsx`에서

```tsx
      {which === "clone" && <VmCloneDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />}
    </>
```

을 다음으로 바꾼다:

```tsx
      {which === "clone" && <VmCloneDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />}
      {which === "migrate" && (
        <VmMigrateDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />
      )}
    </>
```

`web/src/vms/VmMigrateDialog.test.tsx`를 만든다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node, Principal, Vm } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { NODE, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { migrationTargets } from "@/vms/VmMigrateDialog";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const OPERATOR: Principal = {
  user: "00000000-0000-0000-0000-000000000002",
  name: "ops",
  role: "operator",
};
const VIEWER: Principal = { ...OPERATOR, name: "viewer", role: "viewer" };

function node(id: string, name: string, extra: Partial<Node["status"]> = {}, maintenance = false) {
  return {
    ...NODE,
    id,
    name,
    spec: { ...NODE.spec, maintenance },
    status: { ...NODE.status, arch: "aarch64", ...extra },
  } satisfies Node;
}

// 기본 VM은 `virt`(aarch64) 머신으로 n1에서 돈다.
const HERE = node("n1", "node-a");
const NODES = [
  HERE,
  node("n2", "node-b"),
  node("n3", "node-c", { online: false }),
  node("n4", "node-d", {}, true),
  node("n5", "node-e", { arch: "x86_64" }),
  node("n6", "node-f", { arch: null }),
  node("n7", "node-g"),
];

/** 서버처럼 `migration`을 빼고 보낸 VM */
function served(v: Vm): Vm {
  const status: Partial<Vm["status"]> = { ...v.status };
  delete status.migration;
  return { ...v, status } as Vm;
}

const web = served(vm("web", "running"));

function openDetail(as = OPERATOR) {
  return renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web", as });
}

it("offers online nodes of the same architecture other than the current one", () => {
  expect(migrationTargets(web, NODES).map((n) => n.name)).toEqual(["node-b", "node-g"]);
  // x86_64 머신(q35)은 x86_64 노드로만
  const q35 = { ...web, spec: { ...web.spec, machine: "q35" as const } };
  expect(migrationTargets(q35, NODES).map((n) => n.name)).toEqual(["node-e"]);
});

it("migrates a running VM to the chosen node", async () => {
  mockLists({ vm: [web], node: NODES });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${web.id}/migrate`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-migrate", web.id), { status: 202 });
    }),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  const target = within(dialog).getByLabelText("Target node");
  await waitFor(() =>
    expect(
      within(target)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["node-b", "node-g"]),
  );
  await userEvent.selectOptions(target, "n7");
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(await screen.findByText("Migrate web to node-g")).toBeInTheDocument();
  expect(bodies).toEqual([{ node: "n7" }]);
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("shows the server's refusal in the dialog", async () => {
  mockLists({ vm: [web], node: NODES });
  server.use(
    http.post(apiUrl(`/vms/${web.id}/migrate`), () =>
      HttpResponse.json(
        {
          code: "migration_unsupported",
          message: 'disk "web-root" is not on a shared storage pool',
        },
        { status: 409 },
      ),
    ),
  );
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await waitFor(() => expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Migrate" }));
  expect(
    await within(dialog).findByText(
      'This VM cannot be migrated: disk "web-root" is not on a shared storage pool',
    ),
  ).toBeInTheDocument();
});

it("says when no node can take the VM", async () => {
  mockLists({ vm: [web], node: [HERE, node("n3", "node-c", { online: false })] });
  openDetail();
  await userEvent.click(await screen.findByRole("button", { name: "Migrate" }));
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(
    await within(dialog).findByText(
      "No other online node with the VM's architecture is available.",
    ),
  ).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Migrate" })).toBeDisabled();
});

it("blocks other actions and shows the target while migrating", async () => {
  const migrating = {
    ...web,
    status: { ...web.status, phase: "migrating" as const, migration: { node: "n2", task: "t1" } },
  };
  mockLists({ vm: [migrating], node: NODES });
  openDetail();
  expect(await screen.findByText("Migrating")).toBeInTheDocument();
  expect(await screen.findByText("node-a → node-b")).toBeInTheDocument();
  for (const name of ["Migrate", "Stop", "Force stop", "Reboot"]) {
    const button = screen.getByRole("button", { name });
    expect(button).toHaveAttribute("aria-disabled", "true");
    expect(button).toHaveAccessibleDescription("Not available while the VM is being migrated.");
  }
  // 콘솔은 열 수 있다(원본 QEMU가 계속 돈다).
  expect(screen.getByRole("link", { name: "Open console" })).not.toHaveAttribute("aria-disabled");
});

it("shows the source from status.node while migrating", async () => {
  // 확정 쓰기 사이: spec.node는 벌써 대상이지만 QEMU는 아직 원본(n1)에서 돈다.
  const committing = {
    ...web,
    spec: { ...web.spec, node: "n2" },
    status: { ...web.status, phase: "migrating" as const, migration: { node: "n2", task: "t1" } },
  };
  mockLists({ vm: [committing], node: NODES });
  const { unmount } = openDetail();
  expect(await screen.findByText("node-a → node-b")).toBeInTheDocument();
  unmount();

  // 대상이 지금 노드면 화살표를 보이지 않는다.
  const moved = {
    ...committing,
    status: { ...committing.status, phase: "running" as const, node: "n2" },
  };
  mockLists({ vm: [moved], node: NODES });
  openDetail();
  expect(await screen.findByText("node-b")).toBeInTheDocument();
  expect(screen.queryByText(/→/)).not.toBeInTheDocument();
});

it("does not offer migration to a stopped VM or a viewer", async () => {
  const stopped = served(vm("web", "stopped"));
  mockLists({ vm: [stopped], node: NODES });
  const { unmount } = openDetail();
  const button = await screen.findByRole("button", { name: "Migrate" });
  expect(button).toHaveAttribute("aria-disabled", "true");
  unmount();

  mockLists({ vm: [web], node: NODES });
  openDetail(VIEWER);
  expect(await screen.findByRole("heading", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Migrate" })).not.toBeInTheDocument();
});
```

`web/src/vms/VmMigrateDialog.tsx`를 만든다:

```tsx
import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { trackTask } from "@/tasks/tasks";
import { availability } from "@/vms/actions";

type Schemas = components["schemas"];

/** 머신 타입의 아키텍처(서버의 `MachineType::arch`) */
const ARCH_OF: Record<Schemas["MachineType"], Schemas["Arch"]> = {
  q35: "x86_64",
  virt: "aarch64",
};

/**
 * 옮겨 갈 수 있어 보이는 노드: 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인, 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
 */
export function migrationTargets(vm: Vm, nodes: readonly Node[]): Node[] {
  const here = vm.spec.node ?? vm.status.node;
  const arch = ARCH_OF[vm.spec.machine];
  return nodes.filter(
    (n) => n.id !== here && n.status.online && !n.spec.maintenance && n.status.arch === arch,
  );
}

/** 실행 중인 VM의 라이브 마이그레이션 대화상자. 열 때만 그린다. */
export function VmMigrateDialog({
  vm,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const targets = migrationTargets(vm, nodes.data ?? []);
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았거나 고른 노드가 후보에서 빠졌으면 첫 후보
  const target = targets.find((n) => n.id === chosen) ?? targets[0];
  // 대화상자를 연 사이 VM이 멈추거나 옮기기 시작했을 수 있다.
  const able = availability(vm, "migrate", vmTasks);

  async function submit() {
    if (!target) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/migrate", {
        params: { path: { key: vm.id } },
        body: { node: target.id },
      }),
    );
    trackTask(task, t("migrate.op", { vm: vm.name, node: target.name }));
  }

  return (
    <FormDialog
      title={t("migrate.title", { name: vm.name })}
      description={t("migrate.description")}
      submitLabel={t("migrate.submit")}
      submitDisabled={!able.ok || !target}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!able.ok && <p className="text-sm text-muted-foreground">{t(able.reason)}</p>}
      {!nodes.data ? (
        <ListPending error={nodes.error} />
      ) : (
        targets.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("migrate.noTarget")}</p>
        )
      )}
      <Field id="migrate-node" label={t("migrate.node")}>
        {(a11y) => (
          <NativeSelect
            id="migrate-node"
            className="w-full"
            value={target?.id ?? ""}
            onChange={(e) => setChosen(e.target.value)}
            {...a11y}
          >
            {targets.map((n) => (
              <NativeSelectOption key={n.id} value={n.id}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  open(starting);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
```

을 다음으로 바꾼다:

```tsx
  open(starting);
  expect(
    await screen.findByText(
      "The VM is starting, stopping or migrating. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  open(pending);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
```

을 다음으로 바꾼다:

```tsx
  open(pending);
  expect(
    await screen.findByText(
      "The VM is starting, stopping or migrating. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert, clone or migration) is in progress. Try again when it has finished.",
  );
```

`web/src/vms/actions.test.ts`에서

```ts
import type { Task } from "@/api/types";
import { task, vm } from "@/test/fixtures";
import { ACTIONS, availability, isBusy, isReverting, isStopped } from "@/vms/actions";
```

을 다음으로 바꾼다:

```ts
import type { Task, Vm } from "@/api/types";
import { task, vm } from "@/test/fixtures";
import { ACTIONS, availability, isBusy, isMigrating, isReverting, isStopped } from "@/vms/actions";
```

`web/src/vms/actions.test.ts`에서

```ts
  expect(availability(starting, "delete", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});
```

을 다음으로 바꾼다:

```ts
  expect(availability(starting, "delete", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});

/** 서버처럼 `migration`을 빼고 보낸 VM(없으면 undefined) */
function withoutMigration(v: ReturnType<typeof vm>): ReturnType<typeof vm> {
  const status: Partial<Vm["status"]> = { ...v.status };
  delete status.migration;
  return { ...v, status } as Vm;
}

const MIGRATION = { node: "n2", task: "t-migrate" };

it("migrates only a running VM that should run", () => {
  const running = withoutMigration(vm("b", "running"));
  expect(isMigrating(running, [])).toBe(false);
  expect(availability(running, "migrate", [])).toEqual({ ok: true });
  expect(availability(vm("a", "stopped"), "migrate", [])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
  expect(availability(vm("d", "starting"), "migrate", [])).toEqual({
    ok: false,
    reason: "vms.reason.busy",
  });
  // 정지를 기다리는 VM
  const stopping = vm("e", "running", { spec: { ...running.spec, power: "stopped" } });
  expect(availability(stopping, "migrate", []).ok).toBe(false);
  // 전원 Task가 끝나지 않았다.
  expect(availability(running, "migrate", [pendingOf(running, "vm-reboot")])).toEqual({
    ok: false,
    reason: "vms.reason.powerPending",
  });
  // 서버의 check_no_snapshot_task: 스냅샷 만들기·되돌리기·복제 Task가 끝나지 않았다.
  for (const kind of ["snapshot-create", "snapshot-revert", "vm-clone"] as const) {
    expect(availability(running, "migrate", [pendingOf(running, kind)])).toEqual({
      ok: false,
      reason: "vms.reason.snapshotPending",
    });
  }
  // 스냅샷 지우기는 막지 않는다.
  expect(availability(running, "migrate", [pendingOf(running, "snapshot-delete")])).toEqual({
    ok: true,
  });
});

it("follows the server's check_no_migration: every action waits for a migration", () => {
  const running = vm("b", "running");
  const migrating = vm("m", "migrating", {
    spec: { ...running.spec, power: "running" },
    status: { ...running.status, phase: "migrating", migration: MIGRATION },
  });
  // 상태를 아직 쓰지 않았지만 마이그레이션 Task가 끝나지 않았다.
  const queued = [pendingOf(running, "vm-migrate")];
  const reason = { ok: false, reason: "vms.reason.migrating" };
  expect(isMigrating(migrating, [])).toBe(true);
  expect(isMigrating(running, queued)).toBe(true);
  expect(isBusy(migrating)).toBe(true);
  for (const action of [...ACTIONS, "delete", "migrate"] as const) {
    expect(availability(migrating, action, [])).toEqual(reason);
    expect(availability(running, action, queued)).toEqual(reason);
  }
  // 콘솔은 서버가 마이그레이션 중에도 받는다(확정하면 끊기고 다시 연결한다).
  expect(availability(migrating, "console", [])).toEqual({ ok: true });
  expect(availability(running, "console", queued)).toEqual({ ok: true });
  // 끝난 마이그레이션 Task는 막지 않는다.
  const done = { ...queued[0]!, status: { state: "failed", error: "boom" } } as Task;
  expect(availability(running, "migrate", [done])).toEqual({ ok: true });
});
```

`web/src/vms/actions.ts`에서

```ts
/**
 * 서버의 `check_not_busy`: 시작·정지 중이거나 원하는 전원 상태에 아직 닿지 않았다. 이때 서버는 스냅샷·복제
 * 요청을 받지 않는다(vm_busy).
 */
export function isBusy(vm: Vm): boolean {
  const phase = vm.status.phase;
  if (phase === "starting" || phase === "stopping") {
    return true;
```

을 다음으로 바꾼다:

```ts
/**
 * 서버의 `check_not_busy`: 시작·정지·마이그레이션 중이거나 원하는 전원 상태에 아직 닿지 않았다. 이때 서버는
 * 스냅샷·복제 요청을 받지 않는다(vm_busy).
 */
export function isBusy(vm: Vm): boolean {
  const phase = vm.status.phase;
  if (phase === "starting" || phase === "stopping" || phase === "migrating") {
    return true;
```

`web/src/vms/actions.ts`에서

```ts

/** 할 수 있는지, 없으면 그 이유(번역 키) */
```

을 다음으로 바꾼다:

```ts

/**
 * 서버의 `check_no_migration`: 옮기는 중이거나(`migrating`) 이 VM의 마이그레이션 Task가 끝나지 않았다. 이때
 * 서버는 다른 동작을 모두 받지 않는다(vm_busy).
 */
export function isMigrating(vm: Vm, vmTasks: readonly Task[]): boolean {
  return (
    vm.status.phase === "migrating" ||
    vmTasks.some(
      (task) =>
        task.spec.op.kind === "vm-migrate" &&
        task.status.state !== "succeeded" &&
        task.status.state !== "failed",
    )
  );
}

/** 할 수 있는지, 없으면 그 이유(번역 키) */
```

`web/src/vms/actions.ts`에서

```ts

/** 상태만 보고 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
function allowedByState(vm: Vm, action: VmAction | "console" | "delete"): boolean {
  const phase = vm.status.phase;
```

을 다음으로 바꾼다:

```ts

/** 끝나지 않으면 마이그레이션을 막는 스냅샷 Task(서버의 `check_no_snapshot_task`) */
const SNAPSHOT_KINDS: readonly string[] = ["snapshot-create", "snapshot-revert", "vm-clone"];

/** 막힌 이유를 따지는 동작: 전원 동작, 콘솔, 삭제, 마이그레이션 */
export type VmOperation = VmAction | "console" | "delete" | "migrate";

/** 상태만 보고 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
function allowedByState(vm: Vm, action: VmOperation): boolean {
  const phase = vm.status.phase;
```

`web/src/vms/actions.ts`에서

```ts
    case "console":
      return phase === "running";
    case "delete":
      // 서버는 멈추지 않은 VM을 지우지 않는다.
      return isStopped(vm);
  }
```

을 다음으로 바꾼다:

```ts
    case "console":
      // 옮기는 중에도 원본 QEMU가 돈다.
      return phase === "running" || phase === "migrating";
    case "delete":
      // 서버는 멈추지 않은 VM을 지우지 않는다.
      return isStopped(vm);
    case "migrate":
      // 서버는 노드에 놓여 켜져 실행 중인 VM만 옮긴다(vm_not_running).
      return powerOn && phase === "running" && vm.spec.node != null;
  }
```

`web/src/vms/actions.ts`에서

```ts
 * 지금 할 수 있는 동작인지와 막힌 이유. `vmTasks`는 이 VM을 대상으로 한 Task다(`useTaskIndex`). 전원 Task가
 * 끝나지 않았으면 시작·정지·재시작을 막는다(서버는 새 요청으로 앞 것을 대체하지만 웹에서는 겹치지 않게).
 * 강제 정지는 정지가 ACPI에서 멈췄을 때 빠져나갈 길이므로 되돌리기 중에만 막는다.
 */
export function availability(
  vm: Vm,
  action: VmAction | "console" | "delete",
  vmTasks: readonly Task[],
): Availability {
  const power = action === "start" || action === "stop" || action === "reboot";
  if (
```

을 다음으로 바꾼다:

```ts
 * 지금 할 수 있는 동작인지와 막힌 이유. `vmTasks`는 이 VM을 대상으로 한 Task다(`useTaskIndex`). 전원 Task가
 * 끝나지 않았으면 시작·정지·재시작·마이그레이션을 막는다(서버는 새 요청으로 앞 것을 대체하지만 웹에서는 겹치지 않게).
 * 강제 정지는 정지가 ACPI에서 멈췄을 때 빠져나갈 길이므로 되돌리기 중에만 막는다. 마이그레이션 중에는 콘솔
 * 말고 모두 막는다(실패하면 원본이 계속 돈다). 마이그레이션은 스냅샷·복제 Task가 끝나지 않았어도 막는다.
 */
export function availability(vm: Vm, action: VmOperation, vmTasks: readonly Task[]): Availability {
  if (action !== "console" && isMigrating(vm, vmTasks)) {
    return { ok: false, reason: "vms.reason.migrating" };
  }
  const power =
    action === "start" || action === "stop" || action === "reboot" || action === "migrate";
  if (
```

`web/src/vms/actions.ts`에서

```ts
    return { ok: false, reason: "vms.reason.reverting" };
  }
```

을 다음으로 바꾼다:

```ts
    return { ok: false, reason: "vms.reason.reverting" };
  }
  if (
    action === "migrate" &&
    vmTasks.some(
      (task) =>
        SNAPSHOT_KINDS.includes(task.spec.op.kind) &&
        task.status.state !== "succeeded" &&
        task.status.state !== "failed",
    )
  ) {
    return { ok: false, reason: "vms.reason.snapshotPending" };
  }
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 307개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): VM 라이브 마이그레이션 동작과 대상 노드 대화상자"
```

---

### Task 9: 문서

README에 "라이브 마이그레이션"(요구 사항: Linux/KVM, 공유 풀, 같은 QEMU·CPU, 모든 노드 업그레이드; `[migration]`과 방화벽, TLS 파일, 거절 코드, 실패와 정리, UEFI 변수, 공유 LVM, 예전 노드 인증서), 개발 절의 `migration-it`, 설계 문서 "구현:" 메모와 한계, 처음 분해 문서(하위 프로젝트 4, UEFI 변수, `PoolSpec.shared`).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-10-30-live-migration-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션)
```

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## 라이브 마이그레이션

실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(`qm migrate`·vMotion). 원본 노드의 작업(Task)이 지휘하고, 메모리는
두 QEMU가 노드 인증서로 TLS를 맺어 직접 주고받는다. 대상 QEMU는 멈춘 채 받고, 받기를 확인한 뒤 클러스터 기록의
VM 노드를 대상으로 바꾸는 순간 확정된다. 그 뒤에만 대상에서 VM이 이어서 돈다(두 QEMU가 함께 돌지 않는다).

```bash
oplctl vm migrate web node2              # 끝날 때까지 기다린다: "vm web: migrated to node2"
oplctl vm list                           # 옮기는 동안 PHASE migrating, NODE "원본 -> 대상"
```

웹은 VM 상세의 "마이그레이션"(실행 중일 때)에서 대상 노드를 고른다. API는 `POST /api/v1/vms/{key}/migrate`
(`{"node": "이름 또는 id"}`, 운영자 권한, 202와 작업)다.

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[migration]
ports = "60000-60099"     # 받는 쪽 QEMU가 듣는 TCP 포트 범위(둘 다 포함)
bandwidth_mib = 0         # 보내는 대역폭 상한(MiB/s), 0이면 QEMU 기본값
timeout = "30m"           # 이 시간 안에 끝나지 않으면 취소하고 원본을 계속 돌린다("1h30m", "90s")
```

- **요구 사항.** Linux/KVM 노드끼리만 된다(macOS(HVF) 노드는 원본도 대상도 안 된다). 디스크가 **모두 공유 풀**
  (NFS, `--shared` 디렉터리 풀, Ceph RBD, 공유 LVM)에 있어야 한다(디스크 복사는 하지 않는다). 두 노드의 **QEMU
  버전이 같고 CPU가 같아야** 한다(`-cpu host`. 버전이 다르면 작업이 실패하고, CPU는 확인하지 않으므로 다르면
  실패하거나 게스트가 멈출 수 있다). 모든 노드를 이 기능이 있는 opld로 올린 뒤 쓴다.
- **방화벽.** 노드 사이에 `[migration] ports` 범위를 연다. 받는 쪽은 `cluster_listen`의 IP(기본 모든 주소)에서
  범위의 빈 포트를 고르고(모두 쓰는 중이면 "no free migration port in …"으로 실패한다), 원본은 대상의 `address`로
  접속한다.
- **TLS.** opld가 시작할 때 노드 인증서·키와 클러스터 CA를 `{data_dir}/migration-tls/`(기본
  `/var/lib/opl/migration-tls`, 0700, 키 0600)에 쓰고 QEMU가 그것으로 서로를 확인한다. 이 기능 전에 클러스터에
  들어온 노드의 인증서는 키 용도에 keyEncipherment가 없어 QEMU 8.2 이하에서 TLS가 실패한다(opld가 시작할 때
  경고한다). 그 노드를 클러스터에서 뺐다가 다시 합류시켜 새 인증서를 받는다.
- **거절(409).** 실행 중이 아님 `vm_not_running`, 공유 풀이 아닌 디스크나 macOS 노드 `migration_unsupported`,
  대상이 오프라인·유지보수 중 `node_unavailable`, 대상에서 풀·브리지가 준비되지 않았거나 남은 메모리가 모자람
  `no_capacity`, 다른 작업(스냅샷·되돌리기·크기 늘리기 등) 중 `vm_busy`. 같은 노드면 **400**, 아키텍처가 다르면
  **400** `machine_arch_mismatch`.
- **옮기는 동안.** VM은 `migrating`이고 켜기·끄기·재시작·스냅샷·복제·편집·지우기가 모두 **409** `vm_busy`다.
  콘솔은 열 수 있다(넘어가는 순간 끊겼다가 대상 노드로 다시 연결된다). 취소는 없다. 수렴하지 않으면 `auto-converge`가 게스트를 늦추고 시간 제한이 지나면 실패한다.
- **실패.** 확정 전에 실패하면(대상 거절, 연결·TLS 실패, 시간 제한) 대상 QEMU를 끄고 원본이 계속 돈다. 작업
  오류에 QEMU의 이유가 남는다. 중간에 opld가 죽어도 다시 뜰 때 원본을 되살리거나(확정 전) 남은 원본·받는 QEMU를
  끈다(확정 뒤, 시작 때와 30분마다).
- **UEFI 변수.** QEMU가 변수(pflash)도 함께 옮겨 대상 노드의 변수 파일에 쓴다.
- **공유 LVM.** 옮기는 동안 디스크 LV를 공유 잠금(`lvchange -asy`)으로 바꾸고, 대상이 넘겨받은 뒤 다시 배타로
  바꾼다.

## 웹 UI
````

`README.md`에서

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
```

을 다음으로 바꾼다:

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
```

`README.md`에서

````markdown
  OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared
```
````

을 다음으로 바꾼다:

````markdown
  OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared
# 실제 라이브 마이그레이션: 한 호스트의 opld 둘, 공유 디렉터리 풀(macOS는 TCG로 돈다). nfs-it·rbd-it·lvm-shared-it와
# 그 환경 변수를 함께 주면(Linux, root) 그 풀로도 옮긴다.
cargo test -p opld --features migration-it --test migration
```
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
하위 프로젝트마다 별도로 설계 → 계획 → 구현 사이클을 거친다. 위 순서가 구현 권장 순서다.
```

을 다음으로 바꾼다:

```markdown
하위 프로젝트마다 별도로 설계 → 계획 → 구현 사이클을 거친다. 위 순서가 구현 권장 순서다.

구현: 하위 프로젝트 4는 계획 20(공유 풀 위의 라이브 마이그레이션, QEMU TLS 마이그레이션·노드 사이 RPC·`VmMigrate` Task)과 계획 21(UEFI 변수가 멈춘 채 옮긴 VM을 따라감, 디렉터리 풀의 `shared` 바꾸기)로 나눴다(`2026-10-30-live-migration-design.md`). 디스크까지 복사하는 마이그레이션과 macOS(HVF) 노드는 넣지 않았다.
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다.
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
```

을 다음으로 바꾼다:

```markdown
- QEMU는 `-daemonize`로 실행해 `opld`와 세션을 분리한다(Proxmox와 같은 방식). 초기화에 실패하면 부모 프로세스의 종료 코드와 stderr로 바로 알 수 있다. Linux에서는 `systemd-run --scope --unit=opl-vm-<id>`로 감싸 VM마다 cgroup을 나눈다(`[hypervisor] systemd_scope`, systemd가 없는 환경에서는 끈다). cgroup 자원 제한은 아직 걸지 않는다.
- 런타임 파일은 `/run/opl/vm/{uuid}/`(macOS는 설정한 run_dir) 아래에 `qmp.sock`, `vnc.sock`, `qemu.pid`로 둔다. UEFI 변수처럼 재부팅 후에도 남아야 하는 파일은 `/var/lib/opl/vm/{uuid}/uefi-vars.fd`에 둔다. 라이브 마이그레이션(하위 프로젝트 4)에서 공유 스토리지로 옮기는 것을 다시 검토한다. 구현: 변수 파일은 노드 로컬에 그대로 둔다. 라이브 마이그레이션에서는 QEMU가 pflash 내용을 옮겨 대상 노드의 파일에 쓰고(계획 20, CI에서 확인), 멈춘 채 옮긴 VM은 시작할 때 변수를 가진 노드에서 받아 온다(계획 21, 라이브 마이그레이션 설계 §5).
- VM마다 모니터 태스크가 QMP 이벤트를 지켜본다. `SHUTDOWN` 이벤트 뒤에 연결이 끊기면 정상 종료(`Stopped`), 이벤트 없이 끊기면 `Crashed`로 판정하고 런타임 디렉터리를 정리한다.
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다.
- 사용 중인 Volume은 삭제할 수 없다(409).
```

을 다음으로 바꾼다:

```markdown
- `import`는 `qemu-img convert`로 VMDK, qcow2, raw 이미지를 가져온다.
- `PoolSpec.shared`는 라이브 마이그레이션(하위 프로젝트 4)에서 디스크 복사가 필요한지 판단하는 데 쓴다. 1차에서는 값을 저장하기만 한다. 구현: 공유 스토리지 설계(계획 15~19)부터 배치와 Task 노드 선택에 쓰고(`is_shared`, NFS·RBD·공유 LVM은 늘 공유), 라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀일 때만 한다(아니면 409 `migration_unsupported`, 디스크 복사는 하지 않는다). 디렉터리 풀의 `shared` 바꾸기는 계획 21이다.
- 사용 중인 Volume은 삭제할 수 없다(409).
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown

## 3. 스토리지
```

을 다음으로 바꾼다:

```markdown

구현: 원본 Task는 `opl-api/src/tasks/migrate.rs`이고 일반 Task 시간 제한 밖에서 단계마다 따로 제한한다(보내기는 `[migration] timeout`, `MigrateIncoming` 120초, `MigrateIncomingWait` 180초 — 받는 쪽 기다림 120초보다 길게, Abort 60초). 확정 쓰기와 되돌리기 쓰기는 같은 revision을 확인하므로 둘 중 하나만 적용되고, 확정 쓰기가 모호하게 실패하면 60초 동안 다시 해 본 뒤 되돌린다. S의 QEMU 버전은 실행 중인 원본의 QMP `query-version`, T의 버전은 `qemu --version`이다(같은 QEMU 바이너리). 접속 호스트와 `tls-hostname`은 T의 클러스터 멤버 주소(`members().addr`)의 호스트(T의 opld `address`, 노드 인증서에 있다)이고, 받는 QEMU는 T의 `cluster_listen` IP(기본 `0.0.0.0`, IPv6 `[::]`)에서 듣는다(`address`는 그 기계에서 `127.0.1.1` 등으로 풀릴 수 있다). 받는 쪽 처리는 opld가 아니라 opl-api의 `IncomingMigrations`(opld가 피어 서버에 주입)이고, 받는 VM의 기록이 Raft로 올 때까지 10초 기다린다. 포트는 opld가 범위에서 먼저 bind 해 보고 빈 것을 고른다(한 QEMU에 `migrate-incoming`은 한 번만 되므로 실패하면 그 QEMU를 버려야 한다). 모두 쓰는 중이면 "no free migration port in A-B on HOST"로 실패한다. TLS 객체(`opl-mig-tls`)는 QMP `object-add`로 매번 지우고 다시 만들고 양쪽 다 `verify-peer`다. 원본은 `ReconcilerHandle::begin_migration`(워커가 하던 걸음을 마칠 때까지 기다림)·`end_migration`으로 워커를 멈춰 둔다. QEMU 8.2의 받는 QEMU는 받기에 실패하면 스스로 끝나므로 T에는 자기 것이 아닌 VM의 충돌 이벤트가 생길 수 있다(무시한다). 남은 QEMU 정리는 Task 실행기 시작 때와 `tag_sweep`(30분)마다 돈다. 받는 중 표시 파일에는 마이그레이션 Task id를 적고, 정리는 `status.migration`이 그 Task와 이 노드를 가리킬 때만 남긴다(받기는 같은 VM의 다른 Task 받는 QEMU를 먼저 끈다). VM이 지워진 `postmigrate` 원본도 끈다. opld가 끝을 지켜보지 못한 마이그레이션(재시작, 되돌리기)은 `migrate_cancel`로 취소한 뒤 재개 여부를 정하고, 보류가 아니고 표시가 없는 자기 노드 VM의 `postmigrate` QEMU는 그 VM이 이 노드에서 돌던 것일 때만(`status.node`가 이 노드, phase `Running`·`Migrating`) 워커가 이어서 실행한다(확정 쓰기는 표시가 있는 revision에서만 적용되므로 더는 확정될 수 없다). 확정은 늘 `status.node`를 대상으로 바꾸므로, 아니면 다른 노드로 확정된 마이그레이션의 정리되지 않은 원본이 그 뒤 이 노드로 다시 옮겨진 것이다: 낡은 메모리를 이어서 실행하지 않고 워커와 정리가 그 QEMU를 끄고, 워커가 새로 켠다. `Migrating`을 쓴 뒤 그 VM의 스냅샷·복제·크기 늘리기 Task가 끝나지 않았으면 되돌리고, 확정 쓰기는 `spec.power`가 running일 때만 한다(검사와 기록 사이에 들어온 요청). 보류 중의 수명 이벤트(원본 크래시)는 보류가 풀린 뒤 처리한다. CI(QEMU 8.2.2)에서 확인: 완료 뒤 원본은 `postmigrate`, 받는 쪽은 `-S`라 `paused`로 남고 `cont` 뒤 `running`, 실패(연결 거부·TLS 없는 대상)에서 원본은 `running` 그대로.

## 3. 스토리지
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- 남은 활성화 정리(`deactivate_idle_volumes`)는 받는 중인 VM의 디스크를 쓰는 중으로 본다(로컬 하이퍼바이저에 떠 있다 — 기존 규칙). 원본의 `Migrating` VM도 그렇다.
```

을 다음으로 바꾼다:

```markdown
- 남은 활성화 정리(`deactivate_idle_volumes`)는 받는 중인 VM의 디스크를 쓰는 중으로 본다(로컬 하이퍼바이저에 떠 있다 — 기존 규칙). 원본의 `Migrating` VM도 그렇다.

구현: 공유 LVM 마이그레이션을 구현했다. CI(lvm2 2.03.16, sanlock)에서 쓰는 중인 LV의 `lvchange -asy`(배타→공유)·`-aey`(다른 노드가 쥐지 않았을 때 공유→배타)가 모두 되는 것을 확인했다(`lvs`의 `lv_active_exclusively`는 공유 잠금이어도 참으로 보이므로 잠금 모드 판단에 쓰지 않는다). `Storage::share_volume`·`exclusive_volume`(기본 Ok, 공유 LVM만 `-asy`·`-aey`)에 형식 인자를 더했고, T는 `opl_reconciler::resolve::incoming_spec`으로 디스크를 공유로 켠다. S가 `Migrating`을 쓴 뒤 공유로 바꾸고(실패하면 되돌린다), 실패하면 보류가 풀린 뒤 S의 워커가 다시 배타로, T는 넘겨받아 재개한 뒤 배타로 바꾼다 — 원본이 아직 놓지 않았으면 `retry_interval`마다 다시 해 본다. 배타 여부는 메모리에만 있으므로 워커는 자기가 켜서 시작하지 않은 실행 중 VM(opld 재시작, 넘겨받기)에 붙을 때 한 번 `exclusive_volume`(멱등 `-aey`)을 부른다. 남은 활성화 정리는 `status.migration.node`가 자기인 VM의 디스크도 쓰는 중으로 본다.
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown

## 5. UEFI 변수가 VM을 따라간다 (계획 21)
```

을 다음으로 바꾼다:

```markdown

구현: macOS 판정은 노드 상태의 하이퍼바이저 이름(`qemu-hvf`)이다 — TCG로 도는 macOS 노드는 막지 않는다. 메모리 판정은 `scheduler::node_fits`(온라인·유지보수·아키텍처·풀·브리지)와 따로 `Inventory::free_memory_mib`로 하고, 아키텍처가 다르면 400 `machine_arch_mismatch`다. 이미 `Migrating`인 VM은 `vm_not_running`이 아니라 `vm_busy`로 거절한다. 검사와 Task 만들기는 `vm_lock` 안에서 한다. `VmMigrate`는 Task 실행기의 VM 큐로 돈다. `[migration] timeout`은 humantime이 아니라 작은 자체 파서(`30m`, `1h30m`, `90s`)다. TLS 파일은 `{data_dir}/migration-tls/`(드라이버의 `state_dir`가 데이터 디렉터리, 기본 `/var/lib/opl/migration-tls`)이고 opld가 시작할 때마다 쓴다. oplctl `vm list`의 NODE 칸은 마이그레이션 중에 `원본 -> 대상`이다. 웹은 VM 상세에만 "마이그레이션"이 있고(목록 메뉴에는 없다) 대상 목록은 HVF 노드를 거르지 않는다(서버가 409로 이유를 보인다). 마이그레이션 중인 VM은 대시보드에서 원본 노드에 센다.

## 5. UEFI 변수가 VM을 따라간다 (계획 21)
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- 스냅샷·복제가 변수를 복사하는 기존 경로(Task 노드의 로컬 파일)는 `firmware_node`가 그 노드가 아니면 같은 RPC로 먼저 받는다.
```

을 다음으로 바꾼다:

```markdown
- 스냅샷·복제가 변수를 복사하는 기존 경로(Task 노드의 로컬 파일)는 `firmware_node`가 그 노드가 아니면 같은 RPC로 먼저 받는다.

계획 20 확인: 라이브 마이그레이션에서는 QEMU가 pflash 변수를 옮겨 받는 쪽이 재개될 때 대상 노드의 변수 파일에 쓴다(CI에서 원본 파일 끝 표식이 대상 파일에 생기고 두 파일이 같았다, `migration-it`도 본다). 이 절은 멈춘 채 옮기기만 다룬다.
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- 실제(`migration-it`, Linux root, CI `linux-kvm`): 한 호스트에서 opld 둘(`opld/tests/common`의 `Daemon`)로 클러스터를 만들고, 공유 디렉터리 풀의 qcow2 디스크 VM을 띄워 T로 옮긴다 — 확정 뒤 `spec.node`·T의 QEMU `running`·S의 QEMU 없음, 게스트가 계속 도는지(QMP `query-status`와 마이그레이션 전후 디스크 쓰기), UEFI VM의 변수 파일이 T에 쓰였는지. 확정 전 실패(대상 포트가 막힘)에서 원본이 계속 도는지. NFS·RBD·(되면) 공유 LVM 풀로 한 번씩.
- 계획 21: 2노드 실제 시험에서 멈춘 UEFI VM을 옮겨 켰을 때 변수가 같은지(앞 노드에서 `efivar`를 쓰는 대신 변수 파일의 해시로 본다), `PATCH /pools`.
```

을 다음으로 바꾼다:

```markdown
- 실제(`migration-it`, Linux root, CI `linux-kvm`): 한 호스트에서 opld 둘(`opld/tests/common`의 `Daemon`)로 클러스터를 만들고, 공유 디렉터리 풀의 qcow2 디스크 VM을 띄워 T로 옮긴다 — 확정 뒤 `spec.node`·T의 QEMU `running`·S의 QEMU 없음, 게스트가 계속 도는지(QMP `query-status`와 마이그레이션 전후 디스크 쓰기), UEFI VM의 변수 파일이 T에 쓰였는지. 확정 전 실패(대상 포트가 막힘)에서 원본이 계속 도는지. NFS·RBD·(되면) 공유 LVM 풀로 한 번씩.

구현: 기능 `migration-it`(opld, `crates/opld/tests/migration.rs`)은 Linux 전용이 아니고 이 Mac에서도 TCG로 돈다(HVF에서는 원본 QEMU가 가끔 `assert_hvf_ok`로 죽는다). 한 호스트의 두 노드는 systemd 스코프 이름과 탭 이름이 겹치므로 스코프를 끄고 NIC 없이 띄운다. 게스트 OS가 없으므로 디스크 내용은 정지한 VM에서 `qemu-io`로 본다. 포트 막힘 실패 뒤 포트를 풀고 되돌아오기까지 본다. NFS·RBD·공유 LVM은 각 풀 기능과 그 시험의 환경 변수가 있을 때(0→1→0 왕복), 공유 LVM은 세 시점에 `lvchange -aey`가 되는지 본다. 2노드 in-process 시험(`opl-api/tests/two_nodes.rs`, MockHypervisor)은 성공·실패·되돌리기를 본다.
- 계획 21: 2노드 실제 시험에서 멈춘 UEFI VM을 옮겨 켰을 때 변수가 같은지(앞 노드에서 `efivar`를 쓰는 대신 변수 파일의 해시로 본다), `PATCH /pools`.
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown
- macOS 노드는 마이그레이션을 할 수 없다.
- 공유 LVM 디스크: 계획 20의 확인 결과에 따른다(§3).
```

을 다음으로 바꾼다:

```markdown
- macOS 노드는 마이그레이션을 할 수 없다.
- 공유 LVM 디스크: 계획 20의 확인 결과에 따른다(§3). 구현: 지원한다(마이그레이션 동안 LV 공유 잠금).
- 계획 20 전에 클러스터에 들어온 노드의 인증서는 키 용도에 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가 실패한다. opld가 시작할 때 경고하고, 노드를 다시 합류시켜야 새 인증서를 받는다.
- 받기 준비 RPC가 기한(120초)에 끊기면 대상에 띄우던 받는 QEMU나 공유로 켠 디스크가 남을 수 있다. 다음 정리(`tag_sweep`, 30분)나 같은 VM의 다음 마이그레이션이 치운다.
- 받기 확인 뒤 확정 전에 대상의 받는 QEMU가 죽으면, 확정 뒤 대상 워커가 VM을 새로 켠다(게스트 메모리 상태를 잃는다, 두 QEMU가 함께 돌지는 않는다).
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 727개, 실패 0

Run: `cargo test -p opld --features qemu-it,migration-it --test smoke --test e2e --test migration`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 307개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 라이브 마이그레이션 안내, 설계에 구현하며 정한 것, 처음 분해의 하위 프로젝트 4"
```

---

## 완료 기준 (계획 20)

- 디스크가 모두 공유 풀에 있는 실행 중 VM을 API·oplctl·웹으로 다른 노드에 옮길 수 있고, 검사(같은 노드 400, 실행 중 아님·마이그레이션 중·공유 아님·HVF·오프라인·유지보수·자원 부족 409, 아키텍처 400)가 설계와 같다.
- 마이그레이션 동안 VM의 다른 동작은 409 `vm_busy`이고, 콘솔은 쓸 수 있다.
- 확정 전 실패에서는 원본이 계속 돌고, 확정 뒤에는 대상만 돈다. 두 QEMU가 함께 도는 경로가 없다(재시작·모호한 쓰기·늦게 끝난 마이그레이션 포함).
- UEFI 변수가 대상 노드의 파일에 쓰인다. 공유 LVM 디스크는 마이그레이션 뒤 대상에서 배타 잠금으로 돌아간다.
- 이 Mac에서 `migration-it`(공유 디렉터리)이, Linux CI(`linux-kvm`)에서 공유 디렉터리·NFS·RBD·공유 LVM 마이그레이션과 드라이버 TLS 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **디스크가 모두 공유 풀이어야 한다**(로컬 디스크 복사 없음).
- **같은 QEMU 버전·같은 CPU의 노드 사이만 된다.** CPU는 검사하지 않는다(`-cpu host`).
- **취소가 없다.** 수렴하지 않으면 `auto-converge`, 시간 제한(기본 30분)이 지나면 실패로 끝나 원본이 계속 돈다.
- **마이그레이션 포트 범위(기본 60000–60099)를 노드 사이 방화벽에서 열어야 한다.** macOS 노드는 마이그레이션을 할 수 없다.
- **계획 20 전에 합류한 노드는 다시 합류해야** Linux(QEMU 8.2)에서 마이그레이션 TLS가 된다.
- 그 밖의 제한은 설계 문서 §8에 있다.

## 실행 후 기록 (2026-10-31)

9개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts`·`Cargo.lock` 포함). 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 두 차례 돌려 고쳤다 — 위 Task의 코드에 이미 들어 있다.

- 1차: 진행 중이던 마이그레이션이 opld 재시작·되돌리기 뒤에 끝나 원본이 멈춘 채 남는 경우(`cancel_migration`과 `postmigrate` 원본 재개), API 검사와 Task 사이 경쟁(`Migrating` 기록 뒤 다시 확인, `power`가 Running이 아니면 확정하지 않음, `modify_spec` 다시 확인), 받는 중 표시의 Task id, 공유 LVM 배타 복귀를 메모리에 두지 않음, 받는 QEMU는 `cluster_listen` IP에서, 예전 노드 인증서 경고, 보류 중 이벤트, `oplctl vm migrate`의 24시간 대기, 마이그레이션 중 콘솔 허용.
- 2차: 1차의 재개 규칙이 확정된 마이그레이션의 옛 원본(`postmigrate`)을 냉각 이동 뒤 되살릴 수 있었다 — 상태 노드가 이 노드이고 Running·Migrating일 때만 재개하고, 그 밖에는 끄고 새로 켠다.

CI 확인용 브랜치에서 찾아 고친 것: QEMU 8.2(GnuTLS)는 키 용도가 critical인 노드 인증서에 keyEncipherment를 요구한다(`sign_node`에 더함, 예전 노드는 다시 합류). 대상 펌웨어가 넘겨받은 직후 UEFI 변수를 더 바꾸므로 시험은 파일 전체가 아니라 원본 표식을 본다.

테스트: macOS `cargo test --workspace` 727개, `opl-api` `storage-it` 251개, `opl-qemu`·`opl-driver` `qemu-it`(실제 TLS 마이그레이션 2개, TCG), `opld` `qemu-it`·`migration-it`(opld 둘, TCG), 브라우저 E2E, 웹 Vitest 307개, `fmt`·`clippy -D warnings`(모든 기능). Linux CI `linux-kvm`(KVM, QEMU 8.2)에서 드라이버 TLS 마이그레이션 2개와 opld 마이그레이션 E2E 4개(공유 디렉터리·NFS·RBD·공유 LVM, 공유 LVM은 `lvchange -aey` 세 시점)가 통과했다. 확인용 브랜치의 마지막 CI는 5개 작업 모두 통과했다. 병합한 main(`08f6f99`)의 CI도 5개 작업이 모두 통과했다.

## 다음 계획으로 넘길 사항

- 계획 21: 멈춘 채 옮긴 VM의 UEFI 변수 따라가기(`VmStatus.firmware_node`, 노드 사이 변수 전송), 디렉터리 풀의 `shared` 바꾸기(`PATCH /pools/{key}`) — 설계 §5·§6.
- 공유 LVM 마이그레이션 시험은 한 호스트(잠금 하나)라 진짜 두 호스트 잠금은 확인하지 못했다.
- CI에서 `opl-api`의 `storage-it` 전체를 돌리는 것을 검토한다.
