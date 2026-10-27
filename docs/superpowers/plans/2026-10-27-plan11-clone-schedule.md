# opLite 계획 11: 스냅샷에서 VM 복제와 예약 스냅샷 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 스냅샷(또는 꺼진 VM의 지금 상태)에서 디스크를 통째로 복사해 새 VM을 만들고, VM마다 주기(매시간·매일·매주)와 보관 개수를 정해 스냅샷을 자동으로 만들고 정리한다. API·CLI·웹 UI 모두에서 한다(하위 프로젝트 3의 나머지).

**Architecture:** 복제는 새 Task 종류 `vm-clone`으로, 대상은 원본 VM이라 그 VM의 줄에 서서 스냅샷 Task와 차례로 돈다. 작업 노드(계획 10의 `vm_placement`)에서 디스크마다 새 볼륨을 만들고 `Storage::copy_volume`(`qemu-img convert`, 스냅샷이면 `-l snapshot.name=<id>`)으로 복사한 뒤 UEFI 변수를 복사하고 새 VM 리소스를 만든다. 예약은 VM 스펙의 `snapshot_schedule`이고, 리더에서 `tick`마다 도는 `SnapshotScheduler`가 순수 함수 `plan`으로 할 일(만들기·보관 삭제·건너뛰기)을 정해 API 핸들러와 같은 함수(`start_create`·`start_delete`)로 요청한다. 실패 이유는 VM의 `status.schedule_error`에 남는다.

**Tech Stack:** Rust 2024(MSRV 1.88), QEMU 11.1(맥)·8.2(Linux CI)의 `qemu-img convert`/`qemu-io`, openraft, axum 0.8, utoipa, clap; 웹은 계획 8~10과 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW, Playwright). 새 의존성은 `opl-api`의 `time`(이미 워크스페이스에 있다)뿐이다.

**설계 문서:** `docs/superpowers/specs/2026-10-26-snapshots-design.md` (§12의 계획 11)
**선행 계획:** 계획 1~10 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| VM 스펙의 `snapshot_schedule`(`every`·`keep` 1..=100·`memory`), VM 상태의 `schedule_error`, Task 종류 `vm-clone` | 링크드 클론(디스크를 공유하는 복제) |
| `Storage::copy_volume`(스냅샷 태그나 지금 내용에서, 실행 중 VM의 스냅샷이면 `-U`) | 예약 시각 정하기(주기만 있다) |
| `POST /vms/{key}/clone`과 검사, 복제 Task(볼륨 이름, 복사, UEFI 변수, 새 MAC, 실패 정리) | 공유 스토리지(NFS·LVM·ZFS·iSCSI·Ceph) — 계획 12부터 |
| 끝나지 않은 복제 Task가 있으면 원본 VM의 동작·수정·삭제 막기 | 오프라인 노드에 걸린 Task를 끝내거나 무시하기 |
| 예약 작업 `SnapshotScheduler`(리더, 보관 정리, 재시도 규칙, `schedule_error`), opld 설정 `[schedule]` | 웹이 남은 전원 Task를 보고 버튼을 미리 막기 |
| 되돌리기가 VM의 지금 예약을 지키기 | `schedule_error` 번역(코드 없는 서버 메시지) |
| `oplctl vm clone`, `vm snapshot schedule`, 스냅샷 목록의 SCHEDULED 열, `vm delete --with-snapshots`가 예약 먼저 끄기 | |
| 웹: VM 메뉴·상세의 복제 대화상자, VM 폼의 예약 스냅샷, 스냅샷 탭의 예약 요약·마지막 실패·예약 배지 | |
| 실제 QEMU 통합 테스트, 브라우저 E2E 단계 | |

## 이전 계획에서 이어받는 사항

계획 10의 "다음 계획으로 넘길 사항" 가운데 첫 항목(스냅샷에서 VM 복제, 예약 스냅샷, 설계 §12)이 이 계획이다. 나머지는 이 계획에서 하지 않는다:
- 오프라인 노드에 배정된 Task를 끝내거나 무시하는 방법. 그동안 예약 작업은 그 VM을 건너뛴다(알고 받아들인 제한).
- 웹 스냅샷 탭이 남은 전원 Task를 보고 버튼을 미리 막기. 이 계획은 서버의 `check_not_busy` 가운데 VM 상태로 알 수 있는 부분(시작·정지 중, `spec.power`와 phase가 어긋남)만 웹의 `isBusy`로 옮긴다.
- 계획 9에서 넘긴 사항(업로드 사전 확인 API, 시도 제한, 노드 아키텍처 API 등).

## 실행 환경

계획 10과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상과 `qemu-io`), Node.js 22.12 이상, pnpm 10.33.2, `pnpm -C web exec playwright install chromium`(한 번). 실제 QEMU 테스트는 기능으로 켠다: `opl-qemu`·`opl-driver`·`opl-reconciler`·`opld`의 `qemu-it`, `opl-api`·`opl-driver`의 `storage-it`, `opld`의 `web-it`(브라우저 E2E).

Linux CI(`linux-kvm`, Ubuntu 24.04)는 QEMU 8.2다. `qemu-img`·`qemu-io`·QMP에는 8.2에 있는 옵션만 쓴다(계획 10에서 `qemu-img snapshot -f`가 8.2에 없어 Linux에서 실패했다). 이 계획의 `qemu-img convert -q -f F [-U] [-l snapshot.name=T] -O F`는 8.2에서 확인했다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `Cargo.lock`(cargo가 빌드하며 갱신), `web/src/api/schema.d.ts`(`pnpm -C web gen:api`). 둘 다 커밋한다.

## 설계 결정

- **복제는 전체 복사.** `qemu-img convert`로 디스크를 통째로 복사한다(링크드 클론 없음). 형식은 원본과 같고, 스냅샷이면 태그 = 스냅샷 id(`-l snapshot.name=`, 경로처럼 `,`는 `,,`). `-U`(공유 잠금 무시)는 스냅샷에서 복사하고 원본 VM이 완전히 꺼지지 않았을 때만 쓴다(QEMU가 디스크를 쥐고 있지만 스냅샷 데이터는 바뀌지 않는다). 꺼진 VM의 지금 내용을 복사할 때는 쓰지 않아 잠금이 지켜진다. 가져오기처럼 풀 안의 임시 파일에 쓴 뒤 hard link로 게시하고, 대상이 이미 있으면 `VolumeExists`, raw에 태그를 주면 거부, 실패하면 대상 파일을 남기지 않는다.
- **복제 요청과 Task.** `POST /vms/{key}/clone {name, snapshot?, pool?}` → 202. 권한은 VM Write와 Volume Write. 원본은 `ready` 스냅샷(이름이나 id), 없으면 꺼진 VM의 지금 상태(실행 중이면 409 `vm_running`). Task의 대상은 원본 VM이라 그 VM의 줄에서 스냅샷 Task와 차례로 돈다. Task는 원본을 다시 읽어 조건을 다시 보고, 디스크마다 `{새 VM}-{원래 볼륨}`(63자로 자르고, 겹치면 `-2`, `-3`…)을 `pending`으로 만들어 복사한 뒤, UEFI 변수를 복사하고 볼륨을 `ready`로 바꾼 다음 새 VM을 만든다. 새 VM은 디스크 이름만 바꾸고 MAC 새로, `node` 없음, `power: stopped`, 예약·스냅샷·라벨 없음. 실패·시간 초과면 이 Task가 만든 볼륨(파일·리소스)과 변수 사본을 지운다. 이름 경합은 다시 시도하지 않고 실패다.
- **복제 중 원본 VM 막기.** 꺼진 VM을 복사하는 동안 켜면 `qemu-img` 잠금으로 시작이 실패한다. 그래서 끝나지 않은 `vm-clone` Task가 있으면 원본 VM의 동작·수정은 409 `vm_busy`(계획 10의 `check_no_snapshot_task`가 보는 종류에 더한다). VM 삭제도 끝나지 않은 스냅샷·복제 Task가 있으면 `vm_busy`다(`vm_has_snapshots` 검사 뒤). 복제는 원본의 전원을 바꾸지 않으므로 전원 Task의 `superseded` 검사는 그대로다.
- **예약 작업.** 리더에서 `tick`마다(리더 확인 → `read_barrier` → 목록) 예약이 있는 VM을 본다. 그 VM의 스냅샷·복제 Task가 끝나지 않았거나 `pending` 스냅샷이 있으면 건너뛴다. `scheduled`이고 `ready`인 스냅샷이 `keep`개보다 많으면 가장 오래된 것 하나를 지우고 그 tick에는 만들지 않는다(그래서 잠시 `keep + 1`개가 있다). 아니면 마지막 예약 스냅샷에서 주기가 지났을 때(없으면 바로) `auto-YYYYMMDD-HHMM`(UTC, 겹치면 `-2`…) 이름, 빈 설명으로 만든다. 메모리는 `schedule.memory`이고 실행 중이며 디스크가 있을 때. 사용자가 만든 스냅샷은 지우지 않는다. 판단은 순수 함수 `plan`이라 단위 테스트한다.
- **재시도 규칙.** 시도는 VM별로 메모리에만 기억한다(리더가 바뀌거나 opld가 재시작하면 잊고 한 번 일찍 다시 한다). 만들기 요청이 4xx로 거절되면(바쁨, 노드 오프라인 등 잠깐일 수 있다) `hour` 뒤에, Task를 보냈는데 실패했으면 다음 주기에 다시 한다. 5xx는 시도로 세지 않아 다음 tick에 다시 한다. 보관 삭제는 지우려 한 스냅샷이 끝나지 않은 Task 없이 아직 있으면 실패로 보고 그 삭제를 `hour` 동안 미루며, 그동안에도 만들기는 이어 간다(보관 정리가 막혀도 새 스냅샷은 만든다).
- **`schedule_error`.** 4xx 거절이면 그 메시지(raw 디스크면 `snapshot_unsupported`, 시작·정지 중이면 `vm_busy`, Task 노드가 없거나 오프라인이면 요청 전에 `node_offline`)를 쓴다. 예약 스냅샷의 만들기 Task는 끝날 때 성공이면 지우고 실패(시간 초과·재시작 중단 포함)면 그 오류를 쓴다. 보관 삭제 Task는 실패할 때만 쓴다. 같은 값이면 쓰지 않고, 조건부 쓰기(`update_status_checked`)로 쓴다. reconciler의 상태 쓰기는 `snapshot`처럼 이 필드도 저장소 것을 지킨다. 예약을 꺼도 지우지 않는다.
- **주기와 설정.** hourly = `hour`, daily = 24×`hour`, weekly = 168×`hour`. `opld` 설정 `[schedule] tick_secs = 60, hour_secs = 3600`(0은 거부). 테스트만 줄인다(실제 QEMU 테스트 1/1, E2E 1/2).
- **되돌리기는 지금 예약을 지킨다.** 스냅샷의 VM 스펙으로 되돌려도 `snapshot_schedule`은 VM의 지금 값이다. 복제한 VM은 예약이 없다. 예약 설정은 VM 수정(PUT, VM Write)이다.
- **CLI.** CLI에 VM 수정 명령이 없어 설계 §8의 VM 수정 옵션 대신 `oplctl vm snapshot schedule <vm> [--every … --keep N [--memory] | --off]`를 둔다. 옵션이 없으면 지금 예약과 마지막 예약 실패를 보인다. 바꿀 때는 VM을 읽어 `If-Match: revision`으로 라벨·스펙을 그대로 둔 채 예약만 바꿔 PUT 한다(`revision_mismatch`면 세 번까지 바로, 스냅샷·복제 Task 중이라 `vm_busy`면 1초마다 30초까지 다시). `vm delete --with-snapshots`는 확인을 통과하면 예약을 먼저 끄고 다시 확인한 뒤 지운다(그새 예약 스냅샷이 생기지 않게).
- **웹.** "바쁨"은 서버 `check_not_busy`의 VM 상태 부분과 같은 `isBusy`(시작·정지 중, 또는 `spec.power`와 phase가 어긋남)로, 복제 대화상자와 스냅샷 탭이 함께 쓴다. 복제 대화상자의 풀은 서버와 같은 순서(`spec.node` → `status.node` → 첫 디스크 볼륨의 노드 → 접속한 노드)로 정한 노드에서 쓸 수 있는 것만 보이고, 정하기 전에는 "원본과 같은 풀"만 둔다. 고른 스냅샷이 사라지면 다른 것으로 바꾸지 않고 까닭을 보이며 막는다. `schedule_error`는 code가 없는 서버 메시지라 번역하지 않고 그대로 보인다.

## 파일 구조

```
crates/opl-types/src/{vm.rs, task.rs}          # SnapshotSchedule·ScheduleEvery, VmStatus.schedule_error, TaskOp::VmClone
crates/opl-reconciler/src/worker.rs            # 상태 쓰기가 schedule_error를 지킴
crates/opl-driver/src/storage.rs               # Storage::copy_volume, DirStorage(qemu-img convert)
crates/opl-api/src/{snapshots.rs, schedule.rs(새로), tasks.rs, tasks/clone.rs(새로), tasks/snapshot.rs, vms.rs, lib.rs}
crates/opl-api/tests/{clones.rs(새로), schedules.rs(새로), snapshot_disks.rs, common/mod.rs, openapi.rs}
crates/opld/src/{config.rs, member.rs}         # [schedule], SnapshotScheduler 띄우고 멈추기
crates/opld/tests/{snapshots.rs, web.rs, common/mod.rs}
crates/oplctl/src/cli.rs                       # vm clone, vm snapshot schedule, SCHEDULED 열
web/src/vms/{VmCloneDialog.tsx(새로), VmMenu.tsx, VmDetailPage.tsx, VmFormPage.tsx, VmSnapshots.tsx, form.ts, actions.ts}
web/src/i18n/{ko.json, en.json}, web/e2e/vm.spec.ts, README.md, docs/superpowers/specs/2026-10-26-snapshots-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 예약 스냅샷 설정·오류 필드와 VM 복제 Task 종류

`VmSpec.snapshot_schedule`(`SnapshotSchedule`: `every` = `hourly`/`daily`/`weekly`, `keep`, `memory` 기본 false)과 `VmStatus.schedule_error`를 더한다. 둘 다 `#[serde(default)]`라 기존 VM은 그대로 읽힌다. `keep`은 1..=100이고 `VmSpec::validate`의 새 오류로 막는다. Task에 `vm-clone`(`TaskOp::VmClone { name, snapshot, pool }`)을 더하지만 실행은 Task 4까지 없다(지금은 실패로 끝난다). `schedule_error`는 예약 작업과 Task가 쓰는 값이므로 reconciler의 상태 쓰기가 `snapshot`처럼 저장소 것을 지키게 한다. OpenAPI의 `VmSpec`·`VmStatus`·`TaskOp`가 바뀌므로 웹 타입을 다시 만들고, VM 편집 폼이 스펙을 통째로 보낼 때 예약 설정을 잃지 않게 들고 다닌다(화면은 Task 9).

**Files:**
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
            node,
        }
```

을 다음으로 바꾼다:

```rust
            node,
            snapshot_schedule: None,
        }
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        // 복제 실행은 아직 없다.
        TaskOp::VmClone { .. } => Err("vm clone is not implemented yet".into()),
    }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)은 Task 실행기가 쓰는
    /// 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어
    /// 그 `snapshot`으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            match self
```

을 다음으로 바꾼다:

```rust

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            match self
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            snapshot: vm.status.snapshot,
        };
```

을 다음으로 바꾼다:

```rust
            snapshot: vm.status.snapshot,
            schedule_error: vm.status.schedule_error.clone(),
        };
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
            node: Some(node),
        },
```

을 다음으로 바꾼다:

```rust
            node: Some(node),
            snapshot_schedule: None,
        },
```

`crates/opl-reconciler/tests/reconcile.rs` 전체:

```rust
//! Mock 하이퍼바이저로 조정 규칙(설계 5.3)을 확인한다.

mod common;

use std::time::Duration;

use common::{Env, vm};
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, RestartPolicy, VmPhase, VmSpec, VmStatus};
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
            snapshot: None,
            schedule_error: None,
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
async fn new_stop_after_cancelled_stop_sends_acpi_again() {
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(2)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // 종료를 요청했다가 취소했다. 게스트는 ACPI 요청을 무시한다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    tokio::time::sleep(Duration::from_millis(2300)).await;

    // acpi_timeout이 지난 뒤의 새 종료 요청은 앞선 요청을 이어받지 않고 다시 ACPI부터 보낸다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(env.pid(vm.id).await.is_some(), "VM was force-stopped");

    env.hv.guest_shutdown(vm.id);
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn unresponsive_vm_is_forced_off_after_acpi_timeout() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    // QMP로 다룰 수 없으니 ACPI 요청은 실패한다. 그래도 acpi_timeout 뒤 강제 종료한다.
    env.hv.set_unresponsive(vm.id, true);
    env.set_power(vm.id, PowerState::Stopped).await;
    let got = env
        .wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    assert_eq!(got.status.pid, None);
    assert_eq!(env.pid(vm.id).await, None);
}

#[tokio::test]
async fn unresponsive_vm_is_reattached_and_reported() {
    // 응답하지 않게 되어도 이벤트는 없으므로 재동기화로 알아챈다.
    let env = Env::start_with(|c| c.resync_interval = Duration::from_millis(200)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    env.hv.set_unresponsive(vm.id, true);
    let got = env
        .wait(vm.id, "reported unresponsive", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("unresponsive"))
        })
        .await;
    // 끄거나 다시 띄우지 않는다.
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.pid(vm.id).await, pid);

    // 다시 붙으면 오류를 지운다.
    env.hv.set_unresponsive(vm.id, false);
    let got = env
        .wait(vm.id, "recovered", |v| v.status.last_error.is_none())
        .await;
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert_eq!(env.pid(vm.id).await, pid);
}

#[tokio::test]
async fn host_kill_does_not_turn_power_off() {
    let env = Env::start().await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    // 호스트의 SIGTERM처럼 우리가 요청하지 않은, 게스트 밖에서 일어난 종료
    env.hv.stop(vm.id, StopMode::Force).await.unwrap();

    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.last_error, None);
    assert_eq!(env.pid(vm.id).await, got.status.pid);
}

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
    let env = Env::start_with(|c| c.acpi_timeout = Duration::from_secs(60)).await;
    let vm = vm("web", env.node);
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;

    env.force_stop(vm.id);
    // 명령이 처리될 시간을 준다.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(env.pid(vm.id).await, before);
    assert_eq!(env.get(vm.id).status.phase, VmPhase::Running);

    // 무시한 명령이 남아 있다가 뒤이은 종료를 강제 종료로 바꾸지 않는다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopping", |v| v.status.phase == VmPhase::Stopping)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(env.get(vm.id).status.phase, VmPhase::Stopping);
    assert_eq!(env.pid(vm.id).await, before, "VM was force-stopped");
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
async fn power_off_keeps_current_snapshot() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    let snapshot = Uuid::now_v7();
    vm.status.snapshot = Some(snapshot);
    vm.status.schedule_error = Some("vm busy".into());
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.guest_shutdown(vm.id);

    // 상태를 새로 써도 스냅샷 위치와 예약 오류는 reconciler가 지우지 않는다.
    let got = env
        .wait(vm.id, "powered off", |v| {
            v.spec.power == PowerState::Stopped && v.status.phase == VmPhase::Stopped
        })
        .await;
    assert_eq!(got.status.snapshot, Some(snapshot));
    assert_eq!(got.status.schedule_error.as_deref(), Some("vm busy"));
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

/// 정지한 VM을 만들고 reconciler가 처리할 때까지 기다린다.
async fn stopped_vm(env: &Env) -> opl_types::vm::Vm {
    let mut vm = vm("web", env.node);
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    vm
}

#[tokio::test]
async fn restore_tag_is_used_by_next_start_only() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, Some("snap-1".into()))]);

    // 한 번 쓰면 사라진다. 다음 시작은 처음부터 부팅한다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn cleared_restore_tag_is_not_used() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.clear_restore(vm.id);
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, None)]);
}

#[tokio::test]
async fn failed_restore_marks_failed_and_consumes_tag() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 일시 오류라도 다시 시도하지 않는다. 태그 없이 다시 시작하면 처음부터 부팅하게 된다.
    env.hv.fail_next_start(DriverError::Timeout("qmp"));
    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to restore snapshot snap-1: timed out waiting for qmp")
    );
    assert_eq!(env.pid(vm.id).await, None);

    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn status_writes_keep_snapshot_position_written_meanwhile() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 작업자가 VM을 읽고 시작하는 사이에 Task 실행기가 현재 스냅샷 위치를, 예약 작업이 예약 오류를 쓴다.
    env.hv.delay_starts(Duration::from_millis(300));
    env.set_power(vm.id, PowerState::Running).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let snapshot = Uuid::now_v7();
    let status = VmStatus {
        snapshot: Some(snapshot),
        schedule_error: Some("vm busy".into()),
        ..current.status.clone()
    };
    env.cluster
        .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
        .await
        .unwrap();

    // 작업자가 쓴 Running status가 그 값을 덮지 않는다.
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.get(vm.id).status.snapshot, Some(snapshot));
    assert_eq!(
        env.get(vm.id).status.schedule_error.as_deref(),
        Some("vm busy")
    );
}
```

`crates/opl-types/src/task.rs`에서

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷 Task는 VM이다.
    pub target: Uuid,
```

을 다음으로 바꾼다:

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제 Task는 (원본) VM이다.
    pub target: Uuid,
```

`crates/opl-types/src/task.rs`에서

```rust
        size_bytes: u64,
    },
```

을 다음으로 바꾼다:

```rust
        size_bytes: u64,
    },
    /// 대상 VM을 `name`으로 복제한다. `snapshot`이 없으면 지금 상태(꺼진 VM)에서,
    /// `pool`이 없으면 원본 볼륨의 풀에 디스크를 복사한다.
    VmClone {
        name: String,
        snapshot: Option<Uuid>,
        pool: Option<String>,
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
    fn clone_op_is_tagged_by_kind() {
        let snapshot = Uuid::now_v7();
        let op = TaskOp::VmClone {
            name: "web-02".into(),
            snapshot: Some(snapshot),
            pool: Some("fast".into()),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "vm-clone",
                "name": "web-02",
                "snapshot": snapshot,
                "pool": "fast"
            })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        let op = TaskOp::VmClone {
            name: "web-02".into(),
            snapshot: None,
            pool: None,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/opl-types/src/vm.rs` 전체:

```rust
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

pub type Vm = Resource<VmSpec, VmStatus>;

pub const MIN_MEMORY_MIB: u32 = 64;

/// VM의 원하는 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// 예약 스냅샷. None이면 예약하지 않는다.
    #[serde(default)]
    pub snapshot_schedule: Option<SnapshotSchedule>,
}

impl ResourceSpec for VmSpec {
    const KIND: Kind = Kind::Vm;
    type Status = VmStatus;
}

/// VM 하나의 최대 NIC 수. tap 장치 이름(15자)에 순번 두 자리를 넣는다.
pub const MAX_NICS: usize = 16;

/// 예약 스냅샷 보관 개수의 최댓값.
pub const MAX_SCHEDULE_KEEP: u32 = 100;

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
        if self.nics.len() > MAX_NICS {
            return Err(ValidationError::TooManyNics);
        }
        let mut macs = BTreeSet::new();
        for mac in self.nics.iter().filter_map(|n| n.mac) {
            if mac.0[0] & 1 == 1 {
                return Err(ValidationError::MulticastMac(mac));
            }
            if !macs.insert(mac) {
                return Err(ValidationError::DuplicateMac(mac));
            }
        }
        if let Some(schedule) = &self.snapshot_schedule
            && !(1..=MAX_SCHEDULE_KEEP).contains(&schedule.keep)
        {
            return Err(ValidationError::ScheduleKeep);
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
    #[error("MAC address {0} is a multicast address")]
    MulticastMac(MacAddr),
    #[error("MAC address {0} is used by more than one NIC")]
    DuplicateMac(MacAddr),
    #[error("a VM can have at most {MAX_NICS} NICs")]
    TooManyNics,
    #[error("snapshot_schedule.keep must be between 1 and {MAX_SCHEDULE_KEEP}")]
    ScheduleKeep,
}

/// 예약 스냅샷: 주기마다 스냅샷을 만들고, 예약으로 만든 것을 `keep`개만 남긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SnapshotSchedule {
    pub every: ScheduleEvery,
    /// 1부터 `MAX_SCHEDULE_KEEP`까지
    pub keep: u32,
    /// 실행 중이면 메모리도 저장한다.
    #[serde(default)]
    pub memory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ScheduleEvery {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CpuModel {
    /// 호스트 CPU를 그대로 노출한다. TCG에서는 max로 대체된다.
    #[default]
    Host,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Firmware {
    Bios,
    #[default]
    Uefi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MachineType {
    /// x86_64 전용
    Q35,
    /// aarch64 전용
    Virt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DiskSpec {
    /// Volume 리소스 이름
    pub volume: String,
    pub bus: DiskBus,
    #[serde(default)]
    pub boot_order: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DiskBus {
    VirtioBlk,
    /// 드라이버 없이 설치하는 Windows 게스트용. q35에서만 쓸 수 있다.
    Sata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum NicModel {
    Virtio,
    E1000e,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum PowerState {
    Running,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
}

/// VM의 실제 상태. reconciler만 기록한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VmStatus {
    pub phase: VmPhase,
    pub node: Option<Uuid>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    /// 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
    pub snapshot: Option<Uuid>,
    /// 마지막 예약 스냅샷이 실패한 이유. 성공하면 지운다.
    pub schedule_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(value_type = String))]
#[serde(try_from = "String", into = "String")]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// QEMU/KVM 관례 접두사 `52:54:00`에 난수 3바이트를 붙인 주소를 만든다.
    /// 난수가 24비트라 VM이 많으면 충돌할 수 있다. 호출자(API)는 클러스터 안에서 중복을 확인하고 다시 뽑는다.
    pub fn generate() -> Self {
        let r = Uuid::new_v4().into_bytes();
        MacAddr([0x52, 0x54, 0x00, r[0], r[1], r[2]])
    }
}

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
            if part.len() != 2 || !part.bytes().all(|c| c.is_ascii_hexdigit()) {
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
        assert_eq!(spec.snapshot_schedule, None);
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

        let mut s = sample();
        let mac: MacAddr = "01:00:5e:00:00:01".parse().unwrap();
        s.nics[0].mac = Some(mac);
        assert_eq!(s.validate(), Err(ValidationError::MulticastMac(mac)));

        let mut s = sample();
        let mac: MacAddr = "52:54:00:12:34:56".parse().unwrap();
        s.nics[0].mac = Some(mac);
        let mut second = s.nics[0].clone();
        second.network = "wan".into();
        s.nics.push(second);
        assert_eq!(s.validate(), Err(ValidationError::DuplicateMac(mac)));

        let mut s = sample();
        s.nics = vec![s.nics[0].clone(); MAX_NICS + 1];
        for nic in &mut s.nics {
            nic.mac = None;
        }
        assert_eq!(s.validate(), Err(ValidationError::TooManyNics));
    }

    #[test]
    fn status_missing_fields_default() {
        let st: VmStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, VmStatus::default());
        assert_eq!(st.snapshot, None);
        assert_eq!(st.schedule_error, None);
    }

    #[test]
    fn snapshot_schedule_defaults_to_none() {
        assert_eq!(sample().snapshot_schedule, None);
    }

    #[test]
    fn snapshot_schedule_round_trips() {
        let mut json = sample_json();
        json["snapshot_schedule"] = serde_json::json!({ "every": "daily", "keep": 7 });
        let spec: VmSpec = serde_json::from_value(json).unwrap();
        assert_eq!(
            spec.snapshot_schedule,
            Some(SnapshotSchedule {
                every: ScheduleEvery::Daily,
                keep: 7,
                memory: false,
            })
        );
        let back = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            back["snapshot_schedule"],
            serde_json::json!({ "every": "daily", "keep": 7, "memory": false })
        );
        for (every, text) in [
            (ScheduleEvery::Hourly, "hourly"),
            (ScheduleEvery::Daily, "daily"),
            (ScheduleEvery::Weekly, "weekly"),
        ] {
            assert_eq!(serde_json::to_value(every).unwrap(), text);
        }

        let mut json = sample_json();
        json["snapshot_schedule"] = serde_json::json!({ "every": "monthly", "keep": 7 });
        assert!(serde_json::from_value::<VmSpec>(json).is_err());
    }

    #[test]
    fn validate_checks_schedule_keep() {
        let with_keep = |keep| {
            let mut s = sample();
            s.snapshot_schedule = Some(SnapshotSchedule {
                every: ScheduleEvery::Hourly,
                keep,
                memory: true,
            });
            s.validate()
        };
        assert_eq!(with_keep(1), Ok(()));
        assert_eq!(with_keep(MAX_SCHEDULE_KEEP), Ok(()));
        assert_eq!(with_keep(0), Err(ValidationError::ScheduleKeep));
        assert_eq!(
            with_keep(MAX_SCHEDULE_KEEP + 1),
            Err(ValidationError::ScheduleKeep)
        );
    }

    #[test]
    fn mac_parses_and_formats() {
        let mac: MacAddr = "52:54:00:AB:cd:0f".parse().unwrap();
        assert_eq!(mac.0, [0x52, 0x54, 0x00, 0xab, 0xcd, 0x0f]);
        assert_eq!(mac.to_string(), "52:54:00:ab:cd:0f");
        assert_eq!(serde_json::to_value(mac).unwrap(), "52:54:00:ab:cd:0f");
    }

    #[test]
    fn generated_mac_is_unicast_with_qemu_prefix() {
        let a = MacAddr::generate();
        let b = MacAddr::generate();
        assert_eq!(&a.0[..3], &[0x52, 0x54, 0x00]);
        assert_eq!(a.0[0] & 1, 0);
        assert_ne!(a, b);
    }

    #[test]
    fn mac_rejects_malformed() {
        for bad in [
            "",
            "52:54:00:ab:cd",
            "52:54:00:ab:cd:0f:11",
            "5:54:00:ab:cd:0f",
            "zz:54:00:ab:cd:0f",
            "+2:54:00:ab:cd:0f",
        ] {
            assert!(bad.parse::<MacAddr>().is_err(), "{bad} should be rejected");
        }
        assert!(serde_json::from_value::<MacAddr>("nope".into()).is_err());
    }
}
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  const broken = vm("db", "failed", {
    status: { phase: "failed", node: null, pid: null, last_error: "no firmware", snapshot: null },
  });
```

을 다음으로 바꾼다:

```tsx
  const broken = vm("db", "failed", {
    status: {
      phase: "failed",
      node: null,
      pid: null,
      last_error: "no firmware",
      snapshot: null,
      schedule_error: null,
    },
  });
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  const failed = vm("db", "failed", {
    status: { phase: "failed", node: "n1", pid: null, last_error: "boom", snapshot: null },
  });
  const stopping = vm("app", "stopping", {
    status: { phase: "stopping", node: "n1", pid: 7, last_error: null, snapshot: null },
  });
  const elsewhere = vm("far", "running", {
    status: { phase: "running", node: "n2", pid: 8, last_error: null, snapshot: null },
  });
```

을 다음으로 바꾼다:

```tsx
  const failed = vm("db", "failed", {
    status: {
      phase: "failed",
      node: "n1",
      pid: null,
      last_error: "boom",
      snapshot: null,
      schedule_error: null,
    },
  });
  const stopping = vm("app", "stopping", {
    status: {
      phase: "stopping",
      node: "n1",
      pid: 7,
      last_error: null,
      snapshot: null,
      schedule_error: null,
    },
  });
  const elsewhere = vm("far", "running", {
    status: {
      phase: "running",
      node: "n2",
      pid: 8,
      last_error: null,
      snapshot: null,
      schedule_error: null,
    },
  });
```

`web/src/test/fixtures.ts`에서

```ts
      snapshot: null,
    },
```

을 다음으로 바꾼다:

```ts
      snapshot: null,
      schedule_error: null,
    },
```

`web/src/vms/form.test.ts`에서

```ts
      nics: [{ network: "lan", model: "e1000e", mac: "52:54:00:12:34:56" }],
    },
  });
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
```

을 다음으로 바꾼다:

```ts
      nics: [{ network: "lan", model: "e1000e", mac: "52:54:00:12:34:56" }],
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
    },
  });
  // 폼에 없는 예약 스냅샷도 고칠 때 잃지 않는다.
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
```

`web/src/vms/form.ts`에서

```ts
  restart: Schemas["RestartPolicy"];
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
```

을 다음으로 바꾼다:

```ts
  restart: Schemas["RestartPolicy"];
  /** 예약 스냅샷. null이면 없다. */
  snapshotSchedule: Schemas["SnapshotSchedule"] | null;
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
```

`web/src/vms/form.ts`에서

```ts
    restart: "never",
    start: true,
```

을 다음으로 바꾼다:

```ts
    restart: "never",
    snapshotSchedule: null,
    start: true,
```

`web/src/vms/form.ts`에서

```ts
    restart: s.restart ?? "never",
    start: false,
```

을 다음으로 바꾼다:

```ts
    restart: s.restart ?? "never",
    snapshotSchedule: s.snapshot_schedule ?? null,
    start: false,
```

`web/src/vms/form.ts`에서

```ts
    restart: v.restart,
  };
```

을 다음으로 바꾼다:

```ts
    restart: v.restart,
    snapshot_schedule: v.snapshotSchedule,
  };
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 301개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 200개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): 예약 스냅샷 설정·오류 필드와 VM 복제 Task 종류"
```

---

### Task 2: 스토리지 드라이버: 스냅샷이나 지금 내용으로 볼륨 복사

`Storage::copy_volume(src_pool, src_id, format, tag, force_share, dst_pool, dst_id)`를 더하고 `DirStorage`가 `qemu-img convert -q -f F [-U] [-l snapshot.name=T] -O F`로 구현한다(태그의 `,`는 `,,`). 가져오기처럼 풀 안의 임시 파일에 쓴 뒤 hard link로 게시하므로 실패해도 대상 파일이 남지 않는다. 원본이 없으면 `VolumeNotFound`, raw에 태그를 주면 `InvalidSpec`, 대상이 있으면 `VolumeExists`. QEMU 8.2(Linux CI)에 있는 옵션만 쓴다. `opl-api` 테스트의 가짜 스토리지는 복사를 기록하고 원본 크기를 대상에 준다. 실제 `qemu-img`·`qemu-io` 테스트로 스냅샷 태그에서 복사하면 그 순간의 내용이 나오는지(qcow2), raw·qcow2의 지금 내용 복사를 본다.

**Files:**
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/tests/dir_storage.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/tests/common/mod.rs`에서

```rust
        Ok(())
    }
}
```

을 다음으로 바꾼다:

```rust
        Ok(())
    }
    async fn copy_volume(
        &self,
        _: &StoragePoolSpec,
        src_id: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError> {
        self.record("copy", dst_id).await;
        let size = self.sizes.lock().unwrap().get(&src_id).copied();
        self.set_size(dst_id, size.ok_or(DriverError::VolumeNotFound(src_id))?);
        Ok(())
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
        size_bytes: u64,
    ) -> Result<(), DriverError>;
```

을 다음으로 바꾼다:

```rust
        size_bytes: u64,
    ) -> Result<(), DriverError>;

    /// 볼륨을 같은 형식의 새 볼륨 `dst_id`로 복사한다. `tag`가 있으면 그 스냅샷 때의 내용을,
    /// 없으면 지금 내용을 복사한다. `force_share`는 실행 중인 VM이 쥔 볼륨의 스냅샷을 읽을 때만
    /// 쓴다(스냅샷 데이터는 바뀌지 않는다). 대상이 이미 있으면 `VolumeExists`.
    /// 실패하면 만들다 만 파일을 남기지 않는다.
    #[allow(clippy::too_many_arguments)]
    async fn copy_volume(
        &self,
        src_pool: &StoragePoolSpec,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError>;
```

`crates/opl-driver/src/storage.rs`에서

```rust
        Ok(())
    }
}
```

을 다음으로 바꾼다:

```rust
        Ok(())
    }

    async fn copy_volume(
        &self,
        src_pool: &StoragePoolSpec,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError> {
        let src = match tag {
            Some(_) => self.snapshot_path(src_pool, src_id, format)?,
            None => Self::existing_path(src_pool, src_id, format)?,
        };
        pool_dir(dst_pool)?;
        let dst = Self::volume_path(dst_pool, dst_id, format);
        if dst.exists() {
            return Err(DriverError::VolumeExists(dst_id));
        }
        // `-l`의 값은 옵션 문자열이므로 `,`를 `,,`로 쓴다.
        let snapshot = tag.map(|t| format!("snapshot.name={}", t.replace(',', ",,")));
        let mut before = vec!["convert", "-q", "-f", fmt_name(format)];
        if force_share {
            before.push("-U");
        }
        if let Some(s) = &snapshot {
            before.extend(["-l", s.as_str()]);
        }
        before.extend(["-O", fmt_name(format), path_str(&src)?]);
        self.write_new(dst_id, &dst, &before, &[]).await
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

을 다음으로 바꾼다:

```rust
    #[tokio::test]
    async fn copy_volume_passes_snapshot_and_share_options() {
        let tools = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        // 경로와 태그의 쉼표는 옵션 문자열에서 `,,`로 쓴다.
        let dst_dir = tools.path().join("dst");
        std::fs::create_dir(&dst_dir).unwrap();
        let (src_pool, dst_pool) = (
            pool(src_dir.path().to_str().unwrap()),
            pool(dst_dir.to_str().unwrap()),
        );
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let qcow2 = ImageFormat::Qcow2;
        let src_path = DirStorage::volume_path(&src_pool, src, qcow2);
        std::fs::write(&src_path, b"").unwrap();
        // 인자를 한 줄에 하나씩 적고 출력 파일을 만든다.
        let args = tools.path().join("args");
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            &format!("printf '%s\\n' \"$@\" > {args:?}\necho copy > \"$last\""),
        ));

        storage
            .copy_volume(&src_pool, src, qcow2, Some("a,b"), true, &dst_pool, dst)
            .await
            .unwrap();
        let dst_path = DirStorage::volume_path(&dst_pool, dst, qcow2);
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"copy\n");
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "qcow2",
                src_path.to_str().unwrap(),
            ]
        );
        assert!(got[got.len() - 1].ends_with(".part"), "{got:?}");

        // 태그 없이, 잠금을 지키며 지금 내용을 복사한다.
        let dst2 = Uuid::now_v7();
        storage
            .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, dst2)
            .await
            .unwrap();
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-O",
                "qcow2",
                src_path.to_str().unwrap()
            ]
        );
    }

    #[tokio::test]
    async fn copy_volume_checks_source_and_never_overwrites() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeNotFound(id)) if id == src),
            "{res:?}"
        );

        std::fs::write(DirStorage::volume_path(&p, src, raw), b"").unwrap();
        // raw 볼륨에는 스냅샷이 없다.
        let res = storage
            .copy_volume(&p, src, raw, Some("t"), false, &p, dst)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");

        let dst_path = DirStorage::volume_path(&p, dst, raw);
        std::fs::write(&dst_path, b"precious").unwrap();
        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeExists(id)) if id == dst),
            "{res:?}"
        );
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"precious");

        let rel = pool("pools/local");
        let res = storage
            .copy_volume(&p, src, raw, None, false, &rel, dst)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
    }

    #[tokio::test]
    async fn failed_copy_leaves_no_files() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let src = Uuid::now_v7();
        std::fs::write(DirStorage::volume_path(&p, src, ImageFormat::Raw), b"").unwrap();
        // 출력 파일을 반쯤 쓰고 실패한다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho broken >&2\nexit 1",
        ));

        let err = storage
            .copy_volume(&p, src, ImageFormat::Raw, None, false, &p, Uuid::now_v7())
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(pool_files(tmp.path()), vec![format!("{src}.raw")]);
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
fn qemu_io(path: &Path, cmd: &str) -> bool {
    std::process::Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
```

을 다음으로 바꾼다:

```rust
fn qemu_io(path: &Path, cmd: &str) -> bool {
    qemu_io_fmt(path, ImageFormat::Qcow2, cmd)
}

fn qemu_io_fmt(path: &Path, format: ImageFormat, cmd: &str) -> bool {
    let format = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    std::process::Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");
    }
}

#[tokio::test]
async fn copy_volume_copies_snapshot_or_current_contents() {
    let tmp = tempfile::tempdir().unwrap();
    // 다른 풀로 복사한다. 경로의 쉼표는 옵션 문자열에서 `,,`로 써야 한다.
    let (src_dir, dst_dir) = (tmp.path().join("src"), tmp.path().join("d,st"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&dst_dir).unwrap();
    let (src_pool, dst_pool) = (pool(&src_dir), pool(&dst_dir));
    let storage = DirStorage::new("qemu-img");
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storage
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage
        .snapshot_create(&src_pool, src, qcow2, &tag)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xcd 0 64k"));

    // 스냅샷 때의 내용. -U(실행 중인 VM의 스냅샷에서 복사할 때)도 받아들인다.
    for force_share in [false, true] {
        let dst = Uuid::now_v7();
        storage
            .copy_volume(
                &src_pool,
                src,
                qcow2,
                Some(&tag),
                force_share,
                &dst_pool,
                dst,
            )
            .await
            .unwrap();
        let dst_path = DirStorage::volume_path(&dst_pool, dst, qcow2);
        assert!(qemu_io(&dst_path, "read -P 0xab 0 64k"), "{force_share}");
        let info = storage.volume_info(&dst_pool, dst, qcow2).await.unwrap();
        assert_eq!(info.virtual_bytes, 64 * MIB);
        // 사본에는 스냅샷이 따라오지 않는다.
        assert!(
            storage
                .snapshot_tags(&dst_pool, dst, qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // 태그가 없으면 지금 내용
    let now = Uuid::now_v7();
    storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now)
        .await
        .unwrap();
    assert!(qemu_io(
        &DirStorage::volume_path(&dst_pool, now, qcow2),
        "read -P 0xcd 0 64k"
    ));
    // 대상이 있으면 덮어쓰지 않는다.
    let err = storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");

    // 없는 태그는 실패하고 파일을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .copy_volume(
            &src_pool,
            src,
            qcow2,
            Some("gone"),
            false,
            &dst_pool,
            missing,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert_eq!(std::fs::read_dir(&dst_dir).unwrap().count(), 3);
    assert!(!DirStorage::volume_path(&dst_pool, missing, qcow2).exists());

    // raw도 같은 형식으로 복사한다.
    let raw = ImageFormat::Raw;
    let src_raw = Uuid::now_v7();
    let src_raw_path = storage
        .create_volume(&src_pool, src_raw, 64 * MIB, raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(&src_raw_path, raw, "write -P 0x5a 0 64k"));
    let dst_raw = Uuid::now_v7();
    storage
        .copy_volume(&src_pool, src_raw, raw, None, false, &dst_pool, dst_raw)
        .await
        .unwrap();
    let dst_raw_path = DirStorage::volume_path(&dst_pool, dst_raw, raw);
    assert!(qemu_io_fmt(&dst_raw_path, raw, "read -P 0x5a 0 64k"));
    let info = storage.volume_info(&dst_pool, dst_raw, raw).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 304개, 실패 0

Run: `cargo test -p opl-driver --features storage-it --test dir_storage`
Expected: PASS 7개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): 스냅샷이나 지금 내용으로 볼륨을 복사하는 Storage::copy_volume"
```

---

### Task 3: API: VM 복제 요청

`POST /vms/{key}/clone`(`CloneRequest { name, snapshot?, pool? }`, `snapshot`은 이름이나 id) → 202 `vm-clone` Task. Task의 대상은 원본 VM, 노드는 `vm_placement`. 권한은 VM Write와 Volume Write. 검사: 이름 규칙, 같은 이름 VM이 있으면 409 `name_taken`(VM 리소스만 본다), 원본 VM이 바쁘면 409 `vm_busy`, 스냅샷은 `ready`여야 하고(409 `snapshot_not_ready`) 없으면 꺼진 VM이어야 한다(409 `vm_running`), 풀이 없으면 400, Task 노드에서 쓸 수 없으면 409 `pool_not_on_node`, 원본 스펙(스냅샷이면 그 `vm_spec`)의 볼륨이나 NIC 네트워크(`user` 제외)가 없으면 400. 끝나지 않은 복제 Task가 있으면 원본 VM의 전원 조작·수정은 `vm_busy`다(꺼진 VM을 복사하는 동안 켜면 이미지 잠금으로 시작이 실패한다). OpenAPI에 등록하고 웹 타입을 다시 만든다. 테스트는 실행기를 멈춰 둔 mock 클러스터로 Task의 모양만 본다.

**Files:**
- Create: `crates/opl-api/tests/clones.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(snapshots::revert_snapshot))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(snapshots::revert_snapshot))
        .routes(routes!(snapshots::clone_vm))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

`crates/opl-api/src/snapshots.rs` 전체:

```rust
//! VM 스냅샷 목록·조회·만들기·되돌리기·삭제. 권한은 VM을 따른다(보기는 Read, 나머지는 Operate).
//! 스냅샷이나 꺼진 VM에서 VM 복제(VM·볼륨 Write).
//! 실제 작업은 VM이 있는 노드의 Task 실행기가 한다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::Cluster;
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{is_stopped, prepare};

/// 설명의 최대 글자 수
const MAX_DESCRIPTION_CHARS: usize = 1024;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    /// VM 안에서 고유한 이름
    pub name: String,
    /// 최대 1024자
    #[serde(default)]
    pub description: String,
    /// 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으면 true다.
    pub memory: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 복제할 스냅샷(이름이나 id). 없으면 꺼진 VM의 지금 상태에서 복제한다.
    pub snapshot: Option<String>,
    /// 새 디스크를 둘 풀. 없으면 원본 볼륨마다 그 볼륨의 풀이다.
    pub pool: Option<String>,
}

/// VM의 디스크 작업을 할 노드: 다음에 돌 노드(`spec.node`), 지금(또는 마지막으로) 돈 노드
/// (`status.node`), 배치된 적이 없으면 첫 디스크 볼륨을 만든 노드. 꺼진 VM을 다른 노드로 옮겼으면
/// 그 디스크는 옮긴 노드에서 쓰인다. 셋 다 없으면 `None`.
pub(crate) fn vm_placement(cluster: &Cluster, vm: &Vm) -> Option<Uuid> {
    vm.spec.node.or(vm.status.node).or_else(|| {
        let disk = vm.spec.disks.first()?;
        let volume = cluster.get_by_name::<VolumeSpec>(&disk.volume).ok()??;
        volume.status.node
    })
}

/// 스냅샷 Task를 실행할 노드: [`vm_placement`], 없으면 요청을 받은 이 노드(설계 §3).
pub(crate) fn vm_node(state: &AppState, vm: &Vm) -> Uuid {
    vm_placement(&state.cluster, vm).unwrap_or(state.node)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다: phase가 Starting·Stopping이거나, 원하는 전원
/// 상태(`spec.power`)에 아직 닿지 않았거나(reconciler가 켜거나 끄는 중), 전원 Task(시작·정지·
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다.
fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
        PowerState::Running => vm.status.phase == VmPhase::Running,
        PowerState::Stopped => is_stopped(vm),
    };
    let doing = match vm.status.phase {
        VmPhase::Starting => Some("starting"),
        VmPhase::Stopping => Some("stopping"),
        _ if !settled => Some(match vm.spec.power {
            PowerState::Running => "starting",
            PowerState::Stopped => "stopping",
        }),
        _ => None,
    };
    if let Some(doing) = doing {
        return Err(ApiError::conflict(
            "vm_busy",
            format!("VM {:?} is {doing}; try again when it has settled", vm.name),
        ));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::VmStart | TaskOp::VmStop | TaskOp::VmForceStop | TaskOp::VmReboot
            )
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is being started, stopped or rebooted; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 이 VM의 스냅샷 만들기·되돌리기나 복제 Task가 끝나지 않았으면 409 `vm_busy`. 스냅샷 Task는
/// 전원을 끄고 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나
/// 담는 스펙이 실제와 달라진다. 꺼진 VM을 복사하는 동안 켜면 디스크 잠금 때문에 시작이 실패한다. 시작한 Task는 실행기의 시간 제한 안에 끝나지만, 오프라인 노드에 배정된
/// Task는 그 노드가 돌아올 때까지 남는다(설계 §11).
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::SnapshotCreate { .. }
                    | TaskOp::SnapshotRevert { .. }
                    | TaskOp::VmClone { .. }
            )
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is being snapshotted, reverted or cloned; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 모든 디스크가 스냅샷을 담을 수 있고 다 만들어졌는지. dir 백엔드(`Storage::supports_snapshots`)는
/// qcow2만 담는다. raw가 하나라도 있으면 일부 디스크만 되돌아가므로 거부한다.
fn check_disks(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.spec.format != ImageFormat::Qcow2 {
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} has non-qcow2 disk {:?}; only qcow2 disks can be snapshotted",
                    vm.name, disk.volume
                ),
            ));
        }
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
    }
    Ok(())
}

/// VM의 스냅샷 전부
fn snapshots_of(state: &AppState, vm: Uuid) -> Result<(u64, Vec<Snapshot>), ApiError> {
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok((
        revision,
        items.into_iter().filter(|s| s.spec.vm == vm).collect(),
    ))
}

/// 경로의 `{name}`은 VM 안의 스냅샷 이름이나 id다.
fn find_snapshot(state: &AppState, vm: &Vm, key: &str) -> Result<Snapshot, ApiError> {
    let id = Uuid::try_parse(key).ok();
    snapshots_of(state, vm.id)?
        .1
        .into_iter()
        .find(|s| Some(s.id) == id || s.spec.name == key)
        .ok_or_else(|| {
            ApiError::not_found(format!("snapshot {key:?} of VM {:?} not found", vm.name))
        })
}

fn check_ready(snapshot: &Snapshot) -> Result<(), ApiError> {
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err(ApiError::conflict(
            "snapshot_not_ready",
            format!("snapshot {:?} is still being created", snapshot.spec.name),
        ));
    }
    Ok(())
}

/// 모든 VM의 스냅샷. watch와 함께 쓰는 목록이다.
#[utoipa::path(get, path = "/snapshots", tag = "snapshots", responses(
    (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
    (status = "4XX", body = crate::error::ErrorBody),
))]
pub async fn list_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    responses(
        (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_vm_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let (revision, items) = snapshots_of(&state, vm.id)?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses(
        (status = 200, body = Resource<SnapshotSpec, SnapshotStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<Json<Snapshot>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    Ok(Json(find_snapshot(&state, &vm, &name)?))
}

/// `pending` 스냅샷을 만들고(부모는 VM의 현재 위치, `vm_spec`은 지금 스펙) 그것을 채울 Task를
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, qcow2가 아닌 디스크가 있으면 409
/// `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
#[utoipa::path(post, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateSnapshotRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    check_name(&req.name)?;
    if req.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(ApiError::bad_request(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    check_disks(&state, &vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    let memory = req.memory.unwrap_or(running && has_disk);
    if memory && !running {
        return Err(ApiError::bad_request(format!(
            "VM {:?} is not running; only its disks can be snapshotted",
            vm.name
        )));
    }
    if memory && !has_disk {
        return Err(ApiError::bad_request(format!(
            "VM {:?} has no disk to hold the memory state",
            vm.name
        )));
    }
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name: req.name,
            parent: vm.status.snapshot,
            memory,
            description: req.description,
            scheduled: false,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    // 같은 VM에 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&snapshot).await?;
    let op = TaskOp::SnapshotCreate {
        snapshot: snapshot.id,
    };
    match create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
        Err(e) => {
            // 채울 Task가 없는 pending 스냅샷은 이름만 차지한다. 지우지 못하면 주인 노드의
            // Task 실행기가 다음 시작에 고아로 지운다.
            if let Err(d) = state
                .cluster
                .delete::<SnapshotSpec>(snapshot.id, None)
                .await
            {
                tracing::warn!(snapshot = %snapshot.id, error = %d, "cannot remove snapshot without a task");
            }
            Err(e)
        }
    }
}

/// 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌릴 Task를 돌려준다(202). 실행 중이면 강제로 끈다.
/// 스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다.
#[utoipa::path(post, path = "/vms/{key}/snapshots/{name}/revert", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn revert_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    // 되돌린 스펙을 VM 수정처럼 검증한다. 노드는 지금 것을 유지하고, 시작은 Task가 한다.
    let mut spec = snapshot.spec.vm_spec.clone();
    spec.node = vm.spec.node;
    spec.power = PowerState::Stopped;
    prepare(&state, vm.id, &mut spec)?;
    let op = TaskOp::SnapshotRevert {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 스냅샷을 지울 Task를 돌려준다(202). 자식은 지운 스냅샷의 부모로 옮겨 붙는다.
#[utoipa::path(delete, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
/// (실행 중인데 스냅샷이 없으면 409 `vm_running`). 원본 스펙의 볼륨이 모두 있어야 하고, `pool`은 Task
/// 노드에서 쓸 수 있어야 한다. Task는 원본 VM을 대상으로 그 VM의 디스크 노드에서 돈다.
#[utoipa::path(post, path = "/vms/{key}/clone", tag = "vms",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CloneRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn clone_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CloneRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    check_not_busy(&state, &vm)?;
    let (snapshot, spec) = match &req.snapshot {
        Some(key) => {
            let snapshot = find_snapshot(&state, &vm, key)?;
            check_ready(&snapshot)?;
            (Some(snapshot.id), snapshot.spec.vm_spec)
        }
        None if !is_stopped(&vm) => {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "VM {:?} is running; stop it or clone from a snapshot",
                    vm.name
                ),
            ));
        }
        None => (None, vm.spec.clone()),
    };
    let node = vm_node(&state, &vm);
    if let Some(name) = &req.pool {
        let pool = state
            .cluster
            .get_by_name::<StoragePoolSpec>(name)?
            .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ApiError::conflict(
                "pool_not_on_node",
                format!(
                    "storage pool {name:?} is not available on the node holding VM {:?}",
                    vm.name
                ),
            ));
        }
    }
    for disk in &spec.disks {
        if state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "volume {:?} not found",
                disk.volume
            )));
        }
    }
    // 새 VM은 원본 스펙의 NIC를 그대로 가진다(VM 만들기와 같은 검사).
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    let op = TaskOp::VmClone {
        name: req.name,
        snapshot,
        pool: req.pool,
    };
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM의 스냅샷 수. VM 삭제를 막는 데 쓴다.
pub(crate) fn count_snapshots(state: &AppState, vm: Uuid) -> Result<usize, ApiError> {
    Ok(snapshots_of(state, vm)?.1.len())
}
```

`crates/opl-api/tests/clones.rs`를 만든다:

```rust
//! VM 복제 API: 검사, Task 만들기, 복제 중 원본 VM 막기.
//! Task 실행기를 멈춰 두고 API가 남긴 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// Task 실행기 없이 띄운다. 만든 Task는 pending으로 남는다.
async fn env() -> Env {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env
}

/// 남은 Task를 끝난 것으로 기록한다.
async fn finish_tasks(env: &Env) {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    for task in tasks {
        env.cluster
            .update_status::<TaskSpec>(task.id, &done)
            .await
            .unwrap();
    }
}

/// 풀(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, name: &str, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 풀 `local`에 바로 Ready인 qcow2 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, name: &str) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: MIB,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    volume.id
}

/// 볼륨을 붙인 꺼진 VM을 만든다.
async fn vm_with_disks(admin: &Client, name: &str, volumes: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = volumes
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// VM의 실제 상태와 그에 맞는 원하는 전원 상태(시작 중·실행 중이면 켜짐)를 직접 쓴다.
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let power = match phase {
        VmPhase::Starting | VmPhase::Running => PowerState::Running,
        _ => PowerState::Stopped,
    };
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let spec = VmSpec {
        power,
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let status = VmStatus {
        phase,
        node,
        ..vm.status
    };
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// 스냅샷을 만들어 Ready로 기록하고 그 id를 돌려준다.
async fn ready_snapshot(env: &Env, admin: &Client, vm: &str, name: &str) -> Uuid {
    let (status, body) = admin
        .post(
            &format!("/vms/{vm}/snapshots"),
            json!({ "name": name, "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id: Uuid = body["spec"]["op"]["snapshot"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
    };
    env.cluster
        .update_status::<SnapshotSpec>(id, &status)
        .await
        .unwrap();
    finish_tasks(env).await;
    id
}

async fn clone(admin: &Client, vm: &str, body: Value) -> (StatusCode, Value) {
    admin.post(&format!("/vms/{vm}/clone"), body).await
}

async fn assert_conflict(admin: &Client, vm: &str, body: Value, code: &str) {
    let (status, got) = clone(admin, vm, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{got}");
    assert_eq!(got["code"], code, "{got}");
}

#[tokio::test]
async fn clone_records_a_task_on_the_source_vm() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "fast", vec![env.node]).await;
    volume(&env, "root").await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;

    // 꺼진 VM의 지금 상태에서. 배치되지 않은 VM이면 요청을 받은 노드에서 돈다.
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "vm-clone", "name": "web-2", "snapshot": null, "pool": null })
    );
    assert_eq!(task["spec"]["target"], vm.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(task["status"]["state"], "pending");
    finish_tasks(&env).await;

    // 스냅샷(이름이나 id)에서, 다른 풀로
    let snap = ready_snapshot(&env, &admin, "web", "s1").await;
    for key in ["s1".to_string(), snap.to_string()] {
        let (status, task) = clone(
            &admin,
            &vm.to_string(),
            json!({ "name": "web-3", "snapshot": key, "pool": "fast" }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(
            task["spec"]["op"],
            json!({ "kind": "vm-clone", "name": "web-3", "snapshot": snap, "pool": "fast" })
        );
        finish_tasks(&env).await;
    }
}

#[tokio::test]
async fn clone_validates_request() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    volume(&env, "root").await;
    vm_with_disks(&admin, "web", &["root"]).await;
    admin.post("/vms", vm_request("db")).await;

    for body in [
        json!({ "name": "Bad_Name" }),
        json!({ "name": "web-2", "pol": "local" }),
        json!({ "name": "web-2", "pool": "missing" }),
    ] {
        let (status, got) = clone(&admin, "web", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{got}");
    }
    assert_conflict(&admin, "web", json!({ "name": "db" }), "name_taken").await;
    // 풀이 이 VM의 디스크 노드에서 쓸 수 없다.
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "pool": "remote" }),
        "pool_not_on_node",
    )
    .await;
    let (status, _) = clone(&admin, "missing", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = clone(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "missing" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // 아직 만들고 있는 스냅샷에서는 복제하지 않는다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "s1" }),
        "snapshot_not_ready",
    )
    .await;

    // 원본 스펙의 볼륨이 없어졌으면 복사할 수 없다.
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<VolumeSpec>(root.id, None)
        .await
        .unwrap();
    let (status, body) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn clone_needs_the_networks_of_the_source_spec() {
    let env = env().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut req = vm_request("web");
    req["spec"]["nics"] = json!([
        { "network": "user", "model": "virtio" },
        { "network": "lan", "model": "virtio" }
    ]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish_tasks(&env).await;

    // 원본 스펙의 네트워크가 없어졌으면 새 VM을 만들 수 없다(`user`는 늘 있다).
    let lan = env
        .cluster
        .get_by_name::<opl_types::network::NetworkSpec>("lan")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<opl_types::network::NetworkSpec>(lan.id, None)
        .await
        .unwrap();
    let (status, body) = clone(&admin, "web", json!({ "name": "web-3" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|m| m.contains("network \"lan\" not found")),
        "{body}"
    );
}

#[tokio::test]
async fn running_vm_clones_only_from_a_snapshot() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    volume(&env, "root").await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    ready_snapshot(&env, &admin, "web", "s1").await;
    let elsewhere = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Running, Some(elsewhere)).await;

    assert_conflict(&admin, "web", json!({ "name": "web-2" }), "vm_running").await;
    // 스냅샷에서는 된다. Task는 VM이 도는 노드에서 돈다.
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], elsewhere.to_string());
    finish_tasks(&env).await;

    // 시작·정지 중이거나 전원 Task가 끝나지 않았으면 기다린다.
    for phase in [VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(elsewhere)).await;
        assert_conflict(
            &admin,
            "web",
            json!({ "name": "web-2", "snapshot": "s1" }),
            "vm_busy",
        )
        .await;
    }
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "snapshot": "s1" }),
        "vm_busy",
    )
    .await;
}

#[tokio::test]
async fn unfinished_clone_blocks_power_and_changes_of_the_source() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    volume(&env, "root").await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    // 복사하는 동안 켜면 디스크 잠금 때문에 시작이 실패한다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    let (_, current) = admin.get("/vms/web").await;
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": current["spec"] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    finish_tasks(&env).await;
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn cloning_needs_vm_and_volume_write() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    admin.post("/vms", vm_request("web")).await;

    let (status, body) = clone(&viewer, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = clone(&operator, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/vms/{key}/snapshots/{name}/revert",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/vms/{key}/snapshots/{name}/revert",
        "/vms/{key}/clone",
        "/watch",
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 310개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 200개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): VM 복제 요청 POST /vms/{key}/clone"
```

---

### Task 4: VM 복제 Task 실행

`opl-api/src/tasks/clone.rs`. 원본 VM의 줄에 서서 그 VM의 스냅샷 Task와 차례로 돌고, 줄에서 기다리는 사이 바뀌었을 수 있으므로 원본을 다시 본다(VM·스냅샷이 없어졌으면 실패, 스냅샷이 없으면 꺼져 있어야, 원본 볼륨이 이 노드에 있고 `ready`, 대상 풀을 이 노드에서 쓸 수 있어야, 새 VM 이름이 아직 쓰이지 않았어야). 디스크마다 `{새 VM}-{원래 볼륨}`(63자로 자르고 끝의 `-`를 떼며, 겹치면 `-2`, `-3`…)을 원본과 같은 형식·크기의 `pending` 볼륨으로 만들어 `copy_volume`으로 복사한다. `-U`는 스냅샷에서 복사하고 원본 VM이 완전히 꺼지지 않았을 때만. 모든 디스크를 복사하면 UEFI 변수(스냅샷이면 그 사본, 아니면 지금 파일, 없으면 건너뜀)를 새 VM 자리로 복사하고 볼륨을 `ready`로 바꾼 뒤 새 VM(새 MAC, `node` 없음, `stopped`, 예약·라벨 없음)을 만든다. 실패·시간 초과면 이 Task가 만든 볼륨과 변수 사본을 지운다. VM 삭제도 끝나지 않은 스냅샷·복제 Task가 있으면 `vm_busy`다(설명이 바뀌어 웹 타입을 다시 만든다). 실제 qemu-img 테스트는 계획 10의 `snapshot_disks.rs`에 더한다.

**Files:**
- Create: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/clones.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/snapshot_disks.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod clone;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::snapshots::vm_placement;

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
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
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
        sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
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
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_clone = matches!(task.spec.op, TaskOp::VmClone { .. });
        return (is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op) || vm_clone)
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
            fail(&ctx, &task, error).await;
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

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    cleanup(ctx, task).await;
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
async fn cleanup(ctx: &Ctx, task: &Task) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op
        && let Err(e) = snapshot::discard_pending(ctx, snapshot).await
    {
        tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
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
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
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

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나
/// 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서 다시 본다.
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
        Some((vm, _)) if matches!(vm.status.phase, VmPhase::Starting | VmPhase::Stopping) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => ctx
            .hv
            .block_resize(vm.id, disk, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
        _ => ctx
            .storage
            .resize_volume(&pool.spec, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool.spec, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
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

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) {
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
                    if let Err(e) = remove_volume(ctx, volume.id).await {
                        tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume");
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
                // API가 Task를 배정했을 노드(`vm_placement`). 그것도 없으면 디스크가 없는 VM이라
                // 태그가 없으니 어느 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, &vm)));
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

`crates/opl-api/src/tasks/clone.rs`를 만든다:

```rust
//! VM 복제 Task(설계 §5 "복제"). 원본 VM의 줄에 서서 그 VM의 스냅샷 Task와 차례로 돈다. API가
//! 확인한 뒤 줄에서 기다리는 사이 원본 VM이나 스냅샷이 바뀌거나 없어졌을 수 있으므로 다시 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
use opl_types::snapshot::SnapshotPhase;
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{MacAddr, PowerState, Vm, VmSpec};
use uuid::Uuid;

use super::snapshot::{
    check_local, check_settled, copy_file, get_snapshot, get_vm, is_down, ready_disks, vars_copy,
};
use super::{Ctx, mark_ready, remove_volume};

/// 리소스 이름의 최대 길이
const MAX_NAME: usize = 63;

/// 복제 Task가 지금까지 만든 것. Task가 실패하거나 시간이 다 되면 실행기의 정리([`discard`])가
/// 지운다. opld가 죽으면 사라진다. 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 고아 정리가
/// 지우지만(볼륨은 모두 복사한 뒤 VM을 만들기 바로 전에 `ready`로 바꾼다), `ready`로 바꾼 뒤 VM을
/// 만들기 전에 죽으면 볼륨과 UEFI 변수 사본이 남는다(설계 §11).
#[derive(Default)]
pub(super) struct Made {
    volumes: Vec<Uuid>,
    vars: Option<PathBuf>,
}

fn made(ctx: &Ctx, task: Uuid, f: impl FnOnce(&mut Made)) {
    f(ctx.cloned.lock().unwrap().entry(task).or_default());
}

/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
    // 이름은 ASCII다. 자른 끝이 `-`면 이름 규칙에 어긋나므로 뗀다.
    let cut = |max: usize| {
        base[..base.len().min(max)]
            .trim_end_matches('-')
            .to_string()
    };
    let first = cut(MAX_NAME);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            format!("{}{suffix}", cut(MAX_NAME - suffix.len()))
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

fn pool_by_name(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    ctx.cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))
}

/// 원본(스냅샷이나 꺼진 VM의 지금 상태)의 디스크를 이 노드에서 새 볼륨으로 복사하고 UEFI 변수를
/// 복사한 뒤 새 VM을 만든다. 새 VM은 꺼져 있고 노드·예약·스냅샷이 없으며 MAC은 새로 받는다.
pub(super) async fn clone(
    ctx: &Ctx,
    task: &Task,
    name: &str,
    snapshot: Option<Uuid>,
    pool: Option<&str>,
) -> Result<(), String> {
    let vm = get_vm(ctx, task.spec.target)?;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    let (spec, tag) = match snapshot {
        Some(id) => {
            let snapshot = get_snapshot(ctx, id)?
                .filter(|s| s.spec.vm == vm.id)
                .ok_or("snapshot was deleted")?;
            if snapshot.status.phase != SnapshotPhase::Ready {
                return Err("snapshot is not ready".into());
            }
            check_settled(&vm)?;
            (snapshot.spec.vm_spec, Some(id.to_string()))
        }
        None if !stopped => return Err("VM is running; stop it or clone from a snapshot".into()),
        None => (vm.spec.clone(), None),
    };
    // 실행 중인 VM의 디스크는 QEMU가 쥐고 있다. 스냅샷 데이터는 바뀌지 않으므로 잠금을 무시하고 읽는다.
    let force_share = tag.is_some() && !stopped;
    if ctx
        .cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {name:?} already exists"));
    }
    let disks = ready_disks(ctx, &spec)?;
    let mut targets = Vec::with_capacity(disks.len());
    for (volume, source_pool) in &disks {
        check_local(ctx, volume, source_pool)?;
        let target = match pool {
            Some(p) => pool_by_name(ctx, p)?,
            None => source_pool.clone(),
        };
        if !target.spec.nodes.is_empty() && !target.spec.nodes.contains(&ctx.config.node) {
            return Err(format!(
                "storage pool {:?} is not available on this node",
                target.name
            ));
        }
        targets.push(target);
    }

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<(Volume, &StoragePool)> = Vec::with_capacity(disks.len());
    for ((source, source_pool), target) in disks.iter().zip(&targets) {
        let copy_name = volume_name(name, &source.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: source.spec.size_bytes,
                format: source.spec.format,
            },
        );
        copy.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(copy.id));
        ctx.cluster
            .create(&copy)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", copy.name))?;
        ctx.storage
            .copy_volume(
                &source_pool.spec,
                source.id,
                source.spec.format,
                tag.as_deref(),
                force_share,
                &target.spec,
                copy.id,
            )
            .await
            .map_err(|e| format!("cannot copy volume {:?}: {e}", source.name))?;
        copies.push((copy, target));
    }

    let mut new: Vm = Resource::new(name, spec);
    if let Some(dir) = &ctx.config.state_dir {
        let from = match snapshot {
            Some(id) => vars_copy(ctx, vm.id, id).expect("state_dir is set"),
            None => uefi_vars_path(dir, vm.id),
        };
        // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 없다.
        if tokio::fs::try_exists(&from).await.unwrap_or(false) {
            let to = uefi_vars_path(dir, new.id);
            made(ctx, task.id, |m| m.vars = Some(to.clone()));
            copy_file(&from, &to)
                .await
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        }
    }
    for (copy, target) in &copies {
        mark_ready(ctx, copy, &target.spec).await;
    }

    for (disk, (copy, _)) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let mut used: BTreeSet<MacAddr> = vms
        .iter()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in &mut new.spec.nics {
        let mac = std::iter::repeat_with(MacAddr::generate)
            .find(|m| !used.contains(m))
            .expect("random MACs eventually differ");
        used.insert(mac);
        nic.mac = Some(mac);
    }
    new.spec.node = None;
    new.spec.power = PowerState::Stopped;
    new.spec.snapshot_schedule = None;
    // 이름이나 MAC이 그새 쓰였으면 저장소가 거부한다.
    ctx.cluster
        .create(&new)
        .await
        .map_err(|e| format!("cannot create VM {name:?}: {e}"))?;
    ctx.cloned.lock().unwrap().remove(&task.id);
    Ok(())
}

/// 실패한 복제가 만든 볼륨(파일과 리소스)과 UEFI 변수 사본을 지운다.
pub(super) async fn discard(ctx: &Ctx, task: Uuid) {
    let Some(made) = ctx.cloned.lock().unwrap().remove(&task) else {
        return;
    };
    for id in made.volumes {
        if let Err(e) = remove_volume(ctx, id).await {
            tracing::warn!(volume = %id, error = %e, "cannot clean up volume of failed clone");
        }
    }
    if let Some(vars) = made.vars {
        let _ = tokio::fs::remove_file(&vars).await;
        // 새 VM의 상태 디렉터리. 비어 있을 때만 지워진다.
        if let Some(dir) = vars.parent() {
            let _ = tokio::fs::remove_dir(dir).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_name_joins_cuts_and_numbers() {
        let none = |_: &str| false;
        assert_eq!(volume_name("web-2", "root", none), "web-2-root");

        let taken = |n: &str| ["web-2-root", "web-2-root-2"].contains(&n);
        assert_eq!(volume_name("web-2", "root", taken), "web-2-root-3");

        // 63자로 자르고, 자른 끝의 `-`는 뗀다.
        let vm = "v".repeat(40);
        let volume = format!("{}-x", "d".repeat(21));
        let name = volume_name(&vm, &volume, none);
        assert_eq!(name, format!("{vm}-{}", "d".repeat(21)));
        assert_eq!(name.len(), 62);

        // 번호를 붙여도 63자를 넘지 않는다.
        let long = "a".repeat(63);
        let name = volume_name(&long, "root", |n| n.len() == 63 && !n.ends_with("-2"));
        assert_eq!(name, format!("{}-2", "a".repeat(61)));
    }
}
```

`crates/opl-api/src/tasks/snapshot.rs` 전체:

```rust
//! 스냅샷 Task: 만들기·되돌리기·삭제(설계 §5). 같은 VM의 스냅샷 Task는 실행기가 차례로 돌린다.
//! API가 확인한 뒤 Task가 줄 서 기다리는 사이 VM이 바뀌었을 수 있으므로 여기서 다시 확인한다.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{DriverError, uefi_vars_path, vm_state_dir};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use super::{Ctx, set_power, set_volume_size, started, stopped, superseded, wait_vm};

pub(super) fn get_snapshot(ctx: &Ctx, id: Uuid) -> Result<Option<Snapshot>, String> {
    ctx.cluster
        .get::<SnapshotSpec>(id)
        .map_err(|e| e.to_string())
}

pub(super) fn get_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
    ctx.cluster
        .get::<VmSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "VM was deleted".into())
}

/// 이름으로 찾은 볼륨과 그 풀. 볼륨이 없으면 `None`.
fn volume_by_name(ctx: &Ctx, name: &str) -> Result<Option<(Volume, StoragePool)>, String> {
    let Some(volume) = ctx
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok(Some((volume, pool)))
}

/// VM 스펙의 디스크 볼륨들. 모두 있고 다 만들어졌어야 한다.
pub(super) fn ready_disks(ctx: &Ctx, spec: &VmSpec) -> Result<Vec<(Volume, StoragePool)>, String> {
    let mut disks = Vec::with_capacity(spec.disks.len());
    for disk in &spec.disks {
        let (volume, pool) = volume_by_name(ctx, &disk.volume)?
            .ok_or_else(|| format!("volume {:?} not found", disk.volume))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(format!("volume {:?} is not ready", volume.name));
        }
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다.
pub(super) fn check_settled(vm: &Vm) -> Result<(), String> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(format!("VM is {doing}; try again when it has settled"))
}

/// 스냅샷의 UEFI 변수 사본 `{state_dir}/vm/{vm}/snapshots/{snapshot}.vars`
pub(super) fn vars_copy(ctx: &Ctx, vm: Uuid, snapshot: Uuid) -> Option<PathBuf> {
    let dir = ctx.config.state_dir.as_ref()?;
    Some(
        vm_state_dir(dir, vm)
            .join("snapshots")
            .join(format!("{snapshot}.vars")),
    )
}

/// `from`을 `to`로 복사한다. 디렉터리가 없으면 만든다.
pub(super) async fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::copy(from, to).await.map(|_| ())
}

/// 이 노드에서 파일이 보이는 볼륨인지. 다른 노드에 만든 볼륨은 풀이 공유되지 않으면 오류다.
pub(super) fn check_local(ctx: &Ctx, volume: &Volume, pool: &StoragePool) -> Result<(), String> {
    match volume.status.node {
        Some(node) if node != ctx.config.node && !pool.spec.shared => {
            Err(format!("volume {:?} is on node {node}", volume.name))
        }
        _ => Ok(()),
    }
}

/// 디스크의 태그를 지운다. 실행 중이면 QEMU가 디스크를 쥐고 있으므로 하이퍼바이저로, 아니면
/// `spec`의 디스크마다 스토리지로 지운다. 태그나 볼륨이 없는 디스크는 그냥 넘어가고, 한 디스크가
/// 실패해도 나머지를 지운 뒤 첫 오류를 돌려준다. 다른 노드에 있는 볼륨은 이 노드에서 파일이 보이지
/// 않으므로(없는 것으로 보면 태그가 남는다) 오류다.
async fn remove_tags(
    ctx: &Ctx,
    vm: Uuid,
    running: bool,
    spec: &VmSpec,
    tag: &str,
) -> Result<(), String> {
    if running {
        return ctx
            .hv
            .snapshot_delete(vm, tag)
            .await
            .map_err(|e| e.to_string());
    }
    let mut result = Ok(());
    for disk in &spec.disks {
        let Some((volume, pool)) = volume_by_name(ctx, &disk.volume)? else {
            continue;
        };
        if let Err(e) = check_local(ctx, &volume, &pool) {
            if result.is_ok() {
                result = Err(e);
            }
            continue;
        }
        match ctx
            .storage
            .snapshot_delete(&pool.spec, volume.id, volume.spec.format, tag)
            .await
        {
            // 이 노드의 볼륨인데 파일이 없으면 태그도 없다.
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => {
                if result.is_ok() {
                    result = Err(format!("volume {:?}: {e}", volume.name));
                }
            }
        }
    }
    result
}

/// VM의 현재 위치(`status.snapshot`)를 `to(지금 위치)`로 바꾼다. reconciler도 status를 쓰므로
/// revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn set_position(
    ctx: &Ctx,
    vm: Uuid,
    to: impl Fn(Option<Uuid>) -> Option<Uuid>,
) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        let snapshot = to(current.status.snapshot);
        if snapshot == current.status.snapshot {
            return Ok(());
        }
        let status = VmStatus {
            snapshot,
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `pending` 스냅샷을 채운다. 실행 중이면 하이퍼바이저로(메모리 포함 또는 디스크만), 정지했으면
/// 디스크마다 스토리지로 태그를 만들고, UEFI 변수를 복사한 뒤 `ready`로 바꾸고 VM의 현재 위치로
/// 삼는다. 실패를 확인했으면 만든 태그를 지운다(리소스는 실행기의 정리가 지운다). 응답을 기다리다
/// 시간이 다 됐으면 작업이 아직 돌 수 있으므로 태그를 두고 실패한다.
pub(super) async fn create(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    let vm = get_vm(ctx, vm_id)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    if snapshot.spec.memory && !running {
        return Err("VM is no longer running; its memory cannot be saved".into());
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    if let Some((volume, _)) = disks
        .iter()
        .find(|(v, _)| !ctx.storage.supports_snapshots(v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted",
            volume.name, volume.spec.format
        ));
    }
    if snapshot.spec.memory && disks.is_empty() {
        return Err("VM has no disk to hold the memory state".into());
    }

    // 부모와 VM 스펙은 실행하는 지금의 것이다. 앞에 줄 선 스냅샷 Task가 현재 위치를 바꿨을 수 있다.
    let mut spec = snapshot.spec.clone();
    spec.parent = vm.status.snapshot;
    spec.vm_spec = vm.spec.clone();
    if spec != snapshot.spec {
        ctx.cluster
            .update_spec(id, Some(snapshot.revision), snapshot.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    let tag = id.to_string();
    let made = if !running {
        let mut result = Ok(());
        for (volume, pool) in &disks {
            result = ctx
                .storage
                .snapshot_create(&pool.spec, volume.id, volume.spec.format, &tag)
                .await;
            if result.is_err() {
                break;
            }
        }
        result
    } else if disks.is_empty() {
        // 디스크 없는 VM은 설정만 담는다.
        Ok(())
    } else {
        ctx.hv.snapshot_save(vm_id, &tag, spec.memory).await
    };
    let copied = match (&made, ctx.config.state_dir.as_ref()) {
        (Ok(()), Some(dir)) => {
            let vars = uefi_vars_path(dir, vm_id);
            match tokio::fs::try_exists(&vars).await {
                // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM
                Ok(false) => Ok(()),
                Ok(true) => copy_file(&vars, &vars_copy(ctx, vm_id, id).unwrap()).await,
                Err(e) => Err(e),
            }
        }
        _ => Ok(()),
    };
    let failure = match (made, copied) {
        (Err(e), _) if e.may_be_in_progress() => {
            return Err(format!(
                "{e}; the snapshot may still be written to the disks and its tag {tag} is left there"
            ));
        }
        (Err(e), _) => Some(e.to_string()),
        (Ok(()), Err(e)) => Some(format!("cannot copy UEFI variables: {e}")),
        (Ok(()), Ok(())) => None,
    };
    if let Some(error) = failure {
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
            tracing::warn!(snapshot = %id, error = %e, "cannot remove tags of failed snapshot");
        }
        if let Some(copy) = vars_copy(ctx, vm_id, id) {
            let _ = tokio::fs::remove_file(copy).await;
        }
        return Err(error);
    }

    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(now_secs()),
        vmstate_bytes: None,
    };
    // 다 만든 스냅샷이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
    loop {
        match ctx.cluster.update_status::<SnapshotSpec>(id, &status).await {
            Ok(_) => break,
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(snapshot = %id, error = %e, "cannot mark snapshot ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    set_position(ctx, vm_id, |_| Some(id)).await
}

/// 실패한 만들기의 `pending` 리소스를 지운다. 이미 `ready`면 그대로 둔다.
pub(super) async fn discard_pending(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    match get_snapshot(ctx, id)? {
        Some(s) if s.status.phase == SnapshotPhase::Pending => delete_resource(ctx, &s).await,
        _ => Ok(()),
    }
}

async fn delete_resource(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    match ctx.cluster.delete::<SnapshotSpec>(snapshot.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이전 opld가 끝내지 못한 `pending` 스냅샷의 태그(있으면)와 리소스를 지운다. 태그를 지우지
/// 못해도 리소스는 지운다(남은 태그는 공간만 쓴다).
pub(super) async fn discard(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    let vm = snapshot.spec.vm;
    let running = ctx
        .hv
        .inspect(vm)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
    let tag = snapshot.id.to_string();
    if let Err(e) = remove_tags(ctx, vm, running, &snapshot.spec.vm_spec, &tag).await {
        tracing::warn!(snapshot = %snapshot.id, error = %e, "cannot remove tags of unfinished snapshot");
    }
    if let Some(copy) = vars_copy(ctx, vm, snapshot.id) {
        let _ = tokio::fs::remove_file(copy).await;
    }
    delete_resource(ctx, snapshot).await
}

/// VM을 스냅샷 때로 되돌린다(설계 §5): 강제로 끄고, 스펙을 스냅샷의 것으로(노드는 그대로) 바꾸고,
/// 디스크와 UEFI 변수를 되돌리고, 볼륨의 실제 크기를 다시 읽는다. 현재 위치를 이 스냅샷으로
/// 바꾼 뒤, 메모리 스냅샷이면 그 메모리를 불러오며 시작한다.
pub(super) async fn revert(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    let vm_id = task.spec.target;
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err("snapshot is not ready".into());
    }
    // 되돌릴 수 없는데 VM만 꺼지지 않도록 끄기 전에 볼륨을 확인한다: 이 노드에서 보이는지, API가
    // 확인한 뒤 다른 VM이 붙이지 않았는지.
    let disks = ready_disks(ctx, &snapshot.spec.vm_spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    for (volume, _) in &disks {
        if let Some(other) = vms
            .iter()
            .find(|vm| vm.id != vm_id && vm.spec.disks.iter().any(|d| d.volume == volume.name))
        {
            return Err(format!(
                "volume {:?} is attached to VM {:?}",
                volume.name, other.name
            ));
        }
    }
    let tag = id.to_string();

    let vm = get_vm(ctx, vm_id)?;
    if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
        set_power(ctx, vm_id, PowerState::Stopped).await?;
        ctx.reconciler.force_stop(vm_id);
        wait_vm(ctx, vm_id, stopped).await?;
    }
    write_reverted_spec(ctx, vm_id, &snapshot).await?;

    // 여기서 멈추면 VM은 꺼진 채 남는다. 같은 되돌리기를 다시 하면 처음부터 한다(설계 §6).
    for (volume, pool) in &disks {
        // API는 이 Task가 도는 동안 전원 조작을 막지만, 그 확인과 겹친 요청이 켰을 수 있다.
        // QEMU가 연 디스크를 qemu-img로 바꾸면 안 된다.
        if get_vm(ctx, vm_id)?.spec.power != PowerState::Stopped {
            return Err("VM was started again".into());
        }
        ctx.storage
            .snapshot_revert(&pool.spec, volume.id, volume.spec.format, &tag)
            .await
            .map_err(|e| format!("cannot revert volume {:?}: {e}", volume.name))?;
    }
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir)
        && tokio::fs::try_exists(&copy).await.unwrap_or(false)
    {
        copy_file(&copy, &uefi_vars_path(dir, vm_id))
            .await
            .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
    }
    // 스냅샷 뒤에 늘린 크기는 되돌아간다.
    for (volume, pool) in &disks {
        record_size(ctx, volume, pool).await;
    }
    // 디스크가 스냅샷 때로 돌아갔으므로 메모리를 불러오지 못하더라도 현재 위치는 이 스냅샷이다.
    set_position(ctx, vm_id, |_| Some(id)).await?;

    if snapshot.spec.memory {
        // 뒤에 온 전원 요청(정지 등)이 있으면 그것을 따르고 켜지 않는다. 메모리를 불러오며 켜면
        // 그 뒤의 정지를 덮는다.
        if superseded(ctx, task)? {
            return Err("superseded by a later request for this VM".into());
        }
        // power를 쓰기 전에 맡겨야 다음 시작이 받는다. 쓰지 못하면 다른 시작이 쓰지 않게 거둔다.
        ctx.reconciler.restore_on_start(vm_id, tag);
        if let Err(e) = set_power(ctx, vm_id, PowerState::Running).await {
            ctx.reconciler.clear_restore(vm_id);
            return Err(e);
        }
        wait_vm(ctx, vm_id, started).await?;
    }
    Ok(())
}

pub(super) fn is_down(vm: &Vm) -> bool {
    matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드는 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
/// 되돌리고 시작하므로 배치되지 않은 VM은 이 노드로 정한다. 끈 뒤 누가 다시 켰으면 디스크를
/// 건드리기 전에 그만둔다.
async fn write_reverted_spec(ctx: &Ctx, vm_id: Uuid, snapshot: &Snapshot) -> Result<(), String> {
    loop {
        let vm = get_vm(ctx, vm_id)?;
        if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
            return Err("VM was started again".into());
        }
        let mut spec = snapshot.spec.vm_spec.clone();
        spec.node = vm.spec.node;
        if snapshot.spec.memory && spec.node.is_none() {
            spec.node = Some(ctx.config.node);
        }
        spec.power = PowerState::Stopped;
        match ctx
            .cluster
            .update_spec(vm_id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 볼륨의 실제 크기를 읽어 스펙에 반영한다. 실패해도 넘어간다(크기 표시만 틀린다).
async fn record_size(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(&pool.spec, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            return;
        }
    };
    if let Err(e) = set_volume_size(ctx, volume.id, info.virtual_bytes).await {
        tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
    }
}

/// 스냅샷을 지운다: 디스크의 태그(실행 중이면 하이퍼바이저로, 그 뒤 떼어 낸 디스크는 스토리지로),
/// UEFI 변수 사본, 리소스. 자식은 지운 스냅샷의 부모로 옮겨 붙고, 현재 위치였으면 부모로 옮긴다.
/// 태그가 이미 없으면 성공으로 보므로 중간에 멈춰도 다시 하면 된다.
pub(super) async fn delete(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let Some(snapshot) = get_snapshot(ctx, id)? else {
        return Ok(());
    };
    let vm = ctx
        .cluster
        .get::<VmSpec>(vm_id)
        .map_err(|e| e.to_string())?;
    if let Some(vm) = &vm {
        check_settled(vm)?;
    }
    let tag = id.to_string();
    let running = vm.as_ref().filter(|vm| vm.status.phase == VmPhase::Running);
    let mut detached = snapshot.spec.vm_spec.clone();
    if let Some(vm) = running {
        remove_tags(ctx, vm_id, true, &vm.spec, &tag).await?;
        // 실행 중인 VM이 쓰는 디스크(실행 중에는 바뀌지 않는다)는 위에서 지웠다.
        detached
            .disks
            .retain(|d| !vm.spec.disks.iter().any(|a| a.volume == d.volume));
    }
    remove_tags(ctx, vm_id, false, &detached, &tag).await?;
    if let Some(copy) = vars_copy(ctx, vm_id, id) {
        match tokio::fs::remove_file(copy).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove UEFI variables copy: {e}")),
        }
    }

    let (_, all) = ctx
        .cluster
        .list::<SnapshotSpec>()
        .map_err(|e| e.to_string())?;
    for child in all.iter().filter(|s| s.spec.parent == Some(id)) {
        let mut spec = child.spec.clone();
        spec.parent = snapshot.spec.parent;
        match ctx
            .cluster
            .update_spec(child.id, None, child.labels.clone(), &spec)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if vm.is_some() {
        let parent = snapshot.spec.parent;
        set_position(ctx, vm_id, |at| if at == Some(id) { parent } else { at }).await?;
    }
    delete_resource(ctx, &snapshot).await
}
```

`crates/opl-api/src/vms.rs`에서

```rust

/// 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`.
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
```

을 다음으로 바꾼다:

```rust

/// 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`, 끝나지 않은 스냅샷·복제
/// Task가 있으면 409 `vm_busy`(복제는 원본 VM과 그 스냅샷을 읽는다).
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
```

`crates/opl-api/src/vms.rs`에서

```rust
    }
    state
```

을 다음으로 바꾼다:

```rust
    }
    check_no_snapshot_task(&state, &vm)?;
    state
```

`crates/opl-api/tests/clones.rs`에서

```rust
//! VM 복제 API: 검사, Task 만들기, 복제 중 원본 VM 막기.
//! Task 실행기를 멈춰 두고 API가 남긴 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
//! VM 복제: API의 검사, Task 만들기, 복제 중 원본 VM 막기(실행기를 멈춰 두고 Task의 모양만 본다)와
//! 파일 없는 스토리지로 돌리는 복제 Task 실행(새 VM과 볼륨, 실패 정리, 원본 VM의 줄).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
```

`crates/opl-api/tests/clones.rs`에서

```rust
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
```

`crates/opl-api/tests/clones.rs`에서

```rust
    let (status, body) = clone(&operator, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
```

을 다음으로 바꾼다:

```rust
    let (status, body) = clone(&operator, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn unfinished_clone_blocks_deleting_the_source() {
    let env = env().await;
    let admin = env.admin().await;
    admin.post("/vms", vm_request("web")).await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");

    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    finish_tasks(&env).await;
    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// 파일 없는 스토리지로 실행기를 다시 띄운다. `volumes`는 원래 크기로 알린다(나머지는 파일이 없다).
async fn fake_storage(env: &mut Env, volumes: &[Uuid]) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    for id in volumes {
        storage.set_size(*id, MIB);
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<opl_types::storage::Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

fn state_dir(env: &Env) -> std::path::PathBuf {
    env.dir.path().join("state")
}

/// VM 상태 디렉터리가 있는 VM들
fn vm_dirs(env: &Env) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(state_dir(env).join("vm"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// 이름이 `prefix`로 시작하는 볼륨들
fn volumes_named(env: &Env, prefix: &str) -> Vec<String> {
    let (_, volumes) = env.cluster.list::<VolumeSpec>().unwrap();
    volumes
        .into_iter()
        .map(|v| v.name)
        .filter(|n| n.starts_with(prefix))
        .collect()
}

async fn clone_and_wait(admin: &Client, vm: &str, body: Value) -> Value {
    let (status, task) = clone(admin, vm, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

fn failed_with(task: &Value, error: &str) {
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let got = task["status"]["error"].as_str().unwrap();
    assert!(got.contains(error), "{task}");
}

#[tokio::test]
async fn clone_copies_disks_into_a_new_stopped_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "fast", vec![env.node]).await;
    let root = volume(&env, "root").await;
    let data = volume(&env, "data").await;
    let storage = fake_storage(&mut env, &[root, data]).await;
    let id = vm_with_disks(&admin, "web", &["root", "data"]).await;
    // 원본은 노드에 배치됐고 예약이 있다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = json!(env.node);
    spec["snapshot_schedule"] = json!({ "every": "daily", "keep": 3 });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let vars = uefi_vars_path(&state_dir(&env), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-1").unwrap();

    succeeded(&clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await);
    let source = get_vm(&env, "web").unwrap();
    let copy = get_vm(&env, "web-2").unwrap();
    let disks: Vec<&str> = copy.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-2-root", "web-2-data"]);
    assert_eq!(copy.spec.disks[0].bus, source.spec.disks[0].bus);
    assert_eq!(copy.spec.power, PowerState::Stopped);
    assert_eq!(copy.spec.node, None);
    assert_eq!(copy.spec.snapshot_schedule, None);
    assert_eq!(copy.spec.memory_mib, source.spec.memory_mib);
    assert!(copy.spec.nics[0].mac.is_some());
    assert_ne!(copy.spec.nics[0].mac, source.spec.nics[0].mac);
    assert_eq!(copy.status.snapshot, None);
    let mut copied = Vec::new();
    for name in disks {
        let v = get_volume(&env, name).unwrap();
        assert_eq!(v.spec.pool, "local");
        assert_eq!(v.spec.format, ImageFormat::Qcow2);
        assert_eq!(v.spec.size_bytes, MIB);
        assert_eq!(v.status.phase, VolumePhase::Ready);
        assert_eq!(v.status.node, Some(env.node));
        copied.push(("copy", v.id));
    }
    assert_eq!(storage.calls(), copied);
    assert_eq!(
        std::fs::read(uefi_vars_path(&state_dir(&env), copy.id)).unwrap(),
        b"vars-1"
    );

    // 스냅샷에서, 다른 풀로. 이미 있는 이름은 피하고, 변수는 스냅샷 때의 것이다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    succeeded(&admin.wait_task(&body).await);
    std::fs::write(&vars, b"vars-2").unwrap();
    volume(&env, "web-3-root").await;
    succeeded(
        &clone_and_wait(
            &admin,
            "web",
            json!({ "name": "web-3", "snapshot": "s1", "pool": "fast" }),
        )
        .await,
    );
    let copy = get_vm(&env, "web-3").unwrap();
    let disks: Vec<&str> = copy.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-3-root-2", "web-3-data"]);
    for name in disks {
        assert_eq!(get_volume(&env, name).unwrap().spec.pool, "fast");
    }
    assert_eq!(
        std::fs::read(uefi_vars_path(&state_dir(&env), copy.id)).unwrap(),
        b"vars-1"
    );
}

#[tokio::test]
async fn failed_copy_removes_the_volumes_already_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    volume(&env, "data").await;
    // `data`의 파일이 없어 두 번째 복사가 실패한다.
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root", "data"]).await;

    let task = clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await;
    failed_with(&task, "data");
    assert_eq!(storage.calls().len(), 2);
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn name_taken_during_the_copy_removes_volumes_and_uefi_vars() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    let id = vm_with_disks(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&state_dir(&env), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars").unwrap();

    storage.pause();
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    let (status, body) = admin.post("/vms", vm_request("web-2")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    storage.resume();

    let task = admin.wait_task(&task).await;
    failed_with(&task, "web-2");
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").unwrap().spec.disks.is_empty());
    assert_eq!(vm_dirs(&env), [id.to_string()]);
}

#[tokio::test]
async fn timed_out_clone_removes_the_volumes_it_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(root, MIB);
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.timeout = Duration::from_millis(300);
    })
    .await;
    vm_with_disks(&admin, "web", &["root"]).await;

    // 복사가 끝나지 않는다.
    storage.pause();
    let task = clone_and_wait(&admin, "web", json!({ "name": "web-2" })).await;
    failed_with(&task, "timed out");
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn clone_fails_cleanly_when_the_source_is_gone() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    let id = vm_with_disks(&admin, "web", &["root"]).await;
    let (_, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    succeeded(&admin.wait_task(&body).await);

    // 스냅샷 삭제가 먼저 줄 섰다. 복제는 그 뒤에 돌아 스냅샷이 없는 것을 본다.
    env.stop_runner().await;
    let (status, delete) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{delete}");
    let (status, from_snapshot) =
        clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{from_snapshot}");
    env.start_runner_with(storage.clone()).await;
    succeeded(&admin.wait_task(&delete).await);
    failed_with(
        &admin.wait_task(&from_snapshot).await,
        "snapshot was deleted",
    );

    // API를 거치지 않고 원본 VM이 지워졌다.
    env.stop_runner().await;
    let (status, from_vm) = clone(&admin, "web", json!({ "name": "web-2" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{from_vm}");
    env.cluster.delete::<VmSpec>(id, None).await.unwrap();
    env.start_runner_with(storage.clone()).await;
    failed_with(&admin.wait_task(&from_vm).await, "VM was deleted");

    assert!(!storage.calls().iter().any(|(call, _)| *call == "copy"));
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn snapshot_delete_waits_for_a_clone_of_the_same_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let (_, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    succeeded(&admin.wait_task(&body).await);

    // 복사하는 동안 그 스냅샷을 지우라는 요청이 온다. 복제가 끝난 뒤에 지운다.
    storage.pause();
    let (status, copy) = clone(&admin, "web", json!({ "name": "web-2", "snapshot": "s1" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{copy}");
    storage.reached().await;
    let (status, delete) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{delete}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let path = format!("/tasks/{}", delete["id"].as_str().unwrap());
    let (_, current) = admin.get(&path).await;
    assert_eq!(current["status"]["state"], "pending", "{current}");

    storage.resume();
    succeeded(&admin.wait_task(&copy).await);
    succeeded(&admin.wait_task(&delete).await);
    let calls: Vec<&str> = storage.calls().into_iter().map(|(c, _)| c).collect();
    assert_eq!(calls, ["create", "copy", "delete"]);
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    pub async fn start_runner_with(&mut self, storage: Arc<dyn Storage>) {
        self.runner = Some(
```

을 다음으로 바꾼다:

```rust
    pub async fn start_runner_with(&mut self, storage: Arc<dyn Storage>) {
        self.start_runner_config(storage, |_| {}).await;
    }

    /// `storage`를 쓰고 `tweak`으로 설정을 바꾼 Task 실행기를 띄운다.
    pub async fn start_runner_config(
        &mut self,
        storage: Arc<dyn Storage>,
        tweak: impl FnOnce(&mut TaskRunnerConfig),
    ) {
        let mut config = runner_config(self.node, self.dir.path());
        tweak(&mut config);
        self.runner = Some(
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
                self.reconciler.as_ref().unwrap().handle(),
                runner_config(self.node, self.dir.path()),
            )
```

을 다음으로 바꾼다:

```rust
                self.reconciler.as_ref().unwrap().handle(),
                config,
            )
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. 볼륨 크기 늘리기와 그 차례. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
```

을 다음으로 바꾼다:

```rust
//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. 볼륨 크기 늘리기와 그 차례. 스냅샷·꺼진 VM에서 복제.
//! qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
}

/// 복제한 VM의 첫 디스크 파일
fn cloned_disk(env: &Env, vm: &str) -> PathBuf {
    let vm = env.cluster.get_by_name::<VmSpec>(vm).unwrap().unwrap();
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>(&vm.spec.disks[0].volume)
        .unwrap()
        .unwrap();
    assert_eq!(volume.status.phase, VolumePhase::Ready);
    pool_dir(env).join(format!("{}.qcow2", volume.id))
}

#[tokio::test]
async fn clone_copies_the_snapshot_or_the_current_disk() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    create_vm(&admin, "web", &["root"]).await;
    assert!(qemu_io(&file, "write -P 0xaa 0 64k"));
    succeeded(
        &wait(
            &admin,
            admin
                .post("/vms/web/snapshots", json!({ "name": "s1" }))
                .await,
        )
        .await,
    );
    assert!(qemu_io(&file, "write -P 0xbb 0 64k"));

    let clone = |body: Value| {
        let admin = admin.clone();
        async move { wait(&admin, admin.post("/vms/web/clone", body).await).await }
    };
    succeeded(&clone(json!({ "name": "from-s1", "snapshot": "s1" })).await);
    succeeded(&clone(json!({ "name": "now" })).await);
    let from_s1 = cloned_disk(&env, "from-s1");
    assert!(qemu_io(&from_s1, "read -P 0xaa 0 64k"));
    assert!(tags(&from_s1).is_empty());
    assert!(qemu_io(&cloned_disk(&env, "now"), "read -P 0xbb 0 64k"));

    // 실행 중인 VM의 스냅샷에서. Mock 하이퍼바이저는 파일에 쓰지 않으므로 QEMU가 만들었을 태그를
    // 대신 만든다.
    succeeded(&admin.act("web", "start").await);
    succeeded(
        &wait(
            &admin,
            admin
                .post("/vms/web/snapshots", json!({ "name": "mem" }))
                .await,
        )
        .await,
    );
    let id = env
        .cluster
        .get_by_name::<VmSpec>("web")
        .unwrap()
        .unwrap()
        .id;
    let mem = get(&env, id, "mem").unwrap();
    qemu_img(&["snapshot", "-c", &mem.id.to_string(), path_str(&file)]);
    assert!(qemu_io(&file, "write -P 0xcc 0 64k"));
    succeeded(&clone(json!({ "name": "from-mem", "snapshot": "mem" })).await);
    assert!(qemu_io(
        &cloned_disk(&env, "from-mem"),
        "read -P 0xbb 0 64k"
    ));
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 318개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test snapshot_disks`
Expected: PASS 8개

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 200개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): VM 복제 Task 실행"
```

---

### Task 5: 예약 스냅샷 반복 작업과 예약 오류 기록

스냅샷 만들기·삭제 핸들러의 본문을 `start_create`·`start_delete`로 꺼내 예약 작업과 함께 쓴다(이름·설명 검사는 핸들러에 남겨 오류 순서를 지킨다). `opl-api/src/schedule.rs`의 `SnapshotScheduler`는 리더에서 `tick`마다 돌고, 순수 함수 `plan(now, hour, VM, 스냅샷, Task, 시도 기록)`이 건너뛰기·보관 삭제(가장 오래된 예약 스냅샷 하나)·만들기(`auto-YYYYMMDD-HHMM`)를 정한다. 시도 기록은 메모리에만: 거절된 만들기는 `hour` 뒤, 보낸 만들기 Task가 실패했으면 다음 주기 뒤, 실패한 보관 삭제는 그 스냅샷 삭제만 `hour` 동안 미루고 만들기는 이어 간다. 4xx 거절은 `schedule_error`에(조건부 쓰기), 5xx는 로그만. 예약 스냅샷의 만들기 Task는 끝날 때 오류를 지우거나 쓰고, 삭제 Task는 실패할 때만 쓴다. 되돌리기는 VM의 지금 예약을 지킨다. opld 설정 `[schedule] tick_secs = 60, hour_secs = 3600`(0은 거부)과 `Member`의 띄우기·멈추기. UTC 이름에 `time`을 쓴다.

**Files:**
- Create: `crates/opl-api/src/schedule.rs`
- Create: `crates/opl-api/tests/schedules.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
thiserror.workspace = true
tokio.workspace = true
```

을 다음으로 바꾼다:

```toml
thiserror.workspace = true
time.workspace = true
tokio.workspace = true
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod resources;
pub mod scheduler;
```

을 다음으로 바꾼다:

```rust
pub mod resources;
mod schedule;
pub mod scheduler;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
```

을 다음으로 바꾼다:

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
pub use state::AppState;
```

`crates/opl-api/src/schedule.rs`를 만든다:

```rust
//! 예약 스냅샷(설계 §5). 리더만 `tick`마다 예약이 있는 VM을 보고, 보관 개수를 넘은 예약 스냅샷을
//! 하나 지우거나 주기가 지났으면 새 예약 스냅샷을 요청한다. 요청은 API 핸들러와 같은 함수로 하므로
//! 같은 검사를 받는다. 거절되면 그 이유를 `status.schedule_error`에 남긴다. 만들기 Task의 결과와
//! 삭제 Task의 실패는 Task 실행기가 남긴다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{ScheduleEvery, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::ApiError;
use crate::snapshots::{start_create, start_delete, vm_placement};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct SnapshotSchedulerConfig {
    /// 예약을 확인하는 간격
    pub tick: Duration,
    /// 주기의 한 시간. 매일은 24배, 매주는 168배다. 초 단위로 센다(스냅샷의 `created_at`이 초다).
    /// 테스트만 줄인다.
    pub hour: Duration,
}

impl Default for SnapshotSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            hour: Duration::from_secs(3600),
        }
    }
}

/// 예약 스냅샷 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct SnapshotScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl SnapshotScheduler {
    pub fn spawn(state: Arc<AppState>, config: SnapshotSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: SnapshotSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    // VM별 마지막 예약 만들기·삭제 시도. 메모리에만 두므로 리더가 바뀌거나 opld가 다시 시작하면 잊고,
    // 그때는 한 번 일찍 다시 시도한다.
    let mut attempts: HashMap<Uuid, Attempts> = HashMap::new();
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut attempts) => {}
        }
    }
}

/// 예약 작업이 VM에 할 일
#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    /// 보관 개수를 넘은 가장 오래된 예약 스냅샷을 지운다.
    Delete(Uuid),
    /// 이 이름으로 예약 스냅샷을 만든다.
    Create { name: String, memory: bool },
}

/// 마지막 예약 만들기 시도와 그 시각(초)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// 요청이 4xx로 거절되었다(바쁨, 노드 오프라인 등). 잠깐일 수 있으므로 한 시간 뒤에 다시 한다.
    Refused(u64),
    /// Task를 만들었다. 실패했으면 다음 주기에 다시 한다.
    Sent(u64),
}

/// VM 하나에 예약 작업이 한 마지막 시도
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Attempts {
    create: Option<Attempt>,
    /// 지우려 한(요청했거나 거절된) 예약 스냅샷과 그 시각
    delete: Option<(Uuid, u64)>,
}

/// 주기의 길이(초)
fn period_secs(every: ScheduleEvery, hour: Duration) -> u64 {
    let hours = match every {
        ScheduleEvery::Hourly => 1,
        ScheduleEvery::Daily => 24,
        ScheduleEvery::Weekly => 168,
    };
    hour.as_secs() * hours
}

/// `auto-YYYYMMDD-HHMM`(UTC). VM에 같은 이름이 있으면 `-2`, `-3`…을 붙인다.
fn auto_name(now: u64, snapshots: &[Snapshot]) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let base = format!(
        "auto-{:04}{:02}{:02}-{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute()
    );
    let taken = |name: &str| snapshots.iter().any(|s| s.spec.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .unwrap()
}

/// VM 하나에 지금 할 일. `snapshots`는 이 VM의 스냅샷이다. 예약이 없거나, 이 VM의 스냅샷·복제
/// Task가 끝나지 않았거나, 만드는 중인 스냅샷이 있으면 아무것도 하지 않는다. 보관 개수를 넘었으면
/// 한 번에 하나씩 지우고(만들지 않는다), 아니면 마지막 예약 스냅샷에서 주기가 지났을 때 만든다.
/// 마지막 만들기 시도가 그 뒤면, 거절된 것은 한 시간 뒤에, 보낸 Task가 실패한 것은 다음 주기에
/// 다시 한다. 지우려 한 스냅샷이 아직 있으면(지우기가 실패했다) 그 삭제는 한 시간 뒤에 다시 하고,
/// 그동안은 만들기를 본다(보관 정리가 막혀도 새 스냅샷은 만든다).
fn plan(
    now: u64,
    hour: Duration,
    vm: &Vm,
    snapshots: &[Snapshot],
    tasks: &[Task],
    attempts: &Attempts,
) -> Option<Plan> {
    let schedule = vm.spec.snapshot_schedule?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::SnapshotCreate { .. }
                    | TaskOp::SnapshotRevert { .. }
                    | TaskOp::SnapshotDelete { .. }
                    | TaskOp::VmClone { .. }
            )
    });
    if busy
        || snapshots
            .iter()
            .any(|s| s.status.phase == SnapshotPhase::Pending)
    {
        return None;
    }
    let mut scheduled: Vec<&Snapshot> = snapshots
        .iter()
        .filter(|s| s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
        .collect();
    scheduled.sort_by_key(|s| s.status.created_at);
    if scheduled.len() > schedule.keep as usize {
        let oldest = scheduled[0].id;
        // 끝나지 않은 Task가 없는데 아직 있으면 지우기가 실패했거나 거절되었다.
        let failed = attempts
            .delete
            .is_some_and(|(id, at)| id == oldest && now < at + hour.as_secs());
        if !failed {
            return Some(Plan::Delete(oldest));
        }
    }
    let period = period_secs(schedule.every, hour);
    let retry = attempts.create.map(|a| match a {
        Attempt::Refused(at) => at + hour.as_secs(),
        Attempt::Sent(at) => at + period,
    });
    let due = scheduled
        .last()
        .and_then(|s| s.status.created_at)
        .map(|last| last + period)
        .max(retry);
    if due.is_some_and(|due| now < due) {
        return None;
    }
    let running = vm.status.phase == VmPhase::Running;
    Some(Plan::Create {
        name: auto_name(now, snapshots),
        memory: schedule.memory && running && !vm.spec.disks.is_empty(),
    })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn tick(
    state: &AppState,
    config: &SnapshotSchedulerConfig,
    attempts: &mut HashMap<Uuid, Attempts>,
) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        attempts.clear();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<VmSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
        ))
    })();
    let Ok((vms, snapshots, tasks)) = lists else {
        return;
    };
    attempts.retain(|id, _| vms.iter().any(|vm| vm.id == *id));
    let now = now_secs();
    for vm in vms.iter().filter(|vm| vm.spec.snapshot_schedule.is_some()) {
        let mine: Vec<Snapshot> = snapshots
            .iter()
            .filter(|s| s.spec.vm == vm.id)
            .cloned()
            .collect();
        let attempted = attempts.get(&vm.id).copied().unwrap_or_default();
        let Some(plan) = plan(now, config.hour, vm, &mine, &tasks, &attempted) else {
            continue;
        };
        let result = match check_node(state, vm) {
            Err(e) => Err(e),
            Ok(()) => match &plan {
                Plan::Delete(id) => start_delete(state, vm, &id.to_string()).await,
                Plan::Create { name, memory } => {
                    start_create(state, vm, name.clone(), String::new(), Some(*memory), true).await
                }
            },
        };
        // 요청했거나 거절된 것은 시도로 센다. 저장소 오류는 VM의 잘못이 아니므로 세지 않는다.
        let refused = result.as_ref().is_err_and(|e| e.status.is_client_error());
        if result.is_ok() || refused {
            let entry = attempts.entry(vm.id).or_default();
            match &plan {
                Plan::Delete(id) => entry.delete = Some((*id, now)),
                Plan::Create { .. } if refused => entry.create = Some(Attempt::Refused(now)),
                Plan::Create { .. } => entry.create = Some(Attempt::Sent(now)),
            }
        }
        match result {
            Ok(task) => {
                tracing::info!(vm = %vm.name, task = %task.id, ?plan, "scheduled snapshot task")
            }
            // 검사에 걸렸다. 다음 tick에 다시 본다.
            Err(e) if e.status.is_client_error() => {
                tracing::info!(vm = %vm.name, ?plan, error = %e.message, "scheduled snapshot refused");
                if let Err(w) = set_schedule_error(cluster, vm.id, Some(e.message)).await {
                    tracing::warn!(vm = %vm.name, error = %w, "cannot record schedule error");
                }
            }
            // 저장소 오류(리더를 잃는 등)는 VM의 잘못이 아니므로 남기지 않는다.
            Err(e) => {
                tracing::warn!(vm = %vm.name, ?plan, error = %e.message, "cannot request scheduled snapshot")
            }
        }
    }
}

/// Task를 실행할 노드가 오프라인이면 요청하지 않는다(Task가 그 노드가 돌아올 때까지 남는다).
fn check_node(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let Some(node) = vm_placement(&state.cluster, vm) else {
        return Ok(());
    };
    match state.cluster.get::<NodeSpec>(node)? {
        Some(n) if n.status.online => Ok(()),
        n => Err(ApiError::conflict(
            "node_offline",
            format!(
                "node {:?} holding VM {:?} is offline",
                n.map_or_else(|| node.to_string(), |n| n.name),
                vm.name
            ),
        )),
    }
}

/// VM의 `status.schedule_error`를 `error`로 바꾼다. 같으면 쓰지 않는다. reconciler·Task 실행기도
/// status를 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다. VM이 없으면 그만둔다.
pub(crate) async fn set_schedule_error(
    cluster: &Cluster,
    vm: Uuid,
    error: Option<String>,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<VmSpec>(vm)? else {
            return Ok(());
        };
        if current.status.schedule_error == error {
            return Ok(());
        }
        let status = VmStatus {
            schedule_error: error.clone(),
            ..current.status.clone()
        };
        match cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::task::{TaskState, TaskStatus};
    use opl_types::vm::{CpuSpec, DiskBus, DiskSpec, MachineType, PowerState, SnapshotSchedule};

    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    /// 2026-10-27 05:07:00 UTC
    const NOW: u64 = 1_793_077_620;

    fn vm(every: ScheduleEvery, keep: u32, memory: bool) -> Vm {
        let mut vm: Vm = Resource::new(
            "web",
            VmSpec {
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 1,
                    model: Default::default(),
                },
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![DiskSpec {
                    volume: "root".into(),
                    bus: DiskBus::VirtioBlk,
                    boot_order: None,
                }],
                nics: vec![],
                power: PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: Some(SnapshotSchedule {
                    every,
                    keep,
                    memory,
                }),
            },
        );
        vm.status.phase = VmPhase::Running;
        vm
    }

    fn snap(vm: &Vm, name: &str, scheduled: bool, created_at: Option<u64>) -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: vm.id,
                name: name.into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled,
                vm_spec: vm.spec.clone(),
            },
        );
        s.status = SnapshotStatus {
            phase: if created_at.is_some() {
                SnapshotPhase::Ready
            } else {
                SnapshotPhase::Pending
            },
            created_at,
            vmstate_bytes: None,
        };
        s
    }

    fn task(op: TaskOp, target: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target,
                node: Uuid::now_v7(),
            },
        );
        t.status = TaskStatus { state, error: None };
        t
    }

    /// 만들기를 시도한 적이 없을 때의 판단
    fn plan(
        now: u64,
        hour: Duration,
        vm: &Vm,
        snapshots: &[Snapshot],
        tasks: &[Task],
    ) -> Option<Plan> {
        super::plan(now, hour, vm, snapshots, tasks, &Attempts::default())
    }

    /// 만들기 시도 `attempt`가 있었을 때의 판단
    fn after(now: u64, vm: &Vm, snapshots: &[Snapshot], attempt: Attempt) -> Option<Plan> {
        let attempts = Attempts {
            create: Some(attempt),
            delete: None,
        };
        super::plan(now, HOUR, vm, snapshots, &[], &attempts)
    }

    fn create(name: &str, memory: bool) -> Option<Plan> {
        Some(Plan::Create {
            name: name.into(),
            memory,
        })
    }

    #[test]
    fn names_are_utc_minutes_with_a_number_when_taken() {
        let vm = vm(ScheduleEvery::Hourly, 3, false);
        assert_eq!(auto_name(NOW, &[]), "auto-20261027-0507");
        assert_eq!(auto_name(0, &[]), "auto-19700101-0000");
        let taken = [
            snap(&vm, "auto-20261027-0507", true, Some(1)),
            snap(&vm, "auto-20261027-0507-2", false, Some(2)),
        ];
        assert_eq!(auto_name(NOW, &taken), "auto-20261027-0507-3");
    }

    #[test]
    fn creates_first_and_after_each_period() {
        let hourly = vm(ScheduleEvery::Hourly, 3, false);
        // 예약 스냅샷이 없으면 바로 만든다. 사용자가 만든 스냅샷은 주기에 세지 않는다.
        let user = snap(&hourly, "mine", false, Some(NOW - 10));
        assert_eq!(
            plan(NOW, HOUR, &hourly, std::slice::from_ref(&user), &[]),
            create("auto-20261027-0507", false)
        );
        let last = snap(&hourly, "a", true, Some(NOW - 3599));
        assert_eq!(plan(NOW, HOUR, &hourly, &[last.clone(), user], &[]), None);
        assert_eq!(
            plan(NOW + 1, HOUR, &hourly, std::slice::from_ref(&last), &[]),
            create("auto-20261027-0507", false)
        );
        // 매일은 24시간, 매주는 168시간이다.
        let daily = vm(ScheduleEvery::Daily, 3, false);
        let last = snap(&daily, "a", true, Some(NOW - 24 * 3600 + 1));
        assert_eq!(
            plan(NOW, HOUR, &daily, std::slice::from_ref(&last), &[]),
            None
        );
        assert!(plan(NOW + 1, HOUR, &daily, &[last], &[]).is_some());
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        let last = snap(&weekly, "a", true, Some(NOW - 167 * 3600));
        assert_eq!(
            plan(NOW, HOUR, &weekly, std::slice::from_ref(&last), &[]),
            None
        );
        assert!(plan(NOW + 3600, HOUR, &weekly, &[last], &[]).is_some());
        // 테스트용 짧은 한 시간
        let last = snap(&hourly, "a", true, Some(NOW - 2));
        assert!(plan(NOW, Duration::from_secs(2), &hourly, &[last], &[]).is_some());
    }

    #[test]
    fn a_failed_attempt_waits_a_period() {
        let v = vm(ScheduleEvery::Hourly, 3, false);
        // 예약 스냅샷이 없어도 방금 Task를 보냈으면(실패해 스냅샷이 없다) 다음 tick에 다시 하지 않고
        // 한 주기를 기다린다.
        let attempt = Attempt::Sent(NOW);
        assert_eq!(after(NOW + 60, &v, &[], attempt), None);
        assert_eq!(after(NOW + 3599, &v, &[], attempt), None);
        assert!(after(NOW + 3600, &v, &[], attempt).is_some());
        // 마지막 예약 스냅샷 뒤에 실패한 시도가 있으면 그 시도에서 센다.
        let last = snap(&v, "a", true, Some(NOW - 3600));
        let snaps = std::slice::from_ref(&last);
        assert_eq!(after(NOW + 10, &v, snaps, attempt), None);
        assert!(after(NOW + 3600, &v, snaps, attempt).is_some());
        // 마지막 스냅샷보다 앞선 시도는 상관없다.
        for old in [Attempt::Sent(NOW - 7200), Attempt::Refused(NOW - 7200)] {
            assert_eq!(
                after(NOW + 10, &v, snaps, old),
                create("auto-20261027-0507", false)
            );
        }
        // 매주면 실패한 Task 뒤 일주일을 기다린다.
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        assert_eq!(after(NOW + 3600, &weekly, &[], attempt), None);
        assert!(after(NOW + 168 * 3600, &weekly, &[], attempt).is_some());
        // 넘친 것을 지우는 일은 시도와 상관없다.
        let keep1 = vm(ScheduleEvery::Hourly, 1, false);
        let (a, b) = (
            snap(&keep1, "a", true, Some(NOW - 10)),
            snap(&keep1, "b", true, Some(NOW - 5)),
        );
        assert_eq!(
            after(NOW, &keep1, &[a.clone(), b], attempt),
            Some(Plan::Delete(a.id))
        );
    }

    #[test]
    fn a_failed_delete_waits_an_hour_and_lets_creation_go_on() {
        let v = vm(ScheduleEvery::Hourly, 1, false);
        let (a, b) = (
            snap(&v, "a", true, Some(NOW - 2 * 3600)),
            snap(&v, "b", true, Some(NOW - 1800)),
        );
        let snaps = [a.clone(), b.clone()];
        // 방금 지우려 한 a가 (끝나지 않은 Task 없이) 아직 있다. 다시 지우지 않고, 만들 때가 아니면
        // 아무것도 하지 않는다.
        let failed = Attempts {
            create: None,
            delete: Some((a.id, NOW - 60)),
        };
        assert_eq!(super::plan(NOW, HOUR, &v, &snaps, &[], &failed), None);
        // 만들 때가 되면 만든다.
        assert_eq!(
            super::plan(NOW + 1800, HOUR, &v, &snaps, &[], &failed),
            create("auto-20261027-0537", false)
        );
        // 지우기 Task가 아직 끝나지 않았으면 실패가 아니라 기다린다.
        let t = task(
            TaskOp::SnapshotDelete { snapshot: a.id },
            v.id,
            TaskState::Running,
        );
        assert_eq!(super::plan(NOW, HOUR, &v, &snaps, &[t], &failed), None);
        // 한 시간 뒤에 다시 지운다.
        assert_eq!(
            super::plan(NOW - 60 + 3600, HOUR, &v, &snaps, &[], &failed),
            Some(Plan::Delete(a.id))
        );
        // 다른 스냅샷을 지우려 했던 것은 상관없다.
        let other = Attempts {
            create: None,
            delete: Some((b.id, NOW - 60)),
        };
        assert_eq!(
            super::plan(NOW, HOUR, &v, &snaps, &[], &other),
            Some(Plan::Delete(a.id))
        );
    }

    #[test]
    fn a_refused_attempt_waits_an_hour() {
        // 거절(바쁨, 노드 오프라인 등)은 잠깐일 수 있으므로 주기가 길어도 한 시간 뒤에 다시 한다.
        let weekly = vm(ScheduleEvery::Weekly, 3, false);
        let attempt = Attempt::Refused(NOW);
        assert_eq!(after(NOW + 60, &weekly, &[], attempt), None);
        assert_eq!(after(NOW + 3599, &weekly, &[], attempt), None);
        assert!(after(NOW + 3600, &weekly, &[], attempt).is_some());
        // 마지막 예약 스냅샷에서 주기가 지나지 않았으면 거절과 상관없이 기다린다.
        let last = snap(&weekly, "a", true, Some(NOW - 3600));
        let snaps = std::slice::from_ref(&last);
        assert_eq!(after(NOW + 7200, &weekly, snaps, attempt), None);
    }

    #[test]
    fn memory_only_when_asked_running_and_with_disks() {
        let mut v = vm(ScheduleEvery::Hourly, 3, true);
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", true)
        );
        v.status.phase = VmPhase::Stopped;
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", false)
        );
        v.status.phase = VmPhase::Running;
        v.spec.disks.clear();
        assert_eq!(
            plan(NOW, HOUR, &v, &[], &[]),
            create("auto-20261027-0507", false)
        );
        let off = vm(ScheduleEvery::Hourly, 3, false);
        assert_eq!(
            plan(NOW, HOUR, &off, &[], &[]),
            create("auto-20261027-0507", false)
        );
    }

    #[test]
    fn deletes_the_oldest_scheduled_one_at_a_time_before_creating() {
        let v = vm(ScheduleEvery::Hourly, 2, false);
        let (a, b, c) = (
            snap(&v, "a", true, Some(NOW - 3 * 3600)),
            snap(&v, "b", true, Some(NOW - 2 * 3600)),
            snap(&v, "c", true, Some(NOW - 3 * 3600 - 1)),
        );
        // 사용자가 만든 오래된 스냅샷은 세지도 지우지도 않는다.
        let user = snap(&v, "mine", false, Some(1));
        let all = [a.clone(), user.clone(), b.clone(), c.clone()];
        // 주기가 지났어도 넘친 것을 먼저 지운다.
        assert_eq!(plan(NOW, HOUR, &v, &all, &[]), Some(Plan::Delete(c.id)));
        assert_eq!(
            plan(NOW, HOUR, &v, &[a.clone(), user.clone(), b.clone()], &[]),
            create("auto-20261027-0507", false)
        );
        // keep개 이하면 지우지 않는다.
        assert_eq!(
            plan(NOW - 2 * 3600 + 10, HOUR, &v, &[a, user, b], &[]),
            None
        );
    }

    #[test]
    fn skips_without_schedule_or_with_unfinished_work() {
        let mut none = vm(ScheduleEvery::Hourly, 2, false);
        none.spec.snapshot_schedule = None;
        assert_eq!(plan(NOW, HOUR, &none, &[], &[]), None);

        let v = vm(ScheduleEvery::Hourly, 2, false);
        let pending = snap(&v, "p", false, None);
        assert_eq!(plan(NOW, HOUR, &v, &[pending], &[]), None);
        let some = Uuid::now_v7();
        for op in [
            TaskOp::SnapshotCreate { snapshot: some },
            TaskOp::SnapshotRevert { snapshot: some },
            TaskOp::SnapshotDelete { snapshot: some },
            TaskOp::VmClone {
                name: "copy".into(),
                snapshot: None,
                pool: None,
            },
        ] {
            for state in [TaskState::Pending, TaskState::Running] {
                let t = task(op.clone(), v.id, state);
                assert_eq!(plan(NOW, HOUR, &v, &[], &[t]), None, "{op:?}");
            }
            // 끝난 Task나 다른 VM의 Task는 상관없다.
            let done = task(op.clone(), v.id, TaskState::Failed);
            let other = task(op, Uuid::now_v7(), TaskState::Running);
            assert!(plan(NOW, HOUR, &v, &[], &[done, other]).is_some());
        }
        // 전원 Task는 만들기 요청의 검사(vm_busy)가 거절한다.
        let start = task(TaskOp::VmStart, v.id, TaskState::Running);
        assert!(plan(NOW, HOUR, &v, &[], &[start]).is_some());
    }
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    check_disks(&state, &vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    let memory = req.memory.unwrap_or(running && has_disk);
    if memory && !running {
```

을 다음으로 바꾼다:

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_create(&state, &vm, req.name, req.description, req.memory, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`create_snapshot`]의 본문: `pending` 스냅샷과 그것을 채울 Task를 만든다. 예약 작업도 쓴다
/// (`scheduled`). 이름과 설명은 부르는 쪽이 확인한다.
pub(crate) async fn start_create(
    state: &AppState,
    vm: &Vm,
    name: String,
    description: String,
    memory: Option<bool>,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    check_disks(state, vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    let memory = memory.unwrap_or(running && has_disk);
    if memory && !running {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
            vm: vm.id,
            name: req.name,
            parent: vm.status.snapshot,
            memory,
            description: req.description,
            scheduled: false,
            vm_spec: vm.spec.clone(),
```

을 다음으로 바꾼다:

```rust
            vm: vm.id,
            name,
            parent: vm.status.snapshot,
            memory,
            description,
            scheduled,
            vm_spec: vm.spec.clone(),
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    };
    match create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
        Err(e) => {
```

을 다음으로 바꾼다:

```rust
    };
    match create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await {
        Ok(task) => Ok(task),
        Err(e) => {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
```

을 다음으로 바꾼다:

```rust
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_delete(&state, &vm, &name).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`delete_snapshot`]의 본문: VM의 스냅샷 `key`(이름이나 id)를 지울 Task를 만든다. 예약 작업도 쓴다.
pub(crate) async fn start_delete(state: &AppState, vm: &Vm, key: &str) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    let snapshot = find_snapshot(state, vm, key)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
```

을 다음으로 바꾼다:

```rust
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await
}

/// VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
```

`crates/opl-api/src/tasks.rs`에서

```rust
    match result {
        Ok(()) => set_status(ctx, task.id, TaskState::Succeeded, None).await,
        Err(e) => fail(ctx, &task, e).await,
```

을 다음으로 바꾼다:

```rust
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e).await,
```

`crates/opl-api/src/tasks.rs`에서

```rust

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    cleanup(ctx, task).await;
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
```

을 다음으로 바꾼다:

```rust

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기·삭제였으면 VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(error.clone())).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use crate::schedule::set_schedule_error;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust

/// 실패한 만들기의 `pending` 리소스를 지운다. 이미 `ready`면 그대로 둔다.
```

을 다음으로 바꾼다:

```rust

/// 예약으로 만든 스냅샷의 만들기·삭제 Task면 그 VM
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let (TaskOp::SnapshotCreate { snapshot } | TaskOp::SnapshotDelete { snapshot }) = task.spec.op
    else {
        return None;
    };
    get_snapshot(ctx, snapshot)
        .ok()
        .flatten()
        .filter(|s| s.spec.scheduled)
        .map(|s| s.spec.vm)
}

/// 예약 스냅샷 만들기·삭제의 결과를 VM의 `schedule_error`에 남긴다. 성공(`None`)이면 지운다.
pub(super) async fn record_schedule(ctx: &Ctx, vm: Uuid, error: Option<String>) {
    if let Err(e) = set_schedule_error(&ctx.cluster, vm, error).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

/// 실패한 만들기의 `pending` 리소스를 지운다. 이미 `ready`면 그대로 둔다.
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드는 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
/// 되돌리고 시작하므로 배치되지 않은 VM은 이 노드로 정한다. 끈 뒤 누가 다시 켰으면 디스크를
```

을 다음으로 바꾼다:

```rust

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드와 예약은 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
/// 되돌리고 시작하므로 배치되지 않은 VM은 이 노드로 정한다. 끈 뒤 누가 다시 켰으면 디스크를
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        spec.node = vm.spec.node;
        if snapshot.spec.memory && spec.node.is_none() {
```

을 다음으로 바꾼다:

```rust
        spec.node = vm.spec.node;
        // 예약은 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        if snapshot.spec.memory && spec.node.is_none() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    pub dir: TempDir,
    reconciler: Option<Reconciler>,
```

을 다음으로 바꾼다:

```rust
    pub dir: TempDir,
    /// API 서버의 상태. 예약 작업처럼 상태를 받는 반복 작업을 띄울 때 쓴다.
    pub state: Arc<AppState>,
    reconciler: Option<Reconciler>,
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = opl_api::router(state);
        let server = tokio::spawn(async move {
```

을 다음으로 바꾼다:

```rust
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = opl_api::router(state.clone());
        let server = tokio::spawn(async move {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            dir,
            reconciler: Some(reconciler),
```

을 다음으로 바꾼다:

```rust
            dir,
            state,
            reconciler: Some(reconciler),
```

`crates/opl-api/tests/schedules.rs`를 만든다:

```rust
//! 예약 스냅샷: 짧은 주기의 예약 작업이 스냅샷을 만들고 보관 개수대로 지우는지, 거절된 요청과 실패한
//! 만들기 Task를 `schedule_error`에 남기고 성공하면 지우는지, 실패한 보관 삭제를 남기고 tick마다 다시
//! 하지 않는지, 되돌리기가 지금 예약을 지키는지.

mod common;

use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_api::{SnapshotScheduler, SnapshotSchedulerConfig};
use opl_driver::DriverError;
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::{ScheduleEvery, SnapshotSchedule, Vm, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 50ms마다 보고 한 시간이 1초인 예약 작업
fn fast_scheduler(env: &Env) -> SnapshotScheduler {
    SnapshotScheduler::spawn(
        env.state.clone(),
        SnapshotSchedulerConfig {
            tick: Duration::from_millis(50),
            hour: Duration::from_secs(1),
        },
    )
}

/// 디스크 `disks`를 붙이고 예약 `schedule`을 둔 VM을 만들고 켠다.
async fn running_vm(admin: &Client, name: &str, disks: &[&str], schedule: Value) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    req["spec"]["snapshot_schedule"] = schedule;
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 풀 `local`과 바로 Ready인 볼륨 `name`을 저장소에 직접 만든다(파일 없음).
async fn ready_volume(env: &Env, name: &str, format: ImageFormat) {
    if env
        .cluster
        .get_by_name::<StoragePoolSpec>("local")
        .unwrap()
        .is_none()
    {
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: env.dir.path().join("pool"),
                },
                nodes: vec![],
                shared: false,
            },
        );
        env.cluster.create(&pool).await.unwrap();
    }
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

fn snapshots(env: &Env, vm: Uuid) -> Vec<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items.into_iter().filter(|s| s.spec.vm == vm).collect()
}

fn get_vm(env: &Env, vm: Uuid) -> Vm {
    env.cluster.get::<VmSpec>(vm).unwrap().unwrap()
}

/// 조건이 맞을 때까지 기다린다.
async fn until(why: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn scheduled_snapshots_are_created_and_pruned_to_keep() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let schedule = json!({ "every": "hourly", "keep": 2, "memory": true });
    let vm = running_vm(&admin, "web", &[], schedule).await;
    // 사용자가 만든 스냅샷은 세지도 지우지도 않는다.
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "mine" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;

    let scheduler = fast_scheduler(&env);
    let mut seen = std::collections::HashSet::new();
    let deadline = Instant::now() + WAIT;
    while seen.len() < 4 {
        assert!(Instant::now() < deadline, "only {} scheduled", seen.len());
        let all = snapshots(&env, vm);
        let auto: Vec<&Snapshot> = all.iter().filter(|s| s.spec.scheduled).collect();
        // 넘친 것은 다음 tick에 지우므로 잠깐 keep+1개일 수 있다.
        assert!(auto.len() <= 3, "{auto:?}");
        assert!(
            all.iter()
                .any(|s| s.spec.name == "mine" && !s.spec.scheduled)
        );
        for s in auto {
            assert!(s.spec.name.starts_with("auto-"), "{}", s.spec.name);
            // 디스크가 없으면 메모리를 담지 않는다.
            assert!(!s.spec.memory);
            seen.insert(s.id);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    scheduler.shutdown().await;
    // 넷 이상 만들었는데 셋 이하만 남았으니 오래된 것을 지웠다.
    assert_eq!(get_vm(&env, vm).status.schedule_error, None);
}

#[tokio::test]
async fn refused_requests_and_failed_tasks_are_recorded_until_a_success() {
    let env = Env::start().await;
    let admin = env.admin().await;
    ready_volume(&env, "raw", ImageFormat::Raw).await;
    ready_volume(&env, "root", ImageFormat::Qcow2).await;
    // 실패하면 다음 주기(1초)에 다시 한다.
    let schedule = json!({ "every": "hourly", "keep": 3 });
    let raw = running_vm(&admin, "raw-vm", &["raw"], schedule.clone()).await;
    let web = running_vm(&admin, "web", &["root"], schedule).await;

    // 다음 메모리 스냅샷 작업이 실패한다. 예약은 메모리를 담지 않지만 실행 중이면 하이퍼바이저로 만든다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let scheduler = fast_scheduler(&env);

    // raw 디스크는 요청이 거절된다. 스냅샷은 생기지 않는다.
    until("raw refusal", || {
        get_vm(&env, raw)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("non-qcow2"))
    })
    .await;
    assert!(snapshots(&env, raw).is_empty());

    // 실패한 만들기 Task의 오류가 남고, 다음 주기에 다시 만들어 성공하면 지운다.
    until("task failure", || {
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("no space"))
    })
    .await;
    until("success", || {
        get_vm(&env, web).status.schedule_error.is_none()
            && snapshots(&env, web)
                .iter()
                .any(|s| s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
    })
    .await;
    scheduler.shutdown().await;
}

#[tokio::test]
async fn a_failed_delete_is_recorded_and_not_retried_every_tick() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], json!({ "every": "hourly", "keep": 1 })).await;
    // 보관 개수를 넘은 예약 스냅샷 둘을 직접 둔다. 한 시간(1분) 안이라 새로 만들지 않는다.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let spec = get_vm(&env, vm).spec;
    let mut ids = vec![];
    for (name, at) in [("old", now - 2), ("new", now - 1)] {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm,
                name: name.into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: true,
                vm_spec: spec.clone(),
            },
        );
        s.name = s.id.to_string();
        env.cluster.create(&s).await.unwrap();
        s.status.phase = SnapshotPhase::Ready;
        s.status.created_at = Some(at);
        env.cluster
            .update_status::<SnapshotSpec>(s.id, &s.status)
            .await
            .unwrap();
        ids.push(s.id);
    }

    // 실행 중인 VM의 스냅샷은 하이퍼바이저로 지운다. 처음 지우기가 실패한다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let scheduler = SnapshotScheduler::spawn(
        env.state.clone(),
        SnapshotSchedulerConfig {
            tick: Duration::from_millis(50),
            hour: Duration::from_secs(60),
        },
    );
    until("delete failure", || {
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
    tokio::time::sleep(Duration::from_millis(500)).await;
    scheduler.shutdown().await;
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let deletes = tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::SnapshotDelete { .. }))
        .count();
    assert_eq!(deletes, 1);
    assert!(snapshots(&env, vm).iter().any(|s| s.id == ids[0]));
}

#[tokio::test]
async fn offline_task_node_is_recorded_without_a_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], json!({ "every": "hourly", "keep": 1 })).await;
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.online = false;
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();

    let scheduler = fast_scheduler(&env);
    until("offline", || {
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("offline"))
    })
    .await;
    assert!(snapshots(&env, vm).is_empty());

    // 노드가 돌아오면 만들고 오류를 지운다.
    node.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    until("recovered", || {
        get_vm(&env, vm).status.schedule_error.is_none() && snapshots(&env, vm).len() == 1
    })
    .await;
    scheduler.shutdown().await;
}

#[tokio::test]
async fn revert_keeps_the_current_schedule() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], Value::Null).await;
    let (_, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "cfg" }))
        .await;
    admin.wait_task(&task).await;

    // 스냅샷 뒤에 예약을 켠다. 되돌려도 스냅샷 때의 "예약 없음"으로 돌아가지 않는다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["snapshot_schedule"] = json!({ "every": "weekly", "keep": 5 });
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, task) = admin
        .post("/vms/web/snapshots/cfg/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(
        get_vm(&env, vm).spec.snapshot_schedule,
        Some(SnapshotSchedule {
            every: ScheduleEvery::Weekly,
            keep: 5,
            memory: false,
        })
    );
}
```

`crates/opld/src/config.rs` 전체:

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_api::SnapshotSchedulerConfig;
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
    #[serde(default)]
    pub schedule: ScheduleSection,
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

/// 예약 스냅샷 작업. 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    /// 예약을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 주기의 한 시간(초). 매일은 24배, 매주는 168배다.
    #[serde(default = "default_hour_secs")]
    pub hour_secs: u64,
}

impl Default for ScheduleSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            hour_secs: default_hour_secs(),
        }
    }
}

impl ScheduleSection {
    pub fn scheduler_config(&self) -> SnapshotSchedulerConfig {
        SnapshotSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            hour: Duration::from_secs(self.hour_secs),
        }
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
fn default_tick_secs() -> u64 {
    60
}
fn default_hour_secs() -> u64 {
    3600
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
        if self.schedule.tick_secs == 0 || self.schedule.hour_secs == 0 {
            anyhow::bail!("schedule.tick_secs and schedule.hour_secs must be at least 1");
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
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(60));
        assert_eq!(schedule.hour, Duration::from_secs(3600));
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
    fn schedule_section_shortens_periods_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[schedule]\ntick_secs = 1\nhour_secs = 2");
        assert!(c.validate().is_ok());
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(1));
        assert_eq!(schedule.hour, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "hour_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[schedule]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[schedule]\ntick = 1")
                .is_err()
        );
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

    #[test]
    fn rejects_unknown_keys_in_hypervisor_section() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[hypervisor]\nacel = \"kvm\"")
                .is_err()
        );
    }

    #[test]
    fn explicit_kvm_needs_dev_kvm() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"kvm\"");
        // `/dev/kvm`이 있는 호스트(Linux CI)에서는 KVM을 쓰고, 없으면 오류다.
        match c.host_profile() {
            Ok(p) => {
                assert!(kvm_available());
                assert_eq!(p.accel, Accel::Kvm);
            }
            Err(e) => {
                assert!(!kvm_available());
                assert!(e.to_string().contains("/dev/kvm"), "{e}");
            }
        }
    }
}
```

`crates/opld/src/member.rs`에서

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시.
```

을 다음으로 바꾼다:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷.
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
```

을 다음으로 바꾼다:

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, SnapshotScheduler, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
```

`crates/opld/src/member.rs`에서

```rust
    monitor: NodeMonitor,
    register: JoinHandle<()>,
```

을 다음으로 바꾼다:

```rust
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    register: JoinHandle<()>,
```

`crates/opld/src/member.rs`에서

```rust
        let (stop, rx) = oneshot::channel::<()>();
        let app = opl_api::router(Arc::new(state)).merge(crate::web::router());
        let task = tokio::spawn(async move {
```

을 다음으로 바꾼다:

```rust
        let (stop, rx) = oneshot::channel::<()>();
        let state = Arc::new(state);
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let app = opl_api::router(state).merge(crate::web::router());
        let task = tokio::spawn(async move {
```

`crates/opld/src/member.rs`에서

```rust
            monitor,
            register,
```

을 다음으로 바꾼다:

```rust
            monitor,
            schedule,
            register,
```

`crates/opld/src/member.rs`에서

```rust
        self.monitor.shutdown().await;
        self.runner.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.runner.shutdown().await;
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 332개, 실패 0

Run: `cargo test -p opl-api --test schedules`
Expected: PASS 5개

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(api): 예약 스냅샷 반복 작업과 예약 오류 기록"
```

---

### Task 6: 실제 QEMU 통합 테스트: 복제와 예약 스냅샷

계획 10의 `crates/opld/tests/snapshots.rs`에 세 테스트를 더한다. 꺼진 VM 복제: 새 VM(`copy-root` 디스크, 새 MAC, `node` 없음, `stopped`)의 디스크 내용이 원본과 같고 그 뒤로는 따로인지 `qemu-io`로 본다. 실행 중인 VM의 스냅샷에서 복제: 스냅샷 뒤에 디스크를 바꾸고 다시 켠 VM에서 복제해도 새 디스크는 스냅샷 때 내용이고 태그가 없으며(`-U`와 태그 확인), UEFI 변수는 스냅샷 사본과 같고, 복제한 VM이 켜진다. 예약(`[schedule] tick_secs = 1, hour_secs = 1`, hourly keep 2): 예약 스냅샷이 넷 이상 생기는 동안 `keep + 1`개를 넘지 않고, 끈 뒤 디스크 태그와 리소스가 같으며 처음 것은 지웠고 사용자 스냅샷은 남는다. 복제·예약 설정은 아직 API로 한다(CLI는 Task 7). 하네스의 `Daemon`은 버려질 때 그 임시 디렉터리를 명령줄에 담은 QEMU를 죽여, 실패한 테스트가 QEMU를 남기지 않는다.

**Files:**
- Modify: `crates/opld/tests/common/mod.rs`
- Modify: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/common/mod.rs`에서

```rust
            let _ = child.wait();
        }
    }
}
```

을 다음으로 바꾼다:

```rust
            let _ = child.wait();
        }
        // 테스트가 중간에 실패해도 이 데몬이 띄운 QEMU(데몬과 따로 돈다)를 남기지 않는다.
        // 명령줄에 이 임시 디렉터리의 경로가 든 프로세스만 맞는다.
        let _ = Command::new("pkill")
            .arg("-KILL")
            .arg("-f")
            .arg(self.path("run").join("vm").display().to_string())
            .status();
    }
}
```

`crates/opld/tests/snapshots.rs`에서

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제)과 볼륨 크기 늘리기를 데몬 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
```

을 다음으로 바꾼다:

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제), 볼륨 크기 늘리기, VM 복제(꺼진 VM, 실행 중인 VM의
//! 스냅샷), 예약 스냅샷(짧은 주기로 만들고 보관 개수대로 지우기)을 데몬 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
```

`crates/opld/tests/snapshots.rs`에서

```rust
use std::process::Command;

use common::Daemon;
use oplctl::Client;
```

을 다음으로 바꾼다:

```rust
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use oplctl::Client;
```

`crates/opld/tests/snapshots.rs`에서

```rust
        .success()
}
```

을 다음으로 바꾼다:

```rust
        .success()
}

/// 볼륨 `name`의 파일(풀 `local`의 qcow2)
async fn volume_file(d: &Daemon, c: &Client, name: &str) -> PathBuf {
    let volume = c.get(&format!("/volumes/{name}")).await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    d.path("pool")
        .join(format!("{}.qcow2", volume["id"].as_str().unwrap()))
}

/// VM `web`의 스냅샷 목록
async fn snapshots(c: &Client) -> Vec<Value> {
    let list = c.get("/vms/web/snapshots").await.unwrap();
    list["items"].as_array().unwrap().clone()
}
```

`crates/opld/tests/snapshots.rs`에서

```rust
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}
```

을 다음으로 바꾼다:

```rust
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn stopped_vm_clone_copies_disk_content() {
    let mut d = Daemon::new("node1", "");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user",
    ])
    .await
    .unwrap();
    assert!(qemu_io(&file, "write -P 0xab 0 64k"));

    // 꺼진 VM은 지금 디스크 내용을 복사한다.
    run(&c, "/vms/web/clone", json!({ "name": "copy" })).await;
    let web = vm(&c).await;
    let copy = c.get("/vms/copy").await.unwrap();
    assert_eq!(copy["spec"]["disks"][0]["volume"], "copy-root", "{copy}");
    assert_eq!(copy["spec"]["memory_mib"], 256, "{copy}");
    assert_eq!(copy["spec"]["power"], "stopped", "{copy}");
    assert_eq!(copy["spec"]["node"], Value::Null, "{copy}");
    let mac = &copy["spec"]["nics"][0]["mac"];
    assert!(mac.is_string(), "{copy}");
    assert_ne!(mac, &web["spec"]["nics"][0]["mac"], "{copy} {web}");
    let copy_file = volume_file(&d, &c, "copy-root").await;
    assert!(qemu_io(&copy_file, "read -P 0xab 0 64k"));

    // 복사본은 원본과 따로다.
    assert!(qemu_io(&file, "write -P 0xcd 0 64k"));
    assert!(qemu_io(&copy_file, "read -P 0xab 0 64k"));
    // 복제가 끝나면 원본을 다시 다룰 수 있다.
    assert_eq!(vm(&c).await["spec"]["disks"][0]["volume"], "root");

    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    assert!(!copy_file.exists());
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn running_vm_snapshot_clone_starts() {
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user",
    ])
    .await
    .unwrap();
    assert!(qemu_io(&file, "write -P 0xab 0 64k"));
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    run(&c, "/vms/web/snapshots", json!({ "name": "base" })).await;
    let base = snapshot(&c, "base").await;
    assert_eq!(base["spec"]["memory"], true, "{base}");
    // 스냅샷 뒤에 디스크를 바꾸고 다시 켠다(실행 중에는 디스크에 쓸 수 없다).
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&file, "write -P 0xcd 0 64k"));
    d.ctl(&["vm", "start", "web"]).await.unwrap();
    let web = vm(&c).await;
    let pid = web["status"]["pid"].as_u64().unwrap();

    // 실행 중인 VM의 스냅샷에서 복제한다. QEMU가 디스크를 쥔 채로 복사하고 VM은 계속 돈다.
    run(
        &c,
        "/vms/web/clone",
        json!({ "name": "copy", "snapshot": "base" }),
    )
    .await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");

    // 새 디스크는 지금 내용이 아니라 스냅샷의 내용을 담고 스냅샷 태그는 없다.
    let copy_file = volume_file(&d, &c, "copy-root").await;
    assert!(tags(&copy_file).is_empty());
    assert!(qemu_io(&copy_file, "read -P 0xab 0 64k"));
    // UEFI 변수는 스냅샷의 사본을 받는다(BIOS VM이면 둘 다 없다).
    let copy = c.get("/vms/copy").await.unwrap();
    let vm_dir = |id: &Value| d.data_dir().join("vm").join(id.as_str().unwrap());
    let snapshot_vars = vm_dir(&web["id"])
        .join("snapshots")
        .join(format!("{}.vars", base["id"].as_str().unwrap()));
    let copy_vars = vm_dir(&copy["id"]).join("uefi-vars.fd");
    assert_eq!(snapshot_vars.exists(), copy_vars.exists());
    if copy_vars.exists() {
        assert_eq!(
            std::fs::read(&snapshot_vars).unwrap(),
            std::fs::read(&copy_vars).unwrap()
        );
    }

    // 복제한 VM은 원본과 함께 따로 돈다.
    let out = d.ctl(&["vm", "start", "copy"]).await.unwrap();
    assert!(out.contains("vm copy: start done"), "{out}");
    let copy = c.get("/vms/copy").await.unwrap();
    assert_eq!(copy["status"]["phase"], "running", "{copy}");
    assert_ne!(copy["status"]["pid"].as_u64(), Some(pid), "{copy}");

    d.ctl(&["vm", "stop", "copy", "--force"]).await.unwrap();
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

/// VM `web`의 예약을 바꾼다. 스냅샷 Task가 도는 동안은 수정이 `vm_busy`라 다시 한다.
async fn set_schedule(c: &Client, schedule: Value) {
    eventually("schedule update", || {
        let schedule = schedule.clone();
        async move {
            let mut spec = vm(c).await["spec"].clone();
            spec["snapshot_schedule"] = schedule;
            match c.put("/vms/web", &json!({ "spec": spec })).await {
                Ok(_) => true,
                Err(e) if e.to_string().contains("vm_busy") => false,
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await;
}

#[tokio::test]
async fn schedule_creates_and_prunes_snapshots() {
    // 한 시간이 1초인 예약
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[schedule]\ntick_secs = 1\nhour_secs = 1\n",
    );
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    // 사용자가 만든 스냅샷은 세지도 지우지도 않는다.
    run(
        &c,
        "/vms/web/snapshots",
        json!({ "name": "mine", "memory": false }),
    )
    .await;
    set_schedule(&c, json!({ "every": "hourly", "keep": 2 })).await;

    // 넷 이상 만들어질 때까지 본다. 넘친 것은 다음 tick에 지우고 그동안 새로 만들지 않으므로
    // 예약 스냅샷은 많아야 keep+1개다.
    let mut seen: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    while seen.len() < 4 {
        assert!(
            Instant::now() < deadline,
            "only {} scheduled:\n{}",
            seen.len(),
            d.log()
        );
        let all = snapshots(&c).await;
        assert!(all.iter().any(|s| s["spec"]["name"] == "mine"), "{all:?}");
        let auto: Vec<&Value> = all
            .iter()
            .filter(|s| s["spec"]["scheduled"] == true)
            .collect();
        assert!(auto.len() <= 3, "{auto:?}");
        for s in auto {
            assert!(
                s["spec"]["name"].as_str().unwrap().starts_with("auto-"),
                "{s}"
            );
            assert_eq!(s["spec"]["memory"], false, "{s}");
            let id = s["id"].as_str().unwrap().to_string();
            if !seen.contains(&id) {
                seen.push(id);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    set_schedule(&c, Value::Null).await;

    // 예약을 끄고 도는 Task가 끝나면, 디스크의 태그와 스냅샷 리소스가 같다. 가장 먼저 만든 것은 지웠다.
    let (c, file) = (&c, file.as_path());
    eventually("snapshots settle", move || async move {
        let all = snapshots(c).await;
        let ids = all
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_string())
            .collect();
        all.iter().all(|s| s["status"]["phase"] == "ready") && sorted(ids) == tags(file)
    })
    .await;
    let left = snapshots(c).await;
    let auto: Vec<String> = left
        .iter()
        .filter(|s| s["spec"]["scheduled"] == true)
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect();
    assert!((2..=3).contains(&auto.len()), "{left:?}");
    assert!(!auto.contains(&seen[0]), "{left:?}");
    assert!(left.iter().any(|s| s["spec"]["name"] == "mine"), "{left:?}");
    for id in &auto {
        assert_eq!(vm_state_size(file, id), 0);
    }
    let v = vm(c).await;
    assert_eq!(v["status"]["schedule_error"], Value::Null, "{v}");
    assert_eq!(v["status"]["phase"], "running", "{v}");

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(tags(file).is_empty());
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 332개, 실패 0

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 5개(약 12초), 남는 QEMU 없음

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "test(opld): 실제 QEMU로 VM 복제와 예약 스냅샷 확인"
```

---

### Task 7: oplctl: 복제, 예약 설정, 스냅샷 목록의 예약 표시

`oplctl vm clone <vm> <새 이름> [--snapshot …] [--pool …]`(Task를 기다리고 "vm {새 이름} cloned from {vm}"). CLI에 VM 수정 명령이 없으므로 설계 §8의 VM 수정 옵션 대신 `oplctl vm snapshot schedule <vm> [--every hourly|daily|weekly --keep N [--memory] | --off]`를 둔다(`--every`와 `--keep`은 함께, `--off`는 혼자). 옵션이 없으면 지금 예약과 마지막 예약 실패를 보이고(`-o json`이면 `{snapshot_schedule, schedule_error}`), 바꿀 때는 VM을 읽어 `If-Match: revision`으로 라벨·스펙을 그대로 둔 채 예약만 바꿔 PUT 한다(`revision_mismatch`면 세 번까지 바로, `vm_busy`면 1초마다 30초까지 다시). `vm delete --with-snapshots`는 확인을 통과하면 예약을 먼저 끄고 다시 확인한다. `vm snapshot list`에 SCHEDULED 열(MEMORY 뒤). 통합 테스트의 복제·예약 설정을 새 명령으로 바꾸고 보기·목록 열·삭제 때의 예약 끄기를 확인한다.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, fetch_ca, fingerprint, local};
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
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
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
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
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
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
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
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
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
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다.
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
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
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
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
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
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
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

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            writeln!(out, "cluster: {}", cell(&view["name"]))?;
            writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
            let leader = view["leader"].clone();
            let rows: Vec<Vec<String>> = view["members"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|m| {
                    vec![
                        cell(&m["name"]),
                        cell(&m["id"]),
                        cell(&m["address"]),
                        if m["voter"] == true {
                            "voter"
                        } else {
                            "learner"
                        }
                        .into(),
                        cell(&m["online"]),
                        (m["id"] == leader).to_string(),
                    ]
                })
                .collect();
            table(
                out,
                &["NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER"],
                &rows,
            )?;
            let voters = view["members"]
                .as_array()
                .map_or(0, |m| m.iter().filter(|m| m["voter"] == true).count());
            if voters == 2 {
                writeln!(
                    out,
                    "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
                )?;
            }
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
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
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
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

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다). VM을 읽어 그 revision으로(`If-Match`)
/// spec 전체를 다시 쓴다.
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = ScheduleRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&schedule_update(&current, schedule.clone()));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// 예약 PUT을 다시 할 횟수
#[derive(Default)]
struct ScheduleRetries {
    mismatch: u32,
    busy: u32,
}

impl ScheduleRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 예약만 바꾼다(`None`이면 끈다).
fn schedule_update(vm: &Value, schedule: Option<Value>) -> Value {
    let mut spec = vm["spec"].clone();
    spec["snapshot_schedule"] = schedule.unwrap_or(Value::Null);
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    if let Some(e) = vm["status"]["schedule_error"].as_str() {
        text.push_str(&format!("last error: {e}\n"));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
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

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
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
    use serde_json::json;

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
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn schedule_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            schedule_update(&vm, Some(schedule.clone())),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            schedule_update(&vm, None),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn schedule_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = ScheduleRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = ScheduleRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!("VM web is busy")
            )),
            "schedule: hourly, keep 1\nlast error: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/opld/tests/snapshots.rs`에서

```rust
    // 꺼진 VM은 지금 디스크 내용을 복사한다.
    run(&c, "/vms/web/clone", json!({ "name": "copy" })).await;
    let web = vm(&c).await;
```

을 다음으로 바꾼다:

```rust
    // 꺼진 VM은 지금 디스크 내용을 복사한다.
    let out = d.ctl(&["vm", "clone", "web", "copy"]).await.unwrap();
    assert!(out.contains("vm copy cloned from web"), "{out}");
    let web = vm(&c).await;
```

`crates/opld/tests/snapshots.rs`에서

```rust
    // 실행 중인 VM의 스냅샷에서 복제한다. QEMU가 디스크를 쥔 채로 복사하고 VM은 계속 돈다.
    run(
        &c,
        "/vms/web/clone",
        json!({ "name": "copy", "snapshot": "base" }),
    )
    .await;
    let v = vm(&c).await;
```

을 다음으로 바꾼다:

```rust
    // 실행 중인 VM의 스냅샷에서 복제한다. QEMU가 디스크를 쥔 채로 복사하고 VM은 계속 돈다.
    d.ctl(&["vm", "clone", "web", "copy", "--snapshot", "base"])
        .await
        .unwrap();
    let v = vm(&c).await;
```

`crates/opld/tests/snapshots.rs`에서

```rust

/// VM `web`의 예약을 바꾼다. 스냅샷 Task가 도는 동안은 수정이 `vm_busy`라 다시 한다.
async fn set_schedule(c: &Client, schedule: Value) {
    eventually("schedule update", || {
        let schedule = schedule.clone();
        async move {
            let mut spec = vm(c).await["spec"].clone();
            spec["snapshot_schedule"] = schedule;
            match c.put("/vms/web", &json!({ "spec": spec })).await {
                Ok(_) => true,
                Err(e) if e.to_string().contains("vm_busy") => false,
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await;
}
```

을 다음으로 바꾼다:

```rust

/// `oplctl vm snapshot schedule web …`로 예약을 바꾼다. 스냅샷 Task가 도는 동안은 수정이
/// `vm_busy`지만 oplctl이 기다렸다 다시 한다.
async fn set_schedule(d: &Daemon, args: &[&str]) {
    let mut argv = vec!["vm", "snapshot", "schedule", "web"];
    argv.extend_from_slice(args);
    d.ctl(&argv).await.unwrap();
}
```

`crates/opld/tests/snapshots.rs`에서

```rust
    .await;
    set_schedule(&c, json!({ "every": "hourly", "keep": 2 })).await;
```

을 다음으로 바꾼다:

```rust
    .await;
    set_schedule(&d, &["--every", "hourly", "--keep", "2"]).await;
    let out = d.ctl(&["vm", "snapshot", "schedule", "web"]).await.unwrap();
    assert!(out.starts_with("schedule: hourly, keep 2\n"), "{out}");
```

`crates/opld/tests/snapshots.rs`에서

```rust
    }
    set_schedule(&c, Value::Null).await;
```

을 다음으로 바꾼다:

```rust
    }
    set_schedule(&d, &["--off"]).await;
```

`crates/opld/tests/snapshots.rs`에서

```rust
    assert_eq!(v["status"]["phase"], "running", "{v}");

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(tags(file).is_empty());
```

을 다음으로 바꾼다:

```rust
    assert_eq!(v["status"]["phase"], "running", "{v}");
    let out = d.ctl(&["vm", "snapshot", "schedule", "web"]).await.unwrap();
    assert_eq!(out, "schedule: off\n");
    // 목록의 SCHEDULED 열(설명이 비어 마지막 칸)이 예약 스냅샷을 가린다.
    let out = d.ctl(&["vm", "snapshot", "list", "web"]).await.unwrap();
    for line in out.lines().skip(1) {
        assert_eq!(line.ends_with("true"), line.contains("auto-"), "{out}");
    }

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    // 예약이 켜져 있으면 스냅샷과 함께 지울 때 먼저 끈다(마지막 예약 스냅샷에서 한 주가 지나지 않았고
    // 보관 개수 안이라 그새 할 일은 없다).
    set_schedule(&d, &["--every", "weekly", "--keep", "5"]).await;
    let out = d
        .ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(
        out.starts_with("snapshot schedule of vm web turned off\n"),
        "{out}"
    );
    assert!(tags(file).is_empty());
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 337개, 실패 0

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 5개(약 11초)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): vm clone, vm snapshot schedule, 스냅샷 목록의 예약 표시"
```

---

### Task 8: 웹: VM 메뉴·상세의 복제 대화상자

`VmCloneDialog`를 VM 메뉴(목록·상세)와 상세 헤더에 둔다(VM·볼륨 Write일 때만). 새 이름(형식과 기존 VM 이름 확인), 원본(꺼진 VM의 지금 상태 또는 `ready` 스냅샷, 기본값은 꺼졌으면 지금 상태, 아니면 가장 최근 스냅샷), 대상 풀(기본 "원본과 같은 풀")을 고르고 작업 알림으로 진행을 보인다. 서버 `check_not_busy`와 같은 `isBusy`(시작·정지 중, 또는 `spec.power`와 phase가 어긋남)를 `actions.ts`에 두고 스냅샷 탭도 함께 쓴다. 실행 중인데 지금 상태를 고르거나 VM이 바쁘면 까닭을 보이고 막는다. 고른 스냅샷이 사라지면 다른 것으로 바꾸지 않고 막는다. 풀은 `cloneNode`(`spec.node` → `status.node` → 첫 디스크 볼륨의 노드 → 접속한 노드)에서 쓸 수 있는 것만, 노드를 정하기 전에는 "원본과 같은 풀"만. `pool_not_on_node`는 풀 칸에 복제용 문구로 보인다. `errors.vm_busy` 문구를 일반적으로 바꾸고 `tasks.op.vm-clone`을 번역한다.

**Files:**
- Create: `web/src/vms/VmCloneDialog.test.tsx`
- Create: `web/src/vms/VmCloneDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/tasks/tasks.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmMenu.tsx`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`
- Modify: `web/src/vms/actions.test.ts`
- Modify: `web/src/vms/actions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "vm_busy": "The VM is starting or stopping. Try again when it has settled.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2 can take snapshots.",
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2 can take snapshots.",
```

`web/src/i18n/en.json`에서

```json
      "snapshot-delete": "Delete snapshot of {{target}}",
      "volume-resize": "Resize volume {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "snapshot-delete": "Delete snapshot of {{target}}",
      "volume-resize": "Resize volume {{target}}",
      "vm-clone": "Clone {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "delete": "Delete",
      "edit": "Edit"
    },
```

을 다음으로 바꾼다:

```json
      "delete": "Delete",
      "edit": "Edit",
      "clone": "Clone"
    },
```

`web/src/i18n/en.json`에서

```json
      "description": "Use at most 1024 characters."
    }
```

을 다음으로 바꾼다:

```json
      "description": "Use at most 1024 characters."
    }
  },
  "clone": {
    "title": "Clone {{name}}",
    "description": "A new stopped VM is made with full copies of the disks. It gets new MAC addresses and no snapshot schedule.",
    "name": "New VM name",
    "source": "Source",
    "current": "Current state (stopped VM only)",
    "snapshot": "Snapshot {{name}}",
    "running": "The VM is not stopped. Stop it first or choose a snapshot.",
    "busy": "The VM is starting or stopping. Try again when it has settled.",
    "gone": "The chosen snapshot no longer exists or is not ready. Choose another source.",
    "goneOption": "Chosen snapshot (no longer available)",
    "pool": "Storage pool for the disks",
    "samePool": "Same pool as each source disk",
    "submit": "Clone",
    "op": "Clone {{vm}} to {{name}}",
    "errors": {
      "exists": "A VM with this name already exists.",
      "poolNotOnNode": "That storage pool is not available on the node holding the VM."
    }
```

`web/src/i18n/ko.json`에서

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "vm_busy": "VM이 시작하거나 정지하는 중입니다. 끝난 뒤 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다.",
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제)이 진행 중입니다. 끝나면 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다.",
```

`web/src/i18n/ko.json`에서

```json
      "snapshot-delete": "{{target}} 스냅샷 삭제",
      "volume-resize": "볼륨 {{target}} 크기 늘리기"
    }
```

을 다음으로 바꾼다:

```json
      "snapshot-delete": "{{target}} 스냅샷 삭제",
      "volume-resize": "볼륨 {{target}} 크기 늘리기",
      "vm-clone": "{{target}} 복제"
    }
```

`web/src/i18n/ko.json`에서

```json
      "delete": "삭제",
      "edit": "편집"
    },
```

을 다음으로 바꾼다:

```json
      "delete": "삭제",
      "edit": "편집",
      "clone": "복제"
    },
```

`web/src/i18n/ko.json`에서

```json
      "description": "1024자까지 쓰세요."
    }
```

을 다음으로 바꾼다:

```json
      "description": "1024자까지 쓰세요."
    }
  },
  "clone": {
    "title": "{{name}} 복제",
    "description": "디스크를 모두 복사해 꺼진 새 VM을 만듭니다. MAC 주소는 새로 정하고 예약 스냅샷은 없습니다.",
    "name": "새 VM 이름",
    "source": "원본",
    "current": "지금 상태(꺼진 VM만)",
    "snapshot": "스냅샷 {{name}}",
    "running": "VM이 꺼져 있지 않습니다. 먼저 정지하거나 스냅샷을 고르세요.",
    "busy": "VM이 시작하거나 정지하는 중입니다. 끝난 뒤 다시 시도하세요.",
    "gone": "고른 스냅샷이 없어졌거나 준비된 상태가 아닙니다. 원본을 다시 고르세요.",
    "goneOption": "고른 스냅샷(이제 없음)",
    "pool": "디스크를 둘 스토리지 풀",
    "samePool": "원본 디스크와 같은 풀",
    "submit": "복제",
    "op": "{{vm}}을(를) {{name}}(으)로 복제",
    "errors": {
      "exists": "같은 이름의 VM이 이미 있습니다.",
      "poolNotOnNode": "VM이 있는 노드에서 쓸 수 없는 스토리지 풀입니다."
    }
```

`web/src/tasks/tasks.test.tsx`에서

```tsx
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
});
```

을 다음으로 바꾼다:

```tsx
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
  const clone = { ...task("4", "pending"), spec: { op: { kind: "vm-clone" }, target: "x" } };
  expect(describeTask(clone as unknown as Task, i18n.t, "web")).toBe("Clone web");
});
```

`web/src/vms/VmCloneDialog.test.tsx`를 만든다:

```tsx
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Principal, Vm } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, snapshot, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { cloneNode } from "@/vms/VmCloneDialog";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmListPage } from "@/vms/VmListPage";

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

function withDisk(v: Vm): Vm {
  return {
    ...v,
    spec: { ...v.spec, disks: [{ volume: `${v.name}-root`, bus: "virtio-blk", boot_order: 1 }] },
  };
}

const db = withDisk(vm("db", "stopped"));
const web = withDisk(vm("web", "running"));

function openDetail(v: Vm, as = OPERATOR) {
  return renderPage(<VmDetailPage />, { path: "/vms/:name", url: `/vms/${v.name}`, as });
}

/** 보낸 복제 요청의 본문을 모은다. */
function captureClone(v: Vm) {
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${v.id}/clone`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", v.id), { status: 202 });
    }),
  );
  return bodies;
}

it("clones a stopped VM's current state from the list menu", async () => {
  mockLists({
    vm: [db, web],
    node: [NODE],
    volume: [volume("db-root", "local")],
    // 복제 Task가 돌 노드(n1)에서 쓸 수 없는 풀은 고를 수 없다.
    "storage-pool": [
      pool("local"),
      pool("fast", { nodes: ["n1"] }),
      pool("far", { nodes: ["n2"] }),
    ],
  });
  const bodies = captureClone(db);
  renderPage(<VmListPage />, { as: OPERATOR });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  expect(within(dialog).getByLabelText("Source")).toHaveValue("current");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "local", "fast"]),
  );

  // 이미 있는 VM 이름은 보내기 전에 알린다.
  const name = within(dialog).getByLabelText("New VM name");
  await userEvent.type(name, "web");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(name).toHaveAccessibleDescription("A VM with this name already exists.");
  expect(name).toBeInvalid();
  expect(bodies).toEqual([]);

  await userEvent.clear(name);
  await userEvent.type(name, "db-copy");
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(await screen.findByText("Clone db to db-copy")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "db-copy", snapshot: null, pool: "fast" }]);
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("clones a running VM from its newest ready snapshot", async () => {
  const base = snapshot("base", web, 1);
  const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id } });
  const taking = snapshot("taking", web, 3, { status: { phase: "pending", created_at: null } });
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    "storage-pool": [pool("local")],
    snapshot: [base, upgraded, taking, snapshot("other", db, 4)],
  });
  const bodies = captureClone(web);
  openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  const source = within(dialog).getByLabelText("Source");
  // 만드는 중이거나 다른 VM의 스냅샷은 고를 수 없다.
  await waitFor(() =>
    expect(
      within(source)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Current state (stopped VM only)", "Snapshot upgraded", "Snapshot base"]),
  );
  expect(source).toHaveValue(upgraded.id);
  const submit = within(dialog).getByRole("button", { name: "Clone" });

  // 실행 중인 VM의 지금 상태는 복제할 수 없고, 그 까닭을 보인다.
  await userEvent.selectOptions(source, "current");
  expect(source).toHaveAccessibleDescription(
    "The VM is not stopped. Stop it first or choose a snapshot.",
  );
  expect(submit).toBeDisabled();

  await userEvent.selectOptions(source, base.id);
  expect(source).not.toHaveAccessibleDescription();
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web-old");
  await userEvent.click(submit);
  expect(await screen.findByText("Clone web to web-old")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "web-old", snapshot: base.id, pool: null }]);
});

it("keeps a chosen snapshot that goes away and blocks cloning", async () => {
  const base = snapshot("base", web, 1);
  const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id } });
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, upgraded],
  });
  const bodies = captureClone(web);
  const { queryClient } = openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  const source = within(dialog).getByLabelText("Source");
  await waitFor(() => expect(source).toHaveValue(upgraded.id));
  await userEvent.selectOptions(source, base.id);
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web-old");

  // 고른 스냅샷이 지워졌다. 다른 스냅샷으로 바꾸지 않고 까닭을 보인다.
  act(() => queryClient.setQueryData(["list", "snapshot"], { revision: 2, items: [upgraded] }));
  await waitFor(() =>
    expect(source).toHaveAccessibleDescription(
      "The chosen snapshot no longer exists or is not ready. Choose another source.",
    ),
  );
  expect(source).toHaveValue(base.id);
  expect(within(source).getByRole("option", { selected: true })).toHaveTextContent(
    "Chosen snapshot (no longer available)",
  );
  const submit = within(dialog).getByRole("button", { name: "Clone" });
  expect(submit).toBeDisabled();
  await userEvent.click(submit);
  expect(bodies).toEqual([]);

  // 다시 고르면 풀린다.
  await userEvent.selectOptions(source, upgraded.id);
  expect(source).not.toHaveAccessibleDescription();
  await userEvent.click(submit);
  expect(await screen.findByText("Clone web to web-old")).toBeInTheDocument();
  expect(bodies).toEqual([{ name: "web-old", snapshot: upgraded.id, pool: null }]);
});

it("explains why a running VM without snapshots cannot be cloned", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")] });
  openDetail(web);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(within(dialog).getByLabelText("Source")).toHaveAccessibleDescription(
    "The VM is not stopped. Stop it first or choose a snapshot.",
  );
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("blocks cloning while the VM is starting", async () => {
  const starting = withDisk(vm("web", "starting"));
  mockLists({
    vm: [starting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", starting, 1)],
  });
  openDetail(starting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText("The VM is starting or stopping. Try again when it has settled."),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("blocks cloning while a stop has yet to finish", async () => {
  // 정지를 요청했지만 게스트가 아직 꺼지지 않았다(서버의 check_not_busy).
  const halting = withDisk(vm("web", "running"));
  halting.spec.power = "stopped";
  mockLists({
    vm: [halting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", halting, 1)],
  });
  openDetail(halting);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone web" });
  expect(
    within(dialog).getByText("The VM is starting or stopping. Try again when it has settled."),
  ).toBeVisible();
  expect(within(dialog).getByRole("button", { name: "Clone" })).toBeDisabled();
});

it("shows the server's refusal in the dialog", async () => {
  mockLists({ vm: [db], node: [NODE], volume: [volume("db-root", "local")] });
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "vm_busy", message: "busy" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
  );
});

it("points at the pool when the VM's node cannot use it", async () => {
  mockLists({
    vm: [db],
    node: [NODE],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local"), pool("fast")],
  });
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "pool_not_on_node", message: "elsewhere" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() => expect(within(pools).getAllByRole("option")).toHaveLength(3));
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  // 업로드의 "접속한 노드" 문구가 아니라 VM이 있는 노드를 말한다.
  await waitFor(() =>
    expect(pools).toHaveAccessibleDescription(
      "That storage pool is not available on the node holding the VM.",
    ),
  );
  expect(pools).toBeInvalid();
  expect(screen.getByRole("dialog", { name: "Clone db" })).toBeInTheDocument();
});

it("picks the node like the server's vm_node", () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  const vols = [volume("db-root", "local", { node: "n3" })];
  // 고정한 노드, 놓인 노드
  expect(cloneNode(db, undefined, "n9")).toBe("n1");
  expect(cloneNode({ ...free, status: { ...free.status, node: "n2" } }, undefined, "n9")).toBe(
    "n2",
  );
  // 첫 디스크 볼륨의 노드. 볼륨 목록을 읽기 전에는 모른다.
  expect(cloneNode(free, vols, "n9")).toBe("n3");
  expect(cloneNode(free, undefined, "n9")).toBeUndefined();
  // 볼륨이 노드에 없거나 디스크가 없으면 접속한 노드(요청을 받는 노드)
  expect(cloneNode(free, [volume("db-root", "local", { node: null })], "n9")).toBe("n9");
  expect(cloneNode({ ...free, spec: { ...free.spec, disks: [] } }, undefined, "n9")).toBe("n9");
  expect(cloneNode(free, [], undefined)).toBeUndefined();
});

it("offers the pools of the connected node when the VM has no node", async () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  mockLists({
    vm: [free],
    node: [NODE],
    volume: [volume("db-root", "local", { node: null })],
    "storage-pool": [pool("all"), pool("here", { nodes: ["n2"] }), pool("far", { nodes: ["n1"] })],
  });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n2",
        members: [],
      }),
    ),
  );
  openDetail(free);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "all", "here"]),
  );
});

it("offers no pools until it knows the node", async () => {
  const free = withDisk(vm("db", "stopped", { spec: { ...db.spec, node: null } }));
  mockLists({ vm: [free], node: [NODE], "storage-pool": [pool("all"), pool("far")] });
  // 볼륨 목록이 오지 않는다.
  server.use(http.get(apiUrl("/volumes"), () => new Promise<never>(() => {})));
  openDetail(free);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await new Promise((r) => setTimeout(r, 50));
  expect(within(dialog).getByRole("status")).toHaveTextContent("Loading");
  expect(
    within(within(dialog).getByLabelText("Storage pool for the disks"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Same pool as each source disk"]);
});

it("hides cloning from viewers", async () => {
  mockLists({ vm: [db], node: [NODE] });
  openDetail(db, VIEWER);
  await screen.findByRole("heading", { name: "db" });
  expect(screen.queryByRole("button", { name: "Clone" })).not.toBeInTheDocument();
});
```

`web/src/vms/VmCloneDialog.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { poolsOnNode } from "@/storage/UploadDialog";
import { trackTask } from "@/tasks/tasks";
import { isBusy, isStopped } from "@/vms/actions";
import { validName } from "@/vms/form";

/** 원본 칸에서 "지금 상태"를 뜻하는 값(스냅샷은 id를 쓴다) */
const CURRENT = "current";

/**
 * 서버의 `vm_node`처럼 복제 Task가 돌 노드: 고정한 노드, 놓인 노드, 첫 디스크 볼륨의 노드, 요청을 받는
 * 노드(접속한 노드) 순. 볼륨 목록이나 접속한 노드를 아직 몰라 정할 수 없으면 undefined.
 */
export function cloneNode(
  vm: Vm,
  volumes: readonly Volume[] | undefined,
  here: string | undefined,
): string | undefined {
  const placed = vm.spec.node ?? vm.status.node;
  if (placed) {
    return placed;
  }
  const first = vm.spec.disks?.[0]?.volume;
  if (first !== undefined) {
    if (volumes === undefined) {
      return undefined;
    }
    const node = volumes.find((v) => v.name === first)?.status.node;
    if (node) {
      return node;
    }
  }
  return here;
}

/** VM 복제 대화상자. 열 때만 그린다. 원본은 꺼진 VM의 지금 상태나 `ready` 스냅샷이다. */
export function VmCloneDialog({
  vm,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const snapshots = useList("snapshot");
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  // 만들어진 차례(UUIDv7)의 역순: 최근 스냅샷이 위에 온다.
  const ready = (snapshots.data ?? [])
    .filter((s) => s.spec.vm === vm.id && s.status.phase === "ready")
    .sort((a, b) => (a.id < b.id ? 1 : -1));
  const stopped = isStopped(vm);
  const [name, setName] = useState("");
  // 고르지 않았으면 꺼진 VM은 지금 상태, 아니면 최근 스냅샷이다.
  const [chosen, setChosen] = useState<string | null>(null);
  const source = chosen ?? (stopped ? CURRENT : (ready[0]?.id ?? CURRENT));
  // 고른 스냅샷이 사라졌거나 ready가 아니게 되었다. 다른 것으로 바꾸지 않고 다시 고르게 한다.
  const gone = source !== CURRENT && !ready.some((s) => s.id === source);
  const [pool, setPool] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 놓인 노드가 없을 때만 접속한 노드가 필요하다(UploadDialog와 같은 질의).
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    enabled: !(vm.spec.node ?? vm.status.node),
  });
  // 노드를 정할 때까지는(목록을 읽는 중) 풀을 보이지 않는다.
  const node = cloneNode(vm, volumes.data, cluster.data?.node);
  const usable = poolsOnNode(pools.data ?? [], node);
  // 서버는 시작·정지 중인 VM을 복제하지 않는다(vm_busy).
  const busy = isBusy(vm);
  // 지금 상태는 꺼진 VM에서만 복사한다(vm_running). 대화상자를 연 사이 켜질 수 있다.
  const running = source === CURRENT && !stopped;

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    } else if ((vms.data ?? []).some((v) => v.name === name)) {
      found.name = "clone.errors.exists";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    let task;
    try {
      task = await call(
        api.POST("/vms/{key}/clone", {
          params: { path: { key: vm.id } },
          body: {
            name,
            snapshot: source === CURRENT ? null : source,
            pool: pool === "" ? null : pool,
          },
        }),
      );
    } catch (err) {
      // 일반 번역은 업로드의 "접속한 노드"를 말한다. 복제는 VM이 있는 노드에서 돈다.
      if (err instanceof ApiError && err.code === "pool_not_on_node") {
        setErrors({ pool: "clone.errors.poolNotOnNode" });
        return false;
      }
      throw err;
    }
    trackTask(task, t("clone.op", { vm: vm.name, name }));
  }

  return (
    <FormDialog
      title={t("clone.title", { name: vm.name })}
      description={t("clone.description")}
      submitLabel={t("clone.submit")}
      // 스냅샷 목록을 읽어야 원본을 고를 수 있다.
      submitDisabled={busy || running || gone || !snapshots.isSuccess}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {busy && <p className="text-sm text-muted-foreground">{t("clone.busy")}</p>}
      <Field id="clone-name" label={t("clone.name")} error={errors.name}>
        {(a11y) => (
          <Input id="clone-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      {!snapshots.isSuccess && <ListPending error={snapshots.error} />}
      <Field
        id="clone-source"
        label={t("clone.source")}
        hint={running ? t("clone.running") : gone ? t("clone.gone") : undefined}
      >
        {(a11y) => (
          <NativeSelect
            id="clone-source"
            className="w-full"
            value={source}
            onChange={(e) => setChosen(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value={CURRENT}>{t("clone.current")}</NativeSelectOption>
            {gone && (
              <NativeSelectOption value={source}>{t("clone.goneOption")}</NativeSelectOption>
            )}
            {ready.map((s) => (
              <NativeSelectOption key={s.id} value={s.id}>
                {t("clone.snapshot", { name: s.spec.name })}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {!pools.isSuccess ? (
        <ListPending error={pools.error} />
      ) : (
        node === undefined && <ListPending error={volumes.error ?? cluster.error} />
      )}
      <Field id="clone-pool" label={t("clone.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="clone-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("clone.samePool")}</NativeSelectOption>
            {usable.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { VmConsole } from "@/vms/lazyConsole";
import { VmConfirm } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
```

을 다음으로 바꾼다:

```tsx
import { VmConsole } from "@/vms/lazyConsole";
import { VmConfirm, type VmDialog } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<"force-stop" | "delete" | null>(null);
  // 고정한 노드가 있으면 그 노드, 아니면 스케줄러가 놓은 노드
```

을 다음으로 바꾼다:

```tsx
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  // 고정한 노드가 있으면 그 노드, 아니면 스케줄러가 놓은 노드
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
            <Link to={`/vms/${encodeURIComponent(vm.name)}/edit`}>{t("vms.action.edit")}</Link>
          </Button>
```

을 다음으로 바꾼다:

```tsx
            <Link to={`/vms/${encodeURIComponent(vm.name)}/edit`}>{t("vms.action.edit")}</Link>
          </Button>
        )}
        {can(me.role, "write", "vm") && can(me.role, "write", "volume") && (
          <Button variant="outline" size="sm" onClick={() => setConfirm("clone")}>
            {t("vms.action.clone")}
          </Button>
```

`web/src/vms/VmMenu.tsx`에서

```tsx
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
```

을 다음으로 바꾼다:

```tsx
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<"force-stop" | "delete" | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  if (!operate && !write) {
```

을 다음으로 바꾼다:

```tsx
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // 복제는 새 VM과 새 볼륨을 만든다.
  const clone = write && can(me.role, "write", "volume");
  if (!operate && !write) {
```

`web/src/vms/VmMenu.tsx`에서

```tsx
          )}
          {write && (
```

을 다음으로 바꾼다:

```tsx
          )}
          {clone && (
            <DropdownMenuItem onSelect={() => setConfirm("clone")}>
              {t("vms.action.clone")}
            </DropdownMenuItem>
          )}
          {write && (
```

`web/src/vms/VmMenu.tsx`에서

```tsx

/** 강제 정지와 삭제의 확인 대화상자 */
export function VmConfirm({
```

을 다음으로 바꾼다:

```tsx

/** 메뉴·상세 버튼이 여는 대화상자 */
export type VmDialog = "force-stop" | "delete" | "clone";

/** 강제 정지·삭제의 확인과 복제 대화상자 */
export function VmConfirm({
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  vm: Vm;
  which: "force-stop" | "delete" | null;
  onClose(): void;
```

을 다음으로 바꾼다:

```tsx
  vm: Vm;
  which: VmDialog | null;
  onClose(): void;
```

`web/src/vms/VmMenu.tsx`에서

```tsx
      )}
    </>
```

을 다음으로 바꾼다:

```tsx
      )}
      {which === "clone" && <VmCloneDialog vm={vm} onClose={onClose} returnFocus={returnFocus} />}
    </>
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx

it("reverts after warning that the current state is lost", async () => {
```

을 다음으로 바꾼다:

```tsx

it("waits like the server while the VM has yet to start", async () => {
  // 켜려는데 아직 멈춰 있다(노드가 막 받았거나 내려갔다). 서버는 vm_busy로 거절한다.
  const pending = withDisk(vm("web", "stopped"));
  pending.spec.power = "running";
  mockLists({
    vm: [pending],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", pending, 1)],
  });
  open(pending);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
  expect(await screen.findByRole("button", { name: "Revert to base" })).toBeDisabled();
});

it("reverts after warning that the current state is lost", async () => {
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The VM is starting or stopping. Try again when it has settled.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
  );
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
import { trackTask } from "@/tasks/tasks";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";
```

을 다음으로 바꾼다:

```tsx
import { trackTask } from "@/tasks/tasks";
import { isBusy } from "@/vms/actions";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
  // 서버는 시작·정지 중인 VM의 스냅샷 작업을 받지 않는다(vm_busy).
  const busy = vm.status.phase === "starting" || vm.status.phase === "stopping";
  // qcow2가 아닌 디스크가 있으면 만들 수 없다(snapshot_unsupported). 볼륨 목록을 다 읽어야 안다.
```

을 다음으로 바꾼다:

```tsx
  // 서버는 시작·정지 중인 VM의 스냅샷 작업을 받지 않는다(vm_busy).
  const busy = isBusy(vm);
  // qcow2가 아닌 디스크가 있으면 만들 수 없다(snapshot_unsupported). 볼륨 목록을 다 읽어야 안다.
```

`web/src/vms/actions.test.ts`에서

```ts
import { vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isStopped } from "@/vms/actions";
```

을 다음으로 바꾼다:

```ts
import { vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isBusy, isStopped } from "@/vms/actions";
```

`web/src/vms/actions.test.ts`에서

```ts
  expect(allowed(pending)).toEqual(["stop", "force-stop"]);
});
```

을 다음으로 바꾼다:

```ts
  expect(allowed(pending)).toEqual(["stop", "force-stop"]);
});

it("follows the server's check_not_busy: starting, stopping or power and phase apart", () => {
  const power = (v: ReturnType<typeof vm>, p: "running" | "stopped") =>
    vm(v.name, v.status.phase, { spec: { ...v.spec, power: p } });
  expect(isBusy(vm("a", "stopped"))).toBe(false);
  expect(isBusy(vm("b", "running"))).toBe(false);
  expect(isBusy(vm("c", "failed"))).toBe(false);
  expect(isBusy(vm("d", "starting"))).toBe(true);
  expect(isBusy(vm("e", "stopping"))).toBe(true);
  // 켜려는데 아직 실행 중이 아니다(막 시작했거나 노드가 내려갔다).
  expect(isBusy(power(vm("f", "stopped"), "running"))).toBe(true);
  expect(isBusy(power(vm("g", "failed"), "running"))).toBe(true);
  // 끄려는데 아직 실행 중이다(ACPI 정지를 기다린다).
  expect(isBusy(power(vm("h", "running"), "stopped"))).toBe(true);
});
```

`web/src/vms/actions.ts`에서

```ts
  return (vm.spec.power ?? "stopped") === "stopped" && (phase === "stopped" || phase === "failed");
}
```

을 다음으로 바꾼다:

```ts
  return (vm.spec.power ?? "stopped") === "stopped" && (phase === "stopped" || phase === "failed");
}

/**
 * 서버의 `check_not_busy`: 시작·정지 중이거나 원하는 전원 상태에 아직 닿지 않았다. 이때 서버는 스냅샷·복제
 * 요청을 받지 않는다(vm_busy).
 */
export function isBusy(vm: Vm): boolean {
  const phase = vm.status.phase;
  if (phase === "starting" || phase === "stopping") {
    return true;
  }
  return vm.spec.power === "running" ? phase !== "running" : !isStopped(vm);
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 214개 (`VmCloneDialog.test.tsx` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): VM 메뉴·상세의 복제 대화상자"
```

---

### Task 9: 웹: VM 폼의 예약 스냅샷과 스냅샷 탭의 예약 요약·실패

VM 폼(만들기·편집)에 "예약 스냅샷" 카드: 주기(끔/매시간/매일/매주, 안내문은 그 선택의 `hint`), 보관 개수(1–100, 기본 7), 메모리 포함. 폼 값은 문자열 입력(`ScheduleRow`)이고 켰을 때만 보관 개수를 검사한다. 끄면 보관 개수·메모리는 묻지 않지만 값은 남긴다. 실행 중인 VM도 바꿀 수 있다. 예약을 켰는데 붙은 볼륨 중 qcow2가 아닌 것이 있으면(볼륨 목록을 읽은 경우) 경고한다. 스냅샷 탭에는 예약 요약(없으면 "스냅샷 예약이 없습니다."), VM 쓰기 권한이면 편집 화면으로 가는 "예약 바꾸기", 마지막 예약 실패(서버 메시지 그대로, 줄바꿈 유지), 예약으로 만든 스냅샷의 배지를 보인다. ko/en 번역.

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`
- Modify: `web/src/vms/form.test.ts`
- Modify: `web/src/vms/form.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
      "description": "Use at most 1024 characters."
    }
  },
```

을 다음으로 바꾼다:

```json
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
    "scheduleFailed": "Last scheduled snapshot failed:"
  },
```

`web/src/i18n/en.json`에서

```json
      "macMulticast": "A multicast address (odd first byte) cannot be used.",
      "macDuplicate": "Each interface needs a different MAC address."
    },
    "volumesCreated": "Volumes {{names}} were created; {{error}}"
  },
```

을 다음으로 바꾼다:

```json
      "macMulticast": "A multicast address (odd first byte) cannot be used.",
      "macDuplicate": "Each interface needs a different MAC address.",
      "keep": "From 1 to 100."
    },
    "volumesCreated": "Volumes {{names}} were created; {{error}}",
    "schedule": "Scheduled snapshots",
    "scheduleNote": "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2.",
    "scheduleEvery": "Take snapshots",
    "scheduleOff": "Off",
    "scheduleKeep": "Snapshots to keep",
    "scheduleRaw": "{{volume}} is {{format}}, so scheduled snapshots will fail. All disks must be qcow2.",
    "scheduleMemoryHint": "Memory is saved only while the VM is running and has disks."
  },
```

`web/src/i18n/ko.json`에서

```json
      "description": "1024자까지 쓰세요."
    }
  },
```

을 다음으로 바꾼다:

```json
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
    "scheduleFailed": "마지막 예약 스냅샷 실패:"
  },
```

`web/src/i18n/ko.json`에서

```json
      "macMulticast": "멀티캐스트 주소(첫 바이트가 홀수)는 쓸 수 없습니다.",
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다."
    },
    "volumesCreated": "볼륨 {{names}}은(는) 만들어졌습니다. {{error}}"
  },
```

을 다음으로 바꾼다:

```json
      "macMulticast": "멀티캐스트 주소(첫 바이트가 홀수)는 쓸 수 없습니다.",
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다.",
      "keep": "1에서 100 사이여야 합니다."
    },
    "volumesCreated": "볼륨 {{names}}은(는) 만들어졌습니다. {{error}}",
    "schedule": "예약 스냅샷",
    "scheduleNote": "주기마다 스냅샷을 만들고 예약으로 만든 것은 최근 것만 남깁니다. 직접 만든 스냅샷은 지우지 않습니다. 디스크가 모두 qcow2여야 합니다.",
    "scheduleEvery": "스냅샷 주기",
    "scheduleOff": "끔",
    "scheduleKeep": "보관 개수",
    "scheduleRaw": "{{volume}}이(가) {{format}}이라 예약 스냅샷이 실패합니다. 디스크가 모두 qcow2여야 합니다.",
    "scheduleMemoryHint": "VM이 실행 중이고 디스크가 있을 때만 메모리도 저장합니다."
  },
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
import type { Task } from "@/api/types";
import { NODE, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import type { Task } from "@/api/types";
import { NODE, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  );
});
```

을 다음으로 바꾼다:

```tsx
  );
});

it("sets a snapshot schedule and checks how many to keep", async () => {
  const web = vm("web", "running");
  mockLists({ vm: [web], node: [NODE] });
  const bodies: { spec: { snapshot_schedule: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  // 실행 중인 VM도 예약은 바꿀 수 있다.
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toBeEnabled();
  expect(every).toHaveValue("off");
  // 끄면 보관 개수와 메모리는 묻지 않는다.
  expect(screen.queryByLabelText("Snapshots to keep")).not.toBeInTheDocument();
  await user.selectOptions(every, "daily");
  const keep = screen.getByLabelText("Snapshots to keep");
  expect(keep).toHaveValue(7);
  await user.clear(keep);
  await user.type(keep, "0");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(keep).toHaveAttribute("aria-invalid", "true");
  expect(keep).toHaveAccessibleDescription("From 1 to 100.");
  expect(bodies).toHaveLength(0);

  await user.clear(keep);
  await user.type(keep, "3");
  await user.click(screen.getByLabelText("Include memory"));
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.snapshot_schedule).toEqual({ every: "daily", keep: 3, memory: true });
});

it("turns a snapshot schedule off", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      snapshot_schedule: { every: "weekly", keep: 4, memory: false },
    },
  });
  mockLists({ vm: [web], node: [NODE] });
  const bodies: { spec: { snapshot_schedule: unknown } }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toHaveValue("weekly");
  expect(screen.getByLabelText("Snapshots to keep")).toHaveValue(4);
  await user.selectOptions(every, "off");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.snapshot_schedule).toBeNull();
});

it("explains the schedule and warns about raw disks", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      disks: [
        { volume: "web-root", bus: "virtio-blk", boot_order: 1 },
        { volume: "web-data", bus: "virtio-blk", boot_order: null },
      ],
    },
  });
  const raw = volume("web-data", "local");
  raw.spec.format = "raw";
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local"), raw] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const every = await screen.findByLabelText("Take snapshots");
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2.",
  );
  // 끈 동안은 경고하지 않는다.
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
  await user.selectOptions(every, "hourly");
  expect(
    screen.getByText("web-data is raw, so scheduled snapshots will fail. All disks must be qcow2."),
  ).toBeVisible();
  // raw 디스크를 떼면 경고도 사라진다.
  await user.click(screen.getByRole("button", { name: "Remove disk 2" }));
  expect(screen.queryByText(/scheduled snapshots will fail/)).not.toBeInTheDocument();
});
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  type NicRow,
  toSpec,
```

을 다음으로 바꾼다:

```tsx
  type NicRow,
  type ScheduleRow,
  toSpec,
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
    );
  const setNic = (i: number, patch: Partial<NicRow>) =>
```

을 다음으로 바꾼다:

```tsx
    );
  const setSchedule = (patch: Partial<ScheduleRow>) =>
    set("snapshotSchedule", { ...values.snapshotSchedule, ...patch });
  const setNic = (i: number, patch: Partial<NicRow>) =>
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 볼륨
  const freeVolumes = (volumes.data ?? []).filter(
```

을 다음으로 바꾼다:

```tsx
  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 볼륨
  // 예약을 켰는데 붙은 볼륨 중 qcow2가 아닌 것이 있으면 예약 스냅샷이 실패한다. 볼륨 목록을 읽어야 안다.
  // 새로 만드는 디스크는 늘 qcow2다.
  const rawDisk =
    values.snapshotSchedule.every !== "off" && volumes.isSuccess
      ? values.disks
          .filter((d) => d.kind === "existing")
          .map((d) => volumes.data.find((v) => v.name === d.volume))
          .find((v) => v !== undefined && v.spec.format !== "qcow2")
      : undefined;
  const freeVolumes = (volumes.data ?? []).filter(
```

`web/src/vms/VmFormPage.tsx`에서

```tsx

        {creating && (
```

을 다음으로 바꾼다:

```tsx

        <Card>
          <CardHeader>
            <CardTitle>{t("vmForm.schedule")}</CardTitle>
          </CardHeader>
          <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            <Field
              id="vm-schedule-every"
              label={t("vmForm.scheduleEvery")}
              hint={t("vmForm.scheduleNote")}
            >
              {(a11y) => (
                <NativeSelect
                  id="vm-schedule-every"
                  {...a11y}
                  className="w-full"
                  value={values.snapshotSchedule.every}
                  onChange={(e) => setSchedule({ every: e.target.value as ScheduleRow["every"] })}
                >
                  <NativeSelectOption value="off">{t("vmForm.scheduleOff")}</NativeSelectOption>
                  <NativeSelectOption value="hourly">
                    {t("snapshots.every.hourly")}
                  </NativeSelectOption>
                  <NativeSelectOption value="daily">
                    {t("snapshots.every.daily")}
                  </NativeSelectOption>
                  <NativeSelectOption value="weekly">
                    {t("snapshots.every.weekly")}
                  </NativeSelectOption>
                </NativeSelect>
              )}
            </Field>
            {rawDisk && (
              <p className="text-sm text-destructive sm:col-span-2">
                {t("vmForm.scheduleRaw", { volume: rawDisk.name, format: rawDisk.spec.format })}
              </p>
            )}
            {/* 끄면 보관 개수와 메모리는 쓰지 않으므로 묻지 않는다. */}
            {values.snapshotSchedule.every !== "off" && (
              <>
                <Field
                  id="vm-schedule-keep"
                  label={t("vmForm.scheduleKeep")}
                  error={errors["schedule.keep"]}
                >
                  {(a11y) => (
                    <Input
                      id="vm-schedule-keep"
                      {...a11y}
                      type="number"
                      min={1}
                      max={100}
                      value={values.snapshotSchedule.keep}
                      onChange={(e) => setSchedule({ keep: e.target.value })}
                    />
                  )}
                </Field>
                <div className="flex flex-col gap-1.5 sm:col-span-2">
                  <div className="flex items-center gap-2">
                    <Checkbox
                      id="vm-schedule-memory"
                      aria-describedby="vm-schedule-memory-hint"
                      checked={values.snapshotSchedule.memory}
                      onCheckedChange={(v) => setSchedule({ memory: v === true })}
                    />
                    <Label htmlFor="vm-schedule-memory">{t("snapshots.includeMemory")}</Label>
                  </div>
                  <p id="vm-schedule-memory-hint" className="text-xs text-muted-foreground">
                    {t("vmForm.scheduleMemoryHint")}
                  </p>
                </div>
              </>
            )}
          </CardContent>
        </Card>

        {creating && (
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  // 목록을 모르면 이름이 겹치는지 모르므로 만들 수 없다.
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});
```

을 다음으로 바꾼다:

```tsx
  // 목록을 모르면 이름이 겹치는지 모르므로 만들 수 없다.
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});

it("shows the schedule, its last failure and the snapshots it took", async () => {
  const failure =
    'VM "web" is being snapshotted, reverted or cloned; try again when the task has finished';
  const scheduled = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
    status: { ...web.status, schedule_error: failure },
  };
  const auto = snapshot("auto-20261027-0300", web, 5, { spec: { scheduled: true } });
  mockLists({
    vm: [scheduled],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, auto],
  });
  open(scheduled);
  expect(
    await screen.findByText("Snapshot schedule: Daily, keep 7, with memory"),
  ).toBeInTheDocument();
  // 서버가 남긴 이유를 그대로, 무엇의 실패인지와 함께 보인다.
  expect(screen.getByText("Last scheduled snapshot failed:").parentElement).toHaveTextContent(
    `Last scheduled snapshot failed: ${failure}`,
  );
  expect(screen.getByRole("link", { name: "Change schedule" })).toHaveAttribute(
    "href",
    "/vms/web/edit",
  );
  const table = screen.getByRole("table", { name: "Snapshots" });
  await within(table).findByText("base");
  const cells = within(table)
    .getAllByRole("row")
    .slice(1)
    .map((r) => within(r).getAllByRole("cell")[0]!.textContent);
  // 예약으로 만든 스냅샷만 표시한다.
  expect(cells).toEqual(["base", "auto-20261027-0300 Scheduled"]);
});

it("says there is no schedule and lets only writers change it", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")], snapshot: [] });
  open(web, { ...ADMIN, role: "viewer" });
  expect(await screen.findByText("No snapshot schedule.")).toBeInTheDocument();
  expect(screen.queryByText("Last scheduled snapshot failed:")).not.toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Change schedule" })).not.toBeInTheDocument();
});
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
  const columns = operate ? 5 : 4;
```

을 다음으로 바꾼다:

```tsx
  const columns = operate ? 5 : 4;
  const schedule = vm.spec.snapshot_schedule;
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
      {operate && busy && <p className="text-sm text-muted-foreground">{t("snapshots.busy")}</p>}
      <Table aria-labelledby="vm-snapshots">
```

을 다음으로 바꾼다:

```tsx
      {operate && busy && <p className="text-sm text-muted-foreground">{t("snapshots.busy")}</p>}
      <div className="flex flex-col gap-1 text-sm">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <p>
            {schedule
              ? t(schedule.memory ? "snapshots.scheduleMemory" : "snapshots.schedule", {
                  every: t(`snapshots.every.${schedule.every}`),
                  keep: schedule.keep,
                })
              : t("snapshots.noSchedule")}
          </p>
          {/* 예약은 VM 설정이라 VM 편집에서 바꾼다. */}
          {can(me.role, "write", "vm") && (
            <Link
              to={`/vms/${encodeURIComponent(vm.name)}/edit`}
              className="underline-offset-4 hover:underline"
            >
              {t("snapshots.scheduleEdit")}
            </Link>
          )}
        </div>
        {/* 예약을 꺼도 마지막 실패는 남는다. 서버가 쓴 이유를 줄바꿈·긴 낱말까지 그대로 보인다. */}
        {vm.status.schedule_error && (
          <p className="text-destructive">
            <span className="font-medium">{t("snapshots.scheduleFailed")}</span>{" "}
            <span className="whitespace-pre-wrap break-words">{vm.status.schedule_error}</span>
          </p>
        )}
      </div>
      <Table aria-labelledby="vm-snapshots">
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
                  {/* 공백이 없으면 접근 이름이 "이름현재"로 붙는다. */}
                  {vm.status.snapshot === snapshot.id && (
```

을 다음으로 바꾼다:

```tsx
                  {/* 공백이 없으면 접근 이름이 "이름현재"로 붙는다. */}
                  {snapshot.spec.scheduled && (
                    <>
                      {" "}
                      <Badge variant="outline" className="ml-1">
                        {t("snapshots.scheduled")}
                      </Badge>
                    </>
                  )}
                  {vm.status.snapshot === snapshot.id && (
```

`web/src/vms/form.test.ts`에서

```ts
  });
  // 폼에 없는 예약 스냅샷도 고칠 때 잃지 않는다.
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
});
```

을 다음으로 바꾼다:

```ts
  });
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
  // 예약이 없으면 끔으로 보이고 그대로 없다.
  const plain = vm("db", "stopped");
  expect(fromVm(plain).snapshotSchedule.every).toBe("off");
  expect(toSpec(fromVm(plain), "stopped").snapshot_schedule).toBeNull();
});

it("checks the number of scheduled snapshots to keep only when scheduled", () => {
  const v = { ...emptyForm("q35"), name: "web" };
  const schedule = { every: "daily" as const, keep: "7", memory: false };
  for (const keep of ["0", "101", "1.5", ""]) {
    expect(validate({ ...v, snapshotSchedule: { ...schedule, keep } }, true)).toEqual({
      "schedule.keep": "vmForm.errors.keep",
    });
  }
  for (const keep of ["1", "100"]) {
    expect(validate({ ...v, snapshotSchedule: { ...schedule, keep } }, true)).toEqual({});
  }
  // 끄면 남은 보관 개수는 보지 않고 보내지도 않는다.
  const off = { ...v, snapshotSchedule: { ...schedule, every: "off" as const, keep: "0" } };
  expect(validate(off, true)).toEqual({});
  expect(toSpec(off, "stopped").snapshot_schedule).toBeNull();
  expect(toSpec({ ...v, snapshotSchedule: schedule }, "stopped").snapshot_schedule).toEqual({
    every: "daily",
    keep: 7,
    memory: false,
  });
});
```

`web/src/vms/form.ts` 전체:

```ts
import type { components } from "@/api/schema";
import type { Node, Vm, VmSpec } from "@/api/types";

type Schemas = components["schemas"];

/** 서버의 `MIN_MEMORY_MIB` */
export const MIN_MEMORY_MIB = 64;
/** 서버의 `MAX_NICS` */
export const MAX_NICS = 16;
/** 서버의 `cores`(u16)와 `boot_order`(u8)의 최댓값 */
const MAX_CORES = 65535;
const MAX_BOOT_ORDER = 255;
/** 서버의 `MAX_SCHEDULE_KEEP` */
const MAX_SCHEDULE_KEEP = 100;
/** 서버의 리소스 이름 규칙: 소문자·숫자·`-`, 63자까지, `-`로 시작하거나 끝나지 않는다. */
const NAME = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAC = /^[0-9a-f]{2}(:[0-9a-f]{2}){5}$/i;
/** `/volumes/import` 경로와 겹쳐 서버가 볼륨 이름으로 받지 않는다. */
export const RESERVED_VOLUME = "import";
const GIB = 1024 ** 3;

export interface DiskRow {
  /** 있는 볼륨을 붙이거나, 만들기 전에 새 빈 볼륨을 만든다. */
  kind: "existing" | "new";
  volume: string;
  pool: string;
  sizeGib: string;
  bus: Schemas["DiskBus"];
  bootOrder: string;
}

export interface NicRow {
  network: string;
  model: Schemas["NicModel"];
  mac: string;
}

/** 예약 스냅샷 입력. 끄더라도 보관 개수와 메모리는 남겨 두어 다시 켤 때 쓴다. */
export interface ScheduleRow {
  every: Schemas["ScheduleEvery"] | "off";
  keep: string;
  memory: boolean;
}

export interface VmFormValues {
  name: string;
  sockets: number;
  cores: string;
  model: Schemas["CpuModel"];
  memory: string;
  firmware: Schemas["Firmware"];
  machine: Schemas["MachineType"];
  disks: DiskRow[];
  nics: NicRow[];
  /** 빈 문자열이면 스케줄러가 고른다. */
  node: string;
  restart: Schemas["RestartPolicy"];
  snapshotSchedule: ScheduleRow;
  /** 만든 뒤 곧바로 시작한다(만들 때만). */
  start: boolean;
}

/**
 * 노드의 아키텍처를 API가 알려 주지 않으므로, 모든 노드가 hvf(Apple Silicon)면 `virt`, 아니면 `q35`를
 * 기본값으로 한다. 다른 경우는 사용자가 고른다.
 */
export function defaultMachine(nodes: Node[]): Schemas["MachineType"] {
  return nodes.length > 0 && nodes.every((n) => n.status.hypervisor === "qemu-hvf")
    ? "virt"
    : "q35";
}

export function emptyDisk(vmName: string, index: number): DiskRow {
  return {
    kind: "new",
    volume: vmName ? `${vmName}-disk${index}` : "",
    pool: "",
    sizeGib: "16",
    bus: "virtio-blk",
    bootOrder: index === 0 ? "1" : "",
  };
}

export function emptyForm(machine: Schemas["MachineType"]): VmFormValues {
  return {
    name: "",
    sockets: 1,
    cores: "2",
    model: "host",
    memory: "2048",
    firmware: "uefi",
    machine,
    disks: [],
    nics: [{ network: "user", model: "virtio", mac: "" }],
    node: "",
    restart: "never",
    snapshotSchedule: { every: "off", keep: "7", memory: false },
    start: true,
  };
}

export function fromVm(vm: Vm): VmFormValues {
  const s = vm.spec;
  return {
    name: vm.name,
    sockets: s.cpu.sockets ?? 1,
    cores: String(s.cpu.cores),
    model: s.cpu.model ?? "host",
    memory: String(s.memory_mib),
    firmware: s.firmware ?? "uefi",
    machine: s.machine,
    disks: (s.disks ?? []).map((d) => ({
      kind: "existing",
      volume: d.volume,
      pool: "",
      sizeGib: "",
      bus: d.bus,
      bootOrder: d.boot_order == null ? "" : String(d.boot_order),
    })),
    nics: (s.nics ?? []).map((n) => ({ network: n.network, model: n.model, mac: n.mac ?? "" })),
    node: s.node ?? "",
    restart: s.restart ?? "never",
    snapshotSchedule: s.snapshot_schedule
      ? {
          every: s.snapshot_schedule.every,
          keep: String(s.snapshot_schedule.keep),
          memory: s.snapshot_schedule.memory ?? false,
        }
      : { every: "off", keep: "7", memory: false },
    start: false,
  };
}

/** 서버의 리소스 이름 규칙에 맞는지. id(UUID)처럼 보이는 이름은 id와 헷갈리므로 받지 않는다. */
export function validName(name: string): boolean {
  return NAME.test(name) && !UUID.test(name);
}

/** GiB 입력을 바이트로. 숫자가 아니거나 양의 안전한 정수가 되지 않으면 undefined */
export function gibToBytes(text: string): number | undefined {
  if (!/^\d+(\.\d+)?$/.test(text)) {
    return undefined;
  }
  const bytes = Math.round(Number(text) * GIB);
  return Number.isSafeInteger(bytes) && bytes > 0 ? bytes : undefined;
}

/** `min`부터 `max`까지의 정수인지 */
function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 입력 오류: 필드 → 번역 키. 서버도 검증하지만 흔한 실수는 보내기 전에 알린다. `volumes`는 이미 있는
 * 볼륨의 이름이다(새 볼륨이 겹치지 않게).
 */
export function validate(
  v: VmFormValues,
  creating: boolean,
  volumes: readonly string[] = [],
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (creating && !validName(v.name)) {
    errors.name = "vmForm.errors.name";
  }
  if (!intIn(v.cores, 1, MAX_CORES)) {
    errors.cores = "vmForm.errors.cores";
  }
  if (!(Number.isInteger(Number(v.memory)) && Number(v.memory) >= MIN_MEMORY_MIB)) {
    errors.memory = "vmForm.errors.memory";
  }
  const boots = new Set<number>();
  const names = new Set<string>();
  v.disks.forEach((d, i) => {
    if (d.kind === "existing" && d.volume === "") {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeChoose";
    } else if (!validName(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volume";
    } else if (names.has(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeDuplicate";
    } else if (d.kind === "new" && d.volume === RESERVED_VOLUME) {
      errors[`disks.${i}.volume`] = "volumes.errors.reserved";
    } else if (d.kind === "new" && volumes.includes(d.volume)) {
      errors[`disks.${i}.volume`] = "vmForm.errors.volumeExists";
    }
    names.add(d.volume);
    if (d.kind === "new" && !d.pool) {
      errors[`disks.${i}.pool`] = "vmForm.errors.pool";
    }
    if (d.kind === "new" && gibToBytes(d.sizeGib) === undefined) {
      errors[`disks.${i}.size`] = "vmForm.errors.size";
    }
    if (d.bootOrder !== "") {
      // "01"과 "1"은 같은 순서다.
      const order = Number(d.bootOrder);
      if (!intIn(d.bootOrder, 1, MAX_BOOT_ORDER) || boots.has(order)) {
        errors[`disks.${i}.bootOrder`] = "vmForm.errors.bootOrder";
      }
      boots.add(order);
    }
  });
  const macs = new Set<string>();
  v.nics.forEach((n, i) => {
    if (n.mac === "") {
      return;
    }
    const mac = n.mac.toLowerCase();
    if (!MAC.test(mac)) {
      errors[`nics.${i}.mac`] = "vmForm.errors.mac";
    } else if (parseInt(mac.slice(0, 2), 16) & 1) {
      // 첫 바이트의 가장 낮은 비트가 멀티캐스트 표시다.
      errors[`nics.${i}.mac`] = "vmForm.errors.macMulticast";
    } else if (macs.has(mac)) {
      errors[`nics.${i}.mac`] = "vmForm.errors.macDuplicate";
    }
    macs.add(mac);
  });
  const schedule = v.snapshotSchedule;
  if (schedule.every !== "off" && !intIn(schedule.keep, 1, MAX_SCHEDULE_KEEP)) {
    errors["schedule.keep"] = "vmForm.errors.keep";
  }
  return errors;
}

/** 폼 값을 VM spec으로. `power`는 만들 때만 정하고, 고칠 때는 지금 값을 그대로 둔다. */
export function toSpec(v: VmFormValues, power: Schemas["PowerState"]): VmSpec {
  return {
    cpu: { sockets: v.sockets, cores: Number(v.cores), model: v.model },
    memory_mib: Number(v.memory),
    firmware: v.firmware,
    machine: v.machine,
    disks: v.disks.map((d) => ({
      volume: d.volume,
      bus: d.bus,
      boot_order: d.bootOrder === "" ? null : Number(d.bootOrder),
    })),
    nics: v.nics.map((n) => ({
      network: n.network,
      model: n.model,
      mac: n.mac === "" ? null : n.mac.toLowerCase(),
    })),
    node: v.node === "" ? null : v.node,
    power,
    restart: v.restart,
    snapshot_schedule:
      v.snapshotSchedule.every === "off"
        ? null
        : {
            every: v.snapshotSchedule.every,
            keep: Number(v.snapshotSchedule.keep),
            memory: v.snapshotSchedule.memory,
          },
  };
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 220개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): VM 폼의 예약 스냅샷과 스냅샷 탭의 예약 요약·실패"
```

---

### Task 10: 브라우저 E2E: 스냅샷에서 복제와 예약 스냅샷

계획 10의 E2E에서 메모리 스냅샷으로 되돌린 뒤 상세 헤더의 "복제"로(원본 기본값이 최근 스냅샷 "before"인지 본다) `copy`를 만들고, 알림으로 끝을 확인한 뒤 복제한 VM(새 볼륨 `copy-root`)을 시작했다가 강제로 끄고 지운다(볼륨은 끝에). 꺼진 VM에 스냅샷 탭의 "예약 바꾸기"로 매시간·보관 2개를 켜고, 예약 스냅샷이 넷 이상 만들어지는 동안 많아야 `keep + 1`개인지, 끈 뒤 2–3개가 남고 처음 것은 지웠고 사용자 스냅샷은 남았는지 본다(이름은 다시 쓰일 수 있어 만든 시각과 함께 센다). 예약 끄기는 스냅샷 Task가 도는 동안 `vm_busy`라 편집 화면을 다시 열어 될 때까지 저장한다. 하네스는 `[schedule] tick_secs = 1, hour_secs = 2`이고, 실패하면 예약을 먼저 끄고(CLI가 `vm_busy`를 기다린다) `copy` VM·볼륨도 지운다.

**Files:**
- Modify: `crates/opld/tests/web.rs`
- Modify: `web/e2e/vm.spec.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/web.rs`에서

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, 스냅샷을 만들고 되돌리고 지우고, 볼륨을 늘린 뒤, VM(스냅샷과 함께)·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

을 다음으로 바꾼다:

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, 스냅샷을 만들고 되돌리고, 그 스냅샷에서 복제한 VM을 시작했다가 지우고, 스냅샷을
//! 지우고, 볼륨을 늘리고, 예약 스냅샷이 생기고 보관 개수대로 지워지는지 본 뒤, VM(스냅샷과 함께)·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

`crates/opld/tests/web.rs`에서

```rust
    );
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
```

을 다음으로 바꾼다:

```rust
    );
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다. 예약 스냅샷은 한 시간을 2초로.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[schedule]\ntick_secs = 1\nhour_secs = 2\n",
    );
    d.start().await;
```

`crates/opld/tests/web.rs`에서

```rust
        // 있어 잠깐만 기다리고, 풀과 남은 파일은 데몬의 임시 디렉터리와 함께 사라진다. 실패는 무시한다.
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
```

을 다음으로 바꾼다:

```rust
        // 있어 잠깐만 기다리고, 풀과 남은 파일은 데몬의 임시 디렉터리와 함께 사라진다. 실패는 무시한다.
        // 예약이 새 스냅샷을 만들지 않게 먼저 끈다(CLI가 vm_busy면 30초까지 다시 시도한다).
        let _ = tokio::time::timeout(
            Duration::from_secs(40),
            d.ctl(&["vm", "snapshot", "schedule", "web", "--off"]),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(30),
            d.ctl(&["vm", "stop", "copy", "--force"]),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            d.ctl(&["vm", "delete", "copy", "--with-snapshots"]),
        )
        .await;
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
```

`crates/opld/tests/web.rs`에서

```rust
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(30),
            d.ctl(&["volume", "delete", "root"]),
        )
        .await;
    }
```

을 다음으로 바꾼다:

```rust
        .await;
        for volume in ["root", "copy-root"] {
            let _ = tokio::time::timeout(
                Duration::from_secs(30),
                d.ctl(&["volume", "delete", volume]),
            )
            .await;
        }
    }
```

`web/e2e/vm.spec.ts`에서

```ts

test.use({ locale: "en-US" });

test("upload an image, run a VM from it, then delete everything", async ({ page }) => {
  await signIn(page);
```

을 다음으로 바꾼다:

```ts

/** 스냅샷 탭의 예약 스냅샷 행: 이름(뒤 배지 글은 뺀다)과 만든 시각(만드는 중이면 null). */
async function scheduledSnapshots(page: Page) {
  const rows = page.getByRole("row").filter({ has: page.getByText("Scheduled", { exact: true }) });
  const cells = await rows.evaluateAll((trs) =>
    trs.map((tr) => [...(tr as HTMLTableRowElement).cells].map((c) => c.textContent ?? "")),
  );
  return cells.map(([name, created]) => ({
    name: /^auto-[0-9-]+/.exec(name ?? "")?.[0] ?? name ?? "",
    created: created === "Creating" ? null : (created ?? null),
  }));
}

test.use({ locale: "en-US" });

test("upload an image, run a VM from it, clone and snapshot it, then delete everything", async ({
  page,
}) => {
  await signIn(page);
```

`web/e2e/vm.spec.ts`에서

```ts

  await page.getByRole("button", { name: "Delete snapshot before" }).click();
```

을 다음으로 바꾼다:

```ts

  // 실행 중인 VM은 지금 상태를 복사하지 않으므로 복제 대화상자는 최근 스냅샷을 원본으로 고른다.
  // 디스크를 QEMU가 쥐고 있어도 스냅샷에서는 복사할 수 있다.
  await page.getByRole("button", { name: "Clone", exact: true }).click();
  dialog = page.getByRole("dialog", { name: "Clone web" });
  await expect(dialog.getByLabel("Source").locator("option:checked")).toHaveText("Snapshot before");
  await dialog.getByLabel("New VM name").fill("copy");
  await dialog.getByRole("button", { name: "Clone", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText("Clone web to copy: done")).toBeVisible({ timeout: 60_000 });

  // 복제한 VM은 꺼진 채로 생기고, 새 볼륨으로 시작한다.
  await page.getByRole("link", { name: "Virtual machines" }).click();
  await page.getByRole("link", { name: "copy", exact: true }).click();
  await expect(page.getByRole("heading", { name: "copy" })).toBeVisible();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped");
  await page.getByRole("tab", { name: "Hardware" }).click();
  await expect(page.getByRole("cell", { name: "copy-root", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Running", { timeout: 60_000 });
  await page.getByRole("button", { name: "Force stop" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Force stop" }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped", { timeout: 60_000 });
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type copy to confirm").fill("copy");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "copy", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });

  await page.getByRole("link", { name: "web", exact: true }).click();
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Delete snapshot before" }).click();
```

`web/e2e/vm.spec.ts`에서

```ts

  // 스냅샷이 있는 VM은 스냅샷도 지우겠다고 골라야 지울 수 있다.
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await expect(confirm.getByRole("button", { name: "Delete" })).toBeDisabled();
  await confirm.getByRole("checkbox", { name: "Also delete its snapshot" }).check();
  await confirm.getByRole("button", { name: "Delete" }).click();
```

을 다음으로 바꾼다:

```ts

  // 예약 스냅샷을 켠다. 하네스가 한 시간을 2초로 줄였다.
  await expect(page.getByText("No snapshot schedule.")).toBeVisible();
  await page.getByRole("link", { name: "Change schedule" }).click();
  await expect(page.getByRole("heading", { name: "Edit web" })).toBeVisible();
  await page.getByLabel("Take snapshots").selectOption("hourly");
  await page.getByLabel("Snapshots to keep").fill("2");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await expect(page.getByText("Snapshot schedule: Hourly, keep 2")).toBeVisible();

  // 넷 이상 만들어질 때까지 본다. 넘친 것은 다음 tick에 지우고 그동안 새로 만들지 않으므로
  // 예약 스냅샷은 많아야 keep+1개다. 이름은 지운 뒤 다시 쓰일 수 있어 만든 시각(초)과 함께 센다.
  const seen: string[] = [];
  let most = 0;
  await expect
    .poll(
      async () => {
        const rows = await scheduledSnapshots(page);
        most = Math.max(most, rows.length);
        for (const { name, created } of rows) {
          if (created !== null && !seen.includes(`${name} ${created}`)) {
            seen.push(`${name} ${created}`);
          }
        }
        return seen.length;
      },
      { timeout: 90_000, intervals: [200] },
    )
    .toBeGreaterThanOrEqual(4);
  expect(most).toBeLessThanOrEqual(3);

  // 예약을 끈다. 예약 스냅샷 Task가 도는 동안에는 VM을 고칠 수 없어(vm_busy) 될 때까지 다시 한다.
  await expect(async () => {
    if (await page.getByRole("heading", { name: "Edit web" }).isVisible()) {
      await page.getByRole("link", { name: "Cancel" }).click();
    }
    await page.getByRole("link", { name: "Edit", exact: true }).click();
    await page.getByLabel("Take snapshots").selectOption("off");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("heading", { name: "Edit web" })).toHaveCount(0, {
      timeout: 5_000,
    });
  }).toPass({ timeout: 60_000 });
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await expect(page.getByText("No snapshot schedule.")).toBeVisible();
  // 도는 Task가 끝나면 남은 예약 스냅샷은 keep개(마지막이 만들기였으면 keep+1개)이고 처음 것은 지웠다.
  // 사용자가 만든 스냅샷은 그대로다.
  await expect(page.getByText("Creating", { exact: true })).toHaveCount(0, { timeout: 60_000 });
  const left = await scheduledSnapshots(page);
  expect(left.length).toBeGreaterThanOrEqual(2);
  expect(left.length).toBeLessThanOrEqual(3);
  expect(left.map(({ name, created }) => `${name} ${created}`)).not.toContain(seen[0]);
  await expect(off).toBeVisible();
  await expect(page.getByText("Last scheduled snapshot failed:")).toHaveCount(0);

  // 스냅샷이 있는 VM은 스냅샷도 지우겠다고 골라야 지울 수 있다.
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await expect(confirm.getByRole("button", { name: "Delete" })).toBeDisabled();
  await confirm.getByRole("checkbox", { name: /^Also delete all \d+ of its snapshots$/ }).check();
  await confirm.getByRole("button", { name: "Delete" }).click();
```

`web/e2e/vm.spec.ts`에서

```ts
    timeout: 60_000,
  });

  await page.getByRole("link", { name: "Storage pools" }).click();
```

을 다음으로 바꾼다:

```ts
    timeout: 60_000,
  });
  await page.getByRole("button", { name: "Delete copy-root" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type copy-root to confirm").fill("copy-root");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "copy-root", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });

  await page.getByRole("link", { name: "Storage pools" }).click();
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 220개

Run: `cargo fmt --all --check && cargo clippy -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS(약 25초), 남는 QEMU 없음

- [ ] **Step 3: 커밋**

```bash
git add web crates
git commit -m "test(web): 브라우저 E2E에 스냅샷에서 복제와 예약 스냅샷 더하기"
```

---

### Task 11: 문서

README에 상태, `vm clone`·`vm snapshot schedule` CLI 예, 웹 기능 목록을 더한다. 설계 문서에 구현하며 정한 것(CLI가 VM 수정 옵션 대신 `vm snapshot schedule`을 쓰는 것, `copy_volume`의 옵션, 복제 검사·Task·정리와 원본 VM 막기, 예약 작업의 재시도 규칙과 `keep + 1`, `schedule_error`의 내용과 정리, reconciler가 이 필드를 지키는 것, 웹 구현, 테스트 설정)과 알고 받아들인 제한을 적는다. CI는 바꾸지 않는다(`linux-kvm`이 이미 `qemu-it`·`web-it`을 돌린다).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-snapshots-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷)
```

`README.md`에서

```markdown
cargo run -p oplctl -- vm snapshot list web      # 나무 모양, 현재 위치 *
cargo run -p oplctl -- vm snapshot revert web before
cargo run -p oplctl -- vm snapshot delete web before
cargo run -p oplctl -- volume resize root --size 2G   # 늘리기만, 실행 중에도 된다
```

을 다음으로 바꾼다:

```markdown
cargo run -p oplctl -- vm snapshot list web      # 나무 모양, 현재 위치 *
cargo run -p oplctl -- vm clone web web-copy --snapshot before   # 스냅샷에서 새 VM(꺼진 VM은 --snapshot 없이 지금 상태)
cargo run -p oplctl -- vm snapshot revert web before
cargo run -p oplctl -- vm snapshot delete web before
cargo run -p oplctl -- vm snapshot schedule web --every daily --keep 7   # 예약 스냅샷(--memory, 끄기 --off)
cargo run -p oplctl -- vm snapshot schedule web  # 지금 예약과 마지막 예약 실패
cargo run -p oplctl -- volume resize root --size 2G   # 늘리기만, 실행 중에도 된다
```

`README.md`에서

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
```

을 다음으로 바꾼다:

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
```

`docs/superpowers/specs/2026-10-26-snapshots-design.md` 전체:

```markdown
# opLite VM 스냅샷 설계 (하위 프로젝트 3의 첫 부분)

작성일: 2026-10-26
상위 문서: `2026-10-20-opl-node-core-design.md` (§1.1의 하위 프로젝트 3 "공유 스토리지와 스냅샷")

## 1. 목표와 범위

VM의 한 순간(디스크, 실행 중이면 메모리, VM 설정)을 저장했다가 되돌린다. 같은 바탕으로 볼륨 크기 늘리기, 스냅샷에서 VM 복제, 예약 스냅샷을 한다. 하위 프로젝트 3의 나머지(NFS, LVM-thin, ZFS, iSCSI, Ceph)는 이 설계의 드라이버 구조 위에 뒤의 계획으로 붙인다.

| 기능 | 내용 |
|---|---|
| 스냅샷 만들기 | 실행 중이면 메모리 포함(기본) 또는 디스크만, 정지했으면 디스크만. 이름·설명 |
| 스냅샷 나무 | 스냅샷마다 부모. 되돌린 뒤 만든 스냅샷은 새 가지가 된다. VM에 현재 위치 |
| 되돌리기 | 디스크, UEFI 변수, VM 설정을 스냅샷 것으로. 메모리 스냅샷이면 그 순간부터 실행 |
| 삭제 | 아무 스냅샷이나(중간 것도). 자식은 부모로 옮겨 붙는다 |
| 볼륨 크기 늘리기 | 정지·실행 중 모두. 늘리기만 |
| 복제 | 스냅샷 또는 정지한 VM의 지금 상태에서 새 VM(디스크 전체 복사) |
| 예약 스냅샷 | VM마다 주기(매시·매일·매주)와 보관 개수 |

넣지 않는 것: 링크드 클론, 시각을 지정하는 예약(매일 02:00 등), raw → qcow2 변환, 스냅샷 이름 바꾸기, 공유 스토리지.

## 2. 방식 선택

- **qcow2 내부 스냅샷.** 스냅샷은 각 디스크 파일 안에 태그로 둔다. 파일 체인(외부 오버레이)을 관리하지 않아 되돌리기와 중간 스냅샷 삭제가 단순하고, 나무 구조가 그대로 된다. Proxmox의 qcow2 스냅샷과 같다.
- **QEMU 11의 QMP 작업.** 실행 중이면 `snapshot-save`/`snapshot-load`/`snapshot-delete`로 하고 `devices`로 디스크를 고른다. `-loadvm`·`savevm`은 쓰지 않는다. 쓰기 가능한 raw pflash(UEFI 변수)가 있으면 실패하기 때문이다. 맥(QEMU 11.1, hvf)에서 저장(메모리 512MiB, 약 0.6초 멈춤)·불러오기·삭제와, `-S`로 띄운 새 QEMU에 `snapshot-load` 뒤 `cont`로 이어 실행되는 것을 확인했다.
- **정지한 VM은 `qemu-img snapshot`.** `-c`(만들기), `-a`(되돌리기), `-d`(삭제). 내부 스냅샷이 있어도 `qemu-img resize`와 `qemu-img convert -l snapshot.name=<태그>`가 된다(확인함).
- **raw 디스크는 거부.** raw 볼륨에는 내부 스냅샷을 둘 수 없다. raw 디스크가 하나라도 붙은 VM은 스냅샷을 만들지 않는다(`snapshot_unsupported`). 일부 디스크만 되돌아가는 일을 막는다.
- **스냅샷은 클러스터 리소스.** 목록·watch·Task·권한을 기존 체계로 쓴다. 디스크의 실제 태그와 어긋날 수 있는 부분은 노드가 시작할 때 정리한다(§6).

## 3. 데이터 모델

### `Snapshot` (새 종류)

| 필드 | 내용 |
|---|---|
| `spec.vm` | VM id |
| `spec.name` | 사용자 이름. 이름 규칙은 다른 리소스와 같고, VM 안에서 고유 |
| `spec.parent` | 부모 스냅샷 id. 첫 스냅샷이면 없음 |
| `spec.memory` | 메모리 상태를 담았는지 |
| `spec.description` | 설명(최대 1024자) |
| `spec.scheduled` | 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다 |
| `spec.vm_spec` | 만든 순간의 VM 스펙 전체 |
| `status.phase` | `pending`(만드는 중) / `ready` |
| `status.created_at` | 만든 시각(unix 초) |
| `status.vmstate_bytes` | 메모리 상태 크기(메모리 스냅샷만). 계획 10은 채우지 않는다(늘 없음) |

- 리소스 이름은 클러스터 전체에서 고유해야 하므로 스냅샷 id 문자열이다. "VM 안에서 이름 고유"는 배타 값(`exclusive.rs`)의 `(vm, name)` 키로 지킨다. API와 CLI는 `/vms/{key}/snapshots/{name}`처럼 VM과 이름으로 가리킨다. 기존 라우트와 맞춰 `{key}`는 VM 이름 또는 id, `{name}`은 스냅샷 이름 또는 id다(웹은 id를 쓴다). 배타 값은 `("name", "{vm}/{name}")`이고, 겹치면 `name_taken`. watch용 전체 목록 `GET /snapshots`도 둔다.
- 디스크 안의 qcow2 태그는 스냅샷 id다. 메모리 상태는 VM 스펙의 첫 qcow2 디스크(`disks[0]`)에 담는다(`vmstate`).
- UEFI 변수 사본: `{state_dir}/vm/{vm-id}/snapshots/{snapshot-id}.vars`. BIOS VM은 변수 파일이 없어 사본도 없다. 사본이 없는 스냅샷으로 되돌리면 변수는 건드리지 않는다.

### VM에 더하는 것

- `status.snapshot`: 현재 위치(마지막으로 만들었거나 되돌린 스냅샷). 다음 스냅샷의 부모.
- `spec.snapshot_schedule` (계획 11): `{every: hourly|daily|weekly, keep: 1..=100, memory: bool}`. 기본은 없음, `memory` 기본 false.
- `status.schedule_error` (계획 11): 마지막 예약 스냅샷 실패 이유. reconciler의 상태 쓰기는 `snapshot`처럼 이 필드도 저장소 것을 지킨다.

기존 VM은 새 필드가 기본값이라 그대로 읽힌다.

### Task 종류

`snapshot-create`, `snapshot-revert`, `snapshot-delete`, `volume-resize`, `vm-clone`(계획 11). 스냅샷·복제 Task는 VM의 디스크를 쓸 노드에서 돈다: `spec.node`(꺼진 VM을 옮겼으면 옮긴 노드), 없으면 `status.node`, 배치된 적이 없으면 첫 디스크 볼륨의 노드(`status.node`), 그것도 없으면 API를 받은 노드. 크기 늘리기는 볼륨을 쓰는 VM에 같은 순서(API 노드 대신 볼륨의 노드)를 쓰고, 쓰는 VM이 없으면 볼륨의 노드에서 돈다(§5). 시작할 때의 고아 정리도 같은 순서로 주인 노드를 정한다(§6).

`vm-clone`(`{name, snapshot?, pool?}`)의 대상은 **원본 VM**이다. 원본 VM의 줄에 서서 그 VM의 스냅샷 Task와 차례로 돈다.

## 4. 드라이버

- **QMP 작업 도우미** (`opl-qemu`): 작업을 시작하고 `JOB_STATUS_CHANGE`(또는 `query-jobs`)로 `concluded`를 기다린 뒤 오류를 읽고 `job-dismiss`한다. 제한 시간을 둔다(540초. Task 제한 600초보다 짧아야 Task가 먼저 끊기지 않고 작업 시간 초과를 받는다). job id는 `opl-{uuid}`. 시간이 지나면 작업을 취소하지 않고 QEMU에 남긴다. 작업 실패(`JobFailed`)와 시간 초과(`JobTimeout`)는 영구 오류다. QEMU 11.1의 `snapshot-delete`는 없는 태그도 성공한다.
- **`Hypervisor` 트레잇**에 실행 중인 VM용 메서드: `snapshot_save(vm, tag, memory)`, `snapshot_delete(vm, tag)`, `block_resize(vm, disk, bytes)`. 디스크 노드(`disk{i}`)는 `query-named-block-nodes`로 QEMU에 묻는다(다시 붙은 VM은 스펙을 모른다). 디스크 없는 VM의 저장은 거부하고 삭제는 성공으로 본다. 살아 있지만 아직 다시 붙지 못한 VM은 `NotAttached`(일시 오류)로 알린다. 요청을 보내지 않았으므로 시간 초과와 달리 QEMU에 남은 작업이 없다. 디스크만 담는 실행 중 스냅샷은 `transaction` + `blockdev-snapshot-internal-sync`로 모든 디스크를 한 번에 한다(VM이 멈추지 않는다). 메모리 복원은 시작 경로에 넣는다(아래).
- **메모리 복원 시작:** reconciler가 VM을 시작할 때 한 번짜리 "불러올 스냅샷"을 받을 수 있게 한다. 드라이버는 지금처럼 `-S`로 띄우고 QMP에 붙은 뒤 `cont` 전에 `snapshot-load`를 한다. 불러오기에 실패하면 QEMU를 끝내고 VM은 `failed`가 된다.
  - 구현: `Hypervisor::start(vm, restore: Option<tag>)`. reconciler는 VM별 "불러올 태그"를 공유 맵에 둔다(`restore_on_start`·`clear_restore`). 채널이 아니라 맡기는 즉시 보이고 `power` 쓰기와 순서가 꼬이지 않는다. 작업자가 새로 띄울 때만 꺼내 쓴다(다시 붙기에는 없음). 이미 살아 있는 VM에는 무시한다.
  - 태그가 있던 시작이 실패하면 일시 오류라도 재시도 없이 `failed`. 태그는 한 번 쓰면 사라진다. VM 삭제·다른 노드로 옮김에도 버린다.
  - 맵은 메모리에만 있다. 되돌리기와 시작 사이에 opld가 재시작하면 태그가 사라지고 VM은 처음부터 부팅한다.
- **`Storage` 트레잇**에 정지한 볼륨용 메서드: `snapshot_create/revert/delete(pool, volume, format, tag)`, `snapshot_tags`(`qemu-img info -U`의 스냅샷 이름), `resize_volume`, `copy_volume(src_pool, src_id, format, tag: Option, force_share, dst_pool, dst_id)`(계획 11, `qemu-img convert -q -f F [-U] [-l snapshot.name=T] -O F`. 대상이 이미 있으면 `VolumeExists`, raw에 태그를 주면 거부. 가져오기처럼 임시 파일에 쓴 뒤 hard link로 게시한다). 같은 태그 만들기는 거부하고, 삭제는 겹친 태그를 모두 지운다. 줄이기는 `qemu-img`가 거부한다. 백엔드가 스냅샷을 지원하는지 알리는 `supports_snapshots(format)`을 둔다. `dir` 백엔드는 qcow2만 지원한다. 뒤의 LVM·ZFS·RBD는 자기 방식으로 구현한다.
- **같은 VM의 작업 직렬화:** Task 실행기의 대상별 줄(`by_target`)을 VM에도 쓴다. 같은 VM의 스냅샷 Task와 전원 Task가 한 노드에서 동시에 돌지 않는다. reconciler의 시작·정지와는 VM 상태(안정 상태 검사)와 Task 안의 재확인으로 겹치지 않게 한다.
- **조건부 상태 쓰기:** reconciler와 Task가 같은 VM `status`를 쓴다. 통째 교체면 Task가 옮긴 현재 위치를 reconciler가 덮는다. 그래서 revision을 확인하는 새 Raft 명령 `UpdateStatusIf { kind, id, expected_revision, status }`를 더했다(`Cluster::update_status_checked`). 기존 `UpdateStatus`는 형식 그대로 둔다. `UpdateStatus`에 선택 필드를 더하면 옛 노드가 그 필드를 버리고 확인 없이 덮어쓰기 때문이다. 옛 노드는 모르는 명령을 오류로 처리한다(노드 코어 설계 3.1절). reconciler의 쓰기는 revision이 다르면 다시 읽어 `snapshot`만 저장소 것을 쓰고 나머지는 전처럼 덮는다. Task의 위치 쓰기도 revision을 확인한다.

## 5. 동작

### 만들기 — `POST /vms/{vm}/snapshots {name, description?, memory?}` → 202

검사:
- VM이 안정 상태여야 한다: 원하는 전원 상태에 닿았고(`power`가 `running`이면 phase `running`, `stopped`면 `stopped`·`failed`) 이 VM의 시작·정지·강제 정지·재부팅 Task가 끝나 있어야 한다. 아니면(시작·정지 중, 켜거나 끄라고 했지만 reconciler가 아직 따라가지 않음, 재부팅 중) 409 `vm_busy`. 되돌리기·삭제도 같다.
- 반대로 이 VM의 스냅샷 만들기·되돌리기 Task가 끝나지 않았으면 VM 동작(시작·정지·강제 정지·재부팅)과 VM 수정이 409 `vm_busy`다. 강제 정지도 막는다: 되돌리기가 디스크를 바꾸는 중에 켜고 끄는 것이 섞이면 안 되고, 만들기 중에 끄면 저장이 끊긴다. 시작한 Task는 Task 제한 시간 안에 끝나지만, 오프라인 노드에 배정돼 시작하지 못한 Task는 그 노드가 돌아올 때까지 `pending`이다(§11).
- 모든 디스크가 qcow2이고 `ready`. qcow2가 아닌 것이 있으면 409 `snapshot_unsupported`, 디스크가 없으면 메모리 스냅샷은 400.
- `memory`는 실행 중이고 디스크가 있으면 기본 true. 디스크 없는 실행 중 VM은 기본으로 설정만 담는다. 정지한 VM에 `memory: true`는 400.

실행: `pending` 리소스를 만들고(`parent` = VM의 `status.snapshot`, `vm_spec` = 지금 스펙) Task를 돌린다. Task를 만들지 못하면 그 리소스를 지우고(못 지우면 재시작 정리에 맡긴다) 오류를 돌려준다. `parent`와 `vm_spec`은 Task가 실행될 때 다시 쓴다. 같은 VM에 만들기가 겹쳐 줄을 서면 API 시점의 부모가 같기 때문이다.

| VM 상태 | 방법 |
|---|---|
| 실행 중 + 메모리 | `snapshot-save` (devices = 모든 디스크, vmstate = 첫 디스크). 저장하는 동안 VM이 멈춘다 |
| 실행 중 + 디스크만 | `transaction` + `blockdev-snapshot-internal-sync` |
| 정지 | 디스크마다 `qemu-img snapshot -c` |

UEFI 변수를 복사하고 `ready`로 바꾸고 VM의 `status.snapshot`을 이 스냅샷으로. 실패하면 만든 태그를 지우고 리소스를 지운다. 단 실패가 확실할 때만 태그를 지운다. 시간 초과(`JobTimeout`, QMP 응답 없음, Task 제한 시간, 재시작 중단)면 작업이 QEMU에 남아 있을 수 있어 태그는 두고 리소스만 지운다. Task 안에서 VM 상태를 다시 확인한다(Task가 기다리는 사이 바뀔 수 있다).

### 되돌리기 — `POST /vms/{vm}/snapshots/{name}/revert` → 202

검사: 스냅샷이 `ready`(아니면 409 `snapshot_not_ready`), `vm_spec`의 볼륨이 모두 있고 `ready`이며 다른 VM이 쓰지 않는다. VM 만들기와 같은 검사(`vms::prepare`, node = 지금, power = stopped)를 쓴다: 볼륨 없음 400, 준비 안 됨·사용 중 409, MAC 겹침 `mac_in_use`.

실행:
1. 실행 중이면 강제로 끈다. 그 전에 Task 안에서 볼륨을 다시 확인한다: 이 노드에서 파일이 보여야 하고(다른 노드에서 만든 볼륨은 공유 풀이어야 한다. 삭제와 같은 검사), API가 확인한 뒤 다른 VM이 붙이지 않았어야 한다. 아니면 끄지도 스펙을 바꾸지도 않고 실패한다.
2. VM 스펙을 `vm_spec`으로 바꾼다. `node`는 지금 노드를 유지하고, `power`는 메모리 스냅샷이면 `running`, 아니면 `stopped`. 그 뒤로 붙인 디스크는 VM에서 떨어지고 볼륨은 남는다.
3. 디스크마다 `qemu-img snapshot -a`, UEFI 변수 사본을 되돌린다. 디스크마다 `-a` 전에 VM을 다시 읽어 `power`가 `stopped`가 아니면(동작 막기를 지나친 요청) 그만두고 실패한다.
4. 볼륨의 실제 크기를 다시 읽어 볼륨 스펙에 반영한다(스냅샷 뒤 늘린 크기는 되돌아간다). 다시 읽은 revision으로 쓴다(라벨 등 그사이 쓴 것을 덮지 않는다).
5. 메모리 스냅샷이면 "불러올 스냅샷"을 주고 시작한다(§4). 그 순간부터 실행된다. 그 전에 이 Task 뒤에 만들어진 같은 VM의 전원 Task(정지 등)가 있으면 켜지 않고 실패한다(재부팅의 `superseded`와 같다).
6. VM의 `status.snapshot`을 이 스냅샷으로. 실제로는 3 직후(디스크를 되돌린 뒤) 옮긴다. 메모리 불러오기가 실패해도 위치는 이 스냅샷이다.

배치되지 않은 VM을 메모리 스냅샷으로 되돌리면 `node`는 이 노드가 된다. 시작 결과는 VM `status`(`running`/`failed`)로 확인한다.

### 삭제 — `DELETE /vms/{vm}/snapshots/{name}` → 202

실행 중이면 `snapshot-delete`, 정지했으면 `qemu-img snapshot -d`. 변수 사본을 지운다. 자식의 `parent`를 지운 스냅샷의 부모로 바꾸고, 현재 위치였으면 VM의 `status.snapshot`도 부모로. 디스크에 태그가 이미 없으면 성공으로 본다(다시 시도해도 된다). 실행 중이면 붙은 디스크는 QMP로 지우고, 그 뒤 떼어 낸 디스크(`vm_spec`에만 있는 볼륨)는 스토리지로 지운다. 이 노드의 볼륨인데 파일이 없으면 성공이다. 다른 노드에서 만든 볼륨(공유 풀이 아님)이면 이 노드에서 파일이 보이지 않으므로 실패다(없는 것으로 보면 그 노드에 태그가 남는다).

### VM 삭제

스냅샷이 남아 있으면 409 `vm_has_snapshots`. CLI `vm delete --with-snapshots`와 웹의 "스냅샷도 모두 지우기"는 스냅샷 삭제 Task를 차례로 돌린 뒤 VM을 지운다. 볼륨은 지금처럼 남는다.
- 차례는 스냅샷 id(UUIDv7, 만든 차례)다. 하나씩 지우고 Task를 기다린다.
- 아무것도 지우기 전에 VM이 지울 수 있는 상태(API의 `is_stopped`: `power`가 `stopped`이고 phase가 `stopped`·`failed`)인지와 모든 스냅샷이 `ready`인지 본다. 아니면 하나도 지우지 않고 오류로 끝난다(VM을 지울 수 없는데 스냅샷만 사라지지 않게). 웹은 확인을 누를 때 라이브 캐시의 최신 VM 상태로 다시 보고, 멈춰 있지 않으면 "먼저 VM을 정지하세요"를 보인다.
- 웹: 스냅샷이 있으면 "스냅샷도 모두 지우기"를 체크해야 삭제 버튼이 켜진다. 스냅샷 목록을 읽기 전이나 읽기 실패면 막는다. 실패·시간 초과면 대화상자에 오류를 보인다. 화면을 떠나면 멈추고 VM은 지우지 않는다.
- 확인과 삭제 사이에 새 스냅샷이 만들어지는 경합은 막지 않는다.

### 볼륨 규칙

스냅샷의 `vm_spec`이 가리키는 볼륨은 지울 수 없다(409 `volume_in_snapshot`). VM에서 떼어 낸 볼륨도 스냅샷이 남아 있으면 지울 수 없다.

### 볼륨 크기 늘리기 — `POST /volumes/{key}/resize {size_bytes}` → 202

지금보다 크고 512 바이트의 배수여야 한다(400). qcow2는 `qemu-img resize`도 QMP `block_resize`도 512 배수가 아닌 크기를 거부한다(QEMU 11.1에서 확인). raw는 받지만 `virtual_bytes`가 512 배수로 올라가 요청과 달라지므로 형식과 관계없이 거부한다. 웹은 1 MiB 단위로 올린 값을 보낸다. 볼륨이 `ready`가 아니면 409 `volume_not_ready`. 실행 중인 VM이 쓰면 그 노드에서 `block_resize`(게스트가 바로 본다), 아니면 `qemu-img resize`. 성공하면 실제 크기(`virtual_bytes`)를 읽어 `spec.size_bytes`를 바꾼다(읽기 실패면 Task 실패). raw도 된다.

- "실행 중" = VM phase가 `stopped`·`failed`가 아님. Task 안에서 `ready`·크기·VM 상태를 다시 본다. 시작·정지 중이면 실패다. QEMU 디스크 번호는 VM 스펙의 디스크 순서(`disk{i}`)다.
- 차례: 같은 디스크의 스냅샷 Task와 겹치지 않게, 크기 늘리기는 볼륨이 아니라 그 볼륨을 쓰는 VM의 줄에 세운다(스펙에서 쓰는 VM → 그 볼륨을 담은 스냅샷의 VM → 없으면 볼륨). 줄은 노드별이라, 꺼진 VM이 쓰는 볼륨도 그 VM의 스냅샷 Task가 돌 노드(§3 순서)로 보낸다. 스펙 크기는 다시 읽은 revision으로 쓴다. API에서 거절하는 방식은 노드 간 확인·실행 틈이 남아 택하지 않았다.

### 복제 (계획 11) — `POST /vms/{key}/clone {name, snapshot?, pool?}` → 202

- 원본: 스냅샷(이름 또는 id, `ready`가 아니면 409 `snapshot_not_ready`), 또는 정지한 VM의 지금 상태. 실행 중인데 스냅샷을 주지 않으면 409 `vm_running`. 원본 VM이 바쁘면(만들기의 안정 상태 검사) 409 `vm_busy`.
- 이름 규칙, 같은 이름 VM이 있으면 409 `name_taken`(API는 VM 리소스만 본다. 진행 중인 다른 복제와 겹치면 늦은 Task가 VM을 만들 때 실패한다). `pool`은 있어야 하고(400) Task 노드에서 쓸 수 있어야 한다(409 `pool_not_on_node`). 원본 스펙의 볼륨은 있는지만 본다(400). `ready`·노드는 Task가 본다.
- 복제 Task가 끝나지 않았으면 원본 VM의 동작·수정·삭제는 `vm_busy`다(꺼진 VM을 복사하는 동안 켜면 `qemu-img` 잠금으로 시작이 실패한다). 원본의 전원은 바꾸지 않으므로 전원 Task의 `superseded` 검사는 그대로다.
- Task: 원본을 다시 읽고 같은 조건을 다시 본다. 디스크마다 원본 볼륨이 이 노드에 있고 `ready`여야 하며, 대상 풀(요청 풀, 없으면 원본 볼륨의 풀)을 이 노드에서 쓸 수 있어야 한다. 새 볼륨 `{새 VM}-{원래 볼륨}`(63자로 자르고 끝의 `-`는 뗀다, 겹치면 `-2`, `-3`…)을 원본과 같은 형식·크기로 `pending`(`status.node` = 이 노드)으로 만들고 `copy_volume`(스냅샷이면 태그 = 스냅샷 id)으로 복사한다. `-U`는 스냅샷에서 복사하고 원본 VM이 완전히 꺼지지 않았을 때만 쓴다. 모든 디스크를 복사하면 UEFI 변수(스냅샷이면 그 사본, 아니면 지금 변수 파일, 없으면 건너뜀)를 새 VM 자리로 복사하고, 볼륨을 `ready`로 바꾼 뒤 새 VM을 만든다.
- 새 VM: 원본 스펙에서 디스크 볼륨 이름만 바꾸고, MAC 새로(다른 VM과 겹치지 않게), `node` 없음, `power: stopped`, 예약 없음, 스냅샷 없음, 라벨 없음.
- 실패·Task 시간 초과면 이 Task가 만든 볼륨(파일과 리소스)과 변수 사본을 지운다. 이름 경합(새 볼륨·VM 이름이 그새 쓰임)도 다시 시도하지 않고 실패다.

### 예약 스냅샷 (계획 11)

리더에서 `tick`(기본 60초)마다 도는 반복 작업 `SnapshotScheduler`(`schedule.rs`, `NodeMonitor`와 같은 모양: 리더 확인 → `read_barrier` → 목록). 할 일은 순수 함수 `plan`이 정한다.
- 이 VM의 스냅샷 만들기·되돌리기·삭제나 복제 Task가 끝나지 않았거나 `pending` 스냅샷이 있으면 건너뛴다.
- 보관: `scheduled`이고 `ready`인 스냅샷이 `keep`개보다 많으면 가장 오래된 것(`created_at`) 하나를 지우고 그 tick에는 만들지 않는다. 한 번에 하나라, 만든 뒤 다음 tick에 지울 때까지 잠시 `keep + 1`개가 있다. 사용자가 만든 스냅샷은 지우지 않는다. 지우려 한(요청했거나 4xx로 거절된) 스냅샷의 id와 시각을 기억해, 끝나지 않은 Task 없이 그 스냅샷이 아직 있으면 지우기가 실패한 것으로 보고 그 삭제는 `hour` 뒤에 다시 한다. 그동안은 만들기 판단으로 넘어간다(보관 정리가 막혀도 새 스냅샷은 만든다).
- 만들기: 마지막 예약 스냅샷의 `created_at`에서 주기가 지났으면(없으면 바로) `auto-YYYYMMDD-HHMM`(UTC, 겹치면 `-2`…) 이름, 빈 설명으로 만든다. 마지막 만들기 시도는 종류와 시각을 기억한다: 4xx로 거절되었으면(바쁨, 노드 오프라인 등 잠깐일 수 있다) `hour` 뒤에, Task를 보냈는데 실패했으면 **다음 주기**에 다시 한다(둘 중 늦은 때). 5xx는 시도로 세지 않아 다음 tick에 다시 한다. 시도는 메모리에만 있어 리더가 바뀌거나 opld가 재시작하면 잊고 한 번 일찍 다시 시도한다. `memory`는 `schedule.memory && phase running && 디스크 있음`.
- 요청은 API 핸들러와 같은 함수(`start_create`·`start_delete`)로 해 같은 검사를 받는다. Task 노드(`vm_placement`)가 노드 목록에 없거나 오프라인이면 요청하지 않고 `node_offline`으로 거절된 것으로 본다(노드를 정할 수 없으면 보지 않는다).
- `schedule_error`: 4xx 거절이면 그 메시지(예: raw 디스크면 `snapshot_unsupported`, 시작·정지 중이면 `vm_busy`, 노드 오프라인)를 쓴다. 보관 삭제의 거절도 쓴다. 5xx(리더를 잃는 등)는 로그만. 예약으로 만든 스냅샷의 만들기 Task는 끝날 때 성공이면 지우고 실패(시간 초과·재시작 중단 포함)면 Task 오류를 쓴다. 보관 삭제 Task는 실패하면 오류를 쓰고, 성공해도 지우지 않는다(만들기가 성공해야 지운다). 같은 값이면 쓰지 않고, 조건부 쓰기(`update_status_checked`, revision이 다르면 다시 읽어 쓰기)로 쓴다. 예약을 꺼도 지우지 않는다.
- 주기: hourly = `hour`, daily = 24×`hour`, weekly = 168×`hour`. `opld` 설정 `[schedule] tick_secs = 60, hour_secs = 3600`(0은 거부, 테스트만 줄인다).
- 되돌리기는 VM의 지금 `snapshot_schedule`을 지킨다(스냅샷 스펙의 예약으로 되돌리지 않는다). 예약 설정은 VM 수정(PUT, VM Write)이다.

## 6. 일관성과 장애

- **opld 재시작:** 지금처럼 실행 중이던 Task는 실패로 끝난다. 노드가 시작할 때 이 노드의 VM에 대해 `pending` 스냅샷(끝나지 않은 Task가 없는 것)을 지우고 디스크의 같은 태그를 지운다(볼륨 고아 정리와 같은 방식). 맡는 노드는 그 스냅샷 만들기 Task의 노드, 없으면 API가 Task를 배정했을 노드(§3: `spec.node`, `status.node`, 첫 디스크 볼륨의 노드), 다 없으면(디스크 없는 VM) 아무 노드다. Task를 만들기 전에 실패해 남은 `pending` 스냅샷도 이때 지운다.
- **메모리 복원 대기 중 재시작:** 불러올 태그는 메모리에만 있어 사라진다. VM은 메모리 상태 없이 처음부터 부팅한다(§4).
- **되돌리기 중단:** 디스크 일부만 되돌아간 채 멈추면 VM은 정지 상태로 남고 Task는 실패다. 같은 되돌리기를 다시 하면 처음부터 다시 한다(`-a`는 몇 번 해도 같다).
- **삭제 중단:** 태그가 없으면 성공으로 보므로 다시 하면 된다.
- **복제 중단:** 실패·시간 초과는 Task가 정리한다. opld가 죽으면 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 볼륨 고아 정리가 지운다(이미 복사한 UEFI 변수 사본은 남는다). 볼륨을 `ready`로 바꾼 뒤 새 VM을 만들기 전에 죽으면 볼륨도 남는다(§11).
- **태그만 남은 경우:** 리소스 없이 디스크에 남은 태그는 공간만 쓴다. 자동으로 지우지 않고, 노드에서 `qemu-img snapshot -l`로 볼 수 있다. 만들기가 시간 초과로 끝나도 태그가 남는다(§5).

## 7. 권한

스냅샷은 VM 권한을 따른다. 핸들러가 VM 종류로 확인한다(역할 표는 그대로). 목록·조회는 Read, 만들기·되돌리기·삭제는 VM Operate(operator 이상). 복제는 VM·볼륨 Write. 예약 설정은 VM Write. 볼륨 크기 늘리기는 볼륨 Write. viewer는 보기만 한다.

## 8. CLI

| 명령 | 계획 |
|---|---|
| `oplctl vm snapshot create <vm> <이름> [--description …] [--no-memory]` | 10 |
| `oplctl vm snapshot list <vm>` (나무 모양, 현재 위치 `*`) | 10 |
| `oplctl vm snapshot revert <vm> <이름>`, `oplctl vm snapshot delete <vm> <이름>` | 10 |
| `oplctl vm delete <vm> --with-snapshots` | 10 |
| `oplctl volume resize <볼륨> --size 20G` | 10 |
| `oplctl vm clone <vm> <새 이름> [--snapshot …] [--pool …]` | 11 |
| `oplctl vm snapshot schedule <vm> [--every hourly\|daily\|weekly --keep N [--memory] \| --off]` | 11 |

명령은 Task가 끝날 때까지 기다린다.

- `list` 열: 현재 위치 `*`, NAME(`├─`/`└─` 나무, 형제는 id 차례, 목록에 없는 부모면 뿌리), CREATED(UTC, `2026-10-26 07:34 UTC`, `ready`가 아니면 `pending`), MEMORY(true/false), DESCRIPTION. `-o json`이면 `items` 배열.
- `--no-memory`면 `memory: false`를 보낸다. 아니면 생략해 서버 기본을 따른다.
- 예약: CLI에 VM 수정 명령이 없어 처음 생각한 VM 수정 옵션 대신 `vm snapshot schedule`을 둔다. `--every`와 `--keep`은 함께 써야 하고 `--off`는 다른 옵션과 함께 쓸 수 없다. 옵션이 없으면 지금 예약(`schedule: daily, keep 7, with memory` / `schedule: off`)과 마지막 예약 실패(`last error: …`)를 보인다(`-o json`이면 `{snapshot_schedule, schedule_error}`). 바꿀 때는 VM을 읽어 `If-Match: revision`으로 라벨·스펙을 그대로 둔 채 예약만 바꿔 PUT 한다(`revision_mismatch`면 세 번까지, 스냅샷·복제 Task 중이라 `vm_busy`면 1초마다 30초까지 다시). `vm delete --with-snapshots`는 지우기 전 확인을 통과하면 예약을 먼저 끄고(`snapshot schedule of vm X turned off`) 다시 확인한 뒤 지운다(그새 예약 스냅샷이 생기지 않게).
- `list`에 SCHEDULED 열(true/false, MEMORY 뒤). `vm clone`은 "vm {새 이름} cloned from {vm}"을 출력한다.

## 9. 웹 UI

계획 10:
- **VM 상세 "스냅샷" 탭:** 나무 모양 목록(이름, 만든 시각, 메모리 포함·크기, 설명, 현재 위치 표시). 만들기 대화상자(이름, 설명, 실행 중이면 "메모리 포함" 기본 켬). 되돌리기 확인("지금 상태는 사라지고 VM이 꺼졌다가 스냅샷 상태가 된다"), 삭제 확인. 스냅샷할 수 없으면(raw 디스크) 탭 안에 까닭을 글로.
- **볼륨 화면:** "크기 늘리기" 대화상자(지금보다 큰 값만).
- **VM 삭제 확인:** 스냅샷이 있으면 "스냅샷도 모두 지우기" 선택.
- 스냅샷은 watch 대상(`WatchKind`)에 넣어 바로 갱신된다.
- 구현: 경로의 `{name}`에 스냅샷 id를 쓴다. 만든 시각은 브라우저 시간대로 보인다(CLI는 UTC). 메모리 크기는 서버가 채우지 않아 보이지 않는다. "메모리 포함" 칸은 실행 중이고 디스크가 있을 때만 보이고, 안 보이면 `memory: false`를 명시한다(대화상자를 연 사이 VM이 켜져도 메모리 스냅샷이 되지 않게). 만들기 버튼은 스냅샷·볼륨 목록을 읽은 뒤에 켠다. 시작·정지 중이면 모든 동작을 막고 까닭을 글로. `pending` 행은 "만드는 중"으로 보이고 되돌리기·삭제를 막는다.
- 구현: 크기 늘리기는 볼륨이 `ready`일 때만. 입력(GiB)은 1 MiB 단위로 올린 뒤 지금 크기(열어 둔 사이 바뀌면 최신)와 비교한다. VM 목록에 일괄 삭제는 없어 "스냅샷도 지우기"는 메뉴·상세의 삭제 대화상자에만 있다(§5). 만드는 중인 스냅샷이 있으면 삭제를 막고 까닭을 글로. 지우는 동안에는 누를 때의 스냅샷 수를 보이고 취소할 수 없다고 알린다.
- 구현: 나무의 깊이는 스크린 리더용 글("(부모 다음)")로도 알린다. 부모가 돌고 도는 스냅샷은 뿌리로 보인다. 볼륨 목록을 읽지 못하면 만들기 버튼 옆에 까닭을 보인다.

계획 11:
- **VM 메뉴 "복제":** 새 이름, 원본(지금 상태 / 스냅샷), 대상 풀.
- **VM 폼 "예약 스냅샷":** 주기, 보관 개수, 메모리 포함. VM 상세에 마지막 예약 실패.
- 구현: 복제는 VM 메뉴(목록·상세)와 상세 헤더에 있고 VM·볼륨 Write일 때만 보인다. 원본 기본값은 꺼진 VM이면 지금 상태, 아니면 가장 최근 `ready` 스냅샷. 실행 중이면 지금 상태를 고를 수 없고(까닭을 글로), 시작·정지 중이거나 스냅샷 목록을 읽기 전이면 막는다. 대상 풀은 "원본과 같은 풀"과 VM 노드(`vm_placement`와 같은 순서)에서 쓸 수 있는 풀만 보인다(노드를 모르면 모든 풀). 이름은 형식과 기존 VM 이름으로 미리 확인한다. 진행은 작업 알림으로 보인다.
- 구현: 예약은 실행 중에도 바꿀 수 있다. 꺼도 보관 개수·메모리 값은 폼에 남는다. 스냅샷 탭에 예약 요약("Daily, keep 7, with memory"), 수정 권한이 있으면 "예약 바꾸기"(VM 편집 화면으로), 예약 스냅샷 행에 "예약" 배지를 보인다. `schedule_error`는 code 없는 서버 메시지라 번역하지 않고 그대로(영어) 보인다.

## 10. 테스트

- **단위:** 타입·직렬화, API 검사 규칙(바쁜 VM, raw 디스크, 정지 VM의 메모리, 이름 중복, 볼륨 삭제 막기, VM 삭제 막기), 나무 정리(자식 옮기기), 예약 판단(주기, 보관 개수)과 이름, QMP 작업 도우미(가짜 QMP).
- **실제 `qemu-img`:** 정지 볼륨의 스냅샷 만들기·되돌리기·삭제(`qemu-io`로 쓴 패턴이 되돌아가는지), 크기 늘리기, 스냅샷에서 복사.
- **실제 QEMU (`qemu-it`, 맥 hvf와 CI KVM):** 실행 중 메모리 스냅샷 → 되돌리기 → 실행 상태로 돌아옴. 실행 중 디스크만 스냅샷. 실행 중 크기 늘리기(QMP 연결은 opld가 쥐고 있어 `query-block` 대신 `qemu-img info -U`의 `virtual-size`로 확인). 되돌리기로 디스크 내용이 돌아오는지는 정지 상태에서 확인(OS 없는 게스트).
- **브라우저 E2E:** 계획 10은 기존 흐름에 메모리 스냅샷 만들기 → 되돌리기 → 삭제와 볼륨 크기 늘리기를 더한다. 계획 11은 스냅샷에서 복제한 VM 시작, 예약 스냅샷(테스트에서만 주기를 짧게 하는 설정).
- 계획 11 구현: 실제 QEMU 테스트는 정지 VM 복제(`qemu-io`로 내용 확인), 실행 중 VM의 스냅샷에서 복제(스냅샷 뒤 바꾼 내용이 들어가지 않음, `-U`) → 시작, `[schedule] tick_secs = 1, hour_secs = 1`로 hourly keep 2가 만들어지고 정리되는지. E2E 하네스는 `tick_secs = 1, hour_secs = 2`이고 예약 행이 3개(`keep + 1`)를 넘지 않는지 본다. 예약을 끄는 PUT은 예약 만들기 Task와 겹치면 `vm_busy`라 CLI가 기다렸다 다시 하고, E2E는 다시 저장한다.

## 11. 알고 받아들인 제한

- 메모리 스냅샷을 만드는 동안 VM이 멈춘다(메모리 크기에 비례, 512MiB에서 약 0.6초).
- raw 디스크가 붙은 VM은 스냅샷할 수 없다.
- 되돌리기는 실행 중인 VM을 강제로 끈다(저장하지 않은 게스트 상태는 사라진다).
- 복제는 디스크 전체를 복사한다(링크드 클론 없음). 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다.
- 예약은 주기만 있고 시각을 정하지 못한다. 리더가 바뀌면 그 주기의 스냅샷이 1분 남짓 늦을 수 있다.
- 예약 만들기 Task가 실패하면 다음 tick이 아니라 다음 주기에 다시 한다(매일이면 하루 뒤). 요청이 거절되면 `hour` 뒤에, 보관 삭제가 실패하면 그 스냅샷의 삭제를 `hour` 뒤에 다시 한다. 리더가 바뀌거나 opld가 재시작하면 시도를 잊어 한 번 일찍 다시 한다. 보관 정리 때문에 잠시 `keep + 1`개가 있을 수 있다.
- 예약 스냅샷 만들기 Task가 도는 동안 VM 수정(예약 끄기 포함)·동작은 `vm_busy`다. 다시 하면 된다(CLI `vm snapshot schedule`은 30초까지 기다렸다 다시 한다).
- 오프라인 노드에 걸린 그 VM의 스냅샷·복제 Task가 끝나지 않으면 예약 작업은 그 VM을 건너뛰므로, 그동안 `schedule_error`에 아무것도 쓰지 않는다(노드 오프라인이 보이지 않는다).
- `schedule_error`는 영어 서버 메시지 그대로라 웹에서 번역되지 않는다. 예약을 꺼도 남는다(다음 예약 만들기가 성공해야 지워진다).
- 복제·가져오기는 Task 제한 시간 600초 안에 끝나야 한다. 큰 디스크 복제는 시간 초과로 실패할 수 있다(만든 볼륨은 지운다).
- 서로 다른 VM의 복제가 동시에 같은 새 볼륨 이름(`{새 VM}-{원래 볼륨}`, 겹치면 `-2`…)을 고르면 뒤의 것이 이름 경합으로 실패한다(다시 하면 된다).
- 복제 중 볼륨을 `ready`로 바꾼 뒤 새 VM을 만들기 전에 opld가 재시작하면 그 볼륨들(`{새 VM}-…`)과 UEFI 변수 사본이 남는다. 볼륨은 손으로 지운다.
- 리소스 없이 디스크에 남은 태그는 자동으로 지우지 않는다. 만들기가 시간 초과로 끝나도 남는다.
- 메모리 스냅샷으로 되돌린 뒤 VM이 시작되기 전에 opld가 재시작하면 VM은 메모리 상태 없이 처음부터 부팅한다(디스크·위치는 그 스냅샷).
- 메모리 불러오기가 실패하면 되돌리기 Task의 오류가 원래 오류 대신 "VM was stopped"로 보일 수 있다(reconciler가 끄기를 두 번에 쓴다). VM의 `failed` 오류에는 원래 오류가 남는다.
- 크기 늘리기의 줄은 Task를 받을 때 정한다. 그 뒤 볼륨을 붙인 VM의 스냅샷 Task와는 차례가 없다. 배치되지 않은 VM의 Task는 첫 디스크 볼륨의 노드에서 돌므로, 디스크가 여러 노드에 걸쳐 있으면 차례가 없을 수 있다(그때는 `qemu-img` 잠금이 막는다).
- 다른 노드의 API가 `pending` 스냅샷을 만들고 Task를 만들기 전의 짧은 틈에 주인 노드가 재시작 정리를 하면 그 스냅샷을 지울 수 있다. 그 만들기 Task는 "snapshot was deleted"로 실패한다(다시 하면 된다).
- 메모리 스냅샷을 불러오며 시작하는 중(`-S`로 띄워 `snapshot-load`를 하는 동안)에 opld가 재시작하면, 다시 붙을 때 `prelaunch`로 남은 VM으로 보고 `cont`를 보내 불러오기와 겹칠 수 있다.
- 실행 중 메모리 스냅샷의 UEFI 변수 사본은 저장이 끝난 뒤 복사한다. 그사이 게스트가 바꾼 변수가 들어가 메모리 상태보다 조금 뒤일 수 있다.
- VM 삭제 확인과 실제 삭제 사이에 만들어진 스냅샷은 막지 않는다(`oplctl vm delete --with-snapshots`는 예약을 먼저 끈다).
- `status.vmstate_bytes`는 채우지 않는다.
- 오프라인 노드에 배정된 만들기·되돌리기 Task는 시작하지 못하므로 Task 제한 시간이 적용되지 않는다. 그 노드가 돌아올 때까지 `pending`이고, 그동안 그 VM의 전원 조작과 수정은 `vm_busy`다. 반대로 오프라인 노드에 배정된 전원 Task가 끝나지 않으면 그 VM의 스냅샷 요청이 `vm_busy`다. VM을 다른 노드로 옮겨도(`spec.node`) 남은 Task 때문에 마찬가지다.
- 스냅샷이 가리키는 볼륨이 지금 다른 실행 중 VM에 붙어 있으면(되돌리기 뒤 떼어 낸 디스크를 다른 VM에 붙인 경우 등) 그 스냅샷의 삭제는 `qemu-img`의 이미지 잠금으로 실패한다(디스크는 손상되지 않는다). 스냅샷이 남으므로 그 VM이 멈출 때까지 원래 VM도 지울 수 없다.

## 12. 계획 분할

| 계획 | 범위 | 완료 기준 |
|---|---|---|
| 10 | `Snapshot` 리소스, QMP 작업 도우미, 드라이버(실행 중·정지), 만들기·되돌리기·삭제, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기, 재시작 정리, CLI, 웹 스냅샷 탭·크기 늘리기·VM 삭제 선택, E2E | 브라우저에서 실행 중인 VM의 메모리 스냅샷을 만들고 되돌려 그 순간부터 실행되는 E2E가 맥과 CI `linux-kvm`에서 통과 |
| 11 | 복제, 예약 스냅샷(리더 반복 작업, 보관 정리), CLI, 웹 | 스냅샷에서 복제한 VM이 시작되고, 예약 스냅샷이 만들어지고 보관 개수대로 정리되는 것을 E2E로 확인 |
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo deny check`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 337개, 실패 0

Run: `cargo test --workspace --all-features`
Expected: macOS에서 PASS(실제 QEMU 테스트와 브라우저 E2E 포함)

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web audit --prod && pnpm -C web build`
Expected: PASS 220개, `No known vulnerabilities found`

Run: `pnpm -C web gen:api && git diff --exit-code web/src/api/schema.d.ts`
Expected: 차이 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: README에 복제·예약 스냅샷 CLI 예, 설계 문서에 구현하며 정한 것"
```

---

## 완료 기준 (계획 11)

- 브라우저에서 실행 중인 VM의 스냅샷으로 복제한 VM이 시작되고, 예약 스냅샷이 만들어지고 보관 개수대로 정리되는 E2E가 macOS(hvf)에서 통과한다 (`pnpm -C web build && cargo test -p opld --features web-it --test web`). CI `linux-kvm`(QEMU 8.2)에서도 통과한다.
- 실제 QEMU 통합 테스트(`cargo test -p opld --features qemu-it --test snapshots`)가 꺼진 VM 복제의 디스크 내용, 실행 중인 VM의 스냅샷에서 복제(스냅샷 뒤 바꾼 내용이 들어가지 않음)와 복제한 VM 시작, 예약 스냅샷의 생성·보관 정리를 확인한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **전체 복사:** 복제는 디스크 전체를 복사한다(링크드 클론 없음). 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다.
- **복제 시간 한도:** 복제는 Task 제한 시간 600초 안에 끝나야 한다. 큰 디스크 복제는 시간 초과로 실패할 수 있다(만든 볼륨은 지운다).
- **복제 볼륨 이름 경합:** 서로 다른 VM의 복제가 동시에 같은 새 볼륨 이름을 고르면 뒤의 것이 실패한다(다시 하면 된다).
- **복제 중 재시작:** 볼륨을 `ready`로 바꾼 뒤 새 VM을 만들기 전에 opld가 재시작하면 그 볼륨들과 UEFI 변수 사본이 남는다. 볼륨은 손으로 지운다.
- **예약은 주기만:** 시각을 정하지 못한다. 리더가 바뀌면 그 주기의 스냅샷이 1분 남짓 늦을 수 있다.
- **예약 재시도:** 만들기 Task가 실패하면 다음 주기에(매일이면 하루 뒤), 요청이 거절되면 `hour` 뒤에, 보관 삭제가 실패하면 그 삭제를 `hour` 뒤에 다시 한다. 시도는 메모리에만 있어 리더가 바뀌거나 opld가 재시작하면 한 번 일찍 다시 한다. 보관 정리 때문에 잠시 `keep + 1`개가 있을 수 있다.
- **예약 만들기 중 VM 조작:** 예약 스냅샷 만들기 Task가 도는 동안 VM 수정(예약 끄기 포함)·동작은 `vm_busy`다(CLI `vm snapshot schedule`은 30초까지 기다렸다 다시 한다).
- **오프라인 노드:** 그 VM의 스냅샷·복제 Task가 오프라인 노드에 걸려 끝나지 않으면 예약 작업은 그 VM을 건너뛰고, 그동안 `schedule_error`에 아무것도 쓰지 않는다.
- **`schedule_error`:** 영어 서버 메시지 그대로라 웹에서 번역되지 않는다. 예약을 꺼도 남는다(다음 예약 만들기가 성공해야 지워진다).
- 그 밖의 경합과 순서 제한은 설계 문서 §11에 있다.
