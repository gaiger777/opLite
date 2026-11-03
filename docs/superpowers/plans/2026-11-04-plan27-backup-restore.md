# opLite 계획 27: 백업과 복원 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VM을 다른 저장소(dir·NFS 백업 풀)에 통째로 떠 두고, VM을 지운 뒤에도 언제든 새 VM으로 되살린다. 하위 프로젝트 7(백업)의 첫 계획이다.

**Architecture:** dir·NFS 풀에 `backups` 표시를 켜면 백업 풀이 된다. `Backup` 리소스마다 `{풀 루트}/backups/{백업 id}/`에 디스크별 압축 qcow2, UEFI 변수, JSON 매니페스트를 둔다. 백업 Task는 VM의 노드에서 VM 큐로 돌며, 실행 중이면 지금 스냅샷 코드로 메모리 없는 임시 스냅샷을 찍어 그것을 `qemu-img convert`로 내보내고 지운다. 복원 Task는 매니페스트를 클러스터 기록과 대조한 뒤 대상 풀에 새 볼륨을 가져오고 꺼진 새 VM을 만든다(복제와 같은 정리).

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap, qemu-img; 웹은 계획 8~26과 같다. 새 외부 의존성은 opl-types의 `time`(이미 워크스페이스에 있는 크레이트)뿐이다.

**설계 문서:** `docs/superpowers/specs/2026-11-04-backup-design.md` (§2와 §4·§5의 계획 27 부분, "구현(계획 27)" 메모 포함)
**선행 계획:** 계획 1~26 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 풀의 `backups` 표시, `Backup` 리소스·매니페스트 | 백업 작업·일정·보존 규칙 (계획 28) |
| 실행 중·멈춘 VM의 전체 백업(압축 qcow2, crash-consistent), 지우기·강제 지우기 | 증분·dirty bitmap, 중복 제거 저장소, 암호화, 원격 저장소 |
| 새 VM으로 복원(대상 풀 형식에 맞춤), 실패 정리, 매니페스트 대조 | 기존 VM 덮어 복원, 파일 단위 복원, 게스트 파일 시스템 얼리기 |
| oplctl·웹, 실제 시험(실제 QEMU), README | |

## 이전 계획에서 이어받는 사항

- 스냅샷(계획 10~11): 백엔드별 스냅샷(qcow2 내부·LVM-thin·ZFS·RBD), 실행 중 블록 백엔드의 짧은 멈춤, UEFI 변수 복사(`vars_copy`), 블록 태그 청소(`sweep_block_tags`).
- 복제: `volume_source`/`release_source`로 스냅샷 읽기, `copy_format`, 실패 정리(`Made`/`discard`), 새 MAC.
- 풀(계획 15~): 노드마다 마운트·상태·용량, 공유 풀, NFS 마운트 위치 `{data_dir}/pools/{풀 id}`.
- Task 실행기의 VM 큐, 노드별 실행.

## 실행 환경

계획 26과 같다. 이 Mac에 QEMU와 qemu-img(brew)가 있으면 dir 풀 실제 시험(`opl-driver`·`opl-api`의 `storage-it`, `opld`의 `qemu-it` `tests/backup.rs`)을 여기서 돌린다. CI `linux-kvm`은 이 기능들을 이미 돌린다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `Cargo.lock`(`cargo check`), `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **스냅샷 + 이미지 복사:** 새 QMP 기능 없이 지금의 스냅샷·복제 길을 쓴다. 모든 백엔드에서 같은 코드다.
- **표준 파일:** 백업은 압축 qcow2와 JSON이라 opLite 없이 `qemu-img`·QEMU로 열 수 있다.
- **쓰기 보장:** `-t writeback`(닫을 때 flush), 매니페스트·변수 fsync, 디렉터리 fsync를 마친 뒤에만 `ready`. 매니페스트가 있으면 완전한 백업이다.
- **파일이 있는 노드:** 공유가 아닌 풀의 백업은 쓴 노드를 기록하고 지우기·복원을 그 노드로 보낸다. 백업이 남은 풀의 `shared`는 바꿀 수 없다.
- **복원은 클러스터 기록을 믿는다:** 매니페스트는 기록과 대조만 하고, 새 VM은 기록의 spec으로 만든다. 디스크 파일은 경로 없는 일반 파일이어야 하고 backing file이 없어야 한다.
- **지울 수 없는 백업을 남기지 않는다:** 풀이 없어졌으면 리소스만, 쓸 노드가 없으면 `force`로 리소스만 지운다.

## 파일 구조

```
crates/opl-types/src/{backup.rs, storage.rs, task.rs, resource.rs}          # Backup, 풀 backups, TaskOp
crates/opl-api/src/{backups.rs, resources.rs, snapshots.rs, volumes.rs, auth.rs, watch.rs, lib.rs}, tests/backups.rs
crates/opl-api/src/tasks/{backup.rs, snapshot.rs, clone.rs, migrate.rs}, src/tasks.rs, tests/backup_tasks.rs
crates/opl-driver/src/storage.rs, storage/storages.rs                       # export_volume
crates/opld/src/member.rs, tests/backup.rs                                  # mount_root, 실제 시험
crates/oplctl/src/{cli.rs, client.rs}, web/src/{backups, storage, vms, tasks, layout, api, auth, i18n}/...
README.md, docs/superpowers/specs/{2026-11-04-backup-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 백업 풀 표시와 Backup 타입

`StoragePoolSpec.backups`(dir·NFS 풀만, false면 직렬화에서 뺌)와 풀 PATCH의 `backups`(`{ shared?, backups? }`). opl-types `backup.rs`: `Backup` 리소스(`BackupSpec { vm, vm_name, pool, vm_spec, job }`, `BackupStatus { phase, created_at, node, disks, uefi_vars, error }`), 매니페스트(`BackupManifest`, 버전 1, `manifest.json`), 기본 이름 `{VM 이름}-{yyyymmdd-HHMMSS}`, 디렉터리 `{풀 루트}/backups/{백업 id}`. `Kind::Backup`, `TaskOp::{VmBackup, BackupDelete, BackupRestore}`(선 위 이름 `vm-backup`·`backup-delete`·`backup-restore` — 실행은 Task 3·4에서, 그 전에는 명확한 오류로 실패).

**Files:**
- Create: `crates/opl-types/src/backup.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/tests/clones.rs`
- Modify: `crates/opl-api/tests/import_check.rs`
- Modify: `crates/opl-api/tests/pools.rs`
- Modify: `crates/opl-api/tests/schedules.rs`
- Modify: `crates/opl-api/tests/snapshot_disks.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/tests/tasks.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/lvm.rs`
- Modify: `crates/opl-driver/src/storage/rbd.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/src/storage/zfs.rs`
- Modify: `crates/opl-driver/tests/dir_storage.rs`
- Modify: `crates/opl-driver/tests/lvm_shared_storage.rs`
- Modify: `crates/opl-driver/tests/lvm_storage.rs`
- Modify: `crates/opl-driver/tests/rbd_storage.rs`
- Modify: `crates/opl-driver/tests/zfs_storage.rs`
- Modify: `crates/opl-reconciler/tests/ha.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-reconciler/tests/resolve.rs`
- Modify: `crates/opl-types/Cargo.toml`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/opl-types/src/storage.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opld/src/pools.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/resources.rs`에서

```rust

/// 풀에서 바꿀 수 있는 값. 지금은 디렉터리 풀의 `shared`만이다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolPatch {
    pub shared: bool,
}

/// 디렉터리 풀의 `shared`를 바꾼다(다른 백엔드는 400). 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의
/// 볼륨을 쓰는 VM이 그 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면
/// 409 `pool_in_use`다. 같은 값이면 쓰지 않고 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
```

을 다음으로 바꾼다:

```rust

/// 풀에서 바꿀 수 있는 값: 디렉터리 풀의 `shared`, dir·NFS 풀의 `backups`. 적지 않은 값은 그대로 둔다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub shared: Option<bool>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub backups: Option<bool>,
}

/// 디렉터리 풀의 `shared`(다른 백엔드는 400)와 백업 풀 표시 `backups`(dir·NFS 풀만 켤 수 있다)를
/// 바꾼다. 둘 다 없으면 400이다. 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의 볼륨을 쓰는 VM이 그
/// 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
/// 같은 값이면 쓰지 않고 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
```

`crates/opl-api/src/resources.rs`에서

```rust
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    let expected = if_match(&headers)?;
```

을 다음으로 바꾼다:

```rust
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    if req.shared.is_none() && req.backups.is_none() {
        return Err(ApiError::bad_request("give shared or backups"));
    }
    let expected = if_match(&headers)?;
```

`crates/opl-api/src/resources.rs`에서

```rust
        };
        if let Some(why) = fixed {
            return Err(ApiError::bad_request(format!(
```

을 다음으로 바꾼다:

```rust
        };
        if let Some(why) = fixed.filter(|_| req.shared.is_some()) {
            return Err(ApiError::bad_request(format!(
```

`crates/opl-api/src/resources.rs`에서

```rust
        }
        if pool.spec.shared == req.shared {
            return Ok(Json(pool));
        }
        if !req.shared {
            check_pool_local(&state, &pool)?;
        }
        let mut spec = pool.spec.clone();
        spec.shared = req.shared;
        match state
```

을 다음으로 바꾼다:

```rust
        }
        let mut spec = pool.spec.clone();
        spec.shared = req.shared.unwrap_or(spec.shared);
        spec.backups = req.backups.unwrap_or(spec.backups);
        spec.validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if spec == pool.spec {
            return Ok(Json(pool));
        }
        let to_local = pool.spec.shared && !spec.shared;
        if to_local {
            check_pool_local(&state, &pool)?;
        }
        match state
```

`crates/opl-api/src/resources.rs`에서

```rust
        // 바꿨으면 그 값을 둔다).
        if !req.shared
            && let Err(e) = check_pool_local(&state, &written)
        {
            let mut spec = written.spec.clone();
```

을 다음으로 바꾼다:

```rust
        // 바꿨으면 그 값을 둔다).
        if to_local && let Err(e) = check_pool_local(&state, &written) {
            let mut spec = written.spec.clone();
```

`crates/opl-api/src/scheduler.rs`에서

```rust
                shared,
            },
```

을 다음으로 바꾼다:

```rust
                shared,
                backups: false,
            },
```

`crates/opl-api/src/scheduler.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-api/src/snapshots.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
        // 백업·복원 실행은 아직 없다. API도 아직 이 Task를 만들지 않는다.
        TaskOp::VmBackup { .. } | TaskOp::BackupDelete | TaskOp::BackupRestore { .. } => {
            Err("backup tasks are not supported by this node yet".into())
        }
    }
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-api/tests/clones.rs`에서

```rust
            nodes,
            shared: false,
        },
    );
```

을 다음으로 바꾼다:

```rust
            nodes,
            shared: false,
            backups: false,
        },
    );
```

`crates/opl-api/tests/clones.rs`에서

```rust
            nodes: vec![],
            shared: false,
        },
    );
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
```

`crates/opl-api/tests/import_check.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-api/tests/pools.rs`에서

```rust
    assert_eq!(body["spec"]["shared"], false, "{body}");
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(body["spec"]["shared"], false, "{body}");
}

/// 백업 풀 표시(`backups`)는 dir·NFS 풀만 켤 수 있다. 만들 때와 PATCH 모두 받는다.
#[tokio::test]
async fn only_dir_and_nfs_pools_can_store_backups() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    let admin = env.admin().await;
    let with_backups = |name: &str, backend: Value| json!({ "name": name, "spec": { "backend": backend, "backups": true } });
    let not_supported = |body: &Value| {
        assert_eq!(body["code"], "invalid_request", "{body}");
        assert_eq!(
            body["message"], "only dir and nfs pools can store backups",
            "{body}"
        );
    };
    for (name, backend) in [
        ("local", json!({ "kind": "dir", "path": env.dir.path() })),
        (
            "nas",
            json!({ "kind": "nfs", "server": "nas.lan", "export": "/srv/backups" }),
        ),
    ] {
        let (status, body) = admin
            .post("/storage-pools", with_backups(name, backend))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["spec"]["backups"], true, "{body}");
    }
    let blocks = [
        (
            "vg",
            json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }),
        ),
        ("z", json!({ "kind": "zfs", "dataset": "tank/opl" })),
        ("r", json!({ "kind": "rbd", "pool": "rbd" })),
        ("sh", json!({ "kind": "lvm-shared", "vg": "vg1" })),
    ];
    for (name, backend) in &blocks {
        let (status, body) = admin
            .post("/storage-pools", with_backups(name, backend.clone()))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        not_supported(&body);
    }

    // PATCH: 끄고 켠다. 꺼진 표시는 spec에서 빠진다. 같은 값이면 쓰지 않는다.
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "backups": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("backups").is_none(), "{body}");
    assert_eq!(body["spec"]["shared"], false, "{body}");
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "backups": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["backups"], true, "{body}");
    let revision = body["revision"].as_u64().unwrap();
    let (status, body) = admin
        .patch("/storage-pools/local", json!({ "backups": true }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], revision, "{body}");
    // `shared`와 함께 바꿀 수 있다.
    let (status, body) = admin
        .patch(
            "/storage-pools/local",
            json!({ "shared": true, "backups": false }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");
    assert!(body["spec"].get("backups").is_none(), "{body}");
    // NFS 풀은 `shared`를 바꿀 수 없어도 `backups`는 바꾼다.
    let (status, body) = admin
        .patch("/storage-pools/nas", json!({ "backups": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("backups").is_none(), "{body}");
    assert_eq!(body["spec"]["shared"], true, "{body}");

    // 블록 풀은 켤 수 없다(끄는 것은 같은 값이라 그대로).
    let (status, body) = admin
        .post("/storage-pools", pool("z", blocks[1].1.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = admin
        .patch("/storage-pools/z", json!({ "backups": true }), None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    not_supported(&body);
    let (_, got) = admin.get("/storage-pools/z").await;
    assert!(got["spec"].get("backups").is_none(), "{got}");
    let (status, body) = admin
        .patch("/storage-pools/z", json!({ "backups": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
```

`crates/opl-api/tests/schedules.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
                    shared: false,
                },
```

을 다음으로 바꾼다:

```rust
                    shared: false,
                    backups: false,
                },
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
            shared: false,
        };
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        };
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
            shared: false,
        };
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        };
```

`crates/opl-api/tests/tasks.rs`에서

```rust
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
```

`crates/opl-api/tests/tasks.rs`에서

```rust
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
```

을 다음으로 바꾼다:

```rust
                path: env.dir.path().join("local"),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("local").await;
```

`crates/opl-api/tests/tasks.rs`에서

```rust
                nodes: vec![],
            },
```

을 다음으로 바꾼다:

```rust
                nodes: vec![],
                backups: false,
            },
```

`crates/opl-api/tests/tasks.rs`에서

```rust
            nodes: vec![],
        },
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            backups: false,
        },
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            shared: true,
        },
```

을 다음으로 바꾼다:

```rust
            shared: true,
            backups: false,
        },
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

    /// 라이브 마이그레이션 동안 원본·대상 두 노드가 볼륨을 함께 쓸 수 있게 한다: 이 노드에서 켜져
    /// 있으면 다른 노드도 켤 수 있게 바꾸고, 꺼져 있으면 그렇게 켠다. 기본은 할 일이 없다(공유 LVM
    /// 풀만 LV 잠금을 공유로 바꾼다).
    async fn share_volume(
        &self,
        _pool: &StoragePool,
        _id: Uuid,
        _format: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// [`share_volume`](Self::share_volume)으로 함께 쓰게 한 볼륨을 다시 이 노드만 쓰게 한다. 이
    /// 노드에서 꺼져 있으면 할 일이 없다. 기본은 할 일이 없다.
    async fn exclusive_volume(
        &self,
        _pool: &StoragePool,
        _id: Uuid,
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
                backups: false,
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
                backups: false,
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
                backups: false,
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
                backups: false,
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
                backups: false,
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
                backups: false,
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
                        backups: false,
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
                backups: false,
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
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-driver/src/storage/lvm.rs`에서

```rust
                shared: true,
            },
```

을 다음으로 바꾼다:

```rust
                shared: true,
                backups: false,
            },
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
                nodes: vec![],
                shared: true,
            },
        )
```

을 다음으로 바꾼다:

```rust
                nodes: vec![],
                shared: true,
                backups: false,
            },
        )
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
                nodes: vec![],
                shared: true,
            },
        );
```

을 다음으로 바꾼다:

```rust
                nodes: vec![],
                shared: true,
                backups: false,
            },
        );
```

`crates/opl-driver/src/storage/rbd.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-driver/src/storage/zfs.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
            nodes: vec![],
            shared: false,
        },
    )
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            shared: false,
            backups: false,
        },
    )
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
            nodes: vec![],
            shared: false,
        },
    );
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
```

`crates/opl-driver/tests/lvm_shared_storage.rs`에서

```rust
            shared: true,
        },
```

을 다음으로 바꾼다:

```rust
            shared: true,
            backups: false,
        },
```

`crates/opl-driver/tests/lvm_shared_storage.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-driver/tests/lvm_storage.rs`에서

```rust
            backend: PoolBackend::LvmThin { vg, thinpool },
            nodes: vec![],
            shared: false,
        },
    )
}
```

을 다음으로 바꾼다:

```rust
            backend: PoolBackend::LvmThin { vg, thinpool },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    )
}
```

`crates/opl-driver/tests/lvm_storage.rs`에서

```rust
            },
            nodes: vec![],
            shared: false,
        },
    )
}
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    )
}
```

`crates/opl-driver/tests/rbd_storage.rs`에서

```rust
            nodes: vec![],
            shared: true,
        },
    )
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            shared: true,
            backups: false,
        },
    )
```

`crates/opl-driver/tests/rbd_storage.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-driver/tests/rbd_storage.rs`에서

```rust
            nodes: vec![],
            shared: true,
        },
    );
```

을 다음으로 바꾼다:

```rust
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
```

`crates/opl-driver/tests/zfs_storage.rs`에서

```rust
            backend: PoolBackend::Zfs { dataset: dataset() },
            nodes: vec![],
            shared: false,
        },
    )
}
```

을 다음으로 바꾼다:

```rust
            backend: PoolBackend::Zfs { dataset: dataset() },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    )
}
```

`crates/opl-driver/tests/zfs_storage.rs`에서

```rust
            },
            nodes: vec![],
            shared: false,
        },
    )
}
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    )
}
```

`crates/opl-reconciler/tests/ha.rs`에서

```rust
            shared: false,
        },
```

을 다음으로 바꾼다:

```rust
            shared: false,
            backups: false,
        },
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
            shared: true,
        },
```

을 다음으로 바꾼다:

```rust
            shared: true,
            backups: false,
        },
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let volume: Volume = Resource::new(
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let root: Volume = Resource::new(
```

을 다음으로 바꾼다:

```rust
                path: "/pools/local".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let root: Volume = Resource::new(
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            nodes,
            shared: false,
        },
    )
```

을 다음으로 바꾼다:

```rust
            nodes,
            shared: false,
            backups: false,
        },
    )
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            },
            nodes: vec![],
            shared: true,
        },
    );
    cluster.create(&nas).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    cluster.create(&nas).await.unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            },
            nodes: vec![],
            shared: false,
        },
    );
    cluster.create(&vg).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    cluster.create(&vg).await.unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            },
            nodes: vec![],
            shared: true,
        },
    );
    cluster.create(&r).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    cluster.create(&r).await.unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            },
            nodes: vec![],
            shared: true,
        },
    );
    cluster.create(&s).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: true,
            backups: false,
        },
    );
    cluster.create(&s).await.unwrap();
```

`crates/opl-reconciler/tests/resolve.rs`에서

```rust
            },
            nodes: vec![],
            shared: false,
        },
    );
    cluster.create(&z).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    cluster.create(&z).await.unwrap();
```

`crates/opl-types/Cargo.toml`에서

```toml
thiserror.workspace = true
utoipa = { workspace = true, optional = true }
```

을 다음으로 바꾼다:

```toml
thiserror.workspace = true
time.workspace = true
utoipa = { workspace = true, optional = true }
```

`crates/opl-types/src/backup.rs`를 만든다:

```rust
//! VM 백업. 백업 풀(`backups`를 켠 dir·NFS 풀)의 `{루트}/backups/{백업 id}/`에 디스크마다 압축
//! qcow2(`disk{i}.qcow2`), UEFI 변수(`vars.fd`), 매니페스트(`manifest.json`)를 둔다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::VmSpec;

/// 리소스 이름은 사용자가 붙인 이름(클러스터 전체에서 고유)이다. 기본은 [`default_backup_name`].
pub type Backup = Resource<BackupSpec, BackupStatus>;

/// 매니페스트 형식 버전. 다른 버전의 백업은 복원하지 않는다.
pub const MANIFEST_VERSION: u32 = 1;

/// 백업 디렉터리 안의 매니페스트 파일 이름
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupSpec {
    /// VM id. VM을 지워도 백업은 남는다.
    pub vm: Uuid,
    /// 만든 때의 VM 이름(표시용)
    pub vm_name: String,
    /// 백업 풀 이름
    pub pool: String,
    /// 만든 때의 VM 스펙
    pub vm_spec: VmSpec,
    /// 이 백업을 만든 백업 작업 id. 손으로 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<Uuid>,
}

impl ResourceSpec for BackupSpec {
    const KIND: Kind = Kind::Backup;
    type Status = BackupStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BackupStatus {
    pub phase: BackupPhase,
    /// 만든 시각(유닉스 초)
    pub created_at: Option<u64>,
    pub disks: Vec<BackupDisk>,
    /// UEFI 변수 파일(`vars.fd`)을 담았는지
    pub uefi_vars: bool,
    /// 실패한 이유
    pub error: Option<String>,
    /// 파일을 쓴 노드. 공유가 아닌 풀의 백업은 이 노드에만 있으므로 지우기·복원도 이 노드에서 한다.
    /// 공유 풀이면 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BackupPhase {
    /// 만드는 중
    #[default]
    Pending,
    Ready,
    Failed,
}

/// 백업에 담은 디스크 하나. VM 스펙의 디스크 순서를 따른다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupDisk {
    /// 디스크 이름(`disk{i}`)
    pub name: String,
    /// 백업 디렉터리 안의 파일 이름(`disk{i}.qcow2`)
    pub file: String,
    /// 디스크 크기
    pub virtual_bytes: u64,
    /// 백업 파일 크기
    pub stored_bytes: u64,
}

/// 백업 디렉터리의 `manifest.json`. opLite 없이도 백업을 알아볼 수 있게 VM 스펙과 디스크 목록을 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// 형식 버전([`MANIFEST_VERSION`])
    pub version: u32,
    /// 백업 id
    pub backup: Uuid,
    pub vm: Uuid,
    pub vm_name: String,
    pub vm_spec: VmSpec,
    pub disks: Vec<BackupDisk>,
    pub uefi_vars: bool,
    /// 만든 시각(유닉스 초)
    pub created_at: u64,
}

/// 기본 백업 이름 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC). 리소스 이름 길이(63자)를 넘지 않게 VM 이름을
/// 47자까지 줄인다(끝의 `-`는 뺀다).
pub fn default_backup_name(vm_name: &str, at: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let prefix = vm_name[..vm_name.len().min(47)].trim_end_matches('-');
    format!(
        "{prefix}-{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// 풀 루트 아래 백업 하나의 디렉터리 `{root}/backups/{id}`
pub fn backup_dir(pool_root: &Path, backup: Uuid) -> PathBuf {
    pool_root.join("backups").join(backup.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::validate_name;

    fn vm_spec() -> VmSpec {
        serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "virt",
            "disks": [ { "volume": "web-root", "bus": "virtio-blk" } ]
        }))
        .unwrap()
    }

    #[test]
    fn spec_round_trips_and_leaves_out_job() {
        let vm = Uuid::now_v7();
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": vm,
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.vm, vm);
        assert_eq!(spec.job, None);
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("job").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), spec);

        let job = Uuid::now_v7();
        let with = BackupSpec {
            job: Some(job),
            ..spec
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["job"], serde_json::json!(job));
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), with);
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let res = serde_json::from_value::<BackupSpec>(serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "backups",
            "vm_spec": vm_spec(),
            "poll": "x"
        }));
        assert!(res.is_err());
    }

    #[test]
    fn status_defaults_to_pending_and_serializes_phase() {
        let st: BackupStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, BackupStatus::default());
        assert_eq!(st.phase, BackupPhase::Pending);

        let ready = BackupStatus {
            phase: BackupPhase::Ready,
            created_at: Some(1_790_000_000),
            disks: vec![BackupDisk {
                name: "disk0".into(),
                file: "disk0.qcow2".into(),
                virtual_bytes: 10 << 30,
                stored_bytes: 1 << 30,
            }],
            uefi_vars: true,
            error: None,
            node: None,
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "phase": "ready",
                "created_at": 1_790_000_000u64,
                "disks": [{
                    "name": "disk0",
                    "file": "disk0.qcow2",
                    "virtual_bytes": 10u64 << 30,
                    "stored_bytes": 1u64 << 30
                }],
                "uefi_vars": true,
                "error": null
            })
        );
        let failed: BackupStatus =
            serde_json::from_value(serde_json::json!({ "phase": "failed", "error": "disk full" }))
                .unwrap();
        assert_eq!(failed.phase, BackupPhase::Failed);
        assert_eq!(failed.error.as_deref(), Some("disk full"));

        // 공유가 아닌 풀의 백업은 파일을 쓴 노드를 적는다. 없으면 뺀다.
        let node = Uuid::now_v7();
        let local = BackupStatus {
            node: Some(node),
            ..ready
        };
        let json = serde_json::to_value(&local).unwrap();
        assert_eq!(json["node"], serde_json::json!(node));
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), local);
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = BackupManifest {
            version: MANIFEST_VERSION,
            backup: Uuid::now_v7(),
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            vm_spec: vm_spec(),
            disks: vec![BackupDisk {
                name: "disk0".into(),
                file: "disk0.qcow2".into(),
                virtual_bytes: 1 << 30,
                stored_bytes: 1 << 20,
            }],
            uefi_vars: false,
            created_at: 1_790_000_000,
        };
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["version"], 1);
        assert_eq!(json["vm_name"], "web");
        assert_eq!(json["disks"][0]["file"], "disk0.qcow2");
        assert_eq!(json["vm_spec"]["disks"][0]["volume"], "web-root");
        let text = serde_json::to_string_pretty(&manifest).unwrap();
        assert_eq!(
            serde_json::from_str::<BackupManifest>(&text).unwrap(),
            manifest
        );
    }

    #[test]
    fn default_names_are_utc_seconds() {
        // 2026-10-27 05:07:09 UTC
        assert_eq!(
            default_backup_name("web", 1_793_077_629),
            "web-20261027-050709"
        );
        assert_eq!(default_backup_name("db", 0), "db-19700101-000000");
        // 긴 VM 이름은 줄여서 이름 규칙에 맞춘다.
        let long = "a".repeat(63);
        let name = default_backup_name(&long, 0);
        assert_eq!(name.len(), 63);
        assert_eq!(validate_name(&name), Ok(()));
        let dash = format!("{}-b", "a".repeat(46));
        assert_eq!(
            default_backup_name(&dash, 0),
            format!("{}-19700101-000000", "a".repeat(46))
        );
    }

    #[test]
    fn kind_is_backup() {
        assert_eq!(serde_json::to_value(Kind::Backup).unwrap(), "backup");
        assert_eq!(Kind::Backup.as_str(), "backup");
    }

    #[test]
    fn backup_dir_is_under_backups() {
        let id = Uuid::now_v7();
        assert_eq!(
            backup_dir(Path::new("/srv/backups"), id),
            PathBuf::from(format!("/srv/backups/backups/{id}"))
        );
    }
}
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod auth;
pub mod cluster;
```

을 다음으로 바꾼다:

```rust
pub mod auth;
pub mod backup;
pub mod cluster;
```

`crates/opl-types/src/resource.rs`에서

```rust
    SecurityGroup,
}
```

을 다음으로 바꾼다:

```rust
    SecurityGroup,
    Backup,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::SecurityGroup => "security-group",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::SecurityGroup => "security-group",
            Kind::Backup => "backup",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::SecurityGroup,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::SecurityGroup,
            Kind::Backup,
        ] {
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
    /// 백업 파일(`{루트}/backups/`)을 둘 수 있는 백업 풀인지. dir·NFS 풀만 된다. 켜도 VM 디스크는 그대로 둔다.
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub backups: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
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
        // 백업 파일은 파일 시스템이 있는 dir·NFS 풀에만 둔다.
        if self.backups
            && !matches!(
                self.backend,
                PoolBackend::Dir { .. } | PoolBackend::Nfs { .. }
            )
        {
            return Err(PoolError::BackupsNotSupported);
        }
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
/// 한다. 위트니스 노드는 풀을 쓰지 않는다.
pub fn pool_usable_on(pool: &StoragePool, node: &Node) -> bool {
    !node.spec.witness
        && pool_listed_on(pool, node.id)
        && match pool_state_on(pool, node) {
            Some(state) => state.ready,
            None => matches!(pool.spec.backend, PoolBackend::Dir { .. }),
        }
}

/// 노드에서 풀이 준비되지 않은 이유. 노드가 알린 오류이고, 상태를 아직 알리지 않았으면 그렇게 적는다.
pub fn pool_error_on(pool: &StoragePool, node: &Node) -> String {
    if node.spec.witness {
        return "witness nodes do not use storage pools".into();
    }
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
    #[error("only dir and nfs pools can store backups")]
    BackupsNotSupported,
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
            backups: false,
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
            backups: false,
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
                backups: false,
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
                witness: false,
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
                backups: false,
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
        // 위트니스에서는 준비됐다고 알렸거나 상태가 없어도 되는 디렉터리 풀이라도 쓸 수 없다.
        node.spec.witness = true;
        assert!(!pool_usable_on(&pool, &node));
        assert_eq!(
            pool_error_on(&pool, &node),
            "witness nodes do not use storage pools"
        );
        dir.spec.nodes.clear();
        node.status.pools.remove("d");
        assert!(!pool_usable_on(&dir, &node));
    }

    fn lvm(vg: &str, thinpool: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: vg.into(),
                thinpool: thinpool.into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
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
                witness: false,
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
            backups: false,
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
                witness: false,
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
            backups: false,
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
                witness: false,
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
            backups: false,
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
                witness: false,
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
    fn only_dir_and_nfs_pools_store_backups() {
        let spec = |backend: serde_json::Value| -> StoragePoolSpec {
            serde_json::from_value(serde_json::json!({ "backend": backend, "backups": true }))
                .unwrap()
        };
        for ok in [
            serde_json::json!({ "kind": "dir", "path": "/srv/backups" }),
            serde_json::json!({ "kind": "nfs", "server": "nas", "export": "/srv/backups" }),
        ] {
            let spec = spec(ok);
            assert!(spec.backups);
            assert_eq!(spec.validate(), Ok(()));
        }
        for bad in [
            serde_json::json!({ "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" }),
            serde_json::json!({ "kind": "zfs", "dataset": "tank/opl" }),
            serde_json::json!({ "kind": "rbd", "pool": "rbd" }),
            serde_json::json!({ "kind": "lvm-shared", "vg": "vg0" }),
        ] {
            let mut spec = spec(bad);
            assert_eq!(spec.validate(), Err(PoolError::BackupsNotSupported));
            spec.backups = false;
            assert_eq!(spec.validate(), Ok(()));
        }
    }

    #[test]
    fn backups_is_left_out_when_false() {
        let spec: StoragePoolSpec = serde_json::from_value(serde_json::json!({
            "backend": { "kind": "dir", "path": "/p" }
        }))
        .unwrap();
        assert!(!spec.backups);
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("backups").is_none(), "{json}");
        let on = StoragePoolSpec {
            backups: true,
            ..spec
        };
        let json = serde_json::to_value(&on).unwrap();
        assert_eq!(json["backups"], true);
        assert_eq!(serde_json::from_value::<StoragePoolSpec>(json).unwrap(), on);
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

`crates/opl-types/src/task.rs`에서

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제 Task는 (원본) VM이다.
    pub target: Uuid,
```

을 다음으로 바꾼다:

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷·복제·백업 Task는 (원본) VM, 백업 지우기는 백업,
    /// 복원은 새 VM이다.
    pub target: Uuid,
```

`crates/opl-types/src/task.rs`에서

```rust
        from: Uuid,
    },
```

을 다음으로 바꾼다:

```rust
        from: Uuid,
    },
    /// 대상 VM을 백업 `backup`(`pending`으로 먼저 만든 Backup 리소스)에 담는다. VM의 노드에서 돈다.
    VmBackup {
        backup: Uuid,
    },
    /// 대상 백업의 파일과 리소스를 지운다.
    BackupDelete,
    /// 백업 `backup`을 대상 id(미리 정한 새 VM id)의 새 VM `name`으로 되살린다. 디스크는 `pool`에 만든다.
    BackupRestore {
        backup: Uuid,
        name: String,
        pool: String,
    },
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn backup_ops_are_tagged_by_kind() {
        let backup = Uuid::now_v7();
        for (op, json) in [
            (
                TaskOp::VmBackup { backup },
                serde_json::json!({ "kind": "vm-backup", "backup": backup }),
            ),
            (
                TaskOp::BackupDelete,
                serde_json::json!({ "kind": "backup-delete" }),
            ),
            (
                TaskOp::BackupRestore {
                    backup,
                    name: "web-2".into(),
                    pool: "local".into(),
                },
                serde_json::json!({
                    "kind": "backup-restore",
                    "backup": backup,
                    "name": "web-2",
                    "pool": "local"
                }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/opld/src/pools.rs`에서

```rust
                    shared: false,
                },
```

을 다음으로 바꾼다:

```rust
                    shared: false,
                    backups: false,
                },
```

`crates/opld/src/pools.rs`에서

```rust
                shared: false,
            },
```

을 다음으로 바꾼다:

```rust
                shared: false,
                backups: false,
            },
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check --workspace`
Expected: `Cargo.lock`에 opl-types의 `time` 의존이 더해진다(커밋한다)

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 949개, 실패 0

Run: `cargo test -p opl-types`
Expected: PASS

Run: `cargo test -p opl-api --test pools`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 351개

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates web
git commit -m "feat: 백업 풀 표시와 Backup 타입 — StoragePoolSpec.backups(dir·NFS만), 풀 PATCH의 backups, Backup 리소스·매니페스트·기본 이름·디렉터리, Kind::Backup, 백업 Task 종류"
```

---

### Task 2: 백업 API

`POST /vms/{key}/backups { pool, name? }`(백업을 `pending`으로 만들고 `VmBackup` Task; 실행 중 VM은 모든 디스크가 같은 스냅샷 방식이어야, 만든 뒤 풀을 다시 읽어 없거나 백업이 꺼졌으면 되돌림), `GET /backups[?vm=]`(id, 지금 VM 이름, 백업 때 VM 이름), `GET /backups/{key}`, `DELETE /backups/{key}[?force=true]`(파일이 있는 노드로 `BackupDelete` Task; 풀이 없어졌으면 리소스만, 쓸 노드가 없으면 409이고 `force`면 리소스만 지우고 200), `POST /backups/{key}/restore { name, pool, node? }`(ready여야, 공유가 아닌 풀의 백업은 쓴 노드에서만 — 409 `backup_on_other_node`, 노드가 온라인·두 풀을 쓸 수 있어야). 진행 중 Task가 있는 백업 지우기·지우는 중 복원은 409 `backup_busy`. 진행 중 백업은 VM을 붙잡는다(`vm_busy`). 백업이 남은 풀 지우기와 그 풀의 `backups` 끄기·`shared` 바꾸기는 409 `pool_in_use`. operator가 백업을 쓸 수 있다. watch 종류 `backup`.

**Files:**
- Create: `crates/opl-api/src/backups.rs`
- Create: `crates/opl-api/tests/backups.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs`에서

```rust
                Action::Read => !account,
                Action::Write | Action::Operate => matches!(kind, Kind::Vm | Kind::Volume),
            },
```

을 다음으로 바꾼다:

```rust
                Action::Read => !account,
                Action::Write | Action::Operate => {
                    matches!(kind, Kind::Vm | Kind::Volume | Kind::Backup)
                }
            },
```

`crates/opl-api/src/auth.rs`에서

```rust
        assert!(a.check(&op, Action::Write, Kind::Volume));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

을 다음으로 바꾼다:

```rust
        assert!(a.check(&op, Action::Write, Kind::Volume));
        assert!(a.check(&op, Action::Write, Kind::Backup));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

`crates/opl-api/src/backups.rs`를 만든다:

```rust
//! VM 백업 만들기·목록·읽기·지우기·복원(설계 §2). 만들기는 VM 쓰기, 지우기·복원은 백업 쓰기 권한이다.
//! 실제 작업은 Task 실행기가 한다: 백업은 VM의 노드, 지우기·복원은 백업 풀을 쓸 수 있는 노드(공유가
//! 아닌 풀의 백업은 파일을 쓴 노드).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus, default_backup_name};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
    pool_kind_supports_snapshots, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name, lookup};
use crate::snapshots::{check_disks, check_not_busy};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{check_security_groups, is_stopped};
use crate::volumes::{check_pool_on, pool_node};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBackupRequest {
    /// 백업 풀(`backups`를 켠 풀)
    pub pool: String,
    /// 백업 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 새 디스크를 둘 풀
    pub pool: String,
    /// 복원을 실행할 노드(이름이나 id). 없으면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
    pub node: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BackupQuery {
    /// 이 VM(id나 이름)의 백업만. 이름이면 지금 그 이름의 VM 백업과 그 이름으로 남은(지운 VM의) 백업이다.
    pub vm: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DeleteBackupQuery {
    /// 파일을 지울 노드가 없어도 리소스만 지운다. 파일은 백업 풀에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 리소스만 지운 백업 지우기(`force`)의 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupForgotten {
    pub message: String,
}

/// 지금 시각(유닉스 초)
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// `fits`를 만족하는 온라인 노드 가운데 이름이 가장 앞선 노드
fn first_node(state: &AppState, fits: impl Fn(&Node) -> bool) -> Result<Option<Uuid>, ApiError> {
    let (_, mut nodes) = state.cluster.list::<NodeSpec>()?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(nodes
        .iter()
        .find(|n| n.status.online && fits(n))
        .map(|n| n.id))
}

/// 백업 `id`를 다루는 끝나지 않은 Task가 하는 일(만들기, 복원, 지우기)
fn backup_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmBackup { backup } if backup == id => Some("being created"),
            TaskOp::BackupRestore { backup, .. } if backup == id => Some("being restored"),
            TaskOp::BackupDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        })
        .collect())
}

/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    match state.cluster.get::<NodeSpec>(id)? {
        Some(n) if n.status.online => Ok(()),
        n => Err(ApiError::conflict(
            "node_offline",
            format!(
                "node {:?} is offline",
                n.map_or_else(|| id.to_string(), |n| n.name)
            ),
        )),
    }
}

/// 노드 `id`의 이름(없으면 id)
fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
        .cluster
        .get::<NodeSpec>(id)?
        .map_or_else(|| id.to_string(), |n| n.name))
}

fn backup_busy(backup: &Backup, doing: &str) -> ApiError {
    ApiError::conflict(
        "backup_busy",
        format!(
            "backup {:?} is {doing}; try again when the task has finished",
            backup.name
        ),
    )
}

/// 풀에 백업이 남아 있으면 409 `pool_in_use`. 풀 지우기와 백업 표시 끄기가 쓴다.
pub(crate) fn check_no_backups(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    if backups.iter().any(|b| b.spec.pool == pool.name) {
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} holds backups", pool.name),
        ));
    }
    Ok(())
}

/// VM 디스크의 볼륨과 풀. 없으면 400, 만드는 중이면 409 `volume_not_ready`.
fn vm_disks(state: &AppState, vm: &Vm) -> Result<Vec<(Volume, StoragePool)>, ApiError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
        let pool = find_pool(state, &volume.spec.pool)?;
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 백업 Task를 실행할 노드. 실행 중인 VM은 그 노드, 꺼진 VM은 `spec.node`이고, 노드가 없으면 백업
/// 풀과 모든 디스크를 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(로컬 풀의 디스크는 파일이
/// 있는 노드에서만 읽힌다). 그런 노드가 없으면 409 `no_node`.
fn backup_node(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    disks: &[(Volume, StoragePool)],
) -> Result<Uuid, ApiError> {
    let placed = if is_stopped(vm) {
        vm.spec.node
    } else {
        vm.spec.node.or(vm.status.node)
    };
    if let Some(node) = placed {
        return Ok(node);
    }
    let reads = |node: &Node| {
        pool_usable_on(pool, node)
            && disks.iter().all(|(volume, p)| {
                pool_usable_on(p, node) && (is_shared(p) || volume.status.node == Some(node.id))
            })
    };
    first_node(state, reads)?.ok_or_else(|| {
        ApiError::conflict(
            "no_node",
            format!(
                "no online node can read the disks of VM {:?} and use storage pool {:?}",
                vm.name, pool.name
            ),
        )
    })
}

/// 모든 백업. `vm`(id나 이름)을 주면 그 VM의 백업만이다.
#[utoipa::path(get, path = "/backups", tag = "backups",
    params(BackupQuery),
    responses(
        (status = 200, body = List<Resource<BackupSpec, BackupStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_backups(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<BackupQuery>,
) -> Result<Json<List<Backup>>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    let (revision, mut items) = state.cluster.list::<BackupSpec>()?;
    if let Some(key) = q.vm {
        let id = match Uuid::try_parse(&key) {
            Ok(id) => Some(id),
            Err(_) => state.cluster.get_by_name::<VmSpec>(&key)?.map(|vm| vm.id),
        };
        // 이름이면 지금 그 이름인 VM의 백업과, 그 이름으로 남은 백업(지운 VM)을 함께 준다.
        let by_name = Uuid::try_parse(&key).is_err();
        items.retain(|b| Some(b.spec.vm) == id || (by_name && b.spec.vm_name == key));
    }
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses(
        (status = 200, body = Resource<BackupSpec, BackupStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Backup>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    Ok(Json(lookup::<BackupSpec>(&state.cluster, &key)?))
}

/// `pending` 백업을 만들고 그것을 채울 Task를 돌려준다(202). 풀은 백업 풀이어야 한다(400). 시작·정지
/// 중이면 409 `vm_busy`, 실행 중인데 스냅샷을 담지 못하는 디스크가 있으면 409 `vm_running`(스냅샷
/// 방식이 섞였으면 스냅샷처럼 409 `snapshot_unsupported`), Task를 돌릴 노드에서 백업 풀을 쓸 수 없으면
/// 409 `pool_not_on_node`·`pool_unavailable`, 돌릴 노드가 없으면 409 `no_node`, 이름이 겹치면 409
/// `name_taken`. 그사이 풀이 지워졌거나 백업 표시가 꺼졌으면 만든 백업을 지우고 409 `pool_unavailable`.
#[utoipa::path(post, path = "/vms/{key}/backups", tag = "backups",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateBackupRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateBackupRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let pool = find_pool(&state, &req.pool)?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    check_not_busy(&state, &vm)?;
    let disks = vm_disks(&state, &vm)?;
    // 실행 중인 VM은 임시 스냅샷에서 읽는다.
    if !is_stopped(&vm)
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !pool_kind_supports_snapshots(&p.spec.backend, v.spec.format))
    {
        return Err(ApiError::conflict(
            "vm_running",
            format!(
                "VM {:?} is running and its disk {:?} cannot be snapshotted; stop it to back it up",
                vm.name, volume.name
            ),
        ));
    }
    if !is_stopped(&vm) {
        check_disks(&state, &vm)?;
    }
    let node = backup_node(&state, &vm, &pool, &disks)?;
    check_pool_on(&state, &pool, node, || {
        format!(
            "storage pool {:?} is not available on the node holding VM {:?}",
            pool.name, vm.name
        )
    })?;
    let name = req
        .name
        .unwrap_or_else(|| default_backup_name(&vm.name, now()));
    let backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: vm.id,
            vm_name: vm.name.clone(),
            pool: pool.name.clone(),
            vm_spec: vm.spec.clone(),
            job: None,
        },
    );
    // 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    // 풀 지우기·백업 표시 끄기는 백업이 없을 때만 된다. 그 확인과 엇갈렸으면 만든 백업을 거둔다.
    if !find_pool(&state, &pool.name).is_ok_and(|p| p.spec.backups) {
        if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
            tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup of a removed pool");
        }
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} was deleted or stopped storing backups",
                pool.name
            ),
        ));
    }
    let op = TaskOp::VmBackup { backup: backup.id };
    match create_task(&state.cluster, op, vm.id, node).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
        Err(e) => {
            // 채울 Task가 없는 pending 백업은 이름만 차지한다.
            if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
                tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup without a task");
            }
            Err(e)
        }
    }
}

/// 백업 파일을 지울 노드: 파일을 쓴 노드(`status.node`, 공유가 아닌 풀)나 백업 풀을 쓸 수 있는 노드.
/// 그 노드가 오프라인이면 409 `node_offline`, 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn files_node(state: &AppState, backup: &Backup, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let Some(node) = backup.status.node else {
        return pool_node(state, pool);
    };
    check_online(state, node)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding backup {:?}",
            pool.name,
            node_name(state, node).unwrap_or_else(|_| node.to_string()),
            backup.name
        )
    })?;
    Ok(node)
}

/// 백업 파일과 리소스를 지울 Task를 돌려준다(202). 실패한 백업도 지운다. 백업을 만들거나 되살리거나
/// 지우는 Task가 끝나지 않았으면 409 `backup_busy`. Task는 파일이 있는 노드(공유가 아닌 풀은 파일을
/// 쓴 노드, 공유 풀은 백업 풀을 쓸 수 있는 노드)에서 돈다. 풀이 없어졌으면 이 노드의 Task가 리소스만
/// 지운다. 파일을 지울 노드가 없으면 409(`node_offline`, `pool_unavailable` 등)이고, `force`면 리소스만
/// 바로 지우고 200이다(파일은 백업 풀에 남는다).
#[utoipa::path(delete, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), DeleteBackupQuery),
    responses(
        (status = 202, body = Resource<TaskSpec, TaskStatus>),
        (status = 200, body = BackupForgotten, description = "force: only the resource was deleted"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn delete_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<DeleteBackupQuery>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    if let Some(doing) = backup_tasks(&state, backup.id)?.first() {
        return Err(backup_busy(&backup, doing));
    }
    let node = match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)?
    {
        // 풀과 함께 파일도 없어졌다. 이 노드의 Task가 리소스만 지운다.
        None => state.node,
        Some(pool) => match files_node(&state, &backup, &pool) {
            Ok(node) => node,
            Err(e) if q.force && e.status == StatusCode::CONFLICT => {
                tracing::warn!(backup = %backup.name, pool = %pool.name, reason = %e.message, "deleting backup without its files");
                state.cluster.delete::<BackupSpec>(backup.id, None).await?;
                let message = format!(
                    "backup {:?} deleted; its files are left in storage pool {:?} ({})",
                    backup.name, pool.name, e.message
                );
                return Ok(Json(BackupForgotten { message }).into_response());
            }
            Err(e) => return Err(e),
        },
    };
    let task = create_task(&state.cluster, TaskOp::BackupDelete, backup.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// `ready` 백업을 새 VM `name`으로 되살릴 Task를 돌려준다(202). Task의 대상은 미리 정한 새 VM id다.
/// 백업의 VM 스펙이 쓰는 네트워크·보안 그룹이 지금 있어야 한다(400). 백업이 준비되지 않았으면 409
/// `backup_not_ready`, 지우는 중이면 409 `backup_busy`, 이름이 겹치면 409 `name_taken`. 노드는 `node`나
/// 백업 풀과 대상 풀을 함께 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(없으면 409 `no_node`).
/// 공유가 아닌 풀의 백업은 파일을 쓴 노드(`status.node`)에서만 되살린다: `node`가 다르면 409
/// `backup_on_other_node`. 고른 노드가 오프라인이면 409 `node_offline`.
#[utoipa::path(post, path = "/backups/{key}/restore", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    request_body = RestoreRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn restore_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    if backup.status.phase != BackupPhase::Ready {
        return Err(ApiError::conflict(
            "backup_not_ready",
            format!("backup {:?} is not ready", backup.name),
        ));
    }
    if backup_tasks(&state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(&backup, "being deleted"));
    }
    let spec = &backup.spec.vm_spec;
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
    check_security_groups(&state, spec)?;
    let source = find_pool(&state, &backup.spec.pool)?;
    let target = find_pool(&state, &req.pool)?;
    let node = match (&req.node, backup.status.node) {
        (key, Some(stored)) => {
            if let Some(key) = key
                && lookup::<NodeSpec>(&state.cluster, key)?.id != stored
            {
                return Err(ApiError::conflict(
                    "backup_on_other_node",
                    format!(
                        "backup {:?} is stored on node {:?}",
                        backup.name,
                        node_name(&state, stored)?
                    ),
                ));
            }
            check_online(&state, stored)?;
            let name = node_name(&state, stored)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, stored, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, name
                    )
                })?;
            }
            stored
        }
        (Some(key), None) => {
            let node = lookup::<NodeSpec>(&state.cluster, key)?;
            check_online(&state, node.id)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, node.id, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, node.name
                    )
                })?;
            }
            node.id
        }
        (None, None) => first_node(&state, |n| {
            pool_usable_on(&source, n) && pool_usable_on(&target, n)
        })?
        .ok_or_else(|| {
            ApiError::conflict(
                "no_node",
                format!(
                    "no online node can use both storage pools {:?} and {:?}",
                    source.name, target.name
                ),
            )
        })?,
    };
    let op = TaskOp::BackupRestore {
        backup: backup.id,
        name: req.name,
        pool: target.name,
    };
    let task = create_task(&state.cluster, op, Uuid::now_v7(), node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod auth;
pub mod cluster;
```

을 다음으로 바꾼다:

```rust
pub mod auth;
mod backups;
pub mod cluster;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(snapshots::clone_vm))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(snapshots::clone_vm))
        .routes(routes!(backups::list_backups))
        .routes(routes!(backups::get_backup, backups::delete_backup))
        .routes(routes!(backups::create_backup))
        .routes(routes!(backups::restore_backup))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

`crates/opl-api/src/resources.rs`에서

```rust
use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
```

을 다음으로 바꾼다:

```rust
use crate::auth::{Action, Principal};
use crate::backups::check_no_backups;
use crate::error::{ApiError, Json};
```

`crates/opl-api/src/resources.rs`에서

```rust

/// 볼륨이 남아 있으면 409
#[utoipa::path(delete, path = "/storage-pools/{key}", tag = "storage",
```

을 다음으로 바꾼다:

```rust

/// 볼륨이나 백업이 남아 있으면 409 `pool_in_use`
#[utoipa::path(delete, path = "/storage-pools/{key}", tag = "storage",
```

`crates/opl-api/src/resources.rs`에서

```rust
            format!("pool {:?} still has volume {:?}", pool.name, v.name),
        ));
    }
    state
        .cluster
        .delete::<StoragePoolSpec>(pool.id, if_match(&headers)?)
```

을 다음으로 바꾼다:

```rust
            format!("pool {:?} still has volume {:?}", pool.name, v.name),
        ));
    }
    check_no_backups(&state, &pool)?;
    state
        .cluster
        .delete::<StoragePoolSpec>(pool.id, if_match(&headers)?)
```

`crates/opl-api/src/resources.rs`에서

```rust
/// 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
/// 같은 값이면 쓰지 않고 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
```

을 다음으로 바꾼다:

```rust
/// 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
/// 백업이 남은 풀의 `backups`를 끄거나 `shared`를 바꾸면 409 `pool_in_use`다. 같은 값이면 쓰지 않고
/// 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
```

`crates/opl-api/src/resources.rs`에서

```rust
            return Ok(Json(pool));
        }
```

을 다음으로 바꾼다:

```rust
            return Ok(Json(pool));
        }
        // 공유가 아닌 풀의 백업은 쓴 노드에만 있다(`status.node`). 공유 여부를 바꾸면 그 기록이 틀린다.
        if (pool.spec.backups && !spec.backups) || pool.spec.shared != spec.shared {
            check_no_backups(&state, &pool)?;
        }
```

`crates/opl-api/src/snapshots.rs`에서

```rust

/// 이 VM의 스냅샷 만들기·되돌리기나 복제 Task가 끝나지 않았으면 409 `vm_busy`. 스냅샷 Task는
/// 전원을 끄고 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나
```

을 다음으로 바꾼다:

```rust

/// 이 VM의 스냅샷 만들기·되돌리기나 복제·백업 Task가 끝나지 않았으면 409 `vm_busy`. 스냅샷 Task는
/// 전원을 끄고 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나
```

`crates/opl-api/src/snapshots.rs`에서

```rust
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "being snapshotted, reverted or cloned", |op| {
        matches!(
            op,
            TaskOp::SnapshotCreate { .. } | TaskOp::SnapshotRevert { .. } | TaskOp::VmClone { .. }
        )
    })
}
```

을 다음으로 바꾼다:

```rust
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(
        state,
        vm,
        "being snapshotted, reverted, cloned or backed up",
        |op| {
            matches!(
                op,
                TaskOp::SnapshotCreate { .. }
                    | TaskOp::SnapshotRevert { .. }
                    | TaskOp::VmClone { .. }
                    | TaskOp::VmBackup { .. }
            )
        },
    )
}
```

`crates/opl-api/src/snapshots.rs`에서

```rust
/// ([`block_snapshot_kind`]).
fn check_disks(state: &AppState, vm: &Vm) -> Result<Option<&'static str>, ApiError> {
    let mut method = None;
```

을 다음으로 바꾼다:

```rust
/// ([`block_snapshot_kind`]).
pub(crate) fn check_disks(state: &AppState, vm: &Vm) -> Result<Option<&'static str>, ApiError> {
    let mut method = None;
```

`crates/opl-api/src/volumes.rs`에서

```rust
/// Task가 같은 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let lowest = matches!(
```

을 다음으로 바꾼다:

```rust
/// Task가 같은 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
pub(crate) fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let lowest = matches!(
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 8] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 9] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::SecurityGroup,
];
```

을 다음으로 바꾼다:

```rust
    Kind::SecurityGroup,
    Kind::Backup,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
    #[test]
    fn security_groups_are_watchable() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn backups_are_watchable() {
        let kinds = parse_kinds("backup").unwrap();
        assert!(kinds.contains(&Kind::Backup));
    }

    #[test]
    fn security_groups_are_watchable() {
```

`crates/opl-api/tests/backups.rs`를 만든다:

```rust
//! 백업 API: 만들기·목록·읽기·지우기·복원 요청의 검사와 Task 만들기, 백업이 있는 풀 지키기.
//! Task 실행기를 멈춰 두고 API가 남긴 리소스와 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, mark_pool_ready, set_pool_state, vm_request};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus};
use opl_types::node::{Node, NodeSpec, PoolState};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// Task 실행기 없이 띄운다. 만든 Task는 pending으로 남는다.
async fn env() -> Env {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env
}

/// 남은 Task를 끝난 것으로 기록한다.
async fn finish_tasks(env: &Env) {
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let done = TaskStatus {
        state: TaskState::Succeeded,
        error: None,
    };
    for task in tasks {
        env.cluster
            .update_status::<TaskSpec>(task.id, &done)
            .await
            .unwrap();
    }
}

/// 풀을 저장소에 직접 만들고 이 노드에서 준비됐다고 기록한다.
async fn pool_with(env: &Env, name: &str, backend: PoolBackend, shared: bool, backups: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend,
            nodes: vec![],
            shared,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 디렉터리 풀
async fn dir_pool(env: &Env, name: &str, shared: bool, backups: bool) {
    let backend = PoolBackend::Dir {
        path: env.dir.path().join(name),
    };
    pool_with(env, name, backend, shared, backups).await;
}

/// 이 노드에 파일이 있는 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, pool: &str, name: &str, format: ImageFormat) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.into(),
            size_bytes: MIB,
            format,
        },
    );
    env.cluster.create(&volume).await.unwrap();
    volume.status.phase = VolumePhase::Ready;
    volume.status.node = Some(env.node);
    env.cluster
        .update_status::<VolumeSpec>(volume.id, &volume.status)
        .await
        .unwrap();
    volume.id
}

/// 볼륨을 붙인 꺼진 VM을 만든다.
async fn vm_with_disks(admin: &Client, name: &str, volumes: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = volumes
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

/// VM의 실제 상태와 그에 맞는 원하는 전원 상태를 직접 쓴다. 노드는 `status.node`에만 쓴다
/// (이 노드의 reconciler가 VM을 건드리지 않게).
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let power = match phase {
        VmPhase::Starting | VmPhase::Running => PowerState::Running,
        _ => PowerState::Stopped,
    };
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
    let spec = VmSpec {
        power,
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let status = VmStatus {
        phase,
        node,
        ..vm.status
    };
    env.cluster
        .update_status::<VmSpec>(id, &status)
        .await
        .unwrap();
}

/// 온라인인 다른 노드를 저장소에 만든다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    env.cluster.create(&node).await.unwrap();
    node.id
}

async fn backup(admin: &Client, vm: &str, body: Value) -> (StatusCode, Value) {
    admin.post(&format!("/vms/{vm}/backups"), body).await
}

/// 백업을 요청하고 그 백업 id를 돌려준다.
async fn backup_ok(admin: &Client, vm: &str, body: Value) -> Uuid {
    let (status, task) = backup(admin, vm, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task["spec"]["op"]["backup"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn set_phase(env: &Env, id: Uuid, phase: BackupPhase) {
    let status = BackupStatus {
        phase,
        created_at: Some(1),
        ..Default::default()
    };
    env.cluster
        .update_status::<BackupSpec>(id, &status)
        .await
        .unwrap();
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 백업 풀 `backups`, 디스크 풀 `local`, 디스크 하나의 꺼진 VM `web`
async fn setup(env: &Env, admin: &Client) -> Uuid {
    dir_pool(env, "backups", false, true).await;
    dir_pool(env, "local", false, false).await;
    volume(env, "local", "root", ImageFormat::Qcow2).await;
    vm_with_disks(admin, "web", &["root"]).await
}

#[tokio::test]
async fn backup_records_a_pending_backup_and_a_task_on_the_vm() {
    let env = env().await;
    let admin = env.admin().await;
    let vm = setup(&env, &admin).await;

    let (status, task) = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"]["kind"], "vm-backup");
    assert_eq!(task["spec"]["target"], vm.to_string());
    // 꺼져 있고 배치되지 않은 VM은 디스크와 백업 풀을 함께 쓸 수 있는 노드에서 돈다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(task["status"]["state"], "pending");
    let id = task["spec"]["op"]["backup"].as_str().unwrap();

    let (status, got) = admin.get(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["phase"], "pending");
    assert_eq!(got["spec"]["vm"], vm.to_string());
    assert_eq!(got["spec"]["vm_name"], "web");
    assert_eq!(got["spec"]["pool"], "backups");
    assert_eq!(got["spec"]["vm_spec"]["disks"][0]["volume"], "root");
    assert!(got["spec"].get("job").is_none(), "{got}");
    // 기본 이름은 `{VM 이름}-{yyyymmdd-HHMMSS}`다.
    let name = got["name"].as_str().unwrap();
    let stamp = name.strip_prefix("web-").expect(name);
    assert_eq!(stamp.len(), 15, "{name}");
    assert_eq!(&stamp[8..9], "-", "{name}");
    assert!(
        stamp
            .chars()
            .filter(|c| *c != '-')
            .all(|c| c.is_ascii_digit()),
        "{name}"
    );
    finish_tasks(&env).await;

    // 이름을 주면 그 이름이다. 이름이나 id로 읽는다.
    let named = backup_ok(
        &admin,
        "web",
        json!({ "pool": "backups", "name": "before" }),
    )
    .await;
    let (status, got) = admin.get("/backups/before").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], named.to_string());
    assert_error(
        &admin.get("/backups/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    finish_tasks(&env).await;

    // 같은 이름은 백업마다 하나다(VM이 달라도).
    volume(&env, "local", "db-root", ImageFormat::Qcow2).await;
    let db = vm_with_disks(&admin, "db", &["db-root"]).await;
    assert_error(
        &backup(&admin, "db", json!({ "pool": "backups", "name": "before" })).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
    backup_ok(&admin, "db", json!({ "pool": "backups", "name": "db-1" })).await;

    // 목록은 전부, `vm`(id나 이름)으로 거른다.
    let (status, list) = admin.get("/backups").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert!(list["revision"].as_u64().unwrap() > 0);
    for key in ["web".to_string(), vm.to_string()] {
        let (_, list) = admin.get(&format!("/backups?vm={key}")).await;
        let names: Vec<_> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["spec"]["vm"].clone())
            .collect();
        assert_eq!(names, vec![json!(vm), json!(vm)], "{list}");
    }
    let (_, list) = admin.get(&format!("/backups?vm={db}")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // 지운 VM의 백업은 그 VM의 이름으로도 찾는다.
    finish_tasks(&env).await;
    assert_eq!(admin.delete("/vms/db").await.0, StatusCode::NO_CONTENT);
    let (_, list) = admin.get("/backups?vm=db").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["items"][0]["name"], "db-1");
    let (_, list) = admin.get("/backups?vm=nobody").await;
    assert_eq!(list["items"], json!([]));
}

#[tokio::test]
async fn backup_validates_the_request() {
    let env = env().await;
    let admin = env.admin().await;
    let vm = setup(&env, &admin).await;
    dir_pool(&env, "plain", false, false).await;

    assert_error(
        &backup(&admin, "nope", json!({ "pool": "backups" })).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    for body in [
        json!({ "pool": "missing" }),
        // 백업 풀이 아니다.
        json!({ "pool": "plain" }),
        json!({ "pool": "backups", "name": "Bad Name" }),
        json!({ "pool": "backups", "name": Uuid::now_v7().to_string() }),
        json!({ "pool": "backups", "pol": "x" }),
    ] {
        let got = backup(&admin, "web", body).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    }
    let got = backup(&admin, "web", json!({ "pool": "plain" })).await;
    assert!(
        got.1["message"].as_str().unwrap().contains("backups"),
        "{}",
        got.1
    );

    // 시작 중인 VM은 받지 않는다.
    set_vm_status(&env, vm, VmPhase::Starting, Some(env.node)).await;
    assert_error(
        &backup(&admin, "web", json!({ "pool": "backups" })).await,
        StatusCode::CONFLICT,
        "vm_busy",
    );
    // 실행 중이면 VM의 노드에서 돈다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;

    // 백업 풀이 VM의 노드에 없다.
    let n2 = other_node(&env, "n2").await;
    let far = Resource::new(
        "far",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("far"),
            },
            nodes: vec![n2],
            shared: false,
            backups: true,
        },
    );
    env.cluster.create(&far).await.unwrap();
    mark_pool_ready(&env.cluster, n2, "far").await;
    assert_error(
        &backup(&admin, "web", json!({ "pool": "far" })).await,
        StatusCode::CONFLICT,
        "pool_not_on_node",
    );
}

#[tokio::test]
async fn running_vms_need_disks_that_can_be_snapshotted() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let san = PoolBackend::LvmShared {
        vg: "san".into(),
        iscsi: None,
    };
    pool_with(&env, "san", san, true, false).await;
    volume(&env, "local", "root", ImageFormat::Qcow2).await;
    volume(&env, "local", "raw", ImageFormat::Raw).await;
    volume(&env, "san", "lun", ImageFormat::Raw).await;
    let mixed = vm_with_disks(&admin, "mixed", &["root", "raw"]).await;
    let shared = vm_with_disks(&admin, "shared", &["lun"]).await;

    for (vm, name, disk) in [(mixed, "mixed", "raw"), (shared, "shared", "lun")] {
        set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
        let got = backup(&admin, name, json!({ "pool": "backups" })).await;
        assert_error(&got, StatusCode::CONFLICT, "vm_running");
        let message = got.1["message"].as_str().unwrap();
        assert!(message.contains(disk), "{message}");
        assert!(message.contains("stop it to back it up"), "{message}");
        // 멈춘 VM은 된다.
        set_vm_status(&env, vm, VmPhase::Stopped, None).await;
        backup_ok(&admin, name, json!({ "pool": "backups" })).await;
    }
}

#[tokio::test]
async fn stopped_vms_without_a_node_run_where_the_disks_and_the_backup_pool_are() {
    let env = env().await;
    let admin = env.admin().await;
    // 이름 순으로 이 노드(`node1`)보다 앞선 노드
    let a = other_node(&env, "a-node").await;
    dir_pool(&env, "backups", true, true).await;
    dir_pool(&env, "shared", true, false).await;
    dir_pool(&env, "local", false, false).await;
    mark_pool_ready(&env.cluster, a, "backups").await;
    mark_pool_ready(&env.cluster, a, "shared").await;
    volume(&env, "shared", "s0", ImageFormat::Qcow2).await;
    volume(&env, "local", "l0", ImageFormat::Qcow2).await;
    vm_with_disks(&admin, "on-shared", &["s0"]).await;
    vm_with_disks(&admin, "on-local", &["l0"]).await;

    // 공유 풀의 디스크는 두 노드 모두에서 읽힌다. 이름이 앞선 노드다.
    let (status, task) = backup(&admin, "on-shared", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], a.to_string());
    // 로컬 풀의 디스크는 파일이 있는 노드에서만 읽힌다.
    let (status, task) = backup(&admin, "on-local", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;

    // 디스크가 있는 노드에서 백업 풀을 쓸 수 없으면 돌 노드가 없다.
    let only_a = Resource::new(
        "only-a",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("only-a"),
            },
            nodes: vec![a],
            shared: true,
            backups: true,
        },
    );
    env.cluster.create(&only_a).await.unwrap();
    mark_pool_ready(&env.cluster, a, "only-a").await;
    assert_error(
        &backup(&admin, "on-local", json!({ "pool": "only-a" })).await,
        StatusCode::CONFLICT,
        "no_node",
    );
    backup_ok(
        &admin,
        "on-shared",
        json!({ "pool": "only-a", "name": "on-a" }),
    )
    .await;
}

#[tokio::test]
async fn an_unfinished_backup_holds_the_vm() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;

    // 디스크를 복사하는 동안 켜거나 지우지 못한다.
    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    assert!(
        body["message"].as_str().unwrap().contains("backed up"),
        "{body}"
    );
    assert_error(
        &admin.delete("/vms/web").await,
        StatusCode::CONFLICT,
        "vm_busy",
    );
    finish_tasks(&env).await;
    assert_eq!(
        admin.post("/vms/web/actions/start", Value::Null).await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn delete_records_a_task_where_the_backup_pool_is() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let id = backup_ok(&admin, "web", json!({ "pool": "backups", "name": "b1" })).await;

    // 만드는 중에는 지우지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    finish_tasks(&env).await;
    // 실패한 백업도 지운다.
    set_phase(&env, id, BackupPhase::Failed).await;
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "backup-delete" }));
    assert_eq!(task["spec"]["target"], id.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    // 리소스는 Task가 지운다.
    assert_eq!(admin.get("/backups/b1").await.0, StatusCode::OK);
    // 지우는 중에 다시 지우거나 복원하지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    set_phase(&env, id, BackupPhase::Ready).await;
    assert_error(
        &admin
            .post(
                "/backups/b1/restore",
                json!({ "name": "web-2", "pool": "local" }),
            )
            .await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_error(
        &admin.delete("/backups/nope").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 꺼진 VM `web`의 Ready 백업 `b1`
async fn ready_backup(env: &Env, admin: &Client) -> Uuid {
    setup(env, admin).await;
    let id = backup_ok(admin, "web", json!({ "pool": "backups", "name": "b1" })).await;
    finish_tasks(env).await;
    set_phase(env, id, BackupPhase::Ready).await;
    id
}

async fn restore(admin: &Client, backup: &str, body: Value) -> (StatusCode, Value) {
    admin
        .post(&format!("/backups/{backup}/restore"), body)
        .await
}

#[tokio::test]
async fn restore_records_a_task_for_a_new_vm() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();

    let (status, task) = restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "backup-restore", "backup": id, "name": "web-2", "pool": "local" })
    );
    assert_eq!(task["spec"]["node"], env.node.to_string());
    // 대상은 미리 정한 새 VM id다. VM은 Task가 만든다.
    let target: Uuid = task["spec"]["target"].as_str().unwrap().parse().unwrap();
    assert_ne!(target, vm.id);
    assert_ne!(target, id);
    assert!(env.cluster.get::<VmSpec>(target).unwrap().is_none());

    // 복원하는 중에는 백업을 지우지 못한다.
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    // 복원은 여럿 겹쳐도 된다. 노드는 이름이나 id로 고른다.
    for node in ["node1".to_string(), env.node.to_string()] {
        let (status, task) = restore(
            &admin,
            &id.to_string(),
            json!({ "name": "web-3", "pool": "backups", "node": node }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        assert_eq!(task["spec"]["node"], env.node.to_string());
    }
    finish_tasks(&env).await;
    // 원래 VM을 지워도 백업에서 되살린다.
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    let (status, task) = restore(&admin, "b1", json!({ "name": "web", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

#[tokio::test]
async fn restore_validates_the_request() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;

    assert_error(
        &restore(&admin, "nope", json!({ "name": "x", "pool": "local" })).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    // 이름은 지금 VM과 겹치면 안 된다.
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web", "pool": "local" })).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
    for body in [
        json!({ "name": "Bad Name", "pool": "local" }),
        json!({ "name": "web-2", "pool": "missing" }),
        json!({ "name": "web-2" }),
        json!({ "name": "web-2", "pool": "local", "extra": 1 }),
    ] {
        let got = restore(&admin, "b1", body).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert_error(
        &restore(
            &admin,
            "b1",
            json!({ "name": "web-2", "pool": "local", "node": "nope" }),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // 만드는 중이거나 실패한 백업은 되살리지 못한다.
    for phase in [BackupPhase::Pending, BackupPhase::Failed] {
        set_phase(&env, id, phase).await;
        assert_error(
            &restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await,
            StatusCode::CONFLICT,
            "backup_not_ready",
        );
    }
    set_phase(&env, id, BackupPhase::Ready).await;

    // 고른 노드에서 백업 풀이나 대상 풀을 쓸 수 없다.
    let n2 = other_node(&env, "n2").await;
    let down = PoolState {
        ready: false,
        error: Some("not mounted".into()),
        ..Default::default()
    };
    set_pool_state(&env.cluster, n2, "backups", down).await;
    assert_error(
        &restore(
            &admin,
            "b1",
            json!({ "name": "web-2", "pool": "local", "node": "n2" }),
        )
        .await,
        StatusCode::CONFLICT,
        "pool_unavailable",
    );
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    let (status, task) = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": n2 }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;

    // 두 풀을 함께 쓰는 노드가 없다.
    let far = Resource::new(
        "far",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("far"),
            },
            nodes: vec![Uuid::now_v7()],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&far).await.unwrap();
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "far" })).await,
        StatusCode::CONFLICT,
        "no_node",
    );
}

#[tokio::test]
async fn restore_needs_the_networks_and_security_groups_of_the_backup() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let mut vm_spec: VmSpec = serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
    vm_spec.nics = serde_json::from_value(json!([
        { "network": "lan", "model": "virtio", "security_groups": ["web"] }
    ]))
    .unwrap();
    let mut backup: Backup = Resource::new(
        "b1",
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec,
            job: None,
        },
    );
    backup.status.phase = BackupPhase::Ready;
    env.cluster.create(&backup).await.unwrap();
    env.cluster
        .update_status::<BackupSpec>(backup.id, &backup.status)
        .await
        .unwrap();
    let body = json!({ "name": "web-2", "pool": "local" });

    let got = restore(&admin, "b1", body.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("network \"lan\""),
        "{}",
        got.1
    );
    let (status, net) = admin
        .post(
            "/networks",
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{net}");
    let got = restore(&admin, "b1", body.clone()).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("security group \"web\""),
        "{}",
        got.1
    );
    let (status, sg) = admin
        .post("/security-groups", json!({ "name": "web", "spec": {} }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sg}");
    let (status, task) = restore(&admin, "b1", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 백업의 상태를 직접 쓴다: Ready, 파일을 쓴 노드 `node`
async fn set_stored(env: &Env, id: Uuid, node: Option<Uuid>) {
    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(1),
        node,
        ..Default::default()
    };
    env.cluster
        .update_status::<BackupSpec>(id, &status)
        .await
        .unwrap();
}

/// 공유가 아닌 백업 풀의 백업은 파일을 쓴 노드(`status.node`)에만 있다. 지우기와 복원은 그 노드에서
/// 돌고, 다른 노드를 고르거나 그 노드에서 대상 풀을 쓸 수 없으면 409다.
#[tokio::test]
async fn backups_in_local_pools_are_handled_on_their_node() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    set_stored(&env, id, Some(n2)).await;

    // 지우기와 복원은 파일이 있는 n2에서 돈다(이 노드가 백업 풀을 쓸 수 있어도).
    let (status, task) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;
    let (status, task) = restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], n2.to_string());
    finish_tasks(&env).await;
    let (status, task) = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    finish_tasks(&env).await;

    // 다른 노드에서는 되살리지 못한다.
    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "node1" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "backup_on_other_node");
    assert_eq!(got.1["message"], "backup \"b1\" is stored on node \"n2\"");

    // 대상 풀이 n2에 없다.
    let here = Resource::new(
        "here",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join("here"),
            },
            nodes: vec![env.node],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&here).await.unwrap();
    env.pool_ready("here").await;
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "here" })).await,
        StatusCode::CONFLICT,
        "pool_not_on_node",
    );

    // 이 노드에 있는 백업은 이 노드에서 다룬다.
    set_stored(&env, id, Some(env.node)).await;
    let (status, task) = admin.delete("/backups/b1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    finish_tasks(&env).await;
    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "backup_on_other_node");
    assert_eq!(
        got.1["message"],
        "backup \"b1\" is stored on node \"node1\""
    );
}

async fn set_online(env: &Env, node: Uuid, online: bool) {
    let mut n = env.cluster.get::<NodeSpec>(node).unwrap().unwrap();
    n.status.online = online;
    env.cluster
        .update_status::<NodeSpec>(node, &n.status)
        .await
        .unwrap();
}

/// 파일이 있는 노드나 고른 노드가 오프라인이면 409 `node_offline`이다.
#[tokio::test]
async fn offline_nodes_do_not_take_backup_tasks() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;
    mark_pool_ready(&env.cluster, n2, "backups").await;
    mark_pool_ready(&env.cluster, n2, "local").await;
    set_online(&env, n2, false).await;

    let got = restore(
        &admin,
        "b1",
        json!({ "name": "web-2", "pool": "local", "node": "n2" }),
    )
    .await;
    assert_error(&got, StatusCode::CONFLICT, "node_offline");
    assert_eq!(got.1["message"], "node \"n2\" is offline");

    set_stored(&env, id, Some(n2)).await;
    assert_error(
        &restore(&admin, "b1", json!({ "name": "web-2", "pool": "local" })).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    assert_error(
        &admin.delete("/backups/b1").await,
        StatusCode::CONFLICT,
        "node_offline",
    );
}

/// 풀이 없어진 백업은 이 노드의 Task가 리소스만 지운다. 풀은 있는데 파일을 지울 노드가 없으면 409이고,
/// `force`면 리소스만 바로 지운다(파일은 남는다).
#[tokio::test]
async fn backups_without_a_usable_pool_can_be_forgotten() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;
    let n2 = other_node(&env, "n2").await;

    // 파일이 있는 노드에서 풀이 준비되지 않았다.
    let down = PoolState {
        ready: false,
        error: Some("not mounted".into()),
        ..Default::default()
    };
    set_pool_state(&env.cluster, n2, "backups", down).await;
    set_stored(&env, id, Some(n2)).await;
    let got = admin.delete("/backups/b1").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_unavailable");
    let got = admin.delete("/backups/b1?force=false").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_unavailable");
    assert!(env.cluster.get::<BackupSpec>(id).unwrap().is_some());
    let (status, body) = admin.delete("/backups/b1?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("files are left in storage pool \"backups\""),
        "{message}"
    );
    assert!(env.cluster.get::<BackupSpec>(id).unwrap().is_none());
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    assert!(tasks.iter().all(|t| t.spec.target != id), "{tasks:?}");

    // 풀째 없어졌으면 이 노드의 Task가 리소스만 지운다.
    let b2 = backup_ok(&admin, "web", json!({ "pool": "backups", "name": "b2" })).await;
    finish_tasks(&env).await;
    set_stored(&env, b2, Some(n2)).await;
    let pool = env
        .cluster
        .get_by_name::<StoragePoolSpec>("backups")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<StoragePoolSpec>(pool.id, None)
        .await
        .unwrap();
    let (status, task) = admin.delete("/backups/b2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["target"], b2.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
}

/// 지운 풀에는 백업을 만들지 못한다.
#[tokio::test]
async fn backups_need_a_pool_that_still_exists() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    dir_pool(&env, "gone", false, true).await;
    assert_eq!(
        admin.delete("/storage-pools/gone").await.0,
        StatusCode::NO_CONTENT
    );
    let got = backup(&admin, "web", json!({ "pool": "gone" })).await;
    assert!(got.0.is_client_error(), "{got:?}");
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
}

/// 백업을 먼저 만들면(끝나지 않았어도) 그 풀은 지우지 못한다.
#[tokio::test]
async fn pools_with_a_new_backup_cannot_be_deleted() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;
    let got = admin.delete("/storage-pools/backups").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
}

/// 실행 중인 VM의 디스크가 서로 다른 스냅샷 방식을 쓰면 스냅샷처럼 거절한다.
#[tokio::test]
async fn running_vms_with_mixed_snapshot_methods_are_refused() {
    let env = env().await;
    let admin = env.admin().await;
    dir_pool(&env, "backups", false, true).await;
    dir_pool(&env, "local", false, false).await;
    let thin = PoolBackend::LvmThin {
        vg: "vg0".into(),
        thinpool: "tp".into(),
    };
    pool_with(&env, "thin", thin, false, false).await;
    volume(&env, "local", "root", ImageFormat::Qcow2).await;
    volume(&env, "thin", "lv", ImageFormat::Raw).await;
    let vm = vm_with_disks(&admin, "web", &["root", "lv"]).await;

    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let got = backup(&admin, "web", json!({ "pool": "backups" })).await;
    assert_error(&got, StatusCode::CONFLICT, "snapshot_unsupported");
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("different snapshot methods"),
        "{}",
        got.1
    );
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
    // 멈춘 VM은 된다.
    set_vm_status(&env, vm, VmPhase::Stopped, None).await;
    backup_ok(&admin, "web", json!({ "pool": "backups" })).await;
}

/// `?vm=이름`은 지금 그 이름인 VM의 백업과 그 이름으로 남은(지운 VM의) 백업을 함께 준다.
#[tokio::test]
async fn listing_by_vm_name_includes_deleted_vms_of_that_name() {
    let env = env().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backup_ok(&admin, "web", json!({ "pool": "backups", "name": "old" })).await;
    finish_tasks(&env).await;
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    let web = vm_with_disks(&admin, "web", &["root"]).await;
    backup_ok(&admin, "web", json!({ "pool": "backups", "name": "new" })).await;

    let names = |list: &Value| {
        let mut names: Vec<String> = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };
    let (_, list) = admin.get("/backups?vm=web").await;
    assert_eq!(names(&list), ["new", "old"], "{list}");
    // id는 그 VM의 백업만이다.
    let (_, list) = admin.get(&format!("/backups?vm={web}")).await;
    assert_eq!(names(&list), ["new"], "{list}");
}

#[tokio::test]
async fn pools_holding_backups_stay() {
    let env = env().await;
    let admin = env.admin().await;
    let id = ready_backup(&env, &admin).await;

    let got = admin.delete("/storage-pools/backups").await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    let got = admin
        .patch("/storage-pools/backups", json!({ "backups": false }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    // 공유가 아닌 풀의 백업은 쓴 노드에만 있으므로 공유 여부도 바꾸지 못한다.
    let got = admin
        .patch("/storage-pools/backups", json!({ "shared": true }), None)
        .await;
    assert_error(&got, StatusCode::CONFLICT, "pool_in_use");
    assert_eq!(got.1["message"], "pool \"backups\" holds backups");
    // 그대로 두는 PATCH는 된다.
    let (status, body) = admin
        .patch(
            "/storage-pools/backups",
            json!({ "shared": false, "backups": true }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["backups"], true);

    env.cluster.delete::<BackupSpec>(id, None).await.unwrap();
    let (status, body) = admin
        .patch("/storage-pools/backups", json!({ "backups": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        admin.delete("/storage-pools/backups").await.0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn viewers_read_backups_and_operators_manage_them() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    let id = ready_backup(&env, &admin).await;

    assert_eq!(viewer.get("/backups").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/backups/b1").await.0, StatusCode::OK);
    for got in [
        backup(&viewer, "web", json!({ "pool": "backups" })).await,
        restore(&viewer, "b1", json!({ "name": "web-2", "pool": "local" })).await,
        viewer.delete("/backups/b1").await,
    ] {
        assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
    }

    let (status, body) = backup(&operator, "web", json!({ "pool": "backups" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) =
        restore(&operator, "b1", json!({ "name": "web-2", "pool": "local" })).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    finish_tasks(&env).await;
    let (status, body) = operator.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/vms/{key}/migrate",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/vms/{key}/migrate",
        "/vms/{key}/backups",
        "/backups",
        "/backups/{key}",
        "/backups/{key}/restore",
        "/watch",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_BackupSpec_BackupStatus"].is_object());
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 968개, 실패 0

Run: `cargo test -p opl-api --test backups --test openapi --test pools`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 351개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-api): 백업 API — 만들기·목록·읽기·지우기·복원 요청과 Task 만들기, 백업이 남은 풀 지키기, watch 종류 backup"
```

---

### Task 3: 백업 Task

opl-api `tasks/backup.rs`: VM 큐에서(시간 제한 없음) 공유가 아닌 풀이면 자기 노드를 `status.node`에 먼저 적고, 실행 중이면 지금 스냅샷 코드의 길(`take_tags`로 함수화)로 메모리 없는 임시 스냅샷을 찍고(블록 백엔드는 짧게 멈춤 — 재개가 두 번 실패하면 내보내기 전에 실패), 디스크마다 driver `export_volume`(`qemu-img convert -c -t writeback -O qcow2`, `.part` 뒤 rename)으로 내보낸다. UEFI 변수 복사, 매니페스트를 `.part`로 쓰고 fsync·rename·디렉터리 fsync 뒤 `ready`. 임시 태그는 늘 지우고, 실패하면 디렉터리를 지우고 `failed`. opld 재시작으로 끊긴 백업도 정리. 지우기 Task(백업 id 큐): 디렉터리와 리소스를 지우고, 같은 백업의 복원이 진행 중이면 실패. 블록 태그 청소는 진행 중인 백업의 태그만 남긴다. TaskRunner의 `mount_root`(opld가 `{data_dir}/pools`).

**Files:**
- Create: `crates/opl-api/src/tasks/backup.rs`
- Create: `crates/opl-api/tests/backup_tasks.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/migrate.rs`
- Modify: `crates/opl-api/src/tasks/snapshot.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/src/storage/storages.rs`
- Modify: `crates/opl-driver/tests/dir_storage.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod backup;
mod clone;
mod migrate;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::Peers;
use opl_reconciler::{FirmwareFetcher, ReconcilerHandle};
use opl_types::backup::{BackupPhase, BackupSpec};
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

#[derive(Clone)]
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
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 백업 Task가 백업 풀의 파일 경로를 정한다.
    pub mount_root: PathBuf,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로). 확정되지 않았거나 확정 뒤 남은 마이그레이션 QEMU도 이 간격으로 정리한다.
    pub tag_sweep: Duration,
    /// 라이브 마이그레이션을 보낼 때 대상 노드를 부르는 노드 간 클라이언트. 없으면 마이그레이션 Task는
    /// 실패한다.
    pub peers: Option<Arc<Peers>>,
    /// 마이그레이션을 보내는 대역폭 상한(바이트/초). `None`이면 QEMU 기본값
    pub migration_bandwidth: Option<u64>,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다. 마이그레이션 Task는 `timeout` 대신 이것을 쓴다.
    pub migration_timeout: Duration,
    /// 스냅샷·복제가 UEFI 변수를 복사하기 전에, 변수 파일이 다른 노드에 있으면 받아 온다. 없으면
    /// 이 노드의 파일을 그대로 쓴다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            mount_root: PathBuf::new(),
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
            peers: None,
            migration_bandwidth: None,
            migration_timeout: Duration::from_secs(30 * 60),
            firmware: None,
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
    let migrations = tokio::spawn(migrate::sweep_every(ctx.clone()));
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
    migrations.abort();
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
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. 백업의 임시 스냅샷(백업 id의 simple
/// 문자열)도 같은 모양이므로, 백업 Task가 끝나지 않은 `pending` 백업의 태그는 둔다. opLite 밖에서 만든 태그는 두고,
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
            match ctx.cluster.get::<BackupSpec>(id) {
                Ok(Some(b)) if b.status.phase != BackupPhase::Pending => {}
                // 채울 Task가 없는 pending 백업(Task가 끝났거나 지워졌다)의 태그도 남은 것이다.
                Ok(Some(_)) if !backing_up(&ctx, id) => {}
                Ok(None) => {}
                _ => continue,
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

/// 백업 `id`를 채우는 백업 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn backing_up(ctx: &Ctx, id: Uuid) -> bool {
    let Ok((_, tasks)) = ctx.cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == id)
    })
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

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식. 이 노드로
/// 마이그레이션하는 중인 VM의 디스크도 쓰는 중이다(받는 QEMU를 띄우기 전에 켠다). 이 노드에서 보내는
/// 중인 VM은 원하는 상태가 running이고 마이그레이션 Task가 있다.
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
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running)
            || vm.status.migration.is_some_and(|m| m.node == me);
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
/// 마이그레이션 Task는 VM의 줄에 선다(앞의 스냅샷·복제가 끝난 뒤 옮긴다). 백업 Task도 VM의 줄에
/// 선다(임시 스냅샷을 찍고 디스크를 읽는다). 백업 지우기와 복원은 그 백업의 줄에 서서 겹치지 않는다
/// (복원 Task의 대상은 새 VM이다).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if let TaskOp::BackupRestore { backup, .. } = task.spec.op {
        return Some(backup);
    }
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. } | TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
        );
        let backup_queue = matches!(task.spec.op, TaskOp::BackupDelete);
        return (is_volume_op(&task.spec.op)
            || is_snapshot_op(&task.spec.op)
            || vm_queue
            || backup_queue)
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
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업은 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다(설계 §2.3).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
    );
    let (result, unfinished) = if unlimited {
        (perform(ctx, &task).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        }
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
    cleanup(ctx, task, &error, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다. 실패한
/// 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]).
async fn cleanup(ctx: &Ctx, task: &Task, error: &str, unfinished: bool) {
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
    if let TaskOp::VmBackup { backup } = task.spec.op {
        backup::discard(ctx, task, backup, error, unfinished).await;
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
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
        // 기록 Task는 끝난 상태로 만들어지므로 실행되지 않는다.
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
        TaskOp::VmBackup { backup } => backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        // 복원 실행은 아직 없다.
        TaskOp::BackupRestore { .. } => {
            Err("restoring backups is not supported by this node yet".into())
        }
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

`crates/opl-api/src/tasks/backup.rs`를 만든다:

```rust
//! 백업 Task(설계 §2.3)와 백업 지우기 Task. 백업은 VM의 줄에 서서 그 VM의 스냅샷·복제 Task와 차례로
//! 돌고, 지우기는 백업의 줄에 선다. API가 확인한 뒤 줄에서 기다리는 사이 VM이나 풀이 바뀌었을 수
//! 있으므로 다시 확인한다.

use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{check_pool_root, uefi_vars_path};
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE,
    MANIFEST_VERSION, backup_dir,
};
use opl_types::node::NodeSpec;
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, pool_error_on, pool_listed_on, pool_usable_on,
};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::Ctx;
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_vm, is_down, now_secs, ready_disks,
    remove_tags, take_tags,
};

/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(super) const VARS_FILE: &str = "vars.fd";

fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
}

/// 실행 중인 VM의 디스크에 찍는 임시 스냅샷의 태그: 백업 id의 simple 문자열. 스냅샷 태그(하이픈 있는
/// id)와 구분되고, 남으면 태그 청소가 백업 리소스를 보고 지운다.
fn temp_tag(backup: Uuid) -> String {
    backup.simple().to_string()
}

/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면(마운트되지 않은 NFS
/// 풀의 빈 마운트 지점에 쓰지 않는다) 오류다.
fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))?;
    if !pool_listed_on(&pool, ctx.config.node) {
        return Err(format!(
            "storage pool {name:?} is not available on this node"
        ));
    }
    let me = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| e.to_string())?
        .ok_or("this node is not registered")?;
    if !pool_usable_on(&pool, &me) {
        return Err(format!(
            "storage pool {name:?} is not ready on this node: {}",
            pool_error_on(&pool, &me)
        ));
    }
    let root = check_pool_root(&pool, &ctx.config.mount_root)
        .map_err(|e| format!("storage pool {name:?}: {e}"))?;
    Ok((pool, root))
}

/// VM의 UEFI 변수 파일(있으면)을 `to`로 복사한다. 변수 파일이 다른 노드에 있으면 받아 온다.
/// BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 복사할 것이 없다.
async fn copy_vars(ctx: &Ctx, vm: &Vm, to: &Path) -> std::io::Result<()> {
    let Some(dir) = &ctx.config.state_dir else {
        return Ok(());
    };
    fetch_vars(ctx, vm).await;
    let from = uefi_vars_path(dir, vm.id);
    if !tokio::fs::try_exists(&from).await? {
        return Ok(());
    }
    copy_file(&from, to).await
}

/// 디스크마다 `tag`(없으면 지금) 때의 내용을 `{dir}/disk{i}.qcow2`로 내보낸다. `force_share`는 실행
/// 중인 VM이 쥔 디스크의 스냅샷을 읽을 때 쓴다(스냅샷 데이터는 바뀌지 않는다).
async fn export_disks(
    ctx: &Ctx,
    disks: &[(Volume, StoragePool)],
    tag: Option<&str>,
    force_share: bool,
    dir: &Path,
) -> Result<Vec<BackupDisk>, String> {
    let mut out = Vec::with_capacity(disks.len());
    for (i, (volume, pool)) in disks.iter().enumerate() {
        let name = format!("disk{i}");
        let file = format!("{name}.qcow2");
        let dest = dir.join(&file);
        let format = volume.spec.format;
        let source = ctx
            .storage
            .volume_source(pool, volume.id, format, tag)
            .await
            .map_err(|e| format!("cannot read volume {:?}: {e}", volume.name))?;
        let exported = ctx.storage.export_volume(&source, force_share, &dest).await;
        // 내보내기는 끝났다. 내놓지 못하면 켜진 채 남을 뿐이다(opld가 뒤에 정리한다).
        if let Err(e) = ctx
            .storage
            .release_source(pool, volume.id, format, tag)
            .await
        {
            tracing::warn!(volume = %volume.name, error = %e, "cannot release backup source");
        }
        exported.map_err(|e| format!("cannot back up volume {:?}: {e}", volume.name))?;
        let stored_bytes = tokio::fs::metadata(&dest)
            .await
            .map_err(|e| format!("cannot read size of {file}: {e}"))?
            .len();
        out.push(BackupDisk {
            name,
            file,
            virtual_bytes: source.size_bytes,
            stored_bytes,
        });
    }
    Ok(out)
}

/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// 매니페스트를 `{dir}/manifest.json.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고, 디렉터리도 내려
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let path = dir.join(MANIFEST_FILE);
    let part = dir.join(format!("{MANIFEST_FILE}.part"));
    let mut file = tokio::fs::File::create(&part).await?;
    file.write_all(&text).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&part, &path).await?;
    sync_path(dir).await
}

/// `pending` 백업 `id`를 채운다(설계 §2.3): 백업 풀의 `{루트}/backups/{id}/`에 디스크마다 압축 qcow2,
/// UEFI 변수, 매니페스트를 쓰고 `ready`로 바꾼다. 실행 중인 VM은 디스크에 메모리 없는 임시 스냅샷을
/// 찍어 그것을 읽고 끝나면 지운다(스냅샷 리소스는 만들지 않는다). 꺼진 VM은 지금 디스크를 읽는다.
/// 실패하면 실행기의 정리([`discard`])가 디렉터리를 지우고 `failed`로 남긴다.
pub(super) async fn backup(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    use super::snapshot::resume_vm;
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Pending {
        return Err("backup is not pending".into());
    }
    let vm = get_vm(ctx, task.spec.target)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    if !running && !stopped {
        return Err("VM is being started; try again when it has settled".into());
    }
    let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
    if !pool.spec.backups {
        return Err(format!(
            "storage pool {:?} does not store backups",
            pool.name
        ));
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    // 공유가 아닌 풀의 파일은 이 노드에만 생긴다. 쓰기 전에 적어 두어 실패한 백업도 이 노드에서 지운다.
    let node = (!opl_types::storage::is_shared(&pool)).then_some(ctx.config.node);
    if running
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !ctx.storage.supports_snapshots(p, v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted; stop the VM to back it up",
            volume.name, volume.spec.format
        ));
    }

    // 담는 VM 스펙은 실행하는 지금의 것이다(API가 확인한 뒤 VM 스펙이 바뀌었을 수 있다).
    if backup.spec.vm_spec != vm.spec || backup.spec.vm_name != vm.name {
        let spec = BackupSpec {
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            ..backup.spec.clone()
        };
        ctx.cluster
            .update_spec(id, Some(backup.revision), backup.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    if node.is_some() {
        let status = BackupStatus {
            node,
            ..backup.status.clone()
        };
        ctx.cluster
            .update_status::<BackupSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())?;
    }
    let dir = backup_dir(&root, id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create backup directory: {e}"))?;
    // 새 디렉터리 이름이 남게 `backups`와(처음 만들었을 수 있으니) 풀 루트도 내려 쓴다.
    for parent in [root.join("backups"), root.clone()] {
        sync_path(&parent)
            .await
            .map_err(|e| format!("cannot create backup directory: {e}"))?;
    }
    let vars = dir.join(VARS_FILE);
    let tag = temp_tag(id);
    // 실행 중이면 디스크를 임시 스냅샷으로 얼리고 그때의 UEFI 변수를 복사한다.
    let tagged = running && !disks.is_empty();
    let resumed = if tagged {
        take_tags(ctx, &vm, &disks, id, &tag, false, || {
            copy_vars(ctx, &vm, &vars)
        })
        .await?
    } else {
        copy_vars(ctx, &vm, &vars)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        Ok(())
    };
    // 블록 스냅샷을 위해 멈춘 VM을 이어서 실행하지 못했으면 한 번 더 해 본다. 그래도 멈춰 있으면 디스크를
    // 읽지 않고 실패한다(임시 스냅샷은 아래에서 지운다). 오래 걸리는 내보내기 동안 VM을 멈춰 두지 않는다.
    let resumed = match resumed {
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot resume VM after the temporary snapshot; retrying");
            resume_vm(&*ctx.hv, vm.id).await.map_err(|r| {
                format!(
                    "cannot resume the VM after its temporary snapshot: {e}; retrying failed: {r}"
                )
            })
        }
        Ok(()) => Ok(()),
    };
    let created_at = now_secs();
    let exported = match &resumed {
        Ok(()) => export_disks(ctx, &disks, tagged.then_some(&*tag), tagged, &dir).await,
        Err(e) => Err(e.clone()),
    };
    // 임시 스냅샷은 결과와 상관없이 지운다. 남으면 공간만 쓴다(블록 디스크는 태그 청소가 지운다).
    if tagged {
        let running = !matches!(ctx.hv.inspect(vm.id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm.id, running, &vm.spec, &tag).await {
            tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
        }
    }
    let disks = exported?;
    let uefi_vars = tokio::fs::try_exists(&vars).await.unwrap_or(false);
    if uefi_vars {
        sync_path(&vars)
            .await
            .map_err(|e| format!("cannot write UEFI variables: {e}"))?;
    }
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: id,
        vm: vm.id,
        vm_name: vm.name.clone(),
        vm_spec: vm.spec.clone(),
        disks: disks.clone(),
        uefi_vars,
        created_at,
    };
    write_manifest(&dir, &manifest)
        .await
        .map_err(|e| format!("cannot write manifest: {e}"))?;

    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        disks,
        uefi_vars,
        error: None,
        node,
    };
    // 다 만든 백업이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) => break,
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    Ok(())
}

/// 백업 디렉터리를 지운다. 없으면 이미 지운 것이다.
async fn remove_dir(dir: &Path) -> Result<(), String> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove backup directory: {e}")),
    }
}

/// 실패한 백업 Task의 흔적을 지운다: 아직 `pending`인 백업이면 디렉터리를 지우고 `failed`와 `error`를
/// 남긴다(리소스는 사용자가 보고 지운다). `unfinished`면(opld가 재시작해 중단) 실행 중인 VM의 디스크에
/// 남았을 수 있는 임시 스냅샷도 지운다(블록 디스크의 것은 태그 청소도 지운다).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str, unfinished: bool) {
    let backup = match get_backup(ctx, id) {
        Ok(Some(b)) if b.status.phase == BackupPhase::Pending => b,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(backup = %id, error = %e, "cannot read backup of failed task");
            return;
        }
    };
    if unfinished
        && let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(task.spec.target)
        && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_)))
        && let Err(e) = remove_tags(ctx, vm.id, true, &vm.spec, &temp_tag(id)).await
    {
        tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
    }
    match backup_root(ctx, &backup.spec.pool) {
        Ok((_, root)) => {
            if let Err(e) = remove_dir(&backup_dir(&root, id)).await {
                tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup");
            }
        }
        Err(e) => tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup"),
    }
    let status = BackupStatus {
        phase: BackupPhase::Failed,
        error: Some(error.to_string()),
        ..backup.status
    };
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup failed; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 백업 파일(디렉터리)과 리소스를 지운다. 디렉터리가 이미 없어도 성공이므로 중간에 멈춰도 다시 하면 된다.
/// 다만 공유가 아닌 풀에서 다른 노드가 쓴 백업(`status.node`)은 여기에 없는 것이 당연하므로 실패다.
/// 풀이 없어졌으면 파일도 함께 없어진 것으로 보고 리소스만 지운다. 이 백업을 되살리는 Task가 끝나지
/// 않았으면(다른 노드에서 돌 수 있다) 실패다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(());
    };
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::BackupRestore { backup, .. } if backup == id)
    }) {
        return Err("backup is being restored".into());
    }
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)
        .map_err(|e| e.to_string())?;
    if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
    } else {
        let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
        let dir = backup_dir(&root, id);
        let elsewhere = (!opl_types::storage::is_shared(&pool))
            .then_some(backup.status.node)
            .flatten()
            .filter(|n| *n != ctx.config.node);
        if let Some(node) = elsewhere
            && !tokio::fs::try_exists(&dir).await.unwrap_or(true)
        {
            return Err(format!("backup files are on node {node}, not on this node"));
        }
        remove_dir(&dir).await?;
    }
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust

/// 마이그레이션을 기록한 뒤 다시 본다: 이 VM의 스냅샷 만들기·되돌리기·복제 Task나 디스크의 크기
/// 늘리기 Task가 끝나지 않았으면 실패다. API는 마이그레이션 Task와 이것들을 함께 받지 않지만, 검사와
```

을 다음으로 바꾼다:

```rust

/// 마이그레이션을 기록한 뒤 다시 본다: 이 VM의 스냅샷 만들기·되돌리기·복제·백업 Task나 디스크의 크기
/// 늘리기 Task가 끝나지 않았으면 실패다. API는 마이그레이션 Task와 이것들을 함께 받지 않지만, 검사와
```

`crates/opl-api/src/tasks/migrate.rs`에서

```rust
            | TaskOp::VmClone { .. }
                if t.spec.target == vm.id =>
            {
                "being snapshotted, reverted or cloned"
            }
```

을 다음으로 바꾼다:

```rust
            | TaskOp::VmClone { .. }
            | TaskOp::VmBackup { .. }
                if t.spec.target == vm.id =>
            {
                "being snapshotted, reverted, cloned or backed up"
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
use opl_types::vm::{Firmware, PowerState, ScheduleError, Vm, VmPhase, VmSpec, VmStatus};
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
pub(super) async fn resume_vm(hv: &dyn Hypervisor, id: Uuid) -> Result<(), DriverError> {
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

/// 시간을 넘겨 실패한 블록 스냅샷 `snapshot`(백업의 임시 스냅샷이면 백업 id)이 디스크 `disks`에
/// 남겼을 수 있는 태그 `tag`를 뒤에서 지운다. 명령이 아직 돌 수 있으므로 `orphan_retry` 뒤부터 그
/// 간격으로 [`LEFT_TAG_TRIES`]번까지 해 본다. 그 id의 스냅샷 리소스가 있으면 그 스냅샷의 태그이므로
/// 그 차례는 건너뛴다. 그래도 남은 태그는 실행기가 다음에 시작할 때 지운다. 실행기가 멈추면 함께 멈춘다.
fn remove_left_tags(ctx: &Ctx, snapshot: Uuid, tag: &str, mut disks: Vec<(Volume, StoragePool)>) {
    let (cluster, storage) = (ctx.cluster.clone(), ctx.storage.clone());
    let interval = ctx.config.orphan_retry;
    let tag = tag.to_string();
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
        remove_left_tags(ctx, id, &id.to_string(), disks);
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

/// UEFI VM의 변수 파일을 가진 노드(`firmware_node`)가 이 노드가 아니면 거기서 받아 와 이 노드의
/// 파일을 바꾼다(설계 §5). 받지 못하면 경고만 남기고 이 노드의 파일(없으면 복사하지 않는다)을 쓴다.
/// 이 노드에서 QEMU가 돌고 있으면(그 QEMU가 이 노드의 파일을 쓴다) 받지 않는다.
pub(super) async fn fetch_vars(ctx: &Ctx, vm: &Vm) {
    let (Some(fetcher), Some(node)) = (&ctx.config.firmware, vm.status.firmware_node) else {
        return;
    };
    if vm.spec.firmware != Firmware::Uefi || node == ctx.config.node {
        return;
    }
    if !matches!(ctx.hv.inspect(vm.id).await, Ok(None)) {
        return;
    }
    if let Err(e) = fetcher.fetch(vm.id, node).await {
        tracing::warn!(vm = %vm.id, from = %node, error = %e, "cannot fetch UEFI variables; copying this node's copy");
    }
}

/// UEFI 변수 파일을 이 노드에 썼다: 이 노드를 `firmware_node`로 삼는다. reconciler도 status를
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn set_firmware_node(ctx: &Ctx, vm: Uuid) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        if current.status.firmware_node == Some(ctx.config.node) {
            return Ok(());
        }
        let status = VmStatus {
            firmware_node: Some(ctx.config.node),
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
pub(super) fn block_snapshots(
    disks: &[(Volume, StoragePool)],
) -> Result<Option<&'static str>, String> {
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
pub(super) async fn remove_tags(
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

pub(super) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `pending` 스냅샷을 채운다: 디스크에 태그를 찍고 UEFI 변수를 복사한 뒤([`take_tags`]) `ready`로
/// 바꾸고 VM의 현재 위치로 삼는다. 실패하면 리소스는 실행기의 정리가 지운다.
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
    let copy_vars = || async {
        let Some(dir) = ctx.config.state_dir.as_ref() else {
            return Ok(());
        };
        fetch_vars(ctx, &vm).await;
        let vars = uefi_vars_path(dir, vm_id);
        match tokio::fs::try_exists(&vars).await {
            // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM
            Ok(false) => Ok(()),
            Ok(true) => copy_file(&vars, &vars_copy(ctx, vm_id, id).unwrap()).await,
            Err(e) => Err(e),
        }
    };
    let resumed = match take_tags(ctx, &vm, &disks, id, &tag, spec.memory, copy_vars).await {
        Ok(resumed) => resumed,
        Err(e) => {
            if let Some(copy) = vars_copy(ctx, vm_id, id) {
                let _ = tokio::fs::remove_file(copy).await;
            }
            return Err(e);
        }
    };

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

/// VM `vm`의 디스크 `disks`에 스냅샷 태그 `tag`를 찍는다(스냅샷 만들기, 백업의 임시 스냅샷). 실행
/// 중이면 하이퍼바이저로(`memory`면 메모리 포함, 아니면 디스크만), 정지했으면 디스크마다 스토리지로
/// 찍는다(블록 스냅샷은 실행 중이면 VM을 잠시 멈추고 스토리지로). 스토리지로는 같은 풀의 디스크를 한
/// 번에 찍는다(ZFS는 원자적). 찍은 뒤 `copy_vars`로 UEFI 변수를 복사한다. 실패를 확인했으면 만든 태그를
/// 지운다. 응답을 기다리다 시간이 다 됐으면 작업이 아직 돌 수 있으므로 태그를 두고 실패한다(블록
/// 스냅샷의 태그는 뒤에서 다시 지운다, [`remove_left_tags`]; `id`는 스냅샷이나 백업의 id). 찍었으면
/// 블록 스냅샷을 위해 멈춘 VM을 이어서 실행한 결과를 돌려준다.
pub(super) async fn take_tags<F, Fut>(
    ctx: &Ctx,
    vm: &Vm,
    disks: &[(Volume, StoragePool)],
    id: Uuid,
    tag: &str,
    memory: bool,
    copy_vars: F,
) -> Result<Result<(), DriverError>, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = std::io::Result<()>>,
{
    let vm_id = vm.id;
    let running = vm.status.phase == VmPhase::Running;
    let block = block_snapshots(disks)?;
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
            for group in by_pool(disks) {
                let (volume, pool) = group[0];
                let ids: Vec<Uuid> = group.iter().map(|(v, _)| v.id).collect();
                ctx.storage
                    .snapshot_many(pool, &ids, volume.spec.format, tag)
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
        ctx.hv.snapshot_save(vm_id, tag, memory).await
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
    let copied = match &made {
        Ok(()) => copy_vars().await,
        Err(_) => Ok(()),
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
            remove_left_tags(ctx, id, tag, left);
            for (volume, pool) in taken {
                if let Err(e) = ctx
                    .storage
                    .snapshot_delete(pool, volume.id, volume.spec.format, tag)
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
                remove_left_tags(ctx, id, tag, disks.to_vec());
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
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, tag).await {
            tracing::warn!(snapshot = %id, error = %e, "cannot remove tags of failed snapshot");
        }
        return Err(match resumed {
            Err(e) => format!("{error}; resuming the VM failed: {e}"),
            Ok(()) => error,
        });
    }
    Ok(resumed)
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
    if let (Some(copy), Some(dir)) = (vars_copy(ctx, vm_id, id), &ctx.config.state_dir) {
        if tokio::fs::try_exists(&copy).await.unwrap_or(false) {
            copy_file(&copy, &uefi_vars_path(dir, vm_id))
                .await
                .map_err(|e| format!("cannot restore UEFI variables: {e}"))?;
            // 되돌린 변수는 이 노드에 있다. 다음 시작이 다른 노드에서 받아 덮지 않게 한다.
            set_firmware_node(ctx, vm_id).await?;
        } else if get_vm(ctx, vm_id)?.spec.firmware == Firmware::Uefi {
            // 스냅샷을 다른 노드에서 찍었거나 한 번도 켜지 않은 VM이다. 변수 복사본은 노드 사이에
            // 받아 오지 않으므로 지금 변수를 그대로 둔다.
            tracing::warn!(vm = %vm_id, snapshot = %id, "this node has no UEFI variables copy of the snapshot; keeping the current variables");
        }
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
/// 스냅샷이면 그 이름, 아니면 디스크의 태그)과 함께 오류다. UUID 태그인데 스냅샷 자원이 없으면
/// opld가 뒤에서 치우는 남은 스냅샷이라고 알린다.
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
        Ok(id) => match get_snapshot(ctx, id)? {
            Some(s) => format!("{:?}", s.spec.name),
            None => format!(
                "{last:?} left by an opLite snapshot that opld removes in the background (retry \
                 later or restart opld)"
            ),
        },
        Err(_) => format!("{last:?}"),
    };
    Err(format!(
        "volume {:?} has a more recent snapshot {newer}; zfs disks can only revert to the \
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
        // 예약과 HA는 스냅샷 때의 것이 아니라 지금 것을 지킨다.
        spec.snapshot_schedule = vm.spec.snapshot_schedule;
        spec.ha = vm.spec.ha;
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

`crates/opl-api/tests/backup_tasks.rs`를 만든다:

```rust
//! 백업 Task와 백업 지우기 Task: Mock 하이퍼바이저와 dir 풀로 꺼진 VM과 실행 중인 VM을 백업하고(압축
//! qcow2, UEFI 변수, 매니페스트), 실패하면 디렉터리를 지우고 `failed`로 남기며, 지우기는 파일과
//! 리소스를 지운다. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_tasks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Client, Env, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE, MANIFEST_VERSION,
};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn qemu_img(args: &[&str]) -> String {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn info(path: &Path) -> Value {
    serde_json::from_str(&qemu_img(&[
        "info",
        "-U",
        "--output=json",
        path.to_str().unwrap(),
    ]))
    .unwrap()
}

fn pool_dir(env: &Env, pool: &str) -> PathBuf {
    env.dir.path().join(pool)
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다.
async fn create_pool(env: &Env, name: &str, backups: bool) {
    std::fs::create_dir_all(pool_dir(env, name)).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env, name),
            },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 풀 `local`에 볼륨을 API로 만들고 파일 경로를 돌려준다.
async fn create_volume(env: &Env, admin: &Client, name: &str, size: u64, format: &str) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": name,
                "spec": { "pool": "local", "size_bytes": size, "format": format }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    succeeded(&admin.wait_task(&task).await);
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap();
    pool_dir(env, "local").join(format!("{}.{format}", volume.id))
}

async fn create_vm(admin: &Client, name: &str, disks: &[&str]) -> Uuid {
    let mut req = vm_request(name);
    req["spec"]["disks"] = disks
        .iter()
        .map(|v| json!({ "volume": v, "bus": "virtio-blk" }))
        .collect();
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    vm["id"].as_str().unwrap().parse().unwrap()
}

fn succeeded(task: &Value) {
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

async fn wait(admin: &Client, (status, task): (StatusCode, Value)) -> Value {
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await
}

fn backup(env: &Env, name: &str) -> Option<Backup> {
    env.cluster.get_by_name::<BackupSpec>(name).unwrap()
}

/// 백업 풀 `bk` 안의 백업 디렉터리
fn backup_dir(env: &Env, id: Uuid) -> PathBuf {
    opl_types::backup::backup_dir(&pool_dir(env, "bk"), id)
}

/// 두 디스크(qcow2, raw)에 표식을 쓴 VM `web`과 UEFI 변수 파일을 만든다.
async fn setup(env: &Env, admin: &Client) -> (Uuid, PathBuf, PathBuf) {
    create_pool(env, "local", false).await;
    create_pool(env, "bk", true).await;
    let root = create_volume(env, admin, "root", 4 * MIB, "qcow2").await;
    let data = create_volume(env, admin, "data", 2 * MIB, "raw").await;
    assert!(qemu_io(&root, "qcow2", "write -P 0xaa 0 64k"));
    assert!(qemu_io(&data, "raw", "write -P 0x5a 64k 64k"));
    let vm = create_vm(admin, "web", &["root", "data"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"uefi variables").unwrap();
    (vm, root, data)
}

#[tokio::test]
async fn stopped_vm_backup_writes_compressed_disks_vars_and_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (vm, _, _) = setup(&env, &admin).await;

    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let b = backup(&env, "b1").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    assert!(b.status.created_at.is_some());
    assert!(b.status.uefi_vars);
    assert_eq!(b.status.error, None);
    let names: Vec<(&str, &str, u64)> = b
        .status
        .disks
        .iter()
        .map(|d| (d.name.as_str(), d.file.as_str(), d.virtual_bytes))
        .collect();
    assert_eq!(
        names,
        [
            ("disk0", "disk0.qcow2", 4 * MIB),
            ("disk1", "disk1.qcow2", 2 * MIB)
        ]
    );

    let dir = backup_dir(&env, b.id);
    for disk in &b.status.disks {
        let path = dir.join(&disk.file);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), disk.stored_bytes);
        let info = info(&path);
        assert_eq!(info["format"], "qcow2");
        assert_eq!(info["virtual-size"], disk.virtual_bytes);
    }
    assert!(qemu_io(
        &dir.join("disk0.qcow2"),
        "qcow2",
        "read -P 0xaa 0 64k"
    ));
    assert!(qemu_io(
        &dir.join("disk1.qcow2"),
        "qcow2",
        "read -P 0x5a 64k 64k"
    ));
    assert_eq!(
        std::fs::read(dir.join("vars.fd")).unwrap(),
        b"uefi variables"
    );
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(manifest.version, MANIFEST_VERSION);
    assert_eq!(manifest.backup, b.id);
    assert_eq!((manifest.vm, manifest.vm_name.as_str()), (vm, "web"));
    assert_eq!(manifest.vm_spec, b.spec.vm_spec);
    assert_eq!(manifest.disks, b.status.disks);
    assert!(manifest.uefi_vars);
    assert_eq!(Some(manifest.created_at), b.status.created_at);
    // 공유가 아닌 풀이므로 파일을 쓴 이 노드를 적는다.
    assert_eq!(b.status.node, Some(env.node));
    // 다 쓴 파일만 남는다.
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        ["disk0.qcow2", "disk1.qcow2", "manifest.json", "vars.fd"]
    );

    // 지우기는 디렉터리와 리소스를 지운다.
    succeeded(&wait(&admin, admin.delete("/backups/b1").await).await);
    assert!(backup(&env, "b1").is_none());
    assert!(!dir.exists());
}

/// 실행 중인 VM은 디스크에 임시 스냅샷(백업 id의 simple 문자열, 메모리 없음)을 찍어 그때의 내용을
/// 담고, 끝나면 그 스냅샷을 지운다. Mock 하이퍼바이저는 파일에 쓰지 않으므로 QEMU가 만들었을 태그를
/// 대신 만들고, 그 뒤 게스트가 쓴 것처럼 디스크를 바꾼다.
#[tokio::test]
async fn running_vm_backup_reads_the_temporary_snapshot() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    let root = create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    assert!(qemu_io(&root, "qcow2", "write -P 0xaa 0 64k"));
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let vars = uefi_vars_path(&env.dir.path().join("state"), vm);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"at snapshot").unwrap();

    env.stop_runner().await;
    let (status, task) = admin
        .post("/vms/web/backups", json!({ "pool": "bk", "name": "live" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = backup(&env, "live").unwrap().id;
    let tag = id.simple().to_string();
    qemu_img(&["snapshot", "-c", &tag, root.to_str().unwrap()]);
    assert!(qemu_io(&root, "qcow2", "write -P 0xbb 0 64k"));
    env.start_runner().await;
    succeeded(&admin.wait_task(&task).await);

    let b = backup(&env, "live").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready, "{b:?}");
    let dir = backup_dir(&env, id);
    let disk = dir.join("disk0.qcow2");
    assert!(qemu_io(&disk, "qcow2", "read -P 0xaa 0 64k"));
    assert!(info(&disk).get("snapshots").is_none());
    assert_eq!(std::fs::read(dir.join("vars.fd")).unwrap(), b"at snapshot");
    // 임시 스냅샷을 하이퍼바이저로 찍고 지웠다.
    assert!(
        env.hv.snapshots(vm).is_empty(),
        "{:?}",
        env.hv.snapshots(vm)
    );
    // VM은 계속 돈다.
    let vm = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(vm.status.phase, VmPhase::Running);
}

/// 디스크를 읽지 못하면(여기서는 실행 중인 VM의 임시 스냅샷이 디스크에 없다) 백업 디렉터리를 지우고
/// 오류와 함께 `failed`로 남긴다. 실패한 백업도 지울 수 있다.
#[tokio::test]
async fn failed_backup_removes_its_directory_and_records_the_error() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    let vm = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "bad" }))
            .await,
    )
    .await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot back up volume \"root\""), "{error}");
    let b = backup(&env, "bad").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
    assert_eq!(b.status.error.as_deref(), Some(error));
    assert!(!backup_dir(&env, b.id).exists());
    // 디렉터리를 만들기 전에 이 노드를 적었다. 지우기도 이 노드에서 한다.
    assert_eq!(b.status.node, Some(env.node));
    // 임시 스냅샷은 실패해도 지운다.
    assert!(env.hv.snapshots(vm).is_empty());

    succeeded(&wait(&admin, admin.delete("/backups/bad").await).await);
    assert!(backup(&env, "bad").is_none());
}

/// 온라인인 다른 노드를 저장소에 만든다. 그 노드의 실행기는 없다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: opl_types::node::Node = Resource::new(
        name,
        opl_types::node::NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// Task를 저장소에 직접 만든다(API의 검사 없이). `state`가 `Running`이면 이미 돌고 있는 것처럼 둔다.
async fn put_task(
    env: &Env,
    op: opl_types::task::TaskOp,
    target: Uuid,
    node: Uuid,
    state: TaskState,
) -> Value {
    let mut task: opl_types::task::Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    if state != TaskState::Pending {
        let status = TaskStatus { state, error: None };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    json!({ "id": task.id })
}

/// 지우기 Task는 같은 백업을 되살리는 Task가 (다른 노드에서) 끝나지 않았으면 파일을 지우지 않고
/// 실패한다.
#[tokio::test]
async fn delete_waits_for_restores_of_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let task = wait(
        &admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let b = backup(&env, "b1").unwrap();
    let n2 = other_node(&env, "n2").await;
    let restore = opl_types::task::TaskOp::BackupRestore {
        backup: b.id,
        name: "web-2".into(),
        pool: "local".into(),
    };
    put_task(&env, restore, Uuid::now_v7(), n2, TaskState::Running).await;

    let delete = opl_types::task::TaskOp::BackupDelete;
    let task = put_task(&env, delete, b.id, env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "backup is being restored");
    assert!(backup(&env, "b1").is_some());
    assert!(backup_dir(&env, b.id).join(MANIFEST_FILE).exists());
}

/// 공유가 아닌 풀에서 다른 노드가 쓴 백업의 파일은 이 노드에 없는 것이 당연하므로, 지우기가 이 노드에서
/// 돌면 이미 지운 것으로 보지 않고 실패한다. 풀이 없어진 백업은 리소스만 지운다.
#[tokio::test]
async fn delete_does_not_forget_files_on_another_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    create_vm(&admin, "web", &["root"]).await;
    for name in ["b1", "b2"] {
        let task = wait(
            &admin,
            admin
                .post("/vms/web/backups", json!({ "pool": "bk", "name": name }))
                .await,
        )
        .await;
        succeeded(&task);
    }
    let b1 = backup(&env, "b1").unwrap();
    let n2 = other_node(&env, "n2").await;
    let status = opl_types::backup::BackupStatus {
        node: Some(n2),
        ..b1.status.clone()
    };
    env.cluster
        .update_status::<BackupSpec>(b1.id, &status)
        .await
        .unwrap();
    std::fs::remove_dir_all(backup_dir(&env, b1.id)).unwrap();

    let delete = opl_types::task::TaskOp::BackupDelete;
    let task = put_task(&env, delete, b1.id, env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains(&format!("on node {n2}")), "{error}");
    assert!(backup(&env, "b1").is_some());

    // 풀이 없어지면 파일도 없다. 리소스만 지운다.
    env.cluster.delete::<BackupSpec>(b1.id, None).await.unwrap();
    let b2 = backup(&env, "b2").unwrap();
    let pool = env
        .cluster
        .get_by_name::<StoragePoolSpec>("bk")
        .unwrap()
        .unwrap();
    env.cluster
        .delete::<StoragePoolSpec>(pool.id, None)
        .await
        .unwrap();
    succeeded(&wait(&admin, admin.delete("/backups/b2").await).await);
    assert!(backup(&env, "b2").is_none());
    assert!(backup_dir(&env, b2.id).exists());
}

/// opld가 백업 도중 내려갔으면 다음 시작에 그 Task를 실패로 끝내고, 백업은 반쯤 쓴 디렉터리를 지우고
/// `failed`로 남긴다.
#[tokio::test]
async fn backup_interrupted_by_a_restart_is_failed_and_cleaned_up() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env, "local", false).await;
    create_pool(&env, "bk", true).await;
    create_volume(&env, &admin, "root", 4 * MIB, "qcow2").await;
    create_vm(&admin, "web", &["root"]).await;

    env.stop_runner().await;
    let (status, task) = admin
        .post("/vms/web/backups", json!({ "pool": "bk", "name": "cut" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    env.cluster
        .update_status::<TaskSpec>(task_id, &running)
        .await
        .unwrap();
    let id = backup(&env, "cut").unwrap().id;
    let dir = backup_dir(&env, id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("disk0.qcow2.part"), b"half").unwrap();
    env.start_runner().await;

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let b = backup(&env, "cut").unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
    assert!(
        b.status
            .error
            .as_deref()
            .unwrap()
            .starts_with("interrupted"),
        "{b:?}"
    );
    assert!(!dir.exists());
}
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
use opl_qemu::QmpError;
use opl_types::node::NodeSpec;
```

을 다음으로 바꾼다:

```rust
use opl_qemu::QmpError;
use opl_types::backup::{Backup, BackupPhase, BackupSpec};
use opl_types::node::NodeSpec;
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

/// 공유 블록 풀(RBD)의 볼륨은 어느 노드에서나 보이므로, 시작할 때 남은 태그는 볼륨을 만든 노드가
```

을 다음으로 바꾼다:

```rust

/// 백업의 임시 스냅샷 태그(백업 id의 simple 문자열)도 UUID 모양이다. 시작할 때 백업 Task가 끝나지
/// 않은 `pending` 백업의 태그는 두고, Task가 없는 `pending` 백업, 다 만들었거나 실패한 백업과 없는
/// 백업의 태그는 지운다.
#[tokio::test]
async fn block_tags_of_pending_backups_are_kept_at_startup() {
    let mut env = Env::start().await;
    let (vols, storage) = block_volumes(&mut env, &[("thin", lvm("vg0"), &["lv0"])]).await;
    let spec: VmSpec = serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
    // 다른 노드에서 돌고 있는 백업 Task(이 노드의 실행기는 손대지 않는다)
    let mut n2: opl_types::node::Node = Resource::new(
        "n2",
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    n2.status.online = true;
    env.cluster.create(&n2).await.unwrap();
    let mut tags = Vec::new();
    for (name, phase) in [
        ("running", BackupPhase::Pending),
        ("done", BackupPhase::Ready),
        ("broken", BackupPhase::Failed),
        ("abandoned", BackupPhase::Pending),
    ] {
        let mut backup: Backup = Resource::new(
            name,
            BackupSpec {
                vm: Uuid::now_v7(),
                vm_name: "web".into(),
                pool: "thin".into(),
                vm_spec: spec.clone(),
                job: None,
            },
        );
        env.cluster.create(&backup).await.unwrap();
        backup.status.phase = phase;
        env.cluster
            .update_status::<BackupSpec>(backup.id, &backup.status)
            .await
            .unwrap();
        tags.push(backup.id.simple().to_string());
        if name == "running" {
            let op = TaskOp::VmBackup { backup: backup.id };
            let mut task: Task = Resource::new(
                "",
                TaskSpec {
                    op,
                    target: backup.spec.vm,
                    node: n2.id,
                },
            );
            task.name = task.id.to_string();
            env.cluster.create(&task).await.unwrap();
            let running = TaskStatus {
                state: TaskState::Running,
                error: None,
            };
            env.cluster
                .update_status::<TaskSpec>(task.id, &running)
                .await
                .unwrap();
        }
    }
    tags.push(Uuid::now_v7().simple().to_string());
    let tags: Vec<&str> = tags.iter().map(String::as_str).collect();
    storage.set_tags(vols[0], &tags);

    env.stop_runner().await;
    env.start_runner_with(storage.clone()).await;
    let deadline = std::time::Instant::now() + common::WAIT;
    while storage.tags(vols[0]).len() != 1 {
        assert!(std::time::Instant::now() < deadline, "tags were kept");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(storage.tags(vols[0]), [tags[0]]);
}

/// 실행 중인 VM의 백업은 블록 스냅샷을 위해 VM을 잠깐 멈춘다. 이어서 실행하지 못하면 한 번 더 해 보고,
/// 그래도 안 되면 오래 걸리는 내보내기를 하지 않고 실패한다: 임시 태그를 지우고 백업은 `failed`다.
#[tokio::test]
async fn backup_fails_before_exporting_when_the_paused_vm_cannot_resume() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (lvs, storage) = lvm_volumes(&mut env, &["lv0"]).await;
    let vm = running_block_vm(&env, &admin, "web", &["lv0"]).await;
    let dir = env.dir.path().join("bk");
    std::fs::create_dir_all(&dir).unwrap();
    let bk = StoragePoolSpec {
        backend: PoolBackend::Dir { path: dir },
        nodes: vec![],
        shared: false,
        backups: true,
    };
    env.cluster.create(&Resource::new("bk", bk)).await.unwrap();
    env.pool_ready("bk").await;

    storage.pause();
    let (status, task) = admin
        .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    storage.reached().await;
    // 멈춘 동안 QMP로 다룰 수 없게 되어 다시 돌리지 못한다.
    env.hv.set_unresponsive(vm, true);
    storage.resume();
    let task = admin.wait_task(&task).await;
    env.hv.set_unresponsive(vm, false);
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("cannot resume the VM after its temporary snapshot"),
        "{error}"
    );
    assert!(error.contains("retrying failed"), "{error}");
    // 찍은 임시 태그만 지웠고 디스크는 읽지 않았다.
    assert_eq!(storage.calls(), [("create", lvs[0]), ("delete", lvs[0])]);
    let b = env
        .cluster
        .get_by_name::<BackupSpec>("b1")
        .unwrap()
        .unwrap();
    assert_eq!(b.status.phase, BackupPhase::Failed);
}

/// 공유 블록 풀(RBD)의 볼륨은 어느 노드에서나 보이므로, 시작할 때 남은 태그는 볼륨을 만든 노드가
```

`crates/opl-driver/src/storage.rs`에서

```rust
    ) -> Result<(), DriverError>;
}
```

을 다음으로 바꾼다:

```rust
    ) -> Result<(), DriverError>;

    /// 원본 `src`(`volume_source`가 돌려준 것)를 압축 qcow2 파일 `dest`로 내보낸다(백업). 원본을 여는
    /// 인자는 복사와 같다(`force_share`도). `{dest}.part`에 쓰고 다 쓰면 `dest`로 이름을 바꾼다(있으면
    /// 덮어쓴다). 실패하면 `.part`를 남기지 않는다. `dest`는 절대 경로여야 한다. 기본은 지원하지 않는다.
    async fn export_volume(
        &self,
        _src: &VolumeSource,
        _force_share: bool,
        _dest: &Path,
    ) -> Result<(), DriverError> {
        Err(DriverError::InvalidSpec(
            "this storage cannot export volumes".into(),
        ))
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    Ok(args)
}
```

을 다음으로 바꾼다:

```rust
    Ok(args)
}

/// [`Storage::export_volume`]: `qemu-img convert -c -O qcow2`로 `{dest}.part`에 쓰고(`-t writeback`:
/// 닫을 때 디스크에 내려 쓴다) `dest`로 이름을 바꾼다. 파일시스템 호출은 [`on_pool`]로 한다(백업 풀이
/// 멈춘 NFS일 수 있다).
async fn export_to(
    qemu_img: &Path,
    src: &VolumeSource,
    force_share: bool,
    dest: &Path,
) -> Result<(), DriverError> {
    require_absolute(dest)?;
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let from = convert_from(src, force_share, Some(ImageFormat::Qcow2))?;
    let mut args = vec!["convert", "-q", "-c", "-t", "writeback"];
    args.extend(from.iter().map(String::as_str));
    args.push(path_str(&part)?);
    let dir = dest.parent().unwrap_or(dest);
    let written = match run_qemu_img(qemu_img, &args).await {
        Ok(_) => {
            let (from, to) = (part.clone(), dest.to_path_buf());
            on_pool(FS_TIMEOUT, dir, move || std::fs::rename(from, to))
                .await
                .and_then(|r| Ok(r?))
        }
        Err(e) => Err(e),
    };
    if written.is_err() {
        let _ = on_pool(FS_TIMEOUT, dir, move || std::fs::remove_file(part)).await;
    }
    written
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    ) -> Result<(), DriverError> {
        Ok(())
    }
}
```

을 다음으로 바꾼다:

```rust
    ) -> Result<(), DriverError> {
        Ok(())
    }

    async fn export_volume(
        &self,
        src: &VolumeSource,
        force_share: bool,
        dest: &Path,
    ) -> Result<(), DriverError> {
        export_to(&self.qemu_img, src, force_share, dest).await
    }
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 내보내기는 원본을 복사와 같은 인자로 열어 압축 qcow2(`-c -O qcow2`)로 `.part`에 쓰고 이름을
    /// 바꾼다. 실패하면 `.part`를 지운다.
    #[tokio::test]
    async fn export_volume_writes_compressed_qcow2_through_a_part_file() {
        let tools = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let args = tools.path().join("args");
        let storage = dir_storage(fake_tool(
            tools.path(),
            &format!("printf '%s\\n' \"$@\" > {args:?}\necho data > \"$last\""),
        ));
        let src = VolumeSource {
            path: "/pools/local/v.qcow2".into(),
            format: ImageFormat::Qcow2,
            snapshot: Some("a,b".into()),
            size_bytes: 5,
            rbd: None,
        };
        let dest = out.path().join("disk0.qcow2");
        storage.export_volume(&src, true, &dest).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"data\n");
        assert_eq!(pool_files(out.path()), vec!["disk0.qcow2".to_string()]);
        let got = std::fs::read_to_string(&args).unwrap();
        let part = format!("{}.part", dest.to_str().unwrap());
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-c",
                "-t",
                "writeback",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "qcow2",
                "/pools/local/v.qcow2",
                part.as_str(),
            ]
        );

        // 지금 내용(raw), 잠금을 지킨다.
        let raw = VolumeSource {
            path: "/pools/local/w.raw".into(),
            format: ImageFormat::Raw,
            snapshot: None,
            size_bytes: 5,
            rbd: None,
        };
        let dest1 = out.path().join("disk1.qcow2");
        storage.export_volume(&raw, false, &dest1).await.unwrap();
        let got = std::fs::read_to_string(&args).unwrap();
        let part1 = format!("{}.part", dest1.to_str().unwrap());
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-c",
                "-t",
                "writeback",
                "-f",
                "raw",
                "-O",
                "qcow2",
                "/pools/local/w.raw",
                part1.as_str(),
            ]
        );

        // 반쯤 쓰고 실패하면 `.part`를 남기지 않는다.
        let failing = dir_storage(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho full >&2\nexit 1",
        ));
        let dest2 = out.path().join("disk2.qcow2");
        let err = failing
            .export_volume(&raw, false, &dest2)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            pool_files(out.path()),
            vec!["disk0.qcow2".to_string(), "disk1.qcow2".to_string()]
        );

        // 상대 경로는 받지 않는다.
        let err = storage
            .export_volume(&raw, false, Path::new("backups/disk0.qcow2"))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, convert_from, path_str, rbd, run_qemu_img,
};
```

을 다음으로 바꾼다:

```rust
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, convert_from, export_to, path_str, rbd,
    run_qemu_img,
};
```

`crates/opl-driver/src/storage/storages.rs`에서

```rust
        written
    }
```

을 다음으로 바꾼다:

```rust
        written
    }

    /// 원본의 백엔드와 상관없이 `qemu-img`로 내보낸다.
    async fn export_volume(
        &self,
        src: &VolumeSource,
        force_share: bool,
        dest: &Path,
    ) -> Result<(), DriverError> {
        export_to(&self.qemu_img, src, force_share, dest).await
    }
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
    ));
}
```

을 다음으로 바꾼다:

```rust
    ));
}

/// `qemu-img info`의 JSON
fn image_info(path: &Path) -> serde_json::Value {
    let out = std::process::Command::new("qemu-img")
        .args(["info", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 백업 내보내기: qcow2 볼륨의 스냅샷(실행 중인 VM처럼 `-U`)과 raw 볼륨의 지금 내용을 압축 qcow2
/// 파일로 쓴다. 내보낸 파일은 스냅샷 없는 qcow2이고 원본의 크기와 내용을 갖는다.
#[tokio::test]
async fn export_volume_writes_compressed_qcow2_of_snapshot_or_current_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, out) = (tmp.path().join("src"), tmp.path().join("out"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&out).unwrap();
    let src_pool = pool(&src_dir);
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)));
    let (qcow2, raw) = (ImageFormat::Qcow2, ImageFormat::Raw);

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 1M"));
    let tag = Uuid::now_v7().simple().to_string();
    storages
        .snapshot_create(&src_pool, src, qcow2, &tag)
        .await
        .unwrap();
    assert_eq!(
        storages.snapshot_tags(&src_pool, src, qcow2).await.unwrap(),
        vec![tag.clone()]
    );
    assert!(qemu_io(&src_path, "write -P 0xcd 0 1M"));

    let source = storages
        .volume_source(&src_pool, src, qcow2, Some(&tag))
        .await
        .unwrap();
    let dest = out.join("disk0.qcow2");
    storages.export_volume(&source, true, &dest).await.unwrap();
    storages
        .release_source(&src_pool, src, qcow2, Some(&tag))
        .await
        .unwrap();
    assert!(qemu_io(&dest, "read -P 0xab 0 1M"));
    let info = image_info(&dest);
    assert_eq!(info["format"], "qcow2");
    assert_eq!(info["virtual-size"], 64 * MIB);
    assert!(info.get("snapshots").is_none(), "{info}");
    // 압축했으므로 같은 바이트로 채운 1 MiB보다 작다.
    assert!(std::fs::metadata(&dest).unwrap().len() < MIB);
    // 원본의 태그는 그대로다.
    assert_eq!(
        storages.snapshot_tags(&src_pool, src, qcow2).await.unwrap(),
        vec![tag.clone()]
    );

    let src_raw = Uuid::now_v7();
    let raw_path = storages
        .create_volume(&src_pool, src_raw, 32 * MIB, raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(&raw_path, raw, "write -P 0x5a 0 64k"));
    let source = storages
        .volume_source(&src_pool, src_raw, raw, None)
        .await
        .unwrap();
    let dest1 = out.join("disk1.qcow2");
    storages
        .export_volume(&source, false, &dest1)
        .await
        .unwrap();
    assert!(qemu_io(&dest1, "read -P 0x5a 0 64k"));
    assert_eq!(image_info(&dest1)["virtual-size"], 32 * MIB);

    // 없는 태그면 실패하고 `.part`를 남기지 않는다.
    let gone = storages
        .volume_source(&src_pool, src, qcow2, Some("gone"))
        .await
        .unwrap();
    let err = storages
        .export_volume(&gone, true, &out.join("disk2.qcow2"))
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["disk0.qcow2", "disk1.qcow2"]);
}
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.state_dir = Some(config.data_dir.clone());
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
```

을 다음으로 바꾼다:

```rust
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 971개, 실패 0

Run: `cargo test -p opl-driver -p opl-api --features opl-driver/storage-it,opl-api/storage-it`
Expected: PASS (qemu-img가 필요)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat: 백업 Task — VM 백업(압축 qcow2·UEFI 변수·매니페스트)과 백업 지우기, driver export_volume"
```

---

### Task 4: 복원 Task

`BackupRestore`(백업 id 큐, 시간 제한 없음): 백업이 ready이고 이 노드가 파일이 있는 노드인지, 매니페스트 버전·id·`vm_spec`·디스크 수가 클러스터 기록과 같은지, 디스크·변수 파일이 경로 없는 일반 파일이고 backing file이 없는지 확인한 뒤, 디스크마다 `{새 이름}-disk{i}` 볼륨을 대상 풀에(블록 풀이면 raw, 아니면 qcow2) 가져오고, UEFI 변수를 복사하고, 기록의 `vm_spec`으로 꺼진 새 VM을 만든다(새 MAC, 노드·스냅샷 일정 없음, HA 끔, `validate` 통과). 실패하면 복제의 정리(`discard`)로 볼륨을 지운다. 복제의 마지막 단계는 `clone::create_vm`으로 함수화.

**Files:**
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/src/tasks/clone.rs`
- Modify: `crates/opl-api/tests/backup_tasks.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업은 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다(설계 §2.3).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
    );
```

을 다음으로 바꾼다:

```rust
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업과 복원은 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다(설계 §2.3).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. } | TaskOp::BackupRestore { .. }
    );
```

`crates/opl-api/src/tasks.rs`에서

```rust

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제의 흔적을 지워 이름을 다시 쓸 수 있게 한다. 실패한
/// 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다.
```

을 다음으로 바꾼다:

```rust

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제·복원의 흔적을 지워 이름을 다시 쓸 수 있게 한다. 실패한
/// 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다.
```

`crates/opl-api/src/tasks.rs`에서

```rust
    }
    if let TaskOp::VmClone { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
```

을 다음으로 바꾼다:

```rust
    }
    if let TaskOp::VmClone { .. } | TaskOp::BackupRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        // 복원 실행은 아직 없다.
        TaskOp::BackupRestore { .. } => {
            Err("restoring backups is not supported by this node yet".into())
        }
```

을 다음으로 바꾼다:

```rust
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        TaskOp::BackupRestore { backup, name, pool } => {
            backup::restore(ctx, task, *backup, name, pool).await
        }
```

`crates/opl-api/src/tasks/backup.rs` 전체:

```rust
//! 백업 Task(설계 §2.3), 백업 지우기 Task, 복원 Task(설계 §2.4). 백업은 VM의 줄에 서서 그 VM의
//! 스냅샷·복제 Task와 차례로 돌고, 지우기와 복원은 백업의 줄에 선다. API가 확인한 뒤 줄에서 기다리는
//! 사이 VM이나 풀이 바뀌었을 수 있으므로 다시 확인한다.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{check_pool_root, uefi_vars_path};
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE,
    MANIFEST_VERSION, backup_dir,
};
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeSpec, pool_error_on, pool_listed_on,
    pool_usable_on,
};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::clone::{copy_format, create_vm, made, volume_name};
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_vm, is_down, now_secs, ready_disks,
    remove_tags, take_tags,
};
use super::{Ctx, mark_ready};

/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(super) const VARS_FILE: &str = "vars.fd";

fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
}

/// 실행 중인 VM의 디스크에 찍는 임시 스냅샷의 태그: 백업 id의 simple 문자열. 스냅샷 태그(하이픈 있는
/// id)와 구분되고, 남으면 태그 청소가 백업 리소스를 보고 지운다.
fn temp_tag(backup: Uuid) -> String {
    backup.simple().to_string()
}

/// 이 노드에서 준비된 풀 `name`. 준비되지 않았으면(마운트되지 않은 NFS 풀의 빈 마운트 지점에 쓰지
/// 않는다) 오류다.
fn ready_pool(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))?;
    if !pool_listed_on(&pool, ctx.config.node) {
        return Err(format!(
            "storage pool {name:?} is not available on this node"
        ));
    }
    let me = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| e.to_string())?
        .ok_or("this node is not registered")?;
    if !pool_usable_on(&pool, &me) {
        return Err(format!(
            "storage pool {name:?} is not ready on this node: {}",
            pool_error_on(&pool, &me)
        ));
    }
    Ok(pool)
}

/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면 오류다.
fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
    let pool = ready_pool(ctx, name)?;
    let root = check_pool_root(&pool, &ctx.config.mount_root)
        .map_err(|e| format!("storage pool {name:?}: {e}"))?;
    Ok((pool, root))
}

/// VM의 UEFI 변수 파일(있으면)을 `to`로 복사한다. 변수 파일이 다른 노드에 있으면 받아 온다.
/// BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 복사할 것이 없다.
async fn copy_vars(ctx: &Ctx, vm: &Vm, to: &Path) -> std::io::Result<()> {
    let Some(dir) = &ctx.config.state_dir else {
        return Ok(());
    };
    fetch_vars(ctx, vm).await;
    let from = uefi_vars_path(dir, vm.id);
    if !tokio::fs::try_exists(&from).await? {
        return Ok(());
    }
    copy_file(&from, to).await
}

/// 디스크마다 `tag`(없으면 지금) 때의 내용을 `{dir}/disk{i}.qcow2`로 내보낸다. `force_share`는 실행
/// 중인 VM이 쥔 디스크의 스냅샷을 읽을 때 쓴다(스냅샷 데이터는 바뀌지 않는다).
async fn export_disks(
    ctx: &Ctx,
    disks: &[(Volume, StoragePool)],
    tag: Option<&str>,
    force_share: bool,
    dir: &Path,
) -> Result<Vec<BackupDisk>, String> {
    let mut out = Vec::with_capacity(disks.len());
    for (i, (volume, pool)) in disks.iter().enumerate() {
        let name = format!("disk{i}");
        let file = format!("{name}.qcow2");
        let dest = dir.join(&file);
        let format = volume.spec.format;
        let source = ctx
            .storage
            .volume_source(pool, volume.id, format, tag)
            .await
            .map_err(|e| format!("cannot read volume {:?}: {e}", volume.name))?;
        let exported = ctx.storage.export_volume(&source, force_share, &dest).await;
        // 내보내기는 끝났다. 내놓지 못하면 켜진 채 남을 뿐이다(opld가 뒤에 정리한다).
        if let Err(e) = ctx
            .storage
            .release_source(pool, volume.id, format, tag)
            .await
        {
            tracing::warn!(volume = %volume.name, error = %e, "cannot release backup source");
        }
        exported.map_err(|e| format!("cannot back up volume {:?}: {e}", volume.name))?;
        let stored_bytes = tokio::fs::metadata(&dest)
            .await
            .map_err(|e| format!("cannot read size of {file}: {e}"))?
            .len();
        out.push(BackupDisk {
            name,
            file,
            virtual_bytes: source.size_bytes,
            stored_bytes,
        });
    }
    Ok(out)
}

/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

/// 매니페스트를 `{dir}/manifest.json.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고, 디렉터리도 내려
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let text = serde_json::to_vec_pretty(manifest).map_err(std::io::Error::other)?;
    let path = dir.join(MANIFEST_FILE);
    let part = dir.join(format!("{MANIFEST_FILE}.part"));
    let mut file = tokio::fs::File::create(&part).await?;
    file.write_all(&text).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&part, &path).await?;
    sync_path(dir).await
}

/// `pending` 백업 `id`를 채운다(설계 §2.3): 백업 풀의 `{루트}/backups/{id}/`에 디스크마다 압축 qcow2,
/// UEFI 변수, 매니페스트를 쓰고 `ready`로 바꾼다. 실행 중인 VM은 디스크에 메모리 없는 임시 스냅샷을
/// 찍어 그것을 읽고 끝나면 지운다(스냅샷 리소스는 만들지 않는다). 꺼진 VM은 지금 디스크를 읽는다.
/// 실패하면 실행기의 정리([`discard`])가 디렉터리를 지우고 `failed`로 남긴다.
pub(super) async fn backup(ctx: &Ctx, task: &Task, id: Uuid) -> Result<(), String> {
    use super::snapshot::resume_vm;
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Pending {
        return Err("backup is not pending".into());
    }
    let vm = get_vm(ctx, task.spec.target)?;
    check_settled(&vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    if !running && !stopped {
        return Err("VM is being started; try again when it has settled".into());
    }
    let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
    if !pool.spec.backups {
        return Err(format!(
            "storage pool {:?} does not store backups",
            pool.name
        ));
    }
    let disks = ready_disks(ctx, &vm.spec)?;
    for (volume, pool) in &disks {
        check_local(ctx, volume, pool)?;
    }
    // 공유가 아닌 풀의 파일은 이 노드에만 생긴다. 쓰기 전에 적어 두어 실패한 백업도 이 노드에서 지운다.
    let node = (!opl_types::storage::is_shared(&pool)).then_some(ctx.config.node);
    if running
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !ctx.storage.supports_snapshots(p, v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted; stop the VM to back it up",
            volume.name, volume.spec.format
        ));
    }

    // 담는 VM 스펙은 실행하는 지금의 것이다(API가 확인한 뒤 VM 스펙이 바뀌었을 수 있다).
    if backup.spec.vm_spec != vm.spec || backup.spec.vm_name != vm.name {
        let spec = BackupSpec {
            vm_name: vm.name.clone(),
            vm_spec: vm.spec.clone(),
            ..backup.spec.clone()
        };
        ctx.cluster
            .update_spec(id, Some(backup.revision), backup.labels.clone(), &spec)
            .await
            .map_err(|e| e.to_string())?;
    }

    if node.is_some() {
        let status = BackupStatus {
            node,
            ..backup.status.clone()
        };
        ctx.cluster
            .update_status::<BackupSpec>(id, &status)
            .await
            .map_err(|e| e.to_string())?;
    }
    let dir = backup_dir(&root, id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("cannot create backup directory: {e}"))?;
    // 새 디렉터리 이름이 남게 `backups`와(처음 만들었을 수 있으니) 풀 루트도 내려 쓴다.
    for parent in [root.join("backups"), root.clone()] {
        sync_path(&parent)
            .await
            .map_err(|e| format!("cannot create backup directory: {e}"))?;
    }
    let vars = dir.join(VARS_FILE);
    let tag = temp_tag(id);
    // 실행 중이면 디스크를 임시 스냅샷으로 얼리고 그때의 UEFI 변수를 복사한다.
    let tagged = running && !disks.is_empty();
    let resumed = if tagged {
        take_tags(ctx, &vm, &disks, id, &tag, false, || {
            copy_vars(ctx, &vm, &vars)
        })
        .await?
    } else {
        copy_vars(ctx, &vm, &vars)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        Ok(())
    };
    // 블록 스냅샷을 위해 멈춘 VM을 이어서 실행하지 못했으면 한 번 더 해 본다. 그래도 멈춰 있으면 디스크를
    // 읽지 않고 실패한다(임시 스냅샷은 아래에서 지운다). 오래 걸리는 내보내기 동안 VM을 멈춰 두지 않는다.
    let resumed = match resumed {
        Err(e) => {
            tracing::warn!(vm = %vm.id, error = %e, "cannot resume VM after the temporary snapshot; retrying");
            resume_vm(&*ctx.hv, vm.id).await.map_err(|r| {
                format!(
                    "cannot resume the VM after its temporary snapshot: {e}; retrying failed: {r}"
                )
            })
        }
        Ok(()) => Ok(()),
    };
    let created_at = now_secs();
    let exported = match &resumed {
        Ok(()) => export_disks(ctx, &disks, tagged.then_some(&*tag), tagged, &dir).await,
        Err(e) => Err(e.clone()),
    };
    // 임시 스냅샷은 결과와 상관없이 지운다. 남으면 공간만 쓴다(블록 디스크는 태그 청소가 지운다).
    if tagged {
        let running = !matches!(ctx.hv.inspect(vm.id).await, Ok(None));
        if let Err(e) = remove_tags(ctx, vm.id, running, &vm.spec, &tag).await {
            tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
        }
    }
    let disks = exported?;
    let uefi_vars = tokio::fs::try_exists(&vars).await.unwrap_or(false);
    if uefi_vars {
        sync_path(&vars)
            .await
            .map_err(|e| format!("cannot write UEFI variables: {e}"))?;
    }
    let manifest = BackupManifest {
        version: MANIFEST_VERSION,
        backup: id,
        vm: vm.id,
        vm_name: vm.name.clone(),
        vm_spec: vm.spec.clone(),
        disks: disks.clone(),
        uefi_vars,
        created_at,
    };
    write_manifest(&dir, &manifest)
        .await
        .map_err(|e| format!("cannot write manifest: {e}"))?;

    let status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(created_at),
        disks,
        uefi_vars,
        error: None,
        node,
    };
    // 다 만든 백업이다. 실패로 끝내면 정리하면서 지우게 되므로 기록될 때까지 다시 시도한다.
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) => break,
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup ready; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
    Ok(())
}

/// 백업 디렉터리를 지운다. 없으면 이미 지운 것이다.
async fn remove_dir(dir: &Path) -> Result<(), String> {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove backup directory: {e}")),
    }
}

/// 실패한 백업 Task의 흔적을 지운다: 아직 `pending`인 백업이면 디렉터리를 지우고 `failed`와 `error`를
/// 남긴다(리소스는 사용자가 보고 지운다). `unfinished`면(opld가 재시작해 중단) 실행 중인 VM의 디스크에
/// 남았을 수 있는 임시 스냅샷도 지운다(블록 디스크의 것은 태그 청소도 지운다).
pub(super) async fn discard(ctx: &Ctx, task: &Task, id: Uuid, error: &str, unfinished: bool) {
    let backup = match get_backup(ctx, id) {
        Ok(Some(b)) if b.status.phase == BackupPhase::Pending => b,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(backup = %id, error = %e, "cannot read backup of failed task");
            return;
        }
    };
    if unfinished
        && let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(task.spec.target)
        && matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_)))
        && let Err(e) = remove_tags(ctx, vm.id, true, &vm.spec, &temp_tag(id)).await
    {
        tracing::warn!(backup = %id, error = %e, "cannot remove temporary snapshot of backup");
    }
    match backup_root(ctx, &backup.spec.pool) {
        Ok((_, root)) => {
            if let Err(e) = remove_dir(&backup_dir(&root, id)).await {
                tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup");
            }
        }
        Err(e) => tracing::warn!(backup = %id, error = %e, "cannot clean up failed backup"),
    }
    let status = BackupStatus {
        phase: BackupPhase::Failed,
        error: Some(error.to_string()),
        ..backup.status
    };
    loop {
        match ctx.cluster.update_status::<BackupSpec>(id, &status).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot mark backup failed; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 백업 파일(디렉터리)과 리소스를 지운다. 디렉터리가 이미 없어도 성공이므로 중간에 멈춰도 다시 하면 된다.
/// 다만 공유가 아닌 풀에서 다른 노드가 쓴 백업(`status.node`)은 여기에 없는 것이 당연하므로 실패다.
/// 풀이 없어졌으면 파일도 함께 없어진 것으로 보고 리소스만 지운다. 이 백업을 되살리는 Task가 끝나지
/// 않았으면(다른 노드에서 돌 수 있다) 실패다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(());
    };
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::BackupRestore { backup, .. } if backup == id)
    }) {
        return Err("backup is being restored".into());
    }
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)
        .map_err(|e| e.to_string())?;
    if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
    } else {
        let (pool, root) = backup_root(ctx, &backup.spec.pool)?;
        let dir = backup_dir(&root, id);
        let elsewhere = (!opl_types::storage::is_shared(&pool))
            .then_some(backup.status.node)
            .flatten()
            .filter(|n| *n != ctx.config.node);
        if let Some(node) = elsewhere
            && !tokio::fs::try_exists(&dir).await.unwrap_or(true)
        {
            return Err(format!("backup files are on node {node}, not on this node"));
        }
        remove_dir(&dir).await?;
    }
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// 백업 디렉터리의 매니페스트를 읽는다. 형식 버전이 다르면 나머지를 읽지 않고 오류다.
async fn read_manifest(dir: &Path) -> Result<BackupManifest, String> {
    let text = tokio::fs::read(dir.join(MANIFEST_FILE))
        .await
        .map_err(|e| format!("cannot read backup manifest: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&text).map_err(|e| format!("invalid backup manifest: {e}"))?;
    let version = value.get("version").and_then(|v| v.as_u64());
    if version != Some(MANIFEST_VERSION.into()) {
        let version = version.map_or("unknown".into(), |v| v.to_string());
        return Err(format!(
            "backup manifest version {version} is not supported (this node reads version {MANIFEST_VERSION})"
        ));
    }
    serde_json::from_value(value).map_err(|e| format!("invalid backup manifest: {e}"))
}

/// 백업 디렉터리 `dir` 안의 일반 파일 `file`의 경로. 심볼릭 링크(디렉터리 밖을 가리킬 수 있다)나 다른
/// 종류의 파일이면 오류다.
async fn regular_file(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let path = dir.join(file);
    match tokio::fs::symlink_metadata(&path).await {
        Ok(m) if m.file_type().is_file() => Ok(path),
        Ok(_) => Err(format!("{file} is not a regular file")),
        Err(e) => Err(format!("cannot read {file}: {e}")),
    }
}

/// `ready` 백업 `id`를 새 VM `name`(id는 Task의 대상)으로 되살린다(설계 §2.4): 매니페스트의 디스크마다
/// 풀 `pool`에 새 볼륨 `{name}-disk{i}`를 만들어 백업 파일을 쓰고(블록 풀이면 raw, 아니면 qcow2),
/// UEFI 변수를 복사한 뒤 백업 리소스에 적힌 VM 스펙으로 꺼진 VM을 만든다(새 MAC, 노드·예약·HA 없음).
/// 매니페스트는 그 기록과 같아야 한다. 공유가 아닌 풀의 백업은 파일을 쓴 노드에서만 되살린다.
/// 실패하면 실행기의 정리(복제와 같은 [`super::clone::discard`])가 만든 볼륨과 변수 사본을 지운다.
pub(super) async fn restore(
    ctx: &Ctx,
    task: &Task,
    id: Uuid,
    name: &str,
    pool: &str,
) -> Result<(), String> {
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    if ctx
        .cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {name:?} already exists"));
    }
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (source, root) = backup_root(ctx, &backup.spec.pool)?;
    let target = ready_pool(ctx, pool)?;
    let dir = backup_dir(&root, id);
    let manifest = read_manifest(&dir).await?;
    let spec = &backup.spec.vm_spec;
    if manifest.backup != id
        || manifest.vm_spec != *spec
        || manifest.disks.len() != spec.disks.len()
    {
        return Err("backup manifest does not match the backup record".into());
    }

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let format = copy_format(&source, &target, ImageFormat::Qcow2);
    let mut copies: Vec<Volume> = Vec::with_capacity(manifest.disks.len());
    for disk in &manifest.disks {
        let failed =
            |e: &dyn std::fmt::Display| format!("cannot restore disk {:?}: {e}", disk.name);
        // 파일은 백업 디렉터리 바로 안에 있어야 한다. 다른 파일을 참조하는 이미지도 받지 않는다.
        if Path::new(&disk.file).file_name() != Some(OsStr::new(&disk.file)) {
            return Err(failed(&format!("invalid file name {:?}", disk.file)));
        }
        let file = regular_file(&dir, &disk.file)
            .await
            .map_err(|e| failed(&e))?;
        ctx.storage
            .check_import_source(&file, SourceFormat::Qcow2)
            .await
            .map_err(|e| failed(&e))?;
        let copy_name = volume_name(name, &disk.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: disk.virtual_bytes,
                format,
            },
        );
        copy.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(copy.id));
        ctx.cluster
            .create(&copy)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", copy.name))?;
        ctx.storage
            .import_volume(&target, copy.id, &file, SourceFormat::Qcow2, format)
            .await
            .map_err(|e| failed(&e))?;
        copies.push(copy);
    }

    let mut new: Vm = Resource::new(name, backup.spec.vm_spec.clone());
    new.id = task.spec.target;
    if manifest.uefi_vars
        && let Some(state) = &ctx.config.state_dir
    {
        let vars = regular_file(&dir, VARS_FILE).await?;
        let to = uefi_vars_path(state, new.id);
        made(ctx, task.id, |m| m.vars = Some(to.clone()));
        copy_file(&vars, &to)
            .await
            .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
        // 새 VM의 변수는 이 노드에 있다. 다른 노드에서 켜면 여기서 받아 간다.
        new.status.firmware_node = Some(ctx.config.node);
    }
    for copy in &copies {
        mark_ready(ctx, copy, &target).await;
    }
    for (disk, copy) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    new.spec.ha = false;
    new.spec
        .validate()
        .map_err(|e| format!("invalid VM spec in backup: {e}"))?;
    create_vm(ctx, task, new).await
}
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust

/// 복제 Task가 지금까지 만든 것. Task가 실패하거나 시간이 다 되면 실행기의 정리([`discard`])가
/// 지운다. opld가 죽으면 사라진다. 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 고아 정리가
```

을 다음으로 바꾼다:

```rust

/// 복제(와 복원) Task가 지금까지 만든 것. Task가 실패하거나 시간이 다 되면 실행기의 정리([`discard`])가
/// 지운다. opld가 죽으면 사라진다. 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 고아 정리가
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
pub(super) struct Made {
    volumes: Vec<Uuid>,
    vars: Option<PathBuf>,
}

fn made(ctx: &Ctx, task: Uuid, f: impl FnOnce(&mut Made)) {
    f(ctx.cloned.lock().unwrap().entry(task).or_default());
```

을 다음으로 바꾼다:

```rust
pub(super) struct Made {
    pub(super) volumes: Vec<Uuid>,
    pub(super) vars: Option<PathBuf>,
}

pub(super) fn made(ctx: &Ctx, task: Uuid, f: impl FnOnce(&mut Made)) {
    f(ctx.cloned.lock().unwrap().entry(task).or_default());
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
```

을 다음으로 바꾼다:

```rust
/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
pub(super) fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
/// 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
fn copy_format(source: &StoragePool, target: &StoragePool, format: ImageFormat) -> ImageFormat {
    match (
```

을 다음으로 바꾼다:

```rust
/// 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
pub(super) fn copy_format(
    source: &StoragePool,
    target: &StoragePool,
    format: ImageFormat,
) -> ImageFormat {
    match (
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
```

을 다음으로 바꾼다:

```rust
    }
    create_vm(ctx, task, new).await
}

/// 복제·복원이 만든 새 VM `new`를 저장소에 만든다: MAC을 새로 받고, 노드·예약 없이 꺼진 VM이다.
/// 성공하면 Task가 만든 것을 더는 정리하지 않는다.
pub(super) async fn create_vm(ctx: &Ctx, task: &Task, mut new: Vm) -> Result<(), String> {
    let name = new.name.clone();
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
```

`crates/opl-api/src/tasks/clone.rs`에서

```rust

/// 실패한 복제가 만든 볼륨(파일과 리소스)과 UEFI 변수 사본을 지운다.
pub(super) async fn discard(ctx: &Ctx, task: Uuid) {
```

을 다음으로 바꾼다:

```rust

/// 실패한 복제(와 복원)가 만든 볼륨(파일과 리소스)과 UEFI 변수 사본을 지운다.
pub(super) async fn discard(ctx: &Ctx, task: Uuid) {
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
//! qcow2, UEFI 변수, 매니페스트), 실패하면 디렉터리를 지우고 `failed`로 남기며, 지우기는 파일과
//! 리소스를 지운다. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_tasks`
```

을 다음으로 바꾼다:

```rust
//! qcow2, UEFI 변수, 매니페스트), 실패하면 디렉터리를 지우고 `failed`로 남기며, 지우기는 파일과
//! 리소스를 지운다. 복원은 백업에서 꺼진 새 VM과 볼륨을 만들고, 실패하면 만든 볼륨을 지운다.
//! qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_tasks`
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
use std::process::Command;

use common::{Client, Env, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::backup::{
```

을 다음으로 바꾼다:

```rust
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, FakeStorage, MOUNT_ROOT, vm_request};
use opl_driver::{DirStorage, uefi_vars_path};
use opl_types::backup::{
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use reqwest::StatusCode;
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
    );
    assert!(!dir.exists());
}
```

을 다음으로 바꾼다:

```rust
    );
    assert!(!dir.exists());
}

fn get_vm(env: &Env, name: &str) -> Option<Vm> {
    env.cluster.get_by_name::<VmSpec>(name).unwrap()
}

fn get_volume(env: &Env, name: &str) -> Option<Volume> {
    env.cluster.get_by_name::<VolumeSpec>(name).unwrap()
}

/// 꺼진 VM `web`(`setup`)을 백업 `b1`로 백업한다.
async fn backed_up(env: &Env, admin: &Client) -> Backup {
    let task = wait(
        admin,
        admin
            .post("/vms/web/backups", json!({ "pool": "bk", "name": "b1" }))
            .await,
    )
    .await;
    succeeded(&task);
    backup(env, "b1").unwrap()
}

async fn restore(admin: &Client, name: &str, pool: &str) -> Value {
    wait(
        admin,
        admin
            .post("/backups/b1/restore", json!({ "name": name, "pool": pool }))
            .await,
    )
    .await
}

/// 복원은 백업의 VM 스펙으로 꺼진 새 VM을 만든다: 디스크마다 대상 풀에 `{새 이름}-disk{i}` 볼륨(dir
/// 풀이면 qcow2)을 만들어 백업 내용을 쓰고, UEFI 변수를 복사하고, MAC은 새로 받는다. 새 VM의 id는
/// Task의 대상이다. 복원은 디스크 크기만큼 걸리므로 Task 시간 제한을 받지 않는다.
#[tokio::test]
async fn restore_makes_a_stopped_vm_from_the_backup() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    let (vm, root, _) = setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    // 백업 뒤에 바뀐 원본 디스크는 복원에 들어가지 않는다.
    assert!(qemu_io(&root, "qcow2", "write -P 0xcc 0 64k"));
    create_pool(&env, "other", false).await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.timeout = Duration::ZERO
    })
    .await;

    let task = restore(&admin, "web-2", "other").await;
    succeeded(&task);
    let new = get_vm(&env, "web-2").unwrap();
    assert_eq!(task["spec"]["target"], new.id.to_string());
    let old = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(new.spec.power, PowerState::Stopped);
    assert_eq!(new.spec.node, None);
    assert!(!new.spec.ha);
    assert_eq!(new.spec.memory_mib, old.spec.memory_mib);
    let disks: Vec<&str> = new.spec.disks.iter().map(|d| d.volume.as_str()).collect();
    assert_eq!(disks, ["web-2-disk0", "web-2-disk1"]);
    let (old_mac, new_mac) = (old.spec.nics[0].mac, new.spec.nics[0].mac);
    assert!(new_mac.is_some());
    assert_ne!(new_mac, old_mac);

    for (name, size, pattern) in [
        ("web-2-disk0", 4 * MIB, "read -P 0xaa 0 64k"),
        ("web-2-disk1", 2 * MIB, "read -P 0x5a 64k 64k"),
    ] {
        let volume = get_volume(&env, name).unwrap();
        assert_eq!(volume.spec.pool, "other");
        assert_eq!(volume.spec.format, ImageFormat::Qcow2);
        assert_eq!(volume.spec.size_bytes, size);
        assert_eq!(volume.status.phase, VolumePhase::Ready);
        assert_eq!(volume.status.node, Some(env.node));
        let path = pool_dir(&env, "other").join(format!("{}.qcow2", volume.id));
        assert!(qemu_io(&path, "qcow2", pattern), "{name}");
    }
    let vars = uefi_vars_path(&env.dir.path().join("state"), new.id);
    assert_eq!(std::fs::read(vars).unwrap(), b"uefi variables");
    assert_eq!(new.status.firmware_node, Some(env.node));
    // 백업은 그대로 남는다.
    assert_eq!(backup(&env, "b1").unwrap().status.phase, BackupPhase::Ready);
    assert!(backup_dir(&env, b.id).join(MANIFEST_FILE).exists());
}

/// 블록 풀(여기서는 LVM-thin)로 복원하면 볼륨은 raw다.
#[tokio::test]
async fn restore_into_a_block_pool_writes_raw_volumes() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    backed_up(&env, &admin).await;
    let thin = Resource::new(
        "thin",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    env.cluster.create(&thin).await.unwrap();
    env.pool_ready("thin").await;
    env.stop_runner().await;
    let storage = Arc::new(FakeStorage::default());
    env.start_runner_with(storage.clone()).await;

    succeeded(&restore(&admin, "web-2", "thin").await);
    let volumes: Vec<Volume> = ["web-2-disk0", "web-2-disk1"]
        .iter()
        .map(|n| get_volume(&env, n).unwrap())
        .collect();
    for v in &volumes {
        assert_eq!(
            (v.spec.pool.as_str(), v.spec.format),
            ("thin", ImageFormat::Raw)
        );
    }
    assert_eq!(
        storage.import_formats(),
        [
            (volumes[0].id, ImageFormat::Raw),
            (volumes[1].id, ImageFormat::Raw)
        ]
    );
    assert!(get_vm(&env, "web-2").is_some());
}

/// 디스크 하나를 복원하지 못하면 그때까지 만든 볼륨(파일과 리소스)을 지우고 VM을 만들지 않는다.
#[tokio::test]
async fn failed_restore_removes_the_volumes_it_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    std::fs::write(backup_dir(&env, b.id).join("disk1.qcow2"), b"not an image").unwrap();

    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot restore disk \"disk1\""), "{error}");
    assert!(get_vm(&env, "web-2").is_none());
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_volume(&env, "web-2-disk1").is_none());
    let left: Vec<_> = std::fs::read_dir(pool_dir(&env, "other"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // 같은 이름으로 다시 복원할 수 있다(여기서는 같은 이유로 다시 실패한다).
    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
}

/// 다른 형식 버전의 매니페스트는 읽지 않는다. 볼륨을 만들기 전에 실패한다.
#[tokio::test]
async fn restore_refuses_another_manifest_version() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let path = backup_dir(&env, b.id).join(MANIFEST_FILE);
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["version"] = json!(MANIFEST_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("manifest version 2"), "{error}");
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_vm(&env, "web-2").is_none());
}

/// 복원은 백업 리소스에 적힌 VM 스펙으로 VM을 만든다. 매니페스트가 그 기록과 다르면(VM 스펙이나 디스크
/// 수) 볼륨을 만들기 전에 실패한다. 기록된 스펙이 VM 스펙 검사를 통과하지 못하면 만든 볼륨을 지운다.
#[tokio::test]
async fn restore_checks_the_manifest_against_the_backup_record() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let path = backup_dir(&env, b.id).join(MANIFEST_FILE);
    let original = std::fs::read(&path).unwrap();
    let manifest: Value = serde_json::from_slice(&original).unwrap();

    let mut memory = manifest.clone();
    memory["vm_spec"]["memory_mib"] = json!(4096);
    let mut disks = manifest.clone();
    disks["disks"].as_array_mut().unwrap().pop();
    for changed in [memory, disks] {
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        let task = restore(&admin, "web-2", "other").await;
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(
            error.contains("manifest does not match the backup record"),
            "{error}"
        );
        assert!(get_volume(&env, "web-2-disk0").is_none());
        assert!(get_vm(&env, "web-2").is_none());
    }

    // 기록과 매니페스트가 함께 틀린 스펙이면 VM을 만들지 않고 볼륨을 지운다.
    let mut spec = b.spec.clone();
    spec.vm_spec.memory_mib = 1;
    env.cluster
        .update_spec(b.id, Some(b.revision), b.labels.clone(), &spec)
        .await
        .unwrap();
    let mut bad = manifest.clone();
    bad["vm_spec"]["memory_mib"] = json!(1);
    std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
    let task = restore(&admin, "web-2", "other").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("invalid VM spec in backup"), "{error}");
    assert!(get_volume(&env, "web-2-disk0").is_none());
    assert!(get_vm(&env, "web-2").is_none());
}

/// 백업 디렉터리의 디스크·UEFI 변수 파일은 일반 파일이어야 한다. 심볼릭 링크는 따라가지 않는다.
#[tokio::test]
async fn restore_refuses_symlinks_in_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let dir = backup_dir(&env, b.id);
    let outside = env.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    for file in ["disk1.qcow2", "vars.fd"] {
        let moved = outside.join(file);
        std::fs::rename(dir.join(file), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, dir.join(file)).unwrap();
        let task = restore(&admin, "web-2", "other").await;
        assert_eq!(task["status"]["state"], "failed", "{task}");
        let error = task["status"]["error"].as_str().unwrap();
        assert!(
            error.contains(&format!("{file} is not a regular file")),
            "{error}"
        );
        assert!(get_volume(&env, "web-2-disk0").is_none());
        assert!(get_vm(&env, "web-2").is_none());
        std::fs::remove_file(dir.join(file)).unwrap();
        std::fs::rename(&moved, dir.join(file)).unwrap();
    }
    succeeded(&restore(&admin, "web-2", "other").await);
}

/// 공유가 아닌 풀에서 다른 노드가 쓴 백업은 이 노드에서 되살리지 않는다(API는 그 노드에 Task를 둔다).
#[tokio::test]
async fn restore_runs_only_on_the_node_holding_the_backup() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "other", false).await;
    let n2 = other_node(&env, "n2").await;
    let status = opl_types::backup::BackupStatus {
        node: Some(n2),
        ..b.status.clone()
    };
    env.cluster
        .update_status::<BackupSpec>(b.id, &status)
        .await
        .unwrap();

    let op = opl_types::task::TaskOp::BackupRestore {
        backup: b.id,
        name: "web-2".into(),
        pool: "other".into(),
    };
    let task = put_task(&env, op, Uuid::now_v7(), env.node, TaskState::Pending).await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        format!("backup is stored on node {n2}")
    );
    assert!(get_volume(&env, "web-2-disk0").is_none());
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    copy_formats: std::sync::Mutex<Vec<(Uuid, ImageFormat)>>,
    /// `resize_volume`이 크기를 이 단위로 올린다(LVM의 extent처럼). 0이면 그대로다.
```

을 다음으로 바꾼다:

```rust
    copy_formats: std::sync::Mutex<Vec<(Uuid, ImageFormat)>>,
    /// `import_volume`으로 만든 볼륨과 그 형식
    import_formats: std::sync::Mutex<Vec<(Uuid, ImageFormat)>>,
    /// `resize_volume`이 크기를 이 단위로 올린다(LVM의 extent처럼). 0이면 그대로다.
```

`crates/opl-api/tests/common/mod.rs`에서

```rust

    pub fn pause(&self) {
```

을 다음으로 바꾼다:

```rust

    /// `import_volume`으로 만든 볼륨과 그 형식
    pub fn import_formats(&self) -> Vec<(Uuid, ImageFormat)> {
        self.import_formats.lock().unwrap().clone()
    }

    pub fn pause(&self) {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
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
```

을 다음으로 바꾼다:

```rust
    }
    /// 호출을 `import`로 적고 형식을 남긴다. 원본 파일은 읽지 않는다.
    async fn import_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        _: &Path,
        _: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.record("import", id).await;
        self.import_formats.lock().unwrap().push((id, format));
        Ok(PathBuf::new())
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        Ok(())
    }
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 971개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test backup_tasks --test clones`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opl-api): 복원 Task — 백업에서 꺼진 새 VM과 볼륨 만들기, 실패하면 볼륨 정리"
```

---

### Task 5: oplctl·웹에서 백업

oplctl `backup create VM --pool P [--name N]`, `backup list [--vm VM]`(`NAME VM POOL PHASE CREATED SIZE`), `backup get`, `backup delete KEY [--force]`, `backup restore BACKUP --name NEW --pool P [--node N]`(만들기·복원은 24시간까지 기다림), `pool create … --backups`, `pool set NAME [--shared|--no-shared] [--backups true|false]`. 웹: 백업 페이지(모든 백업, VM이 지워진 것 포함), VM 상세의 백업 탭(지금 백업·복원 대화상자), 풀 화면의 백업 저장 스위치와 백업 수, Task 종류 문구, 오류 문구, en·ko.

**Files:**
- Create: `web/src/backups/BackupTable.tsx`
- Create: `web/src/backups/BackupsPage.test.tsx`
- Create: `web/src/backups/BackupsPage.tsx`
- Create: `web/src/backups/VmBackups.test.tsx`
- Create: `web/src/backups/VmBackups.tsx`
- Create: `web/src/backups/backups.test.ts`
- Create: `web/src/backups/backups.ts`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/storage/PoolsPage.test.tsx`
- Modify: `web/src/storage/PoolsPage.tsx`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/test/render.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`

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

use crate::client::{BACKUP_WAIT, Client, MIGRATE_WAIT, fetch_ca, fingerprint, local};
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
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
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
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
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
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
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
        backups,
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
    if backups {
        spec["backups"] = json!(true);
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
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{group}");
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
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
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
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
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
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
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
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
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    Get {
        backup: String,
    },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
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
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
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
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", set.pool),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
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
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
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

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client.get(&format!("/security-groups/{group}")).await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client.delete(&format!("/security-groups/{group}")).await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create { vm, pool, name } => {
            let body = json!({ "pool": pool, "name": name });
            let task = client.post(&format!("/vms/{vm}/backups"), &body).await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            // 이름을 주지 않았으면 서버가 정한 이름을 읽는다.
            let name = match name {
                Some(name) => name,
                None => {
                    let id = cell(&task["spec"]["op"]["backup"]);
                    cell(&client.get(&format!("/backups/{id}")).await?["name"])
                }
            };
            writeln!(out, "backup {name} of vm {vm} created")?;
        }
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => {
            print_json(out, &client.get(&format!("/backups/{backup}")).await?)?
        }
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{backup}/restore"), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{backup}"));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &["NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE"];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
    ]
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
            print_cluster_status(out, &view)?;
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
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
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
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
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
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
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
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
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

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
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

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
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

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
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

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
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

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
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
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
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

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
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
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
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
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
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
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
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
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
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
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
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
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        // 백업 풀과 복원의 새 이름·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..], ["pending", "-", "-"]);
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

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
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

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
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
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
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
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/oplctl/src/client.rs`에서

```rust
pub const MIGRATE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

을 다음으로 바꾼다:

```rust
pub const MIGRATE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// 백업·복원 Task를 기다리는 시간. 디스크를 모두 복사하므로 Task 실행기는 시간 제한을 두지 않는다.
pub const BACKUP_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

`web/src/App.test.tsx`에서

```tsx
      "/security-groups",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

을 다음으로 바꾼다:

```tsx
      "/security-groups",
      "/backups",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

`web/src/App.tsx`에서

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { ClusterPage } from "@/cluster/ClusterPage";
```

을 다음으로 바꾼다:

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupsPage } from "@/backups/BackupsPage";
import { ClusterPage } from "@/cluster/ClusterPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "networks", element: <NetworksPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "backups", element: <BackupsPage /> },
            { path: "networks", element: <NetworksPage /> },
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
    http.get(apiUrl("/security-groups"), empty),
  ];
```

을 다음으로 바꾼다:

```tsx
    http.get(apiUrl("/security-groups"), empty),
    http.get(apiUrl("/backups"), empty),
  ];
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group&since=7",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup&since=7",
  );
```

`web/src/api/live.ts`에서

```ts
  "security-group",
];
```

을 다음으로 바꾼다:

```ts
  "security-group",
  "backup",
];
```

`web/src/api/live.ts`에서

```ts
        return call(api.GET("/security-groups"));
    }
```

을 다음으로 바꾼다:

```ts
        return call(api.GET("/security-groups"));
      case "backup":
        return call(api.GET("/backups"));
    }
```

`web/src/api/types.ts`에서

```ts
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
```

을 다음으로 바꾼다:

```ts
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type Backup = Schemas["Resource_BackupSpec_BackupStatus"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
```

`web/src/api/types.ts`에서

```ts
export type WatchKind =
  "vm" | "volume" | "storage-pool" | "node" | "task" | "network" | "snapshot" | "security-group";
```

을 다음으로 바꾼다:

```ts
export type WatchKind =
  | "vm"
  | "volume"
  | "storage-pool"
  | "node"
  | "task"
  | "network"
  | "snapshot"
  | "security-group"
  | "backup";
```

`web/src/api/types.ts`에서

```ts
  "security-group": SecurityGroup;
}
```

을 다음으로 바꾼다:

```ts
  "security-group": SecurityGroup;
  backup: Backup;
}
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("admin", "write", "security-group")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("admin", "write", "security-group")).toBe(true);
  // 백업은 누구나 읽고 operator도 지우고 되살린다(서버의 백업 쓰기 권한).
  expect(can("viewer", "read", "backup")).toBe(true);
  expect(can("viewer", "write", "backup")).toBe(false);
  expect(can("operator", "write", "backup")).toBe(true);
});
```

`web/src/auth/permissions.ts`에서

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

을 다음으로 바꾼다:

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨, 백업만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

`web/src/auth/permissions.ts`에서

```ts
    case "operator":
      return action === "read" ? !account : kind === "vm" || kind === "volume";
    case "viewer":
```

을 다음으로 바꾼다:

```ts
    case "operator":
      return action === "read" ? !account : kind === "vm" || kind === "volume" || kind === "backup";
    case "viewer":
```

`web/src/backups/BackupTable.tsx`를 만든다:

```tsx
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Backup, Task } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { storedBytes } from "@/backups/backups";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { validName } from "@/vms/form";

const PHASE_VARIANT = {
  pending: "secondary",
  ready: "default",
  failed: "destructive",
} as const;

/**
 * 백업 표와 복원·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만).
 */
export function BackupTable({
  backups,
  query,
  labelledBy,
  showVm = false,
}: {
  backups: Backup[];
  query: { isPending: boolean; isError: boolean; error: unknown };
  labelledBy: string;
  showVm?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const vms = useList("vm");
  const [restoring, setRestoring] = useState<Backup | null>(null);
  // 닫히는 동안에도 대상을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [deleting, setDeleting] = useState<Backup | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  // 복원은 새 VM과 볼륨을 만든다(서버는 백업·VM·볼륨 쓰기를 모두 본다).
  const restore =
    can(me.role, "write", "backup") &&
    can(me.role, "write", "vm") &&
    can(me.role, "write", "volume");
  const remove = can(me.role, "write", "backup");
  const actions = restore || remove;
  const columns = 5 + (showVm ? 1 : 0) + (actions ? 1 : 0);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));

  return (
    <>
      <Table aria-labelledby={labelledBy}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {showVm && <TableHead>{t("backups.vm")}</TableHead>}
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("backups.created")}</TableHead>
            <TableHead>{t("backups.size")}</TableHead>
            {actions && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {backups.length === 0 && (
            <TableMessage colSpan={columns} query={query} empty={t("backups.empty")} />
          )}
          {backups.map((backup) => {
            const vm = vmById.get(backup.spec.vm);
            const size = storedBytes(backup);
            const { phase, created_at: created, error } = backup.status;
            return (
              <TableRow key={backup.id}>
                <TableCell className="font-medium">{backup.name}</TableCell>
                {showVm && (
                  <TableCell>
                    {vm ? (
                      <Link
                        to={`/vms/${encodeURIComponent(vm.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {vm.name}
                      </Link>
                    ) : (
                      backup.spec.vm_name
                    )}
                  </TableCell>
                )}
                <TableCell>{backup.spec.pool}</TableCell>
                <TableCell>
                  <Badge variant={PHASE_VARIANT[phase]}>{t(`backups.phase.${phase}`)}</Badge>
                  {error && (
                    <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                      {error}
                    </div>
                  )}
                </TableCell>
                <TableCell>
                  {created !== null ? formatTime(new Date(created * 1000), i18n.language) : "—"}
                </TableCell>
                <TableCell>{size !== undefined ? formatBytes(size) : "—"}</TableCell>
                {actions && (
                  <TableCell className="text-right whitespace-nowrap">
                    {restore && (
                      <Button
                        variant="outline"
                        size="sm"
                        aria-label={t("backups.restoreNamed", { name: backup.name })}
                        disabled={phase !== "ready"}
                        onClick={(e) => {
                          opener.current = e.currentTarget;
                          setRestoring(backup);
                        }}
                      >
                        {t("backups.restore")}
                      </Button>
                    )}
                    {/* 실패했거나 Task 없이 남은 백업도 지운다. 진행 중인 Task가 있으면 서버가 거절한다. */}
                    {remove && (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="ml-2"
                        aria-label={t("backups.deleteNamed", { name: backup.name })}
                        onClick={(e) => {
                          opener.current = e.currentTarget;
                          setDeleting(backup);
                          setDeleteOpen(true);
                        }}
                      >
                        {t("common.delete")}
                      </Button>
                    )}
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {restoring && (
        <RestoreDialog backup={restoring} returnFocus={opener} onClose={() => setRestoring(null)} />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("backups.deleteTitle", { name: deleting?.name })}
        description={t("backups.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const backup = deleting!;
          // `force` 없이 지우면 응답은 늘 Task다(200 `{ message }`는 force만).
          const task = (await call(
            api.DELETE("/backups/{key}", { params: { path: { key: backup.id } } }),
          )) as Task;
          trackTask(task, t("backups.op.delete", { name: backup.name }));
        }}
      />
    </>
  );
}

/** 백업에서 새 VM(꺼짐)을 만드는 대화상자. 열 때만 그린다. */
function RestoreDialog({
  backup,
  returnFocus,
  onClose,
}: {
  backup: Backup;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const pools = useList("storage-pool");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    } else if ((vms.data ?? []).some((v) => v.name === name)) {
      found.name = "backups.errors.vmExists";
    }
    if (pool === "") {
      found.pool = "backups.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/backups/{key}/restore", {
        params: { path: { key: backup.id } },
        body: { name, pool },
      }),
    );
    trackTask(task, t("backups.op.restore", { backup: backup.name, name }));
  }

  return (
    <FormDialog
      title={t("backups.restoreTitle", { name: backup.name })}
      description={t("backups.restoreDescription")}
      submitLabel={t("backups.restore")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="restore-name" label={t("backups.newName")} error={errors.name}>
        {(a11y) => (
          <Input
            id="restore-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field id="restore-pool" label={t("backups.diskPool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="restore-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("backups.choosePool")}</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/backups/BackupsPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupsPage } from "@/backups/BackupsPage";
import { resetTasks } from "@/tasks/tasks";
import { backup, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(resetTasks);

const web = vm("web", "running");
const gone = vm("old", "stopped");
const ofWeb = backup("web-1", web, 1);
const ofGone = backup("old-1", gone, 2);

it("lists every backup and links only VMs that still exist", async () => {
  mockLists({ vm: [web], backup: [ofWeb, ofGone] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const kept = (await within(table).findByText("web-1")).closest("tr")!;
  expect(within(kept).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  // VM을 지운 백업은 만든 때의 이름만 보인다.
  const orphan = within(table).getByText("old-1").closest("tr")!;
  expect(orphan).toHaveTextContent("old");
  expect(within(orphan).queryByRole("link")).not.toBeInTheDocument();
});

it("restores a backup as a new VM", async () => {
  mockLists({ vm: [web], backup: [ofGone], "storage-pool": [pool("local"), pool("fast")] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/backups/${ofGone.id}/restore`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-restore", "new-vm"), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Restore old-1" }));
  const dialog = await screen.findByRole("dialog", { name: "Restore old-1" });
  // 이름은 새 VM 이름이어야 하고 풀은 꼭 고른다.
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "web");
  await userEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(within(dialog).getByText("A VM with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.clear(within(dialog).getByLabelText("New VM name"));
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "old2");
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Storage pool for the disks"),
    "fast",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Restore" }));
  expect(await screen.findByText("Restore old-1 as old2")).toBeInTheDocument();
  expect(body).toEqual({ name: "old2", pool: "fast" });
});

it("deletes a backup after confirming", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/backups/${ofWeb.id}`), () => {
      deleted = true;
      return HttpResponse.json(task("t1", "backup-delete", ofWeb.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup web-1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-1?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete backup web-1")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("explains a backup that is busy", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  server.use(
    http.delete(apiUrl(`/backups/${ofWeb.id}`), () =>
      HttpResponse.json(
        { code: "backup_busy", message: 'backup "web-1" is being restored' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup web-1" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup web-1?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A task on this backup (backing up, restoring or deleting) is in progress.",
  );
});

it("lets operators restore and delete", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("button", { name: "Restore web-1" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Delete backup web-1" })).toBeEnabled();
});
```

`web/src/backups/BackupsPage.tsx`를 만든다:

```tsx
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";

/** 모든 백업. VM을 지운 백업도 보인다. 백업은 VM 화면의 백업 탭에서 만든다. */
export function BackupsPage() {
  const { t } = useTranslation();
  const backups = useList("backup");
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <h1 id="backups-title" className="text-2xl font-semibold">
          {t("nav.backups")}
        </h1>
        <p className="text-sm text-muted-foreground">{t("backups.pageHint")}</p>
      </div>
      <BackupTable
        backups={newestFirst(backups.data ?? [])}
        query={backups}
        labelledBy="backups-title"
        showVm
      />
    </div>
  );
}
```

`web/src/backups/VmBackups.test.tsx`를 만든다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Vm } from "@/api/types";
import { formatTime } from "@/lib/format";
import { resetTasks } from "@/tasks/tasks";
import { backup, NODE, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(resetTasks);

const web = vm("web", "running");
const first = backup("web-20261104-030000", web, 1);
const taking = backup("web-20261105-030000", web, 2, {
  status: { phase: "pending", created_at: null, disks: [] },
});
const broken = backup("web-broken", web, 3, {
  status: { phase: "failed", disks: [], error: "qemu-img convert failed" },
});
// 다른 VM의 백업은 보이지 않는다.
const other = backup("db-20261104-030000", vm("db", "stopped"), 4);

function open(v: Vm, as = ADMIN) {
  return renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: `/vms/${v.name}?tab=backups`,
    as,
  });
}

it("lists the VM's backups, newest first", async () => {
  mockLists({ vm: [web], node: [NODE], backup: [first, other, taking, broken] });
  open(web);
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-broken");
  const rows = within(table).getAllByRole("row").slice(1);
  expect(rows.map((r) => within(r).getAllByRole("cell")[0]!.textContent)).toEqual([
    "web-broken",
    "web-20261105-030000",
    "web-20261104-030000",
  ]);
  const [failed, pending, ready] = rows as [HTMLElement, HTMLElement, HTMLElement];
  expect(ready).toHaveTextContent("bk");
  expect(ready).toHaveTextContent("Ready");
  expect(ready).toHaveTextContent(formatTime(new Date(1_793_761_200 * 1000), "en"));
  expect(ready).toHaveTextContent("1.5 GiB");
  expect(within(ready).getByRole("button", { name: "Restore web-20261104-030000" })).toBeEnabled();
  // 만드는 중이거나 실패한 백업은 복원할 수 없지만 지울 수는 있다.
  expect(pending).toHaveTextContent("Creating");
  expect(
    within(pending).getByRole("button", { name: "Restore web-20261105-030000" }),
  ).toBeDisabled();
  expect(failed).toHaveTextContent("Failed");
  expect(failed).toHaveTextContent("qemu-img convert failed");
  expect(within(failed).getByRole("button", { name: "Delete backup web-broken" })).toBeEnabled();
});

it("backs up a running VM to the chosen backup pool and follows its task", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    "storage-pool": [pool("local"), pool("bk", { backups: true }), pool("nas", { backups: true })],
    backup: [first],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/backups`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "vm-backup", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  // 실행 중인 VM은 멈추지 않고 한 시점의 디스크를 복사한다고 알린다.
  expect(dialog).toHaveTextContent("The VM keeps running");
  const select = within(dialog).getByLabelText("Backup pool");
  expect(
    within(select)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk", "nas"]);
  await userEvent.selectOptions(select, "nas");
  // 이름은 클러스터에서 고유하다.
  await userEvent.type(within(dialog).getByLabelText("Name (optional)"), "web-20261104-030000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Back up now" }));
  expect(within(dialog).getByText("A backup with this name already exists.")).toBeVisible();
  await userEvent.clear(within(dialog).getByLabelText("Name (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Back up now" }));
  expect(await screen.findByText("Back up web")).toBeInTheDocument();
  // 이름을 비우면 보내지 않는다(서버가 정한다).
  expect(body).toEqual({ pool: "nas" });
  await waitFor(() => expect(screen.getByRole("button", { name: "Back up now" })).toHaveFocus());
});

it("says when no pool stores backups", async () => {
  const off = vm("web", "stopped");
  mockLists({ vm: [off], node: [NODE], "storage-pool": [pool("local")] });
  open(off);
  await userEvent.click(await screen.findByRole("button", { name: "Back up now" }));
  const dialog = await screen.findByRole("dialog", { name: "Back up web" });
  expect(dialog).toHaveTextContent("No storage pool stores backups.");
  expect(within(dialog).getByRole("button", { name: "Back up now" })).toBeDisabled();
});

it("does not back up a VM that is starting", async () => {
  const starting = vm("web", "starting");
  mockLists({ vm: [starting], node: [NODE], "storage-pool": [pool("bk", { backups: true })] });
  open(starting);
  expect(await screen.findByRole("button", { name: "Back up now" })).toBeDisabled();
  expect(screen.getByText(/Back it up when it has settled/)).toBeInTheDocument();
});

it("hides backup changes from viewers", async () => {
  mockLists({ vm: [web], node: [NODE], backup: [first] });
  open(web, { ...ADMIN, role: "viewer" });
  const table = await screen.findByRole("table", { name: "Backups" });
  await within(table).findByText("web-20261104-030000");
  expect(screen.queryByRole("button", { name: "Back up now" })).not.toBeInTheDocument();
  expect(within(table).queryByRole("button")).not.toBeInTheDocument();
});
```

`web/src/backups/VmBackups.tsx`를 만든다:

```tsx
import { Archive } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { backupPools, newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { trackTask } from "@/tasks/tasks";
import { isBusy, isStopped } from "@/vms/actions";
import { validName } from "@/vms/form";

/** VM 상세의 백업 탭: 이 VM의 백업 목록과 지금 백업하기 */
export function VmBackups({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const backups = useList("backup");
  const pools = useList("storage-pool");
  const [creating, setCreating] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const mine = newestFirst((backups.data ?? []).filter((b) => b.spec.vm === vm.id));
  // 서버는 시작·정지 중인 VM을 백업하지 않는다(vm_busy).
  const busy = isBusy(vm);

  return (
    <section className="flex flex-col gap-4 pt-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h2 id="vm-backups" className="font-medium">
          {t("vms.tab.backups")}
        </h2>
        {can(me.role, "write", "vm") && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 풀 목록을 읽어야 백업 풀을 고른다. */}
            {!pools.isSuccess && <ListPending error={pools.error} />}
            <Button
              size="sm"
              disabled={busy || !pools.isSuccess || !backups.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setCreating(true);
              }}
            >
              <Archive />
              {t("backups.create")}
            </Button>
          </div>
        )}
      </div>
      {can(me.role, "write", "vm") && busy && (
        <p className="text-sm text-muted-foreground">{t("backups.busy")}</p>
      )}
      <BackupTable backups={mine} query={backups} labelledBy="vm-backups" />
      {creating && (
        <CreateBackupDialog
          vm={vm}
          taken={(backups.data ?? []).map((b) => b.name)}
          returnFocus={opener}
          onClose={() => setCreating(false)}
        />
      )}
    </section>
  );
}

function CreateBackupDialog({
  vm,
  taken,
  returnFocus,
  onClose,
}: {
  vm: Vm;
  /** 클러스터의 백업 이름(이름은 VM마다가 아니라 클러스터에서 고유하다) */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const pools = backupPools(useList("storage-pool").data ?? []);
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았으면 첫 백업 풀이다.
  const pool = chosen ?? pools[0]?.name ?? "";
  const [name, setName] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    // 비우면 서버가 `{VM 이름}-{UTC 시각}`으로 정한다.
    if (name !== "" && !validName(name)) {
      found.name = "forms.name";
    } else if (taken.includes(name)) {
      found.name = "backups.errors.exists";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/backups", {
        params: { path: { key: vm.id } },
        body: { pool, ...(name !== "" && { name }) },
      }),
    );
    trackTask(task, t("backups.op.create", { vm: vm.name }));
  }

  return (
    <FormDialog
      title={t("backups.createTitle", { name: vm.name })}
      description={t(isStopped(vm) ? "backups.createDescription" : "backups.runningDescription")}
      submitLabel={t("backups.create")}
      submitDisabled={pools.length === 0}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {pools.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("backups.noPools")}</p>
      ) : (
        <Field id="backup-pool" label={t("backups.pool")}>
          <NativeSelect
            id="backup-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setChosen(e.target.value)}
          >
            {pools.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
      )}
      <Field
        id="backup-name"
        label={t("backups.name")}
        error={errors.name}
        hint={t("backups.nameHint")}
      >
        {(a11y) => (
          <Input
            id="backup-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/backups/backups.test.ts`를 만든다:

```ts
import { backupPools, newestFirst, storedBytes } from "@/backups/backups";
import { backup, pool, vm } from "@/test/fixtures";

const web = vm("web", "stopped");

it("adds up the stored size of a ready backup only", () => {
  const ready = backup("b1", web, 1, {
    status: {
      disks: [
        { name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 3 },
        { name: "disk1", file: "disk1.qcow2", virtual_bytes: 10, stored_bytes: 4 },
      ],
    },
  });
  expect(storedBytes(ready)).toBe(7);
  expect(storedBytes(backup("b2", web, 2, { status: { phase: "pending" } }))).toBeUndefined();
  expect(storedBytes(backup("b3", web, 3, { status: { phase: "failed" } }))).toBeUndefined();
});

it("offers only pools that store backups", () => {
  const pools = [pool("local"), pool("bk", { backups: true }), pool("off", { backups: false })];
  expect(backupPools(pools).map((p) => p.name)).toEqual(["bk"]);
});

it("puts the newest backup first", () => {
  const list = [backup("a", web, 1), backup("c", web, 3), backup("b", web, 2)];
  expect(newestFirst(list).map((b) => b.name)).toEqual(["c", "b", "a"]);
});
```

`web/src/backups/backups.ts`를 만든다:

```ts
import type { Backup, StoragePool } from "@/api/types";

/** 백업 파일 크기의 합. 다 만들지 않은 백업은 undefined */
export function storedBytes(backup: Backup): number | undefined {
  if (backup.status.phase !== "ready") {
    return undefined;
  }
  return backup.status.disks.reduce((sum, disk) => sum + disk.stored_bytes, 0);
}

/** 백업을 담는 풀(`backups`를 켠 디렉터리·NFS 풀) */
export function backupPools(pools: readonly StoragePool[]): StoragePool[] {
  return pools.filter((pool) => pool.spec.backups === true);
}

/** 최근 백업이 위에 오게 정렬한다(id가 UUIDv7이라 문자열 순서가 만든 차례다). */
export function newestFirst(backups: readonly Backup[]): Backup[] {
  return [...backups].sort((a, b) => (a.id < b.id ? 1 : -1));
}
```

`web/src/i18n/en.json`에서

```json
    "firewall_unsupported": "The node has no firewall (nftables) and cannot run VMs with security groups.",
    "security_group_in_use": "A VM interface uses this security group. Remove it from the VM first."
  },
```

을 다음으로 바꾼다:

```json
    "firewall_unsupported": "The node has no firewall (nftables) and cannot run VMs with security groups.",
    "security_group_in_use": "A VM interface uses this security group. Remove it from the VM first.",
    "backup_busy": "A task on this backup (backing up, restoring or deleting) is in progress. Try again when it has finished.",
    "backup_not_ready": "The backup is not ready. Only finished backups can be restored."
  },
```

`web/src/i18n/en.json`에서

```json
    "pools": "Storage pools",
    "networks": "Networks",
```

을 다음으로 바꾼다:

```json
    "pools": "Storage pools",
    "backups": "Backups",
    "networks": "Networks",
```

`web/src/i18n/en.json`에서

```json
      "vm-migrate": "Migrate {{target}}",
      "vm-ha-recover": "HA recovery of {{target}} (from node {{from}})"
    }
```

을 다음으로 바꾼다:

```json
      "vm-migrate": "Migrate {{target}}",
      "vm-ha-recover": "HA recovery of {{target}} (from node {{from}})",
      "vm-backup": "Back up {{target}}",
      "backup-delete": "Delete backup {{target}}",
      "backup-restore": "Restore a backup as {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "snapshots": "Snapshots",
      "tasks": "Tasks",
```

을 다음으로 바꾼다:

```json
      "snapshots": "Snapshots",
      "backups": "Backups",
      "tasks": "Tasks",
```

`web/src/i18n/en.json`에서

```json
      "poolUnavailable": "That storage pool is not ready on the node holding the VM."
    }
```

을 다음으로 바꾼다:

```json
      "poolUnavailable": "That storage pool is not ready on the node holding the VM."
    }
  },
  "backups": {
    "vm": "VM",
    "pool": "Backup pool",
    "created": "Created",
    "size": "Size",
    "empty": "No backups.",
    "phase": {
      "pending": "Creating",
      "ready": "Ready",
      "failed": "Failed"
    },
    "pageHint": "Backups of every VM, including VMs that were deleted. Take a backup from the Backups tab of a VM.",
    "create": "Back up now",
    "createTitle": "Back up {{name}}",
    "createDescription": "Copies the disks, firmware variables and settings of the VM to a backup pool. The copy is kept after the VM is deleted.",
    "runningDescription": "Copies the disks, firmware variables and settings of the VM to a backup pool. The VM keeps running: the disks are copied as they were at one moment, like after a power cut.",
    "noPools": "No storage pool stores backups. Turn on \"Store backups\" for a directory or NFS pool in Storage pools.",
    "name": "Name (optional)",
    "nameHint": "Leave empty to name it after the VM and the UTC time, such as web-20261104-030000.",
    "busy": "The VM is starting, stopping or migrating. Back it up when it has settled.",
    "restore": "Restore",
    "restoreNamed": "Restore {{name}}",
    "restoreTitle": "Restore {{name}}",
    "restoreDescription": "Makes a new stopped VM from the backup with copies of its disks. It gets new MAC addresses and the same networks and security groups.",
    "newName": "New VM name",
    "diskPool": "Storage pool for the disks",
    "choosePool": "Choose a pool",
    "deleteNamed": "Delete backup {{name}}",
    "deleteTitle": "Delete backup {{name}}?",
    "deleteDescription": "The backup files are removed from the backup pool. This cannot be undone.",
    "op": {
      "create": "Back up {{vm}}",
      "restore": "Restore {{backup}} as {{name}}",
      "delete": "Delete backup {{name}}"
    },
    "errors": {
      "exists": "A backup with this name already exists.",
      "vmExists": "A VM with this name already exists.",
      "pool": "Choose a storage pool."
    }
```

`web/src/i18n/en.json`에서

```json
    "target": "iSCSI target",
    "targetHint": "The target name, such as iqn.2003-01.org.example:storage."
  },
```

을 다음으로 바꾼다:

```json
    "target": "iSCSI target",
    "targetHint": "The target name, such as iqn.2003-01.org.example:storage.",
    "backups": "Backups",
    "backupsNamed": "Store backups: {{name}}",
    "backupsState": "Stored",
    "backupsOn": "{{name}} now stores backups.",
    "backupsOff": "{{name}} no longer stores backups.",
    "backupsErrors": {
      "pool_in_use": "The pool still holds backups. Delete them first."
    },
    "holdsBackups_one": "{{count}} backup",
    "holdsBackups_other": "{{count}} backups",
    "storeBackups": "Store backups",
    "storeBackupsHint": "VM backups can be kept in this pool, under backups/ in its directory. VM disks can still be kept there too."
  },
```

`web/src/i18n/i18n.test.ts`에서

```ts
  ).toBe("The node has no firewall (nftables) and cannot run VMs with security groups.");
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

을 다음으로 바꾼다:

```ts
  ).toBe("The node has no firewall (nftables) and cannot run VMs with security groups.");
  expect(errorMessage(t, new ApiError(409, "backup_not_ready", 'backup "b" is not ready'))).toBe(
    "The backup is not ready. Only finished backups can be restored.",
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "firewall_unsupported": "노드에 방화벽(nftables)이 없어 보안 그룹을 건 VM을 돌릴 수 없습니다.",
    "security_group_in_use": "이 보안 그룹을 쓰는 VM 인터페이스가 있습니다. 먼저 VM에서 빼세요."
  },
```

을 다음으로 바꾼다:

```json
    "firewall_unsupported": "노드에 방화벽(nftables)이 없어 보안 그룹을 건 VM을 돌릴 수 없습니다.",
    "security_group_in_use": "이 보안 그룹을 쓰는 VM 인터페이스가 있습니다. 먼저 VM에서 빼세요.",
    "backup_busy": "이 백업의 작업(백업, 복원, 삭제)이 진행 중입니다. 끝난 뒤 다시 시도하세요.",
    "backup_not_ready": "백업이 준비되지 않았습니다. 다 만든 백업만 복원할 수 있습니다."
  },
```

`web/src/i18n/ko.json`에서

```json
    "pools": "스토리지 풀",
    "networks": "네트워크",
```

을 다음으로 바꾼다:

```json
    "pools": "스토리지 풀",
    "backups": "백업",
    "networks": "네트워크",
```

`web/src/i18n/ko.json`에서

```json
      "vm-migrate": "{{target}} 마이그레이션",
      "vm-ha-recover": "{{target}} HA 복구 (노드 {{from}}에서)"
    }
```

을 다음으로 바꾼다:

```json
      "vm-migrate": "{{target}} 마이그레이션",
      "vm-ha-recover": "{{target}} HA 복구 (노드 {{from}}에서)",
      "vm-backup": "{{target}} 백업",
      "backup-delete": "백업 {{target}} 삭제",
      "backup-restore": "백업을 {{target}}(으)로 복원"
    }
```

`web/src/i18n/ko.json`에서

```json
      "snapshots": "스냅샷",
      "tasks": "작업",
```

을 다음으로 바꾼다:

```json
      "snapshots": "스냅샷",
      "backups": "백업",
      "tasks": "작업",
```

`web/src/i18n/ko.json`에서

```json
      "poolUnavailable": "VM이 있는 노드에서 준비되지 않은 스토리지 풀입니다."
    }
```

을 다음으로 바꾼다:

```json
      "poolUnavailable": "VM이 있는 노드에서 준비되지 않은 스토리지 풀입니다."
    }
  },
  "backups": {
    "vm": "VM",
    "pool": "백업 풀",
    "created": "만든 시각",
    "size": "크기",
    "empty": "백업이 없습니다.",
    "phase": {
      "pending": "만드는 중",
      "ready": "준비됨",
      "failed": "실패"
    },
    "pageHint": "지운 VM의 백업까지 모든 VM의 백업입니다. 백업은 VM 화면의 백업 탭에서 만듭니다.",
    "create": "지금 백업",
    "createTitle": "{{name}} 백업",
    "createDescription": "VM의 디스크, 펌웨어 변수, 설정을 백업 풀에 복사합니다. VM을 지워도 복사본은 남습니다.",
    "runningDescription": "VM의 디스크, 펌웨어 변수, 설정을 백업 풀에 복사합니다. VM은 계속 실행되며, 디스크는 전원이 갑자기 꺼진 때처럼 한 시점의 상태로 복사됩니다.",
    "noPools": "백업을 담는 스토리지 풀이 없습니다. 스토리지 풀에서 디렉터리나 NFS 풀의 \"백업 저장\"을 켜세요.",
    "name": "이름(선택)",
    "nameHint": "비우면 VM 이름과 UTC 시각으로 정합니다(예: web-20261104-030000).",
    "busy": "VM이 시작·정지·마이그레이션하는 중입니다. 끝나면 백업할 수 있습니다.",
    "restore": "복원",
    "restoreNamed": "{{name}} 복원",
    "restoreTitle": "{{name}} 복원",
    "restoreDescription": "백업에서 디스크를 복사해 꺼진 새 VM을 만듭니다. MAC 주소는 새로 정하고 네트워크와 보안 그룹은 그대로입니다.",
    "newName": "새 VM 이름",
    "diskPool": "디스크를 둘 스토리지 풀",
    "choosePool": "풀을 고르세요",
    "deleteNamed": "백업 {{name}} 삭제",
    "deleteTitle": "백업 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "백업 풀에서 백업 파일을 지웁니다. 되돌릴 수 없습니다.",
    "op": {
      "create": "{{vm}} 백업",
      "restore": "{{backup}}을(를) {{name}}(으)로 복원",
      "delete": "백업 {{name}} 삭제"
    },
    "errors": {
      "exists": "같은 이름의 백업이 이미 있습니다.",
      "vmExists": "같은 이름의 VM이 이미 있습니다.",
      "pool": "스토리지 풀을 고르세요."
    }
```

`web/src/i18n/ko.json`에서

```json
    "target": "iSCSI 타깃",
    "targetHint": "타깃 이름입니다(예: iqn.2003-01.org.example:storage)."
  },
```

을 다음으로 바꾼다:

```json
    "target": "iSCSI 타깃",
    "targetHint": "타깃 이름입니다(예: iqn.2003-01.org.example:storage).",
    "backups": "백업",
    "backupsNamed": "백업 저장: {{name}}",
    "backupsState": "저장",
    "backupsOn": "{{name}}에 백업을 저장합니다.",
    "backupsOff": "{{name}}에 더는 백업을 저장하지 않습니다.",
    "backupsErrors": {
      "pool_in_use": "풀에 백업이 남아 있습니다. 먼저 백업을 지우세요."
    },
    "holdsBackups_one": "백업 {{count}}개",
    "holdsBackups_other": "백업 {{count}}개",
    "storeBackups": "백업 저장",
    "storeBackupsHint": "VM 백업을 이 풀의 디렉터리 아래 backups/에 둘 수 있습니다. VM 디스크도 그대로 둘 수 있습니다."
  },
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  );
});
```

을 다음으로 바꾼다:

```tsx
  );
  expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/backups");
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
import {
  Boxes,
```

을 다음으로 바꾼다:

```tsx
import {
  Archive,
  Boxes,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/storage-pools", key: "nav.pools", icon: Database, end: false },
  { to: "/backups", key: "nav.backups", icon: Archive, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, poolState, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { PoolsPage } from "@/storage/PoolsPage";
import { backup, NODE, pool, poolState, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[5];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[5]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
```

을 다음으로 바꾼다:

```tsx
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[6];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[6]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
```

`web/src/storage/PoolsPage.test.tsx`에서

```tsx
  const nas = (await within(table).findByText("nas")).closest("tr")!;
  expect(nas).toHaveTextContent("Shared");
  expect(within(nas).queryByRole("switch")).not.toBeInTheDocument();
  const fast = within(table).getByText("fast").closest("tr")!;
  expect(fast).toHaveTextContent("Node-local");
  expect(within(fast).queryByRole("switch")).not.toBeInTheDocument();
  expect(within(table).getAllByRole("switch")).toHaveLength(1);
});
```

을 다음으로 바꾼다:

```tsx
  const nas = (await within(table).findByText("nas")).closest("tr")!;
  const sharing = { name: /^Shared storage/ };
  expect(nas).toHaveTextContent("Shared");
  expect(within(nas).queryByRole("switch", sharing)).not.toBeInTheDocument();
  const fast = within(table).getByText("fast").closest("tr")!;
  expect(fast).toHaveTextContent("Node-local");
  expect(within(fast).queryByRole("switch", sharing)).not.toBeInTheDocument();
  expect(within(table).getAllByRole("switch", sharing)).toHaveLength(1);
});

it("turns backups on for directory and NFS pools only", async () => {
  const local = pool("local");
  mockLists({
    "storage-pool": [
      local,
      pool("nas", { backend: { kind: "nfs", server: "nas", export: "/srv/vm" }, backups: true }),
      pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
    ],
  });
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/storage-pools/${local.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("local", { backups: true }));
    }),
  );
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const backups = { name: /^Store backups/ };
  expect(await within(table).findByRole("switch", { name: "Store backups: nas" })).toBeChecked();
  const fast = within(table).getByText("fast").closest("tr")!;
  expect(within(fast).queryByRole("switch", backups)).not.toBeInTheDocument();
  expect(within(table).getAllByRole("switch", backups)).toHaveLength(2);
  const toggle = within(table).getByRole("switch", { name: "Store backups: local" });
  expect(toggle).not.toBeChecked();
  await userEvent.click(toggle);
  expect(await screen.findByText("local now stores backups.")).toBeInTheDocument();
  expect(body).toEqual({ backups: true });
});

it("says why backups cannot be turned off and keeps the pool while it holds backups", async () => {
  const bk = pool("bk", { backups: true });
  mockLists({
    "storage-pool": [bk],
    volume: [],
    backup: [backup("web-1", vm("web", "stopped"), 1)],
  });
  server.use(
    http.patch(apiUrl(`/storage-pools/${bk.id}`), () =>
      HttpResponse.json(
        { code: "pool_in_use", message: 'pool "bk" holds backups' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const row = (await within(table).findByText("bk")).closest("tr")!;
  expect(await within(row).findByText("1 backup")).toBeInTheDocument();
  expect(within(row).getByRole("button", { name: "Delete bk" })).toBeDisabled();
  await userEvent.click(within(row).getByRole("switch", { name: "Store backups: bk" }));
  // 지우기의 pool_in_use 문구(볼륨이 남음)가 아니라 이 동작의 문구
  expect(
    await screen.findByText("The pool still holds backups. Delete them first."),
  ).toBeInTheDocument();
});

it("creates a pool that stores backups", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("bk"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "bk");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/bk");
  await userEvent.click(within(dialog).getByLabelText("Store backups"));
  // 블록 풀은 백업을 담지 못하므로 묻지 않는다.
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "lvm-thin");
  expect(within(dialog).queryByLabelText("Store backups")).not.toBeInTheDocument();
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "dir");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool bk.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "bk",
    spec: { backend: { kind: "dir", path: "/srv/bk" }, nodes: [], backups: true },
  });
});
```

`web/src/storage/PoolsPage.tsx` 전체:

```tsx
import { Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
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
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import {
  cephNameError,
  cephUserError,
  iscsiPortalError,
  iscsiTargetError,
  isShared,
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
  const backups = useList("backup");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [deleting, setDeleting] = useState<StoragePool | null>(null);
  // 공유 여부를 바꿀 디렉터리 풀과 바꿀 값. 닫히는 동안에도 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [sharing, setSharing] = useState<{ pool: StoragePool; shared: boolean } | null>(null);
  const [sharingOpen, setSharingOpen] = useState(false);
  const write = can(me.role, "write", "storage-pool");

  const nodeList = useMemo(() => nodes.data ?? [], [nodes.data]);
  // 볼륨 목록을 다 읽기 전에는 모른다(undefined).
  const volumeCount = (pool: StoragePool) =>
    volumes.isSuccess ? volumes.data.filter((v) => v.spec.pool === pool.name).length : undefined;
  const backupCount = (pool: StoragePool) =>
    backups.isSuccess ? backups.data.filter((b) => b.spec.pool === pool.name).length : undefined;

  // 백업 표시는 확인 없이 바꾼다. 백업이 남은 풀은 끌 수 없다(서버가 409).
  async function setBackups(pool: StoragePool, on: boolean) {
    try {
      await call(
        api.PATCH("/storage-pools/{key}", {
          params: { path: { key: pool.id } },
          body: { backups: on },
        }),
      );
      toast.success(t(on ? "pools.backupsOn" : "pools.backupsOff", { name: pool.name }));
    } catch (err) {
      // pool_in_use는 지우기와 뜻이 달라 이 동작의 문구로 보인다.
      toast.error(
        err instanceof ApiError
          ? codeMessage(t, err.code, err.message, "pools.backupsErrors")
          : errorMessage(t, err),
      );
    }
  }

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
            <TableHead>{t("pools.sharing")}</TableHead>
            <TableHead>{t("pools.backups")}</TableHead>
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
            <TableMessage colSpan={write ? 9 : 8} query={pools} empty={t("pools.empty")} />
          )}
          {(pools.data ?? []).map((pool) => {
            const capacity = poolCapacity(pool, nodeList);
            const used = volumeCount(pool);
            const held = backupCount(pool);
            const backupKind = pool.spec.backend.kind === "dir" || pool.spec.backend.kind === "nfs";
            return (
              <TableRow key={pool.id}>
                <TableCell className="font-medium">{pool.name}</TableCell>
                <TableCell>{t(`pools.kinds.${pool.spec.backend.kind}`)}</TableCell>
                <TableCell className="font-mono text-xs">{poolLocation(pool)}</TableCell>
                <TableCell>
                  {/* 디렉터리 풀만 바꿀 수 있다. 다른 풀은 종류가 정한다(NFS·RBD·공유 LVM은 늘 공유). */}
                  {write && pool.spec.backend.kind === "dir" ? (
                    <Switch
                      aria-label={t("pools.sharedNamed", { name: pool.name })}
                      checked={isShared(pool)}
                      onCheckedChange={(shared) => {
                        setSharing({ pool, shared });
                        setSharingOpen(true);
                      }}
                    />
                  ) : isShared(pool) ? (
                    t("pools.sharedState")
                  ) : (
                    t("pools.localState")
                  )}
                </TableCell>
                <TableCell>
                  {/* 디렉터리·NFS 풀만 백업을 담는다. */}
                  {write && backupKind ? (
                    <Switch
                      aria-label={t("pools.backupsNamed", { name: pool.name })}
                      checked={pool.spec.backups === true}
                      onCheckedChange={(on) => void setBackups(pool, on)}
                    />
                  ) : pool.spec.backups ? (
                    t("pools.backupsState")
                  ) : (
                    "—"
                  )}
                  {write && held !== undefined && held > 0 && (
                    <div className="text-xs text-muted-foreground">
                      {t("pools.holdsBackups", { count: held })}
                    </div>
                  )}
                </TableCell>
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
                      // 볼륨이나 백업이 남은 풀은 서버가 지우지 않는다.
                      disabled={used !== 0 || held !== 0}
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
        open={sharingOpen}
        onOpenChange={setSharingOpen}
        title={t(sharing?.shared ? "pools.makeSharedTitle" : "pools.makeLocalTitle", {
          name: sharing?.pool.name,
        })}
        description={
          sharing?.shared
            ? t("pools.makeSharedDescription", { path: sharing ? poolLocation(sharing.pool) : "" })
            : t("pools.makeLocalDescription")
        }
        confirmLabel={t(sharing?.shared ? "pools.makeShared" : "pools.makeLocal")}
        onConfirm={async () => {
          const { pool, shared } = sharing!;
          try {
            await call(
              api.PATCH("/storage-pools/{key}", {
                params: { path: { key: pool.id } },
                body: { shared },
              }),
            );
          } catch (err) {
            // pool_in_use는 지우기와 뜻이 달라 이 동작의 문구로 보인다.
            throw err instanceof ApiError
              ? new Error(codeMessage(t, err.code, err.message, "pools.sharingErrors"))
              : err;
          }
          toast.success(t(shared ? "pools.madeShared" : "pools.madeLocal", { name: pool.name }));
        }}
      />
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
  const [backups, setBackups] = useState(false);
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
    // 디렉터리·NFS 풀만 백업을 담는다. 끈 풀은 보내지 않는다.
    const backupPool = (kind === "dir" || kind === "nfs") && backups;
    const spec = {
      backend,
      nodes: known,
      ...(sharedDir && { shared: true }),
      ...(backupPool && { backups: true }),
    };
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
      {(kind === "dir" || kind === "nfs") && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Checkbox
              id="pool-backups"
              checked={backups}
              onCheckedChange={(v) => setBackups(v === true)}
              aria-describedby="pool-backups-hint"
            />
            <Label htmlFor="pool-backups">{t("pools.storeBackups")}</Label>
          </div>
          <p id="pool-backups-hint" className="text-xs text-muted-foreground">
            {t("pools.storeBackupsHint")}
          </p>
        </div>
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

`web/src/tasks/TasksPage.test.tsx`에서

```tsx

it("shows HA recoveries with the failed node's name", async () => {
```

을 다음으로 바꾼다:

```tsx

it("names backup tasks by the backup and the VM being restored", async () => {
  const backup = "019a0f3c-0000-7000-8006-000000000001";
  mockLists({
    vm: [{ id: VM, name: "web" }],
    backup: [{ id: backup, name: "web-1" }],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000001",
        spec: { op: { kind: "vm-backup", backup }, target: VM, node: "n1" },
        status: { state: "succeeded", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000002",
        spec: { op: { kind: "backup-delete" }, target: backup, node: "n1" },
        status: { state: "running", error: null },
      },
      {
        // 복원의 대상은 아직 없는 새 VM이다. 요청한 이름으로 보인다.
        id: "019a0f3c-8a10-7000-8000-000000000003",
        spec: {
          op: { kind: "backup-restore", backup, name: "web2", pool: "local" },
          target: "019a0f3c-0000-7000-8000-00000000cafe",
          node: "n1",
        },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByRole("link", { name: "Back up web" })).toHaveAttribute(
    "href",
    "/vms/web",
  );
  expect(await screen.findByText("Delete backup web-1")).toBeInTheDocument();
  expect(screen.getByText("Restore a backup as web2")).toBeInTheDocument();
});

it("shows HA recoveries with the failed node's name", async () => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
  const volumes = useList("volume");
  return useMemo(() => {
    const vmById = new Map(vms.data?.map((v) => [v.id, v]));
    const volumeById = new Map(volumes.data?.map((v) => [v.id, v]));
    return (task: Task) => {
```

을 다음으로 바꾼다:

```tsx
  const volumes = useList("volume");
  const backups = useList("backup");
  return useMemo(() => {
    const vmById = new Map(vms.data?.map((v) => [v.id, v]));
    const volumeById = new Map(volumes.data?.map((v) => [v.id, v]));
    const backupById = new Map(backups.data?.map((b) => [b.id, b]));
    return (task: Task) => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
      }
      const volume = volumeById.get(task.spec.target);
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: volume?.name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data]);
}
```

을 다음으로 바꾼다:

```tsx
      }
      const op = task.spec.op;
      // 복원의 대상은 Task가 끝날 때 만들어지는 새 VM이다. 그 전에는 요청한 이름을 보인다.
      if (op.kind === "backup-restore") {
        return { name: op.name, href: undefined };
      }
      const name = volumeById.get(task.spec.target)?.name ?? backupById.get(task.spec.target)?.name;
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data, backups.data]);
}
```

`web/src/test/fixtures.ts`에서

```ts
import type {
  Node,
```

을 다음으로 바꾼다:

```ts
import type {
  Backup,
  Node,
```

`web/src/test/fixtures.ts`에서

```ts
    status: {},
  };
}
```

을 다음으로 바꾼다:

```ts
    status: {},
  };
}

/** 테스트용 백업. `n`은 id 끝자리(UUIDv7처럼 만든 차례)다. */
export function backup(
  name: string,
  of: Vm,
  n: number,
  extra: { spec?: Partial<Backup["spec"]>; status?: Partial<Backup["status"]> } = {},
): Backup {
  return {
    id: `019a0f3c-0000-7000-8006-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: { vm: of.id, vm_name: of.name, pool: "bk", vm_spec: of.spec, ...extra.spec },
    status: {
      phase: "ready",
      created_at: 1_793_761_200,
      disks: [
        {
          name: "disk0",
          file: "disk0.qcow2",
          virtual_bytes: 16 * 1024 ** 3,
          stored_bytes: 1536 * 1024 ** 2,
        },
      ],
      uefi_vars: true,
      error: null,
      ...extra.status,
    },
  };
}
```

`web/src/test/render.tsx`에서

```tsx
  "security-group": "/security-groups",
};
```

을 다음으로 바꾼다:

```tsx
  "security-group": "/security-groups",
  backup: "/backups",
};
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { can } from "@/auth/permissions";
import { ListPending } from "@/components/ListPending";
```

을 다음으로 바꾼다:

```tsx
import { can } from "@/auth/permissions";
import { VmBackups } from "@/backups/VmBackups";
import { ListPending } from "@/components/ListPending";
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx

const TABS = ["summary", "hardware", "console", "snapshots", "tasks", "json"] as const;
```

을 다음으로 바꾼다:

```tsx

const TABS = ["summary", "hardware", "console", "snapshots", "backups", "tasks", "json"] as const;
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
          <VmSnapshots vm={vm} />
        </TabsContent>
```

을 다음으로 바꾼다:

```tsx
          <VmSnapshots vm={vm} />
        </TabsContent>
        <TabsContent value="backups">
          <VmBackups vm={vm} />
        </TabsContent>
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 976개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 368개

- [ ] **Step 3: 커밋**

```bash
git add crates web
git commit -m "feat: oplctl·웹에서 백업 — backup 명령(만들기·목록·읽기·지우기·복원), pool create --backups·pool set --backups, VM 백업 탭과 백업 화면, 풀의 백업 저장 스위치"
```

---

### Task 6: 실제 시험과 문서

`crates/opld/tests/backup.rs`(기능 `qemu-it`, 실제 QEMU): 디스크에 표식을 쓴 VM을 실행 중에 백업하면 VM은 같은 pid로 계속 돌고 임시 스냅샷이 남지 않으며 압축 qcow2·매니페스트·UEFI 변수가 생긴다; VM을 지우고 복원하면 새 VM의 디스크에 같은 표식이 있고 켜진다; 멈춘 VM의 qcow2·raw 두 디스크가 백업 때 내용으로 돌아오고, 백업이 남은 풀은 지울 수 없다. README 백업 절(백업 풀, 백업·복원·지우기·강제 지우기, crash-consistent, 파일 형식과 opLite 없이 여는 법, 한계와 알려진 틈), 설계 "구현(계획 27)" 메모, 처음 분해 문서 §1.1.

**Files:**
- Create: `crates/opld/tests/backup.rs`
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-04-backup-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown

## 웹 UI
```

을 다음으로 바꾼다:

````markdown

## 백업과 복원

백업은 VM 전체(디스크, UEFI 변수, VM 스펙)를 다른 저장소에 떠 둔 것이다. 스냅샷과 달리 VM 디스크의 저장소를
잃거나 VM을 지워도 남고, 언제든 **새 VM**으로 되살린다. 백업은 `backups`를 켠 **디렉터리·NFS 풀**(백업 풀)에 둔다.

```bash
oplctl pool create bk --nfs nas.lan:/srv/backup --backups    # 백업 풀(디렉터리 풀이면 --path /srv/backup)
oplctl pool set local --backups true                         # 있는 dir·NFS 풀을 백업 풀로(false는 끄기)
oplctl backup create web --pool bk                           # 이름은 web-20261104-030000(UTC)처럼 정해진다
oplctl backup create web --pool bk --name before-upgrade
oplctl backup list                                           # --vm web이면 지금 web인 VM과 web이라는 이름으로 남은(지운 VM) 백업
# NAME            VM   POOL  PHASE  CREATED               SIZE
# before-upgrade  web  bk    ready  2026-11-04 03:00 UTC  412.5MiB
oplctl backup get before-upgrade                             # 디스크별 크기, UEFI 변수, 오류(JSON)
oplctl backup restore before-upgrade --name web2 --pool local   # 새 VM web2(꺼진 채), --node로 노드를 정할 수 있다
oplctl backup delete before-upgrade                          # 파일과 리소스를 지운다(실패한 백업도)
```

웹은 사이드바의 "백업" 화면(모든 백업, VM이 지워진 백업 포함)과 VM 상세의 "백업" 탭(지금 백업, 복원)에서 하고,
스토리지 풀 화면의 "백업 저장" 스위치로 백업 풀을 정한다. API는 `POST /api/v1/vms/{name}/backups`
(`{"pool": …, "name": …}`), `GET /api/v1/backups[?vm=]`, `GET`·`DELETE /api/v1/backups/{name}`,
`POST /api/v1/backups/{name}/restore`(`{"name": …, "pool": …, "node": …}`)이고 만들기·지우기·복원은 작업(Task)을 돌려준다.
`DELETE /api/v1/backups/{name}?force=true`(`oplctl backup delete NAME --force`)는 파일을 지울 노드가 없을 때 리소스만
지우고 200(`{"message": …}`)이다.

- **백업 풀.** dir·NFS 풀만 백업을 담는다(다른 풀은 400). 백업 풀에도 지금처럼 VM 디스크를 둘 수 있다. 백업이 남은
  풀은 지울 수 없고 `backups`를 끄거나 `shared`를 바꿀 수도 없다(409 `pool_in_use`, "holds backups"). 백업 디렉터리를
  다른 서버에 두려면 NFS 풀을 쓴다 — 공유가 아닌 디렉터리 풀은 노드 로컬이라 그 노드의 디스크를 잃으면 함께 잃는다.
  공유가 아닌 풀의 백업은 쓴 노드를 기억하고(`status.node`) 그 노드에서만 복원하고 지운다: 복원에서 다른 `node`를
  주면 409 `backup_on_other_node`, 그 노드가 오프라인이면 409 `node_offline`, 대상 풀을 그 노드에서 쓸 수 없으면 409
  `pool_not_on_node`다. 그 노드를 잃었으면 `oplctl backup delete NAME --force`(`DELETE …?force=true`)로 리소스만
  지운다(파일은 남는다). 풀째 지워진
  백업(저장소에서 풀이 사라짐)은 보통의 지우기가 리소스만 지운다.
- **실행 중·꺼진 VM.** 실행 중인 VM은 디스크마다 메모리 없는 임시 스냅샷(이름은 하이픈 없는 백업 id)을 찍어 그때의
  내용을 복사하고 끝나면 지운다. VM은 계속 돈다(블록 풀 디스크는 스냅샷을 찍는 동안만 잠깐 멈춘다). 그래서 실행
  중인 VM의 디스크는 모두 스냅샷을 지원해야 한다 — dir·NFS 풀의 raw 디스크나 공유 LVM 풀 디스크가 있으면 409
  `vm_running`("stop it to back it up")이고, VM을 끄면 된다. 스냅샷처럼 방식이 다른 디스크(qcow2와 블록 풀, 서로 다른
  블록 백엔드)가 섞여도 409 `snapshot_unsupported`다. 블록 풀 디스크를 위해 멈춘 VM을 이어서 실행하지 못하면 한 번 더
  해 보고, 그래도 안 되면 디스크를 읽지 않고 백업이 실패한다. 백업 작업이 도는 동안 그 VM의 시작·정지·수정·지우기와
  마이그레이션은 409 `vm_busy`다(강제 정지는 된다). 작업은 VM의 노드에서 돈다. 배치된 적 없는 꺼진 VM은 백업 풀과
  디스크를 모두 읽을 수 있는 온라인 노드에서 한다(없으면 409 `no_node`).
- **crash-consistent.** 백업은 그 순간에 전원을 뽑은 것과 같은 디스크다(게스트 파일 시스템을 얼리지 않는다). 파일
  시스템 저널은 되살아나지만, 데이터베이스처럼 메모리에 쓰기를 쥐는 앱은 게스트 안에서 덤프를 떠 두거나 백업 동안
  쓰기를 멈춰 일관성을 맞춘다. 꺼진 VM의 백업은 일관적이다. 디스크 파일은 `qemu-img`가 닫을 때 디스크에 내려
  쓰고(`-t writeback`), `vars.fd`와 `manifest.json`은 fsync한 뒤 이름을 바꾸고 백업 디렉터리도 fsync한 다음에야
  `ready`가 된다.
- **복원.** 늘 새 VM이다(기존 VM을 덮어쓰지 않는다). 디스크마다 대상 풀에 새 볼륨 `{새 이름}-disk0`, `-disk1`…을
  만든다(블록 풀이면 raw, 아니면 qcow2). 새 VM은 백업 때의 VM 스펙(CPU·메모리·NIC·펌웨어 등)과 UEFI 변수를 받고, 새
  MAC, 꺼진 채, 노드·예약 스냅샷 없음, HA 끔이다. VM 스펙은 클러스터의 백업 기록에서 가져오고 `manifest.json`이
  그 기록(VM 스펙, 디스크 수)과 다르면 실패한다. 백업 디렉터리의 디스크·`vars.fd`는 일반 파일이어야 한다(심볼릭
  링크는 거절). 스펙의 네트워크와 보안 그룹이 지금 있어야 한다(400). 복원은 백업
  풀과 대상 풀을 함께 쓸 수 있는 노드에서 돈다. 실패하면 만든 볼륨을 지운다. 이름을 원래대로 돌리려면 원본을
  지운 뒤 그 이름으로 복원한다.
- **권한.** 읽기는 모든 사용자, 만들기·지우기·복원은 operator 이상이다(VM이 지워진 백업도 operator가 지운다).
- **오래 걸린다.** 진행률은 없고 작업에 시간 제한이 없다. `oplctl backup create`·`restore`는 24시간까지 기다리고,
  그 뒤에도 작업은 돈다(`oplctl task get`). 백업 하나는 디스크 크기만큼 읽고 압축해 쓴다(증분은 없다).

**파일 형식과 opLite 없이 열기.** 백업 하나는 백업 풀 루트의 `backups/{백업 id}/` 디렉터리다(id는 `oplctl backup
get NAME`의 `id`; NFS 풀이면 NFS 서버의 내보내기 아래 같은 경로). 그 안에 디스크마다 압축 qcow2 `disk0.qcow2`,
`disk1.qcow2`…(backing file 없음, 스냅샷 없음), UEFI VM이면 변수 파일 `vars.fd`, 그리고 `manifest.json`(형식 버전
`version: 1`, 백업 id, VM id·이름, VM 스펙 전체, 디스크 목록과 크기, 만든 시각)이 있다. 표준 qcow2라 opLite 없이도 연다:

```bash
qemu-img info backups/<id>/disk0.qcow2                                  # 크기 확인
qemu-img convert -O raw backups/<id>/disk0.qcow2 disk0.raw              # raw 이미지로(dd로 디스크에 써도 된다)
qemu-system-x86_64 -m 2048 -drive file=backups/<id>/disk0.qcow2,snapshot=on   # 원본을 바꾸지 않고 켜 보기
jq .vm_spec backups/<id>/manifest.json                                 # CPU·메모리·NIC 등 VM 설정
```

UEFI VM은 OVMF(arm64는 AAVMF) 코드 파일과 함께 `vars.fd`의 사본을 `-drive if=pflash`로 준다. 다른 형식 버전의
매니페스트는 복원하지 않는다(작업이 실패한다).

- **한계.** 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존
  VM 덮어쓰기는 없다. 예약 백업과 보존 규칙은 계획 28에서 온다(지금은 손으로 만들고 지운다).
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
  `oplctl backup delete`로 지운다. 실행 중 백업이 임시 스냅샷을 지우지 못하면 qcow2 디스크에 그 태그가 남을 수 있다
  (블록 풀 디스크는 opld가 나중에 지운다; qcow2는 VM을 끄고 `qemu-img snapshot -d <태그> <파일>`). 복원이 볼륨을
  다 만든 뒤 VM을 만들기 전에 opld가 죽으면 `{새 이름}-disk{i}` 볼륨이 남는다(복제도 같다) — `oplctl volume delete`로
  지운다. 백업 디렉터리의 파일 작업에는 시간 제한이 없어, NFS 서버가 멈추면(hard 마운트) 백업·지우기·복원 작업이
  서버가 돌아올 때까지 멈춘다.

## 웹 UI
````

`README.md`에서

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```

을 다음으로 바꾼다:

```markdown
cargo test -p opld --features qemu-it --test e2e                    # E2E: CLI → VM → 콘솔 → kill -9 → 재연결
cargo test -p opld --features qemu-it --test backup                 # E2E: 실행 중 백업 → VM 지우기 → 복원(qemu-img, qemu-io 필요)
cargo test -p opld --test cluster                                   # E2E: opld 프로세스 3개로 클러스터 구성
```

`README.md`에서

```markdown
- [네트워크·SDN 설계 (하위 프로젝트 6: 계획 25·26)](docs/superpowers/specs/2026-11-03-network-sdn-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [네트워크·SDN 설계 (하위 프로젝트 6: 계획 25·26)](docs/superpowers/specs/2026-11-03-network-sdn-design.md)
- [백업 설계 (하위 프로젝트 7: 계획 27·28)](docs/superpowers/specs/2026-11-04-backup-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`crates/opld/tests/backup.rs`를 만든다:

```rust
//! 실제 QEMU로 VM 백업(실행 중·꺼진 VM)과 새 VM으로의 복원, 백업 지우기를 데몬 전체를 거쳐 확인한다.
//! dir 풀 둘을 쓴다: VM 디스크용 `local`과 백업 풀 `bk`. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크
//! 내용은 VM을 켜기 전에 써 두고, 복원한 볼륨을 `qemu-img convert`로 raw로 빼서 같은지 본다.
//! `cargo test -p opld --features qemu-it --test backup`
//! QEMU(`qemu-it`와 같다)와 qemu-img, qemu-io가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::StatusCode;
use serde_json::{Value, json};

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;

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

/// 클러스터를 만들고 로그인해 VM 디스크 풀 `local`과 백업 풀 `bk`를 만든다.
async fn setup(d: &mut Daemon) {
    d.start().await;
    d.init("backup").await;
    d.login().await;
    for (name, extra) in [("local", None), ("bk", Some("--backups"))] {
        let dir = d.path(name);
        std::fs::create_dir(&dir).unwrap();
        let mut args = vec!["pool", "create", name, "--path", dir.to_str().unwrap()];
        args.extend(extra);
        d.ctl(&args).await.unwrap();
        d.wait_pool_ready(name).await;
    }
}

/// 풀 `local`에 볼륨 `name`(64 MiB)을 만들고 그 파일 경로를 돌려준다.
async fn create_volume(d: &Daemon, name: &str, format: &str) -> PathBuf {
    d.ctl(&[
        "volume", "create", name, "--pool", "local", "--size", "64M", "--format", format,
    ])
    .await
    .unwrap();
    volume_file(d, name).await
}

/// 볼륨 `name`의 파일(풀 `local`)
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// qcow2 파일 앞에 `byte`를 `MARK`만큼 쓴다.
fn mark_qcow2(file: &Path, byte: u8) {
    let ok = Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", &format!("write -P {byte:#x} 0 {MARK}")])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success();
    assert!(ok, "qemu-io write to {}", file.display());
}

/// raw 파일 앞에 `byte`를 `MARK`만큼 쓴다.
fn mark_raw(file: &Path, byte: u8) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
    f.write_all(&[byte; MARK]).unwrap();
}

/// 이미지(`qemu-img`가 형식을 알아본다)의 앞 `MARK` 바이트. raw로 빼서 읽는다. 실행 중인 VM이 쥔 파일도
/// 읽는다(`-U`).
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    let out = Command::new("qemu-img")
        .args(["convert", "-U", "-O", "raw"])
        .arg(file)
        .arg(&raw)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

fn qemu_img_info(file: &Path) -> Value {
    let out = Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(file)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 풀 `bk` 안의 백업 디렉터리
fn backup_dir(d: &Daemon, backup: &Value) -> PathBuf {
    d.path("bk")
        .join("backups")
        .join(backup["id"].as_str().unwrap())
}

/// 디렉터리의 파일 이름(정렬)
fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// VM의 UEFI 변수 파일(BIOS VM이면 없다)
fn vars_file(d: &Daemon, vm: &Value) -> PathBuf {
    d.data_dir()
        .join("vm")
        .join(vm["id"].as_str().unwrap())
        .join("uefi-vars.fd")
}

#[tokio::test]
async fn running_vm_backup_restores_to_a_new_vm() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    setup(&mut d).await;
    let c = client(&d);
    let root = create_volume(&d, "root", "qcow2").await;
    mark_qcow2(&root, 0xab);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--nic", "user", "--start",
    ])
    .await
    .unwrap();
    let web = c.get("/vms/web").await.unwrap();
    let pid = web["status"]["pid"].as_u64().unwrap();

    // 실행 중에 백업한다. 이름을 주지 않으면 `{VM 이름}-{UTC 시각}`이다.
    let out = d
        .ctl(&["backup", "create", "web", "--pool", "bk"])
        .await
        .unwrap();
    assert!(
        out.starts_with("backup web-") && out.ends_with(" of vm web created\n"),
        "{out}"
    );
    let list = d.ctl_json(&["backup", "list", "--vm", "web"]).await;
    let backups = list.as_array().unwrap();
    assert_eq!(backups.len(), 1, "{list}");
    let backup = backups[0].clone();
    let name = backup["name"].as_str().unwrap().to_string();
    assert_eq!(backup["status"]["phase"], "ready", "{backup}");
    assert_eq!(backup["spec"]["vm"], web["id"], "{backup}");
    assert_eq!(backup["spec"]["vm_name"], "web", "{backup}");
    assert_eq!(backup["spec"]["pool"], "bk", "{backup}");
    assert_eq!(backup["status"]["disks"][0]["name"], "disk0", "{backup}");
    assert_eq!(
        backup["status"]["disks"][0]["virtual_bytes"],
        64u64 << 20,
        "{backup}"
    );
    // VM은 그대로 돌고, 임시 스냅샷은 지워 디스크에 태그가 없다.
    let v = c.get("/vms/web").await.unwrap();
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert!(
        qemu_img_info(&root)["snapshots"].is_null(),
        "temporary snapshot left"
    );

    // 백업 디렉터리: 압축 qcow2 디스크, 매니페스트, UEFI 변수(있으면).
    let dir = backup_dir(&d, &backup);
    let uefi = backup["status"]["uefi_vars"] == true;
    let mut expected = vec!["disk0.qcow2", "manifest.json"];
    if uefi {
        expected.push("vars.fd");
    }
    assert_eq!(files(&dir), expected);
    let disk = dir.join("disk0.qcow2");
    let info = qemu_img_info(&disk);
    assert_eq!(info["format"], "qcow2", "{info}");
    assert!(info["backing-filename"].is_null(), "{info}");
    assert_eq!(
        backup["status"]["disks"][0]["stored_bytes"],
        std::fs::metadata(&disk).unwrap().len(),
        "{backup}"
    );
    assert_eq!(head(&d, &disk), vec![0xab; MARK]);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["version"], 1, "{manifest}");
    assert_eq!(manifest["backup"], backup["id"], "{manifest}");
    assert_eq!(manifest["vm_name"], "web", "{manifest}");
    assert_eq!(
        manifest["vm_spec"]["disks"][0]["volume"], "root",
        "{manifest}"
    );

    // 원본 VM과 디스크를 지워도 백업은 남는다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    let out = d.ctl(&["backup", "list"]).await.unwrap();
    assert!(out.lines().nth(1).unwrap().starts_with(&name), "{out}");

    // 새 VM으로 복원한다: 디스크는 `{새 이름}-disk{i}`, 꺼진 채, 노드 없음, 새 MAC.
    let out = d
        .ctl(&[
            "backup", "restore", &name, "--name", "copy", "--pool", "local",
        ])
        .await
        .unwrap();
    assert_eq!(out, format!("vm copy restored from backup {name}\n"));
    let copy = c.get("/vms/copy").await.unwrap();
    assert_ne!(copy["id"], web["id"], "{copy}");
    assert_eq!(copy["spec"]["disks"][0]["volume"], "copy-disk0", "{copy}");
    assert_eq!(copy["spec"]["memory_mib"], 256, "{copy}");
    assert_eq!(copy["spec"]["power"], "stopped", "{copy}");
    assert_eq!(copy["spec"]["node"], Value::Null, "{copy}");
    let mac = &copy["spec"]["nics"][0]["mac"];
    assert!(mac.is_string(), "{copy}");
    assert_ne!(mac, &web["spec"]["nics"][0]["mac"], "{copy} {web}");
    let copy_file = volume_file(&d, "copy-disk0").await;
    assert_eq!(head(&d, &copy_file), vec![0xab; MARK]);
    assert!(qemu_img_info(&copy_file)["snapshots"].is_null());
    // UEFI 변수는 백업 때의 사본을 받는다.
    let copy_vars = vars_file(&d, &copy);
    assert_eq!(copy_vars.exists(), uefi);
    if uefi {
        assert_eq!(
            std::fs::read(dir.join("vars.fd")).unwrap(),
            std::fs::read(&copy_vars).unwrap()
        );
    }

    // 복원한 VM이 켜진다.
    let out = d.ctl(&["vm", "start", "copy"]).await.unwrap();
    assert!(out.contains("vm copy: start done"), "{out}");
    let copy = c.get("/vms/copy").await.unwrap();
    assert_eq!(copy["status"]["phase"], "running", "{copy}");
    d.ctl(&["vm", "stop", "copy", "--force"]).await.unwrap();

    // 백업을 지우면 디렉터리가 사라진다.
    let out = d.ctl(&["backup", "delete", &name]).await.unwrap();
    assert_eq!(out, format!("backup {name} deleted\n"));
    assert!(!dir.exists());
    assert_eq!(d.ctl_json(&["backup", "list"]).await, json!([]));

    d.ctl(&["vm", "delete", "copy"]).await.unwrap();
    d.ctl(&["volume", "delete", "copy-disk0"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn stopped_vm_backup_with_raw_disk_restores_both_disks() {
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    setup(&mut d).await;
    let c = client(&d);
    let root = create_volume(&d, "root", "qcow2").await;
    let data = create_volume(&d, "data", "raw").await;
    mark_qcow2(&root, 0xab);
    mark_raw(&data, 0xcd);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--disk", "data", "--start",
    ])
    .await
    .unwrap();

    // dir 풀의 raw 디스크는 스냅샷을 찍을 수 없으므로 실행 중에는 백업하지 않는다.
    let resp = c
        .request(reqwest::Method::POST, "/vms/web/backups")
        .json(&json!({ "pool": "bk" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "vm_running", "{body}");
    assert_eq!(d.ctl_json(&["backup", "list"]).await, json!([]));

    // 꺼진 VM은 된다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    let out = d
        .ctl(&[
            "backup", "create", "web", "--pool", "bk", "--name", "manual",
        ])
        .await
        .unwrap();
    assert_eq!(out, "backup manual of vm web created\n");
    let backup = c.get("/backups/manual").await.unwrap();
    assert_eq!(backup["status"]["phase"], "ready", "{backup}");
    let dir = backup_dir(&d, &backup);
    assert_eq!(head(&d, &dir.join("disk0.qcow2")), vec![0xab; MARK]);
    assert_eq!(head(&d, &dir.join("disk1.qcow2")), vec![0xcd; MARK]);

    // 백업이 남은 풀은 지울 수 없다.
    let err = d.ctl(&["pool", "delete", "bk"]).await.unwrap_err();
    assert!(err.to_string().contains("holds backups"), "{err}");

    // 원본 디스크를 바꿔도 복원한 디스크는 백업 때의 내용이다. 대상 풀(dir)은 qcow2를 받는다.
    mark_qcow2(&root, 0x11);
    mark_raw(&data, 0x22);
    d.ctl(&[
        "backup", "restore", "manual", "--name", "copy", "--pool", "local",
    ])
    .await
    .unwrap();
    let copy = c.get("/vms/copy").await.unwrap();
    let volumes: Vec<&str> = copy["spec"]["disks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["volume"].as_str().unwrap())
        .collect();
    assert_eq!(volumes, ["copy-disk0", "copy-disk1"], "{copy}");
    for (volume, byte) in [("copy-disk0", 0xab), ("copy-disk1", 0xcd)] {
        let v = d.ctl_json(&["volume", "get", volume]).await;
        assert_eq!(v["spec"]["format"], "qcow2", "{v}");
        assert_eq!(v["spec"]["size_bytes"], 64u64 << 20, "{v}");
        assert_eq!(head(&d, &volume_file(&d, volume).await), vec![byte; MARK]);
    }
    let out = d.ctl(&["vm", "start", "copy"]).await.unwrap();
    assert!(out.contains("vm copy: start done"), "{out}");
    d.ctl(&["vm", "stop", "copy", "--force"]).await.unwrap();

    d.ctl(&["backup", "delete", "manual"]).await.unwrap();
    assert!(!dir.exists());
    d.ctl(&["pool", "set", "bk", "--backups", "false"])
        .await
        .unwrap();
    for vm in ["copy", "web"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for volume in ["copy-disk0", "copy-disk1", "root", "data"] {
        d.ctl(&["volume", "delete", volume]).await.unwrap();
    }
    d.stop();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 6은 계획 25(VXLAN 네트워크: opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만들고 다른 노드의 underlay를 FDB에 넣음, 노드 underlay, tap MTU와 `host_mtu`, 완료)와 계획 26(VM 방화벽: 보안 그룹 `SecurityGroup`을 VM NIC에 걸고 노드마다 nftables `bridge opl_<태그>` 테이블로 tap에서 거름, 노드 능력 `firewall`과 스케줄러 조건, 켜기 직전 규칙 넣기, 완료)으로 나눴다(`2026-11-03-network-sdn-design.md`). 하위 프로젝트 6은 끝났다. IPAM·DHCP·라우팅·SNAT, EVPN·BGP, VXLAN 암호화, 그룹을 원격으로 가리키는 규칙, 방화벽 로그, IP·ARP 위조 막기는 넣지 않았다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 6은 계획 25(VXLAN 네트워크: opld가 모든 노드에 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 만들고 다른 노드의 underlay를 FDB에 넣음, 노드 underlay, tap MTU와 `host_mtu`, 완료)와 계획 26(VM 방화벽: 보안 그룹 `SecurityGroup`을 VM NIC에 걸고 노드마다 nftables `bridge opl_<태그>` 테이블로 tap에서 거름, 노드 능력 `firewall`과 스케줄러 조건, 켜기 직전 규칙 넣기, 완료)으로 나눴다(`2026-11-03-network-sdn-design.md`). 하위 프로젝트 6은 끝났다. IPAM·DHCP·라우팅·SNAT, EVPN·BGP, VXLAN 암호화, 그룹을 원격으로 가리키는 규칙, 방화벽 로그, IP·ARP 위조 막기는 넣지 않았다.

구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`, 리더의 스케줄러, 노드당 동시 실행 제한, 보존 규칙, 남음)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 진행 중이다.
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown

### 2.2 `Backup` 리소스
```

을 다음으로 바꾼다:

```markdown

구현(계획 27): `StoragePoolSpec.backups`(serde 기본 false, false면 뺌, OpenAPI 선택 필드)와 `PoolError::BackupsNotSupported`(400 `invalid_request`, "only dir and nfs pools can store backups")는 풀 만들기와 `PATCH /storage-pools/{key}`(`PoolPatch { shared?, backups? }`, 보낸 것만 바꾼다)가 함께 검사한다. `check_no_backups`(opl-api `backups.rs`)가 풀 지우기와 `backups` 끄기를 409 `pool_in_use`("pool {name:?} holds backups")로 막는다 — `pending`·`failed` 백업도 센다. 루트는 driver `pool_root`(dir은 경로, NFS는 `{data_dir}/pools/{풀 id}`)이고 Task는 `check_pool_root`로 NFS 풀이 실제로 마운트됐는지 본다(빈 마운트 지점에 쓰지 않는다). 디렉터리는 `opl_types::backup::backup_dir(root, id)` = `{root}/backups/{id}`.

### 2.2 `Backup` 리소스
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown

### 2.3 백업 Task (`VmBackup`)
```

을 다음으로 바꾼다:

```markdown

구현(계획 27): `opl_types::backup`의 `Backup = Resource<BackupSpec { vm, vm_name, pool, vm_spec, job: Option<Uuid> }, BackupStatus { phase: BackupPhase(Pending|Ready|Failed), created_at: Option<u64>(유닉스 초), disks: Vec<BackupDisk { name, file, virtual_bytes, stored_bytes }>, uefi_vars, error }>`, `Kind::Backup`(`backup`, watch 종류에도 있다). `job`은 계획 28을 위한 자리로 지금은 늘 None(빼고 쓴다). `MANIFEST_VERSION = 1`, `MANIFEST_FILE = "manifest.json"`, `BackupManifest { version, backup, vm, vm_name, vm_spec, disks, uefi_vars, created_at }`(알 수 없는 필드는 받는다), `default_backup_name(vm_name, at)`(VM 이름을 47자까지 줄여 63자를 넘지 않는다). API는 opl-api `backups.rs`: `POST /vms/{key}/backups { pool, name? }`(202 Task `vm-backup`, `pending` 리소스를 먼저 만든다), `GET /backups[?vm=]`(`vm`은 id나 이름, 지운 VM은 백업에 남은 이름으로), `GET /backups/{key}`, `DELETE /backups/{key}`(202 Task `backup-delete`, `failed`도), `POST /backups/{key}/restore { name, pool, node? }`(202 Task `backup-restore`, 대상은 미리 정한 새 VM id). 권한은 설계와 달리 단순하게 했다: 만들기는 VM 쓰기, 지우기·복원은 `Kind::Backup` 쓰기이고 둘 다 operator 이상이다(VM이 지워진 백업도 operator가 지운다). 오류: 백업 풀이 아님 400, 실행 중인데 스냅샷을 담지 못하는 디스크 409 `vm_running`("… stop it to back it up"), 시작·정지 중이면 409 `vm_busy`, 이름 409 `name_taken`, 돌릴 노드가 없으면 409 `no_node`, 진행 중 Task가 있는 백업 지우기·지우는 중 복원 409 `backup_busy`, `ready`가 아닌 백업 복원 409 `backup_not_ready`, 복원할 VM 스펙의 네트워크·보안 그룹이 없으면 400. 진행 중인 `vm-backup`은 VM을 붙잡는다(`check_no_snapshot_task`: 시작·정지·수정·지우기 409 `vm_busy`, 강제 정지는 받는다; 마이그레이션도 바쁨으로 본다).

### 2.3 백업 Task (`VmBackup`)
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown

### 2.4 복원 Task (`BackupRestore`)
```

을 다음으로 바꾼다:

```markdown

구현(계획 27): opl-api `tasks/backup.rs`의 `backup`(VM 큐, 시간 제한 없음)과 `delete`(백업 id 큐, 디렉터리가 없어도 된다), 실패 정리 `discard`. API는 실행 중인 VM이면 그 노드, 꺼진 VM이면 `spec.node`, 없으면 백업 풀과 모든 디스크를 읽을 수 있는 온라인 노드 가운데 이름이 앞선 것을 Task 노드로 고른다. 실행 중 임시 스냅샷은 스냅샷 Task의 태그 찍기를 뺀 `snapshot.rs`의 `take_tags`(qcow2는 메모리 없는 internal snapshot transaction, 블록 디스크는 멈춤·`snapshot_many`·재개)를 쓰고, 태그는 백업 id의 simple 문자열(하이픈 없음)이다. 블록 태그 청소(`sweep_block_tags`)는 Snapshot 리소스가 없는 UUID 모양 태그를 지우되 `pending` 백업의 태그는 둔다. 디스크는 driver `Storage::export_volume(src, force_share, dest)`(`qemu-img convert -c -O qcow2`, 원본을 여는 인자는 복제와 같음, `.part`에 쓰고 이름 바꿈; `Storages`가 모든 백엔드의 `volume_source`를 받는다)로 `disk{i}.qcow2`에, UEFI 변수는 `vars.fd`에, 매니페스트도 `.part` 뒤 이름을 바꿔 쓴다. `stored_bytes`는 파일 크기다. opld는 `TaskRunnerConfig.mount_root`(`{data_dir}/pools`)를 넘긴다. 실제 시험 `crates/opld/tests/backup.rs`(`qemu-it`, macOS HVF와 CI linux-kvm): opld 하나, dir 풀 `local`과 백업 풀 `bk`. 표식을 쓴 qcow2 디스크의 VM을 실행 중에 백업하면 VM은 같은 pid로 돌고 디스크에 태그가 남지 않으며, 백업 디렉터리에 `disk0.qcow2`(backing 없음, 표식 있음)·`manifest.json`(버전 1)·`vars.fd`가 있다. 원본 VM과 볼륨을 지운 뒤 복원한 VM `copy`의 `copy-disk0`에 같은 표식과 백업 때의 UEFI 변수가 있고 켜진다. 백업을 지우면 디렉터리가 사라진다. 두 번째 시험: qcow2와 raw 디스크의 VM은 실행 중 백업이 409 `vm_running`, 꺼진 뒤 백업 → 원본을 바꿔도 복원한 두 디스크(dir 풀이라 qcow2)가 백업 때의 내용, 백업이 남은 풀 지우기 실패.

알려진 틈(계획 27): Task 없이 남은 `pending` 백업(Task를 만들기 전에 실패했거나, 만들기가 실패한 뒤 리소스를 거두는 되돌림마저 실패했거나, Task가 사라진 경우)은 저절로 치우지 않고 그 이름을 붙잡는다 — 지우기로 치운다. 죽었거나 클러스터에서 빠진 노드의 끝나지 않은 백업 Task는 그 백업을 "바쁨"으로 붙잡아 백업 지우기(409 `backup_busy`)와 풀 지우기가 막힐 수 있다 — 그 노드를 되살리거나 Task가 실패로 처리되면 풀린다. 풀의 `shared` 바꾸기와 백업 만들기가 동시에 오면 만들기가 본 `shared`와 Task가 본 값이 달라 백업에 기록된 노드(`status.node`)가 틀릴 수 있는 좁은 틈이 있다. 실행 중 백업이 임시 스냅샷을 지우지 못하면 qcow2 디스크에는 태그가 남을 수 있다(블록 디스크만 태그 청소가 지운다; `qemu-img snapshot -d`로 지운다). 백업 디렉터리의 파일시스템 호출(만들기·복사·지우기)에는 시간 제한이 없어 멈춘 NFS hard 마운트에서는 Task가 멈춘다.

### 2.4 복원 Task (`BackupRestore`)
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown

### 2.5 API·oplctl·웹
```

을 다음으로 바꾼다:

```markdown

구현(계획 27): `tasks/backup.rs`의 `restore(ctx, task, id, name, pool)`(백업 id 큐, 시간 제한 없음 — 같은 백업의 지우기와 겹치지 않는다). 매니페스트의 `version`이 `MANIFEST_VERSION`이 아니거나 `backup`이 백업 id와 다르거나 디스크 수가 VM 스펙과 다르면 실패한다. 디스크 파일 이름은 디렉터리 밖을 가리킬 수 없고(경로 성분이 있으면 실패), 가져오기 검사(`check_import_source`)로 backing file이 있는 이미지를 받지 않는다. 새 볼륨은 `{새 VM 이름}-disk{i}`(복제의 `volume_name`, 겹치면 번호), 형식은 `copy_format`(블록 풀이면 raw, 아니면 qcow2), `status.node`는 이 노드다. 볼륨은 만들기 전에 Task의 `Made`에 적고, 실패하면 복제와 같은 정리가 지운다. VM은 복제에서 뺀 `clone::create_vm(ctx, task, new)`로 맨 마지막에 만든다(새 MAC, 노드·예약 없음, HA 끔, `power: stopped`). 매니페스트 버전 오류는 Task 실패로 알린다(API는 매니페스트를 읽지 않으므로 400이 아니다). 알려진 틈: 볼륨을 `ready`로 만든 뒤 VM을 만들기 전에 opld가 죽으면 볼륨이 남는다(복제도 같다) — `oplctl volume delete`로 지운다.

### 2.5 API·oplctl·웹
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown
- 웹: VM 상세에 백업 탭(목록, 지금 백업, 복원 대화상자), 백업 목록 페이지(VM이 지워진 백업 포함), 풀 만들기·편집의 "백업 저장" 스위치.
```

을 다음으로 바꾼다:

```markdown
- 웹: VM 상세에 백업 탭(목록, 지금 백업, 복원 대화상자), 백업 목록 페이지(VM이 지워진 백업 포함), 풀 만들기·편집의 "백업 저장" 스위치.

구현(계획 27, 리뷰 반영): 공유가 아닌 백업 풀(dir, `shared: false`)의 백업은 쓴 노드에만 있으므로 `BackupStatus.node: Option<Uuid>`(없으면 뺌)를 둔다. 백업 Task는 공유가 아닌 풀이면 디렉터리를 만들기 전에 자기 노드를 적고(실패한 백업도 그 노드에서 지운다), 공유 풀이면 비워 둔다. API는 `status.node`가 있으면 지우기·복원 Task를 그 노드에 둔다: 복원 본문의 `node`가 다르면 409 `backup_on_other_node`("backup {name:?} is stored on node {n:?}"), 그 노드가 오프라인이면 409 `node_offline`(복원 본문의 `node`도 같다), 그 노드에서 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`. 지우기 Task는 디렉터리가 없을 때 공유 풀이거나 `status.node`가 없거나 이 노드면 이미 지운 것으로 보고, 다른 노드면 실패한다. 백업이 남은 풀의 `shared` 바꾸기도 409 `pool_in_use`. 풀이 저장소에서 사라진 백업의 지우기는 이 노드의 Task가 리소스만 지우고(경고 로그), 풀은 있는데 파일을 지울 노드가 없으면 409이되 `DELETE /backups/{key}?force=true`면 API가 리소스만 바로 지우고 200 `{ message }`(파일은 남는다고 알린다). 만들기는 리소스를 만든 뒤 풀을 다시 읽어 없어졌거나 `backups`가 꺼졌으면 리소스를 지우고 409 `pool_unavailable`(풀 지우기와 엇갈림). 실행 중 VM 백업 요청은 스냅샷 API의 `check_disks`로 방식이 섞인 디스크를 409 `snapshot_unsupported`로 막는다. `GET /backups?vm=NAME`은 지금 그 이름의 VM 백업과 `vm_name == NAME`인 백업을 함께 준다. 쓰기 보장: `export_volume`은 `qemu-img convert -t writeback`(닫을 때 flush), `vars.fd`와 매니페스트 `.part`는 `sync_all`, 백업 디렉터리를 만든 뒤 `backups` 디렉터리와 풀 루트, 이름을 바꾼 뒤 백업 디렉터리도 fsync하고 나서 `ready`. 블록 디스크를 위해 멈춘 VM을 재개하지 못하면 한 번 더 해 보고, 그래도 실패하면 내보내기 전에 실패한다(임시 태그는 지운다). 블록 태그 청소는 끝나지 않은 `VmBackup` Task가 있는 `pending` 백업의 태그만 둔다. 지우기 Task는 같은 백업의 끝나지 않은 복원 Task가 클러스터에 있으면 실패한다("backup is being restored"). 복원은 백업 리소스의 `vm_spec`으로 VM을 만들고, 매니페스트의 `vm_spec`·디스크 수가 다르면 실패("backup manifest does not match the backup record"), 새 스펙은 `VmSpec::validate`를 통과해야 하며, 백업 디렉터리의 디스크·`vars.fd`는 `symlink_metadata`로 일반 파일인지 본다. 다른 노드가 쓴 백업은 복원 Task도 거절한다.

구현(계획 27): oplctl `backup create VM --pool P [--name N]`(출력 "backup {name} of vm {vm} created"), `backup list [--vm VM]`(열 `NAME VM POOL PHASE CREATED SIZE`, SIZE는 `ready`일 때 `stored_bytes`의 합), `backup get`, `backup delete`, `backup restore BACKUP --name NEW --pool P [--node N]`(출력 "vm {NEW} restored from backup {BACKUP}"). 만들기·복원은 Task를 24시간(`BACKUP_WAIT`)까지 기다린다. `pool create … --backups`, `pool set NAME [--shared|--no-shared] [--backups true|false]`. 웹은 사이드바 "백업" 화면(`/backups`, 모든 백업, VM이 지워졌으면 이름만), VM 상세의 "백업" 탭(목록·지금 백업(백업 풀 고르기)·복원 대화상자), 스토리지 풀 화면의 "백업 저장" 스위치, Task 종류 문구, watch 종류 `backup`, ko·en 문구.
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 976개, 실패 0

Run: `cargo test -p opld --features qemu-it --test backup`
Expected: PASS 2개(이 Mac, 실제 QEMU)

- [ ] **Step 3: 커밋**

```bash
git add README.md crates docs
git commit -m "test(opld): 백업 E2E와 문서 — README 백업 절, 설계 "구현(계획 27)" 메모, 하위 프로젝트 7 진행 중"
```

---

## 완료 기준 (계획 27)

- 백업 풀에 실행 중·멈춘 VM을 백업하면 디스크별 압축 qcow2·UEFI 변수·매니페스트가 생기고 `ready`가 되며, 실행 중 VM은 멈추지 않고(블록 백엔드는 짧게) 임시 스냅샷이 남지 않는다.
- VM을 지워도 백업은 남고, 복원하면 같은 디스크 내용의 꺼진 새 VM이 생겨 켜진다. 실패한 백업·복원은 파일·볼륨을 남기지 않는다.
- 백업이 남은 풀은 지우거나 백업 표시를 끄거나 `shared`를 바꿀 수 없다.
- 이 Mac과 Linux CI(`linux-kvm`)에서 `opld`의 `backup` 실제 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **crash-consistent:** 게스트 파일 시스템을 얼리지 않는다. 데이터베이스는 게스트 안에서 일관성을 맞춘다.
- **매번 전체 백업:** 증분·중복 제거가 없어 백업마다 VM 디스크 크기만큼(압축 뒤) 쓴다.
- **알려진 틈:** Task 없이 남은 `pending` 백업(지우기로 치움), qcow2 임시 태그가 남을 수 있음, 죽은 노드의 끝나지 않은 백업 Task가 백업을 붙잡음, 복원·복제 도중 opld가 죽으면 볼륨이 남을 수 있음 — 설계 문서 "구현(계획 27)" 메모와 README에 있다.
