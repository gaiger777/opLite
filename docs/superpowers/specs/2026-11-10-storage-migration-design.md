# opLite 로컬 디스크 라이브 마이그레이션 설계 (하위 프로젝트 13: 계획 42)

작성일: 2026-11-10
상위 문서: `2026-10-30-live-migration-design.md`(라이브 마이그레이션, 계획 20·21), `2026-11-09-node-maintenance-design.md`(노드 비우기, 노드 기능 목록 `features`), `2026-11-09-incremental-backup-design.md`(dirty bitmap `opl-`), `2026-10-29-shared-storage-lvm-thin-design.md`·`2026-10-29-shared-storage-zfs-design.md`(노드 로컬 블록 풀)

## 1. 목표와 범위

라이브 마이그레이션(계획 20)은 디스크가 모두 공유 풀(`is_shared`: NFS·RBD·공유 LVM, `shared = true`인 디렉터리 풀)에 있어야 한다. 노드 로컬 풀(`shared = false`인 디렉터리 풀, LVM-thin, ZFS)에 디스크가 하나라도 있으면 `POST /vms/{key}/migrate`는 409 `migration_unsupported`이고, 노드 비우기(계획 39)는 그 VM을 "disks on a local pool"로 남긴다. 단일 노드로 시작해 로컬 디스크를 쓰는 클러스터는 유지보수 때 VM을 꺼야 한다. 두 설계 문서가 "넣지 않는 것"으로 남긴 "디스크까지 복사하는 마이그레이션"(Proxmox `qm migrate --online --with-local-disks`, Storage vMotion)을 채운다. 계획 하나(계획 42)로 한다.

| 계획 | 내용 |
|---|---|
| 42 | 실행 중인 VM의 로컬 풀 디스크를 대상 노드에 새로 만든 볼륨으로 복사하며 옮기기: 대상 QEMU의 NBD 내보내기(TLS), 원본의 `blockdev-mirror`, 스위치오버 전 미러 완료, 볼륨 기록 옮기기와 원본 지우기, 실패 정리, 진행률, 대상 풀 고르기, 노드 비우기가 로컬 디스크 VM도 옮김, 노드 기능 `storage-migration`, API·oplctl·웹, 실제 시험 |

넣지 않는 것: 꺼진 VM의 디스크 옮기기(오프라인 이동), 같은 노드 안에서 디스크만 다른 풀로 옮기기(`qm move-disk`), 디스크마다 다른 대상 풀(지도), 스냅샷이 있는 VM의 로컬 디스크 복사(스냅샷은 복사하지 않는다 — §3.8), 마이그레이션 취소(지금처럼 시간 제한으로 끝난다), 마이그레이션 전용 네트워크, 공유 풀 디스크 옮기기(그대로 둔다), 다른 QEMU 버전·CPU 사이(계획 20과 같다), macOS(HVF) 노드.

### 견준 길

- **디스크 복사: 대상 NBD 서버 + 원본 `blockdev-mirror`(채택) vs QEMU 블록 마이그레이션(`migrate -b`, `block` 능력) vs opld가 gRPC로 중계 vs 멈춘 채 `qemu-img convert`.** 블록 마이그레이션은 QEMU 8.2에서 deprecated 되고 9.1에서 없어졌다(지원 플랫폼 QEMU 10·11에 없다). gRPC 중계는 일관성(복사 중 바뀐 블록 다시 보내기)을 opld가 다시 만들어야 하고 데이터가 opld를 두 번 지난다. 멈춘 채 복사는 디스크 크기만큼 멈춘다. NBD + mirror는 libvirt(`VIR_MIGRATE_NON_SHARED_DISK`)·Proxmox(`--with-local-disks`)가 쓰는 방식이고, QEMU가 직접 TLS로 주고받으므로 계획 20의 "QEMU가 직접 TCP+TLS" 결정과 같다.
- **스위치오버 맞추기: `pause-before-switchover`에서 미러를 끝냄(채택) vs 메모리 마이그레이션이 끝난 뒤 미러를 끝냄.** 끝난 뒤에는 원본 QEMU가 이미 블록 장치를 놓았으므로(inactivate) 남은 변경을 보낼 수 있는지가 QEMU 버전과 미러 대상 노드의 처리에 달린다. QEMU는 이 경우를 위해 `pause-before-switchover` 능력(게스트를 멈추고 장치 상태를 보내기 전, 블록 장치를 놓기 전에 `pre-switchover`에서 기다림)을 둔다. 게스트가 멈춘 그 시점에 미러를 끝내면 대상 디스크가 원본과 정확히 같다.
- **미러 방식: `copy-mode: background`(QEMU 기본, 채택) vs `write-blocking`.** `write-blocking`은 게스트 쓰기마다 네트워크 왕복을 기다려 복사 내내 게스트 I/O가 느려진다. `background`는 `ready` 뒤의 뒤처짐을 스위치오버 때 메운다(그만큼 중단 시간이 는다, 보통 작다).
- **대상 풀: 같은 이름의 풀이 기본이고 요청에 `pool` 하나를 줄 수 있음(채택) vs 디스크마다 지도.** 로컬 풀은 이미 "노드마다 같은 이름"(LVM-thin의 VG·thin pool, ZFS 데이터셋을 관리자가 노드마다 같은 이름으로 만든다)이 기본이라 대개 아무것도 고를 필요가 없다. 풀이 다른 경우(디렉터리 풀을 노드마다 따로 만든 클러스터, 한 호스트의 두 노드 시험)는 `pool` 하나로 충분하다. 지도는 필요가 보이면 더한다.
- **볼륨 정체성: 같은 Volume 리소스(id·이름)를 두고 위치만 바꿈(채택) vs 대상에 새 볼륨을 만들고 VM 스펙의 디스크 이름을 바꿈.** 로컬 볼륨의 위치는 지금도 `VolumeStatus.node`(만든 노드)이고 파일·LV·zvol 이름은 볼륨 id(`{id}.qcow2`, `opl-{id}`)다. 같은 id로 대상에 만들고 확정 뒤 `status.node`(풀이 바뀌면 `spec.pool`·`spec.format`)만 바꾸면 VM 스펙·백업 매니페스트의 볼륨 이름·이름 유일성이 그대로이고, 증분 백업의 "disks changed since backup" 판단(볼륨 이름·가상 크기)도 그대로 맞는다.
- **복사 계획의 기록: VM `status.migration`에 디스크 계획을 담음(채택) vs 볼륨마다 표시.** `Migrating` 쓰기 하나에 함께 들어가 원자적이고, 확정(`spec.node`를 대상으로)의 순간 그 계획이 "적용된 것"으로 뜻이 바뀐다. 볼륨 기록은 확정 뒤 그 계획으로 고친다(§3.5).

## 2. 지금 있는 것 (계획 41 끝)

- 마이그레이션 API(`opl-api/src/vms.rs` `migrate_vm` → `start_migration(state, vm, target, source)`): `vm_lock` 안에서 검사하고 `TaskOp::VmMigrate { node }` Task를 원본 노드 S에 만든다. 디스크마다 볼륨의 풀이 `is_shared`가 아니면 409 `migration_unsupported` "disk … is not on a shared storage pool…". 이어서 HVF(하이퍼바이저 이름 `qemu-hvf`) 409 `migration_unsupported`, 위트니스·HA·온라인·유지보수, 아키텍처, `scheduler::node_fits`(그 안의 `Inventory::reaches_disks`가 공유가 아닌 풀의 볼륨은 `volume.status.node`가 그 노드일 때만 닿는다고 본다) 409 `no_capacity`, `free_memory_mib` 409 `no_capacity`.
- 원본 Task(`opl-api/src/tasks/migrate.rs` `migrate`): `Hold::begin`(`ReconcilerHandle::begin_migration`) → `mark_migrating`(`phase = Migrating`, `status.migration = MigrationStatus { node, task }`, revision 확인) → `check_no_other_task` → `set_disks_shared` → `send`(RPC `MigrateIncoming` → `hv.migrate` → RPC `MigrateIncomingWait`) → `commit`(`spec.node = T`, 확정) → `finish`(status, `firmware_node`, `stop_source`, `deactivate_disks`). 실패는 `roll_back`(표시 지우기 → `MigrateIncomingAbort` → `cancel_migration` → `resume`). 남은 QEMU는 `sweep`(시작 때와 `tag_sweep` 30분마다)이 끈다. 일반 600초 제한 밖이고 단계마다 제한이 있다(`[migration] timeout` 30분, RPC 120·180·60초).
- 받는 쪽(`opl-api/src/incoming.rs` `IncomingMigrations`, opl-peer의 `MigrationReceiver`): QEMU 버전 비교, `Migrating` 기록을 10초까지 기다림, `incoming_spec`(`opl-reconciler/src/resolve.rs`, `Activate::Incoming` — `share_volume`으로 켬) → `hv.start_incoming(spec, task, listen_host, ports)`(`-incoming defer`, 받는 중 표시 파일 `incoming`에 Task id, TLS 객체 `opl-mig-tls` server·verify-peer, 빈 포트에서 `migrate-incoming`). 응답 `MigrateIncomingResponse { port }`.
- 드라이버(`opl-driver/src/qemu.rs`): `setup_migration`(TLS 객체를 매번 지우고 다시 만듦, 능력 `events`·`auto-converge`·`dirty-bitmaps`, `tls-creds`·`tls-hostname`·`max-bandwidth`), `migrate`(`remove_inconsistent_bitmaps`, `volatile_bitmaps`가 참일 때만 `dirty-bitmaps`, `wait_migration`, 시간 초과면 `migrate_cancel`), `finish_incoming`(`cont`, 표시 지움), `cancel_migration`. 디스크 블록 노드는 `disk{i}-file`(프로토콜)과 `disk{i}`(형식)이다(`opl-qemu/src/cmdline.rs`). VM 정의에 CD-ROM 장치는 없다(설치 이미지도 볼륨으로 붙는 디스크다).
- 볼륨: `VolumeSpec { pool, size_bytes, format }`(`deny_unknown_fields`), `VolumeStatus { phase, allocated_bytes, attached_to, node }`(`serde(default)`). 저장소(`Storage`)는 `create_volume`(있으면 `VolumeExists`), `delete_volume`, `activate_volume`·`deactivate_volume`, `volume_info { virtual_bytes, allocated_bytes }`를 둔다. 크기는 512바이트 배수(API), LVM은 익스텐트로 올린다.
- 노드 기능(`opl_types::node::FEATURES` = `node-drain`·`incremental-backup`·`incremental-replication`)과 `cluster::require_feature`(없는 노드가 있으면 409 `cluster_upgrading`).
- `TaskOp`는 `deny_unknown_fields`이므로 옛 노드는 모르는 필드가 든 Task를 읽지 못한다. `TaskStatus { state, error }`는 `serde(default)`이고 모르는 필드를 무시한다. Task에 진행률 필드는 없다.

## 3. 설계

원본 노드 S, 대상 노드 T. "로컬 디스크"는 볼륨의 풀이 `is_shared`가 아닌 디스크, "옮길 디스크"는 이번 마이그레이션이 복사하는 로컬 디스크다(공유 풀 디스크는 지금처럼 두 노드가 함께 연다).

### 3.1 API

- `POST /vms/{key}/migrate` 본문 `{ "node": "<노드>", "pool": "<풀>"? }`. `pool`(선택)은 옮길 디스크를 둘 T의 풀이다. 없으면 디스크마다 지금과 같은 이름의 풀이다. 공유 풀도 고를 수 있다(로컬 → 공유로 옮기며 마이그레이션 — 경로는 같다).
- `TaskOp::VmMigrate { node, pool: Option<String> }`(`serde(default, skip_serializing_if = "Option::is_none")`). `pool`이 없으면 직렬화가 지금과 같아 공유 풀만의 마이그레이션은 옛 노드도 읽는다.
- `start_migration`의 공유 풀 검사를 디스크 계획(§3.2)으로 바꾼다. 검사 순서(앞부분은 지금 그대로):
  1. 404(VM·노드), 같은 노드 400, `vm_not_running`, `vm_busy`(`check_not_busy`·`check_no_snapshot_task`·`check_no_resize_task`).
  2. `pool`이 있는데 로컬 디스크가 없으면 400 `invalid_request` "VM … has no disks on node-local pools; pool applies only to them". `pool`이 없는 풀이면 404 `not_found`.
  3. 로컬 디스크가 있으면 `cluster::require_feature(cluster, "storage-migration")`(409 `cluster_upgrading`).
  4. 로컬 디스크가 있고 VM에 스냅샷(`SnapshotSpec.vm`이 이 VM)이 하나라도 있으면 409 `migration_unsupported` "VM … has snapshots and disk … is on node-local pool …; snapshots are not copied, delete them first".
  5. HVF 409 `migration_unsupported`(지금 그대로), 위트니스·HA·`node_unavailable`·아키텍처(그대로).
  6. `scheduler::node_fits_with(spec, inv, node, moves)`: `node_fits`와 같되 옮길 디스크는 `reaches_disks` 대신 대상 풀이 그 노드에서 `pool_usable_on`인지 본다. 아니면 409 `no_capacity` "node … cannot use storage pool … for disk …". 지금의 `node_fits`는 `moves`가 빈 `node_fits_with`다.
  7. 메모리 409 `no_capacity`(그대로). 공간은 API가 실제 할당량을 모르므로 Task가 본다(§3.3의 3).

### 3.2 디스크 계획

- 옮길 디스크마다 `DiskMove { volume: Uuid, pool: String, format: ImageFormat }`(대상 풀과 대상 형식). 원본 풀·형식은 볼륨 기록에 그대로 있다(확정 전에는 바꾸지 않는다).
- 대상 형식: 대상 풀 백엔드가 지금 형식을 받으면 그대로(디렉터리·NFS는 qcow2·raw), 아니면 raw(LVM-thin·ZFS·RBD·공유 LVM은 raw만). 그래서 바뀌는 경우는 qcow2 → raw뿐이다.
- 계획은 Task가 `TaskOp`의 `pool`과 그때의 볼륨·풀 기록으로 다시 만든다(API 검사와 Task 시작 사이의 변경을 따른다). `MigrationStatus`에 `disks: Vec<DiskMove>`(`serde(default, skip_serializing_if = "Vec::is_empty")`)를 더해 `mark_migrating`이 `Migrating`과 같은 쓰기로 적는다. 비면 기록이 지금과 같다.
- 크기: 대상 볼륨은 원본의 실제 가상 크기(S의 `volume_info().virtual_bytes`)로 만들고, 만든 뒤 T의 `volume_info().virtual_bytes`가 같아야 한다. 백엔드가 올림해 달라지면(예: 4 MiB 배수가 아닌 qcow2 디스크를 LVM-thin으로) 게스트가 보는 디스크 크기가 바뀌므로 실패한다 "disk … would change size on pool … (A → B bytes)". 같은 이름의 같은 백엔드 풀은 늘 같다.

### 3.3 흐름

Task `VmMigrate { node: T, pool }`은 S에서 돈다. 공유 풀 디스크만 있으면 지금 흐름 그대로다. 로컬 디스크가 있으면:

1. **S: 시작.** `Hold::begin`, `mark_migrating`이 디스크 계획을 함께 적는다. `check_no_other_task`, 공유 디스크만 `set_disks_shared`.
2. **S: 기록.** 이 노드의 마이그레이션 디스크 기록 파일(§3.6)에 역할 `source`와 옮길 볼륨(원본 풀·id·형식)을 쓴다(fsync).
3. **S: 공간 확인.** 대상 풀마다 옮길 디스크의 `volume_info().allocated_bytes` 합이 T가 알린 그 풀의 `PoolState.available_bytes` 이하인지 본다. 아니면 실패 "not enough space in storage pool … on node …: need N, available M"(되돌리기만, T는 부르지 않음).
4. **S → T: 받을 준비(`MigrateIncoming`).** 요청에 `disks: Vec<IncomingDisk { volume, size_bytes }>`(옮길 디스크의 원본 가상 크기, `serde(default)`)를 더한다. T는 지금 검사(버전, `Migrating`을 10초까지 기다림)에 더해 `status.migration.disks`가 요청과 같은지 보고, 이 VM의 남은 디스크 기록을 먼저 정리한 뒤(§3.6 — 같은 VM이 되돌아올 때 남은 원본 사본) 디스크마다: T의 기록 파일(역할 `target`)에 적고 → `create_volume(대상 풀, id, size, 형식)`(이미 있으면 기록에서 빼고 실패 "volume … already exists on node … in pool …; is the pool really node-local?" — 남의 파일을 지우지 않는다) → 크기 확인(§3.2). 그 뒤 `incoming_spec`(옮길 디스크는 계획의 풀·형식으로 `activate_volume`, 공유 디스크는 지금처럼 `share_volume`)으로 `start_incoming(spec, task, listen_host, ports, exports)`를 부른다. 응답 `MigrateIncomingResponse { port, nbd_port: Option<u16> }`. 실패하면 띄운 QEMU를 끄고 만든 볼륨을 지우고 기록을 지운다.
5. **S: 디스크 복사.** `hv.mirror_disks(id, targets, opts)`(§3.4)로 옮길 디스크마다 미러를 시작하고 모두 `ready`가 되기를 기다린다. 진행률을 Task에 쓴다(§3.7). 시간 제한 `[migration] disk_timeout`(기본 12시간)과 정체 감지(10분 동안 복사량이 늘지 않음)를 넘거나 작업이 실패하면 실패.
6. **S: 메모리 보내기.** `hv.migrate(id, target, opts)`에 `opts.switchover_mirrors = true`: `pause-before-switchover`로 보내고, `pre-switchover`에서 미러를 끝낸 뒤 `migrate-continue`(§3.4). 시간 제한은 지금처럼 `[migration] timeout`.
7. **S → T: 받기 확인, S: 확정.** 지금 그대로(`MigrateIncomingWait`, `commit`의 `spec.node = T`). 확정의 순간 디스크 계획이 적용된 것이 된다.
8. **S: 마무리.** `finish`가 status를 쓰기 **전에** 볼륨 기록을 고친다(§3.5). 그 뒤 지금처럼 status·`firmware_node`·`stop_source`. 원본 QEMU가 꺼졌으면 옮긴 디스크의 원본 볼륨을 S에서 `deactivate_volume` 뒤 `delete_volume(원본 풀, id, 원본 형식)`으로 지우고 S의 기록 파일을 지운다(지우기 실패는 경고, 정리가 다시 한다). Task 성공.
9. **T: 넘겨받기.** 지금처럼 T의 워커가 넘겨받는다. `clear_migration`이 확정된(`spec.node == T`) VM의 `status.migration.disks`가 있으면 볼륨 기록 고치기(§3.5, 멱등)를 먼저 하고(실패하면 `retry_interval` 뒤 다시, 그동안 디스크를 다시 해석하지 않는다) 표시를 지운다. `finish_incoming`이 NBD 서버를 멈춘 뒤 `cont`한다. T의 기록 파일은 정리가 지운다(§3.6).

**두 QEMU가 함께 도는 일은 없다**(지금 규칙 그대로): 대상은 `-S`로 떠서 확정 뒤에만 `cont`된다. 대상 디스크는 원본 게스트가 멈춘 `pre-switchover`에서 원본과 같아진다.

### 3.4 QEMU

- **대상(T), `start_incoming`의 `exports: &[usize]`(옮길 디스크 순서 번호).** 비어 있지 않으면 받는 QEMU를 띄운 뒤 TLS 객체 `opl-nbd-tls`(server, `verify-peer=on`, 같은 `{state_dir}/migration-tls` 파일)를 만들고, `[migration] ports`에서 마이그레이션 포트와 다른 빈 포트를 골라(`incoming_ports` 잠금 안에서 `pick_port` 두 번) `nbd-server-start { addr: inet listen_host:port, tls-creds: opl-nbd-tls }`, 디스크마다 `block-export-add { type: nbd, id: opl-export-{i}, node-name: disk{i}, name: disk{i}, writable: true }`. 결과는 `IncomingPorts { migration, nbd: Option<u16> }`. `-incoming defer`로 연 이미지는 비활성이지만 `block-export-add`가 내보낼 노드를 활성화한다(QEMU가 비공유 스토리지 마이그레이션을 위해 둔 동작, 10.0의 `allow-inactive` 기본값 false도 같다 — 계획 42의 환경 탐색에서 8.2·10·11로 확인).
- **원본(S), `mirror_disks(id, targets: &[MirrorTarget { disk: usize, host, port, export, tls_hostname }], opts: MirrorOptions { speed, timeout, stall, progress })`.** TLS 객체 `opl-nbd-tls`(client)를 만들고(마이그레이션의 `opl-mig-tls`와 이름을 나눈다 — NBD 노드가 쥔 객체는 `setup_migration`이 지우고 다시 만들 수 없다), 디스크마다 `blockdev-add { driver: nbd, node-name: opl-mirror-{i}, server: inet host:port, export: disk{i}, tls-creds: opl-nbd-tls, tls-hostname }`과 `blockdev-mirror { job-id: opl-mirror-{i}, device: disk{i}, target: opl-mirror-{i}, sync: full, copy-mode: background, auto-dismiss: false, speed }`. `speed`는 `[migration] bandwidth_mib`을 디스크 수로 나눈 값(0이면 없음). 작업 상태는 `query-jobs`(`current-progress`·`total-progress`, `status: ready`)를 1초마다 읽는다. 원본의 0·할당 안 된 영역은 NBD `WRITE_ZEROES`(unmap)로 가므로 데이터가 흐르지 않고 대상이 성기게 남는다.
- **스위치오버(`migrate`의 `switchover_mirrors`).** 능력 `pause-before-switchover`를 켜고 보낸다. 상태가 `pre-switchover`가 되면(`MigrationState`에 더함) 미러마다 `block-job-cancel { force: false }`(ready 작업은 이 시점까지 맞춘 뒤 `concluded`로 끝난다 — 게스트가 멈춰 새 쓰기가 없다)를 보내고 모두 오류 없이 끝나기를 기다린 뒤(`CANCEL_WAIT` 30초 × 디스크 수 한도) `job-dismiss`·`blockdev-del opl-mirror-{i}`, `migrate-continue { state: pre-switchover }`, 그다음 지금처럼 `completed`를 기다린다. 미러 하나라도 오류면 `migrate_cancel`하고 실패(원본은 `paused`로 남을 수 있어 지금의 되돌리기가 `cont`한다).
- **`dirty-bitmaps`.** 옮길 디스크에 `opl-` 비트맵이 있으면(persistent여도) 켠다 — 대상 이미지는 다른 파일이므로 qcow2의 persistent 비트맵도 스트림으로 가야 한다. 공유 디스크의 지금 규칙(`volatile_bitmaps`)과 OR. 비트맵은 같은 노드 이름 `disk{i}`로 간다(§3.8).
- **되돌리기와 남은 작업.** `cancel_migration`이 `opl-mirror-*` 작업을 `block-job-cancel { force: true }`로 끝내고 dismiss·`blockdev-del`도 한다(보내는 중이 아니어도). 그래서 `roll_back`과 워커의 `clear_migration`(S의 opld가 복사 중에 죽은 뒤)이 남은 미러를 치운다. 남은 미러가 있으면 스냅샷·백업이 실패하므로 `clear_backup_leftovers`도 `opl-mirror-*`를 함께 본다.
- **대상 넘겨받기.** `finish_incoming`이 `cont` 전에 `nbd-server-stop`(내보내기가 없으면 오류 무시)을 한다.
- `MockHypervisor`: 미러 시작·진행·ready·스위치오버를 기록하고(`mirrors()`), 실패 주입(`fail_next_mirror`, `stall_next_mirror`, 스위치오버 때 미러 오류), 가짜 진행률. 디스크 내용은 복사하지 않는다(볼륨 파일은 실제 저장소가 만들고 지운다).

### 3.5 볼륨 기록 바꾸기

- 함수 `opl_reconciler::resolve::apply_disk_moves(cluster, vm)`: VM이 계획의 노드로 확정됐을 때(`spec.node == Some(migration.node)`)만, 계획의 디스크마다 볼륨의 `spec.pool`·`spec.format`이 다르면 `update_spec`(revision 확인)으로, `status.node`가 다르면 `update_status_checked`로 고친다. 이미 고쳐졌으면 할 일이 없다(멱등). 확정되지 않은 VM에는 아무것도 하지 않는다.
- 부르는 곳: S의 `finish`(status를 쓰기 전, `RECORD_TIMEOUT` 안에서 다시 시도), T의 워커 `clear_migration`(표시를 지우기 전). `status.migration`은 이 둘만 지우고(확정 뒤) 둘 다 먼저 고치므로, "확정됐는데 볼륨 기록이 옛 위치이고 계획도 없다"는 상태가 생기지 않는다. 고치는 동안(VM은 아직 `Migrating`) 다른 요청은 `vm_busy`다.
- 확정 전 실패의 `roll_back`과 S 워커의 `clear_migration`(확정 안 됨)은 볼륨 기록을 건드리지 않는다(바뀐 적이 없다).
- 그 뒤의 판단은 모두 지금 규칙이다: 스케줄러 `reaches_disks`는 새 `status.node`를, `runtime_spec`은 새 `spec.pool`·`spec.format`을 본다. `deactivate_idle_volumes`·`volumes_in_use`는 볼륨 id가 같으므로 이 노드로 오는 중인 VM의 디스크를 지금처럼 쓰는 중으로 본다.

### 3.6 실패와 정리

- **마이그레이션 디스크 기록 파일** `{data_dir}/migration-disks/{task}.json`(0600, 쓰고 fsync): `{ vm, task, role: source|target, volumes: [{ id, pool, format }] }`. 그 노드에서 opld가 만들었거나 지울 볼륨 파일의 유일한 근거다. Raft 기록만 보고 지우면 잘못 설정한 풀(`shared = false`인데 실제로 같은 경로)이나 한 호스트의 두 노드에서 남의 디스크를 지울 수 있다. T는 `create_volume` 전에 적고 `VolumeExists`면 그 항목을 뺀다.
- **확정 전 실패(S의 `roll_back`).** 지금 순서에 `cancel_migration`(남은 미러 포함)을 표시 지우기 바로 뒤로 당기고, `MigrateIncomingAbort`에서 T가 받는 QEMU를 끈 뒤 자기 기록의 볼륨을 `deactivate_volume`·`delete_volume`하고 기록을 지운다. S는 원본을 이어서 실행하고 S의 기록을 지운다(원본 볼륨은 그대로). Task 실패 문구는 단계의 오류(미러면 QEMU 작업 오류).
- **정리(`tasks/migrate.rs` `sweep`, 시작 때와 `tag_sweep`마다)에 기록 파일을 더한다.** 지우기 조건은 늘 "VM에 끝나지 않은 마이그레이션이 없고(`status.migration` 없음 또는 다른 Task) 볼륨 기록의 위치가 (이 노드, 기록의 풀)이 아니다"이다.
  - `target` 기록: VM이 이 노드로 확정됐거나 볼륨 위치가 이 노드이면 기록만 지운다(진짜 디스크). 아니면(되돌려졌거나 확정 전에 어느 쪽이 죽음) 기록의 볼륨을 지우고 기록을 지운다. VM·볼륨 리소스가 없으면 파일을 지운다(고아).
  - `source` 기록: VM이 아직 이 노드에 있으면(되돌려짐) 기록만 지운다. 다른 노드로 확정됐고 볼륨 위치가 이 노드가 아니면 원본 볼륨을 지우고 기록을 지운다(원본 opld가 확정 뒤 마무리 전에 죽음). 원본 QEMU(`postmigrate`)는 지금 정리가 먼저 끈다.
  - 받기(§3.3의 4)는 같은 VM의 기록을 이 규칙으로 먼저 정리한다(되돌아오는 VM이 남은 원본 사본에 `VolumeExists`로 막히지 않게).
- **중간에 죽으면:** S의 opld가 복사 중 죽음 — Task "interrupted", S 워커의 `clear_migration`이 미러를 치우고 원본을 이어서 실행, T의 정리가 받는 QEMU와 대상 볼륨을 지운다. T의 opld·호스트가 죽음 — NBD가 끊겨 미러가 실패하고 S가 되돌린다. T가 다시 뜨면 정리가 기록으로 대상 볼륨을 지운다(QEMU는 지금 규칙으로 끈다). 확정 뒤 S가 죽음 — T가 넘겨받으며 볼륨 기록을 고치고, S가 다시 뜨면 정리가 원본 볼륨을 지운다.

### 3.7 시간 제한과 진행률

- `[migration] disk_timeout = "12h"`(지금 `timeout`과 같은 자체 파서)는 미러가 모두 `ready`가 되기까지의 한도다. 따로 10분 동안 모든 미러의 `current-progress` 합이 늘지 않으면 실패한다("disk copy made no progress for 10m" — 끊긴 링크를 일찍 잡는다). 메모리 단계는 지금의 `timeout`(30분)이다. 넘으면 지금처럼 원본이 계속 돈다. `TaskRunnerConfig.migration_disk_timeout`.
- 취소는 없다(계획 20과 같다). 복사가 몇 시간 걸릴 수 있으므로 한계에 적고, 그동안 VM의 끄기·스냅샷·백업은 409 `vm_busy`, 예약 스냅샷은 `schedule_error`다.
- `TaskStatus.progress: Option<TaskProgress { stage: String, done_bytes: u64, total_bytes: u64 }>`(`skip_serializing_if`). 마이그레이션 Task가 5초마다(바뀌었을 때만) `stage: "copying-disks"`와 미러들의 합을, 메모리 단계에 들어가면 `stage: "migrating-memory"`(바이트는 복사 끝 값)를 쓴다. Task가 끝나면 지운다. 쓰기는 이 노드의 실행기만 하므로(`update_status`) 다른 쓰기와 부딪치지 않는다.

### 3.8 비트맵·증분 백업·스냅샷·UEFI

- **dirty bitmap.** 같은 형식으로 옮기면(qcow2 → qcow2, raw → raw) `dirty-bitmaps`로 `opl-` 비트맵이 대상 `disk{i}`로 가고(persistent 표시도), qcow2 대상은 닫을 때 이미지에 저장한다. 미러가 대상에 쓴 블록은 비트맵에 들어가지 않지만 대상 내용이 원본과 같으므로 "백업 B 뒤 바뀐 블록"이라는 뜻은 그대로다. qcow2 → raw로 바뀌는 디스크는 persistent 비트맵을 raw에 저장할 수 없으므로 미러 전에 그 디스크의 `opl-` 비트맵을 지운다(다음 백업은 전체, `full_reason` "no dirty bitmap on disk{i}"). 마이그레이션이 실패하면 계획 40의 섞인 경우처럼 원본 qcow2 비트맵을 잃을 수 있다(다음은 전체 — 안전). 환경 탐색에서 미러한 노드로 비트맵이 오고 대상 qcow2에 저장되는지 확인하고, 안 되면 옮길 디스크의 비트맵을 미러 전에 지우는 것으로 줄인다.
- **증분 백업 체인.** 백업 파일과 기록은 옮기지 않는다. 다음 증분은 `choose_parent`의 지금 규칙을 따른다: 백업 풀이 공유(NFS 등)이면 T에서 그대로 증분, S의 로컬 백업 풀이면 `backup "P" is stored on another node`로 전체. 볼륨 이름과 가상 크기가 같으므로 "disks changed" 이유는 생기지 않는다.
- **스냅샷.** `blockdev-mirror`는 활성 층만 복사한다(qcow2 내부 스냅샷, LVM-thin·ZFS 스냅샷은 대상에 없다). 스냅샷이 있는 VM의 로컬 디스크는 옮기지 않는다(§3.1의 4). 실패한 백업이 남긴 임시 태그는 복사되지 않고 원본과 함께 지워진다.
- **UEFI 변수.** 지금처럼 QEMU가 pflash를 옮기고 `finish`가 `firmware_node = T`를 쓴다(계획 21). 변수 파일은 볼륨이 아니므로 계획과 상관없다.
- **CD-ROM·ISO.** VM 정의에 없다. 설치 이미지는 볼륨이므로 로컬 풀에 있으면 다른 디스크처럼 복사된다.

### 3.9 노드 비우기

로컬 디스크 VM도 옮긴다(`tasks/drain.rs` `move_vm`).

- "disks on a local pool"로 남기던 검사를 지운다. 대신 로컬 디스크가 있으면: 기능 `storage-migration`이 없는 노드가 있으면 남김 "disks on a local pool (some nodes do not support storage migration yet)", 스냅샷이 있으면 남김 "disks on a local pool and snapshots".
- `pick_target`은 `node_fits_with(spec, inv, n, 같은 이름 풀의 계획)`을 쓴다 — 로컬 디스크의 같은 이름 풀을 쓸 수 있는 노드만. 없으면 남김 "no node can take its local disks". `pool`은 주지 않는다(같은 이름 규칙). 공간 부족은 Task가 실패로 알린다(그 메시지로 남김).
- 마이그레이션 Task를 기다리는 한도(`wait_migration`)는 로컬 디스크 VM이면 `disk_timeout + migration_timeout × 2 + 1분`이다.
- 차례와 한 번에 하나는 그대로다(복사가 대역폭을 다 쓰므로 동시에 해도 빨라지지 않는다).

### 3.10 혼합 버전

- 새 노드 기능 `storage-migration`(`FEATURES`에 더함). 로컬 디스크가 있는 마이그레이션과 `pool`은 위트니스를 포함한 모든 노드에 이 기능이 있어야 한다: 옛 노드는 `TaskOp::VmMigrate`의 `pool`, `MigrationStatus.disks`, `TaskStatus.progress`를 읽지 못하거나 버리고, 옛 S·T는 NBD·미러를 모른다. 공유 풀만의 마이그레이션은 직렬화가 같으므로 롤링 업그레이드 중에도 된다(계획 39의 "하나씩 비우고 올림"이 계속 된다 — 로컬 디스크 VM만 업그레이드를 마친 뒤 옮겨진다).
- RPC 요청의 `disks`, 응답의 `nbd_port`는 `serde(default)`다. 기능 확인 뒤에만 비어 있지 않다.

### 3.11 oplctl·웹

- oplctl: `vm migrate VM NODE [--pool POOL]`(줄 때만 `"pool"`을 보낸다 — 옛 서버는 모르는 필드를 400으로 거절). 기다리는 동안 Task의 `progress`가 5퍼센트 포인트 넘게 바뀌면 stderr에 `vm web: copying disks 37% (14.8 GiB of 40.0 GiB)`, 메모리 단계에 들어가면 `vm web: migrating memory`. 마지막 출력과 한도(`MIGRATE_WAIT` 24시간 — `disk_timeout` 12시간 + 메모리 30분보다 길다)는 그대로. `task get`은 `progress`를 JSON 그대로 보인다. 도움말에서 "디스크가 모두 공유 풀에 있어야 한다"를 고친다.
- 웹: `VmMigrateDialog`에 로컬 디스크 목록(볼륨, 풀, 크기)과 "대상 풀" 고르기("같은 이름의 풀"이 기본, 고른 노드에서 준비된 풀들), "디스크 N개(합 X GiB)를 복사합니다 — 오래 걸릴 수 있고 그동안 VM을 끄거나 스냅샷할 수 없습니다" 안내. 로컬 디스크와 스냅샷이 함께 있으면 이유와 함께 막는다. VM 상세는 `Migrating`일 때 그 `vm-migrate` Task의 진행률("디스크 복사 37% (14.8/40.0 GiB)", "메모리 옮기는 중"), Task 목록도 진행률을 보인다. 노드 비우기 대화상자의 고정 안내를 "로컬 디스크 VM은 같은 이름의 풀이 있는 노드로 디스크를 복사해 옮깁니다(스냅샷이 있으면 옮기지 못합니다)"로 바꾼다. 번역 키 ko·en(`vms.migrate.*`, `tasks.progress.*`, `nodes.drain.*`).
- README: "라이브 마이그레이션" 절에 "로컬 디스크 복사" 소절(흐름, `--pool`, `[migration] disk_timeout`, 포트 범위에서 NBD 포트도 쓴다, 스냅샷 제한, 비트맵·백업 체인), "노드 유지보수와 비우기" 절 고치기, 한계, 상태 줄. 설계 문서: 노드 코어 §1.1에 하위 프로젝트 13, 라이브 마이그레이션·노드 유지보수 설계의 "넣지 않는 것"에 계획 42 메모.

## 4. 오류

| 코드 | 언제 |
|---|---|
| 400 `invalid_request` | `pool`을 줬는데 로컬 디스크가 없음, 같은 노드 |
| 404 `not_found` | VM·노드·`pool`이 없음 |
| 409 `migration_unsupported` | 로컬 디스크가 있고 VM에 스냅샷이 있음, HVF 노드(그대로) |
| 409 `cluster_upgrading` | 로컬 디스크나 `pool`인데 `storage-migration` 기능이 없는 노드가 있음 |
| 409 `no_capacity` | 대상 풀(같은 이름 또는 `pool`)을 대상 노드에서 쓸 수 없음, 메모리(그대로) |
| 409 `vm_busy`·`vm_not_running`·`node_unavailable` | 지금 그대로(복사 중에도 `vm_busy`) |
| Task 실패 | 공간 부족, 크기가 바뀜, 대상에 같은 볼륨이 있음, 미러 오류·`disk_timeout`·정체, 스위치오버의 미러 오류, 지금의 마이그레이션 실패 |

## 5. 시험

- **opl-qemu:** `nbd-server-start`·`block-export-add`·`nbd-server-stop`, nbd `blockdev-add`(tls-creds·tls-hostname), `blockdev-mirror`, `block-job-cancel`(force), `migrate-continue`의 직렬화, `pre-switchover` 상태와 `query-jobs` 진행률 해석.
- **opl-driver:** 가짜 QMP로 원본 순서(TLS 객체 → NBD 노드 → 미러 → ready → 능력 `pause-before-switchover` → pre-switchover에서 cancel·dismiss·del → `migrate-continue` → completed), 미러 오류·시간 초과·정체에서 정리, 스위치오버 미러 오류에서 `migrate_cancel`, `cancel_migration`의 남은 미러 치우기, 받는 쪽 포트 둘과 내보내기, `finish_incoming`의 `nbd-server-stop`, 옮길 디스크 비트맵이 있을 때의 `dirty-bitmaps`와 qcow2 → raw의 비트맵 지우기. `MockHypervisor`의 미러 기록과 실패 주입. `qemu-it` 실제 QEMU(한 호스트의 QEMU 둘): 원본 qcow2를 NBD로 대상 qcow2·raw 파일에 미러하고 스위치오버 뒤 `qemu-img compare`가 같음, 복사 중 HMP `qemu-io`로 쓴 블록도 대상에 있음, 비트맵이 대상 qcow2에 저장됨, 실패 뒤 원본 계속 실행.
- **opl-types:** `TaskOp::VmMigrate`의 `pool` 있음·없음(없으면 지금 JSON), `MigrationStatus.disks`, `TaskStatus.progress`, `FEATURES`.
- **opl-api:** `tests/migrate.rs`(각 400·404·409, 기능 확인, 같은 이름 풀이 없는 노드 `no_capacity`, `node_fits_with`), `tests/two_nodes.rs`(실제 mTLS gRPC, `MockHypervisor`, 노드마다 풀 경로 앞에 다른 뿌리를 붙이는 시험용 디렉터리 저장소 — 같은 이름의 로컬 풀): 성공(T에 볼륨이 생기고 S의 볼륨이 지워짐, 볼륨 `status.node`·`spec.pool`·`spec.format`, 진행률), `pool`로 다른 풀, 각 단계 실패(공간, 크기, 대상에 이미 있음 — 그 파일을 지우지 않음, 미러 실패, 스위치오버 실패, 확인 실패)에서 T 볼륨이 지워지고 S가 계속 돎, 확정 뒤 S 마무리 전에 실행기가 멈춤(T 워커가 볼륨 기록을 고치고 S 정리가 원본을 지움), 복사 중 S 실행기 재시작(되돌림과 T 정리), 같은 VM이 되돌아옴, 비우기가 로컬 디스크 VM을 옮김과 남김 이유; `tests/drain.rs`(로컬 디스크 VM의 대상 고르기, 기능 없음·스냅샷 남김); 정리 규칙 단위 시험(기록 파일 역할마다).
- **opld E2E(`migration-it`, 실제 QEMU, macOS는 TCG):** 한 호스트의 두 노드라 같은 이름의 디렉터리 풀은 같은 경로가 되므로 노드마다 다른 경로의 로컬 디렉터리 풀 A(node1)·B(node2)를 만들고 `oplctl vm migrate web node2 --pool B`: 확정 뒤 B에 파일, A의 파일 없음, 볼륨 기록, 진행률이 보였는지, 끈 뒤 `qemu-io`로 마이그레이션 전후에 쓴 무늬 확인(복사 중 HMP `qemu-io` 쓰기 포함). 대상 포트를 막은 실패에서 B에 파일이 남지 않고 원본이 계속 돎. `lvm-it`·`zfs-it`와 함께 켜면 디렉터리 → LVM-thin(또는 ZFS) 풀(node2만) → 디렉터리로 왕복(qcow2 → raw → raw). 노드 비우기는 같은 이름 풀 규칙만 쓰는데 한 호스트의 두 노드에서는 같은 이름 로컬 풀이 같은 경로라 옮길 수 없으므로, 비우기로 로컬 디스크 VM을 옮기는 것은 `two_nodes.rs`가 맡고 opld E2E는 그 VM이 남는 이유(대상에 같은 볼륨이 있음)만 본다.
- **oplctl:** `--pool` 파싱과 본문, 진행률 stderr 줄(fake_api). **웹:** Vitest(대상 풀 고르기와 요청, 스냅샷 막힘, 진행률 표시, 비우기 안내 문구).

## 6. 알려진 한계

- 취소가 없다. 디스크 복사는 디스크 크기와 대역폭에 따라 몇 시간 걸릴 수 있고, 그동안 VM은 `Migrating`이라 끄기·스냅샷·백업·고치기가 409 `vm_busy`다. `disk_timeout`(12시간)이나 10분 정체에서 실패로 끝나 원본이 계속 돈다.
- 스냅샷이 있는 VM의 로컬 디스크는 옮기지 못한다(먼저 지운다).
- 대상 풀은 하나다(같은 이름 또는 `pool`). 디스크마다 다른 풀은 고를 수 없다. 노드 비우기는 같은 이름 규칙만 쓴다.
- 백엔드가 크기를 다르게 올리면(4 MiB 배수가 아닌 디스크를 LVM-thin으로) 옮기지 못한다.
- qcow2 → raw로 옮긴 디스크의 비트맵은 지워져 다음 백업이 전체다. 로컬 백업 풀의 체인은 다른 노드에서 이어지지 않는다(지금 규칙).
- 공간 확인은 할당량 기준이다. 복사 중 원본이 늘면 대상 풀이 찰 수 있고, 그러면 미러가 실패해 되돌린다.
- `background` 미러라 쓰기가 아주 많은 VM은 `ready` 뒤 뒤처짐을 스위치오버 때 메우는 만큼 중단 시간이 길어진다.
- 복사는 클러스터 주소(노드 사이 망)로 간다. 전용 마이그레이션 망은 없다. 방화벽은 `[migration] ports`를 열어야 하고, 로컬 디스크 마이그레이션은 그 범위에서 포트 둘을 쓴다.
- HA VM의 복사 중(보류 중)에는 계획 20의 한계(리스를 잃어도 마이그레이션이 끝날 때까지 처리하지 않음)가 더 길게 이어진다.
- 확정 뒤 원본 볼륨 지우기에 실패하면 다음 정리(30분)까지 S에 남는다.
- macOS(HVF) 노드는 옮기지 못한다(TCG macOS 노드는 디렉터리 풀만 된다).

## 7. 구현 단계 (계획 42)

1. **환경 탐색과 QEMU 층.** CI(QEMU 8.2)와 Debian 13(QEMU 10)·macOS(Homebrew)에서 확인: `-incoming defer` QEMU의 `block-export-add`가 노드를 활성화해 쓰기를 받음, 노드 인증서로 NBD TLS(`verify-peer`, `tls-hostname`), `pause-before-switchover`에서 ready 미러의 `block-job-cancel` 뒤 `migrate-continue`, 미러한 노드로의 `dirty-bitmaps`와 대상 qcow2 저장, `WRITE_ZEROES`로 대상이 성기게 남음. opl-qemu QMP 명령과 상태 해석, 시험.
2. **드라이버.** `Hypervisor`의 `start_incoming(…, exports) -> IncomingPorts`, `mirror_disks`, `migrate`의 `switchover_mirrors`, `cancel_migration`·`clear_backup_leftovers`의 미러 치우기, `finish_incoming`의 NBD 멈춤, 비트맵 규칙, `MockHypervisor`, 가짜 QMP 시험과 `qemu-it` 실제 시험.
3. **타입·API·스케줄러.** `TaskOp::VmMigrate.pool`, `MigrationStatus.disks`·`DiskMove`, `TaskStatus.progress`, 기능 `storage-migration`, `MigrateRequest.pool`, `start_migration`의 디스크 계획 검사와 오류, `node_fits_with`, OpenAPI, `tests/migrate.rs`.
4. **Task·받는 쪽·정리.** RPC의 `disks`·`nbd_port`, `IncomingMigrations`의 볼륨 만들기·크기 확인·기록 파일·`incoming_spec`의 계획, `tasks/migrate.rs`의 공간 확인·복사·진행률·스위치오버·`apply_disk_moves`·원본 지우기·되돌리기, 워커 `clear_migration`의 `apply_disk_moves`, `sweep`의 기록 파일 규칙, `[migration] disk_timeout`(opld 설정·`TaskRunnerConfig`), `tests/two_nodes.rs`.
5. **노드 비우기·oplctl·웹.** `move_vm`·`pick_target`·`wait_migration` 한도, `tests/drain.rs`·`two_nodes.rs`, oplctl `--pool`과 진행률, 웹 대화상자·진행률·비우기 안내, 번역.
6. **실제 시험과 문서.** opld `migration-it`(디렉터리 풀 둘, `lvm-it`·`zfs-it` 왕복), README, 설계 문서 메모(노드 코어 §1.1의 하위 프로젝트 13, 라이브 마이그레이션·노드 유지보수 설계), 이 문서의 "구현" 절.
