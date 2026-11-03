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

### 2.3 백업 Task (`VmBackup`)

VM의 노드(멈춘 VM이면 `spec.node`, 없으면 백업 풀과 디스크 풀을 모두 쓸 수 있는 노드 하나)에서 VM 큐로 돈다(스냅샷·복제와 같은 큐라 겹치지 않는다).

1. 백업 풀이 이 노드에서 준비됐는지, 모든 디스크 볼륨이 이 노드에서 읽히는지(`check_local`) 본다. 아니면 실패.
2. 실행 중이면 임시 스냅샷을 만든다(태그 = 백업 id, 메모리 없음): qcow2 디스크는 `blockdev-snapshot-internal-sync` transaction, 블록 백엔드는 짧게 멈추고 `snapshot_many`(지금 스냅샷 코드의 길 그대로). 공유 LVM 풀처럼 스냅샷이 안 되는 디스크가 있으면 실행 중 백업은 409(만들기 때 API가 거절) — 멈춘 VM은 된다.
3. 디스크마다 `volume_source(tag)`로 읽어 `qemu-img convert -c -O qcow2 [-U] [-l snapshot.name=…]`로 `{dir}/disk{i}.qcow2.part`에 쓰고 끝나면 이름을 바꾼다. UEFI 변수 파일은 그대로 복사(실행 중이면 스냅샷 때 복사한 것).
4. `manifest.json`(형식 버전 1, 백업 id, VM id·이름, `vm_spec`, 디스크 목록과 크기, 만든 시각)을 쓰고 status를 `ready`로.
5. 임시 스냅샷은 성공·실패와 상관없이 지운다(실패하면 지금의 스냅샷 태그 청소가 나중에 지운다 — 태그 접두사로 구분).
6. 실패하면 디렉터리를 지우고 status `failed`와 오류를 남긴다(리소스는 남겨 사용자가 본다; 지우기로 치운다).

진행률은 남기지 않는다(Task 모델에 진행률이 없다). 백업 하나에 시간이 오래 걸릴 수 있으므로 Task 시간 제한은 두지 않는다.

### 2.4 복원 Task (`BackupRestore`)

- 백업 풀과 대상 풀을 모두 쓸 수 있는 노드(본문 `node`가 있으면 그것)에서 돈다. 새 VM의 이름·대상 풀은 본문에서.
- 매니페스트를 읽어 백업의 `vm_spec`을 바탕으로 새 VM을 만든다: 디스크마다 새 볼륨(대상 풀이 qcow2를 받으면 qcow2, 아니면 raw — 복제의 `copy_format`과 같다)에 `qemu-img convert`로 쓴다, 새 MAC, `node` 없음, `power: stopped`, HA 끔, 보안 그룹·네트워크는 그대로(없으면 400 — API가 먼저 검사), UEFI 변수 복사. 실패하면 만든 볼륨을 지운다(복제의 `discard`와 같다).

### 2.5 API·oplctl·웹

- oplctl: `backup create VM --pool P [--name N]`, `backup list [--vm VM]`(열 `NAME VM POOL PHASE CREATED SIZE`), `backup get`, `backup delete`, `backup restore BACKUP --name NEW --pool P [--node N]`. `pool create … --backups`, `pool set NAME --backups true|false`.
- 웹: VM 상세에 백업 탭(목록, 지금 백업, 복원 대화상자), 백업 목록 페이지(VM이 지워진 백업 포함), 풀 만들기·편집의 "백업 저장" 스위치.

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
