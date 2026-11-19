//! VM 복제 Task(설계 §5 "복제"). 원본 VM의 줄에 서서 그 VM의 스냅샷 Task와 차례로 돈다. API가
//! 확인한 뒤 줄에서 기다리는 사이 원본 VM이나 스냅샷이 바뀌거나 없어졌을 수 있으므로 다시 확인한다.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use opl_cluster::ClusterError;
use opl_driver::uefi_vars_path;
use opl_types::cloud_init::IpConfig;
use opl_types::node::{NodeSpec, PLACEMENT, TEMPLATES};
use opl_types::placement::HaGroupSpec;
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotPhase;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec, pool_error_on, pool_listed_on,
    pool_supports_linked_clones, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::template::{CloneOverrides, apply_overrides, grown_disks};
use opl_types::vm::{MacAddr, PowerState, Vm, VmSpec};
use uuid::Uuid;

use super::firmware::new_tpm_warning;
use super::snapshot::{
    check_local, check_settled, copy_file, fetch_vars, get_snapshot, get_vm, is_down, ready_disks,
    vars_copy,
};
use super::{Ctx, mark_ready, remove_volume};
use crate::scheduler::{Inventory, node_fits, schedule};
use crate::volumes::block_pool_kind;

/// 리소스 이름의 최대 길이
const MAX_NAME: usize = 63;

/// 복제(와 복원) Task가 지금까지 만든 것. Task가 실패하거나 시간이 다 되면 실행기의 정리([`discard`])가
/// 지운다. opld가 죽으면 사라진다. 복사 중이던 볼륨은 아직 `pending`이라 다음 시작의 고아 정리가
/// 지우지만(볼륨은 모두 복사한 뒤 VM을 만들기 바로 전에 `ready`로 바꾼다), `ready`로 바꾼 뒤 VM을
/// 만들기 전에 죽으면 볼륨과 UEFI 변수 사본이 남는다(설계 §11).
#[derive(Default)]
pub(super) struct Made {
    pub(super) volumes: Vec<Uuid>,
    pub(super) vars: Option<PathBuf>,
    /// 새 VM의 TPM 상태 디렉터리(복원, 계획 59)
    pub(super) tpm: Option<PathBuf>,
}

pub(super) fn made(ctx: &Ctx, task: Uuid, f: impl FnOnce(&mut Made)) {
    f(ctx.cloned.lock().unwrap().entry(task).or_default());
}

/// 새 볼륨 이름 `{vm}-{volume}`. 63자로 자르고, `taken`이면 `-2`, `-3`…을 붙인다(붙인 뒤에도 63자).
pub(crate) fn volume_name(vm: &str, volume: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{vm}-{volume}");
    // 이름은 ASCII다. 자른 끝이 `-`면 이름 규칙에 어긋나므로 뗀다.
    let cut = |max: usize| {
        base[..base.len().min(max)]
            .trim_end_matches('-')
            .to_string()
    };
    let first = cut(MAX_NAME);
    if !taken(&first) {
        return first;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            format!("{}{suffix}", cut(MAX_NAME - suffix.len()))
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 복사본의 형식(설계 §5): 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 담는다. 블록 볼륨(raw)을
/// 파일 풀로 옮기면 그 풀에서 스냅샷을 담을 수 있는 qcow2로 바꾼다. 그 밖에는 원본 형식 그대로다.
pub(super) fn copy_format(
    source: &StoragePool,
    target: &StoragePool,
    format: ImageFormat,
) -> ImageFormat {
    match (
        block_pool_kind(source).is_some(),
        block_pool_kind(target).is_some(),
    ) {
        (_, true) => ImageFormat::Raw,
        (true, false) => ImageFormat::Qcow2,
        (false, false) => format,
    }
}

fn pool_by_name(ctx: &Ctx, name: &str) -> Result<StoragePool, String> {
    ctx.cluster
        .get_by_name::<StoragePoolSpec>(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {name:?} not found"))
}

/// 원본(스냅샷이나 꺼진 VM의 지금 상태)의 디스크를 이 노드에서 새 볼륨으로 복사하고 UEFI 변수를
/// 복사한 뒤 새 VM을 만든다. 새 VM은 꺼져 있고 노드·예약·스냅샷이 없으며 MAC은 새로 받는다. TPM 상태는 복사하지
/// 않는다: 새 VM은 처음 켤 때 새 TPM을 받고, 원본에 TPM 상태가 있었으면 그 경고를 남긴다(계획 59 §10).
///
/// 템플릿에서 배포하면(계획 58 §6) `linked`에 따라 디스크마다 연결 복제(기반을 함께 쓰는 볼륨, 기록에 `base`)나 전체
/// 복사를 하고, `overrides`(vCPU·메모리·노드·NIC 네트워크·디스크 크기·cloud-init)를 적용하고, `start`면 놓을 노드를
/// 골라 켠 채로 만든다. 연결하지 못해 전체 복사한 디스크, 켜지 못한 이유는 경고로 남긴다.
pub(super) async fn clone(
    ctx: &Ctx,
    task: &Task,
    name: &str,
    snapshot: Option<Uuid>,
    pool: Option<&str>,
    linked: Option<bool>,
    overrides: Option<&CloneOverrides>,
) -> Result<Option<String>, String> {
    let vm = get_vm(ctx, task.spec.target)?;
    // API가 확인했지만 줄에서 기다리는 사이 VM으로 되돌려졌을 수 있다.
    if linked == Some(true) && (!vm.spec.template || snapshot.is_some()) {
        return Err(LINKED_ONLY_FROM_TEMPLATES.into());
    }
    // 연결 복제를 해 볼지: 템플릿의 지금 상태에서, `linked`가 거짓이 아니면
    let link = vm.spec.template && snapshot.is_none() && linked != Some(false);
    let stopped = vm.spec.power == PowerState::Stopped && is_down(&vm);
    // 원본에 TPM 상태가 있었는지(새 VM은 늘 새 TPM이다, 계획 59 §10)
    let (spec, tag, had_tpm) = match snapshot {
        Some(id) => {
            let snapshot = get_snapshot(ctx, id)?
                .filter(|s| s.spec.vm == vm.id)
                .ok_or("snapshot was deleted")?;
            if snapshot.status.phase != SnapshotPhase::Ready {
                return Err("snapshot is not ready".into());
            }
            check_settled(&vm)?;
            let had_tpm = snapshot.status.tpm_node.is_some();
            (snapshot.spec.vm_spec, Some(id.to_string()), had_tpm)
        }
        None if !stopped => return Err("VM is running; stop it or clone from a snapshot".into()),
        None => (vm.spec.clone(), None, vm.status.tpm_sha256.is_some()),
    };
    // 실행 중인 VM의 디스크는 QEMU가 쥐고 있다. 스냅샷 데이터는 바뀌지 않으므로 잠금을 무시하고 읽는다.
    let force_share = tag.is_some() && !stopped;
    if ctx
        .cluster
        .get_by_name::<VmSpec>(name)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(format!("VM {name:?} already exists"));
    }
    let disks = ready_disks(ctx, &spec)?;
    // 늘릴 디스크 크기(디스크 순번별). 원본 볼륨보다 작으면 거절한다.
    let grown = match overrides {
        Some(ov) => {
            let sizes: Vec<u64> = disks.iter().map(|(v, _)| v.spec.size_bytes).collect();
            grown_disks(ov, &sizes).map_err(|e| e.to_string())?
        }
        None => vec![None; disks.len()],
    };
    let mut targets = Vec::with_capacity(disks.len());
    for (volume, source_pool) in &disks {
        check_local(ctx, volume, source_pool)?;
        let target = match pool {
            Some(p) => pool_by_name(ctx, p)?,
            None => source_pool.clone(),
        };
        if !pool_listed_on(&target, ctx.config.node) {
            return Err(format!(
                "storage pool {:?} is not available on this node",
                target.name
            ));
        }
        // 마운트되지 않은 NFS 풀의 빈 마운트 지점에 복사본을 만들지 않는다.
        let me = ctx
            .cluster
            .get::<NodeSpec>(ctx.config.node)
            .map_err(|e| e.to_string())?
            .ok_or("this node is not registered")?;
        if !pool_usable_on(&target, &me) {
            return Err(format!(
                "storage pool {:?} is not ready on this node: {}",
                target.name,
                pool_error_on(&target, &me)
            ));
        }
        targets.push(target);
    }

    // 디스크마다 연결 복제로 만들지 정한다. `linked: true`면 하나라도 안 되면 실패, 없으면 전체 복사하고 경고를 남긴다.
    let mut warnings = Vec::new();
    let mut links = vec![false; disks.len()];
    if link {
        let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
        for (i, ((source, source_pool), target)) in disks.iter().zip(&targets).enumerate() {
            match unlinkable(i, source, source_pool, target) {
                None => {}
                Some(reason) if linked == Some(true) => return Err(reason),
                Some(reason) => {
                    warnings.push(format!("{reason}; made a full copy"));
                    continue;
                }
            }
            // 다른 노드의 API가 기반을 지우기로 했으면 그 위에 복제를 만들지 않는다(지우기 Task도 다시 확인한다).
            let deleting = tasks.iter().any(|t| {
                t.spec.target == source.id
                    && t.spec.op == TaskOp::VolumeDelete
                    && !t.status.state.is_finished()
            });
            if deleting {
                return Err(format!("volume {:?} is being deleted", source.name));
            }
            links[i] = true;
        }
    }

    let (_, volumes) = ctx
        .cluster
        .list::<VolumeSpec>()
        .map_err(|e| e.to_string())?;
    let mut taken: HashSet<String> = volumes.into_iter().map(|v| v.name).collect();
    let mut copies: Vec<(Volume, &StoragePool)> = Vec::with_capacity(disks.len());
    for (i, ((source, source_pool), target)) in disks.iter().zip(&targets).enumerate() {
        let copy_name = volume_name(name, &source.name, |n| taken.contains(n));
        taken.insert(copy_name.clone());
        let format = if links[i] {
            linked_format(target)
        } else {
            copy_format(source_pool, target, source.spec.format)
        };
        let mut copy: Volume = Resource::new(
            copy_name,
            VolumeSpec {
                pool: target.name.clone(),
                size_bytes: source.spec.size_bytes,
                format,
                content: Default::default(),
                // 기반 기록이 먼저다: 저장소 작업 전에 남겨 끊겨도 기반을 지울 수 없다(설계 §4).
                base: links[i].then_some(source.id),
            },
        );
        copy.status.node = Some(ctx.config.node);
        // 만들기 전에 적는다. 만들기가 끝났는지 모르는 채 끊겨도 정리가 찾는다.
        made(ctx, task.id, |m| m.volumes.push(copy.id));
        ctx.cluster
            .create(&copy)
            .await
            .map_err(|e| format!("cannot create volume {:?}: {e}", copy.name))?;
        if links[i] {
            // 기록한 뒤 다시 본다: 다른 노드의 되돌리기 Task는 `template: false`를 쓴 뒤 연결 복제를 다시 보므로, 이 기록이
            // 그 쓰기보다 앞서면 되돌리기가 이 기록을 보고 물러나고, 뒤면 여기서 `false`를 보고 물러난다(정리가 기록을 지운다).
            if !get_vm(ctx, vm.id)?.spec.template {
                return Err(format!("template {:?} was converted back to a VM", vm.name));
            }
            let (_, got) = ctx
                .storage
                .link_clone(source_pool, source.id, source.spec.format, copy.id)
                .await
                .map_err(|e| format!("cannot link volume {:?}: {e}", source.name))?;
            // 백엔드가 고른 형식이 다르면 기록을 고친다(정리가 그 형식으로 지운다).
            if got != copy.spec.format {
                copy.spec.format = got;
                ctx.cluster
                    .update_spec(copy.id, None, copy.labels.clone(), &copy.spec)
                    .await
                    .map_err(|e| format!("cannot record volume {:?}: {e}", copy.name))?;
            }
        } else {
            ctx.storage
                .copy_volume(
                    source_pool,
                    source.id,
                    source.spec.format,
                    tag.as_deref(),
                    force_share,
                    target,
                    copy.id,
                    format,
                )
                .await
                .map_err(|e| format!("cannot copy volume {:?}: {e}", source.name))?;
        }
        // 늘린 크기는 `mark_ready`가 실제 크기로 기록한다.
        if let Some(size) = grown[i] {
            ctx.storage
                .resize_volume(target, copy.id, copy.spec.format, size)
                .await
                .map_err(|e| format!("cannot grow volume {:?}: {e}", copy.name))?;
        }
        copies.push((copy, target));
    }

    // 배포 때 바꾸는 값을 다시 적용한다(API가 같은 검사를 미리 했다). 새 VM은 템플릿이 아니다.
    let mut spec = match overrides {
        Some(ov) => apply_overrides(&spec, ov).map_err(|e| e.to_string())?,
        None => spec,
    };
    spec.template = false;
    // API가 확인한 폴더만 쓴다: 덮어쓰기에 폴더가 없으면 루트였다(원본이 그사이 옮겨졌어도, 리뷰 1).
    if overrides.is_none_or(|ov| ov.folder.is_none()) {
        spec.folder = None;
    }
    let mut new: Vm = Resource::new(name, spec);
    if let Some(dir) = &ctx.config.state_dir {
        let from = match snapshot {
            Some(id) => vars_copy(ctx, vm.id, id).expect("state_dir is set"),
            None => {
                fetch_vars(ctx, &vm).await;
                uefi_vars_path(dir, vm.id)
            }
        };
        // BIOS VM이거나 한 번도 켠 적 없는 UEFI VM이면 없다.
        if tokio::fs::try_exists(&from).await.unwrap_or(false) {
            let to = uefi_vars_path(dir, new.id);
            made(ctx, task.id, |m| m.vars = Some(to.clone()));
            copy_file(&from, &to)
                .await
                .map_err(|e| format!("cannot copy UEFI variables: {e}"))?;
            // 새 VM의 변수는 이 노드에 있다. 다른 노드에서 켜면 여기서 받아 간다.
            new.status.firmware_node = Some(ctx.config.node);
        }
    }
    for (copy, target) in &copies {
        mark_ready(ctx, copy, target).await;
    }

    for (disk, (copy, _)) in new.spec.disks.iter_mut().zip(&copies) {
        disk.volume = copy.name.clone();
    }
    // 복제본은 MAC을 새로 받는다.
    for nic in &mut new.spec.nics {
        nic.mac = None;
    }
    warnings.extend(identity_warning(
        "the clone",
        &kept_identity(&new.spec, overrides),
    ));
    // 새 VM의 TPM은 켤 때 새로 만든다(`tpm_sha256` 없음). UEFI 변수(Secure Boot 키 포함)는 위에서 복사했다.
    if new.spec.tpm && had_tpm {
        warnings.push(new_tpm_warning(name, &vm.name));
    }
    // 켜라고 했으면 놓을 노드를 고른다(주어진 노드는 그 노드가 켤 수 있는지 본다). 없으면 꺼진 채 만든다.
    // 원본의 노드는 받지 않는다: 배포 때 준 노드만(`spec.node`, 켤 곳).
    new.spec.node = overrides.and_then(|ov| ov.node);
    let node = new.spec.node;
    let start = match overrides.is_some_and(|ov| ov.start) {
        false => None,
        true => match start_node(ctx, &new) {
            Ok(node) => Some(node),
            Err(reason) => {
                warnings.push(format!("{reason}; start {name:?} later"));
                None
            }
        },
    };
    create_vm(ctx, task, new, start.or(node), start.is_some()).await?;
    Ok((!warnings.is_empty()).then(|| warnings.join("; ")))
}

/// `linked: true`인데 원본이 템플릿이 아닐 때
pub(crate) const LINKED_ONLY_FROM_TEMPLATES: &str = "linked clones can be made only from templates";

/// 디스크 `disk`(원본 볼륨 `source`, 그 풀 `source_pool`)를 풀 `target`에 연결 복제로 만들 수 없는 이유. 되면 `None`이다.
/// 연결 복제는 기반과 같은 풀에만, 그 백엔드가 지원할 때만, 기반이 독립 볼륨일 때만(사슬은 1단) 된다.
pub(crate) fn unlinkable(
    disk: usize,
    source: &Volume,
    source_pool: &StoragePool,
    target: &StoragePool,
) -> Option<String> {
    if target.name != source_pool.name {
        return Some(format!(
            "disk{disk}: pool {:?} is not the pool {:?} of template disk {:?}",
            target.name, source_pool.name, source.name
        ));
    }
    if !pool_supports_linked_clones(&source_pool.spec.backend, source.spec.format) {
        return Some(format!(
            "disk{disk}: pool {:?} ({}) cannot hold linked clones",
            source_pool.name,
            backend_kind(&source_pool.spec.backend)
        ));
    }
    if source.spec.base.is_some() {
        return Some(format!(
            "disk{disk}: volume {:?} is a linked clone itself",
            source.name
        ));
    }
    None
}

/// 풀 백엔드의 종류 이름(`dir`, `lvm-shared` …)
fn backend_kind(backend: &PoolBackend) -> String {
    serde_json::to_value(backend)
        .ok()
        .and_then(|v| v["kind"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// 연결 복제 볼륨의 형식: 파일 풀(dir, NFS)은 qcow2 겹층, 블록 풀은 raw 네이티브 복제다.
fn linked_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
        _ => ImageFormat::Raw,
    }
}

/// 경고([`identity_warning`])에서 볼 스펙: 배포 때 새로 준 호스트 이름·네트워크 설정은 원본의 것이 아니므로 뺀다.
fn kept_identity(spec: &VmSpec, overrides: Option<&CloneOverrides>) -> VmSpec {
    let mut spec = spec.clone();
    if let (Some(ci), Some(given)) = (
        spec.cloud_init.as_mut(),
        overrides.and_then(|ov| ov.cloud_init.as_ref()),
    ) {
        if given.hostname.is_some() {
            ci.hostname = None;
        }
        if !given.networks.is_empty() {
            ci.networks.clear();
        }
    }
    spec
}

/// 켜라고 한 새 VM을 놓을 노드: 스펙에 노드가 있으면(배포 때 준 노드) 그 노드가 켤 수 있어야 하고, 없으면 스케줄러가
/// 고른다. 놓지 못하면 그 이유다.
fn start_node(ctx: &Ctx, vm: &Vm) -> Result<Uuid, String> {
    let inv = Inventory::load(&ctx.cluster).map_err(|e| e.to_string())?;
    match vm.spec.node {
        Some(id) => {
            let node = inv
                .nodes
                .iter()
                .find(|n| n.id == id)
                .ok_or_else(|| format!("node {id} no longer exists"))?;
            node_fits(&vm.spec, &inv, node)?;
            Ok(id)
        }
        None => schedule(vm, &inv).map_err(|e| e.to_string()),
    }
}

/// 새 VM(복제·복원)이 원본의 cloud-init 고정 IP 주소·호스트 이름을 그대로 가지면 Task가 남길 경고. 원본과 함께 켜면
/// 주소·이름이 겹친다. 호스트 이름이 없으면 새 VM 이름을 쓰므로 겹치지 않는다.
pub(super) fn identity_warning(what: &str, spec: &VmSpec) -> Option<String> {
    let ci = spec.cloud_init.as_ref()?;
    let addresses: Vec<&str> = ci
        .networks
        .iter()
        .flat_map(|n| [&n.ipv4, &n.ipv6])
        .filter_map(|ip| match ip {
            Some(IpConfig::Static { address, .. }) => Some(address.as_str()),
            _ => None,
        })
        .collect();
    let mut kept = Vec::new();
    if !addresses.is_empty() {
        kept.push(format!("static IP addresses {}", addresses.join(", ")));
    }
    if let Some(host) = &ci.hostname {
        kept.push(format!("host name {host:?}"));
    }
    (!kept.is_empty()).then(|| {
        format!(
            "{what} keeps the cloud-init {}; change them before starting it",
            kept.join(" and ")
        )
    })
}

/// 복제·복원·가져오기가 만든 새 VM `new`를 저장소에 만든다: 빠진 MAC을 새로 받고, 예약 없는 VM이다. 노드는 `node`이고
/// (복원·가져오기는 없음), `start`면 켠 채로(그 노드의 reconciler가 켠다), 아니면 꺼진 채로 만든다. `placement`를 모르는
/// 노드가 있으면 VM 고정을, `templates`를 모르는 노드가 있으면 템플릿 표시(복원)를 버린다. 성공하면 Task가 만든 것을 더는
/// 정리하지 않는다.
pub(super) async fn create_vm(
    ctx: &Ctx,
    task: &Task,
    mut new: Vm,
    node: Option<Uuid>,
    start: bool,
) -> Result<(), String> {
    let name = new.name.clone();
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let mut used: BTreeSet<MacAddr> = vms
        .iter()
        .chain([&new])
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    for nic in new.spec.nics.iter_mut().filter(|n| n.mac.is_none()) {
        let mac = std::iter::repeat_with(MacAddr::generate)
            .find(|m| !used.contains(m))
            .expect("random MACs eventually differ");
        used.insert(mac);
        nic.mac = Some(mac);
    }
    new.spec.node = node;
    new.spec.power = if start {
        PowerState::Running
    } else {
        PowerState::Stopped
    };
    new.spec.snapshot_schedule = None;
    // 복제 대상과 그 복제본은 원본 VM의 것이다. 새 VM은 복제 없이 만든다.
    new.spec.replication.clear();
    // 원본(또는 스냅샷)이 가리키던 HA 그룹이 그새 지워졌으면 그룹만 빼고 정책은 남긴다.
    if let Some(group) = new.spec.ha.as_ref().and_then(|ha| ha.group.clone())
        && ctx
            .cluster
            .get_by_name::<HaGroupSpec>(&group)
            .map_err(|e| e.to_string())?
            .is_none()
    {
        tracing::warn!(vm = %name, group = %group, "the HA group of the new VM no longer exists; leaving the VM without a group");
        if let Some(ha) = new.spec.ha.as_mut() {
            ha.group = None;
        }
    }
    // 복제본은 원본의 VM 고정(`placement`)을 받는다. 옛 노드는 그것을 풀지 못하므로 그 노드가 있으면 버린다.
    if new.spec.placement.is_some()
        && !crate::cluster::missing_feature(&ctx.cluster, PLACEMENT)
            .map_err(|e| e.to_string())?
            .is_empty()
    {
        new.spec.placement = None;
    }
    // 템플릿을 백업에서 복원하면 템플릿이다(계획 58). 옛 노드는 그 표시를 풀지 못하므로 그 노드가 있으면 버린다.
    if new.spec.template
        && !crate::cluster::missing_feature(&ctx.cluster, TEMPLATES)
            .map_err(|e| e.to_string())?
            .is_empty()
    {
        crate::backup_receive::drop_templates(&mut new.spec);
    }
    // 이름이나 MAC이 그새 쓰였으면 저장소가 거부한다.
    if let Err(e) = ctx.cluster.create(&new).await {
        // 나중에 만들어질 수도 있으면 새 VM이 볼륨을 쓸 수 있으므로 지우지 않고 남긴다(설계 §11).
        if matches!(e, ClusterError::Timeout | ClusterError::Unreachable(_)) {
            ctx.cloned.lock().unwrap().remove(&task.id);
        }
        return Err(format!("cannot create VM {name:?}: {e}"));
    }
    ctx.cloned.lock().unwrap().remove(&task.id);
    Ok(())
}

/// 실패한 복제(와 복원)가 만든 볼륨(파일과 리소스)과 UEFI 변수 사본, TPM 상태를 지운다.
pub(super) async fn discard(ctx: &Ctx, task: Uuid) {
    let Some(made) = ctx.cloned.lock().unwrap().remove(&task) else {
        return;
    };
    for id in made.volumes {
        if let Err(e) = remove_volume(ctx, id).await {
            tracing::warn!(volume = %id, error = %e, "cannot clean up volume of failed clone");
        }
    }
    if let Some(tpm) = &made.tpm {
        let _ = tokio::fs::remove_dir_all(tpm).await;
    }
    if let Some(vars) = &made.vars {
        let _ = tokio::fs::remove_file(vars).await;
    }
    // 새 VM의 상태 디렉터리. 비어 있을 때만 지워진다.
    let file = made.vars.as_deref().or(made.tpm.as_deref());
    if let Some(dir) = file.and_then(std::path::Path::parent) {
        let _ = tokio::fs::remove_dir(dir).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(backend: PoolBackend) -> StoragePool {
        Resource::new(
            "p",
            StoragePoolSpec {
                backend,
                nodes: vec![],
                shared: false,
                backups: false,
                folder: None,
            },
        )
    }

    #[test]
    fn copies_take_the_format_the_target_pool_holds() {
        let dir = pool(PoolBackend::Dir {
            path: "/pools/local".into(),
        });
        let lvm = pool(PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        });
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);
        assert_eq!(copy_format(&dir, &dir, raw), raw);
        assert_eq!(copy_format(&dir, &dir, qcow2), qcow2);
        // LVM-thin 풀은 raw만 담는다.
        assert_eq!(copy_format(&dir, &lvm, qcow2), raw);
        assert_eq!(copy_format(&lvm, &lvm, raw), raw);
        // 파일 풀로 옮긴 LVM 볼륨은 스냅샷을 담을 수 있게 qcow2로 바꾼다.
        assert_eq!(copy_format(&lvm, &dir, raw), qcow2);
        // ZFS 풀도 블록 풀이다.
        let zfs = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        assert_eq!(copy_format(&dir, &zfs, qcow2), raw);
        assert_eq!(copy_format(&zfs, &lvm, raw), raw);
        assert_eq!(copy_format(&lvm, &zfs, raw), raw);
        assert_eq!(copy_format(&zfs, &dir, raw), qcow2);
        // 스냅샷이 없는 공유 LVM 풀도 raw만 담는 블록 풀이다.
        let shared = pool(PoolBackend::LvmShared {
            vg: "san".into(),
            iscsi: None,
        });
        assert_eq!(copy_format(&dir, &shared, qcow2), raw);
        assert_eq!(copy_format(&shared, &dir, raw), qcow2);
        assert_eq!(copy_format(&shared, &lvm, raw), raw);
    }

    #[test]
    fn volume_name_joins_cuts_and_numbers() {
        let none = |_: &str| false;
        assert_eq!(volume_name("web-2", "root", none), "web-2-root");

        let taken = |n: &str| ["web-2-root", "web-2-root-2"].contains(&n);
        assert_eq!(volume_name("web-2", "root", taken), "web-2-root-3");

        // 63자로 자르고, 자른 끝의 `-`는 뗀다.
        let vm = "v".repeat(40);
        let volume = format!("{}-x", "d".repeat(21));
        let name = volume_name(&vm, &volume, none);
        assert_eq!(name, format!("{vm}-{}", "d".repeat(21)));
        assert_eq!(name.len(), 62);

        // 번호를 붙여도 63자를 넘지 않는다.
        let long = "a".repeat(63);
        let name = volume_name(&long, "root", |n| n.len() == 63 && !n.ends_with("-2"));
        assert_eq!(name, format!("{}-2", "a".repeat(61)));
    }
}
