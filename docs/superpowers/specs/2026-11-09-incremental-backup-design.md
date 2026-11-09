# opLite 증분 백업과 증분 복제 설계 (하위 프로젝트 12: 계획 40·41)

작성일: 2026-11-09
상위 문서: `2026-11-04-backup-design.md`(백업·백업 작업), `2026-11-06-enterprise-design.md`(원격 복제, 계획 38), `2026-11-09-node-maintenance-design.md`(노드 기능 목록 `features`)

## 1. 목표와 범위

지금 백업은 매번 디스크 전체를 읽고 압축해 쓴다. 원격 복제도 매번 백업 전체를 보낸다(설계 문서마다 "증분 없음"을 한계로 남겼다). 디스크가 크고 하루 변경이 작은 VM은 백업 시간, 백업 풀 공간, 복제 대역폭을 거의 모두 바뀌지 않은 데이터에 쓴다.

| 계획 | 내용 |
|---|---|
| 40 | 증분 백업: 실행 중인 VM의 디스크마다 QEMU dirty bitmap으로 바뀐 블록을 따라가고, 바로 앞 백업을 부모로 바뀐 블록만 담은 백업을 만든다. 체인 복원, 부모 지우기 막기, 보존 규칙의 체인 보호, 백업 작업의 `full_every`, API·oplctl·웹, 실제 시험 |
| 41 | 증분 복제: 증분 백업을 원격으로 보낼 때 부모 사본이 원격에 있으면 증분 파일만 보내고, 없으면 체인의 빠진 백업부터 차례로 보낸다. 받는 쪽 부모 확인, API·oplctl·웹, 실제 시험 |

넣지 않는 것: 꺼진 VM의 증분(꺼진 VM은 늘 전체), 중복 제거 청크 저장소(PBS), 증분 병합(부모를 자식에 합쳐 체인 줄이기), 바이트 단위 이어 보내기, 대역폭 제한, 백업 암호화, 원격(S3) 저장소, 파일 단위 복원.

### 견준 길

- **바뀐 블록 찾기: QEMU dirty bitmap(채택) vs 내용 해시 비교.** 해시 비교는 백엔드·전원과 무관하지만 매번 디스크 전체를 읽는다(읽기 시간은 그대로이고 공간·전송만 준다). dirty bitmap은 QEMU가 쓰기를 따라가므로 바뀐 블록만 읽는다. 이것이 libvirt·Proxmox의 표준 방식이다. 비트맵이 틀릴 수 있는 경우(QEMU 밖에서 디스크를 바꾸는 것)는 §3.4의 규칙으로 막고, 의심스러우면 전체 백업으로 돌아간다.
- **증분 읽기: `blockdev-backup sync=incremental`(채택) vs NBD로 내보내고 opld가 읽기.** `blockdev-backup`은 copy-before-write로 시점을 지키며 QEMU가 직접 대상 qcow2에 쓴다(`compress`도 된다). NBD 내보내기(pull 방식)는 opld에 NBD 클라이언트와 블록 상태 질의가 새로 필요하다.
- **증분 형식: 부모 없는 qcow2 + 매니페스트의 부모 링크(채택) vs qcow2 헤더의 backing file.** 받는 클러스터의 백업 id가 다르므로 헤더에 부모 경로를 적으면 복제 때 고쳐 써야 하고 해시가 바뀐다. 헤더의 경로를 따르는 것은 백업 파일 밖을 가리키는 경로를 막는 지금의 `check_import_source`와도 맞지 않는다. 그래서 체인은 매니페스트의 `parent`로만 잇고, 복원할 때 opld가 `json:` 이미지 옵션으로 체인을 명시해 연다.
- **체인 정리: 부모 지우기 막기와 보존 규칙의 조상 보호(채택) vs 병합.** 병합(`qemu-img rebase`/`commit`)은 백업 파일을 고쳐 쓰므로 해시와 원격 사본이 어긋나고, 압축을 잃는다. 체인 길이를 백업 작업의 `full_every`로 제한하면 조상 보호로 더 남는 공간에 상한이 생긴다.

## 2. 지금 있는 것 (계획 39 끝)

- 백업(계획 27·28): 실행 중인 VM은 디스크 백엔드의 스냅샷으로 시점을 잡는다. qcow2(dir·NFS)는 QMP `transaction` + `blockdev-snapshot-internal-sync`를, 블록 풀(LVM-thin·ZFS·RBD)은 `stop` → `snapshot_many` → `cont`(최대 120초)를 쓴다. 그 뒤 `qemu-img convert -c -U -l`로 `{풀}/backups/{id}/disk{i}.qcow2`에 압축 qcow2를 쓰고, 임시 스냅샷을 지운다. 꺼진 VM은 디스크를 그대로 읽는다. `manifest.json`(`MANIFEST_VERSION = 1`)을 쓰고 `ready`로 바꾼다. 복원은 늘 새 VM이다(`check_import_source` → `import_volume`).
- 디스크는 `-blockdev ...,node-name=disk{i}-file`과 `-blockdev driver=qcow2|raw,file=disk{i}-file,node-name=disk{i}`로 붙는다. QMP 클라이언트(`opl-qemu/src/qmp.rs`)에 `transaction`, 작업 기다리기(`query-jobs`·`job-dismiss`)가 있다.
- 백업 작업: 리더의 스케줄러, 노드당 동시 2개(`[backup] max_concurrent`), Proxmox식 보존 규칙(`keep`).
- 원격 복제(계획 38): `backup-replicate` Task가 파일마다 흘려 보내며 해시를 비교한다. 받는 쪽은 `POST /backups/receive` → 파일 `PUT` → `complete`이다. 원본을 지우면 원격 사본도 지운다.
- 노드 기능 목록 `NodeStatus.features`와 `cluster::require_feature`(계획 39): 옛 노드가 있으면 새 기능을 409 `cluster_upgrading`으로 막는다.
- 지원 플랫폼은 Debian 13(QEMU 10)과 macOS(Homebrew QEMU)다. `blockdev-backup`의 `bitmap-mode`(QEMU 4.2+)와 `qemu-img bitmap`(5.0+)을 쓸 수 있다.

## 3. 증분 백업 (계획 40)

### 3.1 비트맵

- 디스크 노드 `disk{i}`마다 비트맵을 하나 둔다. 이름은 `opl-{백업 id(simple)}`이고, "이 백업의 시점 뒤에 바뀐 블록"을 뜻한다.
- qcow2 디스크의 비트맵은 `persistent: true`로, QEMU가 끝날 때 qcow2 파일에 저장하고 다음 시작 때 다시 읽는다. raw 디스크(LVM-thin·ZFS·RBD)의 비트맵은 QEMU 프로세스 안에만 있어 VM을 다시 켜면 사라진다. 그러면 다음 백업은 전체가 된다.
- 백업이 성공할 때마다(전체든 증분이든) 새 비트맵 `opl-{새 id}`를 그 백업의 시점에 만들고, 다른 `opl-` 비트맵은 지운다. 실패하면 새 비트맵을 지우고 이전 비트맵을 그대로 둔다.
  - **실행 중 전체, qcow2:** 내부 스냅샷 `transaction`에 `block-dirty-bitmap-add`를 함께 넣는다(같은 시점).
  - **실행 중 전체, 블록 풀:** `stop`과 `cont` 사이에 비트맵을 더한다.
  - **꺼진 VM, qcow2:** 내보낸 뒤 `qemu-img bitmap --add`(persistent)와 `--remove`로 같은 상태를 만든다. VM 잠금을 쥐고 있어 내보낸 내용과 그 시점의 디스크가 같다. 꺼진 raw 디스크는 비트맵을 두지 않는다.

### 3.2 증분 백업 요청

- `POST /vms/{key}/backups`의 본문에 `incremental: bool`(기본 false)을 더한다. `true`는 "할 수 있으면 증분"이다. 할 수 없으면 전체로 하고 그 이유를 `status.full_reason`에 적는다.
  - 예: `"VM is stopped"`, `"no backup of this VM in pool P"`, `"dirty bitmap for backup B is missing on disk1"`, `"disks changed since backup B"`, `"chain is 6 backups long"`.
- `BackupSpec.incremental: bool`(요청, 기본 false, false면 직렬화에서 뺀다).
- `BackupStatus.parent: Option<Uuid>`(Task가 정한 부모, 전체면 없음)와 `full_reason: Option<String>`.
- `incremental: true`는 기능 `incremental-backup`이 없는 노드가 있으면 409 `cluster_upgrading`이다(`require_feature`).

### 3.3 부모 고르기 (Task, VM 잠금 안에서)

증분으로 하려면 모두 맞아야 한다. 하나라도 어긋나면 전체로 하고 `full_reason`을 적는다.

1. VM이 실행 중이다.
2. 그 VM의 디스크 노드마다 `opl-` 비트맵이 정확히 하나 있고, 모두 같은 이름 `opl-{B}`이며, `recording: true`이고 `inconsistent`가 아니다. QEMU가 비정상으로 끝나면 저장된 비트맵은 inconsistent로 열린다.
3. 백업 B가 같은 풀에 있고 `ready`이다.
4. 디스크가 B와 같다: 수, 순서마다 같은 볼륨, 같은 가상 크기(B 매니페스트의 `vm_spec`과 `disks[].virtual_bytes`).
5. 체인 길이(B에서 전체 백업까지 거슬러 센 증분 수 + 1)가 요청의 한도 이하다. 수동 백업은 한도가 없고, 백업 작업은 `full_every`이다.

### 3.4 비트맵을 믿을 수 없게 되는 일

- **스냅샷 되돌리기:** opLite에서 QEMU 밖에서 디스크 내용을 바꾸는 길은 이것뿐이다(VM을 끈 뒤 `snapshot_revert`). 되돌린 볼륨마다 `opl-` 비트맵을 지운다(qcow2: `qemu-img bitmap --remove`). 그래서 다음 백업은 전체다.
- **디스크를 바꾸거나 크기를 늘림:** §3.3의 4번이 잡는다.
- **QEMU 비정상 종료, HA 복구:** §3.3의 2번(inconsistent)이 잡는다.
- **라이브 마이그레이션:** 공유 qcow2의 persistent 비트맵은 원본이 이미지를 넘길 때 저장되고 대상이 열 때 읽는다. raw 공유 디스크(RBD)의 비트맵을 지키려고 마이그레이션 능력 `dirty-bitmaps`를 켠다(양쪽 모두 같은 노드 이름이다). 잃으면 2번이 잡는다.
- **opLite 밖에서 디스크를 고친 경우:** 알 수 없다. README에 "다음 백업을 `incremental` 없이(전체로) 하라"고 적는다.

### 3.5 증분 백업 Task

같은 `VmBackup` Task 안에서 부모가 정해지면 다음을 한다.

1. 디스크마다 `qemu-img create -f qcow2 {dir}/disk{i}.qcow2.part {가상 크기}`로 부모 없는 빈 qcow2를 만든다.
2. QMP `blockdev-add`로 대상 노드를 더한다: `opl-backup-{i}`, qcow2, file=그 경로, `backing: null`.
3. `transaction`을 하나 보낸다. 디스크마다 `block-dirty-bitmap-add {disk{i}, opl-{새 id}, persistent}`와 `blockdev-backup {job-id: opl-backup-{i}, device: disk{i}, target: opl-backup-{i}, sync: incremental, bitmap: opl-{B}, bitmap-mode: never, compress: true, auto-dismiss: false}`를 넣는다. 모든 디스크가 한 시점이다.
4. 작업이 모두 끝나기를 기다린다(`query-jobs`). 하나라도 실패하면 나머지를 `job-cancel`한다.
5. `job-dismiss`와 `blockdev-del`을 하고 `.part`를 이름 바꿔 둔다.
6. 성공하면 `opl-{B}`를 지운다. 실패하면 `opl-{새 id}`와 대상 파일을 지우고 Task를 실패로 끝낸다. 다음 시도는 다시 B에서 증분이다.
7. 매니페스트는 `MANIFEST_VERSION = 2`에 `parent: Option<Uuid>`를 더해 쓴다(같은 백업 id 공간). 전체 백업은 지금처럼 버전 1로 써서 옛 노드도 읽는다. 읽을 때는 1과 2를 받는다.

`disks[].stored_bytes`는 증분 파일의 크기이다. 웹·oplctl은 "증분"과 체인의 전체 크기(조상 합)를 함께 보인다.

### 3.6 복원

`BackupRestore`는 백업에서 전체 백업까지 부모를 거슬러 체인을 만든다. 모두 같은 풀에 `ready`로 있고, 매니페스트의 디스크 수와 가상 크기가 맞아야 한다. 아니면 실패 "backup chain is broken: …".

- 디스크마다 파일에 `check_import_source`를 지금처럼 한다(헤더에 backing file이 없어야 한다).
- 그 뒤 `qemu-img convert`의 원본을 `json:{"driver":"qcow2","file":{"driver":"file","filename":자식},"backing":{…부모…,"backing":null}}`로 체인을 명시해 연다.
- 새 기능 `import_chain`은 `Storage::import_volume`이 받는 원본을 이미지 옵션 원본으로 넓힌다.
- 복원 대상 노드는 증분 백업이면 `incremental-backup` 기능이 있어야 한다. 버전 2 매니페스트를 옛 노드가 읽지 못하기 때문이다.

### 3.7 지우기와 보존 규칙

- 다른 백업의 `status.parent`인 백업을 지우면 409 `backup_in_use`("backup B is the parent of N incremental backup(s); delete them first")이다. 지우는 Task도 같은 확인을 한다.
- 보존 규칙은 남길 백업을 지금처럼 고른다. 그 뒤 남길 백업의 조상을 모두 남길 목록에 더하고, 자식부터 지운다.
- `failed` 백업은 부모가 될 수 없다(부모는 늘 `ready`). `pending` 증분이 있는 동안 그 부모 지우기는 `backup_in_use`이다.

### 3.8 백업 작업

- `BackupJobSpec.full_every: u32`(기본 0, 0이면 직렬화에서 뺀다)를 더한다. 0이면 지금처럼 늘 전체다. n이면 증분으로 요청하되 체인의 증분이 n개가 되면 다음은 전체다.
  - 예: 매일 + `full_every: 6`이면 주마다 전체 하나와 증분 여섯이다.
- `full_every > 0`인 작업을 만들거나 바꾸려면 `incremental-backup` 기능이 필요하다.

### 3.9 API·oplctl·웹

- **API:** `CreateBackupRequest.incremental`, `Backup`의 `spec.incremental`·`status.parent`·`status.full_reason`, `BackupJobSpec.full_every`, 오류 `backup_in_use`.
- **oplctl:**
  - `oplctl backup create <vm> --pool P [--incremental]`
  - `oplctl backup list`의 `TYPE` 열(`full`/`incr`)
  - `oplctl backup get`의 부모·`full_reason`·체인 크기
  - `oplctl backup-job create|set --full-every N`
- **웹:**
  - 백업 만들기 대화상자의 "증분(가능하면)" 선택
  - 백업 목록의 종류 배지와 부모 링크
  - 백업 상세의 "전체로 한 이유"와 체인 크기
  - 작업 폼의 "전체 백업 주기(증분 n개마다)"
  - 부모 지우기의 `backup_in_use` 오류 문구

## 4. 증분 복제 (계획 41)

- 보내는 쪽 `backup-replicate` Task는 백업 X를 원격 R의 풀 P로 보낼 때 X의 체인을 전체 백업부터 차례로 본다. 각 백업의 `replicas`에 (R, P)가 `ready`이면 건너뛰고, 아니면 계획 38의 방법으로 보낸다. 앞 백업이 끝나야 다음을 보낸다.
- **보낼 때:** `ReceiveRequest`에 `parent: Option<Uuid>`(받는 쪽의 부모 사본 id, 보내는 쪽 `replicas`에 적힌 값)를 더한다. 매니페스트는 버전 2 그대로 보내고, 받는 쪽이 자기 id로 다시 쓴다(`backup`과 `parent`).
- **받는 쪽 확인:** 부모 사본이 같은 풀에 `ready`이고, 출처 CA 지문이 같고, `origin.backup`이 보낸 매니페스트의 `parent`와 같아야 한다. 아니면 409 `replica_parent_missing`이다. 이 오류를 받은 보내는 쪽은 부모의 `replicas` 기록을 지우고 부모부터 다시 보낸다(한 번).
- **같은 사본 확인(`check_copy`):** 부모 링크도 비교한다.
- **원격 지원 확인:** 원격 클러스터의 `/nodes`로 모든 노드에 `incremental-backup` 기능이 있는지 본다. 없으면 복제 실패 "remote cluster does not support incremental backups; upgrade it"이다(전체 백업은 지금처럼 보낸다).
- **지우기 전파:** 원본이 자식부터 지워지므로 원격 사본도 자식부터 지워진다. 받는 쪽에도 §3.7의 `backup_in_use` 규칙이 있다. janitor가 지우는 24시간 넘은 `receiving` 사본은 자식이 없다(자식은 `ready` 부모에만 붙는다).
- **oplctl·웹:** 복제 상태에 "체인 n개 중 k개 보냄" 진행을 보인다(Task 메시지). README DR 절차를 고친다.

## 5. 오류

| 코드 | 언제 |
|---|---|
| 409 `backup_in_use` | 증분 백업의 부모를 지우려 함 |
| 409 `cluster_upgrading` | `incremental-backup` 기능이 없는 노드가 있는데 증분을 요청하거나 `full_every > 0` 작업을 만듦 |
| 409 `replica_parent_missing` | (받는 쪽) 증분 사본의 부모 사본이 없거나 다름 |
| Task 실패 "backup chain is broken: …" | 복원할 체인의 백업이 없거나 맞지 않음 |

전체로 돌아가는 것은 오류가 아니다(`full_reason`).

## 6. 시험

- **opl-qemu:** QMP 명령 직렬화(`block-dirty-bitmap-add`·`-remove`, `blockdev-add`·`-del`, `blockdev-backup`, `transaction` 안의 비트맵)와 `query-named-block-nodes`의 비트맵 해석.
- **opl-driver:** `MockHypervisor`에 비트맵 상태(추가·지우기·inconsistent 주입·증분 작업 실패 주입)를 둔다. `qemu-it`로 실제 QEMU에서 확인한다: 전체 → 쓰기 → 증분이 바뀐 블록만 담고, 체인 복원이 원본과 같다(`qemu-img compare`). 꺼진 VM의 `qemu-img bitmap`도 확인한다.
- **opl-api:**
  - 부모 고르기의 각 전체 이유
  - `backup_in_use`
  - 보존 규칙의 조상 보호와 자식부터 지우기
  - `full_every`
  - 스냅샷 되돌리기가 비트맵을 지움
  - 버전 1·2 매니페스트
  - 기능 확인
  - 두 `Env` 증분 복제: 체인 순서, `replica_parent_missing` 뒤 다시 보내기, 옛 원격 거절
- **opld E2E (`qemu-it`):** 실행 중 VM의 전체 → 증분 → 복원 내용 확인. 두 클러스터 증분 복제 뒤 원격에서 복원.
- **oplctl:** 파싱·fake_api. **웹:** 대화상자·목록·작업 폼.

## 7. 알려진 한계

- 꺼진 VM은 늘 전체 백업이다. 매일 백업하는 꺼진 VM은 매일 전체를 쓴다(백업 작업이 꺼진 VM을 건너뛰는 선택은 없다).
- raw 디스크(LVM-thin·ZFS·RBD)의 비트맵은 VM을 다시 켜면 사라진다. 다시 켠 뒤 첫 백업은 전체다.
- 같은 VM을 두 풀에 번갈아 백업하면 비트맵이 하나라 늘 전체가 된다.
- 체인 중간 백업은 지울 수 없다(자식부터). 병합은 없다.
- 증분 파일만으로는 복원할 수 없다. opLite 밖에서 `qemu-img`로 열려면 체인을 직접 명시해야 한다(README에 예).
- opLite 밖에서 디스크를 고치면 비트맵이 틀릴 수 있다. 그 뒤 한 번은 전체로 백업해야 한다.

## 8. 구현(계획 40)

- QEMU(`opl-qemu`): `transaction`의 동작 `TransactionAction`(`SnapshotInternal`·`BitmapAdd`·`IncrementalBackup`), `query_block_nodes`(`query-named-block-nodes` flat의 노드·드라이버·`dirty-bitmaps` → `BlockNode`·`BitmapInfo{name, recording, inconsistent, persistent}`), `block_dirty_bitmap_add`·`_remove`, `blockdev_add_qcow2`(backing 없는 대상), `blockdev_del`, `query_jobs`·`job_cancel`·`job_dismiss`. 증분 작업은 `blockdev-backup`에 **`sync: bitmap` + `bitmap: opl-{B}` + `bitmap-mode: never`**, `compress: true`, `auto-dismiss: false`다 — QEMU는 `sync: incremental`에 `bitmap-mode: on-success`만 받으므로 §3.5의 3번(`sync: incremental` + `never`)은 그대로 쓸 수 없다. 뜻(B 뒤로 바뀐 블록만 쓰고 B 비트맵은 건드리지 않음)은 같다.
- 하이퍼바이저(`opl-driver`): `snapshot_save_with_bitmap(vm, tag, bitmap)`(내부 스냅샷과 비트맵을 transaction 하나로, 메모리 없음), `backup_bitmaps(vm) -> Vec<DiskBitmaps{node, bitmaps}>`(디스크 노드 `disk{i}` 순서), `add_bitmaps(vm, name)`(블록 풀이 멈춘 동안), `remove_bitmaps(vm, keep)`(`opl-` 중 `keep`이 아닌 것, 없는 것은 무시), `incremental_backup(vm, parent, new, &[BackupTarget{node, path, size}])`(§3.5의 2~6: 앞선 백업이 남긴 `opl-backup-*` 작업·노드를 먼저 치움, 대상이 디스크 노드와 순서·수가 다르면 `InvalidSpec`, 실패하면 나머지 `job-cancel`, dismiss·`blockdev-del`, 성공하면 부모 비트맵을·실패하면 새 비트맵을 지움; 대상 파일은 만들지도 지우지도 않음 — Task가 한다). `blockdev-del`은 대상 이미지의 메타데이터(L2)를 쓰고 플러시하므로(닫기 전 사본은 비어 보인다) 닫지 못하면 작업이 성공했어도 백업 실패다(닫은 뒤에 부모 비트맵을 지울지 정하므로 부모 비트맵이 남아 다음도 같은 부모에서 증분). `blockdev-backup` transaction(게스트 I/O가 끝나기를 기다림)과 `blockdev-del`은 긴 시간 제한(`SNAPSHOT_TIMEOUT`)이고, dismiss 실패는 작업 오류를 덮지 않는다. 비트맵은 qcow2 디스크면 `persistent`. `clear_backup_leftovers(vm)`는 남은 `opl-backup-*` 작업·노드를 치운다(남은 작업이 있으면 내부 스냅샷이 "Need a root block node"로 실패한다). `job-cancel` 오류는 무시하고(그 사이에 끝난 작업) 취소 뒤 `CANCEL_WAIT`(30초)까지만 기다린다 — 넘으면 오류(스냅샷이 영원히 막히지 않게; 스냅샷·전체 백업은 경고만, 증분은 실패). 마이그레이션 능력 `dirty-bitmaps`는 보내는 VM의 디스크에 persistent가 아닌 `opl-` 비트맵(raw 디스크 — RBD·공유 LVM 등)이 있을 때만 켠다: qcow2의 persistent 비트맵은 공유 이미지로 넘어가고, 능력을 켠 채 마이그레이션이 실패하면 원본 이미지에 in-use로 남거나 사라진다(받는 쪽은 켜 둔다). qcow2와 raw 디스크가 섞인 VM은 능력이 켜져 qcow2 비트맵도 스트림으로 가므로, 마이그레이션이 실패하면 qcow2 비트맵을 잃어 다음은 전체다(안전 — `block-bitmap-mapping`으로 고르지 않음). inconsistent인 `opl-` 비트맵(QEMU가 비정상으로 끝나 이미지에 in-use로 남은 것 — 있으면 `migrate`·`block_resize`·`qemu-img resize`가 "Bitmap … is inconsistent and cannot be used"로 실패)은 새로 띄운 VM의 QMP 연결 직후, `migrate` 전, `block_resize` 전에 지운다(실패는 경고). 저장소의 오프라인 `resize_volume`은 qcow2면 먼저 `opl-` 비트맵을 모두 지운다. `MockHypervisor`는 VM·디스크별 비트맵(꺼지면 persistent만 남음, 가짜 마이그레이션은 옮기지 않음)과 `set_bitmaps`·`set_bitmap_inconsistent`·`fail_next_incremental`·`incremental_backups()`·`leftover_clears()`를 둔다.
- 저장소: `import_chain(pool, id, files /*자식부터*/, format)`(dir·NFS·LVM-thin·공유 LVM·ZFS·RBD가 `import_volume`과 같은 길로 `qemu-img convert`에 `json:{"driver":"qcow2","file":{…},"backing":{…,"backing":null}}` 원본 하나를 줌), `create_backup_target(dest, size)`(`qemu-img create -f qcow2`), `set_backup_bitmap(pool, id, format, name)`(꺼진 qcow2: 다른 `opl-` 비트맵을 지우고 `qemu-img bitmap --add`, raw는 할 일 없음), `drop_backup_bitmaps(pool, id, format)`.
- 타입: `BackupSpec.incremental`·`max_chain: Option<u32>`(작업의 한도 — `TaskOp::VmBackup`에 필드를 더하지 않음), `BackupStatus.parent`·`full_reason`, `BackupManifest.parent`, `MANIFEST_VERSION_INCREMENTAL = 2`(전체 백업은 버전 1 그대로), `backup::BITMAP_PREFIX = "opl-"`·`bitmap_name(id)`, `BackupJobSpec.full_every`·`BackupJobPatch.full_every`, 노드 기능 `incremental-backup`. 새 필드는 모두 기본값이면 직렬화에서 빠진다.
- 백업 Task(`tasks/backup.rs`): `choose_parent`가 §3.3을 차례로 보고 이유 `VM is stopped`, `no earlier backup of this VM in pool "P"`, `no dirty bitmap on disk{i}`, `dirty bitmap on disk{i} is inconsistent`(`recording`이 아닌 것도), `dirty bitmaps do not match a ready backup in pool "P"`(디스크마다 `opl-` 비트맵이 하나가 아니거나 이름이 다르거나 같은 VM·풀의 `ready` 백업이 아님), `disks changed since backup "B"`(VM 스펙의 볼륨 이름 순서와 지금 가상 크기(`volume_info`)를 부모와 비교), `backup "P" is stored on another node`(부모의 `status.node`가 이 노드가 아님 — 공유가 아닌 백업 풀에서 VM이 옮겨짐), `files of backup "P" are missing on this node`(이 노드의 `backup_dir(root, P)`에 그 id의 매니페스트가 없음), `chain already has N incremental backup(s)`(`max_chain`)를 `full_reason`에 적는다. 부모와 이유는 `pending` 때 적으므로 실패한 증분도 `parent`가 남는다. 부모를 적은 뒤 부모를 다시 읽어 없거나 `ready`가 아니거나 끝나지 않은 `BackupDelete` Task가 있으면(부모가 적히기 전에 확인을 지난 지우기) `backup "P" is being deleted`로 전체로 바꿔 다시 적는다. `ready`로 바꾸기 직전에 부모가 없어졌으면 새 비트맵을 지우고 "parent backup "P" was deleted during the backup"로 실패한다. 증분: 디스크마다 `disk{i}.qcow2.part`를 만들고 `incremental_backup` 뒤 이름을 바꾸고 그 파일과 디렉터리를 fsync한 뒤 버전 2 매니페스트를 쓴다. 실패하면 실행기의 정리가 디렉터리를 지운다. 전체: 실행 중 qcow2는 `snapshot_save_with_bitmap`(실패하면 — 진행 중일 수 있는 오류가 아니면 — 경고하고 비트맵 없이 `snapshot_save`로 한 번 더; 그 백업은 비트맵을 남기지 않아 다음은 전체), 블록 풀은 멈춘 동안 `add_bitmaps`, 꺼진 VM은 내보낸 뒤 `set_backup_bitmap` — 블록 풀과 꺼진 VM의 비트맵 실패는 경고만(다음이 전체일 뿐). 실행 중 전체가 성공하면 다른 `opl-` 비트맵을 지우고, 실패하면 새 비트맵만 지운다(앞 백업의 비트맵이 모든 디스크에 같은 하나면 남김). 실행 중인 VM이면 `take_tags` 시작(실행 중 전체 백업과 스냅샷 만들기)과 실패한 백업의 정리(`discard`)에서 `clear_backup_leftovers`를 부른다(실패는 경고).
- 롤링 업그레이드: `cluster::missing_feature`(`require_feature`와 같은 판단)로 기능 `incremental-backup`이 없는 노드(위트니스 포함)가 하나라도 있으면 Task는 비트맵을 만들지 않고 증분 요청은 `some nodes do not support incremental backups yet`(다른 이유보다 먼저)로 전체다. 옛 노드는 스냅샷 되돌리기 전에 비트맵을 지우지 못하기 때문이다(아래).
- 스냅샷 되돌리기: VM을 끈 뒤 디스크를 **되돌리기 전에** 볼륨마다 `drop_backup_bitmaps`를 부른다(§3.4의 "되돌린 볼륨마다 지운다"보다 앞). qcow2에 이름 있는 dirty bitmap이 있으면 `qemu-img snapshot -a`와 메모리 스냅샷에서 시작(`snapshot-load`)이 "Device has active dirty bitmaps"로 실패한다(QEMU 11에서 확인). 스냅샷 만들기·지우기는 비트맵이 있어도 된다.
- API: `CreateBackupRequest.incremental`(409 `cluster_upgrading` — `require_feature`), `backups::start_backup(state, vm, pool, node, name, job, incremental, max_chain)`. 지우기는 `backups::parent_in_use`로 다른 백업(`failed`가 아닌 것 — 만드는 중인 증분 포함)의 부모면 409 `backup_in_use` `backup "B" is the parent of N incremental backup(s); delete them first`(`force`여도), 지우기 Task도 같은 확인으로 실패한다. 증분 백업 복원은 고른 노드에 기능이 있어야 한다(409 `cluster_upgrading` `node "X" does not support incremental backups yet; upgrade it first`). 백업 작업의 `full_every > 0`은 기능이 필요하고(409), `replicate`와 함께면 400 `replicating incremental backups is not supported yet`.
- 복원: `read_manifest`가 버전 1·2를 받는다. `read_chain`이 부모를 거슬러 체인을 만들고(조상마다 같은 풀에 `ready`, 이 노드에서 읽을 수 있음, 매니페스트의 id·부모가 기록과 같음, 디스크 수·가상 크기가 같음 — 아니면 `backup chain is broken: …`, 볼륨을 만들기 전에), 디스크마다 체인의 파일을 자식부터 `check_import_source`한 뒤 `import_chain`. 전체 백업은 그대로 `import_volume`.
- 보존(`backup_jobs::retention`): 지울 백업을 지금처럼 고른 뒤, 남는 백업(`failed`가 아닌 모든 백업 — 다른 작업·손으로 만든 것도)의 조상을 빼고 조상이 많은 것(자식)부터 둔다. 자식이 아직 있는 부모는 `backup_in_use`로 거절되고 다음 확인에 지운다. 스케줄러는 `full_every > 0`이면 `incremental: true`, `max_chain = full_every`로 백업을 청한다.
- 복제: 계획 40에서는 증분 백업을 복제하지 않는다. `POST /backups/{key}/actions/replicate`는 증분이면 400 `invalid_request` `incremental backups cannot be replicated yet`(가장 가까운 기존 코드), 복제 Task도 같은 확인, 작업의 복제 후보에서 뺀다. 계획 41이 푼다.
- oplctl: `backup create --incremental`(줄 때만 `"incremental": true` — 옛 서버는 모르는 필드를 거절; 출력 `backup N of vm V created (incremental)` 또는 `(full: <이유>)`), `backup list`의 `TYPE` 열(`SIZE` 뒤, `incr`/`full`, 증분의 `SIZE`는 자기 파일 크기), `backup get`(JSON 그대로)에 증분이면 `chain: {backups: [전체부터 이것까지 이름], stored_bytes: 합}`(목록에 없는 조상은 id로 적고 멈춤, 이미 본 백업이 다시 나와도 멈춤; 체인의 모든 백업이 `ready`이고 목록에 있을 때만 `stored_bytes` — 웹과 같게 부분 합은 보이지 않음), `backup-job create|set --full-every N`, `backup-job list`의 `SCHEDULE`에 `, full every N`.
- 웹: 백업 만들기 "증분(가능하면)"(켰을 때만 보냄), 백업 표의 "종류" 열(전체·증분 배지, "부모:" 링크 `#backup-<id>`, "전체로 한 이유: <full_reason>"(서버 문구 그대로)), 증분 크기 아래 "체인 <크기>", 작업 폼 "전체 백업 사이의 증분 백업 수"(원격 복제와 함께면 막음)와 작업 표 일정 칸 "증분, 증분 n개마다 전체", `errors.backup_in_use`, ko·en. 백업 상세 화면은 없어 §3.9의 "상세"는 표 안에 보인다. 증분 백업의 "원격으로 복제"는 이유("증분 백업은 아직 원격으로 복제하지 못합니다")와 함께 막는다.
- 시험: opl-qemu 직렬화·해석; opl-driver 가짜 QMP(transaction 내용, 증분 성공·실패 뒤 취소와 정리, 대상 닫기 실패는 백업 실패, dismiss 실패보다 작업 오류, 남은 작업 치우기와 `job-cancel` 실패 무시), `MockHypervisor` 비트맵 수명, 체인 `json:`·LVM 체인 가져오기 인자, `storage-it`(꺼진 qcow2 비트맵 교체·지우기 뒤 되돌리기, 손으로 만든 체인 가져오기가 원본과 같음), `qemu-it` 실제 QEMU(`incremental_backup_holds_only_changed_blocks` — 꺼서 쓴 뒤에도 비트맵 유지, 바뀐 블록만, 체인이 디스크와 같음(`qemu-img compare`), 없는 비트맵 실패, `raw_disk_bitmaps_do_not_survive_a_restart`, 라이브 마이그레이션 뒤 비트맵 유지, 비정상 종료 뒤 inconsistent 비트맵을 지워 `block_resize`·마이그레이션이 되고 실패한 마이그레이션 뒤에도 비트맵이 깨끗이 저장됨, 오프라인 resize, 0 쓰기·discard 뒤 체인이 디스크와 같음 — VM을 SIGTERM으로 깨끗이 끄고 다시 켠 뒤 비트맵이 있음을 먼저 확인); opl-api 타입 JSON, API(`incremental`·기능, `backup_in_use`, 증분 복원 노드 기능, `full_every`, 증분 복제 400), `backup_tasks.rs`(`storage-it`: 이유마다 전체/증분(다른 노드의 부모, 매니페스트 없는 부모, 지우는 중인 부모 포함), 비트맵 교체, 실패 정리, 남은 작업 치우기, 비트맵 추가 실패에도 전체 성공, 실제 `qemu-img` 체인 복원과 중간에서 복원, 깨진 체인, 부모 지우기, 체인 한도, 옛 노드가 있으면 비트맵 없음, 버전 3 거절), `snapshot_tasks.rs`·`snapshot_disks.rs`(되돌리기 전에 비트맵 지움), `backup_job_runs.rs`(`full_every` 작업의 백업이 `incremental`·`max_chain`), 보존 단위 시험; oplctl 파싱·fake_api; opld E2E `tests/backup.rs` `incremental_backup_of_a_running_vm_restores_the_same_disk`(`qemu-it`: 실행 중 전체 → 끄고 `qemu-io`로 쓰고 다시 켬 → 증분(버전 2, 작은 파일, `chain`, `TYPE`) → 복원이 원본과 같음 → 부모 먼저 지우기 거절); 웹 Vitest.
- 설계와 다른 점: §3.5 3번의 `sync`(위). §3.2의 이유 예시 대신 위 문구. §3.4의 되돌리기 비트맵 지우기는 되돌린 뒤가 아니라 앞. §3.7의 `backup_in_use`는 `failed` 자식을 세지 않는다(부모가 지워진 `failed` 자식도 그대로 지울 수 있다). 기능이 없는 노드가 있으면 비트맵을 만들지 않는다(§3.2에 없던 규칙). 증분 백업은 복제하지 않는다(계획 41). 마이그레이션 능력 `dirty-bitmaps`는 raw 디스크의 비트맵이 있을 때만 켜고(섞인 VM은 실패하면 qcow2 비트맵을 잃어 다음은 전체), 비정상 종료로 inconsistent가 된 비트맵은 VM을 켤 때 지운다(§3.4에 없던 규칙). 공유가 아닌 백업 풀에서 다른 노드에 있는 부모는 쓰지 않는다(§3.3에 없던 규칙). §2의 플랫폼에 CI의 Ubuntu 24.04(QEMU 8.2)가 더 있고, 쓰는 QMP·`qemu-img` 기능(`bitmap-mode`, `compress`, `persistent`, `qemu-img bitmap`)은 그 버전에도 있다.
