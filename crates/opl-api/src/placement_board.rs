//! 최적 배치 상태(설계 §8.3): 리더가 리스의 노드 부하와 균형 조정기의 마지막 결과(게시판)로 만든다. 다른 노드의
//! API는 노드 사이 RPC(`ReadPlacement`)로 [`PlacementStatusSource`]에 묻는다.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterError};
use opl_types::placement::{ImbalanceView, NodeLoadView, PlacementMove, PlacementStatus};
use opl_types::vm::PowerState;
use tokio::time::Instant;
use uuid::Uuid;

use crate::placement::{Assign, allocated_mib};
use crate::scheduler::Inventory;
use crate::score::{Loads, node_use};

/// 균형 조정기의 마지막 결과(리더 메모리). 리더 자리를 잃으면 균형 조정기가 비운다.
#[derive(Debug, Default)]
pub struct PlacementBoard(Mutex<Board>);

/// 게시판의 내용
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Board {
    /// 마지막 회차(유닉스 초)
    pub last_run: Option<u64>,
    /// 다음 회차(유닉스 초)
    pub next_run: Option<u64>,
    /// 마지막 회차의 불균형(쉬었으면 없음)
    pub imbalance: Option<ImbalanceView>,
    pub paused: Option<String>,
    /// 추천(auto면 시작한 옮기기)
    pub recommendations: Vec<PlacementMove>,
    /// 노드마다 균형 조정기가 살아 있음을 계속 본 처음 때(추천 적용이 다시 본다)
    pub alive_since: BTreeMap<Uuid, Instant>,
    /// 균형 조정기의 리스 기준점
    pub base: Option<Instant>,
    /// 균형 옮기기를 받는 노드가 살아 있어야 하는 시간(`BalancerConfig::settle`)
    pub settle: Duration,
}

impl PlacementBoard {
    pub(crate) fn read(&self) -> Board {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn update(&self, f: impl FnOnce(&mut Board)) {
        f(&mut self.0.lock().unwrap());
    }

    /// 추천 `id`를 적용해 만든 마이그레이션 Task를 적는다(없는 추천이면 그대로).
    pub(crate) fn set_task(&self, id: Uuid, task: Uuid) {
        self.update(|b| {
            if let Some(m) = b.recommendations.iter_mut().find(|m| m.id == id) {
                m.task = Some(task);
            }
        });
    }
}

/// 리더의 최적 배치 상태: 위트니스가 아닌 노드마다 리스의 부하 요약(신선한 것만)과 점수가 보는 사용률, 그리고
/// 게시판. 리더인지는 부르는 쪽이 본다.
pub fn placement_status(
    cluster: &Cluster,
    board: &PlacementBoard,
) -> Result<PlacementStatus, ClusterError> {
    let settings = crate::settings::current(cluster)?.spec.placement();
    let inv = Inventory::load(cluster)?;
    let leases = cluster.leases();
    let now = Instant::now();
    let loads = Loads::from_leases(&leases, &inv.nodes, now);
    let none = Assign::default();
    let mut nodes: Vec<NodeLoadView> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .map(|n| {
            // 점수가 쓰는(신선한) 요약만 보인다.
            let report = leases
                .loads
                .get(&NodeId(n.id))
                .filter(|_| loads.nodes.contains_key(&n.id));
            let used = node_use(n, &inv, &none, &loads, None, None);
            let placed = inv
                .vms
                .iter()
                .filter(|v| v.spec.node == Some(n.id) && v.spec.power == PowerState::Running)
                .count();
            NodeLoadView {
                node: n.id,
                name: n.name.clone(),
                cpu1_pct: report.and_then(|r| r.load.cpu1),
                cpu5_pct: report.and_then(|r| r.load.cpu5),
                mem_used_mib: report.and_then(|r| r.load.mem),
                mem_allocated_mib: allocated_mib(n, &inv, &none, &[]),
                mem_total_mib: n.status.memory_mib,
                cpu_use_pct: permille(used.cpu),
                mem_use_pct: permille(used.mem),
                vms: u32::try_from(placed).unwrap_or(u32::MAX),
                age_secs: report.map(|r| {
                    let age = now.saturating_duration_since(r.at).as_secs();
                    u32::try_from(age).unwrap_or(u32::MAX)
                }),
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let board = board.read();
    Ok(PlacementStatus {
        leader: cluster.id().0,
        settings,
        last_run: board.last_run,
        next_run: board.next_run,
        imbalance: board.imbalance,
        paused: board.paused,
        nodes,
        recommendations: board.recommendations,
    })
}

/// 사용률(0~1)을 0.1% 단위로
pub(crate) fn permille(v: f64) -> u16 {
    (v * 1000.0).round().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// 같은 임기에서 만든 상태를 다시 쓰는 시간. 리더가 아닌 노드가 VM을 놓을 때마다 묻는다.
const STATUS_CACHE: std::time::Duration = std::time::Duration::from_secs(1);

/// 다른 노드가 노드 사이 RPC(`ReadPlacement`)로 묻는 최적 배치 상태. 리더가 아니면 거절한다. 같은 임기에서 만든 지
/// [`STATUS_CACHE`]가 지나지 않은 상태는 다시 만들지 않는다.
pub struct PlacementStatusSource {
    cluster: Arc<Cluster>,
    board: Arc<PlacementBoard>,
    /// 마지막으로 만든 상태(만든 때, 임기)
    cache: std::sync::Mutex<Option<(Instant, u64, PlacementStatus)>>,
}

impl PlacementStatusSource {
    /// `board`는 이 노드 API의 게시판(`AppState::placement`)과 같은 것이다.
    pub fn new(cluster: Arc<Cluster>, board: Arc<PlacementBoard>) -> Self {
        Self {
            cluster,
            board,
            cache: Default::default(),
        }
    }
}

#[async_trait]
impl opl_peer::PlacementSource for PlacementStatusSource {
    async fn status(&self) -> Result<PlacementStatus, String> {
        if self.cluster.members().leader != Some(self.cluster.id()) {
            return Err("not the leader".into());
        }
        let term = self
            .cluster
            .raft()
            .map_or(0, |raft| raft.metrics().borrow().current_term);
        let now = Instant::now();
        if let Some((at, t, status)) = self.cache.lock().unwrap().as_ref()
            && *t == term
            && now.saturating_duration_since(*at) < STATUS_CACHE
        {
            return Ok(status.clone());
        }
        let status = placement_status(&self.cluster, &self.board).map_err(|e| e.to_string())?;
        *self.cache.lock().unwrap() = Some((now, term, status.clone()));
        Ok(status)
    }
}
