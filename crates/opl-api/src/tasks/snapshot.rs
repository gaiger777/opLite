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
            fetch_vars(ctx, &vm).await;
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
