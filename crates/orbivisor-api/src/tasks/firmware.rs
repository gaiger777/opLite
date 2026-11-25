//! 펌웨어 상태(UEFI 변수·TPM) Task 도우미(계획 59 §5·§10): 스냅샷·백업이 담을 TPM 상태를 이 노드에 맞추기, 상태 파일을
//! 0600으로 두기, 초기화 Task(`vm-firmware-reset`), 복제 회차 끝의 사본 밀어 두기, 지운 VM의 상태 디렉터리 정리.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_driver::{
    KeepFirmware, adopt_firmware_epoch, read_firmware_epoch, tpm_state_path, uefi_vars_path,
    vm_state_dir, vm_tpm_dir,
};
use orbivisor_peer::sha256_hex;
use orbivisor_reconciler::{FirmwareCopy, FirmwarePurge};
use orbivisor_types::firmware::{
    TpmSource, firmware_copy_nodes, tpm_is_current, tpm_source, tpm_unavailable, vars_source,
};
use orbivisor_types::node::{Node, NodeSpec};
use orbivisor_types::placement::HaGroupSpec;
use orbivisor_types::task::{Task, TaskSpec};
use orbivisor_types::vm::{PowerState, Vm, VmSpec, VmStatus};
use uuid::Uuid;

use super::Ctx;
use super::snapshot::{get_vm, is_down};

/// 지운 VM의 상태 디렉터리는 마지막으로 바뀐 지 이만큼 지나야 지운다(설계 §10). 복원·복제는 VM을 만들기 전에 그
/// 디렉터리에 파일을 쓴다.
pub const STATE_DIR_MIN_AGE: Duration = Duration::from_secs(3600);

/// 스냅샷의 TPM 상태 사본 `{state_dir}/vm/{vm}/snapshots/{snapshot}.tpm`
pub(super) fn tpm_copy(ctx: &Ctx, vm: Uuid, snapshot: Uuid) -> Option<PathBuf> {
    let dir = ctx.config.state_dir.as_ref()?;
    Some(
        vm_state_dir(dir, vm)
            .join("snapshots")
            .join(format!("{snapshot}.tpm")),
    )
}

/// 파일의 SHA-256(16진수 소문자). 파일이 없으면 None이다.
pub(super) async fn file_hash(path: &Path) -> io::Result<Option<String>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// `from`을 `to`에 0600으로 둔다(임시 파일에 쓰고 rename). 디렉터리가 없으면 0700으로 만든다. 둔 내용의 SHA-256을
/// 돌려준다. TPM 상태는 봉인한 비밀을 지키는 키라서 다른 사용자가 읽지 못하게 한다.
pub(super) async fn place_private(from: &Path, to: &Path) -> io::Result<String> {
    let bytes = tokio::fs::read(from).await?;
    place_private_bytes(bytes, to).await
}

/// [`place_private`]와 같되 내용 `bytes`를 둔다(저장소 백업의 TPM 블롭 — 계획 68).
pub(super) async fn place_private_bytes(bytes: Vec<u8>, to: &Path) -> io::Result<String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let (to, sha256) = (to.to_path_buf(), sha256_hex(&bytes));
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        use std::io::Write;
        if let Some(dir) = to.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let tmp = to.with_extension("part");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &to)
    })
    .await
    .map_err(io::Error::other)??;
    Ok(sha256)
}

/// 노드의 이름(없으면 id)
fn node_name(nodes: &[Node], id: Uuid) -> String {
    nodes
        .iter()
        .find(|n| n.id == id)
        .map_or_else(|| id.to_string(), |n| n.name.clone())
}

/// TPM VM의 지금 TPM 상태를 이 노드에 맞추고(설계 §5의 켜기 규칙과 같다) 그 파일을 돌려준다. 스냅샷·백업이 담는다.
///
/// - TPM이 없는 VM이거나 `state_dir`가 없으면 None이다.
/// - 이 노드에서 QEMU가 돌면 swtpm이 이 노드의 파일을 쓴다: 그 파일이다.
/// - 이 노드가 `tpm_node`이거나 파일 해시가 기록과 같으면 이 노드의 파일이다. 기록된 상태가 있는데 파일이 없으면
///   오류다(상태를 잃었다 — 몰래 TPM 없이 담지 않는다).
/// - 기록된 해시가 없으면(한 번도 켜지 않음, TPM 초기화 뒤) 이 노드에 파일이 있고 이 노드의 회차가 VM의 회차와 같을
///   때만 그것이다(리뷰 2 #8: 회차가 다르면 초기화 전의 TPM이다).
/// - 아니면 `tpm_node`, 그다음 온라인 사본 노드에서 받아 온다(해시 확인). UEFI 변수도 받는다([`vars_source`]).
///   모두 실패하면 [`tpm_unavailable`] 문구로 실패한다.
pub(super) async fn current_tpm(ctx: &Ctx, vm: &Vm) -> Result<Option<PathBuf>, String> {
    let Some(dir) = &ctx.config.state_dir else {
        return Ok(None);
    };
    if !vm.spec.tpm {
        return Ok(None);
    }
    let path = tpm_state_path(dir, vm.id);
    let local = file_hash(&path)
        .await
        .map_err(|e| format!("cannot read the TPM state: {e}"))?;
    if matches!(ctx.hv.inspect(vm.id).await, Ok(Some(_))) {
        return Ok(local.map(|_| path));
    }
    let nodes = ctx.cluster.list::<NodeSpec>().map_err(|e| e.to_string())?.1;
    let from = match tpm_source(vm, ctx.config.node, local.as_deref(), &nodes) {
        TpmSource::Local if local.is_none() => {
            return Err(format!(
                "the TPM state of VM {:?} is missing on this node",
                vm.name
            ));
        }
        TpmSource::Local => return Ok(local.map(|_| path)),
        TpmSource::Fresh => {
            let current =
                matches!(read_firmware_epoch(dir, vm.id), Ok(e) if e == vm.status.firmware_epoch);
            return Ok(local.filter(|_| current).map(|_| path));
        }
        TpmSource::Unavailable(reason) => return Err(reason),
        TpmSource::Fetch(from) => from,
    };
    let fetcher = ctx
        .config
        .firmware
        .as_ref()
        .ok_or("this node cannot fetch the TPM state from other nodes")?;
    let hash = vm.status.tpm_sha256.clone().unwrap_or_default();
    for node in from {
        match fetcher
            .fetch_tpm(vm.id, node, &hash, vm.status.firmware_epoch)
            .await
        {
            Ok(()) => {
                if let Some(vars) = vars_source(vm, ctx.config.node, node, &nodes)
                    && let Err(e) = fetcher.fetch(vm.id, vars).await
                {
                    tracing::warn!(vm = %vm.id, from = %vars, error = %e, "cannot fetch UEFI variables with the TPM state");
                }
                return Ok(Some(path));
            }
            Err(e) => {
                tracing::warn!(vm = %vm.id, from = %node, error = %e, "cannot fetch the TPM state");
            }
        }
    }
    let holder = vm.status.tpm_node.map(|id| node_name(&nodes, id));
    Err(tpm_unavailable(&vm.name, holder.as_deref()))
}

/// 복제·배포가 원본의 TPM 상태를 넘기지 않을 때 Task에 남기는 경고(설계 §10)
pub(super) fn new_tpm_warning(new: &str, source: &str) -> String {
    format!("VM {new:?} got a new TPM; secrets sealed to the TPM of {source:?} do not carry over")
}

/// VM의 status를 `change`로 바꿔 쓴다. reconciler도 status를 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면
/// 다시 읽어 쓴다. `change`가 false를 돌려주면(바꿀 것이 없음) 쓰지 않는다.
pub(super) async fn update_vm_status(
    ctx: &Ctx,
    vm: Uuid,
    change: impl Fn(&mut VmStatus) -> bool,
) -> Result<(), String> {
    loop {
        let current = get_vm(ctx, vm)?;
        let mut status = current.status.clone();
        if !change(&mut status) {
            return Ok(());
        }
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

/// 초기화가 VM status에 쓸 것(설계 §5): 회차를 올린다. `me`는 Task가 도는 노드다.
///
/// - 변수: 어느 노드의 변수도 지금 것이 아니다 — `firmware_node`를 지운다(켤 때 템플릿에서 새로 만든다).
/// - TPM: 해시·`tpm_node`·사본을 지운다(켤 때 새로 만든다).
/// - 변수만: TPM 상태는 그대로라 `tpm_node`와 사본을 지킨다(리뷰 2 #3 — 지우면 TPM을 가진 노드를 잃는다). 사본 노드의
///   변수는 회차가 달라 그 노드가 주지 않는다.
/// - TPM만: 이 노드가 `firmware_node`면 이 노드의 변수가 새 회차로 남으므로 지킨다. 아니면(그 노드가 오프라인이라
///   여기서 돈다 — API가 409로 막는다, 리뷰 2 #10) 그 노드의 변수는 회차가 달라 켤 때 지워지므로 지운다. 사본은
///   낡은 TPM이라 비운다.
pub(super) fn reset_status(status: &mut VmStatus, me: Uuid, uefi_vars: bool, tpm: bool) {
    status.firmware_epoch = status.firmware_epoch.wrapping_add(1);
    if uefi_vars || status.firmware_node != Some(me) {
        status.firmware_node = None;
    }
    if tpm {
        status.tpm_sha256 = None;
        status.tpm_node = None;
        status.firmware_copies.clear();
    }
}

/// `vm-firmware-reset` Task(설계 §5): 꺼진 VM의 UEFI 변수(`uefi_vars`)·TPM 상태(`tpm`)를 초기화한다. 먼저 VM status에
/// 새 회차를 쓰고([`reset_status`]), 이 노드의 파일을 지운 뒤 이 노드의 회차 파일을 새 회차로 적는다(남는 파일이 켤 때
/// 지워지지 않게). 다른 노드의 낡은 파일은 회차가 달라 켤 때 지워지고, 남은 디렉터리는 정리가 지운다. 회차는 이 Task의
/// 쓰기가 기록된 뒤 다시 올리지 않는다.
pub(super) async fn reset(
    ctx: &Ctx,
    task: &Task,
    (uefi_vars, tpm): (bool, bool),
    secure_boot: Option<bool>,
) -> Result<(), String> {
    if !uefi_vars && !tpm {
        return Err("nothing to reset: choose the UEFI variables, the TPM or both".into());
    }
    let id = task.spec.target;
    let vm = get_vm(ctx, id)?;
    if vm.spec.power != PowerState::Stopped || !is_down(&vm) {
        return Err(format!(
            "VM {:?} is running; stop it before resetting its firmware",
            vm.name
        ));
    }
    if !matches!(ctx.hv.inspect(id).await, Ok(None)) {
        return Err(format!("VM {:?} is still running on this node", vm.name));
    }
    if let Some(on) = secure_boot {
        set_secure_boot(ctx, id, on).await?;
    }
    // 이 노드의 파일이 초기화 전 회차의 지금 것인지는 status를 바꾸기 전에 본다.
    let keep = match &ctx.config.state_dir {
        Some(dir) => {
            let local = file_hash(&tpm_state_path(dir, id)).await.ok().flatten();
            let me = ctx.config.node;
            KeepFirmware {
                vars: !uefi_vars
                    && matches!(read_firmware_epoch(dir, id), Ok(e) if e == vm.status.firmware_epoch),
                tpm: !tpm && tpm_is_current(&vm.status, me, local.as_deref()),
            }
        }
        None => KeepFirmware {
            vars: false,
            tpm: false,
        },
    };
    let epoch = write_reset(ctx, &vm, uefi_vars, tpm).await?;
    let Some(dir) = &ctx.config.state_dir else {
        return Ok(());
    };
    let gone = |r: io::Result<()>| match r {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if uefi_vars {
        gone(tokio::fs::remove_file(uefi_vars_path(dir, id)).await)
            .map_err(|e| format!("cannot remove the UEFI variables: {e}"))?;
    }
    if tpm {
        gone(tokio::fs::remove_dir_all(vm_tpm_dir(dir, id)).await)
            .map_err(|e| format!("cannot remove the TPM state: {e}"))?;
    }
    // 초기화하지 않은 파일도 이 노드의 회차가 초기화 전 VM의 회차와 달랐으면 더 낡은 것이다. 같은 규칙으로 지운 뒤 새
    // 회차를 적는다(리뷰 2 #1a — 회차만 적으면 낡은 파일이 새 회차의 것으로 보인다).
    adopt_firmware_epoch(dir, id, epoch, keep)
        .map_err(|e| format!("cannot record the firmware epoch: {e}"))?;
    tracing::info!(vm = %id, uefi_vars, tpm, epoch, "reset VM firmware");
    let purge = FirmwarePurge {
        vm: id,
        epoch: Some(epoch),
        uefi_vars,
        tpm,
        snapshot: None,
    };
    purge_elsewhere(ctx, &purge).await;
    Ok(())
}

/// 다른 온라인 노드(위트니스 빼고)에 남은 초기화 전 파일을 지운다(리뷰 1 I5). 옛 TPM 상태는 비밀이라 사본 노드와 앞
/// `firmware_node`에 남기지 않는다(어느 노드를 거쳐 갔는지 모두 기록하지 않으므로 모든 노드에 보낸다). 닿지 않는 노드는
/// 남기고 경고만 한다 — 그 노드의 파일은 회차가 달라 켤 때 지워진다.
async fn purge_elsewhere(ctx: &Ctx, purge: &FirmwarePurge) {
    let Some(fetcher) = &ctx.config.firmware else {
        return;
    };
    let Ok((_, nodes)) = ctx.cluster.list::<NodeSpec>() else {
        return;
    };
    for node in nodes
        .iter()
        .filter(|n| n.id != ctx.config.node && n.status.online && !n.spec.witness)
    {
        if let Err(e) = fetcher.purge(node.id, purge).await {
            tracing::warn!(vm = %purge.vm, node = %node.name, error = %e,
                "cannot remove old firmware files on another node");
        }
    }
}

/// 스펙의 `secure_boot`를 `on`으로 쓴다(이미 그 값이면 쓰지 않는다 — 다시 해도 된다). 그 사이 누가 스펙을 썼으면 다시
/// 읽어 쓴다.
async fn set_secure_boot(ctx: &Ctx, id: Uuid, on: bool) -> Result<(), String> {
    loop {
        let vm = get_vm(ctx, id)?;
        if vm.spec.secure_boot == on {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.secure_boot = on;
        spec.validate().map_err(|e| e.to_string())?;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 초기화를 VM status에 쓰고 새 회차를 돌려준다. 쓰기 결과를 모르면(시간 초과 등) 다시 읽어, 회차가 이 Task가 쓴
/// 값까지 올랐으면 다시 올리지 않는다(Task 하나에 한 번).
async fn write_reset(ctx: &Ctx, vm: &Vm, uefi_vars: bool, tpm: bool) -> Result<u32, String> {
    let me = ctx.config.node;
    let mut current = vm.clone();
    loop {
        let mut status = current.status.clone();
        reset_status(&mut status, me, uefi_vars, tpm);
        let epoch = status.firmware_epoch;
        match ctx
            .cluster
            .update_status_checked::<VmSpec>(vm.id, current.revision, &status)
            .await
        {
            Ok(_) => return Ok(epoch),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                current = get_vm(ctx, vm.id)?;
            }
            Err(ClusterError::Rejected(r)) => return Err(r.to_string()),
            Err(e) => {
                tracing::warn!(vm = %vm.id, error = %e, "cannot record the firmware reset; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                let now = get_vm(ctx, vm.id)?;
                if now.revision > current.revision && now.status.firmware_epoch == epoch {
                    return Ok(epoch);
                }
                current = now;
            }
        }
    }
}

/// 스냅샷의 변수·TPM 사본을 다른 노드(`tpm_node`)에 두었으면 거기서 지운다(리뷰 1 I5). 그 노드가 오프라인이거나
/// 클러스터에 없으면 경고만 남긴다(스냅샷은 지운다). 온라인인데 지우지 못하면 실패다(다시 하면 된다).
pub(super) async fn purge_snapshot_copy(
    ctx: &Ctx,
    vm: Uuid,
    snapshot: Uuid,
    node: Uuid,
) -> Result<(), String> {
    if node == ctx.config.node {
        return Ok(());
    }
    let online = matches!(ctx.cluster.get::<NodeSpec>(node), Ok(Some(n)) if n.status.online);
    let purge = FirmwarePurge {
        vm,
        snapshot: Some(snapshot),
        ..Default::default()
    };
    let result = match &ctx.config.firmware {
        Some(fetcher) => fetcher.purge(node, &purge).await.map_err(|e| e.to_string()),
        None => Err("this node cannot remove files on other nodes".into()),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if online => Err(format!(
            "cannot remove the TPM copy of the snapshot on node {node}: {e}"
        )),
        Err(e) => {
            tracing::warn!(%vm, %snapshot, %node, error = %e,
                "the TPM copy of a deleted snapshot stays on an offline node");
            Ok(())
        }
    }
}

/// 로컬 디스크 복제 회차가 끝났다: 대상 노드 `to`가 VM의 펌웨어 사본 대상인데 지금 TPM 상태의 사본이 없으면 밀어
/// 두고 `firmware_copies`에 더한다(설계 §10, 복제본으로 하는 HA 복구가 그 노드의 사본으로 켠다). 이 노드가
/// `firmware_node`일 때만 한다. TPM 상태가 기록 뒤에 바뀌었으면 보내지 않는다 — 작업자가 해시를 다시 적고 사본 대상
/// (복제 대상이 먼저)에 다시 밀어 둔다. 실패는 로그만 남긴다(회차는 성공이다).
pub(super) async fn push_replica_copy(ctx: &Ctx, vm: Uuid, to: Uuid) {
    let Some(fetcher) = &ctx.config.firmware else {
        return;
    };
    let Ok(Some(current)) = ctx.cluster.get::<VmSpec>(vm) else {
        return;
    };
    let tpm = current.spec.tpm || current.status.hardware.as_ref().is_some_and(|hw| hw.tpm);
    let Some(hash) = current.status.tpm_sha256.clone() else {
        return;
    };
    if !tpm
        || current.status.firmware_node != Some(ctx.config.node)
        || current.status.firmware_copies.contains(&to)
    {
        return;
    }
    let Ok((_, nodes)) = ctx.cluster.list::<NodeSpec>() else {
        return;
    };
    let group = current
        .spec
        .ha
        .as_ref()
        .and_then(|ha| ha.group.as_deref())
        .and_then(|name| ctx.cluster.get_by_name::<HaGroupSpec>(name).ok().flatten());
    if !firmware_copy_nodes(&current, &nodes, group.as_ref().map(|g| &g.spec)).contains(&to) {
        return;
    }
    let epoch = current.status.firmware_epoch;
    let copy = FirmwareCopy {
        vm,
        to,
        sha256: hash.clone(),
        epoch,
        secure_boot: current.spec.secure_boot,
    };
    if let Err(e) = fetcher.push(&copy).await {
        tracing::info!(vm = %vm, to = %to, error = %e, "cannot push the firmware copy after the replication run; the VM's worker will");
        return;
    }
    let added = update_vm_status(ctx, vm, |status| {
        let same = status.tpm_sha256.as_deref() == Some(hash.as_str())
            && status.firmware_epoch == epoch
            && !status.firmware_copies.contains(&to);
        if same {
            status.firmware_copies.push(to);
        }
        same
    })
    .await;
    if let Err(e) = added {
        tracing::warn!(vm = %vm, to = %to, error = %e, "cannot record the firmware copy");
    }
}

/// 정리의 리더 확인 읽기를 기다리는 시간
const SWEEP_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 앞 정리 차례에 본 후보([`sweep_state_dirs`])
#[derive(Debug, Default)]
pub(super) struct SweepSeen {
    /// Raft에 VM이 없는 상태 디렉터리
    gone: HashSet<Uuid>,
    /// 이 노드의 회차가 VM의 회차(값)와 다르고 지금 것이 아닌 TPM 상태가 남은 VM(리뷰 2 #7)
    stale_tpm: HashSet<(Uuid, u32)>,
}

/// 지운 VM의 상태 디렉터리와 초기화 전 TPM 상태를 `tag_sweep`마다 지운다([`sweep_state_dirs`]). 시작할 때는 후보만 모으고,
/// 다음 차례에도 후보이면 지운다(리뷰 1 I3): 다시 합류한 노드의 로컬 클러스터 상태가 아직 따라잡지 못해 VM 목록이 모자라도
/// 그 사이에 따라잡으므로, 살아 있는 VM의 TPM 상태·UEFI 변수를 지우지 않는다. 실행기가 멈추면 함께 멈춘다.
pub(super) async fn sweep_state_dirs_every(ctx: Arc<Ctx>) {
    let mut seen = SweepSeen::default();
    loop {
        seen = sweep_state_dirs(&ctx, &seen).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드의 `{state_dir}/vm/{id}`를 정리한다. 앞 차례(`before`)에도 같은 후보였던 것만 지우고 이번 후보를 돌려준다.
///
/// - Raft에 VM이 없는 디렉터리(설계 §10): 마지막으로 바뀐 지 `state_dir_min_age`(1시간)가 지났고 그 id를 대상으로 하는
///   끝나지 않은 Task가 없으면 지운다(복원·복제는 VM을 만들기 전에 파일을 쓴다). 사용자가 VM을 지운 뜻대로 거쳐 간 노드의
///   UEFI 변수·TPM 상태(비밀)를 남기지 않는다.
/// - VM이 있으면 디렉터리는 두되, 이 노드의 회차가 VM의 회차와 달라(오프라인 동안 초기화됨) 남은 TPM 상태가 지금 것이
///   아니면([`tpm_is_current`]) 그 TPM 상태만 지운다(리뷰 2 #7 — 그 VM이 이 노드에서 켜지기 전까지 비밀이 남지 않게).
///   VM이 이 노드에서 돌거나 끝나지 않은 Task의 대상이면 두고, 회차 파일을 읽지 못하면 모르는 것이라 둔다.
///
/// 목록을 읽기 전에 리더가 확인한 읽기([`orbivisor_cluster::Cluster::read_index`])로 따라잡았는지 본다(리뷰 2 #6). 리더에 닿지
/// 못하거나 목록을 읽지 못하면 후보만 모으고 아무것도 지우지 않는다.
pub(super) async fn sweep_state_dirs(ctx: &Ctx, before: &SweepSeen) -> SweepSeen {
    let mut found = SweepSeen::default();
    let Some(state) = &ctx.config.state_dir else {
        return found;
    };
    let root = state.join("vm");
    let mut entries = match tokio::fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return found,
        Err(e) => {
            tracing::warn!(dir = %root.display(), error = %e, "cannot read VM state directories");
            return found;
        }
    };
    let caught_up = match ctx.cluster.read_index(SWEEP_READ_TIMEOUT).await {
        Ok(_) => true,
        Err(e) => {
            tracing::info!(error = %e, "cannot confirm the cluster state with the leader; not removing VM state");
            false
        }
    };
    let (Ok((_, vms)), Ok((_, tasks))) =
        (ctx.cluster.list::<VmSpec>(), ctx.cluster.list::<TaskSpec>())
    else {
        return found;
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => return found,
            Err(e) => {
                tracing::warn!(dir = %root.display(), error = %e, "cannot read VM state directories");
                return found;
            }
        };
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<Uuid>().ok())
        else {
            continue;
        };
        if tasks
            .iter()
            .any(|t| t.spec.target == id && !t.status.state.is_finished())
        {
            continue;
        }
        if let Some(vm) = vms.iter().find(|vm| vm.id == id) {
            if let Some(key) = stale_tpm(ctx, state, vm).await {
                found.stale_tpm.insert(key);
                if caught_up && before.stale_tpm.contains(&key) {
                    wipe_stale_tpm(ctx, state, id, key.1).await;
                }
            }
            continue;
        }
        let Ok(meta) = tokio::fs::symlink_metadata(entry.path()).await else {
            continue;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok());
        if !meta.is_dir() || age.is_none_or(|a| a < ctx.config.state_dir_min_age) {
            continue;
        }
        found.gone.insert(id);
        if !caught_up || !before.gone.contains(&id) {
            continue;
        }
        // 목록을 읽은 뒤 만든 VM일 수 있다. 지우기 바로 전에 다시 본다.
        if !matches!(ctx.cluster.get::<VmSpec>(id), Ok(None)) {
            continue;
        }
        match tokio::fs::remove_dir_all(entry.path()).await {
            Ok(()) => {
                found.gone.remove(&id);
                tracing::info!(vm = %id, "removed the state directory of a deleted VM");
            }
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot remove the state directory of a deleted VM");
            }
        }
    }
}

/// 이 노드에 VM `vm`의 초기화 전 TPM 상태가 남았으면 (VM id, VM의 회차): TPM 디렉터리가 있고, 이 노드의 회차를 읽었고
/// VM의 회차와 다르며, 이 노드의 파일이 지금 것이 아니고([`tpm_is_current`]), 이 노드에서 VM이 돌지 않는다.
async fn stale_tpm(ctx: &Ctx, state: &Path, vm: &Vm) -> Option<(Uuid, u32)> {
    let epoch = vm.status.firmware_epoch;
    if !tokio::fs::try_exists(vm_tpm_dir(state, vm.id))
        .await
        .unwrap_or(false)
        || !matches!(read_firmware_epoch(state, vm.id), Ok(e) if e != epoch)
    {
        return None;
    }
    let local = file_hash(&tpm_state_path(state, vm.id)).await.ok()?;
    if tpm_is_current(&vm.status, ctx.config.node, local.as_deref())
        || !matches!(ctx.hv.inspect(vm.id).await, Ok(None))
    {
        return None;
    }
    Some((vm.id, epoch))
}

/// 지우기 바로 전에 VM을 다시 읽어 아직 같은 회차의 낡은 TPM 상태이면 지운다.
async fn wipe_stale_tpm(ctx: &Ctx, state: &Path, id: Uuid, epoch: u32) {
    let Ok(Some(vm)) = ctx.cluster.get::<VmSpec>(id) else {
        return;
    };
    if vm.status.firmware_epoch != epoch || stale_tpm(ctx, state, &vm).await.is_none() {
        return;
    }
    match tokio::fs::remove_dir_all(vm_tpm_dir(state, id)).await {
        Ok(()) => tracing::info!(vm = %id, "removed a TPM state from before a firmware reset"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(vm = %id, error = %e, "cannot remove a TPM state from before a firmware reset");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(node: Uuid) -> VmStatus {
        VmStatus {
            firmware_node: Some(node),
            tpm_node: Some(node),
            tpm_sha256: Some("ab".repeat(32)),
            firmware_copies: vec![Uuid::now_v7()],
            firmware_epoch: 2,
            ..Default::default()
        }
    }

    /// 둘 다 초기화하면 모두 지우고 회차를 올린다.
    #[test]
    fn reset_of_both_forgets_everything() {
        let me = Uuid::now_v7();
        let mut s = status(me);
        reset_status(&mut s, me, true, true);
        assert_eq!(s.firmware_epoch, 3);
        assert_eq!(s.tpm_sha256, None);
        assert_eq!(s.firmware_node, None);
        assert_eq!(s.tpm_node, None);
        assert!(s.firmware_copies.is_empty());
    }

    /// 변수만 초기화하면 TPM 상태의 노드(`tpm_node`)·해시·사본을 지킨다(리뷰 2 #3). 변수의 노드는 지운다.
    #[test]
    fn reset_of_the_vars_keeps_the_tpm_holder_and_copies() {
        let (me, a) = (Uuid::now_v7(), Uuid::now_v7());
        // TPM을 끈 채 me에서 돌았다: 변수는 me, TPM 상태는 a.
        let mut s = VmStatus {
            firmware_node: Some(me),
            tpm_node: Some(a),
            tpm_sha256: Some("ab".repeat(32)),
            firmware_copies: vec![a],
            firmware_epoch: 2,
            ..Default::default()
        };
        reset_status(&mut s, me, true, false);
        assert_eq!(s.firmware_node, None);
        assert_eq!(s.tpm_node, Some(a));
        assert_eq!(s.tpm_sha256, Some("ab".repeat(32)));
        assert_eq!(s.firmware_copies, [a]);
        assert_eq!(s.firmware_epoch, 3);
    }

    /// TPM만 초기화하면 이 노드가 변수를 가진 노드일 때만 그 자리를 지킨다.
    #[test]
    fn reset_of_the_tpm_keeps_the_vars_of_this_node() {
        let (me, other) = (Uuid::now_v7(), Uuid::now_v7());
        let mut here = status(me);
        reset_status(&mut here, me, false, true);
        assert_eq!(here.firmware_node, Some(me));
        assert_eq!(here.tpm_sha256, None);
        assert_eq!(here.tpm_node, None);
        assert!(here.firmware_copies.is_empty());

        let mut elsewhere = status(other);
        reset_status(&mut elsewhere, me, false, true);
        assert_eq!(elsewhere.firmware_node, None);
    }

    #[tokio::test]
    async fn private_files_are_0600_in_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::write(&from, b"tpm").unwrap();
        let to = dir.path().join("new/tpm/tpm2-00.permall");
        let hash = place_private(&from, &to).await.unwrap();
        assert_eq!(hash, sha256_hex(b"tpm"));
        assert_eq!(std::fs::read(&to).unwrap(), b"tpm");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&to), 0o600);
        assert_eq!(mode(to.parent().unwrap()), 0o700);
        assert_eq!(file_hash(&to).await.unwrap(), Some(hash));
        assert_eq!(file_hash(&dir.path().join("none")).await.unwrap(), None);
    }
}
