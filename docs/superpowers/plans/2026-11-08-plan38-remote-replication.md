# opLite 계획 38: 원격 복제(DR) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 백업을 다른 사이트의 opLite 클러스터로 복제해, 주 사이트를 잃어도 DR 클러스터에서 VM을 복원할 수 있게 한다. 백업 작업에 원격 복제 대상을 두면 작업이 만든 백업마다 원격(계획 37)으로 보내고, 원본을 지우면 사본도 지운다. 수동 복제, oplctl, 웹. 하위 프로젝트 10의 마지막 계획이며, 이로써 처음 분해한 하위 프로젝트 1~10이 모두 끝난다.

**Architecture:**
- **받는 쪽:** 받기 시작 → 파일 업로드(스트리밍, 해시 기록) → 완료의 세 API. 사본은 그 클러스터의 보통 백업(출처 표시)이라 거기서 목록·복원·지우기가 된다.
- **보내는 쪽:** `backup-replicate` Task가 원본 파일을 읽으며 SHA-256을 계산해 보내고 원격이 기록한 해시와 비교한다. 결과는 원본의 `replicas`에 적고, 보낸 해시를 원본 디스크 기록에 남겨 다음 확인은 GET과 해시 비교로 끝난다.
- **작업과 지우기:** 리더의 백업 작업 스케줄러가 복제·다시 하기·확인을 맡고(노드당 하나), 원본 지우기가 원격 사본을 지운다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, tokio, sha2, 계획 37의 원격 클라이언트, clap. 웹은 React·TypeScript·react-query·vitest·msw. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-06-enterprise-design.md` (§6과 §6.6 "구현(계획 38)")
**선행 계획:** 계획 1~37 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 받는 쪽 API, 복제 Task, 수동·작업 복제, 지우기 전파 | 증분 전송, 대역폭 제한, 압축·암호화 전송 |
| 사본 확인(출처·풀·VM 스펙·크기·해시) | 디스크 연속 복제(낮은 RPO), 자동 장애 조치·복구 계획 |
| oplctl·웹, 두 클러스터 E2E | DR 쪽 지우기 지연 |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험. 두 클러스터 시험은 127.0.0.1의 HTTPS. opld E2E `replicate.rs`는 qemu-img가 필요해 기능 `qemu-it` 뒤에 있다(이 Mac에 있으면 돌리고, CI linux-kvm 작업이 돌린다).

## 설계 결정

- **같은 사본:** 출처(CA 지문·원본 id)와 풀이 같아야 같은 사본. 이미 있는 사본에 붙기 전에 VM 스펙·크기·해시를 비교해, 위조되거나 다른 사본에 붙지 않는다.
- **긴 전송:** 받는 쪽은 128 MiB마다 sync하고 조각 사이 60초를 기다리며, 보내는 쪽은 파일 업로드의 응답을 120초까지 기다린다. 받는 중인 사본은 마지막 활동에서 24시간 뒤 정리된다.
- **지우기 순서:** 원본의 로컬 파일을 먼저 지우고 원격 사본을 지운다(원격 실패는 경고).
- **두 클러스터는 같은 판:** 모르는 필드는 400.

## 파일 구조

```
crates/opl-types/src/{backup.rs, backup_job.rs, task.rs}
crates/opl-api/src/{backup_receive.rs, backups.rs, backup_jobs.rs, janitor.rs, tasks.rs, tasks/backup.rs, tasks/replicate.rs, remote_client.rs, state.rs, lib.rs}, crates/opl-api/tests/{backup_receive.rs, backup_replicate.rs, backup_jobs.rs, …}
crates/oplctl/src/cli.rs, crates/opld/{src/member.rs, tests/replicate.rs}
web/src/backups/{BackupTable.tsx, BackupJobsPage.tsx, RemoteTargetFields.tsx, jobs.ts}, web/src/{remotes/remotes.ts, tasks/tasks.ts, i18n/*.json}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. 각 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다.

---

### Task 1: 원격 복제 타입

`BackupSpec.origin`(받은 사본의 출처: 보낸 클러스터 이름·CA 지문·원본 id·작업), `BackupPhase::Receiving`, `BackupStatus.replicas`(원격·풀·사본 id·상태 copying·ready·failed·오류·시각)와 `received_at`, `BackupDisk.sha256`, `BackupJobSpec.replicate`(원격·풀), `TaskOp::BackupReplicate`. 새 값은 기본값이면 직렬화하지 않고 쓰기 전 `require_current`. 웹 백업 표의 `receiving` 배지.

**Files:**
- Modify: `crates/opl-api/src/backup_jobs.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/tests/backup_job_runs.rs`
- Modify: `crates/opl-api/tests/backup_jobs.rs`
- Modify: `crates/opl-api/tests/backups.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-types/src/backup.rs`
- Modify: `crates/opl-types/src/backup_job.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `web/src/backups/BackupTable.tsx`
- Modify: `web/src/backups/BackupsPage.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/backup_jobs.rs`에서

```rust
use crate::backups::{check_backup, now, start_backup, start_backup_delete};
use crate::error::{ApiError, Json};
```

을 다음으로 바꾼다:

```rust
use crate::backups::{check_backup, now, start_backup, start_backup_delete};
use crate::cluster::require_current;
use crate::error::{ApiError, Json};
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다.
fn check_spec(state: &AppState, spec: &mut BackupJobSpec) -> Result<(), ApiError> {
```

을 다음으로 바꾼다:

```rust

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다. 원격 복제를
/// 켠 작업은 옛 노드가 풀지 못하므로 모든 노드가 이 빌드일 때만 쓴다(409 `cluster_upgrading`).
fn check_spec(state: &AppState, spec: &mut BackupJobSpec) -> Result<(), ApiError> {
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
        )));
    }
```

을 다음으로 바꾼다:

```rust
        )));
    }
    if spec.replicate.is_some() {
        require_current(&state.cluster)?;
    }
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
                job,
            },
```

을 다음으로 바꾼다:

```rust
                job,
                origin: None,
            },
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
                },
                keep: Keep::default(),
                enabled: true,
            },
        );
        job.status = running(vec![a, b], vec![], vec![]);
```

을 다음으로 바꾼다:

```rust
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        job.status = running(vec![a, b], vec![], vec![]);
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
                },
                keep: Keep::default(),
                enabled: true,
            },
        );
        // 실행은 NOW - 60에 시작했다.
```

을 다음으로 바꾼다:

```rust
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        // 실행은 NOW - 60에 시작했다.
```

`crates/opl-api/src/backups.rs`에서

```rust
            job,
        },
```

을 다음으로 바꾼다:

```rust
            job,
            origin: None,
        },
```

`crates/opl-api/src/tasks.rs`에서

```rust
        }
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
```

을 다음으로 바꾼다:

```rust
        }
        // 아직 보내는 쪽이 없다. 이 Task를 만드는 API가 없으므로 오지 않는다.
        TaskOp::BackupReplicate { .. } => Err("backup replication is not supported yet".into()),
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
            stored_bytes,
        });
```

을 다음으로 바꾼다:

```rust
            stored_bytes,
            sha256: None,
        });
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
        node,
    };
```

을 다음으로 바꾼다:

```rust
        node,
        replicas: Vec::new(),
        received_at: None,
    };
```

`crates/opl-api/tests/backup_job_runs.rs`에서

```rust
            job: Some(id),
        },
```

을 다음으로 바꾼다:

```rust
            job: Some(id),
            origin: None,
        },
```

`crates/opl-api/tests/backup_jobs.rs`에서

```rust
use opl_types::backup_job::{BackupJobSpec, JobRun, JobSchedule, Weekday, next_run};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_types::backup_job::{BackupJobSpec, JobRun, JobSchedule, Weekday, next_run};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/backup_jobs.rs`에서

```rust
        "name_taken",
    );
}
```

을 다음으로 바꾼다:

```rust
        "name_taken",
    );
}

/// 원격 복제 대상은 이름 규칙을 따라야 하고(400), 옛 노드가 남아 있으면 쓰지 않는다(409).
#[tokio::test]
async fn replicate_is_checked_and_waits_for_every_node_to_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let with = |target: Value| {
        let mut spec = nightly();
        spec["replicate"] = target;
        job("nightly", spec)
    };
    for bad in [
        json!({ "remote": "DR", "pool": "backups" }),
        json!({ "remote": "dr", "pool": "" }),
        json!({ "remote": "dr" }),
    ] {
        let got = admin.post("/backup-jobs", with(bad.clone())).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{bad} -> {}", got.1);
    }

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    let target = json!({ "remote": "dr", "pool": "backups" });
    assert_error(
        &admin.post("/backup-jobs", with(target.clone())).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    // 복제가 없는 작업은 옛 노드가 읽는 모양 그대로라 그대로 만든다.
    let body = create(&admin, "plain", nightly()).await;
    assert!(body["spec"].get("replicate").is_none(), "{body}");

    let mut node = env.cluster.get_by_name::<NodeSpec>("old").unwrap().unwrap();
    node.status.version = Some(opl_types::version::BUILD.into());
    env.cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();
    let body = create(&admin, "nightly", with(target.clone())["spec"].clone()).await;
    assert_eq!(body["spec"]["replicate"], target);
}
```

`crates/opl-api/tests/backup_jobs.rs`에서

```rust
            job: Some(id),
        },
```

을 다음으로 바꾼다:

```rust
            job: Some(id),
            origin: None,
        },
```

`crates/opl-api/tests/backups.rs`에서

```rust

    // 만드는 중이거나 실패한 백업은 되살리지 못한다.
    for phase in [BackupPhase::Pending, BackupPhase::Failed] {
        set_phase(&env, id, phase).await;
```

을 다음으로 바꾼다:

```rust

    // 만드는 중이거나 실패했거나 아직 받는 중인 백업은 되살리지 못한다.
    for phase in [
        BackupPhase::Pending,
        BackupPhase::Failed,
        BackupPhase::Receiving,
    ] {
        set_phase(&env, id, phase).await;
```

`crates/opl-api/tests/backups.rs`에서

```rust
            job: None,
        },
```

을 다음으로 바꾼다:

```rust
            job: None,
            origin: None,
        },
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust
                job: None,
            },
```

을 다음으로 바꾼다:

```rust
                job: None,
                origin: None,
            },
```

`crates/opl-types/src/backup.rs` 전체:

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
    /// 다른 클러스터에서 받은 사본이면 그 원본. 이 클러스터에서 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub origin: Option<BackupOrigin>,
}

/// 받은 사본의 원본: 보낸 클러스터와 그 클러스터의 백업
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupOrigin {
    /// 보낸 클러스터 이름
    pub cluster: String,
    /// 보낸 클러스터 CA의 지문(`sha256:…`). 같은 원본의 사본을 알아보는 데 쓴다.
    pub ca_fingerprint: String,
    /// 원본 백업 id
    pub backup: Uuid,
    /// 원본을 만든 백업 작업 이름. 손으로 만든 백업이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub job: Option<String>,
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
    /// 원격 클러스터에 보낸 사본. 없으면 뺀다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(
        feature = "openapi",
        schema(value_type = Option<Vec<BackupReplica>>, nullable = false, default)
    )]
    pub replicas: Vec<BackupReplica>,
    /// 받는 중인 사본이 마지막으로 파일을 받은 시각(유닉스 초). 받은 적이 없으면 뺀다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub received_at: Option<u64>,
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
    /// 다른 클러스터에서 파일을 받는 중
    Receiving,
}

/// 원격 클러스터의 백업 풀에 보낸 사본 하나. (remote, pool)마다 하나다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupReplica {
    /// 원격 이름
    pub remote: String,
    /// 원격의 백업 풀 이름
    pub pool: String,
    /// 원격에 만든 사본의 id. 만들기 전이면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub backup: Option<Uuid>,
    pub state: ReplicaState,
    /// 실패한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 상태가 바뀐 시각(유닉스 초)
    pub at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ReplicaState {
    /// 보내는 중
    Copying,
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
    /// 받은 사본이면 받은 파일의 SHA-256(16진수 소문자). 이 클러스터에서 만든 백업이면 뺀다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub sha256: Option<String>,
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
        assert_eq!(spec.origin, None);
        let json = serde_json::to_value(&spec).unwrap();
        assert!(json.get("job").is_none(), "{json}");
        assert!(json.get("origin").is_none(), "{json}");
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
                sha256: None,
            }],
            uefi_vars: true,
            error: None,
            node: None,
            replicas: vec![],
            received_at: None,
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
    fn spec_round_trips_its_origin() {
        let backup = Uuid::now_v7();
        let fp = format!("sha256:{}", "ab".repeat(32));
        let json = serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "dr",
            "vm_spec": vm_spec(),
            "origin": { "cluster": "main", "ca_fingerprint": fp, "backup": backup, "job": "nightly" }
        });
        let spec: BackupSpec = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            spec.origin,
            Some(BackupOrigin {
                cluster: "main".into(),
                ca_fingerprint: fp.clone(),
                backup,
                job: Some("nightly".into()),
            })
        );
        assert_eq!(serde_json::to_value(&spec).unwrap(), json);

        // 손으로 만든 원본이면 job을 뺀다.
        let manual = BackupSpec {
            origin: Some(BackupOrigin {
                job: None,
                ..spec.origin.clone().unwrap()
            }),
            ..spec
        };
        let json = serde_json::to_value(&manual).unwrap();
        assert!(json["origin"].get("job").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupSpec>(json).unwrap(), manual);

        let bad = serde_json::json!({ "cluster": "main", "ca_fingerprint": fp, "backup": backup, "jobs": "x" });
        assert!(serde_json::from_value::<BackupOrigin>(bad).is_err());
    }

    #[test]
    fn status_round_trips_receiving_and_replicas() {
        let receiving: BackupStatus =
            serde_json::from_value(serde_json::json!({ "phase": "receiving" })).unwrap();
        assert_eq!(receiving.phase, BackupPhase::Receiving);
        assert_eq!(
            serde_json::to_value(BackupPhase::Receiving).unwrap(),
            "receiving"
        );

        // 사본이 없으면 replicas를 빼서 옛 노드가 읽는 모양 그대로다.
        let json = serde_json::to_value(BackupStatus::default()).unwrap();
        assert!(json.get("replicas").is_none(), "{json}");
        assert_eq!(
            json,
            serde_json::json!({
                "phase": "pending",
                "created_at": null,
                "disks": [],
                "uefi_vars": false,
                "error": null
            })
        );

        let copy = Uuid::now_v7();
        let status = BackupStatus {
            phase: BackupPhase::Ready,
            replicas: vec![
                BackupReplica {
                    remote: "dr".into(),
                    pool: "backups".into(),
                    backup: Some(copy),
                    state: ReplicaState::Ready,
                    error: None,
                    at: 1_790_000_000,
                },
                BackupReplica {
                    remote: "far".into(),
                    pool: "backups".into(),
                    backup: None,
                    state: ReplicaState::Failed,
                    error: Some("remote unreachable".into()),
                    at: 1_790_000_001,
                },
            ],
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["replicas"],
            serde_json::json!([
                { "remote": "dr", "pool": "backups", "backup": copy, "state": "ready", "at": 1_790_000_000u64 },
                { "remote": "far", "pool": "backups", "state": "failed", "error": "remote unreachable", "at": 1_790_000_001u64 }
            ])
        );
        assert_eq!(
            serde_json::from_value::<BackupStatus>(json).unwrap(),
            status
        );
        assert_eq!(
            serde_json::to_value(ReplicaState::Copying).unwrap(),
            "copying"
        );
    }

    #[test]
    fn received_copies_keep_file_hashes_and_the_last_activity() {
        let hash = "ab".repeat(32);
        let json = serde_json::json!({
            "phase": "receiving",
            "created_at": 1_790_000_000u64,
            "disks": [{
                "name": "disk0",
                "file": "disk0.qcow2",
                "virtual_bytes": 1u64 << 20,
                "stored_bytes": 3u64,
                "sha256": hash
            }],
            "uefi_vars": false,
            "error": null,
            "received_at": 1_790_000_100u64
        });
        let status: BackupStatus = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(status.received_at, Some(1_790_000_100));
        assert_eq!(status.disks[0].sha256.as_deref(), Some(hash.as_str()));
        assert_eq!(serde_json::to_value(&status).unwrap(), json);

        // 없으면 빼서 옛 노드가 읽는 모양 그대로다.
        let plain = BackupStatus {
            disks: vec![BackupDisk {
                sha256: None,
                ..status.disks[0].clone()
            }],
            received_at: None,
            ..status
        };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("received_at").is_none(), "{json}");
        assert!(json["disks"][0].get("sha256").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BackupStatus>(json).unwrap(), plain);
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
                sha256: None,
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

`crates/opl-types/src/backup_job.rs`에서

```rust

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::Vm;
```

을 다음으로 바꾼다:

```rust

use crate::resource::{Kind, Resource, ResourceSpec, validate_name};
use crate::vm::Vm;
```

`crates/opl-types/src/backup_job.rs`에서

```rust
    pub enabled: bool,
}
```

을 다음으로 바꾼다:

```rust
    pub enabled: bool,
    /// 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub replicate: Option<ReplicateTarget>,
}

/// 백업을 보낼 곳: 원격 `remote`의 백업 풀 `pool`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplicateTarget {
    /// 원격 이름
    pub remote: String,
    /// 원격의 백업 풀 이름
    pub pool: String,
}
```

`crates/opl-types/src/backup_job.rs`에서

```rust
        }
        Ok(())
```

을 다음으로 바꾼다:

```rust
        }
        if let Some(target) = &self.replicate {
            if validate_name(&target.remote).is_err() {
                return Err(BackupJobError::ReplicateRemote(target.remote.clone()));
            }
            if validate_name(&target.pool).is_err() {
                return Err(BackupJobError::ReplicatePool(target.pool.clone()));
            }
        }
        Ok(())
```

`crates/opl-types/src/backup_job.rs`에서

```rust
    DuplicateDay(&'static str),
}
```

을 다음으로 바꾼다:

```rust
    DuplicateDay(&'static str),
    #[error("replicate.remote {0:?} is not a valid remote name")]
    ReplicateRemote(String),
    #[error("replicate.pool {0:?} is not a valid storage pool name")]
    ReplicatePool(String),
}
```

`crates/opl-types/src/backup_job.rs`에서

```rust
    #[test]
    fn selector_is_exactly_one_of_three_shapes() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn replicate_round_trips_and_is_left_out_when_off() {
        // 없으면 빼서 옛 노드가 읽는 모양 그대로다.
        let s = spec(base()).unwrap();
        assert_eq!(s.replicate, None);
        assert!(serde_json::to_value(&s).unwrap().get("replicate").is_none());

        let mut with = base();
        with["replicate"] = json!({ "remote": "dr", "pool": "backups" });
        let s = spec(with).unwrap();
        assert_eq!(
            s.replicate,
            Some(ReplicateTarget {
                remote: "dr".into(),
                pool: "backups".into()
            })
        );
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["replicate"],
            json!({ "remote": "dr", "pool": "backups" })
        );
        assert_eq!(spec(json).unwrap(), s);

        for bad in [
            json!({ "remote": "dr" }),
            json!({ "remote": "dr", "pool": "backups", "node": "a" }),
        ] {
            let mut j = base();
            j["replicate"] = bad.clone();
            assert!(spec(j).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_replicate_names() {
        let with = |remote: &str, pool: &str| {
            let mut s = spec(base()).unwrap();
            s.replicate = Some(ReplicateTarget {
                remote: remote.into(),
                pool: pool.into(),
            });
            s.validate()
        };
        assert_eq!(with("dr-1", "backups"), Ok(()));
        assert_eq!(
            with("", "backups"),
            Err(BackupJobError::ReplicateRemote(String::new()))
        );
        assert_eq!(
            with("DR", "backups"),
            Err(BackupJobError::ReplicateRemote("DR".into()))
        );
        assert_eq!(
            with("dr", "../x"),
            Err(BackupJobError::ReplicatePool("../x".into()))
        );
        assert_eq!(
            BackupJobError::ReplicatePool("a/b".into()).to_string(),
            "replicate.pool \"a/b\" is not a valid storage pool name"
        );
    }

    #[test]
    fn selector_is_exactly_one_of_three_shapes() {
```

`crates/opl-types/src/task.rs`에서

```rust
        name: String,
        pool: String,
```

을 다음으로 바꾼다:

```rust
        name: String,
        pool: String,
    },
    /// 대상 백업(`ready`)을 원격 `remote`의 백업 풀 `pool`에 사본으로 보낸다. 백업 풀을 읽을 수 있는
    /// 노드에서 돈다.
    BackupReplicate {
        remote: String,
        pool: String,
```

`crates/opl-types/src/task.rs`에서

```rust
                }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
```

을 다음으로 바꾼다:

```rust
                }),
            ),
            (
                TaskOp::BackupReplicate {
                    remote: "dr".into(),
                    pool: "backups".into(),
                },
                serde_json::json!({
                    "kind": "backup-replicate",
                    "remote": "dr",
                    "pool": "backups"
                }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&op).unwrap(), json);
```

`web/src/backups/BackupTable.tsx`에서

```tsx
  failed: "destructive",
} as const;
```

을 다음으로 바꾼다:

```tsx
  failed: "destructive",
  receiving: "secondary",
} as const;
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
  expect(await job("web-1")).toHaveTextContent("—");
});
```

을 다음으로 바꾼다:

```tsx
  expect(await job("web-1")).toHaveTextContent("—");
});

it("shows a backup still being received and does not restore it", async () => {
  const copy = backup("web-copy", web, 3, { status: { phase: "receiving" } });
  mockLists({ vm: [web], backup: [copy] });
  renderPage(<BackupsPage />);
  const row = (await screen.findByText("web-copy")).closest("tr")!;
  expect(row).toHaveTextContent("Receiving");
  expect(within(row).getByRole("button", { name: "Restore web-copy" })).toBeDisabled();
});
```

`web/src/i18n/en.json`에서

```json
      "ready": "Ready",
      "failed": "Failed"
    },
```

을 다음으로 바꾼다:

```json
      "ready": "Ready",
      "failed": "Failed",
      "receiving": "Receiving"
    },
```

`web/src/i18n/ko.json`에서

```json
      "ready": "준비됨",
      "failed": "실패"
    },
```

을 다음으로 바꾼다:

```json
      "ready": "준비됨",
      "failed": "실패",
      "receiving": "받는 중"
    },
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1478개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web test`
Expected: 모두 통과

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): 원격 복제 타입 — opl-types backup.rs: BackupSpec.origin(Option<BackupOrigin{cluster, ca_fingerprint, backup, job(원본 작업 이름)}>, 없으면 직렬화하지 않음, deny_unknown_fields), BackupPhase::Receiving(\"receiving\"), BackupStatus.replicas(Vec<BackupReplica{remote, pool, backup?, state: ReplicaState{copying, ready, failed}, error?, at}>, 비면 직렬화하지 않고 API 문서에서도 선택 필드). backup_job.rs: BackupJobSpec.replicate(Option<ReplicateTarget{remote, pool}>, 없으면 직렬화하지 않음)와 검증(원격·풀 이름 규칙 — BackupJobError::ReplicateRemote·ReplicatePool). task.rs: TaskOp::BackupReplicate{remote, pool}(\"backup-replicate\"). 최소 처리: 작업 실행기는 아직 BackupReplicate를 실패로 끝냄(만드는 API가 없음), 백업 작업 만들기·고치기는 replicate가 있으면 require_current(409 cluster_upgrading), 웹 백업 표에 받는 중 배지(ko·en). receiving 백업은 복원(ready만)·작업 보존(작업의 백업만)에서 이미 빠짐. schema.d.ts 다시 만듦. 시험: JSON 왕복과 기본값이면 옛 모양 그대로, origin 모르는 필드 거절, replicate 검증, API — replicate 이름 400·옛 노드 409·복제 없는 작업은 옛 노드가 있어도 만듦, receiving 백업 복원 409, 웹 받는 중 표시와 복원 막힘"
```

---

### Task 2: 백업 사본 받기 API

받는 쪽: `POST /api/v1/backups/receive`(풀·매니페스트·출처 — 같은 출처·풀의 사본이 있으면 409 `replica_exists`, 공간이 모자라면 507), `PUT /api/v1/backups/{key}/files/{file}`(정해진 파일 이름만, `Content-Length`가 매니페스트 크기와 같아야, 128 MiB마다 sync, 조각 사이 60초, 파일별 잠금, 해시와 `received_at` 기록), `POST /api/v1/backups/{key}/actions/complete`(모든 파일이 있으면 매니페스트를 받는 쪽 id로 쓰고 ready). 받는 중인 사본은 마지막 활동에서 24시간 뒤 janitor가 지운다.

**Files:**
- Create: `crates/opl-api/src/backup_receive.rs`
- Create: `crates/opl-api/tests/backup_receive.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/janitor.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/state.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/tests/backup_tasks.rs`
- Modify: `crates/opl-api/tests/janitor.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-types/src/backup.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/backup_receive.rs`를 만든다:

```rust
//! 다른 클러스터가 보내는 백업 사본 받기(엔터프라이즈 설계 §6.1). 받기가 `receiving` 백업과 그
//! 디렉터리를 만들고, 파일마다 `PUT`으로 받아 SHA-256을 돌려주고, 완료가 매니페스트를 써서 `ready`로
//! 바꾼다. 파일은 API를 받은 노드가 직접 쓰므로 백업 풀은 이 노드에서 쓸 수 있어야 한다. 모두 백업
//! 쓰기 권한이다.

use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::check_pool_root;
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupOrigin, BackupPhase, BackupSpec, BackupStatus,
    MANIFEST_VERSION, backup_dir, default_backup_name,
};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::storage::{StoragePool, is_shared};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_not_deleting, find_pool, node_name, now};
use crate::cluster::require_current;
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;
use crate::tasks::backup::{VARS_FILE, sync_path, write_manifest};
use crate::volumes::{check_pool_on, content_length};

/// 받는 동안 이만큼 쓸 때마다 디스크에 내려 써서 끝의 내려 쓰기가 짧게 한다.
const SYNC_EVERY: u64 = 128 << 20;
/// `vars.fd`의 최대 크기. 디스크 파일과 달리 매니페스트에 크기가 없다.
const MAX_VARS_BYTES: u64 = 64 << 20;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveRequest {
    /// 받는 쪽 백업 이름. 없으면 `{VM 이름}-{만든 시각}`이고, 겹치면 뒤에 `-2`, `-3`…을 붙인다.
    pub name: Option<String>,
    /// 받을 백업 풀. 이 노드에서 쓸 수 있어야 한다.
    pub pool: String,
    /// 원본 백업의 매니페스트(`manifest.json`)
    pub manifest: BackupManifest,
    pub origin: BackupOrigin,
}

/// 받기 시작한 백업
#[derive(Debug, Serialize, ToSchema)]
pub struct ReceivingBackup {
    pub id: Uuid,
    pub name: String,
}

/// 받은 파일
#[derive(Debug, Serialize, ToSchema)]
pub struct ReceivedFile {
    /// 받은 내용의 SHA-256(16진수 소문자)
    pub sha256: String,
    pub size: u64,
}

/// 매니페스트 검사: 이 노드가 읽는 버전, 디스크 수가 VM 스펙과 같고 디스크 `i`가 `disk{i}` 이름과
/// `disk{i}.qcow2` 파일, VM 이름과 스펙이 올바름. 아니면 400.
fn check_manifest(m: &BackupManifest) -> Result<(), ApiError> {
    if m.version != MANIFEST_VERSION {
        return Err(ApiError::bad_request(format!(
            "backup manifest version {} is not supported (this node reads version {MANIFEST_VERSION})",
            m.version
        )));
    }
    validate_name(&m.vm_name)
        .map_err(|e| ApiError::bad_request(format!("invalid VM name in manifest: {e}")))?;
    m.vm_spec
        .validate()
        .map_err(|e| ApiError::bad_request(format!("invalid VM spec in manifest: {e}")))?;
    if m.disks.len() != m.vm_spec.disks.len() {
        return Err(ApiError::bad_request(
            "manifest lists a different number of disks than its VM spec",
        ));
    }
    for (i, disk) in m.disks.iter().enumerate() {
        if disk.name != format!("disk{i}") || disk.file != format!("disk{i}.qcow2") {
            return Err(ApiError::bad_request(format!(
                "disk {i} of the manifest must be disk{i} in file disk{i}.qcow2"
            )));
        }
    }
    Ok(())
}

/// `base`가 쓰이지 않았으면 그것, 아니면 `{base}-2`, `{base}-3`… 가운데 처음 빈 이름. 이름 길이(63자)를
/// 넘지 않게 `base`를 줄인다(끝의 `-`는 뺀다). `base`는 올바른 이름이다.
fn free_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let prefix = base[..base.len().min(63 - suffix.len())].trim_end_matches('-');
            format!("{prefix}{suffix}")
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 이 노드에서 백업 풀 `pool`의 루트. 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn pool_root(state: &AppState, pool: &StoragePool) -> Result<PathBuf, ApiError> {
    check_pool_on(state, pool, state.node, || {
        format!(
            "storage pool {:?} is not available on node {:?} that received the request; register \
             this cluster as a remote with the URL of a node holding the pool, or use a shared \
             (NFS) backup pool",
            pool.name,
            node_name(state, state.node).unwrap_or_else(|_| state.node.to_string())
        )
    })?;
    check_pool_root(pool, &state.mount_root).map_err(|e| {
        ApiError::conflict(
            "pool_unavailable",
            format!("storage pool {:?}: {e}", pool.name),
        )
    })
}

/// 풀에 매니페스트의 디스크 파일을 받을 자리가 없으면 507 `insufficient_storage`
fn check_space(root: &FsPath, manifest: &BackupManifest) -> Result<(), ApiError> {
    let need = manifest
        .disks
        .iter()
        .fold(0u64, |sum, d| sum.saturating_add(d.stored_bytes));
    let available = fs4::available_space(root).map_err(ApiError::internal)?;
    if need > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("the backup pool has {available} bytes free; the backup needs {need} bytes"),
        ));
    }
    Ok(())
}

/// 다른 클러스터의 백업 사본을 받기 시작한다(201): 매니페스트의 VM·스펙·디스크로 `receiving` 백업을
/// 만들고 백업 풀에 그 디렉터리를 만든다. 풀이 없거나 백업 풀이 아니거나 매니페스트가 올바르지 않으면
/// 400, 풀을 이 노드에서 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`, 같은 원본(CA 지문과
/// 원본 id)의 사본이 같은 풀에 있으면 409 `replica_exists`(`details.backup`이 그 id), 풀에 디스크 파일을
/// 받을 자리가 없으면 507 `insufficient_storage`, 옛 노드가 있으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/backups/receive", tag = "backups",
    request_body = ReceiveRequest,
    responses((status = 201, body = ReceivingBackup), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn receive_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<ReceiveRequest>,
) -> Result<(StatusCode, Json<ReceivingBackup>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    // 옛 노드는 받는 중인 백업과 출처를 모른다.
    require_current(&state.cluster)?;
    let pool = find_pool(&state, &req.pool)?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    let root = pool_root(&state, &pool)?;
    let manifest = req.manifest;
    check_manifest(&manifest)?;
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    if let Some(copy) = backups.iter().find(|b| {
        // 같은 원본이라도 다른 풀에 받은 것은 다른 사본이다.
        b.spec.pool == pool.name
            && b.spec.origin.as_ref().is_some_and(|o| {
                o.ca_fingerprint == req.origin.ca_fingerprint && o.backup == req.origin.backup
            })
    }) {
        let mut e = ApiError::conflict(
            "replica_exists",
            format!(
                "backup {:?} is already a copy of backup {} of cluster {:?}",
                copy.name, req.origin.backup, req.origin.cluster
            ),
        );
        e.details = Some(serde_json::json!({ "backup": copy.id }));
        return Err(e);
    }
    check_space(&root, &manifest)?;
    let taken: HashSet<&str> = backups.iter().map(|b| b.name.as_str()).collect();
    let base = req
        .name
        .unwrap_or_else(|| default_backup_name(&manifest.vm_name, manifest.created_at));
    let name = free_name(&base, |n| taken.contains(n));

    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: manifest.vm,
            vm_name: manifest.vm_name,
            pool: pool.name.clone(),
            vm_spec: manifest.vm_spec,
            job: None,
            origin: Some(req.origin),
        },
    );
    backup.status = BackupStatus {
        phase: BackupPhase::Receiving,
        created_at: Some(manifest.created_at),
        // 해시는 이 클러스터가 받은 파일의 것이다.
        disks: manifest
            .disks
            .into_iter()
            .map(|d| BackupDisk { sha256: None, ..d })
            .collect(),
        uefi_vars: manifest.uefi_vars,
        error: None,
        // 공유가 아닌 풀의 파일은 이 노드에만 있다.
        node: (!is_shared(&pool)).then_some(state.node),
        replicas: Vec::new(),
        received_at: None,
    };
    // 그사이 같은 이름이 생겼으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    let dir = backup_dir(&root, backup.id);
    if let Err(e) = create_dir(&root, &dir).await {
        if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
            tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup without a directory");
        }
        return Err(ApiError::internal(format!(
            "cannot create backup directory: {e}"
        )));
    }
    Ok((
        StatusCode::CREATED,
        Json(ReceivingBackup {
            id: backup.id,
            name: backup.name,
        }),
    ))
}

/// 백업 디렉터리를 만들고, 새 이름이 남게 `backups`와 풀 루트도 내려 쓴다.
async fn create_dir(root: &FsPath, dir: &FsPath) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for parent in [root.join("backups"), root.to_path_buf()] {
        sync_path(&parent).await?;
    }
    Ok(())
}

/// 받는 중인 백업의 디렉터리. 받는 중이 아니면 409 `backup_not_receiving`, 파일이 다른 노드에 있으면
/// 409 `backup_on_other_node`, 풀을 이 노드에서 쓸 수 없으면 409.
fn receiving_dir(state: &AppState, backup: &Backup) -> Result<PathBuf, ApiError> {
    if backup.status.phase != BackupPhase::Receiving {
        return Err(ApiError::conflict(
            "backup_not_receiving",
            format!("backup {:?} is not receiving files", backup.name),
        ));
    }
    if let Some(node) = backup.status.node.filter(|n| *n != state.node) {
        return Err(ApiError::conflict(
            "backup_on_other_node",
            format!(
                "backup {:?} is received by node {:?}; send its files there",
                backup.name,
                node_name(state, node)?
            ),
        ));
    }
    let pool = find_pool(state, &backup.spec.pool)?;
    Ok(backup_dir(&pool_root(state, &pool)?, backup.id))
}

/// 백업이 받는 파일 이름과 크기(UEFI 변수는 크기를 모른다)
fn expected_files(backup: &Backup) -> Vec<(&str, Option<u64>)> {
    let mut files: Vec<(&str, Option<u64>)> = backup
        .status
        .disks
        .iter()
        .map(|d| (d.file.as_str(), Some(d.stored_bytes)))
        .collect();
    if backup.status.uefi_vars {
        files.push((VARS_FILE, None));
    }
    files
}

/// 본문을 `part`에 `length`바이트까지 받으며 SHA-256을 계산하고 디스크에 내려 쓴다(긴 파일은
/// [`SYNC_EVERY`]마다). 그보다 길거나 짧으면 400, 조각이 `idle` 넘게 오지 않으면 400(멈춘 업로드).
async fn receive_file(
    part: &FsPath,
    body: Body,
    length: u64,
    idle: std::time::Duration,
) -> Result<String, ApiError> {
    let mut file = match tokio::fs::File::create(part).await {
        Ok(f) => f,
        // 디렉터리가 없다: 그사이 지웠다.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::conflict(
                "backup_not_receiving",
                "the backup directory is gone; the backup was deleted",
            ));
        }
        Err(e) => return Err(ApiError::internal(e)),
    };
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut synced = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = tokio::time::timeout(idle, stream.next())
        .await
        .map_err(|_| ApiError::bad_request("the upload stalled"))?
    {
        let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload failed: {e}")))?;
        received += chunk.len() as u64;
        if received > length {
            return Err(ApiError::bad_request(
                "upload is longer than Content-Length",
            ));
        }
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
        if received - synced >= SYNC_EVERY {
            file.sync_data().await.map_err(ApiError::internal)?;
            synced = received;
        }
    }
    if received != length {
        return Err(ApiError::bad_request("upload ended early"));
    }
    file.sync_all().await.map_err(ApiError::internal)?;
    Ok(hex::encode(hash.finalize()))
}

/// 업로드 본문의 문서용 스키마: 바이너리 파일
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct BackupFile(#[allow(dead_code)] Vec<u8>);

/// 받는 중인 백업의 파일 하나를 받는다(200): `{file}.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고,
/// 받은 내용의 SHA-256과 크기를 돌려주며, 디스크 파일의 해시와 받은 시각(`received_at`)을 백업 상태에
/// 적는다. 같은 파일을 다시 보내면 덮는다. `file`은 매니페스트의 디스크 파일이나(UEFI 변수가 있으면)
/// `vars.fd`여야 하고, `Content-Length`는 디스크 파일이면 매니페스트의 크기, `vars.fd`면 64 MiB까지다
/// (400). 본문 조각이 60초 넘게 오지 않으면 받은 것을 버리고 400(멈춘 업로드). `Content-Length`가 없으면
/// 411, 받는 중이 아니면 409 `backup_not_receiving`, 파일이 다른 노드에 있으면 409 `backup_on_other_node`,
/// 같은 파일을 받고 있으면 409 `backup_busy`.
#[utoipa::path(put, path = "/backups/{key}/files/{file}", tag = "backups",
    params(
        ("key" = String, Path, description = "id or name"),
        ("file" = String, Path, description = "disk file of the manifest or vars.fd"),
    ),
    request_body(content = BackupFile, content_type = "application/octet-stream"),
    responses((status = 200, body = ReceivedFile), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_backup_file(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, file)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<ReceivedFile>, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let length = content_length(&headers)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    let dir = receiving_dir(&state, &backup)?;
    // 이름은 정해진 몇 개뿐이므로 경로 문자를 따로 볼 필요가 없다.
    let Some((_, size)) = expected_files(&backup)
        .into_iter()
        .find(|(f, _)| *f == file)
    else {
        return Err(ApiError::bad_request(format!(
            "backup {:?} has no file {file:?}",
            backup.name
        )));
    };
    match size {
        Some(size) if length != size => {
            return Err(ApiError::bad_request(format!(
                "{file} of backup {:?} is {size} bytes, not {length}",
                backup.name
            )));
        }
        None if length > MAX_VARS_BYTES => {
            return Err(ApiError::bad_request(format!(
                "{file} must not be larger than {MAX_VARS_BYTES} bytes"
            )));
        }
        _ => {}
    }
    let _receiving = ReceivingFile::take(&state, &backup, &file)?;
    let part = dir.join(format!("{file}.part"));
    let sha256 = match receive_file(&part, body, length, state.receive_idle_timeout).await {
        Ok(hash) => hash,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    tokio::fs::rename(&part, dir.join(&file))
        .await
        .map_err(ApiError::internal)?;
    sync_path(&dir).await.map_err(ApiError::internal)?;
    record_file(&state, backup.id, &file, &sha256).await?;
    Ok(Json(ReceivedFile {
        sha256,
        size: length,
    }))
}

/// 이 노드가 받고 있는 (백업, 파일)의 표시. 놓으면 지운다.
struct ReceivingFile<'a> {
    state: &'a AppState,
    key: (Uuid, String),
}

impl<'a> ReceivingFile<'a> {
    /// 같은 파일을 받고 있으면 409 `backup_busy`
    fn take(state: &'a AppState, backup: &Backup, file: &str) -> Result<Self, ApiError> {
        let key = (backup.id, file.to_string());
        if !state
            .receiving_files
            .lock()
            .expect("receiving lock")
            .insert(key.clone())
        {
            return Err(ApiError::conflict(
                "backup_busy",
                format!(
                    "{file} of backup {:?} is being received; try again when that upload has finished",
                    backup.name
                ),
            ));
        }
        Ok(Self { state, key })
    }
}

impl Drop for ReceivingFile<'_> {
    fn drop(&mut self) {
        self.state
            .receiving_files
            .lock()
            .expect("receiving lock")
            .remove(&self.key);
    }
}

/// 받은 파일의 해시(디스크 파일이면)와 받은 시각을 백업 상태에 적는다. 다른 요청이 그사이 쓴 상태를
/// 덮지 않게 읽은 revision에서만 쓰고, 어긋나면 다시 한다. 받는 중이 아니게 됐으면 적지 않는다.
async fn record_file(state: &AppState, id: Uuid, file: &str, sha256: &str) -> Result<(), ApiError> {
    loop {
        let Some(backup) = state.cluster.get::<BackupSpec>(id)? else {
            return Ok(());
        };
        if backup.status.phase != BackupPhase::Receiving {
            return Ok(());
        }
        let mut status = backup.status.clone();
        for disk in status.disks.iter_mut().filter(|d| d.file == file) {
            disk.sha256 = Some(sha256.to_string());
        }
        status.received_at = Some(now());
        match state
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// `path`가 일반 파일이면 그 크기
async fn file_size(path: &FsPath) -> Option<u64> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(m) if m.file_type().is_file() => Some(m.len()),
        _ => None,
    }
}

/// 받기를 끝낸다(200, 백업): 모든 파일이 있고 디스크 파일 크기가 매니페스트와 같으면 매니페스트를
/// (`backup`은 이 클러스터의 백업 id로) 쓰고 `ready`로 바꾼다. 빠지거나 크기가 다른 파일이 있으면 409
/// `backup_incomplete`(`details.files`가 그 파일들), 받는 중이 아니면 409 `backup_not_receiving`, 지우는
/// Task가 걸려 있으면 409 `backup_busy`.
#[utoipa::path(post, path = "/backups/{key}/actions/complete", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses(
        (status = 200, body = Resource<BackupSpec, BackupStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn complete_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Backup>, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    let dir = receiving_dir(&state, &backup)?;
    check_not_deleting(&state, &backup)?;
    let mut missing = vec![];
    for (file, size) in expected_files(&backup) {
        let found = file_size(&dir.join(file)).await;
        if found.is_none() || size.is_some_and(|s| found != Some(s)) {
            missing.push(file.to_string());
        }
    }
    if !missing.is_empty() {
        let mut e = ApiError::conflict(
            "backup_incomplete",
            format!(
                "backup {:?} is missing files or has files of the wrong size: {}",
                backup.name,
                missing.join(", ")
            ),
        );
        e.details = Some(serde_json::json!({ "files": missing }));
        return Err(e);
    }
    // 그사이 받은 파일의 기록을 덮지 않게 읽은 revision에서만 쓰고, 어긋나면 다시 읽은 상태로
    // 매니페스트부터 다시 쓴다.
    let mut current = backup;
    loop {
        // 복원은 매니페스트의 백업 id가 이 백업의 id인지 확인한다.
        let manifest = BackupManifest {
            version: MANIFEST_VERSION,
            backup: current.id,
            vm: current.spec.vm,
            vm_name: current.spec.vm_name.clone(),
            vm_spec: current.spec.vm_spec.clone(),
            disks: current.status.disks.clone(),
            uefi_vars: current.status.uefi_vars,
            created_at: current.status.created_at.unwrap_or_default(),
        };
        write_manifest(&dir, &manifest)
            .await
            .map_err(|e| ApiError::internal(format!("cannot write manifest: {e}")))?;
        let status = BackupStatus {
            phase: BackupPhase::Ready,
            error: None,
            ..current.status.clone()
        };
        match state
            .cluster
            .update_status_checked::<BackupSpec>(current.id, current.revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
        current = lookup::<BackupSpec>(&state.cluster, &current.id.to_string())?;
        receiving_dir(&state, &current)?;
        check_not_deleting(&state, &current)?;
    }
    Ok(Json(lookup::<BackupSpec>(
        &state.cluster,
        &current.id.to_string(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_get_a_number_and_stay_short() {
        assert_eq!(free_name("web", |_| false), "web");
        assert_eq!(free_name("web", |n| n == "web"), "web-2");
        assert_eq!(free_name("web", |n| n == "web" || n == "web-2"), "web-3");
        let long = "a".repeat(63);
        let name = free_name(&long, |n| n == long);
        assert_eq!(name, format!("{}-2", "a".repeat(61)));
        assert_eq!(validate_name(&name), Ok(()));
        // 줄여서 끝이 `-`이면 뺀다.
        let dash = format!("{}-b", "a".repeat(60));
        assert_eq!(
            free_name(&dash, |n| n == dash),
            format!("{}-2", "a".repeat(60))
        );
    }
}
```

`crates/opl-api/src/backups.rs`에서

```rust

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
```

을 다음으로 바꾼다:

```rust

pub(crate) fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
```

`crates/opl-api/src/backups.rs`에서

```rust
    )
}
```

을 다음으로 바꾼다:

```rust
    )
}

/// 백업을 지우는 Task가 끝나지 않았으면 409 `backup_busy`. 받는 중인 사본의 완료가 쓴다.
pub(crate) fn check_not_deleting(state: &AppState, backup: &Backup) -> Result<(), ApiError> {
    if backup_tasks(state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(backup, "being deleted"));
    }
    Ok(())
}
```

`crates/opl-api/src/janitor.rs` 전체:

```rust
//! 클러스터 정리 작업(견고성 설계 §3). 리더만 `tick`마다 노드·Task·스냅샷 목록을 보고, 오프라인이
//! 유예보다 오래 간 노드의 `pending` Task와 제거된 노드의 끝나지 않은 Task를 실패로 끝내고, 만들
//! Task 없이 남은 `pending` 스냅샷을 지운다. 실패로 끝낸 Task의 정리(볼륨·태그)는 하지 않는다.
//! 예약 스냅샷의 만들기·예약 정리의 삭제를 끝냈으면 실행기처럼 `task_failed`로 VM에 남긴다.
//! 끝난 Task는 기간·개수 기준으로 지운다(웹 정리 설계 §2). 오래 `receiving`인 백업(보내던 클러스터가
//! 끝내지 못한 사본)은 지우기 Task를 만들어 파일과 함께 지운다(엔터프라이즈 설계 §6.1).

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::backup::{Backup, BackupPhase, BackupSpec};
use opl_types::node::{Node, NodeSpec};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::backups::start_backup_delete;
use crate::schedule::{recorded_vm, set_schedule_error, task_failed};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct ClusterJanitorConfig {
    /// 목록을 확인하는 간격
    pub tick: Duration,
    /// 노드가 이보다 오래 오프라인이면 그 노드의 `pending` Task를 실패로 끝낸다.
    pub offline_grace: Duration,
    /// 만들 Task 없이 `pending`인 스냅샷을 처음 본 뒤 이만큼 지나도 그대로면 지운다. API는 스냅샷을
    /// 만든 뒤 Task를 만들므로 그 사이(전달된 쓰기는 하나에 최대 `write_timeout`)보다 길어야 한다.
    pub orphan_grace: Duration,
    /// 만든 지 이보다 오래된 끝난 Task를 지운다. 만든 시각은 UUIDv7 id의 시각이다.
    pub task_max_age: Duration,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    pub task_max_finished: usize,
    /// 개수 기준으로는 끝난 것을 처음 본 뒤 이만큼 지난 Task만 지운다. 막 끝난 Task를 기다리는
    /// 쪽(oplctl `wait_task`, 웹 `waitForTask`)이 결과를 읽을 시간이다.
    pub finished_grace: Duration,
    /// 만든 지, 그리고 마지막으로 파일을 받은 지(`received_at`) 이보다 오래 `receiving`인 백업을 지운다.
    /// 만든 시각은 UUIDv7 id의 시각이다.
    pub receiving_max_age: Duration,
}

/// 한 tick에 지우는 끝난 Task의 최대 수. 나머지는 다음 tick에 지운다.
const TASK_DELETES_PER_TICK: usize = 200;

impl Default for ClusterJanitorConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            offline_grace: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(30),
            task_max_age: Duration::from_secs(7 * 24 * 3600),
            task_max_finished: 1000,
            finished_grace: Duration::from_secs(600),
            receiving_max_age: Duration::from_secs(24 * 3600),
        }
    }
}

/// 클러스터 정리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct ClusterJanitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ClusterJanitor {
    pub fn spawn(state: Arc<AppState>, config: ClusterJanitorConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: ClusterJanitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut seen = Seen::default();
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut seen) => {}
        }
    }
}

/// 이전 tick들에서 본 것. 메모리에만 두므로 리더가 바뀌면 유예를 다시 잰다.
#[derive(Debug, Default)]
struct Seen {
    /// 노드가 오프라인으로 처음 보인 시각
    offline_since: HashMap<Uuid, Instant>,
    /// 만들 Task 없이 `pending`인 스냅샷과 처음 그렇게 보인 시각
    orphans: HashMap<Uuid, Instant>,
    /// 끝난 Task와 처음 끝난 것으로 보인 시각
    finished: HashMap<Uuid, Instant>,
}

/// 정리 작업이 할 일. 읽은 revision에서만 쓴다(그사이 바뀌었으면 다음 tick에 다시 본다).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    FailTask {
        id: Uuid,
        revision: u64,
        error: String,
    },
    DeleteSnapshot {
        id: Uuid,
        revision: u64,
    },
    DeleteTask {
        id: Uuid,
        revision: u64,
    },
}

/// 지금 할 일을 정하고 `seen`을 고친다. `members`는 Raft 멤버십의 노드다. 노드 목록에도 멤버십에도
/// 없는 노드를 제거된 것으로 본다(멤버가 되고 노드를 등록하기 전인 노드는 제거된 것이 아니다).
fn decide(
    now: Instant,
    config: &ClusterJanitorConfig,
    nodes: &[Node],
    members: &BTreeSet<Uuid>,
    tasks: &[Task],
    snapshots: &[Snapshot],
    seen: &mut Seen,
) -> Vec<Action> {
    // 다시 온라인이 되었거나 목록에서 빠진 노드는 잊는다.
    seen.offline_since
        .retain(|id, _| nodes.iter().any(|n| n.id == *id && !n.status.online));
    for node in nodes.iter().filter(|n| !n.status.online) {
        seen.offline_since.entry(node.id).or_insert(now);
    }
    let mut actions = vec![];
    for task in tasks.iter().filter(|t| !t.status.state.is_finished()) {
        let node = task.spec.node;
        let error = match nodes.iter().find(|n| n.id == node) {
            None if members.contains(&node) => continue,
            // 제거된 노드는 돌아오지 않으므로 실행 중이던 Task도 끝낸다.
            None => format!("node {node} was removed"),
            // 네트워크만 끊긴 노드는 아직 실행하고 있을 수 있으므로 `running`은 두고, 시작 전인
            // Task만 유예 뒤에 끝낸다.
            Some(_) if task.status.state != TaskState::Pending => continue,
            Some(_) => match seen.offline_since.get(&node) {
                Some(since) if now.duration_since(*since) >= config.offline_grace => {
                    format!("node {node} is offline")
                }
                _ => continue,
            },
        };
        actions.push(Action::FailTask {
            id: task.id,
            revision: task.revision,
            error,
        });
    }
    // API는 스냅샷을 만든 뒤에 Task를 만들므로 그 사이를 고아로 보지 않도록 처음 본 뒤 유예가
    // 지나도 고아일 때 지운다. 그 사이 Task가 보였으면 다시 잰다.
    let orphans: HashMap<Uuid, Instant> = snapshots
        .iter()
        .filter(|s| s.status.phase == SnapshotPhase::Pending)
        .filter(|s| {
            !tasks.iter().any(|t| {
                !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id)
            })
        })
        .map(|s| (s.id, seen.orphans.get(&s.id).copied().unwrap_or(now)))
        .collect();
    for s in snapshots.iter() {
        if orphans
            .get(&s.id)
            .is_some_and(|since| now.duration_since(*since) >= config.orphan_grace)
        {
            actions.push(Action::DeleteSnapshot {
                id: s.id,
                revision: s.revision,
            });
        }
    }
    seen.orphans = orphans;
    actions
}

/// 지울 끝난 Task를 오래된 것부터 최대 `limit`개 고르고 `seen`을 고친다. 만든 지 `task_max_age`가
/// 넘은 것과 `task_max_finished`를 넘는 만큼 오래된 것 중 끝난 것을 처음 본 뒤 `finished_grace`가
/// 지난 것이다. 끝나지 않은 Task는 고르지 않는다.
fn expired_tasks(
    now: SystemTime,
    instant: Instant,
    config: &ClusterJanitorConfig,
    tasks: &[Task],
    seen: &mut Seen,
    limit: usize,
) -> Vec<Action> {
    let mut finished: Vec<(SystemTime, &Task)> = tasks
        .iter()
        .filter(|t| t.status.state.is_finished())
        .map(|t| (created(t.id), t))
        .collect();
    finished.sort_by_key(|(at, t)| (*at, t.id));
    // 지워졌거나 없어진 Task는 잊는다.
    seen.finished = finished
        .iter()
        .map(|(_, t)| (t.id, seen.finished.get(&t.id).copied().unwrap_or(instant)))
        .collect();
    let over = finished.len().saturating_sub(config.task_max_finished);
    let old = finished
        .iter()
        .take_while(|(at, _)| now.duration_since(*at).unwrap_or_default() > config.task_max_age)
        .count();
    let settled = |t: &Task| instant.duration_since(seen.finished[&t.id]) >= config.finished_grace;
    finished
        .iter()
        .enumerate()
        .filter(|(i, (_, t))| (*i < old || *i < over) && settled(t))
        .take(limit)
        .map(|(_, (_, t))| Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        })
        .collect()
}

/// 만든 지, 그리고 마지막으로 파일을 받은 지(`received_at`) `receiving_max_age`가 넘은 `receiving` 백업
/// 가운데 지우는 Task가 없는 것
fn stale_receiving<'a>(
    now: SystemTime,
    config: &ClusterJanitorConfig,
    backups: &'a [Backup],
    tasks: &[Task],
) -> Vec<&'a Backup> {
    backups
        .iter()
        .filter(|b| b.status.phase == BackupPhase::Receiving)
        .filter(|b| {
            let received = b.status.received_at.map_or(SystemTime::UNIX_EPOCH, |t| {
                SystemTime::UNIX_EPOCH + Duration::from_secs(t)
            });
            let active = created(b.id).max(received);
            now.duration_since(active).unwrap_or_default() > config.receiving_max_age
        })
        .filter(|b| {
            !tasks.iter().any(|t| {
                !t.status.state.is_finished()
                    && t.spec.op == TaskOp::BackupDelete
                    && t.spec.target == b.id
            })
        })
        .collect()
}

/// UUIDv7 id에 담긴 만든 시각. 시각이 없는 id는 가장 오래된 것으로 본다.
fn created(id: Uuid) -> SystemTime {
    id.get_timestamp().map_or(SystemTime::UNIX_EPOCH, |ts| {
        let (secs, nanos) = ts.to_unix();
        SystemTime::UNIX_EPOCH + Duration::new(secs, nanos)
    })
}

async fn tick(state: &AppState, config: &ClusterJanitorConfig, seen: &mut Seen) {
    let cluster = &state.cluster;
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        *seen = Seen::default();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let lists = (|| {
        Ok::<_, ClusterError>((
            cluster.list::<NodeSpec>()?.1,
            cluster.list::<TaskSpec>()?.1,
            cluster.list::<SnapshotSpec>()?.1,
            cluster.list::<BackupSpec>()?.1,
        ))
    })();
    let Ok((nodes, tasks, snapshots, backups)) = lists else {
        return;
    };
    let ids: BTreeSet<Uuid> = members
        .voters
        .keys()
        .chain(members.learners.keys())
        .map(|id| id.0)
        .collect();
    let now = Instant::now();
    let mut actions = decide(now, config, &nodes, &ids, &tasks, &snapshots, seen);
    actions.extend(expired_tasks(
        SystemTime::now(),
        now,
        config,
        &tasks,
        seen,
        TASK_DELETES_PER_TICK,
    ));
    let mut skip_deletes = false;
    for action in actions {
        if skip_deletes && matches!(action, Action::DeleteTask { .. }) {
            continue;
        }
        let result = apply(cluster, &action).await;
        // 그사이 바뀌었으면(실행기가 시작했다 등) 다음 tick에 다시 본다.
        match result {
            Ok(_) => tracing::info!(?action, "janitor"),
            Err(e) if written_anyway(cluster, &action, &e) => {
                tracing::info!(?action, "janitor (reply lost)")
            }
            Err(e) => {
                tracing::info!(?action, error = %e, "janitor action not applied");
                if stops_deletes(&action, &e) {
                    tracing::warn!(error = %e, "janitor: deleting finished tasks again next tick");
                    skip_deletes = true;
                }
                continue;
            }
        }
        if let Action::FailTask { id, error, .. } = action {
            record_schedule(state, &tasks, &snapshots, id, error).await;
        }
    }
    // 지우기 Task가 파일이 있는 노드에서 파일과 리소스를 지운다. 만들지 못하면(노드가 오프라인 등) 다음
    // tick에 다시 한다.
    for backup in stale_receiving(SystemTime::now(), config, &backups, &tasks) {
        match start_backup_delete(state, backup).await {
            Ok(task) => {
                tracing::info!(backup = %backup.name, task = %task.id, "janitor: deleting a backup that stopped receiving")
            }
            Err(e) => {
                tracing::info!(backup = %backup.name, error = %e.message, "janitor: cannot delete a backup that stopped receiving")
            }
        }
    }
}

/// 할 일을 읽은 revision에서만 쓴다.
async fn apply(cluster: &Cluster, action: &Action) -> Result<u64, ClusterError> {
    match action {
        Action::FailTask {
            id,
            revision,
            error,
        } => {
            let status = TaskStatus {
                state: TaskState::Failed,
                error: Some(error.clone()),
            };
            cluster
                .update_status_checked::<TaskSpec>(*id, *revision, &status)
                .await
        }
        Action::DeleteSnapshot { id, revision } => {
            cluster.delete::<SnapshotSpec>(*id, Some(*revision)).await
        }
        Action::DeleteTask { id, revision } => {
            cluster.delete::<TaskSpec>(*id, Some(*revision)).await
        }
    }
}

/// 끝난 Task 지우기가 거부(그사이 바뀜·없어짐)가 아닌 오류(시간 초과, 리더 아님 등)로 실패했으면
/// 클러스터가 쓰기를 받지 못하는 것이므로 이 tick의 남은 지우기는 건너뛴다(한 tick에 최대
/// `TASK_DELETES_PER_TICK`번 기다리지 않게).
fn stops_deletes(action: &Action, err: &ClusterError) -> bool {
    matches!(action, Action::DeleteTask { .. }) && !matches!(err, ClusterError::Rejected(_))
}

/// 실패 쓰기가 `RevisionMismatch`로 거부됐어도 앞선 시도의 응답만 잃은 자기 쓰기일 수 있다(전달
/// 재시도). Task를 다시 읽어 이 오류로 실패해 있으면 쓰인 것으로 본다.
fn written_anyway(cluster: &Cluster, action: &Action, err: &ClusterError) -> bool {
    let (
        Action::FailTask { id, error, .. },
        ClusterError::Rejected(Rejection::RevisionMismatch { .. }),
    ) = (action, err)
    else {
        return false;
    };
    cluster
        .get::<TaskSpec>(*id)
        .ok()
        .flatten()
        .is_some_and(|t| {
            t.status.state == TaskState::Failed && t.status.error.as_deref() == Some(error)
        })
}

/// 실패로 끝낸 Task가 예약 스냅샷의 만들기나 예약 정리의 삭제면 실행기처럼 VM에 남긴다.
async fn record_schedule(
    state: &AppState,
    tasks: &[Task],
    snapshots: &[Snapshot],
    id: Uuid,
    error: String,
) {
    let Some(task) = tasks.iter().find(|t| t.id == id) else {
        return;
    };
    let find = |s: Uuid| snapshots.iter().find(|x| x.id == s).cloned();
    let Some(vm) = recorded_vm(&task.spec, find) else {
        return;
    };
    if let Err(e) = set_schedule_error(&state.cluster, vm, Some(task_failed(error))).await {
        tracing::warn!(vm = %vm, error = %e, "cannot record schedule error");
    }
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;
    use opl_types::snapshot::SnapshotStatus;
    use opl_types::vm::{CpuSpec, MachineType, PowerState, VmSpec};

    use super::*;

    const GRACE: Duration = Duration::from_secs(60);
    const ORPHAN_GRACE: Duration = Duration::from_secs(30);
    const FINISHED_GRACE: Duration = Duration::from_secs(600);
    const CONFIG: ClusterJanitorConfig = ClusterJanitorConfig {
        tick: Duration::from_secs(10),
        offline_grace: GRACE,
        orphan_grace: ORPHAN_GRACE,
        task_max_age: Duration::from_secs(7 * 24 * 3600),
        task_max_finished: 1000,
        finished_grace: FINISHED_GRACE,
        receiving_max_age: Duration::from_secs(24 * 3600),
    };

    fn node(online: bool) -> Node {
        let mut n: Node = Resource::new(
            "n",
            NodeSpec {
                address: "h:1".into(),
                maintenance: false,
                witness: false,
            },
        );
        n.status = NodeStatus {
            online,
            ..Default::default()
        };
        n
    }

    fn task(op: TaskOp, node: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "",
            TaskSpec {
                op,
                target: Uuid::now_v7(),
                node,
            },
        );
        t.revision = 7;
        t.status = TaskStatus { state, error: None };
        t
    }

    fn pending_snapshot() -> Snapshot {
        let mut s: Snapshot = Resource::new(
            "",
            SnapshotSpec {
                vm: Uuid::now_v7(),
                name: "s".into(),
                parent: None,
                memory: false,
                description: String::new(),
                scheduled: false,
                vm_spec: VmSpec {
                    cpu: CpuSpec {
                        sockets: 1,
                        cores: 1,
                        model: Default::default(),
                    },
                    memory_mib: 256,
                    firmware: Default::default(),
                    machine: MachineType::Virt,
                    disks: vec![],
                    nics: vec![],
                    power: PowerState::Stopped,
                    restart: Default::default(),
                    node: None,
                    snapshot_schedule: None,
                    ha: false,
                },
            },
        );
        s.revision = 3;
        s.status = SnapshotStatus::default();
        s
    }

    fn fail(t: &Task, error: String) -> Action {
        Action::FailTask {
            id: t.id,
            revision: t.revision,
            error,
        }
    }

    #[test]
    fn pending_tasks_of_an_offline_node_fail_after_the_grace() {
        let (up, down) = (node(true), node(false));
        let nodes = [up.clone(), down.clone()];
        let members = BTreeSet::from([up.id, down.id]);
        let pending = task(TaskOp::VmStart, down.id, TaskState::Pending);
        let running = task(TaskOp::VmStop, down.id, TaskState::Running);
        let done = task(TaskOp::VmStart, down.id, TaskState::Failed);
        let elsewhere = task(TaskOp::VmStart, up.id, TaskState::Pending);
        let tasks = [pending.clone(), running, done, elsewhere];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, &nodes, &members, &tasks, &[], &mut seen);
        // 처음 오프라인으로 본 시각에서 유예를 잰다.
        assert_eq!(at(t0), vec![]);
        assert_eq!(at(t0 + GRACE - Duration::from_secs(1)), vec![]);
        // `running`은 건드리지 않는다.
        let error = format!("node {} is offline", down.id);
        assert_eq!(at(t0 + GRACE), vec![fail(&pending, error)]);
    }

    #[test]
    fn coming_back_online_restarts_the_grace() {
        let mut n = node(false);
        let members = BTreeSet::from([n.id]);
        let pending = task(TaskOp::VmStart, n.id, TaskState::Pending);
        let tasks = std::slice::from_ref(&pending);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let half = GRACE / 2;
        assert_eq!(
            decide(
                t0,
                &CONFIG,
                std::slice::from_ref(&n),
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        n.status.online = true;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + half, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert!(seen.offline_since.is_empty());
        // 다시 오프라인이 된 때부터 잰다.
        n.status.online = false;
        let nodes = std::slice::from_ref(&n);
        assert_eq!(
            decide(t0 + GRACE, &CONFIG, nodes, &members, tasks, &[], &mut seen),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + GRACE + half,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
        assert_eq!(
            decide(
                t0 + 2 * GRACE,
                &CONFIG,
                nodes,
                &members,
                tasks,
                &[],
                &mut seen
            )
            .len(),
            1
        );
    }

    #[test]
    fn unfinished_tasks_of_a_removed_node_fail_at_once() {
        let up = node(true);
        let gone = Uuid::now_v7();
        let pending = task(TaskOp::VmStart, gone, TaskState::Pending);
        let running = task(TaskOp::VmStop, gone, TaskState::Running);
        let done = task(TaskOp::VmStop, gone, TaskState::Succeeded);
        let tasks = [pending.clone(), running.clone(), done];
        let error = format!("node {gone} was removed");
        let mut seen = Seen::default();
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![fail(&pending, error.clone()), fail(&running, error)]
        );
        // 멤버십에 있으면 아직 노드를 등록하지 않은 새 노드다.
        assert_eq!(
            decide(
                Instant::now(),
                &CONFIG,
                std::slice::from_ref(&up),
                &BTreeSet::from([up.id, gone]),
                &tasks,
                &[],
                &mut seen
            ),
            vec![]
        );
    }

    #[test]
    fn pending_snapshots_without_a_create_task_go_after_the_orphan_grace() {
        let up = node(true);
        let nodes = std::slice::from_ref(&up);
        let members = BTreeSet::from([up.id]);
        let (orphan, busy, mut ready) =
            (pending_snapshot(), pending_snapshot(), pending_snapshot());
        ready.status.phase = SnapshotPhase::Ready;
        let create =
            |s: &Snapshot, state| task(TaskOp::SnapshotCreate { snapshot: s.id }, up.id, state);
        // 끝난 만들기 Task는 세지 않는다.
        let tasks = [
            create(&busy, TaskState::Running),
            create(&orphan, TaskState::Failed),
        ];
        let snapshots = [orphan.clone(), busy, ready];
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at = |now| decide(now, &CONFIG, nodes, &members, &tasks, &snapshots, &mut seen);
        // tick이 짧아도 처음 본 뒤 유예가 지나기 전에는 지우지 않는다.
        for ms in (0..ORPHAN_GRACE.as_millis() as u64).step_by(100) {
            assert_eq!(at(t0 + Duration::from_millis(ms)), vec![], "{ms}ms");
        }
        let delete = Action::DeleteSnapshot {
            id: orphan.id,
            revision: orphan.revision,
        };
        assert_eq!(at(t0 + ORPHAN_GRACE), vec![delete]);

        // 고아로 보인 뒤 Task가 보이면 지우지 않고, 다시 고아가 된 때부터 잰다.
        let mut seen = Seen::default();
        let snapshots = std::slice::from_ref(&orphan);
        let started = [create(&orphan, TaskState::Pending)];
        let mut at = |now, tasks: &[Task]| {
            decide(now, &CONFIG, nodes, &members, tasks, snapshots, &mut seen)
        };
        assert_eq!(at(t0, &[]), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &started), vec![]);
        assert_eq!(at(t0 + ORPHAN_GRACE, &[]), vec![]);
        assert_eq!(
            at(t0 + 2 * ORPHAN_GRACE - Duration::from_secs(1), &[]),
            vec![]
        );
        assert_eq!(at(t0 + 2 * ORPHAN_GRACE, &[]).len(), 1);
    }

    /// `ago` 전에 만든 끝난(`state`) Task
    fn task_made(now: SystemTime, ago: Duration, state: TaskState) -> Task {
        let at = (now - ago).duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), state);
        t.id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            at.as_secs(),
            at.subsec_nanos(),
        ));
        t
    }

    fn delete(t: &Task) -> Action {
        Action::DeleteTask {
            id: t.id,
            revision: t.revision,
        }
    }

    /// 끝난 것을 처음 본 뒤 `finished_grace`가 지난 리더가 고르는 것
    fn expired(
        now: SystemTime,
        config: &ClusterJanitorConfig,
        tasks: &[Task],
        limit: usize,
    ) -> Vec<Action> {
        let t0 = Instant::now();
        let mut seen = Seen::default();
        expired_tasks(now, t0, config, tasks, &mut seen, limit);
        let later = t0 + config.finished_grace;
        expired_tasks(now, later, config, tasks, &mut seen, limit)
    }

    #[test]
    fn just_finished_tasks_over_the_max_count_stay_for_the_grace() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let day = Duration::from_secs(24 * 3600);
        let older = task_made(now, 2 * min, TaskState::Succeeded);
        let newer = task_made(now, min, TaskState::Succeeded);
        let mut seen = Seen::default();
        let t0 = Instant::now();
        let mut at =
            |instant, tasks: &[Task]| expired_tasks(now, instant, &config, tasks, &mut seen, 200);
        // 막 끝난 것으로 보인 Task는 개수가 넘어도 남는다.
        let tasks = [older.clone(), newer.clone()];
        assert_eq!(at(t0, &tasks), vec![]);
        assert_eq!(
            at(t0 + FINISHED_GRACE - Duration::from_secs(1), &tasks),
            vec![]
        );
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&older)]);
        // 지워진 Task는 잊고, 새로 끝난 Task는 처음 본 때부터 잰다.
        let newest = task_made(now, Duration::ZERO, TaskState::Failed);
        let tasks = [newer.clone(), newest];
        assert_eq!(at(t0 + FINISHED_GRACE, &tasks), vec![delete(&newer)]);
        // 기간 기준도 끝난 것을 처음 본 뒤 유예를 기다린다(오래 꺼져 있던 노드의 Task가 늦게 실패한 경우).
        let old = task_made(now, 8 * day, TaskState::Failed);
        let tasks = std::slice::from_ref(&old);
        assert_eq!(at(t0 + FINISHED_GRACE, tasks), vec![]);
        assert_eq!(
            at(t0 + 2 * FINISHED_GRACE - Duration::from_secs(1), tasks),
            vec![]
        );
        assert_eq!(at(t0 + 2 * FINISHED_GRACE, tasks), vec![delete(&old)]);
        assert!(seen.finished.contains_key(&old.id));
        assert!(!seen.finished.contains_key(&older.id));
    }

    #[test]
    fn finished_tasks_older_than_the_max_age_go() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let old_ok = task_made(now, 8 * day, TaskState::Succeeded);
        let old_failed = task_made(now, 9 * day, TaskState::Failed);
        let young = task_made(now, 6 * day, TaskState::Succeeded);
        // 끝나지 않은 Task는 오래됐어도 남긴다.
        let old_pending = task_made(now, 30 * day, TaskState::Pending);
        let old_running = task_made(now, 30 * day, TaskState::Running);
        let tasks = [
            old_ok.clone(),
            young,
            old_pending,
            old_failed.clone(),
            old_running,
        ];
        // 오래된 것부터 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 200),
            vec![delete(&old_failed), delete(&old_ok)]
        );
    }

    #[test]
    fn finished_tasks_over_the_max_count_go_oldest_first() {
        let now = SystemTime::now();
        let config = ClusterJanitorConfig {
            task_max_finished: 2,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let finished: Vec<Task> = (1..=4)
            .map(|i| task_made(now, i * min, TaskState::Succeeded))
            .collect();
        let pending = task_made(now, 10 * min, TaskState::Pending);
        let mut tasks = finished.clone();
        tasks.push(pending);
        // 끝나지 않은 Task는 세지 않는다.
        assert_eq!(
            expired(now, &config, &tasks, 200),
            vec![delete(&finished[3]), delete(&finished[2])]
        );
        assert_eq!(expired(now, &config, &tasks[2..], 200), vec![]);
    }

    #[test]
    fn a_task_exactly_at_the_max_age_stays_and_ties_go_by_id() {
        let now = SystemTime::now();
        // id의 시각은 밀리초까지다.
        let now = SystemTime::UNIX_EPOCH
            + Duration::from_millis(
                now.duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            );
        let at_limit = task_made(now, CONFIG.task_max_age, TaskState::Succeeded);
        assert_eq!(expired(now, &CONFIG, &[at_limit], 200), vec![]);

        // 만든 시각이 같으면 id가 작은 것을 먼저 지운다.
        let config = ClusterJanitorConfig {
            task_max_finished: 1,
            ..CONFIG
        };
        let min = Duration::from_secs(60);
        let mut tied = [
            task_made(now, min, TaskState::Succeeded),
            task_made(now, min, TaskState::Failed),
        ];
        assert_ne!(tied[0].id, tied[1].id);
        tied.sort_by_key(|t| std::cmp::Reverse(t.id));
        assert_eq!(expired(now, &config, &tied, 200), vec![delete(&tied[1])]);
    }

    #[test]
    fn deletes_per_tick_are_capped() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let tasks: Vec<Task> = (0..5)
            .map(|i| task_made(now, (10 + i) * day, TaskState::Failed))
            .collect();
        // 상한까지 오래된 것부터 고르고 나머지는 다음 tick에 고른다.
        assert_eq!(
            expired(now, &CONFIG, &tasks, 2),
            vec![delete(&tasks[4]), delete(&tasks[3])]
        );
        assert_eq!(expired(now, &CONFIG, &tasks[..3], 2).len(), 2);
        assert_eq!(expired(now, &CONFIG, &tasks[..1], 2).len(), 1);
    }

    #[test]
    fn a_task_delete_that_cannot_be_written_stops_the_rest_of_the_deletes() {
        let t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Succeeded);
        let rejected = || {
            ClusterError::Rejected(Rejection::RevisionMismatch {
                expected: 1,
                actual: 2,
            })
        };
        let not_leader = || ClusterError::NotLeader {
            leader_id: None,
            leader_addr: None,
        };
        // 쓰기를 받지 못하면 남은 지우기를 다음 tick으로 미룬다.
        for err in [
            ClusterError::Timeout,
            ClusterError::Unreachable("x".into()),
            not_leader(),
        ] {
            assert!(stops_deletes(&delete(&t), &err), "{err:?}");
        }
        // 그 Task만 바뀌었거나 없어진 것이면 다음 것을 지운다.
        assert!(!stops_deletes(&delete(&t), &rejected()));
        // 다른 동작의 실패는 지우기를 막지 않는다.
        assert!(!stops_deletes(
            &fail(&t, "x".into()),
            &ClusterError::Timeout
        ));
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    #[tokio::test]
    async fn failing_a_task_the_executor_started_meanwhile_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        // 정리 작업이 `pending`으로 읽은 뒤 실행기가 먼저 시작했다.
        let action = fail(&t, "node x is offline".into());
        let running = TaskStatus {
            state: TaskState::Running,
            error: None,
        };
        cluster
            .update_status_checked::<TaskSpec>(t.id, t.revision, &running)
            .await
            .unwrap();
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClusterError::Rejected(Rejection::RevisionMismatch { .. })
            ),
            "{err:?}"
        );
        let got = cluster.get::<TaskSpec>(t.id).unwrap().unwrap();
        assert_eq!(got.status, running);
    }

    #[tokio::test]
    async fn a_rejected_failure_that_was_already_written_counts_as_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut t = task(TaskOp::VmStart, Uuid::now_v7(), TaskState::Pending);
        t.revision = cluster.create(&t).await.unwrap();
        let action = fail(&t, "node x is offline".into());
        apply(&cluster, &action).await.unwrap();
        // 응답만 잃고 다시 보낸 쓰기는 거부되지만 Task는 이 오류로 실패해 있다.
        let err = apply(&cluster, &action).await.unwrap_err();
        assert!(written_anyway(&cluster, &action, &err), "{err:?}");
        // 다른 오류로 실패해 있으면 다른 쪽이 쓴 것이다.
        let other = fail(&t, "node x was removed".into());
        let err = apply(&cluster, &other).await.unwrap_err();
        assert!(!written_anyway(&cluster, &other, &err), "{err:?}");
    }

    /// `ago` 전에 만든 `phase` 백업
    fn backup_made(now: SystemTime, ago: Duration, phase: BackupPhase) -> Backup {
        let at = (now - ago).duration_since(SystemTime::UNIX_EPOCH).unwrap();
        let spec: BackupSpec = serde_json::from_value(serde_json::json!({
            "vm": Uuid::now_v7(),
            "vm_name": "web",
            "pool": "dr",
            "vm_spec": { "cpu": { "cores": 1 }, "memory_mib": 512, "machine": "virt" }
        }))
        .unwrap();
        let mut b: Backup = Resource::new("b", spec);
        b.id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            at.as_secs(),
            at.subsec_nanos(),
        ));
        b.status.phase = phase;
        b
    }

    #[test]
    fn backups_receiving_for_too_long_are_deleted_once() {
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 3600);
        let hour = Duration::from_secs(3600);
        let stale = backup_made(now, day + hour, BackupPhase::Receiving);
        let fresh = backup_made(now, day - hour, BackupPhase::Receiving);
        let ready = backup_made(now, 2 * day, BackupPhase::Ready);
        let deleting = backup_made(now, 2 * day, BackupPhase::Receiving);
        // 오래전에 만들었어도 마지막으로 파일을 받은 지 하루가 안 됐으면 남긴다.
        let mut active = backup_made(now, 2 * day, BackupPhase::Receiving);
        let recent = (now - hour).duration_since(SystemTime::UNIX_EPOCH).unwrap();
        active.status.received_at = Some(recent.as_secs());
        let mut idle = backup_made(now, 3 * day, BackupPhase::Receiving);
        let old = (now - day - hour)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        idle.status.received_at = Some(old.as_secs());
        let mut task = task(TaskOp::BackupDelete, Uuid::now_v7(), TaskState::Running);
        task.spec.target = deleting.id;
        // 끝난 지우기 Task는 지우는 중이 아니다.
        let mut done = task.clone();
        done.spec.target = stale.id;
        done.status.state = TaskState::Failed;
        let backups = [stale.clone(), fresh, ready, deleting, active, idle.clone()];
        let got = stale_receiving(now, &CONFIG, &backups, &[task, done]);
        let ids: Vec<Uuid> = got.iter().map(|b| b.id).collect();
        assert_eq!(ids, [stale.id, idle.id]);
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
mod backup_jobs;
mod backups;
```

을 다음으로 바꾼다:

```rust
mod backup_jobs;
mod backup_receive;
mod backups;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(r::list_backup_jobs, backup_jobs::create_backup_job))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(backup_receive::receive_backup))
        .routes(routes!(backup_receive::put_backup_file))
        .routes(routes!(backup_receive::complete_backup))
        .routes(routes!(r::list_backup_jobs, backup_jobs::create_backup_job))
```

`crates/opl-api/src/state.rs`에서

```rust
use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
use std::sync::Arc;
use std::time::Duration;
```

`crates/opl-api/src/state.rs`에서

```rust
    pub upload_dir: PathBuf,
    /// 다른 노드에서 도는 VM의 콘솔을 중계할 때 쓴다. 없으면 이 노드의 VM 콘솔만 연다.
```

을 다음으로 바꾼다:

```rust
    pub upload_dir: PathBuf,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 다른 클러스터가 보내는 백업 파일을 받을
    /// 때 백업 풀의 경로를 정한다(`TaskRunnerConfig::mount_root`와 같다).
    pub mount_root: PathBuf,
    /// 다른 노드에서 도는 VM의 콘솔을 중계할 때 쓴다. 없으면 이 노드의 VM 콘솔만 연다.
```

`crates/opl-api/src/state.rs`에서

```rust
    pub(crate) oidc: OidcCache,
}
```

을 다음으로 바꾼다:

```rust
    pub(crate) oidc: OidcCache,
    /// 이 노드가 지금 받고 있는 백업 사본 파일(백업 id, 파일 이름). 같은 파일을 두 요청이 함께 쓰지 않게 한다.
    pub(crate) receiving_files: std::sync::Mutex<std::collections::HashSet<(Uuid, String)>>,
    /// 받는 백업 사본 파일의 본문 조각을 기다리는 시간(기본 60초). 넘으면 그 업로드를 끊는다.
    pub receive_idle_timeout: Duration,
}
```

`crates/opl-api/src/state.rs`에서

```rust
            upload_dir,
            peers: None,
```

을 다음으로 바꾼다:

```rust
            upload_dir,
            mount_root: PathBuf::new(),
            peers: None,
```

`crates/opl-api/src/state.rs`에서

```rust
            oidc: OidcCache::default(),
        }
```

을 다음으로 바꾼다:

```rust
            oidc: OidcCache::default(),
            receiving_files: Default::default(),
            receive_idle_timeout: Duration::from_secs(60),
        }
```

`crates/opl-api/src/tasks.rs`에서

```rust

mod backup;
mod clone;
```

을 다음으로 바꾼다:

```rust

pub(crate) mod backup;
mod clone;
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(super) const VARS_FILE: &str = "vars.fd";
```

을 다음으로 바꾼다:

```rust
/// 백업 디렉터리 안의 UEFI 변수 파일 이름
pub(crate) const VARS_FILE: &str = "vars.fd";
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
```

을 다음으로 바꾼다:

```rust
/// 파일이나 디렉터리 `path`를 디스크에 내려 쓴다(fsync). 디렉터리는 그 안의 이름 바꾸기를 남긴다.
pub(crate) async fn sync_path(path: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
```

을 다음으로 바꾼다:

```rust
/// 쓴다. 매니페스트가 있으면 다 쓴 백업이다(디스크 파일은 `qemu-img`가 닫을 때 내려 썼다).
pub(crate) async fn write_manifest(dir: &Path, manifest: &BackupManifest) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
```

`crates/opl-api/tests/backup_receive.rs`를 만든다:

```rust
//! 다른 클러스터가 보내는 백업 사본 받기: `POST /backups/receive`가 받는 중인 백업을 만들고,
//! `PUT /backups/{key}/files/{file}`이 파일을 받아 SHA-256과 크기를 돌려주며, `actions/complete`가
//! 매니페스트(받는 쪽 id)를 쓰고 `ready`로 바꾼다. 권한, 풀·매니페스트·파일 이름 검사, 같은 원본의
//! 사본, 받는 중 지우기, 옛 노드.

mod common;

use common::{Client, Env, vm_request};
use opl_types::backup::{Backup, BackupManifest, BackupPhase, BackupSpec, MANIFEST_FILE};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const FINGERPRINT: &str = "sha256:abababababababababababababababababababababababababababababababab";

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다. `nodes`가 비면 모든 노드의 것이다.
async fn dir_pool(env: &Env, name: &str, backups: bool, nodes: Vec<Uuid>) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes,
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 백업 풀 `dr`과 백업이 아닌 풀 `local`
async fn setup(env: &Env) {
    dir_pool(env, "dr", true, vec![]).await;
    dir_pool(env, "local", false, vec![]).await;
}

/// 디스크 둘(크기 3, 5바이트)과 UEFI 변수가 있는 원본 백업의 매니페스트
fn manifest() -> Value {
    let mut spec = vm_request("web")["spec"].clone();
    spec["disks"] = json!([
        { "volume": "web-root", "bus": "virtio-blk" },
        { "volume": "web-data", "bus": "virtio-blk" }
    ]);
    json!({
        "version": 1,
        "backup": Uuid::now_v7(),
        "vm": Uuid::now_v7(),
        "vm_name": "web",
        "vm_spec": spec,
        "disks": [
            { "name": "disk0", "file": "disk0.qcow2", "virtual_bytes": 1 << 20, "stored_bytes": 3 },
            { "name": "disk1", "file": "disk1.qcow2", "virtual_bytes": 2 << 20, "stored_bytes": 5 }
        ],
        "uefi_vars": true,
        // 2026-10-27 05:07:09 UTC
        "created_at": 1_793_077_629u64
    })
}

fn origin(backup: &Value) -> Value {
    json!({ "cluster": "main", "ca_fingerprint": FINGERPRINT, "backup": backup, "job": "nightly" })
}

fn receive_body(manifest: &Value) -> Value {
    json!({ "pool": "dr", "manifest": manifest, "origin": origin(&manifest["backup"]) })
}

/// 받기를 시작하고 받는 쪽 백업 id를 돌려준다.
async fn receive_ok(client: &Client, body: Value) -> Uuid {
    let (status, got) = client.post("/backups/receive", body).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    got["id"].as_str().unwrap().parse().unwrap()
}

/// 파일 하나를 보낸다(`Content-Length`가 붙는다).
async fn put_file(client: &Client, backup: Uuid, file: &str, data: &[u8]) -> (StatusCode, Value) {
    let r = client
        .http
        .put(format!(
            "{}/api/v1/backups/{backup}/files/{file}",
            client.base
        ))
        .bearer_auth(client.token.as_ref().unwrap())
        .body(data.to_vec());
    client.send(r).await
}

async fn complete(client: &Client, backup: Uuid) -> (StatusCode, Value) {
    client
        .post(&format!("/backups/{backup}/actions/complete"), Value::Null)
        .await
}

fn get_backup(env: &Env, id: Uuid) -> Option<Backup> {
    env.cluster.get::<BackupSpec>(id).unwrap()
}

/// 풀 `dr`에 있는 백업 디렉터리
fn dir_of(env: &Env, id: Uuid) -> std::path::PathBuf {
    opl_types::backup::backup_dir(&env.dir.path().join("dr"), id)
}

/// 온라인인 다른 노드를 저장소에 만든다.
async fn other_node(env: &Env, name: &str) -> Uuid {
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.1:1".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    node.status.version = Some(opl_types::version::BUILD.into());
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 받기는 받는 중인 백업을 만든다: VM·스펙·디스크·UEFI 변수·만든 시각은 매니페스트, 출처는 요청,
/// 파일을 둘 노드는 이 노드(공유가 아닌 풀)다. 이름이 없으면 원본 이름 규칙이고 겹치면 `-2`…를 붙인다.
#[tokio::test]
async fn receive_records_a_receiving_backup_with_its_origin() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let (status, got) = admin.post("/backups/receive", receive_body(&m)).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["name"], "web-20261027-050709");
    let id: Uuid = got["id"].as_str().unwrap().parse().unwrap();
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.name, "web-20261027-050709");
    assert_eq!(b.status.phase, BackupPhase::Receiving);
    assert_eq!(b.spec.vm.to_string(), m["vm"].as_str().unwrap());
    assert_eq!(b.spec.vm_name, "web");
    assert_eq!(b.spec.pool, "dr");
    assert_eq!(b.spec.job, None);
    assert_eq!(
        b.spec.vm_spec,
        serde_json::from_value::<VmSpec>(m["vm_spec"].clone()).unwrap()
    );
    let o = b.spec.origin.as_ref().unwrap();
    assert_eq!(
        (o.cluster.as_str(), o.ca_fingerprint.as_str()),
        ("main", FINGERPRINT)
    );
    assert_eq!(o.backup.to_string(), m["backup"].as_str().unwrap());
    assert_eq!(o.job.as_deref(), Some("nightly"));
    assert_eq!(serde_json::to_value(&b.status.disks).unwrap(), m["disks"]);
    assert!(b.status.uefi_vars);
    assert_eq!(b.status.created_at, Some(1_793_077_629));
    assert_eq!(b.status.node, Some(env.node));
    assert!(dir_of(&env, id).is_dir());
    // 목록과 보기에 출처가 보인다.
    let (_, list) = admin.get("/backups").await;
    assert_eq!(list["items"][0]["spec"]["origin"]["cluster"], "main");

    // 다른 원본이 같은 이름을 원하면 뒤에 번호를 붙인다. 이름을 주면 그것이다.
    let second = receive_ok(&admin, receive_body(&manifest())).await;
    assert_eq!(
        get_backup(&env, second).unwrap().name,
        "web-20261027-050709-2"
    );
    let mut body = receive_body(&manifest());
    body["name"] = json!("copy");
    let named = receive_ok(&admin, body.clone()).await;
    assert_eq!(get_backup(&env, named).unwrap().name, "copy");
    body["origin"]["backup"] = json!(Uuid::now_v7());
    let again = receive_ok(&admin, body).await;
    assert_eq!(get_backup(&env, again).unwrap().name, "copy-2");
}

/// 받기·파일·완료는 백업 쓰기 권한이다(operator 이상).
#[tokio::test]
async fn receiving_needs_backup_write() {
    let env = Env::start().await;
    setup(&env).await;
    let viewer = env.user("vic", "viewer").await;
    let operator = env.user("olga", "operator").await;
    assert_error(
        &viewer
            .post("/backups/receive", receive_body(&manifest()))
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let id = receive_ok(&operator, receive_body(&manifest())).await;
    assert_error(
        &put_file(&viewer, id, "disk0.qcow2", b"abc").await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &complete(&viewer, id).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_eq!(
        put_file(&operator, id, "disk0.qcow2", b"abc").await.0,
        StatusCode::OK
    );
    // 받는 중인 백업은 viewer도 읽는다.
    assert_eq!(
        viewer.get(&format!("/backups/{id}")).await.0,
        StatusCode::OK
    );
}

/// 받는 풀은 이 노드에서 쓸 수 있는 백업 풀이어야 한다. 매니페스트는 이 노드가 읽는 버전이고, 디스크
/// 파일은 `disk{i}.qcow2`, 디스크 수는 VM 스펙과 같아야 한다.
#[tokio::test]
async fn receive_checks_the_pool_and_the_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let post = |body: Value| {
        let admin = admin.clone();
        async move { admin.post("/backups/receive", body).await }
    };

    let mut body = receive_body(&manifest());
    body["pool"] = json!("local");
    assert_error(
        &post(body.clone()).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    body["pool"] = json!("nope");
    assert_error(
        &post(body).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    // 다른 노드의 dir 풀: 등록 URL을 그 노드로 하거나 NFS 백업 풀을 쓴다.
    let other = other_node(&env, "node2").await;
    dir_pool(&env, "far", true, vec![other]).await;
    let mut body = receive_body(&manifest());
    body["pool"] = json!("far");
    let got = post(body).await;
    assert_error(&got, StatusCode::CONFLICT, "pool_not_on_node");
    assert!(
        got.1["message"].as_str().unwrap().contains("NFS"),
        "{}",
        got.1
    );

    let bad = |edit: &dyn Fn(&mut Value)| {
        let mut m = manifest();
        edit(&mut m);
        receive_body(&m)
    };
    for body in [
        bad(&|m| m["version"] = json!(2)),
        bad(&|m| m["disks"][1]["file"] = json!("../disk1.qcow2")),
        bad(&|m| m["disks"][1]["file"] = json!("manifest.json")),
        bad(&|m| m["disks"][0]["file"] = json!("disk1.qcow2")),
        bad(&|m| m["disks"][1]["name"] = json!("x")),
        bad(&|m| {
            m["disks"].as_array_mut().unwrap().pop();
        }),
        bad(&|m| m["vm_name"] = json!("Web/..")),
        bad(&|m| m["vm_spec"]["memory_mib"] = json!(0)),
    ] {
        let got = post(body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let mut body = receive_body(&manifest());
    body["name"] = json!("Bad Name");
    assert_error(
        &post(body).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert!(env.cluster.list::<BackupSpec>().unwrap().1.is_empty());
}

/// 같은 원본(CA 지문과 원본 id)의 사본이 있으면 409 `replica_exists`이고 그 id를 알려 준다.
#[tokio::test]
async fn a_second_copy_of_the_same_origin_is_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let id = receive_ok(&admin, receive_body(&m)).await;
    let got = admin.post("/backups/receive", receive_body(&m)).await;
    assert_error(&got, StatusCode::CONFLICT, "replica_exists");
    assert_eq!(got.1["details"]["backup"], id.to_string());
    // 다른 클러스터의 같은 id는 다른 원본이다.
    let mut body = receive_body(&m);
    body["origin"]["ca_fingerprint"] = json!(format!("sha256:{}", "cd".repeat(32)));
    receive_ok(&admin, body).await;
    // 같은 원본이라도 다른 풀에 받으면 새 사본이다.
    dir_pool(&env, "dr2", true, vec![]).await;
    let mut body = receive_body(&m);
    body["pool"] = json!("dr2");
    let other = receive_ok(&admin, body.clone()).await;
    assert_ne!(other, id);
    let got = admin.post("/backups/receive", body).await;
    assert_error(&got, StatusCode::CONFLICT, "replica_exists");
    assert_eq!(got.1["details"]["backup"], other.to_string());
}

/// `Content-Length`만 `length`로 알리고 본문은 보내지 않은 PUT의 응답 상태와 본문. 서버가 본문을 읽기 전에
/// 거절하는지 본다.
async fn put_claiming(client: &Client, backup: Uuid, file: &str, length: u64) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = client.base.trim_start_matches("http://");
    let mut conn = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{backup}/files/{file} HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
        client.token.as_ref().unwrap()
    );
    conn.write_all(request.as_bytes()).await.unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = conn.read(&mut buf).await.unwrap();
        got.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&got);
        if n == 0 || text.trim_end().ends_with('}') {
            break;
        }
    }
    let text = String::from_utf8_lossy(&got).to_string();
    let status = text[9..12].parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    (status, body)
}

/// 디스크 파일은 매니페스트의 크기로만, `vars.fd`는 64 MiB까지만 받는다(400, 본문을 읽기 전에). 풀에
/// 매니페스트의 파일을 받을 자리가 없으면 받기를 시작하지 않는다(507 `insufficient_storage`).
#[tokio::test]
async fn sizes_are_limited_by_the_manifest_and_the_pool() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    for data in [&b"ab"[..], b"abcd"] {
        let got = put_file(&admin, id, "disk0.qcow2", data).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    let (status, body) = put_claiming(&admin, id, "vars.fd", (64 << 20) + 1).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["code"], "invalid_request", "{body}");
    assert!(!dir_of(&env, id).join("vars.fd").exists());
    let got = put_file(&admin, id, "vars.fd", b"uefi").await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);

    let mut m = manifest();
    m["disks"][0]["stored_bytes"] = json!(1u64 << 62);
    let got = admin.post("/backups/receive", receive_body(&m)).await;
    assert_error(
        &got,
        StatusCode::INSUFFICIENT_STORAGE,
        "insufficient_storage",
    );
    assert_eq!(env.cluster.list::<BackupSpec>().unwrap().1.len(), 1);
}

/// 같은 파일을 받는 중이면 그 파일의 다른 PUT은 409 `backup_busy`다. 다른 파일과 끝난 뒤는 받는다.
#[tokio::test]
async fn the_same_file_is_received_once_at_a_time() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);
    // 세 바이트 가운데 하나만 보내고 멈춘 PUT
    let addr = admin.base.trim_start_matches("http://").to_string();
    let mut slow = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{id}/files/disk0.qcow2 HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
        admin.token.as_ref().unwrap()
    );
    slow.write_all(request.as_bytes()).await.unwrap();
    let deadline = std::time::Instant::now() + common::WAIT;
    while !dir.join("disk0.qcow2.part").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the upload did not start"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"xyz").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_eq!(
        put_file(&admin, id, "disk1.qcow2", b"abcde").await.0,
        StatusCode::OK
    );

    slow.write_all(b"bc").await.unwrap();
    let mut answer = Vec::new();
    slow.read_to_end(&mut answer).await.unwrap();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
    assert_eq!(
        put_file(&admin, id, "disk0.qcow2", b"xyz").await.0,
        StatusCode::OK
    );
}

/// 본문을 보내다 멈춘 PUT은 조각을 기다리는 시간(기본 60초, 여기서는 줄인 것)이 지나면 400
/// `invalid_request`로 끝나고 `.part`를 지우며 그 파일의 표시를 푼다. 그래서 같은 파일을 다시 받는다.
#[tokio::test]
async fn a_stalled_upload_is_cut_off() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let env = Env::start().await;
    setup(&env).await;
    // 같은 저장소를 쓰고 조각을 300ms만 기다리는 API 서버
    let hv: std::sync::Arc<dyn opl_driver::Hypervisor> = env.hv.clone();
    let mut state = opl_api::AppState::new(
        env.cluster.clone(),
        env.node,
        hv,
        env.dir.path().join("uploads"),
    );
    state.receive_idle_timeout = std::time::Duration::from_millis(300);
    let app = opl_api::router(std::sync::Arc::new(state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let admin = common::login_at(&format!("http://{addr}"), "admin", common::ADMIN_PASSWORD).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);

    // 세 바이트 가운데 하나만 보내고 멈춘 PUT
    let mut slow = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let request = format!(
        "PUT /api/v1/backups/{id}/files/disk0.qcow2 HTTP/1.1\r\nHost: {addr}\r\n\
         Authorization: Bearer {}\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
        admin.token.as_ref().unwrap()
    );
    slow.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(common::WAIT, slow.read_to_end(&mut answer))
        .await
        .expect("the stalled upload was not cut off")
        .unwrap();
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("the upload stalled"), "{answer}");
    assert!(!dir.join("disk0.qcow2.part").exists());
    assert!(!dir.join("disk0.qcow2").exists());

    let got = put_file(&admin, id, "disk0.qcow2", b"abc").await;
    assert_eq!(got.0, StatusCode::OK, "{}", got.1);
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
}

/// 받은 파일마다 디스크의 SHA-256과 마지막 활동 시각(`received_at`)을 적는다.
#[tokio::test]
async fn received_files_record_their_hash_and_the_time() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.status.received_at, None);
    assert!(b.status.disks.iter().all(|d| d.sha256.is_none()));

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    put_file(&admin, id, "disk1.qcow2", b"abcde").await;
    put_file(&admin, id, "vars.fd", b"uefi").await;
    let b = get_backup(&env, id).unwrap();
    assert!(b.status.received_at.unwrap() >= before, "{:?}", b.status);
    assert_eq!(b.status.disks[0].sha256, None);
    assert_eq!(
        b.status.disks[1].sha256.as_deref(),
        Some(hex::encode(Sha256::digest(b"abcde")).as_str())
    );
    // 다시 보내면 해시도 바뀐다. 완료한 뒤에도 남는다.
    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    put_file(&admin, id, "disk1.qcow2", b"vwxyz").await;
    assert_eq!(complete(&admin, id).await.0, StatusCode::OK);
    let b = get_backup(&env, id).unwrap();
    let hashes: Vec<Option<String>> = b.status.disks.iter().map(|d| d.sha256.clone()).collect();
    assert_eq!(
        hashes,
        [
            Some(hex::encode(Sha256::digest(b"abc"))),
            Some(hex::encode(Sha256::digest(b"vwxyz")))
        ]
    );
}

/// 지우는 Task가 걸린 백업은 완료하지 않는다(409 `backup_busy`).
#[tokio::test]
async fn a_backup_being_deleted_is_not_completed() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    for (file, data) in [
        ("disk0.qcow2", &b"abc"[..]),
        ("disk1.qcow2", b"abcde"),
        ("vars.fd", b"uefi"),
    ] {
        assert_eq!(put_file(&admin, id, file, data).await.0, StatusCode::OK);
    }
    env.stop_runner().await;
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
    env.start_runner().await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

/// 파일은 `.part`에 받아 이름을 바꾸고 SHA-256과 크기를 돌려준다. 다시 보내면 덮는다. 매니페스트에
/// 없는 이름은 400, `Content-Length`가 없으면 411, 받는 중이 아니면 409.
#[tokio::test]
async fn files_are_hashed_and_overwritten_and_limited_to_the_manifest() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let dir = dir_of(&env, id);

    let (status, got) = put_file(&admin, id, "disk0.qcow2", b"old").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["sha256"], hex::encode(Sha256::digest(b"old")));
    assert_eq!(got["size"], 3);
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"old");
    let (status, got) = put_file(&admin, id, "disk0.qcow2", b"abc").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["sha256"], hex::encode(Sha256::digest(b"abc")));
    assert_eq!(std::fs::read(dir.join("disk0.qcow2")).unwrap(), b"abc");
    assert!(!dir.join("disk0.qcow2.part").exists());
    let (status, _) = put_file(&admin, id, "vars.fd", b"uefi").await;
    assert_eq!(status, StatusCode::OK);

    for file in [
        "disk2.qcow2",
        "manifest.json",
        "..%2Fdisk0.qcow2",
        "disk0.qcow2.part",
    ] {
        let got = put_file(&admin, id, file, b"x").await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    }
    assert!(!dir.join(MANIFEST_FILE).exists());

    // 스트림 본문은 길이를 모르므로 chunked로 간다.
    let chunks = futures_util::stream::iter([Ok::<_, std::io::Error>(b"abc".to_vec())]);
    let r = admin
        .http
        .put(format!(
            "{}/api/v1/backups/{id}/files/disk0.qcow2",
            admin.base
        ))
        .bearer_auth(admin.token.as_ref().unwrap())
        .body(reqwest::Body::wrap_stream(chunks));
    assert_error(
        &admin.send(r).await,
        StatusCode::LENGTH_REQUIRED,
        "length_required",
    );

    // UEFI 변수가 없는 백업은 vars.fd를 받지 않는다.
    let mut m = manifest();
    m["uefi_vars"] = json!(false);
    let bios = receive_ok(&admin, receive_body(&m)).await;
    assert_error(
        &put_file(&admin, bios, "vars.fd", b"x").await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &put_file(&admin, Uuid::now_v7(), "disk0.qcow2", b"x").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 완료는 모든 파일이 매니페스트의 크기로 있어야 한다(409 `backup_incomplete`와 빠진 파일). 쓴
/// 매니페스트의 `backup`은 받는 쪽 id다. 그 뒤로는 파일도 완료도 받지 않는다.
#[tokio::test]
async fn complete_needs_every_file_and_writes_the_manifest_with_the_local_id() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let m = manifest();
    let id = receive_ok(&admin, receive_body(&m)).await;
    let dir = dir_of(&env, id);

    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    let got = complete(&admin, id).await;
    assert_error(&got, StatusCode::CONFLICT, "backup_incomplete");
    assert_eq!(got.1["details"]["files"], json!(["disk1.qcow2", "vars.fd"]));
    // 크기가 다른 파일도 다 받지 못한 것이다(PUT은 크기가 다르면 받지 않으므로 직접 쓴다).
    std::fs::write(dir.join("disk1.qcow2"), b"abcd").unwrap();
    put_file(&admin, id, "vars.fd", b"uefi").await;
    let got = complete(&admin, id).await;
    assert_error(&got, StatusCode::CONFLICT, "backup_incomplete");
    assert_eq!(got.1["details"]["files"], json!(["disk1.qcow2"]));
    assert_eq!(
        get_backup(&env, id).unwrap().status.phase,
        BackupPhase::Receiving
    );
    assert!(!dir.join(MANIFEST_FILE).exists());

    put_file(&admin, id, "disk1.qcow2", b"abcde").await;
    let (status, got) = complete(&admin, id).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["status"]["phase"], "ready");
    let b = get_backup(&env, id).unwrap();
    assert_eq!(b.status.phase, BackupPhase::Ready);
    let written: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(written.backup, id);
    assert_eq!(written.vm_spec, b.spec.vm_spec);
    assert_eq!(written.disks, b.status.disks);
    assert!(written.uefi_vars);
    assert_eq!(written.created_at, 1_793_077_629);
    assert_eq!(written.vm.to_string(), m["vm"].as_str().unwrap());

    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"abc").await,
        StatusCode::CONFLICT,
        "backup_not_receiving",
    );
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_not_receiving",
    );
}

/// 다른 노드가 받는(파일이 그 노드에 있는) 백업의 파일은 그 노드로 보내야 한다.
#[tokio::test]
async fn files_of_a_backup_on_another_node_are_refused() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    let other = other_node(&env, "node2").await;
    let mut b = get_backup(&env, id).unwrap();
    b.status.node = Some(other);
    env.cluster
        .update_status::<BackupSpec>(id, &b.status)
        .await
        .unwrap();
    assert_error(
        &put_file(&admin, id, "disk0.qcow2", b"abc").await,
        StatusCode::CONFLICT,
        "backup_on_other_node",
    );
    assert_error(
        &complete(&admin, id).await,
        StatusCode::CONFLICT,
        "backup_on_other_node",
    );
}

/// 받는 중인 백업을 지우면 받은 파일도 지운다.
#[tokio::test]
async fn deleting_a_receiving_backup_removes_its_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let id = receive_ok(&admin, receive_body(&manifest())).await;
    put_file(&admin, id, "disk0.qcow2", b"abc").await;
    let dir = dir_of(&env, id);
    assert!(dir.join("disk0.qcow2").exists());
    let (status, task) = admin.delete(&format!("/backups/{id}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(get_backup(&env, id).is_none());
    assert!(!dir.exists());
}

/// 옛 노드는 받는 중인 백업과 출처를 모르므로 모든 노드가 이 빌드일 때만 받는다.
#[tokio::test]
async fn receive_waits_for_every_node_to_run_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    assert_error(
        &admin
            .post("/backups/receive", receive_body(&manifest()))
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
}
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust

/// 디스크 하나를 복원하지 못하면 그때까지 만든 볼륨(파일과 리소스)을 지우고 VM을 만들지 않는다.
```

을 다음으로 바꾼다:

```rust

/// 다른 클러스터에서 받은 사본은 보통 백업처럼 복원한다: 원본 백업의 매니페스트와 파일을 받기 API로
/// 보내 받은 사본(매니페스트의 백업 id는 사본의 id)에서 같은 내용의 VM을 만든다.
#[tokio::test]
async fn a_received_copy_restores_like_its_original() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin).await;
    let b = backed_up(&env, &admin).await;
    create_pool(&env, "dr", true).await;
    create_pool(&env, "other", false).await;
    let source = backup_dir(&env, b.id);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(source.join(MANIFEST_FILE)).unwrap()).unwrap();
    let body = json!({
        "pool": "dr",
        "manifest": manifest,
        "origin": { "cluster": "main", "ca_fingerprint": "sha256:00", "backup": b.id }
    });
    let (status, got) = admin.post("/backups/receive", body).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    let copy: Uuid = got["id"].as_str().unwrap().parse().unwrap();
    for file in ["disk0.qcow2", "disk1.qcow2", "vars.fd"] {
        let r = admin
            .http
            .put(format!("{}/api/v1/backups/{copy}/files/{file}", admin.base))
            .bearer_auth(admin.token.as_ref().unwrap())
            .body(std::fs::read(source.join(file)).unwrap());
        let (status, got) = admin.send(r).await;
        assert_eq!(status, StatusCode::OK, "{file}: {got}");
    }
    let (status, got) = admin
        .post(&format!("/backups/{copy}/actions/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");

    let task = wait(
        &admin,
        admin
            .post(
                &format!("/backups/{copy}/restore"),
                json!({ "name": "web-dr", "pool": "other" }),
            )
            .await,
    )
    .await;
    succeeded(&task);
    let new = get_vm(&env, "web-dr").unwrap();
    assert_eq!(new.spec.memory_mib, b.spec.vm_spec.memory_mib);
    for (name, pattern) in [
        ("web-dr-disk0", "read -P 0xaa 0 64k"),
        ("web-dr-disk1", "read -P 0x5a 64k 64k"),
    ] {
        let volume = get_volume(&env, name).unwrap();
        let path = pool_dir(&env, "other").join(format!("{}.qcow2", volume.id));
        assert!(qemu_io(&path, "qcow2", pattern), "{name}");
    }
    let vars = uefi_vars_path(&env.dir.path().join("state"), new.id);
    assert_eq!(std::fs::read(vars).unwrap(), b"uefi variables");
}

/// 디스크 하나를 복원하지 못하면 그때까지 만든 볼륨(파일과 리소스)을 지우고 VM을 만들지 않는다.
```

`crates/opl-api/tests/janitor.rs`에서

```rust
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지, 끝난 Task를 기간·개수
//! 기준으로 지우고 끝나지 않은 Task는 남기는지.
```

을 다음으로 바꾼다:

```rust
//! 스냅샷을 지우는지, 끝낸 예약 스냅샷 만들기를 `schedule_error`에 남기는지, 끝난 Task를 기간·개수
//! 기준으로 지우고 끝나지 않은 Task는 남기는지, 오래 받는 중인 백업을 지우는지.
```

`crates/opl-api/tests/janitor.rs`에서

```rust
use opl_api::{ClusterJanitor, ClusterJanitorConfig};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
```

을 다음으로 바꾼다:

```rust
use opl_api::{ClusterJanitor, ClusterJanitorConfig};
use opl_types::backup::BackupSpec;
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
```

`crates/opl-api/tests/janitor.rs`에서

```rust
    assert_eq!(left, expected);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(left, expected);
}

/// 보내던 쪽이 끝내지 못해 오래 `receiving`인 백업은 지우기 Task로 파일과 함께 지운다. 받은 백업은
/// 남는다.
#[tokio::test]
async fn backups_left_receiving_are_deleted_with_their_files() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let path = env.dir.path().join("dr");
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        "dr",
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: path.clone() },
            nodes: vec![],
            shared: false,
            backups: true,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready("dr").await;
    let receive = |m: Value| {
        let admin = admin.clone();
        async move {
            let body = serde_json::json!({
                "pool": "dr",
                "manifest": m,
                "origin": { "cluster": "main", "ca_fingerprint": "sha256:00", "backup": m["backup"] }
            });
            let (status, got) = admin.post("/backups/receive", body).await;
            assert_eq!(status, StatusCode::CREATED, "{got}");
            got["id"].as_str().unwrap().parse::<Uuid>().unwrap()
        }
    };
    let manifest = || {
        serde_json::json!({
            "version": 1, "backup": Uuid::now_v7(), "vm": Uuid::now_v7(), "vm_name": "web",
            "vm_spec": vm_request("web")["spec"], "disks": [], "uefi_vars": false, "created_at": 1
        })
    };
    let left = receive(manifest()).await;
    let done = receive(manifest()).await;
    let (status, got) = admin
        .post(&format!("/backups/{done}/actions/complete"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let dir = opl_types::backup::backup_dir(&path, left);
    assert!(dir.is_dir());

    let janitor = ClusterJanitor::spawn(
        env.state.clone(),
        ClusterJanitorConfig {
            tick: Duration::from_millis(50),
            receiving_max_age: GRACE,
            ..ClusterJanitorConfig::default()
        },
    );
    until("receiving backup deleted", || {
        env.cluster.get::<BackupSpec>(left).unwrap().is_none()
    })
    .await;
    janitor.shutdown().await;
    assert!(!dir.exists());
    assert!(env.cluster.get::<BackupSpec>(done).unwrap().is_some());
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/backups/{key}/restore",
        "/backup-jobs",
```

을 다음으로 바꾼다:

```rust
        "/backups/{key}/restore",
        "/backups/receive",
        "/backups/{key}/files/{file}",
        "/backups/{key}/actions/complete",
        "/backup-jobs",
```

`crates/opl-types/src/backup.rs`에서

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
```

을 다음으로 바꾼다:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BackupManifest {
```

`crates/opld/src/member.rs`에서

```rust
            );
            state.peers = Some(peers.clone());
```

을 다음으로 바꾼다:

```rust
            );
            state.mount_root = mount_root.clone();
            state.peers = Some(peers.clone());
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1495개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 백업 사본 받기 API — backup_receive.rs: POST /backups/receive { name?, pool, manifest, origin }(백업 쓰기, 201 { id, name }): 옛 노드가 있으면 409 cluster_upgrading, 풀이 없거나 백업 풀이 아니면 400, 이 노드에서 쓸 수 없으면 409 pool_not_on_node(원격 등록 URL을 풀이 있는 노드로 하거나 NFS 백업 풀을 쓰라는 메시지)·pool_unavailable, 매니페스트 검사(버전, VM 이름·스펙, 디스크 수가 스펙과 같고 디스크 i는 disk{i}·disk{i}.qcow2) 400, 같은 풀에 같은 origin.ca_fingerprint+origin.backup의 사본이 있으면 409 replica_exists(details.backup — 다른 풀이면 새 사본), 풀의 남은 공간(fs4)이 디스크 stored_bytes 합보다 작으면 507 insufficient_storage. 이름은 주면 그것, 없으면 default_backup_name(vm_name, created_at)이고 겹치면 -2, -3…(63자를 넘지 않게 줄임). receiving 백업(spec은 매니페스트의 vm·vm_name·vm_spec과 origin, status는 disks(sha256은 비움)·uefi_vars·created_at, 공유가 아닌 풀이면 node=이 노드)과 디렉터리를 만듦. PUT /backups/{key}/files/{file}(백업 쓰기): Content-Length 없으면 411, receiving이 아니면 409 backup_not_receiving, 다른 노드의 백업이면 409 backup_on_other_node, file은 매니페스트의 디스크 파일이나(uefi_vars면) vars.fd만(아니면 400 — 경로 문자도 여기서 걸림), Content-Length는 디스크 파일이면 매니페스트의 stored_bytes와 같고 vars.fd면 64 MiB까지(아니면 본문을 읽기 전에 400), (백업, 파일)별 메모리 잠금(AppState.receiving_files)으로 같은 파일을 받는 중이면 409 backup_busy, {file}.part에 받으며 SHA-256을 계산하고 128 MiB마다 sync_data, 본문 조각이 60초(AppState.receive_idle_timeout) 넘게 오지 않으면 .part를 지우고 잠금을 풀고 400 invalid_request \"the upload stalled\", 끝나면 fsync·이름 바꾸기·디렉터리 fsync, 디스크의 sha256과 status.received_at(마지막 활동)을 revision 확인 쓰기(다시 하기)로 적고 200 { sha256, size }, 다시 보내면 덮음. POST /backups/{key}/actions/complete(백업 쓰기): 디스크 파일 크기가 stored_bytes와 같고 vars.fd가 있어야(아니면 409 backup_incomplete, details.files), 지우는 Task가 걸려 있으면 409 backup_busy(backups::check_not_deleting), manifest.json을 backup=받는 쪽 id로 쓰고 ready(update_status_checked — 어긋나면 다시 읽어 받는 중·지우기를 다시 확인하고 매니페스트도 다시 읽은 상태로 씀), 200 백업. AppState.mount_root(opld가 {data_dir}/pools)로 이 노드의 풀 경로를 정함. 받는 중 지우기는 기존 지우기 Task가 디렉터리째 지움. janitor: 마지막 활동(만든 시각(UUIDv7)과 received_at 가운데 늦은 것)부터 receiving_max_age(기본 24시간)가 지난 receiving 백업을 지우는 Task가 없으면 start_backup_delete로 지움. BackupManifest에 ToSchema, schema.d.ts 다시 만듦. 시험: tests/backup_receive.rs(받기 기록·출처·이름 번호, viewer 403·operator 됨, 풀·매니페스트 검사와 pool_not_on_node, replica_exists와 다른 풀은 새 사본, 크기 제한(디스크 크기 다름·vars.fd 64 MiB 초과 400·자리 없음 507), 같은 파일 동시 PUT 409, 멈춘 업로드가 끊기고 같은 파일을 다시 받음, 해시·received_at 기록, 지우는 중 complete 409, 해시·크기·덮기·모르는 파일·..%2F·411·BIOS의 vars.fd, complete의 빠진 파일·크기 다름·받는 쪽 id 매니페스트·그 뒤 409, 다른 노드 409, 받는 중 지우기, 옛 노드 409), backup_tasks.rs(storage-it) 받은 사본 복원이 같은 디스크 내용, janitor 단위(활동이 있으면 지우지 않음)·통합(오래 받는 중인 백업과 파일을 지우고 받은 백업은 남김), openapi 경로, free_name 단위"
```

---

### Task 3: 백업 복제 Task와 수동 복제

`POST /api/v1/backups/{key}/actions/replicate {remote, pool}`가 `backup-replicate` Task를 만든다. Task는 기록한 사본을 먼저 확인하고(없으면 새로, 있으면 출처·풀·VM 스펙·크기·해시 비교), 파일을 읽으며 SHA-256을 계산해 원격에 흘려 보내고(응답 120초까지) 해시를 비교한 뒤 complete, 원본의 `replicas`와 디스크 해시를 적는다. 이미 ready인 사본의 확인은 상태를 흔들지 않는다.

**Files:**
- Create: `crates/opl-api/src/tasks/replicate.rs`
- Create: `crates/opl-api/tests/backup_replicate.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/remote_client.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/remote_client.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/backups.rs` 전체:

```rust
//! VM 백업 만들기·목록·읽기·지우기·복원(설계 §2)과 원격 클러스터로 복제(엔터프라이즈 설계 §6.2).
//! 만들기는 VM 쓰기, 지우기·복원·복제는 백업 쓰기 권한이다. 실제 작업은 Task 실행기가 한다: 백업은
//! VM의 노드, 지우기·복원·복제는 백업 풀을 쓸 수 있는 노드(공유가 아닌 풀의 백업은 파일을 쓴 노드).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus, default_backup_name};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::remote::RemoteSpec;
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
use crate::cluster::require_current;
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

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplicateRequest {
    /// 사본을 보낼 원격 클러스터(이름이나 id)
    pub remote: String,
    /// 원격 클러스터의 백업 풀 이름
    pub pool: String,
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
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
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

/// 백업 `id`를 다루는 끝나지 않은 Task가 하는 일(만들기, 복원, 지우기, 복제)
fn backup_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmBackup { backup } if backup == id => Some("being created"),
            TaskOp::BackupRestore { backup, .. } if backup == id => Some("being restored"),
            TaskOp::BackupDelete if t.spec.target == id => Some("being deleted"),
            TaskOp::BackupReplicate { .. } if t.spec.target == id => Some("being replicated"),
            _ => None,
        })
        .collect())
}

/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
pub(crate) fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
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
pub(crate) fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
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

/// 백업을 지우는 Task가 끝나지 않았으면 409 `backup_busy`. 받는 중인 사본의 완료가 쓴다.
pub(crate) fn check_not_deleting(state: &AppState, backup: &Backup) -> Result<(), ApiError> {
    if backup_tasks(state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(backup, "being deleted"));
    }
    Ok(())
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
    let node = check_backup(&state, &vm, &pool)?;
    let name = req
        .name
        .unwrap_or_else(|| default_backup_name(&vm.name, now()));
    let task = start_backup(&state, &vm, &pool, node, name, None).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM을 백업 풀 `pool`에 백업할 수 있는지 보고 백업 Task를 돌릴 노드를 돌려준다. 오류는
/// [`create_backup`]과 같다. 백업 요청 API와 백업 작업 스케줄러가 쓴다(권한 검사는 부르는 쪽).
pub(crate) fn check_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
) -> Result<Uuid, ApiError> {
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    check_not_busy(state, vm)?;
    let disks = vm_disks(state, vm)?;
    // 실행 중인 VM은 임시 스냅샷에서 읽는다.
    if !is_stopped(vm)
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
    if !is_stopped(vm) {
        check_disks(state, vm)?;
    }
    let node = backup_node(state, vm, pool, &disks)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on the node holding VM {:?}",
            pool.name, vm.name
        )
    })?;
    Ok(node)
}

/// [`check_backup`]을 거친 VM의 `pending` 백업 `name`을 만들고 노드 `node`에서 그것을 채울 Task를
/// 돌려준다. 이름이 올바르지 않으면 400. 백업 작업이 만드는 백업은 `job`에 작업 id를 적는다.
pub(crate) async fn start_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    node: Uuid,
    name: String,
    job: Option<Uuid>,
) -> Result<Task, ApiError> {
    check_name(&name)?;
    let backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: vm.id,
            vm_name: vm.name.clone(),
            pool: pool.name.clone(),
            vm_spec: vm.spec.clone(),
            job,
            origin: None,
        },
    );
    // 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    // 풀 지우기·백업 표시 끄기는 백업이 없을 때만 된다. 그 확인과 엇갈렸으면 만든 백업을 거둔다.
    if !find_pool(state, &pool.name).is_ok_and(|p| p.spec.backups) {
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
        Ok(task) => Ok(task),
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
    if q.force {
        check_backup_idle(&state, &backup)?;
        if let Err(e) = delete_node(&state, &backup)
            && e.status == StatusCode::CONFLICT
        {
            tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, reason = %e.message, "deleting backup without its files");
            state.cluster.delete::<BackupSpec>(backup.id, None).await?;
            let message = format!(
                "backup {:?} deleted; its files are left in storage pool {:?} ({})",
                backup.name, backup.spec.pool, e.message
            );
            return Ok(Json(BackupForgotten { message }).into_response());
        }
    }
    let task = start_backup_delete(&state, &backup).await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// 백업을 만들거나 되살리거나 지우거나 복제하는 Task가 끝나지 않았으면 409 `backup_busy`
fn check_backup_idle(state: &AppState, backup: &Backup) -> Result<(), ApiError> {
    match backup_tasks(state, backup.id)?.first() {
        Some(doing) => Err(backup_busy(backup, doing)),
        None => Ok(()),
    }
}

/// 백업 지우기 Task를 돌릴 노드. 풀이 없어졌으면(파일도 함께 없어졌다) 이 노드다.
fn delete_node(state: &AppState, backup: &Backup) -> Result<Uuid, ApiError> {
    match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)?
    {
        None => Ok(state.node),
        Some(pool) => files_node(state, backup, &pool),
    }
}

/// 백업 파일과 리소스를 지울 Task를 만든다. 오류는 `force` 없는 [`delete_backup`]과 같다. 백업 지우기
/// API와 백업 작업의 보존이 쓴다(권한 검사는 부르는 쪽).
pub(crate) async fn start_backup_delete(
    state: &AppState,
    backup: &Backup,
) -> Result<Task, ApiError> {
    check_backup_idle(state, backup)?;
    let node = delete_node(state, backup)?;
    create_task(&state.cluster, TaskOp::BackupDelete, backup.id, node).await
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

/// `ready` 백업을 원격 클러스터 `remote`의 백업 풀 `pool`에 사본으로 보낼 Task를 돌려준다(202). Task는
/// 백업 파일이 있는 노드(공유가 아닌 풀은 파일을 쓴 노드)에서 돌고, 원격의 받기 API로 파일을 보내며 원본
/// `status.replicas`에 결과를 적는다. 원격 쪽 권한은 원격 토큰 주인의 역할이다(백업 쓰기, operator
/// 이상). 원격이 없으면 404, 백업이 준비되지 않았으면 409 `backup_not_ready`, 지우는 중이거나 같은 원격으로
/// 복제하는 Task가 끝나지 않았으면 409 `backup_busy`, 파일을 읽을 노드가 없으면 409(`node_offline`,
/// `pool_unavailable` 등), 옛 노드가 있으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/backups/{key}/actions/replicate", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    request_body = ReplicateRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn replicate_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<ReplicateRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    check_name(&req.pool)?;
    // 옛 노드는 복제 Task와 사본 기록을 모른다.
    require_current(&state.cluster)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &req.remote)?;
    if backup.status.phase != BackupPhase::Ready {
        return Err(ApiError::conflict(
            "backup_not_ready",
            format!("backup {:?} is not ready", backup.name),
        ));
    }
    if backup_tasks(&state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(&backup, "being deleted"));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && t.spec.target == backup.id
            && matches!(&t.spec.op, TaskOp::BackupReplicate { remote: r, .. } if *r == remote.name)
    }) {
        return Err(backup_busy(
            &backup,
            &format!("being replicated to remote {:?}", remote.name),
        ));
    }
    let pool = find_pool(&state, &backup.spec.pool)?;
    let node = files_node(&state, &backup, &pool)?;
    let op = TaskOp::BackupReplicate {
        remote: remote.name,
        pool: req.pool,
    };
    let task = create_task(&state.cluster, op, backup.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(backup_receive::receive_backup))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(backups::replicate_backup))
        .routes(routes!(backup_receive::receive_backup))
```

`crates/opl-api/src/remote_client.rs` 전체:

```rust
//! 원격 opLite 클러스터의 HTTPS 클라이언트. TLS는 원격 클러스터 CA 하나만 믿고 호스트 이름은 확인하지
//! 않는다(노드 인증서의 이름은 노드마다 다르다). CA는 등록할 때 받아 지문을 확인하고 저장한 인증서
//! 하나다. 요청마다 새 연결을 연다(HTTP/1.1). 리다이렉트는 따르지 않는다(3xx는 그대로 응답).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use http_body_util::{BodyExt, Empty};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::header::{ACCEPT, AUTHORIZATION, HOST, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use opl_types::remote::normalize_url;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio::time::{Instant, Sleep};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// TCP 연결과 TLS 핸드셰이크의 시간 제한
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 요청을 다 보낸 뒤 응답 헤더를 기다리는 기본 시간
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// 본문(보내는 것과 받는 것) 조각 사이의 최대 시간
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// `GET /cluster/ca` 본문의 최대 크기
const MAX_CA_BYTES: usize = 64 << 10;
/// [`RemoteClient::get_json`] 본문의 최대 크기
const MAX_JSON_BYTES: usize = 4 << 20;
/// 오류 응답에서 메시지를 읽을 본문의 최대 크기
const MAX_ERROR_BYTES: usize = 64 << 10;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// 원격 클러스터에 요청하지 못한 이유. 메시지는 사용자에게 그대로 보인다.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("cannot reach the remote cluster: {0}")]
    Unreachable(String),
    #[error("TLS with the remote cluster failed: {0}")]
    Tls(String),
    /// 원격이 2xx가 아닌 상태로 답했다(오류 본문의 `message`, 없으면 상태 이름)
    #[error("the remote cluster answered HTTP {0}: {1}")]
    Status(u16, String),
    #[error("{0}")]
    Invalid(String),
}

fn unreachable(e: impl std::fmt::Display) -> RemoteError {
    RemoteError::Unreachable(e.to_string())
}

fn invalid(e: impl std::fmt::Display) -> RemoteError {
    RemoteError::Invalid(e.to_string())
}

/// 원격 클러스터의 CA 인증서(PEM, 정확히 하나). 서버 인증서를 검증하지 않고 `GET /api/v1/cluster/ca`를
/// 읽으므로, 부르는 쪽이 지문을 비교해야 한다. 아무 주소에나 연결하므로 오류에 원격이 준 말(오류 본문,
/// 연결·TLS 오류의 자세한 내용)을 옮기지 않는다.
pub async fn fetch_ca(url: &str) -> Result<String, RemoteError> {
    let target = Target::parse(url)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(AnyCert {
        algorithms: provider.signature_verification_algorithms,
    });
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(invalid)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let request = Request::get(target.uri("/cluster/ca")?)
        .header(HOST, &target.authority)
        .body(Empty::<Bytes>::new())
        .map_err(invalid)?;
    let response = send(&target, Arc::new(config), request, HEADER_TIMEOUT)
        .await
        .map_err(plain)?;
    let status = response.status();
    if !status.is_success() {
        let reason = status.canonical_reason().unwrap_or("error");
        return Err(RemoteError::Status(status.as_u16(), reason.to_string()));
    }
    let body = read_limited(&mut response.into_body(), MAX_CA_BYTES)
        .await
        .map_err(plain)?
        .ok_or_else(|| {
            invalid(format!(
                "the CA certificate is larger than {MAX_CA_BYTES} bytes"
            ))
        })?;
    let pem = String::from_utf8(body)
        .ok()
        .filter(|pem| opl_peer::pki::certs(pem).is_ok())
        .ok_or_else(|| invalid("the address is not an opLite cluster (no CA certificate)"))?;
    single_cert(&pem)?;
    Ok(pem)
}

/// 시간 제한 밖의 연결·TLS 오류를 정해진 말로 바꾼다(원격이 준 말을 옮기지 않게).
fn plain(e: RemoteError) -> RemoteError {
    match e {
        RemoteError::Unreachable(m) if m.starts_with("timed out") => RemoteError::Unreachable(m),
        RemoteError::Unreachable(_) => unreachable("cannot connect"),
        RemoteError::Tls(_) => RemoteError::Tls("TLS handshake failed".into()),
        e => e,
    }
}

/// PEM의 인증서 하나. 덧붙인 다른 CA를 믿게 되지 않게 둘 이상이면 받지 않는다.
fn single_cert(pem: &str) -> Result<CertificateDer<'static>, RemoteError> {
    let mut certs = opl_peer::pki::certs(pem).map_err(invalid)?;
    if certs.len() != 1 {
        return Err(invalid("the remote CA must be a single certificate"));
    }
    Ok(certs.remove(0))
}

/// 원격 클러스터 하나의 클라이언트. 경로는 `/api/v1` 아래 경로다(예: `/vms`).
pub struct RemoteClient {
    target: Target,
    config: Arc<ClientConfig>,
    authorization: HeaderValue,
}

// 토큰이 로그에 나가지 않게 주소만 보인다.
impl std::fmt::Debug for RemoteClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteClient")
            .field("authority", &self.target.authority)
            .finish_non_exhaustive()
    }
}

impl RemoteClient {
    /// `ca_pem`(정확히 인증서 하나)이 서명한 인증서만 믿는다(호스트 이름은 보지 않는다). `token`은
    /// 원격의 API 토큰이다.
    pub fn new(url: &str, ca_pem: &str, token: &str) -> Result<Self, RemoteError> {
        let target = Target::parse(url)?;
        let mut roots = RootCertStore::empty();
        roots.add(single_cert(ca_pem)?).map_err(invalid)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let webpki = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(invalid)?;
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyName(webpki)))
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| invalid("the API token is not a valid header value"))?;
        authorization.set_sensitive(true);
        Ok(Self {
            target,
            config: Arc::new(config),
            authorization,
        })
    }

    /// `GET <path>`의 JSON 본문(4 MiB까지). 2xx가 아니면 [`RemoteError::Status`].
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, RemoteError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let response = self
            .request(Method::GET, path, headers, Empty::<Bytes>::new())
            .await?;
        let body = success(response, MAX_JSON_BYTES, path).await?;
        serde_json::from_slice(&body).map_err(|e| {
            invalid(format!(
                "the remote cluster answered invalid JSON for {path}: {e}"
            ))
        })
    }

    /// 요청 하나를 보내고 응답을 그대로 돌려준다(상태를 보지 않고 본문 한도도 없다). 본문은 흘려
    /// 보내고, 보내는 본문과 받는 본문 모두 조각 사이가 60초를 넘으면 끊는다. 응답 헤더는 본문을 다
    /// 보낸 뒤 10초까지 기다린다. `headers`에 더해 `Host`와 `Authorization`(저장한 토큰)을 이
    /// 클라이언트가 채운다.
    pub async fn request<B>(
        &self,
        method: Method,
        path_and_query: &str,
        headers: HeaderMap,
        body: B,
    ) -> Result<Response<RemoteBody>, RemoteError>
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<BoxError>,
    {
        self.request_with_timeout(method, path_and_query, headers, body, HEADER_TIMEOUT)
            .await
    }

    /// [`Self::request`]와 같되 응답 헤더를 본문을 다 보낸 뒤 `header_timeout`까지 기다린다. 받는 쪽이
    /// 큰 파일을 내려 쓰고 답하는 업로드가 쓴다.
    pub async fn request_with_timeout<B>(
        &self,
        method: Method,
        path_and_query: &str,
        headers: HeaderMap,
        body: B,
        header_timeout: Duration,
    ) -> Result<Response<RemoteBody>, RemoteError>
    where
        B: Body<Data = Bytes> + Send + Unpin + 'static,
        B::Error: Into<BoxError>,
    {
        let mut request = Request::builder()
            .method(method)
            .uri(self.target.uri(path_and_query)?)
            .body(body)
            .map_err(invalid)?;
        *request.headers_mut() = headers;
        request.headers_mut().insert(
            HOST,
            HeaderValue::from_str(&self.target.authority).map_err(invalid)?,
        );
        request
            .headers_mut()
            .insert(AUTHORIZATION, self.authorization.clone());
        send(&self.target, self.config.clone(), request, header_timeout).await
    }
}

/// [`RemoteClient::request`] 응답의 본문(조각 사이 60초 제한)
pub type RemoteBody = IdleTimeout<Incoming>;

/// 연결할 곳
struct Target {
    /// 연결할 호스트(IPv6 주소는 괄호 없이)
    host: String,
    port: u16,
    /// `Host` 헤더 값(URL의 `<호스트>[:포트]` 그대로)
    authority: String,
}

impl Target {
    fn parse(url: &str) -> Result<Self, RemoteError> {
        let url = normalize_url(url).map_err(invalid)?;
        let authority = url["https://".len()..].to_string();
        let (host, port) = match authority.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').unwrap_or((v6, ""));
                (host, after.strip_prefix(':'))
            }
            None => match authority.split_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority.as_str(), None),
            },
        };
        let port = match port {
            Some(port) => port.parse().map_err(invalid)?,
            None => 443,
        };
        Ok(Self {
            host: host.to_string(),
            port,
            authority,
        })
    }

    /// `/api/v1` 아래 경로(`/`로 시작)의 요청 URI
    fn uri(&self, path_and_query: &str) -> Result<Uri, RemoteError> {
        if !path_and_query.starts_with('/') {
            return Err(invalid(format!("invalid remote path {path_and_query:?}")));
        }
        format!("/api/v1{path_and_query}")
            .parse()
            .map_err(|_| invalid(format!("invalid remote path {path_and_query:?}")))
    }
}

/// 새 연결을 열어 요청 하나를 보내고 응답 헤더를 본문을 다 보낸 뒤 `header_timeout`까지 기다린다.
async fn send<B>(
    target: &Target,
    config: Arc<ClientConfig>,
    request: Request<B>,
    header_timeout: Duration,
) -> Result<Response<RemoteBody>, RemoteError>
where
    B: Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<BoxError>,
{
    let (parts, body) = request.into_parts();
    let (sent_tx, sent_rx) = oneshot::channel();
    // 본문이 없으면 곧바로 응답 헤더 시간 제한을 센다.
    let sent_tx = (!body.is_end_stream()).then_some(sent_tx);
    let request = Request::from_parts(parts, IdleTimeout::new(body, sent_tx));
    let tls = connect(target, config).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(unreachable)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    // 응답 헤더는 본문을 다 보낸 뒤(또는 보내기를 그만둔 뒤) `header_timeout`까지 기다린다.
    let header_deadline = async {
        let _ = sent_rx.await;
        tokio::time::sleep(header_timeout).await;
    };
    let response = tokio::select! {
        response = sender.send_request(request) => response.map_err(unreachable)?,
        () = header_deadline => return Err(unreachable("timed out waiting for a response")),
    };
    Ok(response.map(|body| IdleTimeout::new(body, None)))
}

/// TCP와 TLS를 연다.
async fn connect(
    target: &Target,
    config: Arc<ClientConfig>,
) -> Result<TlsStream<TcpStream>, RemoteError> {
    // IP 주소면 SNI를 보내지 않는다(rustls가 알아서 뺀다).
    let name = ServerName::try_from(target.host.clone())
        .map_err(|_| invalid(format!("invalid host name {}", target.host)))?;
    let connector = TlsConnector::from(config);
    let result = tokio::time::timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect((target.host.as_str(), target.port))
            .await
            .map_err(unreachable)?;
        connector
            .connect(name, tcp)
            .await
            .map_err(|e| RemoteError::Tls(e.to_string()))
    })
    .await;
    match result {
        Err(_) => Err(unreachable("timed out connecting")),
        Ok(result) => result,
    }
}

/// 2xx면 본문(`limit`까지), 아니면 [`RemoteError::Status`]
async fn success(
    response: Response<RemoteBody>,
    limit: usize,
    what: &str,
) -> Result<Vec<u8>, RemoteError> {
    let status = response.status();
    let mut body = response.into_body();
    if status.is_success() {
        return read_limited(&mut body, limit)
            .await?
            .ok_or_else(|| invalid(format!("{what} is larger than {limit} bytes")));
    }
    // 오류 본문의 `message`(opLite 오류 응답), 없으면 상태 이름
    let message = read_limited(&mut body, MAX_ERROR_BYTES)
        .await
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_string());
    Err(RemoteError::Status(status.as_u16(), message))
}

/// 본문을 `limit`까지 읽는다. 넘으면 None.
async fn read_limited(body: &mut RemoteBody, limit: usize) -> Result<Option<Vec<u8>>, RemoteError> {
    let mut out = Vec::new();
    while let Some(frame) = body.frame().await {
        let Ok(data) = frame.map_err(unreachable)?.into_data() else {
            continue;
        };
        if out.len() + data.len() > limit {
            return Ok(None);
        }
        out.extend_from_slice(&data);
    }
    Ok(Some(out))
}

/// 조각 사이가 60초를 넘으면 오류로 끝나는 본문. `done`은 본문이 끝나거나 버려질 때 닫힌다.
pub struct IdleTimeout<B> {
    inner: B,
    idle: Pin<Box<Sleep>>,
    done: Option<oneshot::Sender<()>>,
}

impl<B> IdleTimeout<B> {
    fn new(inner: B, done: Option<oneshot::Sender<()>>) -> Self {
        Self {
            inner,
            idle: Box::pin(tokio::time::sleep(IDLE_TIMEOUT)),
            done,
        }
    }
}

impl<B> Body for IdleTimeout<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                this.idle.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                if !matches!(frame, Some(Ok(_))) {
                    this.done.take();
                }
                Poll::Ready(frame.map(|f| f.map_err(Into::into)))
            }
            Poll::Pending => match this.idle.as_mut().poll(cx) {
                Poll::Ready(()) => {
                    this.done.take();
                    Poll::Ready(Some(Err("timed out waiting for body data".into())))
                }
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// 원격 CA가 서명했는지는 확인하고 이름이 맞지 않는 것만 받아들인다.
#[derive(Debug)]
struct AnyName(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for AnyName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // webpki는 서명·기간을 먼저 확인하고 이름은 마지막에 본다.
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// 서버 인증서를 검증하지 않는다(CA를 받을 때만 쓴다). 핸드셰이크 서명은 확인한다.
#[derive(Debug)]
struct AnyCert {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_split_host_and_port() {
        for (url, host, port, authority) in [
            ("https://h", "h", 443, "h"),
            ("https://h:8443/", "h", 8443, "h:8443"),
            ("https://10.0.0.1:8443", "10.0.0.1", 8443, "10.0.0.1:8443"),
            ("https://[::1]", "::1", 443, "[::1]"),
            ("https://[::1]:8443", "::1", 8443, "[::1]:8443"),
        ] {
            let t = Target::parse(url).unwrap();
            assert_eq!(
                (t.host.as_str(), t.port, t.authority.as_str()),
                (host, port, authority),
                "{url}"
            );
        }
        assert!(matches!(
            Target::parse("http://h"),
            Err(RemoteError::Invalid(_))
        ));
    }

    #[test]
    fn paths_go_under_api_v1() {
        let t = Target::parse("https://h").unwrap();
        assert_eq!(t.uri("/vms?x=1").unwrap(), "/api/v1/vms?x=1");
        for bad in ["vms", "", "/a b"] {
            assert!(matches!(t.uri(bad), Err(RemoteError::Invalid(_))), "{bad}");
        }
    }
}
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

pub(crate) mod backup;
mod clone;
mod import;
mod migrate;
mod replicate;
mod snapshot;

pub(crate) use clone::volume_name;

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
use crate::updates::Updates;
use crate::vm_imports::ImportCredentials;
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
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명. 가져오기 Task가 쓰고, 끝나거나 지우면 지운다. API
    /// (`AppState::import_credentials`)와 같은 것을 넘긴다.
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 업데이트 확인. 없으면(업데이트가 꺼진 노드) 업데이트 확인 Task는 실패한다.
    pub updates: Option<Arc<Updates>>,
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
            import_credentials: Arc::default(),
            updates: None,
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
/// (복원 Task의 대상은 새 VM이다). 복제도 그 백업의 줄에 서서 보내는 동안 지우기가 파일을 지우지
/// 않는다. VM 가져오기와 가져오기 지우기도 그 가져오기의 줄에 선다(가져오기 Task의 대상은 새 VM이다).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    match task.spec.op {
        TaskOp::BackupRestore { backup, .. } => return Some(backup),
        TaskOp::VmImport { import, .. } => return Some(import),
        _ => {}
    }
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. } | TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
        );
        let backup_queue = matches!(
            task.spec.op,
            TaskOp::BackupDelete | TaskOp::BackupReplicate { .. } | TaskOp::VmImportDelete
        );
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
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다. 업그레이드 Task는 opld 밖의
/// 유닛에서 돌고 opld를 다시 시작하게 하므로 실패로 기록하지 않고 그 유닛을 이어서 지켜본다.
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
        if interrupted && task.spec.op != TaskOp::NodeUpgrade {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task, interrupted).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

/// `resume`이면 이전 opld가 실행하던 Task를 이어서 한다(업그레이드만).
async fn execute(ctx: &Ctx, task: Task, resume: bool) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업과 복원, 복제, VM 가져오기는 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다
    // (설계 §2.3, 복제는 원격 클라이언트가 조각 사이 60초를 본다).
    // 업그레이드도 오래 걸릴 수 있고 apt를 중간에 끊으면 더 나쁘다.
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::BackupRestore { .. }
            | TaskOp::BackupReplicate { .. }
            | TaskOp::VmImport { .. }
            | TaskOp::NodeUpgrade
    );
    let (result, unfinished) = if unlimited {
        (perform(ctx, &task, resume).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task, resume)).await {
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

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제·복원·VM 가져오기의 흔적을 지워 이름을 다시 쓸 수 있게
/// 한다. 실패한 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다. 실패한 복제는 원본의 사본 기록을
/// `failed`와 `error`로 바꾼다. 실패한 VM 가져오기는 `ready`로 돌아가 `error`를 남긴다.
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
    if let TaskOp::VmClone { .. } | TaskOp::BackupRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if let TaskOp::VmBackup { backup } = task.spec.op {
        backup::discard(ctx, task, backup, error, unfinished).await;
    }
    if let TaskOp::BackupReplicate { remote, pool } = &task.spec.op {
        replicate::discard(ctx, task.spec.target, remote, pool, error).await;
    }
    if let TaskOp::VmImport { import, .. } = task.spec.op {
        import::discard(ctx, task, import, error).await;
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

async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<(), String> {
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
        TaskOp::BackupRestore { backup, name, pool } => {
            backup::restore(ctx, task, *backup, name, pool).await
        }
        TaskOp::BackupReplicate { remote, pool } => {
            replicate::replicate(ctx, id, remote, pool).await
        }
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
        TaskOp::NodeUpdateCheck => match &ctx.config.updates {
            Some(updates) => updates.refresh_waiting(&ctx.cluster, ctx.config.node).await,
            None => Err("updates are not enabled on this node".into()),
        },
        TaskOp::NodeUpgrade => match &ctx.config.updates {
            Some(updates) => {
                updates
                    .upgrade(&ctx.cluster, ctx.config.node, task.id, resume)
                    .await
            }
            None => Err("updates are not enabled on this node".into()),
        },
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
/// 올린 파일, VM 가져오기의 남은 파일([`crate::vm_imports::sweep`]).
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
    crate::vm_imports::sweep(&ctx.cluster, ctx.config.node, dir, revision, tasks).await;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 하위 디렉터리(`imports`)는 가져오기의 정리가 맡는다.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
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

`crates/opl-api/src/tasks/backup.rs`에서

```rust

fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
```

을 다음으로 바꾼다:

```rust

pub(super) fn get_backup(ctx: &Ctx, id: Uuid) -> Result<Option<Backup>, String> {
    ctx.cluster.get::<BackupSpec>(id).map_err(|e| e.to_string())
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면 오류다.
fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
    let pool = ready_pool(ctx, name)?;
```

을 다음으로 바꾼다:

```rust
/// 백업 풀 `name`과 이 노드에서 그 풀의 루트. 풀이 이 노드에서 준비되지 않았으면 오류다.
pub(super) fn backup_root(ctx: &Ctx, name: &str) -> Result<(StoragePool, PathBuf), String> {
    let pool = ready_pool(ctx, name)?;
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 백업 디렉터리의 매니페스트를 읽는다. 형식 버전이 다르면 나머지를 읽지 않고 오류다.
async fn read_manifest(dir: &Path) -> Result<BackupManifest, String> {
    let text = tokio::fs::read(dir.join(MANIFEST_FILE))
```

을 다음으로 바꾼다:

```rust
/// 백업 디렉터리의 매니페스트를 읽는다. 형식 버전이 다르면 나머지를 읽지 않고 오류다.
pub(super) async fn read_manifest(dir: &Path) -> Result<BackupManifest, String> {
    let text = tokio::fs::read(dir.join(MANIFEST_FILE))
```

`crates/opl-api/src/tasks/replicate.rs`를 만든다:

```rust
//! 백업 복제 Task(엔터프라이즈 설계 §6.2): `ready` 백업을 원격 클러스터의 받기 API(§6.1)로 보내 그 백업
//! 풀에 사본을 만든다. 받기를 시작하고(적어 둔 사본이 원격에 아직 있거나 같은 원본의 사본이 있으면, 이
//! 백업의 사본인지 확인하고 그것으로 이어 간다. ready면 받은 파일의 해시가 원본에 적힌 해시와 같아야 끝난
//! 것이다), 파일마다 읽으면서 SHA-256을 계산해 보내고 원격이 받은 해시·크기와 비교해 원본 디스크에 그
//! 해시를 적은 뒤, 완료를 요청한다. 원본 `status.replicas`의 (원격, 풀) 항목을 copying으로 시작해 ready나
//! failed(실행기의 정리)로 바꾼다. 이미 ready인 사본의 확인은 기록을 ready로 둔 채 원격에 묻고, 다시
//! 보내야 할 때만 copying으로 바꾼다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::Method;
use hyper::body::{Body, Bytes, Frame};
use hyper::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupOrigin, BackupPhase, BackupReplica, BackupSpec,
    ReplicaState, backup_dir,
};
use opl_types::backup_job::BackupJobSpec;
use opl_types::cluster::ClusterSpec;
use opl_types::remote::RemoteSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::Ctx;
use super::backup::{VARS_FILE, backup_root, get_backup, read_manifest};
use crate::backups::now;
use crate::remote_client::{RemoteClient, RemoteError};

/// 파일을 읽어 보내는 조각의 크기
const CHUNK: usize = 256 << 10;
/// 원격 JSON 응답 본문의 최대 크기
const MAX_JSON_BYTES: usize = 4 << 20;
/// 파일을 다 보낸 뒤 응답을 기다리는 시간. 받는 쪽은 큰 파일을 디스크에 내려 쓰고서 답한다.
const FILE_ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// 원본 백업 `id`의 (`remote`, `pool`) 사본 기록을 `change`로 바꾼다(없으면 새로 만든다). 다른 쪽이 그사이
/// 쓴 상태를 덮지 않게 읽은 revision에서만 쓰고, 기록될 때까지 다시 시도한다. 백업이 없어졌으면 그만둔다.
async fn record(
    ctx: &Ctx,
    id: Uuid,
    remote: &str,
    pool: &str,
    change: impl Fn(&mut BackupReplica),
) {
    loop {
        let backup = match get_backup(ctx, id) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot read backup; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = backup.status.clone();
        let at = status
            .replicas
            .iter()
            .position(|r| r.remote == remote && r.pool == pool);
        let i = at.unwrap_or_else(|| {
            status.replicas.push(BackupReplica {
                remote: remote.to_string(),
                pool: pool.to_string(),
                backup: None,
                state: ReplicaState::Copying,
                error: None,
                at: 0,
            });
            status.replicas.len() - 1
        });
        change(&mut status.replicas[i]);
        status.replicas[i].at = now();
        match ctx
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot record backup replica; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 사본을 보내기 시작한 기록
fn copying(r: &mut BackupReplica) {
    r.state = ReplicaState::Copying;
    r.error = None;
}

/// 원격 사본이 이 백업의 사본이 아니다: 기록을 failed로 바꾸고 `error`를 돌려준다(ready 기록이라도).
async fn mismatch(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: String) -> String {
    record(ctx, id, remote, pool, |r| r.state = ReplicaState::Failed).await;
    error
}

/// 백업 `id`를 원격 `remote`의 백업 풀 `pool`에 사본으로 보낸다. 실패하면 실행기의 정리([`discard`])가
/// 사본 기록을 failed로 바꾼다. 기록이 ready인 사본은 확인만 하므로 기록을 ready로 둔다(확인이 실패해도
/// 이유만 적힌다). 원격에 없거나 다른 사본이면 그때 copying이나 failed로 바꾼다.
pub(super) async fn replicate(ctx: &Ctx, id: Uuid, remote: &str, pool: &str) -> Result<(), String> {
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    let checking = backup.status.replicas.iter().any(|r| {
        r.remote == remote && r.pool == pool && r.state == ReplicaState::Ready && r.backup.is_some()
    });
    if !checking {
        record(ctx, id, remote, pool, copying).await;
    }
    let copy = send(ctx, &backup, remote, pool, checking).await?;
    record(ctx, id, remote, pool, |r| {
        r.backup = Some(copy);
        r.state = ReplicaState::Ready;
        r.error = None;
    })
    .await;
    Ok(())
}

/// 실패한 복제 Task의 사본 기록을 failed와 `error`로 바꾼다. ready 기록은 확인하다 실패한 것이므로(원격에
/// 닿지 못함 등) ready로 두고 `error`만 적는다. 다음 실행이 다시 확인한다.
pub(super) async fn discard(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: &str) {
    record(ctx, id, remote, pool, |r| {
        if r.state != ReplicaState::Ready {
            r.state = ReplicaState::Failed;
        }
        r.error = Some(error.to_string());
    })
    .await;
}

/// 원격의 오류(사람이 읽을 말)
fn remote_error(remote: &str, doing: &str, e: RemoteError) -> String {
    format!("cannot {doing} on remote {remote:?}: {e}")
}

/// 파일을 보내고 원격 사본의 id를 돌려준다. `checking`이면 기록이 ready인 사본을 확인하는 것이고, 파일을
/// 다시 보내야 할 때 기록을 copying으로 바꾼다.
async fn send(
    ctx: &Ctx,
    backup: &Backup,
    remote: &str,
    pool: &str,
    checking: bool,
) -> Result<Uuid, String> {
    let id = backup.id;
    let spec = ctx
        .cluster
        .get_by_name::<RemoteSpec>(remote)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote {remote:?} not found"))?
        .spec;
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &backup.spec.pool)?;
    let dir = backup_dir(&root, id);
    let manifest = read_manifest(&dir).await?;
    if manifest.backup != id || manifest.vm_spec != backup.spec.vm_spec {
        return Err("backup manifest does not match the backup record".into());
    }
    let origin = origin(ctx, backup)?;
    let client = RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token)
        .map_err(|e| remote_error(remote, "connect", e))?;

    let request = json!({
        "name": backup.name,
        "pool": pool,
        "manifest": manifest,
        "origin": origin,
    });
    let start = |e| remote_error(remote, "start the copy", e);
    // 앞의 복제가 적은 사본이 원격에 아직 있으면 그것을 이어 간다. 원격에서 지웠으면(404) 새로 받는다.
    let recorded = backup
        .status
        .replicas
        .iter()
        .find(|r| r.remote == remote && r.pool == pool)
        .and_then(|r| r.backup);
    let existing = match recorded {
        Some(copy) => get_copy(&client, copy).await.map_err(start)?,
        None => None,
    };
    if checking && existing.is_none() {
        record(ctx, id, remote, pool, copying).await;
    }
    let (copy, existing) = match existing {
        Some(got) => (got.id, Some(got)),
        None => {
            let (status, body) = call(&client, Method::POST, "/backups/receive", Some(&request))
                .await
                .map_err(start)?;
            match status {
                201 => (body_id(&body, "id").map_err(start)?, None),
                // 앞의 복제가 만든 사본이다(기록하기 전에 멈췄다).
                409 if body["code"] == "replica_exists" => {
                    let copy = body_id(&body["details"], "backup").map_err(start)?;
                    let got = get_copy(&client, copy)
                        .await
                        .map_err(start)?
                        .ok_or_else(|| {
                            format!("the copy {copy} on remote {remote:?} was deleted; try again")
                        })?;
                    (copy, Some(got))
                }
                _ => return Err(start(status_error(status, &body))),
            }
        }
    };
    if let Some(got) = &existing {
        if let Err(e) = check_copy(got, backup, &manifest, remote, pool) {
            return Err(mismatch(ctx, id, remote, pool, e).await);
        }
        // 이미 ready면 받은 파일의 해시가 원본과 같을 때만 다 보낸 것이다. ready 사본은 파일을 다시
        // 받지 않으므로 다르면 실패로 알린다.
        if got.status.phase == BackupPhase::Ready {
            if same_files(ctx, backup, got, &dir).await? {
                return Ok(copy);
            }
            let e = format!(
                "the copy {copy} on remote {remote:?} is ready but its files do not match backup \
                 {:?} (or were not hashed); delete it there and replicate again",
                backup.name
            );
            return Err(mismatch(ctx, id, remote, pool, e).await);
        }
    }
    record(ctx, id, remote, pool, |r| {
        r.backup = Some(copy);
        copying(r);
    })
    .await;

    let mut files: Vec<&str> = manifest.disks.iter().map(|d| d.file.as_str()).collect();
    if manifest.uefi_vars {
        files.push(VARS_FILE);
    }
    for file in files {
        let sha256 = send_file(&client, remote, copy, &dir.join(file), file).await?;
        record_hash(ctx, id, file, &sha256).await;
    }
    let (status, body) = call(
        &client,
        Method::POST,
        &format!("/backups/{copy}/actions/complete"),
        None,
    )
    .await
    .map_err(|e| remote_error(remote, "complete the copy", e))?;
    expect(200, status, body).map_err(|e| remote_error(remote, "complete the copy", e))?;
    Ok(copy)
}

/// 원격 사본 `copy`(없으면 `None`)
async fn get_copy(client: &RemoteClient, copy: Uuid) -> Result<Option<Backup>, RemoteError> {
    let (status, body) = call(client, Method::GET, &format!("/backups/{copy}"), None).await?;
    match status {
        404 => Ok(None),
        _ => {
            let body = expect(200, status, body)?;
            serde_json::from_value(body).map(Some).map_err(|e| {
                RemoteError::Invalid(format!("the remote answered an unexpected backup: {e}"))
            })
        }
    }
}

/// 원격 백업 `got`이 이 백업(`manifest`)을 풀 `pool`에 받는 사본인지 본다: 출처의 원본 id, 풀, VM 스펙,
/// 디스크 이름과 크기, UEFI 변수, 받는 중이거나 ready. 다르면 원격에서 지우라는 오류다.
fn check_copy(
    got: &Backup,
    backup: &Backup,
    manifest: &BackupManifest,
    remote: &str,
    pool: &str,
) -> Result<(), String> {
    let disks = |d: &[BackupDisk]| -> Vec<(String, u64)> {
        d.iter().map(|d| (d.name.clone(), d.stored_bytes)).collect()
    };
    let same = got.spec.origin.as_ref().map(|o| o.backup) == Some(backup.id)
        && got.spec.pool == pool
        && got.spec.vm_spec == backup.spec.vm_spec
        && disks(&got.status.disks) == disks(&manifest.disks)
        && got.status.uefi_vars == manifest.uefi_vars
        && matches!(
            got.status.phase,
            BackupPhase::Receiving | BackupPhase::Ready
        );
    if same {
        Ok(())
    } else {
        Err(format!(
            "a different backup claims to be the copy of {} on remote {remote:?}; delete it there",
            backup.id
        ))
    }
}

/// ready 사본 `got`이 받은 디스크 파일의 해시가 원본 `backup`에 적힌 해시와 모두 같은지. 원본에 해시가
/// 없는 디스크(해시를 적기 전의 백업)만 디렉터리 `dir`의 파일을 읽어 계산하고 원본에 적는다.
async fn same_files(
    ctx: &Ctx,
    backup: &Backup,
    got: &Backup,
    dir: &std::path::Path,
) -> Result<bool, String> {
    for (disk, copied) in backup.status.disks.iter().zip(&got.status.disks) {
        let Some(copied) = &copied.sha256 else {
            return Ok(false);
        };
        let sha256 = match &disk.sha256 {
            Some(sha256) => sha256.clone(),
            None => {
                let sha256 = file_sha256(&dir.join(&disk.file)).await?;
                record_hash(ctx, backup.id, &disk.file, &sha256).await;
                sha256
            }
        };
        if *copied != sha256 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 원본 백업 `id`의 디스크 파일 `file`에 SHA-256을 적는다(이미 같으면 쓰지 않는다. 디스크 파일이 아니면
/// 할 일이 없다). 읽은 revision에서만 쓰고, 기록될 때까지 다시 시도한다. 백업이 없어졌으면 그만둔다.
async fn record_hash(ctx: &Ctx, id: Uuid, file: &str, sha256: &str) {
    loop {
        let backup = match get_backup(ctx, id) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot read backup; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = backup.status.clone();
        let Some(disk) = status.disks.iter_mut().find(|d| d.file == file) else {
            return;
        };
        if disk.sha256.as_deref() == Some(sha256) {
            return;
        }
        disk.sha256 = Some(sha256.to_string());
        match ctx
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot record backup hash; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 파일 `path`의 SHA-256(16진수 소문자)
async fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let reading = |e: std::io::Error| format!("cannot read {}: {e}", path.display());
    let mut file = tokio::fs::File::open(path).await.map_err(reading)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = file.read(&mut buf).await.map_err(reading)?;
        if n == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

/// 원격 사본의 출처: 이 클러스터 이름과 CA 지문, 원본 id, 원본 작업 이름(작업이 아직 있으면)
fn origin(ctx: &Ctx, backup: &Backup) -> Result<BackupOrigin, String> {
    let info = ctx
        .cluster
        .list::<ClusterSpec>()
        .map_err(|e| e.to_string())?
        .1
        .into_iter()
        .next()
        .ok_or("the cluster has not been initialized")?;
    let ca_fingerprint =
        opl_peer::pki::fingerprint(&info.spec.ca_cert_pem).map_err(|e| e.to_string())?;
    let job = match backup.spec.job {
        Some(job) => ctx
            .cluster
            .get::<BackupJobSpec>(job)
            .map_err(|e| e.to_string())?
            .map(|j| j.name),
        None => None,
    };
    Ok(BackupOrigin {
        cluster: info.name,
        ca_fingerprint,
        backup: backup.id,
        job,
    })
}

/// 파일 `path`를 원격 사본 `copy`의 `file`로 보내고 그 SHA-256을 돌려준다. 읽으면서 계산한 SHA-256·크기가
/// 원격이 받은 것과 다르면 오류다.
async fn send_file(
    client: &RemoteClient,
    remote: &str,
    copy: Uuid,
    path: &std::path::Path,
    file: &str,
) -> Result<String, String> {
    let opened = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    let size = opened
        .metadata()
        .await
        .map_err(|e| format!("cannot read {file}: {e}"))?
        .len();
    let hash = Arc::new(Mutex::new(Sha256::new()));
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_LENGTH, HeaderValue::from(size));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let sending = |e| remote_error(remote, &format!("send {file}"), e);
    let response = client
        .request_with_timeout(
            Method::PUT,
            &format!("/backups/{copy}/files/{file}"),
            headers,
            hashing_body(opened, hash.clone()),
            FILE_ANSWER_TIMEOUT,
        )
        .await
        .map_err(sending)?;
    let (status, body) = read_json(response).await.map_err(sending)?;
    let got = expect(200, status, body).map_err(sending)?;
    let sent = hex::encode(hash.lock().expect("hash lock").clone().finalize());
    if got["sha256"].as_str() != Some(sent.as_str()) || got["size"].as_u64() != Some(size) {
        return Err(format!(
            "remote {remote:?} received {file} with SHA-256 {} and {} bytes; sent SHA-256 {sent} and {size} bytes",
            got["sha256"], got["size"]
        ));
    }
    Ok(sent)
}

/// 파일을 조각으로 읽어 보내며 `hash`에 더하는 본문
fn hashing_body(
    file: tokio::fs::File,
    hash: Arc<Mutex<Sha256>>,
) -> impl Body<Data = Bytes, Error = std::io::Error> + Send + Unpin + 'static {
    let chunks = stream::unfold(Some(file), move |file| {
        let hash = hash.clone();
        async move {
            let mut file = file?;
            let mut buf = vec![0; CHUNK];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    hash.lock().expect("hash lock").update(&buf);
                    Some((Ok(Frame::data(Bytes::from(buf))), Some(file)))
                }
                // 읽기 오류는 보내기를 끊는다.
                Err(e) => Some((Err(e), None)),
            }
        }
    });
    StreamBody::new(Box::pin(chunks))
}

/// 요청 하나를 보내고 상태와 JSON 본문(없거나 JSON이 아니면 null)을 돌려준다.
async fn call(
    client: &RemoteClient,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), RemoteError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let body = match body {
        Some(body) => {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Bytes::from(serde_json::to_vec(body).map_err(|e| RemoteError::Invalid(e.to_string()))?)
        }
        None => Bytes::new(),
    };
    let response = client
        .request(method, path, headers, Full::new(body))
        .await?;
    read_json(response).await
}

async fn read_json(
    response: hyper::Response<crate::remote_client::RemoteBody>,
) -> Result<(u16, Value), RemoteError> {
    let status = response.status().as_u16();
    let bytes = Limited::new(response.into_body(), MAX_JSON_BYTES)
        .collect()
        .await
        .map_err(|e| RemoteError::Unreachable(e.to_string()))?
        .to_bytes();
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

/// 상태가 `want`면 본문, 아니면 [`RemoteError::Status`]
fn expect(want: u16, status: u16, body: Value) -> Result<Value, RemoteError> {
    if status == want {
        Ok(body)
    } else {
        Err(status_error(status, &body))
    }
}

/// 원격 오류 응답의 `message`(없으면 상태 번호만)
fn status_error(status: u16, body: &Value) -> RemoteError {
    let message = body["message"].as_str().unwrap_or("unexpected response");
    RemoteError::Status(status, message.to_string())
}

/// 본문 `body`의 `field`(UUID)
fn body_id(body: &Value, field: &str) -> Result<Uuid, RemoteError> {
    body[field]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| RemoteError::Invalid(format!("the remote answered without {field}")))
}
```

`crates/opl-api/tests/backup_replicate.rs`를 만든다:

```rust
//! 백업을 원격 클러스터로 복제하기: `POST /backups/{key}/actions/replicate`가 만든 `backup-replicate`
//! Task가 원격(두 번째 `Env`, 그 CA가 서명한 HTTPS)의 받기 API로 파일을 보내 사본을 만들고 원본
//! `status.replicas`에 적는다. 해시가 다른 응답, 꺼진 원격, 원격 쪽 권한, 같은 사본 이어 가기, API 검사.

mod common;

use std::path::PathBuf;

use axum::body::Body;
use axum::http::Method;
use common::{Client, Env, TlsServer, vm_request};
use opl_types::backup::{
    Backup, BackupManifest, BackupPhase, BackupSpec, BackupStatus, MANIFEST_FILE, ReplicaState,
    backup_dir,
};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 이 노드에서 준비된 백업 dir 풀 `name`을 저장소에 직접 만든다.
async fn backup_pool(env: &Env, name: &str) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups: true,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 원본 백업의 파일: 디스크 둘(하나는 여러 조각으로 갈 만큼 크다)과 UEFI 변수
fn files() -> Vec<(&'static str, Vec<u8>)> {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    vec![
        ("disk0.qcow2", b"disk-zero".to_vec()),
        ("disk1.qcow2", big),
        ("vars.fd", b"uefi-vars".to_vec()),
    ]
}

/// 풀 `backups`에 `ready` 백업 `name`과 그 파일·매니페스트를 직접 만든다. `job`은 원본 작업 id다.
async fn ready_backup(env: &Env, name: &str, job: Option<Uuid>) -> Backup {
    let mut spec = vm_request("web")["spec"].clone();
    spec["disks"] = json!([
        { "volume": "web-root", "bus": "virtio-blk" },
        { "volume": "web-data", "bus": "virtio-blk" }
    ]);
    let files = files();
    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec: serde_json::from_value(spec).unwrap(),
            job,
            origin: None,
        },
    );
    let manifest: BackupManifest = serde_json::from_value(json!({
        "version": 1,
        "backup": backup.id,
        "vm": backup.spec.vm,
        "vm_name": "web",
        "vm_spec": backup.spec.vm_spec,
        "disks": [
            { "name": "disk0", "file": "disk0.qcow2", "virtual_bytes": 1 << 20,
              "stored_bytes": files[0].1.len() },
            { "name": "disk1", "file": "disk1.qcow2", "virtual_bytes": 2 << 20,
              "stored_bytes": files[1].1.len() }
        ],
        "uefi_vars": true,
        "created_at": 1_793_077_629u64
    }))
    .unwrap();
    backup.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(manifest.created_at),
        disks: manifest.disks.clone(),
        uefi_vars: true,
        error: None,
        node: Some(env.node),
        replicas: vec![],
        received_at: None,
    };
    let dir = source_dir(env, backup.id);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, data) in &files {
        std::fs::write(dir.join(file), data).unwrap();
    }
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    env.cluster.create(&backup).await.unwrap();
    backup
}

fn source_dir(env: &Env, id: Uuid) -> PathBuf {
    backup_dir(&env.dir.path().join("backups"), id)
}

fn replica_dir(env: &Env, id: Uuid) -> PathBuf {
    backup_dir(&env.dir.path().join("dr"), id)
}

fn get_backup(env: &Env, id: Uuid) -> Backup {
    env.cluster.get::<BackupSpec>(id).unwrap().unwrap()
}

/// 원격 역할을 하는 클러스터(백업 풀 `dr`)와 그 HTTPS 서버
struct Remote {
    env: Env,
    server: TlsServer,
}

async fn remote_with(tweak: impl FnOnce(axum::Router) -> axum::Router) -> Remote {
    let env = Env::start().await;
    backup_pool(&env, "dr").await;
    let server = env.serve_tls_with(tweak).await;
    Remote { env, server }
}

async fn remote() -> Remote {
    remote_with(|app| app).await
}

/// 보내는 클러스터(백업 풀 `backups`)에 원격 `dr`을 원격 쪽 `role` 사용자의 토큰으로 등록한다.
async fn local_with_remote(r: &Remote, role: &str) -> (Env, Client) {
    let local = Env::start().await;
    backup_pool(&local, "backups").await;
    let admin = local.admin().await;
    let token = r.env.user("sender", role).await.token.unwrap();
    let (status, body) = admin
        .post(
            "/remotes",
            json!({
                "name": "dr",
                "url": r.server.url,
                "ca_fingerprint": r.env.ca_fingerprint(),
                "token": token,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (local, admin)
}

async fn replicate(client: &Client, backup: &str) -> (StatusCode, Value) {
    client
        .post(
            &format!("/backups/{backup}/actions/replicate"),
            json!({ "remote": "dr", "pool": "dr" }),
        )
        .await
}

/// 복제를 요청하고 Task가 끝날 때까지 기다린다.
async fn replicate_done(client: &Client, backup: &str) -> Value {
    let (status, task) = replicate(client, backup).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "backup-replicate", "remote": "dr", "pool": "dr" })
    );
    client.wait_task(&task).await
}

/// 원본에 적힌 원격 `dr`·풀 `dr`의 사본 기록 하나
fn only_replica(env: &Env, id: Uuid) -> opl_types::backup::BackupReplica {
    let replicas = get_backup(env, id).status.replicas;
    assert_eq!(replicas.len(), 1, "{replicas:?}");
    let replica = replicas[0].clone();
    assert_eq!(
        (replica.remote.as_str(), replica.pool.as_str()),
        ("dr", "dr")
    );
    assert!(replica.at > 0);
    replica
}

/// 복제는 원격의 백업 풀에 같은 파일의 `ready` 사본을 만든다: 이름은 원본 이름, 출처는 이 클러스터
/// 이름·CA 지문·원본 id·원본 작업 이름, 매니페스트의 백업 id는 원격 쪽 id. 원본에는 사본이 ready로
/// 적힌다. 원격 토큰은 operator면 된다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replication_makes_a_ready_copy_on_the_remote() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let (status, job) = admin
        .post(
            "/backup-jobs",
            json!({ "name": "nightly", "spec": {
                "vms": { "names": ["web"] }, "pool": "backups",
                "schedule": { "at": "03:00" }, "enabled": false,
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let job: Uuid = job["id"].as_str().unwrap().parse().unwrap();
    let source = ready_backup(&local, "web-nightly", Some(job)).await;

    let task = replicate_done(&admin, "web-nightly").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Ready);
    assert_eq!(replica.error, None);
    let id = replica.backup.expect("remote copy id");

    let copy = get_backup(&r.env, id);
    assert_eq!(copy.name, "web-nightly");
    assert_eq!(copy.status.phase, BackupPhase::Ready);
    assert_eq!(copy.spec.vm, source.spec.vm);
    assert_eq!(copy.spec.vm_spec, source.spec.vm_spec);
    let origin = copy.spec.origin.expect("origin");
    assert_eq!(origin.cluster, "test");
    assert_eq!(origin.ca_fingerprint, local.ca_fingerprint());
    assert_eq!(origin.backup, source.id);
    assert_eq!(origin.job.as_deref(), Some("nightly"));
    let dir = replica_dir(&r.env, id);
    for (file, data) in files() {
        assert_eq!(std::fs::read(dir.join(file)).unwrap(), data, "{file}");
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(dir.join(MANIFEST_FILE)).unwrap()).unwrap();
    assert_eq!(manifest.backup, id);
}

/// 원격이 보낸 것과 다른 SHA-256을 돌려주면 Task가 실패하고 complete하지 않는다(사본은 받는 중으로
/// 남는다). 원본에는 사본이 failed와 이유로 적힌다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_hash_from_the_remote_fails_the_copy() {
    // 파일을 받은 응답의 해시를 바꾸는 원격
    let r = remote_with(|app| {
        app.layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let tamper = req.method() == Method::PUT && req.uri().path().contains("/files/");
                let response = next.run(req).await;
                if !tamper || response.status() != StatusCode::OK {
                    return response;
                }
                let (parts, body) = response.into_parts();
                let bytes = axum::body::to_bytes(body, 1 << 16).await.unwrap();
                let mut got: Value = serde_json::from_slice(&bytes).unwrap();
                got["sha256"] = json!("0".repeat(64));
                let mut response = axum::response::Response::from_parts(
                    parts,
                    Body::from(serde_json::to_vec(&got).unwrap()),
                );
                response
                    .headers_mut()
                    .remove(axum::http::header::CONTENT_LENGTH);
                response
            },
        ))
    })
    .await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("SHA-256"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    let id = replica.backup.expect("remote copy id");
    assert_eq!(get_backup(&r.env, id).status.phase, BackupPhase::Receiving);
}

/// 원격에 닿지 못하면 Task가 실패하고 원본에 사본이 failed로 적힌다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_remote_fails_the_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    drop(r.server);

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot reach the remote cluster"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    assert_eq!(replica.backup, None);
}

/// 원격 쪽 권한은 토큰 주인의 역할이다: viewer 토큰이면 원격이 403으로 거절하고 그 이유가 남는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_viewer_token_on_the_remote_is_refused() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "viewer").await;
    let source = ready_backup(&local, "web-1", None).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("HTTP 403"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(replica.state, ReplicaState::Failed);
    assert_eq!(replica.error.as_deref(), Some(error));
    let (_, list) = r.env.admin().await.get("/backups").await;
    assert_eq!(list["items"], json!([]), "{list}");
}

/// 원격에 같은 원본의 사본이 이미 있으면(409 `replica_exists`) 그 사본으로 이어 간다: 받는 중이면
/// 파일을 (반쯤 보낸 것도 덮어) 다시 보내고 끝내며, 이미 ready면 받은 파일의 해시가 원본과 같을 때 그대로
/// 성공한다. 원격의 사본은 하나다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicating_again_continues_with_the_existing_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    // 앞의 복제가 받기만 하고 멈춘 것처럼 원격에 받는 중인 사본을 만든다.
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(source_dir(&local, source.id).join(MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    let remote_admin = r.env.admin().await;
    let (status, started) = remote_admin
        .post(
            "/backups/receive",
            json!({
                "name": "partial",
                "pool": "dr",
                "manifest": manifest,
                "origin": {
                    "cluster": "test",
                    "ca_fingerprint": local.ca_fingerprint(),
                    "backup": source.id,
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let id: Uuid = started["id"].as_str().unwrap().parse().unwrap();
    // 반쯤 보낸 파일: 다른 내용의 디스크 파일과 끝나지 않은 `.part`
    let put = remote_admin
        .http
        .put(format!(
            "{}/api/v1/backups/{id}/files/disk0.qcow2",
            remote_admin.base
        ))
        .bearer_auth(remote_admin.token.as_ref().unwrap())
        .body(b"XXXXXXXXX".to_vec());
    assert_eq!(remote_admin.send(put).await.0, StatusCode::OK);
    std::fs::write(replica_dir(&r.env, id).join("disk1.qcow2.part"), b"half").unwrap();

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup),
        (ReplicaState::Ready, Some(id))
    );
    let copy = get_backup(&r.env, id);
    assert_eq!(
        (copy.name.as_str(), copy.status.phase),
        ("partial", BackupPhase::Ready)
    );
    for (file, data) in files() {
        assert_eq!(
            std::fs::read(replica_dir(&r.env, id).join(file)).unwrap(),
            data
        );
    }

    // 한 번 더: 이미 ready인 사본이다.
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup),
        (ReplicaState::Ready, Some(id))
    );
    let (_, list) = remote_admin.get("/backups").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");

    // ready 사본의 해시가 원본과 다르면 실패로 알린다(사본은 그대로).
    let mut copy = get_backup(&r.env, id);
    copy.status.disks[1].sha256 = Some("0".repeat(64));
    r.env
        .cluster
        .update_status::<BackupSpec>(id, &copy.status)
        .await
        .unwrap();
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains(&id.to_string()), "{error}");
    assert!(error.contains("delete it there"), "{error}");
    assert_eq!(only_replica(&local, source.id).state, ReplicaState::Failed);
    assert_eq!(get_backup(&r.env, id).status.phase, BackupPhase::Ready);
}

/// 같은 원본의 사본이라고 하지만 풀·VM 스펙·디스크가 다른 원격 백업에는 붙지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_different_backup_claiming_the_origin_fails_the_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let mut manifest: Value = serde_json::from_slice(
        &std::fs::read(source_dir(&local, source.id).join(MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    manifest["vm_spec"]["memory_mib"] = json!(4096);
    let (status, started) = r
        .env
        .admin()
        .await
        .post(
            "/backups/receive",
            json!({
                "pool": "dr",
                "manifest": manifest,
                "origin": {
                    "cluster": "test",
                    "ca_fingerprint": local.ca_fingerprint(),
                    "backup": source.id,
                },
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{started}");

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(
        error.contains("a different backup claims to be the copy"),
        "{error}"
    );
    assert_eq!(only_replica(&local, source.id).state, ReplicaState::Failed);
}

/// 원격에서 사본을 지웠으면 다음 복제가 새로 받는다(적어 둔 사본 id를 원격에 물어본다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_deleted_on_the_remote_is_sent_again() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let first = only_replica(&local, source.id).backup.unwrap();
    let remote_admin = r.env.admin().await;
    let (status, task) = remote_admin.delete(&format!("/backups/{first}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    remote_admin.wait_task(&task).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    let second = replica.backup.unwrap();
    assert_ne!(second, first);
    assert_eq!(replica.state, ReplicaState::Ready);
    assert_eq!(get_backup(&r.env, second).status.phase, BackupPhase::Ready);
    for (file, data) in files() {
        assert_eq!(
            std::fs::read(replica_dir(&r.env, second).join(file)).unwrap(),
            data
        );
    }
}

/// 보낸 파일의 해시는 원본 디스크에 적는다. 이미 ready인 사본은 원본에 적힌 해시와 사본에 적힌 해시를
/// 비교할 뿐 파일을 다시 읽지 않는다(원본 파일이 없어도 확인한다). 해시가 적히지 않은 옛 백업은 파일을
/// 읽어 계산하고 적는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ready_copy_is_checked_by_the_recorded_hashes() {
    use sha2::{Digest, Sha256};
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let hashes = |env: &Env, id: Uuid| -> Vec<Option<String>> {
        get_backup(env, id)
            .status
            .disks
            .iter()
            .map(|d| d.sha256.clone())
            .collect()
    };
    let want: Vec<Option<String>> = files()[..2]
        .iter()
        .map(|(_, data)| Some(hex::encode(Sha256::digest(data))))
        .collect();
    assert_eq!(hashes(&local, source.id), want);
    let copy = only_replica(&local, source.id).backup.unwrap();
    assert_eq!(hashes(&r.env, copy), want);

    // 해시가 없는 옛 백업: 파일을 읽어 계산하고 적는다.
    let mut b = get_backup(&local, source.id);
    for disk in &mut b.status.disks {
        disk.sha256 = None;
    }
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(hashes(&local, source.id), want);

    // 적힌 해시가 있으면 원본 파일을 읽지 않는다.
    let dir = source_dir(&local, source.id);
    for (file, _) in &files()[..2] {
        std::fs::remove_file(dir.join(file)).unwrap();
    }
    let at = only_replica(&local, source.id).at;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup, replica.error),
        (ReplicaState::Ready, Some(copy), None)
    );
    assert!(replica.at > at);
}

/// ready 사본을 확인하는 동안 기록은 ready 그대로다. 원격에 닿지 못하면 Task는 실패하지만 기록은 ready로
/// 두고 이유와 시각만 적는다(다음 실행에 다시 확인한다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checking_a_ready_copy_keeps_it_ready() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let before = only_replica(&local, source.id);
    // 확인하는 동안 사본 기록이 바뀌면 남긴다.
    let mut watch = local
        .cluster
        .watch(local.cluster.list::<BackupSpec>().unwrap().0)
        .unwrap();
    drop(r.server);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("cannot reach the remote cluster"), "{error}");
    let replica = only_replica(&local, source.id);
    assert_eq!(
        (replica.state, replica.backup, replica.error.as_deref()),
        (ReplicaState::Ready, before.backup, Some(error))
    );
    assert!(replica.at > before.at);
    let mut written = 0;
    while let Ok(Ok(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), watch.next()).await
    {
        if let opl_cluster::watch::WatchEvent::Put { resource, .. } = event
            && resource.id == source.id
        {
            written += 1;
            assert_eq!(
                resource.status["replicas"][0]["state"], "ready",
                "{}",
                resource.status
            );
        }
    }
    assert!(written > 0);
}

/// 요청 검사: 백업 쓰기 권한(viewer 403), 없는 원격 404, 잘못된 풀 이름 400, 준비되지 않은 백업 409
/// `backup_not_ready`, 같은 원격으로 끝나지 않은 복제가 있으면 409 `backup_busy`(복제 중인 백업은
/// 지우지도 못한다), 옛 노드가 있으면 409 `cluster_upgrading`, 파일을 쓴 노드가 오프라인이면 409
/// `node_offline`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replicate_requests_are_checked() {
    let r = remote().await;
    let (mut local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;

    let viewer = local.user("watcher", "viewer").await;
    assert_error(
        &replicate(&viewer, "web-1").await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &admin
            .post(
                "/backups/web-1/actions/replicate",
                json!({ "remote": "nope", "pool": "dr" }),
            )
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &admin
            .post(
                "/backups/web-1/actions/replicate",
                json!({ "remote": "dr", "pool": "Bad Pool" }),
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    let mut failed = get_backup(&local, source.id);
    failed.status.phase = BackupPhase::Failed;
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &failed.status)
        .await
        .unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "backup_not_ready",
    );
    failed.status.phase = BackupPhase::Ready;
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &failed.status)
        .await
        .unwrap();

    // Task가 돌지 않으면 앞의 복제가 끝나지 않은 채 남는다.
    local.stop_runner().await;
    let (status, task) = replicate(&admin, "web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    assert_error(
        &admin.delete("/backups/web-1").await,
        StatusCode::CONFLICT,
        "backup_busy",
    );
    local.start_runner().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.online = true;
    node.status.version = Some("0.0.1~old".into());
    local.cluster.create(&node).await.unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    node.status.version = Some(opl_types::version::BUILD.into());
    node.status.online = false;
    local
        .cluster
        .update_status::<NodeSpec>(node.id, &node.status)
        .await
        .unwrap();

    // 파일을 쓴 노드가 오프라인이면 보낼 수 없다.
    let mut b = get_backup(&local, source.id);
    b.status.node = Some(node.id);
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();
    assert_error(
        &replicate(&admin, "web-1").await,
        StatusCode::CONFLICT,
        "node_offline",
    );
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
    pub async fn serve_tls(&self) -> TlsServer {
        let info = self.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
```

을 다음으로 바꾼다:

```rust
    pub async fn serve_tls(&self) -> TlsServer {
        self.serve_tls_with(|app| app).await
    }

    /// [`Env::serve_tls`]와 같되 `tweak`으로 라우터를 바꾼다(가짜 원격 응답 등).
    pub async fn serve_tls_with(
        &self,
        tweak: impl FnOnce(axum::Router) -> axum::Router,
    ) -> TlsServer {
        let info = self.cluster.list::<ClusterSpec>().unwrap().1.remove(0);
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let hits = ca_hits.clone();
        let app = opl_api::router(self.state.clone()).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
```

을 다음으로 바꾼다:

```rust
        let hits = ca_hits.clone();
        let app = tweak(opl_api::router(self.state.clone())).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/backups/{key}/actions/complete",
        "/backup-jobs",
```

을 다음으로 바꾼다:

```rust
        "/backups/{key}/actions/complete",
        "/backups/{key}/actions/replicate",
        "/backup-jobs",
```

`crates/opl-api/tests/remote_client.rs`에서

```rust
}

#[tokio::test]
async fn request_returns_any_status_with_its_body() {
```

을 다음으로 바꾼다:

```rust
}

/// 응답 헤더를 기다리는 시간은 요청마다 줄 수 있다(큰 파일을 받은 쪽이 내려 쓰느라 늦게 답한다).
#[tokio::test]
async fn requests_can_wait_longer_or_shorter_for_the_answer() {
    let server = start().await;
    let client = client(&server);
    let started = Instant::now();
    let err = client
        .request_with_timeout(
            Method::GET,
            "/hang",
            HeaderMap::new(),
            Empty::<Bytes>::new(),
            Duration::from_millis(300),
        )
        .await
        .err()
        .expect("timed out");
    assert!(
        matches!(&err, RemoteError::Unreachable(m) if m.contains("timed out")),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let response = client
        .request_with_timeout(
            Method::GET,
            "/info",
            HeaderMap::new(),
            Empty::<Bytes>::new(),
            Duration::from_secs(120),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn request_returns_any_status_with_its_body() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1506개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 백업 복제 Task와 수동 복제 — POST /backups/{key}/actions/replicate { remote, pool }(백업 쓰기, 202 Task): 옛 노드가 있으면 409 cluster_upgrading, 원격이 없으면 404, ready가 아니면 409 backup_not_ready, 지우는 중이거나 같은 원격으로 끝나지 않은 복제가 있으면 409 backup_busy, 노드는 지우기와 같은 files_node(공유가 아닌 풀은 파일을 쓴 노드). 복제 중인 백업은 지우기도 409 backup_busy(\"being replicated\"). tasks/replicate.rs의 backup-replicate Task(백업의 줄, 시간 제한 없음 — 원격 클라이언트가 조각 사이 60초): 원본 replicas의 (remote, pool) 항목을 copying으로 쓰고(revision 확인 쓰기·다시 하기), 매니페스트를 읽어 원격 POST /backups/receive(name = 원본 이름, origin = 이 클러스터 이름·CA 지문·원본 id·원본 작업 이름) → 409 replica_exists면 그 사본으로 이어 감(이미 ready면 성공), 파일마다 읽으면서 SHA-256을 계산하는 스트리밍 본문으로 PUT files/{file}(Content-Length), 응답 해시·크기가 다르면 실패 → complete → ready와 사본 id. 실패(중단 포함)는 실행기 정리가 failed와 오류로 기록. 진행률은 넣지 않음(TaskStatus에 필드 없음). schema.d.ts 다시 만듦. 시험: tests/backup_replicate.rs(두 Env, 받는 쪽 serve_tls) — 원격에 같은 파일의 ready 사본·출처·받는 쪽 id 매니페스트, 해시를 바꾸는 원격이면 실패와 failed·사본은 받는 중, 꺼진 원격 실패, 원격 viewer 토큰 403 기록, 받는 중인 사본 이어 가기와 ready 사본 다시 복제, API 검사(viewer 403·404·backup_not_ready·backup_busy·지우기 busy·cluster_upgrading), openapi 경로. common: Env::serve_tls_with"
```

---

### Task 4: 백업 작업 복제와 원격 사본 지우기

백업 작업의 `replicate`(만들기·고치기, `null`로 끔, 등록되지 않은 원격 400). 스케줄러가 작업의 ready 백업을 복제하고, 실패·빠진 사본은 실행마다 한 번 다시, ready 사본은 실행마다 원격에 있는지 확인(보존 정리가 지울 백업은 빼고), 노드당 복제 하나. 원본 지우기는 로컬 파일을 먼저 지우고 원격 사본을 지운다(실패하면 경고). 보존 정리는 틱마다.

**Files:**
- Modify: `crates/opl-api/src/backup_jobs.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/tasks/backup.rs`
- Modify: `crates/opl-api/src/tasks/replicate.rs`
- Modify: `crates/opl-api/tests/backup_jobs.rs`
- Modify: `crates/opl-api/tests/backup_replicate.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/backup_jobs.rs` 전체:

```rust
//! 백업 작업 만들기·바꾸기·지우기·지금 실행(설계 §3). 목록과 조회는 `resources`의 공통 핸들러다.
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.
//!
//! [`BackupJobScheduler`]는 리더에서 `tick`마다 작업을 이름 순으로 보며 실행을 시작하고, 대상 VM을
//! 하나씩 꺼내 백업을 요청하고(노드당 동시 백업 Task 수 제한), 실행이 끝나면 결과를 적고 보존 규칙대로
//! 지운다. 진행 상태는 작업의 `status.run`에 있으므로 리더가 바뀌어도 이어 간다. 리더가 바뀌는 순간
//! 두 스케줄러가 잠깐 겹치거나 상태 쓰기가 실패해도, 이번 실행이 이미 만든 VM의 백업은 다시 만들지 않고
//! 받아들인다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup::{Backup, BackupPhase, BackupSpec, default_backup_name};
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobError, JobRun, JobRunResult, JobSchedule, Keep,
    ReplicateTarget, VmSelector, next_run, prune,
};
use opl_types::remote::RemoteSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{
    check_backup, files_node, find_pool, now, start_backup, start_backup_delete,
    start_backup_replicate,
};
use crate::cluster::require_current;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다. 원격 복제를
/// 켠 작업은 옛 노드가 풀지 못하므로 모든 노드가 이 빌드일 때만 쓰고(409 `cluster_upgrading`), 원격은
/// 등록돼 있어야 한다(400). 원격의 풀은 보낼 때 원격이 확인한다.
fn check_spec(state: &AppState, spec: &mut BackupJobSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    spec.schedule.days.sort();
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&spec.pool)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {:?} not found", spec.pool)))?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    if let Some(target) = &spec.replicate {
        require_current(&state.cluster)?;
        if state
            .cluster
            .get_by_name::<RemoteSpec>(&target.remote)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "remote {:?} not found",
                target.remote
            )));
        }
    }
    Ok(())
}

/// 작업을 만들고 다음 실행 시각(`status.next_run`)을 적는다. 이름이 겹치면 409 `name_taken`.
#[utoipa::path(post, path = "/backup-jobs", tag = "backups",
    request_body = CreateRequest<BackupJobSpec>,
    responses((status = 201, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut req): Json<CreateRequest<BackupJobSpec>>,
) -> Result<(StatusCode, Json<BackupJob>), ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    check_name(&req.name)?;
    check_spec(&state, &mut req.spec)?;
    let mut job: BackupJob = Resource::new(req.name, req.spec);
    job.labels = req.labels;
    job.status.next_run = Some(next_run(&job.spec.schedule, now()));
    job.revision = state.cluster.create(&job).await?;
    Ok((StatusCode::CREATED, Json(job)))
}

/// 백업 작업에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupJobPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub vms: Option<VmSelector>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub pool: Option<String>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub schedule: Option<JobSchedule>,
    /// 보존 규칙 전체를 이것으로 바꾼다(적지 않은 단위는 0).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub keep: Option<Keep>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
    /// 만든 백업을 보낼 원격의 백업 풀. `null`이면 보내지 않는다.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<ReplicateTarget>, nullable, default)]
    pub replicate: Option<Option<ReplicateTarget>>,
}

/// 적은 값은 `null`이어도 `Some`이다(적지 않으면 `serde(default)`의 `None`).
fn present<'de, D, T>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// 스펙의 주어진 값을 바꾼다. 아무것도 없으면 400. 일정을 바꾸거나 작업을 다시 켜면 다음 실행 시각을
/// 지금부터 다시 계산한다(꺼 둔 사이 지난 실행은 하지 않는다). 다만 켜 둔 작업의 지금 실행 요청이
/// 기다리고 있으면(다음 실행이 지났으면) 그대로 둔다. 진행 중인 실행은 그대로 끝까지 간다.
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(patch, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = BackupJobPatch,
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<BackupJobPatch>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    if req.vms.is_none()
        && req.pool.is_none()
        && req.schedule.is_none()
        && req.keep.is_none()
        && req.enabled.is_none()
        && req.replicate.is_none()
    {
        return Err(ApiError::bad_request(
            "give vms, pool, schedule, keep, enabled or replicate",
        ));
    }
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != job.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: job.revision,
            })
            .into());
        }
        let mut spec = job.spec.clone();
        if let Some(vms) = &req.vms {
            spec.vms = vms.clone();
        }
        if let Some(pool) = &req.pool {
            spec.pool = pool.clone();
        }
        if let Some(schedule) = &req.schedule {
            spec.schedule = schedule.clone();
        }
        if let Some(keep) = req.keep {
            spec.keep = keep;
        }
        if let Some(enabled) = req.enabled {
            spec.enabled = enabled;
        }
        if let Some(replicate) = &req.replicate {
            spec.replicate = replicate.clone();
        }
        check_spec(&state, &mut spec)?;
        if spec == job.spec {
            return Ok(Json(job));
        }
        let now = now();
        let requested = job.spec.enabled && job.status.next_run.is_some_and(|t| t <= now);
        let reschedule = (spec.schedule != job.spec.schedule
            || (spec.enabled && !job.spec.enabled))
            && !requested;
        // 다음 실행을 스펙보다 먼저 쓴다. 스펙 쓰기가 실패해도 스케줄러는 새 다음 실행(앞날)만 보므로
        // 옛 일정이나 꺼 둔 사이 지난 실행을 바로 돌리지 않는다.
        let mut revision = job.revision;
        if reschedule {
            let status = BackupJobStatus {
                next_run: Some(next_run(&spec.schedule, now)),
                ..job.status.clone()
            };
            match state
                .cluster
                .update_status_checked::<BackupJobSpec>(job.id, revision, &status)
                .await
            {
                Ok(r) => revision = r,
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                    if expected.is_none() =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        match state
            .cluster
            .update_spec(job.id, Some(revision), job.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(lookup::<BackupJobSpec>(
            &state.cluster,
            &job.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업의 상태를 `change`가 돌려준 값으로 쓰고 쓴 작업을 돌려준다. 그 사이 스케줄러가 상태를 썼으면
/// 다시 읽어 바꾼다.
async fn update_status(
    state: &AppState,
    id: Uuid,
    change: impl Fn(&BackupJob) -> Result<BackupJobStatus, ApiError>,
) -> Result<BackupJob, ApiError> {
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &id.to_string())?;
        let status = change(&job)?;
        match state
            .cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await
        {
            Ok(_) => return lookup::<BackupJobSpec>(&state.cluster, &id.to_string()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업만 지운다. 작업이 만든 백업은 남는다(백업의 `job`도 그대로).
#[utoipa::path(delete, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<BackupJobSpec>(job.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 다음 실행 시각을 지금으로 당긴다. 리더의 스케줄러가 다음 차례에 실행을 시작한다. 실행이 진행
/// 중이면 409 `job_running`, 꺼 둔 작업이면 409 `job_disabled`.
#[utoipa::path(post, path = "/backup-jobs/{key}/run", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn run_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let id = lookup::<BackupJobSpec>(&state.cluster, &key)?.id;
    let job = update_status(&state, id, |job| {
        if job.status.run.is_some() {
            return Err(ApiError::conflict(
                "job_running",
                format!("backup job {:?} is already running", job.name),
            ));
        }
        if !job.spec.enabled {
            return Err(ApiError::conflict(
                "job_disabled",
                format!("backup job {:?} is disabled; enable it first", job.name),
            ));
        }
        Ok(BackupJobStatus {
            next_run: Some(now()),
            ..job.status.clone()
        })
    })
    .await?;
    Ok(Json(job))
}

#[derive(Debug, Clone)]
pub struct BackupJobSchedulerConfig {
    /// 작업을 확인하는 간격
    pub tick: Duration,
    /// 노드마다 동시에 도는 백업 Task의 최대 수. 손으로 요청한 백업과 복제 Task도 센다.
    pub max_concurrent: usize,
}

impl Default for BackupJobSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            max_concurrent: 2,
        }
    }
}

/// 백업 작업 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct BackupJobScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl BackupJobScheduler {
    pub fn spawn(state: Arc<AppState>, config: BackupJobSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: BackupJobSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config) => {}
        }
    }
}

async fn tick(state: &AppState, config: &BackupJobSchedulerConfig) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, mut jobs)) = cluster.list::<BackupJobSpec>() else {
        return;
    };
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    let now = now();
    for job in jobs {
        // 쓰기가 엇갈렸거나(revision) 저장소 오류면 다음 tick에 다시 본다.
        if let Err(e) = advance(state, config, job.clone(), now).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot advance backup job");
        }
        if let Err(e) = prune_after_run(state, job.id).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot prune backups of job");
        }
        if let Err(e) = replicate(state, config, job.id).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot replicate backups of job");
        }
    }
}

/// 작업 하나를 한 걸음 나아가게 한다: 때가 되면 실행을 시작하고, 큐에서 VM을 꺼내 백업을 요청하고,
/// 큐가 비고 이번 실행의 백업이 모두 끝났으면 실행을 끝낸다. 보존 규칙대로 지우기는 [`prune_after_run`].
async fn advance(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    mut job: BackupJob,
    now: u64,
) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    if job.status.run.is_none() {
        if !job.spec.enabled || job.status.next_run.is_none_or(|t| t > now) {
            return Ok(());
        }
        let (_, mut vms) = cluster.list::<VmSpec>()?;
        vms.sort_by(|a, b| a.name.cmp(&b.name));
        let queue: Vec<Uuid> = vms
            .iter()
            .filter(|vm| job.spec.vms.matches(vm))
            .map(|vm| vm.id)
            .collect();
        // 이름으로 골랐는데 그 VM이 없으면 오류로 남긴다.
        let missing = match &job.spec.vms {
            VmSelector::Names(names) => names
                .iter()
                .filter(|name| !vms.iter().any(|vm| vm.name == **name))
                .map(|name| JobError {
                    vm: name.clone(),
                    error: format!("no VM named {name:?}"),
                })
                .collect(),
            VmSelector::All | VmSelector::Labels(_) => Vec::new(),
        };
        // 놓친 실행이 여럿이어도 한 번만 하고 다음 실행은 지금부터 센다.
        let status = BackupJobStatus {
            next_run: Some(next_run(&job.spec.schedule, now)),
            run: Some(JobRun {
                started_at: now,
                queue,
                backups: Vec::new(),
            }),
            last_run: job.status.last_run.clone(),
            errors: missing,
        };
        job.revision = cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await?;
        job.status = status;
        tracing::info!(job = %job.name, "backup job run started");
    }

    let queue = job
        .status
        .run
        .as_ref()
        .map_or(Vec::new(), |r| r.queue.clone());
    for vm in queue {
        // 실행 도중 꺼졌으면 새 VM은 꺼내지 않는다.
        if !job.spec.enabled {
            break;
        }
        let outcome = dispatch(state, config, &job, vm).await?;
        if outcome == Dispatch::Wait {
            continue;
        }
        job = update_status(state, job.id, |job| take(job, vm, &outcome)).await?;
    }
    if !job.spec.enabled && job.status.run.as_ref().is_some_and(|r| !r.queue.is_empty()) {
        job = update_status(state, job.id, |job| {
            let mut status = job.status.clone();
            if let Some(run) = &mut status.run {
                run.queue.clear();
            }
            Ok(status)
        })
        .await?;
    }

    // Task를 먼저 읽는다: 백업 Task는 백업 상태를 쓴 뒤에 Task를 끝내므로, 이 순서면 끝난 Task의
    // 백업이 pending으로 보이지 않는다.
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if finish(&job.status, &backups, &tasks, now).is_none() {
        return Ok(());
    }
    let job = update_status(state, job.id, |job| {
        finish(&job.status, &backups, &tasks, now)
            .ok_or_else(|| ApiError::conflict("job_running", "the backup job run changed"))
    })
    .await?;
    if let Some(last) = &job.status.last_run {
        tracing::info!(job = %job.name, ok = last.ok, failed = last.failed, "backup job run finished");
    }
    Ok(())
}

/// 끝난 실행의 보존 정리: 실행이 끝나 있으면 보존 규칙이 지울 백업([`retention`])마다 지우기 Task를
/// 만든다. 만들지 못한 백업(복제·복원 중이라 `backup_busy` 등)은 tick마다 다시 본다. 실행이 끝난 뒤
/// 이미 지우기 Task를 만든 백업은 그 Task가 실패했어도 다음 실행이 끝날 때까지 다시 만들지 않는다.
async fn prune_after_run(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    let Some(job) = cluster.get::<BackupJobSpec>(id)? else {
        return Ok(());
    };
    let Some(last) = job
        .status
        .last_run
        .as_ref()
        .filter(|_| job.status.run.is_none())
    else {
        return Ok(());
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    for id in retention(job.id, &job.spec.keep, &backups) {
        let Some(backup) = backups.iter().find(|b| b.id == id) else {
            continue;
        };
        let tried = tasks.iter().any(|t| {
            t.spec.op == TaskOp::BackupDelete
                && t.spec.target == id
                && t.id.get_timestamp().map(|ts| ts.to_unix().0) >= Some(last.finished_at)
        });
        if tried {
            continue;
        }
        match start_backup_delete(state, backup).await {
            Ok(task) => {
                tracing::info!(job = %job.name, backup = %backup.name, task = %task.id, "pruning backup")
            }
            Err(e) => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot prune backup yet")
            }
        }
    }
    Ok(())
}

/// 노드의 동시 백업 수에 드는 끝나지 않은 Task: 백업과 복제
fn occupies(t: &Task) -> bool {
    matches!(
        t.spec.op,
        TaskOp::VmBackup { .. } | TaskOp::BackupReplicate { .. }
    ) && !t.status.state.is_finished()
}

/// 원격 복제를 켠 작업 `id`의 백업 가운데 보낼 것([`to_replicate`])의 복제 Task를 만든다. 노드마다
/// 복제 Task는 하나씩, 백업 Task와 합쳐 `max_concurrent`개까지다. 꺼 둔 작업은 보내지 않는다. 만들지
/// 못한 백업(노드가 오프라인, 원격이 없어짐 등)은 다음 tick에 다시 본다. 옛 노드가 있으면 만들지 않는다.
async fn replicate(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    id: Uuid,
) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    let Some(job) = cluster.get::<BackupJobSpec>(id)? else {
        return Ok(());
    };
    let Some(target) = job.spec.replicate.clone().filter(|_| job.spec.enabled) else {
        return Ok(());
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    let candidates = to_replicate(&job, &target, &backups, &tasks);
    if candidates.is_empty() {
        return Ok(());
    }
    // 옛 노드는 복제 Task와 사본 기록을 모른다. 모든 노드가 올라오면 보낸다.
    if require_current(cluster).is_err() {
        return Ok(());
    }
    let mut running: BTreeMap<Uuid, usize> = BTreeMap::new();
    let mut replicating: Vec<Uuid> = Vec::new();
    for t in tasks.iter().filter(|t| occupies(t)) {
        *running.entry(t.spec.node).or_default() += 1;
        if matches!(t.spec.op, TaskOp::BackupReplicate { .. }) {
            replicating.push(t.spec.node);
        }
    }
    for backup in candidates {
        let node = match find_pool(state, &backup.spec.pool)
            .and_then(|pool| files_node(state, backup, &pool))
        {
            Ok(node) => node,
            Err(e) => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot replicate backup yet");
                continue;
            }
        };
        if replicating.contains(&node)
            || running.get(&node).copied().unwrap_or(0) >= config.max_concurrent
        {
            continue;
        }
        match start_backup_replicate(state, backup, &target.remote, target.pool.clone()).await {
            Ok(task) => {
                tracing::info!(job = %job.name, backup = %backup.name, remote = %target.remote, task = %task.id, "replicating backup");
                replicating.push(node);
                *running.entry(node).or_default() += 1;
            }
            Err(e) if e.status.is_client_error() => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot replicate backup");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// 작업 `job`의 백업 가운데 원격 `target`으로 보낼 것(만든 순서): `ready`이고, 보존 규칙이 지울 것이
/// 아니고([`retention`] — 지우기와 겹치지 않게), 그 사본 기록이 없거나 기록이 마지막 실행을 시작하기 전의
/// 것이다. 그래서 실패한(또는 복제 Task 없이 copying으로 남은) 사본은 실행마다 한 번 더 보내고, ready
/// 사본도 실행마다 한 번 원격에 있는지 확인한다(복제 Task가 원격에 물어 같은 파일이 있으면 기록 시각만
/// 고치고, 원격에서 지웠으면 다시 보낸다). 같은 원격으로 복제하는 Task가 끝나지 않은 백업은 뺀다.
fn to_replicate<'a>(
    job: &BackupJob,
    target: &ReplicateTarget,
    backups: &'a [Backup],
    tasks: &[Task],
) -> Vec<&'a Backup> {
    let started = job
        .status
        .run
        .as_ref()
        .map(|r| r.started_at)
        .or(job.status.last_run.as_ref().map(|l| l.started_at))
        .unwrap_or(0);
    let replicating = |b: &Backup| {
        tasks.iter().any(|t| {
            !t.status.state.is_finished()
                && t.spec.target == b.id
                && matches!(&t.spec.op, TaskOp::BackupReplicate { remote, .. } if *remote == target.remote)
        })
    };
    let pruned = retention(job.id, &job.spec.keep, backups);
    let mut out: Vec<&Backup> = backups
        .iter()
        .filter(|b| b.spec.job == Some(job.id) && b.status.phase == BackupPhase::Ready)
        .filter(|b| !pruned.contains(&b.id))
        .filter(|b| !replicating(b))
        .filter(|b| {
            match b
                .status
                .replicas
                .iter()
                .find(|r| r.remote == target.remote && r.pool == target.pool)
            {
                None => true,
                Some(r) => r.at < started,
            }
        })
        .collect();
    out.sort_by_key(|b| b.id);
    out
}

/// 큐에서 꺼낸 VM 하나에 한 일
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dispatch {
    /// 노드가 이미 백업 Task를 다 돌리고 있다. 큐에 남긴다.
    Wait,
    /// VM이 없어졌다.
    Gone,
    /// 이 백업을 만들었다(또는 이번 실행이 이미 만들었다).
    Started(Uuid),
    /// 백업하지 못했다.
    Refused(JobError),
}

/// 큐의 VM `vm`의 백업을 요청한다. 요청 검사는 백업 API와 같다(거절은 [`Dispatch::Refused`]).
/// 이번 실행이 이 VM의 백업을 이미 만들었으면(상태에 적지 못한 채 리더가 바뀌었거나 쓰기가 실패했다)
/// 그 백업을 받아들인다.
async fn dispatch(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    job: &BackupJob,
    vm: Uuid,
) -> Result<Dispatch, ApiError> {
    let cluster = &state.cluster;
    let Some(vm) = cluster.get::<VmSpec>(vm)? else {
        return Ok(Dispatch::Gone);
    };
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if let Some(made) = backups
        .iter()
        .find(|b| b.spec.vm == vm.id && of_this_run(job, b))
    {
        return Ok(Dispatch::Started(made.id));
    }
    let refused = |error: String| {
        Ok(Dispatch::Refused(JobError {
            vm: vm.name.clone(),
            error,
        }))
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let backing_up =
        |t: &&Task| matches!(t.spec.op, TaskOp::VmBackup { .. }) && !t.status.state.is_finished();
    if tasks
        .iter()
        .filter(backing_up)
        .any(|t| t.spec.target == vm.id)
    {
        return refused("a backup of this VM is already running".into());
    }
    let Some(pool) = cluster.get_by_name::<StoragePoolSpec>(&job.spec.pool)? else {
        return refused(format!("storage pool {:?} not found", job.spec.pool));
    };
    let node = match check_backup(state, &vm, &pool) {
        Ok(node) => node,
        Err(e) if e.status.is_client_error() => return refused(e.message),
        Err(e) => return Err(e),
    };
    let running = tasks
        .iter()
        .filter(|t| occupies(t))
        .filter(|t| t.spec.node == node)
        .count();
    if running >= config.max_concurrent {
        return Ok(Dispatch::Wait);
    }
    let name = backup_name(&vm.name, now(), &backups);
    match start_backup(state, &vm, &pool, node, name, Some(job.id)).await {
        Ok(task) => match task.spec.op {
            TaskOp::VmBackup { backup } => Ok(Dispatch::Started(backup)),
            _ => unreachable!("start_backup makes a backup task"),
        },
        Err(e) if e.status.is_client_error() => refused(e.message),
        Err(e) => Err(e),
    }
}

/// 백업 `backup`이 작업 `job`의 진행 중인 실행이 만든 것인지. 백업 id(v7)의 시각(초)이 실행을 시작한
/// 시각 이후이고 마지막으로 끝난 실행의 끝난 시각보다 뒤여야 한다(앞 실행의 백업은 그 실행이 끝나기 전에
/// 만들었다).
fn of_this_run(job: &BackupJob, backup: &Backup) -> bool {
    let Some(run) = &job.status.run else {
        return false;
    };
    let Some(made) = backup.id.get_timestamp().map(|t| t.to_unix().0) else {
        return false;
    };
    backup.spec.job == Some(job.id)
        && made >= run.started_at
        && job
            .status
            .last_run
            .as_ref()
            .is_none_or(|l| made > l.finished_at)
}

/// 실행의 큐에서 `vm`을 빼고 한 일을 적은 상태
fn take(job: &BackupJob, vm: Uuid, outcome: &Dispatch) -> Result<BackupJobStatus, ApiError> {
    let mut status = job.status.clone();
    let Some(run) = &mut status.run else {
        return Err(ApiError::conflict(
            "job_not_running",
            "the backup job run ended",
        ));
    };
    run.queue.retain(|id| *id != vm);
    match outcome {
        Dispatch::Started(backup) if !run.backups.contains(backup) => run.backups.push(*backup),
        Dispatch::Refused(error) => status.errors.push(error.clone()),
        Dispatch::Started(_) | Dispatch::Wait | Dispatch::Gone => {}
    }
    Ok(status)
}

/// 기본 백업 이름(`{VM 이름}-{yyyymmdd-HHMMSS}`). 같은 이름의 백업이 있으면 `-2`, `-3`…을 붙인다.
/// 번호를 붙이면 63자를 넘지 않게 VM 이름 부분을 번호 길이만큼 줄인다.
fn backup_name(vm: &str, now: u64, backups: &[Backup]) -> String {
    let base = default_backup_name(vm, now);
    let taken = |name: &str| backups.iter().any(|b| b.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            // 기본 이름은 VM 이름의 앞 47자를 쓴다.
            let short = &vm[..vm.len().min(47 - suffix.len())];
            format!("{}{suffix}", default_backup_name(short, now))
        })
        .find(|name| !taken(name))
        .unwrap()
}

/// 끝난 실행을 적은 상태. 실행이 없거나, 큐가 남았거나, 이번 실행의 백업이 아직 만드는 중이면 없다.
/// 성공은 `ready` 백업, 실패는 `failed` 백업과 백업하지 못한 VM이다(없어진 백업은 세지 않는다).
/// 채우는 끝나지 않은 Task가 없는 `pending` 백업(죽은 노드의 Task를 실패 처리했다)도 실패다.
/// 실패한 백업의 오류는 `errors`에 더한다.
fn finish(
    status: &BackupJobStatus,
    backups: &[Backup],
    tasks: &[Task],
    now: u64,
) -> Option<BackupJobStatus> {
    let run = status.run.as_ref()?;
    if !run.queue.is_empty() {
        return None;
    }
    let mine: Vec<&Backup> = run
        .backups
        .iter()
        .filter_map(|id| backups.iter().find(|b| b.id == *id))
        .collect();
    let filling = |b: &Backup| {
        tasks.iter().any(|t| {
            matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == b.id)
                && !t.status.state.is_finished()
        })
    };
    let (pending, mine): (Vec<&Backup>, Vec<&Backup>) = mine
        .into_iter()
        .partition(|b| b.status.phase == BackupPhase::Pending);
    if pending.iter().any(|b| filling(b)) {
        return None;
    }
    let ok = mine
        .iter()
        .filter(|b| b.status.phase == BackupPhase::Ready)
        .count() as u32;
    let mut errors = status.errors.clone();
    errors.extend(
        mine.iter()
            .filter(|b| b.status.phase == BackupPhase::Failed)
            .map(|b| JobError {
                vm: b.spec.vm_name.clone(),
                error: b
                    .status
                    .error
                    .clone()
                    .unwrap_or_else(|| "backup failed".into()),
            }),
    );
    errors.extend(pending.iter().map(|b| JobError {
        vm: b.spec.vm_name.clone(),
        error: "backup task ended without finishing the backup".into(),
    }));
    Some(BackupJobStatus {
        next_run: status.next_run,
        run: None,
        last_run: Some(JobRunResult {
            started_at: run.started_at,
            finished_at: now,
            ok,
            failed: errors.len() as u32,
        }),
        errors,
    })
}

/// 작업 `job`의 백업 가운데 지울 것: VM마다 `ready` 백업은 보존 규칙(`prune`)에 맞지 않는 것,
/// `failed` 백업은 같은 VM의 더 새(id가 큰) `ready` 백업이 있는 것. 다른 작업과 손으로 만든 백업은
/// 건드리지 않는다.
fn retention(job: Uuid, keep: &Keep, backups: &[Backup]) -> Vec<Uuid> {
    let mut by_vm: BTreeMap<Uuid, Vec<&Backup>> = BTreeMap::new();
    for b in backups.iter().filter(|b| b.spec.job == Some(job)) {
        by_vm.entry(b.spec.vm).or_default().push(b);
    }
    let mut out = Vec::new();
    for mine in by_vm.values() {
        let ready: Vec<(Uuid, u64)> = mine
            .iter()
            .filter(|b| b.status.phase == BackupPhase::Ready)
            .filter_map(|b| Some((b.id, b.status.created_at?)))
            .collect();
        out.extend(prune(&ready, keep));
        if let Some(newest) = ready.iter().map(|b| b.0).max() {
            out.extend(
                mine.iter()
                    .filter(|b| b.status.phase == BackupPhase::Failed && b.id < newest)
                    .map(|b| b.id),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use opl_types::backup::{BackupReplica, BackupStatus, ReplicaState};
    use opl_types::backup_job::JobSchedule;
    use opl_types::resource::validate_name;
    use opl_types::task::TaskState;
    use opl_types::vm::{CpuSpec, MachineType, PowerState};

    use super::*;

    /// 2026-10-27 05:07:00 UTC
    const NOW: u64 = 1_793_077_620;
    const DAY: u64 = 86400;

    fn vm_spec() -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
            },
            memory_mib: 256,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power: PowerState::Stopped,
            restart: Default::default(),
            node: None,
            snapshot_schedule: None,
            ha: false,
        }
    }

    /// VM `vm`의 백업. `created_at`이 있으면 `phase`로 끝났다.
    fn backup(
        name: &str,
        vm: Uuid,
        job: Option<Uuid>,
        phase: BackupPhase,
        created_at: Option<u64>,
    ) -> Backup {
        let mut b: Backup = Resource::new(
            name,
            BackupSpec {
                vm,
                vm_name: format!("vm-{}", &vm.simple().to_string()[..4]),
                pool: "bk".into(),
                vm_spec: vm_spec(),
                job,
                origin: None,
            },
        );
        b.status = BackupStatus {
            phase,
            created_at,
            error: (phase == BackupPhase::Failed).then(|| format!("{name} broke")),
            ..Default::default()
        };
        b
    }

    fn running(queue: Vec<Uuid>, backups: Vec<Uuid>, errors: Vec<JobError>) -> BackupJobStatus {
        BackupJobStatus {
            next_run: Some(NOW + DAY),
            run: Some(JobRun {
                started_at: NOW - 60,
                queue,
                backups,
            }),
            last_run: None,
            errors,
        }
    }

    /// 백업 `backup`을 채우는 Task
    fn task(backup: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "t",
            TaskSpec {
                op: TaskOp::VmBackup { backup },
                target: Uuid::now_v7(),
                node: Uuid::now_v7(),
            },
        );
        t.status.state = state;
        t
    }

    fn refused(vm: &str) -> JobError {
        JobError {
            vm: vm.into(),
            error: "no".into(),
        }
    }

    #[test]
    fn names_follow_the_default_with_a_number_when_taken() {
        let vm = Uuid::now_v7();
        assert_eq!(backup_name("web", NOW, &[]), "web-20261027-050700");
        let taken = [
            backup("web-20261027-050700", vm, None, BackupPhase::Ready, Some(1)),
            backup(
                "web-20261027-050700-2",
                vm,
                None,
                BackupPhase::Ready,
                Some(2),
            ),
        ];
        assert_eq!(backup_name("web", NOW, &taken), "web-20261027-050700-3");
        assert_eq!(backup_name("db", NOW, &taken), "db-20261027-050700");

        // 63자 이름 둘의 앞부분이 같으면 번호를 붙이되 63자를 넘지 않게 앞부분을 줄인다.
        let a = format!("{}a", "x".repeat(62));
        let b = format!("{}b", "x".repeat(62));
        let first = backup_name(&a, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&b, NOW, &taken);
        assert_ne!(first, second);
        for name in [&first, &second] {
            assert!(name.len() <= 63, "{name}");
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
        assert!(second.ends_with("-2"), "{second}");
        // 줄인 끝이 `-`이면 그것도 뗀다.
        let dashed = format!("{}-{}", "y".repeat(44), "z".repeat(18));
        let first = backup_name(&dashed, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&dashed, NOW, &taken);
        assert_eq!(second, format!("{}-20261027-050700-2", "y".repeat(44)));
    }

    #[test]
    fn a_run_finishes_when_its_queue_is_empty_and_its_backups_are_done() {
        let vm = Uuid::now_v7();
        let ready = backup("a", vm, None, BackupPhase::Ready, Some(NOW - 10));
        let failed = backup("b", vm, None, BackupPhase::Failed, None);
        let pending = backup("c", vm, None, BackupPhase::Pending, None);
        let all = [ready.clone(), failed.clone(), pending.clone()];
        let filling = [task(pending.id, TaskState::Running)];

        // 실행이 없거나, 큐가 남았거나, 만드는 중인 백업이 있으면 끝나지 않았다.
        assert_eq!(finish(&BackupJobStatus::default(), &all, &[], NOW), None);
        let queued = running(vec![vm], vec![ready.id], vec![]);
        assert_eq!(finish(&queued, &all, &[], NOW), None);
        let waiting = running(vec![], vec![ready.id, pending.id], vec![]);
        assert_eq!(finish(&waiting, &all, &filling, NOW), None);

        // 채우던 Task가 끝났는데(실패 처리 등) pending으로 남은 백업은 실패다.
        for tasks in [vec![], vec![task(pending.id, TaskState::Failed)]] {
            let done = finish(&waiting, &all, &tasks, NOW).unwrap();
            let last = done.last_run.unwrap();
            assert_eq!((last.ok, last.failed), (1, 1));
            assert_eq!(
                done.errors,
                [JobError {
                    vm: pending.spec.vm_name.clone(),
                    error: "backup task ended without finishing the backup".into(),
                }]
            );
        }

        // ready는 성공, failed와 백업하지 못한 VM은 실패다. 없어진 백업은 세지 않는다.
        let gone = Uuid::now_v7();
        let status = running(vec![], vec![ready.id, failed.id, gone], vec![refused("db")]);
        let done = finish(&status, &all, &filling, NOW).unwrap();
        assert_eq!(done.run, None);
        assert_eq!(done.next_run, Some(NOW + DAY));
        assert_eq!(
            done.last_run,
            Some(JobRunResult {
                started_at: NOW - 60,
                finished_at: NOW,
                ok: 1,
                failed: 2,
            })
        );
        assert_eq!(
            done.errors,
            [
                refused("db"),
                JobError {
                    vm: failed.spec.vm_name.clone(),
                    error: "b broke".into(),
                }
            ]
        );
        // 백업 없이 끝난 실행
        let empty = finish(&running(vec![], vec![], vec![]), &[], &[], NOW).unwrap();
        assert_eq!(empty.last_run.unwrap().ok, 0);
    }

    #[test]
    fn taking_a_vm_records_what_happened() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        job.status = running(vec![a, b], vec![], vec![]);
        let backup = Uuid::now_v7();
        let status = take(&job, a, &Dispatch::Started(backup)).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [b]);
        assert_eq!(status.run.as_ref().unwrap().backups, [backup]);
        let status = take(&job, b, &Dispatch::Refused(refused("db"))).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, [refused("db")]);
        let status = take(&job, b, &Dispatch::Gone).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, []);
        job.status.run = None;
        assert!(take(&job, a, &Dispatch::Gone).is_err());
    }

    #[test]
    fn a_backup_belongs_to_the_run_made_after_it_started() {
        let vm = Uuid::now_v7();
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        // 실행은 NOW - 60에 시작했다.
        job.status = running(vec![vm], vec![], vec![]);
        let made_at = |at: u64, job: Option<Uuid>| {
            let mut b = backup("b", vm, job, BackupPhase::Pending, None);
            b.id = Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, at, 0));
            b
        };
        assert!(of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW, Some(job.id))));
        assert!(!of_this_run(&job, &made_at(NOW - 61, Some(job.id))));
        // 다른 작업과 손으로 만든 백업
        assert!(!of_this_run(&job, &made_at(NOW, Some(Uuid::now_v7()))));
        assert!(!of_this_run(&job, &made_at(NOW, None)));
        // 앞 실행이 실행을 시작한 그 초에 끝났으면 그 초의 백업은 앞 실행의 것이다.
        job.status.last_run = Some(JobRunResult {
            started_at: NOW - 120,
            finished_at: NOW - 60,
            ok: 1,
            failed: 0,
        });
        assert!(!of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW - 59, Some(job.id))));
        // 실행이 없으면 아무것도 아니다.
        job.status.run = None;
        assert!(!of_this_run(&job, &made_at(NOW, Some(job.id))));
    }

    /// 사본 기록
    fn replica(remote: &str, pool: &str, state: ReplicaState, at: u64) -> BackupReplica {
        BackupReplica {
            remote: remote.into(),
            pool: pool.into(),
            backup: None,
            state,
            error: None,
            at,
        }
    }

    #[test]
    fn replication_picks_ready_backups_without_a_copy_and_retries_failures_once_a_run() {
        let vm = Uuid::now_v7();
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
                replicate: None,
            },
        );
        let target = ReplicateTarget {
            remote: "dr".into(),
            pool: "far".into(),
        };
        // 마지막 실행은 NOW - 60에 시작했다.
        job.status = running(vec![], vec![], vec![]);
        let with_id = |mut b: Backup, n: u128, replicas: Vec<BackupReplica>| {
            b.id = Uuid::from_u128(n);
            b.status.replicas = replicas;
            b
        };
        let ready = |n, replicas| {
            with_id(
                backup("r", vm, Some(job.id), BackupPhase::Ready, Some(1)),
                n,
                replicas,
            )
        };
        let fresh = ready(9, vec![]);
        // 다른 원격·풀의 사본만 있다.
        let elsewhere = ready(
            2,
            vec![
                replica("other", "far", ReplicaState::Ready, NOW),
                replica("dr", "near", ReplicaState::Ready, NOW),
            ],
        );
        let failed_before = ready(
            3,
            vec![replica("dr", "far", ReplicaState::Failed, NOW - 61)],
        );
        let failed_since = ready(
            4,
            vec![replica("dr", "far", ReplicaState::Failed, NOW - 60)],
        );
        // 실행 전에 ready였던 사본도 실행마다 한 번 원격에 있는지 확인한다.
        let copied = ready(5, vec![replica("dr", "far", ReplicaState::Ready, 1)]);
        let copied_since = ready(
            10,
            vec![replica("dr", "far", ReplicaState::Ready, NOW - 60)],
        );
        // 복제 Task 없이 copying으로 남은 기록은 실패와 같다.
        let stuck = ready(
            6,
            vec![replica("dr", "far", ReplicaState::Copying, NOW - 100)],
        );
        let copying = ready(7, vec![]);
        let pending = with_id(
            backup("p", vm, Some(job.id), BackupPhase::Pending, None),
            8,
            vec![],
        );
        let manual = with_id(
            backup("m", vm, None, BackupPhase::Ready, Some(1)),
            1,
            vec![],
        );
        let mut sending = task(copying.id, TaskState::Running);
        sending.spec.op = TaskOp::BackupReplicate {
            remote: "dr".into(),
            pool: "far".into(),
        };
        sending.spec.target = copying.id;
        let mut done = sending.clone();
        done.spec.target = fresh.id;
        done.status.state = TaskState::Failed;
        let all = [
            fresh.clone(),
            elsewhere.clone(),
            failed_before.clone(),
            failed_since,
            copied.clone(),
            copied_since,
            stuck.clone(),
            copying,
            pending,
            manual,
        ];
        let ids = |job: &BackupJob| -> Vec<Uuid> {
            to_replicate(job, &target, &all, &[sending.clone(), done.clone()])
                .iter()
                .map(|b| b.id)
                .collect()
        };
        assert_eq!(
            ids(&job),
            [
                elsewhere.id,
                failed_before.id,
                copied.id,
                stuck.id,
                fresh.id
            ]
        );
        // 실행이 끝났으면 그 실행의 시작이 기준이다.
        job.status.last_run = Some(JobRunResult {
            started_at: NOW - 60,
            finished_at: NOW,
            ok: 1,
            failed: 0,
        });
        job.status.run = None;
        assert_eq!(
            ids(&job),
            [
                elsewhere.id,
                failed_before.id,
                copied.id,
                stuck.id,
                fresh.id
            ]
        );
        // 실행한 적이 없으면 실패를 다시 보내지 않는다.
        job.status.last_run = None;
        assert_eq!(ids(&job), [elsewhere.id, fresh.id]);
        // 보존 규칙이 지울 백업(ready 여덟 가운데 id가 작은 셋)은 보내지 않는다.
        job.spec.keep = Keep {
            last: 5,
            ..Keep::default()
        };
        assert_eq!(ids(&job), [fresh.id]);
    }

    #[test]
    fn retention_prunes_ready_backups_per_vm_and_failed_ones_before_a_success() {
        let job = Uuid::now_v7();
        let (web, db) = (Uuid::now_v7(), Uuid::now_v7());
        let keep = Keep {
            last: 1,
            ..Keep::default()
        };
        // id가 만든 순서다(v7).
        let with_id = |mut b: Backup, n: u128| {
            b.id = Uuid::from_u128(n);
            b
        };
        let r = |vm, at, n| with_id(backup("r", vm, Some(job), BackupPhase::Ready, Some(at)), n);
        let f = |vm, n| with_id(backup("f", vm, Some(job), BackupPhase::Failed, None), n);
        // 만든 순서: web 실패 → web 성공 둘 → web 실패, db 성공 둘
        let web_failed_old = f(web, 1);
        let web_old = r(web, NOW - 2 * DAY, 2);
        let web_new = r(web, NOW - DAY, 3);
        let web_failed_new = f(web, 4);
        let db_old = r(db, NOW - 2 * DAY, 5);
        let db_new = r(db, NOW - DAY, 6);
        // 손으로 만든 백업, 다른 작업의 백업, 만드는 중인 백업은 건드리지 않는다.
        let manual = backup("m", web, None, BackupPhase::Ready, Some(1));
        let other = backup("o", web, Some(Uuid::now_v7()), BackupPhase::Ready, Some(1));
        let pending = backup("p", web, Some(job), BackupPhase::Pending, None);
        let all = [
            web_failed_old.clone(),
            web_old.clone(),
            web_new.clone(),
            web_failed_new.clone(),
            db_old.clone(),
            db_new.clone(),
            manual,
            other,
            pending,
        ];
        let mut got = retention(job, &keep, &all);
        got.sort();
        let mut want = vec![web_old.id, web_failed_old.id, db_old.id];
        want.sort();
        assert_eq!(got, want);

        // 규칙이 모두 0이면 성공한 백업은 지우지 않지만, 더 새 성공 앞의 실패는 지운다.
        assert_eq!(retention(job, &Keep::default(), &all), [web_failed_old.id]);
        // 성공이 없으면 실패는 남는다.
        assert!(retention(job, &keep, &[web_failed_old, web_failed_new]).is_empty());
    }
}
```

`crates/opl-api/src/backups.rs`에서

```rust
/// 그 노드가 오프라인이면 409 `node_offline`, 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn files_node(state: &AppState, backup: &Backup, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let Some(node) = backup.status.node else {
```

을 다음으로 바꾼다:

```rust
/// 그 노드가 오프라인이면 409 `node_offline`, 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
pub(crate) fn files_node(
    state: &AppState,
    backup: &Backup,
    pool: &StoragePool,
) -> Result<Uuid, ApiError> {
    let Some(node) = backup.status.node else {
```

`crates/opl-api/src/backups.rs`에서

```rust
    let remote = lookup::<RemoteSpec>(&state.cluster, &req.remote)?;
    if backup.status.phase != BackupPhase::Ready {
```

을 다음으로 바꾼다:

```rust
    let remote = lookup::<RemoteSpec>(&state.cluster, &req.remote)?;
    let task = start_backup_replicate(&state, &backup, &remote.name, req.pool).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 백업 `backup`을 원격 `remote`의 백업 풀 `pool`로 보낼 Task를 만든다. 오류는 [`replicate_backup`]과
/// 같다. 복제 API와 백업 작업 스케줄러가 쓴다(권한 검사는 부르는 쪽).
pub(crate) async fn start_backup_replicate(
    state: &AppState,
    backup: &Backup,
    remote: &str,
    pool: String,
) -> Result<Task, ApiError> {
    require_current(&state.cluster)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, remote)?;
    if backup.status.phase != BackupPhase::Ready {
```

`crates/opl-api/src/backups.rs`에서

```rust
        ));
    }
    if backup_tasks(&state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(&backup, "being deleted"));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
```

을 다음으로 바꾼다:

```rust
        ));
    }
    if backup_tasks(state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(backup, "being deleted"));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
```

`crates/opl-api/src/backups.rs`에서

```rust
        return Err(backup_busy(
            &backup,
            &format!("being replicated to remote {:?}", remote.name),
        ));
    }
    let pool = find_pool(&state, &backup.spec.pool)?;
    let node = files_node(&state, &backup, &pool)?;
    let op = TaskOp::BackupReplicate {
        remote: remote.name,
        pool: req.pool,
    };
    let task = create_task(&state.cluster, op, backup.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

을 다음으로 바꾼다:

```rust
        return Err(backup_busy(
            backup,
            &format!("being replicated to remote {:?}", remote.name),
        ));
    }
    let backup_pool = find_pool(state, &backup.spec.pool)?;
    let node = files_node(state, backup, &backup_pool)?;
    let op = TaskOp::BackupReplicate {
        remote: remote.name,
        pool,
    };
    create_task(&state.cluster, op, backup.id, node).await
}
```

`crates/opl-api/src/tasks.rs`에서

```rust
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
```

을 다음으로 바꾼다:

```rust
    match result {
        Ok(warning) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
```

`crates/opl-api/src/tasks.rs`에서

```rust
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
```

을 다음으로 바꾼다:

```rust
            }
            set_status(ctx, task.id, TaskState::Succeeded, warning).await
        }
```

`crates/opl-api/src/tasks.rs`에서

```rust

async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
```

을 다음으로 바꾼다:

```rust

/// 성공한 Task가 남길 경고(`error`)가 있으면 `Ok(Some)`이다(지우지 못한 원격 사본).
async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let done = match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VmBackup { backup } => backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        TaskOp::BackupRestore { backup, name, pool } => {
```

을 다음으로 바꾼다:

```rust
        TaskOp::VmBackup { backup } => backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => return backup::delete(ctx, id).await,
        TaskOp::BackupRestore { backup, name, pool } => {
```

`crates/opl-api/src/tasks.rs`에서

```rust
        },
    }
}
```

을 다음으로 바꾼다:

```rust
        },
    };
    done.map(|()| None)
}
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
/// 풀이 없어졌으면 파일도 함께 없어진 것으로 보고 리소스만 지운다. 이 백업을 되살리는 Task가 끝나지
/// 않았으면(다른 노드에서 돌 수 있다) 실패다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(());
    };
```

을 다음으로 바꾼다:

```rust
/// 풀이 없어졌으면 파일도 함께 없어진 것으로 보고 리소스만 지운다. 이 백업을 되살리는 Task가 끝나지
/// 않았으면(다른 노드에서 돌 수 있다) 실패다. 파일을 지운 뒤 리소스보다 먼저 원격 사본을 지우고(엔터프라이즈
/// 설계 §6.2), 지우지 못한 사본이 있어도 원본은 지우고 그 경고를 돌려준다.
pub(super) async fn delete(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    use opl_types::task::{TaskOp, TaskSpec};
    let Some(backup) = get_backup(ctx, id)? else {
        return Ok(None);
    };
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
        .map_err(|e| e.to_string())?;
    if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
    } else {
```

을 다음으로 바꾼다:

```rust
        .map_err(|e| e.to_string())?;
    let dir = if pool.is_none() {
        tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, "storage pool of backup is gone; deleting only the backup");
        None
    } else {
```

`crates/opl-api/src/tasks/backup.rs`에서

```rust
        }
        remove_dir(&dir).await?;
    }
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
```

을 다음으로 바꾼다:

```rust
        }
        Some(dir)
    };
    // 원본 파일을 지우지 못하면 원본이 남으므로 원격 사본도 건드리지 않는다.
    if let Some(dir) = dir {
        remove_dir(&dir).await?;
    }
    let left = super::replicate::delete_copies(ctx, &backup).await;
    match ctx.cluster.delete::<BackupSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
        Err(e) => return Err(e.to_string()),
    }
    Ok((!left.is_empty()).then(|| {
        format!(
            "backup deleted, but remote copies may be left: {}",
            left.join("; ")
        )
    }))
}
```

`crates/opl-api/src/tasks/replicate.rs`에서

```rust

/// 원격의 오류(사람이 읽을 말)
```

을 다음으로 바꾼다:

```rust

/// 원격 `remote`(등록한 이름)의 클라이언트
fn client(ctx: &Ctx, remote: &str) -> Result<RemoteClient, String> {
    let spec = ctx
        .cluster
        .get_by_name::<RemoteSpec>(remote)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote {remote:?} not found"))?
        .spec;
    RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token)
        .map_err(|e| remote_error(remote, "connect", e))
}

/// 원본 백업 `backup`의 원격 사본(id가 있는 것 — 실패로 끝났어도 원격에 받다 만 사본이 있다)을 원격에서
/// 지운다(원격의 지우기 Task를 요청한다). 404는 이미 지운 것이다. 지우지 못한 사본마다 이유 한 줄을
/// 돌려준다.
pub(super) async fn delete_copies(ctx: &Ctx, backup: &Backup) -> Vec<String> {
    let mut left = Vec::new();
    for replica in &backup.status.replicas {
        let Some(copy) = replica.backup else {
            continue;
        };
        if let Err(e) = delete_copy(ctx, &replica.remote, copy).await {
            tracing::warn!(backup = %backup.name, remote = %replica.remote, copy = %copy, error = %e, "cannot delete remote copy of backup");
            left.push(e);
        }
    }
    left
}

async fn delete_copy(ctx: &Ctx, remote: &str, copy: Uuid) -> Result<(), String> {
    let client = client(ctx, remote)?;
    let deleting = |e| remote_error(remote, &format!("delete copy {copy}"), e);
    let (status, body) = call(&client, Method::DELETE, &format!("/backups/{copy}"), None)
        .await
        .map_err(deleting)?;
    match status {
        200..=299 | 404 => Ok(()),
        _ => Err(deleting(status_error(status, &body))),
    }
}

/// 원격의 오류(사람이 읽을 말)
```

`crates/opl-api/src/tasks/replicate.rs`에서

```rust
    let id = backup.id;
    let spec = ctx
        .cluster
        .get_by_name::<RemoteSpec>(remote)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote {remote:?} not found"))?
        .spec;
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
```

을 다음으로 바꾼다:

```rust
    let id = backup.id;
    let client = client(ctx, remote)?;
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
```

`crates/opl-api/src/tasks/replicate.rs`에서

```rust
    let origin = origin(ctx, backup)?;
    let client = RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token)
        .map_err(|e| remote_error(remote, "connect", e))?;
```

을 다음으로 바꾼다:

```rust
    let origin = origin(ctx, backup)?;
```

`crates/opl-api/tests/backup_jobs.rs`에서

```rust
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::remote::{Remote, RemoteSpec};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/backup_jobs.rs`에서

```rust
        .unwrap();
    let body = create(&admin, "nightly", with(target.clone())["spec"].clone()).await;
    assert_eq!(body["spec"]["replicate"], target);
}
```

을 다음으로 바꾼다:

```rust
        .unwrap();
    // 등록하지 않은 원격은 400이다.
    let got = admin.post("/backup-jobs", with(target.clone())).await;
    assert_eq!(got.0, StatusCode::BAD_REQUEST, "{}", got.1);
    assert!(
        got.1["message"].as_str().unwrap().contains("remote"),
        "{}",
        got.1
    );
    remote(&env, "dr").await;
    let body = create(&admin, "nightly", with(target.clone())["spec"].clone()).await;
    assert_eq!(body["spec"]["replicate"], target);
}

/// 원격 `name`을 저장소에 직접 등록한다(연결하지 않는다).
async fn remote(env: &Env, name: &str) {
    let remote: Remote = Resource::new(
        name,
        RemoteSpec {
            url: "https://10.0.0.9:8006".into(),
            ca_fingerprint: format!("sha256:{}", "0".repeat(64)),
            token: "opl_secret".into(),
            ca_pem: String::new(),
        },
    );
    env.cluster.create(&remote).await.unwrap();
}

/// PATCH의 `replicate`: 주면 바꾸고(등록한 원격만), `null`이면 끄고, 없으면 그대로 둔다. 옛 노드가
/// 남아 있으면 켜지 못한다(409).
#[tokio::test]
async fn patch_turns_replication_on_and_off() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    remote(&env, "dr").await;
    create(&admin, "nightly", nightly()).await;
    let target = json!({ "remote": "dr", "pool": "backups" });

    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "replicate": target }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["replicate"], target);
    // 다른 값을 바꾸면 복제는 그대로다.
    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "enabled": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["replicate"], target);

    for bad in [
        json!({ "remote": "nope", "pool": "backups" }),
        json!({ "remote": "dr", "pool": "Bad" }),
        json!({ "remote": "dr" }),
    ] {
        let got = admin
            .patch("/backup-jobs/nightly", json!({ "replicate": bad }), None)
            .await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{bad} -> {}", got.1);
    }

    let (status, body) = admin
        .patch("/backup-jobs/nightly", json!({ "replicate": null }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["spec"].get("replicate").is_none(), "{body}");
    let job = env
        .cluster
        .get_by_name::<BackupJobSpec>("nightly")
        .unwrap()
        .unwrap();
    assert_eq!(job.spec.replicate, None);

    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    assert_error(
        &admin
            .patch("/backup-jobs/nightly", json!({ "replicate": target }), None)
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
}
```

`crates/opl-api/tests/backup_replicate.rs`에서

```rust
//! `status.replicas`에 적는다. 해시가 다른 응답, 꺼진 원격, 원격 쪽 권한, 같은 사본 이어 가기, API 검사.
```

을 다음으로 바꾼다:

```rust
//! `status.replicas`에 적는다. 해시가 다른 응답, 꺼진 원격, 원격 쪽 권한, 같은 사본 이어 가기, API 검사.
//! 백업 작업의 복제(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 한 번 더)와, 원본을 지우면
//! 원격 사본도 지우는 것(사용자·보존 규칙, 404는 성공, 실패는 경고, `force`는 원격을 건드리지 않음).
```

`crates/opl-api/tests/backup_replicate.rs`에서

```rust
use std::path::PathBuf;

use axum::body::Body;
use axum::http::Method;
use common::{Client, Env, TlsServer, vm_request};
use opl_types::backup::{
```

을 다음으로 바꾼다:

```rust
use std::path::PathBuf;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Method;
use common::{Client, Env, TlsServer, WAIT, vm_request};
use opl_api::{BackupJobScheduler, BackupJobSchedulerConfig};
use opl_types::backup::{
```

`crates/opl-api/tests/backup_replicate.rs`에서

```rust
};
use opl_types::node::{NodeSpec, NodeStatus};
```

을 다음으로 바꾼다:

```rust
};
use opl_types::backup_job::{BackupJobSpec, JobRun, JobRunResult};
use opl_types::node::{NodeSpec, NodeStatus};
```

`crates/opl-api/tests/backup_replicate.rs`에서

```rust
    );
}
```

을 다음으로 바꾼다:

```rust
    );
}

/// `done`이 참이 될 때까지 기다린다.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 지금부터 열두 시간 뒤의 `HH:MM`(UTC). 시험 도중 예약 실행이 시작되지 않게 한다.
fn far_at() -> String {
    let t = (now() + 12 * 3600) % 86400;
    format!("{:02}:{:02}", t / 3600, t / 60 % 60)
}

/// 원격 `dr`의 풀 `dr`로 복제하는 켜 둔 작업 `nightly`(예약 실행은 열두 시간 뒤)의 id
async fn replicating_job(admin: &Client) -> Uuid {
    let (status, job) = admin
        .post(
            "/backup-jobs",
            json!({ "name": "nightly", "spec": {
                "vms": { "names": ["web"] }, "pool": "backups",
                "schedule": { "at": far_at() },
                "replicate": { "remote": "dr", "pool": "dr" },
            }}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    job["id"].as_str().unwrap().parse().unwrap()
}

/// 50ms마다 보는 스케줄러
fn scheduler(env: &Env) -> BackupJobScheduler {
    BackupJobScheduler::spawn(
        env.state.clone(),
        BackupJobSchedulerConfig {
            tick: Duration::from_millis(50),
            max_concurrent: 2,
        },
    )
}

/// 작업 `job`의 상태를 `change`로 바꿔 쓴다.
async fn change_job(
    env: &Env,
    job: Uuid,
    change: impl Fn(&mut opl_types::backup_job::BackupJobStatus),
) {
    let mut got = env.cluster.get::<BackupJobSpec>(job).unwrap().unwrap();
    change(&mut got.status);
    env.cluster
        .update_status::<BackupJobSpec>(job, &got.status)
        .await
        .unwrap();
}

fn replica_state(env: &Env, id: Uuid) -> Option<ReplicaState> {
    get_backup(env, id).status.replicas.first().map(|r| r.state)
}

fn exists(env: &Env, id: Uuid) -> bool {
    env.cluster.get::<BackupSpec>(id).unwrap().is_some()
}

/// 복제를 켠 작업은 스케줄러가 그 작업의 ready 백업을 원격으로 보낸다(손으로 만든 백업은 보내지 않는다).
/// 보존 규칙이 원본을 지우면 원격 사본도 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_replicates_its_backups_and_pruning_deletes_the_copies() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let old = ready_backup(&local, "web-old", Some(job)).await;
    let new = ready_backup(&local, "web-new", Some(job)).await;
    // 같은 VM의 백업이고, 오래된 백업은 하루 앞에 만들었다.
    let spec = BackupSpec {
        vm: old.spec.vm,
        ..new.spec.clone()
    };
    local
        .cluster
        .update_spec(new.id, None, new.labels.clone(), &spec)
        .await
        .unwrap();
    let mut earlier = old.status.clone();
    earlier.created_at = earlier.created_at.map(|t| t - 86400);
    local
        .cluster
        .update_status::<BackupSpec>(old.id, &earlier)
        .await
        .unwrap();
    let manual = ready_backup(&local, "web-manual", None).await;
    let sched = scheduler(&local);

    wait_until("both copies", || {
        [old.id, new.id]
            .iter()
            .all(|id| replica_state(&local, *id) == Some(ReplicaState::Ready))
    })
    .await;
    assert!(get_backup(&local, manual.id).status.replicas.is_empty());
    let copy = |id| only_replica(&local, id).backup.unwrap();
    let (old_copy, new_copy) = (copy(old.id), copy(new.id));
    assert_eq!(
        get_backup(&r.env, old_copy).status.phase,
        BackupPhase::Ready
    );
    let origin = get_backup(&r.env, new_copy).spec.origin.unwrap();
    assert_eq!(
        (origin.backup, origin.job.as_deref()),
        (new.id, Some("nightly"))
    );

    // 마지막 하나만 남기는 규칙으로 실행이 끝나면 오래된 원본과 그 사본이 지워진다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "last": 1 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    change_job(&local, job, |s| {
        s.run = Some(JobRun {
            started_at: now(),
            queue: vec![],
            backups: vec![],
        })
    })
    .await;
    wait_until("the pruned backup and its copy", || {
        !exists(&local, old.id) && !exists(&r.env, old_copy)
    })
    .await;
    assert!(exists(&local, new.id));
    assert!(exists(&r.env, new_copy));
    sched.shutdown().await;
}

/// 실패한 사본은 마지막 실행을 시작하기 전에 실패한 것만 한 번 더 보낸다. 실행 전에 ready였던 사본도
/// 실행마다 한 번 원격에 있는지 확인하므로, 원격에서 지운 사본은 다시 보낸다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_retries_copies_that_failed_before_its_last_run() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let started = now() - 100;
    change_job(&local, job, |s| {
        s.last_run = Some(JobRunResult {
            started_at: started,
            finished_at: started + 10,
            ok: 2,
            failed: 0,
        })
    })
    .await;
    let failed = |at: u64| opl_types::backup::BackupReplica {
        remote: "dr".into(),
        pool: "dr".into(),
        backup: None,
        state: ReplicaState::Failed,
        error: Some("down".into()),
        at,
    };
    let before = ready_backup(&local, "web-before", Some(job)).await;
    let since = ready_backup(&local, "web-since", Some(job)).await;
    for (id, at) in [(before.id, started - 1), (since.id, started + 1)] {
        let mut b = get_backup(&local, id);
        b.status.replicas = vec![failed(at)];
        local
            .cluster
            .update_status::<BackupSpec>(id, &b.status)
            .await
            .unwrap();
    }
    let sched = scheduler(&local);

    wait_until("the retried copy", || {
        replica_state(&local, before.id) == Some(ReplicaState::Ready)
    })
    .await;
    // 나중에 생긴 백업이 보내질 때까지 tick이 돌아도(실행 뒤에 실패한 사본이 다시 보낼 것이었다면 만든
    // 순서상 먼저 보냈다) 실행 뒤에 실패한 사본은 그대로다.
    let later = ready_backup(&local, "web-later", Some(job)).await;
    wait_until("the later copy", || {
        replica_state(&local, later.id) == Some(ReplicaState::Ready)
    })
    .await;
    let replica = only_replica(&local, since.id);
    assert_eq!(
        (replica.state, replica.at),
        (ReplicaState::Failed, started + 1)
    );

    // 원격에서 지운 사본은 다음 실행에 다시 보낸다.
    let gone = only_replica(&local, before.id).backup.unwrap();
    let remote_admin = r.env.admin().await;
    let (status, task) = remote_admin.delete(&format!("/backups/{gone}")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    remote_admin.wait_task(&task).await;
    let next = now() + 1;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    change_job(&local, job, |s| {
        s.last_run = Some(JobRunResult {
            started_at: next,
            finished_at: next,
            ok: 3,
            failed: 0,
        })
    })
    .await;
    wait_until("the copy sent again", || {
        let replica = only_replica(&local, before.id);
        replica.state == ReplicaState::Ready && replica.backup != Some(gone)
    })
    .await;
    let again = only_replica(&local, before.id).backup.unwrap();
    assert_eq!(get_backup(&r.env, again).status.phase, BackupPhase::Ready);
    sched.shutdown().await;
}

/// 보존 규칙이 지울 백업이 복제 Task(ready 사본 확인) 중이라 실행이 끝날 때 지우지 못하면(`backup_busy`)
/// tick마다 다시 해서 Task가 끝나면 지운다. 지울 백업에는 확인 Task를 더 만들지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pruning_waits_for_a_running_check() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    // 켜 두면 사본 읽기(GET /backups/{id})를 2초 늦추고 그 요청이 왔음을 알리는 원격
    let slow = Arc::new(AtomicBool::new(false));
    let reached = Arc::new(AtomicBool::new(false));
    let (s, got) = (slow.clone(), reached.clone());
    let r = remote_with(move |app| {
        app.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let (slow, reached) = (s.clone(), got.clone());
                async move {
                    if slow.load(Ordering::SeqCst)
                        && req.method() == Method::GET
                        && req.uri().path().starts_with("/api/v1/backups/")
                    {
                        reached.store(true, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    next.run(req).await
                }
            },
        ))
    })
    .await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let job = replicating_job(&admin).await;
    let old = ready_backup(&local, "web-old", Some(job)).await;
    let new = ready_backup(&local, "web-new", Some(job)).await;
    let spec = BackupSpec {
        vm: old.spec.vm,
        ..new.spec.clone()
    };
    local
        .cluster
        .update_spec(new.id, None, new.labels.clone(), &spec)
        .await
        .unwrap();
    for name in ["web-old", "web-new"] {
        let task = replicate_done(&admin, name).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
    }
    let old_copy = only_replica(&local, old.id).backup.unwrap();

    // 오래된 백업의 사본을 확인하는 Task가 원격의 답을 기다리는 동안
    slow.store(true, Ordering::SeqCst);
    let (status, check) = replicate(&admin, "web-old").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{check}");
    wait_until("the check", || reached.load(Ordering::SeqCst)).await;
    // 마지막 하나만 남기는 규칙으로 실행이 끝난다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "last": 1 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sched = scheduler(&local);
    change_job(&local, job, |s| {
        s.run = Some(JobRun {
            started_at: now(),
            queue: vec![],
            backups: vec![],
        })
    })
    .await;
    wait_until("the end of the run", || {
        let got = local.cluster.get::<BackupJobSpec>(job).unwrap().unwrap();
        got.status.run.is_none()
    })
    .await;
    let (_, task) = admin
        .get(&format!("/tasks/{}", check["id"].as_str().unwrap()))
        .await;
    assert_eq!(task["status"]["state"], "running", "{task}");
    assert!(exists(&local, old.id));

    wait_until("the pruned backup and its copy", || {
        !exists(&local, old.id) && !exists(&r.env, old_copy)
    })
    .await;
    let task = admin.wait_task(&check).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(exists(&local, new.id));
    // 지운 백업의 복제 Task는 손으로 요청한 둘(복제와 확인)뿐이다.
    let (_, tasks) = local.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    let checks = tasks
        .iter()
        .filter(|t| {
            t.spec.target == old.id
                && matches!(t.spec.op, opl_types::task::TaskOp::BackupReplicate { .. })
        })
        .count();
    assert_eq!(checks, 2);
    sched.shutdown().await;
}

/// 원본을 지우면 로컬 파일을 먼저 지우고 원격 사본을 지운다. 원격에서 이미 지운 사본(404)은 성공이다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_backup_deletes_its_remote_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let remote_admin = r.env.admin().await;
    for name in ["web-1", "web-2"] {
        let source = ready_backup(&local, name, None).await;
        let task = replicate_done(&admin, name).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        let copy = only_replica(&local, source.id).backup.unwrap();
        if name == "web-2" {
            let (status, task) = remote_admin.delete(&format!("/backups/{copy}")).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{task}");
            remote_admin.wait_task(&task).await;
            assert!(!exists(&r.env, copy));
        }

        let (status, task) = admin.delete(&format!("/backups/{name}")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let task = admin.wait_task(&task).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        assert_eq!(task["status"]["error"], Value::Null, "{task}");
        assert!(!exists(&local, source.id));
        assert!(!source_dir(&local, source.id).exists());
        wait_until("the remote copy", || !exists(&r.env, copy)).await;
    }
}

/// 원격 사본을 지우지 못해도 원본은 지우고, Task는 성공하되 그 경고를 오류로 남긴다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_copy_that_cannot_be_deleted_leaves_a_warning() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    drop(r.server);

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let error = task["status"]["error"].as_str().unwrap();
    assert!(error.contains("remote copies may be left"), "{error}");
    assert!(error.contains(&copy.to_string()), "{error}");
    assert!(!exists(&local, source.id));
    assert!(exists(&r.env, copy));
}

/// 실패로 끝난 사본이라도 원격에 만든 사본(id)이 있으면 원본을 지울 때 지운다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_backup_deletes_a_failed_copy_too() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let mut b = get_backup(&local, source.id);
    b.status.replicas[0].state = ReplicaState::Failed;
    let copy = b.status.replicas[0].backup.unwrap();
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["status"]["error"], Value::Null, "{task}");
    wait_until("the remote copy", || !exists(&r.env, copy)).await;
}

/// 원본 파일을 지우지 못하면 원격 사본은 건드리지 않는다(원본이 남으면 사본도 남는다).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_copies_are_deleted_after_the_local_files() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    // 디렉터리 자리에 파일이 있으면 지우지 못한다.
    let dir = source_dir(&local, source.id);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, b"not a directory").unwrap();

    let (status, task) = admin.delete("/backups/web-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(exists(&local, source.id));
    // 원격에 지우기를 요청하지 않았다(지우기 Task가 없다).
    let (_, tasks) = r.env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert!(!tasks.iter().any(|t| t.spec.target == copy), "{tasks:?}");
    assert_eq!(get_backup(&r.env, copy).status.phase, BackupPhase::Ready);
}

/// `force`로 리소스만 지우면 원격 사본은 건드리지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn force_deleting_leaves_the_remote_copy() {
    let r = remote().await;
    let (local, admin) = local_with_remote(&r, "operator").await;
    let source = ready_backup(&local, "web-1", None).await;
    let task = replicate_done(&admin, "web-1").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let copy = only_replica(&local, source.id).backup.unwrap();
    // 파일을 쓴 노드가 없어져 파일을 지울 수 없다.
    let mut b = get_backup(&local, source.id);
    b.status.node = Some(Uuid::now_v7());
    local
        .cluster
        .update_status::<BackupSpec>(source.id, &b.status)
        .await
        .unwrap();

    let (status, body) = admin.delete("/backups/web-1?force=true").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!exists(&local, source.id));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(get_backup(&r.env, copy).status.phase, BackupPhase::Ready);
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1516개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 백업 작업 복제와 원격 사본 지우기 — 백업 작업 만들기·고치기의 replicate: PATCH에 replicate(주면 바꾸고 null이면 끄고 없으면 그대로), replicate가 있으면 옛 노드 409 cluster_upgrading과 등록하지 않은 원격 400. 스케줄러(리더 tick)는 복제를 켠 켜 둔 작업의 ready 백업 가운데 대상 (원격, 풀) 사본 기록이 없거나, 실패(또는 복제 Task 없이 copying으로 남은) 기록이 마지막 실행을 시작하기 전의 것을 만든 순서로 backup-replicate Task로 보냄(실패한 사본은 실행마다 한 번 더, 옛 노드가 있으면 보내지 않음, 노드마다 복제 Task 하나·백업 Task와 합쳐 max_concurrent, 백업 dispatch도 복제 Task를 셈). backups::start_backup_replicate(복제 API와 같은 검사, require_current 포함)와 files_node를 pub(crate)로. 백업 지우기 Task(사용자·보존 규칙·janitor)는 파일보다 먼저 replicas의 ready·copying 사본을 원격 DELETE /backups/{id}로 지우고(404는 성공), 지우지 못하면 원본은 지우고 Task는 성공하되 error에 \"backup deleted, but remote copies may be left: …\" 경고(perform이 Ok(Some(경고))를 돌려줄 수 있게). force로 리소스만 지우면 원격은 건드리지 않음. schema.d.ts 다시 만듦. 시험: backup_jobs.rs(PATCH replicate 켜기·그대로·끄기·검사·옛 노드 409, 만들기의 없는 원격 400), backup_replicate.rs(작업이 ready 백업을 복제하고 손으로 만든 백업은 보내지 않음과 보존 규칙이 지우면 원격 사본도 지움, 실행 전에 실패한 사본만 다시, 지우기가 원격 사본을 지움·원격에서 이미 지운 사본 404는 성공, 원격이 꺼지면 경고하고 원본은 지움, force는 원격 사본을 남김), to_replicate 단위"
```

---

### Task 5: oplctl 백업 복제 명령과 opld E2E

`oplctl backup replicate <백업> --remote --pool [--wait]`, `backup list`의 `ORIGIN`·`REPLICAS` 열, `backup-job create|set --replicate <원격>/<풀>`·`--no-replicate`, `backup delete`의 원격 사본 경고. opld E2E(기능 `qemu-it`, qemu-img 필요): 클러스터 둘에서 복제 → DR 쪽 사본 복원 → 같은 디스크 내용 → 원본 지우면 사본도 지워짐.

**Files:**
- Create: `crates/opld/tests/replicate.rs`
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

use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca, fingerprint,
    local, seg,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version = opl_types::version::BUILD, about = "opLite command-line client")]
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
    /// 저장한 API 토큰을 지운다(login이 만든 토큰 `oplctl-…`이면 서버에서도).
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
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
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
    /// 인증 영역(admin만): 바깥 LDAP 디렉터리나 OIDC IdP로 로그인하게 한다.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
    /// 원격 클러스터: 다른 opLite 클러스터를 등록해 상태를 모아 보고, 그 클러스터의 API를 이 클러스터를
    /// 거쳐 부른다(저장한 그 클러스터의 API 토큰으로).
    Remote {
        #[command(subcommand)]
        cmd: RemoteCmd,
    },
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin", conflicts_with = "token_file")]
    user: String,
    /// 인증 영역. `local`이 아니면 그 영역의 사용자 이름(`@영역` 없이)으로 로그인한다.
    #[arg(long, default_value = "local", conflicts_with = "token_file")]
    realm: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long, conflicts_with = "token_file")]
    password_file: Option<PathBuf>,
    /// 웹 UI에서 만든 API 토큰이 든 파일(`-`면 표준 입력). 비밀번호 대신 이 토큰을 확인해 저장한다
    /// (SSO(OIDC) 사용자).
    #[arg(long)]
    token_file: Option<PathBuf>,
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
    Get {
        node: String,
    },
    /// 노드의 업데이트 확인 결과(올릴 수 있는 패키지)를 보인다.
    Updates {
        node: String,
        /// 먼저 노드에서 다시 확인하고(Task가 끝날 때까지 기다린다) 결과를 보인다.
        #[arg(long)]
        refresh: bool,
    },
    /// 노드의 패키지를 올린다(`apt-get dist-upgrade`). VM은 계속 돌고 opld가 잠깐 다시 시작한다. 한 번에
    /// 한 노드만 한다.
    Upgrade {
        node: String,
        /// 업그레이드 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn node_upgrade(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/upgrade", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, UPGRADE_WAIT).await?;
        writeln!(out, "node {node} upgraded")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "upgrade of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node updates`의 패키지 표 열
const UPDATE_HEADERS: &[&str] = &["PACKAGE", "CURRENT", "CANDIDATE"];

/// `node updates`: `--refresh`면 다시 확인 Task를 기다린 뒤, 노드의 `status.updates`를 보인다. 확인 시각,
/// 오류, 패키지 표(새로 설치할 패키지는 CURRENT가 `-`), 잘렸는지. `-o json`이면 `status.updates` 그대로.
async fn node_updates(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{}/updates/refresh", seg(node)), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let found = client.get(&format!("/nodes/{}", seg(node))).await?;
    let updates = &found["status"]["updates"];
    if json_out {
        return Ok(print_json(out, updates)?);
    }
    if !updates.is_object() {
        bail!("node {node} does not check for updates");
    }
    let checked = updates["checked_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "checked: {checked}")?;
    if let Some(error) = updates["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let packages = updates["packages"].as_array().cloned().unwrap_or_default();
    if packages.is_empty() {
        if updates["checked_at"].is_u64() && updates["error"].is_null() {
            writeln!(out, "no updates available")?;
        }
        return Ok(());
    }
    let rows: Vec<_> = packages
        .iter()
        .map(|p| vec![cell(&p["name"]), cell(&p["current"]), cell(&p["candidate"])])
        .collect();
    table(out, UPDATE_HEADERS, &rows)?;
    if updates["truncated"] == true {
        writeln!(out, "(only the first {} packages are shown)", rows.len())?;
    }
    Ok(())
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
    let path = format!("/security-groups/{}", seg(group));
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
    /// 백업을 원격 클러스터(`remote add`로 등록)의 백업 풀에 사본으로 보낸다. 원격 쪽 토큰 주인은
    /// operator 이상이어야 한다.
    Replicate {
        backup: String,
        /// 원격 이름
        #[arg(long)]
        remote: String,
        /// 원격의 백업 풀
        #[arg(long)]
        pool: String,
        /// 복제 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate)]
    replicate: Option<Value>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
    "replicate", "no_replicate",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
    /// 작업이 만든 백업을 원격의 백업 풀에 사본으로 보낸다.
    #[arg(long, value_name = "REMOTE/POOL", value_parser = parse_replicate, conflicts_with = "no_replicate")]
    replicate: Option<Value>,
    /// 원격 복제를 끈다. 이미 보낸 사본은 남는다.
    #[arg(long)]
    no_replicate: bool,
}

/// `--replicate`의 `REMOTE/POOL`을 `replicate` 값(`{ remote, pool }`)으로
fn parse_replicate(s: &str) -> Result<Value, String> {
    match s.split_once('/') {
        Some((remote, pool)) if !remote.is_empty() && !pool.is_empty() && !pool.contains('/') => {
            Ok(json!({ "remote": remote, "pool": pool }))
        }
        _ => Err(format!("invalid target {s:?}: use REMOTE/POOL")),
    }
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    let mut body = json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    });
    // 복제하지 않으면 보내지 않는다(옛 서버는 모르는 필드를 거절한다).
    if let Some(replicate) = &args.replicate {
        body["spec"]["replicate"] = replicate.clone();
    }
    body
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    // `null`은 복제를 끈다(생략하면 그대로).
    if let Some(replicate) = &args.replicate {
        body["replicate"] = replicate.clone();
    } else if args.no_replicate {
        body["replicate"] = Value::Null;
    }
    body
}

#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 opld를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 opLite 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. opLite 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
        /// 예: `updates.channel=testing`
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

/// `settings set`이 아는 키
const SETTING_KEYS: &[&str] = &["updates.channel"];

/// `settings set`의 요청 본문(`PATCH /settings`). 준 키만 보낸다.
fn settings_set_body(values: &[String]) -> anyhow::Result<Value> {
    let mut body = json!({});
    for value in values {
        let Some((key, v)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got {value:?}");
        };
        match key {
            "updates.channel" => body["updates"]["channel"] = json!(v),
            _ => bail!("unknown setting {key}; known: {}", SETTING_KEYS.join(", ")),
        }
    }
    Ok(body)
}

/// 객체의 값들을 `a.b: 값` 줄로 펼친다.
fn setting_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    match value.as_object() {
        Some(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                setting_lines(&key, v, lines);
            }
        }
        None => lines.push(format!("{prefix}: {}", cell(value))),
    }
}

async fn settings(
    client: &Client,
    cmd: SettingsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SettingsCmd::Get => {
            let settings = client.get("/settings").await?;
            if json_out {
                print_json(out, &settings)?;
            } else {
                let mut lines = Vec::new();
                setting_lines("", &settings["spec"], &mut lines);
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        SettingsCmd::Set { values } => {
            client
                .patch("/settings", &settings_set_body(&values)?)
                .await?;
            writeln!(out, "settings updated")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// 원격 클러스터들과 마지막으로 모은 상태(이름, 클러스터, 버전, 노드 online/total, VM running/total)
    List,
    /// 원격 하나의 설정과 상태를 보인다(토큰은 id만).
    Show { name: String },
    /// 원격을 등록한다(admin). 서버가 그 클러스터에 연결해 CA 지문과 토큰을 확인한다.
    Add(RemoteAdd),
    /// 원격을 바꾼다(admin). 준 것만 바꾼다. 주소나 지문을 바꾸면 토큰도 다시 준다.
    Set(RemoteSet),
    /// 원격을 지운다(admin). 원격 클러스터의 토큰은 그대로 남는다.
    Remove { name: String },
    /// 원격 상태를 곧 다시 모아 보인다(admin).
    Refresh { name: String },
    /// 원격 클러스터의 VM 목록(이 클러스터를 거쳐 읽는다)
    Vms { name: String },
    /// 원격 클러스터의 VM을 시작하거나(start) 정지한다(stop — 로컬 `vm stop`처럼 ACPI, `--force`면
    /// 곧바로 끄기). 원격 쪽 권한은 토큰 주인의 역할이 정한다. Task를 기다리지 않는다.
    Vm {
        name: String,
        #[arg(value_enum)]
        action: RemoteVmAction,
        vm: String,
        /// stop에서 ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args)]
struct RemoteAdd {
    name: String,
    /// 원격 클러스터 노드의 API 주소(`https://<호스트>[:포트]`)
    #[arg(long)]
    url: String,
    /// 원격 클러스터 CA의 지문(`sha256:...`, 원격 노드의 `oplctl cluster status`). 없으면 CA를 받아
    /// 지문을 보이고 확인을 묻는다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 원격 클러스터에서 만든 API 토큰이 든 파일(`-`면 표준 입력 — 그때는 확인을 물을 수 없으므로
    /// `--ca-fingerprint`나 `--yes`가 필요하다)
    #[arg(long)]
    token_file: PathBuf,
    /// 받은 CA 지문을 묻지 않고 믿는다.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct RemoteSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    ca_fingerprint: Option<String>,
    /// 새 API 토큰이 든 파일(`-`면 표준 입력). 주지 않으면 저장된 토큰을 그대로 둔다.
    #[arg(long)]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum RemoteVmAction {
    Start,
    Stop,
}

/// 원격 VM 동작이 부르는 원격 API의 동작(`/vms/<VM>/actions/<동작>`)과 보일 이름
fn remote_vm_action(
    action: RemoteVmAction,
    force: bool,
) -> anyhow::Result<(&'static str, &'static str)> {
    Ok(match (action, force) {
        (RemoteVmAction::Start, false) => ("start", "start"),
        (RemoteVmAction::Start, true) => bail!("--force is only for stop"),
        (RemoteVmAction::Stop, false) => ("stop", "stop"),
        (RemoteVmAction::Stop, true) => ("stop?mode=force", "force stop"),
    })
}

/// 토큰 파일(`-`면 표준 입력)의 토큰. 앞뒤 공백을 뗀다.
fn read_remote_token(file: &PathBuf) -> anyhow::Result<String> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    Ok(token)
}

/// 받은 CA 지문을 보이고 확인을 받는다. `--yes`면 묻지 않고, 터미널이 아니면 묻지 못하므로
/// `--ca-fingerprint`로 다시 실행하게 한다.
fn confirm_fingerprint(
    actual: &str,
    yes: bool,
    terminal: bool,
    input: &mut dyn std::io::BufRead,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    writeln!(out, "the remote cluster CA fingerprint is {actual}")?;
    if yes {
        return Ok(());
    }
    if !terminal {
        bail!(
            "check the fingerprint (`oplctl cluster status` on a node of the remote cluster) and \
             run again with --ca-fingerprint {actual}"
        );
    }
    write!(out, "Is this the CA of the remote cluster? [y/N] ")?;
    out.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        bail!("cancelled");
    }
    Ok(())
}

/// `<a>/<b>` 모양의 수(`online/total` 등). 모르면 `-`.
fn remote_counts(v: &Value, part: &str) -> String {
    if v.is_null() {
        return "-".into();
    }
    format!("{}/{}", cell(&v[part]), cell(&v["total"]))
}

/// 원격의 상태 칸: 닿으면 `reachable`, 아니면 `unreachable: <이유>`
fn remote_state(status: &Value) -> String {
    if status["reachable"] == true {
        "reachable".into()
    } else {
        format!("unreachable: {}", cell(&status["error"]))
    }
}

/// `remote list`의 열
const REMOTE_HEADERS: &[&str] = &["NAME", "CLUSTER", "VERSION", "NODES", "VMS", "STATUS"];

/// `remote list`의 한 행
fn remote_row(r: &Value) -> Vec<String> {
    let s = &r["status"];
    vec![
        cell(&r["name"]),
        cell(&s["cluster_name"]),
        cell(&s["version"]),
        remote_counts(&s["nodes"], "online"),
        remote_counts(&s["vms"], "running"),
        remote_state(s),
    ]
}

/// `remote show`의 `키: 값` 줄
fn remote_lines(r: &Value) -> Vec<String> {
    let s = &r["status"];
    let checked = match s["checked_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    let memory = match s["memory_gib"].as_u64() {
        Some(gib) => format!("{gib} GiB"),
        None => "-".into(),
    };
    vec![
        format!("name: {}", cell(&r["name"])),
        format!("url: {}", cell(&r["url"])),
        format!("ca fingerprint: {}", cell(&r["ca_fingerprint"])),
        format!("token id: {}", cell(&r["token_id"])),
        format!("status: {}", remote_state(s)),
        format!("changed at: {checked}"),
        format!("cluster: {}", cell(&s["cluster_name"])),
        format!("version: {}", cell(&s["version"])),
        format!("nodes online: {}", remote_counts(&s["nodes"], "online")),
        format!("vms running: {}", remote_counts(&s["vms"], "running")),
        format!("cpus: {}", cell(&s["cpus"])),
        format!("memory: {memory}"),
    ]
}

/// `remote set`의 본문: 지금 원격에 준 것만 바꾼다. 토큰을 주지 않으면 빼서 저장된 토큰을 둔다.
fn remote_set_body(current: &Value, args: &RemoteSet, token: Option<String>) -> Value {
    let mut body = json!({
        "url": args.url.as_deref().map_or_else(|| current["url"].clone(), |u| json!(u)),
        "ca_fingerprint": args
            .ca_fingerprint
            .as_deref()
            .map_or_else(|| current["ca_fingerprint"].clone(), |f| json!(f)),
    });
    if let Some(token) = token {
        body["token"] = json!(token);
    }
    body
}

async fn remote(
    client: &Client,
    cmd: RemoteCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let show = |out: &mut (dyn Write + Send), r: &Value| -> anyhow::Result<()> {
        if json_out {
            print_json(out, r)?;
        } else {
            for line in remote_lines(r) {
                writeln!(out, "{line}")?;
            }
        }
        Ok(())
    };
    match cmd {
        RemoteCmd::List => {
            let list = client.get("/remotes").await?;
            print_list(out, json_out, &list, REMOTE_HEADERS, remote_row)?;
        }
        RemoteCmd::Show { name } => {
            show(out, &client.get(&format!("/remotes/{}", seg(&name))).await?)?
        }
        RemoteCmd::Add(args) => {
            // 표준 입력은 토큰을 읽느라 다 썼으므로 지문 확인을 물을 수 없다.
            if args.token_file.as_os_str() == "-" && args.ca_fingerprint.is_none() && !args.yes {
                bail!("reading the token from stdin needs --ca-fingerprint or --yes");
            }
            let token = read_remote_token(&args.token_file)?;
            let fingerprint = match args.ca_fingerprint {
                Some(f) => f,
                None => {
                    let actual = fingerprint(&fetch_ca(&args.url).await?)?;
                    use std::io::IsTerminal;
                    let stdin = std::io::stdin();
                    let terminal = stdin.is_terminal();
                    confirm_fingerprint(&actual, args.yes, terminal, &mut stdin.lock(), out)?;
                    actual
                }
            };
            let body = json!({
                "name": args.name,
                "url": args.url,
                "ca_fingerprint": fingerprint,
                "token": token,
            });
            let created = client.post("/remotes", &body).await?;
            writeln!(
                out,
                "remote {} added ({})",
                args.name,
                remote_state(&created["status"])
            )?;
        }
        RemoteCmd::Set(args) => {
            let token = args
                .token_file
                .as_ref()
                .map(read_remote_token)
                .transpose()?;
            let path = format!("/remotes/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&remote_set_body(&current, &args, token));
            client.send(r).await?;
            writeln!(out, "remote {} updated", args.name)?;
        }
        RemoteCmd::Remove { name } => {
            client.delete(&format!("/remotes/{}", seg(&name))).await?;
            writeln!(out, "remote {name} removed")?;
        }
        RemoteCmd::Refresh { name } => {
            let r = client
                .post(
                    &format!("/remotes/{}/actions/refresh", seg(&name)),
                    &json!({}),
                )
                .await?;
            show(out, &r)?;
        }
        RemoteCmd::Vms { name } => {
            let list = client
                .get(&format!("/remotes/{}/api/vms", seg(&name)))
                .await?;
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        RemoteCmd::Vm {
            name,
            action,
            vm,
            force,
        } => {
            let (action, shown) = remote_vm_action(action, force)?;
            let task = client
                .send(client.request(
                    Method::POST,
                    // 동작은 이 파일이 정한 값이고 쿼리를 담으므로 인코딩하지 않는다.
                    &format!(
                        "/remotes/{}/api/vms/{}/actions/{}",
                        seg(&name),
                        seg(&vm),
                        action
                    ),
                ))
                .await?;
            writeln!(
                out,
                "vm {vm} on remote {name}: {shown} requested (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
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

#[derive(Subcommand)]
enum RealmCmd {
    List,
    /// 영역 설정을 보인다(서비스 계정 비밀번호·클라이언트 비밀은 있는지만).
    Show {
        name: String,
    },
    /// LDAP 영역을 만든다.
    AddLdap(RealmAdd),
    /// LDAP 영역을 바꾼다. 준 것만 바꾼다.
    SetLdap(RealmSet),
    /// OIDC 영역을 만든다. IdP에는 `<노드 주소>/api/v1/auth/oidc/callback`을 리디렉트 URI로 등록한다.
    AddOidc(RealmAddOidc),
    /// OIDC 영역을 바꾼다. 준 것만 바꾼다.
    SetOidc(RealmSetOidc),
    /// 영역을 지운다. 그 영역 사용자들의 세션과 토큰은 멈춘다.
    Remove {
        name: String,
    },
    /// 영역을 시험한다(사용자를 만들지 않는다). LDAP은 사용자 이름과 비밀번호로 디렉터리를,
    /// OIDC는 입력 없이 IdP의 discovery 문서와 키를 확인한다.
    Test {
        name: String,
        /// 영역 안의 사용자 이름(`@영역` 없이). LDAP 영역에는 꼭 필요하고, OIDC 영역이면 무시한다(경고).
        #[arg(long)]
        user: Option<String>,
        /// 비밀번호가 든 파일(`-`면 표준 입력). 없으면 터미널에서 묻는다.
        #[arg(long, requires = "user")]
        password_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RealmAdd {
    name: String,
    /// `ldap://host[:port]`(StartTLS) 또는 `ldaps://host[:port]`
    #[arg(long)]
    url: String,
    /// 사용자를 찾을 기준 DN
    #[arg(long)]
    base_dn: String,
    #[command(flatten)]
    ldap: LdapArgs,
}

#[derive(Args)]
struct RealmSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    base_dn: Option<String>,
    #[command(flatten)]
    ldap: LdapArgs,
    /// 저장된 서비스 계정 비밀번호를 지운다. 서비스 계정 DN이 남으면 안 된다(`--clear-bind-dn`).
    #[arg(long, conflicts_with = "bind_password_file")]
    clear_bind_password: bool,
    /// 서비스 계정 DN과 비밀번호를 지운다(익명 검색).
    #[arg(long, conflicts_with_all = ["bind_dn", "bind_password_file"])]
    clear_bind_dn: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

#[derive(Args)]
struct RealmAddOidc {
    name: String,
    /// IdP의 issuer(`https://...`). discovery 문서의 `issuer`와 글자 그대로 같아야 한다.
    #[arg(long)]
    issuer: String,
    /// IdP에 등록한 클라이언트 ID
    #[arg(long)]
    client_id: String,
    #[command(flatten)]
    oidc: OidcArgs,
}

#[derive(Args)]
struct RealmSetOidc {
    name: String,
    #[arg(long)]
    issuer: Option<String>,
    #[arg(long)]
    client_id: Option<String>,
    #[command(flatten)]
    oidc: OidcArgs,
    /// 저장된 클라이언트 비밀을 지운다(공개 클라이언트).
    #[arg(long, conflicts_with = "client_secret_file")]
    clear_client_secret: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

/// `realm add-oidc`와 `set-oidc`의 공통 선택
#[derive(Args)]
struct OidcArgs {
    /// 클라이언트 비밀이 든 파일(`-`면 표준 입력). 없으면 공개 클라이언트(PKCE만)
    #[arg(long)]
    client_secret_file: Option<PathBuf>,
    /// 요청할 scope(여러 번, `openid`가 있어야 함). set-oidc에서 주면 목록 전체를 바꾼다.
    #[arg(long = "scope", value_name = "SCOPE")]
    scopes: Vec<String>,
    /// 사용자 이름을 담은 클레임(기본 preferred_username)
    #[arg(long)]
    username_claim: Option<String>,
    /// 그룹(문자열 배열)을 담은 클레임(기본 groups)
    #[arg(long)]
    groups_claim: Option<String>,
    /// IdP 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// IdP 요청마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹 클레임 값을 가진 사용자의 역할(`<그룹>=<role>`, 여러 번). set-oidc에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "GROUP=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `realm add-ldap`과 `set-ldap`의 공통 선택
#[derive(Args)]
struct LdapArgs {
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[arg(long)]
    bind_dn: Option<String>,
    /// 서비스 계정 비밀번호가 든 파일(`-`면 표준 입력)
    #[arg(long)]
    bind_password_file: Option<PathBuf>,
    /// 사용자 찾기 필터. `{username}`이 로그인 이름으로 바뀐다.
    #[arg(long)]
    user_filter: Option<String>,
    /// 그룹 DN을 담은 사용자 속성(기본 memberOf)
    #[arg(long)]
    group_attr: Option<String>,
    /// 서버 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 연결과 작업마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹에 든 사용자의 역할(`<그룹 DN>=<role>`, 여러 번). set-ldap에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "DN=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `--group`의 `<그룹 DN>=<role>`. DN에 `=`가 있으므로 마지막 `=`에서 나눈다.
fn parse_group_role(s: &str) -> Result<(String, Role), String> {
    let (group, role) = s
        .rsplit_once('=')
        .ok_or_else(|| format!("invalid group {s:?}: use <group DN>=<role>"))?;
    let role = Role::from_str(role, true)
        .map_err(|_| format!("invalid role {role:?} in {s:?}: use admin, operator or viewer"))?;
    if group.trim().is_empty() {
        return Err(format!("invalid group {s:?}: use <group DN>=<role>"));
    }
    Ok((group.to_string(), role))
}

/// 파일에서 읽은 영역 값들(서비스 계정 비밀번호, CA)
#[derive(Default)]
struct LdapFiles {
    bind_password: Option<String>,
    ca_pem: Option<String>,
}

impl LdapFiles {
    fn read(args: &LdapArgs) -> anyhow::Result<Self> {
        Ok(Self {
            bind_password: args
                .bind_password_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// 비밀번호 파일(`-`면 표준 입력). 끝의 줄바꿈만 뗀다.
fn read_secret_file(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    read_password(Some(path), "")
}

fn group_roles(groups: &[(String, Role)]) -> Value {
    groups
        .iter()
        .map(|(group, role)| json!({ "group": group, "role": value_name(*role) }))
        .collect()
}

/// `realm add-ldap`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_body(args: &RealmAdd, files: LdapFiles) -> Value {
    let l = &args.ldap;
    let mut backend = json!({ "kind": "ldap", "url": args.url, "base_dn": args.base_dn });
    let optional = [
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("bind_password", files.bind_password.map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&l.groups, l.default_role)
    });
    if let Some(display) = &l.display {
        body["display"] = json!(display);
    }
    body
}

/// 만들 영역의 그룹 → 역할 표와 기본 역할
fn new_roles(groups: &[(String, Role)], default: Option<Role>) -> Value {
    let mut roles = json!({ "groups": group_roles(groups) });
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    roles
}

/// 바꿀 영역의 역할: 지금 표(`current`)에 준 것만 바꾼다.
fn changed_roles(
    current: &Value,
    groups: &[(String, Role)],
    default: Option<Role>,
    clear_groups: bool,
    clear_default_role: bool,
) -> Value {
    let mut roles = current.clone();
    if !groups.is_empty() {
        roles["groups"] = group_roles(groups);
    }
    if clear_groups {
        roles["groups"] = json!([]);
    }
    if let Some(role) = default {
        roles["default"] = json!(value_name(role));
    }
    if clear_default_role && let Some(r) = roles.as_object_mut() {
        r.remove("default");
    }
    roles
}

/// `set-ldap`·`set-oidc`가 바꿀 영역의 종류가 맞는지
fn ensure_kind(current: &Value, name: &str, kind: &str) -> anyhow::Result<()> {
    if current["backend"]["kind"] != kind {
        anyhow::bail!(
            "realm {name} is not an {} realm (its kind is {})",
            kind.to_uppercase(),
            cell(&current["backend"]["kind"])
        );
    }
    Ok(())
}

/// 파일에서 읽은 OIDC 영역 값들(클라이언트 비밀, CA)
#[derive(Default)]
struct OidcFiles {
    client_secret: Option<String>,
    ca_pem: Option<String>,
}

impl OidcFiles {
    fn read(args: &OidcArgs) -> anyhow::Result<Self> {
        Ok(Self {
            client_secret: args
                .client_secret_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// `realm add-oidc`·`set-oidc`가 준 선택 값들(이름과 값)
fn oidc_options(o: &OidcArgs, files: OidcFiles) -> [(&'static str, Option<Value>); 6] {
    [
        ("client_secret", files.client_secret.map(Value::from)),
        (
            "scopes",
            (!o.scopes.is_empty()).then(|| Value::from(o.scopes.clone())),
        ),
        ("username_claim", o.username_claim.clone().map(Value::from)),
        ("groups_claim", o.groups_claim.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", o.timeout.map(Value::from)),
    ]
}

/// `realm add-oidc`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_oidc_body(args: &RealmAddOidc, files: OidcFiles) -> Value {
    let o = &args.oidc;
    let mut backend = json!({ "kind": "oidc", "issuer": args.issuer, "client_id": args.client_id });
    for (key, value) in oidc_options(o, files) {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut body = json!({
        "name": args.name,
        "backend": backend,
        "roles": new_roles(&o.groups, o.default_role)
    });
    if let Some(display) = &o.display {
        body["display"] = json!(display);
    }
    body
}

/// `realm set-oidc`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 클라이언트 비밀은 주지 않으면 빼서(서버가 저장된 값을 둔다 — issuer·client_id·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_oidc_body(
    current: &Value,
    args: &RealmSetOidc,
    files: OidcFiles,
) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "oidc")?;
    let o = &args.oidc;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_client_secret");
    }
    let mut changes = vec![
        ("issuer", args.issuer.clone().map(Value::from)),
        ("client_id", args.client_id.clone().map(Value::from)),
    ];
    changes.extend(oidc_options(o, files));
    if args.clear_client_secret {
        changes.push(("client_secret", Some(json!(""))));
    }
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if args.clear_ca
        && let Some(b) = backend.as_object_mut()
    {
        b.remove("ca_pem");
    }
    let roles = changed_roles(
        &current["roles"],
        &o.groups,
        o.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = o
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// `realm set-ldap`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 서비스 계정 비밀번호는 주지 않으면 빼서(서버가 저장된 값을 둔다 — url·bind_dn·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_body(current: &Value, args: &RealmSet, files: LdapFiles) -> anyhow::Result<Value> {
    ensure_kind(current, &args.name, "ldap")?;
    let l = &args.ldap;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_bind_password");
    }
    let changes = [
        ("url", args.url.clone().map(Value::from)),
        ("base_dn", args.base_dn.clone().map(Value::from)),
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
        (
            "bind_password",
            if args.clear_bind_password || args.clear_bind_dn {
                Some(json!(""))
            } else {
                files.bind_password.map(Value::from)
            },
        ),
    ];
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if let Some(b) = backend.as_object_mut() {
        if args.clear_bind_dn {
            b.remove("bind_dn");
        }
        if args.clear_ca {
            b.remove("ca_pem");
        }
        // 서비스 계정 DN에는 비밀번호가 있어야 한다.
        if args.clear_bind_password && b.get("bind_dn").is_some_and(Value::is_string) {
            anyhow::bail!(
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
    }
    let roles = changed_roles(
        &current["roles"],
        &l.groups,
        l.default_role,
        args.clear_groups,
        args.clear_default_role,
    );
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = l
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// OIDC 영역의 콜백 경로. IdP에는 노드 주소 뒤에 붙여 리디렉트 URI로 등록한다.
const OIDC_CALLBACK_PATH: &str = "/api/v1/auth/oidc/callback";

/// `realm show`의 `키: 값` 줄
fn realm_lines(realm: &Value) -> Vec<String> {
    let b = &realm["backend"];
    let set = |v: bool| if v { "set" } else { "none" };
    let ca = if b["ca_pem"].is_string() {
        "custom (PEM)"
    } else {
        "system roots"
    };
    let mut lines = vec![
        format!("name: {}", cell(&realm["name"])),
        format!("display: {}", cell(&realm["display"])),
        format!("kind: {}", cell(&b["kind"])),
    ];
    if b["kind"] == "oidc" {
        let scopes: Vec<String> = b["scopes"]
            .as_array()
            .map(|a| a.iter().map(cell).collect())
            .unwrap_or_default();
        lines.extend([
            format!("issuer: {}", cell(&b["issuer"])),
            format!("client id: {}", cell(&b["client_id"])),
            format!("client secret: {}", set(b["has_client_secret"] == true)),
            format!("scopes: {}", scopes.join(" ")),
            format!("username claim: {}", cell(&b["username_claim"])),
            format!("groups claim: {}", cell(&b["groups_claim"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
            // IdP에는 노드 주소 뒤에 이 경로를 붙여 리디렉트 URI로 등록한다.
            format!("callback path: {OIDC_CALLBACK_PATH}"),
        ]);
    } else {
        lines.extend([
            format!("url: {}", cell(&b["url"])),
            format!("base dn: {}", cell(&b["base_dn"])),
            format!("bind dn: {}", cell(&b["bind_dn"])),
            format!("bind password: {}", set(b["has_bind_password"] == true)),
            format!("user filter: {}", cell(&b["user_filter"])),
            format!("group attribute: {}", cell(&b["group_attr"])),
            format!("ca: {ca}"),
            format!("timeout: {}s", cell(&b["timeout_secs"])),
        ]);
    }
    let groups = realm["roles"]["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        lines.push("groups: none".into());
    } else {
        lines.push("groups:".into());
        for g in &groups {
            lines.push(format!("  {} = {}", cell(&g["group"]), cell(&g["role"])));
        }
    }
    lines.push(format!(
        "default role: {}",
        realm["roles"]["default"].as_str().unwrap_or("none")
    ));
    lines
}

async fn realm(
    client: &Client,
    cmd: RealmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RealmCmd::List => {
            let list = client.get("/realms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "DISPLAY", "KIND", "URL"],
                |r| {
                    vec![
                        cell(&r["name"]),
                        cell(&r["display"]),
                        cell(&r["backend"]["kind"]),
                        // OIDC 영역은 issuer
                        cell(if r["backend"]["kind"] == "oidc" {
                            &r["backend"]["issuer"]
                        } else {
                            &r["backend"]["url"]
                        }),
                    ]
                },
            )?;
        }
        RealmCmd::Show { name } => {
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            if json_out {
                print_json(out, &realm)?;
            } else {
                for line in realm_lines(&realm) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        RealmCmd::AddLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            client
                .post("/realms", &realm_add_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::AddOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            client
                .post("/realms", &realm_add_oidc_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetOidc(args) => {
            let files = OidcFiles::read(&args.oidc)?;
            let path = format!("/realms/{}", seg(&args.name));
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_oidc_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::Remove { name } => {
            client.delete(&format!("/realms/{}", seg(&name))).await?;
            writeln!(out, "realm {name} removed")?;
        }
        RealmCmd::Test {
            name,
            user,
            password_file,
        } => {
            // OIDC 영역은 입력 없이 시험한다(LDAP 영역이면 서버가 사용자를 달라고 한다).
            let realm = client.get(&format!("/realms/{}", seg(&name))).await?;
            let kind = realm["backend"]["kind"].as_str().unwrap_or_default();
            if kind == "oidc" && user.is_some() {
                eprintln!("warning: --user is ignored for an OIDC realm");
            }
            let body = match &user {
                Some(user) if kind != "oidc" => {
                    let password = match &password_file {
                        Some(f) => read_secret_file(f)?,
                        None => read_password(None, "Password: ")?,
                    };
                    json!({ "username": user, "password": password })
                }
                _ => json!({}),
            };
            let result = client
                .post(&format!("/realms/{}/actions/test", seg(&name)), &body)
                .await?;
            let steps = result["steps"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &result)?;
            } else {
                let rows: Vec<Vec<String>> = steps
                    .iter()
                    .map(|s| {
                        let ok = if s["ok"] == true { "ok" } else { "failed" };
                        vec![cell(&s["name"]), ok.into(), cell(&s["message"])]
                    })
                    .collect();
                table(out, &["STEP", "RESULT", "MESSAGE"], &rows)?;
            }
            // 사용자 결과(DN·그룹·역할)는 LDAP 시험에만 있다.
            if !json_out && kind == "ldap" {
                writeln!(out, "dn: {}", cell(&result["dn"]))?;
                let groups: Vec<String> = result["groups"]
                    .as_array()
                    .map(|a| a.iter().map(cell).collect())
                    .unwrap_or_default();
                writeln!(
                    out,
                    "groups: {}",
                    if groups.is_empty() {
                        "-".into()
                    } else {
                        groups.join(", ")
                    }
                )?;
                writeln!(out, "role: {}", result["role"].as_str().unwrap_or("none"))?;
            }
            if let Some(failed) = steps.iter().find(|s| s["ok"] != true) {
                bail!("realm {name}: the {} step failed", cell(&failed["name"]));
            }
        }
    }
    Ok(())
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

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
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
            let read = client.get(&format!("/storage-pools/{}", seg(pool))).await;
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
        let (token, name) = match &args.token_file {
            Some(file) => token_login(&server, file, ca_pem.clone()).await?,
            None => login(&server, args, ca_pem.clone()).await?,
        };
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        // 새 토큰과 같으면 두고, 이 CLI의 login이 만든 토큰(`oplctl-…`)만 지운다(`--token-file`로 넣은
        // 토큰은 사용자가 웹 UI에서 만든 것이다).
        if let Some((old, old_server)) = previous
            && old != token
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(old), ca_pem)?;
            remove_login_token(&client, id).await;
        }
        writeln!(out, "logged in to {server} as {name}")?;
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
            // login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 이미 지워진 토큰이어도 로컬에서는 지운다.
            let kept = match config.token.as_deref().and_then(token_id) {
                Some(id) => remove_login_token(&client, id).await == Some(false),
                None => false,
            };
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
            if kept {
                writeln!(
                    out,
                    "the token stays valid on the server; delete it in the web UI"
                )?;
            }
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{}", seg(&node))).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
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
                let pool = client
                    .get(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
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
                        &format!("/storage-pools/{}", seg(&set.pool)),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client
                    .delete(&format!("/storage-pools/{}", seg(&pool)))
                    .await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => print_json(
                out,
                &client.get(&format!("/networks/{}", seg(&network))).await?,
            )?,
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client
                    .delete(&format!("/networks/{}", seg(&network)))
                    .await?;
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
            VolumeCmd::Get { volume } => print_json(
                out,
                &client.get(&format!("/volumes/{}", seg(&volume))).await?,
            )?,
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
                    .post(&format!("/volumes/{}/resize", seg(&volume)), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{}", seg(&volume))).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
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
                print_json(out, &client.get(&format!("/tasks/{}", seg(&task))).await?)?
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
                client.put(&format!("/users/{}", seg(&name)), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{}", seg(&name))).await?;
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
                client.delete(&format!("/tokens/{}", seg(&token))).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
        Cmd::Realm { cmd } => realm(&client, cmd, json_out, out).await?,
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
        Cmd::Remote { cmd } => remote(&client, cmd, json_out, out).await?,
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
            &client
                .get(&format!("/security-groups/{}", seg(&group)))
                .await?,
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
            client
                .delete(&format!("/security-groups/{}", seg(&group)))
                .await?;
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
            let task = client
                .post(&format!("/vms/{}/backups", seg(&vm)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            // 이름을 주지 않았으면 서버가 정한 이름을 읽는다.
            let name = match name {
                Some(name) => name,
                None => {
                    let id = cell(&task["spec"]["op"]["backup"]);
                    cell(&client.get(&format!("/backups/{}", seg(&id))).await?["name"])
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
        BackupCmd::Get { backup } => print_json(
            out,
            &client.get(&format!("/backups/{}", seg(&backup))).await?,
        )?,
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{}/restore", seg(&backup)), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
        BackupCmd::Replicate {
            backup,
            remote,
            pool,
            wait,
        } => backup_replicate(client, &backup, &remote, &pool, wait, out).await?,
    }
    Ok(())
}

/// `backup replicate`: 복제 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn backup_replicate(
    client: &Client,
    backup: &str,
    remote: &str,
    pool: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(
            &format!("/backups/{}/actions/replicate", seg(backup)),
            &json!({ "remote": remote, "pool": pool }),
        )
        .await?;
    if wait {
        client.wait_task_for(&task, BACKUP_WAIT).await?;
        writeln!(
            out,
            "backup {backup} replicated to pool {pool} of remote {remote}"
        )?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "replication of backup {backup} to remote {remote} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
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
    let mut r = client.request(Method::DELETE, &format!("/backups/{}", seg(backup)));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        let task = client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
        // 원격 사본을 지우지 못했으면 성공한 Task에 경고가 남는다.
        if let Some(warning) = task["status"]["error"].as_str() {
            writeln!(out, "warning: {warning}")?;
        }
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &[
    "NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE", "ORIGIN", "REPLICAS",
];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다.
/// ORIGIN은 원격에서 받은 사본의 `<보낸 클러스터>/<원본 id 앞 8자>`, REPLICAS는 원격 사본마다
/// `<원격>:<상태>`다.
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
    let origin = &b["spec"]["origin"];
    let origin = match origin["backup"].as_str() {
        Some(id) => format!("{}/{}", cell(&origin["cluster"]), &id[..id.len().min(8)]),
        None => "-".into(),
    };
    let replicas = match status["replicas"].as_array() {
        Some(r) if !r.is_empty() => r
            .iter()
            .map(|r| format!("{}:{}", cell(&r["remote"]), cell(&r["state"])))
            .collect::<Vec<_>>()
            .join(","),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
        origin,
        replicas,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => print_json(
            out,
            &client.get(&format!("/backup-jobs/{}", seg(&job))).await?,
        )?,
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", seg(&args.job));
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client
                .delete(&format!("/backup-jobs/{}", seg(&job)))
                .await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{}/run", seg(&job))))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `oplctl import get {name}`"
            )?;
        }
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{}", seg(&import))).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(
                    &client
                        .get(&format!("/vm-imports/{}", seg(&args.import)))
                        .await?,
                )
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", seg(&args.import)), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{}", seg(&import)));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `oplctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 opLite 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
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
            client.delete(&format!("/nodes/{}", seg(&node))).await?;
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
            print_list(out, json_out, &list, VM_HEADERS, vm_row)?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{}", seg(&vm))).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{}", seg(key))).await?),
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
                .post(
                    &format!("/vms/{}/migrate", seg(&vm)),
                    &json!({ "node": node }),
                )
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{}", seg(&vm))).await?;
                    let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{}/snapshots/{}", seg(&vm), seg(&cell(&s["id"])));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{}", seg(&vm))).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client
                .post(&format!("/vms/{}/clone", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{}", seg(&vm))).await?;
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

/// `vm list`의 열
const VM_HEADERS: &[&str] = &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"];

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
            let task = client
                .post(&format!("/vms/{}/snapshots", seg(&vm)), &body)
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{}/snapshots", seg(&vm))).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current =
                client.get(&format!("/vms/{}", seg(&vm))).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{}/snapshots/{}/revert", seg(&vm), seg(&name));
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{}/snapshots/{}", seg(&vm), seg(&name)))
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
    let path = format!("/vms/{}", seg(&args.vm));
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
    let path = format!("/vms/{}", seg(vm));
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
        .send(client.request(
            Method::POST,
            // 동작은 이 파일이 정한 값이고 `stop?mode=force`처럼 쿼리를 담으므로 인코딩하지 않는다.
            &format!("/vms/{}/actions/{action}", seg(vm)),
        ))
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
    "VERSION",
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
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
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
    // 버전을 모르는 멤버(옛 버전 노드, 합류 중)는 세지 않는다.
    let mut versions: Vec<&str> = members
        .iter()
        .filter_map(|m| m["version"].as_str())
        .collect();
    versions.sort_by(|a, b| opl_types::version::compare(a, b));
    versions.dedup_by(|a, b| opl_types::version::compare(a, b).is_eq());
    if versions.len() > 1 {
        writeln!(
            out,
            "warning: nodes run different opLite versions ({}); finish the upgrade on every node",
            versions.join(", ")
        )?;
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

/// 이 CLI의 login이 만든 토큰(이름 `oplctl-…`)이면 지운다. 토큰 자신으로 읽고 지운다(실패는 무시).
/// 지웠으면 `Some(true)`, 다른 토큰(웹 UI에서 만든 것)이라 두었으면 `Some(false)`, 목록을 읽지 못했거나
/// 목록에 없으면(이미 지워짐) `None`.
async fn remove_login_token(client: &Client, id: uuid::Uuid) -> Option<bool> {
    let tokens = client.get("/tokens").await.ok()?;
    let token = tokens["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["id"].as_str() == Some(id.to_string().as_str()))?;
    let made_by_login = token["name"]
        .as_str()
        .is_some_and(|n| n.starts_with(LOGIN_TOKEN_PREFIX));
    if made_by_login {
        let _ = client.delete(&format!("/tokens/{id}")).await;
    }
    Some(made_by_login)
}

/// 웹 UI에서 만든 API 토큰으로 로그인한다: 그 토큰으로 `GET /auth/whoami`를 불러 확인하고 사용자 이름을
/// 얻는다. 토큰과 사용자 이름을 돌려준다.
async fn token_login(
    server: &str,
    file: &PathBuf,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let token = read_secret_file(file)?.trim().to_string();
    if token.is_empty() {
        bail!("the token file is empty");
    }
    let client = Client::new(server, Some(token.clone()), ca_pem)?;
    let me = client
        .get("/auth/whoami")
        .await
        .context("the API token was not accepted")?;
    let name = me["name"]
        .as_str()
        .context("server did not return the user")?
        .to_string();
    Ok((token, name))
}

/// OIDC 영역은 브라우저로만 로그인하므로 비밀번호를 묻기 전에 거절한다. 로그인 화면의
/// 영역 목록(`GET /auth/realms`)을 읽지 못하면 그냥 지나간다(서버가 로그인에서 판단한다).
async fn oidc_login_refused(anon: &Client, server: &str, realm: &str) -> anyhow::Result<()> {
    let resp = anon
        .http
        .get(anon.url("/auth/realms"))
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    let list: Value = if resp.status().is_success() {
        resp.json().await.unwrap_or_default()
    } else {
        Value::Null
    };
    let oidc = list
        .as_array()
        .into_iter()
        .flatten()
        .any(|r| r["name"] == realm && r["kind"] == "oidc");
    if oidc {
        bail!(
            "sign in to {realm} in the web UI, create an API token there and run `oplctl login --token-file`"
        );
    }
    Ok(())
}

/// `login`이 만드는 API 토큰 이름의 앞부분
const LOGIN_TOKEN_PREFIX: &str = "oplctl-";

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다. 토큰과 서버가 정한
/// 사용자 이름(영역 사용자는 `<사용자>@<영역>`)을 돌려준다.
async fn login(
    server: &str,
    args: &LoginArgs,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let anon = Client::new(server, None, ca_pem)?;
    if args.realm != opl_types::realm::LOCAL_REALM {
        oidc_login_refused(&anon, server, &args.realm).await?;
    }
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let mut body = json!({ "username": args.user, "password": password });
    if args.realm != opl_types::realm::LOCAL_REALM {
        body["realm"] = json!(args.realm);
    }
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&body)
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
    let user: Value = resp.json().await.unwrap_or_default();
    let user = user["name"].as_str().unwrap_or(&args.user).to_string();
    let name = format!(
        "{LOGIN_TOKEN_PREFIX}{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    let token = created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")?;
    Ok((token, user))
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
        assert!(matches!(
            parse(&["replicate", "web-1", "--remote", "dr", "--pool", "bk"]).unwrap(),
            BackupCmd::Replicate { backup, remote, pool, wait: false }
                if backup == "web-1" && remote == "dr" && pool == "bk"
        ));
        assert!(matches!(
            parse(&[
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .unwrap(),
            BackupCmd::Replicate { wait: true, .. }
        ));
        // 백업 풀과 복원의 새 이름·풀, 복제할 원격·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
            &["replicate", "web-1", "--pool", "bk"],
            &["replicate", "web-1", "--remote", "dr"],
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
                "3.5GiB",
                "-",
                "-"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..6], ["pending", "-", "-"]);
    }

    #[test]
    fn backup_list_shows_the_origin_and_replicas() {
        // 원격에서 받은 사본은 보낸 클러스터와 원본 id 앞 8자
        let copy = json!({
            "name": "web-20261104-030000",
            "spec": {
                "vm_name": "web", "pool": "bk",
                "origin": {
                    "cluster": "prod", "ca_fingerprint": "sha256:aa",
                    "backup": "019a4f2c-1111-7000-8000-000000000000"
                }
            },
            "status": { "phase": "receiving", "created_at": 1_793_761_200, "disks": [] }
        });
        assert_eq!(backup_row(&copy)[6..], ["prod/019a4f2c", "-"]);
        // 원격 사본마다 원격:상태
        let original = json!({
            "name": "web-20261104-030000",
            "spec": { "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200, "disks": [],
                "replicas": [
                    { "remote": "dr", "pool": "bk", "backup": "019a4f2c-1111-7000-8000-000000000000", "state": "ready", "at": 1 },
                    { "remote": "far", "pool": "bk", "state": "failed", "error": "boom", "at": 2 }
                ]
            }
        });
        assert_eq!(backup_row(&original)[6..], ["-", "dr:ready,far:failed"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        // 복제하지 않으면 `replicate`를 보내지 않는다.
        assert!(all["spec"].get("replicate").is_none(), "{all}");
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk"
            ])
            .unwrap()["spec"]["replicate"],
            json!({ "remote": "dr", "pool": "bk" })
        );
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
            // 복제 대상은 REMOTE/POOL
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "/bk",
            ],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--replicate",
                "dr/bk/x",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 복제는 대상을 주면 바꾸고 --no-replicate면 null(끔)
        assert_eq!(
            set(&["--replicate", "dr/bk"]).unwrap(),
            json!({ "replicate": { "remote": "dr", "pool": "bk" } })
        );
        assert_eq!(
            set(&["--no-replicate"]).unwrap(),
            json!({ "replicate": null })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false, 복제는 켜거나 끄거나
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
            &["--replicate", "dr/bk", "--no-replicate"],
            &["--replicate", "dr"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
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
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
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
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

        // 원격 사본을 지우지 못했으면 Task의 경고를 보인다.
        let (client, _) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": {
                    "state": "succeeded",
                    "error": "backup deleted, but remote copies may be left: dr: unreachable"
                } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup b1 deleted\nwarning: backup deleted, but remote copies may be left: dr: unreachable\n"
        );
    }

    /// `backup replicate`는 원격·풀을 본문으로 복제 Task를 만들고 따라가는 방법을 알린다. `--wait`이면
    /// Task를 기다린다.
    #[tokio::test]
    async fn backup_replicate_starts_the_task() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/backups/web-1/actions/replicate" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Backup {
                    cmd:
                        BackupCmd::Replicate {
                            backup,
                            remote,
                            pool,
                            wait,
                        },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                backup_replicate(&client, &backup, &remote, &pool, wait, &mut out)
                    .await
                    .unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk"
            ])
            .await,
            "replication of backup web-1 to remote dr started (task t1); follow it with `oplctl task get t1`\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "{seen:?}");
            assert_eq!(
                seen[0].line,
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1"
            );
            assert_eq!(seen[0].body, json!({ "remote": "dr", "pool": "bk" }));
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&[
                "oplctl",
                "backup",
                "replicate",
                "web-1",
                "--remote",
                "dr",
                "--pool",
                "bk",
                "--wait"
            ])
            .await,
            "backup web-1 replicated to pool bk of remote dr\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/backups/web-1/actions/replicate HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
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
        a["status"]["version"] = json!("0.2.0");
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
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
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

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
    #[test]
    fn cluster_status_warns_about_mixed_versions() {
        let member = |name: &str, version: Option<&str>| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": true,
                "online": true,
                "witness": false,
                "version": version,
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
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.9.0")),
            member("c", Some("0.10.0")),
            member("d", Some("0.10.0~rc1")),
            member("e", None),
        ]);
        assert!(
            out.ends_with(
                "warning: nodes run different opLite versions (0.9.0, 0.10.0~rc1, 0.10.0); \
                 finish the upgrade on every node\n"
            ),
            "{out}"
        );
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.10.0")),
            member("c", None),
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

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
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
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `oplctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    /// `node updates`는 확인 시각, 오류, 패키지 표, 잘렸는지를 보인다. `--refresh`면 다시 확인 Task를
    /// 기다린 뒤 읽는다.
    #[tokio::test]
    async fn node_updates_shows_the_check() {
        fn reply(path: &str) -> Value {
            match path {
                "/api/v1/nodes/n1/updates/refresh" => json!({ "id": "t1" }),
                "/api/v1/tasks/t1" => json!({ "id": "t1", "status": { "state": "succeeded" } }),
                "/api/v1/nodes/n1" => json!({
                    "name": "n1",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000,
                        "packages": [
                            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
                            { "name": "linux-image-6.12.48+deb13-amd64", "current": null, "candidate": "6.12.48-1" }
                        ],
                        "truncated": true,
                        "error": null
                    } }
                }),
                "/api/v1/nodes/n2" => json!({
                    "name": "n2",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000, "packages": [],
                        "error": "apt-get update -q failed (exit status: 100): E: no network"
                    } }
                }),
                "/api/v1/nodes/n3" => json!({
                    "name": "n3",
                    "status": { "updates": { "checked_at": 1_793_000_000, "packages": [], "error": null } }
                }),
                "/api/v1/nodes/n4" => json!({
                    "name": "n4",
                    "status": { "updates": { "checked_at": null, "packages": [], "error": null } }
                }),
                _ => json!({ "name": "w", "status": { "online": true } }),
            }
        }
        let (client, seen) = fake_api(reply).await;
        let run = |args: &'static [&'static str], json_out: bool| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Updates { node, refresh },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_updates(&client, &node, refresh, json_out, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        let text = run(&["oplctl", "node", "updates", "n1", "--refresh"], false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "checked: 2026-10-26 07:33 UTC\n\
             PACKAGE                          CURRENT  CANDIDATE\n\
             oplite                           0.2.0    0.2.1\n\
             linux-image-6.12.48+deb13-amd64  -        6.12.48-1\n\
             (only the first 2 packages are shown)\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/updates/refresh HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1"
            ]
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n2"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\n\
             error: apt-get update -q failed (exit status: 100): E: no network\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n3"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\nno updates available\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n4"], false)
                .await
                .unwrap(),
            "checked: never\n"
        );
        let err = run(&["oplctl", "node", "updates", "w"], false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "node w does not check for updates");
        let shown: Value = serde_json::from_str(
            &run(&["oplctl", "node", "updates", "n3"], true)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shown["packages"], json!([]));
    }

    /// `node upgrade`는 업그레이드 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다린다.
    #[tokio::test]
    async fn node_upgrade_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/upgrade" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Upgrade { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_upgrade(&client, &node, wait, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1"]).await,
            "upgrade of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/upgrade HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1", "--wait"]).await,
            "node n1 upgraded\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/upgrade HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
    #[test]
    fn settings_set_body_maps_known_keys() {
        assert_eq!(
            settings_set_body(&["updates.channel=testing".into()]).unwrap(),
            json!({ "updates": { "channel": "testing" } })
        );
        let err = settings_set_body(&["updates.mirror=x".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting updates.mirror; known: updates.channel"
        );
        let err = settings_set_body(&["updates.channel".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected KEY=VALUE, got \"updates.channel\""
        );
    }

    /// `settings get`은 spec을 `키: 값` 줄로, `-o json`이면 리소스 그대로 보인다. `set`은 PATCH한다.
    #[tokio::test]
    async fn settings_get_and_set() {
        let (client, seen) = fake_api(|_| {
            json!({
                "id": "00000000-0000-0000-0000-000000000000", "name": "cluster", "revision": 0,
                "spec": { "updates": { "channel": "stable" } }, "status": {}
            })
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "settings", "get"][..],
            &["oplctl", "settings", "set", "updates.channel=testing"][..],
        ] {
            let Cmd::Settings { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            settings(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "updates.channel: stable\nsettings updated\n"
        );

        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "get"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let mut out = Vec::new();
        settings(&client, cmd, true, &mut out).await.unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["spec"]["updates"]["channel"], "stable");

        // 모르는 키는 요청을 보내지 않는다.
        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "set", "x=1"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        assert!(
            settings(&client, cmd, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `fake_api_bodies`가 남긴 요청
    #[derive(Debug)]
    struct SeenRequest {
        line: String,
        if_match: Option<String>,
        body: Value,
    }

    /// `fake_api`와 같고 요청의 `if-match`와 JSON 본문도 남긴다. 응답에는 세션 쿠키를 붙인다.
    async fn fake_api_bodies(
        reply: fn(&str) -> Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0; 4096];
                // 머리와 content-length만큼의 본문을 다 읽는다.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_string(), body.to_string());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let if_match = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-match")
                        .then(|| v.trim().to_string())
                });
                log.lock().unwrap().push(SeenRequest {
                    line,
                    if_match,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: opl_session=s1; Path=/\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn realm_cmd(args: &[&str]) -> Result<RealmCmd, clap::Error> {
        let mut all = vec!["oplctl", "realm"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Realm { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `realm add-ldap`은 url·base DN이 필요하고, `--group`은 마지막 `=`에서 나눈다.
    #[test]
    fn parses_realm_commands() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--group",
            "cn=admins,dc=example,dc=com=admin",
            "--group",
            "cn=staff,dc=example,dc=com=Viewer",
        ])
        .unwrap() else {
            panic!("add-ldap")
        };
        assert_eq!(add.ldap.groups.len(), 2);
        assert_eq!(add.ldap.groups[0].0, "cn=admins,dc=example,dc=com");
        assert!(matches!(add.ldap.groups[0].1, Role::Admin));
        assert!(matches!(add.ldap.groups[1].1, Role::Viewer));

        assert_eq!(
            parse_group_role("cn=a,dc=x=root").unwrap_err(),
            "invalid role \"root\" in \"cn=a,dc=x=root\": use admin, operator or viewer"
        );
        assert!(parse_group_role("admins").is_err());
        assert!(parse_group_role("=admin").is_err());

        for args in [
            // url·base DN이 없다.
            &["add-ldap", "corp", "--url", "ldaps://h"][..],
            &["add-ldap", "corp", "--base-dn", "dc=x"][..],
            // 비밀번호를 주면서 지울 수는 없다.
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-password-file",
                "f",
            ][..],
            // 지우기와 설정은 함께 줄 수 없다.
            &["set-ldap", "corp", "--clear-bind-dn", "--bind-dn", "cn=x"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-dn",
                "--bind-password-file",
                "f",
            ][..],
            &["set-ldap", "corp", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &[
                "set-ldap",
                "corp",
                "--clear-groups",
                "--group",
                "cn=x=admin",
            ][..],
            // 비밀번호 파일은 사용자와 함께
            &["test", "corp", "--password-file", "f"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "corp", "--user", "alice"]).unwrap()
        else {
            panic!("test")
        };
        assert_eq!(user.as_deref(), Some("alice"));
    }

    /// `add-ldap`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_body_sends_only_what_is_given() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldap://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_body(&add, LdapFiles::default()),
            json!({
                "name": "corp",
                "backend": { "kind": "ldap", "url": "ldap://ldap.example.com", "base_dn": "dc=example,dc=com" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--bind-dn",
            "cn=svc,dc=example,dc=com",
            "--user-filter",
            "(sAMAccountName={username})",
            "--group-attr",
            "isMemberOf",
            "--timeout",
            "5",
            "--display",
            "Corp",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("svc-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_body(&add, files),
            json!({
                "name": "corp",
                "display": "Corp",
                "backend": {
                    "kind": "ldap", "url": "ldaps://ldap.example.com", "base_dn": "dc=example,dc=com",
                    "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "svc-pw",
                    "user_filter": "(sAMAccountName={username})", "group_attr": "isMemberOf",
                    "ca_pem": "PEM", "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000001", "name": "corp", "revision": 4,
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 10, "has_bind_password": true
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-ldap`은 지금 영역에 준 것만 바꾼다. 비밀번호는 주지 않으면 빼고(서버가 둔다),
    /// `--clear-bind-password`면 빈 글이다. `--group`을 주면 표 전체를 바꾼다.
    #[test]
    fn realm_set_body_changes_only_what_is_given() {
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        let kept = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 3
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        assert_eq!(
            realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap(),
            kept
        );

        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--url",
            "ldap://other.example.com",
            "--clear-bind-dn",
            "--clear-ca",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "admin",
            "--display",
            "Corporate",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        let body = realm_set_body(&with_ca, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["display"], "Corporate");
        assert_eq!(body["backend"]["url"], "ldap://other.example.com");
        // --clear-bind-dn은 저장된 비밀번호도 지운다.
        assert_eq!(body["backend"]["bind_password"], "");
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
        assert!(body["backend"].get("ca_pem").is_none(), "{body}");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                "default": "admin"
            })
        );

        // 새 비밀번호와 CA
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("new-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_body(&sample_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["bind_password"], "new-pw");
        assert_eq!(body["backend"]["ca_pem"], "PEM");

        // 그룹 표와 기본 역할 지우기
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-groups", "--clear-default-role"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert_eq!(body["roles"], json!({ "groups": [] }));

        // 서비스 계정 DN이 남는데 비밀번호만 지울 수는 없다(새 DN을 줘도).
        for args in [
            &["set-ldap", "corp", "--clear-bind-password"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-dn",
                "cn=other",
            ][..],
        ] {
            let RealmCmd::SetLdap(set) = realm_cmd(args).unwrap() else {
                unreachable!()
            };
            let e = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap_err();
            assert_eq!(
                e.to_string(),
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
        // DN이 없는 영역이나 DN도 지우면 된다.
        let mut anon = sample_realm();
        anon["backend"].as_object_mut().unwrap().remove("bind_dn");
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-bind-password"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&anon, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["backend"]["bind_password"], "");
        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--clear-bind-password",
            "--clear-bind-dn",
        ])
        .unwrap() else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
    }

    /// url·bind_dn·CA를 바꾸면서 비밀번호를 주지 않으면 서버의 400을 그대로 보인다.
    #[tokio::test]
    async fn realm_set_shows_the_server_refusal() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, sample_realm())
            } else {
                (
                    400,
                    json!({
                        "code": "invalid_request",
                        "message": "bind_password must be given again when url, bind_dn or ca_pem changes"
                    }),
                )
            }
        })
        .await;
        let e = realm(
            &client,
            realm_cmd(&["set-ldap", "corp", "--url", "ldaps://other.example.com"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("bind_password must be given again when url, bind_dn or ca_pem changes"),
            "{e}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1"
            ]
        );
    }

    #[test]
    fn realm_show_lines() {
        assert_eq!(
            realm_lines(&sample_realm()).join("\n"),
            "name: corp\n\
             display: Corp\n\
             kind: ldap\n\
             url: ldaps://ldap.example.com\n\
             base dn: dc=example,dc=com\n\
             bind dn: cn=svc,dc=example,dc=com\n\
             bind password: set\n\
             user filter: (uid={username})\n\
             group attribute: memberOf\n\
             ca: system roots\n\
             timeout: 10s\n\
             groups:\n  cn=admins,dc=example,dc=com = admin\n\
             default role: viewer"
        );
        let mut bare = sample_realm();
        bare["backend"]["has_bind_password"] = json!(false);
        bare["backend"]["ca_pem"] = json!("PEM");
        bare["roles"] = json!({ "groups": [] });
        let lines = realm_lines(&bare);
        for line in [
            "bind password: none",
            "ca: custom (PEM)",
            "groups: none",
            "default role: none",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// 영역 명령이 부르는 API: 파일의 비밀번호·CA는 본문에, `set-ldap`은 읽은 revision을
    /// If-Match로, `test`는 단계 표를 보이고 실패한 단계가 있으면 오류다.
    #[tokio::test]
    async fn realm_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_realm()] }),
            "/api/v1/realms/corp/actions/test" => json!({
                "steps": [
                    { "name": "connect", "ok": true, "message": "connected to ldaps://ldap.example.com" },
                    { "name": "service_bind", "ok": true, "message": "bound as cn=svc,dc=example,dc=com" },
                    { "name": "search", "ok": true, "message": "found uid=alice,dc=example,dc=com" },
                    { "name": "user_bind", "ok": true, "message": "the password is correct" },
                    { "name": "groups", "ok": true, "message": "2 groups" }
                ],
                "dn": "uid=alice,dc=example,dc=com",
                "groups": ["cn=admins,dc=example,dc=com", "cn=staff,dc=example,dc=com"],
                "role": "admin"
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "connect", "ok": false, "message": "connection refused" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("pw");
        std::fs::write(&secret, "svc-pw\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-ldap",
                "corp",
                "--url",
                "ldaps://ldap.example.com",
                "--base-dn",
                "dc=example,dc=com",
                "--bind-dn",
                "cn=svc,dc=example,dc=com",
                "--bind-password-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-ldap", "corp", "--bind-password-file", secret][..],
            &["remove", "corp"][..],
            &["test", "corp", "--user", "alice", "--password-file", secret][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken", "--user", "a", "--password-file", secret]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the connect step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1",
                "DELETE /api/v1/realms/corp HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "POST /api/v1/realms/corp/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["bind_password"], "svc-pw");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("4"));
        assert_eq!(seen[3].body["backend"]["bind_password"], "svc-pw");
        assert!(seen[3].body["backend"].get("has_bind_password").is_none());
        assert_eq!(
            seen[6].body,
            json!({ "username": "alice", "password": "svc-pw" })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             corp  Corp     ldap  ldaps://ldap.example.com\n\
             realm corp created\n\
             realm corp updated\n\
             realm corp removed\n\
             STEP          RESULT  MESSAGE\n\
             connect       ok      connected to ldaps://ldap.example.com\n\
             service_bind  ok      bound as cn=svc,dc=example,dc=com\n\
             search        ok      found uid=alice,dc=example,dc=com\n\
             user_bind     ok      the password is correct\n\
             groups        ok      2 groups\n\
             dn: uid=alice,dc=example,dc=com\n\
             groups: cn=admins,dc=example,dc=com, cn=staff,dc=example,dc=com\n\
             role: admin\n"
        );
    }

    /// `login --realm`은 로그인 본문에 영역을 넣고(local이면 넣지 않음) 서버가 정한 이름을 돌려준다.
    #[tokio::test]
    async fn login_sends_the_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/login" => json!({ "name": "alice@corp", "role": "admin" }),
            "/api/v1/tokens" => json!({ "secret": "opl_t_s" }),
            _ => json!({}),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "alice-pw\n").unwrap();
        let pw = pw.to_str().unwrap();
        for (realm, expected) in [(Some("corp"), Some("corp")), (None, None)] {
            let mut args = vec!["oplctl", "login", "--user", "Alice", "--password-file", pw];
            if let Some(realm) = realm {
                args.extend(["--realm", realm]);
            }
            let Cmd::Login(args) = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            assert_eq!(
                login(&base, &args, None).await.unwrap(),
                ("opl_t_s".to_string(), "alice@corp".to_string())
            );
            let seen = seen.lock().unwrap();
            let sent = &seen[seen.len() - 3];
            assert_eq!(sent.line, "POST /api/v1/auth/login HTTP/1.1");
            assert_eq!(sent.body["username"], "Alice");
            assert_eq!(sent.body["password"], "alice-pw");
            assert_eq!(sent.body.get("realm").and_then(Value::as_str), expected);
        }
    }

    /// `realm add-oidc`는 issuer·client ID가 필요하고, 지우기와 설정은 함께 줄 수 없다.
    /// `test`는 사용자 없이도 된다(OIDC).
    #[test]
    fn parses_oidc_realm_commands() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--group",
            "opl-admins=admin",
        ])
        .unwrap() else {
            panic!("add-oidc")
        };
        assert_eq!(add.oidc.scopes, ["openid", "groups"]);
        assert_eq!(add.oidc.groups[0].0, "opl-admins");
        assert!(matches!(add.oidc.groups[0].1, Role::Admin));

        for args in [
            &["add-oidc", "okta", "--issuer", "https://idp"][..],
            &["add-oidc", "okta", "--client-id", "oplite"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-client-secret",
                "--client-secret-file",
                "f",
            ][..],
            &["set-oidc", "okta", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-oidc",
                "okta",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &["set-oidc", "okta", "--clear-groups", "--group", "g=admin"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "okta"]).unwrap() else {
            panic!("test")
        };
        assert_eq!(user, None);
    }

    /// `add-oidc`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_oidc_body_sends_only_what_is_given() {
        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com",
            "--client-id",
            "oplite",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_oidc_body(&add, OidcFiles::default()),
            json!({
                "name": "okta",
                "backend": { "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddOidc(add) = realm_cmd(&[
            "add-oidc",
            "okta",
            "--issuer",
            "https://idp.example.com/oauth2/default",
            "--client-id",
            "oplite",
            "--scope",
            "openid",
            "--scope",
            "groups",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--timeout",
            "5",
            "--display",
            "Okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("s3cret".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_oidc_body(&add, files),
            json!({
                "name": "okta",
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com/oauth2/default",
                    "client_id": "oplite", "client_secret": "s3cret", "scopes": ["openid", "groups"],
                    "username_claim": "email", "groups_claim": "roles", "ca_pem": "PEM",
                    "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "opl-ops", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_oidc_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000002", "name": "okta", "revision": 9,
            "display": "Okta",
            "backend": {
                "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                "groups_claim": "groups", "timeout_secs": 10, "has_client_secret": true
            },
            "roles": {
                "groups": [{ "group": "opl-admins", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-oidc`는 지금 영역에 준 것만 바꾼다. 비밀은 주지 않으면 빼고(서버가 둔다),
    /// `--clear-client-secret`면 빈 글이다. 다른 종류의 영역은 바꾸지 않는다.
    #[test]
    fn realm_set_oidc_body_changes_only_what_is_given() {
        let RealmCmd::SetOidc(set) = realm_cmd(&["set-oidc", "okta", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            realm_set_oidc_body(&sample_oidc_realm(), &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Okta",
                "backend": {
                    "kind": "oidc", "issuer": "https://idp.example.com", "client_id": "oplite",
                    "scopes": ["openid", "profile", "email"], "username_claim": "preferred_username",
                    "groups_claim": "groups", "timeout_secs": 3
                },
                "roles": {
                    "groups": [{ "group": "opl-admins", "role": "admin" }],
                    "default": "viewer"
                }
            })
        );

        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--issuer",
            "https://other.example.com",
            "--client-id",
            "opl2",
            "--scope",
            "openid",
            "--username-claim",
            "email",
            "--groups-claim",
            "roles",
            "--clear-client-secret",
            "--clear-ca",
            "--clear-groups",
            "--clear-default-role",
            "--display",
            "Other",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_oidc_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        assert_eq!(
            realm_set_oidc_body(&with_ca, &set, OidcFiles::default()).unwrap(),
            json!({
                "display": "Other",
                "backend": {
                    "kind": "oidc", "issuer": "https://other.example.com", "client_id": "opl2",
                    "client_secret": "", "scopes": ["openid"], "username_claim": "email",
                    "groups_claim": "roles", "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );

        // 새 비밀·CA와 그룹 표
        let RealmCmd::SetOidc(set) = realm_cmd(&[
            "set-oidc",
            "okta",
            "--group",
            "opl-ops=operator",
            "--default-role",
            "admin",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = OidcFiles {
            client_secret: Some("new-secret".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_oidc_body(&sample_oidc_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["client_secret"], "new-secret");
        assert_eq!(body["backend"]["ca_pem"], "PEM");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "opl-ops", "role": "operator" }],
                "default": "admin"
            })
        );

        // 종류가 다른 영역
        let e = realm_set_oidc_body(&sample_realm(), &set, OidcFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm okta is not an OIDC realm (its kind is ldap)"
        );
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let e = realm_set_body(&sample_oidc_realm(), &set, LdapFiles::default()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "realm corp is not an LDAP realm (its kind is oidc)"
        );
    }

    #[test]
    fn realm_show_lines_for_oidc() {
        assert_eq!(
            realm_lines(&sample_oidc_realm()).join("\n"),
            "name: okta\n\
             display: Okta\n\
             kind: oidc\n\
             issuer: https://idp.example.com\n\
             client id: oplite\n\
             client secret: set\n\
             scopes: openid profile email\n\
             username claim: preferred_username\n\
             groups claim: groups\n\
             ca: system roots\n\
             timeout: 10s\n\
             callback path: /api/v1/auth/oidc/callback\n\
             groups:\n  opl-admins = admin\n\
             default role: viewer"
        );
        let mut public = sample_oidc_realm();
        public["backend"]["has_client_secret"] = json!(false);
        public["backend"]["ca_pem"] = json!("PEM");
        let lines = realm_lines(&public);
        for line in ["client secret: none", "ca: custom (PEM)"] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// OIDC 영역 명령이 부르는 API: 목록의 URL 칸은 issuer, 파일의 비밀·CA는 본문에,
    /// `set-oidc`는 읽은 revision을 If-Match로, `test`는 영역 종류를 읽고 입력 없이 단계 표만 보인다
    /// (`--user`를 줘도 비밀번호를 묻지 않고 DN·그룹·역할 줄이 없다).
    #[tokio::test]
    async fn realm_oidc_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_oidc_realm()] }),
            "/api/v1/realms/okta/actions/test" => json!({
                "steps": [
                    { "name": "discovery", "ok": true, "message": "read https://idp.example.com/.well-known/openid-configuration" },
                    { "name": "jwks", "ok": true, "message": "2 signing keys (RS256 1, ES256 1)" }
                ],
                "dn": null, "groups": [], "role": null
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "discovery", "ok": false, "message": "issuer differs" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_oidc_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-oidc",
                "okta",
                "--issuer",
                "https://idp.example.com",
                "--client-id",
                "oplite",
                "--client-secret-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-oidc", "okta", "--client-secret-file", secret][..],
            &["test", "okta", "--user", "alice"][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the discovery step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "PUT /api/v1/realms/okta HTTP/1.1",
                "GET /api/v1/realms/okta HTTP/1.1",
                "POST /api/v1/realms/okta/actions/test HTTP/1.1",
                "GET /api/v1/realms/broken HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["kind"], "oidc");
        assert_eq!(seen[1].body["backend"]["client_secret"], "s3cret");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("9"));
        assert_eq!(seen[3].body["backend"]["client_secret"], "s3cret");
        assert!(seen[3].body["backend"].get("has_client_secret").is_none());
        assert_eq!(seen[5].body, json!({}));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             okta  Okta     oidc  https://idp.example.com\n\
             realm okta created\n\
             realm okta updated\n\
             STEP       RESULT  MESSAGE\n\
             discovery  ok      read https://idp.example.com/.well-known/openid-configuration\n\
             jwks       ok      2 signing keys (RS256 1, ES256 1)\n"
        );
    }

    /// `login --realm <OIDC 영역>`은 비밀번호를 묻기 전에 웹 UI로 보낸다.
    #[tokio::test]
    async fn login_refuses_an_oidc_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/realms" => json!([
                { "name": "local", "display": null, "kind": "local" },
                { "name": "corp", "display": null, "kind": "ldap" },
                { "name": "okta", "display": "Okta", "kind": "oidc" }
            ]),
            _ => json!({}),
        })
        .await;
        // 비밀번호 파일이 없으므로 묻는다면 터미널이 없어 다른 오류가 난다.
        let Cmd::Login(args) = Cli::try_parse_from(["oplctl", "login", "--realm", "okta"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let e = login(&base, &args, None).await.unwrap_err();
        assert_eq!(
            e.to_string(),
            "sign in to okta in the web UI, create an API token there and run `oplctl login --token-file`"
        );
        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(lines, ["GET /api/v1/auth/realms HTTP/1.1"]);
    }

    /// `login --token-file`은 비밀번호 로그인 인자와 함께 줄 수 없다.
    #[test]
    fn token_file_login_conflicts_with_password_login() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["oplctl", "login", "--token-file", "t"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args).map(|_| ())
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--ca-fingerprint", "sha256:ab"]).is_ok());
        for extra in [
            &["--user", "bob"][..],
            &["--realm", "okta"][..],
            &["--password-file", "pw"][..],
        ] {
            let e = parse(extra).unwrap_err();
            assert_eq!(
                e.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{extra:?}"
            );
        }
    }

    /// `login --token-file`: 토큰으로 whoami를 불러 확인하고 저장한다. 같은 토큰을 다시 넣으면 지우지 않고,
    /// 이전 토큰은 login이 만든 것(`oplctl-…`)만 지운다. 받지 않는 토큰은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_checks_and_saves_the_token() {
        let ids = [
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
        ];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[2] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[2] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/auth/whoami" => (
                200,
                json!({ "name": "alice@corp.com@okta", "role": "admin" }),
            ),
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        let login_with = async |token: &str| {
            std::fs::write(&file, format!("{token}\n")).unwrap();
            let mut out = Vec::new();
            let args = [
                "oplctl",
                "--config",
                config.to_str().unwrap(),
                "--server",
                client.base.as_str(),
                "login",
                "--token-file",
                file.to_str().unwrap(),
            ];
            run(args, &mut out)
                .await
                .map(|()| String::from_utf8(out).unwrap())
        };
        let saved = || CliConfig::load(&config).unwrap().token;
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)
        assert_eq!(
            login_with(&tokens[0]).await.unwrap(),
            format!("logged in to {} as alice@corp.com@okta\n", client.base)
        );
        assert_eq!(saved().as_deref(), Some(tokens[0].as_str()));
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 같은 토큰을 다시 넣어도 지우지 않는다.
        login_with(&tokens[0]).await.unwrap();
        assert_eq!(lines(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
        // 다른 토큰: 이전 토큰(laptop)은 login이 만든 것이 아니므로 두다.
        login_with(&tokens[1]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1",
                "GET /api/v1/tokens HTTP/1.1"
            ]
        );
        // 이전 토큰(oplctl-…)은 지운다.
        login_with(&tokens[2]).await.unwrap();
        assert_eq!(
            lines(),
            [
                "GET /api/v1/auth/whoami HTTP/1.1".to_string(),
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
        assert_eq!(saved().as_deref(), Some(tokens[2].as_str()));
    }

    /// 서버가 받지 않는 토큰(401)은 저장하지 않는다.
    #[tokio::test]
    async fn token_file_login_does_not_save_a_refused_token() {
        let (client, seen) = fake_api_status(|_| {
            (
                401,
                json!({ "code": "unauthorized", "message": "invalid token" }),
            )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let file = dir.path().join("token");
        std::fs::write(&file, "opl_bad_token\n").unwrap();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "login",
            "--token-file",
            file.to_str().unwrap(),
        ];
        let e = run(args, &mut Vec::new()).await.unwrap_err();
        assert_eq!(e.to_string(), "the API token was not accepted");
        assert!(!config.exists());
        assert_eq!(*seen.lock().unwrap(), ["GET /api/v1/auth/whoami HTTP/1.1"]);
    }

    /// 시험 서버 쪽에서 경로 조각의 퍼센트 인코딩을 푼다.
    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    /// `vm stop --force`는 동작 뒤의 쿼리를 그대로 보낸다(VM 이름만 인코딩).
    #[tokio::test]
    async fn vm_stop_force_keeps_the_query() {
        let (client, seen) =
            fake_api(|_| json!({ "id": "t1", "status": { "state": "succeeded" } })).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "vm",
            "stop",
            "web 1",
            "--force",
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "vm web 1: stop done\n");
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines[0],
            "POST /api/v1/vms/web%201/actions/stop?mode=force HTTP/1.1"
        );
    }

    /// 이름을 경로에 넣을 때 퍼센트 인코딩해, `+`·`@`가 든 OIDC 사용자 이름도 서버에서 같은 이름으로 풀린다.
    #[tokio::test]
    async fn user_delete_encodes_the_name_in_the_path() {
        let (client, seen) = fake_api(|_| json!({})).await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let name = "alice+ops@corp.com@okta";
        let mut out = Vec::new();
        let args = [
            "oplctl",
            "--config",
            config.to_str().unwrap(),
            "--server",
            client.base.as_str(),
            "user",
            "delete",
            name,
        ];
        run(args, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("user {name} deleted\n")
        );
        let lines = seen.lock().unwrap().clone();
        assert_eq!(
            lines,
            ["DELETE /api/v1/users/alice%2Bops%40corp.com%40okta HTTP/1.1"]
        );
        let path = lines[0].split(' ').nth(1).unwrap();
        assert_eq!(
            percent_decode(path.strip_prefix("/api/v1/users/").unwrap()),
            name
        );
    }

    /// `logout`은 login이 만든 토큰(`oplctl-…`)만 서버에서 지운다. 웹 UI에서 만든 토큰은 로컬 설정에서만
    /// 지우고 서버에서는 그대로라고 알린다.
    #[tokio::test]
    async fn logout_deletes_only_tokens_made_by_login() {
        let ids = [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()];
        let tokens = ids.map(|id| format!("opl_{}_secret", id.simple()));
        let listed = json!({ "revision": 1, "items": [
            { "id": ids[0], "name": "laptop", "revision": 1, "user": ids[1] },
            { "id": ids[1], "name": "oplctl-1a2b3c4d", "revision": 1, "user": ids[1] },
        ]});
        let (client, seen) = fake_api_status(move |path| match path {
            "/api/v1/tokens" => (200, listed.clone()),
            _ => (200, json!({})),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("oplctl.json");
        let logout_with = async |token: &str| {
            CliConfig {
                server: Some(client.base.clone()),
                token: Some(token.into()),
                ca_pem: None,
            }
            .save(&config)
            .unwrap();
            let mut out = Vec::new();
            let args = ["oplctl", "--config", config.to_str().unwrap(), "logout"];
            run(args, &mut out).await.unwrap();
            assert_eq!(CliConfig::load(&config).unwrap().token, None);
            String::from_utf8(out).unwrap()
        };
        let lines = || std::mem::take(&mut *seen.lock().unwrap());

        // 웹 UI에서 만든 토큰(laptop)은 서버에서 지우지 않는다.
        assert_eq!(
            logout_with(&tokens[0]).await,
            "logged out\nthe token stays valid on the server; delete it in the web UI\n"
        );
        assert_eq!(lines(), ["GET /api/v1/tokens HTTP/1.1"]);
        // login이 만든 토큰은 지운다.
        assert_eq!(logout_with(&tokens[1]).await, "logged out\n");
        assert_eq!(
            lines(),
            [
                "GET /api/v1/tokens HTTP/1.1".to_string(),
                format!("DELETE /api/v1/tokens/{} HTTP/1.1", ids[1]),
            ]
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    fn remote_cmd(args: &[&str]) -> Result<RemoteCmd, clap::Error> {
        let mut all = vec!["oplctl", "remote"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Remote { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `remote add`는 주소와 토큰 파일이 필요하다. `remote vm`의 stop은 로컬 `vm stop`처럼 ACPI이고
    /// `--force`면 곧바로 끈다.
    #[test]
    fn parses_remote_commands() {
        let RemoteCmd::Add(add) = remote_cmd(&[
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
            "--yes",
        ])
        .unwrap() else {
            panic!("not add");
        };
        assert_eq!(add.url, "https://10.0.0.5:8443");
        assert_eq!(add.token_file, PathBuf::from("-"));
        assert!(add.yes && add.ca_fingerprint.is_none());
        assert!(remote_cmd(&["add", "dr", "--url", "https://h"]).is_err());
        assert!(remote_cmd(&["add", "dr", "--token-file", "t"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "reboot", "web"]).is_err());
        assert!(remote_cmd(&["vm", "dr", "shutdown", "web"]).is_err());
        let actions: Vec<_> = [
            &["start"][..],
            &["stop"][..],
            &["stop", "--force"][..],
            &["start", "--force"][..],
        ]
        .iter()
        .map(|a| {
            let mut args = vec!["vm", "dr"];
            args.extend_from_slice(&a[..1]);
            args.push("web");
            args.extend_from_slice(&a[1..]);
            match remote_cmd(&args).unwrap() {
                RemoteCmd::Vm { action, force, .. } => {
                    remote_vm_action(action, force).map_err(|e| e.to_string())
                }
                _ => unreachable!(),
            }
        })
        .collect();
        assert_eq!(
            actions,
            [
                Ok(("start", "start")),
                Ok(("stop", "stop")),
                Ok(("stop?mode=force", "force stop")),
                Err("--force is only for stop".to_string()),
            ]
        );
    }

    /// 토큰을 표준 입력으로 받으면 지문 확인을 물을 수 없으므로 `--ca-fingerprint`나 `--yes`가 없으면 곧바로
    /// 실패한다(표준 입력을 읽거나 원격에 연결하지 않고).
    #[tokio::test]
    async fn remote_add_from_stdin_needs_a_fingerprint_or_yes() {
        let client = Client::new("http://127.0.0.1:1", None, None).unwrap();
        let args = [
            "add",
            "dr",
            "--url",
            "https://10.0.0.5:8443",
            "--token-file",
            "-",
        ];
        let err = remote(&client, remote_cmd(&args).unwrap(), false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "reading the token from stdin needs --ca-fingerprint or --yes"
        );
    }

    /// 원격이 저장한 토큰을 거절하면 중계가 502 `remote_unauthorized`를 주므로 그 메시지를 보이고, 이
    /// 클러스터에 다시 로그인하라고 하지 않는다.
    #[tokio::test]
    async fn a_refused_remote_token_is_not_a_login_problem() {
        let message = "the remote cluster refused the stored token; update it with `oplctl remote set --token-file`";
        let (client, _) = fake_api_status(move |_| {
            (
                502,
                json!({ "code": "remote_unauthorized", "message": message }),
            )
        })
        .await;
        let err = remote(
            &client,
            remote_cmd(&["vms", "dr"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert_eq!(err, format!("{message} (remote_unauthorized)"));
        assert!(!err.contains("oplctl login"), "{err}");
    }

    /// 받은 CA 지문은 보이고, `--yes`가 아니면 터미널에서 묻는다. 터미널이 아니면 지문을 달라고 한다.
    #[test]
    fn remote_fingerprint_needs_a_confirmation() {
        let fp = "sha256:ab";
        let confirm = |yes: bool, terminal: bool, answer: &str| {
            let mut out = Vec::new();
            let r = confirm_fingerprint(fp, yes, terminal, &mut answer.as_bytes(), &mut out);
            (
                r.map_err(|e| e.to_string()),
                String::from_utf8(out).unwrap(),
            )
        };
        let shown = "the remote cluster CA fingerprint is sha256:ab\n";
        assert_eq!(confirm(true, false, ""), (Ok(()), shown.to_string()));
        let (r, out) = confirm(false, false, "y\n");
        assert_eq!(out, shown);
        assert!(
            r.unwrap_err()
                .contains("run again with --ca-fingerprint sha256:ab"),
            "not a terminal"
        );
        let prompt = format!("{shown}Is this the CA of the remote cluster? [y/N] ");
        assert_eq!(confirm(false, true, "Yes\n"), (Ok(()), prompt.clone()));
        assert_eq!(
            confirm(false, true, "\n"),
            (Err("cancelled".to_string()), prompt)
        );
    }

    fn sample_remote() -> Value {
        json!({
            "id": "0190b9d4-0000-7000-8000-000000000001",
            "name": "dr",
            "revision": 9,
            "url": "https://10.0.0.5:8443",
            "ca_fingerprint": "sha256:aa",
            "token_id": "0190b9d4-0000-7000-8000-000000000002",
            "status": {
                "reachable": true,
                "checked_at": 1_793_000_000,
                "error": null,
                "cluster_name": "dr-site",
                "version": "0.37.0",
                "nodes": { "total": 3, "online": 2 },
                "vms": { "total": 5, "running": 4 },
                "cpus": 48,
                "memory_gib": 192
            }
        })
    }

    /// 목록의 노드·VM 칸은 `online/total`·`running/total`이고, 닿지 않는 원격은 이유를 보인다.
    #[test]
    fn remote_rows_and_lines() {
        assert_eq!(
            remote_row(&sample_remote()),
            ["dr", "dr-site", "0.37.0", "2/3", "4/5", "reachable"]
        );
        let down = json!({
            "name": "old",
            "status": { "reachable": false, "error": "connection refused" }
        });
        assert_eq!(
            remote_row(&down),
            ["old", "-", "-", "-", "-", "unreachable: connection refused"]
        );
        let lines = remote_lines(&sample_remote());
        for line in [
            "url: https://10.0.0.5:8443",
            "ca fingerprint: sha256:aa",
            "token id: 0190b9d4-0000-7000-8000-000000000002",
            "status: reachable",
            "nodes online: 2/3",
            "vms running: 4/5",
            "cpus: 48",
            "memory: 192 GiB",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
        assert!(
            remote_lines(&down).contains(&"changed at: never".to_string()),
            "{:?}",
            remote_lines(&down)
        );
    }

    /// 원격 명령이 부르는 API: `add`는 토큰 파일과 준 지문을, `set`은 읽은 revision을 If-Match로 하고
    /// 토큰을 주지 않으면 빼서 보낸다. `vms`·`vm`은 중계 경로로 원격 API를 부른다.
    #[tokio::test]
    async fn remote_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes/dr/api/vms" => json!({
                "revision": 3,
                "items": [{
                    "name": "web",
                    "spec": { "power": "running", "ha": false },
                    "status": { "phase": "running", "node": "n1", "pid": 42 }
                }]
            }),
            "/api/v1/remotes/dr/api/vms/web/actions/stop?mode=force" => {
                json!({ "id": "t1", "status": { "state": "pending" } })
            }
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        let secret = format!("opl_{}_s3cret", uuid::Uuid::now_v7().simple());
        std::fs::write(&token, format!("{secret}\n")).unwrap();
        let token = token.to_str().unwrap();

        let mut out = Vec::new();
        for args in [
            &[
                "add",
                "dr",
                "--url",
                "https://10.0.0.5:8443",
                "--ca-fingerprint",
                "sha256:aa",
                "--token-file",
                token,
            ][..],
            &["set", "dr", "--url", "https://10.0.0.6:8443"][..],
            &["set", "dr", "--token-file", token][..],
            &["remove", "dr"][..],
            &["vms", "dr"][..],
            &["vm", "dr", "stop", "web", "--force"][..],
        ] {
            remote(&client, remote_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "POST /api/v1/remotes HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr HTTP/1.1",
                "PUT /api/v1/remotes/dr HTTP/1.1",
                "DELETE /api/v1/remotes/dr HTTP/1.1",
                "GET /api/v1/remotes/dr/api/vms HTTP/1.1",
                "POST /api/v1/remotes/dr/api/vms/web/actions/stop?mode=force HTTP/1.1",
            ]
        );
        assert_eq!(
            seen[0].body,
            json!({
                "name": "dr",
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(seen[2].if_match.as_deref(), Some("9"));
        assert_eq!(
            seen[2].body,
            json!({ "url": "https://10.0.0.6:8443", "ca_fingerprint": "sha256:aa" })
        );
        assert_eq!(
            seen[4].body,
            json!({
                "url": "https://10.0.0.5:8443",
                "ca_fingerprint": "sha256:aa",
                "token": secret,
            })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "remote dr added (reachable)\n\
             remote dr updated\n\
             remote dr updated\n\
             remote dr removed\n\
             NAME  POWER    PHASE    NODE  HA     PID  ERROR\n\
             web   running  running  n1    false  42   -\n\
             vm web on remote dr: force stop requested (task t1)\n"
        );
    }

    /// `remote list`는 원격마다 한 줄, `show`·`refresh`는 `키: 값` 줄이다.
    #[tokio::test]
    async fn remote_list_and_refresh() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/remotes" => json!({ "revision": 9, "items": [sample_remote()] }),
            _ => sample_remote(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        remote(&client, remote_cmd(&["list"]).unwrap(), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  CLUSTER  VERSION  NODES  VMS  STATUS\n\
             dr    dr-site  0.37.0   2/3    4/5  reachable\n"
        );
        let mut out = Vec::new();
        remote(
            &client,
            remote_cmd(&["refresh", "dr"]).unwrap(),
            false,
            &mut out,
        )
        .await
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            remote_lines(&sample_remote()).join("\n") + "\n"
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.line.clone())
            .collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/remotes HTTP/1.1",
                "POST /api/v1/remotes/dr/actions/refresh HTTP/1.1",
            ]
        );
    }
}
```

`crates/opld/tests/replicate.rs`를 만든다:

```rust
//! 계획 38: 한 노드짜리 opld 클러스터 둘. prod에 dr을 원격으로 등록하고(dr의 operator 토큰) prod의 꺼진
//! VM 백업을 dr의 백업 풀로 복제한다. dr의 사본은 보통 백업이라 dr에서 복원하면 디스크 내용이 같고, prod에서
//! 원본을 지우면 dr의 사본도 지워진다. 백업과 복원이 `qemu-img`를 쓰므로 `qemu-it` 뒤에 둔다(VM은 켜지
//! 않는다).
//! `cargo test -p opld --features qemu-it --test replicate`
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Daemon, eventually};
use serde_json::{Value, json};

/// 디스크 앞에 써 두는 표식의 길이
const MARK: usize = 64 * 1024;

/// 클러스터 `cluster`를 만들고 로그인해 VM 디스크 풀 `local`과 백업 풀 `bk`(dir)를 만든다.
async fn setup(d: &mut Daemon, cluster: &str) {
    d.start().await;
    d.init(cluster).await;
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

/// 풀 `local`에 있는 볼륨 `name`의 파일
async fn volume_file(d: &Daemon, name: &str) -> PathBuf {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    d.path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()))
}

/// 이미지의 앞 `MARK` 바이트. raw로 빼서 읽는다.
fn head(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("head.raw");
    let out = Command::new("qemu-img")
        .args(["convert", "-O", "raw"])
        .arg(file)
        .arg(&raw)
        .output()
        .expect("run qemu-img");
    assert!(out.status.success(), "{out:?}");
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes[..MARK].to_vec()
}

/// dr에 operator 사용자 `repl`을 만들고 그 사용자로 로그인해 만든 API 토큰을 파일에 써 그 경로를 돌려준다.
async fn operator_token(dr: &Daemon, prod: &Daemon) -> String {
    let password = dr.path("repl-password");
    std::fs::write(&password, "repl-password-1").unwrap();
    let password = password.to_str().unwrap();
    dr.ctl(&[
        "user",
        "create",
        "repl",
        "--role",
        "operator",
        "--password-file",
        password,
    ])
    .await
    .unwrap();
    // admin의 로그인 정보(`oplctl.json`)와 따로 둔다.
    let config = dr.path("repl.json");
    let config = config.to_str().unwrap();
    let server = dr.server();
    ctl_with(
        config,
        &[
            "login",
            "--server",
            &server,
            "--user",
            "repl",
            "--password-file",
            password,
            "--ca-fingerprint",
            dr.ca_fingerprint.as_deref().unwrap(),
        ],
    )
    .await;
    let token = ctl_with(config, &["token", "create", "prod"]).await;
    let file = prod.path("dr-token");
    std::fs::write(&file, token).unwrap();
    file.to_str().unwrap().to_string()
}

/// 로그인 정보 파일 `config`로 oplctl 명령을 실행한다.
async fn ctl_with(config: &str, args: &[&str]) -> String {
    let mut argv = vec!["oplctl", "--config", config];
    argv.extend_from_slice(args);
    let mut out = Vec::new();
    oplctl::run(argv, &mut out).await.unwrap();
    String::from_utf8(out).unwrap()
}

#[tokio::test]
async fn a_backup_is_replicated_restored_on_the_remote_and_deleted_with_the_original() {
    let mut prod = Daemon::new("node-a", "");
    let mut dr = Daemon::new("node-b", "");
    setup(&mut prod, "prod").await;
    setup(&mut dr, "dr").await;

    // prod에 dr을 원격으로 등록한다. dr 쪽 토큰 주인은 operator(백업 쓰기)다.
    let token_file = operator_token(&dr, &prod).await;
    let url = dr.server();
    prod.ctl(&[
        "remote",
        "add",
        "dr",
        "--url",
        &url,
        "--ca-fingerprint",
        dr.ca_fingerprint.as_deref().unwrap(),
        "--token-file",
        &token_file,
    ])
    .await
    .unwrap();

    // prod의 꺼진 VM(raw 디스크에 표식)을 백업한다.
    prod.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M", "--format", "raw",
    ])
    .await
    .unwrap();
    {
        use std::io::Write;
        let file = volume_file(&prod, "root").await;
        let mut f = std::fs::OpenOptions::new().write(true).open(file).unwrap();
        f.write_all(&[0xab; MARK]).unwrap();
    }
    prod.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();
    prod.ctl(&[
        "backup", "create", "web", "--pool", "bk", "--name", "manual",
    ])
    .await
    .unwrap();

    // dr의 백업 풀 bk로 복제한다.
    let out = prod
        .ctl(&[
            "backup",
            "replicate",
            "manual",
            "--remote",
            "dr",
            "--pool",
            "bk",
            "--wait",
        ])
        .await
        .unwrap();
    assert_eq!(out, "backup manual replicated to pool bk of remote dr\n");
    let original = prod.ctl_json(&["backup", "get", "manual"]).await;
    let replica = &original["status"]["replicas"][0];
    assert_eq!(replica["remote"], "dr", "{original}");
    assert_eq!(replica["pool"], "bk", "{original}");
    assert_eq!(replica["state"], "ready", "{original}");
    let list = prod.ctl(&["backup", "list"]).await.unwrap();
    let row: Vec<&str> = list.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(row.last(), Some(&"dr:ready"), "{list}");

    // dr에는 같은 이름의 ready 사본이 있고 출처가 prod의 원본이다.
    let copies = dr.ctl_json(&["backup", "list"]).await;
    let copies = copies.as_array().unwrap();
    assert_eq!(copies.len(), 1, "{copies:?}");
    let copy = &copies[0];
    assert_eq!(copy["id"], replica["backup"], "{copy}");
    assert_eq!(copy["name"], "manual", "{copy}");
    assert_eq!(copy["status"]["phase"], "ready", "{copy}");
    assert_eq!(copy["spec"]["vm_name"], "web", "{copy}");
    let origin = &copy["spec"]["origin"];
    assert_eq!(origin["cluster"], "prod", "{copy}");
    assert_eq!(origin["backup"], original["id"], "{copy}");
    assert_eq!(
        origin["ca_fingerprint"].as_str(),
        prod.ca_fingerprint.as_deref(),
        "{copy}"
    );
    let list = dr.ctl(&["backup", "list"]).await.unwrap();
    let id = original["id"].as_str().unwrap();
    assert!(
        list.lines()
            .nth(1)
            .unwrap()
            .contains(&format!("prod/{}", &id[..8])),
        "{list}"
    );

    // dr에서 사본을 복원하면 디스크 내용이 같다.
    let out = dr
        .ctl(&[
            "backup", "restore", "manual", "--name", "web", "--pool", "local",
        ])
        .await
        .unwrap();
    assert_eq!(out, "vm web restored from backup manual\n");
    let restored = volume_file(&dr, "web-disk0").await;
    assert_eq!(head(&dr, &restored), vec![0xab; MARK]);

    // prod에서 원본을 지우면 dr의 사본도 지워진다.
    let out = prod.ctl(&["backup", "delete", "manual"]).await.unwrap();
    assert_eq!(out, "backup manual deleted\n");
    eventually("the remote copy is deleted", || async {
        dr.ctl_json(&["backup", "list"]).await == json!([])
    })
    .await;
    let copy_dir = dr
        .path("bk")
        .join("backups")
        .join(copy["id"].as_str().unwrap());
    assert!(!copy_dir.exists(), "remote copy files left");
    assert_eq!(
        prod.ctl_json(&["backup", "list"]).await,
        Value::Array(vec![])
    );

    prod.stop();
    dr.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1518개, 실패 0

Run: `cargo test -p opld --features qemu-it --test replicate`
Expected: PASS 1개 (qemu-img가 있어야 한다)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): 백업 복제 명령과 opld E2E — oplctl backup replicate <백업> --remote <r> --pool <p> [--wait](POST /backups/{key}/actions/replicate, 기다리지 않으면 Task id와 따라가는 방법, --wait이면 BACKUP_WAIT만큼 기다림), backup list에 ORIGIN(받은 사본이면 <보낸 클러스터>/<원본 id 앞 8자>)·REPLICAS(<원격>:<상태> 쉼표) 열(backup get은 JSON에 origin·replicas가 그대로 보임), backup-job create --replicate <원격>/<풀>(주지 않으면 replicate를 보내지 않음), backup-job set --replicate <원격>/<풀> | --no-replicate({\"replicate\": null}), backup delete가 Task에 남은 경고(원격 사본을 지우지 못함)를 warning: 줄로 보임. 시험: 명령 파싱, 목록 행의 출처·사본, 작업 본문의 replicate와 잘못된 REMOTE/POOL, fake_api로 replicate 요청 본문·출력·--wait, 지우기 경고. opld tests/replicate.rs(qemu-it — 백업·복원이 qemu-img를 씀, VM은 켜지 않음): 한 노드 클러스터 둘, dr의 operator 사용자 토큰으로 prod에 dr 원격 등록, prod의 꺼진 VM(raw 디스크 표식) 백업 → backup replicate --wait → 원본 replicas ready·목록 dr:ready, dr에 같은 이름 ready 사본(id = 사본 기록, 출처 prod·원본 id·CA 지문, 목록 ORIGIN), dr에서 복원한 디스크 내용이 같음, prod에서 원본을 지우면 dr 사본과 파일도 지워짐"
```

---

### Task 6: 웹: 백업 원격 복제 화면

백업 표의 출처 배지와 "원격 사본" 열(상태·실패 오류를 글로), "원격으로 복제" 대화상자(원격의 백업 풀은 중계로), 백업 작업의 원격 복제 고르기, 경고가 있는 성공 Task의 경고 토스트. ko·en.

**Files:**
- Create: `web/src/backups/RemoteTargetFields.tsx`
- Modify: `web/src/backups/BackupJobsPage.test.tsx`
- Modify: `web/src/backups/BackupJobsPage.tsx`
- Modify: `web/src/backups/BackupTable.tsx`
- Modify: `web/src/backups/BackupsPage.test.tsx`
- Modify: `web/src/backups/jobs.test.ts`
- Modify: `web/src/backups/jobs.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/remotes/remotes.ts`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/tasks.test.tsx`
- Modify: `web/src/tasks/tasks.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { backupJob, pool } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { backupJob, pool, remote } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx
const off = backupJob("weekly", 3, { spec: { enabled: false } });
```

을 다음으로 바꾼다:

```tsx
const off = backupJob("weekly", 3, { spec: { enabled: false } });
const copying = backupJob("offsite", 4, { spec: { replicate: { remote: "dr", pool: "dr-bk" } } });

/** 원격 dr과 그 백업 풀 dr-bk·dr-bk2(중계) */
function mockRemotes() {
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({
        revision: 1,
        items: [pool("dr-bk", { backups: true }), pool("dr-bk2", { backups: true })],
      }),
    ),
  );
}
```

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx
  expect(weekly("Next run")).toHaveTextContent("—");
  expect(screen.getByRole("button", { name: "Run weekly now" })).toBeDisabled();
```

을 다음으로 바꾼다:

```tsx
  expect(weekly("Next run")).toHaveTextContent("—");
  expect(weekly("Remote copy")).toHaveTextContent("—");
  expect(screen.getByRole("button", { name: "Run weekly now" })).toBeDisabled();
```

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx

it("creates a job after checking the form", async () => {
```

을 다음으로 바꾼다:

```tsx

it("shows where a job copies its backups", async () => {
  mockLists({ "backup-job": [copying] });
  renderPage(<BackupJobsPage />);
  const table = await screen.findByRole("table", { name: "Backup jobs" });
  await within(table).findByText("offsite");
  expect(cells(table, "offsite")("Remote copy")).toHaveTextContent("dr / dr-bk");
});

it("creates a job that copies its backups to a remote cluster", async () => {
  mockLists({ "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(copying, { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "offsite");
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "03:00");
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Copy to a remote cluster"), "dr");
  await within(dialog).findByRole("option", { name: "dr-bk2" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Backup pool on the remote cluster"),
    "dr-bk2",
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Backup job offsite created.")).toBeInTheDocument();
  expect(body).toMatchObject({
    name: "offsite",
    spec: { pool: "bk", replicate: { remote: "dr", pool: "dr-bk2" } },
  });
});

it("turns off copying to a remote cluster", async () => {
  mockLists({ "backup-job": [copying], "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/backup-jobs/${copying.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(copying);
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit offsite" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit offsite" });
  expect(within(dialog).getByLabelText("Copy to a remote cluster")).toHaveValue("dr");
  expect(within(dialog).getByLabelText("Backup pool on the remote cluster")).toHaveValue("dr-bk");
  await userEvent.selectOptions(
    within(dialog).getByLabelText("Copy to a remote cluster"),
    "Do not copy",
  );
  expect(
    within(dialog).queryByLabelText("Backup pool on the remote cluster"),
  ).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Backup job offsite saved.")).toBeInTheDocument();
  expect(body).toEqual({ replicate: null });
});

it("creates a job after checking the form", async () => {
```

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx
  });
  let body: unknown;
```

을 다음으로 바꾼다:

```tsx
  });
  mockRemotes();
  let body: unknown;
```

`web/src/backups/BackupJobsPage.test.tsx`에서

```tsx
  mockLists({ "backup-job": [nightly], "storage-pool": [pool("bk", { backups: true })] });
  let body: unknown;
```

을 다음으로 바꾼다:

```tsx
  mockLists({ "backup-job": [nightly], "storage-pool": [pool("bk", { backups: true })] });
  mockRemotes();
  let body: unknown;
```

`web/src/backups/BackupJobsPage.tsx`에서

```tsx
} from "@/backups/jobs";
import { ConfirmDialog } from "@/components/ConfirmDialog";
```

을 다음으로 바꾼다:

```tsx
} from "@/backups/jobs";
import { RemoteTargetFields } from "@/backups/RemoteTargetFields";
import { ConfirmDialog } from "@/components/ConfirmDialog";
```

`web/src/backups/BackupJobsPage.tsx`에서

```tsx
            <TableHead>{t("backupJobs.keep")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("backupJobs.keep")}</TableHead>
            <TableHead>{t("backupJobs.replicate")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
```

`web/src/backups/BackupJobsPage.tsx`에서

```tsx
          {list.length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={jobs} empty={t("backupJobs.empty")} />
          )}
          {list.map((job) => {
            const { vms, schedule, enabled } = job.spec;
            const { run: running, last_run: last, next_run: next, errors } = job.status;
```

을 다음으로 바꾼다:

```tsx
          {list.length === 0 && (
            <TableMessage colSpan={write ? 10 : 9} query={jobs} empty={t("backupJobs.empty")} />
          )}
          {list.map((job) => {
            const { vms, schedule, enabled, replicate } = job.spec;
            const { run: running, last_run: last, next_run: next, errors } = job.status;
```

`web/src/backups/BackupJobsPage.tsx`에서

```tsx
                    : keep.map(([unit, n]) => t(`backupJobs.keepUnit.${unit}`, { n })).join(", ")}
                </TableCell>
                <TableCell>
                  {running ? (
```

을 다음으로 바꾼다:

```tsx
                    : keep.map(([unit, n]) => t(`backupJobs.keepUnit.${unit}`, { n })).join(", ")}
                </TableCell>
                <TableCell>{replicate ? `${replicate.remote} / ${replicate.pool}` : "—"}</TableCell>
                <TableCell>
                  {running ? (
```

`web/src/backups/BackupJobsPage.tsx`에서

```tsx
      </fieldset>
      <div className="flex items-center gap-2">
```

을 다음으로 바꾼다:

```tsx
      </fieldset>
      <RemoteTargetFields
        id="job-replicate"
        label={t("backupJobs.replicateLabel")}
        none={t("backupJobs.noReplicate")}
        hint={t("backupJobs.replicateHint")}
        value={{ remote: form.replicateRemote, pool: form.replicatePool }}
        errors={{ pool: errors.replicatePool }}
        onChange={(target) =>
          change({ replicateRemote: target.remote, replicatePool: target.pool })
        }
      />
      <div className="flex items-center gap-2">
```

`web/src/backups/BackupTable.tsx` 전체:

```tsx
import { CircleAlert, CircleCheck, LoaderCircle } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Backup, Task } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { storedBytes } from "@/backups/backups";
import { type RemoteTarget, RemoteTargetFields } from "@/backups/RemoteTargetFields";
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
  receiving: "secondary",
} as const;

type Replica = NonNullable<Backup["status"]["replicas"]>[number];

const REPLICA_ICON = {
  ready: CircleCheck,
  copying: LoaderCircle,
  failed: CircleAlert,
} as const;

/**
 * 원격 사본 하나: 상태 아이콘과 원격 이름. 풀·상태·오류는 툴팁(title)에, 풀과 상태는 화면 읽기용 글에도
 * 둔다(실패한 사본의 오류는 표가 사본 아래에 글로 보인다).
 */
function ReplicaBadge({ replica }: { replica: Replica }) {
  const { t } = useTranslation();
  const Icon = REPLICA_ICON[replica.state];
  const state = t(`backups.replicaState.${replica.state}`);
  const title = t("backups.replicaTitle", { remote: replica.remote, pool: replica.pool, state });
  return (
    <span
      className={`inline-flex items-center gap-1 ${replica.state === "failed" ? "text-destructive" : ""}`}
      title={replica.error ? `${title}. ${replica.error}` : title}
    >
      <Icon className="size-4" aria-hidden />
      <span aria-hidden>{replica.remote}</span>
      <span className="sr-only">{title}</span>
    </span>
  );
}

/**
 * 백업 표와 복원·원격 복제·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만). 작업 열은 백업을 만든 백업 작업이다(손으로 만든 백업은 비어 있다). 받은 사본은
 * 이름 옆에 보낸 클러스터를, 원격 사본 열은 원격으로 보낸 사본의 상태를 보인다.
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
  const jobs = useList("backup-job");
  const [restoring, setRestoring] = useState<Backup | null>(null);
  const [replicating, setReplicating] = useState<Backup | null>(null);
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
  const columns = 7 + (showVm ? 1 : 0) + (actions ? 1 : 0);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));
  const originHint = (origin: NonNullable<Backup["spec"]["origin"]>) =>
    t(origin.job ? "backups.originJobHint" : "backups.originHint", {
      cluster: origin.cluster,
      backup: origin.backup,
      job: origin.job,
    });
  const jobName = (id: string | null | undefined) => {
    if (!id) {
      return "—";
    }
    const job = (jobs.data ?? []).find((j) => j.id === id);
    // 작업을 지워도 백업은 작업 id를 그대로 가진다.
    return job ? job.name : jobs.isSuccess ? t("backups.jobDeleted") : "—";
  };

  return (
    <>
      <Table aria-labelledby={labelledBy}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            {showVm && <TableHead>{t("backups.vm")}</TableHead>}
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("backups.job")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("backups.created")}</TableHead>
            <TableHead>{t("backups.size")}</TableHead>
            <TableHead>{t("backups.replicas")}</TableHead>
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
            const { phase, created_at: created, error, replicas = [] } = backup.status;
            const origin = backup.spec.origin;
            return (
              <TableRow key={backup.id}>
                <TableCell className="font-medium">
                  {backup.name}
                  {/* 다른 클러스터에서 받은 사본 */}
                  {origin && (
                    <Badge variant="outline" className="ml-2" title={originHint(origin)}>
                      {t("backups.origin", { cluster: origin.cluster })}
                      <span className="sr-only">{originHint(origin)}</span>
                    </Badge>
                  )}
                </TableCell>
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
                <TableCell>{jobName(backup.spec.job)}</TableCell>
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
                <TableCell>
                  {replicas.length === 0 ? (
                    "—"
                  ) : (
                    <>
                      <div className="flex flex-wrap gap-2">
                        {replicas.map((r) => (
                          <ReplicaBadge key={`${r.remote}/${r.pool}`} replica={r} />
                        ))}
                      </div>
                      {replicas
                        .filter((r) => r.state === "failed" && r.error)
                        .map((r) => (
                          <div
                            key={`${r.remote}/${r.pool}`}
                            className="max-w-xs text-xs whitespace-normal break-words text-destructive"
                          >
                            {t("backups.replicaError", {
                              remote: r.remote,
                              pool: r.pool,
                              error: r.error,
                            })}
                          </div>
                        ))}
                    </>
                  )}
                </TableCell>
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
                    {/* 복제 API는 백업 쓰기 권한이다(지우기와 같다). 원격 쪽 권한은 원격 토큰의 역할이다. */}
                    {remove && (
                      <Button
                        variant="outline"
                        size="sm"
                        className="ml-2"
                        aria-label={t("backups.replicateNamed", { name: backup.name })}
                        disabled={phase !== "ready"}
                        onClick={(e) => {
                          opener.current = e.currentTarget;
                          setReplicating(backup);
                        }}
                      >
                        {t("backups.replicate")}
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
      {replicating && (
        <ReplicateDialog
          backup={replicating}
          returnFocus={opener}
          onClose={() => setReplicating(null)}
        />
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

/** 백업을 원격 클러스터의 백업 풀에 사본으로 보내는 대화상자. 열 때만 그린다. */
function ReplicateDialog({
  backup,
  returnFocus,
  onClose,
}: {
  backup: Backup;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [target, setTarget] = useState<RemoteTarget>({ remote: "", pool: "" });
  const [errors, setErrors] = useState<{ remote?: string; pool?: string }>({});

  async function submit() {
    const found: { remote?: string; pool?: string } = {};
    if (target.remote === "") {
      found.remote = "backups.errors.remote";
    } else if (target.pool === "") {
      found.pool = "backups.errors.pool";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/backups/{key}/actions/replicate", {
        params: { path: { key: backup.id } },
        body: target,
      }),
    );
    trackTask(task, t("tasks.op.backup-replicate", { target: backup.name, remote: target.remote }));
  }

  return (
    <FormDialog
      title={t("backups.replicateNamed", { name: backup.name })}
      description={t("backups.replicateDescription")}
      submitLabel={t("backups.replicateSubmit")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <RemoteTargetFields
        id="replicate"
        label={t("backups.remote")}
        none={t("backups.chooseRemote")}
        value={target}
        errors={errors}
        onChange={setTarget}
      />
    </FormDialog>
  );
}
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
import { resetTasks } from "@/tasks/tasks";
import { backup, backupJob, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { resetTasks } from "@/tasks/tasks";
import { backup, backupJob, pool, remote, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
  expect(within(row).getByRole("button", { name: "Restore web-copy" })).toBeDisabled();
});

it("restores a backup as a new VM", async () => {
```

을 다음으로 바꾼다:

```tsx
  expect(within(row).getByRole("button", { name: "Restore web-copy" })).toBeDisabled();
});

it("shows where a received copy came from and the state of remote copies", async () => {
  const received = backup("web-copy", web, 3, {
    spec: {
      origin: {
        cluster: "prod",
        ca_fingerprint: `sha256:${"cd".repeat(32)}`,
        backup: "019a0f3c-0000-7000-8006-0000000000aa",
        job: "nightly",
      },
    },
  });
  const copied = backup("web-2", web, 4, {
    status: {
      replicas: [
        { remote: "dr", pool: "dr-bk", state: "ready", at: 1 },
        { remote: "dr2", pool: "bk", state: "copying", at: 1 },
        { remote: "dr3", pool: "bk", state: "failed", error: "remote is down", at: 1 },
      ],
    },
  });
  mockLists({ vm: [web], backup: [ofWeb, received, copied] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  const cell = async (name: string, header: string) =>
    within((await within(table).findByText(name)).closest("tr")!).getAllByRole("cell")[
      headers.findIndex((h) => h.textContent === header)
    ]!;
  const origin = within(await cell("web-copy", "Name")).getByText("From prod");
  expect(origin).toHaveAttribute(
    "title",
    "Copy of backup 019a0f3c-0000-7000-8006-0000000000aa (job nightly) on cluster prod",
  );
  // 툴팁 없이도(화면 읽기) 원본 백업 id와 작업이 읽힌다.
  expect(within(await cell("web-copy", "Name")).getByText(/^Copy of backup/)).toHaveClass(
    "sr-only",
  );
  expect(await cell("web-1", "Name")).not.toHaveTextContent("From");

  const copies = await cell("web-2", "Remote copies");
  expect(within(copies).getByText("dr").closest("[title]")).toHaveAttribute(
    "title",
    "dr / dr-bk: Ready",
  );
  expect(within(copies).getByText("dr2").closest("[title]")).toHaveAttribute(
    "title",
    "dr2 / bk: Copying",
  );
  expect(within(copies).getByText("dr3").closest("[title]")).toHaveAttribute(
    "title",
    "dr3 / bk: Failed. remote is down",
  );
  // 원격의 풀과 상태는 화면 읽기에도 읽히고, 실패한 사본의 오류는 글로 보인다.
  expect(within(copies).getByText("dr / dr-bk: Ready")).toHaveClass("sr-only");
  expect(within(copies).getByText("dr3 / bk: Failed")).toHaveClass("sr-only");
  expect(within(copies).getByText("dr3 / bk: remote is down")).toBeVisible();
  expect(within(copies).queryByText(/dr2 \/ bk: .*down/)).not.toBeInTheDocument();
  expect(await cell("web-1", "Remote copies")).toHaveTextContent("—");
});

it("copies a ready backup to a backup pool of a remote cluster", async () => {
  const notReady = backup("web-0", web, 2, { status: { phase: "pending" } });
  mockLists({ vm: [web], backup: [ofWeb, notReady] });
  let body: unknown;
  server.use(
    http.get(apiUrl("/remotes"), () =>
      HttpResponse.json({ revision: 1, items: [remote("dr"), remote("dr2")] }),
    ),
    // 원격의 풀은 중계로 읽고 백업 풀만 고른다.
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json({
        revision: 1,
        items: [pool("local"), pool("dr-bk", { backups: true })],
      }),
    ),
    http.post(apiUrl(`/backups/${ofWeb.id}/actions/replicate`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "backup-replicate", ofWeb.id), { status: 202 });
    }),
  );
  renderPage(<BackupsPage />);
  expect(
    await screen.findByRole("button", { name: "Copy web-0 to a remote cluster" }),
  ).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: "Copy web-1 to a remote cluster" }));
  const dialog = await screen.findByRole("dialog", { name: "Copy web-1 to a remote cluster" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(within(dialog).getByText("Choose a remote cluster.")).toBeVisible();
  await within(dialog).findByRole("option", { name: "dr2" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  const pools = within(dialog).getByLabelText("Backup pool on the remote cluster");
  await within(dialog).findByRole("option", { name: "dr-bk" });
  expect(
    within(pools)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["Choose a pool", "dr-bk"]);
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  await userEvent.selectOptions(pools, "dr-bk");
  await userEvent.click(within(dialog).getByRole("button", { name: "Copy" }));
  expect(await screen.findByText("Copy backup web-1 to dr")).toBeInTheDocument();
  expect(body).toEqual({ remote: "dr", pool: "dr-bk" });
});

it("explains a remote cluster whose pools cannot be read", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  server.use(
    http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items: [remote("dr")] })),
    http.get(apiUrl("/remotes/dr/api/storage-pools"), () =>
      HttpResponse.json(
        { code: "remote_unreachable", message: "connection refused" },
        { status: 502 },
      ),
    ),
  );
  renderPage(<BackupsPage />);
  await userEvent.click(
    await screen.findByRole("button", { name: "Copy web-1 to a remote cluster" }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Copy web-1 to a remote cluster" });
  await within(dialog).findByRole("option", { name: "dr" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Remote cluster"), "dr");
  // 서버 오류라 두 번 더 읽어 본 뒤(1초·2초 뒤) 보인다.
  expect(await within(dialog).findByRole("alert", {}, { timeout: 6000 })).toHaveTextContent(
    "Cannot reach the remote cluster (connection refused).",
  );
}, 10_000);

it("restores a backup as a new VM", async () => {
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A task on this backup (backing up, restoring or deleting) is in progress.",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A task on this backup (backing up, restoring, copying or deleting) is in progress.",
  );
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
  expect(screen.getByRole("button", { name: "Delete backup web-1" })).toBeEnabled();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("button", { name: "Delete backup web-1" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Copy web-1 to a remote cluster" })).toBeEnabled();
});

it("does not let viewers copy backups", async () => {
  mockLists({ vm: [web], backup: [ofWeb] });
  renderPage(<BackupsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web-1")).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Copy web-1 to a remote cluster" }),
  ).not.toBeInTheDocument();
});
```

`web/src/backups/RemoteTargetFields.tsx`를 만든다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { backupPools } from "@/backups/backups";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { remoteStoragePools } from "@/remotes/remotes";

/** 사본을 보낼 곳. `remote`가 비면 고르지 않은 것이다. */
export interface RemoteTarget {
  remote: string;
  pool: string;
}

/**
 * 원격과 그 원격의 백업 풀을 고르는 칸. 원격 목록은 이 클러스터의 API로, 풀 목록은 중계(`GET
 * /storage-pools`)로 읽는다. 원격을 바꾸면 풀은 다시 고른다. 이미 정한 값(작업의 대상)은 목록에 없어도
 * 보인다(서버가 저장할 때 판단한다). `none`은 원격을 고르지 않은 칸의 글이다.
 */
export function RemoteTargetFields({
  id,
  label,
  none,
  hint,
  value,
  errors,
  onChange,
}: {
  id: string;
  label: string;
  none: string;
  hint?: string;
  value: RemoteTarget;
  errors: { remote?: string; pool?: string };
  onChange(value: RemoteTarget): void;
}) {
  const { t } = useTranslation();
  const remotes = useQuery({
    queryKey: ["remotes"],
    queryFn: () => call(api.GET("/remotes")),
    select: (list) => list.items.map((r) => r.name).sort((a, b) => a.localeCompare(b)),
  });
  const pools = useQuery({
    queryKey: ["remotes", value.remote, "storage-pools"],
    queryFn: ({ signal }) => remoteStoragePools(value.remote, signal),
    select: (items) => backupPools(items).map((p) => p.name),
    enabled: value.remote !== "",
  });
  const remoteNames = [...(remotes.data ?? [])];
  if (value.remote !== "" && !remoteNames.includes(value.remote)) {
    remoteNames.unshift(value.remote);
  }
  const poolNames = [...(pools.data ?? [])];
  if (value.pool !== "" && !poolNames.includes(value.pool)) {
    poolNames.unshift(value.pool);
  }

  return (
    <>
      {!remotes.isSuccess && <ListPending error={remotes.error} />}
      <Field id={`${id}-remote`} label={label} error={errors.remote} hint={hint}>
        {(a11y) => (
          <NativeSelect
            id={`${id}-remote`}
            className="w-full"
            value={value.remote}
            onChange={(e) => onChange({ remote: e.target.value, pool: "" })}
            {...a11y}
          >
            <NativeSelectOption value="">{none}</NativeSelectOption>
            {remoteNames.map((name) => (
              <NativeSelectOption key={name} value={name}>
                {name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {remotes.isSuccess && remotes.data.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("backups.noRemotes")}</p>
      )}
      {value.remote !== "" && (
        <>
          {!pools.isSuccess && <ListPending error={pools.error} />}
          <Field id={`${id}-pool`} label={t("backups.remotePool")} error={errors.pool}>
            {(a11y) => (
              <NativeSelect
                id={`${id}-pool`}
                className="w-full"
                value={value.pool}
                onChange={(e) => onChange({ ...value, pool: e.target.value })}
                {...a11y}
              >
                <NativeSelectOption value="">{t("backups.choosePool")}</NativeSelectOption>
                {poolNames.map((name) => (
                  <NativeSelectOption key={name} value={name}>
                    {name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          {pools.isSuccess && pools.data.length === 0 && (
            <p className="text-sm text-muted-foreground">{t("backups.noRemotePools")}</p>
          )}
        </>
      )}
    </>
  );
}
```

`web/src/backups/jobs.test.ts`에서

```ts
  ).toEqual({});
});
```

을 다음으로 바꾼다:

```ts
  ).toEqual({});
});

it("copies backups to a remote only when one is chosen", () => {
  const form = { ...emptyForm("bk"), name: "nightly", at: "03:00" };
  expect(specOf(form)).not.toHaveProperty("replicate");
  const copying = { ...form, replicateRemote: "dr", replicatePool: "dr-bk" };
  expect(checkForm(copying, [])).toEqual({});
  expect(specOf(copying).replicate).toEqual({ remote: "dr", pool: "dr-bk" });
  // 원격을 골랐으면 그 백업 풀도 고른다.
  expect(checkForm({ ...copying, replicatePool: "" }, []).replicatePool).toBe(
    "backups.errors.pool",
  );

  const job = backupJob("nightly", 1, { spec: { replicate: { remote: "dr", pool: "dr-bk" } } });
  expect(formOf(job)).toMatchObject({ replicateRemote: "dr", replicatePool: "dr-bk" });
  expect(specOf(formOf(job))).toEqual(job.spec);
  expect(patchOf(job, specOf(formOf(job)))).toEqual({});
  expect(patchOf(job, specOf({ ...formOf(job), replicatePool: "other" }))).toEqual({
    replicate: { remote: "dr", pool: "other" },
  });
  // 끄면 null을 보낸다(빼면 그대로 둔다).
  expect(patchOf(job, specOf({ ...formOf(job), replicateRemote: "" }))).toEqual({
    replicate: null,
  });
  expect(patchOf(backupJob("n", 2), specOf(copying))).toMatchObject({
    replicate: { remote: "dr", pool: "dr-bk" },
  });
});
```

`web/src/backups/jobs.ts`에서

```ts
  enabled: boolean;
}
```

을 다음으로 바꾼다:

```ts
  enabled: boolean;
  /** 사본을 보낼 원격. 비면 복제하지 않는다. */
  replicateRemote: string;
  /** 원격의 백업 풀 */
  replicatePool: string;
}
```

`web/src/backups/jobs.ts`에서

```ts
    enabled: true,
  };
```

을 다음으로 바꾼다:

```ts
    enabled: true,
    replicateRemote: "",
    replicatePool: "",
  };
```

`web/src/backups/jobs.ts`에서

```ts
    enabled: job.spec.enabled,
  };
```

을 다음으로 바꾼다:

```ts
    enabled: job.spec.enabled,
    replicateRemote: job.spec.replicate?.remote ?? "",
    replicatePool: job.spec.replicate?.pool ?? "",
  };
```

`web/src/backups/jobs.ts`에서

```ts
    found.pool = "backups.errors.pool";
  }
```

을 다음으로 바꾼다:

```ts
    found.pool = "backups.errors.pool";
  }
  if (form.replicateRemote !== "" && form.replicatePool === "") {
    found.replicatePool = "backups.errors.pool";
  }
```

`web/src/backups/jobs.ts`에서

```ts
    enabled: form.enabled,
  };
}

/** 작업에서 바뀐 필드만(`PATCH` 본문). 일정과 보존 규칙은 통째로 보낸다. 요일은 집합으로 비교한다. */
export function patchOf(job: BackupJob, spec: Spec): BackupJobPatch {
```

을 다음으로 바꾼다:

```ts
    enabled: form.enabled,
    // 복제하지 않으면 필드를 빼서 보낸다.
    ...(form.replicateRemote !== "" && {
      replicate: { remote: form.replicateRemote, pool: form.replicatePool },
    }),
  };
}

/**
 * 작업에서 바뀐 필드만(`PATCH` 본문). 일정과 보존 규칙은 통째로 보낸다. 요일은 집합으로 비교한다. 복제를
 * 끄면 `replicate: null`이다(빼면 그대로 둔다).
 */
export function patchOf(job: BackupJob, spec: Spec): BackupJobPatch {
```

`web/src/backups/jobs.ts`에서

```ts
  }
  return patch;
```

을 다음으로 바꾼다:

```ts
  }
  const target = (r: Spec["replicate"]) => (r ? [r.remote, r.pool] : null);
  if (!same(target(job.spec.replicate), target(spec.replicate))) {
    patch.replicate = spec.replicate ?? null;
  }
  return patch;
```

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
    "vni_in_use": "Another network already uses this VNI.",
    "node_online": "The node is online. Run oplctl cluster leave on it instead.",
    "pool_not_on_node": "That storage pool is not available on the node you are connected to.",
    "insufficient_storage": "The node does not have enough free space for this upload.",
    "last_admin": "The last administrator cannot give up the administrator role.",
    "cannot_delete_self": "You cannot delete your own account.",
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "Another operation on this VM (start, stop, snapshot, revert, clone or migration) is in progress. Try again when it has finished.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2, all in LVM-thin pools, all in ZFS pools, or all in Ceph RBD pools can take snapshots. Snapshots of disks in LVM-thin, ZFS or Ceph RBD pools cannot include memory, and disks in ZFS pools can only revert to their latest snapshot.",
    "vm_has_snapshots": "The VM has snapshots. Delete them first.",
    "volume_in_snapshot": "A VM snapshot still uses this volume. Delete that snapshot first.",
    "snapshot_not_ready": "The snapshot is still being taken.",
    "node_offline": "The node holding this VM is offline.",
    "too_many_attempts": "Too many attempts. Try again later.",
    "machine_arch_mismatch": "The machine type doesn't match the selected node's architecture.",
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The target node is a witness, offline or in maintenance.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
    "task_failed": "Task error: {{message}}",
    "internal": "The server failed: {{message}}",
    "ha_unsupported": "The node has fencing off and cannot run HA VMs.",
    "firewall_unsupported": "The node has no firewall (nftables) and cannot run VMs with security groups.",
    "security_group_in_use": "A VM interface uses this security group. Remove it from the VM first.",
    "backup_busy": "A task on this backup (backing up, restoring, copying or deleting) is in progress. Try again when it has finished.",
    "backup_not_ready": "The backup is not ready. Only finished backups can be restored or copied.",
    "job_running": "The backup job is already running. Wait for the run to finish.",
    "job_disabled": "The backup job is off. Turn on \"Run on schedule\" first.",
    "invalid_ova": "The file is not a usable OVA: {{message}}",
    "import_busy": "The import is running, or another task on it has not finished.",
    "import_done": "The VM has already been imported.",
    "import_failed": "The import cannot run: {{message}}",
    "untrusted_certificate": "Check the ESXi certificate fingerprint and trust it to continue.",
    "fingerprint_mismatch": "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
    "esxi_auth": "ESXi rejected the user name or password. Check them and try again.",
    "esxi_unreachable": "Cannot reach the ESXi host ({{message}}).",
    "version_too_old": "The node runs an older opLite than the cluster. Upgrade it first: {{message}}",
    "updates_unsupported": "The node does not check for updates.",
    "upgrade_in_progress": "An upgrade is in progress: {{message}}",
    "cluster_upgrading": "The cluster is being upgraded: {{message}}",
    "subscription_required": "An active subscription is required for the enterprise channel.",
    "invalid_subscription_key": "The subscription key is not valid: {{message}}",
    "telemetry_endpoint_required": "Set a telemetry endpoint first. This build has no default endpoint.",
    "no_role": "Your directory account has no role in opLite. Ask an administrator to map one of your groups.",
    "realm_unavailable": "Cannot reach the directory of this realm. Try again later or ask an administrator.",
    "external_user": "This user signs in through a realm. Change the password in the directory.",
    "sso_failed": "Single sign-on failed. Try again, or ask an administrator to check the realm.",
    "remote_unreachable": "Cannot reach the remote cluster ({{message}}).",
    "remote_fingerprint_mismatch": "The remote cluster's CA fingerprint is different. Get the CA again and check it.",
    "remote_is_local": "That address is this cluster. Add a different cluster.",
    "remote_unauthorized": "The remote cluster rejected the API token. Check that the token still exists there."
  },
  "nav": {
    "dashboard": "Dashboard",
    "vms": "Virtual machines",
    "imports": "Imports",
    "volumes": "Volumes",
    "pools": "Storage pools",
    "backups": "Backups",
    "backupJobs": "Backup jobs",
    "networks": "Networks",
    "securityGroups": "Security groups",
    "nodes": "Nodes",
    "tasks": "Tasks",
    "audit": "Audit log",
    "users": "Users",
    "realms": "Realms",
    "tokens": "API tokens",
    "cluster": "Cluster",
    "remotes": "Remote clusters",
    "settings": "Settings"
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
    "submit": "Sign in",
    "version": "opLite {{version}}",
    "realm": "Realm",
    "localRealm": "Local users",
    "or": "or",
    "sso": {
      "signIn": "Sign in with {{name}}",
      "no_role": "Your account at the identity provider has no role in opLite. Ask an administrator to map one of your groups.",
      "realm_unavailable": "Cannot reach the identity provider of this realm. Try again later or ask an administrator."
    }
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
    "doneWarning": "{{label}}: done with a warning. {{warning}}",
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
      "vm-clone": "Clone {{target}}",
      "vm-migrate": "Migrate {{target}}",
      "vm-ha-recover": "HA recovery of {{target}} (from node {{from}})",
      "vm-backup": "Back up {{target}}",
      "backup-delete": "Delete backup {{target}}",
      "backup-restore": "Restore a backup as {{target}}",
      "backup-replicate": "Copy backup {{target}} to {{remote}}",
      "vm-import": "Import VM {{target}}",
      "vm-import-delete": "Delete import {{target}}",
      "node-update-check": "Check updates on {{target}}",
      "node-upgrade": "Upgrade {{target}}"
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
      "busy": "The VM is starting or stopping.",
      "migrating": "Not available while the VM is being migrated.",
      "snapshotPending": "Not available while a snapshot or clone task is in progress."
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
      "failed": "Failed",
      "migrating": "Migrating"
    },
    "migratingTo": "{{from}} → {{to}}",
    "action": {
      "start": "Start",
      "stop": "Stop",
      "force-stop": "Force stop",
      "reboot": "Reboot",
      "console": "Console",
      "delete": "Delete",
      "edit": "Edit",
      "clone": "Clone",
      "migrate": "Migrate"
    },
    "notFound": "There is no VM {{name}}",
    "backToList": "Back to virtual machines",
    "tab": {
      "summary": "Summary",
      "hardware": "Hardware",
      "console": "Console",
      "snapshots": "Snapshots",
      "backups": "Backups",
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
    "noNics": "No network interfaces.",
    "ha": "HA"
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
    "busy": "The VM is starting, stopping or migrating. Snapshots can be taken, reverted and deleted once it settles.",
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
    "busy": "The VM is starting, stopping or migrating. Try again when it has settled.",
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
  "backups": {
    "vm": "VM",
    "pool": "Backup pool",
    "job": "Job",
    "jobDeleted": "Deleted job",
    "created": "Created",
    "size": "Size",
    "origin": "From {{cluster}}",
    "originHint": "Copy of backup {{backup}} on cluster {{cluster}}",
    "originJobHint": "Copy of backup {{backup}} (job {{job}}) on cluster {{cluster}}",
    "replicas": "Remote copies",
    "replicaState": {
      "copying": "Copying",
      "ready": "Ready",
      "failed": "Failed"
    },
    "replicaTitle": "{{remote}} / {{pool}}: {{state}}",
    "replicaError": "{{remote}} / {{pool}}: {{error}}",
    "replicate": "Copy to remote",
    "replicateNamed": "Copy {{name}} to a remote cluster",
    "replicateDescription": "Sends the backup files to a backup pool of a remote cluster. The copy there is an ordinary backup that can be restored on that cluster. Deleting this backup deletes the copy too.",
    "replicateSubmit": "Copy",
    "remote": "Remote cluster",
    "chooseRemote": "Choose a remote cluster",
    "remotePool": "Backup pool on the remote cluster",
    "noRemotes": "No remote clusters. Add one on the Remote clusters page.",
    "noRemotePools": "The remote cluster has no storage pool that stores backups.",
    "empty": "No backups.",
    "phase": {
      "pending": "Creating",
      "ready": "Ready",
      "failed": "Failed",
      "receiving": "Receiving"
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
    "deleteDescription": "The backup files are removed from the backup pool, and the copies sent to remote clusters are deleted too. This cannot be undone.",
    "op": {
      "create": "Back up {{vm}}",
      "restore": "Restore {{backup}} as {{name}}",
      "delete": "Delete backup {{name}}"
    },
    "errors": {
      "exists": "A backup with this name already exists.",
      "vmExists": "A VM with this name already exists.",
      "pool": "Choose a storage pool.",
      "remote": "Choose a remote cluster."
    }
  },
  "backupJobs": {
    "pageHint": "Back up the chosen VMs at a set time and delete old backups by the retention rules. Times are in UTC. The cluster leader runs the jobs, at most a few backups per node at a time.",
    "empty": "No backup jobs.",
    "vms": "VMs",
    "schedule": "Schedule (UTC)",
    "keep": "Keep",
    "nextRun": "Next run",
    "lastRun": "Last run",
    "everyDay": "Every day",
    "day": {
      "mon": "Mon",
      "tue": "Tue",
      "wed": "Wed",
      "thu": "Thu",
      "fri": "Fri",
      "sat": "Sat",
      "sun": "Sun"
    },
    "keepAll": "All backups",
    "keepUnit": {
      "last": "last {{n}}",
      "daily": "{{n}} daily",
      "weekly": "{{n}} weekly",
      "monthly": "{{n}} monthly"
    },
    "state": {
      "running": "Running",
      "enabled": "On",
      "disabled": "Off"
    },
    "waiting_one": "{{count}} VM waiting",
    "waiting_other": "{{count}} VMs waiting",
    "result": "{{ok}} ok, {{failed}} failed",
    "runNow": "Run now",
    "runNamed": "Run {{name}} now",
    "runRequested": "{{name}} starts at the scheduler's next check.",
    "editNamed": "Edit {{name}}",
    "deleteNamed": "Delete backup job {{name}}",
    "createTitle": "Create backup job",
    "createDescription": "Backs up the chosen VMs to a backup pool on the chosen days and deletes the backups the retention rules no longer keep.",
    "editTitle": "Edit {{name}}",
    "editDescription": "Changes apply from the next run. A run in progress finishes.",
    "target": "VMs to back up",
    "targets": {
      "all": "All VMs",
      "names": "VMs by name",
      "labels": "VMs with labels"
    },
    "names": "VM names",
    "namesHint": "Separate names with commas.",
    "labels": "Labels",
    "labelsHint": "KEY=VALUE pairs separated by commas. A VM must have all of them.",
    "at": "Time (UTC)",
    "atHint": "HH:MM in UTC, such as 03:00.",
    "days": "Days",
    "daysHint": "Leave every day unchecked to run every day.",
    "retention": "Retention",
    "retentionHint": "Keeps the newest backups (Last), then the newest backup of each day, ISO week and month, counted per VM. Leave all at 0 to keep every backup. Failed backups are deleted after a later success.",
    "keepLabel": {
      "last": "Last",
      "daily": "Daily",
      "weekly": "Weekly",
      "monthly": "Monthly"
    },
    "enabled": "Run on schedule",
    "replicate": "Remote copy",
    "replicateLabel": "Copy to a remote cluster",
    "noReplicate": "Do not copy",
    "replicateHint": "Each backup the job makes is copied to this backup pool of the remote cluster. Failed copies are tried again at the next run.",
    "created": "Backup job {{name}} created.",
    "saved": "Backup job {{name}} saved.",
    "deleted": "Backup job {{name}} deleted.",
    "deleteTitle": "Delete backup job {{name}}?",
    "deleteDescription": "The job stops running. The backups it made are kept.",
    "errors": {
      "exists": "A backup job with this name already exists.",
      "names": "Enter at least one VM name.",
      "labels": "Enter at least one KEY=VALUE label.",
      "at": "Enter a time from 00:00 to 23:59.",
      "keep": "Enter whole numbers of 0 or more."
    }
  },
  "imports": {
    "pageHint": "Bring VMware VMs into opLite from OVA files or straight from ESXi or vCenter. Review the settings proposed from the source VM, then run the import to create a stopped VM.",
    "empty": "No imports.",
    "upload": "Upload OVA",
    "uploadTitle": "Upload an OVA",
    "uploadDescription": "The file goes to the node you are connected to ({{node}}) and stays there until the import is done or deleted. The import runs on that node.",
    "file": "OVA file",
    "uploaded": "Uploaded {{name}}. Review the proposed settings and run the import.",
    "review": "Review",
    "noResponse": "{{name}}: no response from the server. Check the import list for the result.",
    "sourceVm": "Source VM",
    "vm": "VM",
    "vmGone": "Deleted VM",
    "phase": {
      "ready": "Ready",
      "importing": "Importing",
      "done": "Done",
      "failed": "Failed"
    },
    "notFound": "Import {{name}} not found",
    "source": "Source",
    "fileName": "File",
    "guestOs": "Guest OS",
    "cpuValue": "{{vcpus}} vCPU, {{cores}} per socket",
    "diskFile": "File in the OVA",
    "capacity": "Size",
    "controller": "Controller",
    "adapter": "Adapter",
    "warnings": "Warnings",
    "lastError": "The last run failed: {{error}}",
    "failedError": "The source cannot be used: {{error}}",
    "importing": "Importing. The progress is on the Tasks page.",
    "imported": "Imported as VM",
    "settings": "VM settings",
    "settingsHint": "Proposed from the source VM. Change what you need.",
    "vmName": "New VM name",
    "pool": "Storage pool for the disks",
    "poolHint": "Only pools usable on {{node}}, where the import runs, are listed.",
    "sockets": "Sockets",
    "cores": "Cores per socket",
    "diskBus": "Disk bus",
    "nicModel": "NIC model",
    "networks": "Network mapping",
    "networksHint": "Choose an opLite network for each network the source VM uses.",
    "chooseNetwork": "Choose a network",
    "keepMacs": "Keep the source MAC addresses",
    "keepMacsHint": "Power off the VMware VM before starting the imported one. Turn this off to give the interfaces new MAC addresses.",
    "run": "Import",
    "deleteNamed": "Delete import {{name}}",
    "deleteTitle": "Delete import {{name}}?",
    "deleteDescription": "Deletes the import and its files on {{node}}. The source VM and any VM already imported are kept.",
    "forceHint": "{{error}} To forget the import and leave its files on that node, run oplctl import delete {{name}} --force.",
    "codes": {
      "node_offline": "The node running the import is offline or has left the cluster.",
      "mac_in_use": "Another VM uses a source MAC address. Turn off \"Keep the source MAC addresses\" to give the interfaces new MAC addresses."
    },
    "errors": {
      "reserved": "The names ova and esxi are reserved.",
      "exists": "An import with this name already exists.",
      "vmExists": "A VM with this name already exists.",
      "volumeExists": "A volume this import would create (NAME-disk0, …) already exists.",
      "pool": "Choose a storage pool.",
      "network": "Choose a network.",
      "poolNotOnNode": "This storage pool is not available on the node running the import.",
      "poolUnavailable": "This storage pool is not ready on the node running the import.",
      "sockets": "Enter 1 to 65535 sockets.",
      "nameTaken": "Name in use: {{message}}",
      "host": "Enter the ESXi or vCenter host.",
      "port": "Enter a port from 1 to 65535.",
      "user": "Enter the user name.",
      "datacenter": "Enter the datacenter.",
      "vm": "Choose a VM."
    },
    "esxi": "Import from ESXi",
    "esxiTitle": "Import from ESXi",
    "esxiDescription": "The connection is made from the node you are connected to ({{node}}), and the import runs there. The password stays only in that node's memory: if opld restarts there, create the import again.",
    "host": "Host",
    "port": "Port",
    "user": "User",
    "password": "Password",
    "datacenter": "Datacenter",
    "datacenterHint": "ha-datacenter for an ESXi host; for vCenter, the datacenter name.",
    "connect": "Connect",
    "untrusted": "{{host}} presented a certificate that is not trusted yet. Check that this SHA-256 fingerprint matches the certificate of the ESXi host before you continue.",
    "fingerprint": "Certificate fingerprint (SHA-256)",
    "trust": "Trust and continue",
    "vms": "Virtual machines",
    "noVms": "No VMs were found in the top-level folders of the datastores.",
    "datastore": "Datastore",
    "locked": "The VM looks powered on (lock files found). Power it off before importing.",
    "next": "Next",
    "back": "Back",
    "chosenVm": "Source",
    "esxiSource": "ESXi",
    "diskFileEsxi": "File on the datastore",
    "credentialsLost": "The ESXi password is no longer on this node (opld restarted or the request was interrupted). Delete this import and create it again."
  },
  "migrate": {
    "title": "Migrate {{name}}",
    "description": "The running VM moves to another node without stopping. Its disks must all be on shared storage pools, and both nodes need the same QEMU version and CPU.",
    "node": "Target node",
    "noTarget": "No other online node with the VM's architecture is available.",
    "submit": "Migrate",
    "op": "Migrate {{vm}} to {{node}}"
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
    "sharing": "Sharing",
    "sharedState": "Shared",
    "localState": "Node-local",
    "sharedNamed": "Shared storage: {{name}}",
    "makeSharedTitle": "Make {{name}} shared storage?",
    "makeSharedDescription": "Only do this when every node of the pool sees the same files at {{path}}, such as an NFS share mounted outside opLite. VMs with disks in the pool can then start on and migrate to any of its nodes.",
    "makeLocalTitle": "Make {{name}} node-local?",
    "makeLocalDescription": "Each volume goes back to the node that created it. VMs using the pool's volumes must be on that node and not migrating.",
    "makeShared": "Make shared",
    "makeLocal": "Make node-local",
    "madeShared": "{{name}} is now shared storage.",
    "madeLocal": "{{name}} is now node-local.",
    "sharingErrors": {
      "pool_in_use": "A VM uses this pool's volumes away from the node that created them, or is migrating: {{message}}"
    },
    "nodesHint": "Leave all unchecked to use it on every node.",
    "created": "Created storage pool {{name}}.",
    "deleteTitle": "Delete storage pool {{name}}?",
    "deleteDescription": "Only the pool is removed. Files in its directory or export, images in its Ceph pool and logical volumes in its shared volume group are kept.",
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
  "networks": {
    "kind": "Type",
    "kinds": {
      "bridge": "Bridge",
      "vxlan": "VXLAN"
    },
    "kindHint": {
      "bridge": "A Linux bridge that already exists on the nodes. VMs on this network run only on nodes with the bridge.",
      "vxlan": "opld creates the bridge oplv<VNI> and a VXLAN device on every node and links them over UDP 4789."
    },
    "bridge": "Bridge",
    "vlanVni": "VLAN/VNI",
    "vni": "VNI",
    "vniHint": "1–16777215, not used by another network.",
    "mtu": "MTU",
    "mtuOptional": "MTU (optional)",
    "mtuHint": "1280–9000. Defaults to 1450: a 1500-byte underlay minus the 50-byte VXLAN header.",
    "vlanOptional": "VLAN tag (optional)",
    "vlanHint": "1–4094. The bridge must be VLAN-aware (vlan_filtering 1).",
    "nodes": "Nodes with the bridge",
    "vms": "VMs",
    "empty": "No networks.",
    "userNote": "The user network (QEMU NAT) is always available and is not listed.",
    "noNode": "No node has this bridge",
    "noVlanFiltering": "VLAN filtering is off on {{nodes}}",
    "createTitle": "Create a network",
    "createDescription": "Points VM interfaces at a Linux bridge on the nodes or at a VXLAN network that spans the nodes.",
    "created": "Created network {{name}}.",
    "deleteTitle": "Delete network {{name}}?",
    "deleteDescription": "Bridges you created on the nodes are not changed. For a VXLAN network, opld removes its bridge and VXLAN device from the nodes.",
    "deleted": "Deleted network {{name}}.",
    "inUse": "Remove the VM interfaces first.",
    "errors": {
      "reserved": "The name user is reserved for NAT.",
      "bridge": "Use 1–15 characters without /, : or spaces.",
      "vlan": "Use a number from 1 to 4094.",
      "managed": "Names like oplv100 and oplx100 are reserved for VXLAN networks.",
      "vni": "Use a number from 1 to 16777215.",
      "mtu": "Use a number from 1280 to 9000."
    }
  },
  "securityGroups": {
    "note": "Firewall rules for VM interfaces. Nodes with nftables apply them right away, also to running VMs.",
    "rules": "Rules",
    "description": "Description",
    "descriptionOptional": "Description (optional)",
    "vms": "VMs",
    "empty": "No security groups.",
    "inUse": "Remove it from the VM interfaces first.",
    "createTitle": "Create a security group",
    "createDescription": "Creates an empty group. Add its rules on the next page.",
    "created": "Created security group {{name}}.",
    "deleteTitle": "Delete security group {{name}}?",
    "deleteDescription": "No VM interface uses it, so no traffic changes.",
    "deleted": "Deleted security group {{name}}.",
    "notFound": "Security group {{name}} not found",
    "saved": "Saved security group {{name}}.",
    "order": "Rules are checked from the top, and the first match decides. An interface with groups drops incoming traffic that no rule accepts and sends outgoing traffic that no rule drops. With several groups, the first group's rules come first.",
    "always": "Always allowed: replies to allowed traffic, ARP, IPv6 neighbor discovery and DHCP replies. A VM can only send from its own MAC address.",
    "direction": "Direction",
    "action": "Action",
    "protocol": "Protocol",
    "ports": "Ports",
    "remote": "Remote CIDR",
    "directionN": "Direction of rule {{n}}",
    "actionN": "Action of rule {{n}}",
    "protocolN": "Protocol of rule {{n}}",
    "portsN": "Ports of rule {{n}}",
    "remoteN": "Remote CIDR of rule {{n}}",
    "in": "In (to the VM)",
    "out": "Out (from the VM)",
    "accept": "Accept",
    "drop": "Drop",
    "any": "Any",
    "anyPort": "Any port",
    "anywhere": "Anywhere",
    "moveUp": "Move rule {{n}} up",
    "moveDown": "Move rule {{n}} down",
    "removeRule": "Remove rule {{n}}",
    "addRule": "Add a rule",
    "noRules": "No rules: incoming traffic is dropped and outgoing traffic is sent.",
    "errors": {
      "description": "Use at most 200 characters.",
      "ports": "Use a port like 22 or a range like 8000-8080 (1–65535).",
      "remote": "Use a CIDR like 10.0.0.0/8 or 2001:db8::/32.",
      "icmpRemote": "ICMP rules take only IPv4 addresses.",
      "icmpv6Remote": "ICMPv6 rules take only IPv6 addresses."
    }
  },
  "nodes": {
    "address": "API address",
    "underlay": "Underlay address",
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
    },
    "fencing": "Fencing",
    "fencingMode": {
      "watchdog": "Watchdog",
      "process": "Process"
    },
    "version": "Version",
    "updates": {
      "column": "Updates",
      "open": "Updates for {{name}}: {{state}}",
      "stateCount": "{{n}} available",
      "stateFailed": "check failed",
      "stateNotChecked": "not checked yet",
      "menu": "Updates…",
      "title": "Updates for {{name}}",
      "checked": "Last checked {{time}}.",
      "notChecked": "Not checked yet.",
      "none": "No updates available.",
      "failed": "The last check failed:",
      "failedBadge": "Check failed",
      "package": "Package",
      "current": "Installed",
      "candidate": "Candidate",
      "newPackage": "(new)",
      "truncated": "Only the first {{count}} packages are shown.",
      "refresh": "Check again",
      "checking": "Checking…",
      "upgrade": "Upgrade",
      "upgrading": "Upgrading…",
      "upgradeTitle": "Upgrade {{name}}?",
      "upgradeDescription": "VMs keep running, but opld restarts briefly. Upgrade other nodes after this upgrade finishes.",
      "errors": {
        "node_offline": "The node is offline."
      }
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
    "realm": "Realm",
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
  "realms": {
    "pageHint": "Users of a realm sign in with their directory (LDAP) or identity provider (OIDC) account as <name>@<realm>. Their role comes from their groups at each sign-in.",
    "adminOnly": "Only administrators can manage realms.",
    "empty": "No realms. Only local users can sign in.",
    "display": "Display name",
    "displayHint": "Shown on the sign-in page. Leave empty to show the name.",
    "kind": "Kind",
    "kinds": {
      "ldap": "LDAP",
      "oidc": "OIDC"
    },
    "url": "URL",
    "urlHint": "ldap:// always uses StartTLS. ldaps:// uses TLS from the start.",
    "baseDn": "Base DN",
    "bindDn": "Bind DN",
    "bindDnHint": "Service account used to find users. Leave empty to search anonymously.",
    "bindPassword": "Bind password",
    "bindPasswordSaved": "A password is saved. Leave this empty to keep it.",
    "clearBindPassword": "Clear the saved password",
    "userFilter": "User filter",
    "userFilterHint": "{username} is replaced with the escaped sign-in name.",
    "groupAttr": "Group attribute",
    "caPem": "CA certificate (PEM)",
    "caPemHint": "Trust only this CA for the directory's certificate. Leave empty to use the system roots.",
    "timeout": "Timeout (seconds)",
    "groups": "Group to role mapping",
    "groupsHint": "Group DNs match without regard to case. A user in several groups gets the highest role.",
    "groupN": "Group {{n}}",
    "groupRoleN": "Role of group {{n}}",
    "removeGroupN": "Remove group {{n}}",
    "addGroup": "Add group",
    "defaultRole": "Default role",
    "noDefaultRole": "None (refuse sign-in)",
    "createTitle": "Create an LDAP realm",
    "editTitle": "Edit realm {{name}}",
    "formDescription": "The role is set again from the mapping each time a user signs in.",
    "editNamed": "Edit {{name}}",
    "created": "Created realm {{name}}.",
    "saved": "Saved realm {{name}}.",
    "deleted": "Deleted realm {{name}}.",
    "deleteTitle": "Delete realm {{name}}?",
    "deleteDescription": "Users of this realm can no longer sign in, and their sessions and API tokens stop working. The users stay; creating a realm with the same name lets them in again.",
    "test": "Test",
    "testNamed": "Test {{name}}",
    "testTitle": "Test realm {{name}}",
    "testDescription": "Checks each step of signing in as a directory user. No user or session is created.",
    "steps": "Steps",
    "step": "Step",
    "result": "Result",
    "message": "Message",
    "ok": "OK",
    "failed": "Failed",
    "stepNames": {
      "connect": "Connect",
      "service_bind": "Service account bind",
      "search": "Find the user",
      "user_bind": "User bind",
      "groups": "Groups",
      "discovery": "Discovery",
      "jwks": "Signing keys"
    },
    "dn": "DN",
    "groupsFound": "Groups",
    "noRole": "No role: sign-in would be refused.",
    "errors": {
      "local": "local is reserved for local users.",
      "taken": "A realm with this name already exists.",
      "url": "Use ldap://host[:port] (with StartTLS) or ldaps://host[:port].",
      "required": "Enter a value.",
      "userFilter": "Include {username} exactly once.",
      "bindDn": "Enter a bind DN to use a bind password.",
      "bindPasswordNeeded": "Enter a bind password for the bind DN, or clear the bind DN to search anonymously.",
      "bindPasswordAgain": "Enter the bind password again: the URL, bind DN or CA certificate changed.",
      "clearNeedsNoBindDn": "To clear the saved password, also clear the bind DN.",
      "clearSaved": "The bind DN is empty: choose \"Clear the saved password\" to search anonymously.",
      "timeout": "Use a whole number of seconds from 1 to 30.",
      "group": "Enter a group DN or remove the row.",
      "issuer": "Use https://host[:port][/path] without a query or fragment.",
      "scopes": "Include openid.",
      "clientSecretAgain": "Enter the client secret again: the issuer, client ID or CA certificate changed."
    },
    "createOidcTitle": "Create an OIDC realm",
    "issuer": "Issuer",
    "issuerHint": "https:// URL exactly as the identity provider reports it, including any trailing slash.",
    "clientId": "Client ID",
    "clientSecret": "Client secret",
    "clientSecretHint": "Leave empty for a public client (PKCE only).",
    "clientSecretSaved": "A secret is saved. Leave this empty to keep it.",
    "clearClientSecret": "Clear the saved secret (public client)",
    "scopes": "Scopes",
    "scopesHint": "Separated by spaces. openid is required.",
    "usernameClaim": "Username claim",
    "groupsClaim": "Groups claim",
    "groupsClaimHint": "Claim with the user's groups as a list of strings.",
    "redirectUri": "Redirect URI",
    "redirectUriHint": "Register this address with the identity provider. Each node address you sign in through needs its own. Behind a reverse proxy, register the address the proxy passes as the Host.",
    "copyRedirectUri": "Copy the redirect URI",
    "oidcCaPemHint": "Trust only this CA for the identity provider's certificate. Leave empty to use the system roots.",
    "oidcGroupsHint": "Group values from the groups claim match without regard to case. A user in several groups gets the highest role.",
    "oidcTestDescription": "Checks the identity provider's discovery document and signing keys. No one signs in."
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
    "membersNote": "Voters keep the cluster state. A learner is a node that is still joining. A witness votes but runs no VMs.",
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
  "remotes": {
    "pageHint": "Other opLite clusters this cluster can see. The status is collected every 30 seconds. What you can do on a remote cluster also depends on the role of the user who owns its API token there.",
    "empty": "No remote clusters.",
    "add": "Add",
    "addTitle": "Add a remote cluster",
    "addDescription": "Register another opLite cluster with its address, the fingerprint of its CA and an API token created on it.",
    "editTitle": "Edit remote cluster {{name}}",
    "editNamed": "Edit {{name}}",
    "refreshNamed": "Refresh {{name}}",
    "url": "URL",
    "urlHint": "https://host[:port] of a node of the remote cluster.",
    "getCa": "Get the CA",
    "fingerprint": "CA fingerprint",
    "confirmFingerprint": "This matches the CA fingerprint on the Cluster page of the remote cluster.",
    "token": "API token",
    "tokenHint": "Create it on the API tokens page of the remote cluster. Its user's role there limits what this cluster can do.",
    "tokenSaved": "A token is saved. Leave this empty to keep it.",
    "cluster": "Cluster",
    "version": "Version",
    "nodes": "Nodes",
    "nodesValue": "{{online}} / {{total}} online",
    "vms": "VMs",
    "vmsValue": "{{running}} / {{total}} running",
    "cpus": "CPUs",
    "memory": "Memory",
    "memoryValue": "{{gib}} GiB",
    "reachable": "Reachable",
    "unreachable": "Unreachable",
    "created": "Added remote cluster {{name}}.",
    "saved": "Saved remote cluster {{name}}.",
    "deleted": "Deleted remote cluster {{name}}.",
    "refreshFailed": "Could not refresh {{name}}: {{error}}",
    "deleteTitle": "Delete remote cluster {{name}}?",
    "deleteDescription": "This cluster stops showing it. Nothing changes on the remote cluster, and its API token stays there until you delete it.",
    "backToList": "Back to remote clusters",
    "vmsTitle": "Virtual machines on {{name}}",
    "actionRequested": "{{action}} requested for {{name}} on {{remote}}.",
    "errors": {
      "probe": "Could not get the CA: {{error}}",
      "probeFirst": "Get the CA of this URL first.",
      "confirm": "Check the fingerprint against the remote cluster and confirm it.",
      "token": "Enter an API token of the remote cluster.",
      "tokenAgain": "Enter the API token again: the URL or CA changed."
    }
  },
  "vmForm": {
    "createTitle": "Create a virtual machine",
    "editTitle": "Edit {{name}}",
    "runningNote": "The VM is running: its node, disks, and the number, order and MAC addresses of its interfaces cannot change, and if an interface has security groups, only the groups of the interfaces can change. Security groups apply right away; other changes apply from the next start.",
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
    "nicsRunning": "Interfaces cannot be added or removed, and their MAC addresses cannot change, while the VM is running.",
    "securityGroups": "Security groups",
    "nicGroups": "Security groups of interface {{n}}",
    "noSecurityGroups": "No security groups. Create them on the Security groups page.",
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
      "keep": "From 1 to 100.",
      "securityGroups": "Choose at most 8 security groups."
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
    "scheduleSharedLvm": "{{volume}} is in a shared LVM pool, so scheduled snapshots will fail.",
    "ha": "High availability (HA)",
    "haHint": "Runs only on nodes with fencing on. If its node loses the cluster majority, the node stops the VM.",
    "haNodeOff": "{{node}} has fencing off and cannot run HA VMs.",
    "haNoNode": "No node has fencing on, so HA VMs cannot run.",
    "haRunning": "Turn on HA while the VM is stopped."
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
    "witness": "Witness",
    "failedVms": "Failed virtual machines",
    "recentTasks": "Recent tasks",
    "allTasks": "All tasks",
    "noLeader": "No leader",
    "noNodes": "No nodes.",
    "mixedVersions": "Nodes run different opLite versions ({{versions}}). Finish the upgrade on every node.",
    "subscription": {
      "none": "This cluster has no subscription.",
      "invalid": "The subscription key does not match this build.",
      "expired": "The subscription expired on {{date}}.",
      "over_limit": "The cluster has {{used}} nodes, but the subscription covers {{limit}}.",
      "expiring": "The subscription expires on {{date}}.",
      "manage": "Subscription settings",
      "dismiss": "Hide for 7 days"
    }
  },
  "settings": {
    "updates": "Updates",
    "channel": "Update channel",
    "channelHint": "Every node gets opLite packages from this channel. Testing gets new versions before stable.",
    "channels": {
      "stable": "Stable",
      "testing": "Testing",
      "enterprise": "Enterprise"
    },
    "saved": "Settings saved.",
    "adminOnly": "Only administrators can change the settings.",
    "errors": {
      "busy": "Another settings change is in progress. Try again."
    },
    "enterpriseHint": "Enterprise needs an active subscription.",
    "subscription": {
      "title": "Subscription",
      "states": {
        "none": "No subscription",
        "active": "Active",
        "expired": "Expired",
        "invalid": "Invalid key",
        "over_limit": "Over the node limit"
      },
      "development": "Development key",
      "customer": "Customer",
      "level": "Level",
      "levels": {
        "standard": "Standard",
        "premium": "Premium"
      },
      "nodes": "Nodes",
      "expires": "Expires",
      "key": "Subscription key",
      "keyHint": "Paste the key you received. It starts with opl1.",
      "apply": "Apply key",
      "applied": "Subscription key applied.",
      "remove": "Remove key",
      "removeTitle": "Remove the subscription key?",
      "removeDescription": "The cluster will have no subscription. Nodes on the enterprise channel stop getting updates.",
      "removed": "Subscription key removed."
    },
    "telemetry": {
      "title": "Telemetry",
      "intro": "When on, the leader sends a usage report once a day: counts and versions only, without names, addresses or keys.",
      "on": "On",
      "off": "Off",
      "endpoint": "Endpoint",
      "buildDefault": "Not set (the build's default endpoint is used if it has one)",
      "endpointHint": "An https:// address. Leave it empty to use the build default.",
      "saveEndpoint": "Save endpoint",
      "id": "Report ID",
      "lastSent": "Last sent",
      "never": "Never",
      "lastError": "Last error",
      "enable": "Turn on…",
      "disable": "Turn off",
      "resetId": "New report ID",
      "previewTitle": "Turn on telemetry",
      "previewDescription": "This is the report the cluster would send now. Nothing else is sent.",
      "confirmEnable": "Turn on",
      "enabled": "Telemetry turned on.",
      "disabled": "Telemetry turned off.",
      "idReset": "New report ID made."
    }
  },
  "audit": {
    "adminOnly": "Only administrators can read the audit log.",
    "refresh": "Refresh",
    "range": "Time range",
    "ranges": {
      "1h": "Last hour",
      "24h": "Last 24 hours",
      "7d": "Last 7 days"
    },
    "user": "User",
    "action": "Action",
    "apply": "Apply",
    "time": "Time",
    "ip": "IP",
    "target": "Target",
    "status": "Status",
    "refreshFailed": "Could not read the audit log again. {{error}}",
    "empty": "No audit entries in this range.",
    "unreachable": "No audit entries from these nodes (unreachable): {{nodes}}",
    "disabled": "The audit log is turned off on these nodes: {{nodes}}",
    "truncated": "Showing the newest {{count}} entries. More match; narrow the range or the filters.",
    "entryTitle": "Audit entry"
  }
}
```

`web/src/i18n/i18n.test.ts`에서

```ts
  expect(errorMessage(t, new ApiError(409, "backup_not_ready", 'backup "b" is not ready'))).toBe(
    "The backup is not ready. Only finished backups can be restored.",
  );
```

을 다음으로 바꾼다:

```ts
  expect(errorMessage(t, new ApiError(409, "backup_not_ready", 'backup "b" is not ready'))).toBe(
    "The backup is not ready. Only finished backups can be restored or copied.",
  );
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
    "vni_in_use": "다른 네트워크가 이미 이 VNI를 씁니다.",
    "node_online": "온라인인 노드입니다. 그 노드에서 oplctl cluster leave를 실행하세요.",
    "pool_not_on_node": "지금 접속한 노드에서 쓸 수 없는 스토리지 풀입니다.",
    "insufficient_storage": "노드에 이 파일을 받을 빈 공간이 부족합니다.",
    "last_admin": "마지막 관리자는 관리자 역할을 내려놓을 수 없습니다.",
    "cannot_delete_self": "자기 계정은 지울 수 없습니다.",
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "pool_unavailable": "{{message}}",
    "vm_busy": "이 VM에 다른 작업(시작·정지, 스냅샷, 되돌리기, 복제, 마이그레이션)이 진행 중입니다. 끝나면 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2이거나, 모두 LVM-thin 풀에 있거나, 모두 ZFS 풀에 있거나, 모두 Ceph RBD 풀에 있는 VM만 스냅샷을 만들 수 있습니다. LVM-thin·ZFS·Ceph RBD 풀의 디스크 스냅샷에는 메모리를 담을 수 없고, ZFS 풀의 디스크는 가장 최근 스냅샷으로만 되돌릴 수 있습니다.",
    "vm_has_snapshots": "VM에 스냅샷이 있습니다. 먼저 스냅샷을 지우세요.",
    "volume_in_snapshot": "VM 스냅샷이 아직 이 볼륨을 씁니다. 먼저 그 스냅샷을 지우세요.",
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "node_offline": "VM이 있는 노드가 오프라인입니다.",
    "too_many_attempts": "시도가 너무 많습니다. 잠시 뒤에 다시 하세요.",
    "machine_arch_mismatch": "선택한 노드의 아키텍처와 머신 종류가 맞지 않습니다.",
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "대상 노드가 위트니스이거나 오프라인이거나 유지보수 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
    "task_failed": "작업 오류: {{message}}",
    "internal": "서버 오류: {{message}}",
    "ha_unsupported": "노드의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "firewall_unsupported": "노드에 방화벽(nftables)이 없어 보안 그룹을 건 VM을 돌릴 수 없습니다.",
    "security_group_in_use": "이 보안 그룹을 쓰는 VM 인터페이스가 있습니다. 먼저 VM에서 빼세요.",
    "backup_busy": "이 백업의 작업(백업, 복원, 복제, 삭제)이 진행 중입니다. 끝난 뒤 다시 시도하세요.",
    "backup_not_ready": "백업이 준비되지 않았습니다. 다 만든 백업만 복원하거나 복제할 수 있습니다.",
    "job_running": "백업 작업이 이미 실행 중입니다. 실행이 끝난 뒤 다시 시도하세요.",
    "job_disabled": "꺼 둔 백업 작업입니다. 먼저 \"일정대로 실행\"을 켜세요.",
    "invalid_ova": "쓸 수 있는 OVA가 아닙니다: {{message}}",
    "import_busy": "가져오는 중이거나 이 가져오기의 다른 작업이 끝나지 않았습니다.",
    "import_done": "이미 가져온 VM입니다.",
    "import_failed": "가져오기를 실행할 수 없습니다: {{message}}",
    "untrusted_certificate": "ESXi 인증서 지문을 확인하고 믿기로 해야 계속할 수 있습니다.",
    "fingerprint_mismatch": "믿기로 한 뒤에 ESXi 인증서가 바뀌었습니다. 다시 연결해 새 지문을 확인하세요.",
    "esxi_auth": "ESXi가 사용자 이름이나 비밀번호를 거절했습니다. 확인하고 다시 시도하세요.",
    "esxi_unreachable": "ESXi 호스트에 연결할 수 없습니다({{message}}).",
    "version_too_old": "노드의 opLite가 클러스터보다 오래됐습니다. 먼저 업그레이드하세요: {{message}}",
    "updates_unsupported": "이 노드는 업데이트를 확인하지 않습니다.",
    "upgrade_in_progress": "업그레이드가 진행 중입니다: {{message}}",
    "cluster_upgrading": "클러스터를 업그레이드하는 중입니다: {{message}}",
    "subscription_required": "enterprise 채널에는 유효한(active) 구독이 필요합니다.",
    "invalid_subscription_key": "구독 키가 올바르지 않습니다: {{message}}",
    "telemetry_endpoint_required": "먼저 텔레메트리 끝점을 정하세요. 이 빌드에는 기본 끝점이 없습니다.",
    "no_role": "디렉터리 계정에 opLite 역할이 없습니다. 관리자에게 그룹의 역할 매핑을 요청하세요.",
    "realm_unavailable": "이 영역의 디렉터리에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요.",
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요.",
    "sso_failed": "SSO 로그인에 실패했습니다. 다시 시도하거나 관리자에게 영역 확인을 요청하세요.",
    "remote_unreachable": "원격 클러스터에 닿지 못했습니다({{message}}).",
    "remote_fingerprint_mismatch": "원격 클러스터의 CA 지문이 다릅니다. CA를 다시 받아 확인하세요.",
    "remote_is_local": "그 주소는 이 클러스터입니다. 다른 클러스터를 추가하세요.",
    "remote_unauthorized": "원격 클러스터가 API 토큰을 거절했습니다. 그곳에 토큰이 아직 있는지 확인하세요."
  },
  "nav": {
    "dashboard": "대시보드",
    "vms": "가상 머신",
    "imports": "가져오기",
    "volumes": "볼륨",
    "pools": "스토리지 풀",
    "backups": "백업",
    "backupJobs": "백업 작업",
    "networks": "네트워크",
    "securityGroups": "보안 그룹",
    "nodes": "노드",
    "tasks": "작업",
    "audit": "감사 로그",
    "users": "사용자",
    "realms": "인증 영역",
    "tokens": "API 토큰",
    "cluster": "클러스터",
    "remotes": "원격 클러스터",
    "settings": "설정"
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
    "submit": "로그인",
    "version": "opLite {{version}}",
    "realm": "인증 영역",
    "localRealm": "로컬 사용자",
    "or": "또는",
    "sso": {
      "signIn": "{{name}}(으)로 로그인",
      "no_role": "IdP 계정에 opLite 역할이 없습니다. 관리자에게 그룹의 역할 매핑을 요청하세요.",
      "realm_unavailable": "이 영역의 IdP에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요."
    }
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
    "doneWarning": "{{label}}: 완료(경고). {{warning}}",
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
      "vm-clone": "{{target}} 복제",
      "vm-migrate": "{{target}} 마이그레이션",
      "vm-ha-recover": "{{target}} HA 복구 (노드 {{from}}에서)",
      "vm-backup": "{{target}} 백업",
      "backup-delete": "백업 {{target}} 삭제",
      "backup-restore": "백업을 {{target}}(으)로 복원",
      "backup-replicate": "백업 {{target}}을(를) {{remote}}(으)로 복제",
      "vm-import": "VM {{target}} 가져오기",
      "vm-import-delete": "가져오기 {{target}} 삭제",
      "node-update-check": "{{target}} 업데이트 확인",
      "node-upgrade": "{{target}} 업그레이드"
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
      "busy": "VM이 시작하거나 정지하는 중입니다.",
      "migrating": "VM을 마이그레이션하는 중에는 할 수 없습니다.",
      "snapshotPending": "스냅샷이나 복제 작업 중에는 할 수 없습니다."
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
      "failed": "실패",
      "migrating": "마이그레이션 중"
    },
    "migratingTo": "{{from}} → {{to}}",
    "action": {
      "start": "시작",
      "stop": "정지",
      "force-stop": "강제 정지",
      "reboot": "재시작",
      "console": "콘솔",
      "delete": "삭제",
      "edit": "편집",
      "clone": "복제",
      "migrate": "마이그레이션"
    },
    "notFound": "{{name}} VM이 없습니다",
    "backToList": "가상 머신 목록으로",
    "tab": {
      "summary": "요약",
      "hardware": "하드웨어",
      "console": "콘솔",
      "snapshots": "스냅샷",
      "backups": "백업",
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
    "noNics": "네트워크 인터페이스가 없습니다.",
    "ha": "HA"
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
    "busy": "VM이 시작·정지·마이그레이션하는 중입니다. 끝나면 스냅샷을 만들고 되돌리고 지울 수 있습니다.",
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
    "busy": "VM이 시작·정지·마이그레이션하는 중입니다. 끝난 뒤 다시 시도하세요.",
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
  "backups": {
    "vm": "VM",
    "pool": "백업 풀",
    "job": "작업",
    "jobDeleted": "삭제한 작업",
    "created": "만든 시각",
    "size": "크기",
    "origin": "{{cluster}}에서 받음",
    "originHint": "클러스터 {{cluster}}의 백업 {{backup}} 사본",
    "originJobHint": "클러스터 {{cluster}}의 백업 {{backup}}(작업 {{job}}) 사본",
    "replicas": "원격 사본",
    "replicaState": {
      "copying": "복제 중",
      "ready": "준비됨",
      "failed": "실패"
    },
    "replicaTitle": "{{remote}} / {{pool}}: {{state}}",
    "replicaError": "{{remote}} / {{pool}}: {{error}}",
    "replicate": "원격으로 복제",
    "replicateNamed": "{{name}}을(를) 원격 클러스터로 복제",
    "replicateDescription": "백업 파일을 원격 클러스터의 백업 풀로 보냅니다. 그 사본은 원격 클러스터에서 복원할 수 있는 보통 백업입니다. 이 백업을 지우면 사본도 지웁니다.",
    "replicateSubmit": "복제",
    "remote": "원격 클러스터",
    "chooseRemote": "원격 클러스터를 고르세요",
    "remotePool": "원격 클러스터의 백업 풀",
    "noRemotes": "원격 클러스터가 없습니다. 원격 클러스터 화면에서 추가하세요.",
    "noRemotePools": "원격 클러스터에 백업을 담는 스토리지 풀이 없습니다.",
    "empty": "백업이 없습니다.",
    "phase": {
      "pending": "만드는 중",
      "ready": "준비됨",
      "failed": "실패",
      "receiving": "받는 중"
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
    "deleteDescription": "백업 풀에서 백업 파일을 지우고, 원격 클러스터로 보낸 사본도 지웁니다. 되돌릴 수 없습니다.",
    "op": {
      "create": "{{vm}} 백업",
      "restore": "{{backup}}을(를) {{name}}(으)로 복원",
      "delete": "백업 {{name}} 삭제"
    },
    "errors": {
      "exists": "같은 이름의 백업이 이미 있습니다.",
      "vmExists": "같은 이름의 VM이 이미 있습니다.",
      "pool": "스토리지 풀을 고르세요.",
      "remote": "원격 클러스터를 고르세요."
    }
  },
  "backupJobs": {
    "pageHint": "정한 시각에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛 백업을 지웁니다. 시각은 UTC입니다. 클러스터 리더가 작업을 실행하며 노드마다 동시에 도는 백업 수에 제한이 있습니다.",
    "empty": "백업 작업이 없습니다.",
    "vms": "VM",
    "schedule": "일정(UTC)",
    "keep": "보존",
    "nextRun": "다음 실행",
    "lastRun": "마지막 실행",
    "everyDay": "매일",
    "day": {
      "mon": "월",
      "tue": "화",
      "wed": "수",
      "thu": "목",
      "fri": "금",
      "sat": "토",
      "sun": "일"
    },
    "keepAll": "모두 남김",
    "keepUnit": {
      "last": "최근 {{n}}개",
      "daily": "일별 {{n}}",
      "weekly": "주별 {{n}}",
      "monthly": "월별 {{n}}"
    },
    "state": {
      "running": "실행 중",
      "enabled": "켜짐",
      "disabled": "꺼짐"
    },
    "waiting_one": "VM {{count}}개 대기",
    "waiting_other": "VM {{count}}개 대기",
    "result": "성공 {{ok}}, 실패 {{failed}}",
    "runNow": "지금 실행",
    "runNamed": "{{name}} 지금 실행",
    "runRequested": "{{name}}: 스케줄러가 다음 확인 때 실행을 시작합니다.",
    "editNamed": "{{name}} 편집",
    "deleteNamed": "백업 작업 {{name}} 삭제",
    "createTitle": "백업 작업 만들기",
    "createDescription": "고른 요일에 VM들을 백업 풀에 백업하고, 보존 규칙이 더는 남기지 않는 백업을 지웁니다.",
    "editTitle": "{{name}} 편집",
    "editDescription": "바꾼 내용은 다음 실행부터 적용됩니다. 진행 중인 실행은 끝까지 갑니다.",
    "target": "백업할 VM",
    "targets": {
      "all": "모든 VM",
      "names": "이름으로 고르기",
      "labels": "라벨로 고르기"
    },
    "names": "VM 이름",
    "namesHint": "쉼표로 나눠 적습니다.",
    "labels": "라벨",
    "labelsHint": "KEY=VALUE를 쉼표로 나눠 적습니다. VM에 모든 라벨이 있어야 합니다.",
    "at": "시각(UTC)",
    "atHint": "UTC 기준 HH:MM. 예: 03:00",
    "days": "요일",
    "daysHint": "아무 요일도 고르지 않으면 매일 실행합니다.",
    "retention": "보존 규칙",
    "retentionHint": "최근 백업(최근)을 남기고, 그다음 날·ISO 주·달마다 가장 새 백업을 하나씩 남깁니다. VM마다 따로 셉니다. 모두 0이면 지우지 않습니다. 실패한 백업은 뒤에 성공하면 지웁니다.",
    "keepLabel": {
      "last": "최근",
      "daily": "일별",
      "weekly": "주별",
      "monthly": "월별"
    },
    "enabled": "일정대로 실행",
    "replicate": "원격 복제",
    "replicateLabel": "원격 클러스터로 복제",
    "noReplicate": "복제하지 않음",
    "replicateHint": "작업이 만든 백업마다 원격 클러스터의 이 백업 풀로 사본을 보냅니다. 실패한 복제는 다음 실행 때 다시 합니다.",
    "created": "백업 작업 {{name}}을(를) 만들었습니다.",
    "saved": "백업 작업 {{name}}을(를) 저장했습니다.",
    "deleted": "백업 작업 {{name}}을(를) 삭제했습니다.",
    "deleteTitle": "백업 작업 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "작업이 더는 실행되지 않습니다. 작업이 만든 백업은 남습니다.",
    "errors": {
      "exists": "같은 이름의 백업 작업이 있습니다.",
      "names": "VM 이름을 하나 이상 적으세요.",
      "labels": "KEY=VALUE 라벨을 하나 이상 적으세요.",
      "at": "00:00부터 23:59 사이의 시각을 적으세요.",
      "keep": "0 이상의 정수를 적으세요."
    }
  },
  "imports": {
    "pageHint": "OVA 파일이나 ESXi·vCenter에서 바로 VMware VM을 opLite로 가져옵니다. 원본 VM에서 제안한 설정을 확인한 뒤 실행하면 꺼진 VM이 만들어집니다.",
    "empty": "가져오기가 없습니다.",
    "upload": "OVA 올리기",
    "uploadTitle": "OVA 올리기",
    "uploadDescription": "파일은 지금 접속한 노드({{node}})로 올라가고 가져오기를 마치거나 지울 때까지 그 노드에 남습니다. 가져오기도 그 노드에서 돕니다.",
    "file": "OVA 파일",
    "uploaded": "{{name}}을(를) 올렸습니다. 제안한 설정을 확인하고 가져오기를 실행하세요.",
    "review": "확인",
    "noResponse": "{{name}}: 서버 응답이 없습니다. 가져오기 목록에서 결과를 확인하세요.",
    "sourceVm": "원본 VM",
    "vm": "VM",
    "vmGone": "삭제한 VM",
    "phase": {
      "ready": "준비됨",
      "importing": "가져오는 중",
      "done": "완료",
      "failed": "실패"
    },
    "notFound": "가져오기 {{name}}을(를) 찾을 수 없습니다",
    "source": "원본",
    "fileName": "파일",
    "guestOs": "게스트 OS",
    "cpuValue": "vCPU {{vcpus}}개, 소켓당 코어 {{cores}}개",
    "diskFile": "OVA 안의 파일",
    "capacity": "크기",
    "controller": "컨트롤러",
    "adapter": "어댑터",
    "warnings": "경고",
    "lastError": "마지막 실행이 실패했습니다: {{error}}",
    "failedError": "원본을 쓸 수 없습니다: {{error}}",
    "importing": "가져오는 중입니다. 진행 상황은 작업 화면에 있습니다.",
    "imported": "가져온 VM",
    "settings": "VM 설정",
    "settingsHint": "원본 VM에서 제안한 값입니다. 필요한 것을 고치세요.",
    "vmName": "새 VM 이름",
    "pool": "디스크를 둘 스토리지 풀",
    "poolHint": "가져오기가 도는 {{node}}에서 쓸 수 있는 풀만 보입니다.",
    "sockets": "소켓",
    "cores": "소켓당 코어",
    "diskBus": "디스크 버스",
    "nicModel": "NIC 모델",
    "networks": "네트워크 매핑",
    "networksHint": "원본 VM이 쓰는 네트워크마다 opLite 네트워크를 고르세요.",
    "chooseNetwork": "네트워크를 고르세요",
    "keepMacs": "원본 MAC 주소 유지",
    "keepMacsHint": "가져온 VM을 켜기 전에 VMware의 원본 VM을 끄세요. 끄면 인터페이스가 새 MAC 주소를 받습니다.",
    "run": "가져오기",
    "deleteNamed": "가져오기 {{name}} 삭제",
    "deleteTitle": "가져오기 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "{{node}}에 있는 가져오기 파일을 지웁니다. 원본 VM과 이미 가져온 VM은 남습니다.",
    "forceHint": "{{error}} 파일을 그 노드에 남기고 가져오기만 지우려면 oplctl import delete {{name}} --force를 실행하세요.",
    "codes": {
      "node_offline": "가져오기를 하는 노드가 오프라인이거나 클러스터에서 빠졌습니다.",
      "mac_in_use": "다른 VM이 원본 MAC 주소를 쓰고 있습니다. \"원본 MAC 주소 유지\"를 끄면 새 MAC 주소를 받습니다."
    },
    "errors": {
      "reserved": "ova와 esxi는 예약된 이름입니다.",
      "exists": "같은 이름의 가져오기가 이미 있습니다.",
      "vmExists": "같은 이름의 VM이 이미 있습니다.",
      "volumeExists": "가져오기가 만들 볼륨(이름-disk0, …)이 이미 있습니다.",
      "pool": "스토리지 풀을 고르세요.",
      "network": "네트워크를 고르세요.",
      "poolNotOnNode": "가져오기를 하는 노드에서 쓸 수 없는 스토리지 풀입니다.",
      "poolUnavailable": "가져오기를 하는 노드에서 준비되지 않은 스토리지 풀입니다.",
      "sockets": "소켓 수는 1에서 65535 사이여야 합니다.",
      "nameTaken": "이미 쓰는 이름입니다: {{message}}",
      "host": "ESXi나 vCenter 호스트를 입력하세요.",
      "port": "포트는 1에서 65535 사이여야 합니다.",
      "user": "사용자 이름을 입력하세요.",
      "datacenter": "데이터센터를 입력하세요.",
      "vm": "VM을 고르세요."
    },
    "esxi": "ESXi에서 가져오기",
    "esxiTitle": "ESXi에서 가져오기",
    "esxiDescription": "지금 접속한 노드({{node}})에서 연결하고 가져오기도 그 노드에서 돕니다. 비밀번호는 그 노드의 메모리에만 있으므로 그곳의 opld가 다시 시작하면 가져오기를 다시 만들어야 합니다.",
    "host": "호스트",
    "port": "포트",
    "user": "사용자",
    "password": "비밀번호",
    "datacenter": "데이터센터",
    "datacenterHint": "ESXi 호스트는 ha-datacenter, vCenter는 데이터센터 이름입니다.",
    "connect": "연결",
    "untrusted": "{{host}}의 인증서는 아직 믿지 않은 인증서입니다. 계속하기 전에 이 SHA-256 지문이 ESXi 호스트의 인증서와 같은지 확인하세요.",
    "fingerprint": "인증서 지문(SHA-256)",
    "trust": "이 지문을 믿고 계속",
    "vms": "가상 머신",
    "noVms": "데이터스토어의 맨 위 폴더에서 VM을 찾지 못했습니다.",
    "datastore": "데이터스토어",
    "locked": "VM이 켜져 있는 것 같습니다(잠금 파일이 있음). 가져오기 전에 끄세요.",
    "next": "다음",
    "back": "뒤로",
    "chosenVm": "원본",
    "esxiSource": "ESXi",
    "diskFileEsxi": "데이터스토어의 파일",
    "credentialsLost": "ESXi 비밀번호가 이제 이 노드에 없습니다(opld가 다시 시작했거나 요청이 끊겼습니다). 이 가져오기를 지우고 다시 만드세요."
  },
  "migrate": {
    "title": "{{name}} 마이그레이션",
    "description": "실행 중인 VM을 멈추지 않고 다른 노드로 옮깁니다. 디스크가 모두 공유 스토리지 풀에 있어야 하고, 두 노드의 QEMU 버전과 CPU가 같아야 합니다.",
    "node": "대상 노드",
    "noTarget": "VM의 아키텍처를 가진 다른 온라인 노드가 없습니다.",
    "submit": "마이그레이션",
    "op": "{{vm}}을(를) {{node}}(으)로 마이그레이션"
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
    "sharing": "공유",
    "sharedState": "공유",
    "localState": "노드 로컬",
    "sharedNamed": "공유 스토리지: {{name}}",
    "makeSharedTitle": "{{name}}을(를) 공유 스토리지로 바꿀까요?",
    "makeSharedDescription": "풀의 모든 노드가 {{path}}에서 같은 파일을 볼 때만 바꾸세요(예: opLite 밖에서 마운트한 NFS 공유). 그러면 이 풀에 디스크가 있는 VM을 풀의 어느 노드에서나 켜고 그 노드로 마이그레이션할 수 있습니다.",
    "makeLocalTitle": "{{name}}을(를) 노드 로컬로 바꿀까요?",
    "makeLocalDescription": "각 볼륨은 만든 노드에 있는 것으로 돌아갑니다. 이 풀의 볼륨을 쓰는 VM은 그 노드에 있어야 하고 마이그레이션 중이 아니어야 합니다.",
    "makeShared": "공유로 바꾸기",
    "makeLocal": "노드 로컬로 바꾸기",
    "madeShared": "{{name}}이(가) 공유 스토리지가 되었습니다.",
    "madeLocal": "{{name}}이(가) 노드 로컬이 되었습니다.",
    "sharingErrors": {
      "pool_in_use": "이 풀의 볼륨을 만든 노드가 아닌 곳에서 쓰거나 마이그레이션 중인 VM이 있습니다: {{message}}"
    },
    "nodesHint": "모두 비워 두면 모든 노드에서 씁니다.",
    "created": "스토리지 풀 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "스토리지 풀 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "풀만 지웁니다. 디렉터리나 내보내기의 파일, Ceph 풀의 이미지와 공유 볼륨 그룹의 논리 볼륨은 남습니다.",
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
  "networks": {
    "kind": "종류",
    "kinds": {
      "bridge": "브리지",
      "vxlan": "VXLAN"
    },
    "kindHint": {
      "bridge": "노드에 이미 있는 Linux 브리지입니다. 이 네트워크의 VM은 그 브리지가 있는 노드에서만 실행됩니다.",
      "vxlan": "opld가 모든 노드에 브리지 oplv<VNI>와 VXLAN 장치를 만들고 UDP 4789로 잇습니다."
    },
    "bridge": "브리지",
    "vlanVni": "VLAN/VNI",
    "vni": "VNI",
    "vniHint": "1–16777215. 다른 네트워크가 쓰지 않는 값이어야 합니다.",
    "mtu": "MTU",
    "mtuOptional": "MTU(선택)",
    "mtuHint": "1280–9000. 기본값은 1450입니다(underlay 1500바이트에서 VXLAN 머리 50바이트를 뺀 값).",
    "vlanOptional": "VLAN 태그(선택)",
    "vlanHint": "1–4094. 브리지가 VLAN을 알아야 합니다(vlan_filtering 1).",
    "nodes": "브리지가 있는 노드",
    "vms": "VM",
    "empty": "네트워크가 없습니다.",
    "userNote": "user 네트워크(QEMU NAT)는 늘 쓸 수 있으며 목록에 나오지 않습니다.",
    "noNode": "이 브리지가 있는 노드가 없습니다",
    "noVlanFiltering": "{{nodes}}에서 VLAN 필터링이 꺼져 있습니다",
    "createTitle": "네트워크 만들기",
    "createDescription": "노드의 Linux 브리지나 노드들을 잇는 VXLAN 네트워크에 VM 인터페이스를 연결합니다.",
    "created": "네트워크 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "네트워크 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "노드에 직접 만든 브리지는 바꾸지 않습니다. VXLAN 네트워크는 opld가 노드에서 그 브리지와 VXLAN 장치를 지웁니다.",
    "deleted": "네트워크 {{name}}을(를) 삭제했습니다.",
    "inUse": "먼저 VM 인터페이스를 빼세요.",
    "errors": {
      "reserved": "user는 NAT에 예약된 이름입니다.",
      "bridge": "/, :, 공백 없이 1–15자로 쓰세요.",
      "vlan": "1부터 4094까지의 숫자를 쓰세요.",
      "managed": "oplv100, oplx100 같은 이름은 VXLAN 네트워크용으로 예약돼 있습니다.",
      "vni": "1부터 16777215까지의 숫자를 쓰세요.",
      "mtu": "1280부터 9000까지의 숫자를 쓰세요."
    }
  },
  "securityGroups": {
    "note": "VM 인터페이스에 거는 방화벽 규칙입니다. nftables가 있는 노드가 실행 중인 VM에도 곧바로 적용합니다.",
    "rules": "규칙",
    "description": "설명",
    "descriptionOptional": "설명(선택)",
    "vms": "VM",
    "empty": "보안 그룹이 없습니다.",
    "inUse": "먼저 VM 인터페이스에서 빼세요.",
    "createTitle": "보안 그룹 만들기",
    "createDescription": "빈 그룹을 만듭니다. 규칙은 다음 화면에서 적습니다.",
    "created": "보안 그룹 {{name}}을(를) 만들었습니다.",
    "deleteTitle": "보안 그룹 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 그룹을 쓰는 VM 인터페이스가 없으므로 트래픽은 바뀌지 않습니다.",
    "deleted": "보안 그룹 {{name}}을(를) 삭제했습니다.",
    "notFound": "보안 그룹 {{name}}을(를) 찾을 수 없습니다",
    "saved": "보안 그룹 {{name}}을(를) 저장했습니다.",
    "order": "규칙은 위에서부터 보고 처음 맞는 것을 따릅니다. 그룹을 건 인터페이스는 맞는 규칙이 없으면 들어오는 것은 막고 나가는 것은 보냅니다. 그룹이 여럿이면 앞 그룹의 규칙을 먼저 봅니다.",
    "always": "늘 허용: 허용한 연결의 응답, ARP, IPv6 이웃 탐색, DHCP 응답. VM은 자기 MAC 주소로만 보낼 수 있습니다.",
    "direction": "방향",
    "action": "동작",
    "protocol": "프로토콜",
    "ports": "포트",
    "remote": "원격 CIDR",
    "directionN": "규칙 {{n}}의 방향",
    "actionN": "규칙 {{n}}의 동작",
    "protocolN": "규칙 {{n}}의 프로토콜",
    "portsN": "규칙 {{n}}의 포트",
    "remoteN": "규칙 {{n}}의 원격 CIDR",
    "in": "들어옴(VM으로)",
    "out": "나감(VM에서)",
    "accept": "허용",
    "drop": "차단",
    "any": "모두",
    "anyPort": "모든 포트",
    "anywhere": "어디든",
    "moveUp": "규칙 {{n}}을(를) 위로",
    "moveDown": "규칙 {{n}}을(를) 아래로",
    "removeRule": "규칙 {{n}} 빼기",
    "addRule": "규칙 더하기",
    "noRules": "규칙이 없습니다: 들어오는 것은 막고 나가는 것은 보냅니다.",
    "errors": {
      "description": "200자 이하로 입력하세요.",
      "ports": "22 같은 포트나 8000-8080 같은 범위로 입력하세요(1–65535).",
      "remote": "10.0.0.0/8이나 2001:db8::/32 같은 CIDR로 입력하세요.",
      "icmpRemote": "ICMP 규칙에는 IPv4 주소만 씁니다.",
      "icmpv6Remote": "ICMPv6 규칙에는 IPv6 주소만 씁니다."
    }
  },
  "nodes": {
    "address": "API 주소",
    "underlay": "언더레이 주소",
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
    },
    "fencing": "펜싱",
    "fencingMode": {
      "watchdog": "Watchdog",
      "process": "프로세스"
    },
    "version": "버전",
    "updates": {
      "column": "업데이트",
      "open": "{{name}}의 업데이트: {{state}}",
      "stateCount": "{{n}}개",
      "stateFailed": "확인 실패",
      "stateNotChecked": "아직 확인하지 않음",
      "menu": "업데이트…",
      "title": "{{name}}의 업데이트",
      "checked": "마지막 확인: {{time}}",
      "notChecked": "아직 확인하지 않았습니다.",
      "none": "올릴 업데이트가 없습니다.",
      "failed": "마지막 확인이 실패했습니다:",
      "failedBadge": "확인 실패",
      "package": "패키지",
      "current": "설치됨",
      "candidate": "새 버전",
      "newPackage": "(새 패키지)",
      "truncated": "앞의 {{count}}개만 보입니다.",
      "refresh": "다시 확인",
      "checking": "확인 중…",
      "upgrade": "업그레이드",
      "upgrading": "업그레이드 중…",
      "upgradeTitle": "{{name}}을(를) 업그레이드할까요?",
      "upgradeDescription": "VM은 계속 돌지만 opld가 잠깐 다시 시작합니다. 다른 노드는 이 업그레이드가 끝난 뒤에 하세요.",
      "errors": {
        "node_offline": "노드가 오프라인입니다."
      }
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
    "realm": "영역",
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
  "realms": {
    "pageHint": "영역의 사용자는 디렉터리(LDAP)나 IdP(OIDC) 계정으로 로그인하고 <이름>@<영역>이 됩니다. 역할은 로그인할 때마다 그룹으로 정해집니다.",
    "adminOnly": "인증 영역은 관리자만 관리할 수 있습니다.",
    "empty": "영역이 없습니다. 로컬 사용자만 로그인합니다.",
    "display": "표시 이름",
    "displayHint": "로그인 화면에 보입니다. 비우면 이름이 보입니다.",
    "kind": "종류",
    "kinds": {
      "ldap": "LDAP",
      "oidc": "OIDC"
    },
    "url": "URL",
    "urlHint": "ldap://는 늘 StartTLS를 쓰고, ldaps://는 처음부터 TLS를 씁니다.",
    "baseDn": "기준 DN",
    "bindDn": "바인드 DN",
    "bindDnHint": "사용자를 찾을 서비스 계정입니다. 비우면 익명으로 찾습니다.",
    "bindPassword": "바인드 비밀번호",
    "bindPasswordSaved": "저장된 비밀번호가 있습니다. 비워 두면 그대로 둡니다.",
    "clearBindPassword": "저장된 비밀번호 지우기",
    "userFilter": "사용자 필터",
    "userFilterHint": "{username}은 이스케이프한 로그인 이름으로 바뀝니다.",
    "groupAttr": "그룹 속성",
    "caPem": "CA 인증서(PEM)",
    "caPemHint": "디렉터리 인증서를 이 CA로만 확인합니다. 비우면 시스템 루트를 씁니다.",
    "timeout": "시간 제한(초)",
    "groups": "그룹별 역할",
    "groupsHint": "그룹 DN은 대소문자를 가리지 않고 비교합니다. 여러 그룹에 속하면 가장 높은 역할을 받습니다.",
    "groupN": "그룹 {{n}}",
    "groupRoleN": "그룹 {{n}}의 역할",
    "removeGroupN": "그룹 {{n}} 빼기",
    "addGroup": "그룹 더하기",
    "defaultRole": "기본 역할",
    "noDefaultRole": "없음(로그인 거절)",
    "createTitle": "LDAP 영역 만들기",
    "editTitle": "영역 {{name}} 고치기",
    "formDescription": "역할은 사용자가 로그인할 때마다 매핑으로 다시 정해집니다.",
    "editNamed": "{{name}} 고치기",
    "created": "영역 {{name}}을(를) 만들었습니다.",
    "saved": "영역 {{name}}을(를) 저장했습니다.",
    "deleted": "영역 {{name}}을(를) 삭제했습니다.",
    "deleteTitle": "영역 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 영역의 사용자는 더 로그인할 수 없고, 세션과 API 토큰도 멈춥니다. 사용자는 남으며 같은 이름의 영역을 다시 만들면 다시 로그인할 수 있습니다.",
    "test": "시험",
    "testNamed": "{{name}} 시험",
    "testTitle": "영역 {{name}} 시험",
    "testDescription": "디렉터리 사용자로 로그인하는 단계를 하나씩 확인합니다. 사용자나 세션은 만들지 않습니다.",
    "steps": "단계",
    "step": "단계",
    "result": "결과",
    "message": "내용",
    "ok": "성공",
    "failed": "실패",
    "stepNames": {
      "connect": "연결",
      "service_bind": "서비스 계정 바인드",
      "search": "사용자 찾기",
      "user_bind": "사용자 바인드",
      "groups": "그룹",
      "discovery": "Discovery",
      "jwks": "서명 키"
    },
    "dn": "DN",
    "groupsFound": "그룹",
    "noRole": "역할 없음: 로그인이 거절됩니다.",
    "errors": {
      "local": "local은 로컬 사용자용으로 예약된 이름입니다.",
      "taken": "같은 이름의 영역이 이미 있습니다.",
      "url": "ldap://호스트[:포트](StartTLS) 또는 ldaps://호스트[:포트] 형식으로 쓰세요.",
      "required": "값을 입력하세요.",
      "userFilter": "{username}을 정확히 한 번 넣으세요.",
      "bindDn": "바인드 비밀번호를 쓰려면 바인드 DN을 입력하세요.",
      "bindPasswordNeeded": "바인드 DN에는 바인드 비밀번호가 필요합니다. 익명으로 찾으려면 바인드 DN을 비우세요.",
      "bindPasswordAgain": "URL·바인드 DN·CA 인증서를 바꿨으니 바인드 비밀번호를 다시 넣으세요.",
      "clearNeedsNoBindDn": "저장된 비밀번호를 지우려면 바인드 DN도 비우세요.",
      "clearSaved": "바인드 DN을 비웠으니 익명으로 찾으려면 \"저장된 비밀번호 지우기\"를 고르세요.",
      "timeout": "1~30 사이의 정수(초)로 쓰세요.",
      "group": "그룹 DN을 입력하거나 행을 빼세요.",
      "issuer": "쿼리·조각 없이 https://호스트[:포트][/경로] 형식으로 쓰세요.",
      "scopes": "openid를 넣으세요.",
      "clientSecretAgain": "issuer·클라이언트 ID·CA 인증서를 바꿨으니 클라이언트 비밀을 다시 넣으세요."
    },
    "createOidcTitle": "OIDC 영역 만들기",
    "issuer": "Issuer",
    "issuerHint": "IdP가 알리는 그대로의 https:// URL입니다(끝의 / 포함).",
    "clientId": "클라이언트 ID",
    "clientSecret": "클라이언트 비밀",
    "clientSecretHint": "공개 클라이언트(PKCE만)면 비워 두세요.",
    "clientSecretSaved": "저장된 비밀이 있습니다. 비워 두면 그대로 둡니다.",
    "clearClientSecret": "저장된 비밀 지우기(공개 클라이언트)",
    "scopes": "Scope",
    "scopesHint": "공백으로 나눕니다. openid가 있어야 합니다.",
    "usernameClaim": "사용자 이름 클레임",
    "groupsClaim": "그룹 클레임",
    "groupsClaimHint": "사용자의 그룹을 문자열 목록으로 담은 클레임입니다.",
    "redirectUri": "리디렉트 URI",
    "redirectUriHint": "이 주소를 IdP에 등록하세요. 로그인에 쓰는 노드 주소마다 따로 등록해야 합니다. 리버스 프록시 뒤라면 프록시가 Host로 넘기는 주소를 등록하세요.",
    "copyRedirectUri": "리디렉트 URI 복사",
    "oidcCaPemHint": "IdP 인증서를 이 CA로만 확인합니다. 비우면 시스템 루트를 씁니다.",
    "oidcGroupsHint": "그룹 클레임의 값은 대소문자를 가리지 않고 비교합니다. 여러 그룹에 속하면 가장 높은 역할을 받습니다.",
    "oidcTestDescription": "IdP의 discovery 문서와 서명 키를 확인합니다. 아무도 로그인하지 않습니다."
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
    "membersNote": "투표 멤버가 클러스터 상태를 지킵니다. 학습자는 아직 합류 중인 노드입니다. 위트니스는 투표만 하고 VM을 돌리지 않습니다.",
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
  "remotes": {
    "pageHint": "이 클러스터가 보는 다른 opLite 클러스터입니다. 상태는 30초마다 모읍니다. 원격 클러스터에서 할 수 있는 일은 그곳 API 토큰 주인의 역할로도 정해집니다.",
    "empty": "원격 클러스터가 없습니다.",
    "add": "추가",
    "addTitle": "원격 클러스터 추가",
    "addDescription": "다른 opLite 클러스터를 주소, CA 지문, 그 클러스터에서 만든 API 토큰으로 등록합니다.",
    "editTitle": "원격 클러스터 {{name}} 고치기",
    "editNamed": "{{name}} 고치기",
    "refreshNamed": "{{name}} 새로고침",
    "url": "URL",
    "urlHint": "원격 클러스터 노드의 https://호스트[:포트]",
    "getCa": "CA 받기",
    "fingerprint": "CA 지문",
    "confirmFingerprint": "원격 클러스터의 클러스터 화면에 보이는 CA 지문과 같습니다.",
    "token": "API 토큰",
    "tokenHint": "원격 클러스터의 API 토큰 화면에서 만듭니다. 그곳에서 토큰 주인의 역할이 이 클러스터가 할 수 있는 일을 정합니다.",
    "tokenSaved": "토큰이 저장돼 있습니다. 그대로 두려면 비워 두세요.",
    "cluster": "클러스터",
    "version": "버전",
    "nodes": "노드",
    "nodesValue": "{{total}}개 중 {{online}}개 온라인",
    "vms": "VM",
    "vmsValue": "{{total}}개 중 {{running}}개 실행 중",
    "cpus": "CPU",
    "memory": "메모리",
    "memoryValue": "{{gib}} GiB",
    "reachable": "연결됨",
    "unreachable": "연결 안 됨",
    "created": "원격 클러스터 {{name}}을(를) 추가했습니다.",
    "saved": "원격 클러스터 {{name}}을(를) 저장했습니다.",
    "deleted": "원격 클러스터 {{name}}을(를) 삭제했습니다.",
    "refreshFailed": "{{name}}을(를) 새로 읽지 못했습니다: {{error}}",
    "deleteTitle": "원격 클러스터 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 클러스터에서 보이지 않게 됩니다. 원격 클러스터는 바뀌지 않고, 그곳의 API 토큰도 지우기 전까지 남습니다.",
    "backToList": "원격 클러스터 목록으로",
    "vmsTitle": "{{name}}의 가상 머신",
    "actionRequested": "{{remote}}의 {{name}}에 {{action}} 요청을 보냈습니다.",
    "errors": {
      "probe": "CA를 받지 못했습니다: {{error}}",
      "probeFirst": "먼저 이 URL의 CA를 받으세요.",
      "confirm": "지문을 원격 클러스터에서 확인하고 체크하세요.",
      "token": "원격 클러스터의 API 토큰을 넣으세요.",
      "tokenAgain": "URL이나 CA가 바뀌었으니 API 토큰을 다시 넣으세요."
    }
  },
  "vmForm": {
    "createTitle": "가상 머신 만들기",
    "editTitle": "{{name}} 편집",
    "runningNote": "VM이 실행 중이라 노드·디스크와 인터페이스의 개수·순서·MAC은 바꿀 수 없고, 보안 그룹이 걸린 인터페이스가 있으면 인터페이스는 그룹만 바꿀 수 있습니다. 보안 그룹은 곧바로, 다른 변경은 다음 시작부터 적용됩니다.",
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
    "nicsRunning": "VM이 실행 중이라 인터페이스를 더하거나 빼거나 MAC을 바꿀 수 없습니다.",
    "securityGroups": "보안 그룹",
    "nicGroups": "인터페이스 {{n}}의 보안 그룹",
    "noSecurityGroups": "보안 그룹이 없습니다. 보안 그룹 화면에서 만드세요.",
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
      "keep": "1에서 100 사이여야 합니다.",
      "securityGroups": "보안 그룹은 8개까지 고를 수 있습니다."
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
    "scheduleSharedLvm": "{{volume}}이(가) 공유 LVM 풀에 있어 예약 스냅샷이 실패합니다.",
    "ha": "고가용성(HA)",
    "haHint": "펜싱이 켜진 노드에서만 돕니다. 노드가 클러스터 과반과 끊기면 그 노드가 VM을 끕니다.",
    "haNodeOff": "{{node}}의 펜싱이 꺼져 있어 HA VM을 돌릴 수 없습니다.",
    "haNoNode": "펜싱이 켜진 노드가 없어 HA VM을 돌릴 수 없습니다.",
    "haRunning": "HA는 VM이 멈춰 있을 때 켤 수 있습니다."
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
    "witness": "위트니스",
    "failedVms": "실패한 가상 머신",
    "recentTasks": "최근 작업",
    "allTasks": "모든 작업",
    "noLeader": "리더 없음",
    "noNodes": "노드가 없습니다.",
    "mixedVersions": "노드들이 서로 다른 opLite 버전({{versions}})을 돌립니다. 모든 노드의 업그레이드를 끝내세요.",
    "subscription": {
      "none": "이 클러스터에는 구독이 없습니다.",
      "invalid": "구독 키가 이 빌드와 맞지 않습니다.",
      "expired": "구독이 {{date}}에 만료되었습니다.",
      "over_limit": "클러스터의 노드는 {{used}}대인데 구독은 {{limit}}대까지입니다.",
      "expiring": "구독이 {{date}}에 만료됩니다.",
      "manage": "구독 설정",
      "dismiss": "7일 동안 숨기기"
    }
  },
  "settings": {
    "updates": "업데이트",
    "channel": "업데이트 채널",
    "channelHint": "모든 노드가 이 채널에서 opLite 패키지를 받습니다. testing은 stable보다 새 버전을 먼저 받습니다.",
    "channels": {
      "stable": "stable(안정)",
      "testing": "testing(시험)",
      "enterprise": "enterprise(구독)"
    },
    "saved": "설정을 저장했습니다.",
    "adminOnly": "설정은 관리자만 바꿀 수 있습니다.",
    "errors": {
      "busy": "다른 설정 변경이 진행 중입니다. 다시 시도하세요."
    },
    "enterpriseHint": "enterprise는 유효한 구독이 있어야 고를 수 있습니다.",
    "subscription": {
      "title": "구독",
      "states": {
        "none": "구독 없음",
        "active": "유효",
        "expired": "만료됨",
        "invalid": "잘못된 키",
        "over_limit": "노드 수 초과"
      },
      "development": "개발용 키",
      "customer": "고객",
      "level": "수준",
      "levels": {
        "standard": "Standard",
        "premium": "Premium"
      },
      "nodes": "노드",
      "expires": "만료일",
      "key": "구독 키",
      "keyHint": "받은 키를 붙여 넣으세요. opl1.로 시작합니다.",
      "apply": "키 넣기",
      "applied": "구독 키를 넣었습니다.",
      "remove": "키 지우기",
      "removeTitle": "구독 키를 지울까요?",
      "removeDescription": "클러스터에 구독이 없어집니다. enterprise 채널의 노드는 업데이트를 받지 못합니다.",
      "removed": "구독 키를 지웠습니다."
    },
    "telemetry": {
      "title": "텔레메트리",
      "intro": "켜면 리더가 하루에 한 번 사용 보고를 보냅니다. 수와 버전만 보내고 이름·주소·키는 보내지 않습니다.",
      "on": "켜짐",
      "off": "꺼짐",
      "endpoint": "끝점",
      "buildDefault": "설정 안 됨 — 빌드에 기본 끝점이 있으면 그것을 씀",
      "endpointHint": "https:// 주소. 비우면 빌드 기본 끝점을 씁니다.",
      "saveEndpoint": "끝점 저장",
      "id": "보고 ID",
      "lastSent": "마지막 전송",
      "never": "없음",
      "lastError": "마지막 오류",
      "enable": "켜기…",
      "disable": "끄기",
      "resetId": "보고 ID 새로 만들기",
      "previewTitle": "텔레메트리 켜기",
      "previewDescription": "지금 보낼 보고 그대로입니다. 이것 말고는 보내지 않습니다.",
      "confirmEnable": "켜기",
      "enabled": "텔레메트리를 켰습니다.",
      "disabled": "텔레메트리를 껐습니다.",
      "idReset": "보고 ID를 새로 만들었습니다."
    }
  },
  "audit": {
    "adminOnly": "관리자만 감사 로그를 읽을 수 있습니다.",
    "refresh": "새로고침",
    "range": "시간 범위",
    "ranges": {
      "1h": "지난 1시간",
      "24h": "지난 24시간",
      "7d": "지난 7일"
    },
    "user": "사용자",
    "action": "동작",
    "apply": "적용",
    "time": "시각",
    "ip": "IP",
    "target": "대상",
    "status": "상태",
    "refreshFailed": "감사 로그를 다시 읽지 못했습니다. {{error}}",
    "empty": "이 범위에 감사 기록이 없습니다.",
    "unreachable": "다음 노드의 감사 기록을 읽지 못했습니다: {{nodes}}",
    "disabled": "다음 노드는 감사 로그가 꺼져 있습니다: {{nodes}}",
    "truncated": "최근 {{count}}개만 보입니다. 더 있으니 범위나 거르기를 좁히세요.",
    "entryTitle": "감사 항목"
  }
}
```

`web/src/remotes/remotes.ts`에서

```ts
import type { components } from "@/api/schema";
import type { Task, Vm } from "@/api/types";
```

을 다음으로 바꾼다:

```ts
import type { components } from "@/api/schema";
import type { StoragePool, Task, Vm } from "@/api/types";
```

`web/src/remotes/remotes.ts`에서

```ts

/** 원격 VM의 동작을 요청한다. 원격이 만든 Task를 돌려준다(이 클러스터의 Task가 아니라 지켜보지 않는다). */
```

을 다음으로 바꾼다:

```ts

/** 원격의 스토리지 풀 목록(백업 사본을 보낼 백업 풀을 고른다) */
export async function remoteStoragePools(
  remote: string,
  signal?: AbortSignal,
): Promise<StoragePool[]> {
  const list = await proxy<{ items: StoragePool[] }>(remote, "GET", "/storage-pools", signal);
  return list.items;
}

/** 원격 VM의 동작을 요청한다. 원격이 만든 Task를 돌려준다(이 클러스터의 Task가 아니라 지켜보지 않는다). */
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx
        },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
```

을 다음으로 바꾼다:

```tsx
        },
        status: { state: "running", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000004",
        spec: {
          op: { kind: "backup-replicate", remote: "dr", pool: "bk" },
          target: backup,
          node: "n1",
        },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx
  expect(screen.getByText("Restore a backup as web2")).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByText("Restore a backup as web2")).toBeInTheDocument();
  expect(screen.getByText("Copy backup web-1 to dr")).toBeInTheDocument();
});
```

`web/src/tasks/tasks.test.tsx`에서

```tsx
  expect(success).not.toHaveBeenCalled();
});
```

을 다음으로 바꾼다:

```tsx
  expect(success).not.toHaveBeenCalled();
});

it("warns when a task succeeded with a warning", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const warning = vi.spyOn(toast, "warning");
  trackTask(task("9", "pending"), "Delete web-1");
  settleTasks(
    {
      revision: 2,
      items: [task("9", "succeeded", "backup deleted, but remote copies may be left: down")],
    },
    i18n.t,
  );
  expect(
    await screen.findByText(
      "Delete web-1: done with a warning. backup deleted, but remote copies may be left: down",
    ),
  ).toBeInTheDocument();
  expect(warning).toHaveBeenCalledOnce();
});
```

`web/src/tasks/tasks.ts`에서

```ts
      }
    } else if (task.status.state === "succeeded") {
```

을 다음으로 바꾼다:

```ts
      }
    } else if (task.status.state === "succeeded" && task.status.error) {
      // 성공했지만 남긴 경고가 있다(백업 지우기의 "원격 사본이 남았을 수 있음").
      pending.delete(id);
      toast.warning(t("tasks.doneWarning", { label, warning: task.status.error }), {
        id,
        duration: Infinity,
        closeButton: true,
      });
    } else if (task.status.state === "succeeded") {
```

`web/src/tasks/tasks.ts`에서

```ts
  const from = op.kind === "vm-ha-recover" ? (nodeName(op.from) ?? op.from) : undefined;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${op.kind}`, { target, from, defaultValue: `${op.kind} ${target}` });
}
```

을 다음으로 바꾼다:

```ts
  const from = op.kind === "vm-ha-recover" ? (nodeName(op.from) ?? op.from) : undefined;
  const remote = op.kind === "backup-replicate" ? op.remote : undefined;
  // 모르는 작업(새 서버)은 번역 키 대신 종류를 그대로 보여 준다.
  return t(`tasks.op.${op.kind}`, {
    target,
    from,
    remote,
    defaultValue: `${op.kind} ${target}`,
  });
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 백업 원격 복제 화면 — 백업 표(백업 화면·VM 백업 탭)에 받은 사본이면 이름 옆 \"<클러스터>에서 받음\" 배지(툴팁에 원본 백업 id·작업), \"원격 사본\" 열(사본마다 상태 아이콘 — 준비됨·복제 중·실패 — 과 원격 이름, 툴팁에 원격/풀·상태·오류), 백업 쓰기 권한이면 ready 백업의 \"원격으로 복제\" 대화상자(원격은 GET /remotes, 그 원격의 풀은 중계 GET /storage-pools를 fetch로 읽어 백업 풀만, POST /backups/{key}/actions/replicate 뒤 Task를 따라감). 백업 작업: \"원격 복제\" 열(원격 / 풀)과 만들기·고치기의 \"원격 클러스터로 복제\"(복제하지 않음 / 원격과 그 백업 풀 — 원격을 고르면 풀도 골라야 함, 만들기는 고를 때만 replicate를 보내고 고치기는 바뀔 때만, 끄면 replicate: null). 원격·풀 고르기는 RemoteTargetFields로 함께 씀(저장된 원격·풀은 목록에 없어도 보임). Task 이름 \"백업 X을(를) R(으)로 복제\", backup_busy·backup_not_ready·지우기 설명 문구에 복제를 더함, ko·en. 시험: 출처 배지·사본 상태 툴팁, 복제 대화상자(원격·풀 검사, 백업 풀만, 요청 본문, Task 이름), 원격 풀을 읽지 못한 오류, ready가 아니면 막힘, viewer는 버튼 없음, 작업 표의 원격 복제 열, 복제하는 작업 만들기·끄기(PATCH replicate: null), jobs.ts 폼 변환, Task 이름"
```

---

### Task 7: 문서

README 새 절 "원격 복제(DR)"(받는 쪽 준비, 수동·작업 복제, 사본 상태와 확인, 지우기 전파, DR 절차, 한계)와 상태 줄·설계 링크, 엔터프라이즈 설계 §6.6 "구현(계획 38)"과 §7·§9, 처음 분해 문서의 하위 프로젝트 10 줄(하위 프로젝트 1~10이 모두 끝남).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-06-enterprise-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 처음 분해한 하위 프로젝트 1~10 완료 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리, 백업·백업 작업, VMware 가져오기, 감사 로그, LDAP·OIDC, 원격 클러스터·원격 복제)
```

`README.md`에서

```markdown
  그 작업의 실패한 백업은 같은 VM의 더 새 성공한 백업이 생기면 지운다. 손으로 만든 백업과 다른 작업의 백업은
  건드리지 않는다.
- **작업을 지워도 백업은 남는다.** 그 백업은 더는 보존 규칙으로 지워지지 않으니 `oplctl backup delete`로 지운다.
```

을 다음으로 바꾼다:

```markdown
  그 작업의 실패한 백업은 같은 VM의 더 새 성공한 백업이 생기면 지운다. 손으로 만든 백업과 다른 작업의 백업은
  건드리지 않는다. 실행이 끝난 뒤에는 스케줄러가 확인할 때마다 보존 규칙을 다시 보므로, 그때 지우지 못한
  백업(복제·복원 중이라 `backup_busy`)은 그 일이 끝나면 지우고, 실행 뒤에 보존 값을 줄이면 다음 확인 때 지운다.
  지우기 Task가 실패한 백업은 다음 실행이 끝날 때 다시 지운다.
- **작업을 지워도 백업은 남는다.** 그 백업은 더는 보존 규칙으로 지워지지 않으니 `oplctl backup delete`로 지운다.
```

`README.md`에서

```markdown
- **한계.** 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존
  VM 덮어쓰기는 없다.
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
```

을 다음으로 바꾼다:

```markdown
- **한계.** 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존
  VM 덮어쓰기는 없다. 다른 opLite 클러스터로 사본을 보내는 것은 아래 "원격 복제(DR)"다.
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
```

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기·원격으로 복제)과 백업 작업(원격 복제), VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown

## 이미지 업로드와 머신 종류
```

을 다음으로 바꾼다:

````markdown

## 원격 복제(DR)

백업을 등록한 원격 클러스터(위 "원격 클러스터")의 백업 풀로 복사해 두면, 주 사이트(클러스터 prod)를 통째로 잃어도
DR 사이트(클러스터 dr)에서 그 사본을 복원해 VM을 되살린다. 사본은 dr의 **보통 백업**이다: dr의 `oplctl backup
list`·웹 백업 화면에 보이고, dr에서 복원하고 지운다. 백업 작업이 만든 백업은 저절로(`replicate`), 아무 백업이나 손으로
(`backup replicate`) 보낸다. prod에서 원본을 지우면(사용자나 보존 규칙) dr의 사본도 지운다.

- **보내는 길.** 복제는 prod의 Task(`backup-replicate`)가 한다. 백업 파일이 있는 노드(공유가 아닌 풀은 백업을 쓴
  노드, NFS면 그 풀을 쓸 수 있는 노드 — 지우기와 같다)가 원격 등록의 URL·CA·토큰으로 dr의 받기 API를 부른다:
  `POST /backups/receive`(매니페스트와 출처)로 dr에 `receiving` 백업을 만들고, 디스크 파일(`disk0.qcow2`…)과
  `vars.fd`(UEFI)를 파일마다 읽으면서 SHA-256을 계산해 `PUT /backups/{id}/files/{file}`로 흘려 보내고(본문을
  메모리에 모으지 않는다), dr이 받은 내용의 SHA-256과 크기를 돌려주면 보낸 것과 비교해 다르면 실패한다. 다 보내면
  `POST /backups/{id}/actions/complete`로 dr이 `manifest.json`을 써 사본이 `ready`가 된다. 원본이 `ready`여야
  보낸다(**409** `backup_not_ready`). Task에 시간 제한은 없고(디스크 크기만큼 걸린다), 원격 연결의 본문 조각 사이
  60초 제한만 있다. dr은 받는 동안 128 MiB마다 디스크에 내려 쓰고 끝에 한 번 더 내려 쓴 뒤 답하므로, prod는 파일을 다
  보낸 뒤 그 응답을 120초까지 기다린다(다른 요청은 10초). dr은 파일마다 받은 SHA-256을 사본의 디스크
  (`status.disks[].sha256`)에, 마지막으로 받은 시각을 `status.received_at`에 적고, prod는 맞게 보낸 파일의 SHA-256을
  원본의 디스크(`status.disks[].sha256`)에 적는다. dr은 본문 조각이 60초 넘게 오지 않으면 그 파일을 버린다(멈춘
  업로드 — 복제 Task는 실패하고 다시 보낼 수 있다). 진행률은 없다.
- **dr 쪽 준비.**
  - **백업 풀.** 사본을 둘 dr의 백업 풀(`backups`를 켠 dir·NFS 풀). 백업 풀이 아니거나 없으면 dr이 **400**이고,
    남은 공간이 원본 디스크 파일 크기의 합보다 작으면 **507** `insufficient_storage`다.
  - **operator 토큰.** prod에 dr을 등록할 때 쓴 토큰의 주인이 dr에서 operator 이상이어야 한다(받기와 원격 사본 지우기가
    백업 쓰기). viewer 토큰이면 dr의 **403**이 복제 Task의 오류와 사본 기록에 남는다. admin 토큰은 쓰지 않는다. 이
    토큰은 dr의 백업을 지울 수 있다(아래 "한계").
  - **같은 버전.** 두 클러스터는 같은 opLite 버전이어야 한다. prod가 보내는 매니페스트·출처에 dr이 모르는 필드가
    있으면 dr이 **400**으로 거절한다(업그레이드는 dr부터).
  - **파일을 받는 노드.** dr에서 파일을 쓰는 것은 받기 요청을 받은 노드, 곧 prod에 등록한 원격 URL의 노드다. 그래서
    백업 풀이 NFS면 아무 노드나 되지만, 공유가 아닌 디렉터리 풀이면 그 풀이 있는 노드의 주소로 원격을 등록해야
    한다(아니면 **409** `pool_not_on_node` — "register this cluster as a remote with the URL of a node holding the
    pool, or use a shared (NFS) backup pool"). 그 노드를 잃으면 사본도 잃으므로 DR에는 NFS 백업 풀이 낫다. 받은 사본은
    dir 풀이면 그 노드의 것이라(`status.node`) 그 노드에서 복원한다.
  - **복원할 곳.** VM 디스크를 둘 풀, 그리고 prod VM의 NIC가 쓰는 **네트워크와 보안 그룹을 같은 이름으로** 미리
    만들어 둔다. 복원은 백업 때의 VM 스펙을 그대로 쓰므로 없으면 **400**이다(복원에서 바꿀 수 없다).
- **사본의 이름과 출처.** 사본의 이름은 원본 이름이고 dr에 이미 있으면 `-2`, `-3`…을 붙인다. `spec.origin`에 보낸
  클러스터 이름(`cluster`), 그 CA 지문(`ca_fingerprint`), 원본 id(`backup`), 원본을 만든 백업 작업 이름(`job`, 손으로
  만든 백업이면 없음)이 있다. dr의 `oplctl backup list`는 `ORIGIN` 열에 `<보낸 클러스터>/<원본 id 앞 8자>`, 웹은 이름
  옆에 "<클러스터>에서 받음" 배지(툴팁과 화면 읽기에 원본 id·작업)를 보인다. 사본에는 `spec.job`이 없으므로 dr의 백업
  작업 보존 규칙은 사본을 지우지 않는다(사본은 prod의 보존 규칙이 원본을 지울 때 함께 지워진다). 같은 원본(CA 지문과
  원본 id)의 사본은 dr의 백업 풀마다 하나뿐이다(같은 원본을 dr의 다른 풀로 보내면 다른 사본이다).
- **사본 상태.** prod의 원본 `status.replicas`에 (원격, 풀)마다 `{remote, pool, backup(dr의 사본 id), state, error, at}`이
  있다. `state`는 `copying`(보내는 중) → `ready` 또는 `failed`(`error`에 이유)다. `oplctl backup list`의 `REPLICAS` 열은
  `<원격>:<상태>`(쉼표로), 웹 백업 표의 "원격 사본" 열은 사본마다 상태 아이콘과 원격 이름(툴팁에 원격/풀·상태·오류,
  화면 읽기에 원격/풀·상태)이고 실패한 사본은 그 아래에 "원격 / 풀: 오류"를 글로 보인다. `oplctl backup get`(JSON)에서
  전체를 본다. 이미 `ready`인 사본을 다시 확인하는 동안(아래 "다시 하기") 기록은 `ready` 그대로다: 확인이 성공하면
  `at`만 고치고, dr에 닿지 못하면 `ready`로 둔 채 `error`에 이유를 적는다(복제 Task는 실패, 다음 실행이 다시
  확인한다). dr에 없으면(404) `copying`으로 바꿔 다시 보내고, 다른 사본이거나 해시가 다르면 `failed`다.
- **다시 하기.** 같은 백업을 같은 원격·풀로 다시 보내면 dr의 사본으로 이어 간다. 복제 Task는 먼저 사본 기록의 id를
  dr에 묻고(`GET /backups/{id}` — **404**면 dr에서 지운 것이므로 새로 받기부터 한다), 기록이 없으면 받기를 시작해
  dr이 **409** `replica_exists`로 알려 주는 사본을 읽는다. 그 사본이 이 백업의 사본인지 — 출처의 원본 id, 요청한 풀,
  VM 스펙, 디스크 이름과 크기, UEFI 변수 — 확인하고, 다르면 "a different backup claims to be the copy of <원본 id> on
  remote …; delete it there"로 실패한다(dr에서 그 백업을 지운다). 사본이 `ready`면 원본에 적힌 디스크 해시(보낼 때
  적은 것 — 해시가 없는 옛 백업만 원본 파일을 읽어 계산해 적는다)와 dr이 받은 해시를 비교해 모두 같을 때만 보내지
  않고 성공하고, 해시가 없거나 다르면 "… is ready but its files do not
  match …; delete it there and replicate again"으로 실패한다(ready 사본은 파일을 다시 받지 않는다). 아직 `receiving`이면
  파일을 모두 다시 보낸다(같은 파일·반쯤 받은 `.part`는 덮는다). 보내다 멈춘 사본(`receiving`)은 dr의 정리 작업이
  만든 지, 그리고 마지막으로 파일을 받은 지 24시간이 지나면 파일과 함께 지운다. 백업 작업은 실행마다 한 번 실패한
  사본을 다시 보내고 ready 사본도 dr에 있는지 확인한다(아래).

```bash
# dr에서: 백업 풀(dir 풀이면 prod에 등록한 URL의 노드에), 같은 이름의 네트워크·보안 그룹, operator 토큰(위 "원격 클러스터")
oplctl pool create dr-bk --nfs nas2.lan:/srv/dr-backup --backups

# prod에서: dr을 원격으로 등록한 뒤
oplctl backup replicate before-upgrade --remote dr --pool dr-bk          # Task id와 따라가는 방법을 보이고 끝난다
oplctl backup replicate before-upgrade --remote dr --pool dr-bk --wait   # 끝날 때까지(24시간까지) 기다린다
oplctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --replicate dr/dr-bk
oplctl backup-job set db --replicate dr/dr-bk       # 있는 작업에 켜기(대상 바꾸기도 같다)
oplctl backup-job set db --no-replicate             # 끄기(이미 보낸 사본은 남는다)
oplctl backup list
# NAME                 VM   POOL  PHASE  CREATED               SIZE      ORIGIN  REPLICAS
# web-20261107-030000  web  bk    ready  2026-11-07 03:00 UTC  412.5MiB  -       dr:ready
oplctl backup delete web-20261107-030000            # dr의 사본도 지운다

# dr에서
oplctl backup list
# NAME                 VM   POOL   PHASE  CREATED               SIZE      ORIGIN         REPLICAS
# web-20261107-030000  web  dr-bk  ready  2026-11-07 03:00 UTC  412.5MiB  prod/019a5b1c  -
```

- **수동 복제.** `oplctl backup replicate <백업> --remote <원격> --pool <원격의 백업 풀> [--wait]`(API `POST
  /api/v1/backups/{key}/actions/replicate {"remote", "pool"}`, operator 이상, **202** Task). 원격이 등록되지 않았으면
  **404**, 원본이 `ready`가 아니면 **409** `backup_not_ready`, 지우는 중이거나 같은 원격으로 보내는 Task가 끝나지
  않았으면 **409** `backup_busy`, 파일이 있는 노드가 오프라인이면 **409** `node_offline`이다. 웹은 백업 표(백업 화면·VM
  상세의 백업 탭)의 "원격으로 복제" 버튼(ready 백업, operator 이상): 원격을 고르면 그 원격의 백업 풀을 중계(`GET
  /storage-pools`)로 읽어 고르게 하고, 보낸 뒤 Task를 따라간다. 복제 중인 백업을 지우면 **409** `backup_busy`("being
  replicated")다(보내는 동안 파일을 지우지 않게).
- **작업 복제.** 백업 작업에 `replicate: {"remote", "pool"}`(`backup-job create|set --replicate <원격>/<풀>`, 웹 백업
  작업의 "원격 클러스터로 복제")을 주면, 리더의 스케줄러가 확인할 때마다(`[backup] tick_secs`) 그 작업이 만든 `ready`
  백업 가운데 그 (원격, 풀) 사본 기록이 없는 것을 만든 순서로 복제 Task로 보낸다 — 켜기 전에 만든 그 작업의 백업도
  보낸다. 기록이 마지막 실행을 시작하기 전의 것이면 상태와 관계없이 다시 보내므로, `failed`(또는 복제 Task 없이
  `copying`으로 남은) 사본은 실행마다 한 번 다시 보내고, `ready` 사본도 실행마다 한 번 복제 Task가 dr에 물어 같은
  파일이 있으면 기록 시각만 고치고 dr에서 지웠으면 다시 보낸다(dr에 묻고 적힌 해시를 비교할 뿐 원본 파일을 다시 읽지
  않는다). 복제 Task는
  노드마다 하나씩, 그 노드의 백업 Task와 합쳐 `[backup] max_concurrent`(손으로 만든 백업·복제도 센다)까지이고, 백업
  Task도 복제 Task를 센다. 꺼 둔 작업은 보내지 않는다. 그 작업의 보존 규칙이 지울 백업은 확인·복제하지 않고, 실행이
  끝날 때 지울 백업이 복제 중이라 지우지 못하면(`backup_busy`) 확인할 때마다 다시 해서 복제 Task가 끝나면 지운다.
  `PATCH /api/v1/backup-jobs/{name}`의 `replicate`는 객체면 바꾸고 `null`이면 끄고 없으면 그대로다
  (`--no-replicate`가 `null`). 등록되지 않은 원격은 **400**이고 원격의 풀은 보낼 때 dr이 확인한다(틀리면 Task 실패).
- **지우기 전파.** prod에서 원본을 지우는 Task(`backup delete`, 백업 작업의 보존 규칙)는 원본 파일을 먼저 지우고(지우지
  못하면 Task가 실패하고 dr은 건드리지 않는다), 그다음 `replicas`의 사본 id가 있는 사본마다(`failed`도 — 받다 만
  사본이 dr에 있다) dr의 `DELETE /backups/{id}`를 부른다(dr이 지우기 Task를 시작한다 — 끝나기를 기다리지 않는다,
  **404**는 이미 지운 것으로 성공). dr에 닿지 못하거나 거절하면 원본은 그대로 지우고, Task는 성공하되 `error`에
  "backup deleted, but remote copies may be left: …" 경고를 남긴다(`oplctl backup delete`는 `warning: …` 줄로, 웹은
  닫을 때까지 남는 경고 알림으로 보인다). 그 사본은 dr에서 `ORIGIN`으로 찾아 지운다. `backup delete --force`(파일을
  지울 노드가 없어 리소스만 지움)는 원격을 건드리지 않는다. dr에서 사본을 지우면 백업 작업이 다음 실행에 다시
  보낸다(위).
- **업그레이드 중.** 복제·받기·`replicate`가 있는 작업 만들기·고치기는 모든 노드가 이 버전이어야 하고(아니면 **409**
  `cluster_upgrading`), 스케줄러는 모두 올라올 때까지 보내지 않는다. 받는 dr도 이 버전이어야 한다(모르는 필드는
  **400**).
- **받기 API(dr 쪽, 백업 쓰기 — operator 이상).** 보통은 prod의 복제 Task만 부른다.
  - `POST /api/v1/backups/receive {"name"?, "pool", "manifest", "origin"}` → **201** `{"id", "name"}`. 풀이 없거나 백업
    풀이 아니거나 매니페스트가 올바르지 않으면(형식 버전, VM 이름·스펙, 디스크 수가 스펙과 같고 디스크 i가 `disk{i}`·
    `disk{i}.qcow2`) **400**, 이 노드에서 풀을 쓸 수 없으면 **409** `pool_not_on_node`·`pool_unavailable`, 같은 원본의
    사본이 같은 풀에 있으면 **409** `replica_exists`(`details.backup`이 그 id), 풀의 남은 공간이 디스크 파일 크기의
    합보다 작으면 **507** `insufficient_storage`.
  - `PUT /api/v1/backups/{key}/files/{file}`(본문은 파일 그대로, `Content-Length` 필수 — 없으면 **411**) → **200**
    `{"sha256", "size"}`. `file`은 매니페스트의 디스크 파일이나 UEFI면 `vars.fd`뿐(그 밖은 **400**)이고,
    `Content-Length`는 디스크 파일이면 매니페스트의 `stored_bytes`와 같아야, `vars.fd`면 64 MiB까지다(아니면 본문을
    읽기 전에 **400**). `{file}.part`에 받으며 128 MiB마다 내려 쓰고, 끝에 fsync한 뒤 이름을 바꾸고, 디스크 파일의 해시와
    받은 시각을 사본 상태에 적는다. 본문 조각이 60초 넘게 오지 않으면 받던 `.part`를 지우고 **400** `invalid_request`
    ("the upload stalled")다(그 파일을 곧바로 다시 보낼 수 있다). 받는 중이 아니면 **409** `backup_not_receiving`, 다른 노드가 받는 백업이면
    **409** `backup_on_other_node`, 같은 파일을 이 노드가 받고 있으면 **409** `backup_busy`.
  - `POST /api/v1/backups/{key}/actions/complete` → **200** 백업. 디스크 파일의 크기가 매니페스트와 같고 `vars.fd`가
    있어야 한다(아니면 **409** `backup_incomplete`, `details.files`). 지우는 Task가 걸려 있으면 **409** `backup_busy`.
    `manifest.json`의 `backup`은 dr의 사본 id로 쓴다.
  - dr의 감사 로그에는 이 요청들이 토큰 주인의 것으로 남는다. `receiving` 백업은 복원할 수 없고, 지우면 받은 파일도
    지운다.

**DR 절차.**

1. dr에 백업 풀(NFS가 좋다), VM 디스크 풀, prod와 같은 이름의 네트워크·보안 그룹, prod가 쓸 operator 사용자와 그 API
   토큰을 만든다.
2. prod에 dr을 원격으로 등록하고(`oplctl remote add dr …` — dir 백업 풀이면 그 노드의 URL), 백업 작업에
   `--replicate dr/<풀>`을 준다. `oplctl backup list`의 `REPLICAS`가 `dr:ready`인지, 실패면 `error`를 본다.
3. prod를 잃으면 dr에서 `oplctl backup list`(웹 백업 화면)로 `ORIGIN`이 `prod/…`인 사본을 골라 `oplctl backup restore
   <사본> --name <VM> --pool <풀>`로 복원하고 시작한다. 새 VM은 새 MAC이다(MAC으로 묶은 DHCP 예약·라이선스는 다시
   맞춘다). 사본이 원본의 마지막 백업 시각까지의 데이터다(RPO는 백업 주기).
4. dr에서 돌린 VM을 prod로 되돌리려면 같은 방법을 거꾸로(dr에 prod를 원격으로 등록하고 백업을 복제해 prod에서
   복원) 한다. prod를 새로 만들었으면 옛 원본이 없어 dr의 옛 사본은 보존 규칙으로 지워지지 않으므로 dr에서 지운다.

- **한계.**
  - 매번 백업 전체를 보낸다(증분·중복 제거·압축 추가 없음 — 백업 파일 그대로). 대역폭 제한이 없어 큰 백업은 링크를
    채운다(스케줄러는 노드마다 복제 하나). 진행률이 없다.
  - 보내는 동안 멈추면 처음부터 다시 보낸다(파일 단위로 다시 쓴다 — 이어 보내기 없음). 파일 하나를 24시간 넘게
    보내면(마지막으로 받은 파일 뒤 24시간) dr이 그 `receiving` 사본을 지우므로 그런 복제는 끝나지 못한다.
  - 원본을 지울 때 dr에 닿지 못하거나(꺼짐·토큰 거절) `force`로 지우면 사본이 dr에 남는다. dr에서 사본을 지우면 prod는
    다음 작업 실행(손으로 만든 백업이면 다음 `oplctl backup replicate`)에서야 알고 다시 보낸다.
  - **prod 쪽 권한이 dr의 백업에 닿는다.** prod에 등록한 operator 토큰은 dr의 모든 백업을 지울 수 있고, prod에서
    원본을 지우면(사용자·보존 규칙) dr의 사본도 곧바로 지워진다 — prod를 빼앗긴 사람이나 실수가 DR 사본까지 없앨 수
    있다(지우기 지연·보관 잠금은 없다). 오래 지켜야 할 것은 dr 쪽에서 따로 보관한다(예: 사본을 복원한 VM을 dr의 백업
    작업으로 백업). 반대로 dr의 operator는 출처를 꾸민 사본을 만들 수 있다(받는 쪽은 출처를 확인하지 않는다). prod는
    사본의 원본 id·풀·스펙·디스크를 확인하고 ready 사본은 해시를 비교하므로 다른 내용의 사본에 붙지는 않지만, 그런
    사본이 있으면 복제가 실패하므로 dr에서 지워야 한다.
  - 원격 토큰이 Raft에 평문인 것, 원격 URL이 노드 하나인 것 등 "원격 클러스터"의 한계를 그대로 가진다. 복원은 VM
    스펙을 바꾸지 못하므로 네트워크·보안 그룹 이름을 dr에 맞춰 둔다. 자동 장애 조치·복구 계획 실행·연속(디스크 단위)
    복제는 없다.

## 이미지 업로드와 머신 종류
````

`README.md`에서

```markdown
- [설치·업데이트·구독 설계 (하위 프로젝트 9: 계획 31·32·33)](docs/superpowers/specs/2026-11-05-install-update-subscription-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [설치·업데이트·구독 설계 (하위 프로젝트 9: 계획 31·32·33)](docs/superpowers/specs/2026-11-05-install-update-subscription-design.md)
- [엔터프라이즈 설계 (하위 프로젝트 10: 계획 34~38)](docs/superpowers/specs/2026-11-06-enterprise-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 38은 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR: 백업의 출처 `origin`·받는 중 `receiving`·원격 사본 기록 `replicas`, 받는 쪽 API `POST /backups/receive`·`PUT /backups/{key}/files/{file}`(스트리밍, SHA-256과 크기를 돌려줌)·`complete`(매니페스트를 받는 쪽 id로 쓰고 `ready`)와 24시간 넘게 받는 중인 사본을 지우는 janitor, 보내는 쪽 `backup-replicate` Task(계획 37의 원격 클라이언트로 파일마다 해시를 비교하며 흘려 보냄, 같은 출처의 사본이면 이어 감)와 수동 복제 `POST /backups/{key}/actions/replicate`, 백업 작업의 `replicate`(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 다시, 노드마다 하나), 원본을 지우면(사용자·보존 규칙) 원격 사본도 지움(실패는 경고), 받은 사본은 그 클러스터의 보통 백업이라 그곳에서 복원, `oplctl backup replicate`·목록의 `ORIGIN`·`REPLICAS`·`backup-job --replicate`, 웹 출처 배지·사본 상태·"원격으로 복제"·작업의 원격 복제, README DR 절차, 완료)으로 나눴다(`2026-11-06-enterprise-design.md`). 하위 프로젝트 10은 끝났다. 감사 로그 변조 방지와 원격 syslog 직접 전송·요청 본문 기록, SAML·Kerberos, LDAP 사용자·그룹 동기화, 세분화된 RBAC, 외부 사용자의 2단계 인증, oplctl의 SSO 로그인, 원격 클러스터의 전체 웹 화면·콘솔 중계·클러스터 사이 라이브 마이그레이션, 디스크 단위 연속 복제, 자동 장애 조치와 복구 계획 실행, 복제의 대역폭 제한·증분 전송은 넣지 않았다. 이로써 처음 분해한 하위 프로젝트 1~10이 모두 끝났다.
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown

## 7. 오류
```

을 다음으로 바꾼다:

```markdown

### 6.6 구현(계획 38)

- 타입(`opl_types::backup`, `backup_job`, `task`): `BackupSpec.origin: Option<BackupOrigin{cluster, ca_fingerprint, backup, job?}>`(`deny_unknown_fields`, None이면 쓰지 않음 — `job`은 원본 작업의 **이름**, 손으로 만든 백업이면 없음), `BackupPhase::Receiving`(`"receiving"`), `BackupStatus.replicas: Vec<BackupReplica{remote, pool, backup?, state: ReplicaState{copying, ready, failed}, error?, at}>`(비면 쓰지 않음, API 문서에서도 선택 필드 — 설계 §6.2의 항목에 `pool`과 `copying`을 더함: 한 원격의 여러 풀로 보낼 수 있고, 보내는 중을 보이게), `BackupJobSpec.replicate: Option<ReplicateTarget{remote, pool}>`(None이면 쓰지 않음, `deny_unknown_fields`, 검증은 두 이름이 리소스 이름 규칙 — `BackupJobError::{ReplicateRemote, ReplicatePool}`), `TaskOp::BackupReplicate{remote, pool}`(`"backup-replicate"`, 대상은 원본 백업). 리뷰 뒤 `BackupDisk.sha256`(받은 사본의 파일 해시)과 `BackupStatus.received_at`(받는 중 마지막으로 파일을 받은 시각)을 더함(둘 다 None이면 쓰지 않음). `receiving` 백업은 복원(ready만)·작업 보존(작업의 백업만)에서 저절로 빠진다.
- 받는 쪽(`opl_api::backup_receive`, 모두 `Kind::Backup` 쓰기 — operator 이상): `POST /backups/receive {name?, pool, manifest, origin}` → 201 `{id, name}`. 순서: 이름 규칙 400 → 옛 노드 409 `cluster_upgrading` → 풀이 없거나 백업 풀이 아니면 400 → 요청을 받은 노드에서 풀을 쓸 수 없으면 409 `pool_not_on_node`(원격 등록 URL을 풀이 있는 노드로 하거나 NFS 백업 풀을 쓰라는 메시지)·`pool_unavailable` → 매니페스트 400(형식 버전, VM 이름·스펙 검증, 디스크 수가 스펙과 같고 디스크 i는 정확히 `disk{i}`·`disk{i}.qcow2` — 설계의 "`disk<N>.qcow2` 모양"보다 좁힘) → 같은 `origin.ca_fingerprint`+`origin.backup`의 사본이 **같은 풀에** 있으면 409 `replica_exists`(`details.backup` — 다른 풀이면 새 사본) → 풀의 남은 공간(`fs4::available_space`)이 디스크 `stored_bytes` 합보다 작으면 507 `insufficient_storage`(볼륨 업로드와 같은 코드). 이름은 주면 그것, 없으면 `default_backup_name(vm_name, created_at)`이고 겹치면 `free_name`이 `-2`, `-3`…(63자를 넘지 않게 앞을 줄임). 리소스(spec은 매니페스트의 vm·vm_name·vm_spec과 origin, job 없음; status는 `receiving`·disks(해시는 비움)·uefi_vars·created_at, 공유가 아닌 풀이면 `node` = 이 노드)를 만든 뒤 `{root}/backups/{id}`를 만들고 `backups`·풀 루트를 fsync(실패하면 리소스를 지움). 풀 경로는 새 `AppState.mount_root`(opld가 `{data_dir}/pools` — Task 실행기의 `mount_root`와 같음)로 `check_pool_root`. `PUT /backups/{key}/files/{file}`: `Content-Length` 없으면 411(`volumes::content_length`), `receiving`이 아니면 409 `backup_not_receiving`, `status.node`가 다른 노드면 409 `backup_on_other_node`, `file`은 매니페스트 디스크의 `file`이나(uefi_vars면) `vars.fd`만 400(정해진 이름과 같아야 하므로 `..%2F` 같은 경로 문자도 여기서 걸림), `Content-Length`가 디스크 파일이면 `stored_bytes`와 다르거나 `vars.fd`면 64 MiB를 넘으면 본문을 읽기 전에 400, 같은 (백업, 파일)을 이 노드가 받고 있으면 409 `backup_busy`(`AppState.receiving_files` 메모리 표시 — 노드 사이는 `backup_on_other_node`가 막음), `{file}.part`에 길이만큼 받으며 SHA-256(길면·짧으면 400)·128 MiB마다 `sync_data`(본문 조각을 60초 — `AppState.receive_idle_timeout`, 시험이 줄임 — 까지 기다리고 넘으면 `.part`를 지우고 표시를 풀어 400 `invalid_request` "the upload stalled"; 리뷰의 408 `request_timeout`은 기존 코드에 없는 코드라 400) → `sync_all` → 이름 바꾸기 → 디렉터리 fsync → 디스크 파일이면 그 `sha256`과 `received_at = now`를 상태에 적음(revision 확인 쓰기, 어긋나면 다시, 받는 중이 아니게 됐으면 그만둠), 200 `{sha256(소문자 hex), size}`, 다시 보내면 덮음, 실패하면 `.part`를 지움. `POST /backups/{key}/actions/complete`: 지우기 Task가 걸려 있으면 409 `backup_busy`, 디스크 파일이 일반 파일이고 크기가 `stored_bytes`와 같고 `vars.fd`가 있어야(아니면 409 `backup_incomplete`, `details.files`) — **해시는 보지 않음**(보내는 쪽이 파일마다 PUT 응답으로 비교). `manifest.json`을 `backup` = 받는 쪽 id로 써(복원이 `manifest.backup == id`를 봄) `tasks::backup::write_manifest`로 원자적으로 쓰고 `ready`(읽은 revision에서만 `update_status_checked` — 어긋나면 다시 읽어 받는 중·지우기를 다시 확인하고 매니페스트도 다시 읽은 상태로 다시 씀), 200 백업. `receive`라는 이름의 백업은 이름으로 부를 수 없다(id로는 됨 — 라우트가 겹침). 받는 중 지우기는 기존 지우기 Task가 디렉터리째 지운다. janitor(`ClusterJanitorConfig.receiving_max_age`, 기본 24시간, opld 설정 없음): 만든 시각(UUIDv7 id)과 마지막으로 파일을 받은 시각(`received_at`) 가운데 늦은 것부터 그보다 오래 `receiving`이고 끝나지 않은 지우기 Task가 없는 백업을 `start_backup_delete`로 지운다(만들지 못하면 다음 tick).
- 보내는 쪽(`backups::replicate_backup`, `start_backup_replicate`(pub(crate) — 스케줄러도 씀), `tasks/replicate.rs`): `POST /backups/{key}/actions/replicate {remote, pool}` → 202 Task. 순서: 백업 쓰기 403 → 풀 이름 규칙 400 → 옛 노드 409 → 백업·원격(이름이나 id) 404 → `ready`가 아니면 409 `backup_not_ready` → 지우는 중이거나 같은 원격으로 끝나지 않은 복제 Task가 있으면 409 `backup_busy` → 노드는 지우기와 같은 `files_node`(공유가 아닌 풀은 `status.node`, 오프라인이면 409 `node_offline`). Task는 백업의 줄(지우기와 차례로 — 지우기 API도 복제 중이면 409 `backup_busy` "being replicated")에 서고 시간 제한이 없다(원격 클라이언트의 조각 사이 60초만). 실행: 원본 `replicas`의 (remote, pool) 항목이 ready가 아니면 `record`(읽은 revision에서만 `update_status_checked`, 어긋나면 다시)로 copying(ready 기록은 확인이므로 그대로 — 아래) → 원격 spec의 `ca_pem`·토큰으로 `RemoteClient` → 공유가 아닌 풀의 다른 노드 백업이면 실패 → 매니페스트를 읽어 원본 기록과 확인 → 그 (remote, pool) 기록에 사본 id가 있으면 원격 `GET /backups/{id}`(404면 원격에서 지운 것 — 새로 받기부터) → 없으면 `POST /backups/receive`(name = 원본 이름, origin = 이 클러스터 `ClusterSpec`의 이름·CA 지문·원본 id·원본 작업 이름(작업이 아직 있으면)), 409 `replica_exists`면 그 사본을 `GET` → 이미 있는 사본은 타입(`Backup`)으로 읽어 출처의 원본 id·풀(요청한 풀)·`vm_spec`·디스크 이름과 `stored_bytes`·`uefi_vars`·`receiving`이나 `ready`인지 확인(다르면 "a different backup claims to be the copy of <원본 id> on remote <r>; delete it there"), `ready`면 원본에 적힌 디스크 `sha256`(없으면 — 해시를 적기 전의 백업 — 파일을 읽어 계산하고 `record_hash`로 원본에 적음)을 사본의 `status.disks[].sha256`과 비교해 모두 같을 때만 보내지 않고 성공(없거나 다르면 "… is ready but its files do not match …; delete it there and replicate again" — 리뷰의 "파일을 다시 보내고 complete"는 ready 사본이 파일을 받지 않으므로(409 `backup_not_receiving`) 보내지 않고 바로 실패로 알림), `receiving`이면 그 사본으로 이어 감(모든 파일을 다시 보냄) → 사본 id를 기록 → 파일마다 256 KiB 조각으로 읽으며 해시하는 스트리밍 본문의 `PUT`(`Content-Length`, 다 보낸 뒤 응답을 120초까지 기다림 — `RemoteClient::request_with_timeout`, 다른 요청은 10초) → 응답 해시·크기가 다르면 실패, 같으면 그 해시를 원본 디스크 `sha256`에 적음(`record_hash` — 이미 같으면 쓰지 않음, revision 확인 쓰기) → `complete` → ready와 사본 id. 실패(재시작으로 끝내지 못한 Task 포함)는 실행기 `cleanup`이 `replicate::discard`로 failed와 오류를 적는다. 기록이 ready인 사본의 확인은 기록을 copying으로 바꾸지 않는다: 성공하면 `at`만 고치고(오류는 지움), 실패하면(원격에 닿지 못함 등) `discard`가 ready 그대로 `error`와 `at`만 적어 다음 실행이 다시 확인한다. 원격에 없으면(404) 그때 copying으로 바꿔 다시 보내고, 다른 사본이거나 ready 사본의 해시가 다르면 failed(`mismatch`). 그래서 실행마다의 확인은 원격 GET과 문자열 비교뿐이다. 원격 오류 문구는 `cannot <하던 일> on remote "<이름>": <RemoteError>`(원격의 오류 본문 `message`를 옮김). **진행률은 넣지 않았다**(설계의 "진행률은 Task 진행률" — `TaskStatus`에 진행률 필드가 없음).
- 백업 작업(`backup_jobs.rs`): 만들기·고치기의 `check_spec`이 `replicate`가 있으면 `require_current`(409)와 등록된 원격(없으면 400 — 원격의 풀은 보낼 때 원격이 확인). `PATCH`의 `replicate`는 `present` 역직렬화로 객체면 바꾸고 `null`이면 끄고 없으면 그대로(스키마 `replicate?: ReplicateTarget | null`). 스케줄러 tick이 작업마다 `replicate`: 켜 둔 작업의 `to_replicate`(그 작업의 `ready` 백업 가운데 같은 원격으로 끝나지 않은 복제 Task가 없고, (remote, pool) 기록이 없거나 기록의 `at`이 진행 중인 실행 또는 마지막 실행의 시작보다 앞선 것(상태와 관계없이) — 그래서 실패·멈춘 copying은 실행마다 한 번 다시, ready 사본도 실행마다 한 번 복제 Task가 원격에 물어 같은 파일이면 `at`만 고치고 원격에서 지웠으면 다시 보냄, 실행한 적이 없으면 기록이 있는 것은 다시 보내지 않음, 켜기 전에 만든 그 작업의 백업도 보냄; 만든 순서)를 옛 노드가 없을 때만, 노드마다 복제 Task 하나·백업과 복제 Task(`occupies`)를 합쳐 `max_concurrent`까지 `start_backup_replicate`로 만든다(4xx는 로그만 남기고 다음 tick). 그 작업의 보존 규칙이 지울 백업(`retention`을 tick마다 지금 백업으로 계산)은 `to_replicate`에서 빼서 확인·복제하지 않는다. 백업 `dispatch`도 복제 Task를 센다. 보존 정리는 `advance`에서 떼어 `prune_after_run`(tick마다, 실행이 끝나 있을 때)으로 했다: `retention`의 백업마다 `start_backup_delete`, 만들지 못한 것(`backup_busy` 등)은 다음 tick에 다시, 실행이 끝난 뒤(UUIDv7 시각 ≥ `last_run.finished_at`) 지우기 Task를 이미 만든 백업은 그 Task가 실패했어도 다음 실행이 끝날 때까지 다시 만들지 않음 — 그래서 실행 뒤에 보존 값을 줄이면 다음 tick에 지운다. 리뷰의 "실행 시작 때 지울 목록을 계산"은 tick마다 지금 백업으로 계산한 것이라 이번 실행이 만들 백업까지 내다보지는 않는다(실행이 끝날 때 확인과 겹치면 위의 다시 하기가 정리한다).
- 지우기 전파(`tasks/backup.rs` `delete`): 파일 노드 확인 뒤 원본 디렉터리를 먼저 지우고(실패하면 Task가 실패하고 원격은 건드리지 않음), `replicate::delete_copies`가 `replicas`의 사본 id가 있는 사본마다(failed도 — 받다 만 사본이 원격에 있음) 원격 `DELETE /backups/{id}`(2xx·404는 성공 — 원격의 지우기 Task를 기다리지 않음). 실패한 것은 원본을 지운 뒤 Task를 성공으로 끝내되 `status.error`에 "backup deleted, but remote copies may be left: …"(실행기의 `perform`이 `Result<Option<String>, String>`을 돌려줘 성공한 Task에 경고를 남길 수 있게 함). 보존 규칙과 사용자 지우기가 모두 이 Task를 쓴다. `DELETE …?force=true`로 리소스만 지우는 경로는 원격을 건드리지 않는다.
- oplctl: `backup replicate <백업> --remote <r> --pool <p> [--wait]`(기다리지 않으면 Task id와 `oplctl task get`, `--wait`이면 `BACKUP_WAIT`(24시간)), `backup list`에 `ORIGIN`(`<클러스터>/<원본 id 앞 8자>` — 설계의 "원본 이름 또는 id 앞 8자"에서 id만: 사본에 원본 이름이 없음)·`REPLICAS`(`<원격>:<상태>` 쉼표, 없으면 `-`), `backup get`은 JSON 그대로, `backup-job create --replicate R/P`(주지 않으면 `replicate`를 보내지 않음 — 옛 서버가 모르는 필드를 거절), `backup-job set --replicate R/P | --no-replicate`(`{"replicate": null}`), `REMOTE/POOL`이 아니면 파싱 오류, `backup delete`가 Task의 경고를 `warning: …` 줄로. `backup-job list`에는 복제 열을 더하지 않았다(`get`의 JSON).
- 웹(`web/src/backups/`): 백업 표(백업 화면·VM 백업 탭)의 받은 사본 배지("<클러스터>에서 받음", 툴팁에 원본 id·작업), "원격 사본" 열(사본마다 상태 아이콘 — 준비됨·복제 중·실패 — 과 원격 이름, 툴팁에 원격/풀·상태·오류), `receiving` 배지, 백업 쓰기 권한이면 "원격으로 복제" 대화상자(ready만; 원격은 `GET /remotes`, 그 원격의 풀은 중계 `GET /storage-pools`를 `fetch`로 읽어 백업 풀만 — `remoteStoragePools`, 고르기는 `RemoteTargetFields`), Task를 따라감. 백업 작업 표의 "원격 복제" 열과 만들기·고치기의 "원격 클러스터로 복제"(복제하지 않음 / 원격과 그 백업 풀 — 만들기는 고를 때만 보내고 고치기는 바뀔 때만, 끄면 `replicate: null`). Task 이름 `backup-replicate`, `backup_busy`·`backup_not_ready`·지우기 설명 문구 갱신, ko·en. 리뷰 뒤: 출처 배지의 원본 id·작업과 사본의 원격/풀·상태를 화면 읽기용(sr-only) 글로도 두고 실패한 사본은 "원격 / 풀: 오류"를 사본 아래 글로 보임, `settleTasks`는 성공한 Task에 `status.error`가 있으면 닫을 때까지 남는 경고 알림(지우기의 "원격 사본이 남았을 수 있음"). 설계 §6.4의 "백업 상세"는 따로 없어 백업 표의 행 버튼으로 했다.
- 시험: opl-types 단위(JSON 왕복, 기본값이면 옛 모양 그대로, `origin` 모르는 필드 거절, `replicate` 검증, `TaskOp` 모양); `tests/backup_receive.rs`(받기 기록·출처·이름 번호, viewer 403·operator 됨, 풀·매니페스트 검사와 `pool_not_on_node`, `replica_exists`와 다른 풀의 새 사본, 크기 제한(디스크 크기 다름·`vars.fd` 64 MiB 초과 400·자리 없음 507), 같은 파일 동시 PUT 409, 멈춘 업로드가 끊기고 같은 파일을 다시 받음, 해시·`received_at` 기록, 지우는 중 complete 409, 해시·크기·덮기·모르는 파일·`..%2F`·411·BIOS의 `vars.fd`, complete의 빠진 파일·크기 다름·받는 쪽 id 매니페스트·그 뒤 409, 다른 노드 409, 받는 중 지우기, 옛 노드 409); `tests/backup_tasks.rs`(storage-it) 받은 사본 복원이 같은 디스크 내용; janitor 단위·통합; `tests/backup_replicate.rs`(두 `Env`, 받는 쪽 `serve_tls` — 공통 `Env::serve_tls_with(tweak)`: 원격에 같은 파일의 ready 사본·출처·받는 쪽 id 매니페스트, 해시를 바꾸는 원격이면 실패와 failed·사본은 받는 중, 꺼진 원격, 원격 viewer 토큰 403 기록, 받는 중 사본 이어 가기(반쯤 보낸 파일을 덮음)와 ready 사본 다시 복제·해시가 다르면 실패, 보낸 해시가 원본·사본에 적히고 옛 백업은 계산해 적으며 원본 파일을 지워도 ready 사본 확인이 성공, ready 사본 확인 중 원격이 꺼져도 기록은 ready(error·at만), 다른 VM 스펙의 사본 주장은 실패, 원격에서 지운 사본은 다시 보냄, API 검사(잘못된 풀 이름 400·`node_offline` 포함), 작업이 ready 백업을 복제하고 손으로 만든 백업은 보내지 않음과 보존 규칙이 지우면 원격 사본도 지움, 확인 Task가 도는 중에 실행이 끝나도 Task가 끝나면 보존 정리가 됨, 실행 전에 실패한 사본만 다시(나중 백업이 보내질 때까지 tick이 돈 뒤 확인)와 원격에서 지운 사본을 다음 실행에 다시, 지우기가 원격 사본을 지움(실패 사본도)·원격에서 이미 지운 사본 404는 성공·로컬 파일을 못 지우면 원격을 건드리지 않음, 원격이 꺼지면 경고하고 원본은 지움, force는 원격 사본을 남김); `tests/backup_jobs.rs`(PATCH replicate 켜기·그대로·끄기·검사·옛 노드 409, 없는 원격 400); `to_replicate`(보존 규칙이 지울 백업은 고르지 않음 포함)·`free_name`·janitor(활동이 있으면 남김) 단위; `tests/remote_client.rs`의 `request_with_timeout`; openapi 경로; oplctl 파싱·목록 행·작업 본문·fake_api(replicate 요청·`--wait`·지우기 경고); opld E2E `tests/replicate.rs`(기능 `qemu-it` — 백업·복원이 `qemu-img`를 써서 CI linux-kvm 작업에서 돎, VM은 켜지 않음: 한 노드 클러스터 둘, dr의 operator 사용자 토큰으로 prod에 등록, 꺼진 VM(raw 디스크 표식) 백업 → `backup replicate --wait` → 원본 `dr:ready`, dr에 같은 이름 ready 사본(출처·CA 지문, `ORIGIN`), dr에서 복원한 디스크 내용이 같음, prod에서 원본을 지우면 dr 사본과 파일도 지워짐); 웹 Vitest(출처 배지·사본 툴팁과 sr-only·실패 오류 글, 성공 Task의 경고 알림, 복제 대화상자, 원격 풀 읽기 오류, ready가 아니면 막힘, viewer는 버튼 없음, 작업의 원격 복제 열·만들기·끄기, `jobs.ts` 변환, Task 이름).
- 설계와 다른 점(위에 적은 것 밖): DR 절차 §6.3의 "없으면 복원 화면에서 바꿈 — 계획 27"은 없는 기능이다(복원은 VM 스펙의 네트워크·보안 그룹이 있어야 하고 바꾸지 못함) — README는 DR 클러스터에 같은 이름을 미리 만들라고 적는다. 사본 이름은 원본 이름(`default_backup_name`은 이름을 주지 않을 때만).

## 7. 오류
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
| `remote_is_local` | 400 | 자기 클러스터를 원격으로(받은 CA 지문이 이 클러스터의 것) |
| `remote_unreachable` | 502 | 중계·복제 중 원격에 닿지 못함; 등록·바꾸기·`probe`에서 CA를 받지 못함(CA가 인증서 하나가 아님 포함), whoami의 401이 아닌 실패, 확인 30초 넘음, 중계의 연결·TLS·시간 제한(계획 37) |
| `remote_path_unsupported` | 404 | watch·콘솔 중계(첫 조각 `watch`, `vms/<x>/console` — 퍼센트 인코딩을 풀어 봄) |
| `replica_exists` | 409 | 같은 출처의 사본이 이미 있음 |

새 Kind(`Realm`, `Remote`)와 새 필드(`UserSpec.realm`, `BackupJobSpec.replicate`, `BackupSpec.origin`, `BackupStatus.replicas`)를 쓰는 모든 곳은 쓰기 전에 `require_current`(409 `cluster_upgrading`), 새 필드는 기본값이면 직렬화하지 않는다.
```

을 다음으로 바꾼다:

```markdown
| `remote_is_local` | 400 | 자기 클러스터를 원격으로(받은 CA 지문이 이 클러스터의 것) |
| `remote_unreachable` | 502 | 중계 중 원격에 닿지 못함(복제 Task의 원격 실패는 HTTP 코드가 아니라 Task 오류와 사본 기록 `failed` — 계획 38); 등록·바꾸기·`probe`에서 CA를 받지 못함(CA가 인증서 하나가 아님 포함), whoami의 401이 아닌 실패, 확인 30초 넘음, 중계의 연결·TLS·시간 제한(계획 37) |
| `remote_path_unsupported` | 404 | watch·콘솔 중계(첫 조각 `watch`, `vms/<x>/console` — 퍼센트 인코딩을 풀어 봄) |
| `replica_exists` | 409 | 받기에서 같은 출처(`origin.ca_fingerprint`·`origin.backup`)의 사본이 같은 풀에 이미 있음(`details.backup` — 보내는 쪽은 그 사본을 확인하고 이어 감) |
| `insufficient_storage` | 507 | 받기에서 백업 풀의 남은 공간이 디스크 파일 크기의 합보다 작음(볼륨 업로드의 코드를 다시 씀 — 계획 38) |
| `backup_not_receiving` | 409 | 받는 중(`receiving`)이 아닌 백업에 파일 보내기·완료(계획 38) |
| `backup_incomplete` | 409 | 완료할 때 빠졌거나 크기가 매니페스트와 다른 파일이 있음(`details.files` — 계획 38) |
| `pool_not_on_node`·`pool_unavailable` | 409 | 받기 요청을 받은 노드에서 받을 백업 풀을 쓸 수 없음(공유가 아닌 dir 풀이 다른 노드에 있음 — 원격 URL을 그 노드로 하거나 NFS 백업 풀; 계획 27의 코드를 다시 씀) |
| `backup_not_ready`·`backup_busy` | 409 | 복제: 원본이 `ready`가 아님, 지우는 중이거나 같은 원격으로 복제 중; 복제 중인 백업 지우기("being replicated"); 받기: 같은 파일을 이 노드가 받는 중인 PUT, 지우기 Task가 걸린 백업의 complete(계획 27의 코드를 다시 씀) |

새 Kind(`Realm`, `Remote`)와 새 필드·값(`UserSpec.realm`, `BackupJobSpec.replicate`, `BackupSpec.origin`, `BackupStatus.replicas`, `BackupPhase::Receiving`, `TaskOp::BackupReplicate`)을 쓰는 모든 곳은 쓰기 전에 `require_current`(409 `cluster_upgrading`), 새 필드는 기본값이면 직렬화하지 않는다.
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- 복제는 전체 백업을 매번 보낸다(증분 없음). 원본을 지울 때 원격에 닿지 못하면 사본이 남는다.
```

을 다음으로 바꾼다:

```markdown
- 복제는 전체 백업을 매번 보낸다(증분 없음). 원본을 지울 때 원격에 닿지 못하면 사본이 남는다.
- (계획 38) 대역폭 제한·진행률·이어 보내기가 없다(멈추면 파일을 모두 다시 보냄). 받는 쪽 janitor는 만든 지, 그리고 마지막으로 파일을 받은 지 24시간이 지난 `receiving` 사본을 지우므로 파일 하나에 24시간 넘게 걸리는 복제는 끝나지 못한다. 원격 사본 지우기는 원격의 지우기 Task를 요청할 뿐 끝나기를 기다리지 않는다. `force`로 지운 원본과 원격에 닿지 못할 때 지운 원본의 사본은 원격에 남는다(경고만 — oplctl `warning:`, 웹 경고 알림). 원격에서 사본을 지우면 작업의 다음 실행(손으로 만든 백업은 다음 손 복제)에서야 알고 다시 보낸다(그 확인은 원격 GET과 적힌 해시의 비교뿐 — 해시를 적기 전의 옛 백업만 한 번 파일을 읽는다). 확인할 때 원격에 닿지 못하면 기록은 ready로 남고 `error`만 적히므로 사본이 아직 있는지는 다음 확인까지 모른다. 작업의 보존 규칙이 지울 백업은 확인하지 않는다. 받는 쪽은 본문 조각이 60초 넘게 오지 않는 업로드를 끊으므로 그만큼 멈추는 링크에서는 복제가 실패한다(다시 보냄). 받는 쪽 파일은 받기 요청을 받은 노드가 쓰므로 공유가 아닌 dir 백업 풀은 원격 URL의 노드에 있어야 하고 그 노드를 잃으면 사본도 잃는다. 같은 파일 동시 PUT 막기는 노드 메모리라 노드 사이는 `backup_on_other_node`(공유가 아닌 풀)만 막는다(NFS 풀의 같은 사본에 두 노드가 함께 쓰는 것은 막지 않음). 받는 쪽 완료는 크기만 보고(해시는 보내는 쪽이 파일마다, ready 사본은 받은 쪽이 적은 해시로 비교), 받기 API는 operator 이상 누구나 부를 수 있어 받는 클러스터의 operator가 출처를 꾸민 사본을 만들 수 있다 — 보내는 쪽은 원본 id·풀·스펙·디스크를 확인하고 ready 사본의 해시를 비교하므로 다른 내용의 사본에 붙지 않지만, 그런 사본이 있으면 복제가 실패하므로 받는 쪽에서 지워야 한다. 반대로 보내는 쪽에 등록한 operator 토큰은 받는 쪽의 모든 백업을 지울 수 있고 원본 지우기가 곧바로 사본 지우기로 이어지므로, 보내는 클러스터를 빼앗기거나 잘못 지우면 DR 사본도 없어진다(지우기 지연·보관 잠금 없음 — 오래 지킬 것은 DR 쪽 백업 작업으로 따로 보관). 두 클러스터는 같은 버전이어야 한다(받는 쪽이 모르는 필드는 400). 복원은 VM 스펙을 바꾸지 못하므로 DR 클러스터에 같은 이름의 네트워크·보안 그룹이 있어야 한다. 사본에는 작업이 없어 받는 쪽 보존 규칙이 지우지 않으므로, 보낸 클러스터를 잃으면 사본은 손으로 지운다. 자동 장애 조치·복구 계획 실행은 없다.
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 원격 복제(DR) — README 새 절 \"원격 복제(DR)\"(개념: 사본은 원격 클러스터의 보통 백업, 보내는 길 — backup-replicate Task가 파일 노드에서 받기 API로 파일마다 SHA-256을 비교하며 흘려 보냄·시간 제한 없음·진행률 없음, 받는 쪽 준비 — 백업 풀, operator 토큰, dir 풀이면 원격 등록 URL을 그 노드로(pool_not_on_node)·DR에는 NFS가 나음, 같은 이름의 네트워크·보안 그룹, 사본 이름과 출처·ORIGIN 열·배지, 사본 상태 copying/ready/failed와 REPLICAS 열, 다시 하기 — replica_exists로 이어 감·24시간 넘은 receiving은 지움, 수동 복제 oplctl backup replicate와 409 코드·웹 \"원격으로 복제\", 작업 복제 --replicate/--no-replicate·스케줄러가 ready 백업을 보내고 실패는 실행마다 다시·노드마다 하나, 지우기 전파와 경고·force는 원격을 건드리지 않음, 업그레이드 중 409, 받기 API, DR 절차 네 단계, 한계 — 매번 전체 전송·대역폭 제한 없음·이어 보내기 없음·24시간·원격에 남는 사본), 백업 절의 한계와 웹 UI 기능 목록에 복제. 엔터프라이즈 설계 §6.6 \"구현(계획 38)\"(타입, 받는 쪽·보내는 쪽·작업·지우기 전파·oplctl·웹·시험, 설계와 다른 점 — replicas의 pool·copying, 진행률 없음, janitor는 마지막 활동 기준, ORIGIN은 id 앞 8자, 복원에서 네트워크를 바꾸는 기능은 없음), §7 오류(backup_not_receiving·backup_incomplete·받기의 pool_not_on_node·복제의 backup_not_ready·backup_busy, replica_exists·remote_unreachable 설명, require_current 대상), §9 알려진 틈(계획 38). 노드 코어 설계 §1.1의 하위 프로젝트 10 줄: 계획 38 완료, 하위 프로젝트 10 끝과 넣지 않은 것, 처음 분해의 하위 프로젝트 1~10이 모두 끝남"
```

---

## 완료 기준 (계획 38)

- 주 클러스터 A의 백업 작업에 `--replicate dr/<풀>`을 두면 작업이 만든 백업이 DR 클러스터 B에 사본(출처 A)으로 생기고, B에서 그 사본을 복원하면 같은 디스크 내용의 VM이 된다.
- 원본을 지우면(사용자·보존 규칙) B의 사본도 지워지고, B가 꺼져 있으면 경고가 남는다. 원격에서 사본을 지우면 다음 실행에 다시 보낸다.
- 사본은 해시로 확인되고, 위조되거나 다른 풀의 사본에는 붙지 않는다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, opld `qemu-it` 복제 E2E, 웹 `typecheck`·`lint`·`test`·`build`가 통과한다.

## 알고 받아들인 제한

- 매번 전체 백업을 보낸다(증분 없음), 대역폭 제한이 없다.
- 보내는 쪽의 operator 토큰은 DR의 백업을 지울 수 있다 — 원본 지우기가 DR로 이어진다. DR 쪽에서 따로 보관하려면 그쪽 백업 작업을 둔다.
- 받는 쪽 operator는 출처를 내세운 사본을 만들 수 있다(보내는 쪽 확인이 붙지 않게 막는다).
- 그 밖의 제한은 설계 문서 §6.6·§9와 README "원격 복제(DR)" 절에 있다.
