# opLite 공유 스토리지 설계 2: 블록 백엔드 바탕과 LVM-thin (계획 16)

작성일: 2026-10-29
상위 문서: `2026-10-28-shared-storage-nfs-design.md`(§1 분해, 백엔드 구조), `2026-10-26-snapshots-design.md`

## 1. 목표와 범위

계획 15는 파일 백엔드(디렉터리, NFS)만 다뤘다. 이 계획은 볼륨이 블록 장치인 백엔드의 바탕을 놓고, 그 첫 백엔드로 노드 로컬 **LVM-thin** 풀을 더한다. 계획 17(ZFS zvol)과 18(iSCSI, Ceph RBD)이 같은 바탕을 쓴다.

| 항목 | 내용 |
|---|---|
| 디스크 소스 | `RuntimeDisk`가 파일인지 블록 장치인지 알고, QEMU에 `driver=file` 또는 `driver=host_device`로 넘긴다 |
| 백엔드 나누기 | 저장소 호출을 풀 백엔드 종류로 나눠 보내는 `Storages`, 백엔드 사이 복사 |
| LVM-thin 풀 | `PoolBackend::LvmThin { vg, thinpool }`, 볼륨은 raw 형식의 thin LV |
| 스냅샷 | LVM thin 스냅샷으로 멈춘 VM과 실행 중인 VM(디스크만)의 스냅샷·되돌리기·지우기·복제 |
| 노드별 상태 | 마운터가 VG/thin pool을 확인하고 용량을 알린다 |
| 클라이언트 | oplctl `pool create --lvm-thin VG/POOL`, 웹 풀 만들기, 풀 종류에 맞는 기본 형식, 스냅샷 지원 판단 |

넣지 않는 것: LVM 풀의 메모리 스냅샷(vmstate를 둘 qcow2가 없다 — 409 `snapshot_unsupported`), 공유 LVM(iSCSI 위 LVM, 계획 18), 두꺼운(thick) LVM, VG·thin pool 만들기(관리자가 만든다), qcow2 형식의 LVM 볼륨, macOS의 LVM.

## 2. 데이터 모델

- `PoolBackend::LvmThin { vg: String, thinpool: String }`(serde `{"kind":"lvm-thin", ...}`). 이름 규칙: LVM 이름 문자(`A-Za-z0-9+_.-`), `-`로 시작하지 않음, 1–127자. 구현: `StoragePoolSpec::validate`의 `PoolError::BadLvmName(name)`(400 `invalid_request`), 모르는 필드는 거절(`deny_unknown_fields`). LVM이 받지 않는 이름도 거절한다: `.`·`..`, `snapshot`·`pvmove`로 시작하는 것, LVM 내부 LV의 이름 조각(`_cdata`·`_cmeta`·`_corig`·`_mimage`·`_mlog`·`_pmspare`·`_rimage`·`_rmeta`·`_tdata`·`_tmeta`·`_vorigin`·`_vdata`)을 담은 것(웹 `lvmNameError`도 같다).
- LVM-thin 풀은 노드 로컬이다. `shared: true`는 400 `invalid_request`("lvm-thin pools are node-local; shared must be false"). `is_shared`는 `shared` 값과 상관없이 false다. `nodes`가 비어 있으면 모든 노드에 같은 이름의 VG/thin pool이 있어야 쓸 수 있다(노드마다 상태로 알린다).
- 볼륨 형식은 `raw`만 된다. LVM 풀에 `qcow2` 볼륨을 만들거나 가져오면 400 `invalid_request`("lvm-thin pools hold raw volumes"). 볼륨 LV 이름은 `opl-{id}`, 장치는 `/dev/{vg}/opl-{id}`.
- 스냅샷 LV 이름은 `opl-{id}-s-{tag}`(tag는 스냅샷 id). LVM 이름 길이 제한 안에 든다(79자, 되돌리기의 임시 이름 `opl-{id}-revert-{tag}`는 84자). 태그에 LVM 이름에 쓸 수 없는 글자가 있으면 드라이버가 `InvalidSpec`으로 거절한다.
- 볼륨 만들기·가져오기의 형식 확인은 API `volumes.rs`의 `check_pool_format`이다(가져오기는 사전 확인 `check_import`에서). 드라이버도 raw가 아니면 `InvalidSpec`이다.
- `opl_types::storage::pool_kind_supports_snapshots(backend, format) -> bool`: Dir/Nfs는 qcow2, LvmThin은 raw.

## 3. 디스크 소스와 QEMU

- `RuntimeDisk`에 `kind: DiskKind { File, Block }`(`#[serde(default)]` = File)를 더한다.
- `opl-qemu` cmdline: `File` → `-blockdev driver=file,filename=...`, `Block` → `-blockdev driver=host_device,filename=...`. 포맷 노드(`driver=raw|qcow2`)는 그대로다.
- 디스크 위치는 백엔드 함수 하나로 정한다: `opl_driver::disk_location(pool, mount_root, id, format) -> Result<(PathBuf, DiskKind), DriverError>`(테스트는 `/dev` 대신 다른 디렉터리를 보는 내부 함수로 한다).
  - Dir/Nfs: 지금의 `volume_path`와 `check_pool_root`, `File`.
  - LvmThin: `/dev/{vg}/opl-{id}`, `Block`. 장치 노드가 없으면(LV가 비활성) `lvchange -ay`를 하지 않고 `DriverError::VolumeNotActive`(일시 오류) — 활성화는 드라이버의 `activate`가 맡는다(§4). reconciler는 시작 전에 저장소의 `activate_volume`을 부른다. 구현: `/dev/{vg}/{lv}` 링크가 없으면 device-mapper 노드 `/dev/mapper/{vg}-{lv}`(이름 안의 `-`는 `--`)를 쓰고, 둘 다 없으면 `VolumeNotActive`("volume device node missing: …")다. 가져오기·복사의 대상 장치와 복사 원본 장치도 같은 함수(`device_node`)로 찾는다.
- reconciler는 저장소를 받지 않았으므로 `ReconcilerConfig`에 `Arc<dyn Storage>`를 더해 `resolve`가 `disk_location`과 `activate_volume`을 쓰게 한다. 구현: `ReconcilerConfig::new(node, mount_root, storage)`, `resolve::runtime_spec(cluster, vm, node, mount_root, storage)`(async). 디스크마다 `activate_volume` → `disk_location`이고, 저장소 오류가 `VolumeNotFound`·`InvalidSpec`이면 `ResolveError::VolumeUnusable(volume, DriverError)`(VM을 끈다), 그 밖(`VolumeNotActive`·`Timeout` 같은 일시 오류와 `Lvm` 같은 명령 실패)은 `ResolveError::PoolUnavailable`(VM은 `starting`에서 다시 시도)이다. Linux가 아닌 노드의 LVM-thin 디스크는 `ResolveError::PoolUnsupported(pool, "lvm-thin pools are not supported on macOS")`(VM을 끈다).

## 4. 저장소 나누기와 LVM 드라이버

- `opl_driver::storage::Storages`가 `Storage`를 구현하고 `pool.spec.backend`로 `DirStorage`(Dir, Nfs) 또는 `LvmThinStorage`(LvmThin)에 보낸다. opld는 `Storages`를 하나 만들어 Task 실행기와 reconciler에 넘긴다. 구현: `Storages::new(qemu_img, file).with_lvm_thin(storage)`(모듈 `storage/storages.rs`). LVM 저장소를 붙이지 않았으면 lvm-thin 풀은 `InvalidSpec`이다. `DirStorage`에 lvm-thin 풀이 오면 영구 오류(`"lvm-thin pools hold no volume files"`)다. 올린 파일 검사(`check_import_source`)는 풀과 상관없이 파일 저장소가 한다.
- 새 trait 메서드:
  - `activate_volume(pool, id, format)`: 블록 백엔드가 장치를 쓸 수 있게 한다(Dir은 아무것도 안 함). 
  - `volume_source(pool, id, format, tag: Option<&str>) -> Result<VolumeSource, DriverError>`: 복사의 원본(경로, 형식, 스냅샷이면 활성화한 스냅샷 장치 또는 qcow2 태그 옵션). 끝나면 `release_source`. 구현: `VolumeSource { path, format, snapshot: Option<String>, size_bytes }`(`snapshot`은 qcow2 내부 스냅샷 이름, LVM은 늘 `None`). `size_bytes`는 읽을 내용의 가상 크기다: LVM은 읽는 LV(스냅샷이면 스냅샷 LV)의 크기, 파일 풀은 지금 볼륨의 크기(`qemu-img info`는 내부 스냅샷의 크기를 알려 주지 않고 `convert -l`도 지금 크기만큼 읽는다). 두 메서드 모두 기본 구현 없이 백엔드마다 쓴다. `activate_volume`은 멱등이다.
- `copy_volume`은 `Storages`가 처리한다. 같은 백엔드면 그 백엔드의 복사를, 다르면 대상 백엔드에 빈 볼륨을 만들고(`create_volume`) `qemu-img convert -n`으로 원본에서 대상 장치·파일로 쓴다. 원본·대상 형식은 각 풀의 형식이다(LVM → dir는 raw → raw 또는 qcow2; 복제할 때 대상 풀이 LVM이면 형식은 raw). 구현: `copy_volume`에 대상 형식 인자 `dst_format`을 더했다. 백엔드 사이 복사는 `volume_source` → 대상 `create_volume`(원본의 `size_bytes`, 스냅샷이면 그때의 크기) → `convert -n`(대상 백엔드의 `target_reads_zero(pool)`가 참이면 `--target-is-zero`: 파일 풀은 기본 구현으로 늘 참, LVM-thin은 thin pool이 새 블록을 0으로 채울 때) → (실패하면 만든 대상을 지운다) → `release_source`(늘 부르고, 실패해도 복사는 성공)다. 대상을 만들지 못하면(이미 있음 등) 지우지 않는다. 그사이 복사가 취소되면(Task 시간 초과 등) 원본은 버려지는 가드가 뒤에서 내놓는다.
- **LvmThinStorage**(명령은 `LvmOps` trait 뒤 — 테스트는 가짜, 실제는 `lvcreate`/`lvremove`/`lvs --reportformat json`/`lvextend`/`lvrename`/`lvchange`, 모두 시간 제한 60초. 구현: 시간이 지나거나 취소되면 읽기만 하는 `lvs`만 죽이고, LVM을 바꾸는 명령은 중간에 죽이면 메타데이터가 반쯤 바뀐 채 남을 수 있으므로 죽이지 않는다 — `Timeout`을 돌려주고 명령은 뒤에서 끝나게 둔다(끝날 때까지 기다리는 작업이 프로세스를 거둔다). 바꾸는 명령은 다루는 LV(`{vg}/{lv}`, 이름 바꾸기는 둘 다)마다 하나만 돈다: 같은 LV의 앞 명령이 아직 돌고 있으면 새 프로세스를 띄우지 않고 `Timeout`("an earlier lvm command still running on the same lv")이고, 앞 명령이 끝나면 기록에서 빠진다(프로세스 안의 기록). 바꾸는 명령의 표준 출력은 버리고 stderr는 만들자마자 지운 임시 파일에 받는다(기다리는 쪽이 떠난 뒤에도 파이프 때문에 막히거나 SIGPIPE로 죽지 않는다). 오류 문구는 그 첫 줄이다):
  - 구현: 모듈 `storage/lvm.rs`. `LvmOps`는 `list(vg)`·`create_thin`·`snapshot`·`remove`·`extend`·`rename`·`set_active`이고 실제 구현은 `SystemLvm`, 생성은 `LvmThinStorage::new(qemu_img, Arc<dyn LvmOps>)`다. 명령은 `-q`와 환경 `LC_ALL=C`(숫자를 `.`로), `LVM_SUPPRESS_FD_WARNINGS=1`로 부르고, 크기는 바이트(`{n}b`)로 준다. `lvs`는 `--units b --nosuffix -o lv_name,lv_attr,lv_size,data_percent`이고 `parse_lvs`가 읽는다(값이 문자열이든 숫자든, 비활성 thin LV의 빈 `data_percent`는 없음). `LvInfo`의 `is_thin_pool`(`lv_attr` 첫 글자 `t`)·`is_active`(다섯째 글자 `a`)·`used_bytes`.
  - 가져오기와 복사는 `qemu-img convert -n --target-is-zero`(새 thin LV는 0으로 읽히므로 0인 부분을 쓰지 않는다)이고, 실패하면 만든 LV를 지운다. 구현: thin pool이 새 블록을 0으로 채우지 않으면(`lvcreate -Zn`, `lv_attr` 여덟째 글자가 `z`가 아님 — `LvInfo::zeroes_new_blocks`) 덜 쓴 블록에 옛 데이터가 보일 수 있으므로 `--target-is-zero`를 쓰지 않는다. LV를 만들기 전의 `lvs`로 thin pool을 본다. `lvs`의 `zero` 필드는 세그먼트 필드라 보고서가 세그먼트 단위로 바뀌므로 쓰지 않고 `lv_attr`로 본다(마운터와 같은 필드).
  - create: `lvcreate -T {vg}/{thinpool} -V {size}B -n opl-{id} -y`(크기는 512 배수, 바이트 단위). 이미 있으면 `VolumeExists`.
  - import: 원본 `qemu-img info`로 가상 크기를 구해 LV를 만들고 `qemu-img convert -n -O raw`. 실패하면 LV를 지운다.
  - delete: `lvremove -y {vg}/opl-{id}`. 없으면 `VolumeNotFound`. 그 볼륨의 스냅샷 LV가 남아 있으면 함께 지운다. 구현: 볼륨 LV를 먼저 지운다(쓰고 있으면 실패하고 스냅샷은 그대로 남는다). 그다음 스냅샷 LV와 실패한 되돌리기가 남긴 `opl-{id}-revert-*`를 지운다. 볼륨 LV가 이미 없어도(지우다 멈춘 뒤 다시 지움) 남은 것을 지운 뒤 `VolumeNotFound`다.
  - info: `lvs`의 `lv_size`(가상), `lv_size × data_percent`(할당, 비활성이라 값이 없으면 0).
  - capacity: thin pool의 `lv_size`와 `data_percent`. thin pool이 없거나 thin pool이 아니면 `Lvm` 오류.
  - resize: `lvextend -L {size}B`(줄이지 않는다). 구현: LV가 이미 그 크기 이상이면(앞선 시도가 늘렸다) `lvextend`를 부르지 않고 성공이다.
  - snapshot create: `lvcreate -s -n opl-{id}-s-{tag} {vg}/opl-{id}`(thin 스냅샷은 기본으로 활성화 건너뛰기 표시). 이미 있으면 오류.
  - snapshot revert: 원본 LV를 `opl-{id}-revert-{tag}`로 이름을 바꾸고, `lvcreate -s -n opl-{id} {vg}/opl-{id}-s-{tag}` 뒤 `lvchange -ay -K`로 활성화, 이름을 바꾼 옛 LV를 지운다. 중간에 실패하면 이름을 되돌린다. 구현: 활성화가 실패하면 새 LV를 지운 뒤 이름을 되돌린다. 새 LV를 지우지 못하거나 이름을 되돌리지 못하면 남은 LV 이름을 담은 `Lvm` 오류다. 이름 바꾸기·새 LV 만들기·활성화가 `Timeout`이면 `lvs`(VG 잠금을 기다리므로 그 명령이 끝난 뒤의 상태)로 다시 보고, 끝났으면(옛 LV가 있음, 새 볼륨 LV가 있음, 활성임) 이어서 한다. 아니면 지금처럼 실패한다. 마지막에 옛 LV를 지우지 못하면 되돌리기는 성공이고 경고만 남긴다. 앞서 실패한 되돌리기가 남긴 같은 이름의 LV가 있으면(볼륨 LV가 있을 때) 먼저 지운다.
  - snapshot delete: `lvremove -y {vg}/opl-{id}-s-{tag}`(스냅샷 LV가 없으면 성공). tags: `opl-{id}-s-` 접두어의 LV.
  - volume_source(tag): `lvchange -ay -K` 뒤 스냅샷 장치, `release_source`는 `lvchange -an`. 태그가 없으면 볼륨 LV 자체(비활성이면 활성화하고, VM이 쓰고 있을 수 있으므로 `release_source`가 비활성화하지 않는다).
  - activate_volume: 볼륨 LV가 비활성이면 `lvchange -ay -K`.
  - 오류: `DriverError::Lvm(String)`(`{명령}: {stderr 첫 줄}`, 영구), `DriverError::VolumeNotActive(path)`(일시), 시간 초과는 `Timeout`(일시). VM 시작(§3 resolve)에서는 `Lvm`도 다시 시도한다.
  - 복사(같은 VG 안)도 원본(스냅샷이면 스냅샷 LV)의 크기로 대상 LV를 만든다. 장치 노드가 생기지 않으면 만든 LV를 지운다. 원본 스냅샷 LV는 가드가 내놓는다(취소되면 뒤에서 비활성화, `Storages`의 가드와 같은 방식).
- `supports_snapshots(format)`는 `supports_snapshots(pool, format)`로 바꾼다(백엔드마다 다르다).

## 5. 스냅샷과 Task

- API `check_disks`는 `pool_kind_supports_snapshots`로 디스크마다 판단한다. 모든 디스크가 같은 방식이어야 한다: 모두 qcow2(내부 스냅샷) 또는 모두 LVM(LV 스냅샷). 섞이면 409 `snapshot_unsupported`("disks use different snapshot methods").
- LVM 디스크의 VM은 메모리 스냅샷을 할 수 없다: `memory`가 없으면 false, true면 409 `snapshot_unsupported`. 구현: 예약 작업(`schedule.rs`의 `plan`)은 VM 디스크가 LV 스냅샷을 쓰면 예약의 `memory`가 true여도 메모리 없이 요청한다.
- **실행 중인 VM의 LVM 스냅샷:** 하이퍼바이저에서 QMP `stop`(블록 I/O를 비운다) → 디스크마다 `storage.snapshot_create` → `cont`. `cont`는 실패해도 반드시 시도한다. 멈춘 시간은 로그에 남긴다. 구현: `Hypervisor::pause`/`resume`(QEMU는 QMP `stop`/`cont`, 5초 제한, `QmpClient::stop`을 더함). `pause`가 실패하면(응답만 늦었을 수 있으므로) `resume`을 해 보고 Task를 실패로 끝낸다. 멈춘 시간은 로그의 `paused_ms`. 멈춘 구간을 지킨다: 하이퍼바이저가 `Running`이라고 알리는 VM만 멈추고 이어서 실행한다. 그 밖의 상태(`Paused`, QEMU가 스스로 멈춘 `io-error`·`guest-panicked`·`internal-error`·`suspended` 등)는 게스트가 디스크에 쓰지 않으므로 멈추지도 이어서 실행하지도 않고 디스크만 찍는다. `unresponsive`(QMP로 다룰 수 없음), 꺼짐, 상태 조회 오류는 디스크를 건드리기 전에 실패한다. 디스크 찍기는 120초(`PAUSE_LIMIT`, Task 한도 600초보다 짧다)를 넘기면 `Timeout`으로 끝낸다. `resume`은 일시 오류(`is_transient`)만 1초 간격으로 세 번까지 해 보고 `NotRunning`은 끝난 것으로 본다. 멈춘 동안 Task의 future가 버려지면(시간 초과, opld 종료) 가드가 뒤에서 `resume`한다. 한 번(재시도까지) 해 본 뒤에는 결과와 상관없이 가드를 끈다(그 뒤는 표시 파일과 다시 붙기가 맡는다). 디스크는 찍었는데 `resume`이 실패하면 스냅샷을 지우지 않고 `ready`로 기록하고 현재 위치로 삼은 뒤 Task를 "snapshot taken; resuming the VM failed: …"로 실패로 끝낸다(실행기의 정리는 `ready` 스냅샷을 두므로 리소스가 남는다). 다른 실패와 겹치면 그 오류에 "; resuming the VM failed: …"를 붙인다. QEMU 드라이버는 `pause` 때 VM 실행 디렉터리에 표시 파일 `paused-by-opl`을 쓰고 `resume`이 성공하면 지운다. opld가 멈춘 채 죽었으면 reconciler가 멈춘 VM에 다시 붙을 때(`start`) 표시가 있는 멈춘 VM에 `cont`를 보내고 표시를 지운다(지금 `pause`해 둔 VM은 건드리지 않는다). 상태 조회(`inspect`·`list`)가 실행 중인 VM을 보고 지금 `pause`해 둔 것이 아니면 남은 표시를 지운다(이어서 실행했지만 지우지 못했거나 누가 `cont`함 — 그대로 두면 나중에 다른 까닭으로 멈춘 VM을 이어서 실행한다). Task도 디스크 방식이 섞였거나 LVM 디스크에 메모리가 요청됐는지 다시 본다(API 확인 뒤 스펙이 바뀌었을 수 있다). 판단 도우미는 `snapshots::uses_lv_snapshots(pool)`이다.
- **되돌리기:** 지금처럼 강제 정지 뒤 `storage.snapshot_revert`(LVM은 §4의 방식). 메모리 스냅샷이 없으므로 `restore_on_start`는 없다.
- **지우기:** 실행 중이어도 LVM 스냅샷 LV는 저장소로 지운다(QEMU가 쓰지 않는다). 실패한 스냅샷의 정리도 같다.
- **복제:** 스냅샷에서 복제하면 `volume_source(tag)`로 스냅샷 장치를 읽는다. 실행 중인 VM의 지금 상태 복제는 지금처럼 안 된다.
- **크기 늘리기:** LVM 풀이면 실행 중이어도 먼저 `storage.resize_volume`(`lvextend`), 그다음 실행 중이면 QMP `block_resize`.
- **가져오기·복제 형식:** LVM 풀로 가져오거나 복제하면 형식은 raw(요청의 format이 qcow2면 400). 구현(복제, `tasks/clone.rs`의 `copy_format`): 대상이 LVM-thin 풀이면 raw, LVM-thin 볼륨을 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있게 qcow2, 파일 풀 사이는 원본 형식 그대로다. 복제한 볼륨 리소스의 형식도 이것이다.

## 6. 노드별 풀 상태

- `PoolMounter`가 LvmThin 풀은 `lvs --reportformat json -o lv_name,lv_attr,lv_size,data_percent {vg}/{thinpool}`(시간 제한 10초)로 확인한다. thin pool(`lv_attr`이 `t`로 시작)이면 `ready`, 용량은 크기와 data%. 없거나 thin pool이 아니면 `ready: false`와 이유. `MountOps`에 `lvm_pool(vg, thinpool)`을 더한다. 구현: `MountOps::lvm_pool(vg, thinpool) -> Result<LvInfo, String>`(실제는 `--units b --nosuffix`를 더한 `lvs`, 해석은 드라이버의 `parse_lvs`). 확인은 풀마다 비동기 작업으로 동시에 돌고 `CHECK_TIMEOUT`(10초)이 지나면 `lvs`를 죽인다. 이유 문구는 `"{vg}/{thinpool} is not a thin pool"`, `"lvs timed out"`, 그 밖에는 `lvs`의 stderr 첫 줄이다. 지원 여부는 `Pools.lvm_supported`(Linux만, 테스트가 바꾼다).
- macOS는 LVM을 지원하지 않는다(`"lvm-thin pools are not supported on macOS"`, VM은 이유와 함께 끈다 — NFS와 같은 규칙).
- LVM-thin은 디렉터리 풀처럼 보고 전에도 쓸 수 있는 것이 아니라 NFS처럼 `ready: true`가 있어야 쓴다(장치가 없으면 쓸 수 없으므로).

## 7. 클라이언트

- **oplctl:** `pool create NAME --lvm-thin VG/THINPOOL`(`--path`·`--nfs`·`--shared`와 함께 못 씀), 목록·상세의 종류 `lvm-thin`과 위치 `VG/THINPOOL`. `volume create`/`import`의 `--format`이 없으면 풀 종류로 기본값(LVM → raw, 그 밖 → qcow2). 구현: `--nfs-options`와도 함께 못 쓴다. `--path`·`--nfs`·`--lvm-thin` 중 하나가 꼭 있어야 한다(clap `ArgGroup`). `VG/THINPOOL`은 `/`로 나눈 두 부분이 비어 있지 않은지만 보고 이름 규칙은 API가 검사한다. 형식 기본값은 풀을 읽어(`GET /storage-pools/{pool}`) 정하고, 읽지 못하면 "cannot read storage pool … to choose a volume format; pass --format"이다.
- **웹:** 풀 만들기 대화상자의 종류에 LVM-thin(VG, thin pool 필드와 검사), 풀 목록의 종류·위치. 볼륨 만들기·업로드·VM 새 디스크의 형식이 풀 종류를 따른다(LVM 풀은 raw). 스냅샷 지원 판단(`VmSnapshots`, 예약 스냅샷)은 서버 규칙과 같은 도우미로. 구현: `web/src/storage/pools.ts`의 `lvmNameError`(서버와 같은 이름 검사)·`poolFormats`(풀이 담는 형식), `web/src/vms/snapshots.ts`의 `volumeDisk`·`snapshotSupport`·`formSnapshotSupport`(모두 qcow2 또는 모두 LVM, 섞이면 안 됨). LVM 디스크의 VM은 스냅샷 대화상자와 예약 스냅샷(`ScheduleSection`)에서 메모리 선택을 숨긴다. 예약의 `memory`는 저장한 값 그대로 두고(예약 작업이 메모리 없이 찍는다), 스냅샷 탭의 예약 요약에 "메모리 포함"을 보이지 않는다. `snapshotSupport`는 목록에 없는 볼륨을 건너뛰고 모르는 풀을 파일 풀로 본다(서버는 400).

## 8. 오류 처리

- LVM 명령 실패는 stderr 첫 줄을 담은 `DriverError::Lvm(String)`. 시간 초과는 일시 오류. VM 시작의 볼륨 활성화·장치 찾기에서는 볼륨이 없거나(`VolumeNotFound`) 정의가 틀린(`InvalidSpec`) 것만 VM을 끄고, 나머지(`Lvm` 포함)는 잠시 뒤 다시 시도한다.
- 실행 중 스냅샷에서 `stop` 뒤 일부 디스크만 스냅샷이 된 채 실패하면, 만든 스냅샷 LV를 지우고 `cont` 뒤 Task를 실패로 끝낸다.
- 되돌리기 중간 실패는 이름을 되돌리고, 되돌릴 수 없으면 오류에 남은 LV 이름을 적는다(수동 복구).

## 9. 테스트

- **단위:** 타입·검증, `disk_location`, cmdline의 `host_device`, `Storages` 나누기와 백엔드 사이 복사 순서, 가짜 `LvmOps`로 LVM 드라이버의 각 동작(만들기·지우기·스냅샷·되돌리기 실패 복구·정보·용량), API(LVM 풀의 형식 규칙, 스냅샷 방식 섞임·메모리 거절), 실행 중 LVM 스냅샷의 `stop`/`cont` 순서(가짜 하이퍼바이저), 마운터의 LVM 상태.
- **실제 LVM(`lvm-it`, Linux·root):** CI `linux-kvm`에 `lvm2`·`thin-provisioning-tools`, 루프 장치 위 VG `opltest`와 thin pool `tp`(`OPL_TEST_LVM=opltest/tp`). 드라이버 통합 테스트(만들기·가져오기·스냅샷·되돌리기·복사·지우기)와 opld E2E(LVM 디스크로 VM을 TCG로 띄우고, 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제, 지우기). 구현: 기능 `lvm-it`(opl-driver, opld), `crates/opl-driver/tests/lvm_storage.rs`(4개), `crates/opld/tests/lvm.rs`(1개, 풀은 API로 만들고 볼륨은 `--format raw`를 적는다. 디스크 내용은 꺼진 VM의 LV 장치를 `qemu-io`로 본다. 디렉터리 풀로 복제한 볼륨은 qcow2다). CI는 `modprobe dm_thin_pool` 뒤 2G 루프 이미지에 VG를 만들고 1G thin pool을 둔다.
- 기존 시험이 그대로 통과한다.

## 10. 알려진 한계

- LVM 디스크의 VM은 메모리 스냅샷이 없다.
- LVM-thin 풀은 노드 로컬이다(VM은 볼륨이 있는 노드에서만 돈다).
- 실행 중 스냅샷 동안 VM이 잠깐 멈춘다(LV 수만큼의 `lvcreate`).
- thin pool이 가득 차면 쓰기가 실패한다(용량은 노드 상태로 보인다, 자동 확장은 하지 않는다).
- 되돌리기가 마지막에 옛 LV(`opl-{id}-revert-{tag}`)를 지우지 못하면 그 LV가 남아 공간을 차지한다. 같은 태그로 다시 되돌리거나 볼륨을 지우면 함께 지운다.
- qcow2 내부 스냅샷에서 복사하면 `qemu-img convert -l`이 지금 볼륨 크기만큼만 읽는다. 볼륨을 늘린 뒤 찍은 스냅샷을, 그보다 작던 때의 스냅샷으로 되돌린 볼륨에서 복사하면 늘린 부분이 빠진다.
- LVM을 바꾸는 명령이 60초를 넘겨 `Timeout`으로 끝나도 명령은 뒤에서 끝까지 돈다. 결과는 다음 조회(`lvs`)로 보인다. 그동안 같은 LV를 바꾸는 요청은 일시 오류로 끝난다. 이 기록은 opld 프로세스 안의 것이라 opld를 다시 띄우면 사라진다.
- 복사 원본을 내놓는 가드는 `volume_source`가 돌아온 뒤에 생긴다. 스냅샷 LV를 활성화하는 도중에 취소되거나, 내놓을 때 `qemu-img`가 아직 장치를 연 채라 비활성화가 실패하면 스냅샷 LV가 활성인 채 남는다(경고만 남고, 다음 복사·지우기에는 문제가 없다).
- 이미지 가져오기의 공간 사전 확인은 지금처럼 opld의 업로드 디렉터리만 본다(thin pool의 남은 공간은 보지 않는다).
