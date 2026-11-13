//! 노드 비우기 Task(설계 §3.2): 비울 노드의 실행 중인 VM을 이름 순으로 하나씩 다른 노드로 라이브
//! 마이그레이션한다. 옮기지 못한 VM은 건너뛰고 끝에 이유와 함께 실패로 알린다. 이 노드로 들어오던
//! 마이그레이션은 끝나기를 기다려 그 VM도 옮기므로, 성공하면 노드가 비어 있다. hard `affinity`로 묶인 VM은
//! 묶음 전체를 받을 수 있는 노드로 차례로 옮긴다(첫 VM이 간 뒤 나머지는 규칙 때문에 그 노드로만 간다).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use opl_types::node::{Node, NodeSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState};
use opl_types::vm::{DiskMove, PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::{Ctx, TaskRunnerConfig};
use crate::cluster::missing_feature;
use crate::error::ApiError;
use crate::placement::{self, Assign, affinity_partners, hard_violations};
use crate::scheduler::{Inventory, disk_moves, node_fits_with};
use crate::snapshots::count_snapshots;
use crate::state::AppState;
use crate::vms::{RuleCheck, start_migration};

/// VM 하나를 옮겨 본 결과
enum Outcome {
    Moved,
    /// 옮길 것이 없어졌다(꺼졌거나 지워졌거나 다른 노드로 갔다).
    Gone,
    /// 옮기지 못했다(이유)
    NotMoved(String),
    /// 묶음의 한 구성원 때문에 묶음 전체를 옮기지 못했다(상대들 id·이름·이유, 이 VM의 이유). 막은 구성원이
    /// 아닌 구성원의 이유는 `affinity partner <막은 구성원>: <이유>`다.
    BundleNotMoved(Vec<(Uuid, String, String)>, String),
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
            match move_vm(ctx, &state, node, &vm, &not_moved).await? {
                Outcome::Moved => {
                    moved.insert(vm.id);
                    None
                }
                Outcome::Gone => {
                    *gone.entry(vm.id).or_default() += 1;
                    None
                }
                Outcome::NotMoved(reason) => Some(reason),
                Outcome::BundleNotMoved(partners, reason) => {
                    for (id, name, why) in partners {
                        not_moved.insert(id, (name, why));
                    }
                    Some(reason)
                }
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
        .filter(|v| v.status.migration.as_ref().is_some_and(|m| m.node == node))
        .map(|v| v.id)
        .chain(
            tasks
                .iter()
                .filter(|t| {
                    matches!(t.spec.op, TaskOp::VmMigrate { node: n, .. } if n == node)
                        && !t.status.state.is_finished()
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
/// 끝나면 옮기지 않고 비우기를 멈추는 오류다. 이미 옮기지 못한 VM(`not_moved`)이 묶음에 있으면 옮기지 않는다
/// (묶음이 갈라지지 않게).
async fn move_vm(
    ctx: &Ctx,
    state: &AppState,
    node: Uuid,
    vm: &Vm,
    not_moved: &HashMap<Uuid, (String, String)>,
) -> Result<Outcome, String> {
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
    // 함께 옮길 묶음(첫째가 이 VM)과 구성원마다 옮길 디스크. 노드 로컬 풀의 디스크는 대상 노드의 같은 이름
    // 풀로 복사한다(`pool`은 주지 않는다).
    let bundle = bundle(vm, node, &inv);
    if let Some(partner) = bundle[1..].iter().find(|v| not_moved.contains_key(&v.id)) {
        return Ok(Outcome::NotMoved(format!(
            "affinity partner {} could not move",
            partner.name
        )));
    }
    let all_moves: Vec<Vec<DiskMove>> = bundle
        .iter()
        .map(|v| disk_moves(&v.spec, &inv, None))
        .collect();
    let moves = all_moves[0].clone();
    // 디스크를 옮길 수 없는 구성원이 하나라도 있으면 묶음 전체를 옮기지 않는다.
    if let Some((blocker, reason)) = disks_blocked(ctx, state, &bundle, &all_moves) {
        let why = |i: usize| {
            if i == blocker {
                reason.clone()
            } else {
                format!("affinity partner {}: {reason}", bundle[blocker].name)
            }
        };
        let partners = bundle
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, v)| (v.id, v.name.clone(), why(i)))
            .collect();
        return Ok(Outcome::BundleNotMoved(partners, why(0)));
    }
    // HVF는 이 노드의 hypervisor 이름으로만 알 수 있다(macOS 노드).
    if me.status.hypervisor == HVF {
        return Ok(Outcome::NotMoved("hvf node".into()));
    }
    let target = match pick_target(&bundle, &inv, &all_moves) {
        Ok(target) => target,
        Err(rules) if !rules.is_empty() => {
            return Ok(Outcome::NotMoved(format!(
                "placement rule {}",
                rules.join(", ")
            )));
        }
        Err(_) => {
            // 옮길 디스크가 없었다면(공유 디스크처럼) 받을 노드가 있었으면 풀 때문이다.
            let stripped: Vec<Vm> = bundle
                .iter()
                .zip(&all_moves)
                .map(|(v, m)| without_disks(v, &inv, m))
                .collect();
            let reason = if all_moves.iter().any(|m| !m.is_empty())
                && pick_target(&stripped, &inv, &vec![Vec::new(); stripped.len()]).is_ok()
            {
                "no node can take its local disks"
            } else {
                "no node with capacity"
            };
            return Ok(Outcome::NotMoved(reason.into()));
        }
    };
    // 묶음의 나머지는 아직 이 노드에 있지만 대상 노드로 함께 간다고 보고 규칙을 본다.
    let rules = RuleCheck::Group(bundle[1..].iter().map(|v| v.id).collect());
    let task = match start_migration(state, vm.id, target, None, Some(node), rules.clone()).await {
        // 확인과 만들기 사이에 다른 Task가 들어왔다. 한 번 더 자리 잡기를 기다려 다시 한다.
        Err(e) if e.code == "vm_busy" => {
            match settle(ctx, node, vm.id).await {
                Ok(true) => {}
                Ok(false) => return Ok(Outcome::Gone),
                Err(e) => return Ok(Outcome::NotMoved(e)),
            }
            in_maintenance(ctx, node)?;
            start_migration(state, vm.id, target, None, Some(node), rules).await
        }
        r => r,
    };
    match task {
        Ok((task, _)) => {
            let wait = migration_wait(&ctx.config, !moves.is_empty());
            Ok(wait_migration(ctx, task.id, wait).await)
        }
        Err(e) => Ok(rejected(e)),
    }
}

/// 묶음의 어느 구성원이라도 노드 로컬 풀의 디스크(`all_moves[i]`)를 옮길 수 없으면 그 구성원의 순번과 이유:
/// 옮길 디스크가 있는데 저장소 마이그레이션을 모르는 노드가 있거나(옮길 디스크가 있는 첫 구성원) 그 구성원에
/// 스냅샷이 있다.
fn disks_blocked(
    ctx: &Ctx,
    state: &AppState,
    bundle: &[Vm],
    all_moves: &[Vec<DiskMove>],
) -> Option<(usize, String)> {
    let first = all_moves.iter().position(|m| !m.is_empty())?;
    match missing_feature(&ctx.cluster, "storage-migration") {
        Ok(old) if old.is_empty() => {}
        Ok(_) => {
            return Some((
                first,
                "disks on a local pool (some nodes do not support storage migration yet)".into(),
            ));
        }
        Err(e) => return Some((first, e.to_string())),
    }
    for (i, (member, moves)) in bundle.iter().zip(all_moves).enumerate() {
        if moves.is_empty() {
            continue;
        }
        match count_snapshots(state, member.id) {
            Ok(0) => {}
            Ok(_) => return Some((i, "disks on a local pool and snapshots".into())),
            Err(e) => return Some((i, e.message)),
        }
    }
    None
}

/// 옮길 VM과, 노드 `node`에서 도는 그 VM의 hard `affinity` 상대(추이적). 첫째가 `vm`이다.
fn bundle(vm: &Vm, node: Uuid, inv: &Inventory) -> Vec<Vm> {
    let mut found = vec![vm.clone()];
    let mut i = 0;
    while i < found.len() {
        let next: Vec<Vm> = affinity_partners(&found[i], inv)
            .into_iter()
            .filter(|v| v.spec.node == Some(node) && v.spec.power == PowerState::Running)
            .filter(|v| !found.iter().any(|f| f.id == v.id))
            .cloned()
            .collect();
        found.extend(next);
        i += 1;
    }
    found
}

/// 묶음(`bundle`, 첫째가 옮길 VM)을 함께 받을 노드: HVF가 아니고, 모든 구성원이 스케줄러의 조건(옮길 디스크
/// `moves[i]`가 있으면 그 대상 풀)을 지나고, 남은 메모리가 묶음 메모리의 합 이상이며, 묶음을 그 노드에 둔
/// [`Assign`]으로 hard 배치 규칙을 어기지 않는 노드 가운데 [`placement::compare`] 순서(첫 VM의 HA 그룹
/// 우선순위 → 구성원 soft 벌점의 합 → 남은 메모리 → id)로 가장 나은 노드. 스케줄러는 유지보수 노드(이 노드)를
/// 고르지 않는다. 없으면 조건·메모리는 맞는데 막은 hard 규칙의 이름(이름 순)이다. 규칙에 맞는 노드가 메모리
/// 때문에만 막혔으면(앞 구성원이 옮겨 간 노드에 자리가 없음 등) 규칙이 아니라 자리 탓이므로 비어 있다.
fn pick_target(
    bundle: &[Vm],
    inv: &Inventory,
    moves: &[Vec<DiskMove>],
) -> Result<Uuid, Vec<String>> {
    let lead = &bundle[0];
    let mut blocked = BTreeSet::new();
    // 노드 조건과 hard 규칙에는 맞는데 메모리가 모자란 노드가 있다.
    let mut no_room = false;
    let mut found = Vec::new();
    for n in inv.nodes.iter().filter(|n| n.status.hypervisor != HVF) {
        let fits = bundle
            .iter()
            .zip(moves)
            .all(|(v, m)| node_fits_with(&v.spec, inv, n, m).is_ok());
        if !fits {
            continue;
        }
        let mut assign = Assign::default();
        for v in bundle {
            assign.place(v.id, n.id);
        }
        // 다른 구성원은 이 노드에 있다고 본 남은 메모리
        let mut candidate = placement::score(lead, n, inv, &assign);
        let broken: Vec<String> = bundle
            .iter()
            .flat_map(|v| hard_violations(v, n, inv, &assign))
            .collect();
        if candidate.free_mib < i64::from(lead.spec.memory_mib) {
            no_room |= broken.is_empty();
            continue;
        }
        if !broken.is_empty() {
            blocked.extend(broken);
            continue;
        }
        candidate.penalty = bundle
            .iter()
            .map(|v| placement::score(v, n, inv, &assign).penalty)
            .sum();
        found.push(candidate);
    }
    if no_room {
        blocked.clear();
    }
    placement::best(&found)
        .map(|c| c.node)
        .ok_or_else(|| blocked.into_iter().collect())
}

/// 옮길 디스크(`moves`)를 뺀 VM: 그 디스크의 풀을 보지 않고(공유 디스크처럼) 받을 노드를 찾는다.
fn without_disks(vm: &Vm, inv: &Inventory, moves: &[DiskMove]) -> Vm {
    let mut vm = vm.clone();
    vm.spec.disks.retain(|d| {
        let id = inv
            .volumes
            .iter()
            .find(|v| v.name == d.volume)
            .map(|v| v.id);
        !moves.iter().any(|m| Some(m.volume) == id)
    });
    vm
}

/// 마이그레이션 API가 거절한 결과. VM이 꺼졌거나 지워졌거나 이미 다른 노드로 갔으면 옮길 것이 없다.
fn rejected(e: ApiError) -> Outcome {
    match e.code {
        "vm_not_running" | "not_found" | "vm_moved" => Outcome::Gone,
        "vm_busy" => Outcome::NotMoved("busy".into()),
        _ => Outcome::NotMoved(e.message),
    }
}

/// 마이그레이션 Task를 기다리는 한도: `migration_timeout`의 두 배와 1분. 디스크를 복사하면
/// (`copies_disks`) `migration_disk_timeout`을 더한다.
fn migration_wait(config: &TaskRunnerConfig, copies_disks: bool) -> Duration {
    let wait = config.migration_timeout * 2 + Duration::from_secs(60);
    if copies_disks {
        wait + config.migration_disk_timeout
    } else {
        wait
    }
}

/// 마이그레이션 Task가 끝나기를 기다린다. Task는 단계마다 시간 제한이 있지만, 읽기가 계속 실패하거나
/// 끝나지 않으면 `wait`([`migration_wait`]) 뒤에 그만 기다린다.
async fn wait_migration(ctx: &Ctx, task: Uuid, wait: Duration) -> Outcome {
    let deadline = Instant::now() + wait;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_a_disk_copy_adds_the_disk_timeout() {
        let mut config = TaskRunnerConfig::new(Uuid::nil());
        config.migration_timeout = Duration::from_secs(30 * 60);
        config.migration_disk_timeout = Duration::from_secs(12 * 3600);
        assert_eq!(migration_wait(&config, false), Duration::from_secs(61 * 60));
        assert_eq!(
            migration_wait(&config, true),
            Duration::from_secs(12 * 3600 + 61 * 60)
        );
    }
}
