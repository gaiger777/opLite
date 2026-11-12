//! 노드 생존 감시. 리더만 주기적으로 노드의 생존을 보고, 노드의 온라인 여부가 바뀔 때만
//! `NodeStatus.online`을 기록한다(Raft에 주기적인 쓰기가 생기지 않는다). 스케줄러가 이 값을 본다.
//!
//! 모든 멤버(위트니스 포함)가 [`LEASE_LIVENESS`]를 알면 리더의 리스 표를 본다: 모든 노드가 이미 리더에
//! 리스를 갱신하므로(HA 펜싱을 끈 노드와 위트니스는 [`renew_liveness`]로) 따로 묻지 않는다. 옛 노드가
//! 남았으면(롤링 업그레이드) 지금처럼 다른 노드에 `Ping`을 보낸다(옛 노드는 펜싱을 끄면 갱신하지 않는다).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError, Members};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::client::Peers;

/// 이 기능을 아는 노드는 HA 펜싱과 관계없이 리스를 갱신한다. 모든 노드가 알면 리더가 리스로 생존을 본다.
pub const LEASE_LIVENESS: &str = "lease-liveness";

#[derive(Debug, Clone)]
pub struct NodeMonitorConfig {
    /// `Ping`을 보내는 간격
    pub interval: Duration,
    /// 마지막 응답 뒤 이만큼 지나면 오프라인이다.
    pub offline_after: Duration,
}

impl Default for NodeMonitorConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            offline_after: Duration::from_secs(10),
        }
    }
}

pub struct NodeMonitor {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl NodeMonitor {
    pub fn spawn(cluster: Arc<Cluster>, peers: Arc<Peers>, config: NodeMonitorConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, peers, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// HA 펜싱을 쓰지 않는 노드(펜싱을 끈 노드, 위트니스)의 생존 표시. 펜싱 없이 리스만 `interval`마다
/// 갱신한다(펜싱을 쓰는 노드는 `HaGuard`가 갱신한다). 한 번은 간격의 두 배까지 기다린다. 멈추려면
/// 돌려준 작업을 abort한다.
pub fn renew_liveness(cluster: Arc<Cluster>, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let limit = interval * 2;
        let mut failing = false;
        loop {
            let sent = Instant::now();
            let result = tokio::time::timeout(limit, cluster.renew_lease(limit))
                .await
                .unwrap_or(Err(ClusterError::Timeout));
            match result {
                Ok(()) if failing => {
                    tracing::info!("liveness lease renewed again");
                    failing = false;
                }
                Ok(()) => {}
                Err(e) if !failing => {
                    tracing::warn!(error = %e, "cannot renew the liveness lease");
                    failing = true;
                }
                Err(_) => {}
            }
            tokio::time::sleep_until(sent + interval).await;
        }
    })
}

/// 리더가 기억하는 생존 기록. 리더가 아니게 되면 비운다.
#[derive(Default)]
struct Seen {
    /// ping: 노드별 마지막 응답 시각. 리더가 된 시점에 비우고, 온라인으로 기록된 노드는 처음에 그 시점을
    /// 응답으로 쳐서 새 리더가 곧바로 오프라인으로 만들지 않게 한다. 오프라인으로 기록된 노드에는
    /// 유예가 없다(응답해야 온라인이 된다).
    pinged: HashMap<Uuid, Instant>,
    /// 리스: 리더로 본 임기와 그 임기에서 리스로 보기 시작한 시각(기준점). 리더가 바뀌면 리스 표가
    /// 비므로, 온라인으로 기록된 노드는 기준점부터 리스 하나만큼 기다린다.
    base: Option<(u64, Instant)>,
    /// 리스: 오프라인으로 기록된 노드의 그 기록(리소스 revision)과 그것을 처음 본 시각. 그 뒤에 받은 갱신이
    /// 있어야 온라인이 된다(정상 종료한 노드가 스스로 오프라인으로 기록한 뒤, 그 전의 갱신으로 다시 온라인이
    /// 되지 않게). 감시 사이에 온라인 → 오프라인으로 바뀌어 온라인을 보지 못했어도 revision이 다르면 새
    /// 기록으로 본다.
    offline_since: HashMap<Uuid, (u64, Instant)>,
    /// ping으로 보는 까닭(리스를 모르는 노드)을 경고한 임기. 임기마다 한 번만 적는다.
    warned: Option<u64>,
}

async fn run(
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    config: NodeMonitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut seen = Seen::default();
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&cluster, &peers, &config, &mut seen) => {}
        }
    }
}

async fn tick(cluster: &Cluster, peers: &Arc<Peers>, config: &NodeMonitorConfig, seen: &mut Seen) {
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        *seen = Seen::default();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
        return;
    };
    // Raft 구성원이 아닌 노드 기록(빠진 노드의 남은 기록 등)은 갱신하지 않으므로 리스로 볼지 정할 때 보지 않는다
    // (그런 노드는 어느 쪽이든 오프라인이다).
    let old: Vec<&str> = nodes
        .iter()
        .filter(|n| {
            members.contains(NodeId(n.id)) && !n.status.features.iter().any(|f| f == LEASE_LIVENESS)
        })
        .map(|n| n.name.as_str())
        .collect();
    let by_lease = old.is_empty();
    let term = cluster.raft().metrics().borrow().current_term;
    if !by_lease && seen.warned != Some(term) {
        seen.warned = Some(term);
        tracing::warn!(
            nodes = %old.join(","),
            "watching nodes by ping: these members do not renew a liveness lease yet"
        );
    }
    let online = if by_lease {
        seen.pinged.clear();
        by_leases(cluster, &members, &nodes, config, seen)
    } else {
        seen.base = None;
        seen.offline_since.clear();
        by_pings(cluster, peers, &members, &nodes, config, &mut seen.pinged).await
    };
    for (node, online) in nodes.iter().zip(online) {
        if node.status.online == online {
            continue;
        }
        // 핑을 기다리는 동안 리더가 바뀌었으면 기록하지 않는다.
        if cluster.members().leader != Some(cluster.id()) {
            *seen = Seen::default();
            return;
        }
        // 목록을 읽은 뒤 노드가 스스로 등록한 상태를 덮어쓰지 않도록 쓰기 직전에 다시 읽고, 그 사이
        // 노드가 status(풀 상태 등)를 썼으면 쓰지 않는다(다음 핑에서 다시 본다).
        let Ok(Some(fresh)) = cluster.get::<NodeSpec>(node.id) else {
            continue;
        };
        // 목록을 읽은 뒤 다른 쪽(정상 종료하는 노드의 오프라인 기록 등)이 온라인 여부를 바꿨으면 그 값을
        // 두고 다음 감시에서 다시 본다.
        if fresh.status.online == online || fresh.status.online != node.status.online {
            continue;
        }
        let status = NodeStatus {
            online,
            ..fresh.status
        };
        match cluster
            .update_status_checked::<NodeSpec>(node.id, fresh.revision, &status)
            .await
        {
            Ok(_) => tracing::info!(node = %node.name, online, "node availability changed"),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(node = %node.name, error = %e, "cannot record node availability")
            }
        }
    }
}

/// 리더의 리스 표로 본 노드별 생존(`nodes` 순서). 마지막 갱신에서 `offline_after`가 지나지 않았으면
/// 온라인이다. 리더 자신은 늘 온라인, 멤버가 아닌 노드(제거되는 중)는 오프라인이다.
fn by_leases(
    cluster: &Cluster,
    members: &Members,
    nodes: &[Node],
    config: &NodeMonitorConfig,
    seen: &mut Seen,
) -> Vec<bool> {
    let leases = cluster.leases();
    let now = Instant::now();
    let base = match seen.base {
        Some((term, b)) if term == leases.term => b,
        _ => {
            seen.base = Some((leases.term, now));
            seen.offline_since.clear();
            now
        }
    };
    nodes
        .iter()
        .map(|node| {
            let id = NodeId(node.id);
            if id == cluster.id() {
                return true;
            }
            if !members.contains(id) {
                return false;
            }
            let renewed = leases.renewed.get(&id).copied();
            let last = if node.status.online {
                seen.offline_since.remove(&node.id);
                // 이 임기에 갱신을 받기 전에는 기준점을 마지막 갱신으로 친다.
                Some(renewed.map_or(base, |r| r.max(base)))
            } else {
                let since = seen
                    .offline_since
                    .entry(node.id)
                    .and_modify(|(revision, at)| {
                        if *revision != node.revision {
                            *revision = node.revision;
                            *at = now;
                        }
                    })
                    .or_insert((node.revision, now))
                    .1;
                renewed.filter(|r| *r > since)
            };
            last.is_some_and(|at| now.saturating_duration_since(at) < config.offline_after)
        })
        .collect()
}

/// 다른 노드에 `Ping`을 보내 본 노드별 생존(`nodes` 순서, 옛 노드가 섞인 클러스터).
async fn by_pings(
    cluster: &Cluster,
    peers: &Arc<Peers>,
    members: &Members,
    nodes: &[Node],
    config: &NodeMonitorConfig,
    last_seen: &mut HashMap<Uuid, Instant>,
) -> Vec<bool> {
    let now = Instant::now();
    let pings = nodes.iter().map(|node| {
        let addr = members.addr(NodeId(node.id)).map(str::to_string);
        let peers = peers.clone();
        let target = NodeId(node.id);
        let me = target == cluster.id();
        let timeout = config.interval;
        async move {
            match addr {
                _ if me => Some(true),
                // 그 주소에 이제 다른 노드가 있을 수 있으므로 응답한 노드가 대상인지 본다.
                Some(addr) => Some(
                    peers
                        .ping(&addr, timeout)
                        .await
                        .is_ok_and(|id| id == target),
                ),
                // 멤버가 아니다(제거되는 중).
                None => None,
            }
        }
    });
    let results = futures_util::future::join_all(pings).await;
    nodes
        .iter()
        .zip(results)
        .map(|(node, result)| match result {
            None => false,
            Some(ok) => {
                if ok {
                    last_seen.insert(node.id, now);
                } else if node.status.online {
                    last_seen.entry(node.id).or_insert(now);
                }
                last_seen
                    .get(&node.id)
                    .is_some_and(|seen| now.duration_since(*seen) < config.offline_after)
            }
        })
        .collect()
}
