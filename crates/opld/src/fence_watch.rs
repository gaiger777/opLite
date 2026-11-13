//! 이웃 확인(설계 §8.1): 이 노드의 전원 끄기 펜싱 방법(`NodeFence`)이 실제로 BMC에 닿는지 이웃 노드에게 전원을
//! 읽어 달라고 하고(노드 사이 RPC `FenceStatus`, 전원은 건드리지 않는다), 결과를 이 노드의
//! `status.fence_check`에 쓴다. 모든 방법이 읽지 못하면 `fence_unreachable` 경고다. 등록 뒤 곧바로, 그 뒤
//! [`FENCE_CHECK_INTERVAL`]마다, `NodeFence`가 바뀌면(1분마다 본다) 확인한다. 이웃이 [`NEIGHBOUR_FAILURES`]번 이어
//! 확인하지 못하면 그 오류를 쓴다(오래된 결과가 남지 않게).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_api::power_fence::FENCE_CALL_TIMEOUT;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_peer::Peers;
use opl_types::fence::{FenceMethodStatus, NodeFenceSpec, Power, neighbour};
use opl_types::node::{FenceCheck, Node, NodeSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::bootstrap;

/// 같은 설정을 다시 확인하는 간격
pub const FENCE_CHECK_INTERVAL: Duration = Duration::from_secs(3600);
/// `NodeFence`가 바뀌었는지(와 확인할 때인지) 보는 간격
const FENCE_WATCH_POLL: Duration = Duration::from_secs(60);
/// 이웃의 답을 기다리는 시간(이웃이 방법마다 [`FENCE_CALL_TIMEOUT`]까지 기다린다)
const NEIGHBOUR_TIMEOUT: Duration = Duration::from_secs(FENCE_CALL_TIMEOUT.as_secs() + 5);
/// 이웃 확인이 이만큼 이어 실패하면 `fence_check`에 그 오류를 쓴다.
pub const NEIGHBOUR_FAILURES: u32 = 3;

/// 확인하지 못한 이유
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// 이웃에게 부탁했지만 답이 없거나 이웃이 이 RPC를 모른다.
    Neighbour(String),
    /// 저장소를 읽거나 쓰지 못했거나 부탁할 이웃이 없다.
    Other(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Neighbour(e) | CheckError::Other(e) => f.write_str(e),
        }
    }
}

/// 이웃에게 전원 읽기를 부탁한다(시험에서 바꿔 끼운다).
#[async_trait]
pub trait NeighbourCheck: Send + Sync {
    /// 노드 `via`에게 노드 `node`의 전원을 읽어 달라고 한다. `via`가 이 RPC를 모르면 None이다.
    async fn fence_status(
        &self,
        via: &Node,
        node: Uuid,
    ) -> Result<Option<Vec<FenceMethodStatus>>, String>;
}

/// 노드 사이 RPC로 부탁한다(이웃의 주소는 Raft 멤버십의 첫 링크).
pub struct PeerCheck {
    pub cluster: Arc<Cluster>,
    pub peers: Arc<Peers>,
}

#[async_trait]
impl NeighbourCheck for PeerCheck {
    async fn fence_status(
        &self,
        via: &Node,
        node: Uuid,
    ) -> Result<Option<Vec<FenceMethodStatus>>, String> {
        let addr = self
            .cluster
            .members()
            .addr(NodeId(via.id))
            .map(str::to_string)
            .ok_or("it has no cluster address")?;
        tokio::time::timeout(
            NEIGHBOUR_TIMEOUT,
            self.peers
                .fence_status(NodeId(via.id), &addr, node, NEIGHBOUR_TIMEOUT),
        )
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|s| s.message().to_string())
    }
}

/// 이웃 확인 반복 작업
pub struct FenceWatch {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl FenceWatch {
    pub fn spawn(cluster: Arc<Cluster>, peers: Arc<Peers>, node: Uuid) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster.clone(), PeerCheck { cluster, peers }, node, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    cluster: Arc<Cluster>,
    checker: PeerCheck,
    me: Uuid,
    mut shutdown: oneshot::Receiver<()>,
) {
    // 마지막으로 확인한 `NodeFence` revision(없으면 None)과 때
    let mut last: Option<(Option<u64>, Instant)> = None;
    // 이웃 확인이 이어 실패한 수
    let mut failures = 0;
    let mut interval = tokio::time::interval(FENCE_WATCH_POLL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let revision = match cluster.get::<NodeFenceSpec>(me) {
            Ok(fence) => fence.map(|f| f.revision),
            Err(e) => {
                tracing::debug!(error = %e, "cannot read this node's power fencing");
                continue;
            }
        };
        let due = last.is_none_or(|(checked, at)| {
            checked != revision || at.elapsed() >= FENCE_CHECK_INTERVAL
        });
        if !due {
            continue;
        }
        let check = check_counting(&cluster, &checker, me, &mut failures);
        let result = tokio::select! {
            _ = &mut shutdown => return,
            r = check => r,
        };
        match result {
            Ok(checked) => last = Some((checked, Instant::now())),
            // 다음 간격에 다시 한다.
            Err(e) => tracing::info!(error = %e, "cannot check this node's power fencing"),
        }
    }
}

/// [`check_once`]를 하고 이웃 확인의 연속 실패를 `failures`에 센다. [`NEIGHBOUR_FAILURES`]번째 실패에서
/// `fence_check`에 `"no neighbour could check the fence: …"`를 쓴다(그 뒤 실패에는 다시 쓰지 않는다). 확인되면
/// 0으로 돌린다.
pub async fn check_counting(
    cluster: &Cluster,
    checker: &dyn NeighbourCheck,
    me: Uuid,
    failures: &mut u32,
) -> Result<Option<u64>, CheckError> {
    let result = check_once(cluster, checker, me).await;
    match &result {
        Ok(_) => *failures = 0,
        Err(CheckError::Neighbour(e)) => {
            *failures += 1;
            if *failures == NEIGHBOUR_FAILURES {
                let error = format!("no neighbour could check the fence: {e}");
                tracing::warn!(error = %error, "this node's power fencing could not be checked");
                let check = FenceCheck {
                    at: unix_now(),
                    by: String::new(),
                    error: Some(error),
                };
                // 쓰지 못하면 다음 실패에 다시 쓴다.
                if let Err(e) = record(cluster, Some(check)).await {
                    tracing::info!(error = %e, "cannot record the failed power fencing check");
                    *failures -= 1;
                }
            }
        }
        Err(CheckError::Other(_)) => {}
    }
    result
}

/// 한 번 확인한다: `NodeFence`가 없으면 `fence_check`를 지우고, 있으면 이웃([`neighbour`])에게 전원을 읽어 달라고
/// 해 결과를 쓴다. 확인한 `NodeFence` revision(없으면 None)을 돌려준다. 이웃이 없거나 답하지 않으면 오류다(쓰지
/// 않는다).
pub async fn check_once(
    cluster: &Cluster,
    checker: &dyn NeighbourCheck,
    me: Uuid,
) -> Result<Option<u64>, CheckError> {
    let other = |e: String| CheckError::Other(e);
    let fence = cluster
        .get::<NodeFenceSpec>(me)
        .map_err(|e| other(e.to_string()))?;
    let Some(fence) = fence else {
        record(cluster, None).await.map_err(other)?;
        return Ok(None);
    };
    let (_, nodes) = cluster
        .list::<NodeSpec>()
        .map_err(|e| other(e.to_string()))?;
    let via = neighbour(&nodes, me)
        .ok_or_else(|| other("no other online node can check the power".into()))?;
    let methods = checker
        .fence_status(via, me)
        .await
        .map_err(|e| {
            CheckError::Neighbour(format!(
                "node {:?} could not check the power: {e}",
                via.name
            ))
        })?
        .ok_or_else(|| {
            CheckError::Neighbour(format!(
                "node {:?} does not support power fencing",
                via.name
            ))
        })?;
    let check = fence_check(&via.name, &methods, unix_now());
    if let Some(error) = &check.error {
        tracing::warn!(by = %via.name, error = %error, "this node's power fencing cannot be reached; the leader may not be able to power it off");
    }
    record(cluster, Some(check)).await.map_err(other)?;
    Ok(Some(fence.revision))
}

/// 이웃 `by`가 읽은 결과. 모든 방법이 읽지 못했으면(오류·모름) 그 이유들이 `error`다.
pub fn fence_check(by: &str, methods: &[FenceMethodStatus], at: u64) -> FenceCheck {
    let reached = methods
        .iter()
        .any(|m| m.error.is_none() && m.power != Power::Unknown);
    let error = (!reached).then(|| {
        methods
            .iter()
            .map(|m| {
                let why = m.error.as_deref().unwrap_or("power unknown");
                format!("{}: {why}", m.kind.as_str())
            })
            .collect::<Vec<_>>()
            .join("; ")
    });
    FenceCheck {
        at,
        by: by.to_string(),
        error,
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `status.fence_check`를 쓴다(같으면 쓰지 않는다).
async fn record(cluster: &Cluster, check: Option<FenceCheck>) -> Result<(), String> {
    bootstrap::update_node_status(cluster, |status| {
        if status.fence_check == check {
            return false;
        }
        status.fence_check = check.clone();
        true
    })
    .await
    .map_err(|e| format!("cannot record the power fencing check: {e}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use opl_types::fence::{CommandFence, FenceKind, FenceMethod};
    use opl_types::node::{FAST_HA, NodeStatus};
    use opl_types::resource::Resource;

    use super::*;
    use crate::bootstrap::tests::single_node;

    fn status(kind: FenceKind, power: Power, error: Option<&str>) -> FenceMethodStatus {
        FenceMethodStatus {
            kind,
            power,
            error: error.map(Into::into),
        }
    }

    #[test]
    fn the_check_fails_only_when_no_method_reads_the_power() {
        let ok = fence_check(
            "n2",
            &[
                status(FenceKind::Ipmi, Power::Unknown, Some("timed out")),
                status(FenceKind::Command, Power::On, None),
            ],
            7,
        );
        assert_eq!(
            ok,
            FenceCheck {
                at: 7,
                by: "n2".into(),
                error: None
            }
        );
        let bad = fence_check(
            "n2",
            &[
                status(FenceKind::Ipmi, Power::Unknown, Some("timed out")),
                status(FenceKind::Redfish, Power::Unknown, None),
            ],
            7,
        );
        assert_eq!(
            bad.error.as_deref(),
            Some("ipmi: timed out; redfish: power unknown")
        );
    }

    /// 정해 둔 답을 주고 부탁받은 (이웃, 노드)를 적는다.
    struct FakeNeighbour {
        reply: Result<Option<Vec<FenceMethodStatus>>, String>,
        asked: Mutex<Vec<(String, Uuid)>>,
    }

    #[async_trait]
    impl NeighbourCheck for FakeNeighbour {
        async fn fence_status(
            &self,
            via: &Node,
            node: Uuid,
        ) -> Result<Option<Vec<FenceMethodStatus>>, String> {
            self.asked.lock().unwrap().push((via.name.clone(), node));
            self.reply.clone()
        }
    }

    /// 다른 노드 `name`을 저장소에 직접 만든다.
    async fn other_node(cluster: &Cluster, name: &str, online: bool) -> Uuid {
        let mut node = Resource::new(
            name,
            NodeSpec {
                address: "127.0.0.1:1".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.status = NodeStatus {
            online,
            features: vec![FAST_HA.into()],
            ..Default::default()
        };
        cluster.create(&node).await.unwrap();
        node.id
    }

    fn fence_check_of(cluster: &Cluster, id: Uuid) -> Option<FenceCheck> {
        cluster
            .get::<NodeSpec>(id)
            .unwrap()
            .unwrap()
            .status
            .fence_check
    }

    fn neighbour_answering(reply: Result<Option<Vec<FenceMethodStatus>>, String>) -> FakeNeighbour {
        FakeNeighbour {
            reply,
            asked: Mutex::default(),
        }
    }

    #[tokio::test]
    async fn writes_the_neighbours_answer_and_clears_it_without_fencing() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(id)) = single_node(dir.path()).await;
        let mut node = Resource::new(
            "n1",
            NodeSpec {
                address: "127.0.0.1:1".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = id;
        cluster.create(&node).await.unwrap();
        let unreachable = neighbour_answering(Ok(Some(vec![status(
            FenceKind::Command,
            Power::Unknown,
            Some("exit 2"),
        )])));
        // 펜싱이 없으면 묻지 않고 쓰지 않는다.
        assert_eq!(check_once(&cluster, &unreachable, id).await, Ok(None));
        assert!(unreachable.asked.lock().unwrap().is_empty());
        assert_eq!(fence_check_of(&cluster, id), None);
        let mut fence = Resource::new(
            "n1",
            NodeFenceSpec {
                methods: vec![FenceMethod::Command(CommandFence {
                    off: "true".into(),
                    status: "echo on".into(),
                })],
            },
        );
        fence.id = id;
        let revision = cluster.create(&fence).await.unwrap();
        // 이웃(온라인·`fast-ha`인 다른 노드)이 없으면 확인하지 못한다.
        other_node(&cluster, "off", false).await;
        assert!(check_once(&cluster, &unreachable, id).await.is_err());
        assert!(unreachable.asked.lock().unwrap().is_empty());
        other_node(&cluster, "n2", true).await;
        // 모든 방법이 읽지 못하면 `fence_unreachable`
        assert_eq!(
            check_once(&cluster, &unreachable, id).await,
            Ok(Some(revision))
        );
        assert_eq!(*unreachable.asked.lock().unwrap(), [("n2".to_string(), id)]);
        let check = fence_check_of(&cluster, id).unwrap();
        assert_eq!(check.by, "n2");
        assert_eq!(check.error.as_deref(), Some("command: exit 2"));
        assert!(
            cluster
                .get::<NodeSpec>(id)
                .unwrap()
                .unwrap()
                .status
                .fence_unreachable()
        );
        // 이웃이 답하지 않으면 쓰지 않는다(다음에 다시 한다).
        let silent = neighbour_answering(Err("timed out".into()));
        assert!(check_once(&cluster, &silent, id).await.is_err());
        assert_eq!(fence_check_of(&cluster, id), Some(check));
        let reachable =
            neighbour_answering(Ok(Some(vec![status(FenceKind::Command, Power::On, None)])));
        check_once(&cluster, &reachable, id).await.unwrap();
        assert_eq!(fence_check_of(&cluster, id).unwrap().error, None);
        // 이웃이 이어 확인하지 못하면 세 번째에 그 오류를 쓴다(오래된 ok가 남지 않게). 확인되면 다시 센다.
        let mut failures = 0;
        for _ in 1..NEIGHBOUR_FAILURES {
            assert!(
                check_counting(&cluster, &silent, id, &mut failures)
                    .await
                    .is_err()
            );
            assert_eq!(fence_check_of(&cluster, id).unwrap().error, None);
        }
        assert_eq!(
            check_counting(&cluster, &silent, id, &mut failures).await,
            Err(CheckError::Neighbour(
                "node \"n2\" could not check the power: timed out".into()
            ))
        );
        let failed = fence_check_of(&cluster, id).unwrap();
        assert_eq!(
            failed.error.as_deref(),
            Some(
                "no neighbour could check the fence: node \"n2\" could not check the power: timed out"
            )
        );
        assert!(
            cluster
                .get::<NodeSpec>(id)
                .unwrap()
                .unwrap()
                .status
                .fence_unreachable()
        );
        check_counting(&cluster, &reachable, id, &mut failures)
            .await
            .unwrap();
        assert_eq!(failures, 0);
        assert_eq!(fence_check_of(&cluster, id).unwrap().error, None);
        // 펜싱을 지우면 결과도 지운다.
        cluster.delete::<NodeFenceSpec>(id, None).await.unwrap();
        assert_eq!(check_once(&cluster, &reachable, id).await, Ok(None));
        assert_eq!(fence_check_of(&cluster, id), None);
        cluster.shutdown().await.unwrap();
    }
}
