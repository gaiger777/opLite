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
