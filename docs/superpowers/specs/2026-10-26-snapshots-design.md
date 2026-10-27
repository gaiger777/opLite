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
