# opLite 공유 스토리지 설계 4: Ceph RBD (계획 18)

작성일: 2026-10-29
상위 문서: `2026-10-29-shared-storage-zfs-design.md`, `2026-10-29-shared-storage-lvm-thin-design.md`(블록 바탕), `2026-10-28-shared-storage-nfs-design.md`(§1 분해)

## 1. 목표와 범위

처음 분해(§1, NFS 설계)에서 계획 18은 iSCSI와 Ceph RBD였다. 두 백엔드는 성격이 다르다: RBD는 이미지 만들기·지우기·스냅샷·복제를 모두 API로 하는 진짜 공유 블록 스토리지이고, iSCSI는 관리자가 만든 LUN을 쓰는(또는 그 위에 공유 LVM을 얹는) 모델이다. 이 계획은 **Ceph RBD**만 하고 iSCSI는 계획 19로 미룬다.

| 항목 | 내용 |
|---|---|
| RBD 풀 | `PoolBackend::Rbd { pool, namespace, user, conf }`, **공유**(모든 노드가 같은 이미지를 본다), 볼륨은 raw RBD 이미지 `opl-{id}` |
| 디스크 소스 | 네트워크 블록: QEMU `-blockdev driver=rbd,pool=..,image=..,user=..,conf=..`(커널 매핑 없음) |
| 드라이버 | `RbdStorage`와 `RbdOps`(`rbd` CLI 뒤), 복사는 `qemu-img`의 rbd 지원 |
| 스냅샷 | `rbd snap create/rollback/rm`, 아무 스냅샷으로나 되돌리기(최근 제한 없음), 실행 중 VM은 블록 스냅샷 멈춤 경로 |
| 배치 | 공유 풀이라 VM은 풀을 쓸 수 있는 어느 노드에서나 돈다 |
| 클라이언트 | oplctl `pool create --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`, 웹의 RBD 풀 |

넣지 않는 것: Ceph 클러스터 설치·관리(관리자), `krbd`(커널 매핑), 링크드 클론(`rbd clone`, 복제는 전체 복사), 메모리 스냅샷, macOS(Homebrew QEMU에 rbd가 없다), 라이브 마이그레이션(하위 프로젝트 4 — RBD가 그 바탕이 된다), iSCSI(계획 19).

## 2. 데이터 모델

- `PoolBackend::Rbd { pool: String, namespace: Option<String>, user: Option<String>, conf: Option<String> }`(serde `{"kind":"rbd", ...}`).
  - `pool`·`namespace`: Ceph 이름(`A-Za-z0-9_.-`, 1–63자, `-`로 시작 금지). 구현: `StoragePoolSpec::validate`의 `PoolError::BadRbdName(name)`(400 `invalid_request`, 사용자도 같다), 모르는 필드는 거절. 웹 `cephNameError`(`web/src/storage/pools.ts`)도 같다.
  - `user`: Ceph 클라이언트 id(`client.` 없이, 같은 문자 규칙). 없으면 `admin`. 구현: `.`을 받으므로 이름 규칙만으로는 `client.opl`도 통과해 `--id client.opl`(Ceph에서는 `client.client.opl`)로 접속하게 된다. 그래서 `client.`로 시작하는 사용자는 `PoolError::RbdUserPrefix(user)`(400 `invalid_request`, "write the Ceph user without client.")로 거절한다. 웹 `cephUserError`(`pools.errors.cephUser`)도 같고, oplctl은 서버의 400에 맡긴다. 기본값은 `opl_driver::RBD_DEFAULT_USER`(`"admin"`)이고 `RbdTarget.user`에는 기본값을 채운 값이 들어간다.
  - `conf`: ceph.conf 절대 경로(제어 문자·`,`·`:`·`=` 금지 — QEMU 옵션 문자열에 들어간다). 없으면 Ceph 기본(`/etc/ceph/ceph.conf`). 키링은 conf가 가리키는 기본 위치(관리자가 노드마다 둔다). 구현: `PoolError::BadRbdConf(conf)`(400), 웹 `rbdConfError`. conf가 없으면 `--conf`·`conf=`를 넘기지 않는다.
- RBD 풀은 항상 공유(`is_shared` true, `shared: false`는 400). 볼륨은 raw만(qcow2는 400). `pool_kind_supports_snapshots`는 raw. 구현: 문구는 "rbd pools are always shared"(API `create_pool`이 NFS와 함께 본다, `shared`를 적지 않으면 true로 둔다), "rbd pools hold raw volumes"(`check_pool_format`). 만들기·가져오기의 형식 기본값은 raw(`default_format`). `pool_usable_on`은 NFS처럼 준비됐다는 상태가 있어야 참이다. `pool_root`는 빈 경로이고 파일 저장소는 RBD 풀을 거절한다("rbd pools hold no volume files").
- 이미지 `opl-{id}`, 스냅샷 `opl-{id}@{tag}`. 이미지 사양 `{pool}[/{namespace}]/opl-{id}`.

## 3. 디스크 소스

- `RuntimeDisk`에 네트워크 소스를 더한다: `DiskKind::Rbd`와 `RuntimeDisk.rbd: Option<RbdTarget { pool, namespace, image, user, conf }>`(파일·블록은 `path`, RBD는 `rbd`). serde 기본값으로 옛 형식을 읽는다. 구현: `DiskKind::Rbd`(serde `rbd`), `RbdTarget { pool, namespace, image, user, conf }`(`opl-types::runtime`). RBD 디스크의 `path`는 빈 경로다.
- `opl-qemu` cmdline: `-blockdev driver=rbd,pool=P[,namespace=N],image=I,user=U[,conf=C],node-name=disk{i}-file` 뒤에 `driver=raw`. 값은 이미 검증된 문자만 들어간다. 구현: `rbd_source`가 만들고, `kind`가 `Rbd`인데 `rbd`가 없으면 `CmdlineError::MissingRbdImage(i)`다.
- `disk_location`은 RBD면 `(DiskKind::Rbd, target)`을 돌려준다(경로 대신). resolve가 `RuntimeDisk.rbd`를 채운다. `activate_volume`은 이미지가 있는지 `rbd info`로 확인만(일시 오류는 다시 시도). 구현: `disk_location`은 `DiskLocation { Path(PathBuf, DiskKind), Rbd(RbdTarget) }`를 돌려주고(`opl_driver::DiskLocation`) resolve가 `Rbd`면 `RuntimeDisk.rbd`를 채운다. 이미지가 없으면 `VolumeNotFound`(VM을 끈다), `rbd`가 실패하면(`DriverError::Rbd`, Ceph에 닿지 않음 등) LVM처럼 `PoolUnavailable`(VM은 `starting`에서 다시 시도)이다. Linux가 아닌 노드의 RBD 디스크는 `ResolveError::PoolUnsupported(pool, "rbd pools are not supported on macOS")`(VM을 끈다).

## 4. 드라이버 (`storage/rbd.rs`)

- 명령은 `RbdOps` 뒤(가짜/실제). 실제는 `rbd`(`--id U [--conf C] [--namespace N]`), `BlockTool`(바꾸는 명령은 죽이지 않음, 같은 이미지에 하나씩, 읽기만 죽임, 60초 — 단 `rbd rm`·복사는 큰 이미지에서 오래 걸리므로 600초). 구현: `RbdOps`는 `info`(크기, 없으면 `None`)·`used`·`space`·`create`·`remove`(purge 뒤 rm)·`snapshots`(만든 순서 — `rbd snap ls`의 스냅샷 id 순)·`snapshot`·`rollback`·`remove_snapshot`·`resize`·`changing`이고 실제 구현은 `SystemRbd`, 생성은 `RbdStorage::new(qemu_img, Arc<dyn RbdOps>)`, 연결은 `Storages::with_rbd`(opld `member.rs`). 오류는 `DriverError::Rbd(String)`(`rbd failed: {stderr 첫 줄}`, 영구 — `Lvm`·`Zfs`와 같은 분류). 명령은 `--id U [--conf C]`와 `LC_ALL=C`이고(`--no-progress`는 일부 하위 명령만 받아 붙이지 않는다) 네임스페이스는 이미지 사양 `{pool}/{namespace}/{image}`로 넘긴다. 바꾸는 명령의 대상은 이미지 사양이다. `rbd snap ls`의 시각이 UTC로 나오게 `TZ=UTC`도 준다. 600초는 `snap purge`·`rm`·`snap rollback`이고(되돌리기도 이미지 크기에 비례한다) 나머지는 60초다. 복사·가져오기는 `rbd` 명령이 아니라 `qemu-img`이므로 이 제한 밖이고(다른 백엔드처럼 Task 제한 시간만) 따로 두지 않았다. `rbd info`가 실패하면 `rbd ls`로 이미지가 없는 것인지 본다(오류 문구로는 풀이 없는 것과 가리기 어렵다). 단 `rbd info`가 시간을 넘겼으면 `rbd ls`로 넘어가지 않고 곧바로 `Timeout`이다. `RbdOps`에 `watchers`(`rbd status --format json`의 watcher 수)를 더했다.
- 만들기: `rbd create --size {MiB}M --image-format 2 {spec}`(1 MiB 배수로 올림). 있으면 `VolumeExists`. 구현: `create_volume`·`import_volume`은 빈 경로를 돌려준다.
- 가져오기: 가상 크기로 만든 뒤 `qemu-img convert -n -O raw {src} rbd:...`(rbd 대상은 0으로 읽힌다 — `--target-is-zero`). 실패하면 지운다. 구현: 대상은 `--target-image-opts`와 옵션 문자열 `driver=raw,file.driver=rbd,file.pool=P[,file.namespace=N],file.image=I[,file.snapshot=T],file.user=U[,file.conf=C]`(`image_opts`, 값 안의 `,`는 `,,`)이고, 이때 형식이 그 안에 있으므로 `-O`를 쓰지 않는다(`convert_from(src, force_share, out_format: Option<ImageFormat>)`, `None`이면 `-O` 없음).
- 지우기: `rbd snap purge` 뒤 `rbd rm`. 없으면 `VolumeNotFound`. 진행 중 명령이 있으면 일시 오류. 구현: 이미지가 없을 때 그 이미지를 바꾸는 명령이 아직 돌면(시간을 넘긴 만들기) `VolumeNotFound`가 아니라 `Timeout`(일시)이다. `snap purge` 전에 `rbd status`로 이미지를 연 클라이언트(watcher — 다른 노드의 QEMU 등)를 보고, 있으면 스냅샷을 지우지 않고 일시 오류(`Timeout`)다. 그 사이에 열려 `rbd rm`이 "has watchers"로 실패해도 일시 오류다.
- 정보: `rbd info --format json`(size), `rbd du --format json`(used, 실패하면 size로). 용량: `ceph df --format json`이 필요하므로 `rbd pool stats`가 아니라 `ceph df detail --format json`의 그 풀 `max_avail`·`stored`(없으면 0, 오류면 준비 안 됨). 구현: 해석은 `opl_driver::parse_ceph_df(out, pool) -> RbdSpace { available_bytes, stored_bytes }`(풀이 목록에 없으면 오류). 크기는 `max_avail`+`stored`(넘치면 가장 큰 값), 남은 공간은 `max_avail`이다. Ceph 풀에 바이트 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 `quota_bytes - stored`를 넘지 않는다(마운터의 풀 상태도 같은 해석기를 쓴다).
- 크기: `rbd resize --size {MiB}M`(줄이지 않는다, 이미 크면 건너뜀). 구현: 새 크기도 1 MiB 배수로 올린다.
- 스냅샷: `rbd snap create {spec}@{tag}`. `snapshot_many`는 이미지마다 차례로(RBD는 여러 이미지의 원자적 스냅샷이 없다 — 실행 중 VM은 멈춘 동안 찍으므로 일관된다; 멈춘 VM도 디스크가 바뀌지 않는다). 구현: `snapshot_many`는 기본 구현(하나씩)을 쓴다. 찍기 전에 이미지가 있고 같은 태그가 없는지 본다(없으면 `VolumeNotFound`, 있으면 `InvalidSpec`). 태그는 `A-Za-z0-9_.:-`여야 한다(`InvalidSpec`).
- 되돌리기: `rbd snap rollback {spec}@{tag}`(아무 스냅샷으로나, 더 새 스냅샷은 남는다). 이미지를 연 클라이언트가 없어야 한다(되돌리기는 VM을 먼저 멈춘다). 구현: 스냅샷이 없으면 `InvalidSpec`("snapshot {tag} not found on volume {id}"). rollback 전에 `rbd status`로 watcher가 없는지 보고, 있으면 일시 오류(`Timeout`)다. `activate_volume`(VM 시작)은 그 이미지를 바꾸는 명령이 이 노드에서 아직 돌면(시간을 넘긴 되돌리기 등) 일시 오류라서 늦게 끝나는 rollback과 겹치지 않는다. VM 시작 때는 watcher를 보지 않는다(다시 붙는 VM의 QEMU가 그 watcher다).
- 스냅샷 지우기: `rbd snap rm`. 태그: `rbd snap ls --format json`. 구현: 볼륨이나 태그가 없으면 지운 것으로 보되, 그 이미지를 바꾸는 명령이 아직 돌면 일시 오류(`Timeout`)다(LVM-thin·ZFS와 같다). 태그 목록은 만든 순서다.
- 원본: 태그가 있으면 `rbd:{spec}@{tag}`를 `qemu-img`의 원본으로(읽기 전용). 해제는 할 일 없음. 구현: `rbd:` 이름 대신 위 옵션 문자열(`--image-opts`, 스냅샷은 `file.snapshot=`)이다. `VolumeSource`에 `rbd: Option<RbdTarget>`을 더했고(`path`는 빈 경로, `snapshot`은 RBD 스냅샷 이름), `size_bytes`는 스냅샷이면 그때의 크기(`rbd snap ls`의 `size`)다.
- 복사: 같은 백엔드든 다른 백엔드든 `qemu-img convert`(rbd 원본·대상은 `--image-opts`/`--target-image-opts`로 user·conf를 넘긴다). 구현: 같은 백엔드면 `RbdStorage::copy_volume`(Ceph 풀·사용자가 달라도 같다), 다르면 `Storages`의 백엔드 사이 복사가 대상이 RBD일 때 `--target-image-opts`로 쓴다. 둘 다 `--target-is-zero`.
- 고아 정리: 계획 17의 태그 정리(UUID 모양, 리소스 없음)를 RBD 볼륨에도. 구현: 태그 정리(`sweep_block_tags`)는 실행기가 시작할 때와 그 뒤 30분마다(`TaskRunnerConfig::tag_sweep`, 실행기가 멈추면 함께 멈춘다) 돈다. 노드 로컬 볼륨은 만든 노드가, 공유 풀(RBD)의 볼륨은 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드(`usable_node(cluster, pool, None)`, 없으면 만든 노드)가 맡는다. 노드마다 같은 답이 나와 한 노드만 지운다. 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로 만든 지 30분이 지난 태그만 지운다: `Storage::snapshot_tags_older_than`(기본은 모든 태그), RBD는 `rbd snap ls`의 `timestamp`(C `ctime()` 모양 `Thu Oct 29 09:00:00 2026`, `TZ=UTC`로 받는다)를 `RbdSnapshot.created`에 담아 고르고, 읽을 수 없는 시각의 태그는 지우지 않는다.

## 5. 노드별 상태와 배치

- 마운터: RBD 풀은 `rbd ls --format json`(10초)이 되면 준비, 용량은 §4의 `ceph df`. 실패하면 이유(stderr 첫 줄). macOS는 `"rbd pools are not supported on macOS"`. 구현: `MountOps::rbd_pool(pool, namespace, user, conf) -> Result<RbdSpace, String>`(`rbd ls`가 되면 `ceph df detail`, 둘 다 `--id`·`--conf`), 확인은 LVM·ZFS처럼 풀마다 비동기 작업(`block_checks`)이고 10초(QEMU 확인 포함)가 지나면 `"rbd timed out"`이다. 그 밖의 이유는 stderr 첫 줄이다. 지원 여부는 `Pools.rbd_supported`(Linux만).
- 공유 풀이므로 스케줄러·Task 노드 선택은 계획 15의 공유 규칙(준비된 온라인 노드)을 그대로 쓴다. 볼륨의 `status.node`는 기록용일 뿐 배치를 묶지 않는다. 구현: 일반 코드(`is_shared`)로 이미 그렇다. 스케줄러, 다른 노드가 만든 볼륨의 Task·배치 시험을 더했다. 다만 RBD 볼륨의 볼륨 Task(만들기, 지우기, 쓰는 VM이 없는 크기 늘리기)는 요청을 받은 노드를 앞세우지 않고 늘 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드로 보낸다(`pool_node`): 바꾸는 명령이 아직 돈다는 표시(`changing`)가 노드마다 따로이므로 한 볼륨의 Task가 같은 노드에서 돌아야 그 표시가 뜻을 갖는다. 가져오기는 올린 파일이 있는 노드에서, 스냅샷·VM Task는 지금처럼 VM의 노드에서 돈다.
- QEMU에 rbd 블록 드라이버가 없으면(`qemu-system-* -blockdev help`에 `rbd`가 없거나 Ubuntu에서 `qemu-block-extra` 없음) 마운터가 준비 안 됨과 이유를 남긴다. 구현: `-blockdev help`는 쓰지 않는다. `MountOps::qemu_rbd()`가 VM을 실행하는 QEMU(`SystemMount { qemu_binary }`, `PoolMounter::spawn(cluster, mount_root, qemu_binary, refresh)`)를 `-M none -nodefaults -display none -S -drive driver=rbd,if=none`으로 띄워 stderr에 "Unknown driver 'rbd'"가 있으면 `"qemu has no rbd block driver"`다(드라이버가 있으면 빠진 풀 인자 오류 — QEMU 8.2는 "Parameter 'pool' is missing" — 로 곧바로 끝난다. `-blockdev`는 드라이버를 찾기 전에 인자를 검사한다). 되풀이된 드라이브 인자를 뺀 stderr에 `pool`이나 `image`가 있을 때만 드라이버가 있다고 보고, 그 밖의 결과는 "cannot check the qemu rbd block driver: ..."(확인 실패, 기억하지 않아 다음 새로 고침에서 다시 확인)다. 한 번 확인되면 opld가 다시 시작할 때까지 다시 확인하지 않고, 없으면 `rbd`를 부르지 않는다.

## 6. 스냅샷 규칙

- RBD는 블록 스냅샷 종류 `"rbd"`(`block_snapshot_kind`). 다른 블록 종류(LVM, ZFS)와 섞이면 409, 메모리 409, 예약은 메모리 없이 — 계획 17의 일반 규칙 그대로. 구현: 일반 코드로 됐다(`block_snapshot_kind`에 `Rbd`를 더한 것뿐이다). 웹은 `blockSnapshotKind`에 `rbd`, 안내 문구(`snapshots.blockHint` 등)에 Ceph RBD 풀.
- 되돌리기는 최근 제한이 없다(ZFS만 있다). 구현: `check_zfs_latest`·웹 `zfsNotLatest`는 ZFS 디스크만 보므로 바꾸지 않았다. 더 새 스냅샷은 남는다.
- 실행 중 VM은 계획 16·17의 멈춤 경로(`stop` → `snapshot_many` → `cont`).

## 7. 클라이언트

- **oplctl:** `pool create NAME --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`(다른 종류와 함께 못 씀, `--shared`는 필요 없음 — 항상 공유), 목록·상세의 종류 `rbd`·위치 `POOL[/NAMESPACE]`, 형식 기본값 raw. 구현: `PoolCmd::Create(Box<PoolCreate>)`로 옮겼다(clap 인자가 많아 enum이 커졌다). `--rbd-user`·`--rbd-conf`는 `--rbd`가 필요하고 다른 종류와 함께 못 쓴다. `POOL[/NAMESPACE]`는 빈 이름이나 두 번째 `/`만 oplctl이 거절하고 이름 검사는 API가 한다. `pool get`은 `user`·`conf`가 있으면 보인다.
- **웹:** 풀 만들기의 RBD(풀, 네임스페이스, 사용자, conf — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, `blockSnapshotKind`에 rbd. 구현: `isShared`가 RBD를 늘 공유로 본다(용량을 노드별이 아니라 하나로 보인다). i18n `pools.kinds.rbd`·`pools.kindHint.rbd`·`pools.cephPool`·`pools.namespace`·`pools.cephUser`·`pools.cephConf`·`pools.errors.rbdName`·`pools.errors.cephUser`·`pools.errors.rbdConf`. `pools.deleteDescription`은 Ceph 풀의 이미지도 남는다고 적는다.

## 8. 테스트

- **단위:** 타입·검증, cmdline의 rbd blockdev, `disk_location`·resolve의 RBD, 가짜 `RbdOps`로 드라이버 각 동작, `qemu-img` rbd 인자, 마운터, API·웹·oplctl.
- **실제 Ceph(`rbd-it`, Linux·root):** CI `linux-kvm`에 `microceph`(snap)로 노드 하나짜리 Ceph(루프 디스크 OSD 1개, `size 1`), `rbd` 풀, `qemu-block-extra`. `OPL_TEST_RBD=POOL`. 드라이버 통합 테스트와 opld E2E(RBD 디스크 VM, 실행 중 스냅샷, 오래된 스냅샷으로 되돌리기, 디렉터리 풀로 복제, 지우기). microceph 준비가 CI에서 너무 느리거나 불안정하면(10분 넘게) `rbd-it`를 CI에서 빼고 수동 시험으로 남긴다(설계에 기록). 구현: 기능 `rbd-it`(opl-driver, opld), `crates/opl-driver/tests/rbd_storage.rs`(5개: 만들기·정보·크기·지우기, 가져오기와 실패한 가져오기 정리, 아무 스냅샷으로 되돌리기, 백엔드 안팎 복사, 제한된 사용자), `crates/opld/tests/rbd.rs`(1개, 풀은 API로 만들고 볼륨은 `--format raw`, 메모리 스냅샷의 409, 더 최근 스냅샷이 있을 때 앞 스냅샷으로 되돌리기와 다시 최근으로 되돌리기도 본다. 디스크 내용은 꺼진 VM의 이미지를 `qemu-io`로 본다). 시험은 `client.admin`과 `/etc/ceph/ceph.conf`로 하고, 드라이버 시험 하나(`restricted_user_in_a_namespace_with_its_conf`)는 제한된 사용자·네임스페이스·명시적 conf(`OPL_TEST_RBD_USER`·`OPL_TEST_RBD_NAMESPACE`·`OPL_TEST_RBD_CONF`, 없으면 건너뛴다)로 만들기·용량·스냅샷·되돌리기·복사·지우기를 본다. CI `linux-kvm`은 `ceph-common`·`qemu-block-extra`를 설치하고 microceph(`--channel squid/stable`로 고정, 4G 루프 OSD 하나, `mon_allow_pool_size_one`, 풀 `rbd` size 1)의 ceph.conf·키링을 `/etc/ceph`에 링크한다. 그리고 네임스페이스 `rbd/opl`, 사용자 `client.opltest`(caps `mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd'`)를 만들어 키링은 `/etc/opl-ceph`에 두고 그 키링을 가리키는 `/etc/opl-ceph/ceph.conf`로만 찾게 한다. 그 단계는 15분 제한, 작업 제한은 55분으로 늘렸다. 첫 실행에서 microceph 준비는 75초였으므로 CI에 남겼다.

## 9. 알려진 한계

- 메모리 스냅샷이 없다. 실행 중 스냅샷 동안 VM이 잠깐 멈춘다.
- 여러 디스크 스냅샷은 원자적이지 않다(멈춘 동안 찍으므로 VM 입장에서는 일관된다).
- macOS 노드는 RBD를 쓸 수 없다. Linux 노드에는 `qemu-block-extra`(rbd 블록 드라이버)와 `ceph-common`(rbd CLI), ceph.conf·키링이 있어야 한다.
- 링크드 클론은 없다(복제는 전체 복사).
- RBD 명령이 시간을 넘겨 `Timeout`으로 끝나도 바꾸는 명령은 뒤에서 끝까지 돈다(LVM·ZFS와 같다). 그동안 같은 이미지를 바꾸는 요청은 일시 오류로 끝나고, 이 기록은 opld 프로세스 안의 것이다(노드마다 따로다 — 다른 노드는 모른다).
- QEMU rbd 드라이버 확인은 처음 한 번만 한다(opld가 도는 동안 `qemu-block-extra`를 지우면 VM 시작이 QEMU 오류로 실패한다).
- 같은 Ceph 풀을 네임스페이스만 달리해 여러 opLite 풀이 나눠 쓰면 각 풀이 Ceph 풀 전체의 크기·남은 공간을 보인다(`ceph df`는 네임스페이스별 사용량을 주지 않는다).
- 바꾸는 명령이 돈다는 표시가 노드마다 따로이므로 RBD 볼륨 Task를 한 노드(id가 가장 작은 준비된 노드)로 모으지만, 그 노드가 바뀌는 사이(오프라인 등)나 가져오기·스냅샷·VM Task처럼 다른 노드에서 도는 명령은 서로 모른다. 이미지를 연 클라이언트(watcher) 검사가 지우기·되돌리기를 막아 준다.
- Ceph 풀이 가득 차면 게스트의 쓰기가 실패한다(이미지는 thin이다). 가져오기의 공간 사전 확인은 지금처럼 업로드 디렉터리만 본다.
