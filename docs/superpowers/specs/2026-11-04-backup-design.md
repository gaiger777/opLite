# opLite 백업 설계 (하위 프로젝트 7: 계획 27·28)

작성일: 2026-11-04
상위 문서: `2026-10-20-opl-node-core-design.md`(하위 프로젝트 분해 §1.1), `2026-10-26-snapshots-design.md`(백엔드별 스냅샷), `2026-10-28-shared-storage-nfs-design.md`(풀·NFS 마운트)

## 1. 목표와 범위

처음 분해에서 하위 프로젝트 7은 백업(Proxmox vzdump·PBS, VMware VADP)이다. 지금 스냅샷은 VM 디스크와 같은 저장소 안에 있어 저장소를 잃으면 함께 잃고, VM을 지우면 스냅샷도 사라진다. 다른 저장소에 VM을 통째로 떠 두고 언제든 새 VM으로 되살리는 길이 없다.

| 계획 | 내용 |
|---|---|
| 27 | 백업과 복원: 풀의 `backups` 표시, `Backup` 리소스, 실행 중·멈춘 VM의 전체 백업(압축 qcow2 + 매니페스트), 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험 |
| 28 | 백업 작업과 보존: `BackupJob` 리소스(대상 VM, 풀, 시각, 보존 규칙), 리더의 스케줄러, 노드당 동시 실행 제한, 보존 규칙에 따른 정리, 작업 상태 |

넣지 않는 것: 증분·변경 블록 추적(QEMU dirty bitmap), 중복 제거 청크 저장소(PBS), 백업 암호화, 원격(S3 등) 저장소, 게스트 에이전트로 파일 시스템 얼리기(백업은 crash-consistent), 파일 단위 복원, 기존 VM 위에 덮어 복원, 테이프.

### 견준 길

- **방식: 스냅샷 + 이미지 복사(채택) vs QEMU 블록 작업(`blockdev-backup`)과 dirty bitmap.** 지금 코드에는 백엔드마다 스냅샷을 만들고(`snapshot_save`·`snapshot_many`) 스냅샷을 읽는 길(`volume_source(tag)` + `qemu-img convert -U`)이 이미 있고 복제가 그 길을 쓴다. 백업도 같은 길이면 새 QMP 기능 없이 모든 백엔드(dir·NFS·LVM-thin·ZFS·RBD)에서 같은 코드가 된다. 블록 작업과 증분은 나중 과제로 남긴다.
- **저장소: 기존 풀에 표시(채택) vs 새 `BackupStore` 리소스.** dir·NFS 풀은 이미 노드마다 마운트·상태·용량을 관리한다(Proxmox의 "content: backup"과 같은 모양). 새 리소스를 두면 그 코드를 되풀이한다.
- **형식: 디스크마다 압축 qcow2 + JSON 매니페스트(채택) vs 자체 아카이브(vma).** 백업 파일이 표준 qcow2라 opLite 없이도 `qemu-img`로 열 수 있다.
- **복원: 새 VM(채택) vs 기존 VM 덮어쓰기.** 새 VM으로 만들면 지금 VM을 잃을 위험이 없다. 덮어쓰기는 나중에(새 VM으로 복원한 뒤 이름을 바꾸면 된다).

## 2. 백업과 복원 (계획 27)

### 2.1 백업 풀

- `StoragePoolSpec.backups: bool`(기본 false, false면 직렬화에서 뺌). dir·NFS 풀에서만 켤 수 있다(그 밖은 400). 켠 풀에도 지금처럼 VM 디스크를 둘 수 있다.
- 백업 파일은 `{풀 루트}/backups/{백업 id}/`에 둔다: `manifest.json`, `disk{i}.qcow2`, UEFI 변수가 있으면 `vars.fd`.
- 백업이 남아 있는 풀은 지울 수 없다(409 `pool_in_use`, 지금의 볼륨 검사 옆). 백업 표시를 끄는 것도 백업이 남아 있으면 409.

구현(계획 27): `StoragePoolSpec.backups`(serde 기본 false, false면 뺌, OpenAPI 선택 필드)와 `PoolError::BackupsNotSupported`(400 `invalid_request`, "only dir and nfs pools can store backups")는 풀 만들기와 `PATCH /storage-pools/{key}`(`PoolPatch { shared?, backups? }`, 보낸 것만 바꾼다)가 함께 검사한다. `check_no_backups`(opl-api `backups.rs`)가 풀 지우기와 `backups` 끄기를 409 `pool_in_use`("pool {name:?} holds backups")로 막는다 — `pending`·`failed` 백업도 센다. 루트는 driver `pool_root`(dir은 경로, NFS는 `{data_dir}/pools/{풀 id}`)이고 Task는 `check_pool_root`로 NFS 풀이 실제로 마운트됐는지 본다(빈 마운트 지점에 쓰지 않는다). 디렉터리는 `opl_types::backup::backup_dir(root, id)` = `{root}/backups/{id}`.

### 2.2 `Backup` 리소스

```yaml
kind: backup
name: web-20261104-0300            # 기본: "{VM 이름}-{UTC yyyymmdd-HHMMSS}"
spec:
  vm: <VM id>                      # VM을 지워도 백업은 남는다
  vm_name: web                     # 만든 때의 이름(표시용)
  pool: backups                    # 백업 풀 이름
  vm_spec: { ... }                 # 만든 때의 VmSpec 전체
  job: null                        # 계획 28: 이 백업을 만든 BackupJob id
status:
  phase: pending | ready | failed
  created_at: ...
  disks: [ { name: disk0, file: disk0.qcow2, virtual_bytes: ..., stored_bytes: ... } ]
  uefi_vars: true
  error: null
```

- 만들기는 `POST /vms/{key}/backups`(본문 `{ pool, name? }`) → Task `VmBackup { backup }`. 목록 `GET /backups`(`?vm=`), 읽기 `GET /backups/{key}`, 지우기 `DELETE /backups/{key}` → Task `BackupDelete`(파일을 지우고 리소스를 지움), 복원 `POST /backups/{key}/restore`(본문 `{ name, pool, node? }`) → Task `BackupRestore`.
- 권한: 만들기·복원은 VM 쓰기, 지우기는 VM 쓰기(그 VM이 없으면 admin), 읽기는 모두.

구현(계획 27): `opl_types::backup`의 `Backup = Resource<BackupSpec { vm, vm_name, pool, vm_spec, job: Option<Uuid> }, BackupStatus { phase: BackupPhase(Pending|Ready|Failed), created_at: Option<u64>(유닉스 초), disks: Vec<BackupDisk { name, file, virtual_bytes, stored_bytes }>, uefi_vars, error }>`, `Kind::Backup`(`backup`, watch 종류에도 있다). `job`은 계획 28을 위한 자리로 지금은 늘 None(빼고 쓴다). `MANIFEST_VERSION = 1`, `MANIFEST_FILE = "manifest.json"`, `BackupManifest { version, backup, vm, vm_name, vm_spec, disks, uefi_vars, created_at }`(알 수 없는 필드는 받는다), `default_backup_name(vm_name, at)`(VM 이름을 47자까지 줄여 63자를 넘지 않는다). API는 opl-api `backups.rs`: `POST /vms/{key}/backups { pool, name? }`(202 Task `vm-backup`, `pending` 리소스를 먼저 만든다), `GET /backups[?vm=]`(`vm`은 id나 이름, 지운 VM은 백업에 남은 이름으로), `GET /backups/{key}`, `DELETE /backups/{key}`(202 Task `backup-delete`, `failed`도), `POST /backups/{key}/restore { name, pool, node? }`(202 Task `backup-restore`, 대상은 미리 정한 새 VM id). 권한은 설계와 달리 단순하게 했다: 만들기는 VM 쓰기, 지우기·복원은 `Kind::Backup` 쓰기이고 둘 다 operator 이상이다(VM이 지워진 백업도 operator가 지운다). 오류: 백업 풀이 아님 400, 실행 중인데 스냅샷을 담지 못하는 디스크 409 `vm_running`("… stop it to back it up"), 시작·정지 중이면 409 `vm_busy`, 이름 409 `name_taken`, 돌릴 노드가 없으면 409 `no_node`, 진행 중 Task가 있는 백업 지우기·지우는 중 복원 409 `backup_busy`, `ready`가 아닌 백업 복원 409 `backup_not_ready`, 복원할 VM 스펙의 네트워크·보안 그룹이 없으면 400. 진행 중인 `vm-backup`은 VM을 붙잡는다(`check_no_snapshot_task`: 시작·정지·수정·지우기 409 `vm_busy`, 강제 정지는 받는다; 마이그레이션도 바쁨으로 본다).

### 2.3 백업 Task (`VmBackup`)

VM의 노드(멈춘 VM이면 `spec.node`, 없으면 백업 풀과 디스크 풀을 모두 쓸 수 있는 노드 하나)에서 VM 큐로 돈다(스냅샷·복제와 같은 큐라 겹치지 않는다).

1. 백업 풀이 이 노드에서 준비됐는지, 모든 디스크 볼륨이 이 노드에서 읽히는지(`check_local`) 본다. 아니면 실패.
2. 실행 중이면 임시 스냅샷을 만든다(태그 = 백업 id, 메모리 없음): qcow2 디스크는 `blockdev-snapshot-internal-sync` transaction, 블록 백엔드는 짧게 멈추고 `snapshot_many`(지금 스냅샷 코드의 길 그대로). 공유 LVM 풀처럼 스냅샷이 안 되는 디스크가 있으면 실행 중 백업은 409(만들기 때 API가 거절) — 멈춘 VM은 된다.
3. 디스크마다 `volume_source(tag)`로 읽어 `qemu-img convert -c -O qcow2 [-U] [-l snapshot.name=…]`로 `{dir}/disk{i}.qcow2.part`에 쓰고 끝나면 이름을 바꾼다. UEFI 변수 파일은 그대로 복사(실행 중이면 스냅샷 때 복사한 것).
4. `manifest.json`(형식 버전 1, 백업 id, VM id·이름, `vm_spec`, 디스크 목록과 크기, 만든 시각)을 쓰고 status를 `ready`로.
5. 임시 스냅샷은 성공·실패와 상관없이 지운다(실패하면 지금의 스냅샷 태그 청소가 나중에 지운다 — 태그 접두사로 구분).
6. 실패하면 디렉터리를 지우고 status `failed`와 오류를 남긴다(리소스는 남겨 사용자가 본다; 지우기로 치운다).

진행률은 남기지 않는다(Task 모델에 진행률이 없다). 백업 하나에 시간이 오래 걸릴 수 있으므로 Task 시간 제한은 두지 않는다.

구현(계획 27): opl-api `tasks/backup.rs`의 `backup`(VM 큐, 시간 제한 없음)과 `delete`(백업 id 큐, 디렉터리가 없어도 된다), 실패 정리 `discard`. API는 실행 중인 VM이면 그 노드, 꺼진 VM이면 `spec.node`, 없으면 백업 풀과 모든 디스크를 읽을 수 있는 온라인 노드 가운데 이름이 앞선 것을 Task 노드로 고른다. 실행 중 임시 스냅샷은 스냅샷 Task의 태그 찍기를 뺀 `snapshot.rs`의 `take_tags`(qcow2는 메모리 없는 internal snapshot transaction, 블록 디스크는 멈춤·`snapshot_many`·재개)를 쓰고, 태그는 백업 id의 simple 문자열(하이픈 없음)이다. 블록 태그 청소(`sweep_block_tags`)는 Snapshot 리소스가 없는 UUID 모양 태그를 지우되 `pending` 백업의 태그는 둔다. 디스크는 driver `Storage::export_volume(src, force_share, dest)`(`qemu-img convert -c -O qcow2`, 원본을 여는 인자는 복제와 같음, `.part`에 쓰고 이름 바꿈; `Storages`가 모든 백엔드의 `volume_source`를 받는다)로 `disk{i}.qcow2`에, UEFI 변수는 `vars.fd`에, 매니페스트도 `.part` 뒤 이름을 바꿔 쓴다. `stored_bytes`는 파일 크기다. opld는 `TaskRunnerConfig.mount_root`(`{data_dir}/pools`)를 넘긴다. 실제 시험 `crates/opld/tests/backup.rs`(`qemu-it`, macOS HVF와 CI linux-kvm): opld 하나, dir 풀 `local`과 백업 풀 `bk`. 표식을 쓴 qcow2 디스크의 VM을 실행 중에 백업하면 VM은 같은 pid로 돌고 디스크에 태그가 남지 않으며, 백업 디렉터리에 `disk0.qcow2`(backing 없음, 표식 있음)·`manifest.json`(버전 1)·`vars.fd`가 있다. 원본 VM과 볼륨을 지운 뒤 복원한 VM `copy`의 `copy-disk0`에 같은 표식과 백업 때의 UEFI 변수가 있고 켜진다. 백업을 지우면 디렉터리가 사라진다. 두 번째 시험: qcow2와 raw 디스크의 VM은 실행 중 백업이 409 `vm_running`, 꺼진 뒤 백업 → 원본을 바꿔도 복원한 두 디스크(dir 풀이라 qcow2)가 백업 때의 내용, 백업이 남은 풀 지우기 실패.

알려진 틈(계획 27): Task 없이 남은 `pending` 백업(Task를 만들기 전에 실패했거나, 만들기가 실패한 뒤 리소스를 거두는 되돌림마저 실패했거나, Task가 사라진 경우)은 저절로 치우지 않고 그 이름을 붙잡는다 — 지우기로 치운다. 죽었거나 클러스터에서 빠진 노드의 끝나지 않은 백업 Task는 그 백업을 "바쁨"으로 붙잡아 백업 지우기(409 `backup_busy`)와 풀 지우기가 막힐 수 있다 — 그 노드를 되살리거나 Task가 실패로 처리되면 풀린다. 풀의 `shared` 바꾸기와 백업 만들기가 동시에 오면 만들기가 본 `shared`와 Task가 본 값이 달라 백업에 기록된 노드(`status.node`)가 틀릴 수 있는 좁은 틈이 있다. 실행 중 백업이 임시 스냅샷을 지우지 못하면 qcow2 디스크에는 태그가 남을 수 있다(블록 디스크만 태그 청소가 지운다; `qemu-img snapshot -d`로 지운다). 백업 디렉터리의 파일시스템 호출(만들기·복사·지우기)에는 시간 제한이 없어 멈춘 NFS hard 마운트에서는 Task가 멈춘다.

### 2.4 복원 Task (`BackupRestore`)

- 백업 풀과 대상 풀을 모두 쓸 수 있는 노드(본문 `node`가 있으면 그것)에서 돈다. 새 VM의 이름·대상 풀은 본문에서.
- 매니페스트를 읽어 백업의 `vm_spec`을 바탕으로 새 VM을 만든다: 디스크마다 새 볼륨(대상 풀이 qcow2를 받으면 qcow2, 아니면 raw — 복제의 `copy_format`과 같다)에 `qemu-img convert`로 쓴다, 새 MAC, `node` 없음, `power: stopped`, HA 끔, 보안 그룹·네트워크는 그대로(없으면 400 — API가 먼저 검사), UEFI 변수 복사. 실패하면 만든 볼륨을 지운다(복제의 `discard`와 같다).

구현(계획 27): `tasks/backup.rs`의 `restore(ctx, task, id, name, pool)`(백업 id 큐, 시간 제한 없음 — 같은 백업의 지우기와 겹치지 않는다). 매니페스트의 `version`이 `MANIFEST_VERSION`이 아니거나 `backup`이 백업 id와 다르거나 디스크 수가 VM 스펙과 다르면 실패한다. 디스크 파일 이름은 디렉터리 밖을 가리킬 수 없고(경로 성분이 있으면 실패), 가져오기 검사(`check_import_source`)로 backing file이 있는 이미지를 받지 않는다. 새 볼륨은 `{새 VM 이름}-disk{i}`(복제의 `volume_name`, 겹치면 번호), 형식은 `copy_format`(블록 풀이면 raw, 아니면 qcow2), `status.node`는 이 노드다. 볼륨은 만들기 전에 Task의 `Made`에 적고, 실패하면 복제와 같은 정리가 지운다. VM은 복제에서 뺀 `clone::create_vm(ctx, task, new)`로 맨 마지막에 만든다(새 MAC, 노드·예약 없음, HA 끔, `power: stopped`). 매니페스트 버전 오류는 Task 실패로 알린다(API는 매니페스트를 읽지 않으므로 400이 아니다). 알려진 틈: 볼륨을 `ready`로 만든 뒤 VM을 만들기 전에 opld가 죽으면 볼륨이 남는다(복제도 같다) — `oplctl volume delete`로 지운다.

### 2.5 API·oplctl·웹

- oplctl: `backup create VM --pool P [--name N]`, `backup list [--vm VM]`(열 `NAME VM POOL PHASE CREATED SIZE`), `backup get`, `backup delete`, `backup restore BACKUP --name NEW --pool P [--node N]`. `pool create … --backups`, `pool set NAME --backups true|false`.
- 웹: VM 상세에 백업 탭(목록, 지금 백업, 복원 대화상자), 백업 목록 페이지(VM이 지워진 백업 포함), 풀 만들기·편집의 "백업 저장" 스위치.

구현(계획 27, 리뷰 반영): 공유가 아닌 백업 풀(dir, `shared: false`)의 백업은 쓴 노드에만 있으므로 `BackupStatus.node: Option<Uuid>`(없으면 뺌)를 둔다. 백업 Task는 공유가 아닌 풀이면 디렉터리를 만들기 전에 자기 노드를 적고(실패한 백업도 그 노드에서 지운다), 공유 풀이면 비워 둔다. API는 `status.node`가 있으면 지우기·복원 Task를 그 노드에 둔다: 복원 본문의 `node`가 다르면 409 `backup_on_other_node`("backup {name:?} is stored on node {n:?}"), 그 노드가 오프라인이면 409 `node_offline`(복원 본문의 `node`도 같다), 그 노드에서 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`. 지우기 Task는 디렉터리가 없을 때 공유 풀이거나 `status.node`가 없거나 이 노드면 이미 지운 것으로 보고, 다른 노드면 실패한다. 백업이 남은 풀의 `shared` 바꾸기도 409 `pool_in_use`. 풀이 저장소에서 사라진 백업의 지우기는 이 노드의 Task가 리소스만 지우고(경고 로그), 풀은 있는데 파일을 지울 노드가 없으면 409이되 `DELETE /backups/{key}?force=true`면 API가 리소스만 바로 지우고 200 `{ message }`(파일은 남는다고 알린다). 만들기는 리소스를 만든 뒤 풀을 다시 읽어 없어졌거나 `backups`가 꺼졌으면 리소스를 지우고 409 `pool_unavailable`(풀 지우기와 엇갈림). 실행 중 VM 백업 요청은 스냅샷 API의 `check_disks`로 방식이 섞인 디스크를 409 `snapshot_unsupported`로 막는다. `GET /backups?vm=NAME`은 지금 그 이름의 VM 백업과 `vm_name == NAME`인 백업을 함께 준다. 쓰기 보장: `export_volume`은 `qemu-img convert -t writeback`(닫을 때 flush), `vars.fd`와 매니페스트 `.part`는 `sync_all`, 백업 디렉터리를 만든 뒤 `backups` 디렉터리와 풀 루트, 이름을 바꾼 뒤 백업 디렉터리도 fsync하고 나서 `ready`. 블록 디스크를 위해 멈춘 VM을 재개하지 못하면 한 번 더 해 보고, 그래도 실패하면 내보내기 전에 실패한다(임시 태그는 지운다). 블록 태그 청소는 끝나지 않은 `VmBackup` Task가 있는 `pending` 백업의 태그만 둔다. 지우기 Task는 같은 백업의 끝나지 않은 복원 Task가 클러스터에 있으면 실패한다("backup is being restored"). 복원은 백업 리소스의 `vm_spec`으로 VM을 만들고, 매니페스트의 `vm_spec`·디스크 수가 다르면 실패("backup manifest does not match the backup record"), 새 스펙은 `VmSpec::validate`를 통과해야 하며, 백업 디렉터리의 디스크·`vars.fd`는 `symlink_metadata`로 일반 파일인지 본다. 다른 노드가 쓴 백업은 복원 Task도 거절한다.

구현(계획 27): oplctl `backup create VM --pool P [--name N]`(출력 "backup {name} of vm {vm} created"), `backup list [--vm VM]`(열 `NAME VM POOL PHASE CREATED SIZE`, SIZE는 `ready`일 때 `stored_bytes`의 합), `backup get`, `backup delete`, `backup restore BACKUP --name NEW --pool P [--node N]`(출력 "vm {NEW} restored from backup {BACKUP}"). 만들기·복원은 Task를 24시간(`BACKUP_WAIT`)까지 기다린다. `pool create … --backups`, `pool set NAME [--shared|--no-shared] [--backups true|false]`. 웹은 사이드바 "백업" 화면(`/backups`, 모든 백업, VM이 지워졌으면 이름만), VM 상세의 "백업" 탭(목록·지금 백업(백업 풀 고르기)·복원 대화상자), 스토리지 풀 화면의 "백업 저장" 스위치, Task 종류 문구, watch 종류 `backup`, ko·en 문구.

## 3. 백업 작업과 보존 (계획 28)

### 3.1 `BackupJob` 리소스

```yaml
kind: backup-job
name: nightly
spec:
  vms: { all: true } | { names: [web, db] } | { labels: { tier: prod } }
  pool: backups
  schedule: { at: "03:00", days: [mon, tue, wed, thu, fri, sat, sun] }   # UTC
  keep: { last: 3, daily: 7, weekly: 4, monthly: 6 }                     # 0이면 그 단위는 보존하지 않음
  enabled: true
status:
  last_run: { started_at, finished_at, ok: 12, failed: 1 }
  next_run: ...
  errors: [ { vm, error } ]
```

### 3.2 스케줄러 (리더)

- 1분마다: 사용할 수 있고 `next_run`이 지난 작업마다 대상 VM을 고르고 VM마다 백업을 요청한다(Task는 VM의 노드에서 돈다). 같은 VM의 백업이 이미 진행 중이면 건너뛴다. 노드마다 동시에 도는 백업 Task는 `max_concurrent_backups`(opld 설정, 기본 2)까지 — 나머지는 다음 차례.
- 리더가 바뀌면 새 리더가 `status`로 이어 간다(같은 날 두 번 돌지 않게 `last_run`을 본다).
- 보존: 작업의 백업(`spec.job == 작업 id`, `ready`)을 Proxmox `prune` 규칙으로 고른다 — 최신 `last`개, 그다음 날·주·달마다 가장 최근 하나씩 `daily`·`weekly`·`monthly`개. 남지 않는 백업은 지우기 Task로. 실패한 백업은 다음 성공 뒤 지운다. 손으로 만든 백업(`job` 없음)은 건드리지 않는다.
- oplctl `backup-job create|list|get|set|delete|run-now`, 웹 백업 작업 페이지.

## 4. 오류와 경계

- 백업 도중 VM이 멈추거나 다른 노드로 옮겨지는 일은 VM 큐로 막는다(마이그레이션·스냅샷과 같은 큐). HA 복구로 노드가 죽으면 그 노드의 백업 Task는 지금의 Task 실패 처리로 실패한다.
- 백업 풀이 가득 차면 `qemu-img` 실패 → 백업 실패, 디렉터리 정리.
- 백업은 crash-consistent다(전원을 갑자기 끈 것과 같다). 데이터베이스 같은 앱은 게스트 안에서 일관성을 맞춰야 한다(README).
- 형식 버전이 다른 매니페스트는 복원하지 않는다(400).

## 5. 시험

| 층 | 무엇 | 어디 |
|---|---|---|
| 단위 | 매니페스트 읽고 쓰기, 백업 이름, 보존 규칙(날짜 표), 작업 다음 실행 시각 | macOS, CI |
| API | 백업 풀 표시 규칙, 백업·복원·지우기 요청 검사, 공유 LVM 실행 중 409, 풀 지우기 409, 작업 CRUD | macOS, CI |
| Task | Mock 하이퍼바이저·dir 풀로 멈춘·실행 중 백업, 실패 정리, 복원(형식 바꾸기), 지우기 | macOS, CI(`storage-it`) |
| 실제 | opld 하나·실제 QEMU: dir 풀의 VM을 실행 중 백업 → VM 지우기 → 복원한 VM이 같은 디스크 내용으로 켜짐(디스크에 쓴 표식 확인), 백업 작업이 시각에 돌고 보존 규칙대로 지움 | macOS(TCG/HVF), CI(KVM) |

## 6. 계획 나누기

- 계획 27: 풀 `backups` 표시 → `Backup` 타입·API → 백업 Task(멈춘·실행 중) → 복원·지우기 Task → oplctl·웹 → 실제 시험·문서.
- 계획 28: `BackupJob` 타입·API → 보존 규칙(순수 함수) → 리더 스케줄러·동시 실행 제한 → oplctl·웹 → 실제 시험·문서.
