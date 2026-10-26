# opLite 계획 10: VM 스냅샷과 볼륨 크기 늘리기 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** VM의 한 순간(디스크, 실행 중이면 메모리, VM 설정)을 qcow2 내부 스냅샷으로 저장하고 되돌리고 지우며, 볼륨 크기를 늘린다. API·CLI·웹 UI 모두에서 한다(하위 프로젝트 3의 첫 부분).

**Architecture:** 스냅샷은 새 클러스터 리소스 `Snapshot`(나무 구조, 만든 순간의 VM 스펙)이고, 디스크 안의 qcow2 태그는 스냅샷 id다. 만들기·되돌리기·삭제·크기 늘리기는 Task로, VM이 배정된 노드에서 실행된다. 실행 중인 VM은 QEMU 11의 QMP 작업(`snapshot-save`/`snapshot-load`/`snapshot-delete`, `blockdev-snapshot-internal-sync`, `block_resize`)으로, 정지한 VM은 `qemu-img snapshot`/`resize`로 한다. 메모리 스냅샷으로 되돌리면 reconciler가 다음 시작에서 `-S`로 띄운 QEMU에 `snapshot-load`를 한 뒤 `cont`한다.

**Tech Stack:** Rust 2024(MSRV 1.88), QEMU 11.1(QMP 작업), qemu-img/qemu-io, openraft, axum 0.8, utoipa, clap; 웹은 계획 8·9와 같다(React 19.3, TanStack Query 5, shadcn/ui, i18next, Vitest, MSW, Playwright). 새 의존성은 `oplctl`의 `time`(이미 워크스페이스에 있다)뿐이다.

**설계 문서:** `docs/superpowers/specs/2026-10-26-snapshots-design.md` (§12의 계획 10)
**선행 계획:** 계획 1~9 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `Snapshot` 리소스(이름은 VM 안에서 고유, 부모, 메모리 여부, VM 스펙), VM의 현재 위치(`status.snapshot`) | 스냅샷에서 VM 복제 — 계획 11 |
| QMP 작업 도우미, 실행 중·정지 VM의 스냅샷 드라이버, 스냅샷을 불러오며 시작 | 예약 스냅샷 — 계획 11 |
| 만들기·되돌리기·삭제 API와 Task, 같은 VM의 작업 직렬화, 재시작 뒤 고아 정리 | 링크드 클론, raw → qcow2 변환, 스냅샷 이름 바꾸기 |
| 스냅샷이 남은 VM·스냅샷이 가리키는 볼륨의 삭제 막기 | 공유 스토리지(NFS·LVM·ZFS·iSCSI·Ceph) — 계획 12부터 |
| 볼륨 크기 늘리기(정지·실행 중) | 메모리 상태 크기(`vmstate_bytes`) 채우기 |
| 조건부 상태 쓰기(`UpdateStatusIf`)로 reconciler와 Task의 VM 상태 경합 막기 | |
| `oplctl vm snapshot create/list/revert/delete`, `vm delete --with-snapshots`, `volume resize` | |
| 웹: VM 상세 "스냅샷" 탭, 볼륨 크기 늘리기, VM 삭제에서 스냅샷도 지우기 | |
| 실제 QEMU 통합 테스트, 브라우저 E2E 단계 | |

## 이전 계획에서 이어받는 사항

계획 9의 "다음 계획으로 넘길 사항"(업로드 사전 확인 API, 시도 제한, 노드 아키텍처 등)은 이 계획에서 하지 않는다. 하위 프로젝트 3을 먼저 하기로 했다.

## 실행 환경

계획 9와 같다: macOS(Apple Silicon), Rust, `brew install qemu`(QEMU 11.1 이상: `snapshot-save` 등 QMP 작업과 `qemu-io`), Node.js 22.12 이상, pnpm 10.33.2, `pnpm -C web exec playwright install chromium`(한 번). 실제 QEMU 테스트는 기능으로 켠다: `opl-qemu`·`opl-driver`·`opl-reconciler`·`opld`의 `qemu-it`, `opl-api`·`opl-driver`의 `storage-it`, `opld`의 `web-it`(브라우저 E2E).

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `Cargo.lock`(cargo가 빌드하며 갱신), `web/src/api/schema.d.ts`(`pnpm -C web gen:api`). 둘 다 커밋한다.

## 설계 결정

- **qcow2 내부 스냅샷과 QMP 작업.** 디스크 체인을 관리하지 않아 되돌리기와 중간 스냅샷 삭제가 단순하다. 실행 중이면 QMP 작업(`devices`로 디스크를 고르므로 raw pflash인 UEFI 변수 파일이 막지 않는다), 정지했으면 `qemu-img snapshot -c/-a/-d`. raw 디스크가 붙은 VM은 스냅샷하지 않는다(409 `snapshot_unsupported`). UEFI 변수는 스냅샷마다 `{state_dir}/vm/{vm}/snapshots/{snapshot}.vars`로 복사한다.
- **리소스와 이름.** 리소스 이름은 스냅샷 id(클러스터 전체 고유), 사용자 이름은 `spec.name`이고 배타 값 `("name", "{vm}/{name}")`으로 VM 안에서 고유하다(겹치면 409 `name_taken`). API는 `/vms/{key}/snapshots/{name}`이고 `{name}`은 이름이나 id다.
- **작업 노드.** 스냅샷·크기 늘리기 Task는 VM이 배정될 노드(`spec.node` → `status.node` → 첫 디스크 볼륨의 노드 → API 노드)에서 돈다. 같은 VM의 스냅샷 Task와 그 VM이 쓰는 볼륨의 크기 늘리기는 Task 실행기의 대상별 줄(VM)로 차례를 지킨다.
- **VM 상태 경합.** VM status는 reconciler와 Task(현재 위치)가 함께 쓴다. 새 Raft 명령 `UpdateStatusIf`(revision이 같을 때만)로 쓰고, 어긋나면 다시 읽어 상대가 쓴 필드를 지킨다. 기존 `UpdateStatus`의 로그 형식은 바꾸지 않아, 옛 노드는 새 명령을 모르는 명령으로 거절한다.
- **스냅샷 Task 중 전원 조작.** 끝나지 않은 만들기·되돌리기가 있으면 VM 시작·정지(강제 포함)·재시작·수정은 409 `vm_busy`. 되돌리기는 디스크마다 다시 확인하고, 메모리 복원 전에 그 뒤 들어온 전원 Task가 있으면 켜지 않는다.
- **메모리 복원 시작.** 되돌리기 Task가 그 노드의 reconciler에 "다음 시작에 불러올 태그"를 맡기고(`restore_on_start`) 전원을 켠다. 드라이버는 `-S`로 띄운 QEMU에 `snapshot-load` 뒤 `cont`하고, 실패하면 QEMU를 끄고 VM은 `failed`(다시 시도하지 않는다). 맡긴 태그는 메모리에만 있다.
- **시간 초과와 정리.** QMP 작업 한도는 540초(Task 한도 600초보다 짧다). 실패가 확인되면 만든 태그를 지우고, 시간 초과처럼 진행 중일 수 있으면 태그를 두고 리소스만 지운다. 노드가 시작하면 이 노드가 맡은 `pending` 스냅샷(끝나지 않은 Task 없음)을 정리한다.
- **크기 늘리기.** 늘리기만, 512바이트 배수만(qcow2는 그렇지 않은 크기를 거부한다). 실행 중인 VM이 쓰면 `block_resize`, 아니면 `qemu-img resize`, 뒤에 실제 크기를 볼륨 스펙에 쓴다. 웹은 1 MiB 단위로 올린다.
- **삭제 규칙.** 스냅샷이 남은 VM은 지울 수 없다(409 `vm_has_snapshots`). CLI `--with-snapshots`와 웹 "스냅샷도 지우기"가 스냅샷을 하나씩 지운 뒤 VM을 지운다. 스냅샷이 가리키는 볼륨은 지울 수 없다(409 `volume_in_snapshot`).

## 파일 구조

```
crates/opl-types/src/{snapshot.rs(새로), resource.rs, task.rs, vm.rs}   # Snapshot, Kind, TaskOp, VmStatus.snapshot
crates/opl-cluster/src/{exclusive.rs, types.rs, state_machine.rs, cluster.rs}  # 이름 배타 값, UpdateStatusIf
crates/opl-qemu/src/qmp.rs                    # QMP 작업 도우미, 스냅샷·block_resize 명령
crates/opl-driver/src/{storage.rs, hypervisor.rs, qemu.rs, mock.rs, error.rs, lib.rs}
crates/opl-reconciler/src/{reconciler.rs, worker.rs}   # restore_on_start, 조건부 상태 쓰기
crates/opl-api/src/{snapshots.rs(새로), tasks.rs, tasks/snapshot.rs(새로), vms.rs, volumes.rs, watch.rs, error.rs, lib.rs}
crates/opld/src/member.rs                     # Task 실행기에 하이퍼바이저·state_dir
crates/opld/tests/{snapshots.rs(새로), web.rs}
crates/oplctl/src/cli.rs                      # vm snapshot, vm delete --with-snapshots, volume resize
web/src/vms/{VmSnapshots.tsx, snapshots.ts, VmDeleteDialog.tsx}(새로), web/src/storage/VolumesPage.tsx
web/e2e/vm.spec.ts, README.md, docs/superpowers/specs/2026-10-26-snapshots-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 스냅샷 리소스와 Task 종류

새 종류 `Kind::Snapshot`과 `opl-types/src/snapshot.rs`(`SnapshotSpec`: VM·이름·부모·메모리 여부·설명·예약 여부·VM 스펙, `SnapshotStatus`: `pending`/`ready`·만든 시각(unix 초)·메모리 상태 크기)를 더한다. VM에 현재 위치 `status.snapshot`, Task에 `snapshot-create`·`snapshot-revert`·`snapshot-delete`·`volume-resize`를 더한다(실행은 Task 7·8, 지금은 실패로 끝난다). 스냅샷 이름은 배타 값 `("name", "{vm}/{name}")`으로 VM 안에서만 고유하다. reconciler가 전원을 끌 때 status를 새로 만들며 `snapshot`을 지우지 않게 한다. OpenAPI에 이미 있는 `TaskOp`·`VmStatus`가 바뀌므로 웹 타입을 다시 만들고 fixture에 `snapshot: null`을 더한다.

**Files:**
- Create: `crates/opl-types/src/snapshot.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-cluster/src/exclusive.rs`
- Modify: `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/opl-types/src/vm.rs`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/test/fixtures.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        // 실행은 뒤에서 붙인다.
        TaskOp::SnapshotCreate { .. }
        | TaskOp::SnapshotRevert { .. }
        | TaskOp::SnapshotDelete { .. }
        | TaskOp::VolumeResize { .. } => Err("not implemented yet".into()),
    }
```

`crates/opl-cluster/src/exclusive.rs`에서

```rust
/// spec에서 다른 리소스와 겹치면 안 되는 값을 `(필드, 값)`으로 꺼낸다. 지금은 VM이 붙인
/// 볼륨 이름과 NIC의 MAC 주소다.
pub(crate) fn exclusive_values(kind: Kind, spec: &Value) -> Vec<(&'static str, String)> {
    if kind != Kind::Vm {
```

을 다음으로 바꾼다:

```rust
/// spec에서 다른 리소스와 겹치면 안 되는 값을 `(필드, 값)`으로 꺼낸다. 지금은 VM이 붙인
/// 볼륨 이름과 NIC의 MAC 주소, 스냅샷의 `{vm id}/{이름}`이다.
pub(crate) fn exclusive_values(kind: Kind, spec: &Value) -> Vec<(&'static str, String)> {
    if kind == Kind::Snapshot {
        return match (spec["vm"].as_str(), spec["name"].as_str()) {
            (Some(vm), Some(name)) => vec![("name", format!("{vm}/{name}"))],
            _ => Vec::new(),
        };
    }
    if kind != Kind::Vm {
```

`crates/opl-cluster/src/exclusive.rs`에서

```rust
    }
}
```

을 다음으로 바꾼다:

```rust
    }

    #[test]
    fn extracts_snapshot_name_within_its_vm() {
        let vm = "0195f7a0-0000-7000-8000-000000000001";
        let spec = json!({
            "vm": vm,
            "name": "before-upgrade",
            "vm_spec": {"disks": [{"volume": "root"}]}
        });
        // 같은 VM 안에서만 이름이 겹치면 안 된다. vm_spec의 볼륨은 배타 값이 아니다.
        assert_eq!(
            exclusive_values(Kind::Snapshot, &spec),
            vec![("name", format!("{vm}/before-upgrade"))]
        );
    }
}
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust
    ));
}
```

을 다음으로 바꾼다:

```rust
    ));
}

fn snapshot(vm_id: uuid::Uuid, name: &str) -> opl_types::snapshot::Snapshot {
    let spec = serde_json::json!({
        "vm": vm_id,
        "name": name,
        "memory": false,
        "vm_spec": vm("src", &["root-a"], &[]).spec,
    });
    let mut s: opl_types::snapshot::Snapshot =
        opl_types::resource::Resource::new("", serde_json::from_value(spec).unwrap());
    s.name = s.id.to_string();
    s
}

#[tokio::test]
async fn a_snapshot_name_is_unique_within_its_vm() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let (vm_a, vm_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let first = snapshot(vm_a, "before");
    c.create(&first).await.unwrap();

    let err = c.create(&snapshot(vm_a, "before")).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::InUse { kind: Kind::Snapshot, holder, .. }) if holder == first.id
    ));
    // 다른 VM은 같은 이름을 쓸 수 있고, 같은 VM도 다른 이름은 된다.
    c.create(&snapshot(vm_b, "before")).await.unwrap();
    c.create(&snapshot(vm_a, "after")).await.unwrap();
}
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            last_error: error,
        };
```

을 다음으로 바꾼다:

```rust
            last_error: error,
            snapshot: vm.status.snapshot,
        };
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
            last_error: None,
        }
```

을 다음으로 바꾼다:

```rust
            last_error: None,
            snapshot: None,
        }
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
#[tokio::test]
async fn crash_restarts_vm_with_on_failure_policy() {
```

을 다음으로 바꾼다:

```rust
#[tokio::test]
async fn power_off_keeps_current_snapshot() {
    let env = Env::start().await;
    let mut vm = vm("web", env.node);
    let snapshot = Uuid::now_v7();
    vm.status.snapshot = Some(snapshot);
    env.create(&vm).await;
    env.wait(vm.id, "running", running).await;

    env.hv.guest_shutdown(vm.id);

    // 상태를 새로 써도 스냅샷 위치는 reconciler가 지우지 않는다.
    let got = env
        .wait(vm.id, "powered off", |v| {
            v.spec.power == PowerState::Stopped && v.status.phase == VmPhase::Stopped
        })
        .await;
    assert_eq!(got.status.snapshot, Some(snapshot));
}

#[tokio::test]
async fn crash_restarts_vm_with_on_failure_policy() {
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod runtime;
pub mod storage;
```

을 다음으로 바꾼다:

```rust
pub mod runtime;
pub mod snapshot;
pub mod storage;
```

`crates/opl-types/src/resource.rs`에서

```rust
    Network,
}
```

을 다음으로 바꾼다:

```rust
    Network,
    Snapshot,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Network => "network",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::Network => "network",
            Kind::Snapshot => "snapshot",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Network,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::Network,
            Kind::Snapshot,
        ] {
```

`crates/opl-types/src/snapshot.rs`를 만든다:

```rust
//! VM 스냅샷. 디스크(qcow2 내부 스냅샷), 실행 중이었으면 메모리, VM 스펙을 함께 저장한다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::VmSpec;

/// 리소스 이름은 id 문자열이다(클러스터 전체에서 고유). 사용자 이름은 `spec.name`이고 VM 안에서 고유하다.
/// 디스크 안의 qcow2 태그도 id 문자열이다.
pub type Snapshot = Resource<SnapshotSpec, SnapshotStatus>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SnapshotSpec {
    /// VM id
    pub vm: Uuid,
    /// 사용자가 붙인 이름. VM 안에서 고유하다.
    pub name: String,
    /// 부모 스냅샷 id. VM의 첫 스냅샷이면 없다.
    #[serde(default)]
    pub parent: Option<Uuid>,
    /// 메모리 상태를 담았는지. 담았으면 VM의 첫 디스크에 있다.
    pub memory: bool,
    #[serde(default)]
    pub description: String,
    /// 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다.
    #[serde(default)]
    pub scheduled: bool,
    /// 만든 순간의 VM 스펙
    pub vm_spec: VmSpec,
}

impl ResourceSpec for SnapshotSpec {
    const KIND: Kind = Kind::Snapshot;
    type Status = SnapshotStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SnapshotStatus {
    pub phase: SnapshotPhase,
    /// 만든 시각(유닉스 초)
    pub created_at: Option<u64>,
    /// 메모리 상태 크기. 메모리 스냅샷만 있다.
    pub vmstate_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotPhase {
    /// 만드는 중
    #[default]
    Pending,
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm_spec() -> serde_json::Value {
        serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "virt",
            "disks": [ { "volume": "web-root", "bus": "virtio-blk" } ]
        })
    }

    #[test]
    fn spec_deserializes_with_defaults_and_round_trips() {
        let vm = Uuid::now_v7();
        let spec: SnapshotSpec = serde_json::from_value(serde_json::json!({
            "vm": vm,
            "name": "before-upgrade",
            "memory": true,
            "vm_spec": vm_spec()
        }))
        .unwrap();
        assert_eq!(spec.vm, vm);
        assert_eq!(spec.parent, None);
        assert_eq!(spec.description, "");
        assert!(!spec.scheduled);
        assert_eq!(spec.vm_spec.disks[0].volume, "web-root");

        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(serde_json::from_value::<SnapshotSpec>(json).unwrap(), spec);
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        let res = serde_json::from_value::<SnapshotSpec>(serde_json::json!({
            "vm": Uuid::now_v7(),
            "name": "a",
            "memory": false,
            "vm_spec": vm_spec(),
            "memroy": true
        }));
        assert!(res.is_err());
    }

    #[test]
    fn status_defaults_to_pending_and_serializes_phase() {
        let st: SnapshotStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(st, SnapshotStatus::default());
        assert_eq!(st.phase, SnapshotPhase::Pending);

        let ready = SnapshotStatus {
            phase: SnapshotPhase::Ready,
            created_at: Some(1_790_000_000),
            vmstate_bytes: Some(1 << 20),
        };
        assert_eq!(
            serde_json::to_value(&ready).unwrap(),
            serde_json::json!({
                "phase": "ready",
                "created_at": 1_790_000_000u64,
                "vmstate_bytes": 1u64 << 20
            })
        );
    }
}
```

`crates/opl-types/src/task.rs`에서

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id
    pub target: Uuid,
```

을 다음으로 바꾼다:

```rust
    pub op: TaskOp,
    /// 대상 리소스(VM 또는 Volume)의 id. 스냅샷 Task는 VM이다.
    pub target: Uuid,
```

`crates/opl-types/src/task.rs`에서

```rust
    VolumeDelete,
}
```

을 다음으로 바꾼다:

```rust
    VolumeDelete,
    SnapshotCreate {
        snapshot: Uuid,
    },
    /// 디스크·UEFI 변수·VM 스펙을 스냅샷 것으로 되돌린다.
    SnapshotRevert {
        snapshot: Uuid,
    },
    SnapshotDelete {
        snapshot: Uuid,
    },
    /// 볼륨 크기를 늘린다(줄이지 않는다).
    VolumeResize {
        size_bytes: u64,
    },
}
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn snapshot_and_resize_ops_are_tagged_by_kind() {
        let snapshot = Uuid::now_v7();
        for (op, kind) in [
            (TaskOp::SnapshotCreate { snapshot }, "snapshot-create"),
            (TaskOp::SnapshotRevert { snapshot }, "snapshot-revert"),
            (TaskOp::SnapshotDelete { snapshot }, "snapshot-delete"),
        ] {
            let json = serde_json::to_value(&op).unwrap();
            assert_eq!(
                json,
                serde_json::json!({ "kind": kind, "snapshot": snapshot })
            );
            assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
        }
        let op = TaskOp::VolumeResize {
            size_bytes: 20 << 30,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "volume-resize", "size_bytes": 20u64 << 30 })
        );
        assert_eq!(serde_json::from_value::<TaskOp>(json).unwrap(), op);
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/opl-types/src/vm.rs`에서

```rust
    pub last_error: Option<String>,
}
```

을 다음으로 바꾼다:

```rust
    pub last_error: Option<String>,
    /// 현재 위치: 마지막으로 만들었거나 되돌린 스냅샷. 다음 스냅샷의 부모가 된다.
    pub snapshot: Option<Uuid>,
}
```

`crates/opl-types/src/vm.rs`에서

```rust
        assert_eq!(st, VmStatus::default());
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(st, VmStatus::default());
        assert_eq!(st.snapshot, None);
    }
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  const broken = vm("db", "failed", {
    status: { phase: "failed", node: null, pid: null, last_error: "no firmware" },
  });
```

을 다음으로 바꾼다:

```tsx
  const broken = vm("db", "failed", {
    status: { phase: "failed", node: null, pid: null, last_error: "no firmware", snapshot: null },
  });
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  const failed = vm("db", "failed", {
    status: { phase: "failed", node: "n1", pid: null, last_error: "boom" },
  });
  const stopping = vm("app", "stopping", {
    status: { phase: "stopping", node: "n1", pid: 7, last_error: null },
  });
  const elsewhere = vm("far", "running", {
    status: { phase: "running", node: "n2", pid: 8, last_error: null },
  });
```

을 다음으로 바꾼다:

```tsx
  const failed = vm("db", "failed", {
    status: { phase: "failed", node: "n1", pid: null, last_error: "boom", snapshot: null },
  });
  const stopping = vm("app", "stopping", {
    status: { phase: "stopping", node: "n1", pid: 7, last_error: null, snapshot: null },
  });
  const elsewhere = vm("far", "running", {
    status: { phase: "running", node: "n2", pid: 8, last_error: null, snapshot: null },
  });
```

`web/src/test/fixtures.ts`에서

```ts
    },
    status: { phase, node: phase === "running" ? "n1" : null, pid: null, last_error: null },
    ...extra,
```

을 다음으로 바꾼다:

```ts
    },
    status: {
      phase,
      node: phase === "running" ? "n1" : null,
      pid: null,
      last_error: null,
      snapshot: null,
    },
    ...extra,
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 243개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 177개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(types): 스냅샷 리소스와 Task 종류"
```

---

### Task 2: QMP 작업 도우미

QMP 작업을 시작하고 `JOB_STATUS_CHANGE`(놓치면 `query-jobs`)로 `concluded`를 기다린 뒤 오류를 읽고 늘 `job-dismiss`하는 도우미와, 그것으로 만든 `snapshot_save`·`snapshot_load`·`snapshot_delete`(태그, 메모리 디스크, 디스크 목록, 제한 시간), 디스크만 한 번에 스냅샷하는 `snapshot_disks`(`transaction` + `blockdev-snapshot-internal-sync`), `block_resize`를 더한다. 작업 id는 `opl-{uuid}`, 실패는 `QmpError::JobFailed`, 시간 초과는 `JobTimeout`(작업은 QEMU에 남는다). 가짜 QMP 테스트와, OS 없는 실제 QEMU에서 저장·불러오기·삭제·크기 늘리기를 보는 `qemu-it` 테스트를 둔다.

**Files:**
- Modify: `crates/opl-qemu/src/qmp.rs`
- Modify: `crates/opl-qemu/tests/qemu_boot.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-qemu/src/qmp.rs`에서

```rust
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
```

을 다음으로 바꾼다:

```rust
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
    Disconnected,
}
```

을 다음으로 바꾼다:

```rust
    Disconnected,
    #[error("QMP job {id} failed: {error}")]
    JobFailed { id: String, error: String },
    #[error("QMP job {id} did not finish within {timeout:?}")]
    JobTimeout { id: String, timeout: Duration },
}
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
            Err(e) => Err(e),
        }
    }
}

impl Drop for QmpClient {
```

을 다음으로 바꾼다:

```rust
            Err(e) => Err(e),
        }
    }

    /// 내부 스냅샷 `tag`를 `devices`(블록 node-name)에 만들고 메모리 상태를 `vmstate`에 담는다.
    /// 저장하는 동안 VM이 멈췄다가 원래 상태로 돌아온다.
    pub async fn snapshot_save(
        &self,
        tag: &str,
        vmstate: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "vmstate": vmstate, "devices": devices });
        self.run_job("snapshot-save", args, timeout).await
    }

    /// 내부 스냅샷 `tag`로 디스크와 메모리 상태를 되돌린다. `-S`로 띄운 뒤 `cont` 전에 부를 수 있다.
    pub async fn snapshot_load(
        &self,
        tag: &str,
        vmstate: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "vmstate": vmstate, "devices": devices });
        self.run_job("snapshot-load", args, timeout).await
    }

    /// `devices`에서 내부 스냅샷 `tag`를 지운다.
    pub async fn snapshot_delete(
        &self,
        tag: &str,
        devices: &[String],
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let args = json!({ "tag": tag, "devices": devices });
        self.run_job("snapshot-delete", args, timeout).await
    }

    /// 메모리 없이 `devices` 모두에 내부 스냅샷 `tag`를 한 번에 만든다. VM은 멈추지 않는다.
    pub async fn snapshot_disks(&self, tag: &str, devices: &[String]) -> Result<(), QmpError> {
        let actions: Vec<Value> = devices
            .iter()
            .map(|d| {
                json!({
                    "type": "blockdev-snapshot-internal-sync",
                    "data": { "device": d, "name": tag },
                })
            })
            .collect();
        self.execute("transaction", Some(json!({ "actions": actions })))
            .await
            .map(drop)
    }

    /// 블록 노드 `node`의 크기를 `size` 바이트로 바꾼다. 게스트가 바로 본다.
    pub async fn block_resize(&self, node: &str, size: u64) -> Result<(), QmpError> {
        let args = json!({ "node-name": node, "size": size });
        self.execute("block_resize", Some(args)).await.map(drop)
    }

    /// 작업(job) 명령을 새 `job-id`로 시작하고 `concluded`까지 기다린 뒤, 오류를 읽고 `job-dismiss`한다.
    ///
    /// 제한 시간이 지나면 작업은 QEMU에 남겨 두고 `JobTimeout`을 돌려준다.
    async fn run_job(
        &self,
        command: &str,
        mut arguments: Value,
        timeout: Duration,
    ) -> Result<(), QmpError> {
        let id = format!("opl-{}", Uuid::new_v4());
        arguments["job-id"] = json!(id);
        // 작업 이벤트를 놓치지 않게 시작하기 전에 구독한다.
        let mut events = self.subscribe();
        self.execute(command, Some(arguments)).await?;

        let wait = async {
            loop {
                match events.recv().await {
                    Ok(ev) => {
                        if ev.event == "JOB_STATUS_CHANGE"
                            && ev.data["id"] == id.as_str()
                            && ev.data["status"] == "concluded"
                        {
                            return Ok(());
                        }
                    }
                    // 이벤트를 놓쳤으면 query-jobs로 상태를 직접 본다.
                    Err(RecvError::Lagged(_)) => {
                        if self.query_job(&id).await?["status"] == "concluded" {
                            return Ok(());
                        }
                    }
                    Err(RecvError::Closed) => return Err(QmpError::Disconnected),
                }
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| QmpError::JobTimeout {
                id: id.clone(),
                timeout,
            })??;

        let error = self.query_job(&id).await?["error"]
            .as_str()
            .map(str::to_string);
        self.execute("job-dismiss", Some(json!({ "id": id })))
            .await?;
        match error {
            Some(error) => Err(QmpError::JobFailed { id, error }),
            None => Ok(()),
        }
    }

    /// `query-jobs`에서 작업 `id`를 찾는다.
    async fn query_job(&self, id: &str) -> Result<Value, QmpError> {
        let jobs = self.execute("query-jobs", None).await?;
        jobs.as_array()
            .and_then(|jobs| jobs.iter().find(|j| j["id"] == id))
            .cloned()
            .ok_or_else(|| QmpError::Protocol(format!("job {id} not in query-jobs: {jobs}")))
    }
}

impl Drop for QmpClient {
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
    use tokio::net::UnixListener;
    use tokio::sync::broadcast::error::RecvError;
```

을 다음으로 바꾼다:

```rust
    use tokio::net::UnixListener;
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
            self.send(reply).await;
        }
    }
```

을 다음으로 바꾼다:

```rust
            self.send(reply).await;
        }
    }

    /// 작업을 시작하는 명령 하나를 받아 빈 응답을 주고, 인자(`job-id` 포함)를 돌려준다.
    async fn accept_job(fake: &mut FakeQemu, expect: &str) -> Value {
        let cmd = fake.recv().await;
        assert_eq!(cmd["execute"], expect);
        fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        cmd["arguments"].clone()
    }

    async fn job_status(fake: &mut FakeQemu, id: &Value, status: &str) {
        fake.send(json!({
            "event": "JOB_STATUS_CHANGE",
            "data": { "id": id, "status": status },
            "timestamp": {}
        }))
        .await;
    }

    /// 실제 QEMU처럼 created부터 concluded까지 이벤트를 보내고, `query-jobs`(실패면 `error`)와
    /// `job-dismiss`에 답한다.
    async fn run_job(fake: &mut FakeQemu, id: &Value, error: Option<&str>) {
        for status in ["created", "running", "waiting", "pending", "concluded"] {
            job_status(fake, id, status).await;
        }
        answer_jobs(fake, id, error).await;
    }

    async fn answer_jobs(fake: &mut FakeQemu, id: &Value, error: Option<&str>) {
        let mut job = json!({
            "id": id, "type": "snapshot-save", "status": "concluded",
            "current-progress": 1, "total-progress": 1
        });
        if let Some(e) = error {
            job["error"] = json!(e);
        }
        fake.answer("query-jobs", json!({ "return": [job] })).await;
        let cmd = fake.recv().await;
        assert_eq!(cmd["execute"], "job-dismiss");
        assert_eq!(&cmd["arguments"]["id"], id);
        fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
    }
```

`crates/opl-qemu/src/qmp.rs`에서

```rust
        assert_eq!(rx.recv().await.unwrap().event, "RESUME");
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(rx.recv().await.unwrap().event, "RESUME");
    }

    const T: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn snapshot_save_waits_for_job_and_dismisses_it() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string(), "disk1".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-save").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["vmstate"], "disk0");
            assert_eq!(args["devices"], json!(["disk0", "disk1"]));
            let id = args["job-id"].clone();
            assert!(id.as_str().is_some_and(|s| !s.is_empty()));
            // 다른 작업의 이벤트는 무시해야 한다.
            job_status(&mut fake, &json!("other"), "concluded").await;
            run_job(&mut fake, &id, None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn snapshot_load_and_delete_send_their_arguments() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_load("s1", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-load").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["vmstate"], "disk0");
            assert_eq!(args["devices"], json!(["disk0"]));
            run_job(&mut fake, &args["job-id"], None).await;
        });
        res.unwrap();

        let (res, ()) = tokio::join!(client.snapshot_delete("s1", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-delete").await;
            assert_eq!(args["tag"], "s1");
            assert_eq!(args["devices"], json!(["disk0"]));
            assert!(args.get("vmstate").is_none());
            run_job(&mut fake, &args["job-id"], None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn failed_job_reports_error_and_is_still_dismissed() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_load("nope", "disk0", &devices, T), async {
            let args = accept_job(&mut fake, "snapshot-load").await;
            run_job(
                &mut fake,
                &args["job-id"],
                Some("Snapshot 'nope' does not exist"),
            )
            .await;
        });
        match res.unwrap_err() {
            QmpError::JobFailed { error, .. } => {
                assert_eq!(error, "Snapshot 'nope' does not exist");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn job_start_error_is_returned_without_waiting() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.snapshot_delete("s1", &[], T), async {
            fake.answer(
                "snapshot-delete",
                json!({ "error": { "class": "GenericError", "desc": "bad" } }),
            )
            .await;
        });
        assert!(matches!(res, Err(QmpError::Command { .. })));
    }

    #[tokio::test]
    async fn job_times_out_when_it_never_concludes() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, _fake) = tokio::join!(
            client.snapshot_save("s1", "disk0", &devices, Duration::from_millis(200)),
            async {
                let args = accept_job(&mut fake, "snapshot-save").await;
                job_status(&mut fake, &args["job-id"], "running").await;
                fake
            }
        );
        assert!(matches!(res, Err(QmpError::JobTimeout { .. })));
    }

    #[tokio::test]
    async fn job_wait_fails_when_connection_closes() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            accept_job(&mut fake, "snapshot-save").await;
            drop(fake);
        });
        assert!(matches!(res, Err(QmpError::Disconnected)));
    }

    #[tokio::test]
    async fn lagged_job_wait_falls_back_to_query_jobs() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_save("s1", "disk0", &devices, T), async {
            let cmd = fake.recv().await;
            let id = cmd["arguments"]["job-id"].clone();
            // 응답보다 먼저 이벤트를 넘치게 보내 구독자가 `Lagged`를 받게 한다.
            // concluded 이벤트도 이 사이에 묻힌다.
            for _ in 0..100 {
                job_status(&mut fake, &json!("other"), "running").await;
            }
            job_status(&mut fake, &id, "concluded").await;
            for _ in 0..100 {
                job_status(&mut fake, &json!("other"), "running").await;
            }
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
            // 놓친 것을 알고 상태를 묻는다. 그다음은 보통처럼 오류 확인과 dismiss.
            fake.answer(
                "query-jobs",
                json!({ "return": [{ "id": id, "type": "snapshot-save", "status": "concluded" }] }),
            )
            .await;
            answer_jobs(&mut fake, &id, None).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn snapshot_disks_uses_one_transaction() {
        let (client, mut fake, _dir) = setup().await;
        let devices = ["disk0".to_string(), "disk1".to_string()];
        let (res, ()) = tokio::join!(client.snapshot_disks("s1", &devices), async {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], "transaction");
            assert_eq!(
                cmd["arguments"]["actions"],
                json!([
                    { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk0", "name": "s1" } },
                    { "type": "blockdev-snapshot-internal-sync", "data": { "device": "disk1", "name": "s1" } }
                ])
            );
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        });
        res.unwrap();
    }

    #[tokio::test]
    async fn block_resize_sends_node_and_size() {
        let (client, mut fake, _dir) = setup().await;
        let (res, ()) = tokio::join!(client.block_resize("disk1", 2 << 30), async {
            let cmd = fake.recv().await;
            assert_eq!(cmd["execute"], "block_resize");
            assert_eq!(
                cmd["arguments"],
                json!({ "node-name": "disk1", "size": 2u64 << 30 })
            );
            fake.send(json!({ "return": {}, "id": cmd["id"] })).await;
        });
        res.unwrap();
    }
}
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust

#[tokio::test]
async fn boots_paused_resumes_reconnects_and_quits() {
    let host = host();
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
```

을 다음으로 바꾼다:

```rust

/// qcow2 디스크 하나(OS 없음)로 QEMU를 `-S`로 띄우고 QMP 소켓이 생길 때까지 기다린다.
async fn launch(host: &HostProfile) -> (tempfile::TempDir, tokio::process::Child, RuntimePaths) {
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust

    let args = build_cmdline(&spec(id, disk), &host, &paths).unwrap();
    let mut child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
```

을 다음으로 바꾼다:

```rust

    let args = build_cmdline(&spec(id, disk), host, &paths).unwrap();
    let child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
    wait_for(&paths.qmp_socket).await;
```

을 다음으로 바꾼다:

```rust
    wait_for(&paths.qmp_socket).await;
    (tmp, child, paths)
}

#[tokio::test]
async fn boots_paused_resumes_reconnects_and_quits() {
    let host = host();
    let (_tmp, mut child, paths) = launch(&host).await;
```

`crates/opl-qemu/tests/qemu_boot.rs`에서

```rust
    assert!(exit.success(), "qemu exited with {exit}");
}
```

을 다음으로 바꾼다:

```rust
    assert!(exit.success(), "qemu exited with {exit}");
}

/// QMP의 `disk0` 노드에 있는 내부 스냅샷 이름들과 가상 크기
async fn disk0(qmp: &QmpClient) -> (Vec<String>, u64) {
    let nodes = qmp.execute("query-named-block-nodes", None).await.unwrap();
    let node = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node-name"] == "disk0")
        .unwrap();
    let tags = node["image"]["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    (tags, node["image"]["virtual-size"].as_u64().unwrap())
}

#[tokio::test]
async fn snapshot_jobs_and_block_resize() {
    let host = host();
    let (_tmp, mut child, paths) = launch(&host).await;
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    qmp.cont().await.unwrap();
    let devices = ["disk0".to_string()];
    let t = Duration::from_secs(60);

    // 메모리 스냅샷: 저장하는 동안 멈췄다가 다시 실행된다.
    qmp.snapshot_save("mem", "disk0", &devices, t)
        .await
        .unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);
    // 디스크만 스냅샷
    qmp.snapshot_disks("disk", &devices).await.unwrap();
    assert_eq!(disk0(&qmp).await.0, ["mem", "disk"]);

    qmp.snapshot_load("mem", "disk0", &devices, t)
        .await
        .unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);
    // 없는 태그를 불러오면 작업이 오류로 끝난다.
    let err = qmp
        .snapshot_load("nope", "disk0", &devices, t)
        .await
        .unwrap_err();
    assert!(
        matches!(err, opl_qemu::QmpError::JobFailed { .. }),
        "{err:?}"
    );

    qmp.snapshot_delete("disk", &devices, t).await.unwrap();
    // 없는 태그를 지워도 성공한다.
    qmp.snapshot_delete("disk", &devices, t).await.unwrap();
    assert_eq!(disk0(&qmp).await.0, ["mem"]);

    qmp.block_resize("disk0", 2 << 30).await.unwrap();
    assert_eq!(disk0(&qmp).await.1, 2 << 30);

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 252개, 실패 0

Run: `cargo test -p opl-qemu --features qemu-it --test qemu_boot`
Expected: PASS 2개(`snapshot_jobs_and_block_resize` 포함)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(qemu): QMP 작업 도우미와 스냅샷·크기 늘리기 명령"
```

---

### Task 3: 스토리지 드라이버: 정지한 볼륨의 스냅샷과 크기 늘리기

`Storage`에 `supports_snapshots`(dir은 qcow2만), `snapshot_create`·`snapshot_revert`·`snapshot_delete`(태그가 없으면 성공, 같은 태그가 여럿이면 모두)·`snapshot_tags`·`resize_volume`을 더하고 `DirStorage`가 `qemu-img`로 구현한다. qemu-img는 같은 이름의 태그를 두 번 만들어 주므로 만들기가 막는다. 태그 목록은 `qemu-img info -U --output=json`(실행 중에도 읽는다). 실제 `qemu-img`·`qemu-io` 테스트로 패턴을 쓰고 되돌려 원래 내용이 돌아오는지 본다.

**Files:**
- Modify: `crates/opl-driver/src/storage.rs`
- Modify: `crates/opl-driver/tests/dir_storage.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/storage.rs`에서

```rust
    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError>;
}
```

을 다음으로 바꾼다:

```rust
    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError>;

    /// 이 백엔드가 `format` 볼륨의 스냅샷을 지원하는지. 지원하지 않는 형식에 스냅샷 메서드를
    /// 부르면 `InvalidSpec`.
    fn supports_snapshots(&self, format: ImageFormat) -> bool;

    /// 정지한 볼륨에 스냅샷 `tag`를 만든다. 같은 태그가 이미 있으면 `InvalidSpec`.
    async fn snapshot_create(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 정지한 볼륨의 내용을 스냅샷 `tag` 때로 되돌린다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 정지한 볼륨에서 스냅샷 `tag`를 지운다. 태그가 없으면 성공(다시 시도해도 된다).
    async fn snapshot_delete(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 볼륨에 있는 스냅샷 태그들. VM이 실행 중이어도 조회할 수 있다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError>;

    /// 정지한 볼륨의 크기를 `size_bytes`로 늘린다. 줄이지는 않는다(실패한다).
    /// 실제 크기는 `volume_info`로 확인한다.
    async fn resize_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError>;
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
        Ok(out.stdout)
    }
```

을 다음으로 바꾼다:

```rust
        Ok(out.stdout)
    }

    /// 있는 볼륨의 경로. 없으면 `VolumeNotFound`.
    fn existing_path(
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        pool_dir(pool)?;
        let path = Self::volume_path(pool, id, format);
        if !path.exists() {
            return Err(DriverError::VolumeNotFound(id));
        }
        Ok(path)
    }

    /// 스냅샷을 다룰 볼륨의 경로. 형식이 스냅샷을 지원해야 한다.
    fn snapshot_path(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        if !self.supports_snapshots(format) {
            return Err(DriverError::InvalidSpec(format!(
                "{} volumes do not support snapshots",
                fmt_name(format)
            )));
        }
        Self::existing_path(pool, id, format)
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
```

`crates/opl-driver/src/storage.rs`에서

```rust
    ) -> Result<VolumeInfo, DriverError> {
        pool_dir(pool)?;
        let path = Self::volume_path(pool, id, format);
        if !path.exists() {
            return Err(DriverError::VolumeNotFound(id));
        }
        // -U: 실행 중인 VM이 잡고 있는 이미지 잠금을 무시하고 읽기만 한다.
        let out = self
            .qemu_img(&[
                "info",
                "-U",
                "--output=json",
                "-f",
                fmt_name(format),
                path_str(&path)?,
            ])
            .await?;
        let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })?;
        let field = |k: &str| {
```

을 다음으로 바꾼다:

```rust
    ) -> Result<VolumeInfo, DriverError> {
        let path = Self::existing_path(pool, id, format)?;
        let json = self.info_json(&path, format).await?;
        let field = |k: &str| {
```

`crates/opl-driver/src/storage.rs`에서

```rust
            available_bytes: fs4::available_space(path)?,
        })
    }
}
```

을 다음으로 바꾼다:

```rust
            available_bytes: fs4::available_space(path)?,
        })
    }

    /// 내부 스냅샷은 qcow2만 담을 수 있다.
    fn supports_snapshots(&self, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePoolSpec,
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
        let path = path_str(&path)?;
        self.qemu_img(&["snapshot", "-q", "-f", fmt_name(format), "-c", tag, path])
            .await?;
        Ok(())
    }

    async fn snapshot_revert(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        let path = path_str(&path)?;
        self.qemu_img(&["snapshot", "-q", "-f", fmt_name(format), "-a", tag, path])
            .await?;
        Ok(())
    }

    async fn snapshot_delete(
        &self,
        pool: &StoragePoolSpec,
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
        let path = path_str(&path)?;
        for _ in 0..count {
            self.qemu_img(&["snapshot", "-q", "-f", fmt_name(format), "-d", tag, path])
                .await?;
        }
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        self.tags_at(&path, format).await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let path = Self::existing_path(pool, id, format)?;
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
}
```

`crates/opl-driver/src/storage.rs`에서

```rust
            Err(DriverError::InvalidSpec(_))
        ));

        let abs = pool(tmp.path().to_str().unwrap());
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opl-driver/src/storage.rs`에서

```rust
    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

을 다음으로 바꾼다:

```rust
    #[tokio::test]
    async fn snapshots_need_qcow2_and_existing_volume() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);
        assert!(storage.supports_snapshots(qcow2));
        assert!(!storage.supports_snapshots(raw));

        std::fs::write(DirStorage::volume_path(&p, id, raw), b"").unwrap();
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
        std::fs::write(DirStorage::volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // info만 답하고 다른 명령(snapshot -d)은 실패한다.
        let storage = DirStorage::new(fake_tool(
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
        std::fs::write(DirStorage::volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // 스냅샷이 없으면 qemu-img info는 snapshots 항목을 내지 않는다.
        let storage = DirStorage::new(fake_tool(tools.path(), "echo '{\"virtual-size\": 5}'"));
        assert!(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
```

`crates/opl-driver/tests/dir_storage.rs`에서

```rust
    assert!(!DirStorage::volume_path(&pool, id, ImageFormat::Qcow2).exists());
}
```

을 다음으로 바꾼다:

```rust
    assert!(!DirStorage::volume_path(&pool, id, ImageFormat::Qcow2).exists());
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, cmd: &str) -> bool {
    std::process::Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn snapshot_revert_restores_contents_and_delete_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img");
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
    let storage = DirStorage::new("qemu-img");

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
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 255개, 실패 0

Run: `cargo test -p opl-driver --features storage-it --test dir_storage`
Expected: PASS 6개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): 스토리지 스냅샷과 크기 늘리기"
```

---

### Task 4: 하이퍼바이저 드라이버: 실행 중 VM과 스냅샷을 불러오며 시작

`Hypervisor::start`가 `restore: Option<&str>`를 받는다. 태그가 있으면 `-S`로 띄운 QEMU에 `cont` 전 `snapshot-load`를 하고, 실패하면 QEMU를 끄고 오류를 돌려준다(태그가 없으면 예전 그대로). 실행 중 VM용 `snapshot_save(id, tag, memory)`·`snapshot_delete`·`block_resize(id, disk, size)`를 더한다. 디스크 노드는 QEMU에서 읽고(`query-named-block-nodes`의 `disk{i}`), 작업 한도는 540초다. `JobFailed`·`JobTimeout`은 영구 오류, 살아 있지만 아직 다시 붙지 못한 VM은 새 일시 오류 `NotAttached`. mock은 시작마다 받은 태그·스냅샷·디스크 크기를 기록한다.

**Files:**
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/hypervisor.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-driver/tests/qemu_bridge.rs`
- Modify: `crates/opl-driver/tests/qemu_driver.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-driver/src/error.rs`에서

```rust
    Timeout(&'static str),
    #[error(transparent)]
```

을 다음으로 바꾼다:

```rust
    Timeout(&'static str),
    /// 살아 있지만 아직 다시 붙지 못해 QMP로 다룰 수 없는 VM. 요청을 보내지 않았으므로
    /// 하이퍼바이저 안에서 돌고 있는 작업은 없다.
    #[error("VM {0} is running but not attached yet")]
    NotAttached(Uuid),
    #[error(transparent)]
```

`crates/opl-driver/src/error.rs`에서

```rust
            DriverError::Qmp(QmpError::Command { .. }) => false,
            // 바이너리나 풀 디렉터리가 없거나, 권한이 없거나, 파일시스템이 기능(hard link 등)을
```

을 다음으로 바꾼다:

```rust
            DriverError::Qmp(QmpError::Command { .. }) => false,
            // 작업이 오류로 끝났으면 다시 해도 같다. 시간 초과면 작업이 QEMU에 남아 있을 수 있다.
            DriverError::Qmp(QmpError::JobFailed { .. } | QmpError::JobTimeout { .. }) => false,
            // 바이너리나 풀 디렉터리가 없거나, 권한이 없거나, 파일시스템이 기능(hard link 등)을
```

`crates/opl-driver/src/error.rs`에서

```rust
            ),
            DriverError::Qmp(_) | DriverError::Timeout(_) => true,
            DriverError::NotRunning(_)
```

을 다음으로 바꾼다:

```rust
            ),
            DriverError::Qmp(_) | DriverError::Timeout(_) | DriverError::NotAttached(_) => true,
            DriverError::NotRunning(_)
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(DriverError::Timeout("qmp").is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

을 다음으로 바꾼다:

```rust
        assert!(DriverError::Timeout("qmp").is_transient());
        assert!(DriverError::NotAttached(Uuid::nil()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(!DriverError::Qmp(cmd).is_transient());
        assert!(!DriverError::StartFailed("x".into()).is_transient());
```

을 다음으로 바꾼다:

```rust
        assert!(!DriverError::Qmp(cmd).is_transient());
        // 스냅샷 작업이 오류로 끝났거나(없는 태그 등) 제한 시간을 넘겼다. 시간 초과면 작업이
        // QEMU에 남아 있을 수 있으므로 같은 요청을 곧바로 다시 보내지 않는다.
        let failed = QmpError::JobFailed {
            id: "j".into(),
            error: "x".into(),
        };
        assert!(!DriverError::Qmp(failed).is_transient());
        let timeout = QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(1),
        };
        assert!(!DriverError::Qmp(timeout).is_transient());
        assert!(!DriverError::StartFailed("x".into()).is_transient());
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    async fn start(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError>;
```

을 다음으로 바꾼다:

```rust
    /// VM을 시작한다. 이미 실행 중이면 아무것도 하지 않고 성공한다.
    ///
    /// `restore`가 있으면 새로 띄운 VM을 실행하기 전에 그 태그의 내부 스냅샷(디스크와 메모리,
    /// 메모리는 첫 디스크)을 불러온다. 불러오지 못하면 VM을 끝내고 오류를 돌려준다.
    /// 이미 실행 중인 VM에는 쓰지 않는다.
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError>;
```

`crates/opl-driver/src/hypervisor.rs`에서

```rust

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

을 다음으로 바꾼다:

```rust

    /// 실행 중인 VM의 모든 디스크에 내부 스냅샷 `tag`를 만든다. `memory`면 메모리 상태도
    /// 첫 디스크에 담는다(저장하는 동안 VM이 멈춘다). 아니면 VM을 멈추지 않고 디스크만 찍는다.
    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError>;

    /// 실행 중인 VM의 모든 디스크에서 내부 스냅샷 `tag`를 지운다. 태그가 없어도 성공한다.
    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError>;

    /// 실행 중인 VM의 `disk`번째 디스크(스펙의 `disks` 순서) 크기를 바꾼다. 게스트가 바로 본다.
    async fn block_resize(&self, id: Uuid, disk: usize, size_bytes: u64)
    -> Result<(), DriverError>;

    /// 이 시점 이후의 VM 수명 이벤트를 받는다. `Lagged`를 받으면 `list()`로 전체 상태를 다시 맞춘다.
```

`crates/opl-driver/src/mock.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        Ok(())
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }
}
```

`crates/opl-driver/src/qemu.rs` 전체:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use opl_qemu::{HostProfile, QmpClient, QmpError, QmpEvent, RunState, RuntimePaths, build_cmdline};
use opl_types::network::tap_prefix;
use opl_types::runtime::{NicBackend, VmRuntimeSpec};
use opl_types::vm::Firmware;
use rustix::process::{Flock, FlockOffsetType, FlockType, Pid, Signal, fcntl_getlk, kill_process};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};
use crate::net::{HostNet, Tap};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
/// QMP 명령 하나의 응답을 기다리는 한도. 멈춘(SIGSTOP 등) QEMU에서 호출자가 묶이지 않게 한다.
const QMP_TIMEOUT: Duration = Duration::from_secs(5);
const FORCE_STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// SIGKILL을 보낸 뒤 프로세스가 사라지기를 기다리는 한도
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// SHUTDOWN 없이 QMP 연결이 끊긴 뒤 프로세스가 끝나기를 기다리는 한도.
/// 넘기면 QMP만 끊긴 것으로 본다.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// SHUTDOWN 뒤 연결이 끊겼을 때 런타임 디렉터리를 지우려고 프로세스 종료를 잠깐 기다리는 한도.
/// QEMU는 소켓을 닫고 곧 끝나지만, 플러시가 길면 디렉터리는 다음 launch/new()에 맡기고 종료를 바로 알린다.
const SHUTDOWN_EXIT_WAIT: Duration = Duration::from_secs(1);
/// 내부 스냅샷(저장·불러오기·삭제) 하나를 기다리는 한도. 메모리 스냅샷은 메모리 크기만큼 걸린다.
/// Task 한도(600초)보다 짧아야 Task가 먼저 끊기지 않고 이 오류(작업이 남아 있을 수 있음)를 받는다.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub host: HostProfile,
    /// 소켓과 pidfile을 두는 휘발성 디렉터리 (Linux: `/run/opl`)
    pub run_dir: PathBuf,
    /// UEFI 변수처럼 재부팅 후에도 남아야 하는 VM별 파일 (Linux: `/var/lib/opl`)
    pub state_dir: PathBuf,
    /// 브리지 NIC의 tap을 만들고 지운다. 없으면(macOS) 브리지 NIC를 가진 VM은 시작하지 않는다.
    pub net: Option<Arc<dyn HostNet>>,
    /// QEMU를 `systemd-run --scope`로 감싸 VM마다 cgroup(`opl-vm-<id>.scope`)으로 분리한다.
    /// systemd가 있는 Linux에서만 켠다.
    pub systemd_scope: bool,
}

/// QEMU 프로세스를 `-daemonize`로 띄워 opld와 분리한다. opld가 재시작해도 VM은 계속 돈다.
///
/// QEMU의 생존 여부는 QMP 응답이 아니라 pidfile의 fcntl 잠금으로 판정한다([`liveness`]).
/// 잠금이 걸렸거나 판정할 수 없는 런타임 디렉터리는 절대 지우지 않고, 잠금 소유자로 확인한
/// pid만 죽인다.
pub struct QemuHypervisor {
    config: QemuConfig,
    vms: Arc<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
}

struct VmHandle {
    qmp: QmpClient,
    pid: u32,
    paths: RuntimePaths,
    monitor: JoinHandle<()>,
}

impl Drop for VmHandle {
    fn drop(&mut self) {
        // 드라이버가 내려갈 때 모니터가 "연결 끊김 = 크래시"로 오판해 런타임 파일을 지우지 않게 한다.
        self.monitor.abort();
    }
}

impl QemuHypervisor {
    /// 런타임 디렉터리를 훑어 살아 있는 VM에 다시 연결하고, 죽은 VM의 흔적은 지운다.
    pub async fn new(config: QemuConfig) -> Result<Self, DriverError> {
        let vm_run_dir = config.run_dir.join("vm");
        std::fs::create_dir_all(&vm_run_dir)?;
        std::fs::create_dir_all(config.state_dir.join("vm"))?;
        let hv = Self {
            config,
            vms: Arc::new(Mutex::new(HashMap::new())),
            events: broadcast::channel(64).0,
        };
        for entry in std::fs::read_dir(&vm_run_dir)? {
            let entry = entry?;
            let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
            else {
                continue;
            };
            let paths = hv.paths(id)?;
            match liveness(&paths.pidfile) {
                Liveness::Alive(_) => {}
                Liveness::Dead => {
                    tracing::warn!(%id, "cleaning up stale runtime dir");
                    // 런타임 디렉터리가 정리할 것이 남았다는 유일한 기록이므로 tap부터 지운다.
                    // 지우지 못하면 디렉터리를 남겨 다음 new()가 다시 시도한다.
                    if delete_taps(hv.config.net.as_deref(), id).await
                        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
                    {
                        tracing::error!(%id, error = %e, "failed to remove stale runtime dir");
                    }
                    continue;
                }
                Liveness::Unknown => {
                    tracing::error!(%id, "cannot tell whether QEMU is running; leaving runtime dir");
                    continue;
                }
            }
            // 살아 있는 VM이므로 붙지 못해도 아무것도 지우지 않는다. 이후 start()가 다시 시도한다.
            match tokio::time::timeout(ATTACH_TIMEOUT, hv.attach(id, paths, false)).await {
                Ok(Ok(())) => tracing::info!(%id, "reattached to running VM"),
                Ok(Err(e)) => tracing::error!(%id, error = %e, "could not reattach to running VM"),
                Err(_) => tracing::error!(%id, "timed out reattaching to running VM"),
            }
        }
        Ok(hv)
    }

    fn paths(&self, id: Uuid) -> Result<RuntimePaths, DriverError> {
        let mut paths = RuntimePaths::new(&self.config.run_dir, id)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        paths.uefi_vars = self.state_dir(id).join("uefi-vars.fd");
        Ok(paths)
    }

    fn state_dir(&self, id: Uuid) -> PathBuf {
        self.config.state_dir.join("vm").join(id.to_string())
    }

    fn handle(&self, id: Uuid) -> Option<Arc<VmHandle>> {
        self.vms.lock().unwrap().get(&id).cloned()
    }

    /// QMP에 연결하고 모니터 태스크를 붙여 등록한다. `launched`면 `Started`를 알린다.
    async fn attach(
        &self,
        id: Uuid,
        paths: RuntimePaths,
        launched: bool,
    ) -> Result<(), DriverError> {
        let qmp = QmpClient::connect(&paths.qmp_socket).await?;
        let pid = read_pid(&paths.pidfile)?;
        let rx = qmp.subscribe();
        // 잠금을 쥔 채로 모니터를 띄우고 등록한다. 모니터가 곧바로 연결 끊김을 보더라도
        // 등록이 끝난 뒤에야 해제할 수 있으므로 죽은 핸들이 남지 않는다.
        // 같은 이유로 Started도 이 인스턴스의 Stopped/Crashed보다 항상 먼저 나간다.
        let mut vms = self.vms.lock().unwrap();
        let monitor = tokio::spawn(monitor(
            id,
            rx,
            Arc::downgrade(&self.vms),
            self.events.clone(),
            paths.clone(),
            self.config.net.clone(),
        ));
        vms.insert(
            id,
            Arc::new(VmHandle {
                qmp,
                pid,
                paths,
                monitor,
            }),
        );
        if launched {
            let _ = self.events.send(HypervisorEvent {
                id,
                kind: EventKind::Started,
            });
        }
        Ok(())
    }

    async fn launch(
        &self,
        vm: &VmRuntimeSpec,
        paths: &RuntimePaths,
        restore: Option<&str>,
    ) -> Result<(), DriverError> {
        std::fs::create_dir_all(&paths.dir)?;
        std::fs::create_dir_all(self.state_dir(vm.id))?;
        if vm.firmware == Firmware::Uefi && !paths.uefi_vars.exists() {
            std::fs::copy(&self.config.host.uefi_vars_template, &paths.uefi_vars)?;
        }
        // 호출자가 pidfile 잠금이 풀린 것(= QEMU 없음)을 확인했으므로 남은 소켓은 지워도 된다.
        // 이전 인스턴스가 남긴 pidfile은 새 QEMU가 잠그고 다시 쓴다.
        for sock in [&paths.qmp_socket, &paths.vnc_socket] {
            remove_if_exists(sock)?;
        }

        let mut args = build_cmdline(vm, &self.config.host, paths)
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        args.push("-daemonize".into());
        self.create_taps(vm).await?;
        let mut cmd = if self.config.systemd_scope {
            clear_stale_scope(vm.id).await;
            // 스코프는 QEMU가 끝날 때까지 남는다. --collect: 실패로 끝나도 남기지 않는다.
            let mut cmd = tokio::process::Command::new("systemd-run");
            cmd.args(["--scope", "--quiet", "--collect"])
                .arg(format!("--unit=opl-vm-{}", vm.id))
                .arg("--")
                .arg(&self.config.host.qemu_binary);
            cmd
        } else {
            tokio::process::Command::new(&self.config.host.qemu_binary)
        };
        cmd.args(&args)
            .stdin(Stdio::null())
            // 타임아웃으로 기다림을 그만두면 부모 프로세스도 끝낸다. 이미 분리된 자식은
            // 실패 정리(kill_and_cleanup)가 pidfile 잠금으로 찾아 끝낸다.
            .kill_on_drop(true);
        if cfg!(target_os = "macos") {
            // macOS에서는 -daemonize의 fork 뒤 자식이 Objective-C 런타임을 초기화하다
            // "fork() was called ... Crashing instead"로 죽는다. 디스플레이/VNC 장치가 있으면
            // 우연히 피하지만, 이 변수를 켜야 장치 구성과 무관하게 안정적으로 뜬다.
            cmd.env("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES");
        }
        // -daemonize: 초기화가 끝나면 부모 프로세스가 종료 코드로 성공 여부를 알려 주고, 자식은 세션에서 분리된다.
        let output = tokio::time::timeout(START_TIMEOUT, cmd.output())
            .await
            .map_err(|_| DriverError::Timeout("QEMU to start"))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(DriverError::StartFailed(stderr));
        }
        tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths.clone(), true))
            .await
            .map_err(|_| DriverError::Timeout("QMP of new QEMU"))??;
        let Some(handle) = self.handle(vm.id) else {
            // 그새 끝났다. 모니터가 종료 이벤트를 보냈다.
            return Ok(());
        };
        if let Some(tag) = restore {
            // -S로 멈춘 채 불러온 뒤 cont로 이어서 실행한다. 메모리 상태는 첫 디스크(disk0)에 있다.
            let devices: Vec<String> = (0..vm.disks.len()).map(|i| format!("disk{i}")).collect();
            let load = job(
                handle
                    .qmp
                    .snapshot_load(tag, "disk0", &devices, SNAPSHOT_TIMEOUT),
                "QMP snapshot-load",
            )
            .await;
            if let Err(e) = load {
                drop(handle);
                // 작업이 끝나지 않았으면 QMP가 답하지 않을 수 있으므로 끝날 때까지 기다리고 필요하면 죽인다.
                if let Err(stop) = self.stop(vm.id, StopMode::Force).await {
                    tracing::error!(id = %vm.id, error = %stop, "failed to stop QEMU after snapshot load failed");
                }
                return Err(e);
            }
        }
        let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
        if let Err(e) = flatten(cont, "QMP cont") {
            let _ = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await;
            return Err(e);
        }
        Ok(())
    }

    /// 브리지 NIC마다 tap을 만들어 브리지에 붙인다. 하나라도 실패하면 만든 것을 지운다.
    async fn create_taps(&self, vm: &VmRuntimeSpec) -> Result<(), DriverError> {
        let taps: Vec<Tap> = vm
            .nics
            .iter()
            .filter_map(|nic| match &nic.backend {
                NicBackend::Tap {
                    ifname,
                    bridge,
                    vlan,
                } => Some(Tap {
                    ifname: ifname.clone(),
                    bridge: bridge.clone(),
                    vlan: *vlan,
                }),
                NicBackend::User => None,
            })
            .collect();
        if taps.is_empty() {
            return Ok(());
        }
        // create_tap은 같은 이름의 장치를 먼저 지우므로, 이 VM의 이름이 아니면 호스트 인터페이스를 지울 수 있다.
        let prefix = tap_prefix(vm.id);
        if let Some(tap) = taps.iter().find(|t| !t.ifname.starts_with(&prefix)) {
            return Err(DriverError::InvalidSpec(format!(
                "tap name {:?} does not start with {prefix:?}",
                tap.ifname
            )));
        }
        let Some(net) = &self.config.net else {
            return Err(DriverError::InvalidSpec(
                "bridge networks are not supported on this node".into(),
            ));
        };
        for tap in &taps {
            if let Err(e) = net.create_tap(tap).await {
                delete_taps(Some(net.as_ref()), vm.id).await;
                return Err(e);
            }
        }
        Ok(())
    }

    /// 등록되지 않은 QEMU(기동 실패로 남은 데몬, 붙지 못한 VM)를 SIGKILL로 끝내고 런타임
    /// 디렉터리를 지운다. 끝냈음을 확인하지 못하면 살아 있는 VM의 파일이므로 아무것도 지우지
    /// 않고 false를 돌려준다.
    async fn kill_and_cleanup(&self, id: Uuid, paths: &RuntimePaths) -> bool {
        if liveness(&paths.pidfile) != Liveness::Dead {
            let pid = match read_pid(&paths.pidfile) {
                Ok(pid) => pid,
                Err(e) => {
                    tracing::error!(%id, error = %e, "QEMU has no readable pid; keeping runtime dir");
                    return false;
                }
            };
            kill_if_owner(&paths.pidfile, pid);
            if !wait_until(KILL_TIMEOUT, || liveness(&paths.pidfile) == Liveness::Dead).await {
                tracing::error!(%id, pid, "QEMU did not exit; keeping runtime dir");
                return false;
            }
        }
        // tap부터 지운다. 지우지 못하면 런타임 디렉터리를 남겨 다음 new()가 다시 시도한다.
        if delete_taps(self.config.net.as_deref(), id).await
            && let Err(e) = std::fs::remove_dir_all(&paths.dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, error = %e, "failed to remove runtime dir");
        }
        true
    }

    /// QMP로 다룰 수 있는 실행 중인 VM의 핸들
    fn running_handle(&self, id: Uuid) -> Result<Arc<VmHandle>, DriverError> {
        if let Some(handle) = self.handle(id) {
            return Ok(handle);
        }
        match liveness(&self.paths(id)?.pidfile) {
            Liveness::Dead => Err(DriverError::NotRunning(id)),
            // 살아 있지만 QMP로 다룰 수 없다. 꺼졌다고 알리면 호출자가 쓰고 있는 디스크를 건드린다.
            // 요청을 보내지 않았으므로 시간 초과(작업이 남아 있을 수 있음)와 구별한다.
            Liveness::Alive(_) | Liveness::Unknown => Err(DriverError::NotAttached(id)),
        }
    }

    /// opld가 `-daemonize`와 `cont` 사이에 죽으면 VM이 `-S`(prelaunch) 상태로 남는다. 이어서 실행한다.
    async fn resume_if_prelaunch(&self, id: Uuid) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            return Ok(());
        };
        let status = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await;
        if flatten(status, "QMP query-status")? == RunState::Prelaunch {
            tracing::info!(%id, "continuing VM left in prelaunch");
            let cont = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.cont()).await;
            flatten(cont, "QMP cont")?;
        }
        Ok(())
    }

    async fn query_state(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let Some(handle) = self.handle(id) else {
            // 살아 있지만 QMP에 붙지 못한 VM도 드러내야 reconciler가 놓치지 않는다.
            return match liveness(&self.paths(id)?.pidfile) {
                Liveness::Dead => Ok(None),
                Liveness::Alive(pid) => Ok(Some(ActualState {
                    id,
                    pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                Liveness::Unknown => Err(DriverError::Io(std::io::Error::other(format!(
                    "cannot tell whether QEMU of VM {id} is running"
                )))),
            };
        };
        let status = match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.query_status()).await {
            Err(_) => return Err(DriverError::Timeout("QMP query-status")),
            Ok(Ok(RunState::Running)) => RuntimeStatus::Running,
            Ok(Ok(RunState::Prelaunch | RunState::Paused)) => RuntimeStatus::Paused,
            Ok(Ok(RunState::Shutdown)) => RuntimeStatus::Other("shutdown".into()),
            Ok(Ok(RunState::Other(s))) => RuntimeStatus::Other(s),
            // 조회하는 사이에 프로세스가 끝났다.
            Ok(Err(QmpError::Disconnected)) => return Ok(None),
            Ok(Err(e)) => return Err(e.into()),
        };
        Ok(Some(ActualState {
            id,
            pid: handle.pid,
            status,
        }))
    }
}

#[async_trait]
impl Hypervisor for QemuHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        // 이미 살아 있는 VM에는 restore를 쓰지 않는다. -S로 남은 VM도 그대로 이어서 실행한다.
        if self.handle(vm.id).is_some() {
            return self.resume_if_prelaunch(vm.id).await;
        }
        let paths = self.paths(vm.id)?;
        if liveness(&paths.pidfile) != Liveness::Dead {
            // 살아 있지만 등록되지 않은 VM(재시작 때 붙지 못함, QMP 연결 유실). 새로 띄우면
            // pidfile 잠금에 걸려 실패하고 살아 있는 VM의 파일을 지우게 되므로 다시 붙기만 한다.
            // 생존을 판정할 수 없을 때도 같은 이유로 새로 띄우지 않는다.
            match tokio::time::timeout(ATTACH_TIMEOUT, self.attach(vm.id, paths, false)).await {
                Ok(Ok(())) => {
                    tracing::info!(id = %vm.id, "reattached to running VM");
                    return self.resume_if_prelaunch(vm.id).await;
                }
                Ok(Err(e)) => {
                    tracing::error!(id = %vm.id, error = %e, "could not reattach to running VM")
                }
                Err(_) => tracing::error!(id = %vm.id, "timed out reattaching to running VM"),
            }
            return Err(DriverError::Timeout("reattach to running QEMU"));
        }
        if restore.is_some() && vm.disks.is_empty() {
            return Err(DriverError::InvalidSpec(
                "a snapshot to restore needs a disk for its memory state".into(),
            ));
        }
        if let Err(e) = self.launch(vm, &paths, restore).await {
            if self.handle(vm.id).is_none() {
                self.kill_and_cleanup(vm.id, &paths).await;
            }
            return Err(e);
        }
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        let Some(handle) = self.handle(id) else {
            let paths = self.paths(id)?;
            if liveness(&paths.pidfile) == Liveness::Dead {
                return Ok(());
            }
            // 살아 있지만 QMP로 다룰 수 없는 VM
            return match mode {
                StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                StopMode::Force if self.kill_and_cleanup(id, &paths).await => Ok(()),
                StopMode::Force => Err(DriverError::Timeout("QEMU to exit")),
            };
        };
        match mode {
            StopMode::Acpi => {
                let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.system_powerdown()).await;
                flatten(r, "QMP system_powerdown")?;
            }
            StopMode::Force => {
                let pid = handle.pid;
                let pidfile = handle.paths.pidfile.clone();
                match tokio::time::timeout(QMP_TIMEOUT, handle.qmp.quit()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(%id, error = %e, "QMP quit failed"),
                    Err(_) => tracing::warn!(%id, "QMP quit timed out"),
                }
                drop(handle);
                // 등록이 풀렸어도 QMP만 끊기고 프로세스는 살아 있을 수 있으므로 pidfile도 확인한다.
                let gone = || self.handle(id).is_none() && liveness(&pidfile) == Liveness::Dead;
                if !wait_until(FORCE_STOP_TIMEOUT, gone).await {
                    tracing::warn!(%id, pid, "QEMU did not quit; sending SIGKILL");
                    kill_if_owner(&pidfile, pid);
                    if !wait_until(KILL_TIMEOUT, gone).await {
                        return Err(DriverError::Timeout("QEMU to exit"));
                    }
                }
            }
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        match self.query_state(id).await {
            // 멈춘 QEMU도 list()처럼 살아 있는 VM으로 드러내야 reconciler가 강제 종료로 넘어갈 수 있다.
            Err(DriverError::Timeout(what)) => match self.handle(id) {
                Some(handle) => Ok(Some(ActualState {
                    id,
                    pid: handle.pid,
                    status: RuntimeStatus::Other("unresponsive".into()),
                })),
                None => Err(DriverError::Timeout(what)),
            },
            other => other,
        }
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let mut ids: Vec<Uuid> = self.vms.lock().unwrap().keys().copied().collect();
        // 등록되지 않았지만 살아 있는 VM도 포함하도록 런타임 디렉터리를 함께 훑는다.
        for entry in std::fs::read_dir(self.config.run_dir.join("vm"))? {
            if let Some(id) = entry?
                .file_name()
                .to_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.query_state(id).await {
                Ok(Some(st)) => out.push(st),
                Ok(None) => {}
                Err(DriverError::Io(e)) if self.handle(id).is_none() => {
                    tracing::error!(%id, error = %e, "skipping VM whose state is unknown");
                }
                // VM 하나가 응답하지 않는다고 전체 조회가 실패하면 안 된다.
                Err(DriverError::Timeout(_)) => {
                    if let Some(handle) = self.handle(id) {
                        out.push(ActualState {
                            id,
                            pid: handle.pid,
                            status: RuntimeStatus::Other("unresponsive".into()),
                        });
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        self.handle(id)
            .map(|h| h.paths.vnc_socket.clone())
            .ok_or(DriverError::NotRunning(id))
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        let Some(vmstate) = devices.first() else {
            return Err(DriverError::InvalidSpec(format!("VM {id} has no disks")));
        };
        if memory {
            let save = handle
                .qmp
                .snapshot_save(tag, vmstate, &devices, SNAPSHOT_TIMEOUT);
            job(save, "QMP snapshot-save").await
        } else {
            let r =
                tokio::time::timeout(SNAPSHOT_TIMEOUT, handle.qmp.snapshot_disks(tag, &devices))
                    .await;
            flatten(r, "QMP transaction")
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let devices = query_disk_nodes(&handle.qmp).await?;
        if devices.is_empty() {
            return Ok(());
        }
        let delete = handle.qmp.snapshot_delete(tag, &devices, SNAPSHOT_TIMEOUT);
        job(delete, "QMP snapshot-delete").await
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let handle = self.running_handle(id)?;
        let node = format!("disk{disk}");
        let r = tokio::time::timeout(QMP_TIMEOUT, handle.qmp.block_resize(&node, size_bytes)).await;
        flatten(r, "QMP block_resize")
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

/// 실행 중인 VM의 디스크 노드 이름(`disk{i}`)을 디스크 순서로 읽는다. 다시 붙은 VM은 스펙을
/// 모르므로 QEMU에 묻는다.
async fn query_disk_nodes(qmp: &QmpClient) -> Result<Vec<String>, DriverError> {
    let r = tokio::time::timeout(QMP_TIMEOUT, qmp.execute("query-named-block-nodes", None)).await;
    Ok(disk_nodes(&flatten(r, "QMP query-named-block-nodes")?))
}

/// 블록 노드 목록에서 cmdline이 디스크 포맷 노드에 붙인 `disk{i}`만 골라 i 순서로 돌려준다.
fn disk_nodes(nodes: &serde_json::Value) -> Vec<String> {
    let mut disks: Vec<(usize, String)> = nodes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["node-name"].as_str())
        .filter_map(|name| {
            let i = name.strip_prefix("disk")?.parse().ok()?;
            Some((i, name.to_string()))
        })
        .collect();
    disks.sort();
    disks.into_iter().map(|(_, name)| name).collect()
}

/// QMP 작업 명령을 기다린다. 작업의 제한 시간은 이벤트를 기다리는 동안에만 걸리므로, QEMU가
/// 명령 자체에 답하지 않아도(SIGSTOP 등) 묶이지 않게 한 번 더 감싼다.
async fn job(
    fut: impl Future<Output = Result<(), QmpError>>,
    what: &'static str,
) -> Result<(), DriverError> {
    flatten(
        tokio::time::timeout(SNAPSHOT_TIMEOUT + QMP_TIMEOUT, fut).await,
        what,
    )
}

/// QMP 연결이 끊길 때까지 이벤트를 지켜보다가, 종료 종류를 판정해 알리고 런타임 파일을 정리한다.
async fn monitor(
    id: Uuid,
    mut rx: broadcast::Receiver<QmpEvent>,
    // Weak: 맵이 핸들을, 핸들이 이 태스크를 소유하므로 강한 참조를 쥐면 순환이 생겨
    // 드라이버를 내려도 QMP 연결이 닫히지 않는다.
    vms: Weak<Mutex<HashMap<Uuid, Arc<VmHandle>>>>,
    events: broadcast::Sender<HypervisorEvent>,
    paths: RuntimePaths,
    net: Option<Arc<dyn HostNet>>,
) {
    let mut guest_initiated = None;
    loop {
        match rx.recv().await {
            Ok(ev) if ev.event == "SHUTDOWN" => {
                guest_initiated = Some(ev.data["guest"].as_bool().unwrap_or(false));
            }
            Ok(_) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => break,
        }
    }
    let unregister = || {
        if let Some(vms) = vms.upgrade() {
            let handle = vms.lock().unwrap().remove(&id);
            drop(handle);
        }
    };
    let dead = || liveness(&paths.pidfile) == Liveness::Dead;
    // (종료 종류, 프로세스가 끝나 런타임 디렉터리를 지워도 되는지)
    let (kind, exited) = match guest_initiated {
        // SHUTDOWN을 받았으면 종료는 확정이다. QEMU는 소켓을 닫고 곧 끝나므로 잠깐 기다려
        // 디렉터리를 지우되, 플러시 등으로 늦어지면 잠금이 걸린 디렉터리는 남겨 두고
        // (다음 launch/new()가 잠금 해제를 확인하고 정리한다) 종료는 바로 알린다.
        Some(guest_initiated) => {
            let exited = wait_until(SHUTDOWN_EXIT_WAIT, dead).await;
            if !exited {
                tracing::info!(%id, "QEMU is still exiting; leaving runtime dir");
            }
            (EventKind::Stopped { guest_initiated }, exited)
        }
        None => {
            if !wait_until(EXIT_GRACE, dead).await {
                // SHUTDOWN 없이 QMP만 끊기고 QEMU는 살아 있다. 파일을 지우거나 종료로 알리지
                // 않고 등록만 푼다. inspect/list에 "unresponsive"로 드러나고, 이후 start()가
                // 다시 붙거나 stop(Force)가 끝낸다.
                tracing::error!(%id, "lost QMP connection to running QEMU");
                unregister();
                return;
            }
            (EventKind::Crashed, true)
        }
    };
    // 등록을 풀기 전에 tap을 지운다. QEMU가 아직 끝나는 중이어도 tap은 곧바로 지워진다(남은 fd는
    // 오류를 받는다). 게스트는 이미 꺼졌으므로 괜찮다. 런타임 디렉터리는 정리할 것이 남았다는
    // 유일한 기록이므로 tap을 지운 뒤에 지운다. 그 전에 끊기거나(드라이버가 내려가 이 태스크가
    // abort됨) tap을 지우지 못하면 디렉터리가 남아 다음 launch/new()가 다시 정리한다.
    if delete_taps(net.as_deref(), id).await
        && exited
        && let Err(e) = std::fs::remove_dir_all(&paths.dir)
    {
        tracing::warn!(%id, error = %e, "failed to remove runtime dir");
    }
    // 이벤트를 받은 쪽이 곧바로 start를 불러도 "이미 실행 중"으로 오판하지 않도록 등록부터 해제한다.
    // 핸들이 drop되면서 이 태스크에 abort가 걸리지만, 아래에는 await가 없으므로 끝까지 실행된다.
    unregister();
    let _ = events.send(HypervisorEvent { id, kind });
}

/// VM의 tap을 지운다. 지웠거나 지울 것이 없으면 true. 실패하면 기록하고 false를 돌려주며,
/// 호출자는 런타임 디렉터리를 남겨 다음 new()가 다시 지우게 한다.
async fn delete_taps(net: Option<&dyn HostNet>, id: Uuid) -> bool {
    if let Some(net) = net
        && let Err(e) = net.delete_vm_taps(id).await
    {
        tracing::warn!(%id, error = %e, "failed to delete tap devices");
        return false;
    }
    true
}

/// 끝난 QEMU의 `--collect` 스코프는 systemd가 비동기로 회수하므로, 곧바로 다시 띄우면
/// systemd-run이 "Unit ... already exists"로 실패한다. 호출자가 이전 QEMU가 없음을 확인했으므로
/// 남은 스코프를 멈추고 실패 기록을 지운다. 오류와 출력은 무시한다.
async fn clear_stale_scope(id: Uuid) {
    let unit = format!("opl-vm-{id}.scope");
    for verb in ["stop", "reset-failed"] {
        let status = tokio::process::Command::new("systemctl")
            .args([verb, "--quiet", &unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        let _ = tokio::time::timeout(KILL_TIMEOUT, status).await;
    }
}

/// pidfile로 본 QEMU의 생존 여부
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// 이 pid가 pidfile 잠금을 쥐고 있다.
    Alive(u32),
    Dead,
    /// 판정할 수 없다. 살아 있을 수 있으므로 파일을 지우거나 프로세스를 죽이지 않는다.
    Unknown,
}

/// QEMU는 수명 내내 `-pidfile`에 POSIX 잠금(lockf)을 쥐고 있다(SIGSTOP 상태에서도).
/// `F_GETLK`로 잠금을 잡지 않고 소유자를 조회한다. 잠금이 없으면 QEMU는 없다.
/// 같은 프로세스가 쥔 잠금은 보이지 않지만, opld는 pidfile을 잠그지 않는다.
fn liveness(pidfile: &Path) -> Liveness {
    let file = match std::fs::File::open(pidfile) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Liveness::Dead,
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot open pidfile");
            return Liveness::Unknown;
        }
    };
    // 파일 전체(길이 0 = 끝까지)에 대한 쓰기 잠금을 막는 잠금이 있는지 묻는다.
    let probe = Flock {
        start: 0,
        length: 0,
        pid: None,
        typ: FlockType::WriteLock,
        offset_type: FlockOffsetType::Set,
    };
    match fcntl_getlk(&file, &probe) {
        Ok(None) => Liveness::Dead,
        Ok(Some(lock)) => match lock.pid {
            Some(pid) => Liveness::Alive(pid.as_raw_nonzero().get() as u32),
            None => {
                tracing::warn!(?pidfile, "pidfile lock has no owner pid");
                Liveness::Unknown
            }
        },
        Err(e) => {
            tracing::warn!(?pidfile, error = %e, "cannot query pidfile lock");
            Liveness::Unknown
        }
    }
}

/// `pid`가 지금 pidfile 잠금의 소유자일 때만 SIGKILL을 보낸다. pid 재사용이나 잘못된 pidfile로
/// 엉뚱한 프로세스를 죽이지 않기 위해서다.
fn kill_if_owner(pidfile: &Path, pid: u32) {
    match liveness(pidfile) {
        Liveness::Alive(owner) if owner == pid => {}
        Liveness::Dead => return,
        other => {
            tracing::error!(
                ?pidfile,
                pid,
                ?other,
                "pid does not own the pidfile lock; not killing"
            );
            return;
        }
    }
    let Some(target) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    match kill_process(target, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(e) => tracing::warn!(pid, error = %e, "failed to kill QEMU"),
    }
}

/// `cond`가 참이 될 때까지 짧게 쉬며 확인한다. 한도 안에 참이 되면 true.
async fn wait_until(limit: Duration, mut cond: impl FnMut() -> bool) -> bool {
    tokio::time::timeout(limit, async {
        while !cond() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}

/// QMP_TIMEOUT으로 감싼 명령의 결과를 풀어 낸다.
fn flatten<T>(
    r: Result<Result<T, QmpError>, tokio::time::error::Elapsed>,
    what: &'static str,
) -> Result<T, DriverError> {
    Ok(r.map_err(|_| DriverError::Timeout(what))??)
}

fn read_pid(pidfile: &Path) -> Result<u32, DriverError> {
    let text = std::fs::read_to_string(pidfile)?;
    text.trim().parse().map_err(|_| {
        DriverError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("bad pidfile {pidfile:?}: {text:?}"),
        ))
    })
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use opl_qemu::{Accel, Arch};
    use opl_types::runtime::RuntimeNic;
    use opl_types::vm::{CpuModel, CpuSpec, MachineType, NicModel};

    use super::*;

    fn config(root: &Path, qemu_binary: PathBuf) -> QemuConfig {
        let template = root.join("vars-template.fd");
        std::fs::write(&template, b"vars").unwrap();
        QemuConfig {
            host: HostProfile {
                arch: Arch::Aarch64,
                accel: Accel::Tcg,
                qemu_binary,
                uefi_code: root.join("code.fd"),
                uefi_vars_template: template,
            },
            run_dir: root.join("run"),
            state_dir: root.join("state"),
            net: None,
            systemd_scope: false,
        }
    }

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    fn short_tempdir() -> tempfile::TempDir {
        // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
        tempfile::Builder::new()
            .prefix("opl")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[tokio::test]
    async fn start_failure_reports_qemu_stderr_and_cleans_run_dir() {
        let tmp = short_tempdir();
        let fake = tmp.path().join("fake-qemu");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'qemu: something broke' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hv = QemuHypervisor::new(config(tmp.path(), fake)).await.unwrap();
        let vm = spec();

        let err = hv.start(&vm, None).await.unwrap_err();
        match &err {
            DriverError::StartFailed(msg) => assert_eq!(msg, "qemu: something broke"),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!err.is_transient());
        assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
        // UEFI 변수 파일은 state_dir에 복사되어 남는다.
        let vars = tmp
            .path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd");
        assert_eq!(std::fs::read(vars).unwrap(), b"vars");
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn new_removes_stale_runtime_dirs_and_ignores_foreign_entries() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        // pidfile이 없다.
        let stale = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("qmp.sock"), b"not a socket").unwrap();
        // pidfile은 있지만 아무도 잠그고 있지 않다(kill -9 뒤의 모습).
        let unlocked = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        std::fs::create_dir_all(&unlocked).unwrap();
        std::fs::write(unlocked.join("qemu.pid"), b"4242\n").unwrap();
        let foreign = cfg.run_dir.join("vm").join("not-a-uuid");
        std::fs::create_dir_all(&foreign).unwrap();

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(!stale.exists());
        assert!(!unlocked.exists());
        assert!(foreign.exists());
        assert!(hv.list().await.unwrap().is_empty());
    }

    /// 살아 있는 QEMU처럼 `dir/qemu.pid`에 fcntl 잠금을 걸고 자기 pid를 써 두는 자식 프로세스.
    /// fcntl 잠금은 프로세스 단위라 테스트 프로세스가 직접 잠그면 드라이버가 잠금을 볼 수 없다.
    /// 반환값을 drop하면 자식이 죽어 잠금이 풀린다.
    async fn hold_pidfile(dir: &Path) -> tokio::process::Child {
        use tokio::io::{AsyncBufReadExt, BufReader};

        const SCRIPT: &str = "import fcntl, os, sys, time
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o644)
fcntl.lockf(fd, fcntl.LOCK_EX)
os.ftruncate(fd, 0)
os.write(fd, b'%d\\n' % os.getpid())
print('locked', flush=True)
time.sleep(60)
";
        std::fs::create_dir_all(dir).unwrap();
        let mut child = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(SCRIPT)
            .arg(dir.join("qemu.pid"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(line.trim(), "locked");
        child
    }

    struct FakeQmp {
        /// 클라이언트가 연결을 닫으면 완료된다.
        closed: tokio::sync::oneshot::Receiver<()>,
        /// 받은 명령 이름
        commands: Arc<Mutex<Vec<String>>>,
        /// 받은 명령의 인자(`commands`와 같은 순서)
        arguments: Arc<Mutex<Vec<serde_json::Value>>>,
        /// 보내면 서버가 (주어진 이벤트를 먼저 보낸 뒤) 연결을 끊는다.
        hangup: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    }

    /// QEMU 대신 QMP 소켓을 여는 가짜 서버. greeting과 capabilities 협상까지 처리하고,
    /// `query-status`에는 `status`를 답한다.
    async fn fake_qmp(dir: &Path, status: &'static str) -> FakeQmp {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        std::fs::create_dir_all(dir).unwrap();
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        let (closed_tx, closed) = tokio::sync::oneshot::channel();
        let (hangup, mut hangup_rx) = tokio::sync::oneshot::channel::<Option<serde_json::Value>>();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = commands.clone();
        let args = Arc::new(Mutex::new(Vec::new()));
        let arguments = args.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            write
                .write_all(b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\n")
                .await
                .unwrap();
            loop {
                let line = tokio::select! {
                    ev = &mut hangup_rx => {
                        if let Ok(Some(ev)) = ev {
                            let _ = write.write_all(format!("{ev}\n").as_bytes()).await;
                        }
                        break;
                    }
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => line,
                        _ => break,
                    },
                };
                let cmd: serde_json::Value = serde_json::from_str(&line).unwrap();
                let name = cmd["execute"].as_str().unwrap_or_default().to_string();
                let ret = match name.as_str() {
                    "query-status" => {
                        serde_json::json!({ "running": status == "running", "status": status })
                    }
                    // 디스크 두 개를 가진 VM의 블록 노드
                    "query-named-block-nodes" => serde_json::json!([
                        { "node-name": "disk1" },
                        { "node-name": "disk1-file" },
                        { "node-name": "disk0" },
                        { "node-name": "disk0-file" },
                    ]),
                    _ => serde_json::json!({}),
                };
                recorded.lock().unwrap().push(name);
                arguments.lock().unwrap().push(cmd["arguments"].clone());
                let reply = serde_json::json!({ "return": ret, "id": cmd["id"] });
                if write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = closed_tx.send(());
        });
        FakeQmp {
            closed,
            commands,
            arguments: args,
            hangup,
        }
    }

    #[tokio::test]
    async fn dropping_the_driver_releases_qmp_connections() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let dir = cfg.run_dir.join("vm").join(Uuid::now_v7().to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert_eq!(hv.list().await.unwrap().len(), 1);
        drop(hv);

        // QEMU는 QMP 클라이언트를 하나만 받으므로, 연결이 남아 있으면 재시작한 opld가 붙을 수 없다.
        tokio::time::timeout(Duration::from_secs(2), fake.closed)
            .await
            .expect("QMP connection was not closed after dropping the driver")
            .unwrap();
        assert!(dir.exists(), "runtime dir of a live VM must not be removed");
    }

    #[tokio::test]
    async fn new_keeps_runtime_dir_when_reattach_fails_for_other_reasons() {
        use tokio::io::AsyncWriteExt;

        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let id = Uuid::now_v7();
        let dir = cfg.run_dir.join("vm").join(id.to_string());
        // pidfile이 잠겨 있으니 QEMU는 살아 있다. 소켓은 QMP가 아니어서 붙을 수 없지만 지우면 안 된다.
        let _qemu = hold_pidfile(&dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(b"{\"hello\":1}\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.handle(id).is_none());
        // 등록하지 않지만 살아 있는 VM으로 드러난다.
        let listed = hv.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].status,
            RuntimeStatus::Other("unresponsive".into())
        );
        assert!(dir.exists());
    }

    #[tokio::test]
    async fn start_continues_vm_left_in_prelaunch() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // opld가 -daemonize와 cont 사이에 죽어 -S 상태로 남은 VM
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "prelaunch").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        hv.start(&vm, None).await.unwrap();
        assert!(
            fake.commands.lock().unwrap().contains(&"cont".to_string()),
            "commands: {:?}",
            fake.commands.lock().unwrap()
        );
    }

    #[test]
    fn disk_nodes_are_format_nodes_in_disk_order() {
        let nodes = serde_json::json!([
            { "node-name": "disk10" },
            { "node-name": "disk2-file" },
            { "node-name": "disk2" },
            { "node-name": "#block123" },
            { "node-name": "diskx" },
            { "node-name": "disk0" },
        ]);
        assert_eq!(disk_nodes(&nodes), ["disk0", "disk2", "disk10"]);
    }

    #[tokio::test]
    async fn snapshot_ops_use_disk_node_names() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        // 디스크만 스냅샷은 모든 디스크를 transaction 하나로 찍는다.
        hv.snapshot_save(vm.id, "s1", false).await.unwrap();
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        let commands = fake.commands.lock().unwrap().clone();
        let arguments = fake.arguments.lock().unwrap().clone();
        let at = |name: &str| commands.iter().position(|c| c == name).unwrap();
        let actions = &arguments[at("transaction")]["actions"];
        assert_eq!(actions[0]["data"]["device"], "disk0");
        assert_eq!(actions[1]["data"]["device"], "disk1");
        assert_eq!(actions[1]["data"]["name"], "s1");
        assert_eq!(
            arguments[at("block_resize")],
            serde_json::json!({ "node-name": "disk1", "size": 2u64 << 30 })
        );
    }

    #[tokio::test]
    async fn snapshot_ops_on_vm_that_is_not_attached() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let err = hv
            .snapshot_save(Uuid::now_v7(), "s", true)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::NotRunning(_)), "{err:?}");

        // 살아 있지만 QMP로 다룰 수 없는 VM을 꺼진 것으로 알리면 호출자가 쓰고 있는 디스크를
        // qemu-img로 건드린다. 다시 시도할 수 있는 오류로 끝낸다.
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        for err in [
            hv.snapshot_save(vm.id, "s", true).await.unwrap_err(),
            hv.snapshot_delete(vm.id, "s").await.unwrap_err(),
            hv.block_resize(vm.id, 0, 1 << 30).await.unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::NotAttached(_)), "{err:?}");
            assert!(err.is_transient());
        }
    }

    #[tokio::test]
    async fn restore_without_disks_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        // 메모리 상태는 첫 디스크에 있으므로 디스크가 없으면 띄우기 전에 거부한다.
        let err = hv.start(&spec(), Some("s1")).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn start_does_not_touch_live_vm_it_cannot_attach_to() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let hv = QemuHypervisor::new(cfg.clone()).await.unwrap();
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는다.
        let qemu = unresponsive_vm(&dir).await;

        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.join("qemu.pid").exists());
        assert!(dir.join("qmp.sock").exists());
        // 등록되지 않았어도 살아 있는 VM은 조회에 드러나야 reconciler가 놓치지 않는다.
        let expected = ActualState {
            id: vm.id,
            pid: qemu.id().unwrap(),
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(expected.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![expected]);
    }

    /// QEMU는 살아 있지만(SIGSTOP 등) QMP에 답하지 않는 런타임 디렉터리를 만든다.
    async fn unresponsive_vm(dir: &Path) -> tokio::process::Child {
        let qemu = hold_pidfile(dir).await;
        let listener = tokio::net::UnixListener::bind(dir.join("qmp.sock")).unwrap();
        tokio::spawn(async move {
            let _conn = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        qemu
    }

    #[tokio::test]
    async fn liveness_reports_lock_owner() {
        let tmp = short_tempdir();
        let dir = tmp.path().join("vm");
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
        let mut qemu = hold_pidfile(&dir).await;
        assert_eq!(
            liveness(&dir.join("qemu.pid")),
            Liveness::Alive(qemu.id().unwrap())
        );
        qemu.kill().await.unwrap();
        // kill -9 뒤에도 pidfile은 남지만 잠금은 풀린다.
        assert!(dir.join("qemu.pid").exists());
        assert_eq!(liveness(&dir.join("qemu.pid")), Liveness::Dead);
    }

    #[tokio::test]
    async fn force_stop_kills_live_unregistered_vm() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let mut qemu = unresponsive_vm(&dir).await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        assert!(hv.inspect(vm.id).await.unwrap().is_some());

        // ACPI 요청은 QMP 없이 보낼 수 없다. 다시 시도할 수 있는 오류다.
        let err = hv.stop(vm.id, StopMode::Acpi).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(dir.exists());

        hv.stop(vm.id, StopMode::Force).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(2), qemu.wait())
            .await
            .expect("QEMU was not killed")
            .unwrap();
        assert!(!status.success());
        assert!(!dir.exists());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn force_stop_never_kills_a_pid_that_does_not_own_the_lock() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = unresponsive_vm(&dir).await;
        // pidfile에 적힌 pid가 잠금 소유자와 다르다(pid 재사용, 다른 프로세스가 덮어씀 등).
        let mut bystander = tokio::process::Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let bystander_pid = bystander.id().unwrap();
        std::fs::write(dir.join("qemu.pid"), format!("{bystander_pid}\n")).unwrap();
        let hv = QemuHypervisor::new(cfg).await.unwrap();

        let err = hv.stop(vm.id, StopMode::Force).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "bystander was killed"
        );
        assert!(dir.join("qemu.pid").exists());
    }

    #[tokio::test]
    async fn guest_shutdown_is_reported_even_if_qemu_exits_slowly() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        // QEMU가 SHUTDOWN을 보내고 QMP를 닫았지만 아직 플러시 중이라 잠금을 쥐고 있다.
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        let shutdown = serde_json::json!({
            "event": "SHUTDOWN",
            "data": { "guest": true, "reason": "guest-shutdown" },
            "timestamp": { "seconds": 0, "microseconds": 0 }
        });
        fake.hangup.send(Some(shutdown)).unwrap();
        let ev = tokio::time::timeout(EXIT_GRACE - Duration::from_secs(2), rx.recv())
            .await
            .expect("Stopped was not reported promptly")
            .unwrap();
        assert_eq!(
            ev,
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.handle(vm.id).is_none());
        // 잠금이 남아 있으니 파일은 지우지 않는다. 다음 launch/new()가 정리한다.
        assert!(dir.join("qemu.pid").exists());
    }

    #[tokio::test]
    async fn losing_qmp_while_qemu_lives_unregisters_without_cleanup() {
        let tmp = short_tempdir();
        let cfg = config(tmp.path(), PathBuf::from("/nonexistent/qemu"));
        let vm = spec();
        let dir = cfg.run_dir.join("vm").join(vm.id.to_string());
        let _qemu = hold_pidfile(&dir).await;
        let fake = fake_qmp(&dir, "running").await;
        let hv = QemuHypervisor::new(cfg).await.unwrap();
        let mut rx = hv.subscribe();

        fake.hangup.send(None).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while hv.handle(vm.id).is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("VM was not unregistered");
        // 살아 있는 VM이므로 파일을 지우거나 종료로 알리면 안 된다. 이후 start()가 다시 붙는다.
        assert!(dir.join("qemu.pid").exists());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn stop_and_console_on_unknown_vm() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let id = Uuid::now_v7();
        hv.stop(id, StopMode::Force).await.unwrap();
        hv.stop(id, StopMode::Acpi).await.unwrap();
        assert!(matches!(
            hv.console_endpoint(id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn bridge_nic_without_host_net_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: opl_types::network::tap_name(vm.id, 0),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        let err = hv.start(&vm, None).await.unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    #[tokio::test]
    async fn tap_name_of_another_vm_or_host_is_invalid() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let mut vm = spec();
        // create_tap이 같은 이름을 먼저 지우므로 호스트 인터페이스 이름은 받으면 안 된다.
        vm.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::Tap {
                ifname: "eth0".into(),
                bridge: "vmbr0".into(),
                vlan: None,
            },
        });
        match hv.start(&vm, None).await.unwrap_err() {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("eth0"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn incompatible_spec_is_invalid_not_start_failure() {
        let tmp = short_tempdir();
        let hv = QemuHypervisor::new(config(tmp.path(), PathBuf::from("/nonexistent/qemu")))
            .await
            .unwrap();
        let vm = VmRuntimeSpec {
            machine: MachineType::Q35,
            ..spec()
        };
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::InvalidSpec(_))
        ));
    }
}
```

`crates/opl-driver/tests/qemu_bridge.rs`에서

```rust
    let tap = tap_name(vm.id, 0);

    hv.start(&vm).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    hv.stop(vm.id, StopMode::Force).await.unwrap();
```

을 다음으로 바꾼다:

```rust
    let tap = tap_name(vm.id, 0);

    hv.start(&vm, None).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
    hv.stop(vm.id, StopMode::Force).await.unwrap();
```

`crates/opl-driver/tests/qemu_bridge.rs`에서

```rust
    let mut rx = hv.subscribe();
    hv.start(&vm).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
```

을 다음으로 바꾼다:

```rust
    let mut rx = hv.subscribe();
    hv.start(&vm, None).await.unwrap();
    assert_eq!(master_of(&tap), Some(br.0.clone()));
```

`crates/opl-driver/tests/qemu_bridge.rs`에서

```rust
    });
    let err = hv.start(&vm).await.unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
```

을 다음으로 바꾼다:

```rust
    });
    let err = hv.start(&vm, None).await.unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
```

`crates/opl-driver/tests/qemu_bridge.rs`에서

```rust
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm).await.unwrap();
        assert_eq!(master_of(&tap), Some(br.0.clone()));
```

을 다음으로 바꾼다:

```rust
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(master_of(&tap), Some(br.0.clone()));
```

`crates/opl-driver/tests/qemu_driver.rs` 전체:

```rust
//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::Path;
use std::time::Duration;

use opl_driver::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, QemuConfig, QemuHypervisor, RuntimeStatus,
    StopMode,
};
use opl_qemu::{Arch, HostProfile};
use opl_types::runtime::{ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware};
use tokio::sync::broadcast;
use uuid::Uuid;

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // systemd가 있는 Linux(CI)에서 `OPL_TEST_SYSTEMD=1`로 스코프 실행까지 확인한다.
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
    }
}

fn spec() -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "it-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![],
        nics: vec![],
    }
}

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

async fn next_event_for(rx: &mut broadcast::Receiver<HypervisorEvent>, id: Uuid) -> EventKind {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = rx.recv().await.unwrap();
            if ev.id == id {
                return ev.kind;
            }
        }
    })
    .await
    .expect("no event within 10s")
}

#[tokio::test]
async fn start_inspect_and_force_stop() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let mut rx = hv.subscribe();
    let vm = spec();

    hv.start(&vm, None).await.unwrap();
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    hv.start(&vm, None).await.unwrap(); // 이미 실행 중이면 그대로 성공

    let st = hv.inspect(vm.id).await.unwrap().expect("running");
    assert_eq!(st.status, RuntimeStatus::Running);
    assert!(st.pid > 0);
    assert_eq!(hv.list().await.unwrap().len(), 1);
    assert!(hv.console_endpoint(vm.id).await.unwrap().exists());

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(
        next_event_for(&mut rx, vm.id).await,
        EventKind::Stopped {
            guest_initiated: false
        }
    );
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
    assert!(
        tmp.path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd")
            .exists()
    );
}

#[tokio::test]
async fn reattaches_after_driver_restart() {
    let tmp = short_tempdir();
    let vm = spec();
    let pid = {
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap().pid
        // 드라이버(= opld)가 내려간다. QEMU는 분리되어 있으므로 계속 돌아야 한다.
    };

    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let st = hv
        .inspect(vm.id)
        .await
        .unwrap()
        .expect("VM should survive driver restart");
    assert_eq!(st.pid, pid);
    assert_eq!(st.status, RuntimeStatus::Running);

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
}

#[tokio::test]
async fn reports_crash_when_qemu_is_killed() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let mut rx = hv.subscribe();

    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Crashed);
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
}

fn signal(sig: &str, pid: u32) -> bool {
    std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn process_exists(pid: u32) -> bool {
    signal("-0", pid)
}

/// 테스트가 중간에 실패해도 멈춰 둔 QEMU가 남지 않게 drop에서 SIGKILL을 보낸다.
/// 이미 끝났으면 pid가 재사용됐을 수 있으므로 아무것도 하지 않는다.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if process_exists(self.0) {
            signal("-KILL", self.0);
        }
    }
}

/// 드라이버 A로 VM을 띄우고 A를 내린 뒤 QEMU를 SIGSTOP한다. 재시작한 opld가 멈춘 VM을 만나는 상황이다.
async fn start_then_stop_process(root: &Path, vm: &VmRuntimeSpec) -> (u32, KillOnDrop) {
    let pid = {
        let a = QemuHypervisor::new(config(root)).await.unwrap();
        a.start(vm, None).await.unwrap();
        a.inspect(vm.id).await.unwrap().unwrap().pid
    };
    let guard = KillOnDrop(pid);
    assert!(signal("-STOP", pid));
    (pid, guard)
}

fn unresponsive(id: Uuid, pid: u32) -> ActualState {
    ActualState {
        id,
        pid,
        status: RuntimeStatus::Other("unresponsive".into()),
    }
}

#[tokio::test]
async fn start_does_not_destroy_unresponsive_running_vm() {
    let tmp = short_tempdir();
    let vm = spec();
    let run_dir = tmp.path().join("run/vm").join(vm.id.to_string());
    let (pid, _guard) = start_then_stop_process(tmp.path(), &vm).await;

    // 멈춘 QEMU는 QMP에 답하지 않는다. 살아 있으므로 파일을 지우지 않고 등록하지 않지만,
    // 조회에는 pidfile 잠금 소유자(= QEMU) pid와 함께 드러난다.
    let b = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    assert!(run_dir.exists());
    assert_eq!(
        b.inspect(vm.id).await.unwrap(),
        Some(unresponsive(vm.id, pid))
    );

    // 새로 띄우거나 파일을 지우지 않고, 다시 시도할 수 있는 오류로 끝난다.
    let err = b.start(&vm, None).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert!(run_dir.join("qemu.pid").exists());
    assert!(run_dir.join("qmp.sock").exists());
    assert!(process_exists(pid));

    assert!(signal("-CONT", pid));
    b.start(&vm, None).await.unwrap();
    let st = b.inspect(vm.id).await.unwrap().expect("reattached");
    assert_eq!(st.pid, pid);
    assert_eq!(st.status, RuntimeStatus::Running);

    b.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(b.inspect(vm.id).await.unwrap(), None);
    assert!(!run_dir.exists());
}

#[tokio::test]
async fn force_stop_kills_unresponsive_qemu() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let _guard = KillOnDrop(pid);
    let mut rx = hv.subscribe();
    assert!(signal("-STOP", pid));
    // QMP에 답하지 않아도 살아 있는 VM으로 드러난다.
    assert_eq!(
        hv.inspect(vm.id).await.unwrap(),
        Some(unresponsive(vm.id, pid))
    );

    tokio::time::timeout(Duration::from_secs(30), hv.stop(vm.id, StopMode::Force))
        .await
        .expect("force stop took longer than 30s")
        .unwrap();
    // SHUTDOWN 이벤트 없이 죽였으므로 Crashed로 보고된다.
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Crashed);
    // launchd/init이 회수할 때까지 잠시 걸릴 수 있다.
    let mut gone = false;
    for _ in 0..100 {
        if !process_exists(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "QEMU {pid} is still alive");
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
}

#[tokio::test]
async fn force_stop_after_restart_kills_stopped_qemu() {
    let tmp = short_tempdir();
    let vm = spec();
    let run_dir = tmp.path().join("run/vm").join(vm.id.to_string());
    let (pid, _guard) = start_then_stop_process(tmp.path(), &vm).await;

    let b = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    assert_eq!(b.list().await.unwrap(), vec![unresponsive(vm.id, pid)]);
    // QMP 없이는 ACPI 요청을 보낼 수 없다.
    assert!(
        b.stop(vm.id, StopMode::Acpi)
            .await
            .unwrap_err()
            .is_transient()
    );

    tokio::time::timeout(Duration::from_secs(30), b.stop(vm.id, StopMode::Force))
        .await
        .expect("force stop took longer than 30s")
        .unwrap();
    let mut gone = false;
    for _ in 0..100 {
        if !process_exists(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "QEMU {pid} is still alive");
    assert!(!run_dir.exists());
    assert_eq!(b.inspect(vm.id).await.unwrap(), None);
    assert!(b.list().await.unwrap().is_empty());
}

/// `path`에 OS 없는 1GiB qcow2 디스크를 만들어 VM 스펙에 붙인다.
fn with_qcow2_disk(mut vm: VmRuntimeSpec, path: &Path) -> VmRuntimeSpec {
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(path)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
    vm.disks.push(RuntimeDisk {
        path: path.to_path_buf(),
        format: ImageFormat::Qcow2,
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    });
    vm
}

/// 디스크 이미지의 내부 스냅샷 이름과 가상 크기 (QEMU가 쓰는 중이어도 읽는다)
fn image_info(path: &Path) -> (Vec<String>, u64) {
    let out = std::process::Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let tags = info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    (tags, info["virtual-size"].as_u64().unwrap())
}

#[tokio::test]
async fn snapshots_of_running_vm_and_start_restoring_memory() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    hv.start(&vm, None).await.unwrap();

    // 실행 중 크기 늘리기
    hv.block_resize(vm.id, 0, 2 << 30).await.unwrap();
    assert_eq!(image_info(&disk).1, 2 << 30);

    // 메모리 스냅샷: 저장하는 동안 멈췄다가 다시 실행된다.
    hv.snapshot_save(vm.id, "mem", true).await.unwrap();
    let st = hv.inspect(vm.id).await.unwrap().unwrap();
    assert_eq!(st.status, RuntimeStatus::Running);
    // 디스크만 스냅샷
    hv.snapshot_save(vm.id, "disk", false).await.unwrap();
    assert_eq!(image_info(&disk).0, ["mem", "disk"]);
    hv.snapshot_delete(vm.id, "disk").await.unwrap();
    // 없는 태그를 지워도 성공한다.
    hv.snapshot_delete(vm.id, "disk").await.unwrap();
    assert_eq!(image_info(&disk).0, ["mem"]);

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    let mut rx = hv.subscribe();

    // 없는 태그를 불러오면 QEMU를 끝내고 다시 시도하지 않을 오류로 끝난다.
    let err = hv.start(&vm, Some("nope")).await.unwrap_err();
    assert!(
        matches!(
            err,
            opl_driver::DriverError::Qmp(opl_qemu::QmpError::JobFailed { .. })
        ),
        "{err:?}"
    );
    assert!(!err.is_transient());
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    assert!(matches!(
        next_event_for(&mut rx, vm.id).await,
        EventKind::Stopped { .. } | EventKind::Crashed
    ));
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);

    // 메모리 스냅샷을 불러오며 시작하면 실행 상태다.
    hv.start(&vm, Some("mem")).await.unwrap();
    let st = hv.inspect(vm.id).await.unwrap().unwrap();
    assert_eq!(st.status, RuntimeStatus::Running);
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

#[tokio::test]
async fn memory_snapshot_needs_a_disk() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let err = hv.snapshot_save(vm.id, "mem", true).await.unwrap_err();
    assert!(
        matches!(err, opl_driver::DriverError::InvalidSpec(_)),
        "{err:?}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돈다.
#[tokio::test]
async fn qemu_runs_in_its_own_systemd_scope() {
    if std::env::var_os("OPL_TEST_SYSTEMD").is_none() {
        return;
    }
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        cgroup.contains(&format!("opl-vm-{}.scope", vm.id)),
        "{cgroup}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    // 끝난 스코프가 아직 남아 있어도 시작 전에 지우므로 곧바로 다시 시작할 수 있다.
    hv.start(&vm, None).await.unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}
```

`crates/opl-peer/tests/grpc.rs`에서

```rust
    .unwrap();
    target.hv.start(&spec).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
```

을 다음으로 바꾼다:

```rust
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        };
        match self.hv.start(&spec).await {
            Ok(()) => Ok(true),
```

을 다음으로 바꾼다:

```rust
        };
        match self.hv.start(&spec, None).await {
            Ok(()) => Ok(true),
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        let spec = runtime_spec(&self.cluster, vm, self.config.node).map_err(|e| e.to_string())?;
        self.hv.start(&spec).await.map_err(|e| e.to_string())
    }
```

을 다음으로 바꾼다:

```rust
        let spec = runtime_spec(&self.cluster, vm, self.config.node).map_err(|e| e.to_string())?;
        self.hv.start(&spec, None).await.map_err(|e| e.to_string())
    }
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 261개, 실패 0

Run: `cargo test -p opl-driver --features qemu-it --test qemu_driver`
Expected: PASS 9개

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(driver): 실행 중 VM 스냅샷·크기 늘리기와 스냅샷을 불러오며 시작"
```

---

### Task 5: reconciler: 다음 시작에 한 번 불러올 스냅샷

`ReconcilerHandle::restore_on_start(id, tag)`·`clear_restore(id)`. 노드의 공유 맵에 맡긴 태그를 작업자가 VM을 새로 띄울 때 꺼내 `start`에 넘긴다(실패해도 버린다). 태그가 있던 시작이 실패하면 다시 시도하지 않고 VM을 `failed`로 둔다(다시 시도하면 메모리 없이 부팅된다). 메모리에만 있어 opld가 다시 시작하면 사라진다.

**Files:**
- Modify: `crates/opl-reconciler/src/reconciler.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/common/mod.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-reconciler/src/reconciler.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Hypervisor;
use opl_types::resource::Kind;
use opl_types::vm::VmSpec;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::ReconcilerConfig;
use crate::worker::{Trigger, Worker};

/// 한 노드의 조정 루프. `spawn`으로 시작하고 `shutdown`으로 멈춘다.
///
/// 멈춰도 VM은 건드리지 않는다. 다시 시작하면(하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙은 뒤)
/// 현재 실제 상태에서부터 이어서 맞춘다.
pub struct Reconciler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
    commands: mpsc::UnboundedSender<Uuid>,
    restores: Restores,
}

/// VM별로 다음 시작에 불러올 스냅샷 태그. 이 노드의 메모리에만 있어 opld가 재시작하면 사라진다.
pub(crate) type Restores = Arc<Mutex<HashMap<Uuid, String>>>;

/// 실행 중인 reconciler에 VM별 일회성 명령을 보낸다. 명령은 그 VM의 작업자 큐를 거치므로
/// 같은 VM에 대한 드라이버 호출이 겹치지 않는다.
#[derive(Clone)]
pub struct ReconcilerHandle {
    commands: mpsc::UnboundedSender<Uuid>,
    restores: Restores,
}

impl ReconcilerHandle {
    /// 원하는 상태가 stopped인 VM을 ACPI 응답을 기다리지 않고 곧바로 끈다. 원하는 상태를 먼저
    /// stopped로 기록한 뒤에 부른다. 이 노드에 배정되지 않은 VM이거나 원하는 상태가 running이면
    /// 아무것도 하지 않는다. 결과는 VM status로 확인한다.
    pub fn force_stop(&self, id: Uuid) {
        let _ = self.commands.send(id);
    }

    /// 이 노드에서 VM을 다음에 새로 띄울 때 한 번만 이 스냅샷 태그를 불러와 이어서 실행한다.
    /// 원하는 상태를 running으로 기록하기 전에 부른다. 시작이 실패해도 태그는 쓴 것으로 치고,
    /// 그 시작은 다시 시도하지 않는다(VM은 failed). 이미 맡긴 태그는 바꾼다.
    pub fn restore_on_start(&self, id: Uuid, tag: String) {
        self.restores.lock().unwrap().insert(id, tag);
    }

    /// 맡긴 태그를 쓰지 않고 버린다(시작하기 전에 되돌리기가 실패했을 때).
    pub fn clear_restore(&self, id: Uuid) {
        self.restores.lock().unwrap().remove(&id);
    }
}

impl Reconciler {
    pub fn spawn(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReconcilerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let (commands, command_rx) = mpsc::unbounded_channel();
        let restores = Restores::default();
        let dispatcher = Dispatcher {
            cluster,
            hv,
            config: Arc::new(config),
            restores: restores.clone(),
            workers: HashMap::new(),
            retired: HashMap::new(),
        };
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx, command_rx)),
            commands,
            restores,
        }
    }

    pub fn handle(&self) -> ReconcilerHandle {
        ReconcilerHandle {
            commands: self.commands.clone(),
            restores: self.restores.clone(),
        }
    }

    /// 진행 중인 단계가 끝나기를 기다린 뒤 돌아온다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Watch(#[from] WatchError),
}

struct WorkerHandle {
    tx: mpsc::UnboundedSender<Trigger>,
    task: JoinHandle<()>,
}

/// 저장소 변경, 하이퍼바이저 이벤트, 주기적 재동기화를 VM별 작업자에게 나눠 준다.
struct Dispatcher {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    restores: Restores,
    /// 이 노드에 배정된 VM의 작업자
    workers: HashMap<Uuid, WorkerHandle>,
    /// 내보냈지만 아직 끝나지 않았을 수 있는 작업자. 같은 VM이 다시 배정되면 새 작업자가 기다린다.
    retired: HashMap<Uuid, JoinHandle<()>>,
}

impl Dispatcher {
    async fn run(
        mut self,
        mut shutdown: oneshot::Receiver<()>,
        mut commands: mpsc::UnboundedReceiver<Uuid>,
    ) {
        // 목록을 읽기 전에 구독해야 그 사이에 난 이벤트를 놓치지 않는다.
        let mut events = self.hv.subscribe();
        'sync: loop {
            let mut watch = match self.full_sync().await {
                Ok(watch) => watch,
                Err(e) => {
                    tracing::warn!(error = %e, "full sync failed; will retry");
                    tokio::select! {
                        _ = &mut shutdown => break,
                        () = tokio::time::sleep(self.config.retry_interval) => continue,
                    }
                }
            };
            let resync = tokio::time::sleep(self.config.resync_interval);
            tokio::pin!(resync);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break 'sync,
                    () = &mut resync => continue 'sync,
                    ev = watch.next() => match ev {
                        Ok(WatchEvent::Put { kind: Kind::Vm, resource })
                        | Ok(WatchEvent::Deleted { kind: Kind::Vm, resource, .. }) => {
                            self.vm_changed(resource.id);
                        }
                        Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                        Ok(_) => {}
                    },
                    ev = events.recv() => match ev {
                        Ok(ev) => {
                            if !self.notify(ev.id, Trigger::Event(ev.kind)) {
                                tracing::debug!(vm = %ev.id, "event for a VM not assigned to this node");
                            }
                        }
                        // 이벤트를 놓쳤다. 실제 상태를 전부 다시 맞춘다.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue 'sync,
                        Err(broadcast::error::RecvError::Closed) => break 'sync,
                    },
                    Some(id) = commands.recv() => {
                        if !self.notify(id, Trigger::ForceStop) {
                            tracing::warn!(vm = %id, "force stop for a VM not assigned to this node");
                        }
                    }
                }
            }
        }
        let workers = self.workers.drain().map(|(_, w)| w.task);
        let tasks: Vec<_> = workers
            .chain(self.retired.drain().map(|(_, t)| t))
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// 목록을 다시 읽어 작업자를 맞추고, 그 revision부터 구독한다.
    async fn full_sync(&mut self) -> Result<Watch, SyncError> {
        self.retired.retain(|_, task| !task.is_finished());
        let (revision, vms) = self.cluster.list::<VmSpec>()?;
        let mine: HashSet<Uuid> = vms
            .iter()
            .filter(|vm| vm.spec.node == Some(self.config.node))
            .map(|vm| vm.id)
            .collect();
        let gone: Vec<Uuid> = self
            .workers
            .keys()
            .filter(|id| !mine.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.retire(id);
        }
        for &id in &mine {
            self.sync(id);
        }
        // 경고를 남기려는 것뿐이므로 실패해도 동기화는 이어 간다.
        match self.hv.list().await {
            Ok(states) => {
                for state in states {
                    if !mine.contains(&state.id) {
                        tracing::warn!(vm = %state.id, "VM is running on this node but not assigned to it; leaving it alone");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
        }
        Ok(self.cluster.watch(revision)?)
    }

    fn vm_changed(&mut self, id: Uuid) {
        match self.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => self.sync(id),
            Ok(_) => self.retire(id),
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                self.notify(id, Trigger::Sync);
            }
        }
    }

    fn notify(&self, id: Uuid, trigger: Trigger) -> bool {
        self.workers
            .get(&id)
            .is_some_and(|w| w.tx.send(trigger).is_ok())
    }

    fn sync(&mut self, id: Uuid) {
        if self.notify(id, Trigger::Sync) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker::new(
            id,
            self.cluster.clone(),
            self.hv.clone(),
            self.config.clone(),
            self.restores.clone(),
        );
        let previous = self.retired.remove(&id);
        let task = tokio::spawn(worker.run(previous, rx));
        let _ = tx.send(Trigger::Sync);
        self.workers.insert(id, WorkerHandle { tx, task });
    }

    /// 삭제되었거나 다른 노드로 배정된 VM의 작업자를 내보낸다. 작업자는 마지막으로 한 번 확인하고 끝난다.
    fn retire(&mut self, id: Uuid) {
        // 다시 배정되었을 때 묵은 태그로 시작하지 않게 한다.
        self.restores.lock().unwrap().remove(&id);
        if let Some(w) = self.workers.remove(&id) {
            let _ = w.tx.send(Trigger::Sync);
            self.retired.insert(id, w.task);
        }
    }
}
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
use crate::config::ReconcilerConfig;
use crate::resolve::{ResolveError, runtime_spec};
```

을 다음으로 바꾼다:

```rust
use crate::config::ReconcilerConfig;
use crate::reconciler::Restores;
use crate::resolve::{ResolveError, runtime_spec};
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
    config: Arc<ReconcilerConfig>,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
```

을 다음으로 바꾼다:

```rust
    config: Arc<ReconcilerConfig>,
    /// 다음 시작에 불러올 스냅샷 태그(`ReconcilerHandle::restore_on_start`)
    restores: Restores,
    /// 진행 중인 ACPI 종료 요청을 보낸 시각. 강제 종료로 넘어갈 시점을 정한다.
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        config: Arc<ReconcilerConfig>,
    ) -> Self {
```

을 다음으로 바꾼다:

```rust
        config: Arc<ReconcilerConfig>,
        restores: Restores,
    ) -> Self {
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            config,
            stop_requested: None,
```

을 다음으로 바꾼다:

```rust
            config,
            restores,
            stop_requested: None,
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            Err(e) => {
                self.power_off = Some(Some(e.to_string()));
```

을 다음으로 바꾼다:

```rust
            Err(e) => {
                self.restores.lock().unwrap().remove(&self.id);
                self.power_off = Some(Some(e.to_string()));
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        };
        match self.hv.start(&spec, None).await {
            Ok(()) => Ok(true),
            Err(e) if e.is_transient() => {
                self.record_failure(format!("failed to start: {e}"));
                Ok(false)
            }
            Err(e) => {
                self.power_off = Some(Some(format!("failed to start: {e}")));
```

을 다음으로 바꾼다:

```rust
        };
        // 실패해도 다시 쓰지 않는다.
        let restore = self.restores.lock().unwrap().remove(&self.id);
        match (self.hv.start(&spec, restore.as_deref()).await, restore) {
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
```

`crates/opl-reconciler/tests/common/mod.rs`에서

```rust

    /// Mock 하이퍼바이저에서 실행 중인 VM의 pid
```

을 다음으로 바꾼다:

```rust

    pub fn restore_on_start(&self, id: Uuid, tag: &str) {
        self.reconciler
            .as_ref()
            .unwrap()
            .handle()
            .restore_on_start(id, tag.into());
    }

    pub fn clear_restore(&self, id: Uuid) {
        self.reconciler.as_ref().unwrap().handle().clear_restore(id);
    }

    /// Mock 하이퍼바이저에서 실행 중인 VM의 pid
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    assert_eq!(env.get(vm.id).status, before.status);
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(env.get(vm.id).status, before.status);
}

/// 정지한 VM을 만들고 reconciler가 처리할 때까지 기다린다.
async fn stopped_vm(env: &Env) -> opl_types::vm::Vm {
    let mut vm = vm("web", env.node);
    vm.spec.power = PowerState::Stopped;
    env.create(&vm).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    vm
}

#[tokio::test]
async fn restore_tag_is_used_by_next_start_only() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, Some("snap-1".into()))]);

    // 한 번 쓰면 사라진다. 다음 시작은 처음부터 부팅한다.
    env.set_power(vm.id, PowerState::Stopped).await;
    env.wait(vm.id, "stopped", |v| v.status.phase == VmPhase::Stopped)
        .await;
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running again", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn cleared_restore_tag_is_not_used() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    env.restore_on_start(vm.id, "snap-1");
    env.clear_restore(vm.id);
    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.hv.launches(), vec![(vm.id, None)]);
}

#[tokio::test]
async fn failed_restore_marks_failed_and_consumes_tag() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 일시 오류라도 다시 시도하지 않는다. 태그 없이 다시 시작하면 처음부터 부팅하게 된다.
    env.hv.fail_next_start(DriverError::Timeout("qmp"));
    env.restore_on_start(vm.id, "snap-1");
    env.set_power(vm.id, PowerState::Running).await;
    let got = env
        .wait(vm.id, "failed", |v| v.status.phase == VmPhase::Failed)
        .await;
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(
        got.status.last_error.as_deref(),
        Some("failed to restore snapshot snap-1: timed out waiting for qmp")
    );
    assert_eq!(env.pid(vm.id).await, None);

    env.set_power(vm.id, PowerState::Running).await;
    env.wait(vm.id, "running", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 264개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(reconciler): 다음 시작에 한 번 불러올 스냅샷 태그"
```

---

### Task 6: API: 스냅샷과 볼륨 크기 늘리기

`GET /snapshots`(watch 목록), `GET/POST /vms/{key}/snapshots`, `GET/DELETE /vms/{key}/snapshots/{name}`, `POST /vms/{key}/snapshots/{name}/revert`, `POST /volumes/{key}/resize`. 만들기는 `pending` 스냅샷과 Task를 만들고 202(Task를 못 만들면 스냅샷을 지운다). 검사: 시작·정지 중 `vm_busy`, qcow2가 아닌 디스크 `snapshot_unsupported`, 메모리 기본은 실행 중이고 디스크가 있을 때, 정지 VM의 메모리 400, 설명 1024자, 준비 안 된 스냅샷 `snapshot_not_ready`, 되돌리기는 스냅샷의 VM 스펙을 VM 수정과 같은 검사에. 크기는 더 크고 512 배수여야 한다. VM 삭제 `vm_has_snapshots`, 볼륨 삭제 `volume_in_snapshot`, 이름 중복 `name_taken`. Task 노드는 `vm_placement`(spec.node → status.node → 첫 디스크 볼륨의 노드) 또는 API 노드. 권한은 VM Read/Operate, 크기는 Volume Write.

**Files:**
- Create: `crates/opl-api/src/snapshots.rs`
- Create: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/src/error.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/error.rs`에서

```rust
                    ),
                    Rejection::InUse { field, .. } => ApiError::conflict(
                        if field == "mac" {
                            "mac_in_use"
                        } else {
                            "volume_in_use"
                        },
```

을 다음으로 바꾼다:

```rust
                    ),
                    // 배타 값: VM의 MAC·볼륨, 스냅샷의 VM 안 이름
                    Rejection::InUse { field, .. } => ApiError::conflict(
                        match field.as_str() {
                            "mac" => "mac_in_use",
                            "name" => "name_taken",
                            _ => "volume_in_use",
                        },
```

`crates/opl-api/src/error.rs`에서

```rust
    #[test]
    fn internal_error_hides_details() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn snapshot_name_in_its_vm_is_taken() {
        let e = ApiError::from(ClusterError::Rejected(Rejection::InUse {
            kind: opl_types::resource::Kind::Snapshot,
            field: "name".into(),
            value: "0190/before".into(),
            holder: uuid::Uuid::nil(),
        }));
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(e.code, "name_taken");
    }

    #[test]
    fn internal_error_hides_details() {
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod scheduler;
mod state;
```

을 다음으로 바꾼다:

```rust
pub mod scheduler;
mod snapshots;
mod state;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(vms::vm_action))
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
        .routes(routes!(console::console))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(vms::vm_action))
        .routes(routes!(snapshots::list_snapshots))
        .routes(routes!(
            snapshots::list_vm_snapshots,
            snapshots::create_snapshot
        ))
        .routes(routes!(snapshots::get_snapshot, snapshots::delete_snapshot))
        .routes(routes!(snapshots::revert_snapshot))
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
        .routes(routes!(volumes::resize_volume))
        .routes(routes!(console::console))
```

`crates/opl-api/src/snapshots.rs`를 만든다:

```rust
//! VM 스냅샷 목록·조회·만들기·되돌리기·삭제. 권한은 VM을 따른다(보기는 Read, 나머지는 Operate).
//! 실제 작업은 VM이 있는 노드의 Task 실행기가 한다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::Cluster;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{VolumePhase, VolumeSpec};
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
use crate::vms::prepare;

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

/// VM의 디스크 작업을 할 노드: 다음에 돌 노드(`spec.node`), 지금(또는 마지막으로) 돈 노드
/// (`status.node`), 배치된 적이 없으면 첫 디스크 볼륨을 만든 노드. 꺼진 VM을 다른 노드로 옮겼으면
/// 그 디스크는 옮긴 노드에서 쓰인다. 셋 다 없으면 `None`.
pub(crate) fn vm_placement(cluster: &Cluster, vm: &Vm) -> Option<Uuid> {
    vm.spec.node.or(vm.status.node).or_else(|| {
        let disk = vm.spec.disks.first()?;
        let volume = cluster.get_by_name::<VolumeSpec>(&disk.volume).ok()??;
        volume.status.node
    })
}

/// 스냅샷 Task를 실행할 노드: [`vm_placement`], 없으면 요청을 받은 이 노드(설계 §3).
pub(crate) fn vm_node(state: &AppState, vm: &Vm) -> Uuid {
    vm_placement(&state.cluster, vm).unwrap_or(state.node)
}

/// 시작·정지 중인 VM은 스냅샷 작업을 받지 않는다.
fn check_not_busy(vm: &Vm) -> Result<(), ApiError> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(ApiError::conflict(
        "vm_busy",
        format!("VM {:?} is {doing}; try again when it has settled", vm.name),
    ))
}

/// 모든 디스크가 스냅샷을 담을 수 있고 다 만들어졌는지. dir 백엔드(`Storage::supports_snapshots`)는
/// qcow2만 담는다. raw가 하나라도 있으면 일부 디스크만 되돌아가므로 거부한다.
fn check_disks(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.spec.format != ImageFormat::Qcow2 {
            return Err(ApiError::conflict(
                "snapshot_unsupported",
                format!(
                    "VM {:?} has non-qcow2 disk {:?}; only qcow2 disks can be snapshotted",
                    vm.name, disk.volume
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
/// 돌려준다(202). 시작·정지 중이면 409 `vm_busy`, qcow2가 아닌 디스크가 있으면 409
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
    check_not_busy(&vm)?;
    check_disks(&state, &vm)?;
    let running = vm.status.phase == VmPhase::Running;
    let has_disk = !vm.spec.disks.is_empty();
    let memory = req.memory.unwrap_or(running && has_disk);
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
            name: req.name,
            parent: vm.status.snapshot,
            memory,
            description: req.description,
            scheduled: false,
            vm_spec: vm.spec.clone(),
        },
    );
    snapshot.name = snapshot.id.to_string();
    // 같은 VM에 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&snapshot).await?;
    let op = TaskOp::SnapshotCreate {
        snapshot: snapshot.id,
    };
    match create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
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
/// 스냅샷 스펙의 볼륨이 모두 있고 다 만들어졌으며 다른 VM이 쓰지 않아야 한다.
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
    check_not_busy(&vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
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
    check_not_busy(&vm)?;
    let snapshot = find_snapshot(&state, &vm, &name)?;
    check_ready(&snapshot)?;
    let op = TaskOp::SnapshotDelete {
        snapshot: snapshot.id,
    };
    let task = create_task(&state.cluster, op, vm.id, vm_node(&state, &vm)).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM의 스냅샷 수. VM 삭제를 막는 데 쓴다.
pub(crate) fn count_snapshots(state: &AppState, vm: Uuid) -> Result<usize, ApiError> {
    Ok(snapshots_of(state, vm)?.1.len())
}
```

`crates/opl-api/src/vms.rs`에서

```rust
use crate::scheduler::{Inventory, schedule};
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
use crate::scheduler::{Inventory, schedule};
use crate::snapshots::count_snapshots;
use crate::state::AppState;
```

`crates/opl-api/src/vms.rs`에서

```rust
/// `id`는 이 spec을 가질 VM이다(자기 자신의 MAC과 디스크는 충돌로 보지 않는다).
fn prepare(state: &AppState, id: Uuid, spec: &mut VmSpec) -> Result<(), ApiError> {
    let inv = Inventory::load(&state.cluster)?;
```

을 다음으로 바꾼다:

```rust
/// `id`는 이 spec을 가질 VM이다(자기 자신의 MAC과 디스크는 충돌로 보지 않는다).
pub(crate) fn prepare(state: &AppState, id: Uuid, spec: &mut VmSpec) -> Result<(), ApiError> {
    let inv = Inventory::load(&state.cluster)?;
```

`crates/opl-api/src/vms.rs`에서

```rust

/// 꺼진 VM만 지울 수 있다.
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
```

을 다음으로 바꾼다:

```rust

/// 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`.
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
```

`crates/opl-api/src/vms.rs`에서

```rust
        return Err(vm_running(&vm, "delete"));
    }
```

을 다음으로 바꾼다:

```rust
        return Err(vm_running(&vm, "delete"));
    }
    let snapshots = count_snapshots(&state, vm.id)?;
    if snapshots > 0 {
        return Err(ApiError::conflict(
            "vm_has_snapshots",
            format!(
                "VM {:?} has {snapshots} snapshot(s); delete them first",
                vm.name
            ),
        ));
    }
```

`crates/opl-api/src/volumes.rs`에서

```rust
use opl_types::runtime::ImageFormat;
use opl_types::storage::{
```

을 다음으로 바꾼다:

```rust
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotSpec;
use opl_types::storage::{
```

`crates/opl-api/src/volumes.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec};
use serde::Deserialize;
```

`crates/opl-api/src/volumes.rs`에서

```rust
}
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
}
use crate::snapshots::vm_node;
use crate::state::AppState;
```

`crates/opl-api/src/volumes.rs`에서

```rust
        nodes[0]
    }
}
```

을 다음으로 바꾼다:

```rust
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

`crates/opl-api/src/volumes.rs`에서

```rust
/// VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202). 아직 만들고 있는 볼륨은 지울 수 없다.
#[utoipa::path(delete, path = "/volumes/{key}", tag = "storage",
```

을 다음으로 바꾼다:

```rust
/// VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202). 아직 만들고 있는 볼륨은 지울 수 없다.
/// 스냅샷의 VM 스펙이 가리키면 409 `volume_in_snapshot`.
#[utoipa::path(delete, path = "/volumes/{key}", tag = "storage",
```

`crates/opl-api/src/volumes.rs`에서

```rust
    }
    let node = match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
    {
        Some(pool) => pool_node(&state, &pool),
        None => state.node,
    };
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

을 다음으로 바꾼다:

```rust
    }
    let (_, snapshots) = state.cluster.list::<SnapshotSpec>()?;
    if let Some(s) = snapshots
        .iter()
        .find(|s| s.spec.vm_spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(ApiError::conflict(
            "volume_in_snapshot",
            format!(
                "volume {:?} is used by snapshot {:?} of VM {}",
                volume.name, s.spec.name, s.spec.vm
            ),
        ));
    }
    let node = volume_node(&state, &volume)?;
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizeRequest {
    /// 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
    pub size_bytes: u64,
}

/// 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). 실행 중인 VM이 쓰면 그 VM의
/// 노드에서, 아니면 볼륨의 노드에서 한다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    request_body = ResizeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn resize_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<ResizeRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err(ApiError::conflict(
            "volume_not_ready",
            format!("volume {:?} is still being created", volume.name),
        ));
    }
    if req.size_bytes <= volume.spec.size_bytes {
        return Err(ApiError::bad_request(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        )));
    }
    // qcow2는 `qemu-img resize`도 QMP `block_resize`도 512 배수가 아니면 거부한다.
    if req.size_bytes % 512 != 0 {
        return Err(ApiError::bad_request(
            "size_bytes must be a multiple of 512",
        ));
    }
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let running = vms.iter().find(|vm| {
        !matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
            && vm.spec.disks.iter().any(|d| d.volume == volume.name)
    });
    let node = match running {
        Some(vm) => vm_node(&state, vm),
        None => volume_node(&state, &volume)?,
    };
    let op = TaskOp::VolumeResize {
        size_bytes: req.size_bytes,
    };
    let task = create_task(&state.cluster, op, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 6] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 7] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::Network,
];
```

을 다음으로 바꾼다:

```rust
    Kind::Network,
    Kind::Snapshot,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
        }
    }
}
```

을 다음으로 바꾼다:

```rust
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_watchable() {
        let kinds = parse_kinds("vm, snapshot").unwrap();
        assert!(kinds.contains(&Kind::Snapshot));
        assert!(parse_kinds("user").is_err());
    }
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/volumes/import",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/volumes/import",
        "/volumes/{key}/resize",
        "/snapshots",
        "/vms/{key}/snapshots",
        "/vms/{key}/snapshots/{name}",
        "/vms/{key}/snapshots/{name}/revert",
        "/watch",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_TaskSpec_TaskStatus"].is_object());
    let schemes = &doc["components"]["securitySchemes"];
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_TaskSpec_TaskStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_SnapshotSpec_SnapshotStatus"].is_object());
    let schemes = &doc["components"]["securitySchemes"];
```

`crates/opl-api/tests/snapshots.rs`를 만든다:

```rust
//! 스냅샷 API: 검사, 리소스와 Task 만들기, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기.
//! Task 실행은 아직 붙지 않았으므로 Task의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::vm::{VmPhase, VmSpec, VmStatus};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// 풀 `local`(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, name: &str, format: ImageFormat) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: MIB,
            format,
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

/// VM의 실제 상태를 직접 쓴다. `spec.node`가 없는 VM은 reconciler가 다루지 않아 그대로 남는다.
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
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

async fn snapshot_of(env: &Env, vm: Uuid, name: &str) -> Resource<SnapshotSpec, SnapshotStatus> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
        .unwrap_or_else(|| panic!("snapshot {name} not found"))
}

async fn mark_ready(env: &Env, id: Uuid) {
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
    };
    env.cluster
        .update_status::<SnapshotSpec>(id, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_records_pending_snapshot_and_task() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;

    let (status, task) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "before", "description": "first" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let snap = snapshot_of(&env, vm, "before").await;
    assert_eq!(task["spec"]["op"]["kind"], "snapshot-create");
    assert_eq!(task["spec"]["op"]["snapshot"], snap.id.to_string());
    assert_eq!(task["spec"]["target"], vm.to_string());
    // 배치되지 않은 VM이면 요청을 받은 노드에서 돈다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(snap.name, snap.id.to_string());
    assert_eq!(snap.status.phase, SnapshotPhase::Pending);
    assert_eq!(snap.spec.parent, None);
    assert_eq!(snap.spec.description, "first");
    // 꺼진 VM은 디스크만 담는다.
    assert!(!snap.spec.memory);
    assert!(!snap.spec.scheduled);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(snap.spec.vm_spec, current.spec);

    // 이름이나 id로 조회하고, VM별 목록과 전체 목록에 나온다.
    let (status, got) = admin.get("/vms/web/snapshots/before").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], snap.id.to_string());
    let (status, got) = admin.get(&format!("/vms/{vm}/snapshots/{}", snap.id)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["name"], "before");
    let (status, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().unwrap() > 0);
    let (status, all) = admin.get("/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["items"][0]["spec"]["name"], "before");
    let (status, body) = admin.get("/vms/web/snapshots/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 같은 VM 안에서 이름이 겹치면 409, 다른 VM이면 된다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    admin.post("/vms", vm_request("db")).await;
    let (status, body) = admin
        .post("/vms/db/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // 다음 스냅샷의 부모는 VM의 현재 위치다.
    let mut st = current.status.clone();
    st.snapshot = Some(snap.id);
    env.cluster.update_status::<VmSpec>(vm, &st).await.unwrap();
    admin
        .post("/vms/web/snapshots", json!({ "name": "after" }))
        .await;
    assert_eq!(
        snapshot_of(&env, vm, "after").await.spec.parent,
        Some(snap.id)
    );
}

#[tokio::test]
async fn create_validates_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let bad = |body: Value| {
        let admin = admin.clone();
        async move {
            let (status, body) = admin.post("/vms/web/snapshots", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["code"], "invalid_request");
        }
    };
    bad(json!({ "name": "Bad_Name" })).await;
    bad(json!({ "name": Uuid::now_v7() })).await;
    bad(json!({ "name": "a", "description": "x".repeat(1025) })).await;
    bad(json!({ "name": "a", "memroy": true })).await;
    // 꺼진 VM에는 메모리가 없다.
    bad(json!({ "name": "a", "memory": true })).await;
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "a", "description": "가".repeat(1024) }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = admin
        .post("/vms/missing/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn running_vm_snapshots_memory_by_default_on_its_node() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    let elsewhere = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Running, Some(elsewhere)).await;

    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "mem" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], elsewhere.to_string());
    assert!(snapshot_of(&env, vm, "mem").await.spec.memory);
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "disk", "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!snapshot_of(&env, vm, "disk").await.spec.memory);

    // 시작·정지 중인 VM은 기다려야 한다.
    for phase in [VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(elsewhere)).await;
        let (status, body) = admin
            .post("/vms/web/snapshots", json!({ "name": "busy" }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_busy");
    }
}

#[tokio::test]
async fn disks_must_be_qcow2_and_memory_needs_a_disk() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    volume(&env, "data", ImageFormat::Raw).await;
    vm_with_disks(&admin, "web", &["root", "data"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");

    // 디스크 없는 실행 중 VM: 메모리는 담을 곳이 없다. 기본은 설정만 담는다.
    let (_, created) = admin.post("/vms", vm_request("bare")).await;
    let bare: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    set_vm_status(&env, bare, VmPhase::Running, Some(env.node)).await;
    let (status, body) = admin
        .post(
            "/vms/bare/snapshots",
            json!({ "name": "a", "memory": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post("/vms/bare/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(!snapshot_of(&env, bare, "a").await.spec.memory);
}

#[tokio::test]
async fn revert_and_delete_need_a_ready_snapshot() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    let snap = snapshot_of(&env, vm, "s1").await;

    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");

    mark_ready(&env, snap.id).await;
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-revert", "snapshot": snap.id })
    );
    assert_eq!(task["spec"]["target"], vm.to_string());
    let (status, task) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-delete", "snapshot": snap.id })
    );
    // Task가 실행되기 전이므로 리소스는 남아 있다.
    assert_eq!(admin.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(
        admin.delete("/vms/web/snapshots/nope").await.0,
        StatusCode::NOT_FOUND
    );

    // 시작·정지 중에는 되돌리거나 지우지 않는다.
    set_vm_status(&env, vm, VmPhase::Stopping, Some(env.node)).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
}

#[tokio::test]
async fn revert_needs_the_snapshot_volumes() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    mark_ready(&env, snapshot_of(&env, vm, "s1").await.id).await;

    // 볼륨을 떼어 다른 VM에 붙이면 되돌릴 수 없다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    vm_with_disks(&admin, "other", &["root"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");

    // 아직 만들고 있는 볼륨은 쓸 수 없다.
    assert_eq!(admin.delete("/vms/other").await.0, StatusCode::NO_CONTENT);
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut st = root.status.clone();
    st.phase = VolumePhase::Pending;
    env.cluster
        .update_status::<VolumeSpec>(root.id, &st)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_not_ready");

    // 볼륨이 없어졌으면 되돌릴 수 없다.
    env.cluster
        .delete::<VolumeSpec>(root.id, None)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn snapshots_block_vm_and_volume_deletion() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;

    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_has_snapshots");

    // VM에서 떼어 낸 볼륨도 스냅샷이 가리키면 지울 수 없다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_snapshot");

    let snap = snapshot_of(&env, vm, "s1").await;
    env.cluster
        .delete::<SnapshotSpec>(snap.id, None)
        .await
        .unwrap();
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    assert_eq!(admin.delete("/volumes/root").await.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn viewers_read_snapshots_and_operators_manage_them() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    admin.post("/vms", vm_request("web")).await;

    let (status, body) = viewer
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = operator
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(viewer.get("/vms/web/snapshots").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/snapshots").await.0, StatusCode::OK);
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    mark_ready(&env, snapshot_of(&env, vm.id, "s1").await.id).await;
    for (status, _) in [
        viewer
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await,
        viewer.delete("/vms/web/snapshots/s1").await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        operator
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        operator.delete("/vms/web/snapshots/s1").await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn resize_only_grows_and_runs_where_the_volume_is_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    // 풀은 다른 노드에만 있다(파일 작업은 그 노드에서 한다).
    let pool_node = Uuid::now_v7();
    pool(&env, vec![pool_node]).await;
    let id = volume(&env, "data", ImageFormat::Raw).await;

    // 줄이기·같은 크기와 512 배수가 아닌 크기는 받지 않는다.
    for size in [0, MIB, 2 * MIB + 1] {
        let (status, body) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": size }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, _) = viewer
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, task) = operator
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "volume-resize", "size_bytes": 2 * MIB })
    );
    assert_eq!(task["spec"]["target"], id.to_string());
    assert_eq!(task["spec"]["node"], pool_node.to_string());

    // 실행 중인 VM이 쓰면 그 VM의 노드에서 한다.
    let vm = vm_with_disks(&admin, "web", &["data"]).await;
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());

    let (status, _) = admin
        .post("/volumes/missing/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tasks_run_where_the_vm_disks_will_be_used() {
    let env = Env::start().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());

    // 꺼진 VM을 A에서 B로 옮겼다. 디스크는 다음에 B에서 쓰이므로 B에서 한다.
    volume(&env, "root", ImageFormat::Qcow2).await;
    let moved = vm_with_disks(&admin, "moved", &["root"]).await;
    set_vm_status(&env, moved, VmPhase::Stopped, Some(a)).await;
    let vm = env.cluster.get::<VmSpec>(moved).unwrap().unwrap();
    let spec = VmSpec {
        node: Some(b),
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(moved, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/moved/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], b.to_string());

    // 배치된 적 없는 VM은 첫 디스크 볼륨이 있는 노드에서 한다.
    let data = volume(&env, "data", ImageFormat::Qcow2).await;
    let mut on_a = env.cluster.get::<VolumeSpec>(data).unwrap().unwrap();
    on_a.status.node = Some(a);
    env.cluster
        .update_status::<VolumeSpec>(data, &on_a.status)
        .await
        .unwrap();
    vm_with_disks(&admin, "new", &["data"]).await;
    let (status, task) = admin
        .post("/vms/new/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], a.to_string());
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 276개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 177개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 스냅샷 만들기·되돌리기·삭제와 볼륨 크기 늘리기 API"
```

---

### Task 7: 스냅샷 Task 실행

`opl-api/src/tasks/snapshot.rs`가 만들기(실행 중 메모리·디스크만·정지 분기, UEFI 변수 사본, 실행할 때 부모와 VM 스펙을 다시 정함), 되돌리기(다른 VM이 볼륨을 쓰면 끄기 전에 거절, 강제 정지, VM 스펙을 스냅샷 것으로, 디스크마다 전원 재확인 뒤 `-a`, 변수, 실제 크기 반영, 메모리면 그 뒤 전원 Task가 없을 때 `restore_on_start` 뒤 시작), 삭제(자식을 부모로, 현재 위치를 부모로)를 한다. 같은 VM의 Task는 차례로. 실패가 확인되면 태그를 지우고, 진행 중일 수 있으면 남긴다. 노드가 시작하면 이 노드가 맡은 `pending` 스냅샷을 정리한다. VM 상태 경합은 새 Raft 명령 `UpdateStatusIf`(revision이 같을 때만)로 막고 reconciler도 이것으로 쓴다. 끝나지 않은 만들기·되돌리기가 있으면 VM 동작과 수정은 409 `vm_busy`. Task 실행기는 하이퍼바이저·reconciler·`state_dir`을 받는다.

**Files:**
- Create: `crates/opl-api/src/tasks/snapshot.rs`
- Create: `crates/opl-api/tests/snapshot_disks.rs`
- Create: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/snapshots.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/types.rs`
- Modify: `crates/opl-cluster/tests/single_node.rs`
- Modify: `crates/opl-driver/src/error.rs`
- Modify: `crates/opl-driver/src/lib.rs`
- Modify: `crates/opl-driver/src/mock.rs`
- Modify: `crates/opl-driver/src/qemu.rs`
- Modify: `crates/opl-reconciler/src/worker.rs`
- Modify: `crates/opl-reconciler/tests/reconcile.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/Cargo.toml`에서

```toml
[dev-dependencies]
openraft.workspace = true
reqwest.workspace = true
```

을 다음으로 바꾼다:

```toml
[dev-dependencies]
async-trait.workspace = true
openraft.workspace = true
opl-qemu.workspace = true
reqwest.workspace = true
```

`crates/opl-api/src/snapshots.rs`에서

```rust
        format!("VM {:?} is {doing}; try again when it has settled", vm.name),
    ))
}
```

을 다음으로 바꾼다:

```rust
        format!("VM {:?} is {doing}; try again when it has settled", vm.name),
    ))
}

/// 이 VM의 스냅샷 만들기·되돌리기 Task가 끝나지 않았으면 409 `vm_busy`. 그 Task는 전원을 끄고
/// 켜며 디스크를 바꾸므로, 그사이 전원이나 스펙을 바꾸면 되돌리는 디스크를 QEMU가 열거나 담는
/// 스펙이 실제와 달라진다. Task는 실행기의 시간 제한 안에 끝난다.
pub(crate) fn check_no_snapshot_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::SnapshotCreate { .. } | TaskOp::SnapshotRevert { .. }
            )
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is being snapshotted or reverted; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

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
    // 대상(볼륨, 스냅샷 Task의 VM)별 마지막 Task. 같은 대상의 Task는 앞의 것이 끝난 뒤에 시작한다.
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

/// Task를 실행한다. 볼륨 Task는 같은 볼륨의 앞 Task가 끝난 뒤에 시작한다(`Storage`의 요구).
/// 스냅샷 Task도 같은 VM의 앞 스냅샷 Task가 끝난 뒤에 시작한다(부모·현재 위치가 차례로 바뀐다).
/// VM 전원 Task는 기다리기만 하므로 차례를 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
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
    let target = task.spec.target;
    let queued = is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op);
    let previous = if queued {
        by_target.remove(&target)
    } else {
        None
    };
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
    if queued {
        by_target.insert(target, handle);
    }
}

async fn execute(ctx: &Ctx, task: Task) {
    set_status(ctx, task.id, TaskState::Running, None).await;
    let result = match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task)).await {
        Ok(r) => r,
        Err(_) => Err(format!("timed out after {:?}", ctx.config.timeout)),
    };
    match result {
        Ok(()) => set_status(ctx, task.id, TaskState::Succeeded, None).await,
        Err(e) => fail(ctx, &task, e).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다.
async fn fail(ctx: &Ctx, task: &Task, error: String) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    cleanup(ctx, task).await;
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기의 흔적을 지워 이름을 다시 쓸 수 있게 한다.
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
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
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
        TaskOp::SnapshotDelete { snapshot } => snapshot::delete(ctx, id, *snapshot).await,
        // 실행은 뒤에서 붙인다.
        TaskOp::VolumeResize { .. } => Err("not implemented yet".into()),
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
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePoolSpec) {
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
        .create_volume(&pool.spec, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool.spec).await;
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
        .import_volume(&pool.spec, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool.spec).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await
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
            .delete_volume(&pool.spec, id, volume.spec.format)
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

`crates/opl-api/src/tasks/snapshot.rs`를 만든다:

```rust
//! 스냅샷 Task: 만들기·되돌리기·삭제(설계 §5). 같은 VM의 스냅샷 Task는 실행기가 차례로 돌린다.
//! API가 확인한 뒤 Task가 줄 서 기다리는 사이 VM이 바뀌었을 수 있으므로 여기서 다시 확인한다.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::{DriverError, uefi_vars_path, vm_state_dir};
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;

use super::{Ctx, set_power, set_volume_size, started, stopped, superseded, wait_vm};

fn get_snapshot(ctx: &Ctx, id: Uuid) -> Result<Option<Snapshot>, String> {
    ctx.cluster
        .get::<SnapshotSpec>(id)
        .map_err(|e| e.to_string())
}

fn get_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
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
fn ready_disks(ctx: &Ctx, spec: &VmSpec) -> Result<Vec<(Volume, StoragePool)>, String> {
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
fn check_settled(vm: &Vm) -> Result<(), String> {
    let doing = match vm.status.phase {
        VmPhase::Starting => "starting",
        VmPhase::Stopping => "stopping",
        _ => return Ok(()),
    };
    Err(format!("VM is {doing}; try again when it has settled"))
}

/// 스냅샷의 UEFI 변수 사본 `{state_dir}/vm/{vm}/snapshots/{snapshot}.vars`
fn vars_copy(ctx: &Ctx, vm: Uuid, snapshot: Uuid) -> Option<PathBuf> {
    let dir = ctx.config.state_dir.as_ref()?;
    Some(
        vm_state_dir(dir, vm)
            .join("snapshots")
            .join(format!("{snapshot}.vars")),
    )
}

/// `from`을 `to`로 복사한다. 디렉터리가 없으면 만든다.
async fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::copy(from, to).await.map(|_| ())
}

/// 디스크의 태그를 지운다. 실행 중이면 QEMU가 디스크를 쥐고 있으므로 하이퍼바이저로, 아니면
/// `spec`의 디스크마다 스토리지로 지운다. 태그나 볼륨이 없는 디스크는 그냥 넘어가고, 한 디스크가
/// 실패해도 나머지를 지운 뒤 첫 오류를 돌려준다. 다른 노드에 있는 볼륨은 이 노드에서 파일이 보이지
/// 않으므로(없는 것으로 보면 태그가 남는다) 오류다.
async fn remove_tags(
    ctx: &Ctx,
    vm: Uuid,
    running: bool,
    spec: &VmSpec,
    tag: &str,
) -> Result<(), String> {
    if running {
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
        if let Some(node) = volume.status.node
            && node != ctx.config.node
            && !pool.spec.shared
        {
            if result.is_ok() {
                result = Err(format!("volume {:?} is on node {node}", volume.name));
            }
            continue;
        }
        match ctx
            .storage
            .snapshot_delete(&pool.spec, volume.id, volume.spec.format, tag)
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
/// 디스크마다 스토리지로 태그를 만들고, UEFI 변수를 복사한 뒤 `ready`로 바꾸고 VM의 현재 위치로
/// 삼는다. 실패를 확인했으면 만든 태그를 지운다(리소스는 실행기의 정리가 지운다). 응답을 기다리다
/// 시간이 다 됐으면 작업이 아직 돌 수 있으므로 태그를 두고 실패한다.
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
        .find(|(v, _)| !ctx.storage.supports_snapshots(v.spec.format))
    {
        return Err(format!(
            "volume {:?} ({:?}) cannot be snapshotted",
            volume.name, volume.spec.format
        ));
    }
    if snapshot.spec.memory && disks.is_empty() {
        return Err("VM has no disk to hold the memory state".into());
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
    let made = if !running {
        let mut result = Ok(());
        for (volume, pool) in &disks {
            result = ctx
                .storage
                .snapshot_create(&pool.spec, volume.id, volume.spec.format, &tag)
                .await;
            if result.is_err() {
                break;
            }
        }
        result
    } else if disks.is_empty() {
        // 디스크 없는 VM은 설정만 담는다.
        Ok(())
    } else {
        ctx.hv.snapshot_save(vm_id, &tag, spec.memory).await
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
        (Err(e), _) if e.may_be_in_progress() => {
            return Err(format!(
                "{e}; the snapshot may still be written to the disks and its tag {tag} is left there"
            ));
        }
        (Err(e), _) => Some(e.to_string()),
        (Ok(()), Err(e)) => Some(format!("cannot copy UEFI variables: {e}")),
        (Ok(()), Ok(())) => None,
    };
    if let Some(error) = failure {
        if let Err(e) = remove_tags(ctx, vm_id, running, &vm.spec, &tag).await {
            tracing::warn!(snapshot = %id, error = %e, "cannot remove tags of failed snapshot");
        }
        if let Some(copy) = vars_copy(ctx, vm_id, id) {
            let _ = tokio::fs::remove_file(copy).await;
        }
        return Err(error);
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
    set_position(ctx, vm_id, |_| Some(id)).await
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
    // 되돌릴 수 없는데 VM만 꺼지지 않도록 끄기 전에 볼륨을 확인한다. API가 확인한 뒤 다른 VM이
    // 볼륨을 붙였을 수 있다.
    let disks = ready_disks(ctx, &snapshot.spec.vm_spec)?;
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
            .snapshot_revert(&pool.spec, volume.id, volume.spec.format, &tag)
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

fn is_down(vm: &Vm) -> bool {
    matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

/// VM 스펙을 스냅샷의 것으로 바꾼다. 노드는 지금 것을 두되, 메모리 스냅샷이면 이 노드에서 디스크를
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
        if snapshot.spec.memory && spec.node.is_none() {
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
        .volume_info(&pool.spec, volume.id, volume.spec.format)
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
```

`crates/opl-api/src/vms.rs`에서

```rust
use crate::scheduler::{Inventory, schedule};
use crate::snapshots::count_snapshots;
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
use crate::scheduler::{Inventory, schedule};
use crate::snapshots::{check_no_snapshot_task, count_snapshots};
use crate::state::AppState;
```

`crates/opl-api/src/vms.rs`에서

```rust
        let current = lookup::<VmSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
```

을 다음으로 바꾼다:

```rust
        let current = lookup::<VmSpec>(&state.cluster, &key)?;
        check_no_snapshot_task(&state, &current)?;
        if let Some(expected) = expected
```

`crates/opl-api/src/vms.rs`에서

```rust
    }
    let id = lookup::<VmSpec>(&state.cluster, &key)?.id;
    let (op, vm) = match action.as_str() {
```

을 다음으로 바꾼다:

```rust
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 강제 정지도 막는다. 되돌리기가 디스크를 바꾸는 중에 끄고 켜는 것이 섞이면 안 되고, 만들기
    // 중에 끄면 저장이 끊긴다. 멈춘 스냅샷 Task도 실행기의 시간 제한으로 끝난다.
    check_no_snapshot_task(&state, &vm)?;
    let id = vm.id;
    let (op, vm) = match action.as_str() {
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
use opl_types::storage::{SourceFormat, StoragePoolSpec};
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
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    server: tokio::task::JoinHandle<()>,
}

pub fn reconciler_config(node: Uuid) -> ReconcilerConfig {
    ReconcilerConfig {
        node,
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
        };
        cluster.create(&me).await.unwrap();
        let admin = Resource::new(
            "admin",
            UserSpec {
                password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
                role: Role::Admin,
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
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
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
        let app = opl_api::router(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Env {
            cluster,
            hv,
            node,
            base,
            dir,
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
        self.start_runner_with(Arc::new(DirStorage::new("qemu-img")))
            .await;
    }

    /// `storage`를 쓰는 Task 실행기를 띄운다.
    pub async fn start_runner_with(&mut self, storage: Arc<dyn Storage>) {
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                storage,
                self.hv.clone(),
                self.reconciler.as_ref().unwrap().handle(),
                runner_config(self.node, self.dir.path()),
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
        _: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.set_size(id, size_bytes);
        Ok(PathBuf::new())
    }
    async fn import_volume(
        &self,
        _: &StoragePoolSpec,
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
        _: &StoragePoolSpec,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }
    async fn volume_info(
        &self,
        _: &StoragePoolSpec,
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
    async fn capacity(&self, _: &StoragePoolSpec) -> Result<Capacity, DriverError> {
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
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("create", id).await;
        Ok(())
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("revert", id).await;
        Ok(())
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("delete", id).await;
        Ok(())
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePoolSpec,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(vec![])
    }
    async fn resize_volume(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.record("resize", id).await;
        self.set_size(id, size_bytes);
        Ok(())
    }
}
```

`crates/opl-api/tests/snapshot_disks.rs`를 만든다:

```rust
//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
#![cfg(feature = "storage-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Client, Env, vm_request};
use opl_driver::uefi_vars_path;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotSpec};
use opl_types::storage::{PoolBackend, StoragePoolSpec, Volume, VolumePhase, VolumeSpec};
use opl_types::vm::{PowerState, VmPhase, VmSpec};
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
fn qemu_io(path: &Path, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 파일에 있는 스냅샷 태그
fn tags(path: &Path) -> Vec<String> {
    let info: Value =
        serde_json::from_str(&qemu_img(&["info", "-U", "--output=json", path_str(path)])).unwrap();
    info["snapshots"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn pool_dir(env: &Env) -> PathBuf {
    env.dir.path().join("pool")
}

/// 풀 `local`을 저장소에 직접 만든다.
async fn create_pool(env: &Env) {
    std::fs::create_dir_all(pool_dir(env)).unwrap();
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: pool_dir(env),
            },
            nodes: vec![],
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 4 MiB qcow2 볼륨을 API로 만들고 파일 경로를 돌려준다.
async fn create_volume(env: &Env, admin: &Client, name: &str) -> PathBuf {
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": name,
                "spec": { "pool": "local", "size_bytes": 4 * MIB, "format": "qcow2" }
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
    pool_dir(env).join(format!("{}.qcow2", volume.id))
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

fn get(env: &Env, vm: Uuid, name: &str) -> Option<Snapshot> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
}

fn vm(env: &Env, id: Uuid) -> opl_types::vm::Vm {
    env.cluster.get::<VmSpec>(id).unwrap().unwrap()
}

#[tokio::test]
async fn stopped_vm_snapshot_reverts_disk_size_and_uefi_vars() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    let vars = uefi_vars_path(&env.dir.path().join("state"), id);
    std::fs::create_dir_all(vars.parent().unwrap()).unwrap();
    std::fs::write(&vars, b"vars-1").unwrap();
    assert!(qemu_io(&file, "write -P 0xaa 0 64k"));

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "s1" }))
            .await,
    )
    .await;
    succeeded(&task);
    let s1 = get(&env, id, "s1").unwrap();
    assert!(!s1.spec.memory);
    assert_eq!(tags(&file), vec![s1.id.to_string()]);
    assert_eq!(vm(&env, id).status.snapshot, Some(s1.id));

    // 스냅샷 뒤에 내용, 크기, 변수, 설정이 바뀐다.
    assert!(qemu_io(&file, "write -P 0xbb 0 64k"));
    qemu_img(&["resize", "-q", path_str(&file), "8M"]);
    std::fs::write(&vars, b"vars-2").unwrap();
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["memory_mib"] = json!(1024);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await,
    )
    .await;
    succeeded(&task);
    assert!(qemu_io(&file, "read -P 0xaa 0 64k"));
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    assert_eq!(volume.spec.size_bytes, 4 * MIB);
    assert_eq!(std::fs::read(&vars).unwrap(), b"vars-1");
    let got = vm(&env, id);
    assert_eq!(got.spec.memory_mib, 256);
    assert_eq!(got.spec.power, PowerState::Stopped);
    assert_eq!(got.status.snapshot, Some(s1.id));

    let task = wait(&admin, admin.delete("/vms/web/snapshots/s1").await).await;
    succeeded(&task);
    assert!(tags(&file).is_empty());
    assert!(get(&env, id, "s1").is_none());
    assert_eq!(vm(&env, id).status.snapshot, None);
    let copy = vars
        .parent()
        .unwrap()
        .join(format!("snapshots/{}.vars", s1.id));
    assert!(!copy.exists());
}

#[tokio::test]
async fn memory_snapshot_revert_starts_from_the_saved_state() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "mem" }))
            .await,
    )
    .await;
    succeeded(&task);
    let mem = get(&env, id, "mem").unwrap();
    assert!(mem.spec.memory);
    // Mock 하이퍼바이저는 파일에 쓰지 않으므로 QEMU가 만들었을 태그를 대신 만든다.
    qemu_img(&["snapshot", "-c", &mem.id.to_string(), path_str(&file)]);

    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["cpu"]["cores"] = json!(2);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;
    let pid = env.pid(id).await.unwrap();

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots/mem/revert", Value::Null)
            .await,
    )
    .await;
    succeeded(&task);
    // 강제로 끈 뒤 스냅샷의 스펙으로, 그 메모리를 불러오며 다시 시작했다.
    let got = vm(&env, id);
    assert_eq!(got.spec.cpu.cores, 1);
    assert_eq!(got.spec.power, PowerState::Running);
    assert_eq!(got.status.phase, VmPhase::Running);
    assert_eq!(got.status.snapshot, Some(mem.id));
    assert_ne!(env.pid(id).await, Some(pid));
    assert_eq!(
        env.hv.launches().last().unwrap(),
        &(id, Some(mem.id.to_string()))
    );
}

#[tokio::test]
async fn failed_disk_snapshot_removes_tags_already_made() {
    let env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let root = create_volume(&env, &admin, "root").await;
    let data = create_volume(&env, &admin, "data").await;
    let id = create_vm(&admin, "web", &["root", "data"]).await;
    // 두 번째 디스크 파일이 없어 그 태그를 만들지 못한다.
    std::fs::remove_file(&data).unwrap();

    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "s1" }))
            .await,
    )
    .await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(tags(&root).is_empty());
    assert!(get(&env, id, "s1").is_none());
    assert_eq!(vm(&env, id).status.snapshot, None);
}

#[tokio::test]
async fn unfinished_snapshot_tags_are_removed_at_startup() {
    let dir = tempfile::TempDir::new().unwrap();
    let pool_path = dir.path().to_path_buf();
    let file = std::sync::Arc::new(std::sync::Mutex::new(PathBuf::new()));
    let slot = file.clone();
    let env = Env::start_with(
        |_| {},
        move |cluster| async move {
            let me = cluster.id().0;
            let pool = Resource::new(
                "local",
                StoragePoolSpec {
                    backend: PoolBackend::Dir {
                        path: pool_path.clone(),
                    },
                    nodes: vec![],
                    shared: false,
                },
            );
            cluster.create(&pool).await.unwrap();
            let mut volume: Volume = Resource::new(
                "root",
                VolumeSpec {
                    pool: "local".into(),
                    size_bytes: MIB,
                    format: ImageFormat::Qcow2,
                },
            );
            volume.status.phase = VolumePhase::Ready;
            cluster.create(&volume).await.unwrap();
            let path = pool_path.join(format!("{}.qcow2", volume.id));
            qemu_img(&["create", "-q", "-f", "qcow2", path_str(&path), "1M"]);

            let mut spec: VmSpec =
                serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
            spec.disks =
                serde_json::from_value(json!([{ "volume": "root", "bus": "virtio-blk" }])).unwrap();
            spec.node = Some(me);
            let vm = Resource::new("web", spec.clone());
            cluster.create(&vm).await.unwrap();
            // 정지한 VM의 태그를 만든 뒤, 끝내기 전에 opld가 죽었다(Task는 실패로 끝났다).
            let mut s: Snapshot = Resource::new(
                "",
                SnapshotSpec {
                    vm: vm.id,
                    name: "half".into(),
                    parent: None,
                    memory: false,
                    description: String::new(),
                    scheduled: false,
                    vm_spec: spec,
                },
            );
            s.name = s.id.to_string();
            cluster.create(&s).await.unwrap();
            qemu_img(&["snapshot", "-c", &s.id.to_string(), path_str(&path)]);
            *slot.lock().unwrap() = path;
        },
    )
    .await;
    let file = file.lock().unwrap().clone();
    assert!(tags(&file).is_empty());
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    assert!(items.is_empty());
}
```

`crates/opl-api/tests/snapshot_tasks.rs`를 만든다:

```rust
//! 스냅샷 Task 실행: 실행 중인 VM(Mock 하이퍼바이저)의 만들기·삭제, 부모와 현재 위치, 실패 정리,
//! 디스크 없는 VM 되돌리기, 시작할 때의 고아 정리. 디스크 파일을 다루는 경우는 `snapshot_disks`.

mod common;

use std::sync::{Arc, Mutex};

use common::{Client, Env, FakeStorage, vm_request};
use opl_driver::{DriverError, uefi_vars_path};
use opl_qemu::QmpError;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{Snapshot, SnapshotPhase, SnapshotSpec};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, VmPhase, VmSpec};
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
```

`crates/opl-api/tests/snapshots.rs` 전체:

```rust
//! 스냅샷 API: 검사, 리소스와 Task 만들기, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기.
//! Task 실행기를 멈춰 두고 API가 남긴 리소스와 Task의 모양만 확인한다(실행은 `snapshot_tasks`).

mod common;

use common::{Client, Env, vm_request};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec, SnapshotStatus};
use opl_types::storage::{StoragePoolSpec, VolumePhase, VolumeSpec};
use opl_types::task::{TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec, VmStatus};
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

/// 남은 Task를 끝난 것으로 기록한다. 끝나지 않은 스냅샷 만들기 Task는 VM 수정을 막는다.
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

/// 풀 `local`(노드 `nodes`)을 저장소에 직접 만든다.
async fn pool(env: &Env, nodes: Vec<Uuid>) {
    let pool = Resource::new(
        "local",
        StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: env.dir.path().join("pool"),
            },
            nodes,
            shared: false,
        },
    );
    env.cluster.create(&pool).await.unwrap();
}

/// 바로 Ready인 볼륨을 저장소에 직접 만든다(파일은 만들지 않는다).
async fn volume(env: &Env, name: &str, format: ImageFormat) -> Uuid {
    let mut volume = Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: MIB,
            format,
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

/// VM의 실제 상태를 직접 쓴다. `spec.node`가 없는 VM은 reconciler가 다루지 않아 그대로 남는다.
async fn set_vm_status(env: &Env, id: Uuid, phase: VmPhase, node: Option<Uuid>) {
    let vm = env.cluster.get::<VmSpec>(id).unwrap().unwrap();
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

async fn snapshot_of(env: &Env, vm: Uuid, name: &str) -> Resource<SnapshotSpec, SnapshotStatus> {
    let (_, items) = env.cluster.list::<SnapshotSpec>().unwrap();
    items
        .into_iter()
        .find(|s| s.spec.vm == vm && s.spec.name == name)
        .unwrap_or_else(|| panic!("snapshot {name} not found"))
}

async fn mark_ready(env: &Env, id: Uuid) {
    let status = SnapshotStatus {
        phase: SnapshotPhase::Ready,
        created_at: Some(1),
        vmstate_bytes: None,
    };
    env.cluster
        .update_status::<SnapshotSpec>(id, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn create_records_pending_snapshot_and_task() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;

    let (status, task) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "before", "description": "first" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let snap = snapshot_of(&env, vm, "before").await;
    assert_eq!(task["spec"]["op"]["kind"], "snapshot-create");
    assert_eq!(task["spec"]["op"]["snapshot"], snap.id.to_string());
    assert_eq!(task["spec"]["target"], vm.to_string());
    // 배치되지 않은 VM이면 요청을 받은 노드에서 돈다.
    assert_eq!(task["spec"]["node"], env.node.to_string());
    assert_eq!(snap.name, snap.id.to_string());
    assert_eq!(snap.status.phase, SnapshotPhase::Pending);
    assert_eq!(snap.spec.parent, None);
    assert_eq!(snap.spec.description, "first");
    // 꺼진 VM은 디스크만 담는다.
    assert!(!snap.spec.memory);
    assert!(!snap.spec.scheduled);
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    assert_eq!(snap.spec.vm_spec, current.spec);

    // 이름이나 id로 조회하고, VM별 목록과 전체 목록에 나온다.
    let (status, got) = admin.get("/vms/web/snapshots/before").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], snap.id.to_string());
    let (status, got) = admin.get(&format!("/vms/{vm}/snapshots/{}", snap.id)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["name"], "before");
    let (status, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().unwrap() > 0);
    let (status, all) = admin.get("/snapshots").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["items"][0]["spec"]["name"], "before");
    let (status, body) = admin.get("/vms/web/snapshots/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 같은 VM 안에서 이름이 겹치면 409, 다른 VM이면 된다.
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "name_taken");
    admin.post("/vms", vm_request("db")).await;
    let (status, body) = admin
        .post("/vms/db/snapshots", json!({ "name": "before" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, list) = admin.get("/vms/web/snapshots").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // 다음 스냅샷의 부모는 VM의 현재 위치다.
    let mut st = current.status.clone();
    st.snapshot = Some(snap.id);
    env.cluster.update_status::<VmSpec>(vm, &st).await.unwrap();
    admin
        .post("/vms/web/snapshots", json!({ "name": "after" }))
        .await;
    assert_eq!(
        snapshot_of(&env, vm, "after").await.spec.parent,
        Some(snap.id)
    );
}

#[tokio::test]
async fn create_validates_request() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    vm_with_disks(&admin, "web", &["root"]).await;
    let bad = |body: Value| {
        let admin = admin.clone();
        async move {
            let (status, body) = admin.post("/vms/web/snapshots", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["code"], "invalid_request");
        }
    };
    bad(json!({ "name": "Bad_Name" })).await;
    bad(json!({ "name": Uuid::now_v7() })).await;
    bad(json!({ "name": "a", "description": "x".repeat(1025) })).await;
    bad(json!({ "name": "a", "memroy": true })).await;
    // 꺼진 VM에는 메모리가 없다.
    bad(json!({ "name": "a", "memory": true })).await;
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "a", "description": "가".repeat(1024) }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _) = admin
        .post("/vms/missing/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn running_vm_snapshots_memory_by_default_on_its_node() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    let elsewhere = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Running, Some(elsewhere)).await;

    let (status, task) = admin
        .post("/vms/web/snapshots", json!({ "name": "mem" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], elsewhere.to_string());
    assert!(snapshot_of(&env, vm, "mem").await.spec.memory);
    let (status, _) = admin
        .post(
            "/vms/web/snapshots",
            json!({ "name": "disk", "memory": false }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(!snapshot_of(&env, vm, "disk").await.spec.memory);

    // 시작·정지 중인 VM은 기다려야 한다.
    for phase in [VmPhase::Starting, VmPhase::Stopping] {
        set_vm_status(&env, vm, phase, Some(elsewhere)).await;
        let (status, body) = admin
            .post("/vms/web/snapshots", json!({ "name": "busy" }))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "vm_busy");
    }
}

#[tokio::test]
async fn disks_must_be_qcow2_and_memory_needs_a_disk() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    volume(&env, "data", ImageFormat::Raw).await;
    vm_with_disks(&admin, "web", &["root", "data"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_unsupported");

    // 디스크 없는 실행 중 VM: 메모리는 담을 곳이 없다. 기본은 설정만 담는다.
    let (_, created) = admin.post("/vms", vm_request("bare")).await;
    let bare: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    set_vm_status(&env, bare, VmPhase::Running, Some(env.node)).await;
    let (status, body) = admin
        .post(
            "/vms/bare/snapshots",
            json!({ "name": "a", "memory": true }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = admin
        .post("/vms/bare/snapshots", json!({ "name": "a" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(!snapshot_of(&env, bare, "a").await.spec.memory);
}

#[tokio::test]
async fn revert_and_delete_need_a_ready_snapshot() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    let snap = snapshot_of(&env, vm, "s1").await;

    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "snapshot_not_ready");

    mark_ready(&env, snap.id).await;
    let (status, task) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-revert", "snapshot": snap.id })
    );
    assert_eq!(task["spec"]["target"], vm.to_string());
    let (status, task) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "snapshot-delete", "snapshot": snap.id })
    );
    // Task가 실행되기 전이므로 리소스는 남아 있다.
    assert_eq!(admin.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(
        admin.delete("/vms/web/snapshots/nope").await.0,
        StatusCode::NOT_FOUND
    );

    // 시작·정지 중에는 되돌리거나 지우지 않는다.
    set_vm_status(&env, vm, VmPhase::Stopping, Some(env.node)).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
    let (status, body) = admin.delete("/vms/web/snapshots/s1").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_busy");
}

#[tokio::test]
async fn revert_needs_the_snapshot_volumes() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    mark_ready(&env, snapshot_of(&env, vm, "s1").await.id).await;
    finish_tasks(&env).await;

    // 볼륨을 떼어 다른 VM에 붙이면 되돌릴 수 없다.
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    vm_with_disks(&admin, "other", &["root"]).await;
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_use");

    // 아직 만들고 있는 볼륨은 쓸 수 없다.
    assert_eq!(admin.delete("/vms/other").await.0, StatusCode::NO_CONTENT);
    let root = env
        .cluster
        .get_by_name::<VolumeSpec>("root")
        .unwrap()
        .unwrap();
    let mut st = root.status.clone();
    st.phase = VolumePhase::Pending;
    env.cluster
        .update_status::<VolumeSpec>(root.id, &st)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_not_ready");

    // 볼륨이 없어졌으면 되돌릴 수 없다.
    env.cluster
        .delete::<VolumeSpec>(root.id, None)
        .await
        .unwrap();
    let (status, body) = admin
        .post("/vms/web/snapshots/s1/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn snapshots_block_vm_and_volume_deletion() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    volume(&env, "root", ImageFormat::Qcow2).await;
    let vm = vm_with_disks(&admin, "web", &["root"]).await;
    admin
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;

    let (status, body) = admin.delete("/vms/web").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "vm_has_snapshots");

    // VM에서 떼어 낸 볼륨도 스냅샷이 가리키면 지울 수 없다.
    finish_tasks(&env).await;
    let (_, current) = admin.get("/vms/web").await;
    let mut spec = current["spec"].clone();
    spec["disks"] = json!([]);
    admin.put("/vms/web", json!({ "spec": spec }), None).await;
    let (status, body) = admin.delete("/volumes/root").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "volume_in_snapshot");

    let snap = snapshot_of(&env, vm, "s1").await;
    env.cluster
        .delete::<SnapshotSpec>(snap.id, None)
        .await
        .unwrap();
    assert_eq!(admin.delete("/vms/web").await.0, StatusCode::NO_CONTENT);
    assert_eq!(admin.delete("/volumes/root").await.0, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn viewers_read_snapshots_and_operators_manage_them() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    admin.post("/vms", vm_request("web")).await;

    let (status, body) = viewer
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = operator
        .post("/vms/web/snapshots", json!({ "name": "s1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(viewer.get("/vms/web/snapshots").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/vms/web/snapshots/s1").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/snapshots").await.0, StatusCode::OK);
    let vm = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    mark_ready(&env, snapshot_of(&env, vm.id, "s1").await.id).await;
    for (status, _) in [
        viewer
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await,
        viewer.delete("/vms/web/snapshots/s1").await,
    ] {
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert_eq!(
        operator
            .post("/vms/web/snapshots/s1/revert", Value::Null)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        operator.delete("/vms/web/snapshots/s1").await.0,
        StatusCode::ACCEPTED
    );
}

#[tokio::test]
async fn resize_only_grows_and_runs_where_the_volume_is_used() {
    let env = env().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    // 풀은 다른 노드에만 있다(파일 작업은 그 노드에서 한다).
    let pool_node = Uuid::now_v7();
    pool(&env, vec![pool_node]).await;
    let id = volume(&env, "data", ImageFormat::Raw).await;

    // 줄이기·같은 크기와 512 배수가 아닌 크기는 받지 않는다.
    for size in [0, MIB, 2 * MIB + 1] {
        let (status, body) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": size }))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_request");
    }
    let (status, _) = viewer
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, task) = operator
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(
        task["spec"]["op"],
        json!({ "kind": "volume-resize", "size_bytes": 2 * MIB })
    );
    assert_eq!(task["spec"]["target"], id.to_string());
    assert_eq!(task["spec"]["node"], pool_node.to_string());

    // 실행 중인 VM이 쓰면 그 VM의 노드에서 한다.
    let vm = vm_with_disks(&admin, "web", &["data"]).await;
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
    let (status, task) = admin
        .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());

    let (status, _) = admin
        .post("/volumes/missing/resize", json!({ "size_bytes": 2 * MIB }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tasks_run_where_the_vm_disks_will_be_used() {
    let env = env().await;
    let admin = env.admin().await;
    pool(&env, vec![]).await;
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());

    // 꺼진 VM을 A에서 B로 옮겼다. 디스크는 다음에 B에서 쓰이므로 B에서 한다.
    volume(&env, "root", ImageFormat::Qcow2).await;
    let moved = vm_with_disks(&admin, "moved", &["root"]).await;
    set_vm_status(&env, moved, VmPhase::Stopped, Some(a)).await;
    let vm = env.cluster.get::<VmSpec>(moved).unwrap().unwrap();
    let spec = VmSpec {
        node: Some(b),
        ..vm.spec.clone()
    };
    env.cluster
        .update_spec(moved, Some(vm.revision), vm.labels.clone(), &spec)
        .await
        .unwrap();
    let (status, task) = admin
        .post("/vms/moved/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], b.to_string());

    // 배치된 적 없는 VM은 첫 디스크 볼륨이 있는 노드에서 한다.
    let data = volume(&env, "data", ImageFormat::Qcow2).await;
    let mut on_a = env.cluster.get::<VolumeSpec>(data).unwrap().unwrap();
    on_a.status.node = Some(a);
    env.cluster
        .update_status::<VolumeSpec>(data, &on_a.status)
        .await
        .unwrap();
    vm_with_disks(&admin, "new", &["data"]).await;
    let (status, task) = admin
        .post("/vms/new/snapshots", json!({ "name": "s" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], a.to_string());
}
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            storage,
            reconciler.handle(),
```

을 다음으로 바꾼다:

```rust
            storage,
            hv.clone(),
            reconciler.handle(),
```

`crates/opl-cluster/src/cluster.rs`에서

```rust

    pub async fn delete<S: ResourceSpec>(
```

을 다음으로 바꾼다:

```rust

    /// 리소스의 revision이 `expected_revision`일 때만 status를 바꾼다. 아니면 `RevisionMismatch`.
    /// 같은 status를 여러 곳에서 나눠 쓸 때, 읽은 뒤 다른 쪽이 쓴 값을 덮지 않게 한다.
    pub async fn update_status_checked<S: ResourceSpec>(
        &self,
        id: Uuid,
        expected_revision: u64,
        status: &S::Status,
    ) -> Result<u64, ClusterError> {
        self.write(Command::UpdateStatusIf {
            kind: S::KIND,
            id,
            expected_revision,
            status: serde_json::to_value(status)?,
        })
        .await
    }

    pub async fn delete<S: ResourceSpec>(
```

`crates/opl-cluster/src/state_machine.rs`에서

```rust
        }
        Command::Delete {
```

을 다음으로 바꾼다:

```rust
        }
        Command::UpdateStatusIf {
            kind,
            id,
            expected_revision,
            status,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(Some(expected_revision), resource.revision) {
                return Ok(Err(r));
            }
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::Delete {
```

`crates/opl-cluster/src/types.rs`에서

```rust
    UpdateStatus { kind: Kind, id: Uuid, status: Value },
    Delete {
```

을 다음으로 바꾼다:

```rust
    UpdateStatus { kind: Kind, id: Uuid, status: Value },
    /// 현재 revision이 `expected_revision`일 때만 status를 바꾼다. `UpdateStatus`에 필드를 더하면
    /// 옛 노드가 그 필드를 버리고 확인 없이 덮어쓰므로 새 명령으로 둔다. 옛 노드는 모르는 명령을
    /// 오류로 처리한다(설계 3.1절).
    UpdateStatusIf {
        kind: Kind,
        id: Uuid,
        expected_revision: u64,
        status: Value,
    },
    Delete {
```

`crates/opl-cluster/src/types.rs`에서

```rust
    }

    #[test]
    fn rejection_message_uses_wire_kind_name() {
```

을 다음으로 바꾼다:

```rust
    }

    /// 확인하지 않는 status 쓰기는 예전 형식 그대로다. 확인하는 쓰기는 새 명령이다.
    #[test]
    fn update_status_storage_format_is_stable() {
        let id = Uuid::from_u128(1);
        let old = json!({
            "op": "update-status",
            "kind": "vm",
            "id": id.to_string(),
            "status": {"phase": "running"}
        });
        let cmd = Command::UpdateStatus {
            kind: Kind::Vm,
            id,
            status: json!({"phase": "running"}),
        };
        assert_eq!(serde_json::to_value(&cmd).unwrap(), old);
        assert_eq!(serde_json::from_value::<Command>(old).unwrap(), cmd);

        let cmd = Command::UpdateStatusIf {
            kind: Kind::Vm,
            id,
            expected_revision: 7,
            status: json!(null),
        };
        let value = serde_json::to_value(&cmd).unwrap();
        assert_eq!(
            value,
            json!({
                "op": "update-status-if",
                "kind": "vm",
                "id": id.to_string(),
                "expected_revision": 7,
                "status": null
            })
        );
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), cmd);
    }

    #[test]
    fn rejection_message_uses_wire_kind_name() {
```

`crates/opl-cluster/tests/single_node.rs`에서

```rust

    // 거부된 명령은 아무것도 바꾸지 않는다.
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap().unwrap().revision, rev);
}
```

을 다음으로 바꾼다:

```rust

    let err = c
        .update_status_checked::<VolumeSpec>(v.id, rev + 100, &VolumeStatus::default())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::RevisionMismatch { expected, actual })
            if expected == rev + 100 && actual == rev
    ));

    // 거부된 명령은 아무것도 바꾸지 않는다.
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap().unwrap().revision, rev);

    let status = VolumeStatus {
        allocated_bytes: Some(1),
        ..Default::default()
    };
    let rev2 = c
        .update_status_checked::<VolumeSpec>(v.id, rev, &status)
        .await
        .unwrap();
    let got = c.get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!((got.revision, got.status), (rev2, status));
}
```

`crates/opl-driver/src/error.rs`에서

```rust
        }
    }
}
```

을 다음으로 바꾼다:

```rust
        }
    }

    /// 응답을 기다리다 시간이 다 된 오류인지. 요청한 작업(스냅샷 저장 등)이 하이퍼바이저 안에서
    /// 아직 돌고 있을 수 있으므로, 그 결과를 곧바로 되돌리거나 지우면 안 된다.
    pub fn may_be_in_progress(&self) -> bool {
        matches!(
            self,
            DriverError::Qmp(QmpError::JobTimeout { .. }) | DriverError::Timeout(_)
        )
    }
}
```

`crates/opl-driver/src/error.rs`에서

```rust
        assert!(!DriverError::VolumeNotFound(Uuid::nil()).is_transient());
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(!DriverError::VolumeNotFound(Uuid::nil()).is_transient());
    }

    #[test]
    fn timeouts_may_leave_work_in_progress() {
        let timeout = QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(1),
        };
        assert!(DriverError::Qmp(timeout).may_be_in_progress());
        assert!(DriverError::Timeout("QMP transaction").may_be_in_progress());
        let failed = QmpError::JobFailed {
            id: "j".into(),
            error: "x".into(),
        };
        assert!(!DriverError::Qmp(failed).may_be_in_progress());
        assert!(!DriverError::NotRunning(Uuid::nil()).may_be_in_progress());
        // 다시 붙지 못한 VM에는 요청을 보내지 않았다.
        assert!(!DriverError::NotAttached(Uuid::nil()).may_be_in_progress());
    }
}
```

`crates/opl-driver/src/lib.rs`에서

```rust
pub use opl_types::storage::SourceFormat;
pub use qemu::{QemuConfig, QemuHypervisor};
pub use storage::{Capacity, DirStorage, Storage, VolumeInfo};
```

을 다음으로 바꾼다:

```rust
pub use opl_types::storage::SourceFormat;
pub use qemu::{QemuConfig, QemuHypervisor, uefi_vars_path, vm_state_dir};
pub use storage::{Capacity, DirStorage, Storage, VolumeInfo};
```

`crates/opl-driver/src/mock.rs` 전체:

```rust
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::VmRuntimeSpec;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::DriverError;
use crate::hypervisor::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, RuntimeStatus, StopMode,
};

/// 메모리 안에서만 동작하는 하이퍼바이저. reconciler 테스트에서 장애를 주입하는 데 쓴다.
pub struct MockHypervisor {
    state: Mutex<MockState>,
    events: broadcast::Sender<HypervisorEvent>,
}

#[derive(Default)]
struct MockState {
    vms: HashMap<Uuid, u32>,
    next_pid: u32,
    fail_next_start: Option<DriverError>,
    /// `start`가 이만큼 기다린 뒤에 동작한다.
    start_delay: Option<Duration>,
    /// 다음 `snapshot_save`·`snapshot_delete` 한 번을 실패시킬 오류
    fail_next_snapshot: Option<DriverError>,
    /// 살아 있지만 QMP로 다룰 수 없는 VM
    unresponsive: HashSet<Uuid>,
    console_dir: Option<PathBuf>,
    /// 새로 띄우려 한 VM과 불러오려 한 스냅샷 태그 (실패한 시도 포함)
    launches: Vec<(Uuid, Option<String>)>,
    /// VM별 내부 스냅샷 (태그, 메모리 포함)
    snapshots: HashMap<Uuid, Vec<(String, bool)>>,
    /// `block_resize`로 바꾼 (VM, 디스크 번호)별 크기
    disk_sizes: HashMap<(Uuid, usize), u64>,
}

impl Default for MockHypervisor {
    fn default() -> Self {
        Self {
            state: Mutex::new(MockState {
                next_pid: 1000,
                ..Default::default()
            }),
            events: broadcast::channel(64).0,
        }
    }
}

impl MockHypervisor {
    /// 다음 `start` 한 번을 이 오류로 실패시킨다. 이미 실행 중인 VM에 대한 `start`에는 쓰이지 않는다.
    pub fn fail_next_start(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_start = Some(err);
    }

    /// 이후의 `start`가 `delay`만큼 기다린 뒤에 동작하게 한다. 시작 도중에 일어나는 일을 흉내 낸다.
    pub fn delay_starts(&self, delay: Duration) {
        self.state.lock().unwrap().start_delay = Some(delay);
    }

    /// 다음 `snapshot_save`나 `snapshot_delete` 한 번을 이 오류로 실패시킨다. `snapshot_save`는
    /// 태그를 남긴 채 실패한다(일부 디스크에 만들었거나, 시간 초과로 작업이 아직 돌고 있는 것처럼).
    pub fn fail_next_snapshot(&self, err: DriverError) {
        self.state.lock().unwrap().fail_next_snapshot = Some(err);
    }

    /// 게스트 OS 안에서 전원을 끈 것처럼 만든다. `stop(Acpi)` 요청에 게스트가 응한 경우도 이것으로 흉내 낸다.
    pub fn guest_shutdown(&self, id: Uuid) {
        self.remove(
            id,
            EventKind::Stopped {
                guest_initiated: true,
            },
        );
    }

    /// 프로세스가 비정상 종료한 것처럼 만든다.
    pub fn crash(&self, id: Uuid) {
        self.remove(id, EventKind::Crashed);
    }

    /// 실행 중인 VM을 살아 있지만 QMP로 다룰 수 없는 VM으로 만들거나(true) 되돌린다(false).
    /// 실제 드라이버처럼 조회에는 `unresponsive`로 드러나고, `start`와 `stop(Acpi)`는 다시 붙지
    /// 못해 실패하며, `stop(Force)`는 프로세스를 죽이므로 종료 이벤트 없이 사라진다.
    pub fn set_unresponsive(&self, id: Uuid, unresponsive: bool) {
        let mut st = self.state.lock().unwrap();
        if unresponsive {
            st.unresponsive.insert(id);
        } else {
            st.unresponsive.remove(&id);
        }
    }

    /// `console_endpoint`가 `{dir}/{id}.vnc`를 돌려주게 한다. 테스트가 그 경로에 가짜 VNC 서버를 띄운다.
    pub fn set_console_dir(&self, dir: impl Into<PathBuf>) {
        self.state.lock().unwrap().console_dir = Some(dir.into());
    }

    /// 새로 띄우려 한 VM과 `start`에 준 `restore` 태그를 순서대로 돌려준다. 이미 실행 중이어서
    /// 띄우지 않은 `start`는 빠지고, 주입한 오류로 실패한 시도는 들어간다.
    pub fn launches(&self) -> Vec<(Uuid, Option<String>)> {
        self.state.lock().unwrap().launches.clone()
    }

    /// 실행 중에 `snapshot_save`로 만들고 지우지 않은 스냅샷 (태그, 메모리 포함). VM이 꺼져도 남는다.
    pub fn snapshots(&self, id: Uuid) -> Vec<(String, bool)> {
        let st = self.state.lock().unwrap();
        st.snapshots.get(&id).cloned().unwrap_or_default()
    }

    /// `block_resize`로 마지막에 바꾼 크기
    pub fn disk_size(&self, id: Uuid, disk: usize) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .disk_sizes
            .get(&(id, disk))
            .copied()
    }

    /// 실제 드라이버처럼 실행 중인 VM만 다룬다. 살아 있지만 QMP로 다룰 수 없으면 다시 시도할 오류다.
    fn controllable(st: &MockState, id: Uuid) -> Result<(), DriverError> {
        if !st.vms.contains_key(&id) {
            Err(DriverError::NotRunning(id))
        } else if st.unresponsive.contains(&id) {
            Err(DriverError::NotAttached(id))
        } else {
            Ok(())
        }
    }

    fn remove(&self, id: Uuid, kind: EventKind) {
        let mut st = self.state.lock().unwrap();
        st.unresponsive.remove(&id);
        if st.vms.remove(&id).is_some() {
            drop(st);
            let _ = self.events.send(HypervisorEvent { id, kind });
        }
    }

    fn state_of(st: &MockState, id: Uuid, pid: u32) -> ActualState {
        let status = if st.unresponsive.contains(&id) {
            RuntimeStatus::Other("unresponsive".into())
        } else {
            RuntimeStatus::Running
        };
        ActualState { id, pid, status }
    }
}

#[async_trait]
impl Hypervisor for MockHypervisor {
    async fn start(&self, vm: &VmRuntimeSpec, restore: Option<&str>) -> Result<(), DriverError> {
        let delay = self.state.lock().unwrap().start_delay;
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        {
            let mut st = self.state.lock().unwrap();
            // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 주입한 오류도 쓰지 않는다.
            if st.vms.contains_key(&vm.id) {
                if st.unresponsive.contains(&vm.id) {
                    return Err(DriverError::Timeout("reattach to running QEMU"));
                }
                return Ok(());
            }
            st.launches.push((vm.id, restore.map(str::to_string)));
            if let Some(err) = st.fail_next_start.take() {
                return Err(err);
            }
            let pid = st.next_pid;
            st.next_pid += 1;
            st.vms.insert(vm.id, pid);
        }
        let _ = self.events.send(HypervisorEvent {
            id: vm.id,
            kind: EventKind::Started,
        });
        Ok(())
    }

    async fn stop(&self, id: Uuid, mode: StopMode) -> Result<(), DriverError> {
        {
            let mut st = self.state.lock().unwrap();
            if st.vms.contains_key(&id) && st.unresponsive.contains(&id) {
                // 실제 드라이버는 다시 붙지 못한 VM에 ACPI 요청을 보낼 수 없고, 강제 종료는
                // 프로세스를 죽이므로 종료 이벤트가 없다.
                return match mode {
                    StopMode::Acpi => Err(DriverError::Timeout("reattach to running QEMU")),
                    StopMode::Force => {
                        st.unresponsive.remove(&id);
                        st.vms.remove(&id);
                        Ok(())
                    }
                };
            }
        }
        match mode {
            // 실제 QEMU처럼 요청만 보낸다. 게스트는 무시할 수 있으므로 실제 종료는
            // 테스트가 `guest_shutdown`으로 일으킨다.
            StopMode::Acpi => {}
            StopMode::Force => self.remove(
                id,
                EventKind::Stopped {
                    guest_initiated: false,
                },
            ),
        }
        Ok(())
    }

    async fn inspect(&self, id: Uuid) -> Result<Option<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st.vms.get(&id).map(|&pid| Self::state_of(&st, id, pid)))
    }

    async fn list(&self) -> Result<Vec<ActualState>, DriverError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .vms
            .iter()
            .map(|(&id, &pid)| Self::state_of(&st, id, pid))
            .collect())
    }

    async fn console_endpoint(&self, id: Uuid) -> Result<PathBuf, DriverError> {
        let st = self.state.lock().unwrap();
        if st.vms.contains_key(&id) {
            Ok(match &st.console_dir {
                Some(dir) => dir.join(format!("{id}.vnc")),
                None => PathBuf::from(format!("/mock/{id}/vnc.sock")),
            })
        } else {
            Err(DriverError::NotRunning(id))
        }
    }

    async fn snapshot_save(&self, id: Uuid, tag: &str, memory: bool) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.snapshots
            .entry(id)
            .or_default()
            .push((tag.to_string(), memory));
        match st.fail_next_snapshot.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn snapshot_delete(&self, id: Uuid, tag: &str) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        if let Some(err) = st.fail_next_snapshot.take() {
            return Err(err);
        }
        if let Some(snapshots) = st.snapshots.get_mut(&id) {
            snapshots.retain(|(t, _)| t != tag);
        }
        Ok(())
    }

    async fn block_resize(
        &self,
        id: Uuid,
        disk: usize,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let mut st = self.state.lock().unwrap();
        Self::controllable(&st, id)?;
        st.disk_sizes.insert((id, disk), size_bytes);
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<HypervisorEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};

    use super::*;

    fn spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::now_v7(),
            name: "t".into(),
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: CpuModel::Host,
            },
            memory_mib: 256,
            firmware: Firmware::Uefi,
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
        }
    }

    #[tokio::test]
    async fn start_is_idempotent_and_emits_one_event() {
        let hv = MockHypervisor::default();
        let mut rx = hv.subscribe();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: vm.id,
                kind: EventKind::Started
            }
        );
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_vm_and_is_idempotent() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: false
            }
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(matches!(
            hv.console_endpoint(vm.id).await,
            Err(DriverError::NotRunning(_))
        ));
    }

    #[tokio::test]
    async fn console_dir_sets_endpoint() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.set_console_dir("/tmp/consoles");
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.console_endpoint(vm.id).await.unwrap(),
            PathBuf::from(format!("/tmp/consoles/{}.vnc", vm.id))
        );
    }

    #[tokio::test]
    async fn injected_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
        hv.start(&vm, None).await.unwrap();
    }

    #[tokio::test]
    async fn injected_failure_does_not_apply_to_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        hv.fail_next_start(DriverError::StartFailed("boom".into()));
        // 실제 드라이버처럼 이미 실행 중이면 기동을 시도하지 않으므로 실패하지 않는다.
        hv.start(&vm, None).await.unwrap();
        // 주입한 오류는 소비되지 않고 다음 실제 기동에 적용된다.
        hv.crash(vm.id);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::StartFailed(_))
        ));
    }

    #[tokio::test]
    async fn acpi_stop_only_requests_shutdown() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.stop(vm.id, StopMode::Acpi).await.unwrap();
        // 게스트가 요청을 무시할 수 있으므로 VM은 그대로 돌고 이벤트도 없다.
        assert!(rx.try_recv().is_err());
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!(st.status, RuntimeStatus::Running);
        assert_eq!(hv.list().await.unwrap().len(), 1);

        hv.guest_shutdown(vm.id);
        assert_eq!(
            rx.recv().await.unwrap().kind,
            EventKind::Stopped {
                guest_initiated: true
            }
        );
    }

    #[tokio::test]
    async fn unresponsive_vm_cannot_be_controlled_but_can_be_killed() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        let mut rx = hv.subscribe();
        hv.set_unresponsive(vm.id, true);

        let unresponsive = ActualState {
            id: vm.id,
            pid,
            status: RuntimeStatus::Other("unresponsive".into()),
        };
        assert_eq!(hv.inspect(vm.id).await.unwrap(), Some(unresponsive.clone()));
        assert_eq!(hv.list().await.unwrap(), vec![unresponsive]);
        assert!(matches!(
            hv.start(&vm, None).await,
            Err(DriverError::Timeout(_))
        ));
        assert!(matches!(
            hv.stop(vm.id, StopMode::Acpi).await,
            Err(DriverError::Timeout(_))
        ));

        // 다시 붙을 수 있게 되면 그대로 돌고 있다.
        hv.set_unresponsive(vm.id, false);
        hv.start(&vm, None).await.unwrap();
        let st = hv.inspect(vm.id).await.unwrap().unwrap();
        assert_eq!((st.pid, st.status), (pid, RuntimeStatus::Running));

        // 강제 종료는 프로세스를 죽이므로 종료 이벤트가 없다.
        hv.set_unresponsive(vm.id, true);
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn records_restore_tag_of_each_launch() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, Some("s1")).await.unwrap();
        // 이미 실행 중이면 새로 띄우지 않으므로 기록하지 않는다.
        hv.start(&vm, None).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        assert_eq!(
            hv.launches(),
            vec![(vm.id, Some("s1".to_string())), (vm.id, None)]
        );
    }

    #[tokio::test]
    async fn snapshot_ops_need_a_running_vm() {
        let hv = MockHypervisor::default();
        let vm = spec();
        for res in [
            hv.snapshot_save(vm.id, "s", true).await,
            hv.snapshot_delete(vm.id, "s").await,
            hv.block_resize(vm.id, 0, 1 << 30).await,
        ] {
            assert!(matches!(res, Err(DriverError::NotRunning(_))), "{res:?}");
        }

        hv.start(&vm, None).await.unwrap();
        hv.snapshot_save(vm.id, "mem", true).await.unwrap();
        hv.snapshot_save(vm.id, "disk", false).await.unwrap();
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        // 없는 태그를 지워도 성공한다.
        hv.snapshot_delete(vm.id, "mem").await.unwrap();
        assert_eq!(hv.snapshots(vm.id), vec![("disk".to_string(), false)]);
        hv.block_resize(vm.id, 1, 2 << 30).await.unwrap();
        assert_eq!(hv.disk_size(vm.id, 1), Some(2 << 30));
        assert_eq!(hv.disk_size(vm.id, 0), None);

        // QMP로 다룰 수 없는 VM은 다시 시도할 수 있는 오류로 끝난다.
        hv.set_unresponsive(vm.id, true);
        let err = hv.snapshot_save(vm.id, "x", false).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
    }

    #[tokio::test]
    async fn injected_snapshot_failure_applies_once() {
        let hv = MockHypervisor::default();
        let vm = spec();
        hv.start(&vm, None).await.unwrap();
        // 저장은 태그를 남긴 채 실패한다.
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_save(vm.id, "s", true).await.is_err());
        assert_eq!(hv.snapshots(vm.id), vec![("s".to_string(), true)]);
        hv.fail_next_snapshot(DriverError::StartFailed("boom".into()));
        assert!(hv.snapshot_delete(vm.id, "s").await.is_err());
        assert_eq!(hv.snapshots(vm.id).len(), 1);
        hv.snapshot_delete(vm.id, "s").await.unwrap();
        assert!(hv.snapshots(vm.id).is_empty());
    }

    #[tokio::test]
    async fn crash_and_guest_shutdown_emit_events() {
        let hv = MockHypervisor::default();
        let (a, b) = (spec(), spec());
        hv.start(&a, None).await.unwrap();
        hv.start(&b, None).await.unwrap();
        let mut rx = hv.subscribe();
        hv.crash(a.id);
        hv.guest_shutdown(b.id);
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: a.id,
                kind: EventKind::Crashed
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            HypervisorEvent {
                id: b.id,
                kind: EventKind::Stopped {
                    guest_initiated: true
                }
            }
        );
        assert!(hv.list().await.unwrap().is_empty());
    }
}
```

`crates/opl-driver/src/qemu.rs`에서

```rust
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);
```

을 다음으로 바꾼다:

```rust
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(540);

/// VM별 상태 디렉터리 `{state_dir}/vm/{id}`. `state_dir`은 [`QemuConfig::state_dir`]이다.
pub fn vm_state_dir(state_dir: &Path, id: Uuid) -> PathBuf {
    state_dir.join("vm").join(id.to_string())
}

/// VM의 UEFI 변수 파일. UEFI VM을 처음 띄울 때 템플릿을 복사해 만든다(BIOS VM에는 없다).
pub fn uefi_vars_path(state_dir: &Path, id: Uuid) -> PathBuf {
    vm_state_dir(state_dir, id).join("uefi-vars.fd")
}
```

`crates/opl-driver/src/qemu.rs`에서

```rust
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        paths.uefi_vars = self.state_dir(id).join("uefi-vars.fd");
        Ok(paths)
```

을 다음으로 바꾼다:

```rust
            .map_err(|e| DriverError::InvalidSpec(e.to_string()))?;
        paths.uefi_vars = uefi_vars_path(&self.config.state_dir, id);
        Ok(paths)
```

`crates/opl-driver/src/qemu.rs`에서

```rust
    fn state_dir(&self, id: Uuid) -> PathBuf {
        self.config.state_dir.join("vm").join(id.to_string())
    }
```

을 다음으로 바꾼다:

```rust
    fn state_dir(&self, id: Uuid) -> PathBuf {
        vm_state_dir(&self.config.state_dir, id)
    }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust

use opl_cluster::{Cluster, ClusterError};
```

을 다음으로 바꾼다:

```rust

use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
                if status != vm.status {
                    self.cluster
                        .update_status::<VmSpec>(self.id, &status)
                        .await?;
                }
```

을 다음으로 바꾼다:

```rust
                if status != vm.status {
                    self.write_status(status, vm).await?;
                }
```

`crates/opl-reconciler/src/worker.rs`에서

```rust

    /// spec.power를 stopped로 바꾸고 status를 기록한다. 둘 다 성공해야 끝난다.
    async fn write_power_off(&mut self, vm: Vm) -> Result<(), StepError> {
        let error = self.power_off.clone().flatten();
```

을 다음으로 바꾼다:

```rust

    /// `vm`을 읽은 뒤 계산한 status를 기록한다. `snapshot`(현재 스냅샷 위치)은 Task 실행기가 쓰는
    /// 값이므로 저장소의 것을 지킨다: revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어
    /// 그 `snapshot`으로 다시 쓴다. 나머지 필드는 전처럼 이 작업자가 계산한 값으로 덮는다.
    async fn write_status(&self, mut status: VmStatus, mut vm: Vm) -> Result<(), StepError> {
        loop {
            status.snapshot = vm.status.snapshot;
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
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            self.cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
```

을 다음으로 바꾼다:

```rust
            // 다시 읽은 revision으로 쓰므로, 그 사이 사용자가 바꾼 spec.power도 stopped로 덮어쓴다.
            // status는 방금 쓴 revision으로 곧바로 이어 쓴다.
            vm.revision = self
                .cluster
                .update_spec(self.id, Some(vm.revision), vm.labels.clone(), &spec)
```

`crates/opl-reconciler/src/worker.rs`에서

```rust
        };
        self.cluster
            .update_status::<VmSpec>(self.id, &status)
            .await?;
        self.power_off = None;
```

을 다음으로 바꾼다:

```rust
        };
        self.write_status(status, vm).await?;
        self.power_off = None;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, RestartPolicy, VmPhase, VmStatus};
use uuid::Uuid;
```

을 다음으로 바꾼다:

```rust
use opl_driver::{DriverError, Hypervisor, StopMode};
use opl_types::vm::{DiskBus, DiskSpec, PowerState, RestartPolicy, VmPhase, VmSpec, VmStatus};
use uuid::Uuid;
```

`crates/opl-reconciler/tests/reconcile.rs`에서

```rust
    env.wait(vm.id, "running", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}
```

을 다음으로 바꾼다:

```rust
    env.wait(vm.id, "running", running).await;
    assert_eq!(
        env.hv.launches(),
        vec![(vm.id, Some("snap-1".into())), (vm.id, None)]
    );
}

#[tokio::test]
async fn status_writes_keep_snapshot_position_written_meanwhile() {
    let env = Env::start().await;
    let vm = stopped_vm(&env).await;

    // 작업자가 VM을 읽고 시작하는 사이에 Task 실행기가 현재 스냅샷 위치를 쓴다.
    env.hv.delay_starts(Duration::from_millis(300));
    env.set_power(vm.id, PowerState::Running).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let current = env.get(vm.id);
    assert_ne!(current.status.phase, VmPhase::Running);
    let snapshot = Uuid::now_v7();
    let status = VmStatus {
        snapshot: Some(snapshot),
        ..current.status.clone()
    };
    env.cluster
        .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
        .await
        .unwrap();

    // 작업자가 쓴 Running status가 그 값을 덮지 않는다.
    env.wait(vm.id, "running", running).await;
    assert_eq!(env.get(vm.id).status.snapshot, Some(snapshot));
}
```

`crates/opld/src/member.rs`에서

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv, reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
```

을 다음으로 바꾼다:

```rust
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img));
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        let runner =
            TaskRunner::spawn(cluster.clone(), storage, reconciler.handle(), runner_config).await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());
```

을 다음으로 바꾼다:

```rust
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv,
            reconciler.handle(),
            runner_config,
        )
        .await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 290개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test snapshot_disks`
Expected: PASS 4개

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(api): 스냅샷 만들기·되돌리기·삭제 Task 실행"
```

---

### Task 8: 볼륨 크기 늘리기 Task 실행

Task 안에서 준비 상태·크기·VM 상태를 다시 본다. 실행 중인 VM이 쓰면 그 노드에서 `block_resize`(디스크 순서 = QEMU `disk{i}`), 아니면 `qemu-img resize`, 뒤에 `volume_info`의 실제 크기를 볼륨 스펙에 쓴다(다시 읽은 revision으로). 크기 늘리기는 그 볼륨을 쓰는 VM(없으면 그 볼륨을 담은 스냅샷의 VM)의 줄에 서서 되돌리기와 겹치지 않는다. 라우팅은 쓰는 VM의 노드, 없으면 볼륨의 노드.

**Files:**
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/volumes.rs`
- Modify: `crates/opl-api/tests/snapshot_disks.rs`
- Modify: `crates/opl-api/tests/snapshot_tasks.rs`
- Modify: `crates/opl-api/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 대상(볼륨, 스냅샷 Task의 VM)별 마지막 Task. 같은 대상의 Task는 앞의 것이 끝난 뒤에 시작한다.
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
```

을 다음으로 바꾼다:

```rust
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
```

`crates/opl-api/src/tasks.rs`에서

```rust

/// Task를 실행한다. 볼륨 Task는 같은 볼륨의 앞 Task가 끝난 뒤에 시작한다(`Storage`의 요구).
/// 스냅샷 Task도 같은 VM의 앞 스냅샷 Task가 끝난 뒤에 시작한다(부모·현재 위치가 차례로 바뀐다).
/// VM 전원 Task는 기다리기만 하므로 차례를 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다.
```

을 다음으로 바꾼다:

```rust

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        return (is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op)).then_some(target);
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
```

`crates/opl-api/src/tasks.rs`에서

```rust
    let id = task.id;
    let target = task.spec.target;
    let queued = is_volume_op(&task.spec.op) || is_snapshot_op(&task.spec.op);
    let previous = if queued {
        by_target.remove(&target)
    } else {
        None
    };
    let ctx = ctx.clone();
```

을 다음으로 바꾼다:

```rust
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
```

`crates/opl-api/src/tasks.rs`에서

```rust
    running.insert(id, handle.abort_handle());
    if queued {
        by_target.insert(target, handle);
    }
```

을 다음으로 바꾼다:

```rust
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::SnapshotDelete { snapshot } => snapshot::delete(ctx, id, *snapshot).await,
        // 실행은 뒤에서 붙인다.
        TaskOp::VolumeResize { .. } => Err("not implemented yet".into()),
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::SnapshotDelete { snapshot } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
    }
```

`crates/opl-api/src/tasks.rs`에서

```rust

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
```

을 다음으로 바꾼다:

```rust

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
            .resize_volume(&pool.spec, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool.spec, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
```

`crates/opl-api/src/volumes.rs`에서

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{VmPhase, VmSpec};
use serde::Deserialize;
```

을 다음으로 바꾼다:

```rust
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
```

`crates/opl-api/src/volumes.rs`에서

```rust
}
use crate::snapshots::vm_node;
use crate::state::AppState;
```

을 다음으로 바꾼다:

```rust
}
use crate::snapshots::vm_placement;
use crate::state::AppState;
```

`crates/opl-api/src/volumes.rs`에서

```rust

/// 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). 실행 중인 VM이 쓰면 그 VM의
/// 노드에서, 아니면 볼륨의 노드에서 한다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
```

을 다음으로 바꾼다:

```rust

/// 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). VM이 쓰면 그 VM의 스냅샷
/// Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
/// 거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
```

`crates/opl-api/src/volumes.rs`에서

```rust
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let running = vms.iter().find(|vm| {
        !matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
            && vm.spec.disks.iter().any(|d| d.volume == volume.name)
    });
    let node = match running {
        Some(vm) => vm_node(&state, vm),
        None => volume_node(&state, &volume)?,
```

을 다음으로 바꾼다:

```rust
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let user = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name));
    let node = match user.and_then(|vm| vm_placement(&state.cluster, vm)) {
        Some(node) => node,
        None => volume_node(&state, &volume)?,
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
```

을 다음으로 바꾼다:

```rust
//! 디스크 파일의 스냅샷 태그를 다루는 Task: 정지한 VM의 만들기·되돌리기·삭제, 메모리 스냅샷
//! 되돌리기, 실패와 시작할 때의 정리. 볼륨 크기 늘리기와 그 차례. qemu-img와 qemu-io가 필요하다.
//! `cargo test -p opl-api --features storage-it --test snapshot_disks`
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
use std::process::Command;

use common::{Client, Env, vm_request};
use opl_driver::uefi_vars_path;
```

을 다음으로 바꾼다:

```rust
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_driver::uefi_vars_path;
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
        .unwrap_or_default()
}
```

을 다음으로 바꾼다:

```rust
        .unwrap_or_default()
}

/// 파일의 가상 크기
fn virtual_size(path: &Path) -> u64 {
    let info: Value =
        serde_json::from_str(&qemu_img(&["info", "-U", "--output=json", path_str(path)])).unwrap();
    info["virtual-size"].as_u64().unwrap()
}
```

`crates/opl-api/tests/snapshot_disks.rs`에서

```rust
    assert!(items.is_empty());
}
```

을 다음으로 바꾼다:

```rust
    assert!(items.is_empty());
}

fn volume_size(env: &Env, name: &str) -> u64 {
    let volume = env
        .cluster
        .get_by_name::<VolumeSpec>(name)
        .unwrap()
        .unwrap();
    volume.spec.size_bytes
}

async fn resize(admin: &Client, size: u64) -> (StatusCode, Value) {
    admin
        .post("/volumes/root/resize", json!({ "size_bytes": size }))
        .await
}

#[tokio::test]
async fn stopped_volume_grows_and_task_rechecks_the_size() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;

    succeeded(&wait(&admin, resize(&admin, 8 * MIB).await).await);
    assert_eq!(virtual_size(&file), 8 * MIB);
    assert_eq!(volume_size(&env, "root"), 8 * MIB);
    assert_eq!(env.hv.disk_size(id, 0), None);

    // 둘 다 API 검사를 지나지만, 앞의 것이 늘린 뒤에는 뒤의 것이 더 크지 않다.
    env.stop_runner().await;
    let first = resize(&admin, 12 * MIB).await;
    let second = resize(&admin, 12 * MIB).await;
    env.start_runner().await;
    succeeded(&wait(&admin, first).await);
    let task = wait(&admin, second).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert!(
        task["status"]["error"].as_str().unwrap().contains("larger"),
        "{task}"
    );
    assert_eq!(virtual_size(&file), 12 * MIB);
    assert_eq!(volume_size(&env, "root"), 12 * MIB);
}

#[tokio::test]
async fn running_vm_disk_grows_in_qemu_and_task_rechecks_the_vm() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);

    // Mock 하이퍼바이저는 파일을 늘리지 않으므로 QEMU가 늘렸을 크기를 대신 만든다.
    qemu_img(&["resize", "-q", path_str(&file), "8M"]);
    succeeded(&wait(&admin, resize(&admin, 8 * MIB).await).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
    assert_eq!(volume_size(&env, "root"), 8 * MIB);

    // API는 실행 중일 때 받았지만 실행할 때는 꺼졌다. 파일을 늘린다.
    env.stop_runner().await;
    let task = resize(&admin, 12 * MIB).await;
    let current = vm(&env, id);
    let mut spec = current.spec.clone();
    spec.power = PowerState::Stopped;
    env.cluster
        .update_spec(id, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while vm(&env, id).status.phase != VmPhase::Stopped {
        assert!(Instant::now() < deadline, "VM did not stop");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    env.start_runner().await;
    succeeded(&wait(&admin, task).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
    assert_eq!(virtual_size(&file), 12 * MIB);
    assert_eq!(volume_size(&env, "root"), 12 * MIB);
}

#[tokio::test]
async fn resize_waits_for_a_revert_of_the_same_disk() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    create_pool(&env).await;
    let file = create_volume(&env, &admin, "root").await;
    let id = create_vm(&admin, "web", &["root"]).await;
    succeeded(&admin.act("web", "start").await);
    let task = wait(
        &admin,
        admin
            .post("/vms/web/snapshots", json!({ "name": "mem" }))
            .await,
    )
    .await;
    succeeded(&task);
    let mem = get(&env, id, "mem").unwrap();
    qemu_img(&["snapshot", "-c", &mem.id.to_string(), path_str(&file)]);

    // 되돌리기(VM의 줄)를 먼저, 크기 늘리기(볼륨 Task)를 뒤에 맡긴다. 되돌리기는 시작이 늦어
    // 오래 걸린다. 크기 늘리기는 그동안 기다렸다가 다시 켜진 VM에서 한다.
    env.stop_runner().await;
    let (status, revert) = admin
        .post("/vms/web/snapshots/mem/revert", Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{revert}");
    let (status, grow) = resize(&admin, 8 * MIB).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{grow}");
    env.hv.delay_starts(Duration::from_millis(500));
    env.start_runner().await;
    let state = |task: &Value| {
        let path = format!("/tasks/{}", task["id"].as_str().unwrap());
        let admin = admin.clone();
        async move { admin.get(&path).await.1["status"]["state"].clone() }
    };
    let deadline = Instant::now() + WAIT;
    loop {
        // 크기 늘리기를 먼저 읽는다. 되돌리기를 먼저 읽으면 두 읽기 사이에 되돌리기가 끝나고
        // 크기 늘리기가 시작될 수 있다.
        let grown = state(&grow).await;
        let reverted = state(&revert).await;
        if reverted == "succeeded" {
            break;
        }
        assert_ne!(reverted, "failed");
        assert_eq!(grown, "pending", "resize ran during the revert");
        assert!(Instant::now() < deadline, "revert did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    succeeded(&admin.wait_task(&grow).await);
    assert_eq!(env.hv.disk_size(id, 0), Some(8 * MIB));
}
```

`crates/opl-api/tests/snapshot_tasks.rs`에서

```rust

fn vm_spec(node: Uuid) -> VmSpec {
```

을 다음으로 바꾼다:

```rust

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

fn vm_spec(node: Uuid) -> VmSpec {
```

`crates/opl-api/tests/snapshots.rs`에서

```rust

    // 실행 중인 VM이 쓰면 그 VM의 노드에서 한다.
    let vm = vm_with_disks(&admin, "web", &["data"]).await;
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
```

을 다음으로 바꾼다:

```rust

    // 배치된 적 없는 꺼진 VM이 쓰면 볼륨의 노드, 배치된 노드가 있으면 그 노드에서 한다
    // (그 VM의 스냅샷 Task와 같은 실행기의 줄에 선다).
    let vm = vm_with_disks(&admin, "web", &["data"]).await;
    let resize = || async {
        let (status, task) = admin
            .post("/volumes/data/resize", json!({ "size_bytes": 2 * MIB }))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        task["spec"]["node"].as_str().unwrap().to_string()
    };
    assert_eq!(resize().await, pool_node.to_string());
    let placed = Uuid::now_v7();
    set_vm_status(&env, vm, VmPhase::Stopped, Some(placed)).await;
    assert_eq!(resize().await, placed.to_string());

    // 실행 중인 VM이 쓰면 그 VM의 노드에서 한다.
    set_vm_status(&env, vm, VmPhase::Running, Some(env.node)).await;
```

`crates/opl-api/tests/snapshots.rs`에서

```rust
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());

    let (status, _) = admin
```

을 다음으로 바꾼다:

```rust
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());

    // 끈 뒤 다른 노드로 옮겼으면 디스크를 다음에 쓸 그 노드에서 한다.
    set_vm_status(&env, vm, VmPhase::Stopped, Some(env.node)).await;
    let moved = Uuid::now_v7();
    let current = env.cluster.get::<VmSpec>(vm).unwrap().unwrap();
    let spec = VmSpec {
        node: Some(moved),
        ..current.spec.clone()
    };
    env.cluster
        .update_spec(vm, Some(current.revision), current.labels.clone(), &spec)
        .await
        .unwrap();
    assert_eq!(resize().await, moved.to_string());

    let (status, _) = admin
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 291개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test snapshot_disks`
Expected: PASS 7개

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 177개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(api): 볼륨 크기 늘리기 Task 실행"
```

---

### Task 9: 실제 QEMU 통합 테스트

`opld` 데몬 하네스로 실행 중 VM의 메모리 스냅샷 → 실행 중 크기 늘리기 → 디스크만 스냅샷 → 메모리 되돌리기(새 QEMU로 실행, 크기도 돌아옴) → 디스크 되돌리기(정지) → 스냅샷이 남은 VM 삭제 409 → 실행 중·정지 중 삭제(태그가 사라지고 위치는 부모로)를 본다. 정지 VM은 `qemu-io`로 쓴 패턴이 되돌린 뒤 돌아오는지 본다. API는 로그인한 `oplctl` 설정의 토큰으로 부른다.

**Files:**
- Create: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/snapshots.rs`를 만든다:

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제)과 볼륨 크기 늘리기를 데몬 전체를 거쳐 확인한다.
//! 스냅샷 CLI는 아직 없어 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
//! 돌아오는지는 정지한 VM에서 `qemu-io`로 본다.
//! `cargo test -p opld --features qemu-it --test snapshots`
//! QEMU(`qemu-it`와 같다)와 qemu-img, qemu-io가 필요하다.
#![cfg(feature = "qemu-it")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use oplctl::Client;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const MIB: u64 = 1 << 20;

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

/// 클러스터를 만들고 로그인해 풀 `local`과 qcow2 볼륨 `root`(64 MiB)를 만든다. 볼륨 파일 경로를 돌려준다.
async fn setup(d: &mut Daemon) -> PathBuf {
    d.start().await;
    d.init("snap").await;
    d.login().await;
    let pool = d.path("pool");
    std::fs::create_dir(&pool).unwrap();
    d.ctl(&["pool", "create", "local", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    d.ctl(&[
        "volume", "create", "root", "--pool", "local", "--size", "64M",
    ])
    .await
    .unwrap();
    let volume = d.ctl_json(&["volume", "get", "root"]).await;
    pool.join(format!("{}.qcow2", volume["id"].as_str().unwrap()))
}

/// Task를 돌려주는 요청을 보내고 Task가 성공할 때까지 기다린다.
async fn run(c: &Client, path: &str, body: Value) {
    let task = c.post(path, &body).await.unwrap();
    c.wait_task(&task).await.unwrap();
}

async fn delete_snapshot(c: &Client, name: &str) {
    let task = c
        .delete(&format!("/vms/web/snapshots/{name}"))
        .await
        .unwrap();
    c.wait_task(&task).await.unwrap();
}

/// 스냅샷 이름 → 리소스
async fn snapshot(c: &Client, name: &str) -> Value {
    c.get(&format!("/vms/web/snapshots/{name}")).await.unwrap()
}

async fn vm(c: &Client) -> Value {
    c.get("/vms/web").await.unwrap()
}

/// 볼륨 파일에 든 스냅샷 태그. 실행 중인 VM이 쥔 파일도 읽는다(`-U`).
fn tags(file: &Path) -> Vec<String> {
    let info = qemu_img_info(file);
    let mut tags: Vec<String> = info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    tags.sort();
    tags
}

/// 태그에 담긴 메모리 상태의 크기(디스크만 담았으면 0)
fn vm_state_size(file: &Path, tag: &str) -> u64 {
    let info = qemu_img_info(file);
    let snapshot = info["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == tag)
        .unwrap_or_else(|| panic!("no tag {tag}: {info}"))
        .clone();
    snapshot["vm-state-size"].as_u64().unwrap()
}

/// QEMU가 디스크 헤더에 쓴 크기
fn virtual_size(file: &Path) -> u64 {
    qemu_img_info(file)["virtual-size"].as_u64().unwrap()
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

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(file: &Path, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd])
        .arg(file)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn running_vm_snapshots_revert_resize_and_delete() {
    // 디스크에 OS가 없는 UEFI 게스트는 ACPI 종료에 응하지 않을 수 있다.
    let mut d = Daemon::new("node1", "[reconciler]\nacpi_timeout_secs = 5\n");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--start",
    ])
    .await
    .unwrap();
    let pid = vm(&c).await["status"]["pid"].as_u64().unwrap();

    // 실행 중이면 기본으로 메모리까지 담는다. VM은 멈췄다가 계속 돈다.
    run(&c, "/vms/web/snapshots", json!({ "name": "mem" })).await;
    let mem = snapshot(&c, "mem").await;
    let mem_id = mem["id"].as_str().unwrap().to_string();
    assert_eq!(mem["status"]["phase"], "ready", "{mem}");
    assert_eq!(mem["spec"]["memory"], true, "{mem}");
    assert_eq!(mem["spec"]["parent"], Value::Null, "{mem}");
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");
    assert_eq!(tags(&file), vec![mem_id.clone()]);
    assert!(vm_state_size(&file, &mem_id) > 0);

    // 실행 중 크기 늘리기: QEMU가 늘리고(헤더의 크기), 스펙도 그 크기를 받는다.
    run(
        &c,
        "/volumes/root/resize",
        json!({ "size_bytes": 128 * MIB }),
    )
    .await;
    assert_eq!(virtual_size(&file), 128 * MIB);
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["spec"]["size_bytes"], 128 * MIB, "{volume}");
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["pid"].as_u64(), Some(pid), "{v}");

    // 디스크만 담는 스냅샷은 지금 위치(mem)를 부모로 한다.
    run(
        &c,
        "/vms/web/snapshots",
        json!({ "name": "disk", "memory": false }),
    )
    .await;
    let disk = snapshot(&c, "disk").await;
    let disk_id = disk["id"].as_str().unwrap().to_string();
    assert_eq!(disk["spec"]["memory"], false, "{disk}");
    assert_eq!(disk["spec"]["parent"], mem_id.as_str(), "{disk}");
    assert_eq!(vm(&c).await["status"]["snapshot"], disk_id.as_str());
    assert_eq!(tags(&file), sorted(vec![mem_id.clone(), disk_id.clone()]));
    assert_eq!(vm_state_size(&file, &disk_id), 0);
    let list = c.get("/vms/web/snapshots").await.unwrap();
    let mut names: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["spec"]["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["disk", "mem"], "{list}");

    // 메모리 스냅샷으로 되돌리면 그 메모리로 새로 시작해 실행 상태로 돌아온다. 크기도 그때로 돌아간다.
    run(&c, "/vms/web/snapshots/mem/revert", json!({})).await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_ne!(v["status"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");
    assert_eq!(virtual_size(&file), 64 * MIB);
    let volume = c.get("/volumes/root").await.unwrap();
    assert_eq!(volume["spec"]["size_bytes"], 64 * MIB, "{volume}");

    // 디스크만 담은 스냅샷으로 되돌리면 꺼진 채 남는다.
    run(&c, "/vms/web/snapshots/disk/revert", json!({})).await;
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "stopped", "{v}");
    assert_eq!(v["status"]["snapshot"], disk_id.as_str(), "{v}");
    assert_eq!(virtual_size(&file), 128 * MIB);

    // 스냅샷이 남은 VM은 지울 수 없다.
    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "vm_has_snapshots", "{body}");

    // 실행 중에 지우면 QEMU가 디스크의 태그를 지우고, 지금 위치는 부모로 옮긴다.
    let out = d.ctl(&["vm", "start", "web"]).await.unwrap();
    assert!(out.contains("vm web: start done"), "{out}");
    delete_snapshot(&c, "disk").await;
    assert_eq!(tags(&file), vec![mem_id.clone()]);
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "running", "{v}");
    assert_eq!(v["status"]["snapshot"], mem_id.as_str(), "{v}");

    // 꺼진 뒤에 지우면 스토리지가 지운다.
    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    delete_snapshot(&c, "mem").await;
    assert!(tags(&file).is_empty());
    assert_eq!(vm(&c).await["status"]["snapshot"], Value::Null);
    let list = c.get("/vms/web/snapshots").await.unwrap();
    assert_eq!(list["items"], json!([]), "{list}");

    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}

#[tokio::test]
async fn stopped_vm_snapshot_restores_disk_content() {
    let mut d = Daemon::new("node1", "");
    let file = setup(&mut d).await;
    let c = client(&d);
    d.ctl(&["vm", "create", "web", "--memory", "256", "--disk", "root"])
        .await
        .unwrap();

    assert!(qemu_io(&file, "write -P 0xab 0 64k"));
    // 꺼진 VM은 기본으로 디스크만 담는다.
    run(&c, "/vms/web/snapshots", json!({ "name": "before" })).await;
    let before = snapshot(&c, "before").await;
    let id = before["id"].as_str().unwrap().to_string();
    assert_eq!(before["status"]["phase"], "ready", "{before}");
    assert_eq!(before["spec"]["memory"], false, "{before}");
    assert_eq!(tags(&file), vec![id.clone()]);

    assert!(qemu_io(&file, "write -P 0xcd 0 64k"));
    assert!(!qemu_io(&file, "read -P 0xab 0 64k"));
    run(&c, "/vms/web/snapshots/before/revert", json!({})).await;
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    let v = vm(&c).await;
    assert_eq!(v["status"]["phase"], "stopped", "{v}");
    assert_eq!(v["status"]["snapshot"], id.as_str(), "{v}");

    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    delete_snapshot(&c, "before").await;
    assert!(tags(&file).is_empty());
    // 지운 뒤에도 디스크 내용은 그대로다.
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.stop();
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 291개, 실패 0

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 2개(약 7초), 남는 QEMU 없음

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "test(opld): 실제 QEMU로 스냅샷과 볼륨 크기 늘리기 통합 테스트"
```

---

### Task 10: oplctl: 스냅샷 명령과 볼륨 크기 늘리기

`oplctl vm snapshot create <vm> <이름> [--description] [--no-memory]`, `list`(나무 모양, 현재 위치 `*`, 시각은 UTC), `revert`, `delete`, `vm delete --with-snapshots`(스냅샷을 하나씩 지우고 기다린 뒤 VM), `volume resize <볼륨> --size 20G`. 모두 Task가 끝날 때까지 기다린다. 통합 테스트 일부를 새 명령으로 바꾼다.

**Files:**
- Modify: `crates/oplctl/Cargo.toml`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/tests/snapshots.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/Cargo.toml`에서

```toml
sha2.workspace = true
tokio.workspace = true
```

을 다음으로 바꾼다:

```toml
sha2.workspace = true
time.workspace = true
tokio.workspace = true
```

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
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
    /// 로컬 디렉터리 풀을 만든다.
    Create {
        name: String,
        /// 풀 디렉터리(절대 경로)
        #[arg(long)]
        path: PathBuf,
        /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
        #[arg(long = "node")]
        nodes: Vec<String>,
    },
    Delete {
        pool: String,
    },
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

#[derive(Clone, Copy, ValueEnum)]
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
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
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
        #[arg(long, value_enum, default_value = "qcow2")]
        format: Format,
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

#[derive(Clone, Copy, ValueEnum)]
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
    /// 기본값은 호스트에 맞춘다(Apple Silicon: virt, x86_64: q35).
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
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
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
            let machine = args.machine.unwrap_or(if cfg!(target_arch = "aarch64") {
                Machine::Virt
            } else {
                Machine::Q35
            });
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?["id"].clone()),
                None => None,
            };
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
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다.
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                for s in list["items"].as_array().cloned().unwrap_or_default() {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
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
            table(
                out,
                &["", "NAME", "CREATED", "MEMORY", "DESCRIPTION"],
                &rows,
            )?;
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
    }
    Ok(())
}

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
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            snap(7, "b2", Some(2), false, ""),
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(
            &mut out,
            &["", "NAME", "CREATED", "MEMORY", "DESCRIPTION"],
            &rows,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME      CREATED               MEMORY  DESCRIPTION\n\
             \x20  base      2026-10-26 07:34 UTC  false   clean install\n\
             *  ├─ b      2026-10-26 07:35 UTC  true\n\
             \x20  │  └─ b2  2026-10-26 07:40 UTC  false\n\
             \x20  └─ c      2026-10-26 07:36 UTC  false\n\
             \x20  orphan    2026-10-26 07:38 UTC  true\n\
             \x20  └─ new    pending               false\n"
        );
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

`crates/opld/tests/snapshots.rs`에서

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제)과 볼륨 크기 늘리기를 데몬 전체를 거쳐 확인한다.
//! 스냅샷 CLI는 아직 없어 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
//! 돌아오는지는 정지한 VM에서 `qemu-io`로 본다.
```

을 다음으로 바꾼다:

```rust
//! 실제 QEMU로 VM 스냅샷(만들기·되돌리기·삭제)과 볼륨 크기 늘리기를 데몬 전체를 거쳐 확인한다.
//! 꺼진 VM은 oplctl로, 실행 중인 VM은 응답을 자세히 보려고 API를 직접 부른다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용이
//! 돌아오는지는 정지한 VM에서 `qemu-io`로 본다.
```

`crates/opld/tests/snapshots.rs`에서

```rust
    // 실행 중 크기 늘리기: QEMU가 늘리고(헤더의 크기), 스펙도 그 크기를 받는다.
    run(
        &c,
        "/volumes/root/resize",
        json!({ "size_bytes": 128 * MIB }),
    )
    .await;
    assert_eq!(virtual_size(&file), 128 * MIB);
```

을 다음으로 바꾼다:

```rust
    // 실행 중 크기 늘리기: QEMU가 늘리고(헤더의 크기), 스펙도 그 크기를 받는다.
    let out = d
        .ctl(&["volume", "resize", "root", "--size", "128M"])
        .await
        .unwrap();
    assert!(out.contains("volume root resized to 128.0MiB"), "{out}");
    assert_eq!(virtual_size(&file), 128 * MIB);
```

`crates/opld/tests/snapshots.rs`에서

```rust
    // 꺼진 VM은 기본으로 디스크만 담는다.
    run(&c, "/vms/web/snapshots", json!({ "name": "before" })).await;
    let before = snapshot(&c, "before").await;
```

을 다음으로 바꾼다:

```rust
    // 꺼진 VM은 기본으로 디스크만 담는다.
    let out = d
        .ctl(&[
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--description",
            "clean",
        ])
        .await
        .unwrap();
    assert!(out.contains("snapshot before of vm web created"), "{out}");
    let before = snapshot(&c, "before").await;
```

`crates/opld/tests/snapshots.rs`에서

```rust
    assert!(!qemu_io(&file, "read -P 0xab 0 64k"));
    run(&c, "/vms/web/snapshots/before/revert", json!({})).await;
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
```

을 다음으로 바꾼다:

```rust
    assert!(!qemu_io(&file, "read -P 0xab 0 64k"));
    let out = d
        .ctl(&["vm", "snapshot", "revert", "web", "before"])
        .await
        .unwrap();
    assert!(out.contains("vm web: reverted to snapshot before"), "{out}");
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
```

`crates/opld/tests/snapshots.rs`에서

```rust

    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    delete_snapshot(&c, "before").await;
    assert!(tags(&file).is_empty());
    // 지운 뒤에도 디스크 내용은 그대로다.
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
```

을 다음으로 바꾼다:

```rust

    // 두 번째 스냅샷은 지금 위치(before) 아래에 붙고 현재 위치가 된다.
    d.ctl(&["vm", "snapshot", "create", "web", "after"])
        .await
        .unwrap();
    let out = d.ctl(&["vm", "snapshot", "list", "web"]).await.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(
        lines[1].starts_with("   before ") && lines[1].ends_with("clean"),
        "{out}"
    );
    assert!(lines[2].starts_with("*  └─ after "), "{out}");

    let resp = c.request(Method::DELETE, "/vms/web").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    // 스냅샷을 하나씩 지운 뒤 VM을 지운다. 지운 뒤에도 디스크 내용은 그대로다.
    let out = d
        .ctl(&["vm", "delete", "web", "--with-snapshots"])
        .await
        .unwrap();
    assert!(out.contains("snapshot before of vm web deleted"), "{out}");
    assert!(out.contains("snapshot after of vm web deleted"), "{out}");
    assert!(out.contains("vm web deleted"), "{out}");
    assert!(tags(&file).is_empty());
    assert!(qemu_io(&file, "read -P 0xab 0 64k"));
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 293개, 실패 0

Run: `cargo test -p opld --features qemu-it --test snapshots`
Expected: PASS 2개

- [ ] **Step 3: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(oplctl): 스냅샷 명령과 볼륨 크기 늘리기"
```

---

### Task 11: 웹: VM 상세 스냅샷 탭

`snapshot`을 watch 종류에 더하고, VM 상세에 "스냅샷" 탭을 둔다: 나무 목록(현재 위치, 만드는 중, 깊이는 스크린 리더에 "(… 다음)"), 만들기(이름은 이 VM 안에서 고유, 설명 1024자, "메모리 포함"은 실행 중이고 디스크가 있을 때만 보이고 기본 켬, 안 보이면 `memory: false`), 되돌리기·삭제 확인. 권한은 VM Operate. raw 디스크, 시작·정지 중, 목록 실패는 까닭을 글로 보인다. 새 Task 종류와 오류를 번역한다.

**Files:**
- Create: `web/src/vms/VmSnapshots.test.tsx`
- Create: `web/src/vms/VmSnapshots.tsx`
- Create: `web/src/vms/snapshots.test.ts`
- Create: `web/src/vms/snapshots.ts`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/test/render.tsx`
- Modify: `web/src/vms/VmDetailPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx`에서

```tsx
    }),
    ...["/vms", "/volumes", "/storage-pools", "/nodes", "/tasks", "/networks"].map((path) =>
      http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
```

을 다음으로 바꾼다:

```tsx
    }),
    ...["/vms", "/volumes", "/storage-pools", "/nodes", "/tasks", "/networks", "/snapshots"].map(
      (path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
    http.get(apiUrl("/networks"), empty),
  ];
```

을 다음으로 바꾼다:

```tsx
    http.get(apiUrl("/networks"), empty),
    http.get(apiUrl("/snapshots"), empty),
  ];
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(await screen.findByText("live: web=running")).toBeInTheDocument();
  expect(urls[0]).toContain("kinds=vm,volume,storage-pool,node,task,network&since=7");
});
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByText("live: web=running")).toBeInTheDocument();
  expect(urls[0]).toContain("kinds=vm,volume,storage-pool,node,task,network,snapshot&since=7");
});
```

`web/src/api/live.ts`에서

```ts
/** 화면이 실시간으로 보는 리소스 종류. 사용자와 토큰은 감시할 수 없다. */
export const KINDS: WatchKind[] = ["vm", "volume", "storage-pool", "node", "task", "network"];
```

을 다음으로 바꾼다:

```ts
/** 화면이 실시간으로 보는 리소스 종류. 사용자와 토큰은 감시할 수 없다. */
export const KINDS: WatchKind[] = [
  "vm",
  "volume",
  "storage-pool",
  "node",
  "task",
  "network",
  "snapshot",
];
```

`web/src/api/live.ts`에서

```ts
        return call(api.GET("/networks"));
    }
```

을 다음으로 바꾼다:

```ts
        return call(api.GET("/networks"));
      case "snapshot":
        return call(api.GET("/snapshots"));
    }
```

`web/src/api/types.ts`에서

```ts
export type Task = Schemas["Resource_TaskSpec_TaskStatus"];
export type Principal = Schemas["Principal"];
```

을 다음으로 바꾼다:

```ts
export type Task = Schemas["Resource_TaskSpec_TaskStatus"];
export type Snapshot = Schemas["Resource_SnapshotSpec_SnapshotStatus"];
export type Principal = Schemas["Principal"];
```

`web/src/api/types.ts`에서

```ts
/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind = "vm" | "volume" | "storage-pool" | "node" | "task" | "network";
```

을 다음으로 바꾼다:

```ts
/** watch로 받는 리소스 종류. 이름은 API의 `kinds`와 같다. */
export type WatchKind = "vm" | "volume" | "storage-pool" | "node" | "task" | "network" | "snapshot";
```

`web/src/api/types.ts`에서

```ts
  network: Network;
}
```

을 다음으로 바꾼다:

```ts
  network: Network;
  snapshot: Snapshot;
}
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("viewer", "operate", "vm")).toBe(false);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("viewer", "operate", "vm")).toBe(false);
  // 스냅샷 목록은 누구나 읽는다. 만들기·되돌리기·삭제는 서버처럼 VM 동작 권한으로 따진다.
  expect(can("viewer", "read", "snapshot")).toBe(true);
});
```

`web/src/i18n/en.json`에서

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "internal": "The server failed: {{message}}"
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "The pool still has volumes. Delete them first.",
    "vm_busy": "The VM is starting or stopping. Try again when it has settled.",
    "snapshot_unsupported": "Only VMs whose disks are all qcow2 can take snapshots.",
    "vm_has_snapshots": "The VM has snapshots. Delete them first.",
    "volume_in_snapshot": "A VM snapshot still uses this volume. Delete that snapshot first.",
    "snapshot_not_ready": "The snapshot is still being taken.",
    "internal": "The server failed: {{message}}"
```

`web/src/i18n/en.json`에서

```json
      "volume-import": "Import volume {{target}}",
      "volume-delete": "Delete volume {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "volume-import": "Import volume {{target}}",
      "volume-delete": "Delete volume {{target}}",
      "snapshot-create": "Take snapshot of {{target}}",
      "snapshot-revert": "Revert {{target}} to a snapshot",
      "snapshot-delete": "Delete snapshot of {{target}}",
      "volume-resize": "Resize volume {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "console": "Console",
      "tasks": "Tasks",
```

을 다음으로 바꾼다:

```json
      "console": "Console",
      "snapshots": "Snapshots",
      "tasks": "Tasks",
```

`web/src/i18n/en.json`에서

```json
    "noNics": "No network interfaces."
  },
```

을 다음으로 바꾼다:

```json
    "noNics": "No network interfaces."
  },
  "snapshots": {
    "name": "Name",
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
    "unsupported": "Only VMs whose disks are all qcow2 can take snapshots. {{volume}} is {{format}}.",
    "busy": "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    "revert": "Revert",
    "revertNamed": "Revert to {{name}}",
    "revertTitle": "Revert {{vm}} to {{name}}?",
    "revertDescription": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot. It stays off.",
    "revertDescriptionMemory": "The current state of {{vm}} is lost. The VM is powered off, then its disks, firmware variables and settings return to the snapshot, and it resumes running from the saved memory.",
    "delete": "Delete",
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
    }
  },
```

`web/src/i18n/ko.json`에서

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "internal": "서버 오류: {{message}}"
```

을 다음으로 바꾼다:

```json
    "pool_in_use": "풀에 볼륨이 남아 있습니다. 먼저 볼륨을 지우세요.",
    "vm_busy": "VM이 시작하거나 정지하는 중입니다. 끝난 뒤 다시 시도하세요.",
    "snapshot_unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다.",
    "vm_has_snapshots": "VM에 스냅샷이 있습니다. 먼저 스냅샷을 지우세요.",
    "volume_in_snapshot": "VM 스냅샷이 아직 이 볼륨을 씁니다. 먼저 그 스냅샷을 지우세요.",
    "snapshot_not_ready": "아직 만드는 중인 스냅샷입니다.",
    "internal": "서버 오류: {{message}}"
```

`web/src/i18n/ko.json`에서

```json
      "volume-import": "볼륨 {{target}} 가져오기",
      "volume-delete": "볼륨 {{target}} 삭제"
    }
```

을 다음으로 바꾼다:

```json
      "volume-import": "볼륨 {{target}} 가져오기",
      "volume-delete": "볼륨 {{target}} 삭제",
      "snapshot-create": "{{target}} 스냅샷 만들기",
      "snapshot-revert": "{{target}} 스냅샷으로 되돌리기",
      "snapshot-delete": "{{target}} 스냅샷 삭제",
      "volume-resize": "볼륨 {{target}} 크기 늘리기"
    }
```

`web/src/i18n/ko.json`에서

```json
      "console": "콘솔",
      "tasks": "작업",
```

을 다음으로 바꾼다:

```json
      "console": "콘솔",
      "snapshots": "스냅샷",
      "tasks": "작업",
```

`web/src/i18n/ko.json`에서

```json
    "noNics": "네트워크 인터페이스가 없습니다."
  },
```

을 다음으로 바꾼다:

```json
    "noNics": "네트워크 인터페이스가 없습니다."
  },
  "snapshots": {
    "name": "이름",
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
    "unsupported": "디스크가 모두 qcow2인 VM만 스냅샷을 만들 수 있습니다. {{volume}}은(는) {{format}}입니다.",
    "busy": "VM이 시작하거나 정지하는 중입니다. 끝나면 스냅샷을 만들고 되돌리고 지울 수 있습니다.",
    "revert": "되돌리기",
    "revertNamed": "{{name}}(으)로 되돌리기",
    "revertTitle": "{{vm}}을(를) {{name}}(으)로 되돌릴까요?",
    "revertDescription": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌립니다. VM은 꺼진 채로 남습니다.",
    "revertDescriptionMemory": "{{vm}}의 지금 상태는 사라집니다. VM을 끈 뒤 디스크, 펌웨어 변수, 설정을 스냅샷 때로 되돌리고, 저장한 메모리에서 이어서 실행합니다.",
    "delete": "삭제",
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
    }
  },
```

`web/src/test/fixtures.ts`에서

```ts
import type { Node, StoragePool, Task, Vm, Volume } from "@/api/types";
```

을 다음으로 바꾼다:

```ts
import type { Node, Snapshot, StoragePool, Task, Vm, Volume } from "@/api/types";
```

`web/src/test/fixtures.ts`에서

```ts
    status: { phase: "ready", node: "n1", allocated_bytes: 1024 ** 3, attached_to: null, ...extra },
  };
}
```

을 다음으로 바꾼다:

```ts
    status: { phase: "ready", node: "n1", allocated_bytes: 1024 ** 3, attached_to: null, ...extra },
  };
}

/** 테스트용 스냅샷. `n`은 id 끝자리(UUIDv7처럼 만든 차례)다. 리소스 이름은 서버처럼 id다. */
export function snapshot(
  name: string,
  of: Vm,
  n: number,
  extra: { spec?: Partial<Snapshot["spec"]>; status?: Partial<Snapshot["status"]> } = {},
): Snapshot {
  const id = `019a0f3c-0000-7000-8003-${String(n).padStart(12, "0")}`;
  return {
    id,
    name: id,
    revision: 1,
    labels: {},
    spec: {
      vm: of.id,
      name,
      parent: null,
      memory: false,
      description: "",
      scheduled: false,
      vm_spec: of.spec,
      ...extra.spec,
    },
    status: { phase: "ready", created_at: 1_792_000_000, vmstate_bytes: null, ...extra.status },
  };
}
```

`web/src/test/render.tsx`에서

```tsx
  network: "/networks",
};
```

을 다음으로 바꾼다:

```tsx
  network: "/networks",
  snapshot: "/snapshots",
};
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";

const TABS = ["summary", "hardware", "console", "tasks", "json"] as const;
```

을 다음으로 바꾼다:

```tsx
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmSnapshots } from "@/vms/VmSnapshots";

const TABS = ["summary", "hardware", "console", "snapshots", "tasks", "json"] as const;
```

`web/src/vms/VmDetailPage.tsx`에서

```tsx
          )}
        </TabsContent>
```

을 다음으로 바꾼다:

```tsx
          )}
        </TabsContent>
        <TabsContent value="snapshots">
          <VmSnapshots vm={vm} />
        </TabsContent>
```

`web/src/vms/VmSnapshots.test.tsx`를 만든다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Vm } from "@/api/types";
import { formatTime } from "@/lib/format";
import { resetTasks } from "@/tasks/tasks";
import { NODE, snapshot, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(resetTasks);

function withDisk(v: Vm): Vm {
  return {
    ...v,
    spec: { ...v.spec, disks: [{ volume: "web-root", bus: "virtio-blk", boot_order: 1 }] },
  };
}

const web = withDisk(vm("web", "running"));
const base = snapshot("base", web, 1, {
  spec: { description: "fresh install", memory: true },
  status: { vmstate_bytes: 1536 * 1024 ** 2 },
});
const upgraded = snapshot("upgraded", web, 2, { spec: { parent: base.id, memory: true } });
const taking = snapshot("taking", web, 3, {
  spec: { parent: upgraded.id },
  status: { phase: "pending", created_at: null },
});
// 다른 VM의 스냅샷은 보이지 않는다.
const other = snapshot("other", vm("db", "stopped"), 4);

function open(v: Vm, as = ADMIN) {
  return renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: `/vms/${v.name}?tab=snapshots`,
    as,
  });
}

it("shows the VM's snapshots as a tree with the current position", async () => {
  const at = { ...web, status: { ...web.status, snapshot: upgraded.id } };
  mockLists({
    vm: [at],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [taking, other, upgraded, base],
  });
  open(at);
  const table = await screen.findByRole("table", { name: "Snapshots" });
  await within(table).findByText("taking");
  const [, first, second, third] = within(table).getAllByRole("row") as [
    HTMLElement,
    HTMLElement,
    HTMLElement,
    HTMLElement,
  ];
  const rows = [first, second, third];
  // 깊이는 스크린 리더에 부모 이름으로 알리고, 이름과 "현재" 표시는 붙지 않는다.
  expect(rows.map((r) => within(r).getAllByRole("cell")[0]!.textContent)).toEqual([
    "base",
    "upgraded (after base) Current",
    "taking (after upgraded)",
  ]);
  expect(first).toHaveTextContent(formatTime(new Date(1_792_000_000 * 1000), "en"));
  expect(first).toHaveTextContent("Included (1.5 GiB)");
  expect(first).toHaveTextContent("fresh install");
  expect(second).toHaveTextContent("Included");
  expect(third).toHaveTextContent("No");
  // 만드는 중인 스냅샷은 되돌리거나 지울 수 없다.
  expect(third).toHaveTextContent("Creating");
  expect(within(third).getByRole("button", { name: "Revert to taking" })).toBeDisabled();
  expect(within(third).getByRole("button", { name: "Delete snapshot taking" })).toBeDisabled();
  expect(within(first).getByRole("button", { name: "Revert to base" })).toBeEnabled();
});

it("takes a memory snapshot of a running VM and follows its task", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).getByLabelText("Include memory")).toBeChecked();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(within(dialog).getByText("This VM already has a snapshot with this name.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "before-upgrade");
  await userEvent.type(within(dialog).getByLabelText("Description"), "kernel 6.12");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot before-upgrade of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "before-upgrade", description: "kernel 6.12", memory: true });
});

it("offers only a disk snapshot of a stopped VM", async () => {
  const off = withDisk(vm("web", "stopped"));
  mockLists({ vm: [off], node: [NODE], volume: [volume("web-root", "local")] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${off.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", off.id), { status: 202 });
    }),
  );
  open(off);
  expect(await screen.findByText("No snapshots.")).toBeInTheDocument();
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).queryByLabelText("Include memory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot base of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "base", description: "", memory: false });
  await waitFor(() => expect(screen.getByRole("button", { name: "Take snapshot" })).toHaveFocus());
});

it("takes only a disk snapshot of a running VM without disks", async () => {
  const diskless = vm("web", "running");
  mockLists({ vm: [diskless], node: [NODE], volume: [] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${diskless.id}/snapshots`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "snapshot-create", diskless.id), { status: 202 });
    }),
  );
  open(diskless);
  await userEvent.click(await screen.findByRole("button", { name: "Take snapshot" }));
  const dialog = await screen.findByRole("dialog", { name: "Take a snapshot of web" });
  expect(within(dialog).queryByLabelText("Include memory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("Name"), "base");
  await userEvent.click(within(dialog).getByRole("button", { name: "Take snapshot" }));
  expect(await screen.findByText("Take snapshot base of web")).toBeInTheDocument();
  expect(body).toEqual({ name: "base", description: "", memory: false });
});

it("says why a snapshot cannot be taken before the volumes are read", async () => {
  mockLists({ vm: [web], node: [NODE], snapshot: [base] });
  server.use(
    http.get(apiUrl("/volumes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  open(web);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});

it("says why a VM with a raw disk cannot take snapshots", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [
      { ...volume("web-root", "local"), spec: { pool: "local", size_bytes: 1, format: "raw" } },
    ],
  });
  open(web);
  expect(
    await screen.findByText(
      "Only VMs whose disks are all qcow2 can take snapshots. web-root is raw.",
    ),
  ).toBeVisible();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
});

it("says why snapshots wait while the VM is starting", async () => {
  const starting = withDisk(vm("web", "starting"));
  mockLists({
    vm: [starting],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [snapshot("base", starting, 1)],
  });
  open(starting);
  expect(
    await screen.findByText(
      "The VM is starting or stopping. Snapshots can be taken, reverted and deleted once it settles.",
    ),
  ).toBeVisible();
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
  expect(await screen.findByRole("button", { name: "Revert to base" })).toBeDisabled();
});

it("reverts after warning that the current state is lost", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base, upgraded],
  });
  let reverted = false;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/snapshots/${upgraded.id}/revert`), () => {
      reverted = true;
      return HttpResponse.json(task("t1", "snapshot-revert", web.id), { status: 202 });
    }),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Revert to upgraded" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Revert web to upgraded?" });
  expect(dialog).toHaveTextContent("The current state of web is lost.");
  expect(dialog).toHaveTextContent("resumes running from the saved memory");
  await userEvent.click(within(dialog).getByRole("button", { name: "Revert" }));
  expect(await screen.findByText("Revert web to upgraded")).toBeInTheDocument();
  expect(reverted).toBe(true);
  // 연 버튼으로 포커스가 돌아온다.
  const trigger = screen.getByRole("button", { name: "Revert to upgraded" });
  await waitFor(() => expect(trigger).toHaveFocus());
});

it("deletes a snapshot after confirming and shows why it could not", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  server.use(
    http.delete(apiUrl(`/vms/${web.id}/snapshots/${base.id}`), () =>
      HttpResponse.json({ code: "vm_busy", message: "busy" }, { status: 409 }),
    ),
  );
  open(web);
  await userEvent.click(await screen.findByRole("button", { name: "Delete snapshot base" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete snapshot base?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The VM is starting or stopping. Try again when it has settled.",
  );

  server.use(
    http.delete(apiUrl(`/vms/${web.id}/snapshots/${base.id}`), () =>
      HttpResponse.json(task("t1", "snapshot-delete", web.id), { status: 202 }),
    ),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete snapshot base of web")).toBeInTheDocument();
});

it("shows snapshots without actions to viewers", async () => {
  mockLists({
    vm: [web],
    node: [NODE],
    volume: [volume("web-root", "local")],
    snapshot: [base],
  });
  open(web, { ...ADMIN, role: "viewer" });
  expect(await screen.findByText("base")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Take snapshot" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Revert to base" })).not.toBeInTheDocument();
});

it("says why the snapshots could not be read", async () => {
  mockLists({ vm: [web], node: [NODE], volume: [volume("web-root", "local")] });
  server.use(
    http.get(apiUrl("/snapshots"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  open(web);
  const table = await screen.findByRole("table", { name: "Snapshots" });
  expect(
    await within(table).findByText("You do not have permission to do this."),
  ).toBeInTheDocument();
  // 목록을 모르면 이름이 겹치는지 모르므로 만들 수 없다.
  expect(screen.getByRole("button", { name: "Take snapshot" })).toBeDisabled();
});
```

`web/src/vms/VmSnapshots.tsx`를 만든다:

```tsx
import { Camera } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Snapshot, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
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
import { formatBytes, formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";

/** VM 상세의 스냅샷 탭: 나무 목록과 만들기·되돌리기·삭제 */
export function VmSnapshots({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const snapshots = useList("snapshot");
  const volumes = useList("volume");
  const [creating, setCreating] = useState(false);
  // 닫히는 동안에도 대상과 동작을 남겨 두어 대화상자의 글이 바뀌지 않게 한다.
  const [confirm, setConfirm] = useState<{
    action: "revert" | "delete";
    snapshot: Snapshot;
  } | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const operate = can(me.role, "operate", "vm");
  const mine = (snapshots.data ?? []).filter((s) => s.spec.vm === vm.id);
  const rows = snapshotTree(mine);
  const names = new Map(mine.map((s) => [s.id, s.spec.name]));
  // 서버는 시작·정지 중인 VM의 스냅샷 작업을 받지 않는다(vm_busy).
  const busy = vm.status.phase === "starting" || vm.status.phase === "stopping";
  // qcow2가 아닌 디스크가 있으면 만들 수 없다(snapshot_unsupported). 볼륨 목록을 다 읽어야 안다.
  const unsupported = volumes.isSuccess
    ? (vm.spec.disks ?? [])
        .map((d) => volumes.data.find((v) => v.name === d.volume))
        .find((v) => v !== undefined && v.spec.format !== "qcow2")
    : undefined;
  const columns = operate ? 5 : 4;

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
          (unsupported ? (
            <p className="text-sm text-muted-foreground">
              {t("snapshots.unsupported", {
                volume: unsupported.name,
                format: unsupported.spec.format,
              })}
            </p>
          ) : (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              {/* 볼륨 목록을 읽어야 디스크 형식을 알아 만들 수 있다. */}
              {!volumes.isSuccess && <ListPending error={volumes.error} />}
              <Button
                size="sm"
                disabled={busy || !snapshots.isSuccess || !volumes.isSuccess}
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
      <Table aria-labelledby="vm-snapshots">
        <TableHeader>
          <TableRow>
            <TableHead>{t("snapshots.name")}</TableHead>
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
                    <Button
                      variant="outline"
                      size="sm"
                      aria-label={t("snapshots.revertNamed", { name })}
                      disabled={busy || !ready}
                      onClick={(e) => ask(e, "revert", snapshot)}
                    >
                      {t("snapshots.revert")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="ml-2"
                      aria-label={t("snapshots.deleteNamed", { name })}
                      disabled={busy || !ready}
                      onClick={(e) => ask(e, "delete", snapshot)}
                    >
                      {t("snapshots.delete")}
                    </Button>
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
        confirmLabel={t("snapshots.delete")}
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
  returnFocus,
  onClose,
}: {
  vm: Vm;
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [memory, setMemory] = useState(true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 서버는 실행 중이고 디스크가 있는 VM만 메모리를 담는다. 그 밖에는 묻지 않고 디스크만 담는다.
  const canMemory = vm.status.phase === "running" && (vm.spec.disks ?? []).length > 0;

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
      <Field id="snapshot-name" label={t("snapshots.name")} error={errors.name}>
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
    </FormDialog>
  );
}
```

`web/src/vms/snapshots.test.ts`를 만든다:

```ts
import { snapshot, vm } from "@/test/fixtures";
import { descriptionError, snapshotNameError, snapshotTree } from "@/vms/snapshots";

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
```

`web/src/vms/snapshots.ts`를 만든다:

```ts
import type { Snapshot } from "@/api/types";
import { validName } from "@/vms/form";

/** 서버의 `MAX_DESCRIPTION_CHARS`(문자 수) */
const MAX_DESCRIPTION_CHARS = 1024;

/** 나무 목록의 한 줄: 스냅샷과 그 깊이(뿌리가 0) */
export interface TreeRow {
  snapshot: Snapshot;
  depth: number;
}

/**
 * 한 VM의 스냅샷을 나무 차례로 편다. 형제는 id(UUIDv7) 차례, 곧 만든 차례다. 목록에 없는 부모를
 * 가리키는 스냅샷은 뿌리로 둔다.
 */
export function snapshotTree(snapshots: readonly Snapshot[]): TreeRow[] {
  const ids = new Set(snapshots.map((s) => s.id));
  const sorted = [...snapshots].sort((a, b) => (a.id < b.id ? -1 : 1));
  const children = new Map<string | null, Snapshot[]>();
  for (const s of sorted) {
    const parent = s.spec.parent && ids.has(s.spec.parent) ? s.spec.parent : null;
    children.set(parent, [...(children.get(parent) ?? []), s]);
  }
  const rows: TreeRow[] = [];
  const seen = new Set<string>();
  const visit = (parent: string | null, depth: number) => {
    for (const s of children.get(parent) ?? []) {
      if (!seen.has(s.id)) {
        seen.add(s.id);
        rows.push({ snapshot: s, depth });
        visit(s.id, depth + 1);
      }
    }
  };
  visit(null, 0);
  // 부모가 돌고 도는(망가진) 스냅샷은 뿌리에서 닿지 않으므로 남은 것을 뿌리로 더한다.
  for (const s of sorted) {
    if (!seen.has(s.id)) {
      seen.add(s.id);
      rows.push({ snapshot: s, depth: 0 });
      visit(s.id, 1);
    }
  }
  return rows;
}

/** 새 스냅샷 이름의 입력 오류(번역 키). `taken`은 이 VM에 이미 있는 스냅샷 이름이다. */
export function snapshotNameError(name: string, taken: readonly string[]): string | undefined {
  if (!validName(name)) {
    return "forms.name";
  }
  if (taken.includes(name)) {
    return "snapshots.errors.exists";
  }
  return undefined;
}

/** 설명의 입력 오류(번역 키). 서버처럼 바이트가 아니라 문자 수로 센다. */
export function descriptionError(description: string): string | undefined {
  return [...description].length > MAX_DESCRIPTION_CHARS
    ? "snapshots.errors.description"
    : undefined;
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 191개 (`VmSnapshots.test.tsx`, `snapshots.test.ts` 포함)

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): VM 상세 스냅샷 탭"
```

---

### Task 12: 웹: 볼륨 크기 늘리기와 VM 삭제

볼륨 화면의 "크기 늘리기"(지금 크기 표시, GiB 입력을 1 MiB로 올려 지금보다 커야, 준비 안 된 볼륨은 막고 까닭). VM 삭제는 `VmDeleteDialog` 하나로: 스냅샷이 있으면 "스냅샷도 모두 지우기"를 골라야 하고, 만드는 중인 스냅샷이 있으면 막는다. 고르면 그때의 목록을 고정해 하나씩 지우고 기다린 뒤 VM을 지우며, 화면을 떠나면 멈춘다. `ConfirmDialog`에 본문과 확인 막기를 더한다.

**Files:**
- Create: `web/src/vms/VmDeleteDialog.tsx`
- Modify: `web/src/components/ConfirmDialog.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/storage/VolumesPage.test.tsx`
- Modify: `web/src/storage/VolumesPage.tsx`
- Modify: `web/src/vms/VmDetailPage.test.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`
- Modify: `web/src/vms/VmMenu.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/components/ConfirmDialog.tsx`에서

```tsx
  typeToConfirm?: string;
  /** 닫으면 포커스를 돌려줄 곳(메뉴에서 열었으면 메뉴 버튼) */
```

을 다음으로 바꾼다:

```tsx
  typeToConfirm?: string;
  /** 확인 버튼을 막는다. 까닭은 `children`에 글로 보여 준다. */
  confirmDisabled?: boolean;
  /** 확인 전에 고를 것이나 알릴 것(요청 중에는 막는다) */
  children?: React.ReactNode;
  /** 닫으면 포커스를 돌려줄 곳(메뉴에서 열었으면 메뉴 버튼) */
```

`web/src/components/ConfirmDialog.tsx`에서

```tsx
        )}
        {error && (
```

을 다음으로 바꾼다:

```tsx
        )}
        {props.children && (
          <fieldset disabled={busy} className="flex min-w-0 flex-col gap-2">
            {props.children}
          </fieldset>
        )}
        {error && (
```

`web/src/components/ConfirmDialog.tsx`에서

```tsx
          <AlertDialogCancel disabled={busy}>{t("confirm.cancel")}</AlertDialogCancel>
          <Button variant="destructive" disabled={!ready || busy} onClick={() => void confirm()}>
            {props.confirmLabel}
```

을 다음으로 바꾼다:

```tsx
          <AlertDialogCancel disabled={busy}>{t("confirm.cancel")}</AlertDialogCancel>
          <Button
            variant="destructive"
            disabled={!ready || busy || props.confirmDisabled}
            onClick={() => void confirm()}
          >
            {props.confirmLabel}
```

`web/src/i18n/en.json`에서

```json
    "deleteDescription": "The VM is deleted. Its volumes are kept.",
    "phase": {
```

을 다음으로 바꾼다:

```json
    "deleteDescription": "The VM is deleted. Its volumes are kept.",
    "deleteSnapshots_one": "Also delete its snapshot",
    "deleteSnapshots_other": "Also delete all {{count}} of its snapshots",
    "hasSnapshots": "A VM with snapshots cannot be deleted. Delete its snapshots first, or check the box to delete them along with the VM.",
    "snapshotsPending": "A snapshot of this VM is still being taken. Delete the VM once it is done.",
    "deletingSnapshots": "Deleting the snapshots one at a time. This cannot be cancelled once started.",
    "phase": {
```

`web/src/i18n/en.json`에서

```json
    "inUse": "Remove it from the VM first.",
    "errors": {
      "reserved": "The name import is reserved.",
      "exists": "A volume with this name already exists."
    }
```

을 다음으로 바꾼다:

```json
    "inUse": "Remove it from the VM first.",
    "resize": "Resize",
    "resizeNamed": "Resize {{name}}",
    "resizeTitle": "Resize volume {{name}}",
    "resizeDescription": "Volumes can only grow. A running VM sees the new size right away; grow the partition and file system inside the guest yourself.",
    "currentSize": "Current size: {{size}}",
    "newSize": "New size (GiB)",
    "notReady": "Available once ready.",
    "errors": {
      "reserved": "The name import is reserved.",
      "exists": "A volume with this name already exists.",
      "grow": "Enter a size larger than the current size."
    }
```

`web/src/i18n/ko.json`에서

```json
    "deleteDescription": "VM을 삭제합니다. 볼륨은 남습니다.",
    "phase": {
```

을 다음으로 바꾼다:

```json
    "deleteDescription": "VM을 삭제합니다. 볼륨은 남습니다.",
    "deleteSnapshots_one": "스냅샷 {{count}}개도 모두 지우기",
    "deleteSnapshots_other": "스냅샷 {{count}}개도 모두 지우기",
    "hasSnapshots": "스냅샷이 있는 VM은 지울 수 없습니다. 먼저 스냅샷을 지우거나, 위 칸을 골라 VM과 함께 지우세요.",
    "snapshotsPending": "이 VM의 스냅샷을 만드는 중입니다. 다 만든 뒤 VM을 지우세요.",
    "deletingSnapshots": "스냅샷을 하나씩 지우는 중입니다. 시작한 뒤에는 취소할 수 없습니다.",
    "phase": {
```

`web/src/i18n/ko.json`에서

```json
    "inUse": "먼저 VM에서 빼세요.",
    "errors": {
      "reserved": "import는 예약된 이름입니다.",
      "exists": "같은 이름의 볼륨이 이미 있습니다."
    }
```

을 다음으로 바꾼다:

```json
    "inUse": "먼저 VM에서 빼세요.",
    "resize": "크기 늘리기",
    "resizeNamed": "{{name}} 크기 늘리기",
    "resizeTitle": "볼륨 {{name}} 크기 늘리기",
    "resizeDescription": "볼륨은 늘리기만 할 수 있습니다. 실행 중인 VM은 바로 새 크기를 봅니다. 게스트 안의 파티션과 파일 시스템은 직접 늘리세요.",
    "currentSize": "지금 크기: {{size}}",
    "newSize": "새 크기(GiB)",
    "notReady": "준비되면 쓸 수 있습니다.",
    "errors": {
      "reserved": "import는 예약된 이름입니다.",
      "exists": "같은 이름의 볼륨이 이미 있습니다.",
      "grow": "지금 크기보다 큰 값을 넣으세요."
    }
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { volumeNameError } from "@/storage/volumes";
```

을 다음으로 바꾼다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { List } from "@/api/live";
import type { Volume } from "@/api/types";
import { volumeNameError } from "@/storage/volumes";
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  expect(within(img).getByRole("button", { name: "Delete img" })).toBeDisabled();
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(img).getByRole("button", { name: "Delete img" })).toBeDisabled();
  expect(within(img).getByRole("button", { name: "Resize img" })).toBeDisabled();
  expect(img).toHaveTextContent("Available once ready.");
  expect(within(root).getByRole("button", { name: "Resize root" })).toBeEnabled();
  expect(root).not.toHaveTextContent("Available once ready.");
});
```

`web/src/storage/VolumesPage.test.tsx`에서

```tsx
  expect(screen.queryByRole("button", { name: "Delete data" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("button", { name: "Delete data" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Resize data" })).not.toBeInTheDocument();
});

it("grows a volume and follows its task", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t3", "volume-resize", data.id), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  const trigger = await screen.findByRole("button", { name: "Resize data" });
  await userEvent.click(trigger);
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  expect(dialog).toHaveTextContent("Current size: 16 GiB");
  // 지금 크기와 같거나 작으면 보내지 않는다.
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "16");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(within(dialog).getByLabelText("New size (GiB)")).toHaveAccessibleDescription(
    "Enter a size larger than the current size.",
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("New size (GiB)"));
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "20.5");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await screen.findByText("Resize volume data")).toBeInTheDocument();
  expect(body).toEqual({ size_bytes: 20.5 * 1024 ** 3 });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await waitFor(() => expect(trigger).toHaveFocus());
});

it("grows to the latest size rounded up to a whole MiB", async () => {
  const GIB = 1024 ** 3;
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t3", "volume-resize", data.id), { status: 202 });
    }),
  );
  const { queryClient } = renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Resize data" }));
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  expect(dialog).toHaveTextContent("Current size: 16 GiB");
  // 열어 둔 사이 크기가 바뀌면 지금 크기를 새로 보이고 그것과 비교한다.
  const grown = { ...data, spec: { ...data.spec, size_bytes: 20 * GIB + 512 } };
  queryClient.setQueryData<List<Volume>>(["list", "volume"], { revision: 2, items: [grown] });
  expect(await within(dialog).findByText("Current size: 20 GiB")).toBeVisible();
  // 20 GiB + 107 바이트는 지금보다 작지만 1 MiB 단위로 올리면 크다(qcow2는 512 배수만 받는다).
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "20.0000001");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await screen.findByText("Resize volume data")).toBeInTheDocument();
  expect(body).toEqual({ size_bytes: 20 * GIB + 1024 ** 2 });
});

it("keeps the resize dialog open when the server refuses", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Resize data" }));
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "32");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});
```

`web/src/storage/VolumesPage.tsx` 전체:

```tsx
import { Maximize2, Plus, Trash2, Upload } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import type { Volume } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
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
import { formatBytes } from "@/lib/format";
import { UploadDialog } from "@/storage/UploadDialog";
import { volumeNameError } from "@/storage/volumes";
import { trackTask } from "@/tasks/tasks";
import { gibToBytes } from "@/vms/form";

const MIB = 1024 ** 2;

export function VolumesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const volumes = useList("volume");
  const vms = useList("vm");
  const nodes = useList("node");
  const [creating, setCreating] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [deleting, setDeleting] = useState<Volume | null>(null);
  // 볼륨 id. 대화상자는 살아 있는 목록에서 찾아 지금 크기를 최신으로 보인다.
  const [resizing, setResizing] = useState<string | null>(null);
  const resizingVolume = volumes.data?.find((v) => v.id === resizing);
  // 트리거 없이 연 대화상자는 닫힐 때 포커스를 돌려줄 곳을 모르므로 연 버튼을 기억한다.
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "volume");
  // 서버는 VM spec의 디스크로 사용 중인지 판단한다(`attached_to`는 마지막으로 실행한 VM이다).
  const userOf = (volume: Volume) =>
    vms.data?.find((vm) => vm.spec.disks?.some((d) => d.volume === volume.name));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="volumes-title" className="text-2xl font-semibold">
          {t("nav.volumes")}
        </h1>
        {write && (
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setUploading(true)}>
              <Upload />
              {t("uploads.open")}
            </Button>
            <Button onClick={() => setCreating(true)}>
              <Plus />
              {t("volumes.create")}
            </Button>
          </div>
        )}
      </div>
      <Table aria-labelledby="volumes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("volumes.name")}</TableHead>
            <TableHead>{t("volumes.pool")}</TableHead>
            <TableHead>{t("volumes.size")}</TableHead>
            <TableHead>{t("volumes.allocated")}</TableHead>
            <TableHead>{t("volumes.format")}</TableHead>
            <TableHead>{t("volumes.state")}</TableHead>
            <TableHead>{t("volumes.node")}</TableHead>
            <TableHead>{t("volumes.vm")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(volumes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={volumes} empty={t("volumes.empty")} />
          )}
          {(volumes.data ?? []).map((volume) => {
            const vm = userOf(volume);
            const ready = volume.status.phase === "ready";
            const node = nodes.data?.find((n) => n.id === volume.status.node);
            return (
              <TableRow key={volume.id}>
                <TableCell className="font-medium">{volume.name}</TableCell>
                <TableCell>{volume.spec.pool}</TableCell>
                {/* 가져오는 볼륨은 끝나기 전까지 크기를 모른다. */}
                <TableCell>
                  {volume.spec.size_bytes > 0 ? formatBytes(volume.spec.size_bytes) : "—"}
                </TableCell>
                <TableCell>
                  {volume.status.allocated_bytes !== null
                    ? formatBytes(volume.status.allocated_bytes)
                    : "—"}
                </TableCell>
                <TableCell>{volume.spec.format}</TableCell>
                <TableCell>
                  <Badge variant={ready ? "default" : "secondary"}>
                    {t(`volumes.phase.${volume.status.phase}`)}
                  </Badge>
                </TableCell>
                <TableCell>{node?.name ?? volume.status.node?.slice(-8) ?? "—"}</TableCell>
                <TableCell>
                  {vm ? (
                    <>
                      <Link
                        to={`/vms/${encodeURIComponent(vm.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {vm.name}
                      </Link>
                      {write && (
                        <div className="text-xs text-muted-foreground">{t("volumes.inUse")}</div>
                      )}
                    </>
                  ) : (
                    "—"
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right">
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.resizeNamed", { name: volume.name })}
                      // 만드는 중인 볼륨은 서버가 크기를 바꾸지 않는다(volume_not_ready).
                      disabled={!ready}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setResizing(volume.id);
                      }}
                    >
                      <Maximize2 />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("volumes.deleteNamed", { name: volume.name })}
                      // 서버는 VM이 쓰거나 아직 만드는 볼륨을 지우지 않는다. VM 목록을 다 읽어야 안다.
                      disabled={!vms.isSuccess || vm !== undefined || !ready}
                      onClick={() => setDeleting(volume)}
                    >
                      <Trash2 />
                    </Button>
                    {!ready && (
                      <div className="text-xs text-muted-foreground">{t("volumes.notReady")}</div>
                    )}
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {creating && <CreateVolumeDialog onClose={() => setCreating(false)} />}
      {uploading && <UploadDialog onClose={() => setUploading(false)} />}
      {resizingVolume && (
        <ResizeVolumeDialog
          volume={resizingVolume}
          returnFocus={opener}
          onClose={() => setResizing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("volumes.deleteTitle", { name: deleting?.name })}
        description={t("volumes.deleteDescription")}
        confirmLabel={t("volumes.delete")}
        typeToConfirm={deleting?.name}
        onConfirm={async () => {
          const volume = deleting!;
          const task = await call(
            api.DELETE("/volumes/{key}", { params: { path: { key: volume.id } } }),
          );
          trackTask(task, t("tasks.op.volume-delete", { target: volume.name }));
        }}
      />
    </div>
  );
}

function CreateVolumeDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const [name, setName] = useState("");
  const [pool, setPool] = useState("");
  const [sizeGib, setSizeGib] = useState("16");
  const [format, setFormat] = useState<components["schemas"]["ImageFormat"]>("qcow2");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    const nameError = volumeNameError(name, volumes.data?.map((v) => v.name) ?? []);
    if (nameError) {
      found.name = nameError;
    }
    if (!pool) {
      found.pool = "vmForm.errors.pool";
    }
    const size = gibToBytes(sizeGib);
    if (size === undefined) {
      found.size = "vmForm.errors.size";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const task = await call(
      api.POST("/volumes", {
        body: { name, spec: { pool, size_bytes: size!, format } },
      }),
    );
    trackTask(task, t("tasks.op.volume-create", { target: name }));
  }

  return (
    <FormDialog
      title={t("volumes.createTitle")}
      description={t("volumes.createDescription")}
      submitLabel={t("volumes.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="volume-name" label={t("volumes.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="volume-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      {!pools.isSuccess && <ListPending error={pools.error} />}
      <Field id="volume-pool" label={t("volumes.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="volume-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">—</NativeSelectOption>
            {(pools.data ?? []).map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      <div className="grid grid-cols-2 gap-4">
        <Field id="volume-size" label={t("vmForm.sizeGib")} error={errors.size}>
          {(a11y) => (
            <Input
              id="volume-size"
              inputMode="decimal"
              value={sizeGib}
              onChange={(e) => setSizeGib(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field id="volume-format" label={t("volumes.format")}>
          <NativeSelect
            id="volume-format"
            className="w-full"
            value={format}
            onChange={(e) => setFormat(e.target.value as typeof format)}
          >
            <NativeSelectOption value="qcow2">qcow2</NativeSelectOption>
            <NativeSelectOption value="raw">raw</NativeSelectOption>
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/** 볼륨 크기 늘리기. 서버는 지금보다 크고 512 바이트 배수인 크기만 받는다. */
function ResizeVolumeDialog({
  volume,
  returnFocus,
  onClose,
}: {
  volume: Volume;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [sizeGib, setSizeGib] = useState("");
  const [error, setError] = useState<string>();

  async function submit() {
    // qcow2는 512 배수가 아닌 크기로 늘리지 못하므로 1 MiB 단위로 올린 값을 비교하고 보낸다.
    const bytes = gibToBytes(sizeGib);
    const size = bytes === undefined ? undefined : Math.ceil(bytes / MIB) * MIB;
    const found =
      size === undefined
        ? "vmForm.errors.size"
        : size <= volume.spec.size_bytes
          ? "volumes.errors.grow"
          : undefined;
    setError(found);
    if (found) {
      return false;
    }
    const task = await call(
      api.POST("/volumes/{key}/resize", {
        params: { path: { key: volume.id } },
        body: { size_bytes: size! },
      }),
    );
    trackTask(task, t("tasks.op.volume-resize", { target: volume.name }));
  }

  return (
    <FormDialog
      title={t("volumes.resizeTitle", { name: volume.name })}
      description={t("volumes.resizeDescription")}
      submitLabel={t("volumes.resize")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <p className="text-sm">
        {t("volumes.currentSize", { size: formatBytes(volume.spec.size_bytes) })}
      </p>
      <Field id="volume-new-size" label={t("volumes.newSize")} error={error}>
        {(a11y) => (
          <Input
            id="volume-new-size"
            inputMode="decimal"
            value={sizeGib}
            onChange={(e) => setSizeGib(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}
```

`web/src/vms/VmDeleteDialog.tsx`를 만든다:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { type RefObject, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Snapshot, Vm } from "@/api/types";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { trackTask, waitForTask } from "@/tasks/tasks";

/**
 * VM 삭제 확인. 열 때만 그린다. 서버는 스냅샷이 남은 VM을 지우지 않으므로(vm_has_snapshots), 스냅샷이
 * 있으면 함께 지울지 고르게 하고, 고르면 스냅샷 삭제 Task를 하나씩 기다린 뒤 VM을 지운다.
 */
export function VmDeleteDialog({
  vm,
  onClose,
  onRemove,
  afterRemove,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  onRemove(vm: Vm): Promise<void>;
  afterRemove?(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const snapshots = useList("snapshot");
  const [withSnapshots, setWithSnapshots] = useState(false);
  // 확인을 누를 때의 스냅샷. 지우는 동안 목록에서 빠져도 이것으로 그린다.
  const [deleting, setDeleting] = useState<Snapshot[] | null>(null);
  const checkboxId = useId();
  // 대화상자가 사라지면(다른 화면으로 가거나 로그아웃) 스냅샷 삭제를 그만 기다리고 VM도 지우지 않는다.
  const abort = useRef<AbortController | null>(null);
  useEffect(() => () => abort.current?.abort(), []);
  const mine = (snapshots.data ?? []).filter((s) => s.spec.vm === vm.id);
  // 만드는 중인 스냅샷은 지울 수 없다(snapshot_not_ready). 끝나기를 기다리게 한다.
  const pending = mine.some((s) => s.status.phase !== "ready");
  const shown = deleting ?? mine;

  async function confirm() {
    const controller = new AbortController();
    abort.current = controller;
    const targets = withSnapshots ? mine : [];
    setDeleting(targets);
    try {
      for (const snapshot of targets) {
        const task = await call(
          api.DELETE("/vms/{key}/snapshots/{name}", {
            params: { path: { key: vm.id, name: snapshot.id } },
          }),
        );
        trackTask(task, t("snapshots.op.delete", { name: snapshot.spec.name, vm: vm.name }));
        await waitForTask(queryClient, task.id, controller.signal);
      }
      await onRemove(vm);
      afterRemove?.();
    } finally {
      // 실패하면 다시 시도할 때 그때의 목록을 쓴다.
      setDeleting(null);
    }
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("vms.deleteTitle", { name: vm.name })}
      description={t("vms.deleteDescription")}
      confirmLabel={t("vms.action.delete")}
      typeToConfirm={vm.name}
      returnFocus={returnFocus}
      // 스냅샷 목록을 다 읽어야 스냅샷이 있는지 안다.
      confirmDisabled={!snapshots.isSuccess || pending || (mine.length > 0 && !withSnapshots)}
      onConfirm={confirm}
    >
      {!snapshots.isSuccess && <ListPending error={snapshots.error} />}
      {shown.length > 0 && (
        <>
          <div className="flex items-center gap-2">
            <Checkbox
              id={checkboxId}
              checked={withSnapshots}
              onCheckedChange={(v) => setWithSnapshots(v === true)}
            />
            <Label htmlFor={checkboxId}>{t("vms.deleteSnapshots", { count: shown.length })}</Label>
          </div>
          {!withSnapshots && (
            <p className="text-sm text-muted-foreground">{t("vms.hasSnapshots")}</p>
          )}
          {deleting === null && pending && (
            <p className="text-sm text-muted-foreground">{t("vms.snapshotsPending")}</p>
          )}
          {deleting !== null && deleting.length > 0 && (
            <p className="text-sm text-muted-foreground">{t("vms.deletingSnapshots")}</p>
          )}
        </>
      )}
    </ConfirmDialog>
  );
}
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NODE, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NODE, snapshot, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/vms/VmDetailPage.test.tsx`에서

```tsx

it("hides the console from users who cannot operate VMs", async () => {
```

을 다음으로 바꾼다:

```tsx

it("stops deleting snapshots when the page goes away", async () => {
  const db = vm("db", "stopped");
  mockLists({ vm: [db], node: [NODE], snapshot: [snapshot("a", db, 1), snapshot("b", db, 2)] });
  const removed: string[] = [];
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/:name`), ({ params }) => {
      removed.push(String(params.name));
      return HttpResponse.json(task(`t-${removed.length}`, "snapshot-delete", db.id), {
        status: 202,
      });
    }),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient, unmount } = renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/db",
  });
  await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: "Also delete all 2 of its snapshots" }),
  );
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(removed).toHaveLength(1));
  await waitFor(() => expect(queryClient.getQueryCache().hasListeners()).toBe(true));
  unmount();
  // 떠난 뒤 Task가 끝나도 다음 스냅샷과 VM을 지우지 않는다.
  const done = { ...task("t-1", "snapshot-delete", db.id), status: { state: "succeeded" } };
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [done] });
  await new Promise((r) => setTimeout(r, 50));
  expect(removed).toHaveLength(1);
  expect(deleted).toBe(false);
});

it("hides the console from users who cannot operate VMs", async () => {
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
```

을 다음으로 바꾼다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx

import { task, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx

import type { List } from "@/api/live";
import type { Task } from "@/api/types";
import { snapshot, task, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(confirm);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});
```

을 다음으로 바꾼다:

```tsx
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  // 스냅샷이 없으면 고를 것이 없다.
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
  await userEvent.click(confirm);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("deletes the snapshots one by one before the VM", async () => {
  const first = snapshot("before", db, 1);
  const second = snapshot("after", db, 2, { spec: { parent: first.id } });
  mockLists({ vm: [db], node: [NODE], snapshot: [first, second, snapshot("x", web, 3)] });
  const removed: string[] = [];
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/:name`), ({ params }) => {
      removed.push(String(params.name));
      return HttpResponse.json(task(`t-${String(params.name)}`, "snapshot-delete", db.id), {
        status: 202,
      });
    }),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  const succeed = (...ids: string[]) =>
    queryClient.setQueryData<List<Task>>(["list", "task"], {
      revision: 2,
      items: ids.map((id) => ({
        ...task(`t-${id}`, "snapshot-delete", db.id),
        status: { state: "succeeded", error: null },
      })),
    });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  // 스냅샷이 남으면 서버가 지우지 않으므로 고르기 전에는 막고 까닭을 보인다.
  expect(confirm).toBeDisabled();
  expect(dialog).toHaveTextContent("A VM with snapshots cannot be deleted.");
  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: "Also delete all 2 of its snapshots" }),
  );
  expect(dialog).not.toHaveTextContent("A VM with snapshots cannot be deleted.");
  await userEvent.click(confirm);

  // 앞 스냅샷의 Task가 끝나야 다음을 지우고, 모두 끝나야 VM을 지운다.
  await waitFor(() => expect(removed).toEqual([first.id]));
  expect(await screen.findByText("Delete snapshot before of db")).toBeInTheDocument();
  // 다음 DELETE를 함께 보내지 않았다.
  expect(removed).toEqual([first.id]);
  expect(dialog).toHaveTextContent("This cannot be cancelled once started.");
  // 지운 스냅샷이 목록에서 빠져도 누를 때의 수를 그대로 보인다.
  queryClient.setQueryData(["list", "snapshot"], { revision: 3, items: [second] });
  await new Promise((r) => setTimeout(r, 20));
  expect(dialog).toHaveTextContent("Also delete all 2 of its snapshots");
  succeed(first.id);
  await waitFor(() => expect(removed).toEqual([first.id, second.id]));
  expect(deleted).toBe(false);
  succeed(first.id, second.id);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
  await waitFor(() => expect(screen.getByRole("button", { name: "Actions for db" })).toHaveFocus());
});

it("waits for a snapshot being taken before deleting", async () => {
  const taking = snapshot("taking", db, 1, { status: { phase: "pending", created_at: null } });
  mockLists({ vm: [db], node: [NODE], snapshot: [taking] });
  const sent: string[] = [];
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/*`), ({ request }) => {
      sent.push(request.url);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Also delete its snapshot" }));
  expect(dialog).toHaveTextContent("A snapshot of this VM is still being taken.");
  expect(within(dialog).getByRole("button", { name: "Delete" })).toBeDisabled();
  expect(sent).toEqual([]);
});

it("keeps the VM when a snapshot cannot be deleted", async () => {
  const only = snapshot("before", db, 1);
  mockLists({ vm: [db], node: [NODE], snapshot: [only] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/${only.id}`), () =>
      HttpResponse.json(task("t-s", "snapshot-delete", db.id), { status: 202 }),
    ),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Also delete its snapshot" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(queryClient.getQueryCache().hasListeners()).toBe(true));
  queryClient.setQueryData<List<Task>>(["list", "task"], {
    revision: 2,
    items: [
      { ...task("t-s", "snapshot-delete", db.id), status: { state: "failed", error: "disk busy" } },
    ],
  });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("disk busy");
  expect(deleted).toBe(false);
  expect(dialog).toBeVisible();
});

it("waits for the snapshots before offering a delete", async () => {
  mockLists({ vm: [db], node: [NODE] });
  server.use(
    http.get(apiUrl("/snapshots"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(within(dialog).getByRole("button", { name: "Delete" })).toBeDisabled();
});
```

`web/src/vms/VmMenu.tsx`에서

```tsx
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
```

을 다음으로 바꾼다:

```tsx
import { ACTIONS, canDelete, canRun, useVmActions, type VmAction } from "@/vms/actions";
import { VmDeleteDialog } from "@/vms/VmDeleteDialog";
```

`web/src/vms/VmMenu.tsx`에서

```tsx
      />
      <ConfirmDialog
        open={which === "delete"}
        onOpenChange={(open) => !open && onClose()}
        title={t("vms.deleteTitle", { name: vm.name })}
        description={t("vms.deleteDescription")}
        confirmLabel={t("vms.action.delete")}
        typeToConfirm={vm.name}
        returnFocus={returnFocus}
        onConfirm={async () => {
          await onRemove(vm);
          afterRemove?.();
        }}
      />
    </>
```

을 다음으로 바꾼다:

```tsx
      />
      {which === "delete" && (
        <VmDeleteDialog
          vm={vm}
          onClose={onClose}
          onRemove={onRemove}
          afterRemove={afterRemove}
          returnFocus={returnFocus}
        />
      )}
    </>
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 199개

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 볼륨 크기 늘리기와 VM 삭제 때 스냅샷도 지우기"
```

---

### Task 13: 브라우저 E2E: 스냅샷과 크기 늘리기

콘솔 화면 뒤에 메모리 스냅샷 "before" 만들기 → 되돌리기(알림 뒤 다시 Running) → 삭제, 실행 중 볼륨 16 MiB → 1 GiB, 강제 정지 뒤 디스크만 스냅샷 "off" → VM 삭제 대화상자에서 스냅샷도 지우기를 더한다. Rust는 끝에 `GET /snapshots`가 비었는지도 보고, 실패 정리는 `vm delete --with-snapshots`(60초 제한).

**Files:**
- Modify: `crates/opld/tests/web.rs`
- Modify: `web/e2e/vm.spec.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/tests/web.rs`에서

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, VM·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

을 다음으로 바꾼다:

```rust
//! 웹 UI E2E: 실제 opld(QEMU)에 브라우저(Playwright)로 로그인해 풀을 만들고 이미지를 올린 뒤, 그 볼륨으로
//! VM을 만들어 콘솔을 보고, 스냅샷을 만들고 되돌리고 지우고, 볼륨을 늘린 뒤, VM(스냅샷과 함께)·볼륨·풀을 모두 지운다. 클러스터와 로그인 정보는 여기서 준비한다.
//! `pnpm -C web build && cargo test -p opld --features web-it --test web`
```

`crates/opld/tests/web.rs`에서

```rust
use common::Daemon;

/// 브라우저가 올릴 디스크 이미지(raw, 0으로 채운 sparse 파일)의 크기
const IMAGE_BYTES: u64 = 16 << 20;
```

을 다음으로 바꾼다:

```rust
use common::Daemon;
use oplctl::Client;
use serde_json::Value;

/// 브라우저가 올릴 디스크 이미지(raw, 0으로 채운 sparse 파일)의 크기
const IMAGE_BYTES: u64 = 16 << 20;

/// `oplctl login`이 저장한 로그인 정보로 API 클라이언트를 만든다(스냅샷 전체 목록은 oplctl에 없다).
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
```

`crates/opld/tests/web.rs`에서

```rust
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = d.ctl(&["vm", "delete", "web"]).await;
        let _ = tokio::time::timeout(
```

을 다음으로 바꾼다:

```rust
        let _ = d.ctl(&["vm", "stop", "web", "--force"]).await;
        let _ = tokio::time::timeout(
            Duration::from_secs(60),
            d.ctl(&["vm", "delete", "web", "--with-snapshots"]),
        )
        .await;
        let _ = tokio::time::timeout(
```

`crates/opld/tests/web.rs`에서

```rust
    }
    // 볼륨 파일도 지웠다. dir 풀의 디렉터리에는 볼륨 파일과 만드는 동안의 임시 파일만 생긴다.
```

을 다음으로 바꾼다:

```rust
    }
    let snapshots: Value = client(&d).get("/snapshots").await.unwrap();
    assert_eq!(
        snapshots["items"].as_array().map(Vec::len),
        Some(0),
        "snapshots: {snapshots}"
    );
    // 볼륨 파일도 지웠다. dir 풀의 디렉터리에는 볼륨 파일과 만드는 동안의 임시 파일만 생긴다.
```

`web/e2e/vm.spec.ts`에서

```ts

  // OS가 없는 게스트는 ACPI 종료에 응하지 않으므로 강제로 끈다.
```

을 다음으로 바꾼다:

```ts

  // 실행 중에 메모리까지 담은 스냅샷을 만든다. 만들기 Task가 끝나면 현재 위치가 된다.
  // 스냅샷은 qcow2 디스크에만 만들 수 있다. 위에서 올린 raw 이미지를 qcow2로 가져왔다는 것이 전제다.
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Take snapshot" }).click();
  dialog = page.getByRole("dialog", { name: "Take a snapshot of web" });
  await dialog.getByLabel("Name").fill("before");
  await expect(dialog.getByRole("checkbox", { name: "Include memory" })).toBeChecked();
  await dialog.getByRole("button", { name: "Take snapshot" }).click();
  await expect(dialog).toBeHidden();
  const before = page.getByRole("row").filter({ has: page.getByRole("cell", { name: /^before/ }) });
  await expect(before.getByText("Current", { exact: true })).toBeVisible({ timeout: 60_000 });
  await expect(before.getByRole("cell", { name: "Included", exact: true })).toBeVisible();

  // 되돌리면 VM을 껐다가 저장한 메모리에서 다시 실행한다. Task가 끝날 때는 다시 실행 중이다.
  await page.getByRole("button", { name: "Revert to before" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Revert" }).click();
  await expect(page.getByText("Revert web to before: done")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByTestId("vm-phase")).toHaveText("Running");
  await expect(before.getByText("Current", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Delete snapshot before" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No snapshots.")).toBeVisible({ timeout: 60_000 });

  // 실행 중인 VM의 볼륨을 1 GiB로 늘린다.
  await page.getByRole("link", { name: "Volumes" }).click();
  await page.getByRole("button", { name: "Resize root" }).click();
  dialog = page.getByRole("dialog", { name: "Resize volume root" });
  await expect(dialog.getByText("Current size: 16 MiB")).toBeVisible();
  await dialog.getByLabel("New size (GiB)").fill("1");
  await dialog.getByRole("button", { name: "Resize" }).click();
  await expect(dialog).toBeHidden();
  await expect(root.getByRole("cell", { name: "1 GiB", exact: true })).toBeVisible({
    timeout: 60_000,
  });

  await page.getByRole("link", { name: "Virtual machines" }).click();
  await page.getByRole("link", { name: "web", exact: true }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Running");

  // OS가 없는 게스트는 ACPI 종료에 응하지 않으므로 강제로 끈다.
```

`web/e2e/vm.spec.ts`에서

```ts

  await page.getByRole("button", { name: "Delete" }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0);
```

을 다음으로 바꾼다:

```ts

  // 꺼진 VM에는 디스크만 담는 스냅샷을 만든다(메모리 칸이 없다).
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Take snapshot" }).click();
  dialog = page.getByRole("dialog", { name: "Take a snapshot of web" });
  await dialog.getByLabel("Name").fill("off");
  await expect(dialog.getByRole("checkbox", { name: "Include memory" })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Take snapshot" }).click();
  await expect(dialog).toBeHidden();
  const off = page.getByRole("row").filter({ has: page.getByRole("cell", { name: /^off/ }) });
  await expect(off.getByText("Current", { exact: true })).toBeVisible({ timeout: 60_000 });
  await expect(off.getByRole("cell", { name: "No", exact: true })).toBeVisible();

  // 스냅샷이 있는 VM은 스냅샷도 지우겠다고 골라야 지울 수 있다.
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await expect(confirm.getByRole("button", { name: "Delete" })).toBeDisabled();
  await confirm.getByRole("checkbox", { name: "Also delete its snapshot" }).check();
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web typecheck && pnpm -C web lint`
Expected: 통과

Run: `cargo fmt --all --check && cargo clippy -p opld --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `pnpm -C web build && cargo test -p opld --features web-it --test web`
Expected: macOS에서 PASS(약 12초), 남는 QEMU 없음

- [ ] **Step 3: 커밋**

```bash
git add web crates
git commit -m "test(web): 브라우저 E2E에 스냅샷과 볼륨 크기 늘리기 더하기"
```

---

### Task 14: 문서

README에 상태와 스냅샷·크기 늘리기 CLI 예, 웹 기능을 더한다. 설계 문서에 구현하며 정한 것(작업 노드 순서, `UpdateStatusIf`, 스냅샷 Task 중 `vm_busy`, 540초, `NotAttached`, 메모리 기본값, 512 배수, 웹 동작)과 제한을 적는다. CI는 바꾸지 않는다(`linux-kvm`이 이미 `qemu-it`·`web-it`을 돌린다).

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-26-snapshots-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷)
```

`README.md`에서

````markdown
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
```
````

을 다음으로 바꾼다:

````markdown
cargo run -p oplctl -- vm console web            # 출력된 vnc:// 주소를 VNC 뷰어로 연다
cargo run -p oplctl -- vm snapshot create web before --description "업데이트 전"   # 실행 중이면 메모리 포함(--no-memory: 디스크만)
cargo run -p oplctl -- vm snapshot list web      # 나무 모양, 현재 위치 *
cargo run -p oplctl -- vm snapshot revert web before
cargo run -p oplctl -- vm snapshot delete web before
cargo run -p oplctl -- volume resize root --size 2G   # 늘리기만, 실행 중에도 된다
```
````

`README.md`에서

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔),
볼륨(빈 볼륨, 이미지 업로드), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown

웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 네트워크, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
- [하위 프로젝트 2 설계: 웹 UI](docs/superpowers/specs/2026-10-23-web-ui-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [하위 프로젝트 2 설계: 웹 UI](docs/superpowers/specs/2026-10-23-web-ui-design.md)
- [하위 프로젝트 3 설계: VM 스냅샷](docs/superpowers/specs/2026-10-26-snapshots-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-26-snapshots-design.md` 전체:

```markdown
# opLite VM 스냅샷 설계 (하위 프로젝트 3의 첫 부분)

작성일: 2026-10-26
상위 문서: `2026-10-20-opl-node-core-design.md` (§1.1의 하위 프로젝트 3 "공유 스토리지와 스냅샷")

## 1. 목표와 범위

VM의 한 순간(디스크, 실행 중이면 메모리, VM 설정)을 저장했다가 되돌린다. 같은 바탕으로 볼륨 크기 늘리기, 스냅샷에서 VM 복제, 예약 스냅샷을 한다. 하위 프로젝트 3의 나머지(NFS, LVM-thin, ZFS, iSCSI, Ceph)는 이 설계의 드라이버 구조 위에 뒤의 계획으로 붙인다.

| 기능 | 내용 |
|---|---|
| 스냅샷 만들기 | 실행 중이면 메모리 포함(기본) 또는 디스크만, 정지했으면 디스크만. 이름·설명 |
| 스냅샷 나무 | 스냅샷마다 부모. 되돌린 뒤 만든 스냅샷은 새 가지가 된다. VM에 현재 위치 |
| 되돌리기 | 디스크, UEFI 변수, VM 설정을 스냅샷 것으로. 메모리 스냅샷이면 그 순간부터 실행 |
| 삭제 | 아무 스냅샷이나(중간 것도). 자식은 부모로 옮겨 붙는다 |
| 볼륨 크기 늘리기 | 정지·실행 중 모두. 늘리기만 |
| 복제 | 스냅샷 또는 정지한 VM의 지금 상태에서 새 VM(디스크 전체 복사) |
| 예약 스냅샷 | VM마다 주기(매시·매일·매주)와 보관 개수 |

넣지 않는 것: 링크드 클론, 시각을 지정하는 예약(매일 02:00 등), raw → qcow2 변환, 스냅샷 이름 바꾸기, 공유 스토리지.

## 2. 방식 선택

- **qcow2 내부 스냅샷.** 스냅샷은 각 디스크 파일 안에 태그로 둔다. 파일 체인(외부 오버레이)을 관리하지 않아 되돌리기와 중간 스냅샷 삭제가 단순하고, 나무 구조가 그대로 된다. Proxmox의 qcow2 스냅샷과 같다.
- **QEMU 11의 QMP 작업.** 실행 중이면 `snapshot-save`/`snapshot-load`/`snapshot-delete`로 하고 `devices`로 디스크를 고른다. `-loadvm`·`savevm`은 쓰지 않는다. 쓰기 가능한 raw pflash(UEFI 변수)가 있으면 실패하기 때문이다. 맥(QEMU 11.1, hvf)에서 저장(메모리 512MiB, 약 0.6초 멈춤)·불러오기·삭제와, `-S`로 띄운 새 QEMU에 `snapshot-load` 뒤 `cont`로 이어 실행되는 것을 확인했다.
- **정지한 VM은 `qemu-img snapshot`.** `-c`(만들기), `-a`(되돌리기), `-d`(삭제). 내부 스냅샷이 있어도 `qemu-img resize`와 `qemu-img convert -l snapshot.name=<태그>`가 된다(확인함).
- **raw 디스크는 거부.** raw 볼륨에는 내부 스냅샷을 둘 수 없다. raw 디스크가 하나라도 붙은 VM은 스냅샷을 만들지 않는다(`snapshot_unsupported`). 일부 디스크만 되돌아가는 일을 막는다.
- **스냅샷은 클러스터 리소스.** 목록·watch·Task·권한을 기존 체계로 쓴다. 디스크의 실제 태그와 어긋날 수 있는 부분은 노드가 시작할 때 정리한다(§6).

## 3. 데이터 모델

### `Snapshot` (새 종류)

| 필드 | 내용 |
|---|---|
| `spec.vm` | VM id |
| `spec.name` | 사용자 이름. 이름 규칙은 다른 리소스와 같고, VM 안에서 고유 |
| `spec.parent` | 부모 스냅샷 id. 첫 스냅샷이면 없음 |
| `spec.memory` | 메모리 상태를 담았는지 |
| `spec.description` | 설명(최대 1024자) |
| `spec.scheduled` | 예약으로 만들었는지. 보관 개수 정리는 이것만 지운다 |
| `spec.vm_spec` | 만든 순간의 VM 스펙 전체 |
| `status.phase` | `pending`(만드는 중) / `ready` |
| `status.created_at` | 만든 시각(unix 초) |
| `status.vmstate_bytes` | 메모리 상태 크기(메모리 스냅샷만). 계획 10은 채우지 않는다(늘 없음) |

- 리소스 이름은 클러스터 전체에서 고유해야 하므로 스냅샷 id 문자열이다. "VM 안에서 이름 고유"는 배타 값(`exclusive.rs`)의 `(vm, name)` 키로 지킨다. API와 CLI는 `/vms/{key}/snapshots/{name}`처럼 VM과 이름으로 가리킨다. 기존 라우트와 맞춰 `{key}`는 VM 이름 또는 id, `{name}`은 스냅샷 이름 또는 id다(웹은 id를 쓴다). 배타 값은 `("name", "{vm}/{name}")`이고, 겹치면 `name_taken`. watch용 전체 목록 `GET /snapshots`도 둔다.
- 디스크 안의 qcow2 태그는 스냅샷 id다. 메모리 상태는 VM 스펙의 첫 qcow2 디스크(`disks[0]`)에 담는다(`vmstate`).
- UEFI 변수 사본: `{state_dir}/vm/{vm-id}/snapshots/{snapshot-id}.vars`. BIOS VM은 변수 파일이 없어 사본도 없다. 사본이 없는 스냅샷으로 되돌리면 변수는 건드리지 않는다.

### VM에 더하는 것

- `status.snapshot`: 현재 위치(마지막으로 만들었거나 되돌린 스냅샷). 다음 스냅샷의 부모.
- `spec.snapshot_schedule` (계획 11): `{every: hourly|daily|weekly, keep: 1..=100, memory: bool}`. 기본은 없음, `memory` 기본 false.
- `status.schedule_error` (계획 11): 마지막 예약 스냅샷 실패 이유.

기존 VM은 새 필드가 기본값이라 그대로 읽힌다.

### Task 종류

`snapshot-create`, `snapshot-revert`, `snapshot-delete`, `volume-resize`, `vm-clone`(계획 11). 스냅샷·복제 Task는 VM의 디스크를 쓸 노드에서 돈다: `spec.node`(꺼진 VM을 옮겼으면 옮긴 노드), 없으면 `status.node`, 배치된 적이 없으면 첫 디스크 볼륨의 노드(`status.node`), 그것도 없으면 API를 받은 노드. 크기 늘리기는 볼륨을 쓰는 VM에 같은 순서(API 노드 대신 볼륨의 노드)를 쓰고, 쓰는 VM이 없으면 볼륨의 노드에서 돈다(§5). 시작할 때의 고아 정리도 같은 순서로 주인 노드를 정한다(§6).

## 4. 드라이버

- **QMP 작업 도우미** (`opl-qemu`): 작업을 시작하고 `JOB_STATUS_CHANGE`(또는 `query-jobs`)로 `concluded`를 기다린 뒤 오류를 읽고 `job-dismiss`한다. 제한 시간을 둔다(540초. Task 제한 600초보다 짧아야 Task가 먼저 끊기지 않고 작업 시간 초과를 받는다). job id는 `opl-{uuid}`. 시간이 지나면 작업을 취소하지 않고 QEMU에 남긴다. 작업 실패(`JobFailed`)와 시간 초과(`JobTimeout`)는 영구 오류다. QEMU 11.1의 `snapshot-delete`는 없는 태그도 성공한다.
- **`Hypervisor` 트레잇**에 실행 중인 VM용 메서드: `snapshot_save(vm, tag, memory)`, `snapshot_delete(vm, tag)`, `block_resize(vm, disk, bytes)`. 디스크 노드(`disk{i}`)는 `query-named-block-nodes`로 QEMU에 묻는다(다시 붙은 VM은 스펙을 모른다). 디스크 없는 VM의 저장은 거부하고 삭제는 성공으로 본다. 살아 있지만 아직 다시 붙지 못한 VM은 `NotAttached`(일시 오류)로 알린다. 요청을 보내지 않았으므로 시간 초과와 달리 QEMU에 남은 작업이 없다. 디스크만 담는 실행 중 스냅샷은 `transaction` + `blockdev-snapshot-internal-sync`로 모든 디스크를 한 번에 한다(VM이 멈추지 않는다). 메모리 복원은 시작 경로에 넣는다(아래).
- **메모리 복원 시작:** reconciler가 VM을 시작할 때 한 번짜리 "불러올 스냅샷"을 받을 수 있게 한다. 드라이버는 지금처럼 `-S`로 띄우고 QMP에 붙은 뒤 `cont` 전에 `snapshot-load`를 한다. 불러오기에 실패하면 QEMU를 끝내고 VM은 `failed`가 된다.
  - 구현: `Hypervisor::start(vm, restore: Option<tag>)`. reconciler는 VM별 "불러올 태그"를 공유 맵에 둔다(`restore_on_start`·`clear_restore`). 채널이 아니라 맡기는 즉시 보이고 `power` 쓰기와 순서가 꼬이지 않는다. 작업자가 새로 띄울 때만 꺼내 쓴다(다시 붙기에는 없음). 이미 살아 있는 VM에는 무시한다.
  - 태그가 있던 시작이 실패하면 일시 오류라도 재시도 없이 `failed`. 태그는 한 번 쓰면 사라진다. VM 삭제·다른 노드로 옮김에도 버린다.
  - 맵은 메모리에만 있다. 되돌리기와 시작 사이에 opld가 재시작하면 태그가 사라지고 VM은 처음부터 부팅한다.
- **`Storage` 트레잇**에 정지한 볼륨용 메서드: `snapshot_create/revert/delete(pool, volume, format, tag)`, `snapshot_tags`(`qemu-img info -U`의 스냅샷 이름), `resize_volume`, `copy_volume(src, tag: Option, dst)`(계획 11). 같은 태그 만들기는 거부하고, 삭제는 겹친 태그를 모두 지운다. 줄이기는 `qemu-img`가 거부한다. 백엔드가 스냅샷을 지원하는지 알리는 `supports_snapshots(format)`을 둔다. `dir` 백엔드는 qcow2만 지원한다. 뒤의 LVM·ZFS·RBD는 자기 방식으로 구현한다.
- **같은 VM의 작업 직렬화:** Task 실행기의 대상별 줄(`by_target`)을 VM에도 쓴다. 같은 VM의 스냅샷 Task와 전원 Task가 한 노드에서 동시에 돌지 않는다. reconciler의 시작·정지와는 VM 상태(안정 상태 검사)와 Task 안의 재확인으로 겹치지 않게 한다.
- **조건부 상태 쓰기:** reconciler와 Task가 같은 VM `status`를 쓴다. 통째 교체면 Task가 옮긴 현재 위치를 reconciler가 덮는다. 그래서 revision을 확인하는 새 Raft 명령 `UpdateStatusIf { kind, id, expected_revision, status }`를 더했다(`Cluster::update_status_checked`). 기존 `UpdateStatus`는 형식 그대로 둔다. `UpdateStatus`에 선택 필드를 더하면 옛 노드가 그 필드를 버리고 확인 없이 덮어쓰기 때문이다. 옛 노드는 모르는 명령을 오류로 처리한다(노드 코어 설계 3.1절). reconciler의 쓰기는 revision이 다르면 다시 읽어 `snapshot`만 저장소 것을 쓰고 나머지는 전처럼 덮는다. Task의 위치 쓰기도 revision을 확인한다.

## 5. 동작

### 만들기 — `POST /vms/{vm}/snapshots {name, description?, memory?}` → 202

검사:
- VM이 안정 상태(실행 중이거나 정지·실패)여야 한다. 시작·정지 중(phase `starting`·`stopping`)이면 409 `vm_busy`. 되돌리기·삭제도 같다.
- 반대로 이 VM의 스냅샷 만들기·되돌리기 Task가 끝나지 않았으면 VM 동작(시작·정지·강제 정지·재부팅)과 VM 수정이 409 `vm_busy`다. 강제 정지도 막는다: 되돌리기가 디스크를 바꾸는 중에 켜고 끄는 것이 섞이면 안 되고, 만들기 중에 끄면 저장이 끊긴다. 멈춘 Task도 Task 제한 시간으로 끝난다.
- 모든 디스크가 qcow2이고 `ready`. qcow2가 아닌 것이 있으면 409 `snapshot_unsupported`, 디스크가 없으면 메모리 스냅샷은 400.
- `memory`는 실행 중이고 디스크가 있으면 기본 true. 디스크 없는 실행 중 VM은 기본으로 설정만 담는다. 정지한 VM에 `memory: true`는 400.

실행: `pending` 리소스를 만들고(`parent` = VM의 `status.snapshot`, `vm_spec` = 지금 스펙) Task를 돌린다. Task를 만들지 못하면 그 리소스를 지우고(못 지우면 재시작 정리에 맡긴다) 오류를 돌려준다. `parent`와 `vm_spec`은 Task가 실행될 때 다시 쓴다. 같은 VM에 만들기가 겹쳐 줄을 서면 API 시점의 부모가 같기 때문이다.

| VM 상태 | 방법 |
|---|---|
| 실행 중 + 메모리 | `snapshot-save` (devices = 모든 디스크, vmstate = 첫 디스크). 저장하는 동안 VM이 멈춘다 |
| 실행 중 + 디스크만 | `transaction` + `blockdev-snapshot-internal-sync` |
| 정지 | 디스크마다 `qemu-img snapshot -c` |

UEFI 변수를 복사하고 `ready`로 바꾸고 VM의 `status.snapshot`을 이 스냅샷으로. 실패하면 만든 태그를 지우고 리소스를 지운다. 단 실패가 확실할 때만 태그를 지운다. 시간 초과(`JobTimeout`, QMP 응답 없음, Task 제한 시간, 재시작 중단)면 작업이 QEMU에 남아 있을 수 있어 태그는 두고 리소스만 지운다. Task 안에서 VM 상태를 다시 확인한다(Task가 기다리는 사이 바뀔 수 있다).

### 되돌리기 — `POST /vms/{vm}/snapshots/{name}/revert` → 202

검사: 스냅샷이 `ready`(아니면 409 `snapshot_not_ready`), `vm_spec`의 볼륨이 모두 있고 `ready`이며 다른 VM이 쓰지 않는다. VM 만들기와 같은 검사(`vms::prepare`, node = 지금, power = stopped)를 쓴다: 볼륨 없음 400, 준비 안 됨·사용 중 409, MAC 겹침 `mac_in_use`.

실행:
1. 실행 중이면 강제로 끈다. 그 전에 Task 안에서 볼륨을 다시 확인한다(API가 확인한 뒤 다른 VM이 붙였으면 끄지 않고 실패).
2. VM 스펙을 `vm_spec`으로 바꾼다. `node`는 지금 노드를 유지하고, `power`는 메모리 스냅샷이면 `running`, 아니면 `stopped`. 그 뒤로 붙인 디스크는 VM에서 떨어지고 볼륨은 남는다.
3. 디스크마다 `qemu-img snapshot -a`, UEFI 변수 사본을 되돌린다. 디스크마다 `-a` 전에 VM을 다시 읽어 `power`가 `stopped`가 아니면(동작 막기를 지나친 요청) 그만두고 실패한다.
4. 볼륨의 실제 크기를 다시 읽어 볼륨 스펙에 반영한다(스냅샷 뒤 늘린 크기는 되돌아간다). 다시 읽은 revision으로 쓴다(라벨 등 그사이 쓴 것을 덮지 않는다).
5. 메모리 스냅샷이면 "불러올 스냅샷"을 주고 시작한다(§4). 그 순간부터 실행된다. 그 전에 이 Task 뒤에 만들어진 같은 VM의 전원 Task(정지 등)가 있으면 켜지 않고 실패한다(재부팅의 `superseded`와 같다).
6. VM의 `status.snapshot`을 이 스냅샷으로. 실제로는 3 직후(디스크를 되돌린 뒤) 옮긴다. 메모리 불러오기가 실패해도 위치는 이 스냅샷이다.

배치되지 않은 VM을 메모리 스냅샷으로 되돌리면 `node`는 이 노드가 된다. 시작 결과는 VM `status`(`running`/`failed`)로 확인한다.

### 삭제 — `DELETE /vms/{vm}/snapshots/{name}` → 202

실행 중이면 `snapshot-delete`, 정지했으면 `qemu-img snapshot -d`. 변수 사본을 지운다. 자식의 `parent`를 지운 스냅샷의 부모로 바꾸고, 현재 위치였으면 VM의 `status.snapshot`도 부모로. 디스크에 태그가 이미 없으면 성공으로 본다(다시 시도해도 된다). 실행 중이면 붙은 디스크는 QMP로 지우고, 그 뒤 떼어 낸 디스크(`vm_spec`에만 있는 볼륨)는 스토리지로 지운다. 이 노드의 볼륨인데 파일이 없으면 성공이다. 다른 노드에서 만든 볼륨(공유 풀이 아님)이면 이 노드에서 파일이 보이지 않으므로 실패다(없는 것으로 보면 그 노드에 태그가 남는다).

### VM 삭제

스냅샷이 남아 있으면 409 `vm_has_snapshots`. CLI `vm delete --with-snapshots`와 웹의 "스냅샷도 모두 지우기"는 스냅샷 삭제 Task를 차례로 돌린 뒤 VM을 지운다. 볼륨은 지금처럼 남는다.
- 차례는 스냅샷 id(UUIDv7, 만든 차례)다. 하나씩 지우고 Task를 기다린다. `pending` 스냅샷이 있으면 409 `snapshot_not_ready`로 멈춘다(VM은 남는다).
- 웹: 스냅샷이 있으면 "스냅샷도 모두 지우기"를 체크해야 삭제 버튼이 켜진다. 스냅샷 목록을 읽기 전이나 읽기 실패면 막는다. 실패·시간 초과면 대화상자에 오류를 보인다. 화면을 떠나면 멈추고 VM은 지우지 않는다.
- 확인과 삭제 사이에 새 스냅샷이 만들어지는 경합은 막지 않는다.

### 볼륨 규칙

스냅샷의 `vm_spec`이 가리키는 볼륨은 지울 수 없다(409 `volume_in_snapshot`). VM에서 떼어 낸 볼륨도 스냅샷이 남아 있으면 지울 수 없다.

### 볼륨 크기 늘리기 — `POST /volumes/{key}/resize {size_bytes}` → 202

지금보다 크고 512 바이트의 배수여야 한다(400). qcow2는 `qemu-img resize`도 QMP `block_resize`도 512 배수가 아닌 크기를 거부한다(QEMU 11.1에서 확인). raw는 받지만 `virtual_bytes`가 512 배수로 올라가 요청과 달라지므로 형식과 관계없이 거부한다. 웹은 1 MiB 단위로 올린 값을 보낸다. 볼륨이 `ready`가 아니면 409 `volume_not_ready`. 실행 중인 VM이 쓰면 그 노드에서 `block_resize`(게스트가 바로 본다), 아니면 `qemu-img resize`. 성공하면 실제 크기(`virtual_bytes`)를 읽어 `spec.size_bytes`를 바꾼다(읽기 실패면 Task 실패). raw도 된다.

- "실행 중" = VM phase가 `stopped`·`failed`가 아님. Task 안에서 `ready`·크기·VM 상태를 다시 본다. 시작·정지 중이면 실패다. QEMU 디스크 번호는 VM 스펙의 디스크 순서(`disk{i}`)다.
- 차례: 같은 디스크의 스냅샷 Task와 겹치지 않게, 크기 늘리기는 볼륨이 아니라 그 볼륨을 쓰는 VM의 줄에 세운다(스펙에서 쓰는 VM → 그 볼륨을 담은 스냅샷의 VM → 없으면 볼륨). 줄은 노드별이라, 꺼진 VM이 쓰는 볼륨도 그 VM의 스냅샷 Task가 돌 노드(§3 순서)로 보낸다. 스펙 크기는 다시 읽은 revision으로 쓴다. API에서 거절하는 방식은 노드 간 확인·실행 틈이 남아 택하지 않았다.

### 복제 (계획 11) — `POST /vms/{vm}/clone {name, snapshot?, pool?}` → 202

- 원본: 스냅샷, 또는 정지한 VM의 지금 상태. 실행 중인데 스냅샷을 주지 않으면 409 `vm_running`.
- 원본 VM 스펙(스냅샷이면 `vm_spec`)의 디스크마다 새 볼륨 `{새 VM}-{원래 볼륨}`(63자로 자르고, 겹치면 `-2`, `-3`)을 만들고 `qemu-img convert`(스냅샷이면 `-l snapshot.name=<id>`)로 복사한다.
- 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다(기본은 원본과 같은 풀).
- 새 VM: MAC 새로, `node` 없음, `power: stopped`, 예약 없음, 스냅샷 없음.
- 실패하면 만든 볼륨을 지운다.

### 예약 스냅샷 (계획 11)

리더에서 1분마다 도는 반복 작업(`NodeMonitor`와 같은 모양: 리더 확인 → `read_barrier` → 목록).
- 예약이 있는 VM마다, 마지막 예약 스냅샷의 `created_at`에서 주기가 지났으면 `auto-YYYYMMDD-HHMM`(UTC) 스냅샷을 만든다. 실행 중이고 `memory`면 메모리 포함.
- 만든 뒤 `scheduled`인 스냅샷이 `keep`개를 넘으면 오래된 것부터 지운다. 사용자가 만든 스냅샷은 지우지 않는다.
- 만들 수 없으면(raw 디스크, VM 바쁨, 노드 오프라인) `status.schedule_error`에 남기고 다음 주기에 다시 본다. 성공하면 지운다.
- 같은 VM에 예약 Task가 이미 돌고 있으면 건너뛴다.

## 6. 일관성과 장애

- **opld 재시작:** 지금처럼 실행 중이던 Task는 실패로 끝난다. 노드가 시작할 때 이 노드의 VM에 대해 `pending` 스냅샷(끝나지 않은 Task가 없는 것)을 지우고 디스크의 같은 태그를 지운다(볼륨 고아 정리와 같은 방식). 맡는 노드는 그 스냅샷 만들기 Task의 노드, 없으면 API가 Task를 배정했을 노드(§3: `spec.node`, `status.node`, 첫 디스크 볼륨의 노드), 다 없으면(디스크 없는 VM) 아무 노드다. Task를 만들기 전에 실패해 남은 `pending` 스냅샷도 이때 지운다.
- **메모리 복원 대기 중 재시작:** 불러올 태그는 메모리에만 있어 사라진다. VM은 메모리 상태 없이 처음부터 부팅한다(§4).
- **되돌리기 중단:** 디스크 일부만 되돌아간 채 멈추면 VM은 정지 상태로 남고 Task는 실패다. 같은 되돌리기를 다시 하면 처음부터 다시 한다(`-a`는 몇 번 해도 같다).
- **삭제 중단:** 태그가 없으면 성공으로 보므로 다시 하면 된다.
- **태그만 남은 경우:** 리소스 없이 디스크에 남은 태그는 공간만 쓴다. 자동으로 지우지 않고, 노드에서 `qemu-img snapshot -l`로 볼 수 있다. 만들기가 시간 초과로 끝나도 태그가 남는다(§5).

## 7. 권한

스냅샷은 VM 권한을 따른다. 핸들러가 VM 종류로 확인한다(역할 표는 그대로). 목록·조회는 Read, 만들기·되돌리기·삭제는 VM Operate(operator 이상). 복제는 VM·볼륨 Write. 예약 설정은 VM Write. 볼륨 크기 늘리기는 볼륨 Write. viewer는 보기만 한다.

## 8. CLI

| 명령 | 계획 |
|---|---|
| `oplctl vm snapshot create <vm> <이름> [--description …] [--no-memory]` | 10 |
| `oplctl vm snapshot list <vm>` (나무 모양, 현재 위치 `*`) | 10 |
| `oplctl vm snapshot revert <vm> <이름>`, `oplctl vm snapshot delete <vm> <이름>` | 10 |
| `oplctl vm delete <vm> --with-snapshots` | 10 |
| `oplctl volume resize <볼륨> --size 20G` | 10 |
| `oplctl vm clone <vm> <새 이름> [--snapshot …] [--pool …]` | 11 |
| VM 수정에 `--snapshot-every hourly|daily|weekly --snapshot-keep N [--snapshot-memory]`, `--no-snapshot-schedule` | 11 |

명령은 Task가 끝날 때까지 기다린다.

- `list` 열: 현재 위치 `*`, NAME(`├─`/`└─` 나무, 형제는 id 차례, 목록에 없는 부모면 뿌리), CREATED(UTC, `2026-10-26 07:34 UTC`, `ready`가 아니면 `pending`), MEMORY(true/false), DESCRIPTION. `-o json`이면 `items` 배열.
- `--no-memory`면 `memory: false`를 보낸다. 아니면 생략해 서버 기본을 따른다.

## 9. 웹 UI

계획 10:
- **VM 상세 "스냅샷" 탭:** 나무 모양 목록(이름, 만든 시각, 메모리 포함·크기, 설명, 현재 위치 표시). 만들기 대화상자(이름, 설명, 실행 중이면 "메모리 포함" 기본 켬). 되돌리기 확인("지금 상태는 사라지고 VM이 꺼졌다가 스냅샷 상태가 된다"), 삭제 확인. 스냅샷할 수 없으면(raw 디스크) 탭 안에 까닭을 글로.
- **볼륨 화면:** "크기 늘리기" 대화상자(지금보다 큰 값만).
- **VM 삭제 확인:** 스냅샷이 있으면 "스냅샷도 모두 지우기" 선택.
- 스냅샷은 watch 대상(`WatchKind`)에 넣어 바로 갱신된다.
- 구현: 경로의 `{name}`에 스냅샷 id를 쓴다. 만든 시각은 브라우저 시간대로 보인다(CLI는 UTC). 메모리 크기는 서버가 채우지 않아 보이지 않는다. "메모리 포함" 칸은 실행 중이고 디스크가 있을 때만 보이고, 안 보이면 `memory: false`를 명시한다(대화상자를 연 사이 VM이 켜져도 메모리 스냅샷이 되지 않게). 만들기 버튼은 스냅샷·볼륨 목록을 읽은 뒤에 켠다. 시작·정지 중이면 모든 동작을 막고 까닭을 글로. `pending` 행은 "만드는 중"으로 보이고 되돌리기·삭제를 막는다.
- 구현: 크기 늘리기는 볼륨이 `ready`일 때만. 입력(GiB)은 1 MiB 단위로 올린 뒤 지금 크기(열어 둔 사이 바뀌면 최신)와 비교한다. VM 목록에 일괄 삭제는 없어 "스냅샷도 지우기"는 메뉴·상세의 삭제 대화상자에만 있다(§5). 만드는 중인 스냅샷이 있으면 삭제를 막고 까닭을 글로. 지우는 동안에는 누를 때의 스냅샷 수를 보이고 취소할 수 없다고 알린다.
- 구현: 나무의 깊이는 스크린 리더용 글("(부모 다음)")로도 알린다. 부모가 돌고 도는 스냅샷은 뿌리로 보인다. 볼륨 목록을 읽지 못하면 만들기 버튼 옆에 까닭을 보인다.

계획 11:
- **VM 메뉴 "복제":** 새 이름, 원본(지금 상태 / 스냅샷), 대상 풀.
- **VM 폼 "예약 스냅샷":** 주기, 보관 개수, 메모리 포함. VM 상세에 마지막 예약 실패.

## 10. 테스트

- **단위:** 타입·직렬화, API 검사 규칙(바쁜 VM, raw 디스크, 정지 VM의 메모리, 이름 중복, 볼륨 삭제 막기, VM 삭제 막기), 나무 정리(자식 옮기기), 예약 판단(주기, 보관 개수)과 이름, QMP 작업 도우미(가짜 QMP).
- **실제 `qemu-img`:** 정지 볼륨의 스냅샷 만들기·되돌리기·삭제(`qemu-io`로 쓴 패턴이 되돌아가는지), 크기 늘리기, 스냅샷에서 복사.
- **실제 QEMU (`qemu-it`, 맥 hvf와 CI KVM):** 실행 중 메모리 스냅샷 → 되돌리기 → 실행 상태로 돌아옴. 실행 중 디스크만 스냅샷. 실행 중 크기 늘리기(QMP 연결은 opld가 쥐고 있어 `query-block` 대신 `qemu-img info -U`의 `virtual-size`로 확인). 되돌리기로 디스크 내용이 돌아오는지는 정지 상태에서 확인(OS 없는 게스트).
- **브라우저 E2E:** 계획 10은 기존 흐름에 메모리 스냅샷 만들기 → 되돌리기 → 삭제와 볼륨 크기 늘리기를 더한다. 계획 11은 스냅샷에서 복제한 VM 시작, 예약 스냅샷(테스트에서만 주기를 짧게 하는 설정).

## 11. 알고 받아들인 제한

- 메모리 스냅샷을 만드는 동안 VM이 멈춘다(메모리 크기에 비례, 512MiB에서 약 0.6초).
- raw 디스크가 붙은 VM은 스냅샷할 수 없다.
- 되돌리기는 실행 중인 VM을 강제로 끈다(저장하지 않은 게스트 상태는 사라진다).
- 복제는 디스크 전체를 복사한다(링크드 클론 없음). 대상 풀은 원본 볼륨의 노드에서 쓸 수 있어야 한다.
- 예약은 주기만 있고 시각을 정하지 못한다. 리더가 바뀌면 그 주기의 스냅샷이 1분 남짓 늦을 수 있다.
- 리소스 없이 디스크에 남은 태그는 자동으로 지우지 않는다. 만들기가 시간 초과로 끝나도 남는다.
- 메모리 스냅샷으로 되돌린 뒤 VM이 시작되기 전에 opld가 재시작하면 VM은 메모리 상태 없이 처음부터 부팅한다(디스크·위치는 그 스냅샷).
- 메모리 불러오기가 실패하면 되돌리기 Task의 오류가 원래 오류 대신 "VM was stopped"로 보일 수 있다(reconciler가 끄기를 두 번에 쓴다). VM의 `failed` 오류에는 원래 오류가 남는다.
- 크기 늘리기의 줄은 Task를 받을 때 정한다. 그 뒤 볼륨을 붙인 VM의 스냅샷 Task와는 차례가 없다. 배치되지 않은 VM의 Task는 첫 디스크 볼륨의 노드에서 돌므로, 디스크가 여러 노드에 걸쳐 있으면 차례가 없을 수 있다(그때는 `qemu-img` 잠금이 막는다).
- 다른 노드의 API가 `pending` 스냅샷을 만들고 Task를 만들기 전의 짧은 틈에 주인 노드가 재시작 정리를 하면 그 스냅샷을 지울 수 있다. 그 만들기 Task는 "snapshot was deleted"로 실패한다(다시 하면 된다).
- 메모리 스냅샷을 불러오며 시작하는 중(`-S`로 띄워 `snapshot-load`를 하는 동안)에 opld가 재시작하면, 다시 붙을 때 `prelaunch`로 남은 VM으로 보고 `cont`를 보내 불러오기와 겹칠 수 있다.
- 실행 중 메모리 스냅샷의 UEFI 변수 사본은 저장이 끝난 뒤 복사한다. 그사이 게스트가 바꾼 변수가 들어가 메모리 상태보다 조금 뒤일 수 있다.
- VM 삭제 확인과 실제 삭제 사이에 만들어진 스냅샷은 막지 않는다.
- `status.vmstate_bytes`는 채우지 않는다.

## 12. 계획 분할

| 계획 | 범위 | 완료 기준 |
|---|---|---|
| 10 | `Snapshot` 리소스, QMP 작업 도우미, 드라이버(실행 중·정지), 만들기·되돌리기·삭제, VM·볼륨 삭제 규칙, 볼륨 크기 늘리기, 재시작 정리, CLI, 웹 스냅샷 탭·크기 늘리기·VM 삭제 선택, E2E | 브라우저에서 실행 중인 VM의 메모리 스냅샷을 만들고 되돌려 그 순간부터 실행되는 E2E가 맥과 CI `linux-kvm`에서 통과 |
| 11 | 복제, 예약 스냅샷(리더 반복 작업, 보관 정리), CLI, 웹 | 스냅샷에서 복제한 VM이 시작되고, 예약 스냅샷이 만들어지고 보관 개수대로 정리되는 것을 E2E로 확인 |
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo deny check`
Expected: 경고 없음

Run: `cargo test --workspace --all-features`
Expected: macOS에서 PASS(실제 QEMU 테스트와 브라우저 E2E 포함)

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web audit --prod && pnpm -C web build`
Expected: PASS 199개, `No known vulnerabilities found`

Run: `pnpm -C web gen:api && git diff --exit-code web/src/api/schema.d.ts`
Expected: 차이 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: README에 스냅샷 CLI 예, 설계 문서에 구현하며 정한 것"
```

---

## 완료 기준 (계획 10)

- 브라우저에서 실행 중인 VM의 메모리 스냅샷을 만들고 되돌려 VM이 그 순간부터 다시 실행되고, 스냅샷을 지우고, 실행 중에 볼륨을 늘리고, 스냅샷과 함께 VM을 지우는 E2E가 macOS(hvf)에서 통과한다 (`pnpm -C web build && cargo test -p opld --features web-it --test web`). CI `linux-kvm`에서도 통과한다.
- 실제 QEMU 통합 테스트(`cargo test -p opld --features qemu-it --test snapshots`)가 메모리·디스크만 되돌리기, 디스크 내용 복원, 실행 중 크기 늘리기, 삭제 규칙을 확인한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **메모리 스냅샷 동안 멈춤:** 메모리 크기에 비례해 VM이 멈춘다(512MiB에서 약 0.6초).
- **raw 디스크:** raw 디스크가 붙은 VM은 스냅샷할 수 없다.
- **되돌리기는 강제로 끈다:** 저장하지 않은 게스트 상태는 사라진다.
- **스냅샷 작업 중 VM 조작:** 만들기·되돌리기가 끝날 때까지(최대 Task 한도 600초) 강제 정지도 막힌다.
- **복원 태그는 메모리에만:** 되돌리기 중 opld가 다시 시작하면 VM은 메모리 없이 처음부터 부팅된다.
- **시간 초과한 만들기의 태그:** 진행 중일 수 있어 디스크에 남긴다. 리소스 없는 태그는 자동으로 지우지 않는다(`qemu-img snapshot -l`로 볼 수 있다).
- **메모리 상태 크기:** `vmstate_bytes`는 채우지 않는다.
- **버전이 섞인 클러스터:** 옛 노드는 `UpdateStatusIf`와 스냅샷 리소스를 모른다. 모든 노드를 함께 올린다.
- 그 밖의 경합과 순서 제한은 설계 문서 §11에 있다.
