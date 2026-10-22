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
    // 노드별 마지막 응답 시각. 리더가 된 시점에 비우고, 처음에는 그 시점을 응답으로 쳐서
    // 새 리더가 다른 노드를 곧바로 오프라인으로 만들지 않게 한다.
    let mut last_seen: HashMap<Uuid, Instant> = HashMap::new();
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let members = cluster.members();
        if members.leader != Some(cluster.id()) {
            last_seen.clear();
            continue;
        }
        let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
            continue;
        };
        let now = Instant::now();
        let pings = nodes.iter().map(|node| {
            let addr = members.addr(NodeId(node.id)).map(str::to_string);
            let peers = peers.clone();
            let me = node.id == cluster.id().0;
            let timeout = config.interval;
            async move {
                match addr {
                    _ if me => Some(true),
                    Some(addr) => Some(peers.ping(&addr, timeout).await.is_ok()),
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
                    let seen = last_seen.entry(node.id).or_insert(now);
                    if ok {
                        *seen = now;
                    }
                    now.duration_since(*seen) < config.offline_after
                }
            };
            if node.status.online != online {
                tracing::info!(node = %node.name, online, "node availability changed");
                let status = NodeStatus {
                    online,
                    ..node.status.clone()
                };
                if let Err(e) = cluster.update_status::<NodeSpec>(node.id, &status).await {
                    tracing::warn!(node = %node.name, error = %e, "cannot record node availability");
                }
            }
        }
    }
}
