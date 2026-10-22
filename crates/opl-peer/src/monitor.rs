//! 노드 생존 감시. 리더만 다른 노드에 주기적으로 `Ping`을 보내고, 노드의 온라인 여부가 바뀔 때만
//! `NodeStatus.online`을 기록한다(Raft에 주기적인 쓰기가 생기지 않는다). 스케줄러가 이 값을 본다.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_types::node::{NodeSpec, NodeStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::client::Peers;

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

async fn run(
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    config: NodeMonitorConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    // 노드별 마지막 응답 시각. 리더가 된 시점에 비우고, 온라인으로 기록된 노드는 처음에 그 시점을
    // 응답으로 쳐서 새 리더가 곧바로 오프라인으로 만들지 않게 한다. 오프라인으로 기록된 노드에는
    // 유예가 없다(응답해야 온라인이 된다).
    let mut last_seen: HashMap<Uuid, Instant> = HashMap::new();
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
            _ = tick(&cluster, &peers, &config, &mut last_seen) => {}
        }
    }
}

async fn tick(
    cluster: &Cluster,
    peers: &Arc<Peers>,
    config: &NodeMonitorConfig,
    last_seen: &mut HashMap<Uuid, Instant>,
) {
    let members = cluster.members();
    if members.leader != Some(cluster.id()) {
        last_seen.clear();
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
        return;
    };
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
    for (node, result) in nodes.iter().zip(results) {
        let online = match result {
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
        };
        if node.status.online == online {
            continue;
        }
        // 핑을 기다리는 동안 리더가 바뀌었으면 기록하지 않는다.
        if cluster.members().leader != Some(cluster.id()) {
            last_seen.clear();
            return;
        }
        // 목록을 읽은 뒤 노드가 스스로 등록한 상태를 덮어쓰지 않도록 쓰기 직전에 다시 읽는다.
        let Ok(Some(fresh)) = cluster.get::<NodeSpec>(node.id) else {
            continue;
        };
        if fresh.status.online == online {
            continue;
        }
        tracing::info!(node = %node.name, online, "node availability changed");
        let status = NodeStatus {
            online,
            ..fresh.status
        };
        if let Err(e) = cluster.update_status::<NodeSpec>(node.id, &status).await {
            tracing::warn!(node = %node.name, error = %e, "cannot record node availability");
        }
    }
}
