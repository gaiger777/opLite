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
  - `pool`·`namespace`: Ceph 이름(`A-Za-z0-9_.-`, 1–63자, `-`로 시작 금지).
  - `user`: Ceph 클라이언트 id(`client.` 없이, 같은 문자 규칙). 없으면 `admin`.
  - `conf`: ceph.conf 절대 경로(제어 문자·`,`·`:`·`=` 금지 — QEMU 옵션 문자열에 들어간다). 없으면 Ceph 기본(`/etc/ceph/ceph.conf`). 키링은 conf가 가리키는 기본 위치(관리자가 노드마다 둔다).
- RBD 풀은 항상 공유(`is_shared` true, `shared: false`는 400). 볼륨은 raw만(qcow2는 400). `pool_kind_supports_snapshots`는 raw.
- 이미지 `opl-{id}`, 스냅샷 `opl-{id}@{tag}`. 이미지 사양 `{pool}[/{namespace}]/opl-{id}`.

## 3. 디스크 소스

- `RuntimeDisk`에 네트워크 소스를 더한다: `DiskKind::Rbd`와 `RuntimeDisk.rbd: Option<RbdTarget { pool, namespace, image, user, conf }>`(파일·블록은 `path`, RBD는 `rbd`). serde 기본값으로 옛 형식을 읽는다.
- `opl-qemu` cmdline: `-blockdev driver=rbd,pool=P[,namespace=N],image=I,user=U[,conf=C],node-name=disk{i}-file` 뒤에 `driver=raw`. 값은 이미 검증된 문자만 들어간다.
- `disk_location`은 RBD면 `(DiskKind::Rbd, target)`을 돌려준다(경로 대신). resolve가 `RuntimeDisk.rbd`를 채운다. `activate_volume`은 이미지가 있는지 `rbd info`로 확인만(일시 오류는 다시 시도).

## 4. 드라이버 (`storage/rbd.rs`)

- 명령은 `RbdOps` 뒤(가짜/실제). 실제는 `rbd`(`--id U [--conf C] [--namespace N]`), `BlockTool`(바꾸는 명령은 죽이지 않음, 같은 이미지에 하나씩, 읽기만 죽임, 60초 — 단 `rbd rm`·복사는 큰 이미지에서 오래 걸리므로 600초).
- 만들기: `rbd create --size {MiB}M --image-format 2 {spec}`(1 MiB 배수로 올림). 있으면 `VolumeExists`.
- 가져오기: 가상 크기로 만든 뒤 `qemu-img convert -n -O raw {src} rbd:...`(rbd 대상은 0으로 읽힌다 — `--target-is-zero`). 실패하면 지운다.
- 지우기: `rbd snap purge` 뒤 `rbd rm`. 없으면 `VolumeNotFound`. 진행 중 명령이 있으면 일시 오류.
- 정보: `rbd info --format json`(size), `rbd du --format json`(used, 실패하면 size로). 용량: `ceph df --format json`이 필요하므로 `rbd pool stats`가 아니라 `ceph df detail --format json`의 그 풀 `max_avail`·`stored`(없으면 0, 오류면 준비 안 됨).
- 크기: `rbd resize --size {MiB}M`(줄이지 않는다, 이미 크면 건너뜀).
- 스냅샷: `rbd snap create {spec}@{tag}`. `snapshot_many`는 이미지마다 차례로(RBD는 여러 이미지의 원자적 스냅샷이 없다 — 실행 중 VM은 멈춘 동안 찍으므로 일관된다; 멈춘 VM도 디스크가 바뀌지 않는다).
- 되돌리기: `rbd snap rollback {spec}@{tag}`(아무 스냅샷으로나, 더 새 스냅샷은 남는다). 이미지를 연 클라이언트가 없어야 한다(되돌리기는 VM을 먼저 멈춘다).
- 스냅샷 지우기: `rbd snap rm`. 태그: `rbd snap ls --format json`.
- 원본: 태그가 있으면 `rbd:{spec}@{tag}`를 `qemu-img`의 원본으로(읽기 전용). 해제는 할 일 없음.
- 복사: 같은 백엔드든 다른 백엔드든 `qemu-img convert`(rbd 원본·대상은 `--image-opts`/`--target-image-opts`로 user·conf를 넘긴다).
- 고아 정리: 계획 17의 태그 정리(UUID 모양, 리소스 없음)를 RBD 볼륨에도.

## 5. 노드별 상태와 배치

- 마운터: RBD 풀은 `rbd ls --format json`(10초)이 되면 준비, 용량은 §4의 `ceph df`. 실패하면 이유(stderr 첫 줄). macOS는 `"rbd pools are not supported on macOS"`.
- 공유 풀이므로 스케줄러·Task 노드 선택은 계획 15의 공유 규칙(준비된 온라인 노드)을 그대로 쓴다. 볼륨의 `status.node`는 기록용일 뿐 배치를 묶지 않는다.
- QEMU에 rbd 블록 드라이버가 없으면(`qemu-system-* -blockdev help`에 `rbd`가 없거나 Ubuntu에서 `qemu-block-extra` 없음) 마운터가 준비 안 됨과 이유를 남긴다.

## 6. 스냅샷 규칙

- RBD는 블록 스냅샷 종류 `"rbd"`(`block_snapshot_kind`). 다른 블록 종류(LVM, ZFS)와 섞이면 409, 메모리 409, 예약은 메모리 없이 — 계획 17의 일반 규칙 그대로.
- 되돌리기는 최근 제한이 없다(ZFS만 있다).
- 실행 중 VM은 계획 16·17의 멈춤 경로(`stop` → `snapshot_many` → `cont`).

## 7. 클라이언트

- **oplctl:** `pool create NAME --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`(다른 종류와 함께 못 씀, `--shared`는 필요 없음 — 항상 공유), 목록·상세의 종류 `rbd`·위치 `POOL[/NAMESPACE]`, 형식 기본값 raw.
- **웹:** 풀 만들기의 RBD(풀, 네임스페이스, 사용자, conf — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, `blockSnapshotKind`에 rbd.

## 8. 테스트

- **단위:** 타입·검증, cmdline의 rbd blockdev, `disk_location`·resolve의 RBD, 가짜 `RbdOps`로 드라이버 각 동작, `qemu-img` rbd 인자, 마운터, API·웹·oplctl.
- **실제 Ceph(`rbd-it`, Linux·root):** CI `linux-kvm`에 `microceph`(snap)로 노드 하나짜리 Ceph(루프 디스크 OSD 1개, `size 1`), `rbd` 풀, `qemu-block-extra`. `OPL_TEST_RBD=POOL`. 드라이버 통합 테스트와 opld E2E(RBD 디스크 VM, 실행 중 스냅샷, 오래된 스냅샷으로 되돌리기, 디렉터리 풀로 복제, 지우기). microceph 준비가 CI에서 너무 느리거나 불안정하면(10분 넘게) `rbd-it`를 CI에서 빼고 수동 시험으로 남긴다(설계에 기록).

## 9. 알려진 한계

- 메모리 스냅샷이 없다. 실행 중 스냅샷 동안 VM이 잠깐 멈춘다.
- 여러 디스크 스냅샷은 원자적이지 않다(멈춘 동안 찍으므로 VM 입장에서는 일관된다).
- macOS 노드는 RBD를 쓸 수 없다. Linux 노드에는 `qemu-block-extra`(rbd 블록 드라이버)와 `ceph-common`(rbd CLI), ceph.conf·키링이 있어야 한다.
- 링크드 클론은 없다(복제는 전체 복사).
