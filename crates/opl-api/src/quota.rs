//! 폴더 할당량의 API 쪽(계획 62 설계 §9): 긴 Task가 일찍 실패하도록 요청 때 로컬 상태로 하는 사전 확인과 사용량
//! 보기. 경쟁에서 지키는 것은 상태 머신(`opl-cluster` `folders.rs`의 `check_quota`)이고, 계산 규칙은 둘 다
//! `opl_types::quota`다.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::folder::{FolderQuota, FolderSpec, FolderUsage};
use opl_types::quota::{
    DirectUsage, QuotaDimension, QuotaNode, QuotaTree, ancestors, quota_exceeded, subtree_usage,
};
use opl_types::storage::VolumeSpec;
use opl_types::vm::VmSpec;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::folders::folder_path;
use crate::resources::lookup;
use crate::state::AppState;

/// 409 `quota_exceeded`. `details`에 폴더·차원·상한·사용량을 싣는다.
pub(crate) fn exceeded(
    folder: Uuid,
    dimension: QuotaDimension,
    limit: u64,
    used: u64,
    message: String,
) -> ApiError {
    let mut e = ApiError::conflict("quota_exceeded", message);
    e.details = Some(serde_json::json!({
        "folder": folder, "dimension": dimension, "limit": limit, "used": used,
    }));
    e
}

/// 사전 확인할 쓰기
#[derive(Debug, Clone, Copy)]
pub(crate) enum Change<'a> {
    /// VM `replaces`(없으면 새 VM `count`개)를 폴더 `folder`에 사용량 `usage`로 쓴다.
    Vm {
        replaces: Option<Uuid>,
        folder: Option<Uuid>,
        usage: FolderUsage,
        count: u64,
    },
    /// 볼륨 `name`을 `size`바이트로 늘린다.
    Volume { name: &'a str, size: u64 },
    /// 폴더 `id`를 `parent` 아래로 옮긴다.
    Folder { id: Uuid, parent: Option<Uuid> },
}

fn tree(cluster: &Cluster) -> Result<QuotaTree, ApiError> {
    let (_, folders) = cluster.list::<FolderSpec>()?;
    Ok(folders
        .into_iter()
        .map(|f| {
            (
                f.id,
                QuotaNode {
                    parent: f.spec.parent,
                    quota: f.spec.quota,
                },
            )
        })
        .collect())
}

/// 볼륨 이름 → 크기
fn volume_sizes(cluster: &Cluster) -> Result<HashMap<String, u64>, ApiError> {
    let (_, volumes) = cluster.list::<VolumeSpec>()?;
    Ok(volumes
        .into_iter()
        .map(|v| (v.name, v.spec.size_bytes))
        .collect())
}

fn disk_bytes(spec: &VmSpec, sizes: &HashMap<String, u64>) -> u64 {
    spec.disks.iter().fold(0u64, |sum, d| {
        sum.saturating_add(sizes.get(&d.volume).copied().unwrap_or(0))
    })
}

/// VM 스펙 하나의 사용량(디스크는 지금 있는 볼륨의 크기)
pub(crate) fn vm_usage(cluster: &Cluster, spec: &VmSpec) -> Result<FolderUsage, ApiError> {
    let mut bytes = 0u64;
    for d in &spec.disks {
        if let Some(v) = cluster.get_by_name::<VolumeSpec>(&d.volume)? {
            bytes = bytes.saturating_add(v.spec.size_bytes);
        }
    }
    Ok(FolderUsage::of_vm(spec, bytes))
}

/// VM 쓰기(`current`는 바꾸기 전 VM의 id와 스펙, 만들기는 None)의 사전 확인. 늘리지 않고 폴더도 그대로인 바꾸기는
/// 보지 않는다.
pub(crate) fn precheck_vm(
    cluster: &Cluster,
    current: Option<(Uuid, &VmSpec)>,
    spec: &VmSpec,
) -> Result<(), ApiError> {
    let usage = vm_usage(cluster, spec)?;
    if let Some((_, old)) = current
        && old.folder == spec.folder
        && usage.within(&vm_usage(cluster, old)?)
    {
        return Ok(());
    }
    precheck(
        cluster,
        Change::Vm {
            replaces: current.map(|(id, _)| id),
            folder: spec.folder,
            usage,
            count: 1,
        },
    )
}

/// 할당량 `backup_bytes`(계획 68 §13)에 드는 백업 바이트: 있는 VM은 VM id → 그 VM 백업의 합(VM 몫이라 VM을 옮기면
/// 따라감), 지운 VM의 백업은 백업한 때의 VM 폴더 → 합. 상태 머신(`opl-cluster` `folders.rs`)과 같은 규칙이다.
#[derive(Default)]
struct BackupBytes {
    by_vm: HashMap<Uuid, u64>,
    orphans: HashMap<Uuid, u64>,
}

fn backup_bytes(cluster: &Cluster, vms: &[opl_types::vm::Vm]) -> Result<BackupBytes, ApiError> {
    let (_, backups) = cluster.list::<opl_types::backup::BackupSpec>()?;
    let live: std::collections::HashSet<Uuid> = vms.iter().map(|v| v.id).collect();
    let mut out = BackupBytes::default();
    for b in backups {
        let bytes = b.status.backup_bytes();
        let slot = if live.contains(&b.spec.vm) {
            Some(out.by_vm.entry(b.spec.vm).or_default())
        } else {
            b.spec
                .vm_spec
                .folder
                .map(|f| out.orphans.entry(f).or_default())
        };
        if let Some(sum) = slot {
            *sum = sum.saturating_add(bytes);
        }
    }
    Ok(out)
}

impl BackupBytes {
    fn of(&self, vm: Uuid) -> FolderUsage {
        FolderUsage {
            backup_bytes: self.by_vm.get(&vm).copied().unwrap_or(0),
            ..FolderUsage::default()
        }
    }

    fn add_orphans(&self, direct: &mut DirectUsage) {
        for (f, bytes) in &self.orphans {
            direct.entry(*f).or_default().add(&FolderUsage {
                backup_bytes: *bytes,
                ..FolderUsage::default()
            });
        }
    }
}

/// 폴더마다 바로 든 VM의 사용량과(네트워크 폴더) 라우터·유동 IP 수(계획 66), 네트워크·부하 분산기 수와 바깥 VIP(계획 67)
fn direct_usage(cluster: &Cluster) -> Result<DirectUsage, ApiError> {
    use opl_types::resource::Kind;
    let sizes = volume_sizes(cluster)?;
    let (_, vms) = cluster.list::<VmSpec>()?;
    let backups = backup_bytes(cluster, &vms)?;
    let mut direct = DirectUsage::new();
    for vm in &vms {
        if let Some(f) = vm.spec.folder {
            let mut usage = FolderUsage::of_vm(&vm.spec, disk_bytes(&vm.spec, &sizes));
            usage.add(&backups.of(vm.id));
            direct.entry(f).or_default().add(&usage);
        }
    }
    backups.add_orphans(&mut direct);
    let networks = cluster.list::<opl_types::network::NetworkSpec>()?.1;
    let routers = cluster.list::<opl_types::router::RouterSpec>()?.1;
    let fips = cluster.list::<opl_types::floating_ip::FloatingIpSpec>()?.1;
    let lbs = cluster
        .list::<opl_types::load_balancer::LoadBalancerSpec>()?
        .1;
    let members = networks
        .iter()
        .map(|n| (n.spec.folder, Kind::Network, false))
        .chain(routers.iter().map(|r| (r.spec.folder, Kind::Router, false)))
        .chain(
            fips.iter()
                .map(|f| (f.spec.folder, Kind::FloatingIp, false)),
        )
        .chain(
            lbs.iter()
                .map(|l| (l.spec.folder, Kind::LoadBalancer, l.spec.external.is_some())),
        );
    for (folder, kind, external) in members {
        if let Some(f) = folder {
            direct
                .entry(f)
                .or_default()
                .add(&FolderUsage::of_network_member(kind, external));
        }
    }
    Ok(direct)
}

/// 쓰기 `change`가 할당량을 넘기면 409 `quota_exceeded` `"folder /teamA would use 34 of 32 vCPUs"`. 상태 머신과 같은
/// 규칙(늘리는 쓰기만, 할당량 있는 모든 조상)을 이 노드의 상태로 셈한다. 할당량이 하나도 없으면 바로 끝난다.
pub(crate) fn precheck(cluster: &Cluster, change: Change) -> Result<(), ApiError> {
    let before_tree = tree(cluster)?;
    if !before_tree
        .values()
        .any(|n| n.quota.is_some_and(|q| !q.is_empty()))
    {
        return Ok(());
    }
    let sizes = volume_sizes(cluster)?;
    let (_, vms) = cluster.list::<VmSpec>()?;
    let mut grown = None;
    if let Change::Volume { name, size } = change {
        let mut sizes = sizes.clone();
        if let Some(s) = sizes.get_mut(name) {
            *s = (*s).max(size);
        }
        grown = Some((name, sizes));
    }
    let mut after_tree = before_tree.clone();
    let (mut before, mut after) = (DirectUsage::new(), DirectUsage::new());
    // 백업 바이트(계획 68)는 그 할당량이 있을 때만 센다.
    let backups = if before_tree
        .values()
        .any(|n| n.quota.is_some_and(|q| q.backup_bytes.is_some()))
    {
        backup_bytes(cluster, &vms)?
    } else {
        BackupBytes::default()
    };
    backups.add_orphans(&mut before);
    backups.add_orphans(&mut after);
    for vm in &vms {
        let Some(f) = vm.spec.folder else { continue };
        let mut usage = FolderUsage::of_vm(&vm.spec, disk_bytes(&vm.spec, &sizes));
        usage.add(&backups.of(vm.id));
        before.entry(f).or_default().add(&usage);
        let next = match change {
            Change::Vm {
                replaces: Some(id), ..
            } if id == vm.id => continue,
            _ => match &grown {
                Some((name, sizes)) if vm.spec.disks.iter().any(|d| d.volume == *name) => {
                    let mut grown = FolderUsage::of_vm(&vm.spec, disk_bytes(&vm.spec, sizes));
                    grown.add(&backups.of(vm.id));
                    grown
                }
                _ => usage,
            },
        };
        after.entry(f).or_default().add(&next);
    }
    match change {
        Change::Vm {
            replaces,
            folder: Some(f),
            usage,
            count,
        } => {
            let mut usage = usage.times(count);
            // 바꾸는 VM의 백업은 VM을 따라간다.
            if let Some(id) = replaces {
                usage.add(&backups.of(id));
            }
            after.entry(f).or_default().add(&usage)
        }
        Change::Folder { id, parent } => {
            if let Some(node) = after_tree.get_mut(&id) {
                node.parent = parent;
            }
        }
        _ => {}
    }
    match quota_exceeded((&before_tree, &before), (&after_tree, &after)) {
        Some(e) => Err(exceeded(
            e.folder,
            e.dimension,
            e.limit,
            e.used,
            format!(
                "folder {} would use {} of {} {}",
                folder_path(cluster, e.folder),
                e.used,
                e.limit,
                e.dimension.unit()
            ),
        )),
        None => Ok(()),
    }
}

/// 할당량 있는 폴더 하나의 상한과 하위 트리 사용량
#[derive(Debug, Serialize, ToSchema)]
pub struct QuotaLimit {
    pub folder: Uuid,
    /// `/teamA/prod`
    pub path: String,
    pub quota: FolderQuota,
    pub usage: FolderUsage,
}

/// 폴더의 사용량과 그 폴더를 묶는 할당량
#[derive(Debug, Serialize, ToSchema)]
pub struct FolderUsageView {
    pub folder: Uuid,
    pub path: String,
    /// 하위 트리 사용량(할당 기준, 전원과 무관). 템플릿은 디스크만 센다.
    pub usage: FolderUsage,
    /// 이 폴더와 조상 가운데 할당량이 있는 폴더(가까운 것부터). 쓰기는 이 모두를 지켜야 한다.
    pub limits: Vec<QuotaLimit>,
    /// 차원마다 남은 몫이 가장 적은(묶는) 폴더. 상한이 없는 차원은 없다.
    #[schema(value_type = BTreeMap<String, Uuid>)]
    pub binding: BTreeMap<QuotaDimension, Uuid>,
}

/// 폴더의 하위 트리 사용량(vCPU·메모리·붙은 디스크·VM 수)과 그 폴더·조상의 할당량, 차원마다 묶는 폴더. 볼 수 없는
/// 조상(폴더 자체를 읽지 못함)은 `limits`·`binding`에서 뺀다.
#[utoipa::path(get, path = "/folders/{id}/usage", tag = "folders",
    params(("id" = String, Path, description = "folder id")),
    responses((status = 200, body = FolderUsageView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn folder_usage(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<FolderUsageView>, ApiError> {
    let folder = lookup::<FolderSpec>(&state.cluster, &id)?;
    let tree = tree(&state.cluster)?;
    let direct = direct_usage(&state.cluster)?;
    let mut limits = Vec::new();
    let mut binding: BTreeMap<QuotaDimension, (u64, Uuid)> = BTreeMap::new();
    for f in ancestors(&tree, Some(folder.id)) {
        let Some(quota) = tree[&f].quota.filter(|q| !q.is_empty()) else {
            continue;
        };
        // 볼 수 없는 조상의 경로·사용량은 보이지 않는다(리뷰 1). 그 상한에 걸린 쓰기는 409가 알린다.
        if !state.authz.allows(
            &principal,
            opl_types::authz::Privilege::SystemRead,
            opl_types::authz_eval::AuthzTarget::Folder(f),
        ) {
            continue;
        }
        let usage = subtree_usage(&tree, &direct, f);
        for d in QuotaDimension::ALL {
            if let Some(limit) = d.limit(&quota) {
                let left = limit.saturating_sub(d.used(&usage));
                // 남은 몫이 같으면 가까운 폴더
                if binding.get(&d).is_none_or(|(l, _)| left < *l) {
                    binding.insert(d, (left, f));
                }
            }
        }
        limits.push(QuotaLimit {
            folder: f,
            path: folder_path(&state.cluster, f),
            quota,
            usage,
        });
    }
    Ok(Json(FolderUsageView {
        folder: folder.id,
        path: folder_path(&state.cluster, folder.id),
        usage: subtree_usage(&tree, &direct, folder.id),
        limits,
        binding: binding.into_iter().map(|(d, (_, f))| (d, f)).collect(),
    }))
}
