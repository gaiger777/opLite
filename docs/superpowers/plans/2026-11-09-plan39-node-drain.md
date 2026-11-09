# opLite 계획 39: 노드 유지보수 모드(노드 비우기) 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드를 손보기 전에 그 노드의 VM을 한 번에 비운다. `POST /nodes/{key}/drain`이 노드를 유지보수로 바꾸고, 실행 중인 VM을 스케줄러가 고른 노드로 하나씩 라이브 마이그레이션한다. 유지보수 노드에 고정된 꺼진 VM은 켤 때 다른 노드에 놓는다. oplctl·웹·문서. 하위 프로젝트 11(처음 분해 뒤의 첫 하위 프로젝트)이며 계획 하나로 끝난다.

**Architecture:**
- **API:** 확인(위트니스·오프라인·이미 비우는 중·기능 표시) → `maintenance=true` → 비울 노드에 `node-drain` Task.
- **Task:** 그 노드의 TaskRunner가 VM마다 대상을 고르고 기존 마이그레이션 확인·Task(`vms::start_migration`)를 써서 옮긴 뒤 끝을 기다린다. 한 번에 하나, 노드가 비어야 성공, 유지보수를 끄면 멈춤.
- **버전:** 새 Task 종류는 `require_current` 대신 노드 기능 표시(`NodeStatus.features`)로 막아, 이 기능이 있는 버전들 사이의 롤링 업그레이드에서도 비우기를 쓴다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, tokio, clap. 웹은 React·TypeScript·react-query·vitest·msw. 새로 받는 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-09-node-maintenance-design.md` (§7 "구현(계획 39)")
**선행 계획:** 계획 1~38 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 비우기 API·Task, 대상 자동 고르기, 고정 VM 켜기 | 디스크까지 복사하는 마이그레이션(로컬 디스크 VM) |
| 노드 기능 표시와 그 확인 | 유지보수 끝에 VM 되돌리기, 옮기지 못한 VM 끄기 |
| oplctl·웹, 실제 QEMU E2E | 롤링 업그레이드 자동화, 노드 재부팅 |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험. 두 노드 시험은 127.0.0.1의 mTLS gRPC와 가짜 하이퍼바이저. opld E2E는 기능 `migration-it` 뒤에 있고 실제 QEMU가 필요하다(이 Mac은 brew QEMU·tcg, CI linux-kvm 작업이 KVM으로 돌린다).

## 설계 결정

- **한 번에 하나:** 마이그레이션 대역폭은 노드마다라 동시에 해도 빨라지지 않고 대상 메모리 계산이 단순하다.
- **성공 = 노드가 빔:** 비우는 동안 들어오던 VM도 옮기고, 실행 중 VM이 남으면 실패(이름과 이유). 성공한 노드는 HA VM이 없으니 opld를 멈추면 watchdog이 꺼진다.
- **기능 표시:** 노드가 등록과 1분 주기에 `features`를 적어, 옛 리더가 지워도 되살아난다.

## 파일 구조

```
crates/opl-types/src/{node.rs, task.rs}
crates/opl-api/src/{drain.rs, tasks/drain.rs, tasks.rs, vms.rs, scheduler.rs, cluster.rs, lib.rs}, crates/opl-api/tests/{drain.rs, two_nodes.rs, vms.rs, common/mod.rs, …}
crates/opld/src/{bootstrap.rs, member.rs}, crates/opld/tests/migration.rs
crates/oplctl/src/{cli.rs, client.rs}
web/src/nodes/{NodesPage.tsx, NodesPage.test.tsx}, web/src/vms/VmListPage.test.tsx, web/src/i18n/*.json
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

### Task 1: 비우기 Task 종류와 유지보수 노드에 고정된 VM 켜기

`TaskOp::NodeDrain`(`node-drain`, 이 Task에서는 실행하면 실패 — Task 2가 채움), 마이그레이션의 확인·Task 만들기를 crate 함수 `vms::start_migration`으로 뽑기(`migrate_vm` 동작 그대로), 유지보수 노드에 고정된 꺼진 VM을 켜면 다른 노드로 다시 고름(못 하면 409 `node_unavailable`), 만들기·고치기에서 유지보수 노드를 새로 고르면 409.

**Files:**
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/vms.rs`
- Modify: `crates/opl-types/src/task.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/tasks.rs`에서

```rust
        },
    };
```

을 다음으로 바꾼다:

```rust
        },
        // 아직 실행하지 않는다(만드는 API가 없다).
        TaskOp::NodeDrain => Err("node drain is not implemented yet".into()),
    };
```

`crates/opl-api/src/vms.rs` 전체:

```rust
//! VM 생성·수정·삭제와 동작(start, stop, reboot).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::network::USER_NETWORK;
use opl_types::node::{Node, NodeSpec, ha_capable};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::storage::{VolumePhase, is_shared};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{MacAddr, PowerState, Vm, VmPhase, VmSpec, VmStatus};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::scheduler::{Inventory, node_fits, schedule};
use crate::snapshots::{
    check_no_migration, check_no_revert_task, check_no_snapshot_task, check_not_busy,
    check_not_migrating, count_snapshots,
};
use crate::state::AppState;
use crate::tasks::create_task;

/// 원하는 상태도 실제 상태도 꺼져 있다. 삭제와 노드·디스크 변경은 이때만 할 수 있다.
pub(crate) fn is_stopped(vm: &Vm) -> bool {
    vm.spec.power == PowerState::Stopped
        && matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

fn vm_running(vm: &Vm, what: &str) -> ApiError {
    ApiError::conflict(
        "vm_running",
        format!(
            "cannot {what} VM {:?} while it is running; stop it first",
            vm.name
        ),
    )
}

/// 저장하기 전에 spec을 검증하고 채운다: 빠진 MAC 할당, 참조 확인, 필요하면 노드 배치.
/// `id`는 이 spec을 가질 VM이다(자기 자신의 MAC과 디스크는 충돌로 보지 않는다).
pub(crate) fn prepare(state: &AppState, id: Uuid, spec: &mut VmSpec) -> Result<(), ApiError> {
    let inv = Inventory::load(&state.cluster)?;
    let others = inv.vms.iter().filter(|v| v.id != id);

    let mut used: BTreeSet<MacAddr> = others
        .clone()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in &spec.nics {
        if let Some(mac) = nic.mac
            && !used.insert(mac)
        {
            return Err(ApiError::conflict(
                "mac_in_use",
                format!("MAC address {mac} is already in use"),
            ));
        }
    }
    for nic in spec.nics.iter_mut().filter(|n| n.mac.is_none()) {
        let mac = std::iter::repeat_with(MacAddr::generate)
            .find(|m| !used.contains(m))
            .expect("random MACs eventually differ");
        used.insert(mac);
        nic.mac = Some(mac);
    }
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if !inv.networks.iter().any(|n| n.name == nic.network) {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    check_security_groups(state, spec)?;
    let mut disks = BTreeSet::new();
    for disk in &spec.disks {
        if !disks.insert(&disk.volume) {
            return Err(ApiError::bad_request(format!(
                "volume {:?} is attached more than once",
                disk.volume
            )));
        }
        let Some(volume) = inv.volumes.iter().find(|v| v.name == disk.volume) else {
            return Err(ApiError::bad_request(format!(
                "volume {:?} not found",
                disk.volume
            )));
        };
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
        if let Some(other) = others
            .clone()
            .find(|v| v.spec.disks.iter().any(|d| d.volume == disk.volume))
        {
            return Err(ApiError::conflict(
                "volume_in_use",
                format!(
                    "volume {:?} is already attached to VM {:?}",
                    disk.volume, other.name
                ),
            ));
        }
    }
    // 이미 그 노드에 있는 VM(실행 중이거나 고정된 채 꺼진 VM)의 다른 고치기는 유지보수로 막지 않는다.
    let current = inv
        .vms
        .iter()
        .find(|v| v.id == id)
        .and_then(|v| v.spec.node);
    if let Some(id) = spec.node {
        let Some(node) = inv.nodes.iter().find(|n| n.id == id) else {
            return Err(ApiError::bad_request(format!("node {id} not found")));
        };
        check_not_witness(node)?;
        if current != Some(id) {
            check_not_in_maintenance(node)?;
        }
        check_ha_capable(node, spec)?;
        check_node_arch(node, spec)?;
        check_firewall(node, spec)?;
    }
    if spec.power == PowerState::Running && spec.node.is_none() {
        spec.node = Some(
            schedule(id, spec, &inv).map_err(|e| ApiError::conflict("no_node", e.to_string()))?,
        );
    }
    Ok(())
}

/// NIC의 보안 그룹이 모두 있고, 그룹을 건 NIC가 `user` 네트워크가 아닌지. 아니면 400.
pub(crate) fn check_security_groups(state: &AppState, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.nics.iter().all(|n| n.security_groups.is_empty()) {
        return Ok(());
    }
    let (_, groups) = state.cluster.list::<SecurityGroupSpec>()?;
    for (i, nic) in spec.nics.iter().enumerate() {
        if nic.security_groups.is_empty() {
            continue;
        }
        // user 네트워크는 QEMU 안의 NAT라 노드의 방화벽 규칙이 닿지 않는다.
        if nic.network == USER_NETWORK {
            return Err(ApiError::bad_request(format!(
                "NIC {i}: security groups cannot be used on the {USER_NETWORK:?} network"
            )));
        }
        for name in &nic.security_groups {
            if !groups.iter().any(|g| &g.name == name) {
                return Err(ApiError::bad_request(format!(
                    "security group {name:?} not found"
                )));
            }
        }
    }
    Ok(())
}

/// 위트니스 노드면 409 `node_unavailable`. 위트니스는 VM을 돌리지 않는다.
fn check_not_witness(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is a witness node and cannot run VMs", node.name),
        ));
    }
    Ok(())
}

/// 유지보수 노드면 409 `node_unavailable`. 유지보수 노드에는 VM을 새로 놓지 않는다.
fn check_not_in_maintenance(node: &Node) -> Result<(), ApiError> {
    if node.spec.maintenance {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is in maintenance and cannot take VMs; end maintenance first",
                node.name
            ),
        ));
    }
    Ok(())
}

/// HA VM인데 노드가 HA 가능 노드(펜싱이 켜진 노드)가 아니면 409 `ha_unsupported`.
fn check_ha_capable(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.ha && !ha_capable(node) {
        return Err(ApiError::conflict(
            "ha_unsupported",
            format!(
                "node {:?} does not support HA (fencing is off) and cannot run HA VMs",
                node.name
            ),
        ));
    }
    Ok(())
}

/// 보안 그룹을 건 NIC가 있는데 노드에 방화벽(nftables)이 없으면 409 `firewall_unsupported`.
fn check_firewall(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.has_firewall() && !node.status.firewall {
        return Err(ApiError::conflict(
            "firewall_unsupported",
            format!(
                "node {:?} has no firewall (nftables) for security groups",
                node.name
            ),
        ));
    }
    Ok(())
}

/// 노드의 arch가 머신과 다르면 400 `machine_arch_mismatch`. arch를 모르는 노드(옛 버전)는 막지 않는다.
/// 업그레이드 중에도 VM을 고칠 수 있게.
pub(crate) fn check_node_arch(node: &Node, spec: &VmSpec) -> Result<(), ApiError> {
    if let Some(arch) = node.status.arch
        && arch != spec.machine.arch()
    {
        // Debug 이름을 소문자로 바꾸면 API의 값(q35, x86_64 등)과 같다.
        let name = |v: &dyn std::fmt::Debug| format!("{v:?}").to_lowercase();
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "machine_arch_mismatch",
            format!(
                "machine type {} needs an {} node, but node {:?} is {}",
                name(&spec.machine),
                name(&spec.machine.arch()),
                node.name,
                name(&arch)
            ),
        ));
    }
    Ok(())
}

/// 빠진 MAC을 채우고, 켜진 채로 만들면 노드를 고른다.
#[utoipa::path(post, path = "/vms", tag = "vms",
    request_body = CreateRequest<VmSpec>,
    responses((status = 201, body = Resource<VmSpec, VmStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<VmSpec>>,
) -> Result<(StatusCode, Json<Vm>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    check_name(&req.name)?;
    let mut vm: Vm = Resource::new(req.name, req.spec);
    vm.labels = req.labels;
    let _lock = state.vm_lock.lock().await;
    prepare(&state, vm.id, &mut vm.spec)?;
    vm.revision = state.cluster.create(&vm).await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`와 `disks`는 바꿀 수 없고 `ha`를 켤 수 없다. 보안 그룹을
/// 건 NIC가 있으면 실행 중에는 NIC의 `security_groups`만 바꿀 수 있다. NIC의
/// `security_groups`는 곧바로, 다른 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<VmSpec>,
    responses((status = 200, body = Resource<VmSpec, VmStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<VmSpec>>,
) -> Result<Json<Vm>, ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let _lock = state.vm_lock.lock().await;
        let current = lookup::<VmSpec>(&state.cluster, &key)?;
        check_no_migration(&state, &current)?;
        check_no_snapshot_task(&state, &current)?;
        if let Some(expected) = expected
            && expected != current.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current.revision,
            })
            .into());
        }
        let mut spec = req.spec.clone();
        prepare(&state, current.id, &mut spec)?;
        if spec.node != current.spec.node && !is_stopped(&current) {
            return Err(vm_running(&current, "move"));
        }
        if spec.disks != current.spec.disks && !is_stopped(&current) {
            return Err(vm_running(&current, "change the disks of"));
        }
        // 방화벽 규칙은 spec의 NIC 순번·MAC으로 tap을 정한다. 실행 중에 NIC 구성이 바뀌면 도는 tap과
        // 어긋나 막히지 않은 채가 되므로, 방화벽 NIC가 있으면(전이나 뒤) 그룹만 바꿀 수 있다.
        let firewalled = |s: &VmSpec| s.nics.iter().any(|n| !n.security_groups.is_empty());
        let without_groups = |s: &VmSpec| {
            let mut nics = s.nics.clone();
            nics.iter_mut().for_each(|n| n.security_groups.clear());
            nics
        };
        if (firewalled(&spec) || firewalled(&current.spec))
            && without_groups(&spec) != without_groups(&current.spec)
            && !is_stopped(&current)
        {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot change the NICs of VM {:?} while it is running; only security groups can change",
                    current.name
                ),
            ));
        }
        // 방화벽이 없어도 NIC 개수·순서·MAC은 그대로 둔다. 그룹 없이 바꾼 뒤 그룹을 걸면 위 검사를 비켜
        // 규칙의 tap이 도는 tap과 어긋난다.
        let macs = |s: &VmSpec| s.nics.iter().map(|n| n.mac).collect::<Vec<_>>();
        if macs(&spec) != macs(&current.spec) && !is_stopped(&current) {
            return Err(vm_running(
                &current,
                "change the number, order or MAC addresses of the NICs of",
            ));
        }
        // 노드가 과반수와 끊긴 동안 켜면 그 노드는 HA인 줄 모르고 펜싱하지 않는다. 끄기는 언제든.
        if spec.ha && !current.spec.ha && !is_stopped(&current) {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot turn on HA for VM {:?} while it is running; turn on HA while the VM is stopped",
                    current.name
                ),
            ));
        }
        match state
            .cluster
            .update_spec(
                current.id,
                Some(current.revision),
                req.labels.clone(),
                &spec,
            )
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
        return Ok(Json(lookup::<VmSpec>(
            &state.cluster,
            &current.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the VM kept changing while being updated; try again",
    ))
}

/// 꺼진 VM만 지울 수 있다. 스냅샷이 남아 있으면 409 `vm_has_snapshots`, 끝나지 않은 스냅샷·복제
/// Task가 있으면 409 `vm_busy`(복제는 원본 VM과 그 스냅샷을 읽는다).
#[utoipa::path(delete, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    check_no_migration(&state, &vm)?;
    if !is_stopped(&vm) {
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
    check_no_snapshot_task(&state, &vm)?;
    state
        .cluster
        .delete::<VmSpec>(vm.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ActionQuery {
    /// `stop`에만 쓴다. `force`면 ACPI 응답을 기다리지 않는다.
    #[serde(default)]
    #[param(inline)]
    pub mode: StopMode,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StopMode {
    #[default]
    Acpi,
    Force,
}

/// `start`와 `stop`은 `spec.power`를 바꾸고, 그 결과를 지켜볼 Task를 돌려준다(202).
/// `stop?mode=force`와 `reboot`은 대상 노드의 Task 실행기가 수행한다.
#[utoipa::path(post, path = "/vms/{key}/actions/{action}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("action" = String, Path, description = "start, stop or reboot"), ActionQuery),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn vm_action(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, action)): Path<(String, String)>,
    Query(query): Query<ActionQuery>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    if query.mode == StopMode::Force && action != "stop" {
        return Err(ApiError::bad_request("mode=force applies only to stop"));
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    // 스냅샷 Task가 끝나지 않았으면 막는다. 시작한 스냅샷 Task는 실행기의 시간 제한으로 끝난다.
    // 강제 정지는 되돌리기만 막는다. 저장이 멈춘 만들기 중에도 VM을 끌 수 있어야 한다.
    // 마이그레이션 중에는 모두 막는다. 실패하면 원본이 계속 돌고, 시간 제한으로 끝난다.
    check_no_migration(&state, &vm)?;
    if query.mode == StopMode::Force {
        check_no_revert_task(&state, &vm)?;
    } else {
        check_no_snapshot_task(&state, &vm)?;
    }
    let id = vm.id;
    let (op, vm) = match action.as_str() {
        "start" => {
            let _lock = state.vm_lock.lock().await;
            let vm = modify_spec(&state, id, |spec| {
                // 고정된 노드는 VM을 고칠 때 확인했지만, 그때 arch를 모르던 노드일 수 있다.
                // 유지보수 노드에 고정된 꺼진 VM은 다른 노드를 고른다. 아직 그 노드에서 돌거나 멈추는
                // 중인 VM은 옮기지 않는다.
                if let Some(node) = spec.node
                    && let Some(node) = state.cluster.get::<NodeSpec>(node)?
                {
                    if node.spec.maintenance && stopped_now(&state, id)? {
                        let inv = Inventory::load(&state.cluster)?;
                        let other = schedule(id, spec, &inv).map_err(|_| {
                            ApiError::conflict(
                                "node_unavailable",
                                format!(
                                    "node {:?} is in maintenance and no other node can run this VM; end maintenance first",
                                    node.name
                                ),
                            )
                        })?;
                        spec.node = Some(other);
                    } else {
                        check_node_arch(&node, spec)?;
                    }
                }
                if spec.node.is_none() {
                    let inv = Inventory::load(&state.cluster)?;
                    let node = schedule(id, spec, &inv)
                        .map_err(|e| ApiError::conflict("no_node", e.to_string()))?;
                    spec.node = Some(node);
                }
                spec.power = PowerState::Running;
                Ok(())
            })
            .await?;
            (TaskOp::VmStart, vm)
        }
        "stop" => {
            let vm = modify_spec(&state, id, |spec| {
                spec.power = PowerState::Stopped;
                Ok(())
            })
            .await?;
            let op = match query.mode {
                StopMode::Acpi => TaskOp::VmStop,
                StopMode::Force => TaskOp::VmForceStop,
            };
            (op, vm)
        }
        "reboot" => {
            let vm = lookup::<VmSpec>(&state.cluster, &id.to_string())?;
            if vm.spec.power != PowerState::Running || vm.status.phase != VmPhase::Running {
                return Err(ApiError::conflict(
                    "vm_not_running",
                    format!("VM {:?} is not running", vm.name),
                ));
            }
            (TaskOp::VmReboot, vm)
        }
        _ => {
            return Err(ApiError::not_found(format!(
                "unknown action {action:?} (start, stop, reboot)"
            )));
        }
    };
    // 배치되지 않은 VM은 돌고 있지 않다. 정지 Task는 이 노드에서 곧바로 끝난다.
    let node = vm.spec.node.unwrap_or(state.node);
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 저장소의 VM이 지금 꺼져 있는지([`is_stopped`]). [`modify_spec`] 안에서 부르면 그 쓰기가 같은 revision을
/// 확인하므로 읽은 뒤 바뀐 상태로 쓰지 않는다.
fn stopped_now(state: &AppState, id: Uuid) -> Result<bool, ApiError> {
    Ok(state
        .cluster
        .get::<VmSpec>(id)?
        .is_some_and(|vm| is_stopped(&vm)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrateRequest {
    /// 옮겨 갈 노드(이름이나 id)
    pub node: String,
}

/// 실행 중인 VM을 다른 노드로 라이브 마이그레이션할 Task를 돌려준다(202). Task는 지금 노드의 실행기가
/// 한다. 디스크가 모두 공유 풀에 있어야 하고, HVF(macOS) 노드는 원본도 대상도 될 수 없다(409
/// `migration_unsupported`). 대상 노드가 위트니스이거나 오프라인이거나 유지보수 중이면 409 `node_unavailable`,
/// HA VM인데 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`,
/// 스케줄러가 고를 수 없는 노드(풀, 네트워크, 남은 메모리)면 409 `no_capacity`다.
#[utoipa::path(post, path = "/vms/{key}/migrate", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrateRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn migrate_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<MigrateRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let target = lookup::<NodeSpec>(&state.cluster, &req.node)?;
    let task = start_migration(&state, vm.id, target.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`migrate_vm`]의 확인을 하고 VM `vm`을 노드 `target`으로 옮길 `VmMigrate` Task를 원본 노드에 만든다.
/// 노드 비우기 Task도 쓴다. 오류는 [`migrate_vm`]과 같다.
pub(crate) async fn start_migration(
    state: &AppState,
    vm: Uuid,
    target: Uuid,
) -> Result<Task, ApiError> {
    // 두 요청이 함께 검사를 지나 Task를 둘 만들지 않게 한다.
    let _lock = state.vm_lock.lock().await;
    let vm = lookup::<VmSpec>(&state.cluster, &vm.to_string())?;
    let target = lookup::<NodeSpec>(&state.cluster, &target.to_string())?;
    if vm.spec.node == Some(target.id) {
        return Err(ApiError::bad_request(format!(
            "VM {:?} is already on node {:?}",
            vm.name, target.name
        )));
    }
    // 이미 옮기는 중인 VM은 아래의 vm_busy로 거절한다.
    let source = vm.spec.node.filter(|_| {
        vm.spec.power == PowerState::Running
            && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating)
    });
    let Some(source) = source else {
        return Err(ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running", vm.name),
        ));
    };
    check_not_busy(state, &vm)?;
    check_no_snapshot_task(state, &vm)?;
    let inv = Inventory::load(&state.cluster)?;
    check_no_resize_task(state, &inv, &vm)?;
    for disk in &vm.spec.disks {
        let pool = inv
            .volumes
            .iter()
            .find(|v| v.name == disk.volume)
            .and_then(|v| inv.pools.iter().find(|p| p.name == v.spec.pool));
        if !pool.is_some_and(is_shared) {
            return Err(ApiError::conflict(
                "migration_unsupported",
                format!(
                    "disk {:?} is not on a shared storage pool; only VMs whose disks are all on shared pools can be migrated",
                    disk.volume
                ),
            ));
        }
    }
    for node in [Some(&target), inv.nodes.iter().find(|n| n.id == source)]
        .into_iter()
        .flatten()
    {
        // HVF는 이 노드의 hypervisor 이름으로만 알 수 있다(macOS 노드).
        if node.status.hypervisor == "qemu-hvf" {
            return Err(ApiError::conflict(
                "migration_unsupported",
                format!(
                    "node {:?} runs macOS (HVF), which cannot migrate VMs",
                    node.name
                ),
            ));
        }
    }
    check_not_witness(&target)?;
    check_ha_capable(&target, &vm.spec)?;
    if !target.status.online || target.spec.maintenance {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is offline or in maintenance", target.name),
        ));
    }
    check_node_arch(&target, &vm.spec)?;
    node_fits(&vm.spec, &inv, &target).map_err(|e| ApiError::conflict("no_capacity", e))?;
    let free = inv.free_memory_mib(&target, vm.id);
    if free < i64::from(vm.spec.memory_mib) {
        return Err(ApiError::conflict(
            "no_capacity",
            format!(
                "node {:?} has {free} MiB of unassigned memory, but VM {:?} needs {} MiB",
                target.name, vm.name, vm.spec.memory_mib
            ),
        ));
    }
    let op = TaskOp::VmMigrate { node: target.id };
    create_task(&state.cluster, op, vm.id, source).await
}

/// VM 디스크의 크기 늘리기 Task가 끝나지 않았으면 409 `vm_busy`. 실행 중인 VM의 디스크는 QMP로
/// 늘리므로 원본 QEMU가 사라지면 안 된다.
fn check_no_resize_task(state: &AppState, inv: &Inventory, vm: &Vm) -> Result<(), ApiError> {
    let disks: BTreeSet<Uuid> = inv
        .volumes
        .iter()
        .filter(|v| vm.spec.disks.iter().any(|d| d.volume == v.name))
        .map(|v| v.id)
        .collect();
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        disks.contains(&t.spec.target)
            && !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VolumeResize { .. })
    });
    if busy {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "a disk of VM {:?} is being resized; try again when the task has finished",
                vm.name
            ),
        ));
    }
    Ok(())
}

/// spec을 읽고 바꿔 revision을 확인하며 쓴다. reconciler의 status 쓰기와 부딪치면 다시 읽는다.
/// 읽은 VM이 마이그레이션 중이면 409 `vm_busy`다(호출자의 검사 뒤에 시작한 마이그레이션). 이 쓰기가
/// 먼저 적용되면 마이그레이션 Task가 바뀐 spec을 보고 그만둔다.
pub(crate) async fn modify_spec(
    state: &AppState,
    id: Uuid,
    f: impl Fn(&mut VmSpec) -> Result<(), ApiError>,
) -> Result<Vm, ApiError> {
    for _ in 0..10 {
        let vm = state
            .cluster
            .get::<VmSpec>(id)?
            .ok_or_else(|| ApiError::not_found(format!("vm {id} not found")))?;
        check_not_migrating(&vm)?;
        let mut spec = vm.spec.clone();
        f(&mut spec)?;
        if spec == vm.spec {
            return Ok(vm);
        }
        match state
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
        return state
            .cluster
            .get::<VmSpec>(id)?
            .ok_or_else(|| ApiError::not_found(format!("vm {id} not found")));
    }
    Err(ApiError::conflict(
        "busy",
        "the VM kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/tests/vms.rs`에서

```rust
use serde_json::{Value, json};
```

을 다음으로 바꾼다:

```rust
use serde_json::{Value, json};
use uuid::Uuid;
```

`crates/opl-api/tests/vms.rs`에서

```rust

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. HA는 멈춘 VM에서만 켤 수 있다(끄기는 언제든).
```

을 다음으로 바꾼다:

```rust

/// 실행기도 reconciler도 없는 노드를 저장소에만 만든다(온라인, aarch64).
async fn add_node(env: &Env, name: &str) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status.online = true;
    node.status.memory_mib = 8192;
    node.status.arch = Some(Arch::Aarch64);
    env.cluster.create(&node).await.unwrap();
    node.id
}

/// 노드의 유지보수 모드를 바꾼다.
async fn set_maintenance(env: &Env, id: Uuid, maintenance: bool) {
    let mut node = env.cluster.get::<NodeSpec>(id).unwrap().unwrap();
    node.spec.maintenance = maintenance;
    env.cluster
        .update_spec(id, None, node.labels.clone(), &node.spec)
        .await
        .unwrap();
}

/// 유지보수 노드에 고정된 꺼진 VM을 켜면 스케줄러가 다른 노드를 고른다.
#[tokio::test]
async fn a_vm_pinned_to_a_maintenance_node_starts_elsewhere() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(node2);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    set_maintenance(&env, node2, true).await;

    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");
}

/// 다른 노드를 고를 수 없으면 409 `node_unavailable`이고 VM은 그대로다.
#[tokio::test]
async fn start_fails_when_only_the_maintenance_node_can_run_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut req = vm_request("web");
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    set_maintenance(&env, env.node, true).await;

    let (status, body) = admin.post("/vms/web/actions/start", Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"node1\" is in maintenance and no other node can run this VM; end maintenance first"
    );
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["spec"]["power"], "stopped");
}

/// 만들거나 고칠 때 유지보수 노드를 고르면 409 `node_unavailable`이다. 이미 그 노드에 있는 VM의 다른
/// 고치기는 막지 않고, 그 노드에서 도는 VM을 다시 켜도 옮기지 않는다.
#[tokio::test]
async fn a_maintenance_node_cannot_be_chosen() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let node2 = add_node(&env, "node2").await;
    set_maintenance(&env, node2, true).await;

    let mut req = vm_request("db");
    req["spec"]["node"] = json!(node2);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");
    assert_eq!(
        body["message"],
        "node \"node2\" is in maintenance and cannot take VMs; end maintenance first"
    );

    let (status, vm) = admin.post("/vms", vm_request("web")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let mut spec = vm["spec"].clone();
    spec["node"] = json!(node2);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "node_unavailable");

    // 이 노드에서 도는 VM: 이 노드가 유지보수가 되어도 고칠 수 있고, start는 옮기지 않는다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    set_maintenance(&env, env.node, true).await;
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(512);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    assert_eq!(vm["spec"]["node"], env.node.to_string());
    assert_eq!(vm["status"]["phase"], "running");

    // 꺼진 뒤에도 그 노드에 고정된 채로 고칠 수 있다.
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let (_, vm) = admin.get("/vms/web").await;
    let mut spec = vm["spec"].clone();
    spec["memory_mib"] = json!(256);
    let (status, body) = admin.put("/vms/web", json!({ "spec": spec }), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// HA VM은 HA 가능 노드(펜싱이 켜진 노드)에서만 돈다. HA는 멈춘 VM에서만 켤 수 있다(끄기는 언제든).
```

`crates/opl-types/src/task.rs`에서

```rust
    NodeUpgrade,
}
```

을 다음으로 바꾼다:

```rust
    NodeUpgrade,
    /// 대상 노드(Task의 `node`와 같다)를 비운다: 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션한다.
    /// 노드는 API가 먼저 유지보수로 바꾼다.
    NodeDrain,
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
    fn node_drain_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-drain" });
        assert_eq!(serde_json::to_value(TaskOp::NodeDrain).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeDrain
        );
    }

    #[test]
    fn new_task_is_pending() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1522개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 노드 비우기 Task 종류와 유지보수 노드에 고정된 VM 켜기 — TaskOp::NodeDrain(kebab node-drain, 필드 없음, 대상은 노드; 아직 실행하면 \"node drain is not implemented yet\"으로 실패), migrate_vm의 확인·Task 만들기를 crate 함수 vms::start_migration(state, vm, target)으로 뽑음(vm_lock은 그 안에서, 동작 그대로), VM start가 유지보수 노드에 고정된 꺼진 VM을 스케줄러로 다른 노드에 놓고 고를 노드가 없으면 409 node_unavailable(아직 그 노드에서 돌거나 멈추는 중인 VM은 옮기지 않음), prepare(만들기·고치기·스냅샷 되돌리기)가 유지보수 노드를 새로 고르면 409 node_unavailable(이미 그 노드에 있는 VM의 다른 고치기는 막지 않음), schema.d.ts 다시 만듦. 시험: TaskOp JSON, 켜기 다시 고르기·409, 만들기·고치기 409, 실행 중 VM 고치기·다시 켜기는 그대로"
```

---

### Task 2: 노드 비우기 API와 Task

`POST /api/v1/nodes/{key}/drain`(admin): 노드를 유지보수로 바꾸고 `node-drain` Task를 그 노드에 만든다. Task는 실행 중 VM을 하나씩 다른 노드로 라이브 마이그레이션하고(대상은 `node_fits`·HVF 아님·남은 메모리), 들어오던 VM도 옮긴 뒤 노드가 비어야 성공한다. 유지보수를 끄면 멈춘다. 새 Task 종류는 노드 기능 표시 `NodeStatus.features`(`node-drain`)로 막는다 — 섞인 버전에서도 모든 노드가 기능을 적었으면 된다.

**Files:**
- Create: `crates/opl-api/src/drain.rs`
- Create: `crates/opl-api/src/tasks/drain.rs`
- Create: `crates/opl-api/tests/drain.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/vms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust

/// 옛 노드 이름들(이름 순)로 `cluster_upgrading`을 만든다. 없으면 통과.
```

을 다음으로 바꾼다:

```rust

/// 기능 `feature`([`opl_types::node::FEATURES`])를 적지 않은 노드(위트니스 포함)가 남아 있으면 409
/// `cluster_upgrading`으로 거절한다. 버전을 비교하지 않으므로 롤링 업그레이드 중(버전이 섞여도)
/// 모든 노드가 그 기능을 알면 통과한다.
pub(crate) fn require_feature(
    cluster: &opl_cluster::Cluster,
    feature: &str,
) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| !n.status.features.iter().any(|f| f == feature))
        .map(|n| n.name)
        .collect();
    if old.is_empty() {
        return Ok(());
    }
    old.sort();
    Err(ApiError::conflict(
        "cluster_upgrading",
        format!(
            "some nodes do not support {} yet ({}); upgrade them first",
            feature.replace('-', " "),
            old.join(", ")
        ),
    ))
}

/// 옛 노드 이름들(이름 순)로 `cluster_upgrading`을 만든다. 없으면 통과.
```

`crates/opl-api/src/drain.rs`를 만든다:

```rust
//! 노드 비우기 API(설계 §3.1). 노드를 유지보수로 바꾸고 비우기 Task를 그 노드에 만든다. 옮기는 일은
//! 그 노드의 실행기가 한다(`tasks::drain`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// 노드를 유지보수로 바꾸고(이미면 그대로), 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션할
/// Task(`node-drain`, 그 노드에서 돈다)를 돌려준다(202). Task가 실패해도 노드는 유지보수로 남는다.
/// 위트니스이거나 오프라인이면 409 `node_unavailable`, 그 노드의 비우기 Task가 끝나지 않았으면 409
/// `node_busy`, 비우기를 모르는 노드가 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/drain", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn drain_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is a witness node and runs no VMs", node.name),
        ));
    }
    if !node.status.online {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is offline; HA recovers the HA VMs of an offline node",
                node.name
            ),
        ));
    }
    // 이 프로세스에 함께 온 요청이 둘 다 검사를 지나 비우기 둘을 만들지 않게 한다. 다른 노드로 온
    // 요청과 겹쳐도 실행기가 노드의 줄에 세워 차례로 한다.
    static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REQUESTS.lock().await;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeDrain && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Err(ApiError::conflict(
            "node_busy",
            format!("node {:?} is already being drained", node.name),
        ));
    }
    // 롤링 업그레이드 중(버전이 섞여도) 모든 노드가 비우기 Task를 알면 된다.
    crate::cluster::require_feature(&state.cluster, "node-drain")?;
    set_maintenance(&state, &key).await?;
    let task = create_task(&state.cluster, TaskOp::NodeDrain, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드의 `maintenance`를 참으로 쓴다(revision 확인). 다른 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_maintenance(state: &AppState, key: &str) -> Result<(), ApiError> {
    for _ in 0..10 {
        let node = lookup::<NodeSpec>(&state.cluster, key)?;
        if node.spec.maintenance {
            return Ok(());
        }
        let spec = NodeSpec {
            maintenance: true,
            ..node.spec
        };
        match state
            .cluster
            .update_spec(node.id, Some(node.revision), node.labels, &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the node kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/src/lib.rs`에서

```rust
mod console;
pub mod error;
```

을 다음으로 바꾼다:

```rust
mod console;
mod drain;
pub mod error;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(updates::upgrade_node))
        .routes(routes!(cluster::get_cluster))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(updates::upgrade_node))
        .routes(routes!(drain::drain_node))
        .routes(routes!(cluster::get_cluster))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            updates: None,
        };
```

을 다음으로 바꾼다:

```rust
            updates: None,
            features: vec![],
        };
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

pub(crate) mod backup;
mod clone;
mod drain;
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
use crate::state::AppState;
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
    /// 이 노드의 API 상태. 노드 비우기 Task가 마이그레이션 API와 같은 확인으로 Task를 만들 때 쓴다
    /// (이 노드의 API와 `vm_lock`을 함께 쓴다). 없으면 비우기 Task는 실패한다.
    pub api: Option<Arc<AppState>>,
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
            api: None,
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
/// 노드 비우기는 그 노드의 줄에 서서 한 노드를 한 번에 하나만 비운다.
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
        let node_queue = task.spec.op == TaskOp::NodeDrain;
        return (is_volume_op(&task.spec.op)
            || is_snapshot_op(&task.spec.op)
            || vm_queue
            || backup_queue
            || node_queue)
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
    // 업그레이드도 오래 걸릴 수 있고 apt를 중간에 끊으면 더 나쁘다. 노드 비우기는 마이그레이션을 여럿
    // 하므로 VM마다 기다리는 시간만 둔다(`migration_timeout`).
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::BackupRestore { .. }
            | TaskOp::BackupReplicate { .. }
            | TaskOp::VmImport { .. }
            | TaskOp::NodeUpgrade
            | TaskOp::NodeDrain
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
        Ok(warning) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, warning).await
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

/// 성공한 Task가 남길 경고(`error`)가 있으면 `Ok(Some)`이다(지우지 못한 원격 사본).
async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let done = match &task.spec.op {
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
        TaskOp::BackupDelete => return backup::delete(ctx, id).await,
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
        TaskOp::NodeDrain => drain::drain(ctx, id).await,
    };
    done.map(|()| None)
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

`crates/opl-api/src/tasks/drain.rs`를 만든다:

```rust
//! 노드 비우기 Task(설계 §3.2): 비울 노드의 실행 중인 VM을 이름 순으로 하나씩 다른 노드로 라이브
//! 마이그레이션한다. 옮기지 못한 VM은 건너뛰고 끝에 이유와 함께 실패로 알린다. 이 노드로 들어오던
//! 마이그레이션은 끝나기를 기다려 그 VM도 옮기므로, 성공하면 노드가 비어 있다.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use opl_types::node::{Node, NodeSpec};
use opl_types::storage::is_shared;
use opl_types::task::{TaskOp, TaskSpec, TaskState};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::error::ApiError;
use crate::scheduler::{Inventory, node_fits};
use crate::state::AppState;
use crate::vms::start_migration;

/// VM 하나를 옮겨 본 결과
enum Outcome {
    Moved,
    /// 옮길 것이 없어졌다(꺼졌거나 지워졌거나 다른 노드로 갔다).
    Gone,
    /// 옮기지 못했다(이유)
    NotMoved(String),
}

/// 옮길 것이 없어졌다가 이 노드에 다시 나타난 VM을 다시 보는 횟수. 그 뒤에도 다시 나타나면 `busy`로
/// 남긴다(비우기가 끝나게).
const MAX_GONE: u32 = 3;

/// HVF(macOS) 노드의 hypervisor 이름. HVF는 VM을 마이그레이션하지 못한다.
const HVF: &str = "qemu-hvf";

/// 노드 `node`를 비운다. 노드의 유지보수가 끝나면 멈춘다.
pub(super) async fn drain(ctx: &Ctx, node: Uuid) -> Result<(), String> {
    let state = ctx
        .config
        .api
        .clone()
        .ok_or("this node cannot create migration tasks")?;
    // 옮긴 VM. 다시 들어오면 한 번 더 옮기지 않고 옮기지 못한 VM으로 남긴다.
    let mut moved = HashSet::new();
    // VM마다 옮길 것이 없어졌던 횟수. 다시 나타나면(예: 껐다 다시 켬) 다시 본다.
    let mut gone: HashMap<Uuid, u32> = HashMap::new();
    // 옮기지 못한 VM(id → 이름, 이유). 다시 보지 않는다.
    let mut not_moved: HashMap<Uuid, (String, String)> = HashMap::new();
    loop {
        in_maintenance(ctx, node)?;
        let (_, mut vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        vms.retain(|v| {
            v.spec.node == Some(node)
                && v.spec.power == PowerState::Running
                && !not_moved.contains_key(&v.id)
        });
        vms.sort_by(|a, b| a.name.cmp(&b.name));
        let Some(vm) = vms.into_iter().next() else {
            // 이 노드로 들어오는 마이그레이션이 끝나기를 기다려 다시 본다(들어온 VM도 옮긴다).
            if incoming(ctx, node, &not_moved)?.is_empty() {
                break;
            }
            wait_for_incoming(ctx, node, &mut not_moved).await?;
            continue;
        };
        let reason = if moved.contains(&vm.id) {
            Some("moved back during the drain".to_string())
        } else if gone.get(&vm.id).is_some_and(|n| *n >= MAX_GONE) {
            Some("busy".to_string())
        } else {
            match move_vm(ctx, &state, node, &vm).await? {
                Outcome::Moved => {
                    moved.insert(vm.id);
                    None
                }
                Outcome::Gone => {
                    *gone.entry(vm.id).or_default() += 1;
                    None
                }
                Outcome::NotMoved(reason) => Some(reason),
            }
        };
        if let Some(reason) = reason {
            tracing::warn!(vm = %vm.name, reason, "cannot move a VM off the node being drained");
            not_moved.insert(vm.id, (vm.name, reason));
        }
    }
    if not_moved.is_empty() {
        return Ok(());
    }
    let mut not_moved: Vec<(String, String)> = not_moved.into_values().collect();
    not_moved.sort();
    let list: Vec<String> = not_moved
        .iter()
        .map(|(name, reason)| format!("{name} ({reason})"))
        .collect();
    Err(format!(
        "{} VM(s) were not moved: {}",
        not_moved.len(),
        list.join(", ")
    ))
}

/// 노드를 다시 읽는다. 유지보수가 끝났거나 노드가 지워졌으면 비우기를 멈추는 오류다.
fn in_maintenance(ctx: &Ctx, node: Uuid) -> Result<Node, String> {
    let me = ctx
        .cluster
        .get::<NodeSpec>(node)
        .map_err(|e| e.to_string())?
        .ok_or("node was deleted")?;
    if !me.spec.maintenance {
        return Err("drain stopped: maintenance ended".into());
    }
    Ok(me)
}

/// 이 노드로 들어오는 마이그레이션의 VM(id, 이름): 마이그레이션 대상이 이 노드인 VM과, 끝나지 않은
/// 이 노드로의 마이그레이션 Task의 VM. 옮기지 못한 VM은 뺀다.
fn incoming(
    ctx: &Ctx,
    node: Uuid,
    not_moved: &HashMap<Uuid, (String, String)>,
) -> Result<Vec<(Uuid, String)>, String> {
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    let mut ids: Vec<Uuid> = vms
        .iter()
        .filter(|v| v.status.migration.is_some_and(|m| m.node == node))
        .map(|v| v.id)
        .chain(
            tasks
                .iter()
                .filter(|t| {
                    t.spec.op == TaskOp::VmMigrate { node } && !t.status.state.is_finished()
                })
                .map(|t| t.spec.target),
        )
        .filter(|id| !not_moved.contains_key(id))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids
        .into_iter()
        .map(|id| {
            let name = vms.iter().find(|v| v.id == id).map(|v| v.name.clone());
            (id, name.unwrap_or_else(|| id.to_string()))
        })
        .collect())
}

/// 이 노드로 들어오는 마이그레이션이 끝나기를 최대 `migration_timeout` 기다린다. 넘기면 아직 들어오는
/// VM은 `busy`로 남긴다.
async fn wait_for_incoming(
    ctx: &Ctx,
    node: Uuid,
    not_moved: &mut HashMap<Uuid, (String, String)>,
) -> Result<(), String> {
    let deadline = Instant::now() + ctx.config.migration_timeout;
    loop {
        tokio::time::sleep(ctx.config.poll_interval).await;
        in_maintenance(ctx, node)?;
        let incoming = incoming(ctx, node, not_moved)?;
        if incoming.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            for (id, name) in incoming {
                tracing::warn!(vm = %name, "a migration to the node being drained did not finish");
                not_moved.insert(id, (name, "busy".into()));
            }
            return Ok(());
        }
    }
}

/// VM 하나를 다른 노드로 옮기고 그 마이그레이션 Task가 끝나기를 기다린다. 기다리는 동안 유지보수가
/// 끝나면 옮기지 않고 비우기를 멈추는 오류다.
async fn move_vm(ctx: &Ctx, state: &AppState, node: Uuid, vm: &Vm) -> Result<Outcome, String> {
    match settle(ctx, node, vm.id).await {
        Ok(true) => {}
        Ok(false) => return Ok(Outcome::Gone),
        Err(e) => return Ok(Outcome::NotMoved(e)),
    }
    let me = in_maintenance(ctx, node)?;
    let inv = match Inventory::load(&ctx.cluster) {
        Ok(inv) => inv,
        Err(e) => return Ok(Outcome::NotMoved(e.to_string())),
    };
    let shared = vm.spec.disks.iter().all(|disk| {
        inv.volumes
            .iter()
            .find(|v| v.name == disk.volume)
            .and_then(|v| inv.pools.iter().find(|p| p.name == v.spec.pool))
            .is_some_and(is_shared)
    });
    if !shared {
        return Ok(Outcome::NotMoved("disks on a local pool".into()));
    }
    // HVF는 이 노드의 hypervisor 이름으로만 알 수 있다(macOS 노드).
    if me.status.hypervisor == HVF {
        return Ok(Outcome::NotMoved("hvf node".into()));
    }
    let Some(target) = pick_target(vm, &inv) else {
        return Ok(Outcome::NotMoved("no node with capacity".into()));
    };
    let task = match start_migration(state, vm.id, target, Some(node)).await {
        // 확인과 만들기 사이에 다른 Task가 들어왔다. 한 번 더 자리 잡기를 기다려 다시 한다.
        Err(e) if e.code == "vm_busy" => {
            match settle(ctx, node, vm.id).await {
                Ok(true) => {}
                Ok(false) => return Ok(Outcome::Gone),
                Err(e) => return Ok(Outcome::NotMoved(e)),
            }
            in_maintenance(ctx, node)?;
            start_migration(state, vm.id, target, Some(node)).await
        }
        r => r,
    };
    match task {
        Ok(task) => Ok(wait_migration(ctx, task.id).await),
        Err(e) => Ok(rejected(e)),
    }
}

/// VM을 받을 수 있는 노드(스케줄러의 조건을 지나고 HVF가 아니며 남은 메모리가 VM 메모리 이상) 가운데
/// 남은 메모리가 가장 많은 노드. 같으면 id가 작은 노드다. 스케줄러는 유지보수 노드(이 노드)를 고르지
/// 않는다.
fn pick_target(vm: &Vm, inv: &Inventory) -> Option<Uuid> {
    inv.nodes
        .iter()
        .filter(|n| n.status.hypervisor != HVF && node_fits(&vm.spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm.id), n.id))
        .filter(|(free, _)| *free >= i64::from(vm.spec.memory_mib))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
}

/// 마이그레이션 API가 거절한 결과. VM이 꺼졌거나 지워졌거나 이미 다른 노드로 갔으면 옮길 것이 없다.
fn rejected(e: ApiError) -> Outcome {
    match e.code {
        "vm_not_running" | "not_found" | "vm_moved" => Outcome::Gone,
        "vm_busy" => Outcome::NotMoved("busy".into()),
        _ => Outcome::NotMoved(e.message),
    }
}

/// 마이그레이션 Task가 끝나기를 기다린다. Task는 단계마다 시간 제한이 있지만, 읽기가 계속 실패하거나
/// 끝나지 않으면 `migration_timeout`의 두 배와 1분 뒤에 그만 기다린다.
async fn wait_migration(ctx: &Ctx, task: Uuid) -> Outcome {
    let deadline = Instant::now() + ctx.config.migration_timeout * 2 + Duration::from_secs(60);
    loop {
        match ctx.cluster.get::<TaskSpec>(task) {
            Ok(Some(t)) if t.status.state == TaskState::Succeeded => return Outcome::Moved,
            Ok(Some(t)) if t.status.state == TaskState::Failed => {
                return Outcome::NotMoved(
                    t.status.error.unwrap_or_else(|| "migration failed".into()),
                );
            }
            Ok(Some(_)) => {}
            Ok(None) => return Outcome::NotMoved("the migration task was deleted".into()),
            Err(e) => tracing::warn!(task = %task, error = %e, "cannot read task; will retry"),
        }
        if Instant::now() >= deadline {
            return Outcome::NotMoved("migration task did not finish".into());
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// VM이 이 노드에서 자리 잡을 때까지(실행 중이고 끝나지 않은 Task가 없을 때까지) 기다린다. 옮길 것이
/// 없어졌으면 `Ok(false)`, `migration_timeout`을 넘기면 `busy`다.
async fn settle(ctx: &Ctx, node: Uuid, id: Uuid) -> Result<bool, String> {
    let deadline = Instant::now() + ctx.config.migration_timeout;
    loop {
        let vm = ctx.cluster.get::<VmSpec>(id).map_err(|e| e.to_string())?;
        let Some(vm) =
            vm.filter(|v| v.spec.node == Some(node) && v.spec.power == PowerState::Running)
        else {
            return Ok(false);
        };
        let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
        let busy = vm.status.phase != VmPhase::Running
            || vm.status.migration.is_some()
            || tasks
                .iter()
                .any(|t| t.spec.target == id && !t.status.state.is_finished());
        if !busy {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Err("busy".into());
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}
```

`crates/opl-api/src/vms.rs`에서

```rust
    let target = lookup::<NodeSpec>(&state.cluster, &req.node)?;
    let task = start_migration(&state, vm.id, target.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
```

을 다음으로 바꾼다:

```rust
    let target = lookup::<NodeSpec>(&state.cluster, &req.node)?;
    let task = start_migration(&state, vm.id, target.id, None).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
```

`crates/opl-api/src/vms.rs`에서

```rust
/// [`migrate_vm`]의 확인을 하고 VM `vm`을 노드 `target`으로 옮길 `VmMigrate` Task를 원본 노드에 만든다.
/// 노드 비우기 Task도 쓴다. 오류는 [`migrate_vm`]과 같다.
pub(crate) async fn start_migration(
```

을 다음으로 바꾼다:

```rust
/// [`migrate_vm`]의 확인을 하고 VM `vm`을 노드 `target`으로 옮길 `VmMigrate` Task를 원본 노드에 만든다.
/// 노드 비우기 Task도 쓴다. 오류는 [`migrate_vm`]과 같다. `source`가 있으면(비우기) VM이 아직 그 노드에
/// 있는지도 보고, 아니면 409 `vm_moved`다(이미 다른 데로 갔다).
pub(crate) async fn start_migration(
```

`crates/opl-api/src/vms.rs`에서

```rust
    target: Uuid,
) -> Result<Task, ApiError> {
```

을 다음으로 바꾼다:

```rust
    target: Uuid,
    source: Option<Uuid>,
) -> Result<Task, ApiError> {
```

`crates/opl-api/src/vms.rs`에서

```rust
    let target = lookup::<NodeSpec>(&state.cluster, &target.to_string())?;
    if vm.spec.node == Some(target.id) {
```

을 다음으로 바꾼다:

```rust
    let target = lookup::<NodeSpec>(&state.cluster, &target.to_string())?;
    if let Some(source) = source
        && vm.spec.node != Some(source)
    {
        return Err(ApiError::conflict(
            "vm_moved",
            format!("VM {:?} is no longer on node {source}", vm.name),
        ));
    }
    if vm.spec.node == Some(target.id) {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
use opl_types::cluster::ClusterSpec;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_types::cluster::ClusterSpec;
use opl_types::node::{FEATURES, NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            updates: None,
        };
        // 이 노드는 지금 빌드를 돌린다.
        me.status.version = Some(BUILD.into());
        cluster.create(&me).await.unwrap();
```

을 다음으로 바꾼다:

```rust
            updates: None,
            features: vec![],
        };
        // 이 노드는 지금 빌드를 돌린다.
        me.status.version = Some(BUILD.into());
        me.status.features = FEATURES.iter().map(|f| f.to_string()).collect();
        cluster.create(&me).await.unwrap();
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let credentials: Arc<ImportCredentials> = Arc::default();
        let mut config = runner_config(node, dir.path());
        config.import_credentials = credentials.clone();
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv_dyn.clone(),
            reconciler.handle(),
            config,
        )
        .await;
        let mut state = AppState::new(cluster.clone(), node, hv_dyn, dir.path().join("uploads"));
        state.import_credentials = credentials;
```

을 다음으로 바꾼다:

```rust
        let credentials: Arc<ImportCredentials> = Arc::default();
        let mut state = AppState::new(
            cluster.clone(),
            node,
            hv_dyn.clone(),
            dir.path().join("uploads"),
        );
        state.import_credentials = credentials;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        let state = Arc::new(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
```

을 다음으로 바꾼다:

```rust
        let state = Arc::new(state);
        let mut config = runner_config(node, dir.path());
        config.import_credentials = state.import_credentials.clone();
        // opld처럼 노드 비우기 Task가 API 상태를 쓴다.
        config.api = Some(state.clone());
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv_dyn,
            reconciler.handle(),
            config,
        )
        .await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        config.import_credentials = self.state.import_credentials.clone();
        tweak(&mut config);
```

을 다음으로 바꾼다:

```rust
        config.import_credentials = self.state.import_credentials.clone();
        config.api = Some(self.state.clone());
        tweak(&mut config);
```

`crates/opl-api/tests/drain.rs`를 만든다:

```rust
//! 노드 비우기 API(`POST /nodes/{key}/drain`): 권한, 노드 확인, 끝나지 않은 비우기, 유지보수로 바꾸기,
//! 옛 노드. 이 노드 하나만 실제로 돌므로 비우기 Task는 옮기지 못하는 이유만 본다. 실제로 옮기는 것은
//! `two_nodes.rs`가 본다.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Client, Env, MOUNT_ROOT, vm_request};
use opl_driver::DirStorage;
use opl_types::node::{FEATURES, Node, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{Volume, VolumePhase, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::version::BUILD;
use opl_types::vm::Arch;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

/// 실행기도 reconciler도 없는 노드를 저장소에만 만든다. `edit`로 상태를 바꾼다.
async fn add_node(env: &Env, name: &str, edit: impl FnOnce(&mut Node)) -> Uuid {
    let mut node: Node = Resource::new(
        name,
        NodeSpec {
            address: "127.0.0.2:0".into(),
            maintenance: false,
            witness: false,
        },
    );
    node.status = NodeStatus {
        online: true,
        cpus: 4,
        memory_mib: 8192,
        hypervisor: "mock".into(),
        bridges: vec![],
        arch: Some(Arch::Aarch64),
        pools: Default::default(),
        ha: None,
        underlay: None,
        firewall: false,
        version: Some(BUILD.into()),
        updates: None,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
    };
    edit(&mut node);
    env.cluster.create(&node).await.unwrap();
    node.id
}

async fn drain(client: &Client, node: &str) -> (StatusCode, Value) {
    client
        .post(&format!("/nodes/{node}/drain"), Value::Null)
        .await
}

/// 409와 그 code를 확인한다.
fn assert_conflict((status, body): (StatusCode, Value), code: &str) {
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], code, "{body}");
}

fn maintenance(env: &Env, id: Uuid) -> bool {
    env.cluster
        .get::<NodeSpec>(id)
        .unwrap()
        .unwrap()
        .spec
        .maintenance
}

/// 풀 `pool`의 Ready 볼륨 `volume`을 디스크로 쓰는 VM `name`을 이 노드에서 실행한다.
async fn running_vm(env: &Env, admin: &Client, name: &str, pool: &str, shared: bool) {
    let (status, body) = admin
        .post(
            "/storage-pools",
            json!({
                "name": pool,
                "spec": {
                    "backend": { "kind": "dir", "path": env.dir.path().join(pool) },
                    "shared": shared
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    env.pool_ready(pool).await;
    let mut volume: Volume = Resource::new(
        format!("{name}-root"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Qcow2,
        },
    );
    volume.status.node = Some(env.node);
    volume.status.phase = VolumePhase::Ready;
    env.cluster.create(&volume).await.unwrap();
    let mut req = vm_request(name);
    req["spec"]["disks"] = json!([{ "volume": volume.name, "bus": "virtio-blk" }]);
    req["spec"]["node"] = json!(env.node);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
}

#[tokio::test]
async fn drain_puts_the_node_in_maintenance_and_creates_a_task() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    // 실행기가 없는 노드라 Task가 끝나지 않는다.
    let node2 = add_node(&env, "node2", |_| {}).await;

    for client in [&viewer, &operator] {
        let (status, body) = drain(client, "node2").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
    assert!(!maintenance(&env, node2));
    let (status, body) = drain(&admin, "nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, task) = drain(&admin, "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-drain" }));
    assert_eq!(task["spec"]["target"], node2.to_string());
    assert_eq!(task["spec"]["node"], node2.to_string());
    assert!(maintenance(&env, node2));

    // 끝나지 않은 비우기가 있으면(id로 골라도) 409 node_busy다.
    assert_conflict(drain(&admin, &node2.to_string()).await, "node_busy");
}

#[tokio::test]
async fn drain_needs_an_online_node_that_is_not_a_witness() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let witness = add_node(&env, "witness", |n| n.spec.witness = true).await;
    let offline = add_node(&env, "offline", |n| n.status.online = false).await;

    let (status, body) = drain(&admin, "witness").await;
    assert_eq!(
        body["message"], "node \"witness\" is a witness node and runs no VMs",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    let (status, body) = drain(&admin, "offline").await;
    assert_eq!(
        body["message"], "node \"offline\" is offline; HA recovers the HA VMs of an offline node",
        "{body}"
    );
    assert_conflict((status, body), "node_unavailable");
    assert!(!maintenance(&env, witness));
    assert!(!maintenance(&env, offline));
}

/// 비우기를 모르는 노드(위트니스 포함)가 남았으면 409 `cluster_upgrading`이다. 버전은 보지 않으므로
/// 롤링 업그레이드 중(버전이 섞여도) 모든 노드가 비우기를 알면 된다. 끝나지 않은 비우기는 먼저 본다.
#[tokio::test]
async fn drain_needs_every_node_to_support_it() {
    let env = Env::start().await;
    let admin = env.admin().await;
    // 실행기가 없는 노드라 비우기 Task가 끝나지 않는다.
    add_node(&env, "node2", |_| {}).await;
    let (status, task) = drain(&admin, "node2").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let old = add_node(&env, "old", |n| {
        n.spec.witness = true;
        n.status.version = Some(BUILD.into());
        n.status.features.clear();
    })
    .await;

    let (status, body) = drain(&admin, "node1").await;
    assert_eq!(
        body["message"], "some nodes do not support node drain yet (old); upgrade them first",
        "{body}"
    );
    assert_conflict((status, body), "cluster_upgrading");
    assert!(!maintenance(&env, env.node));
    assert_conflict(drain(&admin, "node2").await, "node_busy");

    // 옛 버전이어도 비우기를 알면 된다.
    let mut node = env.cluster.get::<NodeSpec>(old).unwrap().unwrap();
    node.status.version = Some("0.0.1".into());
    node.status.features = vec!["node-drain".into()];
    env.cluster
        .update_status::<NodeSpec>(old, &node.status)
        .await
        .unwrap();
    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
}

/// 이미 유지보수인 노드도 비운다. 옮길 VM이 없으면 바로 성공한다.
#[tokio::test]
async fn drain_of_an_empty_node_succeeds() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let (_, node) = admin.get("/nodes/node1").await;
    let mut spec = node["spec"].clone();
    spec["maintenance"] = json!(true);
    let revision = node["revision"].as_u64();
    let (status, body) = admin
        .put("/nodes/node1", json!({ "spec": spec }), revision)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert!(maintenance(&env, env.node));
}

/// 옮기지 못한 VM은 이름 순으로 이유와 함께 실패 메시지에 남고, 노드는 유지보수로 남는다.
#[tokio::test]
async fn vms_that_cannot_move_are_reported() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    running_vm(&env, &admin, "db", "local", false).await;
    // 다른 노드에는 VM(256 MiB)이 들어갈 메모리가 없다.
    add_node(&env, "node2", |n| n.status.memory_mib = 128).await;

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "2 VM(s) were not moved: db (disks on a local pool), web (no node with capacity)"
    );
    assert!(maintenance(&env, env.node));

    // 이 노드가 HVF(macOS)면 옮기지 못한다.
    let mut node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    node.status.hypervisor = "qemu-hvf".into();
    env.cluster
        .update_status::<NodeSpec>(env.node, &node.status)
        .await
        .unwrap();
    let (_, task) = drain(&admin, "node1").await;
    let task = admin.wait_task(&task).await;
    assert_eq!(
        task["status"]["error"],
        "2 VM(s) were not moved: db (disks on a local pool), web (hvf node)"
    );
}

/// 대상은 VM을 받을 수 있는 노드 가운데 남은 메모리가 가장 많은 노드다. HVF(macOS) 노드는 메모리가
/// 더 많아도 고르지 않는다.
#[tokio::test]
async fn drain_skips_nodes_that_cannot_take_the_vm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    add_node(&env, "mac", |n| {
        n.status.hypervisor = "qemu-hvf".into();
        n.status.memory_mib = 65536;
    })
    .await;
    let small = add_node(&env, "small", |_| {}).await;

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    // 시험 노드는 다른 노드에 닿지 못해 마이그레이션 Task가 실패한다.
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: web (this node cannot reach other nodes)"
    );
    let (_, tasks) = env.cluster.list::<TaskSpec>().unwrap();
    let migrations: Vec<&TaskOp> = tasks
        .iter()
        .map(|t| &t.spec.op)
        .filter(|op| matches!(op, TaskOp::VmMigrate { .. }))
        .collect();
    assert_eq!(migrations, [&TaskOp::VmMigrate { node: small }]);
}

/// 끝나지 않은 Task가 있는 VM은 `migration_timeout`까지 기다린 뒤 `busy`로 남긴다.
#[tokio::test]
async fn a_vm_that_stays_busy_is_reported() {
    let mut env = Env::start().await;
    env.stop_runner().await;
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.migration_timeout = Duration::from_millis(500)
    })
    .await;
    let admin = env.admin().await;
    running_vm(&env, &admin, "web", "nas", true).await;
    add_node(&env, "node2", |_| {}).await;
    let vm = env
        .cluster
        .get_by_name::<opl_types::vm::VmSpec>("web")
        .unwrap()
        .unwrap();
    // 실행기가 없는 노드의 Task라 끝나지 않는다.
    let mut stuck: Task = Resource::new(
        "stuck",
        TaskSpec {
            op: TaskOp::VmStop,
            target: vm.id,
            node: Uuid::now_v7(),
        },
    );
    stuck.status.state = TaskState::Running;
    env.cluster.create(&stuck).await.unwrap();

    let (status, task) = drain(&admin, "node1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let task = admin.wait_task(&task).await;
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: web (busy)"
    );
}
```

`crates/opl-api/tests/ha.rs`에서

```rust
        updates: None,
    };
```

을 다음으로 바꾼다:

```rust
        updates: None,
        features: vec![],
    };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        updates: None,
    };
```

을 다음으로 바꾼다:

```rust
        updates: None,
        features: vec![],
    };
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/nodes/{key}/upgrade",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/nodes/{key}/upgrade",
        "/nodes/{key}/drain",
        "/vms",
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기.
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
```

을 다음으로 바꾼다:

```rust
//! 라이브 마이그레이션(Mock 하이퍼바이저): 성공, 확정 전 실패의 되돌리기(HA VM 포함), 공유 디스크의 잠금 바꾸기.
//! 노드 비우기: 공유 풀 VM은 다른 노드로 옮기고(들어오던 VM도) 로컬 디스크 VM은 남기며, 유지보수가 끝나면 멈춘다.
//! 감사 로그: 한 노드의 `GET /audit`가 두 노드의 항목을 합치고, 닿지 않는 노드를 알린다.
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

을 다음으로 바꾼다:

```rust
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{FEATURES, Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv.clone(),
            reconciler.handle(),
```

을 다음으로 바꾼다:

```rust
        let reconciler = Reconciler::spawn(self.cluster.clone(), hv.clone(), reconciler_config);
        let mut state = AppState::new(
            self.cluster.clone(),
            self.id,
            hv.clone(),
            work.join("uploads"),
        );
        state.peers = Some(self.peers.clone());
        state.audit = audit;
        let state = Arc::new(state);
        let mut config = runner_config(self.id, &work);
        config.peers = Some(self.peers.clone());
        config.firmware = Some(firmware);
        // opld처럼 노드 비우기 Task가 API 상태를 쓴다.
        config.api = Some(state.clone());
        let runner = TaskRunner::spawn(
            self.cluster.clone(),
            storage,
            hv,
            reconciler.handle(),
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
        .await;
        let mut state = AppState::new(self.cluster.clone(), self.id, hv, work.join("uploads"));
        state.peers = Some(self.peers.clone());
        state.audit = audit;
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(Arc::new(state));
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
```

을 다음으로 바꾼다:

```rust
        .await;
        let api = TcpListener::bind("127.0.0.1:0").await.unwrap();
        self.base = format!("http://{}", api.local_addr().unwrap());
        let app = opl_api::router(state);
        tokio::spawn(async move { axum::serve(api, app).await.unwrap() });
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            updates: None,
        };
```

을 다음으로 바꾼다:

```rust
            updates: None,
            // 노드가 등록할 때처럼 이 빌드의 기능을 적는다(노드 비우기).
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
        };
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust

/// `found`가 참이 될 때까지 `GET /audit?{query}`를 다시 부른다(쓰기 스레드와 복제를 기다린다).
```

을 다음으로 바꾼다:

```rust

/// 디렉터리 풀 `pool`을 만든다. `shared`면 두 노드가 함께 쓰는 풀이다.
async fn dir_pool(a: &Node, pool: &str, shared: bool) {
    let pool: StoragePool = Resource::new(
        pool,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: format!("/nonexistent/opl-{pool}").into(),
            },
            nodes: vec![],
            shared,
            backups: false,
        },
    );
    a.cluster.create(&pool).await.unwrap();
}

/// 노드 `on`에서 도는 VM `name`. 디스크는 풀 `pool`의 볼륨 `{name}-root`다.
async fn running_vm_on_pool(
    a: &Node,
    admin: &common::Client,
    name: &str,
    pool: &str,
    on: Uuid,
) -> opl_types::vm::Vm {
    let mut volume: Volume = Resource::new(
        format!("{name}-root"),
        VolumeSpec {
            pool: pool.into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
        },
    );
    volume.status.node = Some(on);
    volume.status.phase = VolumePhase::Ready;
    a.cluster.create(&volume).await.unwrap();
    let mut req = vm_request(name);
    req["spec"]["node"] = serde_json::json!(on);
    req["spec"]["disks"] = serde_json::json!([{ "volume": volume.name, "bus": "virtio-blk" }]);
    let (status, body) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let task = admin.act(name, "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    a.cluster.get_by_name::<VmSpec>(name).unwrap().unwrap()
}

/// 노드 `node`를 비우는 Task를 만든다.
async fn drain(admin: &common::Client, node: &str) -> serde_json::Value {
    let (status, task) = admin
        .post(&format!("/nodes/{node}/drain"), serde_json::Value::Null)
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    task
}

fn node_of(a: &Node, id: Uuid) -> Option<Uuid> {
    a.cluster.get::<VmSpec>(id).unwrap().unwrap().spec.node
}

#[tokio::test]
async fn drain_moves_running_vms_to_the_other_node() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    // B의 API로 요청해도 비우기 Task는 비울 노드 A에서 돈다.
    let admin_b = login_at(&b.base, "admin", ADMIN_PASSWORD).await;
    let task = drain(&admin_b, "node1").await;
    assert_eq!(task["spec"]["node"], a.id.to_string());
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(b.id));
    // 이름 순으로 하나씩 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app1.id, app2.id]);
    for vm in [&app1, &app2] {
        wait_vm(&b, vm.id, "running on B", |v| {
            v.status.phase == VmPhase::Running && v.status.node == Some(b.id)
        })
        .await;
    }
    let node = a.cluster.get::<NodeSpec>(a.id).unwrap().unwrap();
    assert!(node.spec.maintenance);
}

#[tokio::test]
async fn drain_leaves_vms_on_local_pools() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    dir_pool(&a, "local", false).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", a.id).await;
    let db = running_vm_on_pool(&a, &admin, "db", "local", a.id).await;

    let task = admin.wait_task(&drain(&admin, "node1").await).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(
        task["status"]["error"],
        "1 VM(s) were not moved: db (disks on a local pool)"
    );
    assert_eq!(node_of(&a, app.id), Some(b.id));
    // 옮기지 못한 VM은 A에서 계속 돈다.
    let db = a.cluster.get::<VmSpec>(db.id).unwrap().unwrap();
    assert_eq!(db.spec.node, Some(a.id));
    assert_eq!(db.status.phase, VmPhase::Running);
    assert_eq!(
        runtime_status(&a, db.id).await,
        Some(RuntimeStatus::Running)
    );
}

/// 비우기를 시작할 때 이 노드로 들어오던 VM도 들어온 뒤 옮긴다. 성공하면 노드가 비어 있다.
#[tokio::test]
async fn drain_also_moves_a_vm_that_was_coming_in() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app = running_vm_on_pool(&a, &admin, "app", "nas", b.id).await;

    b.hv.delay_migrations(Duration::from_millis(500));
    let (status, migrate) = admin
        .post("/vms/app/migrate", serde_json::json!({ "node": "node1" }))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{migrate}");
    let task = drain(&admin, "node1").await;
    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let migrate = admin.wait_task(&migrate).await;
    assert_eq!(migrate["status"]["state"], "succeeded", "{migrate}");
    // B → A로 들어온 뒤 비우기가 다시 B로 옮겼다.
    let moved: Vec<Uuid> = a.hv.migrations().into_iter().map(|(id, _)| id).collect();
    assert_eq!(moved, [app.id]);
    assert_eq!(node_of(&a, app.id), Some(b.id));
}

/// 옮기는 동안 유지보수를 끝내면 그 마이그레이션이 끝난 뒤 멈춘다.
#[tokio::test]
async fn ending_maintenance_stops_the_drain() {
    let (_root, a, b) = two_nodes().await;
    let admin = login_at(&a.base, "admin", ADMIN_PASSWORD).await;
    dir_pool(&a, "nas", true).await;
    let app1 = running_vm_on_pool(&a, &admin, "app1", "nas", a.id).await;
    let app2 = running_vm_on_pool(&a, &admin, "app2", "nas", a.id).await;

    a.hv.delay_migrations(Duration::from_millis(500));
    let task = drain(&admin, "node1").await;
    wait_vm(&a, app1.id, "migrating", |v| {
        v.status.phase == VmPhase::Migrating
    })
    .await;
    let (status, node) = admin.get("/nodes/node1").await;
    assert_eq!(status, StatusCode::OK, "{node}");
    let mut spec = node["spec"].clone();
    spec["maintenance"] = serde_json::json!(false);
    let (status, body) = admin
        .put(
            "/nodes/node1",
            serde_json::json!({ "spec": spec }),
            node["revision"].as_u64(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let task = admin.wait_task(&task).await;
    assert_eq!(task["status"]["state"], "failed", "{task}");
    assert_eq!(task["status"]["error"], "drain stopped: maintenance ended");
    assert_eq!(node_of(&a, app1.id), Some(b.id));
    assert_eq!(node_of(&a, app2.id), Some(a.id));
}

/// `found`가 참이 될 때까지 `GET /audit?{query}`를 다시 부른다(쓰기 스레드와 복제를 기다린다).
```

`crates/opl-types/src/node.rs`에서

```rust
    pub updates: Option<NodeUpdates>,
}
```

을 다음으로 바꾼다:

```rust
    pub updates: Option<NodeUpdates>,
    /// 이 노드의 opld가 아는 기능(예: `node-drain`). 노드가 등록할 때 쓴다(옛 노드는 비어 있다). 새 기능을
    /// 쓰는 API는 모든 노드가 그 기능을 적었는지 본다(버전이 섞여도 된다).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub features: Vec<String>,
}

/// 이 빌드가 아는 기능. 노드가 [`NodeStatus::features`]에 적는다.
pub const FEATURES: &[&str] = &["node-drain"];
```

`crates/opl-types/src/node.rs`에서

```rust
    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn status_features_round_trip_and_are_omitted_when_empty() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("features").is_none(), "{none}");
        let status = NodeStatus {
            features: FEATURES.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["features"], serde_json::json!(["node-drain"]));
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

`crates/opld/src/bootstrap.rs` 전체:

```rust
//! 노드 id, 새 클러스터의 첫 기록(클러스터 정보, 관리자), Node 리소스.

use std::io::Write;
use std::net::IpAddr;
use std::path::Path;

use anyhow::Context;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_peer::pki::ClusterCa;
use opl_types::auth::{Role, UserSpec};
use opl_types::cluster::ClusterSpec;
use opl_types::node::{FEATURES, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
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
            realm: None,
            subject: None,
        },
    );
    cluster.create(&admin).await?;
    Ok(password)
}

/// 다시 시작한 노드의 역할(합류할 때 정해진 Node `spec.witness`)이 설정의 `witness`와 같은지 본다.
/// 로컬 저장소만 읽는다. Node 리소스가 아직 없으면 그대로 둔다.
pub fn check_role(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let Some(node) = cluster.get::<NodeSpec>(id)? else {
        return Ok(());
    };
    match (node.spec.witness, config.witness) {
        (true, false) => anyhow::bail!(
            "this node joined as a witness but opld.toml says witness = false; leave the cluster \
             and join again to change the role"
        ),
        (false, true) => anyhow::bail!(
            "this node joined as a non-witness node but opld.toml says witness = true; leave the \
             cluster and join again to change the role"
        ),
        _ => Ok(()),
    }
}

/// 이 노드의 Node 리소스가 없으면 만들고(역할은 설정의 `witness`), 있으면 주소만 맞춘다.
async fn ensure_node(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    let NodeId(id) = cluster.id();
    let address = config.api_address();
    match cluster.get::<NodeSpec>(id)? {
        None => {
            let mut node = Resource::new(
                config.name.clone(),
                NodeSpec {
                    address,
                    maintenance: false,
                    witness: config.witness,
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
    Ok(())
}

/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부, 버전, 업데이트 확인 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
    let underlay = config.underlay().await;
    record_node(cluster, config, host, underlay).await
}

/// `register_node`의 기록. `underlay`는 정한 underlay다(None이면 풀지 못한 것).
async fn record_node(
    cluster: &Cluster,
    config: &Config,
    host: &Host,
    underlay: Option<IpAddr>,
) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
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
            // 펜싱이 꺼져 있으면 None이라 리더가 이 노드의 HA VM을 옮기지 않는다.
            ha: config.ha.node_ha(),
            // 이름을 풀지 못하면(설정·IP 리터럴이 아님) 기록된 값을 지킨다(잠깐 DNS가 안 된다고
            // VXLAN 네트워크를 잃지 않게).
            underlay: underlay.or(status.underlay),
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
            // 업데이트 확인이 쓴 결과는 지우지 않는다. 꺼진 노드는 None이다(다시 확인을 받지 않는다).
            updates: config
                .updates
                .enabled()
                .then(|| status.updates.take().unwrap_or_default()),
            features: features(),
        };
        true
    })
    .await
}

/// 이 빌드가 아는 기능([`FEATURES`]). 위트니스도 적는다(새 기능의 API는 모든 노드를 본다).
fn features() -> Vec<String> {
    FEATURES.iter().map(|f| f.to_string()).collect()
}

/// 위트니스의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 아키텍처만 기록한다. VM을 돌리지
/// 않으므로 용량·하이퍼바이저·브리지·풀은 비운다.
pub async fn register_witness(cluster: &Cluster, config: &Config) -> anyhow::Result<()> {
    ensure_node(cluster, config).await?;
    update_node_status(cluster, |status| {
        *status = NodeStatus {
            online: true,
            arch: Some(Arch::native()),
            version: Some(BUILD.to_string()),
            features: features(),
            ..NodeStatus::default()
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

/// 기능 표시가 이 빌드의 목록과 다르면 고쳐 쓴다. 롤링 업그레이드 중 옛 버전 리더가 이 노드의 status를
/// 다시 쓰면(모르는 `features`를 빼고) 지워지므로 주기적으로 되살린다. 같으면 쓰지 않는다.
pub async fn refresh_features(cluster: &Cluster) -> anyhow::Result<()> {
    let features = features();
    update_node_status(cluster, |status| {
        if status.features == features {
            return false;
        }
        tracing::info!(?features, "restoring the recorded features");
        status.features = features.clone();
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

    use super::*;
    use async_trait::async_trait;
    use openraft::{BasicNode, ServerState};
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeUpdates, PoolState};

    use crate::config::FencingSetting;

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
        async fn sync_overlays(
            &self,
            _: &str,
            _: &[Overlay],
        ) -> Result<OverlayChanges, DriverError> {
            unimplemented!()
        }
    }

    #[derive(Debug)]
    struct FakeFirewall;

    #[async_trait]
    impl Firewall for FakeFirewall {
        async fn apply(&self, _: &str) -> Result<(), DriverError> {
            Ok(())
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
        router.register(id, &cluster);
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

    /// address는 IP 리터럴이다(등록이 이름을 풀지 않게).
    fn test_config() -> Config {
        toml::from_str(
            "name = \"n\"\naddress = \"10.0.0.1\"\n[hypervisor]\naccel = \"tcg\"\nsystemd_scope = false",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn registration_records_the_host_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        // 컴파일된 아키텍처가 아니라 HostProfile의 것을 기록한다.
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        for arch in [Arch::X86_64, Arch::Aarch64] {
            host.profile.arch = arch;
            register_node(&cluster, &config, &host).await.unwrap();
            let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
            assert_eq!(node.status.arch, Some(arch));
        }
        cluster.shutdown().await.unwrap();
    }

    /// `[ha]`의 펜싱을 기록하고, 설정을 바꿔 다시 등록하면 그 값으로 바꾼다. `off`면 없다.
    #[tokio::test]
    async fn registration_records_the_ha_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        config.ha.fencing = FencingSetting::Process;
        config.ha.fence_after_secs = 2;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(
            node.status.ha,
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        config.ha.fencing = FencingSetting::Off;
        register_node(&cluster, &config, &host).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.status.ha, None);
        cluster.shutdown().await.unwrap();
    }

    /// `[network] underlay`를, 없으면 `address`의 IP를 기록한다. 이름을 풀지 못하면(None) 기록된 값을
    /// 그대로 둔다(잠깐 DNS가 안 된다고 VM이 쓰는 VXLAN 장치를 지우지 않게).
    #[tokio::test]
    async fn registration_records_the_underlay() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let underlay = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .underlay
        };
        // 이름을 풀지 못한 새 노드에는 없다.
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), None);
        config.network.underlay = Some("10.1.0.5".parse().unwrap());
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("10.1.0.5".parse().unwrap()));
        config.network.underlay = None;
        config.address = "fd00::5".into();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        record_node(&cluster, &config, &host, None).await.unwrap();
        assert_eq!(underlay(), Some("fd00::5".parse().unwrap()));
        cluster.shutdown().await.unwrap();
    }

    /// 방화벽(nftables)이 있는지 기록하고, 다시 등록하면 그때의 값으로 바꾼다.
    #[tokio::test]
    async fn registration_records_the_firewall() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let firewall = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .firewall
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        host.firewall = Some(Arc::new(FakeFirewall));
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(firewall());
        host.firewall = None;
        register_node(&cluster, &config, &host).await.unwrap();
        assert!(!firewall());
        cluster.shutdown().await.unwrap();
    }

    /// 등록할 때 이 빌드의 버전과 기능을 쓴다(옛 버전으로 적힌 값도 바꾼다).
    #[tokio::test]
    async fn registration_records_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let version = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .version
        };
        let features = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .features
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(features(), ["node-drain"]);
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            status.features.clear();
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        assert_eq!(features(), ["node-drain"]);
        cluster.shutdown().await.unwrap();
    }

    /// 옛 버전 리더가 status를 다시 쓰며 지운 기능 표시를 주기 갱신이 되살린다. 같으면 쓰지 않는다.
    #[tokio::test]
    async fn refresh_restores_features_erased_by_an_old_leader() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let node = || cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        let revision = node().revision;
        refresh_features(&cluster).await.unwrap();
        assert_eq!(node().revision, revision);
        // 옛 리더는 모르는 `features`를 빼고 status를 다시 쓴다.
        update_node_status(&cluster, |status| {
            status.features.clear();
            true
        })
        .await
        .unwrap();
        assert!(node().status.features.is_empty());
        refresh_features(&cluster).await.unwrap();
        assert_eq!(node().status.features, ["node-drain"]);
        cluster.shutdown().await.unwrap();
    }

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
    /// 꺼지면 지운다.
    #[tokio::test]
    async fn registration_records_whether_updates_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let updates = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .updates
        };
        config.updates.enabled = Some(true);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(NodeUpdates::default()));
        let checked = NodeUpdates {
            checked_at: Some(1_793_000_000),
            error: Some("apt-get update -q failed".into()),
            ..NodeUpdates::default()
        };
        update_node_status(&cluster, |status| {
            status.updates = Some(checked.clone());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(checked));
        config.updates.enabled = Some(false);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), None);
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
    #[tokio::test]
    async fn witness_registration_records_only_the_role_and_architecture() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        // underlay와 업데이트를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"\n\
             [updates]\nenabled = true",
        )
        .unwrap();
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert_eq!(node.name, "w");
        assert!(node.spec.witness);
        assert_eq!(node.spec.address, "10.0.0.9:8006");
        assert_eq!(
            node.status,
            NodeStatus {
                online: true,
                arch: Some(Arch::native()),
                version: Some(BUILD.to_string()),
                features: vec!["node-drain".into()],
                ..NodeStatus::default()
            }
        );

        mark_offline(&cluster).await;
        register_witness(&cluster, &config).await.unwrap();
        let node = cluster.get::<NodeSpec>(id.0).unwrap().unwrap();
        assert!(node.spec.witness);
        assert!(node.status.online);
        cluster.shutdown().await.unwrap();
    }

    /// 합류할 때 정한 역할과 설정의 `witness`가 다르면 시작하지 않는다. Node 리소스가 없으면 본다.
    #[tokio::test]
    async fn a_role_change_in_the_config_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let witness: Config =
            toml::from_str("name = \"w\"\naddress = \"h\"\nwitness = true").unwrap();
        let normal = test_config();
        check_role(&cluster, &witness).unwrap();
        check_role(&cluster, &normal).unwrap();

        register_witness(&cluster, &witness).await.unwrap();
        check_role(&cluster, &witness).unwrap();
        let err = check_role(&cluster, &normal).unwrap_err().to_string();
        assert!(
            err.contains("joined as a witness but opld.toml says witness = false"),
            "{err}"
        );

        let NodeId(id) = cluster.id();
        let node = cluster.get::<NodeSpec>(id).unwrap().unwrap();
        let spec = NodeSpec {
            witness: false,
            ..node.spec
        };
        cluster
            .update_spec(id, None, node.labels, &spec)
            .await
            .unwrap();
        check_role(&cluster, &normal).unwrap();
        let err = check_role(&cluster, &witness).unwrap_err().to_string();
        assert!(
            err.contains("joined as a non-witness node but opld.toml says witness = true"),
            "{err}"
        );
        cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_bridge_listing_keeps_the_recorded_bridges() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let host = |bridges: Option<Vec<BridgeInfo>>| Host {
            net: Some(Arc::new(FakeNet(bridges))),
            ..Host::from_config(&config, "t").unwrap()
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
        let mut host = Host::from_config(&config, "t").unwrap();
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

`crates/opld/src/member.rs`에서

```rust
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록을 다시 확인하는 간격. 관리자가 브리지를 만들면 이만큼 뒤에 스케줄러가 안다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
```

을 다음으로 바꾼다:

```rust
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록과 기능 표시(위트니스는 기능 표시만)를 다시 확인하는 간격. 관리자가 브리지를 만들면
/// 이만큼 뒤에 스케줄러가 알고, 옛 버전 리더가 지운 기능 표시는 이만큼 뒤에 되살아난다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
```

`crates/opld/src/member.rs`에서

```rust
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                }
            })
```

을 다음으로 바꾼다:

```rust
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.import_credentials = state.import_credentials.clone();
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
```

을 다음으로 바꾼다:

```rust
        runner_config.import_credentials = state.import_credentials.clone();
        // 노드 비우기 Task가 이 노드의 API와 같은 확인으로 마이그레이션 Task를 만든다.
        let state = Arc::new(state);
        runner_config.api = Some(state.clone());
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
```

`crates/opld/src/member.rs`에서

```rust
        let (stop, rx) = oneshot::channel::<()>();
        let state = Arc::new(state);
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
```

을 다음으로 바꾼다:

```rust
        let (stop, rx) = oneshot::channel::<()>();
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
```

`crates/opld/src/member.rs`에서

```rust
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
            })
```

을 다음으로 바꾼다:

```rust
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1535개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): 노드 비우기 API와 Task — POST /nodes/{key}/drain(admin, 202 { task }; 404 → 위트니스·오프라인 409 node_unavailable → 끝나지 않은 비우기 409 node_busy → require_feature(위트니스 포함 모든 노드가 status.features에 node-drain을 적었어야 하고, 아니면 409 cluster_upgrading \"some nodes do not support node drain yet (…); upgrade them first\" — 버전을 보지 않으므로 롤링 업그레이드 중 버전이 섞여도 됨) → maintenance=true(이미면 그대로, revision 확인 쓰기 다시 하기) → 그 노드에 node-drain Task). 기능 표시: NodeStatus.features(serde default, 비면 생략)와 opl_types::node::FEATURES = [\"node-drain\"], opld 등록(record_node·register_witness)이 적고, 노드·위트니스의 주기 갱신(BRIDGE_REFRESH 60초, bootstrap::refresh_features — revision 확인 쓰기, 같으면 쓰지 않음)이 옛 버전 리더가 status를 다시 쓰며 지운 표시를 되살림. node-drain Task(설계 3.2): 매 반복과 VM이 자리 잡은 뒤·마이그레이션 시작 직전·들어오는 마이그레이션을 기다리는 동안 노드 spec을 다시 읽어 유지보수가 끝났으면 \"drain stopped: maintenance ended\"로 멈춤. 이 노드의 실행 중인 VM을 이름 순으로 하나씩 — VM이 자리 잡을 때까지 최대 migration_timeout 기다림(넘으면 busy), 로컬 풀 디스크·이 노드가 HVF면 건너뜀, 대상은 pick_target(node_fits를 지나고 HVF가 아니며 남은 메모리 ≥ VM 메모리인 노드 가운데 남은 메모리가 가장 많은 노드, 같으면 id가 작은 노드; 없으면 no node with capacity), vms::start_migration에 source 인자를 더해(vm_lock 아래에서 원본 노드가 다르면 내부용 409 vm_moved; migrate_vm은 None) VmMigrate Task를 만들고(vm_busy면 한 번 더 자리 잡기를 기다려 다시) 끝나기를 migration_timeout*2+60s까지 기다림(넘으면 migration task did not finish). 꺼지거나 지워지거나 다른 노드로 간 VM은 넘어가고 다시 나타나면 3번까지 다시 봄(그 뒤 busy), 비우기가 옮긴 VM이 다시 들어오면 moved back during the drain. 성공은 노드가 빈 것: 옮길 VM이 없어도 이 노드로 들어오는 마이그레이션(status.migration의 대상·끝나지 않은 VmMigrate { node })을 migration_timeout까지 기다려(넘으면 busy) 들어온 VM도 옮김. 옮기지 못한 VM은 \"N VM(s) were not moved: 이름 (이유), …\"로 실패, 비우기 Task는 일반 600초 제한 없이 노드의 줄에 섬. TaskRunnerConfig.api(이 노드의 AppState — 비우기 Task가 API와 같은 확인과 vm_lock으로 마이그레이션 Task를 만든다, opld·시험 도우미가 넘김), OpenAPI·schema.d.ts. 시험: API(권한, 404, 위트니스·오프라인 409, node_busy, 유지보수로 바뀜, 기능 없는 위트니스면 409 cluster_upgrading이고 node_busy가 먼저·옛 버전이어도 기능이 있으면 202, 빈 노드 성공, 옮기지 못한 이유 — 로컬 풀·메모리가 모자란 노드만 있으면 no node with capacity·HVF, HVF 노드를 건너뛰고 다른 노드로 VmMigrate, settle busy 시간 초과), opld(등록이 기능을 적음, 지운 기능 표시를 주기 갱신이 되살림), 두 노드(실제 gRPC 마이그레이션): 공유 풀 VM 둘이 이름 순으로 옮겨지고 성공, 로컬 디스크 VM은 남고 실패 메시지, 비우기 시작 때 들어오던 VM도 옮김, 옮기는 중 유지보수를 끄면 멈춤"
```

---

### Task 3: oplctl 노드 유지보수·비우기 명령과 opld E2E

`oplctl node maintenance <노드> on|off`(If-Match, 다시 하기, 끌 때 진행 중 비우기 경고), `oplctl node drain <노드> [--wait]`. opld E2E(기능 `migration-it`, 실제 QEMU): 실행 중 VM을 `node drain --wait`로 다른 노드로.

**Files:**
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`
- Modify: `crates/opld/tests/migration.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca, fingerprint,
    local, seg,
};
```

을 다음으로 바꾼다:

```rust
use crate::client::{
    ApiError, BACKUP_WAIT, Client, DRAIN_WAIT, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca,
    fingerprint, local, seg,
};
```

`crates/oplctl/src/cli.rs`에서

```rust
        #[arg(long)]
        wait: bool,
    },
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
```

을 다음으로 바꾼다:

```rust
        #[arg(long)]
        wait: bool,
    },
    /// 노드를 유지보수로 바꾸거나(`on` — 새 VM을 놓지 않는다) 끝낸다(`off`). 도는 VM은 그대로다.
    Maintenance {
        node: String,
        state: Switch,
    },
    /// 노드를 유지보수로 바꾸고 실행 중인 VM을 하나씩 다른 노드로 라이브 마이그레이션한다. 로컬 풀
    /// 디스크 VM은 옮기지 못한다.
    Drain {
        node: String,
        /// 비우기 Task가 끝날 때까지 기다린다. 옮기지 못한 VM이 있으면 실패한다.
        #[arg(long)]
        wait: bool,
    },
}

/// `node maintenance`의 켜기·끄기
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Switch {
    On,
    Off,
}

/// `node maintenance`: 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔 그 revision으로
/// (`If-Match`) 다시 쓴다. 읽은 뒤 status만 바뀌어도 revision이 달라지므로 `revision_mismatch`면 다시 한다.
/// 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 `err`(stderr)에 알린다(Task 목록을 읽지 못하면 그 경고).
async fn node_maintenance(
    client: &Client,
    node: &str,
    on: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let path = format!("/nodes/{}", seg(node));
    let mut retries = SpecRetries::default();
    let id = loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&spec_update(&current, "maintenance", json!(on)));
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => break cell(&current["id"]),
        }
    };
    let state = if on { "on" } else { "off" };
    writeln!(out, "node {node}: maintenance {state}")?;
    if !on {
        // 비우기 Task는 유지보수가 끝나면 멈춘다. 유지보수는 이미 꺼졌으므로 찾아보지 못해도 경고만 한다.
        let tasks = match client.get("/tasks").await {
            Ok(tasks) => tasks,
            Err(e) => {
                writeln!(err, "warning: cannot check for a running drain: {e}")?;
                return Ok(());
            }
        };
        let draining = tasks["items"].as_array().into_iter().flatten().find(|t| {
            t["spec"]["op"]["kind"] == "node-drain"
                && cell(&t["spec"]["node"]) == id
                && !matches!(t["status"]["state"].as_str(), Some("succeeded" | "failed"))
        });
        if let Some(task) = draining {
            writeln!(
                err,
                "warning: the running drain of {node} stops (task {})",
                cell(&task["id"])
            )?;
        }
    }
    Ok(())
}

/// `node drain`: 비우기 Task를 만든다(노드는 곧바로 유지보수가 된다). `wait`이면 끝날 때까지 기다리고,
/// 아니면 Task를 따라가는 방법을 알린다.
async fn node_drain(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{}/drain", seg(node)), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, DRAIN_WAIT).await?;
        writeln!(out, "node {node} drained")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "drain of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
```

`crates/oplctl/src/cli.rs`에서

```rust
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
        },
```

을 다음으로 바꾼다:

```rust
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
            NodeCmd::Maintenance { node, state } => {
                node_maintenance(
                    &client,
                    &node,
                    state == Switch::On,
                    out,
                    &mut std::io::stderr(),
                )
                .await?
            }
            NodeCmd::Drain { node, wait } => node_drain(&client, &node, wait, out).await?,
        },
```

`crates/oplctl/src/cli.rs`에서

```rust

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
```

을 다음으로 바꾼다:

```rust

    /// `node maintenance`는 on·off만 받는다. 노드를 읽어 라벨과 spec은 그대로 두고 `maintenance`만 바꿔
    /// 읽은 revision으로(`If-Match`) 다시 쓴다.
    #[tokio::test]
    async fn node_maintenance_puts_the_spec() {
        let run = |args: &'static [&'static str], base: String| async move {
            let Cmd::Node {
                cmd: NodeCmd::Maintenance { node, state },
            } = Cli::try_parse_from(args).unwrap().cmd
            else {
                unreachable!()
            };
            let client = Client::new(&base, None, None).unwrap();
            let mut out = Vec::new();
            let mut err = Vec::new();
            node_maintenance(&client, &node, state == Switch::On, &mut out, &mut err)
                .await
                .unwrap();
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            String::from_utf8(out).unwrap()
        };
        for args in [
            &["oplctl", "node", "maintenance", "n1"][..],
            &["oplctl", "node", "maintenance", "n1", "yes"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }

        let (base, seen) = fake_api_bodies(|_| {
            json!({
                "name": "n1",
                "revision": 7,
                "labels": { "rack": "a" },
                "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                "status": { "online": true }
            })
        })
        .await;
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "on"], base.clone()).await,
            "node n1: maintenance on\n"
        );
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert_eq!(seen[0].line, "GET /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].line, "PUT /api/v1/nodes/n1 HTTP/1.1");
            assert_eq!(seen[1].if_match.as_deref(), Some("7"));
            assert_eq!(
                seen[1].body,
                json!({
                    "labels": { "rack": "a" },
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false }
                })
            );
        }
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "maintenance", "n1", "off"], base).await,
            "node n1: maintenance off\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].body["spec"]["maintenance"], false);
        // 끄면 그 노드의 비우기 Task를 찾아본다(없으면 경고하지 않는다).
        assert_eq!(seen[2].line, "GET /api/v1/tasks HTTP/1.1");
    }

    /// 유지보수를 끄면 그 노드의 끝나지 않은 비우기가 멈춘다고 stderr에 알린다.
    #[tokio::test]
    async fn node_maintenance_off_warns_about_a_running_drain() {
        let (base, _) = fake_api_bodies(|path| {
            if path == "/api/v1/tasks" {
                let drain = |id: &str, node: &str, state: &str| {
                    json!({
                        "id": id,
                        "spec": { "op": { "kind": "node-drain" }, "target": node, "node": node },
                        "status": { "state": state }
                    })
                };
                json!({ "items": [
                    drain("t1", "id-1", "failed"),
                    drain("t2", "id-2", "running"),
                    drain("t3", "id-1", "running"),
                ] })
            } else {
                json!({
                    "id": "id-1",
                    "name": "n1",
                    "revision": 7,
                    "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                    "status": { "online": true }
                })
            }
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: the running drain of n1 stops (task t3)\n"
        );
    }

    /// 유지보수는 이미 꺼졌으므로, 비우기 Task를 찾아보지 못해도 경고만 하고 성공한다.
    #[tokio::test]
    async fn node_maintenance_off_succeeds_when_tasks_cannot_be_read() {
        let (client, _) = fake_api_status(|path| {
            if path == "/api/v1/tasks" {
                (
                    500,
                    json!({ "code": "internal", "message": "tasks unavailable" }),
                )
            } else {
                (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": true, "witness": false },
                        "status": { "online": true }
                    }),
                )
            }
        })
        .await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        node_maintenance(&client, "n1", false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node n1: maintenance off\n"
        );
        let err = String::from_utf8(err).unwrap();
        assert!(
            err.starts_with("warning: cannot check for a running drain: ")
                && err.contains("tasks unavailable"),
            "{err}"
        );
    }

    /// 읽은 뒤 노드가 바뀌어 PUT이 `revision_mismatch`면 다시 읽어 쓴다.
    #[tokio::test]
    async fn node_maintenance_retries_a_revision_mismatch() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            // GET, PUT(409), GET, PUT 차례다.
            match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                1 => (
                    409,
                    json!({ "code": "revision_mismatch", "message": "node n1 changed" }),
                ),
                _ => (
                    200,
                    json!({
                        "id": "id-1",
                        "name": "n1",
                        "revision": 7,
                        "spec": { "address": "10.0.0.1:7443", "maintenance": false, "witness": false },
                        "status": { "online": true }
                    }),
                ),
            }
        })
        .await;
        let mut out = Vec::new();
        node_maintenance(&client, "n1", true, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "node n1: maintenance on\n");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1",
                "PUT /api/v1/nodes/n1 HTTP/1.1",
            ]
        );
    }

    /// `node drain`은 비우기 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다리고, Task가
    /// 실패하면(옮기지 못한 VM) 그 오류로 끝난다.
    #[tokio::test]
    async fn node_drain_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/drain" => json!({ "id": "t1" }),
            "/api/v1/nodes/n2/drain" => json!({ "id": "t2" }),
            "/api/v1/tasks/t2" => json!({ "id": "t2", "status": {
                "state": "failed",
                "error": "1 VM(s) were not moved: db (disks on a local pool)"
            } }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Drain { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_drain(&client, &node, wait, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "drain", "n1"]).await.unwrap(),
            "drain of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/drain HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "drain", "n1", "--wait"])
                .await
                .unwrap(),
            "node n1 drained\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/drain HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        let err = run(&["oplctl", "node", "drain", "n2", "--wait"])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "task t2 failed: 1 VM(s) were not moved: db (disks on a local pool)"
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
```

`crates/oplctl/src/client.rs`에서

```rust
pub const UPGRADE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

을 다음으로 바꾼다:

```rust
pub const UPGRADE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// 노드 비우기 Task를 기다리는 시간. VM마다 마이그레이션(서버 `[migration] timeout`)을 하나씩 하므로 Task
/// 실행기는 시간 제한을 두지 않는다.
pub const DRAIN_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

`crates/opld/tests/migration.rs`에서

```rust
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다.
//! `cargo test -p opld --features migration-it --test migration`
```

을 다음으로 바꾼다:

```rust
//! 파일이 따라가는지도 본다. 게스트에는 OS가 없으므로(UEFI 펌웨어만) 디스크 내용은 정지한 VM에서
//! `qemu-io`로 본다. 노드 비우기(`oplctl node drain --wait`)가 실행 중인 VM을 다른 노드로 옮기는지도 본다.
//! `cargo test -p opld --features migration-it --test migration`
```

`crates/opld/tests/migration.rs`에서

```rust

/// NFS 풀(두 노드가 각자 마운트한다)
```

을 다음으로 바꾼다:

```rust

/// `oplctl node drain --wait`: 노드 1을 유지보수로 바꾸고 그 노드에서 도는 VM을 스케줄러가 고른 노드 2로
/// 라이브 마이그레이션한다. 유지보수를 끈 뒤에도 VM은 노드 2에 남는다.
#[tokio::test]
async fn drain_moves_a_running_vm_to_the_other_node() {
    let p = Pair::new("mig-drain", ["60200-60209", "60210-60219"]).await;
    let (c, d) = (&p.c[0], &p.d[0]);
    let target = p.node_id(1).await;

    let shared = tempfile::Builder::new()
        .prefix("opldrain")
        .tempdir_in("/tmp")
        .unwrap();
    d.ctl(&[
        "pool",
        "create",
        "shared",
        "--path",
        shared.path().to_str().unwrap(),
        "--shared",
    ])
    .await
    .unwrap();
    p.wait_pool_ready("shared").await;
    d.ctl(&[
        "volume", "create", "root", "--pool", "shared", "--size", "64M",
    ])
    .await
    .unwrap();
    d.ctl(&[
        "vm", "create", "web", "--memory", "256", "--disk", "root", "--node", "node1", "--start",
    ])
    .await
    .unwrap();
    let pid = running_pid(c).await;
    let id = c.get("/vms/web").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let out = d.ctl(&["node", "drain", "node1", "--wait"]).await.unwrap();
    assert_eq!(out, "node node1 drained\n");
    let node = c.get("/nodes/node1").await.unwrap();
    assert_eq!(node["spec"]["maintenance"], true, "{node}");
    for c in &p.c {
        let vm = c.get("/vms/web").await.unwrap();
        assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["node"].as_str(), Some(target.as_str()), "{vm}");
        assert_eq!(vm["status"]["migration"], Value::Null, "{vm}");
    }
    let moved = running_pid(&p.c[1]).await;
    assert_ne!(moved, pid);
    assert!(alive(moved));
    eventually("source QEMU stopped", || async {
        !alive(pid) && !process_with(&vm_run_dir(&p.d[0], &id))
    })
    .await;

    let out = d
        .ctl(&["node", "maintenance", "node1", "off"])
        .await
        .unwrap();
    assert_eq!(out, "node node1: maintenance off\n");
    let node = c.get("/nodes/node1").await.unwrap();
    assert_eq!(node["spec"]["maintenance"], false, "{node}");
    let vm = c.get("/vms/web").await.unwrap();
    assert_eq!(vm["spec"]["node"].as_str(), Some(target.as_str()), "{vm}");

    d.ctl(&["vm", "stop", "web", "--force"]).await.unwrap();
    d.ctl(&["vm", "delete", "web"]).await.unwrap();
    d.ctl(&["volume", "delete", "root"]).await.unwrap();
    d.ctl(&["pool", "delete", "shared"]).await.unwrap();
}

/// NFS 풀(두 노드가 각자 마운트한다)
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1540개, 실패 0

Run: `cargo test -p opld --features migration-it --test migration`
Expected: PASS 3개 (QEMU가 있어야 한다, macOS는 tcg)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(oplctl): 노드 유지보수·비우기 명령 — oplctl node maintenance <노드> on|off(노드를 읽어 라벨과 spec은 그대로 두고 maintenance만 바꿔 읽은 revision으로 If-Match PUT, revision_mismatch면 다시 읽어 세 번까지 다시 함; off 뒤 GET /tasks로 그 노드의 끝나지 않은 node-drain Task가 있으면 stderr에 \"warning: the running drain of X stops (task T)\", 목록을 읽지 못하면 \"warning: cannot check for a running drain: …\"만 쓰고 성공), oplctl node drain <노드> [--wait](POST /nodes/{key}/drain; 기다리지 않으면 Task id와 `oplctl task get`으로 따라가는 방법, --wait이면 DRAIN_WAIT(24시간)까지 따라가고 실패하면 Task 오류 \"task … failed: N VM(s) were not moved: …\"로 종료 코드 1). 시험: 파싱(on|off만), fake_api로 GET→If-Match PUT 본문, revision_mismatch 한 번 뒤 다시 함(GET·PUT·GET·PUT), off의 비우기 경고와 Task 목록을 읽지 못할 때의 경고·성공, drain의 요청·출력·--wait 성공과 실패. opld E2E(migration-it, 실제 QEMU): 두 노드에서 노드 1의 실행 중 VM을 node drain --wait로 노드 2에 옮기고(유지보수로 바뀜, 원본 QEMU 정리), maintenance off 뒤에도 VM은 노드 2에 남음"
```

---

### Task 4: 웹: 노드 비우기 대화상자

노드 화면 행 메뉴 "노드 비우기…"(위트니스 없음, 오프라인·비우는 중·macOS 노드는 이유와 함께 막힘), 확인 대화상자(옮길 VM 수, 로컬 디스크 안내), Task 따라가기, 비우는 중 유지보수 끝내기 확인, `errors.node_unavailable`에 서버 메시지. ko·en.

**Files:**
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/vms/VmListPage.test.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/i18n/en.json`에서

```json
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The target node is a witness, offline or in maintenance.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
```

을 다음으로 바꾼다:

```json
    "migration_unsupported": "This VM cannot be migrated: {{message}}",
    "node_unavailable": "The node is unavailable: {{message}}",
    "node_busy": "The node is already being drained.",
    "no_capacity": "The target node cannot take this VM: {{message}}",
```

`web/src/i18n/en.json`에서

```json
      "node-update-check": "Check updates on {{target}}",
      "node-upgrade": "Upgrade {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "node-update-check": "Check updates on {{target}}",
      "node-upgrade": "Upgrade {{target}}",
      "node-drain": "Drain {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
    "maintenanceOff": "{{name}} is out of maintenance.",
    "remove": "Remove from cluster",
```

을 다음으로 바꾼다:

```json
    "maintenanceOff": "{{name}} is out of maintenance.",
    "drain": {
      "menu": "Drain node…",
      "title": "Drain {{name}}?",
      "description": "The node enters maintenance, and its running VMs are live-migrated one at a time to other nodes the scheduler picks. The node stays in maintenance when the drain finishes or fails.",
      "confirm": "Drain",
      "running_one": "{{count}} running VM will be moved.",
      "running_other": "{{count}} running VMs will be moved.",
      "localNote": "VMs whose disks are on a local pool cannot be moved. They stay on this node, and the drain fails listing them. Stop them or move their disks first.",
      "reason": {
        "offline": "The node is offline.",
        "hvf": "VMs on a macOS node cannot be moved.",
        "draining": "The node is being drained."
      },
      "endTitle": "End maintenance of {{name}}?",
      "endDescription": "Ending maintenance stops the running drain.",
      "errors": {
        "node_unavailable": "This node cannot be drained: {{message}}"
      }
    },
    "remove": "Remove from cluster",
```

`web/src/i18n/ko.json`에서

```json
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "대상 노드가 위트니스이거나 오프라인이거나 유지보수 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
```

을 다음으로 바꾼다:

```json
    "migration_unsupported": "이 VM은 마이그레이션할 수 없습니다: {{message}}",
    "node_unavailable": "노드를 쓸 수 없습니다: {{message}}",
    "node_busy": "이 노드는 이미 비우는 중입니다.",
    "no_capacity": "대상 노드가 이 VM을 받을 수 없습니다: {{message}}",
```

`web/src/i18n/ko.json`에서

```json
      "node-update-check": "{{target}} 업데이트 확인",
      "node-upgrade": "{{target}} 업그레이드"
    }
```

을 다음으로 바꾼다:

```json
      "node-update-check": "{{target}} 업데이트 확인",
      "node-upgrade": "{{target}} 업그레이드",
      "node-drain": "{{target}} 비우기"
    }
```

`web/src/i18n/ko.json`에서

```json
    "maintenanceOff": "{{name}}의 유지보수를 끝냈습니다.",
    "remove": "클러스터에서 제거",
```

을 다음으로 바꾼다:

```json
    "maintenanceOff": "{{name}}의 유지보수를 끝냈습니다.",
    "drain": {
      "menu": "노드 비우기…",
      "title": "{{name}}을(를) 비울까요?",
      "description": "노드를 유지보수 모드로 바꾸고, 실행 중인 VM을 스케줄러가 고른 다른 노드로 하나씩 라이브 마이그레이션합니다. 비우기가 끝나거나 실패해도 노드는 유지보수 모드로 남습니다.",
      "confirm": "비우기",
      "running_one": "실행 중인 VM {{count}}개를 옮깁니다.",
      "running_other": "실행 중인 VM {{count}}개를 옮깁니다.",
      "localNote": "디스크가 로컬 풀에 있는 VM은 옮길 수 없습니다. 이 노드에 그대로 남고, 비우기는 그 VM들을 적어 실패합니다. 먼저 끄거나 디스크를 옮기세요.",
      "reason": {
        "offline": "노드가 오프라인입니다.",
        "hvf": "macOS 노드의 VM은 옮길 수 없습니다.",
        "draining": "이 노드를 비우는 중입니다."
      },
      "endTitle": "{{name}}의 유지보수를 끝낼까요?",
      "endDescription": "끝내면 진행 중인 비우기가 멈춥니다.",
      "errors": {
        "node_unavailable": "이 노드는 비울 수 없습니다: {{message}}"
      }
    },
    "remove": "클러스터에서 제거",
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  expect(within(dialog).queryByRole("button", { name: "Check again" })).not.toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(dialog).queryByRole("button", { name: "Check again" })).not.toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});

it("drains a node after saying how many running VMs move", async () => {
  cluster();
  const other = vm("api", "running", {
    spec: { ...vm("api", "running").spec, node: "n2" },
  });
  mockLists({
    node: [NODE, { ...DOWN, status: { ...DOWN.status, online: true } }],
    vm: [vm("web", "running"), vm("db", "running"), vm("old", "stopped"), other],
  });
  let posted = 0;
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () => {
      posted += 1;
      return HttpResponse.json(task("t1", "node-drain", "n1"), { status: 202 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Drain node-a?" });
  // node-a에서 켜 둔 VM 둘만 센다(꺼진 VM과 다른 노드의 VM은 빼고).
  await vi.waitFor(() => expect(confirm).toHaveTextContent("2 running VMs will be moved"));
  expect(confirm).toHaveTextContent("VMs whose disks are on a local pool cannot be moved");
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await screen.findByText("Drain node-a")).toBeInTheDocument();
  expect(posted).toBe(1);
});

it("says why the drain did not start", async () => {
  cluster();
  mockLists({ node: [NODE] });
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () =>
      HttpResponse.json(
        { code: "node_busy", message: 'node "node-a" is already being drained' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    "The node is already being drained.",
  );
});

it("drains only online nodes that run VMs and are not being drained", async () => {
  cluster();
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  const draining = {
    ...task("t1", "node-drain", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [NODE, DOWN, witness], task: [draining] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const busy = await screen.findByRole("menuitem", { name: "Drain node…" });
  await vi.waitFor(() => expect(busy).toHaveAttribute("aria-disabled", "true"));
  expect(busy).toHaveAccessibleDescription("The node is being drained.");
  await userEvent.keyboard("{Escape}");

  await userEvent.click(screen.getByRole("button", { name: "Actions for node-b" }));
  const offline = await screen.findByRole("menuitem", { name: "Drain node…" });
  expect(offline).toHaveAttribute("aria-disabled", "true");
  expect(offline).toHaveAccessibleDescription("The node is offline.");
  await userEvent.keyboard("{Escape}");

  // 위트니스는 VM을 돌리지 않는다.
  await userEvent.click(screen.getByRole("button", { name: "Actions for node-w" }));
  expect(await screen.findByRole("menuitem", { name: "Enter maintenance" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Drain node…" })).not.toBeInTheDocument();
});

it("says why the node cannot be drained", async () => {
  cluster();
  mockLists({ node: [NODE] });
  server.use(
    http.post(apiUrl("/nodes/n1/drain"), () =>
      HttpResponse.json(
        {
          code: "node_unavailable",
          message: 'node "node-a" is offline; HA recovers the HA VMs of an offline node',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Drain node…" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Drain" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    'This node cannot be drained: node "node-a" is offline; HA recovers the HA VMs of an offline node',
  );
});

it("does not drain a macOS node", async () => {
  cluster();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, hypervisor: "qemu-hvf" } }] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  const item = await screen.findByRole("menuitem", { name: "Drain node…" });
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription("VMs on a macOS node cannot be moved.");
});

it("asks before ending maintenance that stops a running drain", async () => {
  cluster();
  const node = {
    ...DOWN,
    status: { ...DOWN.status, online: true },
    spec: { ...DOWN.spec, maintenance: true },
  };
  const draining = {
    ...task("t1", "node-drain", "n2"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [node], task: [draining] });
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      const body = (await request.json()) as { spec: typeof DOWN.spec };
      sent.push(body.spec);
      return HttpResponse.json({ ...node, revision: node.revision + 1, spec: body.spec });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  // Task 목록을 읽은 뒤(비우는 중으로 보인 뒤) 고른다.
  await vi.waitFor(async () =>
    expect(await screen.findByRole("menuitem", { name: "Drain node…" })).toHaveAttribute(
      "aria-disabled",
      "true",
    ),
  );
  await userEvent.click(screen.getByRole("menuitem", { name: "End maintenance" }));
  const confirm = await screen.findByRole("alertdialog", { name: "End maintenance of node-b?" });
  expect(confirm).toHaveTextContent("Ending maintenance stops the running drain.");
  expect(sent).toEqual([]);
  await userEvent.click(within(confirm).getByRole("button", { name: "End maintenance" }));
  await vi.waitFor(() => expect(sent).toEqual([{ address: "10.0.0.1", maintenance: false }]));
  await vi.waitFor(() => expect(confirm).not.toBeInTheDocument());
});
```

`web/src/nodes/NodesPage.tsx` 전체:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { ApiError, api, call } from "@/api/client";
import { applyEvent, listKey, useList, type List } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { PoolStatus } from "@/storage/PoolStatus";
import { trackTask } from "@/tasks/tasks";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

/** 비우기가 옮길 VM의 수: 그 노드에 고정돼 켜 둔 VM(서버의 비우기 Task와 같은 기준) */
function runningCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => vm.spec.node === node.id && vm.spec.power === "running").length;
}

/** 배지 버튼의 접근 이름에 넣는 확인 상태 */
function updatesState(
  t: ReturnType<typeof useTranslation>["t"],
  updates: NonNullable<Node["status"]["updates"]>,
): string {
  if (updates.error) {
    return t("nodes.updates.stateFailed");
  }
  if (!updates.checked_at) {
    return t("nodes.updates.stateNotChecked");
  }
  return t("nodes.updates.stateCount", {
    n: `${updates.packages.length}${updates.truncated ? "+" : ""}`,
  });
}

/** 올릴 수 있는 패키지 수. 확인이 실패했으면 경고, 아직 확인하지 않았으면 비어 있다. */
function UpdatesBadge({ updates }: { updates: NonNullable<Node["status"]["updates"]> }) {
  const { t } = useTranslation();
  if (updates.error) {
    return <Badge variant="destructive">{t("nodes.updates.failedBadge")}</Badge>;
  }
  if (!updates.checked_at) {
    return <span className="text-muted-foreground">—</span>;
  }
  const count = `${updates.packages.length}${updates.truncated ? "+" : ""}`;
  return <Badge variant={updates.packages.length > 0 ? "default" : "outline"}>{count}</Badge>;
}

export function NodesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const [removing, setRemoving] = useState<Node | null>(null);
  const [showingUpdates, setShowingUpdates] = useState<Node | null>(null);
  const [draining, setDraining] = useState<Node | null>(null);
  const [ending, setEnding] = useState<Node | null>(null);
  const tasks = useTaskIndex();
  /** 그 노드의 비우기 Task가 끝나지 않았는지 */
  const isDraining = (node: Node) =>
    (tasks.get(node.id) ?? []).some((task) => task.spec.op.kind === "node-drain");
  // 메뉴에서 연 대화상자를 닫으면 메뉴 항목이 사라지므로, 메뉴를 연 버튼으로 포커스를 돌린다.
  const trigger = useRef<HTMLButtonElement | null>(null);
  const rememberTrigger = (e: React.SyntheticEvent<HTMLButtonElement>) => {
    trigger.current = e.currentTarget;
  };
  const write = can(me.role, "write", "node");

  async function setMaintenance(node: Node, maintenance: boolean) {
    try {
      const updated = await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": node.revision } },
          body: { labels: node.labels, spec: { ...node.spec, maintenance } },
        }),
      );
      // watch 이벤트보다 먼저 반영한다(곧바로 다시 바꿔도 새 revision으로 보낸다).
      queryClient.setQueryData<List<Node>>(
        listKey("node"),
        (list) => list && applyEvent(list, { type: "put", kind: "node", resource: updated }),
      );
      toast.success(
        t(maintenance ? "nodes.maintenanceOn" : "nodes.maintenanceOff", { name: node.name }),
      );
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 id="nodes-title" className="text-2xl font-semibold">
        {t("nav.nodes")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("nodes.joinNote")}</p>
      <Table aria-labelledby="nodes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
            <TableHead>{t("nodes.version")}</TableHead>
            <TableHead>{t("nodes.updates.column")}</TableHead>
            <TableHead>{t("nodes.underlay")}</TableHead>
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.fencing")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
            <TableHead>{t("nodes.memory")}</TableHead>
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.pools")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 15 : 14} query={nodes} empty={t("dashboard.noNodes")} />
          )}
          {(nodes.data ?? []).map((node) => (
            <TableRow key={node.id}>
              <TableCell className="font-medium">
                {node.name}
                {node.id === cluster.data?.node && (
                  <span className="ml-2 text-xs text-muted-foreground">{t("nodes.thisNode")}</span>
                )}
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap gap-1">
                  <Badge variant={node.status.online ? "default" : "destructive"}>
                    {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                  {node.spec.maintenance && (
                    <Badge variant="outline">{t("dashboard.maintenance")}</Badge>
                  )}
                  {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
                </div>
              </TableCell>
              <TableCell className="font-mono text-xs">{node.spec.address}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell className="font-mono text-xs">{node.status.version ?? "—"}</TableCell>
              {/* 업데이트를 확인하지 않는 노드(위트니스, 꺼 둔 노드)는 `updates`가 없다. */}
              <TableCell>
                {node.status.updates ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={t("nodes.updates.open", {
                      name: node.name,
                      state: updatesState(t, node.status.updates),
                    })}
                    onPointerDown={rememberTrigger}
                    onKeyDown={rememberTrigger}
                    onClick={() => setShowingUpdates(node)}
                  >
                    <UpdatesBadge updates={node.status.updates} />
                  </Button>
                ) : (
                  "—"
                )}
              </TableCell>
              {/* VXLAN 패킷을 주고받는 주소. 위트니스와 정하지 못한 노드는 없다. */}
              <TableCell className="font-mono text-xs">{node.status.underlay ?? "—"}</TableCell>
              <TableCell>{node.status.hypervisor}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              {/* 펜싱이 꺼진 노드(위트니스 포함)는 HA VM을 돌리지 않는다. */}
              <TableCell>
                {node.status.ha ? t(`nodes.fencingMode.${node.status.ha.fencing}`) : "—"}
              </TableCell>
              <TableCell>{node.status.cpus}</TableCell>
              <TableCell>{formatCapacity(node.status.memory_mib)}</TableCell>
              <TableCell className="font-mono text-xs">
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
              {/* VM 목록을 다 읽기 전에는 모른다. */}
              <TableCell>{vms.isSuccess ? vmCount(node, vms.data) : "—"}</TableCell>
              {write && (
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: node.name })}
                        onPointerDown={rememberTrigger}
                        onKeyDown={rememberTrigger}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onSelect={() =>
                          // 유지보수를 끝내면 진행 중인 비우기가 멈추므로 먼저 묻는다.
                          node.spec.maintenance && isDraining(node)
                            ? setEnding(node)
                            : void setMaintenance(node, !node.spec.maintenance)
                        }
                      >
                        {t(
                          node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance",
                        )}
                      </DropdownMenuItem>
                      {/* 위트니스는 VM을 돌리지 않는다. */}
                      {!node.spec.witness && (
                        <ReasonMenuItem
                          reason={
                            !node.status.online
                              ? "nodes.drain.reason.offline"
                              : node.status.hypervisor === "qemu-hvf"
                                ? "nodes.drain.reason.hvf"
                                : isDraining(node)
                                  ? "nodes.drain.reason.draining"
                                  : null
                          }
                          onSelect={() => setDraining(node)}
                        >
                          {t("nodes.drain.menu")}
                        </ReasonMenuItem>
                      )}
                      {node.status.updates && (
                        <DropdownMenuItem onSelect={() => setShowingUpdates(node)}>
                          {t("nodes.updates.menu")}
                        </DropdownMenuItem>
                      )}
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
                        reason={
                          node.id === cluster.data?.node
                            ? "nodes.reason.thisNode"
                            : node.status.online
                              ? "nodes.reason.online"
                              : null
                        }
                        onSelect={() => setRemoving(node)}
                      >
                        {t("nodes.remove")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {write && (
        <>
          <p className="text-sm text-muted-foreground">{t("nodes.maintenanceNote")}</p>
          <p className="text-sm text-muted-foreground">{t("nodes.removeNote")}</p>
        </>
      )}
      {showingUpdates && (
        <NodeUpdatesDialog
          node={showingUpdates}
          returnFocus={trigger}
          onClose={() => setShowingUpdates(null)}
        />
      )}
      <ConfirmDialog
        open={draining !== null}
        onOpenChange={(open) => !open && setDraining(null)}
        title={t("nodes.drain.title", { name: draining?.name })}
        description={t("nodes.drain.description")}
        confirmLabel={t("nodes.drain.confirm")}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = draining!;
          try {
            const task = await call(
              api.POST("/nodes/{key}/drain", { params: { path: { key: node.id } } }),
            );
            trackTask(task, t("tasks.op.node-drain", { target: node.name }));
          } catch (err) {
            throw new Error(
              err instanceof ApiError
                ? codeMessage(t, err.code, err.message, "nodes.drain.errors")
                : errorMessage(t, err),
              { cause: err },
            );
          }
        }}
      >
        {/* VM 목록을 다 읽기 전에는 세지 않는다. */}
        {draining && vms.isSuccess && (
          <p className="text-sm">
            {t("nodes.drain.running", { count: runningCount(draining, vms.data) })}
          </p>
        )}
        <p className="text-sm text-muted-foreground">{t("nodes.drain.localNote")}</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={ending !== null}
        onOpenChange={(open) => !open && setEnding(null)}
        title={t("nodes.drain.endTitle", { name: ending?.name })}
        description={t("nodes.drain.endDescription")}
        confirmLabel={t("nodes.endMaintenance")}
        returnFocus={trigger}
        onConfirm={() => setMaintenance(ending!, false)}
      />
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("nodes.removeTitle", { name: removing?.name })}
        description={t("nodes.removeDescription")}
        confirmLabel={t("nodes.remove")}
        typeToConfirm={removing?.name}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = removing!;
          await call(api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }));
          toast.success(t("nodes.removed", { name: node.name }));
        }}
      />
    </div>
  );
}
```

`web/src/vms/VmListPage.test.tsx`에서

```tsx

it("marks HA VMs", async () => {
```

을 다음으로 바꾼다:

```tsx

it("shows why a VM pinned to a node in maintenance did not start", async () => {
  mockLists({ vm: [web, db], node: [NODE] });
  const message =
    'node "node-a" is in maintenance and no other node can run this VM; end maintenance first';
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), () =>
      HttpResponse.json({ code: "node_unavailable", message }, { status: 409 }),
    ),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  await userEvent.type(screen.getByLabelText("Search by name"), "d");
  await userEvent.click(within(screen.getByText("1 selected").parentElement!).getByText("Start"));
  expect(await screen.findByText(`db: The node is unavailable: ${message}`)).toBeInTheDocument();
});

it("marks HA VMs", async () => {
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 노드 비우기 대화상자 — 노드 화면 행 메뉴 \"노드 비우기…\"(admin, 위트니스는 메뉴 없음; 오프라인이거나 macOS(qemu-hvf) 노드이거나 끝나지 않은 node-drain Task가 있으면 이유와 함께 막힘 — nodes.drain.reason.offline·hvf·draining), 확인 대화상자(유지보수로 바뀌고 실행 중인 VM을 스케줄러가 고른 노드로 하나씩 라이브 마이그레이션, 끝나거나 실패해도 유지보수로 남음; /vms에서 그 노드에 고정돼 켜 둔 VM 수 — 서버 비우기 Task와 같은 기준, 로컬 풀 디스크 VM은 옮기지 못하고 비우기가 실패한다는 안내), POST /nodes/{key}/drain 뒤 Task \"X 비우기\"를 따라감. 그 노드에 끝나지 않은 node-drain Task가 있으면 \"유지보수 끝내기\" 전에 확인 대화상자(nodes.drain.endTitle·endDescription \"끝내면 진행 중인 비우기가 멈춥니다\"). 오류 문구 errors.node_busy, 비우기 전용 nodes.drain.errors.node_unavailable(서버 설명을 그대로 붙임), 일반 errors.node_unavailable이 {{message}}를 받음(\"노드를 쓸 수 없습니다: …\" / \"The node is unavailable: …\" — 유지보수 노드에 고정된 VM 켜기의 409 설명이 보임), ko·en. 시험: 옮길 VM 수와 안내·요청·Task 이름, node_busy·node_unavailable 오류, 오프라인·비우는 중 막힘과 위트니스엔 메뉴 없음, macOS 노드 막힘, 비우는 중 유지보수 끝내기 확인, VmListPage 켜기 오류에 서버 설명"
```

---

### Task 5: 문서

README 새 절 "노드 유지보수와 비우기"(절차, 거절 코드, 옮기는 규칙, 한계)와 롤링 업그레이드 순서·상태 줄, 설계 문서 §7 "구현(계획 39)", 노드 코어 설계 §1.1의 하위 프로젝트 11, 마이그레이션·HA·업데이트 설계의 "넣지 않은 것" 메모.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-10-30-live-migration-design.md`
- Modify: `docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`
- Modify: `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`
- Modify: `docs/superpowers/specs/2026-11-09-node-maintenance-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 처음 분해한 하위 프로젝트 1~10 완료 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리, 백업·백업 작업, VMware 가져오기, 감사 로그, LDAP·OIDC, 원격 클러스터·원격 복제)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 처음 분해한 하위 프로젝트 1~10 완료 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트, 구독과 텔레메트리, 백업·백업 작업, VMware 가져오기, 감사 로그, LDAP·OIDC, 원격 클러스터·원격 복제), 하위 프로젝트 11 노드 유지보수 모드(노드 비우기) 완료
```

`README.md`에서

```markdown
1. `oplctl node updates <노드> --refresh`로 무엇이 바뀌는지 본다.
2. 커널처럼 다시 부팅해야 하는 업데이트가 있으면 그 노드의 VM을 다른 노드로 옮기고(`vm migrate`) 웹에서 유지보수
   모드로 둔다.
3. `oplctl node upgrade <노드> --wait`(다른 노드에 로그인해서). 필요하면 노드를 다시 부팅하고 유지보수 모드를 끈다.
4. `oplctl node list`에서 그 노드의 VERSION과 ONLINE을 확인하고 다음 노드로 간다.
```

을 다음으로 바꾼다:

```markdown
1. `oplctl node updates <노드> --refresh`로 무엇이 바뀌는지 본다.
2. 커널처럼 다시 부팅해야 하는 업데이트가 있으면 `oplctl node drain <노드> --wait`로 노드를 유지보수 모드로 두고
   VM을 다른 노드로 옮긴다("노드 유지보수와 비우기" — 어느 노드에서 해도 된다). 비우기가 처음 들어간 버전으로 올리는
   동안은 옛 노드가 비우기를 몰라 쓸 수 없으므로(`cluster_upgrading`) 그때는 VM을 옮기고(`vm migrate`) 유지보수 모드로
   둔다(`oplctl node maintenance <노드> on` 또는 웹 "유지보수 시작"). 그 뒤의 업그레이드에서는 버전이 섞여도 된다.
3. `oplctl node upgrade <노드> --wait`(다른 노드에 로그인해서). 필요하면 노드를 다시 부팅하고 유지보수 모드를
   끈다(`oplctl node maintenance <노드> off`).
4. `oplctl node list`에서 그 노드의 VERSION과 ONLINE을 확인하고 다음 노드로 간다.
```

`README.md`에서

```markdown
  QEMU는 opld 없이도 계속 돌므로, opld가 `watchdog_timeout_secs` 안에 다시 떠 리스를 갱신하지 못하면 노드가
  재시작된다. 업그레이드·점검으로 opld를 오래 멈출 때는 먼저 HA VM을 끄거나 다른 노드로 옮기거나 `vm set --no-ha`를
  한다. 펜싱을 `off`로 바꿔 다시 띄운 노드의 이미 도는 HA VM은 끄지 않고 status에 "HA is not available on this
  node"만 남긴다(멈춘 HA VM은 켜지 않는다).
- **watchdog 준비(Linux).** 하드웨어 watchdog(`iTCO_wdt`, IPMI 등)가 없으면 `softdog`를 올린다:
```

을 다음으로 바꾼다:

```markdown
  QEMU는 opld 없이도 계속 돌므로, opld가 `watchdog_timeout_secs` 안에 다시 떠 리스를 갱신하지 못하면 노드가
  재시작된다. 업그레이드·점검으로 opld를 오래 멈출 때는 먼저 HA VM을 끄거나 다른 노드로 옮기거나(`node drain`, 아래 "노드
  유지보수와 비우기") `vm set --no-ha`를 한다. 펜싱을 `off`로 바꿔 다시 띄운 노드의 이미 도는 HA VM은 끄지 않고
  status에 "HA is not available on this node"만 남긴다(멈춘 HA VM은 켜지 않는다).
- **watchdog 준비(Linux).** 하드웨어 watchdog(`iTCO_wdt`, IPMI 등)가 없으면 `softdog`를 올린다:
```

`README.md`에서

````markdown
sudo modprobe -r softdog                                        # 끝나면
```

## 백업과 복원
````

을 다음으로 바꾼다:

````markdown
sudo modprobe -r softdog                                        # 끝나면
```

## 노드 유지보수와 비우기

커널·펌웨어를 올리거나 하드웨어를 손보기 전에 노드를 **유지보수 모드**로 두고 실행 중인 VM을 다른 노드로 옮긴다
(Proxmox의 노드 유지보수, vSphere의 유지보수 모드와 같은 자리). 유지보수 노드에는 새 VM을 놓지 않는다: 스케줄러의
자동 배치, HA 복구 대상, 마이그레이션 대상에서 빠진다. **비우기**는 노드를 유지보수로 바꾸고 그 노드의 실행 중인
VM을 스케줄러가 고른 노드로 하나씩 라이브 마이그레이션한다.

```bash
oplctl node drain node1 --wait           # 유지보수로 바꾸고 모두 옮길 때까지 기다린다: "node node1 drained"
oplctl node drain node1                  # 기다리지 않는다: "drain of node1 started (task …); follow it with `oplctl task get …`"
oplctl node list                         # MAINTENANCE 열
oplctl node maintenance node1 off        # 유지보수 끝: "node node1: maintenance off"
oplctl node maintenance node1 on         # VM은 옮기지 않고 배치만 막는다
```

웹은 노드 화면 행 메뉴의 "노드 비우기…"(확인 대화상자에 옮길 실행 중 VM 수와 로컬 디스크 안내, 그 뒤 작업을
따라감 — 오프라인·macOS(HVF)·비우는 중인 노드는 이유와 함께 막힘)와 "유지보수 시작/끝"이다. API는 `POST /api/v1/nodes/{key}/drain`(본문 없음, admin, 202와 작업
`node-drain`)이고, 유지보수 켜기·끄기는 노드 수정 `PUT /api/v1/nodes/{key}`의 `spec.maintenance`(admin, If-Match)다.

**절차.** 한 노드씩:

1. `oplctl node drain <노드> --wait`(어느 노드에서 해도 된다 — 옮기는 일은 비우는 노드의 실행기가 한다). 실패하면
   옮기지 못한 VM과 이유가 나온다 — 그 VM을 끄거나 손으로 옮긴 뒤 다시 `drain`한다.
2. 노드를 손본다(업그레이드 `oplctl node upgrade <노드> --wait` — 다른 노드에 로그인해서, 커널 업데이트, 재부팅,
   하드웨어). 비우기가 성공했으면 그 노드에 HA VM이 없으므로 opld를 멈춰도 watchdog이 꺼져 재시작되지 않는다(위
   "HA VM과 자기 펜싱").
3. `oplctl node list`에서 ONLINE을 확인하고 `oplctl node maintenance <노드> off`. 옮긴 VM은 돌아오지 않는다(필요하면
   `vm migrate`).

- **비우기 요청.** 거절: 노드가 없으면 **404**, 위트니스(`node "X" is a witness node and runs no VMs`)나 오프라인
  노드(`node "X" is offline; HA recovers the HA VMs of an offline node` — 꺼진 노드의 HA VM은 HA 복구가 맡는다)는
  **409** `node_unavailable`, 그 노드의 비우기 작업이 끝나지 않았으면 **409** `node_busy`, 비우기를 모르는 노드(위트니스
  포함)가 남았으면 **409** `cluster_upgrading`("some nodes do not support node drain yet (…); upgrade them first"). 버전이
  아니라 노드가 등록할 때 적는 기능 표시(`status.features`의 `node-drain`)를 보므로, 비우기가 처음 들어간 버전으로 올리는
  동안은 못 쓰고(그때는 `vm migrate`와 유지보수 시작으로) 그 뒤의 롤링 업그레이드에서는 버전이 섞여도 된다. 옛 버전
  리더가 노드 상태를 다시 쓰며 이 표시를 지워도 노드가 1분마다 다시 적으므로 다음 주기에 되살아난다 — 새 버전 노드의
  이름이 `cluster_upgrading`에 나오면 1분쯤 뒤 다시 하거나 그 노드의 opld를 다시 시작한다. 노드를
  유지보수로 바꾼 뒤(이미면 그대로) 작업을 만든다. 작업이 실패해도 노드는 유지보수로 남는다.
- **무엇을, 어떻게.** 비우는 노드의 실행기가 그 노드에 배정되고(`spec.node`) 원하는 상태가 running인 VM을 이름 순서로
  한 번에 하나씩 옮긴다. VM에 다른 작업(전원·스냅샷·마이그레이션 등)이 있으면 끝나기를 `[migration] timeout`(기본 30분)까지
  기다린다. 대상은 VM 만들기의 스케줄러와 같은 규칙(온라인, 유지보수·위트니스 아님, 아키텍처·디스크 풀·브리지·방화벽, HA
  VM이면 HA 가능 노드)을 지나고 macOS(HVF) 노드가 아니며 남은 메모리가 VM 메모리 이상인 노드 가운데 미할당 메모리가 가장
  많은 노드(같으면 id가 작은 노드)다. 마이그레이션은 `vm migrate`와 같은 확인을 거친 보통의 `vm-migrate` 작업이고(작업
  목록에 하나씩 보인다), 비우기는 그것이 끝나기를 기다린 뒤(`[migration] timeout`의 두 배와 1분까지 — 넘으면 `migration
  task did not finish`) 다음 VM으로 간다. 그 사이 VM이 꺼지거나 지워지거나 다른 노드로 갔으면 넘어가고(다시 나타나면
  세 번까지 다시 보고, 그 뒤에도 다시 나타나면 `busy`), 확인과 작업 만들기 사이에 다른 작업이 끼면 한 번 더 기다려 다시
  한다. 비우기 작업에는 일반 작업의 시간 제한(600초)이 없다.
- **성공은 노드가 빈 것이다.** 옮길 VM이 없어도 이 노드로 들어오던 마이그레이션(비우기 전에 시작한 `vm migrate`)이
  있으면 끝나기를 `[migration] timeout`까지 기다려(넘으면 `busy`) 들어온 VM도 옮긴다. 비우기가 옮긴 VM이 비우기 중에
  다시 들어오면 한 번 더 옮기지 않고 `moved back during the drain`으로 남긴다.
- **옮기지 못한 VM.** 디스크가 공유 풀이 아닌 VM(`disks on a local pool`), macOS(HVF) 노드(`hvf node`), 받을 노드가 없음
  (`no node with capacity`), 바쁨(`busy` — VM의 다른 작업이나 이 노드로 들어오는 마이그레이션이 `[migration] timeout`까지
  끝나지 않음, 다시 기다린 뒤에도 마이그레이션 작업 만들기가 다른 작업과 겹침, 사라졌다 다시 나타나기를 세 번 넘게 함),
  마이그레이션 작업이 끝나지 않음(`migration task did not finish`), 다시 들어옴(`moved back during the drain`),
  마이그레이션 실패(그 작업의 오류)는 그 노드에 남기고 다음 VM으로 간다. 끝에 작업이 "N VM(s) were not moved: db (disks
  on a local pool), …"로 실패한다(`--wait`면 이 오류와 종료 코드 1). 꺼진 VM은 옮기지 않는다 — 노드에 배정된 채 남고, 켤 때 다른 노드로 간다(아래).
- **유지보수를 끄면 멈춘다.** 비우기는 VM마다(VM이 자리 잡기를 기다린 뒤 마이그레이션을 시작하기 직전에도, 들어오는
  마이그레이션을 기다리는 동안에도) 노드를 다시 읽어 유지보수가 끝났으면 "drain stopped: maintenance ended"로
  실패한다(옮기는 중인 마이그레이션은 끝까지 간다). 그래서 `oplctl node maintenance <노드> off`는 끝나지 않은 비우기가
  있으면 stderr에 `warning: the running drain of X stops (task T)`를 알리고, 웹은 "유지보수 끝내기" 전에 "끝내면 진행 중인
  비우기가 멈춥니다"로 묻는다. opld가 비우기 도중 다시 시작하면 작업은 "interrupted"로 실패한다. 노드는 유지보수로
  남으므로 다시 `drain`하면 남은 VM부터 옮긴다.
- **유지보수 노드에 고정된 VM 켜기.** 꺼진 VM은 노드 배정을 그대로 두므로, 유지보수 노드에 배정된 꺼진 VM을 켜면
  스케줄러가 다른 노드를 골라 VM의 노드를 바꾸고 그곳에서 켠다(UEFI 변수는 옛 노드에서 받아 온다(옛 노드가 꺼져
  있으면 경고 뒤 켜는 노드의 파일·템플릿) — "라이브 마이그레이션"). 고를 노드가 없으면(디스크가 그 노드의 로컬
  풀에만 있는 등) **409** `node_unavailable`("node "X" is in maintenance and no other node can run this VM; end
  maintenance first"). 아직 그 노드에서 돌거나 멈추는 중인 VM은
  옮기지 않는다. VM 만들기·편집에서 유지보수 노드를 새로 고르면 **409** `node_unavailable`("… is in maintenance and
  cannot take VMs; end maintenance first")이고, 이미 그 노드에 있는 VM의 다른 편집은 막지 않는다.
- **한계.** 로컬 디스크 VM은 옮기지 못한다(디스크 복사 마이그레이션이 없다 — 끄거나 공유 풀로 옮긴다). 한 번에
  하나씩 옮기고, 진행률은 각 `vm-migrate` 작업으로 본다. 유지보수가 끝나도 VM을 원래 노드로 되돌리지 않는다. 옮기지
  못한 VM을 끄는 선택과 롤링 업그레이드 자동화(노드를 차례로 비우고 올리고 재부팅)는 없다. 볼륨 만들기·백업 같은
  작업의 노드 고르기는 유지보수를 보지 않는다.

## 백업과 복원
````

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기·원격으로 복제)과 백업 작업(원격 복제), VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기·원격으로 복제)과 백업 작업(원격 복제), VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 비우기, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(LDAP·OIDC, admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 원격 클러스터(상태 카드, 원격 VM 목록과 동작), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`README.md`에서

```markdown
- [엔터프라이즈 설계 (하위 프로젝트 10: 계획 34~38)](docs/superpowers/specs/2026-11-06-enterprise-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [엔터프라이즈 설계 (하위 프로젝트 10: 계획 34~38)](docs/superpowers/specs/2026-11-06-enterprise-design.md)
- [노드 유지보수(노드 비우기) 설계 (하위 프로젝트 11: 계획 39)](docs/superpowers/specs/2026-11-09-node-maintenance-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
| 10 | 엔터프라이즈 플러그인 (LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR) | — / vCenter 기능 |
```

을 다음으로 바꾼다:

```markdown
| 10 | 엔터프라이즈 플러그인 (LDAP/SSO, 감사, 다중 클러스터 관리, 고급 DR) | — / vCenter 기능 |
| 11 | 노드 유지보수 모드(노드 비우기) — 1~10 뒤에 더함 | 노드 유지보수 / Maintenance Mode |
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown
구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR: 백업의 출처 `origin`·받는 중 `receiving`·원격 사본 기록 `replicas`, 받는 쪽 API `POST /backups/receive`·`PUT /backups/{key}/files/{file}`(스트리밍, SHA-256과 크기를 돌려줌)·`complete`(매니페스트를 받는 쪽 id로 쓰고 `ready`)와 24시간 넘게 받는 중인 사본을 지우는 janitor, 보내는 쪽 `backup-replicate` Task(계획 37의 원격 클라이언트로 파일마다 해시를 비교하며 흘려 보냄, 같은 출처의 사본이면 이어 감)와 수동 복제 `POST /backups/{key}/actions/replicate`, 백업 작업의 `replicate`(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 다시, 노드마다 하나), 원본을 지우면(사용자·보존 규칙) 원격 사본도 지움(실패는 경고), 받은 사본은 그 클러스터의 보통 백업이라 그곳에서 복원, `oplctl backup replicate`·목록의 `ORIGIN`·`REPLICAS`·`backup-job --replicate`, 웹 출처 배지·사본 상태·"원격으로 복제"·작업의 원격 복제, README DR 절차, 완료)으로 나눴다(`2026-11-06-enterprise-design.md`). 하위 프로젝트 10은 끝났다. 감사 로그 변조 방지와 원격 syslog 직접 전송·요청 본문 기록, SAML·Kerberos, LDAP 사용자·그룹 동기화, 세분화된 RBAC, 외부 사용자의 2단계 인증, oplctl의 SSO 로그인, 원격 클러스터의 전체 웹 화면·콘솔 중계·클러스터 사이 라이브 마이그레이션, 디스크 단위 연속 복제, 자동 장애 조치와 복구 계획 실행, 복제의 대역폭 제한·증분 전송은 넣지 않았다. 이로써 처음 분해한 하위 프로젝트 1~10이 모두 끝났다.
```

을 다음으로 바꾼다:

```markdown
구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO: `Realm` 종류 `oidc`(issuer·client_id·client_secret(응답·watch에 없음, 공개 클라이언트도 됨)·scope·이름·그룹 클레임·CA), reqwest와 ring으로 직접 만든 인가 코드 흐름(PKCE S256, state·nonce·검증자를 클러스터 CA 키에서 HKDF로 만든 키로 HMAC한 쿠키 `opl_oidc`에 담아 어느 노드나 콜백을 받음), discovery·JWKS 노드 캐시와 모르는 kid 다시 받기, ID 토큰 RS256·ES256 확인과 iss·aud·azp·exp·iat·nbf·nonce, 인가 응답 iss(RFC 9207), userinfo로 빠진 클레임, 그룹 클레임으로 역할과 계획 35의 외부 사용자(이메일 모양 이름, IdP 사용자 sub에 묶음), `/login?error=` 코드와 콜백 감사(시도 제한 없음), IdP 시험 동작, `oplctl realm add-oidc|set-oidc`·`login --token-file`, 웹 로그인 SSO 버튼과 OIDC 영역 화면, 가짜 IdP 시험, 완료), 계획 37(원격 클러스터: 새 리소스 `Remote`(URL·CA 지문·원격 클러스터의 API 토큰 — 토큰은 응답·watch에 없고 id만 보임), 원격 CA 하나만 믿고 호스트 이름은 보지 않는 hyper·rustls 클라이언트(CA는 검증 없이 받아 지문으로 확인, 노드 메모리 캐시), 등록 때 지문·자기 클러스터·토큰(`whoami`) 확인과 `remote_*` 오류, 웹용 `probe`, 리더의 `RemotePoller`가 원격의 `/cluster`·`/nodes`·`/vms`로 상태를 모아 바뀔 때만 씀(opld `[remotes]`), API 중계 `/remotes/{key}/api/{*path}`(GET은 viewer 이상·나머지 admin, 원격 쪽은 토큰 주인의 역할, watch·콘솔 404, 스트리밍, OpenAPI 밖, 감사 detail), `oplctl remote`, 웹 `/remotes` 카드와 원격 VM 목록·동작, 두 `Env`(HTTPS)·opld 두 클러스터 E2E 시험, 완료), 계획 38(원격 복제·DR: 백업의 출처 `origin`·받는 중 `receiving`·원격 사본 기록 `replicas`, 받는 쪽 API `POST /backups/receive`·`PUT /backups/{key}/files/{file}`(스트리밍, SHA-256과 크기를 돌려줌)·`complete`(매니페스트를 받는 쪽 id로 쓰고 `ready`)와 24시간 넘게 받는 중인 사본을 지우는 janitor, 보내는 쪽 `backup-replicate` Task(계획 37의 원격 클라이언트로 파일마다 해시를 비교하며 흘려 보냄, 같은 출처의 사본이면 이어 감)와 수동 복제 `POST /backups/{key}/actions/replicate`, 백업 작업의 `replicate`(스케줄러가 ready 백업을 보내고 실패한 사본은 실행마다 다시, 노드마다 하나), 원본을 지우면(사용자·보존 규칙) 원격 사본도 지움(실패는 경고), 받은 사본은 그 클러스터의 보통 백업이라 그곳에서 복원, `oplctl backup replicate`·목록의 `ORIGIN`·`REPLICAS`·`backup-job --replicate`, 웹 출처 배지·사본 상태·"원격으로 복제"·작업의 원격 복제, README DR 절차, 완료)으로 나눴다(`2026-11-06-enterprise-design.md`). 하위 프로젝트 10은 끝났다. 감사 로그 변조 방지와 원격 syslog 직접 전송·요청 본문 기록, SAML·Kerberos, LDAP 사용자·그룹 동기화, 세분화된 RBAC, 외부 사용자의 2단계 인증, oplctl의 SSO 로그인, 원격 클러스터의 전체 웹 화면·콘솔 중계·클러스터 사이 라이브 마이그레이션, 디스크 단위 연속 복제, 자동 장애 조치와 복구 계획 실행, 복제의 대역폭 제한·증분 전송은 넣지 않았다. 이로써 처음 분해한 하위 프로젝트 1~10이 모두 끝났다.

구현: 하위 프로젝트 11은 처음 분해한 하위 프로젝트 1~10이 끝난 뒤 라이브 마이그레이션·HA·업데이트 설계가 넣지 않은 것으로 남긴 "유지보수 때 모두 옮기기"·"노드 비우기(유지보수 모드)"를 채운다. 계획 39(노드 비우기: `POST /api/v1/nodes/{key}/drain`(admin)이 노드를 유지보수로 바꾸고 `node-drain` Task를 그 노드에 만듦(위트니스·오프라인 409 `node_unavailable`, 이미 비우는 중 409 `node_busy`), Task가 실행 중인 VM을 이름 순으로 하나씩 스케줄러가 고른 노드로 `vms::start_migration`(마이그레이션 API와 같은 확인)의 `VmMigrate` Task로 옮기고 끝나기를 기다림, 로컬 풀 디스크·HVF·자리 없음·busy·마이그레이션 실패는 남기고 "N VM(s) were not moved: …"로 실패, 유지보수를 끄면 멈춤, 일반 600초 제한 없이 노드의 줄에 섬; 유지보수 노드에 고정된 꺼진 VM을 켜면 다른 노드에 놓고 못 하면 409, 만들기·고치기에서 유지보수 노드를 새로 고르면 409; `oplctl node maintenance on|off`·`node drain [--wait]`, 웹 노드 화면 "노드 비우기…", README 절차, 실제 QEMU 시험, 완료)로 했다(`2026-11-09-node-maintenance-design.md`). 하위 프로젝트 11은 끝났다. 디스크까지 복사하는 마이그레이션(로컬 디스크 VM 옮기기), 유지보수가 끝난 뒤 VM 되돌리기, 옮기지 못한 VM을 끄는 선택, 롤링 업그레이드 자동화와 노드 재부팅은 넣지 않았다.
```

`docs/superpowers/specs/2026-10-30-live-migration-design.md`에서

```markdown

넣지 않는 것: 디스크까지 복사하는 마이그레이션(로컬 풀, `blockdev-mirror` — 디스크가 모두 공유 풀에 있어야 한다), 마이그레이션 취소(시간 제한으로 끝난다), 사후 복사(post-copy), 다른 QEMU 버전 사이의 마이그레이션, CPU 모델 맞추기(같은 CPU의 노드를 전제), macOS 노드(HVF), HA·자동 재배치(뒤의 하위 프로젝트), 노드 유지보수 때 모두 옮기기, 풀의 `nodes`·백엔드 바꾸기.
```

을 다음으로 바꾼다:

```markdown

넣지 않는 것: 디스크까지 복사하는 마이그레이션(로컬 풀, `blockdev-mirror` — 디스크가 모두 공유 풀에 있어야 한다), 마이그레이션 취소(시간 제한으로 끝난다), 사후 복사(post-copy), 다른 QEMU 버전 사이의 마이그레이션, CPU 모델 맞추기(같은 CPU의 노드를 전제), macOS 노드(HVF), HA·자동 재배치(뒤의 하위 프로젝트), 노드 유지보수 때 모두 옮기기(계획 39에서 노드 비우기로 됨 — `2026-11-09-node-maintenance-design.md`), 풀의 `nodes`·백엔드 바꾸기.
```

`docs/superpowers/specs/2026-11-02-ha-fencing-witness-design.md`에서

```markdown

넣지 않는 것: IPMI·BMC 같은 외부 펜싱 장치, 노드 유지보수 때 모두 옮기기(유지보수 모드는 지금처럼 배치만 막는다), HA 그룹·노드 선호·우선순위·반친화성, 복구 뒤 원래 노드로 되돌리기, HA 아닌 VM의 자동 복구, 죽은 노드에만 있던 UEFI 변수 살리기, RBD 배타 잠금·blocklist, sanlock `wdmd`와 watchdog 나눠 쓰기, 노드 하나에 여러 클러스터의 위트니스.
```

을 다음으로 바꾼다:

```markdown

넣지 않는 것: IPMI·BMC 같은 외부 펜싱 장치, 노드 유지보수 때 모두 옮기기(유지보수 모드는 지금처럼 배치만 막는다 — 계획 39에서 노드 비우기로 됨 — `2026-11-09-node-maintenance-design.md`), HA 그룹·노드 선호·우선순위·반친화성, 복구 뒤 원래 노드로 되돌리기, HA 아닌 VM의 자동 복구, 죽은 노드에만 있던 UEFI 변수 살리기, RBD 배타 잠금·blocklist, sanlock `wdmd`와 watchdog 나눠 쓰기, 노드 하나에 여러 클러스터의 위트니스.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown

넣지 않는 것: 오프라인 설치(DVD 크기 ISO, 모든 의존 패키지 포함 — 설치에는 Debian 미러가 필요하다), ZFS 루트와 소프트웨어 RAID 설치 선택지, arm64 ISO(패키지는 나중에 arm64도), 기능 막기(구독이 없어도 모든 기능이 돈다), 온라인 구독 확인·활성화 서버, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동 업그레이드(사람이 누른다), 노드 비우기(유지보수 모드)와 자동 롤링 업그레이드, 커널·펌웨어 관리, 업데이트 되돌리기.
```

을 다음으로 바꾼다:

```markdown

넣지 않는 것: 오프라인 설치(DVD 크기 ISO, 모든 의존 패키지 포함 — 설치에는 Debian 미러가 필요하다), ZFS 루트와 소프트웨어 RAID 설치 선택지, arm64 ISO(패키지는 나중에 arm64도), 기능 막기(구독이 없어도 모든 기능이 돈다), 온라인 구독 확인·활성화 서버, 구독 키의 클러스터 묶기(하드웨어 지문), 텔레메트리 수집 서버, 자동 업그레이드(사람이 누른다), 노드 비우기(유지보수 모드 — 계획 39에서 됨, `2026-11-09-node-maintenance-design.md`)와 자동 롤링 업그레이드, 커널·펌웨어 관리, 업데이트 되돌리기.
```

`docs/superpowers/specs/2026-11-09-node-maintenance-design.md`에서

```markdown
- 볼륨 만들기·백업 같은 Task의 노드 고르기는 유지보수를 보지 않는다(VM을 돌리지 않는 짧은 작업).
```

을 다음으로 바꾼다:

```markdown
- 볼륨 만들기·백업 같은 Task의 노드 고르기는 유지보수를 보지 않는다(VM을 돌리지 않는 짧은 작업).

## 7. 구현(계획 39)

- 타입: `TaskOp::NodeDrain`(`{"kind":"node-drain"}`, 필드 없음, 대상과 노드는 비울 노드). 실행기는 VM·백업 줄처럼 대상(노드) id로 줄을 세워 한 노드를 한 번에 하나만 비우고, 일반 600초 제한을 두지 않는다(VM마다 기다림만 `migration_timeout`).
- API(`opl_api::drain`): `POST /nodes/{key}/drain`(노드 쓰기 — admin, tag nodes) → 202 Task 리소스(업그레이드와 같은 모양). 순서는 §3.1대로 403 → 404 → 위트니스 409 `node_unavailable`("node "X" is a witness node and runs no VMs") → 오프라인 409 `node_unavailable`("node "X" is offline; HA recovers the HA VMs of an offline node") → 끝나지 않은 `node-drain` Task 409 `node_busy`("node "X" is already being drained") → `cluster::require_feature(cluster, "node-drain")`(§3.1의 `require_current` 대신: 위트니스 포함 모든 노드가 `NodeStatus.features`에 그 기능을 적었어야, 아니면 409 `cluster_upgrading` "some nodes do not support node drain yet (a, b); upgrade them first") → `maintenance = true`(이미면 쓰지 않음, revision 확인 쓰기를 10번까지 다시 하고 넘으면 409 `busy`) → Task. 같은 프로세스에 함께 온 요청은 정적 뮤텍스로 차례로 검사한다(다른 노드로 온 요청과 겹쳐도 실행기의 노드 줄이 차례로 한다).
- 기능 표시: `NodeStatus.features: Vec<String>`(serde default, 비면 쓰지 않음 — `NodeStatus`는 모르는 필드를 무시하므로 옛 노드도 읽는다), 이 빌드의 목록은 `opl_types::node::FEATURES`(`["node-drain"]`), opld 등록(`record_node`·`register_witness`)이 적는다. 옛 버전 리더가 노드 status를 다시 쓰면 모르는 `features`가 빠지므로, 노드와 위트니스가 주기 갱신(`BRIDGE_REFRESH` 60초, `bootstrap::refresh_features` — 이 빌드의 목록과 다를 때만 revision 확인 쓰기)으로 되살린다. 그래서 새 버전 노드가 `cluster_upgrading`에 나와도 다음 주기 뒤(또는 그 노드의 opld를 다시 시작하면) 풀린다. `require_current`는 롤링 업그레이드 중(한 노드만 새 버전) 비우기를 늘 막아 "노드 하나씩 비우고 올림" 절차가 깨지므로 버전 대신 기능을 본다: 비우기가 처음 들어간 버전으로 올리는 동안은 못 쓰고, 그 뒤의 업그레이드에서는 버전이 섞여도 된다.
- `vms::start_migration(state, vm, target, source: Option<Uuid>) -> Result<Task, ApiError>`(pub(crate)): `migrate_vm`의 확인과 Task 만들기를 뽑은 함수로, 스스로 `state.vm_lock`을 잡고 VM·노드를 id로 다시 읽는다. `source`가 있으면(비우기) 그 아래에서 VM이 아직 그 노드에 있는지 보고 아니면 409 `vm_moved`다. `migrate_vm`은 `None`으로 부르고 동작은 그대로다. 실행기의 `Ctx`에는 `AppState`가 없어 `TaskRunnerConfig.api: Option<Arc<AppState>>`를 더했다(opld `member.rs`와 시험 도우미가 넣음, 없으면 비우기 Task가 "this node cannot create migration tasks"로 실패). `vm_lock`은 노드마다라 API 노드와 공유되지 않는다(기존 `migrate_vm`과 같은 한계 — revision 확인 쓰기가 막음).
- 비우기 Task(`tasks/drain.rs`): §3.2와 같다. 다른 점과 구체: 옮기지 못한 VM은 다시 보지 않는다. 옮길 것이 없어진(꺼짐·지워짐·다른 노드로 감) VM은 다시 나타나면 다시 보되 세 번까지(그 뒤 `busy`), 옮긴 VM이 다시 들어오면 한 번 더 옮기지 않고 `moved back during the drain`(끝이 있게). 성공은 노드가 빈 것이다: 옮길 VM이 없으면 이 노드로 들어오는 마이그레이션(`status.migration`의 대상이 이 노드인 VM, 끝나지 않은 `VmMigrate { node: 이 노드 }` Task)이 끝나기를 최대 `migration_timeout` 기다려(넘으면 그 VM은 `busy`) 다시 반복하므로 들어온 VM도 옮긴다. 기다리기(settle)는 VM이 이 노드에서 실행 중이고 phase `running`이며 `status.migration`이 없고 그 VM을 대상으로 한 끝나지 않은 Task가 없을 때까지(`poll_interval`마다 클러스터에서 다시 읽음, `migration_timeout`을 넘기면 `busy`). 기다린 뒤(마이그레이션을 시작하기 직전)와 들어오는 마이그레이션을 기다리는 동안에도 노드를 다시 읽어 유지보수가 끝났으면 멈춘다. 대상은 `node_fits`를 지나고 `qemu-hvf`가 아니며 `free_memory_mib`가 VM 메모리 이상인 노드 가운데 남은 메모리가 가장 많은 노드(같으면 id가 작은 것, §3.2의 "`schedule`의 첫 결과"가 HVF 노드면 다른 노드를 보지 못하므로). `start_migration`은 원본(이 노드)을 넘겨 부르고, 오류는 `vm_not_running`·`not_found`·`vm_moved`면 넘어가고, `vm_busy`면 한 번 더 기다려 다시 하고 그래도면 `busy`, 나머지(`no_capacity` 포함)는 오류 메시지 그대로. 마이그레이션 Task는 `poll_interval`마다 다시 읽어 끝나기를 기다리고, 읽기가 계속 실패하거나 끝나지 않으면 `migration_timeout`의 두 배 + 1분에서 `migration task did not finish`. 실패 문구는 이름 순 `"N VM(s) were not moved: a (disks on a local pool), b (busy)"`.
- VM 켜기·고치기(`vms.rs`): `start`는 유지보수 노드에 고정된 VM이 **완전히 꺼져 있을 때만**(`is_stopped`, 같은 revision 확인 쓰기 안에서) `schedule`로 다른 노드를 골라 `spec.node`를 바꾼다. 아직 그 노드에서 돌거나 멈추는 중이면 그대로 둔다. `prepare`(만들기·고치기·스냅샷 되돌리기)는 요청한 노드가 지금 노드와 다를 때만 유지보수를 확인한다(409 `node_unavailable` "node "X" is in maintenance and cannot take VMs; end maintenance first" — §3.3의 "직접 고름"을 "새로 고름"으로 좁힘: 이미 그 노드에 있는 VM의 다른 고치기와 다시 켜기는 막지 않는다).
- oplctl: `node maintenance <노드> on|off`(GET → 라벨과 spec은 그대로 두고 `spec.maintenance`만 바꿔 읽은 revision으로 If-Match PUT, `revision_mismatch`면 다시 읽어 세 번까지, 출력 `node X: maintenance on`; 끄면 그 노드의 끝나지 않은 `node-drain` Task를 찾아 stderr에 `warning: the running drain of X stops (task T)`, 목록을 읽지 못하면 `warning: cannot check for a running drain: …`만 쓰고 성공), `node drain <노드> [--wait]`(기다리지 않으면 ``drain of X started (task T); follow it with `oplctl task get T` ``, `--wait`이면 `node X drained` 또는 Task 오류와 종료 코드 1). 기다리는 한도는 §3.6의 "`migration_timeout` × VM 수" 대신 고정 `client::DRAIN_WAIT`(24시간)이다 — oplctl은 서버의 `migration_timeout`과 옮길 VM 수를 모른다.
- 웹(`web/src/nodes/NodesPage.tsx`): 행 메뉴 "노드 비우기…"(노드 쓰기 권한 메뉴, 위트니스는 메뉴 없음, 오프라인·하이퍼바이저 `qemu-hvf`("macOS 노드의 VM은 옮길 수 없습니다")·끝나지 않은 `node-drain` Task가 있으면 이유와 함께 막힘), "유지보수 끝내기"는 끝나지 않은 `node-drain` Task가 있으면 확인 대화상자("끝내면 진행 중인 비우기가 멈춥니다"), 확인 대화상자(`/vms`에서 `spec.node`가 이 노드이고 `spec.power`가 running인 VM 수 — Task와 같은 기준, 로컬 디스크 VM은 옮기지 못하고 비우기가 실패한다는 고정 안내 — VM마다 풀을 따지지 않음), POST 뒤 Task "X 비우기"를 따라감. i18n `nodes.drain.*`·`errors.node_busy`·`tasks.op.node-drain`, ko·en. 일반 문구 `errors.node_unavailable`에 서버 설명을 붙여(`{{message}}`) VM 켜기의 409가 이유를 보인다.
- 시험: opl-types `TaskOp` JSON; opl-api `tests/vms.rs`(유지보수 노드에 고정된 꺼진 VM이 다른 노드에서 켜짐, 그 노드밖에 없으면 409, 만들기·고치기에서 유지보수 노드를 고르면 409, 실행 중 VM 고치기·다시 켜기는 그대로), `tests/drain.rs`(권한·404·유지보수로 바뀜·Task, 위트니스·오프라인 409, `node_busy`가 `cluster_upgrading`보다 먼저, 기능 없는 노드(위트니스) 409와 옛 버전이어도 기능이 있으면 202, 빈 노드 성공, 옮기지 못한 이유 — 로컬 풀·메모리가 모자란 노드뿐(자리 없음)·HVF, HVF 노드를 건너뛰고 다른 노드로 `VmMigrate`, settle의 `busy` 시간 초과), `tests/two_nodes.rs`(두 노드 실제 mTLS gRPC, 가짜 하이퍼바이저: 공유 풀 VM 둘이 이름 순으로 옮겨지고 성공, 로컬 디스크 VM은 남고 실패 메시지, 비우기를 시작할 때 들어오던 VM도 들어온 뒤 옮김, 옮기는 중 유지보수를 끄면 멈춤); opld 등록이 기능을 적음, 지운 기능 표시를 주기 갱신이 되살림; oplctl 파싱·fake_api(GET → If-Match PUT 본문, `revision_mismatch` 다시 하기, 끄기의 비우기 경고와 Task 목록을 읽지 못할 때의 경고·성공, drain 출력·`--wait` 성공과 실패); opld E2E `tests/migration.rs` `drain_moves_a_running_vm_to_the_other_node`(기능 `migration-it`, 실제 QEMU — macOS는 TCG: 노드 1의 실행 중 VM을 `node drain --wait`로 노드 2에, 원본 QEMU 정리, `maintenance off` 뒤에도 노드 2에 남음); 웹 Vitest(옮길 VM 수와 안내·요청·Task 이름, `node_busy`·`node_unavailable` 오류, 오프라인·macOS·비우는 중 막힘, 위트니스에 메뉴 없음, 비우는 중 유지보수 끝내기 확인, VM 켜기 `node_unavailable` 문구).
- §5와 다른 점: 고정 VM 켜기 시험은 `two_nodes.rs`가 아니라 `tests/vms.rs`(가짜 노드 기록)에 있다. §4의 `cluster_upgrading`은 "옛 버전 노드"가 아니라 "비우기 기능을 적지 않은 노드"다.
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 노드 유지보수와 비우기 — README 새 절 \"노드 유지보수와 비우기\"(유지보수 노드는 배치·HA 복구·마이그레이션 대상에서 빠짐, oplctl node drain [--wait]·node maintenance on|off·MAINTENANCE 열, 웹 \"노드 비우기…\"·API POST /nodes/{key}/drain, 절차 — 비우기 → 손보기·업그레이드·재부팅(비우기가 성공하면 HA VM이 없어 watchdog이 꺼짐) → 유지보수 끝, 거절 404·node_unavailable·node_busy·cluster_upgrading, 이름 순 하나씩·스케줄러 규칙과 남은 메모리·vm-migrate Task·600초 제한 없음, 옮기지 못한 이유와 실패 문구, 유지보수를 끄면 멈춤·재시작 뒤 다시 drain, 유지보수 노드에 고정된 VM 켜기와 만들기·편집 409, 한계), 업데이트 롤링 순서에 node drain·node maintenance off, HA 절의 opld 멈추기 안내에 node drain, 웹 UI 기능 목록·상단 상태·문서 목록에 설계. 설계 §7 \"구현(계획 39)\"(타입·API·start_migration과 TaskRunnerConfig.api·비우기 Task·켜기와 prepare·oplctl DRAIN_WAIT 24시간·웹·시험, 설계와 다른 점). 노드 코어 설계 §1.1에 하위 프로젝트 11 줄과 계획 39 완료, 라이브 마이그레이션·HA·업데이트 설계의 넣지 않는 것에 계획 39 메모"
```

---

## 완료 기준 (계획 39)

- `oplctl node drain <노드> --wait`가 그 노드의 공유 풀 VM을 모두 다른 노드로 옮기고 성공하며, 노드는 유지보수로 남는다. 로컬 디스크 VM이 있으면 남기고 이름과 이유로 실패한다.
- 유지보수 노드에 고정된 꺼진 VM을 켜면 다른 노드에서 켜진다(못 하면 409).
- 섞인 버전이라도 모든 노드가 `node-drain` 기능을 적었으면 비우기가 된다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, opld `migration-it` E2E, 웹 `typecheck`·`lint`·`test`·`build`가 통과한다.

## 알고 받아들인 제한

- 로컬 디스크 VM은 옮기지 못한다(직접 끄거나 유지보수를 끝낸다). macOS(HVF) 노드는 비우지 못한다.
- 한 번에 하나씩 옮기고, 진행률은 각 `VmMigrate` Task로 본다.
- 유지보수가 끝나도 VM을 되돌리지 않는다.
- 이 기능이 처음 들어간 버전으로 올리는 동안은 비우기를 쓰지 못한다(`vm migrate`와 "유지보수 시작"으로).
- 그 밖의 제한은 설계 문서 §6·§7과 README "노드 유지보수와 비우기" 절에 있다.

## 실행 후 기록 (2026-11-09)

- **Task 1~5:** `feat/plan39-drain`에서 계획대로 적용했다. Task마다 검증한 스테이지 커밋과 트리가 같았다(생성 파일 제외). Task 3의 두 노드 비우기 E2E(`migration-it`, 실제 QEMU)도 이 Mac에서 통과했다.
- **1차 리뷰에서 고친 것:**
  - 새 Task 종류를 `require_current`로 막으면 롤링 업그레이드 중에 쓸 수 없어서, 노드가 지원하는 기능 목록(`NodeStatus.features`, `node-drain`)으로 바꿨다(옛 노드가 있으면 409 `cluster_upgrading`).
  - 남은 VM이 있어도 성공하던 것을 고쳐, 노드가 비었을 때만 성공한다(들어오는 마이그레이션도 기다림, 비우기 중에 되돌아온 VM도 다시 봄).
  - 대상 고르기: HVF 노드 빼기, 남은 메모리 ≥ VM 메모리, 남은 메모리가 많은 노드부터. 반복마다 유지보수 여부와 원본 노드를 다시 확인한다. 마이그레이션 기다림 제한은 `migration_timeout*2+60s`.
  - oplctl 유지보수 끝 경고, 웹 확인 대화상자·HVF 노드 막기, 켜기 오류 메시지, 문서 보강.
- **2차 리뷰에서 고친 것:** 옛 리더가 기능 목록을 지울 수 있어서 opld가 60초마다 `refresh_features`로 다시 씀(위트니스도), 사라진 VM은 3번까지 다시 보고 `busy`, oplctl이 Task 조회에 실패해도 경고만 하고 0으로 끝냄, 커밋 메시지·README 문구.
- **불안정한 시험(부하 때, 다시 돌리면 통과):** opld `cluster.rs` a_witness_votes_but_never_leads_or_runs_vms·three_processes_form_a_cluster_that_survives_losing_a_node(Task 1), opld `smoke` 4개 "API did not start"(Task 2).

**테스트:**
- macOS `cargo test --workspace` 1540개(Task 3 이후)와 opld `migration-it` 3개
- 웹 시험 555개와 `typecheck`·`lint`·`build`
- `fmt`·`clippy -D warnings`(모든 기능)

Linux CI(프로브)에서 같은 시험과 linux-kvm·ldap 작업이 통과했다.

## 다음 계획으로 넘길 사항

- 하위 프로젝트 11(노드 유지보수·비우기)이 끝났다.
- 다음 후보: 하위 프로젝트 12 증분 백업(QEMU dirty bitmap)과 증분 원격 복제, 하위 프로젝트 13 디스크까지 복사하는 마이그레이션(로컬 디스크 VM 옮기기).
- 넣지 않은 것: 유지보수가 끝난 뒤 VM 되돌리기, 옮기지 못한 VM 끄기, 롤링 업그레이드 자동화, 동시 여러 마이그레이션.
