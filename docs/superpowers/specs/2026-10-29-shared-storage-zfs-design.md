# opLite 공유 스토리지 설계 3: ZFS zvol (계획 17)

작성일: 2026-10-29
상위 문서: `2026-10-29-shared-storage-lvm-thin-design.md`(블록 백엔드 바탕), `2026-10-28-shared-storage-nfs-design.md`(§1 분해)

## 1. 목표와 범위

계획 16의 블록 백엔드 바탕 위에 노드 로컬 **ZFS** 풀을 더한다. 볼륨은 sparse zvol(raw 블록 장치)이고, 스냅샷은 ZFS 스냅샷이다. LVM-thin과 겹치는 부분(블록 스냅샷 방식, 실행 중 VM 멈춤, 메모리 스냅샷 없음, 형식 규칙)은 "블록 백엔드" 공통 규칙으로 일반화한다.

| 항목 | 내용 |
|---|---|
| ZFS 풀 | `PoolBackend::Zfs { dataset }`, 볼륨은 `{dataset}/opl-{id}` sparse zvol, raw만 |
| 드라이버 | `ZfsStorage`와 `ZfsOps`(zfs/zpool 명령 뒤), `Storages`에 연결 |
| 스냅샷 | `zfs snapshot`(여러 디스크를 한 명령으로 원자적으로), 되돌리기는 그 볼륨의 **가장 최근 스냅샷으로만**, 지우기, 스냅샷에서 복제 |
| 공통화 | "LV 스냅샷"을 "블록 스냅샷"으로 일반화: 실행 중 VM 멈춤 경로, 메모리 거절, 예약의 메모리 끄기, 웹·oplctl 판단 |
| 노드별 상태 | 마운터가 데이터셋을 확인하고 용량을 알린다 |
| 클라이언트 | oplctl `pool create --zfs DATASET`, 웹의 ZFS 풀 |

넣지 않는 것: zpool·데이터셋 만들기(관리자), ZFS 파일 데이터셋(qcow2 파일) — 그건 디렉터리 풀로 쓴다, `zfs send/recv` 복제(복사는 `qemu-img convert`), 가장 최근이 아닌 스냅샷으로 되돌리기, 메모리 스냅샷, macOS의 ZFS, 공유 ZFS.

## 2. 데이터 모델

- `PoolBackend::Zfs { dataset: String }`(serde `{"kind":"zfs","dataset":"tank/opl"}`). 이름 규칙: ZFS 데이터셋 이름(`/`로 나뉜 구성 요소, 각 구성 요소는 `A-Za-z0-9_.:-`, 첫 구성 요소(풀 이름)는 문자로 시작, `-`로 시작하는 구성 요소 금지, `@`·`#`·공백 금지, 전체 255자 이하).
- 노드 로컬(`shared: true`는 400), 볼륨은 raw만(qcow2는 400) — LVM-thin과 같은 규칙. `pool_kind_supports_snapshots`는 Zfs면 raw.
- zvol `{dataset}/opl-{id}`, 장치 `/dev/zvol/{dataset}/opl-{id}`. 스냅샷 `{dataset}/opl-{id}@{tag}`(tag는 스냅샷 id).

## 3. 드라이버 (`storage/zfs.rs`)

- 명령은 `ZfsOps` trait 뒤(가짜/실제). 실제는 `zfs`/`zpool`, `LC_ALL=C`, 60초 제한. **바꾸는 명령은 LVM처럼 죽이지 않고**(출력은 파일), 같은 zvol에 하나씩. 읽기(`zfs list`/`zfs get`)만 죽인다. LVM의 실행 도우미를 공용 모듈(`storage/blockcmd.rs`)로 옮겨 함께 쓴다.
- 만들기: `zfs create -s -V {size} -o volmode=dev -o snapdev=visible {dataset}/opl-{id}`(크기는 volblocksize 배수로 올림 — 기본 16K, `zfs get volblocksize`로 확인하지 않고 1 MiB 배수로 올린다). 장치 노드가 나타날 때까지 짧게 기다린다(udev, 최대 10초, 없으면 지우고 오류).
- 가져오기: 가상 크기로 만든 뒤 `qemu-img convert -n [--target-is-zero] -O raw`(새 sparse zvol은 0으로 읽힌다). 실패하면 지운다.
- 지우기: `zfs destroy -r {dataset}/opl-{id}`(스냅샷 함께). 없으면 `VolumeNotFound`.
- 정보: `zfs get -Hp volsize,referenced`. 용량: 데이터셋의 `available`·`used`.
- 크기: `zfs set volsize={size}`(줄이지 않는다, 이미 크면 건너뜀).
- 스냅샷: `zfs snapshot {zvol}@{tag}`. 여러 디스크를 한 번에 찍는 `snapshot_many(pool, ids, tag)`를 trait에 기본 구현(하나씩)으로 두고 ZFS는 한 명령(`zfs snapshot a@t b@t`, 원자적)으로 덮는다.
- 되돌리기: 그 zvol의 가장 최근 스냅샷이 `tag`일 때만 `zfs rollback {zvol}@{tag}`. 아니면 `DriverError::InvalidSpec("zfs can only revert to the latest snapshot of a volume")`.
- 스냅샷 지우기: `zfs destroy {zvol}@{tag}`. 태그 목록: `zfs list -H -t snapshot -o name -d 1 {zvol}`.
- 원본: 태그가 있으면 `/dev/zvol/{zvol}@{tag}`(snapdev=visible), 없으면 zvol 장치. 해제는 할 일 없음.
- 같은 백엔드 복사: 대상 zvol을 만들고 `qemu-img convert -n`(데이터셋이 달라도 같다).
- `disk_location`: `/dev/zvol/...`, `DiskKind::Block`. 없으면 일시 오류 `VolumeNotActive`. `activate_volume`은 장치 노드가 있는지 확인만.

## 4. 스냅샷 규칙의 일반화

- `uses_lv_snapshots`를 `uses_block_snapshots(pool)`(LvmThin, Zfs)로 바꾼다. 같은 VM의 디스크가 모두 같은 방식이어야 한다: 모두 qcow2 내부, 또는 모두 같은 블록 백엔드 종류(LVM과 ZFS가 섞이면 409).
- 블록 디스크 VM은 메모리 스냅샷이 없다(409, 예약은 메모리 없이).
- 실행 중 블록 스냅샷: 계획 16의 멈춤 경로(상태가 Running일 때만 `stop` → 디스크 스냅샷 → `cont`, 120초, 가드·재시도·표시)를 그대로 쓰고, 디스크 스냅샷은 `snapshot_many`로 찍는다(ZFS는 원자적).
- **되돌리기 제한:** API는 VM의 디스크가 ZFS이고 되돌릴 스냅샷이 그 VM의 가장 최근 스냅샷이 아니면 409 `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에 거절한다. 드라이버도 같은 확인을 한다(경쟁 대비).
- 크기 늘리기: ZFS도 `volsize` 먼저, 실행 중이면 `block_resize`(실제 volsize로).

## 5. 노드별 상태

- 마운터: Zfs 풀은 `zfs get -Hp -o value available,used,type {dataset}`(10초). `type`이 `filesystem`(또는 `volume`이 아닌 데이터셋)이면 준비, 용량은 `available`+`used` 합과 `available`. 없으면 이유. macOS는 `"zfs pools are not supported on macOS"`(VM은 이유와 함께 끈다).
- `MountOps::zfs_dataset(dataset)`.

## 6. 클라이언트

- **oplctl:** `pool create NAME --zfs DATASET`(다른 종류와 함께 못 씀), 목록·상세 종류 `zfs`·위치 `DATASET`, 형식 기본값 raw.
- **웹:** 풀 만들기의 ZFS 종류(데이터셋 필드와 검사), 목록 종류·위치, `poolFormats`가 ZFS면 raw, `snapshotSupport`가 블록 방식 일반화와 ZFS의 되돌리기 제한(가장 최근이 아닌 스냅샷의 되돌리기 메뉴를 이유와 함께 막는다 — `ReasonMenuItem`).

## 7. 테스트

- **단위:** 타입·이름 검증, 가짜 `ZfsOps`로 드라이버 각 동작(만들기·가져오기·지우기·정보·용량·크기·스냅샷·원자적 다중 스냅샷·최근 스냅샷만 되돌리기·태그·원본), 공용 명령 도우미, API(ZFS 규칙, 섞인 블록 방식 409, 최근이 아닌 되돌리기 409), 마운터 ZFS 상태, 웹·oplctl.
- **실제 ZFS(`zfs-it`, Linux·root):** CI `linux-kvm`에 `zfsutils-linux`, 루프 파일 위 zpool `opltestz`와 데이터셋 `opltestz/opl`(`OPL_TEST_ZFS=opltestz/opl`). 드라이버 통합 테스트와 opld E2E(ZFS 디스크 VM, 실행 중 스냅샷, 최근 스냅샷 되돌리기, 디렉터리 풀로 복제, 지우기).

## 8. 알려진 한계

- 가장 최근 스냅샷으로만 되돌릴 수 있다(ZFS rollback의 성질 — 더 오래된 스냅샷으로 가려면 그 뒤의 스냅샷을 먼저 지운다).
- 메모리 스냅샷이 없다. 노드 로컬이다. 실행 중 스냅샷 동안 VM이 잠깐 멈춘다.
- zvol 크기는 1 MiB 배수로 올린다.
