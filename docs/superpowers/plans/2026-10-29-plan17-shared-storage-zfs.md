# opLite 계획 17: 공유 스토리지 3 — ZFS zvol 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 계획 16의 블록 백엔드 바탕 위에 노드 로컬 ZFS 풀(sparse zvol, ZFS 스냅샷)을 더하고, LVM 전용이던 스냅샷 규칙을 "블록 백엔드" 공통 규칙으로 넓힌다.

**Architecture:** `PoolBackend::Zfs { dataset }`의 볼륨은 `{dataset}/opl-{id}` sparse zvol(`/dev/zvol/...`, `DiskKind::Block`)이다. `ZfsStorage`가 `ZfsOps`(실제는 `SystemZfs`) 뒤의 `zfs` 명령으로 만들기·지우기·크기·스냅샷·되돌리기(가장 최근 스냅샷만, 앞뒤로 장치 버퍼 비우기)를 하고, LVM에서 쓰던 명령 실행 도우미를 공용 `storage/blockcmd.rs`(`BlockTool`)로 옮겨 함께 쓴다. 여러 디스크 스냅샷은 `Storage::snapshot_many`(ZFS는 한 명령으로 원자적)로 찍고, 스냅샷 규칙은 `uses_block_snapshots`(LVM-thin, ZFS)로 일반화한다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum 0.8, utoipa, clap, QEMU QMP, OpenZFS 2.2(`zfs`, `udevadm`, `blockdev`); 웹은 계획 8~16과 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW). 새 외부 의존성은 없다. 실제 ZFS 시험은 Linux CI의 파일 위 zpool.

**설계 문서:** `docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md` (전체가 이 계획, "구현:" 메모 포함)
**선행 계획:** 계획 1~16 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `PoolBackend::Zfs { dataset }`(노드 로컬, raw만, 데이터셋 이름 177자 이하) | zpool·데이터셋 만들기(관리자), ZFS 파일 데이터셋의 qcow2(디렉터리 풀로 쓴다) |
| 공용 `BlockTool`(LVM과 ZFS), `ZfsStorage`·`ZfsOps`·`SystemZfs`, `Storage::snapshot_many` | `zfs send/recv` 복제(복사는 `qemu-img convert`) |
| 마운터의 데이터셋 확인, macOS 미지원, resolve | 가장 최근이 아닌 스냅샷으로 되돌리기, 메모리 스냅샷 |
| 블록 스냅샷 규칙 일반화(섞인 방식 409, 메모리 409, 예약), ZFS 되돌리기 최신 제한(API·Task·드라이버), 남은 블록 스냅샷 태그 정리 | 공유 ZFS, iSCSI·Ceph RBD — 계획 18 |
| oplctl `pool create --zfs`, 웹의 ZFS 풀·형식·되돌리기 제한 | |
| 실제 ZFS 시험(`zfs-it`, CI `linux-kvm`) | |

## 이전 계획에서 이어받는 사항

- 계획 16(공유 스토리지 2): 디스크 소스 종류, `Storages`, 백엔드 사이 복사, LVM의 명령 규칙과 실행 중 VM 멈춤 경로 위에 ZFS를 붙인다.

## 실행 환경

계획 16과 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 기능 테스트: `opld`의 `qemu-it`·`web-it`·`nfs-it`·`lvm-it`, `opl-api`·`opl-driver`의 `storage-it`. **실제 ZFS 시험(`opl-driver`·`opld`의 `zfs-it`)은 Linux에서 root로만 돈다.** CI `linux-kvm` 작업이 `zfsutils-linux`를 깔고 `modprobe zfs`·udev 규칙을 다시 읽은 뒤 2G 파일 위에 zpool `opltestz`와 데이터셋 `opltestz/opl`을 만들고 `OPL_TEST_ZFS=opltestz/opl`로 돌린다. macOS에서는 컴파일되지 않는다(시험 파일이 Linux 전용).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1·5). `Cargo.lock`은 바뀌지 않는다.

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **ZFS 풀.** 노드 로컬(`shared: true`는 400), 볼륨은 raw만. 데이터셋은 ZFS 이름 규칙에 177자 이하(zvol과 스냅샷 이름 `{dataset}/opl-{uuid}@{uuid}`가 255자를 넘지 않게). zvol은 `zfs create -s -V {1 MiB 배수} -o volmode=dev -o snapdev=visible -o com.sun:auto-snapshot=false`.
- **명령.** `BlockTool`: 바꾸는 명령은 시간 제한(60초)이나 취소에 죽이지 않고(출력은 파일), 같은 대상(zvol)에 하나씩, 읽기만 죽인다. 도구마다 바쁜 대상 집합이 따로다. `zfs destroy`가 busy면 1초 간격으로 세 번 다시 한다. 장치 노드는 `udevadm settle` 뒤 확인한다(없으면 두 번 연속 보일 때까지).
- **스냅샷.** `snapshot_many`로 풀·형식별로 묶어 찍는다(ZFS는 `zfs snapshot a@t b@t` 원자적). 블록 디스크 VM은 메모리 스냅샷이 없고, LVM과 ZFS가 섞이면 409. 실행 중이면 계획 16의 멈춤 경로를 쓴다.
- **되돌리기.** ZFS는 가장 최근 스냅샷으로만: API가 VM 스냅샷 순서(`created_at`, id, 만드는 중은 가장 새것)로 409, Task가 VM을 멈추기 전에 디스크마다 실제 마지막 태그를 확인, 드라이버가 다시 확인. 되돌리기는 장치 버퍼 비우기 → `zfs rollback` → udev settle → 버퍼 비우기(옛 페이지 캐시를 버린다).
- **남은 태그 정리.** 시간 초과·중단·Task 제한으로 남은 블록 스냅샷 태그는 `orphan_retry`(60초) 간격으로 최대 10번 지운다(대상에 명령이 아직 돌면 다시 시도). 실행기가 시작할 때 이 노드의 준비된 블록 볼륨에서 Snapshot 리소스가 없는 UUID 모양 태그를 지운다.
- **혼합 버전 클러스터**는 지원하지 않는다(모든 노드를 올린 뒤 ZFS 풀을 만든다).

## 파일 구조

```
crates/opl-types/src/storage.rs                            # PoolBackend::Zfs, 이름 검증
crates/opl-driver/src/storage/{blockcmd.rs, zfs.rs}(새로), storage/{lvm.rs, storages.rs}, storage.rs, error.rs, lib.rs   # BlockTool, ZfsStorage, snapshot_many
crates/opl-reconciler/src/resolve.rs, crates/opld/src/{pools.rs, member.rs}   # ZFS 해석, 마운터, 연결
crates/opl-api/src/{volumes.rs, resources.rs, snapshots.rs, schedule.rs, tasks.rs, tasks/*.rs}   # 규칙, 블록 스냅샷, 최신 제한, 태그 정리
crates/opl-driver/tests/zfs_storage.rs, crates/opld/tests/zfs.rs (새로)   # zfs-it
crates/oplctl/src/cli.rs, web/src/{storage,vms}/..., web/src/i18n/{ko,en}.json
.github/workflows/ci.yml, README.md, docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입과 API 규칙: ZFS 풀

`PoolBackend::Zfs { dataset }`(serde `zfs`)과 `PoolError::BadZfsDataset`(구성 요소는 `A-Za-z0-9_.:-`, 비지 않음, `-`로 시작 금지, `.`/`..` 금지, 풀 이름은 문자로 시작, 177자 이하), `pool_kind_supports_snapshots`·`is_shared`·`pool_usable_on`의 ZFS(raw, 노드 로컬, 준비 필요). API: ZFS 풀의 `shared: true`는 400, ZFS 풀의 qcow2는 400, 가져오기 형식이 없으면 블록 풀은 raw. 이 Task에서 `Storages`·`disk_location`·resolve·마운터는 ZFS를 아직 지원하지 않는다는 오류를 내고, `pool_root`는 `/dev/zvol/{dataset}`, 파일 저장소는 ZFS를 영구 거절한다. 웹 타입을 다시 만들고 풀 위치에 데이터셋.

**Files:**
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opld/src/pools.rs`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/pools.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust
    }
    // LVM-thin 풀은 노드 로컬이다.
    if matches!(spec.backend, PoolBackend::LvmThin { .. }) && spec.shared {
        return Err(ApiError::bad_request(
            "lvm-thin pools are node-local; shared must be false",
        ));
```

을 다음으로 바꾼다:

```rust
    }
    // LVM-thin·ZFS 풀은 노드 로컬이다.
    if matches!(spec.backend, PoolBackend::LvmThin { .. }) && spec.shared {
        return Err(ApiError::bad_request(
            "lvm-thin pools are node-local; shared must be false",
        ));
    }
    if matches!(spec.backend, PoolBackend::Zfs { .. }) && spec.shared {
        return Err(ApiError::bad_request(
            "zfs pools are node-local; shared must be false",
        ));
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 풀이 `format` 볼륨을 담을 수 있는지. LVM-thin 풀은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    if matches!(pool.spec.backend, PoolBackend::LvmThin { .. }) && format != ImageFormat::Raw {
        return Err(ApiError::bad_request("lvm-thin pools hold raw volumes"));
    }
```

을 다음으로 바꾼다:

```rust

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    let kind = match pool.spec.backend {
        PoolBackend::LvmThin { .. } => "lvm-thin",
        PoolBackend::Zfs { .. } => "zfs",
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => return Ok(()),
    };
    if format != ImageFormat::Raw {
        return Err(ApiError::bad_request(format!(
            "{kind} pools hold raw volumes"
        )));
    }
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: LVM-thin 풀은 raw, 그 밖은 스냅샷을 담을 수 있는 qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } => ImageFormat::Raw,
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
```

을 다음으로 바꾼다:

```rust

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀(LVM-thin, ZFS)은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => ImageFormat::Raw,
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
```

`crates/opl-api/tests/pools.rs`에서

```rust
            "thin pool with '/'",
        ),
```

을 다음으로 바꾼다:

```rust
            "thin pool with '/'",
        ),
        (
            pool("z", json!({ "kind": "zfs", "dataset": "tank/-opl" })),
            "dataset component starting with '-'",
        ),
        (
            pool("z", json!({ "kind": "zfs", "dataset": "tank/opl@s" })),
            "dataset with '@'",
        ),
```

`crates/opl-api/tests/pools.rs`에서

```rust
            "/volumes/import?name=up&pool=vg&source_format=raw",
            vec![0; 512],
```

을 다음으로 바꾼다:

```rust
            "/volumes/import?name=up&pool=vg&source_format=raw",
            vec![0; 512],
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let (_, volume) = admin.get("/volumes/up").await;
    assert_eq!(volume["spec"]["format"], "raw", "{volume}");
}

/// ZFS 풀도 노드 로컬이고 raw 볼륨만 담는다. 가져오기의 기본 형식은 raw다.
#[tokio::test]
async fn zfs_pools_are_node_local_and_hold_raw_volumes() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let zfs = json!({ "kind": "zfs", "dataset": "tank/opl" });
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "z2", "spec": { "backend": zfs, "shared": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(
        body["message"], "zfs pools are node-local; shared must be false",
        "{body}"
    );
    let (status, body) = admin.post("/storage-pools", pool("z", zfs.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "zfs", "{body}");
    assert_eq!(body["spec"]["backend"]["dataset"], "tank/opl", "{body}");
    env.pool_ready("z").await;

    let create = |format: &str| json!({ "name": "disk", "spec": { "pool": "z", "size_bytes": 1 << 20, "format": format } });
    let (status, body) = admin.post("/volumes", create("qcow2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert_eq!(body["message"], "zfs pools hold raw volumes");
    let (status, body) = admin.post("/volumes", create("raw")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let check = |format: &str| {
        format!("/volumes/import/check?name=up&pool=z&source_format=vmdk&size=512{format}")
    };
    let (status, body) = admin.post(&check("&format=qcow2"), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "zfs pools hold raw volumes");
    for format in ["&format=raw", ""] {
        let (status, body) = admin.post(&check(format), Value::Null).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{format}: {body}");
    }
    let (status, task) = admin
        .upload(
            "/volumes/import?name=up&pool=z&source_format=raw",
            vec![0; 512],
```

`crates/opl-driver/src/storage.rs`에서

```rust
/// 풀의 볼륨 파일이 놓이는 디렉터리. `dir` 풀은 그 경로, `nfs` 풀은 이 노드의 마운트 지점
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`다(볼륨 파일은 없다).
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

을 다음으로 바꾼다:

```rust
/// 풀의 볼륨 파일이 놓이는 디렉터리. `dir` 풀은 그 경로, `nfs` 풀은 이 노드의 마운트 지점
/// `{mount_root}/{풀 id}`, `lvm-thin` 풀은 VG의 장치 디렉터리 `/dev/{vg}`, `zfs` 풀은 데이터셋의 zvol
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다).
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
```

`crates/opl-driver/src/storage.rs`에서

```rust
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
    }
```

을 다음으로 바꾼다:

```rust
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust
    let root = pool_root(pool, mount_root);
    // LVM 풀의 볼륨은 파일이 아니다(`/dev/{vg}`에 파일을 만들지 않는다).
    if let PoolBackend::LvmThin { .. } = &pool.spec.backend {
        return Err(DriverError::InvalidSpec(
            "lvm-thin pools hold no volume files".into(),
        ));
    }
```

을 다음으로 바꾼다:

```rust
    let root = pool_root(pool, mount_root);
    // 블록 풀의 볼륨은 파일이 아니다(`/dev/{vg}`, `/dev/zvol/{dataset}`에 파일을 만들지 않는다).
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
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => {}
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust
        return Ok((lvm::volume_device(dev, vg, id)?, DiskKind::Block));
    }
```

을 다음으로 바꾼다:

```rust
        return Ok((lvm::volume_device(dev, vg, id)?, DiskKind::Block));
    }
    if let PoolBackend::Zfs { .. } = &pool.spec.backend {
        return Err(DriverError::InvalidSpec(
            "zfs pools are not supported yet".into(),
        ));
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust

    /// LVM 풀의 디스크는 LV 장치다. 장치 노드가 없으면 비활성(일시 오류)이다.
```

을 다음으로 바꾼다:

```rust

    /// 파일 저장소는 ZFS 풀도 다루지 않고(`/dev/zvol/{dataset}`에 파일을 만들지 않는다), 디스크
    /// 위치도 아직 정하지 않는다.
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
        match disk_location(&p, mnt.path(), id, f).unwrap_err() {
            DriverError::InvalidSpec(msg) => assert_eq!(msg, "zfs pools are not supported yet"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// LVM 풀의 디스크는 LV 장치다. 장치 노드가 없으면 비활성(일시 오류)이다.
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

을 다음으로 바꾼다:

```rust
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::Zfs { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
    LvmThin,
}
```

을 다음으로 바꾼다:

```rust
    LvmThin,
    /// 아직 다루는 저장소가 없다.
    Zfs,
}
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
        }
```

을 다음으로 바꾼다:

```rust
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
            PoolBackend::Zfs { .. } => Kind::Zfs,
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
            }),
        }
```

을 다음으로 바꾼다:

```rust
            }),
            Kind::Zfs => Err(DriverError::InvalidSpec(
                "zfs pools are not supported yet".into(),
            )),
        }
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

을 다음으로 바꾼다:

```rust

    /// ZFS 풀은 아직 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn zfs_pool_is_unsupported_for_now() {
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

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM을 쓴다.
```

을 다음으로 바꾼다:

```rust
        }
        // ZFS 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::Zfs { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "zfs pools are not supported yet",
            ));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM을 쓴다.
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
                PoolBackend::Dir { .. } | PoolBackend::Zfs { .. } => {}
            }
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// Linux에서는 LVM-thin 볼륨을 활성화한 뒤 LV 장치를 디스크로 쓴다. 활성화한 뒤에도 장치 노드가
```

을 다음으로 바꾼다:

```rust

/// ZFS 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[tokio::test]
async fn refuses_zfs_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let z: StoragePool = Resource::new(
        "z",
        StoragePoolSpec {
            backend: PoolBackend::Zfs {
                dataset: "tank/opl".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    cluster.create(&z).await.unwrap();
    let mut root = volume("root", "z");
    root.spec.format = ImageFormat::Raw;
    cluster.create(&root).await.unwrap();
    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "z" && why.contains("not supported")),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 LVM-thin 볼륨을 활성화한 뒤 LV 장치를 디스크로 쓴다. 활성화한 뒤에도 장치 노드가
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
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`).
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

/// 모든 노드가 같은 데이터를 보는 풀인지. NFS 풀은 `shared` 값과 상관없이 공유이고, LVM-thin·ZFS
/// 풀은 늘 노드 로컬이다.
pub fn is_shared(pool: &StoragePool) -> bool {
    match pool.spec.backend {
        PoolBackend::Dir { .. } => pool.spec.shared,
        PoolBackend::Nfs { .. } => true,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => false,
    }
}

/// 이 백엔드의 `format` 볼륨이 스냅샷을 담을 수 있는지. 파일 풀(dir, nfs)은 qcow2 내부 스냅샷,
/// LVM-thin 풀은 raw 볼륨의 thin 스냅샷, ZFS 풀은 raw zvol의 ZFS 스냅샷이다.
pub fn pool_kind_supports_snapshots(backend: &PoolBackend, format: ImageFormat) -> bool {
    match backend {
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => format == ImageFormat::Qcow2,
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => format == ImageFormat::Raw,
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
/// 데이터셋이 있다고 알려야 한다.
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
const LVM_UNSUPPORTED: &str = "lvm-thin pools are not supported on macOS";
const LVS_TIMED_OUT: &str = "lvs timed out";
```

을 다음으로 바꾼다:

```rust
const LVM_UNSUPPORTED: &str = "lvm-thin pools are not supported on macOS";
const ZFS_UNSUPPORTED: &str = "zfs pools are not supported yet";
const LVS_TIMED_OUT: &str = "lvs timed out";
```

`crates/opld/src/pools.rs`에서

```rust
                return Step::Lvm(vg.clone(), thinpool.clone());
            }
```

을 다음으로 바꾼다:

```rust
                return Step::Lvm(vg.clone(), thinpool.clone());
            }
            PoolBackend::Zfs { .. } => {
                return Step::State(not_ready(pool.id, ZFS_UNSUPPORTED.into()));
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

    /// ZFS 풀은 아직 확인하지 않고 준비되지 않았다고 적는다.
    #[tokio::test]
    async fn zfs_pools_are_not_ready_yet() {
        let mut s = setup().await;
        let backend = PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        };
        let z = s.pool("z", backend, vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["z"],
            not_ready(z.id, "zfs pools are not supported yet".into())
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

/** 만들 수 있는 풀 종류. ZFS 풀 만들기는 아직 없다. */
type Kind = Exclude<StoragePool["spec"]["backend"]["kind"], "zfs">;
```

`web/src/storage/pools.ts`에서

```ts

/** 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL` */
export function poolLocation(pool: StoragePool): string {
```

을 다음으로 바꾼다:

```ts

/**
 * 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은 `VG/THINPOOL`,
 * ZFS 풀은 데이터셋
 */
export function poolLocation(pool: StoragePool): string {
```

`web/src/storage/pools.ts`에서

```ts
    return `${b.vg}/${b.thinpool}`;
  }
```

을 다음으로 바꾼다:

```ts
    return `${b.vg}/${b.thinpool}`;
  }
  if (b.kind === "zfs") {
    return b.dataset;
  }
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 515개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `cargo test -p opl-api --test pools`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 282개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): ZFS 풀 백엔드와 API 규칙"
```

---

### Task 2: 공용 블록 명령 도우미

LVM `SystemLvm`의 프로세스 실행 부분을 새 `storage/blockcmd.rs`의 `BlockTool::new(timeout_msg, busy_msg, env, failure)`로 옮긴다: `run_read`(시간 제한, `kill_on_drop`), `run_change(&'static self, program, args, targets, timeout)`(죽이지 않음, stdout은 null, stderr는 지운 임시 파일, 대상마다 하나, 끝나면 뒤에서 회수), `changing(target)`. 도구마다 바쁜 대상 집합이 따로다. LVM은 `static LVM: BlockTool`을 쓰고 동작은 그대로다.

**Files:**
- Create: `crates/opl-driver/src/storage/blockcmd.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/storage.rs`에서

```rust

mod lvm;
```

을 다음으로 바꾼다:

```rust

mod blockcmd;
mod lvm;
```

`crates/opl-driver/src/storage/blockcmd.rs`를 만든다:

```rust
//! 블록 스토리지 명령(LVM, ZFS)을 돌리는 도우미. 읽기만 하는 명령은 시간이 지나면 죽이고, 바꾸는
//! 명령은 죽이지 않고 뒤에서 끝나게 둔다(대상마다 하나씩).

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use uuid::Uuid;

use crate::error::DriverError;

/// 명령 도구 하나(LVM, ZFS)의 설정과, 바꾸는 명령이 돌고 있는 대상들
pub(super) struct BlockTool {
    /// 명령이 시간을 넘겼을 때 `Timeout`에 담는 말
    timeout: &'static str,
    /// 같은 대상을 바꾸는 앞 명령이 아직 돌고 있을 때 `Timeout`에 담는 말
    busy: &'static str,
    /// 명령에 더하는 환경 변수
    env: &'static [(&'static str, &'static str)],
    /// 실패한 명령의 오류(stderr 첫 줄을 담는다)
    failure: fn(String) -> DriverError,
    /// 바꾸는 명령이 돌고 있는 대상
    changing: Mutex<BTreeSet<String>>,
}

impl BlockTool {
    pub(super) const fn new(
        timeout: &'static str,
        busy: &'static str,
        env: &'static [(&'static str, &'static str)],
        failure: fn(String) -> DriverError,
    ) -> Self {
        Self {
            timeout,
            busy,
            env,
            failure,
            changing: Mutex::new(BTreeSet::new()),
        }
    }

    fn command(&self, program: &str, args: &[&str]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .envs(self.env.iter().copied())
            .stdin(Stdio::null());
        cmd
    }

    /// 실패한 명령의 오류: stderr 첫 줄(없으면 종료 상태)
    fn failure(
        &self,
        program: &str,
        status: std::process::ExitStatus,
        stderr: &[u8],
    ) -> DriverError {
        let stderr = String::from_utf8_lossy(stderr);
        let line = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(|| status.to_string(), str::to_string);
        (self.failure)(format!("{program}: {line}"))
    }

    /// 읽기만 하는 명령을 돌려 표준 출력을 돌려준다. `timeout`이 지나면 `Timeout`이고, 시간이
    /// 지나거나 취소되면 명령을 죽인다.
    pub(super) async fn run_read(
        &self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>, DriverError> {
        let mut cmd = self.command(program, args);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout(self.timeout))??;
        if !out.status.success() {
            return Err(self.failure(program, out.status, &out.stderr));
        }
        Ok(out.stdout)
    }

    /// 대상 `targets`를 바꾸는 명령을 돌린다. `timeout`이 지나면 `Timeout`이지만 명령은 죽이지
    /// 않는다(중간에 죽이면 메타데이터 갱신이 반쯤 끝나 남을 수 있다). 이 future가 버려져도 끝날
    /// 때까지 기다리는 작업이 프로세스를 거둔다. 그동안 같은 대상을 바꾸는 명령은 띄우지 않고 일시
    /// 오류다. 기다리는 쪽이 떠난 뒤에도 출력 때문에 막히거나 죽지 않도록(SIGPIPE) 표준 출력은
    /// 버리고 stderr는 지운 임시 파일에 받는다.
    pub(super) async fn run_change(
        &'static self,
        program: &str,
        args: &[&str],
        targets: &[&str],
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let changing = Changing::begin(self, targets)?;
        let mut stderr = stderr_file()?;
        let mut child = self
            .command(program, args)
            .stdout(Stdio::null())
            .stderr(stderr.try_clone()?)
            .kill_on_drop(false)
            .spawn()?;
        let waited = tokio::spawn(async move {
            let status = child.wait().await;
            drop(changing);
            status
        });
        let status = tokio::time::timeout(timeout, waited)
            .await
            .map_err(|_| DriverError::Timeout(self.timeout))?
            .map_err(|e| DriverError::Io(std::io::Error::other(e)))??;
        if status.success() {
            return Ok(());
        }
        let mut text = Vec::new();
        stderr.seek(SeekFrom::Start(0))?;
        stderr.read_to_end(&mut text)?;
        Err(self.failure(program, status, &text))
    }

    /// 대상 `target`을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    pub(super) fn changing(&self, target: &str) -> bool {
        self.changing.lock().unwrap().contains(target)
    }
}

/// 바꾸는 명령이 돌고 있다는 대상들의 표시. 명령이 끝나 버려지면 지운다.
struct Changing {
    tool: &'static BlockTool,
    targets: Vec<String>,
}

impl Changing {
    /// 대상 중 하나라도 앞 명령이 돌고 있으면(시간을 넘겨 뒤에서 도는 중) 일시 오류다.
    fn begin(tool: &'static BlockTool, targets: &[&str]) -> Result<Self, DriverError> {
        let mut changing = tool.changing.lock().unwrap();
        if targets.iter().any(|t| changing.contains(*t)) {
            return Err(DriverError::Timeout(tool.busy));
        }
        let targets: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
        changing.extend(targets.iter().cloned());
        Ok(Self { tool, targets })
    }
}

impl Drop for Changing {
    fn drop(&mut self) {
        let mut changing = self.tool.changing.lock().unwrap();
        for target in &self.targets {
            changing.remove(target);
        }
    }
}

/// 명령의 stderr를 받을 임시 파일. 만들자마자 지우므로 열린 동안만 있다.
fn stderr_file() -> std::io::Result<std::fs::File> {
    let path = std::env::temp_dir().join(format!("opl-blockcmd-{}.err", Uuid::now_v7()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    static TOOL: BlockTool = BlockTool::new(
        "test command",
        "an earlier test command still running on the same target",
        &[("OPL_BLOCKCMD_TEST", "set")],
        DriverError::Lvm,
    );

    /// 읽기만 하는 명령만 시간이 지나면 죽인다. 바꾸는 명령은 뒤에서 끝나게 둔다.
    #[tokio::test]
    async fn only_reads_are_killed_on_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let short = Duration::from_millis(50);
        let (read, change) = (tmp.path().join("read"), tmp.path().join("change"));
        let script = |done: &Path| format!("sleep 0.3; touch {}", done.display());
        let err = TOOL
            .run_read("/bin/sh", &["-c", &script(&read)], short)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout("test command")),
            "{err:?}"
        );
        let err = TOOL
            .run_change("/bin/sh", &["-c", &script(&change)], &[], short)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout("test command")),
            "{err:?}"
        );
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(!read.exists());
        assert!(change.exists());

        // 읽기는 표준 출력을 돌려준다. 환경 변수가 붙는다. 실패하면 둘 다 stderr 첫 줄이다.
        let long = Duration::from_secs(10);
        let out = TOOL
            .run_read("/bin/sh", &["-c", "echo $OPL_BLOCKCMD_TEST"], long)
            .await;
        assert_eq!(out.unwrap(), b"set\n");
        let fail = ["-c", "echo '  no vg' >&2; echo more >&2; exit 5"];
        for err in [
            TOOL.run_read("/bin/sh", &fail, long).await.unwrap_err(),
            TOOL.run_change("/bin/sh", &fail, &[], long)
                .await
                .unwrap_err(),
        ] {
            assert!(
                matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: no vg"),
                "{err:?}"
            );
        }
        TOOL.run_change("/bin/sh", &["-c", "exit 0"], &[], long)
            .await
            .unwrap();
    }

    /// 바꾸는 명령의 출력은 파이프가 아니다(표준 출력은 버리고 stderr는 파일). 기다리는 쪽이 떠나
    /// 파이프가 닫혀도 명령이 SIGPIPE로 죽지 않는다.
    #[tokio::test]
    async fn change_output_is_not_a_pipe() {
        let script = "if [ -p /dev/fd/1 ] || [ -p /dev/fd/2 ]; then exit 3; fi";
        TOOL.run_change("/bin/sh", &["-c", script], &[], Duration::from_secs(10))
            .await
            .unwrap();
    }

    /// 같은 대상을 바꾸는 앞 명령이 아직 돌고 있으면 새 명령을 띄우지 않고 일시 오류다. 다른 대상과
    /// 다른 도구는 상관없고, 앞 명령이 끝나면 다시 띄운다.
    #[tokio::test]
    async fn one_change_per_target_at_a_time() {
        static OTHER_TOOL: BlockTool = BlockTool::new("other", "other busy", &[], DriverError::Lvm);
        let tmp = tempfile::tempdir().unwrap();
        let (target, other) = (
            format!("vg0/{}", Uuid::now_v7()),
            format!("vg0/{}", Uuid::now_v7()),
        );
        let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
        let slow = format!("sleep 0.4; touch {}", first.display());
        let touch = format!("touch {}", second.display());
        let long = Duration::from_secs(10);

        let err = TOOL
            .run_change(
                "/bin/sh",
                &["-c", &slow],
                &[&target],
                Duration::from_millis(50),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
        assert!(TOOL.changing(&target));
        assert!(!TOOL.changing(&other) && !OTHER_TOOL.changing(&target));
        // 이름 바꾸기처럼 여러 대상을 다루는 명령도 그중 하나가 돌고 있으면 띄우지 않는다.
        for targets in [vec![target.as_str()], vec![other.as_str(), target.as_str()]] {
            let err = TOOL
                .run_change("/bin/sh", &["-c", &touch], &targets, long)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DriverError::Timeout(m) if m == TOOL.busy),
                "{err:?}"
            );
            assert!(err.is_transient(), "{err:?}");
            assert!(!second.exists());
        }
        TOOL.run_change("/bin/sh", &["-c", "exit 0"], &[&other], long)
            .await
            .unwrap();
        OTHER_TOOL
            .run_change("/bin/sh", &["-c", "exit 0"], &[&target], long)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(first.exists());
        assert!(!TOOL.changing(&target));
        TOOL.run_change("/bin/sh", &["-c", &touch], &[&target], long)
            .await
            .unwrap();
        assert!(second.exists());
    }
}
```

`crates/opl-driver/src/storage/lvm.rs` 전체:

```rust
//! LVM-thin 풀: VG의 thin pool 안에 볼륨마다 thin LV `opl-{id}`(raw)를 두고, 스냅샷은 thin 스냅샷
//! LV `opl-{id}-s-{tag}`로 둔다. LVM 명령은 [`LvmOps`] 뒤에 있다(테스트는 가짜).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, INFO_TIMEOUT, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, src_fmt_name,
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
);

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
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::Zfs { .. } => {
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
        let args = [
            "info",
            "--output=json",
            "-f",
            src_fmt_name(src_format),
            src_str,
        ];
        let out = tokio::time::timeout(INFO_TIMEOUT, run_qemu_img(&self.qemu_img, &args))
            .await
            .map_err(|_| DriverError::Timeout("qemu-img info"))??;
        let size = serde_json::from_slice::<Value>(&out)
            .ok()
            .and_then(|json| json.get("virtual-size").and_then(Value::as_u64))
            .ok_or_else(|| DriverError::ToolFailed {
                tool: "qemu-img",
                stderr: "info output has no virtual-size".into(),
            })?;
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
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lvs = self.volume_lvs(vg, id).await?;
        let found = lvs.iter().any(|lv| lv.name == volume_lv(id));
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

    /// VM이 실행 중이어도 지울 수 있다(QEMU는 스냅샷 LV를 쓰지 않는다).
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let name = Self::snapshot_lv(id, tag)?;
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
            let from = convert_from(&source, force_share, dst_format)?;
            self.fill(vg, dst_id, &dst, &from).await
        };
        let copied = copied.await;
        guard.release().await;
        copied
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
    /// 아래에 둔다(udev처럼). `busy`에 적은 LV는 바꾸는 명령이 아직 돌고 있다고 알린다.
    struct FakeLvm {
        lvs: Mutex<BTreeMap<String, FakeLv>>,
        busy: Mutex<Vec<String>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        late: Mutex<Vec<String>>,
        stuck: Mutex<Vec<String>>,
        /// thin pool `tp`가 새 블록을 0으로 채우는지(`lv_attr` 여덟째 글자 `z`)
        zero: Mutex<bool>,
        dev: tempfile::TempDir,
    }

    impl Default for FakeLvm {
        fn default() -> Self {
            Self {
                lvs: Mutex::default(),
                busy: Mutex::default(),
                log: Mutex::default(),
                fail: Mutex::default(),
                late: Mutex::default(),
                stuck: Mutex::default(),
                zero: Mutex::new(true),
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
            let mut lvs = self.lvs.lock().unwrap();
            let removed = lvs.remove(name);
            self.sync_nodes(&lvs);
            removed.map(drop).ok_or_else(|| Self::missing(name))
        }

        async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("extend {vg}/{name} {size_bytes}"))?;
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
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
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
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 516개, 실패 0

Run: `cargo test -p opl-driver --lib storage`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "refactor(storage): LVM 명령 실행 도우미를 공용 blockcmd로 옮긴다"
```

---

### Task 3: ZFS zvol 드라이버

새 `storage/zfs.rs`: `ZfsOps`(volumes/snapshots(`createtxg` 순)/space/create/destroy -r/snapshot/rollback/destroy_snapshot/set_volsize/flush_buffers/settle/changing)와 실제 `SystemZfs`(`static ZFS: BlockTool`, `blockdev --flushbufs`, `udevadm settle --timeout=10`), `ZfsStorage::new(qemu_img, ops)`: 만들기(1 MiB 배수, `-s -V`, `volmode=dev`, `snapdev=visible`, `com.sun:auto-snapshot=false`, settle 뒤 장치 노드 대기), 가져오기·복사(`convert -n --target-is-zero`), 지우기(busy면 세 번 재시도), 정보·용량(`zfs list -Hp`), 크기(이미 크면 건너뜀), `snapshot_many`(한 명령, 원자적), 되돌리기(가장 최근 태그만, 버퍼 비우기 → rollback → settle → 버퍼 비우기), 스냅샷 지우기(대상에 명령이 돌면 일시 오류), 원본(스냅샷 장치). trait 기본 `snapshot_many`, `DriverError::Zfs`(영구), `disk_location`의 ZFS 분기, `Storages::with_zfs`, opld 연결, 공용 `source_virtual_size`. LVM의 `snapshot_delete`도 대상이 바쁘면 일시 오류. 가짜 `ZfsOps`로 테스트한다.

**Files:**
- Create: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/error.rs`에서

```rust
    Lvm(String),
    /// 블록 장치 볼륨의 장치 노드가 이 노드에 없다(활성화돼 있지 않거나 노드가 아직 생기지 않았다).
```

을 다음으로 바꾼다:

```rust
    Lvm(String),
    /// ZFS 명령이 실패했다. 내용은 stderr의 첫 줄이다.
    #[error("zfs failed: {0}")]
    Zfs(String),
    /// 블록 장치 볼륨의 장치 노드가 이 노드에 없다(활성화돼 있지 않거나 노드가 아직 생기지 않았다).
```

`crates/opl-driver/src/error.rs`에서

```rust
            | DriverError::ToolFailed { .. }
            | DriverError::Lvm(_) => false,
        }
```

을 다음으로 바꾼다:

```rust
            | DriverError::ToolFailed { .. }
            | DriverError::Lvm(_)
            | DriverError::Zfs(_) => false,
        }
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(!DriverError::Lvm("Volume group \"vg0\" not found".into()).is_transient());
    }
```

을 다음으로 바꾼다:

```rust
        assert!(!DriverError::Lvm("Volume group \"vg0\" not found".into()).is_transient());
        assert!(!DriverError::Zfs("dataset does not exist".into()).is_transient());
    }
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use storage::{
    Capacity, DirStorage, LvInfo, LvmOps, LvmThinStorage, Storage, Storages, SystemLvm, VolumeInfo,
    VolumeSource, check_pool_root, disk_location, parse_lvs, pool_root,
};
```

을 다음으로 바꾼다:

```rust
pub use storage::{
    Capacity, DirStorage, LvInfo, LvmOps, LvmThinStorage, Storage, Storages, SystemLvm, SystemZfs,
    VolumeInfo, VolumeSource, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root,
    disk_location, parse_lvs, pool_root,
};
```

`crates/opl-driver/src/storage.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::{DiskKind, ImageFormat};
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;
use crate::mounts::{MOUNTINFO, nfs_mount_points};

mod blockcmd;
mod lvm;
mod storages;
mod zfs;

pub use lvm::{LvInfo, LvmOps, LvmThinStorage, SystemLvm, parse_lvs};
pub use storages::Storages;
pub use zfs::{SystemZfs, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo};

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
/// 장치 디렉터리 `/dev/zvol/{dataset}`이다(블록 풀에는 볼륨 파일이 없다).
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
    match &pool.spec.backend {
        PoolBackend::Dir { path } => path.clone(),
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
        PoolBackend::LvmThin { vg, .. } => Path::new("/dev").join(vg),
        PoolBackend::Zfs { dataset } => Path::new("/dev/zvol").join(dataset),
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

/// VM 디스크로 쓸 볼륨의 위치와 종류. 파일 풀은 풀 루트([`check_pool_root`]로 확인한다) 안의
/// 볼륨 파일이다. LVM-thin 풀은 LV 장치 `/dev/{vg}/opl-{id}`(없으면 `/dev/mapper`의 노드)이고, 장치
/// 노드가 없으면(비활성) `VolumeNotActive`다(활성화는 저장소의 `activate_volume`이 한다). ZFS 풀은
/// zvol 장치 `/dev/zvol/{dataset}/opl-{id}`이고, 노드가 없으면(아직 생기지 않음) `VolumeNotActive`다.
pub fn disk_location(
    pool: &StoragePool,
    mount_root: &Path,
    id: Uuid,
    format: ImageFormat,
) -> Result<(PathBuf, DiskKind), DriverError> {
    disk_location_in(Path::new(lvm::DEV), pool, mount_root, id, format)
}

/// [`disk_location`]. 블록 장치 노드를 `dev` 아래에서 찾는다.
fn disk_location_in(
    dev: &Path,
    pool: &StoragePool,
    mount_root: &Path,
    id: Uuid,
    format: ImageFormat,
) -> Result<(PathBuf, DiskKind), DriverError> {
    if let PoolBackend::LvmThin { vg, .. } = &pool.spec.backend {
        lvm::require_raw(format)?;
        return Ok((lvm::volume_device(dev, vg, id)?, DiskKind::Block));
    }
    if let PoolBackend::Zfs { dataset } = &pool.spec.backend {
        zfs::require_raw(format)?;
        return Ok((zfs::volume_device(dev, dataset, id)?, DiskKind::Block));
    }
    check_pool_root(pool, mount_root)?;
    Ok((
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
            (
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
            (path, DiskKind::Block)
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
            (path, DiskKind::Block)
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

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
use super::{
    Capacity, INFO_TIMEOUT, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, src_fmt_name,
};
```

을 다음으로 바꾼다:

```rust
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
        let src_str = path_str(src)?;
        let args = [
            "info",
            "--output=json",
            "-f",
            src_fmt_name(src_format),
            src_str,
        ];
        let out = tokio::time::timeout(INFO_TIMEOUT, run_qemu_img(&self.qemu_img, &args))
            .await
            .map_err(|_| DriverError::Timeout("qemu-img info"))??;
        let size = serde_json::from_slice::<Value>(&out)
            .ok()
            .and_then(|json| json.get("virtual-size").and_then(Value::as_u64))
            .ok_or_else(|| DriverError::ToolFailed {
                tool: "qemu-img",
                stderr: "info output has no virtual-size".into(),
            })?;
        let dst = self.create_lv(vg, thinpool, id, size).await?;
```

을 다음으로 바꾼다:

```rust
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let dst = self.create_lv(vg, thinpool, id, size).await?;
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust

    /// VM이 실행 중이어도 지울 수 있다(QEMU는 스냅샷 LV를 쓰지 않는다).
    async fn snapshot_delete(
```

을 다음으로 바꾼다:

```rust

    /// VM이 실행 중이어도 지울 수 있다(QEMU는 스냅샷 LV를 쓰지 않는다). 스냅샷 LV가 없으면 지운
    /// 것으로 본다. 다만 그 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) LV가 곧 생길
    /// 수 있으므로 일시 오류다.
    async fn snapshot_delete(
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
        let name = Self::snapshot_lv(id, tag)?;
        if self.find(vg, &name).await?.is_some() {
```

을 다음으로 바꾼다:

```rust
        let name = Self::snapshot_lv(id, tag)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the snapshot",
            ));
        }
        if self.find(vg, &name).await?.is_some() {
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust

    /// 볼륨 LV를 옮겨 두고 스냅샷 LV의 스냅샷을 새 볼륨 LV로 만든 뒤 옛 LV를 지운다.
```

을 다음으로 바꾼다:

```rust

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
```

`crates/opl-driver/src/storage/storages.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::{Capacity, Storage, VolumeInfo, VolumeSource, convert_from, path_str, run_qemu_img};
use crate::error::DriverError;

/// 볼륨을 다루는 방식이 같은 풀 백엔드의 묶음.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 볼륨 파일(`dir`, `nfs`)
    File,
    LvmThin,
    Zfs,
}

impl Kind {
    fn of(pool: &StoragePool) -> Self {
        match &pool.spec.backend {
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => Kind::File,
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
            PoolBackend::Zfs { .. } => Kind::Zfs,
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
}

impl Storages {
    pub fn new(qemu_img: impl Into<PathBuf>, file: Arc<dyn Storage>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            file,
            lvm_thin: None,
            zfs: None,
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
            let from = convert_from(src, force_share, format)?;
            // -n: 만들어 둔 볼륨(파일 또는 장치)에 쓴다. 새 볼륨이 0으로 읽히면 0인 부분은 쓰지
            // 않는다(--target-is-zero).
            let mut args = vec!["convert", "-q", "-n"];
            if zero {
                args.push("--target-is-zero");
            }
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(&path)?);
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
            _: &StoragePool,
            id: Uuid,
            format: ImageFormat,
            tag: Option<&str>,
        ) -> Result<VolumeSource, DriverError> {
            self.call("volume_source")?;
            Ok(VolumeSource {
                path: self.dir.join(id.to_string()),
                format,
                snapshot: tag.map(str::to_string),
                size_bytes: 2 << 30,
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

    /// 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜 qemu-img. `fail`이면 실패한다.
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        std::fs::write(
            &path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {args:?}\n{end}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn taken(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    /// 파일 풀(dir, nfs)은 파일 저장소로, lvm-thin 풀은 LVM 저장소로, zfs 풀은 ZFS 저장소로 간다.
    #[tokio::test]
    async fn calls_go_to_the_pool_backend() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)));
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        for (p, name) in [
            (dir_pool(), "file"),
            (nfs_pool(), "file"),
            (lvm_pool(), "lvm"),
            (zfs_pool(), "zfs"),
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
        use std::os::unix::fs::PermissionsExt;
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        std::fs::write(&slow, "#!/bin/sh\nsleep 5\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
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

`crates/opl-driver/src/storage/zfs.rs`를 만든다:

```rust
//! ZFS 풀: 데이터셋 바로 아래에 볼륨마다 sparse zvol `{dataset}/opl-{id}`(raw)를 두고, 스냅샷은 ZFS
//! 스냅샷 `{dataset}/opl-{id}@{tag}`로 둔다. ZFS 명령은 [`ZfsOps`] 뒤에 있다(테스트는 가짜).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// ZFS 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const ZFS_TIMEOUT: Duration = Duration::from_secs(60);

/// 새 zvol의 장치 노드가 생기기를(udev) 기다리는 시간
const NODE_WAIT: Duration = Duration::from_secs(10);

/// udev가 이벤트를 다 다루기를 기다리는 `udevadm settle`(10초)의 명령 제한 시간
const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);

/// udev를 기다리지 못했을 때 장치 노드가 머무는지 다시 보는 간격. udev가 새 zvol을 다시 다루는
/// 동안 `/dev/zvol` 링크가 잠시 사라졌다 다시 생긴다.
const NODE_STABLE: Duration = Duration::from_millis(100);

/// zvol 지우기가 "busy"로 실패하면 다시 시도하는 횟수와 그 사이 시간. 방금 닫힌 장치를
/// udev(blkid) 등이 잠시 열고 있을 수 있다.
const BUSY_RETRIES: usize = 3;
const BUSY_WAIT: Duration = Duration::from_secs(1);

/// zvol 크기는 이 배수로 올린다(volblocksize의 배수여야 한다).
const ZVOL_ALIGN: u64 = 1 << 20;

/// `zfs list`가 알려 준 zvol 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZvolInfo {
    /// 전체 이름(`{dataset}/opl-{id}`)
    pub name: String,
    /// `volsize`
    pub size_bytes: u64,
    /// `referenced`(할당된 바이트)
    pub used_bytes: u64,
}

/// zvol의 스냅샷 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsSnapshot {
    /// `@` 뒤의 이름
    pub tag: String,
    /// 스냅샷 때의 `volsize`
    pub size_bytes: u64,
}

/// 데이터셋의 `available`·`used`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZfsSpace {
    pub available_bytes: u64,
    pub used_bytes: u64,
}

/// ZFS 명령. zvol은 전체 이름이다. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait ZfsOps: Send + Sync {
    /// 데이터셋 바로 아래의 zvol들(`zfs list -t volume -d 1`)
    async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError>;
    /// zvol의 스냅샷들. 만든 순서(오래된 것부터)다.
    async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError>;
    async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError>;
    /// `size_bytes` 크기의 sparse zvol을 만든다(`zfs create -s -V`).
    async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError>;
    /// zvol을 스냅샷과 함께 지운다(`zfs destroy -r`).
    async fn destroy(&self, zvol: &str) -> Result<(), DriverError>;
    /// zvol들에 같은 이름의 스냅샷을 한 명령으로(원자적으로) 만든다.
    async fn snapshot(&self, zvols: &[&str], tag: &str) -> Result<(), DriverError>;
    /// zvol을 가장 최근 스냅샷 `tag`로 되돌린다(`zfs rollback`).
    async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError>;
    async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError>;
    async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError>;
    /// zvol 장치의 커널 버퍼를 비운다(`blockdev --flushbufs`). 되돌린 뒤 옛 데이터가 읽히지 않게.
    async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError>;
    /// zvol을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, zvol: &str) -> bool;
    /// udev가 이벤트(장치 노드 만들기·다시 다루기)를 다 다루기를 기다린다(`udevadm settle`).
    /// udev가 없으면 `false`, 시간을 넘기면 오류다.
    async fn settle(&self) -> Result<bool, DriverError>;
}

/// 실제 ZFS 명령. root여야 한다.
pub struct SystemZfs;

/// 같은 zvol을 바꾸는 앞 명령이 아직 돌고 있을 때의 일시 오류(`Timeout`)에 담는 말
const ZFS_BUSY: &str = "an earlier zfs command still running on the same zvol";

/// ZFS 명령. 바꾸는 명령의 대상은 zvol(전체 이름)이다.
static ZFS: BlockTool = BlockTool::new(
    "zfs command",
    ZFS_BUSY,
    &[("LC_ALL", "C")],
    DriverError::Zfs,
);

/// `zfs list -Hp`/`zfs get -Hp`의 탭으로 나뉜 줄들. 빈 줄은 건너뛴다.
fn rows(out: &[u8]) -> impl Iterator<Item = Vec<&str>> {
    std::str::from_utf8(out)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').collect())
}

fn bad_output(what: impl std::fmt::Display) -> DriverError {
    DriverError::Zfs(format!("unparseable zfs output: {what}"))
}

fn number(v: &str) -> Result<u64, DriverError> {
    v.trim()
        .parse()
        .map_err(|_| bad_output(format!("number {v:?}")))
}

/// `zfs list -Hp -o name,volsize,referenced`의 출력
fn parse_zvols(out: &[u8]) -> Result<Vec<ZvolInfo>, DriverError> {
    rows(out)
        .map(|row| match row.as_slice() {
            [name, size, used] => Ok(ZvolInfo {
                name: name.to_string(),
                size_bytes: number(size)?,
                used_bytes: number(used)?,
            }),
            _ => Err(bad_output(format!("zvol row {row:?}"))),
        })
        .collect()
}

/// `zfs list -Hp -t snapshot -o name,volsize`의 출력
fn parse_snapshots(out: &[u8]) -> Result<Vec<ZfsSnapshot>, DriverError> {
    rows(out)
        .map(|row| match row.as_slice() {
            [name, size] => {
                let (_, tag) = name
                    .split_once('@')
                    .ok_or_else(|| bad_output(format!("snapshot name {name:?}")))?;
                Ok(ZfsSnapshot {
                    tag: tag.to_string(),
                    size_bytes: number(size)?,
                })
            }
            _ => Err(bad_output(format!("snapshot row {row:?}"))),
        })
        .collect()
}

/// `zfs get -Hp -o value available,used`의 출력
fn parse_space(out: &[u8]) -> Result<ZfsSpace, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [available, used] if available.len() == 1 && used.len() == 1 => Ok(ZfsSpace {
            available_bytes: number(available[0])?,
            used_bytes: number(used[0])?,
        }),
        _ => Err(bad_output(format!("space {values:?}"))),
    }
}

#[async_trait]
impl ZfsOps for SystemZfs {
    async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError> {
        let args = [
            "list",
            "-Hp",
            "-t",
            "volume",
            "-d",
            "1",
            "-o",
            "name,volsize,referenced",
            dataset,
        ];
        parse_zvols(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError> {
        let args = [
            "list",
            "-Hp",
            "-t",
            "snapshot",
            "-d",
            "1",
            "-s",
            "createtxg",
            "-o",
            "name,volsize",
            zvol,
        ];
        parse_snapshots(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError> {
        let args = ["get", "-Hp", "-o", "value", "available,used", dataset];
        parse_space(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
        let size = size_bytes.to_string();
        ZFS.run_change("zfs", &create_args(zvol, &size), &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
        ZFS.run_change("zfs", &["destroy", "-r", zvol], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn snapshot(&self, zvols: &[&str], tag: &str) -> Result<(), DriverError> {
        let names: Vec<String> = zvols.iter().map(|z| format!("{z}@{tag}")).collect();
        let mut args = vec!["snapshot"];
        args.extend(names.iter().map(String::as_str));
        ZFS.run_change("zfs", &args, zvols, ZFS_TIMEOUT).await
    }

    async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
        let name = format!("{zvol}@{tag}");
        ZFS.run_change("zfs", &["rollback", &name], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
        let name = format!("{zvol}@{tag}");
        ZFS.run_change("zfs", &["destroy", &name], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
        let prop = format!("volsize={size_bytes}");
        ZFS.run_change("zfs", &["set", &prop, zvol], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError> {
        let dev = device_path(Path::new(super::lvm::DEV), zvol, None);
        let args = ["--flushbufs", path_str(&dev)?];
        ZFS.run_change("blockdev", &args, &[zvol], ZFS_TIMEOUT)
            .await
    }

    fn changing(&self, zvol: &str) -> bool {
        ZFS.changing(zvol)
    }

    async fn settle(&self) -> Result<bool, DriverError> {
        let args = ["settle", "--timeout=10"];
        match ZFS.run_read("udevadm", &args, SETTLE_TIMEOUT).await {
            Ok(_) => Ok(true),
            Err(DriverError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// sparse zvol을 만드는 `zfs` 인자. 장치로만 보이고(`volmode=dev`) 스냅샷 장치도 보인다
/// (`snapdev=visible`). 이 속성을 따르는 자동 스냅샷 도구(zfs-auto-snapshot 등)가 찍지 않게
/// `com.sun:auto-snapshot=false`를 둔다: 더 최근 스냅샷이 생기면 되돌리기가 막힌다.
fn create_args<'a>(zvol: &'a str, size: &'a str) -> [&'a str; 11] {
    [
        "create",
        "-s",
        "-V",
        size,
        "-o",
        "volmode=dev",
        "-o",
        "snapdev=visible",
        "-o",
        "com.sun:auto-snapshot=false",
        zvol,
    ]
}

/// 볼륨의 zvol 이름
fn volume_zvol(dataset: &str, id: Uuid) -> String {
    format!("{dataset}/opl-{id}")
}

/// zvol(`snapshot`이 있으면 그 스냅샷)의 장치 노드 `{dev}/zvol/{zvol}[@{snapshot}]`
fn device_path(dev: &Path, zvol: &str, snapshot: Option<&str>) -> PathBuf {
    let name = match snapshot {
        Some(tag) => format!("{zvol}@{tag}"),
        None => zvol.to_string(),
    };
    dev.join("zvol").join(name)
}

/// 있는 ZFS 볼륨의 장치 노드. 없으면 `VolumeNotActive`(노드가 아직 생기지 않았다).
pub(super) fn volume_device(dev: &Path, dataset: &str, id: Uuid) -> Result<PathBuf, DriverError> {
    let path = device_path(dev, &volume_zvol(dataset, id), None);
    if path.exists() {
        Ok(path)
    } else {
        Err(DriverError::VolumeNotActive(path))
    }
}

/// ZFS 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "zfs pools hold raw volumes".into(),
        )),
    }
}

/// zvol 크기: `ZVOL_ALIGN` 배수로 올린다.
fn zvol_size(size_bytes: u64) -> Result<u64, DriverError> {
    size_bytes
        .div_ceil(ZVOL_ALIGN)
        .checked_mul(ZVOL_ALIGN)
        .ok_or_else(|| DriverError::InvalidSpec(format!("volume size {size_bytes} is too large")))
}

/// ZFS 풀(`zfs`)의 볼륨. 볼륨은 sparse zvol이고 스냅샷은 ZFS 스냅샷이다.
pub struct ZfsStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn ZfsOps>,
    /// 장치 노드가 놓이는 디렉터리(`/dev`, zvol은 `{dev}/zvol/...`). 테스트는 바꾼다.
    dev: PathBuf,
    /// 장치 노드가 생기기를 기다리는 시간([`NODE_WAIT`]). 테스트는 줄인다.
    node_wait: Duration,
    /// busy인 zvol 지우기를 다시 시도하기 전에 기다리는 시간([`BUSY_WAIT`]). 테스트는 줄인다.
    busy_wait: Duration,
}

impl ZfsStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn ZfsOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(super::lvm::DEV),
            node_wait: NODE_WAIT,
            busy_wait: BUSY_WAIT,
        }
    }

    /// 풀의 데이터셋. raw가 아니면 `InvalidSpec`.
    fn dataset(pool: &StoragePool, format: ImageFormat) -> Result<&str, DriverError> {
        require_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::Zfs { dataset } => Ok(dataset),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::LvmThin { .. } => {
                Err(DriverError::InvalidSpec("not a zfs pool".into()))
            }
        }
    }

    /// 스냅샷 태그는 ZFS 이름에 쓸 수 있는 글자여야 한다.
    fn check_tag(tag: &str) -> Result<(), DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c));
        if ok {
            Ok(())
        } else {
            Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid zfs name"
            )))
        }
    }

    /// 데이터셋 바로 아래의 zvol `zvol`
    async fn find(&self, dataset: &str, zvol: &str) -> Result<Option<ZvolInfo>, DriverError> {
        Ok(self
            .ops
            .volumes(dataset)
            .await?
            .into_iter()
            .find(|v| v.name == zvol))
    }

    /// 있는 볼륨의 zvol. 없으면 `VolumeNotFound`.
    async fn existing(&self, dataset: &str, id: Uuid) -> Result<ZvolInfo, DriverError> {
        self.find(dataset, &volume_zvol(dataset, id))
            .await?
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 있는 볼륨의 스냅샷들(만든 순서)
    async fn existing_snapshots(
        &self,
        dataset: &str,
        id: Uuid,
    ) -> Result<Vec<ZfsSnapshot>, DriverError> {
        let zvol = self.existing(dataset, id).await?.name;
        self.ops.snapshots(&zvol).await
    }

    /// 볼륨이 있고 스냅샷 `tag`도 있어야 한다. 볼륨의 스냅샷들(만든 순서)과 그 스냅샷을 돌려준다.
    async fn existing_snapshot(
        &self,
        dataset: &str,
        id: Uuid,
        tag: &str,
    ) -> Result<(Vec<ZfsSnapshot>, ZfsSnapshot), DriverError> {
        let snapshots = self.existing_snapshots(dataset, id).await?;
        let found = snapshots
            .iter()
            .find(|s| s.tag == tag)
            .cloned()
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
            })?;
        Ok((snapshots, found))
    }

    /// zvol을 스냅샷과 함께 지운다. "busy"로 실패하면 [`BUSY_RETRIES`]번까지 다시 시도한다.
    async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
        let mut retries = 0;
        loop {
            match self.ops.destroy(zvol).await {
                Err(DriverError::Zfs(msg)) if msg.contains("busy") && retries < BUSY_RETRIES => {
                    tracing::debug!(zvol, error = %msg, "zvol busy, retrying destroy");
                    retries += 1;
                    tokio::time::sleep(self.busy_wait).await;
                }
                result => return result,
            }
        }
    }

    /// udev가 이벤트를 다 다루기를 기다린다. 기다리지 못했으면(udev가 없거나 시간을 넘김) `false`.
    async fn settle(&self) -> bool {
        match self.ops.settle().await {
            Ok(settled) => settled,
            Err(e) => {
                tracing::warn!(error = %e, "udev did not settle");
                false
            }
        }
    }

    /// udev를 기다린 뒤 장치 노드 `path`가 생기기를 기다린다. udev를 기다리지 못했으면 노드가 잠시
    /// 사라졌다 다시 생길 수 있으므로 [`NODE_STABLE`] 떨어진 두 번의 확인에서 모두 보여야 한다.
    /// 끝내 없으면 `VolumeNotActive`.
    async fn wait_node(&self, path: PathBuf) -> Result<PathBuf, DriverError> {
        let settled = self.settle().await;
        let deadline = tokio::time::Instant::now() + self.node_wait;
        let mut seen = false;
        loop {
            if path.exists() {
                if settled || seen {
                    return Ok(path);
                }
                seen = true;
            } else {
                seen = false;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DriverError::VolumeNotActive(path));
            }
            let pause = if seen {
                NODE_STABLE
            } else {
                Duration::from_millis(50)
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// 새 볼륨 zvol을 만들고 장치 노드를 돌려준다. 크기는 1 MiB 배수로 올린다. 장치 노드가 생기지
    /// 않으면 만든 zvol을 지운다.
    async fn create_zvol(
        &self,
        dataset: &str,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<PathBuf, DriverError> {
        let zvol = volume_zvol(dataset, id);
        if self.find(dataset, &zvol).await?.is_some() {
            return Err(DriverError::VolumeExists(id));
        }
        self.ops.create(&zvol, zvol_size(size_bytes)?).await?;
        match self.wait_node(device_path(&self.dev, &zvol, None)).await {
            Ok(path) => Ok(path),
            Err(e) => {
                if let Err(del) = self.destroy(&zvol).await {
                    tracing::warn!(volume = %id, error = %del, "cannot remove volume without device");
                }
                Err(e)
            }
        }
    }

    /// 만든 zvol(장치 `dst`)에 `qemu-img convert -n`으로 써 넣는다. 새 sparse zvol은 0으로 읽히므로
    /// 0인 부분은 쓰지 않는다(`--target-is-zero`). 실패하면 zvol을 지운다.
    async fn fill(
        &self,
        dataset: &str,
        id: Uuid,
        dst: &Path,
        from: &[String],
    ) -> Result<(), DriverError> {
        let written = async {
            let mut args = vec!["convert", "-q", "-n", "--target-is-zero"];
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(dst)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.destroy(&volume_zvol(dataset, id)).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for ZfsStorage {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        self.create_zvol(dataset, id, size_bytes).await
    }

    /// 원본의 가상 크기로 zvol을 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let dst = self.create_zvol(dataset, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(dataset, id, &dst, &from).await?;
        Ok(dst)
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// zvol을 스냅샷과 함께 지운다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?.name;
        self.destroy(&zvol).await
    }

    /// zvol은 늘 활성이다. 볼륨이 있고 장치 노드가 있는지만 본다.
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        self.existing(dataset, id).await?;
        volume_device(&self.dev, dataset, id).map(drop)
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: zvol.size_bytes,
            allocated_bytes: zvol.used_bytes,
        })
    }

    /// 데이터셋의 크기(`available`+`used`)와 남은 공간(`available`)
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let dataset = Self::dataset(pool, ImageFormat::Raw)?;
        let space = self.ops.space(dataset).await?;
        Ok(Capacity {
            total_bytes: space.available_bytes.saturating_add(space.used_bytes),
            available_bytes: space.available_bytes,
        })
    }

    /// 볼륨이 raw이므로 ZFS 스냅샷으로 스냅샷을 둔다.
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
        self.snapshot_many(pool, &[id], format, tag).await
    }

    /// 모든 볼륨이 있고 어느 볼륨에도 `tag`가 없으면 한 명령으로(원자적으로) 찍는다.
    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        Self::check_tag(tag)?;
        if ids.is_empty() {
            return Ok(());
        }
        let volumes = self.ops.volumes(dataset).await?;
        let mut zvols = Vec::with_capacity(ids.len());
        for &id in ids {
            let zvol = volume_zvol(dataset, id);
            if !volumes.iter().any(|v| v.name == zvol) {
                return Err(DriverError::VolumeNotFound(id));
            }
            if self
                .ops
                .snapshots(&zvol)
                .await?
                .iter()
                .any(|s| s.tag == tag)
            {
                return Err(DriverError::InvalidSpec(format!(
                    "snapshot {tag} already exists on volume {id}"
                )));
            }
            zvols.push(zvol);
        }
        let zvols: Vec<&str> = zvols.iter().map(String::as_str).collect();
        self.ops.snapshot(&zvols, tag).await?;
        // 스냅샷 장치 노드가 생긴다(`snapdev=visible`).
        self.settle().await;
        Ok(())
    }

    /// 그 볼륨의 가장 최근 스냅샷으로만 되돌린다(`zfs rollback`은 더 최근 스냅샷이 있으면 그것을
    /// 지워야 한다). 되돌리기 앞뒤로 장치의 커널 버퍼를 비운다(앞: 아직 쓰이지 않은 버퍼가 되돌린
    /// 뒤에 쓰이지 않게, 뒤: 남은 페이지 캐시가 옛 데이터를 보이지 않게). 비우지 못하면 오류고, 앞에서
    /// 못 비우면 되돌리지 않는다. 되돌린 뒤 udev를 기다리고 장치 노드가 있는지 본다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let (snapshots, _) = self.existing_snapshot(dataset, id, tag).await?;
        if snapshots.last().is_some_and(|s| s.tag != tag) {
            return Err(DriverError::InvalidSpec(
                "zfs can only revert to the latest snapshot of a volume".into(),
            ));
        }
        let zvol = volume_zvol(dataset, id);
        self.ops.flush_buffers(&zvol).await?;
        self.ops.rollback(&zvol, tag).await?;
        // udev가 되돌린 장치를 다시 다루는 동안 장치 노드가 잠시 사라질 수 있다.
        self.wait_node(device_path(&self.dev, &zvol, None)).await?;
        self.ops.flush_buffers(&zvol).await
    }

    /// VM이 실행 중이어도 지울 수 있다. 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 zvol을
    /// 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 곧 생길 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        Self::check_tag(tag)?;
        let zvol = volume_zvol(dataset, id);
        if self.find(dataset, &zvol).await?.is_none() {
            return Ok(());
        }
        if self.ops.changing(&zvol) {
            return Err(DriverError::Timeout(ZFS_BUSY));
        }
        if self
            .ops
            .snapshots(&zvol)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            self.ops.destroy_snapshot(&zvol, tag).await?;
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
        let dataset = Self::dataset(pool, format)?;
        let snapshots = self.existing_snapshots(dataset, id).await?;
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
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?;
        if zvol.size_bytes >= size_bytes {
            return Ok(());
        }
        self.ops
            .set_volsize(&zvol.name, zvol_size(size_bytes)?)
            .await
    }

    /// zvol 장치, 또는 스냅샷의 장치(`snapdev=visible`). 내놓을 것은 없다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let (zvol, size_bytes) = match tag {
            Some(tag) => {
                let (_, snapshot) = self.existing_snapshot(dataset, id, tag).await?;
                (volume_zvol(dataset, id), snapshot.size_bytes)
            }
            None => {
                let zvol = self.existing(dataset, id).await?;
                (zvol.name, zvol.size_bytes)
            }
        };
        let path = self.wait_node(device_path(&self.dev, &zvol, tag)).await?;
        Ok(VolumeSource {
            path,
            format,
            snapshot: None,
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

    /// 원본(스냅샷이면 그때)의 크기로 대상 zvol을 만들고 원본을 써 넣는다. 데이터셋이 달라도 같다.
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
        let dataset = Self::dataset(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        let dst = self.create_zvol(dataset, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, dst_format)?;
        self.fill(dataset, dst_id, &dst, &from).await
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
    struct FakeZvol {
        size: u64,
        /// 만든 순서의 스냅샷(태그, 크기)
        snapshots: Vec<(String, u64)>,
    }

    /// 데이터셋 `tank/opl`, `tank/other`를 메모리에 두는 ZFS. 바꾸는 호출을 적고, `fail`에 적은 호출은
    /// 실패한다. `busy`에 적은 호출은 남은 횟수만큼 "busy"로 실패한다. zvol과 스냅샷의 장치 노드를
    /// `dev/zvol` 아래에 둔다(udev처럼). `no_udev`면 udev 기다리기를 하지 못한다.
    struct FakeZfs {
        zvols: Mutex<BTreeMap<String, FakeZvol>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        busy: Mutex<BTreeMap<String, usize>>,
        /// 바꾸는 명령이 아직 돌고 있다고 알리는 zvol
        changing: Mutex<Vec<String>>,
        no_udev: Mutex<bool>,
        dev: tempfile::TempDir,
    }

    impl Default for FakeZfs {
        fn default() -> Self {
            Self {
                zvols: Mutex::default(),
                log: Mutex::default(),
                fail: Mutex::default(),
                busy: Mutex::default(),
                changing: Mutex::default(),
                no_udev: Mutex::default(),
                dev: tempfile::tempdir().unwrap(),
            }
        }
    }

    impl FakeZfs {
        /// zvol(`snapshot`이 있으면 그 스냅샷)의 장치 노드 경로
        fn node(&self, zvol: &str, snapshot: Option<&str>) -> PathBuf {
            device_path(self.dev.path(), zvol, snapshot)
        }

        /// 장치 노드를 zvol과 스냅샷에 맞춘다.
        fn sync_nodes(&self, zvols: &BTreeMap<String, FakeZvol>) {
            let root = self.dev.path().join("zvol");
            let _ = std::fs::remove_dir_all(&root);
            for (name, zvol) in zvols {
                let node = self.node(name, None);
                std::fs::create_dir_all(node.parent().unwrap()).unwrap();
                std::fs::write(&node, b"").unwrap();
                for (tag, _) in &zvol.snapshots {
                    std::fs::write(self.node(name, Some(tag)), b"").unwrap();
                }
            }
        }

        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Zfs(format!("injected: {call}")));
            }
            if let Some(left) = self.busy.lock().unwrap().get_mut(&call)
                && *left > 0
            {
                *left -= 1;
                return Err(DriverError::Zfs(format!("{call}: dataset is busy")));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        /// 호출 `call`이 `times`번 "busy"로 실패한다.
        fn busy_on(&self, call: impl Into<String>, times: usize) {
            self.busy.lock().unwrap().insert(call.into(), times);
        }

        /// 적은 호출(udev 기다리기 `settle`까지)
        fn taken_all(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        /// 적은 호출 중 udev 기다리기를 뺀 것
        fn taken(&self) -> Vec<String> {
            let mut log = self.taken_all();
            log.retain(|c| c != "settle");
            log
        }

        fn names(&self) -> Vec<String> {
            self.zvols.lock().unwrap().keys().cloned().collect()
        }

        fn zvol(&self, name: &str) -> Option<FakeZvol> {
            self.zvols.lock().unwrap().get(name).cloned()
        }

        fn missing(name: &str) -> DriverError {
            DriverError::Zfs(format!("cannot open '{name}': dataset does not exist"))
        }

        /// 데이터셋 `dataset` 바로 아래의 이름인지
        fn in_dataset(dataset: &str, name: &str) -> bool {
            name.strip_prefix(dataset)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|rest| !rest.contains('/'))
        }
    }

    #[async_trait]
    impl ZfsOps for FakeZfs {
        async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError> {
            if !["tank/opl", "tank/other"].contains(&dataset) {
                return Err(Self::missing(dataset));
            }
            Ok(self
                .zvols
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| Self::in_dataset(dataset, name))
                .map(|(name, z)| ZvolInfo {
                    name: name.clone(),
                    size_bytes: z.size,
                    used_bytes: z.size / 4,
                })
                .collect())
        }

        async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError> {
            let zvols = self.zvols.lock().unwrap();
            let z = zvols.get(zvol).ok_or_else(|| Self::missing(zvol))?;
            Ok(z.snapshots
                .iter()
                .map(|(tag, size)| ZfsSnapshot {
                    tag: tag.clone(),
                    size_bytes: *size,
                })
                .collect())
        }

        async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError> {
            if dataset != "tank/opl" {
                return Err(Self::missing(dataset));
            }
            Ok(ZfsSpace {
                available_bytes: 6 << 30,
                used_bytes: 2 << 30,
            })
        }

        async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("create {zvol} {size_bytes}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            if zvols.contains_key(zvol) {
                return Err(DriverError::Zfs(format!("cannot create '{zvol}': exists")));
            }
            let z = FakeZvol {
                size: size_bytes,
                snapshots: vec![],
            };
            zvols.insert(zvol.into(), z);
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("destroy -r {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let removed = zvols.remove(zvol);
            self.sync_nodes(&zvols);
            removed.map(drop).ok_or_else(|| Self::missing(zvol))
        }

        async fn snapshot(&self, names: &[&str], tag: &str) -> Result<(), DriverError> {
            self.call(format!("snapshot {} @{tag}", names.join(" ")))?;
            let mut zvols = self.zvols.lock().unwrap();
            // 한 명령은 모두 찍거나 하나도 찍지 않는다.
            for name in names {
                let z = zvols.get(*name).ok_or_else(|| Self::missing(name))?;
                if z.snapshots.iter().any(|(t, _)| t == tag) {
                    return Err(DriverError::Zfs(format!("{name}@{tag} exists")));
                }
            }
            for name in names {
                let z = zvols.get_mut(*name).unwrap();
                z.snapshots.push((tag.into(), z.size));
            }
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
            self.call(format!("rollback {zvol}@{tag}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            match z.snapshots.last() {
                Some((t, size)) if t == tag => z.size = *size,
                _ => return Err(DriverError::Zfs("more recent snapshots exist".into())),
            }
            Ok(())
        }

        async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
            self.call(format!("destroy {zvol}@{tag}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            let before = z.snapshots.len();
            z.snapshots.retain(|(t, _)| t != tag);
            if z.snapshots.len() == before {
                return Err(Self::missing(&format!("{zvol}@{tag}")));
            }
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("set volsize={size_bytes} {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            if size_bytes < z.size {
                return Err(DriverError::Zfs("cannot shrink".into()));
            }
            z.size = size_bytes;
            Ok(())
        }

        async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("flushbufs {zvol}"))
        }

        fn changing(&self, zvol: &str) -> bool {
            self.changing.lock().unwrap().iter().any(|z| z == zvol)
        }

        async fn settle(&self) -> Result<bool, DriverError> {
            self.call("settle".into())?;
            Ok(!*self.no_udev.lock().unwrap())
        }
    }

    fn zfs_pool(dataset: &str) -> StoragePool {
        Resource::new(
            "z",
            StoragePoolSpec {
                backend: PoolBackend::Zfs {
                    dataset: dataset.into(),
                },
                nodes: vec![],
                shared: false,
            },
        )
    }

    fn pool() -> StoragePool {
        zfs_pool("tank/opl")
    }

    fn zvol(id: Uuid) -> String {
        format!("tank/opl/opl-{id}")
    }

    /// `info`면 가상 크기 1000을 알리고, 그 밖은 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜
    /// qemu-img. `fail`이면 `info`가 아닌 호출이 실패한다.
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
    }

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeZfs>, ZfsStorage) {
        let zfs = Arc::new(FakeZfs::default());
        let storage = ZfsStorage {
            dev: zfs.dev.path().to_path_buf(),
            busy_wait: Duration::from_millis(10),
            ..ZfsStorage::new(qemu_img, zfs.clone())
        };
        (zfs, storage)
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

    const RAW: ImageFormat = ImageFormat::Raw;
    const MIB: u64 = 1 << 20;

    /// 크기를 1 MiB 배수로 올린 sparse zvol을 만들고 장치 노드를 돌려준다.
    #[tokio::test]
    async fn creates_raw_sparse_zvols() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let path = storage.create_volume(&p, id, MIB + 1, RAW).await.unwrap();
        assert_eq!(path, zfs.node(&zvol(id), None));
        assert_eq!(path, zfs.dev.path().join(format!("zvol/tank/opl/opl-{id}")));
        // 만든 뒤 udev가 장치 노드를 다 다룰 때까지 기다린다.
        assert_eq!(
            zfs.taken_all(),
            [format!("create {} {}", zvol(id), 2 * MIB), "settle".into()]
        );

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
        assert_eq!(zfs.taken(), Vec::<String>::new());
        // 데이터셋이 없으면 ZFS 오류다.
        let err = storage
            .create_volume(&zfs_pool("tank/none"), id, MIB, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
    }

    /// udev를 기다리지 못하면(udev가 없거나 시간을 넘김) 장치 노드가 잠시 사라졌다 다시 생길 수
    /// 있으므로 떨어진 두 번의 확인에서 모두 보여야 한다.
    #[tokio::test]
    async fn without_udev_settle_the_device_node_must_stay() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        *zfs.no_udev.lock().unwrap() = true;
        let node = zfs.dev.path().join("node");
        std::fs::write(&node, b"").unwrap();
        let flicker = {
            let node = node.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                std::fs::remove_file(&node).unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                std::fs::write(&node, b"").unwrap();
            })
        };
        let started = std::time::Instant::now();
        assert_eq!(storage.wait_node(node.clone()).await.unwrap(), node);
        assert!(started.elapsed() >= Duration::from_millis(330));
        flicker.await.unwrap();
        assert_eq!(zfs.taken_all(), ["settle"]);
        // udev를 기다렸으면 바로 돌려준다.
        *zfs.no_udev.lock().unwrap() = false;
        let started = std::time::Instant::now();
        storage.wait_node(node.clone()).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        // 기다리기가 실패해도(시간을 넘김) 기록만 하고 노드가 머무는지 본다.
        zfs.fail_on("settle");
        let started = std::time::Instant::now();
        storage.wait_node(node.clone()).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    /// 장치 노드가 생기지 않으면 만든 zvol을 지운다.
    #[tokio::test]
    async fn create_without_device_node_destroys_the_zvol() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let storage = ZfsStorage {
            dev: zfs.dev.path().join("elsewhere"),
            node_wait: Duration::from_millis(100),
            ..storage
        };
        let (p, id) = (pool(), Uuid::now_v7());
        let err = storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(id)),
                format!("destroy -r {}", zvol(id)),
            ]
        );
        assert!(zfs.names().is_empty());
    }

    /// 원본의 가상 크기로 zvol을 만들고 raw로 변환해 써 넣는다(새 zvol은 0으로 읽힌다). 실패하면
    /// zvol을 지운다.
    #[tokio::test]
    async fn imports_into_a_new_zvol() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, zfs.node(&zvol(id), None));
        assert_eq!(zfs.taken(), [format!("create {} {MIB}", zvol(id))]);
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
                path.to_str().unwrap(),
            ]
        );

        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(id)),
                format!("destroy -r {}", zvol(id)),
            ]
        );
        assert!(zfs.names().is_empty());
        invalid(
            storage
                .import_volume(&p, id, "x.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );
    }

    /// 지우기는 zvol을 스냅샷과 함께 지운다. 없으면 `VolumeNotFound`다.
    #[tokio::test]
    async fn delete_destroys_zvol_with_its_snapshots() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [id, other] {
            storage.create_volume(&p, v, MIB, RAW).await.unwrap();
            storage.snapshot_create(&p, v, RAW, "a").await.unwrap();
        }
        zfs.taken();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(zfs.taken(), [format!("destroy -r {}", zvol(id))]);
        assert_eq!(zfs.names(), [zvol(other)]);
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);
        assert!(zfs.taken().is_empty());
    }

    /// zvol 지우기가 "busy"로 실패하면(방금 닫힌 장치를 아직 누가 열고 있다) 세 번까지 다시
    /// 시도한다. 지우기, 만들기 직후 정리, 가져오기 실패 정리가 같다.
    #[tokio::test]
    async fn destroy_retries_while_busy() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        let (p, id) = (pool(), Uuid::now_v7());
        let destroy = format!("destroy -r {}", zvol(id));
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 3);
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(zfs.taken(), vec![destroy.clone(); 4]);
        assert!(zfs.names().is_empty());

        // 네 번째도 busy면 그 오류다.
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 4);
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("busy")),
            "{err:?}"
        );
        assert_eq!(zfs.taken(), vec![destroy.clone(); 4]);
        assert_eq!(zfs.names(), [zvol(id)]);
        // busy가 아닌 실패는 다시 시도하지 않는다.
        zfs.fail_on(&destroy);
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert_eq!(zfs.taken(), [destroy.as_str()]);

        // 가져오기에 실패하면 busy여도 만든 zvol을 치운다.
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        zfs.busy_on(&destroy, 2);
        storage
            .import_volume(&p, id, "/up/disk.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert_eq!(zfs.taken()[1..], vec![destroy.clone(); 3]);
        assert!(zfs.names().is_empty());

        // 장치 노드가 생기지 않아 지울 때도 같다.
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let storage = ZfsStorage {
            dev: zfs.dev.path().join("elsewhere"),
            node_wait: Duration::from_millis(100),
            ..storage
        };
        zfs.busy_on(&destroy, 1);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert_eq!(zfs.taken()[1..], vec![destroy.clone(); 2]);
        assert!(zfs.names().is_empty());
    }

    #[tokio::test]
    async fn reports_volume_usage_and_dataset_capacity() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.volume_info(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, 4 * MIB, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: MIB,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        let err = storage.capacity(&zfs_pool("tank/none")).await.unwrap_err();
        assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        // 다른 데이터셋의 zvol은 이 풀의 볼륨이 아니다.
        let other = zfs_pool("tank/other");
        not_found(storage.volume_info(&other, id, RAW).await.unwrap_err(), id);
        assert_eq!(zfs.taken().len(), 1);
    }

    /// zvol은 늘 활성이다. 볼륨과 장치 노드가 있는지만 본다.
    #[tokio::test]
    async fn activate_checks_volume_and_device_node() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.activate_volume(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert!(zfs.taken().is_empty());
        std::fs::remove_file(zfs.node(&zvol(id), None)).unwrap();
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeNotActive(ref p) if *p == zfs.node(&zvol(id), None)),
            "{err:?}"
        );
        assert!(err.is_transient());
    }

    /// 크기를 1 MiB 배수로 올려 늘린다. 이미 그 크기 이상이면 아무것도 하지 않는다.
    #[tokio::test]
    async fn resize_sets_volsize() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage
                .resize_volume(&p, id, RAW, 2 * MIB)
                .await
                .unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        storage
            .resize_volume(&p, id, RAW, 2 * MIB + 1)
            .await
            .unwrap();
        assert_eq!(
            zfs.taken(),
            [format!("set volsize={} {}", 3 * MIB, zvol(id))]
        );
        storage.resize_volume(&p, id, RAW, 3 * MIB).await.unwrap();
        storage.resize_volume(&p, id, RAW, MIB).await.unwrap();
        assert!(zfs.taken().is_empty());
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, 3 * MIB);
    }

    #[tokio::test]
    async fn snapshots_are_zfs_snapshots() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
        not_found(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        not_found(storage.snapshot_tags(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();

        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {} @b", zvol(id)),
                format!("snapshot {} @a", zvol(id)),
            ]
        );
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        for bad in ["a/b", "a@b", "", "a b"] {
            invalid(
                storage.snapshot_create(&p, id, RAW, bad).await.unwrap_err(),
                "not a valid zfs name",
            );
        }
        // 만든 순서다.
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["b", "a"]
        );

        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        // 없는 태그나 볼륨은 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        storage
            .snapshot_delete(&p, Uuid::now_v7(), RAW, "b")
            .await
            .unwrap();
        assert_eq!(zfs.taken(), [format!("destroy {}@b", zvol(id))]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["a"]);
        invalid(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
    }

    /// 그 zvol을 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 아직 없어도 지우기는
    /// 일시 오류다. 명령이 끝나면 태그가 생길 수 있다.
    #[tokio::test]
    async fn snapshot_delete_waits_for_a_running_command() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.changing.lock().unwrap().push(zvol(id));
        let err = storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        zfs.changing.lock().unwrap().clear();
        storage.snapshot_delete(&p, id, RAW, "t").await.unwrap();
        assert!(zfs.taken().is_empty());
    }

    /// 여러 볼륨을 한 명령으로 찍는다. 볼륨이 없거나 태그가 이미 있으면 아무것도 찍지 않는다.
    #[tokio::test]
    async fn snapshot_many_takes_one_atomic_command() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (a, b, missing) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        for v in [a, b] {
            storage.create_volume(&p, v, MIB, RAW).await.unwrap();
        }
        zfs.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        // 스냅샷 장치 노드가 생기므로 udev를 기다린다.
        assert_eq!(
            zfs.taken_all(),
            [
                format!("snapshot {} {} @t", zvol(a), zvol(b)),
                "settle".into()
            ]
        );
        for v in [a, b] {
            assert_eq!(storage.snapshot_tags(&p, v, RAW).await.unwrap(), ["t"]);
        }

        not_found(
            storage
                .snapshot_many(&p, &[a, missing], RAW, "u")
                .await
                .unwrap_err(),
            missing,
        );
        storage.snapshot_create(&p, b, RAW, "u").await.unwrap();
        zfs.taken();
        invalid(
            storage
                .snapshot_many(&p, &[a, b], RAW, "u")
                .await
                .unwrap_err(),
            "already exists",
        );
        assert!(zfs.taken().is_empty());
        assert_eq!(storage.snapshot_tags(&p, a, RAW).await.unwrap(), ["t"]);
        // 볼륨이 없으면 할 일이 없다.
        storage.snapshot_many(&p, &[], RAW, "v").await.unwrap();
        assert!(zfs.taken().is_empty());
    }

    /// 가장 최근 스냅샷으로만 되돌린다. 크기도 그때로 돌아간다. 되돌리기 앞뒤로 장치의 버퍼를
    /// 비운다.
    #[tokio::test]
    async fn revert_only_to_the_latest_snapshot() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        storage.resize_volume(&p, id, RAW, 3 * MIB).await.unwrap();
        zfs.taken();

        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        match err {
            DriverError::InvalidSpec(msg) => assert_eq!(
                msg,
                "zfs can only revert to the latest snapshot of a volume"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(zfs.taken().is_empty());

        storage.snapshot_revert(&p, id, RAW, "b").await.unwrap();
        assert_eq!(
            zfs.taken_all(),
            [
                format!("flushbufs {}", zvol(id)),
                format!("rollback {}@b", zvol(id)),
                "settle".into(),
                format!("flushbufs {}", zvol(id)),
            ]
        );
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, 2 * MIB);
        // 최근 스냅샷을 지우면 그 앞 스냅샷으로 되돌릴 수 있다.
        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, MIB);

        // 버퍼를 비우지 못하면 옛 데이터가 읽힐 수 있으므로 되돌리기가 실패한다. 되돌리기 전에
        // 비우지 못하면 되돌리지 않는다.
        zfs.taken();
        zfs.fail_on(format!("flushbufs {}", zvol(id)));
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(zfs.taken(), [format!("flushbufs {}", zvol(id))]);
    }

    /// 원본은 zvol 장치 또는 스냅샷 장치이고 크기는 그때의 것이다. 내놓을 것은 없다.
    #[tokio::test]
    async fn volume_source_reads_zvol_or_snapshot_device() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.volume_source(&p, id, RAW, None).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        zfs.taken();

        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: zfs.node(&zvol(id), Some("t")),
                format: RAW,
                snapshot: None,
                size_bytes: MIB,
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.path, zfs.node(&zvol(id), None));
        assert_eq!(source.size_bytes, 2 * MIB);
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );
        assert!(zfs.taken().is_empty());
    }

    /// 같은 백엔드 복사: 원본(스냅샷이면 그때)의 크기로 zvol을 만들고 원본을 써 넣는다. 다른
    /// 데이터셋으로도 된다. 실패하면 만든 zvol을 지운다.
    #[tokio::test]
    async fn copies_into_a_new_zvol() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, other) = (pool(), zfs_pool("tank/other"));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        storage.resize_volume(&p, src, RAW, 2 * MIB).await.unwrap();
        zfs.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap();
        let to = format!("tank/other/opl-{dst}");
        assert_eq!(zfs.taken(), [format!("create {to} {MIB}")]);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to_node) = (zfs.node(&zvol(src), Some("t")), zfs.node(&to, None));
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
                from.to_str().unwrap(),
                to_node.to_str().unwrap(),
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

        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        zfs.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(dst)),
                format!("destroy -r {}", zvol(dst)),
            ]
        );
        assert_eq!(zfs.names(), [zvol(src)]);
    }

    /// 바꾸는 명령이 실패하면 그 오류를 그대로 돌려준다.
    #[tokio::test]
    async fn command_failures_are_zfs_errors() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.fail_on(format!("snapshot {} @t", zvol(id)));
        let err = storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert!(!err.is_transient());
    }

    /// zvol은 sparse이고 장치로만 보이며 스냅샷 장치도 보인다. 자동 스냅샷 도구(zfs-auto-snapshot
    /// 등)가 찍지 않게 한다(더 최근 스냅샷이 생기면 되돌리기가 막힌다).
    #[test]
    fn create_args_make_sparse_zvols_without_auto_snapshots() {
        assert_eq!(
            create_args("tank/opl/opl-x", "1048576"),
            [
                "create",
                "-s",
                "-V",
                "1048576",
                "-o",
                "volmode=dev",
                "-o",
                "snapdev=visible",
                "-o",
                "com.sun:auto-snapshot=false",
                "tank/opl/opl-x",
            ]
        );
    }

    /// ZFS 명령은 C 로캘로 돌고, 실패는 `Zfs` 오류다.
    #[tokio::test]
    async fn zfs_commands_use_c_locale_and_zfs_errors() {
        let long = Duration::from_secs(10);
        let out = ZFS
            .run_read("/bin/sh", &["-c", "echo $LC_ALL"], long)
            .await
            .unwrap();
        assert_eq!(out, b"C\n");
        let err = ZFS
            .run_change(
                "/bin/sh",
                &[
                    "-c",
                    "echo \"cannot open 'x': dataset does not exist\" >&2; exit 1",
                ],
                &[],
                long,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m == "/bin/sh: cannot open 'x': dataset does not exist"),
            "{err:?}"
        );
    }

    #[test]
    fn parses_zfs_output() {
        let zvols =
            parse_zvols(b"tank/opl/opl-a\t1073741824\t57344\ntank/opl/opl-b\t2097152\t8192\n\n")
                .unwrap();
        assert_eq!(
            zvols,
            [
                ZvolInfo {
                    name: "tank/opl/opl-a".into(),
                    size_bytes: 1 << 30,
                    used_bytes: 57344,
                },
                ZvolInfo {
                    name: "tank/opl/opl-b".into(),
                    size_bytes: 2 * MIB,
                    used_bytes: 8192,
                },
            ]
        );
        assert_eq!(parse_zvols(b"").unwrap(), []);
        let snapshots =
            parse_snapshots(b"tank/opl/opl-a@s1\t1048576\ntank/opl/opl-a@s2\t2097152\n").unwrap();
        assert_eq!(
            snapshots,
            [
                ZfsSnapshot {
                    tag: "s1".into(),
                    size_bytes: MIB,
                },
                ZfsSnapshot {
                    tag: "s2".into(),
                    size_bytes: 2 * MIB,
                },
            ]
        );
        assert_eq!(
            parse_space(b"6442450944\n2147483648\n").unwrap(),
            ZfsSpace {
                available_bytes: 6 << 30,
                used_bytes: 2 << 30,
            }
        );
        for err in [
            parse_zvols(b"tank/opl/opl-a\t-\t0\n").unwrap_err(),
            parse_zvols(b"tank/opl/opl-a\t1\n").unwrap_err(),
            parse_snapshots(b"tank/opl/opl-a\t1\n").unwrap_err(),
            parse_space(b"1\n").unwrap_err(),
            parse_space(b"1\t2\n3\n").unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }
}
```

`crates/opld/src/member.rs`에서

```rust
    DirStorage, Hypervisor, LvmThinStorage, QemuHypervisor, Storage, Storages, SystemLvm,
};
```

을 다음으로 바꾼다:

```rust
    DirStorage, Hypervisor, LvmThinStorage, QemuHypervisor, Storage, Storages, SystemLvm,
    SystemZfs, ZfsStorage,
};
```

`crates/opld/src/member.rs`에서

```rust
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> =
            Arc::new(Storages::new(&config.qemu_img, files).with_lvm_thin(Arc::new(lvm)));
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
```

을 다음으로 바꾼다:

```rust
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs)),
        );
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 539개, 실패 0

Run: `cargo test -p opl-driver --lib storage::zfs`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(storage): ZFS zvol 드라이버"
```

---

### Task 4: 마운터의 ZFS 상태와 resolve

`ZfsDataset { kind, space }`와 `parse_zfs_dataset`, `MountOps::zfs_dataset`(실제는 `zfs get -Hp -o value available,used,type`, 10초), 마운터의 `block_checks`(데이터셋이 `volume`이 아니면 준비, 용량은 available+used(넘치지 않게)와 available, 아니면 이유), Linux가 아니면 `"zfs pools are not supported on macOS"`. resolve는 ZFS 디스크를 zvol 장치로, Linux가 아니면 `PoolUnsupported`.

**Files:**
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opld/src/pools.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/lib.rs`에서

```rust
    Capacity, DirStorage, LvInfo, LvmOps, LvmThinStorage, Storage, Storages, SystemLvm, SystemZfs,
    VolumeInfo, VolumeSource, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, check_pool_root,
    disk_location, parse_lvs, pool_root,
};
```

을 다음으로 바꾼다:

```rust
    Capacity, DirStorage, LvInfo, LvmOps, LvmThinStorage, Storage, Storages, SystemLvm, SystemZfs,
    VolumeInfo, VolumeSource, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo,
    check_pool_root, disk_location, parse_lvs, parse_zfs_dataset, pool_root,
};
```

`crates/opl-driver/src/storage.rs`에서

```rust
pub use storages::Storages;
pub use zfs::{SystemZfs, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo};
```

을 다음으로 바꾼다:

```rust
pub use storages::Storages;
pub use zfs::{
    SystemZfs, ZfsDataset, ZfsOps, ZfsSnapshot, ZfsSpace, ZfsStorage, ZvolInfo, parse_zfs_dataset,
};
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
    pub available_bytes: u64,
    pub used_bytes: u64,
}
```

을 다음으로 바꾼다:

```rust
    pub available_bytes: u64,
    pub used_bytes: u64,
}

/// `zfs get -Hp -o value available,used,type`로 읽은 데이터셋(opld 마운터가 풀을 확인한다)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsDataset {
    /// `type`: `filesystem`, `volume` 등
    pub kind: String,
    pub space: ZfsSpace,
}
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
        _ => Err(bad_output(format!("space {values:?}"))),
    }
```

을 다음으로 바꾼다:

```rust
        _ => Err(bad_output(format!("space {values:?}"))),
    }
}

/// `zfs get -Hp -o value available,used,type`의 출력
pub fn parse_zfs_dataset(out: &[u8]) -> Result<ZfsDataset, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [available, used, kind] if [available, used, kind].iter().all(|v| v.len() == 1) => {
            Ok(ZfsDataset {
                kind: kind[0].trim().to_string(),
                space: ZfsSpace {
                    available_bytes: number(available[0])?,
                    used_bytes: number(used[0])?,
                },
            })
        }
        _ => Err(bad_output(format!("dataset {values:?}"))),
    }
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }
}
```

을 다음으로 바꾼다:

```rust
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }

    #[test]
    fn parses_zfs_dataset() {
        assert_eq!(
            parse_zfs_dataset(b"6442450944\n2147483648\nfilesystem\n").unwrap(),
            ZfsDataset {
                kind: "filesystem".into(),
                space: ZfsSpace {
                    available_bytes: 6 << 30,
                    used_bytes: 2 << 30,
                },
            }
        );
        for bad in [&b"1\n2\n"[..], b"1\nx\nfilesystem\n", b"1\n2\nvolume\n3\n"] {
            let err = parse_zfs_dataset(bad).unwrap_err();
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }
}
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. Linux가 아닌 노드에서 NFS 풀은
/// `NfsUnsupported`, LVM-thin 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다. 저장소의 일시 오류는 `PoolUnavailable`, 그 밖은
```

을 다음으로 바꾼다:

```rust
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다. Linux가 아닌 노드에서 NFS 풀은
/// `NfsUnsupported`, LVM-thin·ZFS 풀은 `PoolUnsupported`(참조 오류)다. 디스크마다 `storage`로 볼륨을
/// 활성화한 뒤 [`disk_location`]으로 위치를 정한다. 저장소의 일시 오류는 `PoolUnavailable`, 그 밖은
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
        }
        // ZFS 풀의 디스크는 아직 다루지 않는다.
        if matches!(pool.spec.backend, PoolBackend::Zfs { .. }) {
            return Err(ResolveError::PoolUnsupported(
                pool_name.clone(),
                "zfs pools are not supported yet",
            ));
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM을 쓴다.
        if !cfg!(target_os = "linux") {
```

을 다음으로 바꾼다:

```rust
        }
        // opld는 Linux에서만 NFS 풀을 마운트하고 LVM·ZFS를 쓴다.
        if !cfg!(target_os = "linux") {
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
                }
                PoolBackend::Dir { .. } | PoolBackend::Zfs { .. } => {}
            }
```

을 다음으로 바꾼다:

```rust
                }
                PoolBackend::Zfs { .. } => {
                    return Err(ResolveError::PoolUnsupported(
                        pool_name.clone(),
                        "zfs pools are not supported on macOS",
                    ));
                }
                PoolBackend::Dir { .. } => {}
            }
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

/// ZFS 풀의 디스크는 아직 쓸 수 없다. 기다려도 풀리지 않으므로 참조 오류이고, 볼륨을 활성화하지 않는다.
#[tokio::test]
async fn refuses_zfs_volumes_for_now() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let z: StoragePool = Resource::new(
```

을 다음으로 바꾼다:

```rust

/// ZFS 풀 `z`(데이터셋 `tank/opl`)와 그 raw 볼륨 `root`를 쓰는 VM을 만든다.
async fn zfs_vm(cluster: &opl_cluster::Cluster, node: Uuid) -> (Volume, Vm) {
    let z: StoragePool = Resource::new(
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

/// ZFS를 지원하지 않는 노드(macOS)에서는 ZFS 풀의 디스크를 쓸 수 없다. 기다려도 풀리지 않으므로
/// 참조 오류이고, 볼륨을 활성화하지 않는다.
#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn refuses_zfs_volumes_on_macos() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (_, vm) = zfs_vm(&cluster, node).await;
    let storage = Activations::default();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "z" && why.contains("not supported")),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}
```

을 다음으로 바꾼다:

```rust
    assert!(
        matches!(&err, ResolveError::PoolUnsupported(n, why) if n == "z" && why == &"zfs pools are not supported on macOS"),
        "{err:?}"
    );
    assert!(storage.calls.lock().unwrap().is_empty());
}

/// Linux에서는 ZFS 볼륨을 활성화(장치 노드 확인)한 뒤 zvol 장치를 디스크로 쓴다. 장치 노드가 없으면
/// 잠시 뒤 다시 시도할 `PoolUnavailable`이다.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn activates_zfs_volumes_and_waits_for_the_device() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let (root, vm) = zfs_vm(&cluster, node).await;
    let storage = Activations::default();
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT), &storage)
        .await
        .unwrap_err();
    assert_eq!(
        *storage.calls.lock().unwrap(),
        vec![("z".to_string(), root.id, ImageFormat::Raw)]
    );
    let device = format!("/dev/zvol/tank/opl/opl-{}", root.id);
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "z" && e.contains(&device)),
        "{err:?}"
    );
}
```

`crates/opld/src/pools.rs` 전체:

```rust
//! 노드별 풀 상태. 이 노드가 쓸 수 있는 풀마다 준비 여부와 용량을 `NodeStatus.pools`에 적는다.
//! NFS 풀은 `{data_dir}/pools/{풀 id}`에 마운트하고, 이 노드가 쓸 수 없게 된 NFS 풀은 언마운트한다.
//! LVM-thin 풀은 `lvs`로 thin pool을, ZFS 풀은 `zfs get`으로 데이터셋을 확인한다.
//! opld가 내려가도 마운트는 그대로 둔다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{Capacity, LvInfo, ZfsDataset};
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
/// 쓰기 확인과 용량 읽기(LVM-thin은 `lvs`, ZFS는 `zfs get`)를 기다리는 시간. NFS 서버가 멈추면 이 작업이 멈춘다.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const NFS_UNSUPPORTED: &str = "nfs pools are not supported on macOS";
const LVM_UNSUPPORTED: &str = "lvm-thin pools are not supported on macOS";
const ZFS_UNSUPPORTED: &str = "zfs pools are not supported on macOS";
const LVS_TIMED_OUT: &str = "lvs timed out";
const ZFS_TIMED_OUT: &str = "zfs timed out";
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
}

/// 실제 명령과 파일시스템
pub struct SystemMount;

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
    pub fn spawn(cluster: Arc<Cluster>, mount_root: PathBuf, refresh: Duration) -> Self {
        let pools = Pools::new(cluster, mount_root, Arc::new(SystemMount));
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
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
Expected: PASS 542개, 실패 0

Run: `cargo test -p opld --bin opld pools`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(storage): ZFS 풀 마운터 확인과 resolve 지원"
```

---

### Task 5: 스냅샷 규칙을 블록 백엔드로 일반화

`block_snapshot_kind`·`uses_block_snapshots`·`vm_uses_block_snapshots`(LVM-thin, ZFS). LVM과 ZFS가 섞이면 409, 블록 디스크의 메모리 스냅샷은 409(예약은 메모리 없이). 실행 중·멈춘 VM 모두 풀·형식으로 묶어 `snapshot_many`(시간 초과면 끝나지 않은 풀의 태그는 남긴다). ZFS 되돌리기는 API가 VM의 가장 최근 스냅샷이 아니면 409, Task가 VM을 멈추기 전에 디스크마다 실제 마지막 태그를 확인한다. 남은 블록 스냅샷 태그는 시간 초과·중단·Task 제한 뒤 `orphan_retry` 간격으로 최대 10번 지우고(실행기 종료 때 멈춤), 실행기가 시작할 때 Snapshot 리소스가 없는 UUID 모양 태그를 지운다. 테스트용 FakeStorage는 볼륨별 태그와 `snapshot_groups()`를 갖는다.

**Files:**
- Modify: `crates/opl-api/src/schedule.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/schedule.rs`에서

```rust
use crate::error::ApiError;
use crate::snapshots::{start_create, start_delete, vm_placement, vm_uses_lv_snapshots};
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
use crate::error::ApiError;
use crate::snapshots::{start_create, start_delete, vm_placement, vm_uses_block_snapshots};
use crate::state::AppState;
```

`crates/opl-api/src/schedule.rs`에서

```rust
/// 다시 한다. 지우려 한 스냅샷이 아직 있으면(지우기가 실패했다) 그 삭제는 한 시간 뒤에 다시 하고,
/// 그동안은 만들기를 본다(보관 정리가 막혀도 새 스냅샷은 만든다). `lv_disks`면 VM의 디스크가 LV
/// 스냅샷을 쓰므로(메모리 상태를 담지 못한다) 예약이 메모리를 원해도 디스크만 찍는다.
```

을 다음으로 바꾼다:

```rust
/// 다시 한다. 지우려 한 스냅샷이 아직 있으면(지우기가 실패했다) 그 삭제는 한 시간 뒤에 다시 하고,
/// 그동안은 만들기를 본다(보관 정리가 막혀도 새 스냅샷은 만든다). `block_disks`면 VM의 디스크가 블록
/// 스냅샷을 쓰므로(메모리 상태를 담지 못한다) 예약이 메모리를 원해도 디스크만 찍는다.
```

`crates/opl-api/src/schedule.rs`에서

```rust
    attempts: &Attempts,
    lv_disks: bool,
) -> Option<Plan> {
```

을 다음으로 바꾼다:

```rust
    attempts: &Attempts,
    block_disks: bool,
) -> Option<Plan> {
```

`crates/opl-api/src/schedule.rs`에서

```rust
        name: auto_name(now, snapshots),
        memory: schedule.memory && running && !vm.spec.disks.is_empty() && !lv_disks,
    })
```

을 다음으로 바꾼다:

```rust
        name: auto_name(now, snapshots),
        memory: schedule.memory && running && !vm.spec.disks.is_empty() && !block_disks,
    })
```

`crates/opl-api/src/schedule.rs`에서

```rust
        let attempted = attempts.get(&vm.id).copied().unwrap_or_default();
        let Ok(lv_disks) = vm_uses_lv_snapshots(cluster, vm) else {
            continue;
        };
        let Some(plan) = plan(now, config.hour, vm, &mine, &tasks, &attempted, lv_disks) else {
            continue;
```

을 다음으로 바꾼다:

```rust
        let attempted = attempts.get(&vm.id).copied().unwrap_or_default();
        let Ok(block_disks) = vm_uses_block_snapshots(cluster, vm) else {
            continue;
        };
        let Some(plan) = plan(now, config.hour, vm, &mine, &tasks, &attempted, block_disks) else {
            continue;
```

`crates/opl-api/src/schedule.rs`에서

```rust
        );
        // LV 스냅샷을 쓰는 디스크는 메모리를 담지 못한다(요청하면 거절된다).
        let v = vm(ScheduleEvery::Hourly, 3, true);
```

을 다음으로 바꾼다:

```rust
        );
        // 블록 스냅샷을 쓰는 디스크는 메모리를 담지 못한다(요청하면 거절된다).
        let v = vm(ScheduleEvery::Hourly, 3, true);
```

`crates/opl-api/src/snapshots.rs` 전체:

```rust
//! VM 스냅샷 목록·조회·만들기·되돌리기·삭제. 권한은 VM을 따른다(보기는 Read, 나머지는 Operate).
//! 스냅샷이나 꺼진 VM에서 VM 복제(VM·볼륨 Write).
//! 실제 작업은 VM이 있는 노드의 Task 실행기가 한다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, VolumePhase, VolumeSpec, is_shared,
    pool_kind_supports_snapshots,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{is_stopped, prepare};
use crate::volumes::{check_pool_on, usable_node};

/// 설명의 최대 글자 수
const MAX_DESCRIPTION_CHARS: usize = 1024;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSnapshotRequest {
    /// VM 안에서 고유한 이름
    pub name: String,
    /// 최대 1024자
    #[serde(default)]
    pub description: String,
    /// 메모리 상태도 담는다. 기본은 VM이 실행 중이고 디스크가 있으면 true다.
    pub memory: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 복제할 스냅샷(이름이나 id). 없으면 꺼진 VM의 지금 상태에서 복제한다.
    pub snapshot: Option<String>,
    /// 새 디스크를 둘 풀. 없으면 원본 볼륨마다 그 볼륨의 풀이다.
    pub pool: Option<String>,
}

/// VM의 디스크 작업을 할 노드: 다음에 돌 노드(`spec.node`), 지금(또는 마지막으로) 돈 노드
/// (`status.node`), 배치된 적이 없으면 첫 디스크 볼륨을 만든 노드. 꺼진 VM을 다른 노드로 옮겼으면
/// 그 디스크는 옮긴 노드에서 쓰인다. 첫 디스크가 공유 풀에 있으면 볼륨을 만든 노드 대신 풀을 쓸 수
/// 있는 온라인 노드(`prefer`가 그런 노드면 그 노드, 아니면 id가 가장 작은 노드)다. 정할 수 없으면 `None`.
pub(crate) fn vm_placement(cluster: &Cluster, prefer: Option<Uuid>, vm: &Vm) -> Option<Uuid> {
    vm.spec.node.or(vm.status.node).or_else(|| {
        let disk = vm.spec.disks.first()?;
        let volume = cluster.get_by_name::<VolumeSpec>(&disk.volume).ok()??;
        match cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
            .ok()?
        {
            Some(pool) if is_shared(&pool) => usable_node(cluster, &pool, prefer).ok()?,
            _ => volume.status.node,
        }
    })
}

/// 스냅샷 Task를 실행할 노드: [`vm_placement`], 없으면 요청을 받은 이 노드(설계 §3).
pub(crate) fn vm_node(state: &AppState, vm: &Vm) -> Uuid {
    vm_placement(&state.cluster, Some(state.node), vm).unwrap_or(state.node)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다: phase가 Starting·Stopping이거나, 원하는 전원
/// 상태(`spec.power`)에 아직 닿지 않았거나(reconciler가 켜거나 끄는 중), 전원 Task(시작·정지·
/// 강제 정지·재시작)가 끝나지 않았으면 409 `vm_busy`. 전원 조작이 스냅샷 Task를 기다리는
/// [`check_no_snapshot_task`]의 반대쪽이다.
fn check_not_busy(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let settled = match vm.spec.power {
        PowerState::Running => vm.status.phase == VmPhase::Running,
        PowerState::Stopped => is_stopped(vm),
    };
    let doing = match vm.status.phase {
        VmPhase::Starting => Some("starting"),
        VmPhase::Stopping => Some("stopping"),
        _ if !settled => Some(match vm.spec.power {
            PowerState::Running => "starting",
            PowerState::Stopped => "stopping",
        }),
        _ => None,
    };
    if let Some(doing) = doing {
        return Err(ApiError::conflict(
            "vm_busy",
            format!("VM {:?} is {doing}; try again when it has settled", vm.name),
        ));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::VmStart | TaskOp::VmStop | TaskOp::VmForceStop | TaskOp::VmReboot
            )
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is being started, stopped or rebooted; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 이 VM의 스냅샷 만들기·되돌리기나 복제 Task가 끝나지 않았으면 409 `vm_busy`. 스냅샷 Task는
/// 전원을 끄고 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나
/// 담는 스펙이 실제와 달라진다. 꺼진 VM을 복사하는 동안 켜면 디스크 잠금 때문에 시작이 실패한다. 시작한 Task는 실행기의 시간 제한 안에 끝나지만, 오프라인 노드에 배정된
/// Task는 그 노드가 돌아올 때까지 남는다(설계 §11).
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being snapshotted, reverted or cloned", |op| {
        matches!(
            op,
            TaskOp::SnapshotCreate { .. } | TaskOp::SnapshotRevert { .. } | TaskOp::VmClone { .. }
        )
    })
}

/// 이 VM의 스냅샷 되돌리기 Task가 끝나지 않았으면 409 `vm_busy`. 강제 정지는 이것만 막는다
/// (설계 §4). 만들기 중에 끄면 저장이 끊겨 만들기가 실패하고, 복제는 꺼진 VM이거나 실행 중인
/// VM의 스냅샷을 `-U`로 읽으므로 꺼도 된다. 되돌리기는 디스크를 바꾸는 중에 끄고 켜는 것이 섞인다.
pub(crate) fn check_no_revert_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being reverted to a snapshot", |op| {
        matches!(op, TaskOp::SnapshotRevert { .. })
    })
}

fn check_no_task(
    state: &AppState,
    vm: &Vm,
    doing: &str,
    op: impl Fn(&TaskOp) -> bool,
) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks
        .iter()
        .any(|t| t.spec.target == vm.id && !t.status.state.is_finished() && op(&t.spec.op));
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is {doing}; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// 디스크 스냅샷을 블록 장치의 스냅샷으로 두는 풀의 종류(`lvm-thin`, `zfs`). 파일 풀(qcow2 내부
/// 스냅샷)이면 `None`.
pub(crate) fn block_snapshot_kind(pool: &StoragePool) -> Option<&'static str> {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } => Some("lvm-thin"),
        PoolBackend::Zfs { .. } => Some("zfs"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
    }
}

/// 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS)인지. 아니면 qcow2 내부 스냅샷이다.
pub(crate) fn uses_block_snapshots(pool: &StoragePool) -> bool {
    block_snapshot_kind(pool).is_some()
}

/// VM의 디스크 중 블록 스냅샷을 쓰는 것([`uses_block_snapshots`])이 있는지. 없는 볼륨·풀은 넘어간다.
pub(crate) fn vm_uses_block_snapshots(cluster: &Cluster, vm: &Vm) -> Result<bool, ClusterError> {
    for disk in &vm.spec.disks {
        let Some(volume) = cluster.get_by_name::<VolumeSpec>(&disk.volume)? else {
            continue;
        };
        if let Some(pool) = cluster.get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
            && uses_block_snapshots(&pool)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 모든 디스크가 스냅샷을 담을 수 있고 다 만들어졌는지. 풀 종류마다 담는 형식이 다르다
/// ([`pool_kind_supports_snapshots`]: 파일 풀은 qcow2, 블록 풀은 raw). 담지 못하는 디스크가
/// 하나라도 있으면 일부 디스크만 되돌아가므로 거부하고, 방식(qcow2 내부 스냅샷, 블록 백엔드
/// 종류마다의 블록 스냅샷)이 섞여도 거부한다. 디스크가 블록 스냅샷을 쓰면 그 종류
/// ([`block_snapshot_kind`]).
fn check_disks(state: &AppState, vm: &Vm) -> Result<Option<&'static str>, ApiError> {
    let mut method = None;
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        let pool = state
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
            .ok_or_else(|| {
                ApiError::bad_request(format!("storage pool {:?} not found", volume.spec.pool))
            })?;
        if !pool_kind_supports_snapshots(&pool.spec.backend, volume.spec.format) {
            let only = if uses_block_snapshots(&pool) {
                "raw"
            } else {
                "qcow2"
            };
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} has disk {:?} that cannot be snapshotted; only {only} disks in \
                     storage pool {:?} can be snapshotted",
                    vm.name, disk.volume, pool.name
                ),
            ));
        }
        let this = block_snapshot_kind(&pool);
        if method.replace(this).is_some_and(|before| before != this) {
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} cannot be snapshotted: disks use different snapshot methods",
                    vm.name
                ),
            ));
        }
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
    }
    Ok(method.flatten())
}

/// ZFS는 볼륨의 가장 최근 스냅샷으로만 되돌린다(설계 §4). `snapshot`의 디스크에 ZFS 볼륨이 있고 VM에
/// 그보다 나중의 스냅샷(만드는 중인 것 포함)이 있으면 409 `snapshot_unsupported`. 순서는 만든 시각,
/// 같으면 id(UUIDv7)다. 드라이버도 되돌릴 때 같은 확인을 한다.
fn check_zfs_latest(state: &AppState, vm: &Vm, snapshot: &Snapshot) -> Result<(), ApiError> {
    let mut zfs = false;
    for disk in &snapshot.spec.vm_spec.disks {
        let Some(volume) = state.cluster.get_by_name::<VolumeSpec>(&disk.volume)? else {
            continue;
        };
        if let Some(pool) = state
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
            && matches!(pool.spec.backend, PoolBackend::Zfs { .. })
        {
            zfs = true;
            break;
        }
    }
    if !zfs {
        return Ok(());
    }
    // 만드는 중인 스냅샷은 가장 나중이다.
    let order = |s: &Snapshot| (s.status.created_at.unwrap_or(u64::MAX), s.id);
    let newer = snapshots_of(state, vm.id)?
        .1
        .iter()
        .any(|s| order(s) > order(snapshot));
    if newer {
        return Err(ApiError::conflict(
            "snapshot_unsupported",
            format!(
                "zfs disks can only revert to the latest snapshot; snapshot {:?} of VM {:?} is \
                 not the latest",
                snapshot.spec.name, vm.name
            ),
        ));
    }
    Ok(())
}

/// VM의 스냅샷 전부
fn snapshots_of(state: &AppState, vm: Uuid) -> Result<(u64, Vec<Snapshot>), ApiError> {
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok((
        revision,
        items.into_iter().filter(|s| s.spec.vm == vm).collect(),
    ))
}

/// 경로의 `{name}`은 VM 안의 스냅샷 이름이나 id다.
fn find_snapshot(state: &AppState, vm: &Vm, key: &str) -> Result<Snapshot, ApiError> {
    let id = Uuid::try_parse(key).ok();
    snapshots_of(state, vm.id)?
        .1
        .into_iter()
        .find(|s| Some(s.id) == id || s.spec.name == key)
        .ok_or_else(|| {
            ApiError::not_found(format!("snapshot {key:?} of VM {:?} not found", vm.name))
        })
}

fn check_ready(snapshot: &Snapshot) -> Result<(), ApiError> {
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err(ApiError::conflict(
            "snapshot_not_ready",
            format!("snapshot {:?} is still being created", snapshot.spec.name),
        ));
    }
    Ok(())
}

/// 모든 VM의 스냅샷. watch와 함께 쓰는 목록이다.
#[utoipa::path(get, path = "/snapshots", tag = "snapshots", responses(
    (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
    (status = "4XX", body = crate::error::ErrorBody),
))]
pub async fn list_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let (revision, items) = state.cluster.list::<SnapshotSpec>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    responses(
        (status = 200, body = List<Resource<SnapshotSpec, SnapshotStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_vm_snapshots(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<List<Snapshot>>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let (revision, items) = snapshots_of(&state, vm.id)?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses(
        (status = 200, body = Resource<SnapshotSpec, SnapshotStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<Json<Snapshot>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    Ok(Json(find_snapshot(&state, &vm, &name)?))
}

/// `pending` 스냅샷을 만들고(부모는 VM의 현재 위치, `vm_spec`은 지금 스펙) 그것을 채울 Task를
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, 스냅샷을 담지 못하는 디스크가 있거나 디스크의
/// 스냅샷 방식이 섞였거나 블록 디스크(LVM-thin, ZFS)의 VM에 메모리 스냅샷을 요청하면 409
/// `snapshot_unsupported`. 실행 중이 아니거나 디스크가 없는 VM의 메모리 스냅샷은 400.
#[utoipa::path(post, path = "/vms/{key}/snapshots", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateSnapshotRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateSnapshotRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    check_name(&req.name)?;
    if req.description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(ApiError::bad_request(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_create(&state, &vm, req.name, req.description, req.memory, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`create_snapshot`]의 본문: `pending` 스냅샷과 그것을 채울 Task를 만든다. 예약 작업도 쓴다
/// (`scheduled`). 이름과 설명은 부르는 쪽이 확인한다.
pub(crate) async fn start_create(
    state: &AppState,
    vm: &Vm,
    name: String,
    description: String,
    memory: Option<bool>,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    let block = check_disks(state, vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    // 블록 스냅샷에는 메모리 상태를 담을 qcow2가 없다.
    if let Some(kind) = block
        && memory == Some(true)
    {
        return Err(ApiError::conflict(
            "snapshot_unsupported",
            format!(
                "VM {:?} has {kind} disks; their snapshots cannot hold the memory state",
                vm.name
            ),
        ));
    }
    let memory = memory.unwrap_or(running && has_disk && block.is_none());
    if memory && !running {
        return Err(ApiError::bad_request(format!(
            "VM {:?} is not running; only its disks can be snapshotted",
            vm.name
        )));
    }
    if memory && !has_disk {
        return Err(ApiError::bad_request(format!(
            "VM {:?} has no disk to hold the memory state",
            vm.name
        )));
    }
    let mut snapshot: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm: vm.id,
            name,
            parent: vm.status.snapshot,
            memory,
            description,
            scheduled,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    // 같은 VM에 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&snapshot).await?;
    let op = TaskOp::SnapshotCreate {
        snapshot: snapshot.id,
    };
    match create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await {
        Ok(task) => Ok(task),
        Err(e) => {
            // 채울 Task가 없는 pending 스냅샷은 이름만 차지한다. 지우지 못하면 주인 노드의
            // Task 실행기가 다음 시작에 고아로 지운다.
            if let Err(d) = state
                .cluster
                .delete::<SnapshotSpec>(snapshot.id, None)
                .await
            {
                tracing::warn!(snapshot = %snapshot.id, error = %d, "cannot remove snapshot without a task");
            }
            Err(e)
        }
    }
}

/// 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌릴 Task를 돌려준다(202). 실행 중이면 강제로 끈다.
/// 스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다. ZFS 디스크는 VM의
/// 가장 최근 스냅샷으로만 되돌린다(아니면 409 `snapshot_unsupported`).
#[utoipa::path(post, path = "/vms/{key}/snapshots/{name}/revert", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn revert_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_not_busy(&state, &vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    check_zfs_latest(&state, &vm, &snapshot)?;
    // 되돌린 스펙을 VM 수정처럼 검증한다. 노드는 지금 것을 유지하고, 시작은 Task가 한다.
    let mut spec = snapshot.spec.vm_spec.clone();
    spec.node = vm.spec.node;
    spec.power = PowerState::Stopped;
    prepare(&state, vm.id, &mut spec)?;
    let op = TaskOp::SnapshotRevert {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 스냅샷을 지울 Task를 돌려준다(202). 자식은 지운 스냅샷의 부모로 옮겨 붙는다.
#[utoipa::path(delete, path = "/vms/{key}/snapshots/{name}", tag = "snapshots",
    params(("key" = String, Path, description = "VM id or name"), ("name" = String, Path, description = "snapshot name or id")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_snapshot(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let task = start_delete(&state, &vm, &name, false).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`delete_snapshot`]의 본문: VM의 스냅샷 `key`(이름이나 id)를 지울 Task를 만든다. 예약 작업도
/// 쓴다(`scheduled`).
pub(crate) async fn start_delete(
    state: &AppState,
    vm: &Vm,
    key: &str,
    scheduled: bool,
) -> Result<Task, ApiError> {
    check_not_busy(state, vm)?;
    let snapshot = find_snapshot(state, vm, key)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
        scheduled,
    };
    create_task(&state.cluster, op, vm.id, vm_node(state, vm)).await
}

/// VM을 `name`으로 복제할 Task를 돌려준다(202). 원본은 `ready` 스냅샷이나 꺼진 VM의 지금 상태다
/// (실행 중인데 스냅샷이 없으면 409 `vm_running`). 원본 스펙의 볼륨이 모두 있어야 하고, `pool`은 Task
/// 노드에서 쓸 수 있어야 한다. Task는 원본 VM을 대상으로 그 VM의 디스크 노드에서 돈다.
#[utoipa::path(post, path = "/vms/{key}/clone", tag = "vms",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CloneRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn clone_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CloneRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    check_not_busy(&state, &vm)?;
    let (snapshot, spec) = match &req.snapshot {
        Some(key) => {
            let snapshot = find_snapshot(&state, &vm, key)?;
            check_ready(&snapshot)?;
            (Some(snapshot.id), snapshot.spec.vm_spec)
        }
        None if !is_stopped(&vm) => {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "VM {:?} is running; stop it or clone from a snapshot",
                    vm.name
                ),
            ));
        }
        None => (None, vm.spec.clone()),
    };
    let node = vm_node(&state, &vm);
    if let Some(name) = &req.pool {
        let pool = state
            .cluster
            .get_by_name::<StoragePoolSpec>(name)?
            .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
        check_pool_on(&state, &pool, node, || {
            format!(
                "storage pool {name:?} is not available on the node holding VM {:?}",
                vm.name
            )
        })?;
    }
    for disk in &spec.disks {
        if state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "volume {:?} not found",
                disk.volume
            )));
        }
    }
    // 새 VM은 원본 스펙의 NIC를 그대로 가진다(VM 만들기와 같은 검사).
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    let op = TaskOp::VmClone {
        name: req.name,
        snapshot,
        pool: req.pool,
    };
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM의 스냅샷 수. VM 삭제를 막는 데 쓴다.
pub(crate) fn count_snapshots(state: &AppState, vm: Uuid) -> Result<usize, ApiError> {
    Ok(snapshots_of(state, vm)?.1.len())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn block_pools_use_block_snapshots_of_their_kind() {
        let dir = pool(PoolBackend::Dir {
            path: "/pools/local".into(),
        });
        let lvm = pool(PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        });
        let zfs = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        assert_eq!(block_snapshot_kind(&dir), None);
        assert_eq!(block_snapshot_kind(&lvm), Some("lvm-thin"));
        assert_eq!(block_snapshot_kind(&zfs), Some("zfs"));
        assert!(!uses_block_snapshots(&dir));
        assert!(uses_block_snapshots(&zfs));
    }
}
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
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};

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
    let tags = tokio::spawn(sweep_block_tags(ctx.clone()));
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

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. opLite 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready
            || volume.status.node.is_some_and(|n| n != ctx.config.node)
        {
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
        let format = volume.spec.format;
        let tags = match ctx.storage.snapshot_tags(&pool, volume.id, format).await {
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

`crates/opl-api/src/tasks/clone.rs`에서

```rust
use super::{Ctx, mark_ready, remove_volume};
use crate::snapshots::uses_lv_snapshots;
```

을 다음으로 바꾼다:

```rust
use super::{Ctx, mark_ready, remove_volume};
use crate::snapshots::uses_block_snapshots;
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust

/// 복사본의 형식(설계 §5): LVM-thin 풀은 raw만 담는다. LVM-thin 볼륨(raw)을 파일 풀로 옮기면
/// 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (uses_lv_snapshots(source), uses_lv_snapshots(target)) {
        (_, true) => ImageFormat::Raw,
```

을 다음으로 바꾼다:

```rust

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS)은 raw만 담는다. 블록 볼륨(raw)을 파일 풀로
/// 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (uses_block_snapshots(source), uses_block_snapshots(target)) {
        (_, true) => ImageFormat::Raw,
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
        assert_eq!(copy_format(&lvm, &dir, raw), qcow2);
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(copy_format(&lvm, &dir, raw), qcow2);
        // ZFS 풀도 블록 풀이다.
        let zfs = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        assert_eq!(copy_format(&dir, &zfs, qcow2), raw);
        assert_eq!(copy_format(&zfs, &lvm, raw), raw);
        assert_eq!(copy_format(&lvm, &zfs, raw), raw);
        assert_eq!(copy_format(&zfs, &dir, raw), qcow2);
    }
```

`crates/opl-api/src/tasks/snapshot.rs` 전체:

```rust
//! 스냅샷 Task: 만들기·되돌리기·삭제(설계 §5). 같은 VM의 스냅샷 Task는 실행기가 차례로 돌린다.
//! API가 확인한 뒤 Task가 줄 서 기다리는 사이 VM이 바뀌었을 수 있으므로 여기서 다시 확인한다.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{DriverError, Hypervisor, RuntimeStatus, uefi_vars_path, vm_state_dir};
use opl_types::node::NodeSpec;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
};
use opl_types::task::Task;
use opl_types::vm::{PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use crate::schedule::{recorded_vm, set_schedule_error};
use crate::snapshots::{block_snapshot_kind, uses_block_snapshots};
use crate::vms::check_node_arch;

use super::{Ctx, set_power, set_volume_size, started, stopped, superseded, wait_vm};

/// 실행 중인 VM을 블록 스냅샷을 위해 멈춰 두는 한도. Task 한도(600초)보다 충분히 짧다.
const PAUSE_LIMIT: Duration = Duration::from_secs(120);

/// 시간을 넘겨 실패한 블록 스냅샷이 남긴 태그를 뒤에서 다시 지워 보는 횟수. 간격은 `orphan_retry`다.
const LEFT_TAG_TRIES: u32 = 10;

/// 멈춘 VM을 이어서 실행하는 시도 횟수와 그 간격
const RESUME_TRIES: u32 = 3;
const RESUME_INTERVAL: Duration = Duration::from_secs(1);

/// 멈춘 VM을 이어서 실행한다. 일시 오류면 [`RESUME_INTERVAL`] 간격으로 모두 [`RESUME_TRIES`]번
/// 해 본다. VM이 꺼졌으면 이어서 실행할 것이 없으므로 성공이다.
async fn resume_vm(hv: &dyn Hypervisor, id: Uuid) -> Result<(), DriverError> {
    let mut tries = 1;
    loop {
        match hv.resume(id).await {
            Err(DriverError::NotRunning(_)) => return Ok(()),
            Err(e) if e.is_transient() && tries < RESUME_TRIES => {
                tracing::warn!(vm = %id, error = %e, "cannot resume VM; retrying");
                tries += 1;
                tokio::time::sleep(RESUME_INTERVAL).await;
            }
            result => return result,
        }
    }
}

/// 스냅샷을 위해 멈춘 VM. 이어서 실행해 보기 전에 버려지면(Task가 시간을 넘기거나 opld가 끝나
/// future가 버려짐) 뒤에서 이어서 실행한다. 해 보고도 못 했거나 opld가 멈춘 채 죽으면 하이퍼바이저가
/// 다시 붙을 때 표시를 보고 이어서 실행한다.
struct Paused {
    hv: Arc<dyn Hypervisor>,
    id: Uuid,
    /// 이어서 실행하기를 (재시도까지) 해 봤다
    tried: bool,
}

impl Paused {
    fn new(hv: &Arc<dyn Hypervisor>, id: Uuid) -> Self {
        Self {
            hv: hv.clone(),
            id,
            tried: false,
        }
    }

    /// 이어서 실행한다([`resume_vm`]). 끝나면 결과와 상관없이 가드를 끈다.
    async fn resume(&mut self) -> Result<(), DriverError> {
        let resumed = resume_vm(&*self.hv, self.id).await;
        self.tried = true;
        resumed
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        if self.tried {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (hv, id) = (self.hv.clone(), self.id);
        rt.spawn(async move {
            if let Err(e) = resume_vm(&*hv, id).await {
                tracing::error!(vm = %id, error = %e, "cannot resume VM paused for block snapshots");
            }
        });
    }
}

/// 시간을 넘겨 실패한 블록 스냅샷 `snapshot`이 디스크 `disks`에 남겼을 수 있는 태그를 뒤에서
/// 지운다. 명령이 아직 돌 수 있으므로 `orphan_retry` 뒤부터 그 간격으로 [`LEFT_TAG_TRIES`]번까지
/// 해 본다. 그 id의 스냅샷 리소스가 있으면 그 스냅샷의 태그이므로 그 차례는 건너뛴다. 그래도 남은
/// 태그는 실행기가 다음에 시작할 때 지운다. 실행기가 멈추면 함께 멈춘다.
fn remove_left_tags(ctx: &Ctx, snapshot: Uuid, mut disks: Vec<(Volume, StoragePool)>) {
    let (cluster, storage) = (ctx.cluster.clone(), ctx.storage.clone());
    let interval = ctx.config.orphan_retry;
    let tag = snapshot.to_string();
    let mut stopped = ctx.stopped.subscribe();
    let remove = async move {
        for _ in 0..LEFT_TAG_TRIES {
            tokio::time::sleep(interval).await;
            if !matches!(cluster.get::<SnapshotSpec>(snapshot), Ok(None)) {
                continue;
            }
            let mut left = Vec::new();
            for (volume, pool) in disks {
                match storage
                    .snapshot_delete(&pool, volume.id, volume.spec.format, &tag)
                    .await
                {
                    Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
                    Err(e) => {
                        tracing::debug!(snapshot = %snapshot, volume = %volume.name, error = %e, "cannot remove block snapshot left by a timed-out snapshot yet");
                        left.push((volume, pool));
                    }
                }
            }
            disks = left;
            if disks.is_empty() {
                tracing::info!(snapshot = %snapshot, "removed block snapshots left by a timed-out snapshot");
                return;
            }
        }
        tracing::warn!(snapshot = %snapshot, "cannot remove block snapshots left by a timed-out snapshot");
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = stopped.wait_for(|s| *s) => {}
            () = remove => {}
        }
    });
}

/// 끝나지 않은 채(Task 시간 초과, 재시작으로 중단) 실패한 만들기 Task의 스냅샷 `id`가 이 노드의
/// 블록 디스크에 남겼을 수 있는 태그를 뒤에서 지운다([`remove_left_tags`]). 리소스를 지우기 전에
/// 부른다(디스크는 리소스의 VM 스펙에서 읽는다).
pub(super) fn remove_unfinished_tags(ctx: &Ctx, id: Uuid) {
    let Ok(Some(snapshot)) = get_snapshot(ctx, id) else {
        return;
    };
    let mut disks = Vec::new();
    for disk in &snapshot.spec.vm_spec.disks {
        if let Ok(Some((volume, pool))) = volume_by_name(ctx, &disk.volume)
            && uses_block_snapshots(&pool)
            && check_local(ctx, &volume, &pool).is_ok()
        {
            disks.push((volume, pool));
        }
    }
    if !disks.is_empty() {
        remove_left_tags(ctx, id, disks);
    }
}

pub(super) fn get_snapshot(ctx: &Ctx, id: Uuid) -> Result<Option<Snapshot>, String> {
    ctx.cluster
        .get::<SnapshotSpec>(id)
        .map_err(|e| e.to_string())
}

pub(super) fn get_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
    ctx.cluster
        .get::<VmSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "VM was deleted".into())
}

/// 이름으로 찾은 볼륨과 그 풀. 볼륨이 없으면 `None`.
fn volume_by_name(ctx: &Ctx, name: &str) -> Result<Option<(Volume, StoragePool)>, String> {
    let Some(volume) = ctx
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok(Some((volume, pool)))
}

/// VM 스펙의 디스크 볼륨들. 모두 있고 다 만들어졌어야 한다.
pub(super) fn ready_disks(ctx: &Ctx, spec: &VmSpec) -> Result<Vec<(Volume, StoragePool)>, String> {
    let mut disks = Vec::with_capacity(spec.disks.len());
    for disk in &spec.disks {
        let (volume, pool) = volume_by_name(ctx, &disk.volume)?
            .ok_or_else(|| format!("volume {:?} not found", disk.volume))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(format!("volume {:?} is not ready", volume.name));
        }
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다.
pub(super) fn check_settled(vm: &Vm) -> Result<(), String> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(format!("VM is {doing}; try again when it has settled"))
}

/// 스냅샷의 UEFI 변수 사본 `{state_dir}/vm/{vm}/snapshots/{snapshot}.vars`
pub(super) fn vars_copy(ctx: &Ctx, vm: Uuid, snapshot: Uuid) -> Option<PathBuf> {
    let dir = ctx.config.state_dir.as_ref()?;
    Some(
        vm_state_dir(dir, vm)
            .join("snapshots")
            .join(format!("{snapshot}.vars")),
    )
}

/// `from`을 `to`로 복사한다. 디렉터리가 없으면 만든다.
pub(super) async fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::copy(from, to).await.map(|_| ())
}

/// 이 노드에서 파일이 보이는 볼륨인지. 다른 노드에 만든 볼륨은 풀이 공유되지 않으면 오류다.
pub(super) fn check_local(ctx: &Ctx, volume: &Volume, pool: &StoragePool) -> Result<(), String> {
    match volume.status.node {
        Some(node) if node != ctx.config.node && !is_shared(pool) => {
            Err(format!("volume {:?} is on node {node}", volume.name))
        }
        _ => Ok(()),
    }
}

/// 블록 스냅샷을 위해 VM을 멈춰야 하는지: 하이퍼바이저가 실행 중(`Running`)이라고 알리면 그렇다.
/// 그 밖의 상태(일시정지, QEMU가 I/O 오류 등으로 스스로 멈춤)는 게스트가 디스크에 쓰지 않으므로
/// 멈추지도 이어서 실행하지도 않는다. QMP로 다룰 수 없거나, 꺼졌거나, 상태를 알 수 없으면 오류다.
async fn needs_pause(ctx: &Ctx, vm: Uuid) -> Result<bool, String> {
    match ctx.hv.inspect(vm).await {
        Ok(Some(s)) => match s.status {
            RuntimeStatus::Running => Ok(true),
            RuntimeStatus::Other(s) if s == "unresponsive" => {
                Err("VM is not responding; it cannot be paused for block snapshots".into())
            }
            _ => Ok(false),
        },
        Ok(None) => Err("VM is no longer running".into()),
        Err(e) => Err(format!("cannot read VM state: {e}")),
    }
}

/// 디스크들이 블록 스냅샷을 쓰면 그 종류([`block_snapshot_kind`]). 방식(qcow2 내부 스냅샷,
/// 블록 백엔드 종류)이 섞였으면 오류다.
fn block_snapshots(disks: &[(Volume, StoragePool)]) -> Result<Option<&'static str>, String> {
    let mut kinds = disks.iter().map(|(_, p)| block_snapshot_kind(p));
    let first = kinds.next().flatten();
    if kinds.any(|k| k != first) {
        return Err("disks use different snapshot methods".into());
    }
    Ok(first)
}

/// 같은 풀·형식의 디스크끼리 묶는다(처음 나온 순서). 한 묶음은 [`Storage::snapshot_many`] 한
/// 번으로 찍는다.
///
/// [`Storage::snapshot_many`]: opl_driver::Storage::snapshot_many
fn by_pool(disks: &[(Volume, StoragePool)]) -> Vec<Vec<&(Volume, StoragePool)>> {
    let mut groups: Vec<Vec<&(Volume, StoragePool)>> = Vec::new();
    for disk in disks {
        let same = |g: &&mut Vec<&(Volume, StoragePool)>| {
            g[0].1.id == disk.1.id && g[0].0.spec.format == disk.0.spec.format
        };
        match groups.iter_mut().find(same) {
            Some(group) => group.push(disk),
            None => groups.push(vec![disk]),
        }
    }
    groups
}

/// `spec`의 디스크 중 블록 스냅샷을 쓰는 것이 있는지. 없는 볼륨은 넘어간다.
fn has_block_disks(ctx: &Ctx, spec: &VmSpec) -> Result<bool, String> {
    for disk in &spec.disks {
        if let Some((_, pool)) = volume_by_name(ctx, &disk.volume)?
            && uses_block_snapshots(&pool)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 디스크의 태그를 지운다. 실행 중이면 QEMU가 디스크를 쥐고 있으므로 하이퍼바이저로, 아니면
/// `spec`의 디스크마다 스토리지로 지운다. 블록 스냅샷은 QEMU가 쓰지 않으므로 실행 중이어도
/// 스토리지로 지운다. 태그나 볼륨이 없는 디스크는 그냥 넘어가고, 한 디스크가 실패해도 나머지를
/// 지운 뒤 첫 오류를 돌려준다. 다른 노드에 있는 볼륨은 이 노드에서 파일이 보이지 않으므로(없는 것으로 보면
/// 태그가 남는다) 오류다.
async fn remove_tags(
    ctx: &Ctx,
    vm: Uuid,
    running: bool,
    spec: &VmSpec,
    tag: &str,
) -> Result<(), String> {
    if running && !has_block_disks(ctx, spec)? {
        return ctx
            .hv
            .snapshot_delete(vm, tag)
            .await
            .map_err(|e| e.to_string());
    }
    let mut result = Ok(());
    for disk in &spec.disks {
        let Some((volume, pool)) = volume_by_name(ctx, &disk.volume)? else {
            continue;
        };
        if let Err(e) = check_local(ctx, &volume, &pool) {
            if result.is_ok() {
                result = Err(e);
            }
            continue;
        }
        match ctx
            .storage
            .snapshot_delete(&pool, volume.id, volume.spec.format, tag)
            .await
        {
            // 이 노드의 볼륨인데 파일이 없으면 태그도 없다.
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => {
                if result.is_ok() {
                    result = Err(format!("volume {:?}: {e}", volume.name));
                }
            }
        }
    }
    result
}

/// VM의 현재 위치(`status.snapshot`)를 `to(지금 위치)`로 바꾼다. reconciler도 status를 쓰므로
/// revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn set_position(
    ctx: &Ctx,
    vm: Uuid,
    to: impl Fn(Option<Uuid>) -> Option<Uuid>,
) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        let snapshot = to(current.status.snapshot);
        if snapshot == current.status.snapshot {
            return Ok(());
        }
        let status = VmStatus {
            snapshot,
            ..current.status.clone()
        };
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(vm, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `pending` 스냅샷을 채운다. 실행 중이면 하이퍼바이저로(메모리 포함 또는 디스크만), 정지했으면
/// 디스크마다 스토리지로 태그를 만든다(블록 스냅샷은 실행 중이면 VM을 잠시 멈추고 스토리지로).
/// 스토리지로는 같은 풀의 디스크를 한 번에 찍는다(ZFS는 원자적).
/// UEFI 변수를 복사한 뒤 `ready`로 바꾸고 VM의 현재 위치로 삼는다. 실패를 확인했으면 만든 태그를
/// 지운다(리소스는 실행기의 정리가 지운다). 응답을 기다리다 시간이 다 됐으면 작업이 아직 돌 수
/// 있으므로 태그를 두고 실패한다(블록 스냅샷의 태그는 뒤에서 다시 지운다, [`remove_left_tags`]).
pub(super) async fn create(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    let vm = get_vm(ctx, vm_id)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    if snapshot.spec.memory && !running {
        return Err("VM is no longer running; its memory cannot be saved".into());
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    if let Some((volume, _)) = disks
        .iter()
        .find(|(v, p)| !ctx.storage.supports_snapshots(p, v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted",
            volume.name, volume.spec.format
        ));
    }
    let block = block_snapshots(&disks)?;
    if snapshot.spec.memory && disks.is_empty() {
        return Err("VM has no disk to hold the memory state".into());
    }
    if let Some(kind) = block
        && snapshot.spec.memory
    {
        return Err(format!("{kind} disks cannot hold the memory state"));
    }

    // 부모와 VM 스펙은 실행하는 지금의 것이다. 앞에 줄 선 스냅샷 Task가 현재 위치를 바꿨을 수 있다.
    let mut spec = snapshot.spec.clone();
    spec.parent = vm.status.snapshot;
    spec.vm_spec = vm.spec.clone();
    if spec != snapshot.spec {
        ctx.cluster
            .update_spec(id, Some(snapshot.revision), snapshot.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    let tag = id.to_string();
    // 실행 중인 VM의 블록 스냅샷: VM을 멈춰 블록 I/O를 비운 뒤 풀마다 저장소로 찍고, 찍기가
    // 실패해도 반드시 이어서 실행한다(설계 §5). 멈춘 구간은 `PAUSE_LIMIT`을 넘기지 않는다. QEMU가
    // 스스로 멈춘 VM(I/O 오류 등)은 멈추지도 이어서 실행하지도 않는다([`needs_pause`]).
    let block_running = running && block.is_some() && !disks.is_empty();
    let mut paused = None;
    if block_running && needs_pause(ctx, vm_id).await? {
        let mut guard = Paused::new(&ctx.hv, vm_id);
        if let Err(e) = ctx.hv.pause(vm_id).await {
            // 응답만 늦었으면 멈췄을 수 있다.
            if let Err(r) = guard.resume().await {
                tracing::warn!(vm = %vm_id, error = %r, "cannot resume VM after a failed pause");
            }
            return Err(format!("cannot pause VM: {e}"));
        }
        paused = Some(guard);
    }
    let paused_at = Instant::now();
    // 찍기가 끝난 묶음의 디스크. 시간을 넘겨 실패하면 이것들의 스냅샷만 지운다.
    let mut taken = Vec::new();
    let made = if !running || block_running {
        let take = async {
            for group in by_pool(&disks) {
                let (volume, pool) = group[0];
                let ids: Vec<Uuid> = group.iter().map(|(v, _)| v.id).collect();
                ctx.storage
                    .snapshot_many(pool, &ids, volume.spec.format, &tag)
                    .await?;
                taken.extend(group.into_iter().map(|(v, p)| (v, p)));
            }
            Ok(())
        };
        match paused {
            Some(_) => tokio::time::timeout(PAUSE_LIMIT, take)
                .await
                .unwrap_or(Err(DriverError::Timeout("block snapshots of a paused VM"))),
            None => take.await,
        }
    } else if disks.is_empty() {
        // 디스크 없는 VM은 설정만 담는다.
        Ok(())
    } else {
        ctx.hv.snapshot_save(vm_id, &tag, spec.memory).await
    };
    let resumed = match &mut paused {
        Some(guard) => {
            let resumed = guard.resume().await;
            let paused_ms = paused_at.elapsed().as_millis() as u64;
            tracing::info!(vm = %vm_id, snapshot = %id, paused_ms, "VM was paused for block snapshots");
            if let Err(e) = &resumed {
                tracing::error!(vm = %vm_id, error = %e, "cannot resume VM paused for block snapshots");
            }
            resumed
        }
        None => Ok(()),
    };
    let copied = match (&made, ctx.config.state_dir.as_ref()) {
        (Ok(()), Some(dir)) => {
            let vars = uefi_vars_path(dir, vm_id);
            match tokio::fs::try_exists(&vars).await {
                // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM
                Ok(false) => Ok(()),
                Ok(true) => copy_file(&vars, &vars_copy(ctx, vm_id, id).unwrap()).await,
                Err(e) => Err(e),
            }
        }
        _ => Ok(()),
    };
    let failure = match (made, copied) {
        // 블록 스냅샷은 풀마다 따로 만든다. 끝난 묶음의 것은 지우고, 아직 돌 수 있는 묶음의
        // 것만 둔다(그 안에서 어느 디스크가 끝났는지는 모른다).
        (Err(e), _) if e.may_be_in_progress() && block_running => {
            let left = disks
                .iter()
                .filter(|(v, _)| !taken.iter().any(|(t, _)| t.id == v.id))
                .cloned()
                .collect();
            remove_left_tags(ctx, id, left);
            for (volume, pool) in taken {
                if let Err(e) = ctx
                    .storage
                    .snapshot_delete(pool, volume.id, volume.spec.format, &tag)
                    .await
                {
                    tracing::warn!(snapshot = %id, volume = %volume.name, error = %e, "cannot remove block snapshot of failed snapshot");
                }
            }
            let error = format!(
                "{e}; the snapshot may still be written to the disks that were not done and its \
                 tag {tag} is left there"
            );
            return Err(match resumed {
                Err(e) => format!("{error}; resuming the VM failed: {e}"),
                Ok(()) => error,
            });
        }
        (Err(e), _) if e.may_be_in_progress() => {
            if block.is_some() {
                remove_left_tags(ctx, id, disks.clone());
            }
            return Err(format!(
                "{e}; the snapshot may still be written to the disks and its tag {tag} is left there"
            ));
        }
        (Err(e), _) => Some(e.to_string()),
        (Ok(()), Err(e)) => Some(format!("cannot copy UEFI variables: {e}")),
        (Ok(()), Ok(())) => None,
    };
    if let Some(error) = failure {
        // 저장하는 동안 VM이 꺼졌으면(강제 정지로 QMP가 끊김) 꺼진 VM처럼 스토리지로 지운다.
        // QMP만 끊기고 QEMU가 아직 살아 있으면 qemu-img가 이미지 잠금으로 거부해 태그가 남는다
        // (디스크는 손상되지 않고, 아래 로그만 남는다).
        let running = running && !matches!(ctx.hv.inspect(vm_id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
            tracing::warn!(snapshot = %id, error = %e, "cannot remove tags of failed snapshot");
        }
        if let Some(copy) = vars_copy(ctx, vm_id, id) {
            let _ = tokio::fs::remove_file(copy).await;
        }
        return Err(match resumed {
            Err(e) => format!("{error}; resuming the VM failed: {e}"),
            Ok(()) => error,
        });
    }

    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(now_secs()),
        vmstate_bytes: None,
    };
    // 다 만든 스냅샷이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
    loop {
        match ctx.cluster.update_status::<SnapshotSpec>(id, &status).await {
            Ok(_) => break,
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(snapshot = %id, error = %e, "cannot mark snapshot ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    set_position(ctx, vm_id, |_| Some(id)).await?;
    // 스냅샷은 다 만들었으므로 그대로 두고 Task만 실패로 끝낸다. VM은 다시 붙을 때 이어서 실행한다.
    resumed.map_err(|e| format!("snapshot taken; resuming the VM failed: {e}"))
}

/// 예약 스냅샷의 만들기나 예약 정리의 삭제 Task면 그 VM([`recorded_vm`])
pub(super) fn scheduled_vm(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    recorded_vm(&task.spec, |id| get_snapshot(ctx, id).ok().flatten())
}

/// 예약 스냅샷 만들기·삭제의 결과를 VM의 `schedule_error`에 남긴다. 성공(`None`)이면 지운다.
pub(super) async fn record_schedule(ctx: &Ctx, vm: Uuid, error: Option<ScheduleError>) {
    if let Err(e) = set_schedule_error(&ctx.cluster, vm, error).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

/// 실패한 만들기의 `pending` 리소스를 지운다. 이미 `ready`면 그대로 둔다.
pub(super) async fn discard_pending(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    match get_snapshot(ctx, id)? {
        Some(s) if s.status.phase == SnapshotPhase::Pending => delete_resource(ctx, &s).await,
        _ => Ok(()),
    }
}

async fn delete_resource(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    match ctx.cluster.delete::<SnapshotSpec>(snapshot.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이전 opld가 끝내지 못한 `pending` 스냅샷의 태그(있으면)와 리소스를 지운다. 태그를 지우지
/// 못해도 리소스는 지운다(남은 태그는 공간만 쓴다).
pub(super) async fn discard(ctx: &Ctx, snapshot: &Snapshot) -> Result<(), String> {
    let vm = snapshot.spec.vm;
    let running = ctx
        .hv
        .inspect(vm)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
    let tag = snapshot.id.to_string();
    if let Err(e) = remove_tags(ctx, vm, running, &snapshot.spec.vm_spec, &tag).await {
        tracing::warn!(snapshot = %snapshot.id, error = %e, "cannot remove tags of unfinished snapshot");
    }
    if let Some(copy) = vars_copy(ctx, vm, snapshot.id) {
        let _ = tokio::fs::remove_file(copy).await;
    }
    delete_resource(ctx, snapshot).await
}

/// VM을 스냅샷 때로 되돌린다(설계 §5): 강제로 끄고, 스펙을 스냅샷의 것으로(노드는 그대로) 바꾸고,
/// 디스크와 UEFI 변수를 되돌리고, 볼륨의 실제 크기를 다시 읽는다. 현재 위치를 이 스냅샷으로
/// 바꾼 뒤, 메모리 스냅샷이면 그 메모리를 불러오며 시작한다.
pub(super) async fn revert(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    let vm_id = task.spec.target;
    let snapshot = get_snapshot(ctx, id)?.ok_or("snapshot was deleted")?;
    if snapshot.status.phase != SnapshotPhase::Ready {
        return Err("snapshot is not ready".into());
    }
    // 되돌릴 수 없는데 VM만 꺼지지 않도록 끄기 전에 볼륨을 확인한다: 이 노드에서 보이는지, API가
    // 확인한 뒤 다른 VM이 붙이지 않았는지.
    let disks = ready_disks(ctx, &snapshot.spec.vm_spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    for (volume, _) in &disks {
        if let Some(other) = vms
            .iter()
            .find(|vm| vm.id != vm_id && vm.spec.disks.iter().any(|d| d.volume == volume.name))
        {
            return Err(format!(
                "volume {:?} is attached to VM {:?}",
                volume.name, other.name
            ));
        }
    }
    let tag = id.to_string();
    // ZFS 디스크는 가장 최근 스냅샷으로만 되돌린다. API는 VM의 스냅샷 순서만 보므로, 디스크에
    // 더 최근 스냅샷(opLite 밖에서 만든 것 등)이 있으면 끄기 전에 그만둔다.
    for (volume, pool) in &disks {
        if block_snapshot_kind(pool) == Some("zfs") {
            check_latest_tag(ctx, volume, pool, &tag, &snapshot.spec.name).await?;
        }
    }

    let vm = get_vm(ctx, vm_id)?;
    if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
        set_power(ctx, vm_id, PowerState::Stopped).await?;
        ctx.reconciler.force_stop(vm_id);
        wait_vm(ctx, vm_id, stopped).await?;
    }
    write_reverted_spec(ctx, vm_id, &snapshot).await?;

    // 여기서 멈추면 VM은 꺼진 채 남는다. 같은 되돌리기를 다시 하면 처음부터 한다(설계 §6).
    for (volume, pool) in &disks {
        // API는 이 Task가 도는 동안 전원 조작을 막지만, 그 확인과 겹친 요청이 켰을 수 있다.
        // QEMU가 연 디스크를 qemu-img로 바꾸면 안 된다.
        if get_vm(ctx, vm_id)?.spec.power != PowerState::Stopped {
            return Err("VM was started again".into());
        }
        ctx.storage
            .snapshot_revert(pool, volume.id, volume.spec.format, &tag)
            .await
            .map_err(|e| format!("cannot revert volume {:?}: {e}", volume.name))?;
    }
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir)
        && tokio::fs::try_exists(&copy).await.unwrap_or(false)
    {
        copy_file(&copy, &uefi_vars_path(dir, vm_id))
            .await
            .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
    }
    // 스냅샷 뒤에 늘린 크기는 되돌아간다.
    for (volume, pool) in &disks {
        record_size(ctx, volume, pool).await;
    }
    // 디스크가 스냅샷 때로 돌아갔으므로 메모리를 불러오지 못하더라도 현재 위치는 이 스냅샷이다.
    set_position(ctx, vm_id, |_| Some(id)).await?;

    if snapshot.spec.memory {
        // 뒤에 온 전원 요청(정지 등)이 있으면 그것을 따르고 켜지 않는다. 메모리를 불러오며 켜면
        // 그 뒤의 정지를 덮는다.
        if superseded(ctx, task)? {
            return Err("superseded by a later request for this VM".into());
        }
        // power를 쓰기 전에 맡겨야 다음 시작이 받는다. 쓰지 못하면 다른 시작이 쓰지 않게 거둔다.
        ctx.reconciler.restore_on_start(vm_id, tag);
        if let Err(e) = set_power(ctx, vm_id, PowerState::Running).await {
            ctx.reconciler.clear_restore(vm_id);
            return Err(e);
        }
        wait_vm(ctx, vm_id, started).await?;
    }
    Ok(())
}

/// 볼륨의 가장 최근 스냅샷이 `tag`(스냅샷 `name`)인지. 아니면 더 최근 스냅샷의 이름(opLite의
/// 스냅샷이면 그 이름, 아니면 디스크의 태그)과 함께 오류다.
async fn check_latest_tag(
    ctx: &Ctx,
    volume: &Volume,
    pool: &StoragePool,
    tag: &str,
    name: &str,
) -> Result<(), String> {
    let tags = ctx
        .storage
        .snapshot_tags(pool, volume.id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read snapshots of volume {:?}: {e}", volume.name))?;
    if !tags.iter().any(|t| t == tag) {
        return Err(format!("volume {:?} has no snapshot {name:?}", volume.name));
    }
    let Some(last) = tags.last().filter(|t| *t != tag) else {
        return Ok(());
    };
    let newer = match last.parse::<Uuid>() {
        Ok(id) => get_snapshot(ctx, id)?.map_or_else(|| last.clone(), |s| s.spec.name),
        Err(_) => last.clone(),
    };
    Err(format!(
        "volume {:?} has a more recent snapshot {newer:?}; zfs disks can only revert to the \
         latest snapshot",
        volume.name
    ))
}

pub(super) fn is_down(vm: &Vm) -> bool {
    matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드와 예약은 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
/// 되돌리고 시작하므로 배치되지 않은 VM은 이 노드로 정한다. 끈 뒤 누가 다시 켰으면 디스크를
/// 건드리기 전에 그만둔다.
async fn write_reverted_spec(ctx: &Ctx, vm_id: Uuid, snapshot: &Snapshot) -> Result<(), String> {
    loop {
        let vm = get_vm(ctx, vm_id)?;
        if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
            return Err("VM was started again".into());
        }
        let mut spec = snapshot.spec.vm_spec.clone();
        spec.node = vm.spec.node;
        // 예약은 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        if snapshot.spec.memory && spec.node.is_none() {
            // 이 노드의 arch가 머신과 다르면 여기서 시작할 수 없다. 다른 노드로 보내면 메모리 상태를
            // 이어 쓸 수 없으므로 고정하지 않고 Task를 실패로 끝낸다(디스크를 건드리기 전이다).
            if let Some(node) = ctx
                .cluster
                .get::<NodeSpec>(ctx.config.node)
                .map_err(|e| e.to_string())?
            {
                check_node_arch(&node, &spec).map_err(|e| e.message)?;
            }
            spec.node = Some(ctx.config.node);
        }
        spec.power = PowerState::Stopped;
        match ctx
            .cluster
            .update_spec(vm_id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 볼륨의 실제 크기를 읽어 스펙에 반영한다. 실패해도 넘어간다(크기 표시만 틀린다).
async fn record_size(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            return;
        }
    };
    if let Err(e) = set_volume_size(ctx, volume.id, info.virtual_bytes).await {
        tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
    }
}

/// 스냅샷을 지운다: 디스크의 태그(실행 중이면 하이퍼바이저로, 그 뒤 떼어 낸 디스크는 스토리지로),
/// UEFI 변수 사본, 리소스. 자식은 지운 스냅샷의 부모로 옮겨 붙고, 현재 위치였으면 부모로 옮긴다.
/// 태그가 이미 없으면 성공으로 보므로 중간에 멈춰도 다시 하면 된다.
pub(super) async fn delete(ctx: &Ctx, vm_id: Uuid, id: Uuid) -> Result<(), String> {
    let Some(snapshot) = get_snapshot(ctx, id)? else {
        return Ok(());
    };
    let vm = ctx
        .cluster
        .get::<VmSpec>(vm_id)
        .map_err(|e| e.to_string())?;
    if let Some(vm) = &vm {
        check_settled(vm)?;
    }
    let tag = id.to_string();
    let running = vm.as_ref().filter(|vm| vm.status.phase == VmPhase::Running);
    let mut detached = snapshot.spec.vm_spec.clone();
    if let Some(vm) = running {
        remove_tags(ctx, vm_id, true, &vm.spec, &tag).await?;
        // 실행 중인 VM이 쓰는 디스크(실행 중에는 바뀌지 않는다)는 위에서 지웠다.
        detached
            .disks
            .retain(|d| !vm.spec.disks.iter().any(|a| a.volume == d.volume));
    }
    remove_tags(ctx, vm_id, false, &detached, &tag).await?;
    if let Some(copy) = vars_copy(ctx, vm_id, id) {
        match tokio::fs::remove_file(copy).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove UEFI variables copy: {e}")),
        }
    }

    let (_, all) = ctx
        .cluster
        .list::<SnapshotSpec>()
        .map_err(|e| e.to_string())?;
    for child in all.iter().filter(|s| s.spec.parent == Some(id)) {
        let mut spec = child.spec.clone();
        spec.parent = snapshot.spec.parent;
        match ctx
            .cluster
            .update_spec(child.id, None, child.labels.clone(), &spec)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if vm.is_some() {
        let parent = snapshot.spec.parent;
        set_position(ctx, vm_id, |at| if at == Some(id) { parent } else { at }).await?;
    }
    delete_resource(ctx, &snapshot).await
}

#[cfg(test)]
mod tests {
    use opl_driver::mock::MockHypervisor;
    use opl_types::runtime::VmRuntimeSpec;
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    /// 멈춘 VM을 Mock 하이퍼바이저에 띄운다.
    async fn paused_vm(hv: &MockHypervisor) -> Uuid {
        let id = Uuid::now_v7();
        let vm = VmRuntimeSpec {
            id,
            name: "web".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Bios,
            machine: MachineType::Q35,
            disks: vec![],
            nics: vec![],
        };
        hv.start(&vm, None).await.unwrap();
        hv.pause(id).await.unwrap();
        id
    }

    /// 이어서 실행하지 못하면 1초 간격으로 세 번까지 해 본다.
    #[tokio::test(start_paused = true)]
    async fn resume_is_retried() {
        let hv = Arc::new(MockHypervisor::default());
        let id = paused_vm(&hv).await;
        // QMP로 잠시 다룰 수 없다.
        hv.set_unresponsive(id, true);
        let later = hv.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            later.set_unresponsive(id, false);
        });
        resume_vm(&*hv, id).await.unwrap();
        assert!(!hv.is_paused(id));

        // 끝내 안 되면 세 번째 오류를 돌려준다.
        hv.pause(id).await.unwrap();
        hv.set_unresponsive(id, true);
        let started = tokio::time::Instant::now();
        let err = resume_vm(&*hv, id).await.unwrap_err();
        assert!(matches!(err, DriverError::NotAttached(_)), "{err:?}");
        assert_eq!(started.elapsed(), RESUME_INTERVAL * (RESUME_TRIES - 1));
    }

    /// 꺼진 VM은 이어서 실행할 것이 없으므로 성공이다(다시 해 보지 않는다).
    #[tokio::test(start_paused = true)]
    async fn resume_of_a_vm_that_is_gone_succeeds() {
        let hv = Arc::new(MockHypervisor::default());
        let id = paused_vm(&hv).await;
        hv.crash(id);
        let started = tokio::time::Instant::now();
        resume_vm(&*hv, id).await.unwrap();
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(hv.pause_calls(id), ["pause", "resume"]);
    }

    /// 가드는 이어서 실행하기를 한 번(재시도 포함) 해 보면 실패해도 꺼진다. 그 뒤는 표시 파일과
    /// 다시 붙기가 맡는다.
    #[tokio::test(start_paused = true)]
    async fn guard_is_off_after_one_round_of_resume_tries() {
        let hv = Arc::new(MockHypervisor::default());
        let id = paused_vm(&hv).await;
        hv.set_unresponsive(id, true);
        let shared: Arc<dyn Hypervisor> = hv.clone();
        let mut guard = Paused::new(&shared, id);
        guard.resume().await.unwrap_err();
        drop(guard);
        hv.set_unresponsive(id, false);
        tokio::time::sleep(RESUME_INTERVAL * 10).await;
        let resumes = hv
            .pause_calls(id)
            .iter()
            .filter(|c| **c == "resume")
            .count();
        assert_eq!(resumes, RESUME_TRIES as usize);
        assert!(hv.is_paused(id));
    }
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    resize_unit: std::sync::Mutex<u64>,
}
```

을 다음으로 바꾼다:

```rust
    resize_unit: std::sync::Mutex<u64>,
    /// `snapshot_many`로 한 번에 찍은 볼륨들
    snapshot_groups: std::sync::Mutex<Vec<Vec<Uuid>>>,
    /// 볼륨마다 스냅샷 태그(만든 순서). 만들기·지우기가 바꾼다.
    tags: std::sync::Mutex<std::collections::HashMap<Uuid, Vec<String>>>,
    /// 볼륨마다 남은 횟수만큼 `snapshot_delete`가 일시 오류다(앞 명령이 아직 돈다).
    busy_deletes: std::sync::Mutex<std::collections::HashMap<Uuid, usize>>,
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        *self.resize_unit.lock().unwrap() = unit;
    }
```

을 다음으로 바꾼다:

```rust
        *self.resize_unit.lock().unwrap() = unit;
    }

    /// `snapshot_many`로 한 번에 찍은 볼륨들(부른 순서)
    pub fn snapshot_groups(&self) -> Vec<Vec<Uuid>> {
        self.snapshot_groups.lock().unwrap().clone()
    }

    /// 볼륨 `id`의 스냅샷 태그(만든 순서)
    pub fn tags(&self, id: Uuid) -> Vec<String> {
        self.tags
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// 볼륨 `id`의 스냅샷 태그를 바꾼다(opLite 밖에서 만든 스냅샷 등).
    pub fn set_tags(&self, id: Uuid, tags: &[&str]) {
        let tags = tags.iter().map(|t| t.to_string()).collect();
        self.tags.lock().unwrap().insert(id, tags);
    }

    /// 볼륨 `id`의 다음 `n`번의 `snapshot_delete`가 (호출을 기록한 뒤) 일시 오류다.
    pub fn busy_snapshot_deletes(&self, id: Uuid, n: usize) {
        self.busy_deletes.lock().unwrap().insert(id, n);
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("create", id).await;
```

을 다음으로 바꾼다:

```rust
        id: Uuid,
        _: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.record("create", id).await;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        }
        if self.timing_out_creates.lock().unwrap().contains(&id) {
            return Err(DriverError::Timeout("lvm command"));
        }
```

을 다음으로 바꾼다:

```rust
        }
        let mut tags = self.tags.lock().unwrap();
        tags.entry(id).or_default().push(tag.into());
        drop(tags);
        if self.timing_out_creates.lock().unwrap().contains(&id) {
            return Err(DriverError::Timeout("lvm command"));
        }
        Ok(())
    }
    /// 묶음을 기록하고 기본처럼 하나씩 찍는다(호출은 `create`로 기록된다).
    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.snapshot_groups.lock().unwrap().push(ids.to_vec());
        for &id in ids {
            self.snapshot_create(pool, id, format, tag).await?;
        }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("delete", id).await;
        Ok(())
```

을 다음으로 바꾼다:

```rust
        _: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.record("delete", id).await;
        if let Some(left) = self.busy_deletes.lock().unwrap().get_mut(&id)
            && *left > 0
        {
            *left -= 1;
            return Err(DriverError::Timeout("an earlier command still running"));
        }
        if let Some(tags) = self.tags.lock().unwrap().get_mut(&id) {
            tags.retain(|t| t != tag);
        }
        Ok(())
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(vec![])
    }
```

을 다음으로 바꾼다:

```rust
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(self.tags(id))
    }
```

`crates/opl-api/tests/snapshot_tasks.rs` 전체:

```rust
//! 스냅샷 Task 실행: 실행 중인 VM(Mock 하이퍼바이저)의 만들기·삭제, 부모와 현재 위치, 실패 정리,
//! 디스크 없는 VM 되돌리기, 시작할 때의 고아 정리, 실행 중인 VM의 블록(LVM, ZFS) 스냅샷과 크기
//! 늘리기. 디스크 파일을 다루는 경우는 `snapshot_disks`.

mod common;

use std::sync::{Arc, Mutex};

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::{DriverError, Hypervisor, RuntimeStatus, uefi_vars_path};
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{Arch, PowerState, VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 풀 `local`과 바로 Ready인 qcow2 볼륨 `root`를 저장소에 직접 만든다(파일은 없다. Mock
/// 하이퍼바이저는 파일을 열지 않는다).
async fn pool_and_root(env: &Env) {
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let mut volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// 디스크 `disks`를 붙인 VM을 만들고 켠다.
async fn running_vm(admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// 스냅샷을 요청하고 Task가 끝날 때까지 기다린다.
async fn snapshot(admin: &Client, vm: &str, body: Value) -> Value {
    let (status, task) = admin.post(&format!("/vms/{vm}/snapshots"), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn find(env: &Env, vm: Uuid, name: &str) -> Option<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
}

fn get(env: &Env, vm: Uuid, name: &str) -> Snapshot {
    find(env, vm, name).unwrap_or_else(|| panic!("snapshot {name} not found"))
}

fn position(env: &Env, vm: Uuid) -> Option<Uuid> {
    env.cluster
        .get::<VmSpec>(vm)
        .unwrap()
        .unwrap()
        .status
        .snapshot
}

fn vars_copy(env: &Env, vm: Uuid, snapshot: Uuid) -> std::path::PathBuf {
    env.dir
        .path()
        .join(format!("state/vm/{vm}/snapshots/{snapshot}.vars"))
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn running_vm_snapshots_chain_and_move_the_position() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    // QEMU 드라이버가 처음 시작할 때 만드는 UEFI 변수 파일
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-a").unwrap();

    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert!(a.status.created_at.is_some());
    assert!(a.spec.memory);
    assert_eq!(a.spec.parent, None);
    assert_eq!(env.hv.snapshots(vm), vec![(a.id.to_string(), true)]);
    assert_eq!(position(&env, vm), Some(a.id));
    assert_eq!(std::fs::read(vars_copy(&env, vm, a.id)).unwrap(), b"vars-a");

    // 줄 선 두 만들기: API는 둘 다 a를 부모로 기록하지만, 실행할 때 앞의 것을 부모로 삼는다.
    let (_, tb) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "b", "memory": false }),
        )
        .await;
    let (_, tc) = admin
        .post("/vms/web/snapshots", json!({ "name": "c" }))
        .await;
    succeeded(&admin.wait_task(&tb).await);
    succeeded(&admin.wait_task(&tc).await);
    let (b, c) = (get(&env, vm, "b"), get(&env, vm, "c"));
    assert_eq!(b.spec.parent, Some(a.id));
    assert_eq!(c.spec.parent, Some(b.id));
    assert_eq!(
        env.hv.snapshots(vm),
        vec![
            (a.id.to_string(), true),
            (b.id.to_string(), false),
            (c.id.to_string(), true)
        ]
    );
    assert_eq!(position(&env, vm), Some(c.id));
}

#[tokio::test]
async fn failed_snapshot_is_removed_but_a_timed_out_one_keeps_its_tag() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;

    // 작업이 오류로 끝났다: 만든 태그와 리소스를 지운다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobFailed {
            id: "j".into(),
            error: "no space".into(),
        }));
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "a").is_none());
    assert!(env.hv.snapshots(vm).is_empty());
    assert_eq!(position(&env, vm), None);

    // 시간 초과: 작업이 아직 돌 수 있으므로 태그는 두고 리소스만 지운다. 이름은 다시 쓸 수 있다.
    env.hv
        .fail_next_snapshot(DriverError::Qmp(QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(600),
            stage: "x".into(),
        }));
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    let id = task["spec"]["op"]["snapshot"].as_str().unwrap();
    assert!(error.contains(id), "{error}");
    assert!(find(&env, vm, "a").is_none());
    assert_eq!(env.hv.snapshots(vm), vec![(id.to_string(), true)]);
    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
}

#[tokio::test]
async fn delete_reparents_children_and_moves_the_position() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars").unwrap();
    for name in ["a", "b", "c"] {
        succeeded(&snapshot(&admin, "web", json!({ "name": name })).await);
    }
    let (a, b, c) = (get(&env, vm, "a"), get(&env, vm, "b"), get(&env, vm, "c"));

    let (status, task) = admin.delete("/vms/web/snapshots/b").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert!(find(&env, vm, "b").is_none());
    assert!(!vars_copy(&env, vm, b.id).exists());
    assert_eq!(get(&env, vm, "c").spec.parent, Some(a.id));
    assert_eq!(position(&env, vm), Some(c.id));

    // 현재 위치를 지우면 부모로 옮긴다.
    let (_, task) = admin.delete("/vms/web/snapshots/c").await;
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(position(&env, vm), Some(a.id));
    assert_eq!(env.hv.snapshots(vm), vec![(a.id.to_string(), true)]);
    assert!(vars_copy(&env, vm, a.id).exists());

    // 디스크에서 지우지 못하면 리소스와 관계는 그대로다. 다시 하면 된다.
    env.hv.fail_next_snapshot(DriverError::Timeout("qmp"));
    let (_, task) = admin.delete("/vms/web/snapshots/a").await;
    assert_eq!(admin.wait_task(&task).await["status"]["state"], "failed");
    assert_eq!(position(&env, vm), Some(a.id));
    assert!(find(&env, vm, "a").is_some());
    let (_, task) = admin.delete("/vms/web/snapshots/a").await;
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(position(&env, vm), None);
    assert!(env.hv.snapshots(vm).is_empty());
}

#[tokio::test]
async fn revert_restores_the_spec_of_a_diskless_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "bare", &[]).await;
    // 디스크가 없으면 설정만 담는다.
    succeeded(&snapshot(&admin, "bare", json!({ "name": "cfg" })).await);
    let cfg = get(&env, vm, "cfg");
    assert!(!cfg.spec.memory);
    assert!(env.hv.snapshots(vm).is_empty());

    let node = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().spec.node;

    let (_, current) = admin.get("/vms/bare").await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(1024);
    admin.put("/vms/bare", json!({ "spec": spec }), None).await;
    let (status, task) = admin
        .post("/vms/bare/snapshots/cfg/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);

    // 메모리가 없는 스냅샷이므로 꺼진 채 남는다. 노드는 그대로다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.memory_mib, 256);
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.spec.node, node);
    assert_eq!(got.status.phase, VmPhase::Stopped);
    assert_eq!(got.status.snapshot, Some(cfg.id));
    assert_eq!(env.pid(vm).await, None);
}

#[tokio::test]
async fn memory_revert_does_not_place_the_vm_on_a_node_of_another_architecture() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "mem" })).await);
    assert!(get(&env, vm, "mem").spec.memory);
    // 끄고 배치를 푼다. 그 뒤 이 노드가 다른 arch로 기록됐다.
    succeeded(&admin.act("web", "stop").await);
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["node"] = Value::Null;
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.arch = Some(Arch::X86_64);
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();

    let (status, task) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("machine type"),
        "{task}"
    );
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.node, None);
    assert_eq!(got.spec.power, PowerState::Stopped);
}

#[tokio::test]
async fn snapshot_tasks_do_not_supersede_a_reboot() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let vm = running_vm(&admin, "web", &[]).await;
    let (_, reboot) = admin.post("/vms/web/actions/reboot", Value::Null).await;
    // 재부팅이 끄는 동안(게스트는 ACPI를 무시하고, acpi_timeout 뒤 강제로 꺼진다) 같은 VM에
    // 스냅샷 Task가 만들어진다. 다른 노드에 배정해 여기서는 실행하지 않는다.
    let task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::SnapshotCreate {
                snapshot: Uuid::now_v7(),
            },
            target: vm,
            node: Uuid::now_v7(),
        },
    );
    env.cluster.create(&task).await.unwrap();
    succeeded(&admin.wait_task(&reboot).await);
}

#[tokio::test]
async fn unfinished_snapshots_of_this_node_are_removed_at_startup() {
    let ids = Arc::new(Mutex::new(Vec::new()));
    let slot = ids.clone();
    let other = Uuid::now_v7();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            let mut ids = Vec::new();
            for (name, node) in [("mine", me), ("theirs", other)] {
                let mut vm: opl_types::vm::Vm = Resource::new(name, vm_spec(node));
                vm.spec.power = PowerState::Stopped;
                cluster.create(&vm).await.unwrap();
                // Task를 만들기 전에 API가 있던 opld가 죽었다.
                let s = pending(vm.id, &vm.spec, "orphan");
                cluster.create(&s).await.unwrap();
                ids.push(s.id);
            }
            // 이 노드가 만들다 opld가 죽었다. Task는 실패로 끝나고 리소스를 지운다.
            let vm = cluster.get_by_name::<VmSpec>("mine").unwrap().unwrap();
            let s = pending(vm.id, &vm.spec, "interrupted");
            cluster.create(&s).await.unwrap();
            ids.push(s.id);
            let mut task: Task = Resource::new(
                "",
                TaskSpec {
                    op: TaskOp::SnapshotCreate { snapshot: s.id },
                    target: vm.id,
                    node: me,
                },
            );
            task.name = task.id.to_string();
            task.status = TaskStatus {
                state: TaskState::Running,
                error: None,
            };
            cluster.create(&task).await.unwrap();
            // 이 노드에서 돌던 꺼진 VM을 다른 노드로 옮겼다. 디스크는 그 노드에 있다.
            let mut vm: opl_types::vm::Vm = Resource::new("moved", vm_spec(other));
            vm.spec.power = PowerState::Stopped;
            vm.status.node = Some(me);
            cluster.create(&vm).await.unwrap();
            let s = pending(vm.id, &vm.spec, "orphan");
            cluster.create(&s).await.unwrap();
            ids.push(s.id);
            *slot.lock().unwrap() = ids;
        },
    )
    .await;
    let ids = ids.lock().unwrap().clone();
    // 실행기는 고아를 지운 뒤에 뜬다.
    assert!(env.cluster.get::<SnapshotSpec>(ids[0]).unwrap().is_none());
    assert!(env.cluster.get::<SnapshotSpec>(ids[1]).unwrap().is_some());
    assert!(env.cluster.get::<SnapshotSpec>(ids[3]).unwrap().is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<SnapshotSpec>(ids[2]).unwrap().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "interrupted snapshot was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 파일 없는 스토리지로 실행기를 다시 띄운다. 볼륨 `root`·`data`는 1 MiB로 알린다.
async fn with_fake_storage(env: &mut Env) -> Arc<FakeStorage> {
    let storage = Arc::new(FakeStorage::default());
    for name in ["root", "data"] {
        if let Some(v) = env.cluster.get_by_name::<VolumeSpec>(name).unwrap() {
            storage.set_size(v.id, v.spec.size_bytes);
        }
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    storage
}

/// 바로 Ready인 qcow2 볼륨을 저장소에 직접 만든다.
async fn ready_volume(env: &Env, name: &str) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    volume.id
}

/// 다른 노드에 배정한 Task를 저장소에 직접 만든다. 여기서는 실행하지 않는다.
async fn foreign_task(env: &Env, op: TaskOp, target: Uuid) {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op,
            target,
            node: Uuid::now_v7(),
        },
    );
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
}

#[tokio::test]
async fn revert_blocks_power_changes_and_stops_if_started_meanwhile() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let data = ready_volume(&env, "data").await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root", "data"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);

    storage.pause();
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    storage.reached().await;

    // 되돌리는 동안에는 전원도 스펙도 바꿀 수 없다(강제 정지 포함).
    for path in ["/vms/web/actions/start", "/vms/web/actions/stop?mode=force"] {
        let (status, body) = admin.post(path, Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_busy", "{body}");
    }
    let (_, current) = admin.get("/vms/web").await;
    let (status, body) = admin
        .put("/vms/web", json!({ "spec": current["spec"] }), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");

    // 그 확인을 지나친 요청이 켰다. 남은 디스크는 되돌리지 않고 실패한다.
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let spec = VmSpec {
        power: PowerState::Running,
        ..current.spec.clone()
    };
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    storage.resume();
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let reverted: Vec<Uuid> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "revert")
        .map(|(_, id)| id)
        .collect();
    assert_eq!(reverted.len(), 1);
    assert!(!reverted.contains(&data));
}

#[tokio::test]
async fn memory_revert_yields_to_a_later_stop_and_keeps_volume_labels() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "mem" })).await);
    let launches = env.hv.launches().len();

    storage.pause();
    let (_, revert) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    storage.reached().await;
    // 되돌리는 사이 정지 요청이 들어왔고(다른 노드의 API), 볼륨 라벨이 바뀌었고, 파일 크기는
    // 스냅샷 때의 것이다.
    foreign_task(&env, TaskOp::VmStop, vm).await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let labels = std::collections::BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    env.cluster
        .update_spec(root.id, Some(root.revision), labels.clone(), &root.spec)
        .await
        .unwrap();
    storage.set_size(root.id, 2 << 20);
    storage.resume();

    // 메모리를 불러오며 켜지 않는다. 정지 요청을 덮으면 안 된다.
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("superseded"),
        "{task}"
    );
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.phase, VmPhase::Stopped);
    assert_eq!(env.hv.launches().len(), launches);
    // 디스크는 되돌렸으므로 현재 위치와 크기는 스냅샷 때의 것이고, 라벨은 그대로다.
    assert_eq!(position(&env, vm), Some(get(&env, vm, "mem").id));
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

#[tokio::test]
async fn revert_checks_volumes_attached_meanwhile_before_stopping() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);
    succeeded(&admin.act("web", "stop").await);
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    succeeded(&admin.act("web", "start").await);
    let vm = env
        .cluster
        .get_by_name::<VmSpec>("web")
        .unwrap()
        .unwrap()
        .id;
    let pid = env.pid(vm).await;

    // API가 확인한 뒤 Task가 돌기 전에 다른 VM이 그 볼륨을 붙였다.
    env.stop_runner().await;
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let mut req = vm_request("other");
    req["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.start_runner_with(storage).await;

    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    // 끄기 전에 그만뒀다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(env.pid(vm).await, pid);
}

#[tokio::test]
async fn delete_does_not_treat_a_volume_of_another_node_as_gone() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    // 이 노드에 배치된 꺼진 VM
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "stop").await);
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);

    // 볼륨 파일이 다른 노드에 있다. 여기서 없는 것으로 보고 지우면 그 노드에 태그가 남는다.
    let mut root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    root.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(root.id, &root.status)
        .await
        .unwrap();
    let (status, task) = admin.delete("/vms/web/snapshots/s").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "s").is_some());
    assert!(!storage.calls().iter().any(|(call, _)| *call == "delete"));
}

#[tokio::test]
async fn revert_checks_volumes_of_another_node_before_stopping() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "s" })).await);
    let pid = env.pid(vm).await;
    let before = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();

    // 볼륨 파일이 다른 노드에 있다. 이 노드에서는 되돌릴 수 없다.
    let mut root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    root.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(root.id, &root.status)
        .await
        .unwrap();
    let (status, revert) = admin.post("/vms/web/snapshots/s/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let task = admin.wait_task(&revert).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("is on node"),
        "{task}"
    );
    // 끄지도, 스펙을 바꾸지도 않았다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec, before.spec);
    assert_eq!(env.pid(vm).await, pid);
    assert!(!storage.calls().iter().any(|(call, _)| *call == "revert"));
}

#[tokio::test]
async fn resize_keeps_volume_labels_written_meanwhile() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;

    storage.pause();
    let (status, task) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let labels = std::collections::BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    env.cluster
        .update_spec(root.id, Some(root.revision), labels.clone(), &root.spec)
        .await
        .unwrap();
    storage.resume();

    succeeded(&admin.wait_task(&task).await);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!((root.spec.size_bytes, root.labels), (2 << 20, labels));
}

#[tokio::test]
async fn task_failed_by_someone_else_before_it_starts_is_not_run() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;

    storage.pause();
    let (status, first) = admin
        .post("/volumes/root/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    storage.reached().await;
    // 같은 줄에서 앞의 Task를 기다리는 Task를, 시작하기 전에 다른 쪽(리더의 정리 작업)이 실패로 끝낸다.
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut second: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::VolumeResize {
                size_bytes: 3 << 20,
            },
            target: root.id,
            node: env.node,
        },
    );
    second.name = second.id.to_string();
    env.cluster.create(&second).await.unwrap();
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some(format!("node {} is offline", env.node)),
    };
    env.cluster
        .update_status::<TaskSpec>(second.id, &failed)
        .await
        .unwrap();
    storage.resume();

    succeeded(&admin.wait_task(&first).await);
    // 뒤의 Task가 돌았다면 이미 크기를 바꾸고 끝났을 시간을 기다린다.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let task = env.cluster.get::<TaskSpec>(second.id).unwrap().unwrap();
    assert_eq!(task.status, failed);
    let resized = storage
        .calls()
        .iter()
        .filter(|(c, _)| *c == "resize")
        .count();
    assert_eq!(resized, 1);
    let root = env.cluster.get::<VolumeSpec>(root.id).unwrap().unwrap();
    assert_eq!(root.spec.size_bytes, 2 << 20);
}

#[tokio::test]
async fn force_stop_during_a_memory_snapshot_fails_it_and_removes_the_tag() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool_and_root(&env).await;
    let storage = with_fake_storage(&mut env).await;
    let vm = running_vm(&admin, "web", &["root"]).await;
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();

    env.hv.hang_next_snapshot_save();
    let (status, create) = admin
        .post("/vms/web/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{create}");
    tokio::time::timeout(common::WAIT, env.hv.snapshot_save_reached())
        .await
        .expect("snapshot save");

    // 만들기 중에도 강제 정지는 받는다(일반 정지는 그대로 막는다). 강제 정지 Task는 같은 VM의
    // 만들기 Task를 기다리지 않는다. 기다렸다면 VM이 꺼지지 않아 저장도 끝나지 않는다.
    let (status, body) = admin.post("/vms/web/actions/stop", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy", "{body}");
    succeeded(&admin.act("web", "stop?mode=force").await);

    // QMP가 끊겨 만들기가 실패한다. 꺼진 VM이므로 디스크의 태그는 스토리지로 지우고 리소스도 지운다.
    let task = admin.wait_task(&create).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "s").is_none());
    assert!(storage.calls().contains(&("delete", root.id)));
    assert!(env.hv.snapshots(vm).is_empty());
    assert_eq!(position(&env, vm), None);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(current.status.phase, VmPhase::Stopped);
}

fn vm_spec(node: Uuid) -> VmSpec {
    let mut spec: VmSpec = serde_json::from_value(vm_request("x")["spec"].clone()).unwrap();
    spec.node = Some(node);
    spec
}

fn pending(vm: Uuid, spec: &VmSpec, name: &str) -> Snapshot {
    let mut s: Snapshot = Resource::new(
        "",
        SnapshotSpec {
            vm,
            name: name.into(),
            parent: None,
            memory: false,
            description: String::new(),
            scheduled: false,
            vm_spec: spec.clone(),
        },
    );
    s.name = s.id.to_string();
    s
}

fn lvm(vg: &str) -> PoolBackend {
    PoolBackend::LvmThin {
        vg: vg.into(),
        thinpool: "tp".into(),
    }
}

fn zfs(dataset: &str) -> PoolBackend {
    PoolBackend::Zfs {
        dataset: dataset.into(),
    }
}

/// 블록 풀들(이름, 백엔드, 볼륨 이름)과 각 풀의 바로 Ready인 1 MiB raw 볼륨을 저장소에 직접 만들고,
/// 파일 없는 스토리지로 실행기를 다시 띄운다(볼륨은 1 MiB로 알린다). id는 볼륨을 적은 순서다.
async fn block_volumes(
    env: &mut Env,
    pools: &[(&str, PoolBackend, &[&str])],
) -> (Vec<Uuid>, Arc<FakeStorage>) {
    let storage = Arc::new(FakeStorage::default());
    let mut ids = Vec::new();
    for (pool, backend, names) in pools {
        let spec = StoragePoolSpec {
            backend: backend.clone(),
            nodes: vec![],
            shared: false,
        };
        env.cluster
            .create(&Resource::new(*pool, spec))
            .await
            .unwrap();
        env.pool_ready(pool).await;
        for name in names.iter() {
            let mut volume = Resource::new(
                *name,
                VolumeSpec {
                    pool: (*pool).into(),
                    size_bytes: 1 << 20,
                    format: ImageFormat::Raw,
                },
            );
            env.cluster.create(&volume).await.unwrap();
            volume.status.phase = VolumePhase::Ready;
            env.cluster
                .update_status::<VolumeSpec>(volume.id, &volume.status)
                .await
                .unwrap();
            storage.set_size(volume.id, 1 << 20);
            ids.push(volume.id);
        }
    }
    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    (ids, storage)
}

/// LVM-thin 풀 `thin`과 그 풀의 볼륨 `names`([`block_volumes`])
async fn lvm_volumes(env: &mut Env, names: &[&str]) -> (Vec<Uuid>, Arc<FakeStorage>) {
    block_volumes(env, &[("thin", lvm("vg0"), names)]).await
}

/// 디스크 `disks`를 붙인 VM을 이 노드에서 실행 중으로 만든다. LVM-thin 풀은 macOS에서 쓸 수 없어
/// reconciler가 띄우지 못하므로 Mock 하이퍼바이저에 직접 띄운 뒤 이 노드에 켜진 것으로 배치한다
/// (reconciler는 이미 돌고 있는 VM을 그대로 둔다).
async fn running_block_vm(env: &Env, admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let id: Uuid = vm["id"].as_str().unwrap().parse().unwrap();
    let current = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let runtime = VmRuntimeSpec {
        id,
        name: name.into(),
        cpu: current.spec.cpu.clone(),
        memory_mib: current.spec.memory_mib,
        firmware: current.spec.firmware,
        machine: current.spec.machine,
        disks: vec![],
        nics: vec![],
    };
    env.hv.start(&runtime, None).await.unwrap();
    let mut spec = current.spec.clone();
    spec.node = Some(env.node);
    spec.power = PowerState::Running;
    env.cluster
        .update_spec(id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<VmSpec>(id).unwrap().unwrap().status.phase != VmPhase::Running {
        assert!(std::time::Instant::now() < deadline, "VM did not run");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    id
}

#[tokio::test]
async fn running_vm_lvm_snapshot_pauses_it_while_the_disks_are_snapshotted() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0", "lv1"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0", "lv1"]).await;

    // 첫 디스크를 찍는 동안 VM은 멈춰 있다.
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
    assert_eq!(storage.calls(), [("create", lvs[0]), ("create", lvs[1])]);
    // QEMU의 내부 스냅샷은 쓰지 않는다.
    assert!(env.hv.snapshots(vm).is_empty());
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert!(!a.spec.memory);
    assert_eq!(position(&env, vm), Some(a.id));

    // 실행 중이어도 스냅샷 LV는 스토리지로 지운다.
    let (status, task) = admin.delete("/vms/web/snapshots/a").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(
        storage.calls()[2..],
        [("delete", lvs[0]), ("delete", lvs[1])]
    );
    assert!(find(&env, vm, "a").is_none());
    assert!(env.pid(vm).await.is_some());
}

#[tokio::test]
async fn failed_lvm_snapshot_resumes_the_vm_and_removes_the_lvs_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0", "lv1"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0", "lv1"]).await;

    storage.fail_snapshot_create(lvs[1]);
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(!env.hv.is_paused(vm));
    assert_eq!(
        storage.calls(),
        [
            ("create", lvs[0]),
            ("create", lvs[1]),
            ("delete", lvs[0]),
            ("delete", lvs[1]),
        ]
    );
    assert!(find(&env, vm, "a").is_none());
    assert_eq!(position(&env, vm), None);
}

/// 한 풀의 찍기가 시간을 넘기면 그 풀의 스냅샷 LV는 아직 만들어지고 있을 수 있으므로 두고, 찍기가
/// 끝난 풀의 것만 지운다. 다시 돌리지 못했으면 그것도 알린다.
#[tokio::test]
async fn timed_out_lvm_snapshot_removes_only_the_lvs_made() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = block_volumes(
        &mut env,
        &[
            ("thin", lvm("vg0"), &["lv0"]),
            ("thin2", lvm("vg1"), &["lv1"]),
        ],
    )
    .await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0", "lv1"]).await;

    storage.time_out_snapshot_create(lvs[1]);
    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 멈춘 동안 QMP로 다룰 수 없게 되어 다시 돌리지 못한다.
    env.hv.set_unresponsive(vm, true);
    storage.resume();
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("may still"), "{error}");
    assert!(error.contains("resuming the VM failed"), "{error}");
    assert_eq!(
        storage.calls(),
        [("create", lvs[0]), ("create", lvs[1]), ("delete", lvs[0])]
    );
    env.hv.set_unresponsive(vm, false);
}

/// 같은 풀의 디스크는 한 번에 찍으므로(ZFS는 원자적) 그 묶음이 시간을 넘기면 어느 디스크가
/// 끝났는지 모른다. 그 풀의 스냅샷은 모두 둔다.
#[tokio::test]
async fn timed_out_snapshot_of_one_pool_leaves_all_its_disks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0", "z1"])]).await;
    running_block_vm(&env, &admin, "web", &["z0", "z1"]).await;

    storage.time_out_snapshot_create(zvols[1]);
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(storage.snapshot_groups(), [vec![zvols[0], zvols[1]]]);
    assert_eq!(
        storage.calls(),
        [("create", zvols[0]), ("create", zvols[1])]
    );
}

/// 시간을 넘겨 남긴 태그는 그 스냅샷 리소스가 없으면 뒤에서 `orphan_retry` 간격으로 다시 지운다.
/// 시간을 넘긴 명령이 아직 돌아 지우기가 일시 오류면 다음 차례에 다시 지운다. 지웠으면 더 하지
/// 않는다.
#[tokio::test]
async fn timed_out_block_snapshot_tags_are_removed_later() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(
        &mut env,
        &[
            ("y", zfs("tank/other"), &["y0"]),
            ("z", zfs("tank/opl"), &["z0", "z1"]),
        ],
    )
    .await;
    let retry = std::time::Duration::from_millis(100);
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |config| config.orphan_retry = retry)
        .await;
    running_block_vm(&env, &admin, "web", &["y0", "z0", "z1"]).await;

    storage.time_out_snapshot_create(zvols[2]);
    storage.busy_snapshot_deletes(zvols[2], 1);
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    // 끝난 풀의 것은 바로 지우고, 시간을 넘긴 풀의 것은 뒤에서 지운다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while !(storage.tags(zvols[1]).is_empty() && storage.tags(zvols[2]).is_empty()) {
        assert!(std::time::Instant::now() < deadline, "tags were kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    tokio::time::sleep(retry * 4).await;
    let deletes = |id: Uuid| {
        storage
            .calls()
            .iter()
            .filter(|c| **c == ("delete", id))
            .count()
    };
    assert_eq!(
        (deletes(zvols[0]), deletes(zvols[1]), deletes(zvols[2])),
        (1, 1, 2)
    );
}

/// 남은 태그가 지워질 때까지 기다린다.
async fn wait_untagged(storage: &FakeStorage, id: Uuid) {
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage.tags(id).is_empty() {
        assert!(std::time::Instant::now() < deadline, "tags were kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// 블록 스냅샷 Task가 Task 제한 시간에 걸려 버려지면 명령이 아직 돌다 태그를 남길 수 있다. 리소스를
/// 지운 뒤 뒤에서 `orphan_retry` 간격으로 그 태그를 지운다.
#[tokio::test]
async fn block_snapshot_abandoned_at_the_task_limit_has_its_tags_removed_later() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0"])]).await;
    let vm = running_block_vm(&env, &admin, "web", &["z0"]).await;
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.timeout = std::time::Duration::from_millis(500);
        c.orphan_retry = std::time::Duration::from_millis(100);
    })
    .await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 버려진 명령이 끝나 태그를 남긴다.
    let tag = get(&env, vm, "a").id.to_string();
    storage.set_tags(zvols[0], &[&tag]);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "a").is_none());
    wait_untagged(&storage, zvols[0]).await;
}

/// 블록 스냅샷 Task가 끝나지 않은 채 실행기가 멈췄다 다시 시작하면 Task를 실패로 끝내고 리소스를
/// 지운 뒤, 남았을 수 있는 태그를 뒤에서 지운다(시작할 때의 정리는 리소스가 아직 있어 두었다).
#[tokio::test]
async fn interrupted_block_snapshot_has_its_tags_removed_later() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0"])]).await;
    let vm = running_block_vm(&env, &admin, "web", &["z0"]).await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    env.stop_runner().await;
    let tag = get(&env, vm, "a").id.to_string();
    storage.set_tags(zvols[0], &[&tag]);
    env.start_runner_config(storage.clone(), |c| {
        c.orphan_retry = std::time::Duration::from_millis(100)
    })
    .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(find(&env, vm, "a").is_none());
    wait_untagged(&storage, zvols[0]).await;
}

/// 실행기가 멈추면 뒤에서 태그를 지우던 일도 멈춘다.
#[tokio::test]
async fn left_tag_removal_stops_with_the_runner() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0"])]).await;
    let retry = std::time::Duration::from_millis(300);
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| c.orphan_retry = retry)
        .await;
    running_block_vm(&env, &admin, "web", &["z0"]).await;

    storage.time_out_snapshot_create(zvols[0]);
    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    env.stop_runner().await;
    tokio::time::sleep(retry * 3).await;
    assert!(!storage.calls().contains(&("delete", zvols[0])));
    assert_eq!(storage.tags(zvols[0]).len(), 1);
}

/// 실행기가 시작하면 이 노드의 블록 풀 볼륨에서 스냅샷 리소스가 없는 opLite 태그(UUID)를 지운다.
/// 리소스가 있는 태그, opLite 밖의 태그, 다른 노드의 볼륨은 그대로 둔다.
#[tokio::test]
async fn block_snapshot_tags_without_snapshots_are_removed_at_startup() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (vols, storage) = block_volumes(
        &mut env,
        &[
            ("z", zfs("tank/opl"), &["z0", "z1"]),
            ("thin", lvm("vg0"), &["lv0"]),
        ],
    )
    .await;
    let vm = running_block_vm(&env, &admin, "web", &["z0"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
    let a = get(&env, vm, "a").id.to_string();
    let orphan = Uuid::now_v7().to_string();
    storage.set_tags(vols[0], &[&a, &orphan, "autosnap_hourly"]);
    storage.set_tags(vols[1], &[&orphan]);
    storage.set_tags(vols[2], &[&orphan]);
    let mut z1 = env.cluster.get::<VolumeSpec>(vols[1]).unwrap().unwrap();
    z1.status.node = Some(Uuid::now_v7());
    env.cluster
        .update_status::<VolumeSpec>(z1.id, &z1.status)
        .await
        .unwrap();

    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while !storage.tags(vols[2]).is_empty() || storage.tags(vols[0]).len() != 2 {
        assert!(std::time::Instant::now() < deadline, "tags were kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(storage.tags(vols[0]), [a.as_str(), "autosnap_hourly"]);
    assert_eq!(storage.tags(vols[1]), [orphan.as_str()]);
}

/// 실행 중인 VM의 ZFS 스냅샷: VM을 멈추고 같은 풀의 디스크를 한 번에 찍는다. 풀이 다르면 풀마다
/// 찍는다. 메모리는 담지 못한다.
#[tokio::test]
async fn running_vm_zfs_snapshot_takes_each_pool_at_once() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(
        &mut env,
        &[
            ("z", zfs("tank/opl"), &["z0", "z1"]),
            ("y", zfs("tank/other"), &["y0"]),
        ],
    )
    .await;
    let vm = running_block_vm(&env, &admin, "web", &["z0", "y0", "z1"]).await;

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
    assert_eq!(
        storage.snapshot_groups(),
        [vec![zvols[0], zvols[1]], vec![zvols[2]]]
    );
    assert!(env.hv.snapshots(vm).is_empty());
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert!(!a.spec.memory);

    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("zfs disks"),
        "{body}"
    );
}

/// ZFS 디스크는 가장 최근 스냅샷으로만 되돌린다. API가 확인한 뒤에도 디스크에 더 최근 스냅샷이
/// 있으면(opLite 밖에서 만든 것 등) VM을 끄기 전에 그 이름과 함께 실패한다.
#[tokio::test]
async fn zfs_revert_checks_the_latest_disk_snapshot_before_stopping() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0", "z1"])]).await;
    let vm = running_block_vm(&env, &admin, "web", &["z0", "z1"]).await;
    succeeded(&snapshot(&admin, "web", json!({ "name": "a" })).await);
    succeeded(&snapshot(&admin, "web", json!({ "name": "b" })).await);
    let (a, b) = (
        get(&env, vm, "a").id.to_string(),
        get(&env, vm, "b").id.to_string(),
    );
    assert_eq!(storage.tags(zvols[1]), [a.as_str(), b.as_str()]);
    let pid = env.pid(vm).await;
    let before = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let revert_b = || async {
        let (status, task) = admin.post("/vms/web/snapshots/b/revert", Value::Null).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let task = admin.wait_task(&task).await;
        assert_eq!(task["status"]["state"], "failed", "{task}");
        task["status"]["error"].as_str().unwrap().to_string()
    };

    // 자동 스냅샷 도구가 찍은 것
    storage.set_tags(zvols[1], &[&a, &b, "autosnap_hourly"]);
    let error = revert_b().await;
    assert_eq!(
        error,
        "volume \"z1\" has a more recent snapshot \"autosnap_hourly\"; zfs disks can only \
         revert to the latest snapshot"
    );
    // opLite의 스냅샷이면 그 이름이다.
    storage.set_tags(zvols[1], &[&b, &a]);
    let error = revert_b().await;
    assert!(error.contains("more recent snapshot \"a\""), "{error}");
    // 되돌릴 스냅샷이 디스크에 없다.
    storage.set_tags(zvols[1], &[&a]);
    let error = revert_b().await;
    assert!(error.contains("has no snapshot \"b\""), "{error}");

    // 끄지도, 스펙을 바꾸지도 않았다.
    let got = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(got.spec, before.spec);
    assert_eq!(env.pid(vm).await, pid);
    assert!(!storage.calls().iter().any(|(call, _)| *call == "revert"));

    // 가장 최근이면 되돌린다.
    storage.set_tags(zvols[1], &[&a, &b]);
    let (status, task) = admin.post("/vms/web/snapshots/b/revert", Value::Null).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let reverts: Vec<_> = storage
        .calls()
        .into_iter()
        .filter(|(call, _)| *call == "revert")
        .collect();
    assert_eq!(reverts, [("revert", zvols[0]), ("revert", zvols[1])]);
}

/// 실행 중인 VM의 ZFS 디스크도 스토리지로 먼저 늘린 뒤 QEMU에 알린다.
#[tokio::test]
async fn running_vm_zfs_disk_grows_in_storage_before_qemu() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (zvols, storage) = block_volumes(&mut env, &[("z", zfs("tank/opl"), &["z0"])]).await;
    let vm = running_block_vm(&env, &admin, "web", &["z0"]).await;
    storage.round_resizes(4 << 20);

    let (status, task) = admin
        .post("/volumes/z0/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(storage.calls(), [("resize", zvols[0])]);
    assert_eq!(env.hv.disk_size(vm, 0), Some(4 << 20));
}

#[tokio::test]
async fn running_vm_lvm_disk_grows_in_storage_before_qemu() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;

    // LV를 늘리는 동안 QEMU에는 아직 알리지 않았다.
    storage.pause();
    let (status, task) = admin
        .post("/volumes/lv0/resize", json!({ "size_bytes": 2 << 20 }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert_eq!(env.hv.disk_size(vm, 0), None);
    storage.resume();
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(storage.calls(), [("resize", lvs[0])]);
    assert_eq!(env.hv.disk_size(vm, 0), Some(2 << 20));
    let lv0 = env.cluster.get::<VolumeSpec>(lvs[0]).unwrap().unwrap();
    assert_eq!(lv0.spec.size_bytes, 2 << 20);
}

/// LVM이 크기를 extent 단위로 올리면 QEMU에도 기록한 크기와 같은 LV의 실제 크기를 알린다.
#[tokio::test]
async fn running_vm_lvm_disk_grows_to_the_lv_size() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    storage.round_resizes(4 << 20);

    let (status, task) = admin
        .post(
            "/volumes/lv0/resize",
            json!({ "size_bytes": (2 << 20) + 512 }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    assert_eq!(env.hv.disk_size(vm, 0), Some(4 << 20));
    let lv0 = env.cluster.get::<VolumeSpec>(lvs[0]).unwrap().unwrap();
    assert_eq!(lv0.spec.size_bytes, 4 << 20);
}

/// LVM 디스크의 VM은 예약이 메모리를 원해도 디스크만 찍는다(메모리를 요청하면 거절되어 예약이
/// 실패로 남는다).
#[tokio::test]
async fn scheduled_lvm_snapshot_leaves_out_memory() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let mut spec = current.spec.clone();
    spec.snapshot_schedule = Some(opl_types::vm::SnapshotSchedule {
        every: opl_types::vm::ScheduleEvery::Hourly,
        keep: 3,
        memory: true,
    });
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();

    let scheduler = opl_api::SnapshotScheduler::spawn(
        env.state.clone(),
        opl_api::SnapshotSchedulerConfig {
            tick: std::time::Duration::from_millis(50),
            hour: std::time::Duration::from_secs(3600),
        },
    );
    let deadline = std::time::Instant::now() + common::WAIT;
    let made = loop {
        let (_, all) = env.cluster.list::<SnapshotSpec>().unwrap();
        if let Some(s) = all
            .into_iter()
            .find(|s| s.spec.vm == vm && s.spec.scheduled && s.status.phase == SnapshotPhase::Ready)
        {
            break s;
        }
        let error = env.cluster.get::<VmSpec>(vm).unwrap().unwrap().status;
        assert_eq!(error.schedule_error, None);
        assert!(
            std::time::Instant::now() < deadline,
            "no scheduled snapshot"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    scheduler.shutdown().await;
    assert!(!made.spec.memory);
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert!(env.hv.snapshots(vm).is_empty());
}

/// 멈춘 동안 Task가 시간을 넘겨 버려져도 VM을 다시 돌린다.
#[tokio::test]
async fn abandoned_lvm_snapshot_still_resumes_the_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    env.stop_runner().await;
    env.start_runner_config(storage.clone(), |c| {
        c.timeout = std::time::Duration::from_millis(500)
    })
    .await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    assert!(env.hv.is_paused(vm));
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.hv.is_paused(vm) {
        assert!(std::time::Instant::now() < deadline, "VM stayed paused");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    storage.resume();
}

/// QEMU가 스스로 멈춘 VM(I/O 오류 등)은 멈추지도 이어서 실행하지도 않고 디스크만 찍는다.
#[tokio::test]
async fn lvm_snapshot_leaves_a_vm_paused_by_qemu_paused() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    // Mock 하이퍼바이저에서는 멈춘 것처럼 보이게 한다.
    env.hv.pause(vm).await.unwrap();

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    succeeded(&task);
    assert!(env.hv.is_paused(vm));
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert_eq!(get(&env, vm, "a").status.phase, SnapshotPhase::Ready);
}

/// QEMU가 스스로 멈춘 VM(`io-error` 등, 실행 중도 일시정지도 아닌 상태)은 멈추지도 이어서 실행하지도
/// 않고 디스크만 찍는다.
#[tokio::test]
async fn lvm_snapshot_of_a_vm_stopped_on_io_error_neither_pauses_nor_resumes_it() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    env.hv
        .set_status(vm, Some(RuntimeStatus::Other("io-error".into())));

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    succeeded(&task);
    assert_eq!(env.hv.pause_calls(vm), Vec::<&str>::new());
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    assert_eq!(get(&env, vm, "a").status.phase, SnapshotPhase::Ready);
}

/// QMP로 다룰 수 없는 VM은 멈출 수 없으므로 디스크를 건드리기 전에 실패한다.
#[tokio::test]
async fn lvm_snapshot_of_an_unresponsive_vm_fails_before_touching_the_disks() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (_, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    env.hv.set_unresponsive(vm, true);

    let task = snapshot(&admin, "web", json!({ "name": "a" })).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"]
            .as_str()
            .unwrap()
            .contains("not responding"),
        "{task}"
    );
    assert_eq!(env.hv.pause_calls(vm), Vec::<&str>::new());
    assert!(storage.calls().is_empty());
    assert!(find(&env, vm, "a").is_none());
}

/// 디스크는 찍었는데 VM을 이어서 실행하지 못하면 스냅샷은 그대로 두고 Task만 실패로 끝낸다.
#[tokio::test]
async fn lvm_snapshot_taken_but_not_resumed_is_kept() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 멈춘 동안 QMP를 잃었다.
    env.hv.set_unresponsive(vm, true);
    storage.resume();
    let task = admin.wait_task(&task).await;
    env.hv.set_unresponsive(vm, false);
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("snapshot taken; resuming the VM failed:"),
        "{error}"
    );
    assert_eq!(storage.calls(), [("create", lvs[0])]);
    let a = get(&env, vm, "a");
    assert_eq!(a.status.phase, SnapshotPhase::Ready);
    assert_eq!(position(&env, vm), Some(a.id));
}
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
```

을 다음으로 바꾼다:

```rust
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
async fn mark_ready(env: &Env, id: Uuid) {
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
```

을 다음으로 바꾼다:

```rust
async fn mark_ready(env: &Env, id: Uuid) {
    mark_ready_at(env, id, 1).await;
}

/// 스냅샷을 `created_at`에 만든 것으로 Ready로 바꾼다.
async fn mark_ready_at(env: &Env, id: Uuid, created_at: u64) {
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(created_at),
        vmstate_bytes: None,
```

`crates/opl-api/tests/snapshots.rs`에서

```rust

/// LVM-thin 풀 `thin`과 그 풀의 바로 Ready인 볼륨 `name`(형식 `format`)을 저장소에 직접 만든다.
async fn lvm_volume(env: &Env, name: &str, format: ImageFormat) {
    if env
        .cluster
        .get_by_name::<StoragePoolSpec>("thin")
        .unwrap()
        .is_none()
    {
        let pool = Resource::new(
            "thin",
            StoragePoolSpec {
                backend: opl_types::storage::PoolBackend::LvmThin {
                    vg: "vg0".into(),
                    thinpool: "tp".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        env.cluster.create(&pool).await.unwrap();
        env.pool_ready("thin").await;
    }
```

을 다음으로 바꾼다:

```rust

/// 블록 풀 `pool`(`backend`, 없으면 만든다)과 그 풀의 바로 Ready인 볼륨 `name`(형식 `format`)을
/// 저장소에 직접 만든다.
async fn block_volume(
    env: &Env,
    pool: &str,
    backend: PoolBackend,
    name: &str,
    format: ImageFormat,
) {
    if env
        .cluster
        .get_by_name::<StoragePoolSpec>(pool)
        .unwrap()
        .is_none()
    {
        let spec = StoragePoolSpec {
            backend,
            nodes: vec![],
            shared: false,
        };
        env.cluster
            .create(&Resource::new(pool, spec))
            .await
            .unwrap();
        env.pool_ready(pool).await;
    }
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
        VolumeSpec {
            pool: "thin".into(),
            size_bytes: MIB,
```

을 다음으로 바꾼다:

```rust
        VolumeSpec {
            pool: pool.into(),
            size_bytes: MIB,
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

#[tokio::test]
```

을 다음으로 바꾼다:

```rust
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
}

/// LVM-thin 풀 `thin`과 그 풀의 바로 Ready인 볼륨 `name`(형식 `format`)을 저장소에 직접 만든다.
async fn lvm_volume(env: &Env, name: &str, format: ImageFormat) {
    let backend = PoolBackend::LvmThin {
        vg: "vg0".into(),
        thinpool: "tp".into(),
    };
    block_volume(env, "thin", backend, name, format).await;
}

/// ZFS 풀 `zfs`와 그 풀의 바로 Ready인 raw 볼륨 `name`을 저장소에 직접 만든다.
async fn zfs_volume(env: &Env, name: &str) {
    let backend = PoolBackend::Zfs {
        dataset: "tank/opl".into(),
    };
    block_volume(env, "zfs", backend, name, ImageFormat::Raw).await;
}

#[tokio::test]
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

#[tokio::test]
async fn zfs_disks_take_block_snapshots_of_their_own_method() {
    let env = env().await;
    let admin = env.admin().await;
    zfs_volume(&env, "z0").await;
    zfs_volume(&env, "z1").await;
    lvm_volume(&env, "lv0", ImageFormat::Raw).await;

    // 메모리는 담을 곳이 없다. 실행 중이어도 기본은 디스크만이다.
    let web = vm_with_disks(&admin, "web", &["z0"]).await;
    set_vm_status(&env, web, VmPhase::Running, Some(env.node)).await;
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "m", "memory": true }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert!(
        body["message"].as_str().unwrap().contains("zfs disks"),
        "{body}"
    );
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(!snapshot_of(&env, web, "a").await.spec.memory);

    // 블록 스냅샷이어도 LVM과 ZFS는 섞지 않는다.
    vm_with_disks(&admin, "mixed", &["z1", "lv0"]).await;
    let (status, body) = admin
        .post("/vms/mixed/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("different snapshot methods"),
        "{body}"
    );
}

#[tokio::test]
async fn zfs_disks_revert_only_to_the_latest_snapshot() {
    let env = env().await;
    let admin = env.admin().await;
    zfs_volume(&env, "z0").await;
    lvm_volume(&env, "lv0", ImageFormat::Raw).await;
    let web = vm_with_disks(&admin, "web", &["z0"]).await;
    let thin = vm_with_disks(&admin, "thin", &["lv0"]).await;
    // 만든 시각 순서가 id 순서와 반대여도 만든 시각을 따른다.
    for (vm, key) in [(web, "web"), (thin, "thin")] {
        for name in ["s2", "s1"] {
            let path = format!("/vms/{key}/snapshots");
            let (status, body) = admin.post(&path, json!({ "name": name })).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        }
        finish_tasks(&env).await;
        mark_ready_at(&env, snapshot_of(&env, vm, "s1").await.id, 10).await;
        mark_ready_at(&env, snapshot_of(&env, vm, "s2").await.id, 20).await;
    }
    let revert = |vm: &'static str, name: &'static str| {
        let admin = &admin;
        async move {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            admin.post(&path, Value::Null).await
        }
    };

    let (status, body) = revert("web", "s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");
    assert!(
        body["message"].as_str().unwrap().contains("latest"),
        "{body}"
    );
    let (status, body) = revert("web", "s2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // LVM 디스크는 어느 스냅샷으로도 되돌린다.
    let (status, body) = revert("thin", "s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;

    // 만드는 중인 스냅샷도 나중의 것이다.
    let s2 = snapshot_of(&env, web, "s2").await;
    env.cluster
        .delete::<SnapshotSpec>(s2.id, None)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "s3" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) = revert("web", "s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let s3 = snapshot_of(&env, web, "s3").await;
    env.cluster
        .delete::<SnapshotSpec>(s3.id, None)
        .await
        .unwrap();
    let (status, body) = revert("web", "s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 554개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 스냅샷 규칙을 블록 백엔드(LVM-thin, ZFS)로 일반화"
```

---

### Task 6: 실제 ZFS 시험(zfs-it)과 CI

기능 `zfs-it`(opl-driver, opld)과 Linux 전용 `crates/opl-driver/tests/zfs_storage.rs`(만들기·정보·크기·지우기, vmdk 가져오기, 최근 스냅샷 되돌리기, 원자적 다중 스냅샷, 스냅샷 장치 읽기와 ZFS ↔ 디렉터리 복사 — 읽기는 `qemu-io -r`), `crates/opld/tests/zfs.rs`(ZFS 풀로 VM을 띄워 장치 확인, 메모리 스냅샷 409, 실행 중 스냅샷, 최근이 아닌 되돌리기 409와 그 뒤 되돌리기, 디렉터리 풀로 qcow2 복제, 지우기). `OPL_TEST_ZFS=POOL/DATASET`. CI `linux-kvm`에 zfsutils-linux, `modprobe zfs`, udev 규칙 다시 읽기, 파일 위 zpool `opltestz`·`opltestz/opl`, `zfs version`·`zpool status`.

**Files:**
- Create: `crates/opl-driver/tests/zfs_storage.rs`
- Create: `crates/opld/tests/zfs.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-driver/Cargo.toml`
- Modify: `crates/opld/Cargo.toml`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
          sudo lvs opltest
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

을 다음으로 바꾼다:

```yaml
          sudo lvs opltest
      # ZFS 풀 시험이 zvol을 만들 데이터셋 `opltestz/opl`. zpool은 파일 하나 위에 둔다.
      - name: ZFS pool on a file
        run: |
          sudo apt-get update
          sudo apt-get install -y zfsutils-linux
          sudo modprobe zfs
          sudo udevadm control --reload-rules
          truncate -s 2G "$RUNNER_TEMP/opltestz.img"
          sudo zpool create opltestz "$RUNNER_TEMP/opltestz.img"
          sudo zfs create opltestz/opl
          sudo zfs list -r opltestz
          zfs version
          sudo zpool status opltestz
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
          OPL_TEST_LVM: opltest/tp
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it,opl-driver/lvm-it,opld/lvm-it,opl-driver/zfs-it,opld/zfs-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
          OPL_TEST_LVM: opltest/tp
          OPL_TEST_ZFS: opltestz/opl
```

`crates/opl-driver/Cargo.toml`에서

```toml
lvm-it = []
```

을 다음으로 바꾼다:

```toml
lvm-it = []
# 실제 ZFS 풀의 통합 테스트. Linux에서 root, zfsutils-linux, qemu-img, qemu-io와 테스트가 zvol을 만들고
# 지워도 되는 데이터셋(`OPL_TEST_ZFS=POOL/DATASET`)이 필요하다.
zfs-it = []
```

`crates/opl-driver/tests/zfs_storage.rs`를 만든다:

```rust
//! 실제 ZFS 풀의 저장소 통합 테스트. 관리자가 만든 데이터셋(`OPL_TEST_ZFS=POOL/DATASET`)에 볼륨
//! zvol을 만들고 가져오고, ZFS 스냅샷을 찍어 되돌리고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_ZFS=opltestz/opl cargo test -p opl-driver --features zfs-it --test zfs_storage`
//! Linux에서 root, zfsutils-linux, qemu-img, qemu-io가 필요하다.
#![cfg(all(feature = "zfs-it", target_os = "linux"))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage, Storages, SystemZfs, ZfsStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// `OPL_TEST_ZFS`의 데이터셋
fn dataset() -> String {
    std::env::var("OPL_TEST_ZFS").expect("OPL_TEST_ZFS must name POOL/DATASET")
}

fn zfs_pool() -> StoragePool {
    Resource::new(
        "zfs",
        StoragePoolSpec {
            backend: PoolBackend::Zfs { dataset: dataset() },
            nodes: vec![],
            shared: false,
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

fn storage() -> ZfsStorage {
    ZfsStorage::new("qemu-img", Arc::new(SystemZfs))
}

/// 데이터셋 아래 `name`(zvol, 또는 `zvol@tag` 스냅샷)의 장치 노드
fn device(name: &str) -> PathBuf {
    Path::new("/dev/zvol").join(dataset()).join(name)
}

fn volume_device(id: Uuid) -> PathBuf {
    device(&format!("opl-{id}"))
}

/// 데이터셋 아래 zvol과 스냅샷 이름(데이터셋 부분을 뺀 것) 중 `prefix`로 시작하는 것
fn zfs_names(prefix: &str) -> Vec<String> {
    let dataset = dataset();
    let out = Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-t", "volume,snapshot", "-r"])
        .arg(&dataset)
        .output()
        .expect("run zfs list");
    assert!(out.status.success(), "{out:?}");
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().strip_prefix(&format!("{dataset}/")))
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// 지운 zvol의 장치 노드는 udev가 조금 뒤에 없앤다.
fn wait_gone(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "{} remains", path.display());
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: ImageFormat, cmd: &str) -> bool {
    let format = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    let mut qemu_io = Command::new("qemu-io");
    // 읽기는 읽기 전용으로 연다: ZFS 스냅샷 장치는 쓰기로 열 수 없다.
    if cmd.starts_with("read") {
        qemu_io.arg("-r");
    }
    qemu_io
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_inspect_resize_and_delete_volume() {
    let pool = zfs_pool();
    let storage = storage();
    let id = Uuid::now_v7();

    // 크기는 1 MiB 배수로 올린다.
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, volume_device(id));
    assert!(path.exists(), "{}", path.display());
    assert_eq!(zfs_names(&format!("opl-{id}")), vec![format!("opl-{id}")]);
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(info.allocated_bytes < info.virtual_bytes, "{info:?}");
    assert!(qemu_io(&path, RAW, "write -P 0xab 0 1M"));
    let (location, _) = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, path);
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

    // 늘리면 내용은 그대로다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지 않는다).
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(zfs_names(&format!("opl-{id}")).is_empty());
    wait_gone(&path);
    let gone = storage.volume_info(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let err = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap_err();
    assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = zfs_pool();
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
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    assert_eq!(path, volume_device(id));
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, MIB, "{info:?}");
    let mut head = vec![0u8; MIB as usize];
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert!(head == pattern, "imported contents differ");
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 zvol을 남기지 않는다.
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
    assert!(zfs_names(&format!("opl-{missing}")).is_empty());
}

#[tokio::test]
async fn snapshots_revert_to_the_latest_and_go_with_the_volume() {
    let pool = zfs_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    let path = storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, RAW));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Qcow2));
    let zvol = format!("opl-{id}");

    assert!(qemu_io(&path, RAW, "write -P 0xab 0 64k"));
    let s1 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone()]
    );
    assert_eq!(zfs_names(&zvol), vec![zvol.clone(), format!("{zvol}@{s1}")]);
    let dup = storage.snapshot_create(&pool, id, RAW, &s1).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    // 가장 최근 스냅샷으로 되돌리면 그때 내용이 같은 장치에 돌아오고 스냅샷은 남는다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    // 더 최근 스냅샷이 있으면 되돌리지 않고 아무것도 바꾸지 않는다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    let s2 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io(&path, RAW, "write -P 0x5a 0 64k"));
    let err = storage
        .snapshot_revert(&pool, id, RAW, &s1)
        .await
        .unwrap_err();
    match err {
        DriverError::InvalidSpec(msg) => assert!(msg.contains("latest snapshot"), "{msg}"),
        e => panic!("{e:?}"),
    }
    assert!(qemu_io(&path, RAW, "read -P 0x5a 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );
    // 최근 것을 지우면 그 앞 스냅샷으로 되돌릴 수 있다.
    storage.snapshot_delete(&pool, id, RAW, &s2).await.unwrap();
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, RAW, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(
        storage
            .snapshot_tags(&pool, id, RAW)
            .await
            .unwrap()
            .is_empty()
    );
    // 이미 없어도 성공한다.
    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    // 여러 볼륨을 한 번에 찍는다. 볼륨을 지우면 남은 스냅샷도 함께 지운다.
    let other = Uuid::now_v7();
    storage
        .create_volume(&pool, other, 64 * MIB, RAW)
        .await
        .unwrap();
    storage
        .snapshot_many(&pool, &[id, other], RAW, "both")
        .await
        .unwrap();
    for v in [id, other] {
        assert_eq!(
            storage.snapshot_tags(&pool, v, RAW).await.unwrap(),
            vec!["both".to_string()]
        );
    }
    // 한 볼륨에라도 그 태그가 있으면 어느 볼륨에도 찍지 않는다.
    let third = Uuid::now_v7();
    storage
        .create_volume(&pool, third, 64 * MIB, RAW)
        .await
        .unwrap();
    let dup = storage
        .snapshot_many(&pool, &[third, other], RAW, "both")
        .await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");
    assert!(
        storage
            .snapshot_tags(&pool, third, RAW)
            .await
            .unwrap()
            .is_empty()
    );
    for v in [id, other, third] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        assert!(zfs_names(&format!("opl-{v}")).is_empty());
    }
}

#[tokio::test]
async fn copies_snapshots_within_and_across_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = zfs_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_zfs(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, RAW, "write -P 0xab 0 64k"));
    storages
        .snapshot_create(&pool, src, RAW, "s1")
        .await
        .unwrap();
    assert!(qemu_io(&src_path, RAW, "write -P 0xcd 0 64k"));
    // `snapdev=visible`이라 스냅샷도 장치로 읽는다.
    assert!(qemu_io(
        &device(&format!("opl-{src}@s1")),
        RAW,
        "read -P 0xab 0 64k"
    ));

    // 같은 풀 안: 스냅샷 때의 내용을 새 zvol로
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(copy), RAW, "read -P 0xab 0 64k"));
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
    assert!(qemu_io(&out_path, qcow2, "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw zvol, 지금 내용
    assert!(qemu_io(&out_path, qcow2, "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(back), RAW, "read -P 0x5a 0 64k"));

    // 없는 스냅샷은 실패하고 대상 zvol을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("gone"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(zfs_names(&format!("opl-{missing}")).is_empty());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(zfs_names(&format!("opl-{id}")).is_empty());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}
```

`crates/opld/Cargo.toml`에서

```toml
lvm-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

을 다음으로 바꾼다:

```toml
lvm-it = []
# 실제 ZFS 풀의 볼륨(zvol)으로 VM을 띄우는 E2E 테스트. qemu-it의 준비물에 더해 Linux에서 root,
# zfsutils-linux, qemu-io와 테스트가 zvol을 만들고 지워도 되는 데이터셋(`OPL_TEST_ZFS=POOL/DATASET`)이 필요하다.
zfs-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

`crates/opld/tests/zfs.rs`를 만든다:

```rust
//! 실제 ZFS 풀: opld가 관리자가 만든 데이터셋을 확인해 준비됐다고 알리고, 그 풀의 raw 볼륨(zvol)을
//! 블록 장치로 붙인 VM을 띄운다. 실행 중에 디스크 스냅샷(ZFS 스냅샷)을 찍고, 멈춘 뒤 가장 최근
//! 스냅샷으로만 되돌리고, 스냅샷에서 디렉터리 풀로 복제하고, 모두 지운다.
//! 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 꺼진 VM의 zvol 장치를 `qemu-io`로 본다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs`
//! Linux에서 root, zfsutils-linux, QEMU(`qemu-it`와 같다), qemu-img, qemu-io가 필요하다.
//! `OPL_TEST_ZFS`는 테스트가 zvol을 만들고 지워도 되는 데이터셋이다.
#![cfg(all(feature = "zfs-it", target_os = "linux"))]

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

/// 데이터셋 아래 zvol과 스냅샷 이름(데이터셋 부분을 뺀 것) 중 `prefix`로 시작하는 것
fn zfs_names(dataset: &str, prefix: &str) -> Vec<String> {
    let out = Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-t", "volume,snapshot", "-r"])
        .arg(dataset)
        .output()
        .expect("run zfs list");
    assert!(out.status.success(), "{out:?}");
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().strip_prefix(&format!("{dataset}/")))
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
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

#[tokio::test]
async fn zfs_pool_runs_snapshots_reverts_and_clones_a_vm() {
    let dataset = std::env::var("OPL_TEST_ZFS").expect("OPL_TEST_ZFS must name POOL/DATASET");
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    d.start().await;
    d.init("zfs").await;
    d.login().await;
    let c = client(&d);

    // 마운터가 데이터셋을 확인하면 준비됐다고 알리고 데이터셋의 용량을 보인다.
    let backend = json!({ "kind": "zfs", "dataset": dataset });
    c.post(
        "/storage-pools",
        &json!({ "name": "zfs", "spec": { "backend": backend } }),
    )
    .await
    .unwrap();
    d.wait_pool_ready("zfs").await;
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["zfs"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨은 zvol `opl-{id}`이고 장치 노드로 쓴다.
    d.ctl(&[
        "volume", "create", "root", "--pool", "zfs", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let zvol = format!("opl-{}", volume["id"].as_str().unwrap());
    let dev = Path::new("/dev/zvol").join(&dataset).join(&zvol);
    assert!(dev.exists(), "{}", dev.display());
    assert!(qemu_io(&dev, "raw", "write -P 0xab 0 64k"));

    // VM은 zvol을 host_device로 연다.
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

    // ZFS 디스크는 메모리를 담을 수 없다.
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
    let before_snapshot = format!("{zvol}@{}", before["id"].as_str().unwrap());
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "running", "{web}");
    assert_eq!(web["status"]["pid"].as_u64(), Some(pid), "{web}");

    // 멈춘 뒤 디스크를 바꾸고 스냅샷을 하나 더 찍으면 앞 스냅샷으로는 되돌릴 수 없다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    assert!(qemu_io(&dev, "raw", "write -P 0xcd 0 64k"));
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let resp = c
        .request(Method::POST, "/vms/web/snapshots/before/revert")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "snapshot_unsupported", "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("latest snapshot"),
        "{body}"
    );
    assert!(qemu_io(&dev, "raw", "read -P 0xcd 0 64k"));

    // 최근 스냅샷을 지우면 앞 스냅샷이 가장 최근이라 되돌릴 수 있고, 그때 내용이 같은 장치에 돌아온다.
    d.ctl(&["vm", "snapshot", "delete", "web", "after"])
        .await
        .unwrap();
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io(&dev, "raw", "read -P 0xab 0 64k"));
    assert_eq!(
        zfs_names(&dataset, &zvol),
        vec![zvol.clone(), before_snapshot.clone()]
    );
    let web = c.get("/vms/web").await.unwrap();
    assert_eq!(web["status"]["phase"], "stopped", "{web}");

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
    assert!(qemu_io(&dev, "raw", "write -P 0xcd 0 64k"));
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
    assert!(qemu_io(&copy_file, "qcow2", "read -P 0xab 0 64k"));

    // 스냅샷과 함께 VM을 지우면 ZFS 스냅샷이, 볼륨을 지우면 zvol이 없어진다.
    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-root"]).await.unwrap();
    assert!(!copy_file.exists());
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert_eq!(zfs_names(&dataset, &zvol), vec![zvol.clone()]);
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(zfs_names(&dataset, &zvol).is_empty());
    d.ctl(&["pool", "delete", "local"]).await.unwrap();
    d.ctl(&["pool", "delete", "zfs"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 554개, 실패 0

Run: `cargo clippy -p opl-driver -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음(macOS에서는 zfs 시험 파일이 비어 있다)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "test(storage): 실제 ZFS 풀 통합 시험과 CI"
```

---

### Task 7: oplctl: ZFS 풀

`pool create NAME --zfs DATASET`(다른 종류 플래그·`--shared`와 함께 못 씀, 검증은 API), 목록·상세의 종류 `zfs`·위치 데이터셋, 형식 기본값 raw.

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
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀을 만든다.
    #[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs"])))]
    Create {
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
        /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
        /// NFS 풀은 늘 공유다.
        #[arg(long, requires = "path", conflicts_with = "nfs")]
        shared: bool,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    Delete {
        pool: String,
    },
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
        /// 생략하면 풀 종류로 정한다(LVM-thin과 ZFS는 raw, 그 밖은 qcow2).
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
        /// 생략하면 풀 종류로 정한다(LVM-thin과 ZFS는 raw, 그 밖은 qcow2).
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

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS)은 raw(스냅샷은 블록 스냅샷), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" {
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
            PoolCmd::Create {
                name,
                path,
                nfs,
                nfs_options,
                lvm_thin,
                zfs,
                shared,
                nodes,
            } => {
                // NFS 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
                let backend = match (nfs, lvm_thin, zfs) {
                    (Some(n), _, _) => json!({
                        "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
                    }),
                    (None, Some(l), _) => {
                        json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
                    }
                    (None, None, Some(dataset)) => json!({ "kind": "zfs", "dataset": dataset }),
                    (None, None, None) => json!({ "kind": "dir", "path": path }),
                };
                let mut spec = json!({ "backend": backend, "nodes": nodes });
                if shared {
                    spec["shared"] = json!(true);
                }
                let body = json!({ "name": name, "spec": spec });
                client.post("/storage-pools", &body).await?;
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
/// `vg/thinpool`, ZFS 풀은 데이터셋
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
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
                    cmd:
                        PoolCmd::Create {
                            path,
                            nfs,
                            nfs_options,
                            shared,
                            ..
                        },
                } => (path, nfs, nfs_options, shared),
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
                    cmd: PoolCmd::Create { lvm_thin, .. },
                } => lvm_thin,
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
        // 종류(--path, --nfs, --lvm-thin, --zfs) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in ["--path", "--nfs", "--lvm-thin", "--zfs"] {
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
                    cmd: PoolCmd::Create { zfs, .. },
                } => zfs,
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
Expected: PASS 555개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): ZFS 풀 만들기와 ZFS 풀의 볼륨 형식 기본값 raw"
```

---

### Task 8: 웹: ZFS 풀, 블록 스냅샷 판단, 되돌리기 제한

풀 만들기의 ZFS(데이터셋 필드, `zfsDatasetError`는 서버와 같은 규칙), 목록 종류·위치, `poolFormats`가 ZFS면 raw, `blockSnapshotKind`와 `snapshotSupport.ok.block`(LVM과 ZFS가 섞이면 안내, 메모리 숨김, `snapshots.blockHint`), `zfsNotLatest`로 ZFS 디스크 VM의 가장 최근이 아닌 스냅샷 되돌리기를 이유와 함께 막음(이유는 줄바꿈해 표를 넓히지 않음). en·ko.

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
    "snapshot_unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. Snapshots of disks in LVM-thin or ZFS pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
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
    "unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. {{volume}} is {{format}}.",
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
    "mixed": "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. This VM mixes them.",
    "blockHint": "Disks in LVM-thin and ZFS pools are saved without memory. A running VM pauses briefly while they are saved.",
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
    "createDescription": "Where volumes are kept: a directory on each node, an NFS export that the nodes mount, or an LVM thin pool or ZFS dataset on each node.",
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
      "zfs": "ZFS"
    },
    "location": "Location",
    "kindHint": {
      "dir": "Create the directory on each node first.",
      "nfs": "Each node mounts the export under its data directory. Mounting needs Linux nodes running as root. The pool is shared by its nodes.",
      "lvm-thin": "Create the volume group and thin pool on each node first. Volumes are raw thin volumes kept on the node that creates them, and their snapshots hold disks only. Needs Linux nodes running as root.",
      "zfs": "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root."
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
      "zfsDataset": "Use a dataset name such as tank/opl, at most 177 characters: parts separated by / with letters, digits, _, ., : or -, none empty, . or .. or starting with -. The pool name comes first and starts with a letter."
    },
    "vg": "Volume group",
    "thinpool": "Thin pool",
    "dataset": "Dataset"
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
    "scheduleNote": "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
    "scheduleEvery": "Take snapshots",
    "scheduleOff": "Off",
    "scheduleKeep": "Snapshots to keep",
    "scheduleRaw": "{{volume}} is {{format}}, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
    "scheduleMemoryHint": "Memory is saved only while the VM is running and has disks.",
    "scheduleMixed": "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools) are mixed, so scheduled snapshots will fail."
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
    "snapshot_unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있는 VM만 스냅샷을 만들 수 있습니다. LVM-thin·ZFS 풀의 디스크 스냅샷에는 메모리를 담을 수 없고, ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
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
    "unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있는 VM만 스냅샷을 만들 수 있습니다. {{volume}}은(는) {{format}}입니다.",
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
    "mixed": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있는 VM만 스냅샷을 만들 수 있습니다. 이 VM에는 여러 방식이 섞여 있습니다.",
    "blockHint": "LVM-thin·ZFS 풀의 디스크는 메모리 없이 저장합니다. 실행 중인 VM은 저장하는 동안 잠깐 멈춥니다.",
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
    "createDescription": "볼륨을 두는 곳입니다: 각 노드의 디렉터리, 노드들이 마운트하는 NFS 내보내기, 또는 각 노드의 LVM thin pool이나 ZFS 데이터셋.",
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
      "zfs": "ZFS"
    },
    "location": "위치",
    "kindHint": {
      "dir": "먼저 각 노드에 디렉터리를 만드세요.",
      "nfs": "각 노드가 내보내기를 데이터 디렉터리 아래에 마운트합니다. 마운트는 root로 도는 Linux 노드에서만 됩니다. 풀의 노드들이 함께 쓰는 공유 풀입니다.",
      "lvm-thin": "먼저 각 노드에 볼륨 그룹과 thin pool을 만드세요. 볼륨은 만든 노드에 두는 raw thin 볼륨이고, 스냅샷은 디스크만 담습니다. root로 도는 Linux 노드에서만 됩니다.",
      "zfs": "먼저 각 노드에 데이터셋을 만드세요. 볼륨은 만든 노드에 두는 raw zvol이고, 스냅샷은 디스크만 담습니다. 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다. root로 도는 Linux 노드에서만 됩니다."
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
      "zfsDataset": "tank/opl처럼 177자 이하의 데이터셋 이름을 쓰세요: /로 나눈 각 부분은 영문자, 숫자, _, ., :, -로 되고 비어 있거나 . 또는 ..이거나 -로 시작하면 안 됩니다. 맨 앞은 풀 이름이고 영문자로 시작합니다."
    },
    "vg": "볼륨 그룹",
    "thinpool": "Thin pool",
    "dataset": "데이터셋"
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
    "scheduleNote": "주기마다 스냅샷을 만들고 예약으로 만든 것은 최근 것만 남깁니다. 직접 만든 스냅샷은 지우지 않습니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있어야 합니다.",
    "scheduleEvery": "스냅샷 주기",
    "scheduleOff": "끔",
    "scheduleKeep": "보관 개수",
    "scheduleRaw": "{{volume}}이(가) {{format}}이라 예약 스냅샷이 실패합니다. 디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있어야 합니다.",
    "scheduleMemoryHint": "VM이 실행 중이고 디스크가 있을 때만 메모리도 저장합니다.",
    "scheduleMixed": "스냅샷 방식(qcow2, LVM-thin 풀, ZFS 풀)이 다른 디스크가 섞여 있어 예약 스냅샷이 실패합니다."
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
      pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
    ],
```

을 다음으로 바꾼다:

```tsx
      pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
      pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } }),
    ],
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(fast).toHaveTextContent("node-a: No status reported yet");
});
```

을 다음으로 바꾼다:

```tsx
  expect(fast).toHaveTextContent("node-a: No status reported yet");
  const tank = within(table).getByText("tank").closest("tr")!;
  expect(tank).toHaveTextContent("ZFS");
  expect(tank).toHaveTextContent("tank/opl");
});
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  await vi.waitFor(() => expect(body).toBeDefined());
  console.log("DBG", JSON.stringify(body), dialog.textContent);
  expect(await screen.findByText("Created storage pool fast.")).toBeInTheDocument();
```

을 다음으로 바꾼다:

```tsx
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(await screen.findByText("Created storage pool fast.")).toBeInTheDocument();
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx

it("sends NFS mount options", async () => {
```

을 다음으로 바꾼다:

```tsx

it("creates a ZFS pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("tank"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "tank");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Shared storage" }));
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "zfs");
  expect(within(dialog).getByLabelText("Kind")).toHaveAccessibleDescription(
    "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root.",
  );
  // 노드 로컬 풀이라 공유를 고를 수 없고, 다른 종류의 입력도 없다.
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  expect(within(dialog).queryByLabelText("Directory")).toBeNull();
  expect(within(dialog).queryByLabelText("Volume group")).toBeNull();
  await userEvent.type(within(dialog).getByLabelText("Dataset"), "tank/-opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Dataset")).toHaveAccessibleDescription(
    "Use a dataset name such as tank/opl, at most 177 characters: parts separated by / with letters, digits, _, ., : or -, none empty, . or .. or starting with -. The pool name comes first and starts with a letter.",
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Dataset"));
  await userEvent.type(within(dialog).getByLabelText("Dataset"), "tank/opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  // 디렉터리 풀에서 고른 공유는 보내지 않는다.
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "tank",
      spec: { backend: { kind: "zfs", dataset: "tank/opl" }, nodes: [] },
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
  lvmNameError,
  nfsExportError,
  nfsOptionsError,
  nfsServerError,
  poolCapacity,
  poolLocation,
  poolNodes,
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
    // NFS 풀의 `shared`는 보내지 않는다(서버가 늘 공유로 둔다). 빈 옵션은 커널 기본값이다.
    // LVM-thin·ZFS 풀은 노드 로컬이라 `shared`를 보내지 않는다.
    const sharedDir = kind === "dir" && shared;
    const backend =
      kind === "dir"
        ? { kind, path }
        : kind === "lvm-thin"
          ? { kind, vg, thinpool }
          : kind === "zfs"
            ? { kind, dataset }
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
  poolUsableSomewhere,
} from "@/storage/pools";
```

을 다음으로 바꾼다:

```ts
  poolUsableSomewhere,
  zfsDatasetError,
} from "@/storage/pools";
```

`web/src/storage/pools.test.ts`에서

```ts
const lvm = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const withPool = (
```

을 다음으로 바꾼다:

```ts
const lvm = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const zfs = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const withPool = (
```

`web/src/storage/pools.test.ts`에서

```ts
  expect(poolLocation(lvm)).toBe("vg0/tp");
});
```

을 다음으로 바꾼다:

```ts
  expect(poolLocation(lvm)).toBe("vg0/tp");
  expect(poolLocation(zfs)).toBe("tank/opl");
});
```

`web/src/storage/pools.test.ts`에서

```ts

it("offers the volume formats a pool holds, the default first", () => {
```

을 다음으로 바꾼다:

```ts

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

it("offers the volume formats a pool holds, the default first", () => {
```

`web/src/storage/pools.test.ts`에서

```ts
  expect(poolFormats(lvm)).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
```

을 다음으로 바꾼다:

```ts
  expect(poolFormats(lvm)).toEqual(["raw"]);
  // ZFS 풀도 raw zvol만 둔다.
  expect(poolFormats(zfs)).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
```

`web/src/storage/pools.ts`에서

```ts
/**
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). LVM-thin 풀은 raw만 둔다. 풀을 모르면 파일 풀처럼 본다.
 */
export function poolFormats(pool: StoragePool | undefined): [ImageFormat, ...ImageFormat[]] {
  return pool?.spec.backend.kind === "lvm-thin" ? ["raw"] : ["qcow2", "raw"];
}
```

을 다음으로 바꾼다:

```ts
/**
 * ZFS 데이터셋 입력 오류(번역 키). 서버처럼 177자 이하이고 `/`로 나뉜 구성 요소가 비지 않으며,
 * 구성 요소는 `A-Za-z0-9_.:-`로만 되고 `-`로 시작하지 않으며 `.`·`..`이 아니다. 첫 구성 요소(풀
 * 이름)는 문자로 시작한다.
 */
export function zfsDatasetError(dataset: string): string | undefined {
  const ok =
    dataset.length <= 177 &&
    /^[A-Za-z]/.test(dataset) &&
    dataset
      .split("/")
      .every(
        (part) => /^[A-Za-z0-9_.:][A-Za-z0-9_.:-]*$/.test(part) && part !== "." && part !== "..",
      );
  return ok ? undefined : "pools.errors.zfsDataset";
}

/** 디스크 스냅샷을 블록 스냅샷으로 두는 풀(LVM-thin, ZFS)의 종류. 아니면(파일 풀, 모름) `undefined`. */
export function blockSnapshotKind(pool: StoragePool | undefined): "lvm-thin" | "zfs" | undefined {
  const kind = pool?.spec.backend.kind;
  return kind === "lvm-thin" || kind === "zfs" ? kind : undefined;
}

/**
 * 풀에 둘 수 있는 볼륨 형식. 첫 값이 기본값이고 스냅샷을 담는 형식이다(서버의
 * `pool_kind_supports_snapshots`). 블록 풀(LVM-thin, ZFS)은 raw만 둔다. 풀을 모르면 파일 풀처럼 본다.
 */
export function poolFormats(pool: StoragePool | undefined): [ImageFormat, ...ImageFormat[]] {
  return blockSnapshotKind(pool) ? ["raw"] : ["qcow2", "raw"];
}
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, or all in LVM-thin pools.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(every).toHaveAccessibleDescription(
    "Takes snapshots periodically and keeps only the newest scheduled ones. Snapshots you take yourself are never deleted. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
  );
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    screen.getByText(
      "web-data is raw, so scheduled snapshots will fail. All disks must be qcow2, or all in LVM-thin pools.",
    ),
```

을 다음으로 바꾼다:

```tsx
    screen.getByText(
      "web-data is raw, so scheduled snapshots will fail. All disks must be qcow2, all in LVM-thin pools, or all in ZFS pools.",
    ),
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    await screen.findByText(
      "Disks in LVM-thin pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Disks in LVM-thin and ZFS pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

`web/src/vms/VmFormPage.test.tsx`에서

```tsx
    screen.getByText(
      "Disks in LVM-thin pools and qcow2 disks are mixed, so scheduled snapshots will fail.",
    ),
```

을 다음으로 바꾼다:

```tsx
    screen.getByText(
      "Disks with different snapshot methods (qcow2, LVM-thin pools, ZFS pools) are mixed, so scheduled snapshots will fail.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. web-root is raw.",
    ),
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. web-root is raw.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    within(dialog).getByText(
      "Disks in LVM-thin pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

을 다음으로 바꾼다:

```tsx
    within(dialog).getByText(
      "Disks in LVM-thin and ZFS pools are saved without memory. A running VM pauses briefly while they are saved.",
    ),
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. This VM has both.",
    ),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
    await screen.findByText(
      "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. This VM mixes them.",
    ),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
});

it("says why a VM with disks in ZFS pools cannot revert to older snapshots", async () => {
  const tank = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
  const zvol = volume("web-root", "tank");
  const stopped = withDisk(vm("web", "stopped"));
  const first = snapshot("first", stopped, 1);
  const second = snapshot("second", stopped, 2, { spec: { parent: first.id } });
  mockLists({
    vm: [stopped],
    node: [NODE],
    volume: [{ ...zvol, spec: { ...zvol.spec, format: "raw" } }],
    "storage-pool": [tank],
    snapshot: [first, second],
  });
  let reverted = false;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots/${first.id}/revert`), () => {
      reverted = true;
      return HttpResponse.json(task("t1", "snapshot-revert", web.id), { status: 202 });
    }),
  );
  open(stopped);
  // 가장 최근 스냅샷만 되돌릴 수 있고, 막힌 버튼은 이유를 알린다.
  const older = await screen.findByRole("button", { name: "Revert to first" });
  await waitFor(() =>
    expect(older).toHaveAccessibleDescription(
      "Disks in ZFS pools can only revert to their latest snapshot.",
    ),
  );
  expect(older).toHaveAttribute("aria-disabled", "true");
  // 이유는 줄을 바꿔 표를 넓히지 않는다.
  const reason = screen.getByText("Disks in ZFS pools can only revert to their latest snapshot.");
  expect(reason.parentElement).toHaveClass("whitespace-normal", "max-w-xs");
  await userEvent.click(older);
  expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  expect(reverted).toBe(false);
  const latest = screen.getByRole("button", { name: "Revert to second" });
  expect(latest).toBeEnabled();
  expect(latest).not.toHaveAttribute("aria-disabled");
});
```

`web/src/vms/VmSnapshots.test.tsx`에서

```tsx
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2, or all in LVM-thin pools, can take snapshots. Snapshots of disks in LVM-thin pools cannot include memory.",
  ],
```

을 다음으로 바꾼다:

```tsx
    "raw disks cannot be snapshotted",
    "Only VMs whose disks are all qcow2, all in LVM-thin pools, or all in ZFS pools can take snapshots. Snapshots of disks in LVM-thin or ZFS pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
  ],
```

`web/src/vms/VmSnapshots.tsx` 전체:

```tsx
import { Camera } from "lucide-react";
import { type RefObject, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Snapshot, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage } from "@/i18n/errors";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { isBusy } from "@/vms/actions";
import {
  descriptionError,
  snapshotNameError,
  snapshotSupport,
  snapshotTree,
  volumeDisk,
  zfsNotLatest,
} from "@/vms/snapshots";

/** VM 상세의 스냅샷 탭: 나무 목록과 만들기·되돌리기·삭제 */
export function VmSnapshots({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const snapshots = useList("snapshot");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const [creating, setCreating] = useState(false);
  // 닫히는 동안에도 대상과 동작을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [confirm, setConfirm] = useState<{
    action: "revert" | "delete";
    snapshot: Snapshot;
  } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const reasonBase = useId();
  const operate = can(me.role, "operate", "vm");
  const mine = (snapshots.data ?? []).filter((s) => s.spec.vm === vm.id);
  const rows = snapshotTree(mine);
  const names = new Map(mine.map((s) => [s.id, s.spec.name]));
  // 서버는 시작·정지 중인 VM의 스냅샷 작업을 받지 않는다(vm_busy).
  const busy = isBusy(vm);
  // 풀 종류가 담지 못하는 형식의 디스크가 있거나 방식이 섞이면 만들 수 없다(snapshot_unsupported).
  // 볼륨·풀 목록을 다 읽어야 안다.
  const listed = volumes.isSuccess && pools.isSuccess;
  const support = listed
    ? snapshotSupport(
        (vm.spec.disks ?? []).flatMap((d) => {
          const volume = volumes.data.find((v) => v.name === d.volume);
          return volume ? [volumeDisk(volume, pools.data)] : [];
        }),
      )
    : undefined;
  const block = support?.kind === "ok" && support.block;
  const columns = operate ? 5 : 4;
  const schedule = vm.spec.snapshot_schedule;

  function ask(e: React.MouseEvent<HTMLElement>, action: "revert" | "delete", snapshot: Snapshot) {
    opener.current = e.currentTarget;
    setConfirm({ action, snapshot });
    setConfirmOpen(true);
  }

  return (
    <section className="flex flex-col gap-4 pt-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="vm-snapshots" className="font-medium">
          {t("vms.tab.snapshots")}
        </h2>
        {operate &&
          (support?.kind === "format" ? (
            <p className="text-sm text-muted-foreground">
              {t("snapshots.unsupported", {
                volume: support.disk.volume,
                format: support.disk.format,
              })}
            </p>
          ) : support?.kind === "mixed" ? (
            <p className="text-sm text-muted-foreground">{t("snapshots.mixed")}</p>
          ) : (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              {/* 볼륨·풀 목록을 읽어야 디스크의 스냅샷 방식을 알아 만들 수 있다. */}
              {!volumes.isSuccess && <ListPending error={volumes.error} />}
              {!pools.isSuccess && <ListPending error={pools.error} />}
              <Button
                size="sm"
                disabled={busy || !snapshots.isSuccess || !listed}
                onClick={(e) => {
                  opener.current = e.currentTarget;
                  setCreating(true);
                }}
              >
                <Camera />
                {t("snapshots.create")}
              </Button>
            </div>
          ))}
      </div>
      {operate && busy && <p className="text-sm text-muted-foreground">{t("snapshots.busy")}</p>}
      <div className="flex flex-col gap-1 text-sm">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <p>
            {/* 블록 디스크의 VM은 예약이 메모리를 원해도 메모리 없이 찍는다. */}
            {schedule
              ? t(schedule.memory && !block ? "snapshots.scheduleMemory" : "snapshots.schedule", {
                  every: t(`snapshots.every.${schedule.every}`),
                  keep: schedule.keep,
                })
              : t("snapshots.noSchedule")}
          </p>
          {/* 예약은 VM 설정이라 VM 편집에서 바꾼다. */}
          {can(me.role, "write", "vm") && (
            <Link
              to={`/vms/${encodeURIComponent(vm.name)}/edit`}
              className="underline-offset-4 hover:underline"
            >
              {t("snapshots.scheduleEdit")}
            </Link>
          )}
        </div>
        {/* 예약을 꺼도 마지막 실패는 남는다. code를 예약 전용 문구, 오류 번역 순으로 찾고, 번역이
            없으면 서버가 쓴 이유를 줄바꿈·긴 낱말까지 그대로 보인다. */}
        {vm.status.schedule_error && (
          <p className="text-destructive">
            <span className="font-medium">{t("snapshots.scheduleFailed")}</span>{" "}
            <span className="whitespace-pre-wrap break-words">
              {codeMessage(
                t,
                vm.status.schedule_error.code,
                vm.status.schedule_error.message,
                "snapshots.scheduleError",
              )}
            </span>
          </p>
        )}
      </div>
      <Table aria-labelledby="vm-snapshots">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("snapshots.created")}</TableHead>
            <TableHead>{t("snapshots.memory")}</TableHead>
            <TableHead>{t("snapshots.description")}</TableHead>
            {operate && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.length === 0 && (
            <TableMessage colSpan={columns} query={snapshots} empty={t("snapshots.empty")} />
          )}
          {rows.map(({ snapshot, depth }) => {
            const ready = snapshot.status.phase === "ready";
            const { name } = snapshot.spec;
            const parent = depth > 0 ? names.get(snapshot.spec.parent ?? "") : undefined;
            // ZFS 디스크는 가장 최근 스냅샷으로만 되돌린다(서버가 409).
            const revertReason =
              listed && zfsNotLatest(snapshot, mine, volumes.data, pools.data)
                ? "snapshots.zfsLatest"
                : null;
            const reasons = `${reasonBase}-${snapshot.id}`;
            return (
              <TableRow key={snapshot.id}>
                {/* 깊이만큼 들여 써 나무를 보인다. */}
                <TableCell
                  className="font-medium"
                  style={{ paddingLeft: `${0.5 + depth * 1.5}rem` }}
                >
                  {name}
                  {/* 들여쓰기는 보이기만 하므로 스크린 리더에는 부모를 말로 알린다. */}
                  {parent !== undefined && (
                    <span className="sr-only"> {t("snapshots.after", { parent })}</span>
                  )}
                  {/* 공백이 없으면 접근 이름이 "이름현재"로 붙는다. */}
                  {snapshot.spec.scheduled && (
                    <>
                      {" "}
                      <Badge variant="outline" className="ml-1">
                        {t("snapshots.scheduled")}
                      </Badge>
                    </>
                  )}
                  {vm.status.snapshot === snapshot.id && (
                    <>
                      {" "}
                      <Badge className="ml-1">{t("snapshots.current")}</Badge>
                    </>
                  )}
                </TableCell>
                <TableCell>
                  {!ready ? (
                    <Badge variant="secondary">{t("snapshots.pending")}</Badge>
                  ) : snapshot.status.created_at !== null ? (
                    formatTime(new Date(snapshot.status.created_at * 1000), i18n.language)
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell>
                  {!snapshot.spec.memory
                    ? t("snapshots.memoryNo")
                    : snapshot.status.vmstate_bytes !== null
                      ? t("snapshots.memorySize", {
                          size: formatBytes(snapshot.status.vmstate_bytes),
                        })
                      : t("snapshots.memoryYes")}
                </TableCell>
                <TableCell className="whitespace-normal break-words">
                  {snapshot.spec.description || "—"}
                </TableCell>
                {operate && (
                  <TableCell className="text-right whitespace-nowrap">
                    {revertReason ? (
                      <ReasonButton
                        variant="outline"
                        size="sm"
                        aria-label={t("snapshots.revertNamed", { name })}
                        reason={revertReason}
                        reasons={reasons}
                      >
                        {t("snapshots.revert")}
                      </ReasonButton>
                    ) : (
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={t("snapshots.revertNamed", { name })}
                        disabled={busy || !ready}
                        onClick={(e) => ask(e, "revert", snapshot)}
                      >
                        {t("snapshots.revert")}
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="sm"
                      className="ml-2"
                      aria-label={t("snapshots.deleteNamed", { name })}
                      disabled={busy || !ready}
                      onClick={(e) => ask(e, "delete", snapshot)}
                    >
                      {t("common.delete")}
                    </Button>
                    {/* 셀은 줄을 바꾸지 않으므로 이유는 좁게 줄을 바꿔 표를 넓히지 않는다. */}
                    <ReasonList
                      base={reasons}
                      reasons={[revertReason]}
                      className="ml-auto max-w-xs whitespace-normal"
                    />
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && (
        <CreateSnapshotDialog
          vm={vm}
          taken={mine.map((s) => s.spec.name)}
          block={block}
          returnFocus={opener}
          onClose={() => setCreating(false)}
        />
      )}
      <ConfirmDialog
        open={confirmOpen && confirm?.action === "revert"}
        onOpenChange={setConfirmOpen}
        returnFocus={opener}
        title={t("snapshots.revertTitle", { vm: vm.name, name: confirm?.snapshot.spec.name })}
        description={t(
          confirm?.snapshot.spec.memory
            ? "snapshots.revertDescriptionMemory"
            : "snapshots.revertDescription",
          { vm: vm.name },
        )}
        confirmLabel={t("snapshots.revert")}
        onConfirm={async () => {
          const { snapshot } = confirm!;
          const task = await call(
            api.POST("/vms/{key}/snapshots/{name}/revert", {
              params: { path: { key: vm.id, name: snapshot.id } },
            }),
          );
          trackTask(task, t("snapshots.op.revert", { vm: vm.name, name: snapshot.spec.name }));
        }}
      />
      <ConfirmDialog
        open={confirmOpen && confirm?.action === "delete"}
        onOpenChange={setConfirmOpen}
        returnFocus={opener}
        title={t("snapshots.deleteTitle", { name: confirm?.snapshot.spec.name })}
        description={t("snapshots.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const { snapshot } = confirm!;
          const task = await call(
            api.DELETE("/vms/{key}/snapshots/{name}", {
              params: { path: { key: vm.id, name: snapshot.id } },
            }),
          );
          trackTask(task, t("snapshots.op.delete", { vm: vm.name, name: snapshot.spec.name }));
        }}
      />
    </section>
  );
}

function CreateSnapshotDialog({
  vm,
  taken,
  block,
  returnFocus,
  onClose,
}: {
  vm: Vm;
  taken: string[];
  /** 디스크가 블록 스냅샷(LVM-thin, ZFS)을 쓴다(메모리를 담지 못한다). */
  block: boolean;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [memory, setMemory] = useState(true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 서버는 실행 중이고 디스크가 있는 VM만 메모리를 담는다. 그 밖에는 묻지 않고 디스크만 담는다.
  // 블록 스냅샷은 메모리를 담지 못한다(서버가 409).
  const canMemory = vm.status.phase === "running" && (vm.spec.disks ?? []).length > 0 && !block;

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = snapshotNameError(name, taken);
    if (nameError) {
      found.name = nameError;
    }
    const tooLong = descriptionError(description);
    if (tooLong) {
      found.description = tooLong;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/snapshots", {
        params: { path: { key: vm.id } },
        body: { name, description, memory: canMemory && memory },
      }),
    );
    trackTask(task, t("snapshots.op.create", { vm: vm.name, name }));
  }

  return (
    <FormDialog
      title={t("snapshots.createTitle", { name: vm.name })}
      description={t("snapshots.createDescription")}
      submitLabel={t("snapshots.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="snapshot-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="snapshot-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="snapshot-description"
        label={t("snapshots.description")}
        error={errors.description}
      >
        {(a11y) => (
          <Input
            id="snapshot-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {canMemory && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Checkbox
              id="snapshot-memory"
              checked={memory}
              onCheckedChange={(v) => setMemory(v === true)}
            />
            <Label htmlFor="snapshot-memory">{t("snapshots.includeMemory")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("snapshots.includeMemoryHint")}</p>
        </div>
      )}
      {block && <p className="text-xs text-muted-foreground">{t("snapshots.blockHint")}</p>}
    </FormDialog>
  );
}
```

`web/src/vms/form/ScheduleSection.tsx`에서

```tsx
  const on = values.snapshotSchedule.every !== "off";
  // LV 스냅샷은 메모리를 담지 못하므로 묻지 않는다(예약 작업이 메모리 없이 찍는다).
  const lvm = support?.kind === "ok" && support.lvm;
  return (
```

을 다음으로 바꾼다:

```tsx
  const on = values.snapshotSchedule.every !== "off";
  // 블록 스냅샷은 메모리를 담지 못하므로 묻지 않는다(예약 작업이 메모리 없이 찍는다).
  const block = support?.kind === "ok" && support.block;
  return (
```

`web/src/vms/form/ScheduleSection.tsx`에서

```tsx
            </Field>
            {lvm ? (
              <p className="text-xs text-muted-foreground sm:col-span-2">
                {t("snapshots.lvmHint")}
              </p>
```

을 다음으로 바꾼다:

```tsx
            </Field>
            {block ? (
              <p className="text-xs text-muted-foreground sm:col-span-2">
                {t("snapshots.blockHint")}
              </p>
```

`web/src/vms/snapshots.test.ts` 전체:

```ts
import type { Volume } from "@/api/types";
import { pool, snapshot, vm, volume } from "@/test/fixtures";
import { emptyDisk, emptyForm } from "@/vms/form";
import {
  descriptionError,
  formSnapshotSupport,
  snapshotNameError,
  snapshotSupport,
  snapshotTree,
  volumeDisk,
  zfsNotLatest,
} from "@/vms/snapshots";

const web = vm("web", "running");

it("orders snapshots as a tree, siblings by creation", () => {
  const base = snapshot("base", web, 1);
  const b = snapshot("b", web, 3, { spec: { parent: base.id } });
  const a = snapshot("a", web, 2, { spec: { parent: base.id } });
  const child = snapshot("child", web, 4, { spec: { parent: a.id } });
  // 목록에 없는 부모(지워지는 중 등)를 가리키면 뿌리로 보인다.
  const lost = snapshot("lost", web, 5, {
    spec: { parent: "019a0f3c-0000-7000-8003-999999999999" },
  });
  expect(
    snapshotTree([child, b, lost, a, base]).map(({ snapshot, depth }) => [
      snapshot.spec.name,
      depth,
    ]),
  ).toEqual([
    ["base", 0],
    ["a", 1],
    ["child", 2],
    ["b", 1],
    ["lost", 0],
  ]);
});

it("shows snapshots whose parents form a cycle as roots", () => {
  const a = snapshot("a", web, 1);
  const b = snapshot("b", web, 2, { spec: { parent: a.id } });
  const self = snapshot("self", web, 3);
  const cycle = [
    { ...a, spec: { ...a.spec, parent: b.id } },
    b,
    { ...self, spec: { ...self.spec, parent: self.id } },
  ];
  expect(snapshotTree(cycle).map(({ snapshot, depth }) => [snapshot.spec.name, depth])).toEqual([
    ["a", 0],
    ["b", 1],
    ["self", 0],
  ]);
});

it("checks snapshot names and descriptions like the server", () => {
  expect(snapshotNameError("before-upgrade", [])).toBeUndefined();
  expect(snapshotNameError("Before", [])).toBe("forms.name");
  expect(snapshotNameError("base", ["base"])).toBe("snapshots.errors.exists");
  expect(descriptionError("가".repeat(1024))).toBeUndefined();
  expect(descriptionError("가".repeat(1025))).toBe("snapshots.errors.description");
});

const local = pool("local");
const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const tank = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const pools = [local, fast, tank];
const raw = (name: string, poolName: string) => {
  const v = volume(name, poolName);
  return { ...v, spec: { ...v.spec, format: "raw" as const } };
};

it("decides like the server whether disks can be snapshotted", () => {
  const disk = (v: ReturnType<typeof volume>) => volumeDisk(v, pools);
  // 디스크가 없으면 스냅샷 방식을 따질 것이 없다.
  expect(snapshotSupport([])).toEqual({ kind: "ok", block: false });
  // 파일 풀은 qcow2 내부 스냅샷, LVM-thin 풀은 raw LV 스냅샷이다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(volume("b", "local"))])).toEqual({
    kind: "ok",
    block: false,
  });
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "fast"))])).toEqual({
    kind: "ok",
    block: true,
  });
  // 파일 풀의 raw 디스크(또는 LVM-thin 풀의 qcow2 디스크)는 담지 못한다.
  const plain = disk(raw("plain", "local"));
  expect(snapshotSupport([disk(volume("a", "local")), plain])).toEqual({
    kind: "format",
    disk: plain,
  });
  const odd = disk(volume("odd", "fast"));
  expect(snapshotSupport([odd])).toEqual({ kind: "format", disk: odd });
  // ZFS 풀도 raw 블록 스냅샷이다.
  expect(snapshotSupport([disk(raw("a", "tank")), disk(raw("b", "tank"))])).toEqual({
    kind: "ok",
    block: true,
  });
  const qcow = disk(volume("q", "tank"));
  expect(snapshotSupport([qcow])).toEqual({ kind: "format", disk: qcow });
  // 방식이 섞이면 안 된다. 블록 백엔드끼리도 종류가 다르면 섞인 것이다.
  expect(snapshotSupport([disk(volume("a", "local")), disk(raw("b", "fast"))])).toEqual({
    kind: "mixed",
  });
  expect(snapshotSupport([disk(raw("a", "fast")), disk(raw("b", "tank"))])).toEqual({
    kind: "mixed",
  });
  // 풀을 모르는 볼륨은 파일 풀처럼 본다.
  expect(snapshotSupport([volumeDisk(volume("a", "gone"), pools)])).toEqual({
    kind: "ok",
    block: false,
  });
});

it("judges the VM form's disks, new ones in their pool's format", () => {
  const values = {
    ...emptyForm("q35"),
    disks: [
      { ...emptyDisk("web", 0), kind: "existing" as const, volume: "data" },
      { ...emptyDisk("web", 1), pool: "fast" },
    ],
  };
  // 새 디스크는 풀의 기본 형식(LVM-thin 풀은 raw)으로 만든다.
  expect(formSnapshotSupport(values, [raw("data", "fast")], pools)).toEqual({
    kind: "ok",
    block: true,
  });
  expect(formSnapshotSupport(values, [volume("data", "local")], pools)).toEqual({
    kind: "mixed",
  });
  // 목록을 다 읽기 전에는 모른다.
  expect(formSnapshotSupport(values, undefined, pools)).toBeUndefined();
  expect(formSnapshotSupport(values, [], undefined)).toBeUndefined();
  // 아직 고르지 않은 볼륨·풀은 따지지 않는다.
  const blank = { ...values, disks: [{ ...emptyDisk("web", 0), pool: "" }] };
  expect(formSnapshotSupport(blank, [], pools)).toEqual({ kind: "ok", block: false });
});

it("finds snapshots a VM with disks in ZFS pools cannot revert to, like the server", () => {
  const base = vm("z", "stopped");
  const zvm = {
    ...base,
    spec: {
      ...base.spec,
      disks: [{ volume: "z-root", bus: "virtio-blk" as const, boot_order: 1 }],
    },
  };
  const zvols = [raw("z-root", "tank")];
  const old = snapshot("old", zvm, 1);
  const mid = snapshot("mid", zvm, 2, { status: { created_at: 1_792_000_100 } });
  // 만든 시각이 같으면 id(만든 차례)로 가린다.
  const tie = snapshot("tie", zvm, 3, { status: { created_at: 1_792_000_100 } });
  const all = [tie, old, mid];
  const notLatest = (s: ReturnType<typeof snapshot>, list = all, vols: Volume[] = zvols) =>
    zfsNotLatest(s, list, vols, pools);
  expect(notLatest(old)).toBe(true);
  expect(notLatest(mid)).toBe(true);
  expect(notLatest(tie)).toBe(false);
  // 만드는 중인 스냅샷은 가장 나중이다.
  const pending = snapshot("pending", zvm, 0, {
    status: { phase: "pending", created_at: null },
  });
  expect(notLatest(tie, [...all, pending])).toBe(true);
  // 스냅샷이 찍은 디스크가 ZFS 풀에 없으면 막지 않는다(지금 VM의 디스크가 아니다).
  expect(notLatest(old, all, [volume("z-root", "local")])).toBe(false);
  expect(notLatest(old, all, [raw("z-root", "fast")])).toBe(false);
  // 목록에 없는 볼륨은 건너뛴다.
  expect(notLatest(old, all, [])).toBe(false);
});
```

`web/src/vms/snapshots.ts`에서

```ts
import type { Snapshot, StoragePool, Volume } from "@/api/types";
import { poolFormats } from "@/storage/pools";
import { validName, type VmFormValues } from "@/vms/form";
```

을 다음으로 바꾼다:

```ts
import type { Snapshot, StoragePool, Volume } from "@/api/types";
import { blockSnapshotKind, poolFormats } from "@/storage/pools";
import { validName, type VmFormValues } from "@/vms/form";
```

`web/src/vms/snapshots.ts`에서

```ts
/**
 * 디스크들의 스냅샷 가능 여부. `ok`면 `lvm`이 LV 스냅샷(메모리 없음)인지, `format`은 풀 종류가
 * 담지 못하는 형식의 디스크, `mixed`는 qcow2 내부 스냅샷과 LV 스냅샷이 섞인 것이다.
 */
export type SnapshotSupport =
  { kind: "ok"; lvm: boolean } | { kind: "format"; disk: SnapshotDisk } | { kind: "mixed" };
```

을 다음으로 바꾼다:

```ts
/**
 * 디스크들의 스냅샷 가능 여부. `ok`면 `block`이 블록 스냅샷(LVM-thin·ZFS, 메모리 없음)인지,
 * `format`은 풀 종류가 담지 못하는 형식의 디스크, `mixed`는 방식(qcow2 내부 스냅샷, 블록 백엔드
 * 종류마다의 블록 스냅샷)이 섞인 것이다.
 */
export type SnapshotSupport =
  { kind: "ok"; block: boolean } | { kind: "format"; disk: SnapshotDisk } | { kind: "mixed" };
```

`web/src/vms/snapshots.ts`에서

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, LVM-thin 풀의 디스크는 raw여야 하고,
 * 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
 * (서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
export function snapshotSupport(disks: readonly SnapshotDisk[]): SnapshotSupport {
  let lvm: boolean | undefined;
  for (const disk of disks) {
    const lv = disk.pool?.spec.backend.kind === "lvm-thin";
    if (disk.format !== poolFormats(disk.pool)[0]) {
      return { kind: "format", disk };
    }
    if (lvm !== undefined && lvm !== lv) {
      return { kind: "mixed" };
    }
    lvm = lv;
  }
  return { kind: "ok", lvm: lvm === true };
}
```

을 다음으로 바꾼다:

```ts
/**
 * 서버의 `check_disks`처럼 판단한다: 파일 풀의 디스크는 qcow2, 블록 풀(LVM-thin, ZFS)의 디스크는
 * raw여야 하고, 모든 디스크가 같은 방식이어야 한다. 서버와 다른 점: 호출자는 목록에 없는 볼륨을 넘기지 않으므로
 * (서버는 400) 그런 디스크는 건너뛴 셈이고, 풀을 모르는 디스크는 파일 풀처럼 본다(서버는 400).
 */
export function snapshotSupport(disks: readonly SnapshotDisk[]): SnapshotSupport {
  // 디스크마다의 방식: 블록 백엔드 종류, qcow2 내부 스냅샷이면 `null`. 아직 디스크가 없으면 `undefined`.
  let method: string | null | undefined;
  for (const disk of disks) {
    const kind = blockSnapshotKind(disk.pool) ?? null;
    if (disk.format !== poolFormats(disk.pool)[0]) {
      return { kind: "format", disk };
    }
    if (method !== undefined && method !== kind) {
      return { kind: "mixed" };
    }
    method = kind;
  }
  return { kind: "ok", block: typeof method === "string" };
}

/**
 * ZFS 디스크라 되돌릴 수 없는 스냅샷인지. 서버의 `check_zfs_latest`처럼 스냅샷이 찍은 디스크
 * (`vm_spec`)에 ZFS 풀의 볼륨이 있고, `snapshots`(같은 VM의 스냅샷) 가운데 더 나중 것이 있으면 막힌다.
 * 순서는 만든 시각, 같으면 id(UUIDv7)이고 만드는 중인 스냅샷은 가장 나중이다. 목록에 없는 볼륨·풀은
 * 건너뛴다.
 */
export function zfsNotLatest(
  snapshot: Snapshot,
  snapshots: readonly Snapshot[],
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): boolean {
  const zfs = (snapshot.spec.vm_spec.disks ?? []).some((d) => {
    const volume = volumes.find((v) => v.name === d.volume);
    const pool = volume && pools.find((p) => p.name === volume.spec.pool);
    return pool?.spec.backend.kind === "zfs";
  });
  const newer = (a: Snapshot, b: Snapshot) => {
    const at = (s: Snapshot) => s.status.created_at ?? Infinity;
    return at(a) !== at(b) ? at(a) > at(b) : a.id > b.id;
  };
  return zfs && snapshots.some((s) => newer(s, snapshot));
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 286개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): ZFS 풀 만들기와 블록 스냅샷 판단, ZFS 디스크의 최신 스냅샷만 되돌리기"
```

---

### Task 9: 문서

README에 "ZFS 풀"(준비, LVM-thin과 다른 점, 최신 스냅샷만 되돌리기와 자동 스냅샷 도구 주의, 호스트 LVM 필터, `zfs-it` 실행)과 문서 목록, 설계 문서 §2–§8에 "구현:" 메모.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md` 전체:

````markdown
# opLite

Rust로 작성하는 온프레미스 가상화·클라우드 플랫폼 (Proxmox VE / VMware vSphere 대안).

- 아키텍처: 대칭형 노드 + 내장 Raft, QEMU/KVM을 QMP로 직접 제어
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS 풀)

## 실행 (macOS, Apple Silicon)

```bash
brew install qemu
mkdir -p /tmp/opl
cat > /tmp/opl/node1.toml <<'TOML'
name = "node1"
address = "127.0.0.1"          # 다른 노드와 클라이언트가 이 노드에 닿는 호스트
data_dir = "/tmp/opl/node1"
run_dir = "/tmp/opl/run1"
TOML
cargo run -p opld -- --config /tmp/opl/node1.toml &       # 제어 소켓: /tmp/opl/run1/opld.sock
cargo run -p oplctl -- cluster init --name dev --socket /tmp/opl/run1/opld.sock
# 출력된 admin 비밀번호와 CA 지문으로 로그인한다(API: https://127.0.0.1:8006).
cargo run -p oplctl -- login --server https://127.0.0.1:8006 --ca-fingerprint sha256:...
mkdir -p /tmp/opl/pool
cargo run -p oplctl -- pool create local --path /tmp/opl/pool
cargo run -p oplctl -- volume create root --pool local --size 1G
cargo run -p oplctl -- vm create web --memory 512 --disk root --nic user --start
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
cargo run -p oplctl -- vm snapshot create web before --description "업데이트 전"   # 실행 중이면 메모리 포함(--no-memory: 디스크만)
cargo run -p oplctl -- vm snapshot list web      # 나무 모양, 현재 위치 *
cargo run -p oplctl -- vm clone web web-copy --snapshot before   # 스냅샷에서 새 VM(꺼진 VM은 --snapshot 없이 지금 상태)
cargo run -p oplctl -- vm snapshot revert web before
cargo run -p oplctl -- vm snapshot delete web before
cargo run -p oplctl -- vm snapshot schedule web --every daily --keep 7   # 예약 스냅샷(--memory, 끄기 --off)
cargo run -p oplctl -- vm snapshot schedule web  # 지금 예약과 마지막 예약 실패
cargo run -p oplctl -- volume resize root --size 2G   # 늘리기만, 실행 중에도 된다
```

같은 Mac에 노드를 더하려면 포트와 디렉터리를 바꿔 opld를 하나 더 띄우고, 조인 URL로 합류한다.

```bash
cat > /tmp/opl/node2.toml <<'TOML'
name = "node2"
address = "127.0.0.1"
listen = "127.0.0.1:8016"
cluster_listen = "127.0.0.1:8017"
data_dir = "/tmp/opl/node2"
run_dir = "/tmp/opl/run2"
TOML
cargo run -p opld -- --config /tmp/opl/node2.toml &
cargo run -p oplctl -- cluster join "$(cargo run -q -p oplctl -- cluster join-token)" \
    --socket /tmp/opl/run2/opld.sock
cargo run -p oplctl -- cluster status
```

API 문서(OpenAPI)는 `https://127.0.0.1:8006/api/v1/openapi.json`에 있다.

## 실행 (Linux, KVM)

opld는 root로 돈다(VM NIC의 tap 장치를 만든다). QEMU는 VM마다 `opl-vm-<id>.scope`로 감싼다.
KVM(`/dev/kvm`)이 없으면 TCG로 돌고 경고를 남긴다.

```bash
sudo apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils iproute2
# 브리지는 관리자가 미리 만든다. VLAN 태그를 쓰려면 VLAN-aware여야 한다.
sudo ip link add vmbr0 type bridge vlan_filtering 1
sudo ip link set eth1 master vmbr0 && sudo ip link set vmbr0 up
sudo mkdir -p /etc/opl
sudo tee /etc/opl/opld.toml >/dev/null <<'TOML'
name = "node1"
address = "10.0.0.11"
# [hypervisor]
# accel = "auto"          # auto | kvm | tcg
# systemd_scope = true
# [janitor]
# task_max_age_days = 7     # 끝난 작업(Task)을 남기는 날수(만든 때부터)
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [pools]
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
TOML
sudo opld --config /etc/opl/opld.toml &
sudo oplctl cluster init --name prod
oplctl login --server https://10.0.0.11:8006 --ca-fingerprint sha256:...
oplctl network create lan --bridge vmbr0 --vlan 10   # VLAN 10을 태그 없이 주고받는 NIC
# 풀과 볼륨은 macOS와 같다(/var/lib/opl/pool 등).
oplctl vm create web --memory 1024 --disk root --nic lan --start
```

브리지를 쓰는 VM은 그 브리지가 있는 노드에만 배치된다(`--node`로 노드를 직접 정하면 확인하지 않는다).
노드는 브리지 목록을 시작할 때와 1분마다 알린다.

## 스토리지 풀과 NFS

파일 풀은 두 종류다. **디렉터리 풀**(`--path`)은 노드의 디렉터리이고, **NFS 풀**(`--nfs`)은 NFS 서버의 내보내기를
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다. 볼륨이 블록 장치인
**LVM-thin 풀**(`--lvm-thin`)과 **ZFS 풀**(`--zfs`)은 [아래](#lvm-thin-풀)에 있다.

```bash
sudo apt install nfs-common                          # 노드마다 NFS 클라이언트(mount.nfs)
oplctl pool create nas --nfs nas.lan:/srv/vm         # IPv6 서버는 --nfs '[fd00::5]:/srv/vm'
oplctl pool create nas2 --nfs 10.0.0.5:/srv/vm --nfs-options vers=4.2,hard   # mount -o nosuid,nodev,noexec,retry=0 뒤에 붙는다
oplctl pool create shared --path /mnt/vm --shared    # opLite 밖에서 마운트한 공유 디렉터리(모든 노드가 같은 파일을 본다)
oplctl pool list                                     # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
oplctl pool get nas                                  # 노드별 준비·용량·오류(-o json: 풀 리소스 그대로)
                                                     # 꺼진 노드는 node offline, 목록의 없는 노드는 node not found
```

- **NFS는 Linux 노드에서 root인 opld만** 마운트한다. 내보내기는 opld(root)가 쓸 수 있어야 한다(예: `no_root_squash`).
  macOS 노드는 NFS 풀을 마운트하지 않고 "nfs pools are not supported on macOS"로 알린다. 옵션을 주지 않으면 커널
  기본값으로 마운트한다.
- **마운트 옵션.** opld는 늘 `nosuid,nodev,noexec,retry=0`을 붙이고 `--nfs-options`를 그 뒤에 붙인다(`retry=0`은
  시간 제한으로 `mount`를 끝낸 뒤 mount.nfs가 계속 다시 시도하지 않게 한다. 다시 시도는 opld가 한다). 마운트의 성격을 바꾸는
  옵션(`bind`, `rbind`, `remount`, `move`, `suid`, `dev`, `exec`, `helper`, `bg`, `x-*`·`X-mount.*`)은 **400**으로 받지 않는다.
- **마운트 위치.** `{data_dir}/pools/{풀 id}`. opld가 내려가도 마운트는 그대로 두고, 다시 올라오면 그대로 쓴다.
  풀을 지우면(볼륨이 남아 있으면 지금처럼 409) 각 노드가 언마운트하고 마운트 지점을 지운다. 그 풀의 디스크를 쓰는
  VM이 그 노드에서 돌고 있으면 VM이 멈출 때까지 미룬다(`umount -f`는 쓰지 않는다). 풀을 쓸 노드는 만들 때 `--node`로
  정한다(생략하면 모든 노드).
- **주의: `{data_dir}`를 통째로 지우지 않는다.** 마운트된 NFS 풀이 있는 채로 `rm -rf /var/lib/opl`을 하면 마운트를
  따라 들어가 NFS 내보내기의 볼륨 파일까지 지운다. 노드를 클러스터에서 빼거나 opLite를 지워도 `{data_dir}/pools` 아래의
  NFS 마운트는 그대로 남는다. 먼저 opld를 멈추고 `{data_dir}/pools` 아래를 `umount`한 뒤 지운다.
- **NFS 풀은 늘 공유 풀이다.** API에서 `shared`를 적지 않으면 `true`가 되고, `false`면 **400** `invalid_request`다.
  opld 밖에서(fstab 등) 마운트한 공유 디렉터리는 디렉터리 풀을 공유로 만든다: `oplctl pool create --path ... --shared`
  (API `"shared": true`, 웹은 "공유 스토리지" 체크). `--shared`는 `--nfs`와 함께 쓰지 않는다.
- **노드별 풀 상태.** 노드는 자기가 쓸 수 있는 풀마다 준비 여부·용량·오류를 노드 상태(`status.pools`)에 알린다.
  15초마다(`[pools] refresh_secs`), 그리고 풀을 만들거나 지우면 곧바로 확인한다. 디렉터리 풀은 디렉터리가 있고
  쓸 수 있어야(작은 파일을 만들고 지워 본다) 준비된다. NFS 풀은 마운트(30초 제한)와 쓰기 확인(10초 제한)이 되어야
  준비되고, 실패하면 `mount`의 오류나 "nfs server not responding"을 남긴다. 마운트는 풀마다 따로 돌아(진행 중이면
  "mount in progress") 멈춘 NFS 서버 하나가 다른 풀의 상태를 늦추지 않고, 실패한 마운트는 15초부터 두 배씩(최대 5분)
  기다렸다가 다시 시도한다. 상태에는 어느 풀의 것인지(`pool_id`)가 함께 적혀, 같은 이름으로 다시 만든 풀에 옛 상태를
  쓰지 않는다. opld를 다시 시작해도 적힌 풀 상태는 지우지 않는다. 웹의 스토리지 풀 화면과 노드 목록에서도 보인다
  (꺼진 노드는 "오프라인", 용량은 온라인이고 준비된 노드의 값).
- **배치 규칙.** 자동 배치는 VM의 디스크 풀을 쓸 수 있는 노드만 고른다. NFS 풀은 준비됐다고 알린 노드여야 하고,
  디렉터리 풀은 준비되지 않았다고 알린 노드만 빠진다(상태를 아직 알리지 않은 노드는 쓸 수 있다고 본다. 화면에는
  그 노드가 "아직 알린 상태 없음"으로 보인다). 공유가
  아닌 풀의 볼륨은 그 파일이 있는 노드(볼륨을 만든 노드)에만 배치된다. 전에는 노드 목록이
  빈 디렉터리 풀이면 파일이 없는 노드를 고를 수 있었다. 공유 풀은 준비된 어느 노드든 된다.
- **볼륨 작업 노드.** 볼륨 만들기는 접속한 노드에서 풀이 준비됐으면 그 노드, 아니면 풀이 준비된 온라인 노드 가운데
  하나에서 한다. 그런 노드가 없으면 **409** `pool_unavailable`이다. 공유가 아닌 풀의 볼륨 지우기·크기 늘리기는
  볼륨이 있는 노드에서 한다. 그 노드가 클러스터에서 빠졌으면 접속한 노드에서 한다(파일이 없으니 지우기는 리소스만
  지운다). 배치된 적 없는 VM의 스냅샷 작업은 첫 디스크가 공유 풀이면 볼륨을 만든 노드가 아니라 풀이 준비된 노드에서 한다. 이미지 가져오기와 복제는 풀이 그 노드의 목록에 없으면 지금처럼 `pool_not_on_node`,
  목록에는 있지만 준비되지 않았으면 `pool_unavailable`(노드가 알린 오류)이다. 웹의 업로드·복제·볼륨 만들기
  대화상자는 그 노드에서(볼륨 만들기는 어느 온라인 노드에서든) 쓸 수 있는 풀만 보인다.
- **VM 시작.** 노드가 준비되지 않았다고 알린 풀이나 이 노드에 실제로 마운트되지 않은 NFS 풀의 디스크가 있으면
  VM은 꺼지지 않고 `starting`에 머물며(`last_error`에 이유) 잠시 뒤 다시 시도한다. 일시 오류라 실패 횟수로 세지
  않으므로 NFS 서버가 돌아오면 시작한다. 드라이버도 마운트되지 않은 NFS 풀(빈 마운트 지점)에는 볼륨을 만들거나
  읽지 않고, 풀의 파일시스템 호출이 30초 안에 끝나지 않으면(멈춘 hard 마운트) 기다리지 않고 일시 오류로 돌려준다.
  NFS를 지원하지 않는 노드(macOS)에 놓인 NFS 풀 디스크의 VM은 기다려도 시작할 수 없으므로 꺼지고 `last_error`에 이유가 남는다.
- **올릴 때.** 새 opld는 첫 확인(시작 직후)에서 풀 상태를 채운다. 디렉터리 풀은 그 전에도 쓸 수 있다고 보지만,
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다. 공유가 아닌 풀의 VM은 이제 볼륨이 있는
  노드에만 배치된다. 공유 스토리지 위의 디렉터리 풀은 `--shared`로 만들어야 VM이 다른 노드로 갈 수 있다. 풀을 바꾸는
  API는 없으므로 기존 풀을 공유로 바꾸려면 볼륨을 다른 풀로 옮기고(예: 복제) 풀을 다시 만든다.

## LVM-thin 풀

**LVM-thin 풀**(`--lvm-thin VG/THINPOOL`)은 노드의 LVM thin pool이다. 볼륨마다 thin LV `opl-{볼륨 id}`를 만들고
VM에는 블록 장치(`/dev/{VG}/opl-{볼륨 id}`, QEMU `host_device`)로 붙인다.

```bash
# VG와 thin pool은 관리자가 노드마다 같은 이름으로 미리 만든다(opLite는 만들지 않는다).
sudo apt install lvm2 thin-provisioning-tools
sudo vgcreate vg0 /dev/sdb
sudo lvcreate -T -L 500G vg0/tp
oplctl pool create fast --lvm-thin vg0/tp            # --node로 쓸 노드를 정한다(생략하면 모든 노드)
oplctl volume create db --pool fast --size 20G       # --format을 생략하면 raw(디렉터리·NFS 풀은 qcow2)
oplctl pool get fast                                 # 노드별 준비·thin pool 크기·남은 공간·오류
```

- **노드 로컬.** LV는 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수 없고 API의
  `"shared": true`는 **400** `invalid_request`다. VG·thin pool 이름은 `A-Za-z0-9+_.-`의 1–127자이고 `-`로 시작하지 않는다. LVM이 받지 않는 이름(`.`·`..`,
  `snapshot`·`pvmove`로 시작, `_tdata`·`_tmeta` 같은 LVM 내부 이름 조각을 담은 것)도 안 된다.
- **raw만.** LVM-thin 풀의 볼륨은 raw다. qcow2 볼륨을 만들거나 가져오면 **400** `invalid_request`("lvm-thin pools
  hold raw volumes"). `oplctl volume create`/`import`와 웹은 풀 종류로 형식을 정한다. API의 가져오기(`/volumes/import`,
  `/volumes/import/check`)도 `format`을 생략하면 풀 종류로 정한다(LVM-thin은 raw, 그 밖은 qcow2). 가져온 이미지는
  raw로 바뀐다.
- **업그레이드 먼저.** LVM-thin(또는 NFS) 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다.
  예전 opld는 모르는 종류의 풀을 읽지 못해 풀 목록부터 실패한다.
- **노드별 상태.** opld는 15초마다(`[pools] refresh_secs`) `lvs`로 thin pool을 확인해(10초 제한) 크기와 사용률로 용량을 알린다. VG나
  thin pool이 없거나 thin pool이 아니면 준비되지 않은 것이고(`"vg0/tp is not a thin pool"`, `lvs`의 오류,
  `"lvs timed out"`), NFS 풀처럼 준비됐다고 알린 노드에만 VM과 볼륨이 간다. LVM 명령은 opld(root)가 부르고 하나에
  60초를 넘으면 실패로 본다. 이때 `lvs` 말고 LV를 바꾸는 명령(`lvcreate`·`lvremove` 등)은 중간에 죽이면 LVM
  메타데이터가 어긋날 수 있으므로 죽이지 않고 뒤에서 끝나게 둔다. 그동안 같은 LV를 바꾸는 명령은 띄우지 않고
  잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "lvm-thin pools are not supported on macOS"로 알리고, 그 노드에 놓인 LVM 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** 비활성인 LV(노드를 다시 켠 뒤 등)는 VM을 시작할 때 opld가 활성화한다(`lvchange -ay -K`). 장치는
  `/dev/{VG}/{LV}`, 그 링크가 없으면 `/dev/mapper`의 노드를 쓴다. 활성화나 LVM 명령이 실패해도 VM을 끄지 않고
  잠시 뒤 다시 시도한다(볼륨이 없거나 정의가 틀렸을 때만 이유와 함께 끈다).
- **스냅샷.** VM 스냅샷은 thin 스냅샷 LV(`opl-{볼륨 id}-s-{스냅샷 id}`)다. VM의 디스크가 모두 qcow2(디렉터리·NFS
  풀의 내부 스냅샷)이거나 모두 LVM-thin이어야 하고, 섞이면 **409** `snapshot_unsupported`("disks use different
  snapshot methods")다. LVM-thin 디스크의 VM은 **메모리 스냅샷이 없다**: `memory`를 생략하면 디스크만 찍고, `true`면
  **409** `snapshot_unsupported`다. 예약 스냅샷은 `memory: true`여도 이런 VM은 디스크만 찍는다(웹은 메모리 선택을
  숨긴다).
- **실행 중 스냅샷의 짧은 멈춤.** 실행 중인 VM은 QMP `stop`으로 잠깐 멈춰 블록 I/O를 비운 뒤 디스크마다 `lvcreate`
  하고 `cont`로 다시 돌린다(디스크 수만큼 걸린다. 멈춘 시간은 opld 로그의 `paused_ms`). 하나라도 실패하면 먼저
  VM을 다시 돌리고 만든 스냅샷 LV를 지운다. 디스크 찍기가 시간을 넘겼으면(120초 한도나 LVM 명령의 60초) 찍기가
  끝난 디스크의 스냅샷 LV만 지우고, 아직 만들어지고 있을 수 있는 나머지는 남긴다(Task 오류에 태그가 나오고,
  다시 돌리지 못했으면 "; resuming the VM failed: …"가 붙는다). 남긴 것은 opld가 60초 뒤부터 60초 간격으로 열
  번까지 다시 지워 보고, 그래도 남은 것(스냅샷이 없는 opLite 태그)은 opld가 다시 시작할 때 지운다. 멈춘 채 두지 않는다: 디스크 찍기는 120초를 넘기면 실패로 끝내고,
  `cont`는 일시 오류면 1초 간격으로 세 번까지 해 보며(VM이 꺼졌으면 할 것이 없다), Task가 시간을 넘겨 버려져도
  뒤에서 다시 돌린다. 디스크는 찍었는데 다시 돌리지 못하면 스냅샷은 그대로 두고 Task만 실패로 끝낸다("snapshot
  taken; resuming the VM failed: …"). opld가 멈춘 채 죽었거나 다시 돌리지 못한 VM은 VM 실행 디렉터리의
  표시(`paused-by-opl`)를 보고 다시 붙을 때 이어서 실행한다(디스크를 다시 해석하지 않으므로 LVM이 응답하지 않아도
  된다. 실행 중인 VM에 남은 표시는 상태를 볼 때 지운다).
  하이퍼바이저가 실행 중이라고 알리는 VM만 멈춘다: 일시정지됐거나 QEMU가 스스로 멈춘 VM(`io-error`,
  `guest-panicked` 등)은 멈추지도 다시 돌리지도 않고 디스크만 찍고, QMP로 다룰 수 없는 VM은 디스크를 건드리기
  전에 실패한다.
- **되돌리기·지우기·복제·크기.** 되돌리기는 지금처럼 VM을 강제로 끈 뒤 스냅샷 LV에서 볼륨 LV를 새로 만든다(앞서
  실패한 되돌리기가 남긴 `opl-{볼륨 id}-revert-…` LV는 먼저 지운다. 한 단계가 시간을 넘기면 `lvs`로 다시 보고
  끝났으면 이어서 한다. 새 볼륨 LV는 활성화 건너뛰기 표시를 지운다(`lvchange -kn`)). 시간을 넘긴 이름 바꾸기가
  되돌리기가 실패한 뒤에 끝나 볼륨 LV가 `-revert-` 이름으로만 남으면, opld가 그 LV를 쓸 때(VM 시작, 되돌리기,
  스냅샷, 지우기 등) 이름을 되돌린다. 그 LV의 명령이 아직 돌고 있으면 잠시 뒤 다시 시도하고(VM을 끄지 않는다),
  `-revert-` LV가 여럿이면 어느 것인지 모르므로 손대지 않고 오류로 알린다(관리자가 맞는 것을 `opl-{볼륨 id}`로
  `lvrename`한다). 스냅샷 지우기는 VM이 돌고 있어도 된다. 볼륨을
  지우면 볼륨 LV를 먼저 지우고(쓰고 있으면 실패하고 스냅샷은 남는다) 그 스냅샷 LV와 남은 `-revert-` LV도 함께
  지운다(볼륨 LV가 `-revert-` 이름으로만 남았으면 위처럼 이름을 되돌린 뒤 볼륨으로 지운다). 스냅샷에서 복제하면 대상 풀이 LVM-thin이면 raw, 디렉터리·NFS 풀이면 스냅샷을 담을 수 있게 qcow2로
  만들고, 대상은 스냅샷 때의 크기로 만든다. qcow2 디스크의 스냅샷에서 복제하면 `qemu-img`가 지금 볼륨 크기만큼만
  읽는다(볼륨을 늘린 뒤 찍은 스냅샷을, 그보다 작던 때의 스냅샷으로 되돌린 볼륨에서 복제하면 늘린 부분이 빠진다). 새 LV에 써 넣을 때 thin pool이 새 블록을 0으로 채우면
  (`lv_attr`의 `z`, 기본값) 0인 부분은 쓰지 않고, `lvcreate -Zn`으로 만든 thin pool이면 모두 쓴다. 크기 늘리기는
  실행 중이어도 되고 `lvextend` 뒤 QEMU에 LV의 실제 크기(extent 단위로 올라간다)를 알린다(LV가 이미 그 크기
  이상이면 `lvextend`는 건너뛴다). 기록되는 볼륨 크기도 그 크기다.
- **thin pool이 가득 차면** 게스트의 쓰기가 실패한다. opLite는 thin pool을 늘리지 않으므로 노드별 남은 공간을 보고
  관리자가 늘린다(LVM의 `thin_pool_autoextend_threshold`도 쓸 수 있다).
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 볼륨 LV에서 게스트의
  PV·VG를 찾아 활성화할 수 있다. lvm2 2.03의 기본값 `scan_lvs = 0`이면 LV를 보지 않으므로 그대로 두고, 바꿨다면
  `/etc/lvm/lvm.conf`의 `global_filter`로 `/dev/{VG}/opl-*`를 뺀다([ZFS 풀](#zfs-풀)의 zvol도 같다).

## ZFS 풀

**ZFS 풀**(`--zfs POOL/DATASET`)은 노드의 ZFS 데이터셋이다. 볼륨마다 그 바로 아래에 sparse zvol
`opl-{볼륨 id}`를 만들고(`volmode=dev`, `snapdev=visible`, `com.sun:auto-snapshot=false`) VM에는 블록 장치(`/dev/zvol/{DATASET}/opl-{볼륨 id}`,
QEMU `host_device`)로 붙인다. 블록 장치라는 점, 스냅샷 규칙, 실행 중 스냅샷의 짧은 멈춤은 [LVM-thin 풀](#lvm-thin-풀)과
같다. 다른 점만 적는다.

```bash
# zpool과 데이터셋은 관리자가 노드마다 같은 이름으로 미리 만든다(opLite는 만들지 않는다).
sudo apt install zfsutils-linux
sudo zpool create tank /dev/sdb
sudo zfs create tank/opl
oplctl pool create z --zfs tank/opl                  # --node로 쓸 노드를 정한다(생략하면 모든 노드)
oplctl volume create db --pool z --size 20G          # --format을 생략하면 raw
oplctl pool get z                                    # 노드별 준비·데이터셋 크기·남은 공간·오류
```

- **노드 로컬, raw만.** zvol은 그 노드에만 있으므로 VM은 볼륨이 있는 노드에서만 돈다. `--shared`와 함께 쓸 수
  없고 API의 `"shared": true`는 **400** `invalid_request`다. 볼륨은 raw이고 qcow2 볼륨을 만들거나 가져오면 **400**
  ("zfs pools hold raw volumes"). `oplctl`·웹·API 가져오기는 형식을 생략하면 raw로 정한다. 데이터셋 이름은 `/`로
  나뉜 구성 요소가 `A-Za-z0-9_.:-`로 되고(비거나 `.`·`..`이거나 `-`로 시작하면 안 된다) 맨 앞(풀 이름)이 영문자로
  시작하는 177자 이하다(zvol 스냅샷 이름 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`가 ZFS 이름 상한 255자 안에 들게).
  볼륨 크기는 1 MiB 배수로 올린다.
- **업그레이드 먼저.** ZFS 풀을 만들기 전에 클러스터의 모든 노드를 이 기능이 있는 버전으로 올린다(예전 opld는
  모르는 종류의 풀을 읽지 못한다).
- **노드별 상태.** opld는 15초마다 `zfs get -Hp -o value available,used,type`으로 데이터셋을 확인해(10초 제한)
  `available`+`used`를 크기로, `available`을 남은 공간으로 알린다. 데이터셋이 없으면(`zfs`의 오류), zvol이면
  (`"tank/opl is a volume, not a filesystem dataset"`), 시간을 넘기면(`"zfs timed out"`) 준비되지 않은 것이고,
  준비됐다고 알린 노드에만 VM과 볼륨이 간다. ZFS 명령은 opld(root)가 `LC_ALL=C`로 부르고 하나에 60초를 넘으면
  실패로 본다. LVM처럼 읽기(`zfs list`·`zfs get`) 말고 바꾸는 명령은 죽이지 않고 뒤에서 끝나게 두며, 그동안 같은
  zvol을 바꾸는 명령은 띄우지 않고 잠시 뒤 다시 시도할 오류로 끝낸다.
- **macOS 미지원.** macOS 노드는 "zfs pools are not supported on macOS"로 알리고, 그 노드에 놓인 ZFS 디스크의
  VM은 이유와 함께 꺼진다.
- **VM 시작.** zvol은 늘 활성이므로 활성화할 것이 없다. 장치 노드가 아직 없으면(udev) VM을 끄지 않고 잠시 뒤 다시
  시도한다. 새 zvol은 만든 뒤 udev가 장치를 다 다루기를 기다리고(`udevadm settle`, 10초. udev가 없거나 시간을
  넘기면 기록만 하고 장치 노드가 잠시 사라지지 않는지 두 번 본다) 장치 노드를 10초까지 기다리며, 생기지 않으면
  지우고 실패한다. 스냅샷을 찍은 뒤와 되돌린 뒤에도 udev를 기다린다.
- **스냅샷.** VM 스냅샷은 ZFS 스냅샷 `{DATASET}/opl-{볼륨 id}@{스냅샷 id}`다. VM의 디스크는 모두 qcow2이거나 모두
  LVM-thin이거나 모두 ZFS여야 한다(LVM-thin과 ZFS가 섞여도 **409** "disks use different snapshot methods").
  메모리 스냅샷은 없다(`memory: true`면 **409**, 예약은 디스크만). 실행 중인 VM은 LVM-thin처럼 잠깐 멈추고, 같은
  풀의 디스크를 `zfs snapshot` 한 명령으로 함께(원자적으로) 찍는다. 스냅샷 지우기는 VM이 돌고 있어도 된다.
  찍기가 시간을 넘겨 남긴 스냅샷은 LVM-thin처럼 뒤에서 다시 지우고, 남은 것은 opld가 다시 시작할 때 지운다.
  Task 제한 시간에 걸리거나 opld가 다시 시작해 중단된 스냅샷 Task도 같다. 뒤에서 지울 때 그 zvol에 시간을 넘긴
  명령이 아직 돌고 있으면 스냅샷이 곧 생길 수 있으므로 지운 것으로 보지 않고 다음 차례에 다시 해 본다.
- **가장 최근 스냅샷으로만 되돌린다.** `zfs rollback`은 더 나중의 스냅샷을 지워야 하므로 opLite는 그렇게 하지
  않는다. ZFS 디스크가 있는 스냅샷이 VM의 가장 최근 스냅샷(만든 시각, 같으면 id 순, 만드는 중인 것 포함)이
  아니면 **409** `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에
  거절한다. Task도 VM을 끄기 전에 디스크마다 가장 최근 ZFS 스냅샷이 그것인지 다시 보고, 아니면 VM을 그대로 두고
  더 최근 스냅샷의 이름(opLite 밖에서 만든 것이면 그 ZFS 이름)과 함께 실패한다. 드라이버도 한 번 더 본다. 웹은
  그런 스냅샷의 되돌리기 버튼을 이유와 함께 막는다. 더 오래된 스냅샷으로 가려면 그 뒤의 스냅샷을 먼저 지운다.
  되돌리기는 지금처럼 VM을 강제로 끈 뒤 하고, `zfs rollback` 앞뒤로 zvol 장치의 커널 버퍼를 비운다(`blockdev
  --flushbufs`. 비우지 못하면 옛 내용이 읽힐 수 있으므로 Task가 실패하고, 앞에서 못 비우면 되돌리지 않는다).
- **자동 스냅샷 도구 주의.** opl zvol에 opLite 밖에서 스냅샷이 생기면 그보다 앞의 스냅샷으로는 되돌릴 수 없다.
  zvol에 `com.sun:auto-snapshot=false`를 두므로 이 속성을 따르는 zfs-auto-snapshot은 찍지 않는다. sanoid처럼 이
  속성을 보지 않는 도구는 설정에서 이 데이터셋을 빼거나(`autosnap = no`) 재귀 설정이 opl zvol에 닿지 않게 한다.
- **호스트 LVM이 게스트 디스크를 읽지 않게.** 게스트가 디스크에 LVM을 쓰면 호스트의 LVM이 zvol 장치(`/dev/zd*`)에서
  게스트의 PV·VG를 찾아 활성화할 수 있다. `/etc/lvm/lvm.conf`의 `devices` 절에 `global_filter = [ "r|^/dev/zd.*|" ]`를
  둔다(필터가 이미 있으면 맨 앞에 더한다). LVM-thin 풀의 `/dev/{VG}/opl-*`도 같은 이유로 뺀다([위](#lvm-thin-풀)).
- **지우기·복제·크기.** 볼륨을 지우면 zvol을 스냅샷과 함께 지운다(`zfs destroy -r`, VM이 쓰고 있으면 실패한다.
  방금 닫힌 장치를 udev 등이 아직 열고 있어 "busy"면 1초 간격으로 세 번까지 다시 해 본다).
  스냅샷에서 복제하면 스냅샷 장치(`/dev/zvol/…@{스냅샷 id}`)를 읽어 대상 풀이 블록 풀(ZFS·LVM-thin)이면 raw,
  디렉터리·NFS 풀이면 qcow2로 만들고, 대상은 스냅샷 때의 크기로 만든다(`zfs send`가 아니라 `qemu-img convert`로
  복사한다). 크기 늘리기는 실행 중이어도 되고 `zfs set volsize` 뒤 QEMU에 zvol의 실제 크기(1 MiB 단위로
  올라간다)를 알린다(이미 그 크기 이상이면 건너뛴다).
- **데이터셋이 가득 차면** 게스트의 쓰기가 실패한다(sparse zvol은 미리 자리를 잡지 않는다). 노드별 남은 공간을
  보고 관리자가 zpool을 늘린다.

## 웹 UI

opld를 `web-ui` 기능으로 빌드하면 API와 같은 주소(`https://<노드>:8006/`)에서 웹 UI가 열린다.
Node.js 22.12 이상과 pnpm이 필요하다.

```bash
pnpm -C web install
pnpm -C web build                                   # web/dist
cargo build -p opld --release --features web-ui     # web/dist를 바이너리에 넣는다
```

브라우저는 클러스터 CA를 모르므로 처음 접속하면 인증서 경고가 뜬다. CA를 내려받아
(`curl -k https://<노드>:8006/api/v1/cluster/ca -o opl-ca.pem`, `openssl x509 -in opl-ca.pem -noout
-fingerprint -sha256`의 지문을 `cluster init` 출력과 비교) 운영체제나 브라우저의 신뢰할 인증서로
가져오면 경고가 사라진다. 로그인한 뒤에는 클러스터 화면에서 CA를 내려받고 지문을 볼 수 있다.

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
그 노드에서 쓸 수 있는 풀에만 가져올 수 있다.

- **작업 보존.** 끝난(성공·실패) 작업은 만든 지 7일이 넘으면 지워지고, 그래도 1000개를 넘으면 오래된 것부터
  지워진다(opld 설정 `[janitor]`의 `task_max_age_days`·`task_max_finished`, 리더 노드가 지운다). 어느 쪽이든 끝난
  뒤 10분은 남겨 결과를 볼 수 있게 한다. 처리 중인 작업은 지우지 않는다. 작업 화면과 VM 상세의 작업 탭은 50개씩 페이지로 나눠 보여 준다.
- **막힌 동작과 이유.** 할 수 없는 메뉴 항목·버튼은 숨기지 않고 막힌 채로 이유를 보인다(예: "VM이 정지되어
  있습니다.", "내 계정은 다른 관리자가 삭제합니다."). 키보드로 닿고 화면 읽기 프로그램이 이유를 읽는다. VM에 처리
  중인 시작·정지·재시작 작업이 있으면 웹에서는 시작·정지·재시작이 막힌다(강제 정지는 된다. CLI·API에서는 새 요청이
  앞 작업을 대체한다).
- **캐시.** 웹 파일에는 내용 해시로 만든 ETag가 붙는다. `index.html`은 매번 다시 확인하지만 바뀌지 않았으면
  **304**로 본문 없이 끝난다.

## 로그인과 세션

- **시도 제한.** 같은 사용자 이름·IP로 5번 연속 실패하면 30초, 같은 IP에서 (사용자 이름과 상관없이) 20번
  연속 실패하면 30초 동안 막힌다. 풀린 뒤에도 실패가 이어지면 실패할 때마다 막는 시간이 두 배가 된다(최대 15분).
  막힌 동안에는 맞는 비밀번호도 **429** `too_many_attempts`("try again in N seconds", `Retry-After` 헤더)를
  받는다. 로그인에 성공하면 그 사용자 이름·IP의 기록은 지워지지만, IP의 기록에서는 그 시도 하나만 빠진다(자기
  계정으로 로그인해 IP의 실패 수를 지울 수 없다). 맞는 로그인이 IP를 새로 잠그지는 않는다. 마지막 실패 뒤 15분이 넘게 지나면 기록이 지워진다. IPv6 주소는
  /64 단위로 센다. 자기 비밀번호를 바꿀 때의 현재
  비밀번호 확인도 같은 기록으로 센다. 노드마다 따로 세고, opld를 다시 시작하면 기록이 사라진다.
- **프록시 뒤.** IP는 TLS 연결의 주소다(`X-Forwarded-For`는 보지 않는다). 리버스 프록시 뒤에 두면 모든 요청이
  프록시 IP로 보이므로, 한 사람이 20번 틀리면 프록시를 거치는 모든 사용자가 함께 막힌다.
- **비밀번호 변경.** 비밀번호를 바꾸면(본인이든 admin이든) 모든 노드에서 그 사용자의 로그인 세션이 끝나 다시
  로그인해야 한다. 역할만 바꾸면 세션은 그대로다. **API 토큰은 끊기지 않는다.** 토큰이 새었을 수 있으면
  `oplctl token list`·`oplctl token delete`(웹: API 토큰 화면)로 따로 지운다. 열려 있는 콘솔(WebSocket)은 연결할
  때 한 번 인증하므로 비밀번호를 바꿔도 끊기지 않는다.

## 이미지 업로드와 머신 종류

- **업로드 사전 확인.** 웹과 `oplctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에서 준비됐는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
- **처리 대기.** 웹에서 파일을 다 보낸 뒤 서버가 2분 동안 응답하지 않으면 "처리 중" 알림이 "<이름>: 서버
  응답이 없습니다. 볼륨 목록에서 결과를 확인하세요."로 바뀌고 닫을 수 있다. 업로드는 끊지 않고 서버에서 계속된다.
  나중에 응답이 오면 평소처럼 결과를 알린다.
- **머신 기본값.** 노드는 시작할 때 자기 아키텍처(x86_64·aarch64)를 등록한다(노드 목록의 "아키텍처" 열).
  머신 종류를 정하지 않으면 웹과 `oplctl vm create`는 지정한 노드의 아키텍처를, 노드를 정하지 않았으면 클러스터
  노드가 모두 같은 아키텍처일 때 그것을 쓴다(x86_64 → `q35`, aarch64 → `virt`). 아키텍처가 섞인 클러스터에서
  노드를 정하지 않으면 oplctl은 `--machine q35` 또는 `--machine virt`를 달라는 오류를 내고, 웹은 `q35`를 고른다.
- **배치.** 자동 배치는 머신 종류와 아키텍처가 같은 노드만 고른다. 지정한 노드와 머신이 맞지 않으면 **400**
  `machine_arch_mismatch`다(VM을 시작할 때도 확인한다). 버전이 섞인 클러스터는 지원하지 않으므로 모든 노드를 함께
  올린다. 새 opld로 다시 시작해 아키텍처를 등록하기 전의 노드는 자동 배치 후보가 아니다(직접 지정하면 쓸 수 있다).

## 개발

Linux에서 빌드하려면 `libssl-dev`와 `pkg-config`가 필요하다(클라이언트 TLS가 native-tls다).

```bash
cargo test --workspace                                              # 단위 테스트
brew install qemu                                                   # macOS 통합 테스트용 (Linux: 위 apt 패키지)
cargo test -p opl-driver --features storage-it --test dir_storage   # qemu-img 스토리지 테스트
cargo test -p opl-qemu --features qemu-it --test qemu_boot          # 실제 QEMU 기동 (macOS hvf, Linux KVM/TCG)
cargo test -p opl-driver --features qemu-it --test qemu_driver      # 드라이버: 기동, 재부착, 크래시 감지
cargo test -p opl-reconciler --features qemu-it --test qemu_restart # reconciler: opld 재시작 후 재연결
cargo test -p opl-api --features storage-it --test volumes          # API: 볼륨 생성·가져오기 (qemu-img)
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
# Linux, root: 실제 tap과 브리지
# root가 만든 빌드 결과가 ./target에 섞이지 않게 따로 둔다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opl-driver --features qemu-it,net-it   # tap·브리지·VLAN
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs
# Linux, root: 실제 LVM-thin 풀. 테스트가 LV를 만들고 지워도 되는 VG/thin pool과 lvm2, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opl-driver --features lvm-it --test lvm_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_LVM=opltest/tp cargo test -p opld --features lvm-it --test lvm
# Linux, root: 실제 ZFS 풀. 테스트가 zvol을 만들고 지워도 되는 데이터셋과 zfsutils-linux, qemu-io가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opl-driver --features zfs-it --test zfs_storage
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_ZFS=opltestz/opl cargo test -p opld --features zfs-it --test zfs
```

`nfs-it` 시험은 opld가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.

`lvm-it` 시험은 `OPL_TEST_LVM`의 thin pool에 `opl-` LV를 만들고 지운다. 드라이버 시험은 만들기·가져오기·스냅샷·
되돌리기·복사·지우기를, opld 시험은 LVM 디스크로 VM을 띄워 실행 중 스냅샷, 멈춘 뒤 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 VG**를 쓴다. CI `linux-kvm` 작업은 루프 장치 위에 VG `opltest`와 thin pool `tp`를 둔다:

```bash
truncate -s 2G /tmp/opltest.img
LOOP=$(sudo losetup --find --show /tmp/opltest.img)
sudo pvcreate -y "$LOOP" && sudo vgcreate opltest "$LOOP" && sudo lvcreate -y -T -L 1G opltest/tp
# 끝나면: sudo vgremove -y opltest && sudo pvremove "$LOOP" && sudo losetup -d "$LOOP"
```

`zfs-it` 시험은 `OPL_TEST_ZFS`의 데이터셋에 `opl-` zvol을 만들고 지운다. 드라이버 시험은 만들기·정보·크기·
가져오기·스냅샷(가장 최근으로만 되돌리기, 여러 볼륨을 한 명령으로)·복사·지우기를, opld 시험은 ZFS 디스크로 VM을
띄워 실행 중 스냅샷, 최근이 아닌 스냅샷 되돌리기의 409, 멈춘 뒤 최근 스냅샷으로 되돌리기, 디렉터리 풀로 복제,
지우기를 본다. **버려도 되는 데이터셋**을 쓴다. CI `linux-kvm` 작업은 파일 위에 zpool `opltestz`와 데이터셋
`opltestz/opl`을 둔다:

```bash
sudo modprobe zfs
truncate -s 2G /tmp/opltestz.img
sudo zpool create opltestz /tmp/opltestz.img && sudo zfs create opltestz/opl
# 끝나면: sudo zpool destroy opltestz && rm /tmp/opltestz.img
```

웹 UI (`web/`):

```bash
pnpm -C web install
pnpm -C web dev              # http://localhost:5173, /api는 OPLD_URL(기본 https://localhost:8006)로 넘긴다
pnpm -C web test             # 단위·컴포넌트 테스트 (Vitest)
pnpm -C web typecheck && pnpm -C web lint
pnpm -C web gen:api          # API를 바꾼 뒤 src/api/schema.d.ts를 다시 만든다(CI가 확인한다)
pnpm -C web exec playwright install chromium
pnpm -C web build && cargo test -p opld --features web-it --test web   # E2E: 브라우저 → 업로드 → VM → 콘솔 → 정리
```

`cargo test --workspace --all-features`는 `web-it`도 켜므로 위 준비(빌드한 `web/dist`, pnpm, Chromium)가
필요하다.

systemd가 있는 Linux에서는 `OPL_TEST_SYSTEMD=1`을 주면 QEMU 테스트가 systemd 스코프로 돈다(CI의
`linux-kvm` 작업). macOS에서는 Docker의 privileged Linux 컨테이너(`--init`)로 Linux 테스트를 TCG로
돌릴 수 있다.

## 문서

- [1차 하위 프로젝트 설계: 노드 코어](docs/superpowers/specs/2026-10-20-opl-node-core-design.md)
- [하위 프로젝트 2 설계: 웹 UI](docs/superpowers/specs/2026-10-23-web-ui-design.md)
- [하위 프로젝트 3 설계: VM 스냅샷](docs/superpowers/specs/2026-10-26-snapshots-design.md)
- [클러스터·Task 견고성 설계 (계획 12)](docs/superpowers/specs/2026-10-27-robustness-design.md)
- [API·보안 보완 설계 (계획 13)](docs/superpowers/specs/2026-10-28-api-security-design.md)
- [웹 정리 설계 (계획 14)](docs/superpowers/specs/2026-10-28-web-cleanup-design.md)
- [공유 스토리지 설계 1: 백엔드 구조와 NFS (계획 15)](docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md)
- [공유 스토리지 설계 2: 블록 백엔드 바탕과 LVM-thin (계획 16)](docs/superpowers/specs/2026-10-29-shared-storage-lvm-thin-design.md)
- [공유 스토리지 설계 3: ZFS zvol (계획 17)](docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
- [계획 2: 하이퍼바이저·스토리지 드라이버](docs/superpowers/plans/2026-10-20-plan2-opl-driver.md)
- [계획 3: 클러스터 저장소](docs/superpowers/plans/2026-10-21-plan3-opl-cluster.md)
- [계획 4: reconciler](docs/superpowers/plans/2026-10-21-plan4-opl-reconciler.md)
- [계획 5: API, opld, oplctl](docs/superpowers/plans/2026-10-21-plan5-api-opld-oplctl.md)
- [계획 6: 클러스터 부트스트랩, 노드 간 통신](docs/superpowers/plans/2026-10-22-plan6-cluster-bootstrap.md)
- [계획 7: Linux/KVM, 브리지 네트워크](docs/superpowers/plans/2026-10-22-plan7-kvm-bridge.md)
- [계획 8: 웹 UI 기반과 가상 머신 화면](docs/superpowers/plans/2026-10-23-plan8-web-ui.md)
- [계획 9: 웹 UI 관리 화면](docs/superpowers/plans/2026-10-26-plan9-web-ui-admin.md)
- [계획 10: VM 스냅샷과 볼륨 크기 늘리기](docs/superpowers/plans/2026-10-26-plan10-snapshots.md)
- [계획 11: 스냅샷에서 VM 복제와 예약 스냅샷](docs/superpowers/plans/2026-10-27-plan11-clone-schedule.md)
- [계획 12: 클러스터·Task 견고성](docs/superpowers/plans/2026-10-27-plan12-robustness.md)
- [계획 13: API·보안 보완](docs/superpowers/plans/2026-10-28-plan13-api-security.md)
- [계획 14: 웹 정리](docs/superpowers/plans/2026-10-28-plan14-web-cleanup.md)
- [계획 15: 공유 스토리지 1 — 백엔드 구조와 NFS](docs/superpowers/plans/2026-10-28-plan15-shared-storage-nfs.md)
- [계획 16: 공유 스토리지 2 — 블록 백엔드 바탕과 LVM-thin](docs/superpowers/plans/2026-10-29-plan16-shared-storage-lvm-thin.md)
````

`docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`에서

```markdown

- `PoolBackend::Zfs { dataset: String }`(serde `{"kind":"zfs","dataset":"tank/opl"}`). 이름 규칙: ZFS 데이터셋 이름(`/`로 나뉜 구성 요소, 각 구성 요소는 `A-Za-z0-9_.:-`, 첫 구성 요소(풀 이름)는 문자로 시작, `-`로 시작하는 구성 요소 금지, `@`·`#`·공백 금지, 전체 255자 이하).
- 노드 로컬(`shared: true`는 400), 볼륨은 raw만(qcow2는 400) — LVM-thin과 같은 규칙. `pool_kind_supports_snapshots`는 Zfs면 raw.
- zvol `{dataset}/opl-{id}`, 장치 `/dev/zvol/{dataset}/opl-{id}`. 스냅샷 `{dataset}/opl-{id}@{tag}`(tag는 스냅샷 id).
```

을 다음으로 바꾼다:

```markdown

- `PoolBackend::Zfs { dataset: String }`(serde `{"kind":"zfs","dataset":"tank/opl"}`). 이름 규칙: ZFS 데이터셋 이름(`/`로 나뉜 구성 요소, 각 구성 요소는 `A-Za-z0-9_.:-`, 첫 구성 요소(풀 이름)는 문자로 시작, `-`로 시작하는 구성 요소 금지, `@`·`#`·공백 금지, 전체 255자 이하). 구현: `StoragePoolSpec::validate`의 `PoolError::BadZfsDataset(name)`(400 `invalid_request`), 모르는 필드는 거절. 구성 요소 `.`·`..`도 거절한다. 상한은 255자가 아니라 177자다: zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`(78자를 더한다)가 ZFS 이름 상한 255자 안에 들어야 한다. 웹 `zfsDatasetError`(`web/src/storage/pools.ts`)도 같다.
- 노드 로컬(`shared: true`는 400), 볼륨은 raw만(qcow2는 400) — LVM-thin과 같은 규칙. `pool_kind_supports_snapshots`는 Zfs면 raw. 구현: 문구는 "zfs pools are node-local; shared must be false", "zfs pools hold raw volumes"(API `check_pool_format`이 두 블록 종류를 함께 본다). 가져오기·만들기의 형식 기본값은 raw. `is_shared`는 늘 false. `pool_root`는 `/dev/zvol/{dataset}`이고 파일 저장소(`check_pool_root`)는 ZFS 풀을 거절한다("zfs pools hold no volume files").
- zvol `{dataset}/opl-{id}`, 장치 `/dev/zvol/{dataset}/opl-{id}`. 스냅샷 `{dataset}/opl-{id}@{tag}`(tag는 스냅샷 id).
```

`docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`에서

```markdown

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
```

을 다음으로 바꾼다:

```markdown

- 명령은 `ZfsOps` trait 뒤(가짜/실제). 실제는 `zfs`/`zpool`, `LC_ALL=C`, 60초 제한. **바꾸는 명령은 LVM처럼 죽이지 않고**(출력은 파일), 같은 zvol에 하나씩. 읽기(`zfs list`/`zfs get`)만 죽인다. LVM의 실행 도우미를 공용 모듈(`storage/blockcmd.rs`)로 옮겨 함께 쓴다. 구현: `ZfsOps`는 `volumes(dataset)`(`zfs list -Hp -t volume -d 1 -o name,volsize,referenced`)·`snapshots(zvol)`(`-t snapshot -d 1 -s createtxg -o name,volsize`, 만든 순서)·`space`·`create`·`destroy`(`-r`)·`snapshot(zvols, tag)`·`rollback`·`destroy_snapshot`·`set_volsize`·`flush_buffers`·`changing(zvol)`(바꾸는 명령이 아직 도는지)·`settle`(`udevadm settle --timeout=10`, udev가 없으면 `false`)이고 실제 구현은 `SystemZfs`, 생성은 `ZfsStorage::new(qemu_img, Arc<dyn ZfsOps>)`, 연결은 `Storages::with_zfs`(opld `member.rs`). 오류는 `DriverError::Zfs(String)`(`zfs: {stderr 첫 줄}`, 영구 — `Lvm`과 같은 분류). 명령은 `BlockTool::new(timeout_msg, busy_msg, env, failure)`를 도구마다 `static`으로 두고(바쁜 대상 집합도 도구마다 따로) `run_read`(kill_on_drop·시간 제한)와 `run_change(program, args, targets, timeout)`(죽이지 않음, stderr는 만들자마자 지운 임시 파일, 대상마다 하나 — 앞 명령이 돌고 있으면 `Timeout`("an earlier zfs command still running on the same zvol"), 늦은 회수)를 쓴다. ZFS의 대상은 zvol 전체 이름, 여러 zvol을 찍는 `snapshot`은 그 zvol 모두다.
- 만들기: `zfs create -s -V {size} -o volmode=dev -o snapdev=visible {dataset}/opl-{id}`(크기는 volblocksize 배수로 올림 — 기본 16K, `zfs get volblocksize`로 확인하지 않고 1 MiB 배수로 올린다). 장치 노드가 나타날 때까지 짧게 기다린다(udev, 최대 10초, 없으면 지우고 오류). 구현: 이미 있으면 `VolumeExists`, 노드가 끝내 없으면 `VolumeNotActive`(일시). udev가 새 zvol을 다시 다루는 동안 `/dev/zvol` 링크가 잠시 사라졌다 생겨 노드를 본 직후 열기가 실패했으므로(CI에서 발견) 노드를 기다리기 전에 `udevadm settle`을 하고, 하지 못하면(없음, 시간 초과는 경고) 100 ms 떨어진 두 번의 확인에서 모두 보여야 한다. 스냅샷을 찍은 뒤와 되돌린 뒤에도 settle한다. `-o com.sun:auto-snapshot=false`도 둔다(이 속성을 따르는 자동 스냅샷 도구가 zvol을 찍으면 더 앞의 스냅샷으로 되돌릴 수 없다. sanoid처럼 속성을 보지 않는 도구는 README에서 설정으로 빼라고 안내한다).
- 가져오기: 가상 크기로 만든 뒤 `qemu-img convert -n [--target-is-zero] -O raw`(새 sparse zvol은 0으로 읽힌다). 실패하면 지운다. 구현: `--target-is-zero`는 늘 쓴다(`target_reads_zero`도 기본값 참이라 백엔드 사이 복사에서도 쓴다). 원본 가상 크기는 LVM과 함께 쓰는 `source_virtual_size`(`qemu-img info`)다.
- 지우기: `zfs destroy -r {dataset}/opl-{id}`(스냅샷 함께). 없으면 `VolumeNotFound`. 구현: "busy"로 실패하면(방금 닫힌 장치를 udev 등이 아직 열고 있다) 1초 간격으로 세 번까지 다시 한다. 볼륨 지우기와, 만들기·가져오기·복사가 실패한 뒤의 정리가 같다.
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
- **되돌리기 제한:** API는 VM의 디스크가 ZFS이고 되돌릴 스냅샷이 그 VM의 가장 최근 스냅샷이 아니면 409 `snapshot_unsupported`("zfs disks can only revert to the latest snapshot")로 Task를 만들기 전에 거절한다. 드라이버도 같은 확인을 한다(경쟁 대비). 구현: `snapshots.rs`의 `check_zfs_latest`. 되돌릴 스냅샷의 `vm_spec` 디스크에 ZFS 풀의 볼륨이 있을 때만 보고, 순서는 `status.created_at`(만드는 중이라 없으면 가장 나중), 같으면 id(UUIDv7)다. 웹은 `web/src/vms/snapshots.ts`의 `zfsNotLatest`로 같은 규칙을 보고 되돌리기 버튼을 이유(`snapshots.zfsLatest`)와 함께 막는다(`ReasonButton`). 드라이버는 볼륨의 ZFS 스냅샷 목록의 마지막인지 본다(`InvalidSpec`). API는 VM의 스냅샷 순서만 보므로 opLite 밖에서 만든 스냅샷(자동 스냅샷 도구 등)은 모른다. 그래서 되돌리기 Task가 VM을 끄기 전에 ZFS 디스크마다 `snapshot_tags`의 마지막이 되돌릴 태그인지 보고, 아니면 VM을 건드리지 않고 더 최근 스냅샷의 이름(opLite 스냅샷이면 그 이름, 아니면 태그)과 함께 실패한다.
- 크기 늘리기: ZFS도 `volsize` 먼저, 실행 중이면 `block_resize`(실제 volsize로).
- 구현(복제 형식, `tasks/clone.rs`의 `copy_format`): 대상이 블록 풀(LVM-thin, ZFS)이면 raw, 블록 볼륨을 파일 풀로 옮기면 qcow2, 파일 풀 사이는 원본 형식 그대로다.

## 5. 노드별 상태

- 마운터: Zfs 풀은 `zfs get -Hp -o value available,used,type {dataset}`(10초). `type`이 `filesystem`(또는 `volume`이 아닌 데이터셋)이면 준비, 용량은 `available`+`used` 합과 `available`. 없으면 이유. macOS는 `"zfs pools are not supported on macOS"`(VM은 이유와 함께 끈다). 구현: 해석은 드라이버의 `parse_zfs_dataset` → `ZfsDataset { kind, space }`. 확인은 LVM과 함께 풀마다 비동기 작업(`block_checks`)으로 돌고 10초가 지나면 `zfs`를 죽인다. 이유 문구는 `"{dataset} is a volume, not a filesystem dataset"`, `"zfs timed out"`, 그 밖에는 `zfs`의 stderr 첫 줄이다. 지원 여부는 `Pools.zfs_supported`(Linux만, 테스트가 바꾼다).
- `MountOps::zfs_dataset(dataset)`.
```

`docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`에서

```markdown

- **oplctl:** `pool create NAME --zfs DATASET`(다른 종류와 함께 못 씀), 목록·상세 종류 `zfs`·위치 `DATASET`, 형식 기본값 raw.
- **웹:** 풀 만들기의 ZFS 종류(데이터셋 필드와 검사), 목록 종류·위치, `poolFormats`가 ZFS면 raw, `snapshotSupport`가 블록 방식 일반화와 ZFS의 되돌리기 제한(가장 최근이 아닌 스냅샷의 되돌리기 메뉴를 이유와 함께 막는다 — `ReasonMenuItem`).
```

을 다음으로 바꾼다:

```markdown

- **oplctl:** `pool create NAME --zfs DATASET`(다른 종류와 함께 못 씀), 목록·상세 종류 `zfs`·위치 `DATASET`, 형식 기본값 raw. 구현: `--shared`·`--nfs-options`와도 함께 못 쓴다(clap). 데이터셋 이름 검사는 API가 한다.
- **웹:** 풀 만들기의 ZFS 종류(데이터셋 필드와 검사), 목록 종류·위치, `poolFormats`가 ZFS면 raw, `snapshotSupport`가 블록 방식 일반화와 ZFS의 되돌리기 제한(가장 최근이 아닌 스냅샷의 되돌리기 메뉴를 이유와 함께 막는다 — `ReasonMenuItem`). 구현: 스냅샷 탭의 되돌리기는 메뉴가 아니라 버튼이므로 `ReasonButton`으로 막는다. `pools.ts`의 `blockSnapshotKind`, `SnapshotSupport`의 `ok.block`(옛 `lvm`), i18n `snapshots.blockHint`(옛 `lvmHint`)·`snapshots.zfsLatest`·`pools.kinds.zfs`·`pools.dataset`·`pools.errors.zfsDataset`.
```

`docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`에서

```markdown
- **단위:** 타입·이름 검증, 가짜 `ZfsOps`로 드라이버 각 동작(만들기·가져오기·지우기·정보·용량·크기·스냅샷·원자적 다중 스냅샷·최근 스냅샷만 되돌리기·태그·원본), 공용 명령 도우미, API(ZFS 규칙, 섞인 블록 방식 409, 최근이 아닌 되돌리기 409), 마운터 ZFS 상태, 웹·oplctl.
- **실제 ZFS(`zfs-it`, Linux·root):** CI `linux-kvm`에 `zfsutils-linux`, 루프 파일 위 zpool `opltestz`와 데이터셋 `opltestz/opl`(`OPL_TEST_ZFS=opltestz/opl`). 드라이버 통합 테스트와 opld E2E(ZFS 디스크 VM, 실행 중 스냅샷, 최근 스냅샷 되돌리기, 디렉터리 풀로 복제, 지우기).
```

을 다음으로 바꾼다:

```markdown
- **단위:** 타입·이름 검증, 가짜 `ZfsOps`로 드라이버 각 동작(만들기·가져오기·지우기·정보·용량·크기·스냅샷·원자적 다중 스냅샷·최근 스냅샷만 되돌리기·태그·원본), 공용 명령 도우미, API(ZFS 규칙, 섞인 블록 방식 409, 최근이 아닌 되돌리기 409), 마운터 ZFS 상태, 웹·oplctl.
- **실제 ZFS(`zfs-it`, Linux·root):** CI `linux-kvm`에 `zfsutils-linux`, 루프 파일 위 zpool `opltestz`와 데이터셋 `opltestz/opl`(`OPL_TEST_ZFS=opltestz/opl`). 드라이버 통합 테스트와 opld E2E(ZFS 디스크 VM, 실행 중 스냅샷, 최근 스냅샷 되돌리기, 디렉터리 풀로 복제, 지우기). 구현: 기능 `zfs-it`(opl-driver, opld), `crates/opl-driver/tests/zfs_storage.rs`(4개), `crates/opld/tests/zfs.rs`(1개, 풀은 API로 만들고 볼륨은 `--format raw`, 최근이 아닌 되돌리기의 409도 본다. 디스크 내용은 꺼진 VM의 zvol 장치를 `qemu-io`로 보고, 읽기는 `-r` — 스냅샷 장치는 읽기 전용이다). CI는 `zfsutils-linux` 설치, `modprobe zfs`, `udevadm control --reload-rules` 뒤 2G 파일 위에 zpool을 만든다.
```

`docs/superpowers/specs/2026-10-29-shared-storage-zfs-design.md`에서

```markdown
- zvol 크기는 1 MiB 배수로 올린다.
```

을 다음으로 바꾼다:

```markdown
- zvol 크기는 1 MiB 배수로 올린다.
- ZFS를 바꾸는 명령이 60초를 넘겨 `Timeout`으로 끝나도 명령은 뒤에서 끝까지 돈다(LVM과 같다). 그동안 같은 zvol을 바꾸는 요청은 일시 오류로 끝나고, 이 기록은 opld 프로세스 안의 것이다.
- 데이터셋이 가득 차면 sparse zvol에 쓰기가 실패한다(자리를 미리 잡지 않는다). 가져오기의 공간 사전 확인은 지금처럼 업로드 디렉터리만 본다.
- 웹의 최근 스냅샷 판단은 화면의 목록으로 하므로 그사이 다른 스냅샷이 생기면 서버가 409로 거절한다.
- LVM의 기본 `snapshot_many`(하나씩)가 묶음 중간에 시간을 넘기면 먼저 끝난 디스크의 스냅샷 LV가 남을 수 있다(남긴 태그의 뒤 정리와 실행기 시작 때의 정리가 치운다).
- 지연 태그 정리가 같은 zvol의 새 스냅샷과 겹치면 새 스냅샷이 일시 오류로 실패할 수 있다.
- 호스트의 LVM이 zvol(`/dev/zd*`)이나 LVM-thin 볼륨 LV에서 게스트의 PV를 볼 수 있다. README가 `global_filter`(LV는 기본값 `scan_lvs = 0`)를 권한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 555개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e --test snapshots`
Expected: PASS

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: ZFS 풀 안내, 공유 스토리지 설계 3에 구현하며 정한 것"
```

---

## 완료 기준 (계획 17)

- ZFS 풀을 API·oplctl·웹으로 만들 수 있고, 공유·qcow2·잘못된 이름(177자 초과 포함)은 400이다.
- 노드가 데이터셋을 확인해 상태와 용량을 알리고, macOS는 이유를 남긴다.
- ZFS 볼륨의 만들기·가져오기·지우기·크기·스냅샷·최근 스냅샷 되돌리기·복제가 되고, 되돌린 뒤 옛 데이터가 보이지 않으며, 가장 최근이 아닌 스냅샷 되돌리기는 VM을 건드리기 전에 거절된다.
- 여러 블록 디스크의 스냅샷이 한 번에 찍히고, 남은 태그는 뒤에서 정리된다. LVM 동작은 바뀌지 않는다.
- Linux에서 실제 ZFS로 드라이버 통합 테스트와 VM E2E가 통과한다(`zfs-it`, CI `linux-kvm`).
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **가장 최근 스냅샷으로만 되돌릴 수 있다.** 더 오래된 스냅샷으로 가려면 그 뒤 스냅샷을 먼저 지운다. 자동 스냅샷 도구(sanoid 등)가 opl zvol에 스냅샷을 만들면 되돌리기가 막힌다(설정에서 제외한다).
- **메모리 스냅샷이 없다.** 노드 로컬이다. 실행 중 스냅샷 동안 VM이 잠깐 멈춘다.
- **zvol 크기는 1 MiB 배수로 올린다.**
- 그 밖의 제한은 설계 문서 §8에 있다.

## 실행 후 기록 (2026-10-29)

9개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). 최종 리뷰(심각·중요 없음)의 작은 지적으로 아래가 바뀌었다(`63a884d`). 저장소의 코드가 기준이다. 위 Task의 코드 블록은 고치기 전 것이다.

- 실행 중 VM의 블록 스냅샷 멈춤 구간에서 `udevadm settle`을 빼고, 스냅샷·볼륨 지우기 직전에 settle하며 스냅샷 지우기도 busy면 다시 한다.
- 만들기가 시간 초과된 볼륨을 지울 때 그 zvol·LV에 명령이 아직 돌면 `VolumeNotFound` 대신 일시 오류(리소스를 지우고 볼륨을 새게 두지 않는다, LVM도).
- ZFS 되돌리기의 최신 규칙은 VM 전체가 아니라 **같은 ZFS 볼륨을 담은** 더 새 스냅샷이 있을 때만 막는다(서버·웹).
- 남은 opLite 태그 때문에 되돌리기가 막히면 오류가 "opld가 뒤에서 지우는 남은 스냅샷 — 잠시 뒤 다시 하거나 opld를 다시 시작"이라고 알린다. 가져오기 형식 문서에 ZFS.

알고 받아들인 동작(위 제한 목록에 더해):
- 옛 opld는 ZFS 풀 리소스를 읽지 못한다(모든 노드를 올린 뒤 ZFS 풀을 만든다, 서버 쪽 막음은 없다).
- 지연 태그 정리가 같은 zvol의 새 스냅샷과 겹치면 새 스냅샷이 일시 오류로 실패할 수 있다.

테스트: macOS `cargo test --workspace` 559개, `opl-driver` `storage-it` 133개, `opl-api` `storage-it` 210개, `qemu-it` `smoke`·`e2e`·`snapshots` 11개, 브라우저 E2E, 웹 Vitest 286개, `fmt`·`clippy -D warnings`(모든 기능). 실제 ZFS(`zfs-it`)·LVM·NFS는 Linux CI `linux-kvm`에서.

## 다음 계획으로 넘길 사항

- 계획 18: iSCSI, Ceph RBD(QEMU 네트워크 블록 드라이버, 공유 블록 스토리지).
- 하위 프로젝트 4: 라이브 마이그레이션, UEFI 변수 공유, 풀 `shared` 바꾸기.
