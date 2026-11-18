//! VM 펌웨어(UEFI 변수·TPM 상태, 계획 59)의 API: 초기화 Task(`vm-firmware-reset`)를 만드는 경로, Secure Boot 바꾸기와
//! 꺼진 TPM VM 켜기의 미리 보기.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_types::firmware::{TpmSource, tpm_source};
use opl_types::node::{NodeSpec, VTPM};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::snapshots::{check_no_snapshot_task, check_no_task, check_not_busy, vm_node};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::is_stopped;

/// 초기화할 것. 적어도 하나는 참이어야 한다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FirmwareResetRequest {
    /// UEFI 변수(부팅 항목, Secure Boot 키)를 펌웨어 원본으로 되돌린다.
    #[serde(default)]
    pub uefi_vars: bool,
    /// TPM 상태를 버린다(다음 켜기가 새 TPM을 만든다). TPM에 봉인한 비밀(BitLocker·LUKS 키)을 잃는다.
    #[serde(default)]
    pub tpm: bool,
    /// UEFI 변수 초기화와 함께 VM 스펙의 `secure_boot`를 이 값으로 바꾼다(`uefi_vars`가 있어야 한다). 변수가 있는
    /// VM의 Secure Boot는 이 길로만 바꾼다(`PUT`은 409 `firmware_reset_required`).
    #[serde(default)]
    #[schema(nullable = false)]
    pub secure_boot: Option<bool>,
}

/// 끝나지 않은 펌웨어 초기화 Task가 있으면 409 `vm_busy`: 초기화 Task는 꺼진 VM에만 되므로 그 전에 켜지 않는다.
pub(crate) fn check_no_firmware_reset_task(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    check_no_task(state, vm, "having its firmware reset", |op| {
        matches!(op, TaskOp::VmFirmwareReset { .. })
    })
}

/// VM `vm`의 `secure_boot`를 `on`으로 바꿀 수 있는지(설계 §6): 꺼져 있어야 하고(409 `vm_running` — SMM과 펌웨어가
/// 바뀐다), UEFI 변수가 없어야 한다(`firmware_node`가 있으면 409 `firmware_reset_required` — 키 없이 만든 변수로는
/// Secure Boot가 설정 모드로 남고, 키가 든 변수로 끄면 계속 막는다).
pub(crate) fn check_secure_boot_change(vm: &Vm, on: bool) -> Result<(), ApiError> {
    let (turn, was) = if on { ("on", "off") } else { ("off", "on") };
    if !is_stopped(vm) {
        return Err(ApiError::conflict(
            "vm_running",
            format!(
                "cannot turn Secure Boot {turn} for VM {:?} while it is running; stop it first",
                vm.name
            ),
        ));
    }
    if vm.status.firmware_node.is_some() {
        return Err(ApiError::conflict(
            "firmware_reset_required",
            format!(
                "VM {name:?} has UEFI variables made for Secure Boot {was}; reset them together with the change \
                 (`vm set {name} --secure-boot {turn} --reset-uefi-vars`, boot entries are lost) before turning Secure Boot {turn}",
                name = vm.name
            ),
        ));
    }
    Ok(())
}

/// 꺼진 TPM VM `vm`(스펙은 켤 모양, status는 지금 것)을 노드 `node`에서 켤 때 TPM 상태를 얻을 수 있는지 미리 본다(설계
/// §5 켜기 규칙). 얻을 곳이 없으면 409 `tpm_state_unavailable`(작업자가 남길 문구와 같다). `node`가 `firmware_node`나
/// 사본 노드면 그 노드의 파일이 지금 해시와 같다고 본다(API는 파일을 볼 수 없다 — 없으면 작업자가 사본에서 받는다).
pub(crate) fn check_tpm_state(state: &AppState, vm: &Vm, node: Uuid) -> Result<(), ApiError> {
    if !vm.spec.tpm {
        return Ok(());
    }
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let local = vm.status.tpm_sha256.as_deref().filter(|_| {
        vm.status.firmware_node == Some(node) || vm.status.firmware_copies.contains(&node)
    });
    match tpm_source(vm, node, local, &nodes) {
        TpmSource::Unavailable(message) => {
            Err(ApiError::conflict("tpm_state_unavailable", message))
        }
        _ => Ok(()),
    }
}

/// 초기화 Task를 돌릴 노드: 온라인인 `firmware_node`(그 노드의 파일을 지운다), 없거나 오프라인이면 VM의 디스크로 정한
/// 노드다. TPM 상태가 있는 VM의 변수만 초기화는 `firmware_node`가 오프라인이면 409 `node_unavailable`이다(다른
/// 노드에서 지우면 나중에 그 노드의 TPM 상태와 함께 낡은 변수가 돌아온다 — 설계 §16 S4).
fn reset_node(state: &AppState, vm: &Vm, uefi_vars: bool, tpm: bool) -> Result<Uuid, ApiError> {
    let Some(holder) = vm.status.firmware_node else {
        return Ok(vm_node(state, vm));
    };
    let node = state.cluster.get::<NodeSpec>(holder)?;
    if node
        .as_ref()
        .is_some_and(|n| n.status.online && !n.spec.witness)
    {
        return Ok(holder);
    }
    let tpm_vm = vm.spec.tpm || vm.status.tpm_sha256.is_some();
    if uefi_vars && !tpm && tpm_vm {
        let name = node.map_or_else(|| holder.to_string(), |n| n.name);
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "the UEFI variables and TPM state of VM {:?} are on node {name:?} (offline); reset the UEFI variables when {name:?} is back, or reset the TPM as well",
                vm.name
            ),
        ));
    }
    Ok(vm_node(state, vm))
}

/// 꺼진 VM의 UEFI 변수·TPM 상태를 초기화할 Task(`vm-firmware-reset`)를 만든다(202, 설계 §5). Task는 온라인인
/// `firmware_node`에서 돌고(없거나 오프라인이면 VM의 디스크가 있는 노드), 그 노드의 파일을 지우고 `firmware_epoch`를
/// 올린다(다른 노드의 사본은 회차가 달라 켤 때 지워진다). `uefi_vars`·`tpm`이 모두 거짓이면 400, VM이 꺼져 있지 않으면
/// 409 `vm_running`, 켜거나 끄는 중이거나 마이그레이션·스냅샷·복제·백업 Task가 끝나지 않았으면 409 `vm_busy`, 모든
/// 노드가 `vtpm`을 알아야 한다(409 `cluster_upgrading`). TPM 상태가 있는 VM의 변수만 초기화는 `firmware_node`가
/// 오프라인이면 409 `node_unavailable`이다. `secure_boot`를 주면 Task가 스펙의 `secure_boot`도 바꾼다(UEFI 펌웨어가
/// 아니면 400). Task가 끝날 때까지 켜기는 409 `vm_busy`다.
#[utoipa::path(post, path = "/vms/{key}/firmware/reset", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    request_body = FirmwareResetRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn reset_firmware(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<FirmwareResetRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    require_feature(&state.cluster, VTPM)?;
    let FirmwareResetRequest {
        uefi_vars,
        tpm,
        secure_boot,
    } = req;
    if !uefi_vars && !tpm {
        return Err(ApiError::bad_request(
            "nothing to reset: set uefi_vars, tpm or both",
        ));
    }
    if secure_boot.is_some() && !uefi_vars {
        return Err(ApiError::bad_request(
            "secure_boot can change only together with a reset of the UEFI variables (uefi_vars)",
        ));
    }
    // 검사와 Task·스펙 쓰기 사이에 켜기·고치기가 끼지 않게 한다(켜기는 끝나지 않은 초기화 Task를 본다).
    let _lock = state.vm_lock.lock().await;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    if !is_stopped(&vm) {
        return Err(ApiError::conflict(
            "vm_running",
            format!(
                "cannot reset the firmware of VM {:?} while it is running; stop it first",
                vm.name
            ),
        ));
    }
    check_not_busy(&state, &vm)?;
    check_no_snapshot_task(&state, &vm)?;
    let secure_boot = secure_boot.filter(|on| *on != vm.spec.secure_boot);
    if let Some(on) = secure_boot {
        let mut spec = vm.spec.clone();
        spec.secure_boot = on;
        spec.validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
    }
    let node = reset_node(&state, &vm, uefi_vars, tpm)?;
    // Secure Boot 바꾸기는 Task가 초기화와 함께 쓴다(리뷰 1 M1): Task를 만든 뒤 스펙 쓰기가 실패하면 되돌릴 수 없는 초기화만
    // 남는다.
    let op = TaskOp::VmFirmwareReset {
        uefi_vars,
        tpm,
        secure_boot,
    };
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    audit.detail(reset_detail(&vm.name, uefi_vars, tpm, secure_boot));
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 감사 설명: `vm firmware reset win (uefi vars, tpm)`, Secure Boot를 함께 바꾸면 `, secure boot on`을 더한다.
fn reset_detail(vm: &str, uefi_vars: bool, tpm: bool, secure_boot: Option<bool>) -> String {
    let mut what: Vec<&str> = Vec::new();
    if uefi_vars {
        what.push("uefi vars");
    }
    if tpm {
        what.push("tpm");
    }
    match secure_boot {
        Some(true) => what.push("secure boot on"),
        Some(false) => what.push("secure boot off"),
        None => {}
    }
    format!("vm firmware reset {vm} ({})", what.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_detail_names_what_was_reset() {
        assert_eq!(
            reset_detail("win", true, true, None),
            "vm firmware reset win (uefi vars, tpm)"
        );
        assert_eq!(
            reset_detail("win", false, true, None),
            "vm firmware reset win (tpm)"
        );
        assert_eq!(
            reset_detail("win", true, false, Some(true)),
            "vm firmware reset win (uefi vars, secure boot on)"
        );
    }
}
