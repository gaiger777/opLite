//! 노드 자신의 역할을 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`, 리더의 `VoterKeeper`가 씀)에
//! 맞춘다(설계 §6). 모든 노드가 띄운다.
//!
//! - learner인데 목록에 있으면 관찰 노드가 된다([`Cluster::become_observer`]).
//! - 관찰 노드인데 목록에 없고 저장된 멤버십에 있으면(리더가 learner로 더함) Raft 구성원이 된다
//!   ([`Cluster::become_member`]).
//! - 투표 멤버는 아무것도 하지 않는다.
//! - 대비책: 멤버 모드의 비투표 노드의 적용이 `stalled_after` 넘게 멈추면(리더를 모르는 것도) 투표 멤버에게
//!   Follow로 자기 역할을 묻고(`Hello.role`), 관찰 노드라 하면 관찰 노드가 된다(꺼진 동안 내려져 목록을 적용하지
//!   못한 노드). openraft 0.9.25는 커밋된 투표를 가진 learner가 다시 시작하면 리더 소식이 없어도 그 투표의 리더를
//!   `current_leader`로 계속 알리고 팔로워의 마지막 하트비트 시각을 지표에 내지 않으므로, "리더를 모름" 대신
//!   적용이 멈춘 것으로 본다. 쓰기가 없는 클러스터에서도 묻게 되지만 묻는 것은 Follow 하나(첫 프레임만)다.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::Cluster;
use crate::follow::NodeRole;

#[derive(Debug, Clone)]
pub struct RoleKeeperConfig {
    /// 살펴보는 간격
    pub interval: Duration,
    /// 멤버 모드의 비투표 노드의 적용이 이만큼 멈추면 투표 멤버에게 역할을 묻는다.
    pub stalled_after: Duration,
}

impl Default for RoleKeeperConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            stalled_after: Duration::from_secs(30),
        }
    }
}

/// 이 노드의 역할(관찰 노드 ↔ 멤버)을 목록에 맞춘다.
pub struct RoleKeeper {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RoleKeeper {
    pub fn spawn(cluster: Arc<Cluster>, config: RoleKeeperConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, config, rx)),
        }
    }

    /// 멈춘다. 진행 중인 전환은 끝까지 기다린다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 이번 틱에 할 전환
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    BecomeObserver,
    BecomeMember,
}

async fn run(cluster: Arc<Cluster>, config: RoleKeeperConfig, mut shutdown: oneshot::Receiver<()>) {
    // 멤버 모드의 비투표 노드의 적용 위치와 그것이 그대로인 첫 시각
    let mut stall = None;
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let step = tokio::select! {
            _ = &mut shutdown => return,
            step = decide(&cluster, &config, &mut stall) => step,
        };
        let Some(step) = step else {
            continue;
        };
        // 전환은 도중에 취소되면 안 되므로 따로 띄운 작업에서 하고 끝을 기다린다(종료 요청이 와도 끝까지 간다).
        let task = {
            let cluster = cluster.clone();
            tokio::spawn(async move {
                match step {
                    Step::BecomeObserver => cluster.become_observer().await,
                    Step::BecomeMember => cluster.become_member().await,
                }
            })
        };
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::info!(error = %e, ?step, "cannot change this node's role yet"),
            Err(e) => tracing::warn!(error = %e, ?step, "the role change task failed"),
        }
    }
}

async fn decide(
    cluster: &Cluster,
    config: &RoleKeeperConfig,
    stall: &mut Option<(u64, Instant)>,
) -> Option<Step> {
    let me = cluster.id();
    let listed = cluster.listed_as_observer();
    let Some(raft) = cluster.raft() else {
        *stall = None;
        let members = cluster.members();
        let in_raft = members.voters.contains_key(&me) || members.learners.contains_key(&me);
        return (!listed && in_raft).then_some(Step::BecomeMember);
    };
    let m = raft.metrics().borrow().clone();
    if m.membership_config
        .membership()
        .voter_ids()
        .any(|v| v == me)
    {
        *stall = None;
        return None;
    }
    if listed {
        *stall = None;
        return Some(Step::BecomeObserver);
    }
    let revision = cluster.revision();
    let leader_known = m.current_leader.is_some();
    let since = match *stall {
        Some((seen, since)) if seen == revision => since,
        _ => {
            *stall = Some((revision, Instant::now()));
            return None;
        }
    };
    if since.elapsed() < config.stalled_after {
        return None;
    }
    // 다음 물음은 다시 `stalled_after` 뒤에 한다.
    *stall = Some((revision, Instant::now()));
    match cluster.role_from_voters().await {
        Some(NodeRole::Observer) => {
            tracing::info!(
                leader_known,
                "this node's log has stalled and a voting member says it is an observer; following it"
            );
            Some(Step::BecomeObserver)
        }
        _ => None,
    }
}
