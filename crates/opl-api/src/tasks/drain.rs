//! 노드 비우기 Task(설계 §3.2): 비울 노드의 실행 중인 VM을 이름 순으로 하나씩 다른 노드로 라이브
//! 마이그레이션한다. 옮기지 못한 VM은 건너뛰고 끝에 이유와 함께 실패로 알린다. 이 노드로 들어오던
//! 마이그레이션은 끝나기를 기다려 그 VM도 옮기므로, 성공하면 노드가 비어 있다.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use opl_types::node::{Node, NodeSpec};
use opl_types::storage::is_shared;
use opl_types::task::{TaskOp, TaskSpec, TaskState};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::time::Instant;
use uuid::Uuid;

use super::Ctx;
use crate::error::ApiError;
use crate::scheduler::{Inventory, node_fits};
use crate::state::AppState;
use crate::vms::start_migration;

/// VM 하나를 옮겨 본 결과
enum Outcome {
    Moved,
    /// 옮길 것이 없어졌다(꺼졌거나 지워졌거나 다른 노드로 갔다).
    Gone,
    /// 옮기지 못했다(이유)
    NotMoved(String),
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
            match move_vm(ctx, &state, node, &vm).await? {
                Outcome::Moved => {
                    moved.insert(vm.id);
                    None
                }
                Outcome::Gone => {
                    *gone.entry(vm.id).or_default() += 1;
                    None
                }
                Outcome::NotMoved(reason) => Some(reason),
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
        .filter(|v| v.status.migration.is_some_and(|m| m.node == node))
        .map(|v| v.id)
        .chain(
            tasks
                .iter()
                .filter(|t| {
                    t.spec.op == TaskOp::VmMigrate { node } && !t.status.state.is_finished()
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
/// 끝나면 옮기지 않고 비우기를 멈추는 오류다.
async fn move_vm(ctx: &Ctx, state: &AppState, node: Uuid, vm: &Vm) -> Result<Outcome, String> {
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
    let shared = vm.spec.disks.iter().all(|disk| {
        inv.volumes
            .iter()
            .find(|v| v.name == disk.volume)
            .and_then(|v| inv.pools.iter().find(|p| p.name == v.spec.pool))
            .is_some_and(is_shared)
    });
    if !shared {
        return Ok(Outcome::NotMoved("disks on a local pool".into()));
    }
    // HVF는 이 노드의 hypervisor 이름으로만 알 수 있다(macOS 노드).
    if me.status.hypervisor == HVF {
        return Ok(Outcome::NotMoved("hvf node".into()));
    }
    let Some(target) = pick_target(vm, &inv) else {
        return Ok(Outcome::NotMoved("no node with capacity".into()));
    };
    let task = match start_migration(state, vm.id, target, Some(node)).await {
        // 확인과 만들기 사이에 다른 Task가 들어왔다. 한 번 더 자리 잡기를 기다려 다시 한다.
        Err(e) if e.code == "vm_busy" => {
            match settle(ctx, node, vm.id).await {
                Ok(true) => {}
                Ok(false) => return Ok(Outcome::Gone),
                Err(e) => return Ok(Outcome::NotMoved(e)),
            }
            in_maintenance(ctx, node)?;
            start_migration(state, vm.id, target, Some(node)).await
        }
        r => r,
    };
    match task {
        Ok(task) => Ok(wait_migration(ctx, task.id).await),
        Err(e) => Ok(rejected(e)),
    }
}

/// VM을 받을 수 있는 노드(스케줄러의 조건을 지나고 HVF가 아니며 남은 메모리가 VM 메모리 이상) 가운데
/// 남은 메모리가 가장 많은 노드. 같으면 id가 작은 노드다. 스케줄러는 유지보수 노드(이 노드)를 고르지
/// 않는다.
fn pick_target(vm: &Vm, inv: &Inventory) -> Option<Uuid> {
    inv.nodes
        .iter()
        .filter(|n| n.status.hypervisor != HVF && node_fits(&vm.spec, inv, n).is_ok())
        .map(|n| (inv.free_memory_mib(n, vm.id), n.id))
        .filter(|(free, _)| *free >= i64::from(vm.spec.memory_mib))
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)))
        .map(|(_, id)| id)
}

/// 마이그레이션 API가 거절한 결과. VM이 꺼졌거나 지워졌거나 이미 다른 노드로 갔으면 옮길 것이 없다.
fn rejected(e: ApiError) -> Outcome {
    match e.code {
        "vm_not_running" | "not_found" | "vm_moved" => Outcome::Gone,
        "vm_busy" => Outcome::NotMoved("busy".into()),
        _ => Outcome::NotMoved(e.message),
    }
}

/// 마이그레이션 Task가 끝나기를 기다린다. Task는 단계마다 시간 제한이 있지만, 읽기가 계속 실패하거나
/// 끝나지 않으면 `migration_timeout`의 두 배와 1분 뒤에 그만 기다린다.
async fn wait_migration(ctx: &Ctx, task: Uuid) -> Outcome {
    let deadline = Instant::now() + ctx.config.migration_timeout * 2 + Duration::from_secs(60);
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
