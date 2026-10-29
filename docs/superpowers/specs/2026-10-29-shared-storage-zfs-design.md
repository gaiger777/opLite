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

- `PoolBackend::Zfs { dataset: String }`(serde `{"kind":"zfs","dataset":"tank/opl"}`). 이름 규칙: ZFS 데이터셋 이름(`/`로 나뉜 구성 요소, 각 구성 요소는 `A-Za-z0-9_.:-`, 첫 구성 요소(풀 이름)는 문자로 시작, `-`로 시작하는 구성 요소 금지, `@`·`#`·공백 금지, 전체 255자 이하). 구현: `StoragePoolSpec::validate`의 `PoolError::BadZfsDataset(name)`(400 `invalid_request`), 모르는 필드는 거절. 구성 요소 `.`·`..`도 거절한다. 상한은 255자가 아니라 177자다: zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`(78자를 더한다)가 ZFS 이름 상한 255자 안에 들어야 한다. 웹 `zfsDatasetError`(`web/src/storage/pools.ts`)도 같다.
- 노드 로컬(`shared: true`는 400), 볼륨은 raw만(qcow2는 400) — LVM-thin과 같은 규칙. `pool_kind_supports_snapshots`는 Zfs면 raw. 구현: 문구는 "zfs pools are node-local; shared must be false", "zfs pools hold raw volumes"(API `check_pool_format`이 두 블록 종류를 함께 본다). 가져오기·만들기의 형식 기본값은 raw. `is_shared`는 늘 false. `pool_root`는 `/dev/zvol/{dataset}`이고 파일 저장소(`check_pool_root`)는 ZFS 풀을 거절한다("zfs pools hold no volume files").
- zvol `{dataset}/opl-{id}`, 장치 `/dev/zvol/{dataset}/opl-{id}`. 스냅샷 `{dataset}/opl-{id}@{tag}`(tag는 스냅샷 id).

## 3. 드라이버 (`storage/zfs.rs`)

- 명령은 `ZfsOps` trait 뒤(가짜/실제). 실제는 `zfs`/`zpool`, `LC_ALL=C`, 60초 제한. **바꾸는 명령은 LVM처럼 죽이지 않고**(출력은 파일), 같은 zvol에 하나씩. 읽기(`zfs list`/`zfs get`)만 죽인다. LVM의 실행 도우미를 공용 모듈(`storage/blockcmd.rs`)로 옮겨 함께 쓴다. 구현: `ZfsOps`는 `volumes(dataset)`(`zfs list -Hp -t volume -d 1 -o name,volsize,referenced`)·`snapshots(zvol)`(`-t snapshot -d 1 -s createtxg -o name,volsize`, 만든 순서)·`space`·`create`·`destroy`(`-r`)·`snapshot(zvols, tag)`·`rollback`·`destroy_snapshot`·`set_volsize`·`flush_buffers`·`changing(zvol)`(바꾸는 명령이 아직 도는지)·`settle`(`udevadm settle --timeout=10`, udev가 없으면 `false`)이고 실제 구현은 `SystemZfs`, 생성은 `ZfsStorage::new(qemu_img, Arc<dyn ZfsOps>)`, 연결은 `Storages::with_zfs`(opld `member.rs`). 오류는 `DriverError::Zfs(String)`(`zfs: {stderr 첫 줄}`, 영구 — `Lvm`과 같은 분류). 명령은 `BlockTool::new(timeout_msg, busy_msg, env, failure)`를 도구마다 `static`으로 두고(바쁜 대상 집합도 도구마다 따로) `run_read`(kill_on_drop·시간 제한)와 `run_change(program, args, targets, timeout)`(죽이지 않음, stderr는 만들자마자 지운 임시 파일, 대상마다 하나 — 앞 명령이 돌고 있으면 `Timeout`("an earlier zfs command still running on the same zvol"), 늦은 회수)를 쓴다. ZFS의 대상은 zvol 전체 이름, 여러 zvol을 찍는 `snapshot`은 그 zvol 모두다.
- 만들기: `zfs create -s -V {size} -o volmode=dev -o snapdev=visible {dataset}/opl-{id}`(크기는 volblocksize 배수로 올림 — 기본 16K, `zfs get volblocksize`로 확인하지 않고 1 MiB 배수로 올린다). 장치 노드가 나타날 때까지 짧게 기다린다(udev, 최대 10초, 없으면 지우고 오류). 구현: 이미 있으면 `VolumeExists`, 노드가 끝내 없으면 `VolumeNotActive`(일시). udev가 새 zvol을 다시 다루는 동안 `/dev/zvol` 링크가 잠시 사라졌다 생겨 노드를 본 직후 열기가 실패했으므로(CI에서 발견) 노드를 기다리기 전에 `udevadm settle`을 하고, 하지 못하면(없음, 시간 초과는 경고) 100 ms 떨어진 두 번의 확인에서 모두 보여야 한다. 되돌린 뒤에도 settle한다. 스냅샷을 찍은 뒤에는 하지 않는다(최종 리뷰: 실행 중 VM이 멈춘 동안이고 `udevadm settle`은 호스트의 udev 큐 전체를 기다린다). 대신 스냅샷·볼륨을 지우기 전에 settle하고(방금 생긴 스냅샷 장치를 udev가 열고 있으면 "busy"), 스냅샷 장치를 읽을 때는 `wait_node`가 settle한다. `-o com.sun:auto-snapshot=false`도 둔다(이 속성을 따르는 자동 스냅샷 도구가 zvol을 찍으면 더 앞의 스냅샷으로 되돌릴 수 없다. sanoid처럼 속성을 보지 않는 도구는 README에서 설정으로 빼라고 안내한다).
- 가져오기: 가상 크기로 만든 뒤 `qemu-img convert -n [--target-is-zero] -O raw`(새 sparse zvol은 0으로 읽힌다). 실패하면 지운다. 구현: `--target-is-zero`는 늘 쓴다(`target_reads_zero`도 기본값 참이라 백엔드 사이 복사에서도 쓴다). 원본 가상 크기는 LVM과 함께 쓰는 `source_virtual_size`(`qemu-img info`)다.
- 지우기: `zfs destroy -r {dataset}/opl-{id}`(스냅샷 함께). 없으면 `VolumeNotFound`. 구현: "busy"로 실패하면(방금 닫힌 장치를 udev 등이 아직 열고 있다) 1초 간격으로 세 번까지 다시 한다. 볼륨 지우기와, 만들기·가져오기·복사가 실패한 뒤의 정리, 스냅샷 지우기(`destroy_snapshot`)가 같다. zvol이 없어도 그 zvol을 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 만들기) `VolumeNotFound`가 아니라 일시 오류(`Timeout`)다: 그대로 리소스를 지우면 명령이 끝나 생긴 zvol이 남는다(최종 리뷰). LVM-thin의 볼륨 지우기도 볼륨 LV에 대해 같다.
- 정보: `zfs get -Hp volsize,referenced`. 용량: 데이터셋의 `available`·`used`. 구현: 정보는 `zfs list`(위 `volumes`)의 `volsize`·`referenced`이고, 용량은 `zfs get -Hp -o value available,used`로 크기 `available`+`used`, 남은 공간 `available`이다.
- 크기: `zfs set volsize={size}`(줄이지 않는다, 이미 크면 건너뜀). 구현: 새 크기도 1 MiB 배수로 올린다.
- 스냅샷: `zfs snapshot {zvol}@{tag}`. 여러 디스크를 한 번에 찍는 `snapshot_many(pool, ids, tag)`를 trait에 기본 구현(하나씩)으로 두고 ZFS는 한 명령(`zfs snapshot a@t b@t`, 원자적)으로 덮는다. 구현: 시그니처는 `snapshot_many(pool, ids, format, tag)`이고 `Storages`가 백엔드로 넘긴다. ZFS는 모든 볼륨이 있고 어느 볼륨에도 `tag`가 없는지 먼저 본다(없으면 `VolumeNotFound`, 있으면 `InvalidSpec`). `snapshot_create`도 같은 길이다. 태그는 `A-Za-z0-9_.:-`여야 한다(아니면 `InvalidSpec`).
- 되돌리기: 그 zvol의 가장 최근 스냅샷이 `tag`일 때만 `zfs rollback {zvol}@{tag}`. 아니면 `DriverError::InvalidSpec("zfs can only revert to the latest snapshot of a volume")`. 구현: rollback 뒤 커널 페이지 캐시에 남은 옛 내용이 같은 장치에서 읽혔으므로(CI에서 발견) 곧바로 `blockdev --flushbufs /dev/zvol/{zvol}`로 버퍼를 비운다(`ZfsOps::flush_buffers`, 바꾸는 명령 규칙, 대상은 그 zvol). 실패하면 경고가 아니라 오류다. rollback 전에도 한 번 비우고, 이때 실패하면 되돌리지 않는다.
- 스냅샷 지우기: `zfs destroy {zvol}@{tag}`. 태그 목록: `zfs list -H -t snapshot -o name -d 1 {zvol}`. 구현: 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 zvol을 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷이 곧 태그를 만들 수 있다) 태그가 없어도 일시 오류(`Timeout`)이고, LVM-thin도 스냅샷 LV에 대해 같다. 태그 목록은 만든 순서(`createtxg`)다. 스냅샷이 없는 볼륨을 되돌리려 하면 `InvalidSpec`("snapshot {tag} not found on volume {id}").
- 원본: 태그가 있으면 `/dev/zvol/{zvol}@{tag}`(snapdev=visible), 없으면 zvol 장치. 해제는 할 일 없음. 구현: `size_bytes`는 스냅샷 때의 `volsize`(스냅샷의 `volsize` 속성)이고, 장치 노드는 최대 10초 기다린다. 스냅샷 장치는 읽기 전용으로만 열린다.
- 같은 백엔드 복사: 대상 zvol을 만들고 `qemu-img convert -n`(데이터셋이 달라도 같다).
- `disk_location`: `/dev/zvol/...`, `DiskKind::Block`. 없으면 일시 오류 `VolumeNotActive`. `activate_volume`은 장치 노드가 있는지 확인만. 구현: `activate_volume`은 zvol이 없으면 `VolumeNotFound`(VM을 끈다), 노드가 없으면 `VolumeNotActive`(VM은 `starting`에서 다시 시도). resolve는 LVM과 같은 분류를 쓰고, Linux가 아닌 노드의 ZFS 디스크는 `ResolveError::PoolUnsupported(pool, "zfs pools are not supported on macOS")`다. LVM과 ZFS 사이 복사는 `Storages`의 백엔드 사이 복사(`volume_source` → 대상 `create_volume` → `convert -n`)다.

## 4. 스냅샷 규칙의 일반화

- `uses_lv_snapshots`를 `uses_block_snapshots(pool)`(LvmThin, Zfs)로 바꾼다. 같은 VM의 디스크가 모두 같은 방식이어야 한다: 모두 qcow2 내부, 또는 모두 같은 블록 백엔드 종류(LVM과 ZFS가 섞이면 409). 구현: `snapshots::block_snapshot_kind(pool) -> Option<&'static str>`(`"lvm-thin"`, `"zfs"`), `uses_block_snapshots`, `vm_uses_block_snapshots`(예약). 섞이면 지금처럼 "disks use different snapshot methods"이고 Task도 다시 본다.
- 블록 디스크 VM은 메모리 스냅샷이 없다(409, 예약은 메모리 없이). 구현: 문구는 "VM … has {kind} disks; their snapshots cannot hold the memory state".
- 실행 중 블록 스냅샷: 계획 16의 멈춤 경로(상태가 Running일 때만 `stop` → 디스크 스냅샷 → `cont`, 120초, 가드·재시도·표시)를 그대로 쓰고, 디스크 스냅샷은 `snapshot_many`로 찍는다(ZFS는 원자적). 구현: 디스크를 같은 풀·형식끼리 묶어(처음 나온 순서) 묶음마다 `snapshot_many` 한 번이다(멈추지 않는 꺼진 VM도 같다). 시간을 넘기면 끝난 묶음의 스냅샷만 지우고, 끝나지 않은 묶음의 디스크는 모두 태그를 남긴다(그 안에서 어느 디스크가 끝났는지 모른다). 남긴 태그는 Task가 기억해 뒤에서 `orphan_retry`(60초) 뒤부터 그 간격으로 열 번까지, 그 스냅샷 리소스가 없으면 `snapshot_delete`를 다시 해 본다(꺼진 VM의 블록 스냅샷이 시간을 넘겨도 같다). Task 제한 시간에 걸리거나 재시작으로 중단된 블록 스냅샷 만들기 Task도 정리(`cleanup`)가 리소스를 지우기 전에 그 VM 스펙의 이 노드 블록 디스크로 같은 뒤 정리를 걸어 둔다. 뒤 정리는 실행기가 멈추면 함께 멈춘다. 실행기가 시작할 때도 이 노드의 블록 풀(LVM-thin, ZFS) 볼륨마다 태그를 읽어 UUID 모양인데 스냅샷 리소스가 없는 것을 지운다(opLite 밖의 태그는 둔다, 실행기 시작을 막지 않게 뒤에서). 로그·오류 문구의 "lvm snapshots"는 "block snapshots"가 됐다.
- **되돌리기 제한:** API는 VM의 디스크가 ZFS이고 되돌릴 스냅샷이 그 VM의 가장 최근 스냅샷이 아니면 409 `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에 거절한다. 드라이버도 같은 확인을 한다(경쟁 대비). 구현: `snapshots.rs`의 `check_zfs_latest`. 되돌릴 스냅샷의 `vm_spec` 디스크에 ZFS 풀의 볼륨이 있을 때만 보고, 그 볼륨(이름으로 찾은 볼륨 id) 중 하나를 `vm_spec` 디스크에 담은 더 나중의 스냅샷만 막는다(최종 리뷰: 디스크를 바꾼 뒤의 스냅샷은 그 볼륨의 ZFS 스냅샷이 아니다). 순서는 `status.created_at`(만드는 중이라 없으면 가장 나중), 같으면 id(UUIDv7)다. 웹은 `web/src/vms/snapshots.ts`의 `zfsNotLatest`로 같은 규칙을 보고 되돌리기 버튼을 이유(`snapshots.zfsLatest`)와 함께 막는다(`ReasonButton`). 드라이버는 볼륨의 ZFS 스냅샷 목록의 마지막인지 본다(`InvalidSpec`). API는 VM의 스냅샷 순서만 보므로 opLite 밖에서 만든 스냅샷(자동 스냅샷 도구 등)은 모른다. 그래서 되돌리기 Task가 VM을 끄기 전에 ZFS 디스크마다 `snapshot_tags`의 마지막이 되돌릴 태그인지 보고, 아니면 VM을 건드리지 않고 더 최근 스냅샷의 이름(opLite 스냅샷이면 그 이름, 아니면 태그)과 함께 실패한다. UUID 모양 태그인데 스냅샷 리소스가 없으면 opld가 뒤에서 지우는 opLite 스냅샷의 남은 것이라고 알린다("retry later or restart opld").
- 크기 늘리기: ZFS도 `volsize` 먼저, 실행 중이면 `block_resize`(실제 volsize로).
- 구현(복제 형식, `tasks/clone.rs`의 `copy_format`): 대상이 블록 풀(LVM-thin, ZFS)이면 raw, 블록 볼륨을 파일 풀로 옮기면 qcow2, 파일 풀 사이는 원본 형식 그대로다.

## 5. 노드별 상태

- 마운터: Zfs 풀은 `zfs get -Hp -o value available,used,type {dataset}`(10초). `type`이 `filesystem`(또는 `volume`이 아닌 데이터셋)이면 준비, 용량은 `available`+`used` 합과 `available`. 없으면 이유. macOS는 `"zfs pools are not supported on macOS"`(VM은 이유와 함께 끈다). 구현: 해석은 드라이버의 `parse_zfs_dataset` → `ZfsDataset { kind, space }`. 확인은 LVM과 함께 풀마다 비동기 작업(`block_checks`)으로 돌고 10초가 지나면 `zfs`를 죽인다. 이유 문구는 `"{dataset} is a volume, not a filesystem dataset"`, `"zfs timed out"`, 그 밖에는 `zfs`의 stderr 첫 줄이다. 지원 여부는 `Pools.zfs_supported`(Linux만, 테스트가 바꾼다).
- `MountOps::zfs_dataset(dataset)`.

## 6. 클라이언트

- **oplctl:** `pool create NAME --zfs DATASET`(다른 종류와 함께 못 씀), 목록·상세 종류 `zfs`·위치 `DATASET`, 형식 기본값 raw. 구현: `--shared`·`--nfs-options`와도 함께 못 쓴다(clap). 데이터셋 이름 검사는 API가 한다.
- **웹:** 풀 만들기의 ZFS 종류(데이터셋 필드와 검사), 목록 종류·위치, `poolFormats`가 ZFS면 raw, `snapshotSupport`가 블록 방식 일반화와 ZFS의 되돌리기 제한(가장 최근이 아닌 스냅샷의 되돌리기 메뉴를 이유와 함께 막는다 — `ReasonMenuItem`). 구현: 스냅샷 탭의 되돌리기는 메뉴가 아니라 버튼이므로 `ReasonButton`으로 막는다. `pools.ts`의 `blockSnapshotKind`, `SnapshotSupport`의 `ok.block`(옛 `lvm`), i18n `snapshots.blockHint`(옛 `lvmHint`)·`snapshots.zfsLatest`·`pools.kinds.zfs`·`pools.dataset`·`pools.errors.zfsDataset`.

## 7. 테스트

- **단위:** 타입·이름 검증, 가짜 `ZfsOps`로 드라이버 각 동작(만들기·가져오기·지우기·정보·용량·크기·스냅샷·원자적 다중 스냅샷·최근 스냅샷만 되돌리기·태그·원본), 공용 명령 도우미, API(ZFS 규칙, 섞인 블록 방식 409, 최근이 아닌 되돌리기 409), 마운터 ZFS 상태, 웹·oplctl.
- **실제 ZFS(`zfs-it`, Linux·root):** CI `linux-kvm`에 `zfsutils-linux`, 루프 파일 위 zpool `opltestz`와 데이터셋 `opltestz/opl`(`OPL_TEST_ZFS=opltestz/opl`). 드라이버 통합 테스트와 opld E2E(ZFS 디스크 VM, 실행 중 스냅샷, 최근 스냅샷 되돌리기, 디렉터리 풀로 복제, 지우기). 구현: 기능 `zfs-it`(opl-driver, opld), `crates/opl-driver/tests/zfs_storage.rs`(4개), `crates/opld/tests/zfs.rs`(1개, 풀은 API로 만들고 볼륨은 `--format raw`, 최근이 아닌 되돌리기의 409도 본다. 디스크 내용은 꺼진 VM의 zvol 장치를 `qemu-io`로 보고, 읽기는 `-r` — 스냅샷 장치는 읽기 전용이다). CI는 `zfsutils-linux` 설치, `modprobe zfs`, `udevadm control --reload-rules` 뒤 2G 파일 위에 zpool을 만든다.

## 8. 알려진 한계

- 가장 최근 스냅샷으로만 되돌릴 수 있다(ZFS rollback의 성질 — 더 오래된 스냅샷으로 가려면 그 뒤의 스냅샷을 먼저 지운다).
- 메모리 스냅샷이 없다. 노드 로컬이다. 실행 중 스냅샷 동안 VM이 잠깐 멈춘다.
- zvol 크기는 1 MiB 배수로 올린다.
- ZFS를 바꾸는 명령이 60초를 넘겨 `Timeout`으로 끝나도 명령은 뒤에서 끝까지 돈다(LVM과 같다). 그동안 같은 zvol을 바꾸는 요청은 일시 오류로 끝나고, 이 기록은 opld 프로세스 안의 것이다.
- 데이터셋이 가득 차면 sparse zvol에 쓰기가 실패한다(자리를 미리 잡지 않는다). 가져오기의 공간 사전 확인은 지금처럼 업로드 디렉터리만 본다.
- 웹의 최근 스냅샷 판단은 화면의 목록으로 하므로 그사이 다른 스냅샷이 생기면 서버가 409로 거절한다.
- LVM의 기본 `snapshot_many`(하나씩)가 묶음 중간에 시간을 넘기면 먼저 끝난 디스크의 스냅샷 LV가 남을 수 있다(남긴 태그의 뒤 정리와 실행기 시작 때의 정리가 치운다).
- 지연 태그 정리가 같은 zvol의 새 스냅샷과 겹치면 새 스냅샷이 일시 오류로 실패할 수 있다.
- 호스트의 LVM이 zvol(`/dev/zd*`)이나 LVM-thin 볼륨 LV에서 게스트의 PV를 볼 수 있다. README가 `global_filter`(LV는 기본값 `scan_lvs = 0`)를 권한다.
