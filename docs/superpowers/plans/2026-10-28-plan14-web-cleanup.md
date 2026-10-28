# opLite 계획 14: 웹 정리 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 계획 8·9·12에서 넘긴 웹 정리 사항을 마친다. 끝난 Task가 끝없이 쌓이지 않게 janitor가 지우고, 내장 웹 파일에 ETag를 붙이며, 웹은 Task 색인으로 행마다 목록을 훑지 않고, 처리 중인 전원 Task로 버튼을 미리 막고, 막힌 동작의 이유를 보이고 읽게 하며, 작업 목록을 페이지로 나누고, VM 폼·번역 키·대화상자 뒤 포커스를 정리한다.

**Architecture:** 리더의 `ClusterJanitor`가 tick마다 끝난 Task를 UUIDv7 id의 시각으로 7일·1000개 기준으로 지운다(tick당 200개, 끝난 뒤 10분 유예). opld `web.rs`는 rust-embed의 sha256으로 강한 ETag를 붙이고 `If-None-Match`(약한 비교)에 304로 답한다. 웹은 `TaskIndexProvider`가 끝나지 않은 Task를 대상별로 한 번 묶고, `availability(vm, action, vmTasks)`가 버튼을 막을 이유(번역 키)를 돌려주며, `ReasonMenuItem`·`ReasonButton`·`ReasonList`가 `aria-disabled`와 `aria-describedby`로 이유를 보이고 읽게 한다. `usePage`·`Pager`가 50개씩 나눈다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft, axum 0.8, rust-embed; 웹은 계획 8~13과 같다(React 19.3, TanStack Query 5, shadcn/ui·Radix, i18next, Vitest, MSW). 새 의존성은 없다(opld가 워크스페이스의 `hex`를 쓴다).

**설계 문서:** `docs/superpowers/specs/2026-10-28-web-cleanup-design.md` (전체가 이 계획)
**선행 계획:** 계획 1~13 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| janitor의 끝난 Task 보존 정책(7일·1000개, tick당 200개, opld `[janitor]` 설정) | `GET /tasks`의 서버 페이지 나누기, Task를 감시 스트림에서 빼기, Task의 끝난 시각 필드 |
| 내장 웹 파일의 ETag와 304 | |
| Task 색인과 화면별 `Map` 조회(행 비용) | |
| 처리 중인 전원 Task로 시작·정지·재시작 막기(웹) | 서버에서 처리 중인 전원 Task로 요청 거절(새 전원 Task가 앞 것을 대체하는 동작은 그대로) |
| 막힌 메뉴 항목·버튼의 이유 표시와 화면 읽기 | |
| 작업 목록·VM 작업 탭 50개씩 페이지 | |
| VM 폼 섹션 나누기, `common.*` 번역 키, 비밀값 대화상자 뒤 포커스 | |
| | 공유 스토리지(NFS·LVM·ZFS·iSCSI·Ceph) — 계획 14 뒤 |

## 이전 계획에서 이어받는 사항

- 계획 8·9: 작업 목록 페이지(Task 6), `index.html` 캐시(Task 2), VM 폼 나누기(Task 7), 공통 번역 키(Task 8), 대화상자 뒤 포커스(Task 9), 막힌 메뉴의 이유(Task 5).
- 계획 12: 남은 전원 Task로 버튼 미리 막기(Task 4), 목록 행마다 Task 목록을 훑는 비용(Task 3).

## 실행 환경

계획 13과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 실제 QEMU 테스트는 기능으로 켠다: `opld`의 `qemu-it`(`smoke`·`e2e`), `opl-api`의 `storage-it`, `opld`의 `web-it`(브라우저 E2E). Linux CI(`linux-kvm`)는 QEMU 8.2다. 이 계획은 QEMU를 새로 쓰지 않는다.

`Cargo.lock`은 Task 2에서 한 줄(opld의 `hex` 의존) 바뀐다. `cargo build`가 저절로 고친다. API는 바뀌지 않으므로 `schema.d.ts`는 그대로다.

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **보존.** 끝난(`succeeded`·`failed`) Task만 지운다. 만든 시각은 Task id(UUIDv7)의 시각이다. 기간 규칙: 나이가 `task_max_age`(7일)보다 크면 지운다. 개수 규칙: 그 뒤에도 끝난 Task가 `task_max_finished`(1000)를 넘으면 오래된 것부터 지운다. 두 규칙 모두 리더가 끝난 것을 처음 본 뒤 `finished_grace`(10분)가 지난 것만 지운다(oplctl `wait_task`·웹 `waitForTask`가 결과를 놓치지 않게). tick당 200개, 오래된 것부터, 리비전을 걸고 지운다. 쓰기가 `Rejected`가 아닌 오류로 실패하면 이 tick의 남은 삭제를 건너뛴다. opld는 0과 36500일 넘는 값을 거절한다. 실행기는 상태를 쓸 Task가 지워졌으면(`NotFound`) 다시 시도하지 않는다.
- **ETag.** 모든 내장 파일 응답에 `"<sha256 hex>"`. `If-None-Match`는 RFC 9110 약한 비교(`W/`를 떼고 비교, `*`는 모두)로, 맞으면 본문 없는 304에 ETag·Cache-Control·보안 헤더. `Cache-Control`은 그대로(`assets/*`는 immutable, 그 밖은 `no-cache`).
- **Task 색인.** `TaskIndexProvider`가 `LiveProvider` 안쪽에서 `useList("task")`를 한 번 읽어 대상 id → 끝나지 않은 Task 목록을 만든다. Provider 밖에서는 빈 `Map`. 행은 `useTaskIndex().get(vm.id) ?? []`.
- **전원 버튼.** `availability(vm, action, vmTasks)`: 시작·정지·재시작은 끝나지 않은 전원 Task가 있으면 `vms.reason.powerPending`(상태보다 먼저), 그다음 상태(`failed`·`stopped`·`busy`·`running`), 강제 정지는 되돌리는 중이면 `reverting`. 서버는 그대로다.
- **비활성 이유.** `disabled` 대신 `aria-disabled`로 키보드로 닿게 하고, 선택·클릭은 `preventDefault`만 한다. 메뉴 항목은 이름 옆에 이유(`aria-hidden` + `aria-describedby`, typeahead는 `textValue`로 이름만), 버튼은 같은 영역에서 `ReasonList`로 이유를 한 번 그린다.
- **페이지.** `usePage(items, 50)`, 항목이 줄면 마지막 페이지로, 필터를 바꾸면 첫 페이지. 이전/다음은 `aria-disabled`로 포커스를 잃지 않는다.
- **동작을 바꾸지 않는 정리.** VM 폼 나누기(`VmFormPage.test.tsx`를 고치지 않고 통과), `common.*` 번역 키(화면 문구 그대로).

## 파일 구조

```
crates/opl-api/src/{janitor.rs, tasks.rs}, crates/opl-api/tests/{janitor.rs, tasks.rs}   # 보존 정책, 지워진 Task의 상태 쓰기
crates/opld/src/{config.rs, web.rs}, crates/opld/Cargo.toml                            # [janitor] 설정, ETag
web/src/api/taskIndex.tsx(새로)                       # TaskIndexProvider, useTaskIndex
web/src/vms/actions.ts                                # availability
web/src/components/{Reason.tsx, Pager.tsx}(새로)       # 비활성 이유, 페이지
web/src/vms/form/{General,Disks,Nics,Schedule}Section.tsx, web/src/vms/form/submit.ts(새로)   # VM 폼 나누기
web/src/{tasks,vms,storage,nodes,accounts,cluster,...}/*.tsx, web/src/i18n/{ko,en}.json
README.md, docs/superpowers/specs/2026-10-28-web-cleanup-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: janitor: 끝난 Task 보존 정책

리더의 `ClusterJanitor`가 tick마다 끝난(`succeeded`·`failed`) Task를 지운다. 만든 시각은 Task id(UUIDv7)의 시각(`created`)이다. 기간 규칙: 나이가 `task_max_age`(기본 7일)보다 크면 지운다. 개수 규칙: 그 뒤에도 끝난 Task가 `task_max_finished`(기본 1000)를 넘으면 넘는 만큼 오래된 것부터 지우되, 리더가 끝난 것을 처음 본 뒤(`Seen.finished`) `finished_grace`(기본 10분)가 지난 것만 지운다. 끝나지 않은 Task는 지우지 않는다. 한 tick에 최대 `TASK_DELETES_PER_TICK`(200)개, 오래된 것부터, 읽은 리비전을 걸고 지운다(`Action::DeleteTask`). 삭제 쓰기가 `Rejected`가 아닌 오류로 실패하면 이 tick의 남은 삭제를 건너뛴다(`stops_deletes`). 실행기의 `set_status`는 Task가 지워졌으면(`Rejected(NotFound)`) 다시 시도하지 않는다. opld 설정 `[janitor]`에 `task_max_age_days`(기본 7, 0과 36500 초과는 거절)와 `task_max_finished`(기본 1000, 0은 거절)를 더한다. 테스트: `expired_tasks` 단위(기간, 경계, 같은 시각의 id 순, 개수, 유예, tick 상한, 멈춤 규칙), 통합(기간·개수), 지워진 Task가 큐를 막지 않음, opld 설정.

**Files:**
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/tests/janitor.rs`
- Modify: `crates/opl-api/tests/tasks.rs`
- Modify: `crates/opld/src/config.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/janitor.rs` 전체:

```rust
//! 클러스터 정리 작업(견고성 설계 §3). 리더만 `tick`마다 노드·Task·스냅샷 목록을 보고, 오프라인이
//! 유예보다 오래 간 노드의 `pending` Task와 제거된 노드의 끝나지 않은 Task를 실패로 끝내고, 만들
//! Task 없이 남은 `pending` 스냅샷을 지운다. 실패로 끝낸 Task의 정리(볼륨·태그)는 하지 않는다.
//! 예약 스냅샷의 만들기·예약 정리의 삭제를 끝냈으면 실행기처럼 `task_failed`로 VM에 남긴다.
//! 끝난 Task는 기간·개수 기준으로 지운다(웹 정리 설계 §2).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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
    /// 만든 지 이보다 오래된 끝난 Task를 지운다. 만든 시각은 UUIDv7 id의 시각이다.
    pub task_max_age: Duration,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    pub task_max_finished: usize,
    /// 개수 기준으로는 끝난 것을 처음 본 뒤 이만큼 지난 Task만 지운다. 막 끝난 Task를 기다리는
    /// 쪽(oplctl `wait_task`, 웹 `waitForTask`)이 결과를 읽을 시간이다.
    pub finished_grace: Duration,
}

/// 한 tick에 지우는 끝난 Task의 최대 수. 나머지는 다음 tick에 지운다.
const TASK_DELETES_PER_TICK: usize = 200;

impl Default for ClusterJanitorConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            offline_grace: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(30),
            task_max_age: Duration::from_secs(7 * 24 * 3600),
            task_max_finished: 1000,
            finished_grace: Duration::from_secs(600),
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
    /// 끝난 Task와 처음 끝난 것으로 보인 시각
    finished: HashMap<Uuid, Instant>,
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
    DeleteTask {
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

/// 지울 끝난 Task를 오래된 것부터 최대 `limit`개 고르고 `seen`을 고친다. 만든 지 `task_max_age`가
/// 넘은 것과, 그러고도 `task_max_finished`를 넘는 만큼 오래된 것 중 끝난 것을 처음 본 뒤
/// `finished_grace`가 지난 것이다. 끝나지 않은 Task는 고르지 않는다.
fn expired_tasks(
    now: SystemTime,
    instant: Instant,
    config: &ClusterJanitorConfig,
    tasks: &[Task],
    seen: &mut Seen,
    limit: usize,
) -> Vec<Action> {
    let mut finished: Vec<(SystemTime, &Task)> = tasks
        .iter()
        .filter(|t| t.status.state.is_finished())
        .map(|t| (created(t.id), t))
        .collect();
    finished.sort_by_key(|(at, t)| (*at, t.id));
    // 지워졌거나 없어진 Task는 잊는다.
    seen.finished = finished
        .iter()
        .map(|(_, t)| (t.id, seen.finished.get(&t.id).copied().unwrap_or(instant)))
        .collect();
    let over = finished.len().saturating_sub(config.task_max_finished);
    let old = finished
        .iter()
        .take_while(|(at, _)| now.duration_since(*at).unwrap_or_default() > config.task_max_age)
        .count();
    let settled = |t: &Task| instant.duration_since(seen.finished[&t.id]) >= config.finished_grace;
    finished
        .iter()
        .enumerate()
        .filter(|(i, (_, t))| *i < old || (*i < over && settled(t)))
        .take(limit)
        .map(|(_, (_, t))| Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        })
        .collect()
}

/// UUIDv7 id에 담긴 만든 시각. 시각이 없는 id는 가장 오래된 것으로 본다.
fn created(id: Uuid) -> SystemTime {
    id.get_timestamp().map_or(SystemTime::UNIX_EPOCH, |ts| {
        let (secs, nanos) = ts.to_unix();
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    })
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
    let now = Instant::now();
    let mut actions = decide(now, config, &nodes, &ids, &tasks, &snapshots, seen);
    actions.extend(expired_tasks(
        SystemTime::now(),
        now,
        config,
        &tasks,
        seen,
        TASK_DELETES_PER_TICK,
    ));
    let mut skip_deletes = false;
    for action in actions {
        if skip_deletes && matches!(action, Action::DeleteTask { .. }) {
            continue;
        }
        let result = apply(cluster, &action).await;
        // 그사이 바뀌었으면(실행기가 시작했다 등) 다음 tick에 다시 본다.
        match result {
            Ok(_) => tracing::info!(?action, "janitor"),
            Err(e) if written_anyway(cluster, &action, &e) => {
                tracing::info!(?action, "janitor (reply lost)")
            }
            Err(e) => {
                tracing::info!(?action, error = %e, "janitor action not applied");
                if stops_deletes(&action, &e) {
                    tracing::warn!(error = %e, "janitor: deleting finished tasks again next tick");
                    skip_deletes = true;
                }
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
        Action::DeleteTask { id, revision } => {
            cluster.delete::<TaskSpec>(*id, Some(*revision)).await
        }
    }
}

/// 끝난 Task 지우기가 거부(그사이 바뀜·없어짐)가 아닌 오류(시간 초과, 리더 아님 등)로 실패했으면
/// 클러스터가 쓰기를 받지 못하는 것이므로 이 tick의 남은 지우기는 건너뛴다(한 tick에 최대
/// `TASK_DELETES_PER_TICK`번 기다리지 않게).
fn stops_deletes(action: &Action, err: &ClusterError) -> bool {
    matches!(action, Action::DeleteTask { .. }) && !matches!(err, ClusterError::Rejected(_))
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
    const FINISHED_GRACE: Duration = Duration::from_secs(600);
    const CONFIG: ClusterJanitorConfig = ClusterJanitorConfig {
        tick: Duration::from_secs(10),
        offline_grace: GRACE,
        orphan_grace: ORPHAN_GRACE,
        task_max_age: Duration::from_secs(7 * 24 * 3600),
        task_max_finished: 1000,
        finished_grace: FINISHED_GRACE,
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

    /// `ago` 전에 만든 끝난(`state`) Task
    fn task_made(now: SystemTime, ago: Duration, state: TaskState) -> Task {
        let at = (now - ago).duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), state);
        t.id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            at.as_secs(),
            at.subsec_nanos(),
        ));
        t
    }

    fn delete(t: &Task) -> Action {
        Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        }
    }

    /// 끝난 것을 처음 본 뒤 `finished_grace`가 지난 리더가 고르는 것
    fn expired(
        now: SystemTime,
        config: &ClusterJanitorConfig,
        tasks: &[Task],
        limit: usize,
    ) -> Vec<Action> {
        let t0 = Instant::now();
        let mut seen = Seen::default();
        expired_tasks(now, t0, config, tasks, &mut seen, limit);
        let later = t0 + config.finished_grace;
        expired_tasks(now, later, config, tasks, &mut seen, limit)
    }

    #[test]
    fn just_finished_tasks_over_the_max_count_stay_for_the_grace() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let day = Duration::from_secs(24 * 3600);
        let older = task_made(now, 2 * min, TaskState::Succeeded);
        let newer = task_made(now, min, TaskState::Succeeded);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at =
            |instant, tasks: &[Task]| expired_tasks(now, instant, &config, tasks, &mut seen, 200);
        // 막 끝난 것으로 보인 Task는 개수가 넘어도 남는다.
        let tasks = [older.clone(), newer.clone()];
        assert_eq!(at(t0, &tasks), vec![]);
        assert_eq!(
            at(t0 + FINISHED_GRACE - Duration::from_secs(1), &tasks),
            vec![]
        );
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&older)]);
        // 지워진 Task는 잊고, 새로 끝난 Task는 처음 본 때부터 잰다.
        let newest = task_made(now, Duration::ZERO, TaskState::Failed);
        let tasks = [newer.clone(), newest];
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&newer)]);
        // 기간 기준은 유예를 기다리지 않는다.
        let old = task_made(now, 8 * day, TaskState::Succeeded);
        assert_eq!(
            at(t0 + FINISHED_GRACE, std::slice::from_ref(&old)),
            vec![delete(&old)]
        );
        assert!(seen.finished.contains_key(&old.id));
        assert!(!seen.finished.contains_key(&older.id));
    }

    #[test]
    fn finished_tasks_older_than_the_max_age_go() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let old_ok = task_made(now, 8 * day, TaskState::Succeeded);
        let old_failed = task_made(now, 9 * day, TaskState::Failed);
        let young = task_made(now, 6 * day, TaskState::Succeeded);
        // 끝나지 않은 Task는 오래됐어도 남긴다.
        let old_pending = task_made(now, 30 * day, TaskState::Pending);
        let old_running = task_made(now, 30 * day, TaskState::Running);
        let tasks = [
            old_ok.clone(),
            young,
            old_pending,
            old_failed.clone(),
            old_running,
        ];
        // 오래된 것부터 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 200),
            vec![delete(&old_failed), delete(&old_ok)]
        );
    }

    #[test]
    fn finished_tasks_over_the_max_count_go_oldest_first() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 2,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let finished: Vec<Task> = (1..=4)
            .map(|i| task_made(now, i * min, TaskState::Succeeded))
            .collect();
        let pending = task_made(now, 10 * min, TaskState::Pending);
        let mut tasks = finished.clone();
        tasks.push(pending);
        // 끝나지 않은 Task는 세지 않는다.
        assert_eq!(
            expired(now, &config, &tasks, 200),
            vec![delete(&finished[3]), delete(&finished[2])]
        );
        assert_eq!(expired(now, &config, &tasks[2..], 200), vec![]);
    }

    #[test]
    fn a_task_exactly_at_the_max_age_stays_and_ties_go_by_id() {
        let now = SystemTime::now();
        // id의 시각은 밀리초까지다.
        let now = SystemTime::UNIX_EPOCH
            + Duration::from_millis(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            );
        let at_limit = task_made(now, CONFIG.task_max_age, TaskState::Succeeded);
        assert_eq!(expired(now, &CONFIG, &[at_limit], 200), vec![]);

        // 만든 시각이 같으면 id가 작은 것을 먼저 지운다.
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let mut tied = [
            task_made(now, min, TaskState::Succeeded),
            task_made(now, min, TaskState::Failed),
        ];
        assert_ne!(tied[0].id, tied[1].id);
        tied.sort_by_key(|t| std::cmp::Reverse(t.id));
        assert_eq!(expired(now, &config, &tied, 200), vec![delete(&tied[1])]);
    }

    #[test]
    fn deletes_per_tick_are_capped() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let tasks: Vec<Task> = (0..5)
            .map(|i| task_made(now, (10 + i) * day, TaskState::Failed))
            .collect();
        // 상한까지 오래된 것부터 고르고 나머지는 다음 tick에 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 2),
            vec![delete(&tasks[4]), delete(&tasks[3])]
        );
        assert_eq!(expired(now, &CONFIG, &tasks[..3], 2).len(), 2);
        assert_eq!(expired(now, &CONFIG, &tasks[..1], 2).len(), 1);
    }

    #[test]
    fn a_task_delete_that_cannot_be_written_stops_the_rest_of_the_deletes() {
        let t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Succeeded);
        let rejected = || {
            ClusterError::Rejected(Rejection::RevisionMismatch {
                expected: 1,
                actual: 2,
            })
        };
        let not_leader = || ClusterError::NotLeader {
            leader_id: None,
            leader_addr: None,
        };
        // 쓰기를 받지 못하면 남은 지우기를 다음 tick으로 미룬다.
        for err in [
            ClusterError::Timeout,
            ClusterError::Unreachable("x".into()),
            not_leader(),
        ] {
            assert!(stops_deletes(&delete(&t), &err), "{err:?}");
        }
        // 그 Task만 바뀌었거나 없어진 것이면 다음 것을 지운다.
        assert!(!stops_deletes(&delete(&t), &rejected()));
        // 다른 동작의 실패는 지우기를 막지 않는다.
        assert!(!stops_deletes(
            &fail(&t, "x".into()),
            &ClusterError::Timeout
        ));
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

`crates/opl-api/src/tasks.rs`에서

```rust
            Ok(_) => return,
            Err(e) => {
```

을 다음으로 바꾼다:

```rust
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
```

`crates/opl-api/tests/janitor.rs`에서

```rust
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지.

mod common;

use std::time::{Duration, Instant};
```

을 다음으로 바꾼다:

```rust
//! `running` Task는 남는지, 제거된 노드의 Task가 곧바로 실패하는지, 만들 Task 없이 남은 `pending`
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지, 끝난 Task를 기간·개수
//! 기준으로 지우고 끝나지 않은 Task는 남기는지.

mod common;

use std::time::{Duration, Instant, SystemTime};
```

`crates/opl-api/tests/janitor.rs`에서

```rust
            orphan_grace: GRACE,
        },
```

을 다음으로 바꾼다:

```rust
            orphan_grace: GRACE,
            ..ClusterJanitorConfig::default()
        },
```

`crates/opl-api/tests/janitor.rs`에서

```rust
async fn task(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) -> Task {
    let mut t: Task = Resource::new("", TaskSpec { op, target, node });
    t.name = t.id.to_string();
```

을 다음으로 바꾼다:

```rust
async fn task(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) -> Task {
    task_with_id(env, Uuid::now_v7(), op, target, node, state).await
}

/// `ago` 전에 만든 것처럼 id의 시각을 정한 Task
async fn old_task(env: &Env, ago: Duration, node: Uuid, state: TaskState) -> Task {
    let at = (SystemTime::now() - ago)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    let id = Uuid::new_v7(uuid::Timestamp::from_unix(
        uuid::NoContext,
        at.as_secs(),
        at.subsec_nanos(),
    ));
    task_with_id(env, id, TaskOp::VmStart, Uuid::now_v7(), node, state).await
}

async fn task_with_id(
    env: &Env,
    id: Uuid,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
    state: TaskState,
) -> Task {
    let mut t: Task = Resource::new("", TaskSpec { op, target, node });
    t.id = id;
    t.name = t.id.to_string();
```

`crates/opl-api/tests/janitor.rs`에서

```rust
    env.cluster.get::<TaskSpec>(id).unwrap().unwrap()
}
```

을 다음으로 바꾼다:

```rust
    env.cluster.get::<TaskSpec>(id).unwrap().unwrap()
}

fn task_ids(env: &Env) -> Vec<Uuid> {
    let (_, items) = env.cluster.list::<TaskSpec>().unwrap();
    items.into_iter().map(|t| t.id).collect()
}
```

`crates/opl-api/tests/janitor.rs`에서

```rust
            orphan_grace: Duration::from_secs(3600),
        },
```

을 다음으로 바꾼다:

```rust
            orphan_grace: Duration::from_secs(3600),
            ..ClusterJanitorConfig::default()
        },
```

`crates/opl-api/tests/janitor.rs`에서

```rust
    assert_eq!(get_task(&env, pending.id).status.state, TaskState::Failed);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(get_task(&env, pending.id).status.state, TaskState::Failed);
}

#[tokio::test]
async fn old_finished_tasks_go_and_unfinished_ones_stay() {
    let env = Env::start().await;
    // 실행기가 집어 가지 않도록 다른 온라인 노드에 배정한다.
    let node = other_node(&env, "node2", true).await;
    let day = Duration::from_secs(24 * 3600);
    let succeeded = old_task(&env, 8 * day, node, TaskState::Succeeded).await;
    let failed = old_task(&env, 9 * day, node, TaskState::Failed).await;
    let young = old_task(&env, 6 * day, node, TaskState::Succeeded).await;
    let pending = old_task(&env, 30 * day, node, TaskState::Pending).await;
    let running = old_task(&env, 30 * day, node, TaskState::Running).await;
    let janitor = fast_janitor(&env);
    until("old finished tasks deleted", || {
        let ids = task_ids(&env);
        !ids.contains(&succeeded.id) && !ids.contains(&failed.id)
    })
    .await;
    // 몇 tick 더 지나도 남은 것은 그대로다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    janitor.shutdown().await;
    let mut left = task_ids(&env);
    left.sort();
    let mut expected = vec![young.id, pending.id, running.id];
    expected.sort();
    assert_eq!(left, expected);
    assert_eq!(get_task(&env, pending.id).status.state, TaskState::Pending);
}

#[tokio::test]
async fn finished_tasks_over_the_limit_go_oldest_first() {
    let env = Env::start().await;
    let node = other_node(&env, "node2", true).await;
    let min = Duration::from_secs(60);
    let mut finished = vec![];
    for i in (1..=5).rev() {
        finished.push(old_task(&env, i * min, node, TaskState::Succeeded).await);
    }
    let pending = old_task(&env, 10 * min, node, TaskState::Pending).await;
    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            task_max_finished: 3,
            finished_grace: GRACE,
            ..ClusterJanitorConfig::default()
        },
    );
    until("over the limit", || task_ids(&env).len() == 4).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    janitor.shutdown().await;
    // 가장 최근 3개와 끝나지 않은 Task가 남는다.
    let mut left = task_ids(&env);
    left.sort();
    let mut expected: Vec<Uuid> = finished[2..].iter().map(|t| t.id).collect();
    expected.push(pending.id);
    expected.sort();
    assert_eq!(left, expected);
}
```

`crates/opl-api/tests/tasks.rs`에서

```rust
//! Task 실행기의 재시작 처리.

mod common;

use common::Env;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
//! Task 실행기의 재시작 처리와 실행 중에 지워진 Task.

mod common;

use std::sync::Arc;

use common::{Env, FakeStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use reqwest::StatusCode;
use uuid::Uuid;
```

`crates/opl-api/tests/tasks.rs`에서

```rust
            .is_some()
    );
}
```

을 다음으로 바꾼다:

```rust
            .is_some()
    );
}

#[tokio::test]
async fn a_task_deleted_while_running_does_not_block_its_queue() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
        "data",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    let storage = Arc::new(FakeStorage::default());
    storage.set_size(volume.id, 1 << 20);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;

    // 크기 늘리기가 도는 사이에 Task가 지워졌다. 결과를 쓸 곳이 없으므로 끝난 것으로 본다.
    storage.pause();
    let resize = |size: u64| {
        admin.post(
            "/volumes/data/resize",
            serde_json::json!({ "size_bytes": size }),
        )
    };
    let (status, first) = resize(2 << 20).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    storage.reached().await;
    let id: Uuid = first["id"].as_str().unwrap().parse().unwrap();
    env.cluster.delete::<TaskSpec>(id, None).await.unwrap();
    storage.resume();

    // 같은 볼륨의 줄에 선 다음 Task가 돈다.
    let (status, second) = resize(3 << 20).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{second}");
    let second = admin.wait_task(&second).await;
    assert_eq!(second["status"]["state"], "succeeded", "{second}");
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

/// 클러스터 정리 작업. 간격·유예는 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JanitorSection {
    /// 목록을 확인하는 간격(초)
    #[serde(default = "default_janitor_tick_secs")]
    pub tick_secs: u64,
    /// 오프라인 노드의 `pending` Task를 실패로 끝내기까지 기다리는 시간(초)
    #[serde(default = "default_offline_grace_secs")]
    pub offline_grace_secs: u64,
    /// 만든 지 이 날수가 넘은 끝난 Task를 지운다.
    #[serde(default = "default_task_max_age_days")]
    pub task_max_age_days: u64,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    #[serde(default = "default_task_max_finished")]
    pub task_max_finished: usize,
}

impl Default for JanitorSection {
    fn default() -> Self {
        Self {
            tick_secs: default_janitor_tick_secs(),
            offline_grace_secs: default_offline_grace_secs(),
            task_max_age_days: default_task_max_age_days(),
            task_max_finished: default_task_max_finished(),
        }
    }
}

impl JanitorSection {
    pub fn janitor_config(&self) -> ClusterJanitorConfig {
        ClusterJanitorConfig {
            tick: Duration::from_secs(self.tick_secs),
            offline_grace: Duration::from_secs(self.offline_grace_secs),
            task_max_age: Duration::from_secs(self.task_max_age_days * 24 * 3600),
            task_max_finished: self.task_max_finished,
            // 고아 스냅샷·끝난 Task의 유예는 설정하지 않는다(기본 30초·10분).
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
/// `task_max_age_days`의 상한(100년)
const MAX_TASK_AGE_DAYS: u64 = 36500;
fn default_task_max_age_days() -> u64 {
    7
}
fn default_task_max_finished() -> usize {
    1000
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
        // 0이면 끝난 Task를 곧바로 지워 Task를 기다리는 쪽이 결과를 못 본다.
        if self.janitor.task_max_age_days == 0 || self.janitor.task_max_finished == 0 {
            anyhow::bail!(
                "janitor.task_max_age_days and janitor.task_max_finished must be at least 1"
            );
        }
        // 날수를 초로 바꿀 때 넘치지 않도록 100년까지만 받는다.
        if self.janitor.task_max_age_days > MAX_TASK_AGE_DAYS {
            anyhow::bail!("janitor.task_max_age_days must be at most {MAX_TASK_AGE_DAYS}");
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
        assert_eq!(janitor.task_max_age, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 1000);
        assert_eq!(janitor.finished_grace, Duration::from_secs(600));
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
    fn janitor_section_sets_task_retention_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 2\ntask_max_finished = 50",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.task_max_age, Duration::from_secs(2 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 50);
        let c = parse("name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 36500");
        assert!(c.validate().is_ok());
        for bad in [
            "task_max_age_days = 0",
            "task_max_finished = 0",
            "task_max_age_days = 36501",
        ] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
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

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 400개, 실패 0

Run: `cargo test -p opl-api --test janitor --test tasks`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): janitor가 끝난 Task를 기간·개수 기준으로 지우기"
```

---

### Task 2: opld: 웹 UI 파일의 ETag와 304

`web.rs`의 `file_response(path, file, req)`가 rust-embed의 `sha256_hash()`를 `hex`로 바꿔 강한 ETag `"<hex>"`를 모든 응답에 붙이고, `matches(req, etag)`(여러 `If-None-Match` 헤더와 쉼표 목록, 앞의 `W/`를 떼는 약한 비교, `*`)가 맞으면 본문 없는 304에 ETag·Cache-Control·보안 헤더를 붙인다. SPA 폴백은 `index.html`의 ETag다. `Cache-Control`은 그대로다. opld가 워크스페이스 의존성 `hex`를 쓴다. 테스트: ETag 값, assets와 다름, 폴백, 304와 헤더, 목록·`*`·`W/`, 다른 값이면 200.

**Files:**
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/src/web.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/Cargo.toml`에서

```toml
clap.workspace = true
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
clap.workspace = true
hex.workspace = true
openraft.workspace = true
```

`crates/opld/src/web.rs` 전체:

```rust
//! 웹 UI 정적 파일. `web-ui` 기능으로 빌드하면 `web/dist`(`pnpm -C web build`의 결과)를 바이너리에
//! 넣고, API와 같은 주소에서 준다. 쿠키로 인증한 요청은 Origin이 서버와 같아야 하므로 UI는 같은
//! 출처에서 나가야 한다.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use opl_api::error::ApiError;
use rust_embed::EmbeddedFile;
#[cfg(feature = "web-ui")]
use rust_embed::RustEmbed;

/// 웹 UI가 붙는 응답의 보안 헤더. 스크립트와 연결은 같은 출처로만, 다른 사이트의 frame 안에는 뜨지 않는다.
/// 일부 UI 컴포넌트가 style 속성을 쓰므로 인라인 스타일은 허용하고, noVNC 커서는 `data:` 이미지다.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; \
                   base-uri 'none'; form-action 'self'";

/// 파일 이름에 내용 해시가 들어간 Vite 빌드 결과
const ASSETS: &str = "assets/";

#[cfg(feature = "web-ui")]
#[derive(RustEmbed)]
#[folder = "$OPL_WEB_DIST"]
// 디버그 빌드(테스트, clippy)는 UI를 빌드하지 않아도 되고 안내 페이지를 준다. 릴리스 빌드에서
// 빠뜨리면 컴파일 오류다. 디버그 빌드는 파일을 실행 중에 디스크에서 읽으므로, 나중에 만든 dist도
// 보인다. 경로는 build.rs가 `..` 없이 넘긴다.
#[cfg_attr(debug_assertions, allow_missing = true)]
struct Dist;

/// 경로로 빌드 결과 파일을 찾는다.
type Files = fn(&str) -> Option<EmbeddedFile>;

/// `/api/` 밖의 모든 경로를 웹 UI로 보낸다. API 라우터와 합친다.
pub fn router() -> Router {
    #[cfg(feature = "web-ui")]
    let files: Files = Dist::get;
    #[cfg(not(feature = "web-ui"))]
    let files: Files = |_| None;
    serve(files)
}

fn serve(files: Files) -> Router {
    Router::new().fallback(move |method: Method, uri: Uri, req: HeaderMap| async move {
        if let Some(r) = reject(&method, &uri) {
            return r;
        }
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        let mut r = match files(path) {
            Some(file) => file_response(path, file, &req),
            // 없는 빌드 결과를 index.html로 주면 브라우저가 HTML을 스크립트로 읽는다.
            None if path.starts_with(ASSETS) => StatusCode::NOT_FOUND.into_response(),
            // 화면 주소(`/vms/web-01`)는 index.html이 받아 브라우저에서 고른다.
            None => match files("index.html") {
                Some(file) => file_response("index.html", file, &req),
                None => return not_built(),
            },
        };
        secure(&mut r);
        r
    })
}

/// 웹 UI 없이 빌드한 opld의 안내
fn not_built() -> Response {
    let page = "<!doctype html><meta charset=\"utf-8\"><title>opLite</title>\
                <p>This opld was built without the web UI. Build it with \
                <code>pnpm -C web build</code> and the <code>web-ui</code> feature. \
                The API is at <code>/api/v1</code>.</p>";
    let mut r = (StatusCode::NOT_FOUND, axum::response::Html(page)).into_response();
    secure(&mut r);
    r
}

/// API 경로는 웹 UI로 대체하지 않고 API 오류(JSON)로 답한다. 정적 파일은 읽기만 한다.
fn reject(method: &Method, uri: &Uri) -> Option<Response> {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return Some(ApiError::not_found(format!("no API at {path}")).into_response());
    }
    if method != Method::GET && method != Method::HEAD {
        return Some(
            (
                StatusCode::METHOD_NOT_ALLOWED,
                [(header::ALLOW, "GET, HEAD")],
            )
                .into_response(),
        );
    }
    None
}

/// 파일 응답. 내용 해시로 강한 ETag를 붙이고, `If-None-Match`가 맞으면 본문 없이 304로 답한다.
fn file_response(path: &str, file: EmbeddedFile, req: &HeaderMap) -> Response {
    let cache = if path.starts_with(ASSETS) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));
    let fresh = matches(req, &etag);
    let etag = HeaderValue::from_str(&etag).expect("16진수 ETag");
    if fresh {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        let h = r.headers_mut();
        h.insert(header::ETAG, etag);
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        return r;
    }
    let mime = match file.metadata.mimetype() {
        "text/html" => "text/html; charset=utf-8".to_string(),
        other => other.to_string(),
    };
    let mut r = Response::new(Body::from(file.data.into_owned()));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::ETAG, etag);
    r
}

/// `If-None-Match`의 쉼표 목록 가운데 하나가 ETag와 같거나 `*`인가. 약한 비교(RFC 9110)라
/// 앞의 `W/`는 떼고 비교한다.
fn matches(req: &HeaderMap, etag: &str) -> bool {
    req.get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|v| v == "*" || v.strip_prefix("W/").unwrap_or(v) == etag)
}

fn secure(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

#[cfg(test)]
mod tests {
    use rust_embed::RustEmbed;

    use super::*;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/web"]
    struct Fixture;

    async fn get(
        app: Router,
        method: &str,
        path: &str,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        get_if(app, method, path, None).await
    }

    /// `If-None-Match`를 붙여 요청한다.
    async fn get_if(
        app: Router,
        method: &str,
        path: &str,
        if_none_match: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut req =
            reqwest::Client::new().request(method.parse().unwrap(), format!("http://{addr}{path}"));
        if let Some(v) = if_none_match {
            req = req.header(header::IF_NONE_MATCH, v);
        }
        let resp = req.send().await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        (status, headers, resp.text().await.unwrap())
    }

    #[tokio::test]
    async fn serves_files_and_falls_back_to_index_for_screens() {
        let (status, h, body) = get(serve(Fixture::get), "GET", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        assert_eq!(h["content-type"], "text/html; charset=utf-8");
        assert_eq!(h["cache-control"], "no-cache");
        assert_eq!(h["content-security-policy"], CSP);
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");

        let (status, h, body) = get(serve(Fixture::get), "GET", "/assets/app-1a2b3c.js").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture script"), "{body}");
        assert_eq!(h["content-type"], "text/javascript");
        assert_eq!(h["cache-control"], "public, max-age=31536000, immutable");

        // 화면 주소는 index.html
        let (status, _, body) = get(serve(Fixture::get), "GET", "/vms/web-01").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        // 없는 빌드 결과는 index.html이 아니라 404
        let (status, _, _) = get(serve(Fixture::get), "GET", "/assets/gone-000.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn etag_revalidation_answers_not_modified() {
        // 모든 파일 응답에 내용 해시의 강한 ETag
        let (_, h, _) = get(serve(Fixture::get), "GET", "/").await;
        let etag = h["etag"].to_str().unwrap().to_string();
        let hash = hex::encode(Fixture::get("index.html").unwrap().metadata.sha256_hash());
        assert_eq!(etag, format!("\"{hash}\""));
        let (_, h, _) = get(serve(Fixture::get), "GET", "/assets/app-1a2b3c.js").await;
        assert_ne!(h["etag"], etag.as_str());
        // SPA 폴백은 index.html의 ETag
        let (_, h, _) = get(serve(Fixture::get), "GET", "/vms/web-01").await;
        assert_eq!(h["etag"], etag.as_str());

        // 같으면 본문 없이 304, 헤더는 그대로
        for path in ["/", "/vms/web-01"] {
            let (status, h, body) = get_if(serve(Fixture::get), "GET", path, Some(&etag)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{path}");
            assert!(body.is_empty(), "{body}");
            assert_eq!(h["etag"], etag.as_str());
            assert_eq!(h["cache-control"], "no-cache");
            assert_eq!(h["content-security-policy"], CSP);
            assert_eq!(h["x-content-type-options"], "nosniff");
            assert_eq!(h["referrer-policy"], "no-referrer");
        }
        // 쉼표 목록 가운데 하나, `*`, 약한 비교
        let list = format!("\"other\", {etag}");
        let weak = format!("W/{etag}");
        for v in [list.as_str(), "*", weak.as_str()] {
            let (status, _, _) = get_if(serve(Fixture::get), "GET", "/", Some(v)).await;
            assert_eq!(status, StatusCode::NOT_MODIFIED, "{v}");
        }
        // 다르면 200과 본문
        let (status, h, body) = get_if(serve(Fixture::get), "GET", "/", Some("\"other\"")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("fixture index"), "{body}");
        assert_eq!(h["etag"], etag.as_str());
    }

    #[tokio::test]
    async fn api_paths_and_writes_are_not_served_as_the_ui() {
        for (method, path) in [
            ("GET", "/api/v1/nope"),
            ("POST", "/api/v2/x"),
            ("GET", "/api"),
        ] {
            let (status, h, body) = get(serve(Fixture::get), method, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(h["content-type"], "application/json", "{method} {path}");
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["code"], "not_found");
        }
        let (status, h, _) = get(serve(Fixture::get), "POST", "/vms").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h["allow"], "GET, HEAD");
        let (status, h, _) = get(serve(Fixture::get), "HEAD", "/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(h["content-security-policy"], CSP);
    }

    #[tokio::test]
    async fn without_a_built_ui_explains_the_build() {
        let (status, h, body) = get(serve(|_| None), "GET", "/vms").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("without the web UI"), "{body}");
        assert_eq!(h["content-security-policy"], CSP);
        let (_, h, _) = get(serve(|_| None), "GET", "/api/v1/nope").await;
        assert_eq!(h["content-type"], "application/json");
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 401개, 실패 0

Run: `cargo test -p opld web::`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(opld): 웹 UI 파일에 ETag와 If-None-Match 304"
```

---

### Task 3: 웹: Task 색인과 행마다 훑지 않기

새 `api/taskIndex.tsx`의 `TaskIndexProvider`가 `useList("task")`를 한 번 읽어 `useMemo`로 대상 id → 끝나지 않은 Task 목록을 만들고, `useTaskIndex()`가 돌려준다(Provider 밖이면 빈 `Map`). `App.tsx`에서 `LiveProvider` 안쪽, 테스트의 `renderPage`에도 둔다(테스트 서버는 기본으로 `GET /tasks`에 빈 목록). `VmMenu`와 VM 상세 머리는 색인에서 자기 VM 것만 꺼내고, `isReverting(vmTasks)`는 대상 확인 없이 받은 목록만 본다. 작업·볼륨·풀·토큰 화면의 행마다 `find`는 화면당 한 번 만든 `Map`으로, VM 작업 탭의 거르기는 `useMemo`로 바꾼다. 테스트: 색인 단위.

**Files:**
- Create: `web/src/api/taskIndex.test.tsx`
- Create: `web/src/api/taskIndex.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/accounts/TokensPage.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/VolumesPage.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/test/render.tsx`
- Modify: `web/src/test/server.ts`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmMenu.tsx`
- Modify: `web/src/vms/actions.test.ts`
- Modify: `web/src/vms/actions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.tsx`에서

```tsx
import { createQueryClient } from "@/api/queryClient";
import { AuthProvider } from "@/auth/AuthProvider";
```

을 다음으로 바꾼다:

```tsx
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import { AuthProvider } from "@/auth/AuthProvider";
```

`web/src/App.tsx`에서

```tsx
            <LiveProvider>
              <TaskToasts />
              <AppLayout />
            </LiveProvider>
```

을 다음으로 바꾼다:

```tsx
            <LiveProvider>
              <TaskIndexProvider>
                <TaskToasts />
                <AppLayout />
              </TaskIndexProvider>
            </LiveProvider>
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
  const changed = () => queryClient.invalidateQueries({ queryKey: ["tokens"] });
  const owner = (token: Token) =>
    token.user === me.user
      ? me.name
      : (users.data?.find((u) => u.id === token.user)?.name ?? token.user.slice(-8));
```

을 다음으로 바꾼다:

```tsx
  const changed = () => queryClient.invalidateQueries({ queryKey: ["tokens"] });
  const userName = useMemo(() => new Map(users.data?.map((u) => [u.id, u.name])), [users.data]);
  const owner = (token: Token) =>
    token.user === me.user ? me.name : (userName.get(token.user) ?? token.user.slice(-8));
```

`web/src/api/taskIndex.test.tsx`를 만든다:

```tsx
import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";

import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider, useTaskIndex } from "@/api/taskIndex";
import type { Task } from "@/api/types";
import { task } from "@/test/fixtures";

function of(id: string, target: string, state: Task["status"]["state"]): Task {
  return { ...task(id, "vm-start", target), status: { state, error: null } };
}

it("indexes unfinished tasks by target", async () => {
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "task"], {
    revision: 1,
    items: [
      of("1", "web", "pending"),
      of("2", "web", "running"),
      of("3", "web", "succeeded"),
      of("4", "db", "failed"),
      of("5", "db", "running"),
    ],
  });
  const { result } = renderHook(() => useTaskIndex(), {
    wrapper: ({ children }) => (
      <QueryClientProvider client={queryClient}>
        <TaskIndexProvider>{children}</TaskIndexProvider>
      </QueryClientProvider>
    ),
  });
  await waitFor(() => expect(result.current.size).toBe(2));
  expect(result.current.get("web")?.map((t) => t.id)).toEqual(["1", "2"]);
  expect(result.current.get("db")?.map((t) => t.id)).toEqual(["5"]);
});
```

`web/src/api/taskIndex.tsx`를 만든다:

```tsx
import { createContext, useContext, useMemo } from "react";

import { useList } from "@/api/live";
import type { Task } from "@/api/types";

const TaskIndexContext = createContext<Map<string, Task[]>>(new Map());

/** 끝나지 않은 Task를 대상 id별로 모은 색인을 한 번 만든다. 행마다 Task 목록을 훑지 않게 한다. */
export function TaskIndexProvider({ children }: { children: React.ReactNode }) {
  const tasks = useList("task");
  const index = useMemo(() => {
    const byTarget = new Map<string, Task[]>();
    for (const task of tasks.data ?? []) {
      if (task.status.state === "succeeded" || task.status.state === "failed") {
        continue;
      }
      const list = byTarget.get(task.spec.target);
      if (list) {
        list.push(task);
      } else {
        byTarget.set(task.spec.target, [task]);
      }
    }
    return byTarget;
  }, [tasks.data]);
  return <TaskIndexContext value={index}>{children}</TaskIndexContext>;
}

/** 대상 id → 끝나지 않은 Task. Task 목록을 아직 모르면 비어 있다. */
export function useTaskIndex(): Map<string, Task[]> {
  return useContext(TaskIndexContext);
}
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
```

`web/src/storage/PoolsPage.tsx`에서

```tsx

  const nodeNames = (pool: StoragePool) => {
```

을 다음으로 바꾼다:

```tsx

  const nodeName = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n.name])), [nodes.data]);
  const nodeNames = (pool: StoragePool) => {
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
      ? t("pools.allNodes")
      : ids.map((id) => nodes.data?.find((n) => n.id === id)?.name ?? id.slice(-8)).join(", ");
  };
```

을 다음으로 바꾼다:

```tsx
      ? t("pools.allNodes")
      : ids.map((id) => nodeName.get(id) ?? id.slice(-8)).join(", ");
  };
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
import { Maximize2, Plus, Trash2, Upload } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { Maximize2, Plus, Trash2, Upload } from "lucide-react";
import { type RefObject, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
import type { components } from "@/api/schema";
import type { Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
```

을 다음으로 바꾼다:

```tsx
import type { components } from "@/api/schema";
import type { Vm, Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userOf = (volume: Volume) =>
    vms.data?.find((vm) => vm.spec.disks?.some((d) => d.volume === volume.name));
```

을 다음으로 바꾼다:

```tsx
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userByVolume = useMemo(() => {
    const users = new Map<string, Vm>();
    for (const vm of vms.data ?? []) {
      for (const disk of vm.spec.disks ?? []) {
        // 목록에서 먼저 나온 VM을 보인다.
        if (!users.has(disk.volume)) {
          users.set(disk.volume, vm);
        }
      }
    }
    return users;
  }, [vms.data]);
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
          {(volumes.data ?? []).map((volume) => {
            const vm = userOf(volume);
            const ready = volume.status.phase === "ready";
            const node = nodes.data?.find((n) => n.id === volume.status.node);
            return (
```

을 다음으로 바꾼다:

```tsx
          {(volumes.data ?? []).map((volume) => {
            const vm = userByVolume.get(volume.name);
            const ready = volume.status.phase === "ready";
            const node = volume.status.node ? nodeById.get(volume.status.node) : undefined;
            return (
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
  const volumes = useList("volume");
  return (task: Task) => {
    const vm = vms.data?.find((v) => v.id === task.spec.target);
    if (vm) {
      return { name: vm.name, href: `/vms/${encodeURIComponent(vm.name)}` };
    }
    const volume = volumes.data?.find((v) => v.id === task.spec.target);
    // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
    return { name: volume?.name ?? task.spec.target.slice(-8), href: undefined };
  };
}
```

을 다음으로 바꾼다:

```tsx
  const volumes = useList("volume");
  return useMemo(() => {
    const vmById = new Map(vms.data?.map((v) => [v.id, v]));
    const volumeById = new Map(volumes.data?.map((v) => [v.id, v]));
    return (task: Task) => {
      const vm = vmById.get(task.spec.target);
      if (vm) {
        return { name: vm.name, href: `/vms/${encodeURIComponent(vm.name)}` };
      }
      const volume = volumeById.get(task.spec.target);
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: volume?.name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data]);
}
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
  const nodes = useList("node");
  return (
```

을 다음으로 바꾼다:

```tsx
  const nodes = useList("node");
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  return (
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
          const { name, href } = target(task);
          const node = nodes.data?.find((n) => n.id === task.spec.node);
          const time = uuidTime(task.id);
```

을 다음으로 바꾼다:

```tsx
          const { name, href } = target(task);
          const node = nodeById.get(task.spec.node);
          const time = uuidTime(task.id);
```

`web/src/test/render.tsx`에서

```tsx
import { createQueryClient } from "@/api/queryClient";
import type { Principal, Resources, WatchKind } from "@/api/types";
```

을 다음으로 바꾼다:

```tsx
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import type { Principal, Resources, WatchKind } from "@/api/types";
```

`web/src/test/render.tsx`에서

```tsx
    [
      { element: <RequireAuth />, children: [{ path, element }] },
      { path: "*", element: <p>moved</p> },
```

을 다음으로 바꾼다:

```tsx
    [
      {
        element: <RequireAuth />,
        // 앱처럼 화면들 바깥에 Task 색인을 둔다.
        children: [{ path, element: <TaskIndexProvider>{element}</TaskIndexProvider> }],
      },
      { path: "*", element: <p>moved</p> },
```

`web/src/test/server.ts`에서

```ts
import { setupServer } from "msw/node";

/** 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. */
export const server = setupServer();
```

을 다음으로 바꾼다:

```ts
import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";
```

`web/src/test/server.ts`에서

```ts
}
```

을 다음으로 바꾼다:

```ts
}

/**
 * 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. 다만 Task 목록은
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 */
export const server = setupServer(
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
);
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { Suspense, useId, useState } from "react";
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { Suspense, useId, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```tsx
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const nodes = useList("node");
  const tasks = useList("task");
  const hint = useId();
```

을 다음으로 바꾼다:

```tsx
  const nodes = useList("node");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const hint = useId();
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = operate && canRun(vm, "force-stop") && isReverting(vm, tasks.data ?? []);
  const button = (action: VmAction) => (
```

을 다음으로 바꾼다:

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = operate && canRun(vm, "force-stop") && isReverting(vmTasks);
  const button = (action: VmAction) => (
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const tasks = useList("task");
  const mine = newestFirst((tasks.data ?? []).filter((task) => task.spec.target === vm.id));
  return (
```

을 다음으로 바꾼다:

```tsx
  const tasks = useList("task");
  const mine = useMemo(
    () => newestFirst((tasks.data ?? []).filter((task) => task.spec.target === vm.id)),
    [tasks.data, vm.id],
  );
  return (
```

`web/src/vms/VmMenu.tsx`에서

```tsx

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```tsx

import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  const trigger = useRef<HTMLButtonElement>(null);
  const tasks = useList("task");
  const hint = useId();
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = canRun(vm, "force-stop") && isReverting(vm, tasks.data ?? []);
  const operate = can(me.role, "operate", "vm");
```

을 다음으로 바꾼다:

```tsx
  const trigger = useRef<HTMLButtonElement>(null);
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const hint = useId();
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = canRun(vm, "force-stop") && isReverting(vmTasks);
  const operate = can(me.role, "operate", "vm");
```

`web/src/vms/actions.test.ts`에서

```ts
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
```

을 다음으로 바꾼다:

```ts
  const web = vm("web", "running");
  const of = (kind: Task["spec"]["op"]["kind"], state: Task["status"]["state"]) => ({
    ...task(`t-${kind}-${state}`, kind, web.id),
    status: { state, error: null },
  });
  expect(isReverting([])).toBe(false);
  expect(isReverting([of("snapshot-revert", "pending")])).toBe(true);
  expect(isReverting([of("snapshot-revert", "running")])).toBe(true);
  // 끝난 되돌리기
  expect(isReverting([of("snapshot-revert", "succeeded")])).toBe(false);
  expect(isReverting([of("snapshot-revert", "failed")])).toBe(false);
  // 만들기·삭제·복제 중에는 강제 정지를 받는다.
  for (const kind of ["snapshot-create", "snapshot-delete", "vm-clone"] as const) {
    expect(isReverting([of(kind, "running")])).toBe(false);
  }
```

`web/src/vms/actions.ts`에서

```ts
 * 서버의 `check_no_revert_task`: 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았다. 강제 정지는 이때만
 * 막힌다(만들기·삭제·복제 중에는 받는다). `tasks`는 watch로 들어온 Task 목록이다.
 */
export function isReverting(vm: Vm, tasks: readonly Task[]): boolean {
  return tasks.some(
    (task) =>
      task.spec.target === vm.id &&
      task.spec.op.kind === "snapshot-revert" &&
```

을 다음으로 바꾼다:

```ts
 * 서버의 `check_no_revert_task`: 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았다. 강제 정지는 이때만
 * 막힌다(만들기·삭제·복제 중에는 받는다). `vmTasks`는 이 VM을 대상으로 한 Task다(`useTaskIndex`).
 */
export function isReverting(vmTasks: readonly Task[]): boolean {
  return vmTasks.some(
    (task) =>
      task.spec.op.kind === "snapshot-revert" &&
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 237개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "refactor(web): Task 색인을 한 번 만들고 행마다 목록을 훑지 않기"
```

---

### Task 4: 웹: 처리 중인 전원 Task로 버튼 막기

`vms/actions.ts`의 `canRun`·`canDelete`를 `availability(vm, action | "console" | "delete", vmTasks): Availability`(`{ ok: true }` 또는 `{ ok: false, reason }`, reason은 번역 키)로 바꾼다. 시작·정지·재시작은 끝나지 않은 `vm-start`·`vm-stop`·`vm-force-stop`·`vm-reboot` Task가 있으면 `vms.reason.powerPending`(상태보다 먼저), 그다음 상태 규칙(`failed` → `vms.reason.failed`, 꺼짐 → `stopped`, 바뀌는 중 → `busy`, 그 밖 → `running`), 강제 정지는 상태가 허락할 때 되돌리는 중이면 `reverting`. `runMany(vms, action, taskIndex)`도 쓰고, VM 목록이 색인을 넘긴다. 이 Task에서는 이유를 아직 보이지 않고 막기만 한다. 테스트: `availability` 단위, 처리 중인 시작 Task가 있는 VM의 메뉴 시작이 막히고 일괄 시작이 건너뜀.

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`
- Modify: `web/src/vms/VmListPage.tsx`
- Modify: `web/src/vms/VmMenu.tsx`
- Modify: `web/src/vms/actions.test.ts`
- Modify: `web/src/vms/actions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "forceStopReverting": "Force stop is not available while a snapshot is being reverted.",
```

을 다음으로 바꾼다:

```json
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "reason": {
      "powerPending": "A power action is still in progress.",
      "reverting": "Not available while a snapshot is being reverted.",
      "running": "The VM is running.",
      "stopped": "The VM is stopped.",
      "failed": "The VM has failed.",
      "busy": "The VM is starting or stopping."
    },
    "forceStopReverting": "Force stop is not available while a snapshot is being reverted.",
```

`web/src/i18n/ko.json`에서

```json
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "forceStopReverting": "스냅샷을 되돌리는 중에는 강제 정지할 수 없습니다.",
```

을 다음으로 바꾼다:

```json
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "reason": {
      "powerPending": "전원 작업이 아직 처리 중입니다.",
      "reverting": "스냅샷을 되돌리는 중에는 할 수 없습니다.",
      "running": "VM이 실행 중입니다.",
      "stopped": "VM이 정지되어 있습니다.",
      "failed": "VM이 실패 상태입니다.",
      "busy": "VM이 시작하거나 정지하는 중입니다."
    },
    "forceStopReverting": "스냅샷을 되돌리는 중에는 강제 정지할 수 없습니다.",
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
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
```

을 다음으로 바꾼다:

```tsx
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
import { VmConsole } from "@/vms/lazyConsole";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = operate && canRun(vm, "force-stop") && isReverting(vmTasks);
  const button = (action: VmAction) => (
```

을 다음으로 바꾼다:

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const forceStop = availability(vm, "force-stop", vmTasks);
  const reverting = operate && !forceStop.ok && forceStop.reason === "vms.reason.reverting";
  const button = (action: VmAction) => (
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
      size="sm"
      disabled={!canRun(vm, action) || (action === "force-stop" && reverting)}
      aria-describedby={action === "force-stop" && reverting ? hint : undefined}
```

을 다음으로 바꾼다:

```tsx
      size="sm"
      disabled={!availability(vm, action, vmTasks).ok}
      aria-describedby={action === "force-stop" && reverting ? hint : undefined}
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
            size="sm"
            disabled={!canDelete(vm)}
            onClick={() => setConfirm("delete")}
```

을 다음으로 바꾼다:

```tsx
            size="sm"
            disabled={!availability(vm, "delete", vmTasks).ok}
            onClick={() => setConfirm("delete")}
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
});

it("blocks starting a VM whose start is still pending, alone or with others", async () => {
  const app = vm("app", "stopped");
  const starting = task("t-start", "vm-start", db.id);
  mockLists({ vm: [web, db, app], node: [NODE], task: [starting] });
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      started.push(String(params.key));
      return HttpResponse.json(task(`t-p${started.length}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  // Task 목록을 읽은 뒤에 메뉴를 연다.
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  await userEvent.click(screen.getByRole("button", { name: "Actions for db" }));
  expect(await screen.findByRole("menuitem", { name: "Start" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await userEvent.keyboard("{Escape}");

  // 여럿을 시작할 때도 db는 건너뛴다.
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  await userEvent.click(within(screen.getByText("3 selected").parentElement!).getByText("Start"));
  expect(await screen.findByText("Start: 1 requested, 0 failed, 2 skipped.")).toBeInTheDocument();
  expect(started).toEqual([app.id]);
});
```

`web/src/vms/VmListPage.tsx`에서

```tsx
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```tsx
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Vm } from "@/api/types";
```

`web/src/vms/VmListPage.tsx`에서

```tsx
  const { runMany } = useVmActions();
  const [filter, setFilter] = useState("");
```

을 다음으로 바꾼다:

```tsx
  const { runMany } = useVmActions();
  const taskIndex = useTaskIndex();
  const [filter, setFilter] = useState("");
```

`web/src/vms/VmListPage.tsx`에서

```tsx
  const bulk = (action: "start" | "stop") => {
    void runMany(selected, action);
    setSelection({});
```

을 다음으로 바꾼다:

```tsx
  const bulk = (action: "start" | "stop") => {
    void runMany(selected, action, taskIndex);
    setSelection({});
```

`web/src/vms/VmMenu.tsx`에서

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

을 다음으로 바꾼다:

```tsx
} from "@/components/ui/dropdown-menu";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const reverting = canRun(vm, "force-stop") && isReverting(vmTasks);
  const operate = can(me.role, "operate", "vm");
```

을 다음으로 바꾼다:

```tsx
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const forceStop = availability(vm, "force-stop", vmTasks);
  const reverting = !forceStop.ok && forceStop.reason === "vms.reason.reverting";
  const operate = can(me.role, "operate", "vm");
```

`web/src/vms/VmMenu.tsx`에서

```tsx
                key={action}
                disabled={!canRun(vm, action) || (action === "force-stop" && reverting)}
                aria-describedby={action === "force-stop" && reverting ? hint : undefined}
```

을 다음으로 바꾼다:

```tsx
                key={action}
                disabled={!availability(vm, action, vmTasks).ok}
                aria-describedby={action === "force-stop" && reverting ? hint : undefined}
```

`web/src/vms/VmMenu.tsx`에서

```tsx
            <DropdownMenuItem
              disabled={vm.status.phase !== "running"}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
```

을 다음으로 바꾼다:

```tsx
            <DropdownMenuItem
              disabled={!availability(vm, "console", vmTasks).ok}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
```

`web/src/vms/VmMenu.tsx`에서

```tsx
                variant="destructive"
                disabled={!canDelete(vm)}
                onSelect={() => setConfirm("delete")}
```

을 다음으로 바꾼다:

```tsx
                variant="destructive"
                disabled={!availability(vm, "delete", vmTasks).ok}
                onSelect={() => setConfirm("delete")}
```

`web/src/vms/actions.test.ts`에서

```ts
import { task, vm } from "@/test/fixtures";
import { ACTIONS, canDelete, canRun, isBusy, isReverting, isStopped } from "@/vms/actions";

function allowed(v: ReturnType<typeof vm>) {
  return ACTIONS.filter((action) => canRun(v, action));
}
```

을 다음으로 바꾼다:

```ts
import { task, vm } from "@/test/fixtures";
import { ACTIONS, availability, isBusy, isReverting, isStopped } from "@/vms/actions";

function allowed(v: ReturnType<typeof vm>, vmTasks: Task[] = []) {
  return ACTIONS.filter((action) => availability(v, action, vmTasks).ok);
}

function canDelete(v: ReturnType<typeof vm>) {
  return availability(v, "delete", []).ok;
}
```

`web/src/vms/actions.test.ts`에서

```ts
    expect(isReverting([of(kind, "running")])).toBe(false);
  }
});
```

을 다음으로 바꾼다:

```ts
    expect(isReverting([of(kind, "running")])).toBe(false);
  }
});

/** 이 VM을 대상으로 한, 아직 끝나지 않은 Task */
function pendingOf(v: ReturnType<typeof vm>, kind: Task["spec"]["op"]["kind"]): Task {
  return { ...task(`t-${kind}`, kind, v.id), status: { state: "running", error: null } };
}

it("says why a power action is not available in the VM's state", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const stopping = vm("c", "stopping");
  const starting = vm("d", "starting");
  expect(availability(stopped, "start", [])).toEqual({ ok: true });
  expect(availability(stopped, "stop", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(stopped, "force-stop", [])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
  expect(availability(stopped, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(running, "start", [])).toEqual({ ok: false, reason: "vms.reason.running" });
  expect(availability(stopping, "start", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopping, "stop", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopping, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(starting, "start", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(starting, "reboot", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});

it("blames a failed VM's state as failed", () => {
  const failed = vm("e", "failed");
  const reason = { ok: false, reason: "vms.reason.failed" };
  expect(availability(failed, "start", [])).toEqual({ ok: true });
  expect(availability(failed, "delete", [])).toEqual({ ok: true });
  for (const action of ["stop", "force-stop", "reboot", "console"] as const) {
    expect(availability(failed, action, [])).toEqual(reason);
  }
  // 켜려는 채 실패했으면 시작도 실패 때문에 막힌다.
  const wantsRunning = vm("f", "failed", {
    spec: { ...failed.spec, power: "running" },
  });
  expect(availability(wantsRunning, "start", [])).toEqual(reason);
});

it("blocks start, stop and reboot while a power task of the VM is unfinished", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const pending = { ok: false, reason: "vms.reason.powerPending" };
  for (const kind of ["vm-start", "vm-stop", "vm-force-stop", "vm-reboot"] as const) {
    expect(availability(stopped, "start", [pendingOf(stopped, kind)])).toEqual(pending);
    expect(availability(running, "stop", [pendingOf(running, kind)])).toEqual(pending);
    expect(availability(running, "reboot", [pendingOf(running, kind)])).toEqual(pending);
    // 강제 정지는 ACPI 정지가 멈췄을 때 빠져나갈 길이다.
    expect(availability(running, "force-stop", [pendingOf(running, kind)])).toEqual({ ok: true });
  }
  // 끝난 전원 Task나 다른 Task는 막지 않는다.
  const done = { ...pendingOf(stopped, "vm-start"), status: { state: "succeeded", error: null } };
  expect(availability(stopped, "start", [done as Task])).toEqual({ ok: true });
  expect(availability(stopped, "start", [pendingOf(stopped, "snapshot-create")])).toEqual({
    ok: true,
  });
});

it("blocks force stop while reverting, but blames the state first", () => {
  const running = vm("b", "running");
  const stopped = vm("a", "stopped");
  expect(availability(running, "force-stop", [pendingOf(running, "snapshot-revert")])).toEqual({
    ok: false,
    reason: "vms.reason.reverting",
  });
  expect(availability(stopped, "force-stop", [pendingOf(stopped, "snapshot-revert")])).toEqual({
    ok: false,
    reason: "vms.reason.stopped",
  });
});

it("says why the console or delete is not available", () => {
  const stopped = vm("a", "stopped");
  const running = vm("b", "running");
  const starting = vm("d", "starting");
  expect(availability(running, "console", [])).toEqual({ ok: true });
  expect(availability(stopped, "console", [])).toEqual({ ok: false, reason: "vms.reason.stopped" });
  expect(availability(starting, "console", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
  expect(availability(stopped, "delete", [])).toEqual({ ok: true });
  expect(availability(running, "delete", [])).toEqual({ ok: false, reason: "vms.reason.running" });
  expect(availability(starting, "delete", [])).toEqual({ ok: false, reason: "vms.reason.busy" });
});
```

`web/src/vms/actions.ts`에서

```ts

/** 지금 상태에서 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
export function canRun(vm: Vm, action: VmAction): boolean {
  const phase = vm.status.phase;
```

을 다음으로 바꾼다:

```ts

/** 할 수 있는지, 없으면 그 이유(번역 키) */
export type Availability = { ok: true } | { ok: false; reason: string };

/** 끝나지 않으면 다른 시작·정지·재시작을 막는 전원 Task */
const POWER_KINDS: readonly string[] = ["vm-start", "vm-stop", "vm-force-stop", "vm-reboot"];

/** 상태만 보고 할 수 있는 동작인지. 서버도 확인하지만 할 수 없는 버튼은 막아 둔다. */
function allowedByState(vm: Vm, action: VmAction | "console" | "delete"): boolean {
  const phase = vm.status.phase;
```

`web/src/vms/actions.ts`에서

```ts
      return powerOn && phase === "running";
  }
}

/** 지울 수 있는 상태인지(서버는 멈추지 않은 VM을 지우지 않는다) */
export function canDelete(vm: Vm): boolean {
  return isStopped(vm);
}
```

을 다음으로 바꾼다:

```ts
      return powerOn && phase === "running";
    case "console":
      return phase === "running";
    case "delete":
      // 서버는 멈추지 않은 VM을 지우지 않는다.
      return isStopped(vm);
  }
}

/** 상태 때문에 막혔을 때의 이유: 실패, 멈춤, 켜져 실행 중, 그 사이(바뀌는 중) */
function stateReason(vm: Vm): string {
  if (vm.status.phase === "failed") {
    return "vms.reason.failed";
  }
  if (isStopped(vm)) {
    return "vms.reason.stopped";
  }
  return isBusy(vm) ? "vms.reason.busy" : "vms.reason.running";
}

/**
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
    power &&
    vmTasks.some(
      (task) =>
        POWER_KINDS.includes(task.spec.op.kind) &&
        task.status.state !== "succeeded" &&
        task.status.state !== "failed",
    )
  ) {
    return { ok: false, reason: "vms.reason.powerPending" };
  }
  if (!allowedByState(vm, action)) {
    return { ok: false, reason: stateReason(vm) };
  }
  if (action === "force-stop" && isReverting(vmTasks)) {
    return { ok: false, reason: "vms.reason.reverting" };
  }
  return { ok: true };
}
```

`web/src/vms/actions.ts`에서

```ts

  /** 여러 VM에 같은 동작을 한다. 건너뛰거나 실패한 것이 있으면 요약 알림을 하나 더 띄운다. */
  async function runMany(vms: Vm[], action: VmAction): Promise<BulkResult> {
    const able = vms.filter((vm) => canRun(vm, action));
    const results = await Promise.allSettled(
```

을 다음으로 바꾼다:

```ts

  /**
   * 여러 VM에 같은 동작을 한다. 할 수 없는 VM은 건너뛴다(`taskIndex`는 `useTaskIndex`). 건너뛰거나 실패한
   * 것이 있으면 요약 알림을 하나 더 띄운다.
   */
  async function runMany(
    vms: Vm[],
    action: VmAction,
    taskIndex: Map<string, Task[]>,
  ): Promise<BulkResult> {
    const able = vms.filter((vm) => availability(vm, action, taskIndex.get(vm.id) ?? []).ok);
    const results = await Promise.allSettled(
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 243개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 처리 중인 전원 Task가 있으면 시작·정지·재시작을 막기"
```

---

### Task 5: 웹: 막힌 동작의 이유를 보이고 읽게 하기

새 `components/Reason.tsx`: `ReasonMenuItem`(이유가 있으면 `aria-disabled`, `onSelect`는 `preventDefault`만, 이름 옆 이유는 `aria-hidden`이고 `aria-describedby`로 잇는다, 문자열 이름이면 `textValue`), `ReasonButton`(`aria-disabled`, `onClick`은 `preventDefault`만, `reasons` id 바탕과 이유로 `aria-describedby`, `asChild` 가능), `ReasonList`(같은 영역의 버튼 이유를 한 번씩). VM 메뉴·VM 상세(시작·정지·재시작·강제 정지·콘솔·삭제, 콘솔은 운영자에게 항상 같은 링크), 노드 제거, 자기 계정의 역할·비밀번호·삭제, 볼륨 크기 늘리기·삭제에 적용하고, 강제 정지 힌트와 볼륨 "준비 안 됨"·"사용 중" 글씨를 이유로 바꾼다(`vms.forceStopReverting`을 지운다). 이유 키 `nodes.reason.*`, `users.reason.*`, `volumes.reason.*`. 테스트: 컴포넌트 단위(키보드, 접근성 설명, 동작 없음, `textValue`), 화면 테스트의 `toBeDisabled()`를 `aria-disabled`·설명 확인으로.

**Files:**
- Create: `web/src/components/Reason.test.tsx`
- Create: `web/src/components/Reason.tsx`
- Modify: `web/src/accounts/UsersPage.test.tsx`
- Modify: `web/src/accounts/UsersPage.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/storage/VolumesPage.test.tsx`
- Modify: `web/src/storage/VolumesPage.tsx`
- Modify: `web/src/vms/VmDetailPage.test.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`
- Modify: `web/src/vms/VmMenu.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/accounts/UsersPage.test.tsx`에서

```tsx
  await userEvent.click(within(me).getByRole("button", { name: "Actions for admin" }));
  expect(await screen.findByRole("menuitem", { name: "Delete" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByRole("menuitem", { name: "Reset password" })).toHaveAttribute(
```

을 다음으로 바꾼다:

```tsx
  await userEvent.click(within(me).getByRole("button", { name: "Actions for admin" }));
  const remove = await screen.findByRole("menuitem", { name: "Delete" });
  expect(remove).toHaveAttribute("aria-disabled", "true");
  expect(remove).toHaveAccessibleDescription("Only another administrator can delete your account.");
  expect(screen.getByRole("menuitem", { name: "Reset password" })).toHaveAttribute(
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
```

을 다음으로 바꾼다:

```tsx
import { FormDialog } from "@/components/FormDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
```

을 다음으로 바꾼다:

```tsx
  DropdownMenuContent,
  DropdownMenuSeparator,
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        disabled={self}
                        onSelect={() => setDialog({ kind: "role", user })}
                      >
                        {t("users.changeRole")}
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        disabled={self}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </DropdownMenuItem>
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        variant="destructive"
                        disabled={self}
                        onSelect={() => setDialog({ kind: "delete", user })}
                      >
                        {t("users.delete")}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
```

을 다음으로 바꾼다:

```tsx
                    <DropdownMenuContent align="end">
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfRole" : null}
                        onSelect={() => setDialog({ kind: "role", user })}
                      >
                        {t("users.changeRole")}
                      </ReasonMenuItem>
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfPassword" : null}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </ReasonMenuItem>
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        reason={self ? "users.reason.selfDelete" : null}
                        onSelect={() => setDialog({ kind: "delete", user })}
                      >
                        {t("users.delete")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
```

`web/src/components/Reason.test.tsx`를 만든다:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useId } from "react";

import { ReasonButton, ReasonList, ReasonMenuItem } from "@/components/Reason";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// 메뉴 항목이 받은 typeahead 글자(textValue)를 적어 둔다. 항목은 그대로 그린다.
const textValues = vi.hoisted(() => [] as (string | undefined)[]);
vi.mock("@/components/ui/dropdown-menu", async (importOriginal) => {
  const ui = await importOriginal<typeof import("@/components/ui/dropdown-menu")>();
  return {
    ...ui,
    DropdownMenuItem: (props: React.ComponentProps<typeof ui.DropdownMenuItem>) => {
      textValues.push(props.textValue);
      return <ui.DropdownMenuItem {...props} />;
    },
  };
});

function Menu({ onStart, onStop }: { onStart(): void; onStop(): void }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button>Actions</Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <ReasonMenuItem reason="vms.reason.powerPending" onSelect={onStart}>
          Start
        </ReasonMenuItem>
        <ReasonMenuItem onSelect={onStop}>Stop</ReasonMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

it("keeps a blocked menu item reachable and says why without running it", async () => {
  const onStart = vi.fn();
  const onStop = vi.fn();
  render(<Menu onStart={onStart} onStop={onStop} />);
  await userEvent.click(screen.getByRole("button", { name: "Actions" }));
  // 비활성 항목도 키보드로 닿는다.
  await userEvent.keyboard("{ArrowDown}");
  const start = screen.getByRole("menuitem", { name: "Start" });
  expect(start).toHaveFocus();
  expect(start).toHaveAttribute("aria-disabled", "true");
  expect(start).toHaveAccessibleDescription("A power action is still in progress.");
  // 골라도 동작하지 않고 메뉴도 닫히지 않는다.
  await userEvent.keyboard("{Enter}");
  await userEvent.click(start);
  expect(onStart).not.toHaveBeenCalled();
  expect(screen.getByRole("menuitem", { name: "Start" })).toBeInTheDocument();

  const stop = screen.getByRole("menuitem", { name: "Stop" });
  expect(stop).not.toHaveAttribute("aria-disabled");
  expect(stop).not.toHaveAccessibleDescription();
  await userEvent.click(stop);
  expect(onStop).toHaveBeenCalledOnce();
});

it("lets typeahead see only the name of a blocked menu item", async () => {
  render(<Menu onStart={() => {}} onStop={() => {}} />);
  textValues.length = 0;
  await userEvent.click(screen.getByRole("button", { name: "Actions" }));
  // 이유 글씨("A power action ...")가 붙은 항목도 이름만 쓴다.
  expect(new Set(textValues)).toEqual(new Set(["Start", "Stop"]));
});

function Buttons({ onStart, onStop }: { onStart(): void; onStop(): void }) {
  const base = useId();
  const reasons = ["vms.reason.stopped", "vms.reason.stopped", null];
  return (
    <form onSubmit={(e) => e.preventDefault()}>
      <ReasonButton reason={reasons[0]} reasons={base} onClick={onStop}>
        Stop
      </ReasonButton>
      <ReasonButton reason={reasons[1]} reasons={base} type="submit">
        Reboot
      </ReasonButton>
      <ReasonButton reason={reasons[2]} reasons={base} onClick={onStart}>
        Start
      </ReasonButton>
      <ReasonList base={base} reasons={reasons} />
    </form>
  );
}

it("keeps a blocked button in the tab order and says why without running it", async () => {
  const onStart = vi.fn();
  const onStop = vi.fn();
  render(<Buttons onStart={onStart} onStop={onStop} />);
  await userEvent.tab();
  const stop = screen.getByRole("button", { name: "Stop" });
  expect(stop).toHaveFocus();
  expect(stop).toHaveAttribute("aria-disabled", "true");
  expect(stop).toHaveAccessibleDescription("The VM is stopped.");
  await userEvent.keyboard("{Enter}");
  await userEvent.click(stop);
  expect(onStop).not.toHaveBeenCalled();

  // 폼 안의 막힌 제출 버튼은 폼을 보내지 않는다.
  const submitted = vi.fn((e: SubmitEvent) => e.preventDefault());
  document.querySelector("form")!.addEventListener("submit", submitted);
  await userEvent.click(screen.getByRole("button", { name: "Reboot" }));
  expect(submitted).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Reboot" })).toHaveAccessibleDescription(
    "The VM is stopped.",
  );
  // 같은 이유는 한 번만 보인다.
  expect(screen.getAllByText("The VM is stopped.")).toHaveLength(1);

  const start = screen.getByRole("button", { name: "Start" });
  expect(start).not.toHaveAttribute("aria-disabled");
  expect(start).not.toHaveAccessibleDescription();
  await userEvent.click(start);
  expect(onStart).toHaveBeenCalledOnce();
});
```

`web/src/components/Reason.tsx`를 만든다:

```tsx
import { cn } from "cn";
import { useId, type ComponentProps } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { DropdownMenuItem } from "@/components/ui/dropdown-menu";

/**
 * 막힐 수 있는 메뉴 항목. `reason`(번역 키)이 있으면 `disabled` 대신 `aria-disabled`로 막아, 키보드로
 * 닿고 화면 읽기 프로그램이 이름·비활성·이유를 읽게 한다. 골라도 동작하지 않고 메뉴도 닫히지 않는다.
 * 이름이 문자열이면 글자 입력으로 항목을 찾을 때(typeahead) 이유 글씨는 보지 않게 이름만 쓴다.
 */
export function ReasonMenuItem({
  reason,
  onSelect,
  className,
  children,
  ...props
}: Omit<ComponentProps<typeof DropdownMenuItem>, "disabled"> & { reason?: string | null }) {
  const { t } = useTranslation();
  const id = useId();
  return (
    <DropdownMenuItem
      textValue={typeof children === "string" ? children : undefined}
      {...props}
      aria-disabled={reason ? true : undefined}
      aria-describedby={reason ? id : undefined}
      className={cn(reason && "opacity-50", className)}
      onSelect={(e) => {
        if (reason) {
          e.preventDefault();
          return;
        }
        onSelect?.(e);
      }}
    >
      {children}
      {/* 이름에 섞이지 않게 숨기고 aria-describedby로만 읽힌다. */}
      {reason && (
        <span id={id} aria-hidden className="ml-auto pl-2 text-xs text-muted-foreground">
          {t(reason)}
        </span>
      )}
    </DropdownMenuItem>
  );
}

/** 이유 글씨의 id. 같은 이유로 막힌 버튼들은 같은 글씨를 가리킨다. */
function reasonId(base: string, reason: string) {
  return `${base}-${reason}`;
}

/**
 * 막힐 수 있는 버튼. `reason`(번역 키)이 있으면 `disabled` 대신 `aria-disabled`로 막아 탭 순서에 남기고,
 * 눌러도 동작하지 않는다(폼 안에서도 보내지 않는다). 이유 글씨는 같은 `reasons`(`useId()`)를 받은
 * `ReasonList`가 보이고 `aria-describedby`로 잇는다.
 */
export function ReasonButton({
  reason,
  reasons,
  onClick,
  className,
  ...props
}: Omit<ComponentProps<typeof Button>, "disabled"> & { reason?: string | null; reasons: string }) {
  return (
    <Button
      {...props}
      aria-disabled={reason ? true : undefined}
      aria-describedby={reason ? reasonId(reasons, reason) : undefined}
      className={cn(reason && "opacity-50", className)}
      onClick={(e) => {
        if (reason) {
          e.preventDefault();
          return;
        }
        onClick?.(e);
      }}
    />
  );
}

/** 막힌 `ReasonButton`들의 이유. 같은 이유는 한 번만 보이고, 막힌 것이 없으면 그리지 않는다. */
export function ReasonList({
  base,
  reasons,
  className,
}: {
  base: string;
  reasons: readonly (string | null | undefined)[];
  className?: string;
}) {
  const { t } = useTranslation();
  const shown = [...new Set(reasons.filter((r): r is string => !!r))];
  if (shown.length === 0) {
    return null;
  }
  return (
    <div className={cn("text-xs text-muted-foreground", className)}>
      {shown.map((reason) => (
        <p key={reason} id={reasonId(base, reason)}>
          {t(reason)}
        </p>
      ))}
    </div>
  );
}
```

`web/src/i18n/en.json`에서

```json
    },
    "forceStopReverting": "Force stop is not available while a snapshot is being reverted.",
    "deleteTitle": "Delete {{name}}?",
```

을 다음으로 바꾼다:

```json
    },
    "deleteTitle": "Delete {{name}}?",
```

`web/src/i18n/en.json`에서

```json
    "deleteDescription": "The disk image and all data on it are deleted.",
    "inUse": "Remove it from the VM first.",
    "resize": "Resize",
```

을 다음으로 바꾼다:

```json
    "deleteDescription": "The disk image and all data on it are deleted.",
    "resize": "Resize",
```

`web/src/i18n/en.json`에서

```json
    "newSize": "New size (GiB)",
    "notReady": "Available once ready.",
    "errors": {
```

을 다음으로 바꾼다:

```json
    "newSize": "New size (GiB)",
    "errors": {
```

`web/src/i18n/en.json`에서

```json
      "grow": "Enter a size larger than the current size."
    }
```

을 다음으로 바꾼다:

```json
      "grow": "Enter a size larger than the current size."
    },
    "reason": {
      "inUse": "Remove it from the VM first.",
      "notReady": "Available once ready.",
      "vmsUnknown": "Cannot tell which VMs use it."
    }
```

`web/src/i18n/en.json`에서

```json
    "removeNote": "Only offline nodes can be removed here. To remove a live node, run oplctl cluster leave on it.",
    "removed": "Removed {{name}} from the cluster."
  },
```

을 다음으로 바꾼다:

```json
    "removeNote": "Only offline nodes can be removed here. To remove a live node, run oplctl cluster leave on it.",
    "removed": "Removed {{name}} from the cluster.",
    "reason": {
      "thisNode": "You are connected through this node.",
      "online": "The node is online. Run oplctl cluster leave on it."
    }
  },
```

`web/src/i18n/en.json`에서

```json
    "adminOnly": "Only administrators can manage users.",
    "selfNote": "Change your own password from the account menu. Only another administrator can change your role or delete your account."
  },
```

을 다음으로 바꾼다:

```json
    "adminOnly": "Only administrators can manage users.",
    "selfNote": "Change your own password from the account menu. Only another administrator can change your role or delete your account.",
    "reason": {
      "selfRole": "Only another administrator can change your role.",
      "selfPassword": "Change your own password from the account menu.",
      "selfDelete": "Only another administrator can delete your account."
    }
  },
```

`web/src/i18n/ko.json`에서

```json
    },
    "forceStopReverting": "스냅샷을 되돌리는 중에는 강제 정지할 수 없습니다.",
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
```

을 다음으로 바꾼다:

```json
    },
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
```

`web/src/i18n/ko.json`에서

```json
    "deleteDescription": "디스크 이미지와 그 안의 데이터가 모두 지워집니다.",
    "inUse": "먼저 VM에서 빼세요.",
    "resize": "크기 늘리기",
```

을 다음으로 바꾼다:

```json
    "deleteDescription": "디스크 이미지와 그 안의 데이터가 모두 지워집니다.",
    "resize": "크기 늘리기",
```

`web/src/i18n/ko.json`에서

```json
    "newSize": "새 크기(GiB)",
    "notReady": "준비되면 쓸 수 있습니다.",
    "errors": {
```

을 다음으로 바꾼다:

```json
    "newSize": "새 크기(GiB)",
    "errors": {
```

`web/src/i18n/ko.json`에서

```json
      "grow": "지금 크기보다 큰 값을 넣으세요."
    }
```

을 다음으로 바꾼다:

```json
      "grow": "지금 크기보다 큰 값을 넣으세요."
    },
    "reason": {
      "inUse": "먼저 VM에서 빼세요.",
      "notReady": "준비되면 쓸 수 있습니다.",
      "vmsUnknown": "어느 VM이 쓰는지 알 수 없습니다."
    }
```

`web/src/i18n/ko.json`에서

```json
    "removeNote": "오프라인 노드만 여기서 제거할 수 있습니다. 살아 있는 노드는 그 노드에서 oplctl cluster leave를 실행하세요.",
    "removed": "{{name}}을(를) 클러스터에서 제거했습니다."
  },
```

을 다음으로 바꾼다:

```json
    "removeNote": "오프라인 노드만 여기서 제거할 수 있습니다. 살아 있는 노드는 그 노드에서 oplctl cluster leave를 실행하세요.",
    "removed": "{{name}}을(를) 클러스터에서 제거했습니다.",
    "reason": {
      "thisNode": "지금 이 노드로 접속해 있습니다.",
      "online": "노드가 온라인입니다. 그 노드에서 oplctl cluster leave를 실행하세요."
    }
  },
```

`web/src/i18n/ko.json`에서

```json
    "adminOnly": "사용자는 관리자만 관리할 수 있습니다.",
    "selfNote": "내 비밀번호는 계정 메뉴에서 바꿉니다. 내 역할 변경과 삭제는 다른 관리자가 합니다."
  },
```

을 다음으로 바꾼다:

```json
    "adminOnly": "사용자는 관리자만 관리할 수 있습니다.",
    "selfNote": "내 비밀번호는 계정 메뉴에서 바꿉니다. 내 역할 변경과 삭제는 다른 관리자가 합니다.",
    "reason": {
      "selfRole": "내 역할은 다른 관리자가 바꿉니다.",
      "selfPassword": "내 비밀번호는 계정 메뉴에서 바꿉니다.",
      "selfDelete": "내 계정은 다른 관리자가 삭제합니다."
    }
  },
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Remove from cluster" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
});
```

을 다음으로 바꾼다:

```tsx
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const remove = await screen.findByRole("menuitem", { name: "Remove from cluster" });
  expect(remove).toHaveAttribute("aria-disabled", "true");
  expect(remove).toHaveAccessibleDescription("You are connected through this node.");
});
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { TableMessage } from "@/components/TableMessage";
```

을 다음으로 바꾼다:

```tsx
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
                      <DropdownMenuSeparator />
                      <DropdownMenuItem
                        variant="destructive"
                        // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
                        disabled={node.status.online || node.id === cluster.data?.node}
                        onSelect={() => setRemoving(node)}
                      >
                        {t("nodes.remove")}
                      </DropdownMenuItem>
                    </DropdownMenuContent>
```

을 다음으로 바꾼다:

```tsx
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
                        reason={
                          node.id === cluster.data?.node
                            ? "nodes.reason.thisNode"
                            : node.status.online
                              ? "nodes.reason.online"
                              : null
                        }
                        onSelect={() => setRemoving(node)}
                      >
                        {t("nodes.remove")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  expect(within(root).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(root).getByRole("button", { name: "Delete root" })).toBeDisabled();
  expect(root).toHaveTextContent("Remove it from the VM first.");
  const img = within(table).getByText("img").closest("tr")!;
```

을 다음으로 바꾼다:

```tsx
  expect(within(root).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(root).getByRole("button", { name: "Delete root" })).toHaveAccessibleDescription(
    "Remove it from the VM first.",
  );
  const img = within(table).getByText("img").closest("tr")!;
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  expect(within(img).getAllByRole("cell")[sizeColumn]).toHaveTextContent("—");
  expect(within(img).getByRole("button", { name: "Delete img" })).toBeDisabled();
  expect(within(img).getByRole("button", { name: "Resize img" })).toBeDisabled();
  expect(img).toHaveTextContent("Available once ready.");
  expect(within(root).getByRole("button", { name: "Resize root" })).toBeEnabled();
  expect(root).not.toHaveTextContent("Available once ready.");
```

을 다음으로 바꾼다:

```tsx
  expect(within(img).getAllByRole("cell")[sizeColumn]).toHaveTextContent("—");
  expect(within(img).getByRole("button", { name: "Delete img" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(img).getByRole("button", { name: "Resize img" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(img).getByRole("button", { name: "Resize img" })).toHaveAccessibleDescription(
    "Available once ready.",
  );
  // 두 버튼이 같은 이유면 한 번만 보인다.
  expect(within(img).getAllByText("Available once ready.")).toHaveLength(1);
  expect(within(root).getByRole("button", { name: "Resize root" })).not.toHaveAttribute(
    "aria-disabled",
  );
  expect(root).not.toHaveTextContent("Available once ready.");
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  const data = (await screen.findByText("data")).closest("tr")!;
  expect(within(data).getByRole("button", { name: "Delete data" })).toBeDisabled();
  expect(data).toHaveTextContent("0000abcd");
```

을 다음으로 바꾼다:

```tsx
  const data = (await screen.findByText("data")).closest("tr")!;
  expect(within(data).getByRole("button", { name: "Delete data" })).toHaveAccessibleDescription(
    "Cannot tell which VMs use it.",
  );
  expect(data).toHaveTextContent("0000abcd");
```

`web/src/storage/VolumesPage.tsx` 전체:

```tsx
import { Maximize2, Plus, Trash2, Upload } from "lucide-react";
import { type RefObject, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Vm, Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes } from "@/lib/format";
import { UploadDialog } from "@/storage/UploadDialog";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";
import { gibToBytes } from "@/vms/form";

const MIB = 1024 ** 2;

export function VolumesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  // 볼륨 id. 대화상자는 살아 있는 목록에서 찾아 지금 크기를 최신으로 보인다.
  const [resizing, setResizing] = useState<string | null>(null);
  const resizingVolume = volumes.data?.find((v) => v.id === resizing);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "volume");
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userByVolume = useMemo(() => {
    const users = new Map<string, Vm>();
    for (const vm of vms.data ?? []) {
      for (const disk of vm.spec.disks ?? []) {
        // 목록에서 먼저 나온 VM을 보인다.
        if (!users.has(disk.volume)) {
          users.set(disk.volume, vm);
        }
      }
    }
    return users;
  }, [vms.data]);
  const nodeById = useMemo(() => new Map(nodes.data?.map((n) => [n.id, n])), [nodes.data]);
  const reasonBase = useId();

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="volumes-title" className="text-2xl font-semibold">
          {t("nav.volumes")}
        </h1>
        {write && (
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setUploading(true)}>
              <Upload />
              {t("uploads.open")}
            </Button>
            <Button onClick={() => setCreating(true)}>
              <Plus />
              {t("volumes.create")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="volumes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("volumes.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("volumes.size")}</TableHead>
            <TableHead>{t("volumes.allocated")}</TableHead>
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("volumes.state")}</TableHead>
            <TableHead>{t("volumes.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(volumes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={volumes} empty={t("volumes.empty")} />
          )}
          {(volumes.data ?? []).map((volume) => {
            const vm = userByVolume.get(volume.name);
            const ready = volume.status.phase === "ready";
            const node = volume.status.node ? nodeById.get(volume.status.node) : undefined;
            // 만드는 중인 볼륨은 서버가 크기를 바꾸지 않는다(volume_not_ready).
            const resizeReason = ready ? null : "volumes.reason.notReady";
            // 서버는 VM이 쓰거나 아직 만드는 볼륨을 지우지 않는다. VM 목록을 다 읽어야 안다.
            const deleteReason = !ready
              ? "volumes.reason.notReady"
              : !vms.isSuccess
                ? "volumes.reason.vmsUnknown"
                : vm
                  ? "volumes.reason.inUse"
                  : null;
            const reasons = `${reasonBase}-${volume.id}`;
            return (
              <TableRow key={volume.id}>
                <TableCell className="font-medium">{volume.name}</TableCell>
                <TableCell>{volume.spec.pool}</TableCell>
                {/* 가져오는 볼륨은 끝나기 전까지 크기를 모른다. */}
                <TableCell>
                  {volume.spec.size_bytes > 0 ? formatBytes(volume.spec.size_bytes) : "—"}
                </TableCell>
                <TableCell>
                  {volume.status.allocated_bytes !== null
                    ? formatBytes(volume.status.allocated_bytes)
                    : "—"}
                </TableCell>
                <TableCell>{volume.spec.format}</TableCell>
                <TableCell>
                  <Badge variant={ready ? "default" : "secondary"}>
                    {t(`volumes.phase.${volume.status.phase}`)}
                  </Badge>
                </TableCell>
                <TableCell>{node?.name ?? volume.status.node?.slice(-8) ?? "—"}</TableCell>
                <TableCell>
                  {vm ? (
                    <Link
                      to={`/vms/${encodeURIComponent(vm.name)}`}
                      className="underline-offset-4 hover:underline"
                    >
                      {vm.name}
                    </Link>
                  ) : (
                    "—"
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.resizeNamed", { name: volume.name })}
                      reason={resizeReason}
                      reasons={reasons}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setResizing(volume.id);
                      }}
                    >
                      <Maximize2 />
                    </ReasonButton>
                    <ReasonButton
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.deleteNamed", { name: volume.name })}
                      reason={deleteReason}
                      reasons={reasons}
                      onClick={() => setDeleting(volume)}
                    >
                      <Trash2 />
                    </ReasonButton>
                    <ReasonList base={reasons} reasons={[resizeReason, deleteReason]} />
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
      {resizingVolume && (
        <ResizeVolumeDialog
          volume={resizingVolume}
          returnFocus={opener}
          onClose={() => setResizing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("volumes.deleteTitle", { name: deleting?.name })}
        description={t("volumes.deleteDescription")}
        confirmLabel={t("volumes.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const volume = deleting!;
          const task = await call(
            api.DELETE("/volumes/{key}", { params: { path: { key: volume.id } } }),
          );
          trackTask(task, t("tasks.op.volume-delete", { target: volume.name }));
        }}
      />
    </div>
  );
}

function CreateVolumeDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sizeGib, setSizeGib] = useState("16");
  const [format, setFormat] = useState<components["schemas"]["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    const size = gibToBytes(sizeGib);
    if (size === undefined) {
      found.size = "vmForm.errors.size";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/volumes", {
        body: { name, spec: { pool, size_bytes: size!, format } },
      }),
    );
    trackTask(task, t("tasks.op.volume-create", { target: name }));
  }

  return (
    <FormDialog
      title={t("volumes.createTitle")}
      description={t("volumes.createDescription")}
      submitLabel={t("volumes.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="volume-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field id="volume-pool" label={t("volumes.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="volume-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">—</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field id="volume-size" label={t("vmForm.sizeGib")} error={errors.size}>
          {(a11y) => (
            <Input
              id="volume-size"
              inputMode="decimal"
              value={sizeGib}
              onChange={(e) => setSizeGib(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field id="volume-format" label={t("volumes.format")}>
          <NativeSelect
            id="volume-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/** 볼륨 크기 늘리기. 서버는 지금보다 크고 512 바이트 배수인 크기만 받는다. */
function ResizeVolumeDialog({
  volume,
  returnFocus,
  onClose,
}: {
  volume: Volume;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [sizeGib, setSizeGib] = useState("");
  const [error, setError] = useState<string>();

  async function submit() {
    // qcow2는 512 배수가 아닌 크기로 늘리지 못하므로 1 MiB 단위로 올린 값을 비교하고 보낸다.
    const bytes = gibToBytes(sizeGib);
    const size = bytes === undefined ? undefined : Math.ceil(bytes / MIB) * MIB;
    const found =
      size === undefined
        ? "vmForm.errors.size"
        : size <= volume.spec.size_bytes
          ? "volumes.errors.grow"
          : undefined;
    setError(found);
    if (found) {
      return false;
    }
    const task = await call(
      api.POST("/volumes/{key}/resize", {
        params: { path: { key: volume.id } },
        body: { size_bytes: size! },
      }),
    );
    trackTask(task, t("tasks.op.volume-resize", { target: volume.name }));
  }

  return (
    <FormDialog
      title={t("volumes.resizeTitle", { name: volume.name })}
      description={t("volumes.resizeDescription")}
      submitLabel={t("volumes.resize")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <p className="text-sm">
        {t("volumes.currentSize", { size: formatBytes(volume.spec.size_bytes) })}
      </p>
      <Field id="volume-new-size" label={t("volumes.newSize")} error={error}>
        {(a11y) => (
          <Input
            id="volume-new-size"
            inputMode="decimal"
            value={sizeGib}
            onChange={(e) => setSizeGib(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  expect(screen.getByText("1 × 2 cores (host)")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Delete" })).toBeDisabled();
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("1 × 2 cores (host)")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Start" })).toHaveAttribute("aria-disabled", "true");
  expect(screen.getByRole("button", { name: "Start" })).toHaveAccessibleDescription(
    "The VM is running.",
  );
  expect(screen.getByRole("button", { name: "Delete" })).toHaveAttribute("aria-disabled", "true");
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(button).toBeDisabled());
  expect(button).toHaveAccessibleDescription(
    "Force stop is not available while a snapshot is being reverted.",
  );
  // 되돌리기가 끝나면 다시 켠다.
```

을 다음으로 바꾼다:

```tsx
  const button = await screen.findByRole("button", { name: "Force stop" });
  await waitFor(() => expect(button).toHaveAttribute("aria-disabled", "true"));
  expect(button).toHaveAccessibleDescription("Not available while a snapshot is being reverted.");
  // 되돌리기가 끝나면 다시 켠다.
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  });
  await waitFor(() => expect(button).toBeEnabled());
  expect(button).not.toHaveAccessibleDescription();
  expect(
    screen.queryByText("Force stop is not available while a snapshot is being reverted."),
  ).not.toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  });
  await waitFor(() => expect(button).not.toHaveAttribute("aria-disabled"));
  expect(button).not.toHaveAccessibleDescription();
  expect(
    screen.queryByText("Not available while a snapshot is being reverted."),
  ).not.toBeInTheDocument();
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  expect(button).toBeDisabled();
  expect(button).not.toHaveAccessibleDescription();
});
```

을 다음으로 바꾼다:

```tsx
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  expect(button).toHaveAttribute("aria-disabled", "true");
  expect(button).toHaveAccessibleDescription("The VM is stopped.");
});

it("keeps the same console link when the VM stops and blocks it with a reason", async () => {
  mockLists({ vm: [web], node: [NODE] });
  const { queryClient } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const link = await screen.findByRole("link", { name: "Open console" });
  expect(link).not.toHaveAttribute("aria-disabled");
  link.focus();
  // 멈추면 같은 링크가 막히고 이유를 읽는다. 포커스는 그대로다.
  queryClient.setQueryData(["list", "vm"], {
    revision: 2,
    items: [
      {
        ...web,
        spec: { ...web.spec, power: "stopped" },
        status: { ...web.status, phase: "stopped" },
      },
    ],
  });
  await waitFor(() => expect(link).toHaveAttribute("aria-disabled", "true"));
  expect(link).toHaveAccessibleDescription("The VM is stopped.");
  expect(link).toHaveFocus();
});
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { ListPending } from "@/components/ListPending";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  const operate = can(me.role, "operate", "vm");
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const forceStop = availability(vm, "force-stop", vmTasks);
  const reverting = operate && !forceStop.ok && forceStop.reason === "vms.reason.reverting";
  const button = (action: VmAction) => (
    <Button
      key={action}
      variant="outline"
      size="sm"
      disabled={!availability(vm, action, vmTasks).ok}
      aria-describedby={action === "force-stop" && reverting ? hint : undefined}
      onClick={() => (action === "force-stop" ? setConfirm("force-stop") : void run(vm, action))}
    >
      {t(`vms.action.${action}`)}
    </Button>
  );
```

을 다음으로 바꾼다:

```tsx
  const operate = can(me.role, "operate", "vm");
  const write = can(me.role, "write", "vm");
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmAction | "console" | "delete") => {
    const able = availability(vm, action, vmTasks);
    return able.ok ? null : able.reason;
  };
  const consoleReason = reasonOf("console");
  // 보이는 버튼의 막힌 이유만 모은다.
  const reasons = [
    ...(operate ? [...ACTIONS.map(reasonOf), consoleReason] : []),
    write ? reasonOf("delete") : null,
  ];
  const button = (action: VmAction) => (
    <ReasonButton
      key={action}
      variant="outline"
      size="sm"
      reason={reasonOf(action)}
      reasons={hint}
      onClick={() => (action === "force-stop" ? setConfirm("force-stop") : void run(vm, action))}
    >
      {t(`vms.action.${action}`)}
    </ReasonButton>
  );
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
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
```

을 다음으로 바꾼다:

```tsx
        {operate && ACTIONS.map(button)}
        {/* 막혀도 같은 링크로 두어 상태가 바뀔 때 포커스를 잃지 않는다. 막히면 눌러도 열리지 않는다. */}
        {operate && (
          <ReasonButton variant="outline" size="sm" reason={consoleReason} reasons={hint} asChild>
            <Link to={`/vms/${encodeURIComponent(vm.name)}/console`} target="_blank">
              {t("vms.openConsole")}
            </Link>
          </ReasonButton>
        )}
        {write && (
          <Button variant="outline" size="sm" asChild>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
        )}
        {can(me.role, "write", "vm") && can(me.role, "write", "volume") && (
          <Button variant="outline" size="sm" onClick={() => setConfirm("clone")}>
```

을 다음으로 바꾼다:

```tsx
        )}
        {write && can(me.role, "write", "volume") && (
          <Button variant="outline" size="sm" onClick={() => setConfirm("clone")}>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
        )}
        {can(me.role, "write", "vm") && (
          <Button
            variant="destructive"
            size="sm"
            disabled={!availability(vm, "delete", vmTasks).ok}
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
```

을 다음으로 바꾼다:

```tsx
        )}
        {write && (
          <ReasonButton
            variant="destructive"
            size="sm"
            reason={reasonOf("delete")}
            reasons={hint}
            onClick={() => setConfirm("delete")}
          >
            {t("vms.action.delete")}
          </ReasonButton>
        )}
      </div>
      <ReasonList base={hint} reasons={reasons} className="basis-full" />
      <VmConfirm
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription(
    "Force stop is not available while a snapshot is being reverted.",
  );
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
```

을 다음으로 바꾼다:

```tsx
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription("Not available while a snapshot is being reverted.");
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  await userEvent.click(screen.getByRole("button", { name: "Actions for db" }));
  expect(await screen.findByRole("menuitem", { name: "Start" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await userEvent.keyboard("{Escape}");
```

을 다음으로 바꾼다:

```tsx
  await userEvent.click(screen.getByRole("button", { name: "Actions for db" }));
  const start = await screen.findByRole("menuitem", { name: "Start" });
  expect(start).toHaveAttribute("aria-disabled", "true");
  expect(start).toHaveAccessibleDescription("A power action is still in progress.");
  // 골라도 시작하지 않는다.
  await userEvent.click(start);
  await userEvent.keyboard("{Escape}");
```

`web/src/vms/VmMenu.tsx`에서

```tsx
import { MoreHorizontal } from "lucide-react";
import { useId, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
```

을 다음으로 바꾼다:

```tsx
import { MoreHorizontal } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Button } from "@/components/ui/button";
```

을 다음으로 바꾼다:

```tsx
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { Button } from "@/components/ui/button";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const hint = useId();
  // 서버는 되돌리기 중의 강제 정지를 받지 않는다. Task 목록을 아직 모르면 서버에 맡긴다.
  // 상태로도 끌 수 없는 VM이면 까닭은 되돌리기가 아니므로 보이지 않는다.
  const forceStop = availability(vm, "force-stop", vmTasks);
  const reverting = !forceStop.ok && forceStop.reason === "vms.reason.reverting";
  const operate = can(me.role, "operate", "vm");
```

을 다음으로 바꾼다:

```tsx
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  // Task 목록을 아직 모르면 Task 때문에는 막지 않는다(서버에 맡긴다).
  const reasonOf = (action: VmAction | "console" | "delete") => {
    const able = availability(vm, action, vmTasks);
    return able.ok ? null : able.reason;
  };
  const operate = can(me.role, "operate", "vm");
```

`web/src/vms/VmMenu.tsx`에서

```tsx
            ACTIONS.map((action) => (
              <DropdownMenuItem
                key={action}
                disabled={!availability(vm, action, vmTasks).ok}
                aria-describedby={action === "force-stop" && reverting ? hint : undefined}
                onSelect={() =>
```

을 다음으로 바꾼다:

```tsx
            ACTIONS.map((action) => (
              <ReasonMenuItem
                key={action}
                reason={reasonOf(action)}
                onSelect={() =>
```

`web/src/vms/VmMenu.tsx`에서

```tsx
                {t(`vms.action.${action}`)}
              </DropdownMenuItem>
            ))}
          {/* Radix는 비활성 항목을 키보드로 건너뛰므로 aria-describedby가 읽히지 않을 수 있다.
              같은 까닭은 상세 화면에도 보인다. */}
          {operate && reverting && (
            <p id={hint} className="px-1.5 py-1 text-xs text-muted-foreground">
              {t("vms.forceStopReverting")}
            </p>
          )}
          {operate && (
            <DropdownMenuItem
              disabled={!availability(vm, "console", vmTasks).ok}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
            >
              {t("vms.action.console")}
            </DropdownMenuItem>
          )}
```

을 다음으로 바꾼다:

```tsx
                {t(`vms.action.${action}`)}
              </ReasonMenuItem>
            ))}
          {operate && (
            <ReasonMenuItem
              reason={reasonOf("console")}
              onSelect={() => void navigate(`/vms/${encodeURIComponent(vm.name)}/console`)}
            >
              {t("vms.action.console")}
            </ReasonMenuItem>
          )}
```

`web/src/vms/VmMenu.tsx`에서

```tsx
              <DropdownMenuSeparator />
              <DropdownMenuItem
                variant="destructive"
                disabled={!availability(vm, "delete", vmTasks).ok}
                onSelect={() => setConfirm("delete")}
              >
                {t("vms.action.delete")}
              </DropdownMenuItem>
            </>
```

을 다음으로 바꾼다:

```tsx
              <DropdownMenuSeparator />
              <ReasonMenuItem
                variant="destructive"
                reason={reasonOf("delete")}
                onSelect={() => setConfirm("delete")}
              >
                {t("vms.action.delete")}
              </ReasonMenuItem>
            </>
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 247개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 막힌 메뉴 항목·버튼이 이유를 보이고 화면 읽기 프로그램이 읽게"
```

---

### Task 6: 웹: 작업 목록 페이지 나누기

새 `components/Pager.tsx`: `usePage(items, pageSize)` → `{ page, pageItems, pageCount, setPage }`(항목이 줄어 페이지가 넘치면 마지막 페이지로), `Pager`(`nav`, 이전/다음은 끝에서 `aria-disabled`라 포커스를 잃지 않음, 범위 "51–100 / 734", 항목이 없으면 그리지 않음). `TasksPage`는 `SHOWN = 200`을 없애고 `TASK_PAGE_SIZE`(50)개씩, "실패만"을 바꾸면 첫 페이지. VM 상세 작업 탭도 50개씩(`key={vm.id}`). 번역 키 `pager.*`. 테스트: `usePage`/`Pager`, 작업 화면의 다음 페이지와 필터.

**Files:**
- Create: `web/src/components/Pager.test.tsx`
- Create: `web/src/components/Pager.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/components/Pager.test.tsx`를 만든다:

```tsx
import { act, render, renderHook, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Pager, usePage } from "@/components/Pager";

const range = (n: number) => Array.from({ length: n }, (_, i) => i + 1);

it("pages through items", () => {
  const { result } = renderHook(() => usePage(range(120), 50));
  expect(result.current.page).toBe(0);
  expect(result.current.pageCount).toBe(3);
  expect(result.current.pageItems).toEqual(range(50));
  act(() => result.current.setPage(2));
  expect(result.current.pageItems).toEqual(range(120).slice(100));
});

it("moves to the last page when items shrink", () => {
  const { result, rerender } = renderHook(({ items }) => usePage(items, 50), {
    initialProps: { items: range(120) },
  });
  act(() => result.current.setPage(2));
  rerender({ items: range(60) });
  expect(result.current.page).toBe(1);
  expect(result.current.pageItems).toEqual([51, 52, 53, 54, 55, 56, 57, 58, 59, 60]);
  // 다시 늘어도 옮긴 페이지에 머문다.
  rerender({ items: range(120) });
  expect(result.current.page).toBe(1);
});

function Paged({ total }: { total: number }) {
  const pager = usePage(range(total), 50);
  return (
    <>
      <p>first {pager.pageItems[0]}</p>
      <Pager {...pager} pageSize={50} total={total} />
    </>
  );
}

it("shows the range and moves between pages", async () => {
  render(<Paged total={120} />);
  expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Previous" })).toHaveAttribute("aria-disabled", "true");
  await userEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
  expect(screen.getByText("first 51")).toBeInTheDocument();
  const next = screen.getByRole("button", { name: "Next" });
  await userEvent.click(next);
  expect(screen.getByText("101–120 of 120")).toBeInTheDocument();
  // 마지막 페이지에서 막혀도 포커스는 그대로이고, 또 눌러도 넘어가지 않는다.
  expect(next).toHaveAttribute("aria-disabled", "true");
  expect(next).toHaveFocus();
  await userEvent.click(next);
  expect(screen.getByText("101–120 of 120")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Previous" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
});

it("draws nothing without items", () => {
  render(<Paged total={0} />);
  expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});
```

`web/src/components/Pager.tsx`를 만든다:

```tsx
import { cn } from "cn";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";

/**
 * `items`를 `pageSize`개씩 나눈다. `page`는 0부터 센다. 항목이 줄어 지금 페이지가 없어지면 마지막
 * 페이지로 옮긴다.
 */
export function usePage<T>(items: readonly T[], pageSize: number) {
  const [page, setPage] = useState(0);
  const pageCount = Math.ceil(items.length / pageSize);
  const last = Math.max(pageCount - 1, 0);
  if (page > last) {
    // 렌더 중에 상태를 맞춘다(React가 바로 다시 그린다).
    setPage(last);
  }
  const current = Math.min(page, last);
  return {
    page: current,
    pageItems: items.slice(current * pageSize, (current + 1) * pageSize),
    pageCount,
    setPage,
  };
}

/**
 * "이전/다음" 버튼과 지금 보이는 범위. 항목이 없으면 그리지 않는다. 끝에 닿은 버튼은 `disabled` 대신
 * `aria-disabled`로 막아 누른 버튼이 포커스를 잃지 않게 한다.
 */
export function Pager({
  page,
  pageCount,
  setPage,
  pageSize,
  total,
}: {
  page: number;
  pageCount: number;
  setPage(page: number): void;
  pageSize: number;
  total: number;
}) {
  const { t } = useTranslation();
  if (total === 0) {
    return null;
  }
  const from = page * pageSize + 1;
  const to = Math.min((page + 1) * pageSize, total);
  const first = page === 0;
  const last = page >= pageCount - 1;
  return (
    <nav aria-label={t("pager.label")} className="flex items-center justify-end gap-2">
      <span className="text-sm text-muted-foreground">{t("pager.range", { from, to, total })}</span>
      <Button
        variant="outline"
        size="sm"
        aria-disabled={first || undefined}
        className={cn(first && "opacity-50")}
        onClick={() => !first && setPage(page - 1)}
      >
        {t("pager.previous")}
      </Button>
      <Button
        variant="outline"
        size="sm"
        aria-disabled={last || undefined}
        className={cn(last && "opacity-50")}
        onClick={() => !last && setPage(page + 1)}
      >
        {t("pager.next")}
      </Button>
    </nav>
  );
}
```

`web/src/i18n/en.json`에서

```json
      "vm-clone": "Clone {{target}}"
    }
  },
  "common": {
```

을 다음으로 바꾼다:

```json
      "vm-clone": "Clone {{target}}"
    }
  },
  "pager": {
    "label": "Pages",
    "previous": "Previous",
    "next": "Next",
    "range": "{{from}}–{{to}} of {{total}}"
  },
  "common": {
```

`web/src/i18n/ko.json`에서

```json
      "vm-clone": "{{target}} 복제"
    }
  },
  "common": {
```

을 다음으로 바꾼다:

```json
      "vm-clone": "{{target}} 복제"
    }
  },
  "pager": {
    "label": "페이지",
    "previous": "이전",
    "next": "다음",
    "range": "{{from}}–{{to}} / {{total}}"
  },
  "common": {
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx
  expect(screen.getByText("No failed tasks.")).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("No failed tasks.")).toBeInTheDocument();
});

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
  return Array.from({ length: n }, (_, i) => ({
    id: `019a0f3c-8a10-7000-8000-${String(i).padStart(12, "0")}`,
    spec: { op: { kind: "vm-start" as const }, target: VM, node: "n1" },
    status: { state, error: state === "failed" ? "boom" : null },
  }));
}

it("shows tasks fifty at a time", async () => {
  mockLists({ vm: [{ id: VM, name: "web" }], task: many(51) });
  renderPage(<TasksPage />);
  expect(await screen.findByText("1–50 of 51")).toBeInTheDocument();
  // 머리 줄 + 50줄
  expect(screen.getAllByRole("row")).toHaveLength(51);
  await userEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("51–51 of 51")).toBeInTheDocument();
  expect(screen.getAllByRole("row")).toHaveLength(2);
});

it("goes back to the first page when the filter changes", async () => {
  mockLists({ vm: [{ id: VM, name: "web" }], task: many(120, "failed") });
  renderPage(<TasksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Next" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
  await userEvent.click(screen.getByLabelText("Failed only"));
  expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
});
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
```

을 다음으로 바꾼다:

```tsx
import { Checkbox } from "@/components/ui/checkbox";
import { Pager, usePage } from "@/components/Pager";
import { Label } from "@/components/ui/label";
```

`web/src/tasks/TasksPage.tsx`에서

```tsx

/** API에 페이지 나누기가 없어 모두 받아 최근 것만 보여 준다. */
const SHOWN = 200;
```

을 다음으로 바꾼다:

```tsx

/** 한 페이지에 보일 Task 수. API에 페이지 나누기가 없어 모두 받아 화면에서 나눈다. */
export const TASK_PAGE_SIZE = 50;
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
    () =>
      newestFirst(tasks.data ?? [])
        .filter((task) => !failedOnly || task.status.state === "failed")
        .slice(0, SHOWN),
    [tasks.data, failedOnly],
  );
  return (
```

을 다음으로 바꾼다:

```tsx
    () =>
      newestFirst(tasks.data ?? []).filter((task) => !failedOnly || task.status.state === "failed"),
    [tasks.data, failedOnly],
  );
  const pager = usePage(shown, TASK_PAGE_SIZE);
  return (
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
            checked={failedOnly}
            onCheckedChange={(v) => setFailedOnly(v === true)}
          />
```

을 다음으로 바꾼다:

```tsx
            checked={failedOnly}
            onCheckedChange={(v) => {
              setFailedOnly(v === true);
              pager.setPage(0);
            }}
          />
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
      <TaskTable
        tasks={shown}
        labelledBy="tasks-title"
        empty={failedOnly ? t("tasks.emptyFailed") : undefined}
      />
    </div>
```

을 다음으로 바꾼다:

```tsx
      <TaskTable
        tasks={pager.pageItems}
        labelledBy="tasks-title"
        empty={failedOnly ? t("tasks.emptyFailed") : undefined}
      />
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={shown.length} />
    </div>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { ListPending } from "@/components/ListPending";
import { ReasonButton, ReasonList } from "@/components/Reason";
```

을 다음으로 바꾼다:

```tsx
import { ListPending } from "@/components/ListPending";
import { Pager, usePage } from "@/components/Pager";
import { ReasonButton, ReasonList } from "@/components/Reason";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { formatMib, formatTime, uuidTime } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
```

을 다음으로 바꾼다:

```tsx
import { formatMib, formatTime, uuidTime } from "@/lib/format";
import { newestFirst, TASK_PAGE_SIZE, TaskTable } from "@/tasks/TasksPage";
import { ACTIONS, availability, useVmActions, type VmAction } from "@/vms/actions";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
        <TabsContent value="tasks">
          <VmTasks vm={vm} />
        </TabsContent>
```

을 다음으로 바꾼다:

```tsx
        <TabsContent value="tasks">
          <VmTasks key={vm.id} vm={vm} />
        </TabsContent>
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
  );
  return (
    <section>
      <h2 id="vm-tasks" className="sr-only">
        {t("vms.tab.tasks")}
      </h2>
      <TaskTable tasks={mine.slice(0, 50)} labelledBy="vm-tasks" />
    </section>
```

을 다음으로 바꾼다:

```tsx
  );
  const pager = usePage(mine, TASK_PAGE_SIZE);
  return (
    <section className="flex flex-col gap-4">
      <h2 id="vm-tasks" className="sr-only">
        {t("vms.tab.tasks")}
      </h2>
      <TaskTable tasks={pager.pageItems} labelledBy="vm-tasks" />
      <Pager {...pager} pageSize={TASK_PAGE_SIZE} total={mine.length} />
    </section>
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 253개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 작업 목록과 VM 작업 탭을 50개씩 페이지로 나누기"
```

---

### Task 7: 웹: VM 폼 나누기

`VmFormPage.tsx`의 카드를 `vms/form/`의 `GeneralSection`·`DisksSection`·`NicsSection`·`ScheduleSection`으로 옮긴다(각각 `values`·`errors`·`onChange(patch)`, 행을 더하고 지우는 섹션은 오류를 비우는 `onRowsChange`, 필요한 목록). 상태·`reload`·`submit`은 `VmForm`에 남기고, 새 볼륨 만들기와 기다리기·VM POST/PUT(If-Match)·충돌 판단은 `form/submit.ts`의 `createVolume`·`createVm`·`updateVm`·`isConflict`로 뺀다. 동작은 그대로이고 `VmFormPage.test.tsx`는 고치지 않는다. 테스트: `submit.ts` 단위.

**Files:**
- Create: `web/src/vms/form/DisksSection.tsx`
- Create: `web/src/vms/form/GeneralSection.tsx`
- Create: `web/src/vms/form/NicsSection.tsx`
- Create: `web/src/vms/form/ScheduleSection.tsx`
- Create: `web/src/vms/form/submit.test.ts`
- Create: `web/src/vms/form/submit.ts`
- Modify: `web/src/vms/VmFormPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/vms/VmFormPage.tsx` 전체:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";

import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
import { isStopped } from "@/vms/actions";
import { defaultMachine, emptyForm, fromVm, validate, type VmFormValues } from "@/vms/form";
import { DisksSection } from "@/vms/form/DisksSection";
import { GeneralSection } from "@/vms/form/GeneralSection";
import { NicsSection } from "@/vms/form/NicsSection";
import { ScheduleSection } from "@/vms/form/ScheduleSection";
import { createVm, createVolume, isConflict, updateVm } from "@/vms/form/submit";
import { useVmByName } from "@/vms/VmDetailPage";

export function VmCreatePage() {
  const nodes = useList("node");
  if (!nodes.data) {
    return <ListPending error={nodes.error} />;
  }
  return <VmForm initial={emptyForm(defaultMachine(nodes.data))} />;
}

export function VmEditPage() {
  const { t } = useTranslation();
  const { name = "" } = useParams();
  const vms = useList("vm");
  const vm = useVmByName(name);
  if (vm === undefined) {
    return <ListPending error={vms.error} />;
  }
  if (vm === null) {
    return <h1 className="text-2xl font-semibold">{t("vms.notFound", { name })}</h1>;
  }
  return <VmForm key={vm.id} vm={vm} initial={fromVm(vm)} />;
}

/** VM 만들기(`vm` 없음)와 고치기 */
function VmForm({ vm, initial }: { vm?: Vm; initial: VmFormValues }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const networks = useList("network");
  const volumes = useList("volume");
  const [values, setValues] = useState(initial);
  const [revision, setRevision] = useState(vm?.revision);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<{ message: string; conflict: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // 사용자가 머신을 직접 고르기 전에는 노드를 바꾸면 그 노드에 맞게 기본값을 다시 정한다(만들 때만).
  const [machineChosen, setMachineChosen] = useState(false);
  // 화면을 떠나면 기다리던 볼륨 Task를 그만 기다리고 VM을 만들지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const creating = vm === undefined;
  // 멈추지 않은 VM(서버의 `is_stopped`가 아닌)의 노드와 디스크는 서버가 바꾸지 않는다.
  const locked = !creating && !isStopped(vm);

  const change = (patch: Partial<VmFormValues>) => setValues((v) => ({ ...v, ...patch }));
  // 행을 넣거나 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeRows = (patch: Pick<VmFormValues, "disks"> | Pick<VmFormValues, "nics">) => {
    change(patch);
    setErrors({});
  };

  // 이 VM에 붙어 있거나 아무 데도 붙지 않은 준비된 볼륨
  const freeVolumes = (volumes.data ?? []).filter(
    (v) => v.status.phase === "ready" && (!v.status.attached_to || v.status.attached_to === vm?.id),
  );

  /** 다른 사람이 바꾼 VM을 다시 읽는다. `vm`은 watch로 늘 최신이다(지워지면 이 폼은 사라진다). */
  function reload() {
    if (vm) {
      setValues(fromVm(vm));
      setRevision(vm.revision);
    }
    setFailure(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const found = validate(
      values,
      creating,
      (volumes.data ?? []).map((v) => v.name),
    );
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    setFailure(null);
    const controller = new AbortController();
    abort.current = controller;
    const { signal } = controller;
    // 이번에 만든 볼륨. VM 만들기가 실패하면 남았다고 알린다.
    const created: string[] = [];
    try {
      if (creating) {
        // 새 빈 볼륨을 먼저 만들고, 그 Task가 끝나면 VM을 만든다.
        for (const disk of values.disks.filter((d) => d.kind === "new")) {
          await createVolume(
            queryClient,
            disk,
            t("tasks.op.volume-create", { target: disk.volume }),
            signal,
          );
          created.push(disk.volume);
          // 다시 보낼 때 같은 볼륨을 또 만들지 않게 있는 볼륨으로 바꾼다.
          setValues((v) => ({
            ...v,
            disks: v.disks.map((d) =>
              d.kind === "new" && d.volume === disk.volume ? { ...d, kind: "existing" } : d,
            ),
          }));
        }
        if (signal.aborted) {
          return;
        }
        await createVm(values);
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(values.name)}`);
        }
      } else {
        await updateVm(vm, revision, values);
        if (!signal.aborted) {
          void navigate(`/vms/${encodeURIComponent(vm.name)}`);
        }
      }
    } catch (err) {
      if (signal.aborted) {
        return;
      }
      const message = errorMessage(t, err);
      setFailure({
        message:
          created.length > 0
            ? t("vmForm.volumesCreated", { names: created.join(", "), error: message })
            : message,
        conflict: isConflict(err),
      });
    } finally {
      if (!signal.aborted) {
        setBusy(false);
      }
    }
  }

  return (
    <form onSubmit={submit} className="flex max-w-3xl flex-col gap-4" noValidate>
      <h1 className="text-2xl font-semibold">
        {creating ? t("vmForm.createTitle") : t("vmForm.editTitle", { name: vm.name })}
      </h1>
      {locked && <p className="text-sm text-muted-foreground">{t("vmForm.runningNote")}</p>}

      {/* 보내는 동안에는 고치지 못하게 한다. */}
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
        <GeneralSection
          values={values}
          errors={errors}
          onChange={change}
          nodes={nodes.data}
          creating={creating}
          locked={locked}
          autoMachine={creating && !machineChosen}
          onMachineChosen={() => setMachineChosen(true)}
        />
        <DisksSection
          values={values}
          errors={errors}
          onChange={change}
          onRowsChange={(disks) => changeRows({ disks })}
          volumes={freeVolumes}
          pools={pools.data}
          creating={creating}
          locked={locked}
        />
        <NicsSection
          values={values}
          errors={errors}
          onChange={change}
          onRowsChange={(nics) => changeRows({ nics })}
          networks={networks.data}
        />
        <ScheduleSection
          values={values}
          errors={errors}
          onChange={change}
          volumes={volumes.isSuccess ? volumes.data : undefined}
        />

        {creating && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="vm-start"
              checked={values.start}
              onCheckedChange={(v) => change({ start: v === true })}
            />
            <Label htmlFor="vm-start">{t("vmForm.startAfter")}</Label>
          </div>
        )}
      </fieldset>
      {failure && (
        <div role="alert" className="flex items-center gap-3 text-sm text-destructive">
          <span>{failure.message}</span>
          {failure.conflict && (
            <Button type="button" variant="outline" size="sm" onClick={reload}>
              {t("vmForm.reload")}
            </Button>
          )}
        </div>
      )}
      <div className="flex gap-2">
        <Button type="submit" disabled={busy}>
          {creating ? t("vms.create") : t("vmForm.save")}
        </Button>
        <Button variant="ghost" asChild>
          <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
            {t("confirm.cancel")}
          </Link>
        </Button>
      </div>
    </form>
  );
}
```

`web/src/vms/form/DisksSection.tsx`를 만든다:

```tsx
import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { StoragePool, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { type DiskRow, emptyDisk, type VmFormValues } from "@/vms/form";

/** 디스크 행: 있는 볼륨을 붙이거나(고치기·만들기) 새 빈 볼륨을 만든다(만들기). */
export function DisksSection({
  values,
  errors,
  onChange,
  onRowsChange,
  volumes,
  pools,
  creating,
  locked,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (disks: DiskRow[]) => void;
  /** 붙일 수 있는 볼륨 */
  volumes: Volume[];
  pools: StoragePool[] | undefined;
  creating: boolean;
  /** 멈추지 않은 VM의 디스크는 바꾸지 못한다. */
  locked: boolean;
}) {
  const { t } = useTranslation();
  const setDisk = (i: number, patch: Partial<DiskRow>) =>
    onChange({ disks: values.disks.map((d, j) => (j === i ? { ...d, ...patch } : d)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.disks")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {values.disks.map((disk, i) => (
          <fieldset
            key={i}
            disabled={locked}
            aria-label={t("vmForm.diskN", { n: i + 1 })}
            className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-5"
          >
            {disk.kind === "existing" ? (
              <Field
                id={`disk-${i}-volume`}
                label={t("vms.volume")}
                error={errors[`disks.${i}.volume`]}
              >
                {(a11y) => (
                  <NativeSelect
                    id={`disk-${i}-volume`}
                    {...a11y}
                    className="w-full"
                    value={disk.volume}
                    onChange={(e) => setDisk(i, { volume: e.target.value })}
                  >
                    <NativeSelectOption value="">—</NativeSelectOption>
                    {volumes
                      .filter(
                        (v) =>
                          v.name === disk.volume || !values.disks.some((d) => d.volume === v.name),
                      )
                      .map((v) => (
                        <NativeSelectOption key={v.id} value={v.name}>
                          {v.name}
                        </NativeSelectOption>
                      ))}
                  </NativeSelect>
                )}
              </Field>
            ) : (
              <>
                <Field
                  id={`disk-${i}-volume`}
                  label={t("vmForm.newVolume")}
                  error={errors[`disks.${i}.volume`]}
                >
                  {(a11y) => (
                    <Input
                      id={`disk-${i}-volume`}
                      {...a11y}
                      value={disk.volume}
                      onChange={(e) => setDisk(i, { volume: e.target.value })}
                    />
                  )}
                </Field>
                <Field
                  id={`disk-${i}-pool`}
                  label={t("vmForm.pool")}
                  error={errors[`disks.${i}.pool`]}
                >
                  {(a11y) => (
                    <NativeSelect
                      id={`disk-${i}-pool`}
                      {...a11y}
                      className="w-full"
                      value={disk.pool}
                      onChange={(e) => setDisk(i, { pool: e.target.value })}
                    >
                      <NativeSelectOption value="">—</NativeSelectOption>
                      {pools?.map((p) => (
                        <NativeSelectOption key={p.id} value={p.name}>
                          {p.name}
                        </NativeSelectOption>
                      ))}
                    </NativeSelect>
                  )}
                </Field>
                <Field
                  id={`disk-${i}-size`}
                  label={t("vmForm.sizeGib")}
                  error={errors[`disks.${i}.size`]}
                >
                  {(a11y) => (
                    <Input
                      id={`disk-${i}-size`}
                      {...a11y}
                      type="number"
                      min={1}
                      value={disk.sizeGib}
                      onChange={(e) => setDisk(i, { sizeGib: e.target.value })}
                    />
                  )}
                </Field>
              </>
            )}
            <Field id={`disk-${i}-bus`} label={t("vms.bus")}>
              <NativeSelect
                id={`disk-${i}-bus`}
                className="w-full"
                value={disk.bus}
                onChange={(e) => setDisk(i, { bus: e.target.value as DiskRow["bus"] })}
              >
                <NativeSelectOption value="virtio-blk">virtio-blk</NativeSelectOption>
                <NativeSelectOption value="sata">sata</NativeSelectOption>
              </NativeSelect>
            </Field>
            <div className="flex items-end gap-2">
              <Field
                id={`disk-${i}-boot`}
                label={t("vms.bootOrder")}
                error={errors[`disks.${i}.bootOrder`]}
              >
                {(a11y) => (
                  <Input
                    id={`disk-${i}-boot`}
                    {...a11y}
                    type="number"
                    min={1}
                    max={255}
                    className="w-20"
                    value={disk.bootOrder}
                    onChange={(e) => setDisk(i, { bootOrder: e.target.value })}
                  />
                )}
              </Field>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("vmForm.removeDisk", { n: i + 1 })}
                onClick={() => onRowsChange(values.disks.filter((_, j) => j !== i))}
              >
                <X />
              </Button>
            </div>
          </fieldset>
        ))}
        {!locked && (
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([
                  ...values.disks,
                  { ...emptyDisk("", values.disks.length), kind: "existing" },
                ])
              }
            >
              <Plus />
              {t("vmForm.addExisting")}
            </Button>
            {creating && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() =>
                  onRowsChange([...values.disks, emptyDisk(values.name, values.disks.length)])
                }
              >
                <Plus />
                {t("vmForm.addNew")}
              </Button>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
```

`web/src/vms/form/GeneralSection.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { defaultMachine, type VmFormValues } from "@/vms/form";

/** 이름·CPU·메모리·펌웨어·머신·노드·재시작 정책 */
export function GeneralSection({
  values,
  errors,
  onChange,
  nodes,
  creating,
  locked,
  autoMachine,
  onMachineChosen,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  nodes: Node[] | undefined;
  creating: boolean;
  /** 멈추지 않은 VM의 노드는 바꾸지 못한다. */
  locked: boolean;
  /** 노드를 바꾸면 그 노드에 맞게 머신 기본값을 다시 정한다. */
  autoMachine: boolean;
  /** 사용자가 머신을 직접 골랐다. */
  onMachineChosen: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.general")}</CardTitle>
      </CardHeader>
      <CardContent className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        {creating && (
          <Field id="vm-name" label={t("vms.name")} error={errors.name}>
            {(a11y) => (
              <Input
                id="vm-name"
                {...a11y}
                value={values.name}
                onChange={(e) => onChange({ name: e.target.value })}
              />
            )}
          </Field>
        )}
        <Field id="vm-cores" label={t("vmForm.cores")} error={errors.cores}>
          {(a11y) => (
            <Input
              id="vm-cores"
              {...a11y}
              type="number"
              min={1}
              max={65535}
              value={values.cores}
              onChange={(e) => onChange({ cores: e.target.value })}
            />
          )}
        </Field>
        <Field id="vm-memory" label={t("vmForm.memory")} error={errors.memory}>
          {(a11y) => (
            <Input
              id="vm-memory"
              {...a11y}
              type="number"
              min={64}
              step={256}
              value={values.memory}
              onChange={(e) => onChange({ memory: e.target.value })}
            />
          )}
        </Field>
        <Field id="vm-cpu-model" label={t("vmForm.cpuModel")}>
          <NativeSelect
            id="vm-cpu-model"
            className="w-full"
            value={values.model}
            onChange={(e) => onChange({ model: e.target.value as VmFormValues["model"] })}
          >
            <NativeSelectOption value="host">host</NativeSelectOption>
            <NativeSelectOption value="max">max</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="vm-firmware" label={t("vms.firmware")}>
          <NativeSelect
            id="vm-firmware"
            className="w-full"
            value={values.firmware}
            onChange={(e) => onChange({ firmware: e.target.value as VmFormValues["firmware"] })}
          >
            <NativeSelectOption value="uefi">UEFI</NativeSelectOption>
            <NativeSelectOption value="bios">BIOS</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="vm-machine" label={t("vms.machine")}>
          <NativeSelect
            id="vm-machine"
            className="w-full"
            value={values.machine}
            onChange={(e) => {
              onMachineChosen();
              onChange({ machine: e.target.value as VmFormValues["machine"] });
            }}
          >
            <NativeSelectOption value="q35">q35 (x86_64)</NativeSelectOption>
            <NativeSelectOption value="virt">virt (aarch64)</NativeSelectOption>
          </NativeSelect>
        </Field>
        <Field id="vm-node" label={t("vms.node")}>
          <NativeSelect
            id="vm-node"
            className="w-full"
            disabled={locked}
            value={values.node}
            onChange={(e) => {
              const node = e.target.value;
              onChange(
                autoMachine ? { node, machine: defaultMachine(nodes ?? [], node) } : { node },
              );
            }}
          >
            <NativeSelectOption value="">{t("vmForm.nodeAuto")}</NativeSelectOption>
            {nodes?.map((n) => (
              <NativeSelectOption key={n.id} value={n.id}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <Field id="vm-restart" label={t("vms.restart")}>
          <NativeSelect
            id="vm-restart"
            className="w-full"
            value={values.restart}
            onChange={(e) => onChange({ restart: e.target.value as VmFormValues["restart"] })}
          >
            <NativeSelectOption value="never">{t("vms.restartPolicy.never")}</NativeSelectOption>
            <NativeSelectOption value="on-failure">
              {t("vms.restartPolicy.on-failure")}
            </NativeSelectOption>
          </NativeSelect>
        </Field>
      </CardContent>
    </Card>
  );
}
```

`web/src/vms/form/NicsSection.tsx`를 만든다:

```tsx
import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { Network } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { MAX_NICS, type NicRow, type VmFormValues } from "@/vms/form";

/** 네트워크 카드 행 */
export function NicsSection({
  values,
  errors,
  onChange,
  onRowsChange,
  networks,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 행을 넣거나 뺀다. 행 번호로 단 오류가 다른 행을 가리키므로 오류도 지운다. */
  onRowsChange: (nics: NicRow[]) => void;
  networks: Network[] | undefined;
}) {
  const { t } = useTranslation();
  const setNic = (i: number, patch: Partial<NicRow>) =>
    onChange({ nics: values.nics.map((n, j) => (j === i ? { ...n, ...patch } : n)) });
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vms.nics")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {values.nics.map((nic, i) => (
          <fieldset
            key={i}
            aria-label={t("vmForm.nicN", { n: i + 1 })}
            className="grid grid-cols-2 items-end gap-2 rounded-md border p-3 sm:grid-cols-4"
          >
            <Field id={`nic-${i}-network`} label={t("vms.network")}>
              <NativeSelect
                id={`nic-${i}-network`}
                className="w-full"
                value={nic.network}
                onChange={(e) => setNic(i, { network: e.target.value })}
              >
                <NativeSelectOption value="user">{t("vmForm.userNetwork")}</NativeSelectOption>
                {networks?.map((n) => (
                  <NativeSelectOption key={n.id} value={n.name}>
                    {n.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-model`} label={t("vms.model")}>
              <NativeSelect
                id={`nic-${i}-model`}
                className="w-full"
                value={nic.model}
                onChange={(e) => setNic(i, { model: e.target.value as NicRow["model"] })}
              >
                <NativeSelectOption value="virtio">virtio</NativeSelectOption>
                <NativeSelectOption value="e1000e">e1000e</NativeSelectOption>
              </NativeSelect>
            </Field>
            <Field id={`nic-${i}-mac`} label={t("vms.mac")} error={errors[`nics.${i}.mac`]}>
              {(a11y) => (
                <Input
                  id={`nic-${i}-mac`}
                  {...a11y}
                  placeholder={t("vmForm.macAuto")}
                  value={nic.mac}
                  onChange={(e) => setNic(i, { mac: e.target.value })}
                />
              )}
            </Field>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              aria-label={t("vmForm.removeNic", { n: i + 1 })}
              onClick={() => onRowsChange(values.nics.filter((_, j) => j !== i))}
            >
              <X />
            </Button>
          </fieldset>
        ))}
        {values.nics.length < MAX_NICS && (
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                onRowsChange([...values.nics, { network: "user", model: "virtio", mac: "" }])
              }
            >
              <Plus />
              {t("vmForm.addNic")}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
```

`web/src/vms/form/ScheduleSection.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import type { Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import type { ScheduleRow, VmFormValues } from "@/vms/form";

/** 예약 스냅샷 */
export function ScheduleSection({
  values,
  errors,
  onChange,
  volumes,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  /** 볼륨 목록. 아직 읽지 못했으면 `undefined`다. */
  volumes: Volume[] | undefined;
}) {
  const { t } = useTranslation();
  const setSchedule = (patch: Partial<ScheduleRow>) =>
    onChange({ snapshotSchedule: { ...values.snapshotSchedule, ...patch } });
  // 예약을 켰는데 붙은 볼륨 중 qcow2가 아닌 것이 있으면 예약 스냅샷이 실패한다. 볼륨 목록을 읽어야 안다.
  // 새로 만드는 디스크는 늘 qcow2다.
  const rawDisk =
    values.snapshotSchedule.every !== "off" && volumes
      ? values.disks
          .filter((d) => d.kind === "existing")
          .map((d) => volumes.find((v) => v.name === d.volume))
          .find((v) => v !== undefined && v.spec.format !== "qcow2")
      : undefined;
  return (
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
              <NativeSelectOption value="hourly">{t("snapshots.every.hourly")}</NativeSelectOption>
              <NativeSelectOption value="daily">{t("snapshots.every.daily")}</NativeSelectOption>
              <NativeSelectOption value="weekly">{t("snapshots.every.weekly")}</NativeSelectOption>
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
  );
}
```

`web/src/vms/form/submit.test.ts`를 만든다:

```ts
import { http, HttpResponse } from "msw";

import { vm } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";
import { fromVm } from "@/vms/form";
import { isConflict, updateVm } from "@/vms/form/submit";

it("sends the read revision as If-Match and reports a conflict", async () => {
  const web = vm("web", "stopped", { labels: { team: "a" } });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(
        { code: "revision_mismatch", message: "expected 1, found 2" },
        { status: 412 },
      );
    }),
  );
  const err: unknown = await updateVm(web, 1, fromVm(web)).catch((e: unknown) => e);
  expect(isConflict(err)).toBe(true);
  expect(isConflict(new Error("boom"))).toBe(false);
  expect(sent).toEqual([
    {
      ifMatch: "1",
      body: expect.objectContaining({ labels: { team: "a" }, spec: expect.anything() }),
    },
  ]);
});
```

`web/src/vms/form/submit.ts`를 만든다:

```ts
import type { QueryClient } from "@tanstack/react-query";

import { ApiError, api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { trackTask, waitForTask } from "@/tasks/tasks";
import { type DiskRow, gibToBytes, toSpec, type VmFormValues } from "@/vms/form";

/** 새 빈 qcow2 볼륨을 만들고 그 Task가 끝날 때까지 기다린다. `label`은 진행 알림 문구다. */
export async function createVolume(
  queryClient: QueryClient,
  disk: DiskRow,
  label: string,
  signal: AbortSignal,
): Promise<void> {
  const task = await call(
    api.POST("/volumes", {
      body: {
        name: disk.volume,
        spec: {
          pool: disk.pool,
          size_bytes: gibToBytes(disk.sizeGib)!,
          format: "qcow2",
        },
      },
    }),
  );
  trackTask(task, label);
  await waitForTask(queryClient, task.id, signal);
}

/** VM을 만든다. `values.start`면 켜진 상태로 만든다. */
export async function createVm(values: VmFormValues): Promise<void> {
  await call(
    api.POST("/vms", {
      body: { name: values.name, spec: toSpec(values, values.start ? "running" : "stopped") },
    }),
  );
}

/** VM spec 전체를 바꾼다. 읽은 `revision`을 If-Match로 보내므로 그 사이 바뀌었으면 실패한다. */
export async function updateVm(
  vm: Vm,
  revision: Vm["revision"] | undefined,
  values: VmFormValues,
): Promise<void> {
  await call(
    api.PUT("/vms/{key}", {
      params: {
        path: { key: vm.id },
        header: { "If-Match": revision },
      },
      body: { labels: vm.labels, spec: toSpec(values, vm.spec.power ?? "stopped") },
    }),
  );
}

/** 다른 사람이 먼저 바꿔 If-Match가 맞지 않았다. */
export function isConflict(err: unknown): boolean {
  return err instanceof ApiError && err.code === "revision_mismatch";
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 254개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "refactor(web): VM 폼을 섹션 컴포넌트와 보내기 함수로 나누기"
```

---

### Task 8: 웹: 공통 번역 키 common.*

같은 뜻의 일반 단어 `cancel`, `save`, `create`, `delete`, `deleteNamed`, `done`, `name`, `state`, `node`를 `common`으로 옮기고(값은 en·ko 그대로), 옛 개별 키(`confirm.cancel`, `users.save`, `vmForm.save`, `tokens.done`, `tasks.node`, `tasks.stateColumn`, `dashboard.state`, 화면별 `create`·`delete`·`deleteNamed`·`name`·`state`·`node`)를 지우고 쓰는 곳을 바꾼다. 뜻이 다른 키(`vms.action.delete`, `snapshots.create` 등)는 그대로다. 테스트: 옮긴 키가 다시 생기지 않음.

**Files:**
- Modify: `web/src/accounts/ChangePasswordDialog.tsx`
- Modify: `web/src/accounts/TokensPage.tsx`
- Modify: `web/src/accounts/UsersPage.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`
- Modify: `web/src/components/ConfirmDialog.tsx`
- Modify: `web/src/components/FormDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/networks/NetworksPage.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/UploadDialog.tsx`
- Modify: `web/src/storage/VolumesPage.tsx`
- Modify: `web/src/storage/upload.ts`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/vms/VmFormPage.tsx`
- Modify: `web/src/vms/VmListPage.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`
- Modify: `web/src/vms/form/GeneralSection.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/accounts/ChangePasswordDialog.tsx`에서

```tsx
      description={t("accounts.changePasswordDescription")}
      submitLabel={t("users.save")}
      returnFocus={returnFocus}
```

을 다음으로 바꾼다:

```tsx
      description={t("accounts.changePasswordDescription")}
      submitLabel={t("common.save")}
      returnFocus={returnFocus}
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
          <Plus />
          {t("tokens.create")}
        </Button>
```

을 다음으로 바꾼다:

```tsx
          <Plus />
          {t("common.create")}
        </Button>
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("tokens.name")}</TableHead>
            {admin && <TableHead>{t("tokens.owner")}</TableHead>}
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {admin && <TableHead>{t("tokens.owner")}</TableHead>}
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
                    size="icon-sm"
                    aria-label={t("tokens.deleteNamed", { name: token.name })}
                    onClick={() => setDeleting(token)}
```

을 다음으로 바꾼다:

```tsx
                    size="icon-sm"
                    aria-label={t("common.deleteNamed", { name: token.name })}
                    onClick={() => setDeleting(token)}
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
        description={t("tokens.deleteDescription")}
        confirmLabel={t("tokens.delete")}
        onConfirm={async () => {
```

을 다음으로 바꾼다:

```tsx
        description={t("tokens.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
      description={t("tokens.createDescription")}
      submitLabel={t("tokens.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="token-name" label={t("tokens.name")} error={error}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
      description={t("tokens.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="token-name" label={t("common.name")} error={error}>
        {(a11y) => (
```

`web/src/accounts/TokensPage.tsx`에서

```tsx
        <DialogFooter>
          <Button onClick={onClose}>{t("tokens.done")}</Button>
        </DialogFooter>
```

을 다음으로 바꾼다:

```tsx
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
```

`web/src/accounts/UsersPage.tsx` 전체:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal, Plus } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { passwordErrors } from "@/accounts/password";
import { PasswordFields } from "@/accounts/PasswordFields";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field, type FieldA11y } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { validName } from "@/vms/form";

type User = components["schemas"]["UserView"];
type Role = components["schemas"]["Role"];

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

/**
 * 사용자 목록. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다. 관리자가 아니면 읽을 수 없으므로
 * `enabled`를 내린다.
 */
export function useUsers(enabled = true) {
  return useQuery({
    queryKey: ["users"],
    queryFn: () => call(api.GET("/users")),
    select: (list) => list.items,
    enabled,
  });
}

type Dialog = { kind: "create" } | { kind: "role" | "password" | "delete"; user: User };

export function UsersPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "write", "user");
  const users = useUsers(admin);
  const queryClient = useQueryClient();
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const close = () => setDialog(null);
  // 메뉴에서 연 대화상자를 닫으면 메뉴 항목이 사라지므로, 메뉴를 연 버튼으로 포커스를 돌린다.
  const trigger = useRef<HTMLButtonElement | null>(null);
  const rememberTrigger = (e: React.SyntheticEvent<HTMLButtonElement>) => {
    trigger.current = e.currentTarget;
  };
  const changed = () => queryClient.invalidateQueries({ queryKey: ["users"] });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. 관리자가 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.users")}</h1>
        <p className="text-sm text-muted-foreground">{t("users.adminOnly")}</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="users-title" className="text-2xl font-semibold">
          {t("nav.users")}
        </h1>
        <Button onClick={() => setDialog({ kind: "create" })}>
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <Table aria-labelledby="users-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("users.role")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(users.data ?? []).length === 0 && (
            <TableMessage colSpan={3} query={users} empty={t("users.empty")} />
          )}
          {(users.data ?? []).map((user) => {
            // 자기 비밀번호는 현재 비밀번호가 필요하므로 계정 메뉴에서 바꾼다. 자기 자신은 지울 수 없다.
            // 자기 역할도 바꾸지 않는다(낮추면 이 화면의 권한이 옛 역할로 남는다).
            const self = user.id === me.user;
            return (
              <TableRow key={user.id}>
                <TableCell className="font-medium">
                  {user.name}
                  {self && (
                    <span className="ml-2 text-xs text-muted-foreground">{t("users.you")}</span>
                  )}
                </TableCell>
                <TableCell>{t(`roles.${user.role}`)}</TableCell>
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: user.name })}
                        onPointerDown={rememberTrigger}
                        onKeyDown={rememberTrigger}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfRole" : null}
                        onSelect={() => setDialog({ kind: "role", user })}
                      >
                        {t("users.changeRole")}
                      </ReasonMenuItem>
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfPassword" : null}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </ReasonMenuItem>
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        reason={self ? "users.reason.selfDelete" : null}
                        onSelect={() => setDialog({ kind: "delete", user })}
                      >
                        {t("common.delete")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      <p className="text-sm text-muted-foreground">{t("users.selfNote")}</p>
      {dialog?.kind === "create" && <CreateUserDialog onClose={close} onDone={changed} />}
      {dialog?.kind === "role" && (
        <RoleDialog user={dialog.user} returnFocus={trigger} onClose={close} onDone={changed} />
      )}
      {dialog?.kind === "password" && (
        <ResetPasswordDialog user={dialog.user} returnFocus={trigger} onClose={close} />
      )}
      <ConfirmDialog
        open={dialog?.kind === "delete"}
        onOpenChange={(open) => !open && close()}
        title={t("users.deleteTitle", { name: dialog?.kind === "delete" ? dialog.user.name : "" })}
        description={t("users.deleteDescription")}
        confirmLabel={t("common.delete")}
        returnFocus={trigger}
        onConfirm={async () => {
          if (dialog?.kind !== "delete") {
            return;
          }
          const { user } = dialog;
          await call(api.DELETE("/users/{key}", { params: { path: { key: user.id } } }));
          toast.success(t("users.deleted", { name: user.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleSelect({
  value,
  onChange,
  a11y,
}: {
  value: Role;
  onChange(role: Role): void;
  a11y: FieldA11y;
}) {
  const { t } = useTranslation();
  return (
    <NativeSelect
      id="user-role"
      className="w-full"
      value={value}
      onChange={(e) => onChange(e.target.value as Role)}
      {...a11y}
    >
      {ROLES.map((role) => (
        <NativeSelectOption key={role} value={role}>
          {t(`roles.${role}`)}
        </NativeSelectOption>
      ))}
    </NativeSelect>
  );
}

function CreateUserDialog({ onClose, onDone }: { onClose(): void; onDone(): Promise<void> }) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    if (!validName(name)) {
      found.name = "forms.name";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.POST("/users", { body: { name, role, password } }));
    toast.success(t("users.created", { name }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.createTitle")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="user-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        {(a11y) => <RoleSelect value={role} onChange={setRole} a11y={a11y} />}
      </Field>
      <PasswordFields
        id="user"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}

function RoleDialog({
  user,
  returnFocus,
  onClose,
  onDone,
}: {
  user: User;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [role, setRole] = useState<Role>(user.role);

  async function submit() {
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { role } }));
    toast.success(t("users.roleChanged", { name: user.name, role: t(`roles.${role}`) }));
    await onDone();
  }

  return (
    <FormDialog
      title={t("users.roleTitle", { name: user.name })}
      submitLabel={t("common.save")}
      submitDisabled={role === user.role}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="user-role" label={t("users.role")} hint={t("users.roleHint")}>
        {(a11y) => <RoleSelect value={role} onChange={setRole} a11y={a11y} />}
      </Field>
    </FormDialog>
  );
}

function ResetPasswordDialog({
  user,
  returnFocus,
  onClose,
}: {
  user: User;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = passwordErrors(password, confirm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(api.PUT("/users/{key}", { params: { path: { key: user.id } }, body: { password } }));
    toast.success(t("users.passwordReset", { name: user.name }));
  }

  return (
    <FormDialog
      title={t("users.passwordTitle", { name: user.name })}
      description={t("users.passwordDescription")}
      submitLabel={t("common.save")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <PasswordFields
        id="reset"
        password={password}
        confirm={confirm}
        errors={errors}
        onPassword={setPassword}
        onConfirm={setConfirm}
      />
    </FormDialog>
  );
}
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
            <TableRow>
              <TableHead>{t("nodes.name")}</TableHead>
              <TableHead>{t("nodes.state")}</TableHead>
              <TableHead>{t("cluster.role")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("cluster.role")}</TableHead>
```

`web/src/cluster/ClusterPage.tsx`에서

```tsx
        <DialogFooter>
          <Button onClick={onClose}>{t("tokens.done")}</Button>
        </DialogFooter>
```

을 다음으로 바꾼다:

```tsx
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
```

`web/src/components/ConfirmDialog.tsx`에서

```tsx
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>{t("confirm.cancel")}</AlertDialogCancel>
          <Button
```

을 다음으로 바꾼다:

```tsx
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>{t("common.cancel")}</AlertDialogCancel>
          <Button
```

`web/src/components/FormDialog.tsx`에서

```tsx
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("confirm.cancel")}
            </Button>
```

을 다음으로 바꾼다:

```tsx
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.cancel")}
            </Button>
```

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
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2 can take snapshots.",
    "vm_has_snapshots": "The VM has snapshots. Delete them first.",
    "volume_in_snapshot": "A VM snapshot still uses this volume. Delete that snapshot first.",
    "snapshot_not_ready": "The snapshot is still being taken.",
    "node_offline": "The node holding this VM is offline.",
    "too_many_attempts": "Too many attempts. Try again later.",
    "machine_arch_mismatch": "The machine type doesn't match the selected node's architecture.",
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
      "vm-clone": "Clone {{target}}"
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
      "busy": "The VM is starting or stopping."
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
      "failed": "Failed"
    },
    "action": {
      "start": "Start",
      "stop": "Stop",
      "force-stop": "Force stop",
      "reboot": "Reboot",
      "console": "Console",
      "delete": "Delete",
      "edit": "Edit",
      "clone": "Clone"
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
    "unsupported": "Only VMs whose disks are all qcow2 can take snapshots. {{volume}} is {{format}}.",
    "busy": "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
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
    "createDescription": "A directory on the nodes that holds volume files. Create the directory on each node first.",
    "pathHint": "An absolute path on the nodes.",
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory stay on the nodes.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "errors": {
      "path": "Enter an absolute path (starting with /).",
      "nodesGone": "The chosen nodes are no longer in the cluster. Choose again."
    }
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
    "scheduleNote": "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2.",
    "scheduleEvery": "Take snapshots",
    "scheduleOff": "Off",
    "scheduleKeep": "Snapshots to keep",
    "scheduleRaw": "{{volume}} is {{format}}, so scheduled snapshots will fail. All disks must be qcow2.",
    "scheduleMemoryHint": "Memory is saved only while the VM is running and has disks."
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

`web/src/i18n/i18n.test.ts`에서

```ts
  expect(keys(ko).sort()).toEqual(keys(en).sort());
});
```

을 다음으로 바꾼다:

```ts
  expect(keys(ko).sort()).toEqual(keys(en).sort());
});

// `common.*`으로 옮긴 키. 같은 뜻의 개별 키를 다시 만들지 않는다.
const MOVED_TO_COMMON = [
  "confirm.cancel",
  "users.save",
  "vmForm.save",
  "tokens.done",
  "tasks.node",
  "tasks.stateColumn",
  "dashboard.state",
  ...["vms", "volumes", "pools", "networks", "users", "tokens"].map((s) => `${s}.create`),
  ...["snapshots", "volumes", "pools", "networks", "users", "tokens"].map((s) => `${s}.delete`),
  ...["volumes", "pools", "networks", "tokens"].map((s) => `${s}.deleteNamed`),
  ...["vms", "snapshots", "volumes", "pools", "networks", "nodes", "users", "tokens"].map(
    (s) => `${s}.name`,
  ),
  ...["vms", "volumes", "nodes"].map((s) => `${s}.state`),
  ...["vms", "volumes"].map((s) => `${s}.node`),
];

it("keeps common words only under common", () => {
  for (const language of [en, ko]) {
    const present = keys(language);
    expect(MOVED_TO_COMMON.filter((key) => present.includes(key))).toEqual([]);
  }
  expect(en.common).toMatchObject({ cancel: "Cancel", deleteNamed: "Delete {{name}}" });
  expect(ko.common).toMatchObject({ cancel: "취소", deleteNamed: "{{name}} 삭제" });
});
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
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제)이 진행 중입니다. 끝나면 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다.",
    "vm_has_snapshots": "VM에 스냅샷이 있습니다. 먼저 스냅샷을 지우세요.",
    "volume_in_snapshot": "VM 스냅샷이 아직 이 볼륨을 씁니다. 먼저 그 스냅샷을 지우세요.",
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "too_many_attempts": "시도가 너무 많습니다. 잠시 뒤에 다시 하세요.",
    "machine_arch_mismatch": "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다.",
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
      "vm-clone": "{{target}} 복제"
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
      "busy": "VM이 시작하거나 정지하는 중입니다."
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
      "failed": "실패"
    },
    "action": {
      "start": "시작",
      "stop": "정지",
      "force-stop": "강제 정지",
      "reboot": "재시작",
      "console": "콘솔",
      "delete": "삭제",
      "edit": "편집",
      "clone": "복제"
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
    "unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다. {{volume}}은(는) {{format}}입니다.",
    "busy": "VM이 시작하거나 정지하는 중입니다. 끝나면 스냅샷을 만들고 되돌리고 지울 수 있습니다.",
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
    "createDescription": "볼륨 파일을 두는 노드의 디렉터리입니다. 먼저 각 노드에 디렉터리를 만드세요.",
    "pathHint": "노드의 절대 경로입니다.",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리의 파일은 노드에 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "errors": {
      "path": "절대 경로(/로 시작)를 입력하세요.",
      "nodesGone": "고른 노드가 더 이상 클러스터에 없습니다. 다시 고르세요."
    }
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
    "scheduleNote": "주기마다 스냅샷을 만들고 예약으로 만든 것은 최근 것만 남깁니다. 직접 만든 스냅샷은 지우지 않습니다. 디스크가 모두 qcow2여야 합니다.",
    "scheduleEvery": "스냅샷 주기",
    "scheduleOff": "끔",
    "scheduleKeep": "보관 개수",
    "scheduleRaw": "{{volume}}이(가) {{format}}이라 예약 스냅샷이 실패합니다. 디스크가 모두 qcow2여야 합니다.",
    "scheduleMemoryHint": "VM이 실행 중이고 디스크가 있을 때만 메모리도 저장합니다."
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

`web/src/networks/NetworksPage.tsx`에서

```tsx
            <Plus />
            {t("networks.create")}
          </Button>
```

을 다음으로 바꾼다:

```tsx
            <Plus />
            {t("common.create")}
          </Button>
```

`web/src/networks/NetworksPage.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("networks.name")}</TableHead>
            <TableHead>{t("networks.bridge")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("networks.bridge")}</TableHead>
```

`web/src/networks/NetworksPage.tsx`에서

```tsx
                      size="icon-sm"
                      aria-label={t("networks.deleteNamed", { name: network.name })}
                      // VM이 쓰는 네트워크는 서버가 지우지 않는다.
```

을 다음으로 바꾼다:

```tsx
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: network.name })}
                      // VM이 쓰는 네트워크는 서버가 지우지 않는다.
```

`web/src/networks/NetworksPage.tsx`에서

```tsx
        description={t("networks.deleteDescription")}
        confirmLabel={t("networks.delete")}
        onConfirm={async () => {
```

을 다음으로 바꾼다:

```tsx
        description={t("networks.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
```

`web/src/networks/NetworksPage.tsx`에서

```tsx
      description={t("networks.createDescription")}
      submitLabel={t("networks.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="network-name" label={t("networks.name")} error={errors.name}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
      description={t("networks.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="network-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("nodes.name")}</TableHead>
            <TableHead>{t("nodes.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
            <TableRow>
              <TableHead>{t("vms.node")}</TableHead>
              <TableHead>{t("dashboard.state")}</TableHead>
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableRow>
              <TableHead>{t("common.node")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("dashboard.hypervisor")}</TableHead>
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
            <Plus />
            {t("pools.create")}
          </Button>
```

을 다음으로 바꾼다:

```tsx
            <Plus />
            {t("common.create")}
          </Button>
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("pools.name")}</TableHead>
            <TableHead>{t("pools.path")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("pools.path")}</TableHead>
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
                      size="icon-sm"
                      aria-label={t("pools.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
```

을 다음으로 바꾼다:

```tsx
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
        description={t("pools.deleteDescription")}
        confirmLabel={t("pools.delete")}
        onConfirm={async () => {
```

을 다음으로 바꾼다:

```tsx
        description={t("pools.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
      description={t("pools.createDescription")}
      submitLabel={t("pools.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("pools.name")} error={errors.name}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
      description={t("pools.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
```

`web/src/storage/UploadDialog.tsx`에서

```tsx
      </Field>
      <Field id="upload-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
      </Field>
      <Field id="upload-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
              <Plus />
              {t("volumes.create")}
            </Button>
```

을 다음으로 바꾼다:

```tsx
              <Plus />
              {t("common.create")}
            </Button>
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("volumes.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("volumes.state")}</TableHead>
            <TableHead>{t("volumes.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
                      size="icon-sm"
                      aria-label={t("volumes.deleteNamed", { name: volume.name })}
                      reason={deleteReason}
```

을 다음으로 바꾼다:

```tsx
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: volume.name })}
                      reason={deleteReason}
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
        description={t("volumes.deleteDescription")}
        confirmLabel={t("volumes.delete")}
        typeToConfirm={deleting?.name}
```

을 다음으로 바꾼다:

```tsx
        description={t("volumes.deleteDescription")}
        confirmLabel={t("common.delete")}
        typeToConfirm={deleting?.name}
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
      description={t("volumes.createDescription")}
      submitLabel={t("volumes.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
      description={t("volumes.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
```

`web/src/storage/upload.ts`에서

```ts
  const controller = new AbortController();
  const cancel = { label: t("confirm.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
```

을 다음으로 바꾼다:

```ts
  const controller = new AbortController();
  const cancel = { label: t("common.cancel"), onClick: () => controller.abort() };
  if (active.size === 0) {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
          <TableHead>{t("tasks.operation")}</TableHead>
          <TableHead>{t("tasks.node")}</TableHead>
          <TableHead>{t("tasks.stateColumn")}</TableHead>
          <TableHead>{t("tasks.error")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableHead>{t("tasks.operation")}</TableHead>
          <TableHead>{t("common.node")}</TableHead>
          <TableHead>{t("common.state")}</TableHead>
          <TableHead>{t("tasks.error")}</TableHead>
```

`web/src/vms/VmFormPage.tsx`에서

```tsx
        <Button type="submit" disabled={busy}>
          {creating ? t("vms.create") : t("vmForm.save")}
        </Button>
        <Button variant="ghost" asChild>
          <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
            {t("confirm.cancel")}
          </Link>
```

을 다음으로 바꾼다:

```tsx
        <Button type="submit" disabled={busy}>
          {creating ? t("common.create") : t("common.save")}
        </Button>
        <Button variant="ghost" asChild>
          <Link to={creating ? "/vms" : `/vms/${encodeURIComponent(vm.name)}`}>
            {t("common.cancel")}
          </Link>
```

`web/src/vms/VmListPage.tsx`에서

```tsx
        accessorKey: "name",
        header: t("vms.name"),
        cell: ({ row }) => (
```

을 다음으로 바꾼다:

```tsx
        accessorKey: "name",
        header: t("common.name"),
        cell: ({ row }) => (
```

`web/src/vms/VmListPage.tsx`에서

```tsx
        accessorFn: (vm) => vm.status.phase,
        header: t("vms.state"),
        cell: ({ row }) => <VmPhaseBadge vm={row.original} />,
```

을 다음으로 바꾼다:

```tsx
        accessorFn: (vm) => vm.status.phase,
        header: t("common.state"),
        cell: ({ row }) => <VmPhaseBadge vm={row.original} />,
```

`web/src/vms/VmListPage.tsx`에서

```tsx
        accessorFn: (vm) => nodeName(vm.spec.node ?? vm.status.node),
        header: t("vms.node"),
      },
```

을 다음으로 바꾼다:

```tsx
        accessorFn: (vm) => nodeName(vm.spec.node ?? vm.status.node),
        header: t("common.node"),
      },
```

`web/src/vms/VmListPage.tsx`에서

```tsx
                <Plus />
                {t("vms.create")}
              </Link>
```

을 다음으로 바꾼다:

```tsx
                <Plus />
                {t("common.create")}
              </Link>
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
          <TableRow>
            <TableHead>{t("snapshots.name")}</TableHead>
            <TableHead>{t("snapshots.created")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("snapshots.created")}</TableHead>
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
                    >
                      {t("snapshots.delete")}
                    </Button>
```

을 다음으로 바꾼다:

```tsx
                    >
                      {t("common.delete")}
                    </Button>
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
        description={t("snapshots.deleteDescription")}
        confirmLabel={t("snapshots.delete")}
        onConfirm={async () => {
```

을 다음으로 바꾼다:

```tsx
        description={t("snapshots.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
    >
      <Field id="snapshot-name" label={t("snapshots.name")} error={errors.name}>
        {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
    >
      <Field id="snapshot-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
        {creating && (
          <Field id="vm-name" label={t("vms.name")} error={errors.name}>
            {(a11y) => (
```

을 다음으로 바꾼다:

```tsx
        {creating && (
          <Field id="vm-name" label={t("common.name")} error={errors.name}>
            {(a11y) => (
```

`web/src/vms/form/GeneralSection.tsx`에서

```tsx
        </Field>
        <Field id="vm-node" label={t("vms.node")}>
          <NativeSelect
```

을 다음으로 바꾼다:

```tsx
        </Field>
        <Field id="vm-node" label={t("common.node")}>
          <NativeSelect
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 255개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "refactor(web): 같은 뜻의 일반 단어 번역 키를 common.*으로 모으기"
```

---

### Task 9: 웹: 비밀값 대화상자 뒤 포커스

토큰 화면의 "만들기" 버튼과 클러스터 화면의 "참가 토큰 발급" 버튼에 `useRef`를 달아, 만들기·발급 `FormDialog`와 `SecretDialog`·`JoinTokenDialog`(필수 `returnFocus`, `onCloseAutoFocus`에서 `focusBack`)에 넘긴다. 테스트: 완료와 취소 뒤 포커스가 여는 버튼.

**Files:**
- Modify: `web/src/accounts/TokensPage.test.tsx`
- Modify: `web/src/accounts/TokensPage.tsx`
- Modify: `web/src/cluster/ClusterPage.test.tsx`
- Modify: `web/src/cluster/ClusterPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/accounts/TokensPage.test.tsx`에서

```tsx
  expect(await screen.findByRole("cell", { name: "backup" })).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByRole("cell", { name: "backup" })).toBeInTheDocument();
  // 비밀값 대화상자를 닫으면 연 버튼으로 돌아간다.
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Create" })).toHaveFocus());
});

it("returns focus to the create button after cancelling", async () => {
  const user = userEvent.setup();
  tokens([]);
  renderPage(<TokensPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const form = await screen.findByRole("dialog", { name: "Create an API token" });
  await user.click(within(form).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(form).not.toBeInTheDocument());
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Create" })).toHaveFocus());
});
```

`web/src/accounts/TokensPage.tsx` 전체:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useUsers } from "@/accounts/UsersPage";
import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatTime, uuidTime } from "@/lib/format";
import { validName } from "@/vms/form";

type Token = components["schemas"]["TokenView"];
type CreatedToken = components["schemas"]["CreatedToken"];

/**
 * API 토큰. 누구나 자기 토큰을 만들고 지운다(토큰의 권한은 주인의 역할을 따른다). 관리자는 모든 토큰을
 * 보고 지울 수 있다. 감시할 수 없으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function TokensPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const tokens = useQuery({
    queryKey: ["tokens"],
    queryFn: () => call(api.GET("/tokens")),
    select: (list) => list.items,
  });
  // 남의 토큰을 다루는 것은 관리자뿐이다(서버의 may_manage와 같은 규칙).
  const admin = can(me.role, "write", "api-token");
  const users = useUsers(admin);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [deleting, setDeleting] = useState<Token | null>(null);
  // 만들기·비밀값 대화상자를 닫으면 "만들기" 버튼으로 돌아간다.
  const opener = useRef<HTMLButtonElement>(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["tokens"] });
  const userName = useMemo(() => new Map(users.data?.map((u) => [u.id, u.name])), [users.data]);
  const owner = (token: Token) =>
    token.user === me.user ? me.name : (userName.get(token.user) ?? token.user.slice(-8));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="tokens-title" className="text-2xl font-semibold">
          {t("nav.tokens")}
        </h1>
        <Button ref={opener} onClick={() => setCreating(true)}>
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("tokens.note")}</p>
      <Table aria-labelledby="tokens-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {admin && <TableHead>{t("tokens.owner")}</TableHead>}
            <TableHead>{t("tokens.created")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {(tokens.data ?? []).length === 0 && (
            <TableMessage colSpan={admin ? 4 : 3} query={tokens} empty={t("tokens.empty")} />
          )}
          {(tokens.data ?? []).map((token) => {
            const time = uuidTime(token.id);
            return (
              <TableRow key={token.id}>
                <TableCell className="font-medium">{token.name}</TableCell>
                {admin && <TableCell>{owner(token)}</TableCell>}
                <TableCell className="whitespace-nowrap">
                  <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("common.deleteNamed", { name: token.name })}
                    onClick={() => setDeleting(token)}
                  >
                    <Trash2 />
                  </Button>
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && (
        <CreateTokenDialog
          returnFocus={opener}
          onClose={() => setCreating(false)}
          onCreated={(token) => {
            setCreated(token);
            void changed();
          }}
        />
      )}
      {created && (
        <SecretDialog created={created} returnFocus={opener} onClose={() => setCreated(null)} />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("tokens.deleteTitle", { name: deleting?.name })}
        description={t("tokens.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const token = deleting!;
          await call(api.DELETE("/tokens/{key}", { params: { path: { key: token.id } } }));
          toast.success(t("tokens.deleted", { name: token.name }));
          await changed();
        }}
      />
    </div>
  );
}

function CreateTokenDialog({
  returnFocus,
  onClose,
  onCreated,
}: {
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onCreated(token: CreatedToken): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [error, setError] = useState<string | undefined>();

  async function submit() {
    const invalid = validName(name) ? undefined : "forms.name";
    setError(invalid);
    if (invalid) {
      return false;
    }
    onCreated(await call(api.POST("/tokens", { body: { name } })));
  }

  return (
    <FormDialog
      title={t("tokens.createTitle")}
      description={t("tokens.createDescription")}
      submitLabel={t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="token-name" label={t("common.name")} error={error}>
        {(a11y) => (
          <Input
            id="token-name"
            autoComplete="off"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 만든 토큰의 비밀값. 서버는 다시 보여 주지 않으므로 "완료"로만 닫는다. */
function SecretDialog({
  created,
  returnFocus,
  onClose,
}: {
  created: CreatedToken;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onInteractOutside={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        <DialogHeader>
          <DialogTitle>{t("tokens.secretTitle", { name: created.token.name })}</DialogTitle>
          <DialogDescription>{t("tokens.secretDescription")}</DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("tokens.secret")}
            className="font-mono text-xs"
            value={created.secret}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={created.secret} label={t("tokens.copySecret")} />
        </div>
        <p className="text-xs text-muted-foreground">
          {t("tokens.usage")} <code>Authorization: Bearer …</code>
        </p>
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
```

`web/src/cluster/ClusterPage.test.tsx`에서

```tsx
  await vi.waitFor(() => expect(dialog).not.toBeInTheDocument());
});
```

을 다음으로 바꾼다:

```tsx
  await vi.waitFor(() => expect(dialog).not.toBeInTheDocument());
  // 명령 대화상자를 닫으면 연 버튼으로 돌아간다.
  await vi.waitFor(() =>
    expect(screen.getByRole("button", { name: "Issue a join token" })).toHaveFocus(),
  );
});

it("returns focus to the issue button after cancelling", async () => {
  const user = userEvent.setup();
  renderPage(<ClusterPage />);
  await user.click(await screen.findByRole("button", { name: "Issue a join token" }));
  const form = await screen.findByRole("dialog", { name: "Issue a join token" });
  await user.click(within(form).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(form).not.toBeInTheDocument());
  await vi.waitFor(() =>
    expect(screen.getByRole("button", { name: "Issue a join token" })).toHaveFocus(),
  );
});
```

`web/src/cluster/ClusterPage.tsx` 전체:

```tsx
import { useQuery } from "@tanstack/react-query";
import { Download, Plus } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";

type JoinToken = components["schemas"]["CreatedJoinToken"];

/** 조인 토큰의 유효 시간(초). 서버의 기본은 1시간, 최대는 7일이다. */
const TTLS = [3600, 86400, 604800] as const;

/** 새 노드에서 실행할 명령. URL에 `?`와 `&`가 있어 작은따옴표로 감싸고, 안의 `'`는 `'\''`로 바꾼다. */
export function joinCommand(url: string): string {
  return `oplctl cluster join '${url.replaceAll("'", "'\\''")}'`;
}

export function ClusterPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  // 멤버·리더·온라인은 감시할 수 없으므로 이 화면에서만 주기적으로 다시 읽는다.
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    refetchInterval: 10_000,
  });
  const [issuing, setIssuing] = useState(false);
  const [issued, setIssued] = useState<JoinToken | null>(null);
  // 발급·명령 대화상자를 닫으면 "참가 토큰 발급" 버튼으로 돌아간다.
  const opener = useRef<HTMLButtonElement>(null);

  if (!cluster.data) {
    return <ListPending error={cluster.error} />;
  }
  const { name, ca_fingerprint: fingerprint, leader, node: here, members } = cluster.data;
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-semibold">{t("cluster.title", { name })}</h1>
        {can(me.role, "write", "join-token") && (
          <Button ref={opener} onClick={() => setIssuing(true)}>
            <Plus />
            {t("cluster.issue")}
          </Button>
        )}
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {cluster.isError && (
        <p role="alert" className="text-sm text-destructive">
          {t("cluster.refreshFailed", { error: errorMessage(t, cluster.error) })}
        </p>
      )}
      <section className="flex flex-col gap-2">
        <h2 className="font-medium">{t("cluster.ca")}</h2>
        <p className="text-sm text-muted-foreground">{t("cluster.caNote")}</p>
        <div className="flex flex-wrap items-center gap-2">
          <code className="rounded-md bg-muted px-2 py-1 text-xs break-all">{fingerprint}</code>
          <CopyButton text={fingerprint} label={t("cluster.copyFingerprint")} />
          <Button variant="outline" size="sm" asChild>
            <a href="/api/v1/cluster/ca" download="opl-ca.pem">
              <Download />
              {t("cluster.downloadCa")}
            </a>
          </Button>
        </div>
      </section>
      <section className="flex flex-col gap-2">
        <h2 id="cluster-members" className="font-medium">
          {t("cluster.members")}
        </h2>
        <Table aria-labelledby="cluster-members">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("cluster.role")}</TableHead>
              <TableHead>{t("nodes.address")}</TableHead>
              <TableHead>{t("cluster.clusterAddress")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {members.map((m) => (
              <TableRow key={m.id}>
                <TableCell className="font-medium">
                  {m.name ?? m.id.slice(-8)}
                  {m.id === here && (
                    <span className="ml-2 text-xs text-muted-foreground">
                      {t("nodes.thisNode")}
                    </span>
                  )}
                </TableCell>
                <TableCell>
                  <Badge variant={m.online ? "default" : "destructive"}>
                    {t(m.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                </TableCell>
                <TableCell>
                  {m.id === leader
                    ? t("cluster.leader")
                    : t(m.voter ? "cluster.voter" : "cluster.learner")}
                </TableCell>
                <TableCell className="font-mono text-xs">{m.address ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{m.cluster_address}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        <p className="text-sm text-muted-foreground">{t("cluster.membersNote")}</p>
      </section>
      {issuing && (
        <IssueDialog
          returnFocus={opener}
          onClose={() => setIssuing(false)}
          onIssued={(token) => setIssued(token)}
        />
      )}
      {issued && (
        <JoinTokenDialog token={issued} returnFocus={opener} onClose={() => setIssued(null)} />
      )}
    </div>
  );
}

function IssueDialog({
  returnFocus,
  onClose,
  onIssued,
}: {
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onIssued(token: JoinToken): void;
}) {
  const { t } = useTranslation();
  const [ttl, setTtl] = useState<number>(TTLS[0]);

  async function submit() {
    onIssued(await call(api.POST("/cluster/join-tokens", { body: { ttl_secs: ttl } })));
  }

  return (
    <FormDialog
      title={t("cluster.issueTitle")}
      description={t("cluster.issueDescription")}
      submitLabel={t("cluster.issue")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="join-ttl" label={t("cluster.ttl")}>
        <NativeSelect
          id="join-ttl"
          className="w-full"
          value={ttl}
          onChange={(e) => setTtl(Number(e.target.value))}
        >
          {TTLS.map((secs) => (
            <NativeSelectOption key={secs} value={secs}>
              {t(`cluster.ttlOption.${secs}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
    </FormDialog>
  );
}

/** 발급한 조인 URL. 서버는 다시 보여 주지 않으므로 "완료"로만 닫는다. */
function JoinTokenDialog({
  token,
  returnFocus,
  onClose,
}: {
  token: JoinToken;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t, i18n } = useTranslation();
  const command = joinCommand(token.url);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        showCloseButton={false}
        onEscapeKeyDown={(e) => e.preventDefault()}
        onInteractOutside={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        <DialogHeader>
          <DialogTitle>{t("cluster.issuedTitle")}</DialogTitle>
          <DialogDescription>
            {t("cluster.issuedDescription", {
              time: formatTime(new Date(token.expires_at * 1000), i18n.language),
            })}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            readOnly
            aria-label={t("cluster.command")}
            className="font-mono text-xs"
            value={command}
            onFocus={(e) => e.target.select()}
          />
          <CopyButton text={command} label={t("cluster.copyCommand")} />
        </div>
        <p className="text-xs text-muted-foreground">{t("cluster.commandNote")}</p>
        <DialogFooter>
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 257개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "fix(web): 비밀값 대화상자와 만들기 대화상자를 닫으면 연 버튼으로 포커스 돌려주기"
```

---

### Task 10: 문서

README의 opld 설정 예시에 `[janitor]`의 `task_max_age_days`·`task_max_finished`(주석), 웹 UI 절에 Task 보존과 50개씩 페이지, 막힌 동작의 이유와 웹에서만 막는 전원 버튼, `index.html`의 304를 적고 문서 목록에 설계를 더한다. 설계 문서 §2–§10에 구현하며 정한 것을 적는다.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-28-web-cleanup-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
# systemd_scope = true
TOML
```

을 다음으로 바꾼다:

```markdown
# systemd_scope = true
# [janitor]
# task_max_age_days = 7     # 끝난 작업(Task)을 남기는 날수(만든 때부터)
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
TOML
```

`README.md`에서

```markdown
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다.
```

을 다음으로 바꾼다:

```markdown
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다.

- **작업 보존.** 끝난(성공·실패) 작업은 만든 지 7일이 넘으면 지워지고, 그래도 1000개를 넘으면 오래된 것부터
  지워진다(opld 설정 `[janitor]`의 `task_max_age_days`·`task_max_finished`, 리더 노드가 지운다). 처리 중인 작업은
  지우지 않는다. 작업 화면과 VM 상세의 작업 탭은 50개씩 페이지로 나눠 보여 준다.
- **막힌 동작과 이유.** 할 수 없는 메뉴 항목·버튼은 숨기지 않고 막힌 채로 이유를 보인다(예: "VM이 정지되어
  있습니다.", "내 계정은 다른 관리자가 삭제합니다."). 키보드로 닿고 화면 읽기 프로그램이 이유를 읽는다. VM에 처리
  중인 시작·정지·재시작 작업이 있으면 웹에서는 시작·정지·재시작이 막힌다(강제 정지는 된다. CLI·API에서는 새 요청이
  앞 작업을 대체한다).
- **캐시.** 웹 파일에는 내용 해시로 만든 ETag가 붙는다. `index.html`은 매번 다시 확인하지만 바뀌지 않았으면
  **304**로 본문 없이 끝난다.
```

`README.md`에서

```markdown
- [API·보안 보완 설계 (계획 13)](docs/superpowers/specs/2026-10-28-api-security-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [API·보안 보완 설계 (계획 13)](docs/superpowers/specs/2026-10-28-api-security-design.md)
- [웹 정리 설계 (계획 14)](docs/superpowers/specs/2026-10-28-web-cleanup-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-28-web-cleanup-design.md` 전체:

```markdown
# opLite 웹 정리 설계 (계획 14)

작성일: 2026-10-28
상위 문서: `2026-10-23-web-ui-design.md`, `2026-10-27-robustness-design.md`

## 1. 목표와 범위

계획 8·9·12에서 넘긴 웹 정리 사항을 마친다. 작업 목록이 끝없이 커지는 문제는 서버에서 함께 다룬다.

| 항목 | 내용 |
|---|---|
| 작업 보존 | janitor가 끝난 Task를 기간·개수 기준으로 지운다 |
| `index.html` ETag | 내장 웹 파일에 ETag를 붙이고 `If-None-Match`에 304로 답한다 |
| 작업 목록 페이지 | 작업 화면과 VM 작업 탭을 50개씩 나눠 보여 준다 |
| 행 비용 | Task 색인을 한 번 만들고, 행마다 목록을 훑지 않는다 |
| 전원 버튼 | 처리 중인 전원 Task가 있으면 시작·정지·재시작을 막는다 |
| 비활성 이유 | 막힌 메뉴 항목·버튼이 이유를 보여 주고 화면 읽기 프로그램이 읽는다 |
| VM 폼 | `VmFormPage.tsx`를 섹션 컴포넌트로 나눈다 |
| 번역 키 | 일반 단어를 `common.*`로 모은다 |
| 포커스 | 비밀값 대화상자를 닫으면 연 버튼으로 포커스를 돌린다 |

넣지 않는 것: `GET /tasks`의 서버 페이지 나누기(cursor·limit)와 Task를 감시 스트림에서 빼기, Task의 끝난 시각 필드, 서버에서 처리 중인 전원 Task로 요청 거절(새 전원 Task가 앞 것을 대체하는 동작은 그대로다). 공유 스토리지는 그 뒤다.

## 2. 작업 보존

리더의 `ClusterJanitor`가 `tick`마다 끝난(`succeeded`·`failed`) Task를 지운다.

- **기간.** 만든 지 `task_max_age`(기본 7일)가 넘은(같으면 남긴다) 끝난 Task를 지운다. 만든 시각은 UUIDv7 id의 시각이다(Task id는 UUIDv7로 만든다). 시각을 담지 않은 id는 가장 오래된 것으로 본다. 만든 시각이 같으면 id 순서로 줄 세운다. Task에 끝난 시각이 없으므로, 오래 걸린 Task는 끝난 뒤 7일보다 일찍 지워질 수 있다. 7일이면 실제로는 문제가 없다고 본다.
- **개수.** 기간으로 지우고도 끝난 Task가 `task_max_finished`(기본 1000)개를 넘으면, 넘는 만큼 만든 시각이 오래된 것부터 지운다. 끝나지 않은 Task는 세지 않는다. 구현은 끝난 Task를 오래된 것부터 줄 세우고 앞에서 기간이 넘은 것과, 개수를 넘은 수 안에서 아래 유예가 지난 것을 고른다(`janitor.rs`의 `expired_tasks`).
- **개수 기준의 유예.** 개수 기준으로는 리더가 끝난 것을 처음 본 뒤 `finished_grace`(10분)가 지난 Task만 지운다. 막 끝난 Task를 기다리는 oplctl `wait_task`·웹 `waitForTask`가 결과를 읽기 전에 Task가 지워지지 않게 한다. 처음 본 시각은 고아 스냅샷처럼 리더의 메모리(`Seen`)에만 두고, 지워졌거나 없어진 Task는 잊는다. 리더가 바뀌면 기억이 비므로 새 리더는 유예 동안 개수 기준으로 지우지 않는다. 기간 기준은 유예를 기다리지 않는다(Task 시간 제한이 7일보다 훨씬 짧다). `finished_grace`는 테스트를 위해 `ClusterJanitorConfig` 필드로 두고 opld 설정에는 내놓지 않는다.
- **끝나지 않은 Task**(`pending`·`running`)는 지우지 않는다. 오래 걸려 멈춘 Task는 기존 janitor 규칙이 실패로 끝낸 뒤 이 규칙의 대상이 된다.
- **tick 상한.** 한 tick에 최대 200개를 지운다(상수). 쌓여 있던 Task가 많아도 Raft 쓰기가 한꺼번에 몰리지 않고, 나머지는 다음 tick에 지운다. 지울 순서는 오래된 것부터다.
- **삭제 실패.** 거부(그사이 바뀜, 이미 지워짐)는 로그만 남기고 다음 Task로 넘어간다. 거부가 아닌 쓰기 오류(시간 초과, 리더 아님, 리더에 닿지 못함)면 클러스터가 쓰기를 받지 못하는 것이므로 이 tick의 남은 삭제를 건너뛰고(로그 한 줄) 다음 tick에 다시 판단한다.
- **설정.** opld 설정의 `[janitor]`에 `task_max_age_days`(기본 7)와 `task_max_finished`(기본 1000)를 더하고, `ClusterJanitorConfig`의 `task_max_age`·`task_max_finished`로 넘긴다. 0은 거절한다(opld가 시작하지 않는다). 0이면 끝난 Task가 곧바로 모두 지워져 작업 결과를 볼 수 없다. `task_max_age_days`는 36500(100년)을 넘어도 거절한다(초로 바꿀 때 넘치지 않게).
- 지우면 감시 스트림에 삭제 이벤트가 가므로 웹 캐시에서도 빠진다. 웹에는 Task 하나를 가리키는 링크나 화면이 없으므로(VM의 마지막 오류는 문구만 보인다) 지워진 Task 때문에 고칠 곳은 없다.

## 3. `index.html` ETag

`crates/opld/src/web.rs`의 `file_response`.

- 내장 파일마다 rust-embed가 계산한 sha256(`file.metadata.sha256_hash()`)을 16진수로 바꿔 강한 ETag `"<hex>"`를 모든 응답(assets, `index.html`, SPA 폴백)에 붙인다.
- 요청의 `If-None-Match`가 그 ETag와 맞으면(쉼표로 나열된 값 가운데 하나가 맞거나 `*`이면) 본문 없이 **304**를 돌려준다. `If-None-Match`는 RFC 9110의 약한 비교라 각 값의 앞뒤 공백과 앞의 `W/`를 떼고 ETag와 같은 문자열인지 본다(`W/"<hex>"`도 304). 304에도 `ETag`, `Cache-Control`, 보안 헤더(`secure()`)를 같은 값으로 붙인다.
- `Cache-Control`은 그대로다: `assets/*`는 `public, max-age=31536000, immutable`, 그 밖은 `no-cache`. 그래서 `index.html`의 재확인이 304로 끝난다.
- 디버그 빌드(디스크에서 읽음)에서도 rust-embed가 해시를 계산하므로 같은 동작이다.

## 4. Task 색인과 행 비용

- `web/src/api/taskIndex.tsx`: `TaskIndexProvider`가 `useList("task")`를 한 번 읽어 `useMemo`로 색인을 만든다. 색인은 대상 id → 끝나지 않은 Task 목록(`Map<string, Task[]>`)이다. `useTaskIndex()`가 색인을 돌려준다. `LiveProvider` 안쪽, 화면들 바깥에 둔다.
- `VmMenu`는 행마다 `useList("task")`를 부르지 않고 `useTaskIndex()`에서 자기 VM 것만 꺼낸다(`useTaskIndex().get(vm.id) ?? []`). `isReverting`은 VM의 Task 목록을 받는다(대상은 다시 확인하지 않는다).
- Provider 바깥이나 Task 목록을 아직 모를 때 색인은 빈 `Map`이다. 그때는 Task 때문에는 막지 않고 서버에 맡긴다. 테스트 렌더 도우미도 화면 바깥에 Provider를 두고, 테스트 서버는 `GET /tasks`에 기본으로 빈 목록을 답한다.
- 행마다 `find`로 하던 조회는 화면마다 한 번 만든 `Map`(id → 리소스)으로 바꾼다: `TasksPage`(VM·볼륨·노드), `VolumesPage`(볼륨 id → 붙은 VM), `TokensPage`, `PoolsPage`. VM 상세의 작업 탭은 `useMemo`로 걸러 둔다.

## 5. 전원 버튼

- `web/src/vms/actions.ts`의 `canRun(vm, action)`을 `availability(vm, action, vmTasks)`로 바꾼다. 돌려주는 값은 `{ ok: true }` 또는 `{ ok: false, reason }`이고, `reason`은 번역 키다.
- 그 VM에 끝나지 않은 `vm-start`·`vm-stop`·`vm-force-stop`·`vm-reboot` Task가 있으면 시작·정지·재시작은 `{ ok: false, reason: "vms.reason.powerPending" }`이다.
- 강제 정지는 지금 규칙 그대로다(되돌리는 중이면 `vms.reason.reverting`). 정지가 ACPI에서 멈췄을 때 빠져나갈 길이다.
- 상태 때문에 안 되는 경우에도 이유를 준다: 실패 상태(`vms.reason.failed`, phase `failed`면 먼저 본다), 이미 켜짐(`vms.reason.running`), 꺼짐(`vms.reason.stopped`), 상태가 바뀌는 중(`vms.reason.busy`). 콘솔·삭제도 같은 함수로 이유를 얻는다(꺼짐 → 콘솔 불가, 켜짐 → 삭제 불가).
- 함수는 `availability(vm, action: VmAction | "console" | "delete", vmTasks): Availability`다. `canRun`·`canDelete`는 없앤다. 판단 순서는 처리 중인 전원 Task(시작·정지·재시작만) → 상태 → 되돌리는 중(강제 정지만)이다.
- 여러 VM 시작·정지(`runMany(vms, action, taskIndex)`)도 `availability`를 쓰고, 막힌 VM은 지금처럼 건너뛴다. 부르는 쪽이 `useTaskIndex()`의 색인을 넘긴다.
- 서버는 바꾸지 않는다. CLI·API에서는 새 전원 Task가 앞 것을 대체한다.

## 6. 비활성 이유

`web/src/components/Reason.tsx`에 두 컴포넌트를 둔다.

- `ReasonMenuItem`: `DropdownMenuItem`을 `disabled` 없이 그리고, 막혔을 때 `aria-disabled="true"`를 단다. `onSelect`에서 `preventDefault()`만 하고 동작하지 않는다(메뉴도 닫히지 않는다). 이름 옆에 이유를 작은 글씨로 보여 주고 `aria-describedby`로 잇는다. 막힌 모양은 기존 `opacity-50`과 같다. Radix가 키보드 이동에서 건너뛰지 않으므로 화면 읽기 프로그램이 이름·비활성·이유를 읽는다.
- `ReasonButton`: `Button`을 `disabled` 없이 그리고 같은 방식으로 `aria-disabled`·이유·`aria-describedby`를 단다. 막혔을 때 `onClick`을 부르지 않는다(폼 안에서는 `preventDefault`). 탭 순서에 남는다.
- 적용할 곳: VM 메뉴와 VM 상세의 시작·정지·재시작·강제 정지·콘솔·삭제, 노드 제거(`NodesPage`), 자기 계정의 역할·비밀번호·삭제(`UsersPage`), 볼륨 크기 늘리기·삭제(`VolumesPage`). 기존 강제 정지 힌트 `<p>`와 볼륨의 "준비 안 됨" 글씨는 이 컴포넌트의 이유로 바꾼다.
- 버튼은 이유 글씨를 저마다 그리지 않는다. 같은 영역(VM 상세 머리, 볼륨 행)의 버튼 이유는 `ReasonList({ base, reasons })`가 한 번 그리고, 같은 이유는 한 번만 보인다. 버튼은 `reasons`에 같은 `base`(`useId()`)를 받아 `aria-describedby`로 그 글씨를 가리킨다. 메뉴 항목의 이유 글씨는 `aria-hidden`이라 이름에 섞이지 않고 `aria-describedby`로만 읽힌다.
- VM 메뉴는 시작·정지·재시작·강제 정지·콘솔·삭제를 늘 보이고 막히면 이유를 단다. VM 상세의 콘솔 버튼(전에는 실행 중일 때만 보임)도 운영자에게 늘 보인다. 막힘·가능이 바뀌어도 포커스를 잃지 않게 늘 같은 링크(`ReasonButton asChild` + `Link`)이고, 막히면 `aria-disabled`·이유이며 눌러도 열리지 않는다. `ReasonMenuItem`은 이름이 문자열이면 `textValue`로 넘겨 글자 입력으로 항목을 찾을 때(typeahead) 이유 글씨를 보지 않는다. 기존 `vms.forceStopReverting` 키는 지운다. 볼륨의 "사용 중" 글씨도 이유(`volumes.reason.inUse`)로 바꾼다.
- 이유 문구는 `vms.reason.*`, `nodes.reason.*`, `users.reason.*`, `volumes.reason.*`에 둔다(en·ko). 키: `vms.reason.{powerPending,reverting,running,stopped,failed,busy}`, `nodes.reason.{thisNode,online}`, `users.reason.{selfRole,selfPassword,selfDelete}`, `volumes.reason.{notReady,inUse,vmsUnknown}`.
- 여러 VM 선택 시의 일괄 시작·정지 버튼은 그대로 켜 둔다(할 수 있는 VM만 처리한다).

## 7. 작업 목록 페이지

- `web/src/components/Pager.tsx`: `usePage(items, pageSize)`가 `{ page, pageItems, pageCount, setPage }`를 돌려주고, `Pager`가 "이전/다음" 버튼과 "51–100 / 734" 범위를 그린다. 지금 페이지가 `pageCount`를 넘으면(항목이 지워짐) 마지막 페이지로 옮긴다. 항목이 없으면 `Pager`를 그리지 않는다. 한 페이지뿐이어도 범위("1–12 / 12")를 보인다(이전·다음은 막힌다). 끝에 닿은 이전·다음은 `disabled` 대신 `aria-disabled`로 막고 눌러도 넘어가지 않는다. 그래서 마지막 페이지로 넘긴 다음 버튼이 포커스를 잃지 않는다. 문구 키는 `pager.{label,previous,next,range}`이고 페이지 크기는 `TASK_PAGE_SIZE = 50`(`@/tasks/TasksPage`)이다.
- `TasksPage`: `SHOWN = 200` 자르기를 없애고 50개씩 나눈다. "실패만" 필터를 바꾸면 첫 페이지로 간다.
- VM 상세의 작업 탭: 최근 50개 자르기 대신 같은 `Pager`(50개씩)를 쓴다.
- 대시보드는 최근 5개 그대로다.

## 8. VM 폼 나누기

- `web/src/vms/form/`에 `GeneralSection.tsx`, `DisksSection.tsx`, `NicsSection.tsx`, `ScheduleSection.tsx`를 둔다. 각 섹션은 `values`, `errors`, `onChange(patch)`와 필요한 목록(노드·풀·볼륨·네트워크)만 받는다.
  디스크·NIC 섹션은 행을 더하거나 뺄 때 `onRowsChange(rows)`를 부른다. 행 번호로 단 오류가 다른 행을 가리키지 않게 `VmForm`이 오류를 비운다(전과 같은 동작).
- 상태·`reload`·`submit`은 `VmForm`(`VmFormPage.tsx`)에 남긴다. `submit` 안의 새 볼륨 만들기와 기다리기, VM POST/PUT과 If-Match 충돌 처리는 `web/src/vms/form/submit.ts`의 함수(`createVolume`, `createVm`, `updateVm`, `isConflict`)로 뺀다. 섹션 파일도 문구를 직접 번역한다.
- 순수 도우미 `web/src/vms/form.ts`는 그대로 둔다.
- 동작은 바꾸지 않는다. `VmFormPage.test.tsx`는 고치지 않고 그대로 통과해야 한다.

## 9. `common.*` 번역 키

- 같은 뜻으로 여러 곳에서 쓰는 일반 단어를 `common`으로 옮긴다: `cancel`, `save`, `create`, `delete`, `deleteNamed`(`"Delete {{name}}"`), `done`, `name`, `state`, `node`.
- 옮긴 개별 키(`*.name`, `*.delete`, `*.create`, `*.state`, `*.node`, `*.deleteNamed`, `users.save`, `vmForm.save`, `confirm.cancel`, `tokens.done` 가운데 위 단어와 같은 뜻인 것)는 지우고 쓰는 곳을 `common.*`으로 바꾼다. 화면에 보이는 문구는 바뀌지 않는다.
- 작업 화면의 열 제목 `tasks.stateColumn`도 같은 뜻이라 `common.state`로 합친다. 비밀값 대화상자의 완료 버튼은 `common.done`이다.
- 뜻이 다른 키는 그대로다: VM 상태 이름(`Running`), 동작 이름(`Stop`), 화면 제목 등. 예: `vms.action.delete`, `snapshots.create`·`snapshots.deleteNamed`, `tasks.done`, `clone.name`, `forms.name`.
- `i18n.test.ts`에 지운 키 목록이 en·ko 어디에도 다시 생기지 않았는지 확인하는 테스트를 더한다.

## 10. 비밀값 대화상자 뒤 포커스

- 토큰 화면의 `SecretDialog`와 클러스터 화면의 `JoinTokenDialog`에 `returnFocus`를 받게 하고 `onCloseAutoFocus`에서 기존 `focusBack`을 부른다.
- 여는 버튼("토큰 만들기", "참가 토큰 발급")에 ref를 달아 넘긴다. 앞 단계의 만들기·발급 `FormDialog`에도 같은 ref를 넘겨, 취소했을 때도 같은 버튼으로 돌아간다. `returnFocus`는 선택이 아니라 필수 속성이다.

## 11. 테스트

- **서버.** janitor: 7일 넘은 끝난 Task를 지우고 끝나지 않은 Task는 남긴다, 1000개를 넘으면 오래된 것부터 지운다, tick 상한을 지킨다(작은 값으로 설정해 확인). opld 설정 기본값. `web.rs`: ETag가 붙는다, 같은 `If-None-Match`면 304이고 보안 헤더가 있다, 다른 값이면 200이다.
- **웹.**
  - `availability`: 상태별 결과와 이유, 처리 중인 전원 Task로 막힘, 강제 정지는 막히지 않음.
  - `useTaskIndex`: 끝나지 않은 Task만 대상별로 모은다.
  - `ReasonMenuItem`·`ReasonButton`: 키보드로 닿는다, 접근성 설명이 이유다, 눌러도 동작하지 않는다.
  - `Pager`: 페이지 넘기기, 필터를 바꾸면 첫 페이지, 항목이 줄면 마지막 페이지로.
  - 화면: 처리 중인 전원 Task가 있는 VM의 시작이 이유와 함께 막힌다, 자기 계정 삭제가 이유와 함께 막힌다.
  - 포커스: 비밀값 대화상자의 완료·만들기 취소 뒤 `document.activeElement`가 여는 버튼이다.
  - i18n: 지운 키가 다시 생기지 않는다.
- 기존 시험(워크스페이스, `storage-it`, 웹 검사, `web-it`, CI)이 그대로 통과한다.

## 12. 알려진 한계

- 웹은 여전히 모든 Task를 받는다. 보존 정책으로 끝난 Task가 1000개 남짓을 넘지 않으므로 받아 들이는 양에 상한이 생긴다.
- Task 보존 기간은 만든 시각 기준이다.
- 웹에서만 전원 버튼을 막으므로 CLI·API로는 처리 중인 전원 Task를 대체할 수 있다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 401개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e`
Expected: PASS 5개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 작업 보존·페이지·비활성 이유 안내와 웹 정리에서 구현하며 정한 것"
```

---

## 완료 기준 (계획 14)

- 리더의 janitor가 7일 넘은 끝난 Task를 지우고, 1000개를 넘으면 유예가 지난 오래된 것부터 지우며, 끝나지 않은 Task는 남는다(`cargo test -p opl-api --test janitor`와 `janitor` 단위 테스트).
- 바뀌지 않은 `index.html`을 다시 확인하면 304이고 보안 헤더가 붙는다(`cargo test -p opld web::`).
- 처리 중인 전원 Task가 있는 VM의 시작·정지·재시작이 이유와 함께 막히고, 강제 정지는 열려 있다.
- 막힌 메뉴 항목·버튼이 키보드로 닿고 이유가 접근성 설명으로 읽힌다.
- 작업 목록과 VM 작업 탭이 50개씩 나뉜다.
- `VmFormPage.test.tsx`가 고치지 않고 통과하고, 화면 문구가 바뀌지 않는다.
- 비밀값 대화상자를 닫으면 포커스가 연 버튼으로 돌아간다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과한다.

## 알고 받아들인 제한

- **Task 전송:** 웹은 여전히 모든 Task를 받는다. 보존 정책으로 끝난 Task가 1000개 남짓을 넘지 않아 상한이 생긴다.
- **보존 기준:** 기간은 만든 시각 기준이다. 리더가 바뀌면 새 리더는 10분 동안 끝난 Task를 지우지 않는다.
- **전원 버튼:** 웹에서만 막는다. CLI·API로는 처리 중인 전원 Task를 대체할 수 있다.
- 그 밖의 제한은 설계 문서 §12에 있다.
