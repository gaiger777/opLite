# opLite 계획 18: 공유 스토리지 4 — Ceph RBD 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 모든 노드가 같은 이미지를 보는 공유 블록 스토리지 Ceph RBD 풀을 더한다. QEMU는 `driver=rbd`로 이미지에 바로 붙고, 볼륨·스냅샷·복제는 `rbd` CLI와 `qemu-img`의 rbd 지원으로 한다.

**Architecture:** `PoolBackend::Rbd { pool, namespace, user, conf }`는 항상 공유이고 볼륨은 raw 이미지 `opl-{id}`다. `disk_location`은 RBD면 경로 대신 `DiskLocation::Rbd(RbdTarget)`을 돌려주고, QEMU 명령줄은 `-blockdev driver=rbd,...`를 쓴다. `RbdStorage`가 `RbdOps`(실제는 `SystemRbd` — 계획 17의 `BlockTool`)로 만들기·지우기(watcher가 있으면 기다림)·크기·스냅샷·아무 스냅샷으로 되돌리기를 하고, 복사·가져오기는 `qemu-img`의 image-opts로 rbd를 읽고 쓴다. 공유 RBD 볼륨의 Task는 정해진 한 노드로 보내고, 남은 스냅샷 태그는 30분이 넘은 것만 주기적으로 정리한다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8, utoipa, clap, QEMU QMP, Ceph(`rbd`, `ceph`, librbd — Ubuntu `ceph-common`·`qemu-block-extra`); 웹은 계획 8~17과 같다. 새 외부 의존성은 `time`(opl-driver, `rbd snap ls` 시각 해석). 실제 Ceph 시험은 Linux CI의 microceph(snap, squid).

**설계 문서:** `docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md` (전체가 이 계획, "구현:" 메모 포함)
**선행 계획:** 계획 1~17 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `PoolBackend::Rbd`(항상 공유, raw만, Ceph 이름·conf 검증, 사용자 이름의 `client.` 접두어 거절) | Ceph 클러스터 설치·관리(관리자), `krbd`, 링크드 클론, 메모리 스냅샷 |
| `DiskKind::Rbd`·`RbdTarget`·`DiskLocation`, QEMU `driver=rbd` | macOS의 RBD(Homebrew QEMU에 rbd가 없다) |
| `RbdStorage`·`RbdOps`·`SystemRbd`, `Storages::with_rbd`, image-opts 복사 | iSCSI — 계획 19 |
| 마운터의 RBD 확인(QEMU rbd 드라이버, `rbd ls`, `ceph df`), resolve | 라이브 마이그레이션 — 하위 프로젝트 4(RBD가 바탕) |
| 블록 스냅샷 규칙에 rbd(아무 스냅샷으로 되돌리기), 공유 풀의 Task 노드와 태그 정리 | |
| oplctl `pool create --rbd`, 웹의 RBD 풀 | |
| 실제 Ceph 시험(`rbd-it`, CI microceph, 제한된 사용자·네임스페이스·conf 포함) | |

## 이전 계획에서 이어받는 사항

- 계획 15~17(공유 스토리지 1~3): 백엔드 구조, 노드별 풀 상태, 블록 백엔드 바탕(`Storages`, 백엔드 사이 복사, 멈춤 경로), `BlockTool`, 블록 스냅샷 규칙과 남은 태그 정리 위에 RBD를 붙인다.

## 실행 환경

계획 17과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. **실제 Ceph 시험(`opl-driver`·`opld`의 `rbd-it`)은 Linux에서 root로만 돈다.** CI `linux-kvm` 작업이 `ceph-common`·`qemu-block-extra`·`jq`를 깔고 microceph(snap, `squid/stable`)를 띄워 루프 OSD 하나, 복제 수 1의 `rbd` 풀, 네임스페이스 `rbd/opl`, 제한된 사용자 `client.opltest`(`/etc/opl-ceph/ceph.conf`)를 만든 뒤 `OPL_TEST_RBD=rbd`(와 `OPL_TEST_RBD_USER`·`_NAMESPACE`·`_CONF`)로 돌린다. microceph의 snap 별칭(`rbd`·`ceph`)은 해제한다(`/snap/bin`이 PATH에서 `/usr/bin`보다 앞이라 snap에 갇힌 클라이언트가 잡힌다). macOS에서는 컴파일되지 않는다(시험 파일이 Linux 전용).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1·9), `Cargo.lock`(Task 3에서 opl-driver의 `time`, `cargo build`가 고친다).

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **RBD 풀.** 항상 공유(`shared: false`는 400), raw만. 풀·네임스페이스·사용자는 `A-Za-z0-9_.-` 1–63자(`-`로 시작 금지), 사용자는 `client.` 없이(있으면 400), conf는 절대 경로에 제어 문자·`,`·`:`·`=` 없음. 이미지 `opl-{id}`, 스냅샷 `@{tag}`.
- **디스크.** QEMU `-blockdev driver=rbd,pool=..[,namespace=..],image=..,user=..[,conf=..]` 뒤 `driver=raw`. 값의 `,`는 `,,`로.
- **명령.** `rbd --id U [--conf C]`, `LC_ALL=C`, `TZ=UTC`(스냅샷 시각), 60초(지우기·되돌리기는 600초), 바꾸는 명령은 죽이지 않고 이미지마다 하나. 지우기·되돌리기는 watcher가 있으면 일시 오류. 스냅샷은 이미지마다 차례로(멈춘 동안 찍어 일관된다).
- **복사.** `qemu-img`의 image-opts(`driver=raw,file.driver=rbd,...`)로 rbd를 읽고 쓴다. rbd 대상은 0으로 읽힌다.
- **노드별 상태.** QEMU에 rbd 블록 드라이버가 있는지(`-drive driver=rbd`의 오류 문구), `rbd ls`, `ceph df detail`(할당량이 있으면 그 안에서). macOS는 지원 안 함.
- **공유 풀의 규칙.** VM과 Task는 풀을 쓸 수 있는 어느 노드에서나 돈다. 공유 RBD 볼륨의 Task(만들기·지우기·크기)는 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드로 보낸다(진행 중 표시가 노드마다 따로라서). 남은 블록 스냅샷 태그 정리는 시작할 때와 30분마다, 공유 풀에서는 만든 지 30분이 넘은 태그만.
- **혼합 버전 클러스터**는 지원하지 않는다(모든 노드를 올린 뒤 RBD 풀을 만든다).

## 파일 구조

```
crates/opl-types/src/{storage.rs, runtime.rs}               # PoolBackend::Rbd, 검증, DiskKind::Rbd, RbdTarget
crates/opl-qemu/src/cmdline.rs                               # driver=rbd
crates/opl-driver/src/storage/rbd.rs(새로), storage.rs, storage/{storages.rs, lvm.rs, zfs.rs}, error.rs, lib.rs   # RbdStorage, DiskLocation, image-opts 복사
crates/opl-reconciler/src/resolve.rs, crates/opld/src/{pools.rs, member.rs}   # RBD 해석, 마운터, 연결
crates/opl-api/src/{resources.rs, volumes.rs, snapshots.rs, tasks.rs, tasks/*.rs}   # 규칙, 공유 볼륨 Task 노드, 태그 정리
crates/opl-driver/tests/rbd_storage.rs, crates/opld/tests/rbd.rs (새로)   # rbd-it
crates/oplctl/src/cli.rs, web/src/{storage,vms}/..., web/src/i18n/{ko,en}.json
.github/workflows/ci.yml, README.md, docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md, 2026-10-28-shared-storage-nfs-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입과 API 규칙: Ceph RBD 풀

`PoolBackend::Rbd { pool, namespace, user, conf }`(serde `rbd`)과 `PoolError::BadRbdName`·`BadRbdConf`·`RbdUserPrefix`(Ceph 이름 `A-Za-z0-9_.-` 1–63자, `-`로 시작 금지, 사용자의 `client.` 접두어 금지, conf는 절대 경로에 제어 문자·`,`·`:`·`=` 없음), `is_shared`는 RBD면 항상 참, raw만. `runtime::DiskKind::Rbd`·`RbdTarget`·`RuntimeDisk.rbd`(옛 형식은 없음). API: RBD 풀은 `shared`가 없으면 참, `false`면 400, qcow2는 400, 가져오기 기본 raw. `block_snapshot_kind`는 `"rbd"`. 이 Task에서 `disk_location`·`Storages`·마운터·resolve는 RBD를 아직 지원하지 않는다는 오류를 낸다(QEMU 명령줄도). 웹 타입을 다시 만들고 풀 위치에 `POOL[/NAMESPACE]`.

**Files:**
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-qemu/src/cmdline.rs`
- Modify: `crates/opl-qemu/tests/qemu_boot.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/runtime.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opld/src/pools.rs`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/pools.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    if matches!(spec.backend, PoolBackend::Nfs { .. }) {
        if shared_given && !spec.shared {
            return Err(ApiError::bad_request("nfs pools are always shared"));
        }
```

을 다음으로 바꾼다:

```rust
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS·RBD 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    let always_shared = match spec.backend {
        PoolBackend::Nfs { .. } => Some("nfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::Dir { .. } | PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => None,
    };
    if let Some(kind) = always_shared {
        if shared_given && !spec.shared {
            return Err(ApiError::bad_request(format!(
                "{kind} pools are always shared"
            )));
        }
```

`crates/opl-api/src/snapshots.rs`에서

```rust

/// 디스크 스냅샷을 블록 장치의 스냅샷으로 두는 풀의 종류(`lvm-thin`, `zfs`). 파일 풀(qcow2 내부
/// 스냅샷)이면 `None`.
pub(crate) fn block_snapshot_kind(pool: &StoragePool) -> Option<&'static str> {
```

을 다음으로 바꾼다:

```rust

/// 디스크 스냅샷을 블록 장치의 스냅샷으로 두는 풀의 종류(`lvm-thin`, `zfs`, `rbd`). 파일 풀(qcow2
/// 내부 스냅샷)이면 `None`.
pub(crate) fn block_snapshot_kind(pool: &StoragePool) -> Option<&'static str> {
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        PoolBackend::Zfs { .. } => Some("zfs"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
```

을 다음으로 바꾼다:

```rust
        PoolBackend::Zfs { .. } => Some("zfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        assert_eq!(block_snapshot_kind(&zfs), Some("zfs"));
        assert!(!uses_block_snapshots(&dir));
```

을 다음으로 바꾼다:

```rust
        assert_eq!(block_snapshot_kind(&zfs), Some("zfs"));
        let rbd = pool(PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        });
        assert_eq!(block_snapshot_kind(&rbd), Some("rbd"));
        assert!(!uses_block_snapshots(&dir));
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
```

을 다음으로 바꾼다:

```rust

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS, RBD)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
```

`crates/opl-api/src/volumes.rs`에서

```rust
        PoolBackend::Zfs { .. } => "zfs",
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => return Ok(()),
```

을 다음으로 바꾼다:

```rust
        PoolBackend::Zfs { .. } => "zfs",
        PoolBackend::Rbd { .. } => "rbd",
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => return Ok(()),
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀(LVM-thin, ZFS)은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => ImageFormat::Raw,
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
```

을 다음으로 바꾼다:

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD)은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            ImageFormat::Raw
        }
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
```

`crates/opl-api/tests/pools.rs`에서

```rust
            "dataset with '@'",
        ),
```

을 다음으로 바꾼다:

```rust
            "dataset with '@'",
        ),
        (
            pool("r", json!({ "kind": "rbd", "pool": "-rbd" })),
            "rbd pool starting with '-'",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "namespace": "a/b" }),
            ),
            "rbd namespace with '/'",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "conf": "/etc/ceph/a,b.conf" }),
            ),
            "rbd conf with ','",
        ),
        (
            pool(
                "r",
                json!({ "kind": "rbd", "pool": "rbd", "user": "client.admin" }),
            ),
            "ceph user with client.",
        ),
```

`crates/opl-api/tests/pools.rs`에서

```rust

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

을 다음으로 바꾼다:

```rust

/// RBD 풀은 늘 공유이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn rbd_pools_are_always_shared_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let rbd = json!({ "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl" });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "r2", "spec": { "backend": rbd, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "rbd pools are always shared", "{body}");
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin.post("/storage-pools", pool("r", rbd.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(
        body["spec"]["backend"],
        json!({ "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl", "conf": null }),
        "{body}"
    );
    env.pool_ready("r").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "r", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "rbd pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=r&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "rbd pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=r&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

`crates/opl-driver/src/storage.rs`에서

```rust
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다).
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

을 다음으로 바꾼다:

```rust
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는
/// 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

`crates/opl-driver/src/storage.rs`에서

```rust
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
    }
```

을 다음으로 바꾼다:

```rust
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
        PoolBackend::Rbd { .. } => PathBuf::new(),
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust
    // 블록 풀의 볼륨은 파일이 아니다(`/dev/{vg}`, `/dev/zvol/{dataset}`에 파일을 만들지 않는다).
    match &pool.spec.backend {
```

을 다음으로 바꾼다:

```rust
    // 블록 풀의 볼륨은 파일이 아니다(`/dev/{vg}`, `/dev/zvol/{dataset}`에 파일을 만들지 않는다).
    // RBD 풀의 볼륨은 Ceph 이미지다.
    match &pool.spec.backend {
```

`crates/opl-driver/src/storage.rs`에서

```rust
                "zfs pools hold no volume files".into(),
            ));
```

을 다음으로 바꾼다:

```rust
                "zfs pools hold no volume files".into(),
            ));
        }
        PoolBackend::Rbd { .. } => {
            return Err(DriverError::InvalidSpec(
                "rbd pools hold no volume files".into(),
            ));
```

`crates/opl-driver/src/storage.rs`에서

```rust
        return Ok((zfs::volume_device(dev, dataset, id)?, DiskKind::Block));
    }
```

을 다음으로 바꾼다:

```rust
        return Ok((zfs::volume_device(dev, dataset, id)?, DiskKind::Block));
    }
    if let PoolBackend::Rbd { .. } = &pool.spec.backend {
        return Err(DriverError::InvalidSpec(
            "rbd pools are not supported yet".into(),
        ));
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust

    /// ZFS 풀의 디스크는 zvol 장치다. 장치 노드가 없으면(아직 생기지 않음) 일시 오류다.
```

을 다음으로 바꾼다:

```rust

    /// 파일 저장소는 RBD 풀도 다루지 않고, 디스크 위치도 아직 정하지 않는다.
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
        match disk_location(&p, mnt.path(), id, f).unwrap_err() {
            DriverError::InvalidSpec(msg) => assert_eq!(msg, "rbd pools are not supported yet"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// ZFS 풀의 디스크는 zvol 장치다. 장치 노드가 없으면(아직 생기지 않음) 일시 오류다.
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::Zfs { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

을 다음으로 바꾼다:

```rust
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
    Zfs,
}
```

을 다음으로 바꾼다:

```rust
    Zfs,
    /// 아직 다루는 저장소가 없다.
    Rbd,
}
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            PoolBackend::Zfs { .. } => Kind::Zfs,
        }
```

을 다음으로 바꾼다:

```rust
            PoolBackend::Zfs { .. } => Kind::Zfs,
            PoolBackend::Rbd { .. } => Kind::Rbd,
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                .ok_or_else(|| DriverError::InvalidSpec("zfs pools are not supported yet".into())),
        }
```

을 다음으로 바꾼다:

```rust
                .ok_or_else(|| DriverError::InvalidSpec("zfs pools are not supported yet".into())),
            Kind::Rbd => Err(DriverError::InvalidSpec(
                "rbd pools are not supported yet".into(),
            )),
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

을 다음으로 바꾼다:

```rust

    /// RBD 풀은 아직 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn rbd_pool_is_unsupported_for_now() {
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

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
            PoolBackend::Zfs { dataset } => Ok(dataset),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::LvmThin { .. } => {
                Err(DriverError::InvalidSpec("not a zfs pool".into()))
            }
        }
```

을 다음으로 바꾼다:

```rust
            PoolBackend::Zfs { dataset } => Ok(dataset),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Rbd { .. } => Err(DriverError::InvalidSpec("not a zfs pool".into())),
        }
```

`crates/opl-driver/tests/qemu_driver.rs`에서

```rust
        kind: DiskKind::File,
        format: ImageFormat::Qcow2,
```

을 다음으로 바꾼다:

```rust
        kind: DiskKind::File,
        rbd: None,
        format: ImageFormat::Qcow2,
```

`crates/opl-qemu/src/cmdline.rs` 전체:

```rust
use std::path::Path;

use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
use thiserror::Error;

use crate::host::{Accel, HostProfile};
use crate::paths::RuntimePaths;

/// q35의 내장 AHCI 컨트롤러 포트 수
const Q35_SATA_PORTS: usize = 6;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CmdlineError {
    #[error("machine type {machine:?} is not supported on {arch:?} hosts")]
    MachineArchMismatch { machine: MachineType, arch: Arch },
    #[error("BIOS firmware is not supported on aarch64; use UEFI")]
    BiosOnAarch64,
    #[error("SATA disks require the q35 machine type")]
    SataRequiresQ35,
    #[error("at most {Q35_SATA_PORTS} SATA disks are supported")]
    TooManySataDisks,
    #[error("rbd disks are not supported yet")]
    RbdDisksUnsupported,
}

/// QEMU 인자 목록(바이너리 제외)을 만든다. 부수 효과가 없는 순수 함수다.
///
/// VM은 항상 `-S`(일시정지) 상태로 시작한다. 호출자는 QMP 연결 뒤 `cont`로 실행을 재개한다.
pub fn build_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    check_compat(spec, host)?;

    let mut a = Args::default();
    a.opt(
        "-name",
        format!("guest={},debug-threads=on", esc(&spec.name)),
    );
    a.opt("-uuid", spec.id.to_string());
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    a.flag("-S");
    let machine = match spec.machine {
        MachineType::Q35 => "q35",
        MachineType::Virt => "virt",
    };
    a.opt(
        "-machine",
        format!("{machine},accel={}", host.accel.as_str()),
    );
    let cpu = match (spec.cpu.model, host.accel) {
        (CpuModel::Host, Accel::Tcg) | (CpuModel::Max, _) => "max",
        (CpuModel::Host, _) => "host",
    };
    a.opt("-cpu", cpu.to_string());
    a.opt(
        "-smp",
        format!(
            "sockets={},cores={},threads=1",
            spec.cpu.sockets, spec.cpu.cores
        ),
    );
    a.opt("-m", format!("{}M", spec.memory_mib));

    if spec.firmware == Firmware::Uefi {
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=0,format=raw,readonly=on,file={}",
                esc_path(&host.uefi_code)
            ),
        );
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=1,format=raw,file={}",
                esc_path(&paths.uefi_vars)
            ),
        );
    }

    let video = match host.arch {
        Arch::X86_64 => "VGA,id=video0",
        Arch::Aarch64 => "virtio-gpu-pci,id=video0",
    };
    a.opt("-device", video.to_string());
    a.opt("-device", "qemu-xhci,id=usb".to_string());
    a.opt("-device", "usb-tablet,bus=usb.0".to_string());
    a.opt("-device", "usb-kbd,bus=usb.0".to_string());

    let mut sata_port = 0;
    for (i, disk) in spec.disks.iter().enumerate() {
        let fmt = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        let proto = match disk.kind {
            DiskKind::File => "file",
            DiskKind::Block => "host_device",
            DiskKind::Rbd => return Err(CmdlineError::RbdDisksUnsupported),
        };
        a.opt(
            "-blockdev",
            format!(
                "driver={proto},filename={},node-name=disk{i}-file",
                esc_path(&disk.path)
            ),
        );
        a.opt(
            "-blockdev",
            format!("driver={fmt},file=disk{i}-file,node-name=disk{i}"),
        );
        let mut dev = match disk.bus {
            DiskBus::VirtioBlk => format!("virtio-blk-pci,drive=disk{i},id=disk{i}-dev"),
            DiskBus::Sata => {
                let d = format!("ide-hd,drive=disk{i},bus=ide.{sata_port},id=disk{i}-dev");
                sata_port += 1;
                d
            }
        };
        if let Some(order) = disk.boot_order {
            dev.push_str(&format!(",bootindex={order}"));
        }
        a.opt("-device", dev);
    }

    for (i, nic) in spec.nics.iter().enumerate() {
        let netdev = match &nic.backend {
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname, .. } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
                esc(ifname)
            ),
        };
        a.opt("-netdev", netdev);
        let model = match nic.model {
            NicModel::Virtio => "virtio-net-pci",
            NicModel::E1000e => "e1000e",
        };
        a.opt(
            "-device",
            format!("{model},netdev=net{i},mac={},id=nic{i}", nic.mac),
        );
    }

    a.opt("-vnc", format!("unix:{}", esc_path(&paths.vnc_socket)));
    a.opt(
        "-qmp",
        format!("unix:{},server=on,wait=off", esc_path(&paths.qmp_socket)),
    );
    a.opt("-pidfile", paths.pidfile.display().to_string());
    Ok(a.0)
}

fn check_compat(spec: &VmRuntimeSpec, host: &HostProfile) -> Result<(), CmdlineError> {
    match (spec.machine, host.arch) {
        (MachineType::Q35, Arch::X86_64) | (MachineType::Virt, Arch::Aarch64) => {}
        (machine, arch) => return Err(CmdlineError::MachineArchMismatch { machine, arch }),
    }
    if host.arch == Arch::Aarch64 && spec.firmware == Firmware::Bios {
        return Err(CmdlineError::BiosOnAarch64);
    }
    let sata = spec.disks.iter().filter(|d| d.bus == DiskBus::Sata).count();
    if sata > 0 && spec.machine != MachineType::Q35 {
        return Err(CmdlineError::SataRequiresQ35);
    }
    if sata > Q35_SATA_PORTS {
        return Err(CmdlineError::TooManySataDisks);
    }
    Ok(())
}

#[derive(Default)]
struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, flag: &str) {
        self.0.push(flag.to_string());
    }

    fn opt(&mut self, flag: &str, value: String) {
        self.0.push(flag.to_string());
        self.0.push(value);
    }
}

/// QEMU 옵션 값 안의 쉼표는 두 번 써서 이스케이프한다.
fn esc(s: &str) -> String {
    s.replace(',', ",,")
}

fn esc_path(p: &Path) -> String {
    esc(&p.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use opl_types::runtime::{DiskKind, RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
    use uuid::Uuid;

    use super::*;

    fn x86_host() -> HostProfile {
        HostProfile {
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
            uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
        }
    }

    fn mac_host() -> HostProfile {
        HostProfile::homebrew_hvf(Path::new("/opt/homebrew/share/qemu"))
    }

    fn paths() -> RuntimePaths {
        RuntimePaths::new(Path::new("/run/opl"), Uuid::nil()).unwrap()
    }

    fn x86_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::nil(),
            name: "web-01".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 4,
                model: CpuModel::Host,
            },
            memory_mib: 8192,
            firmware: Firmware::Uefi,
            machine: MachineType::Q35,
            disks: vec![
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-root.qcow2".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Qcow2,
                    bus: DiskBus::VirtioBlk,
                    boot_order: Some(1),
                },
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-data.raw".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Raw,
                    bus: DiskBus::Sata,
                    boot_order: None,
                },
            ],
            nics: vec![
                RuntimeNic {
                    mac: "52:54:00:12:34:56".parse().unwrap(),
                    model: NicModel::Virtio,
                    backend: NicBackend::Tap {
                        ifname: "tap0i0".into(),
                        bridge: "vmbr0".into(),
                        vlan: Some(10),
                    },
                },
                RuntimeNic {
                    mac: "52:54:00:12:34:57".parse().unwrap(),
                    model: NicModel::E1000e,
                    backend: NicBackend::User,
                },
            ],
        }
    }

    fn mac_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            machine: MachineType::Virt,
            disks: vec![RuntimeDisk {
                path: "/tmp/opl/root.qcow2".into(),
                kind: DiskKind::File,
                rbd: None,
                format: ImageFormat::Qcow2,
                bus: DiskBus::VirtioBlk,
                boot_order: Some(1),
            }],
            nics: vec![RuntimeNic {
                mac: "52:54:00:12:34:56".parse().unwrap(),
                model: NicModel::Virtio,
                backend: NicBackend::User,
            }],
            ..x86_spec()
        }
    }

    #[test]
    fn x86_kvm_uefi_full() {
        let args = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn aarch64_hvf_minimal() {
        let args = build_cmdline(&mac_spec(), &mac_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn bios_omits_pflash() {
        let spec = VmRuntimeSpec {
            firmware: Firmware::Bios,
            ..x86_spec()
        };
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(!args.iter().any(|a| a.contains("pflash")));
    }

    #[test]
    fn host_cpu_falls_back_to_max_under_tcg() {
        let host = HostProfile {
            accel: Accel::Tcg,
            ..x86_host()
        };
        let args = build_cmdline(&x86_spec(), &host, &paths()).unwrap();
        let i = args.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(args[i + 1], "max");
        assert!(args.contains(&"q35,accel=tcg".to_string()));
    }

    #[test]
    fn escapes_commas_in_names_and_paths() {
        let mut spec = x86_spec();
        spec.name = "a,b".into();
        spec.disks[0].path = "/pool/x,y.qcow2".into();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"guest=a,,b,debug-threads=on".to_string()));
        assert!(
            args.contains(
                &"driver=file,filename=/pool/x,,y.qcow2,node-name=disk0-file".to_string()
            )
        );
    }

    /// 블록 장치 디스크는 `host_device`로 연다. 포맷 노드는 파일 디스크와 같다.
    #[test]
    fn block_disks_use_host_device() {
        let mut spec = x86_spec();
        spec.disks[1].path = "/dev/vg0/opl-a".into();
        spec.disks[1].kind = DiskKind::Block;
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=host_device,filename=/dev/vg0/opl-a,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        assert!(args.contains(
            &"driver=file,filename=/var/lib/opl/pools/local/web-01-root.qcow2,node-name=disk0-file"
                .to_string()
        ));
    }

    /// RBD 디스크는 아직 열지 않는다.
    #[test]
    fn rbd_disks_are_unsupported_for_now() {
        let mut spec = x86_spec();
        spec.disks[1].kind = DiskKind::Rbd;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::RbdDisksUnsupported
        );
    }

    #[test]
    fn rejects_incompatible_combinations() {
        let err = |spec: &VmRuntimeSpec, host: &HostProfile| {
            build_cmdline(spec, host, &paths()).unwrap_err()
        };
        assert_eq!(
            err(&x86_spec(), &mac_host()),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
        assert_eq!(
            err(
                &VmRuntimeSpec {
                    firmware: Firmware::Bios,
                    ..mac_spec()
                },
                &mac_host()
            ),
            CmdlineError::BiosOnAarch64
        );
        let mut sata_on_virt = mac_spec();
        sata_on_virt.disks[0].bus = DiskBus::Sata;
        assert_eq!(
            err(&sata_on_virt, &mac_host()),
            CmdlineError::SataRequiresQ35
        );
        let mut many = x86_spec();
        many.disks = (0..7)
            .map(|i| RuntimeDisk {
                path: format!("/d{i}.raw").into(),
                kind: DiskKind::File,
                rbd: None,
                format: ImageFormat::Raw,
                bus: DiskBus::Sata,
                boot_order: None,
            })
            .collect();
        assert_eq!(err(&many, &x86_host()), CmdlineError::TooManySataDisks);
    }
}
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
            kind: DiskKind::File,
            format: ImageFormat::Qcow2,
```

을 다음으로 바꾼다:

```rust
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS를 쓴다.
```

을 다음으로 바꾼다:

```rust
        }
        // RBD 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::Rbd { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "rbd pools are not supported yet",
            ));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS를 쓴다.
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
                }
                PoolBackend::Dir { .. } => {}
            }
```

을 다음으로 바꾼다:

```rust
                }
                PoolBackend::Dir { .. } | PoolBackend::Rbd { .. } => {}
            }
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            kind,
            format: volume.spec.format,
```

을 다음으로 바꾼다:

```rust
            kind,
            rbd: None,
            format: volume.spec.format,
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            kind: DiskKind::File,
            format: ImageFormat::Qcow2,
```

을 다음으로 바꾼다:

```rust
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "vg" && why.contains("macOS")),
        "{err:?}"
```

을 다음으로 바꾼다:

```rust
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "vg" && why.contains("macOS")),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// RBD 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[tokio::test]
async fn refuses_rbd_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let r: StoragePool = Resource::new(
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
    cluster.create(&r).await.unwrap();
    let mut root = volume("root", "r");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "r" && why == &"rbd pools are not supported yet"),
        "{err:?}"
```

`crates/opl-types/src/runtime.rs`에서

```rust
pub struct RuntimeDisk {
    pub path: PathBuf,
    /// `path`가 파일인지 블록 장치인지
    #[serde(default)]
    pub kind: DiskKind,
    pub format: ImageFormat,
```

을 다음으로 바꾼다:

```rust
pub struct RuntimeDisk {
    /// 파일·블록 디스크의 경로. RBD 디스크는 비어 있고 `rbd`를 쓴다.
    pub path: PathBuf,
    /// 디스크 소스가 파일인지, 블록 장치인지, RBD 이미지인지
    #[serde(default)]
    pub kind: DiskKind,
    /// RBD 디스크(`kind`가 `rbd`)의 이미지
    #[serde(default)]
    pub rbd: Option<RbdTarget>,
    pub format: ImageFormat,
```

`crates/opl-types/src/runtime.rs`에서

```rust

/// 디스크 소스의 종류. QEMU에 `driver=file` 또는 `driver=host_device`로 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
```

을 다음으로 바꾼다:

```rust

/// 디스크 소스의 종류. QEMU에 `driver=file`, `driver=host_device` 또는 `driver=rbd`로 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
```

`crates/opl-types/src/runtime.rs`에서

```rust
    Block,
}
```

을 다음으로 바꾼다:

```rust
    Block,
    /// Ceph RBD 이미지(rbd 풀). QEMU가 네트워크로 직접 연다.
    Rbd,
}

/// QEMU가 여는 RBD 이미지. 값은 풀을 만들 때 검증된 Ceph 이름과 conf 경로다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RbdTarget {
    pub pool: String,
    pub namespace: Option<String>,
    /// 이미지 이름(`opl-{id}`)
    pub image: String,
    /// Ceph 클라이언트 id(`client.` 없이)
    pub user: String,
    /// ceph.conf 경로. 없으면 Ceph 기본.
    pub conf: Option<String>,
}
```

`crates/opl-types/src/runtime.rs`에서

```rust
    }
}
```

을 다음으로 바꾼다:

```rust
    }

    #[test]
    fn rbd_disk_round_trips() {
        // rbd가 없는 옛 정의는 RBD 이미지가 없다.
        let disk: RuntimeDisk = serde_json::from_value(serde_json::json!({
            "path": "/dev/vg0/opl-a", "kind": "block", "format": "raw", "bus": "virtio-blk",
            "boot_order": null
        }))
        .unwrap();
        assert_eq!(disk.rbd, None);
        let rbd = RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "admin".into(),
                conf: None,
            }),
            ..disk
        };
        let json = serde_json::to_value(&rbd).unwrap();
        assert_eq!(json["kind"], "rbd");
        assert_eq!(json["rbd"]["image"], "opl-a");
        assert_eq!(serde_json::from_value::<RuntimeDisk>(json).unwrap(), rbd);
    }
}
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
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS·RBD 풀은 늘 공유다(`is_shared`).
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

/// 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD 풀은 `shared` 값과 상관없이 공유이고, LVM-thin·ZFS
/// 풀은 늘 노드 로컬이다.
pub fn is_shared(pool: &StoragePool) -> bool {
    match pool.spec.backend {
        PoolBackend::Dir { .. } => pool.spec.shared,
        PoolBackend::Nfs { .. } | PoolBackend::Rbd { .. } => true,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => false,
    }
}

/// 이 백엔드의 `format` 볼륨이 스냅샷을 담을 수 있는지. 파일 풀(dir, nfs)은 qcow2 내부 스냅샷,
/// LVM-thin 풀은 raw 볼륨의 thin 스냅샷, ZFS 풀은 raw zvol의 ZFS 스냅샷, RBD 풀은 raw 이미지의 RBD
/// 스냅샷이다.
pub fn pool_kind_supports_snapshots(backend: &PoolBackend, format: ImageFormat) -> bool {
    match backend {
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => format == ImageFormat::Qcow2,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            format == ImageFormat::Raw
        }
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
/// 데이터셋이 있다고, RBD 풀은 Ceph 풀에 닿는다고 알려야 한다.
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
const ZFS_UNSUPPORTED: &str = "zfs pools are not supported on macOS";
const LVS_TIMED_OUT: &str = "lvs timed out";
```

을 다음으로 바꾼다:

```rust
const ZFS_UNSUPPORTED: &str = "zfs pools are not supported on macOS";
const RBD_UNSUPPORTED: &str = "rbd pools are not supported yet";
const LVS_TIMED_OUT: &str = "lvs timed out";
```

`crates/opld/src/pools.rs`에서

```rust
                return Step::Zfs(dataset.clone());
            }
```

을 다음으로 바꾼다:

```rust
                return Step::Zfs(dataset.clone());
            }
            PoolBackend::Rbd { .. } => {
                return Step::State(not_ready(pool.id, RBD_UNSUPPORTED.into()));
            }
```

`crates/opld/src/pools.rs`에서

```rust
    }

    #[tokio::test]
    async fn refreshes_as_soon_as_a_pool_is_created() {
```

을 다음으로 바꾼다:

```rust
    }

    /// RBD 풀은 아직 확인하지 않고 준비되지 않았다고 적는다.
    #[tokio::test]
    async fn rbd_pools_are_not_ready_yet() {
        let mut s = setup().await;
        let backend = PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        };
        let r = s.pool("r", backend, vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["r"],
            not_ready(r.id, "rbd pools are not supported yet".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn refreshes_as_soon_as_a_pool_is_created() {
```

`web/src/storage/PoolsPage.tsx`에서

```tsx

type Kind = StoragePool["spec"]["backend"]["kind"];
```

을 다음으로 바꾼다:

```tsx

/** 만들 수 있는 풀 종류. RBD 풀 만들기는 아직 없다. */
type Kind = Exclude<StoragePool["spec"]["backend"]["kind"], "rbd">;
```

`web/src/storage/pools.ts`에서

```ts
 * 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL`,
 * ZFS 풀은 데이터셋
 */
```

을 다음으로 바꾼다:

```ts
 * 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL`,
 * ZFS 풀은 데이터셋, RBD 풀은 `POOL[/NAMESPACE]`
 */
```

`web/src/storage/pools.ts`에서

```ts
    return b.dataset;
  }
```

을 다음으로 바꾼다:

```ts
    return b.dataset;
  }
  if (b.kind === "rbd") {
    return b.namespace ? `${b.pool}/${b.namespace}` : b.pool;
  }
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 569개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `cargo test -p opl-api --test pools`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 286개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): Ceph RBD 풀 백엔드와 API 규칙"
```

---

### Task 2: RBD 디스크 소스

QEMU 명령줄이 `DiskKind::Rbd`면 `-blockdev driver=rbd,pool=..[,namespace=..],image=..,user=..[,conf=..]`(값의 `,`는 `,,`, 대상이 없으면 `MissingRbdImage`). `disk_location`은 `DiskLocation { Path(PathBuf, DiskKind), Rbd(RbdTarget) }`를 돌려주고 RBD면 `opl-{id}` 대상(사용자 기본 `admin`, raw만). 새 `storage/rbd.rs`에 `volume_target`. resolve가 RBD 디스크의 `RuntimeDisk.rbd`를 채운다.

**Files:**
- Create: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/tests/zfs_storage.rs`
- Modify: `crates/opl-qemu/src/cmdline.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/lib.rs`에서

```rust
pub use storage::{
    Capacity, DirStorage, LvInfo, LvmOps, LvmThinStorage, Storage, Storages, SystemLvm, SystemZfs,
    VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo,
    check_pool_root, disk_location, parse_lvs, parse_zfs_dataset, pool_root,
};
```

을 다음으로 바꾼다:

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, Storage, Storages,
    SystemLvm, SystemZfs, VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace,
    ZfsStorage, ZvolInfo, check_pool_root, disk_location, parse_lvs, parse_zfs_dataset, pool_root,
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

pub use lvm::{LvInfo, LvmOps, LvmThinStorage, SystemLvm, parse_lvs};
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
    /// 볼륨 파일 또는 블록 장치
    pub path: PathBuf,
    pub format: ImageFormat,
    /// 이미지 안의 내부 스냅샷(qcow2)에서 읽으면 그 이름(`qemu-img convert -l snapshot.name=`)
    pub snapshot: Option<String>,
    /// 읽을 내용의 가상 크기. 백엔드 사이 복사는 대상 볼륨을 이 크기로 만든다. LV 스냅샷이면 그
    /// 스냅샷 때의 크기다(뒤에 볼륨을 되돌려 지금이 더 작을 수 있다).
    pub size_bytes: u64,
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
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는
/// 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
    match &pool.spec.backend {
        PoolBackend::Dir { path } => path.clone(),
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
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
/// RBD 풀은 경로가 아니라 RBD 이미지 `opl-{id}`다(있는지는 `activate_volume`이 확인한다).
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
/// `force_share`면 실행 중인 VM이 쥔 이미지의 잠금을 무시하고 읽는다(`-U`).
fn convert_from(
    src: &VolumeSource,
    force_share: bool,
    out_format: ImageFormat,
) -> Result<Vec<String>, DriverError> {
    let mut args = vec!["-f".to_string(), fmt_name(src.format).to_string()];
    if force_share {
        args.push("-U".into());
    }
    // `-l`의 값은 옵션 문자열이므로 `,`를 `,,`로 쓴다.
    if let Some(name) = &src.snapshot {
        args.push("-l".into());
        args.push(format!("snapshot.name={}", name.replace(',', ",,")));
    }
    args.push("-O".into());
    args.push(fmt_name(out_format).into());
    args.push(path_str(&src.path)?.into());
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
        let from = convert_from(&src, force_share, dst_format)?;
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

    /// 실행 파일 셸 스크립트를 만든다. `"$last"`에 마지막 인자가 들어 있다.
    fn fake_tool(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        std::fs::write(&path, format!("#!/bin/sh\nfor last; do :; done\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
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

`crates/opl-driver/src/storage/rbd.rs`를 만든다:

```rust
//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음).

use opl_types::runtime::{ImageFormat, RbdTarget};
use uuid::Uuid;

use crate::error::DriverError;

/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
const DEFAULT_USER: &str = "admin";

/// 볼륨의 이미지 이름
fn volume_image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// QEMU가 여는 볼륨의 RBD 이미지. 사용자가 없으면 `admin`이다.
pub(super) fn volume_target(
    pool: &str,
    namespace: Option<&String>,
    user: Option<&String>,
    conf: Option<&String>,
    id: Uuid,
) -> RbdTarget {
    RbdTarget {
        pool: pool.to_string(),
        namespace: namespace.cloned(),
        image: volume_image(id),
        user: user.map_or(DEFAULT_USER, String::as_str).to_string(),
        conf: conf.cloned(),
    }
}

/// RBD 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "rbd pools hold raw volumes".into(),
        )),
    }
}
```

`crates/opl-driver/tests/zfs_storage.rs`에서

```rust

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage, Storages, SystemZfs, ZfsStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
```

을 다음으로 바꾼다:

```rust

use opl_driver::{
    DirStorage, DiskLocation, DriverError, SourceFormat, Storage, Storages, SystemZfs, ZfsStorage,
};
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
```

`crates/opl-driver/tests/zfs_storage.rs`에서

```rust
    assert!(qemu_io(&path, RAW, "write -P 0xab 0 1M"));
    let (location, _) = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, path);
    storage.activate_volume(&pool, id, RAW).await.unwrap();
```

을 다음으로 바꾼다:

```rust
    assert!(qemu_io(&path, RAW, "write -P 0xab 0 1M"));
    let location = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, DiskLocation::Path(path.clone(), DiskKind::Block));
    storage.activate_volume(&pool, id, RAW).await.unwrap();
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust

use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
```

을 다음으로 바꾼다:

```rust

use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RbdTarget, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, DiskBus, Firmware, MachineType, NicModel};
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
    TooManySataDisks,
    #[error("rbd disks are not supported yet")]
    RbdDisksUnsupported,
}
```

을 다음으로 바꾼다:

```rust
    TooManySataDisks,
    #[error("disk {0} is an rbd disk without an rbd image")]
    MissingRbdImage(usize),
}
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
        };
        let proto = match disk.kind {
            DiskKind::File => "file",
            DiskKind::Block => "host_device",
            DiskKind::Rbd => return Err(CmdlineError::RbdDisksUnsupported),
        };
        a.opt(
            "-blockdev",
            format!(
                "driver={proto},filename={},node-name=disk{i}-file",
                esc_path(&disk.path)
            ),
        );
        a.opt(
```

을 다음으로 바꾼다:

```rust
        };
        let source = match disk.kind {
            DiskKind::File => format!("driver=file,filename={}", esc_path(&disk.path)),
            DiskKind::Block => format!("driver=host_device,filename={}", esc_path(&disk.path)),
            DiskKind::Rbd => {
                let rbd = disk.rbd.as_ref().ok_or(CmdlineError::MissingRbdImage(i))?;
                rbd_source(rbd)
            }
        };
        a.opt("-blockdev", format!("{source},node-name=disk{i}-file"));
        a.opt(
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust
    Ok(a.0)
}
```

을 다음으로 바꾼다:

```rust
    Ok(a.0)
}

/// RBD 이미지를 여는 `rbd` 블록 드라이버 옵션. 네임스페이스와 conf는 있을 때만 넣는다.
fn rbd_source(rbd: &RbdTarget) -> String {
    let mut s = format!("driver=rbd,pool={}", esc(&rbd.pool));
    if let Some(ns) = &rbd.namespace {
        s.push_str(&format!(",namespace={}", esc(ns)));
    }
    s.push_str(&format!(
        ",image={},user={}",
        esc(&rbd.image),
        esc(&rbd.user)
    ));
    if let Some(conf) = &rbd.conf {
        s.push_str(&format!(",conf={}", esc(conf)));
    }
    s
}
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust

    use opl_types::runtime::{DiskKind, RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
```

을 다음으로 바꾼다:

```rust

    use opl_types::runtime::{DiskKind, RbdTarget, RuntimeDisk, RuntimeNic};
    use opl_types::vm::CpuSpec;
```

`crates/opl-qemu/src/cmdline.rs`에서

```rust

    /// RBD 디스크는 아직 열지 않는다.
    #[test]
    fn rbd_disks_are_unsupported_for_now() {
        let mut spec = x86_spec();
        spec.disks[1].kind = DiskKind::Rbd;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::RbdDisksUnsupported
        );
```

을 다음으로 바꾼다:

```rust

    /// RBD 디스크는 QEMU의 `rbd` 드라이버로 연다. 네임스페이스와 conf는 있을 때만 넘긴다.
    #[test]
    fn rbd_disks_use_rbd_driver() {
        let mut spec = x86_spec();
        spec.disks[0] = RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "opl".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            }),
            format: ImageFormat::Raw,
            ..spec.disks[0].clone()
        };
        spec.disks[1].path = PathBuf::new();
        spec.disks[1].kind = DiskKind::Rbd;
        spec.disks[1].rbd = Some(RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-b".into(),
            user: "admin".into(),
            conf: None,
        });
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=rbd,pool=rbd,namespace=opl,image=opl-a,user=opl,conf=/etc/ceph/opl.conf,node-name=disk0-file"
                .to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk0-file,node-name=disk0".to_string()));
        assert!(args.contains(
            &"driver=rbd,pool=rbd,image=opl-b,user=admin,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        // 이미지가 없는 RBD 디스크는 열 수 없다.
        spec.disks[1].rbd = None;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::MissingRbdImage(1)
        );
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust

use std::path::Path;

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Storage, disk_location};
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::node::NodeSpec;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{
```

을 다음으로 바꾼다:

```rust

use std::path::{Path, PathBuf};

use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DiskLocation, DriverError, Storage, disk_location};
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::node::NodeSpec;
use opl_types::runtime::{DiskKind, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
/// `NfsUnsupported`, LVM-thin·ZFS 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다. 저장소의 일시 오류는 `PoolUnavailable`, 그 밖은
/// `VolumeUnusable`이다.
/// (다시 해도 안 되는 것은 볼륨이 없거나 정의가 틀린 것뿐이고, 그 밖의 저장소 오류는 일시 오류로 본다.)
```

을 다음으로 바꾼다:

```rust
/// `NfsUnsupported`, LVM-thin·ZFS 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다(RBD 풀은 경로 대신 `RuntimeDisk.rbd`의 이미지).
/// 저장소의 일시 오류는 `PoolUnavailable`, 그 밖은 `VolumeUnusable`이다.
/// (다시 해도 안 되는 것은 볼륨이 없거나 정의가 틀린 것뿐이고, 그 밖의 저장소 오류는 일시 오류로 본다.)
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        // RBD 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::Rbd { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "rbd pools are not supported yet",
            ));
        }
```

을 다음으로 바꾼다:

```rust
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        };
        let (path, kind) = match located {
            Ok(location) => location,
            // 볼륨이 없거나 정의가 틀리면 다시 해도 같다.
```

을 다음으로 바꾼다:

```rust
        };
        let (path, kind, rbd) = match located {
            Ok(DiskLocation::Path(path, kind)) => (path, kind, None),
            Ok(DiskLocation::Rbd(target)) => (PathBuf::new(), DiskKind::Rbd, Some(target)),
            // 볼륨이 없거나 정의가 틀리면 다시 해도 같다.
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            kind,
            rbd: None,
            format: volume.spec.format,
```

을 다음으로 바꾼다:

```rust
            kind,
            rbd,
            format: volume.spec.format,
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RbdTarget, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// RBD 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[tokio::test]
async fn refuses_rbd_volumes_for_now() {
    let dir = TempDir::new().unwrap();
```

을 다음으로 바꾼다:

```rust

/// RBD 풀의 디스크는 볼륨을 활성화(이미지 확인)한 뒤 경로 대신 RBD 이미지로 푼다.
#[tokio::test]
async fn resolves_rbd_volumes_to_images() {
    let dir = TempDir::new().unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
                pool: "rbd".into(),
                namespace: None,
                user: None,
                conf: None,
            },
```

을 다음으로 바꾼다:

```rust
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                user: None,
                conf: Some("/etc/ceph/opl.conf".into()),
            },
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "r" && why == &"rbd pools are not supported yet"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}
```

을 다음으로 바꾼다:

```rust
    let storage = Activations::default();
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("r".to_string(), root.id, ImageFormat::Raw)]
    );
    assert_eq!(
        spec.disks,
        vec![RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: format!("opl-{}", root.id),
                user: "admin".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            }),
            format: ImageFormat::Raw,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }]
    );
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 570개, 실패 0

Run: `cargo test -p opl-qemu --lib cmdline`
Expected: PASS

Run: `cargo test -p opl-reconciler`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(qemu): RBD 디스크 소스 — rbd blockdev와 disk_location·resolve의 RBD 이미지"
```

---

### Task 3: Ceph RBD 이미지 드라이버

`storage/rbd.rs`: `RbdOps`(info/used/space/create/remove(purge 뒤 rm)/snapshots(시각 포함)/snapshot/rollback/remove_snapshot/resize/watchers/changing)와 실제 `SystemRbd`(`static RBD: BlockTool`, `--id U [--conf C]`, `LC_ALL=C`·`TZ=UTC`, 60초, purge·rm·rollback 600초), `RbdStorage::new(qemu_img, ops)`: 만들기(1 MiB 배수), 가져오기·복사(`qemu-img`의 image-opts `driver=raw,file.driver=rbd,...`), 지우기·되돌리기는 watcher가 있으면 일시 오류, 활성화는 진행 중 명령이 있으면 일시 오류, 정보(`rbd info` 시간 초과는 곧바로 Timeout)·용량(`ceph df detail`, 할당량 반영), 크기, 스냅샷, 아무 스냅샷으로 되돌리기, 원본(`file.snapshot=`). `DriverError::Rbd`, `VolumeSource.rbd`, `Storages::with_rbd`와 백엔드 사이 복사의 image-opts, `snapshot_tags_older_than`, opld 연결. 가짜 스크립트를 쓰는 테스트 도우미는 자식 `sh`로 쓴다(ETXTBSY). 가짜 `RbdOps`로 테스트한다.

**Files:**
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/Cargo.toml`에서

```toml
thiserror.workspace = true
tokio.workspace = true
```

을 다음으로 바꾼다:

```toml
thiserror.workspace = true
time.workspace = true
tokio.workspace = true
```

`crates/opl-driver/src/error.rs`에서

```rust
    Zfs(String),
    /// 블록 장치 볼륨의 장치 노드가 이 노드에 없다(활성화돼 있지 않거나 노드가 아직 생기지 않았다).
```

을 다음으로 바꾼다:

```rust
    Zfs(String),
    /// RBD 명령(`rbd`, `ceph`)이 실패했다. 내용은 stderr의 첫 줄이다.
    #[error("rbd failed: {0}")]
    Rbd(String),
    /// 블록 장치 볼륨의 장치 노드가 이 노드에 없다(활성화돼 있지 않거나 노드가 아직 생기지 않았다).
```

`crates/opl-driver/src/error.rs`에서

```rust
            | DriverError::Lvm(_)
            | DriverError::Zfs(_) => false,
        }
```

을 다음으로 바꾼다:

```rust
            | DriverError::Lvm(_)
            | DriverError::Zfs(_)
            | DriverError::Rbd(_) => false,
        }
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(!DriverError::Zfs("dataset does not exist".into()).is_transient());
    }
```

을 다음으로 바꾼다:

```rust
        assert!(!DriverError::Zfs("dataset does not exist".into()).is_transient());
        assert!(!DriverError::Rbd("rbd: error opening pool 'x'".into()).is_transient());
    }
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, Storage, Storages,
    SystemLvm, SystemZfs, VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace,
    ZfsStorage, ZvolInfo, check_pool_root, disk_location, parse_lvs, parse_zfs_dataset, pool_root,
};
```

을 다음으로 바꾼다:

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, RbdOps, RbdSnapshot,
    RbdSpace, RbdStorage, Storage, Storages, SystemLvm, SystemRbd, SystemZfs, VolumeInfo,
    VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root,
    disk_location, parse_lvs, parse_zfs_dataset, pool_root,
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

pub use lvm::{LvInfo, LvmOps, LvmThinStorage, SystemLvm, parse_lvs};
pub use rbd::{RbdOps, RbdSnapshot, RbdSpace, RbdStorage, SystemRbd};
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
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다). `rbd` 풀은 노드에 놓이는
/// 곳이 없으므로 빈 경로다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
    match &pool.spec.backend {
        PoolBackend::Dir { path } => path.clone(),
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
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
/// RBD 풀은 경로가 아니라 RBD 이미지 `opl-{id}`다(있는지는 `activate_volume`이 확인한다).
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

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            size_bytes: lv.size_bytes,
        })
```

을 다음으로 바꾼다:

```rust
            size_bytes: lv.size_bytes,
            rbd: None,
        })
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
                .await?;
            let from = convert_from(&source, force_share, dst_format)?;
            self.fill(vg, dst_id, &dst, &from).await
```

을 다음으로 바꾼다:

```rust
                .await?;
            let from = convert_from(&source, force_share, Some(dst_format))?;
            self.fill(vg, dst_id, &dst, &from).await
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = info ]; then echo '{{\"virtual-size\": 1000}}'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {args:?}\n{end}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
                size_bytes: 1 << 20,
            }
```

을 다음으로 바꾼다:

```rust
                size_bytes: 1 << 20,
                rbd: None,
            }
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
    async fn cancelled_copy_deactivates_the_source_snapshot() {
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (lvm, storage) = setup(slow);
```

을 다음으로 바꾼다:

```rust
    async fn cancelled_copy_deactivates_the_source_snapshot() {
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        crate::storage::tests::write_script(&slow, "#!/bin/sh\nsleep 5\n");
        let (lvm, storage) = setup(slow);
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음).

use opl_types::runtime::{ImageFormat, RbdTarget};
use uuid::Uuid;

use crate::error::DriverError;
```

을 다음으로 바꾼다:

```rust
//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음). 스냅샷은 RBD 스냅샷 `opl-{id}@{tag}`다.
//! RBD 명령은 [`RbdOps`] 뒤에 있다(테스트는 가짜).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use opl_types::runtime::{ImageFormat, RbdTarget};
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
const DEFAULT_USER: &str = "admin";
```

을 다음으로 바꾼다:

```rust
const DEFAULT_USER: &str = "admin";

/// RBD 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const RBD_TIMEOUT: Duration = Duration::from_secs(60);

/// 이미지 크기에 비례해 오래 걸리는 명령(지우기, 되돌리기)을 기다리는 시간
const RBD_LONG_TIMEOUT: Duration = Duration::from_secs(600);

/// 이미지 크기는 이 배수로 올린다(`rbd create --size`는 MiB 단위다).
const IMAGE_ALIGN: u64 = 1 << 20;

/// 이미지의 스냅샷 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbdSnapshot {
    /// `@` 뒤의 이름
    pub tag: String,
    /// 스냅샷 때의 이미지 크기
    pub size_bytes: u64,
    /// 만든 시각. 알 수 없으면 `None`.
    pub created: Option<SystemTime>,
}

/// Ceph 풀의 `ceph df`: 남은 공간(`max_avail`)과 담긴 데이터(`stored`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RbdSpace {
    pub available_bytes: u64,
    pub stored_bytes: u64,
}

/// RBD 명령. 이미지는 [`RbdTarget`](풀, 네임스페이스, 이미지, 클라이언트 id, ceph.conf)이다.
/// 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait RbdOps: Send + Sync {
    /// 이미지의 크기(`rbd info`). 이미지가 없으면 `None`.
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError>;
    /// 이미지가 쓰는 바이트(`rbd du`)
    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError>;
    /// Ceph 풀 `pool`의 공간(`ceph df detail`). `user`·`conf`로 접속한다.
    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError>;
    /// `size_bytes`(MiB 배수) 크기의 이미지를 만든다(`rbd create`).
    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`).
    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError>;
    /// 이미지의 스냅샷들. 만든 순서(오래된 것부터)다.
    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError>;
    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지를 스냅샷 `tag`로 되돌린다(`rbd snap rollback`). 더 새 스냅샷은 남는다.
    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지 크기를 `size_bytes`(MiB 배수)로 늘린다(`rbd resize`, 줄이지 않는다).
    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, image: &RbdTarget) -> bool;
    /// 이미지를 연 클라이언트(watcher) 수(`rbd status`)
    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError>;
}

/// 실제 RBD 명령(`rbd`, `ceph`). 노드에 ceph.conf와 키링이 있어야 한다.
pub struct SystemRbd;

/// 같은 이미지를 바꾸는 앞 명령이 아직 돌고 있을 때의 일시 오류(`Timeout`)에 담는 말
const RBD_BUSY: &str = "an earlier rbd command still running on the same image";

/// 이미지를 연 클라이언트(다른 노드의 QEMU 등)가 있어 지우거나 되돌릴 수 없을 때의 일시 오류에 담는 말
const RBD_WATCHED: &str = "clients to close the rbd image";

/// RBD 명령. 바꾸는 명령의 대상은 이미지 사양(`{pool}[/{namespace}]/{image}`)이다. `rbd snap ls`의
/// 시각(C `ctime()`, 지역 시간)이 UTC로 나오게 `TZ=UTC`로 돌린다.
static RBD: BlockTool = BlockTool::new(
    "rbd command",
    RBD_BUSY,
    &[("LC_ALL", "C"), ("TZ", "UTC")],
    DriverError::Rbd,
);

/// 이미지 사양 `{pool}[/{namespace}]/{image}`
fn image_spec(image: &RbdTarget) -> String {
    match &image.namespace {
        Some(ns) => format!("{}/{ns}/{}", image.pool, image.image),
        None => format!("{}/{}", image.pool, image.image),
    }
}

/// Ceph 명령(`rbd`, `ceph`)의 접속 인자 `--id U [--conf C]` 뒤에 `args`를 붙인다.
fn ceph_args<'a>(user: &'a str, conf: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--id", user];
    if let Some(conf) = conf {
        all.extend(["--conf", conf]);
    }
    all.extend_from_slice(args);
    all
}

/// 이미지 `image`에 대한 `rbd` 인자. `--no-progress`는 일부 하위 명령만 받으므로 붙이지 않는다
/// (진행률은 어차피 버리는 출력에 찍힌다).
fn rbd_args<'a>(image: &'a RbdTarget, args: &[&'a str]) -> Vec<&'a str> {
    ceph_args(&image.user, image.conf.as_deref(), args)
}

fn bad_output(what: impl std::fmt::Display) -> DriverError {
    DriverError::Rbd(format!("unparseable rbd output: {what}"))
}

fn json(out: &[u8]) -> Result<Value, DriverError> {
    serde_json::from_slice(out).map_err(bad_output)
}

/// `rbd info --format json`의 `size`
fn parse_info(out: &[u8]) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_output(format!("info {v}")))
}

/// `rbd du --format json`에서 이미지 `name`(스냅샷이 아닌 줄)의 `used_size`
fn parse_du(out: &[u8], name: &str) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| {
            i.get("name").and_then(Value::as_str) == Some(name) && i.get("snapshot").is_none()
        })
        .and_then(|i| i.get("used_size").and_then(Value::as_u64))
        .ok_or_else(|| bad_output(format!("du {v}")))
}

/// `rbd snap ls`의 `timestamp`: C `ctime()` 모양(`Thu Oct 29 09:00:00 2026`, 날이 한 자리면 공백이
/// 둘)의 UTC 시각(명령을 `TZ=UTC`로 돌린다). 읽을 수 없으면(빈 값 등) `None`.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, hms, year] = parts[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|m| *m == month)? as u8 + 1;
    let hms: Vec<u8> = hms
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [hour, minute, second] = hms[..] else {
        return None;
    };
    let date = time::Date::from_calendar_date(
        year.parse().ok()?,
        time::Month::try_from(month).ok()?,
        day.parse().ok()?,
    )
    .ok()?;
    let at = time::PrimitiveDateTime::new(date, time::Time::from_hms(hour, minute, second).ok()?);
    let secs = u64::try_from(at.assume_utc().unix_timestamp()).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// `rbd status --format json`의 watcher 수
fn parse_status(out: &[u8]) -> Result<usize, DriverError> {
    let v = json(out)?;
    v.get("watchers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| bad_output(format!("status {v}")))
}

/// `rbd rm`이 이미지를 연 클라이언트 때문에 실패했으면(stderr 첫 줄이 librbd의 "image has
/// watchers"이거나 rbd의 "image still has watchers") 일시 오류로 바꾼다.
fn watched(e: DriverError) -> DriverError {
    match e {
        DriverError::Rbd(msg) if msg.contains("has watchers") => DriverError::Timeout(RBD_WATCHED),
        e => e,
    }
}

/// `rbd info`의 결과(`read`)로 이미지 크기를 낸다. 실패했으면 풀의 이미지 목록(`images`)으로
/// 이미지가 없는지 확인한다(오류 메시지로는 풀이 없는 것과 이미지가 없는 것을 가리기 어렵다). 시간을
/// 넘겼으면 목록도 늦을 것이므로 곧바로 그 오류다.
async fn info_from(
    read: Result<Vec<u8>, DriverError>,
    name: &str,
    images: impl Future<Output = Result<Vec<String>, DriverError>>,
) -> Result<Option<u64>, DriverError> {
    match read {
        Ok(out) => parse_info(&out).map(Some),
        Err(e @ DriverError::Timeout(_)) => Err(e),
        Err(e) => match images.await {
            Ok(names) if !names.iter().any(|n| n == name) => Ok(None),
            _ => Err(e),
        },
    }
}

/// `rbd snap ls --format json`. 스냅샷 id 순(만든 순서)으로 돌려준다.
fn parse_snapshots(out: &[u8]) -> Result<Vec<RbdSnapshot>, DriverError> {
    let v = json(out)?;
    let items = v
        .as_array()
        .ok_or_else(|| bad_output(format!("snap ls {v}")))?;
    let mut snapshots = items
        .iter()
        .map(|s| {
            let field = |k: &str| s.get(k).ok_or_else(|| bad_output(format!("snapshot {s}")));
            let id = field("id")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let tag = field("name")?
                .as_str()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let size = field("size")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let created = s
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp);
            Ok((
                id,
                RbdSnapshot {
                    tag: tag.to_string(),
                    size_bytes: size,
                    created,
                },
            ))
        })
        .collect::<Result<Vec<_>, DriverError>>()?;
    snapshots.sort_by_key(|(id, _)| *id);
    Ok(snapshots.into_iter().map(|(_, s)| s).collect())
}

/// `ceph df detail --format json`에서 풀 `pool`의 `max_avail`·`stored`(없는 값은 0). 풀이 없으면 오류.
fn parse_ceph_df(out: &[u8], pool: &str) -> Result<RbdSpace, DriverError> {
    let v = json(out)?;
    let stats = v
        .get("pools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(pool))
        .and_then(|p| p.get("stats"))
        .ok_or_else(|| DriverError::Rbd(format!("ceph pool {pool} not found in ceph df")))?;
    let field = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
    // 풀에 바이트 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다.
    let (stored, quota) = (field("stored"), field("quota_bytes"));
    let mut available = field("max_avail");
    if quota > 0 {
        available = available.min(quota.saturating_sub(stored));
    }
    Ok(RbdSpace {
        available_bytes: available,
        stored_bytes: stored,
    })
}

impl SystemRbd {
    /// 이미지의 풀(네임스페이스)에 있는 이미지 이름들(`rbd ls`)
    async fn images(&self, image: &RbdTarget) -> Result<Vec<String>, DriverError> {
        let mut args = vec!["ls", "--format", "json", "--pool", &image.pool];
        if let Some(ns) = &image.namespace {
            args.extend(["--namespace", ns]);
        }
        let v = json(
            &RBD.run_read("rbd", &rbd_args(image, &args), RBD_TIMEOUT)
                .await?,
        )?;
        let names = v.as_array().ok_or_else(|| bad_output(format!("ls {v}")))?;
        Ok(names
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect())
    }

    async fn change(
        &self,
        image: &RbdTarget,
        args: &[&str],
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let spec = image_spec(image);
        RBD.run_change("rbd", &rbd_args(image, args), &[&spec], timeout)
            .await
    }
}

#[async_trait]
impl RbdOps for SystemRbd {
    /// `rbd info`가 실패하면 풀의 이미지 목록으로 이미지가 없는지 확인한다([`info_from`]).
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["info", "--format", "json", &spec]);
        let read = RBD.run_read("rbd", &args, RBD_TIMEOUT).await;
        info_from(read, &image.image, self.images(image)).await
    }

    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["du", "--format", "json", &spec]);
        parse_du(
            &RBD.run_read("rbd", &args, RBD_TIMEOUT).await?,
            &image.image,
        )
    }

    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError> {
        let args = ceph_args(user, conf, &["df", "detail", "--format", "json"]);
        parse_ceph_df(&RBD.run_read("ceph", &args, RBD_TIMEOUT).await?, pool)
    }

    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        let args = ["create", "--size", &size, "--image-format", "2", &spec];
        self.change(image, &args, RBD_TIMEOUT).await
    }

    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
        let spec = image_spec(image);
        self.change(image, &["snap", "purge", &spec], RBD_LONG_TIMEOUT)
            .await?;
        self.change(image, &["rm", &spec], RBD_LONG_TIMEOUT)
            .await
            .map_err(watched)
    }

    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["snap", "ls", "--format", "json", &spec]);
        parse_snapshots(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }

    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "create", &name], RBD_TIMEOUT)
            .await
    }

    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rollback", &name], RBD_LONG_TIMEOUT)
            .await
    }

    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rm", &name], RBD_TIMEOUT)
            .await
    }

    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        self.change(image, &["resize", "--size", &size, &spec], RBD_TIMEOUT)
            .await
    }

    fn changing(&self, image: &RbdTarget) -> bool {
        RBD.changing(&image_spec(image))
    }

    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["status", "--format", "json", &spec]);
        parse_status(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }
}

/// `rbd --size` 값: MiB 단위(`{n}M`)
fn mib_arg(size_bytes: u64) -> String {
    format!("{}M", size_bytes / IMAGE_ALIGN)
}
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust

/// RBD 풀은 raw 볼륨만 담는다.
```

을 다음으로 바꾼다:

```rust

/// RBD 풀이면 그 풀의 볼륨 이미지
pub(super) fn pool_target(pool: &StoragePool, id: Uuid) -> Option<RbdTarget> {
    match &pool.spec.backend {
        PoolBackend::Rbd {
            pool,
            namespace,
            user,
            conf,
        } => Some(volume_target(
            pool,
            namespace.as_ref(),
            user.as_ref(),
            conf.as_ref(),
            id,
        )),
        PoolBackend::Dir { .. }
        | PoolBackend::Nfs { .. }
        | PoolBackend::LvmThin { .. }
        | PoolBackend::Zfs { .. } => None,
    }
}

/// RBD 풀은 raw 볼륨만 담는다.
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
        )),
    }
}
```

을 다음으로 바꾼다:

```rust
        )),
    }
}

/// `qemu-img`가 raw RBD 이미지(`snapshot`이 있으면 그 스냅샷)를 열게 하는 옵션 문자열
/// (`--image-opts`/`--target-image-opts`). 사용자·conf·네임스페이스를 함께 넘긴다. 옵션 값 안의
/// `,`는 `,,`로 쓴다.
pub(super) fn image_opts(image: &RbdTarget, snapshot: Option<&str>) -> String {
    let esc = |v: &str| v.replace(',', ",,");
    let mut s = format!("driver=raw,file.driver=rbd,file.pool={}", esc(&image.pool));
    if let Some(ns) = &image.namespace {
        s.push_str(&format!(",file.namespace={}", esc(ns)));
    }
    s.push_str(&format!(",file.image={}", esc(&image.image)));
    if let Some(tag) = snapshot {
        s.push_str(&format!(",file.snapshot={}", esc(tag)));
    }
    s.push_str(&format!(",file.user={}", esc(&image.user)));
    if let Some(conf) = &image.conf {
        s.push_str(&format!(",file.conf={}", esc(conf)));
    }
    s
}

/// 이미지 크기: `IMAGE_ALIGN` 배수로 올린다.
fn image_size(size_bytes: u64) -> Result<u64, DriverError> {
    size_bytes
        .div_ceil(IMAGE_ALIGN)
        .checked_mul(IMAGE_ALIGN)
        .ok_or_else(|| DriverError::InvalidSpec(format!("volume size {size_bytes} is too large")))
}

/// Ceph RBD 풀(`rbd`)의 볼륨. 볼륨은 raw 이미지이고 스냅샷은 RBD 스냅샷이다. 이미지는 노드에
/// 놓이지 않으므로 볼륨의 경로는 빈 경로다.
pub struct RbdStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn RbdOps>,
}

impl RbdStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn RbdOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
        }
    }

    /// 풀의 볼륨 이미지. RBD 풀이 아니거나 raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, id: Uuid, format: ImageFormat) -> Result<RbdTarget, DriverError> {
        require_raw(format)?;
        pool_target(pool, id).ok_or_else(|| DriverError::InvalidSpec("not an rbd pool".into()))
    }

    /// 스냅샷 태그는 RBD 스냅샷 이름과 qemu-img 옵션에 그대로 쓸 수 있는 글자여야 한다.
    fn check_tag(tag: &str) -> Result<(), DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c));
        if ok {
            Ok(())
        } else {
            Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid rbd snapshot name"
            )))
        }
    }

    /// 있는 볼륨의 크기. 없으면 `VolumeNotFound`.
    async fn existing(&self, image: &RbdTarget, id: Uuid) -> Result<u64, DriverError> {
        self.ops
            .info(image)
            .await?
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨이 있고 스냅샷 `tag`도 있어야 한다. 그 스냅샷을 돌려준다.
    async fn existing_snapshot(
        &self,
        image: &RbdTarget,
        id: Uuid,
        tag: &str,
    ) -> Result<RbdSnapshot, DriverError> {
        self.existing(image, id).await?;
        self.ops
            .snapshots(image)
            .await?
            .into_iter()
            .find(|s| s.tag == tag)
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
            })
    }

    /// 이미지를 연 클라이언트(다른 노드에서 도는 VM의 QEMU 등)가 있으면 일시 오류다.
    async fn unwatched(&self, image: &RbdTarget) -> Result<(), DriverError> {
        if self.ops.watchers(image).await? > 0 {
            return Err(DriverError::Timeout(RBD_WATCHED));
        }
        Ok(())
    }

    /// 새 볼륨 이미지를 만든다. 크기는 1 MiB 배수로 올린다.
    async fn create_image(
        &self,
        image: &RbdTarget,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let size = image_size(size_bytes)?;
        if self.ops.info(image).await?.is_some() {
            return Err(DriverError::VolumeExists(id));
        }
        self.ops.create(image, size).await
    }

    /// 만든 이미지에 `qemu-img convert -n`으로 써 넣는다(`from`은 원본 인자, `-O` 없이). 새 이미지는
    /// 0으로 읽히므로 0인 부분은 쓰지 않는다(`--target-is-zero`). 실패하면 이미지를 지운다.
    async fn fill(&self, image: &RbdTarget, id: Uuid, from: &[String]) -> Result<(), DriverError> {
        let written = async {
            let dst = image_opts(image, None);
            let mut args = vec![
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
            ];
            args.extend(from.iter().map(String::as_str));
            args.push(&dst);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.ops.remove(image).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for RbdStorage {
    /// 이미지에는 경로가 없으므로 빈 경로를 돌려준다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.create_image(&image, id, size_bytes).await?;
        Ok(PathBuf::new())
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        self.create_image(&image, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), src_str].map(String::from);
        self.fill(&image, id, &from).await?;
        Ok(PathBuf::new())
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 이미지를 스냅샷과 함께 지운다. 이미지가 없어도 그 이미지를 바꾸는 명령이 아직 돌고 있으면
    /// (시간을 넘긴 만들기) 이미지가 곧 생길 수 있으므로 `VolumeNotFound`가 아니라 일시 오류다.
    /// 이미지를 연 클라이언트가 있으면 스냅샷도 지우지 않고 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.info(&image).await?.is_none() {
            if self.ops.changing(&image) {
                return Err(DriverError::Timeout(RBD_BUSY));
            }
            return Err(DriverError::VolumeNotFound(id));
        }
        self.unwatched(&image).await?;
        self.ops.remove(&image).await
    }

    /// QEMU가 이미지를 직접 연다. 이미지가 있는지만 본다. 그 이미지를 바꾸는 명령(시간을 넘긴
    /// 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 이미지를 연 클라이언트는 보지 않는다(다시 붙는
    /// VM의 QEMU가 그 클라이언트다).
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        self.existing(&image, id).await.map(drop)
    }

    /// 쓰는 바이트(`rbd du`)를 알 수 없으면 이미지 크기로 본다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size = self.existing(&image, id).await?;
        let used = match self.ops.used(&image).await {
            Ok(used) => used,
            Err(e) => {
                tracing::debug!(volume = %id, error = %e, "rbd du failed, using image size");
                size
            }
        };
        Ok(VolumeInfo {
            virtual_bytes: size,
            allocated_bytes: used,
        })
    }

    /// Ceph 풀의 크기(`max_avail`+`stored`)와 남은 공간(`max_avail`)
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let image = Self::target(pool, Uuid::nil(), ImageFormat::Raw)?;
        let space = self
            .ops
            .space(&image.pool, &image.user, image.conf.as_deref())
            .await?;
        Ok(Capacity {
            total_bytes: space.available_bytes.saturating_add(space.stored_bytes),
            available_bytes: space.available_bytes,
        })
    }

    /// 볼륨이 raw이므로 RBD 스냅샷으로 스냅샷을 둔다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Raw
    }

    /// VM이 실행 중이어도 만들 수 있다(일관성은 호출자가 VM을 잠시 멈춰 맞춘다). 여러 볼륨은
    /// 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없다).
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        self.existing(&image, id).await?;
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        self.ops.snapshot(&image, tag).await
    }

    /// 아무 스냅샷으로나 되돌린다(더 새 스냅샷은 남는다). 이미지를 연 클라이언트가 없어야 한다
    /// (있으면 일시 오류).
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing_snapshot(&image, id, tag).await?;
        self.unwatched(&image).await?;
        self.ops.rollback(&image, tag).await
    }

    /// VM이 실행 중이어도 지울 수 있다. 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 이미지를
    /// 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 곧 생길 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        if self.ops.info(&image).await?.is_none() {
            return Ok(());
        }
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            self.ops.remove_snapshot(&image, tag).await?;
        }
        Ok(())
    }

    /// 만든 순서다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing(&image, id).await?;
        let snapshots = self.ops.snapshots(&image).await?;
        Ok(snapshots.into_iter().map(|s| s.tag).collect())
    }

    /// VM이 실행 중이어도 늘릴 수 있다(그 뒤 QEMU에 새 크기를 알리는 것은 호출자의 일이다). 크기는
    /// 1 MiB 배수로 올린다. 이미 그 크기 이상이면(앞선 시도가 늘렸다) 아무것도 하지 않는다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.existing(&image, id).await? >= size_bytes {
            return Ok(());
        }
        self.ops.resize(&image, image_size(size_bytes)?).await
    }

    /// 이미지, 또는 그 스냅샷(읽기 전용). `qemu-img`가 `rbd` 드라이버로 연다. 내놓을 것은 없다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size_bytes = match tag {
            Some(tag) => self.existing_snapshot(&image, id, tag).await?.size_bytes,
            None => self.existing(&image, id).await?,
        };
        Ok(VolumeSource {
            path: PathBuf::new(),
            format,
            snapshot: tag.map(str::to_string),
            size_bytes,
            rbd: Some(image),
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

    /// 원본(스냅샷이면 그때)의 크기로 대상 이미지를 만들고 원본을 써 넣는다. Ceph 풀·사용자가
    /// 달라도 같다.
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
        let image = Self::target(dst_pool, dst_id, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        self.create_image(&image, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, None)?;
        self.fill(&image, dst_id, &from).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct FakeImage {
        size: u64,
        /// 만든 순서의 스냅샷(태그, 크기)
        snapshots: Vec<(String, u64)>,
    }

    /// Ceph 풀 `rbd`, `other`를 메모리에 두는 RBD. 이미지는 사양(`{pool}[/{ns}]/{image}`)으로
    /// 둔다. 바꾸는 호출을 적고, `fail`에 적은 호출은 실패한다.
    #[derive(Default)]
    struct FakeRbd {
        images: Mutex<BTreeMap<String, FakeImage>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        /// 바꾸는 명령이 아직 돌고 있다고 알리는 이미지 사양
        changing: Mutex<Vec<String>>,
        /// `rbd du`가 실패한다.
        no_du: Mutex<bool>,
        /// 연 클라이언트(watcher)가 있는 이미지 사양
        watched: Mutex<Vec<String>>,
    }

    impl FakeRbd {
        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Rbd(format!("injected: {call}")));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.images.lock().unwrap().keys().cloned().collect()
        }

        fn image(&self, spec: &str) -> Option<FakeImage> {
            self.images.lock().unwrap().get(spec).cloned()
        }

        fn missing(spec: &str) -> DriverError {
            DriverError::Rbd(format!(
                "rbd: error opening image {spec}: (2) No such file or directory"
            ))
        }

        fn known_pool(image: &RbdTarget) -> Result<(), DriverError> {
            if ["rbd", "other"].contains(&image.pool.as_str()) {
                Ok(())
            } else {
                Err(DriverError::Rbd(format!(
                    "rbd: error opening pool '{}': (2) No such file or directory",
                    image.pool
                )))
            }
        }
    }

    #[async_trait]
    impl RbdOps for FakeRbd {
        async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
            Self::known_pool(image)?;
            Ok(self.image(&image_spec(image)).map(|i| i.size))
        }

        async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
            if *self.no_du.lock().unwrap() {
                return Err(DriverError::Rbd("du failed".into()));
            }
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.size / 4)
        }

        async fn space(
            &self,
            pool: &str,
            user: &str,
            conf: Option<&str>,
        ) -> Result<RbdSpace, DriverError> {
            self.call(format!("df {pool} {user} {conf:?}"))?;
            Ok(RbdSpace {
                available_bytes: 6 << 30,
                stored_bytes: 2 << 30,
            })
        }

        async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("create {spec} {size_bytes}"))?;
            Self::known_pool(image)?;
            let mut images = self.images.lock().unwrap();
            if images.contains_key(&spec) {
                return Err(DriverError::Rbd(
                    "rbd: create error: (17) File exists".into(),
                ));
            }
            let i = FakeImage {
                size: size_bytes,
                snapshots: vec![],
            };
            images.insert(spec, i);
            Ok(())
        }

        async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("remove {spec}"))?;
            let removed = self.images.lock().unwrap().remove(&spec);
            removed.map(drop).ok_or_else(|| Self::missing(&spec))
        }

        async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.snapshots
                .iter()
                .map(|(tag, size)| RbdSnapshot {
                    tag: tag.clone(),
                    size_bytes: *size,
                    created: None,
                })
                .collect())
        }

        async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap create {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots.push((tag.into(), i.size));
            Ok(())
        }

        async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rollback {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            let (_, size) = i
                .snapshots
                .iter()
                .find(|(t, _)| t == tag)
                .cloned()
                .ok_or_else(|| Self::missing(&format!("{spec}@{tag}")))?;
            i.size = size;
            Ok(())
        }

        async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rm {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots.retain(|(t, _)| t != tag);
            Ok(())
        }

        async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("resize {spec} {size_bytes}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            if size_bytes < i.size {
                return Err(DriverError::Rbd("shrinking requires --allow-shrink".into()));
            }
            i.size = size_bytes;
            Ok(())
        }

        fn changing(&self, image: &RbdTarget) -> bool {
            let spec = image_spec(image);
            self.changing.lock().unwrap().contains(&spec)
        }

        async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
            let spec = image_spec(image);
            self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            let watched = self.watched.lock().unwrap();
            Ok(watched.iter().filter(|s| **s == spec).count())
        }
    }

    fn rbd_pool(pool: &str, namespace: Option<&str>) -> StoragePool {
        Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: pool.into(),
                    namespace: namespace.map(Into::into),
                    user: None,
                    conf: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    fn pool() -> StoragePool {
        rbd_pool("rbd", None)
    }

    fn spec(id: Uuid) -> String {
        format!("rbd/opl-{id}")
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

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeRbd>, RbdStorage) {
        let rbd = Arc::new(FakeRbd::default());
        let storage = RbdStorage::new(qemu_img, rbd.clone());
        (rbd, storage)
    }

    fn invalid(e: DriverError, want: &str) {
        match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains(want), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn not_found(e: DriverError, id: Uuid) {
        assert!(
            matches!(e, DriverError::VolumeNotFound(v) if v == id),
            "{e:?}"
        );
    }

    fn args(dir: &Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        args.lines().map(str::to_string).collect()
    }

    const RAW: ImageFormat = ImageFormat::Raw;
    const MIB: u64 = 1 << 20;

    /// 크기를 1 MiB 배수로 올린 이미지를 만든다. 이미지에는 경로가 없다.
    #[tokio::test]
    async fn creates_raw_images() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let path = storage.create_volume(&p, id, MIB + 1, RAW).await.unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {}", spec(id), 2 * MIB)]);

        let err = storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), MIB, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), u64::MAX, RAW)
                .await
                .unwrap_err(),
            "too large",
        );
        assert!(rbd.taken().is_empty());
        // 네임스페이스 안에 만든다. Ceph 풀이 없으면 RBD 오류다.
        let id = Uuid::now_v7();
        let ns = rbd_pool("rbd", Some("opl"));
        storage.create_volume(&ns, id, MIB, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("create rbd/opl/opl-{id} {MIB}")]);
        let err = storage
            .create_volume(&rbd_pool("none", None), id, MIB, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다(새 이미지는 0으로 읽힌다). 대상은
    /// 옵션 문자열이다. 실패하면 이미지를 지운다.
    #[tokio::test]
    async fn imports_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {MIB}", spec(id))]);
        let dst =
            format!("driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{id},file.user=admin");
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "-f",
                "vmdk",
                "/up/disk.vmdk",
                &dst,
            ]
        );
        invalid(
            storage
                .import_volume(&p, id, Path::new("up.vmdk"), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(id)),
                format!("remove {}", spec(id)),
            ]
        );
        assert!(rbd.names().is_empty());
    }

    /// 이미지를 스냅샷과 함께 지운다. 없으면 `VolumeNotFound`, 그 이미지를 바꾸는 명령이 아직
    /// 돌고 있으면(시간을 넘긴 만들기) 일시 오류다.
    #[tokio::test]
    async fn delete_removes_image_with_its_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
    }

    /// 이미지를 연 클라이언트(watcher, 다른 노드의 QEMU 등)가 있으면 스냅샷도 지우지 않고 일시
    /// 오류다. `rbd rm`이 watcher 때문에 실패해도 일시 오류다.
    #[tokio::test]
    async fn delete_waits_for_watchers() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["t"]);

        rbd.watched.lock().unwrap().clear();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);

        // `rbd rm`의 stderr 첫 줄(librbd 기록이나 rbd의 오류)
        for line in [
            "rbd: 2026-10-29T09:00:00.000+0000 7f0 -1 librbd::image::PreRemoveRequest: 0x1 \
             check_image_watchers: image has watchers - not removing",
            "rbd: rbd: error: image still has watchers",
        ] {
            let err = watched(DriverError::Rbd(line.into()));
            assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        }
        let err = watched(DriverError::Rbd("rbd: error: no such image".into()));
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 활성화는 이미지가 있는지만 본다. 정보는 크기와 쓰는 바이트(알 수 없으면 크기), 용량은 Ceph
    /// 풀의 `max_avail`+`stored`다.
    #[tokio::test]
    async fn reports_volume_usage_and_pool_capacity() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.activate_volume(&p, id, RAW).await.unwrap_err(), id);
        not_found(storage.volume_info(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, 4 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: MIB,
            }
        );
        *rbd.no_du.lock().unwrap() = true;
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: 4 * MIB,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        // 사용자·conf가 있으면 그것으로 접속한다.
        let custom: StoragePool = Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: "other".into(),
                    namespace: Some("ns".into()),
                    user: Some("opl".into()),
                    conf: Some("/etc/ceph/opl.conf".into()),
                },
                nodes: vec![],
                shared: true,
            },
        );
        storage.capacity(&custom).await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                "df rbd admin None",
                "df other opl Some(\"/etc/ceph/opl.conf\")"
            ]
        );
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
    }

    /// 늘리기는 1 MiB 배수로 올린다. 이미 크면 아무것도 하지 않는다.
    #[tokio::test]
    async fn resize_grows_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.resize_volume(&p, id, RAW, MIB).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, 2 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.resize_volume(&p, id, RAW, MIB).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        assert!(rbd.taken().is_empty());
        storage
            .resize_volume(&p, id, RAW, 2 * MIB + 1)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("resize {} {}", spec(id), 3 * MIB)]);
    }

    /// 스냅샷은 RBD 스냅샷이다. 같은 태그는 다시 만들지 않고, 태그는 만든 순서다. 아무 스냅샷으로나
    /// 되돌릴 수 있고 더 새 스냅샷은 남는다.
    #[tokio::test]
    async fn snapshots_are_rbd_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        storage.snapshot_many(&p, &[id], RAW, "b").await.unwrap();
        rbd.taken();
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        invalid(
            storage
                .snapshot_create(&p, id, RAW, "a,b")
                .await
                .unwrap_err(),
            "not a valid",
        );
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );

        // 가장 최근이 아닌 스냅샷으로도 되돌린다.
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.image(&spec(id)).unwrap().size, MIB);
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );
        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        // 없는 태그와 없는 볼륨은 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "x").await.unwrap();
        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        storage
            .snapshot_delete(&p, Uuid::now_v7(), RAW, "a")
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("snap rm {}@a", spec(id))]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["b"]);

        // 그 이미지를 바꾸는 명령이 아직 돌고 있으면 태그가 곧 생길 수 있다.
        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.snapshot_delete(&p, id, RAW, "c").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// 되돌리기는 이미지를 연 클라이언트가 없어야 한다(일시 오류). 활성화(VM 시작)는 그 이미지를
    /// 바꾸는 명령(시간을 넘긴 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 활성화는 watcher를 보지
    /// 않는다(다시 붙는 VM의 QEMU가 watcher다).
    #[tokio::test]
    async fn revert_and_activate_wait_for_the_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(rbd.taken().is_empty());
        storage.activate_volume(&p, id, RAW).await.unwrap();
        rbd.watched.lock().unwrap().clear();
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        // 만들기가 시간을 넘겨 아직 이미지가 없어도 같다.
        let new = Uuid::now_v7();
        rbd.changing.lock().unwrap().push(spec(new));
        let err = storage.activate_volume(&p, new, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// `rbd info`가 실패하면 이미지 목록으로 없는 것을 가린다. 시간을 넘겼으면 목록을 읽지 않는다.
    #[tokio::test]
    async fn info_falls_back_to_the_image_list_unless_timed_out() {
        let list = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            async move { Ok(names) }
        };
        let info = br#"{"size":1048576}"#.to_vec();
        assert_eq!(
            info_from(Ok(info), "opl-a", list(&[])).await.unwrap(),
            Some(MIB)
        );
        let failed = || Err(DriverError::Rbd("rbd: error opening image".into()));
        assert_eq!(
            info_from(failed(), "opl-a", list(&["opl-b"]))
                .await
                .unwrap(),
            None
        );
        let err = info_from(failed(), "opl-a", list(&["opl-a"]))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
        let unlisted = async { panic!("listed images after a timeout") };
        let err = info_from(Err(DriverError::Timeout("rbd command")), "opl-a", unlisted)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
    }

    /// 여러 볼륨은 하나씩 찍는다.
    #[tokio::test]
    async fn snapshot_many_takes_one_image_at_a_time() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, a, MIB, RAW).await.unwrap();
        storage.create_volume(&p, b, MIB, RAW).await.unwrap();
        rbd.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                format!("snap create {}@t", spec(a)),
                format!("snap create {}@t", spec(b)),
            ]
        );
    }

    /// 원본은 이미지 또는 그 스냅샷이고 크기는 그때의 것이다. 내놓을 것은 없다.
    #[tokio::test]
    async fn volume_source_is_the_image_or_its_snapshot() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.volume_source(&p, id, RAW, None).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        let image = pool_target(&p, id).unwrap();
        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: PathBuf::new(),
                format: RAW,
                snapshot: Some("t".into()),
                size_bytes: MIB,
                rbd: Some(image.clone()),
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!((source.snapshot, source.size_bytes), (None, 2 * MIB));
        assert_eq!(source.rbd, Some(image));
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );
        assert!(rbd.taken().is_empty());
    }

    /// 같은 백엔드 복사: 원본(스냅샷이면 그때)의 크기로 이미지를 만들고 원본을 써 넣는다. 원본과
    /// 대상 모두 옵션 문자열이다. 다른 Ceph 풀로도 된다. 실패하면 만든 이미지를 지운다.
    #[tokio::test]
    async fn copies_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, other) = (pool(), rbd_pool("other", Some("ns")));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        storage.resize_volume(&p, src, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("create other/ns/opl-{dst} {MIB}")]);
        let from = format!(
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{src},file.snapshot=t,file.user=admin"
        );
        let to = format!(
            "driver=raw,file.driver=rbd,file.pool=other,file.namespace=ns,file.image=opl-{dst},file.user=admin"
        );
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "--image-opts",
                "-U",
                &from,
                &to,
            ]
        );
        let err = storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == dst),
            "{err:?}"
        );

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        rbd.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(dst)),
                format!("remove {}", spec(dst)),
            ]
        );
        assert_eq!(rbd.names(), [spec(src)]);
    }

    /// 다른 풀 종류는 다루지 않는다.
    #[tokio::test]
    async fn refuses_other_pools() {
        let (_, storage) = setup("/nonexistent/qemu-img");
        let dir: StoragePool = Resource::new(
            "d",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pools/local".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        invalid(
            storage
                .create_volume(&dir, Uuid::now_v7(), MIB, RAW)
                .await
                .unwrap_err(),
            "not an rbd pool",
        );
        invalid(storage.capacity(&dir).await.unwrap_err(), "not an rbd pool");
    }

    /// 바꾸는 명령이 실패하면 그 오류를 그대로 돌려준다.
    #[tokio::test]
    async fn command_failures_are_rbd_errors() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        rbd.fail_on(format!("snap create {}@t", spec(id)));
        let err = storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert!(!err.is_transient());
    }

    /// `qemu-img`의 옵션 문자열은 네임스페이스·스냅샷·conf를 있을 때만 넣고 `,`를 `,,`로 쓴다.
    #[test]
    fn image_opts_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(
            image_opts(&image, None),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-a,file.user=admin"
        );
        image.namespace = Some("ns".into());
        image.user = "opl".into();
        image.conf = Some("/etc/ceph/a,b.conf".into());
        assert_eq!(
            image_opts(&image, Some("t")),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.namespace=ns,file.image=opl-a,\
             file.snapshot=t,file.user=opl,file.conf=/etc/ceph/a,,b.conf"
        );
    }

    /// `rbd`·`ceph` 인자: 접속(`--id`, `--conf`)을 앞에 두고 진행률을 찍지 않는다. 이미지 사양은
    /// 네임스페이스를 담는다.
    #[test]
    fn rbd_commands_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(image_spec(&image), "rbd/opl-a");
        assert_eq!(
            rbd_args(&image, &["info", "rbd/opl-a"]),
            ["--id", "admin", "info", "rbd/opl-a"]
        );
        image.namespace = Some("ns".into());
        image.conf = Some("/etc/ceph/opl.conf".into());
        assert_eq!(image_spec(&image), "rbd/ns/opl-a");
        assert_eq!(
            rbd_args(&image, &["rm", "rbd/ns/opl-a"]),
            [
                "--id",
                "admin",
                "--conf",
                "/etc/ceph/opl.conf",
                "rm",
                "rbd/ns/opl-a"
            ]
        );
        assert_eq!(
            ceph_args("opl", None, &["df", "detail"]),
            ["--id", "opl", "df", "detail"]
        );
        assert_eq!(mib_arg(3 * MIB), "3M");
    }

    /// RBD 명령은 C 로캘로 돌고, 실패는 `Rbd` 오류다.
    #[tokio::test]
    async fn rbd_commands_use_c_locale_and_rbd_errors() {
        let long = Duration::from_secs(10);
        let out = RBD
            .run_read("/bin/sh", &["-c", "echo $LC_ALL $TZ"], long)
            .await
            .unwrap();
        // `rbd snap ls`의 시각(C `ctime()`)이 UTC로 나온다.
        assert_eq!(out, b"C UTC\n");
        let err = RBD
            .run_read("/bin/sh", &["-c", "echo 'rbd: error' >&2; exit 2"], long)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m == "/bin/sh: rbd: error"),
            "{err:?}"
        );
    }

    #[test]
    fn parses_rbd_output() {
        assert_eq!(
            parse_info(br#"{"name":"opl-a","size":1073741824,"objects":256}"#).unwrap(),
            1 << 30
        );
        assert!(parse_info(b"{}").is_err());
        assert!(parse_info(b"not json").is_err());

        // 스냅샷 줄이 아닌 이미지 줄의 used_size
        let du = br#"{"images":[
            {"name":"opl-a","snapshot":"t","id":4,"provisioned_size":10,"used_size":7},
            {"name":"opl-a","id":"1","provisioned_size":10,"used_size":3},
            {"name":"opl-b","id":"2","provisioned_size":10,"used_size":9}
        ],"total_provisioned_size":20,"total_used_size":19}"#;
        assert_eq!(parse_du(du, "opl-a").unwrap(), 3);
        assert!(parse_du(du, "opl-c").is_err());

        // 스냅샷 id 순. 시각은 읽을 수 있을 때만 담는다.
        let snaps = br#"[
            {"id":9,"name":"b","size":2048,"protected":"false","timestamp":"x"},
            {"id":4,"name":"a","size":1024,"protected":"false","timestamp":"Thu Oct  1 09:00:05 2026"},
            {"id":5,"name":"c","size":1024,"protected":"false"}
        ]"#;
        assert_eq!(
            parse_snapshots(snaps).unwrap(),
            [
                RbdSnapshot {
                    tag: "a".into(),
                    size_bytes: 1024,
                    created: Some(UNIX_EPOCH + Duration::from_secs(1_790_845_205)),
                },
                RbdSnapshot {
                    tag: "c".into(),
                    size_bytes: 1024,
                    created: None,
                },
                RbdSnapshot {
                    tag: "b".into(),
                    size_bytes: 2048,
                    created: None,
                },
            ]
        );
        assert_eq!(parse_snapshots(b"[]").unwrap(), []);
        assert!(parse_snapshots(br#"[{"id":1}]"#).is_err());

        let df = br#"{"stats":{},"pools":[
            {"name":"other","id":1,"stats":{"stored":1,"max_avail":2}},
            {"name":"rbd","id":2,"stats":{"stored":300,"max_avail":700,"objects":3}},
            {"name":"empty","id":3,"stats":{}}
        ]}"#;
        assert_eq!(
            parse_ceph_df(df, "rbd").unwrap(),
            RbdSpace {
                available_bytes: 700,
                stored_bytes: 300,
            }
        );
        assert_eq!(
            parse_ceph_df(df, "empty").unwrap(),
            RbdSpace {
                available_bytes: 0,
                stored_bytes: 0,
            }
        );
        assert!(matches!(
            parse_ceph_df(df, "none").unwrap_err(),
            DriverError::Rbd(_)
        ));
        // 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 담긴 데이터를 뺀 것까지다.
        let df = br#"{"pools":[
            {"name":"q","stats":{"stored":300,"max_avail":700,"quota_bytes":500}},
            {"name":"full","stats":{"stored":600,"max_avail":700,"quota_bytes":500}},
            {"name":"wide","stats":{"stored":300,"max_avail":700,"quota_bytes":5000}},
            {"name":"none","stats":{"stored":300,"max_avail":700,"quota_bytes":0}}
        ]}"#;
        let available = |pool| parse_ceph_df(df, pool).unwrap().available_bytes;
        assert_eq!(available("q"), 200);
        assert_eq!(available("full"), 0);
        assert_eq!(available("wide"), 700);
        assert_eq!(available("none"), 700);
        assert_eq!(parse_ceph_df(df, "q").unwrap().stored_bytes, 300);

        // `rbd status`의 watcher 수
        let status = br#"{"watchers":[
            {"address":"10.0.0.1:0/1","client":4100,"cookie":1},
            {"address":"10.0.0.2:0/2","client":4200,"cookie":2}
        ]}"#;
        assert_eq!(parse_status(status).unwrap(), 2);
        assert_eq!(parse_status(br#"{"watchers":[]}"#).unwrap(), 0);
        assert!(parse_status(b"{}").is_err());
    }

    /// `ctime()` 모양의 UTC 시각. 읽을 수 없으면 `None`이다.
    #[test]
    fn parses_ctime_timestamps() {
        let at = |secs| Some(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(
            parse_timestamp("Thu Oct 29 09:00:00 2026"),
            at(1_793_264_400)
        );
        assert_eq!(parse_timestamp("Thu Jan  1 00:00:00 1970"), at(0));
        assert_eq!(parse_timestamp("Mon Feb 29 23:59:59 2027"), None);
        for bad in [
            "",
            "x",
            "Thu Oct 29 09:00 2026",
            "Thu Okt 29 09:00:00 2026",
            "Thu Oct 29 25:00:00 2026",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }
}
```

`crates/opl-driver/src/storage/storages.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
}

impl Kind {
    fn of(pool: &StoragePool) -> Self {
        match &pool.spec.backend {
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => Kind::File,
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
            PoolBackend::Zfs { .. } => Kind::Zfs,
            PoolBackend::Rbd { .. } => Kind::Rbd,
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
}

impl Storages {
    pub fn new(qemu_img: impl Into<PathBuf>, file: Arc<dyn Storage>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            file,
            lvm_thin: None,
            zfs: None,
            rbd: None,
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
        }
    }

    /// 대상 백엔드에 형식 `format`의 빈 볼륨을 만들고 `src`를 써 넣는다. 쓰다 실패하면 만든
    /// 볼륨을 지운다. 그사이 취소되면 볼륨이 남는다(호출자가 만든 볼륨으로 적어 두고 정리한다).
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
        let written = async {
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
    /// 풀은 RBD 저장소로 간다.
    #[tokio::test]
    async fn calls_go_to_the_pool_backend() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)))
        .with_rbd(Arc::new(Recorder::new("rbd", "".as_ref(), &log)));
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        for (p, name) in [
            (dir_pool(), "file"),
            (nfs_pool(), "file"),
            (lvm_pool(), "lvm"),
            (zfs_pool(), "zfs"),
            (rbd_pool(), "rbd"),
        ] {
            storages.create_volume(&p, id, 1, f).await.unwrap();
            let src = Path::new("/up/x.vmdk");
            storages
                .import_volume(&p, id, src, SourceFormat::Vmdk, f)
                .await
                .unwrap();
            storages.delete_volume(&p, id, f).await.unwrap();
            storages.activate_volume(&p, id, f).await.unwrap();
            storages.volume_info(&p, id, f).await.unwrap();
            storages.capacity(&p).await.unwrap();
            assert!(storages.supports_snapshots(&p, f));
            storages.snapshot_create(&p, id, f, "t").await.unwrap();
            storages.snapshot_many(&p, &[id], f, "t").await.unwrap();
            storages.snapshot_revert(&p, id, f, "t").await.unwrap();
            storages.snapshot_delete(&p, id, f, "t").await.unwrap();
            storages.snapshot_tags(&p, id, f).await.unwrap();
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
                "volume_info",
                "capacity",
                "supports_snapshots",
                "snapshot_create",
                "snapshot_many",
                "snapshot_revert",
                "snapshot_delete",
                "snapshot_tags",
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

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
            size_bytes,
        })
```

을 다음으로 바꾼다:

```rust
            size_bytes,
            rbd: None,
        })
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
        let dst = self.create_zvol(dataset, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, dst_format)?;
        self.fill(dataset, dst_id, &dst, &from).await
```

을 다음으로 바꾼다:

```rust
        let dst = self.create_zvol(dataset, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, Some(dst_format))?;
        self.fill(dataset, dst_id, &dst, &from).await
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = info ]; then echo '{{\"virtual-size\": 1000}}'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {args:?}\n{end}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
                size_bytes: MIB,
            }
```

을 다음으로 바꾼다:

```rust
                size_bytes: MIB,
                rbd: None,
            }
```

`crates/opld/src/member.rs`에서

```rust
use opl_driver::{
    DirStorage, Hypervisor, LvmThinStorage, QemuHypervisor, Storage, Storages, SystemLvm,
    SystemZfs, ZfsStorage,
};
```

을 다음으로 바꾼다:

```rust
use opl_driver::{
    DirStorage, Hypervisor, LvmThinStorage, QemuHypervisor, RbdStorage, Storage, Storages,
    SystemLvm, SystemRbd, SystemZfs, ZfsStorage,
};
```

`crates/opld/src/member.rs`에서

```rust
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs)),
        );
```

을 다음으로 바꾼다:

```rust
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd)),
        );
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 590개, 실패 0

Run: `cargo test -p opl-driver --lib storage::rbd`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(storage): Ceph RBD 이미지 드라이버"
```

---

### Task 4: 마운터의 RBD 상태와 resolve

`parse_ceph_df`, `RBD_DEFAULT_USER`, `MountOps::rbd_pool(pool, namespace, user, conf)`·`qemu_rbd()`, `SystemMount { qemu_binary }`, `PoolMounter::spawn(.., qemu_binary, ..)`. 마운터가 RBD 풀은 QEMU rbd 드라이버 확인("Unknown driver 'rbd'"면 없음, rbd 매개변수 오류면 있음, 그 밖은 확인 실패), `rbd ls`, `ceph df detail`로 상태·용량(10초). macOS는 `"rbd pools are not supported on macOS"`, resolve도 비Linux면 `PoolUnsupported`.

**Files:**
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/src/pools.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/lib.rs`에서

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, RbdOps, RbdSnapshot,
    RbdSpace, RbdStorage, Storage, Storages, SystemLvm, SystemRbd, SystemZfs, VolumeInfo,
    VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root,
    disk_location, parse_lvs, parse_zfs_dataset, pool_root,
};
```

을 다음으로 바꾼다:

```rust
pub use storage::{
    Capacity, DirStorage, DiskLocation, LvInfo, LvmOps, LvmThinStorage, RBD_DEFAULT_USER, RbdOps,
    RbdSnapshot, RbdSpace, RbdStorage, Storage, Storages, SystemLvm, SystemRbd, SystemZfs,
    VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo,
    check_pool_root, disk_location, parse_ceph_df, parse_lvs, parse_zfs_dataset, pool_root,
};
```

`crates/opl-driver/src/storage.rs`에서

```rust
pub use lvm::{LvInfo, LvmOps, LvmThinStorage, SystemLvm, parse_lvs};
pub use rbd::{RbdOps, RbdSnapshot, RbdSpace, RbdStorage, SystemRbd};
pub use storages::Storages;
```

을 다음으로 바꾼다:

```rust
pub use lvm::{LvInfo, LvmOps, LvmThinStorage, SystemLvm, parse_lvs};
pub use rbd::{
    DEFAULT_USER as RBD_DEFAULT_USER, RbdOps, RbdSnapshot, RbdSpace, RbdStorage, SystemRbd,
    parse_ceph_df,
};
pub use storages::Storages;
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
const DEFAULT_USER: &str = "admin";
```

을 다음으로 바꾼다:

```rust
/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
pub const DEFAULT_USER: &str = "admin";
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
/// `ceph df detail --format json`에서 풀 `pool`의 `max_avail`·`stored`(없는 값은 0). 풀이 없으면 오류.
fn parse_ceph_df(out: &[u8], pool: &str) -> Result<RbdSpace, DriverError> {
    let v = json(out)?;
```

을 다음으로 바꾼다:

```rust
/// `ceph df detail --format json`에서 풀 `pool`의 `max_avail`·`stored`(없는 값은 0). 풀이 없으면 오류.
pub fn parse_ceph_df(out: &[u8], pool: &str) -> Result<RbdSpace, DriverError> {
    let v = json(out)?;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS를 쓴다.
        if !cfg!(target_os = "linux") {
```

을 다음으로 바꾼다:

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS·RBD를 쓴다(Homebrew QEMU에는 rbd가 없다).
        if !cfg!(target_os = "linux") {
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
                }
                PoolBackend::Dir { .. } | PoolBackend::Rbd { .. } => {}
            }
```

을 다음으로 바꾼다:

```rust
                }
                PoolBackend::Rbd { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "rbd pools are not supported on macOS",
                    ));
                }
                PoolBackend::Dir { .. } => {}
            }
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RbdTarget, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, NicBackend, RuntimeDisk, RuntimeNic};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// RBD 풀의 디스크는 볼륨을 활성화(이미지 확인)한 뒤 경로 대신 RBD 이미지로 푼다.
#[tokio::test]
async fn resolves_rbd_volumes_to_images() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let r: StoragePool = Resource::new(
```

을 다음으로 바꾼다:

```rust

/// RBD 풀 `r`(Ceph 풀 `rbd`, 네임스페이스 `opl`)과 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn rbd_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let r: StoragePool = Resource::new(
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

/// QEMU에 rbd 드라이버가 없는 노드(macOS)에서는 RBD 풀의 디스크를 쓸 수 없다. 기다려도 풀리지
/// 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_rbd_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = rbd_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "r" && why == &"rbd pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 RBD 풀의 디스크를 볼륨을 활성화(이미지 확인)한 뒤 경로 대신 RBD 이미지로 푼다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn resolves_rbd_volumes_to_images() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = rbd_vm(&cluster, node).await;
    let storage = Activations::default();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
```

을 다음으로 바꾼다:

```rust
            kind: DiskKind::Rbd,
            rbd: Some(opl_types::runtime::RbdTarget {
                pool: "rbd".into(),
```

`crates/opld/src/member.rs`에서

```rust
        };
        let register = {
```

을 다음으로 바꾼다:

```rust
        };
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        let register = {
```

`crates/opld/src/member.rs`에서

```rust
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(cluster.clone(), mount_root.clone(), config.pools.refresh());
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

을 다음으로 바꾼다:

```rust
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(
            cluster.clone(),
            mount_root.clone(),
            qemu_binary,
            config.pools.refresh(),
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

`crates/opld/src/pools.rs` 전체:

```rust
//! 노드별 풀 상태. 이 노드가 쓸 수 있는 풀마다 준비 여부와 용량을 `NodeStatus.pools`에 적는다.
//! NFS 풀은 `{data_dir}/pools/{풀 id}`에 마운트하고, 이 노드가 쓸 수 없게 된 NFS 풀은 언마운트한다.
//! LVM-thin 풀은 `lvs`로 thin pool을, ZFS 풀은 `zfs get`으로 데이터셋을, RBD 풀은 `rbd ls`와
//! `ceph df`로 Ceph 풀을(그 전에 QEMU의 rbd 블록 드라이버를) 확인한다.
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
use opl_driver::{Capacity, LvInfo, RbdSpace, ZfsDataset};
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Kind;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
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
const QEMU_NO_RBD: &str = "qemu has no rbd block driver";
const LVS_TIMED_OUT: &str = "lvs timed out";
const ZFS_TIMED_OUT: &str = "zfs timed out";
const RBD_TIMED_OUT: &str = "rbd timed out";
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
}

/// QEMU rbd 드라이버 확인에 여는 드라이브(풀·이미지가 빠졌다)
const RBD_PROBE_DRIVE: &str = "driver=rbd,if=none";

/// QEMU rbd 드라이버 확인의 stderr를 읽는다. `Unknown driver 'rbd'`면 드라이버가 없다. rbd 드라이버가
/// 빠진 인자를 알렸으면(QEMU 8.2: `Parameter 'pool' is missing`) 있다. 그 밖의 결과는 확인하지 못한
/// 것이다(있다고 기억하지 않으므로 다음 새로 고침에서 다시 확인한다). 오류 줄 앞에 QEMU가 되풀이하는
/// 드라이브 인자(`driver=rbd,...`)는 빼고 본다.
fn qemu_rbd_result(stderr: &str) -> Result<(), String> {
    if stderr.contains("Unknown driver 'rbd'") {
        return Err(QEMU_NO_RBD.into());
    }
    let reason = stderr.replace(RBD_PROBE_DRIVE, "");
    if reason.contains("pool") || reason.contains("image") {
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
        let mut states = BTreeMap::new();
        let mut checks = Vec::new();
        let mut block_checks = Vec::new();
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
Expected: PASS 594개, 실패 0

Run: `cargo test -p opld --bin opld pools`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): RBD 풀 상태 — rbd ls·ceph df와 QEMU rbd 드라이버 확인, macOS 거절"
```

---

### Task 5: RBD 스냅샷 규칙과 공유 풀의 Task

RBD는 아무 스냅샷으로나 되돌린다(최신 제한은 ZFS만), 메모리·섞인 블록 방식은 409. 공유 RBD 볼륨의 Task(만들기·지우기·VM이 쓰지 않는 크기 늘리기)는 쓸 수 있는 온라인 노드 중 id가 가장 작은 노드로. 남은 블록 스냅샷 태그 정리는 시작할 때와 `tag_sweep`(30분)마다 돌고, 공유 풀은 그 노드가 맡으며 만든 지 30분이 넘은 태그만 지운다.

**Files:**
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/scheduler.rs`에서

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &shared), Ok(mid.id));
        // 상태를 알리지 않은 노드(옛 노드, 첫 새로 고침 전)는 NFS 풀에는 후보가 아니고, 디렉터리
```

을 다음으로 바꾼다:

```rust
        assert_eq!(schedule(Uuid::now_v7(), &me, &shared), Ok(mid.id));
        // RBD 풀은 `shared` 값과 상관없이 공유다. 볼륨을 만든 노드에 묶이지 않는다.
        let mut ceph = pool("ceph", false);
        ceph.spec.backend = PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        };
        let rbd = inv(
            ceph,
            volume("root", "ceph", Some(small.id)),
            [false, true, true],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &rbd), Ok(mid.id));
        // 상태를 알리지 않은 노드(옛 노드, 첫 새로 고침 전)는 NFS 풀에는 후보가 아니고, 디렉터리
```

`crates/opl-api/src/snapshots.rs`에서

```rust

/// 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS)인지. 아니면 qcow2 내부 스냅샷이다.
pub(crate) fn uses_block_snapshots(pool: &StoragePool) -> bool {
```

을 다음으로 바꾼다:

```rust

/// 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS, RBD)인지. 아니면 qcow2 내부 스냅샷이다.
pub(crate) fn uses_block_snapshots(pool: &StoragePool) -> bool {
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
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::volumes::usable_node;

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
    /// 블록 스냅샷이 남긴 태그를 다시 찾아 지우는 간격(시작할 때 한 번 찾고 그 뒤로)
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
/// 본다. 블록 볼륨(LVM-thin, ZFS)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에
/// 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
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
        Some((vm, _)) if matches!(vm.status.phase, VmPhase::Starting | VmPhase::Stopping) => {
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
            if uses_block_snapshots(&pool) {
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
/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    usable_node(&state.cluster, pool, Some(state.node))?.ok_or_else(|| {
        ApiError::conflict(
```

을 다음으로 바꾼다:

```rust
/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. RBD 풀은 이 노드를 앞세우지 않고 늘 id가 가장 작은
/// 노드다: 이미지를 바꾸는 명령이 아직 돌고 있다는 표시가 노드마다 따로이므로 한 볼륨의 Task가 같은
/// 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let rbd = matches!(pool.spec.backend, PoolBackend::Rbd { .. });
    let prefer = (!rbd).then_some(state.node);
    usable_node(&state.cluster, pool, prefer)?.ok_or_else(|| {
        ApiError::conflict(
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    busy_deletes: std::sync::Mutex<std::collections::HashMap<Uuid, usize>>,
}
```

을 다음으로 바꾼다:

```rust
    busy_deletes: std::sync::Mutex<std::collections::HashMap<Uuid, usize>>,
    /// 막 만든 스냅샷 태그. `snapshot_tags_older_than`이 고르지 않는다.
    fresh_tags: std::sync::Mutex<Vec<String>>,
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            .unwrap_or_default()
    }
```

을 다음으로 바꾼다:

```rust
            .unwrap_or_default()
    }

    /// 태그 `tag`를 막 만든 것으로 둔다(`snapshot_tags_older_than`이 고르지 않는다).
    pub fn mark_fresh(&self, tag: &str) {
        self.fresh_tags.lock().unwrap().push(tag.into());
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    }
    async fn resize_volume(
```

을 다음으로 바꾼다:

```rust
    }
    async fn snapshot_tags_older_than(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        _: Duration,
    ) -> Result<Vec<String>, DriverError> {
        let fresh = self.fresh_tags.lock().unwrap().clone();
        Ok(self
            .tags(id)
            .into_iter()
            .filter(|t| !fresh.contains(t))
            .collect())
    }
    async fn resize_volume(
```

`crates/opl-api/tests/pools.rs`에서

```rust

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

을 다음으로 바꾼다:

```rust

/// RBD 풀의 볼륨 Task(만들기, 지우기, 쓰는 VM이 없는 크기 늘리기)는 이 노드에서 풀이 준비됐어도 풀이
/// 준비된 온라인 노드 가운데 id가 가장 작은 노드가 맡는다. 바꾸는 명령이 아직 돌고 있다는 표시가
/// 노드마다 따로이므로 한 볼륨의 Task가 늘 같은 노드에서 돌게 한다.
#[tokio::test]
async fn rbd_volume_tasks_go_to_the_lowest_ready_node() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let rbd = json!({ "kind": "rbd", "pool": "rbd" });
    let (status, body) = admin.post("/storage-pools", pool("ceph", rbd)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("ceph").await;
    let mut low: Node = Resource::new(
        "low",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    low.id = Uuid::nil();
    env.cluster.create(&low).await.unwrap();
    low.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(low.id, &low.status)
        .await
        .unwrap();
    common::mark_pool_ready(&env.cluster, low.id, "ceph").await;
    let low = low.id.to_string();

    let body = json!({
        "name": "r",
        "spec": { "pool": "ceph", "size_bytes": 1 << 20, "format": "raw" }
    });
    let (status, task) = admin.post("/volumes", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
    let mut v = env.cluster.get_by_name::<VolumeSpec>("r").unwrap().unwrap();
    v.status.phase = VolumePhase::Ready;
    v.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(v.id, &v.status)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/volumes/r/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
    let (status, task) = admin.delete("/volumes/r").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], low, "{task}");
}

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
//! 스냅샷 Task 실행: 실행 중인 VM(Mock 하이퍼바이저)의 만들기·삭제, 부모와 현재 위치, 실패 정리,
//! 디스크 없는 VM 되돌리기, 시작할 때의 고아 정리, 실행 중인 VM의 블록(LVM, ZFS) 스냅샷과 크기
//! 늘리기. 디스크 파일을 다루는 경우는 `snapshot_disks`.
```

을 다음으로 바꾼다:

```rust
//! 스냅샷 Task 실행: 실행 중인 VM(Mock 하이퍼바이저)의 만들기·삭제, 부모와 현재 위치, 실패 정리,
//! 디스크 없는 VM 되돌리기, 시작할 때의 고아 정리, 실행 중인 VM의 블록(LVM, ZFS, RBD) 스냅샷과 크기
//! 늘리기. 디스크 파일을 다루는 경우는 `snapshot_disks`.
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

/// 블록 풀들(이름, 백엔드, 볼륨 이름)과 각 풀의 바로 Ready인 1 MiB raw 볼륨을 저장소에 직접 만들고,
```

을 다음으로 바꾼다:

```rust

fn rbd(pool: &str) -> PoolBackend {
    PoolBackend::Rbd {
        pool: pool.into(),
        namespace: None,
        user: None,
        conf: None,
    }
}

/// 볼륨을 `node`가 만든 것으로 기록한다.
async fn set_volume_node(env: &Env, id: Uuid, node: Uuid) {
    let mut volume = env.cluster.get::<VolumeSpec>(id).unwrap().unwrap();
    volume.status.node = Some(node);
    env.cluster
        .update_status::<VolumeSpec>(id, &volume.status)
        .await
        .unwrap();
}

/// 블록 풀들(이름, 백엔드, 볼륨 이름)과 각 풀의 바로 Ready인 1 MiB raw 볼륨을 저장소에 직접 만들고,
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

/// 실행 중인 VM의 ZFS 스냅샷: VM을 멈추고 같은 풀의 디스크를 한 번에 찍는다. 풀이 다르면 풀마다
```

을 다음으로 바꾼다:

```rust

/// 공유 블록 풀(RBD)의 볼륨은 어느 노드에서나 보이므로, 시작할 때 남은 태그는 볼륨을 만든 노드가
/// 아니라 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가 지운다.
#[tokio::test]
async fn shared_block_pool_tags_are_removed_by_one_usable_node_at_startup() {
    let mut env = Env::start().await;
    let (vols, storage) = block_volumes(
        &mut env,
        &[
            ("ceph", rbd("rbd"), &["r0"]),
            ("thin", lvm("vg0"), &["lv0"]),
        ],
    )
    .await;
    let orphan = Uuid::now_v7().to_string();
    // 다른 노드(지금은 클러스터에 없다)가 만든 RBD 볼륨도 이 노드가 지운다.
    set_volume_node(&env, vols[0], Uuid::now_v7()).await;
    storage.set_tags(vols[0], &[&orphan]);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    wait_untagged(&storage, vols[0]).await;

    // id가 더 작은 온라인 노드도 그 풀을 쓸 수 있으면 그 노드에 맡긴다. 뒤에 만든 이 노드의 LVM
    // 볼륨이 지워지면 RBD 볼륨은 이미 지나갔다.
    let mut low: opl_types::node::Node = Resource::new(
        "low",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    low.id = Uuid::nil();
    env.cluster.create(&low).await.unwrap();
    low.status.online = true;
    env.cluster
        .update_status::<NodeSpec>(low.id, &low.status)
        .await
        .unwrap();
    common::mark_pool_ready(&env.cluster, low.id, "ceph").await;
    storage.set_tags(vols[0], &[&orphan]);
    storage.set_tags(vols[1], &[&orphan]);
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    wait_untagged(&storage, vols[1]).await;
    assert_eq!(storage.tags(vols[0]), [orphan.as_str()]);
}

/// 남은 태그 정리는 시작할 때뿐 아니라 `tag_sweep`마다 돈다. 공유 풀(RBD)에서는 만든 지 오래된
/// 태그만 지운다(다른 노드의 Task가 막 찍은 태그일 수 있다). 노드 로컬 풀은 나이를 보지 않는다.
#[tokio::test]
async fn block_tags_are_swept_periodically_sparing_new_shared_tags() {
    let mut env = Env::start().await;
    let (vols, storage) = block_volumes(
        &mut env,
        &[
            ("ceph", rbd("rbd"), &["r0"]),
            ("thin", lvm("vg0"), &["lv0"]),
        ],
    )
    .await;
    let sweep = std::time::Duration::from_millis(100);
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| c.tag_sweep = sweep)
        .await;
    // 실행기가 시작한 뒤에 남은 태그
    tokio::time::sleep(sweep).await;
    let (old, new) = (Uuid::now_v7().to_string(), Uuid::now_v7().to_string());
    storage.mark_fresh(&new);
    storage.set_tags(vols[0], &[&old, &new]);
    storage.set_tags(vols[1], &[&new]);
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.tags(vols[0]).len() != 1 || !storage.tags(vols[1]).is_empty() {
        assert!(std::time::Instant::now() < deadline, "tags were kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(sweep * 3).await;
    assert_eq!(storage.tags(vols[0]), [new.as_str()]);
}

/// RBD 디스크: 실행 중인 VM을 멈추고 찍으며 메모리는 담지 못한다. 다른 노드가 만든 볼륨도 공유
/// 풀이라 이 노드에서 찍고, 가장 최근이 아닌 스냅샷으로도(디스크에 더 최근 스냅샷이 있어도)
/// 되돌린다.
#[tokio::test]
async fn rbd_disks_snapshot_anywhere_and_revert_to_any_snapshot() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (vols, storage) = block_volumes(&mut env, &[("ceph", rbd("rbd"), &["r0", "r1"])]).await;
    set_volume_node(&env, vols[1], Uuid::now_v7()).await;
    let vm = running_block_vm(&env, &admin, "web", &["r0", "r1"]).await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert!(env.hv.is_paused(vm));
    storage.resume();
    succeeded(&admin.wait_task(&task).await);
    assert!(!env.hv.is_paused(vm));
    assert_eq!(storage.snapshot_groups(), [vec![vols[0], vols[1]]]);
    assert!(env.hv.snapshots(vm).is_empty());
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("rbd disks"),
        "{body}"
    );

    succeeded(&snapshot(&admin, "web", json!({ "name": "b" })).await);
    let (a, b) = (
        get(&env, vm, "a").id.to_string(),
        get(&env, vm, "b").id.to_string(),
    );
    storage.set_tags(vols[1], &[&a, &b, "backup"]);
    let (status, task) = admin.post("/vms/web/snapshots/a/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let reverts: Vec<_> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "revert")
        .collect();
    assert_eq!(reverts, [("revert", vols[0]), ("revert", vols[1])]);
    assert_eq!(storage.tags(vols[1]), [a.as_str(), b.as_str(), "backup"]);
}

/// 실행 중인 VM의 ZFS 스냅샷: VM을 멈추고 같은 풀의 디스크를 한 번에 찍는다. 풀이 다르면 풀마다
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
}

#[tokio::test]
async fn lvm_disks_take_disk_snapshots_of_one_method() {
```

을 다음으로 바꾼다:

```rust
}

/// RBD 풀 `ceph`와 그 풀의 바로 Ready인 raw 볼륨 `name`을 저장소에 직접 만든다.
async fn rbd_volume(env: &Env, name: &str) -> Uuid {
    let backend = PoolBackend::Rbd {
        pool: "rbd".into(),
        namespace: None,
        user: None,
        conf: None,
    };
    block_volume(env, "ceph", backend, name, ImageFormat::Raw).await;
    env.cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap()
        .id
}

#[tokio::test]
async fn lvm_disks_take_disk_snapshots_of_one_method() {
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
        assert_eq!(body["code"], "snapshot_unsupported");
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(body["code"], "snapshot_unsupported");
    }
}

/// RBD 디스크는 자기 종류의 블록 스냅샷을 쓰고(메모리 없이, 다른 블록 종류와 섞지 않는다) 어느
/// 스냅샷으로나 되돌린다. 공유 풀이라 배치된 적 없는 VM의 Task는 볼륨을 만든 노드가 아니라 풀을
/// 쓸 수 있는 노드에서 한다.
#[tokio::test]
async fn rbd_disks_revert_to_any_snapshot_and_run_where_the_pool_is_usable() {
    let env = env().await;
    let admin = env.admin().await;
    let r0 = rbd_volume(&env, "r0").await;
    rbd_volume(&env, "r1").await;
    zfs_volume(&env, "z0").await;
    let mut volume = env.cluster.get::<VolumeSpec>(r0).unwrap().unwrap();
    volume.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(r0, &volume.status)
        .await
        .unwrap();

    let web = vm_with_disks(&admin, "web", &["r0"]).await;
    for name in ["s1", "s2"] {
        let (status, task) = admin
            .post("/vms/web/snapshots", json!({ "name": name }))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(task["spec"]["node"], env.node.to_string());
        finish_tasks(&env).await;
    }
    mark_ready_at(&env, snapshot_of(&env, web, "s1").await.id, 10).await;
    mark_ready_at(&env, snapshot_of(&env, web, "s2").await.id, 20).await;
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;

    // 실행 중이어도 메모리는 담지 못한다.
    set_vm_status(&env, web, VmPhase::Running, Some(env.node)).await;
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("rbd disks"),
        "{body}"
    );

    // 블록 스냅샷이어도 RBD와 ZFS는 섞지 않는다.
    vm_with_disks(&admin, "mixed", &["r1", "z0"]).await;
    let (status, body) = admin
        .post("/vms/mixed/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("different snapshot methods"),
        "{body}"
    );
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    ) -> Result<Vec<String>, DriverError>;
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage/rbd.rs` 전체:

```rust
//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음). 스냅샷은 RBD 스냅샷 `opl-{id}@{tag}`다.
//! RBD 명령은 [`RbdOps`] 뒤에 있다(테스트는 가짜).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use opl_types::runtime::{ImageFormat, RbdTarget};
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
pub const DEFAULT_USER: &str = "admin";

/// RBD 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const RBD_TIMEOUT: Duration = Duration::from_secs(60);

/// 이미지 크기에 비례해 오래 걸리는 명령(지우기, 되돌리기)을 기다리는 시간
const RBD_LONG_TIMEOUT: Duration = Duration::from_secs(600);

/// 이미지 크기는 이 배수로 올린다(`rbd create --size`는 MiB 단위다).
const IMAGE_ALIGN: u64 = 1 << 20;

/// 이미지의 스냅샷 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbdSnapshot {
    /// `@` 뒤의 이름
    pub tag: String,
    /// 스냅샷 때의 이미지 크기
    pub size_bytes: u64,
    /// 만든 시각. 알 수 없으면 `None`.
    pub created: Option<SystemTime>,
}

/// Ceph 풀의 `ceph df`: 남은 공간(`max_avail`)과 담긴 데이터(`stored`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RbdSpace {
    pub available_bytes: u64,
    pub stored_bytes: u64,
}

/// RBD 명령. 이미지는 [`RbdTarget`](풀, 네임스페이스, 이미지, 클라이언트 id, ceph.conf)이다.
/// 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait RbdOps: Send + Sync {
    /// 이미지의 크기(`rbd info`). 이미지가 없으면 `None`.
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError>;
    /// 이미지가 쓰는 바이트(`rbd du`)
    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError>;
    /// Ceph 풀 `pool`의 공간(`ceph df detail`). `user`·`conf`로 접속한다.
    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError>;
    /// `size_bytes`(MiB 배수) 크기의 이미지를 만든다(`rbd create`).
    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`).
    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError>;
    /// 이미지의 스냅샷들. 만든 순서(오래된 것부터)다.
    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError>;
    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지를 스냅샷 `tag`로 되돌린다(`rbd snap rollback`). 더 새 스냅샷은 남는다.
    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지 크기를 `size_bytes`(MiB 배수)로 늘린다(`rbd resize`, 줄이지 않는다).
    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, image: &RbdTarget) -> bool;
    /// 이미지를 연 클라이언트(watcher) 수(`rbd status`)
    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError>;
}

/// 실제 RBD 명령(`rbd`, `ceph`). 노드에 ceph.conf와 키링이 있어야 한다.
pub struct SystemRbd;

/// 같은 이미지를 바꾸는 앞 명령이 아직 돌고 있을 때의 일시 오류(`Timeout`)에 담는 말
const RBD_BUSY: &str = "an earlier rbd command still running on the same image";

/// 이미지를 연 클라이언트(다른 노드의 QEMU 등)가 있어 지우거나 되돌릴 수 없을 때의 일시 오류에 담는 말
const RBD_WATCHED: &str = "clients to close the rbd image";

/// RBD 명령. 바꾸는 명령의 대상은 이미지 사양(`{pool}[/{namespace}]/{image}`)이다. `rbd snap ls`의
/// 시각(C `ctime()`, 지역 시간)이 UTC로 나오게 `TZ=UTC`로 돌린다.
static RBD: BlockTool = BlockTool::new(
    "rbd command",
    RBD_BUSY,
    &[("LC_ALL", "C"), ("TZ", "UTC")],
    DriverError::Rbd,
);

/// 이미지 사양 `{pool}[/{namespace}]/{image}`
fn image_spec(image: &RbdTarget) -> String {
    match &image.namespace {
        Some(ns) => format!("{}/{ns}/{}", image.pool, image.image),
        None => format!("{}/{}", image.pool, image.image),
    }
}

/// Ceph 명령(`rbd`, `ceph`)의 접속 인자 `--id U [--conf C]` 뒤에 `args`를 붙인다.
fn ceph_args<'a>(user: &'a str, conf: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--id", user];
    if let Some(conf) = conf {
        all.extend(["--conf", conf]);
    }
    all.extend_from_slice(args);
    all
}

/// 이미지 `image`에 대한 `rbd` 인자. `--no-progress`는 일부 하위 명령만 받으므로 붙이지 않는다
/// (진행률은 어차피 버리는 출력에 찍힌다).
fn rbd_args<'a>(image: &'a RbdTarget, args: &[&'a str]) -> Vec<&'a str> {
    ceph_args(&image.user, image.conf.as_deref(), args)
}

fn bad_output(what: impl std::fmt::Display) -> DriverError {
    DriverError::Rbd(format!("unparseable rbd output: {what}"))
}

fn json(out: &[u8]) -> Result<Value, DriverError> {
    serde_json::from_slice(out).map_err(bad_output)
}

/// `rbd info --format json`의 `size`
fn parse_info(out: &[u8]) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_output(format!("info {v}")))
}

/// `rbd du --format json`에서 이미지 `name`(스냅샷이 아닌 줄)의 `used_size`
fn parse_du(out: &[u8], name: &str) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| {
            i.get("name").and_then(Value::as_str) == Some(name) && i.get("snapshot").is_none()
        })
        .and_then(|i| i.get("used_size").and_then(Value::as_u64))
        .ok_or_else(|| bad_output(format!("du {v}")))
}

/// `rbd snap ls`의 `timestamp`: C `ctime()` 모양(`Thu Oct 29 09:00:00 2026`, 날이 한 자리면 공백이
/// 둘)의 UTC 시각(명령을 `TZ=UTC`로 돌린다). 읽을 수 없으면(빈 값 등) `None`.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, hms, year] = parts[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|m| *m == month)? as u8 + 1;
    let hms: Vec<u8> = hms
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [hour, minute, second] = hms[..] else {
        return None;
    };
    let date = time::Date::from_calendar_date(
        year.parse().ok()?,
        time::Month::try_from(month).ok()?,
        day.parse().ok()?,
    )
    .ok()?;
    let at = time::PrimitiveDateTime::new(date, time::Time::from_hms(hour, minute, second).ok()?);
    let secs = u64::try_from(at.assume_utc().unix_timestamp()).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// `rbd status --format json`의 watcher 수
fn parse_status(out: &[u8]) -> Result<usize, DriverError> {
    let v = json(out)?;
    v.get("watchers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| bad_output(format!("status {v}")))
}

/// `rbd rm`이 이미지를 연 클라이언트 때문에 실패했으면(stderr 첫 줄이 librbd의 "image has
/// watchers"이거나 rbd의 "image still has watchers") 일시 오류로 바꾼다.
fn watched(e: DriverError) -> DriverError {
    match e {
        DriverError::Rbd(msg) if msg.contains("has watchers") => DriverError::Timeout(RBD_WATCHED),
        e => e,
    }
}

/// `rbd info`의 결과(`read`)로 이미지 크기를 낸다. 실패했으면 풀의 이미지 목록(`images`)으로
/// 이미지가 없는지 확인한다(오류 메시지로는 풀이 없는 것과 이미지가 없는 것을 가리기 어렵다). 시간을
/// 넘겼으면 목록도 늦을 것이므로 곧바로 그 오류다.
async fn info_from(
    read: Result<Vec<u8>, DriverError>,
    name: &str,
    images: impl Future<Output = Result<Vec<String>, DriverError>>,
) -> Result<Option<u64>, DriverError> {
    match read {
        Ok(out) => parse_info(&out).map(Some),
        Err(e @ DriverError::Timeout(_)) => Err(e),
        Err(e) => match images.await {
            Ok(names) if !names.iter().any(|n| n == name) => Ok(None),
            _ => Err(e),
        },
    }
}

/// `rbd snap ls --format json`. 스냅샷 id 순(만든 순서)으로 돌려준다.
fn parse_snapshots(out: &[u8]) -> Result<Vec<RbdSnapshot>, DriverError> {
    let v = json(out)?;
    let items = v
        .as_array()
        .ok_or_else(|| bad_output(format!("snap ls {v}")))?;
    let mut snapshots = items
        .iter()
        .map(|s| {
            let field = |k: &str| s.get(k).ok_or_else(|| bad_output(format!("snapshot {s}")));
            let id = field("id")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let tag = field("name")?
                .as_str()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let size = field("size")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let created = s
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp);
            Ok((
                id,
                RbdSnapshot {
                    tag: tag.to_string(),
                    size_bytes: size,
                    created,
                },
            ))
        })
        .collect::<Result<Vec<_>, DriverError>>()?;
    snapshots.sort_by_key(|(id, _)| *id);
    Ok(snapshots.into_iter().map(|(_, s)| s).collect())
}

/// `ceph df detail --format json`에서 풀 `pool`의 `max_avail`·`stored`(없는 값은 0). 풀이 없으면 오류.
pub fn parse_ceph_df(out: &[u8], pool: &str) -> Result<RbdSpace, DriverError> {
    let v = json(out)?;
    let stats = v
        .get("pools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(pool))
        .and_then(|p| p.get("stats"))
        .ok_or_else(|| DriverError::Rbd(format!("ceph pool {pool} not found in ceph df")))?;
    let field = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
    // 풀에 바이트 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다.
    let (stored, quota) = (field("stored"), field("quota_bytes"));
    let mut available = field("max_avail");
    if quota > 0 {
        available = available.min(quota.saturating_sub(stored));
    }
    Ok(RbdSpace {
        available_bytes: available,
        stored_bytes: stored,
    })
}

impl SystemRbd {
    /// 이미지의 풀(네임스페이스)에 있는 이미지 이름들(`rbd ls`)
    async fn images(&self, image: &RbdTarget) -> Result<Vec<String>, DriverError> {
        let mut args = vec!["ls", "--format", "json", "--pool", &image.pool];
        if let Some(ns) = &image.namespace {
            args.extend(["--namespace", ns]);
        }
        let v = json(
            &RBD.run_read("rbd", &rbd_args(image, &args), RBD_TIMEOUT)
                .await?,
        )?;
        let names = v.as_array().ok_or_else(|| bad_output(format!("ls {v}")))?;
        Ok(names
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect())
    }

    async fn change(
        &self,
        image: &RbdTarget,
        args: &[&str],
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let spec = image_spec(image);
        RBD.run_change("rbd", &rbd_args(image, args), &[&spec], timeout)
            .await
    }
}

#[async_trait]
impl RbdOps for SystemRbd {
    /// `rbd info`가 실패하면 풀의 이미지 목록으로 이미지가 없는지 확인한다([`info_from`]).
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["info", "--format", "json", &spec]);
        let read = RBD.run_read("rbd", &args, RBD_TIMEOUT).await;
        info_from(read, &image.image, self.images(image)).await
    }

    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["du", "--format", "json", &spec]);
        parse_du(
            &RBD.run_read("rbd", &args, RBD_TIMEOUT).await?,
            &image.image,
        )
    }

    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError> {
        let args = ceph_args(user, conf, &["df", "detail", "--format", "json"]);
        parse_ceph_df(&RBD.run_read("ceph", &args, RBD_TIMEOUT).await?, pool)
    }

    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        let args = ["create", "--size", &size, "--image-format", "2", &spec];
        self.change(image, &args, RBD_TIMEOUT).await
    }

    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
        let spec = image_spec(image);
        self.change(image, &["snap", "purge", &spec], RBD_LONG_TIMEOUT)
            .await?;
        self.change(image, &["rm", &spec], RBD_LONG_TIMEOUT)
            .await
            .map_err(watched)
    }

    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["snap", "ls", "--format", "json", &spec]);
        parse_snapshots(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }

    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "create", &name], RBD_TIMEOUT)
            .await
    }

    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rollback", &name], RBD_LONG_TIMEOUT)
            .await
    }

    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rm", &name], RBD_TIMEOUT)
            .await
    }

    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        self.change(image, &["resize", "--size", &size, &spec], RBD_TIMEOUT)
            .await
    }

    fn changing(&self, image: &RbdTarget) -> bool {
        RBD.changing(&image_spec(image))
    }

    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["status", "--format", "json", &spec]);
        parse_status(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }
}

/// `rbd --size` 값: MiB 단위(`{n}M`)
fn mib_arg(size_bytes: u64) -> String {
    format!("{}M", size_bytes / IMAGE_ALIGN)
}

/// 볼륨의 이미지 이름
fn volume_image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// QEMU가 여는 볼륨의 RBD 이미지. 사용자가 없으면 `admin`이다.
pub(super) fn volume_target(
    pool: &str,
    namespace: Option<&String>,
    user: Option<&String>,
    conf: Option<&String>,
    id: Uuid,
) -> RbdTarget {
    RbdTarget {
        pool: pool.to_string(),
        namespace: namespace.cloned(),
        image: volume_image(id),
        user: user.map_or(DEFAULT_USER, String::as_str).to_string(),
        conf: conf.cloned(),
    }
}

/// RBD 풀이면 그 풀의 볼륨 이미지
pub(super) fn pool_target(pool: &StoragePool, id: Uuid) -> Option<RbdTarget> {
    match &pool.spec.backend {
        PoolBackend::Rbd {
            pool,
            namespace,
            user,
            conf,
        } => Some(volume_target(
            pool,
            namespace.as_ref(),
            user.as_ref(),
            conf.as_ref(),
            id,
        )),
        PoolBackend::Dir { .. }
        | PoolBackend::Nfs { .. }
        | PoolBackend::LvmThin { .. }
        | PoolBackend::Zfs { .. } => None,
    }
}

/// RBD 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "rbd pools hold raw volumes".into(),
        )),
    }
}

/// `qemu-img`가 raw RBD 이미지(`snapshot`이 있으면 그 스냅샷)를 열게 하는 옵션 문자열
/// (`--image-opts`/`--target-image-opts`). 사용자·conf·네임스페이스를 함께 넘긴다. 옵션 값 안의
/// `,`는 `,,`로 쓴다.
pub(super) fn image_opts(image: &RbdTarget, snapshot: Option<&str>) -> String {
    let esc = |v: &str| v.replace(',', ",,");
    let mut s = format!("driver=raw,file.driver=rbd,file.pool={}", esc(&image.pool));
    if let Some(ns) = &image.namespace {
        s.push_str(&format!(",file.namespace={}", esc(ns)));
    }
    s.push_str(&format!(",file.image={}", esc(&image.image)));
    if let Some(tag) = snapshot {
        s.push_str(&format!(",file.snapshot={}", esc(tag)));
    }
    s.push_str(&format!(",file.user={}", esc(&image.user)));
    if let Some(conf) = &image.conf {
        s.push_str(&format!(",file.conf={}", esc(conf)));
    }
    s
}

/// 이미지 크기: `IMAGE_ALIGN` 배수로 올린다.
fn image_size(size_bytes: u64) -> Result<u64, DriverError> {
    size_bytes
        .div_ceil(IMAGE_ALIGN)
        .checked_mul(IMAGE_ALIGN)
        .ok_or_else(|| DriverError::InvalidSpec(format!("volume size {size_bytes} is too large")))
}

/// Ceph RBD 풀(`rbd`)의 볼륨. 볼륨은 raw 이미지이고 스냅샷은 RBD 스냅샷이다. 이미지는 노드에
/// 놓이지 않으므로 볼륨의 경로는 빈 경로다.
pub struct RbdStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn RbdOps>,
}

impl RbdStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn RbdOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
        }
    }

    /// 풀의 볼륨 이미지. RBD 풀이 아니거나 raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, id: Uuid, format: ImageFormat) -> Result<RbdTarget, DriverError> {
        require_raw(format)?;
        pool_target(pool, id).ok_or_else(|| DriverError::InvalidSpec("not an rbd pool".into()))
    }

    /// 스냅샷 태그는 RBD 스냅샷 이름과 qemu-img 옵션에 그대로 쓸 수 있는 글자여야 한다.
    fn check_tag(tag: &str) -> Result<(), DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c));
        if ok {
            Ok(())
        } else {
            Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid rbd snapshot name"
            )))
        }
    }

    /// 있는 볼륨의 크기. 없으면 `VolumeNotFound`.
    async fn existing(&self, image: &RbdTarget, id: Uuid) -> Result<u64, DriverError> {
        self.ops
            .info(image)
            .await?
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨이 있고 스냅샷 `tag`도 있어야 한다. 그 스냅샷을 돌려준다.
    async fn existing_snapshot(
        &self,
        image: &RbdTarget,
        id: Uuid,
        tag: &str,
    ) -> Result<RbdSnapshot, DriverError> {
        self.existing(image, id).await?;
        self.ops
            .snapshots(image)
            .await?
            .into_iter()
            .find(|s| s.tag == tag)
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
            })
    }

    /// 이미지를 연 클라이언트(다른 노드에서 도는 VM의 QEMU 등)가 있으면 일시 오류다.
    async fn unwatched(&self, image: &RbdTarget) -> Result<(), DriverError> {
        if self.ops.watchers(image).await? > 0 {
            return Err(DriverError::Timeout(RBD_WATCHED));
        }
        Ok(())
    }

    /// 새 볼륨 이미지를 만든다. 크기는 1 MiB 배수로 올린다.
    async fn create_image(
        &self,
        image: &RbdTarget,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let size = image_size(size_bytes)?;
        if self.ops.info(image).await?.is_some() {
            return Err(DriverError::VolumeExists(id));
        }
        self.ops.create(image, size).await
    }

    /// 만든 이미지에 `qemu-img convert -n`으로 써 넣는다(`from`은 원본 인자, `-O` 없이). 새 이미지는
    /// 0으로 읽히므로 0인 부분은 쓰지 않는다(`--target-is-zero`). 실패하면 이미지를 지운다.
    async fn fill(&self, image: &RbdTarget, id: Uuid, from: &[String]) -> Result<(), DriverError> {
        let written = async {
            let dst = image_opts(image, None);
            let mut args = vec![
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
            ];
            args.extend(from.iter().map(String::as_str));
            args.push(&dst);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.ops.remove(image).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for RbdStorage {
    /// 이미지에는 경로가 없으므로 빈 경로를 돌려준다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.create_image(&image, id, size_bytes).await?;
        Ok(PathBuf::new())
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        self.create_image(&image, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), src_str].map(String::from);
        self.fill(&image, id, &from).await?;
        Ok(PathBuf::new())
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 이미지를 스냅샷과 함께 지운다. 이미지가 없어도 그 이미지를 바꾸는 명령이 아직 돌고 있으면
    /// (시간을 넘긴 만들기) 이미지가 곧 생길 수 있으므로 `VolumeNotFound`가 아니라 일시 오류다.
    /// 이미지를 연 클라이언트가 있으면 스냅샷도 지우지 않고 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.info(&image).await?.is_none() {
            if self.ops.changing(&image) {
                return Err(DriverError::Timeout(RBD_BUSY));
            }
            return Err(DriverError::VolumeNotFound(id));
        }
        self.unwatched(&image).await?;
        self.ops.remove(&image).await
    }

    /// QEMU가 이미지를 직접 연다. 이미지가 있는지만 본다. 그 이미지를 바꾸는 명령(시간을 넘긴
    /// 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 이미지를 연 클라이언트는 보지 않는다(다시 붙는
    /// VM의 QEMU가 그 클라이언트다).
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        self.existing(&image, id).await.map(drop)
    }

    /// 쓰는 바이트(`rbd du`)를 알 수 없으면 이미지 크기로 본다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size = self.existing(&image, id).await?;
        let used = match self.ops.used(&image).await {
            Ok(used) => used,
            Err(e) => {
                tracing::debug!(volume = %id, error = %e, "rbd du failed, using image size");
                size
            }
        };
        Ok(VolumeInfo {
            virtual_bytes: size,
            allocated_bytes: used,
        })
    }

    /// Ceph 풀의 크기(`max_avail`+`stored`)와 남은 공간(`max_avail`)
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let image = Self::target(pool, Uuid::nil(), ImageFormat::Raw)?;
        let space = self
            .ops
            .space(&image.pool, &image.user, image.conf.as_deref())
            .await?;
        Ok(Capacity {
            total_bytes: space.available_bytes.saturating_add(space.stored_bytes),
            available_bytes: space.available_bytes,
        })
    }

    /// 볼륨이 raw이므로 RBD 스냅샷으로 스냅샷을 둔다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Raw
    }

    /// VM이 실행 중이어도 만들 수 있다(일관성은 호출자가 VM을 잠시 멈춰 맞춘다). 여러 볼륨은
    /// 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없다).
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        self.existing(&image, id).await?;
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        self.ops.snapshot(&image, tag).await
    }

    /// 아무 스냅샷으로나 되돌린다(더 새 스냅샷은 남는다). 이미지를 연 클라이언트가 없어야 한다
    /// (있으면 일시 오류).
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing_snapshot(&image, id, tag).await?;
        self.unwatched(&image).await?;
        self.ops.rollback(&image, tag).await
    }

    /// VM이 실행 중이어도 지울 수 있다. 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 이미지를
    /// 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 곧 생길 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        if self.ops.info(&image).await?.is_none() {
            return Ok(());
        }
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            self.ops.remove_snapshot(&image, tag).await?;
        }
        Ok(())
    }

    /// 만든 시각(`rbd snap ls`의 `timestamp`)으로 고른다. 시각을 알 수 없는 태그는 고르지 않는다.
    async fn snapshot_tags_older_than(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        age: Duration,
    ) -> Result<Vec<String>, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing(&image, id).await?;
        let now = SystemTime::now();
        let snapshots = self.ops.snapshots(&image).await?;
        Ok(snapshots
            .into_iter()
            .filter(|s| {
                s.created
                    .and_then(|t| now.duration_since(t).ok())
                    .is_some_and(|d| d > age)
            })
            .map(|s| s.tag)
            .collect())
    }

    /// 만든 순서다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing(&image, id).await?;
        let snapshots = self.ops.snapshots(&image).await?;
        Ok(snapshots.into_iter().map(|s| s.tag).collect())
    }

    /// VM이 실행 중이어도 늘릴 수 있다(그 뒤 QEMU에 새 크기를 알리는 것은 호출자의 일이다). 크기는
    /// 1 MiB 배수로 올린다. 이미 그 크기 이상이면(앞선 시도가 늘렸다) 아무것도 하지 않는다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.existing(&image, id).await? >= size_bytes {
            return Ok(());
        }
        self.ops.resize(&image, image_size(size_bytes)?).await
    }

    /// 이미지, 또는 그 스냅샷(읽기 전용). `qemu-img`가 `rbd` 드라이버로 연다. 내놓을 것은 없다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size_bytes = match tag {
            Some(tag) => self.existing_snapshot(&image, id, tag).await?.size_bytes,
            None => self.existing(&image, id).await?,
        };
        Ok(VolumeSource {
            path: PathBuf::new(),
            format,
            snapshot: tag.map(str::to_string),
            size_bytes,
            rbd: Some(image),
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

    /// 원본(스냅샷이면 그때)의 크기로 대상 이미지를 만들고 원본을 써 넣는다. Ceph 풀·사용자가
    /// 달라도 같다.
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
        let image = Self::target(dst_pool, dst_id, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        self.create_image(&image, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, None)?;
        self.fill(&image, dst_id, &from).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct FakeImage {
        size: u64,
        /// 만든 순서의 스냅샷(태그, 크기, 만든 시각)
        snapshots: Vec<(String, u64, Option<SystemTime>)>,
    }

    /// Ceph 풀 `rbd`, `other`를 메모리에 두는 RBD. 이미지는 사양(`{pool}[/{ns}]/{image}`)으로
    /// 둔다. 바꾸는 호출을 적고, `fail`에 적은 호출은 실패한다.
    #[derive(Default)]
    struct FakeRbd {
        images: Mutex<BTreeMap<String, FakeImage>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        /// 바꾸는 명령이 아직 돌고 있다고 알리는 이미지 사양
        changing: Mutex<Vec<String>>,
        /// `rbd du`가 실패한다.
        no_du: Mutex<bool>,
        /// 연 클라이언트(watcher)가 있는 이미지 사양
        watched: Mutex<Vec<String>>,
    }

    impl FakeRbd {
        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Rbd(format!("injected: {call}")));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.images.lock().unwrap().keys().cloned().collect()
        }

        fn image(&self, spec: &str) -> Option<FakeImage> {
            self.images.lock().unwrap().get(spec).cloned()
        }

        /// 이미지 `spec`의 스냅샷 `tag`를 만든 시각을 바꾼다.
        fn set_created(&self, spec: &str, tag: &str, created: Option<SystemTime>) {
            let mut images = self.images.lock().unwrap();
            let snapshots = &mut images.get_mut(spec).unwrap().snapshots;
            snapshots.iter_mut().find(|s| s.0 == tag).unwrap().2 = created;
        }

        fn missing(spec: &str) -> DriverError {
            DriverError::Rbd(format!(
                "rbd: error opening image {spec}: (2) No such file or directory"
            ))
        }

        fn known_pool(image: &RbdTarget) -> Result<(), DriverError> {
            if ["rbd", "other"].contains(&image.pool.as_str()) {
                Ok(())
            } else {
                Err(DriverError::Rbd(format!(
                    "rbd: error opening pool '{}': (2) No such file or directory",
                    image.pool
                )))
            }
        }
    }

    #[async_trait]
    impl RbdOps for FakeRbd {
        async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
            Self::known_pool(image)?;
            Ok(self.image(&image_spec(image)).map(|i| i.size))
        }

        async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
            if *self.no_du.lock().unwrap() {
                return Err(DriverError::Rbd("du failed".into()));
            }
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.size / 4)
        }

        async fn space(
            &self,
            pool: &str,
            user: &str,
            conf: Option<&str>,
        ) -> Result<RbdSpace, DriverError> {
            self.call(format!("df {pool} {user} {conf:?}"))?;
            Ok(RbdSpace {
                available_bytes: 6 << 30,
                stored_bytes: 2 << 30,
            })
        }

        async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("create {spec} {size_bytes}"))?;
            Self::known_pool(image)?;
            let mut images = self.images.lock().unwrap();
            if images.contains_key(&spec) {
                return Err(DriverError::Rbd(
                    "rbd: create error: (17) File exists".into(),
                ));
            }
            let i = FakeImage {
                size: size_bytes,
                snapshots: vec![],
            };
            images.insert(spec, i);
            Ok(())
        }

        async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("remove {spec}"))?;
            let removed = self.images.lock().unwrap().remove(&spec);
            removed.map(drop).ok_or_else(|| Self::missing(&spec))
        }

        async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.snapshots
                .iter()
                .map(|(tag, size, created)| RbdSnapshot {
                    tag: tag.clone(),
                    size_bytes: *size,
                    created: *created,
                })
                .collect())
        }

        async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap create {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots
                .push((tag.into(), i.size, Some(SystemTime::now())));
            Ok(())
        }

        async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rollback {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            let (_, size, _) = i
                .snapshots
                .iter()
                .find(|(t, _, _)| t == tag)
                .cloned()
                .ok_or_else(|| Self::missing(&format!("{spec}@{tag}")))?;
            i.size = size;
            Ok(())
        }

        async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rm {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots.retain(|(t, _, _)| t != tag);
            Ok(())
        }

        async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("resize {spec} {size_bytes}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            if size_bytes < i.size {
                return Err(DriverError::Rbd("shrinking requires --allow-shrink".into()));
            }
            i.size = size_bytes;
            Ok(())
        }

        fn changing(&self, image: &RbdTarget) -> bool {
            let spec = image_spec(image);
            self.changing.lock().unwrap().contains(&spec)
        }

        async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
            let spec = image_spec(image);
            self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            let watched = self.watched.lock().unwrap();
            Ok(watched.iter().filter(|s| **s == spec).count())
        }
    }

    fn rbd_pool(pool: &str, namespace: Option<&str>) -> StoragePool {
        Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: pool.into(),
                    namespace: namespace.map(Into::into),
                    user: None,
                    conf: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    fn pool() -> StoragePool {
        rbd_pool("rbd", None)
    }

    fn spec(id: Uuid) -> String {
        format!("rbd/opl-{id}")
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

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeRbd>, RbdStorage) {
        let rbd = Arc::new(FakeRbd::default());
        let storage = RbdStorage::new(qemu_img, rbd.clone());
        (rbd, storage)
    }

    fn invalid(e: DriverError, want: &str) {
        match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains(want), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn not_found(e: DriverError, id: Uuid) {
        assert!(
            matches!(e, DriverError::VolumeNotFound(v) if v == id),
            "{e:?}"
        );
    }

    fn args(dir: &Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        args.lines().map(str::to_string).collect()
    }

    const RAW: ImageFormat = ImageFormat::Raw;
    const MIB: u64 = 1 << 20;

    /// 크기를 1 MiB 배수로 올린 이미지를 만든다. 이미지에는 경로가 없다.
    #[tokio::test]
    async fn creates_raw_images() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let path = storage.create_volume(&p, id, MIB + 1, RAW).await.unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {}", spec(id), 2 * MIB)]);

        let err = storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), MIB, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), u64::MAX, RAW)
                .await
                .unwrap_err(),
            "too large",
        );
        assert!(rbd.taken().is_empty());
        // 네임스페이스 안에 만든다. Ceph 풀이 없으면 RBD 오류다.
        let id = Uuid::now_v7();
        let ns = rbd_pool("rbd", Some("opl"));
        storage.create_volume(&ns, id, MIB, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("create rbd/opl/opl-{id} {MIB}")]);
        let err = storage
            .create_volume(&rbd_pool("none", None), id, MIB, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다(새 이미지는 0으로 읽힌다). 대상은
    /// 옵션 문자열이다. 실패하면 이미지를 지운다.
    #[tokio::test]
    async fn imports_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {MIB}", spec(id))]);
        let dst =
            format!("driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{id},file.user=admin");
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "-f",
                "vmdk",
                "/up/disk.vmdk",
                &dst,
            ]
        );
        invalid(
            storage
                .import_volume(&p, id, Path::new("up.vmdk"), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(id)),
                format!("remove {}", spec(id)),
            ]
        );
        assert!(rbd.names().is_empty());
    }

    /// 이미지를 스냅샷과 함께 지운다. 없으면 `VolumeNotFound`, 그 이미지를 바꾸는 명령이 아직
    /// 돌고 있으면(시간을 넘긴 만들기) 일시 오류다.
    #[tokio::test]
    async fn delete_removes_image_with_its_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
    }

    /// 이미지를 연 클라이언트(watcher, 다른 노드의 QEMU 등)가 있으면 스냅샷도 지우지 않고 일시
    /// 오류다. `rbd rm`이 watcher 때문에 실패해도 일시 오류다.
    #[tokio::test]
    async fn delete_waits_for_watchers() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["t"]);

        rbd.watched.lock().unwrap().clear();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);

        // `rbd rm`의 stderr 첫 줄(librbd 기록이나 rbd의 오류)
        for line in [
            "rbd: 2026-10-29T09:00:00.000+0000 7f0 -1 librbd::image::PreRemoveRequest: 0x1 \
             check_image_watchers: image has watchers - not removing",
            "rbd: rbd: error: image still has watchers",
        ] {
            let err = watched(DriverError::Rbd(line.into()));
            assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        }
        let err = watched(DriverError::Rbd("rbd: error: no such image".into()));
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 활성화는 이미지가 있는지만 본다. 정보는 크기와 쓰는 바이트(알 수 없으면 크기), 용량은 Ceph
    /// 풀의 `max_avail`+`stored`다.
    #[tokio::test]
    async fn reports_volume_usage_and_pool_capacity() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.activate_volume(&p, id, RAW).await.unwrap_err(), id);
        not_found(storage.volume_info(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, 4 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: MIB,
            }
        );
        *rbd.no_du.lock().unwrap() = true;
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: 4 * MIB,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        // 사용자·conf가 있으면 그것으로 접속한다.
        let custom: StoragePool = Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: "other".into(),
                    namespace: Some("ns".into()),
                    user: Some("opl".into()),
                    conf: Some("/etc/ceph/opl.conf".into()),
                },
                nodes: vec![],
                shared: true,
            },
        );
        storage.capacity(&custom).await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                "df rbd admin None",
                "df other opl Some(\"/etc/ceph/opl.conf\")"
            ]
        );
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
    }

    /// 늘리기는 1 MiB 배수로 올린다. 이미 크면 아무것도 하지 않는다.
    #[tokio::test]
    async fn resize_grows_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.resize_volume(&p, id, RAW, MIB).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, 2 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.resize_volume(&p, id, RAW, MIB).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        assert!(rbd.taken().is_empty());
        storage
            .resize_volume(&p, id, RAW, 2 * MIB + 1)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("resize {} {}", spec(id), 3 * MIB)]);
    }

    /// 스냅샷은 RBD 스냅샷이다. 같은 태그는 다시 만들지 않고, 태그는 만든 순서다. 아무 스냅샷으로나
    /// 되돌릴 수 있고 더 새 스냅샷은 남는다.
    #[tokio::test]
    async fn snapshots_are_rbd_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        storage.snapshot_many(&p, &[id], RAW, "b").await.unwrap();
        rbd.taken();
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        invalid(
            storage
                .snapshot_create(&p, id, RAW, "a,b")
                .await
                .unwrap_err(),
            "not a valid",
        );
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );

        // 가장 최근이 아닌 스냅샷으로도 되돌린다.
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.image(&spec(id)).unwrap().size, MIB);
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );
        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        // 없는 태그와 없는 볼륨은 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "x").await.unwrap();
        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        storage
            .snapshot_delete(&p, Uuid::now_v7(), RAW, "a")
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("snap rm {}@a", spec(id))]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["b"]);

        // 그 이미지를 바꾸는 명령이 아직 돌고 있으면 태그가 곧 생길 수 있다.
        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.snapshot_delete(&p, id, RAW, "c").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// 되돌리기는 이미지를 연 클라이언트가 없어야 한다(일시 오류). 활성화(VM 시작)는 그 이미지를
    /// 바꾸는 명령(시간을 넘긴 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 활성화는 watcher를 보지
    /// 않는다(다시 붙는 VM의 QEMU가 watcher다).
    #[tokio::test]
    async fn revert_and_activate_wait_for_the_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(rbd.taken().is_empty());
        storage.activate_volume(&p, id, RAW).await.unwrap();
        rbd.watched.lock().unwrap().clear();
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        // 만들기가 시간을 넘겨 아직 이미지가 없어도 같다.
        let new = Uuid::now_v7();
        rbd.changing.lock().unwrap().push(spec(new));
        let err = storage.activate_volume(&p, new, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// `rbd info`가 실패하면 이미지 목록으로 없는 것을 가린다. 시간을 넘겼으면 목록을 읽지 않는다.
    #[tokio::test]
    async fn info_falls_back_to_the_image_list_unless_timed_out() {
        let list = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            async move { Ok(names) }
        };
        let info = br#"{"size":1048576}"#.to_vec();
        assert_eq!(
            info_from(Ok(info), "opl-a", list(&[])).await.unwrap(),
            Some(MIB)
        );
        let failed = || Err(DriverError::Rbd("rbd: error opening image".into()));
        assert_eq!(
            info_from(failed(), "opl-a", list(&["opl-b"]))
                .await
                .unwrap(),
            None
        );
        let err = info_from(failed(), "opl-a", list(&["opl-a"]))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
        let unlisted = async { panic!("listed images after a timeout") };
        let err = info_from(Err(DriverError::Timeout("rbd command")), "opl-a", unlisted)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
    }

    /// 만든 지 오래된 태그만 고른다. 만든 시각을 알 수 없는 태그는 고르지 않는다.
    #[tokio::test]
    async fn old_snapshot_tags_by_creation_time() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let age = Duration::from_secs(30 * 60);
        not_found(
            storage
                .snapshot_tags_older_than(&p, id, RAW, age)
                .await
                .unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        for tag in ["old", "new", "unknown", "older"] {
            storage.snapshot_create(&p, id, RAW, tag).await.unwrap();
        }
        let ago = |secs| Some(SystemTime::now() - Duration::from_secs(secs));
        rbd.set_created(&spec(id), "old", ago(31 * 60));
        rbd.set_created(&spec(id), "new", ago(29 * 60));
        rbd.set_created(&spec(id), "unknown", None);
        rbd.set_created(&spec(id), "older", ago(24 * 3600));
        assert_eq!(
            storage
                .snapshot_tags_older_than(&p, id, RAW, age)
                .await
                .unwrap(),
            ["old", "older"]
        );
    }

    /// 여러 볼륨은 하나씩 찍는다.
    #[tokio::test]
    async fn snapshot_many_takes_one_image_at_a_time() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, a, MIB, RAW).await.unwrap();
        storage.create_volume(&p, b, MIB, RAW).await.unwrap();
        rbd.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                format!("snap create {}@t", spec(a)),
                format!("snap create {}@t", spec(b)),
            ]
        );
    }

    /// 원본은 이미지 또는 그 스냅샷이고 크기는 그때의 것이다. 내놓을 것은 없다.
    #[tokio::test]
    async fn volume_source_is_the_image_or_its_snapshot() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.volume_source(&p, id, RAW, None).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        let image = pool_target(&p, id).unwrap();
        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: PathBuf::new(),
                format: RAW,
                snapshot: Some("t".into()),
                size_bytes: MIB,
                rbd: Some(image.clone()),
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!((source.snapshot, source.size_bytes), (None, 2 * MIB));
        assert_eq!(source.rbd, Some(image));
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );
        assert!(rbd.taken().is_empty());
    }

    /// 같은 백엔드 복사: 원본(스냅샷이면 그때)의 크기로 이미지를 만들고 원본을 써 넣는다. 원본과
    /// 대상 모두 옵션 문자열이다. 다른 Ceph 풀로도 된다. 실패하면 만든 이미지를 지운다.
    #[tokio::test]
    async fn copies_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, other) = (pool(), rbd_pool("other", Some("ns")));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        storage.resize_volume(&p, src, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("create other/ns/opl-{dst} {MIB}")]);
        let from = format!(
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{src},file.snapshot=t,file.user=admin"
        );
        let to = format!(
            "driver=raw,file.driver=rbd,file.pool=other,file.namespace=ns,file.image=opl-{dst},file.user=admin"
        );
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "--image-opts",
                "-U",
                &from,
                &to,
            ]
        );
        let err = storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == dst),
            "{err:?}"
        );

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        rbd.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(dst)),
                format!("remove {}", spec(dst)),
            ]
        );
        assert_eq!(rbd.names(), [spec(src)]);
    }

    /// 다른 풀 종류는 다루지 않는다.
    #[tokio::test]
    async fn refuses_other_pools() {
        let (_, storage) = setup("/nonexistent/qemu-img");
        let dir: StoragePool = Resource::new(
            "d",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pools/local".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        invalid(
            storage
                .create_volume(&dir, Uuid::now_v7(), MIB, RAW)
                .await
                .unwrap_err(),
            "not an rbd pool",
        );
        invalid(storage.capacity(&dir).await.unwrap_err(), "not an rbd pool");
    }

    /// 바꾸는 명령이 실패하면 그 오류를 그대로 돌려준다.
    #[tokio::test]
    async fn command_failures_are_rbd_errors() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        rbd.fail_on(format!("snap create {}@t", spec(id)));
        let err = storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert!(!err.is_transient());
    }

    /// `qemu-img`의 옵션 문자열은 네임스페이스·스냅샷·conf를 있을 때만 넣고 `,`를 `,,`로 쓴다.
    #[test]
    fn image_opts_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(
            image_opts(&image, None),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-a,file.user=admin"
        );
        image.namespace = Some("ns".into());
        image.user = "opl".into();
        image.conf = Some("/etc/ceph/a,b.conf".into());
        assert_eq!(
            image_opts(&image, Some("t")),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.namespace=ns,file.image=opl-a,\
             file.snapshot=t,file.user=opl,file.conf=/etc/ceph/a,,b.conf"
        );
    }

    /// `rbd`·`ceph` 인자: 접속(`--id`, `--conf`)을 앞에 두고 진행률을 찍지 않는다. 이미지 사양은
    /// 네임스페이스를 담는다.
    #[test]
    fn rbd_commands_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(image_spec(&image), "rbd/opl-a");
        assert_eq!(
            rbd_args(&image, &["info", "rbd/opl-a"]),
            ["--id", "admin", "info", "rbd/opl-a"]
        );
        image.namespace = Some("ns".into());
        image.conf = Some("/etc/ceph/opl.conf".into());
        assert_eq!(image_spec(&image), "rbd/ns/opl-a");
        assert_eq!(
            rbd_args(&image, &["rm", "rbd/ns/opl-a"]),
            [
                "--id",
                "admin",
                "--conf",
                "/etc/ceph/opl.conf",
                "rm",
                "rbd/ns/opl-a"
            ]
        );
        assert_eq!(
            ceph_args("opl", None, &["df", "detail"]),
            ["--id", "opl", "df", "detail"]
        );
        assert_eq!(mib_arg(3 * MIB), "3M");
    }

    /// RBD 명령은 C 로캘로 돌고, 실패는 `Rbd` 오류다.
    #[tokio::test]
    async fn rbd_commands_use_c_locale_and_rbd_errors() {
        let long = Duration::from_secs(10);
        let out = RBD
            .run_read("/bin/sh", &["-c", "echo $LC_ALL $TZ"], long)
            .await
            .unwrap();
        // `rbd snap ls`의 시각(C `ctime()`)이 UTC로 나온다.
        assert_eq!(out, b"C UTC\n");
        let err = RBD
            .run_read("/bin/sh", &["-c", "echo 'rbd: error' >&2; exit 2"], long)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m == "/bin/sh: rbd: error"),
            "{err:?}"
        );
    }

    #[test]
    fn parses_rbd_output() {
        assert_eq!(
            parse_info(br#"{"name":"opl-a","size":1073741824,"objects":256}"#).unwrap(),
            1 << 30
        );
        assert!(parse_info(b"{}").is_err());
        assert!(parse_info(b"not json").is_err());

        // 스냅샷 줄이 아닌 이미지 줄의 used_size
        let du = br#"{"images":[
            {"name":"opl-a","snapshot":"t","id":4,"provisioned_size":10,"used_size":7},
            {"name":"opl-a","id":"1","provisioned_size":10,"used_size":3},
            {"name":"opl-b","id":"2","provisioned_size":10,"used_size":9}
        ],"total_provisioned_size":20,"total_used_size":19}"#;
        assert_eq!(parse_du(du, "opl-a").unwrap(), 3);
        assert!(parse_du(du, "opl-c").is_err());

        // 스냅샷 id 순. 시각은 읽을 수 있을 때만 담는다.
        let snaps = br#"[
            {"id":9,"name":"b","size":2048,"protected":"false","timestamp":"x"},
            {"id":4,"name":"a","size":1024,"protected":"false","timestamp":"Thu Oct  1 09:00:05 2026"},
            {"id":5,"name":"c","size":1024,"protected":"false"}
        ]"#;
        assert_eq!(
            parse_snapshots(snaps).unwrap(),
            [
                RbdSnapshot {
                    tag: "a".into(),
                    size_bytes: 1024,
                    created: Some(UNIX_EPOCH + Duration::from_secs(1_790_845_205)),
                },
                RbdSnapshot {
                    tag: "c".into(),
                    size_bytes: 1024,
                    created: None,
                },
                RbdSnapshot {
                    tag: "b".into(),
                    size_bytes: 2048,
                    created: None,
                },
            ]
        );
        assert_eq!(parse_snapshots(b"[]").unwrap(), []);
        assert!(parse_snapshots(br#"[{"id":1}]"#).is_err());

        let df = br#"{"stats":{},"pools":[
            {"name":"other","id":1,"stats":{"stored":1,"max_avail":2}},
            {"name":"rbd","id":2,"stats":{"stored":300,"max_avail":700,"objects":3}},
            {"name":"empty","id":3,"stats":{}}
        ]}"#;
        assert_eq!(
            parse_ceph_df(df, "rbd").unwrap(),
            RbdSpace {
                available_bytes: 700,
                stored_bytes: 300,
            }
        );
        assert_eq!(
            parse_ceph_df(df, "empty").unwrap(),
            RbdSpace {
                available_bytes: 0,
                stored_bytes: 0,
            }
        );
        assert!(matches!(
            parse_ceph_df(df, "none").unwrap_err(),
            DriverError::Rbd(_)
        ));
        // 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 담긴 데이터를 뺀 것까지다.
        let df = br#"{"pools":[
            {"name":"q","stats":{"stored":300,"max_avail":700,"quota_bytes":500}},
            {"name":"full","stats":{"stored":600,"max_avail":700,"quota_bytes":500}},
            {"name":"wide","stats":{"stored":300,"max_avail":700,"quota_bytes":5000}},
            {"name":"none","stats":{"stored":300,"max_avail":700,"quota_bytes":0}}
        ]}"#;
        let available = |pool| parse_ceph_df(df, pool).unwrap().available_bytes;
        assert_eq!(available("q"), 200);
        assert_eq!(available("full"), 0);
        assert_eq!(available("wide"), 700);
        assert_eq!(available("none"), 700);
        assert_eq!(parse_ceph_df(df, "q").unwrap().stored_bytes, 300);

        // `rbd status`의 watcher 수
        let status = br#"{"watchers":[
            {"address":"10.0.0.1:0/1","client":4100,"cookie":1},
            {"address":"10.0.0.2:0/2","client":4200,"cookie":2}
        ]}"#;
        assert_eq!(parse_status(status).unwrap(), 2);
        assert_eq!(parse_status(br#"{"watchers":[]}"#).unwrap(), 0);
        assert!(parse_status(b"{}").is_err());
    }

    /// `ctime()` 모양의 UTC 시각. 읽을 수 없으면 `None`이다.
    #[test]
    fn parses_ctime_timestamps() {
        let at = |secs| Some(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(
            parse_timestamp("Thu Oct 29 09:00:00 2026"),
            at(1_793_264_400)
        );
        assert_eq!(parse_timestamp("Thu Jan  1 00:00:00 1970"), at(0));
        assert_eq!(parse_timestamp("Mon Feb 29 23:59:59 2027"), None);
        for bad in [
            "",
            "x",
            "Thu Oct 29 09:00 2026",
            "Thu Okt 29 09:00:00 2026",
            "Thu Oct 29 25:00:00 2026",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }
}
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
use std::sync::Arc;
use std::time::Duration;
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        self.backend(pool)?.snapshot_tags(pool, id, format).await
    }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        }
        async fn resize_volume(
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            storages.snapshot_tags(&p, id, f).await.unwrap();
            storages.resize_volume(&p, id, f, 2).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            storages.snapshot_tags(&p, id, f).await.unwrap();
            storages
                .snapshot_tags_older_than(&p, id, f, Duration::from_secs(60))
                .await
                .unwrap();
            storages.resize_volume(&p, id, f, 2).await.unwrap();
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                "snapshot_tags",
                "resize_volume",
```

을 다음으로 바꾼다:

```rust
                "snapshot_tags",
                "snapshot_tags_older_than",
                "resize_volume",
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 600개, 실패 0

Run: `cargo test -p opl-api`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(api): RBD 스냅샷 규칙 — 아무 스냅샷으로 되돌리기, 공유 풀의 남은 태그 정리"
```

---

### Task 6: 실제 Ceph 시험(rbd-it)과 CI

기능 `rbd-it`(opl-driver, opld)과 Linux 전용 `crates/opl-driver/tests/rbd_storage.rs`(만들기·정보·크기·지우기, vmdk 가져오기, 아무 스냅샷으로 되돌리기, RBD ↔ 디렉터리 복사, 제한된 사용자·네임스페이스·conf), `crates/opld/tests/rbd.rs`(RBD 풀로 VM, `driver=rbd`, 스냅샷·되돌리기, 디렉터리 풀로 복제, 지우기). `OPL_TEST_RBD`(와 `_USER`·`_NAMESPACE`·`_CONF`). CI `linux-kvm`에 ceph-common·qemu-block-extra·jq, microceph(`squid/stable`, 별칭 해제, `/usr/bin/rbd` 확인), 루프 OSD, `rbd` 풀(크기 1), `rbd/opl`, `client.opltest`. 작업 제한 55분.

**Files:**
- Create: `crates/opl-driver/tests/rbd_storage.rs`
- Create: `crates/opld/tests/rbd.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opld/Cargo.toml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
    runs-on: ubuntu-latest
    timeout-minutes: 40
    # root로 도는 테스트도 runner가 설치한 브라우저를 쓴다.
```

을 다음으로 바꾼다:

```yaml
    runs-on: ubuntu-latest
    timeout-minutes: 55
    # root로 도는 테스트도 runner가 설치한 브라우저를 쓴다.
```

`.github/workflows/ci.yml`에서

```yaml
          sudo zpool status opltestz
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

을 다음으로 바꾼다:

```yaml
          sudo zpool status opltestz
      # RBD 풀 시험이 이미지를 만들 Ceph 풀 `rbd`. microceph로 노드 하나짜리 Ceph(루프 파일 OSD 하나,
      # 복제 1)를 띄운다. ceph-common의 rbd·ceph와 QEMU(qemu-block-extra의 librbd)가 Ceph 기본 위치에서
      # 읽도록 microceph의 ceph.conf와 키링을 /etc/ceph에 둔다. 제한된 사용자 시험을 위해 네임스페이스
      # `rbd/opl`과 사용자 `client.opltest`(caps `profile rbd`)를 만들고, 그 키링은 Ceph 기본 위치가 아닌
      # /etc/opl-ceph에 두어 그곳의 ceph.conf로만 찾게 한다.
      - name: Ceph (microceph) with an rbd pool
        timeout-minutes: 15
        run: |
          sudo apt-get update
          sudo apt-get install -y ceph-common qemu-block-extra jq
          sudo snap install microceph --channel squid/stable
          sudo snap refresh --hold microceph
          # microceph의 별칭(/snap/bin/rbd·ceph 등)은 snap에 갇힌 클라이언트라 /etc/opl-ceph와 시험의
          # /tmp(snap 전용 /tmp)를 보지 못한다. 시험은 `sudo -E env "PATH=$PATH"`로 돌고 러너의 PATH는
          # /snap/bin이 /usr/bin보다 앞이므로 별칭을 없애 ceph-common의 명령을 쓰게 한다.
          snap aliases microceph
          if snap aliases microceph | grep -q '^microceph\.'; then sudo snap unalias microceph; fi
          sudo env "PATH=$PATH" sh -c 'echo "PATH=$PATH"; command -v rbd ceph qemu-img qemu-io'
          test "$(sudo env "PATH=$PATH" sh -c 'command -v rbd')" = /usr/bin/rbd
          test "$(sudo env "PATH=$PATH" sh -c 'command -v ceph')" = /usr/bin/ceph
          sudo microceph cluster bootstrap
          sudo mkdir -p /etc/ceph
          sudo ln -sf /var/snap/microceph/current/conf/ceph.conf /etc/ceph/ceph.conf
          sudo ls -l /var/snap/microceph/current/conf
          sudo sh -c 'ln -sf /var/snap/microceph/current/conf/*.keyring /etc/ceph/'
          ls -l /etc/ceph
          trap 'sudo ceph -s || true' EXIT
          # 조건이 맞을 때까지 5초마다, 최대 10분 기다린다.
          wait_for() {
            for _ in $(seq 120); do
              if "$@"; then return 0; fi
              sleep 5
            done
            return 1
          }
          sudo ceph config set global mon_allow_pool_size_one true
          sudo ceph config set global osd_pool_default_size 1
          sudo microceph disk add loop,4G,1
          wait_for sh -c 'sudo ceph osd stat | grep -q " 1 up"'
          sudo ceph osd pool create rbd 32
          sudo ceph osd pool set rbd size 1 --yes-i-really-mean-it
          sudo ceph osd pool set .mgr size 1 --yes-i-really-mean-it || true
          wait_for sh -c "sudo ceph status --format json | jq -e '.pgmap.num_pgs > 0 and ([.pgmap.pgs_by_state[] | select(.state_name | startswith(\"active\")) | .count] | add) == .pgmap.num_pgs'"
          sudo rbd pool init rbd
          sudo rbd create --size 1 rbd/opl-ci-probe
          sudo rbd rm --no-progress rbd/opl-ci-probe
          sudo ceph df
          sudo rbd namespace create rbd/opl
          sudo mkdir -p /etc/opl-ceph
          sudo ceph auth get-or-create client.opltest mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd' \
            | sudo tee /etc/opl-ceph/opltest.keyring >/dev/null
          { sudo cat /etc/ceph/ceph.conf; printf '\n[client.opltest]\nkeyring = /etc/opl-ceph/opltest.keyring\n'; } \
            | sudo tee /etc/opl-ceph/ceph.conf >/dev/null
          sudo rbd --id opltest --conf /etc/opl-ceph/ceph.conf ls --pool rbd --namespace opl
          sudo ceph --id opltest --conf /etc/opl-ceph/ceph.conf df detail >/dev/null
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
          OPL_TEST_LVM: opltest/tp
          OPL_TEST_ZFS: opltestz/opl
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it,opl-driver/rbd-it,opld/rbd-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
          OPL_TEST_LVM: opltest/tp
          OPL_TEST_ZFS: opltestz/opl
          OPL_TEST_RBD: rbd
          OPL_TEST_RBD_USER: opltest
          OPL_TEST_RBD_NAMESPACE: opl
          OPL_TEST_RBD_CONF: /etc/opl-ceph/ceph.conf
```

`crates/opl-driver/Cargo.toml`에서

```toml
zfs-it = []
```

을 다음으로 바꾼다:

```toml
zfs-it = []
# 실제 Ceph RBD 풀의 통합 테스트. Linux에서 root, ceph-common, qemu-block-extra, qemu-img, qemu-io,
# `client.admin`으로 접속할 수 있는 ceph.conf·키링과 테스트가 이미지를 만들고 지워도 되는 Ceph 풀
# (`OPL_TEST_RBD=POOL`)이 필요하다. 제한된 사용자 시험은 `OPL_TEST_RBD_USER`·`OPL_TEST_RBD_NAMESPACE`·
# `OPL_TEST_RBD_CONF`가 있을 때만 돈다.
rbd-it = []
```

`crates/opl-driver/tests/rbd_storage.rs`를 만든다:

```rust
//! 실제 Ceph 풀의 저장소 통합 테스트. 관리자가 만든 Ceph 풀(`OPL_TEST_RBD=POOL`, `rbd pool init`을
//! 마친 것)에 볼륨 이미지를 만들고 가져오고, RBD 스냅샷을 찍어 되돌리고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_RBD=rbd cargo test -p opl-driver --features rbd-it --test rbd_storage`
//! Linux에서 root, ceph-common(`rbd`, `ceph`), qemu-block-extra(rbd 블록 드라이버), qemu-img, qemu-io와
//! `client.admin`으로 접속할 수 있는 `/etc/ceph/ceph.conf`·키링이 필요하다. 제한된 사용자 시험은
//! `OPL_TEST_RBD_USER`(caps `profile rbd`), `OPL_TEST_RBD_NAMESPACE`(그 풀의 네임스페이스),
//! `OPL_TEST_RBD_CONF`(그 사용자의 키링을 가리키는 ceph.conf)가 있을 때만 돈다.
#![cfg(all(feature = "rbd-it", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use opl_driver::{
    DirStorage, DiskLocation, DriverError, RbdStorage, SourceFormat, Storage, Storages, SystemRbd,
};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, RbdTarget};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use serde_json::Value;
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// `OPL_TEST_RBD`의 Ceph 풀
fn ceph_pool() -> String {
    std::env::var("OPL_TEST_RBD").expect("OPL_TEST_RBD must name a Ceph pool")
}

fn rbd_pool() -> StoragePool {
    Resource::new(
        "rbd",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: ceph_pool(),
                namespace: None,
                user: None,
                conf: None,
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

fn storage() -> RbdStorage {
    RbdStorage::new("qemu-img", Arc::new(SystemRbd))
}

fn image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 이미지의 RBD 대상(기본 사용자 `admin`, Ceph 기본 conf)
fn target(id: Uuid) -> RbdTarget {
    RbdTarget {
        pool: ceph_pool(),
        namespace: None,
        image: image(id),
        user: "admin".into(),
        conf: None,
    }
}

/// `rbd` 명령을 실행하고 표준 출력을 돌려준다.
fn rbd(args: &[&str]) -> Vec<u8> {
    let out = Command::new("rbd").args(args).output().expect("run rbd");
    assert!(out.status.success(), "rbd {args:?}: {out:?}");
    out.stdout
}

/// 풀의 이미지와 스냅샷(`image@tag`) 이름 중 `prefix`로 시작하는 것
fn rbd_names(prefix: &str) -> Vec<String> {
    let pool = ceph_pool();
    let images: Vec<String> =
        serde_json::from_slice(&rbd(&["ls", "--format", "json", "--pool", &pool])).unwrap();
    let mut names = vec![];
    for name in images.into_iter().filter(|n| n.starts_with(prefix)) {
        let spec = format!("{pool}/{name}");
        let snaps: Value =
            serde_json::from_slice(&rbd(&["snap", "ls", "--format", "json", &spec])).unwrap();
        for s in snaps.as_array().unwrap() {
            names.push(format!("{name}@{}", s["name"].as_str().unwrap()));
        }
        names.push(name);
    }
    names.sort();
    names
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// 볼륨 이미지(`snapshot`이 있으면 그 스냅샷)에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
/// `read -P`는 패턴이 다르면 실패한다. 읽기는 읽기 전용으로 연다(스냅샷은 쓸 수 없다).
fn qemu_io_rbd(id: Uuid, snapshot: Option<&str>, cmd: &str) -> bool {
    qemu_io_target(&target(id), snapshot, cmd)
}

/// [`qemu_io_rbd`]를 RBD 대상(네임스페이스·사용자·conf)에
fn qemu_io_target(target: &RbdTarget, snapshot: Option<&str>, cmd: &str) -> bool {
    let mut opts = format!("driver=raw,file.driver=rbd,file.pool={}", target.pool);
    if let Some(ns) = &target.namespace {
        opts.push_str(&format!(",file.namespace={ns}"));
    }
    opts.push_str(&format!(
        ",file.image={},file.user={}",
        target.image, target.user
    ));
    if let Some(conf) = &target.conf {
        opts.push_str(&format!(",file.conf={conf}"));
    }
    if let Some(tag) = snapshot {
        opts.push_str(&format!(",file.snapshot={tag}"));
    }
    let mut qemu_io = Command::new("qemu-io");
    if cmd.starts_with("read") {
        qemu_io.arg("-r");
    }
    qemu_io
        .args(["--image-opts", "-c", cmd, &opts])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// 파일 볼륨에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
fn qemu_io_file(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_inspect_resize_and_delete_volume() {
    let pool = rbd_pool();
    let storage = storage();
    let id = Uuid::now_v7();

    // 이미지에는 경로가 없다. 크기는 1 MiB 배수로 올린다.
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, Path::new(""));
    assert_eq!(rbd_names(&image(id)), vec![image(id)]);
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(info.allocated_bytes < info.virtual_bytes, "{info:?}");
    assert!(qemu_io_rbd(id, None, "write -P 0xab 0 1M"));
    let location = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, DiskLocation::Rbd(target(id)));
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    assert!(
        capacity.available_bytes <= capacity.total_bytes,
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

    // 늘리면 내용은 그대로다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지 않는다).
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io_rbd(id, None, "read -P 0xab 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(rbd_names(&image(id)).is_empty());
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
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = rbd_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk
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
    storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, MIB, "{info:?}");
    // 이미지 내용을 파일로 꺼내 비교한다.
    let exported = tmp.path().join("exported.raw");
    rbd(&[
        "export",
        "--no-progress",
        &format!("{}/{}", ceph_pool(), image(id)),
        exported.to_str().unwrap(),
    ]);
    assert!(
        std::fs::read(&exported).unwrap() == pattern,
        "imported contents differ"
    );
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 이미지를 남기지 않는다.
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
    assert!(rbd_names(&image(missing)).is_empty());
}

#[tokio::test]
async fn snapshots_revert_to_any_and_go_with_the_volume() {
    let pool = rbd_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, RAW));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Qcow2));
    let name = image(id);

    assert!(qemu_io_rbd(id, None, "write -P 0xab 0 64k"));
    let s1 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone()]
    );
    assert_eq!(rbd_names(&name), vec![name.clone(), format!("{name}@{s1}")]);
    let dup = storage.snapshot_create(&pool, id, RAW, &s1).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    // 스냅샷은 그때 내용으로 읽힌다.
    assert!(qemu_io_rbd(id, None, "write -P 0xcd 0 64k"));
    assert!(qemu_io_rbd(id, Some(&s1), "read -P 0xab 0 64k"));
    let s2 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io_rbd(id, None, "write -P 0x5a 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );

    // 더 최근 스냅샷이 있어도 앞 스냅샷으로 되돌릴 수 있고, 스냅샷은 모두 남는다.
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xab 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );
    storage.snapshot_revert(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xcd 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, RAW, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s2.clone()]
    );
    // 이미 없어도 성공한다.
    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xcd 0 64k"));

    // 여러 볼륨에 같은 태그를 찍는다(하나씩). 볼륨을 지우면 남은 스냅샷도 함께 지운다.
    let other = Uuid::now_v7();
    storage
        .create_volume(&pool, other, 64 * MIB, RAW)
        .await
        .unwrap();
    storage
        .snapshot_many(&pool, &[id, other], RAW, "both")
        .await
        .unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s2.clone(), "both".to_string()]
    );
    assert_eq!(
        storage.snapshot_tags(&pool, other, RAW).await.unwrap(),
        vec!["both".to_string()]
    );
    for v in [id, other] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        assert!(rbd_names(&image(v)).is_empty());
    }
}

#[tokio::test]
async fn copies_snapshots_within_and_across_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = rbd_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_rbd(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(src, None, "write -P 0xab 0 64k"));
    storages
        .snapshot_create(&pool, src, RAW, "s1")
        .await
        .unwrap();
    assert!(qemu_io_rbd(src, None, "write -P 0xcd 0 64k"));
    // 스냅샷 원본은 경로가 아니라 이미지와 스냅샷 이름이다.
    let snap = storages
        .volume_source(&pool, src, RAW, Some("s1"))
        .await
        .unwrap();
    assert_eq!(snap.rbd, Some(target(src)));
    assert_eq!(snap.snapshot.as_deref(), Some("s1"));
    assert_eq!(snap.size_bytes, 64 * MIB);
    storages
        .release_source(&pool, src, RAW, Some("s1"))
        .await
        .unwrap();

    // 같은 풀 안: 스냅샷 때의 내용을 새 이미지로
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(copy, None, "read -P 0xab 0 64k"));
    let info = storages.volume_info(&pool, copy, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    // 사본에는 스냅샷이 따라오지 않는다.
    assert!(
        storages
            .snapshot_tags(&pool, copy, RAW)
            .await
            .unwrap()
            .is_empty()
    );

    // 디렉터리 풀로: 스냅샷 때의 내용을 qcow2로
    let out = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &local, out, qcow2)
        .await
        .unwrap();
    let out_path = DirStorage::volume_path(&local, Path::new(MNT), out, qcow2);
    assert!(qemu_io_file(&out_path, "qcow2", "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw 이미지, 지금 내용
    assert!(qemu_io_file(&out_path, "qcow2", "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(back, None, "read -P 0x5a 0 64k"));

    // 없는 스냅샷은 실패하고 대상 이미지를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("gone"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(rbd_names(&image(missing)).is_empty());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(rbd_names(&image(id)).is_empty());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}

/// 제한된 사용자(`profile rbd`)가 네임스페이스 안에서 명시적 conf(키링은 그 conf만 가리킨다)로
/// 접속한다: 만들기·용량, QEMU로 쓰기, 스냅샷과 되돌리기, 복사, 지우기.
#[tokio::test]
async fn restricted_user_in_a_namespace_with_its_conf() {
    let var = |name| std::env::var(name).ok();
    let (Some(user), Some(namespace), Some(conf)) = (
        var("OPL_TEST_RBD_USER"),
        var("OPL_TEST_RBD_NAMESPACE"),
        var("OPL_TEST_RBD_CONF"),
    ) else {
        eprintln!(
            "skipped: OPL_TEST_RBD_USER, OPL_TEST_RBD_NAMESPACE and OPL_TEST_RBD_CONF are not set"
        );
        return;
    };
    let pool: StoragePool = Resource::new(
        "limited",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: ceph_pool(),
                namespace: Some(namespace.clone()),
                user: Some(user.clone()),
                conf: Some(conf.clone()),
            },
            nodes: vec![],
            shared: true,
        },
    );
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    let image = match opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap() {
        DiskLocation::Rbd(image) => image,
        other => panic!("not an rbd image: {other:?}"),
    };
    assert_eq!(
        (image.namespace.as_deref(), image.user.as_str()),
        (Some(namespace.as_str()), user.as_str())
    );
    assert_eq!(image.conf.as_deref(), Some(conf.as_str()));
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io_target(&image, None, "write -P 0xab 0 64k"));

    storage.snapshot_create(&pool, id, RAW, "s1").await.unwrap();
    assert!(qemu_io_target(&image, None, "write -P 0xcd 0 64k"));
    assert!(qemu_io_target(&image, Some("s1"), "read -P 0xab 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, "s1").await.unwrap();
    assert!(qemu_io_target(&image, None, "read -P 0xab 0 64k"));
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();

    let copy = Uuid::now_v7();
    storage
        .copy_volume(&pool, id, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    let copied = RbdTarget {
        image: format!("opl-{copy}"),
        ..image.clone()
    };
    assert!(qemu_io_target(&copied, None, "read -P 0xab 0 64k"));

    for v in [id, copy] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        let gone = storage.volume_info(&pool, v, RAW).await;
        assert!(
            matches!(gone, Err(DriverError::VolumeNotFound(_))),
            "{gone:?}"
        );
    }
}
```

`crates/opld/Cargo.toml`에서

```toml
zfs-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

을 다음으로 바꾼다:

```toml
zfs-it = []
# 실제 Ceph RBD 풀의 볼륨(RBD 이미지)으로 VM을 띄우는 E2E 테스트. qemu-it의 준비물에 더해 Linux에서 root,
# ceph-common, qemu-block-extra, qemu-io, `client.admin`으로 접속할 수 있는 ceph.conf·키링과 테스트가
# 이미지를 만들고 지워도 되는 Ceph 풀(`OPL_TEST_RBD=POOL`)이 필요하다.
rbd-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

`crates/opld/tests/rbd.rs`를 만든다:

```rust
//! 실제 Ceph RBD 풀: opld가 관리자가 만든 Ceph 풀을 확인해 준비됐다고 알리고, 그 풀의 raw 볼륨(RBD
//! 이미지)을 QEMU `rbd` 블록 드라이버로 붙인 VM을 띄운다. 실행 중에 디스크 스냅샷(RBD 스냅샷)을 찍고,
//! 멈춘 뒤 더 최근 스냅샷이 있어도 앞 스냅샷으로 되돌리고, 스냅샷에서 디렉터리 풀로 복제하고, 모두 지운다.
//! 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 꺼진 VM의 이미지를 `qemu-io`로 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd`
//! Linux에서 root, ceph-common, qemu-block-extra, QEMU(`qemu-it`와 같다), qemu-img, qemu-io와
//! `client.admin`으로 접속할 수 있는 `/etc/ceph/ceph.conf`·키링이 필요하다. `OPL_TEST_RBD`는 테스트가
//! 이미지를 만들고 지워도 되는 Ceph 풀(`rbd pool init`을 마친 것)이다.
#![cfg(all(feature = "rbd-it", target_os = "linux"))]

mod common;

use std::path::Path;
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

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

/// Task를 돌려주는 요청을 보내고 Task가 성공할 때까지 기다린다.
async fn run(c: &Client, path: &str, body: Value) {
    let task = c.post(path, &body).await.unwrap();
    c.wait_task(&task).await.unwrap();
}

/// `rbd` 명령을 실행하고 표준 출력을 돌려준다.
fn rbd(args: &[&str]) -> Vec<u8> {
    let out = Command::new("rbd").args(args).output().expect("run rbd");
    assert!(out.status.success(), "rbd {args:?}: {out:?}");
    out.stdout
}

/// Ceph 풀의 이미지와 스냅샷(`image@tag`) 이름 중 `prefix`로 시작하는 것
fn rbd_names(pool: &str, prefix: &str) -> Vec<String> {
    let images: Vec<String> =
        serde_json::from_slice(&rbd(&["ls", "--format", "json", "--pool", pool])).unwrap();
    let mut names = vec![];
    for name in images.into_iter().filter(|n| n.starts_with(prefix)) {
        let spec = format!("{pool}/{name}");
        let snaps: Value =
            serde_json::from_slice(&rbd(&["snap", "ls", "--format", "json", &spec])).unwrap();
        for s in snaps.as_array().unwrap() {
            names.push(format!("{name}@{}", s["name"].as_str().unwrap()));
        }
        names.push(name);
    }
    names.sort();
    names
}

/// 이미지에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io_rbd(pool: &str, image: &str, cmd: &str) -> bool {
    let opts = format!("driver=raw,file.driver=rbd,file.pool={pool},file.image={image}");
    Command::new("qemu-io")
        .args(["--image-opts", "-c", cmd, &opts])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// 파일 볼륨에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
fn qemu_io_file(path: &Path, format: &str, cmd: &str) -> bool {
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

#[tokio::test]
async fn rbd_pool_runs_snapshots_reverts_and_clones_a_vm() {
    let pool = std::env::var("OPL_TEST_RBD").expect("OPL_TEST_RBD must name a Ceph pool");
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("rbd").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 Ceph 풀과 QEMU의 rbd 드라이버를 확인하면 준비됐다고 알리고 풀의 용량을 보인다.
    // RBD 풀은 늘 공유다.
    let backend = json!({ "kind": "rbd", "pool": pool });
    let created = c
        .post(
            "/storage-pools",
            &json!({ "name": "ceph", "spec": { "backend": backend } }),
        )
        .await
        .unwrap();
    assert_eq!(created["spec"]["shared"], true, "{created}");
    d.wait_pool_ready("ceph").await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["ceph"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨은 RBD 이미지 `opl-{id}`다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "ceph", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let image = format!("opl-{}", volume["id"].as_str().unwrap());
    assert_eq!(rbd_names(&pool, &image), vec![image.clone()]);
    assert!(qemu_io_rbd(&pool, &image, "write -P 0xab 0 64k"));

    // VM은 이미지를 QEMU의 rbd 드라이버로 연다(커널 매핑 없음).
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
        args.contains(&format!(
            "driver=rbd,pool={pool},image={image},user=admin,node-name=disk0-file"
        )),
        "{args}"
    );

    // RBD 디스크는 메모리를 담을 수 없다.
    let resp = c
        .request(Method::POST, "/vms/web/snapshots")
        .json(&json!({ "name": "mem", "memory": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");

    // 실행 중 스냅샷은 디스크만 담는다. VM은 잠깐 멈췄다가 같은 프로세스로 계속 돈다.
    run(&c, "/vms/web/snapshots", json!({ "name": "before" })).await;
    let before = c.get("/vms/web/snapshots/before").await.unwrap();
    assert_eq!(before["status"]["phase"], "ready", "{before}");
    assert_eq!(before["spec"]["memory"], false, "{before}");
    let before_snapshot = format!("{image}@{}", before["id"].as_str().unwrap());
    assert_eq!(
        rbd_names(&pool, &image),
        vec![image.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");

    // 멈춘 뒤 디스크를 바꾸고 스냅샷을 하나 더 찍어도 앞 스냅샷으로 되돌릴 수 있다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io_rbd(&pool, &image, "write -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let after = c.get("/vms/web/snapshots/after").await.unwrap();
    let after_snapshot = format!("{image}@{}", after["id"].as_str().unwrap());
    assert!(qemu_io_rbd(&pool, &image, "write -P 0x5a 0 64k"));
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io_rbd(&pool, &image, "read -P 0xab 0 64k"));
    let mut both = vec![image.clone(), before_snapshot.clone(), after_snapshot];
    both.sort();
    assert_eq!(rbd_names(&pool, &image), both);
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "stopped", "{web}");

    // 더 최근 스냅샷으로도 되돌아갈 수 있다.
    d.ctl(&["vm", "snapshot", "revert", "web", "after"])
        .await
        .unwrap();
    assert!(qemu_io_rbd(&pool, &image, "read -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "delete", "web", "after"])
        .await
        .unwrap();
    assert_eq!(
        rbd_names(&pool, &image),
        vec![image.clone(), before_snapshot.clone()]
    );

    // 되돌린 디스크로 다시 켜진다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();

    // 스냅샷에서 디렉터리 풀로 복제하면 새 디스크는 qcow2 파일이고 스냅샷 때 내용을 담는다.
    let dir = d.path("pool");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&[
        "vm",
        "clone",
        "web",
        "copy",
        "--snapshot",
        "before",
        "--pool",
        "local",
    ])
    .await
    .unwrap();
    let copy_volume = c.get("/volumes/copy-root").await.unwrap();
    assert_eq!(copy_volume["status"]["phase"], "ready", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["pool"], "local", "{copy_volume}");
    assert_eq!(copy_volume["spec"]["format"], "qcow2", "{copy_volume}");
    let copy_file = dir.join(format!("{}.qcow2", copy_volume["id"].as_str().unwrap()));
    assert!(qemu_io_file(&copy_file, "qcow2", "read -P 0xab 0 64k"));

    // 스냅샷과 함께 VM을 지우면 RBD 스냅샷이, 볼륨을 지우면 이미지가 없어진다.
    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    assert!(!copy_file.exists());
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert_eq!(rbd_names(&pool, &image), vec![image.clone()]);
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(rbd_names(&pool, &image).is_empty());
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "ceph"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 600개, 실패 0

Run: `cargo clippy -p opl-driver -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음(macOS에서는 rbd 시험 파일이 비어 있다)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "test(storage): 실제 Ceph RBD 풀 통합 시험과 CI"
```

---

### Task 7: oplctl: RBD 풀

`pool create NAME --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`(`PoolCmd::Create(Box<PoolCreate>)`로 옮김, 다른 종류·`--shared`와 함께 못 씀, 본문은 `pool_create_body`), 목록·상세의 종류 `rbd`·위치·사용자·conf, 형식 기본값 raw.

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
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀을 만든다.
    Create(Box<PoolCreate>),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd"])))]
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
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
}

/// `pool create`의 요청 본문. NFS·RBD 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
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
        shared,
        nodes,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd) {
        (Some(n), _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r)) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None) => json!({ "kind": "dir", "path": path }),
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
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD는 raw, 그 밖은 qcow2).
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
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD는 raw, 그 밖은 qcow2).
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

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD)은 raw(스냅샷은 블록 스냅샷), 파일 풀은
/// qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" {
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
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
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
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in ["--path", "--nfs", "--lvm-thin", "--zfs", "--rbd"] {
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
Expected: PASS 602개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): Ceph RBD 풀 만들기와 RBD 풀의 볼륨 형식 기본값 raw"
```

---

### Task 8: 웹: RBD 풀

풀 만들기의 Ceph RBD(풀·네임스페이스·사용자·conf — `cephNameError`·`cephUserError`·`rbdConfError`는 서버와 같은 규칙), 목록 종류·위치, RBD는 항상 공유, `blockSnapshotKind`에 rbd(raw만, 블록 스냅샷 판단). en·ko.

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
    "zfsLatest": "Disks in ZFS pools can only revert to their latest snapshot."
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
    "createDescription": "Where volumes are kept: a directory on each node, an NFS export that the nodes mount, an LVM thin pool or ZFS dataset on each node, or a Ceph RBD pool.",
    "pathHint": "An absolute path on the nodes.",
    "shared": "Shared storage",
    "sharedHint": "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export are kept.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "kind": "Kind",
    "kinds": {
      "dir": "Directory",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD"
    },
    "location": "Location",
    "kindHint": {
      "dir": "Create the directory on each node first.",
      "nfs": "Each node mounts the export under its data directory. Mounting needs Linux nodes running as root. The pool is shared by its nodes.",
      "lvm-thin": "Create the volume group and thin pool on each node first. Volumes are raw thin volumes kept on the node that creates them, and their snapshots hold disks only. Needs Linux nodes running as root.",
      "zfs": "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root.",
      "rbd": "Create the Ceph pool first and put ceph.conf and the keyring on each node. Volumes are raw RBD images that every node of the pool can use, and their snapshots hold disks only. Needs Linux nodes with QEMU's rbd driver (qemu-block-extra) and the rbd command (ceph-common)."
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
      "rbdConf": "Enter an absolute path (starting with /) without control characters, commas, colons or equals signs."
    },
    "vg": "Volume group",
    "thinpool": "Thin pool",
    "dataset": "Dataset",
    "cephPool": "Ceph pool",
    "namespace": "Namespace (optional)",
    "cephUser": "Ceph user (optional)",
    "cephUserHint": "The Ceph client ID without the \"client.\" prefix. Uses admin if empty.",
    "cephConf": "ceph.conf path (optional)",
    "cephConfHint": "An absolute path on the nodes, /etc/ceph/ceph.conf if empty."
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
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail."
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
    "zfsLatest": "ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다."
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
    "createDescription": "볼륨을 두는 곳입니다: 각 노드의 디렉터리, 노드들이 마운트하는 NFS 내보내기, 각 노드의 LVM thin pool이나 ZFS 데이터셋, 또는 Ceph RBD 풀.",
    "pathHint": "노드의 절대 경로입니다.",
    "shared": "공유 스토리지",
    "sharedHint": "모든 노드가 이 경로에서 같은 파일을 보면 고르세요(예: opLite 밖에서 마운트한 NFS 공유).",
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일은 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "kind": "종류",
    "kinds": {
      "dir": "디렉터리",
      "nfs": "NFS",
      "lvm-thin": "LVM-thin",
      "zfs": "ZFS",
      "rbd": "Ceph RBD"
    },
    "location": "위치",
    "kindHint": {
      "dir": "먼저 각 노드에 디렉터리를 만드세요.",
      "nfs": "각 노드가 내보내기를 데이터 디렉터리 아래에 마운트합니다. 마운트는 root로 도는 Linux 노드에서만 됩니다. 풀의 노드들이 함께 쓰는 공유 풀입니다.",
      "lvm-thin": "먼저 각 노드에 볼륨 그룹과 thin pool을 만드세요. 볼륨은 만든 노드에 두는 raw thin 볼륨이고, 스냅샷은 디스크만 담습니다. root로 도는 Linux 노드에서만 됩니다.",
      "zfs": "먼저 각 노드에 데이터셋을 만드세요. 볼륨은 만든 노드에 두는 raw zvol이고, 스냅샷은 디스크만 담습니다. 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다. root로 도는 Linux 노드에서만 됩니다.",
      "rbd": "먼저 Ceph 풀을 만들고 각 노드에 ceph.conf와 키링을 두세요. 볼륨은 풀의 모든 노드가 쓰는 raw RBD 이미지이고, 스냅샷은 디스크만 담습니다. QEMU rbd 드라이버(qemu-block-extra)와 rbd 명령(ceph-common)이 있는 Linux 노드에서만 됩니다."
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
      "rbdConf": "제어 문자·쉼표·콜론·등호 없는 절대 경로(/로 시작)를 입력하세요."
    },
    "vg": "볼륨 그룹",
    "thinpool": "Thin pool",
    "dataset": "데이터셋",
    "cephPool": "Ceph 풀",
    "namespace": "네임스페이스(선택)",
    "cephUser": "Ceph 사용자(선택)",
    "cephUserHint": "client. 없는 Ceph 클라이언트 ID입니다. 비우면 admin입니다.",
    "cephConf": "ceph.conf 경로(선택)",
    "cephConfHint": "노드의 절대 경로입니다. 비우면 /etc/ceph/ceph.conf입니다."
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
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀, Ceph RBD 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다."
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
      pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } }),
    ],
```

을 다음으로 바꾼다:

```tsx
      pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } }),
      pool("ceph", { backend: { kind: "rbd", pool: "rbd", namespace: "opl" } }),
    ],
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(tank).toHaveTextContent("tank/opl");
});
```

을 다음으로 바꾼다:

```tsx
  expect(tank).toHaveTextContent("tank/opl");
  const ceph = within(table).getByText("ceph").closest("tr")!;
  expect(ceph).toHaveTextContent("Ceph RBD");
  expect(ceph).toHaveTextContent("rbd/opl");
});
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx

it("sends NFS mount options", async () => {
```

을 다음으로 바꾼다:

```tsx

it("creates a Ceph RBD pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("ceph"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "ceph");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "rbd");
  expect(within(dialog).getByLabelText("Kind")).toHaveAccessibleDescription(
    "Create the Ceph pool first and put ceph.conf and the keyring on each node. Volumes are raw RBD images that every node of the pool can use, and their snapshots hold disks only. Needs Linux nodes with QEMU's rbd driver (qemu-block-extra) and the rbd command (ceph-common).",
  );
  // 늘 공유라 고를 것이 없고, 다른 종류의 입력도 없다.
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  expect(within(dialog).queryByLabelText("Directory")).toBeNull();
  expect(within(dialog).queryByLabelText("Dataset")).toBeNull();
  expect(within(dialog).getByLabelText("Ceph user (optional)")).toHaveAccessibleDescription(
    'The Ceph client ID without the "client." prefix. Uses admin if empty.',
  );
  expect(within(dialog).getByLabelText("ceph.conf path (optional)")).toHaveAccessibleDescription(
    "An absolute path on the nodes, /etc/ceph/ceph.conf if empty.",
  );
  // 서버처럼 이름과 conf 경로를 검사한다.
  await userEvent.type(within(dialog).getByLabelText("Ceph pool"), "rbd/opl");
  await userEvent.type(within(dialog).getByLabelText("Namespace (optional)"), "-opl");
  await userEvent.type(within(dialog).getByLabelText("Ceph user (optional)"), "opl user");
  await userEvent.type(within(dialog).getByLabelText("ceph.conf path (optional)"), "ceph.conf");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  const nameError = "Use 1 to 63 letters, digits, _, . or -, not starting with -.";
  expect(within(dialog).getByLabelText("Ceph pool")).toHaveAccessibleDescription(nameError);
  expect(within(dialog).getByLabelText("Namespace (optional)")).toHaveAccessibleDescription(
    nameError,
  );
  expect(within(dialog).getByLabelText("Ceph user (optional)")).toHaveAccessibleDescription(
    nameError,
  );
  expect(within(dialog).getByLabelText("ceph.conf path (optional)")).toHaveAccessibleDescription(
    "Enter an absolute path (starting with /) without control characters, commas, colons or equals signs.",
  );
  expect(body).toBeUndefined();
  // 사용자는 `client.` 없이 쓴다.
  await userEvent.clear(within(dialog).getByLabelText("Ceph user (optional)"));
  await userEvent.type(within(dialog).getByLabelText("Ceph user (optional)"), "client.opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Ceph user (optional)")).toHaveAccessibleDescription(
    'Write the Ceph user without the "client." prefix, for example opl instead of client.opl.',
  );
  expect(body).toBeUndefined();

  // 선택 입력을 비우면 보내지 않는다.
  await userEvent.clear(within(dialog).getByLabelText("Ceph pool"));
  await userEvent.type(within(dialog).getByLabelText("Ceph pool"), "rbd");
  await userEvent.clear(within(dialog).getByLabelText("Namespace (optional)"));
  await userEvent.clear(within(dialog).getByLabelText("Ceph user (optional)"));
  await userEvent.clear(within(dialog).getByLabelText("ceph.conf path (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool ceph.")).toBeInTheDocument();
  // 공유 여부는 서버가 정한다.
  expect(body).toEqual({
    name: "ceph",
    spec: { backend: { kind: "rbd", pool: "rbd" }, nodes: [] },
  });
});

it("creates a Ceph RBD pool with a namespace, user and ceph.conf", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("ceph"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "ceph");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "rbd");
  await userEvent.type(within(dialog).getByLabelText("Ceph pool"), "rbd");
  await userEvent.type(within(dialog).getByLabelText("Namespace (optional)"), "opl");
  await userEvent.type(within(dialog).getByLabelText("Ceph user (optional)"), "opl");
  await userEvent.type(
    within(dialog).getByLabelText("ceph.conf path (optional)"),
    "/etc/ceph/opl.conf",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "ceph",
      spec: {
        backend: {
          kind: "rbd",
          pool: "rbd",
          namespace: "opl",
          user: "opl",
          conf: "/etc/ceph/opl.conf",
        },
        nodes: [],
      },
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
    // NFS·RBD 풀의 `shared`는 보내지 않는다(서버가 늘 공유로 둔다). 빈 옵션은 커널 기본값이다.
    // LVM-thin·ZFS 풀은 노드 로컬이라 `shared`를 보내지 않는다.
    const sharedDir = kind === "dir" && shared;
    const backend =
      kind === "dir"
        ? { kind, path }
        : kind === "lvm-thin"
          ? { kind, vg, thinpool }
          : kind === "zfs"
            ? { kind, dataset }
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

`web/src/storage/pools.test.ts` 전체:

```ts
import {
  cephNameError,
  cephUserError,
  lvmNameError,
  nfsServerError,
  nfsExportError,
  nfsOptionsError,
  poolCapacity,
  poolLocation,
  poolFormats,
  poolNodes,
  poolsUsableOn,
  poolUsableSomewhere,
  rbdConfError,
  zfsDatasetError,
} from "@/storage/pools";
import { NODE, pool, poolState } from "@/test/fixtures";

const GIB = 1024 ** 3;
const nfs = (server: string) =>
  pool("nas", { backend: { kind: "nfs", server, export: "/srv/vm" }, shared: true });
const lvm = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const zfs = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const rbd = (namespace?: string) =>
  pool("ceph", { backend: { kind: "rbd", pool: "rbd", ...(namespace && { namespace }) } });
const withPool = (
  id: string,
  name: string,
  state: Parameters<typeof poolState>[1],
  online = true,
) => ({
  ...NODE,
  id,
  name,
  status: {
    ...NODE.status,
    online,
    pools: { local: poolState("local", state), nas: poolState("nas", state) },
  },
});

it("shows where a pool keeps its files", () => {
  expect(poolLocation(pool("local"))).toBe("/srv/local");
  expect(poolLocation(nfs("nas.lan"))).toBe("nas.lan:/srv/vm");
  // IPv6 주소는 mount처럼 대괄호로 감싼다.
  expect(poolLocation(nfs("fd00::5"))).toBe("[fd00::5]:/srv/vm");
  expect(poolLocation(lvm)).toBe("vg0/tp");
  expect(poolLocation(zfs)).toBe("tank/opl");
  expect(poolLocation(rbd())).toBe("rbd");
  expect(poolLocation(rbd("opl"))).toBe("rbd/opl");
});

it("checks LVM names like the server", () => {
  for (const ok of ["vg0", "VG_data", "a+b.c-d", "x".repeat(127), "...", "my_snapshot", "tdata"]) {
    expect(lvmNameError(ok)).toBeUndefined();
  }
  for (const bad of ["", "-vg", "vg 0", "vg/tp", "볼륨", "x".repeat(128)]) {
    expect(lvmNameError(bad)).toBe("pools.errors.lvmName");
  }
  // LVM이 받지 않는 이름: `.`·`..`, `snapshot`·`pvmove`로 시작, 내부 LV의 접미어 포함
  for (const reserved of [
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
  ]) {
    expect(lvmNameError(reserved)).toBe("pools.errors.lvmName");
  }
});

it("checks ZFS datasets like the server", () => {
  // zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한 255자를 넘지 않게 177자까지다.
  const long = `t/${"a".repeat(175)}`;
  for (const ok of ["tank", "tank/opl", "Tank0/a_b.c:d-e/f", "t/x-", "t/.x", "t/9", long]) {
    expect(zfsDatasetError(ok)).toBeUndefined();
  }
  for (const bad of [
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
    `${long}a`,
  ]) {
    expect(zfsDatasetError(bad)).toBe("pools.errors.zfsDataset");
  }
});

it("checks Ceph names like the server", () => {
  for (const ok of ["rbd", "Opl_vm.1-a", "9", "a".repeat(63)]) {
    expect(cephNameError(ok)).toBeUndefined();
  }
  // `-`로 시작하면 명령 옵션으로 읽힐 수 있다.
  for (const bad of [
    "",
    "-rbd",
    "rbd/opl",
    "rbd@s",
    "a,b",
    "a:b",
    "a=b",
    "my rbd",
    "데이터",
    "a".repeat(64),
  ]) {
    expect(cephNameError(bad)).toBe("pools.errors.rbdName");
  }
});

it("checks Ceph users like the server, without the client. prefix", () => {
  expect(cephUserError("opl")).toBeUndefined();
  expect(cephUserError("clients")).toBeUndefined();
  expect(cephUserError("client.opl")).toBe("pools.errors.cephUser");
  expect(cephUserError("client.admin")).toBe("pools.errors.cephUser");
  expect(cephUserError("-opl")).toBe("pools.errors.rbdName");
});

it("checks ceph.conf paths like the server", () => {
  expect(rbdConfError("/etc/ceph/ceph.conf")).toBeUndefined();
  expect(rbdConfError("/etc/ceph/my ceph.conf")).toBeUndefined();
  // conf는 QEMU 옵션 문자열(`conf=...`)에 들어간다.
  for (const bad of [
    "",
    "ceph.conf",
    "/etc/ceph/a,b.conf",
    "/etc/ceph/a:b.conf",
    "/etc/ceph/a=b.conf",
    "/etc/ceph/\nceph.conf",
  ]) {
    expect(rbdConfError(bad)).toBe("pools.errors.rbdConf");
  }
});

it("offers the volume formats a pool holds, the default first", () => {
  expect(poolFormats(pool("local"))).toEqual(["qcow2", "raw"]);
  expect(poolFormats(nfs("nas.lan"))).toEqual(["qcow2", "raw"]);
  // LVM-thin 풀은 raw 볼륨만 둔다(서버가 qcow2를 거절한다).
  expect(poolFormats(lvm)).toEqual(["raw"]);
  // ZFS 풀도 raw zvol만 둔다.
  expect(poolFormats(zfs)).toEqual(["raw"]);
  // RBD 풀도 raw 이미지만 둔다.
  expect(poolFormats(rbd())).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
  expect(poolFormats(undefined)).toEqual(["qcow2", "raw"]);
});

it("checks NFS servers like the server", () => {
  for (const ok of ["nas.lan", "10.0.0.5", "fd00::5", "NAS-01"]) {
    expect(nfsServerError(ok)).toBeUndefined();
  }
  for (const bad of ["", "nas lan", "a,b", "[fd00::5]", "-oremount", "a/b"]) {
    expect(nfsServerError(bad)).toBe("pools.errors.server");
  }
});

it("checks NFS exports like the server", () => {
  expect(nfsExportError("/srv/vm")).toBeUndefined();
  for (const bad of ["", "srv/vm", "/srv/\nvm", "/srv/\u007fvm"]) {
    expect(nfsExportError(bad)).toBe("pools.errors.export");
  }
});

it("refuses mount options that change what the mount is, like the server", () => {
  for (const ok of ["vers=4.2,hard", "nosuid,nodev,noexec", "soft,timeo=600", "nconnect=4"]) {
    expect(nfsOptionsError(ok)).toBeUndefined();
  }
  for (const bad of [
    "",
    "vers=4.2, hard",
    "hard\u0001",
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
    "bg",
    "hard,BG",
  ]) {
    expect(nfsOptionsError(bad)).toBe("pools.errors.options");
  }
});

it("lists the nodes a pool may use", () => {
  const b = withPool("n2", "node-b", {});
  expect(poolNodes(pool("local"), [NODE, b]).map((n) => n.name)).toEqual(["node-a", "node-b"]);
  expect(poolNodes(pool("local", { nodes: ["n2"] }), [NODE, b]).map((n) => n.name)).toEqual([
    "node-b",
  ]);
  // 목록에 없는 노드 id는 id 끝자리로 보인다.
  expect(poolNodes(pool("local", { nodes: ["gone-node-12345678"] }), [NODE])).toEqual([
    { id: "gone-node-12345678", name: "12345678", state: undefined, offline: false },
  ]);
  // 꺼진 노드는 오프라인으로 보인다.
  const down = withPool("n3", "node-c", {}, false);
  expect(poolNodes(pool("local"), [down])[0]?.offline).toBe(true);
  // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
  const stale = withPool("n2", "node-b", { pool_id: "019a0f3c-0000-7000-8001-0000000000ff" });
  expect(poolNodes(pool("local"), [stale])[0]?.state).toBeUndefined();
});

it("takes the capacity from online nodes where the pool is ready", () => {
  const a = withPool("n1", "node-a", { ready: false, error: "mount failed" });
  const b = withPool("n2", "node-b", { available_bytes: 10 * GIB });
  const c = withPool("n3", "node-c", { available_bytes: 20 * GIB });
  const down = withPool("n4", "node-d", { available_bytes: 30 * GIB }, false);
  // 공유 풀은 첫 준비된 온라인 노드의 값 하나다.
  expect(poolCapacity(nfs("nas"), [down, a, b, c])).toEqual([
    { node: null, free: 10 * GIB, total: 128 * GIB },
  ]);
  // 로컬 풀은 준비된 노드마다 값이 다르다.
  expect(poolCapacity(pool("local"), [a, b, c, down])).toEqual([
    { node: "node-b", free: 10 * GIB, total: 128 * GIB },
    { node: "node-c", free: 20 * GIB, total: 128 * GIB },
  ]);
  expect(poolCapacity(pool("local"), [a])).toEqual([]);
  // RBD 풀은 `shared`가 없어도 늘 공유다(서버의 `is_shared`).
  const ceph = (id: string, name: string) => ({
    ...NODE,
    id,
    name,
    status: { ...NODE.status, pools: { ceph: poolState("ceph", { available_bytes: GIB }) } },
  });
  expect(poolCapacity(rbd(), [ceph("n1", "node-a"), ceph("n2", "node-b")])).toEqual([
    { node: null, free: GIB, total: 128 * GIB },
  ]);
});

it("offers the pools a node can use, like the server's pool_usable_on", () => {
  const nas = nfs("nas.lan");
  const unknown = { ...NODE, status: { ...NODE.status, pools: {} } };
  const pools = [pool("local"), pool("far", { nodes: ["n2"] }), nas];
  // 상태를 알리지 않은 노드에서도 디렉터리 풀은 쓸 수 있고, NFS 풀은 쓸 수 없다.
  expect(poolsUsableOn(pools, [unknown], "n1").map((p) => p.name)).toEqual(["local"]);
  // 준비됐다고 알리면 NFS 풀도, 준비되지 않았다고 알리면 디렉터리 풀도 그렇다.
  const ready = withPool("n1", "node-a", {});
  expect(poolsUsableOn(pools, [ready], "n1").map((p) => p.name)).toEqual(["local", "nas"]);
  const down = withPool("n1", "node-a", { ready: false, error: "x" });
  expect(poolsUsableOn(pools, [down], "n1")).toEqual([]);
  // 다른 풀(같은 이름)의 상태는 알리지 않은 것으로 본다.
  const stale = withPool("n1", "node-a", { pool_id: "019a0f3c-0000-7000-8001-0000000000ff" });
  expect(poolsUsableOn(pools, [stale], "n1").map((p) => p.name)).toEqual(["local"]);
  // 꺼진 노드, 모르는 노드에서는 아무 풀도 쓸 수 없다.
  expect(poolsUsableOn(pools, [withPool("n1", "node-a", {}, false)], "n1")).toEqual([]);
  expect(poolsUsableOn(pools, [ready], undefined)).toEqual([]);
  expect(poolsUsableOn(pools, [ready], "n9")).toEqual([]);

  expect(poolUsableSomewhere(nas, [unknown, withPool("n2", "node-b", {}, false)])).toBe(false);
  expect(poolUsableSomewhere(nas, [unknown, withPool("n2", "node-b", {})])).toBe(true);

  // LVM-thin 풀도 NFS 풀처럼 준비됐다고 알린 노드에서만 쓴다.
  const fast = (state: Parameters<typeof poolState>[1]) => ({
    ...NODE,
    status: { ...NODE.status, pools: { fast: poolState("fast", state) } },
  });
  expect(poolsUsableOn([lvm], [unknown], "n1")).toEqual([]);
  expect(poolsUsableOn([lvm], [fast({})], "n1")).toEqual([lvm]);
  expect(poolsUsableOn([lvm], [fast({ ready: false, error: "x" })], "n1")).toEqual([]);
});
```

`web/src/storage/pools.ts`에서

```ts

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  return pool.spec.shared === true || pool.spec.backend.kind === "nfs";
}
```

을 다음으로 바꾼다:

```ts

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS·RBD 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  const kind = pool.spec.backend.kind;
  return pool.spec.shared === true || kind === "nfs" || kind === "rbd";
}
```

`web/src/storage/pools.ts`에서

```ts

/** 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS)의 종류. 아니면(파일 풀, 모름) `undefined`. */
export function blockSnapshotKind(pool: StoragePool | undefined): "lvm-thin" | "zfs" | undefined {
  const kind = pool?.spec.backend.kind;
  return kind === "lvm-thin" || kind === "zfs" ? kind : undefined;
}
```

을 다음으로 바꾼다:

```ts

/**
 * Ceph 이름(풀, 네임스페이스, 사용자) 입력 오류(번역 키). 서버처럼 `A-Za-z0-9_.-`의 1–63자이고
 * `-`로 시작하지 않아야 한다.
 */
export function cephNameError(name: string): string | undefined {
  return /^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,62}$/.test(name) ? undefined : "pools.errors.rbdName";
}

/**
 * Ceph 사용자 입력 오류(번역 키). 이름 규칙([`cephNameError`])에 더해, 서버처럼 `client.` 없이 써야
 * 한다(`rbd --id`와 QEMU가 접두어 없는 id를 받는다).
 */
export function cephUserError(user: string): string | undefined {
  return cephNameError(user) ?? (user.startsWith("client.") ? "pools.errors.cephUser" : undefined);
}

/**
 * ceph.conf 경로 입력 오류(번역 키). 서버처럼 절대 경로이고 제어 문자와 `,`·`:`·`=`가 없어야
 * 한다(QEMU 옵션 문자열에 들어간다).
 */
export function rbdConfError(conf: string): string | undefined {
  return !conf.startsWith("/") || hasControl(conf) || /[,:=]/.test(conf)
    ? "pools.errors.rbdConf"
    : undefined;
}

/** 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS, RBD)의 종류. 아니면(파일 풀, 모름) `undefined`. */
export function blockSnapshotKind(
  pool: StoragePool | undefined,
): "lvm-thin" | "zfs" | "rbd" | undefined {
  const kind = pool?.spec.backend.kind;
  return kind === "lvm-thin" || kind === "zfs" || kind === "rbd" ? kind : undefined;
}
```

`web/src/storage/pools.ts`에서

```ts
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). 블록 풀(LVM-thin, ZFS)은 raw만 둔다. 풀을 모르면 파일 풀처럼 본다.
 */
```

을 다음으로 바꾼다:

```ts
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). 블록 풀(LVM-thin, ZFS, RBD)은 raw만 둔다. 풀을 모르면 파일 풀처럼
 * 본다.
 */
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
  );
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    screen.getByText(
      "web-data is raw, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
    ),
```

을 다음으로 바꾼다:

```tsx
    screen.getByText(
      "web-data is raw, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools.",
    ),
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    await screen.findByText(
      "Disks in LVM-thin and ZFS pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Disks in LVM-thin, ZFS and Ceph RBD pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    screen.getByText(
      "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools) are mixed, so scheduled snapshots will fail.",
    ),
```

을 다음으로 바꾼다:

```tsx
    screen.getByText(
      "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools, Ceph RBD pools) are mixed, so scheduled snapshots will fail.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. web-root is raw.",
    ),
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. web-root is raw.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    within(dialog).getByText(
      "Disks in LVM-thin and ZFS pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

을 다음으로 바꾼다:

```tsx
    within(dialog).getByText(
      "Disks in LVM-thin, ZFS and Ceph RBD pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. This VM mixes them.",
    ),
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. This VM mixes them.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. Snapshots of disks in LVM-thin or ZFS pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
  ],
```

을 다음으로 바꾼다:

```tsx
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. Snapshots of disks in LVM-thin, ZFS or Ceph RBD pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
  ],
```

`web/src/vms/VmSnapshots.tsx`에서

```tsx
  taken: string[];
  /** 디스크가 블록 스냅샷(LVM-thin, ZFS)을 쓴다(메모리를 담지 못한다). */
  block: boolean;
```

을 다음으로 바꾼다:

```tsx
  taken: string[];
  /** 디스크가 블록 스냅샷(LVM-thin, ZFS, RBD)을 쓴다(메모리를 담지 못한다). */
  block: boolean;
```

`web/src/vms/snapshots.test.ts`에서

```ts
const tank = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const pools = [local, fast, tank];
const raw = (name: string, poolName: string) => {
```

을 다음으로 바꾼다:

```ts
const tank = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const ceph = pool("ceph", { backend: { kind: "rbd", pool: "rbd" } });
const pools = [local, fast, tank, ceph];
const raw = (name: string, poolName: string) => {
```

`web/src/vms/snapshots.test.ts`에서

```ts
  expect(snapshotSupport([qcow])).toEqual({ kind: "format", disk: qcow });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
```

을 다음으로 바꾼다:

```ts
  expect(snapshotSupport([qcow])).toEqual({ kind: "format", disk: qcow });
  // RBD 풀도 raw 블록 스냅샷이다.
  expect(snapshotSupport([disk(raw("a", "ceph")), disk(raw("b", "ceph"))])).toEqual({
    kind: "ok",
    block: true,
  });
  const image = disk(volume("i", "ceph"));
  expect(snapshotSupport([image])).toEqual({ kind: "format", disk: image });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
```

`web/src/vms/snapshots.test.ts`에서

```ts
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "tank"))])).toEqual({
    kind: "mixed",
```

을 다음으로 바꾼다:

```ts
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "tank"))])).toEqual({
    kind: "mixed",
  });
  expect(snapshotSupport([disk(raw("a", "tank")), disk(raw("b", "ceph"))])).toEqual({
    kind: "mixed",
```

`web/src/vms/snapshots.test.ts`에서

```ts
  expect(notLatest(old, all, [raw("z-root", "fast")])).toBe(false);
  // 목록에 없는 볼륨은 건너뛴다.
```

을 다음으로 바꾼다:

```ts
  expect(notLatest(old, all, [raw("z-root", "fast")])).toBe(false);
  // RBD 디스크는 아무 스냅샷으로나 되돌린다.
  expect(notLatest(old, all, [raw("z-root", "ceph")])).toBe(false);
  // 목록에 없는 볼륨은 건너뛴다.
```

`web/src/vms/snapshots.ts`에서

```ts
/**
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS, 메모리 없음)인지,
 * `format`은 풀 종류가 담지 못하는 형식의 디스크, `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드
```

을 다음으로 바꾼다:

```ts
/**
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS·RBD, 메모리 없음)인지,
 * `format`은 풀 종류가 담지 못하는 형식의 디스크, `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드
```

`web/src/vms/snapshots.ts`에서

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, 블록 풀(LVM-thin, ZFS)의 디스크는
 * raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
```

을 다음으로 바꾼다:

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, 블록 풀(LVM-thin, ZFS, RBD)의 디스크는
 * raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 291개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): Ceph RBD 풀 만들기와 RBD 디스크의 블록 스냅샷 판단"
```

---

### Task 9: 문서

README에 "Ceph RBD 풀"(준비, 공유·raw, 이름과 접근, 노드별 상태, 스냅샷, 지우기·복제·크기, `rbd-it`과 microceph 준비)과 문서 목록, 설계 문서 "구현:" 메모, NFS 설계 §1의 계획 18(RBD)·19(iSCSI), 코드 주석의 블록 백엔드 목록, 웹 풀 지우기 안내.

**Files:**
- Modify: `README.md`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md`
- Modify: `docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS 풀)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD 풀)
```

`README.md`에서

```markdown
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다. 볼륨이 블록 장치인
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에 있다.
```

을 다음으로 바꾼다:

```markdown
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다. 볼륨이 블록 장치인
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에, 볼륨이 Ceph RBD 이미지인
**Ceph RBD 풀**(`--rbd`)은 [그 뒤](#ceph-rbd-풀)에 있다.
```

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## Ceph RBD 풀

**Ceph RBD 풀**(`--rbd POOL[/NAMESPACE]`)은 Ceph 클러스터의 풀(과 그 안의 RBD 네임스페이스)이다. 볼륨마다 raw RBD
이미지 `opl-{볼륨 id}`를 만들고(`--image-format 2`) QEMU가 `rbd` 블록 드라이버로 Ceph에 직접 붙는다(커널 매핑
`rbd map`을 하지 않으므로 노드에 블록 장치가 생기지 않는다). 모든 노드가 같은 이미지를 보는 **공유 풀**이다. 스냅샷
규칙과 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과 같다. 다른 점만 적는다.

```bash
# Ceph 클러스터와 풀은 관리자가 미리 만든다(opLite는 만들지 않는다).
ceph osd pool create rbd && rbd pool init rbd
# 노드마다: rbd·ceph 명령과 QEMU의 rbd 블록 드라이버, ceph.conf와 키링
sudo apt install ceph-common qemu-block-extra
sudo cp ceph.conf ceph.client.admin.keyring /etc/ceph/
oplctl pool create ceph --rbd rbd                    # client.admin, /etc/ceph/ceph.conf
oplctl pool create vms --rbd rbd/opl --rbd-user opl --rbd-conf /etc/ceph/opl.conf   # 네임스페이스·사용자·conf
oplctl volume create db --pool ceph --size 20G       # --format을 생략하면 raw
oplctl pool get ceph                                 # 노드별 준비·Ceph 풀 크기·남은 공간·오류, user·conf
```

- **공유, raw만.** VM은 풀이 준비된 어느 노드에서나 돈다(볼륨을 만든 노드에 묶이지 않는다). `--shared`는 필요 없고
  함께 쓸 수 없으며 API의 `"shared": false`는 **400** `invalid_request`("rbd pools are always shared")다. 쓸 노드는
  `--node`로 정한다(생략하면 모든 노드). 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**("rbd pools hold raw
  volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 볼륨 크기는 1 MiB 배수로 올린다.
- **이름과 접속.** Ceph 풀·네임스페이스·사용자는 `A-Za-z0-9_.-`의 1–63자이고 `-`로 시작하지 않는다. 사용자는
  `client.` 없이 적는다(`client.opl`이 아니라 `opl`. 생략하면 `admin`. `client.`로 시작하면 **400** "write the Ceph user
  without client."). `--rbd-conf`는 노드의 ceph.conf 절대
  경로이고(제어 문자·`,`·`:`·`=` 없이, 생략하면 Ceph 기본 `/etc/ceph/ceph.conf`) 키링은 그 conf가 가리키는 곳(기본
  `/etc/ceph/ceph.client.{사용자}.keyring`)에 노드마다 둔다. opld와 opld가 띄우는 QEMU가 같은 사용자로 읽는다. 사용자는
  그 풀의 이미지를 만들고 지우고 스냅샷할 수 있어야 하고 `ceph df`를 읽을 수 있어야 한다. `admin`이 아닌 사용자는
  `ceph auth get-or-create client.opl mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd'`로
  만든다(CI가 이 caps의 사용자로 네임스페이스와 명시적 conf를 써서 시험한다).
- **업그레이드 먼저.** RBD 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld는 먼저 VM을 띄우는 QEMU에 rbd 블록 드라이버가 있는지 한 번 본다(없으면 `"qemu has no rbd
  block driver"` — Ubuntu는 `qemu-block-extra`). 그다음 15초마다 `rbd ls`로 풀(네임스페이스)에 닿는지 보고
  `ceph df detail`에서 그 풀의 `max_avail`+`stored`를 크기로, `max_avail`을 남은 공간으로 알린다(Ceph 풀에 바이트
  쿼터가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다. 같은 Ceph 풀을 네임스페이스만 달리해 여러 opLite 풀이
  나눠 쓰면 각 풀이 Ceph 풀 전체의 크기·남은 공간을 보인다. 합쳐 10초 제한,
  넘으면 `"rbd timed out"`). 실패하면 `rbd`·`ceph`의 오류 첫 줄이 이유로 남고, 준비됐다고 알린 노드에만 VM과
  볼륨 작업이 간다. RBD 명령은 `LC_ALL=C`로 부르고 하나에 60초(이미지를 지우는 `rbd snap purge`·`rbd rm`과 되돌리는
  `rbd snap rollback`은 이미지 크기에 따라 오래 걸리므로 600초)를 넘으면 실패로 본다. LVM처럼 읽기 말고 바꾸는 명령은
  죽이지 않고 뒤에서 끝나게 두며, 그동안 이 노드에서 같은 이미지를 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할
  오류로 끝낸다. 이 표시는 노드마다 따로이므로 RBD 볼륨의 만들기·지우기·크기 늘리기(쓰는 VM이 없을 때) Task는 요청을
  받은 노드가 아니라 늘 풀이 준비된 온라인 노드 가운데 id가 가장 작은 노드가 한다.
- **macOS 미지원.** Homebrew QEMU에는 rbd 드라이버가 없다. macOS 노드는 "rbd pools are not supported on macOS"로
  알리고, 그 노드에 놓인 RBD 디스크의 VM은 이유와 함께 꺼진다.
- **VM 시작.** 활성화할 것이 없고 이미지가 있는지(`rbd info`)만 본다. 이미지가 없으면 VM을 이유와 함께 끄고,
  Ceph에 닿지 않는 등 `rbd`가 실패하면 끄지 않고 잠시 뒤 다시 시도한다. 이 노드에서 그 이미지를 바꾸는 명령(시간을
  넘긴 되돌리기 등)이 아직 돌고 있어도 잠시 뒤 다시 시도한다.
- **스냅샷.** VM 스냅샷은 RBD 스냅샷 `opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS이거나 모두 RBD여야 한다(블록 풀끼리 섞여도 **409** "disks use different snapshot
  methods"). 메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐
  멈추고 디스크를 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없지만 VM이 멈춘 동안 찍으므로 VM에게는
  한 시점이다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시
  지우고, 남은 것은 opld가 시작할 때와 그 뒤 30분마다 지운다. 공유 풀이므로 이 정리는 풀을 쓸 수 있는 온라인 노드
  가운데 id가 가장 작은 노드 하나가 맡고, 다른 노드가 막 찍은 스냅샷을 건드리지 않게 만든 지 30분이 지난 것만
  지운다.
- **아무 스냅샷으로나 되돌린다.** ZFS와 달리 더 오래된 스냅샷으로도 되돌릴 수 있고 그 뒤의 스냅샷은 남는다(다시
  더 최근 스냅샷으로 갈 수 있다). 되돌리기는 지금처럼 VM을 강제로 끈 뒤 `rbd snap rollback`을 한다. 이미지를 연
  클라이언트(다른 노드의 QEMU 등)가 있으면 잠시 뒤 다시 해 볼 오류로 끝난다.
- **지우기·복제·크기.** 볼륨을 지우면 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`). 이미지를 연
  클라이언트가 있으면(`rbd status`) 스냅샷도 지우지 않고 잠시 뒤 다시 해 볼 오류로 끝난다. 시간을 넘긴 만들기가 아직 돌고 있어 이미지가 없으면 볼륨을 지운 것으로 보지 않고 잠시 뒤 다시 해 본다.
  풀을 지워도 opLite 리소스만 지우고 Ceph 풀과 그 안의 다른 이미지는 그대로 둔다. 복제·복사는 `rbd clone`이 아니라
  `qemu-img convert`로 하는 전체 복사다(QEMU의 rbd 드라이버로 사용자·conf·네임스페이스를 넘긴다). 스냅샷에서
  복제하면 그 RBD 스냅샷을 읽어 대상 풀이 블록 풀(RBD·LVM-thin·ZFS)이면 raw, 디렉터리·NFS 풀이면 qcow2로 만들고,
  대상은 스냅샷 때의 크기로 만든다. 크기 늘리기는 실행 중이어도 되고 `rbd resize` 뒤 QEMU에 새 크기를 알린다(이미
  그 크기 이상이면 건너뛴다).
- **Ceph 풀이 가득 차면** 게스트의 쓰기가 실패한다(이미지는 thin이다). `oplctl pool get`의 남은 공간을 보고
  관리자가 Ceph 클러스터를 늘린다.

## 웹 UI
````

`README.md`에서

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs
```
````

을 다음으로 바꾼다:

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs
# Linux, root: 실제 Ceph RBD 풀. 테스트가 이미지를 만들고 지워도 되는 Ceph 풀과 ceph-common, qemu-block-extra, qemu-io,
# client.admin으로 접속할 수 있는 /etc/ceph/ceph.conf·키링이 필요하다. 제한된 사용자 시험은 아래 세 변수가 있을 때만 돈다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd \
  OPL_TEST_RBD_USER=opltest OPL_TEST_RBD_NAMESPACE=opl OPL_TEST_RBD_CONF=/etc/opl-ceph/ceph.conf \
  cargo test -p opl-driver --features rbd-it --test rbd_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_RBD=rbd cargo test -p opld --features rbd-it --test rbd
```
````

`README.md`에서

```markdown

웹 UI (`web/`):
```

을 다음으로 바꾼다:

````markdown

`rbd-it` 시험은 `OPL_TEST_RBD`의 Ceph 풀에 `opl-` 이미지를 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(더 오래된 스냅샷으로 되돌리기)·백엔드 안팎의 복사·지우기와 제한된 사용자·네임스페이스·명시적
conf로 접속하기를, opld 시험은 RBD 디스크로 VM을 띄워
실행 중 스냅샷, 메모리 스냅샷의 409, 멈춘 뒤 앞 스냅샷으로 되돌렸다가 다시 최근 스냅샷으로 되돌리기, 디렉터리
풀로 복제, 지우기를 본다. **버려도 되는 Ceph 풀**을 쓴다. CI `linux-kvm` 작업은 microceph로 노드 하나짜리 Ceph를
띄우고 풀 `rbd`를 둔다(준비에 1–2분):

```bash
sudo apt install ceph-common qemu-block-extra
sudo snap install microceph --channel squid/stable && sudo microceph cluster bootstrap
sudo snap unalias microceph                          # 별칭(/snap/bin/rbd 등)은 snap에 갇혀 /etc/opl-ceph·/tmp를 못 본다
sudo mkdir -p /etc/ceph && sudo ln -sf /var/snap/microceph/current/conf/ceph.conf /etc/ceph/ceph.conf
sudo sh -c 'ln -sf /var/snap/microceph/current/conf/*.keyring /etc/ceph/'
sudo ceph config set global mon_allow_pool_size_one true && sudo ceph config set global osd_pool_default_size 1
sudo microceph disk add loop,4G,1                     # OSD가 올라올 때까지 기다린다(sudo ceph osd stat)
sudo ceph osd pool create rbd 32 && sudo ceph osd pool set rbd size 1 --yes-i-really-mean-it
sudo rbd pool init rbd                                # PG가 active가 된 뒤(sudo ceph -s)
# 제한된 사용자 시험: 네임스페이스와 사용자, 그 키링만 가리키는 conf
sudo rbd namespace create rbd/opl && sudo mkdir -p /etc/opl-ceph
sudo ceph auth get-or-create client.opltest mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd' \
  | sudo tee /etc/opl-ceph/opltest.keyring >/dev/null
{ sudo cat /etc/ceph/ceph.conf; printf '\n[client.opltest]\nkeyring = /etc/opl-ceph/opltest.keyring\n'; } \
  | sudo tee /etc/opl-ceph/ceph.conf >/dev/null
# 끝나면: sudo snap remove --purge microceph
```

웹 UI (`web/`):
````

`README.md`에서

```markdown
- [공유 스토리지 설계 3: ZFS zvol (계획 17)](docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [공유 스토리지 설계 3: ZFS zvol (계획 17)](docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md)
- [공유 스토리지 설계 4: Ceph RBD (계획 18)](docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`crates/opl-api/src/snapshots.rs`에서

```rust
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, 스냅샷을 담지 못하는 디스크가 있거나 디스크의
/// 스냅샷 방식이 섞였거나 블록 디스크(LVM-thin, ZFS)의 VM에 메모리 스냅샷을 요청하면 409
/// `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
#[utoipa::path(post, path = "/vms/{key}/snapshots", tag = "snapshots",
```

을 다음으로 바꾼다:

```rust
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, 스냅샷을 담지 못하는 디스크가 있거나 디스크의
/// 스냅샷 방식이 섞였거나 블록 디스크(LVM-thin, ZFS, RBD)의 VM에 메모리 스냅샷을 요청하면
/// 409 `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
#[utoipa::path(post, path = "/vms/{key}/snapshots", tag = "snapshots",
```

`crates/opl-api/src/tasks.rs`에서

```rust
/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에
/// 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
```

을 다음으로 바꾼다:

```rust
/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS)은 raw만 담는다. 블록 볼륨(raw)을 파일 풀로
/// 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
```

을 다음으로 바꾼다:

```rust

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS, RBD)은 raw만 담는다. 블록 볼륨(raw)을 파일
/// 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
```

`docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md`에서

```markdown
| 17 | **ZFS**(zvol) | 16의 블록 바탕 |
| 18 | **iSCSI**, **Ceph RBD** | QEMU 네트워크 블록 드라이버(`iscsi`, `rbd`) |
```

을 다음으로 바꾼다:

```markdown
| 17 | **ZFS**(zvol) | 16의 블록 바탕 |
| 18 | **Ceph RBD** | QEMU 네트워크 블록 드라이버(`rbd`), 네트워크 디스크 소스(`RuntimeDisk.rbd`) |
| 19 | **iSCSI** | 18의 네트워크 디스크 소스(`iscsi`) 또는 공유 LUN 위의 블록 바탕 |

구현: 처음에는 계획 18이 iSCSI와 Ceph RBD를 함께 했으나, 성격이 달라(RBD는 이미지·스냅샷을 API로 다루고 iSCSI는 관리자가 만든 LUN을 쓴다) 계획 18은 Ceph RBD만 하고 iSCSI는 계획 19로 미뤘다(`2026-10-29-shared-storage-ceph-rbd-design.md` §1).
```

`docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`에서

```markdown
- `PoolBackend::Rbd { pool: String, namespace: Option<String>, user: Option<String>, conf: Option<String> }`(serde `{"kind":"rbd", ...}`).
  - `pool`·`namespace`: Ceph 이름(`A-Za-z0-9_.-`, 1–63자, `-`로 시작 금지).
  - `user`: Ceph 클라이언트 id(`client.` 없이, 같은 문자 규칙). 없으면 `admin`.
  - `conf`: ceph.conf 절대 경로(제어 문자·`,`·`:`·`=` 금지 — QEMU 옵션 문자열에 들어간다). 없으면 Ceph 기본(`/etc/ceph/ceph.conf`). 키링은 conf가 가리키는 기본 위치(관리자가 노드마다 둔다).
- RBD 풀은 항상 공유(`is_shared` true, `shared: false`는 400). 볼륨은 raw만(qcow2는 400). `pool_kind_supports_snapshots`는 raw.
- 이미지 `opl-{id}`, 스냅샷 `opl-{id}@{tag}`. 이미지 사양 `{pool}[/{namespace}]/opl-{id}`.
```

을 다음으로 바꾼다:

```markdown
- `PoolBackend::Rbd { pool: String, namespace: Option<String>, user: Option<String>, conf: Option<String> }`(serde `{"kind":"rbd", ...}`).
  - `pool`·`namespace`: Ceph 이름(`A-Za-z0-9_.-`, 1–63자, `-`로 시작 금지). 구현: `StoragePoolSpec::validate`의 `PoolError::BadRbdName(name)`(400 `invalid_request`, 사용자도 같다), 모르는 필드는 거절. 웹 `cephNameError`(`web/src/storage/pools.ts`)도 같다.
  - `user`: Ceph 클라이언트 id(`client.` 없이, 같은 문자 규칙). 없으면 `admin`. 구현: `.`을 받으므로 이름 규칙만으로는 `client.opl`도 통과해 `--id client.opl`(Ceph에서는 `client.client.opl`)로 접속하게 된다. 그래서 `client.`로 시작하는 사용자는 `PoolError::RbdUserPrefix(user)`(400 `invalid_request`, "write the Ceph user without client.")로 거절한다. 웹 `cephUserError`(`pools.errors.cephUser`)도 같고, oplctl은 서버의 400에 맡긴다. 기본값은 `opl_driver::RBD_DEFAULT_USER`(`"admin"`)이고 `RbdTarget.user`에는 기본값을 채운 값이 들어간다.
  - `conf`: ceph.conf 절대 경로(제어 문자·`,`·`:`·`=` 금지 — QEMU 옵션 문자열에 들어간다). 없으면 Ceph 기본(`/etc/ceph/ceph.conf`). 키링은 conf가 가리키는 기본 위치(관리자가 노드마다 둔다). 구현: `PoolError::BadRbdConf(conf)`(400), 웹 `rbdConfError`. conf가 없으면 `--conf`·`conf=`를 넘기지 않는다.
- RBD 풀은 항상 공유(`is_shared` true, `shared: false`는 400). 볼륨은 raw만(qcow2는 400). `pool_kind_supports_snapshots`는 raw. 구현: 문구는 "rbd pools are always shared"(API `create_pool`이 NFS와 함께 본다, `shared`를 적지 않으면 true로 둔다), "rbd pools hold raw volumes"(`check_pool_format`). 만들기·가져오기의 형식 기본값은 raw(`default_format`). `pool_usable_on`은 NFS처럼 준비됐다는 상태가 있어야 참이다. `pool_root`는 빈 경로이고 파일 저장소는 RBD 풀을 거절한다("rbd pools hold no volume files").
- 이미지 `opl-{id}`, 스냅샷 `opl-{id}@{tag}`. 이미지 사양 `{pool}[/{namespace}]/opl-{id}`.
```

`docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`에서

```markdown

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
```

을 다음으로 바꾼다:

```markdown

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
```

`docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`에서

```markdown

- **oplctl:** `pool create NAME --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`(다른 종류와 함께 못 씀, `--shared`는 필요 없음 — 항상 공유), 목록·상세의 종류 `rbd`·위치 `POOL[/NAMESPACE]`, 형식 기본값 raw.
- **웹:** 풀 만들기의 RBD(풀, 네임스페이스, 사용자, conf — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, `blockSnapshotKind`에 rbd.
```

을 다음으로 바꾼다:

```markdown

- **oplctl:** `pool create NAME --rbd POOL[/NAMESPACE] [--rbd-user ID] [--rbd-conf PATH]`(다른 종류와 함께 못 씀, `--shared`는 필요 없음 — 항상 공유), 목록·상세의 종류 `rbd`·위치 `POOL[/NAMESPACE]`, 형식 기본값 raw. 구현: `PoolCmd::Create(Box<PoolCreate>)`로 옮겼다(clap 인자가 많아 enum이 커졌다). `--rbd-user`·`--rbd-conf`는 `--rbd`가 필요하고 다른 종류와 함께 못 쓴다. `POOL[/NAMESPACE]`는 빈 이름이나 두 번째 `/`만 oplctl이 거절하고 이름 검사는 API가 한다. `pool get`은 `user`·`conf`가 있으면 보인다.
- **웹:** 풀 만들기의 RBD(풀, 네임스페이스, 사용자, conf — 서버와 같은 검사), 목록 종류·위치, `poolFormats` raw, `blockSnapshotKind`에 rbd. 구현: `isShared`가 RBD를 늘 공유로 본다(용량을 노드별이 아니라 하나로 보인다). i18n `pools.kinds.rbd`·`pools.kindHint.rbd`·`pools.cephPool`·`pools.namespace`·`pools.cephUser`·`pools.cephConf`·`pools.errors.rbdName`·`pools.errors.cephUser`·`pools.errors.rbdConf`. `pools.deleteDescription`은 Ceph 풀의 이미지도 남는다고 적는다.
```

`docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`에서

```markdown
- **단위:** 타입·검증, cmdline의 rbd blockdev, `disk_location`·resolve의 RBD, 가짜 `RbdOps`로 드라이버 각 동작, `qemu-img` rbd 인자, 마운터, API·웹·oplctl.
- **실제 Ceph(`rbd-it`, Linux·root):** CI `linux-kvm`에 `microceph`(snap)로 노드 하나짜리 Ceph(루프 디스크 OSD 1개, `size 1`), `rbd` 풀, `qemu-block-extra`. `OPL_TEST_RBD=POOL`. 드라이버 통합 테스트와 opld E2E(RBD 디스크 VM, 실행 중 스냅샷, 오래된 스냅샷으로 되돌리기, 디렉터리 풀로 복제, 지우기). microceph 준비가 CI에서 너무 느리거나 불안정하면(10분 넘게) `rbd-it`를 CI에서 빼고 수동 시험으로 남긴다(설계에 기록).
```

을 다음으로 바꾼다:

```markdown
- **단위:** 타입·검증, cmdline의 rbd blockdev, `disk_location`·resolve의 RBD, 가짜 `RbdOps`로 드라이버 각 동작, `qemu-img` rbd 인자, 마운터, API·웹·oplctl.
- **실제 Ceph(`rbd-it`, Linux·root):** CI `linux-kvm`에 `microceph`(snap)로 노드 하나짜리 Ceph(루프 디스크 OSD 1개, `size 1`), `rbd` 풀, `qemu-block-extra`. `OPL_TEST_RBD=POOL`. 드라이버 통합 테스트와 opld E2E(RBD 디스크 VM, 실행 중 스냅샷, 오래된 스냅샷으로 되돌리기, 디렉터리 풀로 복제, 지우기). microceph 준비가 CI에서 너무 느리거나 불안정하면(10분 넘게) `rbd-it`를 CI에서 빼고 수동 시험으로 남긴다(설계에 기록). 구현: 기능 `rbd-it`(opl-driver, opld), `crates/opl-driver/tests/rbd_storage.rs`(5개: 만들기·정보·크기·지우기, 가져오기와 실패한 가져오기 정리, 아무 스냅샷으로 되돌리기, 백엔드 안팎 복사, 제한된 사용자), `crates/opld/tests/rbd.rs`(1개, 풀은 API로 만들고 볼륨은 `--format raw`, 메모리 스냅샷의 409, 더 최근 스냅샷이 있을 때 앞 스냅샷으로 되돌리기와 다시 최근으로 되돌리기도 본다. 디스크 내용은 꺼진 VM의 이미지를 `qemu-io`로 본다). 시험은 `client.admin`과 `/etc/ceph/ceph.conf`로 하고, 드라이버 시험 하나(`restricted_user_in_a_namespace_with_its_conf`)는 제한된 사용자·네임스페이스·명시적 conf(`OPL_TEST_RBD_USER`·`OPL_TEST_RBD_NAMESPACE`·`OPL_TEST_RBD_CONF`, 없으면 건너뛴다)로 만들기·용량·스냅샷·되돌리기·복사·지우기를 본다. CI `linux-kvm`은 `ceph-common`·`qemu-block-extra`를 설치하고 microceph(`--channel squid/stable`로 고정, 4G 루프 OSD 하나, `mon_allow_pool_size_one`, 풀 `rbd` size 1)의 ceph.conf·키링을 `/etc/ceph`에 링크한다. 그리고 네임스페이스 `rbd/opl`, 사용자 `client.opltest`(caps `mon 'profile rbd' osd 'profile rbd pool=rbd' mgr 'profile rbd pool=rbd'`)를 만들어 키링은 `/etc/opl-ceph`에 두고 그 키링을 가리키는 `/etc/opl-ceph/ceph.conf`로만 찾게 한다. 그 단계는 15분 제한, 작업 제한은 55분으로 늘렸다. 첫 실행에서 microceph 준비는 75초였으므로 CI에 남겼다.
```

`docs/superpowers/specs/2026-10-29-shared-storage-ceph-rbd-design.md`에서

```markdown
- 링크드 클론은 없다(복제는 전체 복사).
```

을 다음으로 바꾼다:

```markdown
- 링크드 클론은 없다(복제는 전체 복사).
- RBD 명령이 시간을 넘겨 `Timeout`으로 끝나도 바꾸는 명령은 뒤에서 끝까지 돈다(LVM·ZFS와 같다). 그동안 같은 이미지를 바꾸는 요청은 일시 오류로 끝나고, 이 기록은 opld 프로세스 안의 것이다(노드마다 따로다 — 다른 노드는 모른다).
- QEMU rbd 드라이버 확인은 처음 한 번만 한다(opld가 도는 동안 `qemu-block-extra`를 지우면 VM 시작이 QEMU 오류로 실패한다).
- 같은 Ceph 풀을 네임스페이스만 달리해 여러 opLite 풀이 나눠 쓰면 각 풀이 Ceph 풀 전체의 크기·남은 공간을 보인다(`ceph df`는 네임스페이스별 사용량을 주지 않는다).
- 바꾸는 명령이 돈다는 표시가 노드마다 따로이므로 RBD 볼륨 Task를 한 노드(id가 가장 작은 준비된 노드)로 모으지만, 그 노드가 바뀌는 사이(오프라인 등)나 가져오기·스냅샷·VM Task처럼 다른 노드에서 도는 명령은 서로 모른다. 이미지를 연 클라이언트(watcher) 검사가 지우기·되돌리기를 막아 준다.
- Ceph 풀이 가득 차면 게스트의 쓰기가 실패한다(이미지는 thin이다). 가져오기의 공간 사전 확인은 지금처럼 업로드 디렉터리만 본다.
```

`web/src/i18n/en.json`에서

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export are kept.",
    "deleted": "Deleted storage pool {{name}}.",
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export and images in its Ceph pool are kept.",
    "deleted": "Deleted storage pool {{name}}.",
```

`web/src/i18n/ko.json`에서

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일은 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일과 Ceph 풀의 이미지는 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 602개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e --test snapshots`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 291개

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 4: 커밋**

```bash
git add README.md docs crates web
git commit -m "docs: Ceph RBD 풀 안내, 공유 스토리지 설계 4에 구현하며 정한 것"
```

---

## 완료 기준 (계획 18)

- RBD 풀을 API·oplctl·웹으로 만들 수 있고, `shared: false`·qcow2·잘못된 이름·`client.` 접두어·잘못된 conf는 400이다.
- 노드가 QEMU rbd 드라이버와 Ceph 접근을 확인해 상태와 용량을 알리고, macOS는 이유를 남긴다.
- RBD 볼륨의 만들기·가져오기·지우기·크기·스냅샷·아무 스냅샷으로 되돌리기·복제가 되고, 다른 클라이언트가 이미지를 열어 둔 동안 지우기·되돌리기는 기다린다.
- VM이 RBD 디스크를 `driver=rbd`로 열고 풀을 쓸 수 있는 어느 노드에서나 돈다. 디렉터리 풀과 RBD 풀 사이 복제가 된다.
- Linux에서 실제 Ceph로 드라이버 통합 테스트(제한된 사용자·네임스페이스·conf 포함)와 VM E2E가 통과한다(`rbd-it`, CI `linux-kvm`).
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **메모리 스냅샷이 없다.** 실행 중 스냅샷 동안 VM이 잠깐 멈춘다. 여러 디스크 스냅샷은 원자적이지 않다(멈춘 동안 찍어 VM에는 일관된다).
- **macOS 노드는 RBD를 쓸 수 없다.** Linux 노드에는 `qemu-block-extra`, `ceph-common`, ceph.conf·키링이 있어야 한다.
- **같은 Ceph 풀을 여러 opLite 풀(네임스페이스)이 나눠 쓰면** 각자 풀 전체 용량을 보인다.
- **진행 중 명령 표시는 노드마다 따로다.** 공유 볼륨 Task를 한 노드로 보내 대부분을 막지만, 그 노드가 바뀌는 사이는 막지 못한다.
- 그 밖의 제한은 설계 문서 §9에 있다.
