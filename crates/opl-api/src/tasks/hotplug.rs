//! 핫플러그 Task(계획 54): 실행 중인 VM의 vCPU·메모리·디스크·NIC를 지금 스펙에 맞춘다. 스펙과 도는 하드웨어
//! (`status.hardware`)를 견준 계획([`hotplug::plan`])의 단계를 차례로 드라이버로 하고, 단계마다 곧바로 기록을
//! 고친다. 첫 실패에서 멈추고, 스펙의 디스크·NIC 목록을 도는 장치에 맞게 되돌린다(스펙 순번 = QEMU 장치 순번).
//! 게스트가 시간 안에 내려 주지 않은 떼기는 되돌리지 않고(사용자가 뗀 그대로) 기록의 `unplugging`에 두어 다음
//! Task가 마저 뗀다. 실행 중에 할 수 없는 것과 실패한 것은 다음 재시작 때 적용할 변경(`pending`)으로 남긴다. Task
//! 도중 QEMU가 다시 켜지거나 옮겨지면(노드·pid가 바뀜) 아무것도 되돌리지 않고 멈춘다.

use std::time::Duration;

use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::AddOutcome;
use opl_reconciler::resolve::{runtime_disk, runtime_nic};
use opl_types::hotplug::{self, Step};
use opl_types::node::{NodeHotplug, NodeSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::Task;
use opl_types::vm::{PendingChange, PendingReason, RunningHardware, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::Ctx;

/// 실패 뒤 하지 않은 vCPU·메모리 단계의 보류 설명
const SKIPPED: &str = "not tried because an earlier change failed";
/// QEMU가 vCPU 떼기를 거절했을 때(게스트가 vCPU 떼기를 켜지 않음)의 보류 설명
const VCPU_REFUSED: &str = "the guest does not support vCPU removal yet; restart the VM to apply";
/// 스펙의 장치 목록을 되돌렸을 때 오류 끝에 붙인다.
const UNDONE: &str = "; the change was undone";
/// Task 도중 QEMU가 바뀐 오류
const MOVED: &str =
    "the VM was restarted or moved while its hardware was being changed; nothing was undone";

/// Task를 멈춘 까닭
enum Stop {
    /// 단계가 실패했다(스펙의 장치 목록을 되돌린다).
    Failed(String),
    /// Task 도중 VM이 다시 켜졌거나 옮겨졌다(그 QEMU의 기록이 아니므로 아무것도 되돌리지 않는다).
    Moved,
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Failed(e)
    }
}

/// Task가 다루는 QEMU: 시작할 때 VM이 돌던 노드와 pid. 다르면 다른 QEMU다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Qemu {
    node: Option<Uuid>,
    pid: Option<u32>,
}

impl Qemu {
    fn of(vm: &Vm) -> Self {
        Qemu {
            node: vm.status.node,
            pid: vm.status.pid,
        }
    }
}

/// VM을 지금 스펙에 맞춘다. 성공했지만 다음 재시작 때 적용할 변경이 남으면 그 목록을 경고로 돌려준다.
pub(super) async fn hotplug(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let vm = read_vm(ctx, id)?;
    let running = vm.status.phase == VmPhase::Running && vm.status.node == Some(ctx.config.node);
    let Some(hw) = vm.status.hardware.clone().filter(|_| running) else {
        return Err(format!("VM {:?} is not running on this node", vm.name));
    };
    let caps = node_caps(ctx)?;
    match run(ctx, &vm, Qemu::of(&vm), hw, caps).await {
        Ok(warning) => Ok(warning),
        Err(Stop::Failed(e)) => Err(e),
        Err(Stop::Moved) => Err(format!("VM {:?}: {MOVED}", vm.name)),
    }
}

/// 계획의 단계를 하고 보류를 쓴다.
async fn run(
    ctx: &Ctx,
    vm: &Vm,
    qemu: Qemu,
    mut hw: RunningHardware,
    caps: NodeHotplug,
) -> Result<Option<String>, Stop> {
    let id = vm.id;
    let plan = match hotplug::plan(vm.id, &vm.name, &vm.spec, &hw, caps) {
        Ok(plan) => plan,
        Err(e) => {
            // 스펙이 도는 장치와 실행 중에 맞출 수 없게 어긋났다. 장치 목록을 되돌리고 보류를 다시 셈한다. 함께 바뀐
            // vCPU·메모리는 하지 않았으므로 `failed` 보류로 남긴다(다시 해 보기로 적용).
            let mut e = format!("VM {:?}: {e}", vm.name);
            if undo_devices(ctx, id, qemu, &hw).await? {
                e.push_str(UNDONE);
            }
            let rest = hotplug::plan_resources(vm.id, &vm.name, &vm.spec, &hw, caps).steps;
            write_pending(ctx, id, qemu, caps, &skipped(rest, &hw, &vm.spec)).await?;
            return Err(Stop::Failed(e));
        }
    };
    let mut failed = Vec::new();
    let mut error = None;
    let mut steps = plan.steps.into_iter();
    for step in steps.by_ref() {
        match apply(ctx, vm, qemu, &mut hw, step, &mut failed).await {
            Ok(()) => {}
            Err(Stop::Failed(e)) => {
                error = Some(e);
                break;
            }
            Err(Stop::Moved) => return Err(Stop::Moved),
        }
    }
    if let Some(e) = &mut error {
        // 남은 vCPU·메모리 단계는 하지 않았다(다시 해 보기로 적용). 남은 디스크·NIC 단계는 스펙에서 되돌린다.
        failed.extend(skipped(steps, &hw, &vm.spec));
        if undo_devices(ctx, id, qemu, &hw).await? {
            e.push_str(UNDONE);
        }
    }
    let hw = write_pending(ctx, id, qemu, caps, &failed).await?;
    match error {
        Some(e) => Err(Stop::Failed(e)),
        None if hw.pending.is_empty() => Ok(None),
        None => Ok(Some(pending_warning(&hw.pending))),
    }
}

/// 하지 않은 vCPU·메모리 단계의 `failed` 보류("not tried …")
fn skipped(
    steps: impl IntoIterator<Item = Step>,
    hw: &RunningHardware,
    spec: &VmSpec,
) -> Vec<PendingChange> {
    steps
        .into_iter()
        .filter_map(|step| match step {
            Step::SetVcpus(n) => Some(failure(
                "vcpus",
                hw.vcpus().to_string(),
                n.to_string(),
                SKIPPED.into(),
            )),
            Step::SetMemory { .. } => Some(failure(
                "memory_mib",
                hw.memory_now().to_string(),
                spec.memory_mib.to_string(),
                SKIPPED.into(),
            )),
            _ => None,
        })
        .collect()
}

/// Task 끝의 보류를 쓴다: 지금 셈한 것에 실패한 것(`failed`)과 게스트가 아직 내려 주지 않은 장치를 더한다.
async fn write_pending(
    ctx: &Ctx,
    id: Uuid,
    qemu: Qemu,
    caps: NodeHotplug,
    failed: &[PendingChange],
) -> Result<RunningHardware, Stop> {
    update_hardware(ctx, id, qemu, |hw, vm| {
        let mut failed = failed.to_vec();
        failed.extend(still_unplugging(hw));
        hw.pending = hotplug::pending(vm.id, &vm.name, &vm.spec, hw, caps, &failed);
    })
    .await
}

/// 단계 하나를 하고 `hw`(저장한 기록)를 고친다. vCPU·메모리가 목표에 닿지 못하면 `failed`에 더한다. 드라이버를
/// 부르기 전마다 Task를 시작할 때의 QEMU인지 본다.
async fn apply(
    ctx: &Ctx,
    vm: &Vm,
    qemu: Qemu,
    hw: &mut RunningHardware,
    step: Step,
    failed: &mut Vec<PendingChange>,
) -> Result<(), Stop> {
    let id = vm.id;
    let timeout = ctx.config.hotplug_timeout;
    same_qemu(ctx, id, qemu)?;
    match step {
        Step::SetVcpus(to) => {
            let from = u16::try_from(hw.vcpus()).unwrap_or(u16::MAX);
            let detail = match ctx.hv.set_vcpus(id, &hw.cpu, from, to, timeout).await {
                Ok(change) => {
                    let reached = change.reached;
                    // 게스트가 내려 주지 않은 vCPU(목표 위에 남은 것)는 `unplugging`에 둔다(마이그레이션을 막고 다음
                    // Task가 마저 뗀다). QEMU가 떼기를 거절했으면 진행 중인 떼기가 없다.
                    *hw = update_hardware(ctx, id, qemu, |hw, _| {
                        if let Some(layout) = &mut hw.layout {
                            layout.vcpus = reached;
                        }
                        hw.unplugging.retain(|d| !d.starts_with("cpu"));
                        if !change.refused {
                            hw.unplugging
                                .extend((to..reached).map(|i| format!("cpu{i}")));
                        }
                    })
                    .await?;
                    if reached == to {
                        return Ok(());
                    }
                    if change.refused {
                        VCPU_REFUSED.to_string()
                    } else if to < reached {
                        vcpu_held(reached, timeout)
                    } else {
                        format!("only {reached} of {to} vCPUs were added")
                    }
                }
                Err(e) => e.to_string(),
            };
            failed.push(failure(
                "vcpus",
                hw.vcpus().to_string(),
                to.to_string(),
                detail.clone(),
            ));
            Err(Stop::Failed(format!(
                "cannot change the vCPUs of VM {:?}: {detail}",
                vm.name
            )))
        }
        Step::SetMemory { plugged_mib } => {
            let before = hw.layout.as_ref().map_or(0, |l| l.plugged_mib);
            let detail = match ctx.hv.set_memory(id, plugged_mib, timeout).await {
                Ok(reached) => {
                    *hw = update_hardware(ctx, id, qemu, |hw, _| {
                        if let Some(layout) = &mut hw.layout {
                            layout.plugged_mib = reached;
                        }
                    })
                    .await?;
                    if reached == plugged_mib {
                        return Ok(());
                    }
                    if plugged_mib < before {
                        format!(
                            "the guest released only {} of {} MiB within {timeout:?}",
                            before.saturating_sub(reached),
                            before - plugged_mib
                        )
                    } else {
                        format!(
                            "only {} of {} MiB were plugged",
                            reached.saturating_sub(before),
                            plugged_mib - before
                        )
                    }
                }
                Err(e) => e.to_string(),
            };
            failed.push(failure(
                "memory_mib",
                hw.memory_now().to_string(),
                (hw.memory_mib + plugged_mib).to_string(),
                detail.clone(),
            ));
            Err(Stop::Failed(format!(
                "cannot change the memory of VM {:?}: {detail}",
                vm.name
            )))
        }
        Step::AddDisk { index, port } => {
            let disk = vm.spec.disks[index].clone();
            let not_added =
                |e: &dyn std::fmt::Display| format!("disk {:?} was not added: {e}", disk.volume);
            // 켤 때처럼 볼륨을 켜고 위치를 정한다(실패하면 켠 볼륨을 끈다).
            let runtime = runtime_disk(
                &ctx.cluster,
                vm,
                index,
                ctx.config.node,
                &ctx.config.mount_root,
                ctx.storage.as_ref(),
            )
            .await
            .map_err(|e| not_added(&e))?;
            same_qemu(ctx, id, qemu)?;
            match ctx.hv.add_disk(id, index, &runtime, port).await {
                Ok(AddOutcome::Added) => {}
                // 앞서 중단된 Task가 붙였다. 그대로 기록한다(볼륨은 그 장치가 쓴다).
                Ok(AddOutcome::AlreadyPresent) => {
                    tracing::info!(vm = %id, index, volume = %disk.volume, "the disk was already added; recording it");
                }
                Err(e) => {
                    // 같은 이름의 노드·장치가 이미 있으면 그 디스크가 볼륨을 쓰고 있을 수 있어 끄지 않는다.
                    let text = e.to_string();
                    if !text.contains("Duplicate") && !text.contains("already exists") {
                        same_qemu(ctx, id, qemu)?;
                        deactivate(ctx, id, &disk.volume).await;
                    }
                    return Err(Stop::Failed(not_added(&e)));
                }
            }
            *hw = update_hardware(ctx, id, qemu, |hw, _| {
                hw.disks.truncate(index);
                hw.disks.push(disk.clone());
                if let Some(layout) = &mut hw.layout {
                    layout.disk_ports.push(port);
                }
            })
            .await?;
            Ok(())
        }
        Step::RemoveDisk(index) => {
            let volume = hw.disks[index].volume.clone();
            let device = format!("disk{index}-dev");
            match ctx.hv.remove_disk(id, index, timeout).await {
                Ok(true) => {}
                Ok(false) => {
                    // 요청은 QEMU에 남아 게스트가 나중에 내려 줄 수 있다. 스펙은 사용자가 뗀 그대로 두고, 다음 Task가
                    // 마저 뗀다.
                    *hw = unplugging(ctx, id, qemu, &device).await?;
                    return Err(Stop::Failed(format!(
                        "disk {volume:?} was not removed: the guest did not release it within {timeout:?}"
                    )));
                }
                Err(e) => {
                    return Err(Stop::Failed(format!(
                        "disk {volume:?} was not removed: {e}"
                    )));
                }
            }
            *hw = update_hardware(ctx, id, qemu, |hw, _| {
                hw.disks.truncate(index);
                hw.unplugging.retain(|d| *d != device);
                let boot = hw.layout.as_ref().map_or(0, |l| usize::from(l.boot_disks));
                if let Some(layout) = &mut hw.layout {
                    layout.disk_ports.truncate(index.saturating_sub(boot));
                }
            })
            .await?;
            deactivate(ctx, id, &volume).await;
            Ok(())
        }
        Step::AddNic { index, port } => {
            let nic = vm.spec.nics[index].clone();
            let not_added = |e: &dyn std::fmt::Display| {
                format!(
                    "NIC {index} on network {:?} was not added: {e}",
                    nic.network
                )
            };
            let runtime = runtime_nic(&ctx.cluster, vm, index).map_err(|e| not_added(&e))?;
            // 보안 그룹을 건 NIC는 붙이기 전에 규칙을 넣는다(방화벽 없이 도는 순간이 없게).
            if !nic.security_groups.is_empty() {
                let Some(gate) = &ctx.config.firewall else {
                    return Err(Stop::Failed(not_added(
                        &"this node has no firewall (nftables)",
                    )));
                };
                gate.sync()
                    .await
                    .map_err(|e| not_added(&format!("firewall: {e}")))?;
                same_qemu(ctx, id, qemu)?;
            }
            let added = ctx
                .hv
                .add_nic(id, index, &runtime, port)
                .await
                .map_err(|e| not_added(&e))?;
            // 앞서 중단된 Task가 붙였으면 그대로 기록한다.
            if added == AddOutcome::AlreadyPresent {
                tracing::info!(vm = %id, index, "the NIC was already added; recording it");
            }
            *hw = update_hardware(ctx, id, qemu, |hw, _| {
                hw.nics.truncate(index);
                hw.nics.push(nic.clone());
                if let Some(layout) = &mut hw.layout {
                    layout.nic_ports.push(port);
                }
            })
            .await?;
            Ok(())
        }
        Step::ChangeMedium { index } => Err(Stop::Failed(format!(
            "CD-ROM drive {index}: this node cannot change CD-ROM media yet"
        ))),
        Step::RemoveNic(index) => {
            let network = hw.nics[index].network.clone();
            let device = format!("nic{index}");
            match ctx.hv.remove_nic(id, index, timeout).await {
                Ok(true) => {}
                Ok(false) => {
                    *hw = unplugging(ctx, id, qemu, &device).await?;
                    return Err(Stop::Failed(format!(
                        "NIC {index} on network {network:?} was not removed: the guest did not release it within {timeout:?}"
                    )));
                }
                Err(e) => {
                    return Err(Stop::Failed(format!(
                        "NIC {index} on network {network:?} was not removed: {e}"
                    )));
                }
            }
            *hw = update_hardware(ctx, id, qemu, |hw, _| {
                hw.nics.truncate(index);
                hw.unplugging.retain(|d| *d != device);
                let boot = hw.layout.as_ref().map_or(0, |l| usize::from(l.boot_nics));
                if let Some(layout) = &mut hw.layout {
                    layout.nic_ports.truncate(index.saturating_sub(boot));
                }
            })
            .await?;
            Ok(())
        }
    }
}

/// 게스트가 아직 내려 주지 않은 장치 `device`를 기록의 `unplugging`에 둔다(장치는 기록의 목록에 남는다).
async fn unplugging(
    ctx: &Ctx,
    id: Uuid,
    qemu: Qemu,
    device: &str,
) -> Result<RunningHardware, Stop> {
    update_hardware(ctx, id, qemu, |hw, _| {
        if !hw.unplugging.iter().any(|d| d == device) {
            hw.unplugging.push(device.to_string());
        }
    })
    .await
}

/// 기록의 `unplugging` 디스크·NIC마다 `failed` 보류(field는 장치 id): 다음 Task(`vm hotplug`)가 마저 뗀다. vCPU는
/// `vcpus` 보류가 알린다.
fn still_unplugging(hw: &RunningHardware) -> Vec<PendingChange> {
    hw.unplugging
        .iter()
        .filter(|device| !device.starts_with("cpu"))
        .map(|device| {
            let what = hotplug::device_name(hw, device);
            failure(
                device,
                what.clone(),
                "removed".into(),
                format!("the guest has not released {what} yet; run `vm hotplug` again"),
            )
        })
        .collect()
}

/// vCPU 줄이기가 멈춘 까닭: 큰 번호부터 떼므로 남은 가장 큰 vCPU를 게스트가 내려 주지 않았다.
fn vcpu_held(reached: u16, timeout: Duration) -> String {
    format!(
        "the guest did not release vCPU {} within {timeout:?}",
        reached.saturating_sub(1)
    )
}

fn failure(field: &str, running: String, wanted: String, detail: String) -> PendingChange {
    PendingChange {
        field: field.into(),
        running,
        wanted,
        reason: PendingReason::Failed,
        detail: Some(detail),
    }
}

/// 성공한 Task의 경고: `pending until restart: memory_mib (unsupported: this node has no virtio-mem)`
fn pending_warning(pending: &[PendingChange]) -> String {
    let items: Vec<String> = pending
        .iter()
        .map(|p| {
            let reason = reason_name(p.reason);
            match &p.detail {
                Some(detail) => format!("{} ({reason}: {detail})", p.field),
                None => format!("{} ({reason})", p.field),
            }
        })
        .collect();
    format!("pending until restart: {}", items.join(", "))
}

fn reason_name(reason: PendingReason) -> &'static str {
    match reason {
        PendingReason::Restart => "restart",
        PendingReason::Unsupported => "unsupported",
        PendingReason::Limit => "limit",
        PendingReason::Failed => "failed",
    }
}

fn read_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
    ctx.cluster
        .get::<VmSpec>(id)
        .map_err(|e| format!("cannot read VM {id}: {e}"))?
        .ok_or_else(|| format!("VM {id} not found"))
}

/// VM을 다시 읽어 Task를 시작할 때의 QEMU(`qemu`)가 아직 돌고 기록이 있는지 본다. 아니면 `Moved`다.
fn same_qemu(ctx: &Ctx, id: Uuid, qemu: Qemu) -> Result<Vm, Stop> {
    let vm = read_vm(ctx, id)?;
    if Qemu::of(&vm) != qemu || vm.status.hardware.is_none() {
        return Err(Stop::Moved);
    }
    Ok(vm)
}

/// 이 노드의 핫플러그 능력(기록이 없으면 vCPU·메모리 모두 못 함)
fn node_caps(ctx: &Ctx) -> Result<NodeHotplug, String> {
    let node = ctx
        .cluster
        .get::<NodeSpec>(ctx.config.node)
        .map_err(|e| format!("cannot read this node: {e}"))?;
    Ok(node.and_then(|n| n.status.hotplug).unwrap_or_default())
}

/// VM의 실행 중 하드웨어 기록을 `f`로 고쳐 revision을 확인하며 쓰고 쓴 값을 돌려준다. 그사이 누가 썼으면(reconciler의
/// 상태 쓰기) 다시 읽어 같은 고치기를 한다. 기록이 없어졌거나 다른 QEMU의 기록이면(VM이 꺼지거나 다시 켜지거나
/// 옮겨짐) `Moved`다.
async fn update_hardware(
    ctx: &Ctx,
    id: Uuid,
    qemu: Qemu,
    f: impl Fn(&mut RunningHardware, &Vm),
) -> Result<RunningHardware, Stop> {
    loop {
        let vm = same_qemu(ctx, id, qemu)?;
        let Some(mut hw) = vm.status.hardware.clone() else {
            return Err(Stop::Moved);
        };
        f(&mut hw, &vm);
        if vm.status.hardware.as_ref() == Some(&hw) {
            return Ok(hw);
        }
        let mut status = vm.status.clone();
        status.hardware = Some(hw.clone());
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(id, vm.revision, &status)
            .await
        {
            Ok(_) => return Ok(hw),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {
                return Err(Stop::Failed(format!("VM {:?} was deleted", vm.name)));
            }
            // QEMU는 이미 바뀌었으므로 기록될 때까지 다시 해 본다(Task 시간 제한 안에서).
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot record the running hardware; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 실패 뒤 스펙의 디스크·NIC 목록을 도는 장치(`hw`)에 맞게 되돌린다: 붙이지 못한 것과 하지 않은 붙이기는 빼고,
/// 떼지 못한 것(게스트가 내려 주기를 기다리는 `unplugging`은 빼고)과 하지 않은 떼기는 다시 넣는다. NIC의 보안 그룹은
/// 스펙의 것을 둔다. 다른 QEMU가 돌면 `Moved`다. 바꿨으면 true다.
async fn undo_devices(ctx: &Ctx, id: Uuid, qemu: Qemu, hw: &RunningHardware) -> Result<bool, Stop> {
    let held = |device: String| hw.unplugging.contains(&device);
    for _ in 0..10 {
        let vm = same_qemu(ctx, id, qemu)?;
        let mut spec = vm.spec.clone();
        spec.disks = hw
            .disks
            .iter()
            .enumerate()
            .filter(|(i, _)| !held(format!("disk{i}-dev")))
            .map(|(_, d)| d.clone())
            .collect();
        spec.nics = hw
            .nics
            .iter()
            .enumerate()
            .filter(|(i, _)| !held(format!("nic{i}")))
            .map(|(_, n)| {
                let mut nic = n.clone();
                if let Some(now) = vm.spec.nics.iter().find(|s| s.mac == n.mac) {
                    nic.security_groups = now.security_groups.clone();
                }
                nic
            })
            .collect();
        if spec == vm.spec {
            return Ok(false);
        }
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(true),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                return Err(Stop::Failed(format!(
                    "cannot undo the device change in the spec: {e}"
                )));
            }
        }
    }
    Err(Stop::Failed(
        "cannot undo the device change in the spec: the VM kept changing".into(),
    ))
}

/// 뗀 디스크나 붙이지 못한 디스크의 볼륨을 끈다(켤 때 켠 것을 되돌림). 실패는 기록만 한다(남은 볼륨은 실행기의
/// 주기 정리가 끈다).
async fn deactivate(ctx: &Ctx, vm: Uuid, volume: &str) {
    let found = ctx.cluster.get_by_name::<VolumeSpec>(volume).and_then(|v| {
        let Some(v) = v else { return Ok(None) };
        Ok(ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&v.spec.pool)?
            .map(|pool| (pool, v)))
    });
    match found {
        Ok(Some((pool, v))) => {
            if let Err(e) = ctx
                .storage
                .deactivate_volume(&pool, v.id, v.spec.format)
                .await
            {
                tracing::warn!(%vm, volume = %v.id, error = %e, "cannot deactivate volume");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(%vm, volume, error = %e, "cannot read volume to deactivate"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_warning_lists_each_pending_change_with_its_reason() {
        let pending = vec![
            PendingChange {
                field: "memory_mib".into(),
                running: "4096".into(),
                wanted: "8192".into(),
                reason: PendingReason::Unsupported,
                detail: Some("this node has no virtio-mem".into()),
            },
            PendingChange {
                field: "firmware".into(),
                running: "bios".into(),
                wanted: "uefi".into(),
                reason: PendingReason::Restart,
                detail: None,
            },
        ];
        assert_eq!(
            pending_warning(&pending),
            "pending until restart: memory_mib (unsupported: this node has no virtio-mem), firmware (restart)"
        );
    }

    #[test]
    fn a_held_vcpu_is_the_highest_left() {
        assert_eq!(
            vcpu_held(4, Duration::from_secs(60)),
            "the guest did not release vCPU 3 within 60s"
        );
    }
}
