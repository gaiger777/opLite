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

- `PoolBackend::LvmThin { vg: String, thinpool: String }`(serde `{"kind":"lvm-thin", ...}`). 이름 규칙: LVM 이름 문자(`A-Za-z0-9+_.-`), `-`로 시작하지 않음, 1–127자.
- LVM-thin 풀은 노드 로컬이다. `shared: true`는 400 `invalid_request`. `nodes`가 비어 있으면 모든 노드에 같은 이름의 VG/thin pool이 있어야 쓸 수 있다(노드마다 상태로 알린다).
- 볼륨 형식은 `raw`만 된다. LVM 풀에 `qcow2` 볼륨을 만들거나 가져오면 400 `invalid_request`("lvm-thin pools hold raw volumes"). 볼륨 LV 이름은 `opl-{id}`, 장치는 `/dev/{vg}/opl-{id}`.
- 스냅샷 LV 이름은 `opl-{id}-s-{tag}`(tag는 스냅샷 id). LVM 이름 길이 제한 안에 든다.
- `opl_types::storage::pool_kind_supports_snapshots(backend, format) -> bool`: Dir/Nfs는 qcow2, LvmThin은 raw.

## 3. 디스크 소스와 QEMU

- `RuntimeDisk`에 `kind: DiskKind { File, Block }`(`#[serde(default)]` = File)를 더한다.
- `opl-qemu` cmdline: `File` → `-blockdev driver=file,filename=...`, `Block` → `-blockdev driver=host_device,filename=...`. 포맷 노드(`driver=raw|qcow2`)는 그대로다.
- 디스크 위치는 백엔드 함수 하나로 정한다: `opl_driver::storage::disk_location(pool, mount_root, id, format) -> Result<(PathBuf, DiskKind), DriverError>`.
  - Dir/Nfs: 지금의 `volume_path`와 `check_pool_root`, `File`.
  - LvmThin: `/dev/{vg}/opl-{id}`, `Block`. 장치 노드가 없으면(LV가 비활성) `lvchange -ay`를 하지 않고 `DriverError::VolumeNotActive`(일시 오류) — 활성화는 드라이버의 `activate`가 맡는다(§4). reconciler는 시작 전에 저장소의 `activate_volume`을 부른다.
- reconciler는 저장소를 받지 않았으므로 `ReconcilerConfig`에 `Arc<dyn Storage>`를 더해 `resolve`가 `disk_location`과 `activate_volume`을 쓰게 한다.

## 4. 저장소 나누기와 LVM 드라이버

- `opl_driver::storage::Storages`가 `Storage`를 구현하고 `pool.spec.backend`로 `DirStorage`(Dir, Nfs) 또는 `LvmThinStorage`(LvmThin)에 보낸다. opld는 `Storages`를 하나 만들어 Task 실행기와 reconciler에 넘긴다.
- 새 trait 메서드:
  - `activate_volume(pool, id, format)`: 블록 백엔드가 장치를 쓸 수 있게 한다(Dir은 아무것도 안 함). 
  - `volume_source(pool, id, format, tag: Option<&str>) -> Result<VolumeSource, DriverError>`: 복사의 원본(경로, 형식, 스냅샷이면 활성화한 스냅샷 장치 또는 qcow2 태그 옵션). 끝나면 `release_source`.
- `copy_volume`은 `Storages`가 처리한다. 같은 백엔드면 그 백엔드의 복사를, 다르면 대상 백엔드에 빈 볼륨을 만들고(`create_volume`) `qemu-img convert -n`으로 원본에서 대상 장치·파일로 쓴다. 원본·대상 형식은 각 풀의 형식이다(LVM → dir는 raw → raw 또는 qcow2; 복제할 때 대상 풀이 LVM이면 형식은 raw).
- **LvmThinStorage**(명령은 `LvmOps` trait 뒤 — 테스트는 가짜, 실제는 `lvcreate`/`lvremove`/`lvs --reportformat json`/`lvextend`/`lvrename`/`lvchange`, 모두 `kill_on_drop`과 시간 제한 60초):
  - create: `lvcreate -T {vg}/{thinpool} -V {size}B -n opl-{id} -y`(크기는 512 배수, 바이트 단위). 이미 있으면 `VolumeExists`.
  - import: 원본 `qemu-img info`로 가상 크기를 구해 LV를 만들고 `qemu-img convert -n -O raw`. 실패하면 LV를 지운다.
  - delete: `lvremove -y {vg}/opl-{id}`. 없으면 `VolumeNotFound`. 그 볼륨의 스냅샷 LV가 남아 있으면 함께 지운다.
  - info: `lvs`의 `lv_size`(가상), `lv_size × data_percent`(할당).
  - capacity: thin pool의 `lv_size`와 `data_percent`.
  - resize: `lvextend -L {size}B`(줄이지 않는다).
  - snapshot create: `lvcreate -s -n opl-{id}-s-{tag} {vg}/opl-{id}`(thin 스냅샷은 기본으로 활성화 건너뛰기 표시). 이미 있으면 오류.
  - snapshot revert: 원본 LV를 `opl-{id}-revert-{tag}`로 이름을 바꾸고, `lvcreate -s -n opl-{id} {vg}/opl-{id}-s-{tag}` 뒤 `lvchange -ay -K`로 활성화, 이름을 바꾼 옛 LV를 지운다. 중간에 실패하면 이름을 되돌린다.
  - snapshot delete: `lvremove -y {vg}/opl-{id}-s-{tag}`. tags: `opl-{id}-s-` 접두어의 LV.
  - volume_source(tag): `lvchange -ay -K` 뒤 스냅샷 장치, `release_source`는 `lvchange -an`.
- `supports_snapshots(format)`는 `supports_snapshots(pool, format)`로 바꾼다(백엔드마다 다르다).

## 5. 스냅샷과 Task

- API `check_disks`는 `pool_kind_supports_snapshots`로 디스크마다 판단한다. 모든 디스크가 같은 방식이어야 한다: 모두 qcow2(내부 스냅샷) 또는 모두 LVM(LV 스냅샷). 섞이면 409 `snapshot_unsupported`("disks use different snapshot methods").
- LVM 디스크의 VM은 메모리 스냅샷을 할 수 없다: `memory`가 없으면 false, true면 409 `snapshot_unsupported`.
- **실행 중인 VM의 LVM 스냅샷:** 하이퍼바이저에서 QMP `stop`(블록 I/O를 비운다) → 디스크마다 `storage.snapshot_create` → `cont`. `cont`는 실패해도 반드시 시도한다. 멈춘 시간은 로그에 남긴다.
- **되돌리기:** 지금처럼 강제 정지 뒤 `storage.snapshot_revert`(LVM은 §4의 방식). 메모리 스냅샷이 없으므로 `restore_on_start`는 없다.
- **지우기:** 실행 중이어도 LVM 스냅샷 LV는 저장소로 지운다(QEMU가 쓰지 않는다).
- **복제:** 스냅샷에서 복제하면 `volume_source(tag)`로 스냅샷 장치를 읽는다. 실행 중인 VM의 지금 상태 복제는 지금처럼 안 된다.
- **크기 늘리기:** LVM 풀이면 실행 중이어도 먼저 `storage.resize_volume`(`lvextend`), 그다음 실행 중이면 QMP `block_resize`.
- **가져오기·복제 형식:** LVM 풀로 가져오거나 복제하면 형식은 raw(요청의 format이 qcow2면 400).

## 6. 노드별 풀 상태

- `PoolMounter`가 LvmThin 풀은 `lvs --reportformat json -o lv_name,lv_attr,lv_size,data_percent {vg}/{thinpool}`(시간 제한 10초)로 확인한다. thin pool(`lv_attr`이 `t`로 시작)이면 `ready`, 용량은 크기와 data%. 없거나 thin pool이 아니면 `ready: false`와 이유. `MountOps`에 `lvm_pool(vg, thinpool)`을 더한다.
- macOS는 LVM을 지원하지 않는다(`"lvm-thin pools are not supported on macOS"`, VM은 이유와 함께 끈다 — NFS와 같은 규칙).
- LVM-thin은 디렉터리 풀처럼 보고 전에도 쓸 수 있는 것이 아니라 NFS처럼 `ready: true`가 있어야 쓴다(장치가 없으면 쓸 수 없으므로).

## 7. 클라이언트

- **oplctl:** `pool create NAME --lvm-thin VG/THINPOOL`(`--path`·`--nfs`·`--shared`와 함께 못 씀), 목록·상세의 종류 `lvm-thin`과 위치 `VG/THINPOOL`. `volume create`/`import`의 `--format`이 없으면 풀 종류로 기본값(LVM → raw, 그 밖 → qcow2).
- **웹:** 풀 만들기 대화상자의 종류에 LVM-thin(VG, thin pool 필드와 검사), 풀 목록의 종류·위치. 볼륨 만들기·업로드·VM 새 디스크의 형식이 풀 종류를 따른다(LVM 풀은 raw). 스냅샷 지원 판단(`VmSnapshots`, 예약 스냅샷)은 서버 규칙과 같은 도우미로.

## 8. 오류 처리

- LVM 명령 실패는 stderr 첫 줄을 담은 `DriverError::Lvm(String)`. 시간 초과는 일시 오류.
- 실행 중 스냅샷에서 `stop` 뒤 일부 디스크만 스냅샷이 된 채 실패하면, 만든 스냅샷 LV를 지우고 `cont` 뒤 Task를 실패로 끝낸다.
- 되돌리기 중간 실패는 이름을 되돌리고, 되돌릴 수 없으면 오류에 남은 LV 이름을 적는다(수동 복구).

## 9. 테스트

- **단위:** 타입·검증, `disk_location`, cmdline의 `host_device`, `Storages` 나누기와 백엔드 사이 복사 순서, 가짜 `LvmOps`로 LVM 드라이버의 각 동작(만들기·지우기·스냅샷·되돌리기 실패 복구·정보·용량), API(LVM 풀의 형식 규칙, 스냅샷 방식 섞임·메모리 거절), 실행 중 LVM 스냅샷의 `stop`/`cont` 순서(가짜 하이퍼바이저), 마운터의 LVM 상태.
- **실제 LVM(`lvm-it`, Linux·root):** CI `linux-kvm`에 `lvm2`·`thin-provisioning-tools`, 루프 장치 위 VG `opltest`와 thin pool `tp`(`OPL_TEST_LVM=opltest/tp`). 드라이버 통합 테스트(만들기·가져오기·스냅샷·되돌리기·복사·지우기)와 opld E2E(LVM 디스크로 VM을 TCG로 띄우고, 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제, 지우기).
- 기존 시험이 그대로 통과한다.

## 10. 알려진 한계

- LVM 디스크의 VM은 메모리 스냅샷이 없다.
- LVM-thin 풀은 노드 로컬이다(VM은 볼륨이 있는 노드에서만 돈다).
- 실행 중 스냅샷 동안 VM이 잠깐 멈춘다(LV 수만큼의 `lvcreate`).
- thin pool이 가득 차면 쓰기가 실패한다(용량은 노드 상태로 보인다, 자동 확장은 하지 않는다).
