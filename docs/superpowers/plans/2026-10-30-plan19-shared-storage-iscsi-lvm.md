# opLite 계획 19: 공유 스토리지 5 — iSCSI와 공유 LVM 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** iSCSI(또는 FC·멀티패스) 공유 LUN 위의 lvmlockd 공유 VG를 opLite 풀로 쓴다. 볼륨마다 두꺼운 LV를 만들고, 쓰는 노드에서만 배타로 켜며, 노드 사이 잠금은 sanlock(또는 dlm)에 맡긴다.

**Architecture:** `PoolBackend::LvmShared { vg, iscsi }`는 항상 공유이고 볼륨은 raw LV `{vg}/opl-{id}`(블록 장치 `/dev/{vg}/opl-{id}`, 계획 16의 디스크 소스)다. `LvmSharedStorage`가 계획 16의 `LvmOps`·`BlockTool`로 LV를 만들고(만들 때와 크기를 늘릴 때 새 부분을 0으로 채운다) 배타 활성화(`-aey`)로 켠다. opld 마운터가 iSCSI 로그인·`vgchange --lockstart`·VG 잠금 종류를 확인하고, reconciler가 VM이 멈추면 LV를 끄며, Task 실행기가 남은 활성화를 주기적으로 정리한다. 스냅샷은 없다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8, utoipa, clap, QEMU QMP, LVM2 2.03(lvmlockd, sanlock), open-iscsi(`iscsiadm`), util-linux(`blkdiscard`); 웹은 계획 8~18과 같다. 새 외부 의존성은 없다. 실제 시험은 Linux CI의 LIO iSCSI 타깃(targetcli-fb)과 sanlock 공유 VG.

**설계 문서:** `docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md` (전체가 이 계획, "구현:" 메모 포함)
**선행 계획:** 계획 1~18 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `PoolBackend::LvmShared`(항상 공유, raw만, 스냅샷 없음, VG·포털·IQN 검증) | iSCSI 타깃·LUN·공유 VG 만들기, lvmlockd·sanlock 설치(관리자) |
| `LvmSharedStorage`: 두꺼운 LV, 0 채우기(만들기·크기 늘리기), 배타 활성화, 잠금 오류는 일시 오류 | CHAP 인증(타깃의 이니시에이터 ACL로 막는다) |
| `Storage::deactivate_volume`·`active_volumes`, 활성화 수명과 남은 활성화 정리 | LUN을 그대로 디스크로 쓰는 풀(QEMU `iscsi` 드라이버) |
| 마운터의 iSCSI 로그인·잠금 시작·공유 VG 확인, resolve | 공유 LVM의 스냅샷·thin, 풀을 지울 때 로그아웃 |
| 크기 늘리기 Task 동안 VM 시작 대기(모든 백엔드), 공유 LVM 볼륨 Task 노드 | macOS, 라이브 마이그레이션 — 하위 프로젝트 4 |
| oplctl `pool create --lvm-shared`, 웹의 공유 LVM 풀 | |
| 실제 시험(`lvm-shared-it`, CI의 LIO 타깃·open-iscsi·sanlock) | |

## 이전 계획에서 이어받는 사항

- 계획 16(LVM-thin): `LvmOps`·`SystemLvm`, 블록 디스크 소스, `VolumeNotActive`.
- 계획 17(ZFS): 공용 `BlockTool`(바꾸는 명령은 죽이지 않음, 같은 대상에 하나씩).
- 계획 18(Ceph RBD): 항상 공유인 블록 풀의 규칙, 공유 볼륨 Task를 한 노드로 보내기, `tag_sweep` 주기, 실제 시험 CI 단계의 모양.

## 실행 환경

계획 18과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. **실제 시험(`opl-driver`·`opld`의 `lvm-shared-it`)은 Linux에서 root로만 돈다.** CI `linux-kvm` 작업이 `targetcli-fb`·`open-iscsi`·`lvm2-lockd`·`sanlock`을 깔고, 2 GB 파일 LUN을 IQN `iqn.2003-01.org.linux-iscsi.opl:shared`(데모 모드 ACL)로 내보내 127.0.0.1에서 로그인한 뒤, softdog·`use_lvmlockd = 1`·`host_id = 1`로 `wdmd sanlock lvmlockd`를 띄우고 공유 VG `oplshared`를 만든다(`vgcreate --shared`는 처음 잠금 시작 때문에 20초쯤 걸린다). 그다음 `OPL_TEST_LVM_SHARED=oplshared`, `OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared`로 돌린다. lvmlockd를 켜도 기존 로컬 VG(LVM-thin 시험)는 그대로 된다. macOS에서는 컴파일되지 않는다(시험 파일이 Linux 전용).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1·4).

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **공유 LVM 위의 볼륨.** LUN을 그대로 디스크로 쓰면 opLite가 볼륨을 만들거나 키울 수 없으므로, 큰 LUN 하나에 공유 VG를 두고 볼륨마다 LV를 만든다. FC·멀티패스 SAN에도 같은 풀을 쓴다(`iscsi` 없이).
- **잠금은 LVM에.** lvmlockd 공유 VG(sanlock·dlm)가 VG 메타데이터와 LV 배타 활성화를 노드 사이에서 지킨다. 마운터는 잠금 종류가 `sanlock`·`dlm`이 아닌 VG를 준비 안 됨으로 둔다. 다른 노드가 쥔 잠금은 일시 오류다.
- **0 채우기.** 두꺼운 LV는 지운 볼륨의 옛 익스텐트를 받으므로, 만들 때 전체를, 크기를 늘릴 때 늘어난 꼬리를 `blkdiscard -f -z`로 채운다. 늘리기 전 크기를 LV 태그로 남겨 실패해도 다음 켜기·복사 원본·크기 늘리기가 남은 부분을 마저 채운다.
- **활성화 수명.** VM 시작 때 켜고, QEMU가 끝나거나 시작이 실패하면 끈다. opld 시작 때와 30분마다 이 노드에서 쓰지 않는 공유 LV를 끈다.
- **크기 늘리기와 VM 시작.** 디스크를 대상으로 하는 끝나지 않은 크기 늘리기 Task가 있으면 VM 시작이 기다리고, 켜거나 끄는 중인 VM의 크기 늘리기는 409다(모든 백엔드).
- **Task 노드.** 공유 LVM 볼륨 Task는 RBD처럼 준비된 온라인 노드 중 id가 가장 작은 노드로(진행 중 표시가 노드마다 따로라서).
- **혼합 버전 클러스터**는 지원하지 않는다(모든 노드를 올린 뒤 공유 LVM 풀을 만든다).

## 파일 구조

```
crates/opl-types/src/storage.rs                                  # PoolBackend::LvmShared, IscsiTarget, 검증
crates/opl-driver/src/storage.rs, storage/{lvm.rs, blockcmd.rs, storages.rs, rbd.rs, zfs.rs}, lib.rs   # LvmSharedStorage, deactivate_volume, active_volumes
crates/opl-reconciler/src/{resolve.rs, worker.rs}, crates/opld/src/{pools.rs, member.rs}   # 활성화·끄기, 크기 늘리기 대기, 마운터, 연결
crates/opl-api/src/{resources.rs, volumes.rs, snapshots.rs, tasks.rs, tasks/clone.rs}   # 규칙, Task 노드, 남은 활성화 정리
crates/opl-driver/tests/lvm_shared_storage.rs, crates/opld/tests/lvm_shared.rs (새로)   # lvm-shared-it
crates/oplctl/src/cli.rs, web/src/{storage,vms}/..., web/src/i18n/{ko,en}.json
.github/workflows/ci.yml, README.md, docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md, 2026-10-28-shared-storage-nfs-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입과 API 규칙: 공유 LVM 풀

`PoolBackend::LvmShared { vg, iscsi: Option<IscsiTarget { portal, target }> }`(serde `lvm-shared`)과 `PoolError::BadIscsiPortal`·`BadIscsiTarget`(포털은 `호스트[:포트]` 또는 `[IPv6][:포트]`, 포트 1–65535; 타깃은 `iqn.`·`eui.`·`naa.`로 시작하는 소문자·숫자·`.:-` 223바이트 이하; VG는 LVM 이름 규칙), `is_shared`는 항상 참, raw만, 스냅샷 없음, `pool_root`는 `/dev/{vg}`. API: `shared: false`는 400, qcow2는 400, 가져오기 기본 raw, 블록 풀 판단 `block_pool_kind`(복제 형식·실행 중 크기 늘리기), 공유 LVM 디스크 VM의 스냅샷은 409 `snapshot_unsupported`. 이 Task에서 `Storages`·`disk_location`·resolve·마운터는 공유 LVM을 아직 지원하지 않는다는 오류를 낸다. 웹 타입을 다시 만들고 풀 위치에 `VG` / `VG @ PORTAL TARGET`.

**Files:**
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opld/src/pools.rs`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/pools.test.ts`
- Modify: `web/src/storage/pools.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS·RBD 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    let always_shared = match spec.backend {
        PoolBackend::Nfs { .. } => Some("nfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::Dir { .. } | PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => None,
```

을 다음으로 바꾼다:

```rust
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS·RBD·공유 LVM 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    let always_shared = match spec.backend {
        PoolBackend::Nfs { .. } => Some("nfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::LvmShared { .. } => Some("lvm-shared"),
        PoolBackend::Dir { .. } | PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => None,
```

`crates/opl-api/src/snapshots.rs`에서

```rust
/// 디스크 스냅샷을 블록 장치의 스냅샷으로 두는 풀의 종류(`lvm-thin`, `zfs`, `rbd`). 파일 풀(qcow2
/// 내부 스냅샷)이면 `None`.
pub(crate) fn block_snapshot_kind(pool: &StoragePool) -> Option<&'static str> {
```

을 다음으로 바꾼다:

```rust
/// 디스크 스냅샷을 블록 장치의 스냅샷으로 두는 풀의 종류(`lvm-thin`, `zfs`, `rbd`). 파일 풀(qcow2
/// 내부 스냅샷)과 스냅샷이 없는 공유 LVM 풀이면 `None`.
pub(crate) fn block_snapshot_kind(pool: &StoragePool) -> Option<&'static str> {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
    }
```

을 다음으로 바꾼다:

```rust
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::LvmShared { .. } => None,
    }
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        if !pool_kind_supports_snapshots(&pool.spec.backend, volume.spec.format) {
            let only = if uses_block_snapshots(&pool) {
                "raw"
            } else {
                "qcow2"
            };
```

을 다음으로 바꾼다:

```rust
        if !pool_kind_supports_snapshots(&pool.spec.backend, volume.spec.format) {
            // 공유 LVM 풀은 어떤 형식의 스냅샷도 담지 못한다.
            let why = if matches!(pool.spec.backend, PoolBackend::LvmShared { .. }) {
                format!(
                    "disks in storage pool {:?} cannot be snapshotted",
                    pool.name
                )
            } else {
                let only = if uses_block_snapshots(&pool) {
                    "raw"
                } else {
                    "qcow2"
                };
                format!(
                    "only {only} disks in storage pool {:?} can be snapshotted",
                    pool.name
                )
            };
```

`crates/opl-api/src/snapshots.rs`에서

```rust
                format!(
                    "VM {:?} has disk {:?} that cannot be snapshotted; only {only} disks in \
                     storage pool {:?} can be snapshotted",
                    vm.name, disk.volume, pool.name
                ),
```

을 다음으로 바꾼다:

```rust
                format!(
                    "VM {:?} has disk {:?} that cannot be snapshotted; {why}",
                    vm.name, disk.volume
                ),
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        assert_eq!(block_snapshot_kind(&rbd), Some("rbd"));
        assert!(!uses_block_snapshots(&dir));
```

을 다음으로 바꾼다:

```rust
        assert_eq!(block_snapshot_kind(&rbd), Some("rbd"));
        // 공유 LVM 풀은 스냅샷이 없다.
        let shared = pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        });
        assert_eq!(block_snapshot_kind(&shared), None);
        assert!(!uses_block_snapshots(&dir));
```

`crates/opl-api/src/tasks.rs`에서

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::volumes::usable_node;
```

을 다음으로 바꾼다:

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::volumes::{block_pool_kind, usable_node};
```

`crates/opl-api/src/tasks.rs`에서

```rust
/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
```

을 다음으로 바꾼다:

```rust
/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
```

`crates/opl-api/src/tasks.rs`에서

```rust
            let mut size_bytes = size_bytes;
            if uses_block_snapshots(&pool) {
                ctx.storage
```

을 다음으로 바꾼다:

```rust
            let mut size_bytes = size_bytes;
            if block_pool_kind(&pool).is_some() {
                ctx.storage
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
use super::{Ctx, mark_ready, remove_volume};
use crate::snapshots::uses_block_snapshots;
```

을 다음으로 바꾼다:

```rust
use super::{Ctx, mark_ready, remove_volume};
use crate::volumes::block_pool_kind;
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS, RBD)은 raw만 담는다. 블록 볼륨(raw)을 파일
/// 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (uses_block_snapshots(source), uses_block_snapshots(target)) {
        (_, true) => ImageFormat::Raw,
```

을 다음으로 바꾼다:

```rust

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 담는다. 블록 볼륨(raw)을
/// 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (
        block_pool_kind(source).is_some(),
        block_pool_kind(target).is_some(),
    ) {
        (_, true) => ImageFormat::Raw,
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
        assert_eq!(copy_format(&zfs, &dir, raw), qcow2);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(copy_format(&zfs, &dir, raw), qcow2);
        // 스냅샷이 없는 공유 LVM 풀도 raw만 담는 블록 풀이다.
        let shared = pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        });
        assert_eq!(copy_format(&dir, &shared, qcow2), raw);
        assert_eq!(copy_format(&shared, &dir, raw), qcow2);
        assert_eq!(copy_format(&shared, &lvm, raw), raw);
    }
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS, RBD)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    let kind = match pool.spec.backend {
        PoolBackend::LvmThin { .. } => "lvm-thin",
        PoolBackend::Zfs { .. } => "zfs",
        PoolBackend::Rbd { .. } => "rbd",
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => return Ok(()),
    };
```

을 다음으로 바꾼다:

```rust

/// 볼륨이 raw 블록 장치나 이미지인 블록 풀의 종류(`lvm-thin`, `zfs`, `rbd`, `lvm-shared`). 파일
/// 풀(dir, nfs)이면 `None`.
pub(crate) fn block_pool_kind(pool: &StoragePool) -> Option<&'static str> {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } => Some("lvm-thin"),
        PoolBackend::Zfs { .. } => Some("zfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::LvmShared { .. } => Some("lvm-shared"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
    }
}

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    let Some(kind) = block_pool_kind(pool) else {
        return Ok(());
    };
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD)은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            ImageFormat::Raw
        }
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
    }
```

을 다음으로 바꾼다:

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀([`block_pool_kind`])은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    if block_pool_kind(pool).is_some() {
        ImageFormat::Raw
    } else {
        ImageFormat::Qcow2
    }
```

`crates/opl-api/tests/pools.rs`에서

```rust
            "ceph user with client.",
        ),
```

을 다음으로 바꾼다:

```rust
            "ceph user with client.",
        ),
        (
            pool("s", json!({ "kind": "lvm-shared", "vg": "-san" })),
            "shared vg starting with '-'",
        ),
        (
            pool(
                "s",
                json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "fd00::5", "target": "iqn.2003-01.org:opl" } }),
            ),
            "ipv6 portal without brackets",
        ),
        (
            pool(
                "s",
                json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "san", "target": "opl" } }),
            ),
            "target without iqn.",
        ),
```

`crates/opl-api/tests/pools.rs`에서

```rust

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

을 다음으로 바꾼다:

```rust

/// 공유 LVM 풀은 늘 공유이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn lvm_shared_pools_are_always_shared_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let backend = json!({
        "kind": "lvm-shared", "vg": "san",
        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.linux-iscsi.san:opl" }
    });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "s2", "spec": { "backend": backend, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"], "lvm-shared pools are always shared",
        "{body}"
    );
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin
        .post("/storage-pools", pool("s", backend.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"], backend, "{body}");
    env.pool_ready("s").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "s", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "lvm-shared pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=s&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "lvm-shared pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=s&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
}

#[tokio::test]
async fn running_vm_lvm_disk_grows_in_storage_before_qemu() {
```

을 다음으로 바꾼다:

```rust
}

/// 공유 LVM 디스크도 블록 볼륨이므로 스토리지로 먼저 늘린 뒤 QEMU에 알린다.
#[tokio::test]
async fn running_vm_lvm_shared_disk_grows_in_storage_before_qemu() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let shared = PoolBackend::LvmShared {
        vg: "san".into(),
        iscsi: None,
    };
    let (lvs, storage) = block_volumes(&mut env, &[("s", shared, &["s0"])]).await;
    let vm = running_block_vm(&env, &admin, "web", &["s0"]).await;
    storage.round_resizes(4 << 20);

    let (status, task) = admin
        .post("/volumes/s0/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(storage.calls(), [("resize", lvs[0])]);
    assert_eq!(env.hv.disk_size(vm, 0), Some(4 << 20));
}

#[tokio::test]
async fn running_vm_lvm_disk_grows_in_storage_before_qemu() {
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
    assert_eq!(body["code"], "snapshot_unsupported");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(body["code"], "snapshot_unsupported");
}

/// 공유 LVM 디스크는 스냅샷을 담지 못한다. 그런 디스크가 하나라도 있는 VM의 스냅샷은 409다.
#[tokio::test]
async fn lvm_shared_disks_cannot_be_snapshotted() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let backend = PoolBackend::LvmShared {
        vg: "san".into(),
        iscsi: None,
    };
    block_volume(&env, "san", backend.clone(), "s0", ImageFormat::Raw).await;
    block_volume(&env, "san", backend, "s1", ImageFormat::Raw).await;

    let web = vm_with_disks(&admin, "web", &["s0"]).await;
    set_vm_status(&env, web, VmPhase::Running, Some(env.node)).await;
    vm_with_disks(&admin, "mixed", &["root", "s1"]).await;
    for (vm, request) in [
        ("web", json!({ "name": "a" })),
        ("web", json!({ "name": "m", "memory": true })),
        ("mixed", json!({ "name": "a" })),
    ] {
        let (status, body) = admin.post(&format!("/vms/{vm}/snapshots"), request).await;
        assert_eq!(status, StatusCode::CONFLICT, "{vm}: {body}");
        assert_eq!(body["code"], "snapshot_unsupported");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("disks in storage pool \"san\" cannot be snapshotted"),
            "{body}"
        );
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는
/// 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

을 다음으로 바꾼다:

```rust
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`, `lvm-shared` 풀은 공유 VG의 장치 디렉터리 `/dev/{vg}`이다(블록
/// 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

`crates/opl-driver/src/storage.rs`에서

```rust
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
```

을 다음으로 바꾼다:

```rust
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } | PoolBackend::LvmShared { vg, .. } => {
            Path::new("/dev").join(vg)
        }
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
```

`crates/opl-driver/src/storage.rs`에서

```rust
                "rbd pools hold no volume files".into(),
            ));
```

을 다음으로 바꾼다:

```rust
                "rbd pools hold no volume files".into(),
            ));
        }
        PoolBackend::LvmShared { .. } => {
            return Err(DriverError::InvalidSpec(
                "lvm-shared pools hold no volume files".into(),
            ));
```

`crates/opl-driver/src/storage.rs`에서

```rust
        )));
    }
```

을 다음으로 바꾼다:

```rust
        )));
    }
    if let PoolBackend::LvmShared { .. } = &pool.spec.backend {
        return Err(DriverError::InvalidSpec(
            "lvm-shared pools are not supported yet".into(),
        ));
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust

    /// RBD 풀의 디스크는 경로가 아니라 RBD 이미지 `opl-{id}`다. 사용자가 없으면 `admin`이다.
```

을 다음으로 바꾼다:

```rust

    /// 파일 저장소는 공유 LVM 풀도 다루지 않고, 디스크 위치도 아직 정하지 않는다. 풀 루트는 VG의 장치
    /// 디렉터리다.
    #[tokio::test]
    async fn lvm_shared_pools_are_not_file_pools() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p: StoragePool = Resource::new(
            "s",
            StoragePoolSpec {
                backend: PoolBackend::LvmShared {
                    vg: "san".into(),
                    iscsi: None,
                },
                nodes: vec![],
                shared: true,
            },
        );
        assert_eq!(pool_root(&p, mnt.path()), PathBuf::from("/dev/san"));
        let storage = DirStorage::new(fake_tool(tools.path(), "exit 1"), mnt.path());
        let refused = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "lvm-shared pools hold no volume files")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        refused(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        refused(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        refused(storage.capacity(&p).await.unwrap_err());
        match disk_location(&p, mnt.path(), id, f).unwrap_err() {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "lvm-shared pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// RBD 풀의 디스크는 경로가 아니라 RBD 이미지 `opl-{id}`다. 사용자가 없으면 `admin`이다.
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

을 다음으로 바꾼다:

```rust
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. }
            | PoolBackend::LvmShared { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
        | PoolBackend::LvmThin { .. }
        | PoolBackend::Zfs { .. } => None,
    }
```

을 다음으로 바꾼다:

```rust
        | PoolBackend::LvmThin { .. }
        | PoolBackend::Zfs { .. }
        | PoolBackend::LvmShared { .. } => None,
    }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
    Rbd,
}
```

을 다음으로 바꾼다:

```rust
    Rbd,
    /// 아직 다루는 저장소가 없다.
    LvmShared,
}
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            PoolBackend::Rbd { .. } => Kind::Rbd,
        }
```

을 다음으로 바꾼다:

```rust
            PoolBackend::Rbd { .. } => Kind::Rbd,
            PoolBackend::LvmShared { .. } => Kind::LvmShared,
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                .ok_or_else(|| DriverError::InvalidSpec("rbd pools are not supported yet".into())),
        }
```

을 다음으로 바꾼다:

```rust
                .ok_or_else(|| DriverError::InvalidSpec("rbd pools are not supported yet".into())),
            Kind::LvmShared => Err(DriverError::InvalidSpec(
                "lvm-shared pools are not supported yet".into(),
            )),
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

을 다음으로 바꾼다:

```rust

    /// 공유 LVM 풀은 아직 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn lvm_shared_pool_is_unsupported_for_now() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)))
        .with_rbd(Arc::new(Recorder::new("rbd", "/r".as_ref(), &log)));
        let p = pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        });
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "lvm-shared pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&lvm_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Rbd { .. } => Err(DriverError::InvalidSpec("not a zfs pool".into())),
        }
```

을 다음으로 바꾼다:

```rust
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Rbd { .. }
            | PoolBackend::LvmShared { .. } => {
                Err(DriverError::InvalidSpec("not a zfs pool".into()))
            }
        }
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS·RBD를 쓴다(Homebrew QEMU에는 rbd가 없다).
```

을 다음으로 바꾼다:

```rust
        }
        // 공유 LVM 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::LvmShared { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "lvm-shared pools are not supported yet",
            ));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS·RBD를 쓴다(Homebrew QEMU에는 rbd가 없다).
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
                        "rbd pools are not supported on macOS",
                    ));
```

을 다음으로 바꾼다:

```rust
                        "rbd pools are not supported on macOS",
                    ));
                }
                PoolBackend::LvmShared { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "lvm-shared pools are not supported on macOS",
                    ));
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// ZFS 풀 `z`(데이터셋 `tank/opl`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
```

을 다음으로 바꾼다:

```rust

/// 공유 LVM 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지
/// 않는다.
#[tokio::test]
async fn refuses_lvm_shared_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let s: StoragePool = Resource::new(
        "s",
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
            nodes: vec![],
            shared: true,
        },
    );
    cluster.create(&s).await.unwrap();
    let mut root = volume("root", "s");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "s" && why == &"lvm-shared pools are not supported yet"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// ZFS 풀 `z`(데이터셋 `tank/opl`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
```

`crates/opl-types/src/storage.rs` 전체:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::{Node, PoolState};
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::runtime::ImageFormat;

pub type StoragePool = Resource<StoragePoolSpec, StoragePoolStatus>;
pub type Volume = Resource<VolumeSpec, VolumeStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct StoragePoolSpec {
    pub backend: PoolBackend,
    /// 이 풀을 쓸 수 있는 노드. 비어 있으면 모든 노드.
    #[serde(default)]
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD·공유 LVM 풀은 늘 공유다(`is_shared`).
    #[serde(default)]
    pub shared: bool,
}

impl ResourceSpec for StoragePoolSpec {
    const KIND: Kind = Kind::StoragePool;
    type Status = StoragePoolStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case", tag = "kind", deny_unknown_fields)]
pub enum PoolBackend {
    /// 로컬 디렉터리에 이미지 파일을 둔다.
    Dir {
        #[cfg_attr(feature = "openapi", schema(value_type = String))]
        path: PathBuf,
    },
    /// NFS 내보내기를 노드마다 마운트하고 그 안에 이미지 파일을 둔다. 늘 공유 풀이다.
    Nfs {
        /// 호스트 이름 또는 IP. IPv6는 `[...]` 없이 적는다.
        server: String,
        /// 서버의 내보내기 경로(절대 경로)
        export: String,
        /// `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 늘 붙는 `nosuid,nodev,noexec,retry=0` 뒤에
        /// 붙고, 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec` 등)은 받지 않는다. 없으면 커널
        /// 기본값.
        #[serde(default)]
        options: Option<String>,
    },
    /// 노드의 LVM thin pool에 볼륨마다 thin LV(`opl-{id}`, raw)를 둔다. 노드 로컬 풀이고, VG와 thin
    /// pool은 관리자가 노드마다 같은 이름으로 만들어 둔다.
    LvmThin {
        /// 볼륨 그룹 이름
        vg: String,
        /// VG 안의 thin pool LV 이름
        thinpool: String,
    },
    /// 노드의 ZFS 데이터셋 아래에 볼륨마다 sparse zvol(`{dataset}/opl-{id}`, raw)을 둔다. 노드 로컬
    /// 풀이고, 데이터셋은 관리자가 노드마다 같은 이름으로 만들어 둔다.
    Zfs {
        /// zvol을 둘 데이터셋 이름(예: `tank/opl`)
        dataset: String,
    },
    /// Ceph RBD 풀에 볼륨마다 raw 이미지(`opl-{id}`)를 둔다. 모든 노드가 같은 이미지를 보는 공유
    /// 풀이고, Ceph 클러스터와 노드의 ceph.conf·키링은 관리자가 준비해 둔다.
    Rbd {
        /// Ceph 풀 이름
        pool: String,
        /// 풀 안의 RBD 네임스페이스. 없으면 기본 네임스페이스.
        #[serde(default)]
        namespace: Option<String>,
        /// Ceph 클라이언트 id(`client.` 없이). 없으면 `admin`.
        #[serde(default)]
        user: Option<String>,
        /// ceph.conf 절대 경로. 없으면 Ceph 기본(`/etc/ceph/ceph.conf`).
        #[serde(default)]
        conf: Option<String>,
    },
    /// 여러 노드가 함께 보는 LUN 위의 공유 VG(lvmlockd)에 볼륨마다 두꺼운 LV(`opl-{id}`, raw)를 둔다.
    /// 늘 공유 풀이고, 공유 VG와 lvmlockd·sanlock은 관리자가 준비해 둔다.
    LvmShared {
        /// 공유 볼륨 그룹 이름
        vg: String,
        /// VG가 놓인 iSCSI LUN. 있으면 노드가 그 타깃에 로그인한다. 없으면 LUN이 이미 보인다(FC 등).
        #[serde(default)]
        iscsi: Option<IscsiTarget>,
    },
}

/// 노드가 로그인할 iSCSI 타깃
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct IscsiTarget {
    /// `호스트[:포트]` 또는 `[IPv6][:포트]`
    pub portal: String,
    /// 타깃 이름(`iqn.`·`eui.`·`naa.`로 시작)
    pub target: String,
}

impl StoragePoolSpec {
    /// 백엔드별 값 검사. API가 풀을 만들 때 부른다.
    pub fn validate(&self) -> Result<(), PoolError> {
        match &self.backend {
            PoolBackend::Dir { path } => {
                if !path.is_absolute() {
                    return Err(PoolError::RelativePath(path.clone()));
                }
            }
            PoolBackend::Nfs {
                server,
                export,
                options,
            } => {
                if !valid_server(server) {
                    return Err(PoolError::BadServer(server.clone()));
                }
                if !export.starts_with('/') || export.chars().any(char::is_control) {
                    return Err(PoolError::BadExport(export.clone()));
                }
                if let Some(options) = options
                    && (options.is_empty()
                        || options.chars().any(|c| c.is_whitespace() || c.is_control())
                        || options.split(',').any(denied_option))
                {
                    return Err(PoolError::BadOptions(options.clone()));
                }
            }
            PoolBackend::LvmThin { vg, thinpool } => {
                for name in [vg, thinpool] {
                    if !valid_lvm_name(name) {
                        return Err(PoolError::BadLvmName(name.clone()));
                    }
                }
            }
            PoolBackend::Zfs { dataset } => {
                if !valid_zfs_dataset(dataset) {
                    return Err(PoolError::BadZfsDataset(dataset.clone()));
                }
            }
            PoolBackend::Rbd {
                pool,
                namespace,
                user,
                conf,
            } => {
                for name in std::iter::once(pool).chain(namespace).chain(user) {
                    if !valid_ceph_name(name) {
                        return Err(PoolError::BadRbdName(name.clone()));
                    }
                }
                // `rbd --id`와 QEMU `user`는 `client.` 없는 id를 받는다.
                if let Some(user) = user
                    && user.starts_with("client.")
                {
                    return Err(PoolError::RbdUserPrefix(user.clone()));
                }
                if let Some(conf) = conf
                    && (!conf.starts_with('/')
                        || conf.chars().any(|c| c.is_control() || ",:=".contains(c)))
                {
                    return Err(PoolError::BadRbdConf(conf.clone()));
                }
            }
            PoolBackend::LvmShared { vg, iscsi } => {
                if !valid_lvm_name(vg) {
                    return Err(PoolError::BadLvmName(vg.clone()));
                }
                if let Some(IscsiTarget { portal, target }) = iscsi {
                    if !valid_portal(portal) {
                        return Err(PoolError::BadIscsiPortal(portal.clone()));
                    }
                    if !valid_iscsi_target(target) {
                        return Err(PoolError::BadIscsiTarget(target.clone()));
                    }
                }
            }
        }
        Ok(())
    }
}

/// NFS 서버: IPv6 주소이거나, 공백·`:`·`,`·`/`·`[`·`]` 없는 ASCII 이름·IPv4.
/// `mount`의 인자 `server:export`가 옵션으로 읽히지 않게 `-`로 시작하지 않는다.
fn valid_server(server: &str) -> bool {
    if server.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    !server.is_empty()
        && !server.starts_with('-')
        && server
            .bytes()
            .all(|c| c.is_ascii_graphic() && !b":,/[]".contains(&c))
}

/// iSCSI 포털: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS 서버와 같은 규칙이되 IPv6는
/// 포트와 헷갈리지 않게 `[...]`로 감싼다. 포트는 1–65535.
fn valid_portal(portal: &str) -> bool {
    let (host_ok, port) = match portal.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((addr, port)) => (addr.parse::<std::net::Ipv6Addr>().is_ok(), port),
            None => return false,
        },
        None => match portal.find(':') {
            Some(i) => (valid_server(&portal[..i]), &portal[i..]),
            None => (valid_server(portal), ""),
        },
    };
    host_ok
        && (port.is_empty()
            || port.strip_prefix(':').is_some_and(|p| {
                !p.is_empty()
                    && p.bytes().all(|c| c.is_ascii_digit())
                    && p.parse::<u16>().is_ok_and(|n| n > 0)
            }))
}

/// iSCSI 타깃 이름: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`로만 된 223바이트 이하
fn valid_iscsi_target(target: &str) -> bool {
    target.len() <= 223
        && ["iqn.", "eui.", "naa."]
            .iter()
            .any(|p| target.len() > p.len() && target.starts_with(p))
        && target
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".:-".contains(&c))
}

/// LVM 이름(VG, LV): `A-Za-z0-9+_.-`의 1–127자이고 `-`로 시작하지 않는다(명령 옵션으로 읽히지 않게).
/// LVM이 받지 않는 이름(`.`·`..`, `snapshot`·`pvmove`로 시작, [`LVM_RESERVED`]를 담은 것)도 거절한다.
fn valid_lvm_name(name: &str) -> bool {
    (1..=127).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"+_.-".contains(&c))
        && name != "."
        && name != ".."
        && !name.starts_with("snapshot")
        && !name.starts_with("pvmove")
        && !LVM_RESERVED.iter().any(|r| name.contains(r))
}

/// ZFS 데이터셋 이름: 177자 이하이고(zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한
/// 255자 안에 들게) `/`로 나뉜 구성 요소가 비지 않는다. 구성 요소는
/// `A-Za-z0-9_.:-`로만 되고 `-`로 시작하지 않으며(명령 옵션으로 읽히지 않게) `.`·`..`이 아니다.
/// 첫 구성 요소(풀 이름)는 문자로 시작한다.
fn valid_zfs_dataset(name: &str) -> bool {
    name.len() <= 177
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        })
}

/// Ceph 이름(풀, 네임스페이스, 클라이언트 id): `A-Za-z0-9_.-`의 1–63자이고 `-`로 시작하지 않는다
/// (명령 옵션으로 읽히지 않게). QEMU 옵션 문자열에도 그대로 들어간다.
fn valid_ceph_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

/// LVM이 내부 LV 이름에 쓰므로 LV 이름에 담을 수 없는 문자열
const LVM_RESERVED: [&str; 12] = [
    "_cdata", "_cmeta", "_corig", "_mimage", "_mlog", "_pmspare", "_rimage", "_rmeta", "_tdata",
    "_tmeta", "_vorigin", "_vdata",
];

/// 받지 않는 마운트 옵션(`=` 앞 이름, 대소문자 무시). 다른 경로를 연결하거나(`bind`, `rbind`,
/// `move`), 기존 마운트를 바꾸거나(`remount`), 장치·setuid·실행을 허용하거나(`dev`, `suid`, `exec`),
/// 다른 프로그램을 부르는(`helper`, `x-*`, 그중 `X-mount.*`) 옵션, 마운트하지 못해도 뒤에서 다시
/// 시도하며 성공으로 돌아오는(`bg`) 옵션이다. opld는 늘 `nosuid,nodev,noexec,retry=0`을 붙인다.
fn denied_option(option: &str) -> bool {
    let name = option
        .split_once('=')
        .map_or(option, |(name, _)| name)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "bind" | "rbind" | "remount" | "move" | "suid" | "dev" | "exec" | "helper" | "bg"
    ) || name.starts_with("x-")
}

/// 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD·공유 LVM 풀은 `shared` 값과 상관없이 공유이고,
/// LVM-thin·ZFS 풀은 늘 노드 로컬이다.
pub fn is_shared(pool: &StoragePool) -> bool {
    match pool.spec.backend {
        PoolBackend::Dir { .. } => pool.spec.shared,
        PoolBackend::Nfs { .. } | PoolBackend::Rbd { .. } | PoolBackend::LvmShared { .. } => true,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => false,
    }
}

/// 이 백엔드의 `format` 볼륨이 스냅샷을 담을 수 있는지. 파일 풀(dir, nfs)은 qcow2 내부 스냅샷,
/// LVM-thin 풀은 raw 볼륨의 thin 스냅샷, ZFS 풀은 raw zvol의 ZFS 스냅샷, RBD 풀은 raw 이미지의 RBD
/// 스냅샷이다. 공유 LVM 풀은 스냅샷을 담지 못한다.
pub fn pool_kind_supports_snapshots(backend: &PoolBackend, format: ImageFormat) -> bool {
    match backend {
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => format == ImageFormat::Qcow2,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            format == ImageFormat::Raw
        }
        PoolBackend::LvmShared { .. } => false,
    }
}

/// 풀의 노드 목록이 이 노드를 허용하는지(`nodes`가 비었거나 포함)
pub fn pool_listed_on(pool: &StoragePool, node: Uuid) -> bool {
    pool.spec.nodes.is_empty() || pool.spec.nodes.contains(&node)
}

/// 노드가 알린 이 풀의 상태. 상태의 `pool_id`가 풀 id와 다르면(같은 이름으로 다시 만든 풀, 옛 상태)
/// 알리지 않은 것으로 본다.
pub fn pool_state_on<'a>(pool: &StoragePool, node: &'a Node) -> Option<&'a PoolState> {
    node.status
        .pools
        .get(&pool.name)
        .filter(|s| s.pool_id == pool.id)
}

/// 이 노드에서 풀을 쓸 수 있는지: 노드 목록이 허용하고, 노드가 알린 그 풀의 상태가 준비됨.
/// 디렉터리 풀은 노드가 상태를 아직 알리지 않았어도(옛 노드, 첫 새로 고침 전) 쓸 수 있다고 보고
/// 준비되지 않았다고 알렸을 때만 막는다. NFS 풀은 마운트됐다고, LVM-thin 풀은 thin pool이, ZFS 풀은
/// 데이터셋이 있다고, RBD 풀은 Ceph 풀에 닿는다고, 공유 LVM 풀은 공유 VG의 잠금이 시작됐다고 알려야
/// 한다.
pub fn pool_usable_on(pool: &StoragePool, node: &Node) -> bool {
    pool_listed_on(pool, node.id)
        && match pool_state_on(pool, node) {
            Some(state) => state.ready,
            None => matches!(pool.spec.backend, PoolBackend::Dir { .. }),
        }
}

/// 노드에서 풀이 준비되지 않은 이유. 노드가 알린 오류이고, 상태를 아직 알리지 않았으면 그렇게 적는다.
pub fn pool_error_on(pool: &StoragePool, node: &Node) -> String {
    match pool_state_on(pool, node) {
        Some(state) => state.error.clone().unwrap_or_else(|| "not ready".into()),
        None => "no status reported yet".into(),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PoolError {
    #[error("pool path must be absolute: {0:?}")]
    RelativePath(PathBuf),
    #[error("invalid nfs server {0:?}: use a host name or IP address without spaces, ':' or ','")]
    BadServer(String),
    #[error("invalid nfs export {0:?}: use an absolute path")]
    BadExport(String),
    #[error(
        "invalid nfs options {0:?}: use a comma-separated list without spaces, \
         and without bind, rbind, remount, move, suid, dev, exec, helper, bg or x-* options"
    )]
    BadOptions(String),
    #[error(
        "invalid lvm name {0:?}: use 1-127 characters from A-Z, a-z, 0-9, '+', '_', '.' and '-', \
         not starting with '-', 'snapshot' or 'pvmove', other than '.' and '..', and without \
         names lvm reserves such as '_tdata' or '_tmeta'"
    )]
    BadLvmName(String),
    #[error(
        "invalid zfs dataset {0:?}: use at most 177 characters in '/'-separated components of \
         A-Z, a-z, 0-9, '_', '.', ':' and '-', none empty, '.', '..' or starting with '-', \
         and the first (the pool name) starting with a letter"
    )]
    BadZfsDataset(String),
    #[error(
        "invalid ceph name {0:?}: use 1-63 characters from A-Z, a-z, 0-9, '_', '.' and '-', \
         not starting with '-'"
    )]
    BadRbdName(String),
    #[error("invalid ceph user {0:?}: write the Ceph user without client.")]
    RbdUserPrefix(String),
    #[error(
        "invalid ceph conf path {0:?}: use an absolute path without control characters, ',', ':' or '='"
    )]
    BadRbdConf(String),
    #[error(
        "invalid iscsi portal {0:?}: use host[:port] or [ipv6][:port] with a port from 1 to 65535"
    )]
    BadIscsiPortal(String),
    #[error(
        "invalid iscsi target {0:?}: use at most 223 characters from a-z, 0-9, '.', ':' and '-', \
         starting with iqn., eui. or naa."
    )]
    BadIscsiTarget(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct StoragePoolStatus {
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct VolumeSpec {
    /// StoragePool 리소스 이름
    pub pool: String,
    pub size_bytes: u64,
    pub format: ImageFormat,
}

impl ResourceSpec for VolumeSpec {
    const KIND: Kind = Kind::Volume;
    type Status = VolumeStatus;
}

/// 가져올 원본 이미지 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct VolumeStatus {
    pub phase: VolumePhase,
    /// 실제로 디스크에 할당된 크기
    pub allocated_bytes: Option<u64>,
    pub attached_to: Option<Uuid>,
    /// 볼륨을 만든 Task를 실행하는 노드. 그 노드만 시작할 때 남은 고아 볼륨을 정리한다.
    pub node: Option<Uuid>,
}

/// 볼륨 파일이 준비됐는지. VM은 준비된 볼륨만 붙일 수 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VolumePhase {
    /// 파일을 만들거나 가져오는 중
    #[default]
    Pending,
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_pool_deserializes_with_defaults() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "dir", "path": "/var/lib/opl/pools/local" }
        }))
        .unwrap();
        assert_eq!(
            spec.backend,
            PoolBackend::Dir {
                path: "/var/lib/opl/pools/local".into()
            }
        );
        assert!(spec.nodes.is_empty());
        assert!(!spec.shared);
    }

    fn nfs(server: &str, export: &str, options: Option<&str>) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: server.into(),
                export: export.into(),
                options: options.map(Into::into),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn nfs_pool_round_trips_and_options_are_optional() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }
        }))
        .unwrap();
        assert_eq!(spec, nfs("nas.lan", "/srv/vm", None));
        let with = nfs("10.0.0.5", "/srv/vm", Some("vers=4.2,hard"));
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["backend"]["kind"], "nfs");
        assert_eq!(json["backend"]["options"], "vers=4.2,hard");
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            with
        );
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "nfs", "server": "nas", "export": "/x", "opts": "hard" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_backend_values() {
        let dir = |path: &str| StoragePoolSpec {
            backend: PoolBackend::Dir { path: path.into() },
            nodes: vec![],
            shared: false,
        };
        assert_eq!(dir("/srv/pool").validate(), Ok(()));
        assert_eq!(
            dir("pool").validate(),
            Err(PoolError::RelativePath("pool".into()))
        );
        for server in ["nas.lan", "10.0.0.5", "fd00::5", "::1", "NAS-01"] {
            assert_eq!(nfs(server, "/srv/vm", None).validate(), Ok(()), "{server}");
        }
        for bad in [
            "",
            "nas lan",
            "nas:2049",
            "a,b",
            "[fd00::5]",
            "-oremount",
            "a/b",
        ] {
            assert_eq!(
                nfs(bad, "/srv/vm", None).validate(),
                Err(PoolError::BadServer(bad.into())),
                "{bad}"
            );
        }
        for bad in ["", "srv/vm", "/srv/\nvm"] {
            assert_eq!(
                nfs("nas", bad, None).validate(),
                Err(PoolError::BadExport(bad.into()))
            );
        }
        assert_eq!(
            nfs("nas", "/srv/vm", Some("vers=4.2,hard")).validate(),
            Ok(())
        );
        for bad in ["", "vers=4.2, hard", "hard\t"] {
            assert_eq!(
                nfs("nas", "/srv/vm", Some(bad)).validate(),
                Err(PoolError::BadOptions(bad.into()))
            );
        }
        // 마운트의 성격을 바꾸는 옵션(다른 경로 연결, 기존 마운트 변경, 실행 허용 등)은 받지 않는다.
        for bad in [
            "bind",
            "hard,rbind",
            "remount,rw",
            "move",
            "suid",
            "DEV",
            "exec",
            "helper=foo",
            "x-systemd.automount",
            "X-mount.mkdir",
            "vers=4.2,Exec",
            "bg",
            "hard,BG",
        ] {
            assert_eq!(
                nfs("nas", "/srv/vm", Some(bad)).validate(),
                Err(PoolError::BadOptions(bad.into())),
                "{bad}"
            );
        }
        for ok in [
            "nosuid,nodev,noexec",
            "soft,timeo=600",
            "nconnect=4",
            "sec=krb5",
        ] {
            assert_eq!(nfs("nas", "/srv/vm", Some(ok)).validate(), Ok(()), "{ok}");
        }
    }

    #[test]
    fn nfs_pools_are_always_shared() {
        let mut pool = StoragePool::new(
            "p",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/srv/p".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(is_shared(&pool));
        pool.spec = nfs("nas", "/srv/vm", None);
        assert!(is_shared(&pool));
    }

    #[test]
    fn usable_on_needs_the_node_list_and_a_ready_state() {
        use crate::node::NodeSpec;

        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        let mut pool = StoragePool::new("p", nfs("nas", "/srv/vm", None));
        // 상태를 알리지 않은 노드에서는 NFS 풀을 쓸 수 없다. 디렉터리 풀은 쓸 수 있다.
        assert!(pool_listed_on(&pool, node.id));
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(pool_error_on(&pool, &node), "no status reported yet");
        let mut dir = StoragePool::new(
            "d",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/srv/d".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        assert!(pool_usable_on(&dir, &node));
        // 디렉터리 풀도 준비되지 않았다고 알렸거나 노드 목록에 없으면 쓸 수 없다.
        let down = PoolState {
            pool_id: dir.id,
            ready: false,
            error: Some("no such directory".into()),
            ..Default::default()
        };
        node.status.pools.insert("d".into(), down);
        assert!(!pool_usable_on(&dir, &node));
        node.status.pools.get_mut("d").unwrap().pool_id = Uuid::now_v7();
        assert!(pool_usable_on(&dir, &node));
        dir.spec.nodes = vec![Uuid::now_v7()];
        assert!(!pool_usable_on(&dir, &node));
        let failed = PoolState {
            pool_id: pool.id,
            ready: false,
            error: Some("mount.nfs: access denied".into()),
            ..Default::default()
        };
        node.status.pools.insert("p".into(), failed);
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(pool_error_on(&pool, &node), "mount.nfs: access denied");
        node.status.pools.get_mut("p").unwrap().ready = true;
        assert!(pool_usable_on(&pool, &node));
        // 같은 이름의 다른 풀(지우고 다시 만든 풀)이나 pool_id 전의 상태는 알리지 않은 것으로 본다.
        for other in [Uuid::now_v7(), Uuid::nil()] {
            node.status.pools.get_mut("p").unwrap().pool_id = other;
            assert_eq!(pool_state_on(&pool, &node), None);
            assert!(!pool_usable_on(&pool, &node));
            assert_eq!(pool_error_on(&pool, &node), "no status reported yet");
        }
        node.status.pools.get_mut("p").unwrap().pool_id = pool.id;
        assert!(pool_state_on(&pool, &node).is_some_and(|s| s.ready));
        // 노드 목록에 없으면 준비됐다고 알려도 쓸 수 없다.
        pool.spec.nodes = vec![Uuid::now_v7()];
        assert!(!pool_listed_on(&pool, node.id));
        assert!(!pool_usable_on(&pool, &node));
        pool.spec.nodes.push(node.id);
        assert!(pool_usable_on(&pool, &node));
    }

    fn lvm(vg: &str, thinpool: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: vg.into(),
                thinpool: thinpool.into(),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn lvm_thin_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }
        }))
        .unwrap();
        assert_eq!(spec, lvm("vg0", "tp"));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backend"]["kind"], "lvm-thin");
        assert_eq!(json["backend"]["thinpool"], "tp");
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "lvm-thin", "vg": "vg0", "pool": "tp" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_lvm_names() {
        let long = "a".repeat(127);
        for ok in [
            "vg0",
            "opl_vg",
            "a+b.c-d",
            "VG",
            long.as_str(),
            "...",
            "my_snapshot",
            "tdata",
        ] {
            assert_eq!(lvm(ok, "tp").validate(), Ok(()), "{ok}");
            assert_eq!(lvm("vg", ok).validate(), Ok(()), "{ok}");
        }
        let too_long = "a".repeat(128);
        for bad in [
            "",
            "-vg",
            "vg 0",
            "vg/tp",
            "vg:0",
            "그룹",
            too_long.as_str(),
            // LVM이 받지 않는 이름: `.`·`..`, `snapshot`·`pvmove`로 시작, 내부 LV의 접미어 포함
            ".",
            "..",
            "snapshot1",
            "pvmove0",
            "data_tdata",
            "x_tmeta",
            "a_cdata",
            "a_cmeta",
            "a_corig",
            "a_mimage_0",
            "a_mlog",
            "a_pmspare",
            "a_rimage",
            "a_rmeta",
            "a_vorigin",
            "a_vdata",
        ] {
            assert_eq!(
                lvm(bad, "tp").validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
            assert_eq!(
                lvm("vg", bad).validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn lvm_thin_pools_are_node_local_and_need_a_ready_state() {
        use crate::node::NodeSpec;

        let mut pool = StoragePool::new("vg", lvm("vg0", "tp"));
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(!is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        // 디렉터리 풀과 달리 상태를 알리기 전에는 쓸 수 없다.
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("vg".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    #[test]
    fn snapshot_support_depends_on_backend_and_format() {
        let dir = PoolBackend::Dir {
            path: "/srv/p".into(),
        };
        let nfs = nfs("nas", "/srv/vm", None).backend;
        let lvm = lvm("vg0", "tp").backend;
        let zfs = zfs("tank/opl").backend;
        for (backend, qcow2, raw) in [
            (&dir, true, false),
            (&nfs, true, false),
            (&lvm, false, true),
            (&zfs, false, true),
            (&rbd("rbd").backend, false, true),
            (&lvm_shared("vg0", None).backend, false, false),
        ] {
            assert_eq!(
                pool_kind_supports_snapshots(backend, ImageFormat::Qcow2),
                qcow2,
                "{backend:?}"
            );
            assert_eq!(
                pool_kind_supports_snapshots(backend, ImageFormat::Raw),
                raw,
                "{backend:?}"
            );
        }
    }

    fn zfs(dataset: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Zfs {
                dataset: dataset.into(),
            },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn zfs_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "zfs", "dataset": "tank/opl" }
        }))
        .unwrap();
        assert_eq!(spec, zfs("tank/opl"));
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backend"]["kind"], "zfs");
        assert_eq!(json["backend"]["dataset"], "tank/opl");
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "zfs", "pool": "tank/opl" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_zfs_dataset_names() {
        // zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한 255자를 넘지 않게 177자까지다.
        let long = format!("t/{}", "a".repeat(175));
        for ok in [
            "tank",
            "tank/opl",
            "Tank0/a_b.c:d-e/f",
            "t/x-",
            "t/.x",
            "t/9",
            long.as_str(),
        ] {
            assert_eq!(zfs(ok).validate(), Ok(()), "{ok}");
        }
        let too_long = format!("t/{}", "a".repeat(176));
        for bad in [
            "",
            "/tank",
            "tank/",
            "tank//opl",
            // 풀 이름은 문자로 시작한다.
            "0tank",
            "_tank",
            "-tank",
            ".tank",
            // `-`로 시작하는 구성 요소는 명령 옵션으로 읽힐 수 있다.
            "tank/-opl",
            "tank/opl@snap",
            "tank/opl#bm",
            "tank/my opl",
            "tank/opl+",
            "tank/데이터",
            "tank/.",
            "tank/..",
            too_long.as_str(),
        ] {
            assert_eq!(
                zfs(bad).validate(),
                Err(PoolError::BadZfsDataset(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn zfs_pools_are_node_local_and_need_a_ready_state() {
        use crate::node::NodeSpec;

        let mut pool = StoragePool::new("z", zfs("tank/opl"));
        assert!(!is_shared(&pool));
        pool.spec.shared = true;
        assert!(!is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("z".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    fn rbd(pool: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: pool.into(),
                namespace: None,
                user: None,
                conf: None,
            },
            nodes: vec![],
            shared: true,
        }
    }

    /// `rbd` 풀의 `namespace`·`user`·`conf`의 값을 바꾼 spec
    fn rbd_with(
        namespace: Option<&str>,
        user: Option<&str>,
        conf: Option<&str>,
    ) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: "rbd".into(),
                namespace: namespace.map(Into::into),
                user: user.map(Into::into),
                conf: conf.map(Into::into),
            },
            ..rbd("rbd")
        }
    }

    #[test]
    fn rbd_pool_round_trips_with_optional_fields() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "rbd", "pool": "rbd" }, "shared": true
        }))
        .unwrap();
        assert_eq!(spec, rbd("rbd"));
        let full = rbd_with(Some("opl"), Some("opl"), Some("/etc/ceph/opl.conf"));
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(
            json["backend"],
            serde_json::json!({
                "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                "conf": "/etc/ceph/opl.conf"
            })
        );
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            full
        );
        let typo = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "rbd", "pool": "rbd", "id": "admin" }
        }));
        assert!(typo.is_err());
    }

    #[test]
    fn validates_rbd_names_and_conf() {
        let long = "a".repeat(63);
        for ok in ["rbd", "Opl_vm.1-a", "9", long.as_str()] {
            assert_eq!(rbd(ok).validate(), Ok(()), "{ok}");
            assert_eq!(
                rbd_with(Some(ok), Some(ok), None).validate(),
                Ok(()),
                "{ok}"
            );
        }
        let too_long = "a".repeat(64);
        for bad in [
            "",
            // `-`로 시작하면 명령 옵션으로 읽힐 수 있다.
            "-rbd",
            "rbd/opl",
            "rbd@s",
            "a,b",
            "a:b",
            "a=b",
            "my rbd",
            "데이터",
            too_long.as_str(),
        ] {
            let err = Err(PoolError::BadRbdName(bad.into()));
            assert_eq!(rbd(bad).validate(), err, "pool {bad}");
            assert_eq!(rbd_with(Some(bad), None, None).validate(), err, "ns {bad}");
            assert_eq!(
                rbd_with(None, Some(bad), None).validate(),
                err,
                "user {bad}"
            );
        }
        // `rbd --id`와 QEMU `user`는 `client.` 없는 id를 받는다.
        for user in ["client.admin", "client.opl"] {
            let err = rbd_with(None, Some(user), None).validate().unwrap_err();
            assert_eq!(err, PoolError::RbdUserPrefix(user.into()));
            assert!(
                err.to_string()
                    .contains("write the Ceph user without client."),
                "{err}"
            );
        }
        assert_eq!(
            rbd_with(Some("client.a"), Some("clients"), None).validate(),
            Ok(())
        );
        assert_eq!(
            rbd_with(None, None, Some("/etc/ceph/my ceph.conf")).validate(),
            Ok(())
        );
        // conf는 QEMU 옵션 문자열(`conf=...`)과 `rbd:` 이미지 이름에 들어간다.
        for bad in [
            "",
            "ceph.conf",
            "/etc/ceph/a,b.conf",
            "/etc/ceph/a:b.conf",
            "/etc/ceph/a=b.conf",
            "/etc/ceph/\nceph.conf",
        ] {
            assert_eq!(
                rbd_with(None, None, Some(bad)).validate(),
                Err(PoolError::BadRbdConf(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn rbd_pools_are_always_shared() {
        let mut pool = StoragePool::new("r", rbd("rbd"));
        assert!(is_shared(&pool));
        pool.spec.shared = false;
        assert!(is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            crate::node::NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        // 디렉터리 풀과 달리 준비됐다는 상태가 있어야 쓸 수 있다.
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("r".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    fn lvm_shared(vg: &str, iscsi: Option<(&str, &str)>) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: vg.into(),
                iscsi: iscsi.map(|(portal, target)| IscsiTarget {
                    portal: portal.into(),
                    target: target.into(),
                }),
            },
            nodes: vec![],
            shared: true,
        }
    }

    const IQN: &str = "iqn.2003-01.org.linux-iscsi.san:opl";

    #[test]
    fn lvm_shared_pool_round_trips() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "lvm-shared", "vg": "san" }, "shared": true
        }))
        .unwrap();
        assert_eq!(spec, lvm_shared("san", None));
        let full = lvm_shared("san", Some(("10.0.0.5:3260", IQN)));
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(
            json["backend"],
            serde_json::json!({
                "kind": "lvm-shared", "vg": "san",
                "iscsi": { "portal": "10.0.0.5:3260", "target": IQN }
            })
        );
        assert_eq!(
            serde_json::from_value::<StoragePoolSpec>(json).unwrap(),
            full
        );
        for typo in [
            serde_json::json!({ "kind": "lvm-shared", "vg": "san", "thinpool": "tp" }),
            serde_json::json!({
                "kind": "lvm-shared", "vg": "san",
                "iscsi": { "portal": "nas", "target": IQN, "chap": "secret" }
            }),
            serde_json::json!({ "kind": "lvm-shared", "vg": "san", "iscsi": { "portal": "nas" } }),
        ] {
            let res =
                serde_json::from_value::<StoragePoolSpec>(serde_json::json!({ "backend": typo }));
            assert!(res.is_err(), "{typo}");
        }
    }

    #[test]
    fn validates_lvm_shared_vg_portal_and_target() {
        assert_eq!(lvm_shared("san", None).validate(), Ok(()));
        // VG 이름은 LVM-thin과 같은 규칙이다.
        for bad in ["", "-vg", "a/b", "snapshot"] {
            assert_eq!(
                lvm_shared(bad, None).validate(),
                Err(PoolError::BadLvmName(bad.into())),
                "{bad}"
            );
        }
        for ok in [
            "san.lan",
            "san.lan:3260",
            "10.0.0.5",
            "10.0.0.5:1",
            "NAS-01:65535",
            "[fd00::5]",
            "[::1]:3260",
        ] {
            assert_eq!(
                lvm_shared("san", Some((ok, IQN))).validate(),
                Ok(()),
                "{ok}"
            );
        }
        for bad in [
            "",
            "san lan",
            "-oremount",
            "a,b",
            "a/b",
            "san:",
            "san:0",
            "san:65536",
            "san:+80",
            "san:32a",
            "san:3260:1",
            // IPv6는 포트와 헷갈리지 않게 `[...]`로 감싼다.
            "fd00::5",
            "[fd00::5",
            "[nas]:3260",
            "[fd00::5]3260",
            "[fd00::5]:",
            ":3260",
        ] {
            assert_eq!(
                lvm_shared("san", Some((bad, IQN))).validate(),
                Err(PoolError::BadIscsiPortal(bad.into())),
                "{bad}"
            );
        }
        let long = format!("iqn.{}", "a".repeat(219));
        for ok in [
            IQN,
            "eui.02004567a425678d",
            "naa.52004567ba64678d",
            long.as_str(),
        ] {
            assert_eq!(
                lvm_shared("san", Some(("san", ok))).validate(),
                Ok(()),
                "{ok}"
            );
        }
        let too_long = format!("iqn.{}", "a".repeat(220));
        for bad in [
            "",
            "iqn.",
            "opl",
            "IQN.2003-01.org:opl",
            "iqn.2003-01.org:OPL",
            "iqn.2003-01.org:my opl",
            "iqn.2003-01.org:a,b",
            "iqn.2003-01.org:a_b",
            "-iqn.2003",
            too_long.as_str(),
        ] {
            assert_eq!(
                lvm_shared("san", Some(("san", bad))).validate(),
                Err(PoolError::BadIscsiTarget(bad.into())),
                "{bad}"
            );
        }
    }

    #[test]
    fn lvm_shared_pools_are_always_shared_and_need_a_ready_state() {
        let mut pool = StoragePool::new("s", lvm_shared("san", None));
        assert!(is_shared(&pool));
        pool.spec.shared = false;
        assert!(is_shared(&pool));
        let mut node: Node = Resource::new(
            "n1",
            crate::node::NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        assert!(!pool_usable_on(&pool, &node));
        let state = PoolState {
            pool_id: pool.id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert("s".into(), state);
        assert!(pool_usable_on(&pool, &node));
    }

    #[test]
    fn rejects_unknown_backend_fields() {
        let res = serde_json::from_value::<StoragePoolSpec>(serde_json::json!({
            "backend": { "kind": "dir", "path": "/p", "pth": "/typo" }
        }));
        assert!(res.is_err());
    }

    #[test]
    fn volume_spec_round_trips() {
        let spec = VolumeSpec {
            pool: "local".into(),
            size_bytes: 10 << 30,
            format: ImageFormat::Qcow2,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["format"], "qcow2");
        assert_eq!(serde_json::from_value::<VolumeSpec>(json).unwrap(), spec);
    }

    #[test]
    fn statuses_default_missing_fields() {
        let p: StoragePoolStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p, StoragePoolStatus::default());
        let v: VolumeStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(v, VolumeStatus::default());
    }
}
```

`crates/opld/src/pools.rs`에서

```rust
const RBD_UNSUPPORTED: &str = "rbd pools are not supported on macOS";
const QEMU_NO_RBD: &str = "qemu has no rbd block driver";
```

을 다음으로 바꾼다:

```rust
const RBD_UNSUPPORTED: &str = "rbd pools are not supported on macOS";
const LVM_SHARED_UNSUPPORTED: &str = "lvm-shared pools are not supported yet";
const QEMU_NO_RBD: &str = "qemu has no rbd block driver";
```

`crates/opld/src/pools.rs`에서

```rust
                };
            }
```

을 다음으로 바꾼다:

```rust
                };
            }
            PoolBackend::LvmShared { .. } => {
                return Step::State(not_ready(pool.id, LVM_SHARED_UNSUPPORTED.into()));
            }
```

`crates/opld/src/pools.rs`에서

```rust

    /// QEMU에 rbd 블록 드라이버가 없으면 RBD 풀은 그 이유로 준비되지 않았고 `rbd`를 부르지 않는다.
```

을 다음으로 바꾼다:

```rust

    /// 공유 LVM 풀은 아직 확인하지 않고 준비되지 않았다고 적는다.
    #[tokio::test]
    async fn lvm_shared_pools_are_not_ready_yet() {
        let mut s = setup().await;
        let backend = PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        };
        let p = s.pool("s", backend, vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["s"],
            not_ready(p.id, "lvm-shared pools are not supported yet".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    /// QEMU에 rbd 블록 드라이버가 없으면 RBD 풀은 그 이유로 준비되지 않았고 `rbd`를 부르지 않는다.
```

`web/src/storage/PoolsPage.tsx`에서

```tsx

type Kind = StoragePool["spec"]["backend"]["kind"];
```

을 다음으로 바꾼다:

```tsx

/** 만들 수 있는 풀 종류. 공유 LVM 풀 만들기는 아직 없다. */
type Kind = Exclude<StoragePool["spec"]["backend"]["kind"], "lvm-shared">;
```

`web/src/storage/pools.test.ts`에서

```ts
  pool("ceph", { backend: { kind: "rbd", pool: "rbd", ...(namespace && { namespace }) } });
const withPool = (
```

을 다음으로 바꾼다:

```ts
  pool("ceph", { backend: { kind: "rbd", pool: "rbd", ...(namespace && { namespace }) } });
const lvmShared = (iscsi?: { portal: string; target: string }) =>
  pool("san", { backend: { kind: "lvm-shared", vg: "san", ...(iscsi && { iscsi }) } });
const withPool = (
```

`web/src/storage/pools.test.ts`에서

```ts
  expect(poolLocation(rbd("opl"))).toBe("rbd/opl");
});
```

을 다음으로 바꾼다:

```ts
  expect(poolLocation(rbd("opl"))).toBe("rbd/opl");
  expect(poolLocation(lvmShared())).toBe("san");
  expect(poolLocation(lvmShared({ portal: "10.0.0.5:3260", target: "iqn.2003-01.org:opl" }))).toBe(
    "san @ 10.0.0.5:3260 iqn.2003-01.org:opl",
  );
});
```

`web/src/storage/pools.test.ts`에서

```ts
  ]);
});
```

을 다음으로 바꾼다:

```ts
  ]);
  // 공유 LVM 풀도 그렇다.
  const san = (id: string, name: string) => ({
    ...NODE,
    id,
    name,
    status: { ...NODE.status, pools: { san: poolState("san", { available_bytes: GIB }) } },
  });
  expect(poolCapacity(lvmShared(), [san("n1", "node-a"), san("n2", "node-b")])).toEqual([
    { node: null, free: GIB, total: 128 * GIB },
  ]);
});
```

`web/src/storage/pools.ts`에서

```ts
 * 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL`,
 * ZFS 풀은 데이터셋, RBD 풀은 `POOL[/NAMESPACE]`
 */
```

을 다음으로 바꾼다:

```ts
 * 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL`,
 * ZFS 풀은 데이터셋, RBD 풀은 `POOL[/NAMESPACE]`, 공유 LVM 풀은 `VG`(iSCSI면 `VG @ PORTAL TARGET`)
 */
```

`web/src/storage/pools.ts`에서

```ts
  }
  return b.server.includes(":") ? `[${b.server}]:${b.export}` : `${b.server}:${b.export}`;
}

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  const kind = pool.spec.backend.kind;
  return pool.spec.shared === true || kind === "nfs" || kind === "rbd";
}
```

을 다음으로 바꾼다:

```ts
  }
  if (b.kind === "lvm-shared") {
    return b.iscsi ? `${b.vg} @ ${b.iscsi.portal} ${b.iscsi.target}` : b.vg;
  }
  return b.server.includes(":") ? `[${b.server}]:${b.export}` : `${b.server}:${b.export}`;
}

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD·공유 LVM 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  const kind = pool.spec.backend.kind;
  return pool.spec.shared === true || kind === "nfs" || kind === "rbd" || kind === "lvm-shared";
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 612개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `cargo test -p opl-api --test pools --test snapshots`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 291개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): 공유 LVM(iSCSI) 풀 백엔드와 API 규칙"
```

---

### Task 2: 공유 LVM 드라이버

`storage/lvm.rs`에 `LvmSharedStorage::new(qemu_img, ops)`: 두꺼운 LV `opl-{id}`(`lvcreate -an -Zn -Wn`), 만들 때 배타 활성화(`-aey`) 뒤 `blkdiscard -f -z`로 0 채우기(시간 제한 없음), 가져오기·복사는 0 채운 LV에 `qemu-img convert -n --target-is-zero`, 지우기 `lvremove`, 크기 늘리기는 늘리기 전 크기를 LV 태그 `opl-zero-from-{bytes}`로 남기고 늘어난 꼬리를 0 채운 뒤 태그를 지운다. 켜기(`activate_volume`)와 복사 원본(`volume_source`)은 `activate_ready`로 노드 안 표시(`Zeroing`)를 먼저 잡고 남은 0 채우기를 마친 뒤 장치를 내준다. 다른 노드가 쥔 잠금(`locked by other host`·`held by other host`)은 일시 오류(`BlockTool::preferring`으로 경고 줄 뒤의 문구도 찾는다). 새 `Storage::deactivate_volume`(기본 Ok), `VgInfo`·`parse_vgs`, `Storages::with_lvm_shared`, `disk_location`의 `/dev/{vg}/opl-{id}`, opld 연결. 가짜 `LvmOps`로 테스트한다.

**Files:**
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/blockcmd.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/lib.rs`에서

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, RBD_DEFAULT_USER, RbdOps,
    RbdSnapshot, RbdSpace, RbdStorage, Storage, Storages, SystemLvm, SystemRbd, SystemZfs,
    VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo,
    check_pool_root, disk_location, parse_ceph_df, parse_lvs, parse_zfs_dataset, pool_root,
};
```

을 다음으로 바꾼다:

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmSharedStorage, LvmThinStorage,
    RBD_DEFAULT_USER, RbdOps, RbdSnapshot, RbdSpace, RbdStorage, Storage, Storages, SystemLvm,
    SystemRbd, SystemZfs, VgInfo, VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot,
    ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root, disk_location, parse_ceph_df, parse_lvs,
    parse_vgs, parse_zfs_dataset, pool_root,
};
```

`crates/opl-driver/src/storage.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::{DiskKind, ImageFormat, RbdTarget};
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;
use crate::mounts::{MOUNTINFO, nfs_mount_points};

mod blockcmd;
mod lvm;
mod rbd;
mod storages;
mod zfs;

pub use lvm::{
    LvInfo, LvmOps, LvmSharedStorage, LvmThinStorage, SystemLvm, VgInfo, parse_lvs, parse_vgs,
};
pub use rbd::{
    DEFAULT_USER as RBD_DEFAULT_USER, RbdOps, RbdSnapshot, RbdSpace, RbdStorage, SystemRbd,
    parse_ceph_df,
};
pub use storages::Storages;
pub use zfs::{
    SystemZfs, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, parse_zfs_dataset,
};

/// 올린 이미지를 검사하는 `qemu-img info`가 멈추면(이상한 extent 등) 무한정 기다리지 않는다.
const INFO_TIMEOUT: Duration = Duration::from_secs(60);

/// 풀 경로의 파일시스템 호출(있는지 보기·링크·지우기·용량)을 기다리는 시간. hard 마운트한 NFS의
/// 서버가 멈추면 끝나지 않으므로 blocking 스레드에서 돌리고 이 시간이 지나면 `PoolNotResponding`이다.
const FS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeInfo {
    pub virtual_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// 복사할 때 읽을 볼륨 내용([`Storage::volume_source`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSource {
    /// 볼륨 파일 또는 블록 장치. RBD 이미지면 빈 경로다.
    pub path: PathBuf,
    pub format: ImageFormat,
    /// 이미지 안의 내부 스냅샷(qcow2)에서 읽으면 그 이름(`qemu-img convert -l snapshot.name=`).
    /// RBD 이미지면 읽을 RBD 스냅샷의 이름이다.
    pub snapshot: Option<String>,
    /// 읽을 내용의 가상 크기. 백엔드 사이 복사는 대상 볼륨을 이 크기로 만든다. LV 스냅샷이면 그
    /// 스냅샷 때의 크기다(뒤에 볼륨을 되돌려 지금이 더 작을 수 있다).
    pub size_bytes: u64,
    /// RBD 이미지면 그 이미지(`qemu-img`가 `rbd` 드라이버로 연다)
    pub rbd: Option<RbdTarget>,
}

/// 볼륨 파일의 생성, 가져오기, 삭제. 볼륨은 풀 안에서 id로 식별한다.
/// 같은 볼륨 id에 대한 호출은 호출자가 직렬화한다.
#[async_trait]
pub trait Storage: Send + Sync {
    /// 이미 있으면 `VolumeExists`. qemu-img는 크기를 512바이트 단위로 올림하므로
    /// 실제 크기는 `VolumeInfo::virtual_bytes`로 확인한다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// 외부 이미지(VMDK 등)를 변환해 새 볼륨으로 만든다. 실패하면 만들다 만 파일을 남기지 않는다.
    ///
    /// `src`와 그것이 참조하는 VMDK extent 파일은 믿을 수 있는 경로여야 한다.
    /// monolithicFlat VMDK 디스크립터는 호스트의 임의 파일을 extent로 가리킬 수 있다.
    /// 사용자가 올린 파일을 검증하는 것은 API(계획 5)의 책임이다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// `src`가 다른 파일을 참조하지 않는 이미지 하나인지 확인한다(VMDK extent, backing file).
    /// 사용자가 올린 파일을 `import_volume`에 넘기기 전에 부른다.
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError>;

    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError>;

    /// VM이 볼륨을 디스크로 쓸 수 있게 한다([`disk_location`]의 장치를 만든다). 이미 쓸 수 있으면
    /// 아무것도 하지 않는다. 파일 볼륨은 할 일이 없다.
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError>;

    /// `activate_volume`이 이 노드에서 쓸 수 있게 한 것을 되돌린다(VM이 멈춘 뒤). 이미 되돌렸으면
    /// 아무것도 하지 않는다. 기본은 할 일이 없다(공유 LVM 풀만 LV를 끈다).
    async fn deactivate_volume(
        &self,
        _pool: &StoragePool,
        _id: Uuid,
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// VM이 실행 중이어도 조회할 수 있다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError>;

    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError>;

    /// `pool`에 새로 만든 볼륨이 쓰기 전에 0으로 읽히는지. 그러면 새 볼륨에 써 넣을 때 0인 부분을
    /// 쓰지 않는다(`qemu-img convert --target-is-zero`). 파일 풀의 새 파일은 그렇다.
    async fn target_reads_zero(&self, _pool: &StoragePool) -> Result<bool, DriverError> {
        Ok(true)
    }

    /// 이 백엔드가 `pool`의 `format` 볼륨의 스냅샷을 지원하는지. 지원하지 않는 형식에 스냅샷
    /// 메서드를 부르면 `InvalidSpec`.
    fn supports_snapshots(&self, pool: &StoragePool, format: ImageFormat) -> bool;

    /// 정지한 볼륨에 스냅샷 `tag`를 만든다. 같은 태그가 이미 있으면 `InvalidSpec`.
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 같은 풀의 볼륨들(`ids`)에 같은 스냅샷 `tag`를 만든다. 기본은 하나씩 만든다(중간에 실패하면
    /// 앞서 만든 것은 남는다). 여러 볼륨을 한 번에 찍을 수 있는 백엔드(ZFS)는 덮어쓴다.
    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        for &id in ids {
            self.snapshot_create(pool, id, format, tag).await?;
        }
        Ok(())
    }

    /// 정지한 볼륨의 내용을 스냅샷 `tag` 때로 되돌린다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 정지한 볼륨에서 스냅샷 `tag`를 지운다. 태그가 없으면 성공(다시 시도해도 된다).
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 볼륨에 있는 스냅샷 태그들. VM이 실행 중이어도 조회할 수 있다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError>;

    /// 볼륨의 스냅샷 태그 중 만든 지 `age`가 지난 것(만든 순서). 공유 풀의 남은 태그 정리가 다른
    /// 노드에서 막 찍는 스냅샷을 건드리지 않게 쓴다. 기본은 만든 시각을 모르므로 모든 태그다.
    async fn snapshot_tags_older_than(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        _age: Duration,
    ) -> Result<Vec<String>, DriverError> {
        self.snapshot_tags(pool, id, format).await
    }

    /// 정지한 볼륨의 크기를 `size_bytes`로 늘린다. 줄이지는 않는다(실패한다).
    /// 실제 크기는 `volume_info`로 확인한다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError>;

    /// 볼륨(`tag`가 있으면 그 스냅샷 때)의 내용을 읽을 수 있게 하고 그 위치를 돌려준다. 백엔드가 다른
    /// 풀 사이 복사([`Storages`])의 원본이다. 볼륨이 없으면 `VolumeNotFound`, 스냅샷을 지원하지 않는
    /// 형식에 `tag`를 주면 `InvalidSpec`. 다 읽으면 같은 인자로 `release_source`를 부른다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError>;

    /// `volume_source`가 읽을 수 있게 한 것을 되돌린다. 파일 볼륨은 할 일이 없다.
    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError>;

    /// 볼륨(형식 `format`)을 형식 `dst_format`의 새 볼륨 `dst_id`로 복사한다. `tag`가 있으면 그
    /// 스냅샷 때의 내용을, 없으면 지금 내용을 복사한다. `force_share`는 실행 중인 VM이 쥔 볼륨의
    /// 스냅샷을 읽을 때만 쓴다(스냅샷 데이터는 바뀌지 않는다). 대상이 이미 있으면 `VolumeExists`.
    /// 실패하면 만들다 만 파일을 남기지 않는다.
    #[allow(clippy::too_many_arguments)]
    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError>;
}

/// 풀의 볼륨 파일이 놓이는 디렉터리. `dir` 풀은 그 경로, `nfs` 풀은 이 노드의 마운트 지점
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`, `lvm-shared` 풀은 공유 VG의 장치 디렉터리 `/dev/{vg}`이다(블록
/// 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
    match &pool.spec.backend {
        PoolBackend::Dir { path } => path.clone(),
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } | PoolBackend::LvmShared { vg, .. } => {
            Path::new("/dev").join(vg)
        }
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
        PoolBackend::Rbd { .. } => PathBuf::new(),
    }
}

/// mountinfo를 읽는 함수. 테스트는 내용을 주입한다.
type ReadMountinfo = dyn Fn() -> std::io::Result<String> + Send + Sync;

fn read_mountinfo() -> std::io::Result<String> {
    std::fs::read_to_string(MOUNTINFO)
}

/// 풀 루트를 확인해 돌려준다. NFS 풀은 루트가 `/proc/self/mountinfo`의 NFS 마운트 지점이어야 한다.
/// 마운트되지 않았으면(mountinfo를 읽을 수 없거나 Linux가 아니어도) `PoolNotMounted` — 그 자리에
/// 볼륨을 만들거나 디스크로 쓰면 로컬 디스크의 빈 디렉터리를 쓰게 된다.
pub fn check_pool_root(pool: &StoragePool, mount_root: &Path) -> Result<PathBuf, DriverError> {
    check_pool_root_with(pool, mount_root, &read_mountinfo)
}

fn check_pool_root_with(
    pool: &StoragePool,
    mount_root: &Path,
    mountinfo: &ReadMountinfo,
) -> Result<PathBuf, DriverError> {
    let root = pool_root(pool, mount_root);
    // 블록 풀의 볼륨은 파일이 아니다(`/dev/{vg}`, `/dev/zvol/{dataset}`에 파일을 만들지 않는다).
    // RBD 풀의 볼륨은 Ceph 이미지다.
    match &pool.spec.backend {
        PoolBackend::LvmThin { .. } => {
            return Err(DriverError::InvalidSpec(
                "lvm-thin pools hold no volume files".into(),
            ));
        }
        PoolBackend::Zfs { .. } => {
            return Err(DriverError::InvalidSpec(
                "zfs pools hold no volume files".into(),
            ));
        }
        PoolBackend::Rbd { .. } => {
            return Err(DriverError::InvalidSpec(
                "rbd pools hold no volume files".into(),
            ));
        }
        PoolBackend::LvmShared { .. } => {
            return Err(DriverError::InvalidSpec(
                "lvm-shared pools hold no volume files".into(),
            ));
        }
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => {}
    }
    if let PoolBackend::Nfs { .. } = &pool.spec.backend {
        // mountinfo는 실제 경로를 적는다. 마운트 지점은 stat하지 않고(NFS 서버가 멈추면 끝나지 않는다)
        // 로컬 디렉터리인 마운트 루트만 실제 경로로 바꾼다.
        let mounted = std::fs::canonicalize(mount_root)
            .ok()
            .zip(mountinfo().ok())
            .is_some_and(|(real, info)| {
                nfs_mount_points(&info).contains(&real.join(pool.id.to_string()))
            });
        if !mounted {
            return Err(DriverError::PoolNotMounted(root));
        }
    }
    Ok(root)
}

/// VM 디스크의 소스([`disk_location`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskLocation {
    /// 볼륨 파일 또는 블록 장치의 경로와 그 종류(`File`, `Block`)
    Path(PathBuf, DiskKind),
    /// QEMU가 네트워크로 여는 RBD 이미지
    Rbd(RbdTarget),
}

/// VM 디스크로 쓸 볼륨의 위치와 종류. 파일 풀은 풀 루트([`check_pool_root`]로 확인한다) 안의
/// 볼륨 파일이다. LVM-thin 풀은 LV 장치 `/dev/{vg}/opl-{id}`(없으면 `/dev/mapper`의 노드)이고, 장치
/// 노드가 없으면(비활성) `VolumeNotActive`다(활성화는 저장소의 `activate_volume`이 한다). ZFS 풀은
/// zvol 장치 `/dev/zvol/{dataset}/opl-{id}`이고, 노드가 없으면(아직 생기지 않음) `VolumeNotActive`다.
/// 공유 LVM 풀은 LVM-thin과 같은 LV 장치다(이 노드에서 켜야 생긴다). RBD 풀은 경로가 아니라 RBD 이미지 `opl-{id}`다(있는지는 `activate_volume`이 확인한다).
pub fn disk_location(
    pool: &StoragePool,
    mount_root: &Path,
    id: Uuid,
    format: ImageFormat,
) -> Result<DiskLocation, DriverError> {
    disk_location_in(Path::new(lvm::DEV), pool, mount_root, id, format)
}

/// [`disk_location`]. 블록 장치 노드를 `dev` 아래에서 찾는다.
fn disk_location_in(
    dev: &Path,
    pool: &StoragePool,
    mount_root: &Path,
    id: Uuid,
    format: ImageFormat,
) -> Result<DiskLocation, DriverError> {
    if let PoolBackend::LvmThin { vg, .. } = &pool.spec.backend {
        lvm::require_raw(format)?;
        return Ok(DiskLocation::Path(
            lvm::volume_device(dev, vg, id)?,
            DiskKind::Block,
        ));
    }
    if let PoolBackend::Zfs { dataset } = &pool.spec.backend {
        zfs::require_raw(format)?;
        return Ok(DiskLocation::Path(
            zfs::volume_device(dev, dataset, id)?,
            DiskKind::Block,
        ));
    }
    if let PoolBackend::Rbd {
        pool: rbd_pool,
        namespace,
        user,
        conf,
    } = &pool.spec.backend
    {
        rbd::require_raw(format)?;
        return Ok(DiskLocation::Rbd(rbd::volume_target(
            rbd_pool,
            namespace.as_ref(),
            user.as_ref(),
            conf.as_ref(),
            id,
        )));
    }
    if let PoolBackend::LvmShared { vg, .. } = &pool.spec.backend {
        lvm::require_shared_raw(format)?;
        return Ok(DiskLocation::Path(
            lvm::volume_device(dev, vg, id)?,
            DiskKind::Block,
        ));
    }
    check_pool_root(pool, mount_root)?;
    Ok(DiskLocation::Path(
        DirStorage::volume_path(pool, mount_root, id, format),
        DiskKind::File,
    ))
}

/// 파일 풀(`dir`, `nfs`): 풀 루트([`pool_root`]) 안에 `{id}.qcow2` / `{id}.raw` 파일을 두고
/// `qemu-img`로 다룬다. 풀 루트는 hard link를 지원하는 파일시스템이어야 한다.
pub struct DirStorage {
    qemu_img: PathBuf,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`)
    mount_root: PathBuf,
    mountinfo: Arc<ReadMountinfo>,
}

impl DirStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, mount_root: impl Into<PathBuf>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            mount_root: mount_root.into(),
            mountinfo: Arc::new(read_mountinfo),
        }
    }

    pub fn volume_path(
        pool: &StoragePool,
        mount_root: &Path,
        id: Uuid,
        format: ImageFormat,
    ) -> PathBuf {
        pool_root(pool, mount_root).join(format!("{id}.{}", fmt_name(format)))
    }

    /// 풀 루트. qemu-img에 넘기므로 절대 경로여야 하고, NFS 풀은 마운트돼 있어야 한다.
    fn pool_dir(&self, pool: &StoragePool) -> Result<PathBuf, DriverError> {
        let root = check_pool_root_with(pool, &self.mount_root, &*self.mountinfo)?;
        require_absolute(&root)?;
        Ok(root)
    }

    /// 볼륨 파일이 없을 때의 오류. 그사이 NFS 풀의 마운트가 풀렸으면 빈 마운트 지점을 본 것이므로
    /// `PoolNotMounted`다.
    fn not_found(&self, pool: &StoragePool, id: Uuid) -> DriverError {
        match self.pool_dir(pool) {
            Err(e) => e,
            Ok(_) => DriverError::VolumeNotFound(id),
        }
    }

    /// 풀 안의 볼륨 파일 경로. 파일이 있는지는 보지 않는다.
    fn file_path(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.pool_dir(pool)?;
        Ok(Self::volume_path(pool, &self.mount_root, id, format))
    }

    async fn qemu_img(&self, args: &[&str]) -> Result<Vec<u8>, DriverError> {
        run_qemu_img(&self.qemu_img, args).await
    }

    /// 풀 안의 파일시스템 호출. [`on_pool`]에 풀 루트를 넘긴다.
    async fn on_pool<T: Send + 'static>(
        &self,
        pool: &StoragePool,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, DriverError> {
        on_pool(FS_TIMEOUT, &pool_root(pool, &self.mount_root), f).await
    }

    /// 풀 안의 `path`가 있는지
    async fn exists(&self, pool: &StoragePool, path: &Path) -> Result<bool, DriverError> {
        let path = path.to_path_buf();
        self.on_pool(pool, move || path.exists()).await
    }

    /// 있는 볼륨의 경로. 없으면 `VolumeNotFound`.
    async fn existing_path(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = self.file_path(pool, id, format)?;
        if !self.exists(pool, &path).await? {
            return Err(self.not_found(pool, id));
        }
        Ok(path)
    }

    /// 스냅샷을 다룰 볼륨의 경로. 형식이 스냅샷을 지원해야 한다.
    async fn snapshot_path(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        if !self.supports_snapshots(pool, format) {
            return Err(DriverError::InvalidSpec(format!(
                "{} volumes do not support snapshots",
                fmt_name(format)
            )));
        }
        self.existing_path(pool, id, format).await
    }

    /// `qemu-img info`의 JSON 출력.
    async fn info_json(&self, path: &Path, format: ImageFormat) -> Result<Value, DriverError> {
        // -U: 실행 중인 VM이 잡고 있는 이미지 잠금을 무시하고 읽기만 한다.
        let out = self
            .qemu_img(&[
                "info",
                "-U",
                "--output=json",
                "-f",
                fmt_name(format),
                path_str(path)?,
            ])
            .await?;
        serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })
    }

    async fn tags_at(&self, path: &Path, format: ImageFormat) -> Result<Vec<String>, DriverError> {
        let json = self.info_json(path, format).await?;
        // 스냅샷이 없으면 snapshots 항목이 없다.
        let Some(snapshots) = json.get("snapshots") else {
            return Ok(Vec::new());
        };
        snapshots
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                s.get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| DriverError::ToolFailed {
                        tool: "qemu-img",
                        stderr: format!("info output has a snapshot without name: {s}"),
                    })
            })
            .collect()
    }

    /// 풀 안의 임시 파일(`.{id}.{fmt}.{난수}.part`)에 이미지를 만든 뒤 `path`로 하드 링크한다. qemu-img는 기존 파일을
    /// 경고 없이 덮어쓰므로, 같은 볼륨을 동시에 만들어도 먼저 생긴 파일을 덮어쓰지 않게 한다.
    /// 출력 경로는 `before`와 `after` 인자 사이에 들어간다.
    async fn write_new(
        &self,
        pool: &StoragePool,
        id: Uuid,
        path: &Path,
        before: &[&str],
        after: &[&str],
    ) -> Result<(), DriverError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // 호출마다 고유한 이름을 쓴다. opld가 죽은 뒤에도 이전 시도의 qemu-img가 같은 이름의
        // 파일을 쓰고 있을 수 있고, qemu-img는 기존 파일을 잘라 덮어쓰므로 이름을 공유하면
        // 손상된 볼륨이 성공으로 게시될 수 있다. 크래시로 남은 .part는 용량만 차지한다.
        let tmp = TempFile(path.with_file_name(format!(".{name}.{}.part", Uuid::new_v4())));
        let mut args = before.to_vec();
        args.push(path_str(&tmp.0)?);
        args.extend_from_slice(after);
        let linked = match self.qemu_img(&args).await {
            Ok(_) => {
                let (from, to) = (tmp.0.clone(), path.to_path_buf());
                self.on_pool(pool, move || std::fs::hard_link(from, to))
                    .await
            }
            Err(e) => Err(e),
        };
        // 임시 파일은 결과와 상관없이 기다려서 지운다(이 future가 버려지면 drop이 지운다).
        tmp.remove(self, pool).await;
        match linked? {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(DriverError::VolumeExists(id))
            }
            other => Ok(other?),
        }
    }
}

/// `qemu-img`를 돌려 표준 출력을 돌려준다. 실패하면 stderr를 담은 `ToolFailed`.
async fn run_qemu_img(qemu_img: &Path, args: &[&str]) -> Result<Vec<u8>, DriverError> {
    let out = tokio::process::Command::new(qemu_img)
        .args(args)
        .stdin(std::process::Stdio::null())
        // 호출자가 취소하면 qemu-img도 끝낸다.
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() {
        return Err(DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

/// 가져올 원본 `src`의 가상 크기(`qemu-img info`). 블록 풀은 이 크기로 볼륨을 만든 뒤 써 넣는다.
async fn source_virtual_size(
    qemu_img: &Path,
    src: &Path,
    src_format: SourceFormat,
) -> Result<u64, DriverError> {
    let args = [
        "info",
        "--output=json",
        "-f",
        src_fmt_name(src_format),
        path_str(src)?,
    ];
    let out = tokio::time::timeout(INFO_TIMEOUT, run_qemu_img(qemu_img, &args))
        .await
        .map_err(|_| DriverError::Timeout("qemu-img info"))??;
    serde_json::from_slice::<Value>(&out)
        .ok()
        .and_then(|json| json.get("virtual-size").and_then(Value::as_u64))
        .ok_or_else(|| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: "info output has no virtual-size".into(),
        })
}

/// `qemu-img convert`가 `src`를 읽어 `out_format`으로 쓰게 하는 인자. 출력 경로는 뒤에 붙인다.
/// 대상을 옵션 문자열로 줄 때(`--target-image-opts`, RBD 이미지)는 형식도 그 안에 있으므로
/// `out_format`이 `None`이다(`-O`를 함께 쓰면 qemu-img가 거부한다). RBD 원본은 옵션
/// 문자열(`--image-opts`)로 연다. `force_share`면 실행 중인 VM이 쥔 이미지의 잠금을 무시하고
/// 읽는다(`-U`).
fn convert_from(
    src: &VolumeSource,
    force_share: bool,
    out_format: Option<ImageFormat>,
) -> Result<Vec<String>, DriverError> {
    let mut args = match &src.rbd {
        Some(_) => vec!["--image-opts".to_string()],
        None => vec!["-f".to_string(), fmt_name(src.format).to_string()],
    };
    if force_share {
        args.push("-U".into());
    }
    // `-l`의 값은 옵션 문자열이므로 `,`를 `,,`로 쓴다. RBD 스냅샷은 옵션 문자열 안에 있다.
    if let (Some(name), None) = (&src.snapshot, &src.rbd) {
        args.push("-l".into());
        args.push(format!("snapshot.name={}", name.replace(',', ",,")));
    }
    if let Some(out_format) = out_format {
        args.push("-O".into());
        args.push(fmt_name(out_format).into());
    }
    match &src.rbd {
        Some(image) => args.push(rbd::image_opts(image, src.snapshot.as_deref())),
        None => args.push(path_str(&src.path)?.into()),
    }
    Ok(args)
}

/// `f`를 blocking 스레드에서 돌린다. 멈춘 NFS에서 끝나지 않는 호출이 tokio 워커를 붙잡지 않게
/// 하고, `timeout`이 지나면 기다리지 않고 `PoolNotResponding(root)`(일시 오류)를 돌려준다.
/// 끝나지 않은 호출은 blocking 스레드에 남는다.
async fn on_pool<T: Send + 'static>(
    timeout: Duration,
    root: &Path,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DriverError> {
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(f)).await {
        Ok(joined) => joined.map_err(|e| DriverError::Io(std::io::Error::other(e))),
        Err(_) => Err(DriverError::PoolNotResponding(root.to_path_buf())),
    }
}

/// drop되면 파일을 지운다. 실패하거나 취소돼도 임시 파일이 남지 않게 한다.
struct TempFile(PathBuf);

impl TempFile {
    /// 기다려서 지운다. 결과는 보지 않는다.
    async fn remove(mut self, storage: &DirStorage, pool: &StoragePool) {
        let path = std::mem::take(&mut self.0);
        let _ = storage
            .on_pool(pool, move || std::fs::remove_file(path))
            .await;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        // `remove`로 이미 지웠다.
        if self.0.as_os_str().is_empty() {
            return;
        }
        // 취소된 경우. 멈춘 NFS에서 워커를 붙잡지 않게 blocking 스레드에서 지운다.
        let path = std::mem::take(&mut self.0);
        let remove = move || {
            let _ = std::fs::remove_file(path);
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => drop(rt.spawn_blocking(remove)),
            Err(_) => remove(),
        }
    }
}

/// qemu-img는 상대 경로를 `json:{...}`, `nbd:` 같은 프로토콜로 해석할 수 있으므로 절대 경로만 받는다.
fn require_absolute(p: &Path) -> Result<(), DriverError> {
    if p.is_absolute() {
        Ok(())
    } else {
        Err(DriverError::InvalidSpec(format!(
            "path must be absolute: {p:?}"
        )))
    }
}

fn fmt_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    }
}

/// `--image-opts`로 줄 옵션 문자열. `qemu-img snapshot`은 QEMU 8.2에서 `-f`를 받지 않으므로
/// 형식을 추측하지 않게 드라이버를 이렇게 정한다. 옵션 값 안의 `,`는 `,,`로 쓴다.
fn image_opts(path: &Path, format: ImageFormat) -> Result<String, DriverError> {
    Ok(format!(
        "driver={},file.filename={}",
        fmt_name(format),
        path_str(path)?.replace(',', ",,")
    ))
}

fn src_fmt_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Vmdk => "vmdk",
        SourceFormat::Qcow2 => "qcow2",
        SourceFormat::Raw => "raw",
    }
}

/// `qemu-img info`가 다른 파일이나 연결을 열기 전에 헤더만 보고 거부한다.
/// info는 보고하려고 VMDK extent와 qcow2 외부 데이터 파일을 실제로 연다.
async fn check_source_header(src: &Path, src_format: SourceFormat) -> Result<(), DriverError> {
    use tokio::io::AsyncReadExt;

    let mut header = Vec::with_capacity(80);
    tokio::fs::File::open(src)
        .await?
        .take(80)
        .read_to_end(&mut header)
        .await?;
    match src_format {
        // 텍스트 디스크립터는 다른 파일만 가리킬 수 있다.
        SourceFormat::Vmdk if header.starts_with(b"# Disk DescriptorFile") => {
            Err(DriverError::InvalidSpec(
                "descriptor-only VMDK cannot be imported; \
                 upload a monolithicSparse or streamOptimized VMDK"
                    .into(),
            ))
        }
        // 헤더는 빅엔디언이다. 매직이 틀리면 qemu-img가 거부한다.
        SourceFormat::Qcow2 if header.len() >= 16 && header.starts_with(b"QFI\xfb") => {
            let be32 = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
            let be64 = |at: usize| u64::from_be_bytes(header[at..at + 8].try_into().unwrap());
            if be64(8) != 0 {
                return Err(DriverError::InvalidSpec(
                    "qcow2 image has a backing file".into(),
                ));
            }
            // incompatible_features bit 2: 외부 데이터 파일 (version 3부터)
            if be32(4) >= 3 && header.len() >= 80 && be64(72) & 4 != 0 {
                return Err(DriverError::InvalidSpec(
                    "qcow2 image has an external data file".into(),
                ));
            }
            Ok(())
        }
        SourceFormat::Vmdk | SourceFormat::Qcow2 | SourceFormat::Raw => Ok(()),
    }
}

/// [`Storage::check_import_source`]: 헤더를 본 뒤 `qemu-img info`로 이미지가 `src` 말고 다른 파일을
/// 참조하지 않는지 확인한다.
async fn check_image(
    qemu_img: &Path,
    src: &Path,
    src_format: SourceFormat,
) -> Result<(), DriverError> {
    require_absolute(src)?;
    check_source_header(src, src_format).await?;
    let src = path_str(src)?;
    let args = ["info", "--output=json", "-f", src_fmt_name(src_format), src];
    // 시간이 지나 future가 버려지면 kill_on_drop이 qemu-img를 끝낸다.
    let out = tokio::time::timeout(INFO_TIMEOUT, run_qemu_img(qemu_img, &args))
        .await
        .map_err(|_| DriverError::Timeout("qemu-img info"))??;
    let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
        tool: "qemu-img",
        stderr: format!("unparseable info output: {e}"),
    })?;
    // 이미지 자신과 그 하위 노드(extent 등)가 가리키는 파일이 모두 src여야 한다.
    let mut files = Vec::new();
    referenced_files(&json, &mut files);
    match files.into_iter().find(|f| f != src) {
        None => Ok(()),
        Some(other) => Err(DriverError::InvalidSpec(format!(
            "image refers to another file: {other}"
        ))),
    }
}

/// `qemu-img info` 출력에서 이미지가 여는(열 수 있는) 파일 이름을 모두 모은다.
fn referenced_files(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    (
                        "filename" | "backing-filename" | "full-backing-filename" | "data-file",
                        Value::String(f),
                    ) => out.push(f.clone()),
                    _ => referenced_files(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| referenced_files(v, out)),
        _ => {}
    }
}

fn path_str(p: &Path) -> Result<&str, DriverError> {
    p.to_str()
        .ok_or_else(|| DriverError::InvalidSpec(format!("path is not UTF-8: {p:?}")))
}

#[async_trait]
impl Storage for DirStorage {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = self.file_path(pool, id, format)?;
        if self.exists(pool, &path).await? {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.to_string();
        self.write_new(
            pool,
            id,
            &path,
            &["create", "-q", "-f", fmt_name(format)],
            &[&size],
        )
        .await?;
        Ok(path)
    }

    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = self.file_path(pool, id, format)?;
        require_absolute(src)?;
        if self.exists(pool, &path).await? {
            return Err(DriverError::VolumeExists(id));
        }
        let before = [
            "convert",
            "-q",
            "-f",
            src_fmt_name(src_format),
            "-O",
            fmt_name(format),
            path_str(src)?,
        ];
        self.write_new(pool, id, &path, &before, &[]).await?;
        Ok(path)
    }

    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let path = self.file_path(pool, id, format)?;
        match self
            .on_pool(pool, move || std::fs::remove_file(path))
            .await?
        {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(self.not_found(pool, id)),
            other => Ok(other?),
        }
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let path = self.existing_path(pool, id, format).await?;
        let json = self.info_json(&path, format).await?;
        let field = |k: &str| {
            json.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| DriverError::ToolFailed {
                    tool: "qemu-img",
                    stderr: format!("info output has no {k}: {json}"),
                })
        };
        Ok(VolumeInfo {
            virtual_bytes: field("virtual-size")?,
            allocated_bytes: field("actual-size")?,
        })
    }

    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let path = self.pool_dir(pool)?;
        let space = move || -> std::io::Result<Capacity> {
            Ok(Capacity {
                total_bytes: fs4::total_space(&path)?,
                available_bytes: fs4::available_space(&path)?,
            })
        };
        Ok(self.on_pool(pool, space).await??)
    }

    async fn activate_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// 내부 스냅샷은 qcow2만 담을 수 있다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format).await?;
        // qemu-img는 이름이 겹치는 스냅샷도 만든다.
        if self.tags_at(&path, format).await?.iter().any(|t| t == tag) {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        let opts = image_opts(&path, format)?;
        self.qemu_img(&["snapshot", "-q", "--image-opts", "-c", tag, &opts])
            .await?;
        Ok(())
    }

    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format).await?;
        let opts = image_opts(&path, format)?;
        self.qemu_img(&["snapshot", "-q", "--image-opts", "-a", tag, &opts])
            .await?;
        Ok(())
    }

    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format).await?;
        // `-d`는 태그가 없으면 실패하므로 먼저 확인한다. 겹친 태그가 있으면 모두 지운다.
        let count = self
            .tags_at(&path, format)
            .await?
            .iter()
            .filter(|t| *t == tag)
            .count();
        let opts = image_opts(&path, format)?;
        for _ in 0..count {
            self.qemu_img(&["snapshot", "-q", "--image-opts", "-d", tag, &opts])
                .await?;
        }
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let path = self.snapshot_path(pool, id, format).await?;
        self.tags_at(&path, format).await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let path = self.existing_path(pool, id, format).await?;
        let size = size_bytes.to_string();
        // --shrink 없이는 qemu-img가 줄이기를 거부한다.
        self.qemu_img(&[
            "resize",
            "-q",
            "-f",
            fmt_name(format),
            path_str(&path)?,
            &size,
        ])
        .await?;
        Ok(())
    }

    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError> {
        let src = self.volume_source(src_pool, src_id, format, tag).await?;
        let dst = self.file_path(dst_pool, dst_id, dst_format)?;
        if self.exists(dst_pool, &dst).await? {
            return Err(DriverError::VolumeExists(dst_id));
        }
        let from = convert_from(&src, force_share, Some(dst_format))?;
        let mut before = vec!["convert", "-q"];
        before.extend(from.iter().map(String::as_str));
        self.write_new(dst_pool, dst_id, &dst, &before, &[]).await
    }

    /// 볼륨 파일. 스냅샷은 qcow2 안의 내부 스냅샷으로 읽는다. 크기는 지금 볼륨의 가상 크기다
    /// (`qemu-img info`는 내부 스냅샷의 크기를 알려 주지 않고, `convert -l`도 지금 크기만큼 읽는다).
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let path = match tag {
            Some(_) => self.snapshot_path(pool, id, format).await?,
            None => self.existing_path(pool, id, format).await?,
        };
        let size_bytes = self.volume_info(pool, id, format).await?.virtual_bytes;
        Ok(VolumeSource {
            path,
            format,
            snapshot: tag.map(str::to_string),
            size_bytes,
            rbd: None,
        })
    }

    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    /// `dir` 풀만 쓰는 테스트의 마운트 루트. 쓰이지 않는다.
    const MNT: &str = "/nonexistent/opl-pools";

    fn pool(path: &str) -> StoragePool {
        Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir { path: path.into() },
                nodes: vec![],
                shared: false,
            },
        )
    }

    fn dir_storage(qemu_img: impl Into<PathBuf>) -> DirStorage {
        DirStorage::new(qemu_img, MNT)
    }

    fn volume_path(pool: &StoragePool, id: Uuid, format: ImageFormat) -> PathBuf {
        DirStorage::volume_path(pool, Path::new(MNT), id, format)
    }

    #[test]
    fn volume_path_uses_id_and_format() {
        let id = Uuid::nil();
        assert_eq!(
            volume_path(&pool("/pools/local"), id, ImageFormat::Qcow2),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.qcow2")
        );
        assert_eq!(
            volume_path(&pool("/pools/local"), id, ImageFormat::Raw),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.raw")
        );
    }

    #[test]
    fn image_opts_escapes_commas() {
        assert_eq!(
            image_opts(Path::new("/pools/a,b/v.qcow2"), ImageFormat::Qcow2).unwrap(),
            "driver=qcow2,file.filename=/pools/a,,b/v.qcow2"
        );
    }

    #[tokio::test]
    async fn tool_failure_carries_stderr() {
        let storage = dir_storage("/bin/sh");
        // `sh create ...`는 파일이 없어 실패하고 stderr를 남긴다.
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let err = storage
            .create_volume(&p, Uuid::nil(), 1 << 20, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        match err {
            DriverError::ToolFailed { tool, stderr } => {
                assert_eq!(tool, "qemu-img");
                assert!(!stderr.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn nfs_pool() -> StoragePool {
        Resource::new(
            "nas",
            StoragePoolSpec {
                backend: PoolBackend::Nfs {
                    server: "nas".into(),
                    export: "/srv/vm".into(),
                    options: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    #[test]
    fn pool_root_depends_on_backend() {
        let mnt = Path::new("/var/lib/opl/pools");
        assert_eq!(
            pool_root(&pool("/pools/local"), mnt),
            PathBuf::from("/pools/local")
        );
        let nfs = nfs_pool();
        assert_eq!(pool_root(&nfs, mnt), mnt.join(nfs.id.to_string()));
        assert_eq!(
            DirStorage::volume_path(&nfs, mnt, Uuid::nil(), ImageFormat::Qcow2),
            mnt.join(nfs.id.to_string())
                .join("00000000-0000-0000-0000-000000000000.qcow2")
        );
    }

    /// 파일 풀의 디스크는 볼륨 파일이다. 활성화할 것이 없다.
    #[tokio::test]
    async fn file_pool_disks_are_volume_files() {
        let mnt = tempfile::tempdir().unwrap();
        let p = pool("/pools/local");
        let id = Uuid::nil();
        assert_eq!(
            disk_location(&p, mnt.path(), id, ImageFormat::Raw).unwrap(),
            DiskLocation::Path(
                PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.raw"),
                DiskKind::File
            )
        );
        // 없는 볼륨이어도 dir 저장소는 아무것도 하지 않는다.
        let storage = dir_storage("/nonexistent/qemu-img");
        storage
            .activate_volume(&p, id, ImageFormat::Qcow2)
            .await
            .unwrap();
        // 마운트되지 않은 NFS 풀의 디스크는 쓸 수 없다(일시 오류).
        let nfs = nfs_pool();
        match disk_location(&nfs, mnt.path(), id, ImageFormat::Qcow2).unwrap_err() {
            DriverError::PoolNotMounted(path) => {
                assert_eq!(path, mnt.path().join(nfs.id.to_string()))
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn lvm_pool() -> StoragePool {
        Resource::new(
            "vg",
            StoragePoolSpec {
                backend: PoolBackend::LvmThin {
                    vg: "vg0".into(),
                    thinpool: "tp".into(),
                },
                nodes: vec![],
                shared: false,
            },
        )
    }

    /// 파일 저장소는 LVM 풀을 다루지 않는다(`/dev/{vg}`에 파일을 만들지 않는다).
    #[tokio::test]
    async fn file_storage_refuses_lvm_thin_pools() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p = lvm_pool();
        assert_eq!(pool_root(&p, mnt.path()), PathBuf::from("/dev/vg0"));
        let storage = DirStorage::new(fake_tool(tools.path(), "exit 1"), mnt.path());
        let refused = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("no volume files"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        };
        refused(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        refused(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        refused(storage.delete_volume(&p, id, f).await.unwrap_err());
        refused(storage.volume_info(&p, id, f).await.unwrap_err());
        refused(storage.capacity(&p).await.unwrap_err());
    }

    /// 파일 저장소는 ZFS 풀도 다루지 않는다(`/dev/zvol/{dataset}`에 파일을 만들지 않는다).
    #[tokio::test]
    async fn zfs_pools_are_not_file_pools() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p: StoragePool = Resource::new(
            "z",
            StoragePoolSpec {
                backend: PoolBackend::Zfs {
                    dataset: "tank/opl".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        assert_eq!(
            pool_root(&p, mnt.path()),
            PathBuf::from("/dev/zvol/tank/opl")
        );
        let storage = DirStorage::new(fake_tool(tools.path(), "exit 1"), mnt.path());
        let refused = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("no volume files"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        };
        refused(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        refused(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        refused(storage.capacity(&p).await.unwrap_err());
    }

    /// 파일 저장소는 RBD 풀도 다루지 않는다.
    #[tokio::test]
    async fn rbd_pools_are_not_file_pools() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p: StoragePool = Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: "rbd".into(),
                    namespace: None,
                    user: None,
                    conf: None,
                },
                nodes: vec![],
                shared: true,
            },
        );
        assert_eq!(pool_root(&p, mnt.path()), PathBuf::new());
        let storage = DirStorage::new(fake_tool(tools.path(), "exit 1"), mnt.path());
        let refused = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => assert_eq!(msg, "rbd pools hold no volume files"),
            other => panic!("unexpected error: {other:?}"),
        };
        refused(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        refused(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        refused(storage.capacity(&p).await.unwrap_err());
    }

    /// 파일 저장소는 공유 LVM 풀을 다루지 않는다. 풀 루트는 VG의 장치 디렉터리다.
    #[tokio::test]
    async fn lvm_shared_pools_are_not_file_pools() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p = lvm_shared_pool();
        assert_eq!(pool_root(&p, mnt.path()), PathBuf::from("/dev/san"));
        let storage = DirStorage::new(fake_tool(tools.path(), "exit 1"), mnt.path());
        let refused = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "lvm-shared pools hold no volume files")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        refused(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        refused(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        refused(storage.capacity(&p).await.unwrap_err());
    }

    fn lvm_shared_pool() -> StoragePool {
        Resource::new(
            "s",
            StoragePoolSpec {
                backend: PoolBackend::LvmShared {
                    vg: "san".into(),
                    iscsi: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    /// 공유 LVM 풀의 디스크는 LVM-thin과 같은 LV 장치다. 이 노드에서 켜지 않았으면 장치 노드가 없다.
    #[test]
    fn lvm_shared_disks_are_lv_devices() {
        let dev = tempfile::tempdir().unwrap();
        let mnt = Path::new(MNT);
        let (p, id) = (lvm_shared_pool(), Uuid::nil());
        let path = dev
            .path()
            .join("san/opl-00000000-0000-0000-0000-000000000000");
        match disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap_err() {
            DriverError::VolumeNotActive(missing) => assert_eq!(missing, path),
            other => panic!("unexpected error: {other:?}"),
        }
        std::fs::create_dir(dev.path().join("san")).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap(),
            DiskLocation::Path(path, DiskKind::Block)
        );
        match disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Qcow2).unwrap_err() {
            DriverError::InvalidSpec(msg) => assert_eq!(msg, "lvm-shared pools hold raw volumes"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// RBD 풀의 디스크는 경로가 아니라 RBD 이미지 `opl-{id}`다. 사용자가 없으면 `admin`이다.
    #[test]
    fn rbd_disks_are_rbd_images() {
        let mnt = Path::new(MNT);
        let rbd =
            |namespace: Option<&str>, user: Option<&str>, conf: Option<&str>| -> StoragePool {
                Resource::new(
                    "r",
                    StoragePoolSpec {
                        backend: PoolBackend::Rbd {
                            pool: "rbd".into(),
                            namespace: namespace.map(Into::into),
                            user: user.map(Into::into),
                            conf: conf.map(Into::into),
                        },
                        nodes: vec![],
                        shared: true,
                    },
                )
            };
        let id = Uuid::nil();
        let image = "opl-00000000-0000-0000-0000-000000000000".to_string();
        assert_eq!(
            disk_location(&rbd(None, None, None), mnt, id, ImageFormat::Raw).unwrap(),
            DiskLocation::Rbd(RbdTarget {
                pool: "rbd".into(),
                namespace: None,
                image: image.clone(),
                user: "admin".into(),
                conf: None,
            })
        );
        let p = rbd(Some("opl"), Some("opl"), Some("/etc/ceph/opl.conf"));
        assert_eq!(
            disk_location(&p, mnt, id, ImageFormat::Raw).unwrap(),
            DiskLocation::Rbd(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image,
                user: "opl".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            })
        );
        // RBD 풀은 raw 볼륨만 담는다.
        match disk_location(&p, mnt, id, ImageFormat::Qcow2).unwrap_err() {
            DriverError::InvalidSpec(msg) => assert_eq!(msg, "rbd pools hold raw volumes"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// ZFS 풀의 디스크는 zvol 장치다. 장치 노드가 없으면(아직 생기지 않음) 일시 오류다.
    #[test]
    fn zfs_disks_are_zvol_devices() {
        let dev = tempfile::tempdir().unwrap();
        let mnt = Path::new(MNT);
        let p: StoragePool = Resource::new(
            "z",
            StoragePoolSpec {
                backend: PoolBackend::Zfs {
                    dataset: "tank/opl".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        let id = Uuid::nil();
        let path = dev
            .path()
            .join("zvol/tank/opl/opl-00000000-0000-0000-0000-000000000000");
        match disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap_err() {
            DriverError::VolumeNotActive(missing) => assert_eq!(missing, path),
            other => panic!("unexpected error: {other:?}"),
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap(),
            DiskLocation::Path(path, DiskKind::Block)
        );
        // ZFS 풀은 raw 볼륨만 담는다.
        let err = disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Qcow2).unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        // 실제 장치 디렉터리
        let id = Uuid::now_v7();
        match disk_location(&p, mnt, id, ImageFormat::Raw).unwrap_err() {
            DriverError::VolumeNotActive(missing) => {
                assert_eq!(
                    missing,
                    PathBuf::from(format!("/dev/zvol/tank/opl/opl-{id}"))
                )
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// LVM 풀의 디스크는 LV 장치다. 장치 노드가 없으면 비활성(일시 오류)이다.
    #[test]
    fn lvm_thin_disks_are_lv_devices() {
        let dev = tempfile::tempdir().unwrap();
        let mnt = Path::new(MNT);
        let p = lvm_pool();
        let id = Uuid::nil();
        let path = dev
            .path()
            .join("vg0/opl-00000000-0000-0000-0000-000000000000");
        match disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap_err() {
            DriverError::VolumeNotActive(missing) => assert_eq!(missing, path),
            other => panic!("unexpected error: {other:?}"),
        }
        std::fs::create_dir(dev.path().join("vg0")).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Raw).unwrap(),
            DiskLocation::Path(path, DiskKind::Block)
        );
        // LVM 풀은 raw 볼륨만 담는다.
        let err = disk_location_in(dev.path(), &p, mnt, id, ImageFormat::Qcow2).unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        // 실제 장치 디렉터리
        let id = Uuid::now_v7();
        match disk_location(&p, mnt, id, ImageFormat::Raw).unwrap_err() {
            DriverError::VolumeNotActive(missing) => {
                assert_eq!(missing, PathBuf::from(format!("/dev/vg0/opl-{id}")))
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unmounted_nfs_pool_is_refused_everywhere() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p = nfs_pool();
        // 마운트되지 않은 빈 마운트 지점(mountinfo에 없다). 여기에 볼륨을 만들면 로컬 디스크에 쌓인다.
        let root = mnt.path().join(p.id.to_string());
        std::fs::create_dir(&root).unwrap();
        let body = format!("{INFO_5}\necho new > \"$last\"");
        let storage = DirStorage::new(fake_tool(tools.path(), &body), mnt.path());
        let not_mounted = |e: DriverError| match e {
            DriverError::PoolNotMounted(path) => {
                assert_eq!(path, root);
                assert!(DriverError::PoolNotMounted(path).is_transient());
            }
            other => panic!("unexpected error: {other:?}"),
        };
        assert_eq!(
            check_pool_root(&pool(tmp_str(&mnt)), mnt.path()).unwrap(),
            mnt.path()
        );
        not_mounted(check_pool_root(&p, mnt.path()).unwrap_err());
        let (id, f) = (Uuid::nil(), ImageFormat::Qcow2);
        let src = tools.path().join("src.qcow2");
        not_mounted(storage.create_volume(&p, id, 1 << 20, f).await.unwrap_err());
        not_mounted(
            storage
                .import_volume(&p, id, &src, SourceFormat::Qcow2, f)
                .await
                .unwrap_err(),
        );
        not_mounted(storage.delete_volume(&p, id, f).await.unwrap_err());
        not_mounted(storage.volume_info(&p, id, f).await.unwrap_err());
        not_mounted(storage.capacity(&p).await.unwrap_err());
        not_mounted(storage.snapshot_create(&p, id, f, "t").await.unwrap_err());
        not_mounted(storage.snapshot_revert(&p, id, f, "t").await.unwrap_err());
        not_mounted(storage.snapshot_delete(&p, id, f, "t").await.unwrap_err());
        not_mounted(storage.snapshot_tags(&p, id, f).await.unwrap_err());
        not_mounted(storage.resize_volume(&p, id, f, 2 << 20).await.unwrap_err());
        // 복사는 원본이 있는 로컬 풀에서 마운트되지 않은 풀로 가도 거절된다.
        let local_dir = tempfile::tempdir().unwrap();
        let local = pool(tmp_str(&local_dir));
        std::fs::write(volume_path(&local, id, f), b"x").unwrap();
        not_mounted(
            storage
                .copy_volume(&local, id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        not_mounted(
            storage
                .copy_volume(&p, id, f, None, false, &local, id, f)
                .await
                .unwrap_err(),
        );
        // 마운트 지점이 아예 없어도 같다.
        std::fs::remove_dir(&root).unwrap();
        not_mounted(storage.capacity(&p).await.unwrap_err());
        assert_eq!(pool_files(mnt.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn stuck_pool_calls_time_out() {
        let root = Path::new("/pools/nas");
        let stuck = || std::thread::sleep(Duration::from_millis(300));
        match on_pool(Duration::from_millis(20), root, stuck).await {
            Err(DriverError::PoolNotResponding(path)) => {
                assert_eq!(path, root);
                assert!(DriverError::PoolNotResponding(path).is_transient());
            }
            other => panic!("unexpected result: {other:?}"),
        }
        assert_eq!(on_pool(FS_TIMEOUT, root, || 7).await.unwrap(), 7);
    }

    fn tmp_str(dir: &tempfile::TempDir) -> &str {
        dir.path().to_str().unwrap()
    }

    /// 실행 파일 `path`(가짜 도구 스크립트)를 `text`로 만든다. 이 프로세스가 쓰려고 연 파일은 그동안
    /// 다른 테스트가 띄우는 자식 프로세스가 물려받을 수 있고, 그러면 스크립트 실행이 ETXTBSY("Text
    /// file busy")로 실패한다. 그래서 파일은 자식 셸이 열고 쓰고 닫는다.
    pub(super) fn write_script(path: &Path, text: &str) {
        use std::io::Write;
        let mut sh = std::process::Command::new("/bin/sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        assert!(sh.wait().unwrap().success(), "cannot write {path:?}");
    }

    /// 실행 파일 셸 스크립트를 만든다. `"$last"`에 마지막 인자가 들어 있다.
    fn fake_tool(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("fake-qemu-img");
        write_script(&path, &format!("#!/bin/sh\nfor last; do :; done\n{body}\n"));
        path
    }

    /// [`fake_tool`]의 본문 앞에 두면 `info`에 가상 크기 5를 답한다(복사 원본의 크기).
    const INFO_5: &str =
        "if [ \"$1\" = info ]; then echo '{\"virtual-size\": 5, \"actual-size\": 0}'; exit 0; fi";

    fn pool_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn existing_volume_is_never_overwritten() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        let fin = volume_path(&p, id, ImageFormat::Raw);
        std::fs::write(&fin, b"precious").unwrap();
        let storage = dir_storage(fake_tool(tools.path(), "echo new > \"$last\""));

        let err = storage
            .create_volume(&p, id, 1 << 20, ImageFormat::Raw)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&fin).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn volume_created_concurrently_wins_over_ours() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        let fin = volume_path(&p, id, ImageFormat::Qcow2);
        // 도구가 도는 사이에 다른 누군가가 같은 볼륨을 만든다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            &format!("echo other > {fin:?}\necho ours > \"$last\""),
        ));
        let src = tools.path().join("src.vmdk");
        std::fs::write(&src, b"x").unwrap();

        let err = storage
            .import_volume(&p, id, &src, SourceFormat::Vmdk, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&fin).unwrap(), b"other\n");
        assert_eq!(pool_files(tmp.path()), vec![format!("{id}.qcow2")]);
    }

    #[tokio::test]
    async fn temp_file_of_an_earlier_attempt_is_not_reused() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        // opld가 죽은 뒤에도 고아 qemu-img가 이전 시도의 임시 파일을 계속 쓰고 있을 수 있다.
        let orphan = tmp.path().join(format!(".{id}.raw.part"));
        std::fs::write(&orphan, b"orphan").unwrap();
        // create는 크기가 마지막 인자이므로 .part로 끝나는 출력 인자를 찾는다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            "for a; do case \"$a\" in *.part) out=\"$a\";; esac; done\necho ours > \"$out\"",
        ));

        let path = storage
            .create_volume(&p, id, 1 << 20, ImageFormat::Raw)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"ours\n");
        assert_eq!(std::fs::read(&orphan).unwrap(), b"orphan");
        assert_eq!(pool_files(tmp.path()).len(), 2);
    }

    #[tokio::test]
    async fn failed_import_leaves_no_files() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        // 출력 파일을 반쯤 쓰고 실패한다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho broken >&2\nexit 1",
        ));
        let src = tools.path().join("src.vmdk");
        std::fs::write(&src, b"x").unwrap();

        let err = storage
            .import_volume(
                &p,
                Uuid::now_v7(),
                &src,
                SourceFormat::Vmdk,
                ImageFormat::Qcow2,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert!(pool_files(tmp.path()).is_empty());

        let storage = dir_storage("/usr/bin/false");
        storage
            .import_volume(
                &p,
                Uuid::now_v7(),
                &src,
                SourceFormat::Vmdk,
                ImageFormat::Qcow2,
            )
            .await
            .unwrap_err();
        assert!(pool_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn relative_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        // qemu-img는 상대 경로를 `json:{...}`, `nbd:` 같은 프로토콜로 해석할 수 있다.
        let storage = dir_storage("/usr/bin/true");
        let rel = pool("pools/local");
        let id = Uuid::nil();
        let raw = ImageFormat::Raw;
        assert!(matches!(
            storage.create_volume(&rel, id, 1 << 20, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage
                .import_volume(&rel, id, Path::new("/a.vmdk"), SourceFormat::Vmdk, raw)
                .await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.delete_volume(&rel, id, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.volume_info(&rel, id, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.capacity(&rel).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.resize_volume(&rel, id, raw, 2 << 20).await,
            Err(DriverError::InvalidSpec(_))
        ));
        let qcow2 = ImageFormat::Qcow2;
        assert!(matches!(
            storage.snapshot_create(&rel, id, qcow2, "t").await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.snapshot_tags(&rel, id, qcow2).await,
            Err(DriverError::InvalidSpec(_))
        ));

        let abs = pool(tmp.path().to_str().unwrap());
        assert!(matches!(
            storage
                .import_volume(&abs, id, Path::new("json:{}"), SourceFormat::Vmdk, raw)
                .await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(pool_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn import_source_header_is_rejected_before_running_tool() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = dir_storage("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str, mut bytes: Vec<u8>| {
            bytes.resize(bytes.len().max(80), 0);
            let path = tmp.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let qcow2 = |version: u32, backing: u64, incompatible: u64| {
            let mut h = vec![0u8; 80];
            h[..4].copy_from_slice(b"QFI\xfb");
            h[4..8].copy_from_slice(&version.to_be_bytes());
            h[8..16].copy_from_slice(&backing.to_be_bytes());
            h[72..80].copy_from_slice(&incompatible.to_be_bytes());
            h
        };

        let desc = file(
            "desc.vmdk",
            b"# Disk DescriptorFile\nversion=1\nRW 2048 FLAT \"/dev/zero\" 0\n".to_vec(),
        );
        let backing = file("backing.qcow2", qcow2(3, 512, 0));
        let external = file("external.qcow2", qcow2(3, 0, 4));
        for (path, format, reason) in [
            (desc, SourceFormat::Vmdk, "descriptor"),
            (backing, SourceFormat::Qcow2, "backing file"),
            (external, SourceFormat::Qcow2, "external data file"),
        ] {
            let err = storage
                .check_import_source(&path, format)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DriverError::InvalidSpec(ref m) if m.contains(reason)),
                "{reason}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn volume_info_without_sizes_is_tool_failure() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(volume_path(&p, id, ImageFormat::Raw), b"").unwrap();
        let storage = dir_storage(fake_tool(tools.path(), "echo '{\"virtual-size\": 5}'"));

        let err = storage
            .volume_info(&p, id, ImageFormat::Raw)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn snapshots_need_qcow2_and_existing_volume() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = dir_storage("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);
        assert!(storage.supports_snapshots(&p, qcow2));
        assert!(!storage.supports_snapshots(&p, raw));

        std::fs::write(volume_path(&p, id, raw), b"").unwrap();
        for res in [
            storage.snapshot_create(&p, id, raw, "t").await,
            storage.snapshot_revert(&p, id, raw, "t").await,
            storage.snapshot_delete(&p, id, raw, "t").await,
            storage.snapshot_tags(&p, id, raw).await.map(|_| ()),
        ] {
            assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
        }

        for res in [
            storage.snapshot_create(&p, id, qcow2, "t").await,
            storage.snapshot_revert(&p, id, qcow2, "t").await,
            storage.snapshot_delete(&p, id, qcow2, "t").await,
            storage.snapshot_tags(&p, id, qcow2).await.map(|_| ()),
            storage.resize_volume(&p, id, qcow2, 1 << 20).await,
        ] {
            assert!(
                matches!(res, Err(DriverError::VolumeNotFound(_))),
                "{res:?}"
            );
        }
    }

    /// 파일 볼륨의 복사 원본은 볼륨 파일이고, 스냅샷은 qcow2 안의 내부 스냅샷이다.
    #[tokio::test]
    async fn file_volume_source_is_the_volume_file() {
        let tools = tempfile::tempdir().unwrap();
        let storage = dir_storage(fake_tool(tools.path(), &format!("{INFO_5}\nexit 1")));
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp_str(&tmp));
        let id = Uuid::now_v7();
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);

        let res = storage.volume_source(&p, id, qcow2, None).await;
        assert!(
            matches!(res, Err(DriverError::VolumeNotFound(v)) if v == id),
            "{res:?}"
        );
        for format in [raw, qcow2] {
            std::fs::write(volume_path(&p, id, format), b"").unwrap();
            assert_eq!(
                storage.volume_source(&p, id, format, None).await.unwrap(),
                VolumeSource {
                    path: volume_path(&p, id, format),
                    format,
                    snapshot: None,
                    size_bytes: 5,
                    rbd: None,
                }
            );
        }
        assert_eq!(
            storage
                .volume_source(&p, id, qcow2, Some("t1"))
                .await
                .unwrap(),
            VolumeSource {
                path: volume_path(&p, id, qcow2),
                format: qcow2,
                snapshot: Some("t1".into()),
                size_bytes: 5,
                rbd: None,
            }
        );
        // raw 볼륨에는 스냅샷이 없다.
        let res = storage.volume_source(&p, id, raw, Some("t1")).await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
        // 내놓을 것이 없다.
        storage
            .release_source(&p, id, qcow2, Some("t1"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn snapshot_delete_skips_missing_tag() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // info만 답하고 다른 명령(snapshot -d)은 실패한다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            "case \"$1\" in info) echo '{\"snapshots\": [{\"name\": \"other\"}]}';; \
             *) echo unexpected >&2; exit 1;; esac",
        ));

        assert_eq!(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap(),
            vec!["other".to_string()]
        );
        storage
            .snapshot_delete(&p, id, ImageFormat::Qcow2, "gone")
            .await
            .unwrap();
        let err = storage
            .snapshot_delete(&p, id, ImageFormat::Qcow2, "other")
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn volume_without_snapshots_has_no_tags() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // 스냅샷이 없으면 qemu-img info는 snapshots 항목을 내지 않는다.
        let storage = dir_storage(fake_tool(tools.path(), "echo '{\"virtual-size\": 5}'"));
        assert!(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn copy_volume_passes_snapshot_and_share_options() {
        let tools = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        // 경로와 태그의 쉼표는 옵션 문자열에서 `,,`로 쓴다.
        let dst_dir = tools.path().join("dst");
        std::fs::create_dir(&dst_dir).unwrap();
        let (src_pool, dst_pool) = (
            pool(src_dir.path().to_str().unwrap()),
            pool(dst_dir.to_str().unwrap()),
        );
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let qcow2 = ImageFormat::Qcow2;
        let src_path = volume_path(&src_pool, src, qcow2);
        std::fs::write(&src_path, b"").unwrap();
        // 인자를 한 줄에 하나씩 적고 출력 파일을 만든다.
        let args = tools.path().join("args");
        let storage = dir_storage(fake_tool(
            tools.path(),
            &format!("{INFO_5}\nprintf '%s\\n' \"$@\" > {args:?}\necho copy > \"$last\""),
        ));

        storage
            .copy_volume(
                &src_pool,
                src,
                qcow2,
                Some("a,b"),
                true,
                &dst_pool,
                dst,
                qcow2,
            )
            .await
            .unwrap();
        let dst_path = volume_path(&dst_pool, dst, qcow2);
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"copy\n");
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "qcow2",
                src_path.to_str().unwrap(),
            ]
        );
        assert!(got[got.len() - 1].ends_with(".part"), "{got:?}");

        // 태그 없이, 잠금을 지키며 지금 내용을 복사한다.
        let dst2 = Uuid::now_v7();
        storage
            .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, dst2, qcow2)
            .await
            .unwrap();
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-O",
                "qcow2",
                src_path.to_str().unwrap()
            ]
        );

        // 대상 형식이 다르면 그 형식으로 바꿔 쓴다.
        let dst3 = Uuid::now_v7();
        let raw = ImageFormat::Raw;
        storage
            .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, dst3, raw)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(volume_path(&dst_pool, dst3, raw)).unwrap(),
            b"copy\n"
        );
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-O",
                "raw",
                src_path.to_str().unwrap()
            ]
        );
    }

    #[tokio::test]
    async fn copy_volume_checks_source_and_never_overwrites() {
        // 원본 크기(`info`)만 답한다. 복사까지 가면 다른 오류(ToolFailed)가 난다.
        let tools = tempfile::tempdir().unwrap();
        let storage = dir_storage(fake_tool(tools.path(), &format!("{INFO_5}\nexit 1")));
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst, raw)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeNotFound(id)) if id == src),
            "{res:?}"
        );

        std::fs::write(volume_path(&p, src, raw), b"").unwrap();
        // raw 볼륨에는 스냅샷이 없다.
        let res = storage
            .copy_volume(&p, src, raw, Some("t"), false, &p, dst, raw)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");

        let dst_path = volume_path(&p, dst, raw);
        std::fs::write(&dst_path, b"precious").unwrap();
        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst, raw)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeExists(id)) if id == dst),
            "{res:?}"
        );
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"precious");

        let rel = pool("pools/local");
        let res = storage
            .copy_volume(&p, src, raw, None, false, &rel, dst, raw)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
    }

    #[tokio::test]
    async fn failed_copy_leaves_no_files() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let src = Uuid::now_v7();
        std::fs::write(volume_path(&p, src, ImageFormat::Raw), b"").unwrap();
        // 원본 크기를 읽은 뒤 출력 파일을 반쯤 쓰고 실패한다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            &format!("{INFO_5}\necho partial > \"$last\"\necho broken >&2\nexit 1"),
        ));

        let err = storage
            .copy_volume(
                &p,
                src,
                ImageFormat::Raw,
                None,
                false,
                &p,
                Uuid::now_v7(),
                ImageFormat::Raw,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(pool_files(tmp.path()), vec![format!("{src}.raw")]);
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = dir_storage("qemu-img");
        let err = storage
            .delete_volume(
                &pool(tmp.path().to_str().unwrap()),
                Uuid::nil(),
                ImageFormat::Raw,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)));
    }

    #[tokio::test]
    async fn capacity_reports_filesystem_space() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = dir_storage("qemu-img")
            .capacity(&pool(tmp.path().to_str().unwrap()))
            .await
            .unwrap();
        assert!(cap.total_bytes > 0);
        assert!(cap.available_bytes <= cap.total_bytes);
    }

    /// `dir`의 NFS 마운트가 있다고 적은 mountinfo
    fn mountinfo_with(dir: &Path) -> String {
        format!(
            "22 1 8:1 / / rw - ext4 /dev/sda1 rw\n40 22 0:45 / {} rw - nfs4 nas:/srv/vm rw\n",
            dir.display()
        )
    }

    #[test]
    fn nfs_pool_is_mounted_only_when_mountinfo_lists_it() {
        let mnt = tempfile::tempdir().unwrap();
        let p = nfs_pool();
        let root = mnt.path().join(p.id.to_string());
        // mountinfo는 실제 경로를 적는다(macOS의 임시 디렉터리는 심볼릭 링크 아래에 있다).
        let real = mnt.path().canonicalize().unwrap().join(p.id.to_string());
        let ok = |info: String| move || Ok::<_, std::io::Error>(info.clone());
        assert_eq!(
            check_pool_root_with(&p, mnt.path(), &ok(mountinfo_with(&real))).unwrap(),
            root
        );
        // 마운트 지점은 stat하지 않는다. 디렉터리가 없어도 mountinfo에 있으면 마운트된 것이다.
        assert!(!root.exists());
        let not_mounted = |res: Result<PathBuf, DriverError>| match res {
            Err(DriverError::PoolNotMounted(path)) => assert_eq!(path, root),
            other => panic!("unexpected result: {other:?}"),
        };
        // 다른 풀의 마운트, NFS가 아닌 마운트, 읽을 수 없는 mountinfo
        let other = mnt
            .path()
            .canonicalize()
            .unwrap()
            .join(Uuid::nil().to_string());
        not_mounted(check_pool_root_with(
            &p,
            mnt.path(),
            &ok(mountinfo_with(&other)),
        ));
        not_mounted(check_pool_root_with(
            &p,
            mnt.path(),
            &ok(format!(
                "43 22 0:48 / {} rw - tmpfs tmpfs rw\n",
                real.display()
            )),
        ));
        not_mounted(check_pool_root_with(&p, mnt.path(), &|| {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        }));
        // 마운트 루트가 없다.
        let missing = mnt.path().join("missing");
        let res = check_pool_root_with(&p, &missing, &ok(mountinfo_with(&real)));
        assert!(
            matches!(res, Err(DriverError::PoolNotMounted(_))),
            "{res:?}"
        );
        // 디렉터리 풀은 mountinfo를 보지 않는다.
        let local = pool(tmp_str(&mnt));
        assert_eq!(
            check_pool_root_with(&local, Path::new(MNT), &|| panic!("read mountinfo")).unwrap(),
            mnt.path()
        );
    }

    /// 파일이 없다는 결과는 풀이 마운트된 채일 때만 믿는다. 그사이 마운트가 풀렸으면 빈 마운트
    /// 지점을 본 것이다.
    #[tokio::test]
    async fn missing_volume_on_a_pool_unmounted_meanwhile_is_not_mounted() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let mnt = tempfile::tempdir().unwrap();
        let p = nfs_pool();
        let root = mnt.path().join(p.id.to_string());
        std::fs::create_dir(&root).unwrap();
        let info = mountinfo_with(&mnt.path().canonicalize().unwrap().join(p.id.to_string()));
        // `mounted_reads`번 읽을 때까지만 마운트돼 있다.
        let storage = |mounted_reads: usize| {
            let reads = AtomicUsize::new(0);
            let info = info.clone();
            let mut s = DirStorage::new("qemu-img", mnt.path());
            s.mountinfo = Arc::new(move || {
                let n = reads.fetch_add(1, Ordering::SeqCst);
                Ok(if n < mounted_reads {
                    info.clone()
                } else {
                    String::new()
                })
            });
            s
        };
        let (id, f) = (Uuid::nil(), ImageFormat::Qcow2);
        // 계속 마운트돼 있으면 정말 없는 볼륨이다.
        let mounted = storage(usize::MAX);
        let err = mounted.delete_volume(&p, id, f).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        let err = mounted.volume_info(&p, id, f).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        // 처음 확인 뒤에 마운트가 풀렸다.
        let err = storage(1).delete_volume(&p, id, f).await.unwrap_err();
        assert!(matches!(err, DriverError::PoolNotMounted(_)), "{err:?}");
        let err = storage(1).volume_info(&p, id, f).await.unwrap_err();
        assert!(matches!(err, DriverError::PoolNotMounted(_)), "{err:?}");
        let err = storage(1).snapshot_tags(&p, id, f).await.unwrap_err();
        assert!(matches!(err, DriverError::PoolNotMounted(_)), "{err:?}");
    }
}
```

`crates/opl-driver/src/storage/blockcmd.rs`에서

```rust
    failure: fn(String) -> DriverError,
    /// 바꾸는 명령이 돌고 있는 대상
```

을 다음으로 바꾼다:

```rust
    failure: fn(String) -> DriverError,
    /// 이 말이 든 stderr 줄은 첫 줄보다 먼저 오류에 담는다(앞에 경고 줄이 올 수 있는 오류).
    prefer: &'static [&'static str],
    /// 바꾸는 명령이 돌고 있는 대상
```

`crates/opl-driver/src/storage/blockcmd.rs`에서

```rust
            failure,
            changing: Mutex::new(BTreeSet::new()),
        }
    }
```

을 다음으로 바꾼다:

```rust
            failure,
            prefer: &[],
            changing: Mutex::new(BTreeSet::new()),
        }
    }

    /// 오류에 첫 줄보다 먼저 담을 stderr 줄의 말([`BlockTool::prefer`])
    pub(super) const fn preferring(mut self, prefer: &'static [&'static str]) -> Self {
        self.prefer = prefer;
        self
    }
```

`crates/opl-driver/src/storage/blockcmd.rs`에서

```rust

    /// 실패한 명령의 오류: stderr 첫 줄(없으면 종료 상태)
    fn failure(
```

을 다음으로 바꾼다:

```rust

    /// 실패한 명령의 오류: `prefer`의 말이 든 stderr 줄, 없으면 첫 줄(없으면 종료 상태)
    fn failure(
```

`crates/opl-driver/src/storage/blockcmd.rs`에서

```rust
        let stderr = String::from_utf8_lossy(stderr);
        let line = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(|| status.to_string(), str::to_string);
```

을 다음으로 바꾼다:

```rust
        let stderr = String::from_utf8_lossy(stderr);
        let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
        let line = lines
            .clone()
            .find(|l| self.prefer.iter().any(|p| l.contains(p)))
            .or_else(|| lines.next())
            .map_or_else(|| status.to_string(), str::to_string);
```

`crates/opl-driver/src/storage/blockcmd.rs`에서

```rust

    /// 바꾸는 명령의 출력은 파이프가 아니다(표준 출력은 버리고 stderr는 파일). 기다리는 쪽이 떠나
```

을 다음으로 바꾼다:

```rust

    /// `prefer`의 말이 든 stderr 줄은 앞에 다른 줄이 있어도 오류에 담는다.
    #[tokio::test]
    async fn preferred_stderr_lines_come_first() {
        static PREFERRING: BlockTool =
            BlockTool::new("test", "busy", &[], DriverError::Lvm).preferring(&["by other host"]);
        let long = Duration::from_secs(10);
        let fail = [
            "-c",
            "echo 'Retrying ex lock on LV' >&2; echo '  LV locked by other host' >&2; exit 5",
        ];
        for err in [
            PREFERRING
                .run_read("/bin/sh", &fail, long)
                .await
                .unwrap_err(),
            PREFERRING
                .run_change("/bin/sh", &fail, &[], long)
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: LV locked by other host"),
                "{err:?}"
            );
        }
        let err = TOOL.run_read("/bin/sh", &fail, long).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: Retrying ex lock on LV"),
            "{err:?}"
        );
    }

    /// 바꾸는 명령의 출력은 파이프가 아니다(표준 출력은 버리고 stderr는 파일). 기다리는 쪽이 떠나
```

`crates/opl-driver/src/storage/lvm.rs` 전체:

```rust
//! LVM-thin 풀: VG의 thin pool 안에 볼륨마다 thin LV `opl-{id}`(raw)를 두고, 스냅샷은 thin 스냅샷
//! LV `opl-{id}-s-{tag}`로 둔다. 공유 LVM 풀: 공유(lvmlockd) VG 안에 볼륨마다 두꺼운 LV `opl-{id}`
//! (raw)를 두고 쓰는 노드에서만 배타 활성화한다. LVM 명령은 [`LvmOps`] 뒤에 있다(테스트는 가짜).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// LVM 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const LVM_TIMEOUT: Duration = Duration::from_secs(60);

/// LV 장치 노드가 놓이는 디렉터리(`/dev/{vg}/{lv}`)
pub(super) const DEV: &str = "/dev";

/// `lvs`가 알려 준 LV 하나
#[derive(Debug, Clone, PartialEq)]
pub struct LvInfo {
    pub name: String,
    /// `lv_attr`(예: thin pool `twi-aotz--`, thin 볼륨 `Vwi-a-tz--`)
    pub attr: String,
    pub size_bytes: u64,
    /// thin pool과 thin 볼륨의 데이터 사용률(%). 없으면(비활성 등) `None`.
    pub data_percent: Option<f64>,
}

impl LvInfo {
    pub fn is_thin_pool(&self) -> bool {
        self.attr.starts_with('t')
    }

    /// 장치 노드가 있는지(`lv_attr`의 다섯째 글자 `a`)
    pub fn is_active(&self) -> bool {
        self.attr.chars().nth(4) == Some('a')
    }

    /// thin pool이 새로 할당한 블록을 쓰기 전에 0으로 채우는지(`lv_attr`의 여덟째 글자 `z`,
    /// `lvcreate -Zn`이면 아니다). 아니면 thin LV의 덜 쓴 블록에 옛 데이터가 보일 수 있다.
    pub fn zeroes_new_blocks(&self) -> bool {
        self.attr.chars().nth(7) == Some('z')
    }

    /// 데이터 사용률로 계산한 할당 바이트
    pub fn used_bytes(&self) -> u64 {
        let percent = self.data_percent.unwrap_or(0.0).clamp(0.0, 100.0);
        (self.size_bytes as f64 * percent / 100.0) as u64
    }
}

/// `lvs --reportformat json --units b --nosuffix -o lv_name,lv_attr,lv_size,data_percent`의 출력을
/// 읽는다. 값은 문자열로 오지만 숫자도 받는다.
pub fn parse_lvs(out: &[u8]) -> Result<Vec<LvInfo>, DriverError> {
    let bad = |what: String| DriverError::Lvm(format!("unparseable lvs output: {what}"));
    let json: Value = serde_json::from_slice(out).map_err(|e| bad(e.to_string()))?;
    let reports = json
        .get("report")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no report".into()))?;
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut lvs = Vec::new();
    for lv in reports
        .iter()
        .filter_map(|r| r.get("lv").and_then(Value::as_array))
        .flatten()
    {
        let field = |k: &str| lv.get(k).and_then(text);
        let (Some(name), Some(attr), Some(size)) =
            (field("lv_name"), field("lv_attr"), field("lv_size"))
        else {
            return Err(bad(format!("lv without name, attr or size: {lv}")));
        };
        let size_bytes = size.parse().map_err(|_| bad(format!("lv_size {size:?}")))?;
        let data_percent = match field("data_percent").as_deref() {
            None | Some("") => None,
            Some(p) => Some(p.parse().map_err(|_| bad(format!("data_percent {p:?}")))?),
        };
        lvs.push(LvInfo {
            name,
            attr,
            size_bytes,
            data_percent,
        });
    }
    Ok(lvs)
}

/// `vgs`가 알려 준 VG 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgInfo {
    pub name: String,
    /// 잠금 종류(`vg_lock_type`). 공유 VG면 `sanlock`·`dlm`, 아니면 빈 문자열이다.
    pub lock_type: String,
    pub size_bytes: u64,
    pub free_bytes: u64,
}

/// `vgs --reportformat json --units b --nosuffix -o vg_name,vg_lock_type,vg_size,vg_free`의 출력을
/// 읽는다. 값은 문자열로 오지만 숫자도 받는다.
pub fn parse_vgs(out: &[u8]) -> Result<Vec<VgInfo>, DriverError> {
    let bad = |what: String| DriverError::Lvm(format!("unparseable vgs output: {what}"));
    let json: Value = serde_json::from_slice(out).map_err(|e| bad(e.to_string()))?;
    let reports = json
        .get("report")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no report".into()))?;
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut vgs = Vec::new();
    for vg in reports
        .iter()
        .filter_map(|r| r.get("vg").and_then(Value::as_array))
        .flatten()
    {
        let field = |k: &str| vg.get(k).and_then(text);
        let (Some(name), Some(size), Some(free)) =
            (field("vg_name"), field("vg_size"), field("vg_free"))
        else {
            return Err(bad(format!("vg without name, size or free: {vg}")));
        };
        let number = |k: &str, v: &str| v.parse().map_err(|_| bad(format!("{k} {v:?}")));
        vgs.push(VgInfo {
            name,
            lock_type: field("vg_lock_type").unwrap_or_default(),
            size_bytes: number("vg_size", &size)?,
            free_bytes: number("vg_free", &free)?,
        });
    }
    Ok(vgs)
}

/// `lvs --noheadings -o lv_tags`의 출력(쉼표로 나눈 태그)을 읽는다.
fn parse_lv_tags(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .trim()
        .split(',')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// LVM 명령. 이름은 VG 안의 LV 이름이다. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait LvmOps: Send + Sync {
    /// VG의 LV들(`lvs`)
    async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError>;
    /// thin pool에 `size_bytes` 크기의 thin 볼륨을 만든다(`lvcreate -T`).
    async fn create_thin(
        &self,
        vg: &str,
        thinpool: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError>;
    /// `origin`의 thin 스냅샷을 만든다(`lvcreate -s`). 활성화 건너뛰기 표시가 붙는다.
    async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError>;
    async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 크기를 `size_bytes`로 늘린다(`lvextend`).
    async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError>;
    async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError>;
    /// 활성화(건너뛰기 표시가 있어도) 또는 비활성화(`lvchange -ay -K` / `-an`)
    async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError>;
    /// 활성화 건너뛰기 표시를 지운다(`lvchange -kn`).
    async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// LV `name`을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, vg: &str, name: &str) -> bool;
    /// `size_bytes` 크기의 두꺼운(linear) LV를 켜지 않고 만든다(`lvcreate -an`).
    async fn create_linear(&self, vg: &str, name: &str, size_bytes: u64)
    -> Result<(), DriverError>;
    /// 이 노드에서만 켠다(`lvchange -aey`). 공유 VG에서 다른 노드가 켜 두었으면 lvmlockd가 거절한다.
    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 켜 둔 LV의 내용을 `offset` 바이트부터 끝까지 0으로 채운다(`blkdiscard -z`). 크기에 비례해
    /// 걸리므로 시간 제한이 없다.
    async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError>;
    /// LV의 태그(`lvs -o lv_tags`)
    async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError>;
    /// 태그를 붙이거나 뗀다(`lvchange --addtag` / `--deltag`).
    async fn set_tag(&self, vg: &str, name: &str, tag: &str, on: bool) -> Result<(), DriverError>;
    /// VG의 잠금 종류와 크기·남은 공간(`vgs`)
    async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError>;
}

/// 실제 LVM 명령. root여야 한다.
pub struct SystemLvm;

/// LVM 명령. 숫자를 `.`로 쓰고, 물려받은 파일 기술자 경고를 stderr에 섞지 않는다. 바꾸는 명령의
/// 대상은 `{vg}/{lv}`다.
static LVM: BlockTool = BlockTool::new(
    "lvm command",
    "an earlier lvm command still running on the same lv",
    &[("LC_ALL", "C"), ("LVM_SUPPRESS_FD_WARNINGS", "1")],
    DriverError::Lvm,
)
.preferring(&[LV_HELD_BY, VG_HELD_BY]);

impl SystemLvm {
    /// VG `vg`의 LV `lvs`를 바꾸는 명령([`BlockTool::run_change`])
    async fn change(
        program: &str,
        args: &[&str],
        vg: &str,
        lvs: &[&str],
    ) -> Result<(), DriverError> {
        let lvs: Vec<String> = lvs.iter().map(|lv| format!("{vg}/{lv}")).collect();
        let lvs: Vec<&str> = lvs.iter().map(String::as_str).collect();
        LVM.run_change(program, args, &lvs, LVM_TIMEOUT).await
    }
}

#[async_trait]
impl LvmOps for SystemLvm {
    async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError> {
        let out = LVM
            .run_read(
                "lvs",
                &[
                    "--reportformat",
                    "json",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "lv_name,lv_attr,lv_size,data_percent",
                    vg,
                ],
                LVM_TIMEOUT,
            )
            .await?;
        parse_lvs(&out)
    }

    async fn create_thin(
        &self,
        vg: &str,
        thinpool: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let (pool, size) = (format!("{vg}/{thinpool}"), format!("{size_bytes}b"));
        let args = ["-q", "-T", &pool, "-V", &size, "-n", name, "-y"];
        Self::change("lvcreate", &args, vg, &[name]).await
    }

    async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError> {
        let origin = format!("{vg}/{origin}");
        Self::change("lvcreate", &["-q", "-s", "-n", name, &origin], vg, &[name]).await
    }

    async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvremove", &["-q", "-y", &lv], vg, &[name]).await
    }

    async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError> {
        let (lv, size) = (format!("{vg}/{name}"), format!("{size_bytes}b"));
        Self::change("lvextend", &["-q", "-L", &size, &lv], vg, &[name]).await
    }

    async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError> {
        Self::change("lvrename", &["-q", vg, from, to], vg, &[from, to]).await
    }

    async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        let args: &[&str] = if active {
            &["-q", "-ay", "-K", &lv]
        } else {
            &["-q", "-an", &lv]
        };
        Self::change("lvchange", args, vg, &[name]).await
    }

    async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvchange", &["-q", "-kn", &lv], vg, &[name]).await
    }

    fn changing(&self, vg: &str, name: &str) -> bool {
        LVM.changing(&format!("{vg}/{name}"))
    }

    /// 켜지 않으므로 처음 부분을 0으로 채우거나 서명을 지우지 않는다(`-Zn -Wn`). 내용은 뒤에
    /// [`zero`](LvmOps::zero)가 모두 0으로 채운다.
    async fn create_linear(
        &self,
        vg: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let size = format!("{size_bytes}b");
        let args = ["-q", "-an", "-Zn", "-Wn", "-L", &size, "-n", name, vg];
        Self::change("lvcreate", &args, vg, &[name]).await
    }

    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvchange", &["-q", "-aey", &lv], vg, &[name]).await
    }

    /// 새 LV에 옛 데이터의 서명이 남아 있어도 묻지 않는다(`-f`).
    async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError> {
        let device = device_node(Path::new(DEV), vg, name)?;
        let (lv, offset) = (format!("{vg}/{name}"), offset.to_string());
        let args = ["-f", "-z", "-o", &offset, path_str(&device)?];
        LVM.run_change("blkdiscard", &args, &[&lv], Duration::MAX)
            .await
    }

    async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError> {
        let lv = format!("{vg}/{name}");
        let out = LVM
            .run_read("lvs", &["--noheadings", "-o", "lv_tags", &lv], LVM_TIMEOUT)
            .await?;
        Ok(parse_lv_tags(&out))
    }

    async fn set_tag(&self, vg: &str, name: &str, tag: &str, on: bool) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        let op = if on { "--addtag" } else { "--deltag" };
        Self::change("lvchange", &["-q", op, tag, &lv], vg, &[name]).await
    }

    async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError> {
        let out = LVM
            .run_read(
                "vgs",
                &[
                    "--reportformat",
                    "json",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "vg_name,vg_lock_type,vg_size,vg_free",
                    vg,
                ],
                LVM_TIMEOUT,
            )
            .await?;
        parse_vgs(&out)?
            .into_iter()
            .find(|v| v.name == vg)
            .ok_or_else(|| DriverError::Lvm(format!("volume group {vg} not found")))
    }
}

/// 볼륨의 LV 이름
fn volume_lv(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 스냅샷 LV 이름의 앞부분
fn snapshot_prefix(id: Uuid) -> String {
    format!("opl-{id}-s-")
}

/// 되돌리는 동안 옛 볼륨 LV를 옮겨 두는 이름의 앞부분. 되돌리기가 실패하면 남을 수 있다.
fn revert_prefix(id: Uuid) -> String {
    format!("opl-{id}-revert-")
}

/// LV 장치 노드 `{dev}/{vg}/{lv}`
pub(super) fn device_path(dev: &Path, vg: &str, lv: &str) -> PathBuf {
    dev.join(vg).join(lv)
}

/// 있는 LV 장치 노드. udev가 만드는 `{dev}/{vg}/{lv}`가 없으면 device-mapper 노드
/// `{dev}/mapper/{vg}-{lv}`(이름 안의 `-`는 `--`)를 쓴다. 둘 다 없으면 `VolumeNotActive`(비활성이거나
/// 노드가 아직 생기지 않았다).
pub(super) fn device_node(dev: &Path, vg: &str, lv: &str) -> Result<PathBuf, DriverError> {
    let path = device_path(dev, vg, lv);
    if path.exists() {
        return Ok(path);
    }
    let dm = format!("{}-{}", vg.replace('-', "--"), lv.replace('-', "--"));
    let mapper = dev.join("mapper").join(dm);
    if mapper.exists() {
        return Ok(mapper);
    }
    Err(DriverError::VolumeNotActive(path))
}

/// LVM-thin 볼륨의 장치 노드([`device_node`])
pub(super) fn volume_device(dev: &Path, vg: &str, id: Uuid) -> Result<PathBuf, DriverError> {
    device_node(dev, vg, &volume_lv(id))
}

/// LVM-thin 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "lvm-thin pools hold raw volumes".into(),
        )),
    }
}

/// 새로 만든 볼륨 LV
struct NewLv {
    /// 장치 노드
    path: PathBuf,
    /// 쓰기 전에 0으로 읽히는지(thin pool이 새 블록을 0으로 채운다)
    reads_zero: bool,
}

/// LVM-thin 풀(`lvm-thin`)의 볼륨. 볼륨은 thin LV이고 스냅샷은 thin 스냅샷 LV다.
pub struct LvmThinStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn LvmOps>,
    /// LV 장치 노드가 놓이는 디렉터리([`DEV`]). 테스트는 바꾼다.
    dev: PathBuf,
}

impl LvmThinStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn LvmOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(DEV),
        }
    }

    /// 풀의 VG와 thin pool. raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, format: ImageFormat) -> Result<(&str, &str), DriverError> {
        require_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. }
            | PoolBackend::LvmShared { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
            }
        }
    }

    /// 스냅샷 LV 이름. 태그는 LVM 이름에 쓸 수 있는 글자여야 한다.
    fn snapshot_lv(id: Uuid, tag: &str) -> Result<String, DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+_.-".contains(&c));
        if !ok {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid lvm name"
            )));
        }
        Ok(format!("{}{tag}", snapshot_prefix(id)))
    }

    /// VG의 LV 중 `name`
    async fn find(&self, vg: &str, name: &str) -> Result<Option<LvInfo>, DriverError> {
        Ok(self
            .ops
            .list(vg)
            .await?
            .into_iter()
            .find(|lv| lv.name == name))
    }

    /// VG의 LV들. 볼륨 LV가 없고 되돌리기가 옮겨 둔 `-revert-` LV가 하나만 있으면(시간을 넘긴
    /// 이름 바꾸기가 되돌리기가 실패한 뒤에 끝났다) 그 LV가 볼륨이므로 이름을 되돌린 뒤 다시 읽는다.
    /// 둘 중 하나라도 바꾸는 명령이 아직 돌고 있으면 일시 오류(`Timeout`)이고, `-revert-` LV가
    /// 여럿이면 어느 것인지 모르므로 `Lvm`이다(수동 복구).
    async fn volume_lvs(&self, vg: &str, id: Uuid) -> Result<Vec<LvInfo>, DriverError> {
        let lvs = self.ops.list(vg).await?;
        let volume = volume_lv(id);
        if lvs.iter().any(|lv| lv.name == volume) {
            return Ok(lvs);
        }
        let prefix = revert_prefix(id);
        let reverts: Vec<&str> = lvs
            .iter()
            .map(|lv| lv.name.as_str())
            .filter(|name| name.starts_with(&prefix))
            .collect();
        let old = match reverts.as_slice() {
            [] => return Ok(lvs),
            [old] => old.to_string(),
            _ => {
                return Err(DriverError::Lvm(format!(
                    "volume {id} is missing and several lvs may hold it ({}); rename the right \
                     one to {vg}/{volume}",
                    reverts.join(", ")
                )));
            }
        };
        if self.ops.changing(vg, &old) || self.ops.changing(vg, &volume) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        tracing::warn!(volume = %id, lv = %old, "restoring volume left by an unfinished revert");
        if let Err(e) = self.ops.rename(vg, &old, &volume).await
            && !self.done_after(&e, vg, &volume, false).await
        {
            return Err(e);
        }
        self.ops.list(vg).await
    }

    /// 있는 볼륨 LV([`volume_lvs`](Self::volume_lvs)). 없으면 `VolumeNotFound`.
    async fn existing(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        self.volume_lvs(vg, id)
            .await?
            .into_iter()
            .find(|lv| lv.name == volume_lv(id))
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨이 있고 스냅샷 LV `tag`도 있어야 한다. 스냅샷 LV를 돌려준다.
    async fn existing_snapshot(
        &self,
        vg: &str,
        id: Uuid,
        tag: &str,
    ) -> Result<LvInfo, DriverError> {
        let name = Self::snapshot_lv(id, tag)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        lvs.into_iter().find(|lv| lv.name == name).ok_or_else(|| {
            DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
        })
    }

    /// VG `vg`의 thin pool `thinpool`. 없거나 thin pool이 아니면 `Lvm`.
    async fn thin_pool(&self, vg: &str, thinpool: &str) -> Result<LvInfo, DriverError> {
        self.find(vg, thinpool)
            .await?
            .filter(LvInfo::is_thin_pool)
            .ok_or_else(|| DriverError::Lvm(format!("thin pool {vg}/{thinpool} not found")))
    }

    /// 새 볼륨 LV를 만든다. 크기는 512 배수로 올린다(LVM은 extent 단위로 다시 올린다). 장치 노드가
    /// 없으면 만든 LV를 지운다.
    async fn create_lv(
        &self,
        vg: &str,
        thinpool: &str,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<NewLv, DriverError> {
        let lvs = self.ops.list(vg).await?;
        if lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeExists(id));
        }
        let reads_zero = lvs
            .iter()
            .any(|lv| lv.name == thinpool && lv.is_thin_pool() && lv.zeroes_new_blocks());
        let size = size_bytes.div_ceil(512) * 512;
        self.ops
            .create_thin(vg, thinpool, &volume_lv(id), size)
            .await?;
        match volume_device(&self.dev, vg, id) {
            Ok(path) => Ok(NewLv { path, reads_zero }),
            Err(e) => {
                if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
                    tracing::warn!(volume = %id, error = %del, "cannot remove volume without device");
                }
                Err(e)
            }
        }
    }

    /// 만든 볼륨 LV에 `qemu-img convert -n`으로 써 넣는다. 실패하면 볼륨 LV를 지운다.
    /// 새 LV가 0으로 읽히면 0인 부분은 쓰지 않는다(`--target-is-zero`).
    async fn fill(
        &self,
        vg: &str,
        id: Uuid,
        dst: &NewLv,
        from: &[String],
    ) -> Result<(), DriverError> {
        let written = async {
            let mut args = vec!["convert", "-q", "-n"];
            if dst.reads_zero {
                args.push("--target-is-zero");
            }
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(&dst.path)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }

    /// 시간을 넘긴 명령(`e`가 `Timeout`) 뒤에 LV `name`이 있는지(`active`면 활성인지) 다시 본다.
    /// `lvs`는 VG 잠금을 기다리므로 그 명령이 끝난 뒤의 상태다. 다른 오류거나 볼 수 없으면 `false`.
    async fn done_after(&self, e: &DriverError, vg: &str, name: &str, active: bool) -> bool {
        e.may_be_in_progress()
            && matches!(self.find(vg, name).await, Ok(Some(lv)) if !active || lv.is_active())
    }

    /// `volume_source`로 읽게 한 복사 원본을 내놓을 가드([`release_source`](Storage::release_source)와
    /// 같다: 스냅샷이면 그 LV를 비활성화한다).
    fn source_guard(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<SourceGuard, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = tag.map(|tag| Self::snapshot_lv(id, tag)).transpose()?;
        Ok(SourceGuard {
            ops: self.ops.clone(),
            vg: vg.to_string(),
            lv,
        })
    }

    /// 되돌리기가 중간에 실패했을 때 옛 볼륨 LV `old`의 이름을 되돌린다. 되돌리지 못하면 남은 LV
    /// 이름을 오류에 적는다(수동 복구).
    async fn restore_name(&self, vg: &str, id: Uuid, old: &str, cause: DriverError) -> DriverError {
        match self.ops.rename(vg, old, &volume_lv(id)).await {
            Ok(()) => cause,
            Err(e) => DriverError::Lvm(format!(
                "revert of volume {id} failed ({cause}) and the volume is left as {vg}/{old}: {e}"
            )),
        }
    }
}

/// 복사 원본으로 활성화한 스냅샷 LV(`lv`, 스냅샷이 아니면 `None`). `release`로 내놓지 않고 버려지면
/// (복사 future가 취소됨) 뒤에서 비활성화한다.
struct SourceGuard {
    ops: Arc<dyn LvmOps>,
    vg: String,
    lv: Option<String>,
}

impl SourceGuard {
    /// 원본을 내놓는다. 복사는 끝났으므로 실패해도 로그만 남긴다(활성화된 채 남을 뿐이다).
    async fn release(mut self) {
        if let Some(lv) = self.lv.take() {
            deactivate(&*self.ops, &self.vg, &lv).await;
        }
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        let Some(lv) = self.lv.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (ops, vg) = (self.ops.clone(), std::mem::take(&mut self.vg));
        rt.spawn(async move { deactivate(&*ops, &vg, &lv).await });
    }
}

async fn deactivate(ops: &dyn LvmOps, vg: &str, lv: &str) {
    if let Err(e) = ops.set_active(vg, lv, false).await {
        tracing::warn!(lv = %format!("{vg}/{lv}"), error = %e, "cannot release copy source");
    }
}

#[async_trait]
impl Storage for LvmThinStorage {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let (vg, thinpool) = Self::target(pool, format)?;
        Ok(self.create_lv(vg, thinpool, id, size_bytes).await?.path)
    }

    /// 원본의 가상 크기로 볼륨 LV를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let (vg, thinpool) = Self::target(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let dst = self.create_lv(vg, thinpool, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(vg, id, &dst, &from).await?;
        Ok(dst.path)
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 볼륨 LV를 먼저 지운다(쓰고 있으면 실패하고 스냅샷은 그대로 남는다). 그다음 스냅샷 LV와
    /// 실패한 되돌리기가 남긴 LV를 지운다. 볼륨 LV가 없어도(지우다 멈춘 뒤 다시 지움) 남은 것을
    /// 지우고 `VolumeNotFound`다. 볼륨 LV가 `-revert-` 이름으로만 남았으면 먼저 이름을 되돌려
    /// 볼륨으로 지운다([`volume_lvs`](Self::volume_lvs): 명령이 돌고 있거나 여럿이면 지우지 않는다).
    /// 볼륨 LV가 없어도 그 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 만들기) LV가 곧 생길 수
    /// 있으므로 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lvs = self.volume_lvs(vg, id).await?;
        let found = lvs.iter().any(|lv| lv.name == volume_lv(id));
        if !found && self.ops.changing(vg, &volume_lv(id)) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        if found {
            self.ops.remove(vg, &volume_lv(id)).await?;
        }
        let (snapshots, reverts) = (snapshot_prefix(id), revert_prefix(id));
        for lv in lvs
            .iter()
            .filter(|lv| lv.name.starts_with(&snapshots) || lv.name.starts_with(&reverts))
        {
            self.ops.remove(vg, &lv.name).await?;
        }
        if !found {
            return Err(DriverError::VolumeNotFound(id));
        }
        Ok(())
    }

    /// 비활성인 볼륨 LV를 활성화한다.
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        if !self.existing(vg, id).await?.is_active() {
            self.ops.set_active(vg, &volume_lv(id), true).await?;
        }
        Ok(())
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = self.existing(vg, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: lv.size_bytes,
            allocated_bytes: lv.used_bytes(),
        })
    }

    /// thin pool의 크기와 남은 데이터 공간
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let (vg, thinpool) = Self::target(pool, ImageFormat::Raw)?;
        let lv = self.thin_pool(vg, thinpool).await?;
        Ok(Capacity {
            total_bytes: lv.size_bytes,
            available_bytes: lv.size_bytes - lv.used_bytes(),
        })
    }

    /// thin pool이 새 블록을 0으로 채우는지([`LvInfo::zeroes_new_blocks`])
    async fn target_reads_zero(&self, pool: &StoragePool) -> Result<bool, DriverError> {
        let (vg, thinpool) = Self::target(pool, ImageFormat::Raw)?;
        Ok(self.thin_pool(vg, thinpool).await?.zeroes_new_blocks())
    }

    /// 볼륨이 raw이므로 thin 스냅샷으로 스냅샷을 둔다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Raw
    }

    /// VM이 실행 중이어도 만들 수 있다(일관성은 호출자가 VM을 잠시 멈춰 맞춘다).
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let name = Self::snapshot_lv(id, tag)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        if lvs.iter().any(|lv| lv.name == name) {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        self.ops.snapshot(vg, &volume_lv(id), &name).await
    }

    /// 볼륨 LV를 `opl-{id}-revert-{tag}`로 옮기고, 스냅샷 LV의 thin 스냅샷을 새 볼륨 LV로 만들어
    /// 활성화하고 활성화 건너뛰기 표시를 지운 뒤 옮긴 옛 LV를 지운다. 새 LV를 만들거나 활성화하지
    /// 못하면 옛 LV를 되돌린다. 볼륨 LV가 `-revert-` 이름으로만 남았으면 먼저 되돌리고
    /// ([`volume_lvs`](Self::volume_lvs)), 앞서 실패한 되돌리기가 남긴 `-revert-` LV는 먼저 지운다
    /// (볼륨 LV는 있다). 단계가 시간을 넘겼으면 다시 보고 끝났으면 이어서 한다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let snapshot = self.existing_snapshot(vg, id, tag).await?.name;
        let (volume, old) = (volume_lv(id), format!("{}{tag}", revert_prefix(id)));
        // 볼륨 LV가 있으므로 남은 `-revert-` LV는 모두 옛것이다. 남겨 두면 다음에 볼륨 LV가 없을
        // 때 어느 것이 볼륨인지 모른다.
        for lv in self.ops.list(vg).await? {
            if lv.name.starts_with(&revert_prefix(id)) {
                self.ops.remove(vg, &lv.name).await?;
            }
        }
        if let Err(e) = self.ops.rename(vg, &volume, &old).await
            && !self.done_after(&e, vg, &old, false).await
        {
            return Err(e);
        }
        if let Err(e) = self.ops.snapshot(vg, &snapshot, &volume).await
            && !self.done_after(&e, vg, &volume, false).await
        {
            return Err(self.restore_name(vg, id, &old, e).await);
        }
        if let Err(e) = self.ops.set_active(vg, &volume, true).await
            && !self.done_after(&e, vg, &volume, true).await
        {
            if let Err(del) = self.ops.remove(vg, &volume).await {
                return Err(DriverError::Lvm(format!(
                    "revert of volume {id} failed ({e}); the old volume is left as {vg}/{old} \
                     and the new one as {vg}/{volume}: {del}"
                )));
            }
            return Err(self.restore_name(vg, id, &old, e).await);
        }
        // 스냅샷으로 만든 LV에 붙은 표시를 지워 노드를 다시 켜면 다른 LV처럼 활성화되게 한다.
        // 지우지 못해도 VM을 시작할 때 표시와 상관없이 활성화한다(`-K`).
        if let Err(e) = self.ops.clear_skip(vg, &volume).await {
            tracing::warn!(volume = %id, error = %e, "cannot clear activation skip of reverted volume");
        }
        // 되돌리기는 끝났다. 옛 LV가 남으면 공간만 차지한다.
        if let Err(e) = self.ops.remove(vg, &old).await {
            tracing::warn!(volume = %id, lv = %old, error = %e, "cannot remove reverted volume");
        }
        Ok(())
    }

    /// VM이 실행 중이어도 지울 수 있다(QEMU는 스냅샷 LV를 쓰지 않는다). 스냅샷 LV가 없으면 지운
    /// 것으로 본다. 다만 그 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) LV가 곧 생길
    /// 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let name = Self::snapshot_lv(id, tag)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the snapshot",
            ));
        }
        if self.find(vg, &name).await?.is_some() {
            self.ops.remove(vg, &name).await?;
        }
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        let prefix = snapshot_prefix(id);
        Ok(lvs
            .iter()
            .filter_map(|lv| lv.name.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }

    /// VM이 실행 중이어도 늘릴 수 있다(그 뒤 QEMU에 새 크기를 알리는 것은 호출자의 일이다).
    /// LV가 이미 그 크기 이상이면(앞선 시도가 늘렸다) 아무것도 하지 않는다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        if self.existing(vg, id).await?.size_bytes >= size_bytes {
            return Ok(());
        }
        self.ops.extend(vg, &volume_lv(id), size_bytes).await
    }

    /// 볼륨 LV(이미 쓰고 있을 수 있으므로 내놓을 때 비활성화하지 않는다) 또는 활성화한 스냅샷 LV
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = match tag {
            Some(tag) => self.existing_snapshot(vg, id, tag).await?,
            None => self.existing(vg, id).await?,
        };
        if !lv.is_active() {
            self.ops.set_active(vg, &lv.name, true).await?;
        }
        Ok(VolumeSource {
            path: device_node(&self.dev, vg, &lv.name)?,
            format,
            snapshot: None,
            size_bytes: lv.size_bytes,
            rbd: None,
        })
    }

    /// 스냅샷 LV를 다시 비활성화한다.
    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        match tag {
            Some(tag) => {
                let name = Self::snapshot_lv(id, tag)?;
                self.ops.set_active(vg, &name, false).await
            }
            None => Ok(()),
        }
    }

    /// 원본(스냅샷이면 그 LV)의 크기로 대상 볼륨 LV를 만들고 원본을 써 넣는다.
    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, thinpool) = Self::target(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        let guard = self.source_guard(src_pool, src_id, format, tag)?;
        let copied = async {
            let dst = self
                .create_lv(vg, thinpool, dst_id, source.size_bytes)
                .await?;
            let from = convert_from(&source, force_share, Some(dst_format))?;
            self.fill(vg, dst_id, &dst, &from).await
        };
        let copied = copied.await;
        guard.release().await;
        copied
    }
}

/// 공유 LVM 풀은 raw 볼륨만 담는다.
pub(super) fn require_shared_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "lvm-shared pools hold raw volumes".into(),
        )),
    }
}

/// 공유 LVM 볼륨의 크기 단위(1 MiB)
const SHARED_SIZE_UNIT: u64 = 1 << 20;

/// 늘린 뒤 아직 0으로 채우지 않은 부분의 시작(바이트)을 LV에 적는 태그의 앞부분
const ZERO_FROM_TAG: &str = "opl-zero-from-";

/// 이 노드가 LV의 남은 부분을 0으로 채우는 중일 때의 일시 오류(`Timeout`)에 담는 말
const LV_ZEROING: &str = "the lv to be zeroed";

/// 다른 노드가 켜 둔 LV를 바꾸거나 켜려 했을 때의 일시 오류(`Timeout`)에 담는 말
const LV_HELD: &str = "another node to stop using the lv";

/// 다른 노드가 공유 VG의 잠금을 쥐고 있을 때의 일시 오류(`Timeout`)에 담는 말
const VG_HELD: &str = "another node to release the shared vg lock";

/// 다른 노드가 LV를 켜 두었다는 lvmlockd의 말("LV locked by other host")
const LV_HELD_BY: &str = "locked by other host";

/// 다른 노드가 VG 잠금을 쥐고 있다는 lvmlockd의 말("VG ... lock failed: held by other host")
const VG_HELD_BY: &str = "held by other host";

/// 공유 VG에서 다른 노드가 잠금을 쥐어 실패한 LVM 명령은 일시 오류로 바꾼다. 이 말이 든 줄은 앞에
/// 경고 줄("Retrying ex lock …")이 있어도 오류에 담긴다([`LVM`]).
fn held_elsewhere(e: DriverError) -> DriverError {
    match e {
        DriverError::Lvm(msg) if msg.contains(LV_HELD_BY) => DriverError::Timeout(LV_HELD),
        DriverError::Lvm(msg) if msg.contains(VG_HELD_BY) => DriverError::Timeout(VG_HELD),
        e => e,
    }
}

fn no_snapshots() -> DriverError {
    DriverError::InvalidSpec("lvm-shared pools have no snapshots".into())
}

/// 공유 LVM 풀(`lvm-shared`)의 볼륨. 볼륨은 공유 VG의 두꺼운 LV이고, 쓰는 동안만 이 노드에서
/// 배타 활성화한다(VM 디스크면 [`activate_volume`](Storage::activate_volume)부터
/// [`deactivate_volume`](Storage::deactivate_volume)까지). 스냅샷은 없다.
pub struct LvmSharedStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn LvmOps>,
    /// LV 장치 노드가 놓이는 디렉터리([`DEV`]). 테스트는 바꾼다.
    dev: PathBuf,
    /// 이 노드에서 늘린 부분을 0으로 채우는 중인 LV(`{vg}/{lv}`, [`Zeroing`])
    zeroing: Mutex<BTreeSet<String>>,
}

/// LV를 0으로 채우는 중이라는 표시. 버려지면 지운다.
struct Zeroing<'a> {
    set: &'a Mutex<BTreeSet<String>>,
    lv: String,
}

impl<'a> Zeroing<'a> {
    /// 이미 채우는 중이면 일시 오류다.
    fn begin(set: &'a Mutex<BTreeSet<String>>, vg: &str, name: &str) -> Result<Self, DriverError> {
        let lv = format!("{vg}/{name}");
        if !set.lock().unwrap().insert(lv.clone()) {
            return Err(DriverError::Timeout(LV_ZEROING));
        }
        Ok(Self { set, lv })
    }
}

impl Drop for Zeroing<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.lv);
    }
}

impl LvmSharedStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn LvmOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(DEV),
            zeroing: Mutex::default(),
        }
    }

    /// 늘린 뒤 아직 0으로 채우지 않은 부분의 시작(태그 `opl-zero-from-{바이트}`)
    async fn pending_zero(&self, vg: &str, name: &str) -> Result<Option<u64>, DriverError> {
        Ok(self
            .ops
            .tags(vg, name)
            .await?
            .iter()
            .find_map(|t| t.strip_prefix(ZERO_FROM_TAG)?.parse::<u64>().ok()))
    }

    /// 남은 부분(태그의 위치부터 끝까지)을 0으로 채우고 태그를 뗀다. LV는 이 노드에서 켜져 있고
    /// [`Zeroing`]을 쥐고 있어야 한다. 태그의 위치가 LV 끝이면(늘리기 전) 채울 것이 없으므로 태그를
    /// 그대로 둔다(늘린 뒤 채운다).
    async fn zero_pending(&self, vg: &str, id: Uuid) -> Result<(), DriverError> {
        let name = volume_lv(id);
        let Some(from) = self.pending_zero(vg, &name).await? else {
            return Ok(());
        };
        if from >= self.existing(vg, id).await?.size_bytes {
            return Ok(());
        }
        self.ops.zero(vg, &name, from).await?;
        self.ops
            .set_tag(vg, &name, &format!("{ZERO_FROM_TAG}{from}"), false)
            .await
            .map_err(held_elsewhere)
    }

    /// 꺼져 있으면 배타 활성화하고 크기 늘리기가 남긴 0 채우기(태그)를 마친 뒤 LV를 돌려준다. 켜 둔
    /// LV의 장치를 내주기 전에(VM 시작, 복제·복사 원본) 부른다. 남은 부분에는 지운 볼륨의 옛 데이터가
    /// 있을 수 있고, 장치를 쓰기 시작한 뒤에 채우면 쓴 데이터를 지운다. [`Zeroing`]을 잡은 뒤 LV를
    /// 읽으므로 그동안 이 노드의 크기 늘리기가 LV를 켜거나 끄지 않는다. 이 노드가 그 LV를 채우는
    /// 중이거나(크기 늘리기) 그 LV를 바꾸는 명령이 도는 중이면 LV를 건드리지 않고 일시 오류다. 마치지
    /// 못하면 자기가 켠 LV만 끈다.
    async fn activate_ready(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        let lv = self.existing(vg, id).await?;
        if !lv.is_active() {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
        }
        let done = self.zero_pending(vg, id).await;
        if done.is_err() && !lv.is_active() {
            self.deactivate_quietly(vg, id).await;
        }
        done.map(|()| lv)
    }

    /// 풀의 VG. raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, format: ImageFormat) -> Result<&str, DriverError> {
        require_shared_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::LvmShared { vg, .. } => Ok(vg),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-shared pool".into()))
            }
        }
    }

    /// 볼륨 LV. 없으면 `VolumeNotFound`.
    async fn existing(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        self.ops
            .list(vg)
            .await?
            .into_iter()
            .find(|lv| lv.name == volume_lv(id))
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨 LV를 만들어 배타 활성화하고 0으로 채운다(켜 둔 채 돌려준다). 크기는 1 MiB 배수로
    /// 올린다. 켜거나 채우지 못하면 만든 LV를 지운다.
    async fn create_lv(&self, vg: &str, id: Uuid, size_bytes: u64) -> Result<PathBuf, DriverError> {
        let name = volume_lv(id);
        if self.ops.list(vg).await?.iter().any(|lv| lv.name == name) {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.div_ceil(SHARED_SIZE_UNIT) * SHARED_SIZE_UNIT;
        self.ops
            .create_linear(vg, &name, size)
            .await
            .map_err(held_elsewhere)?;
        let zeroed = async {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
            self.ops.zero(vg, &name, 0).await?;
            device_node(&self.dev, vg, &name)
        };
        match zeroed.await {
            Ok(path) => Ok(path),
            Err(e) => {
                self.remove_partial(vg, id).await;
                Err(e)
            }
        }
    }

    /// 만들다 만 볼륨 LV를 지운다. 지우지 못하면 로그만 남긴다.
    async fn remove_partial(&self, vg: &str, id: Uuid) {
        if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
            tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
        }
    }

    /// 볼륨 LV를 끈다. 끄지 못하면 로그만 남긴다(켜진 채 남은 LV는 opld가 뒤에 정리한다).
    async fn deactivate_quietly(&self, vg: &str, id: Uuid) {
        if let Err(e) = self.ops.set_active(vg, &volume_lv(id), false).await {
            tracing::warn!(volume = %id, error = %e, "cannot deactivate volume");
        }
    }

    /// 새 볼륨 LV를 만들어(0으로 채운다) `qemu-img convert -n --target-is-zero`로 써 넣은 뒤 끈다.
    /// 쓰다 실패하면 볼륨 LV를 지운다.
    async fn fill(
        &self,
        vg: &str,
        id: Uuid,
        size_bytes: u64,
        from: &[String],
    ) -> Result<PathBuf, DriverError> {
        let path = self.create_lv(vg, id, size_bytes).await?;
        let written = async {
            let mut args = vec!["convert", "-q", "-n", "--target-is-zero"];
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(&path)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            self.remove_partial(vg, id).await;
            return Err(e);
        }
        self.deactivate_quietly(vg, id).await;
        Ok(path)
    }
}

#[async_trait]
impl Storage for LvmSharedStorage {
    /// 만들어 0으로 채운 뒤 끈다. 돌려주는 장치 경로는 켜야 생긴다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let vg = Self::target(pool, format)?;
        self.create_lv(vg, id, size_bytes).await?;
        self.deactivate_quietly(vg, id).await;
        Ok(device_path(&self.dev, vg, &volume_lv(id)))
    }

    /// 원본의 가상 크기로 볼륨 LV를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let vg = Self::target(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(vg, id, size, &from).await
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 다른 노드가 켜 두었으면 lvmlockd가 거절하므로 일시 오류다. 볼륨 LV가 없어도 그 LV를 바꾸는
    /// 명령이 아직 돌고 있으면(시간을 넘긴 만들기) LV가 곧 생길 수 있으므로 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        match self.existing(vg, id).await {
            Err(DriverError::VolumeNotFound(_)) if self.ops.changing(vg, &volume_lv(id)) => Err(
                DriverError::Timeout("an earlier lvm command still running on the volume"),
            ),
            Err(e) => Err(e),
            Ok(_) => self
                .ops
                .remove(vg, &volume_lv(id))
                .await
                .map_err(held_elsewhere),
        }
    }

    /// 이 노드에서 꺼져 있으면 배타 활성화한다. 다른 노드가 켜 두었으면 일시 오류다. 크기 늘리기가
    /// 남긴 0 채우기를 마친 뒤 돌아온다([`activate_ready`](Self::activate_ready)).
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        self.activate_ready(vg, id).await.map(drop)
    }

    /// 이 노드에서 켜져 있으면 끈다. 볼륨이 없거나 이미 꺼져 있으면 할 일이 없다. 이 노드가 0으로
    /// 채우는 중인 LV([`Zeroing`])는 끄지 않고 성공이다(켜진 채 남으면 남은 활성화 정리가 끈다).
    /// 끄는 동안 표시를 쥐어 그사이 크기 늘리기가 LV를 켜지 않게 한다.
    async fn deactivate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let Ok(_zeroing) = Zeroing::begin(&self.zeroing, vg, &volume_lv(id)) else {
            tracing::info!(vg, lv = %volume_lv(id), "lv is being zeroed; leaving it active for the idle sweep");
            return Ok(());
        };
        match self.existing(vg, id).await {
            Ok(lv) if lv.is_active() => self.ops.set_active(vg, &volume_lv(id), false).await,
            Ok(_) | Err(DriverError::VolumeNotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 두꺼운 LV이므로 크기만큼 쓴다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let vg = Self::target(pool, format)?;
        let lv = self.existing(vg, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: lv.size_bytes,
            allocated_bytes: lv.size_bytes,
        })
    }

    /// VG의 크기와 남은 공간
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let vg = Self::target(pool, ImageFormat::Raw)?;
        let info = self.ops.vg_info(vg).await?;
        Ok(Capacity {
            total_bytes: info.size_bytes,
            available_bytes: info.free_bytes,
        })
    }

    /// 새 볼륨은 만들 때 0으로 채운다.
    async fn target_reads_zero(&self, _pool: &StoragePool) -> Result<bool, DriverError> {
        Ok(true)
    }

    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        false
    }

    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Err(no_snapshots())
    }

    /// 늘린 뒤 새 부분(늘리기 전 크기부터 끝까지)을 0으로 채운다. 새 익스텐트에는 지운 볼륨의 옛
    /// 데이터가 남아 있을 수 있다. 꺼져 있으면 배타 활성화해 채운 뒤 끄고, 이 노드에서 켜져 있으면
    /// (실행 중인 VM) 켠 채로 둔다. 다른 노드가 켜 두었으면 일시 오류다.
    ///
    /// 늘리기 전에 늘리기 전 크기를 태그(`opl-zero-from-{바이트}`)로 붙이고 다 채운 뒤 뗀다. 채우기가
    /// 실패하면 오류이고 태그가 남으므로, 다시 하면(LV가 이미 그 크기여도) 남은 부분을 다시 채운다.
    /// 그 전에 VM 시작이나 복제 원본이 LV를 켜면 그쪽이 마저 채운다. 태그가 없고 LV가 이미 그 크기
    /// 이상이면 할 일이 없다. LV를 읽기 전부터 다 채울 때까지 [`Zeroing`]을 쥐어, 그동안 이 노드의
    /// VM 시작·복제 원본은 일시 오류다(읽은 활성 여부가 끝까지 맞다).
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        let lv = self.existing(vg, id).await?;
        let marked = self.pending_zero(vg, &name).await?;
        let grow = lv.size_bytes < size_bytes;
        if !grow && marked.is_none() {
            return Ok(());
        }
        if grow {
            if marked.is_none() {
                let tag = format!("{ZERO_FROM_TAG}{}", lv.size_bytes);
                self.ops
                    .set_tag(vg, &name, &tag, true)
                    .await
                    .map_err(held_elsewhere)?;
            }
            self.ops
                .extend(vg, &name, size_bytes)
                .await
                .map_err(held_elsewhere)?;
        }
        if !lv.is_active() {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
        }
        let zeroed = self.zero_pending(vg, id).await;
        if !lv.is_active() {
            self.deactivate_quietly(vg, id).await;
        }
        zeroed
    }

    /// 볼륨 LV를 배타 활성화하고 남은 0 채우기를 마친 뒤 읽게 한다. 스냅샷이 없으므로 `tag`는
    /// `InvalidSpec`이다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let vg = Self::target(pool, format)?;
        if tag.is_some() {
            return Err(no_snapshots());
        }
        let lv = self.activate_ready(vg, id).await?;
        Ok(VolumeSource {
            path: device_node(&self.dev, vg, &lv.name)?,
            format,
            snapshot: None,
            size_bytes: lv.size_bytes,
            rbd: None,
        })
    }

    /// 볼륨 LV를 끈다.
    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        if tag.is_some() {
            return Err(no_snapshots());
        }
        self.ops.set_active(vg, &volume_lv(id), false).await
    }

    /// 원본 볼륨을 켜서 그 크기로 대상 볼륨 LV를 만들고 써 넣은 뒤 둘 다 끈다.
    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        // 읽는 동안 취소돼도 원본 LV를 끈다.
        let guard = SourceGuard {
            ops: self.ops.clone(),
            vg: Self::target(src_pool, format)?.to_string(),
            lv: Some(volume_lv(src_id)),
        };
        let copied = async {
            let from = convert_from(&source, force_share, Some(dst_format))?;
            self.fill(vg, dst_id, source.size_bytes, &from).await
        };
        let copied = copied.await;
        guard.release().await;
        copied.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    #[derive(Debug, Clone)]
    struct FakeLv {
        size: u64,
        active: bool,
        /// 활성화 건너뛰기 표시
        skip: bool,
    }

    /// VG `vg0` 하나를 메모리에 두는 LVM. thin pool `tp`와 관계없는 LV `root`가 있다. 바꾸는 호출을
    /// 적고, `fail`에 적은 호출은 실패한다. `stuck`에 적은 호출은 하지 않고, `late`에 적은 이름
    /// 바꾸기·스냅샷은 해 놓고 `Timeout`을 돌려준다(시간을 넘긴 명령). 활성인 LV의 장치 노드를 `dev`
    /// 아래에 둔다(udev처럼). `busy`에 적은 LV는 바꾸는 명령이 아직 돌고 있다고 알린다. `held`에
    /// 적은 LV는 다른 노드가 켜 두었다(공유 VG: 켜기·지우기·늘리기를 lvmlockd가 거절한다).
    /// `on_list`는 다음 `list`가 결과를 만든 뒤 한 번 부른다(읽은 뒤 끼어드는 다른 작업).
    struct FakeLvm {
        lvs: Mutex<BTreeMap<String, FakeLv>>,
        busy: Mutex<Vec<String>>,
        held: Mutex<Vec<String>>,
        /// LV별 태그
        tags: Mutex<BTreeMap<String, Vec<String>>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        late: Mutex<Vec<String>>,
        stuck: Mutex<Vec<String>>,
        /// thin pool `tp`가 새 블록을 0으로 채우는지(`lv_attr` 여덟째 글자 `z`)
        zero: Mutex<bool>,
        on_list: Mutex<Option<OnList>>,
        dev: tempfile::TempDir,
    }

    type OnList = Box<dyn FnOnce(&mut BTreeMap<String, FakeLv>) + Send>;

    impl Default for FakeLvm {
        fn default() -> Self {
            Self {
                lvs: Mutex::default(),
                busy: Mutex::default(),
                held: Mutex::default(),
                tags: Mutex::default(),
                log: Mutex::default(),
                fail: Mutex::default(),
                late: Mutex::default(),
                stuck: Mutex::default(),
                zero: Mutex::new(true),
                on_list: Mutex::default(),
                dev: tempfile::tempdir().unwrap(),
            }
        }
    }

    impl FakeLvm {
        /// LV `name`의 장치 노드 경로
        fn node(&self, name: &str) -> PathBuf {
            device_path(self.dev.path(), "vg0", name)
        }

        /// 장치 노드를 활성인 LV들에 맞춘다.
        fn sync_nodes(&self, lvs: &BTreeMap<String, FakeLv>) {
            let dir = self.dev.path().join("vg0");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for name in lvs.iter().filter(|(_, lv)| lv.active).map(|(name, _)| name) {
                std::fs::write(dir.join(name), b"").unwrap();
            }
        }

        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            let stuck = self.stuck.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Lvm(format!("injected: {call}")));
            }
            if stuck {
                return Err(DriverError::Timeout("lvm command"));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        /// 해 놓은 호출 `call`이 `late`에 있으면 `Timeout`
        fn finish(&self, call: &str) -> Result<(), DriverError> {
            if self.late.lock().unwrap().iter().any(|c| c == call) {
                return Err(DriverError::Timeout("lvm command"));
            }
            Ok(())
        }

        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.lvs.lock().unwrap().keys().cloned().collect()
        }

        fn lv(&self, name: &str) -> Option<FakeLv> {
            self.lvs.lock().unwrap().get(name).cloned()
        }

        fn missing(name: &str) -> DriverError {
            DriverError::Lvm(format!("Failed to find logical volume \"vg0/{name}\""))
        }

        /// 다른 노드가 켜 둔 LV면 lvmlockd의 거절
        fn check_held(&self, name: &str) -> Result<(), DriverError> {
            if self.held.lock().unwrap().iter().any(|lv| lv == name) {
                return Err(DriverError::Lvm(format!(
                    "lvchange: LV locked by other host: vg0/{name}"
                )));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl LvmOps for FakeLvm {
        async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError> {
            assert_eq!(vg, "vg0");
            let mut out = vec![
                LvInfo {
                    name: "root".into(),
                    attr: "-wi-ao----".into(),
                    size_bytes: 1 << 30,
                    data_percent: None,
                },
                LvInfo {
                    name: "tp".into(),
                    attr: if *self.zero.lock().unwrap() {
                        "twi-aotz--"
                    } else {
                        "twi-aot---"
                    }
                    .into(),
                    size_bytes: 8 << 30,
                    data_percent: Some(25.0),
                },
            ];
            for (name, lv) in self.lvs.lock().unwrap().iter() {
                out.push(LvInfo {
                    name: name.clone(),
                    attr: if lv.active {
                        "Vwi-a-tz--"
                    } else {
                        "Vwi---tz--"
                    }
                    .into(),
                    size_bytes: lv.size,
                    data_percent: lv.active.then_some(50.0),
                });
            }
            let hook = self.on_list.lock().unwrap().take();
            if let Some(hook) = hook {
                hook(&mut self.lvs.lock().unwrap());
            }
            Ok(out)
        }

        async fn create_thin(
            &self,
            vg: &str,
            thinpool: &str,
            name: &str,
            size_bytes: u64,
        ) -> Result<(), DriverError> {
            self.call(format!("create {vg}/{thinpool} {name} {size_bytes}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            let lv = FakeLv {
                size: size_bytes,
                active: true,
                skip: false,
            };
            lvs.insert(name.into(), lv);
            self.sync_nodes(&lvs);
            Ok(())
        }

        async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError> {
            let call = format!("snapshot {vg}/{origin} {name}");
            self.call(call.clone())?;
            let mut lvs = self.lvs.lock().unwrap();
            let size = lvs.get(origin).ok_or_else(|| Self::missing(origin))?.size;
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            // thin 스냅샷은 활성화 건너뛰기 표시가 붙어 비활성이다.
            let lv = FakeLv {
                size,
                active: false,
                skip: true,
            };
            lvs.insert(name.into(), lv);
            self.finish(&call)
        }

        async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("remove {vg}/{name}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            let removed = lvs.remove(name);
            self.sync_nodes(&lvs);
            removed.map(drop).ok_or_else(|| Self::missing(name))
        }

        async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("extend {vg}/{name} {size_bytes}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            let lv = lvs.get_mut(name).ok_or_else(|| Self::missing(name))?;
            if size_bytes <= lv.size {
                return Err(DriverError::Lvm("New size not larger".into()));
            }
            lv.size = size_bytes;
            Ok(())
        }

        async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError> {
            let call = format!("rename {vg}/{from} {to}");
            self.call(call.clone())?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(to) {
                return Err(DriverError::Lvm(format!("{to} already exists")));
            }
            let lv = lvs.remove(from).ok_or_else(|| Self::missing(from))?;
            lvs.insert(to.into(), lv);
            self.sync_nodes(&lvs);
            self.finish(&call)
        }

        async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError> {
            let op = if active { "activate" } else { "deactivate" };
            self.call(format!("{op} {vg}/{name}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.active = active;
            self.sync_nodes(&lvs);
            Ok(())
        }

        async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("unskip {vg}/{name}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.skip = false;
            Ok(())
        }

        fn changing(&self, vg: &str, name: &str) -> bool {
            assert_eq!(vg, "vg0");
            self.busy.lock().unwrap().iter().any(|lv| lv == name)
        }

        async fn create_linear(
            &self,
            vg: &str,
            name: &str,
            size_bytes: u64,
        ) -> Result<(), DriverError> {
            self.call(format!("create-linear {vg} {name} {size_bytes}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            let lv = FakeLv {
                size: size_bytes,
                active: false,
                skip: false,
            };
            lvs.insert(name.into(), lv);
            Ok(())
        }

        async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("activate-ex {vg}/{name}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.active = true;
            self.sync_nodes(&lvs);
            Ok(())
        }

        /// 켜 둔 LV만 채울 수 있다(장치 노드가 있어야 한다).
        async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError> {
            self.call(format!("zero {vg}/{name} {offset}"))?;
            if !self.lv(name).is_some_and(|lv| lv.active) {
                return Err(DriverError::Lvm(format!("blkdiscard: {name} not active")));
            }
            Ok(())
        }

        async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError> {
            assert_eq!(vg, "vg0");
            self.lv(name).ok_or_else(|| Self::missing(name))?;
            Ok(self
                .tags
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .unwrap_or_default())
        }

        async fn set_tag(
            &self,
            vg: &str,
            name: &str,
            tag: &str,
            on: bool,
        ) -> Result<(), DriverError> {
            let op = if on { "addtag" } else { "deltag" };
            self.call(format!("{op} {vg}/{name} {tag}"))?;
            self.lv(name).ok_or_else(|| Self::missing(name))?;
            let mut tags = self.tags.lock().unwrap();
            let tags = tags.entry(name.into()).or_default();
            tags.retain(|t| t != tag);
            if on {
                tags.push(tag.into());
            }
            Ok(())
        }

        async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError> {
            assert_eq!(vg, "vg0");
            Ok(VgInfo {
                name: "vg0".into(),
                lock_type: "sanlock".into(),
                size_bytes: 10 << 30,
                free_bytes: 4 << 30,
            })
        }
    }

    fn lvm_pool() -> StoragePool {
        Resource::new(
            "vg",
            StoragePoolSpec {
                backend: PoolBackend::LvmThin {
                    vg: "vg0".into(),
                    thinpool: "tp".into(),
                },
                nodes: vec![],
                shared: false,
            },
        )
    }

    /// `info`면 가상 크기 1000을 알리고, 그 밖은 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜
    /// qemu-img. `fail`이면 `info`가 아닌 호출이 실패한다.
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        crate::storage::tests::write_script(
            &path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = info ]; then echo '{{\"virtual-size\": 1000}}'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {args:?}\n{end}\n"
            ),
        );
        path
    }

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeLvm>, LvmThinStorage) {
        let lvm = Arc::new(FakeLvm::default());
        let storage = LvmThinStorage {
            dev: lvm.dev.path().to_path_buf(),
            ..LvmThinStorage::new(qemu_img, lvm.clone())
        };
        (lvm, storage)
    }

    fn invalid(e: DriverError, want: &str) {
        match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains(want), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    const RAW: ImageFormat = ImageFormat::Raw;

    #[tokio::test]
    async fn creates_raw_thin_volumes() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        // 크기는 512 배수로 올린다.
        let path = storage
            .create_volume(&p, id, (1 << 20) + 1, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&format!("opl-{id}")));
        assert_eq!(lvm.taken(), [format!("create vg0/tp opl-{id} 1049088")]);

        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        let qcow2 = ImageFormat::Qcow2;
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), 1 << 20, qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
        assert_eq!(lvm.taken(), Vec::<String>::new());
    }

    /// 원본의 가상 크기로 LV를 만들고 raw로 변환해 써 넣는다. 실패하면 LV를 지운다.
    #[tokio::test]
    async fn imports_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        let node = lvm.node(&format!("opl-{id}"));
        let dev = node.to_str().unwrap();
        assert_eq!(path, node);
        assert_eq!(lvm.taken(), [format!("create vg0/tp opl-{id} 1024")]);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "vmdk",
                "-O",
                "raw",
                "/up/disk.vmdk",
                dev,
            ]
        );

        let (lvm, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{id} 1024"),
                format!("remove vg0/opl-{id}")
            ]
        );
        assert_eq!(lvm.names(), Vec::<String>::new());
        // 상대 경로는 qemu-img가 프로토콜로 읽을 수 있다.
        invalid(
            storage
                .import_volume(&p, id, "x.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );
    }

    /// 볼륨을 지우면 볼륨 LV를 먼저 지우고 그 스냅샷 LV와 실패한 되돌리기가 남긴 LV도 지운다. 다른
    /// 볼륨의 LV는 그대로다.
    #[tokio::test]
    async fn delete_removes_volume_with_its_snapshots() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let p = lvm_pool();
        let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [id, other] {
            storage.create_volume(&p, v, 1 << 20, RAW).await.unwrap();
            storage.snapshot_create(&p, v, RAW, "a").await.unwrap();
        }
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        lvm.snapshot("vg0", &format!("opl-{id}"), &format!("opl-{id}-revert-a"))
            .await
            .unwrap();
        lvm.taken();

        // 볼륨 LV를 지우지 못하면(쓰는 중) 스냅샷은 그대로다.
        lvm.fail_on(format!("remove vg0/opl-{id}"));
        let names = lvm.names();
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{id}")]);
        assert_eq!(lvm.names(), names);
        lvm.fail.lock().unwrap().clear();

        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("remove vg0/opl-{id}"),
                format!("remove vg0/opl-{id}-revert-a"),
                format!("remove vg0/opl-{id}-s-a"),
                format!("remove vg0/opl-{id}-s-b"),
            ]
        );
        assert_eq!(
            lvm.names(),
            [format!("opl-{other}"), format!("opl-{other}-s-a")]
        );
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeNotFound(v) if v == id),
            "{err:?}"
        );

        // 볼륨 LV를 지운 뒤 멈췄으면 다시 지울 때 남은 스냅샷 LV를 지운다.
        lvm.fail_on(format!("remove vg0/opl-{other}-s-a"));
        storage.delete_volume(&p, other, RAW).await.unwrap_err();
        lvm.fail.lock().unwrap().clear();
        assert_eq!(lvm.names(), [format!("opl-{other}-s-a")]);
        lvm.taken();
        let err = storage.delete_volume(&p, other, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{other}-s-a")]);
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn reports_volume_usage_and_thin_pool_capacity() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.volume_info(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 1 << 20,
                allocated_bytes: 1 << 19,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        // thin pool이 없거나 thin pool이 아니다.
        let mut root = lvm_pool();
        root.spec.backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "root".into(),
        };
        let err = storage.capacity(&root).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("vg0/root")),
            "{err:?}"
        );
        assert_eq!(lvm.taken().len(), 1);
    }

    #[tokio::test]
    async fn activates_only_inactive_volumes() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
        lvm.set_active("vg0", &format!("opl-{id}"), false)
            .await
            .unwrap();
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate vg0/opl-{id}")]);
        assert!(lvm.lv(&format!("opl-{id}")).unwrap().active);
    }

    #[tokio::test]
    async fn resize_extends_the_lv() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(lvm.taken(), [format!("extend vg0/opl-{id} 2097152")]);
        assert_eq!(lvm.lv(&format!("opl-{id}")).unwrap().size, 2 << 20);
        // 이미 그 크기 이상이면(앞선 시도가 늘렸다) lvextend를 부르지 않는다.
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 1 << 20).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
        assert_eq!(lvm.lv(&format!("opl-{id}")).unwrap().size, 2 << 20);
    }

    #[tokio::test]
    async fn snapshots_are_thin_snapshot_lvs() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
        let err = storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();

        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("snapshot vg0/opl-{id} opl-{id}-s-a"),
                format!("snapshot vg0/opl-{id} opl-{id}-s-b"),
            ]
        );
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        invalid(
            storage
                .snapshot_create(&p, id, RAW, "a/b")
                .await
                .unwrap_err(),
            "not a valid lvm name",
        );
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );

        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        // 없는 태그는 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{id}-s-a")]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["b"]);
        invalid(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
    }

    /// 그 스냅샷 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) LV가 아직 없어도
    /// 지우기는 일시 오류다. 명령이 끝나면 LV가 생길 수 있다.
    #[tokio::test]
    async fn snapshot_delete_waits_for_a_running_command() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        *lvm.busy.lock().unwrap() = vec![format!("opl-{id}-s-t")];
        let err = storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        lvm.busy.lock().unwrap().clear();
        storage.snapshot_delete(&p, id, RAW, "t").await.unwrap();
        assert!(lvm.taken().is_empty());
    }

    /// 볼륨 LV가 아직 없어도 그 LV를 바꾸는 명령이 돌고 있으면(시간을 넘긴 만들기) 지우기는 일시
    /// 오류다. `VolumeNotFound`로 자원을 지우면 명령이 끝나 생긴 LV가 남는다.
    #[tokio::test]
    async fn delete_waits_for_a_running_create() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        *lvm.busy.lock().unwrap() = vec![format!("opl-{id}")];
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        assert!(lvm.taken().is_empty());
        lvm.busy.lock().unwrap().clear();
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
    }

    /// 여러 볼륨의 스냅샷은 기본 구현대로 하나씩 만든다.
    #[tokio::test]
    async fn snapshot_many_takes_one_volume_at_a_time() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let p = lvm_pool();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [a, b] {
            storage.create_volume(&p, v, 1 << 20, RAW).await.unwrap();
        }
        lvm.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("snapshot vg0/opl-{a} opl-{a}-s-t"),
                format!("snapshot vg0/opl-{b} opl-{b}-s-t"),
            ]
        );
    }

    /// 볼륨 LV를 옮겨 두고 스냅샷 LV의 스냅샷을 새 볼륨 LV로 만든 뒤 옛 LV를 지운다.
    #[tokio::test]
    async fn revert_replaces_volume_with_a_snapshot_of_the_snapshot() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        lvm.taken();
        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );

        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        // 스냅샷 때의 크기로 돌아가고, 스냅샷은 남는다. 새 볼륨 LV는 활성화 건너뛰기 표시가 없다.
        let lv = lvm.lv(&volume).unwrap();
        assert!(lv.active);
        assert!(!lv.skip);
        assert_eq!(lv.size, 1 << 20);
        assert_eq!(lvm.names(), [volume.clone(), format!("opl-{id}-s-t")]);

        // 앞서 실패한 되돌리기가 남긴 옛 LV는 먼저 지운다.
        lvm.snapshot("vg0", &volume, &old).await.unwrap();
        lvm.taken();
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("remove vg0/{old}"),
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        assert_eq!(lvm.names(), [volume, format!("opl-{id}-s-t")]);
    }

    /// 새 볼륨 LV를 만들거나 활성화하지 못하면 옛 LV의 이름을 되돌린다.
    #[tokio::test]
    async fn failed_revert_restores_the_volume() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let names = lvm.names();
        lvm.taken();

        lvm.fail_on(format!("snapshot vg0/opl-{id}-s-t {volume}"));
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("rename vg0/{old} {volume}"),
            ]
        );
        assert_eq!(lvm.names(), names);

        lvm.fail.lock().unwrap().clear();
        lvm.fail_on(format!("activate vg0/{volume}"));
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("remove vg0/{volume}"),
                format!("rename vg0/{old} {volume}"),
            ]
        );
        assert_eq!(lvm.names(), names);

        // 이름을 되돌리지 못하면 남은 LV를 알린다.
        lvm.fail.lock().unwrap().clear();
        lvm.fail_on(format!("snapshot vg0/opl-{id}-s-t {volume}"));
        lvm.fail_on(format!("rename vg0/{old} {volume}"));
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains(&format!("vg0/{old}"))),
            "{err:?}"
        );
    }

    /// 이름 바꾸기나 새 볼륨 LV 만들기가 시간을 넘겼으면 `lvs`로 다시 보고, 끝났으면 되돌리기를
    /// 이어서 끝낸다.
    #[tokio::test]
    async fn revert_goes_on_after_a_step_that_timed_out_but_finished() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let steps = [
            format!("rename vg0/{volume} {old}"),
            format!("snapshot vg0/opl-{id}-s-t {volume}"),
            format!("activate vg0/{volume}"),
            format!("unskip vg0/{volume}"),
            format!("remove vg0/{old}"),
        ];
        for late in &steps[..2] {
            lvm.taken();
            *lvm.late.lock().unwrap() = vec![late.clone()];
            storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
            assert_eq!(lvm.taken(), steps, "late: {late}");
            assert_eq!(lvm.names(), [volume.clone(), format!("opl-{id}-s-t")]);
            assert!(lvm.lv(&volume).unwrap().active);
        }

        // 시간을 넘긴 이름 바꾸기가 끝나지 않았으면 그 오류로 끝난다(볼륨 LV는 그대로다).
        lvm.late.lock().unwrap().clear();
        lvm.taken();
        *lvm.stuck.lock().unwrap() = vec![steps[0].clone()];
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
        assert_eq!(lvm.taken(), steps[..1]);
        assert_eq!(lvm.names(), [volume, format!("opl-{id}-s-t")]);
    }

    /// 시간을 넘긴 이름 바꾸기가 되돌리기가 실패한 뒤에 끝나 볼륨 LV가 `-revert-` 이름으로만
    /// 남았으면, 그 LV를 다루는 명령이 돌고 있지 않을 때 볼륨 LV로 이름을 되돌린 뒤 이어서 한다.
    /// 돌고 있으면 일시 오류이고, `-revert-` LV가 여럿이면 어느 것인지 모르므로 건드리지 않는다.
    #[tokio::test]
    async fn volume_left_under_a_revert_name_is_restored() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let snapshot = format!("opl-{id}-s-t");
        // 되돌리기의 이름 바꾸기가 시간을 넘겨 실패하고, 그 뒤에 끝난다.
        let leave = || async {
            *lvm.stuck.lock().unwrap() = vec![format!("rename vg0/{volume} {old}")];
            let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
            assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
            lvm.stuck.lock().unwrap().clear();
            lvm.rename("vg0", &volume, &old).await.unwrap();
            assert_eq!(lvm.names(), [old.clone(), snapshot.clone()]);
            lvm.taken();
        };

        leave().await;
        // 아직 돌고 있으면 볼륨이 없다고 하지 않고(VM을 끄지 않는다) 잠시 뒤 다시 해 볼 오류다.
        for busy in [&old, &volume] {
            *lvm.busy.lock().unwrap() = vec![busy.clone()];
            let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
            assert!(err.is_transient(), "{busy}: {err:?}");
            let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
            assert!(err.is_transient(), "{busy}: {err:?}");
            assert_eq!(lvm.names(), [old.clone(), snapshot.clone()]);
        }
        assert!(lvm.taken().is_empty());
        lvm.busy.lock().unwrap().clear();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("rename vg0/{old} {volume}")]);
        assert_eq!(lvm.names(), [volume.clone(), snapshot.clone()]);
        let info = storage.volume_info(&p, id, RAW).await.unwrap();
        assert_eq!(info.virtual_bytes, 1 << 20);

        // 되돌리기를 다시 하면 이름을 되돌린 뒤 처음부터 한다.
        leave().await;
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{old} {volume}"),
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/{snapshot} {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        assert_eq!(lvm.names(), [volume.clone(), snapshot.clone()]);

        // `-revert-` LV가 여럿이면 어느 것이 볼륨인지 모른다. 지우기도 아무것도 지우지 않는다.
        leave().await;
        let other = format!("opl-{id}-revert-u");
        lvm.snapshot("vg0", &snapshot, &other).await.unwrap();
        lvm.taken();
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains(&old) && m.contains(&other)),
            "{err:?}"
        );
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(lvm.taken().is_empty());
        lvm.remove("vg0", &other).await.unwrap();

        // 볼륨을 지우면 `-revert-` LV를 볼륨으로 보고 지운다(볼륨이 없다고 하지 않는다).
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert!(lvm.names().is_empty());
    }

    /// 스냅샷은 활성화해 읽고 내놓을 때 비활성화한다. 볼륨 자체는 활성화만 한다.
    #[tokio::test]
    async fn volume_source_activates_snapshot_until_released() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.volume_source(&p, id, RAW, None).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        lvm.taken();

        let snap = format!("opl-{id}-s-t");
        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: lvm.node(&snap),
                format: RAW,
                snapshot: None,
                size_bytes: 1 << 20,
                rbd: None,
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("deactivate vg0/{snap}")
            ]
        );
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );

        // 크기는 읽는 LV의 것이다(스냅샷이면 그때의 크기).
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        let snapshot = storage.volume_source(&p, id, RAW, Some("t")).await.unwrap();
        assert_eq!(snapshot.size_bytes, 1 << 20);
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        lvm.taken();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.path, lvm.node(&format!("opl-{id}")));
        assert_eq!(source.size_bytes, 2 << 20);
        storage.release_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
    }

    /// 같은 VG 안 복사: 원본(스냅샷 LV)의 크기로 LV를 만들고 원본을 써 넣는다.
    #[tokio::test]
    async fn copies_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let p = lvm_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        // 스냅샷 뒤에 늘렸어도 스냅샷 때의 크기로 만든다.
        storage.resize_volume(&p, src, RAW, 2 << 20).await.unwrap();
        lvm.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &p, dst, RAW)
            .await
            .unwrap();
        let snap = format!("opl-{src}-s-t");
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("create vg0/tp opl-{dst} 1048576"),
                format!("deactivate vg0/{snap}"),
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to) = (lvm.node(&snap), lvm.node(&format!("opl-{dst}")));
        let (from, to) = (from.to_str().unwrap(), to.to_str().unwrap());
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "raw",
                "-U",
                "-O",
                "raw",
                from,
                to,
            ]
        );
        // 대상이 이미 있으면 덮어쓰지 않는다. 원본은 내놓는다.
        let err = storage
            .copy_volume(&p, src, RAW, Some("t"), true, &p, dst, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == dst),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("deactivate vg0/{snap}")
            ]
        );

        // 쓰다 실패하면 만든 LV를 지운다.
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{dst} 1048576"),
                format!("remove vg0/opl-{dst}"),
            ]
        );
        assert_eq!(lvm.names(), [format!("opl-{src}")]);
    }

    /// 장치 노드는 `{dev}/{vg}/{lv}`, 없으면 device-mapper 노드다. 둘 다 없으면 노드가 없다는 오류다.
    #[test]
    fn finds_device_nodes() {
        let dev = tempfile::tempdir().unwrap();
        let (vg, lv) = ("vg-0", "opl-x");
        let path = dev.path().join("vg-0/opl-x");
        let err = device_node(dev.path(), vg, lv).unwrap_err();
        assert!(err.to_string().contains("device node missing"), "{err}");
        assert!(
            matches!(err, DriverError::VolumeNotActive(ref p) if *p == path),
            "{err:?}"
        );
        // device-mapper는 이름 안의 `-`를 `--`로 쓴다.
        let mapper = dev.path().join("mapper/vg--0-opl--x");
        std::fs::create_dir(dev.path().join("mapper")).unwrap();
        std::fs::write(&mapper, b"").unwrap();
        assert_eq!(device_node(dev.path(), vg, lv).unwrap(), mapper);
        std::fs::create_dir(dev.path().join(vg)).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(device_node(dev.path(), vg, lv).unwrap(), path);
    }

    /// 장치 노드가 생기지 않으면 만든 LV를 지운다.
    #[tokio::test]
    async fn create_without_device_node_removes_the_lv() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let storage = LvmThinStorage {
            dev: lvm.dev.path().join("elsewhere"),
            ..storage
        };
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{id} 1048576"),
                format!("remove vg0/opl-{id}"),
            ]
        );
    }

    /// LVM 명령은 숫자를 `.`로 쓰고 파일 기술자 경고를 끄며, 실패는 `Lvm` 오류다.
    #[tokio::test]
    async fn lvm_commands_use_c_locale_and_lvm_errors() {
        let long = Duration::from_secs(10);
        let env = "echo $LC_ALL $LVM_SUPPRESS_FD_WARNINGS";
        let out = LVM.run_read("/bin/sh", &["-c", env], long).await.unwrap();
        assert_eq!(out, b"C 1\n");
        let err = LVM
            .run_read("/bin/sh", &["-c", "echo no vg >&2; exit 5"], long)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: no vg"),
            "{err:?}"
        );
    }

    /// 같은 VG 안 복사가 취소돼도(future가 버려짐) 원본 스냅샷 LV를 뒤에서 비활성화한다.
    #[tokio::test]
    async fn cancelled_copy_deactivates_the_source_snapshot() {
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        crate::storage::tests::write_script(&slow, "#!/bin/sh\nsleep 5\n");
        let (lvm, storage) = setup(slow);
        let p = lvm_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        let snap = format!("opl-{src}-s-t");

        let copy = storage.copy_volume(&p, src, RAW, Some("t"), false, &p, dst, RAW);
        let res = tokio::time::timeout(Duration::from_millis(300), copy).await;
        assert!(res.is_err(), "copy should still be running");
        for _ in 0..200 {
            if !lvm.lv(&snap).unwrap().active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!lvm.lv(&snap).unwrap().active);
    }

    /// thin pool이 새 블록을 0으로 채우지 않으면(`lvcreate -Zn`) 새 LV의 덜 쓴 블록에 옛 데이터가
    /// 보일 수 있으므로 새 LV에 써 넣을 때(가져오기·복사) `--target-is-zero`를 쓰지 않는다.
    #[tokio::test]
    async fn target_is_zero_needs_thin_pool_zeroing() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let p = lvm_pool();
        assert!(storage.target_reads_zero(&p).await.unwrap());
        *lvm.zero.lock().unwrap() = false;
        assert!(!storage.target_reads_zero(&p).await.unwrap());
        let args = || std::fs::read_to_string(tools.path().join("args")).unwrap();

        let up = Path::new("/up/disk.vmdk");
        storage
            .import_volume(&p, Uuid::now_v7(), up, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert!(args().starts_with("convert\n-q\n-n\n-f\n"), "{}", args());

        // thin pool이 없거나 thin pool이 아니면 오류다.
        let mut root = lvm_pool();
        root.spec.backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "root".into(),
        };
        let err = storage.target_reads_zero(&root).await.unwrap_err();
        assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
    }

    #[test]
    fn parses_lvs_json() {
        let out = br#"  {
      "report": [
          {
              "lv": [
                  {"lv_name":"tp", "lv_attr":"twi-aotz--", "lv_size":"8589934592", "data_percent":"12.50"},
                  {"lv_name":"opl-x", "lv_attr":"Vwi---tz-k", "lv_size":"1073741824", "data_percent":""},
                  {"lv_name":"root", "lv_attr":"-wi-ao----", "lv_size":4096, "data_percent":null}
              ]
          }
      ]
  }
"#;
        let lvs = parse_lvs(out).unwrap();
        assert_eq!(
            lvs,
            [
                LvInfo {
                    name: "tp".into(),
                    attr: "twi-aotz--".into(),
                    size_bytes: 8 << 30,
                    data_percent: Some(12.5),
                },
                LvInfo {
                    name: "opl-x".into(),
                    attr: "Vwi---tz-k".into(),
                    size_bytes: 1 << 30,
                    data_percent: None,
                },
                LvInfo {
                    name: "root".into(),
                    attr: "-wi-ao----".into(),
                    size_bytes: 4096,
                    data_percent: None,
                },
            ]
        );
        assert!(lvs[0].is_thin_pool() && lvs[0].is_active());
        assert!(lvs[0].zeroes_new_blocks());
        assert!(!lvs[2].zeroes_new_blocks());
        assert_eq!(lvs[0].used_bytes(), 1 << 30);
        assert!(!lvs[1].is_thin_pool() && !lvs[1].is_active());
        assert_eq!(lvs[1].used_bytes(), 0);

        for bad in [
            &b"not json"[..],
            br#"{"report":[{"lv":[{"lv_name":"x","lv_attr":"V","lv_size":"big"}]}]}"#,
            br#"{"report":[{"lv":[{"lv_name":"x","lv_size":"1"}]}]}"#,
            br#"{"nothing":[]}"#,
        ] {
            let err = parse_lvs(bad).unwrap_err();
            assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
        }
    }

    fn shared_pool() -> StoragePool {
        Resource::new(
            "san",
            StoragePoolSpec {
                backend: PoolBackend::LvmShared {
                    vg: "vg0".into(),
                    iscsi: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    fn setup_shared(qemu_img: impl Into<PathBuf>) -> (Arc<FakeLvm>, LvmSharedStorage) {
        let lvm = Arc::new(FakeLvm::default());
        let storage = LvmSharedStorage {
            dev: lvm.dev.path().to_path_buf(),
            ..LvmSharedStorage::new(qemu_img, lvm.clone())
        };
        (lvm, storage)
    }

    /// 다른 노드가 켜 두어 거절된 일시 오류
    fn held(e: DriverError) {
        assert!(matches!(e, DriverError::Timeout(LV_HELD)), "{e:?}");
        assert!(e.is_transient());
        assert!(e.to_string().contains("another node"), "{e}");
    }

    /// 두꺼운 LV를 켜지 않고 만든 뒤(1 MiB 배수로 올린다) 배타 활성화해 0으로 채우고 끈다.
    #[tokio::test]
    async fn shared_creates_zeroed_thick_lvs() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let path = storage
            .create_volume(&p, id, (1 << 20) + 1, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&lv));
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert!(storage.target_reads_zero(&p).await.unwrap());

        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), 1 << 20, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "lvm-shared pools hold raw volumes",
        );
        assert_eq!(lvm.taken(), Vec::<String>::new());
        // 다른 종류의 풀은 다루지 않는다.
        invalid(
            storage.capacity(&lvm_pool()).await.unwrap_err(),
            "not an lvm-shared pool",
        );
    }

    /// 0으로 채우지 못하면 만든 LV를 지운다.
    #[tokio::test]
    async fn shared_create_removes_the_lv_when_zeroing_fails() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        lvm.fail_on(format!("zero vg0/{lv} 0"));
        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 1048576"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("remove vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    /// 이 노드가 0으로 채우는 중인 LV(크기 늘리기)는 끄지 않고 성공이다(채우는 쪽이 켜 둔 LV를 끄면
    /// 채우기가 실패한다. 남은 활성화는 나중의 정리가 끈다).
    #[tokio::test]
    async fn shared_deactivation_skips_a_zeroing_lv() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        lvm.taken();

        let zeroing = format!("vg0/{lv}");
        storage.zeroing.lock().unwrap().insert(zeroing.clone());
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert!(lvm.taken().is_empty());
        assert!(lvm.lv(&lv).unwrap().active);
        storage.zeroing.lock().unwrap().remove(&zeroing);

        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("deactivate vg0/{lv}")]);
        assert!(!lvm.lv(&lv).unwrap().active);
        // 끄는 동안 표시를 쥐었다가 놓는다.
        assert!(storage.zeroing.lock().unwrap().is_empty());
    }

    /// 꺼져 있으면 배타 활성화하고, 다른 노드가 켜 두었으면 일시 오류다. 끄기는 켜져 있을 때만
    /// 끄고, 볼륨이 없거나 이미 꺼져 있으면 성공이다.
    #[tokio::test]
    async fn shared_activation_is_exclusive() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();

        lvm.held.lock().unwrap().push(lv.clone());
        held(storage.activate_volume(&p, id, RAW).await.unwrap_err());
        assert!(!lvm.lv(&lv).unwrap().active);
        lvm.held.lock().unwrap().clear();
        lvm.taken();

        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate-ex vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("deactivate vg0/{lv}")]);
        assert!(!lvm.lv(&lv).unwrap().active);
    }

    /// 지우기: 다른 노드가 켜 두었으면 일시 오류, 없으면 `VolumeNotFound`, 없어도 만드는 명령이
    /// 아직 돌고 있으면 일시 오류다.
    #[tokio::test]
    async fn shared_delete_removes_the_lv() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        lvm.held.lock().unwrap().push(lv.clone());
        held(storage.delete_volume(&p, id, RAW).await.unwrap_err());
        assert_eq!(lvm.names(), std::slice::from_ref(&lv));
        lvm.held.lock().unwrap().clear();
        lvm.taken();

        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("remove vg0/{lv}")]);
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        *lvm.busy.lock().unwrap() = vec![lv];
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(lvm.taken().is_empty());
    }

    /// 크기는 늘리기 전 크기를 태그로 적고 늘린 뒤, 새 부분을 배타 활성화해 0으로 채우고 끄고 태그를
    /// 뗀다. 다른 노드가 켜 두었으면 일시 오류다. 정보의 사용량은 크기와 같고, 용량은 VG의 크기와 남은
    /// 공간이다. 스냅샷은 없다.
    #[tokio::test]
    async fn shared_resize_info_capacity_and_no_snapshots() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 1 << 20).await.unwrap();
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
        lvm.held.lock().unwrap().push(lv);
        held(
            storage
                .resize_volume(&p, id, RAW, 3 << 20)
                .await
                .unwrap_err(),
        );

        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 2 << 20,
                allocated_bytes: 2 << 20,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 10 << 30,
                available_bytes: 4 << 30,
            }
        );
        assert!(!storage.supports_snapshots(&p, RAW));
        let no = "lvm-shared pools have no snapshots";
        invalid(
            storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(
            storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(
            storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(storage.snapshot_tags(&p, id, RAW).await.unwrap_err(), no);
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("t"))
                .await
                .unwrap_err(),
            no,
        );
    }

    /// 늘린 부분을 0으로 채우지 못하면 오류이고 태그가 남는다. 다시 하면 LV가 이미 그 크기여도 늘리기
    /// 전 크기부터 다시 채운다. 이 노드에서 켜져 있으면(실행 중인 VM) 켠 채로 채운다.
    #[tokio::test]
    async fn shared_resize_zeroes_the_new_part_again_after_a_failure() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        lvm.taken();
        let err = storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.lv(&lv).unwrap().size, 2 << 20);
        assert!(!lvm.lv(&lv).unwrap().active);
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        lvm.fail.lock().unwrap().clear();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );

        storage.activate_volume(&p, id, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 3 << 20).await.unwrap();
        let tag = "opl-zero-from-2097152";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 3145728"),
                format!("zero vg0/{lv} 2097152"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.lv(&lv).unwrap().active);
    }

    /// 크기 늘리기가 늘린 부분을 0으로 다 채우지 못해 남긴 태그는 VM 시작(`activate_volume`)이 장치를
    /// 내주기 전에 마저 채우고 뗀다. 또 채우지 못하면 오류이고 켠 LV를 끈다. 다 채운 뒤에는 할 일이 없다.
    #[tokio::test]
    async fn shared_activation_finishes_a_pending_zero() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        lvm.taken();

        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        lvm.fail.lock().unwrap().clear();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.lv(&lv).unwrap().active);
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
    }

    /// 복제·복사 원본(`volume_source`)도 읽게 하기 전에 남은 0 채우기를 마친다. 채우지 못하면 켠 LV를
    /// 끈다.
    #[tokio::test]
    async fn shared_source_finishes_a_pending_zero() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        lvm.taken();

        storage.volume_source(&p, id, RAW, None).await.unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);

        lvm.fail.lock().unwrap().clear();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.size_bytes, 2 << 20);
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
    }

    /// 꺼진 볼륨을 늘리는 중에 같은 노드에서 VM 시작이나 복제 원본이 LV를 켜려 할 수 있다. 이 노드가
    /// 그 LV를 채우는 중이거나(크기 늘리기) 그 LV를 바꾸는 명령이 도는 중이면 켜기는 LV를 건드리지
    /// 않고 일시 오류다(reconciler가 잠시 뒤 다시 한다). 늘리기 전(태그만 붙은
    /// 때)에는 채울 것이 없으므로 태그를 그대로 둔다. 켜기가 남은 부분을 먼저 채웠으면 크기 늘리기는
    /// 다시 채우지 않는다(게스트가 쓴 데이터를 지우지 않는다).
    #[tokio::test]
    async fn shared_activation_during_a_resize() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        // 늘리기 전에 태그만 붙었다.
        lvm.tags
            .lock()
            .unwrap()
            .insert(lv.clone(), vec![tag.into()]);
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        // 늘린 뒤
        lvm.lvs.lock().unwrap().get_mut(&lv).unwrap().size = 2 << 20;
        *lvm.busy.lock().unwrap() = vec![lv.clone()];
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        lvm.busy.lock().unwrap().clear();
        let zeroing = format!("vg0/{lv}");
        storage.zeroing.lock().unwrap().insert(zeroing.clone());
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        storage.zeroing.lock().unwrap().remove(&zeroing);
        assert!(lvm.taken().is_empty());
        assert!(!lvm.lv(&lv).unwrap().active);

        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
    }

    /// 크기 늘리기는 [`Zeroing`]을 잡은 뒤 LV를 읽는다. 먼저 읽으면, 읽은 직후 같은 노드의 VM 시작이
    /// LV를 켜 QEMU에 내준 것을 모르고 채운 뒤 끈다. 표시를 먼저 잡으면 그 VM 시작은 일시 오류로
    /// 기다린다.
    #[tokio::test]
    async fn shared_resize_marks_the_lv_before_reading_it() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let storage = Arc::new(storage);
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        // LV를 읽은 직후 VM 시작이 끼어든다: 표시가 없으면 LV를 켜 QEMU에 내준다.
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, flag, name) = (storage.clone(), started.clone(), lv.clone());
        *lvm.on_list.lock().unwrap() = Some(Box::new(move |lvs| {
            if Zeroing::begin(&s.zeroing, "vg0", &name).is_ok() {
                lvs.get_mut(&name).unwrap().active = true;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
    }

    /// VM 시작(`activate_volume`)도 [`Zeroing`]을 잡은 뒤 LV를 읽고, 마치지 못하면 자기가 켠 LV만
    /// 끈다. 먼저 읽으면, 읽은 직후 같은 노드의 크기 늘리기가 표시를 잡고 LV를 켠 것을 모르고 일시
    /// 오류에 그 LV를 꺼 채우는 중인 크기 늘리기를 망친다.
    #[tokio::test]
    async fn shared_activation_marks_the_lv_before_reading_it() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let storage = Arc::new(storage);
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        // LV를 읽은 직후 크기 늘리기가 끼어든다: 표시가 비어 있으면 잡고 LV를 켠다.
        let resizing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, flag, name) = (storage.clone(), resizing.clone(), lv.clone());
        *lvm.on_list.lock().unwrap() = Some(Box::new(move |lvs| {
            if s.zeroing.lock().unwrap().insert(format!("vg0/{name}")) {
                lvs.get_mut(&name).unwrap().active = true;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert!(!resizing.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(lvm.taken(), [format!("activate-ex vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);
    }

    /// 가져오기: 원본의 가상 크기로 LV를 만들어 0으로 채운 뒤 켠 채로 써 넣고 끈다. 실패하면 지운다.
    #[tokio::test]
    async fn shared_imports_into_a_zeroed_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), false));
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&lv));
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 1048576"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let dev = path.to_str().unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "vmdk",
                "-O",
                "raw",
                "/up/disk.vmdk",
                dev,
            ]
        );

        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(lvm.taken().last().unwrap(), &format!("remove vg0/{lv}"));
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    /// 같은 VG 안 복사: 원본을 켜서 그 크기로 대상을 만들어(0 채움) 써 넣은 뒤 둘 다 끈다. 원본은
    /// `volume_source`로 켜고 `release_source`로 끈다. 다른 노드가 원본을 켜 두었으면 일시 오류다.
    #[tokio::test]
    async fn shared_copies_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), false));
        let p = shared_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (a, b) = (format!("opl-{src}"), format!("opl-{dst}"));
        storage.create_volume(&p, src, 2 << 20, RAW).await.unwrap();
        lvm.taken();

        let source = storage.volume_source(&p, src, RAW, None).await.unwrap();
        assert_eq!(source.path, lvm.node(&a));
        assert_eq!(source.size_bytes, 2 << 20);
        storage.release_source(&p, src, RAW, None).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{a}"),
                format!("deactivate vg0/{a}")
            ]
        );

        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{a}"),
                format!("create-linear vg0 {b} 2097152"),
                format!("activate-ex vg0/{b}"),
                format!("zero vg0/{b} 0"),
                format!("deactivate vg0/{b}"),
                format!("deactivate vg0/{a}"),
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to) = (lvm.node(&a), lvm.node(&b));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "raw",
                "-O",
                "raw",
                from.to_str().unwrap(),
                to.to_str().unwrap(),
            ]
        );

        lvm.held.lock().unwrap().push(a);
        let other = Uuid::now_v7();
        held(
            storage
                .copy_volume(&p, src, RAW, None, false, &p, other, RAW)
                .await
                .unwrap_err(),
        );
        assert!(!lvm.names().contains(&format!("opl-{other}")));
    }

    /// lvmlockd가 다른 노드의 잠금 때문에 거절하면(LV 또는 VG) 일시 오류다. 다른 LVM 오류는 그대로다.
    #[test]
    fn locks_held_by_other_hosts_are_transient() {
        let lv = held_elsewhere(DriverError::Lvm(
            "lvchange: LV locked by other host: san/opl-x".into(),
        ));
        assert!(matches!(lv, DriverError::Timeout(LV_HELD)), "{lv:?}");
        let vg = held_elsewhere(DriverError::Lvm(
            "lvcreate: VG san lock failed: held by other host.".into(),
        ));
        assert!(matches!(vg, DriverError::Timeout(VG_HELD)), "{vg:?}");
        assert!(vg.is_transient());
        let other = held_elsewhere(DriverError::Lvm("Volume group \"san\" not found".into()));
        assert!(matches!(other, DriverError::Lvm(_)), "{other:?}");
    }

    /// LVM 명령이 잠금 오류 앞에 경고 줄을 내도 잠금 오류로 알아본다.
    #[tokio::test]
    async fn lock_errors_after_warning_lines_are_transient() {
        let long = Duration::from_secs(10);
        for (stderr, want) in [
            (
                "WARNING: retrying ex lock on LV san/opl-x\n  LV locked by other host: san/opl-x",
                LV_HELD,
            ),
            (
                "Retrying ex lock on VG san\n  VG san lock failed: held by other host.",
                VG_HELD,
            ),
        ] {
            let script = format!("printf '{stderr}\\n' >&2; exit 5");
            let err = LVM
                .run_change("/bin/sh", &["-c", &script], &[], long)
                .await
                .unwrap_err();
            let err = held_elsewhere(err);
            assert!(
                matches!(err, DriverError::Timeout(m) if m == want),
                "{err:?}"
            );
        }
    }

    #[test]
    fn parses_lv_tags() {
        assert_eq!(
            parse_lv_tags(b"  opl-zero-from-1048576,other\n"),
            ["opl-zero-from-1048576", "other"]
        );
        assert!(parse_lv_tags(b"  \n").is_empty());
    }

    #[test]
    fn parses_vgs_json() {
        let out = br#"  {
      "report": [
          {
              "vg": [
                  {"vg_name":"san", "vg_lock_type":"sanlock", "vg_size":"10737418240", "vg_free":"4294967296"},
                  {"vg_name":"local", "vg_lock_type":"", "vg_size":4096, "vg_free":0}
              ]
          }
      ]
  }
"#;
        assert_eq!(
            parse_vgs(out).unwrap(),
            [
                VgInfo {
                    name: "san".into(),
                    lock_type: "sanlock".into(),
                    size_bytes: 10 << 30,
                    free_bytes: 4 << 30,
                },
                VgInfo {
                    name: "local".into(),
                    lock_type: String::new(),
                    size_bytes: 4096,
                    free_bytes: 0,
                },
            ]
        );
        for bad in [
            &b"not json"[..],
            br#"{"report":[{"vg":[{"vg_name":"x","vg_size":"big","vg_free":"1"}]}]}"#,
            br#"{"report":[{"vg":[{"vg_name":"x","vg_size":"1"}]}]}"#,
            br#"{"nothing":[]}"#,
        ] {
            let err = parse_vgs(bad).unwrap_err();
            assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
        }
    }
}
```

`crates/opl-driver/src/storage/storages.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, convert_from, path_str, rbd, run_qemu_img,
};
use crate::error::DriverError;

/// 볼륨을 다루는 방식이 같은 풀 백엔드의 묶음.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 볼륨 파일(`dir`, `nfs`)
    File,
    LvmThin,
    Zfs,
    Rbd,
    LvmShared,
}

impl Kind {
    fn of(pool: &StoragePool) -> Self {
        match &pool.spec.backend {
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => Kind::File,
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
            PoolBackend::Zfs { .. } => Kind::Zfs,
            PoolBackend::Rbd { .. } => Kind::Rbd,
            PoolBackend::LvmShared { .. } => Kind::LvmShared,
        }
    }
}

/// 저장소 호출을 풀 백엔드 종류에 맞는 저장소로 보낸다. 백엔드가 다른 풀 사이의 복사는 여기서
/// 한다(대상 백엔드에 빈 볼륨을 만들고 `qemu-img convert -n`으로 원본을 쓴다).
pub struct Storages {
    qemu_img: PathBuf,
    /// 파일 풀(`dir`, `nfs`)
    file: Arc<dyn Storage>,
    /// `lvm-thin` 풀. 없으면 그 풀은 쓸 수 없다.
    lvm_thin: Option<Arc<dyn Storage>>,
    /// `zfs` 풀. 없으면 그 풀은 쓸 수 없다.
    zfs: Option<Arc<dyn Storage>>,
    /// `rbd` 풀. 없으면 그 풀은 쓸 수 없다.
    rbd: Option<Arc<dyn Storage>>,
    /// `lvm-shared` 풀. 없으면 그 풀은 쓸 수 없다.
    lvm_shared: Option<Arc<dyn Storage>>,
}

impl Storages {
    pub fn new(qemu_img: impl Into<PathBuf>, file: Arc<dyn Storage>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            file,
            lvm_thin: None,
            zfs: None,
            rbd: None,
            lvm_shared: None,
        }
    }

    /// `lvm-thin` 풀을 다룰 저장소를 정한다.
    pub fn with_lvm_thin(mut self, storage: Arc<dyn Storage>) -> Self {
        self.lvm_thin = Some(storage);
        self
    }

    /// `zfs` 풀을 다룰 저장소를 정한다.
    pub fn with_zfs(mut self, storage: Arc<dyn Storage>) -> Self {
        self.zfs = Some(storage);
        self
    }

    /// `rbd` 풀을 다룰 저장소를 정한다.
    pub fn with_rbd(mut self, storage: Arc<dyn Storage>) -> Self {
        self.rbd = Some(storage);
        self
    }

    /// `lvm-shared` 풀을 다룰 저장소를 정한다.
    pub fn with_lvm_shared(mut self, storage: Arc<dyn Storage>) -> Self {
        self.lvm_shared = Some(storage);
        self
    }

    fn backend(&self, pool: &StoragePool) -> Result<&dyn Storage, DriverError> {
        self.shared(pool).map(|b| &**b)
    }

    /// [`Self::backend`]의 저장소. 뒤에서 쓰도록 복제할 수 있다.
    fn shared(&self, pool: &StoragePool) -> Result<&Arc<dyn Storage>, DriverError> {
        match Kind::of(pool) {
            Kind::File => Ok(&self.file),
            Kind::LvmThin => self.lvm_thin.as_ref().ok_or_else(|| {
                DriverError::InvalidSpec("lvm-thin pools are not supported yet".into())
            }),
            Kind::Zfs => self
                .zfs
                .as_ref()
                .ok_or_else(|| DriverError::InvalidSpec("zfs pools are not supported yet".into())),
            Kind::Rbd => self
                .rbd
                .as_ref()
                .ok_or_else(|| DriverError::InvalidSpec("rbd pools are not supported yet".into())),
            Kind::LvmShared => self.lvm_shared.as_ref().ok_or_else(|| {
                DriverError::InvalidSpec("lvm-shared pools are not supported yet".into())
            }),
        }
    }

    /// 대상 백엔드에 형식 `format`의 빈 볼륨을 만들고 `src`를 써 넣는다. 쓰다 실패하면 만든
    /// 볼륨을 지운다. 그사이 취소되면 볼륨이 남는다(호출자가 만든 볼륨으로 적어 두고 정리한다).
    /// 공유 LVM 볼륨은 만든 뒤 꺼져 있으므로 쓰는 동안만 켠다.
    #[allow(clippy::too_many_arguments)]
    async fn write_into(
        &self,
        src: &VolumeSource,
        force_share: bool,
        size_bytes: u64,
        dst: &dyn Storage,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let zero = dst.target_reads_zero(dst_pool).await?;
        let path = dst
            .create_volume(dst_pool, dst_id, size_bytes, format)
            .await?;
        let activate = Kind::of(dst_pool) == Kind::LvmShared;
        let written = async {
            if activate {
                dst.activate_volume(dst_pool, dst_id, format).await?;
            }
            // RBD 이미지는 경로가 없으므로 옵션 문자열로 준다(형식도 그 안에 있다).
            let image = rbd::pool_target(dst_pool, dst_id);
            let from = convert_from(src, force_share, image.is_none().then_some(format))?;
            // -n: 만들어 둔 볼륨(파일, 장치 또는 RBD 이미지)에 쓴다. 새 볼륨이 0으로 읽히면 0인
            // 부분은 쓰지 않는다(--target-is-zero).
            let mut args = vec!["convert", "-q", "-n"];
            if zero {
                args.push("--target-is-zero");
            }
            if image.is_some() {
                args.push("--target-image-opts");
            }
            args.extend(from.iter().map(String::as_str));
            let dst = match &image {
                Some(image) => rbd::image_opts(image, None),
                None => path_str(&path)?.to_string(),
            };
            args.push(&dst);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = dst.delete_volume(dst_pool, dst_id, format).await {
                tracing::warn!(volume = %dst_id, error = %del, "cannot remove partial copy");
            }
            return Err(e);
        }
        // 복사는 끝났다. 끄지 못하면 켜진 채 남을 뿐이다(opld가 뒤에 정리한다).
        if activate && let Err(e) = dst.deactivate_volume(dst_pool, dst_id, format).await {
            tracing::warn!(volume = %dst_id, error = %e, "cannot deactivate copied volume");
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for Storages {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.backend(pool)?
            .create_volume(pool, id, size_bytes, format)
            .await
    }

    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.backend(pool)?
            .import_volume(pool, id, src, src_format, format)
            .await
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        self.file.check_import_source(src, src_format).await
    }

    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.delete_volume(pool, id, format).await
    }

    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.activate_volume(pool, id, format).await
    }

    async fn deactivate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .deactivate_volume(pool, id, format)
            .await
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        self.backend(pool)?.volume_info(pool, id, format).await
    }

    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        self.backend(pool)?.capacity(pool).await
    }

    async fn target_reads_zero(&self, pool: &StoragePool) -> Result<bool, DriverError> {
        self.backend(pool)?.target_reads_zero(pool).await
    }

    /// 다룰 저장소가 없는 풀은 지원하지 않는다.
    fn supports_snapshots(&self, pool: &StoragePool, format: ImageFormat) -> bool {
        self.backend(pool)
            .is_ok_and(|b| b.supports_snapshots(pool, format))
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_create(pool, id, format, tag)
            .await
    }

    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_many(pool, ids, format, tag)
            .await
    }

    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_revert(pool, id, format, tag)
            .await
    }

    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_delete(pool, id, format, tag)
            .await
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        self.backend(pool)?.snapshot_tags(pool, id, format).await
    }

    async fn snapshot_tags_older_than(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        age: Duration,
    ) -> Result<Vec<String>, DriverError> {
        self.backend(pool)?
            .snapshot_tags_older_than(pool, id, format, age)
            .await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .resize_volume(pool, id, format, size_bytes)
            .await
    }

    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        self.backend(pool)?
            .volume_source(pool, id, format, tag)
            .await
    }

    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .release_source(pool, id, format, tag)
            .await
    }

    /// 같은 백엔드 사이면 그 저장소가 복사한다. 다르면 원본(`volume_source`)의 크기로 대상 볼륨을
    /// 만들고 `qemu-img convert -n`으로 원본을 써 넣은 뒤 원본을 내놓는다.
    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (src, dst) = (self.backend(src_pool)?, self.backend(dst_pool)?);
        if Kind::of(src_pool) == Kind::of(dst_pool) {
            return dst
                .copy_volume(
                    src_pool,
                    src_id,
                    format,
                    tag,
                    force_share,
                    dst_pool,
                    dst_id,
                    dst_format,
                )
                .await;
        }
        let source = src.volume_source(src_pool, src_id, format, tag).await?;
        // 읽는 동안 취소돼도 원본을 내놓는다. 대상은 원본(스냅샷이면 그때)의 크기로 만든다.
        let guard = Source::new(self.shared(src_pool)?, src_pool, src_id, format, tag);
        let size = source.size_bytes;
        let written = self
            .write_into(
                &source,
                force_share,
                size,
                dst,
                dst_pool,
                dst_id,
                dst_format,
            )
            .await;
        // 복사는 끝났으므로 내놓기가 실패해도 성공이다(활성화된 채 남을 뿐이다).
        guard.release().await;
        written
    }
}

/// [`Storage::volume_source`]로 읽을 수 있게 한 복사 원본. `release`로 내놓지 않고 버려지면(복사
/// future가 취소됨) 뒤에서 내놓는다.
struct Source {
    storage: Arc<dyn Storage>,
    pool: StoragePool,
    id: Uuid,
    format: ImageFormat,
    tag: Option<String>,
    released: bool,
}

impl Source {
    fn new(
        storage: &Arc<dyn Storage>,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Self {
        Self {
            storage: storage.clone(),
            pool: pool.clone(),
            id,
            format,
            tag: tag.map(str::to_string),
            released: false,
        }
    }

    /// 원본을 내놓는다. 복사는 끝났으므로 실패해도 로그만 남긴다(활성화된 채 남을 뿐이다).
    async fn release(mut self) {
        self.released = true;
        release(
            &*self.storage,
            &self.pool,
            self.id,
            self.format,
            self.tag.as_deref(),
        )
        .await;
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (storage, pool, tag) = (self.storage.clone(), self.pool.clone(), self.tag.take());
        let (id, format) = (self.id, self.format);
        rt.spawn(async move { release(&*storage, &pool, id, format, tag.as_deref()).await });
    }
}

async fn release(
    storage: &dyn Storage,
    pool: &StoragePool,
    id: Uuid,
    format: ImageFormat,
    tag: Option<&str>,
) {
    if let Err(e) = storage.release_source(pool, id, format, tag).await {
        tracing::warn!(volume = %id, error = %e, "cannot release copy source");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    type Log = Arc<Mutex<Vec<String>>>;

    /// 호출을 `{이름}.{메서드}`로 적는 저장소. 볼륨은 `{dir}/{id}`에 있다고 한다. 복사 원본의 크기는
    /// 2GiB이고(`volume_info`는 1GiB), 만든 볼륨의 크기를 `created`에 적는다.
    /// `fail`에 적은 메서드는 `ToolFailed`로 실패한다.
    struct Recorder {
        name: &'static str,
        dir: PathBuf,
        log: Log,
        fail: Vec<&'static str>,
        created: Mutex<Vec<u64>>,
        /// 새 볼륨이 0으로 읽히는지([`Storage::target_reads_zero`])
        reads_zero: bool,
    }

    impl Recorder {
        fn new(name: &'static str, dir: &Path, log: &Log) -> Self {
            Self {
                name,
                dir: dir.to_path_buf(),
                log: log.clone(),
                fail: vec![],
                created: Mutex::default(),
                reads_zero: true,
            }
        }

        fn failing(mut self, method: &'static str) -> Self {
            self.fail.push(method);
            self
        }

        fn call(&self, method: &'static str) -> Result<(), DriverError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.{method}", self.name));
            if self.fail.contains(&method) {
                return Err(DriverError::ToolFailed {
                    tool: "fake",
                    stderr: method.into(),
                });
            }
            Ok(())
        }
    }

    #[async_trait]
    impl Storage for Recorder {
        async fn create_volume(
            &self,
            _: &StoragePool,
            id: Uuid,
            size_bytes: u64,
            _: ImageFormat,
        ) -> Result<PathBuf, DriverError> {
            self.call("create_volume")?;
            self.created.lock().unwrap().push(size_bytes);
            Ok(self.dir.join(id.to_string()))
        }
        async fn import_volume(
            &self,
            _: &StoragePool,
            id: Uuid,
            _: &Path,
            _: SourceFormat,
            _: ImageFormat,
        ) -> Result<PathBuf, DriverError> {
            self.call("import_volume")?;
            Ok(self.dir.join(id.to_string()))
        }
        async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
            self.call("check_import_source")
        }
        async fn delete_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("delete_volume")
        }
        async fn activate_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("activate_volume")
        }
        async fn deactivate_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("deactivate_volume")
        }
        async fn volume_info(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<VolumeInfo, DriverError> {
            self.call("volume_info")?;
            Ok(VolumeInfo {
                virtual_bytes: 1 << 30,
                allocated_bytes: 0,
            })
        }
        async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
            self.call("capacity")?;
            Ok(Capacity {
                total_bytes: 0,
                available_bytes: 0,
            })
        }
        fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
            self.call("supports_snapshots").is_ok()
        }
        async fn target_reads_zero(&self, _: &StoragePool) -> Result<bool, DriverError> {
            Ok(self.reads_zero)
        }
        async fn snapshot_create(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_create")
        }
        async fn snapshot_many(
            &self,
            _: &StoragePool,
            _: &[Uuid],
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_many")
        }
        async fn snapshot_revert(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_revert")
        }
        async fn snapshot_delete(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_delete")
        }
        async fn snapshot_tags(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<Vec<String>, DriverError> {
            self.call("snapshot_tags")?;
            Ok(vec![])
        }
        async fn snapshot_tags_older_than(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: Duration,
        ) -> Result<Vec<String>, DriverError> {
            self.call("snapshot_tags_older_than")?;
            Ok(vec![])
        }
        async fn resize_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: u64,
        ) -> Result<(), DriverError> {
            self.call("resize_volume")
        }
        async fn volume_source(
            &self,
            pool: &StoragePool,
            id: Uuid,
            format: ImageFormat,
            tag: Option<&str>,
        ) -> Result<VolumeSource, DriverError> {
            self.call("volume_source")?;
            // RBD 풀이면 RBD 이미지다.
            let image = rbd::pool_target(pool, id);
            Ok(VolumeSource {
                path: match image {
                    Some(_) => PathBuf::new(),
                    None => self.dir.join(id.to_string()),
                },
                format,
                snapshot: tag.map(str::to_string),
                size_bytes: 2 << 30,
                rbd: image,
            })
        }
        async fn release_source(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: Option<&str>,
        ) -> Result<(), DriverError> {
            self.call("release_source")
        }
        async fn copy_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: Option<&str>,
            _: bool,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("copy_volume")
        }
    }

    fn pool(backend: PoolBackend) -> StoragePool {
        Resource::new(
            "p",
            StoragePoolSpec {
                backend,
                nodes: vec![],
                shared: false,
            },
        )
    }

    fn dir_pool() -> StoragePool {
        pool(PoolBackend::Dir {
            path: "/pools/local".into(),
        })
    }

    fn nfs_pool() -> StoragePool {
        pool(PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: None,
        })
    }

    fn lvm_pool() -> StoragePool {
        pool(PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        })
    }

    fn zfs_pool() -> StoragePool {
        pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        })
    }

    fn rbd_pool() -> StoragePool {
        pool(PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: Some("opl".into()),
            conf: None,
        })
    }

    fn lvm_shared_pool() -> StoragePool {
        pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        })
    }

    /// 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜 qemu-img. `fail`이면 실패한다.
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        crate::storage::tests::write_script(
            &path,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {args:?}\n{end}\n"),
        );
        path
    }

    fn taken(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    /// 파일 풀(dir, nfs)은 파일 저장소로, lvm-thin 풀은 LVM 저장소로, zfs 풀은 ZFS 저장소로, rbd
    /// 풀은 RBD 저장소로, lvm-shared 풀은 공유 LVM 저장소로 간다.
    #[tokio::test]
    async fn calls_go_to_the_pool_backend() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)))
        .with_rbd(Arc::new(Recorder::new("rbd", "".as_ref(), &log)))
        .with_lvm_shared(Arc::new(Recorder::new("shared", "/s".as_ref(), &log)));
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        for (p, name) in [
            (dir_pool(), "file"),
            (nfs_pool(), "file"),
            (lvm_pool(), "lvm"),
            (zfs_pool(), "zfs"),
            (rbd_pool(), "rbd"),
            (lvm_shared_pool(), "shared"),
        ] {
            storages.create_volume(&p, id, 1, f).await.unwrap();
            let src = Path::new("/up/x.vmdk");
            storages
                .import_volume(&p, id, src, SourceFormat::Vmdk, f)
                .await
                .unwrap();
            storages.delete_volume(&p, id, f).await.unwrap();
            storages.activate_volume(&p, id, f).await.unwrap();
            storages.deactivate_volume(&p, id, f).await.unwrap();
            storages.volume_info(&p, id, f).await.unwrap();
            storages.capacity(&p).await.unwrap();
            assert!(storages.supports_snapshots(&p, f));
            storages.snapshot_create(&p, id, f, "t").await.unwrap();
            storages.snapshot_many(&p, &[id], f, "t").await.unwrap();
            storages.snapshot_revert(&p, id, f, "t").await.unwrap();
            storages.snapshot_delete(&p, id, f, "t").await.unwrap();
            storages.snapshot_tags(&p, id, f).await.unwrap();
            storages
                .snapshot_tags_older_than(&p, id, f, Duration::from_secs(60))
                .await
                .unwrap();
            storages.resize_volume(&p, id, f, 2).await.unwrap();
            storages.volume_source(&p, id, f, None).await.unwrap();
            storages.release_source(&p, id, f, None).await.unwrap();
            // 같은 백엔드 사이 복사는 그 저장소가 한다.
            storages
                .copy_volume(&p, id, f, None, false, &p, Uuid::now_v7(), f)
                .await
                .unwrap();
            let methods = [
                "create_volume",
                "import_volume",
                "delete_volume",
                "activate_volume",
                "deactivate_volume",
                "volume_info",
                "capacity",
                "supports_snapshots",
                "snapshot_create",
                "snapshot_many",
                "snapshot_revert",
                "snapshot_delete",
                "snapshot_tags",
                "snapshot_tags_older_than",
                "resize_volume",
                "volume_source",
                "release_source",
                "copy_volume",
            ];
            let want: Vec<String> = methods.iter().map(|m| format!("{name}.{m}")).collect();
            assert_eq!(taken(&log), want);
        }
        // 올린 파일 검사는 풀과 상관없이 파일 저장소가 한다.
        storages
            .check_import_source(Path::new("/up/x.vmdk"), SourceFormat::Vmdk)
            .await
            .unwrap();
        assert_eq!(taken(&log), ["file.check_import_source"]);
        // dir과 nfs는 같은 백엔드다.
        storages
            .copy_volume(&dir_pool(), id, f, None, false, &nfs_pool(), id, f)
            .await
            .unwrap();
        assert_eq!(taken(&log), ["file.copy_volume"]);
    }

    /// LVM 저장소가 없으면 lvm-thin 풀은 쓸 수 없다.
    #[tokio::test]
    async fn lvm_thin_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        );
        let (p, id, f) = (lvm_pool(), Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("not supported"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.delete_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        unsupported(storages.volume_source(&p, id, f, None).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&dir_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// ZFS 저장소가 없으면 zfs 풀은 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn zfs_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let p = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "zfs pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&lvm_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// RBD 저장소가 없으면 rbd 풀은 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn rbd_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)));
        let p = pool(PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        });
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "rbd pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&zfs_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// 공유 LVM 저장소가 없으면 lvm-shared 풀은 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn lvm_shared_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)))
        .with_rbd(Arc::new(Recorder::new("rbd", "/r".as_ref(), &log)));
        let p = lvm_shared_pool();
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "lvm-shared pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.deactivate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&lvm_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// 공유 LVM 풀로 복사하면 만든 볼륨(꺼져 있다)을 쓰는 동안만 켠다. 쓰다 실패하면 지운다.
    #[tokio::test]
    async fn copy_into_lvm_shared_activates_the_new_volume_while_writing() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;
        let (from, to) = (dir_pool(), lvm_shared_pool());
        for (fail, want) in [
            (
                false,
                [
                    "file.volume_source",
                    "shared.create_volume",
                    "shared.activate_volume",
                    "shared.deactivate_volume",
                    "file.release_source",
                ],
            ),
            (
                true,
                [
                    "file.volume_source",
                    "shared.create_volume",
                    "shared.activate_volume",
                    "shared.delete_volume",
                    "file.release_source",
                ],
            ),
        ] {
            let storages = Storages::new(
                fake_qemu_img(tools.path(), fail),
                Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
            )
            .with_lvm_shared(Arc::new(Recorder::new(
                "shared",
                "/dev/san".as_ref(),
                &log,
            )));
            let copied = storages
                .copy_volume(&from, src, raw, None, false, &to, dst, raw)
                .await;
            assert_eq!(copied.is_err(), fail);
            assert_eq!(taken(&log), want);
        }
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        assert!(args.ends_with(&format!("/dev/san/{dst}\n")), "{args}");

        // 다른 백엔드로는 켜고 끄지 않는다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        storages
            .copy_volume(&from, src, raw, None, false, &lvm_pool(), dst, raw)
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "file.release_source"
            ]
        );
    }

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
    #[tokio::test]
    async fn copy_between_backends_converts_into_new_volume() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/dev/vg0".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);

        // 실행 중인 VM의 qcow2 스냅샷을 raw로 바꿔 읽는다. 태그의 쉼표는 옵션 문자열에서 `,,`로 쓴다.
        storages
            .copy_volume(
                &dir_pool(),
                src,
                qcow2,
                Some("a,b"),
                true,
                &lvm_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "file.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/f/{src}"), format!("/dev/vg0/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "raw",
                &src_path,
                &dst_path,
            ]
        );

        // 반대 방향, 지금 내용
        storages
            .copy_volume(&lvm_pool(), src, raw, None, false, &nfs_pool(), dst, raw)
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/dev/vg0/{src}"), format!("/f/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "raw",
                "-O",
                "raw",
                &src_path,
                &dst_path,
            ]
        );
    }

    /// LVM과 ZFS는 다른 백엔드다(원본을 읽어 대상에 새로 써 넣는다).
    #[tokio::test]
    async fn copy_between_lvm_and_zfs_converts_into_new_volume() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/dev/vg0".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new(
            "zfs",
            "/dev/zvol/tank/opl".as_ref(),
            &log,
        )));
        let (src, dst, raw) = (Uuid::now_v7(), Uuid::now_v7(), ImageFormat::Raw);
        storages
            .copy_volume(
                &lvm_pool(),
                src,
                raw,
                Some("t"),
                false,
                &zfs_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "zfs.create_volume",
                "lvm.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let dst_path = format!("/dev/zvol/tank/opl/{dst}");
        assert_eq!(args.lines().last(), Some(dst_path.as_str()));
    }

    /// RBD 이미지는 경로가 없으므로 `qemu-img`에 옵션 문자열로 준다: 대상은 `--target-image-opts`
    /// (`-O` 없이), 원본은 `--image-opts`(스냅샷도 그 안에)다.
    #[tokio::test]
    async fn copy_to_and_from_rbd_uses_image_opts() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_rbd(Arc::new(Recorder::new("rbd", "".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);

        storages
            .copy_volume(
                &dir_pool(),
                src,
                qcow2,
                Some("t"),
                true,
                &rbd_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "rbd.create_volume",
                "file.release_source",
            ]
        );
        let (src_path, to) = (
            format!("/f/{src}"),
            format!("driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{dst},file.user=opl"),
        );
        assert_eq!(
            std::fs::read_to_string(tools.path().join("args"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=t",
                &src_path,
                &to,
            ]
        );

        storages
            .copy_volume(
                &rbd_pool(),
                src,
                raw,
                Some("t"),
                false,
                &nfs_pool(),
                dst,
                qcow2,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "rbd.volume_source",
                "file.create_volume",
                "rbd.release_source",
            ]
        );
        let (from, dst_path) = (
            format!(
                "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{src},file.snapshot=t,file.user=opl"
            ),
            format!("/f/{dst}"),
        );
        assert_eq!(
            std::fs::read_to_string(tools.path().join("args"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--image-opts",
                "-O",
                "qcow2",
                &from,
                &dst_path,
            ]
        );
    }

    /// 써 넣다 실패하면 만든 볼륨을 지우고, 원본은 어느 경우에도 내놓는다.
    #[tokio::test]
    async fn failed_copy_between_backends_cleans_up() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;
        let (from, to) = (dir_pool(), lvm_pool());

        let storages = Storages::new(
            fake_qemu_img(tools.path(), true),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let err = storages
            .copy_volume(&from, src, raw, None, false, &to, dst, raw)
            .await
            .unwrap_err();
        match err {
            DriverError::ToolFailed { tool, stderr } => {
                assert_eq!((tool, stderr.as_str()), ("qemu-img", "broken"))
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "lvm.delete_volume",
                "file.release_source",
            ]
        );

        // 대상을 만들지 못하면(이미 있음 등) 지우지 않는다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(
            Recorder::new("lvm", "/l".as_ref(), &log).failing("create_volume"),
        ));
        storages
            .copy_volume(&from, src, raw, None, false, &to, dst, raw)
            .await
            .unwrap_err();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "file.release_source",
            ]
        );

        // 원본을 읽을 수 없으면 아무것도 만들지 않는다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log).failing("volume_source")),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        storages
            .copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw)
            .await
            .unwrap_err();
        assert_eq!(taken(&log), ["file.volume_source"]);

        // 복사가 끝났으면 원본을 내놓지 못해도 성공이다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(
            Recorder::new("lvm", "/l".as_ref(), &log).failing("release_source"),
        ));
        storages
            .copy_volume(&to, src, raw, None, false, &from, dst, raw)
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
    }

    /// 대상은 원본(`volume_source`, 스냅샷이면 그때)의 크기로 만든다. 볼륨을 되돌려 지금이 더 작아도
    /// 스냅샷의 내용이 다 담긴다.
    #[tokio::test]
    async fn copy_between_backends_uses_the_source_size() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let lvm = Arc::new(Recorder::new("lvm", "/l".as_ref(), &log));
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(lvm.clone());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, from, to) = (ImageFormat::Raw, dir_pool(), lvm_pool());
        storages
            .copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw)
            .await
            .unwrap();
        assert_eq!(*lvm.created.lock().unwrap(), [2 << 30]);
    }

    /// 복사가 취소돼도(future가 버려짐) 원본을 뒤에서 내놓는다.
    #[tokio::test]
    async fn cancelled_copy_between_backends_releases_the_source() {
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        crate::storage::tests::write_script(&slow, "#!/bin/sh\nsleep 5\n");
        let log = Log::default();
        let storages = Storages::new(slow, Arc::new(Recorder::new("file", "/f".as_ref(), &log)))
            .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let (from, to) = (lvm_pool(), dir_pool());
        let copy = storages.copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw);
        let res = tokio::time::timeout(std::time::Duration::from_millis(300), copy).await;
        assert!(res.is_err(), "copy should still be running");
        for _ in 0..200 {
            if log
                .lock()
                .unwrap()
                .iter()
                .any(|c| c == "lvm.release_source")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
    }

    /// 백엔드 사이 복사는 대상 백엔드가 새 볼륨이 0으로 읽힌다고 하지 않으면(0으로 채우지 않는 thin
    /// pool) `--target-is-zero`를 쓰지 않는다.
    #[tokio::test]
    async fn copy_between_backends_asks_the_target_whether_it_reads_zero() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let lvm = Recorder {
            reads_zero: false,
            ..Recorder::new("lvm", "/l".as_ref(), &log)
        };
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(lvm));
        assert!(storages.target_reads_zero(&dir_pool()).await.unwrap());
        assert!(!storages.target_reads_zero(&lvm_pool()).await.unwrap());
        let (src, dst, raw) = (Uuid::now_v7(), Uuid::now_v7(), ImageFormat::Raw);
        let source = VolumeSource {
            path: format!("/f/{src}").into(),
            format: raw,
            snapshot: None,
            size_bytes: 1 << 20,
            rbd: None,
        };
        let target = storages.backend(&lvm_pool()).unwrap();
        storages
            .write_into(&source, false, 1 << 20, target, &lvm_pool(), dst, raw)
            .await
            .unwrap();
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/f/{src}"), format!("/l/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert", "-q", "-n", "-f", "raw", "-O", "raw", &src_path, &dst_path
            ]
        );
    }
}
```

`crates/opld/src/member.rs`에서

```rust
use opl_driver::{
    DirStorage, Hypervisor, LvmThinStorage, QemuHypervisor, RbdStorage, Storage, Storages,
    SystemLvm, SystemRbd, SystemZfs, ZfsStorage,
};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage,
};
```

`crates/opld/src/member.rs`에서

```rust
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let storage: Arc<dyn Storage> = Arc::new(
```

을 다음으로 바꾼다:

```rust
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
```

`crates/opld/src/member.rs`에서

```rust
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd)),
        );
```

을 다음으로 바꾼다:

```rust
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 633개, 실패 0

Run: `cargo test -p opl-driver --lib storage::lvm`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): 공유 LVM 드라이버(두꺼운 LV, 0 채우기, 배타 활성화)"
```

---

### Task 3: 마운터의 공유 LVM 상태와 resolve

`MountOps::iscsi_login(portal, target)`(세션이 없으면 discovery 뒤 `--login`, 종료 코드 15는 이미 로그인, `-m session`의 21은 세션 없음, 뒤에 `udevadm settle`)·`lvm_lockstart(vg)`·`lvm_vg(vg)`. 로그인·잠금 시작은 죽이지 않는 뒤 작업으로 돌고(10초 안에 끝나지 않으면 "in progress"로 준비 안 됨, 풀을 잠시 못 써도 핸들을 지킨다), `vgs`의 잠금 종류가 `sanlock`·`dlm`이어야 준비. macOS는 `"lvm-shared pools are not supported on macOS"`. resolve는 Linux에서 활성화 뒤 `/dev/{vg}/opl-{id}`.

**Files:**
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opld/src/pools.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-reconciler/src/resolve.rs`에서

```rust
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. Linux가 아닌 노드에서 NFS 풀은
/// `NfsUnsupported`, LVM-thin·ZFS 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다(RBD 풀은 경로 대신 `RuntimeDisk.rbd`의 이미지).
```

을 다음으로 바꾼다:

```rust
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. Linux가 아닌 노드에서 NFS 풀은
/// `NfsUnsupported`, LVM-thin·ZFS·RBD·공유 LVM 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다(RBD 풀은 경로 대신 `RuntimeDisk.rbd`의 이미지).
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // 공유 LVM 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::LvmShared { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "lvm-shared pools are not supported yet",
            ));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS·RBD를 쓴다(Homebrew QEMU에는 rbd가 없다).
        if !cfg!(target_os = "linux") {
```

을 다음으로 바꾼다:

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM(공유 LVM 포함)·ZFS·RBD를 쓴다(Homebrew QEMU에는
        // rbd가 없다).
        if !cfg!(target_os = "linux") {
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// 공유 LVM 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지
/// 않는다.
#[tokio::test]
async fn refuses_lvm_shared_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let s: StoragePool = Resource::new(
```

을 다음으로 바꾼다:

```rust

/// 공유 LVM 풀 `s`(VG `san`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn lvm_shared_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let s: StoragePool = Resource::new(
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    vm.spec.disks.push(disk("root"));
    let storage = Activations::default();
```

을 다음으로 바꾼다:

```rust
    vm.spec.disks.push(disk("root"));
    (root, vm)
}

/// 공유 LVM을 지원하지 않는 노드(macOS)에서는 공유 LVM 풀의 디스크를 쓸 수 없다. 기다려도 풀리지
/// 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_lvm_shared_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = lvm_shared_vm(&cluster, node).await;
    let storage = Activations::default();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "s" && why == &"lvm-shared pools are not supported yet"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}
```

을 다음으로 바꾼다:

```rust
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "s" && why == &"lvm-shared pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 공유 LVM 볼륨을 (배타) 활성화한 뒤 LV 장치를 디스크로 쓴다. 활성화한 뒤에도 장치
/// 노드가 없으면 잠시 뒤 다시 시도할 `PoolUnavailable`이다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn activates_lvm_shared_volumes_and_waits_for_the_device() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = lvm_shared_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("s".to_string(), root.id, ImageFormat::Raw)]
    );
    let device = format!("/dev/san/opl-{}", root.id);
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "s" && e.contains(&device)),
        "{err:?}"
    );
}
```

`crates/opld/src/pools.rs` 전체:

```rust
//! 노드별 풀 상태. 이 노드가 쓸 수 있는 풀마다 준비 여부와 용량을 `NodeStatus.pools`에 적는다.
//! NFS 풀은 `{data_dir}/pools/{풀 id}`에 마운트하고, 이 노드가 쓸 수 없게 된 NFS 풀은 언마운트한다.
//! LVM-thin 풀은 `lvs`로 thin pool을, ZFS 풀은 `zfs get`으로 데이터셋을, RBD 풀은 `rbd ls`와
//! `ceph df`로 Ceph 풀을(그 전에 QEMU의 rbd 블록 드라이버를) 확인한다. 공유 LVM 풀은 iSCSI에
//! 로그인하고(있으면) VG 잠금을 시작한 뒤 `vgs`로 공유 VG를 확인한다.
//! opld가 내려가도 마운트는 그대로 둔다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{Capacity, LvInfo, RbdSpace, VgInfo, ZfsDataset};
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Kind;
use opl_types::storage::{IscsiTarget, PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
use opl_types::vm::{VmPhase, VmSpec};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// `mount`·`umount`를 기다리는 시간
pub const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);
/// 쓰기 확인과 용량 읽기(LVM-thin은 `lvs`, ZFS는 `zfs get`, RBD는 `rbd ls`와 `ceph df`)를 기다리는 시간. NFS 서버가 멈추면 이 작업이 멈춘다.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const NFS_UNSUPPORTED: &str = "nfs pools are not supported on macOS";
const LVM_UNSUPPORTED: &str = "lvm-thin pools are not supported on macOS";
const ZFS_UNSUPPORTED: &str = "zfs pools are not supported on macOS";
const RBD_UNSUPPORTED: &str = "rbd pools are not supported on macOS";
const LVM_SHARED_UNSUPPORTED: &str = "lvm-shared pools are not supported on macOS";
/// iSCSI 로그인이나 VG 잠금 시작이 확인 시간 안에 끝나지 않았다(처음 잠금 시작은 오래 걸린다).
const LVM_SHARED_STARTING: &str = "iscsi login or vg lock start in progress";
const QEMU_NO_RBD: &str = "qemu has no rbd block driver";
const LVS_TIMED_OUT: &str = "lvs timed out";
const ZFS_TIMED_OUT: &str = "zfs timed out";
const RBD_TIMED_OUT: &str = "rbd timed out";
const VGS_TIMED_OUT: &str = "vgs timed out";
/// `iscsiadm`의 종료 코드: 세션이 이미 있다(ISCSI_ERR_SESS_EXISTS).
const ISCSI_SESSION_EXISTS: i32 = 15;
/// `iscsiadm -m session`의 종료 코드: 세션이 하나도 없다(ISCSI_ERR_NO_OBJS_FOUND).
const ISCSI_NO_SESSIONS: i32 = 21;
const NFS_NOT_RESPONDING: &str = "nfs server not responding";
const DIR_NOT_RESPONDING: &str = "pool directory not responding";
const MOUNT_IN_PROGRESS: &str = "mount in progress";
/// 마운트에 실패한 뒤 다시 시도하기까지 처음 기다리는 시간. 실패할 때마다 두 배로 늘린다.
const MOUNT_BACKOFF_INITIAL: Duration = Duration::from_secs(15);
const MOUNT_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// 용량이 앞서 적은 값과 이만큼 바뀌어야 노드 상태를 다시 쓴다.
const GIB: u64 = 1 << 30;

/// 마운트와 풀 디렉터리 확인. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait MountOps: Send + Sync + 'static {
    /// `/proc/self/mountinfo`의 내용
    fn mountinfo(&self) -> std::io::Result<String>;
    /// `mount <args>`. 실패하면 stderr.
    async fn mount(&self, args: &[OsString]) -> Result<(), String>;
    /// `umount <dir>`. 실패하면 stderr.
    async fn umount(&self, dir: &Path) -> Result<(), String>;
    /// `dir`에 `probe` 파일을 만들고 지운 뒤 용량을 읽는다. 블로킹이고, 멈춘 NFS에서는 끝나지 않을 수 있다.
    fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity>;
    /// `lvs`로 읽은 VG `vg`의 LV `thinpool`. 실패하면 stderr.
    async fn lvm_pool(&self, vg: &str, thinpool: &str) -> Result<LvInfo, String>;
    /// `zfs get`으로 읽은 데이터셋 `dataset`. 실패하면 stderr.
    async fn zfs_dataset(&self, dataset: &str) -> Result<ZfsDataset, String>;
    /// Ceph 풀 `pool`(네임스페이스 `namespace`)에 `rbd ls`로 접근해 보고 `ceph df`로 읽은 공간.
    /// 클라이언트 id `user`, ceph.conf `conf`로 접속한다. 실패하면 stderr.
    async fn rbd_pool(
        &self,
        pool: &str,
        namespace: Option<&str>,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, String>;
    /// QEMU에 rbd 블록 드라이버가 있는지. 없으면 그 이유.
    async fn qemu_rbd(&self) -> Result<(), String>;
    /// 타깃 `target`의 세션이 없으면 포털 `portal`에서 찾아 로그인한다. 실패하면 stderr.
    async fn iscsi_login(&self, portal: &str, target: &str) -> Result<(), String>;
    /// VG `vg`의 잠금을 시작한다(`vgchange --lockstart`). 실패하면 stderr.
    async fn lvm_lockstart(&self, vg: &str) -> Result<(), String>;
    /// `vgs`로 읽은 VG `vg`. 실패하면 stderr.
    async fn lvm_vg(&self, vg: &str) -> Result<VgInfo, String>;
}

/// 실제 명령과 파일시스템
pub struct SystemMount {
    /// VM을 실행하는 QEMU(rbd 블록 드라이버를 확인한다)
    pub qemu_binary: PathBuf,
}

#[async_trait]
impl MountOps for SystemMount {
    fn mountinfo(&self) -> std::io::Result<String> {
        std::fs::read_to_string(opl_driver::mounts::MOUNTINFO)
    }

    async fn mount(&self, args: &[OsString]) -> Result<(), String> {
        run("mount", args).await
    }

    async fn umount(&self, dir: &Path) -> Result<(), String> {
        run("umount", &[dir.as_os_str().to_owned()]).await
    }

    fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity> {
        let file = dir.join(probe);
        std::fs::write(&file, b"opl")?;
        std::fs::remove_file(&file)?;
        let st = rustix::fs::statvfs(dir)?;
        Ok(Capacity {
            total_bytes: st.f_blocks * st.f_frsize,
            available_bytes: st.f_bavail * st.f_frsize,
        })
    }

    async fn lvm_pool(&self, vg: &str, thinpool: &str) -> Result<LvInfo, String> {
        let out = tokio::process::Command::new("lvs")
            .args(["--reportformat", "json", "--units", "b", "--nosuffix", "-o"])
            .arg("lv_name,lv_attr,lv_size,data_percent")
            .arg(format!("{vg}/{thinpool}"))
            // 숫자를 `.`로 쓰고, 물려받은 파일 기술자 경고를 stderr에 섞지 않는다.
            .env("LC_ALL", "C")
            .env("LVM_SUPPRESS_FD_WARNINGS", "1")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run lvs: {e}"))?;
        if !out.status.success() {
            return Err(failure("lvs", &out));
        }
        opl_driver::parse_lvs(&out.stdout)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|lv| lv.name == thinpool)
            .ok_or_else(|| format!("{vg}/{thinpool} not found"))
    }

    async fn zfs_dataset(&self, dataset: &str) -> Result<ZfsDataset, String> {
        let out = tokio::process::Command::new("zfs")
            .args(["get", "-Hp", "-o", "value", "available,used,type", dataset])
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run zfs: {e}"))?;
        if !out.status.success() {
            return Err(failure("zfs", &out));
        }
        opl_driver::parse_zfs_dataset(&out.stdout).map_err(|e| e.to_string())
    }

    async fn rbd_pool(
        &self,
        pool: &str,
        namespace: Option<&str>,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, String> {
        let mut ls = vec!["ls", "--format", "json", "--pool", pool];
        if let Some(ns) = namespace {
            ls.extend(["--namespace", ns]);
        }
        ceph("rbd", user, conf, &ls).await?;
        let df = ceph("ceph", user, conf, &["df", "detail", "--format", "json"]).await?;
        opl_driver::parse_ceph_df(&df, pool).map_err(|e| e.to_string())
    }

    /// 빠진 인자(풀)로 rbd 드라이브를 열어 본다. 드라이버가 있으면 인자 오류로, 없으면(Homebrew
    /// QEMU, Ubuntu에서 `qemu-block-extra`가 없음) `Unknown driver 'rbd'`로 곧바로 끝난다
    /// ([`qemu_rbd_result`]). `-blockdev`는 드라이버를 찾기 전에 인자를 검사하므로 `-drive`를 쓴다.
    async fn qemu_rbd(&self) -> Result<(), String> {
        let out = tokio::process::Command::new(&self.qemu_binary)
            .args(["-M", "none", "-nodefaults", "-display", "none", "-S"])
            .args(["-drive", RBD_PROBE_DRIVE])
            .env("LC_ALL", "C")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", self.qemu_binary.display()))?;
        qemu_rbd_result(&String::from_utf8_lossy(&out.stderr))
    }

    /// 세션이 있으면 그대로 둔다. 없으면 sendtargets로 찾은 뒤 로그인하고, 장치 노드가 생길 때까지
    /// 기다린다. 그 사이 다른 쪽이 로그인했으면(세션이 이미 있음) 성공이다. 시간 제한으로 죽이지
    /// 않는다(iscsiadm이 스스로 시간 제한을 둔다).
    async fn iscsi_login(&self, portal: &str, target: &str) -> Result<(), String> {
        let out = run_to_end("iscsiadm", &["-m", "session"]).await?;
        if out.status.code() != Some(ISCSI_NO_SESSIONS) {
            if !out.status.success() {
                return Err(failure("iscsiadm", &out));
            }
            if iscsi_session_present(&String::from_utf8_lossy(&out.stdout), target) {
                return Ok(());
            }
        }
        let discovery = ["-m", "discovery", "-t", "sendtargets", "-p", portal];
        let out = run_to_end("iscsiadm", &discovery).await?;
        if !out.status.success() {
            return Err(failure("iscsiadm", &out));
        }
        let login = ["-m", "node", "-T", target, "-p", portal, "--login"];
        let out = run_to_end("iscsiadm", &login).await?;
        if !out.status.success() && out.status.code() != Some(ISCSI_SESSION_EXISTS) {
            return Err(failure("iscsiadm", &out));
        }
        let out = run_to_end("udevadm", &["settle"]).await?;
        if !out.status.success() {
            return Err(failure("udevadm", &out));
        }
        Ok(())
    }

    /// 처음에는 sanlock 임대 때문에 오래 걸릴 수 있다. 이미 시작했으면 곧바로 끝난다. 시간 제한으로
    /// 죽이지 않는다.
    async fn lvm_lockstart(&self, vg: &str) -> Result<(), String> {
        let out = run_to_end("vgchange", &["--lockstart", vg]).await?;
        if !out.status.success() {
            return Err(failure("vgchange", &out));
        }
        Ok(())
    }

    async fn lvm_vg(&self, vg: &str) -> Result<VgInfo, String> {
        let out = tokio::process::Command::new("vgs")
            .args(["--reportformat", "json", "--units", "b", "--nosuffix", "-o"])
            .arg("vg_name,vg_lock_type,vg_size,vg_free")
            .arg(vg)
            .env("LC_ALL", "C")
            .env("LVM_SUPPRESS_FD_WARNINGS", "1")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run vgs: {e}"))?;
        if !out.status.success() {
            return Err(failure("vgs", &out));
        }
        opl_driver::parse_vgs(&out.stdout)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|v| v.name == vg)
            .ok_or_else(|| format!("volume group {vg} not found"))
    }
}

/// `iscsiadm -m session`의 출력(`tcp: [1] 10.0.0.9:3260,1 iqn.… (non-flash)`)에 타깃 `target`의
/// 세션이 있는지. 포털은 보지 않는다(호스트 이름 포털도 세션에는 주소로 보인다).
fn iscsi_session_present(sessions: &str, target: &str) -> bool {
    sessions
        .lines()
        .any(|l| l.split_whitespace().nth(3) == Some(target))
}

/// 명령을 끝까지 실행해 결과를 돌려준다. 바꾸는 명령이므로 버려져도 죽이지 않는다.
async fn run_to_end(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    tokio::process::Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("LVM_SUPPRESS_FD_WARNINGS", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("cannot run {program}: {e}"))
}

/// QEMU rbd 드라이버 확인에 여는 드라이브(풀·이미지가 빠졌다)
const RBD_PROBE_DRIVE: &str = "driver=rbd,if=none";

/// QEMU rbd 드라이버 확인의 stderr를 읽는다. `Unknown driver 'rbd'`면 드라이버가 없다. rbd 드라이버가
/// 빠진 인자를 알렸으면(QEMU 8.2: `Parameter 'pool' is missing`) 있다. 그 밖의 결과는 확인하지 못한
/// 것이다(있다고 기억하지 않으므로 다음 새로 고침에서 다시 확인한다).
fn qemu_rbd_result(stderr: &str) -> Result<(), String> {
    if stderr.contains("Unknown driver 'rbd'") {
        return Err(QEMU_NO_RBD.into());
    }
    if ["pool", "image"]
        .iter()
        .any(|p| stderr.contains(&format!("Parameter '{p}' is missing")))
    {
        return Ok(());
    }
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no error from qemu");
    Err(format!("cannot check the qemu rbd block driver: {line}"))
}

/// Ceph 명령 `program --id user [--conf conf] args`를 실행해 stdout을 돌려준다. 실패하면 stderr.
async fn ceph(
    program: &str,
    user: &str,
    conf: Option<&str>,
    args: &[&str],
) -> Result<Vec<u8>, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(["--id", user]);
    if let Some(conf) = conf {
        cmd.args(["--conf", conf]);
    }
    let out = cmd
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if !out.status.success() {
        return Err(failure(program, &out));
    }
    Ok(out.stdout)
}

/// 명령을 실행한다. 시간 제한으로 버려지면 프로세스를 죽인다.
async fn run(program: &str, args: &[OsString]) -> Result<(), String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(failure(program, &out))
}

/// 실패한 명령의 stderr 첫 줄. 비어 있으면 종료 상태.
fn failure(program: &str, out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(
            || format!("{program} failed: {}", out.status),
            str::to_string,
        )
}

/// `mount -t nfs -o nosuid,nodev,noexec,retry=0[,options] server:export dir`의 인자. 풀에는 이미지
/// 파일만 두므로 setuid·장치 파일·실행을 늘 막는다. `retry=0`은 시간 제한으로 `mount`를 끝낸 뒤
/// mount.nfs가 앞에서 계속 다시 시도하지 않게 한다(다시 시도는 마운터가 한다). 사용자 옵션은 그
/// 뒤에 붙어 `retry`도 바꿀 수 있다. IPv6 서버는 `[...]`로 감싼다.
pub fn mount_args(server: &str, export: &str, options: Option<&str>, dir: &Path) -> Vec<OsString> {
    let source = if server.contains(':') {
        format!("[{server}]:{export}")
    } else {
        format!("{server}:{export}")
    };
    let mut opts = String::from("nosuid,nodev,noexec,retry=0");
    if let Some(options) = options {
        opts.push(',');
        opts.push_str(options);
    }
    vec![
        "-t".into(),
        "nfs".into(),
        "-o".into(),
        opts.into(),
        source.into(),
        dir.as_os_str().to_owned(),
    ]
}

/// 풀 상태를 주기적으로 새로 고치는 작업
pub struct PoolMounter {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl PoolMounter {
    pub fn spawn(
        cluster: Arc<Cluster>,
        mount_root: PathBuf,
        qemu_binary: PathBuf,
        refresh: Duration,
    ) -> Self {
        let pools = Pools::new(cluster, mount_root, Arc::new(SystemMount { qemu_binary }));
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_pools(pools, refresh, rx)),
        }
    }

    /// 멈춘다. 마운트는 그대로 둔다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_pools(mut pools: Pools, refresh: Duration, mut shutdown: oneshot::Receiver<()>) {
    let done = pools.done.clone();
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        // 풀이 바뀌면(만들기·지우기) 곧바로 새로 고친다. 배치와 볼륨 Task는 준비된 노드만 고른다.
        tokio::select! {
            _ = &mut shutdown => return,
            // 마운트·언마운트가 끝나면 곧바로 결과를 적는다.
            () = done.notified() => {}
            _ = interval.tick() => {}
            () = pool_changed(&mut watch) => {}
        }
        // 이 새로 고침이 읽을 목록 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = pools
                .cluster
                .list::<StoragePoolSpec>()
                .ok()
                .and_then(|(revision, _)| pools.cluster.watch(revision).ok());
        }
        // 마운트·기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            r = pools.tick() => if let Err(e) = r {
                tracing::warn!(error = format!("{e:#}"), "cannot refresh storage pool states");
            }
        }
    }
}

/// 풀 리소스가 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 새로
/// 고친 뒤 다시 구독하게 한다. 구독이 없으면 기다리기만 한다.
async fn pool_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::StoragePool,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::StoragePool,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

/// 한 번의 새로 고침에 필요한 것
struct Pools {
    cluster: Arc<Cluster>,
    node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(`{data_dir}/pools`)
    mount_root: PathBuf,
    ops: Arc<dyn MountOps>,
    /// 이 OS가 NFS 마운트를 지원하는지(Linux)
    nfs_supported: bool,
    /// 이 OS가 LVM을 지원하는지(Linux)
    lvm_supported: bool,
    /// 이 OS가 ZFS를 지원하는지(Linux)
    zfs_supported: bool,
    /// 이 OS가 RBD를 지원하는지(Linux)
    rbd_supported: bool,
    /// QEMU에 rbd 블록 드라이버가 있음을 확인했는지. 확인한 뒤에는 다시 확인하지 않는다.
    qemu_rbd: Arc<AtomicBool>,
    mount_timeout: Duration,
    check_timeout: Duration,
    /// 시간 안에 끝나지 않은 확인 작업(풀 id별). 끝날 때까지 그 풀은 건너뛴다.
    stuck: HashMap<Uuid, JoinHandle<std::io::Result<Capacity>>>,
    /// 시간 안에 끝나지 않은 공유 LVM 풀 확인(풀 id별). 끝날 때까지 새로 시작하지 않는다.
    lvm_shared_checks: HashMap<Uuid, JoinHandle<PoolState>>,
    /// 진행 중인 마운트(풀 id)와 언마운트(마운트 지점). 풀마다 따로 돌고, 끝나기 전에는 같은 풀의
    /// 마운트·언마운트를 새로 시작하지 않는다.
    mounting: HashSet<Uuid>,
    unmounting: HashSet<PathBuf>,
    /// 끝난 마운트·언마운트의 결과. 작업이 넣고 `done`으로 알린다.
    finished: Arc<Mutex<Vec<Finished>>>,
    /// 마운트·언마운트가 끝났음을 알린다. 새로 고침이 주기를 기다리지 않고 결과를 적는다.
    done: Arc<Notify>,
    /// 마운트에 실패한 풀의 다음 시도 시각(풀 id별)
    backoff: HashMap<Uuid, Backoff>,
    /// 마지막 언마운트 실패(같은 실패를 tick마다 기록하지 않는다)
    umount_errors: HashMap<PathBuf, String>,
}

/// 끝난 마운트·언마운트 작업
enum Finished {
    Mount(Uuid, Result<(), String>),
    Unmount(PathBuf, Result<(), String>),
}

/// 마운트 실패 뒤 다음 시도를 미룬다. 그동안 그 풀의 상태는 마지막 오류다.
struct Backoff {
    until: Instant,
    delay: Duration,
    error: String,
}

/// 풀 하나를 이번 새로 고침에서 어떻게 할지
enum Step {
    /// 상태가 정해졌다(마운트 실패, 마운트 중 등).
    State(PoolState),
    /// 이 디렉터리를 확인한다. 멈추면 이 오류를 적는다.
    Check(PathBuf, &'static str),
    /// VG의 thin pool을 `lvs`로 확인한다.
    Lvm(String, String),
    /// ZFS 데이터셋을 `zfs get`으로 확인한다.
    Zfs(String),
    /// Ceph 풀(네임스페이스)을 `rbd ls`와 `ceph df`로 확인한다. 사용자는 기본값을 채웠다.
    Rbd {
        pool: String,
        namespace: Option<String>,
        user: String,
        conf: Option<String>,
    },
    /// iSCSI에 로그인하고(있으면) VG 잠금을 시작한 뒤 공유 VG를 `vgs`로 확인한다.
    LvmShared(String, Option<IscsiTarget>),
}

/// 마운트에 실패한 뒤 다음 시도까지 기다리는 시간: 15초부터 실패할 때마다 두 배, 최대 5분
fn next_backoff(prev: Option<Duration>) -> Duration {
    prev.map_or(MOUNT_BACKOFF_INITIAL, |d| (d * 2).min(MOUNT_BACKOFF_MAX))
}

impl Pools {
    fn new(cluster: Arc<Cluster>, mount_root: PathBuf, ops: Arc<dyn MountOps>) -> Self {
        let NodeId(node) = cluster.id();
        Self {
            cluster,
            node,
            mount_root,
            ops,
            nfs_supported: cfg!(target_os = "linux"),
            lvm_supported: cfg!(target_os = "linux"),
            zfs_supported: cfg!(target_os = "linux"),
            rbd_supported: cfg!(target_os = "linux"),
            qemu_rbd: Arc::default(),
            mount_timeout: MOUNT_TIMEOUT,
            check_timeout: CHECK_TIMEOUT,
            stuck: HashMap::new(),
            lvm_shared_checks: HashMap::new(),
            mounting: HashSet::new(),
            unmounting: HashSet::new(),
            finished: Arc::default(),
            done: Arc::default(),
            backoff: HashMap::new(),
            umount_errors: HashMap::new(),
        }
    }

    /// 풀 상태를 새로 고친다. 마운트·언마운트는 기다리지 않고(끝나면 `done`이 알린다), 확인은 풀마다
    /// 동시에 해 한 풀이 다른 풀의 상태 기록을 늦추지 않는다.
    async fn tick(&mut self) -> anyhow::Result<()> {
        let Some(me) = self.cluster.get::<NodeSpec>(self.node)? else {
            // 아직 Node 리소스를 기록하지 않았다.
            return Ok(());
        };
        let (_, pools) = self.cluster.list::<StoragePoolSpec>()?;
        let usable: Vec<&StoragePool> = pools
            .iter()
            .filter(|p| p.spec.nodes.is_empty() || p.spec.nodes.contains(&self.node))
            .collect();
        // mountinfo는 실제 경로를 적으므로 마운트 지점도 실제 경로로 비교한다.
        std::fs::create_dir_all(&self.mount_root)?;
        self.mount_root = std::fs::canonicalize(&self.mount_root)?;
        // 끝난 마운트를 먼저 거둔다. 그 뒤에 읽는 mountinfo에 성공한 마운트가 보인다.
        let mut mount_results = self.collect_finished();
        let mounted = if self.nfs_supported {
            opl_driver::mounts::nfs_mount_points(&self.ops.mountinfo()?)
        } else {
            BTreeSet::new()
        };
        self.backoff
            .retain(|id, _| usable.iter().any(|p| p.id == *id));
        // 쓸 수 없게 된 풀의 확인도 끝날 때까지 둔다(다시 쓸 수 있게 되면 그것을 기다린다).
        self.lvm_shared_checks
            .retain(|id, task| !task.is_finished() || usable.iter().any(|p| p.id == *id));
        let deadline = Instant::now() + self.check_timeout;
        let mut states = BTreeMap::new();
        let mut checks = Vec::new();
        let mut block_checks = Vec::new();
        let mut lvm_shared_checks = Vec::new();
        for pool in &usable {
            match self.step(pool, &mounted, mount_results.remove(&pool.id)) {
                Step::State(state) => {
                    states.insert(pool.name.clone(), state);
                }
                Step::Check(dir, not_responding) => checks.push((*pool, dir, not_responding)),
                Step::Lvm(vg, thinpool) => {
                    block_checks.push((*pool, self.spawn_lvm_check(pool.id, vg, thinpool)));
                }
                Step::Zfs(dataset) => {
                    block_checks.push((*pool, self.spawn_zfs_check(pool.id, dataset)));
                }
                Step::Rbd {
                    pool: ceph_pool,
                    namespace,
                    user,
                    conf,
                } => {
                    let task = self.spawn_rbd_check(pool.id, ceph_pool, namespace, user, conf);
                    block_checks.push((*pool, task));
                }
                Step::LvmShared(vg, iscsi) => {
                    let task = self.lvm_shared_check(pool.id, vg, iscsi);
                    lvm_shared_checks.push((*pool, task));
                }
            }
        }
        for (pool, state) in self.check_all(checks).await {
            let state = match state {
                Some(state) => state,
                // 앞 확인이 아직 멈춰 있다. 같은 풀(id)의 지난 상태를 그대로 둔다.
                None => me
                    .status
                    .pools
                    .get(&pool.name)
                    .filter(|s| s.pool_id == pool.id)
                    .cloned()
                    .unwrap_or_else(|| not_ready(pool.id, NFS_NOT_RESPONDING.into())),
            };
            states.insert(pool.name.clone(), state);
        }
        for (pool, task) in block_checks {
            let state = task
                .await
                .unwrap_or_else(|e| not_ready(pool.id, e.to_string()));
            states.insert(pool.name.clone(), state);
        }
        // 공유 LVM 확인은 죽이지 않는다. 시간 안에 끝나지 않으면 두고 다음에 결과를 거둔다.
        for (pool, mut task) in lvm_shared_checks {
            let state = match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(state) => state.unwrap_or_else(|e| not_ready(pool.id, e.to_string())),
                Err(_) => {
                    self.lvm_shared_checks.insert(pool.id, task);
                    not_ready(pool.id, LVM_SHARED_STARTING.into())
                }
            };
            states.insert(pool.name.clone(), state);
        }
        if self.nfs_supported {
            self.unmount_unused(&pools, &usable, &mounted)?;
        }
        for (name, state) in &states {
            if me.status.pools.get(name).map(|s| (s.ready, &s.error))
                != Some((state.ready, &state.error))
            {
                match &state.error {
                    None => tracing::info!(pool = %name, "storage pool is ready"),
                    Some(error) => {
                        tracing::warn!(pool = %name, %error, "storage pool is not ready")
                    }
                }
            }
        }
        self.write(states).await
    }

    /// 풀 하나를 이번에 어떻게 할지 정한다. 마운트되지 않은 NFS 풀은 마운트를 시작한다.
    /// `mount_result`는 이번에 거둔 이 풀의 마운트 결과다.
    fn step(
        &mut self,
        pool: &StoragePool,
        mounted: &BTreeSet<PathBuf>,
        mount_result: Option<Result<(), String>>,
    ) -> Step {
        let (server, export, options) = match &pool.spec.backend {
            PoolBackend::Dir { path } => return Step::Check(path.clone(), DIR_NOT_RESPONDING),
            PoolBackend::LvmThin { vg, thinpool } => {
                if !self.lvm_supported {
                    return Step::State(not_ready(pool.id, LVM_UNSUPPORTED.into()));
                }
                return Step::Lvm(vg.clone(), thinpool.clone());
            }
            PoolBackend::Zfs { dataset } => {
                if !self.zfs_supported {
                    return Step::State(not_ready(pool.id, ZFS_UNSUPPORTED.into()));
                }
                return Step::Zfs(dataset.clone());
            }
            PoolBackend::Rbd {
                pool: ceph_pool,
                namespace,
                user,
                conf,
            } => {
                if !self.rbd_supported {
                    return Step::State(not_ready(pool.id, RBD_UNSUPPORTED.into()));
                }
                return Step::Rbd {
                    pool: ceph_pool.clone(),
                    namespace: namespace.clone(),
                    user: user
                        .clone()
                        .unwrap_or_else(|| opl_driver::RBD_DEFAULT_USER.into()),
                    conf: conf.clone(),
                };
            }
            PoolBackend::LvmShared { vg, iscsi } => {
                if !self.lvm_supported {
                    return Step::State(not_ready(pool.id, LVM_SHARED_UNSUPPORTED.into()));
                }
                return Step::LvmShared(vg.clone(), iscsi.clone());
            }
            PoolBackend::Nfs {
                server,
                export,
                options,
            } => (server, export, options),
        };
        if !self.nfs_supported {
            return Step::State(not_ready(pool.id, NFS_UNSUPPORTED.into()));
        }
        let dir = self.mount_root.join(pool.id.to_string());
        // 언마운트 중이면 mountinfo에 아직 보여도 곧 풀린다. 끝난 뒤에 다시 마운트한다.
        if self.unmounting.contains(&dir) {
            return Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()));
        }
        if mounted.contains(&dir) {
            self.backoff.remove(&pool.id);
            return Step::Check(dir, NFS_NOT_RESPONDING);
        }
        if let Some(result) = mount_result {
            // 마운트가 실패했다(성공했다면 mountinfo에 보였다). 다음 시도를 미룬다.
            let error = result
                .err()
                .unwrap_or_else(|| format!("{} is not mounted after mount", dir.display()));
            let delay = next_backoff(self.backoff.get(&pool.id).map(|b| b.delay));
            tracing::debug!(pool = %pool.name, %error, ?delay, "nfs mount failed; retrying later");
            let backoff = Backoff {
                until: Instant::now() + delay,
                delay,
                error: error.clone(),
            };
            self.backoff.insert(pool.id, backoff);
            return Step::State(not_ready(pool.id, error));
        }
        if self.mounting.contains(&pool.id) {
            return Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()));
        }
        if let Some(backoff) = self.backoff.get(&pool.id)
            && Instant::now() < backoff.until
        {
            return Step::State(not_ready(pool.id, backoff.error.clone()));
        }
        // 마운트되지 않은 지점만 건드린다(멈춘 NFS 마운트 지점은 stat도 멈춘다).
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Step::State(not_ready(
                pool.id,
                format!("cannot create {}: {e}", dir.display()),
            ));
        }
        let args = mount_args(server, export, options.as_deref(), &dir);
        self.mounting.insert(pool.id);
        let (ops, finished, done) = (self.ops.clone(), self.finished.clone(), self.done.clone());
        let (id, name, timeout) = (pool.id, pool.name.clone(), self.mount_timeout);
        tokio::spawn(async move {
            let result = tokio::time::timeout(timeout, ops.mount(&args))
                .await
                .unwrap_or_else(|_| Err(format!("mount timed out after {}s", timeout.as_secs())));
            if result.is_ok() {
                tracing::info!(pool = %name, dir = %dir.display(), "mounted nfs pool");
            }
            finished.lock().unwrap().push(Finished::Mount(id, result));
            done.notify_one();
        });
        Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()))
    }

    /// `lvs`로 thin pool을 확인하는 작업을 시작한다. 시간 제한이 지나면 `lvs`를 죽인다.
    fn spawn_lvm_check(&self, id: Uuid, vg: String, thinpool: String) -> JoinHandle<PoolState> {
        let (ops, timeout) = (self.ops.clone(), self.check_timeout);
        tokio::spawn(async move {
            match tokio::time::timeout(timeout, ops.lvm_pool(&vg, &thinpool)).await {
                Ok(Ok(lv)) if lv.is_thin_pool() => PoolState {
                    pool_id: id,
                    ready: true,
                    error: None,
                    total_bytes: lv.size_bytes,
                    available_bytes: lv.size_bytes - lv.used_bytes(),
                },
                Ok(Ok(_)) => not_ready(id, format!("{vg}/{thinpool} is not a thin pool")),
                Ok(Err(e)) => not_ready(id, e),
                Err(_) => not_ready(id, LVS_TIMED_OUT.into()),
            }
        })
    }

    /// `zfs get`으로 데이터셋을 확인하는 작업을 시작한다. zvol이 아닌 데이터셋이면 준비됐고, 용량은
    /// `available`+`used`와 `available`이다. 시간 제한이 지나면 `zfs`를 죽인다.
    fn spawn_zfs_check(&self, id: Uuid, dataset: String) -> JoinHandle<PoolState> {
        let (ops, timeout) = (self.ops.clone(), self.check_timeout);
        tokio::spawn(async move {
            match tokio::time::timeout(timeout, ops.zfs_dataset(&dataset)).await {
                Ok(Ok(ds)) if ds.kind != "volume" => PoolState {
                    pool_id: id,
                    ready: true,
                    error: None,
                    total_bytes: ds.space.available_bytes.saturating_add(ds.space.used_bytes),
                    available_bytes: ds.space.available_bytes,
                },
                Ok(Ok(_)) => not_ready(
                    id,
                    format!("{dataset} is a volume, not a filesystem dataset"),
                ),
                Ok(Err(e)) => not_ready(id, e),
                Err(_) => not_ready(id, ZFS_TIMED_OUT.into()),
            }
        })
    }

    /// QEMU의 rbd 블록 드라이버(아직 확인하지 않았으면)와 Ceph 풀을 확인하는 작업을 시작한다. 접근할
    /// 수 있으면 준비됐고, 용량은 `max_avail`+`stored`와 `max_avail`이다. 시간 제한이 지나면 명령을
    /// 죽인다.
    fn spawn_rbd_check(
        &self,
        id: Uuid,
        pool: String,
        namespace: Option<String>,
        user: String,
        conf: Option<String>,
    ) -> JoinHandle<PoolState> {
        let (ops, timeout, qemu_rbd) =
            (self.ops.clone(), self.check_timeout, self.qemu_rbd.clone());
        tokio::spawn(async move {
            let check = async {
                if !qemu_rbd.load(Ordering::SeqCst) {
                    ops.qemu_rbd().await?;
                    qemu_rbd.store(true, Ordering::SeqCst);
                }
                ops.rbd_pool(&pool, namespace.as_deref(), &user, conf.as_deref())
                    .await
            };
            match tokio::time::timeout(timeout, check).await {
                Ok(Ok(space)) => PoolState {
                    pool_id: id,
                    ready: true,
                    error: None,
                    total_bytes: space.available_bytes.saturating_add(space.stored_bytes),
                    available_bytes: space.available_bytes,
                },
                Ok(Err(e)) => not_ready(id, e),
                Err(_) => not_ready(id, RBD_TIMED_OUT.into()),
            }
        })
    }

    /// 공유 LVM 풀 확인 작업. 앞 작업이 아직 돌고 있으면 그것을, 아니면 새 작업을 돌려준다.
    /// iSCSI 로그인(있으면)과 잠금 시작은 시간 제한 없이 기다리고, `vgs`는 시간 제한이 지나면 죽인다.
    /// 잠금 종류가 `sanlock`·`dlm`인 공유 VG면 준비됐고, 용량은 VG의 크기와 남은 공간이다.
    fn lvm_shared_check(
        &mut self,
        id: Uuid,
        vg: String,
        iscsi: Option<IscsiTarget>,
    ) -> JoinHandle<PoolState> {
        if let Some(task) = self.lvm_shared_checks.remove(&id) {
            return task;
        }
        let (ops, timeout) = (self.ops.clone(), self.check_timeout);
        tokio::spawn(async move {
            let prepare = async {
                if let Some(t) = &iscsi {
                    ops.iscsi_login(&t.portal, &t.target).await?;
                }
                ops.lvm_lockstart(&vg).await
            };
            if let Err(e) = prepare.await {
                return not_ready(id, e);
            }
            match tokio::time::timeout(timeout, ops.lvm_vg(&vg)).await {
                Ok(Ok(info)) if matches!(info.lock_type.as_str(), "sanlock" | "dlm") => PoolState {
                    pool_id: id,
                    ready: true,
                    error: None,
                    total_bytes: info.size_bytes,
                    available_bytes: info.free_bytes,
                },
                Ok(Ok(_)) => not_ready(
                    id,
                    format!("volume group {vg} is not a shared (lvmlockd) VG"),
                ),
                Ok(Err(e)) => not_ready(id, e),
                Err(_) => not_ready(id, VGS_TIMED_OUT.into()),
            }
        })
    }

    /// 끝난 마운트·언마운트를 거둔다. 마운트 결과는 풀 id별로 돌려주고, 언마운트 결과는 기록한다.
    fn collect_finished(&mut self) -> HashMap<Uuid, Result<(), String>> {
        let mut mounts = HashMap::new();
        for finished in std::mem::take(&mut *self.finished.lock().unwrap()) {
            match finished {
                Finished::Mount(pool, result) => {
                    self.mounting.remove(&pool);
                    mounts.insert(pool, result);
                }
                Finished::Unmount(dir, result) => {
                    self.unmounting.remove(&dir);
                    match result {
                        Ok(()) => {
                            tracing::info!(dir = %dir.display(), "unmounted nfs pool no longer used on this node");
                            self.umount_errors.remove(&dir);
                        }
                        Err(e) => {
                            if self.umount_errors.get(&dir) != Some(&e) {
                                tracing::warn!(dir = %dir.display(), error = %e, "cannot unmount nfs pool");
                            }
                            self.umount_errors.insert(dir, e);
                        }
                    }
                }
            }
        }
        mounts
    }

    /// 쓰기 확인과 용량을 풀마다 별도 blocking 작업으로 한꺼번에 시작하고, 모두 같은 시간 제한
    /// 안에서 기다린다. 앞 확인이 아직 멈춰 있어 건너뛴 풀은 `None`.
    async fn check_all<'a>(
        &mut self,
        checks: Vec<(&'a StoragePool, PathBuf, &'static str)>,
    ) -> Vec<(&'a StoragePool, Option<PoolState>)> {
        let deadline = Instant::now() + self.check_timeout;
        let mut out = Vec::new();
        let mut running = Vec::new();
        for (pool, dir, not_responding) in checks {
            if let Some(task) = self.stuck.get(&pool.id) {
                if !task.is_finished() {
                    out.push((pool, None));
                    continue;
                }
                self.stuck.remove(&pool.id);
            }
            let ops = self.ops.clone();
            let probe = format!(".opl-check-{}", self.node);
            let checked = dir.clone();
            let task = tokio::task::spawn_blocking(move || ops.check(&checked, &probe));
            running.push((pool, dir, not_responding, task));
        }
        for (pool, dir, not_responding, mut task) in running {
            let state = match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(Ok(Ok(capacity))) => PoolState {
                    pool_id: pool.id,
                    ready: true,
                    error: None,
                    total_bytes: capacity.total_bytes,
                    available_bytes: capacity.available_bytes,
                },
                Ok(Ok(Err(e))) => not_ready(pool.id, format!("{}: {e}", dir.display())),
                Ok(Err(e)) => not_ready(pool.id, format!("{}: {e}", dir.display())),
                Err(_) => {
                    self.stuck.insert(pool.id, task);
                    not_ready(pool.id, not_responding.into())
                }
            };
            out.push((pool, Some(state)));
        }
        out
    }

    /// 이 노드가 쓸 수 없게 된 NFS 풀(지워졌거나 `nodes`에서 빠짐)의 언마운트를 시작한다. 끝나면
    /// 마운트 지점을 지운다. 그 풀의 디스크를 쓰는 VM이 이 노드에서 돌고 있거나 앞 언마운트가 아직
    /// 끝나지 않았으면 다음 tick으로 미룬다. `umount -f`는 쓰지 않는다(쓰던 데이터를 잃을 수 있다).
    fn unmount_unused(
        &mut self,
        pools: &[StoragePool],
        usable: &[&StoragePool],
        mounted: &BTreeSet<PathBuf>,
    ) -> anyhow::Result<()> {
        let keep: BTreeSet<PathBuf> = usable
            .iter()
            .filter(|p| matches!(p.spec.backend, PoolBackend::Nfs { .. }))
            .map(|p| self.mount_root.join(p.id.to_string()))
            .collect();
        let pool_dir = |path: &Path| {
            path.parent() == Some(self.mount_root.as_path())
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| Uuid::try_parse(n).is_ok())
        };
        let unused: Vec<PathBuf> = mounted
            .iter()
            .filter(|p| pool_dir(p) && !keep.contains(*p) && !self.unmounting.contains(*p))
            .cloned()
            .collect();
        if !unused.is_empty() {
            let busy = self.busy_pool_dirs(pools)?;
            for dir in unused {
                if busy.contains(&dir) {
                    tracing::debug!(dir = %dir.display(), "a running VM uses this nfs pool; not unmounting yet");
                    continue;
                }
                self.unmounting.insert(dir.clone());
                let (ops, finished, done) =
                    (self.ops.clone(), self.finished.clone(), self.done.clone());
                let timeout = self.mount_timeout;
                tokio::spawn(async move {
                    let result = tokio::time::timeout(timeout, ops.umount(&dir))
                        .await
                        .unwrap_or_else(|_| Err("umount timed out".into()));
                    if result.is_ok() {
                        let _ = std::fs::remove_dir(&dir);
                    }
                    finished
                        .lock()
                        .unwrap()
                        .push(Finished::Unmount(dir, result));
                    done.notify_one();
                });
            }
        }
        // 마운트되지 않은 남은 마운트 지점(빈 디렉터리)을 지운다.
        for entry in std::fs::read_dir(&self.mount_root)?.flatten() {
            let path = entry.path();
            if pool_dir(&path) && !keep.contains(&path) && !mounted.contains(&path) {
                let _ = std::fs::remove_dir(&path);
            }
        }
        Ok(())
    }

    /// 이 노드에서 도는 VM의 디스크가 있는 풀의 마운트 지점
    fn busy_pool_dirs(&self, pools: &[StoragePool]) -> anyhow::Result<BTreeSet<PathBuf>> {
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        let (_, volumes) = self.cluster.list::<VolumeSpec>()?;
        let mut dirs = BTreeSet::new();
        let running = vms.iter().filter(|vm| {
            vm.status.node == Some(self.node)
                && matches!(
                    vm.status.phase,
                    VmPhase::Starting | VmPhase::Running | VmPhase::Stopping
                )
        });
        for disk in running.flat_map(|vm| &vm.spec.disks) {
            let pool = volumes
                .iter()
                .find(|v| v.name == disk.volume)
                .and_then(|v| pools.iter().find(|p| p.name == v.spec.pool));
            if let Some(pool) = pool {
                dirs.insert(self.mount_root.join(pool.id.to_string()));
            }
        }
        Ok(dirs)
    }

    /// 바뀌었으면(용량은 1 GiB 넘게, [`keep_capacity`]) `NodeStatus.pools`에 쓴다. 다른 쪽이 그 사이
    /// status를 썼으면 다시 읽고 쓴다.
    async fn write(&self, pools: BTreeMap<String, PoolState>) -> anyhow::Result<()> {
        loop {
            let Some(node) = self.cluster.get::<NodeSpec>(self.node)? else {
                return Ok(());
            };
            let pools = keep_capacity(&node.status.pools, pools.clone());
            if node.status.pools == pools {
                return Ok(());
            }
            let status = NodeStatus {
                pools,
                ..node.status
            };
            match self
                .cluster
                .update_status_checked::<NodeSpec>(self.node, node.revision, &status)
                .await
            {
                Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// 앞서 적은 상태와 용량만 1 GiB 넘게 바뀌지 않았으면 앞 용량을 둔다. 쓰기·지우기로 조금씩 바뀌는
/// 용량 때문에 tick마다 노드 상태를 다시 쓰지 않게 한다.
fn keep_capacity(
    old: &BTreeMap<String, PoolState>,
    mut new: BTreeMap<String, PoolState>,
) -> BTreeMap<String, PoolState> {
    for (name, state) in &mut new {
        if let Some(prev) = old.get(name)
            && prev.pool_id == state.pool_id
            && prev.ready == state.ready
            && prev.error == state.error
            && prev.total_bytes.abs_diff(state.total_bytes) < GIB
            && prev.available_bytes.abs_diff(state.available_bytes) < GIB
        {
            state.total_bytes = prev.total_bytes;
            state.available_bytes = prev.available_bytes;
        }
    }
    new
}

fn not_ready(pool: Uuid, error: String) -> PoolState {
    PoolState {
        pool_id: pool,
        ready: false,
        error: Some(error),
        total_bytes: 0,
        available_bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;

    use opl_driver::ZfsSpace;
    use opl_types::resource::Resource;
    use opl_types::vm::VmStatus;

    use super::*;
    use crate::bootstrap::tests::single_node;

    #[test]
    fn builds_mount_arguments() {
        let dir = Path::new("/var/lib/opl/pools/p1");
        // 사용자 옵션이 있든 없든 nosuid,nodev,noexec,retry=0을 먼저 붙인다(사용자 옵션이 이긴다).
        assert_eq!(
            mount_args("nas.lan", "/srv/vm", None, dir),
            [
                "-t",
                "nfs",
                "-o",
                "nosuid,nodev,noexec,retry=0",
                "nas.lan:/srv/vm",
                "/var/lib/opl/pools/p1"
            ]
        );
        assert_eq!(
            mount_args("10.0.0.5", "/srv/vm", Some("vers=4.2,hard"), dir),
            [
                "-t",
                "nfs",
                "-o",
                "nosuid,nodev,noexec,retry=0,vers=4.2,hard",
                "10.0.0.5:/srv/vm",
                "/var/lib/opl/pools/p1"
            ]
        );
        assert_eq!(
            mount_args("fd00::5", "/srv/vm", None, dir)[4],
            "[fd00::5]:/srv/vm"
        );
    }

    /// 마운트를 메모리에만 기록하는 가짜
    #[derive(Default)]
    struct FakeMount {
        mounted: Mutex<BTreeSet<PathBuf>>,
        calls: Mutex<Vec<String>>,
        mount_error: Mutex<Option<String>>,
        /// 참이면 확인이 풀릴 때까지 멈춘다(멈춘 NFS 서버).
        hang: AtomicBool,
        /// 참이면 `/srv/slow` 마운트가 풀릴 때까지 멈춘다.
        hang_slow_mount: AtomicBool,
        checks: Mutex<usize>,
        /// 용량에 더하는 바이트(쓰기·지우기로 조금씩 바뀌는 용량)
        extra: AtomicU64,
        /// VG별 LV
        lvs: Mutex<BTreeMap<String, Vec<LvInfo>>>,
        /// 참이면 `lvs`가 풀릴 때까지 멈춘다.
        hang_lvs: AtomicBool,
        /// 이름별 ZFS 데이터셋
        datasets: Mutex<BTreeMap<String, ZfsDataset>>,
        /// 참이면 `zfs`가 풀릴 때까지 멈춘다.
        hang_zfs: AtomicBool,
        /// `{pool}[/{namespace}]`별 Ceph 풀 공간
        rbd: Mutex<BTreeMap<String, RbdSpace>>,
        /// 참이면 `rbd`가 풀릴 때까지 멈춘다.
        hang_rbd: AtomicBool,
        /// 참이면 QEMU에 rbd 블록 드라이버가 없다.
        no_qemu_rbd: AtomicBool,
        /// 이름별 VG
        vgs: Mutex<BTreeMap<String, VgInfo>>,
        /// iSCSI 로그인 오류
        iscsi_error: Mutex<Option<String>>,
        /// 참이면 잠금 시작이 풀릴 때까지 멈춘다.
        hang_lockstart: AtomicBool,
    }

    impl FakeMount {
        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut self.calls.lock().unwrap())
        }
    }

    #[async_trait]
    impl MountOps for FakeMount {
        fn mountinfo(&self) -> std::io::Result<String> {
            let mounted = self.mounted.lock().unwrap();
            Ok(mounted
                .iter()
                .map(|p| format!("40 22 0:45 / {} rw - nfs4 nas:/srv rw\n", p.display()))
                .collect())
        }

        async fn mount(&self, args: &[OsString]) -> Result<(), String> {
            let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
            self.calls
                .lock()
                .unwrap()
                .push(format!("mount {}", args.join(" ")));
            while self.hang_slow_mount.load(Ordering::SeqCst)
                && args.iter().any(|a| a.ends_with(":/srv/slow"))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if let Some(e) = self.mount_error.lock().unwrap().clone() {
                return Err(e);
            }
            self.mounted
                .lock()
                .unwrap()
                .insert(PathBuf::from(args.last().unwrap().as_ref()));
            Ok(())
        }

        async fn umount(&self, dir: &Path) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("umount {}", dir.display()));
            self.mounted.lock().unwrap().remove(dir);
            Ok(())
        }

        async fn lvm_pool(&self, vg: &str, thinpool: &str) -> Result<LvInfo, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("lvs {vg}/{thinpool}"));
            while self.hang_lvs.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let lvs = self.lvs.lock().unwrap();
            let lvs = lvs
                .get(vg)
                .ok_or_else(|| format!("Volume group \"{vg}\" not found"))?;
            lvs.iter()
                .find(|lv| lv.name == thinpool)
                .cloned()
                .ok_or_else(|| format!("Failed to find logical volume \"{vg}/{thinpool}\""))
        }

        async fn zfs_dataset(&self, dataset: &str) -> Result<ZfsDataset, String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("zfs get {dataset}"));
            while self.hang_zfs.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.datasets
                .lock()
                .unwrap()
                .get(dataset)
                .cloned()
                .ok_or_else(|| format!("cannot open '{dataset}': dataset does not exist"))
        }

        async fn rbd_pool(
            &self,
            pool: &str,
            namespace: Option<&str>,
            user: &str,
            conf: Option<&str>,
        ) -> Result<RbdSpace, String> {
            let spec = namespace.map_or_else(|| pool.to_string(), |ns| format!("{pool}/{ns}"));
            let conf = conf.map(|c| format!(" --conf {c}")).unwrap_or_default();
            self.calls
                .lock()
                .unwrap()
                .push(format!("rbd ls {spec} --id {user}{conf}"));
            while self.hang_rbd.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            self.rbd.lock().unwrap().get(&spec).copied().ok_or_else(|| {
                format!("rbd: error opening pool '{pool}': (2) No such file or directory")
            })
        }

        async fn qemu_rbd(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("qemu rbd".into());
            if self.no_qemu_rbd.load(Ordering::SeqCst) {
                return Err(QEMU_NO_RBD.into());
            }
            Ok(())
        }

        async fn iscsi_login(&self, portal: &str, target: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("iscsiadm login {target} -p {portal}"));
            match self.iscsi_error.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }

        async fn lvm_lockstart(&self, vg: &str) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("vgchange --lockstart {vg}"));
            while self.hang_lockstart.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if !self.vgs.lock().unwrap().contains_key(vg) {
                return Err(format!("Volume group \"{vg}\" not found"));
            }
            Ok(())
        }

        async fn lvm_vg(&self, vg: &str) -> Result<VgInfo, String> {
            self.calls.lock().unwrap().push(format!("vgs {vg}"));
            self.vgs
                .lock()
                .unwrap()
                .get(vg)
                .cloned()
                .ok_or_else(|| format!("Volume group \"{vg}\" not found"))
        }

        fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity> {
            *self.checks.lock().unwrap() += 1;
            while self.hang.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(probe.starts_with(".opl-check-"));
            if !dir.is_dir() {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            let extra = self.extra.load(Ordering::SeqCst);
            Ok(Capacity {
                total_bytes: 100 * GIB + extra,
                available_bytes: 40 * GIB + extra,
            })
        }
    }

    struct Setup {
        _dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        node: Uuid,
        root: PathBuf,
        local: PathBuf,
        fake: Arc<FakeMount>,
        pools: Pools,
    }

    async fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(node)) = single_node(&dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let mut me = Resource::new(
            "n1",
            NodeSpec {
                address: "h:8006".into(),
                maintenance: false,
            },
        );
        me.id = node;
        cluster.create(&me).await.unwrap();
        let root = dir.path().canonicalize().unwrap().join("pools");
        let local = dir.path().join("local");
        std::fs::create_dir(&local).unwrap();
        let fake = Arc::new(FakeMount::default());
        let mut pools = Pools::new(cluster.clone(), root.clone(), fake.clone());
        pools.nfs_supported = true;
        pools.lvm_supported = true;
        pools.zfs_supported = true;
        pools.rbd_supported = true;
        pools.check_timeout = Duration::from_millis(200);
        Setup {
            _dir: dir,
            cluster,
            node,
            root,
            local,
            fake,
            pools,
        }
    }

    impl Pools {
        /// 새로 고치고, 그때 시작한 마운트·언마운트가 끝나면 한 번 더 새로 고친다.
        async fn settle(&mut self) {
            self.tick().await.unwrap();
            while self.finished.lock().unwrap().len() < self.mounting.len() + self.unmounting.len()
            {
                tokio::time::timeout(Duration::from_secs(10), self.done.notified())
                    .await
                    .expect("mount or unmount did not finish");
            }
            self.tick().await.unwrap();
        }
    }

    impl Setup {
        async fn pool(&self, name: &str, backend: PoolBackend, nodes: Vec<Uuid>) -> StoragePool {
            let pool = Resource::new(
                name,
                StoragePoolSpec {
                    backend,
                    nodes,
                    shared: false,
                },
            );
            self.cluster.create(&pool).await.unwrap();
            pool
        }

        fn states(&self) -> BTreeMap<String, PoolState> {
            self.cluster
                .get::<NodeSpec>(self.node)
                .unwrap()
                .unwrap()
                .status
                .pools
        }
    }

    fn nfs() -> PoolBackend {
        PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: Some("vers=4.2".into()),
        }
    }

    fn ready(pool: &StoragePool) -> PoolState {
        PoolState {
            pool_id: pool.id,
            ready: true,
            error: None,
            total_bytes: 100 * GIB,
            available_bytes: 40 * GIB,
        }
    }

    #[tokio::test]
    async fn records_dir_and_nfs_pools_usable_on_this_node() {
        let mut s = setup().await;
        let local = s
            .pool(
                "local",
                PoolBackend::Dir {
                    path: s.local.clone(),
                },
                vec![],
            )
            .await;
        let missing = s
            .pool(
                "missing",
                PoolBackend::Dir {
                    path: s.root.join("nope"),
                },
                vec![s.node],
            )
            .await;
        // 다른 노드만 쓰는 풀은 적지 않는다.
        s.pool("elsewhere", nfs(), vec![Uuid::now_v7()]).await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        let mount_dir = s.root.join(nas.id.to_string());

        s.pools.settle().await;
        assert_eq!(
            s.fake.calls(),
            [format!(
                "mount -t nfs -o nosuid,nodev,noexec,retry=0,vers=4.2 nas:/srv/vm {}",
                mount_dir.display()
            )]
        );
        let states = s.states();
        assert_eq!(
            states.keys().collect::<Vec<_>>(),
            ["local", "missing", "nas"]
        );
        // 상태마다 어느 풀의 것인지(id) 적는다.
        assert_eq!(states["local"], ready(&local));
        assert_eq!(states["nas"], ready(&nas));
        assert_eq!(states["missing"].pool_id, missing.id);
        assert!(!states["missing"].ready);
        assert!(states["missing"].error.as_ref().unwrap().contains("nope"));

        // 이미 마운트돼 있으면 다시 마운트하지 않고, 바뀐 것이 없으면 쓰지 않는다.
        let revision = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        let after = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        assert_eq!(after, revision);

        // 용량이 1 GiB 넘게 바뀌지 않았으면 다시 쓰지 않는다.
        s.fake.extra.store(GIB - 1, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        let after = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        assert_eq!(after, revision);
        assert_eq!(s.states()["nas"], ready(&nas));
        s.fake.extra.store(GIB, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        let state = &s.states()["nas"];
        assert_eq!(state.total_bytes, 101 * GIB);
        assert_eq!(state.available_bytes, 41 * GIB);
        s.cluster.shutdown().await.unwrap();
    }

    fn lvm(vg: &str, thinpool: &str) -> PoolBackend {
        PoolBackend::LvmThin {
            vg: vg.into(),
            thinpool: thinpool.into(),
        }
    }

    fn lv(name: &str, attr: &str, size_bytes: u64, data_percent: Option<f64>) -> LvInfo {
        LvInfo {
            name: name.into(),
            attr: attr.into(),
            size_bytes,
            data_percent,
        }
    }

    /// LVM-thin 풀은 `lvs`로 thin pool을 확인해 크기와 데이터 사용률로 용량을 적는다. VG나 LV가
    /// 없거나 thin pool이 아니면 이유와 함께 준비되지 않았다.
    #[tokio::test]
    async fn records_lvm_thin_pool_states() {
        let mut s = setup().await;
        s.fake.lvs.lock().unwrap().insert(
            "vg0".into(),
            vec![
                lv("tp", "twi-aotz--", 100 * GIB, Some(60.0)),
                lv("data", "-wi-a-----", 10 * GIB, None),
            ],
        );
        let tp = s.pool("tp", lvm("vg0", "tp"), vec![]).await;
        let plain = s.pool("plain", lvm("vg0", "data"), vec![]).await;
        let gone = s.pool("gone", lvm("vg0", "nope"), vec![]).await;
        let novg = s.pool("novg", lvm("vg1", "tp"), vec![]).await;

        s.pools.settle().await;
        let states = s.states();
        assert_eq!(states["tp"], ready(&tp));
        assert_eq!(
            states["plain"],
            not_ready(plain.id, "vg0/data is not a thin pool".into())
        );
        assert_eq!(
            states["gone"],
            not_ready(gone.id, "Failed to find logical volume \"vg0/nope\"".into())
        );
        assert_eq!(
            states["novg"],
            not_ready(novg.id, "Volume group \"vg1\" not found".into())
        );
        // 마운트하지 않는다.
        let mut calls = s.fake.calls();
        calls.sort();
        calls.dedup();
        assert_eq!(
            calls,
            ["lvs vg0/data", "lvs vg0/nope", "lvs vg0/tp", "lvs vg1/tp"]
        );
        s.cluster.shutdown().await.unwrap();
    }

    /// 멈춘 `lvs`는 시간 제한 뒤 준비되지 않았다고 적고, 다른 풀의 기록을 막지 않는다.
    #[tokio::test]
    async fn hung_lvs_times_out() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        let tp = s.pool("tp", lvm("vg0", "tp"), vec![]).await;
        s.fake.hang_lvs.store(true, Ordering::SeqCst);
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["tp"], not_ready(tp.id, LVS_TIMED_OUT.into()));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn lvm_thin_is_not_checked_where_unsupported() {
        let mut s = setup().await;
        s.pools.lvm_supported = false;
        let tp = s.pool("tp", lvm("vg0", "tp"), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["tp"],
            not_ready(tp.id, "lvm-thin pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    fn zfs(dataset: &str) -> PoolBackend {
        PoolBackend::Zfs {
            dataset: dataset.into(),
        }
    }

    fn dataset(kind: &str, available_bytes: u64, used_bytes: u64) -> ZfsDataset {
        ZfsDataset {
            kind: kind.into(),
            space: ZfsSpace {
                available_bytes,
                used_bytes,
            },
        }
    }

    /// ZFS 풀은 `zfs get`으로 데이터셋을 확인해 `available`+`used`를 전체로, `available`을 남은
    /// 용량으로 적는다. 데이터셋이 없거나 zvol이면 이유와 함께 준비되지 않았다.
    #[tokio::test]
    async fn records_zfs_pool_states() {
        let mut s = setup().await;
        {
            let mut datasets = s.fake.datasets.lock().unwrap();
            datasets.insert("tank/opl".into(), dataset("filesystem", 40 * GIB, 60 * GIB));
            datasets.insert("tank/vol".into(), dataset("volume", 40 * GIB, 10 * GIB));
            datasets.insert("tank/huge".into(), dataset("filesystem", u64::MAX, GIB));
        }
        let z = s.pool("z", zfs("tank/opl"), vec![]).await;
        let huge = s.pool("huge", zfs("tank/huge"), vec![]).await;
        let vol = s.pool("vol", zfs("tank/vol"), vec![]).await;
        let gone = s.pool("gone", zfs("tank/gone"), vec![]).await;

        s.pools.settle().await;
        let states = s.states();
        assert_eq!(states["z"], ready(&z));
        // 합이 넘치면 가장 큰 값이다.
        assert_eq!(
            states["huge"],
            PoolState {
                total_bytes: u64::MAX,
                available_bytes: u64::MAX,
                ..ready(&huge)
            }
        );
        assert_eq!(
            states["vol"],
            not_ready(
                vol.id,
                "tank/vol is a volume, not a filesystem dataset".into()
            )
        );
        assert_eq!(
            states["gone"],
            not_ready(
                gone.id,
                "cannot open 'tank/gone': dataset does not exist".into()
            )
        );
        // 마운트하지 않는다.
        let mut calls = s.fake.calls();
        calls.sort();
        calls.dedup();
        assert_eq!(
            calls,
            [
                "zfs get tank/gone",
                "zfs get tank/huge",
                "zfs get tank/opl",
                "zfs get tank/vol"
            ]
        );
        s.cluster.shutdown().await.unwrap();
    }

    /// 멈춘 `zfs`는 시간 제한 뒤 준비되지 않았다고 적고, 다른 풀의 기록을 막지 않는다.
    #[tokio::test]
    async fn hung_zfs_times_out() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        let z = s.pool("z", zfs("tank/opl"), vec![]).await;
        s.fake.hang_zfs.store(true, Ordering::SeqCst);
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["z"], not_ready(z.id, ZFS_TIMED_OUT.into()));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn zfs_is_not_checked_where_unsupported() {
        let mut s = setup().await;
        s.pools.zfs_supported = false;
        let z = s.pool("z", zfs("tank/opl"), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["z"],
            not_ready(z.id, "zfs pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    fn rbd(
        pool: &str,
        namespace: Option<&str>,
        user: Option<&str>,
        conf: Option<&str>,
    ) -> PoolBackend {
        PoolBackend::Rbd {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
            user: user.map(Into::into),
            conf: conf.map(Into::into),
        }
    }

    fn space(available_bytes: u64, stored_bytes: u64) -> RbdSpace {
        RbdSpace {
            available_bytes,
            stored_bytes,
        }
    }

    /// RBD 풀은 `rbd ls`로 풀(네임스페이스)에 접근할 수 있으면 준비됐고, 용량은 `ceph df`의
    /// `max_avail`+`stored`와 `max_avail`이다. 사용자가 없으면 `admin`으로 접속한다. 접근할 수
    /// 없으면 이유와 함께 준비되지 않았다. 마운트하지 않는다.
    #[tokio::test]
    async fn records_rbd_pool_states() {
        let mut s = setup().await;
        {
            let mut rbd = s.fake.rbd.lock().unwrap();
            rbd.insert("rbd".into(), space(40 * GIB, 60 * GIB));
            rbd.insert("rbd/opl".into(), space(40 * GIB, 60 * GIB));
            rbd.insert("huge".into(), space(u64::MAX, GIB));
        }
        let r = s.pool("r", rbd("rbd", None, None, None), vec![]).await;
        let ns = rbd("rbd", Some("opl"), Some("opl"), Some("/etc/ceph/opl.conf"));
        let ns = s.pool("ns", ns, vec![]).await;
        let huge = s.pool("huge", rbd("huge", None, None, None), vec![]).await;
        let gone = s.pool("gone", rbd("gone", None, None, None), vec![]).await;

        s.pools.settle().await;
        let states = s.states();
        assert_eq!(states["r"], ready(&r));
        assert_eq!(states["ns"], ready(&ns));
        // 합이 넘치면 가장 큰 값이다.
        assert_eq!(
            states["huge"],
            PoolState {
                total_bytes: u64::MAX,
                available_bytes: u64::MAX,
                ..ready(&huge)
            }
        );
        assert_eq!(
            states["gone"],
            not_ready(
                gone.id,
                "rbd: error opening pool 'gone': (2) No such file or directory".into()
            )
        );
        let mut calls = s.fake.calls();
        calls.sort();
        calls.dedup();
        assert_eq!(
            calls,
            [
                "qemu rbd",
                "rbd ls gone --id admin",
                "rbd ls huge --id admin",
                "rbd ls rbd --id admin",
                "rbd ls rbd/opl --id opl --conf /etc/ceph/opl.conf",
            ]
        );
        // QEMU의 rbd 드라이버는 한 번 확인하면 다시 확인하지 않는다.
        s.pools.tick().await.unwrap();
        assert!(!s.fake.calls().contains(&"qemu rbd".to_string()));
        s.cluster.shutdown().await.unwrap();
    }

    fn lvm_shared(vg: &str, iscsi: Option<(&str, &str)>) -> PoolBackend {
        PoolBackend::LvmShared {
            vg: vg.into(),
            iscsi: iscsi.map(|(portal, target)| IscsiTarget {
                portal: portal.into(),
                target: target.into(),
            }),
        }
    }

    fn vg(name: &str, lock_type: &str, size_bytes: u64, free_bytes: u64) -> VgInfo {
        VgInfo {
            name: name.into(),
            lock_type: lock_type.into(),
            size_bytes,
            free_bytes,
        }
    }

    const IQN: &str = "iqn.2003-01.org.linux-iscsi.san:opl";

    /// 공유 LVM 풀은 iSCSI에 로그인하고(있으면) VG 잠금을 시작한 뒤 `vgs`로 확인한다. 잠금 종류가
    /// `sanlock`·`dlm`이면 준비됐고 용량은 VG의 크기와 남은 공간이다. 잠금이 없는 VG나 없는 VG는 이유와
    /// 함께 준비되지 않았다. 마운트하지 않는다.
    #[tokio::test]
    async fn records_lvm_shared_pool_states() {
        let mut s = setup().await;
        {
            let mut vgs = s.fake.vgs.lock().unwrap();
            vgs.insert("san".into(), vg("san", "sanlock", 100 * GIB, 40 * GIB));
            vgs.insert("fc".into(), vg("fc", "dlm", 100 * GIB, 40 * GIB));
            vgs.insert("local".into(), vg("local", "", 100 * GIB, 40 * GIB));
        }
        let san = s
            .pool("san", lvm_shared("san", Some(("10.0.0.9", IQN))), vec![])
            .await;
        let fc = s.pool("fc", lvm_shared("fc", None), vec![]).await;
        let local = s.pool("local", lvm_shared("local", None), vec![]).await;
        let gone = s.pool("gone", lvm_shared("gone", None), vec![]).await;

        s.pools.settle().await;
        let states = s.states();
        assert_eq!(states["san"], ready(&san));
        assert_eq!(states["fc"], ready(&fc));
        assert_eq!(
            states["local"],
            not_ready(
                local.id,
                "volume group local is not a shared (lvmlockd) VG".into()
            )
        );
        assert_eq!(
            states["gone"],
            not_ready(gone.id, "Volume group \"gone\" not found".into())
        );
        let mut calls = s.fake.calls();
        calls.sort();
        calls.dedup();
        assert_eq!(
            calls,
            [
                format!("iscsiadm login {IQN} -p 10.0.0.9"),
                "vgchange --lockstart fc".into(),
                "vgchange --lockstart gone".into(),
                "vgchange --lockstart local".into(),
                "vgchange --lockstart san".into(),
                "vgs fc".into(),
                "vgs local".into(),
                "vgs san".into(),
            ]
        );
        s.cluster.shutdown().await.unwrap();
    }

    /// iSCSI 로그인이 실패하면 그 이유로 준비되지 않았고 잠금을 시작하지 않는다.
    #[tokio::test]
    async fn lvm_shared_pools_need_the_iscsi_login() {
        let mut s = setup().await;
        s.fake
            .vgs
            .lock()
            .unwrap()
            .insert("san".into(), vg("san", "sanlock", 100 * GIB, 40 * GIB));
        let error = "iscsiadm: No portals found";
        *s.fake.iscsi_error.lock().unwrap() = Some(error.into());
        let san = s
            .pool("san", lvm_shared("san", Some(("10.0.0.9", IQN))), vec![])
            .await;
        s.pools.tick().await.unwrap();
        assert_eq!(
            s.fake.calls(),
            [format!("iscsiadm login {IQN} -p 10.0.0.9")]
        );
        assert_eq!(s.states()["san"], not_ready(san.id, error.into()));
        s.cluster.shutdown().await.unwrap();
    }

    /// 오래 걸리는 잠금 시작은 죽이지 않고 두며 다른 풀의 기록을 막지 않는다. 그동안 새로 시작하지
    /// 않고, 끝나면 그 결과를 적는다.
    #[tokio::test]
    async fn slow_lock_start_does_not_hold_up_other_pools() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        s.fake
            .vgs
            .lock()
            .unwrap()
            .insert("san".into(), vg("san", "sanlock", 100 * GIB, 40 * GIB));
        let san = s.pool("san", lvm_shared("san", None), vec![]).await;
        s.fake.hang_lockstart.store(true, Ordering::SeqCst);
        let starting = not_ready(san.id, LVM_SHARED_STARTING.into());
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["san"], starting);
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["vgchange --lockstart san"]);
        assert_eq!(s.states()["san"], starting);

        s.fake.hang_lockstart.store(false, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["vgs san"]);
        assert_eq!(s.states()["san"], ready(&san));
        assert!(s.pools.lvm_shared_checks.is_empty());
        s.cluster.shutdown().await.unwrap();
    }

    /// 잠금 시작이 도는 동안 풀을 이 노드에서 쓸 수 없게 되어도 그 작업을 버리지 않는다. 다시 쓸 수
    /// 있게 되면 새로 시작하지 않고 그 결과를 쓴다.
    #[tokio::test]
    async fn lock_start_in_progress_is_kept_while_the_pool_is_unusable() {
        let mut s = setup().await;
        s.fake
            .vgs
            .lock()
            .unwrap()
            .insert("san".into(), vg("san", "sanlock", 100 * GIB, 40 * GIB));
        let san = s.pool("san", lvm_shared("san", None), vec![]).await;
        s.fake.hang_lockstart.store(true, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["vgchange --lockstart san"]);

        let set_nodes = |nodes: Vec<Uuid>| {
            let spec = StoragePoolSpec {
                nodes,
                ..san.spec.clone()
            };
            let cluster = s.cluster.clone();
            async move {
                cluster
                    .update_spec(san.id, None, Default::default(), &spec)
                    .await
                    .unwrap();
            }
        };
        set_nodes(vec![Uuid::now_v7()]).await;
        s.pools.tick().await.unwrap();
        assert!(s.pools.lvm_shared_checks.contains_key(&san.id));

        set_nodes(vec![]).await;
        s.fake.hang_lockstart.store(false, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["vgs san"]);
        assert_eq!(s.states()["san"], ready(&san));
        assert!(s.pools.lvm_shared_checks.is_empty());
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn lvm_shared_is_not_checked_where_unsupported() {
        let mut s = setup().await;
        s.pools.lvm_supported = false;
        let san = s.pool("san", lvm_shared("san", None), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["san"],
            not_ready(san.id, "lvm-shared pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    /// `iscsiadm -m session`의 출력에서 타깃의 세션을 찾는다(포털은 보지 않는다).
    #[test]
    fn finds_iscsi_sessions_by_target() {
        let sessions = format!(
            "tcp: [1] 127.0.0.1:3260,1 {IQN} (non-flash)\n\
             tcp: [2] [fd00::9]:3260,1 iqn.2003-01.org.other:x (non-flash)\n"
        );
        assert!(iscsi_session_present(&sessions, IQN));
        assert!(iscsi_session_present(&sessions, "iqn.2003-01.org.other:x"));
        assert!(!iscsi_session_present(
            &sessions,
            "iqn.2003-01.org.linux-iscsi.san"
        ));
        assert!(!iscsi_session_present("", IQN));
    }

    /// QEMU에 rbd 블록 드라이버가 없으면 RBD 풀은 그 이유로 준비되지 않았고 `rbd`를 부르지 않는다.
    /// 드라이버를 설치하면 다음 새로 고침에서 준비된다.
    #[tokio::test]
    async fn rbd_pools_need_the_qemu_rbd_driver() {
        let mut s = setup().await;
        s.fake
            .rbd
            .lock()
            .unwrap()
            .insert("rbd".into(), space(40 * GIB, 60 * GIB));
        s.fake.no_qemu_rbd.store(true, Ordering::SeqCst);
        let r = s.pool("r", rbd("rbd", None, None, None), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["qemu rbd"]);
        assert_eq!(s.states()["r"], not_ready(r.id, QEMU_NO_RBD.into()));

        s.fake.no_qemu_rbd.store(false, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), ["qemu rbd", "rbd ls rbd --id admin"]);
        assert_eq!(s.states()["r"], ready(&r));
        s.cluster.shutdown().await.unwrap();
    }

    /// QEMU rbd 드라이브 확인: 모르는 드라이버면 없음, rbd 드라이버의 인자 오류면 있음, 그 밖은
    /// 확인하지 못한 것이다.
    #[test]
    fn classifies_the_qemu_rbd_probe() {
        let drive = "qemu-system-x86_64: -drive driver=rbd,if=none:";
        assert_eq!(
            qemu_rbd_result(&format!("{drive} Unknown driver 'rbd'\n")),
            Err(QEMU_NO_RBD.to_string())
        );
        for missing in [
            "Parameter 'pool' is missing",
            "Parameter 'image' is missing",
        ] {
            assert_eq!(qemu_rbd_result(&format!("{drive} {missing}\n")), Ok(()));
        }
        for other in [
            String::new(),
            "qemu-system-x86_64: cannot allocate memory\n".into(),
            format!("{drive} Could not open 'x': Permission denied\n"),
            format!("{drive} Could not open image: pool not found\n"),
        ] {
            let err = qemu_rbd_result(&other).unwrap_err();
            assert!(
                err.starts_with("cannot check the qemu rbd block driver"),
                "{err}"
            );
        }
    }

    /// 멈춘 `rbd`는 시간 제한 뒤 준비되지 않았다고 적고, 다른 풀의 기록을 막지 않는다.
    #[tokio::test]
    async fn hung_rbd_times_out() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        let r = s.pool("r", rbd("rbd", None, None, None), vec![]).await;
        s.fake.hang_rbd.store(true, Ordering::SeqCst);
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["r"], not_ready(r.id, RBD_TIMED_OUT.into()));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn rbd_is_not_checked_where_unsupported() {
        let mut s = setup().await;
        s.pools.rbd_supported = false;
        let r = s.pool("r", rbd("rbd", None, None, None), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["r"],
            not_ready(r.id, "rbd pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn refreshes_as_soon_as_a_pool_is_created() {
        let s = setup().await;
        let dir = |path: &Path| PoolBackend::Dir {
            path: path.to_path_buf(),
        };
        s.pool("first", dir(&s.local), vec![]).await;
        let (stop, rx) = oneshot::channel();
        // 주기는 길게 둔다. 첫 새로 고침 뒤에 만든 풀도 주기를 기다리지 않고 적는다.
        let task = tokio::spawn(run_pools(s.pools, Duration::from_secs(3600), rx));
        let wait_ready = |name: &'static str| {
            let cluster = s.cluster.clone();
            let node = s.node;
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let states = cluster.get::<NodeSpec>(node).unwrap().unwrap().status.pools;
                    if states.get(name).is_some_and(|p| p.ready) {
                        return;
                    }
                    assert!(tokio::time::Instant::now() < deadline, "{name}: {states:?}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        wait_ready("first").await;
        let local = Resource::new(
            "local",
            StoragePoolSpec {
                backend: dir(&s.local),
                nodes: vec![],
                shared: false,
            },
        );
        s.cluster.create(&local).await.unwrap();
        wait_ready("local").await;
        stop.send(()).unwrap();
        task.await.unwrap();
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn mount_failure_is_reported_and_retried() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        *s.fake.mount_error.lock().unwrap() = Some("mount.nfs: access denied by server".into());
        let denied = not_ready(nas.id, "mount.nfs: access denied by server".into());
        s.pools.settle().await;
        assert_eq!(s.states()["nas"], denied);
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.pools.backoff[&nas.id].delay, Duration::from_secs(15));
        // 백오프 동안에는 다시 마운트하지 않고 마지막 오류를 둔다.
        *s.fake.mount_error.lock().unwrap() = None;
        s.pools.settle().await;
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(s.states()["nas"], denied);
        // 시간이 지나 다시 실패하면 두 배로 기다린다.
        *s.fake.mount_error.lock().unwrap() = Some("mount.nfs: access denied by server".into());
        s.pools.backoff.get_mut(&nas.id).unwrap().until = Instant::now();
        s.pools.settle().await;
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.pools.backoff[&nas.id].delay, Duration::from_secs(30));
        // 다음 시도가 성공하면 준비되고 백오프를 잊는다.
        *s.fake.mount_error.lock().unwrap() = None;
        s.pools.backoff.get_mut(&nas.id).unwrap().until = Instant::now();
        s.pools.settle().await;
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.states()["nas"], ready(&nas));
        assert!(s.pools.backoff.is_empty());
        s.cluster.shutdown().await.unwrap();
    }

    #[test]
    fn mount_backoff_doubles_up_to_five_minutes() {
        let mut delays = vec![next_backoff(None)];
        for _ in 0..6 {
            delays.push(next_backoff(delays.last().copied()));
        }
        assert_eq!(
            delays,
            [15, 30, 60, 120, 240, 300, 300].map(Duration::from_secs)
        );
    }

    #[tokio::test]
    async fn a_hung_mount_does_not_hold_up_other_pools() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        let slow = PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/slow".into(),
            options: None,
        };
        let slow = s.pool("slow", slow, vec![]).await;
        s.fake.hang_slow_mount.store(true, Ordering::SeqCst);
        let in_progress = not_ready(slow.id, "mount in progress".into());

        // 멈춘 마운트(시간 제한 30초)를 기다리지 않고 다른 풀의 상태를 적는다.
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["slow"], in_progress);
        // 진행 중인 마운트가 있으면 새로 시작하지 않는다.
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.states()["slow"], in_progress);

        // 그동안 만든 다른 NFS 풀은 따로 마운트돼 준비된다.
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.tick().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), s.pools.done.notified())
            .await
            .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(s.states()["nas"], ready(&nas));
        assert_eq!(s.states()["slow"], in_progress);
        assert_eq!(s.fake.calls().len(), 1);

        s.fake.hang_slow_mount.store(false, Ordering::SeqCst);
        s.pools.settle().await;
        assert_eq!(s.states()["slow"], ready(&slow));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pool_being_unmounted_is_not_ready_while_still_mounted() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.settle().await;
        assert_eq!(s.states()["nas"], ready(&nas));
        // 이 노드에서 쓰지 않게 돼 언마운트를 시작했고, 끝나기 전에 다시 쓰게 됐다. 아직 mountinfo에
        // 보여도 곧 풀린다.
        s.pools.unmounting.insert(s.root.join(nas.id.to_string()));
        s.pools.tick().await.unwrap();
        let state = &s.states()["nas"];
        assert!(!state.ready);
        assert_eq!(state.error.as_deref(), Some(MOUNT_IN_PROGRESS));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn nfs_is_not_mounted_where_unsupported() {
        let mut s = setup().await;
        s.pools.nfs_supported = false;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stuck_check_reports_not_responding_and_is_skipped_until_it_ends() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.fake.hang.store(true, Ordering::SeqCst);
        s.pools.settle().await;
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        // 앞 확인이 멈춰 있는 동안 새 확인을 시작하지 않는다.
        s.pools.tick().await.unwrap();
        assert_eq!(*s.fake.checks.lock().unwrap(), 1);
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        // 기록된 상태가 같은 이름의 다른 풀(지우고 다시 만든 풀)의 것이면 그대로 두지 않는다.
        let mut me = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap();
        let stale = PoolState {
            pool_id: Uuid::now_v7(),
            ..ready(&nas)
        };
        me.status.pools.insert("nas".into(), stale);
        s.cluster
            .update_status::<NodeSpec>(s.node, &me.status)
            .await
            .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        s.fake.hang.store(false, Ordering::SeqCst);
        // 멈췄던 확인이 실제로 끝날 때까지 기다린다(느린 머신에서는 50ms로 모자랐다).
        tokio::time::timeout(Duration::from_secs(10), async {
            while !s.pools.stuck[&nas.id].is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(*s.fake.checks.lock().unwrap(), 2);
        assert_eq!(s.states()["nas"], ready(&nas));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unmounts_nfs_pools_no_longer_usable_unless_a_running_vm_uses_them() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        let mount_dir = s.root.join(nas.id.to_string());
        s.pools.settle().await;
        s.fake.calls();
        // 이 노드에서 도는 VM이 이 풀의 볼륨을 쓴다.
        let volume = Resource::new(
            "disk1",
            VolumeSpec {
                pool: "nas".into(),
                size_bytes: 1 << 20,
                format: opl_types::runtime::ImageFormat::Qcow2,
            },
        );
        s.cluster.create(&volume).await.unwrap();
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "sockets": 1, "cores": 1 },
            "memory_mib": 512,
            "machine": "q35",
            "disks": [{ "volume": "disk1", "bus": "virtio-blk" }],
        }))
        .unwrap();
        let vm = Resource::new("vm1", spec);
        s.cluster.create(&vm).await.unwrap();
        let running = VmStatus {
            phase: VmPhase::Running,
            node: Some(s.node),
            ..Default::default()
        };
        s.cluster
            .update_status::<VmSpec>(vm.id, &running)
            .await
            .unwrap();
        // 풀에서 이 노드를 뺀다.
        let spec = StoragePoolSpec {
            nodes: vec![Uuid::now_v7()],
            ..nas.spec.clone()
        };
        s.cluster
            .update_spec(nas.id, None, Default::default(), &spec)
            .await
            .unwrap();

        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert!(s.states().is_empty());
        assert!(mount_dir.is_dir());

        // VM이 멈추면 언마운트하고 마운트 지점을 지운다.
        s.cluster
            .update_status::<VmSpec>(vm.id, &VmStatus::default())
            .await
            .unwrap();
        s.pools.settle().await;
        assert_eq!(s.fake.calls(), [format!("umount {}", mount_dir.display())]);
        assert!(!mount_dir.exists());
        s.cluster.shutdown().await.unwrap();
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 638개, 실패 0

Run: `cargo test -p opld --bin opld pools`
Expected: PASS

Run: `cargo test -p opl-reconciler`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): 공유 LVM 풀 마운터(iSCSI 로그인, 잠금 시작, 공유 VG 확인)와 resolve 지원"
```

---

### Task 4: 활성화 수명, 크기 늘리기와 VM 시작, Task 노드

reconciler가 QEMU가 끝난 뒤·시작 실패 뒤·resolve가 중간에 실패한 뒤 디스크를 끈다(`deactivate_volume`). 새 `Storage::active_volumes`와 Task 실행기의 남은 활성화 정리(시작 때와 `tag_sweep` 30분마다, 볼륨마다 끄기 직전에 VM·Task를 다시 확인). 끝나지 않은 크기 늘리기 Task가 디스크를 대상으로 하면 VM 시작은 `ResolveError::VolumeResizing`으로 기다리고(모든 백엔드), 켜거나 끄는 중인 VM의 크기 늘리기는 409 `vm_busy`, 실행기는 완전히 멈춘 VM만 꺼진 경로로 늘린다. 공유 LVM 볼륨 Task는 가장 작은 id의 준비된 온라인 노드로.

**Files:**
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/tests/tasks.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/snapshots.rs`에서

```rust
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다.
fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
```

을 다음으로 바꾼다:

```rust
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다. 디스크 크기 늘리기도 같은 규칙이다.
pub(crate) fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod clone;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_reconciler::ReconcilerHandle;
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared, pool_listed_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vms::is_stopped;
use crate::volumes::{block_pool_kind, usable_node};

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Debug, Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로)
    pub tag_sweep: Duration,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
    /// 실행기가 멈췄는지. 뒤에서 도는 일(남은 태그 지우기)이 보고 함께 멈춘다.
    stopped: watch::Sender<bool>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
            stopped: watch::channel(false).0,
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    let tags = tokio::spawn(sweep_block_tags_every(ctx.clone()));
    let idle = tokio::spawn(deactivate_idle_volumes_every(ctx.clone()));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
    retry.abort();
    tags.abort();
    idle.abort();
    ctx.stopped.send_replace(true);
}

/// 공유 풀에서 남은 태그로 보고 지우려면 스냅샷이 만들어진 뒤 지나야 하는 시간. 다른 노드의 스냅샷
/// Task가 막 찍은 태그를 그 리소스보다 먼저 보고 지우지 않게 한다.
const SHARED_TAG_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// 남은 블록 스냅샷 태그를 시작할 때와 그 뒤 `tag_sweep`마다 지운다([`sweep_block_tags`]). 실행기가
/// 멈추면 함께 멈춘다.
async fn sweep_block_tags_every(ctx: Arc<Ctx>) {
    loop {
        sweep_block_tags(ctx.clone()).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS, RBD) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. opLite 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
///
/// 노드 로컬 풀의 볼륨은 그 볼륨을 만든 노드가 맡는다. 공유 풀(RBD)의 볼륨은 어느 노드에서나
/// 보이므로 노드마다 같은 답이 나오게 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가
/// 맡는다(그런 노드가 없으면 만든 노드). 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로
/// 만든 지 [`SHARED_TAG_MIN_AGE`]가 지난 태그만 지운다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready {
            continue;
        }
        let Ok(Some(pool)) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        else {
            continue;
        };
        if !uses_block_snapshots(&pool) {
            continue;
        }
        let shared = is_shared(&pool);
        let owner = if shared {
            usable_node(&ctx.cluster, &pool, None)
                .ok()
                .flatten()
                .or(volume.status.node)
        } else {
            volume.status.node
        };
        if owner.is_some_and(|n| n != ctx.config.node) {
            continue;
        }
        let format = volume.spec.format;
        let tags = if shared {
            ctx.storage
                .snapshot_tags_older_than(&pool, volume.id, format, SHARED_TAG_MIN_AGE)
                .await
        } else {
            ctx.storage.snapshot_tags(&pool, volume.id, format).await
        };
        let tags = match tags {
            Ok(tags) => tags,
            Err(e) => {
                tracing::debug!(volume = %volume.name, error = %e, "cannot read block snapshots");
                continue;
            }
        };
        for tag in tags {
            let Ok(id) = tag.parse::<Uuid>() else {
                continue;
            };
            if !matches!(ctx.cluster.get::<SnapshotSpec>(id), Ok(None)) {
                continue;
            }
            tracing::warn!(volume = %volume.name, tag, "removing a block snapshot left without its snapshot");
            if let Err(e) = ctx
                .storage
                .snapshot_delete(&pool, volume.id, format, &tag)
                .await
            {
                tracing::warn!(volume = %volume.name, tag, error = %e, "cannot remove block snapshot");
            }
        }
    }
}

/// 이 노드에서 켜진 채 남은 볼륨을 시작할 때와 그 뒤 `tag_sweep`마다 끈다
/// ([`deactivate_idle_volumes`]). 실행기가 멈추면 함께 멈춘다.
async fn deactivate_idle_volumes_every(ctx: Arc<Ctx>) {
    loop {
        deactivate_idle_volumes(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드에서 켜진 채 남은 볼륨(공유 LVM의 배타 활성화)을 끈다. VM이 멈춘 뒤 reconciler가 끄지
/// 못했거나 opld가 그 전에 내려갔을 때 남는다. 이 노드에 놓여 실행 중이거나 켜려는 VM(하이퍼바이저에서
/// 도는 VM 포함)의 디스크, 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직
/// 만드는 중인(pending) 볼륨은 그대로 둔다. 리소스가 없는 볼륨은 끈다.
///
/// 켜진 볼륨을 먼저 읽고 그 뒤에 VM·Task를 읽는다: 볼륨을 켜는 VM·Task는 켜기 전에 이미 저장소에
/// 있으므로 쓰는 중인 볼륨을 끄지 않는다(그 사이 켠 볼륨은 이번에 보지 않는다). 이미 켜져 있던 볼륨을
/// 그 사이 시작한 VM이 그대로 쓸 수도 있으므로 VM·Task는 볼륨마다 끄기 직전에 다시 읽는다.
async fn deactivate_idle_volumes(ctx: &Ctx) {
    let me = ctx.config.node;
    let pools = match ctx.cluster.list::<StoragePoolSpec>() {
        Ok((_, pools)) => pools,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list storage pools");
            return;
        }
    };
    let mut active = Vec::new();
    for pool in pools.into_iter().filter(|p| pool_listed_on(p, me)) {
        match ctx.storage.active_volumes(&pool).await {
            Ok(ids) if !ids.is_empty() => active.push((pool, ids)),
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(pool = %pool.name, error = %e, "cannot list active volumes")
            }
        }
    }
    for (pool, ids) in active {
        for id in ids {
            let (in_use, formats) = match volumes_in_use(ctx).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read which volumes are in use");
                    return;
                }
            };
            if in_use.contains(&id) {
                continue;
            }
            let format = formats.get(&id).copied().unwrap_or(ImageFormat::Raw);
            tracing::info!(pool = %pool.name, volume = %id, "deactivating a volume left active");
            if let Err(e) = ctx.storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(pool = %pool.name, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
}

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식
async fn volumes_in_use(ctx: &Ctx) -> Result<(HashSet<Uuid>, HashMap<Uuid, ImageFormat>), String> {
    let me = ctx.config.node;
    let running = ctx
        .hv
        .list()
        .await
        .map_err(|e| format!("cannot list running VMs: {e}"))?;
    let (volumes, tasks, vms) = ctx
        .cluster
        .list::<VolumeSpec>()
        .and_then(|(_, volumes)| {
            Ok((
                volumes,
                ctx.cluster.list::<TaskSpec>()?.1,
                ctx.cluster.list::<VmSpec>()?.1,
            ))
        })
        .map_err(|e| format!("cannot list volumes, tasks or VMs: {e}"))?;
    let mut in_use: HashSet<Uuid> = volumes
        .iter()
        .filter(|v| v.status.phase != VolumePhase::Ready)
        .map(|v| v.id)
        .collect();
    let mut task_vms = HashSet::new();
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == me && !t.status.state.is_finished())
    {
        in_use.insert(task.spec.target);
        task_vms.insert(task.spec.target);
    }
    let by_name: HashMap<&str, Uuid> = volumes.iter().map(|v| (v.name.as_str(), v.id)).collect();
    for vm in &vms {
        let used = task_vms.contains(&vm.id)
            || running.iter().any(|r| r.id == vm.id)
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running);
        if used {
            in_use.extend(
                vm.spec
                    .disks
                    .iter()
                    .filter_map(|d| by_name.get(d.volume.as_str())),
            );
        }
    }
    let formats = volumes.iter().map(|v| (v.id, v.spec.format)).collect();
    Ok((in_use, formats))
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_clone = matches!(task.spec.op, TaskOp::VmClone { .. });
        return (is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op) || vm_clone)
            .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| uses(&vm.spec))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다).
    let (result, unfinished) =
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        };
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e, unfinished).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다. `unfinished`면 작업이 끝나지 않았다(시간 초과, 재시작으로 중단).
async fn fail(ctx: &Ctx, task: &Task, error: String, unfinished: bool) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]).
async fn cleanup(ctx: &Ctx, task: &Task, unfinished: bool) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op {
        if unfinished {
            snapshot::remove_unfinished_tags(ctx, snapshot);
        }
        if let Err(e) = snapshot::discard_pending(ctx, snapshot).await {
            tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
        }
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let TaskOp::VmClone { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

async fn perform(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
    }
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    match user {
        // 꺼진 VM의 경로(저장소로 늘린다)는 원하는 상태도 꺼짐일 때만 탄다. 켜는 중에 늘리면 그동안
        // 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
        Some((vm, _)) if vm.status.phase != VmPhase::Running && !is_stopped(vm) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. 블록 볼륨은
        // QEMU가 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 볼륨의 실제 크기(LVM은 extent,
        // ZFS는 1 MiB 단위로 올린다)를 알린다. 그래야 게스트가 보는 크기가 아래에서 기록하는
        // 크기와 같다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            let mut size_bytes = size_bytes;
            if block_pool_kind(&pool).is_some() {
                ctx.storage
                    .resize_volume(&pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                size_bytes = ctx
                    .storage
                    .volume_info(&pool, id, volume.spec.format)
                    .await
                    .map_err(|e| format!("cannot read the new size: {e}"))?
                    .virtual_bytes;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => ctx
            .storage
            .resize_volume(&pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let wanted = unfinished
            .iter()
            .any(|t| matches!(&t.spec.op, TaskOp::VolumeImport { source, .. } if *source == path));
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
    unmounted
}
```

`crates/opl-api/src/volumes.rs`에서

```rust
}
use crate::snapshots::vm_placement;
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
}
use crate::snapshots::{check_not_busy, vm_placement};
use crate::state::AppState;
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. RBD 풀은 이 노드를 앞세우지 않고 늘 id가 가장 작은
/// 노드다: 이미지를 바꾸는 명령이 아직 돌고 있다는 표시가 노드마다 따로이므로 한 볼륨의 Task가 같은
/// 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let rbd = matches!(pool.spec.backend, PoolBackend::Rbd { .. });
    let prefer = (!rbd).then_some(state.node);
    usable_node(&state.cluster, pool, prefer)?.ok_or_else(|| {
```

을 다음으로 바꾼다:

```rust
/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. RBD·공유 LVM 풀은 이 노드를 앞세우지 않고 늘 id가 가장
/// 작은 노드다: 이미지(LV)를 바꾸는 명령이 아직 돌고 있다는 표시가 노드마다 따로이므로 한 볼륨의
/// Task가 같은 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let lowest = matches!(
        pool.spec.backend,
        PoolBackend::Rbd { .. } | PoolBackend::LvmShared { .. }
    );
    let prefer = (!lowest).then_some(state.node);
    usable_node(&state.cluster, pool, prefer)?.ok_or_else(|| {
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
/// 거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
```

을 다음으로 바꾼다:

```rust
/// Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
/// 거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다. 쓰는 VM이 켜지거나 꺼지는
/// 중이면(원하는 상태와 실제 상태가 다르다) 409 `vm_busy`다. 꺼진 VM으로 보고 저장소로 늘리는 동안
/// 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
```

`crates/opl-api/src/volumes.rs`에서

```rust
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name));
    let node = match user.and_then(|vm| vm_placement(&state.cluster, Some(state.node), vm)) {
```

을 다음으로 바꾼다:

```rust
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name));
    if let Some(vm) = user {
        check_not_busy(&state, vm)?;
    }
    let node = match user.and_then(|vm| vm_placement(&state.cluster, Some(state.node), vm)) {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    fresh_tags: std::sync::Mutex<Vec<String>>,
}
```

을 다음으로 바꾼다:

```rust
    fresh_tags: std::sync::Mutex<Vec<String>>,
    /// 이 노드에서 켜져 있는 볼륨(풀 이름, 볼륨). `active_volumes`가 알리고 `deactivate_volume`이 뺀다.
    active: std::sync::Mutex<Vec<(String, Uuid)>>,
    /// 끄는 데 이만큼 걸리는 볼륨
    slow_deactivates: std::sync::Mutex<std::collections::HashMap<Uuid, std::time::Duration>>,
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.busy_deletes.lock().unwrap().insert(id, n);
    }
```

을 다음으로 바꾼다:

```rust
        self.busy_deletes.lock().unwrap().insert(id, n);
    }

    /// 풀 `pool`의 볼륨 `id`가 이 노드에서 켜져 있는 것으로 둔다.
    pub fn set_active(&self, pool: &str, id: Uuid) {
        self.active.lock().unwrap().push((pool.into(), id));
    }

    /// 볼륨 `id`를 끄는 데 `delay`가 걸리게 한다(호출은 바로 적는다).
    pub fn slow_deactivate(&self, id: Uuid, delay: std::time::Duration) {
        self.slow_deactivates.lock().unwrap().insert(id, delay);
    }

    /// 켜져 있는 볼륨들(켠 순서)
    pub fn active(&self) -> Vec<Uuid> {
        self.active
            .lock()
            .unwrap()
            .iter()
            .map(|(_, id)| *id)
            .collect()
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    ) -> Result<(), DriverError> {
        Ok(())
    }
    fn supports_snapshots(&self, pool: &StoragePool, format: ImageFormat) -> bool {
```

을 다음으로 바꾼다:

```rust
    ) -> Result<(), DriverError> {
        Ok(())
    }
    /// 호출을 `deactivate`로 적는다.
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("deactivate", id));
        let delay = self.slow_deactivates.lock().unwrap().get(&id).copied();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        self.active.lock().unwrap().retain(|(_, v)| *v != id);
        Ok(())
    }
    async fn active_volumes(&self, pool: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
        let active = self.active.lock().unwrap();
        Ok(active
            .iter()
            .filter(|(p, _)| *p == pool.name)
            .map(|(_, id)| *id)
            .collect())
    }
    fn supports_snapshots(&self, pool: &StoragePool, format: ImageFormat) -> bool {
```

`crates/opl-api/tests/pools.rs`에서

```rust
async fn rbd_volume_tasks_go_to_the_lowest_ready_node() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let rbd = json!({ "kind": "rbd", "pool": "rbd" });
    let (status, body) = admin.post("/storage-pools", pool("ceph", rbd)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
```

을 다음으로 바꾼다:

```rust
async fn rbd_volume_tasks_go_to_the_lowest_ready_node() {
    volume_tasks_go_to_the_lowest_ready_node(json!({ "kind": "rbd", "pool": "rbd" })).await;
}

/// 공유 LVM 풀의 볼륨 Task도 RBD처럼 id가 가장 작은 준비된 온라인 노드가 맡는다.
#[tokio::test]
async fn lvm_shared_volume_tasks_go_to_the_lowest_ready_node() {
    volume_tasks_go_to_the_lowest_ready_node(json!({ "kind": "lvm-shared", "vg": "san" })).await;
}

/// 공유 블록 풀 `backend`(이름 `ceph`)의 볼륨 Task가 id가 가장 작은 준비된 온라인 노드로 가는지 본다.
async fn volume_tasks_go_to_the_lowest_ready_node(backend: Value) {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let (status, body) = admin.post("/storage-pools", pool("ceph", backend)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
    succeeded(&admin.wait_task(&task).await);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

#[tokio::test]
```

을 다음으로 바꾼다:

```rust
    succeeded(&admin.wait_task(&task).await);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

/// 원하는 상태가 켜짐인데 아직 멈춘 VM(켜는 중)의 디스크는 늘리지 않는다. 꺼진 VM으로 보고
/// 저장소로 늘리면 그동안 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다. API가 받은 뒤 켰을 수 있어
/// 실행기가 다시 본다.
#[tokio::test]
async fn resize_fails_while_the_vm_is_being_started() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let mut req = vm_request("web");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    // 노드가 없는 VM은 reconciler가 다루지 않아 멈춘 채 남는다.
    let vm: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let spec = VmSpec {
        power: PowerState::Running,
        ..current.spec.clone()
    };
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 2 << 20,
            },
            target: root.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();

    let done = admin.wait_task(&json!({ "id": task.id })).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    let error = done["status"]["error"].as_str().unwrap();
    assert!(error.contains("starting or stopping"), "{error}");
    assert!(!storage.calls().iter().any(|(c, _)| *c == "resize"));
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!(root.spec.size_bytes, 1 << 20);
}

#[tokio::test]
```

`crates/opl-api/tests/snapshots.rs`에서

```rust

    // 끈 뒤 다른 노드로 옮겼으면 디스크를 다음에 쓸 그 노드에서 한다.
```

을 다음으로 바꾼다:

```rust

    // 켜는 중이거나(원하는 상태가 켜짐인데 아직 멈춤·시작 중) 끄는 중인 VM의 디스크는 409다.
    // 꺼진 VM으로 보고 저장소로 늘리면 그동안 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
    for phase in [VmPhase::Stopped, VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(env.node)).await;
        if phase == VmPhase::Stopped {
            set_vm_power(&env, vm, PowerState::Running).await;
        }
        let (status, body) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{phase:?}: {body}");
        assert_eq!(body["code"], "vm_busy");
    }

    // 끈 뒤 다른 노드로 옮겼으면 디스크를 다음에 쓸 그 노드에서 한다.
```

`crates/opl-api/tests/tasks.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, Vm, VmSpec};
use reqwest::StatusCode;
```

`crates/opl-api/tests/tasks.rs`에서

```rust
    assert_eq!(second["status"]["state"], "succeeded", "{second}");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(second["status"]["state"], "succeeded", "{second}");
}

/// 디스크 `disks`를 쓰는 VM을 저장소에 바로 만든다(이 노드에 놓인다).
async fn vm_with_disks(env: &Env, name: &str, power: PowerState, disks: &[&str]) -> Vm {
    let mut spec: VmSpec =
        serde_json::from_value(common::vm_request(name)["spec"].clone()).unwrap();
    spec.node = Some(env.node);
    spec.power = power;
    spec.nics.clear();
    spec.disks = disks
        .iter()
        .map(|volume| DiskSpec {
            volume: (*volume).into(),
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        })
        .collect();
    let vm: Vm = Resource::new(name, spec);
    env.cluster.create(&vm).await.unwrap();
    vm
}

/// 이 노드에서 `state`로 도는 Task를 저장소에 바로 만든다. 실행 중(running)으로 만든 Task는 실행기가
/// 다시 시작하지 않는다.
async fn task_on(env: &Env, op: TaskOp, target: Uuid, node: Uuid, state: TaskState) {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.status = TaskStatus { state, error: None };
    env.cluster.create(&task).await.unwrap();
}

/// 실행기는 시작할 때와 `tag_sweep`마다 이 노드에서 켜진 채 남은 볼륨을 끈다(공유 LVM의 배타
/// 활성화가 남으면 다른 노드가 그 디스크를 쓸 수 없다). 이 노드의 실행 중이거나 켜려는 VM의 디스크,
/// 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직 만드는 중인 볼륨은 그대로 둔다.
#[tokio::test]
async fn volumes_left_active_are_deactivated() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_config(storage.clone(), |c| {
        c.tag_sweep = std::time::Duration::from_millis(50)
    })
    .await;
    for (name, backend) in [
        (
            "local",
            PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
        ),
        (
            "san",
            PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
        ),
    ] {
        let pool = Resource::new(
            name,
            StoragePoolSpec {
                shared: matches!(backend, PoolBackend::LvmShared { .. }),
                backend,
                nodes: vec![],
            },
        );
        env.cluster.create(&pool).await.unwrap();
    }
    let mut ids = std::collections::HashMap::new();
    for (name, pool, ready) in [
        ("boot", "local", true),
        ("idle", "san", true),
        ("stopped", "san", true),
        ("theirs", "san", true),
        ("resizing", "san", true),
        ("snapping", "san", true),
        ("making", "san", false),
    ] {
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Raw,
            },
        );
        if ready {
            volume.status.phase = VolumePhase::Ready;
        }
        env.cluster.create(&volume).await.unwrap();
        ids.insert(name, (pool, volume.id));
    }
    let web = vm_with_disks(&env, "web", PowerState::Running, &["boot"]).await;
    vm_with_disks(&env, "off", PowerState::Stopped, &["stopped"]).await;
    let snap = vm_with_disks(&env, "snap", PowerState::Stopped, &["snapping"]).await;
    let running = TaskState::Running;
    let resize = TaskOp::VolumeResize {
        size_bytes: 2 << 20,
    };
    task_on(&env, resize.clone(), ids["resizing"].1, env.node, running).await;
    let other = Uuid::now_v7();
    task_on(&env, resize, ids["theirs"].1, other, TaskState::Pending).await;
    let op = TaskOp::SnapshotCreate {
        snapshot: Uuid::now_v7(),
    };
    task_on(&env, op, snap.id, env.node, running).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.pid(web.id).await.is_none() {
        assert!(std::time::Instant::now() < deadline, "web was not started");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let orphan = Uuid::now_v7();
    storage.set_active("san", orphan);
    for (pool, id) in ids.values() {
        storage.set_active(pool, *id);
    }
    let kept = |names: &[&str]| -> Vec<Uuid> {
        let mut v: Vec<Uuid> = names.iter().map(|n| ids[n].1).collect();
        v.sort();
        v
    };
    let want = kept(&["boot", "resizing", "snapping", "making"]);
    let active = || {
        let mut v = storage.active();
        v.sort();
        v
    };
    let deadline = std::time::Instant::now() + common::WAIT;
    while active() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "active volumes: {:?}",
            storage.active()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 여러 번 돌아도 쓰이는 볼륨은 끄지 않고, 끈 볼륨을 다시 끄지 않는다.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(active(), want);
    let mut deactivated: Vec<Uuid> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "deactivate")
        .map(|(_, id)| id)
        .collect();
    deactivated.sort();
    let mut gone = kept(&["idle", "stopped", "theirs"]);
    gone.push(orphan);
    gone.sort();
    assert_eq!(deactivated, gone);
}

/// 남은 활성화를 정리하는 동안 이미 켜진 볼륨을 쓰기 시작하면(그 볼륨을 디스크로 쓰는 VM의 Task)
/// 그 볼륨은 끄지 않는다(볼륨마다 끄기 직전에 VM·Task를 다시 읽는다).
#[tokio::test]
async fn a_volume_taken_up_during_the_sweep_is_not_deactivated() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_config(storage.clone(), |c| {
        c.tag_sweep = std::time::Duration::from_millis(50)
    })
    .await;
    let pool = Resource::new(
        "san",
        StoragePoolSpec {
            shared: true,
            backend: PoolBackend::LvmShared {
                vg: "san".into(),
                iscsi: None,
            },
            nodes: vec![],
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        let mut volume: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: "san".into(),
                size_bytes: 1 << 20,
                format: ImageFormat::Raw,
            },
        );
        volume.status.phase = VolumePhase::Ready;
        env.cluster.create(&volume).await.unwrap();
        ids.push(volume.id);
    }
    let (first, second) = (ids[0], ids[1]);
    storage.slow_deactivate(first, std::time::Duration::from_millis(500));
    storage.set_active("san", first);
    storage.set_active("san", second);

    // 첫 볼륨을 끄는 동안 이 노드에서 둘째 볼륨을 디스크로 쓰는 VM의 Task가 시작한다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage.calls().contains(&("deactivate", first)) {
        assert!(
            std::time::Instant::now() < deadline,
            "first was not deactivated"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let vm = vm_with_disks(&env, "late", PowerState::Stopped, &["second"]).await;
    let op = TaskOp::SnapshotCreate {
        snapshot: Uuid::now_v7(),
    };
    task_on(&env, op, vm.id, env.node, TaskState::Running).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.active().contains(&first) {
        assert!(std::time::Instant::now() < deadline, "first stayed active");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(storage.active(), [second]);
    assert!(!storage.calls().contains(&("deactivate", second)));
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// VM이 실행 중이어도 조회할 수 있다.
```

을 다음으로 바꾼다:

```rust
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// 이 노드에서 `activate_volume`으로 켜져 있는 `pool`의 볼륨들. 켜진 채 남은 볼륨을 찾아 끄는
    /// 정리가 쓴다. 기본은 없다(켜고 끌 것이 없다).
    async fn active_volumes(&self, _pool: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
        Ok(Vec::new())
    }

    /// VM이 실행 중이어도 조회할 수 있다.
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            Err(e) => Err(e),
        }
    }
```

을 다음으로 바꾼다:

```rust
            Err(e) => Err(e),
        }
    }

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`). 다른 노드가 켠 LV는 이 노드에서 꺼져 보인다.
    async fn active_volumes(&self, pool: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
        let vg = Self::target(pool, ImageFormat::Raw)?;
        Ok(self
            .ops
            .list(vg)
            .await?
            .iter()
            .filter(|lv| lv.is_active())
            .filter_map(|lv| {
                let id = lv.name.strip_prefix("opl-")?.parse::<Uuid>().ok()?;
                (lv.name == volume_lv(id)).then_some(id)
            })
            .collect())
    }
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust

    /// 지우기: 다른 노드가 켜 두었으면 일시 오류, 없으면 `VolumeNotFound`, 없어도 만드는 명령이
```

을 다음으로 바꾼다:

```rust

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`)만 알린다. 꺼진 볼륨 LV와 opLite 밖의 LV는 빼고 LVM
    /// 명령은 목록뿐이다.
    #[tokio::test]
    async fn shared_lists_volumes_active_on_this_node() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let p = shared_pool();
        let (on, off) = (Uuid::now_v7(), Uuid::now_v7());
        for id in [on, off] {
            storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        }
        storage.activate_volume(&p, on, RAW).await.unwrap();
        lvm.lvs.lock().unwrap().insert(
            format!("opl-{on}-x"),
            FakeLv {
                size: 1 << 20,
                active: true,
                skip: false,
            },
        );
        lvm.taken();
        assert_eq!(storage.active_volumes(&p).await.unwrap(), [on]);
        assert!(lvm.taken().is_empty());
    }

    /// 지우기: 다른 노드가 켜 두었으면 일시 오류, 없으면 `VolumeNotFound`, 없어도 만드는 명령이
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            .deactivate_volume(pool, id, format)
            .await
    }
```

을 다음으로 바꾼다:

```rust
            .deactivate_volume(pool, id, format)
            .await
    }

    async fn active_volumes(&self, pool: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
        self.backend(pool)?.active_volumes(pool).await
    }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        }
        async fn volume_info(
```

을 다음으로 바꾼다:

```rust
        }
        async fn active_volumes(&self, _: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
            self.call("active_volumes")?;
            Ok(Vec::new())
        }
        async fn volume_info(
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            storages.deactivate_volume(&p, id, f).await.unwrap();
            storages.volume_info(&p, id, f).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            storages.deactivate_volume(&p, id, f).await.unwrap();
            storages.active_volumes(&p).await.unwrap();
            storages.volume_info(&p, id, f).await.unwrap();
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                "deactivate_volume",
                "volume_info",
```

을 다음으로 바꾼다:

```rust
                "deactivate_volume",
                "active_volumes",
                "volume_info",
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        unsupported(storages.deactivate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
```

을 다음으로 바꾼다:

```rust
        unsupported(storages.deactivate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.active_volumes(&p).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use opl_types::node::NodeSpec;
use opl_types::runtime::{DiskKind, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{
    PoolBackend, StoragePoolSpec, VolumeSpec, pool_error_on, pool_listed_on, pool_state_on,
};
use opl_types::vm::Vm;
```

을 다음으로 바꾼다:

```rust
use opl_types::node::NodeSpec;
use opl_types::runtime::{
    DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec,
};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec, pool_error_on, pool_listed_on,
    pool_state_on,
};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::Vm;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    PoolUnavailable(String, String),
    /// 이 노드의 OS(macOS)는 NFS 풀을 지원하지 않는다. 기다려도 풀리지 않는다.
```

을 다음으로 바꾼다:

```rust
    PoolUnavailable(String, String),
    /// 볼륨의 크기 늘리기 Task가 끝나지 않았다. 끝나면 풀린다.
    #[error("volume {0:?} is being resized")]
    VolumeResizing(String),
    /// 이 노드의 OS(macOS)는 NFS 풀을 지원하지 않는다. 기다려도 풀리지 않는다.
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
/// (다시 해도 안 되는 것은 볼륨이 없거나 정의가 틀린 것뿐이고, 그 밖의 저장소 오류는 일시 오류로 본다.)
pub async fn runtime_spec(
```

을 다음으로 바꾼다:

```rust
/// (다시 해도 안 되는 것은 볼륨이 없거나 정의가 틀린 것뿐이고, 그 밖의 저장소 오류는 일시 오류로 본다.)
/// 끝나지 않은 크기 늘리기 Task가 있는 볼륨은 켜지 않고 `VolumeResizing`이다(늘리는 중인 볼륨을 QEMU가
/// 열면 안 된다. 공유 LVM은 늘린 부분을 0으로 채우기 전이다).
/// 실패하면 그때까지 활성화한 볼륨을 끈다(`deactivate_volume`, 끄지 못하면 경고만 남긴다).
pub async fn runtime_spec(
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    let me = cluster.get::<NodeSpec>(node)?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

을 다음으로 바꾼다:

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, true).await
}

/// 이미 도는 VM에 다시 붙을 때의 [`runtime_spec`]. 크기 늘리기 Task를 기다리지 않는다(QEMU가 이미
/// 장치를 열었다).
pub async fn running_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
) -> Result<VmRuntimeSpec, ResolveError> {
    resolve_activated(cluster, vm, node, mount_root, storage, false).await
}

/// [`runtime_spec`]. `wait_resize`가 거짓이면 크기 늘리기 Task를 보지 않는다.
async fn resolve_activated(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
    wait_resize: bool,
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut activated = Vec::new();
    let result = resolve(
        cluster,
        vm,
        node,
        mount_root,
        storage,
        wait_resize,
        &mut activated,
    )
    .await;
    if result.is_err() {
        for (pool, id, format) in activated {
            if let Err(e) = storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(vm = %vm.id, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
    result
}

/// [`runtime_spec`]. 활성화한 볼륨을 `activated`에 적는다.
async fn resolve(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
    storage: &dyn Storage,
    wait_resize: bool,
    activated: &mut Vec<(StoragePool, Uuid, ImageFormat)>,
) -> Result<VmRuntimeSpec, ResolveError> {
    let me = cluster.get::<NodeSpec>(node)?;
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        let pool_name = &volume.spec.pool;
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ResolveError::VolumeNotFound(disk.volume.clone()))?;
        if wait_resize
            && tasks.iter().any(|t| {
                t.spec.target == volume.id
                    && !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::VolumeResize { .. })
            })
        {
            return Err(ResolveError::VolumeResizing(disk.volume.clone()));
        }
        let pool_name = &volume.spec.pool;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        let located = match storage.activate_volume(&pool, volume.id, format).await {
            Ok(()) => disk_location(&pool, mount_root, volume.id, format),
            Err(e) => Err(e),
```

을 다음으로 바꾼다:

```rust
        let located = match storage.activate_volume(&pool, volume.id, format).await {
            Ok(()) => {
                let located = disk_location(&pool, mount_root, volume.id, format);
                activated.push((pool.clone(), volume.id, format));
                located
            }
            Err(e) => Err(e),
```

`crates/opl-reconciler/src/worker.rs` 전체:

```rust
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, EventKind, Hypervisor, RuntimeStatus, StopMode};
use opl_types::runtime::VmRuntimeSpec;
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::{PowerState, RestartPolicy, Vm, VmPhase, VmSpec, VmStatus};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::backoff::{FailureTracker, Next};
use crate::config::ReconcilerConfig;
use crate::reconciler::Restores;
use crate::resolve::{ResolveError, running_spec, runtime_spec};

/// 실행 중이던 VM이 사라졌을 때 그 이유를 알리는 종료 이벤트를 기다리는 시간.
/// 이벤트보다 재동기화가 먼저 오면, 게스트가 스스로 끈 VM을 다시 켰다가 끄게 된다.
/// QEMU 드라이버는 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 최대 5초(`EXIT_GRACE`) 기다린 다음에
/// 크래시를 알리므로 그보다 길게 잡는다. 짧으면 느리게 끝난 크래시를 두 번 세게 된다.
const EVENT_GRACE: Duration = Duration::from_secs(6);

/// 작업자를 깨우는 이유.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Trigger {
    /// spec이 바뀌었거나 전체 재동기화. 원하는 상태와 실제 상태를 다시 맞춘다.
    Sync,
    /// 하이퍼바이저가 알린 수명 이벤트
    Event(EventKind),
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다.
    ForceStop,
}

#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Driver(#[from] DriverError),
}

/// VM 하나를 맡는 작업자. 이 VM에 대한 드라이버 호출은 모두 이 태스크에서만 일어난다.
///
/// 매번 로컬 저장소에서 VM을 다시 읽고 실제 상태를 조회해 맞추므로(level-triggered),
/// 같은 트리거를 여러 번 받아도 안전하다. 이벤트는 "게스트가 스스로 꺼졌다"처럼 실제 상태만
/// 봐서는 알 수 없는 사실을 알릴 때만 쓴다.
pub(crate) struct Worker {
    id: Uuid,
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
    /// 원하는 상태가 다시 running으로 바뀌면 취소하므로, 새 종료는 늘 ACPI부터 다시 시작한다.
    stop_requested: Option<Instant>,
    /// 이 작업자가 마지막으로 종료를 요청한(ACPI 또는 강제) 시각. 뒤이은 종료 이벤트가 우리
    /// 요청의 결과인지 가린다. 요청을 취소했거나 원하는 상태가 다시 running으로 바뀌어도
    /// 게스트(또는 강제 종료)는 요청대로 꺼질 수 있으므로 `acpi_timeout` 동안 유효하다.
    stopped_by_us: Option<Instant>,
    /// 다음 단계에서 ACPI 응답을 기다리지 않고 강제 종료한다. 원하는 상태가 stopped일 때만 켜고,
    /// 그 뒤 running으로 바뀌면 reconcile이 끈다.
    force_requested: bool,
    failures: FailureTracker,
    /// 백오프 중이면 이 시각까지 시작하지 않는다. status에 보일 이유를 함께 둔다.
    backoff: Option<(Instant, String)>,
    /// spec.power를 stopped로 바꿔야 한다(게스트 종료, 재시작 포기). 안쪽 값은 status에 남길 오류다.
    /// 쓰기에 실패하면 남겨 두었다가 다음 단계에서 다시 쓴다.
    power_off: Option<Option<String>>,
    /// 마지막으로 조회했을 때 VM이 실행 중이었는지
    was_running: bool,
    /// 사라진 VM의 종료 이벤트를 이 시각까지 기다린다.
    awaiting_event: Option<Instant>,
    /// 디스크 볼륨이 켜져 있을 수 있다(시작하려고 켰거나 VM이 도는 것을 봤다). QEMU가 끝난 것을
    /// 확인하면 끈다([`Worker::deactivate_disks`]).
    disks_active: bool,
    wake_at: Option<Instant>,
}

impl Worker {
    pub(crate) fn new(
        id: Uuid,
        cluster: Arc<Cluster>,
        hv: Arc<dyn Hypervisor>,
        config: Arc<ReconcilerConfig>,
        restores: Restores,
    ) -> Self {
        Self {
            id,
            cluster,
            hv,
            config,
            restores,
            stop_requested: None,
            stopped_by_us: None,
            force_requested: false,
            failures: FailureTracker::default(),
            backoff: None,
            power_off: None,
            was_running: false,
            awaiting_event: None,
            disks_active: false,
            wake_at: None,
        }
    }

    /// 보내는 쪽이 모두 사라지면 남은 트리거를 처리하고 끝난다. `previous`는 같은 VM을 맡았던
    /// 작업자다. 같은 VM에 대한 드라이버 호출이 겹치지 않도록 그것이 끝난 뒤에 시작한다.
    pub(crate) async fn run(
        mut self,
        previous: Option<JoinHandle<()>>,
        mut rx: mpsc::UnboundedReceiver<Trigger>,
    ) {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        loop {
            let trigger = match self.wake_at {
                Some(at) => tokio::select! {
                    t = rx.recv() => t,
                    () = tokio::time::sleep_until(at) => Some(Trigger::Sync),
                },
                None => rx.recv().await,
            };
            let Some(trigger) = trigger else { return };
            self.wake_at = None;
            if let Err(e) = self.step(trigger).await {
                tracing::warn!(vm = %self.id, error = %e, "reconcile failed; will retry");
                self.wake(Instant::now() + self.config.retry_interval);
            }
        }
    }

    fn wake(&mut self, at: Instant) {
        self.wake_at = Some(self.wake_at.map_or(at, |w| w.min(at)));
    }

    async fn step(&mut self, trigger: Trigger) -> Result<(), StepError> {
        let node = self.config.node;
        let vm = self.cluster.get::<VmSpec>(self.id)?;
        let Some(mut vm) = vm.filter(|vm| vm.spec.node == Some(node)) else {
            // 삭제되었거나 다른 노드로 배정되었다. 돌고 있더라도 멋대로 끄지 않는다.
            if self.hv.inspect(self.id).await?.is_some() {
                tracing::warn!(vm = %self.id, "VM is running but not assigned to this node; leaving it alone");
            }
            return Ok(());
        };
        match trigger {
            Trigger::Event(kind) => self.on_event(&vm, kind),
            // running을 원할 때 온 명령은 버린다. 남겨 두면 나중의 종료가 ACPI 없이 강제 종료된다.
            Trigger::ForceStop if vm.spec.power == PowerState::Stopped => {
                self.force_requested = true
            }
            Trigger::ForceStop => {}
            Trigger::Sync => {}
        }
        if self.power_off.is_none() {
            let status = self.reconcile(&mut vm).await?;
            if self.power_off.is_none() {
                if status != vm.status {
                    self.write_status(status, vm).await?;
                }
                return Ok(());
            }
        }
        self.write_power_off(vm).await
    }

    fn on_event(&mut self, vm: &Vm, kind: EventKind) {
        let now = Instant::now();
        if kind != EventKind::Started {
            self.was_running = false;
            self.awaiting_event = None;
            self.stop_requested = None;
        }
        let requested = match kind {
            EventKind::Started => return,
            EventKind::Stopped { .. } | EventKind::Crashed => self
                .stopped_by_us
                .take()
                .is_some_and(|at| now < at + self.config.acpi_timeout),
        };
        // 원하던 대로 꺼졌다. 강제 종료의 결과로 Crashed가 와도 실패로 세지 않는다.
        if vm.spec.power == PowerState::Stopped {
            return;
        }
        match kind {
            // 우리가 요청한 종료가, 원하는 상태가 다시 running으로 바뀐 뒤에 끝났다. 다시 시작한다.
            // 강제 종료의 결과로 Crashed가 와도 크래시로 보지 않는다.
            _ if requested => {}
            // 게스트가 스스로 꺼졌다(설계 5.3). `guest_initiated`는 opld가 요청한 ACPI 종료에서도
            // true이므로 우리가 요청했는지로 먼저 가린다.
            EventKind::Stopped {
                guest_initiated: true,
            } => self.power_off = Some(None),
            // 우리가 요청하지 않았고 게스트의 결정도 아니다(호스트의 SIGTERM 등). 원하는 상태는
            // 그대로 두므로 실패로 세지 않고 다시 시작한다.
            EventKind::Stopped {
                guest_initiated: false,
            } => {}
            EventKind::Crashed => match vm.spec.restart {
                RestartPolicy::Never => self.power_off = Some(Some("VM crashed".into())),
                RestartPolicy::OnFailure => self.record_failure("VM crashed".into()),
            },
            EventKind::Started => {}
        }
    }

    fn record_failure(&mut self, reason: String) {
        let now = Instant::now();
        match self.failures.record(now, &self.config.restart) {
            Next::RetryAfter(delay) => {
                self.backoff = Some((now + delay, format!("{reason}; retrying in {delay:?}")));
            }
            Next::GiveUp => {
                let n = self.config.restart.max_failures;
                self.power_off = Some(Some(format!("{reason}; gave up after {n} failures")));
            }
        }
    }

    /// 원하는 상태를 향해 한 걸음 움직이고, 기록해야 할 status를 돌려준다. 그 전에 status를 먼저
    /// 기록했으면 `vm`을 다시 읽은 것으로 바꾼다.
    async fn reconcile(&mut self, vm: &mut Vm) -> Result<VmStatus, StepError> {
        let actual = self.hv.inspect(self.id).await?;
        let mut status = vm.status.clone();
        status.node = Some(self.config.node);
        let now = Instant::now();
        if std::mem::replace(&mut self.was_running, actual.is_some()) && actual.is_none() {
            self.awaiting_event = Some(now + EVENT_GRACE);
        }
        if actual.is_some() {
            self.disks_active = true;
        }
        match (vm.spec.power, actual) {
            (PowerState::Running, Some(state)) => {
                // 진행 중인 종료 요청을 취소한다. 게스트가 이미 받은 요청대로 꺼지면
                // `stopped_by_us`로 가려 다시 시작한다.
                self.stop_requested = None;
                self.force_requested = false;
                status.phase = VmPhase::Running;
                status.pid = Some(state.pid);
                status.last_error = None;
                if state.status != RuntimeStatus::Running {
                    // opld가 기동 도중(cont 전)이나 스냅샷으로 멈춘 채 죽었거나 QMP로 다룰 수 없다.
                    // 붙어 있으면 디스크를 다시 해석하지 않고(LVM이 응답하지 않을 수 있다) 이어서
                    // 실행하고, 아니면 start()가 다시 붙는다. 실패해도 돌고 있는 VM이므로 끄거나
                    // 실패로 세지 않는다.
                    let continued = match self.hv.resume_left_stopped(self.id).await {
                        Ok(true) => Ok(()),
                        Ok(false) => self.reattach(vm).await,
                        Err(e) => Err(e.to_string()),
                    };
                    if let Err(e) = continued {
                        tracing::warn!(vm = %self.id, error = %e, "VM is unresponsive; will retry");
                        status.last_error = Some(format!("VM is unresponsive: {e}"));
                        self.wake(now + self.config.retry_interval);
                    }
                }
            }
            // 방금까지 돌던 VM이 사라졌다. 왜 꺼졌는지(게스트 종료, 크래시) 이벤트로 안 뒤에 움직인다.
            // 기다려도 오지 않으면(이벤트 유실) 원하는 상태대로 다시 시작한다.
            (PowerState::Running, None)
                if self.awaiting_event.is_some_and(|deadline| now < deadline) =>
            {
                self.wake(self.awaiting_event.unwrap());
            }
            (PowerState::Running, None) => {
                self.awaiting_event = None;
                self.force_requested = false;
                self.deactivate_disks(vm).await;
                if self.backoff.as_ref().is_some_and(|(at, _)| *at <= now) {
                    self.backoff = None;
                }
                if self.backoff.is_none() && vm.status.phase == VmPhase::Running {
                    // 저장된 phase가 Running인 채 다시 켜면(QEMU가 스스로 끝났다) 그사이 API·Task
                    // 실행기가 실행 중 VM으로 보고 디스크를 늘린다(공유 LVM은 늘린 부분을 0으로 채우기
                    // 전에 QEMU가 연다). 디스크를 해석하기 전에 Starting을 먼저 기록해, 먼저 들어온
                    // 크기 늘리기 Task는 resolve가 보고 나중 것은 API·실행기가 거절하게 한다. 기록하지
                    // 못해도(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) VM은 다시 켠다(설계 10절
                    // 불변식 4). 기록이 시간 초과로 실패한 사이 과반수가 돌아오면 좁은 틈이 남는다.
                    let mut starting = status.clone();
                    starting.phase = VmPhase::Starting;
                    starting.pid = None;
                    match self.write_status(starting, vm.clone()).await {
                        Ok(()) => match self.cluster.get::<VmSpec>(self.id)? {
                            Some(current) => *vm = current,
                            None => return Ok(status),
                        },
                        Err(e) => {
                            tracing::warn!(vm = %self.id, error = %e, "cannot record starting phase; restarting anyway");
                        }
                    }
                }
                if self.backoff.is_none() && self.start(vm).await? {
                    // 이제 돌고 있다. 곧바로 꺼져도 이벤트보다 먼저 온 재동기화가 다시 시작하지
                    // 않고 이벤트를 기다리게 한다.
                    self.was_running = true;
                    match self.hv.inspect(self.id).await? {
                        Some(state) => {
                            status.phase = VmPhase::Running;
                            status.pid = Some(state.pid);
                            status.last_error = None;
                        }
                        // 시작하자마자 꺼졌다. 뒤따르는 이벤트에서 다시 맞춘다.
                        None => {
                            self.was_running = false;
                            let deadline = Instant::now() + EVENT_GRACE;
                            self.awaiting_event = Some(deadline);
                            self.wake(deadline);
                            status.phase = VmPhase::Starting;
                            status.pid = None;
                        }
                    }
                }
                if let Some((at, reason)) = self.backoff.clone() {
                    self.wake(at);
                    status.phase = VmPhase::Starting;
                    status.pid = None;
                    status.last_error = Some(reason);
                }
            }
            (PowerState::Stopped, Some(state)) => {
                self.backoff = None;
                match self.stop_requested {
                    _ if self.force_requested => {
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.force_requested = false;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    None => {
                        // 요청이 실패해도(QMP로 다룰 수 없는 VM) acpi_timeout 뒤 강제 종료한다.
                        self.stop_requested = Some(now);
                        self.stopped_by_us = Some(now);
                        self.wake(now + self.config.acpi_timeout);
                        if let Err(e) = self.hv.stop(self.id, StopMode::Acpi).await {
                            tracing::warn!(vm = %self.id, error = %e, "ACPI shutdown request failed; will force after timeout");
                        }
                    }
                    Some(at) if now >= at + self.config.acpi_timeout => {
                        tracing::warn!(vm = %self.id, "guest ignored ACPI shutdown; forcing");
                        self.hv.stop(self.id, StopMode::Force).await?;
                        self.stop_requested = None;
                        self.stopped_by_us = Some(Instant::now());
                        status.phase = VmPhase::Stopped;
                        status.pid = None;
                        return Ok(status);
                    }
                    Some(at) => self.wake(at + self.config.acpi_timeout),
                }
                status.phase = VmPhase::Stopping;
                status.pid = Some(state.pid);
            }
            (PowerState::Stopped, None) => {
                self.stop_requested = None;
                self.force_requested = false;
                self.awaiting_event = None;
                self.deactivate_disks(vm).await;
                self.backoff = None;
                self.failures.clear();
                // 포기해서 멈춘 VM은 사용자가 다시 시작할 때까지 Failed로 남긴다.
                if status.phase != VmPhase::Failed {
                    status.phase = VmPhase::Stopped;
                }
                status.pid = None;
            }
        }
        Ok(status)
    }

    /// VM을 시작한다. 시작하지 못하고 실패를 기록했으면(백오프 또는 포기) false.
    async fn start(&mut self, vm: &Vm) -> Result<bool, StepError> {
        let spec = match self.resolve(vm).await {
            Ok(spec) => spec,
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았거나(NFS 서버가 멈춤, 마운트 전 등) 디스크를 늘리는
            // 중이다. 끝나면 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ (ResolveError::PoolUnavailable(..) | ResolveError::VolumeResizing(_))) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(false);
            }
            // 참조가 틀렸거나, 이 노드가 지원하지 않는 풀(NFS on macOS)이거나, 저장소가 볼륨을
            // 내주지 못했다. spec을 고치거나 VM을 옮겨야 하므로 다시 시도하지 않는다.
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
                return Ok(false);
            }
        };
        self.disks_active = true;
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        let started = self.hv.start(&spec, restore.as_deref()).await;
        if started.is_err() {
            self.deactivate_disks(vm).await;
        }
        match (started, restore) {
            (Ok(()), _) => Ok(true),
            // 태그 없이 다시 시도하면 스냅샷이 아닌 처음부터 부팅하게 되므로 일시 오류라도 멈춘다.
            (Err(e), Some(tag)) => {
                self.power_off = Some(Some(format!("failed to restore snapshot {tag}: {e}")));
                Ok(false)
            }
            (Err(e), None) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            (Err(e), None) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
                Ok(false)
            }
        }
    }

    /// 켜져 있을 수 있는 디스크 볼륨을 끈다(QEMU가 끝난 뒤). 끄지 못하면 경고만 남긴다(켜진 채 남은
    /// 볼륨은 Task 실행기의 정리가 다시 끈다).
    async fn deactivate_disks(&mut self, vm: &Vm) {
        if !std::mem::take(&mut self.disks_active) {
            return;
        }
        for disk in &vm.spec.disks {
            let found = self
                .cluster
                .get_by_name::<VolumeSpec>(&disk.volume)
                .and_then(|volume| {
                    let Some(volume) = volume else {
                        return Ok(None);
                    };
                    let pool = self
                        .cluster
                        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?;
                    Ok(pool.map(|pool| (volume, pool)))
                });
            let (volume, pool) = match found {
                Ok(Some(found)) => found,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
                    continue;
                }
            };
            if let Err(e) = self
                .config
                .storage
                .deactivate_volume(&pool, volume.id, volume.spec.format)
                .await
            {
                tracing::warn!(vm = %self.id, volume = %disk.volume, error = %e, "cannot deactivate volume");
            }
        }
    }

    /// VM의 런타임 정의. 디스크 볼륨을 활성화한다.
    async fn resolve(&self, vm: &Vm) -> Result<VmRuntimeSpec, ResolveError> {
        let c = &self.config;
        runtime_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage).await
    }

    /// 살아 있는 VM에 다시 붙거나 이어서 실행한다(`start`는 멱등이다). 참조 해석 오류도 돌려준다.
    /// QEMU가 이미 디스크를 열었으므로 크기 늘리기 Task를 기다리지 않는다.
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let c = &self.config;
        let spec = running_spec(&self.cluster, vm, c.node, &c.mount_root, &*c.storage)
            .await
            .map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
    }

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)과 `schedule_error`는
    /// Task 실행기·예약 작업이 쓰는 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이
    /// 누가 썼으면 다시 읽어 그 값으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
            status.schedule_error = vm.status.schedule_error.clone();
            match self
                .cluster
                .update_status_checked::<VmSpec>(self.id, vm.revision, &status)
                .await
            {
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                    match self.cluster.get::<VmSpec>(self.id)? {
                        Some(current) => vm = current,
                        None => return Ok(()),
                    }
                }
                r => return r.map(|_| ()).map_err(Into::into),
            }
        }
    }

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, mut vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
        if vm.spec.power == PowerState::Running {
            let mut spec = vm.spec.clone();
            spec.power = PowerState::Stopped;
            // 이 단계에서 읽은 뒤 spec이 바뀌었으면 RevisionMismatch로 거부된다. 다음 단계에서
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
                .await?;
        }
        let status = VmStatus {
            phase: if error.is_some() {
                VmPhase::Failed
            } else {
                VmPhase::Stopped
            },
            node: Some(self.config.node),
            pid: None,
            last_error: error,
            snapshot: vm.status.snapshot,
            schedule_error: vm.status.schedule_error.clone(),
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
        self.backoff = None;
        self.failures.clear();
        Ok(())
    }
}
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust

use std::time::Duration;

use common::{Env, vm};
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::node::{Node, NodeSpec, PoolState};
```

을 다음으로 바꾼다:

```rust

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{Env, vm};
use opl_driver::{
    Capacity, DriverError, Hypervisor, SourceFormat, StopMode, Storage, VolumeInfo, VolumeSource,
};
use opl_reconciler::ReconcilerConfig;
use opl_types::node::{Node, NodeSpec, PoolState};
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.get(vm.id).status.schedule_error, Some(busy()));
}

/// 볼륨 켜기·끄기를 적는 저장소. 켜기는 늘 성공한다. reconciler는 켜기·끄기만 부른다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(&'static str, Uuid)>>,
    /// 켜기 때마다 이 VM의 저장된 phase를 `phases`에 적는다.
    watch: std::sync::OnceLock<(Arc<opl_cluster::Cluster>, Uuid)>,
    phases: Mutex<Vec<VmPhase>>,
}

impl Activations {
    fn deactivated(&self) -> Vec<Uuid> {
        let calls = self.calls.lock().unwrap();
        calls
            .iter()
            .filter(|(call, _)| *call == "deactivate")
            .map(|&(_, id)| id)
            .collect()
    }
}

#[async_trait]
impl Storage for Activations {
    async fn activate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("activate", id));
        if let Some((cluster, vm)) = self.watch.get() {
            let vm = cluster.get::<VmSpec>(*vm).unwrap().unwrap();
            self.phases.lock().unwrap().push(vm.status.phase);
        }
        Ok(())
    }
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.calls.lock().unwrap().push(("deactivate", id));
        Ok(())
    }
    async fn create_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        unreachable!()
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        unreachable!()
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        unreachable!()
    }
    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        unreachable!()
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        unreachable!()
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: u64,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        unreachable!()
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!()
    }
}

/// `storage`로 볼륨을 켜고 끄는 reconciler와, dir 풀 볼륨 `root`를 디스크로 쓰는 VM `web`(아직
/// 만들지 않았다)
async fn disk_vm(storage: Arc<Activations>) -> (Env, Volume, opl_types::vm::Vm) {
    disk_vm_with(storage, |_| {}).await
}

/// [`disk_vm`]. 설정을 `tweak`으로 바꾼다.
async fn disk_vm_with(
    storage: Arc<Activations>,
    tweak: impl FnOnce(&mut ReconcilerConfig),
) -> (Env, Volume, opl_types::vm::Vm) {
    let env = Env::start_with(|c| {
        c.storage = storage;
        tweak(c);
    })
    .await;
    let pool: StoragePool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let root: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&root).await.unwrap();
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    (env, root, vm)
}

/// 기다리는 동안 `storage`가 볼륨을 `want`대로 끄기를 기다린다.
async fn wait_deactivated(storage: &Activations, want: &[Uuid]) {
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.deactivated() != want {
        assert!(
            std::time::Instant::now() < deadline,
            "deactivated {:?}, want {want:?}",
            storage.deactivated()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// VM이 멈추면(사용자가 끔, 크래시) QEMU가 끝난 뒤 디스크 볼륨을 한 번 끈다(공유 LVM의 배타
/// 활성화를 풀어 다른 노드가 쓸 수 있게). 실행 중에는 끄지 않는다.
#[tokio::test]
async fn stopped_vm_deactivates_its_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(*storage.calls.lock().unwrap(), [("activate", root.id)]);

    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    wait_deactivated(&storage, &[root.id]).await;

    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    env.hv.crash(vm.id);
    env.wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    wait_deactivated(&storage, &[root.id, root.id]).await;
    // 멈춘 VM을 다시 맞춰도 또 끄지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(storage.deactivated(), [root.id, root.id]);
}

/// 디스크 볼륨의 크기 늘리기 Task가 끝나지 않았으면 디스크를 켜지 않고 시작을 미룬다(실패로 세지
/// 않는다). Task가 끝나면 시작한다.
#[tokio::test]
async fn start_waits_for_a_resize_of_its_disk() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 2 << 30,
            },
            target: root.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    env.create(&vm).await;
    let waiting = env
        .wait(vm.id, "waiting for the resize", |v| {
            v.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("being resized"))
        })
        .await;
    assert_eq!(waiting.status.phase, VmPhase::Starting);
    assert_eq!(waiting.spec.power, PowerState::Running);
    // 여러 번 다시 해 본 뒤에도 포기하지 않는다.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still = env.get(vm.id);
    assert_eq!(still.spec.power, PowerState::Running);
    assert_eq!(still.status.phase, VmPhase::Starting);
    assert!(storage.calls.lock().unwrap().is_empty());
    assert!(env.hv.inspect(vm.id).await.unwrap().is_none());

    let status = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task.id, &status)
        .await
        .unwrap();
    env.wait(vm.id, "running", running).await;
    assert_eq!(*storage.calls.lock().unwrap(), [("activate", root.id)]);
}

/// 이미 도는 VM에 다시 붙을 때는 크기 늘리기 Task를 기다리지 않고 디스크를 해석한다(QEMU가 이미
/// 장치를 열었다). 기다리면 실행 중 VM의 크기 늘리기 동안 다시 붙지 못한다.
#[tokio::test]
async fn reattach_does_not_wait_for_a_resize() {
    let storage = Arc::new(Activations::default());
    // 응답하지 않는 VM은 재동기화로 알아채고 다시 붙는다.
    let (env, root, vm) = disk_vm_with(storage.clone(), |c| {
        c.resync_interval = Duration::from_millis(100)
    })
    .await;
    env.create(&vm).await;
    let pid = env.wait(vm.id, "running", running).await.status.pid;

    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 2 << 30,
            },
            target: root.id,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    // 가짜 하이퍼바이저는 응답하지 않는 VM에 다시 붙지 못한다(디스크를 해석한 뒤 실패한다).
    env.hv.set_unresponsive(vm.id, true);
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.calls.lock().unwrap().len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "disks not resolved: {:?}",
            env.get(vm.id).status
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got = env
        .wait(vm.id, "reported unresponsive", |v| {
            v.status.last_error.is_some()
        })
        .await;
    let error = got.status.last_error.unwrap();
    assert!(!error.contains("being resized"), "{error}");
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.pid, pid);
    assert!(
        storage
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|&call| call == ("activate", root.id))
    );
}

/// 돌던 QEMU가 끝나 곧바로(백오프 없이) 다시 켤 때 디스크를 해석하기 전에 phase를 Starting으로
/// 먼저 기록한다. 저장된 phase가 Running인 채면 그사이 API·Task 실행기가 실행 중 VM으로 보고 디스크를
/// 늘린다(공유 LVM은 늘린 부분을 0으로 채우기 전에 QEMU가 연다).
#[tokio::test]
async fn restart_records_starting_before_resolving_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.create(&vm).await;
    let before = env.wait(vm.id, "running", running).await.status.pid;
    storage
        .watch
        .set((env.cluster.clone(), vm.id))
        .unwrap_or_else(|_| unreachable!());

    // 호스트의 SIGTERM처럼 우리가 요청하지 않은 종료
    env.hv.stop(vm.id, StopMode::Force).await.unwrap();
    let got = env
        .wait(vm.id, "restarted", |v| running(v) && v.status.pid != before)
        .await;
    assert_eq!(got.status.last_error, None);
    assert_eq!(*storage.phases.lock().unwrap(), [VmPhase::Starting]);
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [
            ("activate", root.id),
            ("deactivate", root.id),
            ("activate", root.id)
        ]
    );
}

/// 디스크를 켠 뒤 QEMU가 시작하지 못하면 켠 볼륨을 끈다.
#[tokio::test]
async fn failed_start_deactivates_its_disks() {
    let storage = Arc::new(Activations::default());
    let (env, root, vm) = disk_vm(storage.clone()).await;
    env.hv
        .fail_next_start(DriverError::StartFailed("no firmware".into()));
    env.create(&vm).await;
    env.wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    wait_deactivated(&storage, &[root.id]).await;
    assert_eq!(
        *storage.calls.lock().unwrap(),
        [("activate", root.id), ("deactivate", root.id)]
    );
}
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{DiskBus, DiskSpec, MacAddr, NicModel, NicSpec, Vm};
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{DiskBus, DiskSpec, MacAddr, NicModel, NicSpec, Vm};
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// 활성화한 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화만 부른다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(String, Uuid, ImageFormat)>>,
    fail: Mutex<Option<fn() -> DriverError>>,
```

을 다음으로 바꾼다:

```rust

/// 활성화한 볼륨과 끈 볼륨을 적는 저장소. `fail`이 있으면 활성화가 그 오류로 실패한다.
/// resolve는 활성화와 (실패했을 때) 끄기만 부른다.
#[derive(Default)]
struct Activations {
    calls: Mutex<Vec<(String, Uuid, ImageFormat)>>,
    deactivated: Mutex<Vec<Uuid>>,
    fail: Mutex<Option<fn() -> DriverError>>,
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
        }
    }
```

을 다음으로 바꾼다:

```rust
        }
    }
    async fn deactivate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        self.deactivated.lock().unwrap().push(id);
        Ok(())
    }
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            backend: NicBackend::User,
        }]
    );
}

/// 디스크로 쓰기 전에 저장소가 볼륨을 활성화한다. 활성화가 실패하면 다시 시도할 `PoolUnavailable`이고,
```

을 다음으로 바꾼다:

```rust
            backend: NicBackend::User,
        }]
    );
}

/// 풀다가 실패하면 그때까지 활성화한 볼륨을 끈다(VM을 시작하지 않으니 켜 둘 까닭이 없다).
/// 성공하면 끄지 않는다.
#[tokio::test]
async fn failed_resolve_deactivates_the_volumes_it_activated() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data) = (volume("root", "local"), volume("data", "local"));
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));

    // 둘째 디스크의 볼륨이 없다.
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::VolumeNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id]);

    // 디스크를 모두 활성화한 뒤 NIC에서 실패해도 모두 끈다.
    cluster.create(&data).await.unwrap();
    vm.spec.nics.push(nic("missing", None));
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::NetworkNotFound(_)), "{err:?}");
    assert_eq!(*storage.deactivated.lock().unwrap(), [root.id, data.id]);

    vm.spec.nics.clear();
    let storage = Activations::default();
    runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert!(storage.deactivated.lock().unwrap().is_empty());
}

/// 디스크 볼륨에 끝나지 않은 크기 늘리기 Task가 있으면 그 볼륨을 켜지 않고 기다릴
/// `VolumeResizing`이다. 시작과 겹치면 QEMU가 늘리는 중인 볼륨(공유 LVM은 0으로 채우기 전의 늘린
/// 부분)을 열 수 있다. 다른 볼륨의 Task, 끝난 Task와 크기 늘리기가 아닌 Task는 막지 않는다.
#[tokio::test]
async fn waits_for_resizes_of_its_disks() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    cluster.create(&pool("local", vec![node])).await.unwrap();
    let (root, data, other) = (
        volume("root", "local"),
        volume("data", "local"),
        volume("other", "local"),
    );
    for v in [&root, &data, &other] {
        cluster.create(v).await.unwrap();
    }
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    vm.spec.disks.push(disk("data"));
    let resize = TaskOp::VolumeResize {
        size_bytes: 2 << 30,
    };
    for (op, target, state) in [
        (resize.clone(), other.id, TaskState::Running),
        (resize.clone(), data.id, TaskState::Failed),
        (TaskOp::VolumeDelete, data.id, TaskState::Pending),
    ] {
        let mut task: Task = Resource::new("", TaskSpec { op, target, node });
        task.name = task.id.to_string();
        task.status = TaskStatus { state, error: None };
        cluster.create(&task).await.unwrap();
    }
    resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();

    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: resize,
            target: data.id,
            node,
        },
    );
    task.name = task.id.to_string();
    cluster.create(&task).await.unwrap();
    for state in [TaskState::Pending, TaskState::Running] {
        let status = TaskStatus { state, error: None };
        cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
        let storage = Activations::default();
        let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ResolveError::VolumeResizing(ref v) if v == "data"),
            "{err:?}"
        );
        assert!(err.to_string().contains("being resized"), "{err}");
        let activated: Vec<Uuid> = storage.calls.lock().unwrap().iter().map(|c| c.1).collect();
        assert!(!activated.contains(&data.id), "{activated:?}");
    }

    let status = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    cluster
        .update_status::<TaskSpec>(task.id, &status)
        .await
        .unwrap();
    resolve(&cluster, &vm, node, Path::new(MNT)).await.unwrap();
}

/// 디스크로 쓰기 전에 저장소가 볼륨을 활성화한다. 활성화가 실패하면 다시 시도할 `PoolUnavailable`이고,
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 650개, 실패 0

Run: `cargo test -p opl-api`
Expected: PASS

Run: `cargo test -p opl-reconciler`
Expected: PASS

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(reconciler): 공유 LVM 볼륨 활성화 수명(멈춤·시작 실패 뒤 끄기, 남은 활성화 정리)과 Task 노드"
```

---

### Task 5: 실제 공유 LVM 시험(lvm-shared-it)과 CI

기능 `lvm-shared-it`(opl-driver, opld)과 Linux 전용 `crates/opl-driver/tests/lvm_shared_storage.rs`(0 채우기·배타 활성화·크기·지우기, 열린 볼륨은 꺼지지 않음, vmdk 가져오기, 백엔드 사이 복사, 늘어난 꼬리의 0 채우기와 남은 태그 — 한 번에 하나씩), `crates/opld/tests/lvm_shared.rs`(로그아웃·잠금 멈춤 뒤 opld가 처음부터 로그인, 공유 LVM 디스크 VM, 재시작 때 남은 활성화 정리, 멈춘 뒤 LV 꺼짐, 복제, 스냅샷 409). `OPL_TEST_LVM_SHARED=VG`, `OPL_TEST_ISCSI=PORTAL,TARGET`. CI `linux-kvm`에 targetcli-fb(파일 LUN, 데모 모드 ACL)·open-iscsi·sanlock·lvm2-lockd(softdog, `use_lvmlockd = 1`, `host_id = 1`)와 공유 VG `oplshared`.

**Files:**
- Create: `crates/opl-driver/tests/lvm_shared_storage.rs`
- Create: `crates/opld/tests/lvm_shared.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opld/Cargo.toml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
          sudo ceph --id opltest --conf /etc/opl-ceph/ceph.conf df detail >/dev/null
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

을 다음으로 바꾼다:

```yaml
          sudo ceph --id opltest --conf /etc/opl-ceph/ceph.conf df detail >/dev/null
      # 공유 LVM 풀 시험이 LV를 만들 공유 VG `oplshared`. targetcli(LIO)로 파일 백스토어 LUN 하나를
      # 내보내는 루프백 iSCSI 타깃(데모 모드 ACL, 인증 없음)을 만들어 open-iscsi로 로그인하고,
      # lvmlockd와 sanlock(softdog 감시, host_id 1)으로 그 LUN 위에 공유 VG를 만든다. lvmlockd는 호스트
      # 전체에 켜지지만 로컬 VG(LVM-thin 시험)는 그대로 쓴다. E2E는 잠금을 멈추고 로그아웃한 뒤 opld가
      # 다시 로그인하고 잠금을 시작하게 한다.
      - name: iSCSI target and a shared (lvmlockd) VG
        timeout-minutes: 10
        env:
          IQN: iqn.2003-01.org.linux-iscsi.opl:shared
        run: |
          sudo apt-get update
          sudo apt-get install -y targetcli-fb open-iscsi lvm2-lockd sanlock
          sudo modprobe target_core_mod
          sudo modprobe iscsi_target_mod
          sudo systemctl start iscsid
          truncate -s 2G "$RUNNER_TEMP/oplshared.img"
          sudo targetcli /backstores/fileio create oplshared "$RUNNER_TEMP/oplshared.img"
          sudo targetcli /iscsi create "$IQN"
          sudo targetcli "/iscsi/$IQN/tpg1/luns" create /backstores/fileio/oplshared
          sudo targetcli "/iscsi/$IQN/tpg1" set attribute authentication=0 demo_mode_write_protect=0 generate_node_acls=1 cache_dynamic_acls=1
          sudo targetcli ls
          sudo iscsiadm -m discovery -t sendtargets -p 127.0.0.1
          sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --login
          sudo udevadm settle
          sudo iscsiadm -m session
          DEV="/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"
          ls -l "$DEV"
          sudo modprobe softdog
          sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf
          printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf
          sudo lvmconfig global/use_lvmlockd local/host_id
          sudo systemctl start wdmd sanlock lvmlockd
          sudo vgcreate --shared oplshared "$DEV"
          sudo vgchange --lockstart oplshared
          sudo vgs -o vg_name,vg_lock_type,vg_size,vg_free oplshared
          sudo lvs opltest
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it,opl-driver/lvm-shared-it,opld/lvm-shared-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

`.github/workflows/ci.yml`에서

```yaml
          OPL_TEST_RBD_CONF: /etc/opl-ceph/ceph.conf
```

을 다음으로 바꾼다:

```yaml
          OPL_TEST_RBD_CONF: /etc/opl-ceph/ceph.conf
          OPL_TEST_LVM_SHARED: oplshared
          OPL_TEST_ISCSI: 127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared
```

`crates/opl-driver/Cargo.toml`에서

```toml
rbd-it = []
```

을 다음으로 바꾼다:

```toml
rbd-it = []
# 실제 공유 LVM 풀(lvmlockd 공유 VG)의 통합 테스트. Linux에서 root, lvm2-lockd와 sanlock(또는 dlm),
# qemu-img, qemu-io와 잠금을 시작한 공유 VG(`OPL_TEST_LVM_SHARED=VG`, 테스트가 LV를 만들고 지워도 된다)가
# 필요하다.
lvm-shared-it = []
```

`crates/opl-driver/tests/lvm_shared_storage.rs`를 만든다:

```rust
//! 실제 공유 LVM 풀의 저장소 통합 테스트. 관리자가 만든 공유 VG(`OPL_TEST_LVM_SHARED=VG`, lvmlockd의
//! `vgcreate --shared`, 잠금을 시작한 것)에 볼륨 LV를 만들어 0으로 채우고, 배타 활성화하고 끄고,
//! 늘리고(늘린 부분을 0으로 채운다), 가져오고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_LVM_SHARED=oplshared cargo test -p opl-driver --features lvm-shared-it --test lvm_shared_storage`
//! Linux에서 root, lvm2-lockd와 sanlock(또는 dlm), qemu-img, qemu-io가 필요하다.
#![cfg(all(feature = "lvm-shared-it", target_os = "linux"))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use opl_driver::{
    DirStorage, DiskLocation, DriverError, LvmSharedStorage, SourceFormat, Storage, Storages,
    SystemLvm,
};
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// 이 파일의 시험은 한 VG를 함께 쓰므로 하나씩 돈다. 늘린 부분의 0 채우기 시험은 지운 LV의 익스텐트에
/// 늘린 부분이 놓이는지 보는데, 다른 시험이 함께 돌면 그 자리를 먼저 차지할 수 있다.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `OPL_TEST_LVM_SHARED`의 공유 VG
fn vg() -> String {
    std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG")
}

fn shared_pool() -> StoragePool {
    Resource::new(
        "shared",
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: vg(),
                iscsi: None,
            },
            nodes: vec![],
            shared: true,
        },
    )
}

fn dir_pool(dir: &Path) -> StoragePool {
    Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: dir.to_path_buf(),
            },
            nodes: vec![],
            shared: false,
        },
    )
}

fn storage() -> LvmSharedStorage {
    LvmSharedStorage::new("qemu-img", Arc::new(SystemLvm))
}

fn lv(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 LV의 장치 노드
fn device(id: Uuid) -> PathBuf {
    Path::new("/dev").join(vg()).join(lv(id))
}

/// 명령을 실행해 결과를 돌려준다(실패해도).
fn output(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("LVM_SUPPRESS_FD_WARNINGS", "1")
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"))
}

/// 실패한 확인을 이해하도록 VG와 그 LV, lvmlockd의 상태를 보인다.
fn state() -> String {
    let vg = vg();
    let mut text = String::new();
    for (program, args) in [
        (
            "vgs",
            vec!["-o", "vg_name,vg_lock_type,vg_size,vg_free", &vg],
        ),
        (
            "lvs",
            vec![
                "-a",
                "-o",
                "lv_name,lv_attr,lv_size,lv_tags,seg_pe_ranges",
                &vg,
            ],
        ),
        ("lvmlockctl", vec!["--info"]),
    ] {
        let out = output(program, &args);
        text.push_str(&format!(
            "\n$ {program} {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    text
}

/// VG에 있는 LV 이름 중 `prefix`로 시작하는 것
fn lvs(prefix: &str) -> Vec<String> {
    let out = output("lvs", &["--noheadings", "-o", "lv_name", &vg()]);
    assert!(out.status.success(), "{out:?}{}", state());
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    names.sort();
    names
}

/// 볼륨 LV의 `lvs` 필드 하나
fn lv_field(id: Uuid, field: &str) -> String {
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvs", &["--noheadings", "-o", field, &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// 볼륨 LV의 물리 익스텐트 구간(PV, 처음, 끝)을 세그먼트 순서대로
fn pe_ranges(id: Uuid) -> Vec<(String, u64, u64)> {
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvs", &["--noheadings", "-o", "seg_pe_ranges", &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .map(|range| {
            let (pv, pes) = range.rsplit_once(':').expect("PV:FIRST-LAST");
            let (first, last) = pes.split_once('-').expect("FIRST-LAST");
            (
                pv.to_string(),
                first.parse().unwrap(),
                last.parse().unwrap(),
            )
        })
        .collect()
}

/// 볼륨 LV가 이 노드에서 켜져 있는지(`lv_attr` 다섯째 글자 `a`)
fn is_active(id: Uuid) -> bool {
    lv_field(id, "lv_attr").chars().nth(4) == Some('a')
}

/// 볼륨 LV가 이 노드에서 배타로 켜져 있는지
fn is_exclusive(id: Uuid) -> bool {
    lv_field(id, "lv_active_exclusively") == "active exclusively"
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_zeroes_activates_exclusively_resizes_and_deletes() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();

    // 앞 볼륨이 쓴 자리에 새 볼륨이 놓여도 0으로 읽힌다: 만들 때 켜지 않으므로(`-Zn`) LVM은 채우지
    // 않고 드라이버가 모두 0으로 채운다. 이 노드가 켠 LV는 켜진 채로도 지운다.
    let old = Uuid::now_v7();
    storage
        .create_volume(&pool, old, 64 * MIB, RAW)
        .await
        .unwrap();
    storage.activate_volume(&pool, old, RAW).await.unwrap();
    assert!(qemu_io(&device(old), "raw", "write -P 0xab 0 64M"));
    storage.delete_volume(&pool, old, RAW).await.unwrap();
    assert!(lvs(&lv(old)).is_empty(), "{}", state());

    // 만든 뒤에는 꺼져 있다. 크기는 1 MiB 배수로 올린다.
    let id = Uuid::now_v7();
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, device(id));
    assert!(!path.exists(), "{}", state());
    assert!(!is_active(id), "{}", state());
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert_eq!(info.allocated_bytes, 64 * MIB);
    let err = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap_err();
    assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
    assert!(storage.target_reads_zero(&pool).await.unwrap());

    // 켜면 이 노드에서만(배타로) 켜지고 장치가 생긴다. 내용은 모두 0이다.
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(path.exists(), "{}", state());
    assert!(is_exclusive(id), "{}", state());
    assert!(qemu_io(&path, "raw", "read -P 0 0 64M"), "{}", state());
    let location = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, DiskLocation::Path(path.clone(), DiskKind::Block));
    assert!(storage.active_volumes(&pool).await.unwrap().contains(&id));
    // 이미 켜져 있어도 된다.
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    assert!(
        capacity.available_bytes < capacity.total_bytes,
        "{capacity:?}"
    );

    let again = storage.create_volume(&pool, id, 64 * MIB, RAW).await;
    assert!(
        matches!(again, Err(DriverError::VolumeExists(_))),
        "{again:?}"
    );
    let qcow2 = storage
        .create_volume(&pool, Uuid::now_v7(), 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(
        matches!(qcow2, Err(DriverError::InvalidSpec(_))),
        "{qcow2:?}"
    );
    assert!(!storage.supports_snapshots(&pool, RAW));
    let snap = storage.snapshot_create(&pool, id, RAW, "s1").await;
    assert!(matches!(snap, Err(DriverError::InvalidSpec(_))), "{snap:?}");

    // 켜진 볼륨을 늘리면 내용은 그대로다.
    assert!(qemu_io(&path, "raw", "write -P 0xcd 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io(&path, "raw", "read -P 0xcd 0 1M"));

    // 끄면 장치가 없어진다. 이미 꺼져 있어도 된다.
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    assert!(!path.exists(), "{}", state());
    assert!(!is_active(id), "{}", state());
    assert!(!storage.active_volumes(&pool).await.unwrap().contains(&id));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();

    // 꺼진 볼륨은 늘린 부분을 채우는 동안만 켠다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지
    // 않는다).
    storage
        .resize_volume(&pool, id, RAW, 192 * MIB)
        .await
        .unwrap();
    assert!(!is_active(id), "{}", state());
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 192 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(lvs(&lv(id)).is_empty(), "{}", state());
    let gone = storage.volume_info(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let gone = storage.activate_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
}

/// 늘린 부분은 0으로 읽힌다: 바로 뒤에 놓였다가 지운 볼륨의 데이터가 보이지 않는다. 꺼진 볼륨은 늘린
/// 뒤에도 꺼져 있고, 켜진 볼륨(실행 중인 VM)은 켠 채로 채운다. 다 채우면 표시 태그를 뗀다. 크기
/// 늘리기가 남긴 태그는 켜기(VM 시작)가 마저 채운다.
#[tokio::test]
async fn resize_zeroes_the_new_part() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();

    // 작은 볼륨 바로 뒤에 놓인 볼륨을 패턴으로 채우고 지운다. 작은 볼륨을 늘리면 그 자리를 다시 쓴다.
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 4 * MIB, RAW)
        .await
        .unwrap();
    let old = Uuid::now_v7();
    storage
        .create_volume(&pool, old, 64 * MIB, RAW)
        .await
        .unwrap();
    // 패턴 볼륨이 바로 뒤 익스텐트에 놓여야 늘린 부분이 그 자리를 다시 쓴다.
    let (small, pattern) = (pe_ranges(id), pe_ranges(old));
    let (pv, _, last) = small.last().unwrap().clone();
    let (pattern_pv, pattern_first, _) = pattern[0].clone();
    assert!(
        pv == pattern_pv && pattern_first == last + 1,
        "the pattern LV is not right after the volume: {small:?} {pattern:?}{}",
        state()
    );
    storage.activate_volume(&pool, old, RAW).await.unwrap();
    assert!(qemu_io(&device(old), "raw", "write -P 0xab 0 64M"));
    storage.deactivate_volume(&pool, old, RAW).await.unwrap();
    storage.delete_volume(&pool, old, RAW).await.unwrap();

    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io(&device(id), "raw", "write -P 0xcd 0 4M"));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage
        .resize_volume(&pool, id, RAW, 68 * MIB)
        .await
        .unwrap();
    assert!(!is_active(id), "{}", state());
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    let grown = pe_ranges(id);
    assert!(
        grown
            .iter()
            .any(|(p, first, last)| *p == pattern_pv && (*first..=*last).contains(&pattern_first)),
        "the new part is not on the pattern LV's extents: {grown:?} {pattern:?}{}",
        state()
    );
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io(&device(id), "raw", "read -P 0xcd 0 4M"));
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 4M 64M"),
        "{}",
        state()
    );

    // 켜진 볼륨은 켠 채로 늘리고 채운다.
    storage
        .resize_volume(&pool, id, RAW, 72 * MIB)
        .await
        .unwrap();
    assert!(is_exclusive(id), "{}", state());
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 4M 68M"),
        "{}",
        state()
    );

    // 늘린 부분을 다 채우지 못해 태그가 남았으면 켜기가 장치를 내주기 전에 마저 채우고 뗀다.
    assert!(qemu_io(&device(id), "raw", "write -P 0xef 68M 4M"));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    let tag = format!("opl-zero-from-{}", 68 * MIB);
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvchange", &["-q", "--addtag", &tag, &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    assert!(qemu_io(&device(id), "raw", "read -P 0xcd 0 4M"));
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 68M 4M"),
        "{}",
        state()
    );
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage.delete_volume(&pool, id, RAW).await.unwrap();
}

/// 장치를 연 채로는 끌 수 없다(실행 중인 VM의 디스크). 끄기는 실패하고 LV는 켜진 채 남는다.
#[tokio::test]
async fn an_open_volume_stays_active() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    let open = std::fs::File::open(device(id)).unwrap();
    let err = storage.deactivate_volume(&pool, id, RAW).await;
    assert!(err.is_err(), "{err:?}{}", state());
    assert!(is_active(id), "{}", state());
    drop(open);
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    assert!(!is_active(id), "{}", state());
    storage.delete_volume(&pool, id, RAW).await.unwrap();
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let _serial = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let pool = shared_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk. 가상 크기는 LV의 extent 크기로 올라간다.
    let pattern: Vec<u8> = (0..MIB as usize).map(|i| (i % 251) as u8).collect();
    let raw = tmp.path().join("src.raw");
    std::fs::write(&raw, &pattern).unwrap();
    let vmdk = tmp.path().join("src.vmdk");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw.to_str().unwrap(),
        vmdk.to_str().unwrap(),
    ]);
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    assert_eq!(path, device(id));
    // 써 넣은 뒤 끈다.
    assert!(!is_active(id), "{}", state());
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert!(info.virtual_bytes >= MIB, "{info:?}");
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    let mut head = vec![0u8; MIB as usize];
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert!(head == pattern, "imported contents differ");
    // 원본 뒤는 0이다.
    let rest = format!("read -P 0 1M {}", info.virtual_bytes - MIB);
    assert!(qemu_io(&path, "raw", &rest));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 LV를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .import_volume(
            &pool,
            missing,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            RAW,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(lvs(&lv(missing)).is_empty(), "{}", state());
}

#[tokio::test]
async fn copies_within_and_across_backends() {
    let _serial = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let pool = shared_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_lvm_shared(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    storages.activate_volume(&pool, src, RAW).await.unwrap();
    assert!(qemu_io(&device(src), "raw", "write -P 0xab 0 64k"));
    storages.deactivate_volume(&pool, src, RAW).await.unwrap();

    // 원본은 볼륨 LV를 켜서 읽게 하고, 놓으면 끈다. 스냅샷은 없다.
    let source = storages.volume_source(&pool, src, RAW, None).await.unwrap();
    assert_eq!(source.path, device(src));
    assert_eq!(source.size_bytes, 64 * MIB);
    assert!(is_exclusive(src), "{}", state());
    storages
        .release_source(&pool, src, RAW, None)
        .await
        .unwrap();
    assert!(!is_active(src), "{}", state());
    let snap = storages.volume_source(&pool, src, RAW, Some("s1")).await;
    assert!(matches!(snap, Err(DriverError::InvalidSpec(_))), "{snap:?}");
    assert!(!is_active(src), "{}", state());

    // 같은 풀 안: 새 LV로. 끝나면 둘 다 꺼져 있다.
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, None, false, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(!is_active(src) && !is_active(copy), "{}", state());
    let info = storages.volume_info(&pool, copy, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    storages.activate_volume(&pool, copy, RAW).await.unwrap();
    assert!(qemu_io(&device(copy), "raw", "read -P 0xab 0 64k"));
    assert!(qemu_io(&device(copy), "raw", "read -P 0 64k 1M"));
    storages.deactivate_volume(&pool, copy, RAW).await.unwrap();

    // 디렉터리 풀로: qcow2로
    let out = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, None, false, &local, out, qcow2)
        .await
        .unwrap();
    assert!(!is_active(src), "{}", state());
    let out_path = DirStorage::volume_path(&local, Path::new(MNT), out, qcow2);
    assert!(qemu_io(&out_path, "qcow2", "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw LV
    assert!(qemu_io(&out_path, "qcow2", "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(!is_active(back), "{}", state());
    storages.activate_volume(&pool, back, RAW).await.unwrap();
    assert!(qemu_io(&device(back), "raw", "read -P 0x5a 0 64k"));
    storages.deactivate_volume(&pool, back, RAW).await.unwrap();

    // 스냅샷에서는 복사할 수 없고 대상 LV를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("s1"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(lvs(&lv(missing)).is_empty(), "{}", state());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(lvs(&lv(id)).is_empty(), "{}", state());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}
```

`crates/opld/Cargo.toml`에서

```toml
rbd-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

을 다음으로 바꾼다:

```toml
rbd-it = []
# 실제 공유 LVM 풀(lvmlockd 공유 VG)의 볼륨으로 VM을 띄우는 E2E 테스트. qemu-it의 준비물에 더해 Linux에서
# root, lvm2-lockd와 sanlock(또는 dlm), qemu-io와 테스트가 LV를 만들고 지워도 되는 공유 VG
# (`OPL_TEST_LVM_SHARED=VG`)가 필요하다. `OPL_TEST_ISCSI=PORTAL,TARGET`이 있으면 VG가 그 iSCSI 타깃의
# LUN 위에 있다고 보고, 시험 전에 잠금을 멈추고 로그아웃해 opld가 다시 로그인하게 한다.
lvm-shared-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

`crates/opld/tests/lvm_shared.rs`를 만든다:

```rust
//! 실제 공유 LVM 풀: opld가 iSCSI 타깃에 로그인하고(있으면) 공유 VG의 잠금을 시작해 준비됐다고 알리고,
//! 그 풀의 raw 볼륨(두꺼운 LV)을 블록 장치로 붙인 VM을 띄운다. VM이 도는 동안에만 LV가 이 노드에서
//! 켜져 있고, opld가 다시 뜰 때 켜진 채 남은 LV는 끄되 실행 중인 VM의 LV는 그대로 둔다. 디렉터리
//! 풀로 복제하고 모두 지운다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 LV 장치를 직접 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_LVM_SHARED=oplshared OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared`
//! Linux에서 root, lvm2-lockd와 sanlock(또는 dlm), QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다.
//! `OPL_TEST_LVM_SHARED`는 테스트가 LV를 만들고 지워도 되는 공유 VG다. `OPL_TEST_ISCSI`가 있으면 VG가
//! 그 타깃의 LUN 위에 있다고 보고, 시험 전에 VG 잠금을 멈추고 로그아웃해 opld가 다시 로그인하게 한다.
#![cfg(all(feature = "lvm-shared-it", target_os = "linux"))]

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// 잠금 시작(sanlock 임대)과 iSCSI 로그인을 기다리는 시간
const WAIT: Duration = Duration::from_secs(180);

/// `OPL_TEST_LVM_SHARED`의 공유 VG
fn vg() -> String {
    std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG")
}

/// `OPL_TEST_ISCSI`의 포털과 타깃
fn iscsi() -> Option<(String, String)> {
    let value = std::env::var("OPL_TEST_ISCSI").ok()?;
    let (portal, target) = value
        .split_once(',')
        .expect("OPL_TEST_ISCSI must look like PORTAL,TARGET");
    Some((portal.to_string(), target.to_string()))
}

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다.
fn client(d: &Daemon) -> Client {
    let config: Value =
        serde_json::from_slice(&std::fs::read(d.path("oplctl.json")).unwrap()).unwrap();
    Client::new(
        config["server"].as_str().unwrap(),
        config["token"].as_str().map(str::to_string),
        config["ca_pem"].as_str().map(str::to_string),
    )
    .unwrap()
}

/// 명령을 실행해 결과를 돌려준다(실패해도).
fn output(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("LVM_SUPPRESS_FD_WARNINGS", "1")
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"))
}

/// 실패한 확인을 이해하도록 iSCSI 세션, VG와 그 LV, lvmlockd의 상태를 보인다.
fn state() -> String {
    let vg = vg();
    let mut text = String::new();
    for (program, args) in [
        ("iscsiadm", vec!["-m", "session"]),
        ("ls", vec!["-l", "/dev/disk/by-path"]),
        (
            "vgs",
            vec!["-o", "vg_name,vg_lock_type,vg_size,vg_free", &vg],
        ),
        ("lvs", vec!["-a", "-o", "lv_name,lv_attr,lv_size", &vg]),
        ("lvmlockctl", vec!["--info"]),
    ] {
        let out = output(program, &args);
        text.push_str(&format!(
            "\n$ {program} {} ({})\n{}{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    text
}

/// [`state`]와 opld 로그의 끝
fn diagnostics(d: &Daemon) -> String {
    let log = d.log();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(200)..].join("\n");
    format!("{}\n--- opld log (tail) ---\n{tail}", state())
}

/// 명령이 성공해야 한다.
fn must(program: &str, args: &[&str]) {
    let out = output(program, args);
    assert!(
        out.status.success(),
        "{program} {args:?}: {out:?}{}",
        state()
    );
}

/// 조건이 맞을 때까지 [`WAIT`]까지 다시 확인한다. 시간이 지나면 진단 정보와 함께 실패한다.
async fn until<F, Fut>(what: &str, d: &Daemon, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + WAIT;
    while !check().await {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}{}",
            diagnostics(d)
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 타깃 `target`의 iSCSI 세션이 있는지. 세션이 하나도 없으면 `iscsiadm`은 21로 끝난다.
fn session_present(target: &str) -> bool {
    let out = output("iscsiadm", &["-m", "session"]);
    assert!(
        out.status.success() || out.status.code() == Some(21),
        "iscsiadm -m session: {out:?}"
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.split_whitespace().any(|w| w == target))
}

/// LV `name`이 이 노드에서 켜져 있는지(`lv_attr` 다섯째 글자 `a`)
fn is_active(name: &str) -> bool {
    let target = format!("{}/{name}", vg());
    let out = output("lvs", &["--noheadings", "-o", "lv_attr", &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8_lossy(&out.stdout).trim().chars().nth(4) == Some('a')
}

/// VG에 있는 LV 이름 중 `prefix`로 시작하는 것
fn lvs(prefix: &str) -> Vec<String> {
    let out = output("lvs", &["--noheadings", "-o", "lv_name", &vg()]);
    assert!(out.status.success(), "{out:?}{}", state());
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    names.sort();
    names
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// QEMU 명령줄(`/proc/{pid}/cmdline`)
fn cmdline(pid: u64) -> String {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap()
        .split(|b| *b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 볼륨 `name`의 LV 이름(`opl-{id}`)
async fn volume_lv(c: &Client, name: &str) -> String {
    let volume = c.get(&format!("/volumes/{name}")).await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    format!("opl-{}", volume["id"].as_str().unwrap())
}

#[tokio::test]
async fn lvm_shared_pool_runs_and_clones_a_vm_and_deactivates_its_disk() {
    let vg = vg();
    let iscsi = iscsi();
    // opld가 iSCSI 로그인부터 하도록 VG 잠금을 멈추고 로그아웃한다(LV는 모두 끈다).
    if let Some((portal, target)) = &iscsi {
        must("vgchange", &["-an", &vg]);
        must("vgchange", &["--lockstop", &vg]);
        if session_present(target) {
            must(
                "iscsiadm",
                &["-m", "node", "-T", target, "-p", portal, "--logout"],
            );
        }
        assert!(!session_present(target), "{}", state());
    }

    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다. 풀 상태는 자주 새로 고친다.
    let mut d = Daemon::new(
        "node1",
        "[reconciler]\nacpi_timeout_secs = 5\n[pools]\nrefresh_secs = 2\n",
    );
    d.start().await;
    d.init("lvm-shared").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 로그인하고 잠금을 시작해 공유 VG를 확인하면 준비됐다고 알리고 VG의 용량을 보인다.
    // 공유 LVM 풀은 늘 공유다.
    let backend = match &iscsi {
        Some((portal, target)) => {
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": { "portal": portal, "target": target } })
        }
        None => json!({ "kind": "lvm-shared", "vg": vg }),
    };
    let created = c
        .post(
            "/storage-pools",
            &json!({ "name": "san", "spec": { "backend": backend } }),
        )
        .await
        .unwrap();
    assert_eq!(created["spec"]["shared"], true, "{created}");
    until("pool san ready", &d, || async {
        let node = c.get("/nodes/node1").await.unwrap();
        node["status"]["pools"]["san"]["ready"] == true
    })
    .await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["san"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");
    if let Some((_, target)) = &iscsi {
        assert!(session_present(target), "{}", diagnostics(&d));
    }

    // 볼륨은 두꺼운 LV `opl-{id}`이고 만든 뒤에는 꺼져 있다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "san", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let lv = volume_lv(&c, "root").await;
    let dev = PathBuf::from("/dev").join(&vg).join(&lv);
    assert!(!is_active(&lv), "{}", diagnostics(&d));
    must("lvchange", &["-aey", &format!("{vg}/{lv}")]);
    assert!(qemu_io(&dev, "raw", "read -P 0 0 64M"));
    assert!(qemu_io(&dev, "raw", "write -P 0xab 0 64k"));
    must("lvchange", &["-an", &format!("{vg}/{lv}")]);

    // VM을 켜면 LV를 이 노드에서 켜고 host_device로 연다.
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    let pid = web["status"]["pid"].as_u64().unwrap();
    let args = cmdline(pid);
    assert!(
        args.contains(&format!("driver=host_device,filename={}", dev.display())),
        "{args}"
    );
    assert!(is_active(&lv), "{}", diagnostics(&d));

    // 공유 LVM 디스크는 스냅샷이 없다.
    let resp = c
        .request(Method::POST, "/vms/web/snapshots")
        .json(&json!({ "name": "before" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");

    // 켜진 채 남은 LV(쓰는 VM·Task가 없다)는 opld가 다시 뜰 때 끈다. 실행 중인 VM의 LV는 그대로 둔다.
    d.ctl(&[
        "volume", "create", "idle", "--pool", "san", "--size", "8M", "--format", "raw",
    ])
    .await
    .unwrap();
    let idle = volume_lv(&c, "idle").await;
    must("lvchange", &["-aey", &format!("{vg}/{idle}")]);
    d.kill();
    d.start().await;
    until("left-over lv deactivated", &d, || async {
        !is_active(&idle)
    })
    .await;
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");
    assert!(is_active(&lv), "{}", diagnostics(&d));

    // VM이 멈추면 LV를 끈다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    until("vm disk deactivated", &d, || async { !is_active(&lv) }).await;
    assert!(!dev.exists(), "{}", diagnostics(&d));

    // 디렉터리 풀로 복제하면 새 디스크는 qcow2 파일이고 같은 내용을 담는다. 읽으려고 켠 LV는 끈다.
    let dir = d.path("pool");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&["vm", "clone", "web", "copy", "--pool", "local"])
        .await
        .unwrap();
    let copy_volume = c.get("/volumes/copy-root").await.unwrap();
    assert_eq!(copy_volume["status"]["phase"], "ready", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["format"], "qcow2", "{copy_volume}");
    let copy_file = dir.join(format!("{}.qcow2", copy_volume["id"].as_str().unwrap()));
    assert!(qemu_io(&copy_file, "qcow2", "read -P 0xab 0 64k"));
    assert!(!is_active(&lv), "{}", diagnostics(&d));

    // 같은 풀 안의 복제는 새 LV다.
    d.ctl(&["vm", "clone", "web", "twin"]).await.unwrap();
    let twin = volume_lv(&c, "twin-root").await;
    assert!(!is_active(&lv) && !is_active(&twin), "{}", diagnostics(&d));
    must("lvchange", &["-aey", &format!("{vg}/{twin}")]);
    let twin_dev = PathBuf::from("/dev").join(&vg).join(&twin);
    assert!(qemu_io(&twin_dev, "raw", "read -P 0xab 0 64k"));
    must("lvchange", &["-an", &format!("{vg}/{twin}")]);

    // 다시 켜고 멈춰도 LV는 꺼진다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    assert!(is_active(&lv), "{}", diagnostics(&d));
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    until("vm disk deactivated again", &d, || async {
        !is_active(&lv)
    })
    .await;

    for vm in ["copy", "twin", "web"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for volume in ["copy-root", "twin-root", "root", "idle"] {
        d.ctl(&["volume", "delete", volume]).await.unwrap();
    }
    assert!(!copy_file.exists());
    for name in [&lv, &twin, &idle] {
        assert!(lvs(name).is_empty(), "{}", diagnostics(&d));
    }
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "san"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 650개, 실패 0

Run: `cargo clippy -p opl-driver -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음(macOS에서는 공유 LVM 시험 파일이 비어 있다)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "feat(ci): 공유 LVM 실제 시험(lvm-shared-it)과 CI의 iSCSI 타깃·공유 VG"
```

---

### Task 6: oplctl: 공유 LVM 풀

`pool create NAME --lvm-shared VG [--iscsi-portal P --iscsi-target IQN]`(iSCSI 두 옵션은 함께, 다른 종류·`--shared`와 함께 못 씀, 본문은 `shared` 없이), 목록·상세의 종류 `lvm-shared`·위치 `VG` / `VG @ PORTAL TARGET`, 형식 기본값 raw.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, fetch_ca, fingerprint, local};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get { node: String },
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크를 만든다.
    Create {
        name: String,
        /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
        #[arg(long)]
        bridge: String,
        /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
        #[arg(long)]
        vlan: Option<u16>,
    },
    Delete {
        network: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다.
    #[arg(long = "nic")]
    nics: Vec<String>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{pool}")).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let token = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {}", args.user)?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &[
                        "NAME",
                        "ID",
                        "ADDRESS",
                        "ONLINE",
                        "CPUS",
                        "MEMORY",
                        "MAINTENANCE",
                    ],
                    |n| {
                        vec![
                            cell(&n["name"]),
                            cell(&n["id"]),
                            cell(&n["spec"]["address"]),
                            cell(&n["status"]["online"]),
                            cell(&n["status"]["cpus"]),
                            bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
                            cell(&n["spec"]["maintenance"]),
                        ]
                    },
                )?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client.get(&format!("/storage-pools/{pool}")).await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, &["NAME", "BRIDGE", "VLAN"], |n| {
                    vec![
                        cell(&n["name"]),
                        cell(&n["spec"]["bridge"]),
                        cell(&n["spec"]["vlan"]),
                    ]
                })?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create { name, bridge, vlan } => {
                let body = json!({ "name": name, "spec": { "bridge": bridge, "vlan": vlan } });
                client.post("/networks", &body).await?;
                writeln!(out, "network {name} created")?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    |t| {
                        vec![
                            cell(&t["id"]),
                            cell(&t["spec"]["op"]["kind"]),
                            cell(&t["spec"]["target"]),
                            cell(&t["status"]["state"]),
                            cell(&t["status"]["error"]),
                        ]
                    },
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            writeln!(out, "cluster: {}", cell(&view["name"]))?;
            writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
            let leader = view["leader"].clone();
            let rows: Vec<Vec<String>> = view["members"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|m| {
                    vec![
                        cell(&m["name"]),
                        cell(&m["id"]),
                        cell(&m["address"]),
                        if m["voter"] == true {
                            "voter"
                        } else {
                            "learner"
                        }
                        .into(),
                        cell(&m["online"]),
                        (m["id"] == leader).to_string(),
                    ]
                })
                .collect();
            table(
                out,
                &["NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER"],
                &rows,
            )?;
            let voters = view["members"]
                .as_array()
                .map_or(0, |m| m.iter().filter(|m| m["voter"] == true).count());
            if voters == 2 {
                writeln!(
                    out,
                    "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
                )?;
            }
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "PID", "ERROR"],
                |v| {
                    vec![
                        cell(&v["name"]),
                        cell(&v["spec"]["power"]),
                        cell(&v["status"]["phase"]),
                        cell(&v["status"]["node"]),
                        cell(&v["status"]["pid"]),
                        cell(&v["status"]["last_error"]),
                    ]
                },
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args
                .nics
                .iter()
                .map(|network| json!({ "network": network, "model": "virtio" }))
                .collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다). VM을 읽어 그 revision으로(`If-Match`)
/// spec 전체를 다시 쓴다.
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = ScheduleRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&schedule_update(&current, schedule.clone()));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// 예약 PUT을 다시 할 횟수
#[derive(Default)]
struct ScheduleRetries {
    mismatch: u32,
    busy: u32,
}

impl ScheduleRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 예약만 바꾼다(`None`이면 끈다).
fn schedule_update(vm: &Value, schedule: Option<Value>) -> Value {
    let mut spec = vm["spec"].clone();
    spec["snapshot_schedule"] = schedule.unwrap_or(Value::Null);
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 모든 노드이고, 아니면 목록의 노드를 그 순서대로
/// 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes.iter().map(Ok).collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다.
async fn login(server: &str, args: &LoginArgs, ca_pem: Option<String>) -> anyhow::Result<String> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&json!({ "username": args.user, "password": password }))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn schedule_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            schedule_update(&vm, Some(schedule.clone())),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            schedule_update(&vm, None),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn schedule_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = ScheduleRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = ScheduleRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 652개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): 공유 LVM 풀 만들기와 공유 LVM 풀의 볼륨 형식 기본값 raw"
```

---

### Task 7: 웹: 공유 LVM 풀

풀 만들기의 공유 LVM(VG, iSCSI 체크와 포털·타깃 — `iscsiPortalError`·`iscsiTargetError`는 서버와 같은 규칙, 체크를 바꾸면 그 오류를 지운다), 목록 종류, raw만, 공유 LVM 디스크 VM의 스냅샷 버튼을 이유와 함께 막고(`SnapshotSupport` `{ kind: "pool" }`, 서버와 같은 순서), 예약 스냅샷 경고(메모리 질문은 숨긴다). en·ko.

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/storage/PoolsPage.test.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/pools.test.ts`
- Modify: `web/src/storage/pools.ts`
- Modify: `web/src/vms/VmFormPage.test.tsx`
- Modify: `web/src/vms/VmSnapshots.test.tsx`
- Modify: `web/src/vms/VmSnapshots.tsx`
- Modify: `web/src/vms/form/ScheduleSection.tsx`
- Modify: `web/src/vms/snapshots.test.ts`
- Modify: `web/src/vms/snapshots.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json` 전체:

```json
{
  "errors": {
    "network": "Cannot reach the server.",
    "unauthorized": "Your session has ended. Sign in again.",
    "invalid_credentials": "Wrong username or password.",
    "forbidden": "You do not have permission to do this.",
    "cross_origin": "The request came from another site and was rejected.",
    "not_found": "It no longer exists.",
    "already_exists": "It already exists.",
    "name_taken": "That name is already in use.",
    "revision_mismatch": "Someone else changed it first. Reload and try again.",
    "mac_in_use": "That MAC address is used by another VM.",
    "volume_in_use": "A volume is attached to another VM.",
    "volume_not_ready": "A volume is not ready yet.",
    "vm_running": "Stop the VM first.",
    "vm_not_running": "The VM is not running.",
    "busy": "Another operation on this VM is in progress. Try again shortly.",
    "no_node": "No node can run this VM: {{message}}",
    "console_unavailable": "The console is not available right now.",
    "cluster_no_quorum": "The cluster has no quorum. The change may still be applied later.",
    "not_leader": "The cluster leader is changing. Try again shortly.",
    "network_in_use": "A VM uses this network. Remove its interface first.",
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_not_on_node": "That storage pool is not available on the node you are connected to.",
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "last_admin": "The last administrator cannot give up the administrator role.",
    "cannot_delete_self": "You cannot delete your own account.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. Snapshots of disks in LVM-thin, ZFS or Ceph RBD pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
    "vm_has_snapshots": "The VM has snapshots. Delete them first.",
    "volume_in_snapshot": "A VM snapshot still uses this volume. Delete that snapshot first.",
    "snapshot_not_ready": "The snapshot is still being taken.",
    "node_offline": "The node holding this VM is offline.",
    "too_many_attempts": "Too many attempts. Try again later.",
    "machine_arch_mismatch": "The machine type doesn't match the selected node's architecture.",
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}"
  },
  "nav": {
    "dashboard": "Dashboard",
    "vms": "Virtual machines",
    "volumes": "Volumes",
    "pools": "Storage pools",
    "networks": "Networks",
    "nodes": "Nodes",
    "tasks": "Tasks",
    "users": "Users",
    "tokens": "API tokens",
    "cluster": "Cluster"
  },
  "header": {
    "language": "Language",
    "theme": "Theme",
    "themeSystem": "System",
    "themeLight": "Light",
    "themeDark": "Dark",
    "account": "Account",
    "logout": "Sign out"
  },
  "roles": {
    "admin": "Administrator",
    "operator": "Operator",
    "viewer": "Viewer"
  },
  "login": {
    "username": "Username",
    "password": "Password",
    "submit": "Sign in"
  },
  "notFound": {
    "title": "Page not found",
    "home": "Go to the dashboard"
  },
  "live": {
    "live": "Live",
    "connecting": "Connecting…",
    "offline": "Disconnected · reconnecting"
  },
  "tasks": {
    "time": "Time",
    "operation": "Operation",
    "error": "Error",
    "empty": "No tasks.",
    "emptyFailed": "No failed tasks.",
    "failedOnly": "Failed only",
    "done": "{{label}}: done",
    "failed": "{{label}}: failed. {{error}}",
    "gone": "{{label}}: result is no longer available",
    "state": {
      "pending": "Pending",
      "running": "Running",
      "succeeded": "Succeeded",
      "failed": "Failed"
    },
    "op": {
      "vm-start": "Start {{target}}",
      "vm-stop": "Stop {{target}}",
      "vm-force-stop": "Force stop {{target}}",
      "vm-reboot": "Reboot {{target}}",
      "volume-create": "Create volume {{target}}",
      "volume-import": "Import volume {{target}}",
      "volume-delete": "Delete volume {{target}}",
      "snapshot-create": "Take snapshot of {{target}}",
      "snapshot-revert": "Revert {{target}} to a snapshot",
      "snapshot-delete": "Delete snapshot of {{target}}",
      "volume-resize": "Resize volume {{target}}",
      "vm-clone": "Clone {{target}}"
    }
  },
  "pager": {
    "label": "Pages",
    "previous": "Previous",
    "next": "Next",
    "range": "{{from}}–{{to}} of {{total}}"
  },
  "common": {
    "loading": "Loading…",
    "copied": "Copied.",
    "copyFailed": "Could not copy. Select the text and copy it yourself.",
    "cancel": "Cancel",
    "save": "Save",
    "create": "Create",
    "delete": "Delete",
    "deleteNamed": "Delete {{name}}",
    "done": "Done",
    "name": "Name",
    "state": "State",
    "node": "Node"
  },
  "confirm": {
    "typeName": "Type {{name}} to confirm"
  },
  "vms": {
    "vcpus": "vCPUs",
    "memory": "Memory",
    "actions": "Actions",
    "actionsFor": "Actions for {{name}}",
    "search": "Search by name",
    "empty": "No virtual machines.",
    "selectAll": "Select all",
    "select": "Select {{name}}",
    "selected_one": "{{count}} selected",
    "selected_other": "{{count}} selected",
    "deleted": "Deleted {{name}}.",
    "forceStopTitle": "Force stop {{name}}?",
    "forceStopDescription": "The VM is turned off without shutting down its OS. Unsaved data in the guest is lost.",
    "reason": {
      "powerPending": "A power action is still in progress.",
      "reverting": "Not available while a snapshot is being reverted.",
      "running": "The VM is running.",
      "stopped": "The VM is stopped.",
      "failed": "The VM has failed.",
      "busy": "The VM is starting or stopping."
    },
    "deleteTitle": "Delete {{name}}?",
    "deleteDescription": "The VM is deleted. Its volumes are kept.",
    "deleteSnapshots_one": "Also delete its snapshot",
    "deleteSnapshots_other": "Also delete all {{count}} of its snapshots",
    "hasSnapshots": "A VM with snapshots cannot be deleted. Delete its snapshots first, or check the box to delete them along with the VM.",
    "snapshotsPending": "A snapshot of this VM is still being taken. Delete the VM once it is done.",
    "deletingSnapshots": "Deleting the snapshots one at a time. This cannot be cancelled once started.",
    "phase": {
      "stopped": "Stopped",
      "starting": "Starting",
      "running": "Running",
      "stopping": "Stopping",
      "failed": "Failed"
    },
    "action": {
      "start": "Start",
      "stop": "Stop",
      "force-stop": "Force stop",
      "reboot": "Reboot",
      "console": "Console",
      "delete": "Delete",
      "edit": "Edit",
      "clone": "Clone"
    },
    "notFound": "There is no VM {{name}}",
    "backToList": "Back to virtual machines",
    "tab": {
      "summary": "Summary",
      "hardware": "Hardware",
      "console": "Console",
      "snapshots": "Snapshots",
      "tasks": "Tasks",
      "json": "JSON"
    },
    "cpu": "CPU",
    "cpuValue": "{{sockets}} × {{cores}} cores ({{model}})",
    "firmware": "Firmware",
    "machine": "Machine type",
    "restart": "Restart policy",
    "power": "Desired power",
    "pid": "QEMU PID",
    "created": "Created",
    "lastError": "Last error",
    "restartPolicy": {
      "never": "Never",
      "on-failure": "On failure"
    },
    "powerState": {
      "running": "Running",
      "stopped": "Stopped"
    },
    "disks": "Disks",
    "volume": "Volume",
    "bus": "Bus",
    "bootOrder": "Boot order",
    "nics": "Network interfaces",
    "network": "Network",
    "model": "Model",
    "mac": "MAC address",
    "openConsole": "Open console",
    "actionFailed": "{{name}}: {{error}}",
    "bulkSummary": "{{action}}: {{done}} requested, {{failed}} failed, {{skipped}} skipped.",
    "noDisks": "No disks.",
    "noNics": "No network interfaces."
  },
  "snapshots": {
    "created": "Created",
    "memory": "Memory",
    "description": "Description",
    "empty": "No snapshots.",
    "current": "Current",
    "after": "(after {{parent}})",
    "pending": "Creating",
    "memoryYes": "Included",
    "memoryNo": "No",
    "memorySize": "Included ({{size}})",
    "create": "Take snapshot",
    "createTitle": "Take a snapshot of {{name}}",
    "createDescription": "Saves the VM's disks, firmware variables and settings so you can return to this point later.",
    "includeMemory": "Include memory",
    "includeMemoryHint": "Also saves the running state. Reverting resumes the VM where it was.",
    "unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. {{volume}} is {{format}}.",
    "busy": "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    "revert": "Revert",
    "revertNamed": "Revert to {{name}}",
    "revertTitle": "Revert {{vm}} to {{name}}?",
    "revertDescription": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot. It stays off.",
    "revertDescriptionMemory": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot, and it resumes running from the saved memory.",
    "deleteNamed": "Delete snapshot {{name}}",
    "deleteTitle": "Delete snapshot {{name}}?",
    "deleteDescription": "The snapshot is deleted. Snapshots taken after it are kept, and the VM's current state does not change.",
    "op": {
      "create": "Take snapshot {{name}} of {{vm}}",
      "revert": "Revert {{vm}} to {{name}}",
      "delete": "Delete snapshot {{name}} of {{vm}}"
    },
    "errors": {
      "exists": "This VM already has a snapshot with this name.",
      "description": "Use at most 1024 characters."
    },
    "scheduled": "Scheduled",
    "every": {
      "hourly": "Hourly",
      "daily": "Daily",
      "weekly": "Weekly"
    },
    "schedule": "Snapshot schedule: {{every}}, keep {{keep}}",
    "scheduleMemory": "Snapshot schedule: {{every}}, keep {{keep}}, with memory",
    "noSchedule": "No snapshot schedule.",
    "scheduleEdit": "Change schedule",
    "scheduleFailed": "Last scheduled snapshot failed:",
    "scheduleError": {
      "vm_busy": "Skipped because the VM was busy; it will try again later.",
      "node_offline": "Skipped because the VM's node is offline ({{message}})."
    },
    "mixed": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. This VM mixes them.",
    "blockHint": "Disks in LVM-thin, ZFS and Ceph RBD pools are saved without memory. A running VM pauses briefly while they are saved.",
    "zfsLatest": "Disks in ZFS pools can only revert to their latest snapshot.",
    "sharedLvm": "Disks in shared LVM pools cannot be snapshotted. {{volume}} is in one."
  },
  "clone": {
    "title": "Clone {{name}}",
    "description": "A new stopped VM is made with full copies of the disks. It gets new MAC addresses and no snapshot schedule.",
    "name": "New VM name",
    "source": "Source",
    "current": "Current state (stopped VM only)",
    "snapshot": "Snapshot {{name}}",
    "running": "The VM is not stopped. Stop it first or choose a snapshot.",
    "busy": "The VM is starting or stopping. Try again when it has settled.",
    "gone": "The chosen snapshot no longer exists or is not ready. Choose another source.",
    "goneOption": "Chosen snapshot (no longer available)",
    "pool": "Storage pool for the disks",
    "samePool": "Same pool as each source disk",
    "submit": "Clone",
    "op": "Clone {{vm}} to {{name}}",
    "errors": {
      "exists": "A VM with this name already exists.",
      "poolNotOnNode": "That storage pool is not available on the node holding the VM.",
      "poolUnavailable": "That storage pool is not ready on the node holding the VM."
    }
  },
  "forms": {
    "name": "Use 1–63 lowercase letters, digits and -, not starting or ending with -."
  },
  "volumes": {
    "pool": "Storage pool",
    "size": "Size",
    "allocated": "Allocated",
    "format": "Format",
    "vm": "Used by",
    "empty": "No volumes.",
    "phase": {
      "pending": "Creating",
      "ready": "Ready"
    },
    "createTitle": "Create an empty volume",
    "createDescription": "A new disk image in a storage pool. A qcow2 volume takes space only as it is written.",
    "deleteTitle": "Delete volume {{name}}?",
    "deleteDescription": "The disk image and all data on it are deleted.",
    "resize": "Resize",
    "resizeNamed": "Resize {{name}}",
    "resizeTitle": "Resize volume {{name}}",
    "resizeDescription": "Volumes can only grow. A running VM sees the new size right away; grow the partition and file system inside the guest yourself.",
    "currentSize": "Current size: {{size}}",
    "newSize": "New size (GiB)",
    "errors": {
      "reserved": "The name import is reserved.",
      "exists": "A volume with this name already exists.",
      "grow": "Enter a size larger than the current size."
    },
    "reason": {
      "inUse": "Remove it from the VM first.",
      "notReady": "Available once ready.",
      "vmsUnknown": "Cannot tell which VMs use it."
    }
  },
  "uploads": {
    "open": "Upload image",
    "title": "Upload a disk image",
    "description": "The file goes to the node you are connected to ({{node}}), so only storage pools on that node can be chosen. The image is converted into a new volume.",
    "file": "Image file",
    "sourceFormat": "File format",
    "submit": "Upload",
    "noPool": "No storage pool is available on this node.",
    "progress": "Uploading {{name}}: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "Cancelled the upload of {{name}}.",
    "failed": "Upload of {{name}} failed. {{error}}",
    "processing": "Processing {{name}}…",
    "noResponse": "{{name}}: no response from the server. Check the volume list for the result.",
    "dropped": "The server rejected the upload or the connection was lost.",
    "noNode": "Cannot tell which node you are connected to. {{error}}",
    "errors": {
      "file": "Choose a file."
    }
  },
  "pools": {
    "path": "Directory",
    "nodes": "Nodes",
    "free": "Free / total",
    "volumes": "Volumes",
    "empty": "No storage pools.",
    "allNodes": "All nodes",
    "createTitle": "Create a storage pool",
    "createDescription": "Where volumes are kept: a directory on each node, an NFS export that the nodes mount, an LVM thin pool or ZFS dataset on each node, a Ceph RBD pool, or a volume group shared by the nodes.",
    "pathHint": "An absolute path on the nodes.",
    "shared": "Shared storage",
    "sharedHint": "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export and images in its Ceph pool are kept.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "kind": "Kind",
    "kinds": {
      "dir": "Directory",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD",
      "lvm-shared": "Shared LVM"
    },
    "location": "Location",
    "kindHint": {
      "dir": "Create the directory on each node first.",
      "nfs": "Each node mounts the export under its data directory. Mounting needs Linux nodes running as root. The pool is shared by its nodes.",
      "lvm-thin": "Create the volume group and thin pool on each node first. Volumes are raw thin volumes kept on the node that creates them, and their snapshots hold disks only. Needs Linux nodes running as root.",
      "zfs": "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root.",
      "rbd": "Create the Ceph pool first and put ceph.conf and the keyring on each node. Volumes are raw RBD images that every node of the pool can use, and their snapshots hold disks only. Needs Linux nodes with QEMU's rbd driver (qemu-block-extra) and the rbd command (ceph-common).",
      "lvm-shared": "Create the shared volume group (vgcreate --shared) on a LUN every node sees first, and run lvmlockd with sanlock or dlm on each node. Volumes are raw logical volumes that every node of the pool can use, without snapshots. Needs Linux nodes running as root."
    },
    "server": "NFS server",
    "serverHint": "Host name or IP address.",
    "export": "Export path",
    "exportHint": "The absolute path the server exports, such as /srv/vm.",
    "options": "Mount options (optional)",
    "optionsHint": "Comma-separated options passed to mount -o after nosuid,nodev,noexec,retry=0, such as vers=4.2,hard.",
    "state": {
      "ready": "Ready",
      "notReady": "Not ready",
      "unknown": "No status reported yet",
      "offline": "Offline"
    },
    "errors": {
      "path": "Enter an absolute path (starting with /).",
      "export": "Enter an absolute path (starting with /) without control characters.",
      "nodesGone": "The chosen nodes are no longer in the cluster. Choose again.",
      "server": "Enter a host name or IP address without spaces, commas, slashes or brackets.",
      "options": "Separate the options with commas, without spaces. bind, rbind, remount, move, suid, dev, exec, helper, bg and x-* options are not allowed.",
      "lvmName": "Use 1 to 127 letters, digits, +, _, . or -, not starting with -, snapshot or pvmove. LVM also reserves . and .. and names containing parts like _tdata or _tmeta.",
      "zfsDataset": "Use a dataset name such as tank/opl, at most 177 characters: parts separated by / with letters, digits, _, ., : or -, none empty, . or .. or starting with -. The pool name comes first and starts with a letter.",
      "rbdName": "Use 1 to 63 letters, digits, _, . or -, not starting with -.",
      "cephUser": "Write the Ceph user without the \"client.\" prefix, for example opl instead of client.opl.",
      "rbdConf": "Enter an absolute path (starting with /) without control characters, commas, colons or equals signs.",
      "portal": "Enter a host name or IP address with an optional :port (1–65535), without spaces, commas or slashes. Put IPv6 addresses in brackets, such as [fd00::5]:3260.",
      "target": "Use a target name starting with iqn., eui. or naa., at most 223 characters of lowercase letters, digits, ., : or -."
    },
    "vg": "Volume group",
    "thinpool": "Thin pool",
    "dataset": "Dataset",
    "cephPool": "Ceph pool",
    "namespace": "Namespace (optional)",
    "cephUser": "Ceph user (optional)",
    "cephUserHint": "The Ceph client ID without the \"client.\" prefix. Uses admin if empty.",
    "cephConf": "ceph.conf path (optional)",
    "cephConfHint": "An absolute path on the nodes, /etc/ceph/ceph.conf if empty.",
    "iscsi": "Log in to an iSCSI target",
    "iscsiHint": "Each node logs in to the target before using the volume group. Leave unchecked when the nodes already see the LUN, such as over Fibre Channel.",
    "portal": "iSCSI portal",
    "portalHint": "Host name or IP address with an optional port, such as san.lan:3260 or [fd00::5]:3260.",
    "target": "iSCSI target",
    "targetHint": "The target name, such as iqn.2003-01.org.example:storage."
  },
  "networks": {
    "bridge": "Bridge",
    "vlan": "VLAN",
    "vlanOptional": "VLAN tag (optional)",
    "vlanHint": "1–4094. The bridge must be VLAN-aware (vlan_filtering 1).",
    "nodes": "Nodes with the bridge",
    "vms": "VMs",
    "empty": "No networks.",
    "userNote": "The user network (QEMU NAT) is always available and is not listed.",
    "noNode": "No node has this bridge",
    "noVlanFiltering": "VLAN filtering is off on {{nodes}}",
    "createTitle": "Create a network",
    "createDescription": "Points VM interfaces at a Linux bridge that already exists on the nodes. VMs on this network run only on nodes with the bridge.",
    "created": "Created network {{name}}.",
    "deleteTitle": "Delete network {{name}}?",
    "deleteDescription": "Only the network is removed. The bridges on the nodes are not changed.",
    "deleted": "Deleted network {{name}}.",
    "inUse": "Remove the VM interfaces first.",
    "errors": {
      "reserved": "The name user is reserved for NAT.",
      "bridge": "Use 1–15 characters without /, : or spaces.",
      "vlan": "Use a number from 1 to 4094."
    }
  },
  "nodes": {
    "address": "API address",
    "hypervisor": "Hypervisor",
    "arch": "Architecture",
    "cpus": "CPUs",
    "memory": "Memory",
    "bridges": "Bridges",
    "pools": "Storage pools",
    "vms": "VMs",
    "thisNode": "(this node)",
    "joinNote": "To add a node, issue a join token on the Cluster page and run oplctl cluster join on the new node.",
    "maintenanceNote": "No new VMs are placed on a node in maintenance. VMs already running there keep running.",
    "startMaintenance": "Enter maintenance",
    "endMaintenance": "End maintenance",
    "maintenanceOn": "{{name}} is in maintenance.",
    "maintenanceOff": "{{name}} is out of maintenance.",
    "remove": "Remove from cluster",
    "removeTitle": "Remove {{name}} from the cluster?",
    "removeDescription": "Only for a node that is dead for good. It cannot rejoin with its current data, and VMs assigned to it stay assigned.",
    "removeNote": "Only offline nodes can be removed here. To remove a live node, run oplctl cluster leave on it.",
    "removed": "Removed {{name}} from the cluster.",
    "reason": {
      "thisNode": "You are connected through this node.",
      "online": "The node is online. Run oplctl cluster leave on it."
    }
  },
  "accounts": {
    "currentPassword": "Current password",
    "changePassword": "Change my password",
    "changePasswordDescription": "All your sessions end. Sign in again with the new password.",
    "passwordChanged": "Your password was changed. Sign in with the new password.",
    "newPassword": "New password",
    "confirmPassword": "Confirm the new password",
    "passwordHint": "At least 8 characters.",
    "errors": {
      "password": "Use at least 8 characters.",
      "mismatch": "The passwords do not match.",
      "current": "Enter your current password.",
      "wrongCurrent": "The current password is wrong."
    }
  },
  "users": {
    "role": "Role",
    "you": "(you)",
    "empty": "No users.",
    "roleHint": "Operators change VMs and volumes. Viewers only read. Neither can see accounts.",
    "createTitle": "Create a user",
    "created": "Created user {{name}}.",
    "changeRole": "Change role",
    "roleTitle": "Change the role of {{name}}",
    "roleChanged": "{{name}} is now {{role}}.",
    "resetPassword": "Reset password",
    "passwordTitle": "Reset the password of {{name}}",
    "passwordDescription": "The user's sessions end and they sign in with the new password.",
    "passwordReset": "Reset the password of {{name}}.",
    "deleteTitle": "Delete user {{name}}?",
    "deleteDescription": "The user and their API tokens are deleted.",
    "deleted": "Deleted user {{name}}.",
    "adminOnly": "Only administrators can manage users.",
    "selfNote": "Change your own password from the account menu. Only another administrator can change your role or delete your account.",
    "reason": {
      "selfRole": "Only another administrator can change your role.",
      "selfPassword": "Change your own password from the account menu.",
      "selfDelete": "Only another administrator can delete your account."
    }
  },
  "tokens": {
    "owner": "Owner",
    "created": "Created",
    "empty": "No API tokens.",
    "note": "Scripts and tools use API tokens. A token has the role of its owner. oplctl login makes one for the CLI.",
    "createTitle": "Create an API token",
    "createDescription": "The token acts as you, with your role.",
    "secretTitle": "Token {{name}} was created",
    "secretDescription": "Copy the token now. It cannot be shown again.",
    "secret": "Token",
    "copySecret": "Copy the token",
    "usage": "Send it in the header",
    "deleteTitle": "Delete API token {{name}}?",
    "deleteDescription": "Anything using the token stops working right away.",
    "deleted": "Deleted API token {{name}}."
  },
  "cluster": {
    "title": "Cluster {{name}}",
    "ca": "Certificate authority",
    "caNote": "Every node's HTTPS certificate is signed by the cluster CA. Import the CA into your browser to stop the certificate warning, and check that its fingerprint matches. Browsers show the SHA-256 fingerprint in uppercase hex, in pairs separated by colons or spaces; compare it with the hex after sha256: (letter case does not matter).",
    "copyFingerprint": "Copy the CA fingerprint",
    "downloadCa": "Download the CA",
    "members": "Members",
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining.",
    "refreshFailed": "Could not refresh the cluster. {{error}}",
    "role": "Role",
    "leader": "Leader",
    "voter": "Voter",
    "learner": "Learner",
    "clusterAddress": "Cluster address",
    "issue": "Issue a join token",
    "issueTitle": "Issue a join token",
    "issueDescription": "A new node joins with the token once. Start opld on the new node first.",
    "ttl": "Valid for",
    "ttlOption": {
      "3600": "1 hour",
      "86400": "1 day",
      "604800": "7 days"
    },
    "issuedTitle": "Join a new node",
    "issuedDescription": "Run this on the new node before {{time}}. It cannot be shown again.",
    "command": "Command",
    "copyCommand": "Copy the command",
    "commandNote": "Add --socket if opld on the new node does not use /run/opl/opld.sock."
  },
  "vmForm": {
    "createTitle": "Create a virtual machine",
    "editTitle": "Edit {{name}}",
    "runningNote": "The VM is running: its node and disks cannot change. Other changes apply from the next start.",
    "general": "General",
    "cores": "vCPU cores",
    "memory": "Memory (MiB)",
    "cpuModel": "CPU model",
    "nodeAuto": "Automatic",
    "diskN": "Disk {{n}}",
    "newVolume": "New volume",
    "pool": "Storage pool",
    "sizeGib": "Size (GiB)",
    "removeDisk": "Remove disk {{n}}",
    "addExisting": "Attach a volume",
    "addNew": "New empty volume",
    "nicN": "Network interface {{n}}",
    "userNetwork": "user (NAT)",
    "macAuto": "Automatic",
    "removeNic": "Remove interface {{n}}",
    "addNic": "Add an interface",
    "startAfter": "Start after creating",
    "reload": "Reload",
    "errors": {
      "name": "Use 1–63 lowercase letters, digits and -, not starting or ending with -.",
      "cores": "From 1 to 65535.",
      "memory": "At least 64 MiB.",
      "volume": "Choose or name a volume.",
      "pool": "Choose a storage pool.",
      "size": "Enter a size.",
      "bootOrder": "Boot orders must be unique numbers from 1 to 255.",
      "mac": "Use the form 52:54:00:12:34:56.",
      "volumeChoose": "Choose a volume.",
      "volumeDuplicate": "Each disk needs a different volume.",
      "volumeExists": "A volume with this name already exists.",
      "macMulticast": "A multicast address (odd first byte) cannot be used.",
      "macDuplicate": "Each interface needs a different MAC address.",
      "keep": "From 1 to 100."
    },
    "volumesCreated": "Volumes {{names}} were created; {{error}}",
    "schedule": "Scheduled snapshots",
    "scheduleNote": "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    "scheduleEvery": "Take snapshots",
    "scheduleOff": "Off",
    "scheduleKeep": "Snapshots to keep",
    "scheduleRaw": "{{volume}} is {{format}}, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    "scheduleMemoryHint": "Memory is saved only while the VM is running and has disks.",
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    "scheduleSharedLvm": "{{volume}} is in a shared LVM pool, so scheduled snapshots will fail."
  },
  "console": {
    "connecting": "Connecting…",
    "connected": "Connected",
    "failed": "Disconnected. The VM may have stopped or the console is not available.",
    "reconnect": "Reconnect",
    "paste": "Type clipboard",
    "pasteDenied": "The browser did not allow reading the clipboard.",
    "fullscreen": "Full screen",
    "pasteTruncated": "Only the first {{count}} characters were typed.",
    "screen": "VM screen",
    "hint": "Click the screen to type into the VM. Click outside it to release the keyboard.",
    "forbidden": "You do not have permission to use the console."
  },
  "dashboard": {
    "nodes": "Nodes",
    "online": "online",
    "vms": "Virtual machines",
    "vmCounts": "{{running}} running · {{stopped}} stopped · {{failed}} failed",
    "leader": "Leader",
    "perNode": "Nodes",
    "hypervisor": "Hypervisor",
    "activeVms": "Active VMs",
    "nodeOnline": "Online",
    "nodeOffline": "Offline",
    "maintenance": "Maintenance",
    "failedVms": "Failed virtual machines",
    "recentTasks": "Recent tasks",
    "allTasks": "All tasks",
    "noLeader": "No leader",
    "noNodes": "No nodes."
  }
}
```

`web/src/i18n/ko.json` 전체:

```json
{
  "errors": {
    "network": "서버에 연결할 수 없습니다.",
    "unauthorized": "세션이 끝났습니다. 다시 로그인하세요.",
    "invalid_credentials": "사용자 이름이나 비밀번호가 틀렸습니다.",
    "forbidden": "이 작업을 할 권한이 없습니다.",
    "cross_origin": "다른 사이트에서 온 요청이라 거부했습니다.",
    "not_found": "더 이상 존재하지 않습니다.",
    "already_exists": "이미 있습니다.",
    "name_taken": "이미 쓰는 이름입니다.",
    "revision_mismatch": "다른 사람이 먼저 바꿨습니다. 다시 불러온 뒤 시도하세요.",
    "mac_in_use": "다른 VM이 쓰는 MAC 주소입니다.",
    "volume_in_use": "다른 VM에 붙어 있는 볼륨입니다.",
    "volume_not_ready": "아직 준비되지 않은 볼륨입니다.",
    "vm_running": "먼저 VM을 정지하세요.",
    "vm_not_running": "VM이 실행 중이 아닙니다.",
    "busy": "이 VM에 다른 작업이 진행 중입니다. 잠시 뒤 다시 시도하세요.",
    "no_node": "이 VM을 실행할 노드가 없습니다: {{message}}",
    "console_unavailable": "지금은 콘솔을 열 수 없습니다.",
    "cluster_no_quorum": "클러스터 과반수가 없습니다. 변경은 나중에 적용될 수 있습니다.",
    "not_leader": "클러스터 리더가 바뀌는 중입니다. 잠시 뒤 다시 시도하세요.",
    "network_in_use": "이 네트워크를 쓰는 VM이 있습니다. 먼저 그 인터페이스를 빼세요.",
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_not_on_node": "지금 접속한 노드에서 쓸 수 없는 스토리지 풀입니다.",
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "last_admin": "마지막 관리자는 관리자 역할을 내려놓을 수 없습니다.",
    "cannot_delete_self": "자기 계정은 지울 수 없습니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제)이 진행 중입니다. 끝나면 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. LVM-thin·ZFS·Ceph RBD 풀의 디스크 스냅샷에는 메모리를 담을 수 없고, ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
    "vm_has_snapshots": "VM에 스냅샷이 있습니다. 먼저 스냅샷을 지우세요.",
    "volume_in_snapshot": "VM 스냅샷이 아직 이 볼륨을 씁니다. 먼저 그 스냅샷을 지우세요.",
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "too_many_attempts": "시도가 너무 많습니다. 잠시 뒤에 다시 하세요.",
    "machine_arch_mismatch": "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다.",
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}"
  },
  "nav": {
    "dashboard": "대시보드",
    "vms": "가상 머신",
    "volumes": "볼륨",
    "pools": "스토리지 풀",
    "networks": "네트워크",
    "nodes": "노드",
    "tasks": "작업",
    "users": "사용자",
    "tokens": "API 토큰",
    "cluster": "클러스터"
  },
  "header": {
    "language": "언어",
    "theme": "테마",
    "themeSystem": "시스템",
    "themeLight": "밝게",
    "themeDark": "어둡게",
    "account": "계정",
    "logout": "로그아웃"
  },
  "roles": {
    "admin": "관리자",
    "operator": "운영자",
    "viewer": "조회자"
  },
  "login": {
    "username": "사용자 이름",
    "password": "비밀번호",
    "submit": "로그인"
  },
  "notFound": {
    "title": "페이지를 찾을 수 없습니다",
    "home": "대시보드로 가기"
  },
  "live": {
    "live": "실시간",
    "connecting": "연결 중…",
    "offline": "실시간 연결 끊김 · 재연결 중"
  },
  "tasks": {
    "time": "시각",
    "operation": "작업",
    "error": "오류",
    "empty": "작업이 없습니다.",
    "emptyFailed": "실패한 작업이 없습니다.",
    "failedOnly": "실패만",
    "done": "{{label}}: 완료",
    "failed": "{{label}}: 실패. {{error}}",
    "gone": "{{label}}: 결과를 더 이상 볼 수 없습니다",
    "state": {
      "pending": "대기",
      "running": "진행 중",
      "succeeded": "성공",
      "failed": "실패"
    },
    "op": {
      "vm-start": "{{target}} 시작",
      "vm-stop": "{{target}} 정지",
      "vm-force-stop": "{{target}} 강제 정지",
      "vm-reboot": "{{target}} 재시작",
      "volume-create": "볼륨 {{target}} 만들기",
      "volume-import": "볼륨 {{target}} 가져오기",
      "volume-delete": "볼륨 {{target}} 삭제",
      "snapshot-create": "{{target}} 스냅샷 만들기",
      "snapshot-revert": "{{target}} 스냅샷으로 되돌리기",
      "snapshot-delete": "{{target}} 스냅샷 삭제",
      "volume-resize": "볼륨 {{target}} 크기 늘리기",
      "vm-clone": "{{target}} 복제"
    }
  },
  "pager": {
    "label": "페이지",
    "previous": "이전",
    "next": "다음",
    "range": "{{from}}–{{to}} / {{total}}"
  },
  "common": {
    "loading": "불러오는 중…",
    "copied": "복사했습니다.",
    "copyFailed": "복사하지 못했습니다. 글을 선택해 직접 복사하세요.",
    "cancel": "취소",
    "save": "저장",
    "create": "만들기",
    "delete": "삭제",
    "deleteNamed": "{{name}} 삭제",
    "done": "완료",
    "name": "이름",
    "state": "상태",
    "node": "노드"
  },
  "confirm": {
    "typeName": "확인하려면 {{name}}을(를) 입력하세요"
  },
  "vms": {
    "vcpus": "vCPU",
    "memory": "메모리",
    "actions": "동작",
    "actionsFor": "{{name}} 동작",
    "search": "이름 검색",
    "empty": "가상 머신이 없습니다.",
    "selectAll": "모두 선택",
    "select": "{{name}} 선택",
    "selected_one": "{{count}}개 선택",
    "selected_other": "{{count}}개 선택",
    "deleted": "{{name}}을(를) 삭제했습니다.",
    "forceStopTitle": "{{name}}을(를) 강제 정지할까요?",
    "forceStopDescription": "게스트 OS를 종료하지 않고 VM을 끕니다. 저장하지 않은 게스트의 데이터를 잃습니다.",
    "reason": {
      "powerPending": "전원 작업이 아직 처리 중입니다.",
      "reverting": "스냅샷을 되돌리는 중에는 할 수 없습니다.",
      "running": "VM이 실행 중입니다.",
      "stopped": "VM이 정지되어 있습니다.",
      "failed": "VM이 실패 상태입니다.",
      "busy": "VM이 시작하거나 정지하는 중입니다."
    },
    "deleteTitle": "{{name}}을(를) 삭제할까요?",
    "deleteDescription": "VM을 삭제합니다. 볼륨은 남습니다.",
    "deleteSnapshots_one": "스냅샷 {{count}}개도 모두 지우기",
    "deleteSnapshots_other": "스냅샷 {{count}}개도 모두 지우기",
    "hasSnapshots": "스냅샷이 있는 VM은 지울 수 없습니다. 먼저 스냅샷을 지우거나, 위 칸을 골라 VM과 함께 지우세요.",
    "snapshotsPending": "이 VM의 스냅샷을 만드는 중입니다. 다 만든 뒤 VM을 지우세요.",
    "deletingSnapshots": "스냅샷을 하나씩 지우는 중입니다. 시작한 뒤에는 취소할 수 없습니다.",
    "phase": {
      "stopped": "정지",
      "starting": "시작 중",
      "running": "실행 중",
      "stopping": "정지 중",
      "failed": "실패"
    },
    "action": {
      "start": "시작",
      "stop": "정지",
      "force-stop": "강제 정지",
      "reboot": "재시작",
      "console": "콘솔",
      "delete": "삭제",
      "edit": "편집",
      "clone": "복제"
    },
    "notFound": "{{name}} VM이 없습니다",
    "backToList": "가상 머신 목록으로",
    "tab": {
      "summary": "요약",
      "hardware": "하드웨어",
      "console": "콘솔",
      "snapshots": "스냅샷",
      "tasks": "작업",
      "json": "JSON"
    },
    "cpu": "CPU",
    "cpuValue": "{{sockets}} × {{cores}}코어 ({{model}})",
    "firmware": "펌웨어",
    "machine": "머신 타입",
    "restart": "재시작 정책",
    "power": "원하는 전원 상태",
    "pid": "QEMU PID",
    "created": "만든 시각",
    "lastError": "마지막 오류",
    "restartPolicy": {
      "never": "하지 않음",
      "on-failure": "실패하면"
    },
    "powerState": {
      "running": "켜짐",
      "stopped": "꺼짐"
    },
    "disks": "디스크",
    "volume": "볼륨",
    "bus": "버스",
    "bootOrder": "부팅 순서",
    "nics": "네트워크 인터페이스",
    "network": "네트워크",
    "model": "모델",
    "mac": "MAC 주소",
    "openConsole": "콘솔 열기",
    "actionFailed": "{{name}}: {{error}}",
    "bulkSummary": "{{action}}: {{done}}개 요청, {{failed}}개 실패, {{skipped}}개 건너뜀.",
    "noDisks": "디스크가 없습니다.",
    "noNics": "네트워크 인터페이스가 없습니다."
  },
  "snapshots": {
    "created": "만든 시각",
    "memory": "메모리",
    "description": "설명",
    "empty": "스냅샷이 없습니다.",
    "current": "현재 위치",
    "after": "({{parent}} 다음)",
    "pending": "만드는 중",
    "memoryYes": "포함",
    "memoryNo": "없음",
    "memorySize": "포함 ({{size}})",
    "create": "스냅샷 만들기",
    "createTitle": "{{name}} 스냅샷 만들기",
    "createDescription": "VM의 디스크, 펌웨어 변수, 설정을 저장해 나중에 이 시점으로 돌아올 수 있게 합니다.",
    "includeMemory": "메모리 포함",
    "includeMemoryHint": "실행 상태도 저장합니다. 되돌리면 VM이 그때 상태에서 이어서 실행됩니다.",
    "unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. {{volume}}은(는) {{format}}입니다.",
    "busy": "VM이 시작하거나 정지하는 중입니다. 끝나면 스냅샷을 만들고 되돌리고 지울 수 있습니다.",
    "revert": "되돌리기",
    "revertNamed": "{{name}}(으)로 되돌리기",
    "revertTitle": "{{vm}}을(를) {{name}}(으)로 되돌릴까요?",
    "revertDescription": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌립니다. VM은 꺼진 채로 남습니다.",
    "revertDescriptionMemory": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌리고, 저장한 메모리에서 이어서 실행합니다.",
    "deleteNamed": "스냅샷 {{name}} 삭제",
    "deleteTitle": "스냅샷 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "스냅샷을 지웁니다. 그 뒤에 만든 스냅샷은 남고, VM의 지금 상태는 바뀌지 않습니다.",
    "op": {
      "create": "{{vm}} 스냅샷 {{name}} 만들기",
      "revert": "{{vm}}을(를) {{name}}(으)로 되돌리기",
      "delete": "{{vm}} 스냅샷 {{name}} 삭제"
    },
    "errors": {
      "exists": "이 VM에 같은 이름의 스냅샷이 이미 있습니다.",
      "description": "1024자까지 쓰세요."
    },
    "scheduled": "예약",
    "every": {
      "hourly": "매시간",
      "daily": "매일",
      "weekly": "매주"
    },
    "schedule": "예약 스냅샷: {{every}}, {{keep}}개 보관",
    "scheduleMemory": "예약 스냅샷: {{every}}, {{keep}}개 보관, 메모리 포함",
    "noSchedule": "스냅샷 예약이 없습니다.",
    "scheduleEdit": "예약 바꾸기",
    "scheduleFailed": "마지막 예약 스냅샷 실패:",
    "scheduleError": {
      "vm_busy": "VM이 다른 작업 중이라 건너뛰었습니다. 다음 기회에 다시 합니다.",
      "node_offline": "VM의 노드가 오프라인이라 건너뛰었습니다 ({{message}})."
    },
    "mixed": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. 이 VM에는 여러 방식이 섞여 있습니다.",
    "blockHint": "LVM-thin·ZFS·Ceph RBD 풀의 디스크는 메모리 없이 저장합니다. 실행 중인 VM은 저장하는 동안 잠깐 멈춥니다.",
    "zfsLatest": "ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
    "sharedLvm": "공유 LVM 풀의 디스크는 스냅샷을 만들 수 없습니다. {{volume}}이(가) 공유 LVM 풀에 있습니다."
  },
  "clone": {
    "title": "{{name}} 복제",
    "description": "디스크를 모두 복사해 꺼진 새 VM을 만듭니다. MAC 주소는 새로 정하고 예약 스냅샷은 없습니다.",
    "name": "새 VM 이름",
    "source": "원본",
    "current": "지금 상태(꺼진 VM만)",
    "snapshot": "스냅샷 {{name}}",
    "running": "VM이 꺼져 있지 않습니다. 먼저 정지하거나 스냅샷을 고르세요.",
    "busy": "VM이 시작하거나 정지하는 중입니다. 끝난 뒤 다시 시도하세요.",
    "gone": "고른 스냅샷이 없어졌거나 준비된 상태가 아닙니다. 원본을 다시 고르세요.",
    "goneOption": "고른 스냅샷(이제 없음)",
    "pool": "디스크를 둘 스토리지 풀",
    "samePool": "원본 디스크와 같은 풀",
    "submit": "복제",
    "op": "{{vm}}을(를) {{name}}(으)로 복제",
    "errors": {
      "exists": "같은 이름의 VM이 이미 있습니다.",
      "poolNotOnNode": "VM이 있는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "VM이 있는 노드에서 준비되지 않은 스토리지 풀입니다."
    }
  },
  "forms": {
    "name": "소문자·숫자·-로 1–63자, -로 시작하거나 끝날 수 없습니다."
  },
  "volumes": {
    "pool": "스토리지 풀",
    "size": "크기",
    "allocated": "사용량",
    "format": "형식",
    "vm": "사용하는 VM",
    "empty": "볼륨이 없습니다.",
    "phase": {
      "pending": "만드는 중",
      "ready": "준비됨"
    },
    "createTitle": "빈 볼륨 만들기",
    "createDescription": "스토리지 풀에 새 디스크 이미지를 만듭니다. qcow2 볼륨은 쓴 만큼만 공간을 차지합니다.",
    "deleteTitle": "볼륨 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "디스크 이미지와 그 안의 데이터가 모두 지워집니다.",
    "resize": "크기 늘리기",
    "resizeNamed": "{{name}} 크기 늘리기",
    "resizeTitle": "볼륨 {{name}} 크기 늘리기",
    "resizeDescription": "볼륨은 늘리기만 할 수 있습니다. 실행 중인 VM은 바로 새 크기를 봅니다. 게스트 안의 파티션과 파일 시스템은 직접 늘리세요.",
    "currentSize": "지금 크기: {{size}}",
    "newSize": "새 크기(GiB)",
    "errors": {
      "reserved": "import는 예약된 이름입니다.",
      "exists": "같은 이름의 볼륨이 이미 있습니다.",
      "grow": "지금 크기보다 큰 값을 넣으세요."
    },
    "reason": {
      "inUse": "먼저 VM에서 빼세요.",
      "notReady": "준비되면 쓸 수 있습니다.",
      "vmsUnknown": "어느 VM이 쓰는지 알 수 없습니다."
    }
  },
  "uploads": {
    "open": "이미지 올리기",
    "title": "디스크 이미지 올리기",
    "description": "파일은 지금 접속한 노드({{node}})로 올라가므로 그 노드에서 쓸 수 있는 스토리지 풀만 고를 수 있습니다. 이미지는 새 볼륨으로 변환됩니다.",
    "file": "이미지 파일",
    "sourceFormat": "파일 형식",
    "submit": "올리기",
    "noPool": "이 노드에서 쓸 수 있는 스토리지 풀이 없습니다.",
    "progress": "{{name}} 올리는 중: {{percent}}% ({{loaded}} / {{total}})",
    "cancelled": "{{name}} 업로드를 취소했습니다.",
    "failed": "{{name}} 업로드에 실패했습니다. {{error}}",
    "processing": "{{name}} 처리 중…",
    "noResponse": "{{name}}: 서버 응답이 없습니다. 볼륨 목록에서 결과를 확인하세요.",
    "dropped": "서버가 업로드를 거절했거나 연결이 끊겼습니다.",
    "noNode": "접속한 노드를 알 수 없습니다. {{error}}",
    "errors": {
      "file": "파일을 고르세요."
    }
  },
  "pools": {
    "path": "디렉터리",
    "nodes": "노드",
    "free": "남은 공간 / 전체",
    "volumes": "볼륨",
    "empty": "스토리지 풀이 없습니다.",
    "allNodes": "모든 노드",
    "createTitle": "스토리지 풀 만들기",
    "createDescription": "볼륨을 두는 곳입니다: 각 노드의 디렉터리, 노드들이 마운트하는 NFS 내보내기, 각 노드의 LVM thin pool이나 ZFS 데이터셋, Ceph RBD 풀, 또는 노드들이 함께 쓰는 볼륨 그룹.",
    "pathHint": "노드의 절대 경로입니다.",
    "shared": "공유 스토리지",
    "sharedHint": "모든 노드가 이 경로에서 같은 파일을 보면 고르세요(예: opLite 밖에서 마운트한 NFS 공유).",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일과 Ceph 풀의 이미지는 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "kind": "종류",
    "kinds": {
      "dir": "디렉터리",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD",
      "lvm-shared": "공유 LVM"
    },
    "location": "위치",
    "kindHint": {
      "dir": "먼저 각 노드에 디렉터리를 만드세요.",
      "nfs": "각 노드가 내보내기를 데이터 디렉터리 아래에 마운트합니다. 마운트는 root로 도는 Linux 노드에서만 됩니다. 풀의 노드들이 함께 쓰는 공유 풀입니다.",
      "lvm-thin": "먼저 각 노드에 볼륨 그룹과 thin pool을 만드세요. 볼륨은 만든 노드에 두는 raw thin 볼륨이고, 스냅샷은 디스크만 담습니다. root로 도는 Linux 노드에서만 됩니다.",
      "zfs": "먼저 각 노드에 데이터셋을 만드세요. 볼륨은 만든 노드에 두는 raw zvol이고, 스냅샷은 디스크만 담습니다. 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다. root로 도는 Linux 노드에서만 됩니다.",
      "rbd": "먼저 Ceph 풀을 만들고 각 노드에 ceph.conf와 키링을 두세요. 볼륨은 풀의 모든 노드가 쓰는 raw RBD 이미지이고, 스냅샷은 디스크만 담습니다. QEMU rbd 드라이버(qemu-block-extra)와 rbd 명령(ceph-common)이 있는 Linux 노드에서만 됩니다.",
      "lvm-shared": "먼저 모든 노드가 보는 LUN에 공유 볼륨 그룹(vgcreate --shared)을 만들고, 각 노드에서 sanlock이나 dlm과 함께 lvmlockd를 띄우세요. 볼륨은 풀의 모든 노드가 쓰는 raw 논리 볼륨이고, 스냅샷은 없습니다. root로 도는 Linux 노드에서만 됩니다."
    },
    "server": "NFS 서버",
    "serverHint": "호스트 이름 또는 IP 주소입니다.",
    "export": "내보내기 경로",
    "exportHint": "서버가 내보내는 절대 경로입니다(예: /srv/vm).",
    "options": "마운트 옵션(선택)",
    "optionsHint": "nosuid,nodev,noexec,retry=0 뒤에 붙여 mount -o에 넘길 쉼표 목록입니다(예: vers=4.2,hard).",
    "state": {
      "ready": "준비됨",
      "notReady": "준비 안 됨",
      "unknown": "아직 알린 상태 없음",
      "offline": "오프라인"
    },
    "errors": {
      "path": "절대 경로(/로 시작)를 입력하세요.",
      "export": "제어 문자 없는 절대 경로(/로 시작)를 입력하세요.",
      "nodesGone": "고른 노드가 더 이상 클러스터에 없습니다. 다시 고르세요.",
      "server": "공백·쉼표·빗금·대괄호 없이 호스트 이름이나 IP 주소를 입력하세요.",
      "options": "옵션은 공백 없이 쉼표로 나누세요. bind, rbind, remount, move, suid, dev, exec, helper, bg와 x-* 옵션은 쓸 수 없습니다.",
      "lvmName": "-, snapshot, pvmove로 시작하지 않는 1–127자의 영문자, 숫자, +, _, ., -를 쓰세요. LVM이 쓰는 .과 .., _tdata·_tmeta 같은 이름이 든 것도 쓸 수 없습니다.",
      "zfsDataset": "tank/opl처럼 177자 이하의 데이터셋 이름을 쓰세요: /로 나눈 각 부분은 영문자, 숫자, _, ., :, -로 되고 비어 있거나 . 또는 ..이거나 -로 시작하면 안 됩니다. 맨 앞은 풀 이름이고 영문자로 시작합니다.",
      "rbdName": "-로 시작하지 않는 1–63자의 영문자, 숫자, _, ., -를 쓰세요.",
      "cephUser": "Ceph 사용자는 \"client.\" 없이 쓰세요(client.opl이 아니라 opl).",
      "rbdConf": "제어 문자·쉼표·콜론·등호 없는 절대 경로(/로 시작)를 입력하세요.",
      "portal": "공백·쉼표·빗금 없이 호스트 이름이나 IP 주소를 입력하고, 필요하면 :포트(1–65535)를 붙이세요. IPv6 주소는 [fd00::5]:3260처럼 대괄호로 감싸세요.",
      "target": "iqn.·eui.·naa.로 시작하고 소문자, 숫자, ., :, -로만 된 223자 이하의 타깃 이름을 쓰세요."
    },
    "vg": "볼륨 그룹",
    "thinpool": "Thin pool",
    "dataset": "데이터셋",
    "cephPool": "Ceph 풀",
    "namespace": "네임스페이스(선택)",
    "cephUser": "Ceph 사용자(선택)",
    "cephUserHint": "client. 없는 Ceph 클라이언트 ID입니다. 비우면 admin입니다.",
    "cephConf": "ceph.conf 경로(선택)",
    "cephConfHint": "노드의 절대 경로입니다. 비우면 /etc/ceph/ceph.conf입니다.",
    "iscsi": "iSCSI 타깃에 로그인",
    "iscsiHint": "각 노드가 볼륨 그룹을 쓰기 전에 타깃에 로그인합니다. 파이버 채널처럼 노드가 이미 LUN을 보면 고르지 마세요.",
    "portal": "iSCSI 포털",
    "portalHint": "호스트 이름이나 IP 주소에 포트를 붙일 수 있습니다(예: san.lan:3260, [fd00::5]:3260).",
    "target": "iSCSI 타깃",
    "targetHint": "타깃 이름입니다(예: iqn.2003-01.org.example:storage)."
  },
  "networks": {
    "bridge": "브리지",
    "vlan": "VLAN",
    "vlanOptional": "VLAN 태그(선택)",
    "vlanHint": "1–4094. 브리지가 VLAN을 알아야 합니다(vlan_filtering 1).",
    "nodes": "브리지가 있는 노드",
    "vms": "VM",
    "empty": "네트워크가 없습니다.",
    "userNote": "user 네트워크(QEMU NAT)는 늘 쓸 수 있으며 목록에 나오지 않습니다.",
    "noNode": "이 브리지가 있는 노드가 없습니다",
    "noVlanFiltering": "{{nodes}}에서 VLAN 필터링이 꺼져 있습니다",
    "createTitle": "네트워크 만들기",
    "createDescription": "노드에 이미 있는 Linux 브리지에 VM 인터페이스를 연결합니다. 이 네트워크의 VM은 그 브리지가 있는 노드에서만 실행됩니다.",
    "created": "네트워크 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "네트워크 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "네트워크만 지웁니다. 노드의 브리지는 바꾸지 않습니다.",
    "deleted": "네트워크 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 VM 인터페이스를 빼세요.",
    "errors": {
      "reserved": "user는 NAT에 예약된 이름입니다.",
      "bridge": "/, :, 공백 없이 1–15자로 쓰세요.",
      "vlan": "1부터 4094까지의 숫자를 쓰세요."
    }
  },
  "nodes": {
    "address": "API 주소",
    "hypervisor": "하이퍼바이저",
    "arch": "아키텍처",
    "cpus": "CPU",
    "memory": "메모리",
    "bridges": "브리지",
    "pools": "스토리지 풀",
    "vms": "VM",
    "thisNode": "(이 노드)",
    "joinNote": "노드를 더하려면 클러스터 화면에서 조인 토큰을 발급한 뒤 새 노드에서 oplctl cluster join을 실행하세요.",
    "maintenanceNote": "유지보수 중인 노드에는 새 VM을 놓지 않습니다. 이미 실행 중인 VM은 그대로 둡니다.",
    "startMaintenance": "유지보수 시작",
    "endMaintenance": "유지보수 끝내기",
    "maintenanceOn": "{{name}}을(를) 유지보수 모드로 바꿨습니다.",
    "maintenanceOff": "{{name}}의 유지보수를 끝냈습니다.",
    "remove": "클러스터에서 제거",
    "removeTitle": "{{name}}을(를) 클러스터에서 제거할까요?",
    "removeDescription": "완전히 멈춘 노드에만 쓰세요. 지금 데이터로는 다시 합류할 수 없고, 이 노드에 배정된 VM은 그대로 남습니다.",
    "removeNote": "오프라인 노드만 여기서 제거할 수 있습니다. 살아 있는 노드는 그 노드에서 oplctl cluster leave를 실행하세요.",
    "removed": "{{name}}을(를) 클러스터에서 제거했습니다.",
    "reason": {
      "thisNode": "지금 이 노드로 접속해 있습니다.",
      "online": "노드가 온라인입니다. 그 노드에서 oplctl cluster leave를 실행하세요."
    }
  },
  "accounts": {
    "currentPassword": "현재 비밀번호",
    "changePassword": "내 비밀번호 변경",
    "changePasswordDescription": "내 세션이 모두 끝납니다. 새 비밀번호로 다시 로그인하세요.",
    "passwordChanged": "비밀번호를 바꿨습니다. 새 비밀번호로 로그인하세요.",
    "newPassword": "새 비밀번호",
    "confirmPassword": "새 비밀번호 확인",
    "passwordHint": "8자 이상입니다.",
    "errors": {
      "password": "8자 이상으로 쓰세요.",
      "mismatch": "두 비밀번호가 다릅니다.",
      "current": "현재 비밀번호를 입력하세요.",
      "wrongCurrent": "현재 비밀번호가 틀렸습니다."
    }
  },
  "users": {
    "role": "역할",
    "you": "(나)",
    "empty": "사용자가 없습니다.",
    "roleHint": "운영자는 VM과 볼륨을 바꾸고, 조회자는 읽기만 합니다. 둘 다 계정은 볼 수 없습니다.",
    "createTitle": "사용자 만들기",
    "created": "사용자 {{name}}을(를) 만들었습니다.",
    "changeRole": "역할 바꾸기",
    "roleTitle": "{{name}}의 역할 바꾸기",
    "roleChanged": "{{name}}의 역할을 {{role}}(으)로 바꿨습니다.",
    "resetPassword": "비밀번호 재설정",
    "passwordTitle": "{{name}}의 비밀번호 재설정",
    "passwordDescription": "그 사용자의 세션이 끝나고, 새 비밀번호로 다시 로그인합니다.",
    "passwordReset": "{{name}}의 비밀번호를 재설정했습니다.",
    "deleteTitle": "사용자 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "사용자와 그 사용자의 API 토큰이 지워집니다.",
    "deleted": "사용자 {{name}}을(를) 삭제했습니다.",
    "adminOnly": "사용자는 관리자만 관리할 수 있습니다.",
    "selfNote": "내 비밀번호는 계정 메뉴에서 바꿉니다. 내 역할 변경과 삭제는 다른 관리자가 합니다.",
    "reason": {
      "selfRole": "내 역할은 다른 관리자가 바꿉니다.",
      "selfPassword": "내 비밀번호는 계정 메뉴에서 바꿉니다.",
      "selfDelete": "내 계정은 다른 관리자가 삭제합니다."
    }
  },
  "tokens": {
    "owner": "주인",
    "created": "만든 시각",
    "empty": "API 토큰이 없습니다.",
    "note": "스크립트와 도구는 API 토큰을 씁니다. 토큰의 권한은 주인의 역할을 따릅니다. oplctl login은 CLI용 토큰을 만듭니다.",
    "createTitle": "API 토큰 만들기",
    "createDescription": "토큰은 내 역할로 나 대신 요청합니다.",
    "secretTitle": "토큰 {{name}}을(를) 만들었습니다",
    "secretDescription": "지금 토큰을 복사하세요. 다시 볼 수 없습니다.",
    "secret": "토큰",
    "copySecret": "토큰 복사",
    "usage": "요청 헤더에 넣어 보냅니다",
    "deleteTitle": "API 토큰 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 토큰을 쓰는 곳은 곧바로 동작하지 않습니다.",
    "deleted": "API 토큰 {{name}}을(를) 삭제했습니다."
  },
  "cluster": {
    "title": "클러스터 {{name}}",
    "ca": "인증 기관(CA)",
    "caNote": "모든 노드의 HTTPS 인증서는 클러스터 CA가 서명합니다. 인증서 경고를 없애려면 CA를 브라우저에 가져오고, 지문이 같은지 확인하세요. 브라우저는 SHA-256 지문을 두 자리씩 콜론이나 공백으로 나눈 대문자 16진수로 보여 줍니다. sha256: 뒤의 16진수와 비교하세요(대소문자는 상관없습니다).",
    "copyFingerprint": "CA 지문 복사",
    "downloadCa": "CA 내려받기",
    "members": "멤버",
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다.",
    "refreshFailed": "클러스터를 다시 읽지 못했습니다. {{error}}",
    "role": "역할",
    "leader": "리더",
    "voter": "투표 멤버",
    "learner": "학습자",
    "clusterAddress": "클러스터 주소",
    "issue": "조인 토큰 발급",
    "issueTitle": "조인 토큰 발급",
    "issueDescription": "새 노드가 이 토큰으로 한 번 합류합니다. 먼저 새 노드에서 opld를 시작하세요.",
    "ttl": "유효 기간",
    "ttlOption": {
      "3600": "1시간",
      "86400": "1일",
      "604800": "7일"
    },
    "issuedTitle": "새 노드 합류",
    "issuedDescription": "{{time}} 전에 새 노드에서 실행하세요. 다시 볼 수 없습니다.",
    "command": "명령",
    "copyCommand": "명령 복사",
    "commandNote": "새 노드의 opld가 /run/opl/opld.sock을 쓰지 않으면 --socket을 붙이세요."
  },
  "vmForm": {
    "createTitle": "가상 머신 만들기",
    "editTitle": "{{name}} 편집",
    "runningNote": "VM이 실행 중이라 노드와 디스크는 바꿀 수 없습니다. 다른 변경은 다음 시작부터 적용됩니다.",
    "general": "일반",
    "cores": "vCPU 코어",
    "memory": "메모리 (MiB)",
    "cpuModel": "CPU 모델",
    "nodeAuto": "자동",
    "diskN": "디스크 {{n}}",
    "newVolume": "새 볼륨",
    "pool": "스토리지 풀",
    "sizeGib": "크기 (GiB)",
    "removeDisk": "디스크 {{n}} 빼기",
    "addExisting": "볼륨 붙이기",
    "addNew": "새 빈 볼륨",
    "nicN": "네트워크 인터페이스 {{n}}",
    "userNetwork": "user (NAT)",
    "macAuto": "자동",
    "removeNic": "인터페이스 {{n}} 빼기",
    "addNic": "인터페이스 추가",
    "startAfter": "만든 뒤 시작",
    "reload": "다시 불러오기",
    "errors": {
      "name": "소문자·숫자·-로 1~63자, -로 시작하거나 끝날 수 없습니다.",
      "cores": "1에서 65535 사이여야 합니다.",
      "memory": "64 MiB 이상이어야 합니다.",
      "volume": "볼륨을 고르거나 이름을 정하세요.",
      "pool": "스토리지 풀을 고르세요.",
      "size": "크기를 입력하세요.",
      "bootOrder": "부팅 순서는 1부터 255까지 겹치지 않는 숫자여야 합니다.",
      "mac": "52:54:00:12:34:56 형식으로 입력하세요.",
      "volumeChoose": "볼륨을 고르세요.",
      "volumeDuplicate": "디스크마다 다른 볼륨이어야 합니다.",
      "volumeExists": "같은 이름의 볼륨이 이미 있습니다.",
      "macMulticast": "멀티캐스트 주소(첫 바이트가 홀수)는 쓸 수 없습니다.",
      "macDuplicate": "인터페이스마다 다른 MAC 주소여야 합니다.",
      "keep": "1에서 100 사이여야 합니다."
    },
    "volumesCreated": "볼륨 {{names}}은(는) 만들어졌습니다. {{error}}",
    "schedule": "예약 스냅샷",
    "scheduleNote": "주기마다 스냅샷을 만들고 예약으로 만든 것은 최근 것만 남깁니다. 직접 만든 스냅샷은 지우지 않습니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있어야 합니다.",
    "scheduleEvery": "스냅샷 주기",
    "scheduleOff": "끔",
    "scheduleKeep": "보관 개수",
    "scheduleRaw": "{{volume}}이(가) {{format}}이라 예약 스냅샷이 실패합니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있어야 합니다.",
    "scheduleMemoryHint": "VM이 실행 중이고 디스크가 있을 때만 메모리도 저장합니다.",
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀, Ceph RBD 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다.",
    "scheduleSharedLvm": "{{volume}}이(가) 공유 LVM 풀에 있어 예약 스냅샷이 실패합니다."
  },
  "console": {
    "connecting": "연결 중…",
    "connected": "연결됨",
    "failed": "연결이 끊겼습니다. VM이 정지했거나 콘솔을 열 수 없습니다.",
    "reconnect": "다시 연결",
    "paste": "클립보드 입력",
    "pasteDenied": "브라우저가 클립보드 읽기를 허락하지 않았습니다.",
    "fullscreen": "전체 화면",
    "pasteTruncated": "앞의 {{count}}자만 입력했습니다.",
    "screen": "VM 화면",
    "hint": "화면을 누르면 VM에 입력합니다. 화면 밖을 누르면 키보드를 돌려받습니다.",
    "forbidden": "콘솔을 쓸 권한이 없습니다."
  },
  "dashboard": {
    "nodes": "노드",
    "online": "온라인",
    "vms": "가상 머신",
    "vmCounts": "실행 {{running}} · 정지 {{stopped}} · 실패 {{failed}}",
    "leader": "리더",
    "perNode": "노드",
    "hypervisor": "하이퍼바이저",
    "activeVms": "활성 VM",
    "nodeOnline": "온라인",
    "nodeOffline": "오프라인",
    "maintenance": "유지보수",
    "failedVms": "실패한 가상 머신",
    "recentTasks": "최근 작업",
    "allTasks": "모든 작업",
    "noLeader": "리더 없음",
    "noNodes": "노드가 없습니다."
  }
}
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
      pool("ceph", { backend: { kind: "rbd", pool: "rbd", namespace: "opl" } }),
    ],
```

을 다음으로 바꾼다:

```tsx
      pool("ceph", { backend: { kind: "rbd", pool: "rbd", namespace: "opl" } }),
      pool("san", {
        backend: {
          kind: "lvm-shared",
          vg: "san",
          iscsi: { portal: "10.0.0.5:3260", target: "iqn.2003-01.org:opl" },
        },
      }),
    ],
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(ceph).toHaveTextContent("rbd/opl");
});
```

을 다음으로 바꾼다:

```tsx
  expect(ceph).toHaveTextContent("rbd/opl");
  const san = within(table).getByText("san").closest("tr")!;
  expect(san).toHaveTextContent("Shared LVM");
  expect(san).toHaveTextContent("san @ 10.0.0.5:3260 iqn.2003-01.org:opl");
});
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx

it("sends NFS mount options", async () => {
```

을 다음으로 바꾼다:

```tsx

it("creates a shared LVM pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("san"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "san");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "lvm-shared");
  expect(within(dialog).getByLabelText("Kind")).toHaveAccessibleDescription(
    "Create the shared volume group (vgcreate --shared) on a LUN every node sees first, and run lvmlockd with sanlock or dlm on each node. Volumes are raw logical volumes that every node of the pool can use, without snapshots. Needs Linux nodes running as root.",
  );
  // 늘 공유라 고를 것이 없고, 다른 종류의 입력도 없다.
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  expect(within(dialog).queryByLabelText("Thin pool")).toBeNull();
  // iSCSI는 고를 때만 묻는다.
  const iscsi = within(dialog).getByRole("checkbox", { name: "Log in to an iSCSI target" });
  expect(iscsi).toHaveAccessibleDescription(
    "Each node logs in to the target before using the volume group. Leave unchecked when the nodes already see the LUN, such as over Fibre Channel.",
  );
  expect(within(dialog).queryByLabelText("iSCSI portal")).toBeNull();
  await userEvent.type(within(dialog).getByLabelText("Volume group"), "-san");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Volume group")).toHaveAccessibleDescription(
    "Use 1 to 127 letters, digits, +, _, . or -, not starting with -, snapshot or pvmove. LVM also reserves . and .. and names containing parts like _tdata or _tmeta.",
  );
  expect(body).toBeUndefined();

  // 서버처럼 포털과 타깃을 검사한다.
  await userEvent.click(iscsi);
  expect(within(dialog).getByLabelText("iSCSI portal")).toHaveAccessibleDescription(
    "Host name or IP address with an optional port, such as san.lan:3260 or [fd00::5]:3260.",
  );
  expect(within(dialog).getByLabelText("iSCSI target")).toHaveAccessibleDescription(
    "The target name, such as iqn.2003-01.org.example:storage.",
  );
  await userEvent.type(within(dialog).getByLabelText("iSCSI portal"), "fd00::5");
  await userEvent.type(within(dialog).getByLabelText("iSCSI target"), "IQN.2003-01.org:opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("iSCSI portal")).toHaveAccessibleDescription(
    "Enter a host name or IP address with an optional :port (1–65535), without spaces, commas or slashes. Put IPv6 addresses in brackets, such as [fd00::5]:3260.",
  );
  expect(within(dialog).getByLabelText("iSCSI target")).toHaveAccessibleDescription(
    "Use a target name starting with iqn., eui. or naa., at most 223 characters of lowercase letters, digits, ., : or -.",
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Volume group"));
  await userEvent.type(within(dialog).getByLabelText("Volume group"), "san");
  await userEvent.clear(within(dialog).getByLabelText("iSCSI portal"));
  await userEvent.type(within(dialog).getByLabelText("iSCSI portal"), "[[fd00::5]:3260"); // `[[`는 글자 `[`다.
  await userEvent.clear(within(dialog).getByLabelText("iSCSI target"));
  await userEvent.type(within(dialog).getByLabelText("iSCSI target"), "iqn.2003-01.org:opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool san.")).toBeInTheDocument();
  // 공유 여부는 서버가 정한다.
  expect(body).toEqual({
    name: "san",
    spec: {
      backend: {
        kind: "lvm-shared",
        vg: "san",
        iscsi: { portal: "[fd00::5]:3260", target: "iqn.2003-01.org:opl" },
      },
      nodes: [],
    },
  });
});

it("creates a shared LVM pool without iSCSI, ignoring a portal typed before unchecking", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("san"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "san");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "lvm-shared");
  await userEvent.type(within(dialog).getByLabelText("Volume group"), "san");
  const iscsi = within(dialog).getByRole("checkbox", { name: "Log in to an iSCSI target" });
  await userEvent.click(iscsi);
  await userEvent.type(within(dialog).getByLabelText("iSCSI portal"), "bad portal");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("iSCSI portal")).toHaveAccessibleDescription(
    /^Enter a host name/,
  );
  expect(body).toBeUndefined();
  // iSCSI를 바꾸면 포털·타깃의 오류를 지운다(다시 골라도 남지 않는다).
  await userEvent.click(iscsi);
  await userEvent.click(iscsi);
  expect(within(dialog).getByLabelText("iSCSI portal")).toHaveAccessibleDescription(
    "Host name or IP address with an optional port, such as san.lan:3260 or [fd00::5]:3260.",
  );
  expect(within(dialog).getByLabelText("iSCSI target")).toHaveAccessibleDescription(
    "The target name, such as iqn.2003-01.org.example:storage.",
  );
  await userEvent.click(iscsi);
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "san",
      spec: { backend: { kind: "lvm-shared", vg: "san" }, nodes: [] },
    }),
  );
});

it("sends NFS mount options", async () => {
```

`web/src/storage/PoolsPage.tsx` 전체:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { StoragePool } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes } from "@/lib/format";
import {
  cephNameError,
  cephUserError,
  iscsiPortalError,
  iscsiTargetError,
  lvmNameError,
  nfsExportError,
  nfsOptionsError,
  nfsServerError,
  poolCapacity,
  poolLocation,
  poolNodes,
  rbdConfError,
  zfsDatasetError,
} from "@/storage/pools";
import { PoolStatus } from "@/storage/PoolStatus";
import { validName } from "@/vms/form";

export function PoolsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<StoragePool | null>(null);
  const write = can(me.role, "write", "storage-pool");

  const nodeList = useMemo(() => nodes.data ?? [], [nodes.data]);
  // 볼륨 목록을 다 읽기 전에는 모른다(undefined).
  const volumeCount = (pool: StoragePool) =>
    volumes.isSuccess ? volumes.data.filter((v) => v.spec.pool === pool.name).length : undefined;

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="pools-title" className="text-2xl font-semibold">
          {t("nav.pools")}
        </h1>
        {write && (
          <Button onClick={() => setCreating(true)}>
            <Plus />
            {t("common.create")}
          </Button>
        )}
      </div>
      <Table aria-labelledby="pools-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("pools.kind")}</TableHead>
            <TableHead>{t("pools.location")}</TableHead>
            <TableHead>{t("pools.nodes")}</TableHead>
            <TableHead>{t("pools.free")}</TableHead>
            <TableHead>{t("pools.volumes")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(pools.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 7 : 6} query={pools} empty={t("pools.empty")} />
          )}
          {(pools.data ?? []).map((pool) => {
            const capacity = poolCapacity(pool, nodeList);
            const used = volumeCount(pool);
            return (
              <TableRow key={pool.id}>
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell>{t(`pools.kinds.${pool.spec.backend.kind}`)}</TableCell>
                <TableCell className="font-mono text-xs">{poolLocation(pool)}</TableCell>
                <TableCell>
                  {/* 노드를 정하지 않은 풀은 모든 노드에서 쓴다. */}
                  {(pool.spec.nodes ?? []).length === 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.allNodes")}</div>
                  )}
                  {poolNodes(pool, nodeList).map((n) => (
                    <PoolStatus key={n.id} name={n.name} state={n.state} offline={n.offline} />
                  ))}
                </TableCell>
                <TableCell>
                  {/* 공유 풀은 값 하나, 로컬 풀은 준비된 노드마다(노드가 둘 이상이면 이름을 붙인다). */}
                  {capacity.length === 0
                    ? "—"
                    : capacity.map((c) => (
                        <div key={c.node ?? ""}>
                          {capacity.length > 1 && `${c.node}: `}
                          {`${formatBytes(c.free)} / ${formatBytes(c.total)}`}
                        </div>
                      ))}
                </TableCell>
                <TableCell>
                  {used ?? "—"}
                  {write && used !== undefined && used > 0 && (
                    <div className="text-xs text-muted-foreground">{t("pools.inUse")}</div>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("common.deleteNamed", { name: pool.name })}
                      // 볼륨이 남은 풀은 서버가 지우지 않는다.
                      disabled={used !== 0}
                      onClick={() => setDeleting(pool)}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreatePoolDialog onClose={() => setCreating(false)} />}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("pools.deleteTitle", { name: deleting?.name })}
        description={t("pools.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const pool = deleting!;
          await call(api.DELETE("/storage-pools/{key}", { params: { path: { key: pool.id } } }));
          toast.success(t("pools.deleted", { name: pool.name }));
        }}
      />
    </div>
  );
}

type Kind = StoragePool["spec"]["backend"]["kind"];

function CreatePoolDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<Kind>("dir");
  const [path, setPath] = useState("");
  const [shared, setShared] = useState(false);
  const [nfsServer, setNfsServer] = useState("");
  const [nfsExport, setNfsExport] = useState("");
  const [nfsOptions, setNfsOptions] = useState("");
  const [vg, setVg] = useState("");
  const [thinpool, setThinpool] = useState("");
  const [dataset, setDataset] = useState("");
  const [cephPool, setCephPool] = useState("");
  const [namespace, setNamespace] = useState("");
  const [cephUser, setCephUser] = useState("");
  const [cephConf, setCephConf] = useState("");
  const [iscsi, setIscsi] = useState(false);
  const [portal, setPortal] = useState("");
  const [target, setTarget] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (kind === "dir" && !path.startsWith("/")) {
      found.path = "pools.errors.path";
    }
    if (kind === "nfs") {
      const server = nfsServerError(nfsServer);
      if (server) {
        found.server = server;
      }
      const exportError = nfsExportError(nfsExport);
      if (exportError) {
        found.export = exportError;
      }
      const optionsError = nfsOptions === "" ? undefined : nfsOptionsError(nfsOptions);
      if (optionsError) {
        found.options = optionsError;
      }
    }
    if (kind === "lvm-thin") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      const thinpoolError = lvmNameError(thinpool);
      if (thinpoolError) {
        found.thinpool = thinpoolError;
      }
    }
    if (kind === "lvm-shared") {
      const vgError = lvmNameError(vg);
      if (vgError) {
        found.vg = vgError;
      }
      // iSCSI를 고르지 않았으면 포털·타깃은 보지 않고 보내지도 않는다.
      const portalError = iscsi ? iscsiPortalError(portal) : undefined;
      if (portalError) {
        found.portal = portalError;
      }
      const targetError = iscsi ? iscsiTargetError(target) : undefined;
      if (targetError) {
        found.target = targetError;
      }
    }
    if (kind === "zfs") {
      const datasetError = zfsDatasetError(dataset);
      if (datasetError) {
        found.dataset = datasetError;
      }
    }
    if (kind === "rbd") {
      const poolError = cephNameError(cephPool);
      if (poolError) {
        found.cephPool = poolError;
      }
      // 네임스페이스·사용자·conf는 비우면 보내지 않는다(Ceph 기본값).
      const namespaceError = namespace === "" ? undefined : cephNameError(namespace);
      if (namespaceError) {
        found.namespace = namespaceError;
      }
      const userError = cephUser === "" ? undefined : cephUserError(cephUser);
      if (userError) {
        found.cephUser = userError;
      }
      const confError = cephConf === "" ? undefined : rbdConfError(cephConf);
      if (confError) {
        found.cephConf = confError;
      }
    }
    // 고른 뒤에 목록에서 빠진 노드는 보내지 않는다.
    const known = chosen.filter((id) => nodes.data?.some((n) => n.id === id));
    // 고른 노드가 모두 빠졌으면 빈 목록(모든 노드)으로 보내지 않는다.
    // 빠진 노드는 고른 목록에서도 지워, 다시 보내면 모든 노드로 만들 수 있게 한다.
    if (chosen.length > 0 && known.length === 0) {
      found.nodes = "pools.errors.nodesGone";
      setChosen(known);
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    // NFS·RBD·공유 LVM 풀의 `shared`는 보내지 않는다(서버가 늘 공유로 둔다). 빈 옵션은 커널 기본값이다.
    // LVM-thin·ZFS 풀은 노드 로컬이라 `shared`를 보내지 않는다.
    const sharedDir = kind === "dir" && shared;
    const backend =
      kind === "dir"
        ? { kind, path }
        : kind === "lvm-thin"
          ? { kind, vg, thinpool }
          : kind === "zfs"
            ? { kind, dataset }
            : kind === "lvm-shared"
              ? { kind, vg, ...(iscsi && { iscsi: { portal, target } }) }
              : kind === "rbd"
                ? {
                    kind,
                    pool: cephPool,
                    ...(namespace !== "" && { namespace }),
                    ...(cephUser !== "" && { user: cephUser }),
                    ...(cephConf !== "" && { conf: cephConf }),
                  }
                : {
                    kind,
                    server: nfsServer,
                    export: nfsExport,
                    ...(nfsOptions !== "" && { options: nfsOptions }),
                  };
    const spec = { backend, nodes: known, ...(sharedDir && { shared: true }) };
    await call(api.POST("/storage-pools", { body: { name, spec } }));
    toast.success(t("pools.created", { name }));
  }

  return (
    <FormDialog
      title={t("pools.createTitle")}
      description={t("pools.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="pool-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input id="pool-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      <Field id="pool-kind" label={t("pools.kind")} hint={t(`pools.kindHint.${kind}`)}>
        {(a11y) => (
          <NativeSelect
            id="pool-kind"
            className="w-full"
            value={kind}
            onChange={(e) => setKind(e.target.value as Kind)}
            {...a11y}
          >
            <NativeSelectOption value="dir">{t("pools.kinds.dir")}</NativeSelectOption>
            <NativeSelectOption value="nfs">{t("pools.kinds.nfs")}</NativeSelectOption>
            <NativeSelectOption value="lvm-thin">{t("pools.kinds.lvm-thin")}</NativeSelectOption>
            <NativeSelectOption value="zfs">{t("pools.kinds.zfs")}</NativeSelectOption>
            <NativeSelectOption value="rbd">{t("pools.kinds.rbd")}</NativeSelectOption>
            <NativeSelectOption value="lvm-shared">
              {t("pools.kinds.lvm-shared")}
            </NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      {kind === "lvm-thin" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="vg0"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-thinpool" label={t("pools.thinpool")} error={errors.thinpool}>
            {(a11y) => (
              <Input
                id="pool-thinpool"
                placeholder="thinpool"
                value={thinpool}
                onChange={(e) => setThinpool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "lvm-shared" && (
        <>
          <Field id="pool-vg" label={t("pools.vg")} error={errors.vg}>
            {(a11y) => (
              <Input
                id="pool-vg"
                placeholder="san"
                value={vg}
                onChange={(e) => setVg(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* FC·멀티패스처럼 노드가 이미 LUN을 보면 로그인하지 않는다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-iscsi"
                checked={iscsi}
                onCheckedChange={(v) => {
                  setIscsi(v === true);
                  // 바꾸면 포털·타깃의 앞 오류는 맞지 않으므로 지운다.
                  setErrors((prev) => {
                    const next = { ...prev };
                    delete next.portal;
                    delete next.target;
                    return next;
                  });
                }}
                aria-describedby="pool-iscsi-hint"
              />
              <Label htmlFor="pool-iscsi">{t("pools.iscsi")}</Label>
            </div>
            <p id="pool-iscsi-hint" className="text-xs text-muted-foreground">
              {t("pools.iscsiHint")}
            </p>
          </div>
          {iscsi && (
            <>
              <Field
                id="pool-portal"
                label={t("pools.portal")}
                error={errors.portal}
                hint={t("pools.portalHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-portal"
                    placeholder="san.lan:3260"
                    value={portal}
                    onChange={(e) => setPortal(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
              <Field
                id="pool-target"
                label={t("pools.target")}
                error={errors.target}
                hint={t("pools.targetHint")}
              >
                {(a11y) => (
                  <Input
                    id="pool-target"
                    placeholder="iqn.2003-01.org.example:storage"
                    value={target}
                    onChange={(e) => setTarget(e.target.value)}
                    {...a11y}
                  />
                )}
              </Field>
            </>
          )}
        </>
      )}
      {kind === "zfs" && (
        <Field id="pool-dataset" label={t("pools.dataset")} error={errors.dataset}>
          {(a11y) => (
            <Input
              id="pool-dataset"
              placeholder="tank/opl"
              value={dataset}
              onChange={(e) => setDataset(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      )}
      {kind === "rbd" && (
        <>
          <Field id="pool-ceph-pool" label={t("pools.cephPool")} error={errors.cephPool}>
            {(a11y) => (
              <Input
                id="pool-ceph-pool"
                placeholder="rbd"
                value={cephPool}
                onChange={(e) => setCephPool(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field id="pool-namespace" label={t("pools.namespace")} error={errors.namespace}>
            {(a11y) => (
              <Input
                id="pool-namespace"
                value={namespace}
                onChange={(e) => setNamespace(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-user"
            label={t("pools.cephUser")}
            error={errors.cephUser}
            hint={t("pools.cephUserHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-user"
                value={cephUser}
                onChange={(e) => setCephUser(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-ceph-conf"
            label={t("pools.cephConf")}
            error={errors.cephConf}
            hint={t("pools.cephConfHint")}
          >
            {(a11y) => (
              <Input
                id="pool-ceph-conf"
                value={cephConf}
                onChange={(e) => setCephConf(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      {kind === "dir" && (
        <>
          <Field
            id="pool-path"
            label={t("pools.path")}
            error={errors.path}
            hint={t("pools.pathHint")}
          >
            {(a11y) => (
              <Input
                id="pool-path"
                placeholder="/var/lib/opl/pool"
                value={path}
                onChange={(e) => setPath(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          {/* 밖에서 마운트한 공유 디렉터리. 공유가 아닌 풀의 VM은 볼륨이 있는 노드에서만 돈다. */}
          <div className="flex flex-col gap-1.5">
            <div className="flex items-center gap-2">
              <Checkbox
                id="pool-shared"
                checked={shared}
                onCheckedChange={(v) => setShared(v === true)}
                aria-describedby="pool-shared-hint"
              />
              <Label htmlFor="pool-shared">{t("pools.shared")}</Label>
            </div>
            <p id="pool-shared-hint" className="text-xs text-muted-foreground">
              {t("pools.sharedHint")}
            </p>
          </div>
        </>
      )}
      {kind === "nfs" && (
        <>
          <Field
            id="pool-server"
            label={t("pools.server")}
            error={errors.server}
            hint={t("pools.serverHint")}
          >
            {(a11y) => (
              <Input
                id="pool-server"
                placeholder="nas.lan"
                value={nfsServer}
                onChange={(e) => setNfsServer(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-export"
            label={t("pools.export")}
            error={errors.export}
            hint={t("pools.exportHint")}
          >
            {(a11y) => (
              <Input
                id="pool-export"
                placeholder="/srv/vm"
                value={nfsExport}
                onChange={(e) => setNfsExport(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="pool-options"
            label={t("pools.options")}
            error={errors.options}
            hint={t("pools.optionsHint")}
          >
            {(a11y) => (
              <Input
                id="pool-options"
                value={nfsOptions}
                onChange={(e) => setNfsOptions(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
      <fieldset
        className="flex flex-col gap-2"
        aria-describedby={errors.nodes ? "pool-nodes-error" : undefined}
      >
        <legend className="mb-2 text-sm font-medium">{t("pools.nodes")}</legend>
        {!nodes.isSuccess && <ListPending error={nodes.error} />}
        {(nodes.data ?? []).map((node) => (
          <div key={node.id} className="flex items-center gap-2">
            <Checkbox
              id={`pool-node-${node.id}`}
              checked={chosen.includes(node.id)}
              onCheckedChange={(v) =>
                setChosen((c) => (v === true ? [...c, node.id] : c.filter((id) => id !== node.id)))
              }
            />
            <Label htmlFor={`pool-node-${node.id}`}>{node.name}</Label>
          </div>
        ))}
        <p className="text-xs text-muted-foreground">{t("pools.nodesHint")}</p>
        {errors.nodes && (
          <p id="pool-nodes-error" className="text-xs text-destructive">
            {t(errors.nodes)}
          </p>
        )}
      </fieldset>
    </FormDialog>
  );
}
```

`web/src/storage/pools.test.ts`에서

```ts
  cephUserError,
  lvmNameError,
```

을 다음으로 바꾼다:

```ts
  cephUserError,
  iscsiPortalError,
  iscsiTargetError,
  lvmNameError,
```

`web/src/storage/pools.test.ts`에서

```ts

it("offers the volume formats a pool holds, the default first", () => {
```

을 다음으로 바꾼다:

```ts

it("checks iSCSI portals like the server", () => {
  for (const ok of [
    "san.lan",
    "san.lan:3260",
    "10.0.0.5",
    "10.0.0.5:1",
    "NAS-01:65535",
    "[fd00::5]",
    "[::1]:3260",
    "[::ffff:10.0.0.5]:3260",
  ]) {
    expect(iscsiPortalError(ok)).toBeUndefined();
  }
  for (const bad of [
    "",
    "san lan",
    "-oremount",
    "a,b",
    "a/b",
    "san:",
    "san:0",
    "san:65536",
    "san:+80",
    "san:32a",
    "san:3260:1",
    "산",
    // IPv6는 포트와 헷갈리지 않게 `[...]`로 감싼다.
    "fd00::5",
    "[fd00::5",
    "[nas]:3260",
    "[fd00::5]3260",
    "[fd00::5]:",
    // 대괄호 안은 IPv6 주소여야 한다.
    "[:]",
    "[1:2:3]",
    "[::1.2]",
    "[fd00::5%eth0]",
    ":3260",
  ]) {
    expect(iscsiPortalError(bad)).toBe("pools.errors.portal");
  }
});

it("checks iSCSI target names like the server", () => {
  for (const ok of [
    "iqn.2003-01.org.linux-iscsi.opl:shared",
    "eui.02004567a425678d",
    "naa.52004567ba64678d",
    `iqn.${"a".repeat(219)}`,
  ]) {
    expect(iscsiTargetError(ok)).toBeUndefined();
  }
  for (const bad of [
    "",
    "iqn.",
    "opl",
    "IQN.2003-01.org:opl",
    "iqn.2003-01.org:OPL",
    "iqn.2003-01.org:my opl",
    "iqn.2003-01.org:a,b",
    "iqn.2003-01.org:a_b",
    "-iqn.2003",
    `iqn.${"a".repeat(220)}`,
  ]) {
    expect(iscsiTargetError(bad)).toBe("pools.errors.target");
  }
});

it("offers the volume formats a pool holds, the default first", () => {
```

`web/src/storage/pools.test.ts`에서

```ts
  expect(poolFormats(rbd())).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
```

을 다음으로 바꾼다:

```ts
  expect(poolFormats(rbd())).toEqual(["raw"]);
  // 공유 LVM 풀은 raw LV만 두고 스냅샷이 없다.
  expect(poolFormats(lvmShared())).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
```

`web/src/storage/pools.ts`에서

```ts

/** 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS, RBD)의 종류. 아니면(파일 풀, 모름) `undefined`. */
```

을 다음으로 바꾼다:

```ts

/** 포트(`:` 없이)가 서버처럼 숫자만으로 된 1–65535인지 */
function validPort(port: string): boolean {
  return /^[0-9]+$/.test(port) && Number(port) >= 1 && Number(port) <= 65535;
}

/** 대괄호 없는 IPv6 주소인지(서버의 `Ipv6Addr`처럼 브라우저의 URL 파서로 읽어 본다) */
function validIpv6(addr: string): boolean {
  if (!/^[0-9A-Fa-f:.]+$/.test(addr)) return false;
  try {
    new URL(`http://[${addr}]/`);
    return true;
  } catch {
    return false;
  }
}

/**
 * iSCSI 포털 입력 오류(번역 키). 서버처럼 `호스트[:포트]` 또는 `[IPv6][:포트]`이고 포트는 1–65535다.
 * 호스트는 `-`로 시작하지 않고 공백·`:`·`,`·`/`·`[`·`]` 없는 ASCII다.
 */
export function iscsiPortalError(portal: string): string | undefined {
  let hostOk: boolean;
  let port: string;
  if (portal.startsWith("[")) {
    const end = portal.indexOf("]");
    const addr = end < 0 ? "" : portal.slice(1, end);
    hostOk = end >= 0 && validIpv6(addr);
    port = end < 0 ? "" : portal.slice(end + 1);
  } else {
    const colon = portal.indexOf(":");
    const host = colon < 0 ? portal : portal.slice(0, colon);
    hostOk = host !== "" && !host.startsWith("-") && /^[!-~]+$/.test(host) && !/[,/[\]]/.test(host);
    port = colon < 0 ? "" : portal.slice(colon);
  }
  const portOk = port === "" || (port.startsWith(":") && validPort(port.slice(1)));
  return hostOk && portOk ? undefined : "pools.errors.portal";
}

/**
 * iSCSI 타깃 이름 입력 오류(번역 키). 서버처럼 `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`로만
 * 된 223자 이하여야 한다.
 */
export function iscsiTargetError(target: string): string | undefined {
  return target.length <= 223 && /^(iqn|eui|naa)\.[a-z0-9.:-]+$/.test(target)
    ? undefined
    : "pools.errors.target";
}

/** 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS, RBD)의 종류. 아니면(파일 풀, 모름) `undefined`. */
```

`web/src/storage/pools.ts`에서

```ts
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). 블록 풀(LVM-thin, ZFS, RBD)은 raw만 둔다. 풀을 모르면 파일 풀처럼
 * 본다.
 */
export function poolFormats(pool: StoragePool | undefined): [ImageFormat, ...ImageFormat[]] {
  return blockSnapshotKind(pool) ? ["raw"] : ["qcow2", "raw"];
}
```

을 다음으로 바꾼다:

```ts
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 둔다(공유 LVM 풀은
 * 스냅샷이 없다). 풀을 모르면 파일 풀처럼 본다.
 */
export function poolFormats(pool: StoragePool | undefined): [ImageFormat, ...ImageFormat[]] {
  return blockSnapshotKind(pool) || pool?.spec.backend.kind === "lvm-shared"
    ? ["raw"]
    : ["qcow2", "raw"];
}
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx

it("makes new volumes in LVM-thin pools raw and warns about mixed snapshot methods", async () => {
```

을 다음으로 바꾼다:

```tsx

it("warns that disks in shared LVM pools cannot be snapshotted on a schedule", async () => {
  const web = vm("web", "stopped", {
    spec: {
      ...vm("web", "stopped").spec,
      disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }],
    },
  });
  const root = volume("web-root", "san");
  root.spec.format = "raw";
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [root],
    "storage-pool": [pool("san", { backend: { kind: "lvm-shared", vg: "san" } })],
  });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  await user.selectOptions(await screen.findByLabelText("Take snapshots"), "daily");
  expect(
    screen.getByText("web-root is in a shared LVM pool, so scheduled snapshots will fail."),
  ).toBeVisible();
  // 스냅샷이 없으므로 메모리도 묻지 않는다.
  expect(screen.queryByLabelText("Include memory")).not.toBeInTheDocument();
  expect(screen.queryByText(/saved without memory/)).not.toBeInTheDocument();
});

it("makes new volumes in LVM-thin pools raw and warns about mixed snapshot methods", async () => {
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx

it("says why a VM with disks in ZFS pools cannot revert to older snapshots", async () => {
```

을 다음으로 바꾼다:

```tsx

it("says why a VM with disks in shared LVM pools cannot take snapshots", async () => {
  const shared = volume("web-root", "san");
  const scheduled = {
    ...web,
    spec: { ...web.spec, snapshot_schedule: { every: "daily" as const, keep: 7, memory: true } },
  };
  mockLists({
    vm: [scheduled],
    node: [NODE],
    volume: [{ ...shared, spec: { ...shared.spec, format: "raw" } }],
    "storage-pool": [pool("san", { backend: { kind: "lvm-shared", vg: "san" } })],
  });
  open(scheduled);
  // raw여도 공유 LVM 풀에는 스냅샷이 없다(서버가 409).
  expect(
    await screen.findByText("Disks in shared LVM pools cannot be snapshotted. web-root is in one."),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
  // 예약 요약은 메모리를 말하지 않는다.
  expect(screen.getByText("Snapshot schedule: Daily, keep 7")).toBeInTheDocument();
  expect(screen.queryByText(/with memory/)).not.toBeInTheDocument();
});

it("says why a VM with disks in ZFS pools cannot revert to older snapshots", async () => {
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
  const busy = isBusy(vm);
  // 풀 종류가 담지 못하는 형식의 디스크가 있거나 방식이 섞이면 만들 수 없다(snapshot_unsupported).
  // 볼륨·풀 목록을 다 읽어야 안다.
```

을 다음으로 바꾼다:

```tsx
  const busy = isBusy(vm);
  // 스냅샷이 없는 풀(공유 LVM)이나 풀 종류가 담지 못하는 형식의 디스크가 있거나 방식이 섞이면 만들 수
  // 없다(snapshot_unsupported).
  // 볼륨·풀 목록을 다 읽어야 안다.
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
        {operate &&
          (support?.kind === "format" ? (
            <p className="text-sm text-muted-foreground">
```

을 다음으로 바꾼다:

```tsx
        {operate &&
          (support?.kind === "pool" ? (
            <p className="text-sm text-muted-foreground">
              {t("snapshots.sharedLvm", { volume: support.disk.volume })}
            </p>
          ) : support?.kind === "format" ? (
            <p className="text-sm text-muted-foreground">
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
          <p>
            {/* 블록 디스크의 VM은 예약이 메모리를 원해도 메모리 없이 찍는다. */}
            {schedule
              ? t(schedule.memory && !block ? "snapshots.scheduleMemory" : "snapshots.schedule", {
                  every: t(`snapshots.every.${schedule.every}`),
                  keep: schedule.keep,
                })
              : t("snapshots.noSchedule")}
```

을 다음으로 바꾼다:

```tsx
          <p>
            {/* 블록 디스크의 VM은 예약이 메모리를 원해도 메모리 없이 찍는다. 공유 LVM 디스크의 VM은
                스냅샷이 없으므로 메모리를 말하지 않는다. */}
            {schedule
              ? t(
                  schedule.memory && !block && support?.kind !== "pool"
                    ? "snapshots.scheduleMemory"
                    : "snapshots.schedule",
                  {
                    every: t(`snapshots.every.${schedule.every}`),
                    keep: schedule.keep,
                  },
                )
              : t("snapshots.noSchedule")}
```

`web/src/vms/form/ScheduleSection.tsx`에서

```tsx
  const block = support?.kind === "ok" && support.block;
  return (
```

을 다음으로 바꾼다:

```tsx
  const block = support?.kind === "ok" && support.block;
  // 스냅샷이 없는 풀(공유 LVM)의 디스크면 메모리도 묻지 않는다.
  const noSnapshots = support?.kind === "pool";
  return (
```

`web/src/vms/form/ScheduleSection.tsx`에서

```tsx
        </Field>
        {on && support?.kind === "format" && (
```

을 다음으로 바꾼다:

```tsx
        </Field>
        {on && support?.kind === "pool" && (
          <p className="text-sm text-destructive sm:col-span-2">
            {t("vmForm.scheduleSharedLvm", { volume: support.disk.volume })}
          </p>
        )}
        {on && support?.kind === "format" && (
```

`web/src/vms/form/ScheduleSection.tsx`에서

```tsx
            </Field>
            {block ? (
              <p className="text-xs text-muted-foreground sm:col-span-2">
```

을 다음으로 바꾼다:

```tsx
            </Field>
            {noSnapshots ? null : block ? (
              <p className="text-xs text-muted-foreground sm:col-span-2">
```

`web/src/vms/snapshots.test.ts`에서

```ts
const ceph = pool("ceph", { backend: { kind: "rbd", pool: "rbd" } });
const pools = [local, fast, tank, ceph];
const raw = (name: string, poolName: string) => {
```

을 다음으로 바꾼다:

```ts
const ceph = pool("ceph", { backend: { kind: "rbd", pool: "rbd" } });
const san = pool("san", { backend: { kind: "lvm-shared", vg: "san" } });
const pools = [local, fast, tank, ceph, san];
const raw = (name: string, poolName: string) => {
```

`web/src/vms/snapshots.test.ts`에서

```ts
  expect(snapshotSupport([image])).toEqual({ kind: "format", disk: image });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
```

을 다음으로 바꾼다:

```ts
  expect(snapshotSupport([image])).toEqual({ kind: "format", disk: image });
  // 공유 LVM 풀의 디스크는 스냅샷이 없다. 서버처럼 디스크 차례대로 보고, 섞였는지보다 먼저다.
  const shared = disk(raw("s", "san"));
  expect(snapshotSupport([shared])).toEqual({ kind: "pool", disk: shared });
  expect(snapshotSupport([disk(raw("a", "fast")), shared, disk(volume("b", "local"))])).toEqual({
    kind: "pool",
    disk: shared,
  });
  expect(snapshotSupport([plain, shared])).toEqual({ kind: "format", disk: plain });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
```

`web/src/vms/snapshots.test.ts`에서

```ts
  expect(formSnapshotSupport(blank, [], pools)).toEqual({ kind: "ok", block: false });
});
```

을 다음으로 바꾼다:

```ts
  expect(formSnapshotSupport(blank, [], pools)).toEqual({ kind: "ok", block: false });
  // 공유 LVM 풀의 새 디스크(raw)는 스냅샷이 없다.
  const onSan = { ...values, disks: [{ ...emptyDisk("web", 0), volume: "web-0", pool: "san" }] };
  expect(formSnapshotSupport(onSan, [], pools)).toEqual({
    kind: "pool",
    disk: { volume: "web-0", format: "raw", pool: san },
  });
});
```

`web/src/vms/snapshots.ts`에서

```ts
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS·RBD, 메모리 없음)인지,
 * `format`은 풀 종류가 담지 못하는 형식의 디스크, `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드
 * 종류마다의 블록 스냅샷)이 섞인 것이다.
 */
export type SnapshotSupport =
  { kind: "ok"; block: boolean } | { kind: "format"; disk: SnapshotDisk } | { kind: "mixed" };
```

을 다음으로 바꾼다:

```ts
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS·RBD, 메모리 없음)인지,
 * `pool`은 스냅샷이 없는 풀(공유 LVM)의 디스크, `format`은 풀 종류가 담지 못하는 형식의 디스크,
 * `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드 종류마다의 블록 스냅샷)이 섞인 것이다.
 */
export type SnapshotSupport =
  | { kind: "ok"; block: boolean }
  | { kind: "pool"; disk: SnapshotDisk }
  | { kind: "format"; disk: SnapshotDisk }
  | { kind: "mixed" };
```

`web/src/vms/snapshots.ts`에서

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, 블록 풀(LVM-thin, ZFS, RBD)의 디스크는
 * raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
 * (서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
```

을 다음으로 바꾼다:

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 공유 LVM 풀의 디스크는 스냅샷이 없고, 파일 풀의 디스크는 qcow2,
 * 블록 풀(LVM-thin, ZFS, RBD)의 디스크는 raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와
 * 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로(서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을
 * 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
```

`web/src/vms/snapshots.ts`에서

```ts
  for (const disk of disks) {
    const kind = blockSnapshotKind(disk.pool) ?? null;
```

을 다음으로 바꾼다:

```ts
  for (const disk of disks) {
    if (disk.pool?.spec.backend.kind === "lvm-shared") {
      return { kind: "pool", disk };
    }
    const kind = blockSnapshotKind(disk.pool) ?? null;
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 297개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 공유 LVM 풀 만들기와 공유 LVM 디스크 VM의 스냅샷 막기"
```

---

### Task 8: 문서

README에 "공유 LVM(iSCSI) 풀"(관리자 준비: LUN과 이니시에이터 ACL, lvm2-lockd·sanlock·softdog, `use_lvmlockd`, 노드마다 다른 `host_id`, `vgcreate --shared`; 규칙, 노드별 상태, 0 채우기, 활성화 수명과 정리, 크기 늘리기와 VM 시작, `lvm-shared-it`과 CI 준비), 설계 문서 "구현:" 메모, NFS 설계 §1의 계획 19, 웹 풀 지우기 안내.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md`
- Modify: `docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD 풀)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀)
```

`README.md`에서

```markdown
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에, 볼륨이 Ceph RBD 이미지인
**Ceph RBD 풀**(`--rbd`)은 [그 뒤](#ceph-rbd-풀)에 있다.
```

을 다음으로 바꾼다:

```markdown
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에, 볼륨이 Ceph RBD 이미지인
**Ceph RBD 풀**(`--rbd`)은 [그 뒤](#ceph-rbd-풀)에, 볼륨이 공유 LUN 위의 LV인 **공유 LVM 풀**(`--lvm-shared`)은
[마지막](#공유-lvmiscsi-풀)에 있다.
```

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## 공유 LVM(iSCSI) 풀

**공유 LVM 풀**(`--lvm-shared VG`)은 모든 노드가 보는 LUN(iSCSI 타깃이나 FC·멀티패스 SAN) 위의 **공유 VG**다.
관리자가 큰 LUN 하나에 공유 VG를 한 번 만들면 opLite가 그 안에 볼륨마다 두꺼운 LV `opl-{볼륨 id}`를 만들고, VM에는
[LVM-thin 풀](#lvm-thin-풀)처럼 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다. 여러 노드가 같은
VG를 바꾸므로 노드 사이 잠금은 LVM의 **lvmlockd**(sanlock 또는 dlm)가 맡고, LV는 쓰는 노드 하나에서만 켠다(배타
활성화). `--iscsi-portal`·`--iscsi-target`을 주면 opld가 노드마다 그 타깃에 로그인한다. 모든 노드가 같은 LV를 보는
**공유 풀**이고 **스냅샷이 없다**.

관리자가 미리 할 일(opLite는 타깃·LUN·VG를 만들지 않고 잠금 데몬을 설치하지 않는다):

```bash
# 1. 스토리지: LUN 하나를 모든 노드에 내보낸다. iSCSI면 타깃의 이니시에이터 ACL로 노드만 들인다(CHAP은 지원하지 않는다).
# 2. 노드마다: 잠금 데몬과 iSCSI 이니시에이터. sanlock은 watchdog이 필요하다(하드웨어가 없으면 softdog).
sudo apt install lvm2-lockd sanlock open-iscsi
sudo modprobe softdog                                # 하드웨어 watchdog이 없을 때만(부팅 때 올라오게 둔다)
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf   # global 절의 use_lvmlockd = 1
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf   # host_id는 노드마다 달라야 한다(1–2000)
sudo systemctl enable --now wdmd sanlock lvmlockd
# 3. 한 노드에서 한 번: LUN에 로그인해 공유 VG를 만든다(잠금 시작까지 수십 초 걸릴 수 있다).
sudo iscsiadm -m discovery -t sendtargets -p san.lan
sudo iscsiadm -m node -T iqn.2003-01.org.example:storage -p san.lan --login
sudo vgcreate --shared san /dev/disk/by-path/ip-10.0.0.9:3260-iscsi-iqn.2003-01.org.example:storage-lun-0
# 그다음은 opLite가 한다: 노드마다 로그인하고 잠금을 시작한다(vgchange --lockstart).
oplctl pool create san --lvm-shared san --iscsi-portal san.lan:3260 --iscsi-target iqn.2003-01.org.example:storage
oplctl pool create fc --lvm-shared fcvg              # 노드가 이미 LUN을 보면(FC 등) iSCSI 옵션 없이
oplctl volume create db --pool san --size 20G        # --format을 생략하면 raw
oplctl pool list                                     # LOCATION: san @ san.lan:3260 iqn.2003-01.org.example:storage
oplctl pool get san                                  # 노드별 준비·VG 크기·남은 공간·오류
```

- **공유, raw만, 스냅샷 없음.** VM은 풀이 준비된 어느 노드에서나 돈다. `--shared`는 필요 없고 함께 쓸 수 없으며
  API의 `"shared": false`는 **400** `invalid_request`("lvm-shared pools are always shared")다. 볼륨은 raw이고 qcow2
  볼륨을 만들거나 가져오면 **400**("lvm-shared pools hold raw volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면
  raw로 정한다. 공유 LVM 디스크가 있는 VM의 스냅샷은 **409** `snapshot_unsupported`("disks in storage pool … cannot
  be snapshotted")이고 그런 VM의 예약 스냅샷도 실패한다(웹은 스냅샷 버튼을 이유와 함께 막고 예약을 켜면 알린다).
  그래서 복제는 멈춘 VM의 지금 상태로만 한다.
- **이름과 접속.** VG 이름은 [LVM-thin 풀](#lvm-thin-풀)과 같은 규칙이다(틀리면 **400** "invalid lvm name …"). 포털은
  `호스트[:포트]`이고 IPv6 주소는 대괄호로 감싼다(`[fd00::5]:3260`, 포트 1–65535). 타깃은 `iqn.`·`eui.`·`naa.`로
  시작하고 소문자·숫자·`.`·`:`·`-`로 된 223자 이하다. 틀리면 **400**("invalid iscsi portal …", "invalid iscsi
  target …"). `--iscsi-portal`과 `--iscsi-target`은 함께 쓴다.
- **업그레이드 먼저.** 공유 LVM 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld(root)는 15초마다 풀을 준비한다: iSCSI 타깃이 있으면 그 타깃의 세션이 없을 때 `iscsiadm`으로
  찾아 로그인하고(`-m discovery -t sendtargets`, `-m node --login`, 그 뒤 `udevadm settle`. 세션은 타깃 이름으로 찾는다),
  `vgchange --lockstart`로 VG 잠금을 시작한 뒤(이미 시작했으면 곧바로 끝난다) `vgs`(10초 제한)로 VG를 본다. 잠금
  종류가 `sanlock`·`dlm`이면 준비되고 VG의 크기·남은 공간을 알린다. 잠금이 없는 VG는 `"volume group san is not a
  shared (lvmlockd) VG"`, 그 밖의 실패는 명령의 오류 첫 줄이다(`vgs`가 시간을 넘기면 `"vgs timed out"`). 로그인과
  처음 잠금 시작은 sanlock 임대 때문에 오래 걸릴 수 있으므로 죽이지 않는다: 10초 안에 끝나지 않으면 `"iscsi login or
  vg lock start in progress"`로 알리고 다음 확인에서 결과를 거둔다. 풀을 지워도 로그아웃하거나 잠금을 멈추지 않는다.
- **macOS 미지원.** macOS 노드는 "lvm-shared pools are not supported on macOS"로 알리고, 그 노드에 놓인 공유 LVM
  디스크의 VM은 이유와 함께 꺼진다.
- **볼륨 만들기.** LV는 켜지 않고 만든 뒤(`lvcreate -an -Zn -Wn`, 크기는 1 MiB 배수로 올린다) 이 노드에서 배타
  활성화해 `blkdiscard -z`로 모두 0으로 채우고 끈다. 0 채우기는 크기에 비례해 걸리므로 시간 제한이 없다(SAN이 WRITE
  SAME·UNMAP 오프로드를 하면 빠르다). 켜거나 채우지 못하면 LV를 지운다. 두꺼운 LV는 만들 때 자리를 잡으므로 VG가
  모자라면 만들기가 실패하고 게스트의 쓰기가 나중에 실패하지는 않는다. 가져오기·복사의 대상도 이렇게 만든 뒤
  `qemu-img convert -n --target-is-zero`로 써 넣고 끈다.
- **활성화의 수명.** LV는 쓰는 동안만 그 노드에서 켜져 있다. VM을 시작할 때 opld가 디스크 LV를 배타 활성화하고
  (`lvchange -aey`), QEMU가 끝나면(멈춤·충돌·포기) 그리고 시작이 실패하면 끈다(`-an`. 끄지 못하면 경고만 남긴다).
  켜진 채 남은 LV(opld가 그 전에 내려갔을 때 등)는 opld가 시작할 때와 그 뒤 30분마다 끈다. 이때 이 노드에서
  실행 중이거나 시작하려는 VM의 디스크, 이 노드에서 끝나지 않은 작업의 볼륨, 만드는 중인 볼륨은 그대로 둔다.
- **다른 노드가 쓰는 중.** 다른 노드가 켜 둔 LV를 켜거나 지우거나 늘리려 하면 lvmlockd가 거절하고, opLite는 잠시
  뒤 다시 해 볼 오류로 끝낸다("timed out waiting for another node to stop using the lv", VG 잠금이면 "… to release
  the shared vg lock"). VM은 꺼지지 않고 `starting`에서 다시 시도한다. **노드가 죽으면** 그 노드가 켜 둔 LV는 sanlock
  임대가 끝날 때까지(기본 수 분) 다른 노드에서 쓸 수 없다. 한 노드에서 **끄기가 실패하면**(경고만 남긴다) 그
  노드의 정리(opld 시작 때와 30분마다)가 끌 때까지 다른 노드에서 그 볼륨을 켜거나 늘리거나 지울 수 없다(같은 일시
  오류).
- **볼륨 작업 노드.** LVM 명령은 LVM-thin처럼 하나에 60초를 넘으면 실패로 보되 LV를 바꾸는 명령은 죽이지 않고, 그
  표시가 노드마다 따로이므로 공유 LVM 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) 작업은 [Ceph RBD
  풀](#ceph-rbd-풀)처럼 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다. 가져오기는 올린 파일이 있는 노드,
  실행 중인 VM의 디스크 크기 늘리기는 VM의 노드에서 한다.
- **지우기·복제·크기.** 볼륨을 지우면 LV를 지운다(`lvremove`). 시간을 넘긴 만들기가 아직 돌고 있어 LV가 없으면 지운
  것으로 보지 않고 잠시 뒤 다시 해 본다. 풀을 지워도 opLite 리소스만 지우고 VG와 그 안의 다른 LV는 그대로 둔다.
  복제·복사는 원본 LV를 켜서 읽고 끝나면 끈다. 대상 풀이 블록 풀이면 raw, 디렉터리·NFS 풀이면 qcow2로 만든다.
  크기 늘리기는 실행 중이어도 된다. `lvextend` 뒤 늘린 부분을 `blkdiscard -z`로 0으로 채우고(새 익스텐트에 지운
  볼륨의 옛 데이터가 남아 있을 수 있다. 시간 제한 없음) 실행 중이면 QEMU에 새 크기를 알린다. 꺼진 볼륨은 채우는
  동안만 배타 활성화한다. 늘리기 전 크기를 LV 태그(`opl-zero-from-…`)로 적어 두고 다 채우면 뗀다. 0 채우기가
  실패하면 작업이 실패하므로 **크기 늘리기가 실패하면 다시 한다**: LV가 이미 그 크기여도 남은 부분을 다시 채운다.
  다시 하기 전에 VM을 시작하거나 그 볼륨을 복제·복사하면 LV를 켤 때 남은 부분을 먼저 마저 채우고 태그를 뗀다(옛
  데이터를 내주지 않는다). 이 0 채우기도 남은 크기에 비례하므로 **0 채우기가 실패한 볼륨의 VM은 다음 시작이 오래
  걸릴 수 있다**. 크기 늘리기 작업이 끝나지 않은 디스크의 VM은 켜지 않고 `starting`에서 기다리고, VM이 켜지거나
  꺼지는 중이면 그 디스크의 크기 늘리기는 409 `vm_busy`다(모든 풀이 같다). 크기 늘리기 작업을 하던 **노드가
  죽으면** 그 작업은 그 노드가 돌아오거나 클러스터에서 빠질 때까지 끝나지 않으므로, 그동안 그 디스크의 VM은
  시작하지 않고 `starting`에서 기다린다(오프라인 노드의 아직 시작하지 않은 작업은 약 1분 뒤 실패로 정리된다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** [LVM-thin 풀](#lvm-thin-풀)과 같다(lvm2 기본값 `scan_lvs = 0`이면
  그대로 둔다).

## 웹 UI
````

`README.md`에서

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd
```
````

을 다음으로 바꾼다:

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd
# Linux, root: 실제 공유 LVM 풀. 테스트가 LV를 만들고 지워도 되는, 잠금을 시작한 공유(lvmlockd) VG와 lvm2-lockd, sanlock,
# qemu-io가 필요하다. OPL_TEST_ISCSI(포털,타깃)가 있으면 opld 시험은 VG가 그 타깃의 LUN 위에 있다고 본다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  cargo test -p opl-driver --features lvm-shared-it --test lvm_shared_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM_SHARED=oplshared \
  OPL_TEST_ISCSI=127.0.0.1,iqn.2003-01.org.linux-iscsi.opl:shared cargo test -p opld --features lvm-shared-it --test lvm_shared
```
````

`README.md`에서

```markdown

웹 UI (`web/`):
```

을 다음으로 바꾼다:

````markdown

`lvm-shared-it` 시험은 `OPL_TEST_LVM_SHARED`의 공유 VG에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기(0
채우기)·배타 활성화와 끄기·연 장치는 끌 수 없음·크기(늘린 부분의 0 채우기)·가져오기와 실패한 가져오기 정리·백엔드 안팎의 복사·지우기를,
opld 시험은 opld가 iSCSI 로그인과 잠금 시작부터 해서 풀을 준비하는지(`OPL_TEST_ISCSI`가 있으면 시험이 먼저 잠금을
멈추고 로그아웃한다), 공유 LVM 디스크로 VM을 켜면 LV가 켜지고 멈추면 꺼지는지, 스냅샷의 409, opld가 다시 뜰 때 남은
활성화 정리(실행 중인 VM의 LV는 그대로), 디렉터리 풀과 같은 풀로 복제, 지우기를 본다. **버려도 되는 VG**를 쓴다. CI
`linux-kvm` 작업은 targetcli(LIO)로 루프백 iSCSI 타깃을 만들어 로그인하고 그 LUN 위에 공유 VG `oplshared`를 둔다
(lvmlockd는 호스트 전체에 켜지지만 로컬 VG인 LVM-thin 시험의 `opltest`는 그대로 쓴다):

```bash
sudo apt install targetcli-fb open-iscsi lvm2-lockd sanlock
IQN=iqn.2003-01.org.linux-iscsi.opl:shared
sudo modprobe target_core_mod && sudo modprobe iscsi_target_mod && sudo systemctl start iscsid
truncate -s 2G /tmp/oplshared.img
sudo targetcli /backstores/fileio create oplshared /tmp/oplshared.img
sudo targetcli /iscsi create "$IQN"                  # 포털 0.0.0.0:3260이 함께 생긴다
sudo targetcli "/iscsi/$IQN/tpg1/luns" create /backstores/fileio/oplshared
sudo targetcli "/iscsi/$IQN/tpg1" set attribute authentication=0 demo_mode_write_protect=0 generate_node_acls=1 cache_dynamic_acls=1
sudo iscsiadm -m discovery -t sendtargets -p 127.0.0.1 && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --login
sudo udevadm settle
sudo modprobe softdog
sudo sed -i 's/^\s*#\?\s*use_lvmlockd = .*/\tuse_lvmlockd = 1/' /etc/lvm/lvm.conf
printf 'local {\n\thost_id = 1\n}\n' | sudo tee /etc/lvm/lvmlocal.conf
sudo systemctl start wdmd sanlock lvmlockd
sudo vgcreate --shared oplshared "/dev/disk/by-path/ip-127.0.0.1:3260-iscsi-$IQN-lun-0"   # 잠금 시작까지 20–30초
# 끝나면: sudo vgremove -y oplshared && sudo iscsiadm -m node -T "$IQN" -p 127.0.0.1 --logout
#         && sudo targetcli /iscsi delete "$IQN" && sudo targetcli /backstores/fileio delete oplshared
#         (lvm.conf의 use_lvmlockd를 0으로 되돌린다)
```

웹 UI (`web/`):
````

`README.md`에서

```markdown
- [공유 스토리지 설계 4: Ceph RBD (계획 18)](docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [공유 스토리지 설계 4: Ceph RBD (계획 18)](docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md)
- [공유 스토리지 설계 5: iSCSI와 공유 LVM (계획 19)](docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md`에서

```markdown
| 18 | **Ceph RBD** | QEMU 네트워크 블록 드라이버(`rbd`), 네트워크 디스크 소스(`RuntimeDisk.rbd`) |
| 19 | **iSCSI** | 18의 네트워크 디스크 소스(`iscsi`) 또는 공유 LUN 위의 블록 바탕 |

구현: 처음에는 계획 18이 iSCSI와 Ceph RBD를 함께 했으나, 성격이 달라(RBD는 이미지·스냅샷을 API로 다루고 iSCSI는 관리자가 만든 LUN을 쓴다) 계획 18은 Ceph RBD만 하고 iSCSI는 계획 19로 미뤘다(`2026-10-29-shared-storage-ceph-rbd-design.md` §1).
```

을 다음으로 바꾼다:

```markdown
| 18 | **Ceph RBD** | QEMU 네트워크 블록 드라이버(`rbd`), 네트워크 디스크 소스(`RuntimeDisk.rbd`) |
| 19 | **iSCSI**(공유 LVM) | 공유 LUN 위의 공유(lvmlockd) VG, 16의 블록 디스크 소스, LV 배타 활성화 |

구현: 처음에는 계획 18이 iSCSI와 Ceph RBD를 함께 했으나, 성격이 달라(RBD는 이미지·스냅샷을 API로 다루고 iSCSI는 관리자가 만든 LUN을 쓴다) 계획 18은 Ceph RBD만 하고 iSCSI는 계획 19로 미뤘다(`2026-10-29-shared-storage-ceph-rbd-design.md` §1).

구현: 계획 19는 두 길 가운데 LUN을 그대로 디스크로 쓰는 QEMU `iscsi` 드라이버가 아니라 공유 LUN 위의 공유 LVM(lvmlockd·sanlock, 볼륨마다 두꺼운 LV)을 택했다. LUN을 opLite가 만들거나 늘릴 수 없어 볼륨 모델과 맞지 않기 때문이다. 같은 풀을 iSCSI 로그인 없이 FC·멀티패스 SAN에도 쓴다(`2026-10-30-shared-storage-iscsi-lvm-design.md` §1).
```

`docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`에서

```markdown
- `PoolBackend::LvmShared { vg: String, iscsi: Option<IscsiTarget> }`, `IscsiTarget { portal: String, target: String }`(`deny_unknown_fields`).
  - `vg`: LVM-thin과 같은 VG 이름 규칙.
  - `portal`: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS `server`와 같은 규칙, 포트는 1–65535.
  - `target`: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`만, 223바이트 이하.
- 공유 LVM 풀은 항상 공유(`is_shared` 참, `shared: false`는 400), raw만(qcow2는 400), `pool_kind_supports_snapshots`는 거짓.
- 블록 스냅샷 종류는 없다. 공유 LVM 디스크가 있는 VM의 스냅샷은 409 `snapshot_unsupported`(스냅샷을 지원하지 않는 디스크의 기존 규칙). 예약 스냅샷도 그 기존 규칙을 따른다.

## 3. 드라이버 (`storage/lvm.rs`의 공유 LVM)

계획 16의 `LvmOps`·`BlockTool`을 그대로 쓰고 두꺼운 LV 명령을 더한다(같은 LV에는 명령 하나, 바꾸는 명령은 죽이지 않는다).

- 만들기: `lvcreate -q -an -L {bytes}b -n opl-{id} {vg}`(1 MiB 배수로 올림, 만들 때 켜지 않는다) → 배타 활성화 → `blkdiscard -z`로 0 채우기(SAN이 WRITE SAME·UNMAP으로 빨리 할 수 있다, 시간 제한 없음) → 끈다. 실패하면 지운다. 0으로 채웠으므로 `target_reads_zero`는 참.
- 가져오기·복사의 대상: 만들기(0 채움) → 배타 활성화 → `qemu-img convert -n --target-is-zero` → 끈다.
- 지우기: `lvremove`. 다른 노드가 켜 두었으면 lvmlockd가 거절한다(`LV locked by other host`) → 일시 오류.
- 크기: `lvextend -L`. 실행 중 VM의 디스크면 VM 노드(이미 켜 둔 노드)에서 돈다(기존 규칙). 꺼진 볼륨은 켜지 않고 늘린다.
- 원본(`volume_source`): 배타 활성화하고 장치 경로, `release_source`가 끈다. 태그는 `InvalidSpec`.
- `activate_volume`: 배타 활성화(`lvchange -aey`). 다른 노드가 쥐고 있으면 일시 오류(메시지에 "다른 노드가 쓰는 중").
- 새 메서드 `Storage::deactivate_volume`(기본 할 일 없음): 공유 LVM은 `lvchange -an`(이미 꺼져 있으면 성공).
- 정보: `lvs`(크기, 사용량은 크기와 같다). 용량: `vgs`의 크기·남은 공간.
```

을 다음으로 바꾼다:

```markdown
- `PoolBackend::LvmShared { vg: String, iscsi: Option<IscsiTarget> }`, `IscsiTarget { portal: String, target: String }`(`deny_unknown_fields`).
  - `vg`: LVM-thin과 같은 VG 이름 규칙. 구현: 틀리면 LVM-thin과 같은 `PoolError::BadLvmName`(400 `invalid_request`)이고 모르는 필드는 거절한다(`iscsi` 안의 `chap` 등도).
  - `portal`: `호스트[:포트]` 또는 `[IPv6][:포트]`. 호스트는 NFS `server`와 같은 규칙, 포트는 1–65535. 구현: `PoolError::BadIscsiPortal(portal)`(400). IPv6 주소는 대괄호가 있어야 한다(`fd00::5`처럼 대괄호 없는 주소는 포트와 가릴 수 없어 거절). 대괄호 안은 `Ipv6Addr`로 읽고 포트는 숫자만이다(`+80` 거절). 웹 `iscsiPortalError`(`pools.errors.portal`)도 같되 대괄호 안은 대강만 본다.
  - `target`: `iqn.`·`eui.`·`naa.`로 시작하고 소문자·숫자·`.`·`:`·`-`만, 223바이트 이하. 구현: `PoolError::BadIscsiTarget(target)`(400). 접두사 뒤에 한 글자 이상 있어야 한다. 웹 `iscsiTargetError`(`pools.errors.target`), oplctl은 서버의 400에 맡긴다.
- 공유 LVM 풀은 항상 공유(`is_shared` 참, `shared: false`는 400), raw만(qcow2는 400), `pool_kind_supports_snapshots`는 거짓. 구현: 문구는 "lvm-shared pools are always shared"(`create_pool`이 NFS·RBD와 함께 본다), "lvm-shared pools hold raw volumes"(`check_pool_format`, 드라이버도 같은 문구로 거절). 블록 풀인지는 `opl-api` `volumes::block_pool_kind(&StoragePool) -> Option<&'static str>`에 모았다(형식 검사, 만들기·가져오기의 형식 기본값 raw, 복제의 `copy_format`, 실행 중 크기 늘리기). `uses_block_snapshots`는 블록 스냅샷 종류만 본다. `pool_usable_on`은 NFS·RBD처럼 준비됐다는 상태가 있어야 참이다. `pool_root`는 `/dev/{vg}`이고 파일 저장소는 공유 LVM 풀을 거절한다("lvm-shared pools hold no volume files").
- 블록 스냅샷 종류는 없다. 공유 LVM 디스크가 있는 VM의 스냅샷은 409 `snapshot_unsupported`(스냅샷을 지원하지 않는 디스크의 기존 규칙). 예약 스냅샷도 그 기존 규칙을 따른다. 구현: `block_snapshot_kind`는 `None`이다. 기존 문구("only qcow2 disks in storage pool … can be snapshotted")는 맞지 않으므로 공유 LVM 디스크는 "VM … has disk … that cannot be snapshotted; disks in storage pool … cannot be snapshotted"다. 웹은 `SnapshotSupport`에 `{ kind: "pool"; disk }`를 더해 스냅샷 버튼을 이유(`snapshots.sharedLvm`)와 함께 막고, 예약을 켜면 `vmForm.scheduleSharedLvm`으로 알린다. 스냅샷이 없으므로 실행 중인 VM은 복제할 수 없다(멈춘 VM의 지금 상태로만).

## 3. 드라이버 (`storage/lvm.rs`의 공유 LVM)

계획 16의 `LvmOps`·`BlockTool`을 그대로 쓰고 두꺼운 LV 명령을 더한다(같은 LV에는 명령 하나, 바꾸는 명령은 죽이지 않는다). 구현: `LvmSharedStorage::new(qemu_img, Arc<dyn LvmOps>)`, 연결은 `Storages::with_lvm_shared`(opld `member.rs`, LVM-thin과 같은 `SystemLvm`). `LvmOps`에 `create_linear`·`activate_exclusive`·`zero`·`vg_info`를 더했다. `VgInfo { name, lock_type, size_bytes, free_bytes }`와 해석기 `parse_vgs`(`vgs --reportformat json --units b --nosuffix -o vg_name,vg_lock_type,vg_size,vg_free`, 마운터도 쓴다)는 공개다.

- 만들기: `lvcreate -q -an -L {bytes}b -n opl-{id} {vg}`(1 MiB 배수로 올림, 만들 때 켜지 않는다) → 배타 활성화 → `blkdiscard -z`로 0 채우기(SAN이 WRITE SAME·UNMAP으로 빨리 할 수 있다, 시간 제한 없음) → 끈다. 실패하면 지운다. 0으로 채웠으므로 `target_reads_zero`는 참. 구현: 켜지 않고 만들므로 `lvcreate`에 `-Zn -Wn`을 준다(처음 부분 0 채우기·서명 지우기는 켜야 할 수 있다 — 0 채우기는 뒤의 `blkdiscard`가 한다). `blkdiscard -f -z`(`-f`: 옛 데이터의 서명이 있어도 묻지 않는다). 이미 있으면 `VolumeExists`. 만든 뒤 끄지 못하면 경고만 남긴다(§4의 정리가 끈다). `create_volume`은 장치 경로를 돌려주지만 그 노드는 켜야 생긴다.
- 가져오기·복사의 대상: 만들기(0 채움) → 배타 활성화 → `qemu-img convert -n --target-is-zero` → 끈다. 구현: 가져오기는 원본의 가상 크기로 만들고 `-O raw`로 쓴다. 쓰다 실패하면 LV를 지운다. 같은 백엔드 복사는 `LvmSharedStorage::copy_volume`(원본 LV를 켜서 읽고, 취소돼도 끈다). 다른 백엔드에서 오는 복사는 `Storages`의 백엔드 사이 복사가 대상이 공유 LVM이면 만든 볼륨을 쓰는 동안만 켜고(`activate_volume`/`deactivate_volume`) `target_reads_zero`가 참이라 `--target-is-zero`로 쓴다.
- 지우기: `lvremove`. 다른 노드가 켜 두었으면 lvmlockd가 거절한다(`LV locked by other host`) → 일시 오류. 구현: lvmlockd 오류에 "locked by other host"가 있으면 `DriverError::Timeout("another node to stop using the lv")`, "held by other host"(VG 잠금)면 `Timeout("another node to release the shared vg lock")`이다(만들기·켜기·지우기·늘리기 모두). lvmlockd가 그 앞에 경고 줄("Retrying ex lock …")을 낼 수 있어, LVM 명령의 오류에는 stderr 첫 줄 대신 이 말이 든 줄을 담는다(`BlockTool::preferring`). 이 노드에서 켜진 LV는 `lvremove -y`가 그대로 지우므로(CI에서 확인) 먼저 끄지 않는다. LV가 없는데 그 LV를 바꾸는 명령이 아직 돌면(시간을 넘긴 만들기) `VolumeNotFound`가 아니라 일시 오류다.
- 크기: `lvextend -L` 뒤 늘린 부분(늘리기 전 크기부터 끝까지)을 `blkdiscard -f -z -o {늘리기 전 크기}`로 0으로 채운다(새 익스텐트에 지운 볼륨의 옛 데이터가 남아 있을 수 있다. 바꾸는 명령, 시간 제한 없음). 실행 중 VM의 디스크면 VM 노드(이미 켜 둔 노드)에서 돌고 켠 채로 채운다(기존 규칙). 꺼진 볼륨은 채우는 동안만 배타 활성화한다. 구현: 늘리기 전에 늘리기 전 크기를 LV 태그 `opl-zero-from-{바이트}`로 붙이고(`lvchange --addtag`) 다 채운 뒤 뗀다(`--deltag`). 0 채우기가 실패하면 오류(Task 실패)이고 태그가 남아, 다시 하면 LV가 이미 그 크기여도 태그의 위치부터 다시 채운다. 태그가 없고 이미 그 크기 이상이면 건너뛴다. 남은 태그는 LV를 켜서 장치를 내주는 쪽도 본다: VM 시작(`activate_volume`)과 복제·복사 원본(`volume_source`)이 켠 뒤 장치를 내주기 전에 남은 부분을 0으로 채우고 태그를 뗀다(공용 도우미 `activate_ready`, 실패하면 오류이고 자기가 켠 LV만 끈다). 이 0 채우기도 남은 크기에 비례하므로 그런 VM의 시작은 오래 걸릴 수 있다. 그렇지 않으면 게스트가 늘린 크기 전체를 보고 옛 데이터를 읽고, 나중의 크기 늘리기가 게스트가 쓴 데이터를 지운다. 태그의 위치가 LV 끝이면(태그만 붙고 늘리기 전) 채울 것이 없으므로 태그를 둔다. 크기 늘리기와 VM 시작은 겹치지 않는다(백엔드 공통 — qcow2의 `qemu-img resize`와 QEMU 시작도): reconciler의 resolve는 디스크 볼륨에 끝나지 않은 크기 늘리기 Task가 있으면 켜지 않고 `ResolveError::VolumeResizing`("volume … is being resized")으로 실패로 세지 않고 `starting`에서 다시 시도한다. API 크기 늘리기는 쓰는 VM이 켜지거나 꺼지는 중이면(원하는 상태와 실제 상태가 다르거나 전원 Task가 끝나지 않았다) 409 `vm_busy`(`check_not_busy`)이고, Task 실행기는 원하는 상태도 꺼짐이고 멈춘 VM일 때만 꺼진 VM 경로(저장소로 늘리기)를 탄다(켜는 중이면 Task 실패). 그래서 Task가 꺼진 VM을 본 뒤 켜면 reconciler가 그 Task를 보고 기다리고, reconciler가 Task 없이 켜기 시작했으면 원하는 상태가 이미 켜짐이라 API·실행기가 거절한다. 원하는 상태가 그대로 켜짐인데 QEMU가 스스로 끝나 곧바로(백오프 없이) 다시 켤 때는 저장된 phase가 `running`인 채라 API·실행기가 실행 중 VM으로 볼 수 있으므로, worker가 디스크를 해석하기 전에 phase `starting`을 먼저 기록한다(그 전에 커밋된 Task는 resolve가 보고, 뒤의 Task는 API·실행기가 거절한다). 기록하지 못하면(예: 과반수 없음 — 그동안은 Task도 만들 수 없다) 그대로 다시 켠다(VM은 과반수 없이도 다시 켠다). 기록이 시간 초과(`write_timeout`, 5초)로 실패한 사이 과반수가 돌아와 크기 늘리기 Task가 들어오면 좁은 틈이 남는다. 과반수가 없으면 백오프 없는 다시 켜기가 그 시간만큼 늦어진다. 이미 도는 VM에 다시 붙을 때(`reattach`)는 QEMU가 이미 장치를 열었으므로 크기 늘리기 Task를 기다리지 않는다(`resolve::running_spec`). 드라이버도 같은 노드 안에서 노드 안 표시(`Zeroing`)로 가른다: 크기 늘리기와 켜기(VM 시작·복제 원본)는 표시를 잡은 뒤 LV를 읽고(읽은 활성 여부가 끝까지 맞다), 표시를 잡지 못하거나 그 LV를 바꾸는 LVM 명령이 도는 동안이면 LV를 건드리지 않고 일시 오류다. 다른 노드와는 lvmlockd의 배타 잠금이 가른다. `LvmOps::zero`가 시작 위치를 받고 `tags`(`lvs --noheadings -o lv_tags`)·`set_tag`가 더해졌다. 켜진 LV·꺼진 LV 모두 `lvextend`가 된다(CI에서 확인).
- 원본(`volume_source`): 배타 활성화하고 장치 경로, `release_source`가 끈다. 태그는 `InvalidSpec`.
- `activate_volume`: 배타 활성화(`lvchange -aey`). 다른 노드가 쥐고 있으면 일시 오류(메시지에 "다른 노드가 쓰는 중"). 구현: 이 노드에서 이미 켜져 있으면 다시 켜지 않는다. 메시지는 위의 "timed out waiting for another node to stop using the lv". `disk_location`은 LVM-thin과 같은 `/dev/{vg}/opl-{id}`(`DiskKind::Block`)이고 장치 노드가 없으면 `VolumeNotActive`(일시)다.
- 새 메서드 `Storage::deactivate_volume`(기본 할 일 없음): 공유 LVM은 `lvchange -an`(이미 꺼져 있으면 성공). 구현: 볼륨 LV가 없어도 성공이다. 이 노드가 0으로 채우는 중인 LV(`Zeroing` 표시, 크기 늘리기)는 끄지 않고 성공이다(켜진 채 남으면 §4의 정리가 끈다). 끄는 동안 표시를 쥐므로 그사이 크기 늘리기는 일시 오류로 기다린다. `Storages`가 백엔드로 전달한다. 정리용으로 `Storage::active_volumes(pool) -> Vec<Uuid>`(기본 빈 목록, 공유 LVM은 이 노드에서 켜진 `opl-{uuid}` LV)도 더했다.
- 정보: `lvs`(크기, 사용량은 크기와 같다). 용량: `vgs`의 크기·남은 공간.
```

`docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`에서

```markdown

- **VM 시작:** resolve가 `activate_volume`(배타)을 부른다. 시작이 실패하면 켠 LV를 끈다.
- **VM 멈춤:** reconciler가 QEMU 프로세스가 끝난 것을 본 뒤 그 VM의 공유 LVM 디스크를 끈다(`deactivate_volume`). 끄지 못하면 경고를 남기고 아래 정리가 다시 한다.
- **정리:** opld 시작 때와 30분마다(남은 태그 정리와 같은 주기) 이 노드에서 켜져 있는 공유 LVM의 `opl-*` LV 가운데, 이 노드에서 실행 중인 VM의 디스크도 이 노드에서 도는 Task의 볼륨도 아닌 것을 끈다.
- **노드 장애:** 죽은 노드의 잠금은 sanlock 임대가 끝나면(기본 수 분) 풀린다. 그 전에는 다른 노드에서 그 디스크로 VM을 켤 수 없다(일시 오류).
```

을 다음으로 바꾼다:

```markdown

- **VM 시작:** resolve가 `activate_volume`(배타)을 부른다. 시작이 실패하면 켠 LV를 끈다. 구현: resolve(`runtime_spec`)가 켠 볼륨을 적어 두었다가 디스크가 여럿인 VM의 해석이 중간에 실패하면 이미 켠 것을 끈다. QEMU 시작이 실패하면 reconciler worker가 디스크를 끈다.
- **VM 멈춤:** reconciler가 QEMU 프로세스가 끝난 것을 본 뒤 그 VM의 공유 LVM 디스크를 끈다(`deactivate_volume`). 끄지 못하면 경고를 남기고 아래 정리가 다시 한다. 구현: worker가 디스크를 켰거나(시작) 도는 VM을 봤으면 표시(`disks_active`)를 두고, QEMU가 끝난 것(멈춤·충돌·포기)을 확인한 뒤 한 번 VM의 모든 디스크에 일반 `deactivate_volume`을 부른다(공유 LVM만 할 일이 있다).
- **정리:** opld 시작 때와 30분마다(남은 태그 정리와 같은 주기) 이 노드에서 켜져 있는 공유 LVM의 `opl-*` LV 가운데, 이 노드에서 실행 중인 VM의 디스크도 이 노드에서 도는 Task의 볼륨도 아닌 것을 끈다. 구현: opl-api Task 실행기의 `deactivate_idle_volumes`(시작할 때와 `TaskRunnerConfig::tag_sweep`마다, 실행기가 멈추면 함께 멈춘다)가 이 노드의 목록에 있는 풀마다 `active_volumes`를 읽는다. 하이퍼바이저에서 도는 VM과 이 노드에 놓여 `power: running`인 VM(시작하려는 VM)의 디스크, 이 노드의 끝나지 않은 Task의 대상(VM Task면 그 VM의 디스크), `Ready`가 아닌(만드는 중인) 볼륨은 건너뛰고, 리소스가 없는 볼륨 LV는 끈다. 켜진 LV를 먼저 읽고 VM·Task를 나중에 읽는다(LV를 켜는 VM·Task는 켜기 전에 이미 저장소에 있으므로 쓰는 LV를 끄지 않는다). 이미 켜져 있던 LV를 그 사이 시작한 VM이 그대로 쓸 수 있으므로 VM·Task는 LV마다 끄기 직전에 다시 읽는다. 열린 장치(실행 중인 VM의 디스크)는 `lvchange -an`이 실패해 켜진 채 남으므로 잘못 끄려 해도 경고만 남는다(드라이버 실제 시험 `an_open_volume_stays_active`).
- **노드 장애:** 죽은 노드의 잠금은 sanlock 임대가 끝나면(기본 수 분) 풀린다. 그 전에는 다른 노드에서 그 디스크로 VM을 켤 수 없다(일시 오류).
```

`docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`에서

```markdown
  3. `vgs --reportformat json -o vg_name,vg_lock_type,vg_size,vg_free {vg}`: 잠금 종류가 `sanlock`·`dlm`이 아니면 준비 안 됨(`"volume group {vg} is not a shared (lvmlockd) VG"`). 용량은 크기·남은 공간.
  - 실패하면 이유(stderr 첫 줄). macOS는 `"shared LVM pools are not supported on macOS"`.
- 볼륨 Task 노드: RBD와 같이 풀을 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드(LV를 바꾸는 명령이 아직 돈다는 표시가 노드마다 따로이므로). 가져오기는 업로드를 받은 노드, VM 디스크 크기는 VM 노드(기존 규칙).

## 6. 클라이언트

- **oplctl:** `pool create NAME --lvm-shared VG [--iscsi-portal PORTAL --iscsi-target IQN]`(둘은 함께, 다른 종류와 함께 못 씀), 목록·상세의 종류 `lvm-shared`·위치 `VG` 또는 `VG @ PORTAL TARGET`, 형식 기본값 raw.
- **웹:** 풀 만들기의 공유 LVM(VG, iSCSI 사용 여부, 포털, 타깃 — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, 공유 LVM 디스크 VM의 스냅샷 버튼은 이유와 함께 막는다.
```

을 다음으로 바꾼다:

```markdown
  3. `vgs --reportformat json -o vg_name,vg_lock_type,vg_size,vg_free {vg}`: 잠금 종류가 `sanlock`·`dlm`이 아니면 준비 안 됨(`"volume group {vg} is not a shared (lvmlockd) VG"`). 용량은 크기·남은 공간.
  - 실패하면 이유(stderr 첫 줄). macOS는 `"shared LVM pools are not supported on macOS"`. 구현: macOS 문구는 다른 블록 풀처럼 종류 이름을 쓴 `"lvm-shared pools are not supported on macOS"`다(마운터·resolve 같다). `MountOps::iscsi_login(portal, target)`·`lvm_lockstart(vg)`·`lvm_vg(vg) -> VgInfo`. 세션은 `iscsiadm -m session` 출력의 넷째 칸(타깃 이름)으로 찾고 포털은 보지 않는다(호스트 이름 포털도 세션에는 주소로 보인다). `-m session`의 종료 코드 21은 세션 없음, `--login`의 15("already present")는 성공이다. 로그인·잠금 시작은 죽이지 않는 뒤 작업으로 풀마다 하나만 돌고(`lvm_shared_checks`, 풀을 이 노드에서 쓸 수 없게 되어도 끝날 때까지 두어 다시 쓸 수 있게 되면 그것을 기다린다) 확인 시간(10초) 안에 끝나지 않으면 `"iscsi login or vg lock start in progress"`로 알리며 다음 새로 고침이 결과를 거둔다. `vgs`만 시간 제한이 있다(`"vgs timed out"`, `--units b --nosuffix`).
- 볼륨 Task 노드: RBD와 같이 풀을 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드(LV를 바꾸는 명령이 아직 돈다는 표시가 노드마다 따로이므로). 가져오기는 업로드를 받은 노드, VM 디스크 크기는 VM 노드(기존 규칙). 구현: `pool_node`가 RBD와 함께 공유 LVM 풀도 요청을 받은 노드를 앞세우지 않는다.

## 6. 클라이언트

- **oplctl:** `pool create NAME --lvm-shared VG [--iscsi-portal PORTAL --iscsi-target IQN]`(둘은 함께, 다른 종류와 함께 못 씀), 목록·상세의 종류 `lvm-shared`·위치 `VG` 또는 `VG @ PORTAL TARGET`, 형식 기본값 raw. 구현: `--iscsi-portal`·`--iscsi-target`은 서로와 `--lvm-shared`가 필요하고 `--lvm-shared`는 다른 종류·`--shared`와 함께 못 쓴다. 본문은 `{"kind":"lvm-shared","vg","iscsi"}`(iSCSI가 없으면 `null`)이고 `shared`는 적지 않는다.
- **웹:** 풀 만들기의 공유 LVM(VG, iSCSI 사용 여부, 포털, 타깃 — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, 공유 LVM 디스크 VM의 스냅샷 버튼은 이유와 함께 막는다. 구현: 종류 "Shared LVM"(`pools.kinds.lvm-shared`, `pools.kindHint.lvm-shared`), "iSCSI 타깃에 로그인" 체크(`pools.iscsi`)를 고르면 포털·타깃 입력. `isShared`가 공유 LVM을 늘 공유로 보고(용량을 하나로 보인다) 위치는 oplctl과 같다. `poolFormats`는 raw만. `pools.deleteDescription`은 공유 VG의 LV도 남는다고 적는다.
```

`docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`에서

```markdown
- **단위:** 타입·검증, API 규칙(공유·형식·스냅샷 409), 가짜 `LvmOps`로 공유 LVM 드라이버 각 동작(0 채우기, 배타 활성화, 다른 노드가 쥔 경우), 마운터(iscsiadm·lockstart·vgs, 잠금 없는 VG), reconciler의 끄기·정리, oplctl·웹.
- **실제(`lvm-shared-it`, Linux·root):** CI `linux-kvm`에 `targetcli-fb`(LIO, 파일 백스토어, 데모 모드 ACL)로 루프백 iSCSI 타깃, `open-iscsi`로 로그인, `sanlock`·`lvm2-lockd`(softdog, `host_id = 1`)로 공유 VG. `OPL_TEST_LVM_SHARED=VG`, `OPL_TEST_ISCSI=PORTAL,TARGET`. 드라이버 통합 시험과 opld E2E(공유 LVM 디스크 VM 켜고 끄기 — 끈 뒤 LV가 꺼졌는지, 디렉터리 풀로 복제, 지우기). sanlock·lvmlockd를 CI에서 띄울 수 없으면 iSCSI 로그인만 CI에서 시험하고 잠금 부분은 수동 시험으로 남긴다(설계에 기록).
```

을 다음으로 바꾼다:

```markdown
- **단위:** 타입·검증, API 규칙(공유·형식·스냅샷 409), 가짜 `LvmOps`로 공유 LVM 드라이버 각 동작(0 채우기, 배타 활성화, 다른 노드가 쥔 경우), 마운터(iscsiadm·lockstart·vgs, 잠금 없는 VG), reconciler의 끄기·정리, oplctl·웹.
- **실제(`lvm-shared-it`, Linux·root):** CI `linux-kvm`에 `targetcli-fb`(LIO, 파일 백스토어, 데모 모드 ACL)로 루프백 iSCSI 타깃, `open-iscsi`로 로그인, `sanlock`·`lvm2-lockd`(softdog, `host_id = 1`)로 공유 VG. `OPL_TEST_LVM_SHARED=VG`, `OPL_TEST_ISCSI=PORTAL,TARGET`. 드라이버 통합 시험과 opld E2E(공유 LVM 디스크 VM 켜고 끄기 — 끈 뒤 LV가 꺼졌는지, 디렉터리 풀로 복제, 지우기). sanlock·lvmlockd를 CI에서 띄울 수 없으면 iSCSI 로그인만 CI에서 시험하고 잠금 부분은 수동 시험으로 남긴다(설계에 기록). 구현: 기능 `lvm-shared-it`(opl-driver, opld). `crates/opl-driver/tests/lvm_shared_storage.rs`(5개: 만들기·0 채우기·배타 활성화·크기·지우기, 늘린 부분의 0 채우기(바로 뒤에 놓였다 지운 볼륨의 패턴이 보이지 않는지 — 그 볼륨이 바로 뒤 익스텐트에 있었고 늘린 부분이 그 자리에 놓였는지 `lvs -o seg_pe_ranges`로 확인, 켜진 볼륨은 켠 채로, 남은 태그는 켜기가 마저 채우는지), 연 장치는 끌 수 없음, vmdk 가져오기와 실패한 가져오기 정리, 백엔드 안팎 복사. 한 VG를 쓰므로 파일 안 시험은 정적 뮤텍스로 하나씩 돈다), `crates/opld/tests/lvm_shared.rs`(1개: `OPL_TEST_ISCSI`가 있으면 잠금을 멈추고 로그아웃한 뒤 opld가 처음부터 로그인·잠금 시작해 준비하는지, VM을 켜면 LV가 켜지고 `host_device`로 열리는지, 스냅샷 409, opld를 다시 띄울 때 남은 활성화 정리와 실행 중인 VM의 LV는 그대로인지, 멈춘 뒤 LV가 꺼지는지, 디렉터리 풀과 같은 풀로 복제, 지우기). opld 시험은 `--format raw`를 명시한다. CI 단계 "iSCSI target and a shared (lvmlockd) VG"(10분 제한): IQN `iqn.2003-01.org.linux-iscsi.opl:shared`, 2G 파일 백스토어, `127.0.0.1` 포털 로그인, `use_lvmlockd = 1`·`host_id = 1`, `wdmd sanlock lvmlockd`, VG `oplshared`(`vgcreate --shared`는 잠금 시작까지 약 24초). lvmlockd를 켜도 LVM-thin 시험의 로컬 VG는 그대로 된다. sanlock·lvmlockd가 CI에서 떴으므로 대안(로그인만 시험)은 쓰지 않았다. Linux 전용 resolve 시험 `activates_lvm_shared_volumes_and_waits_for_the_device`도 CI에서 돈다.
```

`docs/superpowers/specs/2026-10-30-shared-storage-iscsi-lvm-design.md`에서

```markdown
- macOS 노드는 공유 LVM을 쓸 수 없다.
```

을 다음으로 바꾼다:

```markdown
- macOS 노드는 공유 LVM을 쓸 수 없다.
- 끄지 못한 LV(경고만 남긴 경우)는 다음 정리(opld 시작 때나 30분마다)까지 그 노드에서 켜져 있고, 그동안 다른 노드에서 그 디스크로 VM을 켜거나 그 볼륨을 늘리거나 지우면 일시 오류로 기다린다.
- 죽은 노드에서 돌던 크기 늘리기 Task는 그 노드가 돌아오거나 클러스터에서 빠질 때까지 끝나지 않으므로, 그동안 그 디스크의 VM은 다른 노드에서도 시작하지 않고 `starting`에서 기다린다(오프라인 노드의 대기 중(`pending`) Task는 약 1분 뒤 정리 작업이 실패로 끝낸다). 백엔드 공통이다.
- LV를 바꾸는 명령이 돈다는 표시는 노드마다 따로다(RBD와 같다). 볼륨 Task를 한 노드로 모으고, 그 밖의 경우는 lvmlockd 잠금이 노드 사이를 막는다.
- 풀을 지워도 iSCSI 로그아웃이나 잠금 멈춤(`vgchange --lockstop`)은 하지 않는다(관리자가 한다).
```

`web/src/i18n/en.json`에서

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export and images in its Ceph pool are kept.",
    "deleted": "Deleted storage pool {{name}}.",
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export, images in its Ceph pool and logical volumes in its shared volume group are kept.",
    "deleted": "Deleted storage pool {{name}}.",
```

`web/src/i18n/ko.json`에서

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일과 Ceph 풀의 이미지는 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일, Ceph 풀의 이미지와 공유 볼륨 그룹의 논리 볼륨은 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 652개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e --test snapshots`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 297개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs web
git commit -m "docs: 공유 LVM(iSCSI) 풀 안내, 공유 스토리지 설계 5에 구현하며 정한 것"
```

---

## 완료 기준 (계획 19)

- 공유 LVM 풀(iSCSI 있음·없음)을 API·oplctl·웹으로 만들 수 있고, `shared: false`·qcow2·잘못된 VG·포털·타깃은 400이다.
- 노드가 iSCSI 로그인·잠금 시작·VG 잠금 종류를 확인해 상태와 용량을 알리고, 잠금 없는 VG와 macOS는 이유를 남긴다.
- 공유 LVM 볼륨의 만들기·가져오기·지우기·크기 늘리기·복제가 되고, 새 볼륨과 늘어난 부분은 0으로 읽힌다. 공유 LVM 디스크 VM의 스냅샷은 409다.
- VM은 디스크를 이 노드에서만 켜서 쓰고, 멈추면 끈다. 남은 활성화는 정리되고, 다른 노드가 쥔 볼륨은 일시 오류로 기다린다.
- Linux에서 실제 iSCSI 타깃과 sanlock 공유 VG로 드라이버 통합 테스트와 VM E2E가 통과한다(`lvm-shared-it`, CI `linux-kvm`).
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **스냅샷이 없다**(링크드 클론도 없다, 복제는 전체 복사).
- **새 볼륨과 늘린 부분을 0으로 채우므로** 만들기·크기 늘리기 시간이 크기에 비례한다. 0 채우기가 실패한 볼륨은 다음 VM 시작이 남은 부분을 채운다.
- **노드가 죽으면** sanlock 임대가 끝날 때까지 그 노드가 켠 디스크를 다른 노드에서 쓸 수 없다. 한 노드에서 끄기가 실패하면 그 노드의 정리(30분)까지 다른 노드에서 그 볼륨을 쓸 수 없다.
- **각 노드에 lvm2-lockd·sanlock과 고유한 `host_id`**, iSCSI면 open-iscsi가 있어야 한다. CHAP은 지원하지 않는다. macOS는 공유 LVM을 쓸 수 없다.
- 그 밖의 제한은 설계 문서 §8에 있다.

## 실행 후 기록 (2026-10-30)

8개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). 계획 문서를 만들기 전에 스테이지 작업에서 리뷰를 네 차례 돌려 고쳤다(크기 늘리기의 0 채우기와 남은 태그, 남은 활성화 정리의 다시 확인, 잠금 오류 문구, 크기 늘리기와 VM 시작의 경쟁 — 위 Task의 코드에 이미 들어 있다).

Task 8의 확인에서 `cargo test -p opl-api --features storage-it`의 `snapshot_disks::resize_waits_for_a_revert_of_the_same_disk`가 실패해 아래를 고쳤다(`371b337`). 저장소의 코드가 기준이다. 위 Task 4의 코드 블록은 고치기 전 것이다.

- VM 시작은 디스크를 대상으로 하는 **실행 중(`running`)인** 크기 늘리기 Task만 기다린다. 끝나지 않은 Task를 모두 기다리면, 메모리 스냅샷 되돌리기(VM을 다시 켠다) 뒤에 줄 선 `pending` 크기 늘리기와 서로 기다려 끝나지 않았다. 경쟁은 그대로 닫힌다: 실행기는 Task를 `running`으로 기록(커밋·적용)한 뒤에 VM을 읽으므로, 그 전에 켜기 시작한 VM은 실행기가 켜는 중으로 보고 꺼진 VM 경로를 타지 않고, 그 뒤에 켜는 VM은 `running` Task를 보고 기다린다. 테스트 `start_does_not_wait_for_a_pending_resize`.
- CI는 `opl-api`의 `storage-it` 중 `volumes`만 돌리므로 이 실패를 잡지 못했다. 계획을 마칠 때 `storage-it` 전체를 macOS에서 돌린다.

테스트: macOS `cargo test --workspace` 653개, `opl-api` `storage-it` 223개, `opl-driver` `storage-it` `dir_storage` 8개, `qemu-it` `smoke`·`e2e`·`snapshots` 11개, 브라우저 E2E, 웹 Vitest 297개, `fmt`·`clippy -D warnings`(모든 기능). 실제 공유 LVM(`lvm-shared-it`)·RBD·ZFS·LVM·NFS는 Linux CI `linux-kvm`에서.
CI(확인용 브랜치, 리뷰 수정마다): `check` 2개, `deny`, `web`, `linux-kvm`(실제 iSCSI 타깃과 sanlock 공유 VG의 드라이버 시험 5개와 E2E 포함)이 모두 통과했다. 위 수정은 main CI에서 확인한다.

## 다음 계획으로 넘길 사항

- 하위 프로젝트 4: 라이브 마이그레이션(공유 풀 — NFS·RBD·공유 LVM이 바탕), UEFI 변수 공유, 풀 `shared` 바꾸기.
- CI에서 `opl-api`의 `storage-it` 전체를 돌리는 것을 검토한다.
