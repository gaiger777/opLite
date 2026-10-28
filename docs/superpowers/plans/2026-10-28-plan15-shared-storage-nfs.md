# opLite 계획 15: 공유 스토리지 1 — 백엔드 구조와 NFS 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 저장소 코드가 풀 백엔드 종류를 가정하지 않게 하고, opld가 마운트하는 NFS 풀을 더하며, 노드가 풀마다 쓸 수 있는지·용량을 알리고, 배치와 볼륨 Task 노드가 그 상태와 공유 여부를 보게 한다.

**Architecture:** `PoolBackend::Nfs { server, export, options }`를 더하고 저장소 함수가 풀 리소스를 받아 `pool_root`(Dir → 경로, Nfs → `{data_dir}/pools/{id}`)로 루트를 정한다. 드라이버와 reconciler는 NFS 루트가 실제로 마운트됐는지 `/proc/self/mountinfo`로 확인한다(`check_pool_root`, 아니면 일시 오류 `PoolNotMounted`). opld의 `PoolMounter`가 노드마다 풀을 확인·마운트·언마운트해 `NodeStatus.pools`(풀 이름 → `PoolState { pool_id, ready, error, 용량 }`)에 쓰고, 스케줄러·`pool_node`·가져오기·복제가 `pool_usable_on`으로 판단한다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft, axum 0.8, utoipa, clap, rustix(statvfs); 웹은 계획 8~14와 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW). 새 외부 의존성은 없다(opld가 `async-trait`를 일반 의존성으로 쓴다). 실제 NFS 시험은 Linux CI의 `nfs-kernel-server`.

**설계 문서:** `docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md` (§1은 하위 프로젝트 3 나머지의 분해, §2–§10이 이 계획)
**선행 계획:** 계획 1~14 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `PoolBackend::Nfs`와 검증(마운트 옵션 거부 목록, 항상 `nosuid,nodev,noexec`) | macOS 노드의 NFS 마운트, NFS 서버 관리 |
| 풀 리소스를 받는 저장소, `pool_root`, `check_pool_root`(mountinfo) | 블록 백엔드와 디스크 소스 추상화 — 계획 16(LVM-thin) |
| opld `PoolMounter`(풀마다 마운트·언마운트, 진행 중 추적, 백오프), `NodeStatus.pools` | ZFS — 계획 17, iSCSI·Ceph RBD — 계획 18 |
| 배치·Task 노드·가져오기·복제의 `pool_usable_on`, 409 `pool_unavailable`, VM 시작의 일시 재시도 | UEFI 변수 공유, 라이브 마이그레이션 — 하위 프로젝트 4 |
| oplctl `pool create --nfs`, `pool list`/`pool get`의 노드별 상태 | |
| 웹 풀 만들기(NFS)·풀 목록의 노드별 상태·노드 목록의 풀 열, 준비된 풀만 고르는 대화상자 | |
| 실제 NFS 시험(`nfs-it`, CI `linux-kvm`) | |

## 이전 계획에서 이어받는 사항

- 노드 코어 설계 §6: `PoolSpec.shared`를 저장만 하던 것을 배치와 Task 노드 선택에 쓴다.
- 스냅샷 설계: 하위 프로젝트 3의 나머지(NFS, LVM-thin, ZFS, iSCSI, Ceph)를 드라이버 구조 위에 붙인다 — 이 계획이 NFS와 백엔드 구조다.
- 지금까지 `nodes`가 빈 로컬 디렉터리 풀의 VM이 파일이 없는 노드에 배치될 수 있던 문제를 고친다.

## 실행 환경

계획 14와 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상), Node.js 22.12 이상, pnpm 10.33.2. 실제 QEMU 테스트는 기능으로 켠다: `opld`의 `qemu-it`, `opl-api`·`opl-driver`의 `storage-it`, `opld`의 `web-it`. **실제 NFS 시험(`opld`의 `nfs-it`)은 Linux에서 root로만 돈다.** CI `linux-kvm` 작업이 `nfs-kernel-server`를 깔고 `/srv/opl-nfs`를 `127.0.0.1`에 내보낸 뒤 `OPL_TEST_NFS_EXPORT=/srv/opl-nfs`로 돌린다. 시험용 내보내기는 지워져도 되는 디렉터리여야 한다. macOS에서는 컴파일되지 않는다(시험 파일이 Linux 전용).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`, Task 1). `Cargo.lock`은 바뀌지 않는다.

`opld`의 `web-it` 테스트는 빌드 스크립트가 기억한 `web/dist` 경로를 쓴다. 다른 디렉터리에서 빌드한 target을 다시 쓰다가 "built without the web UI"가 나오면 `touch crates/opld/build.rs` 뒤 다시 돌린다.

## 설계 결정

- **NFS 풀.** `server`(`-`로 시작하거나 `: , / [ ]`·공백이 있으면 거절, IPv6는 대괄호 없이), `export`(절대 경로), `options`(쉼표 목록, 거부 목록: `bind`, `rbind`, `remount`, `move`, `suid`, `dev`, `exec`, `helper`, `bg`, `x-*`, `X-mount.*`). 마운트는 항상 `-o nosuid,nodev,noexec[,사용자 옵션]`. NFS 풀은 항상 공유이고 `shared: false`는 400 `invalid_request`.
- **풀 루트.** `pool_root`: Dir → 경로, Nfs → `{data_dir}/pools/{id}`. `check_pool_root`는 NFS면 정규화한 그 경로가 mountinfo의 nfs/nfs4 마운트 지점인지 본다(NFS 서버에 닿지 않는다). 아니면 `DriverError::PoolNotMounted`(일시 오류). 파일이 없다는 결과가 나오면 마운트를 다시 확인해, 마운트가 풀린 것을 "없음 = 성공"으로 보지 않는다.
- **노드별 풀 상태.** `NodeStatus.pools: BTreeMap<이름, PoolState { pool_id, ready, error, total_bytes, available_bytes }>`. 상태는 `pool_id`가 풀의 id와 같을 때만 믿는다(같은 이름으로 다시 만든 풀이 옛 상태를 물려받지 않게). 노드 상태를 쓰는 곳은 모두 리비전을 확인하고 쓴다(`update_node_status`).
- **마운터.** 노드마다 `[pools] refresh_secs`(기본 15초) 간격과 풀이 바뀔 때 곧바로. Dir은 쓰기 확인과 statvfs(10초 제한), Nfs는 마운트(30초 제한) 뒤 같은 확인. 마운트·언마운트는 풀마다 뒤에서 따로 돌고, 진행 중이면 새로 시작하지 않으며, 실패하면 15초부터 두 배씩 최대 5분 기다린다. `umount -f`는 쓰지 않는다. 그 풀의 디스크를 쓰는 VM이 이 노드에서 돌고 있으면 언마운트를 미룬다. macOS는 NFS를 마운트하지 않는다.
- **쓸 수 있음.** `pool_usable_on(pool, node)`: 노드 목록이 허용하고, NFS면 `ready: true`, Dir이면 명시적 `ready: false`가 아닐 것(보고 전에도 쓸 수 있다). 스케줄러는 공유가 아닌 풀의 볼륨이 있는 노드만 고른다. 볼륨 Task 노드는 이 노드 → 쓸 수 있는 온라인 노드 순. 노드 목록에 없으면 `pool_not_on_node`, 있지만 준비 안 됨이면 409 `pool_unavailable`.
- **VM 시작.** 풀을 쓸 수 없거나 NFS가 마운트되지 않았으면 VM을 끄지 않고 `retry_interval`마다 다시 시도한다(이유는 `last_error`).
- **고아 볼륨.** 시작할 때 정리하지 못한 NFS 볼륨(`PoolNotMounted`)은 `orphan_retry`(60초)마다 다시 시도한다.
- **혼합 버전 클러스터**는 지원하지 않는다(모든 노드를 함께 올린다).

## 파일 구조

```
crates/opl-types/src/{storage.rs, node.rs}            # PoolBackend::Nfs, 검증, is_shared, pool_usable_on, PoolState
crates/opl-driver/src/{storage.rs, mounts.rs(새로), lib.rs}   # pool_root, check_pool_root, mountinfo 해석, 풀 리소스 인자
crates/opl-reconciler/src/{config.rs, resolve.rs, worker.rs}  # mount_root, PoolUnavailable 일시 재시도
crates/opl-api/src/{resources.rs, scheduler.rs, volumes.rs, snapshots.rs, tasks.rs, tasks/*.rs}   # NFS 풀 만들기, 배치·Task 노드, 고아 재시도
crates/opl-peer/src/monitor.rs                        # 확인된 상태 쓰기
crates/opld/src/{pools.rs(새로), config.rs, member.rs, bootstrap.rs}   # PoolMounter, [pools], 확인된 노드 상태 쓰기
crates/opld/tests/nfs.rs(새로)                         # nfs-it
crates/oplctl/src/cli.rs                              # pool create --nfs, pool list/get
web/src/storage/{pools.ts(새로), PoolStatus.tsx(새로), PoolsPage.tsx, UploadDialog.tsx, VolumesPage.tsx}, web/src/vms/VmCloneDialog.tsx, web/src/nodes/NodesPage.tsx, web/src/i18n/{ko,en}.json
.github/workflows/ci.yml, README.md, docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 타입: NFS 풀 백엔드와 노드별 풀 상태

`PoolBackend::Nfs { server, export, options }`(serde `{"kind":"nfs"}`, 모르는 필드 거절)와 `StoragePoolSpec::validate()`(`PoolError`: 디렉터리 경로는 절대 경로, 서버는 비지 않은 출력 가능 ASCII이고 `-`로 시작하거나 `: , / [ ]`가 없을 것 — IPv6는 대괄호 없이, 내보내기는 `/`로 시작하고 제어 문자 없음, 옵션은 공백·제어 문자 없고 거부 목록 `bind`·`rbind`·`remount`·`move`·`suid`·`dev`·`exec`·`helper`·`bg`·`x-*`·`X-mount.*`가 아님), `is_shared`(공유 표시 또는 NFS), `node::PoolState { pool_id, ready, error, total_bytes, available_bytes }`와 `NodeStatus.pools`(풀 이름 → 상태, 옛 상태는 빈 맵)를 더한다. 풀 API는 `validate`를 쓰고, 이 Task에서는 NFS 풀 만들기를 아직 400으로 거절한다(Task 4에서 허용). opld는 등록할 때 `pools`를 빈 맵으로 쓴다(Task 4에서 보존으로 바뀐다). 웹 타입을 다시 만들고 풀 목록이 NFS 위치를 보이게 컴파일만 맞춘다.

**Files:**
- Create: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust
    check_name(&req.name)?;
    let PoolBackend::Dir { path } = &req.spec.backend;
    if !path.is_absolute() {
        return Err(ApiError::bad_request("pool path must be absolute"));
    }
```

을 다음으로 바꾼다:

```rust
    check_name(&req.name)?;
    req.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS 풀 마운트와 배치는 아직 없다.
    if matches!(req.spec.backend, PoolBackend::Nfs { .. }) {
        return Err(ApiError::bad_request("nfs pools are not supported yet"));
    }
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            arch: Some(Arch::Aarch64),
        };
```

을 다음으로 바꾼다:

```rust
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            arch: Some(Arch::Aarch64),
        };
```

을 다음으로 바꾼다:

```rust
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
```

`crates/opl-api/tests/pools.rs`를 만든다:

```rust
//! 스토리지 풀 API: 백엔드별 값 검사.

mod common;

use common::Env;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn pool(name: &str, backend: Value) -> Value {
    json!({ "name": name, "spec": { "backend": backend } })
}

#[tokio::test]
async fn rejects_bad_backend_values() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let nfs =
        |server: &str, export: &str| json!({ "kind": "nfs", "server": server, "export": export });
    for (bad, why) in [
        (
            pool("rel", json!({ "kind": "dir", "path": "pool" })),
            "relative path",
        ),
        (pool("srv", nfs("nas:2049", "/srv/vm")), "server with ':'"),
        (pool("exp", nfs("nas", "srv/vm")), "relative export"),
        (
            pool(
                "opt",
                json!({ "kind": "nfs", "server": "nas", "export": "/srv/vm", "options": "vers=4.2, hard" }),
            ),
            "options with space",
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {body}");
        assert_eq!(body["code"], "invalid_request", "{why}");
    }
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("dir", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn nfs_pools_are_not_supported_yet() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool(
                "nas",
                json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("nfs"), "{body}");
    assert_eq!(
        admin.get("/storage-pools/nas").await.0,
        StatusCode::NOT_FOUND
    );
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            arch: Some(Arch::Aarch64),
        };
```

을 다음으로 바꾼다:

```rust
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
```

`crates/opl-driver/src/storage.rs`에서

```rust
    pub fn volume_path(pool: &StoragePoolSpec, id: Uuid, format: ImageFormat) -> PathBuf {
        let PoolBackend::Dir { path } = &pool.backend;
        path.join(format!("{id}.{}", fmt_name(format)))
    }
```

을 다음으로 바꾼다:

```rust
    pub fn volume_path(pool: &StoragePoolSpec, id: Uuid, format: ImageFormat) -> PathBuf {
        let root = match &pool.backend {
            PoolBackend::Dir { path } => path.as_path(),
            // 아직 지원하지 않는다. 드라이버는 pool_dir에서, reconciler는 디스크를 풀 때 거절한다.
            PoolBackend::Nfs { .. } => Path::new(""),
        };
        root.join(format!("{id}.{}", fmt_name(format)))
    }
```

`crates/opl-driver/src/storage.rs`에서

```rust
fn pool_dir(pool: &StoragePoolSpec) -> Result<&Path, DriverError> {
    let PoolBackend::Dir { path } = &pool.backend;
    require_absolute(path)?;
    Ok(path)
}
```

을 다음으로 바꾼다:

```rust
fn pool_dir(pool: &StoragePoolSpec) -> Result<&Path, DriverError> {
    match &pool.backend {
        PoolBackend::Dir { path } => {
            require_absolute(path)?;
            Ok(path)
        }
        PoolBackend::Nfs { .. } => Err(DriverError::InvalidSpec(
            "nfs pools are not supported yet".into(),
        )),
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
            other => panic!("unexpected error: {other:?}"),
        }
    }
```

을 다음으로 바꾼다:

```rust
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_nfs_pools_for_now() {
        // qemu-img를 부르기 전에 거절한다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let p = StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: "nas".into(),
                export: "/srv/vm".into(),
                options: None,
            },
            nodes: vec![],
            shared: true,
        };
        let err = storage
            .create_volume(&p, Uuid::nil(), 1 << 20, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
```

을 다음으로 바꾼다:

```rust
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        // NFS 풀은 아직 마운트하지 않는다.
        if (!pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node))
            || matches!(pool.spec.backend, PoolBackend::Nfs { .. })
        {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
```

`crates/opl-types/src/node.rs`에서

```rust
use serde::{Deserialize, Serialize};
```

을 다음으로 바꾼다:

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
```

`crates/opl-types/src/node.rs`에서

```rust
    pub arch: Option<Arch>,
}
```

을 다음으로 바꾼다:

```rust
    pub arch: Option<Arch>,
    /// 이 노드가 쓸 수 있는 스토리지 풀(풀 이름 → 상태). 노드가 주기적으로 기록한다.
    pub pools: BTreeMap<String, PoolState>,
}

/// 한 노드에서 본 스토리지 풀 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct PoolState {
    /// 이 상태를 적을 때의 풀 id. 같은 이름으로 다시 만든 풀에 옛 상태를 쓰지 않게, 쓰는 쪽은 이 값이
    /// 풀 id와 같을 때만 믿는다. 이 필드 전의 상태는 nil이다.
    pub pool_id: Uuid,
    /// 이 노드에서 풀에 볼륨을 만들고 VM 디스크로 쓸 수 있는지
    pub ready: bool,
    /// 준비되지 않은 이유(예: 마운트 실패 메시지)
    pub error: Option<String>,
    pub total_bytes: u64,
    pub available_bytes: u64,
}
```

`crates/opl-types/src/node.rs`에서

```rust
        assert_eq!(old.arch, None);
        let status = NodeStatus {
```

을 다음으로 바꾼다:

```rust
        assert_eq!(old.arch, None);
        assert!(old.pools.is_empty());
        let status = NodeStatus {
```

`crates/opl-types/src/node.rs`에서

```rust
    }
}
```

을 다음으로 바꾼다:

```rust
    }

    #[test]
    fn status_pools_round_trip() {
        let mut status = NodeStatus::default();
        status.pools.insert(
            "nas".into(),
            PoolState {
                pool_id: Uuid::now_v7(),
                ready: false,
                error: Some("mount.nfs: access denied".into()),
                total_bytes: 0,
                available_bytes: 0,
            },
        );
        status.pools.insert(
            "local".into(),
            PoolState {
                pool_id: Uuid::now_v7(),
                ready: true,
                error: None,
                total_bytes: 100 << 30,
                available_bytes: 40 << 30,
            },
        );
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["pools"]["local"]["ready"], true);
        assert_eq!(json["pools"]["nas"]["error"], "mount.nfs: access denied");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn pool_state_without_pool_id_reads_as_nil() {
        // pool_id를 적기 전의 상태. 어느 풀의 상태인지 모르므로 쓰는 쪽이 믿지 않는다.
        let old: PoolState = serde_json::from_value(serde_json::json!({ "ready": true })).unwrap();
        assert_eq!(old.pool_id, Uuid::nil());
        assert!(old.ready);
    }
}
```

`crates/opl-types/src/storage.rs`에서

```rust
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. 라이브 마이그레이션에서 쓴다.
    #[serde(default)]
```

을 다음으로 바꾼다:

```rust
    pub nodes: Vec<Uuid>,
    /// 모든 노드가 같은 데이터를 보는 공유 스토리지인지. NFS 풀은 늘 공유다(`is_shared`).
    #[serde(default)]
```

`crates/opl-types/src/storage.rs`에서

```rust
    },
}
```

을 다음으로 바꾼다:

```rust
    },
    /// NFS 내보내기를 노드마다 마운트하고 그 안에 이미지 파일을 둔다. 늘 공유 풀이다.
    Nfs {
        /// 호스트 이름 또는 IP. IPv6는 `[...]` 없이 적는다.
        server: String,
        /// 서버의 내보내기 경로(절대 경로)
        export: String,
        /// `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 늘 붙는 `nosuid,nodev,noexec` 뒤에 붙고,
        /// 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec` 등)은 받지 않는다. 없으면 커널 기본값.
        #[serde(default)]
        options: Option<String>,
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

/// 받지 않는 마운트 옵션(`=` 앞 이름, 대소문자 무시). 다른 경로를 연결하거나(`bind`, `rbind`,
/// `move`), 기존 마운트를 바꾸거나(`remount`), 장치·setuid·실행을 허용하거나(`dev`, `suid`, `exec`),
/// 다른 프로그램을 부르는(`helper`, `x-*`, 그중 `X-mount.*`) 옵션, 마운트하지 못해도 뒤에서 다시
/// 시도하며 성공으로 돌아오는(`bg`) 옵션이다. opld는 늘 `nosuid,nodev,noexec`를 붙인다.
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

/// 모든 노드가 같은 데이터를 보는 풀인지. NFS 풀은 `shared` 값과 상관없이 공유다.
pub fn is_shared(pool: &StoragePool) -> bool {
    pool.spec.shared || matches!(pool.spec.backend, PoolBackend::Nfs { .. })
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
}
```

`crates/opl-types/src/storage.rs`에서

```rust
    }

    #[test]
    fn rejects_unknown_backend_fields() {
```

을 다음으로 바꾼다:

```rust
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
    fn rejects_unknown_backend_fields() {
```

`crates/opld/src/bootstrap.rs`에서

```rust
        arch: Some(host.profile.arch),
    };
```

을 다음으로 바꾼다:

```rust
        arch: Some(host.profile.arch),
        // 풀 상태는 풀 새로 고침이 채운다.
        pools: Default::default(),
    };
```

`web/src/storage/PoolsPage.tsx`에서

```tsx
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell className="font-mono text-xs">{pool.spec.backend.path}</TableCell>
                <TableCell>{nodeNames(pool)}</TableCell>
```

을 다음으로 바꾼다:

```tsx
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell className="font-mono text-xs">
                  {pool.spec.backend.kind === "dir"
                    ? pool.spec.backend.path
                    : `${pool.spec.backend.server}:${pool.spec.backend.export}`}
                </TableCell>
                <TableCell>{nodeNames(pool)}</TableCell>
```

`web/src/test/fixtures.ts`에서

```ts
    arch: null,
  },
```

을 다음으로 바꾼다:

```ts
    arch: null,
    pools: {},
  },
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 409개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `cargo test -p opl-api --test pools`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 258개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): NFS 풀 백엔드와 노드별 풀 상태 타입"
```

---

### Task 2: 저장소: 백엔드별 풀 루트와 마운트 확인

저장소 trait·`DirStorage`·테스트의 `FakeStorage`·호출하는 곳이 `&StoragePoolSpec` 대신 `&StoragePool`을 받는다. `pool_root(pool, mount_root)`(Dir → 경로, Nfs → `{mount_root}/{id}`), `DirStorage::new(qemu_img, mount_root)`, 정적 `DirStorage::volume_path(pool, mount_root, id, format)`. 새 `opl_driver::mounts`(`MOUNTINFO`, `nfs_mount_points` — 8진 이스케이프 해석)와 `check_pool_root(pool, mount_root)`: NFS면 정규화한 마운트 루트 아래 `{id}`가 mountinfo의 nfs/nfs4 마운트 지점인지 보고(NFS 서버에 닿지 않는다) 아니면 일시 오류 `DriverError::PoolNotMounted`. 모든 드라이버 진입점이 이 확인을 거치고, 지우기·기존 파일 찾기에서 파일이 없으면 마운트를 다시 확인한다. reconciler는 `ReconcilerConfig::new(node, mount_root)`와 `runtime_spec(.., mount_root)`로 경로를 정한다. opld는 `{data_dir}/pools`를 넘긴다. 디렉터리 풀의 동작은 그대로다.

**Files:**
- Create: `crates/opl-driver/src/mounts.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/tests/dir_storage.rs`
- Modify: `crates/opl-reconciler/src/config.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

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
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::vm_placement;

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
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
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
        sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>) {
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
            fail(&ctx, &task, error).await;
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
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", ctx.config.timeout)),
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
        Err(e) => fail(ctx, &task, e).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
async fn cleanup(ctx: &Ctx, task: &Task) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op
        && let Err(e) = snapshot::discard_pending(ctx, snapshot).await
    {
        tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
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
    remove_volume(ctx, id).await
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나
/// 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서 다시 본다.
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
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => ctx
            .hv
            .block_resize(vm.id, disk, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
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

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
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
        return Err(format!("volume is attached to VM {:?}", vm.name));
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
            Err(e) => return Err(e.to_string()),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) {
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
                    if let Err(e) = remove_volume(ctx, volume.id).await {
                        tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume");
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
                // API가 Task를 배정했을 노드(`vm_placement`). 그것도 없으면 디스크가 없는 VM이라
                // 태그가 없으니 어느 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, &vm)));
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
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
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
}
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
            .copy_volume(
                &source_pool.spec,
                source.id,
```

을 다음으로 바꾼다:

```rust
            .copy_volume(
                source_pool,
                source.id,
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
                force_share,
                &target.spec,
                copy.id,
```

을 다음으로 바꾼다:

```rust
                force_share,
                target,
                copy.id,
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
    for (copy, target) in &copies {
        mark_ready(ctx, copy, &target.spec).await;
    }
```

을 다음으로 바꾼다:

```rust
    for (copy, target) in &copies {
        mark_ready(ctx, copy, target).await;
    }
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
            .storage
            .snapshot_delete(&pool.spec, volume.id, volume.spec.format, tag)
            .await
```

을 다음으로 바꾼다:

```rust
            .storage
            .snapshot_delete(&pool, volume.id, volume.spec.format, tag)
            .await
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
                .storage
                .snapshot_create(&pool.spec, volume.id, volume.spec.format, &tag)
                .await;
```

을 다음으로 바꾼다:

```rust
                .storage
                .snapshot_create(pool, volume.id, volume.spec.format, &tag)
                .await;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        ctx.storage
            .snapshot_revert(&pool.spec, volume.id, volume.spec.format, &tag)
            .await
```

을 다음으로 바꾼다:

```rust
        ctx.storage
            .snapshot_revert(pool, volume.id, volume.spec.format, &tag)
            .await
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
        .storage
        .volume_info(&pool.spec, volume.id, volume.spec.format)
        .await
```

을 다음으로 바꾼다:

```rust
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
```

`crates/opl-api/tests/common/mod.rs` 전체:

```rust
//! 노드 하나짜리 클러스터, Mock 하이퍼바이저, reconciler, Task 실행기 위에 API 서버를 띄우는 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    Capacity, DirStorage, DriverError, Hypervisor, MockHypervisor, Storage, VolumeInfo,
};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePool};
use opl_types::vm::Arch;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

pub const WAIT: Duration = Duration::from_secs(10);
pub const ADMIN_PASSWORD: &str = "admin-password";

pub struct Env {
    pub cluster: Arc<Cluster>,
    pub hv: Arc<MockHypervisor>,
    pub node: Uuid,
    pub base: String,
    pub dir: TempDir,
    /// API 서버의 상태. 예약 작업처럼 상태를 받는 반복 작업을 띄울 때 쓴다.
    pub state: Arc<AppState>,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    server: tokio::task::JoinHandle<()>,
}

/// NFS 풀을 마운트하는 디렉터리. 테스트는 dir 풀만 쓰므로 쓰이지 않는다.
pub const MOUNT_ROOT: &str = "/nonexistent/opl-pools";

pub fn reconciler_config(node: Uuid) -> ReconcilerConfig {
    ReconcilerConfig {
        node,
        mount_root: MOUNT_ROOT.into(),
        resync_interval: Duration::from_secs(3600),
        acpi_timeout: Duration::from_millis(300),
        retry_interval: Duration::from_millis(100),
        restart: RestartBackoff {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            max_failures: 3,
            window: Duration::from_secs(60),
        },
    }
}

/// API 서버와 같은 업로드 디렉터리를 쓰는 Task 실행기 설정. VM 상태 디렉터리는 `{dir}/state`다.
pub fn runner_config(node: Uuid, dir: &Path) -> TaskRunnerConfig {
    let mut config = TaskRunnerConfig::new(node);
    config.poll_interval = Duration::from_millis(20);
    config.upload_dir = Some(dir.join("uploads"));
    config.state_dir = Some(dir.join("state"));
    config
}

impl Env {
    pub async fn start() -> Self {
        Self::start_with(|_| {}, |_| async {}).await
    }

    /// `tweak`로 reconciler 설정을 바꾸고, `before_runner`로 Task 실행기가 뜨기 전에 저장소를 채운다.
    pub async fn start_with<F, Fut>(
        tweak: impl FnOnce(&mut ReconcilerConfig),
        before_runner: F,
    ) -> Self
    where
        F: FnOnce(Arc<Cluster>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let dir = TempDir::new().unwrap();
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let mut config = ClusterConfig::new(id, dir.path().join("cluster"));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 300,
            election_timeout_max: 600,
            ..Default::default()
        };
        let cluster = Arc::new(Cluster::open(config, router.network(id)).await.unwrap());
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(id.to_string()))]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(WAIT))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        let node = id.0;

        let mut me = Resource::new(
            "node1",
            NodeSpec {
                address: "127.0.0.1:0".into(),
                maintenance: false,
            },
        );
        me.id = node;
        me.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
            // 테스트 VM은 virt(aarch64) 머신이다.
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
        cluster.create(&me).await.unwrap();
        let admin = Resource::new(
            "admin",
            UserSpec {
                password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
                role: Role::Admin,
                session_epoch: 0,
            },
        );
        cluster.create(&admin).await.unwrap();
        seed_cluster_info(&cluster).await;
        before_runner(cluster.clone()).await;

        let hv = Arc::new(MockHypervisor::default());
        let mut rc = reconciler_config(node);
        tweak(&mut rc);
        let hv_dyn: Arc<dyn Hypervisor> = hv.clone();
        let reconciler = Reconciler::spawn(cluster.clone(), hv_dyn.clone(), rc);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT));
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv_dyn.clone(),
            reconciler.handle(),
            runner_config(node, dir.path()),
        )
        .await;
        let state = Arc::new(AppState::new(
            cluster.clone(),
            node,
            hv_dyn,
            dir.path().join("uploads"),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = opl_api::router(state.clone());
        let server = tokio::spawn(async move {
            // opld처럼 연결 주소를 넘겨 시도 제한이 클라이언트 IP로 센다.
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Env {
            cluster,
            hv,
            node,
            base,
            dir,
            state,
            reconciler: Some(reconciler),
            runner: Some(runner),
            server,
        }
    }

    /// 인증 없는 클라이언트
    pub fn anonymous(&self) -> Client {
        Client {
            base: self.base.clone(),
            http: reqwest::Client::new(),
            token: None,
        }
    }

    /// 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
    pub async fn login(&self, user: &str, password: &str) -> Client {
        login_at(&self.base, user, password).await
    }

    pub async fn admin(&self) -> Client {
        self.login("admin", ADMIN_PASSWORD).await
    }

    /// 관리자로 사용자를 만들고 그 사용자로 로그인한다.
    pub async fn user(&self, name: &str, role: &str) -> Client {
        let admin = self.admin().await;
        let (status, body) = admin
            .post(
                "/users",
                json!({ "name": name, "password": "password-1", "role": role }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        self.login(name, "password-1").await
    }

    pub async fn stop_runner(&mut self) {
        self.runner.take().unwrap().shutdown().await;
    }

    pub async fn start_runner(&mut self) {
        self.start_runner_with(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)))
            .await;
    }

    /// `storage`를 쓰는 Task 실행기를 띄운다.
    pub async fn start_runner_with(&mut self, storage: Arc<dyn Storage>) {
        self.start_runner_config(storage, |_| {}).await;
    }

    /// `storage`를 쓰고 `tweak`으로 설정을 바꾼 Task 실행기를 띄운다.
    pub async fn start_runner_config(
        &mut self,
        storage: Arc<dyn Storage>,
        tweak: impl FnOnce(&mut TaskRunnerConfig),
    ) {
        let mut config = runner_config(self.node, self.dir.path());
        tweak(&mut config);
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                storage,
                self.hv.clone(),
                self.reconciler.as_ref().unwrap().handle(),
                config,
            )
            .await,
        );
    }

    pub async fn pid(&self, id: Uuid) -> Option<u32> {
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
    pub token: Option<String>,
}

impl Client {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    pub async fn send(&self, r: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let resp = r.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, body)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).json(&body))
            .await
    }

    pub async fn put(&self, path: &str, body: Value, if_match: Option<u64>) -> (StatusCode, Value) {
        let mut r = self.req(reqwest::Method::PUT, path).json(&body);
        if let Some(rev) = if_match {
            r = r.header("if-match", rev.to_string());
        }
        self.send(r).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::DELETE, path)).await
    }

    pub async fn upload(&self, path: &str, body: Vec<u8>) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).body(body))
            .await
    }

    /// Task가 끝날 때까지 기다려 마지막 모습을 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> Value {
        let id = task["id"].as_str().expect("task id");
        let deadline = Instant::now() + WAIT;
        loop {
            let (status, task) = self.get(&format!("/tasks/{id}")).await;
            assert_eq!(status, StatusCode::OK, "{task}");
            let state = task["status"]["state"].as_str().unwrap_or("pending");
            if state == "succeeded" || state == "failed" {
                return task;
            }
            assert!(Instant::now() < deadline, "task did not finish: {task}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 동작을 요청하고 Task가 끝날 때까지 기다린다.
    pub async fn act(&self, vm: &str, action: &str) -> Value {
        let (status, task) = self
            .post(&format!("/vms/{vm}/actions/{action}"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        self.wait_task(&task).await
    }
}

/// `base`의 API에 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
pub async fn login_at(base: &str, user: &str, password: &str) -> Client {
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login as {user}");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(format!("{base}/api/v1/tokens"))
        .header("cookie", cookie)
        .json(&json!({ "name": format!("{user}-{}", Uuid::new_v4().simple()) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    Client {
        base: base.to_string(),
        http,
        token: Some(body["secret"].as_str().unwrap().to_string()),
    }
}

/// 클러스터 정보(이름 `test`, 새 CA)를 저장소에 만든다. 클러스터를 만들 때 opld가 하는 일이다.
pub async fn seed_cluster_info(cluster: &Cluster) {
    let ca = opl_peer::pki::ClusterCa::generate("test").unwrap();
    let info = Resource::new(
        "test",
        opl_types::cluster::ClusterSpec {
            ca_cert_pem: ca.cert_pem,
            ca_key_pem: ca.key_pem,
        },
    );
    cluster.create(&info).await.unwrap();
}

/// 디스크 없는 작은 VM 생성 요청
pub fn vm_request(name: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "virt",
            "nics": [ { "network": "user", "model": "virtio" } ]
        }
    })
}

/// 파일 없이 볼륨 호출을 받아 기록하는 스토리지. `pause()` 뒤 처음 오는 `snapshot_revert`나
/// `resize_volume`은 `resume()`까지 멈춘다. 그사이 테스트가 다른 요청을 끼워 넣는다.
#[derive(Default)]
pub struct FakeStorage {
    calls: std::sync::Mutex<Vec<(&'static str, Uuid)>>,
    sizes: std::sync::Mutex<std::collections::HashMap<Uuid, u64>>,
    paused: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl FakeStorage {
    pub fn calls(&self) -> Vec<(&'static str, Uuid)> {
        self.calls.lock().unwrap().clone()
    }

    /// `volume_info`가 알릴 크기
    pub fn set_size(&self, id: Uuid, size_bytes: u64) {
        self.sizes.lock().unwrap().insert(id, size_bytes);
    }

    pub fn pause(&self) {
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 멈춘 호출이 들어올 때까지 기다린다.
    pub async fn reached(&self) {
        tokio::time::timeout(WAIT, self.reached.notified())
            .await
            .expect("paused call");
    }

    pub fn resume(&self) {
        self.release.notify_one();
    }

    async fn record(&self, call: &'static str, id: Uuid) {
        self.calls.lock().unwrap().push((call, id));
        if self.paused.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait::async_trait]
impl Storage for FakeStorage {
    async fn create_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.set_size(id, size_bytes);
        Ok(PathBuf::new())
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        Err(DriverError::InvalidSpec("not supported".into()))
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        Err(DriverError::InvalidSpec("not supported".into()))
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let size = self.sizes.lock().unwrap().get(&id).copied();
        let virtual_bytes = size.ok_or(DriverError::VolumeNotFound(id))?;
        Ok(VolumeInfo {
            virtual_bytes,
            allocated_bytes: 0,
        })
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        Ok(Capacity {
            total_bytes: 1 << 40,
            available_bytes: 1 << 40,
        })
    }
    fn supports_snapshots(&self, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("create", id).await;
        Ok(())
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("revert", id).await;
        Ok(())
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("delete", id).await;
        Ok(())
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(vec![])
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.record("resize", id).await;
        self.set_size(id, size_bytes);
        Ok(())
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        src_id: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        dst_id: Uuid,
    ) -> Result<(), DriverError> {
        self.record("copy", dst_id).await;
        let size = self.sizes.lock().unwrap().get(&src_id).copied();
        self.set_size(dst_id, size.ok_or(DriverError::VolumeNotFound(src_id))?);
        Ok(())
    }
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust

use common::{ADMIN_PASSWORD, WAIT, login_at, reconciler_config, runner_config, vm_request};
```

을 다음으로 바꾼다:

```rust

use common::{
    ADMIN_PASSWORD, MOUNT_ROOT, WAIT, login_at, reconciler_config, runner_config, vm_request,
};
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config(self.id));
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
        let work = root.path().join(format!("{}-work", self.id));
```

을 다음으로 바꾼다:

```rust
            Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config(self.id));
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT));
        let work = root.path().join(format!("{}-work", self.id));
```

`crates/opl-driver/src/error.rs`에서

```rust
    ToolFailed { tool: &'static str, stderr: String },
    #[error("timed out waiting for {0}")]
```

을 다음으로 바꾼다:

```rust
    ToolFailed { tool: &'static str, stderr: String },
    /// NFS 풀이 이 노드에 마운트돼 있지 않다. opld의 풀 새로 고침이 다시 마운트하면 풀린다.
    #[error("storage pool is not mounted at {}", .0.display())]
    PoolNotMounted(std::path::PathBuf),
    #[error("timed out waiting for {0}")]
```

`crates/opl-driver/src/error.rs`에서

```rust
            ),
            DriverError::Qmp(_) | DriverError::Timeout(_) | DriverError::NotAttached(_) => true,
            DriverError::NotRunning(_)
```

을 다음으로 바꾼다:

```rust
            ),
            DriverError::Qmp(_)
            | DriverError::Timeout(_)
            | DriverError::NotAttached(_)
            | DriverError::PoolNotMounted(_) => true,
            DriverError::NotRunning(_)
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(DriverError::NotAttached(Uuid::nil()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

을 다음으로 바꾼다:

```rust
        assert!(DriverError::NotAttached(Uuid::nil()).is_transient());
        // NFS 풀을 다시 마운트하면 풀린다.
        assert!(DriverError::PoolNotMounted("/var/lib/opl/pools/p".into()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub mod mock;
pub mod net;
```

을 다음으로 바꾼다:

```rust
pub mod mock;
pub mod mounts;
pub mod net;
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use qemu::{QemuConfig, QemuHypervisor, uefi_vars_path, vm_state_dir};
pub use storage::{Capacity, DirStorage, Storage, VolumeInfo};
```

을 다음으로 바꾼다:

```rust
pub use qemu::{QemuConfig, QemuHypervisor, uefi_vars_path, vm_state_dir};
pub use storage::{Capacity, DirStorage, Storage, VolumeInfo, check_pool_root, pool_root};
```

`crates/opl-driver/src/mounts.rs`를 만든다:

```rust
//! `/proc/self/mountinfo` 해석. 마운트 지점을 `stat`하지 않고(멈춘 NFS에서는 끝나지 않는다)
//! 마운트됐는지 판단한다.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// 이 프로세스의 마운트 목록 파일. Linux에만 있다.
pub const MOUNTINFO: &str = "/proc/self/mountinfo";

/// mountinfo에서 NFS(`nfs`, `nfs4`) 마운트 지점들
pub fn nfs_mount_points(mountinfo: &str) -> BTreeSet<PathBuf> {
    mountinfo
        .lines()
        .filter_map(|line| {
            // `id parent major:minor root 마운트지점 옵션 [선택 필드...] - 종류 원본 슈퍼블록옵션`
            let (left, right) = line.split_once(" - ")?;
            let point = left.split(' ').nth(4)?;
            let fstype = right.split(' ').next()?;
            matches!(fstype, "nfs" | "nfs4").then(|| PathBuf::from(unescape(point)))
        })
        .collect()
}

/// mountinfo가 `\040`처럼 8진수로 적은 공백·탭·줄바꿈·역슬래시를 되돌린다.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 4);
        match code.and_then(|c| u8::from_str_radix(c, 8).ok()) {
            Some(b) => {
                out.push(b as char);
                rest = &rest[i + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nfs_mount_points_in_mountinfo() {
        let info = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 22 0:45 / /var/lib/opl/pools/a rw,relatime shared:20 - nfs4 nas:/srv/vm rw,vers=4.2
41 22 0:46 / /var/lib/opl/pools/b rw,relatime - nfs [fd00::5]:/srv/vm rw,vers=3
42 22 0:47 / /mnt/with\\040space rw - nfs4 nas:/x rw
43 22 0:48 / /mnt/tmp rw - tmpfs tmpfs rw
";
        let points = nfs_mount_points(info);
        assert_eq!(
            points,
            BTreeSet::from([
                PathBuf::from("/var/lib/opl/pools/a"),
                PathBuf::from("/var/lib/opl/pools/b"),
                PathBuf::from("/mnt/with space"),
            ])
        );
        assert_eq!(unescape("a\\134b\\011c\\x"), "a\\b\tc\\x");
    }
}
```

`crates/opl-driver/src/storage.rs` 전체:

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;
use crate::mounts::{MOUNTINFO, nfs_mount_points};

/// 올린 이미지를 검사하는 `qemu-img info`가 멈추면(이상한 extent 등) 무한정 기다리지 않는다.
const INFO_TIMEOUT: Duration = Duration::from_secs(60);

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

    /// VM이 실행 중이어도 조회할 수 있다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError>;

    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError>;

    /// 이 백엔드가 `format` 볼륨의 스냅샷을 지원하는지. 지원하지 않는 형식에 스냅샷 메서드를
    /// 부르면 `InvalidSpec`.
    fn supports_snapshots(&self, format: ImageFormat) -> bool;

    /// 정지한 볼륨에 스냅샷 `tag`를 만든다. 같은 태그가 이미 있으면 `InvalidSpec`.
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

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

    /// 볼륨을 같은 형식의 새 볼륨 `dst_id`로 복사한다. `tag`가 있으면 그 스냅샷 때의 내용을,
    /// 없으면 지금 내용을 복사한다. `force_share`는 실행 중인 VM이 쥔 볼륨의 스냅샷을 읽을 때만
    /// 쓴다(스냅샷 데이터는 바뀌지 않는다). 대상이 이미 있으면 `VolumeExists`.
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
    ) -> Result<(), DriverError>;
}

/// 풀의 볼륨 파일이 놓이는 디렉터리. `dir` 풀은 그 경로, `nfs` 풀은 이 노드의 마운트 지점
/// `{mount_root}/{풀 id}`다.
pub fn pool_root(pool: &StoragePool, mount_root: &Path) -> PathBuf {
    match &pool.spec.backend {
        PoolBackend::Dir { path } => path.clone(),
        PoolBackend::Nfs { .. } => mount_root.join(pool.id.to_string()),
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
        let out = tokio::process::Command::new(&self.qemu_img)
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

    /// 있는 볼륨의 경로. 없으면 `VolumeNotFound`.
    fn existing_path(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = self.file_path(pool, id, format)?;
        if !path.exists() {
            return Err(self.not_found(pool, id));
        }
        Ok(path)
    }

    /// 스냅샷을 다룰 볼륨의 경로. 형식이 스냅샷을 지원해야 한다.
    fn snapshot_path(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        if !self.supports_snapshots(format) {
            return Err(DriverError::InvalidSpec(format!(
                "{} volumes do not support snapshots",
                fmt_name(format)
            )));
        }
        self.existing_path(pool, id, format)
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
        self.qemu_img(&args).await?;
        match std::fs::hard_link(&tmp.0, path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(DriverError::VolumeExists(id))
            }
            other => Ok(other?),
        }
    }
}

/// drop되면 파일을 지운다. 실패하거나 취소돼도 임시 파일이 남지 않게 한다.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
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
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.to_string();
        self.write_new(
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
        if path.exists() {
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
        self.write_new(id, &path, &before, &[]).await?;
        Ok(path)
    }

    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        require_absolute(src)?;
        check_source_header(src, src_format).await?;
        let src = path_str(src)?;
        let args = ["info", "--output=json", "-f", src_fmt_name(src_format), src];
        // 시간이 지나 future가 버려지면 kill_on_drop이 qemu-img를 끝낸다.
        let out = tokio::time::timeout(INFO_TIMEOUT, self.qemu_img(&args))
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

    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        match std::fs::remove_file(self.file_path(pool, id, format)?) {
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
        let path = self.existing_path(pool, id, format)?;
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
        Ok(Capacity {
            total_bytes: fs4::total_space(&path)?,
            available_bytes: fs4::available_space(&path)?,
        })
    }

    /// 내부 스냅샷은 qcow2만 담을 수 있다.
    fn supports_snapshots(&self, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
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
        let path = self.snapshot_path(pool, id, format)?;
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
        let path = self.snapshot_path(pool, id, format)?;
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
        let path = self.snapshot_path(pool, id, format)?;
        self.tags_at(&path, format).await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let path = self.existing_path(pool, id, format)?;
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
    ) -> Result<(), DriverError> {
        let src = match tag {
            Some(_) => self.snapshot_path(src_pool, src_id, format)?,
            None => self.existing_path(src_pool, src_id, format)?,
        };
        let dst = self.file_path(dst_pool, dst_id, format)?;
        if dst.exists() {
            return Err(DriverError::VolumeExists(dst_id));
        }
        // `-l`의 값은 옵션 문자열이므로 `,`를 `,,`로 쓴다.
        let snapshot = tag.map(|t| format!("snapshot.name={}", t.replace(',', ",,")));
        let mut before = vec!["convert", "-q", "-f", fmt_name(format)];
        if force_share {
            before.push("-U");
        }
        if let Some(s) = &snapshot {
            before.extend(["-l", s.as_str()]);
        }
        before.extend(["-O", fmt_name(format), path_str(&src)?]);
        self.write_new(dst_id, &dst, &before, &[]).await
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

    #[tokio::test]
    async fn unmounted_nfs_pool_is_refused_everywhere() {
        let tools = tempfile::tempdir().unwrap();
        let mnt = tempfile::tempdir().unwrap();
        let p = nfs_pool();
        // 마운트되지 않은 빈 마운트 지점(mountinfo에 없다). 여기에 볼륨을 만들면 로컬 디스크에 쌓인다.
        let root = mnt.path().join(p.id.to_string());
        std::fs::create_dir(&root).unwrap();
        let storage = DirStorage::new(fake_tool(tools.path(), "echo new > \"$last\""), mnt.path());
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
                .copy_volume(&local, id, f, None, false, &p, id)
                .await
                .unwrap_err(),
        );
        not_mounted(
            storage
                .copy_volume(&p, id, f, None, false, &local, id)
                .await
                .unwrap_err(),
        );
        // 마운트 지점이 아예 없어도 같다.
        std::fs::remove_dir(&root).unwrap();
        not_mounted(storage.capacity(&p).await.unwrap_err());
        assert_eq!(pool_files(mnt.path()), Vec::<String>::new());
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
        assert!(storage.supports_snapshots(qcow2));
        assert!(!storage.supports_snapshots(raw));

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
            &format!("printf '%s\\n' \"$@\" > {args:?}\necho copy > \"$last\""),
        ));

        storage
            .copy_volume(&src_pool, src, qcow2, Some("a,b"), true, &dst_pool, dst)
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
            .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, dst2)
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
    }

    #[tokio::test]
    async fn copy_volume_checks_source_and_never_overwrites() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = dir_storage("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeNotFound(id)) if id == src),
            "{res:?}"
        );

        std::fs::write(volume_path(&p, src, raw), b"").unwrap();
        // raw 볼륨에는 스냅샷이 없다.
        let res = storage
            .copy_volume(&p, src, raw, Some("t"), false, &p, dst)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");

        let dst_path = volume_path(&p, dst, raw);
        std::fs::write(&dst_path, b"precious").unwrap();
        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeExists(id)) if id == dst),
            "{res:?}"
        );
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"precious");

        let rel = pool("pools/local");
        let res = storage
            .copy_volume(&p, src, raw, None, false, &rel, dst)
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
        // 출력 파일을 반쯤 쓰고 실패한다.
        let storage = dir_storage(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho broken >&2\nexit 1",
        ));

        let err = storage
            .copy_volume(&p, src, ImageFormat::Raw, None, false, &p, Uuid::now_v7())
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

`crates/opl-driver/tests/dir_storage.rs` 전체:

```rust
//! 실제 qemu-img를 쓰는 스토리지 통합 테스트. `cargo test -p opl-driver --features storage-it`
#![cfg(feature = "storage-it")]

use std::path::{Path, PathBuf};

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

fn pool(dir: &Path) -> StoragePool {
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

fn volume_path(pool: &StoragePool, id: Uuid, format: ImageFormat) -> PathBuf {
    DirStorage::volume_path(pool, Path::new(MNT), id, format)
}

fn qemu_img(args: &[&str]) {
    let status = std::process::Command::new("qemu-img")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

#[tokio::test]
async fn create_inspect_and_delete_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();

    let path = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(path, volume_path(&pool, id, ImageFormat::Qcow2));
    let info = storage
        .volume_info(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(
        info.allocated_bytes < 64 * MIB,
        "qcow2 should be thin: {info:?}"
    );

    let again = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(matches!(again, Err(DriverError::VolumeExists(_))));

    storage
        .delete_volume(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert!(!path.exists());
    let gone = storage.volume_info(&pool, id, ImageFormat::Qcow2).await;
    assert!(matches!(gone, Err(DriverError::VolumeNotFound(_))));
}

#[tokio::test]
async fn imports_vmdk_preserving_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let pool_dir = tmp.path().join("pool");
    std::fs::create_dir(&pool_dir).unwrap();
    let pool = pool(&pool_dir);

    // VMware에서 가져온 디스크를 흉내 낸다: 알려진 패턴의 raw → vmdk
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

    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, ImageFormat::Qcow2)
        .await
        .unwrap();

    let back = tmp.path().join("back.raw");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "qcow2",
        "-O",
        "raw",
        path.to_str().unwrap(),
        back.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read(&back).unwrap(), pattern);
}

#[tokio::test]
async fn import_source_must_be_a_single_file() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = DirStorage::new("qemu-img", MNT);
    let raw = tmp.path().join("disk.raw");
    std::fs::write(&raw, vec![0u8; MIB as usize]).unwrap();
    let vmdk = tmp.path().join("disk.vmdk");
    let raw_s = raw.to_str().unwrap();
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw_s,
        vmdk.to_str().unwrap(),
    ]);
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    storage
        .check_import_source(&raw, SourceFormat::Raw)
        .await
        .unwrap();

    // monolithicFlat 디스크립터는 호스트의 임의 파일을 extent로 가리킬 수 있다.
    let desc = tmp.path().join("desc.vmdk");
    std::fs::write(
        &desc,
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
             createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"{raw_s}\" 0\n"
        ),
    )
    .unwrap();
    let err = storage
        .check_import_source(&desc, SourceFormat::Vmdk)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DriverError::InvalidSpec(ref m) if m.contains("descriptor")),
        "{err:?}"
    );

    // backing file도 다른 파일이다.
    let overlay = tmp.path().join("overlay.qcow2");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-b",
        raw_s,
        "-F",
        "raw",
        overlay.to_str().unwrap(),
    ]);
    let err = storage
        .check_import_source(&overlay, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    // qcow2 외부 데이터 파일도 마찬가지다.
    let external = tmp.path().join("external.qcow2");
    let data_file = format!("data_file={raw_s},data_file_raw=on");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-o",
        &data_file,
        external.to_str().unwrap(),
        "1M",
    ]);
    let err = storage
        .check_import_source(&external, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
}

#[tokio::test]
async fn failed_import_leaves_no_partial_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();

    let err = storage
        .import_volume(
            &pool,
            id,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            ImageFormat::Qcow2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(!volume_path(&pool, id, ImageFormat::Qcow2).exists());
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, cmd: &str) -> bool {
    qemu_io_fmt(path, ImageFormat::Qcow2, cmd)
}

fn qemu_io_fmt(path: &Path, format: ImageFormat, cmd: &str) -> bool {
    let format = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    std::process::Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn snapshot_revert_restores_contents_and_delete_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    // 경로의 쉼표는 `--image-opts` 옵션 문자열에서 `,,`로 써야 한다.
    let dir = tmp.path().join("a,b");
    std::fs::create_dir(&dir).unwrap();
    let pool = pool(&dir);
    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();
    let qcow2 = ImageFormat::Qcow2;
    let path = storage
        .create_volume(&pool, id, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(qcow2));
    assert!(!storage.supports_snapshots(ImageFormat::Raw));

    assert!(qemu_io(&path, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage
        .snapshot_create(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, qcow2).await.unwrap(),
        vec![tag.clone()]
    );
    // 같은 태그를 또 만들지 않는다(qemu-img는 이름이 겹치는 스냅샷을 허용한다).
    let dup = storage.snapshot_create(&pool, id, qcow2, &tag).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    assert!(qemu_io(&path, "write -P 0xcd 0 64k"));
    storage
        .snapshot_revert(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert!(qemu_io(&path, "read -P 0xab 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, qcow2, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");

    storage
        .snapshot_delete(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert!(
        storage
            .snapshot_tags(&pool, id, qcow2)
            .await
            .unwrap()
            .is_empty()
    );
    // 태그가 이미 없어도 성공한다(다시 시도해도 된다).
    storage
        .snapshot_delete(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    // 스냅샷을 지워도 현재 내용은 그대로다.
    assert!(qemu_io(&path, "read -P 0xab 0 64k"));
}

#[tokio::test]
async fn resize_grows_volume_with_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img", MNT);

    for format in [ImageFormat::Qcow2, ImageFormat::Raw] {
        let id = Uuid::now_v7();
        storage
            .create_volume(&pool, id, 64 * MIB, format)
            .await
            .unwrap();
        if format == ImageFormat::Qcow2 {
            // 내부 스냅샷이 있어도 크기를 늘릴 수 있다.
            storage
                .snapshot_create(&pool, id, format, "s")
                .await
                .unwrap();
        }
        storage
            .resize_volume(&pool, id, format, 128 * MIB)
            .await
            .unwrap();
        let info = storage.volume_info(&pool, id, format).await.unwrap();
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");

        // 줄이기는 하지 않는다.
        let err = storage
            .resize_volume(&pool, id, format, 32 * MIB)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        let info = storage.volume_info(&pool, id, format).await.unwrap();
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");
    }
}

#[tokio::test]
async fn copy_volume_copies_snapshot_or_current_contents() {
    let tmp = tempfile::tempdir().unwrap();
    // 다른 풀로 복사한다. 경로의 쉼표는 옵션 문자열에서 `,,`로 써야 한다.
    let (src_dir, dst_dir) = (tmp.path().join("src"), tmp.path().join("d,st"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&dst_dir).unwrap();
    let (src_pool, dst_pool) = (pool(&src_dir), pool(&dst_dir));
    let storage = DirStorage::new("qemu-img", MNT);
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storage
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage
        .snapshot_create(&src_pool, src, qcow2, &tag)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xcd 0 64k"));

    // 스냅샷 때의 내용. -U(실행 중인 VM의 스냅샷에서 복사할 때)도 받아들인다.
    for force_share in [false, true] {
        let dst = Uuid::now_v7();
        storage
            .copy_volume(
                &src_pool,
                src,
                qcow2,
                Some(&tag),
                force_share,
                &dst_pool,
                dst,
            )
            .await
            .unwrap();
        let dst_path = volume_path(&dst_pool, dst, qcow2);
        assert!(qemu_io(&dst_path, "read -P 0xab 0 64k"), "{force_share}");
        let info = storage.volume_info(&dst_pool, dst, qcow2).await.unwrap();
        assert_eq!(info.virtual_bytes, 64 * MIB);
        // 사본에는 스냅샷이 따라오지 않는다.
        assert!(
            storage
                .snapshot_tags(&dst_pool, dst, qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // 태그가 없으면 지금 내용
    let now = Uuid::now_v7();
    storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now)
        .await
        .unwrap();
    assert!(qemu_io(
        &volume_path(&dst_pool, now, qcow2),
        "read -P 0xcd 0 64k"
    ));
    // 대상이 있으면 덮어쓰지 않는다.
    let err = storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");

    // 없는 태그는 실패하고 파일을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .copy_volume(
            &src_pool,
            src,
            qcow2,
            Some("gone"),
            false,
            &dst_pool,
            missing,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert_eq!(std::fs::read_dir(&dst_dir).unwrap().count(), 3);
    assert!(!volume_path(&dst_pool, missing, qcow2).exists());

    // raw도 같은 형식으로 복사한다.
    let raw = ImageFormat::Raw;
    let src_raw = Uuid::now_v7();
    let src_raw_path = storage
        .create_volume(&src_pool, src_raw, 64 * MIB, raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(&src_raw_path, raw, "write -P 0x5a 0 64k"));
    let dst_raw = Uuid::now_v7();
    storage
        .copy_volume(&src_pool, src_raw, raw, None, false, &dst_pool, dst_raw)
        .await
        .unwrap();
    let dst_raw_path = volume_path(&dst_pool, dst_raw, raw);
    assert!(qemu_io_fmt(&dst_raw_path, raw, "read -P 0x5a 0 64k"));
    let info = storage.volume_info(&dst_pool, dst_raw, raw).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
}
```

`crates/opl-reconciler/src/config.rs`에서

```rust
use std::time::Duration;
```

을 다음으로 바꾼다:

```rust
use std::path::PathBuf;
use std::time::Duration;
```

`crates/opl-reconciler/src/config.rs`에서

```rust
    pub node: Uuid,
    /// 이벤트를 놓쳤더라도 원하는 상태와 실제 상태를 다시 맞추는 주기
```

을 다음으로 바꾼다:

```rust
    pub node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 디스크 경로를 만들 때 쓴다.
    pub mount_root: PathBuf,
    /// 이벤트를 놓쳤더라도 원하는 상태와 실제 상태를 다시 맞추는 주기
```

`crates/opl-reconciler/src/config.rs`에서

```rust
impl ReconcilerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            resync_interval: Duration::from_secs(30),
```

을 다음으로 바꾼다:

```rust
impl ReconcilerConfig {
    pub fn new(node: Uuid, mount_root: impl Into<PathBuf>) -> Self {
        Self {
            node,
            mount_root: mount_root.into(),
            resync_interval: Duration::from_secs(30),
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.
```

을 다음으로 바꾼다:

```rust
//! VM spec의 이름 참조(Volume, Network)를 풀어 드라이버가 받는 `VmRuntimeSpec`을 만든다.

use std::path::Path;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
```

을 다음으로 바꾼다:

```rust
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id다.
pub fn runtime_spec(cluster: &Cluster, vm: &Vm, node: Uuid) -> Result<VmRuntimeSpec, ResolveError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

을 다음으로 바꾼다:

```rust

/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id이고
/// `mount_root`는 이 노드가 NFS 풀을 마운트하는 디렉터리다.
pub fn runtime_spec(
    cluster: &Cluster,
    vm: &Vm,
    node: Uuid,
    mount_root: &Path,
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        // NFS 풀은 아직 마운트하지 않는다.
        if (!pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node))
            || matches!(pool.spec.backend, PoolBackend::Nfs { .. })
        {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        disks.push(RuntimeDisk {
            path: DirStorage::volume_path(&pool.spec, volume.id, volume.spec.format),
            format: volume.spec.format,
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        disks.push(RuntimeDisk {
            path: DirStorage::volume_path(&pool, mount_root, volume.id, volume.spec.format),
            format: volume.spec.format,
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
    async fn start(&mut self, vm: &Vm) -> Result<bool, StepError> {
        let spec = match runtime_spec(&self.cluster, vm, self.config.node) {
            Ok(spec) => spec,
```

을 다음으로 바꾼다:

```rust
    async fn start(&mut self, vm: &Vm) -> Result<bool, StepError> {
        let spec = match runtime_spec(&self.cluster, vm, self.config.node, &self.config.mount_root)
        {
            Ok(spec) => spec,
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let spec = runtime_spec(&self.cluster, vm, self.config.node).map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
```

을 다음으로 바꾼다:

```rust
    async fn reattach(&self, vm: &Vm) -> Result<(), String> {
        let spec = runtime_spec(&self.cluster, vm, self.config.node, &self.config.mount_root)
            .map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust
        node,
        resync_interval: Duration::from_secs(3600),
```

을 다음으로 바꾼다:

```rust
        node,
        // 테스트는 dir 풀만 쓰므로 NFS 마운트 디렉터리는 쓰이지 않는다.
        mount_root: "/nonexistent/opl-pools".into(),
        resync_interval: Duration::from_secs(3600),
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
mod common;
```

을 다음으로 바꾼다:

```rust
mod common;

use std::path::Path;
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use uuid::Uuid;

/// NFS 풀을 마운트하는 디렉터리
const MNT: &str = "/var/lib/opl/pools";
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

    let spec = runtime_spec(&cluster, &vm, node).unwrap();
    assert_eq!(spec.id, vm.id);
```

을 다음으로 바꾼다:

```rust

    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(spec.id, vm.id);
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
#[tokio::test]
async fn resolves_bridge_networks_to_taps() {
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn resolves_nfs_volumes_under_mount_point() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let nas: StoragePool = Resource::new(
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
    );
    cluster.create(&nas).await.unwrap();
    let root = volume("root", "nas");
    cluster.create(&root).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("root"));
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(
        spec.disks[0].path,
        Path::new(MNT)
            .join(nas.id.to_string())
            .join(format!("{}.qcow2", root.id))
    );
}

#[tokio::test]
async fn resolves_bridge_networks_to_taps() {
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    vm.spec.nics.push(nic("lan", Some(MacAddr::generate())));
    let spec = runtime_spec(&cluster, &vm, node).unwrap();
    assert_eq!(spec.nics[0].backend, NicBackend::User);
```

을 다음으로 바꾼다:

```rust
    vm.spec.nics.push(nic("lan", Some(MacAddr::generate())));
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(spec.nics[0].backend, NicBackend::User);
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
        vm.spec.nics = nics;
        runtime_spec(&cluster, &vm, node).unwrap_err()
    };
```

을 다음으로 바꾼다:

```rust
        vm.spec.nics = nics;
        runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap_err()
    };
```

`crates/opld/src/member.rs`에서

```rust
        };
        let mut reconciler_config = ReconcilerConfig::new(node);
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

을 다음으로 바꾼다:

```rust
        };
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root);
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, mount_root));
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 414개, 실패 0

Run: `cargo test -p opl-driver --features storage-it`
Expected: PASS

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "refactor(storage): 풀 루트를 백엔드별로 정하고 저장소가 풀 리소스를 받는다"
```

---

### Task 3: opld: 노드별 풀 상태와 NFS 마운트

새 `crates/opld/src/pools.rs`의 `PoolMounter`(`spawn(cluster, mount_root, refresh)`/`shutdown`, 마운트는 그대로 둔다)가 `[pools] refresh_secs`(기본 15, 0 거절) 간격과 풀 목록이 바뀔 때 곧바로, 이 노드가 쓸 수 있는 풀마다 상태를 만든다. 명령은 `MountOps`(mountinfo·mount·umount·check) 뒤에 있고 실제는 `SystemMount`(`kill_on_drop`, `rustix::fs::statvfs`, `.opl-check-{노드 id}` 쓰기 확인). Dir은 확인(10초 제한), Nfs는 마운트 지점이 마운트돼 있지 않으면 `mount_args`(`-t nfs -o nosuid,nodev,noexec[,옵션] server:export dir`, IPv6는 대괄호)로 뒤에서 마운트(30초 제한, 풀마다 하나, 실패하면 15초부터 두 배 최대 5분 백오프) 뒤 확인. 언마운트 중인 풀은 준비 안 됨. 쓸 수 없게 된 NFS 풀은 이 노드에서 그 풀을 쓰는 VM이 없을 때 뒤에서 `umount`(`-f` 없음)하고 빈 디렉터리를 지운다. 멈춘 확인은 건너뛴다. 상태는 `pool_id`를 적고 바뀐 경우만 `update_status_checked`로 쓴다. macOS는 NFS를 마운트하지 않고 이유를 남긴다. 가짜 `MountOps`로 테스트한다.

**Files:**
- Create: `crates/opld/src/pools.rs`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/Cargo.toml`에서

```toml
anyhow.workspace = true
axum.workspace = true
```

을 다음으로 바꾼다:

```toml
anyhow.workspace = true
async-trait.workspace = true
axum.workspace = true
```

`crates/opld/Cargo.toml`에서

```toml
[dev-dependencies]
async-trait.workspace = true
oplctl.workspace = true
```

을 다음으로 바꾼다:

```toml
[dev-dependencies]
oplctl.workspace = true
```

`crates/opld/src/bootstrap.rs`에서

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
```

을 다음으로 바꾼다:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
```

`crates/opld/src/bootstrap.rs`에서

```rust
    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
```

을 다음으로 바꾼다:

```rust
    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
```

`crates/opld/src/config.rs`에서

```rust
    pub janitor: JanitorSection,
}
```

을 다음으로 바꾼다:

```rust
    pub janitor: JanitorSection,
    #[serde(default)]
    pub pools: PoolsSection,
}
```

`crates/opld/src/config.rs`에서

```rust

fn default_listen() -> SocketAddr {
```

을 다음으로 바꾼다:

```rust

/// 노드별 풀 상태 새로 고침(NFS 마운트 포함)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolsSection {
    /// 풀 상태를 확인하는 간격(초)
    #[serde(default = "default_pools_refresh_secs")]
    pub refresh_secs: u64,
}

impl Default for PoolsSection {
    fn default() -> Self {
        Self {
            refresh_secs: default_pools_refresh_secs(),
        }
    }
}

impl PoolsSection {
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_secs)
    }
}

fn default_listen() -> SocketAddr {
```

`crates/opld/src/config.rs`에서

```rust
    1000
}
```

을 다음으로 바꾼다:

```rust
    1000
}
fn default_pools_refresh_secs() -> u64 {
    15
}
```

`crates/opld/src/config.rs`에서

```rust
            anyhow::bail!("janitor.task_max_age_days must be at most {MAX_TASK_AGE_DAYS}");
        }
```

을 다음으로 바꾼다:

```rust
            anyhow::bail!("janitor.task_max_age_days must be at most {MAX_TASK_AGE_DAYS}");
        }
        if self.pools.refresh_secs == 0 {
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
```

`crates/opld/src/config.rs`에서

```rust
        assert_eq!(janitor.finished_grace, Duration::from_secs(600));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
```

을 다음으로 바꾼다:

```rust
        assert_eq!(janitor.finished_grace, Duration::from_secs(600));
        assert_eq!(c.pools.refresh(), Duration::from_secs(15));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn pools_section_sets_the_refresh_interval_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 2");
        assert!(c.validate().is_ok());
        assert_eq!(c.pools.refresh(), Duration::from_secs(2));
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 0");
        assert!(c.validate().is_err());
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh = 1")
                .is_err()
        );
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

`crates/opld/src/main.rs`에서

```rust
mod member;
mod web;
```

을 다음으로 바꾼다:

```rust
mod member;
mod pools;
mod web;
```

`crates/opld/src/member.rs`에서

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리.
```

을 다음으로 바꾼다:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, 노드별 풀 상태.
```

`crates/opld/src/member.rs`에서

```rust
use crate::host::Host;
```

을 다음으로 바꾼다:

```rust
use crate::host::Host;
use crate::pools::PoolMounter;
```

`crates/opld/src/member.rs`에서

```rust
    janitor: ClusterJanitor,
    register: JoinHandle<()>,
```

을 다음으로 바꾼다:

```rust
    janitor: ClusterJanitor,
    pools: PoolMounter,
    register: JoinHandle<()>,
```

`crates/opld/src/member.rs`에서

```rust
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, mount_root));
```

을 다음으로 바꾼다:

```rust
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(cluster.clone(), mount_root.clone(), config.pools.refresh());
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, mount_root));
```

`crates/opld/src/member.rs`에서

```rust
            janitor,
            register,
```

을 다음으로 바꾼다:

```rust
            janitor,
            pools,
            register,
```

`crates/opld/src/member.rs`에서

```rust
        self.janitor.shutdown().await;
        self.runner.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.janitor.shutdown().await;
        self.pools.shutdown().await;
        self.runner.shutdown().await;
```

`crates/opld/src/pools.rs`를 만든다:

```rust
//! 노드별 풀 상태. 이 노드가 쓸 수 있는 풀마다 준비 여부와 용량을 `NodeStatus.pools`에 적는다.
//! NFS 풀은 `{data_dir}/pools/{풀 id}`에 마운트하고, 이 노드가 쓸 수 없게 된 NFS 풀은 언마운트한다.
//! opld가 내려가도 마운트는 그대로 둔다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Capacity;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
use opl_types::vm::{VmPhase, VmSpec};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// `mount`·`umount`를 기다리는 시간
pub const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);
/// 쓰기 확인과 용량 읽기를 기다리는 시간. NFS 서버가 멈추면 이 작업이 멈춘다.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const NFS_UNSUPPORTED: &str = "nfs pools are not supported on macOS";
const NFS_NOT_RESPONDING: &str = "nfs server not responding";
const DIR_NOT_RESPONDING: &str = "pool directory not responding";
const MOUNT_IN_PROGRESS: &str = "mount in progress";
/// 마운트에 실패한 뒤 다시 시도하기까지 처음 기다리는 시간. 실패할 때마다 두 배로 늘린다.
const MOUNT_BACKOFF_INITIAL: Duration = Duration::from_secs(15);
const MOUNT_BACKOFF_MAX: Duration = Duration::from_secs(300);

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
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(
            || format!("{program} failed: {}", out.status),
            str::to_string,
        ))
}

/// `mount -t nfs -o nosuid,nodev,noexec[,options] server:export dir`의 인자. 풀에는 이미지 파일만
/// 두므로 setuid·장치 파일·실행을 늘 막고, 사용자 옵션은 그 뒤에 붙인다. IPv6 서버는 `[...]`로 감싼다.
pub fn mount_args(server: &str, export: &str, options: Option<&str>, dir: &Path) -> Vec<OsString> {
    let source = if server.contains(':') {
        format!("[{server}]:{export}")
    } else {
        format!("{server}:{export}")
    };
    let mut opts = String::from("nosuid,nodev,noexec");
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
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            // 마운트·언마운트가 끝나면 곧바로 결과를 적는다.
            () = done.notified() => {}
            _ = interval.tick() => {}
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

/// 한 번의 새로 고침에 필요한 것
struct Pools {
    cluster: Arc<Cluster>,
    node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(`{data_dir}/pools`)
    mount_root: PathBuf,
    ops: Arc<dyn MountOps>,
    /// 이 OS가 NFS 마운트를 지원하는지(Linux)
    nfs_supported: bool,
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
        for pool in &usable {
            match self.step(pool, &mounted, mount_results.remove(&pool.id)) {
                Step::State(state) => {
                    states.insert(pool.name.clone(), state);
                }
                Step::Check(dir, not_responding) => checks.push((*pool, dir, not_responding)),
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

    /// 바뀌었으면 `NodeStatus.pools`에 쓴다. 다른 쪽이 그 사이 status를 썼으면 다시 읽고 쓴다.
    async fn write(&self, pools: BTreeMap<String, PoolState>) -> anyhow::Result<()> {
        loop {
            let Some(node) = self.cluster.get::<NodeSpec>(self.node)? else {
                return Ok(());
            };
            if node.status.pools == pools {
                return Ok(());
            }
            let status = NodeStatus {
                pools: pools.clone(),
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use opl_types::resource::Resource;
    use opl_types::vm::VmStatus;

    use super::*;
    use crate::bootstrap::tests::single_node;

    #[test]
    fn builds_mount_arguments() {
        let dir = Path::new("/var/lib/opl/pools/p1");
        // 사용자 옵션이 있든 없든 nosuid,nodev,noexec를 먼저 붙인다.
        assert_eq!(
            mount_args("nas.lan", "/srv/vm", None, dir),
            [
                "-t",
                "nfs",
                "-o",
                "nosuid,nodev,noexec",
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
                "nosuid,nodev,noexec,vers=4.2,hard",
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

        fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity> {
            *self.checks.lock().unwrap() += 1;
            while self.hang.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(probe.starts_with(".opl-check-"));
            if !dir.is_dir() {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            Ok(Capacity {
                total_bytes: 100,
                available_bytes: 40,
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
            total_bytes: 100,
            available_bytes: 40,
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
                "mount -t nfs -o nosuid,nodev,noexec,vers=4.2 nas:/srv/vm {}",
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
        tokio::time::sleep(Duration::from_millis(50)).await;
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
Expected: PASS 424개, 실패 0

Run: `cargo test -p opld --bin opld pools`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): 노드별 풀 상태를 기록하고 NFS 풀을 마운트한다"
```

---

### Task 4: 배치와 볼륨 Task 노드가 풀 상태와 공유 여부를 보기

`pool_listed_on`, `pool_state_on`(`pool_id`가 맞는 상태만), `pool_usable_on`(노드 목록이 허용하고, NFS는 `ready: true`, Dir은 명시적 `ready: false`가 아닐 것), `pool_error_on`. 스케줄러는 디스크 풀을 쓸 수 있고 공유가 아닌 풀은 볼륨이 있는 노드만 고른다. `pool_node`/`volume_node`는 이 노드 → 쓸 수 있는 온라인 노드(id 순), 없으면 409 `pool_unavailable`. 가져오기 확인·복제 대상 풀은 `check_pool_on`(목록에 없으면 `pool_not_on_node`, 준비 안 됨이면 `pool_unavailable`), 복제 Task는 실행할 때 다시 확인. `check_local`은 `is_shared`. NFS 풀 만들기를 허용하고(`shared` 키가 없으면 true, false면 400 `invalid_request`), 풀이 바뀌면 마운터가 곧바로 새로 고친다. reconciler: 풀을 쓸 수 없거나 NFS가 마운트되지 않았으면 `ResolveError::PoolUnavailable`로 VM을 끄지 않고 `retry_interval`마다 다시 시도한다. 노드 상태 쓰기(`register_node`·브리지 새로 고침·오프라인 표시·감시)는 `update_node_status`로 리비전을 확인하고 `pools`를 보존한다. 시작할 때 정리하지 못한 NFS 고아 볼륨은 `orphan_retry`(60초)마다 다시 시도한다.

**Files:**
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/clones.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/import_check.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/schedules.rs`
- Modify: `crates/opl-api/tests/snapshot_disks.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/tests/tasks.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opl-api/tests/volumes.rs`
- Modify: `crates/opl-peer/src/monitor.rs`
- Modify: `crates/opl-reconciler/src/resolve.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/pools.rs`
- Modify: `crates/opld/tests/common/mod.rs`
- Modify: `crates/opld/tests/e2e.rs`
- Modify: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust
    principal: Principal,
    Json(req): Json<CreateRequest<StoragePoolSpec>>,
) -> Result<(StatusCode, Json<StoragePool>), ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    check_name(&req.name)?;
    req.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS 풀 마운트와 배치는 아직 없다.
    if matches!(req.spec.backend, PoolBackend::Nfs { .. }) {
        return Err(ApiError::bad_request("nfs pools are not supported yet"));
    }
    for node in &req.spec.nodes {
        if state.cluster.get::<NodeSpec>(*node)?.is_none() {
```

을 다음으로 바꾼다:

```rust
    principal: Principal,
    Json(req): Json<CreateRequest<serde_json::Value>>,
) -> Result<(StatusCode, Json<StoragePool>), ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    check_name(&req.name)?;
    // `shared`를 적었는지 알아야 하므로 spec은 JSON 값으로 받아 읽는다.
    let shared_given = req.spec.get("shared").is_some();
    let mut spec: StoragePoolSpec = serde_json::from_value(req.spec)
        .map_err(|e| ApiError::bad_request(format!("spec: {e}")))?;
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    if matches!(spec.backend, PoolBackend::Nfs { .. }) {
        if shared_given && !spec.shared {
            return Err(ApiError::bad_request("nfs pools are always shared"));
        }
        spec.shared = true;
    }
    for node in &spec.nodes {
        if state.cluster.get::<NodeSpec>(*node)?.is_none() {
```

`crates/opl-api/src/resources.rs`에서

```rust
    }
    let mut pool = Resource::new(req.name, req.spec);
    pool.labels = req.labels;
```

을 다음으로 바꾼다:

```rust
    }
    let mut pool = Resource::new(req.name, spec);
    pool.labels = req.labels;
```

`crates/opl-api/src/scheduler.rs` 전체:

```rust
//! `spec.node`가 비어 있는 VM을 시작할 때 노드를 고른다(설계 3.3).

use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{Network, NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, is_shared, pool_usable_on,
};
use opl_types::vm::{PowerState, Vm, VmSpec};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error(
    "no node can run this VM (needs an online node that is not in maintenance, has the architecture of its machine type, can reach its storage pools and has the bridges of its networks)"
)]
pub struct NoNode;

/// 스케줄러가 보는 클러스터 상태
#[derive(Debug, Default)]
pub struct Inventory {
    pub nodes: Vec<Node>,
    pub vms: Vec<Vm>,
    pub volumes: Vec<Volume>,
    pub pools: Vec<StoragePool>,
    pub networks: Vec<Network>,
}

impl Inventory {
    pub fn load(cluster: &Cluster) -> Result<Self, ClusterError> {
        Ok(Self {
            nodes: cluster.list::<NodeSpec>()?.1,
            vms: cluster.list::<VmSpec>()?.1,
            volumes: cluster.list::<VolumeSpec>()?.1,
            pools: cluster.list::<StoragePoolSpec>()?.1,
            networks: cluster.list::<NetworkSpec>()?.1,
        })
    }

    /// 이 VM의 브리지 네트워크가 쓰는 브리지가 모두 이 노드에 있는지. VLAN을 쓰면 VLAN-aware여야 한다.
    fn has_bridges(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.nics
            .iter()
            .filter(|nic| nic.network != USER_NETWORK)
            .all(|nic| {
                let Some(network) = self.networks.iter().find(|n| n.name == nic.network) else {
                    return false;
                };
                node.status.bridges.iter().any(|b| {
                    b.name == network.spec.bridge
                        && (network.spec.vlan.is_none() || b.vlan_filtering)
                })
            })
    }

    /// 이 VM의 디스크가 있는 풀을 모두 쓸 수 있는 노드인지. 공유가 아닌 풀의 볼륨은 그 파일이 있는
    /// 노드(볼륨을 만든 노드)에서만 쓸 수 있다.
    fn reaches_disks(&self, spec: &VmSpec, node: &Node) -> bool {
        spec.disks.iter().all(|disk| {
            let Some(volume) = self.volumes.iter().find(|v| v.name == disk.volume) else {
                return true;
            };
            let Some(pool) = self.pools.iter().find(|p| p.name == volume.spec.pool) else {
                return true;
            };
            pool_usable_on(pool, node)
                && (is_shared(pool) || volume.status.node.is_none_or(|n| n == node.id))
        })
    }

    /// 실행하기로 된 다른 VM에 배정되지 않은 메모리
    fn free_memory_mib(&self, node: &Node, vm: Uuid) -> i64 {
        let used: i64 = self
            .vms
            .iter()
            .filter(|v| v.id != vm && v.spec.node == Some(node.id))
            .filter(|v| v.spec.power == PowerState::Running)
            .map(|v| i64::from(v.spec.memory_mib))
            .sum();
        node.status.memory_mib as i64 - used
    }
}

/// 온라인이고 유지보수 중이 아니며 머신 타입의 아키텍처이고 디스크의 풀을 쓸 수 있고 네트워크의 브리지가 있는 노드
/// 가운데 미할당 메모리가 가장 많은 노드를 고른다. 같으면 id가 작은 노드다. 메모리가 모자라도 고른다(오버커밋 정책은
/// 범위 밖). arch를 모르는 노드(옛 버전)는 고르지 않는다.
pub fn schedule(vm: Uuid, spec: &VmSpec, inv: &Inventory) -> Result<Uuid, NoNode> {
    inv.nodes
        .iter()
        .filter(|n| n.status.online && !n.spec.maintenance)
        .filter(|n| n.status.arch == Some(spec.machine.arch()))
        .filter(|n| inv.reaches_disks(spec, n))
        .filter(|n| inv.has_bridges(spec, n))
        .map(|n| (inv.free_memory_mib(n, vm), n.id))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
        .ok_or(NoNode)
}

#[cfg(test)]
mod tests {
    use opl_types::node::{BridgeInfo, NodeStatus, PoolState};
    use opl_types::resource::Resource;
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::PoolBackend;
    use opl_types::vm::{Arch, CpuSpec, DiskBus, DiskSpec, MachineType, NicModel, NicSpec};

    use super::*;

    fn node(memory_mib: u64) -> Node {
        let mut n = Resource::new(
            "n",
            NodeSpec {
                address: "a".into(),
                maintenance: false,
            },
        );
        n.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib,
            hypervisor: "mock".into(),
            bridges: vec![],
            arch: Some(Arch::Aarch64),
            pools: Default::default(),
        };
        n
    }

    /// 노드가 알린 풀 상태
    fn with_pool(mut node: Node, pool: &StoragePool, ready: bool) -> Node {
        let state = PoolState {
            pool_id: pool.id,
            ready,
            ..Default::default()
        };
        node.status.pools.insert(pool.name.clone(), state);
        node
    }

    fn pool(name: &str, shared: bool) -> StoragePool {
        Resource::new(
            name,
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pool".into(),
                },
                nodes: vec![],
                shared,
            },
        )
    }

    fn volume(name: &str, pool: &str, node: Option<Uuid>) -> Volume {
        let mut v: Volume = Resource::new(
            name,
            VolumeSpec {
                pool: pool.into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
            },
        );
        v.status.node = node;
        v
    }

    fn spec(memory_mib: u32, node: Option<Uuid>, power: PowerState) -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
            },
            memory_mib,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power,
            restart: Default::default(),
            node,
            snapshot_schedule: None,
        }
    }

    #[test]
    fn picks_node_with_most_unassigned_memory() {
        let (a, b) = (node(8192), node(4096));
        let mut inv = Inventory {
            nodes: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        let me = spec(1024, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(a.id));

        // a에 6 GiB를 쓰는 VM이 돌면 b가 더 비어 있다. 꺼진 VM은 세지 않는다.
        inv.vms.push(Resource::new(
            "big",
            spec(6144, Some(a.id), PowerState::Running),
        ));
        inv.vms.push(Resource::new(
            "off",
            spec(6144, Some(b.id), PowerState::Stopped),
        ));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(b.id));
    }

    #[test]
    fn skips_offline_maintenance_and_unreachable_pools() {
        let mut offline = node(64000);
        offline.status.online = false;
        let mut maint = node(64000);
        maint.spec.maintenance = true;
        let near = node(1024);
        let pool = Resource::new(
            "local",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pool".into(),
                },
                nodes: vec![near.id],
                shared: false,
            },
        );
        let far = with_pool(node(32000), &pool, true);
        let near = with_pool(near, &pool, true);
        let volume = Resource::new(
            "root",
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1,
                format: ImageFormat::Qcow2,
            },
        );
        let inv = Inventory {
            nodes: vec![offline, maint, far, near.clone()],
            vms: vec![],
            volumes: vec![volume],
            pools: vec![pool],
            networks: vec![],
        };
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(near.id));

        let empty = Inventory::default();
        assert_eq!(schedule(Uuid::now_v7(), &me, &empty), Err(NoNode));
    }

    #[test]
    fn local_volumes_stay_on_their_node_and_shared_pools_need_a_ready_node() {
        let big = node(64000);
        let mid = node(32000);
        let small = node(1024);
        let mut me = spec(512, None, PowerState::Running);
        me.disks.push(DiskSpec {
            volume: "root".into(),
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        });
        let inv = |pool: StoragePool, volume: Volume, ready: [bool; 3]| Inventory {
            nodes: [&big, &mid, &small]
                .into_iter()
                .zip(ready)
                .map(|(n, r)| with_pool(n.clone(), &pool, r))
                .collect(),
            volumes: vec![volume],
            pools: vec![pool],
            ..Default::default()
        };

        // 공유가 아닌 풀은 모든 노드가 쓸 수 있어도 볼륨 파일이 있는 노드로 간다.
        let local = inv(
            pool("local", false),
            volume("root", "local", Some(small.id)),
            [true; 3],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &local), Ok(small.id));
        // 그 노드에서 풀이 준비되지 않았으면 갈 곳이 없다.
        let local = inv(
            pool("local", false),
            volume("root", "local", Some(small.id)),
            [true, true, false],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &local), Err(NoNode));

        // 공유 풀은 준비된 노드 가운데 메모리가 가장 많은 노드로 간다.
        let shared = inv(
            pool("nas", true),
            volume("root", "nas", Some(small.id)),
            [false, true, true],
        );
        assert_eq!(schedule(Uuid::now_v7(), &me, &shared), Ok(mid.id));
        // 상태를 알리지 않은 노드(옛 노드, 첫 새로 고침 전)는 NFS 풀에는 후보가 아니고, 디렉터리
        // 풀에는 후보다.
        let mut nfs = pool("nas", true);
        nfs.spec.backend = PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: None,
        };
        let mut unknown = inv(nfs, volume("root", "nas", None), [true; 3]);
        for n in &mut unknown.nodes {
            n.status.pools.clear();
        }
        assert_eq!(schedule(Uuid::now_v7(), &me, &unknown), Err(NoNode));
        unknown.pools[0] = pool("nas", true);
        assert_eq!(schedule(Uuid::now_v7(), &me, &unknown), Ok(big.id));
    }

    #[test]
    fn needs_the_bridges_of_its_networks() {
        let bridge = |name: &str, vlan_filtering| BridgeInfo {
            name: name.into(),
            vlan_filtering,
        };
        let mut none = node(64000);
        none.status.bridges = vec![bridge("vmbr1", true)];
        let mut plain = node(32000);
        plain.status.bridges = vec![bridge("vmbr0", false)];
        let mut aware = node(1024);
        aware.status.bridges = vec![bridge("vmbr0", true)];
        let network = |name: &str, vlan| -> Network {
            Resource::new(
                name,
                NetworkSpec {
                    bridge: "vmbr0".into(),
                    vlan,
                },
            )
        };
        let inv = Inventory {
            nodes: vec![none, plain.clone(), aware.clone()],
            networks: vec![network("lan", None), network("dmz", Some(20))],
            ..Default::default()
        };
        let nic = |network: &str| NicSpec {
            network: network.into(),
            model: NicModel::Virtio,
            mac: None,
        };
        let mut me = spec(512, None, PowerState::Running);
        me.nics = vec![nic("user"), nic("lan")];
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(plain.id));
        // VLAN을 쓰려면 VLAN-aware 브리지가 있어야 한다.
        me.nics.push(nic("dmz"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(aware.id));
        // 없는 네트워크는 어디서도 돌 수 없다.
        me.nics.push(nic("gone"));
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }

    #[test]
    fn needs_the_architecture_of_its_machine() {
        let x86 = {
            let mut n = node(64000);
            n.status.arch = Some(Arch::X86_64);
            n
        };
        let unknown = {
            let mut n = node(32000);
            n.status.arch = None;
            n
        };
        let arm = node(1024);
        let inv = Inventory {
            nodes: vec![x86.clone(), unknown, arm.clone()],
            ..Default::default()
        };
        // virt는 aarch64 노드에서만 돈다. arch를 모르는 노드(옛 버전)는 후보가 아니다.
        let mut me = spec(512, None, PowerState::Running);
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(arm.id));
        me.machine = MachineType::Q35;
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Ok(x86.id));
        let inv = Inventory {
            nodes: vec![arm],
            ..Default::default()
        };
        assert_eq!(schedule(Uuid::now_v7(), &me, &inv), Err(NoNode));
    }
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
use crate::vms::{is_stopped, prepare};
```

을 다음으로 바꾼다:

```rust
use crate::vms::{is_stopped, prepare};
use crate::volumes::check_pool_on;
```

`crates/opl-api/src/snapshots.rs`에서

```rust
            .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ApiError::conflict(
                "pool_not_on_node",
                format!(
                    "storage pool {name:?} is not available on the node holding VM {:?}",
                    vm.name
                ),
            ));
        }
    }
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))?;
        check_pool_on(&state, &pool, node, || {
            format!(
                "storage pool {name:?} is not available on the node holding VM {:?}",
                vm.name
            )
        })?;
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
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::vm_placement;

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
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨을 다시 지워 보는 간격
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
}

/// 풀이 마운트되지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(DriverError::PoolNotMounted(_))) => left.push(id),
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
            fail(&ctx, &task, error).await;
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
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", ctx.config.timeout)),
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
        Err(e) => fail(ctx, &task, e).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
async fn cleanup(ctx: &Ctx, task: &Task) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op
        && let Err(e) = snapshot::discard_pending(ctx, snapshot).await
    {
        tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
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
/// 본다), 아니면 스토리지로 늘린 뒤 실제 크기를 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나
/// 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서 다시 본다.
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
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => ctx
            .hv
            .block_resize(vm.id, disk, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
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
                        Err(e @ RemoveError::Storage(DriverError::PoolNotMounted(_))) => {
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
                // API가 Task를 배정했을 노드(`vm_placement`). 그것도 없으면 디스크가 없는 VM이라
                // 태그가 없으니 어느 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, &vm)));
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
use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
use opl_types::snapshot::SnapshotPhase;
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::Task;
```

을 다음으로 바꾼다:

```rust
use opl_driver::uefi_vars_path;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::snapshot::SnapshotPhase;
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumeSpec, pool_error_on, pool_listed_on, pool_usable_on,
};
use opl_types::task::Task;
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
        };
        if !target.spec.nodes.is_empty() && !target.spec.nodes.contains(&ctx.config.node) {
            return Err(format!(
                "storage pool {:?} is not available on this node",
                target.name
            ));
```

을 다음으로 바꾼다:

```rust
        };
        if !pool_listed_on(&target, ctx.config.node) {
            return Err(format!(
                "storage pool {:?} is not available on this node",
                target.name
            ));
        }
        // 마운트되지 않은 NFS 풀의 빈 마운트 지점에 복사본을 만들지 않는다.
        let me = ctx
            .cluster
            .get::<NodeSpec>(ctx.config.node)
            .map_err(|e| e.to_string())?
            .ok_or("this node is not registered")?;
        if !pool_usable_on(&target, &me) {
            return Err(format!(
                "storage pool {:?} is not ready on this node: {}",
                target.name,
                pool_error_on(&target, &me)
            ));
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
```

을 다음으로 바꾼다:

```rust
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
};
use opl_types::task::Task;
```

`crates/opl-api/src/tasks/snapshot.rs`에서

```rust
    match volume.status.node {
        Some(node) if node != ctx.config.node && !pool.spec.shared => {
            Err(format!("volume {:?} is on node {node}", volume.name))
```

을 다음으로 바꾼다:

```rust
    match volume.status.node {
        Some(node) if node != ctx.config.node && !is_shared(pool) => {
            Err(format!("volume {:?} is on node {node}", volume.name))
```

`crates/opl-api/src/volumes.rs`에서

```rust
use futures_util::StreamExt;
use opl_types::resource::{Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust
use futures_util::StreamExt;
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
```

`crates/opl-api/src/volumes.rs`에서

```rust
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
};
```

을 다음으로 바꾼다:

```rust
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
    pool_error_on, pool_listed_on, pool_usable_on,
};
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 볼륨 파일 작업을 실행할 노드. 이 노드가 풀을 쓸 수 있으면 이 노드다.
fn pool_node(state: &AppState, pool: &StoragePool) -> Uuid {
    let nodes = &pool.spec.nodes;
    if nodes.is_empty() || nodes.contains(&state.node) {
        state.node
    } else {
        nodes[0]
    }
}

/// 볼륨 파일 작업을 실행할 노드. 풀이 없어졌으면 이 노드다.
fn volume_node(state: &AppState, volume: &Volume) -> Result<Uuid, ApiError> {
    Ok(
        match state
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
        {
            Some(pool) => pool_node(state, &pool),
            None => state.node,
        },
    )
}
```

을 다음으로 바꾼다:

```rust

/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let usable: Vec<Uuid> = nodes
        .iter()
        .filter(|n| n.status.online && pool_usable_on(pool, n))
        .map(|n| n.id)
        .collect();
    if usable.contains(&state.node) {
        return Ok(state.node);
    }
    usable.into_iter().min().ok_or_else(|| {
        ApiError::conflict(
            "pool_unavailable",
            format!("storage pool {:?} is not ready on any node", pool.name),
        )
    })
}

/// 볼륨 파일 작업을 실행할 노드. 공유가 아닌 풀은 파일이 있는 노드(볼륨을 만든 노드)이고, 공유
/// 풀은 [`pool_node`]의 규칙이다. 풀이 없어졌으면 이 노드다.
fn volume_node(state: &AppState, volume: &Volume) -> Result<Uuid, ApiError> {
    match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
    {
        Some(pool) => match volume.status.node {
            Some(node) if !is_shared(&pool) => Ok(node),
            _ => pool_node(state, &pool),
        },
        None => Ok(state.node),
    }
}

/// `node`에서 풀을 쓸 수 있는지 확인한다. 노드 목록에 없으면 409 `pool_not_on_node`(메시지는
/// `not_on_node`), 있지만 그 노드에서 준비되지 않았으면 409 `pool_unavailable`(노드가 알린 오류).
pub(crate) fn check_pool_on(
    state: &AppState,
    pool: &StoragePool,
    node: Uuid,
    not_on_node: impl FnOnce() -> String,
) -> Result<(), ApiError> {
    if !pool_listed_on(pool, node) {
        return Err(ApiError::conflict("pool_not_on_node", not_on_node()));
    }
    let Some(n) = state.cluster.get::<NodeSpec>(node)? else {
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} is not ready: node {node} not found",
                pool.name
            ),
        ));
    };
    if !pool_usable_on(pool, &n) {
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} is not ready on node {:?}: {}",
                pool.name,
                n.name,
                pool_error_on(pool, &n)
            ),
        ));
    }
    Ok(())
}
```

`crates/opl-api/src/volumes.rs`에서

```rust
    let pool = find_pool(&state, &req.spec.pool)?;
    let node = pool_node(&state, &pool);
    let mut volume: Volume = Resource::new(req.name, req.spec);
```

을 다음으로 바꾼다:

```rust
    let pool = find_pool(&state, &req.spec.pool)?;
    let node = pool_node(&state, &pool)?;
    let mut volume: Volume = Resource::new(req.name, req.spec);
```

`crates/opl-api/src/volumes.rs`에서

```rust
    let pool = find_pool(state, &q.pool)?;
    if pool_node(state, &pool) != state.node {
        return Err(ApiError::conflict(
            "pool_not_on_node",
            format!(
                "storage pool {:?} is not available on this node; upload to one of its nodes",
                pool.name
            ),
        ));
    }
    if state.cluster.get_by_name::<VolumeSpec>(&q.name)?.is_some() {
```

을 다음으로 바꾼다:

```rust
    let pool = find_pool(state, &q.pool)?;
    check_pool_on(state, &pool, state.node, || {
        format!(
            "storage pool {:?} is not available on this node; upload to one of its nodes",
            pool.name
        )
    })?;
    if state.cluster.get_by_name::<VolumeSpec>(&q.name)?.is_some() {
```

`crates/opl-api/tests/clones.rs`에서

```rust
use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_driver::uefi_vars_path;
use opl_types::node::PoolState;
use opl_types::resource::Resource;
```

`crates/opl-api/tests/clones.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}
```

`crates/opl-api/tests/clones.rs`에서

```rust
        "pool_not_on_node",
    )
```

을 다음으로 바꾼다:

```rust
        "pool_not_on_node",
    )
    .await;
    // 노드 목록에는 있지만 그 노드에서 준비되지 않았다.
    pool(&env, "down", vec![]).await;
    common::set_pool_state(&env.cluster, env.node, "down", Default::default()).await;
    assert_conflict(
        &admin,
        "web",
        json!({ "name": "web-2", "pool": "down" }),
        "pool_unavailable",
    )
```

`crates/opl-api/tests/clones.rs`에서

```rust
#[tokio::test]
async fn snapshot_delete_waits_for_a_clone_of_the_same_vm() {
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn clone_fails_when_the_target_pool_is_no_longer_ready() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, "local", vec![]).await;
    pool(&env, "nas", vec![]).await;
    let root = volume(&env, "root").await;
    let storage = fake_storage(&mut env, &[root]).await;
    vm_with_disks(&admin, "web", &["root"]).await;

    // API가 받은 뒤 Task가 돌기 전에 대상 풀이 이 노드에서 준비되지 않게 됐다(NFS 서버가 멈춤).
    env.stop_runner().await;
    let (status, task) = clone(&admin, "web", json!({ "name": "web-2", "pool": "nas" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let down = PoolState {
        ready: false,
        error: Some("nfs server not responding".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "nas", down).await;
    env.start_runner_with(storage.clone()).await;
    failed_with(&admin.wait_task(&task).await, "nfs server not responding");
    assert!(!storage.calls().iter().any(|(call, _)| *call == "copy"));
    assert!(volumes_named(&env, "web-2").is_empty());
    assert!(get_vm(&env, "web-2").is_none());
}

#[tokio::test]
async fn snapshot_delete_waits_for_a_clone_of_the_same_vm() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePool};
use opl_types::vm::Arch;
```

을 다음으로 바꾼다:

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePool, StoragePoolSpec};
use opl_types::vm::Arch;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust

    pub async fn pid(&self, id: Uuid) -> Option<u32> {
```

을 다음으로 바꾼다:

```rust

    /// 이 노드가 풀을 준비됐다고 알린 것처럼 기록한다([`mark_pool_ready`]).
    pub async fn pool_ready(&self, pool: &str) {
        mark_pool_ready(&self.cluster, self.node, pool).await;
    }

    pub async fn pid(&self, id: Uuid) -> Option<u32> {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust

/// 클러스터 정보(이름 `test`, 새 CA)를 저장소에 만든다. 클러스터를 만들 때 opld가 하는 일이다.
```

을 다음으로 바꾼다:

```rust

/// opld의 풀 새로 고침 대신 `node`가 풀 `pool`을 준비됐다고 알린 것처럼 기록한다. 배치와 볼륨
/// Task 노드는 준비된 노드만 고른다.
pub async fn mark_pool_ready(cluster: &Cluster, node: Uuid, pool: &str) {
    let state = PoolState {
        ready: true,
        ..Default::default()
    };
    set_pool_state(cluster, node, pool, state).await;
}

/// `node`가 풀 `pool`의 상태를 `state`로 알린 것처럼 기록한다. 상태의 `pool_id`는 그 풀의 id로 채운다.
pub async fn set_pool_state(cluster: &Cluster, node: Uuid, pool: &str, mut state: PoolState) {
    state.pool_id = cluster
        .get_by_name::<StoragePoolSpec>(pool)
        .unwrap()
        .expect("create the pool first")
        .id;
    let mut n = cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.pools.insert(pool.into(), state);
    cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

/// 클러스터 정보(이름 `test`, 새 CA)를 저장소에 만든다. 클러스터를 만들 때 opld가 하는 일이다.
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    release: tokio::sync::Notify,
}
```

을 다음으로 바꾼다:

```rust
    release: tokio::sync::Notify,
    /// 남은 횟수만큼 `delete_volume`이 `PoolNotMounted`로 실패한다(풀이 아직 마운트되지 않음).
    unmounted_deletes: std::sync::Mutex<usize>,
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        self.sizes.lock().unwrap().insert(id, size_bytes);
    }
```

을 다음으로 바꾼다:

```rust
        self.sizes.lock().unwrap().insert(id, size_bytes);
    }

    /// 다음 `n`번의 `delete_volume`이 `PoolNotMounted`로 실패한다.
    pub fn fail_deletes_unmounted(&self, n: usize) {
        *self.unmounted_deletes.lock().unwrap() = n;
    }
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    ) -> Result<(), DriverError> {
        Ok(())
```

을 다음으로 바꾼다:

```rust
    ) -> Result<(), DriverError> {
        let mut left = self.unmounted_deletes.lock().unwrap();
        if *left > 0 {
            *left -= 1;
            return Err(DriverError::PoolNotMounted(
                "/nonexistent/opl-pools/p".into(),
            ));
        }
        Ok(())
```

`crates/opl-api/tests/import_check.rs`에서

```rust
use common::{Client, Env};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use common::{Client, Env};
use opl_types::node::PoolState;
use opl_types::resource::Resource;
```

`crates/opl-api/tests/import_check.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}
```

`crates/opl-api/tests/import_check.rs`에서

```rust
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    volume(&env, "taken").await;
```

을 다음으로 바꾼다:

```rust
    pool(&env, "remote", vec![Uuid::now_v7()]).await;
    pool(&env, "down", vec![]).await;
    let down = PoolState {
        ready: false,
        error: Some("mount.nfs: access denied".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "down", down).await;
    volume(&env, "taken").await;
```

`crates/opl-api/tests/import_check.rs`에서

```rust
            "pool_not_on_node",
        ),
        (
            "taken",
```

을 다음으로 바꾼다:

```rust
            "pool_not_on_node",
        ),
        // 노드 목록에는 있지만 이 노드에서 준비되지 않았다.
        (
            "disk",
            "down",
            1 << 20,
            StatusCode::CONFLICT,
            "pool_unavailable",
        ),
        (
            "taken",
```

`crates/opl-api/tests/import_check.rs`에서

```rust
        );
    }

    // 크기가 없으면 확인할 수 없다.
```

을 다음으로 바꾼다:

```rust
        );
    }

    // 준비되지 않은 이유는 노드가 알린 오류다.
    let (_, body) = check(&admin, "disk", "down", 1 << 20).await;
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("access denied"), "{body}");

    // 크기가 없으면 확인할 수 없다.
```

`crates/opl-api/tests/pools.rs`에서

```rust
//! 스토리지 풀 API: 백엔드별 값 검사.
```

을 다음으로 바꾼다:

```rust
//! 스토리지 풀 API: 백엔드별 값 검사, 볼륨 파일 작업을 할 노드.
```

`crates/opl-api/tests/pools.rs`에서

```rust
use common::Env;
use reqwest::StatusCode;
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use common::Env;
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;
```

`crates/opl-api/tests/pools.rs`에서

```rust
#[tokio::test]
async fn nfs_pools_are_not_supported_yet() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool(
                "nas",
                json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["message"].as_str().unwrap().contains("nfs"), "{body}");
    assert_eq!(
        admin.get("/storage-pools/nas").await.0,
        StatusCode::NOT_FOUND
    );
}
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn nfs_pools_are_always_shared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let nfs = json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" });
    // `shared`를 적지 않으면 공유 풀로 만든다.
    let (status, body) = admin.post("/storage-pools", pool("nas", nfs.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert_eq!(body["spec"]["backend"]["kind"], "nfs", "{body}");
    // 공유가 아니라고 적으면 거절한다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "nas2", "spec": { "backend": nfs, "shared": false } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("shared"),
        "{body}"
    );
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "nas2", "spec": { "backend": nfs, "shared": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // dir 풀은 `shared`를 적은 대로 둔다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            pool("dir", json!({ "kind": "dir", "path": env.dir.path() })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    // 모르는 필드는 전처럼 거절한다.
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({ "name": "typo", "spec": { "backend": nfs, "shard": true } }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "invalid_request");
}

/// 풀 `pools`가 준비됐다고 알린 다른 노드를 저장소에 직접 만든다.
async fn other_node(env: &Env, name: &str, online: bool, pools: &[&str]) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
        },
    );
    node.status.online = online;
    for pool in pools {
        let id = env
            .cluster
            .get_by_name::<StoragePoolSpec>(pool)
            .unwrap()
            .unwrap()
            .id;
        let state = PoolState {
            pool_id: id,
            ready: true,
            ..Default::default()
        };
        node.status.pools.insert((*pool).into(), state);
    }
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 볼륨 파일 작업을 맡은 노드는 이 노드에서 풀이 준비됐으면 이 노드, 아니면 풀이 준비된 온라인
/// 노드다(디렉터리 풀은 상태를 아직 알리지 않은 노드도 쓸 수 있다고 본다). 공유가 아닌 풀의 볼륨은 파일이 있는 노드에서 지운다.
#[tokio::test]
async fn volume_tasks_go_to_a_node_where_the_pool_is_ready() {
    let mut env = Env::start().await;
    // Task는 실행하지 않고 맡은 노드만 본다.
    env.stop_runner().await;
    let admin = env.admin().await;
    for (name, backend) in [
        ("local", json!({ "kind": "dir", "path": env.dir.path() })),
        (
            "nas",
            json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/vm" }),
        ),
    ] {
        let (status, body) = admin.post("/storage-pools", pool(name, backend)).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    let create = |name: &str, pool: &str| {
        let body = json!({
            "name": name,
            "spec": { "pool": pool, "size_bytes": 1 << 20, "format": "qcow2" }
        });
        let admin = &admin;
        async move { admin.post("/volumes", body).await }
    };
    let task_node = |task: &Value| task["spec"]["node"].as_str().unwrap().to_string();

    // 어느 노드도 풀 상태를 알리지 않았다. NFS 풀은 쓸 수 없고, 디렉터리 풀은 쓸 수 있다고 본다.
    let (status, body) = create("x", "nas").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "pool_unavailable");
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);
    let (status, task) = create("d", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), env.node.to_string());

    // 이 노드가 디렉터리 풀이 준비되지 않았다고 알리면 준비된 다른 노드다. 오프라인 노드는
    // 준비됐다고 알렸어도 고르지 않는다.
    let down = PoolState {
        ready: false,
        error: Some("no such directory".into()),
        ..Default::default()
    };
    common::set_pool_state(&env.cluster, env.node, "local", down).await;
    other_node(&env, "off", false, &["local", "nas"]).await;
    let other = other_node(&env, "n2", true, &["local", "nas"]).await;
    let (status, task) = create("a", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
    let (_, volume) = admin.get("/volumes/a").await;
    assert_eq!(volume["status"]["node"], other.to_string());

    // 이 노드에서 준비되면 이 노드다.
    env.pool_ready("local").await;
    let (status, task) = create("b", "local").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), env.node.to_string());

    // 공유 풀은 이 노드에서 준비되지 않았으면 준비된 노드다.
    let (status, task) = create("c", "nas").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());

    // 지우기: 공유가 아닌 풀의 볼륨은 파일이 있는 노드, 공유 풀의 볼륨은 풀이 준비된 노드다.
    for (name, node) in [("a", other), ("c", env.node)] {
        let mut v = env
            .cluster
            .get_by_name::<VolumeSpec>(name)
            .unwrap()
            .unwrap();
        v.status.phase = VolumePhase::Ready;
        v.status.node = Some(node);
        env.cluster
            .update_status::<VolumeSpec>(v.id, &v.status)
            .await
            .unwrap();
    }
    let (status, task) = admin.delete("/volumes/a").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
    let (status, task) = admin.delete("/volumes/c").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task_node(&task), other.to_string());
}
```

`crates/opl-api/tests/schedules.rs`에서

```rust
        env.cluster.create(&pool).await.unwrap();
    }
```

을 다음으로 바꾼다:

```rust
        env.cluster.create(&pool).await.unwrap();
        env.pool_ready("local").await;
    }
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
}
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
    let mut volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let mut volume = Resource::new(
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
}
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
}
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
    let operator = env.user("operator", "operator").await;
    // 풀은 다른 노드에만 있다(파일 작업은 그 노드에서 한다).
    let pool_node = Uuid::now_v7();
    pool(&env, vec![pool_node]).await;
    let id = volume(&env, "data", ImageFormat::Raw).await;
```

을 다음으로 바꾼다:

```rust
    let operator = env.user("operator", "operator").await;
    // 풀은 다른 노드에만 있고 볼륨 파일은 그 노드에 있다(파일 작업은 그 노드에서 한다).
    let pool_node = Uuid::now_v7();
    pool(&env, vec![pool_node]).await;
    let id = volume(&env, "data", ImageFormat::Raw).await;
    let mut data = env.cluster.get::<VolumeSpec>(id).unwrap().unwrap();
    data.status.node = Some(pool_node);
    env.cluster
        .update_status::<VolumeSpec>(id, &data.status)
        .await
        .unwrap();
```

`crates/opl-api/tests/tasks.rs`에서

```rust
}

#[tokio::test]
async fn tasks_created_while_runner_is_down_run_when_it_starts() {
```

을 다음으로 바꾼다:

```rust
}

/// 풀이 아직 마운트되지 않아(NFS) 지우지 못한 고아 볼륨은 남겨 두고 나중에 다시 지운다.
#[tokio::test]
async fn orphans_on_an_unmounted_pool_are_removed_later() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
        "half",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1,
            format: ImageFormat::Raw,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let storage = Arc::new(FakeStorage::default());
    storage.fail_deletes_unmounted(2);
    env.start_runner_config(storage, |config| {
        config.orphan_retry = std::time::Duration::from_millis(50);
    })
    .await;
    // 실행기가 뜰 때 지우지 못했다.
    assert!(env.cluster.get::<VolumeSpec>(volume.id).unwrap().is_some());
    let deadline = std::time::Instant::now() + common::WAIT;
    while env.cluster.get::<VolumeSpec>(volume.id).unwrap().is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "orphan volume was kept"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn tasks_created_while_runner_is_down_run_when_it_starts() {
```

`crates/opl-api/tests/tasks.rs`에서

```rust
    env.cluster.create(&pool).await.unwrap();
    let mut volume: Volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
    let mut volume: Volume = Resource::new(
```

`crates/opl-api/tests/vms.rs`에서

```rust
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let mut volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
    let mut volume = Resource::new(
```

`crates/opl-api/tests/volumes.rs` 전체:

```rust
//! 볼륨 생성·가져오기·삭제. qemu-img가 필요하다.
//! `cargo test -p opl-api --features storage-it --test volumes`
#![cfg(feature = "storage-it")]

mod common;

use std::path::Path;
use std::process::Command;

use common::{Client, Env};
use reqwest::StatusCode;
use serde_json::json;

const MIB: u64 = 1 << 20;

async fn create_pool(env: &Env, admin: &Client, dir: &Path) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": "local",
                "spec": { "backend": { "kind": "dir", "path": dir } }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready("local").await;
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[tokio::test]
async fn creates_and_deletes_volume_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;

    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": 10 * MIB, "format": "qcow2" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/root").await;
    assert_eq!(volume["status"]["phase"], "ready");
    assert!(volume["status"]["allocated_bytes"].as_u64().unwrap() > 0);
    let file = pool.join(format!("{}.qcow2", volume["id"].as_str().unwrap()));
    assert!(file.exists());

    let (status, body) = admin.delete("/storage-pools/local").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "pool_in_use");

    // VM이 쓰는 볼륨은 지울 수 없다.
    let mut vm = common::vm_request("web");
    vm["spec"]["disks"] = json!([{ "volume": "root", "bus": "virtio-blk" }]);
    assert_eq!(admin.post("/vms", vm).await.0, StatusCode::CREATED);
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "volume_in_use");
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);

    let (status, task) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(!file.exists());
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        admin.delete("/storage-pools/local").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn failed_create_removes_the_volume() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 풀 디렉터리가 없으면 qemu-img가 실패한다.
    create_pool(&env, &admin, &env.dir.path().join("missing")).await;
    let (_, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "root",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw" }
            }),
        )
        .await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    assert_eq!(admin.get("/volumes/root").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn imports_uploaded_vmdk() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    let raw = env.dir.path().join("disk.raw");
    std::fs::write(&raw, vec![7u8; 3 * MIB as usize]).unwrap();
    let vmdk = env.dir.path().join("disk.vmdk");
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

    let (status, task) = admin
        .upload(
            "/volumes/import?name=imported&pool=local&source_format=vmdk",
            std::fs::read(&vmdk).unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, volume) = admin.get("/volumes/imported").await;
    assert_eq!(volume["spec"]["size_bytes"], 3 * MIB);
    assert_eq!(volume["spec"]["format"], "qcow2");
    // 올린 파일은 가져온 뒤 지운다.
    let uploads: Vec<_> = std::fs::read_dir(env.dir.path().join("uploads"))
        .unwrap()
        .collect();
    assert!(uploads.is_empty());
}

#[tokio::test]
async fn upload_needs_content_length() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    // 스트림 본문은 길이를 모르므로 chunked로 간다.
    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(vec![0u8; 512])]);
    let r = admin
        .http
        .post(format!(
            "{}/api/v1/volumes/import?name=x&pool=local&source_format=raw",
            admin.base
        ))
        .bearer_auth(admin.token.as_ref().unwrap())
        .body(reqwest::Body::wrap_stream(chunks));
    let (status, body) = admin.send(r).await;
    assert_eq!(status, StatusCode::LENGTH_REQUIRED, "{body}");
    assert_eq!(admin.get("/volumes/x").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_images_that_refer_to_other_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let pool = env.dir.path().join("pool");
    std::fs::create_dir(&pool).unwrap();
    create_pool(&env, &admin, &pool).await;
    let descriptor = "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
                      createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"/etc/hosts\" 0\n";

    let (status, task) = admin
        .upload(
            "/volumes/import?name=evil&pool=local&source_format=vmdk",
            descriptor.as_bytes().to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("descriptor"), "{error}");
    assert_eq!(admin.get("/volumes/evil").await.0, StatusCode::NOT_FOUND);
    assert_eq!(std::fs::read_dir(&pool).unwrap().count(), 0);
}
```

`crates/opl-peer/src/monitor.rs`에서

```rust

use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_types::node::{NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust

use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{NodeSpec, NodeStatus};
```

`crates/opl-peer/src/monitor.rs`에서

```rust
        }
        // 목록을 읽은 뒤 노드가 스스로 등록한 상태를 덮어쓰지 않도록 쓰기 직전에 다시 읽는다.
        let Ok(Some(fresh)) = cluster.get::<NodeSpec>(node.id) else {
```

을 다음으로 바꾼다:

```rust
        }
        // 목록을 읽은 뒤 노드가 스스로 등록한 상태를 덮어쓰지 않도록 쓰기 직전에 다시 읽고, 그 사이
        // 노드가 status(풀 상태 등)를 썼으면 쓰지 않는다(다음 핑에서 다시 본다).
        let Ok(Some(fresh)) = cluster.get::<NodeSpec>(node.id) else {
```

`crates/opl-peer/src/monitor.rs`에서

```rust
        }
        tracing::info!(node = %node.name, online, "node availability changed");
        let status = NodeStatus {
```

을 다음으로 바꾼다:

```rust
        }
        let status = NodeStatus {
```

`crates/opl-peer/src/monitor.rs`에서

```rust
        };
        if let Err(e) = cluster.update_status::<NodeSpec>(node.id, &status).await {
            tracing::warn!(node = %node.name, error = %e, "cannot record node availability");
        }
```

을 다음으로 바꾼다:

```rust
        };
        match cluster
            .update_status_checked::<NodeSpec>(node.id, fresh.revision, &status)
            .await
        {
            Ok(_) => tracing::info!(node = %node.name, online, "node availability changed"),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(node = %node.name, error = %e, "cannot record node availability")
            }
        }
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
use opl_cluster::{Cluster, ClusterError};
use opl_driver::DirStorage;
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::vm::Vm;
```

을 다음으로 바꾼다:

```rust
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DirStorage, check_pool_root};
use opl_types::network::{NetworkSpec, USER_NETWORK, tap_name};
use opl_types::node::NodeSpec;
use opl_types::runtime::{NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::storage::{
    PoolBackend, StoragePoolSpec, VolumeSpec, pool_error_on, pool_listed_on, pool_state_on,
};
use opl_types::vm::Vm;
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
    PoolNotOnNode(String),
    #[error("network {0:?} not found")]
```

을 다음으로 바꾼다:

```rust
    PoolNotOnNode(String),
    #[error("storage pool {0:?} is not ready on this node: {1}")]
    PoolUnavailable(String, String),
    #[error("network {0:?} not found")]
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id이고
/// `mount_root`는 이 노드가 NFS 풀을 마운트하는 디렉터리다.
pub fn runtime_spec(
```

을 다음으로 바꾼다:

```rust
/// 로컬 저장소에서 참조를 찾아 런타임 정의를 만든다. `node`는 이 노드의 id이고
/// `mount_root`는 이 노드가 NFS 풀을 마운트하는 디렉터리다. 이 노드가 준비되지 않았다고 알린 풀은
/// `PoolUnavailable`(잠시 뒤 다시 시도할 일)로 거절한다. 아직 알리지 않은 풀(opld가 막 시작해 첫 새로
/// 고침 전)은 막지 않되, NFS 풀은 실제로 마운트돼 있는지 확인한다.
pub fn runtime_spec(
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

을 다음으로 바꾼다:

```rust
) -> Result<VmRuntimeSpec, ResolveError> {
    let me = cluster.get::<NodeSpec>(node)?;
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
```

`crates/opl-reconciler/src/resolve.rs`에서

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
```

을 다음으로 바꾼다:

```rust
            .ok_or_else(|| ResolveError::PoolNotFound(pool_name.clone()))?;
        if !pool_listed_on(&pool, node) {
            return Err(ResolveError::PoolNotOnNode(pool_name.clone()));
        }
        if let Some(me) = &me
            && pool_state_on(&pool, me).is_some_and(|s| !s.ready)
        {
            let error = pool_error_on(&pool, me);
            return Err(ResolveError::PoolUnavailable(pool_name.clone(), error));
        }
        if matches!(pool.spec.backend, PoolBackend::Nfs { .. })
            && let Err(e) = check_pool_root(&pool, mount_root)
        {
            return Err(ResolveError::PoolUnavailable(
                pool_name.clone(),
                e.to_string(),
            ));
        }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 참조가 틀렸다. spec을 고쳐야 하므로 다시 시도하지 않는다.
```

을 다음으로 바꾼다:

```rust
            Err(ResolveError::Cluster(e)) => return Err(e.into()),
            // 풀이 이 노드에서 아직 준비되지 않았다(NFS 서버가 멈춤, 마운트 전 등). 풀이 돌아오면
            // 시작할 수 있으므로 실패로 세지 않고 잠시 뒤 다시 시도한다.
            Err(e @ ResolveError::PoolUnavailable(..)) => {
                let delay = self.config.retry_interval;
                self.backoff = Some((
                    Instant::now() + delay,
                    format!("{e}; retrying in {delay:?}"),
                ));
                return Ok(false);
            }
            // 참조가 틀렸다. spec을 고쳐야 하므로 다시 시도하지 않는다.
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
}

#[tokio::test]
async fn restarted_reconciler_keeps_running_vms() {
```

을 다음으로 바꾼다:

```rust
}

/// 풀이 이 노드에서 준비되지 않았으면(NFS 서버가 멈춤 등) 시작을 미루고 다시 시도한다. 실패로 세지
/// 않으므로 오래 걸려도 포기하고 끄지 않는다.
#[tokio::test]
async fn unavailable_pool_is_retried_without_powering_off() {
    let env = Env::start().await;
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
    let volume: Volume = Resource::new(
        "root",
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    let mut me: Node = Resource::new(
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = env.node;
    let down = PoolState {
        pool_id: pool.id,
        ready: false,
        error: Some("pool directory not responding".into()),
        ..Default::default()
    };
    me.status.pools.insert("local".into(), down);
    env.cluster.create(&me).await.unwrap();
    let mut vm = vm("web", env.node);
    vm.spec.disks.push(DiskSpec {
        volume: "root".into(),
        bus: DiskBus::VirtioBlk,
        boot_order: Some(1),
    });
    env.create(&vm).await;

    let got = env
        .wait(vm.id, "waiting for the pool", |v| {
            v.status.phase == VmPhase::Starting
                && v.status
                    .last_error
                    .as_deref()
                    .is_some_and(|e| e.contains("pool directory not responding"))
        })
        .await;
    assert_eq!(got.spec.power, PowerState::Running);
    // 재시작 포기 횟수(3번)보다 훨씬 많이 다시 시도해도 끄지 않는다.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let got = env.get(vm.id);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Starting);
    assert_eq!(env.pid(vm.id).await, None);

    // 풀이 준비되면 시작한다.
    let mut me = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    me.status.pools.get_mut("local").unwrap().ready = true;
    env.cluster
        .update_status::<NodeSpec>(env.node, &me.status)
        .await
        .unwrap();
    let got = env.wait(vm.id, "running", running).await;
    assert_eq!(got.status.last_error, None);
}

#[tokio::test]
async fn restarted_reconciler_keeps_running_vms() {
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
use opl_types::network::{Network, NetworkSpec, tap_name};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_types::network::{Network, NetworkSpec, tap_name};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust

#[tokio::test]
async fn resolves_nfs_volumes_under_mount_point() {
    let dir = TempDir::new().unwrap();
```

을 다음으로 바꾼다:

```rust

/// NFS 풀의 디스크는 마운트 지점 아래에 있다. 마운트되지 않았으면(빈 마운트 지점) 노드가 아직
/// 상태를 알리지 않았어도 잠시 뒤 다시 시도할 `PoolUnavailable`이다. 경로는 `pool_root`가 정한다.
#[tokio::test]
async fn refuses_nfs_volumes_until_mounted() {
    let dir = TempDir::new().unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    vm.spec.disks.push(disk("root"));
    let spec = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    assert_eq!(
        spec.disks[0].path,
        Path::new(MNT)
            .join(nas.id.to_string())
            .join(format!("{}.qcow2", root.id))
    );
```

을 다음으로 바꾼다:

```rust
    vm.spec.disks.push(disk("root"));
    let mnt = TempDir::new().unwrap();
    std::fs::create_dir(mnt.path().join(nas.id.to_string())).unwrap();
    let err = runtime_spec(&cluster, &vm, node, mnt.path()).unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e) if n == "nas" && e.contains("not mounted")),
        "{err:?}"
    );
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
    ));
}
```

을 다음으로 바꾼다:

```rust
    ));
}

#[tokio::test]
async fn refuses_pools_this_node_reports_not_ready() {
    let dir = TempDir::new().unwrap();
    let cluster = common::single_node(&MemRouter::default(), dir.path()).await;
    let node = cluster.id().0;
    let mut pools = Vec::new();
    for name in ["down", "unknown", "stale"] {
        let p = pool(name, vec![]);
        cluster.create(&p).await.unwrap();
        pools.push(p);
        cluster
            .create(&volume(&format!("{name}-root"), name))
            .await
            .unwrap();
    }
    let mut me: Node = Resource::new(
        "n1",
        NodeSpec {
            address: "a".into(),
            maintenance: false,
        },
    );
    me.id = node;
    let down = PoolState {
        pool_id: pools[0].id,
        ready: false,
        error: Some("mount.nfs: access denied".into()),
        ..Default::default()
    };
    me.status.pools.insert("down".into(), down.clone());
    // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
    let stale = PoolState {
        pool_id: Uuid::now_v7(),
        ..down
    };
    me.status.pools.insert("stale".into(), stale);
    cluster.create(&me).await.unwrap();

    let mut vm = common::vm("web", node);
    vm.spec.disks.push(disk("down-root"));
    let err = runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap_err();
    assert!(
        matches!(&err, ResolveError::PoolUnavailable(n, e)
            if n == "down" && e == "mount.nfs: access denied"),
        "{err:?}"
    );
    assert!(err.to_string().contains("access denied"), "{err}");
    // 상태를 아직 알리지 않은 풀(opld가 막 시작함)은 막지 않는다. 파일이 없으면 시작이 실패한다.
    for disk_name in ["unknown-root", "stale-root"] {
        vm.spec.disks = vec![disk(disk_name)];
        runtime_spec(&cluster, &vm, node, Path::new(MNT)).unwrap();
    }
}
```

`crates/opl-types/src/storage.rs`에서

```rust

use crate::resource::{Kind, Resource, ResourceSpec};
```

을 다음으로 바꾼다:

```rust

use crate::node::{Node, PoolState};
use crate::resource::{Kind, Resource, ResourceSpec};
```

`crates/opl-types/src/storage.rs`에서

```rust
    pool.spec.shared || matches!(pool.spec.backend, PoolBackend::Nfs { .. })
}
```

을 다음으로 바꾼다:

```rust
    pool.spec.shared || matches!(pool.spec.backend, PoolBackend::Nfs { .. })
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
/// 준비되지 않았다고 알렸을 때만 막는다. NFS 풀은 마운트됐다고 알려야 한다.
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
```

`crates/opl-types/src/storage.rs`에서

```rust
    #[test]
    fn rejects_unknown_backend_fields() {
```

을 다음으로 바꾼다:

```rust
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

    #[test]
    fn rejects_unknown_backend_fields() {
```

`crates/opld/src/bootstrap.rs` 전체:

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::path::Path;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use uuid::Uuid;

use crate::config::Config;
use crate::host::Host;

pub const NODE_ID_FILE: &str = "node-id";
const NODE_ID_TMP_FILE: &str = "node-id.tmp";

/// 처음 시작할 때 만든 노드 id를 계속 쓴다. Raft 노드 id이자 Node 리소스 id다.
pub fn node_id(data_dir: &Path) -> anyhow::Result<Uuid> {
    let path = data_dir.join(NODE_ID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => Uuid::try_parse(s.trim()).with_context(|| format!("bad {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = Uuid::now_v7();
            // 임시 파일에 쓰고 바꿔 넣어, 도중에 죽어도 반쯤 쓴 id가 남지 않게 한다.
            let tmp = data_dir.join(NODE_ID_TMP_FILE);
            let mut file = std::fs::File::create(&tmp)?;
            writeln!(file, "{id}")?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)?;
            Ok(id)
        }
        Err(e) => Err(e.into()),
    }
}

/// 새 클러스터의 첫 기록: 클러스터 정보(이름, CA), 이 노드, `admin` 사용자. 돌려준 비밀번호는
/// 저장하지 않는다(해시만 남는다).
pub async fn create_cluster(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    name: &str,
    ca: &ClusterCa,
) -> anyhow::Result<String> {
    let info = Resource::new(
        name,
        ClusterSpec {
            ca_cert_pem: ca.cert_pem.clone(),
            ca_key_pem: ca.key_pem.clone(),
        },
    );
    cluster.create(&info).await?;
    register_node(cluster, config, host).await?;
    let password = Uuid::new_v4().simple().to_string();
    let admin = Resource::new(
        "admin",
        UserSpec {
            password_hash: opl_api::hash_password(password.clone())
                .await
                .map_err(|e| anyhow::anyhow!(e.message))?,
            role: Role::Admin,
            session_epoch: 0,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                },
            );
            node.id = id;
            cluster.create(&node).await?;
        }
        Some(node) if node.spec.address != address => {
            let spec = NodeSpec {
                address,
                ..node.spec
            };
            cluster.update_spec(id, None, node.labels, &spec).await?;
        }
        Some(_) => {}
    }
    let bridges = host.bridges().await;
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let memory_mib = crate::host::memory_mib();
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            cpus,
            memory_mib,
            hypervisor: host.hypervisor_name(),
            // 브리지 목록을 얻지 못하면 기록된 목록을 그대로 둔다.
            bridges: bridges.clone().unwrap_or_else(|| status.bridges.clone()),
            arch: Some(host.profile.arch),
            // 풀 상태는 풀 새로 고침이 쓴다. 이미 적힌 것을 지우지 않는다.
            pools: std::mem::take(&mut status.pools),
        };
        true
    })
    .await
}

/// 브리지가 바뀌었으면(관리자가 만들거나 지움) 기록한다. 바뀌지 않았거나 목록을 얻지 못하면
/// 쓰지 않는다.
pub async fn refresh_bridges(cluster: &Cluster, host: &Host) -> anyhow::Result<()> {
    let Some(bridges) = host.bridges().await else {
        return Ok(());
    };
    update_node_status(cluster, |status| {
        if status.bridges == bridges {
            return false;
        }
        tracing::info!(?bridges, "bridges changed");
        status.bridges = bridges.clone();
        true
    })
    .await
}

/// 정상 종료할 때 오프라인으로 표시한다. 실패해도 종료는 계속한다.
pub async fn mark_offline(cluster: &Cluster) {
    let result = update_node_status(cluster, |status| {
        status.online = false;
        true
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot mark node offline");
    }
}

/// 이 노드의 status를 `change`로 고쳐 쓴다(`false`면 쓰지 않는다). status의 다른 필드는 다른
/// 작업(풀 새로 고침)도 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 다시 고친다.
async fn update_node_status(
    cluster: &Cluster,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    loop {
        let Some(node) = cluster.get::<NodeSpec>(id)? else {
            return Ok(());
        };
        let mut status = node.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(id, node.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, HostNet, Tap};
    use opl_types::node::{BridgeInfo, PoolState};
    use opl_types::vm::Arch;

    use super::*;

    /// 정해 둔 브리지를 돌려준다. `None`이면 `ip`가 실패한 것처럼 오류를 낸다.
    #[derive(Debug)]
    struct FakeNet(Option<Vec<BridgeInfo>>);

    #[async_trait]
    impl HostNet for FakeNet {
        async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
            self.0.clone().ok_or(DriverError::ToolFailed {
                tool: "ip",
                stderr: "boom".into(),
            })
        }
        async fn create_tap(&self, _: &Tap) -> Result<(), DriverError> {
            unimplemented!()
        }
        async fn delete_vm_taps(&self, _: Uuid) -> Result<(), DriverError> {
            unimplemented!()
        }
    }

    #[test]
    fn node_id_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let id = node_id(dir.path()).unwrap();
        assert_eq!(node_id(dir.path()).unwrap(), id);
        std::fs::write(dir.path().join(NODE_ID_FILE), "garbage").unwrap();
        assert!(node_id(dir.path()).is_err());
    }

    /// 멤버가 이 노드 하나인 클러스터. 리더가 될 때까지 기다린다.
    pub(crate) async fn single_node(dir: &Path) -> (Cluster, NodeId) {
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, cluster.raft().clone());
        let members = BTreeMap::from([(id, BasicNode::new(id.to_string()))]);
        cluster.initialize(members).await.unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        (cluster, id)
    }

    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config).unwrap()
        };
        let br0 = vec![BridgeInfo {
            name: "br0".into(),
            vlan_filtering: false,
        }];
        let (ok, failing) = (host(Some(br0.clone())), host(None));
        let recorded = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .bridges
        };

        // 새 노드는 목록을 얻지 못하면 브리지가 없다.
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), vec![]);
        register_node(&cluster, &config, &ok).await.unwrap();
        assert_eq!(recorded(), br0);
        // 이미 기록된 브리지는 목록을 얻지 못해도 지우지 않는다.
        refresh_bridges(&cluster, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        register_node(&cluster, &config, &failing).await.unwrap();
        assert_eq!(recorded(), br0);
        refresh_bridges(&cluster, &host(Some(vec![])))
            .await
            .unwrap();
        assert_eq!(recorded(), vec![]);
        cluster.shutdown().await.unwrap();
    }

    /// 풀 새로 고침이 쓴 `pools`를 브리지 기록과 오프라인 표시가 지우지 않는다. 읽은 뒤 쓰기 전에 다른
    /// 쪽이 status를 써도 그 값을 지키며 다시 쓴다.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_status_writes_keep_pools_written_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config).unwrap();
        host.net = None;
        register_node(&cluster, &config, &host).await.unwrap();
        let pools = BTreeMap::from([("local".to_string(), PoolState::default())]);
        // 처음 고칠 때 풀 새로 고침이 끼어든다.
        let mut first = true;
        update_node_status(&cluster, |status| {
            if std::mem::take(&mut first) {
                let mut meanwhile = status.clone();
                meanwhile.pools = pools.clone();
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(cluster.update_status::<NodeSpec>(id.0, &meanwhile))
                })
                .unwrap();
            }
            status.bridges = vec![BridgeInfo {
                name: "br0".into(),
                vlan_filtering: false,
            }];
            true
        })
        .await
        .unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.pools, pools);
        assert_eq!(node.status.bridges.len(), 1);

        mark_offline(&cluster).await;
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(!node.status.online);
        assert_eq!(node.status.pools, pools);

        // 다시 시작해 등록해도 풀 새로 고침이 먼저 쓴 pools를 지우지 않는다.
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.status.online);
        assert_eq!(node.status.pools, pools);
        cluster.shutdown().await.unwrap();
    }
}
```

`crates/opld/src/pools.rs`에서

```rust
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Capacity;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Capacity;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Kind;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
```

`crates/opld/src/pools.rs`에서

```rust
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
```

을 다음으로 바꾼다:

```rust
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        // 풀이 바뀌면(만들기·지우기) 곧바로 새로 고친다. 배치와 볼륨 Task는 준비된 노드만 고른다.
        tokio::select! {
```

`crates/opld/src/pools.rs`에서

```rust
            _ = interval.tick() => {}
        }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opld/src/pools.rs`에서

```rust
                tracing::warn!(error = format!("{e:#}"), "cannot refresh storage pool states");
            }
        }
    }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opld/src/pools.rs`에서

```rust
    #[tokio::test]
    async fn mount_failure_is_reported_and_retried() {
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opld/tests/common/mod.rs`에서

```rust
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }
}
```

을 다음으로 바꾼다:

```rust
        serde_json::from_str(&self.ctl(&argv).await.unwrap()).unwrap()
    }

    /// 이 노드가 풀 `pool`을 준비됐다고 알릴 때까지 기다린다. 볼륨 만들기와 배치는 준비된 노드만 고른다.
    pub async fn wait_pool_ready(&self, pool: &str) {
        eventually(&format!("pool {pool} ready on {}", self.name), || async {
            let nodes = self.ctl_json(&["node", "list"]).await;
            nodes.as_array().unwrap().iter().any(|n| {
                n["name"] == self.name.as_str() && n["status"]["pools"][pool]["ready"] == true
            })
        })
        .await;
    }
}
```

`crates/opld/tests/e2e.rs`에서

```rust
        .unwrap();
    d.ctl(&[
```

을 다음으로 바꾼다:

```rust
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&[
```

`crates/opld/tests/snapshots.rs`에서

```rust
        .unwrap();
    d.ctl(&[
```

을 다음으로 바꾼다:

```rust
        .unwrap();
    d.wait_pool_ready("local").await;
    d.ctl(&[
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 433개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(storage): 배치와 볼륨 Task 노드가 노드별 풀 상태와 공유 여부를 본다"
```

---

### Task 5: 실제 NFS 시험(nfs-it)과 CI

opld 기능 `nfs-it`과 Linux 전용 `crates/opld/tests/nfs.rs`: 데몬을 띄워 `127.0.0.1:{OPL_TEST_NFS_EXPORT}` NFS 풀을 만들고, 마운트(mountinfo)·준비·용량을 확인하고, 볼륨이 서버 쪽 내보내기에 생기는지, 멈춘 VM의 qcow2 스냅샷 태그, VM·볼륨 지우기, 풀을 지우면 언마운트되는지 본다. 테스트가 실패해도 TempDir을 지우기 전에 opld를 멈추고 언마운트하는 가드(실패하면 TempDir을 남긴다). CI `linux-kvm` 작업에 `nfs-kernel-server` 설치(`apt-get update` 포함), `/srv/opl-nfs` 내보내기, 이 기능과 환경 변수를 켠다.

**Files:**
- Create: `crates/opld/tests/nfs.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
          pnpm -C web exec playwright install --with-deps chromium
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

을 다음으로 바꾼다:

```yaml
          pnpm -C web exec playwright install --with-deps chromium
      # NFS 풀 E2E가 마운트할 내보내기. root로 마운트한 opld가 쓰도록 root를 그대로 둔다.
      - name: NFS server exporting a test directory
        run: |
          sudo apt-get update
          sudo apt-get install -y nfs-kernel-server
          sudo mkdir -p /srv/opl-nfs
          echo '/srv/opl-nfs 127.0.0.1(rw,sync,no_subtree_check,no_root_squash)' | sudo tee -a /etc/exports
          sudo systemctl restart nfs-kernel-server
          sudo exportfs -v
      # root로 빌드하므로 캐시하지 않고 임시 디렉터리에 빌드한다.
```

`.github/workflows/ci.yml`에서

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it
```

을 다음으로 바꾼다:

```yaml
        env:
          FEATURES: opl-qemu/qemu-it,opl-driver/qemu-it,opl-driver/net-it,opl-reconciler/qemu-it,opld/qemu-it,opld/net-it,opld/web-it,opld/nfs-it
          OPL_TEST_NFS_EXPORT: /srv/opl-nfs
```

`crates/opld/Cargo.toml`에서

```toml
web-ui = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

을 다음으로 바꾼다:

```toml
web-ui = []
# 실제 NFS 풀을 마운트하는 E2E 테스트. Linux에서 root, NFS 클라이언트(nfs-common), qemu-img와
# 127.0.0.1에 내보낸 디렉터리(`OPL_TEST_NFS_EXPORT`)가 필요하다.
nfs-it = []
# 브라우저(Playwright)로 웹 UI를 확인하는 E2E. qemu-it의 준비물에 더해 pnpm과
```

`crates/opld/tests/common/mod.rs`에서

```rust

    /// kill -9
```

을 다음으로 바꾼다:

```rust

    /// `start`한 뒤 아직 멈추지 않았는지
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// kill -9
```

`crates/opld/tests/nfs.rs`를 만든다:

```rust
//! 실제 NFS 풀: opld가 `mount -t nfs`로 localhost의 내보내기를 마운트하고, 그 풀에 볼륨을 만들고
//! qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면 언마운트한다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs`
//! Linux에서 root, NFS 클라이언트(`mount.nfs`), qemu-img가 필요하다. `OPL_TEST_NFS_EXPORT`는
//! 127.0.0.1에 `rw,no_root_squash`로 내보낸 디렉터리다(서버 쪽 경로로 파일을 확인한다).
#![cfg(all(feature = "nfs-it", target_os = "linux"))]

mod common;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use oplctl::Client;
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

/// NFS 마운트 지점들(`/proc/self/mountinfo`의 마운트 지점과 종류)
fn nfs_mount_points() -> Vec<PathBuf> {
    let info = std::fs::read_to_string(opl_driver::mounts::MOUNTINFO).unwrap();
    opl_driver::mounts::nfs_mount_points(&info)
        .into_iter()
        .collect()
}

/// 그 경로에 NFS 마운트가 있는지
fn nfs_mounted(dir: &Path) -> bool {
    nfs_mount_points().iter().any(|p| p == dir)
}

/// 테스트가 중간에 실패해도 데몬의 임시 디렉터리를 지우기 전에 opld를 멈추고 `{data_dir}/pools`
/// 아래의 NFS 마운트를 푼다. 마운트된 채로 지우면 내보내기의 파일까지 지운다. 풀지 못하면 임시
/// 디렉터리를 지우지 않고 남긴다.
struct Unmounting(Daemon);

impl Deref for Unmounting {
    type Target = Daemon;

    fn deref(&self) -> &Daemon {
        &self.0
    }
}

impl DerefMut for Unmounting {
    fn deref_mut(&mut self) -> &mut Daemon {
        &mut self.0
    }
}

impl Drop for Unmounting {
    fn drop(&mut self) {
        // opld가 다시 마운트하지 않게 먼저 멈춘다.
        if self.0.is_running() {
            self.0.kill();
        }
        let Ok(pools) = std::fs::canonicalize(self.0.data_dir().join("pools")) else {
            return;
        };
        let mut left = false;
        for point in nfs_mount_points().iter().filter(|p| p.starts_with(&pools)) {
            let unmounted = Command::new("umount")
                .arg(point)
                .status()
                .is_ok_and(|s| s.success());
            if !unmounted {
                eprintln!("cannot unmount {}", point.display());
                left = true;
            }
        }
        if left {
            let dir = std::mem::replace(&mut self.0.dir, tempfile::TempDir::new().unwrap());
            eprintln!(
                "left {} in place because nfs pools are still mounted under it",
                dir.keep().display()
            );
        }
    }
}

/// 볼륨 파일에 든 qcow2 스냅샷 태그
fn tags(file: &Path) -> Vec<String> {
    let out = Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(file)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let info: Value = serde_json::from_slice(&out.stdout).unwrap();
    info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn nfs_pool_mounts_holds_volumes_and_unmounts() {
    let export = PathBuf::from(
        std::env::var("OPL_TEST_NFS_EXPORT")
            .expect("OPL_TEST_NFS_EXPORT must name a directory exported to 127.0.0.1"),
    );
    let mut d = Unmounting(Daemon::new("node1", ""));
    d.start().await;
    d.init("nfs").await;
    d.login().await;
    let c = client(&d);

    // NFS 풀은 shared를 적지 않아도 공유다. 만들면 곧바로 마운트한다.
    c.post(
        "/storage-pools",
        &json!({
            "name": "nas",
            "spec": { "backend": { "kind": "nfs", "server": "127.0.0.1", "export": export } }
        }),
    )
    .await
    .unwrap();
    d.wait_pool_ready("nas").await;
    let pool = c.get("/storage-pools/nas").await.unwrap();
    assert_eq!(pool["spec"]["shared"], true, "{pool}");
    let mount_point = std::fs::canonicalize(d.data_dir())
        .unwrap()
        .join("pools")
        .join(pool["id"].as_str().unwrap());
    assert!(nfs_mounted(&mount_point), "{}", mount_point.display());
    let node = c.get("/nodes/node1").await.unwrap();
    let state = &node["status"]["pools"]["nas"];
    assert!(state["total_bytes"].as_u64().unwrap() > 0, "{node}");
    assert_eq!(state["error"], Value::Null, "{node}");

    // 볼륨 파일은 마운트를 거쳐 서버의 내보내기 디렉터리에 생긴다.
    d.ctl(&["volume", "create", "root", "--pool", "nas", "--size", "64M"])
        .await
        .unwrap();
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let file_name = format!("{}.qcow2", volume["id"].as_str().unwrap());
    let file = export.join(&file_name);
    assert!(file.exists(), "{}", file.display());
    assert!(mount_point.join(&file_name).exists());

    // 꺼진 VM의 스냅샷은 NFS 위 qcow2의 내부 스냅샷이다.
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    d.ctl(&["vm", "snapshot", "create", "web", "before"])
        .await
        .unwrap();
    let snapshot = c.get("/vms/web/snapshots/before").await.unwrap();
    assert_eq!(snapshot["status"]["phase"], "ready", "{snapshot}");
    assert_eq!(
        tags(&file),
        vec![snapshot["id"].as_str().unwrap().to_string()]
    );
    d.ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(tags(&file).is_empty());
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    assert!(!file.exists(), "{}", file.display());

    // 풀을 지우면 언마운트하고 마운트 지점도 지운다.
    d.ctl(&["pool", "delete", "nas"]).await.unwrap();
    eventually("nfs pool unmounted", || async {
        !nfs_mounted(&mount_point) && !mount_point.exists()
    })
    .await;
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 433개, 실패 0

Run: `cargo clippy -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음(macOS에서는 nfs.rs가 비어 있다)

- [ ] **Step 3: 커밋**

```bash
git add .github crates
git commit -m "feat(opld): 실제 NFS 풀을 마운트하는 nfs-it E2E와 CI 준비"
```

---

### Task 6: oplctl: NFS 풀 만들기와 노드별 상태

`pool create NAME --nfs SERVER:/EXPORT [--nfs-options OPTS]`(IPv6는 `[ADDR]:/EXPORT`, `--path`와 함께 못 씀, `shared`는 보내지 않음), `pool list` 열 `NAME KIND LOCATION NODES READY`(READY는 온라인·준비 노드 수 / 쓸 수 있는 노드 수), `pool get`의 표 출력(풀 정보와 노드별 `NODE READY SIZE AVAILABLE ERROR` — 오프라인은 `node offline`, 없는 노드는 `node not found`, `pool_id`가 다른 상태는 보고 없음). `-o json`은 그대로. smoke와 nfs-it가 새 명령을 쓴다.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/tests/nfs.rs`
- Modify: `crates/opld/tests/smoke.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
    },
    /// 로컬 디렉터리 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long)]
        path: PathBuf,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
```

을 다음으로 바꾼다:

```rust
    },
    /// 로컬 디렉터리 풀이나 NFS 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long, required_unless_present = "nfs", conflicts_with = "nfs")]
        path: Option<PathBuf>,
        /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
        #[arg(long, value_parser = parse_nfs)]
        nfs: Option<NfsSource>,
        /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
        #[arg(long, requires = "nfs", conflicts_with = "path")]
        nfs_options: Option<String>,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
```

`crates/oplctl/src/cli.rs`에서

```rust
        pool: String,
    },
}
```

을 다음으로 바꾼다:

```rust
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
```

`crates/oplctl/src/cli.rs`에서

```rust
                let list = client.get("/storage-pools").await?;
                print_list(out, json_out, &list, &["NAME", "PATH", "NODES"], |p| {
                    let nodes = p["spec"]["nodes"].as_array().map_or(0, Vec::len);
                    vec![
                        cell(&p["name"]),
                        cell(&p["spec"]["backend"]["path"]),
                        if nodes == 0 {
                            "all".into()
                        } else {
                            nodes.to_string()
                        },
                    ]
                })?;
            }
            PoolCmd::Get { pool } => {
                print_json(out, &client.get(&format!("/storage-pools/{pool}")).await?)?
            }
            PoolCmd::Create { name, path, nodes } => {
                let body = json!({
                    "name": name,
                    "spec": { "backend": { "kind": "dir", "path": path }, "nodes": nodes }
                });
```

을 다음으로 바꾼다:

```rust
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
                nodes,
            } => {
                // NFS 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
                let backend = match nfs {
                    Some(n) => json!({
                        "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
                    }),
                    None => json!({ "kind": "dir", "path": path }),
                };
                let body = json!({
                    "name": name,
                    "spec": { "backend": backend, "nodes": nodes }
                });
```

`crates/oplctl/src/cli.rs`에서

```rust

/// 토큰 문자열 `opl_<id>_<secret>`의 id
```

을 다음으로 바꾼다:

```rust

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

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
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
```

`crates/oplctl/src/cli.rs`에서

```rust
    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
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
                            ..
                        },
                } => (path, nfs, nfs_options),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None)
        );
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into())
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로
        for args in [
            &[][..],
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
```

`crates/opld/tests/nfs.rs`에서

```rust
use oplctl::Client;
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use oplctl::Client;
use serde_json::Value;
```

`crates/opld/tests/nfs.rs`에서

```rust

    // NFS 풀은 shared를 적지 않아도 공유다. 만들면 곧바로 마운트한다.
    c.post(
        "/storage-pools",
        &json!({
            "name": "nas",
            "spec": { "backend": { "kind": "nfs", "server": "127.0.0.1", "export": export } }
        }),
    )
    .await
    .unwrap();
    d.wait_pool_ready("nas").await;
```

을 다음으로 바꾼다:

```rust

    // oplctl로 만든 NFS 풀은 shared를 적지 않아도 공유다. 만들면 곧바로 마운트한다.
    let source = format!("127.0.0.1:{}", export.display());
    d.ctl(&["pool", "create", "nas", "--nfs", &source])
        .await
        .unwrap();
    d.wait_pool_ready("nas").await;
```

`crates/opld/tests/smoke.rs`에서

```rust
        .unwrap();
    d.ctl(&["vm", "create", "web", "--memory", "256", "--nic", "user"])
```

을 다음으로 바꾼다:

```rust
        .unwrap();
    // 풀 목록과 상세는 노드별 준비 상태를 보인다.
    d.wait_pool_ready("local").await;
    let pools = d.ctl(&["pool", "list"]).await.unwrap();
    assert!(pools.contains("dir") && pools.contains("1/1"), "{pools}");
    let detail = d.ctl(&["pool", "get", "local"]).await.unwrap();
    assert!(detail.contains("\nnode1  true "), "{detail}");
    d.ctl(&["vm", "create", "web", "--memory", "256", "--nic", "user"])
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 435개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `cargo test -p opld --features qemu-it --test smoke`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): NFS 풀 만들기와 풀의 노드별 상태 보기"
```

---

### Task 7: 웹: NFS 풀 만들기와 노드별 풀 상태

새 `storage/pools.ts`(`poolLocation`, `isShared`, `nfsServerError`, 옵션 거부 목록, `poolNodes`, `poolCapacity`, 서버 규칙과 같은 `usableOn`·`poolsUsableOn`·`poolUsableSomewhere`)와 `storage/PoolStatus.tsx`. 풀 만들기 대화상자의 종류(디렉터리/NFS)와 NFS 필드·검사, 풀 목록의 종류·위치 열과 노드별 상태(준비, 준비 안 됨과 이유, 오프라인, 보고 없음)·용량(공유 풀은 온라인·준비된 첫 노드의 값), 노드 목록의 "스토리지 풀" 열. 업로드(연결된 노드)·복제(VM 노드, `pool_unavailable`은 풀 입력란 오류)·볼륨 만들기(어딘가에서 쓸 수 있는 풀) 대화상자가 쓸 수 있는 풀만 보인다. `errors.pool_unavailable`은 서버 메시지 그대로. en·ko.

**Files:**
- Create: `web/src/storage/PoolStatus.tsx`
- Create: `web/src/storage/pools.test.ts`
- Create: `web/src/storage/pools.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/storage/PoolsPage.test.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/storage/UploadDialog.test.tsx`
- Modify: `web/src/storage/UploadDialog.tsx`
- Modify: `web/src/storage/VolumesPage.test.tsx`
- Modify: `web/src/storage/VolumesPage.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/vms/VmCloneDialog.test.tsx`
- Modify: `web/src/vms/VmCloneDialog.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert or clone) is in progress. Try again when it has finished.",
```

`web/src/i18n/en.json`에서

```json
      "exists": "A VM with this name already exists.",
      "poolNotOnNode": "That storage pool is not available on the node holding the VM."
    }
```

을 다음으로 바꾼다:

```json
      "exists": "A VM with this name already exists.",
      "poolNotOnNode": "That storage pool is not available on the node holding the VM.",
      "poolUnavailable": "That storage pool is not ready on the node holding the VM."
    }
```

`web/src/i18n/en.json`에서

```json
    "createTitle": "Create a storage pool",
    "createDescription": "A directory on the nodes that holds volume files. Create the directory on each node first.",
    "pathHint": "An absolute path on the nodes.",
```

을 다음으로 바꾼다:

```json
    "createTitle": "Create a storage pool",
    "createDescription": "Where volume files are kept: a directory on each node, or an NFS export that the nodes mount.",
    "pathHint": "An absolute path on the nodes.",
```

`web/src/i18n/en.json`에서

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory stay on the nodes.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "errors": {
      "path": "Enter an absolute path (starting with /).",
      "nodesGone": "The chosen nodes are no longer in the cluster. Choose again."
    }
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export are kept.",
    "deleted": "Deleted storage pool {{name}}.",
    "inUse": "Delete its volumes first.",
    "kind": "Kind",
    "kinds": {
      "dir": "Directory",
      "nfs": "NFS"
    },
    "location": "Location",
    "kindHint": {
      "dir": "Create the directory on each node first.",
      "nfs": "Each node mounts the export under its data directory. Mounting needs Linux nodes running as root. The pool is shared by its nodes."
    },
    "server": "NFS server",
    "serverHint": "Host name or IP address.",
    "export": "Export path",
    "exportHint": "The absolute path the server exports, such as /srv/vm.",
    "options": "Mount options (optional)",
    "optionsHint": "Comma-separated options passed to mount -o after nosuid,nodev,noexec, such as vers=4.2,hard.",
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
      "options": "Separate the options with commas, without spaces. bind, rbind, remount, move, suid, dev, exec, helper, bg and x-* options are not allowed."
    }
```

`web/src/i18n/en.json`에서

```json
    "bridges": "Bridges",
    "vms": "VMs",
```

을 다음으로 바꾼다:

```json
    "bridges": "Bridges",
    "pools": "Storage pools",
    "vms": "VMs",
```

`web/src/i18n/i18n.test.ts`에서

```ts
  );
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
```

을 다음으로 바꾼다:

```ts
  );
  expect(
    errorMessage(t, new ApiError(409, "pool_unavailable", 'storage pool "nas" is not ready')),
  ).toBe('storage pool "nas" is not ready');
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제)이 진행 중입니다. 끝나면 다시 시도하세요.",
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제)이 진행 중입니다. 끝나면 다시 시도하세요.",
```

`web/src/i18n/ko.json`에서

```json
      "exists": "같은 이름의 VM이 이미 있습니다.",
      "poolNotOnNode": "VM이 있는 노드에서 쓸 수 없는 스토리지 풀입니다."
    }
```

을 다음으로 바꾼다:

```json
      "exists": "같은 이름의 VM이 이미 있습니다.",
      "poolNotOnNode": "VM이 있는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "VM이 있는 노드에서 준비되지 않은 스토리지 풀입니다."
    }
```

`web/src/i18n/ko.json`에서

```json
    "createTitle": "스토리지 풀 만들기",
    "createDescription": "볼륨 파일을 두는 노드의 디렉터리입니다. 먼저 각 노드에 디렉터리를 만드세요.",
    "pathHint": "노드의 절대 경로입니다.",
```

을 다음으로 바꾼다:

```json
    "createTitle": "스토리지 풀 만들기",
    "createDescription": "볼륨 파일을 두는 곳입니다: 각 노드의 디렉터리나 노드들이 마운트하는 NFS 내보내기.",
    "pathHint": "노드의 절대 경로입니다.",
```

`web/src/i18n/ko.json`에서

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리의 파일은 노드에 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "errors": {
      "path": "절대 경로(/로 시작)를 입력하세요.",
      "nodesGone": "고른 노드가 더 이상 클러스터에 없습니다. 다시 고르세요."
    }
```

을 다음으로 바꾼다:

```json
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일은 남습니다.",
    "deleted": "스토리지 풀 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 볼륨을 지우세요.",
    "kind": "종류",
    "kinds": {
      "dir": "디렉터리",
      "nfs": "NFS"
    },
    "location": "위치",
    "kindHint": {
      "dir": "먼저 각 노드에 디렉터리를 만드세요.",
      "nfs": "각 노드가 내보내기를 데이터 디렉터리 아래에 마운트합니다. 마운트는 root로 도는 Linux 노드에서만 됩니다. 풀의 노드들이 함께 쓰는 공유 풀입니다."
    },
    "server": "NFS 서버",
    "serverHint": "호스트 이름 또는 IP 주소입니다.",
    "export": "내보내기 경로",
    "exportHint": "서버가 내보내는 절대 경로입니다(예: /srv/vm).",
    "options": "마운트 옵션(선택)",
    "optionsHint": "nosuid,nodev,noexec 뒤에 붙여 mount -o에 넘길 쉼표 목록입니다(예: vers=4.2,hard).",
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
      "options": "옵션은 공백 없이 쉼표로 나누세요. bind, rbind, remount, move, suid, dev, exec, helper, bg와 x-* 옵션은 쓸 수 없습니다."
    }
```

`web/src/i18n/ko.json`에서

```json
    "bridges": "브리지",
    "vms": "VM",
```

을 다음으로 바꾼다:

```json
    "bridges": "브리지",
    "pools": "스토리지 풀",
    "vms": "VM",
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, poolState, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(a).getAllByRole("cell")[archColumn]).toHaveTextContent("—");
});

it("shows the state of each pool on the node", async () => {
  cluster();
  const pools = {
    nas: poolState("nas", { ready: false, error: "mount.nfs: access denied by server" }),
    local: poolState("local"),
  };
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, pools } }, DOWN] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const poolsColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Storage pools");
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  // 이름 순으로 보인다.
  expect(within(a).getAllByRole("cell")[poolsColumn]).toHaveTextContent(
    "local: Readynas: Not readymount.nfs: access denied by server",
  );
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[poolsColumn]).toHaveTextContent("—");
});
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
import { formatCapacity } from "@/lib/format";
```

을 다음으로 바꾼다:

```tsx
import { formatCapacity } from "@/lib/format";
import { PoolStatus } from "@/storage/PoolStatus";
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.pools")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 10 : 9} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

을 다음으로 바꾼다:

```tsx
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 11 : 10} query={nodes} empty={t("dashboard.noNodes")} />
          )}
```

`web/src/nodes/NodesPage.tsx`에서

```tsx
                {node.status.bridges.map((b) => b.name).join(", ") || "—"}
              </TableCell>
```

을 다음으로 바꾼다:

```tsx
                {node.status.bridges.map((b) => b.name).join(", ") || "—"}
              </TableCell>
              <TableCell>
                {Object.keys(node.status.pools).length === 0
                  ? "—"
                  : Object.entries(node.status.pools)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([name, state]) => (
                        <PoolStatus
                          key={name}
                          name={name}
                          state={state}
                          offline={!node.status.online}
                        />
                      ))}
              </TableCell>
```

`web/src/storage/PoolStatus.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import type { PoolState } from "@/storage/pools";

/**
 * 한 노드에서 본 풀 상태: `이름: 준비됨`, 준비되지 않았으면 그 아래에 노드가 알린 이유. 꺼진 노드는
 * 알린 상태와 상관없이 오프라인이다.
 */
export function PoolStatus({
  name,
  state,
  offline = false,
}: {
  name: string;
  state: PoolState | undefined;
  offline?: boolean;
}) {
  const { t } = useTranslation();
  if (offline) {
    return (
      <div>
        {name}: <span className="text-muted-foreground">{t("pools.state.offline")}</span>
      </div>
    );
  }
  return (
    <div>
      {name}:{" "}
      {state === undefined ? (
        <span className="text-muted-foreground">{t("pools.state.unknown")}</span>
      ) : state.ready ? (
        t("pools.state.ready")
      ) : (
        <span className="text-destructive">{t("pools.state.notReady")}</span>
      )}
      {state?.error && <div className="text-xs text-muted-foreground">{state.error}</div>}
    </div>
  );
}
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, poolState, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
    "storage-pool": [pool("local"), pool("busy", { nodes: ["n1"] })],
    node: [NODE],
    volume: [volume("disk", "busy")],
```

을 다음으로 바꾼다:

```tsx
    "storage-pool": [pool("local"), pool("busy", { nodes: ["n1"] })],
    node: [
      {
        ...NODE,
        status: { ...NODE.status, pools: { local: poolState("local"), busy: poolState("busy") } },
      },
    ],
    volume: [volume("disk", "busy")],
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  const local = (await within(table).findByText("local")).closest("tr")!;
  expect(local).toHaveTextContent("/srv/local");
  expect(local).toHaveTextContent("All nodes");
  expect(local).toHaveTextContent("64 GiB / 128 GiB");
  const busy = within(table).getByText("busy").closest("tr")!;
  expect(busy).toHaveTextContent("node-a");
  expect(within(busy).getByRole("button", { name: "Delete busy" })).toBeDisabled();
```

을 다음으로 바꾼다:

```tsx
  const local = (await within(table).findByText("local")).closest("tr")!;
  expect(local).toHaveTextContent("Directory");
  expect(local).toHaveTextContent("/srv/local");
  expect(local).toHaveTextContent("All nodes");
  expect(local).toHaveTextContent("node-a: Ready");
  // 용량은 노드가 알린 값이다.
  expect(local).toHaveTextContent("64 GiB / 128 GiB");
  const busy = within(table).getByText("busy").closest("tr")!;
  expect(busy).toHaveTextContent("node-a");
  expect(busy).not.toHaveTextContent("All nodes");
  expect(within(busy).getByRole("button", { name: "Delete busy" })).toBeDisabled();
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(local).not.toHaveTextContent("Delete its volumes first.");
});
```

을 다음으로 바꾼다:

```tsx
  expect(local).not.toHaveTextContent("Delete its volumes first.");
});

it("shows each node's pool state and space", async () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "fd00::5", export: "/srv/vm" } });
  const b = { ...NODE, id: "n2", name: "node-b" };
  const c = { ...NODE, id: "n3", name: "node-c" };
  // 꺼진 노드는 준비됐다고 알렸어도 오프라인이고 용량을 보이지 않는다.
  const d = {
    ...NODE,
    id: "n0",
    name: "node-d",
    status: {
      ...NODE.status,
      online: false,
      pools: { nas: poolState("nas", { available_bytes: 9 * 1024 ** 3 }) },
    },
  };
  mockLists({
    "storage-pool": [nas, pool("local")],
    node: [
      d,
      {
        ...NODE,
        status: {
          ...NODE.status,
          pools: {
            nas: poolState("nas", { ready: false, error: "access denied by server" }),
            local: poolState("local", { available_bytes: 1024 ** 3 }),
          },
        },
      },
      {
        ...b,
        status: {
          ...b.status,
          pools: {
            nas: poolState("nas"),
            local: poolState("local", { available_bytes: 2 * 1024 ** 3 }),
          },
        },
      },
      c,
    ],
    volume: [],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const row = (await within(table).findByText("nas")).closest("tr")!;
  expect(row).toHaveTextContent("NFS");
  expect(row).toHaveTextContent("[fd00::5]:/srv/vm");
  expect(row).toHaveTextContent("node-a: Not ready");
  expect(row).toHaveTextContent("access denied by server");
  expect(row).toHaveTextContent("node-b: Ready");
  expect(row).toHaveTextContent("node-c: No status reported yet");
  expect(row).toHaveTextContent("node-d: Offline");
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[4];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[4]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
  );
});
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("creates an NFS pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("nas"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "nas");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "nfs");
  expect(within(dialog).queryByLabelText("Directory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "[[fd00::5]"); // `[[`는 글자 `[`다.
  await userEvent.type(within(dialog).getByLabelText("Export path"), "srv/vm");
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2, hard");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("NFS server")).toHaveAccessibleDescription(
    "Enter a host name or IP address without spaces, commas, slashes or brackets.",
  );
  expect(within(dialog).getByLabelText("Export path")).toHaveAccessibleDescription(
    "Enter an absolute path (starting with /) without control characters.",
  );
  const optionsError =
    "Separate the options with commas, without spaces. bind, rbind, remount, move, suid, dev, exec, helper, bg and x-* options are not allowed.";
  expect(within(dialog).getByLabelText("Mount options (optional)")).toHaveAccessibleDescription(
    optionsError,
  );
  // 서버처럼 마운트의 성격을 바꾸는 옵션도 받지 않는다.
  await userEvent.clear(within(dialog).getByLabelText("Mount options (optional)"));
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2,Exec");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Mount options (optional)")).toHaveAccessibleDescription(
    optionsError,
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("NFS server"));
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "fd00::5");
  await userEvent.clear(within(dialog).getByLabelText("Export path"));
  await userEvent.type(within(dialog).getByLabelText("Export path"), "/srv/vm");
  await userEvent.clear(within(dialog).getByLabelText("Mount options (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool nas.")).toBeInTheDocument();
  // 옵션을 비우면 보내지 않고, 공유 여부는 서버가 정한다.
  expect(body).toEqual({
    name: "nas",
    spec: { backend: { kind: "nfs", server: "fd00::5", export: "/srv/vm" }, nodes: [] },
  });
});

it("sends NFS mount options", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("nas"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "nas");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "nfs");
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "nas.lan");
  await userEvent.type(within(dialog).getByLabelText("Export path"), "/srv/vm");
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2,hard");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "nas",
      spec: {
        backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm", options: "vers=4.2,hard" },
        nodes: [],
      },
    }),
  );
});
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
  nfsExportError,
  nfsOptionsError,
  nfsServerError,
  poolCapacity,
  poolLocation,
  poolNodes,
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

function CreatePoolDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"dir" | "nfs">("dir");
  const [path, setPath] = useState("");
  const [nfsServer, setNfsServer] = useState("");
  const [nfsExport, setNfsExport] = useState("");
  const [nfsOptions, setNfsOptions] = useState("");
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
    const backend =
      kind === "dir"
        ? { kind, path }
        : {
            kind,
            server: nfsServer,
            export: nfsExport,
            ...(nfsOptions !== "" && { options: nfsOptions }),
          };
    await call(api.POST("/storage-pools", { body: { name, spec: { backend, nodes: known } } }));
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
            onChange={(e) => setKind(e.target.value as "dir" | "nfs")}
            {...a11y}
          >
            <NativeSelectOption value="dir">{t("pools.kinds.dir")}</NativeSelectOption>
            <NativeSelectOption value="nfs">{t("pools.kinds.nfs")}</NativeSelectOption>
          </NativeSelect>
        )}
      </Field>
      {kind === "dir" ? (
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
      ) : (
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

`web/src/storage/UploadDialog.test.tsx`에서

```tsx

import { poolsOnNode } from "@/storage/UploadDialog";
import { resetUploads } from "@/storage/upload";
import { VolumesPage } from "@/storage/VolumesPage";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx

import { resetUploads } from "@/storage/upload";
import { VolumesPage } from "@/storage/VolumesPage";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool, poolState } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

`web/src/storage/UploadDialog.test.tsx`에서

```tsx

it("offers only the pools of the node it is connected to", () => {
  const pools = [pool("all"), pool("here", { nodes: ["n1"] }), pool("far", { nodes: ["n2"] })];
  expect(poolsOnNode(pools, "n1").map((p) => p.name)).toEqual(["all", "here"]);
  expect(poolsOnNode(pools, undefined)).toEqual([]);
});

it("uploads a chosen file into a new volume", async () => {
  mockLists({ "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] })], node: [NODE] });
  server.use(
```

을 다음으로 바꾼다:

```tsx

const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });

it("uploads a chosen file into a new volume", async () => {
  // 접속한 노드(n1)에서 쓸 수 없는 풀(다른 노드의 풀, 준비되지 않은 NFS 풀)은 고를 수 없다.
  const here = {
    ...NODE,
    status: { ...NODE.status, pools: { nas: poolState("nas", { ready: false, error: "down" }) } },
  };
  mockLists({
    "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] }), nas],
    node: [here],
  });
  server.use(
```

`web/src/storage/UploadDialog.tsx`에서

```tsx
import type { components } from "@/api/schema";
import type { StoragePool } from "@/api/types";
import { Field } from "@/components/Field";
```

을 다음으로 바꾼다:

```tsx
import type { components } from "@/api/schema";
import { Field } from "@/components/Field";
```

`web/src/storage/UploadDialog.tsx`에서

```tsx
import { formatBytes } from "@/lib/format";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
```

을 다음으로 바꾼다:

```tsx
import { formatBytes } from "@/lib/format";
import { poolsUsableOn } from "@/storage/pools";
import { nameFromFile, sourceFormatOf, startUpload } from "@/storage/upload";
```

`web/src/storage/UploadDialog.tsx`에서

```tsx
type Schemas = components["schemas"];

/** 서버는 받은 노드에서 쓸 수 있는 풀로만 가져온다(노드를 정하지 않은 풀은 모든 노드). */
export function poolsOnNode(pools: StoragePool[], node: string | undefined): StoragePool[] {
  return node === undefined
    ? []
    : pools.filter((p) => (p.spec.nodes ?? []).length === 0 || p.spec.nodes!.includes(node));
}
```

을 다음으로 바꾼다:

```tsx
type Schemas = components["schemas"];
```

`web/src/storage/UploadDialog.tsx`에서

```tsx
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const usable = poolsOnNode(pools.data ?? [], cluster.data?.node);
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
```

을 다음으로 바꾼다:

```tsx
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  // 서버는 받은 노드에서 쓸 수 있는(준비된) 풀로만 가져온다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], cluster.data?.node);
  const here = nodes.data?.find((n) => n.id === cluster.data?.node)?.name ?? "—";
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
it("creates an empty volume and follows its task", async () => {
  mockLists({ "storage-pool": [pool("local")], volume: [volume("root", "local")] });
  let body: unknown;
```

을 다음으로 바꾼다:

```tsx
it("creates an empty volume and follows its task", async () => {
  // 어느 노드에서도 쓸 수 없는 풀(상태를 알린 노드가 없는 NFS 풀)은 고를 수 없다.
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });
  mockLists({
    "storage-pool": [pool("local"), nas],
    volume: [volume("root", "local")],
    node: [NODE],
  });
  let body: unknown;
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
```

을 다음으로 바꾼다:

```tsx
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();

  const pools = within(within(dialog).getByLabelText("Storage pool")).getAllByRole("option");
  expect(pools.map((o) => o.textContent)).toEqual(["—", "local"]);
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
import { formatBytes } from "@/lib/format";
import { UploadDialog } from "@/storage/UploadDialog";
```

을 다음으로 바꾼다:

```tsx
import { formatBytes } from "@/lib/format";
import { poolUsableSomewhere } from "@/storage/pools";
import { UploadDialog } from "@/storage/UploadDialog";
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
  const volumes = useList("volume");
  const [name, setName] = useState("");
```

을 다음으로 바꾼다:

```tsx
  const volumes = useList("volume");
  const nodes = useList("node");
  // 서버는 풀을 쓸 수 있는 온라인 노드에 볼륨을 만든다. 그런 노드가 없는 풀은 고를 수 없다.
  const usable = (pools.data ?? []).filter((p) => poolUsableSomewhere(p, nodes.data ?? []));
  const [name, setName] = useState("");
```

`web/src/storage/VolumesPage.tsx`에서

```tsx
            <NativeSelectOption value="">—</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
```

을 다음으로 바꾼다:

```tsx
            <NativeSelectOption value="">—</NativeSelectOption>
            {usable.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
```

`web/src/storage/pools.test.ts`를 만든다:

```ts
import {
  nfsServerError,
  nfsExportError,
  nfsOptionsError,
  poolCapacity,
  poolLocation,
  poolNodes,
  poolsUsableOn,
  poolUsableSomewhere,
} from "@/storage/pools";
import { NODE, pool, poolState } from "@/test/fixtures";

const GIB = 1024 ** 3;
const nfs = (server: string) =>
  pool("nas", { backend: { kind: "nfs", server, export: "/srv/vm" }, shared: true });
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
});
```

`web/src/storage/pools.ts`를 만든다:

```ts
import type { components } from "@/api/schema";
import type { Node, StoragePool } from "@/api/types";

export type PoolState = components["schemas"]["PoolState"];

/** 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`) */
export function poolLocation(pool: StoragePool): string {
  const b = pool.spec.backend;
  if (b.kind === "dir") {
    return b.path;
  }
  return b.server.includes(":") ? `[${b.server}]:${b.export}` : `${b.server}:${b.export}`;
}

/** 모든 노드가 같은 데이터를 보는 풀인지. NFS 풀은 늘 공유다(서버의 `is_shared`). */
export function isShared(pool: StoragePool): boolean {
  return pool.spec.shared === true || pool.spec.backend.kind === "nfs";
}

/**
 * NFS 서버 입력 오류(번역 키). 서버처럼 공백·`,`·`/`·`[`·`]`가 없고 `-`로 시작하지 않아야 한다.
 * `:`는 IPv6 주소에 쓰므로 여기서 막지 않는다(나머지는 서버가 본다).
 */
export function nfsServerError(server: string): string | undefined {
  return server === "" || server.startsWith("-") || /[\s,/[\]]/.test(server)
    ? "pools.errors.server"
    : undefined;
}

/** 서버의 `char::is_control`처럼 C0·C1 제어 문자와 DEL */
function hasControl(text: string): boolean {
  return [...text].some((c) => {
    const code = c.charCodeAt(0);
    return code < 0x20 || (code >= 0x7f && code <= 0x9f);
  });
}

/** NFS 내보내기 입력 오류(번역 키). 서버처럼 절대 경로이고 제어 문자가 없어야 한다. */
export function nfsExportError(path: string): string | undefined {
  return !path.startsWith("/") || hasControl(path) ? "pools.errors.export" : undefined;
}

/** 서버가 받지 않는 마운트 옵션(`=` 앞 이름, 대소문자 무시). 서버의 `denied_option`과 같다. */
const DENIED_OPTIONS = ["bind", "rbind", "remount", "move", "suid", "dev", "exec", "helper", "bg"];

/**
 * NFS 마운트 옵션 입력 오류(번역 키). 서버처럼 비어 있지 않은 쉼표 목록이고, 공백·제어 문자와
 * 마운트의 성격을 바꾸는 옵션(`bind`, `remount`, `exec`, `x-*` 등)이 없어야 한다.
 */
export function nfsOptionsError(options: string): string | undefined {
  const denied = options.split(",").some((option) => {
    const [name = ""] = option.toLowerCase().split("=");
    return DENIED_OPTIONS.includes(name) || name.startsWith("x-");
  });
  return options === "" || /\s/.test(options) || hasControl(options) || denied
    ? "pools.errors.options"
    : undefined;
}

/** 노드가 알린 이 풀의 상태. 같은 이름의 다른 풀(`pool_id`가 다름)의 상태는 없는 것으로 본다. */
function poolStateOn(pool: StoragePool, node: Node): PoolState | undefined {
  const state = node.status.pools[pool.name];
  return state?.pool_id === pool.id ? state : undefined;
}

/**
 * 이 노드에서 풀을 쓸 수 있는지. 서버의 `pool_usable_on`(노드 목록이 허용하고 준비됐다고 알림 —
 * 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다.
 */
function usableOn(pool: StoragePool, node: Node): boolean {
  const listed = pool.spec.nodes ?? [];
  if (!node.status.online || (listed.length > 0 && !listed.includes(node.id))) {
    return false;
  }
  const state = poolStateOn(pool, node);
  return state === undefined ? pool.spec.backend.kind === "dir" : state.ready;
}

/** 노드 `nodeId`에서 쓸 수 있는 풀들. 노드를 아직 모르면(읽는 중) 없다. */
export function poolsUsableOn(
  pools: StoragePool[],
  nodes: Node[],
  nodeId: string | undefined,
): StoragePool[] {
  const node = nodes.find((n) => n.id === nodeId);
  return node === undefined ? [] : pools.filter((p) => usableOn(p, node));
}

/** 온라인 노드 가운데 하나에서라도 풀을 쓸 수 있는지(서버가 볼륨을 만들 노드를 고를 수 있는지) */
export function poolUsableSomewhere(pool: StoragePool, nodes: Node[]): boolean {
  return nodes.some((n) => usableOn(pool, n));
}

/**
 * 풀을 쓸 수 있는 노드와 그 노드가 알린 풀 상태. 노드 목록이 비었으면 모든 노드다. 꺼진 노드는
 * `offline`이다.
 */
export function poolNodes(
  pool: StoragePool,
  nodes: Node[],
): { id: string; name: string; state: PoolState | undefined; offline: boolean }[] {
  const listed = pool.spec.nodes ?? [];
  const ids = listed.length === 0 ? nodes.map((n) => n.id) : listed;
  return ids.map((id) => {
    const node = nodes.find((n) => n.id === id);
    return {
      id,
      // 목록에 없는 노드는 id 끝자리로 보인다.
      name: node?.name ?? id.slice(-8),
      state: node && poolStateOn(pool, node),
      offline: node !== undefined && !node.status.online,
    };
  });
}

/**
 * 노드가 알린 풀 용량. 공유 풀은 첫 준비된 온라인 노드의 값 하나(`node`는 null), 로컬 풀은 준비된
 * 온라인 노드마다.
 */
export function poolCapacity(
  pool: StoragePool,
  nodes: Node[],
): { node: string | null; free: number; total: number }[] {
  const ready = poolNodes(pool, nodes).flatMap(({ name, state, offline }) =>
    !offline && state?.ready
      ? [{ node: name, free: state.available_bytes, total: state.total_bytes }]
      : [],
  );
  return isShared(pool) ? ready.slice(0, 1).map((c) => ({ ...c, node: null })) : ready;
}
```

`web/src/test/fixtures.ts`에서

```ts
import type { Node, Snapshot, StoragePool, Task, Vm, Volume } from "@/api/types";
```

을 다음으로 바꾼다:

```ts
import type { Node, Snapshot, StoragePool, Task, Vm, Volume } from "@/api/types";
import type { PoolState } from "@/storage/pools";
```

`web/src/test/fixtures.ts`에서

```ts
    spec: { backend: { kind: "dir", path: `/srv/${name}` }, nodes: [], ...extra },
    status: { available_bytes: 64 * 1024 ** 3, total_bytes: 128 * 1024 ** 3 },
  };
```

을 다음으로 바꾼다:

```ts
    spec: { backend: { kind: "dir", path: `/srv/${name}` }, nodes: [], ...extra },
    // 서버는 풀의 용량을 비워 두고, 노드가 풀 상태에 용량을 알린다.
    status: { available_bytes: null, total_bytes: null },
  };
}

/** 노드가 알린 풀 `name`(`pool(name)`)의 상태. 기본은 준비된 128 GiB 풀이다. */
export function poolState(name: string, extra: Partial<PoolState> = {}): PoolState {
  return {
    pool_id: pool(name).id,
    ready: true,
    error: null,
    total_bytes: 128 * 1024 ** 3,
    available_bytes: 64 * 1024 ** 3,
    ...extra,
  };
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, snapshot, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, poolState, snapshot, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx

it("picks the node like the server's vm_node", () => {
```

을 다음으로 바꾼다:

```tsx

it("offers only pools ready on the VM's node and points at one that is not", async () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });
  const fast = pool("fast", { backend: { kind: "nfs", server: "fast.lan", export: "/srv/vm" } });
  const here = {
    ...NODE,
    status: {
      ...NODE.status,
      pools: { nas: poolState("nas", { ready: false, error: "down" }), fast: poolState("fast") },
    },
  };
  mockLists({
    vm: [db],
    node: [here],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local"), nas, fast],
  });
  // 대화상자를 연 사이 준비되지 않게 됐다.
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), () =>
      HttpResponse.json({ code: "pool_unavailable", message: "down" }, { status: 409 }),
    ),
  );
  openDetail(db);
  await userEvent.click(await screen.findByRole("button", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  const pools = within(dialog).getByLabelText("Storage pool for the disks");
  await waitFor(() =>
    expect(
      within(pools)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Same pool as each source disk", "local", "fast"]),
  );
  await userEvent.selectOptions(pools, "fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() =>
    expect(pools).toHaveAccessibleDescription(
      "That storage pool is not ready on the node holding the VM.",
    ),
  );
  expect(pools).toBeInvalid();
});

it("picks the node like the server's vm_node", () => {
```

`web/src/vms/VmCloneDialog.test.tsx`에서

```tsx
    vm: [free],
    node: [NODE],
    volume: [volume("db-root", "local", { node: null })],
```

을 다음으로 바꾼다:

```tsx
    vm: [free],
    node: [NODE, { ...NODE, id: "n2", name: "node-b" }],
    volume: [volume("db-root", "local", { node: null })],
```

`web/src/vms/VmCloneDialog.tsx`에서

```tsx
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { poolsOnNode } from "@/storage/UploadDialog";
import { trackTask } from "@/tasks/tasks";
```

을 다음으로 바꾼다:

```tsx
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { poolsUsableOn } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";
```

`web/src/vms/VmCloneDialog.tsx`에서

```tsx
  const volumes = useList("volume");
  // 만들어진 차례(UUIDv7)의 역순: 최근 스냅샷이 위에 온다.
```

을 다음으로 바꾼다:

```tsx
  const volumes = useList("volume");
  const nodes = useList("node");
  // 만들어진 차례(UUIDv7)의 역순: 최근 스냅샷이 위에 온다.
```

`web/src/vms/VmCloneDialog.tsx`에서

```tsx
  const node = cloneNode(vm, volumes.data, cluster.data?.node);
  const usable = poolsOnNode(pools.data ?? [], node);
  // 서버는 시작·정지 중인 VM을 복제하지 않는다(vm_busy).
```

을 다음으로 바꾼다:

```tsx
  const node = cloneNode(vm, volumes.data, cluster.data?.node);
  // 복제 Task는 그 노드에서 돈다. 그 노드에서 준비된 풀만 고른다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], node);
  // 서버는 시작·정지 중인 VM을 복제하지 않는다(vm_busy).
```

`web/src/vms/VmCloneDialog.tsx`에서

```tsx
        setErrors({ pool: "clone.errors.poolNotOnNode" });
        return false;
```

을 다음으로 바꾼다:

```tsx
        setErrors({ pool: "clone.errors.poolNotOnNode" });
        return false;
      }
      // 대화상자를 연 사이 그 노드에서 풀이 준비되지 않게 됐다.
      if (err instanceof ApiError && err.code === "pool_unavailable") {
        setErrors({ pool: "clone.errors.poolUnavailable" });
        return false;
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 269개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): NFS 풀 만들기와 노드별 풀 상태 보기"
```

---

### Task 8: 문서

README에 "스토리지 풀과 NFS"(만들기 예, Linux·root·`nfs-common`, 마운트 위치와 언마운트, 항상 공유, 마운트 옵션 규칙, 노드별 상태와 `[pools] refresh_secs`, 배치 규칙, `pool_unavailable`, VM 시작 재시도, 업그레이드 주의, `rm -rf {data_dir}` 주의), `nfs-it` 실행 방법, 문서 목록을 더한다. 설계 문서 §3–§10에 구현하며 정한 것을 적는다.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지)
```

`README.md`에서

```markdown
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
TOML
```

을 다음으로 바꾼다:

```markdown
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [pools]
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
TOML
```

`README.md`에서

```markdown
노드는 브리지 목록을 시작할 때와 1분마다 알린다.
```

을 다음으로 바꾼다:

````markdown
노드는 브리지 목록을 시작할 때와 1분마다 알린다.

## 스토리지 풀과 NFS

풀은 두 종류다. **디렉터리 풀**(`--path`)은 노드의 디렉터리이고, **NFS 풀**(`--nfs`)은 NFS 서버의 내보내기를
opld가 노드마다 마운트한 것이다. 어느 쪽이든 볼륨은 그 안의 qcow2·raw 파일이다.

```bash
sudo apt install nfs-common                          # 노드마다 NFS 클라이언트(mount.nfs)
oplctl pool create nas --nfs nas.lan:/srv/vm         # IPv6 서버는 --nfs '[fd00::5]:/srv/vm'
oplctl pool create nas2 --nfs 10.0.0.5:/srv/vm --nfs-options vers=4.2,hard   # mount -o nosuid,nodev,noexec 뒤에 붙는다
oplctl pool list                                     # NAME KIND LOCATION NODES READY(준비된 노드/쓸 수 있는 노드)
oplctl pool get nas                                  # 노드별 준비·용량·오류(-o json: 풀 리소스 그대로)
                                                     # 꺼진 노드는 node offline, 목록의 없는 노드는 node not found
```

- **NFS는 Linux 노드에서 root인 opld만** 마운트한다. 내보내기는 opld(root)가 쓸 수 있어야 한다(예: `no_root_squash`).
  macOS 노드는 NFS 풀을 마운트하지 않고 "nfs pools are not supported on macOS"로 알린다. 옵션을 주지 않으면 커널
  기본값으로 마운트한다.
- **마운트 옵션.** opld는 늘 `nosuid,nodev,noexec`를 붙이고 `--nfs-options`를 그 뒤에 붙인다. 마운트의 성격을 바꾸는
  옵션(`bind`, `rbind`, `remount`, `move`, `suid`, `dev`, `exec`, `helper`, `bg`, `x-*`·`X-mount.*`)은 **400**으로 받지 않는다.
- **마운트 위치.** `{data_dir}/pools/{풀 id}`. opld가 내려가도 마운트는 그대로 두고, 다시 올라오면 그대로 쓴다.
  풀을 지우면(볼륨이 남아 있으면 지금처럼 409) 각 노드가 언마운트하고 마운트 지점을 지운다. 그 풀의 디스크를 쓰는
  VM이 그 노드에서 돌고 있으면 VM이 멈출 때까지 미룬다(`umount -f`는 쓰지 않는다). 풀을 쓸 노드는 만들 때 `--node`로
  정한다(생략하면 모든 노드).
- **주의: `{data_dir}`를 통째로 지우지 않는다.** 마운트된 NFS 풀이 있는 채로 `rm -rf /var/lib/opl`을 하면 마운트를
  따라 들어가 NFS 내보내기의 볼륨 파일까지 지운다. 먼저 opld를 멈추고 `{data_dir}/pools` 아래를 `umount`한다.
- **NFS 풀은 늘 공유 풀이다.** API에서 `shared`를 적지 않으면 `true`가 되고, `false`면 **400** `invalid_request`다.
  opld 밖에서(fstab 등) 마운트한 공유 디렉터리는 지금처럼 디렉터리 풀에 `"shared": true`로 쓴다.
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
  볼륨이 있는 노드에서 한다. 이미지 가져오기와 복제는 풀이 그 노드의 목록에 없으면 지금처럼 `pool_not_on_node`,
  목록에는 있지만 준비되지 않았으면 `pool_unavailable`(노드가 알린 오류)이다. 웹의 업로드·복제·볼륨 만들기
  대화상자는 그 노드에서(볼륨 만들기는 어느 온라인 노드에서든) 쓸 수 있는 풀만 보인다.
- **VM 시작.** 노드가 준비되지 않았다고 알린 풀이나 이 노드에 실제로 마운트되지 않은 NFS 풀의 디스크가 있으면
  VM은 꺼지지 않고 `starting`에 머물며(`last_error`에 이유) 잠시 뒤 다시 시도한다. 일시 오류라 실패 횟수로 세지
  않으므로 NFS 서버가 돌아오면 시작한다. 드라이버도 마운트되지 않은 NFS 풀(빈 마운트 지점)에는 볼륨을 만들거나
  읽지 않는다.
- **올릴 때.** 새 opld는 첫 확인(시작 직후)에서 풀 상태를 채운다. 디렉터리 풀은 그 전에도 쓸 수 있다고 보지만,
  디렉터리가 없는 노드는 첫 확인 뒤 그 풀이 준비되지 않은 것으로 나온다.
````

`README.md`에서

```markdown
- **업로드 사전 확인.** 웹과 `oplctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에 있는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
```

을 다음으로 바꾼다:

```markdown
- **업로드 사전 확인.** 웹과 `oplctl volume import`는 파일을 보내기 전에 받을 수 있는지 먼저 묻는다
  (`POST /api/v1/volumes/import/check`): 권한, 이름, 풀이 있는지, 풀이 접속한 노드에서 준비됐는지, 같은 이름의 볼륨이
  있는지, 파일 크기 + 1 GiB의 빈 공간이 있는지. 거절되면 파일을 보내지 않고 이유를 보인다.
```

`README.md`에서

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
```
````

을 다음으로 바꾼다:

````markdown
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target cargo test -p opld --features qemu-it,net-it --test bridge  # E2E: 브리지 네트워크
# Linux, root: 실제 NFS 풀. nfs-kernel-server로 127.0.0.1에 rw,no_root_squash로 내보낸 디렉터리가 필요하다.
sudo -E env "PATH=$PATH" CARGO_TARGET_DIR=/tmp/opl-root-target OPL_TEST_NFS_EXPORT=/srv/opl-nfs cargo test -p opld --features nfs-it --test nfs
```

`nfs-it` 시험은 opld가 내보내기를 마운트하고, 그 풀에 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤, 풀을 지우면
언마운트하는지 본다(CI `linux-kvm` 작업이 `/srv/opl-nfs`를 내보낸다). **내보내기는 버려도 되는 디렉터리**를 쓴다.
시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 마운트를 풀며, 풀지 못하면 임시 디렉터리를 지우지 않고
남긴다(그 경로를 출력한다). NFS 서버를 막 시작했으면 잠금 유예(약 90초)
때문에 첫 실행이 느릴 수 있다.
````

`README.md`에서

```markdown
- [웹 정리 설계 (계획 14)](docs/superpowers/specs/2026-10-28-web-cleanup-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [웹 정리 설계 (계획 14)](docs/superpowers/specs/2026-10-28-web-cleanup-design.md)
- [공유 스토리지 설계 1: 백엔드 구조와 NFS (계획 15)](docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-28-shared-storage-nfs-design.md` 전체:

```markdown
# opLite 공유 스토리지 설계 1: 백엔드 구조와 NFS (계획 15)

작성일: 2026-10-28
상위 문서: `2026-10-20-opl-node-core-design.md`(§6 스토리지), `2026-10-26-snapshots-design.md`

## 1. 하위 프로젝트 3 나머지의 분해

공유 스토리지(NFS, LVM-thin, ZFS, iSCSI, Ceph)는 한 계획으로 하기에 크다. 지금 코드는 `dir` 백엔드 하나, 파일 경로로만 넘기는 디스크, qcow2 내부 스냅샷을 전제로 한다. 아래 순서로 나눈다. 각 계획은 혼자서 동작하는 소프트웨어를 낸다.

| 계획 | 범위 | 새로 필요한 바탕 |
|---|---|---|
| **15 (이 문서)** | 백엔드 구조, `shared`의 뜻 바로잡기, 노드별 풀 상태·용량, **NFS** 풀 | 백엔드별 풀 경로, 노드가 알리는 풀 상태 |
| 16 | 블록 백엔드 바탕과 **LVM-thin** | 디스크 소스 추상화(`host_device`), 백엔드 스냅샷, 메모리 스냅샷의 vmstate 위치 |
| 17 | **ZFS**(zvol) | 16의 블록 바탕 |
| 18 | **iSCSI**, **Ceph RBD** | QEMU 네트워크 블록 드라이버(`iscsi`, `rbd`) |

하위 프로젝트 4(라이브 마이그레이션)는 그 뒤다. UEFI 변수를 공유 스토리지로 옮기는 일도 거기서 다룬다.

## 2. 계획 15 목표와 범위

| 항목 | 내용 |
|---|---|
| 백엔드 구조 | 저장소 코드가 `PoolBackend`의 종류를 가정하지 않게, 풀의 루트 디렉터리를 백엔드별로 정한다 |
| NFS 풀 | `PoolBackend::Nfs { server, export, options }`. opld가 풀을 쓸 수 있는 노드마다 마운트한다 |
| 노드별 풀 상태 | 노드가 자기에게서 풀을 쓸 수 있는지·용량을 `NodeStatus.pools`로 알린다 |
| `shared` 바로잡기 | 스케줄러·Task 노드 선택·복제·가져오기가 `shared`와 볼륨이 있는 노드를 본다 |
| 클라이언트 | oplctl `pool create --nfs`, 웹의 풀 만들기·풀 목록(종류, 노드별 상태) |

넣지 않는 것: macOS 노드의 NFS 마운트(macOS는 `dir`만), NFS 서버 관리, 마운트를 opld 밖(fstab)에 맡기는 방식의 자동 감지(그런 경우는 지금처럼 `dir` + `shared: true`), UEFI 변수 공유(하위 프로젝트 4), 블록 백엔드(계획 16 이후).

## 3. 데이터 모델

- `PoolBackend`에 `Nfs { server: String, export: String, options: Option<String> }`를 더한다(serde `{"kind":"nfs", ...}`, `deny_unknown_fields`).
  - `server`: 호스트 이름 또는 IP(공백·`:`·`,` 없음, IPv6는 `[...]` 없이 적고 마운트할 때 감싼다). 구현: IPv6 주소로 읽히면 통과, 아니면 비어 있지 않고 `-`로 시작하지 않으며(`mount`가 옵션으로 읽지 않게) 공백·`:`·`,`·`/`·`[`·`]` 없는 ASCII 출력 문자.
  - `export`: `/`로 시작하는 절대 경로(제어 문자 금지).
  - `options`: `mount -o`에 넘길 쉼표 목록(예: `vers=4.2,hard`). 빈 문자열·공백·제어 문자 금지. 마운트의 성격을 바꾸는 옵션도 받지 않는다: `=` 앞 이름(대소문자 무시)이 `bind`·`rbind`·`remount`·`move`·`suid`·`dev`·`exec`·`helper`·`bg`이거나 `x-`로 시작하는 것(`X-mount.*` 포함). `bg`는 마운트하지 못해도 뒤에서 다시 시도하며 성공으로 돌아오므로 받지 않는다. opld는 늘 `nosuid,nodev,noexec`를 앞에 붙이고 사용자 옵션을 그 뒤에 붙인다. 옵션이 없으면 `vers=4.2`가 아니라 커널 기본값을 쓴다(`nosuid,nodev,noexec`만).
  - 검사는 `StoragePoolSpec::validate() -> Result<(), PoolError>`(`opl-types`, `RelativePath`·`BadServer`·`BadExport`·`BadOptions`)이고, API가 풀을 만들 때 부른다. 거절은 400 `invalid_request`다.
- NFS 풀은 항상 공유다. API는 NFS 풀을 만들 때 `shared`가 없으면 `true`로 두고 `false`면 400으로 거절한다. 오류 code는 따로 만들지 않고 기존 `invalid_request`다(이 문서 처음의 `invalid`는 없는 code였다). `shared`를 적었는지 알아야 하므로 `create_pool`은 spec을 JSON 값으로 받아 키를 본 뒤 `StoragePoolSpec`으로 읽는다.
- `fn is_shared(pool) -> bool`(`opl-types`): `spec.shared || backend가 Nfs`.
- `NodeStatus.pools: BTreeMap<String, PoolState>`(풀 이름 → 상태, `#[serde(default)]`):
  - `PoolState { pool_id: Uuid, ready: bool, error: Option<String>, total_bytes: u64, available_bytes: u64 }`. `pool_id`는 상태를 적을 때의 풀 id다(serde default, 이 필드 전의 상태는 nil). 같은 이름으로 지우고 다시 만든 풀에 옛 상태를 쓰지 않게, 읽는 쪽(`pool_state_on`과 그것을 쓰는 `pool_usable_on`·`pool_error_on`, reconciler, oplctl, 웹)은 `pool_id`가 풀 id와 같을 때만 그 상태를 믿고, 다르면 "알리지 않음"으로 본다.
  - 노드는 쓸 수 있는 풀(`nodes`가 비었거나 자기를 포함)만 적는다. 없는 풀 이름은 지운다.
  - opld는 Node 리소스를 등록할 때 이미 적힌 `pools`를 그대로 두고(다시 시작해도 지우지 않는다, 확인된 쓰기 `update_node_status`), `PoolMounter`가 채운다.
- `StoragePoolStatus`의 `total_bytes`·`available_bytes`는 지금처럼 비워 둔다(노드별 값이 기준). 웹·oplctl은 노드별 값을 보인다.

## 4. 풀 루트와 저장소 드라이버

- 저장소 함수들은 풀 스펙 대신 풀 리소스(`&StoragePool`, id·이름 포함)를 받는다. 기존 `&StoragePoolSpec` 인자를 모두 바꾼다(기계적 변경).
- `opl-driver::storage::pool_root(pool, mount_root) -> PathBuf`:
  - `Dir { path }` → `path`
  - `Nfs { .. }` → `{mount_root}/{pool.id}`
- `DirStorage::new(qemu_img, mount_root)`. `mount_root`는 opld의 `{data_dir}/pools`. 파일 배치(`{root}/{id}.qcow2|raw`), 하드 링크로 넣기, qemu-img 사용, qcow2 내부 스냅샷은 그대로다(NFSv3·v4 모두 하드 링크를 지원한다).
- 볼륨 파일 경로는 `DirStorage::volume_path(pool, mount_root, id, format)`(정적 함수)로 만든다.
- **마운트 확인.** `check_pool_root(pool, mount_root) -> Result<PathBuf, DriverError>`(공개)는 NFS 풀이면 `{canonicalize한 mount_root}/{id}`가 `/proc/self/mountinfo`의 `nfs`/`nfs4` 마운트 지점인지 보고, 아니면(mountinfo를 읽을 수 없거나 Linux가 아니어도) `DriverError::PoolNotMounted(path)`다. 마운트 지점 자체는 stat하지 않는다(처음엔 `st_dev`를 부모와 비교했지만 NFS 서버가 멈추면 stat이 끝나지 않는다). mountinfo 해석은 `opl_driver::mounts::nfs_mount_points`이고 opld의 마운터도 이것을 쓴다. 볼륨 파일이 없다는 결과(`VolumeNotFound`)는 다시 확인해 그사이 마운트가 풀렸으면 `PoolNotMounted`로 돌려준다(빈 마운트 지점을 보고 "없음"으로 판단하지 않게). 이 오류는 일시 오류(`is_transient`)다. `DirStorage`의 모든 진입점(만들기·가져오기·지우기·복사·스냅샷·크기·정보·용량)이 이 확인을 거쳐, 마운트되지 않은 빈 마운트 지점(로컬 디스크)에 볼륨을 만들거나 그 자리를 디스크로 쓰지 않는다. 리뷰에서는 `StorageError`라는 이름이 나왔지만 드라이버의 오류 타입은 `DriverError` 하나라 그 변형으로 두었다.
- reconciler의 디스크 경로(`resolve.rs`)도 `pool_root`를 쓴다. 디스크는 여전히 파일 경로다. `mount_root`는 `ReconcilerConfig::new(node, mount_root)`의 `mount_root` 필드로 받아 `resolve::runtime_spec(cluster, vm, node, mount_root)`에 넘긴다.
- 리소스 API의 `PoolBackend::Dir` 분해는 `match`로 바꾸고 백엔드별로 검증한다(`Dir`: 절대 경로, `Nfs`: 위 규칙).

## 5. NFS 마운트 (opld)

`crates/opld/src/pools.rs`의 `PoolMounter`. 노드마다 반복 작업 하나다(브리지 새로 고침처럼 `tick`, 기본 15초, `[pools] refresh_secs`).

- 명령과 파일시스템은 `MountOps` trait(`mountinfo`, `mount`, `umount`, `check`) 뒤에 둔다. 실제 구현은 `SystemMount`이고 테스트는 가짜를 쓴다. 마운트 인자는 `mount_args`, mountinfo 해석은 드라이버의 `opl_driver::mounts::nfs_mount_points`(마운트 지점의 `\040` 같은 8진수 이스케이프를 되돌린다)다. 마운트 지점은 실제 경로(`canonicalize`한 `mount_root` 아래)로 비교한다.
- 풀 리소스를 구독해 풀이 만들어지거나 지워지면 tick을 기다리지 않고 곧바로 새로 고친다(배치와 볼륨 Task가 준비된 노드만 고르므로 만든 풀을 바로 쓸 수 있게). 구독이 끊기면 다음 새로 고침 때 다시 구독한다.
- tick마다 풀 목록을 읽고 이 노드가 쓸 수 있는 풀마다:
  - `Dir`: 디렉터리가 있고 쓸 수 있으면 `ready`, 용량은 statvfs. 쓰기 확인은 NFS와 같은 `check`(`.opl-check-{노드 id}` 파일을 만들고 지운다)이고 같은 10초 제한이다(넘으면 `"pool directory not responding"`). 디렉터리가 없으면 준비되지 않은 것이다(opld가 만들지 않는다).
  - `Nfs`: 마운트 지점 `{data_dir}/pools/{id}`를 만들고, `/proc/self/mountinfo`에서 그 지점에 `nfs`/`nfs4` 마운트가 있는지 본다. 없으면 `mount -t nfs -o nosuid,nodev,noexec[,options] server:export <dir>`(IPv6는 `[server]:export`)를 시간 제한(30초)을 두고 부른다. 실패하면 `ready: false`, `error`에 stderr 첫 줄. 마운트돼 있으면 쓰기 확인(작은 파일 만들고 지우기) 뒤 `ready`, 용량은 statvfs. 마운트 지점 디렉터리는 마운트되지 않았을 때만 만든다(멈춘 NFS 마운트 지점은 stat도 멈춘다).
- **풀마다 따로.** 마운트·언마운트는 풀마다 별도 작업으로 돌고, tick은 기다리지 않는다. 진행 중인 마운트가 있는 풀은 끝나기 전에 새로 마운트하지 않고 `error: "mount in progress"`로 알린다(진행 중인 언마운트도 다시 시작하지 않고 로그만 남긴다). 언마운트 중인 풀을 다시 쓰게 되면 mountinfo에 아직 보여도 준비되지 않은 것(`"mount in progress"`)으로 알리고, 언마운트가 끝난 뒤 다시 마운트한다. 작업이 끝나면 곧바로 새로 고쳐 결과를 적는다. 쓰기 확인도 풀마다 동시에 시작해 같은 10초 안에서 기다린다. 그래서 멈춘 NFS 서버 하나가 다른 풀의 상태 기록(즉시 새로 고침 포함)을 늦추지 않는다(처음 멈춘 확인이 최대 10초, 그 뒤로는 건너뛴다).
- **백오프.** 마운트가 실패하면 그 풀의 다음 시도를 15초부터 실패할 때마다 두 배(최대 5분) 미룬다. 그동안 상태는 마지막 오류다. 마운트되면 백오프를 잊는다.
- 이 노드가 쓸 수 없게 된 NFS 풀(지워졌거나 `nodes`에서 빠짐)의 마운트 지점은 `umount`하고 디렉터리를 지운다. 그 풀의 디스크를 쓰는 QEMU가 이 노드에서 돌고 있으면 언마운트하지 않고 다음 tick에 다시 본다. 판단은 클러스터 상태로 한다: `status.node`가 이 노드이고 phase가 `starting`·`running`·`stopping`인 VM의 디스크 볼륨이 있는 풀. `mount_root` 아래 풀 id 이름의 마운트 지점만 건드리고, 마운트되지 않은 채 남은 빈 마운트 지점도 지운다. 언마운트 실패는 같은 메시지면 한 번만 기록한다. `umount -f`는 쓰지 않는다(쓰던 데이터를 잃을 수 있다). (풀 API에 수정이 없으므로 `nodes`에서 빠지는 경우는 지금은 없다.)
- 결과를 `NodeStatus.pools`에 쓴다(바뀐 경우만, 리비전 충돌이면 다시 읽고 다시 쓴다. `update_status_checked`). 준비 여부나 오류가 바뀔 때만 로그를 남긴다.
- Node status를 쓰는 다른 곳도 리비전을 확인하며 써서 `pools`를 지우지 않는다: 브리지 기록과 opld 종료 때의 오프라인 표시는 `bootstrap::update_node_status`(다시 읽고 고쳐 확인된 쓰기, 충돌이면 다시), 리더의 생존 감시(`opl-peer` monitor)는 충돌이면 쓰지 않고 다음 핑에서 다시 본다. 처음 설계의 "쓰는 쪽은 하나"는 맞지 않았다.
- 설정 `[pools] refresh_secs`(기본 15)는 0이면 opld가 시작하지 않는다. 시간 제한은 상수 `MOUNT_TIMEOUT`(30초, `umount`에도 쓴다)·`CHECK_TIMEOUT`(10초)이다.
- opld가 내려갈 때 마운트는 그대로 둔다(돌던 VM이 없고, 다시 올라오면 그대로 쓴다).
- **macOS:** NFS 풀은 마운트하지 않고 `ready: false`, `error: "nfs pools are not supported on macOS"`.
- 마운트는 root 권한이 필요하다. opld가 root가 아니면 `mount`가 실패하고 그 메시지가 `error`에 남는다.

## 6. 배치와 Task 노드 (`shared` 바로잡기)

공통 도우미 `fn pool_usable_on(pool, node) -> bool`: `nodes`가 비었거나 노드를 포함하고, 노드가 알린 그 풀의 상태(`pool_id`가 같은 것)가 `ready`. 디렉터리 풀은 노드가 상태를 아직 알리지 않았으면(항목 없음, 또는 `pool_id`가 다름) 쓸 수 있는 것으로 보고 `ready: false`를 알렸을 때만 막는다. NFS 풀은 `ready: true`를 알려야 한다.

- **스케줄러 `reaches_disks`:** VM의 디스크 볼륨마다 `pool_usable_on(pool, node)`이고, 풀이 공유가 아니며 `volume.status.node`가 있으면 그 노드여야 한다. (지금은 `nodes`가 빈 로컬 풀에서 파일이 없는 노드를 고를 수 있었다.)
- 도우미는 `opl-types::storage`의 `pool_listed_on(pool, node_id)`(노드 목록만), `pool_state_on(pool, &Node)`(`pool_id`가 같은 상태), `pool_usable_on(pool, &Node)`, `pool_error_on(pool, &Node)`(노드가 알린 오류, 상태가 없으면 `"no status reported yet"`)이다. 상태를 알리지 않은 노드는 NFS 풀에는 준비되지 않은 것이고 디렉터리 풀에는 준비된 것이다. 스케줄러·`pool_node`·`check_pool_on`이 모두 이 규칙을 따른다.
- **볼륨 Task 노드(`pool_node`, `volume_node`):** 볼륨 만들기는 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 쓸 수 있는 **온라인** 노드 가운데 첫째(id 순). 쓸 수 있는 노드가 없으면 409 `pool_unavailable`. 기존 볼륨의 작업(삭제·크기)은 공유가 아니면 `volume.status.node`, 공유면 위와 같은 규칙.
- **가져오기 확인(`check_import`)·복제 대상 풀:** 노드 목록에 없으면 지금처럼 409 `pool_not_on_node`, 노드 목록에는 있지만 준비되지 않았으면 409 `pool_unavailable`(메시지에 노드 상태의 `error`). 공통 확인은 `volumes::check_pool_on`이다. 복제 Task도 실행할 때 이 노드에서 대상 풀이 준비됐는지 다시 본다(마운트되지 않은 NFS 풀의 빈 마운트 지점에 복사본을 만들지 않게).
- **스냅샷의 `check_local`:** `is_shared`를 쓴다(NFS도 통과).
- **VM 시작(reconciler):** 디스크 풀을 이 노드에서 쓸 수 없으면 `ResolveError::PoolUnavailable`(메시지에 풀 상태 오류)이고, 이것은 **일시 오류**다: VM을 끄지 않고(`power`는 그대로) 실패로 세지도 않으며, `retry_interval`(기본 2초) 뒤 다시 시도한다(phase `starting`, `last_error`에 이유). 풀이 돌아오면 시작한다. 노드가 그 풀을 `ready: false`로 알렸으면 거절하고, 아직 알리지 않았으면(opld가 막 시작해 첫 새로 고침 전) 클러스터 상태로는 막지 않는다. NFS 디스크는 그와 별도로 `check_pool_root`로 이 노드에 실제로 마운트돼 있는지 확인한다(마운트되지 않았으면 `PoolUnavailable`). 노드 목록에 없는 풀(`PoolNotOnNode`)·없는 볼륨 같은 참조 오류는 지금처럼 끄고 실패로 남긴다.
- **시작할 때의 고아 볼륨 정리(Task 실행기):** 풀이 아직 마운트되지 않아(`PoolNotMounted`) 지우지 못한 볼륨은 남겨 두고 `TaskRunnerConfig::orphan_retry`(기본 1분)마다 그 볼륨만 다시 지워 본다. 지웠거나 볼륨이 없어지면 목록에서 뺀다.
- 스케줄러에서 볼륨에 `status.node`가 없으면(아직 만들어지지 않음) 노드를 제한하지 않는다.
- 옛 노드는 `pools`가 비어 있다. 혼합 버전 클러스터는 지원하지 않는다(계획 13과 같다). 디렉터리 풀은 알리지 않은 노드도 쓸 수 있다고 보므로 올린 직후에도 배치 후보가 있다. NFS 풀은 첫 마운트가 끝날 때까지 후보가 없다.

## 7. API·클라이언트

- 풀 API는 그대로이고 `PoolBackend`의 새 종류와 `NodeStatus.pools`가 OpenAPI에 나온다. 새 오류 code `pool_unavailable`(409).
- **oplctl:** `pool create NAME --nfs SERVER:/EXPORT [--nfs-options OPTS]`(기존 `--path`와 함께 쓸 수 없다, IPv6는 `[ADDR]:/EXPORT`), `pool list`에 종류와 "준비된 노드 수/쓸 수 있는 노드 수"(열 `NAME KIND LOCATION NODES READY`), 기존 상세 `pool get`의 기본 출력을 풀 정보와 노드별 표(`NODE READY SIZE AVAILABLE ERROR`)로 바꾼다. `-o json`은 풀 리소스 그대로다. 노드는 풀의 `nodes`를 그 순서대로 따르고(비었으면 모든 노드), 목록에 있지만 없는 노드는 그 id와 `node not found`로 보이며 분모에 센다. 오프라인 노드는 `node offline`이고 준비로 세지 않는다. `pool_id`가 다른 상태는 알리지 않은 것으로 본다.
- **웹:** 풀 만들기 대화상자에 종류(디렉터리/NFS) 선택과 NFS 필드(서버, 내보내기 경로, 옵션), 풀 목록에 종류 열과 노드별 상태(준비/오류 이유), 용량은 노드별 값(공유 풀은 첫 준비된 노드의 값, 로컬 풀은 준비된 노드마다). 웹에는 노드 상세 화면이 없으므로 노드 목록에 "스토리지 풀" 열을 둔다. 오류 문구 `errors.pool_unavailable`. 문구는 en·ko. 도우미는 `web/src/storage/pools.ts`(`poolLocation`, `isShared`, `poolNodes`, `poolCapacity`, `poolsUsableOn`, `poolUsableSomewhere`, `nfsExportError`, `nfsOptionsError`)와 `PoolStatus.tsx`다. `poolsUsableOn(pools, nodes, nodeId)`는 서버의 `pool_usable_on`과 같은 규칙(노드 목록, `pool_id`가 같은 상태의 `ready`, 디렉터리 풀은 알리지 않았어도 됨)에 노드가 온라인일 것을 더한다. 이미지 업로드 대화상자는 접속한 노드에서, 복제 대화상자는 VM이 있는 노드에서 쓸 수 있는 풀만 보이고(복제가 `pool_unavailable`로 거절되면 풀 입력란의 오류로 보인다), 볼륨 만들기 대화상자는 온라인 노드 가운데 어디서든 쓸 수 있는 풀(`poolUsableSomewhere`)만 보인다. 오프라인 노드의 풀 상태는 "오프라인"이고, 용량은 온라인이고 준비된 노드의 값이다. 풀 만들기의 내보내기·옵션 검사는 서버와 같다(제어 문자, 옵션 거부 목록). `errors.pool_unavailable`은 서버 메시지(`{{message}}`) 그대로다.

## 8. 오류 처리

- 마운트 실패·시간 초과는 노드 상태의 `error`로만 남기고, 백오프(15초부터 두 배, 최대 5분) 뒤 다시 시도한다(로그는 상태가 바뀔 때 한 번).
- NFS 서버가 멈추면 statvfs·쓰기 확인이 멈출 수 있다. 확인은 별도 blocking 작업에 시간 제한(10초)을 두고, 넘으면 `ready: false`, `error: "nfs server not responding"`. 멈춘 blocking 작업은 버린다(다음 tick은 앞 작업이 끝나지 않았으면 그 풀을 건너뛴다).
- 풀 삭제는 지금처럼 볼륨이 남아 있으면 409다. 지운 NFS 풀은 각 노드의 다음 tick에서 언마운트된다.

## 9. 테스트

- **단위:** `PoolBackend::Nfs` 직렬화·검증, `is_shared`, `pool_root`, 마운트 인자 만들기(IPv6 포함), mountinfo 해석, `pool_usable_on`, 스케줄러(로컬 풀은 볼륨 노드로, 공유 풀은 준비된 노드로, 준비 안 된 노드 제외), `pool_node`/`volume_node`, API(NFS 풀 만들기·`shared: false` 거절·`pool_unavailable`).
- **opld:** `PoolMounter`를 가짜 마운트 명령(테스트용 trait)으로 돌려 상태 쓰기·언마운트·macOS 메시지를 본다.
- **실제 NFS(`nfs-it` 기능, Linux만):** CI `linux-kvm` 작업에 `nfs-kernel-server`를 깔고 임시 디렉터리를 `localhost`로 내보낸 뒤(sudo), opld 통합 테스트가 NFS 풀을 마운트하고 볼륨을 만들고 qcow2 스냅샷을 찍고 지운 뒤 언마운트되는지 본다. macOS 개발 환경에서는 건너뛴다. 구현: `crates/opld/tests/nfs.rs`(opld의 `nfs-it` 기능), 내보낸 디렉터리는 환경 변수 `OPL_TEST_NFS_EXPORT`로 받고(서버 쪽 경로로 파일을 확인한다), 풀은 oplctl로 만든다. CI는 `/srv/opl-nfs`를 `127.0.0.1(rw,sync,no_subtree_check,no_root_squash)`로 내보낸다. NFS 서버를 막 시작했으면 잠금 유예(약 90초) 때문에 첫 실행이 느릴 수 있다.
- 테스트 도우미: opl-api `common::mark_pool_ready`·`set_pool_state`(풀의 `pool_id`를 채운다), `Env::pool_ready`, opld `Daemon::wait_pool_ready`. 디렉터리 풀은 알리지 않아도 쓸 수 있게 되어 이 도우미가 꼭 필요하지 않은 곳도 그대로 두었다. 디렉터리 풀을 쓰는 opld 통합 테스트는 그 디렉터리를 만들어 둔다.
- nfs-it 시험은 실패해도 임시 디렉터리를 지우기 전에 opld를 멈추고 `{data_dir}/pools` 아래 마운트를 언마운트한다(풀지 못하면 임시 디렉터리를 남긴다). 내보내기는 버려도 되는 디렉터리여야 한다.
- 기존 시험(워크스페이스, `storage-it`, `qemu-it`, `web-it`, 웹)이 그대로 통과한다.

## 10. 알려진 한계

- NFS 마운트는 Linux 노드만, opld가 root일 때만 된다.
- UEFI 변수는 노드 로컬이다. 공유 풀의 VM이 다른 노드에서 시작되면 새 UEFI 변수로 부팅한다(지금과 같다, 하위 프로젝트 4에서 다룬다).
- NFS의 qcow2 잠금은 QEMU의 OFD 잠금에 기대며 NFSv4에서만 노드 사이에 동작한다. 같은 볼륨을 두 노드에서 동시에 쓰는 일은 배치 규칙이 막는다.
- 노드별 풀 상태는 tick 간격(기본 15초)만큼 늦다.
- 마운트된 NFS 풀이 있는 노드에서 `{data_dir}`를 통째로 지우면(`rm -rf /var/lib/opl`) 마운트를 따라 들어가 NFS 내보내기의 파일까지 지운다. 먼저 opld를 멈추고 `{data_dir}/pools` 아래를 언마운트한다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 435개, 실패 0

Run: `cargo test -p opl-api --features storage-it`
Expected: PASS, 실패 0

Run: `cargo test -p opld --features qemu-it --test smoke --test e2e`
Expected: PASS

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS 1개

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: NFS 풀과 노드별 풀 상태 안내, 공유 스토리지 설계에 구현하며 정한 것"
```

---

## 완료 기준 (계획 15)

- NFS 풀을 API·oplctl·웹으로 만들 수 있고, 잘못된 서버·내보내기·옵션은 400이며, `shared: false` NFS 풀은 거절된다.
- 각 노드가 쓸 수 있는 풀의 상태(`pool_id`, 준비, 오류, 용량)를 알리고, 디렉터리 풀은 쓰기 확인, NFS 풀은 마운트 뒤 확인을 거친다.
- 스케줄러가 공유가 아닌 풀의 볼륨이 있는 노드만 고르고, 준비 안 된 노드를 빼며, 볼륨 Task·가져오기·복제가 `pool_unavailable`로 거절한다.
- NFS가 마운트되지 않았을 때 드라이버가 로컬 디렉터리에 쓰거나 "없음"을 성공으로 보지 않고, VM은 꺼지지 않고 다시 시도된다.
- Linux에서 실제 NFS 풀이 마운트되고 볼륨·qcow2 스냅샷이 서버 쪽에 생기며, 풀을 지우면 언마운트된다(`nfs-it`, CI `linux-kvm`).
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **NFS는 Linux 노드에서 opld가 root일 때만** 마운트된다. macOS 노드는 이유를 상태에 남긴다.
- **UEFI 변수는 노드 로컬이다.** 공유 풀의 VM이 다른 노드에서 시작되면 새 UEFI 변수로 부팅한다(하위 프로젝트 4).
- **상태 지연:** 노드별 풀 상태는 `refresh_secs`(15초)만큼 늦을 수 있다(풀을 만들고 지울 때는 곧바로 새로 고친다). 디렉터리 풀은 보고 전에도 쓸 수 있지만 화면에는 "아직 알린 상태 없음"으로 보인다.
- **마운트 위치 주의:** 마운트된 풀이 있는 노드에서 `rm -rf {data_dir}`을 하면 NFS 내보내기의 내용까지 지워진다.
- 그 밖의 제한은 설계 문서 §10에 있다.

## 실행 후 기록 (2026-10-29)

8개 Task는 계획대로 적용했고, Task마다 검증된 단계와 트리가 같았다(생성 파일 `schema.d.ts` 포함). 최종 리뷰(심각 없음, 중요 셋)로 아래가 바뀌었다(`811f226`). 저장소의 코드가 기준이다. 위 Task의 코드 블록은 고치기 전 것이다.

- **지워진 노드의 볼륨(중요):** 공유가 아닌 풀의 볼륨을 만든 노드가 클러스터에서 지워졌으면 지우기·크기 Task를 이 노드에서 돌린다(파일에는 닿지 못하므로 리소스만 지운다). 전에는 그 볼륨과 풀을 지울 수 없었다.
- **멈춘 NFS의 파일 호출(중요):** 드라이버의 파일 호출(있는지 확인, 하드 링크, 지우기, 용량, 임시 파일 정리)을 `spawn_blocking`과 30초 제한(`FS_TIMEOUT`)으로 돌리고, 넘으면 일시 오류 `DriverError::PoolNotResponding`. 전에는 하드 마운트가 멈추면 opld의 비동기 스레드가 묶였다.
- **기존 공유 디렉터리 풀(중요):** `pool create --path … --shared`와 웹의 "공유 스토리지" 확인란을 더했다. 공유가 아닌 풀은 이제 VM을 볼륨이 있는 노드에 묶으므로 README에 업그레이드 안내를 적었다.
- 마운트 옵션 맨 앞이 `nosuid,nodev,noexec,retry=0`(사용자가 `retry`를 덮을 수 있다), 노드 상태의 용량은 1 GiB 넘게 바뀔 때만 다시 쓴다, 배치된 적 없는 VM의 공유 풀 Task는 쓸 수 있는 노드로, macOS 노드의 NFS 디스크는 VM을 이유와 함께 끈다(`ResolveError::NfsUnsupported`), README에 클러스터를 떠나도 마운트가 남는다는 주의.

알고 받아들인 동작(위 제한 목록에 더해):
- 풀의 `shared`를 나중에 바꾸는 API는 없다(볼륨을 옮긴 뒤 다시 만든다).
- CI의 TCG에서 실행 중 VM의 메모리 스냅샷(`snapshot-save`)이 느려 시간 제한에 걸린 적이 한 번 있었다(같은 트리의 다른 실행은 통과).

테스트: macOS `cargo test --workspace` 437개, `opl-driver` `storage-it` 66개, `opl-api` `storage-it` 178개, `qemu-it` `smoke`·`e2e` 5개, 브라우저 E2E, 웹 Vitest 270개, `fmt`·`clippy -D warnings`(모든 기능). 실제 NFS(`nfs-it`)는 Linux CI `linux-kvm`에서.
CI(확인용 브랜치): `check` 2개, `deny`, `web`, `linux-kvm`(실제 NFS 포함)이 모두 통과했다.

## 다음 계획으로 넘길 사항

- 계획 16: 블록 백엔드 바탕(디스크 소스 추상화 `host_device`, 백엔드 스냅샷, 메모리 스냅샷의 vmstate 위치)과 LVM-thin.
- 계획 17: ZFS(zvol). 계획 18: iSCSI, Ceph RBD.
- 하위 프로젝트 4: 라이브 마이그레이션, UEFI 변수 공유, 풀 `shared` 바꾸기.
