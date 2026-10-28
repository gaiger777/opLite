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
- **개수.** 기간으로 지우고도 끝난 Task가 `task_max_finished`(기본 1000)개를 넘으면, 넘는 만큼 만든 시각이 오래된 것부터 지운다. 끝나지 않은 Task는 세지 않는다. 구현은 끝난 Task를 오래된 것부터 줄 세우고 앞에서 기간이 넘은 것과 개수를 넘은 수 안의 것 중 아래 유예가 지난 것을 고른다(`janitor.rs`의 `expired_tasks`).
- **유예.** 두 기준 모두 리더가 끝난 것을 처음 본 뒤 `finished_grace`(10분)가 지난 Task만 지운다. 막 끝난 Task를 기다리는 oplctl `wait_task`·웹 `waitForTask`가 결과를 읽기 전에 Task가 지워지지 않게 한다. 기간이 넘은 Task도 늦게 끝날 수 있다(예: `task_max_age`보다 오래 꺼져 있던 노드의 Task가 실패로 끝난다). 처음 본 시각은 고아 스냅샷처럼 리더의 메모리(`Seen`)에만 두고, 지워졌거나 없어진 Task는 잊는다. 리더가 바뀌면 기억이 비므로 새 리더는 유예 동안 끝난 Task를 지우지 않는다. `finished_grace`는 테스트를 위해 `ClusterJanitorConfig` 필드로 두고 opld 설정에는 내놓지 않는다.
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
