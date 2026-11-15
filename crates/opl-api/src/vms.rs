//! VM 생성·수정·삭제와 동작(start, stop, reboot).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::hotplug;
use opl_types::network::USER_NETWORK;
use opl_types::node::{FAST_HA, HA_PLACEMENT, HOTPLUG, Node, NodeSpec, PLACEMENT, ha_capable};
use opl_types::placement::PlacementRuleSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::security_group::SecurityGroupSpec;
use opl_types::storage::VolumePhase;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{
    HaState, MacAddr, PowerState, RunningHardware, Vm, VmHa, VmPhase, VmSpec, VmStatus,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::cluster::{missing_feature, require_feature};
use crate::error::{ApiError, Json, Query};
use crate::placement::{Assign, hard_violations};
use crate::resources::{CreateRequest, UpdateRequest, check_name, if_match, lookup};
use crate::revision::TASK;
use crate::scheduler::{
    HaCapacity, Inventory, ScheduleError, disk_moves, ha_capacity, node_fits_with, schedule_with,
};
use crate::score::{self, Scorer};
use crate::snapshots::{
    check_no_hotplug_task, check_no_migration, check_no_revert_task, check_no_snapshot_task,
    check_not_busy, check_not_migrating, count_snapshots,
};
use crate::state::AppState;
use crate::tasks::create_task;

/// 원하는 상태도 실제 상태도 꺼져 있다. 삭제와 노드·디스크 변경은 이때만 할 수 있다.
pub(crate) fn is_stopped(vm: &Vm) -> bool {
    vm.spec.power == PowerState::Stopped
        && matches!(vm.status.phase, VmPhase::Stopped | VmPhase::Failed)
}

/// VM `vm`이 볼륨 `volume`(이름)을 쓰는지: 스펙의 디스크이거나, 도는 QEMU에 아직 붙은 디스크(`status.hardware` —
/// 스펙에서 뺐지만 게스트가 아직 내려 주지 않은 것)다.
pub(crate) fn vm_uses_volume(vm: &Vm, volume: &str) -> bool {
    vm.spec.disks.iter().any(|d| d.volume == volume)
        || vm
            .status
            .hardware
            .as_ref()
            .is_some_and(|hw| hw.disks.iter().any(|d| d.volume == volume))
}

/// 볼륨 `volume`을 VM `vm`이 써서 다룰 수 없다(409 `volume_in_use`). 스펙의 디스크면 `attached` 문구, 떼는 중인
/// 디스크면 `vm hotplug`나 재시작을 알린다.
pub(crate) fn volume_in_use(vm: &Vm, volume: &str, attached: String) -> ApiError {
    let message = if vm.spec.disks.iter().any(|d| d.volume == volume) {
        attached
    } else {
        format!(
            "volume {volume:?} is still being released by VM {:?}; run `vm hotplug` or restart the VM",
            vm.name
        )
    };
    ApiError::conflict("volume_in_use", message)
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

/// hard 배치 규칙 때문에 놓을 노드가 없으면 409 `placement_rule`(`details.rules`에 어긴 규칙 이름)
pub(crate) fn placement_rule(message: String, rules: Vec<String>) -> ApiError {
    let mut e = ApiError::conflict("placement_rule", message);
    e.details = Some(serde_json::json!({ "rules": rules }));
    e
}

/// 스케줄러가 노드를 고르지 못함: 노드 조건이면 409 `no_node`, hard 규칙이면 409 `placement_rule`
fn schedule_error(e: ScheduleError) -> ApiError {
    let message = e.to_string();
    match e {
        ScheduleError::NoNode => ApiError::conflict("no_node", message),
        ScheduleError::PlacementRule(rules) => placement_rule(message, rules),
    }
}

/// HA 용량 예약을 넘으면 409 `ha_capacity`(`details`에 MiB 단위 셈)
fn ha_capacity_error(vm: &Vm, c: HaCapacity) -> ApiError {
    let free = c.capacity_mib.saturating_sub(c.reserve_mib + c.used_mib);
    let mut e = ApiError::conflict(
        "ha_capacity",
        format!(
            "starting HA VM {:?} ({} MiB) would use capacity reserved for HA recovery: {free} MiB left of {} MiB ({} MiB reserved, {} MiB used)",
            vm.name, c.need_mib, c.capacity_mib, c.reserve_mib, c.used_mib
        ),
    );
    e.details = Some(serde_json::json!({
        "capacity_mib": c.capacity_mib,
        "reserve_mib": c.reserve_mib,
        "used_mib": c.used_mib,
        "need_mib": c.need_mib,
    }));
    e
}

/// 이 VM을 켜도 HA 용량 예약이 남는지 본다(아니면 409 `ha_capacity`).
fn check_ha_capacity(state: &AppState, inv: &Inventory, vm: &Vm) -> Result<(), ApiError> {
    let settings = crate::settings::current(&state.cluster)?.spec;
    ha_capacity(inv, &settings, vm).map_err(|c| ha_capacity_error(vm, c))
}

/// 저장하기 전에 VM의 spec을 검증하고 채운다: 빠진 MAC 할당, 참조 확인, 필요하면 노드 배치.
/// `vm`은 저장할 모양이다(id·이름·레이블은 배치 규칙에 쓰고, 자기 자신의 MAC과 디스크는 충돌로 보지 않는다).
/// 노드를 고를 때 `scorer`의 부하 점수를 쓴다.
pub(crate) fn prepare(state: &AppState, vm: &mut Vm, scorer: &Scorer) -> Result<(), ApiError> {
    let id = vm.id;
    let spec = &mut vm.spec;
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
    // 핫플러그 상한은 준 그대로 둔다(지금 값과 같아도): 상한까지 늘린 VM이 상한을 잃으면 나중에 줄일 때 상한이
    // 바뀐 것이 되어 다시 켜야 한다.
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // 핫플러그 상한은 옛 노드가 풀지 못한다.
    if spec.cpu.max_vcpus.is_some() || spec.max_memory_mib.is_some() {
        require_feature(&state.cluster, HOTPLUG)?;
    }
    // 기본과 다른 HA 정책은 객체로 저장되어 옛 노드가 VM을 읽지 못한다.
    if spec.ha.as_ref().is_some_and(|ha| *ha != VmHa::default()) {
        require_feature(&state.cluster, HA_PLACEMENT)?;
    }
    // 우선순위·게스트 감시는 계획 50 노드도 풀지 못한다.
    if spec.ha.as_ref().is_some_and(VmHa::uses_fast_ha) {
        require_feature(&state.cluster, FAST_HA)?;
    }
    // 고정하지 않음(`pinned: false`)은 쓰지 않아 옛 VM 모양 그대로 둔다. 고정은 옛 노드가 풀지 못한다.
    spec.placement = spec.placement.filter(|p| p.pinned);
    if spec.placement.is_some() {
        require_feature(&state.cluster, PLACEMENT)?;
    }
    if let Some(group) = spec.ha.as_ref().and_then(|ha| ha.group.as_ref())
        && !inv.groups.iter().any(|g| &g.name == group)
    {
        return Err(ApiError::bad_request(format!(
            "HA group {group:?} not found"
        )));
    }

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
        if let Some(other) = others.clone().find(|v| vm_uses_volume(v, &disk.volume)) {
            return Err(volume_in_use(
                other,
                &disk.volume,
                format!(
                    "volume {:?} is already attached to VM {:?}",
                    disk.volume, other.name
                ),
            ));
        }
    }
    // 이미 그 노드에 있는 VM(실행 중이거나 고정된 채 꺼진 VM)의 다른 고치기는 유지보수로 막지 않는다.
    let current = inv.vms.iter().find(|v| v.id == id);
    if let Some(node_id) = vm.spec.node {
        let Some(node) = inv.nodes.iter().find(|n| n.id == node_id) else {
            return Err(ApiError::bad_request(format!("node {node_id} not found")));
        };
        check_not_witness(node)?;
        if current.and_then(|v| v.spec.node) != Some(node_id) {
            check_not_in_maintenance(node)?;
        }
        check_ha_capable(node, &vm.spec)?;
        check_node_arch(node, &vm.spec)?;
        check_firewall(node, &vm.spec)?;
        // 켜진 채 노드를 준 요청은 hard 배치 규칙도 지켜야 한다. 이미 그 노드에 자리 잡은 VM(실행 중 고치기)은
        // 새 규칙을 어겨도 옮기거나 막지 않는다(위반으로 보인다).
        let placed = current
            .is_some_and(|v| v.spec.power == PowerState::Running && v.spec.node == Some(node_id));
        if vm.spec.power == PowerState::Running && !placed {
            let broken = hard_violations(vm, node, &inv, &Assign::default());
            if !broken.is_empty() {
                return Err(placement_rule(
                    format!(
                        "VM {:?} on node {:?} breaks placement rules: {}",
                        vm.name,
                        node.name,
                        broken.join(", ")
                    ),
                    broken,
                ));
            }
        }
    }
    if vm.spec.power == PowerState::Running && vm.spec.node.is_none() {
        vm.spec.node = Some(schedule_with(vm, &inv, scorer).map_err(schedule_error)?);
    }
    // 켜진 채 만들거나, 켜진 채로 바꾸거나 메모리를 늘리면 HA 용량 예약을 본다. 이미 켜진 HA VM의 다른
    // 고치기는 예약이 넘친 때(노드 장애 뒤)에도 막지 않는다.
    let started = |v: &Vm| {
        v.spec.power == PowerState::Running
            && v.spec
                .ha
                .as_ref()
                .is_some_and(|ha| ha.state == HaState::Started)
    };
    let adds = current.is_none_or(|c| !started(c) || c.spec.memory_mib < vm.spec.memory_mib);
    if started(vm) && adds {
        check_ha_capacity(state, &inv, vm)?;
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
    if spec.ha_active() && !ha_capable(node) {
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

/// 시작이 노드를 고를 수 있다: 노드가 없거나, 그 노드가 유지보수 중이거나 hard 배치 규칙을 어긴다(아래 시작의
/// 닫힘과 같은 조건 — 잠그기 전에 보므로 그사이 바뀌면 부하 없이 고른다). 그때만 점수용 부하를 읽는다.
fn start_picks(state: &AppState, vm: &Vm) -> Result<bool, ApiError> {
    let Some(node) = vm.spec.node else {
        return Ok(true);
    };
    let Some(node) = state.cluster.get::<NodeSpec>(node)? else {
        return Ok(false);
    };
    if node.spec.maintenance {
        return Ok(true);
    }
    let inv = Inventory::load(&state.cluster)?;
    Ok(!hard_violations(vm, &node, &inv, &Assign::default()).is_empty())
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
    // 부하는 잠그기 전에 읽는다(리더가 아니면 리더에 물을 수 있다). 켜진 채 노드 없이 만들 때만 노드를 고른다.
    let picks = vm.spec.power == PowerState::Running && vm.spec.node.is_none();
    let scoring = score::scorer(&state, picks).await;
    let _lock = state.vm_lock.lock().await;
    prepare(&state, &mut vm, &scoring.scorer())?;
    vm.revision = state.cluster.create(&vm).await?;
    Ok((StatusCode::CREATED, Json(vm)))
}

/// spec 전체를 바꾼다. 실행 중인 VM의 `node`는 바꿀 수 없고 `ha`를 켜거나 `guest_watch`를 켜고 끌 수 없다(409
/// `vm_running`). NIC의 `security_groups`는 곧바로 적용된다.
///
/// 모든 노드가 `hotplug`를 알면 실행 중인 VM의 하드웨어(CPU·메모리·상한·펌웨어·머신·디스크·NIC) 변경은 핫플러그
/// Task(`vm-hotplug`)를 만들고 그 id를 응답 헤더 `orbivisor-task`로 준다. 상한 안의 vCPU(`cpu.cores`)·메모리, 끝에
/// 붙이는 virtio-blk 디스크·NIC, 실행 중 붙인 것을 끝에서 떼기는 바로 하고, 나머지는 `status.hardware.pending`(다음
/// 재시작 때 적용)에 남는다. 부팅 때 단 디스크·NIC 떼기, 가운데 떼기, 순서·버스·MAC·모델·네트워크 바꾸기, SATA 디스크
/// 붙이기는 409 `vm_running`, 핫플러그 없이 켠 VM이나 빈 루트 포트가 없을 때의 디스크·NIC 붙이기·떼기는 409
/// `hotplug_unavailable`, 켜거나 끄는 중이거나 앞의 핫플러그 Task가 끝나지 않았으면 409 `vm_busy`, 메모리를 바로
/// 늘릴 미할당 메모리가 노드에 없으면 409 `no_capacity`다. 같은 PUT으로 끄는(`power: stopped`) VM의 하드웨어 변경은
/// Task 없이 다음 시작부터 적용된다. 기능이 모자라거나 켜져 있어야 하지만 QEMU가 없는 VM(실패, 다시 켜기를
/// 기다림)은 실행 중에 디스크와 NIC 개수·순서·MAC을 바꿀 수 없고(보안 그룹을 건 NIC가 있으면 `security_groups`만)
/// 다른 변경은 다음 시작부터 적용된다. `If-Match`가 없으면 검증한 revision에 쓰고, 그
/// 사이 바뀌었으면 다시 읽어 검증한다.
#[utoipa::path(put, path = "/vms/{key}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<VmSpec>,
    responses(
        (status = 200, body = Resource<VmSpec, VmStatus>, headers(
            ("orbivisor-task" = String, description = "id of the vm-hotplug task that applies a hardware change to the running VM (only when one was created)"),
        )),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn update_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<VmSpec>>,
) -> Result<(HeaderMap, Json<Vm>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    let expected = if_match(&headers)?;
    let picks = req.spec.power == PowerState::Running && req.spec.node.is_none();
    let scoring = score::scorer(&state, picks).await;
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
        let mut next = current.clone();
        next.spec = req.spec.clone();
        next.labels = req.labels.clone();
        prepare(&state, &mut next, &scoring.scorer())?;
        let spec = next.spec;
        if spec.node != current.spec.node && !is_stopped(&current) {
            return Err(vm_running(&current, "move"));
        }
        // 모든 노드가 `hotplug`를 알면 실행 중인 VM의 하드웨어 변경을 핫플러그 Task로 맞춘다. 같은 PUT으로 끄면
        // 다음 시작부터 적용하는 변경이다. 켜져 있어야 하지만 QEMU가 없고 전원 Task도 없는 VM(실패, 다시 켜기를
        // 기다림)은 지금 규칙대로 받는다(CPU·메모리는 다음 시작부터, 디스크·NIC 순서는 그대로).
        let hot = !is_stopped(&current) && missing_feature(&state.cluster, HOTPLUG)?.is_empty();
        let stopping = spec.power == PowerState::Stopped;
        let idle = current.status.hardware.is_none()
            && matches!(current.status.phase, VmPhase::Stopped | VmPhase::Failed)
            && !power_task_running(&state, &current)?;
        let changed = hotplug::hardware_changed(&current.spec, &spec);
        let hotplug = hot && !stopping && !idle && changed;
        if hotplug {
            check_hot_change(&state, &current, &spec)?;
        } else if !(is_stopped(&current) || hot && stopping) {
            check_running_devices(&current, &spec)?;
        }
        // 끄면서 바꾸는 하드웨어도 끝나지 않은 핫플러그 Task의 되돌리기가 지우지 않게 기다린다.
        if hot && stopping && changed {
            check_no_hotplug_task(&state, &current)?;
        }
        if spec.power == PowerState::Running && current.spec.power == PowerState::Stopped {
            check_stopping_devices(&current, &spec)?;
        }
        // 노드가 과반수와 끊긴 동안 켜면 그 노드는 HA인 줄 모르고 펜싱하지 않는다. 끄기는 언제든.
        if spec.ha_active() && !current.spec.ha_active() && !is_stopped(&current) {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot turn on HA for VM {:?} while it is running; turn on HA while the VM is stopped",
                    current.name
                ),
            ));
        }
        // 게스트 에이전트 채널은 `guest_watch`가 있는 VM에만 다는 장치다. 실행 중에 켜거나 끄면 도는 QEMU와
        // 어긋나 라이브 마이그레이션(비우기·종료·되돌아가기·옮기기)이 장치 차이로 실패한다. 시간만 바꾸기는 된다.
        let watched = |s: &VmSpec| s.ha.as_ref().is_some_and(|ha| ha.guest_watch.is_some());
        if watched(&spec) != watched(&current.spec) && !is_stopped(&current) {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot turn guest_watch on or off for VM {:?} while it is running; guest_watch can be turned on or off only while the VM is stopped",
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
        // 같은 잠금 안에서 Task를 만들어 그사이 마이그레이션이 시작하지 않게 한다.
        let mut response = HeaderMap::new();
        if hotplug {
            // 스펙은 이미 썼다. Task를 만들지 못하면 헤더 없이 200이고 변경은 `vm hotplug`로 다시 맞출 수 있다.
            match start_hotplug(&state, &current).await {
                Ok(task) => {
                    response.insert(
                        TASK,
                        HeaderValue::from_str(&task.id.to_string()).expect("uuid"),
                    );
                }
                Err(e) => {
                    tracing::warn!(vm = %current.id, error = %e.message, "cannot start the hotplug task after updating the spec; run `vm hotplug` to apply it");
                }
            }
        }
        let vm = lookup::<VmSpec>(&state.cluster, &current.id.to_string())?;
        return Ok((response, Json(vm)));
    }
    Err(ApiError::conflict(
        "busy",
        "the VM kept changing while being updated; try again",
    ))
}

/// 이 VM의 전원 Task(시작·정지·강제 정지·재시작)가 끝나지 않았는지(켜는 중이라 곧 QEMU가 생긴다)
fn power_task_running(state: &AppState, vm: &Vm) -> Result<bool, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == vm.id
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                TaskOp::VmStart | TaskOp::VmStop | TaskOp::VmForceStop | TaskOp::VmReboot
            )
    }))
}

/// `hotplug` 기능이 모자란 클러스터에서 실행 중인 VM의 디스크·NIC 변경을 거절한다(409 `vm_running`).
fn check_running_devices(current: &Vm, spec: &VmSpec) -> Result<(), ApiError> {
    if spec.disks != current.spec.disks {
        return Err(vm_running(current, "change the disks of"));
    }
    // 방화벽 규칙은 spec의 NIC 순번·MAC으로 tap을 정한다. 실행 중에 NIC 구성이 바뀌면 도는 tap과
    // 어긋나 막히지 않은 채가 되므로, 방화벽 NIC가 있으면(전이나 뒤) 그룹만 바꿀 수 있다.
    let firewalled = |s: &VmSpec| s.nics.iter().any(|n| !n.security_groups.is_empty());
    let without_groups = |s: &VmSpec| {
        let mut nics = s.nics.clone();
        nics.iter_mut().for_each(|n| n.security_groups.clear());
        nics
    };
    let only_groups = || {
        ApiError::conflict(
            "vm_running",
            format!(
                "cannot change the NICs of VM {:?} while it is running; only security groups can change",
                current.name
            ),
        )
    };
    if (firewalled(spec) || firewalled(&current.spec))
        && without_groups(spec) != without_groups(&current.spec)
    {
        return Err(only_groups());
    }
    // 방화벽이 없어도 NIC 개수·순서·MAC은 그대로 둔다. 그룹 없이 바꾼 뒤 그룹을 걸면 위 검사를 비켜
    // 규칙의 tap이 도는 tap과 어긋난다.
    let macs = |s: &VmSpec| s.nics.iter().map(|n| n.mac).collect::<Vec<_>>();
    if macs(spec) != macs(&current.spec) {
        return Err(vm_running(
            current,
            "change the number, order or MAC addresses of the NICs of",
        ));
    }
    // 도는 하드웨어 기록이 있으면(마이그레이션 대상이 기록의 NIC로 켠다) 그룹이 없어도 네트워크·모델을 바꾸지 않는다.
    if current.status.hardware.is_some() && without_groups(spec) != without_groups(&current.spec) {
        return Err(only_groups());
    }
    Ok(())
}

/// 실행 중인 VM `vm`의 하드웨어를 `spec`으로 바꾸는 핫플러그를 받을 수 있는지 본다. 도는 하드웨어 기록이 있는 실행
/// 중 VM이어야 하고 전원 Task와 끝나지 않은 핫플러그 Task가 없어야 하며(409 `vm_busy` — 실패한 Task의 되돌리기가 이
/// 변경을 지우지 않게), 디스크를 바꾸면 크기 늘리기 Task도 없어야 한다. 디스크·NIC
/// 변경은 끝에 붙이기와 실행 중 붙인 것 떼기만 된다([`hotplug::plan`]). 메모리를 바로 늘리면 VM이 도는 노드에 그만큼
/// 미할당 메모리가 있어야 한다(409 `no_capacity`).
fn check_hot_change(state: &AppState, vm: &Vm, spec: &VmSpec) -> Result<(), ApiError> {
    let hw =
        vm.status.hardware.as_ref().filter(|_| {
            vm.spec.power == PowerState::Running && vm.status.phase == VmPhase::Running
        });
    let Some(hw) = hw else {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is starting or stopping; change its hardware when it is running or stopped",
                vm.name
            ),
        ));
    };
    check_not_busy(state, vm)?;
    check_no_hotplug_task(state, vm)?;
    // 게스트가 아직 내려 주지 않은 디스크·NIC가 있으면 디스크·NIC 목록은 바꾸지 않는다(떼기를 먼저 마친다).
    let devices = |s: &VmSpec| {
        let mut nics = s.nics.clone();
        nics.iter_mut().for_each(|n| n.security_groups.clear());
        (s.disks.clone(), nics)
    };
    let releasing: Vec<String> = hw
        .unplugging
        .iter()
        .filter(|d| !d.starts_with("cpu"))
        .map(|d| hotplug::device_name(hw, d))
        .collect();
    if !releasing.is_empty() && devices(spec) != devices(&vm.spec) {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "the guest is still releasing {} from VM {:?}; run `vm hotplug` first",
                releasing.join(", "),
                vm.name
            ),
        ));
    }
    let inv = Inventory::load(&state.cluster)?;
    if spec.disks != vm.spec.disks {
        check_no_resize_task(state, &inv, vm)?;
    }
    let node = vm
        .status
        .node
        .and_then(|id| inv.nodes.iter().find(|n| n.id == id));
    let caps = node.and_then(|n| n.status.hotplug).unwrap_or_default();
    let plan = hotplug::plan(spec, hw, caps).map_err(|e| device_error(vm, hw, e))?;
    check_hotplug_memory(&inv, vm, hw, node, &plan)
}

/// 끄는 중인 VM(QEMU가 아직 돌아 기록이 있음)을 다시 켤 때 스펙 `spec`의 디스크·NIC가 도는 장치와 다르면 409
/// `vm_busy`다: 작업자는 도는 QEMU를 그대로 이어 쓰므로 스펙과 장치가 어긋난다. 꺼진 뒤 켜면 스펙대로 켠다.
fn check_stopping_devices(vm: &Vm, spec: &VmSpec) -> Result<(), ApiError> {
    match &vm.status.hardware {
        Some(hw) if !hotplug::devices_match(spec, hw) => Err(ApiError::conflict(
            "vm_busy",
            format!(
                "VM {:?} is stopping with changed devices; start it after it stops",
                vm.name
            ),
        )),
        _ => Ok(()),
    }
}

/// 실행 중에 할 수 없는 디스크·NIC 변경의 409
fn device_error(vm: &Vm, hw: &RunningHardware, e: hotplug::DeviceError) -> ApiError {
    use hotplug::DeviceError;
    match e {
        DeviceError::Changed(what) => ApiError::conflict(
            "vm_running",
            format!(
                "cannot {what} of VM {:?} while it is running; only disks and NICs added at the end can be removed while it runs; stop it first",
                vm.name
            ),
        ),
        DeviceError::SataDisk(volume) => ApiError::conflict(
            "vm_running",
            format!(
                "SATA disks cannot be added while the VM is running; add disk {volume:?} to VM {:?} as virtio-blk or stop it first",
                vm.name
            ),
        ),
        DeviceError::NoLayout => ApiError::conflict(
            "hotplug_unavailable",
            format!(
                "VM {:?} was started without hotplug support; restart it once to add or remove disks and NICs while it runs",
                vm.name
            ),
        ),
        DeviceError::NoFreePort => ApiError::conflict(
            "hotplug_unavailable",
            format!(
                "no free hotplug port ({} in use); restart VM {:?} to add more disks or NICs while it runs",
                hw.layout.as_ref().map_or(0, |l| l.ports),
                vm.name
            ),
        ),
    }
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
/// `stop?mode=force`와 `reboot`은 대상 노드의 Task 실행기가 수행한다. `hotplug`는 실행 중인 VM의 하드웨어를
/// 지금 스펙에 맞추는 Task(`vm-hotplug`, 실패한 변경 다시 해 보기)다: 모든 노드가 `hotplug`를 알아야 하고(409
/// `cluster_upgrading`) 핫플러그 기록이 있는 실행 중 VM이어야 한다(409 `vm_not_running`). 끝나지 않은 스냅샷·되돌리기·
/// 복제·백업 Task가 있으면 409 `vm_busy`이고, 메모리를 바로 늘리면 VM이 도는 노드에 그만큼 미할당 메모리가 있어야 한다
/// (409 `no_capacity`).
#[utoipa::path(post, path = "/vms/{key}/actions/{action}", tag = "vms",
    params(("key" = String, Path, description = "id or name"), ("action" = String, Path, description = "start, stop, reboot or hotplug"), ActionQuery),
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
    if action == "hotplug" {
        // 마이그레이션 검사와 Task 만들기 사이에 마이그레이션이 시작하지 않게 한다(`start_migration`과 같은 잠금).
        let _lock = state.vm_lock.lock().await;
        let vm = lookup::<VmSpec>(&state.cluster, &vm.id.to_string())?;
        check_no_migration(&state, &vm)?;
        check_no_snapshot_task(&state, &vm)?;
        check_hotplug_room(&state, &vm)?;
        let task = start_hotplug(&state, &vm).await?;
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    if query.mode == StopMode::Force {
        check_no_revert_task(&state, &vm)?;
    } else {
        check_no_snapshot_task(&state, &vm)?;
    }
    let id = vm.id;
    let (op, vm) = match action.as_str() {
        "start" => {
            let scoring = score::scorer(&state, start_picks(&state, &vm)?).await;
            let scorer = scoring.scorer();
            let _lock = state.vm_lock.lock().await;
            let vm = modify_spec(&state, id, |spec| {
                // 배치 규칙은 VM의 이름·레이블로 고른다.
                let mut me = vm.clone();
                me.spec = spec.clone();
                // 고정된 노드는 VM을 고칠 때 확인했지만, 그때 arch를 모르던 노드일 수 있다.
                // 유지보수 노드나 hard 배치 규칙을 어기는 노드에 고정된 꺼진 VM은 다른 노드를 고른다.
                // 아직 그 노드에서 돌거나 멈추는 중인 VM은 옮기지 않는다.
                if let Some(node) = spec.node
                    && let Some(node) = state.cluster.get::<NodeSpec>(node)?
                {
                    if node.spec.maintenance && stopped_now(&state, id)? {
                        let inv = Inventory::load(&state.cluster)?;
                        let other = schedule_with(&me, &inv, &scorer).map_err(|e| match e {
                            ScheduleError::NoNode => ApiError::conflict(
                                "node_unavailable",
                                format!(
                                    "node {:?} is in maintenance and no other node can run this VM; end maintenance first",
                                    node.name
                                ),
                            ),
                            e => schedule_error(e),
                        })?;
                        spec.node = Some(other);
                    } else {
                        check_node_arch(&node, spec)?;
                        let inv = Inventory::load(&state.cluster)?;
                        let broken = hard_violations(&me, &node, &inv, &Assign::default());
                        if !broken.is_empty() && stopped_now(&state, id)? {
                            let other = schedule_with(&me, &inv, &scorer).map_err(|e| {
                                let rules = match e {
                                    ScheduleError::PlacementRule(rules) => rules,
                                    ScheduleError::NoNode => broken.clone(),
                                };
                                placement_rule(
                                    format!(
                                        "node {:?} breaks placement rules of VM {:?} and no other node can run it: {}",
                                        node.name,
                                        me.name,
                                        rules.join(", ")
                                    ),
                                    rules,
                                )
                            })?;
                            spec.node = Some(other);
                        }
                    }
                }
                if spec.node.is_none() {
                    let inv = Inventory::load(&state.cluster)?;
                    spec.node = Some(schedule_with(&me, &inv, &scorer).map_err(schedule_error)?);
                }
                // 끄는 중에 디스크·NIC를 바꾼 VM은 QEMU가 끝난 뒤 켠다.
                if spec.power != PowerState::Running
                    && let Some(now) = state.cluster.get::<VmSpec>(id)?
                {
                    check_stopping_devices(&now, spec)?;
                }
                // 이미 켜기로 된 VM은 자리를 더 쓰지 않는다.
                if spec.power != PowerState::Running {
                    me.spec = spec.clone();
                    check_ha_capacity(&state, &Inventory::load(&state.cluster)?, &me)?;
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
                "unknown action {action:?} (start, stop, reboot, hotplug)"
            )));
        }
    };
    // 배치되지 않은 VM은 돌고 있지 않다. 정지 Task는 이 노드에서 곧바로 끝난다.
    let node = vm.spec.node.unwrap_or(state.node);
    let task = create_task(&state.cluster, op, vm.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 실행 중인 VM `vm`의 하드웨어를 지금 스펙에 맞출 핫플러그 Task(`vm-hotplug`)를 VM이 도는 노드에 만든다. `vm_lock`
/// 안에서 부른다. 모든 노드가 `hotplug`를 알아야 하고(409 `cluster_upgrading`), 핫플러그 기록(`status.hardware`)이
/// 있는 실행 중 VM이어야 한다(409 `vm_not_running`).
pub(crate) async fn start_hotplug(state: &AppState, vm: &Vm) -> Result<Task, ApiError> {
    require_feature(&state.cluster, HOTPLUG)?;
    let running = vm.spec.power == PowerState::Running
        && vm.status.phase == VmPhase::Running
        && vm.status.hardware.is_some();
    let Some(node) = vm.status.node.filter(|_| running) else {
        return Err(ApiError::conflict(
            "vm_not_running",
            format!("VM {:?} is not running", vm.name),
        ));
    };
    create_task(&state.cluster, TaskOp::VmHotplug, vm.id, node).await
}

/// 실행 중인 VM `vm`을 지금 스펙에 맞추는 계획이 메모리를 바로 늘리면 VM이 도는 노드에 그만큼 미할당 메모리가 있는지
/// 본다(409 `no_capacity`). 기록이 없거나 계획할 수 없으면(Task가 실패로 알린다) 보지 않는다.
fn check_hotplug_room(state: &AppState, vm: &Vm) -> Result<(), ApiError> {
    let Some(hw) = &vm.status.hardware else {
        return Ok(());
    };
    let inv = Inventory::load(&state.cluster)?;
    let node = vm
        .status
        .node
        .and_then(|id| inv.nodes.iter().find(|n| n.id == id));
    let caps = node.and_then(|n| n.status.hotplug).unwrap_or_default();
    match hotplug::plan(&vm.spec, hw, caps) {
        Ok(plan) => check_hotplug_memory(&inv, vm, hw, node, &plan),
        Err(_) => Ok(()),
    }
}

/// 핫플러그 계획 `plan`이 메모리를 바로 늘리면 VM이 도는 노드 `node`에 늘리는 만큼 미할당 메모리가 있어야 한다(409
/// `no_capacity`).
fn check_hotplug_memory(
    inv: &Inventory,
    vm: &Vm,
    hw: &opl_types::vm::RunningHardware,
    node: Option<&Node>,
    plan: &hotplug::Plan,
) -> Result<(), ApiError> {
    let plugged = plan.steps.iter().find_map(|s| match s {
        hotplug::Step::SetMemory { plugged_mib } => Some(*plugged_mib),
        _ => None,
    });
    if let (Some(plugged), Some(node)) = (plugged, node) {
        // 다른 VM에 배정되지 않은 메모리에서 이 VM이 지금 도는 만큼을 뺀 것이 늘릴 수 있는 양이다(스펙은 이미 새 값일
        // 수 있다 — 다시 적용).
        let now = hw.memory_now();
        let want = hw.memory_mib + plugged;
        let free = inv.free_memory_mib(node, vm.id) - i64::from(now);
        if want > now && free < i64::from(want - now) {
            return Err(ApiError::conflict(
                "no_capacity",
                format!(
                    "node {:?} has only {} MiB of unallocated memory; VM {:?} needs {} MiB more",
                    node.name,
                    free.max(0),
                    vm.name,
                    want - now
                ),
            ));
        }
    }
    Ok(())
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
    /// 노드 로컬 풀의 디스크를 복사해 둘 대상 노드의 풀. 없으면 디스크마다 같은 이름의 풀이다.
    #[serde(default)]
    pub pool: Option<String>,
    /// hard 배치 규칙(restricted HA 그룹 포함)을 넘어 옮긴다. 노드 조건과 메모리는 그대로 본다. 배치 규칙을
    /// 쓸 수 있는 사용자(관리자)만이다(아니면 403). 넘은 규칙은 감사 기록에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 마이그레이션이 배치 규칙을 보는 방법
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleCheck {
    /// 대상 노드가 hard 규칙을 어기면 409 `placement_rule`(자리 잡은 hard affinity 상대가 있는 VM 하나만 옮기기 포함)
    Enforce,
    /// 규칙을 넘는다(관리자의 `force`).
    Force,
    /// 이 VM들(affinity 묶음)도 대상 노드로 함께 옮긴다고 보고 hard 규칙을 본다(비우기). 중간의 잠깐 위반은 받아들인다.
    Group(Vec<Uuid>),
}

/// 실행 중인 VM을 다른 노드로 라이브 마이그레이션할 Task를 돌려준다(202). Task는 지금 노드의 실행기가
/// 한다. 노드 로컬 풀의 디스크는 대상 노드의 같은 이름의 풀(`pool`을 주면 그 풀)에 복사한다. 그런 디스크가
/// 있으면 모든 노드가 디스크 복사를 알아야 하고(409 `cluster_upgrading`) VM에 스냅샷이 없어야 한다(409
/// `migration_unsupported`). `pool`을 줬는데 그런 디스크가 없으면 400, 없는 풀이면 404다. HVF(macOS)
/// 노드는 원본도 대상도 될 수 없다(409 `migration_unsupported`). 핫플러그 배치로 도는 VM은 대상이 같은 장치로 켤 수
/// 있어야 하고(`hotplug` 기능, vCPU 핫플러그, virtio-mem — 아니면 409 `migration_unsupported`), 핫플러그 Task가 끝나지
/// 않았거나 스펙의 디스크·NIC가 도는 장치와 다르면 409 `vm_busy`다. 대상 노드가 위트니스이거나 오프라인이거나
/// 유지보수 중이거나 호스트 종료 중이면 409 `node_unavailable`, HA VM인데 대상이 HA 가능 노드가 아니면 409 `ha_unsupported`,
/// 스케줄러가 고를 수 없는 노드(풀, 대상 풀, 네트워크, 남은 메모리)면 409 `no_capacity`다. 대상 노드가 hard
/// 배치 규칙을 어기면(자리 잡은 hard affinity 상대가 있는 VM 하나만 옮기기 포함) 409 `placement_rule`
/// (`details.rules`)이고, 관리자는 `force`로 넘을 수 있다(감사 detail `force rules=<이름들>`).
#[utoipa::path(post, path = "/vms/{key}/migrate", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    request_body = MigrateRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn migrate_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<MigrateRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Operate, VmSpec::KIND)?;
    // 규칙을 넘는 것은 규칙을 쓸 수 있는 사용자만이다.
    let rules = if req.force {
        state.authorize(&principal, Action::Write, PlacementRuleSpec::KIND)?;
        RuleCheck::Force
    } else {
        RuleCheck::Enforce
    };
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let target = lookup::<NodeSpec>(&state.cluster, &req.node)?;
    let (task, forced) = start_migration(&state, vm.id, target.id, req.pool, None, rules).await?;
    if req.force {
        audit.detail(format!("force rules={}", forced.join(",")));
    }
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// [`migrate_vm`]의 확인을 하고 VM `vm`을 노드 `target`(로컬 디스크는 그 노드의 풀 `pool`)으로 옮길
/// `VmMigrate` Task를 원본 노드에 만든다. 노드 비우기 Task도 쓴다. 오류는 [`migrate_vm`]과 같다. `source`가
/// 있으면(비우기) VM이 아직 그 노드에 있는지도 보고, 아니면 409 `vm_moved`다(이미 다른 데로 갔다). 배치 규칙은
/// `rules`대로 보고, Task와 함께 대상 노드에서 어기는 hard 규칙(넘은 규칙, `Force`일 때만 빌 수 없음)을 돌려준다.
pub(crate) async fn start_migration(
    state: &AppState,
    vm: Uuid,
    target: Uuid,
    pool: Option<String>,
    source: Option<Uuid>,
    rules: RuleCheck,
) -> Result<(Task, Vec<String>), ApiError> {
    // 두 요청이 함께 검사를 지나 Task를 둘 만들지 않게 한다.
    let _lock = state.vm_lock.lock().await;
    let vm = lookup::<VmSpec>(&state.cluster, &vm.to_string())?;
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
    // HA 검사(`restart_elsewhere`)가 원본 노드에 끄라고 한 VM: 옮기는 동안 꺼지면 어긋난다.
    if vm.status.ha.as_ref().is_some_and(|ha| ha.evict.is_some()) {
        return Err(ApiError::conflict(
            "vm_busy",
            "the VM is being restarted elsewhere by an HA check",
        ));
    }
    check_no_snapshot_task(state, &vm)?;
    // 대상은 원본이 도는 하드웨어로 켜므로 장치가 바뀌는 중에는 옮기지 않는다.
    check_no_hotplug_task(state, &vm)?;
    // 대상은 스펙의 디스크·NIC로 켠다. 원본 QEMU의 장치와 다르면(실패한 핫플러그, 게스트가 아직 내려 주지 않은
    // 장치) 옮기지 않는다.
    if let Some(hw) = &vm.status.hardware
        && !hotplug::devices_match(&vm.spec, hw)
    {
        return Err(ApiError::conflict(
            "vm_busy",
            format!(
                "cannot migrate VM {:?}: the VM's devices do not match the running hardware; run `vm hotplug` or restart it",
                vm.name
            ),
        ));
    }
    let inv = Inventory::load(&state.cluster)?;
    check_no_resize_task(state, &inv, &vm)?;
    // 노드 로컬 풀의 디스크는 대상 노드에 복사한다(같은 이름의 풀 또는 `pool`).
    let mut moves = disk_moves(&vm.spec, &inv, None);
    if let Some(name) = &pool {
        if moves.is_empty() {
            return Err(ApiError::bad_request(format!(
                "VM {:?} has no disks on node-local pools; pool applies only to them",
                vm.name
            )));
        }
        let pool = inv
            .pools
            .iter()
            .find(|p| &p.name == name)
            .ok_or_else(|| ApiError::not_found(format!("storage pool {name:?} not found")))?;
        moves = disk_moves(&vm.spec, &inv, Some(pool));
    }
    if let Some(first) = moves.first() {
        require_feature(&state.cluster, "storage-migration")?;
        if count_snapshots(state, vm.id)? > 0 {
            let volume = inv.volumes.iter().find(|v| v.id == first.volume);
            return Err(ApiError::conflict(
                "migration_unsupported",
                format!(
                    "VM {:?} has snapshots and disk {:?} is on node-local pool {:?}; snapshots are not copied, delete them first",
                    vm.name,
                    volume.map_or("", |v| v.name.as_str()),
                    volume.map_or("", |v| v.spec.pool.as_str()),
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
    if target.status.shutdown.is_some() {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is shutting down", target.name),
        ));
    }
    check_node_arch(&target, &vm.spec)?;
    // 대상은 원본과 같은 장치(핫플러그 배치·붙인 vCPU·virtio-mem)로 켜야 한다.
    hotplug::can_receive(vm.status.hardware.as_ref(), &target).map_err(|e| {
        ApiError::conflict(
            "migration_unsupported",
            format!("cannot migrate VM {:?}: {e}", vm.name),
        )
    })?;
    node_fits_with(&vm.spec, &inv, &target, &moves)
        .map_err(|e| ApiError::conflict("no_capacity", e))?;
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
    // 노드 조건과 메모리가 맞은 뒤의 hard 배치 규칙. 묶음은 대상 노드에 함께 있다고 본다.
    let mut assign = Assign::default();
    if let RuleCheck::Group(members) = &rules {
        for m in members {
            assign.place(*m, target.id);
        }
    }
    let broken = hard_violations(&vm, &target, &inv, &assign);
    if !broken.is_empty() && rules != RuleCheck::Force {
        return Err(placement_rule(
            format!(
                "moving VM {:?} to node {:?} breaks placement rules: {} (VMs kept together by an affinity rule move together only when their node is drained)",
                vm.name,
                target.name,
                broken.join(", ")
            ),
            broken,
        ));
    }
    let op = TaskOp::VmMigrate {
        node: target.id,
        pool,
    };
    let task = create_task(&state.cluster, op, vm.id, source).await?;
    Ok((task, broken))
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
