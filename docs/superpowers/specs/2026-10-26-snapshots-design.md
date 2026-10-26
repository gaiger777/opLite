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
| `status.created_at` | 만든 시각 |
| `status.vmstate_bytes` | 메모리 상태 크기(메모리 스냅샷만) |

- 리소스 이름은 클러스터 전체에서 고유해야 하므로 스냅샷 id 문자열이다. "VM 안에서 이름 고유"는 배타 값(`exclusive.rs`)의 `(vm, name)` 키로 지킨다. API와 CLI는 `/vms/{vm}/snapshots/{name}`처럼 VM과 이름으로 가리킨다.
- 디스크 안의 qcow2 태그는 스냅샷 id다. 메모리 상태는 VM 스펙의 첫 qcow2 디스크(`disks[0]`)에 담는다(`vmstate`).
- UEFI 변수 사본: `{state_dir}/vm/{vm-id}/snapshots/{snapshot-id}.vars`.

### VM에 더하는 것

- `status.snapshot`: 현재 위치(마지막으로 만들었거나 되돌린 스냅샷). 다음 스냅샷의 부모.
- `spec.snapshot_schedule` (계획 11): `{every: hourly|daily|weekly, keep: 1..=100, memory: bool}`. 기본은 없음, `memory` 기본 false.
- `status.schedule_error` (계획 11): 마지막 예약 스냅샷 실패 이유.

기존 VM은 새 필드가 기본값이라 그대로 읽힌다.

### Task 종류

`snapshot-create`, `snapshot-revert`, `snapshot-delete`, `volume-resize`, `vm-clone`(계획 11). 스냅샷·복제 Task는 VM이 있는 노드(`status.node`, 없으면 `spec.node`, 둘 다 없으면 API를 받은 노드)에서, 크기 늘리기는 실행 중인 VM이 쓰면 그 VM의 노드, 아니면 볼륨의 노드에서 돈다.

## 4. 드라이버

- **QMP 작업 도우미** (`opl-qemu`): 작업을 시작하고 `JOB_STATUS_CHANGE`(또는 `query-jobs`)로 `concluded`를 기다린 뒤 오류를 읽고 `job-dismiss`한다. 제한 시간을 둔다.
- **`Hypervisor` 트레잇**에 실행 중인 VM용 메서드: `snapshot_save(vm, tag, devices, vmstate: Option)`, `snapshot_delete(vm, tag, devices)`, `block_resize(vm, device, bytes)`. 디스크만 담는 실행 중 스냅샷은 `transaction` + `blockdev-snapshot-internal-sync`로 모든 디스크를 한 번에 한다(VM이 멈추지 않는다). 메모리 복원은 시작 경로에 넣는다(아래).
- **메모리 복원 시작:** reconciler가 VM을 시작할 때 한 번짜리 "불러올 스냅샷"을 받을 수 있게 한다. 드라이버는 지금처럼 `-S`로 띄우고 QMP에 붙은 뒤 `cont` 전에 `snapshot-load`를 한다. 불러오기에 실패하면 QEMU를 끝내고 VM은 `failed`가 된다.
- **`Storage` 트레잇**에 정지한 볼륨용 메서드: `snapshot_create/revert/delete(pool, volume, tag)`, `snapshot_list`, `resize_volume`, `copy_volume(src, tag: Option, dst)`. 백엔드가 스냅샷을 지원하는지 알리는 `supports_snapshots(format)`을 둔다. `dir` 백엔드는 qcow2만 지원한다. 뒤의 LVM·ZFS·RBD는 자기 방식으로 구현한다.
- **같은 VM의 작업 직렬화:** Task 실행기의 대상별 줄(`by_target`)을 VM에도 쓴다. 같은 VM의 스냅샷 Task와 전원 Task가 한 노드에서 동시에 돌지 않는다. reconciler의 시작·정지와는 VM 상태(안정 상태 검사)와 Task 안의 재확인으로 겹치지 않게 한다.

## 5. 동작

### 만들기 — `POST /vms/{vm}/snapshots {name, description?, memory?}` → 202

검사:
- VM이 안정 상태(실행 중이거나 정지·실패)여야 한다. 시작·정지 중이면 409 `vm_busy`.
- 모든 디스크가 qcow2이고 `ready`. raw가 있으면 409 `snapshot_unsupported`, 디스크가 없으면 메모리 스냅샷은 400.
- `memory`는 실행 중이면 기본 true. 정지한 VM에 `memory: true`는 400.

실행: `pending` 리소스를 만들고(`parent` = VM의 `status.snapshot`, `vm_spec` = 지금 스펙) Task를 돌린다.

| VM 상태 | 방법 |
|---|---|
| 실행 중 + 메모리 | `snapshot-save` (devices = 모든 디스크, vmstate = 첫 디스크). 저장하는 동안 VM이 멈춘다 |
| 실행 중 + 디스크만 | `transaction` + `blockdev-snapshot-internal-sync` |
| 정지 | 디스크마다 `qemu-img snapshot -c` |

UEFI 변수를 복사하고 `ready`로 바꾸고 VM의 `status.snapshot`을 이 스냅샷으로. 실패하면 만든 태그를 지우고 리소스를 지운다. Task 안에서 VM 상태를 다시 확인한다(Task가 기다리는 사이 바뀔 수 있다).

### 되돌리기 — `POST /vms/{vm}/snapshots/{name}/revert` → 202

검사: 스냅샷이 `ready`, `vm_spec`의 볼륨이 모두 있고 `ready`이며 다른 VM이 쓰지 않는다.

실행:
1. 실행 중이면 강제로 끈다.
2. VM 스펙을 `vm_spec`으로 바꾼다. `node`는 지금 노드를 유지하고, `power`는 메모리 스냅샷이면 `running`, 아니면 `stopped`. 그 뒤로 붙인 디스크는 VM에서 떨어지고 볼륨은 남는다.
3. 디스크마다 `qemu-img snapshot -a`, UEFI 변수 사본을 되돌린다.
4. 볼륨의 실제 크기를 다시 읽어 볼륨 스펙에 반영한다(스냅샷 뒤 늘린 크기는 되돌아간다).
5. 메모리 스냅샷이면 "불러올 스냅샷"을 주고 시작한다(§4). 그 순간부터 실행된다.
6. VM의 `status.snapshot`을 이 스냅샷으로.

### 삭제 — `DELETE /vms/{vm}/snapshots/{name}` → 202

실행 중이면 `snapshot-delete`, 정지했으면 `qemu-img snapshot -d`. 변수 사본을 지운다. 자식의 `parent`를 지운 스냅샷의 부모로 바꾸고, 현재 위치였으면 VM의 `status.snapshot`도 부모로. 디스크에 태그가 이미 없으면 성공으로 본다(다시 시도해도 된다).

### VM 삭제

스냅샷이 남아 있으면 409 `vm_has_snapshots`. CLI `vm delete --with-snapshots`와 웹의 "스냅샷도 모두 지우기"는 스냅샷 삭제 Task를 차례로 돌린 뒤 VM을 지운다. 볼륨은 지금처럼 남는다.

### 볼륨 규칙

스냅샷의 `vm_spec`이 가리키는 볼륨은 지울 수 없다(409 `volume_in_snapshot`). VM에서 떼어 낸 볼륨도 스냅샷이 남아 있으면 지울 수 없다.

### 볼륨 크기 늘리기 — `POST /volumes/{v}/resize {size_bytes}` → 202

지금보다 커야 한다(400). 실행 중인 VM이 쓰면 그 노드에서 `block_resize`(게스트가 바로 본다), 아니면 볼륨의 노드에서 `qemu-img resize`. 성공하면 실제 크기를 읽어 `spec.size_bytes`를 바꾼다. raw도 된다.

### 복제 (계획 11) — `POST /vms/{vm}/clone {name, snapshot?, pool?}` → 202

- 원본: 스냅샷, 또는 정지한 VM의 지금 상태. 실행 중인데 스냅샷을 주지 않으면 409 `vm_running`.
- 원본 VM 스펙(스냅샷이면 `vm_spec`)의 디스크마다 새 볼륨 `{새 VM}-{원래 볼륨}`(63자로 자르고, 겹치면 `-2`, `-3`)을 만들고 `qemu-img convert`(스냅샷이면 `-l snapshot.name=<id>`)로 복사한다.
- 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다(기본은 원본과 같은 풀).
- 새 VM: MAC 새로, `node` 없음, `power: stopped`, 예약 없음, 스냅샷 없음.
- 실패하면 만든 볼륨을 지운다.

### 예약 스냅샷 (계획 11)

리더에서 1분마다 도는 반복 작업(`NodeMonitor`와 같은 모양: 리더 확인 → `read_barrier` → 목록).
- 예약이 있는 VM마다, 마지막 예약 스냅샷의 `created_at`에서 주기가 지났으면 `auto-YYYYMMDD-HHMM`(UTC) 스냅샷을 만든다. 실행 중이고 `memory`면 메모리 포함.
- 만든 뒤 `scheduled`인 스냅샷이 `keep`개를 넘으면 오래된 것부터 지운다. 사용자가 만든 스냅샷은 지우지 않는다.
- 만들 수 없으면(raw 디스크, VM 바쁨, 노드 오프라인) `status.schedule_error`에 남기고 다음 주기에 다시 본다. 성공하면 지운다.
- 같은 VM에 예약 Task가 이미 돌고 있으면 건너뛴다.

## 6. 일관성과 장애

- **opld 재시작:** 지금처럼 실행 중이던 Task는 실패로 끝난다. 노드가 시작할 때 이 노드의 VM에 대해 `pending` 스냅샷(끝나지 않은 Task가 없는 것)을 지우고 디스크의 같은 태그를 지운다(볼륨 고아 정리와 같은 방식).
- **되돌리기 중단:** 디스크 일부만 되돌아간 채 멈추면 VM은 정지 상태로 남고 Task는 실패다. 같은 되돌리기를 다시 하면 처음부터 다시 한다(`-a`는 몇 번 해도 같다).
- **삭제 중단:** 태그가 없으면 성공으로 보므로 다시 하면 된다.
- **태그만 남은 경우:** 리소스 없이 디스크에 남은 태그는 공간만 쓴다. 자동으로 지우지 않고, 노드에서 `qemu-img snapshot -l`로 볼 수 있다.

## 7. 권한

스냅샷은 VM 권한을 따른다. 목록은 Read, 만들기·되돌리기·삭제는 VM Operate(operator 이상). 복제는 VM·볼륨 Write. 예약 설정은 VM Write. 볼륨 크기 늘리기는 볼륨 Write. viewer는 보기만 한다.

## 8. CLI

| 명령 | 계획 |
|---|---|
| `oplctl vm snapshot create <vm> <이름> [--description …] [--no-memory]` | 10 |
| `oplctl vm snapshot list <vm>` (나무 모양, 현재 위치 `*`) | 10 |
| `oplctl vm snapshot revert <vm> <이름>`, `oplctl vm snapshot delete <vm> <이름>` | 10 |
| `oplctl vm delete <vm> --with-snapshots` | 10 |
| `oplctl volume resize <볼륨> --size 20G` | 10 |
| `oplctl vm clone <vm> <새 이름> [--snapshot …] [--pool …]` | 11 |
| VM 수정에 `--snapshot-every hourly|daily|weekly --snapshot-keep N [--snapshot-memory]`, `--no-snapshot-schedule` | 11 |

명령은 Task가 끝날 때까지 기다린다.

## 9. 웹 UI

계획 10:
- **VM 상세 "스냅샷" 탭:** 나무 모양 목록(이름, 만든 시각, 메모리 포함·크기, 설명, 현재 위치 표시). 만들기 대화상자(이름, 설명, 실행 중이면 "메모리 포함" 기본 켬). 되돌리기 확인("지금 상태는 사라지고 VM이 꺼졌다가 스냅샷 상태가 된다"), 삭제 확인. 스냅샷할 수 없으면(raw 디스크) 탭 안에 까닭을 글로.
- **볼륨 화면:** "크기 늘리기" 대화상자(지금보다 큰 값만).
- **VM 삭제 확인:** 스냅샷이 있으면 "스냅샷도 모두 지우기" 선택.
- 스냅샷은 watch 대상(`WatchKind`)에 넣어 바로 갱신된다.

계획 11:
- **VM 메뉴 "복제":** 새 이름, 원본(지금 상태 / 스냅샷), 대상 풀.
- **VM 폼 "예약 스냅샷":** 주기, 보관 개수, 메모리 포함. VM 상세에 마지막 예약 실패.

## 10. 테스트

- **단위:** 타입·직렬화, API 검사 규칙(바쁜 VM, raw 디스크, 정지 VM의 메모리, 이름 중복, 볼륨 삭제 막기, VM 삭제 막기), 나무 정리(자식 옮기기), 예약 판단(주기, 보관 개수)과 이름, QMP 작업 도우미(가짜 QMP).
- **실제 `qemu-img`:** 정지 볼륨의 스냅샷 만들기·되돌리기·삭제(`qemu-io`로 쓴 패턴이 되돌아가는지), 크기 늘리기, 스냅샷에서 복사.
- **실제 QEMU (`qemu-it`, 맥 hvf와 CI KVM):** 실행 중 메모리 스냅샷 → 되돌리기 → 실행 상태로 돌아옴. 실행 중 디스크만 스냅샷. 실행 중 크기 늘리기(`query-block`으로 새 크기). 되돌리기로 디스크 내용이 돌아오는지는 정지 상태에서 확인(OS 없는 게스트).
- **브라우저 E2E:** 계획 10은 기존 흐름에 메모리 스냅샷 만들기 → 되돌리기 → 삭제와 볼륨 크기 늘리기를 더한다. 계획 11은 스냅샷에서 복제한 VM 시작, 예약 스냅샷(테스트에서만 주기를 짧게 하는 설정).

## 11. 알고 받아들인 제한

- 메모리 스냅샷을 만드는 동안 VM이 멈춘다(메모리 크기에 비례, 512MiB에서 약 0.6초).
- raw 디스크가 붙은 VM은 스냅샷할 수 없다.
- 되돌리기는 실행 중인 VM을 강제로 끈다(저장하지 않은 게스트 상태는 사라진다).
- 복제는 디스크 전체를 복사한다(링크드 클론 없음). 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다.
- 예약은 주기만 있고 시각을 정하지 못한다. 리더가 바뀌면 그 주기의 스냅샷이 1분 남짓 늦을 수 있다.
- 리소스 없이 디스크에 남은 태그는 자동으로 지우지 않는다.

## 12. 계획 분할

| 계획 | 범위 | 완료 기준 |
|---|---|---|
| 10 | `Snapshot` 리소스, QMP 작업 도우미, 드라이버(실행 중·정지), 만들기·되돌리기·삭제, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기, 재시작 정리, CLI, 웹 스냅샷 탭·크기 늘리기·VM 삭제 선택, E2E | 브라우저에서 실행 중인 VM의 메모리 스냅샷을 만들고 되돌려 그 순간부터 실행되는 E2E가 맥과 CI `linux-kvm`에서 통과 |
| 11 | 복제, 예약 스냅샷(리더 반복 작업, 보관 정리), CLI, 웹 | 스냅샷에서 복제한 VM이 시작되고, 예약 스냅샷이 만들어지고 보관 개수대로 정리되는 것을 E2E로 확인 |
