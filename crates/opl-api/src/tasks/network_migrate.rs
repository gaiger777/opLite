//! 단순 → SDN 옮기기(계획 67 §9)의 엔진: 계획(미리 보기와 같은 함수), `NetworkMigration` 리소스 만들기, 걸음(`start`·
//! `rollback`·`finish`) Task와 멈춤. API(스테이지 5)와 시험 고리가 앞의 함수들을 부르고, `network-migrate` Task가 VM을
//! 옮긴다.
//!
//! - **VM마다 상태**를 리소스의 `status.vms`에 적는다(`pending` → `moving` → `moved`, 실패면 `failed`, 되돌리기는
//!   `rolling-back` → `rolled-back`). Task가 죽거나 opld가 다시 떠도(이 Task는 다시 뜬 opld가 이어서 한다) `moving`인 VM은
//!   VM 스펙·전원·도는 NIC(`status.hardware`)를 보고 마저 하거나 되돌린다 — 모든 걸음이 "지금 모습을 보고 다음 한 걸음"이다.
//! - 옮기기 직전에 VM마다 **다시 계획**해(적은 주소를 준 주소로) 막히거나 방식·주소가 달라졌으면 그 VM만 `failed`.
//! - `offline`: 스펙만(리비전 확인 쓰기). `live`: 스펙을 쓰고 VM 노드에 핫플러그 Task(`RelinkNic` — tap만 옮김)를 만들어
//!   기다린다. 실패하면 핫플러그 Task가 스펙의 NIC를 도는 것으로 되돌리고 여기서 cloud-init 항목을 되살린다. `restart`: 끄고
//!   스펙을 바꿔 켠다. 켜지 못하면 원본으로 되돌려 다시 켠다. 켜진 VM의 NIC는 사용자가 고른 방식(계획)으로만 바뀐다.
//! - **다리**: 대상이 오버레이면 첫 VM 앞에 `status.bridge`(노드·physnet `mig<h8>`)를 적고 그 노드의 SDN 에이전트가 physnet을
//!   알릴 때까지 기다린다(NB `l2gateway` 행은 리더 OvnSync가 같은 칸에서 그린다). 끝내기·모두 되돌리기가 걷는다.
//! - 리소스에 VM을 더하는 일은 만들 때만 한다(Raft `migration-vm` 겹침은 만들기·스펙 쓰기에서만 본다 — 스테이지 1).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::time::Duration;

use futures_util::StreamExt;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::floating_ip::FloatingIpSpec;
use opl_types::guest::GuestInfo;
use opl_types::load_balancer::LoadBalancerSpec;
use opl_types::network::{Network, NetworkSpec};
use opl_types::network_migration::{
    self as nm, MigrationBridge, MigrationPhase, MigrationPlan, MigrationStep, MoveState,
    NetworkMigration, NetworkMigrationSpec, NetworkMigrationStatus, PlanInput, PlanVm, PlannedMode,
    VmMove,
};
use opl_types::node::{Node, NodeSpec, SDN_SERVICES};
use opl_types::resource::Resource;
use opl_types::router::RouterSpec;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use uuid::Uuid;

use super::{Ctx, create_task};
use crate::cluster::require_feature;
use crate::error::ApiError;
use crate::vms::is_stopped;

/// 다리 노드가 physnet을 알리기(또는 걷기)를 기다리는 한도
const BRIDGE_WAIT: Duration = Duration::from_secs(120);
/// VM 하나의 끄기·켜기·핫플러그를 기다리는 한도
const VM_WAIT: Duration = Duration::from_secs(15 * 60);
/// 다시 읽는 간격(시험은 `poll_interval`이 더 짧다)
const POLL: Duration = Duration::from_millis(500);

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("invalid_migration")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- 계획에 쓰는 세상 ----

/// 계획 한 번에 읽은 저장소
struct World {
    source: Network,
    target: Network,
    /// 원본에 NIC가 있는 VM(스펙 `vms`가 있으면 그것만)
    vms: Vec<Vm>,
    nodes: Vec<Node>,
    taken: BTreeMap<IpAddr, Uuid>,
    busy: BTreeSet<Uuid>,
}

/// 끝나지 않은 옮기기 `m`이 잡은 VM(`spec.vms ∪ status.vms[].vm`)
fn held_vms(m: &NetworkMigration) -> impl Iterator<Item = Uuid> + '_ {
    m.spec
        .vms
        .iter()
        .copied()
        .chain(m.status.vms.iter().map(|v| v.vm))
        .filter(|_| !m.status.is_done())
}

/// 대상 네트워크 `target`(이름)에서 Raft `ip`로 잡힌 주소와 그 주인: VM NIC·라우터 바깥·유동 IP·부하 분산(VIP·바깥 VIP·
/// 건강 검사 주소)
fn taken_on(
    cluster: &Cluster,
    target: &str,
    vms: &[Vm],
) -> Result<BTreeMap<IpAddr, Uuid>, ApiError> {
    let mut taken = BTreeMap::new();
    for v in vms.iter().filter(|v| !v.spec.template) {
        for n in v.spec.nics.iter().filter(|n| n.network == target) {
            taken.extend(n.ips.iter().map(|ip| (ip.to_canonical(), v.id)));
        }
    }
    let (_, routers) = cluster.list::<RouterSpec>()?;
    for r in &routers {
        if let Some(e) = r.spec.external.as_ref().filter(|e| e.network == target) {
            taken.extend(e.ips.iter().map(|ip| (ip.to_canonical(), r.id)));
        }
    }
    let (_, fips) = cluster.list::<FloatingIpSpec>()?;
    for f in fips.iter().filter(|f| f.spec.network == target) {
        if let Some(a) = f.spec.address {
            taken.insert(a.to_canonical(), f.id);
        }
    }
    let (_, lbs) = cluster.list::<LoadBalancerSpec>()?;
    for l in &lbs {
        if l.spec.network == target
            && let Some(vip) = l.spec.vip
        {
            taken.insert(IpAddr::V4(vip), l.id);
        }
        if let Some(e) = l.spec.external.as_ref().filter(|e| e.network == target)
            && let Some(ip) = e.ip
        {
            taken.insert(IpAddr::V4(ip), l.id);
        }
        for m in l.spec.monitors.iter().filter(|m| m.network == target) {
            taken.insert(IpAddr::V4(m.ip), l.id);
        }
    }
    Ok(taken)
}

/// 스펙의 원본·대상·VM을 읽고 본다(§9.1): 원본은 단순 네트워크(브리지·VLAN·VXLAN), 대상은 서브넷이 있는 SDN 네트워크, 준
/// VM은 있고 원본에 NIC가 있다(400 `invalid_migration`). `except`는 다시 계획하는 옮기기 자신(바쁜 VM에서 뺀다).
fn load_world(
    cluster: &Cluster,
    spec: &NetworkMigrationSpec,
    except: Option<Uuid>,
) -> Result<World, ApiError> {
    let network = |name: &str| -> Result<Network, ApiError> {
        cluster
            .get_by_name::<NetworkSpec>(name)?
            .ok_or_else(|| invalid(format!("network {name:?} not found")))
    };
    let source = network(&spec.source)?;
    if source.spec.is_sdn() {
        return Err(invalid(format!(
            "network {:?} is an SDN network; the source must be a bridge, VLAN or VXLAN network",
            source.name
        )));
    }
    let target = network(&spec.target)?;
    if !target.spec.is_sdn() {
        return Err(invalid(format!(
            "network {:?} is not an SDN network; the target must be one",
            target.name
        )));
    }
    if target.spec.subnets.is_empty() {
        return Err(invalid(format!(
            "SDN network {:?} has no subnet; give it the source's subnet first",
            target.name
        )));
    }
    let (_, all) = cluster.list::<VmSpec>()?;
    for id in &spec.vms {
        let Some(v) = all.iter().find(|v| v.id == *id) else {
            return Err(invalid(format!("VM {id} not found")));
        };
        if !v.spec.nics.iter().any(|n| n.network == spec.source) {
            return Err(invalid(format!(
                "VM {:?} has no NIC on network {:?}",
                v.name, spec.source
            )));
        }
    }
    let vms: Vec<Vm> = all
        .iter()
        .filter(|v| v.spec.nics.iter().any(|n| n.network == spec.source))
        .filter(|v| spec.vms.is_empty() || spec.vms.contains(&v.id))
        .cloned()
        .collect();
    let (_, nodes) = cluster.list::<NodeSpec>()?;
    let taken = taken_on(cluster, &spec.target, &all)?;
    let (_, migrations) = cluster.list::<NetworkMigrationSpec>()?;
    let busy = migrations
        .iter()
        .filter(|m| Some(m.id) != except)
        .flat_map(held_vms)
        .collect();
    Ok(World {
        source,
        target,
        vms,
        nodes,
        taken,
        busy,
    })
}

/// 옮기기 다리가 놓인(끝나지 않은 옮기기의 `status.bridge`가 있는) 대상 네트워크 이름(§9.5 — 라우터 인터페이스로 붙이지 못함)
pub(crate) fn bridged_networks(cluster: &Cluster) -> Result<BTreeSet<String>, ApiError> {
    let (_, migrations) = cluster.list::<NetworkMigrationSpec>()?;
    Ok(migrations
        .into_iter()
        .filter(|m| !m.status.is_done() && m.status.bridge.is_some())
        .map(|m| m.spec.target)
        .collect())
}

/// 옮기기 계획(§9.2 — 미리 보기와 만들기가 같다). `guests`는 VM마다 게스트 에이전트가 답한 것(없으면 주소를 모름),
/// `allowed`는 부른 사용자가 그 VM의 `vm.config`를 가졌는지다. 모든 노드가 `sdn-services`를 알아야 한다.
pub fn plan_migration(
    cluster: &Cluster,
    spec: &NetworkMigrationSpec,
    guests: &BTreeMap<Uuid, GuestInfo>,
    allowed: &(dyn Fn(&Vm) -> bool + Sync),
) -> Result<MigrationPlan, ApiError> {
    require_feature(cluster, SDN_SERVICES)?;
    spec.validate().map_err(|e| invalid(e.to_string()))?;
    let world = load_world(cluster, spec, None)?;
    Ok(plan_world(&world, spec, guests, allowed))
}

fn plan_world(
    world: &World,
    spec: &NetworkMigrationSpec,
    guests: &BTreeMap<Uuid, GuestInfo>,
    allowed: &(dyn Fn(&Vm) -> bool + Sync),
) -> MigrationPlan {
    let pvs: Vec<PlanVm> = world
        .vms
        .iter()
        .map(|vm| PlanVm {
            vm,
            running: !is_stopped(vm),
            guest: guests.get(&vm.id),
            allowed: allowed(vm),
        })
        .collect();
    nm::plan(&PlanInput {
        spec,
        source: &world.source.spec,
        target: &world.target.spec,
        vms: &pvs,
        nodes: &world.nodes,
        taken: &world.taken,
        busy: &world.busy,
    })
}

/// 옮기기 `name`을 만든다: 계획을 `status.vms`에 적은 `planned` 리소스(원본 네트워크의 폴더 — 스펙에 없으면). 한 원본에
/// 끝나지 않은 옮기기 하나, 한 VM은 한 옮기기에만(Raft `migration-source`·`migration-vm` — 409). 계획도 돌려준다.
pub async fn create_migration(
    cluster: &Cluster,
    name: &str,
    mut spec: NetworkMigrationSpec,
    guests: &BTreeMap<Uuid, GuestInfo>,
    allowed: &(dyn Fn(&Vm) -> bool + Sync),
) -> Result<(NetworkMigration, MigrationPlan), ApiError> {
    require_feature(cluster, SDN_SERVICES)?;
    spec.validate().map_err(|e| invalid(e.to_string()))?;
    let world = load_world(cluster, &spec, None)?;
    let plan = plan_world(&world, &spec, guests, allowed);
    if spec.folder.is_none() {
        spec.folder = world.source.spec.folder;
    }
    let mut m: NetworkMigration = Resource::new(name, spec);
    m.status.vms = plan
        .nics
        .iter()
        .filter_map(|p| {
            let vm = world.vms.iter().find(|v| v.id == p.vm)?;
            VmMove::planned(p, &vm.spec)
        })
        .collect();
    m.revision = cluster.create(&m).await?;
    Ok((m, plan))
}

/// 옮기기 `id`의 걸음 하나를 노드 `node`(요청을 받은 노드)의 `network-migrate` Task로 시작한다. 이미 Task가 돌면 409
/// `migration_in_progress`, 끝난 옮기기는 409 `migration_finished`. `vms`는 되돌릴 VM(비면 모두), `router`는 끝내며 붙일 라우터다.
pub async fn start_step(
    cluster: &Cluster,
    id: Uuid,
    step: MigrationStep,
    vms: Vec<Uuid>,
    router: Option<String>,
    node: Uuid,
) -> Result<Task, ApiError> {
    require_feature(cluster, SDN_SERVICES)?;
    let m = cluster
        .get::<NetworkMigrationSpec>(id)?
        .ok_or_else(|| ApiError::not_found(format!("network migration {id} not found")))?;
    if m.status.is_done() {
        return Err(ApiError::conflict(
            "migration_finished",
            format!(
                "network migration {:?} is {}",
                m.name,
                m.status.phase.as_str()
            ),
        ));
    }
    if let Some(t) = live_task(cluster, &m)? {
        return Err(ApiError::conflict(
            "migration_in_progress",
            format!("network migration {:?} is running task {t}", m.name),
        ));
    }
    if step == MigrationStep::Rollback
        && let Some(v) = vms
            .iter()
            .find(|v| !m.status.vms.iter().any(|x| x.vm == **v))
    {
        return Err(invalid(format!(
            "VM {v} is not in network migration {:?}",
            m.name
        )));
    }
    let op = TaskOp::NetworkMigrate { step, vms, router };
    let task = create_task(cluster, op, id, node).await?;
    // 옮기기의 Task를 적는다(다른 걸음이 겹치지 않게 — Task가 시작할 때 다시 본다).
    let task_id = task.id;
    update(cluster, id, |s| s.task = Some(task_id))
        .await
        .map_err(|e| ApiError::conflict("migration_in_progress", e))?;
    Ok(task)
}

/// 옮기기 `m`의 끝나지 않은 Task(노드가 오프라인이면 끝난 것으로 본다 — 다른 노드에서 이어 갈 수 있게)
pub(crate) fn live_task(cluster: &Cluster, m: &NetworkMigration) -> Result<Option<Uuid>, ApiError> {
    let Some(t) = m.status.task else {
        return Ok(None);
    };
    let Some(task) = cluster.get::<TaskSpec>(t)? else {
        return Ok(None);
    };
    if task.status.state.is_finished() {
        return Ok(None);
    }
    let online = cluster
        .get::<NodeSpec>(task.spec.node)?
        .is_some_and(|n| n.status.online);
    Ok(online.then_some(t))
}

/// 도는 옮기기를 멈춘다(§9.4 — 지금 옮기는 VM이 끝난 뒤 Task가 멈춘다). `running`이 아니면 409 `migration_not_running`.
pub async fn pause_migration(cluster: &Cluster, id: Uuid) -> Result<NetworkMigration, ApiError> {
    let m = cluster
        .get::<NetworkMigrationSpec>(id)?
        .ok_or_else(|| ApiError::not_found(format!("network migration {id} not found")))?;
    if m.status.phase != MigrationPhase::Running {
        return Err(ApiError::conflict(
            "migration_not_running",
            format!(
                "network migration {:?} is {}, not running",
                m.name,
                m.status.phase.as_str()
            ),
        ));
    }
    update(cluster, id, |s| {
        if s.phase == MigrationPhase::Running {
            s.phase = MigrationPhase::Paused;
        }
    })
    .await
    .map_err(|e| ApiError::conflict("migration_not_running", e))
}

/// 클러스터에서 지운 노드 `node`가 다리 노드인 끝나지 않은 옮기기를 멈춘다(§13 — `paused`, 다리 기록을 지우고 까닭을
/// 적는다). 다시 이어 갈 때 다른 다리 노드를 고르거나(`bridge_node`) 계획이 고른다. 노드 지우기 응답의 경고 줄을 돌려준다.
pub(crate) async fn drop_bridge_node(cluster: &Cluster, node: Uuid, name: &str) -> Vec<String> {
    let Ok((_, migrations)) = cluster.list::<NetworkMigrationSpec>() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for m in migrations
        .iter()
        .filter(|m| !m.status.is_done() && m.status.bridge.as_ref().is_some_and(|b| b.node == node))
    {
        let why = format!(
            "the bridge node {name:?} was removed from the cluster; resume with another bridge node \
             (`orbivisorctl network migration resume {} --bridge-node NODE`)",
            m.name
        );
        let reason = why.clone();
        match update(cluster, m.id, move |s| {
            if s.bridge.as_ref().is_some_and(|b| b.node == node) {
                s.bridge = None;
                s.phase = MigrationPhase::Paused;
                s.error = Some(reason.clone());
            }
        })
        .await
        {
            Ok(_) => out.push(format!("network migration {:?} paused: {why}", m.name)),
            Err(e) => out.push(format!(
                "network migration {:?} uses the removed node as its bridge and could not be paused ({e})",
                m.name
            )),
        }
    }
    out
}

// ---- 상태 쓰기 ----

/// 상태를 `f`로 고쳐 쓴다(리비전 확인 — 그사이 다른 쪽이 쓴 VM 상태·멈춤을 덮지 않게 다시 읽어 고친다).
async fn update(
    cluster: &Cluster,
    id: Uuid,
    f: impl Fn(&mut NetworkMigrationStatus),
) -> Result<NetworkMigration, String> {
    loop {
        let m = cluster
            .get::<NetworkMigrationSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("the network migration was deleted")?;
        let mut status = m.status.clone();
        f(&mut status);
        if status == m.status {
            return Ok(m);
        }
        match cluster
            .update_status_checked::<NetworkMigrationSpec>(id, m.revision, &status)
            .await
        {
            Ok(revision) => {
                return Ok(NetworkMigration {
                    status,
                    revision,
                    ..m
                });
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// VM NIC 하나(`vm`·`nic`)의 기록을 `f`로 고친다. 상태가 바뀌면 시각을 적는다.
async fn set_move(
    cluster: &Cluster,
    id: Uuid,
    key: (Uuid, usize),
    f: impl Fn(&mut VmMove),
) -> Result<NetworkMigration, String> {
    update(cluster, id, |s| {
        if let Some(m) = s.vms.iter_mut().find(|m| (m.vm, m.nic) == key) {
            let before = m.state;
            f(m);
            if m.state != before {
                m.at = Some(now_secs());
            }
        }
    })
    .await
}

// ---- Task ----

/// 한 VM NIC의 결과
enum Outcome {
    Done,
    Failed(String),
    Skipped,
}

/// `network-migrate` Task. 성공해도 실패한 VM·남은 원본 NIC는 경고로 알린다.
pub(super) async fn perform(ctx: &Ctx, task: &Task) -> Result<Option<String>, String> {
    let TaskOp::NetworkMigrate { step, vms, router } = &task.spec.op else {
        return Err("not a network migration task".into());
    };
    let id = task.spec.target;
    // 이 Task가 옮기기를 맡는다. 다른 Task가 아직 돌며 맡고 있으면 하지 않는다(걸음을 시작한 API는 Task를 만든 뒤 적으므로
    // 그보다 먼저 시작해도 된다).
    let current = read(ctx, id)?;
    if let Some(other) = current.status.task.filter(|t| *t != task.id)
        && live_task(&ctx.cluster, &current).map_err(|e| e.message)? == Some(other)
    {
        return Err(format!(
            "network migration {:?} is driven by another task {other}",
            current.name
        ));
    }
    let me = task.id;
    let m = update(&ctx.cluster, id, |s| s.task = Some(me)).await?;
    if m.status.is_done() {
        return Err(format!(
            "network migration {:?} is {}",
            m.name,
            m.status.phase.as_str()
        ));
    }
    let result = match step {
        MigrationStep::Start => Box::pin(start(ctx, id)).await,
        MigrationStep::Rollback => Box::pin(rollback(ctx, id, vms)).await,
        MigrationStep::Finish => Box::pin(finish(ctx, id, router.as_deref())).await,
    };
    if let Err(e) = &result {
        let e = e.clone();
        let _ = update(&ctx.cluster, id, |s| {
            if !s.is_done() {
                s.phase = MigrationPhase::Failed;
                s.error = Some(e.clone());
            }
        })
        .await;
    }
    result
}

fn read(ctx: &Ctx, id: Uuid) -> Result<NetworkMigration, String> {
    ctx.cluster
        .get::<NetworkMigrationSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the network migration was deleted".to_string())
}

fn read_vm(ctx: &Ctx, id: Uuid) -> Result<Option<Vm>, String> {
    ctx.cluster.get::<VmSpec>(id).map_err(|e| e.to_string())
}

/// 클러스터 태그(다리 physnet 이름) — 이 노드의 SDN 설정에서
fn tag(ctx: &Ctx) -> Result<String, String> {
    ctx.config
        .sdn
        .as_ref()
        .map(|h| h.tag().to_string())
        .ok_or_else(|| "this node has no SDN (OVN) settings to run a network migration".into())
}

/// 다리 노드 `node`의 physnet `physnet` 상태: Some(true)면 준비, Some(false)면 알렸지만 준비 안 됨(까닭), None이면 없음
fn physnet_state(ctx: &Ctx, node: Uuid, physnet: &str) -> Option<Result<(), String>> {
    let n = ctx.cluster.get::<NodeSpec>(node).ok()??;
    let p = n
        .status
        .sdn
        .as_ref()?
        .physnets
        .iter()
        .find(|p| p.name == physnet)?
        .clone();
    Some(if p.ready {
        Ok(())
    } else {
        Err(p.error.unwrap_or_else(|| "not ready".into()))
    })
}

/// 다리를 놓는다(§9.5): 계획이 다리를 원하면 `status.bridge`를 적고 그 노드가 physnet을 알릴 때까지 기다린다.
async fn lay_bridge(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let m = read(ctx, id)?;
    // 지운 노드의 다리 기록은 없는 것으로 본다(다시 계획해 다른 노드를 고른다 — §13).
    let laid = m.status.bridge.clone().filter(|b| {
        ctx.cluster
            .get::<NodeSpec>(b.node)
            .is_ok_and(|n| n.is_some())
    });
    let bridge = match laid {
        Some(b) => b,
        None => {
            let world = load_world(&ctx.cluster, &m.spec, Some(id)).map_err(|e| e.message)?;
            let plan = plan_world(&world, &m.spec, &BTreeMap::new(), &|_| true);
            if let Some(e) = plan.bridge.error {
                return Err(format!("cannot lay the migration bridge: {e}"));
            }
            if !plan.bridge.needed {
                return Ok(());
            }
            // 라우터 인터페이스인 대상은 라우터 포트가 서브넷 게이트웨이 주소를 가져 원본 망의 물리 라우터와 다툰다.
            let (_, routers) = ctx
                .cluster
                .list::<RouterSpec>()
                .map_err(|e| e.to_string())?;
            if let Some(r) = routers
                .iter()
                .find(|r| r.spec.interfaces.iter().any(|i| i.network == m.spec.target))
            {
                return Err(format!(
                    "cannot lay the migration bridge: network {:?} is an interface of router {:?} (its port has the gateway address of the source network); detach it, or migrate without a bridge",
                    m.spec.target, r.name
                ));
            }
            let Some(node) = plan.bridge.node else {
                return Err("cannot lay the migration bridge: no bridge node".into());
            };
            let b = MigrationBridge {
                node,
                physnet: opl_types::sdn::migration_physnet(&tag(ctx)?, id),
            };
            let laid = b.clone();
            update(&ctx.cluster, id, move |s| {
                s.bridge = Some(laid.clone());
            })
            .await?;
            b
        }
    };
    let deadline = tokio::time::Instant::now() + BRIDGE_WAIT;
    loop {
        let state = physnet_state(ctx, bridge.node, &bridge.physnet);
        if let Some(Ok(())) = state {
            return Ok(());
        }
        // 그새 다리 노드를 지워 멈췄으면(`drop_bridge_node`) 기다리지 않는다 — `start`가 멈춘 것을 보고 끝낸다.
        if read(ctx, id)?.status.bridge.as_ref() != Some(&bridge) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            let why = match state {
                Some(Err(e)) => e,
                _ => "the node did not report it".into(),
            };
            return Err(format!(
                "the migration bridge {} on node {} is not ready after {}s: {why}",
                bridge.physnet,
                bridge.node,
                BRIDGE_WAIT.as_secs()
            ));
        }
        tokio::time::sleep(POLL.min(ctx.config.poll_interval)).await;
    }
}

/// 다리를 걷는다(`status.bridge`를 지우고 노드가 physnet을 걷기를 기다림). 시간 안에 걷히지 않으면 경고다.
async fn take_bridge(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    let m = read(ctx, id)?;
    let Some(bridge) = m.status.bridge.clone() else {
        return Ok(None);
    };
    update(&ctx.cluster, id, |s| s.bridge = None).await?;
    let deadline = tokio::time::Instant::now() + BRIDGE_WAIT;
    while physnet_state(ctx, bridge.node, &bridge.physnet).is_some() {
        if tokio::time::Instant::now() >= deadline {
            return Ok(Some(format!(
                "node {} still reports the migration bridge {}",
                bridge.node, bridge.physnet
            )));
        }
        tokio::time::sleep(POLL.min(ctx.config.poll_interval)).await;
    }
    Ok(None)
}

/// 한 번에 옮기는 수
fn parallel(m: &NetworkMigration) -> usize {
    usize::from(m.spec.parallel.unwrap_or(1).max(1))
}

/// `start` 걸음: 다리를 놓고 옮길 VM을 차례로(`parallel`개씩) 옮긴다. 멈춤을 보면 하던 VM만 끝낸다.
async fn start(ctx: &Ctx, id: Uuid) -> Result<Option<String>, String> {
    update(&ctx.cluster, id, |s| {
        s.phase = MigrationPhase::Running;
        s.error = None;
    })
    .await?;
    lay_bridge(ctx, id).await?;
    let m = read(ctx, id)?;
    let keys: Vec<(Uuid, usize)> = m
        .status
        .vms
        .iter()
        .filter(|v| {
            v.mode != PlannedMode::Blocked
                && matches!(
                    v.state,
                    MoveState::Pending | MoveState::Moving | MoveState::Failed
                )
        })
        .map(|v| (v.vm, v.nic))
        .collect();
    let outcomes: Vec<Outcome> = futures_util::stream::iter(keys)
        .map(|key| async move {
            if read(ctx, id).is_ok_and(|m| m.status.phase == MigrationPhase::Paused) {
                return Outcome::Skipped;
            }
            Box::pin(move_one(ctx, id, key)).await
        })
        .buffer_unordered(parallel(&m))
        .collect()
        .await;
    let failed: Vec<String> = outcomes
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Failed(e) => Some(e),
            _ => None,
        })
        .collect();
    let m = update(&ctx.cluster, id, |s| {
        s.phase = settled(s, failed.is_empty());
        if s.phase == MigrationPhase::Failed {
            s.error = Some(format!("{} NIC(s) failed to move", failed.len()));
        }
    })
    .await?;
    match m.status.phase {
        MigrationPhase::Failed => Err(failed.join("; ")),
        MigrationPhase::Paused => Ok(Some("paused".into())),
        _ => Ok(warnings(&m)),
    }
}

/// `start`가 끝난 뒤의 단계: 멈췄으면 그대로, 실패가 있으면 `failed`, 옮길 VM이 남았으면 `paused`, 다 옮겼으면 `moved`
fn settled(s: &NetworkMigrationStatus, ok: bool) -> MigrationPhase {
    if s.phase == MigrationPhase::Paused {
        return MigrationPhase::Paused;
    }
    if !ok || s.vms.iter().any(|v| v.state == MoveState::Failed) {
        return MigrationPhase::Failed;
    }
    let left = s.vms.iter().any(|v| {
        v.mode != PlannedMode::Blocked && matches!(v.state, MoveState::Pending | MoveState::Moving)
    });
    if left {
        MigrationPhase::Paused
    } else {
        MigrationPhase::Moved
    }
}

/// 끝난 옮기기의 경고: 원본에 남은 NIC(막힘·실패)
fn warnings(m: &NetworkMigration) -> Option<String> {
    let left: Vec<String> = m
        .status
        .vms
        .iter()
        .filter(|v| !matches!(v.state, MoveState::Moved))
        .map(|v| format!("VM {} NIC {}", v.vm, v.nic))
        .collect();
    (!left.is_empty()).then(|| format!("still on network {:?}: {}", m.spec.source, left.join(", ")))
}

/// VM NIC 하나를 옮긴다(§9.4). 결과를 기록에 적는다.
async fn move_one(ctx: &Ctx, id: Uuid, key: (Uuid, usize)) -> Outcome {
    match Box::pin(try_move(ctx, id, key)).await {
        Ok(o) => o,
        Err(e) => {
            let _ = set_move(&ctx.cluster, id, key, |m| {
                m.state = MoveState::Failed;
                m.error = Some(e.clone());
            })
            .await;
            Outcome::Failed(format!("VM {} NIC {}: {e}", key.0, key.1))
        }
    }
}

async fn try_move(ctx: &Ctx, id: Uuid, key: (Uuid, usize)) -> Result<Outcome, String> {
    let mig = read(ctx, id)?;
    let Some(rec) = mig
        .status
        .vms
        .iter()
        .find(|v| (v.vm, v.nic) == key)
        .cloned()
    else {
        return Ok(Outcome::Skipped);
    };
    let Some(vm) = read_vm(ctx, rec.vm)? else {
        // 그사이 지운 VM은 건너뛴다(§13).
        set_move(&ctx.cluster, id, key, |m| {
            m.mode = PlannedMode::Blocked;
            m.error = Some("the VM was deleted; skipped".into());
        })
        .await?;
        return Ok(Outcome::Skipped);
    };
    if rec.state != MoveState::Moving {
        if let Some(why) = replan(ctx, &mig, &vm, &rec)? {
            return Err(why);
        }
        set_move(&ctx.cluster, id, key, |m| {
            m.state = MoveState::Moving;
            m.error = None;
        })
        .await?;
    }
    let leg = Leg {
        to: mig.spec.target.clone(),
        from: mig.spec.source.clone(),
        rec: rec.clone(),
        forward: true,
    };
    let done = match rec.mode {
        PlannedMode::Offline => Box::pin(offline(ctx, &leg)).await,
        PlannedMode::Live => Box::pin(live(ctx, &leg)).await,
        PlannedMode::Restart => Box::pin(restart(ctx, &leg)).await,
        PlannedMode::Blocked => return Ok(Outcome::Skipped),
    };
    match done {
        Ok(ips) => {
            set_move(&ctx.cluster, id, key, |m| {
                m.state = MoveState::Moved;
                m.error = None;
                m.ips.clone_from(&ips);
            })
            .await?;
            Ok(Outcome::Done)
        }
        Err(e) => {
            settle_spec(ctx, &leg).await;
            Err(e)
        }
    }
}

/// 만들 때 적은 계획과 지금 다시 한 계획이 다른 까닭(§9.2). 적은 주소를 준 주소로 다시 계획한다(게스트 에이전트를 묻지
/// 않는다).
fn replan(
    ctx: &Ctx,
    mig: &NetworkMigration,
    vm: &Vm,
    rec: &VmMove,
) -> Result<Option<String>, String> {
    let mut spec = mig.spec.clone();
    spec.vms = vec![vm.id];
    spec.addresses
        .retain(|a| !(a.vm == rec.vm && a.mac == rec.mac));
    if !rec.ips.is_empty() {
        spec.addresses.push(nm::MigrationAddress {
            vm: rec.vm,
            mac: rec.mac,
            ip: rec.ips.clone(),
        });
    }
    let world = load_world(&ctx.cluster, &spec, Some(mig.id)).map_err(|e| e.message)?;
    let plan = plan_world(&world, &spec, &BTreeMap::new(), &|_| true);
    let Some(now) = plan
        .nics
        .iter()
        .find(|p| p.nic == rec.nic && p.mac == Some(rec.mac))
    else {
        return Ok(Some(format!(
            "NIC {} (MAC {}) is no longer on network {:?}",
            rec.nic, rec.mac, mig.spec.source
        )));
    };
    Ok(nm::plan_changed(rec, now))
}

/// 한 방향의 옮기기(앞으로: 원본 → 대상, 되돌리기: 대상 → 원본)
struct Leg {
    /// 끝날 네트워크
    to: String,
    /// 지금(옮기기 전) 네트워크
    from: String,
    rec: VmMove,
    forward: bool,
}

impl Leg {
    /// 스펙을 이 방향으로 바꾼다.
    fn edit(&self, spec: &mut VmSpec) {
        if self.forward {
            nm::move_nic(spec, self.rec.nic, &self.to, &self.rec.ips);
        } else {
            nm::restore_nic(spec, self.rec.nic, &self.rec.from);
        }
    }

    fn on_to(&self, spec: &VmSpec) -> bool {
        nm::nic_on(spec, &self.rec, &self.to)
    }

    fn on_from(&self, spec: &VmSpec) -> bool {
        nm::nic_on(spec, &self.rec, &self.from)
    }
}

/// 도는 QEMU의 이 NIC(기록)가 네트워크 `network`에 있는지
fn running_on(vm: &Vm, rec: &VmMove, network: &str) -> bool {
    vm.status.hardware.as_ref().is_some_and(|hw| {
        hw.nics
            .get(rec.nic)
            .is_some_and(|n| n.mac == Some(rec.mac) && n.network == network)
    })
}

/// 스펙을 이 방향으로 써 본다(리비전 확인). 앞으로면 IPAM이 빈 주소 체계를 채우고 정한 주소가 그대로인지 본다. 썼으면
/// Ok(true), 다른 쪽이 그새 써서 다시 읽어야 하면 Ok(false)
async fn write_leg(
    ctx: &Ctx,
    vm: &Vm,
    leg: &Leg,
    power: Option<PowerState>,
) -> Result<bool, String> {
    let mut spec = vm.spec.clone();
    leg.edit(&mut spec);
    if let Some(p) = power {
        spec.power = p;
    }
    if leg.forward {
        let (_, all) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
        let (_, networks) = ctx
            .cluster
            .list::<NetworkSpec>()
            .map_err(|e| e.to_string())?;
        let others: Vec<&Vm> = all.iter().filter(|v| v.id != vm.id).collect();
        let held = crate::ipam::load_held(&ctx.cluster).map_err(|e| e.message)?;
        let _ = crate::ipam::assign_new(&mut spec, &others, &networks, &held)?;
        let got = &spec.nics[leg.rec.nic].ips;
        if let Some(ip) = leg.rec.ips.iter().find(|ip| !got.contains(ip)) {
            return Err(format!(
                "address {ip} on network {:?} is already in use",
                leg.to
            ));
        }
    }
    match ctx
        .cluster
        .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
    {
        Ok(_) => Ok(true),
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => Ok(false),
        Err(e) if crate::ipam::is_ip_taken(&e) && leg.rec.ips.is_empty() => Ok(false),
        Err(e) => Err(format!("cannot write the VM: {e}")),
    }
}

/// 이 NIC의 지금 주소(옮긴 뒤 기록에 적는다)
fn nic_ips(vm: &Vm, rec: &VmMove) -> Vec<IpAddr> {
    vm.spec
        .nics
        .get(rec.nic)
        .map(|n| n.ips.clone())
        .unwrap_or_default()
}

fn get_vm(ctx: &Ctx, id: Uuid) -> Result<Vm, String> {
    read_vm(ctx, id)?.ok_or_else(|| "the VM was deleted".into())
}

/// `offline`: 꺼진 VM의 스펙만 바꾼다.
async fn offline(ctx: &Ctx, leg: &Leg) -> Result<Vec<IpAddr>, String> {
    for _ in 0..20 {
        let vm = get_vm(ctx, leg.rec.vm)?;
        if leg.on_to(&vm.spec) {
            return Ok(nic_ips(&vm, &leg.rec));
        }
        if !leg.on_from(&vm.spec) {
            return Err("the NIC was changed outside the migration".into());
        }
        if !is_stopped(&vm) {
            return Err("the VM was started; plan the migration again".into());
        }
        write_leg(ctx, &vm, leg, None).await?;
    }
    Err("the VM kept changing".into())
}

/// VM 노드의 끝나지 않은 핫플러그 Task
fn hotplug_task(ctx: &Ctx, vm: Uuid) -> Result<Option<Task>, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::VmHotplug && t.spec.target == vm && !t.status.state.is_finished()
    }))
}

/// Task `id`가 끝날 때까지 기다린다(실패면 그 오류).
async fn wait_task(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + VM_WAIT;
    loop {
        let t = ctx
            .cluster
            .get::<TaskSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("the hotplug task was deleted")?;
        match t.status.state {
            TaskState::Succeeded => return Ok(()),
            TaskState::Failed => {
                return Err(t
                    .status
                    .error
                    .unwrap_or_else(|| "the hotplug task failed".into()));
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the hotplug task did not finish in time".into());
        }
        tokio::time::sleep(POLL.min(ctx.config.poll_interval)).await;
    }
}

/// `live`: 스펙을 쓰고 VM 노드의 핫플러그 Task가 tap을 옮기기를 기다린다(§9.4 `RelinkNic`). 핫플러그가 실패하면 그 Task가
/// 스펙의 NIC를 도는 것으로 되돌린다.
async fn live(ctx: &Ctx, leg: &Leg) -> Result<Vec<IpAddr>, String> {
    let mut tries = 0;
    let vm = loop {
        tries += 1;
        if tries > 20 {
            return Err("the VM kept changing".into());
        }
        let vm = get_vm(ctx, leg.rec.vm)?;
        let running = vm.spec.power == PowerState::Running
            && vm.status.phase == VmPhase::Running
            && vm.status.hardware.is_some();
        if leg.on_to(&vm.spec) && running_on(&vm, &leg.rec, &leg.to) {
            return Ok(nic_ips(&vm, &leg.rec));
        }
        if !running {
            return Err("the VM is no longer running; plan the migration again".into());
        }
        if leg.on_to(&vm.spec) {
            break vm;
        }
        if !leg.on_from(&vm.spec) {
            return Err("the NIC was changed outside the migration".into());
        }
        write_leg(ctx, &vm, leg, None).await?;
    };
    let task = match hotplug_task(ctx, vm.id)? {
        Some(t) => t,
        None => {
            let node = vm.status.node.ok_or("the VM has no node")?;
            create_task(&ctx.cluster, TaskOp::VmHotplug, vm.id, node)
                .await
                .map_err(|e| e.message)?
        }
    };
    let waited = wait_task(ctx, task.id).await;
    let vm = get_vm(ctx, leg.rec.vm)?;
    if leg.on_to(&vm.spec) && running_on(&vm, &leg.rec, &leg.to) {
        return Ok(nic_ips(&vm, &leg.rec));
    }
    Err(match waited {
        Err(e) => format!("the NIC was not moved live: {e}"),
        Ok(()) => "the NIC was not moved live".into(),
    })
}

/// VM `id`가 켜질(Ok)·켜지지 못할(Err) 때까지
async fn wait_started(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + VM_WAIT;
    loop {
        let vm = get_vm(ctx, id)?;
        match (vm.spec.power, vm.status.phase) {
            (PowerState::Running, VmPhase::Running) => return Ok(()),
            (PowerState::Stopped, _) => {
                return Err(vm
                    .status
                    .last_error
                    .unwrap_or_else(|| "the VM did not start".into()));
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the VM did not start in time".into());
        }
        tokio::time::sleep(POLL.min(ctx.config.poll_interval)).await;
    }
}

/// 전원을 `power`로 쓴다(리비전 확인).
async fn set_power(ctx: &Ctx, vm: &Vm, power: PowerState) -> Result<(), String> {
    let mut spec = vm.spec.clone();
    spec.power = power;
    match ctx
        .cluster
        .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
        .await
    {
        Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => Ok(()),
        Err(e) => Err(format!("cannot write the VM: {e}")),
    }
}

/// `restart`: 끄고(ACPI — reconciler가 VM의 시간 한도로) 스펙을 바꿔 켠다. 켜지 못하면 원래 네트워크로 되돌려 다시 켠다.
async fn restart(ctx: &Ctx, leg: &Leg) -> Result<Vec<IpAddr>, String> {
    let deadline = tokio::time::Instant::now() + VM_WAIT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("the VM did not stop or start in time".into());
        }
        let vm = get_vm(ctx, leg.rec.vm)?;
        let (on_to, on_from) = (leg.on_to(&vm.spec), leg.on_from(&vm.spec));
        if !on_to && !on_from {
            return Err("the NIC was changed outside the migration".into());
        }
        match (on_to, vm.spec.power) {
            // 끈다.
            (false, PowerState::Running) => set_power(ctx, &vm, PowerState::Stopped).await?,
            // 꺼지면 바꿔 켠다.
            (false, PowerState::Stopped) => {
                if is_stopped(&vm) {
                    write_leg(ctx, &vm, leg, Some(PowerState::Running)).await?;
                    continue;
                }
            }
            (true, PowerState::Running) => {
                match wait_started(ctx, vm.id).await {
                    Ok(()) => return Ok(nic_ips(&get_vm(ctx, vm.id)?, &leg.rec)),
                    // 켜지 못했다: 아래 갈래가 되돌린다.
                    Err(_) => continue,
                }
            }
            (true, PowerState::Stopped) => {
                let why = vm
                    .status
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "the VM did not start".into());
                back_and_start(ctx, leg).await;
                return Err(format!(
                    "the VM did not start on network {:?} and was started on {:?} again: {why}",
                    leg.to, leg.from
                ));
            }
        }
        tokio::time::sleep(POLL.min(ctx.config.poll_interval)).await;
    }
}

/// 켜지 못한 VM을 원래 네트워크로 되돌려 다시 켠다(기다리되 실패는 기록만 — VM이 망 없이 꺼진 채 남지 않게 최선을 다한다).
async fn back_and_start(ctx: &Ctx, leg: &Leg) {
    let back = Leg {
        to: leg.from.clone(),
        from: leg.to.clone(),
        rec: leg.rec.clone(),
        forward: !leg.forward,
    };
    for _ in 0..20 {
        let Ok(vm) = get_vm(ctx, leg.rec.vm) else {
            return;
        };
        if back.on_to(&vm.spec) {
            if vm.spec.power != PowerState::Running {
                let _ = set_power(ctx, &vm, PowerState::Running).await;
                continue;
            }
            if let Err(e) = wait_started(ctx, vm.id).await {
                tracing::warn!(vm = %vm.id, error = %e, "the VM did not start again on its original network");
            }
            return;
        }
        if let Err(e) = write_leg(ctx, &vm, &back, Some(PowerState::Running)).await {
            tracing::warn!(vm = %vm.id, error = %e, "cannot move the NIC back after a failed start");
            return;
        }
    }
}

/// 실패한 뒤 스펙을 지금 네트워크에 맞게 고른다: 원본 쪽이면 옮기기 전 모습(cloud-init 항목·보안 그룹)을, 대상 쪽이면 빈
/// cloud-init 항목을. 핫플러그가 NIC만 되돌린 경우를 고친다(실패는 기록만).
async fn settle_spec(ctx: &Ctx, leg: &Leg) {
    let (source, target) = if leg.forward {
        (&leg.from, &leg.to)
    } else {
        (&leg.to, &leg.from)
    };
    for _ in 0..10 {
        let Ok(vm) = get_vm(ctx, leg.rec.vm) else {
            return;
        };
        let mut spec = vm.spec.clone();
        if nm::nic_on(&spec, &leg.rec, source) {
            nm::restore_nic(&mut spec, leg.rec.nic, &leg.rec.from);
        } else if nm::nic_on(&spec, &leg.rec, target) {
            let ips = nic_ips(&vm, &leg.rec);
            nm::move_nic(&mut spec, leg.rec.nic, target, &ips);
        } else {
            return;
        }
        if spec == vm.spec {
            return;
        }
        match ctx
            .cluster
            .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(vm = %vm.id, error = %e, "cannot settle the VM spec after a failed network migration step");
                return;
            }
        }
    }
}

/// `rollback` 걸음(§9.6): `vms`(비면 모두)의 옮겼거나 실패한 NIC를 원본으로 되돌린다. 켜진 VM은 MTU가 줄지 않으면 `live`,
/// 아니면 `restart`, 꺼졌으면 `offline`. 모두 되돌아갔으면 다리를 걷고 `rolled-back`.
async fn rollback(ctx: &Ctx, id: Uuid, vms: &[Uuid]) -> Result<Option<String>, String> {
    let m = update(&ctx.cluster, id, |s| {
        s.phase = MigrationPhase::RollingBack;
        s.error = None;
    })
    .await?;
    if ctx
        .cluster
        .get_by_name::<NetworkSpec>(&m.spec.source)
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err(format!(
            "the source network {:?} was deleted; there is nothing to roll back to",
            m.spec.source
        ));
    }
    let keys: Vec<(Uuid, usize)> = m
        .status
        .vms
        .iter()
        .filter(|v| vms.is_empty() || vms.contains(&v.vm))
        .filter(|v| {
            matches!(
                v.state,
                MoveState::Moved | MoveState::Moving | MoveState::Failed | MoveState::RollingBack
            )
        })
        .map(|v| (v.vm, v.nic))
        .collect();
    let outcomes: Vec<Outcome> = futures_util::stream::iter(keys)
        .map(|key| async move { Box::pin(back_one(ctx, id, key)).await })
        .buffer_unordered(parallel(&m))
        .collect()
        .await;
    let failed: Vec<String> = outcomes
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Failed(e) => Some(e),
            _ => None,
        })
        .collect();
    let m = read(ctx, id)?;
    let all_back = m
        .status
        .vms
        .iter()
        .all(|v| matches!(v.state, MoveState::Pending | MoveState::RolledBack));
    let mut warning = None;
    if all_back && failed.is_empty() {
        warning = take_bridge(ctx, id).await?;
    }
    let m = update(&ctx.cluster, id, |s| {
        s.phase = if !failed.is_empty() || s.vms.iter().any(|v| v.state == MoveState::Failed) {
            MigrationPhase::Failed
        } else if all_back {
            MigrationPhase::RolledBack
        } else if s
            .vms
            .iter()
            .any(|v| v.mode != PlannedMode::Blocked && v.state == MoveState::Pending)
        {
            MigrationPhase::Paused
        } else {
            MigrationPhase::Moved
        };
        if s.phase == MigrationPhase::Failed {
            s.error = Some(format!("{} NIC(s) failed to roll back", failed.len()));
        }
    })
    .await?;
    if m.status.phase == MigrationPhase::Failed {
        return Err(failed.join("; "));
    }
    Ok(warning)
}

/// VM NIC 하나를 원본으로 되돌린다.
async fn back_one(ctx: &Ctx, id: Uuid, key: (Uuid, usize)) -> Outcome {
    match Box::pin(try_back(ctx, id, key)).await {
        Ok(o) => o,
        Err(e) => {
            let _ = set_move(&ctx.cluster, id, key, |m| {
                m.state = MoveState::Failed;
                m.error = Some(format!("rollback: {e}"));
            })
            .await;
            Outcome::Failed(format!("VM {} NIC {}: {e}", key.0, key.1))
        }
    }
}

async fn try_back(ctx: &Ctx, id: Uuid, key: (Uuid, usize)) -> Result<Outcome, String> {
    let mig = read(ctx, id)?;
    let Some(rec) = mig
        .status
        .vms
        .iter()
        .find(|v| (v.vm, v.nic) == key)
        .cloned()
    else {
        return Ok(Outcome::Skipped);
    };
    let done = |e: Option<String>| {
        let e = e.clone();
        move |m: &mut VmMove| {
            m.state = MoveState::RolledBack;
            m.error.clone_from(&e);
        }
    };
    let Some(vm) = read_vm(ctx, rec.vm)? else {
        set_move(
            &ctx.cluster,
            id,
            key,
            done(Some("the VM was deleted".into())),
        )
        .await?;
        return Ok(Outcome::Done);
    };
    let leg = Leg {
        to: mig.spec.source.clone(),
        from: mig.spec.target.clone(),
        rec: rec.clone(),
        forward: false,
    };
    // 옮기지 못하고 원본에 남은 NIC(실패한 앞 걸음): 스펙만 옮기기 전 모습으로 고른다.
    if leg.on_to(&vm.spec) && (is_stopped(&vm) || running_on(&vm, &rec, &leg.to)) {
        settle_spec(ctx, &leg).await;
        set_move(&ctx.cluster, id, key, done(None)).await?;
        return Ok(Outcome::Done);
    }
    set_move(&ctx.cluster, id, key, |m| {
        m.state = MoveState::RollingBack;
        m.error = None;
    })
    .await?;
    let source = ctx
        .cluster
        .get_by_name::<NetworkSpec>(&mig.spec.source)
        .map_err(|e| e.to_string())?
        .ok_or("the source network was deleted")?;
    let target = ctx
        .cluster
        .get_by_name::<NetworkSpec>(&mig.spec.target)
        .map_err(|e| e.to_string())?;
    // 되돌아갈 때 게스트가 볼 MTU가 줄지 않으면(원본 ≥ 대상, 또는 tap만 옮긴 VM — QEMU의 host_mtu가 원본 그대로) live
    let from_mtu = source
        .spec
        .mtu()
        .unwrap_or(opl_types::network::DEFAULT_PROVIDER_MTU);
    let to_mtu = target
        .and_then(|t| t.spec.mtu())
        .unwrap_or(opl_types::sdn::DEFAULT_SDN_MTU);
    let live_ok = from_mtu >= to_mtu || rec.mode == PlannedMode::Live;
    let result = if is_stopped(&vm) {
        Box::pin(offline(ctx, &leg)).await
    } else if live_ok && vm.status.hardware.is_some() {
        Box::pin(live(ctx, &leg)).await
    } else {
        Box::pin(restart(ctx, &leg)).await
    };
    match result {
        Ok(_) => {
            set_move(&ctx.cluster, id, key, done(None)).await?;
            Ok(Outcome::Done)
        }
        Err(e) => {
            settle_spec(ctx, &leg).await;
            Err(e)
        }
    }
}

/// `finish` 걸음(§9.7): 다리를 걷고 `router`가 있으면 대상 네트워크를 그 라우터 인터페이스로 붙인 뒤 `finished`. 옮기는
/// 중인 VM이 있으면 실패, 원본에 NIC가 남은 VM은 경고다.
async fn finish(ctx: &Ctx, id: Uuid, router: Option<&str>) -> Result<Option<String>, String> {
    let m = read(ctx, id)?;
    if let Some(v) = m
        .status
        .vms
        .iter()
        .find(|v| matches!(v.state, MoveState::Moving | MoveState::RollingBack))
    {
        return Err(format!(
            "VM {} NIC {} is still being moved; resume or roll back first",
            v.vm, v.nic
        ));
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(w) = take_bridge(ctx, id).await? {
        notes.push(w);
    }
    if let Some(router) = router {
        crate::routing::attach_interface(&ctx.cluster, router, &m.spec.target)
            .await
            .map_err(|e| {
                format!(
                    "cannot attach network {:?} to router {router:?}: {}",
                    m.spec.target, e.message
                )
            })?;
    }
    let m = update(&ctx.cluster, id, |s| {
        s.phase = MigrationPhase::Finished;
        s.error = None;
    })
    .await?;
    if let Some(w) = warnings(&m) {
        notes.push(w);
    }
    Ok((!notes.is_empty()).then(|| notes.join("; ")))
}
