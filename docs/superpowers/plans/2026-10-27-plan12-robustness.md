# opLite 계획 12: 클러스터·Task 견고성 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 계획 8–11에서 넘긴 사항 가운데 데이터가 어긋나거나 VM이 멈춘 채 남는 문제를 고친다. 오프라인·제거된 노드에 걸린 Task와 Task 없이 남은 `pending` 스냅샷을 리더가 끝내고, 응답 없이 끝난 전달 뒤 다시 보낸 만들기·삭제를 성공으로 판단하며, 스냅샷 만들기 중에도 강제 정지로 VM을 끌 수 있게 하고, 예약 오류를 code로 남겨 웹에서 번역한다.

**Architecture:** 새 반복 작업 `ClusterJanitor`(`opl-api/src/janitor.rs`)가 `SnapshotScheduler`와 같은 모양으로 리더에서 `tick`마다 노드·Task·스냅샷 목록을 읽고, 순수 함수 `decide`로 할 일(`FailTask`·`DeleteSnapshot`)을 정해 읽은 revision으로만 쓴다. 실행기의 `pending` → `running` 쓰기도 조건부(`update_status_checked`)로 바꿔 둘이 겹치면 한쪽만 이긴다. `Cluster::request`는 전달 시도가 응답 없이 끝나면 "적용됐을 수 있음"으로 표시하고, 그 뒤의 `AlreadyExists`(같은 id 만들기)·`NotFound`(삭제)를 성공으로 바꾼다. `VmStatus.schedule_error`는 `ScheduleError { code, message }` 구조가 되고, `TaskOp::SnapshotDelete`의 `scheduled`로 예약 정리 삭제를 사용자 삭제와 구분한다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft, axum 0.8, utoipa, clap, QEMU 11.1(맥)·8.2(Linux CI); 웹은 계획 8~11과 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW). 새 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-10-27-robustness-design.md` (전체가 이 계획)
**선행 계획:** 계획 1~11 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `ScheduleError { code, message }`(옛 문자열은 `code: "unknown"`으로 읽기), `TaskOp::SnapshotDelete`의 `scheduled` | 업로드 사전 확인 API, 비밀번호·로그인 시도 제한, 노드 아키텍처 API — 계획 13 |
| 전달 재시도의 결과 판단(다시 보낸 `Create`의 같은 id `AlreadyExists`, `Delete`의 `NotFound`를 성공으로) | 작업 목록 페이지 나누기, `index.html` ETag, VM 폼 나누기, 번역 키 `common.*`, 대화상자 뒤 포커스, 남은 전원 Task로 버튼 미리 막기 — 계획 14 |
| 실행기의 조건부 Task 시작(그새 실패로 끝난 Task는 실행하지 않음) | 공유 스토리지(NFS·LVM·ZFS·iSCSI·Ceph) — 계획 14 뒤 |
| `ClusterJanitor`: 오프라인 노드(유예 뒤)·제거된 노드의 Task 끝내기, Task 없이 남은 `pending` 스냅샷 지우기, opld `[janitor]` 설정 | 실행 중이던 Task를 리더가 끝내기(네트워크만 끊긴 노드는 아직 일하고 있을 수 있다) |
| 스냅샷 만들기·삭제·복제 중 강제 정지 받기, 끊긴 만들기의 태그 지우기 | 모든 쓰기의 정확히 한 번 적용(요청 id 표) |
| 예약 오류 code(거절 code, `task_failed`), 예약 정리 삭제의 실패만 기록, CLI `last error: <code>: <message>` | 정리 작업이 끝낸 Task의 볼륨 파일을 다른 노드에서 지우기 |
| 웹: 예약 오류 번역(`snapshots.scheduleError.<code>` → `errors.<code>` → 메시지), 되돌리기 중에만 강제 정지 막기 | 브라우저 E2E 바꾸기(오프라인 노드는 브라우저 하나로 재현하기 어렵다) |
| 실제 QEMU 통합 테스트(메모리 스냅샷 중 강제 정지) | |

## 이전 계획에서 이어받는 사항

계획 11의 "다음 계획으로 넘길 사항" 가운데 이 계획이 하는 것:
- Task 없이 남은 `pending` 스냅샷을 주인 노드 재시작 없이 정리하기(Task 4), 사용자 삭제와 예약 정리 삭제의 오류 구분(Task 1·6).
- 오프라인 노드에 배정된 Task를 끝내기(Task 3·4, 계획 10에서 넘김).
- `schedule_error`에 오류 code를 담아 번역하기(Task 1·6·7).

이미 끝났거나 다른 계획이 하는 것:
- 불안정한 테스트 `new_leader_does_not_revive_nodes_recorded_offline`은 이 계획 전에 main에서 고쳤다(`1b9b5ec`, 새 리더의 read barrier가 잠깐 실패하면 다시 한다).
- 웹이 남은 전원 Task를 보고 버튼을 미리 막기는 계획 14다. 이 계획은 강제 정지를 끝나지 않은 되돌리기 Task가 있을 때만 막는다(Task 7).
- 계획 9에서 넘긴 사항(업로드 사전 확인 API, 시도 제한, 노드 아키텍처 API)은 계획 13이다.
- 하위 프로젝트 3의 나머지(공유 스토리지)는 계획 13·14 뒤에 한다(설계 §1).

## 실행 환경

계획 11과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상과 `qemu-io`), Node.js 22.12 이상, pnpm 10.33.2. 실제 QEMU 테스트는 기능으로 켠다: `opld`의 `qemu-it`(이 계획은 `tests/snapshots.rs`에 하나를 더한다), `opl-api`·`opl-driver`의 `storage-it`, `opld`의 `web-it`(브라우저 E2E, 이 계획은 바꾸지 않는다).

Linux CI(`linux-kvm`, Ubuntu 24.04)는 QEMU 8.2다. `qemu-img`·`qemu-io`·QMP에는 8.2에 있는 옵션만 쓴다. 이 계획은 새 QEMU 옵션을 쓰지 않는다(강제 정지 테스트는 프로세스를 `SIGSTOP`·`SIGKILL`로 다룬다).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1만). 커밋한다. `Cargo.lock`은 바뀌지 않는다.

## 설계 결정

- **`ScheduleError`.** `VmStatus.schedule_error: Option<ScheduleError>`, `ScheduleError { code, message }`. 직렬화는 늘 구조이고 OpenAPI에도 `ScheduleError` 컴포넌트로 보인다. 역직렬화는 옛 문자열도 받아 `{ code: "unknown", message }`로 읽는다(저장된 상태를 옮기지 않는다). `TaskOp::SnapshotDelete { snapshot, #[serde(default)] scheduled }`: 예약 정리(`start_delete(…, scheduled: true)`)만 `true`, API의 사용자 삭제와 옛 Task는 `false`. 옛 노드와 섞인 클러스터는 지원하지 않는다.
- **전달 재시도의 결과 판단.** `Cluster::request`가 리더에 전달한 시도가 시도 제한 시간에 걸리거나 전달자가 `Unreachable`을 돌려주면 "적용됐을 수 있음"으로 표시한다(연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 `Unreachable`은 늘 표시한다). 표시 뒤 다시 보낸 `Create`가 **같은 id**로 `AlreadyExists`를 받으면(id는 요청한 쪽이 만든 UUID v7) 이 노드에 보일 때까지 기다려 그 **지금 revision**을, `Delete`가 `NotFound`를 받으면 이 노드에서 사라질 때까지 기다려 **이 노드가 적용한 인덱스**를 돌려준다. 남은 제한 시간 안에 안 되면 `Timeout`이다. 조건부 수정 등은 판단하지 않아 다시 보낸 요청이 `revision_mismatch`로 끝날 수 있다. `Unreachable` 뒤의 `Delete`는 다른 쪽이 지웠어도 성공이다. 공개 API와 Raft 로그 형식은 바뀌지 않는다.
- **실행기의 시작(`begin`).** `execute`는 무조건 `running`을 쓰는 대신 Task를 이 노드에서 다시 읽어, `pending`이면 그 revision으로 `running`을 쓰고(`update_status_checked`), 이미 `running`이면 실행한다(앞선 쓰기의 응답만 잃은 경우, `running`은 그 노드만 쓴다). `succeeded`·`failed`이거나 없으면(`NotFound` 거부 포함) 실행하지 않는다. 그 밖의 오류(`revision_mismatch` 포함)는 `poll_interval` 뒤 다시 읽어 판단한다. 그 뒤의 상태 쓰기(성공·실패)는 지금처럼 그 노드만, 조건 없이 쓴다.
- **정리 작업 `ClusterJanitor`.** 리더에서 `tick`(기본 10초)마다 리더 확인 → `read_barrier` → 목록. 판단은 순수 함수 `decide(now, config, nodes, members, tasks, snapshots, &mut seen) -> Vec<Action>`. 오프라인 노드는 처음 오프라인으로 본 시각을 메모리에 두고(온라인이 되면 지운다) `offline_grace`(기본 60초)가 지나면 그 노드의 `pending` Task를 실패로(`node {id} is offline`). `running`은 건드리지 않는다. **제거된 노드** = 노드 목록에 없고 Raft 멤버십에도 없는 노드(조인 중인 노드는 제거된 것이 아니다)이고, 그 노드의 `pending`·`running` Task를 곧바로 실패로(`node {id} was removed`). `{id}`는 노드 UUID. 리더가 아니면 기억한 상태를 지운다.
- **남은 `pending` 스냅샷은 시간으로.** 끝나지 않은 `SnapshotCreate` Task가 없는 `pending` 스냅샷을 처음 본 뒤 `orphan_grace`(`ClusterJanitorConfig`, 기본 30초)가 지나도 그대로면 리소스를 지운다. tick이 짧아도 API가 스냅샷을 만들고 Task를 만드는 사이(전달된 쓰기 하나에 최대 `write_timeout` 5초)에 지우지 않게 한다. 그새 Task가 보이면 다시 잰다. opld 설정에는 두지 않는다. 디스크의 태그는 남을 수 있다. 볼륨은 업로드 중에도 Task 없이 `pending`이라 정리하지 않는다.
- **정리 작업의 쓰기.** 읽은 revision으로만 쓴다(`update_status_checked`, `delete(Some(revision))`). 실패하면 로그만 남기고 다음 tick에 다시 본다. 실패 쓰기가 `revision_mismatch`로 거부되면 Task를 다시 읽어 이 정리 작업의 오류로 `failed`면 쓰인 것으로 본다(전달 재시도에서 앞선 시도의 응답만 잃은 경우). 실행기가 먼저 `running`을 썼으면 옛 revision의 실패 쓰기는 거부된다. opld `[janitor] tick_secs = 10, offline_grace_secs = 60`(0은 거부, 없으면 기본값), `Member`가 예약 작업 다음에 띄우고 그 다음에 멈춘다.
- **스냅샷 만들기 중 강제 정지.** `vm_action`의 `stop?mode=force`는 끝나지 않은 `SnapshotRevert`가 있을 때만 409 `vm_busy`(`check_no_revert_task`, `VM {name} is being reverted to a snapshot; …`). 만들기·삭제·복제 중에는 받는다. 일반 정지·시작·재부팅·수정은 그대로 막는다. 강제 정지 Task는 전원 Task라 원래 대상별 줄에 서지 않으므로 실행기는 바꾸지 않고 테스트로 확인한다. QEMU는 진행 중인 `snapshot-save`가 끝난 뒤에야 `quit`을 처리하므로(QEMU 11.1에서 확인), 강제 정지는 약 15초(QMP 응답 제한 5초 + 강제 정지 대기 10초) 기다린 뒤 QEMU를 죽인다. 그 안에 끝나는 저장은 끝난 뒤 꺼지고 스냅샷은 성공하며, 그보다 오래 걸리는 저장(멈춘 저장, 아주 큰 메모리)만 끊긴다. 만들기 Task가 실패하면 하이퍼바이저로 VM을 다시 보고, 꺼졌으면(`inspect`가 없음) 꺼진 VM처럼 스토리지(`qemu-img snapshot -d`)로 태그를 지운다. QMP만 끊기고 QEMU가 살아 있으면 qemu-img가 이미지 잠금으로 거부해 태그가 남는다(손상 없음, 로그).
- **예약 오류 code와 기록 규칙.** 예약 작업이 요청 전에 거절하거나 요청이 거절되면 그 API 오류 code와 메시지(`snapshot_unsupported`, `vm_busy`, `node_offline` 등). 예약 만들기 Task가 실패하면 `task_failed`와 Task 오류, 성공하면 지운다. 기록할 Task는 `recorded_vm(task_spec, find)`가 정한다: 예약 스냅샷의 `SnapshotCreate`와 `SnapshotDelete { scheduled: true }`(스냅샷을 찾지 않고 Task 대상 VM으로, 이미 지워졌어도 기록). 사용자가 지운 예약 스냅샷의 삭제 실패는 기록하지 않는다. 같은 code·메시지면 쓰지 않는다. 실행기와 정리 작업이 같은 `recorded_vm`·`task_failed`를 쓴다: 정리 작업은 실패 쓰기가 성공했거나 `revision_mismatch` 뒤 다시 읽어 쓰인 것으로 볼 때 `task_failed`와 자기 오류(`node {id} is offline` 등)를 쓴다(`node_offline`은 예약 작업의 거절 code로 남긴다).
- **CLI.** `oplctl vm snapshot schedule <vm>`는 마지막 실패를 `last error: <code>: <message>`로 보인다.
- **웹.** 강제 정지는 `isReverting(vm, tasks)`(대상이 이 VM이고 끝나지 않은 `snapshot-revert` Task, 서버 `check_no_revert_task`와 같음)일 때만 막고 까닭(`vms.forceStopReverting`)을 `aria-describedby`로 잇는다. 웹은 원래 스냅샷 Task로 강제 정지를 미리 막지 않았고(`canRun`은 phase만 본다) `isBusy`는 그대로다. Task 목록은 VM 메뉴·상세 헤더가 이미 watch하는 것을 쓴다. 예약 오류는 `codeMessage(t, code, message, own?)`(`i18n/errors.ts`, API 오류 표시와 같이 쓴다)로 `<own>.<code>` → `errors.<code>` 순으로 찾아 `{{message}}`를 넘겨 번역하고, 둘 다 없으면(`unknown` 포함) 메시지를 그대로 보인다. 스냅샷 탭은 `own = "snapshots.scheduleError"`이고 전용 문구는 `vm_busy`("…건너뛰었습니다. 다음 기회에 다시 합니다.")와 `node_offline`(메시지의 노드를 괄호에). `errors.task_failed`는 "작업 오류: {{message}}". 메뉴의 비활성 항목은 Radix가 키보드로 건너뛰어 까닭이 읽히지 않을 수 있지만 상세 화면에 같은 까닭이 보인다.

## 파일 구조

```
crates/opl-types/src/{vm.rs, task.rs}          # ScheduleError, VmStatus.schedule_error, SnapshotDelete.scheduled
crates/opl-cluster/src/cluster.rs              # request의 maybe_applied, already_applied, wait_local
crates/opl-cluster/tests/three_nodes.rs        # LoseFirstReply 전달자로 다시 보낸 만들기·삭제
crates/opl-api/src/{janitor.rs(새로), lib.rs, schedule.rs, snapshots.rs, tasks.rs, tasks/snapshot.rs, vms.rs}
crates/opl-api/tests/{janitor.rs(새로), schedules.rs, snapshots.rs, snapshot_tasks.rs}
crates/opl-driver/src/mock.rs                  # hang_next_snapshot_save
crates/opl-reconciler/tests/reconcile.rs
crates/opld/src/{config.rs, member.rs}         # [janitor], ClusterJanitor 띄우고 멈추기
crates/opld/tests/snapshots.rs                 # 메모리 스냅샷 중 강제 정지
crates/oplctl/src/cli.rs                       # last error: <code>: <message>
web/src/i18n/{ko.json, en.json, errors.ts}, web/src/vms/{VmMenu.tsx, VmDetailPage.tsx, VmSnapshots.tsx, actions.ts}
docs/superpowers/specs/{2026-10-27-robustness-design.md, 2026-10-26-snapshots-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 예약 오류 구조와 예약 정리 삭제 표시

`ScheduleError { code, message }`를 더하고 `VmStatus.schedule_error`를 `Option<ScheduleError>`로 바꾼다. 쓸 때는 늘 구조이고, 읽을 때는 옛 문자열도 받아 `{ code: "unknown", message }`로 읽는다(저장된 상태를 옮기지 않는다). `TaskOp::SnapshotDelete`에 `#[serde(default)] scheduled: bool`을 더한다(`TaskOp`는 `deny_unknown_fields`지만 필드가 없는 옛 Task도 읽힌다). 이 Task에서는 쓰는 곳을 모두 `false`로 만들고, 예약 정리만 `true`로 바꾸는 것은 Task 6이다. 쓰는 곳을 맞춘다: 예약 작업의 거절은 API 오류의 code와 메시지를, Task 실패는 `task_failed`와 Task 오류를 쓴다. CLI와 웹은 아직 `message`만 그대로 보인다. reconciler 테스트와 API 테스트의 기대값(사용자 삭제 op의 `"scheduled": false`)도 맞춘다. OpenAPI에 `ScheduleError` 컴포넌트가 생기므로 웹 타입을 다시 만든다.

**Files:**
- Modify: `crates/opl-api/src/schedule.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/schedules.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/schedule.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{ScheduleEvery, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::{ScheduleError, ScheduleEvery, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::oneshot;
```

`crates/opl-api/src/schedule.rs`에서

```rust
                tracing::info!(vm = %vm.name, ?plan, error = %e.message, "scheduled snapshot refused");
                if let Err(w) = set_schedule_error(cluster, vm.id, Some(e.message)).await {
                    tracing::warn!(vm = %vm.name, error = %w, "cannot record schedule error");
```

을 다음으로 바꾼다:

```rust
                tracing::info!(vm = %vm.name, ?plan, error = %e.message, "scheduled snapshot refused");
                let error = ScheduleError {
                    code: e.code.into(),
                    message: e.message,
                };
                if let Err(w) = set_schedule_error(cluster, vm.id, Some(error)).await {
                    tracing::warn!(vm = %vm.name, error = %w, "cannot record schedule error");
```

`crates/opl-api/src/schedule.rs`에서

```rust
    vm: Uuid,
    error: Option<String>,
) -> Result<(), ClusterError> {
```

을 다음으로 바꾼다:

```rust
    vm: Uuid,
    error: Option<ScheduleError>,
) -> Result<(), ClusterError> {
```

`crates/opl-api/src/schedule.rs`에서

```rust
        let t = task(
            TaskOp::SnapshotDelete { snapshot: a.id },
            v.id,
```

을 다음으로 바꾼다:

```rust
        let t = task(
            TaskOp::SnapshotDelete {
                snapshot: a.id,
                scheduled: false,
            },
            v.id,
```

`crates/opl-api/src/schedule.rs`에서

```rust
            TaskOp::SnapshotRevert { snapshot: some },
            TaskOp::SnapshotDelete { snapshot: some },
            TaskOp::VmClone {
```

을 다음으로 바꾼다:

```rust
            TaskOp::SnapshotRevert { snapshot: some },
            TaskOp::SnapshotDelete {
                snapshot: some,
                scheduled: false,
            },
            TaskOp::VmClone {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await
```

을 다음으로 바꾼다:

```rust
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
        scheduled: false,
    };
    create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await
```

`crates/opl-api/src/tasks.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
```

`crates/opl-api/src/tasks.rs`에서

```rust
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(error.clone())).await;
    }
```

을 다음으로 바꾼다:

```rust
    if let Some(vm) = scheduled {
        let failure = ScheduleError {
            code: "task_failed".into(),
            message: error.clone(),
        };
        snapshot::record_schedule(ctx, vm, Some(failure)).await;
    }
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
```

을 다음으로 바꾼다:

```rust
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let (TaskOp::SnapshotCreate { snapshot } | TaskOp::SnapshotDelete { snapshot }) = task.spec.op
    else {
```

을 다음으로 바꾼다:

```rust
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let (TaskOp::SnapshotCreate { snapshot } | TaskOp::SnapshotDelete { snapshot, .. }) =
        task.spec.op
    else {
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
/// 예약 스냅샷 만들기·삭제의 결과를 VM의 `schedule_error`에 남긴다. 성공(`None`)이면 지운다.
pub(super) async fn record_schedule(ctx: &Ctx, vm: Uuid, error: Option<String>) {
    if let Err(e) = set_schedule_error(&ctx.cluster, vm, error).await {
```

을 다음으로 바꾼다:

```rust
/// 예약 스냅샷 만들기·삭제의 결과를 VM의 `schedule_error`에 남긴다. 성공(`None`)이면 지운다.
pub(super) async fn record_schedule(ctx: &Ctx, vm: Uuid, error: Option<ScheduleError>) {
    if let Err(e) = set_schedule_error(&ctx.cluster, vm, error).await {
```

`crates/opl-api/tests/schedules.rs`에서

```rust
            .schedule_error
            .is_some_and(|e| e.contains("non-qcow2"))
    })
```

을 다음으로 바꾼다:

```rust
            .schedule_error
            .is_some_and(|e| e.message.contains("non-qcow2"))
    })
```

`crates/opl-api/tests/schedules.rs`에서

```rust
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("no space"))
    })
    .await;
    until("success", || {
```

을 다음으로 바꾼다:

```rust
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.message.contains("no space"))
    })
    .await;
    until("success", || {
```

`crates/opl-api/tests/schedules.rs`에서

```rust
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
```

을 다음으로 바꾼다:

```rust
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.message.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
```

`crates/opl-api/tests/schedules.rs`에서

```rust
            .schedule_error
            .is_some_and(|e| e.contains("offline"))
    })
```

을 다음으로 바꾼다:

```rust
            .schedule_error
            .is_some_and(|e| e.message.contains("offline"))
    })
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
        task["spec"]["op"],
        json!({ "kind": "snapshot-delete", "snapshot": snap.id })
    );
```

을 다음으로 바꾼다:

```rust
        task["spec"]["op"],
        json!({ "kind": "snapshot-delete", "snapshot": snap.id, "scheduled": false })
    );
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, RestartPolicy, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{
    DiskBus, DiskSpec, PowerState, RestartPolicy, ScheduleError, VmPhase, VmSpec, VmStatus,
};
use uuid::Uuid;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
}

#[tokio::test]
async fn power_off_keeps_current_snapshot() {
```

을 다음으로 바꾼다:

```rust
}

/// 예약 작업이 남긴 오류
fn busy() -> ScheduleError {
    ScheduleError {
        code: "vm_busy".into(),
        message: "vm busy".into(),
    }
}

#[tokio::test]
async fn power_off_keeps_current_snapshot() {
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    vm.status.snapshot = Some(snapshot);
    vm.status.schedule_error = Some("vm busy".into());
    env.create(&vm).await;
```

을 다음으로 바꾼다:

```rust
    vm.status.snapshot = Some(snapshot);
    vm.status.schedule_error = Some(busy());
    env.create(&vm).await;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    assert_eq!(got.status.snapshot, Some(snapshot));
    assert_eq!(got.status.schedule_error.as_deref(), Some("vm busy"));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(got.status.snapshot, Some(snapshot));
    assert_eq!(got.status.schedule_error, Some(busy()));
}
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
        snapshot: Some(snapshot),
        schedule_error: Some("vm busy".into()),
        ..current.status.clone()
```

을 다음으로 바꾼다:

```rust
        snapshot: Some(snapshot),
        schedule_error: Some(busy()),
        ..current.status.clone()
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    assert_eq!(env.get(vm.id).status.snapshot, Some(snapshot));
    assert_eq!(
        env.get(vm.id).status.schedule_error.as_deref(),
        Some("vm busy")
    );
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.get(vm.id).status.snapshot, Some(snapshot));
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}
```

`crates/opl-types/src/task.rs`에서

```rust
    SnapshotDelete {
        snapshot: Uuid,
    },
    /// 볼륨 크기를 늘린다(줄이지 않는다).
```

을 다음으로 바꾼다:

```rust
    SnapshotDelete {
        snapshot: Uuid,
        /// 예약 정리가 만든 삭제면 true. 이 삭제의 실패만 `schedule_error`에 남긴다.
        #[serde(default)]
        scheduled: bool,
    },
    /// 볼륨 크기를 늘린다(줄이지 않는다).
```

`crates/opl-types/src/task.rs`에서

```rust
            (TaskOp::SnapshotRevert { snapshot }, "snapshot-revert"),
            (TaskOp::SnapshotDelete { snapshot }, "snapshot-delete"),
        ] {
```

을 다음으로 바꾼다:

```rust
            (TaskOp::SnapshotRevert { snapshot }, "snapshot-revert"),
        ] {
```

`crates/opl-types/src/task.rs`에서

```rust
        }
        let op = TaskOp::VolumeResize {
```

을 다음으로 바꾼다:

```rust
        }
        let op = TaskOp::SnapshotDelete {
            snapshot,
            scheduled: true,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "snapshot-delete", "snapshot": snapshot, "scheduled": true })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        // 옛 Task에는 scheduled가 없다. 사용자 삭제로 읽는다.
        assert_eq!(
            serde_json::from_value::<TaskOp>(
                serde_json::json!({ "kind": "snapshot-delete", "snapshot": snapshot })
            )
            .unwrap(),
            TaskOp::SnapshotDelete {
                snapshot,
                scheduled: false
            }
        );
        let op = TaskOp::VolumeResize {
```

`crates/opl-types/src/vm.rs`에서

```rust
    /// 마지막 예약 스냅샷이 실패한 이유. 성공하면 지운다.
    pub schedule_error: Option<String>,
}
```

을 다음으로 바꾼다:

```rust
    /// 마지막 예약 스냅샷이 실패한 이유. 성공하면 지운다.
    pub schedule_error: Option<ScheduleError>,
}

/// 예약 스냅샷이 실패한 이유. `code`는 거절한 API 오류의 code(`vm_busy` 등)이고, Task가 실패했으면
/// `task_failed`다. 옛 상태의 문자열은 `{ code: "unknown", message: <문자열> }`로 읽는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(from = "ScheduleErrorRepr")]
pub struct ScheduleError {
    pub code: String,
    pub message: String,
}

/// [`ScheduleError`]가 읽는 형식: 지금의 구조나 옛 문자열
#[derive(Deserialize)]
#[serde(untagged)]
enum ScheduleErrorRepr {
    Current { code: String, message: String },
    Old(String),
}

impl From<ScheduleErrorRepr> for ScheduleError {
    fn from(r: ScheduleErrorRepr) -> Self {
        match r {
            ScheduleErrorRepr::Current { code, message } => ScheduleError { code, message },
            ScheduleErrorRepr::Old(message) => ScheduleError {
                code: "unknown".into(),
                message,
            },
        }
    }
}
```

`crates/opl-types/src/vm.rs`에서

```rust
    #[test]
    fn snapshot_schedule_defaults_to_none() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn schedule_error_reads_old_string_and_writes_structure() {
        let e = ScheduleError {
            code: "vm_busy".into(),
            message: "VM web is busy".into(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "code": "vm_busy", "message": "VM web is busy" })
        );
        assert_eq!(serde_json::from_value::<ScheduleError>(json).unwrap(), e);
        // 옛 상태는 문자열이었다. code를 모르므로 unknown으로 읽는다.
        let st: VmStatus =
            serde_json::from_value(serde_json::json!({ "schedule_error": "VM web is busy" }))
                .unwrap();
        assert_eq!(
            st.schedule_error,
            Some(ScheduleError {
                code: "unknown".into(),
                message: "VM web is busy".into(),
            })
        );
        assert_eq!(
            serde_json::to_value(&st).unwrap()["schedule_error"],
            serde_json::json!({ "code": "unknown", "message": "VM web is busy" })
        );
    }

    #[test]
    fn snapshot_schedule_defaults_to_none() {
```

`crates/oplctl/src/cli.rs`에서

```rust
    };
    if let Some(e) = vm["status"]["schedule_error"].as_str() {
        text.push_str(&format!("last error: {e}\n"));
```

을 다음으로 바꾼다:

```rust
    };
    if let Some(e) = vm["status"]["schedule_error"]["message"].as_str() {
        text.push_str(&format!("last error: {e}\n"));
```

`crates/oplctl/src/cli.rs`에서

```rust
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!("VM web is busy")
            )),
```

을 다음으로 바꾼다:

```rust
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
    status: { ...web.status, schedule_error: failure },
  };
```

을 다음으로 바꾼다:

```tsx
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
    status: { ...web.status, schedule_error: { code: "vm_busy", message: failure } },
  };
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
            <span className="font-medium">{t("snapshots.scheduleFailed")}</span>{" "}
            <span className="whitespace-pre-wrap break-words">{vm.status.schedule_error}</span>
          </p>
```

을 다음으로 바꾼다:

```tsx
            <span className="font-medium">{t("snapshots.scheduleFailed")}</span>{" "}
            <span className="whitespace-pre-wrap break-words">
              {vm.status.schedule_error.message}
            </span>
          </p>
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 338개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 220개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): 예약 오류를 code·메시지 구조로, 스냅샷 삭제 Task에 예약 정리 표시"
```

---

### Task 2: 전달 재시도의 결과 판단

`Cluster::request`가 리더에 전달한 시도가 시도 제한 시간(`forward attempt timed out`)에 걸리거나 전달자가 `Unreachable`을 돌려주면 `maybe_applied`로 표시한다(연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 `Unreachable`은 늘 표시한다). 표시 뒤의 결과는 비공개 `already_applied`가 판단한다: 다시 보낸 `Create`가 같은 id로 `AlreadyExists`면 `wait_local`로 이 노드에 보일 때까지 기다려 지금 revision을, `Delete`가 `NotFound`면 사라질 때까지 기다려 이 노드가 적용한 인덱스를 돌려주고, 남은 제한 시간 안에 안 되면 `Timeout`이다. 조건부 수정 등은 판단하지 않는다(거부를 그대로 돌려준다). 공개 API와 Raft 로그 형식은 바뀌지 않는다. 테스트는 `three_nodes.rs`의 `LoseFirstReply` 전달자(리더에서 실행한 뒤 응답을 버리거나 멈춘다. 안쪽 전달이 성공했을 때만 `lost`를 표시해, 첫 전달이 `NotLeader` 등으로 실패하면 다음 전달에서 다시 잃는다)로 응답을 잃은 만들기, 시도 제한 시간에 걸린 만들기(약 3초), 삭제, 조건부 수정이 여전히 `RevisionMismatch`인지를 보고, 전달자가 실제로 응답을 잃었는지 단언한다.

**Files:**
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/tests/three_nodes.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-cluster/src/cluster.rs`에서

```rust
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Resource, ResourceSpec};
use redb::Database;
```

을 다음으로 바꾼다:

```rust
use openraft::{BasicNode, Raft, RaftNetworkFactory};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use redb::Database;
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    /// 리더가 없거나(선거 중) 닿지 않으면 `write_timeout`까지 다시 시도하고, 그래도 안 되면
    /// `Timeout`이다. 전달이 실패해 다시 보낸 요청이 이미 적용된 것이었다면 두 번째는 거부될 수 있다.
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + self.write_timeout;
        loop {
```

을 다음으로 바꾼다:

```rust
    /// 리더가 없거나(선거 중) 닿지 않으면 `write_timeout`까지 다시 시도하고, 그래도 안 되면
    /// `Timeout`이다. 전달 시도가 응답 없이 끝나면(시도 제한 시간, 보낸 뒤 연결 끊김) 요청이 이미
    /// 적용됐을 수 있으므로, 그 뒤 다시 보낸 만들기·삭제의 거부는 `already_applied`로 판단한다.
    pub async fn request(&self, req: LeaderRequest) -> Result<u64, ClusterError> {
        let deadline = tokio::time::Instant::now() + self.write_timeout;
        // 응답 없이 끝난 전달 시도가 있었다(리더가 처리했을 수 있다).
        let mut maybe_applied = false;
        loop {
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
                        // 이번 시도만 제한에 걸렸다. 리더를 다시 읽고 다시 보낸다.
                        Err(_) => Err(ClusterError::Unreachable(
                            "forward attempt timed out".into(),
                        )),
                        // 자기 자신을 뺀 요청이면 리더가 더는 이 노드에 복제하지 않으므로
```

을 다음으로 바꾼다:

```rust
                        // 이번 시도만 제한에 걸렸다. 리더를 다시 읽고 다시 보낸다.
                        Err(_) => {
                            maybe_applied = true;
                            Err(ClusterError::Unreachable(
                                "forward attempt timed out".into(),
                            ))
                        }
                        // 자기 자신을 뺀 요청이면 리더가 더는 이 노드에 복제하지 않으므로
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
                        }
                        Ok(Err(e)) => Err(e),
                    }
```

을 다음으로 바꾼다:

```rust
                        }
                        // 연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 둘 다 적용됐을 수 있다고 본다.
                        Ok(Err(e)) => {
                            maybe_applied |= matches!(e, ClusterError::Unreachable(_));
                            Err(e)
                        }
                    }
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            };
            let retry = match &result {
```

을 다음으로 바꾼다:

```rust
            };
            if maybe_applied && let Some(done) = self.already_applied(&req, &result, deadline).await
            {
                return done;
            }
            let retry = match &result {
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
```

을 다음으로 바꾼다:

```rust
            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }

    /// 다시 보낸 요청의 거부가 앞서 응답 없이 끝난 같은 요청이 적용된 결과면, 이 노드에도 적용되기를
    /// 기다려 성공으로 돌려준다. 만들기의 id는 요청한 쪽이 만든 UUID v7이라 같은 id가 있으면 자기
    /// 요청이다. 조건부 수정 등은 거부가 자기 요청 때문인지 알 수 없어 판단하지 않는다(`None`, 다시
    /// 보낸 요청이 `RevisionMismatch`로 끝날 수 있다).
    async fn already_applied(
        &self,
        req: &LeaderRequest,
        result: &Result<u64, ClusterError>,
        deadline: tokio::time::Instant,
    ) -> Option<Result<u64, ClusterError>> {
        let LeaderRequest::Write { command } = req else {
            return None;
        };
        match (command, result) {
            (
                Command::Create { kind, resource },
                Err(ClusterError::Rejected(Rejection::AlreadyExists { id, .. })),
            ) if *id == resource.id => Some(self.wait_local(*kind, *id, true, deadline).await),
            (
                Command::Delete { kind, id, .. },
                Err(ClusterError::Rejected(Rejection::NotFound { .. })),
            ) => Some(self.wait_local(*kind, *id, false, deadline).await),
            _ => None,
        }
    }

    /// 이 노드에서 리소스가 보이거나(`present`) 사라질 때까지 기다린다. 보이면 그 revision을,
    /// 사라지면 이 노드가 적용한 인덱스를 돌려준다. `deadline`까지 안 되면 `Timeout`이다.
    /// 한 번에 적용된 묶음에 다른 쪽의 삭제까지 들어 있으면 "있음"을 보지 못하고 `Timeout`
    /// (=적용됐을 수 있음)으로 끝난다.
    async fn wait_local(
        &self,
        kind: Kind,
        id: Uuid,
        present: bool,
        deadline: tokio::time::Instant,
    ) -> Result<u64, ClusterError> {
        let db = self.db.clone();
        let metrics = self
            .raft
            .wait(Some(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            ))
            .metrics(
                move |_| get_object(&db, kind, id).is_ok_and(|r| r.is_some() == present),
                "retried request applied",
            )
            .await
            .map_err(|_| ClusterError::Timeout)?;
        match get_object(&self.db, kind, id)? {
            Some(r) if present => Ok(r.revision),
            _ => Ok(metrics.last_applied.map_or(0, |l| l.index)),
        }
```

`crates/opl-cluster/tests/three_nodes.rs`에서

```rust
}

#[tokio::test]
async fn concurrent_membership_changes_wait_for_each_other() {
```

을 다음으로 바꾼다:

```rust
}

/// 첫 전달은 리더가 처리하지만 응답이 오지 않는다. `hang`이면 시도 제한 시간까지 붙잡고,
/// 아니면 보낸 뒤 연결이 끊긴 것처럼 `Unreachable`을 돌려준다. 리더가 처리하지 못한 전달
/// (`NotLeader` 등)은 그대로 돌려주고 다음 전달에서 다시 잃는다(`lost`는 실제로 잃었을 때만).
struct LoseFirstReply {
    lost: AtomicBool,
    hang: bool,
    inner: Arc<MemForwarder>,
}

#[async_trait]
impl Forwarder for LoseFirstReply {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let result = self.inner.forward(leader, addr, req).await;
        if result.is_err() || self.lost.swap(true, Ordering::SeqCst) {
            return result;
        }
        if self.hang {
            std::future::pending::<()>().await;
        }
        Err(ClusterError::Unreachable("connection reset".into()))
    }
}

/// 리더가 아닌 노드 하나의 전달자를 첫 응답을 잃는 것으로 바꾸고 그 노드와 전달자를 돌려준다.
async fn follower_losing_first_reply(
    tc: &mut TestCluster,
    hang: bool,
) -> (NodeId, Arc<LoseFirstReply>) {
    let leader = tc.leader().await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let forwarder = Arc::new(LoseFirstReply {
        lost: AtomicBool::new(false),
        hang,
        inner: tc.router.forwarder(follower),
    });
    let cluster = tc.nodes.remove(&follower).unwrap();
    let cluster = cluster.with_forwarder(forwarder.clone());
    tc.nodes.insert(follower, cluster);
    (follower, forwarder)
}

#[tokio::test]
async fn retried_create_after_a_lost_reply_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    // 다시 보낸 요청의 `AlreadyExists`는 첫 요청이 적용된 결과이므로 그 revision으로 성공한다.
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
}

#[tokio::test]
async fn retried_create_after_a_timed_out_attempt_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, true).await;

    let v = volume("disk-a");
    let rev = tc.get(follower).create(&v).await.unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    let got = tc.get(follower).get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
}

#[tokio::test]
async fn retried_delete_after_a_lost_reply_succeeds() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let v = volume("disk-a");
    let leader = tc.leader().await;
    let rev = tc.get(leader).create(&v).await.unwrap();
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;
    tc.wait_applied(follower, rev).await;

    // 다시 보낸 요청의 `NotFound`는 첫 요청이 지운 결과다. 이 노드에서도 곧바로 없다.
    tc.get(follower)
        .delete::<VolumeSpec>(v.id, Some(rev))
        .await
        .unwrap();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    assert!(tc.get(follower).get::<VolumeSpec>(v.id).unwrap().is_none());
}

#[tokio::test]
async fn retried_conditional_update_after_a_lost_reply_is_still_rejected() {
    let mut tc = TestCluster::start_forwarding(3).await;
    let v = volume("disk-a");
    let leader = tc.leader().await;
    let rev = tc.get(leader).create(&v).await.unwrap();
    let (follower, forwarder) = follower_losing_first_reply(&mut tc, false).await;
    tc.wait_applied(follower, rev).await;

    // 조건부 수정은 거부가 자기 요청 때문인지 알 수 없어 그대로 돌려준다.
    let err = tc
        .get(follower)
        .update_spec(v.id, Some(rev), v.labels.clone(), &v.spec)
        .await
        .unwrap_err();
    assert!(forwarder.lost.load(Ordering::SeqCst));
    assert!(
        matches!(
            err,
            ClusterError::Rejected(Rejection::RevisionMismatch { .. })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn concurrent_membership_changes_wait_for_each_other() {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 342개, 실패 0

Run: `cargo test -p opl-cluster --test three_nodes`
Expected: PASS 13개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(cluster): 응답 없이 끝난 전달 뒤 다시 보낸 만들기·삭제를 성공으로 판단"
```

---

### Task 3: 조건부 Task 시작

실행기의 `execute`가 무조건 `running`을 쓰는 대신 비공개 `begin(ctx, id) -> bool`을 부른다. Task를 이 노드에서 다시 읽어 `pending`이면 그 revision으로 `update_status_checked`(계획 10)를 써서 `running`으로 바꾸고, 이미 `running`이면 실행한다(앞선 쓰기의 응답만 잃은 경우, `running`은 그 노드만 쓴다). `succeeded`·`failed`이거나 지워졌으면(`NotFound` 거부 포함) 실행하지 않는다. 그 밖의 오류(`RevisionMismatch` 포함)는 `poll_interval` 뒤 다시 읽어 판단한다. 중단 경로와 그 뒤의 상태 쓰기는 그대로(조건 없음)다. Task 4의 정리 작업이 실패로 끝낸 Task를 노드가 실행하지 않게 하려는 것이다. 테스트: 멈춰 둔 Task 뒤에 줄 선 볼륨 크기 늘리기 Task를 시작 전에 실패로 쓰면, 실패로 남고 크기 늘리기가 한 번만 불린다.

**Files:**
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
async fn execute(ctx: &Ctx, task: Task) {
    set_status(ctx, task.id, TaskState::Running, None).await;
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
```

을 다음으로 바꾼다:

```rust
async fn execute(ctx: &Ctx, task: Task) {
    if !begin(ctx, task.id).await {
        return;
    }
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
```

`crates/opl-api/src/tasks.rs`에서

```rust
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

fn vm_spec(node: Uuid) -> VmSpec {
```

을 다음으로 바꾼다:

```rust

#[tokio::test]
async fn task_failed_by_someone_else_before_it_starts_is_not_run() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;

    storage.pause();
    let (status, first) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    storage.reached().await;
    // 같은 줄에서 앞의 Task를 기다리는 Task를, 시작하기 전에 다른 쪽(리더의 정리 작업)이 실패로 끝낸다.
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut second: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 3 << 20,
            },
            target: root.id,
            node: env.node,
        },
    );
    second.name = second.id.to_string();
    env.cluster.create(&second).await.unwrap();
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some(format!("node {} is offline", env.node)),
    };
    env.cluster
        .update_status::<TaskSpec>(second.id, &failed)
        .await
        .unwrap();
    storage.resume();

    succeeded(&admin.wait_task(&first).await);
    // 뒤의 Task가 돌았다면 이미 크기를 바꾸고 끝났을 시간을 기다린다.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let task = env.cluster.get::<TaskSpec>(second.id).unwrap().unwrap();
    assert_eq!(task.status, failed);
    let resized = storage
        .calls()
        .iter()
        .filter(|(c, _)| *c == "resize")
        .count();
    assert_eq!(resized, 1);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!(root.spec.size_bytes, 2 << 20);
}

fn vm_spec(node: Uuid) -> VmSpec {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 343개, 실패 0

Run: `cargo test -p opl-api --test snapshot_tasks`
Expected: PASS 13개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): Task 시작을 조건부로 써서 그새 실패로 끝난 Task는 실행하지 않기"
```

---

### Task 4: 클러스터 정리 작업 ClusterJanitor

`opl-api/src/janitor.rs`의 `ClusterJanitor { spawn(state, config), shutdown() }`는 `SnapshotScheduler`와 같은 모양으로 리더에서 `tick`마다 리더 확인 → `read_barrier` → 노드·Task·스냅샷 목록과 Raft 멤버십을 읽는다. 판단은 순수 함수 `decide(now, config, nodes, members, tasks, snapshots, &mut seen) -> Vec<Action>`이다. `offline_grace`보다 오래 오프라인인 노드의 `pending` Task를 실패로(`node {id} is offline`, 온라인이 되면 다시 잰다), 노드 목록에도 Raft 멤버십에도 없는 노드의 `pending`·`running` Task를 곧바로 실패로(`node {id} was removed`), 끝나지 않은 `SnapshotCreate` Task 없는 `pending` 스냅샷을 처음 본 뒤 `orphan_grace`가 지나도 그대로면 지운다(그새 Task가 보이면 다시 잰다). `ClusterJanitorConfig { tick: 10초, offline_grace: 60초, orphan_grace: 30초 }`. 쓰기(비공개 `apply`)는 읽은 revision으로만 하고(`update_status_checked`, `delete(Some(revision))`), 실패하면 로그만 남기고 다음 tick에 다시 본다. 리더가 아니면 기억한 시각을 지운다. opld 설정 `[janitor] tick_secs = 10, offline_grace_secs = 60`(0은 거부, `orphan_grace`는 설정에 두지 않는다), `Member`가 예약 작업 다음에 띄우고 그 다음에 멈춘다. 단위 테스트(유예 전·후, 다시 온라인, 제거된 노드, 100ms tick으로 30초 동안 지우지 않다가 유예에 지움, 한 노드 메모리 클러스터에서 실행기가 먼저 `running`을 쓴 Task에 옛 revision의 실패 쓰기가 `RevisionMismatch`로 거부됨)와 mock 클러스터 테스트(오프라인 노드의 `pending` 스냅샷 Task가 유예 뒤 실패하고 `vm_busy`가 풀리며 남은 `pending` 스냅샷이 지워지고 `running`은 남음, 제거된 노드, 살아 있는 만들기 Task의 `pending` 스냅샷은 남음).

**Files:**
- Create: `crates/opl-api/src/janitor.rs`
- Create: `crates/opl-api/tests/janitor.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs`를 만든다:

```rust
//! 클러스터 정리 작업(견고성 설계 §3). 리더만 `tick`마다 노드·Task·스냅샷 목록을 보고, 오프라인이
//! 유예보다 오래 간 노드의 `pending` Task와 제거된 노드의 끝나지 않은 Task를 실패로 끝내고, 만들
//! Task 없이 남은 `pending` 스냅샷을 지운다. 실패로 끝낸 Task의 정리(볼륨·태그)는 하지 않는다.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct ClusterJanitorConfig {
    /// 목록을 확인하는 간격
    pub tick: Duration,
    /// 노드가 이보다 오래 오프라인이면 그 노드의 `pending` Task를 실패로 끝낸다.
    pub offline_grace: Duration,
    /// 만들 Task 없이 `pending`인 스냅샷을 처음 본 뒤 이만큼 지나도 그대로면 지운다. API는 스냅샷을
    /// 만든 뒤 Task를 만들므로 그 사이(전달된 쓰기는 하나에 최대 `write_timeout`)보다 길어야 한다.
    pub orphan_grace: Duration,
}

impl Default for ClusterJanitorConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            offline_grace: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(30),
        }
    }
}

/// 클러스터 정리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct ClusterJanitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ClusterJanitor {
    pub fn spawn(state: Arc<AppState>, config: ClusterJanitorConfig) -> Self {
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
    config: ClusterJanitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut seen = Seen::default();
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
            _ = tick(&state, &config, &mut seen) => {}
        }
    }
}

/// 이전 tick들에서 본 것. 메모리에만 두므로 리더가 바뀌면 유예를 다시 잰다.
#[derive(Debug, Default)]
struct Seen {
    /// 노드가 오프라인으로 처음 보인 시각
    offline_since: HashMap<Uuid, Instant>,
    /// 만들 Task 없이 `pending`인 스냅샷과 처음 그렇게 보인 시각
    orphans: HashMap<Uuid, Instant>,
}

/// 정리 작업이 할 일. 읽은 revision에서만 쓴다(그사이 바뀌었으면 다음 tick에 다시 본다).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    FailTask {
        id: Uuid,
        revision: u64,
        error: String,
    },
    DeleteSnapshot {
        id: Uuid,
        revision: u64,
    },
}

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십의 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
fn decide(
    now: Instant,
    config: &ClusterJanitorConfig,
    nodes: &[Node],
    members: &BTreeSet<Uuid>,
    tasks: &[Task],
    snapshots: &[Snapshot],
    seen: &mut Seen,
) -> Vec<Action> {
    // 다시 온라인이 되었거나 목록에서 빠진 노드는 잊는다.
    seen.offline_since
        .retain(|id, _| nodes.iter().any(|n| n.id == *id && !n.status.online));
    for node in nodes.iter().filter(|n| !n.status.online) {
        seen.offline_since.entry(node.id).or_insert(now);
    }
    let mut actions = vec![];
    for task in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let node = task.spec.node;
        let error = match nodes.iter().find(|n| n.id == node) {
            None if members.contains(&node) => continue,
            // 제거된 노드는 돌아오지 않으므로 실행 중이던 Task도 끝낸다.
            None => format!("node {node} was removed"),
            // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 두고, 시작 전인
            // Task만 유예 뒤에 끝낸다.
            Some(_) if task.status.state != TaskState::Pending => continue,
            Some(_) => match seen.offline_since.get(&node) {
                Some(since) if now.duration_since(*since) >= config.offline_grace => {
                    format!("node {node} is offline")
                }
                _ => continue,
            },
        };
        actions.push(Action::FailTask {
            id: task.id,
            revision: task.revision,
            error,
        });
    }
    // API는 스냅샷을 만든 뒤에 Task를 만들므로 그 사이를 고아로 보지 않도록 처음 본 뒤 유예가
    // 지나도 고아일 때 지운다. 그 사이 Task가 보였으면 다시 잰다.
    let orphans: HashMap<Uuid, Instant> = snapshots
        .iter()
        .filter(|s| s.status.phase == SnapshotPhase::Pending)
        .filter(|s| {
            !tasks.iter().any(|t| {
                !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id)
            })
        })
        .map(|s| (s.id, seen.orphans.get(&s.id).copied().unwrap_or(now)))
        .collect();
    for s in snapshots.iter() {
        if orphans
            .get(&s.id)
            .is_some_and(|since| now.duration_since(*since) >= config.orphan_grace)
        {
            actions.push(Action::DeleteSnapshot {
                id: s.id,
                revision: s.revision,
            });
        }
    }
    seen.orphans = orphans;
    actions
}

async fn tick(state: &AppState, config: &ClusterJanitorConfig, seen: &mut Seen) {
    let cluster = &state.cluster;
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        *seen = Seen::default();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
        ))
    })();
    let Ok((nodes, tasks, snapshots)) = lists else {
        return;
    };
    let ids: BTreeSet<Uuid> = members
        .voters
        .keys()
        .chain(members.learners.keys())
        .map(|id| id.0)
        .collect();
    let actions = decide(
        Instant::now(),
        config,
        &nodes,
        &ids,
        &tasks,
        &snapshots,
        seen,
    );
    for action in actions {
        let result = apply(cluster, &action).await;
        // 그사이 바뀌었으면(실행기가 시작했다 등) 다음 tick에 다시 본다.
        match result {
            Ok(_) => tracing::info!(?action, "janitor"),
            Err(e) => tracing::info!(?action, error = %e, "janitor action not applied"),
        }
    }
}

/// 할 일을 읽은 revision에서만 쓴다.
async fn apply(cluster: &Cluster, action: &Action) -> Result<u64, ClusterError> {
    match action {
        Action::FailTask {
            id,
            revision,
            error,
        } => {
            let status = TaskStatus {
                state: TaskState::Failed,
                error: Some(error.clone()),
            };
            cluster
                .update_status_checked::<TaskSpec>(*id, *revision, &status)
                .await
        }
        Action::DeleteSnapshot { id, revision } => {
            cluster.delete::<SnapshotSpec>(*id, Some(*revision)).await
        }
    }
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::vm::{CpuSpec, MachineType, PowerState, VmSpec};

    use super::*;

    const GRACE: Duration = Duration::from_secs(60);
    const ORPHAN_GRACE: Duration = Duration::from_secs(30);
    const CONFIG: ClusterJanitorConfig = ClusterJanitorConfig {
        tick: Duration::from_secs(10),
        offline_grace: GRACE,
        orphan_grace: ORPHAN_GRACE,
    };

    fn node(online: bool) -> Node {
        let mut n: Node = Resource::new(
            "n",
            NodeSpec {
                address: "h:1".into(),
                maintenance: false,
            },
        );
        n.status = NodeStatus {
            online,
            ..Default::default()
        };
        n
    }

    fn task(op: TaskOp, node: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node,
            },
        );
        t.revision = 7;
        t.status = TaskStatus { state, error: None };
        t
    }

    fn pending_snapshot() -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: Uuid::now_v7(),
                name: "s".into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: false,
                vm_spec: VmSpec {
                    cpu: CpuSpec {
                        sockets: 1,
                        cores: 1,
                        model: Default::default(),
                    },
                    memory_mib: 256,
                    firmware: Default::default(),
                    machine: MachineType::Virt,
                    disks: vec![],
                    nics: vec![],
                    power: PowerState::Stopped,
                    restart: Default::default(),
                    node: None,
                    snapshot_schedule: None,
                },
            },
        );
        s.revision = 3;
        s.status = SnapshotStatus::default();
        s
    }

    fn fail(t: &Task, error: String) -> Action {
        Action::FailTask {
            id: t.id,
            revision: t.revision,
            error,
        }
    }

    #[test]
    fn pending_tasks_of_an_offline_node_fail_after_the_grace() {
        let (up, down) = (node(true), node(false));
        let nodes = [up.clone(), down.clone()];
        let members = BTreeSet::from([up.id, down.id]);
        let pending = task(TaskOp::VmStart, down.id, TaskState::Pending);
        let running = task(TaskOp::VmStop, down.id, TaskState::Running);
        let done = task(TaskOp::VmStart, down.id, TaskState::Failed);
        let elsewhere = task(TaskOp::VmStart, up.id, TaskState::Pending);
        let tasks = [pending.clone(), running, done, elsewhere];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, &nodes, &members, &tasks, &[], &mut seen);
        // 처음 오프라인으로 본 시각에서 유예를 잰다.
        assert_eq!(at(t0), vec![]);
        assert_eq!(at(t0 + GRACE - Duration::from_secs(1)), vec![]);
        // `running`은 건드리지 않는다.
        let error = format!("node {} is offline", down.id);
        assert_eq!(at(t0 + GRACE), vec![fail(&pending, error)]);
    }

    #[test]
    fn coming_back_online_restarts_the_grace() {
        let mut n = node(false);
        let members = BTreeSet::from([n.id]);
        let pending = task(TaskOp::VmStart, n.id, TaskState::Pending);
        let tasks = std::slice::from_ref(&pending);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let half = GRACE / 2;
        assert_eq!(
            decide(
                t0,
                &CONFIG,
                std::slice::from_ref(&n),
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        n.status.online = true;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + half, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert!(seen.offline_since.is_empty());
        // 다시 오프라인이 된 때부터 잰다.
        n.status.online = false;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + GRACE, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + GRACE + half,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + 2 * GRACE,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            )
            .len(),
            1
        );
    }

    #[test]
    fn unfinished_tasks_of_a_removed_node_fail_at_once() {
        let up = node(true);
        let gone = Uuid::now_v7();
        let pending = task(TaskOp::VmStart, gone, TaskState::Pending);
        let running = task(TaskOp::VmStop, gone, TaskState::Running);
        let done = task(TaskOp::VmStop, gone, TaskState::Succeeded);
        let tasks = [pending.clone(), running.clone(), done];
        let error = format!("node {gone} was removed");
        let mut seen = Seen::default();
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![fail(&pending, error.clone()), fail(&running, error)]
        );
        // 멤버십에 있으면 아직 노드를 등록하지 않은 새 노드다.
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id, gone]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
    }

    #[test]
    fn pending_snapshots_without_a_create_task_go_after_the_orphan_grace() {
        let up = node(true);
        let nodes = std::slice::from_ref(&up);
        let members = BTreeSet::from([up.id]);
        let (orphan, busy, mut ready) =
            (pending_snapshot(), pending_snapshot(), pending_snapshot());
        ready.status.phase = SnapshotPhase::Ready;
        let create =
            |s: &Snapshot, state| task(TaskOp::SnapshotCreate { snapshot: s.id }, up.id, state);
        // 끝난 만들기 Task는 세지 않는다.
        let tasks = [
            create(&busy, TaskState::Running),
            create(&orphan, TaskState::Failed),
        ];
        let snapshots = [orphan.clone(), busy, ready];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, nodes, &members, &tasks, &snapshots, &mut seen);
        // tick이 짧아도 처음 본 뒤 유예가 지나기 전에는 지우지 않는다.
        for ms in (0..ORPHAN_GRACE.as_millis() as u64).step_by(100) {
            assert_eq!(at(t0 + Duration::from_millis(ms)), vec![], "{ms}ms");
        }
        let delete = Action::DeleteSnapshot {
            id: orphan.id,
            revision: orphan.revision,
        };
        assert_eq!(at(t0 + ORPHAN_GRACE), vec![delete]);

        // 고아로 보인 뒤 Task가 보이면 지우지 않고, 다시 고아가 된 때부터 잰다.
        let mut seen = Seen::default();
        let snapshots = std::slice::from_ref(&orphan);
        let started = [create(&orphan, TaskState::Pending)];
        let mut at = |now, tasks: &[Task]| {
            decide(now, &CONFIG, nodes, &members, tasks, snapshots, &mut seen)
        };
        assert_eq!(at(t0, &[]), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &started), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &[]), vec![]);
        assert_eq!(
            at(t0 + 2 * ORPHAN_GRACE - Duration::from_secs(1), &[]),
            vec![]
        );
        assert_eq!(at(t0 + 2 * ORPHAN_GRACE, &[]).len(), 1);
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    #[tokio::test]
    async fn failing_a_task_the_executor_started_meanwhile_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        // 정리 작업이 `pending`으로 읽은 뒤 실행기가 먼저 시작했다.
        let action = fail(&t, "node x is offline".into());
        let running = TaskStatus {
            state: TaskState::Running,
            error: None,
        };
        cluster
            .update_status_checked::<TaskSpec>(t.id, t.revision, &running)
            .await
            .unwrap();
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClusterError::Rejected(opl_cluster::types::Rejection::RevisionMismatch { .. })
            ),
            "{err:?}"
        );
        let got = cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
        assert_eq!(got.status, running);
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod error;
pub mod resources;
```

을 다음으로 바꾼다:

```rust
pub mod error;
mod janitor;
pub mod resources;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
```

을 다음으로 바꾼다:

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
```

`crates/opl-api/tests/janitor.rs`를 만든다:

```rust
//! 클러스터 정리 작업: 오프라인 노드의 `pending` 스냅샷 Task가 유예 뒤 실패해 VM 조작이 풀리고
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지.

mod common;

use std::time::{Duration, Instant};

use common::{Env, WAIT, vm_request};
use opl_api::{ClusterJanitor, ClusterJanitorConfig};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::Value;
use uuid::Uuid;

const GRACE: Duration = Duration::from_millis(500);

/// 50ms마다 보고 유예(오프라인·고아 스냅샷)가 0.5초인 정리 작업
fn fast_janitor(env: &Env) -> ClusterJanitor {
    ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            offline_grace: GRACE,
            orphan_grace: GRACE,
        },
    )
}

/// 노드 `name`을 저장소에 직접 만든다(멤버십에는 없다).
async fn other_node(env: &Env, name: &str, online: bool) -> Uuid {
    let mut node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    env.cluster.create(&node).await.unwrap();
    node.status = NodeStatus {
        online,
        ..Default::default()
    };
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    node.id
}

/// VM `vm`의 `pending` 스냅샷과 노드 `node`에 배정된 만들기 Task를 직접 만든다.
async fn snapshot_task(env: &Env, vm: Uuid, name: &str, node: Uuid, state: TaskState) -> Task {
    let spec = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec;
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: name.into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: spec,
        },
    );
    snapshot.name = snapshot.id.to_string();
    env.cluster.create(&snapshot).await.unwrap();
    task(
        env,
        TaskOp::SnapshotCreate {
            snapshot: snapshot.id,
        },
        vm,
        node,
        state,
    )
    .await
}

async fn task(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) -> Task {
    let mut t: Task = Resource::new("", TaskSpec { op, target, node });
    t.name = t.id.to_string();
    env.cluster.create(&t).await.unwrap();
    if state != TaskState::Pending {
        t.status = TaskStatus { state, error: None };
        env.cluster
            .update_status::<TaskSpec>(t.id, &t.status)
            .await
            .unwrap();
    }
    t
}

fn get_task(env: &Env, id: Uuid) -> Task {
    env.cluster.get::<TaskSpec>(id).unwrap().unwrap()
}

fn snapshot_ids(env: &Env) -> Vec<Uuid> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items.into_iter().map(|s| s.id).collect()
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
async fn offline_node_tasks_fail_after_the_grace_and_free_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let away = other_node(&env, "node2", false).await;
    let pending = snapshot_task(&env, vm, "p", away, TaskState::Pending).await;
    // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 남는다(다른 VM 대상).
    let running = task(
        &env,
        TaskOp::VmStart,
        Uuid::now_v7(),
        away,
        TaskState::Running,
    )
    .await;

    // 끝나지 않은 스냅샷 Task가 VM 조작을 막는다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    let janitor = fast_janitor(&env);
    let started = Instant::now();
    until("offline failure", || {
        get_task(&env, pending.id).status.state == TaskState::Failed
    })
    .await;
    assert!(started.elapsed() >= GRACE, "{:?}", started.elapsed());
    assert_eq!(
        get_task(&env, pending.id).status.error,
        Some(format!("node {away} is offline"))
    );
    // Task 없이 남은 pending 스냅샷도 지운다.
    until("orphan snapshot", || snapshot_ids(&env).is_empty()).await;
    janitor.shutdown().await;
    assert_eq!(get_task(&env, running.id).status.state, TaskState::Running);

    // VM 조작이 풀린다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn removed_node_tasks_fail_at_once() {
    let env = Env::start().await;
    let gone = Uuid::now_v7();
    let target = Uuid::now_v7();
    let pending = task(&env, TaskOp::VmStart, target, gone, TaskState::Pending).await;
    let running = task(&env, TaskOp::VmStop, target, gone, TaskState::Running).await;
    // 유예가 길어도 제거된 노드는 기다리지 않는다.
    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            offline_grace: Duration::from_secs(3600),
            orphan_grace: Duration::from_secs(3600),
        },
    );
    let error = Some(format!("node {gone} was removed"));
    until("removed", || {
        [&pending, &running].iter().all(|t| {
            let t = get_task(&env, t.id);
            t.status.state == TaskState::Failed && t.status.error == error
        })
    })
    .await;
    janitor.shutdown().await;
}

#[tokio::test]
async fn pending_snapshots_with_a_live_task_stay() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    // 온라인 노드(이 노드는 아니다)의 끝나지 않은 만들기 Task가 있으면 지우지 않는다.
    let node = other_node(&env, "node2", true).await;
    let live = snapshot_task(&env, vm, "live", node, TaskState::Pending).await;
    let janitor = fast_janitor(&env);
    tokio::time::sleep(GRACE).await;
    janitor.shutdown().await;
    assert_eq!(snapshot_ids(&env).len(), 1);
    assert_eq!(get_task(&env, live.id).status.state, TaskState::Pending);
}
```

`crates/opld/src/config.rs` 전체:

```rust
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_api::{ClusterJanitorConfig, SnapshotSchedulerConfig};
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
    #[serde(default)]
    pub janitor: JanitorSection,
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

/// 클러스터 정리 작업. 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JanitorSection {
    /// 목록을 확인하는 간격(초)
    #[serde(default = "default_janitor_tick_secs")]
    pub tick_secs: u64,
    /// 오프라인 노드의 `pending` Task를 실패로 끝내기까지 기다리는 시간(초)
    #[serde(default = "default_offline_grace_secs")]
    pub offline_grace_secs: u64,
}

impl Default for JanitorSection {
    fn default() -> Self {
        Self {
            tick_secs: default_janitor_tick_secs(),
            offline_grace_secs: default_offline_grace_secs(),
        }
    }
}

impl JanitorSection {
    pub fn janitor_config(&self) -> ClusterJanitorConfig {
        ClusterJanitorConfig {
            tick: Duration::from_secs(self.tick_secs),
            offline_grace: Duration::from_secs(self.offline_grace_secs),
            // 고아 스냅샷의 유예는 설정하지 않는다(기본 30초).
            ..ClusterJanitorConfig::default()
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
fn default_janitor_tick_secs() -> u64 {
    10
}
fn default_offline_grace_secs() -> u64 {
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
        if self.schedule.tick_secs == 0 || self.schedule.hour_secs == 0 {
            anyhow::bail!("schedule.tick_secs and schedule.hour_secs must be at least 1");
        }
        if self.janitor.tick_secs == 0 || self.janitor.offline_grace_secs == 0 {
            anyhow::bail!("janitor.tick_secs and janitor.offline_grace_secs must be at least 1");
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
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(10));
        assert_eq!(janitor.offline_grace, Duration::from_secs(60));
        assert_eq!(janitor.orphan_grace, Duration::from_secs(30));
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
    fn janitor_section_shortens_periods_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntick_secs = 1\noffline_grace_secs = 2",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(1));
        assert_eq!(janitor.offline_grace, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "offline_grace_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[janitor]\ngrace = 1")
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
//! 노드 감시, 예약 스냅샷.
```

을 다음으로 바꾼다:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리.
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, SnapshotScheduler, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
```

을 다음으로 바꾼다:

```rust
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{AppState, ClusterJanitor, SnapshotScheduler, TaskRunner, TaskRunnerConfig};
use opl_cluster::types::{LeaderRequest, NodeId};
```

`crates/opld/src/member.rs`에서

```rust
    schedule: SnapshotScheduler,
    register: JoinHandle<()>,
```

을 다음으로 바꾼다:

```rust
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    register: JoinHandle<()>,
```

`crates/opld/src/member.rs`에서

```rust
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let app = opl_api::router(state).merge(crate::web::router());
```

을 다음으로 바꾼다:

```rust
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let app = opl_api::router(state).merge(crate::web::router());
```

`crates/opld/src/member.rs`에서

```rust
            schedule,
            register,
```

을 다음으로 바꾼다:

```rust
            schedule,
            janitor,
            register,
```

`crates/opld/src/member.rs`에서

```rust
        self.schedule.shutdown().await;
        self.runner.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.runner.shutdown().await;
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 352개, 실패 0

Run: `cargo test -p opl-api --test janitor`
Expected: PASS 3개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 오프라인·제거된 노드의 Task와 남은 pending 스냅샷을 끝내는 클러스터 정리 작업"
```

---

### Task 5: 스냅샷 만들기 중 강제 정지

`vm_action`의 `stop?mode=force`는 끝나지 않은 `SnapshotRevert`가 있을 때만 409 `vm_busy`다(새 `pub(crate) snapshots::check_no_revert_task`, `check_no_snapshot_task`와 비공개 `check_no_task`를 함께 쓴다). 일반 정지·시작·재부팅은 그대로 막는다. 강제 정지 Task는 전원 Task라 원래 대상별 줄에 서지 않으므로 실행기는 바꾸지 않고 테스트로 확인한다. `tasks/snapshot.rs`의 만들기가 실패하면 하이퍼바이저로 VM을 다시 보고, 꺼졌으면 하이퍼바이저 대신 스토리지로 태그를 지운다(QMP만 끊기고 QEMU가 살아 있으면 이미지 잠금으로 거부돼 태그가 남는다는 주석). `MockHypervisor::hang_next_snapshot_save()`·`snapshot_save_reached()`: VM이 지워질 때까지 끝나지 않고 QMP가 끊긴 오류로 실패하는 메모리 저장. QEMU는 진행 중인 `snapshot-save`가 끝난 뒤에야 `quit`을 처리하므로, 실제 QEMU 테스트(`force_stop_during_memory_snapshot_fails_it`, 메모리 4096 MiB VM)는 vmstate가 쓰이기 시작해 qcow2 파일이 커지면 QEMU를 `SIGSTOP`으로 멈추고(패닉하면 `SIGKILL`), 태그가 아직 없고 스냅샷이 `pending`인지 본 뒤 `oplctl vm stop --force`(약 15초 뒤 QEMU를 죽인다) → QEMU 없음, VM 정지, 만들기 실패("QMP connection closed"), 스냅샷·태그 없음을 확인한다.

**Files:**
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/snapshots.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being snapshotted, reverted or cloned", |op| {
        matches!(
            op,
            TaskOp::SnapshotCreate { .. } | TaskOp::SnapshotRevert { .. } | TaskOp::VmClone { .. }
        )
    })
}

/// 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았으면 409 `vm_busy`. 강제 정지는 이것만 막는다
/// (설계 §4). 만들기 중에 끄면 저장이 끊겨 만들기가 실패하고, 복제는 꺼진 VM이거나 실행 중인
/// VM의 스냅샷을 `-U`로 읽으므로 꺼도 된다. 되돌리기는 디스크를 바꾸는 중에 끄고 켜는 것이 섞인다.
pub(crate) fn check_no_revert_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being reverted to a snapshot", |op| {
        matches!(op, TaskOp::SnapshotRevert { .. })
    })
}

fn check_no_task(
    state: &AppState,
    vm: &Vm,
    doing: &str,
    op: impl Fn(&TaskOp) -> bool,
) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks
        .iter()
        .any(|t| t.spec.target == vm.id && !t.status.state.is_finished() && op(&t.spec.op));
    if busy {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
            format!(
                "VM {:?} is being snapshotted, reverted or cloned; try again when the task has finished",
                vm.name
```

을 다음으로 바꾼다:

```rust
            format!(
                "VM {:?} is {doing}; try again when the task has finished",
                vm.name
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
    if let Some(error) = failure {
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
```

을 다음으로 바꾼다:

```rust
    if let Some(error) = failure {
        // 저장하는 동안 VM이 꺼졌으면(강제 정지로 QMP가 끊김) 꺼진 VM처럼 스토리지로 지운다.
        // QMP만 끊기고 QEMU가 아직 살아 있으면 qemu-img가 이미지 잠금으로 거부해 태그가 남는다
        // (디스크는 손상되지 않고, 아래 로그만 남는다).
        let running = running && !matches!(ctx.hv.inspect(vm_id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
```

`crates/opl-api/src/vms.rs`에서

```rust
use crate::scheduler::{Inventory, schedule};
use crate::snapshots::{check_no_snapshot_task, count_snapshots};
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
use crate::scheduler::{Inventory, schedule};
use crate::snapshots::{check_no_revert_task, check_no_snapshot_task, count_snapshots};
use crate::state::AppState;
```

`crates/opl-api/src/vms.rs`에서

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 강제 정지도 막는다. 되돌리기가 디스크를 바꾸는 중에 끄고 켜는 것이 섞이면 안 되고, 만들기
    // 중에 끄면 저장이 끊긴다. 시작한 스냅샷 Task는 실행기의 시간 제한으로 끝난다.
    check_no_snapshot_task(&state, &vm)?;
    let id = vm.id;
```

을 다음으로 바꾼다:

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 스냅샷 Task가 끝나지 않았으면 막는다. 시작한 스냅샷 Task는 실행기의 시간 제한으로 끝난다.
    // 강제 정지는 되돌리기만 막는다. 저장이 멈춘 만들기 중에도 VM을 끌 수 있어야 한다.
    if query.mode == StopMode::Force {
        check_no_revert_task(&state, &vm)?;
    } else {
        check_no_snapshot_task(&state, &vm)?;
    }
    let id = vm.id;
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

fn vm_spec(node: Uuid) -> VmSpec {
```

을 다음으로 바꾼다:

```rust

#[tokio::test]
async fn force_stop_during_a_memory_snapshot_fails_it_and_removes_the_tag() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();

    env.hv.hang_next_snapshot_save();
    let (status, create) = admin
        .post("/vms/web/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{create}");
    tokio::time::timeout(common::WAIT, env.hv.snapshot_save_reached())
        .await
        .expect("snapshot save");

    // 만들기 중에도 강제 정지는 받는다(일반 정지는 그대로 막는다). 강제 정지 Task는 같은 VM의
    // 만들기 Task를 기다리지 않는다. 기다렸다면 VM이 꺼지지 않아 저장도 끝나지 않는다.
    let (status, body) = admin.post("/vms/web/actions/stop", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    succeeded(&admin.act("web", "stop?mode=force").await);

    // QMP가 끊겨 만들기가 실패한다. 꺼진 VM이므로 디스크의 태그는 스토리지로 지우고 리소스도 지운다.
    let task = admin.wait_task(&create).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "s").is_none());
    assert!(storage.calls().contains(&("delete", root.id)));
    assert!(env.hv.snapshots(vm).is_empty());
    assert_eq!(position(&env, vm), None);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(current.status.phase, VmPhase::Stopped);
}

fn vm_spec(node: Uuid) -> VmSpec {
```

`crates/opl-driver/src/mock.rs`에서

```rust
use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use async_trait::async_trait;
use opl_qemu::QmpError;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;
```

`crates/opl-driver/src/mock.rs`에서

```rust
    events: broadcast::Sender<HypervisorEvent>,
}
```

을 다음으로 바꾼다:

```rust
    events: broadcast::Sender<HypervisorEvent>,
    /// 멈춘 `snapshot_save`가 들어왔다
    save_reached: Notify,
}
```

`crates/opl-driver/src/mock.rs`에서

```rust
    fail_next_snapshot: Option<DriverError>,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
```

을 다음으로 바꾼다:

```rust
    fail_next_snapshot: Option<DriverError>,
    /// 다음 `snapshot_save` 한 번을 VM이 꺼질 때까지 멈춘다.
    hang_next_save: bool,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
```

`crates/opl-driver/src/mock.rs`에서

```rust
            events: broadcast::channel(64).0,
        }
```

을 다음으로 바꾼다:

```rust
            events: broadcast::channel(64).0,
            save_reached: Notify::new(),
        }
```

`crates/opl-driver/src/mock.rs`에서

```rust
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/mock.rs`에서

```rust
    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.snapshots
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opld/tests/snapshots.rs`에서

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제), 볼륨 크기 늘리기, VM 복제(꺼진 VM, 실행 중인 VM의
//! 스냅샷), 예약 스냅샷(짧은 주기로 만들고 보관 개수대로 지우기)을 데몬 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
```

을 다음으로 바꾼다:

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제), 볼륨 크기 늘리기, VM 복제(꺼진 VM, 실행 중인 VM의
//! 스냅샷), 예약 스냅샷(짧은 주기로 만들고 보관 개수대로 지우기), 메모리 스냅샷 중 강제 정지를 데몬
//! 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
```

`crates/opld/tests/snapshots.rs`에서

```rust
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use reqwest::{Method, StatusCode};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::{Value, json};
```

`crates/opld/tests/snapshots.rs`에서

```rust

/// `oplctl vm snapshot schedule web …`로 예약을 바꾼다. 스냅샷 Task가 도는 동안은 수정이
```

을 다음으로 바꾼다:

```rust

/// 테스트가 SIGSTOP으로 멈춘 QEMU. 끝나기 전에 테스트가 실패하면 SIGKILL로 끝낸다.
struct Frozen(Option<Pid>);

impl Frozen {
    fn new(pid: u64) -> Self {
        let pid = Pid::from_raw(pid as i32).unwrap();
        kill_process(pid, Signal::STOP).unwrap();
        Self(Some(pid))
    }

    /// opld가 끝낸 프로세스는 더 지키지 않는다.
    async fn wait_gone(&mut self) {
        let pid = self.0.unwrap();
        eventually("frozen QEMU to exit", || async move {
            test_kill_process(pid).is_err()
        })
        .await;
        self.0 = None;
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = kill_process(pid, Signal::KILL);
        }
    }
}

#[tokio::test]
async fn force_stop_during_memory_snapshot_fails_it() {
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    let file = setup(&mut d).await;
    let c = client(&d);
    // 메모리 저장은 손대지 않은 메모리까지 훑으므로 메모리가 크면 디스크에 쓰는 동안을 볼 틈이 넉넉하다.
    d.ctl(&[
        "vm", "create", "web", "--memory", "4096", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let pid = vm(&c).await["status"]["pid"].as_u64().unwrap();
    let size = std::fs::metadata(&file).unwrap().len();
    let create = c
        .post("/vms/web/snapshots", &json!({ "name": "mem" }))
        .await
        .unwrap();

    // 메모리 상태를 쓰기 시작하면(파일이 커진다) QEMU를 멈춰 저장이 끝나지 않게 한다(멈춘 스토리지처럼).
    // 저장 중인 QEMU는 QMP quit에 답하지 않으므로 강제 정지는 기다린 뒤 SIGKILL로 끝낸다.
    let deadline = Instant::now() + Duration::from_secs(60);
    while std::fs::metadata(&file).unwrap().len() == size {
        assert!(
            Instant::now() < deadline,
            "memory state was not written:\n{}",
            d.log()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut frozen = Frozen::new(pid);
    // 저장 도중이다. 스냅샷 표는 마지막에 쓰므로 태그가 아직 없다.
    assert!(
        tags(&file).is_empty(),
        "snapshot finished before QEMU was frozen"
    );
    assert_eq!(snapshot(&c, "mem").await["status"]["phase"], "pending");

    // 만들기 중에도 강제 정지를 받는다.
    let out = d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(out.contains("vm web: stop done"), "{out}");
    frozen.wait_gone().await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "stopped", "{v}");
    assert_eq!(v["status"]["snapshot"], Value::Null, "{v}");

    // QMP가 끊겨 만들기가 실패하고 리소스를 지운다. 디스크에 태그도 없다.
    let err = c.wait_task(&create).await.unwrap_err();
    assert!(err.to_string().contains("QMP connection closed"), "{err}");
    assert!(snapshots(&c).await.is_empty());
    assert!(tags(&file).is_empty());

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

/// `oplctl vm snapshot schedule web …`로 예약을 바꾼다. 스냅샷 Task가 도는 동안은 수정이
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 353개, 실패 0

Run: `cargo test -p opl-api --test snapshot_tasks`
Expected: PASS 14개

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 6개(약 35초)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 스냅샷 만들기 중에도 강제 정지를 받고 끊긴 만들기의 태그를 꺼진 VM처럼 지우기"
```

---

### Task 6: 예약 오류 code와 기록 규칙

예약 작업의 보관 삭제는 `start_delete(state, vm, key, scheduled: true)`로 `SnapshotDelete { scheduled: true }`를 만든다(API의 사용자 삭제는 `false`). 실패를 기록할 Task는 새 `pub(crate) schedule::recorded_vm(task_spec, find)`가 정한다: 예약 스냅샷의 `SnapshotCreate`, 그리고 `SnapshotDelete { scheduled: true }`는 스냅샷을 찾지 않고 Task 대상(VM)으로(이미 지워졌어도 기록). 사용자가 지운 예약 스냅샷의 삭제 실패는 기록하지 않는다. `schedule::task_failed(message)`로 `task_failed` 오류를 만들고, 실행기와 정리 작업이 같이 쓴다. 정리 작업은 실패 쓰기가 성공하면, 또는 `RevisionMismatch`로 거부됐지만 Task를 다시 읽어 이 정리 작업의 오류로 `failed`면(`written_anyway`, 응답만 잃은 경우) `task_failed`와 자기 오류(`node {id} is offline` 등)를 그 VM에 쓴다. CLI `vm snapshot schedule`은 `last error: <code>: <message>`. 테스트: 거절 code(`snapshot_unsupported`, `node_offline`), 보관 삭제 op의 `scheduled: true`, 사용자 삭제 실패를 기록하지 않음, 정리 작업이 끝낸 예약 만들기의 기록, `recorded_vm` 단위 테스트, 한 노드 메모리 클러스터에서 거부됐지만 이미 쓰인 실패 쓰기.

**Files:**
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/schedule.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/janitor.rs`
- Modify: `crates/opl-api/tests/schedules.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs` 전체:

```rust
//! 클러스터 정리 작업(견고성 설계 §3). 리더만 `tick`마다 노드·Task·스냅샷 목록을 보고, 오프라인이
//! 유예보다 오래 간 노드의 `pending` Task와 제거된 노드의 끝나지 않은 Task를 실패로 끝내고, 만들
//! Task 없이 남은 `pending` 스냅샷을 지운다. 실패로 끝낸 Task의 정리(볼륨·태그)는 하지 않는다.
//! 예약 스냅샷의 만들기·예약 정리의 삭제를 끝냈으면 실행기처럼 `task_failed`로 VM에 남긴다.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::schedule::{recorded_vm, set_schedule_error, task_failed};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct ClusterJanitorConfig {
    /// 목록을 확인하는 간격
    pub tick: Duration,
    /// 노드가 이보다 오래 오프라인이면 그 노드의 `pending` Task를 실패로 끝낸다.
    pub offline_grace: Duration,
    /// 만들 Task 없이 `pending`인 스냅샷을 처음 본 뒤 이만큼 지나도 그대로면 지운다. API는 스냅샷을
    /// 만든 뒤 Task를 만들므로 그 사이(전달된 쓰기는 하나에 최대 `write_timeout`)보다 길어야 한다.
    pub orphan_grace: Duration,
}

impl Default for ClusterJanitorConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            offline_grace: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(30),
        }
    }
}

/// 클러스터 정리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct ClusterJanitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ClusterJanitor {
    pub fn spawn(state: Arc<AppState>, config: ClusterJanitorConfig) -> Self {
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
    config: ClusterJanitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut seen = Seen::default();
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
            _ = tick(&state, &config, &mut seen) => {}
        }
    }
}

/// 이전 tick들에서 본 것. 메모리에만 두므로 리더가 바뀌면 유예를 다시 잰다.
#[derive(Debug, Default)]
struct Seen {
    /// 노드가 오프라인으로 처음 보인 시각
    offline_since: HashMap<Uuid, Instant>,
    /// 만들 Task 없이 `pending`인 스냅샷과 처음 그렇게 보인 시각
    orphans: HashMap<Uuid, Instant>,
}

/// 정리 작업이 할 일. 읽은 revision에서만 쓴다(그사이 바뀌었으면 다음 tick에 다시 본다).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    FailTask {
        id: Uuid,
        revision: u64,
        error: String,
    },
    DeleteSnapshot {
        id: Uuid,
        revision: u64,
    },
}

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십의 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
fn decide(
    now: Instant,
    config: &ClusterJanitorConfig,
    nodes: &[Node],
    members: &BTreeSet<Uuid>,
    tasks: &[Task],
    snapshots: &[Snapshot],
    seen: &mut Seen,
) -> Vec<Action> {
    // 다시 온라인이 되었거나 목록에서 빠진 노드는 잊는다.
    seen.offline_since
        .retain(|id, _| nodes.iter().any(|n| n.id == *id && !n.status.online));
    for node in nodes.iter().filter(|n| !n.status.online) {
        seen.offline_since.entry(node.id).or_insert(now);
    }
    let mut actions = vec![];
    for task in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let node = task.spec.node;
        let error = match nodes.iter().find(|n| n.id == node) {
            None if members.contains(&node) => continue,
            // 제거된 노드는 돌아오지 않으므로 실행 중이던 Task도 끝낸다.
            None => format!("node {node} was removed"),
            // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 두고, 시작 전인
            // Task만 유예 뒤에 끝낸다.
            Some(_) if task.status.state != TaskState::Pending => continue,
            Some(_) => match seen.offline_since.get(&node) {
                Some(since) if now.duration_since(*since) >= config.offline_grace => {
                    format!("node {node} is offline")
                }
                _ => continue,
            },
        };
        actions.push(Action::FailTask {
            id: task.id,
            revision: task.revision,
            error,
        });
    }
    // API는 스냅샷을 만든 뒤에 Task를 만들므로 그 사이를 고아로 보지 않도록 처음 본 뒤 유예가
    // 지나도 고아일 때 지운다. 그 사이 Task가 보였으면 다시 잰다.
    let orphans: HashMap<Uuid, Instant> = snapshots
        .iter()
        .filter(|s| s.status.phase == SnapshotPhase::Pending)
        .filter(|s| {
            !tasks.iter().any(|t| {
                !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id)
            })
        })
        .map(|s| (s.id, seen.orphans.get(&s.id).copied().unwrap_or(now)))
        .collect();
    for s in snapshots.iter() {
        if orphans
            .get(&s.id)
            .is_some_and(|since| now.duration_since(*since) >= config.orphan_grace)
        {
            actions.push(Action::DeleteSnapshot {
                id: s.id,
                revision: s.revision,
            });
        }
    }
    seen.orphans = orphans;
    actions
}

async fn tick(state: &AppState, config: &ClusterJanitorConfig, seen: &mut Seen) {
    let cluster = &state.cluster;
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        *seen = Seen::default();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
        ))
    })();
    let Ok((nodes, tasks, snapshots)) = lists else {
        return;
    };
    let ids: BTreeSet<Uuid> = members
        .voters
        .keys()
        .chain(members.learners.keys())
        .map(|id| id.0)
        .collect();
    let actions = decide(
        Instant::now(),
        config,
        &nodes,
        &ids,
        &tasks,
        &snapshots,
        seen,
    );
    for action in actions {
        let result = apply(cluster, &action).await;
        // 그사이 바뀌었으면(실행기가 시작했다 등) 다음 tick에 다시 본다.
        match result {
            Ok(_) => tracing::info!(?action, "janitor"),
            Err(e) if written_anyway(cluster, &action, &e) => {
                tracing::info!(?action, "janitor (reply lost)")
            }
            Err(e) => {
                tracing::info!(?action, error = %e, "janitor action not applied");
                continue;
            }
        }
        if let Action::FailTask { id, error, .. } = action {
            record_schedule(state, &tasks, &snapshots, id, error).await;
        }
    }
}

/// 할 일을 읽은 revision에서만 쓴다.
async fn apply(cluster: &Cluster, action: &Action) -> Result<u64, ClusterError> {
    match action {
        Action::FailTask {
            id,
            revision,
            error,
        } => {
            let status = TaskStatus {
                state: TaskState::Failed,
                error: Some(error.clone()),
            };
            cluster
                .update_status_checked::<TaskSpec>(*id, *revision, &status)
                .await
        }
        Action::DeleteSnapshot { id, revision } => {
            cluster.delete::<SnapshotSpec>(*id, Some(*revision)).await
        }
    }
}

/// 실패 쓰기가 `RevisionMismatch`로 거부됐어도 앞선 시도의 응답만 잃은 자기 쓰기일 수 있다(전달
/// 재시도). Task를 다시 읽어 이 오류로 실패해 있으면 쓰인 것으로 본다.
fn written_anyway(cluster: &Cluster, action: &Action, err: &ClusterError) -> bool {
    let (
        Action::FailTask { id, error, .. },
        ClusterError::Rejected(Rejection::RevisionMismatch { .. }),
    ) = (action, err)
    else {
        return false;
    };
    cluster
        .get::<TaskSpec>(*id)
        .ok()
        .flatten()
        .is_some_and(|t| {
            t.status.state == TaskState::Failed && t.status.error.as_deref() == Some(error)
        })
}

/// 실패로 끝낸 Task가 예약 스냅샷의 만들기나 예약 정리의 삭제면 실행기처럼 VM에 남긴다.
async fn record_schedule(
    state: &AppState,
    tasks: &[Task],
    snapshots: &[Snapshot],
    id: Uuid,
    error: String,
) {
    let Some(task) = tasks.iter().find(|t| t.id == id) else {
        return;
    };
    let find = |s: Uuid| snapshots.iter().find(|x| x.id == s).cloned();
    let Some(vm) = recorded_vm(&task.spec, find) else {
        return;
    };
    if let Err(e) = set_schedule_error(&state.cluster, vm, Some(task_failed(error))).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::vm::{CpuSpec, MachineType, PowerState, VmSpec};

    use super::*;

    const GRACE: Duration = Duration::from_secs(60);
    const ORPHAN_GRACE: Duration = Duration::from_secs(30);
    const CONFIG: ClusterJanitorConfig = ClusterJanitorConfig {
        tick: Duration::from_secs(10),
        offline_grace: GRACE,
        orphan_grace: ORPHAN_GRACE,
    };

    fn node(online: bool) -> Node {
        let mut n: Node = Resource::new(
            "n",
            NodeSpec {
                address: "h:1".into(),
                maintenance: false,
            },
        );
        n.status = NodeStatus {
            online,
            ..Default::default()
        };
        n
    }

    fn task(op: TaskOp, node: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node,
            },
        );
        t.revision = 7;
        t.status = TaskStatus { state, error: None };
        t
    }

    fn pending_snapshot() -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: Uuid::now_v7(),
                name: "s".into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: false,
                vm_spec: VmSpec {
                    cpu: CpuSpec {
                        sockets: 1,
                        cores: 1,
                        model: Default::default(),
                    },
                    memory_mib: 256,
                    firmware: Default::default(),
                    machine: MachineType::Virt,
                    disks: vec![],
                    nics: vec![],
                    power: PowerState::Stopped,
                    restart: Default::default(),
                    node: None,
                    snapshot_schedule: None,
                },
            },
        );
        s.revision = 3;
        s.status = SnapshotStatus::default();
        s
    }

    fn fail(t: &Task, error: String) -> Action {
        Action::FailTask {
            id: t.id,
            revision: t.revision,
            error,
        }
    }

    #[test]
    fn pending_tasks_of_an_offline_node_fail_after_the_grace() {
        let (up, down) = (node(true), node(false));
        let nodes = [up.clone(), down.clone()];
        let members = BTreeSet::from([up.id, down.id]);
        let pending = task(TaskOp::VmStart, down.id, TaskState::Pending);
        let running = task(TaskOp::VmStop, down.id, TaskState::Running);
        let done = task(TaskOp::VmStart, down.id, TaskState::Failed);
        let elsewhere = task(TaskOp::VmStart, up.id, TaskState::Pending);
        let tasks = [pending.clone(), running, done, elsewhere];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, &nodes, &members, &tasks, &[], &mut seen);
        // 처음 오프라인으로 본 시각에서 유예를 잰다.
        assert_eq!(at(t0), vec![]);
        assert_eq!(at(t0 + GRACE - Duration::from_secs(1)), vec![]);
        // `running`은 건드리지 않는다.
        let error = format!("node {} is offline", down.id);
        assert_eq!(at(t0 + GRACE), vec![fail(&pending, error)]);
    }

    #[test]
    fn coming_back_online_restarts_the_grace() {
        let mut n = node(false);
        let members = BTreeSet::from([n.id]);
        let pending = task(TaskOp::VmStart, n.id, TaskState::Pending);
        let tasks = std::slice::from_ref(&pending);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let half = GRACE / 2;
        assert_eq!(
            decide(
                t0,
                &CONFIG,
                std::slice::from_ref(&n),
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        n.status.online = true;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + half, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert!(seen.offline_since.is_empty());
        // 다시 오프라인이 된 때부터 잰다.
        n.status.online = false;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + GRACE, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + GRACE + half,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + 2 * GRACE,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            )
            .len(),
            1
        );
    }

    #[test]
    fn unfinished_tasks_of_a_removed_node_fail_at_once() {
        let up = node(true);
        let gone = Uuid::now_v7();
        let pending = task(TaskOp::VmStart, gone, TaskState::Pending);
        let running = task(TaskOp::VmStop, gone, TaskState::Running);
        let done = task(TaskOp::VmStop, gone, TaskState::Succeeded);
        let tasks = [pending.clone(), running.clone(), done];
        let error = format!("node {gone} was removed");
        let mut seen = Seen::default();
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![fail(&pending, error.clone()), fail(&running, error)]
        );
        // 멤버십에 있으면 아직 노드를 등록하지 않은 새 노드다.
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id, gone]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
    }

    #[test]
    fn pending_snapshots_without_a_create_task_go_after_the_orphan_grace() {
        let up = node(true);
        let nodes = std::slice::from_ref(&up);
        let members = BTreeSet::from([up.id]);
        let (orphan, busy, mut ready) =
            (pending_snapshot(), pending_snapshot(), pending_snapshot());
        ready.status.phase = SnapshotPhase::Ready;
        let create =
            |s: &Snapshot, state| task(TaskOp::SnapshotCreate { snapshot: s.id }, up.id, state);
        // 끝난 만들기 Task는 세지 않는다.
        let tasks = [
            create(&busy, TaskState::Running),
            create(&orphan, TaskState::Failed),
        ];
        let snapshots = [orphan.clone(), busy, ready];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, nodes, &members, &tasks, &snapshots, &mut seen);
        // tick이 짧아도 처음 본 뒤 유예가 지나기 전에는 지우지 않는다.
        for ms in (0..ORPHAN_GRACE.as_millis() as u64).step_by(100) {
            assert_eq!(at(t0 + Duration::from_millis(ms)), vec![], "{ms}ms");
        }
        let delete = Action::DeleteSnapshot {
            id: orphan.id,
            revision: orphan.revision,
        };
        assert_eq!(at(t0 + ORPHAN_GRACE), vec![delete]);

        // 고아로 보인 뒤 Task가 보이면 지우지 않고, 다시 고아가 된 때부터 잰다.
        let mut seen = Seen::default();
        let snapshots = std::slice::from_ref(&orphan);
        let started = [create(&orphan, TaskState::Pending)];
        let mut at = |now, tasks: &[Task]| {
            decide(now, &CONFIG, nodes, &members, tasks, snapshots, &mut seen)
        };
        assert_eq!(at(t0, &[]), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &started), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &[]), vec![]);
        assert_eq!(
            at(t0 + 2 * ORPHAN_GRACE - Duration::from_secs(1), &[]),
            vec![]
        );
        assert_eq!(at(t0 + 2 * ORPHAN_GRACE, &[]).len(), 1);
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    #[tokio::test]
    async fn failing_a_task_the_executor_started_meanwhile_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        // 정리 작업이 `pending`으로 읽은 뒤 실행기가 먼저 시작했다.
        let action = fail(&t, "node x is offline".into());
        let running = TaskStatus {
            state: TaskState::Running,
            error: None,
        };
        cluster
            .update_status_checked::<TaskSpec>(t.id, t.revision, &running)
            .await
            .unwrap();
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClusterError::Rejected(Rejection::RevisionMismatch { .. })
            ),
            "{err:?}"
        );
        let got = cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
        assert_eq!(got.status, running);
    }

    #[tokio::test]
    async fn a_rejected_failure_that_was_already_written_counts_as_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        let action = fail(&t, "node x is offline".into());
        apply(&cluster, &action).await.unwrap();
        // 응답만 잃고 다시 보낸 쓰기는 거부되지만 Task는 이 오류로 실패해 있다.
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(written_anyway(&cluster, &action, &err), "{err:?}");
        // 다른 오류로 실패해 있으면 다른 쪽이 쓴 것이다.
        let other = fail(&t, "node x was removed".into());
        let err = apply(&cluster, &other).await.unwrap_err();
        assert!(!written_anyway(&cluster, &other, &err), "{err:?}");
    }
}
```

`crates/opl-api/src/schedule.rs`에서

```rust
//! 하나 지우거나 주기가 지났으면 새 예약 스냅샷을 요청한다. 요청은 API 핸들러와 같은 함수로 하므로
//! 같은 검사를 받는다. 거절되면 그 이유를 `status.schedule_error`에 남긴다. 만들기 Task의 결과와
//! 삭제 Task의 실패는 Task 실행기가 남긴다.
```

을 다음으로 바꾼다:

```rust
//! 하나 지우거나 주기가 지났으면 새 예약 스냅샷을 요청한다. 요청은 API 핸들러와 같은 함수로 하므로
//! 같은 검사를 받는다. 거절되면 그 이유(API 오류 code·메시지)를 `status.schedule_error`에 남긴다.
//! 만들기 Task의 결과와 예약 정리 삭제(`scheduled: true`) Task의 실패는 Task 실행기(노드가 끝내기
//! 전에 정리 작업이 끝냈으면 정리 작업)가 `task_failed`로 남긴다.
```

`crates/opl-api/src/schedule.rs`에서

```rust
            Ok(()) => match &plan {
                Plan::Delete(id) => start_delete(state, vm, &id.to_string()).await,
                Plan::Create { name, memory } => {
```

을 다음으로 바꾼다:

```rust
            Ok(()) => match &plan {
                Plan::Delete(id) => start_delete(state, vm, &id.to_string(), true).await,
                Plan::Create { name, memory } => {
```

`crates/opl-api/src/schedule.rs`에서

```rust
        )),
    }
```

을 다음으로 바꾼다:

```rust
        )),
    }
}

/// 실패를 VM의 `schedule_error`에 남길 Task면 그 VM: 예약 스냅샷의 만들기와 예약 정리의 삭제.
/// 사용자가 지운 예약 스냅샷의 삭제는 남기지 않는다. `find`는 id로 스냅샷을 찾는다. 예약 정리의
/// 삭제는 스냅샷이 이미 지워졌어도 남기도록 Task의 대상(VM)으로 정한다.
pub(crate) fn recorded_vm(
    task: &TaskSpec,
    find: impl FnOnce(Uuid) -> Option<Snapshot>,
) -> Option<Uuid> {
    match task.op {
        TaskOp::SnapshotCreate { snapshot } => find(snapshot)
            .filter(|s| s.spec.scheduled)
            .map(|s| s.spec.vm),
        TaskOp::SnapshotDelete {
            scheduled: true, ..
        } => Some(task.target),
        _ => None,
    }
}

/// 실패한 Task의 예약 오류
pub(crate) fn task_failed(message: String) -> ScheduleError {
    ScheduleError {
        code: "task_failed".into(),
        message,
    }
```

`crates/opl-api/src/schedule.rs`에서

```rust
        assert!(plan(NOW, HOUR, &v, &[], &[start]).is_some());
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(plan(NOW, HOUR, &v, &[], &[start]).is_some());
    }

    #[test]
    fn records_scheduled_creation_and_scheduled_deletion() {
        let v = vm(ScheduleEvery::Hourly, 3, false);
        let (auto, manual) = (snap(&v, "a", true, None), snap(&v, "m", false, None));
        let find = |s: &Snapshot| {
            let s = s.clone();
            move |_| Some(s)
        };
        let create = |s: &Snapshot| {
            task(
                TaskOp::SnapshotCreate { snapshot: s.id },
                v.id,
                TaskState::Failed,
            )
        };
        assert_eq!(recorded_vm(&create(&auto).spec, find(&auto)), Some(v.id));
        assert_eq!(recorded_vm(&create(&manual).spec, find(&manual)), None);
        let delete = |scheduled| {
            let op = TaskOp::SnapshotDelete {
                snapshot: auto.id,
                scheduled,
            };
            task(op, v.id, TaskState::Failed)
        };
        // 예약 정리의 삭제는 스냅샷이 이미 지워졌어도 대상 VM으로 남긴다.
        assert_eq!(recorded_vm(&delete(true).spec, |_| None), Some(v.id));
        // 사용자가 지운 예약 스냅샷의 삭제는 남기지 않는다.
        assert_eq!(recorded_vm(&delete(false).spec, find(&auto)), None);
    }
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_delete(&state, &vm, &name).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`delete_snapshot`]의 본문: VM의 스냅샷 `key`(이름이나 id)를 지울 Task를 만든다. 예약 작업도 쓴다.
pub(crate) async fn start_delete(state: &AppState, vm: &Vm, key: &str) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
```

을 다음으로 바꾼다:

```rust
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_delete(&state, &vm, &name, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`delete_snapshot`]의 본문: VM의 스냅샷 `key`(이름이나 id)를 지울 Task를 만든다. 예약 작업도
/// 쓴다(`scheduled`).
pub(crate) async fn start_delete(
    state: &AppState,
    vm: &Vm,
    key: &str,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        snapshot: snapshot.id,
        scheduled: false,
    };
```

을 다음으로 바꾼다:

```rust
        snapshot: snapshot.id,
        scheduled,
    };
```

`crates/opl-api/src/tasks.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::oneshot;
```

`crates/opl-api/src/tasks.rs`에서

```rust
use crate::error::ApiError;
use crate::snapshots::vm_placement;
```

을 다음으로 바꾼다:

```rust
use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::vm_placement;
```

`crates/opl-api/src/tasks.rs`에서

```rust

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기·삭제였으면 VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
```

을 다음으로 바꾼다:

```rust

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
```

`crates/opl-api/src/tasks.rs`에서

```rust
    if let Some(vm) = scheduled {
        let failure = ScheduleError {
            code: "task_failed".into(),
            message: error.clone(),
        };
        snapshot::record_schedule(ctx, vm, Some(failure)).await;
    }
```

을 다음으로 바꾼다:

```rust
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp};
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use crate::schedule::set_schedule_error;
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use crate::schedule::{recorded_vm, set_schedule_error};
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust

/// 예약으로 만든 스냅샷의 만들기·삭제 Task면 그 VM
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let (TaskOp::SnapshotCreate { snapshot } | TaskOp::SnapshotDelete { snapshot, .. }) =
        task.spec.op
    else {
        return None;
    };
    get_snapshot(ctx, snapshot)
        .ok()
        .flatten()
        .filter(|s| s.spec.scheduled)
        .map(|s| s.spec.vm)
}
```

을 다음으로 바꾼다:

```rust

/// 예약 스냅샷의 만들기나 예약 정리의 삭제 Task면 그 VM([`recorded_vm`])
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    recorded_vm(&task.spec, |id| get_snapshot(ctx, id).ok().flatten())
}
```

`crates/opl-api/tests/janitor.rs`에서

```rust
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지.
```

을 다음으로 바꾼다:

```rust
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지.
```

`crates/opl-api/tests/janitor.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{ScheduleError, VmSpec};
use reqwest::StatusCode;
```

`crates/opl-api/tests/janitor.rs`에서

```rust
async fn snapshot_task(env: &Env, vm: Uuid, name: &str, node: Uuid, state: TaskState) -> Task {
    let spec = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec;
```

을 다음으로 바꾼다:

```rust
async fn snapshot_task(env: &Env, vm: Uuid, name: &str, node: Uuid, state: TaskState) -> Task {
    snapshot_task_of(env, vm, name, false, node, state).await
}

/// [`snapshot_task`]에 예약 스냅샷인지(`scheduled`)를 더한 것
async fn snapshot_task_of(
    env: &Env,
    vm: Uuid,
    name: &str,
    scheduled: bool,
    node: Uuid,
    state: TaskState,
) -> Task {
    let spec = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec;
```

`crates/opl-api/tests/janitor.rs`에서

```rust
            description: String::new(),
            scheduled: false,
            vm_spec: spec,
```

을 다음으로 바꾼다:

```rust
            description: String::new(),
            scheduled,
            vm_spec: spec,
```

`crates/opl-api/tests/janitor.rs`에서

```rust
    assert_eq!(get_task(&env, live.id).status.state, TaskState::Pending);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(get_task(&env, live.id).status.state, TaskState::Pending);
}

#[tokio::test]
async fn failed_scheduled_snapshot_creation_is_recorded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let away = other_node(&env, "node2", false).await;
    let pending = snapshot_task_of(&env, vm, "auto", true, away, TaskState::Pending).await;
    let janitor = fast_janitor(&env);
    // 실행기가 실패로 끝낸 예약 만들기처럼 Task 실패로 남긴다.
    let expected = ScheduleError {
        code: "task_failed".into(),
        message: format!("node {away} is offline"),
    };
    until("schedule error", || {
        env.cluster
            .get::<VmSpec>(vm)
            .unwrap()
            .unwrap()
            .status
            .schedule_error
            .as_ref()
            == Some(&expected)
    })
    .await;
    janitor.shutdown().await;
    assert_eq!(get_task(&env, pending.id).status.state, TaskState::Failed);
}
```

`crates/opl-api/tests/schedules.rs`에서

```rust
            .schedule_error
            .is_some_and(|e| e.message.contains("non-qcow2"))
    })
```

을 다음으로 바꾼다:

```rust
            .schedule_error
            .is_some_and(|e| e.code == "snapshot_unsupported" && e.message.contains("non-qcow2"))
    })
```

`crates/opl-api/tests/schedules.rs`에서

```rust
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.message.contains("no space"))
    })
    .await;
    until("success", || {
```

을 다음으로 바꾼다:

```rust
        get_vm(&env, web)
            .status
            .schedule_error
            .is_some_and(|e| e.code == "task_failed" && e.message.contains("no space"))
    })
    .await;
    until("success", || {
```

`crates/opl-api/tests/schedules.rs`에서

```rust
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.message.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
```

을 다음으로 바꾼다:

```rust
        get_vm(&env, vm)
            .status
            .schedule_error
            .is_some_and(|e| e.code == "task_failed" && e.message.contains("no space"))
    })
    .await;
    // 실패한 삭제를 tick마다 다시 하지 않는다(한 시간 뒤에 한다).
```

`crates/opl-api/tests/schedules.rs`에서

```rust
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let deletes = tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::SnapshotDelete { .. }))
        .count();
    assert_eq!(deletes, 1);
    assert!(snapshots(&env, vm).iter().any(|s| s.id == ids[0]));
}
```

을 다음으로 바꾼다:

```rust
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    // 예약 정리의 삭제로 표시한다.
    let deletes: Vec<_> = tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::SnapshotDelete { .. }))
        .collect();
    assert_eq!(deletes.len(), 1);
    assert_eq!(
        deletes[0].spec.op,
        TaskOp::SnapshotDelete {
            snapshot: ids[0],
            scheduled: true
        }
    );
    assert!(snapshots(&env, vm).iter().any(|s| s.id == ids[0]));
}

#[tokio::test]
async fn a_failed_user_delete_of_a_scheduled_snapshot_is_not_recorded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[], Value::Null).await;
    let mut s: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: "auto".into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: true,
            vm_spec: get_vm(&env, vm).spec,
        },
    );
    s.name = s.id.to_string();
    env.cluster.create(&s).await.unwrap();
    s.status.phase = SnapshotPhase::Ready;
    s.status.created_at = Some(1);
    env.cluster
        .update_status::<SnapshotSpec>(s.id, &s.status)
        .await
        .unwrap();

    // 사용자가 지운 예약 스냅샷의 삭제가 실패해도 예약 오류로 남기지 않는다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let (status, task) = admin.delete("/vms/web/snapshots/auto").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["scheduled"], false, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(get_vm(&env, vm).status.schedule_error, None);
}
```

`crates/opl-api/tests/schedules.rs`에서

```rust
            .schedule_error
            .is_some_and(|e| e.message.contains("offline"))
    })
```

을 다음으로 바꾼다:

```rust
            .schedule_error
            .is_some_and(|e| e.code == "node_offline" && e.message.contains("offline"))
    })
```

`crates/oplctl/src/cli.rs`에서

```rust
    };
    if let Some(e) = vm["status"]["schedule_error"]["message"].as_str() {
        text.push_str(&format!("last error: {e}\n"));
    }
```

을 다음으로 바꾼다:

```rust
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
            )),
            "schedule: hourly, keep 1\nlast error: VM web is busy\n"
        );
```

을 다음으로 바꾼다:

```rust
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 357개, 실패 0

Run: `cargo test -p opl-api --test schedules`
Expected: PASS 6개

Run: `cargo test -p opl-api --test janitor`
Expected: PASS 4개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): 예약 오류를 code로 남기고 예약 정리 삭제의 실패만 기록하기"
```

---

### Task 7: 웹: 예약 오류 번역과 되돌리기 중에만 강제 정지 막기

`i18n/errors.ts`에 `codeMessage(t, code, message, own?)`를 두고 `errorMessage`와 함께 쓴다: `<own>.<code>`, `errors.<code>` 순으로 있는 번역에 `{{message}}`를 넘기고, 둘 다 없으면(`unknown` 포함) 서버 메시지를 그대로 보인다. 스냅샷 탭은 `own = "snapshots.scheduleError"`로 마지막 예약 실패를 보인다. 새 번역: `errors.task_failed`("작업 오류: {{message}}"), `errors.node_offline`, 예약 전용 `snapshots.scheduleError.vm_busy`·`node_offline`, `vms.forceStopReverting`(ko/en). 웹은 원래 스냅샷 Task로 강제 정지를 막지 않았으므로(`canRun`은 phase만 본다) 만들기 중 켜기는 바꿀 것이 없고, `actions.ts`에 서버 `check_no_revert_task`와 같은 `isReverting(vm, tasks)`를 두어 VM 메뉴와 상세 헤더가 이미 watch하는 Task 목록으로 끝나지 않은 되돌리기가 있을 때만 강제 정지를 막고 까닭을 `aria-describedby`로 잇는다(메뉴의 비활성 항목은 Radix가 키보드로 건너뛴다는 주석). `isBusy`는 그대로다. 테스트: `isReverting` 단위, 목록 메뉴(실행 중 만들기 → 강제 정지가 `?mode=force`로 보냄, 되돌리기 → 비활성과 까닭, 일반 정지는 켜짐), 상세(되돌리기가 끝나면 다시 켜짐), 스냅샷 탭(전용 문구, `task_failed`, `errors.*`로 돌아감, 모르는 code).

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/errors.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/vms/VmDetailPage.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`
- Modify: `web/src/vms/VmMenu.tsx`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`
- Modify: `web/src/vms/actions.test.ts`
- Modify: `web/src/vms/actions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "snapshot_not_ready": "The snapshot is still being taken.",
    "internal": "The server failed: {{message}}"
```

을 다음으로 바꾼다:

```json
    "snapshot_not_ready": "The snapshot is still being taken.",
    "node_offline": "The node holding this VM is offline.",
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}"
```

`web/src/i18n/en.json`에서

```json
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "deleteTitle": "Delete {{name}}?",
```

을 다음으로 바꾼다:

```json
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "forceStopReverting": "Force stop is not available while a snapshot is being reverted.",
    "deleteTitle": "Delete {{name}}?",
```

`web/src/i18n/en.json`에서

```json
    "scheduleEdit": "Change schedule",
    "scheduleFailed": "Last scheduled snapshot failed:"
  },
```

을 다음으로 바꾼다:

```json
    "scheduleEdit": "Change schedule",
    "scheduleFailed": "Last scheduled snapshot failed:",
    "scheduleError": {
      "vm_busy": "Skipped because the VM was busy; it will try again later.",
      "node_offline": "Skipped because the VM's node is offline ({{message}})."
    }
  },
```

`web/src/i18n/errors.ts`에서

```ts
  if (error instanceof ApiError) {
    const key = `errors.${error.code}`;
    // 번역이 없으면 서버의 설명을 그대로 쓴다(번역 틀로 해석하지 않는다).
    return i18n.exists(key) ? t(key, { message: error.message }) : error.message;
  }
```

을 다음으로 바꾼다:

```ts
  if (error instanceof ApiError) {
    return codeMessage(t, error.code, error.message);
  }
```

`web/src/i18n/errors.ts`에서

```ts
  return error instanceof Error ? error.message : String(error);
}
```

을 다음으로 바꾼다:

```ts
  return error instanceof Error ? error.message : String(error);
}

/**
 * 서버의 오류 code와 설명을 문장으로: `errors.<code>` 번역, 번역이 없으면 설명 그대로. 예약 오류도 쓴다.
 * `own`이 있으면 `<own>.<code>` 전용 문구를 먼저 찾는다.
 */
export function codeMessage(t: TFunction, code: string, message: string, own?: string): string {
  for (const key of [own && `${own}.${code}`, `errors.${code}`]) {
    if (key && i18n.exists(key)) {
      return t(key, { message });
    }
  }
  // 번역이 없으면 서버의 설명을 그대로 쓴다(번역 틀로 해석하지 않는다).
  return message;
}
```

`web/src/i18n/ko.json`에서

```json
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "internal": "서버 오류: {{message}}"
```

을 다음으로 바꾼다:

```json
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}"
```

`web/src/i18n/ko.json`에서

```json
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
```

을 다음으로 바꾼다:

```json
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "forceStopReverting": "스냅샷을 되돌리는 중에는 강제 정지할 수 없습니다.",
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
```

`web/src/i18n/ko.json`에서

```json
    "scheduleEdit": "예약 바꾸기",
    "scheduleFailed": "마지막 예약 스냅샷 실패:"
  },
```

을 다음으로 바꾼다:

```json
    "scheduleEdit": "예약 바꾸기",
    "scheduleFailed": "마지막 예약 스냅샷 실패:",
    "scheduleError": {
      "vm_busy": "VM이 다른 작업 중이라 건너뛰었습니다. 다음 기회에 다시 합니다.",
      "node_offline": "VM의 노드가 오프라인이라 건너뛰었습니다 ({{message}})."
    }
  },
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("tab", { name: "Console" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("tab", { name: "Console" })).not.toBeInTheDocument();
});

it("blocks force stop while a snapshot is being reverted and says why", async () => {
  const revert = {
    ...task("t-revert", "snapshot-revert", web.id),
    status: { state: "running" as const, error: null },
  };
  mockLists({ vm: [web], node: [NODE], task: [revert] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(button).toBeDisabled());
  expect(button).toHaveAccessibleDescription(
    "Force stop is not available while a snapshot is being reverted.",
  );
  // 되돌리기가 끝나면 다시 켠다.
  queryClient.setQueryData(["list", "task"], {
    revision: 2,
    items: [{ ...revert, status: { state: "succeeded", error: null } }],
  });
  await waitFor(() => expect(button).toBeEnabled());
  expect(button).not.toHaveAccessibleDescription();
  expect(
    screen.queryByText("Force stop is not available while a snapshot is being reverted."),
  ).not.toBeInTheDocument();
});

it("does not blame a revert for a VM that is already off", async () => {
  const db = vm("db", "stopped");
  const revert = {
    ...task("t-revert", "snapshot-revert", db.id),
    status: { state: "running" as const, error: null },
  };
  mockLists({ vm: [db], node: [NODE], task: [revert] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  expect(button).toBeDisabled();
  expect(button).not.toHaveAccessibleDescription();
});
```

`web/src/vms/VmDetailPage.tsx` 전체:

```tsx
import { Suspense, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { formatMib, formatTime, uuidTime } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import {
  ACTIONS,
  canDelete,
  canRun,
  isReverting,
  useVmActions,
  type VmAction,
} from "@/vms/actions";
import { VmConsole } from "@/vms/lazyConsole";
import { VmConfirm, type VmDialog } from "@/vms/VmMenu";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmSnapshots } from "@/vms/VmSnapshots";

const TABS = ["summary", "hardware", "console", "snapshots", "tasks", "json"] as const;

/** 이름으로 VM을 찾는다. 목록이 아직 없으면 undefined, 없는 VM이면 null */
export function useVmByName(name: string): Vm | null | undefined {
  const vms = useList("vm");
  if (!vms.data) {
    return undefined;
  }
  return vms.data.find((vm) => vm.name === name) ?? null;
}

export function VmDetailPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const vms = useList("vm");
  const vm = useVmByName(name);
  const me = usePrincipal();
  const [params, setParams] = useSearchParams();
  // 콘솔 탭은 VM 동작 권한이 있을 때만 보인다.
  const tabs = TABS.filter((id) => id !== "console" || can(me.role, "operate", "vm"));
  const tab = tabs.find((id) => id === params.get("tab")) ?? "summary";

  if (vm === undefined) {
    return <ListPending error={vms.error} />;
  }
  if (vm === null) {
    return (
      <div className="flex flex-col items-start gap-2">
        <h1 className="text-2xl font-semibold">{t("vms.notFound", { name })}</h1>
        <Link to="/vms" className="text-sm underline">
          {t("vms.backToList")}
        </Link>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-4">
      <VmHeader vm={vm} />
      <Tabs
        value={tab}
        onValueChange={(value) =>
          // 다른 쿼리 값은 그대로 둔다.
          setParams(
            (p) => {
              p.set("tab", value);
              return p;
            },
            { replace: true },
          )
        }
      >
        <TabsList>
          {tabs.map((id) => (
            <TabsTrigger key={id} value={id}>
              {t(`vms.tab.${id}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="summary">
          <Summary vm={vm} />
        </TabsContent>
        <TabsContent value="hardware">
          <Hardware vm={vm} />
        </TabsContent>
        <TabsContent value="console">
          {/* 탭을 열 때만 연결한다. */}
          {tab === "console" && (
            <Suspense fallback={null}>
              <VmConsole vm={vm} />
            </Suspense>
          )}
        </TabsContent>
        <TabsContent value="snapshots">
          <VmSnapshots vm={vm} />
        </TabsContent>
        <TabsContent value="tasks">
          <VmTasks vm={vm} />
        </TabsContent>
        <TabsContent value="json">
          <pre className="overflow-x-auto rounded-md bg-muted p-4 text-xs">
            {JSON.stringify(vm, null, 2)}
          </pre>
        </TabsContent>
      </Tabs>
    </div>
  );
}

function VmHeader({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const navigate = useNavigate();
  const nodes = useList("node");
  const tasks = useList("task");
  const hint = useId();
  const { run, remove } = useVmActions();
  const [confirm, setConfirm] = useState<VmDialog | null>(null);
  // 고정한 노드가 있으면 그 노드, 아니면 스케줄러가 놓은 노드
  const nodeId = vm.spec.node ?? vm.status.node;
  const node = nodes.data?.find((n) => n.id === nodeId);
  const operate = can(me.role, "operate", "vm");
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = operate && canRun(vm, "force-stop") && isReverting(vm, tasks.data ?? []);
  const button = (action: VmAction) => (
    <Button
      key={action}
      variant="outline"
      size="sm"
      disabled={!canRun(vm, action) || (action === "force-stop" && reverting)}
      aria-describedby={action === "force-stop" && reverting ? hint : undefined}
      onClick={() => (action === "force-stop" ? setConfirm("force-stop") : void run(vm, action))}
    >
      {t(`vms.action.${action}`)}
    </Button>
  );
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <div className="flex items-center gap-3">
        <h1 className="text-2xl font-semibold">{vm.name}</h1>
        <VmPhaseBadge vm={vm} />
        {node && <span className="text-sm text-muted-foreground">{node.name}</span>}
      </div>
      <div className="flex flex-wrap gap-2">
        {operate && ACTIONS.map(button)}
        {operate && vm.status.phase === "running" && (
          <Button variant="outline" size="sm" asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}/console`} target="_blank">
              {t("vms.openConsole")}
            </Link>
          </Button>
        )}
        {can(me.role, "write", "vm") && (
          <Button variant="outline" size="sm" asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}/edit`}>{t("vms.action.edit")}</Link>
          </Button>
        )}
        {can(me.role, "write", "vm") && can(me.role, "write", "volume") && (
          <Button variant="outline" size="sm" onClick={() => setConfirm("clone")}>
            {t("vms.action.clone")}
          </Button>
        )}
        {can(me.role, "write", "vm") && (
          <Button
            variant="destructive"
            size="sm"
            disabled={!canDelete(vm)}
            onClick={() => setConfirm("delete")}
          >
            {t("vms.action.delete")}
          </Button>
        )}
      </div>
      {reverting && (
        <p id={hint} className="basis-full text-sm text-muted-foreground">
          {t("vms.forceStopReverting")}
        </p>
      )}
      <VmConfirm
        vm={vm}
        which={confirm}
        onClose={() => setConfirm(null)}
        onRun={run}
        onRemove={remove}
        afterRemove={() => void navigate("/vms")}
      />
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

function Summary({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const { cpu } = vm.spec;
  return (
    <dl className="grid grid-cols-1 gap-4 pt-2 sm:grid-cols-2 lg:grid-cols-3">
      <Field label={t("vms.cpu")}>
        {t("vms.cpuValue", {
          sockets: cpu.sockets ?? 1,
          cores: cpu.cores,
          model: cpu.model ?? "host",
        })}
      </Field>
      <Field label={t("vms.memory")}>{formatMib(vm.spec.memory_mib)}</Field>
      <Field label={t("vms.firmware")}>{(vm.spec.firmware ?? "uefi").toUpperCase()}</Field>
      <Field label={t("vms.machine")}>{vm.spec.machine}</Field>
      <Field label={t("vms.restart")}>{t(`vms.restartPolicy.${vm.spec.restart ?? "never"}`)}</Field>
      <Field label={t("vms.power")}>{t(`vms.powerState.${vm.spec.power ?? "stopped"}`)}</Field>
      <Field label={t("vms.pid")}>{vm.status.pid ?? "—"}</Field>
      <Field label={t("vms.created")}>{formatTime(uuidTime(vm.id), i18n.language)}</Field>
      <Field label={t("vms.lastError")}>{vm.status.last_error ?? "—"}</Field>
    </dl>
  );
}

function Hardware({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col gap-6 pt-2">
      <section className="flex flex-col gap-2">
        <h2 id="vm-disks" className="font-medium">
          {t("vms.disks")}
        </h2>
        <Table aria-labelledby="vm-disks">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.volume")}</TableHead>
              <TableHead>{t("vms.bus")}</TableHead>
              <TableHead>{t("vms.bootOrder")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.disks ?? []).length === 0 && (
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.noDisks")}
                </TableCell>
              </TableRow>
            )}
            {(vm.spec.disks ?? []).map((disk) => (
              <TableRow key={disk.volume}>
                <TableCell>{disk.volume}</TableCell>
                <TableCell>{disk.bus}</TableCell>
                <TableCell>{disk.boot_order ?? "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="vm-nics" className="font-medium">
          {t("vms.nics")}
        </h2>
        <Table aria-labelledby="vm-nics">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.network")}</TableHead>
              <TableHead>{t("vms.model")}</TableHead>
              <TableHead>{t("vms.mac")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(vm.spec.nics ?? []).length === 0 && (
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.noNics")}
                </TableCell>
              </TableRow>
            )}
            {(vm.spec.nics ?? []).map((nic, i) => (
              <TableRow key={i}>
                <TableCell>{nic.network}</TableCell>
                <TableCell>{nic.model}</TableCell>
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
    </div>
  );
}

function VmTasks({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const tasks = useList("task");
  const mine = newestFirst((tasks.data ?? []).filter((task) => task.spec.target === vm.id));
  return (
    <section>
      <h2 id="vm-tasks" className="sr-only">
        {t("vms.tab.tasks")}
      </h2>
      <TaskTable tasks={mine.slice(0, 50)} labelledBy="vm-tasks" />
    </section>
  );
}
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  );
});
```

을 다음으로 바꾼다:

```tsx
  );
});

it("force stops a VM while a snapshot is being taken but not while one is reverted", async () => {
  const running = (id: string, kind: Task["spec"]["op"]["kind"]) => ({
    ...task(id, kind, web.id),
    status: { state: "running" as const, error: null },
  });
  mockLists({ vm: [web], node: [NODE], task: [running("t-create", "snapshot-create")] });
  const sent: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ request }) => {
      sent.push(new URL(request.url).search);
      return HttpResponse.json(task("t-stop", "vm-stop", web.id), { status: 202 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  // Task 목록을 읽은 뒤에 메뉴를 연다.
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  // 서버는 만들기 중 강제 정지를 받는다(만들기가 실패로 끝난다).
  await userEvent.click(await screen.findByRole("menuitem", { name: "Force stop" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Force stop" }),
  );
  await waitFor(() => expect(sent).toEqual(["?mode=force"]));

  // 되돌리기 중에는 막고 까닭을 보인다.
  queryClient.setQueryData<List<Task>>(["list", "task"], {
    revision: 2,
    items: [running("t-revert", "snapshot-revert")],
  });
  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  const item = await screen.findByRole("menuitem", { name: "Force stop" });
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription(
    "Force stop is not available while a snapshot is being reverted.",
  );
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
});
```

`web/src/vms/VmMenu.tsx`에서

```tsx
import { MoreHorizontal } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```tsx
import { MoreHorizontal } from "lucide-react";
import { useId, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
} from "@/components/ui/dropdown-menu";
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
```

을 다음으로 바꾼다:

```tsx
} from "@/components/ui/dropdown-menu";
import {
  ACTIONS,
  canDelete,
  canRun,
  isReverting,
  useVmActions,
  type VmAction,
} from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  const trigger = useRef<HTMLButtonElement>(null);
  const operate = can(me.role, "operate", "vm");
```

을 다음으로 바꾼다:

```tsx
  const trigger = useRef<HTMLButtonElement>(null);
  const tasks = useList("task");
  const hint = useId();
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = canRun(vm, "force-stop") && isReverting(vm, tasks.data ?? []);
  const operate = can(me.role, "operate", "vm");
```

`web/src/vms/VmMenu.tsx`에서

```tsx
                key={action}
                disabled={!canRun(vm, action)}
                onSelect={() =>
```

을 다음으로 바꾼다:

```tsx
                key={action}
                disabled={!canRun(vm, action) || (action === "force-stop" && reverting)}
                aria-describedby={action === "force-stop" && reverting ? hint : undefined}
                onSelect={() =>
```

`web/src/vms/VmMenu.tsx`에서

```tsx
            ))}
          {operate && (
```

을 다음으로 바꾼다:

```tsx
            ))}
          {/* Radix는 비활성 항목을 키보드로 건너뛰므로 aria-describedby가 읽히지 않을 수 있다.
              같은 까닭은 상세 화면에도 보인다. */}
          {operate && reverting && (
            <p id={hint} className="px-1.5 py-1 text-xs text-muted-foreground">
              {t("vms.forceStopReverting")}
            </p>
          )}
          {operate && (
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  ).toBeInTheDocument();
  // 서버가 남긴 이유를 그대로, 무엇의 실패인지와 함께 보인다.
  expect(screen.getByText("Last scheduled snapshot failed:").parentElement).toHaveTextContent(
    `Last scheduled snapshot failed: ${failure}`,
  );
  expect(screen.getByRole("link", { name: "Change schedule" })).toHaveAttribute(
```

을 다음으로 바꾼다:

```tsx
  ).toBeInTheDocument();
  // 서버가 남긴 code를 예약 전용 문구로 번역해, 무엇의 실패인지와 함께 보인다.
  const shown = screen.getByText("Last scheduled snapshot failed:").parentElement;
  expect(shown).toHaveTextContent(
    "Last scheduled snapshot failed: Skipped because the VM was busy; it will try again later.",
  );
  expect(shown).not.toHaveTextContent(failure);
  expect(screen.getByRole("link", { name: "Change schedule" })).toHaveAttribute(
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Change schedule" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Change schedule" })).not.toBeInTheDocument();
});

it.each([
  // 실패한 Task는 번역 뒤에 Task의 오류를 덧붙인다.
  ["task_failed", "no space left on device", "Task error: no space left on device"],
  // 예약 전용 문구에 서버의 설명(노드 이름)을 넣는다.
  [
    "node_offline",
    'node "node-a" holding VM "web" is offline',
    'Skipped because the VM\'s node is offline (node "node-a" holding VM "web" is offline).',
  ],
  // 예약 전용 문구가 없으면 오류 번역을 쓴다.
  [
    "snapshot_unsupported",
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2 can take snapshots.",
  ],
  // 번역이 없는 code(옛 문자열을 읽은 unknown 포함)는 서버의 설명을 그대로 보인다.
  ["unknown", "VM busy", "VM busy"],
  ["something_new", "a new failure", "a new failure"],
])("shows the schedule failure %s in the user's language", async (code, message, text) => {
  const failed = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "hourly" as const, keep: 3, memory: false } },
    status: { ...web.status, schedule_error: { code, message } },
  };
  mockLists({ vm: [failed], node: [NODE], volume: [volume("web-root", "local")] });
  open(failed);
  const shown = (await screen.findByText("Last scheduled snapshot failed:")).parentElement;
  expect(shown).toHaveTextContent(`Last scheduled snapshot failed: ${text}`);
});
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
```

을 다음으로 바꾼다:

```tsx
} from "@/components/ui/table";
import { codeMessage } from "@/i18n/errors";
import { formatBytes, formatTime } from "@/lib/format";
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
        </div>
        {/* 예약을 꺼도 마지막 실패는 남는다. 서버가 쓴 이유를 줄바꿈·긴 낱말까지 그대로 보인다. */}
        {vm.status.schedule_error && (
```

을 다음으로 바꾼다:

```tsx
        </div>
        {/* 예약을 꺼도 마지막 실패는 남는다. code를 예약 전용 문구, 오류 번역 순으로 찾고, 번역이
            없으면 서버가 쓴 이유를 줄바꿈·긴 낱말까지 그대로 보인다. */}
        {vm.status.schedule_error && (
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
            <span className="whitespace-pre-wrap break-words">
              {vm.status.schedule_error.message}
            </span>
```

을 다음으로 바꾼다:

```tsx
            <span className="whitespace-pre-wrap break-words">
              {codeMessage(
                t,
                vm.status.schedule_error.code,
                vm.status.schedule_error.message,
                "snapshots.scheduleError",
              )}
            </span>
```

`web/src/vms/actions.test.ts`에서

```ts
import { vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isBusy, isStopped } from "@/vms/actions";
```

을 다음으로 바꾼다:

```ts
import type { Task } from "@/api/types";
import { task, vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isBusy, isReverting, isStopped } from "@/vms/actions";
```

`web/src/vms/actions.test.ts`에서

```ts
  expect(isBusy(power(vm("h", "running"), "stopped"))).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(isBusy(power(vm("h", "running"), "stopped"))).toBe(true);
});

it("follows the server's check_no_revert_task: only an unfinished revert of this VM", () => {
  const web = vm("web", "running");
  const of = (
    kind: Task["spec"]["op"]["kind"],
    state: Task["status"]["state"],
    target = web.id,
  ) => ({
    ...task(`t-${kind}-${state}`, kind, target),
    status: { state, error: null },
  });
  expect(isReverting(web, [])).toBe(false);
  expect(isReverting(web, [of("snapshot-revert", "pending")])).toBe(true);
  expect(isReverting(web, [of("snapshot-revert", "running")])).toBe(true);
  // 끝난 되돌리기, 다른 VM의 되돌리기
  expect(isReverting(web, [of("snapshot-revert", "succeeded")])).toBe(false);
  expect(isReverting(web, [of("snapshot-revert", "failed")])).toBe(false);
  expect(isReverting(web, [of("snapshot-revert", "running", vm("db", "running").id)])).toBe(false);
  // 만들기·삭제·복제 중에는 강제 정지를 받는다.
  for (const kind of ["snapshot-create", "snapshot-delete", "vm-clone"] as const) {
    expect(isReverting(web, [of(kind, "running")])).toBe(false);
  }
});
```

`web/src/vms/actions.ts`에서

```ts
import { api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
```

을 다음으로 바꾼다:

```ts
import { api, call } from "@/api/client";
import type { Task, Vm } from "@/api/types";
import { errorMessage } from "@/i18n/errors";
```

`web/src/vms/actions.ts`에서

```ts
  return vm.spec.power === "running" ? phase !== "running" : !isStopped(vm);
}
```

을 다음으로 바꾼다:

```ts
  return vm.spec.power === "running" ? phase !== "running" : !isStopped(vm);
}

/**
 * 서버의 `check_no_revert_task`: 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았다. 강제 정지는 이때만
 * 막힌다(만들기·삭제·복제 중에는 받는다). `tasks`는 watch로 들어온 Task 목록이다.
 */
export function isReverting(vm: Vm, tasks: readonly Task[]): boolean {
  return tasks.some(
    (task) =>
      task.spec.target === vm.id &&
      task.spec.op.kind === "snapshot-revert" &&
      task.status.state !== "succeeded" &&
      task.status.state !== "failed",
  );
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 229개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 예약 오류 code 번역과 스냅샷 만들기 중 강제 정지 켜기"
```

---

### Task 8: 문서

견고성 설계 문서의 §2–§6에 "구현하며 정한 것"(제거된 노드의 정의, `decide`·`orphan_grace`, 정리 작업의 쓰기와 `revision_mismatch` 뒤 다시 읽기, 실행기 `begin`, `Unreachable`은 늘 적용됐을 수 있음, 만들기는 지금 revision·삭제는 적용한 인덱스, QEMU가 저장 뒤에야 `quit`을 처리해 강제 정지가 약 15초 뒤 끊는 것, 꺼진 VM의 태그 지우기, `recorded_vm`·`task_failed`, 웹 `isReverting`·`codeMessage`)을, §8에 알고 받아들인 제한을 더한다. 스냅샷 설계 문서의 §5(되돌리기만 강제 정지를 막음, 끊긴 만들기의 태그 정리, `schedule_error` code, 예약 정리 삭제 표시), §6(정리 작업이 남은 `pending` 스냅샷을 지움), §8(CLI `last error`), §9(웹 번역), §11(오프라인 노드 Task, 사용자 삭제 제한 없앰)을 맞춘다. README는 바꾸지 않는다(opld 설정 절을 싣지 않는다). CI는 바꾸지 않는다(`linux-kvm`이 이미 `qemu-it`·`web-it`을 돌린다).

**Files:**
- Modify: `docs/superpowers/specs/2026-10-26-snapshots-design.md`
- Modify: `docs/superpowers/specs/2026-10-27-robustness-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

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
- 반대로 이 VM의 스냅샷 만들기·되돌리기 Task가 끝나지 않았으면 VM 동작(시작·정지·재부팅)과 VM 수정이 409 `vm_busy`다. 강제 정지는 끝나지 않은 되돌리기가 있을 때만 막는다(되돌리기가 디스크를 바꾸는 중에 켜고 끄는 것이 섞이면 안 된다). 계획 10·11은 만들기 중의 강제 정지도 막았으나, 계획 12부터 만들기·삭제·복제 중에는 받는다(견고성 설계(`2026-10-27-robustness-design.md`) §4). 시작한 Task는 Task 제한 시간 안에 끝나고, 오프라인 노드에 배정돼 시작하지 못한 Task는 계획 12의 정리 작업이 유예(기본 60초) 뒤 실패로 끝낸다(§11).
- 모든 디스크가 qcow2이고 `ready`. qcow2가 아닌 것이 있으면 409 `snapshot_unsupported`, 디스크가 없으면 메모리 스냅샷은 400.
- `memory`는 실행 중이고 디스크가 있으면 기본 true. 디스크 없는 실행 중 VM은 기본으로 설정만 담는다. 정지한 VM에 `memory: true`는 400.

실행: `pending` 리소스를 만들고(`parent` = VM의 `status.snapshot`, `vm_spec` = 지금 스펙) Task를 돌린다. Task를 만들지 못하면 그 리소스를 지우고(못 지우면 재시작 정리에 맡긴다) 오류를 돌려준다. `parent`와 `vm_spec`은 Task가 실행될 때 다시 쓴다. 같은 VM에 만들기가 겹쳐 줄을 서면 API 시점의 부모가 같기 때문이다.

| VM 상태 | 방법 |
|---|---|
| 실행 중 + 메모리 | `snapshot-save` (devices = 모든 디스크, vmstate = 첫 디스크). 저장하는 동안 VM이 멈춘다 |
| 실행 중 + 디스크만 | `transaction` + `blockdev-snapshot-internal-sync` |
| 정지 | 디스크마다 `qemu-img snapshot -c` |

UEFI 변수를 복사하고 `ready`로 바꾸고 VM의 `status.snapshot`을 이 스냅샷으로. 실패하면 만든 태그를 지우고 리소스를 지운다. 단 실패가 확실할 때만 태그를 지운다. 시간 초과(`JobTimeout`, QMP 응답 없음, Task 제한 시간, 재시작 중단)면 작업이 QEMU에 남아 있을 수 있어 태그는 두고 리소스만 지운다. 저장하는 동안 강제 정지로 VM이 꺼졌으면(QMP가 끊김, 계획 12) 꺼진 VM처럼 `qemu-img snapshot -d`로 태그를 지운다. QEMU는 저장이 끝난 뒤에야 `quit`을 처리하고 강제 정지는 약 15초 뒤 QEMU를 죽이므로, 이것은 그보다 오래 걸리는 저장(멈춘 저장, 아주 큰 메모리)에서만 생긴다. Task 안에서 VM 상태를 다시 확인한다(Task가 기다리는 사이 바뀔 수 있다).

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
- `schedule_error`: 4xx 거절이면 그 API 오류의 code와 메시지(예: raw 디스크면 `snapshot_unsupported`, 시작·정지 중이면 `vm_busy`, 노드 오프라인이면 `node_offline`)를 쓴다(code는 계획 12, 그 전에는 메시지만). 보관 삭제의 거절도 쓴다. 5xx(리더를 잃는 등)는 로그만. 예약으로 만든 스냅샷의 만들기 Task는 끝날 때 성공이면 지우고 실패(시간 초과·재시작 중단·정리 작업이 끝냄 포함)면 `task_failed`와 Task 오류를 쓴다. 보관 삭제 Task(`snapshot-delete`의 `scheduled: true`, 계획 12)는 실패하면 같은 방식으로 쓰고, 성공해도 지우지 않는다(만들기가 성공해야 지운다). 사용자가 지운 예약 스냅샷의 삭제 실패는 쓰지 않는다(계획 12). 같은 값이면 쓰지 않고, 조건부 쓰기(`update_status_checked`, revision이 다르면 다시 읽어 쓰기)로 쓴다. 예약을 꺼도 지우지 않는다.
- 주기: hourly = `hour`, daily = 24×`hour`, weekly = 168×`hour`. `opld` 설정 `[schedule] tick_secs = 60, hour_secs = 3600`(0은 거부, 테스트만 줄인다).
- 되돌리기는 VM의 지금 `snapshot_schedule`을 지킨다(스냅샷 스펙의 예약으로 되돌리지 않는다). 예약 설정은 VM 수정(PUT, VM Write)이다.

## 6. 일관성과 장애

- **opld 재시작:** 지금처럼 실행 중이던 Task는 실패로 끝난다. 노드가 시작할 때 이 노드의 VM에 대해 `pending` 스냅샷(끝나지 않은 Task가 없는 것)을 지우고 디스크의 같은 태그를 지운다(볼륨 고아 정리와 같은 방식). 맡는 노드는 그 스냅샷 만들기 Task의 노드, 없으면 API가 Task를 배정했을 노드(§3: `spec.node`, `status.node`, 첫 디스크 볼륨의 노드), 다 없으면(디스크 없는 VM) 아무 노드다. Task를 만들기 전에 실패해 남은 `pending` 스냅샷도 이때 지운다. 계획 12부터는 리더의 정리 작업도 끝나지 않은 만들기 Task 없는 `pending` 스냅샷을 처음 본 뒤 30초가 지나도 그대로면 리소스를 지운다(태그는 남을 수 있다).
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
- 예약: CLI에 VM 수정 명령이 없어 처음 생각한 VM 수정 옵션 대신 `vm snapshot schedule`을 둔다. `--every`와 `--keep`은 함께 써야 하고 `--off`는 다른 옵션과 함께 쓸 수 없다. 옵션이 없으면 지금 예약(`schedule: daily, keep 7, with memory` / `schedule: off`)과 마지막 예약 실패(`last error: <code>: <message>`, code는 계획 12)를 보인다(`-o json`이면 `{snapshot_schedule, schedule_error}`). 바꿀 때는 VM을 읽어 `If-Match: revision`으로 라벨·스펙을 그대로 둔 채 예약만 바꿔 PUT 한다(`revision_mismatch`면 세 번까지, 스냅샷·복제 Task 중이라 `vm_busy`면 1초마다 30초까지 다시). `vm delete --with-snapshots`는 지우기 전 확인을 통과하면 예약을 먼저 끄고(`snapshot schedule of vm X turned off`) 다시 확인한 뒤 지운다(그새 예약 스냅샷이 생기지 않게).
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
- 구현: 복제는 VM 메뉴(목록·상세)와 상세 헤더에 있고 VM·볼륨 Write일 때만 보인다. 원본 기본값은 꺼진 VM이면 지금 상태, 아니면 가장 최근 `ready` 스냅샷. 실행 중이면 지금 상태를 고를 수 없고(까닭을 글로), 시작·정지 중이거나 스냅샷 목록을 읽기 전이면 막는다. 대상 풀은 "원본과 같은 풀"과 VM 노드(`vm_placement`와 같은 순서)에서 쓸 수 있는 풀만 보인다(노드를 정할 때까지는 "원본과 같은 풀"만). 이름은 형식과 기존 VM 이름으로 미리 확인한다. 진행은 작업 알림으로 보인다.
- 구현: 예약은 실행 중에도 바꿀 수 있다. 꺼도 보관 개수·메모리 값은 폼에 남는다. 스냅샷 탭에 예약 요약("Daily, keep 7, with memory"), 수정 권한이 있으면 "예약 바꾸기"(VM 편집 화면으로), 예약 스냅샷 행에 "예약" 배지를 보인다. `schedule_error`는 계획 11에서 code 없는 서버 메시지라 그대로(영어) 보였다. 계획 12부터 `errors.<code>`로 번역하고, `task_failed`는 번역 뒤에 Task 오류를 붙이고, 번역이 없는 code는 메시지를 그대로 보인다. 강제 정지 메뉴는 되돌리기 중에만 막는다.

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
- 오프라인 노드에 걸린 그 VM의 스냅샷·복제 Task가 끝나지 않으면 예약 작업은 그 VM을 건너뛰므로, 그동안 `schedule_error`에 아무것도 쓰지 않는다. 계획 12부터 시작 전(`pending`)인 Task는 정리 작업이 유예 뒤 실패로 끝내고 예약 만들기·보관 삭제면 `task_failed`로 남긴다. 네트워크만 끊긴 노드에서 실행 중(`running`)인 Task는 그 노드가 돌아올 때까지 남는다.
- `schedule_error`는 예약을 꺼도 남는다(다음 예약 만들기가 성공해야 지워진다). `task_failed`의 Task 오류는 영어 서버 문구다(계획 12 전에는 전체가 번역되지 않는 메시지였다).
- 복제·가져오기는 Task 제한 시간 600초 안에 끝나야 한다. 큰 디스크 복제는 시간 초과로 실패할 수 있다(만든 볼륨은 지운다).
- 서로 다른 VM의 복제가 동시에 같은 새 볼륨 이름(`{새 VM}-{원래 볼륨}`, 겹치면 `-2`…)을 고르면 뒤의 것이 이름 경합으로 실패한다(다시 하면 된다).
- 복제 중 볼륨을 `ready`로 바꾼 뒤 새 VM을 만들기 전에 opld가 재시작하면 그 볼륨들(`{새 VM}-…`)과 UEFI 변수 사본이 남는다. 볼륨은 손으로 지운다.
- 복제의 새 VM 만들기가 시간 초과나 리더 연결 실패로 끝나면(나중에 적용될 수 있다. 계획 12부터 응답 없이 끝난 전달 뒤 다시 보낸 만들기가 같은 id로 이미 있다고 거부되면 성공으로 본다) 복사한 볼륨과 UEFI 변수 사본을 지우지 않는다. VM이 만들어지지 않았으면 볼륨은 손으로 지운다.
- 복제한 VM은 노드가 없고 UEFI 변수 사본은 복제한 노드에만 있다. 공유 풀이나 노드를 정하지 않은 풀이라 다른 노드에서 시작되면 새 UEFI 변수로 부팅한다.
- Task 없이 남은 `pending` 스냅샷(시간 초과 뒤 적용된 만들기, 또는 Task 만들기가 실패하고 그 정리도 실패한 경우)이 있으면 예약 작업은 그 VM을 말없이 건너뛴다. 계획 11에서는 주인 노드가 재시작할 때까지 이어졌고, 계획 12부터는 정리 작업이 30초쯤 뒤 그 리소스를 지운다.
- 리소스 없이 디스크에 남은 태그는 자동으로 지우지 않는다. 만들기가 시간 초과로 끝나도 남는다.
- 메모리 스냅샷으로 되돌린 뒤 VM이 시작되기 전에 opld가 재시작하면 VM은 메모리 상태 없이 처음부터 부팅한다(디스크·위치는 그 스냅샷).
- 메모리 불러오기가 실패하면 되돌리기 Task의 오류가 원래 오류 대신 "VM was stopped"로 보일 수 있다(reconciler가 끄기를 두 번에 쓴다). VM의 `failed` 오류에는 원래 오류가 남는다.
- 크기 늘리기의 줄은 Task를 받을 때 정한다. 그 뒤 볼륨을 붙인 VM의 스냅샷 Task와는 차례가 없다. 배치되지 않은 VM의 Task는 첫 디스크 볼륨의 노드에서 돌므로, 디스크가 여러 노드에 걸쳐 있으면 차례가 없을 수 있다(그때는 `qemu-img` 잠금이 막는다).
- 다른 노드의 API가 `pending` 스냅샷을 만들고 Task를 만들기 전의 짧은 틈에 주인 노드가 재시작 정리를 하면 그 스냅샷을 지울 수 있다. 그 만들기 Task는 "snapshot was deleted"로 실패한다(다시 하면 된다).
- 메모리 스냅샷을 불러오며 시작하는 중(`-S`로 띄워 `snapshot-load`를 하는 동안)에 opld가 재시작하면, 다시 붙을 때 `prelaunch`로 남은 VM으로 보고 `cont`를 보내 불러오기와 겹칠 수 있다.
- 실행 중 메모리 스냅샷의 UEFI 변수 사본은 저장이 끝난 뒤 복사한다. 그사이 게스트가 바꾼 변수가 들어가 메모리 상태보다 조금 뒤일 수 있다.
- VM 삭제 확인과 실제 삭제 사이에 만들어진 스냅샷은 막지 않는다(`oplctl vm delete --with-snapshots`는 예약을 먼저 끈다).
- `status.vmstate_bytes`는 채우지 않는다.
- 오프라인 노드에 배정된 만들기·되돌리기 Task는 시작하지 못하므로 Task 제한 시간이 적용되지 않는다. 계획 11에서는 그 노드가 돌아올 때까지 `pending`이었고, 계획 12부터는 정리 작업이 유예(기본 60초) 뒤 실패로 끝낸다(제거된 노드면 곧바로, 실행 중이던 것도). 그동안 그 VM의 전원 조작과 수정은 `vm_busy`다(강제 정지는 되돌리기 중에만 막는다). 반대로 오프라인 노드에 배정된 전원 Task가 끝나지 않으면 그 VM의 스냅샷 요청이 `vm_busy`다. VM을 다른 노드로 옮겨도(`spec.node`) 남은 Task 때문에 마찬가지다.
- 스냅샷이 가리키는 볼륨이 지금 다른 실행 중 VM에 붙어 있으면(되돌리기 뒤 떼어 낸 디스크를 다른 VM에 붙인 경우 등) 그 스냅샷의 삭제는 `qemu-img`의 이미지 잠금으로 실패한다(디스크는 손상되지 않는다). 스냅샷이 남으므로 그 VM이 멈출 때까지 원래 VM도 지울 수 없다.

## 12. 계획 분할

| 계획 | 범위 | 완료 기준 |
|---|---|---|
| 10 | `Snapshot` 리소스, QMP 작업 도우미, 드라이버(실행 중·정지), 만들기·되돌리기·삭제, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기, 재시작 정리, CLI, 웹 스냅샷 탭·크기 늘리기·VM 삭제 선택, E2E | 브라우저에서 실행 중인 VM의 메모리 스냅샷을 만들고 되돌려 그 순간부터 실행되는 E2E가 맥과 CI `linux-kvm`에서 통과 |
| 11 | 복제, 예약 스냅샷(리더 반복 작업, 보관 정리), CLI, 웹 | 스냅샷에서 복제한 VM이 시작되고, 예약 스냅샷이 만들어지고 보관 개수대로 정리되는 것을 E2E로 확인 |
```

`docs/superpowers/specs/2026-10-27-robustness-design.md` 전체:

```markdown
# opLite 클러스터·Task 견고성 설계 (계획 12)

작성일: 2026-10-27
상위 문서: `2026-10-20-opl-node-core-design.md`, `2026-10-26-snapshots-design.md`

## 1. 목표와 범위

계획 8–11에서 넘긴 사항 가운데, 데이터가 어긋나거나 VM이 멈춘 채 남는 문제를 고친다. 넘긴 사항은 셋으로 나눴다.

| 계획 | 묶음 | 항목 |
|---|---|---|
| **12 (이 문서)** | 클러스터·Task 견고성 | 오프라인·제거된 노드의 Task 끝내기, Task 없이 남은 `pending` 스냅샷 정리, 전달 재시도의 결과 판단, `schedule_error`의 code, 사용자 삭제와 예약 정리 삭제 구분, 스냅샷 만들기 중 강제 정지 |
| 13 | API·보안 보완 | 업로드 사전 확인 API, 비밀번호·로그인 시도 제한, 노드 아키텍처 API(머신 기본값) |
| 14 | 웹 정리 | 작업 목록 페이지 나누기, `index.html` ETag, VM 폼 컴포넌트 나누기, 번역 키 `common.*`, 비밀값 대화상자 뒤 포커스, 남은 전원 Task로 버튼 미리 막기 |

그 뒤에 하위 프로젝트 3의 나머지(공유 스토리지)를 한다.

넣지 않는 것: 실행 중이던 Task를 리더가 끝내기(네트워크만 끊긴 노드는 아직 일하고 있을 수 있다), 모든 쓰기의 정확히 한 번 적용(요청 id 표), 정리 작업이 끝낸 Task의 볼륨 파일을 다른 노드에서 지우기.

## 2. 타입

- **`VmStatus.schedule_error: Option<ScheduleError>`**, `ScheduleError { code: String, message: String }`. 역직렬화는 옛 문자열도 받아 `{ code: "unknown", message: <문자열> }`로 읽는다(저장된 상태를 옮기지 않는다). `code`는 API 오류 code(`snapshot_unsupported`, `vm_busy`, `node_offline` 등), Task 실패는 `task_failed`.
- **`TaskOp::SnapshotDelete { snapshot, #[serde(default)] scheduled: bool }`**. 예약 정리가 만든 삭제만 `true`. 옛 Task(필드 없음)는 `false`로 읽는다.
- 구현: `ScheduleError`는 직렬화하면 늘 `{code, message}`이고 OpenAPI에도 구조(`ScheduleError` 컴포넌트)로 보인다. 쓰는 곳은 모두 `scheduled: false`로 시작하고 예약 정리(`start_delete(…, scheduled: true)`)만 `true`다.

## 3. 정리 작업 `ClusterJanitor`

`opl-api/src/janitor.rs`. `SnapshotScheduler`와 같은 모양(spawn/shutdown, 리더 확인 → `read_barrier` → 목록)으로 리더에서 `tick`마다(기본 10초)돈다. 판단은 순수 함수(지금 시각, 노드·Task·스냅샷 목록, 기억한 상태 → 할 일)로 두고 단위 테스트한다.

1. **오프라인 노드의 Task.** 노드가 오프라인으로 처음 보인 시각을 메모리에 기억한다(온라인이 되면 지운다). 유예(`offline_grace`, 기본 60초)가 지나도 오프라인이면 그 노드에 배정된 `pending` Task를 실패로 끝낸다(`node {id} is offline`). `running` Task는 건드리지 않는다.
2. **제거된 노드의 Task.** Task의 `node`가 노드 목록에 없으면 `pending`·`running` 모두 곧바로 실패로 끝낸다(`node {id} was removed`).
3. **남은 `pending` 스냅샷.** `pending` 스냅샷에 끝나지 않은 `SnapshotCreate` Task가 없는 것을 처음 본 뒤 `orphan_grace`(기본 30초)가 지나도 그대로면 리소스를 지운다. 디스크의 태그는 남을 수 있다(스냅샷 설계 §6 "태그만 남은 경우"). 노드가 살아 있든 아니든 같다. 볼륨은 업로드 중에도 Task 없이 `pending`이라 정리하지 않는다.

정리 작업이 실패로 끝낸 Task는 실행 노드가 하던 정리를 하지 않는다. 볼륨(복제 볼륨 포함)은 그 노드가 재시작할 때 기존 고아 정리가 지우고, 스냅샷 리소스는 위 3이 지운다.

**경합.** Task 상태 쓰기를 조건부(`update_status_checked`, 계획 10)로 바꾼다. 리더는 읽은 revision에서 그 Task가 아직 `pending`(제거된 노드면 끝나지 않음)일 때만 실패로 쓴다. 노드의 실행기는 `pending` → `running` 쓰기를 읽은 revision으로 하고, 지면(그새 실패로 끝났다) 실행하지 않는다. 그 뒤의 상태 쓰기(성공·실패)는 지금처럼 그 노드만 쓴다.

**설정.** opld `[janitor] tick_secs = 10, offline_grace_secs = 60`(0은 거부). 테스트만 줄인다.

구현하며 정한 것:
- 판단 함수 `decide(now, config, nodes, members, tasks, snapshots, &mut seen) -> Vec<Action>`. 할 일은 `FailTask { id, revision, error }`와 `DeleteSnapshot { id, revision }`이고, 읽은 revision으로만 쓴다(`update_status_checked`, `delete(Some(revision))`). 쓰기가 실패하면(그새 바뀜 등) 로그만 남기고 다음 tick에 다시 본다. 실패 쓰기가 `revision_mismatch`로 거부되면 Task를 다시 읽어, 이 정리 작업의 오류로 `failed`면 쓰인 것으로 본다(전달 재시도에서 앞선 시도의 응답만 잃은 경우, §5).
- 남은 `pending` 스냅샷의 유예 `orphan_grace`는 `ClusterJanitorConfig`의 필드다(기본 30초). tick이 짧아도 API가 스냅샷을 만들고 Task를 만드는 사이(전달된 쓰기 하나에 최대 `write_timeout`, 기본 5초)에 지우지 않게 하려는 것이다. opld 설정에는 두지 않는다(늘 30초).
- "제거된 노드" = 노드 목록에 없고 **Raft 멤버십에도 없는** 노드. 멤버가 되었지만 아직 노드를 등록하지 않은 노드(조인 중)는 제거된 것이 아니다. 오류 문구의 `{id}`는 노드 UUID다.
- 리더가 아니면 기억한 상태(오프라인 시각, 고아 스냅샷을 처음 본 시각)를 지운다. 목록은 `read_barrier` 뒤에 읽는다.
- 끝낸 Task가 예약 스냅샷의 만들기이거나 예약 정리 삭제(`scheduled: true`)면 실행기와 같이 그 VM의 `schedule_error`에 `task_failed`와 Task 오류(`node {id} is offline` 등)를 쓴다(§6). `node_offline`은 예약 작업이 요청 전에 거절할 때의 code로 남긴다.
- 실행기의 시작(`begin`): Task를 이 노드에서 다시 읽어 `pending`이면 그 revision으로 `running`을 쓰고, 이미 `running`이면 실행한다(앞선 쓰기의 응답만 잃은 경우. `running`은 그 노드만 쓴다). 끝났거나 없으면(`NotFound` 거부 포함) 실행하지 않는다. 그 밖의 오류(`revision_mismatch` 포함)는 `poll_interval` 뒤 다시 읽어 판단한다.
- `[janitor]`가 없으면 기본값이다. `Member`가 예약 작업 다음에 띄우고 그 다음에 멈춘다.

## 4. 스냅샷 만들기 중 강제 정지

- `vm_action`의 강제 정지(`stop?mode=force`)는 끝나지 않은 `SnapshotRevert`가 있을 때만 409 `vm_busy`. 만들기·삭제·복제 중에는 받는다. 복제는 지금 상태면 VM이 이미 꺼져 있고, 실행 중 VM의 스냅샷이면 `-U`로 읽으므로 꺼도 된다. 일반 정지·시작·재부팅·수정은 그대로 막는다.
- 강제 정지 Task는 같은 VM의 대상별 줄에서 앞의 Task를 기다리지 않고 곧바로 돈다(기다리면 뜻이 없다).
- 만들기 Task는 QEMU가 죽어 QMP가 끊기면 실패한다. 이 오류는 "진행 중일 수 있음"이 아니므로 기존 정리 경로대로 태그를 지운다(꺼진 VM이면 `qemu-img snapshot -d`, 없으면 성공). 메모리 저장은 스냅샷 표를 마지막에 쓰므로 끊기면 공간 누수만 남는다(`qemu-img check -r leaks`로 회수).

구현하며 정한 것:
- 검사는 `check_no_revert_task`(오류 문구 `VM {name} is being reverted to a snapshot; …`). 강제 정지 Task는 원래 대상별 줄에 서지 않아(전원 Task) 실행기는 바꾸지 않았고, 테스트로 확인한다.
- **QEMU는 `snapshot-save`가 끝나기 전에는 `quit`을 처리하지 않는다**(저장이 메인 루프에서 돈다, QEMU 11.1에서 확인). 강제 정지는 약 15초(QMP 응답 제한 5초 + 강제 정지 대기 10초) 기다린 뒤 QEMU를 죽인다. 그 안에 끝나는 메모리 저장은 끝난 뒤 꺼지고 스냅샷은 성공한다. 그보다 오래 걸리는 저장(멈춘 저장, 아주 큰 메모리)만 끊긴다. 실제 QEMU 테스트는 vmstate가 쓰이기 시작한 뒤 QEMU를 `SIGSTOP`으로 멈춰 이 경우를 만든다.
- 만들기 Task가 실패하면 하이퍼바이저로 VM을 다시 본다. 저장하는 동안 VM이 꺼졌으면(`inspect`가 없음) 꺼진 VM처럼 스토리지(`qemu-img snapshot -d`)로 태그를 지운다. 오류는 "QMP connection closed"류다.
- 웹: 강제 정지는 `isReverting(vm, tasks)`(대상이 이 VM이고 끝나지 않은 `snapshot-revert` Task)일 때만 막고 까닭(`vms.forceStopReverting`)을 `aria-describedby`로 잇는다. 웹은 원래 스냅샷 Task로 강제 정지를 미리 막지 않았다. `isBusy`는 그대로다.

## 5. 전달 재시도의 결과 판단

`Cluster::request`가 리더에 전달한 시도가 응답 없이 끝나면(시도 제한 시간, 보낸 뒤 연결 끊김) "적용됐을 수 있음"으로 표시한다. 표시 뒤의 재시도가

- `Create`이고 같은 id로 `AlreadyExists`를 받으면 성공이다. 이 노드에 그 리소스가 적용될 때까지(남은 제한 시간 안에서) 기다려 그 revision을 돌려준다. id는 요청한 쪽이 만든 UUID v7이라 같은 id면 자기 요청이다.
- `Delete`이고 `NotFound`를 받으면 성공이다.
- 그 밖(조건부 수정 등)은 지금처럼 오류를 돌려준다(코드 주석).

구현하며 정한 것:
- "응답 없이 끝남" = 시도 제한 시간에 걸림, 또는 전달자가 `Unreachable`을 돌려줌. 연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 `Unreachable`은 늘 적용됐을 수 있다고 본다. 그래서 그 뒤의 `Delete`가 `NotFound`면 다른 쪽이 지웠어도 성공이다.
- 다시 보낸 `Create`가 같은 id로 `AlreadyExists`면 이 노드에 보일 때까지 기다려 **지금 revision**을(그새 바뀌었으면 바뀐 revision), `Delete`가 `NotFound`면 이 노드에서 사라질 때까지 기다려 **이 노드가 적용한 인덱스**를 돌려준다. 남은 제한 시간 안에 안 되면 `Timeout`이다. 공개 API는 바뀌지 않는다.

Raft 로그 형식은 바뀌지 않는다.

## 6. 예약 오류와 표시

- 예약 작업은 거절된 요청의 API 오류 code와 메시지를 쓴다. 예약 만들기 Task가 실패하면 `task_failed`와 Task 오류. 예약 정리 삭제(`scheduled: true`) Task가 실패할 때만 기록하고, 사용자가 지운 삭제의 실패는 기록하지 않는다. 같은 code·메시지면 쓰지 않는다. 성공한 예약 만들기가 지운다(지금과 같다).
- CLI `vm snapshot schedule <vm>`는 마지막 실패를 `last error: <code>: <message>`로 보인다.
- 웹 스냅샷 탭은 `errors.<code>` 번역을 보인다. `task_failed`는 번역 문구 뒤에 메시지를 덧붙이고, 번역이 없는 code(`unknown` 포함)는 메시지를 그대로 보인다.
- 웹은 스냅샷 만들기 중에도 강제 정지 메뉴를 켠다(끝나지 않은 되돌리기만 막는다. Task 목록은 이미 watch한다).
- 구현: 기록할 Task는 `recorded_vm(task_spec, find)`가 정한다(예약 스냅샷의 `SnapshotCreate`, `SnapshotDelete { scheduled: true }`). 예약 정리 삭제는 스냅샷을 찾지 않고 Task 대상(VM)으로 정해, 스냅샷이 이미 지워졌어도 기록한다. 실행기와 정리 작업(§3)이 같은 함수와 `task_failed(message)`를 쓴다. 사용자가 지운 예약 스냅샷의 삭제 실패는 기록하지 않는다.
- 구현: 웹 `codeMessage(t, code, message, own?)`(`i18n/errors.ts`, API 오류 표시와 같이 쓴다)는 `<own>.<code>` 전용 문구, `errors.<code>` 번역 순으로 찾아 `{{message}}`를 넘겨 번역하고(`task_failed`: "작업 오류: …"), 둘 다 없으면 메시지를 그대로 보인다. 스냅샷 탭은 `own = "snapshots.scheduleError"`로 부르고, 예약 전용 문구는 `vm_busy`("…건너뛰었습니다. 다음 기회에 다시 합니다.")와 `node_offline`(메시지의 노드를 괄호에 넣는다)이다. 새 번역 `errors.task_failed`, `errors.node_offline`, `snapshots.scheduleError.*`.

## 7. 테스트

- **단위:** `ScheduleError`가 옛 문자열을 읽는지, `SnapshotDelete`가 `scheduled` 없이 읽히는지. 정리 작업 판단(유예 전·후, 다시 온라인, 제거된 노드, 스냅샷 유예, `running`은 건드리지 않음).
- **클러스터(`opl-cluster`, 메모리 네트워크):** 첫 응답을 버리는 테스트용 전달자로, 다시 보낸 `Create`·`Delete`가 성공으로 끝나는지와 조건부 수정은 그대로인지.
- **API(mock):** 오프라인 노드의 `pending` Task가 유예 뒤 실패하고 `vm_busy`가 풀리는지, 정리 작업과 실행기의 시작이 겹칠 때 한쪽만 이기는지, 남은 `pending` 스냅샷 정리, 만들기 중 강제 정지 202·되돌리기 중 409, 강제 정지가 줄을 기다리지 않는지, 예약 오류 code와 사용자 삭제 실패를 기록하지 않는지.
- **실제 QEMU(`qemu-it`):** 실행 중 VM의 메모리 스냅샷을 만드는 동안 강제 정지 → 만들기 실패, 디스크에 태그 없음, VM 정지.
- **웹:** 오류 번역(code, `task_failed`, 모르는 code), 만들기 중 강제 정지 켜짐.
- **E2E:** 바꾸지 않는다(오프라인 노드는 브라우저 하나로 재현하기 어렵다).

## 8. 알고 받아들인 제한

- 정리 작업은 노드가 처음 오프라인으로 보인 시각을 메모리에만 둔다. 리더가 바뀌면 유예를 다시 잰다.
- 정리 작업의 실패 쓰기가 응답만 잃어 `revision_mismatch`로 돌아오면 Task를 다시 읽어 확인하는데, 그사이 리더가 바뀌어 이 노드의 저장소가 뒤처져 있으면 확인하지 못해 예약 오류를 남기지 않을 수 있다(Task 상태는 맞다).
- 정리 작업이 끝낸 Task의 볼륨·복제 볼륨은 그 노드가 재시작할 때 고아 정리가 지운다. 돌아오지 않는 노드면 손으로 지운다. 스냅샷 태그는 그 노드 디스크에 남는다.
- 실행 중이던 Task는 노드가 제거될 때만 리더가 끝낸다. 네트워크만 끊긴 노드의 Task는 돌아올 때까지 남는다.
- 강제 정지로 끊긴 메모리 스냅샷은 qcow2에 공간 누수를 남길 수 있다.
- 전달 재시도의 결과 판단은 만들기·삭제만 한다. 조건부 수정은 다시 보낸 요청이 `revision_mismatch`로 끝날 수 있다.
- 정리 작업이 끝낸 Task의 오류는 영어 서버 문구이고, 웹은 `task_failed` 번역 뒤에 그대로 붙인다.
- 강제 정지는 진행 중인 메모리 저장이 끝나기를 약 15초까지 기다린다(QEMU가 저장 뒤에야 `quit`을 처리한다, §4). 그 안에 끝나지 않는 저장(멈춘 저장, 아주 큰 메모리)은 QEMU를 죽여 끊는다.
- 정리 작업이 `pending` 스냅샷 리소스를 먼저 지우므로, 만들기 시간 초과와 실행기 정리 실패가 겹치면 그 노드가 재시작할 때의 정리가 그 태그를 다시 지울 기회가 없어 qcow2 공간이 샌다.
- 정리 작업이 실패로 끝낸 전원 Task도 `spec.power`는 이미 바뀌었으므로, 노드가 돌아오면 reconciler가 그 시작·정지를 한다(전원 Task는 기다리는 역할만 한다).
- 웹 메뉴의 비활성 강제 정지 까닭은 `aria-describedby`로 이었지만 Radix가 비활성 항목을 키보드로 건너뛰어 읽히지 않을 수 있다. 상세 화면에 같은 까닭이 보인다.
- 옛 노드와 섞인 클러스터는 지원하지 않는다(옛 노드는 `scheduled` 필드와 구조로 바뀐 `schedule_error`를 모른다).

## 9. 계획

한 계획(계획 12)으로 한다. 완료 기준: 위 테스트가 macOS와 CI(`linux-kvm` 포함)에서 통과하고, 오프라인 노드에 걸린 스냅샷 Task가 유예 뒤 끝나 VM 조작이 풀리며, 스냅샷 만들기 중 강제 정지로 VM을 끌 수 있다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 357개, 실패 0

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 6개(약 35초)

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS 137개, 실패 0

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개(약 26초)

- [ ] **Step 3: 커밋**

```bash
git add docs
git commit -m "docs: 계획 12 구현하며 정한 것과 스냅샷 설계의 바뀐 동작"
```

---

## 완료 기준 (계획 12)

- 오프라인 노드에 걸린 `pending` 스냅샷 Task가 유예 뒤 실패로 끝나 `vm_busy`가 풀리고, Task 없이 남은 `pending` 스냅샷이 `orphan_grace` 뒤 지워지며, 실행기가 먼저 시작한 Task에 정리 작업의 옛 revision 실패 쓰기가 거부된다(`cargo test -p opl-api --test janitor`와 `janitor.rs` 단위 테스트).
- 응답을 잃은 전달 뒤 다시 보낸 만들기·삭제가 성공으로 끝나고 조건부 수정은 그대로 거부된다(`cargo test -p opl-cluster --test three_nodes`).
- 실제 QEMU 통합 테스트(`cargo test -p opld --features qemu-it --test snapshots`)가 메모리 스냅샷을 쓰는 동안 강제 정지 → 만들기 실패, 디스크에 태그 없음, VM 정지를 확인한다. CI `linux-kvm`(QEMU 8.2)에서도 통과한다.
- 예약 오류가 code로 남고(거절 code, `task_failed`), 사용자 삭제 실패는 기록하지 않으며, 웹이 그 code를 번역한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **오프라인 유예는 메모리에:** 정리 작업은 노드가 처음 오프라인으로 보인 시각을 메모리에만 둔다. 리더가 바뀌면 유예를 다시 잰다.
- **끝낸 Task의 정리:** 정리 작업이 끝낸 Task의 볼륨·복제 볼륨은 그 노드가 재시작할 때 고아 정리가 지운다. 돌아오지 않는 노드면 손으로 지운다. 스냅샷 태그는 그 노드 디스크에 남는다.
- **실행 중이던 Task:** 노드가 제거될 때만 리더가 끝낸다. 네트워크만 끊긴 노드의 Task는 돌아올 때까지 남는다.
- **끊긴 메모리 스냅샷:** 강제 정지로 끊긴 메모리 스냅샷은 qcow2에 공간 누수를 남길 수 있다(`qemu-img check -r leaks`로 회수).
- **전달 재시도:** 결과 판단은 만들기·삭제만 한다. 조건부 수정은 다시 보낸 요청이 `revision_mismatch`로 끝날 수 있다.
- **Task 오류 문구:** 정리 작업이 끝낸 Task의 오류는 영어 서버 문구이고, 웹은 `task_failed` 번역 뒤에 그대로 붙인다.
- **강제 정지 대기:** 강제 정지는 진행 중인 메모리 저장이 끝나기를 약 15초까지 기다린다(QEMU가 저장 뒤에야 `quit`을 처리한다). 그 안에 끝나지 않는 저장(멈춘 저장, 아주 큰 메모리)은 QEMU를 죽여 끊는다.
- **pending 스냅샷의 태그:** 정리 작업이 `pending` 스냅샷 리소스를 먼저 지우므로, 만들기 시간 초과와 실행기 정리 실패가 겹치면 그 노드가 재시작할 때의 정리가 그 태그를 다시 지울 기회가 없어 qcow2 공간이 샌다.
- **끝낸 전원 Task:** 정리 작업이 실패로 끝낸 전원 Task도 `spec.power`는 이미 바뀌었으므로, 노드가 돌아오면 reconciler가 그 시작·정지를 한다(전원 Task는 기다리는 역할만 한다).
- **메뉴의 비활성 까닭:** 웹 메뉴의 비활성 강제 정지 까닭은 `aria-describedby`로 이었지만 Radix가 비활성 항목을 키보드로 건너뛰어 읽히지 않을 수 있다. 상세 화면에 같은 까닭이 보인다.
- **응답을 잃은 실패 쓰기:** 정리 작업의 실패 쓰기가 응답만 잃어 `revision_mismatch`로 돌아오면 Task를 다시 읽어 확인한다. 그사이 리더가 바뀌어 이 노드가 뒤처져 있으면 예약 오류를 남기지 않을 수 있다(Task 상태는 맞다).
- **섞인 클러스터:** 옛 노드와 섞인 클러스터는 지원하지 않는다(옛 노드는 `scheduled` 필드와 구조로 바뀐 `schedule_error`를 모른다).
- 그 밖의 경합과 순서 제한은 설계 문서 §8과 스냅샷 설계 §11에 있다.
