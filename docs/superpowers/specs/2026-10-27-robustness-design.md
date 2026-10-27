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
- 조건 없는 `Delete`이고 `NotFound`를 받으면 성공이다. 조건부 삭제(`expected_revision`이 있는 것, 합류 토큰 소비·정리 작업의 스냅샷 삭제)는 "지운 쪽만 성공"을 뜻할 수 있어 판단하지 않는다(최종 리뷰에서 바꿨다: 같은 토큰으로 두 노드가 합류할 수 있었다).
- 그 밖(조건부 수정 등)은 지금처럼 오류를 돌려준다(코드 주석).

구현하며 정한 것:
- "응답 없이 끝남" = 시도 제한 시간에 걸림, 또는 전달자가 `Unreachable`을 돌려줌. 연결하지 못한 것과 보낸 뒤 끊긴 것을 구분할 수 없어 `Unreachable`은 늘 적용됐을 수 있다고 본다. 그래서 그 뒤의 조건 없는 `Delete`가 `NotFound`면 다른 쪽이 지웠어도 성공이다.
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
