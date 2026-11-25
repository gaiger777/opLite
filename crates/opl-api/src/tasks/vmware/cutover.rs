//! 전환·켠 뒤 되돌리기(계획 72 설계 §10). 전환은 이어 가는 Task이고 걸음(`cutover_step`)을 Raft에 먼저 적은 뒤 한다:
//!
//! | 걸음 | 하는 일 | 실패하면 |
//! |---|---|---|
//! | `preparing` | 원본을 건드리기 전 검사(이름·MAC·VMware 쪽 표·Tools), 따뜻한 이전은 켜진 채 델타 한 번, 차가운 이전은 flat 수정 시각 | 원본 그대로, 앞 상태로 |
//! | `stopping-source` | 원본 보호(설명·NIC `startConnected=false` — 옛 값을 먼저 적음), 게스트 종료(시간 한도) 또는 전원 끄기 | 바꾼 원본 설정을 되돌리고(우리가 껐으면 켬) 앞 상태로 |
//! | `final-sync` | (따뜻한 이전) 꺼진 원본의 마지막 델타, 우리 스냅샷 지움 | 자동 되돌림 |
//! | `creating-vm` | 볼륨 `ready`, 매핑한 VmSpec(머신·Mirror 폴더·Tools IP 예약·드라이버 ISO)으로 VM 만들기 | 자동 되돌림 |
//! | `starting-vm` | `start_after`면 켜고 `running`을 본다(5분) | 이 Task가 처음 켜다 실패했을 때만 자동 되돌림. 켰거나 켰을 수 있으면(이어 감) `done`에 오류, VM·디스크는 둠 |
//!
//! 자동 되돌림(§10.2): 대상 VM을 끄고 지움(볼륨은 `pending`으로 되돌려 남김) → 원본 NIC·설명 되돌림 → 우리가 끈 원본을 켬 →
//! `rolled-back`. 되돌림이 실패하면 `failed`로 두고 어느 걸음까지 했는지와 손으로 할 일을 적는다(다시 하기가 되돌림을 다시
//! 한다). 원본을 지우는 걸음은 없다.

use opl_types::folder::{FolderKind, FolderSpec};
use opl_types::network::NetworkSpec;
use opl_types::node::{FOLDERS, NodeSpec};
use opl_types::storage::VolumeStatus;
use opl_types::vm::{MacAddr, PowerState, Vm, VmSpec};
use opl_types::vm_check::VmCheckLevel;
use opl_types::vmware::{
    SHUTDOWN_WATCH_S, VmwareCutoverStep, VmwareFolderRule, VmwarePower, VmwareShutdown,
    VmwareSourceChanges,
};

use super::super::clone::create_vm;
use super::super::{mark_ready, set_power, started, stopped, wait_vm};
use super::*;
use crate::audit::system_entry;
use opl_types::audit::AuditTarget;

/// 게스트 종료 시간을 넘긴 뒤 더 지켜보는 시간의 상한(그새 꺼지면 다시 켠다)
const SHUTDOWN_GRACE: Duration = Duration::from_secs(120);
/// 게스트 종료가 시간 한도를 넘겨 멈춘 전환 뒤 다음 동기화까지의 시간 상한(초 — 늦게 꺼진 원본을 곧 다시 켠다)
const SHUTDOWN_RECHECK_S: u64 = 300;

/// 걸음 하나의 실패
struct StepError {
    step: VmwareCutoverStep,
    message: String,
    /// 실패를 걸음 안에서 이미 적었다(차가운 이전의 다시 복사)
    recorded: bool,
}

/// 걸음 `step`의 실패로 바꾼다.
fn at<E: std::fmt::Display>(step: VmwareCutoverStep) -> impl Fn(E) -> StepError {
    move |e| StepError {
        step,
        message: e.to_string(),
        recorded: false,
    }
}

impl From<SyncError> for String {
    fn from(e: SyncError) -> Self {
        e.message
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn step_name(step: VmwareCutoverStep) -> &'static str {
    match step {
        VmwareCutoverStep::Preparing => "preparing",
        VmwareCutoverStep::StoppingSource => "stopping-source",
        VmwareCutoverStep::FinalSync => "final-sync",
        VmwareCutoverStep::CreatingVm => "creating-vm",
        VmwareCutoverStep::StartingVm => "starting-vm",
    }
}

/// 사람이 읽는 시간(`4m12s`)
fn duration_text(secs: u64) -> String {
    match secs {
        s if s >= 3600 => format!("{}h{}m{}s", s / 3600, s % 3600 / 60, s % 60),
        s if s >= 60 => format!("{}m{}s", s / 60, s % 60),
        s => format!("{s}s"),
    }
}

/// 노드의 일을 감사 로그에 적는다(요청이 아닌 Task의 결과 — 내린 시간·되돌림).
fn audit(ctx: &Ctx, action: &str, item: &str, detail: String) {
    if let Some(api) = &ctx.config.api {
        let target = AuditTarget {
            kind: "vmware-vm".into(),
            key: item.to_string(),
        };
        api.audit.record(system_entry(action, target, detail));
    }
}

/// 원본 VM의 세션과 참조(되돌림·되돌리기 — 인벤토리 없이)
async fn source_session(ctx: &Ctx, item: &VmwareVm) -> Result<(Arc<Session>, MoRef), String> {
    let migration = get_migration(ctx, item.spec.migration).map_err(|e| e.message)?;
    let source = get_source(ctx, &migration).map_err(|e| e.message)?;
    let session = session_for(ctx, &source)
        .await
        .map_err(|e| format!("cannot reach the source: {e}"))?;
    Ok((session, MoRef::new("VirtualMachine", &item.spec.source_ref)))
}

/// 바꾼 원본 설정을 되돌린다(옛 값은 Raft에): NIC `startConnected`를 다시 켜고 설명을 되살린 뒤, 우리가 끈 원본이 아직
/// 꺼져 있으면 켠다(NIC를 먼저 되돌려야 켤 때 연결된다). 되돌린 것마다 Raft에서 지운다.
pub(super) async fn restore_source(
    ctx: &Ctx,
    session: &Session,
    vm_ref: &MoRef,
    id: Uuid,
) -> Result<(), String> {
    let changes = get_item(ctx, id)?.status.source_changes;
    if !changes.nics_disconnected.is_empty() {
        session
            .set_nics_start_connected(vm_ref, &changes.nics_disconnected, true)
            .await
            .map_err(|e| format!("cannot reconnect the source NICs: {e}"))?;
        update_item(ctx, id, |s| s.source_changes.nics_disconnected.clear()).await?;
    }
    if changes.annotation_added {
        let old = changes.old_annotation.clone().unwrap_or_default();
        session
            .set_annotation(vm_ref, &old)
            .await
            .map_err(|e| format!("cannot restore the source annotation: {e}"))?;
        update_item(ctx, id, |s| {
            s.source_changes.annotation_added = false;
            s.source_changes.old_annotation = None;
        })
        .await?;
    }
    if changes.powered_off_by_us {
        let power = session
            .power_state(vm_ref)
            .await
            .map_err(|e| format!("cannot read the source power state: {e}"))?;
        if power == VmwarePower::PoweredOff {
            session
                .power_on(vm_ref)
                .await
                .map_err(|e| format!("cannot power the source VM on: {e}"))?;
        }
        update_item(ctx, id, |s| s.source_changes.powered_off_by_us = false).await?;
    }
    Ok(())
}

/// 전환이 만든 VM을 끄고 지운다(볼륨은 지우지 않고 `pending`으로 되돌린다 — 다시 전환하면 쓴다).
pub(super) async fn remove_target_vm(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let item = get_item(ctx, id)?;
    let Some(vm) = item.status.target_vm else {
        return Ok(());
    };
    if ctx
        .cluster
        .get::<VmSpec>(vm)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        stop_vm(ctx, vm).await?;
        match ctx.cluster.delete::<VmSpec>(vm, None).await {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(format!("cannot delete the Orbivisor VM: {e}")),
        }
    }
    for disk in &item.status.disks {
        let Some(vol) = disk.volume else { continue };
        let Some(v) = ctx
            .cluster
            .get::<VolumeSpec>(vol)
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        if v.status.phase == VolumePhase::Pending {
            continue;
        }
        let status = VolumeStatus {
            phase: VolumePhase::Pending,
            attached_to: None,
            ..v.status.clone()
        };
        ctx.cluster
            .update_status::<VolumeSpec>(vol, &status)
            .await
            .map_err(|e| format!("cannot return volume {:?} to pending: {e}", v.name))?;
    }
    update_item(ctx, id, |s| s.target_vm = None).await?;
    Ok(())
}

/// VM을 끄고 꺼질 때까지 기다린다(게스트가 ACPI를 무시하면 reconciler가 강제로 끈다).
async fn stop_vm(ctx: &Ctx, vm: Uuid) -> Result<(), String> {
    set_power(ctx, vm, PowerState::Stopped).await?;
    match tokio::time::timeout(ctx.config.vmware_start_timeout, wait_vm(ctx, vm, stopped)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) if vm_gone(ctx, vm) => Ok(()),
        Ok(Err(e)) => Err(format!("cannot stop the Orbivisor VM: {e}")),
        Err(_) => Err(format!(
            "the Orbivisor VM did not stop within {}s",
            ctx.config.vmware_start_timeout.as_secs()
        )),
    }
}

fn vm_gone(ctx: &Ctx, vm: Uuid) -> bool {
    matches!(ctx.cluster.get::<VmSpec>(vm), Ok(None))
}

// ---- vmware-cutover ----

/// `vmware-cutover` Task(설계 §10.1): `ready`·`syncing`·`rolled-back` 항목을 전환한다. 끊긴 전환(`cutting-over`)은 적힌
/// 걸음부터 잇고, 되돌림이 실패한 항목(`failed` + 걸음)은 되돌림을 다시 한다 — 다시 하기가 적은 Task(`rollback_task`)일
/// 때만. 그 Task가 항목을 다른 상태에서 보면(겹친 요청 등) 아무것도 하지 않는다. 취소된 계획의 항목은 새로 전환하지 않는다.
pub(in crate::tasks) async fn cutover(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let item = get_item(ctx, id)?;
    let skip = |why: &str| {
        Ok(Some(format!(
            "skipped: vmware-vm {:?} is {}{why}",
            item.name,
            item.status.state.as_str()
        )))
    };
    let retry = item.status.rollback_task == Some(task.id);
    let from = match item.status.state {
        VmwareVmState::Failed if retry && item.status.cutover_step.is_some() => {
            let step = item.status.cutover_step.expect("checked");
            // 앞 실패의 까닭만 남긴다(되돌림 실패 문구는 버린다).
            let error = item.status.error.clone().unwrap_or_default();
            let cause = error.split("; rollback failed").next().unwrap_or_default();
            return Box::pin(roll_back(ctx, id, step, cause)).await;
        }
        _ if retry => return skip("; nothing to roll back"),
        VmwareVmState::Ready | VmwareVmState::Syncing | VmwareVmState::RolledBack => {
            VmwareCutoverStep::Preparing
        }
        VmwareVmState::CuttingOver => item
            .status
            .cutover_step
            .unwrap_or(VmwareCutoverStep::Preparing),
        VmwareVmState::Failed if item.status.cutover_step.is_some() => {
            return skip("; retry it to roll back");
        }
        _ => return skip(""),
    };
    let migration = get_migration(ctx, item.spec.migration).map_err(|e| e.message)?;
    if from == VmwareCutoverStep::Preparing
        && item.status.state != VmwareVmState::CuttingOver
        && migration.status.phase == VmwareMigrationPhase::Cancelled
    {
        return skip(" and its migration was cancelled");
    }
    let warm = migration.spec.mode == VmwareMigrationMode::Warm;
    if item.status.state != VmwareVmState::CuttingOver {
        let at = now();
        update_item(ctx, id, |s| {
            s.state = VmwareVmState::CuttingOver;
            s.cutover_step = Some(VmwareCutoverStep::Preparing);
            s.error = None;
            s.downtime_s = None;
            s.source_stopped_at = None;
            s.push_history(at, "cutover started");
        })
        .await?;
        recount(ctx, item.spec.migration).await;
    }
    match Box::pin(steps(ctx, task, id, migration, from)).await {
        Ok(warning) => Ok(warning),
        Err(e) if e.recorded => Err(e.message),
        Err(e) if e.step.rolls_back() => {
            let message = format!("cutover failed at {}: {}", step_name(e.step), e.message);
            Box::pin(roll_back(ctx, id, e.step, &message)).await?;
            Err(message)
        }
        Err(e) => {
            let message = format!("cutover stopped at {}: {}", step_name(e.step), e.message);
            Box::pin(stop_before_source(ctx, id, warm, &message)).await?;
            Err(message)
        }
    }
}

/// 걸음을 적는다(하기 전에).
async fn set_step(ctx: &Ctx, id: Uuid, step: VmwareCutoverStep) -> Result<(), StepError> {
    update_item(ctx, id, |s| {
        s.cutover_step = Some(step);
        if step == VmwareCutoverStep::StoppingSource {
            s.source_stopped_at.get_or_insert_with(now);
        }
    })
    .await
    .map(|_| ())
    .map_err(at(step))
}

async fn steps(
    ctx: &Ctx,
    task: &Task,
    id: Uuid,
    migration: VmwareMigration,
    from: VmwareCutoverStep,
) -> Result<Option<String>, StepError> {
    use VmwareCutoverStep::*;
    let item = get_item(ctx, id).map_err(at(from))?;
    // 만든 VM이 있으면 남은 걸음(켜기·적기)에 vSphere가 필요 없다: 이어 갈 때 원본 연결이 안 돼도(세션 만료·원본을 지움)
    // 켠 VM을 되돌리지 않는다(리뷰 2).
    let exists = item.status.target_vm.is_some_and(|vm| !vm_gone(ctx, vm));
    let mut warnings = Vec::new();
    if from <= FinalSync || (from == CreatingVm && !exists) {
        let mut run = Box::pin(open_run(ctx, task, &item, migration.clone()))
            .await
            .map_err(at(from))?;
        if from <= Preparing {
            Box::pin(prepare(&run)).await?;
        }
        if from <= StoppingSource {
            set_step(ctx, id, StoppingSource).await?;
            refresh(&mut run).await.map_err(at(StoppingSource))?;
            Box::pin(stop_source(&run))
                .await
                .map_err(at(StoppingSource))?;
        }
        if from <= FinalSync {
            set_step(ctx, id, FinalSync).await?;
            if run.warm() {
                refresh(&mut run).await.map_err(at(FinalSync))?;
                Box::pin(final_sync(&run)).await.map_err(at(FinalSync))?;
            }
        }
        set_step(ctx, id, CreatingVm).await?;
        Box::pin(create_target(&run, &mut warnings))
            .await
            .map_err(at(CreatingVm))?;
        warnings.extend(run.warnings());
    }
    set_step(ctx, id, StartingVm).await?;
    // 이어 가는 `starting-vm`은 앞 opld가 이미 켰을 수 있다(QEMU는 opld 없이도 돈다 — 게스트가 대상 디스크에 썼다).
    Box::pin(start_and_finish(ctx, &migration, id, from == StartingVm)).await?;
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

/// 원본 VM을 새로 읽는다(걸음 사이 — 전원·NIC·설명·스냅샷).
async fn refresh(run: &mut Run<'_>) -> Result<(), SyncError> {
    run.vm = inventory::refresh_vm(&run.session, &run.inv, &run.vm.moref).await?;
    Ok(())
}

/// 원본을 건드리기 전 검사(우리 쪽 — 주체 없이 볼 수 있는 것: 이름·MAC, 할당량·용량은 VM을 만들 때 상태 머신이 본다). 다시
/// 해도 같은 문제를 하나의 문장으로 돌려준다.
fn target_problems(ctx: &Ctx, item: &VmwareVm, run: &Run<'_>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let others: Vec<&Vm> = vms
        .iter()
        .filter(|v| Some(v.id) != item.status.target_vm)
        .collect();
    if others.iter().any(|v| v.name == item.spec.target_name) {
        out.push(format!(
            "name_taken: a VM named {:?} already exists",
            item.spec.target_name
        ));
    }
    let keep_macs = item
        .spec
        .overrides
        .as_ref()
        .and_then(|o| o.keep_macs)
        .unwrap_or(run.migration.spec.rules.keep_macs);
    if keep_macs {
        for nic in &run.vm.nics {
            let Some(mac) = nic.mac.as_deref().and_then(|m| m.parse::<MacAddr>().ok()) else {
                continue;
            };
            if others
                .iter()
                .any(|v| v.spec.nics.iter().any(|n| n.mac == Some(mac)))
            {
                out.push(format!(
                    "mac_taken: MAC address {mac} of the source VM is already used by an Orbivisor VM"
                ));
            }
        }
    }
    Ok(out)
}

/// `preparing`: 다시 미리 확인(원본 호출 없이 — VMware 쪽 표·Tools, 우리 쪽 이름·MAC). 따뜻한 이전은 켜진 원본에서 델타
/// 한 번(마지막 델타를 줄임 — `max_final_delta_gib`를 넘으면 멈춤), 차가운 이전은 원본이 꺼져 있고 flat 파일이 복사 때와
/// 같은지 본다(달라졌으면 다시 전체 복사로 돌린다).
async fn prepare(run: &Run<'_>) -> Result<(), StepError> {
    let step = VmwareCutoverStep::Preparing;
    let ctx = run.ctx;
    let item = get_item(ctx, run.item).map_err(at(step))?;
    let ours: Vec<&str> = [&item.status.snapshot, &item.status.pending_snapshot]
        .into_iter()
        .flatten()
        .map(|s| s.moref.as_str())
        .collect();
    let source_ctx = plan::SourceContext {
        mode: run.migration.spec.mode,
        cutover: &run.migration.spec.cutover,
        status: &run.source.status,
        transport: &run.source.spec.transport,
        inventory: &run.inv,
    };
    let mut problems: Vec<String> = plan::source_problems(&source_ctx, &item.spec, &run.vm, &ours)
        .into_iter()
        .filter(|p| p.level == VmCheckLevel::Error)
        .map(|p| format!("{}: {}", p.code, p.message))
        .collect();
    let on = run.vm.power != Some(VmwarePower::PoweredOff);
    if on && run.migration.spec.cutover.shutdown == VmwareShutdown::Guest && !run.vm.tools_running()
    {
        problems.push(
            "vmware_no_tools: VMware Tools is not running, so the guest cannot be shut down; start Tools or use the power-off shutdown"
                .into(),
        );
    }
    problems.extend(target_problems(ctx, &item, run).map_err(at(step))?);
    if !problems.is_empty() {
        return Err(at(step)(problems.join("; ")));
    }
    if run.warm() {
        let targets = Box::pin(ensure_disks(run)).await.map_err(at(step))?;
        let started = Instant::now();
        let copied = Box::pin(warm_round(run, &targets, false))
            .await
            .map_err(at(step))?;
        finish_round(run, copied, started.elapsed(), None)
            .await
            .map_err(at(step))?;
        if let Some(gib) = run.migration.spec.cutover.max_final_delta_gib {
            let delta: u64 = get_item(ctx, run.item)
                .map_err(at(step))?
                .status
                .disks
                .iter()
                .map(|d| d.last_delta_bytes)
                .sum();
            if delta > u64::from(gib) << 30 {
                return Err(at(step)(format!(
                    "final_delta_too_large: the last delta was {} (limit {gib} GiB); the source changes too fast for this cutover window",
                    size_text(delta)
                )));
            }
        }
        return Ok(());
    }
    if on {
        return Err(at(step)(
            "vmware_powered_on: the VM is powered on; power it off for a cold cutover",
        ));
    }
    for (i, disk) in item.status.disks.iter().enumerate() {
        let Some(copied) = &disk.source_mtime else {
            continue;
        };
        let Some(now_mtime) = flat_mtime(run, disk).await else {
            continue;
        };
        if &now_mtime != copied {
            // 그새 켜졌다: 다시 전체 복사(스케줄러가 곧 잇는다).
            let at_time = now();
            let message = format!(
                "the source disk {i} changed after the copy (modified {now_mtime}); copying it again in full"
            );
            let m = message.clone();
            update_item(ctx, run.item, |s| {
                s.state = VmwareVmState::Copying;
                s.cutover_step = None;
                s.rounds = 0;
                s.next_sync_at = Some(at_time);
                for d in &mut s.disks {
                    d.rounds = 0;
                    d.copied_bytes = 0;
                    d.source_mtime = None;
                }
                s.error = Some(m.clone());
                s.push_history(at_time, "cutover stopped: the source disk changed");
            })
            .await
            .map_err(at(step))?;
            recount(ctx, item.spec.migration).await;
            return Err(StepError {
                step,
                message,
                recorded: true,
            });
        }
    }
    Ok(())
}

/// 원본에 다는 설명(옛 설명은 뒤에 남긴다)
fn protect_note(run: &Run<'_>, item: &VmwareVm, old: &str) -> String {
    let note = format!(
        "Migrated to Orbivisor VM {:?} by migration {:?}. Keep this VM powered off; its NICs stay disconnected at power-on.",
        item.spec.target_name, run.migration.name
    );
    if old.is_empty() {
        note
    } else {
        format!("{note}\n\n{old}")
    }
}

/// `stopping-source`: 원본 보호(옛 값을 먼저 적고 바꿈), 게스트 종료(시간 한도) 또는 전원 끄기
async fn stop_source(run: &Run<'_>) -> Result<(), String> {
    let ctx = run.ctx;
    let vm_ref = run.vm_ref();
    let policy = &run.migration.spec.cutover;
    let item = get_item(ctx, run.item)?;
    if policy.protect_source {
        // 이어 갈 때도 적은 것만 믿지 않고 원본의 지금 값(걸음 앞에서 새로 읽음)과 맞춘다: 적은 뒤 `ReconfigVM` 전에 opld가
        // 죽었으면 다시 바꾼다(같은 값으로 바꾸기는 해가 없다 — 리뷰 2).
        let changes = &item.status.source_changes;
        let old = if changes.annotation_added {
            changes.old_annotation.clone().unwrap_or_default()
        } else {
            let old = run.vm.annotation.clone().unwrap_or_default();
            let o = old.clone();
            update_item(ctx, run.item, |s| {
                s.source_changes.annotation_added = true;
                s.source_changes.old_annotation = Some(o.clone());
            })
            .await?;
            old
        };
        let note = protect_note(run, &item, &old);
        if run.vm.annotation.as_deref().unwrap_or_default() != note {
            run.session
                .set_annotation(&vm_ref, &note)
                .await
                .map_err(|e| format!("cannot annotate the source VM: {e}"))?;
        }
        let keys: Vec<i32> = run
            .vm
            .nics
            .iter()
            .filter(|n| n.start_connected != Some(false))
            .map(|n| n.key)
            .collect();
        if !keys.is_empty() {
            let k = keys.clone();
            update_item(ctx, run.item, |s| {
                for key in &k {
                    if !s.source_changes.nics_disconnected.contains(key) {
                        s.source_changes.nics_disconnected.push(*key);
                    }
                }
            })
            .await?;
            run.session
                .set_nics_start_connected(&vm_ref, &keys, false)
                .await
                .map_err(|e| format!("cannot disconnect the source NICs at power-on: {e}"))?;
        }
    }
    let power = run
        .session
        .power_state(&vm_ref)
        .await
        .map_err(|e| e.to_string())?;
    if item.status.shutdown_sent_at.is_some() {
        // 앞서 멈춘 전환이 보낸 게스트 종료로 꺼졌을 수 있다: 우리가 끈 것으로 적는다(되돌림이 켠다).
        let off = power == VmwarePower::PoweredOff;
        update_item(ctx, run.item, |s| {
            s.shutdown_sent_at = None;
            s.source_changes.powered_off_by_us |= off;
        })
        .await?;
    }
    if power == VmwarePower::PoweredOff {
        return Ok(());
    }
    update_item(ctx, run.item, |s| s.source_changes.powered_off_by_us = true).await?;
    let guest = policy.shutdown == VmwareShutdown::Guest && power == VmwarePower::PoweredOn;
    if guest {
        let limit = Duration::from_secs(policy.shutdown_timeout_s.into());
        let limit = ctx
            .config
            .vmware_shutdown_limit
            .map_or(limit, |c| c.min(limit));
        match run.session.shutdown_guest(&vm_ref).await {
            // 그새 꺼졌다(`InvalidPowerState`) — 아래에서 꺼짐을 본다.
            Ok(()) => {}
            Err(VsphereError::InvalidState { kind, .. }) if kind == "InvalidPowerState" => {}
            Err(e) => return Err(format!("cannot shut the guest down: {e}")),
        }
        let sent = now();
        let every = (limit / 4).clamp(Duration::from_millis(50), Duration::from_secs(2));
        let deadline = Instant::now() + limit;
        loop {
            let p = run
                .session
                .power_state(&vm_ref)
                .await
                .map_err(|e| e.to_string())?;
            if p == VmwarePower::PoweredOff {
                break;
            }
            if Instant::now() >= deadline {
                // 보낸 종료는 거둘 수 없다: 느린 게스트(업데이트·큰 DB)는 곧 꺼진다. 조금 더 지켜보고, 그새 꺼지면 되돌림이
                // (NIC를 먼저 되돌린 뒤) 다시 켠다 — `powered_off_by_us`가 남아 있다(리뷰 1).
                let grace = limit.min(SHUTDOWN_GRACE);
                let until = Instant::now() + grace;
                let mut off = false;
                while Instant::now() < until {
                    tokio::time::sleep(every).await;
                    let p = run
                        .session
                        .power_state(&vm_ref)
                        .await
                        .map_err(|e| e.to_string())?;
                    if p == VmwarePower::PoweredOff {
                        let at_time = now();
                        update_item(ctx, run.item, |s| {
                            s.push_history(
                                at_time,
                                "the guest shut down after the time limit; powering the source VM on again",
                            );
                        })
                        .await?;
                        off = true;
                        break;
                    }
                }
                if off {
                    return Err(format!(
                        "vmware_shutdown_timeout: the guest did not shut down within {}s; the source VM was left running",
                        limit.as_secs()
                    ));
                }
                // 아직 켜져 있다: 게스트가 더 늦게 꺼질 수 있다 — 다음 동기화가 보고 다시 켠다(리뷰 2).
                update_item(ctx, run.item, |s| s.shutdown_sent_at = Some(sent)).await?;
                return Err(format!(
                    "vmware_shutdown_timeout: the guest did not shut down within {}s; the source VM was left running, \
                     but it may still finish shutting down: the next sync powers it on again if it does (within {}h)",
                    limit.as_secs(),
                    SHUTDOWN_WATCH_S / 3600
                ));
            }
            tokio::time::sleep(every).await;
        }
    } else {
        run.session
            .power_off(&vm_ref)
            .await
            .map_err(|e| format!("cannot power the source VM off: {e}"))?;
    }
    let at_time = now();
    let how = if guest {
        "guest shut down"
    } else {
        "powered off"
    };
    update_item(ctx, run.item, |s| {
        s.push_history(at_time, format!("source VM {how}"));
    })
    .await?;
    Ok(())
}

/// 멈춘 전환이 보낸 게스트 종료를 동기화가 다시 볼 때 할 일([`VmwareVmStatus::shutdown_sent_at`])
#[derive(Debug, PartialEq, Eq)]
enum ShutdownWatch {
    /// 아직 켜져 있고 기간 안 — 표시를 두고 다음에 다시 본다.
    Wait,
    /// 기간 안에 꺼졌다 — 보낸 종료로 꺼진 것이다: 다시 켠다.
    PowerOn,
    /// 기간이 지났다 — 표시를 지운다(꺼져 있으면 켜지 않고 경고만 — 사용자가 끈 것일 수 있다).
    Expired { off: bool },
}

fn shutdown_watch(sent: u64, at: u64, off: bool) -> ShutdownWatch {
    match (at < sent + SHUTDOWN_WATCH_S, off) {
        (true, false) => ShutdownWatch::Wait,
        (true, true) => ShutdownWatch::PowerOn,
        (false, off) => ShutdownWatch::Expired { off },
    }
}

/// 동기화 회차 앞(리뷰 2): 멈춘 전환이 보낸 게스트 종료가 늦게 끝나 원본이 꺼졌으면 다시 켜고 기록·감사에 남긴다(원본은
/// 아직 운영 VM이다 — 대상 VM이 없다). 기간([`SHUTDOWN_WATCH_S`])이 지나면 표시를 지우고, 그때 꺼져 있으면 경고를 남긴다.
pub(super) async fn recheck_shutdown(run: &mut Run<'_>, sent: u64) -> Result<(), SyncError> {
    let ctx = run.ctx;
    let at_time = now();
    let off = run.vm.power == Some(VmwarePower::PoweredOff);
    match shutdown_watch(sent, at_time, off) {
        ShutdownWatch::Wait => Ok(()),
        ShutdownWatch::PowerOn => {
            run.session.power_on(&run.vm_ref()).await?;
            let item = update_item(ctx, run.item, |s| {
                s.shutdown_sent_at = None;
                s.push_history(
                    at_time,
                    "the guest finished the shutdown sent by the stopped cutover; the source VM was powered on again",
                );
            })
            .await?;
            audit(
                ctx,
                "vmware-vm.source-power-on",
                &item.name,
                format!(
                    "vmware-vm {}: source VM powered on again after a late guest shutdown",
                    item.name
                ),
            );
            refresh(run).await
        }
        ShutdownWatch::Expired { off } => {
            let warning = off.then(|| {
                "the source VM is powered off; it may have finished the guest shutdown sent by a stopped cutover \
                 — power it on in vCenter if it should run"
                    .to_string()
            });
            update_item(ctx, run.item, |s| {
                s.shutdown_sent_at = None;
                if let Some(w) = &warning
                    && !s.warnings.contains(w)
                {
                    s.warnings.push(w.clone());
                    s.push_history(
                        at_time,
                        "source VM found powered off after the shutdown watch",
                    );
                }
            })
            .await?;
            if let Some(w) = warning {
                run.warn(w);
            }
            Ok(())
        }
    }
}

/// 원본이 꺼져 있는지 본다(같은 MAC이 둘 켜지지 않게 — 대상 VM을 만들기 전에).
async fn require_source_off(run: &Run<'_>) -> Result<(), String> {
    let power = run
        .session
        .power_state(&run.vm_ref())
        .await
        .map_err(|e| e.to_string())?;
    if power != VmwarePower::PoweredOff {
        return Err("the source VM was powered on during the cutover".into());
    }
    Ok(())
}

/// `final-sync`(따뜻한 이전): 꺼진 원본에서 회차 하나(S(final) → 옛 것 지움 → 바뀐 구역), 그다음 S(final)을 지운다(기반 =
/// 마지막 내용, 원본에 우리 스냅샷 없음).
async fn final_sync(run: &Run<'_>) -> Result<(), String> {
    let ctx = run.ctx;
    require_source_off(run).await?;
    let targets = Box::pin(ensure_disks(run)).await?;
    let started = Instant::now();
    let copied = Box::pin(warm_round(run, &targets, false)).await?;
    finish_round(run, copied, started.elapsed(), None).await?;
    let item = get_item(ctx, run.item)?;
    for snap in [&item.status.pending_snapshot, &item.status.snapshot]
        .into_iter()
        .flatten()
    {
        remove_ours(&run.session, snap)
            .await
            .map_err(|e| format!("cannot remove snapshot {}: {e}", snap.name))?;
        let moref = snap.moref.clone();
        update_item(ctx, run.item, |s| {
            if s.pending_snapshot
                .as_ref()
                .is_some_and(|p| p.moref == moref)
            {
                s.pending_snapshot = None;
            }
            if s.snapshot.as_ref().is_some_and(|p| p.moref == moref) {
                s.snapshot = None;
            }
        })
        .await?;
    }
    Ok(())
}

/// Mirror 폴더: 원본 VM 폴더 경로(`Prod/Web`)의 우리 VM 폴더를 `under` 아래에 찾거나 만든다. 마지막 폴더를 돌려준다.
async fn mirror_folder(ctx: &Ctx, under: Option<Uuid>, path: &str) -> Result<Uuid, String> {
    let mut parent = under;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        let (_, folders) = ctx
            .cluster
            .list::<FolderSpec>()
            .map_err(|e| e.to_string())?;
        let found = folders.iter().find(|f| {
            f.spec.kind == FolderKind::Vm
                && f.spec.parent == parent
                && f.spec.name.eq_ignore_ascii_case(part)
        });
        let id = match found {
            Some(f) => f.id,
            None => {
                let id = Uuid::now_v7();
                let spec = FolderSpec {
                    name: part.to_string(),
                    kind: FolderKind::Vm,
                    parent,
                    quota: None,
                };
                spec.validate(id)
                    .map_err(|e| format!("cannot create VM folder {part:?}: {e}"))?;
                let mut folder: Resource<FolderSpec, _> = Resource::new(id.to_string(), spec);
                folder.id = id;
                ctx.cluster
                    .create(&folder)
                    .await
                    .map_err(|e| format!("cannot create VM folder {part:?}: {e}"))?;
                id
            }
        };
        parent = Some(id);
    }
    parent.ok_or_else(|| "empty folder path".to_string())
}

/// `creating-vm`: 볼륨을 `ready`로, 매핑한 VmSpec으로 VM을 만든다(id는 먼저 적는다 — 이어 갈 때 다시 만들지 않는다).
async fn create_target(run: &Run<'_>, warnings: &mut Vec<String>) -> Result<(), String> {
    let ctx = run.ctx;
    let item = get_item(ctx, run.item)?;
    if let Some(vm) = item.status.target_vm
        && ctx
            .cluster
            .get::<VmSpec>(vm)
            .map_err(|e| e.to_string())?
            .is_some()
    {
        return Ok(());
    }
    require_source_off(run).await?;
    let rules = &run.migration.spec.rules;
    let (_, networks) = ctx
        .cluster
        .list::<NetworkSpec>()
        .map_err(|e| e.to_string())?;
    let names: Vec<String> = networks.iter().map(|n| n.name.clone()).collect();
    let format = |pool: &str| {
        ctx.cluster
            .get_by_name::<StoragePoolSpec>(pool)
            .ok()
            .flatten()
            .map(|p| default_format(&p))
            .unwrap_or(ImageFormat::Qcow2)
    };
    let mapped = plan::map_vm(&plan::MapInput {
        rules,
        item_id: item.id,
        item: &item.spec,
        vm: &run.vm,
        networks: &names,
        format: &format,
    })
    .map_err(|problems| {
        problems
            .iter()
            .map(|p| format!("{}: {}", p.code, p.message))
            .collect::<Vec<_>>()
            .join("; ")
    })?;
    let mut spec = mapped.spec;
    // 머신은 노드 아키텍처로(미리 확인이 `machine`을 빼고 채운 것과 같다).
    let (_, nodes) = ctx.cluster.list::<NodeSpec>().map_err(|e| e.to_string())?;
    spec.machine =
        crate::vms::default_machine(&nodes, item.spec.node.or(spec.node)).map_err(|e| e.message)?;
    // 노드: 규칙의 노드, 아니면 동기화한 노드(대상 볼륨이 있는 곳 — 로컬 풀일 수 있다).
    spec.node = rules.node.or(item.spec.node).or(Some(ctx.config.node));
    let overridden = item
        .spec
        .overrides
        .as_ref()
        .is_some_and(|o| o.folder.is_some());
    if let (VmwareFolderRule::Mirror { under }, false, Some(path)) =
        (&rules.folders, overridden, plan::mirror_path(&run.vm))
        && crate::cluster::missing_feature(&ctx.cluster, FOLDERS)
            .map_err(|e| e.to_string())?
            .is_empty()
    {
        spec.folder = Some(mirror_folder(ctx, *under, &path).await?);
    }
    let requested = item
        .spec
        .overrides
        .as_ref()
        .is_some_and(|o| !o.ips.is_empty());
    if rules.reserve_ips && !requested {
        warnings.extend(plan::reserve_ips(&mut spec, &run.vm.guest_ips, &networks));
    }
    spec.validate()
        .map_err(|e| format!("invalid VM spec: {e}"))?;
    let vm_id = item.status.target_vm.unwrap_or_else(Uuid::now_v7);
    update_item(ctx, run.item, |s| s.target_vm = Some(vm_id)).await?;
    // 볼륨은 VM을 만들기 바로 전에 `ready`로(가져오기와 같다).
    for disk in &item.status.disks {
        let Some(vol) = disk.volume else { continue };
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(vol)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("target volume {vol} no longer exists"))?;
        if volume.status.phase == VolumePhase::Pending {
            let pool = ready_pool(ctx, &volume.spec.pool)?;
            mark_ready(ctx, &volume, &pool).await;
        }
    }
    let mut new: Vm = Resource::new(item.spec.target_name.as_str(), spec);
    new.id = vm_id;
    let node = new.spec.node;
    create_vm(ctx, run.task, new, node, false, requested, warnings).await?;
    let at_time = now();
    let name = item.spec.target_name.clone();
    update_item(ctx, run.item, |s| {
        s.push_history(at_time, format!("Orbivisor VM {name} created"));
    })
    .await?;
    Ok(())
}

/// `starting-vm`: `start_after`면 켜고 `running`이 보일 때까지(한도 5분) 기다린 뒤 `done`을 적는다. 이 Task가 켜기를
/// 처음 청했고 `running`을 보기 전에 실패했을 때만 되돌림이다(게스트가 돌지 않았다). VM이 이미 켜졌거나 켜졌을 수 있으면
/// (이어 감, 손으로 켬) 되돌리지 않는다: 켜지 못해도 `done`에 오류를 남기고 VM·디스크를 둔다 — 되돌아갈 길은 되돌리기뿐이다.
/// `done` 쓰기가 실패하면(Raft) 그 걸음에 머문 채 다시 쓴다(리뷰 2).
async fn start_and_finish(
    ctx: &Ctx,
    migration: &VmwareMigration,
    id: Uuid,
    resumed: bool,
) -> Result<(), StepError> {
    let step = VmwareCutoverStep::StartingVm;
    if migration.spec.rules.start_after {
        let item = get_item(ctx, id).map_err(at(step))?;
        let vm = item
            .status
            .target_vm
            .ok_or_else(|| at(step)("the cutover has no Orbivisor VM"))?;
        let started_before = resumed
            || ctx
                .cluster
                .get::<VmSpec>(vm)
                .ok()
                .flatten()
                .is_some_and(|v| v.spec.power == PowerState::Running);
        match start_target(ctx, vm).await {
            Ok(()) => {}
            Err(e) if !started_before => return Err(at(step)(e)),
            Err(e) => {
                let message = format!(
                    "the Orbivisor VM {} may have run already but is not running now ({e}); it and its disks were kept: \
                     check it, then finalize, or revert to run the source VM again",
                    item.spec.target_name
                );
                finish(ctx, id, Some(&message)).await.map_err(at(step))?;
                return Err(StepError {
                    step,
                    message: format!("cutover finished at starting-vm: {message}"),
                    recorded: true,
                });
            }
        }
    }
    finish(ctx, id, None).await.map_err(|e| StepError {
        step,
        message: format!("the cutover could not be recorded: {e}"),
        recorded: true,
    })
}

/// VM을 켜고 `running`이 보일 때까지(한도 `vmware_start_timeout`) 기다린다.
async fn start_target(ctx: &Ctx, vm: Uuid) -> Result<(), String> {
    set_power(ctx, vm, PowerState::Running).await?;
    let limit = ctx.config.vmware_start_timeout;
    match tokio::time::timeout(limit, wait_vm(ctx, vm, started)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("the Orbivisor VM did not start: {e}")),
        Err(_) => Err(format!(
            "the Orbivisor VM did not start within {}s",
            limit.as_secs()
        )),
    }
}

/// `done`을 적는다. 쓰기가 실패하면(Raft 쿼럼 없음 등) 항목이 아직 전환 중인 동안 간격을 늘려 가며 다시 쓴다(Task는 이어
/// 가는 Task라 opld가 다시 떠도 이 걸음부터 잇는다). 항목이 지워졌거나 다른 상태면 그 오류를 돌려준다.
async fn finish(ctx: &Ctx, id: Uuid, error: Option<&str>) -> Result<(), String> {
    let mut wait = Duration::from_secs(1);
    loop {
        match record_done(ctx, id, error).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let cutting = match ctx.cluster.get::<VmwareVmSpec>(id) {
                    Ok(Some(i)) => i.status.state == VmwareVmState::CuttingOver,
                    Ok(None) => false,
                    Err(_) => true,
                };
                if !cutting {
                    return Err(e);
                }
                tracing::warn!(item = %id, error = %e, "cannot record the finished cutover; retrying");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// `done`: 내린 시간(원본 끄기 시작 → 대상 켜짐)을 적고 감사 로그에 남긴다(`error`면 켜지지 않은 VM의 오류도).
async fn record_done(ctx: &Ctx, id: Uuid, error: Option<&str>) -> Result<(), String> {
    let at_time = now();
    let item = update_item(ctx, id, |s| {
        let downtime = s.source_stopped_at.map(|t| at_time.saturating_sub(t));
        s.state = VmwareVmState::Done;
        s.cutover_step = None;
        s.downtime_s = downtime;
        s.error = error.map(str::to_string);
        s.next_sync_at = None;
        s.estimate_s = None;
        s.push_history(
            at_time,
            format!(
                "cut over; downtime {}",
                duration_text(downtime.unwrap_or(0))
            ),
        );
        if error.is_some() {
            s.push_history(at_time, "the Orbivisor VM is not running; it was kept");
        }
    })
    .await?;
    recount(ctx, item.spec.migration).await;
    audit(
        ctx,
        "vmware-vm.cutover",
        &item.name,
        format!(
            "vmware-vm cutover {} (downtime {})",
            item.name,
            duration_text(item.status.downtime_s.unwrap_or(0))
        ),
    );
    Ok(())
}

/// 원본을 끄기 전에(또는 끄다가) 멈춘 전환: 바꾼 원본 설정을 되돌리고 앞 상태(따뜻한 `syncing`, 차가운 `ready`)로 둔다.
async fn stop_before_source(ctx: &Ctx, id: Uuid, warm: bool, message: &str) -> Result<(), String> {
    let item = get_item(ctx, id)?;
    let restored = if item.status.source_changes.is_empty() {
        Ok(())
    } else {
        match source_session(ctx, &item).await {
            Ok((session, vm_ref)) => restore_source(ctx, &session, &vm_ref, id).await,
            Err(e) => Err(e),
        }
    };
    if let Err(e) = restored {
        return fail_rollback(ctx, id, VmwareCutoverStep::StoppingSource, message, &e).await;
    }
    let at_time = now();
    let every = get_migration(ctx, item.spec.migration)
        .map(|m| super::sync_every(ctx, &m))
        .unwrap_or(3600);
    let item = update_item(ctx, id, |s| {
        s.state = if warm {
            VmwareVmState::Syncing
        } else {
            VmwareVmState::Ready
        };
        s.cutover_step = None;
        s.source_stopped_at = None;
        if warm {
            // 보낸 게스트 종료가 남았으면 곧 다시 본다(늦게 꺼진 원본을 켠다).
            let wait = if s.shutdown_sent_at.is_some() {
                every.min(SHUTDOWN_RECHECK_S)
            } else {
                every
            };
            let next = at_time + wait;
            s.next_sync_at = Some(s.next_sync_at.map_or(next, |t| t.min(next)));
        }
        s.error = Some(message.to_string());
        s.push_history(at_time, "cutover stopped; the source VM keeps running");
    })
    .await?;
    recount(ctx, item.spec.migration).await;
    Ok(())
}

/// 자동 되돌림(§10.2): 대상 VM 지움(볼륨은 `pending`으로) → 원본 NIC·설명 되돌림 → 우리가 끈 원본을 켬 → `rolled-back`.
/// 되돌림이 실패하면 `failed`와 손으로 할 일.
async fn roll_back(
    ctx: &Ctx,
    id: Uuid,
    step: VmwareCutoverStep,
    message: &str,
) -> Result<Option<String>, String> {
    let item = get_item(ctx, id)?;
    let undone = async {
        remove_target_vm(ctx, id).await?;
        if !get_item(ctx, id)?.status.source_changes.is_empty() {
            let (session, vm_ref) = source_session(ctx, &item).await?;
            restore_source(ctx, &session, &vm_ref, id).await?;
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(e) = undone {
        fail_rollback(ctx, id, step, message, &e).await?;
        return Err(format!("{message}; rollback failed: {e}"));
    }
    let migration = get_migration(ctx, item.spec.migration).ok();
    let warm = migration
        .as_ref()
        .is_none_or(|m| m.spec.mode == VmwareMigrationMode::Warm);
    // 켠 대상 VM의 게스트가 대상 볼륨에 썼다(저널·로그): 따뜻한 이전의 다음 델타는 원본이 바꾼 곳만 옮기므로 두 갈래가 섞인다.
    // 대상 내용을 버리고 처음부터 다시 복사한다(리뷰 1). 차가운 이전은 같은 갈래(한 번 켠 디스크)라 그대로 둔다.
    let booted = step == VmwareCutoverStep::StartingVm
        && migration.as_ref().is_none_or(|m| m.spec.rules.start_after);
    let discard = warm && booted;
    let mut note = None;
    if discard && let Err(e) = discard_target(ctx, id).await {
        // 파일이 남아도 다음 전체 복사가 빈 곳을 0으로 쓴다(새 볼륨이 아니면).
        note = Some(format!("cannot delete the target volume files: {e}"));
    }
    let at_time = now();
    let item = update_item(ctx, id, |s| {
        if discard {
            s.state = VmwareVmState::Copying;
            s.rounds = 0;
            s.estimate_s = None;
            for d in &mut s.disks {
                d.change_id = None;
                d.copied_bytes = 0;
                d.rounds = 0;
                d.last_delta_bytes = 0;
            }
        } else {
            s.state = VmwareVmState::RolledBack;
        }
        s.cutover_step = None;
        s.source_stopped_at = None;
        // 원본이 다시 켜져 바뀐 것은 다음 델타가 옮긴다.
        s.next_sync_at = (warm || discard).then_some(at_time);
        s.error = Some(if discard {
            format!(
                "{message}; rolled back (the source VM was restored); the Orbivisor VM had started, so its disks are copied again in full"
            )
        } else {
            format!("{message}; rolled back (the source VM was restored)")
        });
        s.push_history(at_time, format!("rolled back after {}", step_name(step)));
        if discard {
            s.push_history(at_time, "target disks discarded; copying again in full");
        }
        if let Some(n) = &note {
            s.push_history(at_time, n.clone());
        }
    })
    .await?;
    recount(ctx, item.spec.migration).await;
    audit(
        ctx,
        "vmware-vm.rollback",
        &item.name,
        format!("vmware-vm rollback {} ({message})", item.name),
    );
    Ok(None)
}

/// 대상 볼륨 파일을 지운다(기록은 `pending`으로 남긴다 — 다음 동기화가 새로 만들어 전체로 복사한다).
async fn discard_target(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let item = get_item(ctx, id)?;
    for disk in &item.status.disks {
        let Some(vol) = disk.volume else { continue };
        let Some(v) = ctx
            .cluster
            .get::<VolumeSpec>(vol)
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let pool = ready_pool(ctx, &v.spec.pool)?;
        match ctx.storage.delete_volume(&pool, vol, v.spec.format).await {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(format!("{:?}: {e}", v.name)),
        }
    }
    Ok(())
}

/// 되돌림이 실패했다: `failed`(걸음은 남김 — 다시 하기가 되돌림을 다시 한다)와 손으로 할 일.
async fn fail_rollback(
    ctx: &Ctx,
    id: Uuid,
    step: VmwareCutoverStep,
    message: &str,
    error: &str,
) -> Result<(), String> {
    let item = get_item(ctx, id)?;
    let source = item
        .status
        .source
        .as_ref()
        .map_or_else(|| item.spec.source_ref.clone(), |s| s.vm.name.clone());
    let manual = manual_steps(
        &source,
        &item.status.source_changes,
        item.status.target_vm.is_some(),
        &item.spec.target_name,
    );
    let at_time = now();
    let text = format!(
        "{message}; rollback failed at {}: {error}; to restore by hand: {manual}",
        step_name(step)
    );
    let item = update_item(ctx, id, |s| {
        s.state = VmwareVmState::Failed;
        s.cutover_step = Some(step);
        s.error = Some(text.clone());
        s.push_history(at_time, "rollback failed");
    })
    .await?;
    recount(ctx, item.spec.migration).await;
    audit(
        ctx,
        "vmware-vm.rollback",
        &item.name,
        format!("vmware-vm rollback {} failed: {error}", item.name),
    );
    Ok(())
}

/// 되돌림이 남긴 손으로 할 일
fn manual_steps(
    source: &str,
    changes: &VmwareSourceChanges,
    target: bool,
    target_name: &str,
) -> String {
    let mut steps = Vec::new();
    if target {
        steps.push(format!("stop and delete Orbivisor VM {target_name}"));
    }
    if !changes.nics_disconnected.is_empty() {
        steps.push(format!(
            "turn \"Connect At Power On\" back on for the NICs of {source} in vCenter"
        ));
    }
    if changes.annotation_added {
        steps.push(format!("restore the notes of {source}"));
    }
    if changes.powered_off_by_us {
        steps.push(format!("power on {source} in vCenter"));
    }
    if steps.is_empty() {
        steps.push("nothing is left to undo; retry".into());
    }
    steps.join(", then ")
}

// ---- vmware-revert ----

/// `vmware-revert` Task(설계 §10.3): `done` 항목의 대상 VM을 끄고(지우지 않음), 원본 NIC·설명을 되돌리고 원본을 켠 뒤,
/// 우리가 켠 CBT를 끈다 → `reverted`. 대상에서 쓴 데이터는 원본으로 가지 않는다. 실패하면 `done`에 오류를 남긴다(다시 하면
/// 남은 것부터 한다).
pub(in crate::tasks) async fn revert(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let id = task.spec.target;
    let item = get_item(ctx, id)?;
    if item.status.state != VmwareVmState::Done {
        return Ok(Some(format!(
            "skipped: vmware-vm {:?} is {}",
            item.name,
            item.status.state.as_str()
        )));
    }
    let result = async {
        if let Some(vm) = item.status.target_vm
            && !vm_gone(ctx, vm)
        {
            stop_vm(ctx, vm).await?;
        }
        let (session, vm_ref) = source_session(ctx, &item).await?;
        // 끄지 않은 원본(손으로 켰다 등)도 되돌리면 켜진다.
        update_item(ctx, id, |s| s.source_changes.powered_off_by_us = true).await?;
        restore_source(ctx, &session, &vm_ref, id).await?;
        let s = get_item(ctx, id)?.status;
        for snap in [&s.pending_snapshot, &s.snapshot].into_iter().flatten() {
            remove_ours(&session, snap)
                .await
                .map_err(|e| format!("cannot remove snapshot {}: {e}", snap.name))?;
        }
        if s.cbt_enabled_by_us {
            match session.set_change_tracking(&vm_ref, false).await {
                Ok(()) | Err(VsphereError::NotFound { .. }) => {}
                Err(e) => return Err(format!("cannot turn changed block tracking off: {e}")),
            }
        }
        Ok::<_, String>(())
    }
    .await;
    let at_time = now();
    match result {
        Ok(()) => {
            let item = update_item(ctx, id, |s| {
                s.state = VmwareVmState::Reverted;
                s.snapshot = None;
                s.pending_snapshot = None;
                s.cbt_enabled_by_us = false;
                s.error = None;
                s.push_history(
                    at_time,
                    "reverted: the source VM runs again; writes made on the Orbivisor VM were not copied back",
                );
            })
            .await?;
            recount(ctx, item.spec.migration).await;
            audit(
                ctx,
                "vmware-vm.revert",
                &item.name,
                format!("vmware-vm revert {}", item.name),
            );
            Ok(None)
        }
        Err(e) => {
            let message = format!("revert failed: {e}");
            let m = message.clone();
            update_item(ctx, id, |s| {
                s.error = Some(m.clone());
                s.push_history(at_time, "revert failed");
            })
            .await?;
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_well() {
        assert_eq!(duration_text(42), "42s");
        assert_eq!(duration_text(252), "4m12s");
        assert_eq!(duration_text(3725), "1h2m5s");
    }

    #[test]
    fn manual_steps_list_what_is_left() {
        let changes = VmwareSourceChanges {
            powered_off_by_us: true,
            nics_disconnected: vec![4000],
            annotation_added: false,
            old_annotation: None,
        };
        let text = manual_steps("web01", &changes, true, "web01");
        assert_eq!(
            text,
            "stop and delete Orbivisor VM web01, then turn \"Connect At Power On\" back on for the NICs of web01 in vCenter, then power on web01 in vCenter"
        );
        assert_eq!(
            manual_steps("web01", &VmwareSourceChanges::default(), false, "x"),
            "nothing is left to undo; retry"
        );
    }

    #[test]
    fn a_late_shutdown_is_powered_on_only_within_the_watch() {
        let sent = 1_000;
        assert_eq!(shutdown_watch(sent, sent + 60, false), ShutdownWatch::Wait);
        assert_eq!(
            shutdown_watch(sent, sent + 60, true),
            ShutdownWatch::PowerOn
        );
        let late = sent + SHUTDOWN_WATCH_S;
        assert_eq!(
            shutdown_watch(sent, late, true),
            ShutdownWatch::Expired { off: true }
        );
        assert_eq!(
            shutdown_watch(sent, late, false),
            ShutdownWatch::Expired { off: false }
        );
    }

    #[test]
    fn steps_after_stopping_the_source_roll_back() {
        assert!(!VmwareCutoverStep::Preparing.rolls_back());
        assert!(!VmwareCutoverStep::StoppingSource.rolls_back());
        for s in [
            VmwareCutoverStep::FinalSync,
            VmwareCutoverStep::CreatingVm,
            VmwareCutoverStep::StartingVm,
        ] {
            assert!(s.rolls_back(), "{}", step_name(s));
        }
    }
}
