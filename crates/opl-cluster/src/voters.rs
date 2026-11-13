//! 투표 멤버 상한. 리더의 [`VoterKeeper`]가 투표 멤버를 클러스터 설정 `voters`(3·5·7, 기본 5)로 맞춘다.
//! 나머지 노드는 learner로 남는다(쓰기 확정과 선거는 투표 멤버 과반만 기다린다).
//!
//! 한 번에 하나만 바꾼다(죽은 투표 멤버의 교체는 변경 한 번). 바꾸기 전과 뒤의 구성 모두에서 살아 있는(최근
//! `alive_within` 안에 이 리더에 리스를 갱신한) 투표 멤버가 과반일 때만 바꾼다: 두 단계 변경의 중간(joint)
//! 구성은 두 구성의 과반이 모두 있어야 커밋된다. 고르는 순서는 [`plan`]에 있다. 모든 노드가 [`VOTER_LIMIT`]를
//! 알 때만 움직인다: 그 전에는 지금처럼 노드가 스스로 투표 멤버 승격을 요청한다(옛 노드, 롤링 업그레이드).
//!
//! openraft의 멤버십 변경은 joint를 커밋한 뒤 uniform을 제안하는 두 단계라, 그 사이에 끊기면(시간 초과, 리더
//! 교체) joint가 남는다. 키퍼는 남은 joint를 마무리한다.
//!
//! 관찰 노드(설계 §6): 투표 멤버와 대기 learner([`STANDBY_LEARNERS`]) 밖의 노드는 Raft 밖의 관찰 노드다. 키퍼는
//! 클러스터 정보의 관찰 노드 목록(`ClusterStatus.observers`)을 쓴다. 내리기는 목록에 더한 뒤(노드의
//! `RoleKeeper`가 보고 관찰 노드가 된다) 다음 틱에 멤버십에서 뺀다(`RemoveNodes`). 올리기는 learner로 더한 뒤
//! 목록에서 뺀다(그 사이 끊기면 "목록에 있는 learner"라 다시 내려간다: 어느 쪽이든 목록과 멤버십 가운데 한 곳에는
//! 남는다). 투표 멤버는 목록에 두지 않는다(있으면 목록에서 뺀다).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use openraft::error::{ChangeMembershipError, ClientWriteError, RaftError};
use openraft::raft::ClientWriteResponse;
use openraft::{ChangeMembers, LogId, Raft, RaftMetrics, ServerState};
use opl_types::cluster::{ClusterInfo, ClusterSpec, ClusterStatus};
use opl_types::node::{Node, NodeSpec};
use opl_types::settings::{SETTINGS_NAME, SettingsSpec};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cluster::{Cluster, ClusterError};
use crate::types::{NodeId, OrbNode, TypeConfig};

/// 이 기능을 아는 노드는 투표 멤버 승격을 리더의 `VoterKeeper`에 맡긴다. 모든 노드가 알면 리더가 상한을 지킨다.
pub const VOTER_LIMIT: &str = "voter-limit";

/// 이 기능을 아는 노드는 관찰 노드가 될 수 있다(설계 §6). 모든 노드가 알 때만 키퍼가 learner를 관찰 노드로 내린다.
pub const OBSERVER: &str = "observer";

/// 투표 멤버 밖에 learner로 남기는 건강한 노드 수(위트니스 제외). 나머지는 관찰 노드다. 투표 멤버가 죽으면 이것이
/// 먼저 올라가고, 줄면 관찰 노드를 learner로 올린다.
pub const STANDBY_LEARNERS: usize = 2;

/// 장애 영역을 적는 노드 레이블(노드 설정 `[node] zone`)
pub const ZONE_LABEL: &str = "zone";

/// learner가 리더의 마지막 로그에서 이만큼 안쪽이면 따라잡았다고 보고 올린다.
const CATCH_UP_SLACK: u64 = 64;

#[derive(Debug, Clone)]
pub struct VoterKeeperConfig {
    /// 살펴보는 간격
    pub interval: Duration,
    /// 마지막 리스 갱신이 이만큼 안이면 건강하다(올릴 수 있다). 새 임기의 처음 이만큼은 개수 때문에 내리지 않는다.
    pub healthy_within: Duration,
    /// 마지막 리스 갱신이 이만큼 안이면 살아 있다(과반을 셀 때). openraft 0.9.25의 지표에는 팔로워별 최근
    /// 응답 시각이 없어 리스 갱신(2초마다)으로 본다.
    pub alive_within: Duration,
    /// 투표 멤버의 마지막 리스 갱신이 이만큼 지나면 오프라인으로 보고 올릴 수 있는 learner로 바꾼다.
    pub offline_after: Duration,
    /// 멤버십 변경 하나를 기다리는 시간
    pub change_timeout: Duration,
}

impl Default for VoterKeeperConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(1),
            healthy_within: Duration::from_secs(10),
            alive_within: Duration::from_secs(3),
            offline_after: Duration::from_secs(30),
            change_timeout: Duration::from_secs(10),
        }
    }
}

/// 리더에서 투표 멤버를 상한에 맞춘다. 모든 노드가 띄우고, 리더일 때만 일한다.
pub struct VoterKeeper {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl VoterKeeper {
    pub fn spawn(cluster: Arc<Cluster>, config: VoterKeeperConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(cluster, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// 멤버 하나에 대해 `plan`이 보는 것
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) id: NodeId,
    pub(crate) voter: bool,
    pub(crate) witness: bool,
    /// 노드 레이블 `zone`. 없는 노드끼리는 같은 영역으로 본다.
    pub(crate) zone: Option<String>,
    /// 최근(`healthy_within`)에 리스를 갱신했다.
    pub(crate) healthy: bool,
    /// 아주 최근(`alive_within`)에 이 리더에 리스를 갱신했다(리더 자신은 늘 참). 과반은 이것으로 센다.
    pub(crate) alive: bool,
    /// `offline_after` 넘게 리스를 갱신하지 않았다.
    pub(crate) offline: bool,
    /// learner가 로그를 따라잡았다(투표 멤버는 늘 참).
    pub(crate) caught_up: bool,
    /// Raft 밖의 관찰 노드(클러스터 정보의 목록에 있다)
    pub(crate) observer: bool,
}

/// 멤버십 변경 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    /// learner를 투표 멤버로 올린다.
    Promote(NodeId),
    /// 투표 멤버를 learner로 내린다.
    Demote(NodeId),
    /// 투표 멤버 `out`을 learner로 내리고 learner `by`를 올린다(변경 한 번).
    Replace { out: NodeId, by: NodeId },
    /// learner를 관찰 노드로 내린다(목록에 더한다. 멤버십에서 빼는 것은 다음 틱).
    ToObserver(NodeId),
    /// 관찰 노드를 learner로 올린다(learner로 더한 뒤 목록에서 뺀다).
    ToLearner(NodeId),
}

impl Change {
    /// 바꾼 뒤의 투표 멤버
    fn voters_after(self, before: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
        let mut after = before.clone();
        match self {
            Change::Promote(id) => {
                after.insert(id);
            }
            Change::Demote(id) => {
                after.remove(&id);
            }
            Change::Replace { out, by } => {
                after.remove(&out);
                after.insert(by);
            }
            Change::ToObserver(_) | Change::ToLearner(_) => {}
        }
        after
    }
}

/// 다음에 할 멤버십 변경 하나. 순서:
/// 1. 위트니스 learner를 올린다(위트니스는 늘 투표 멤버이고 상한에 센다).
/// 2. 투표 멤버가 `limit`보다 적으면 건강하고 따라잡은 learner를 올린다. 투표 멤버가 적은 zone의 것 먼저.
/// 3. `limit`보다 많으면 하나를 내린다: 오프라인, 살아 있지 않은 것, 투표 멤버가 많은 zone의 것, 건강하지 않은 것,
///    나중에 들어온 것 순. 새 임기의 처음(`settling`)에는 내리지 않는다(건강 정보가 아직 없다).
/// 4. `limit`만큼이면 오프라인 투표 멤버를 올릴 수 있는 learner로 바꾼다(변경 한 번). 그런 learner가 없으면
///    그대로 둔다: 내리기만 하면 투표 멤버가 줄어 하나만 더 죽어도 정족수가 깨진다(노드 둘과 위트니스에서 하나가
///    오래 꺼진 뒤 다른 하나가 죽는 경우). 노드가 상한 이하면 모두 투표 멤버로 남는다.
/// 5. 관찰 노드(설계 §6, 새 임기의 처음에는 하지 않음): 투표 멤버가 상한보다 적은데 올릴 learner가 없거나, 건강한
///    대기 learner(위트니스가 아닌 비투표 구성원)가 [`STANDBY_LEARNERS`]보다 적으면 건강한 관찰 노드 하나를
///    learner로 올린다(투표 멤버가 적은 zone의 것 먼저). 대기 learner가 그보다 많고 모든 노드가 [`OBSERVER`]를
///    알면(`observers_ready`) 건강한 대기 learner 하나를 관찰 노드로 내린다(대기 learner가 많은 zone의 것, 따라잡지
///    못한 것, 나중에 들어온 것 순). 오프라인 투표 멤버가 있으면 내리지 않는다.
///
/// 위트니스와 리더는 내리지 않는다. 투표 멤버는 관찰 노드로 내리지 않는다. 바꾸기 전이나 뒤의 투표 멤버에서 살아
/// 있는 것이 과반이 아니면 바꾸지 않는다.
pub(crate) fn plan(
    leader: NodeId,
    members: &[Member],
    limit: usize,
    settling: bool,
    observers_ready: bool,
) -> Option<Change> {
    let voters: Vec<&Member> = members.iter().filter(|m| m.voter).collect();
    let change = pick(leader, members, &voters, limit, settling, observers_ready)?;
    let before: BTreeSet<NodeId> = voters.iter().map(|m| m.id).collect();
    let after = change.voters_after(&before);
    (alive_majority(members, &before) && alive_majority(members, &after)).then_some(change)
}

/// `voters` 가운데 살아 있는 것이 과반이다.
fn alive_majority(members: &[Member], voters: &BTreeSet<NodeId>) -> bool {
    let alive = members
        .iter()
        .filter(|m| m.alive && voters.contains(&m.id))
        .count();
    alive * 2 > voters.len()
}

fn pick(
    leader: NodeId,
    members: &[Member],
    voters: &[&Member],
    limit: usize,
    settling: bool,
    observers_ready: bool,
) -> Option<Change> {
    let demotable = |m: &&Member| !m.witness && m.id != leader;
    let promotable = |m: &&Member| !m.voter && !m.observer && m.healthy && m.caught_up;
    if let Some(m) = members.iter().filter(promotable).find(|m| m.witness) {
        return Some(Change::Promote(m.id));
    }
    // `except`를 뺀 투표 멤버 가운데 `zone`의 것의 수
    let in_zone = |zone: &Option<String>, except: Option<NodeId>| {
        voters
            .iter()
            .filter(|v| v.zone == *zone && Some(v.id) != except)
            .count()
    };
    // 올릴 learner: 투표 멤버가 적은 zone의 것, 같으면 id가 작은 것
    let best = |except: Option<NodeId>| {
        members
            .iter()
            .filter(promotable)
            .min_by_key(|m| (in_zone(&m.zone, except), m.id))
            .map(|m| m.id)
    };
    // learner로 올릴 관찰 노드: 건강한 것 가운데 투표 멤버가 적은 zone의 것, 같으면 id가 작은 것. 새 임기의
    // 처음에는 건강 정보가 없어 올리지 않는다.
    let returning = members
        .iter()
        .filter(|m| m.observer && m.healthy && !settling)
        .min_by_key(|m| (in_zone(&m.zone, None), m.id))
        .map(|m| Change::ToLearner(m.id));
    if voters.len() < limit {
        return best(None).map(Change::Promote).or(returning);
    }
    if voters.len() > limit {
        if settling {
            return None;
        }
        let m = voters.iter().copied().filter(demotable).max_by_key(|m| {
            (
                m.offline,
                !m.alive,
                in_zone(&m.zone, None),
                !m.healthy,
                m.id,
            )
        })?;
        return Some(Change::Demote(m.id));
    }
    let dead = voters.iter().copied().filter(demotable).find(|m| m.offline);
    if let Some(dead) = dead
        && let Some(by) = best(Some(dead.id))
    {
        return Some(Change::Replace { out: dead.id, by });
    }
    if settling {
        return None;
    }
    // 대기 learner: 건강한 비투표 Raft 구성원(위트니스는 늘 투표 멤버로 올라가므로 세지 않는다)
    let standby: Vec<&Member> = members
        .iter()
        .filter(|m| !m.voter && !m.observer && !m.witness && m.healthy)
        .collect();
    if standby.len() < STANDBY_LEARNERS {
        return returning;
    }
    if standby.len() > STANDBY_LEARNERS && observers_ready && dead.is_none() {
        let in_standby_zone =
            |zone: &Option<String>| standby.iter().filter(|s| s.zone == *zone).count();
        return standby
            .iter()
            .max_by_key(|m| (in_standby_zone(&m.zone), !m.caught_up, m.id))
            .map(|m| Change::ToObserver(m.id));
    }
    None
}

/// [`VOTER_LIMIT`]를 보고하지 않아 키퍼를 막는 멤버. 합류하자마자 죽어 등록하지 못한 learner(Node 기록이 없거나
/// 기능·버전이 비었고, 이 임기에 리스도 없다)는 뺀다: 건강하지 않아 올리지 않고, 다시 떠서 등록하면 기능을
/// 보고한다.
fn blocking_members(
    members: impl Iterator<Item = NodeId>,
    voters: &BTreeSet<NodeId>,
    nodes: &BTreeMap<NodeId, Node>,
    renewed: &BTreeMap<NodeId, Instant>,
) -> Vec<NodeId> {
    members
        .filter(|id| {
            let node = nodes.get(id);
            if node.is_some_and(|n| n.status.features.iter().any(|f| f == VOTER_LIMIT)) {
                return false;
            }
            let unregistered =
                node.is_none_or(|n| n.status.features.is_empty() && n.status.version.is_none());
            voters.contains(id) || !unregistered || renewed.contains_key(id)
        })
        .collect()
}

/// 리더의 [`VoterKeeper`]가 투표 멤버를 고르는지: [`VOTER_LIMIT`]를 보고하지 않아 막는 멤버가 없다. 키퍼와 노드
/// (가입·재시작 때 스스로 승격을 요청할지, opld)가 이 규칙 하나를 쓴다. 규칙이 다르면 키퍼가 움직이는데 노드가
/// 스스로 승격해 승격·강등이 반복된다. `renewed`는 이 임기에 리스를 갱신한 멤버(리더의 리스 표)이고, 그것을
/// 모르는 노드는 비워 넘긴다(그러면 키퍼보다 덜 막지는 않는다).
pub fn leader_manages_voters(
    members: impl IntoIterator<Item = NodeId>,
    voters: &BTreeSet<NodeId>,
    nodes: &BTreeMap<NodeId, Node>,
    renewed: &BTreeMap<NodeId, Instant>,
) -> bool {
    blocking_members(members.into_iter(), voters, nodes, renewed).is_empty()
}

/// 키퍼가 틱 사이에 기억하는 것. 리더가 아니게 되면 비운다.
#[derive(Default)]
struct State {
    /// 리더로 본 임기와 그 임기에 살펴보기 시작한 시각(기준점). 새 리더의 리스 표는 비어 있으므로 투표 멤버는
    /// 기준점에 갱신한 것으로 친다(곧바로 오프라인으로 보지 않는다).
    base: Option<(u64, Instant)>,
    /// 이 키퍼가 시작했지만 끝을 보지 못한 변경(실패, 시간 초과)이 있다. 첫 단계(joint)만 커밋됐을 수 있다.
    unfinished: bool,
    /// 지금 보이는 joint 구성의 로그와 그것을 처음 본 시각
    joint: Option<(LogId<NodeId>, Instant)>,
    /// 마무리하지 못했다고 경고한 joint 구성의 로그(joint마다 한 번)
    finish_warned: Option<LogId<NodeId>>,
    /// 키퍼를 막는 멤버를 경고한 임기(임기마다 한 번)
    warned: Option<u64>,
    /// 관찰 노드 목록에 있는 learner(내려가는 중)를 처음 본 시각
    listed_since: BTreeMap<NodeId, Instant>,
}

async fn run(
    cluster: Arc<Cluster>,
    config: VoterKeeperConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut state = State::default();
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 과반이 없으면 변경이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&cluster, &config, &mut state) => {}
        }
    }
}

async fn tick(cluster: &Cluster, config: &VoterKeeperConfig, state: &mut State) {
    // Raft 구성원이 아니면 리더일 수 없다.
    let Some(raft) = cluster.raft() else {
        *state = State::default();
        return;
    };
    let m = raft.metrics().borrow().clone();
    if m.state != ServerState::Leader {
        *state = State::default();
        return;
    }
    if m.membership_config.membership().get_joint_config().len() > 1 {
        finish_joint(cluster, config, state, &m).await;
        return;
    }
    state.joint = None;
    let leases = cluster.leases();
    let since = match state.base {
        Some((term, at)) if term == leases.term => at,
        _ => {
            // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
            if cluster.read_barrier().await.is_err() {
                return;
            }
            let at = Instant::now();
            state.base = Some((leases.term, at));
            at
        }
    };
    let Ok((_, nodes)) = cluster.list::<NodeSpec>() else {
        return;
    };
    let nodes: BTreeMap<NodeId, _> = nodes.into_iter().map(|n| (NodeId(n.id), n)).collect();
    let name = |id: NodeId| {
        nodes
            .get(&id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    };
    let membership = m.membership_config.membership();
    let voters: BTreeSet<NodeId> = membership.voter_ids().collect();
    // 옛 노드는 스스로 투표 멤버가 되려 하고 펜싱을 끄면 리스를 갱신하지 않는다. 모든 멤버가 기능을 알 때까지
    // 둔다(투표 멤버의 Node 리소스가 아직 없으면 합류 중이라 어느 버전인지 모른다).
    let blocking = blocking_members(
        membership.nodes().map(|(id, _)| *id),
        &voters,
        &nodes,
        &leases.renewed,
    );
    if !blocking.is_empty() {
        if state.warned != Some(leases.term) {
            state.warned = Some(leases.term);
            let names: Vec<String> = blocking.into_iter().map(name).collect();
            tracing::warn!(
                nodes = %names.join(","),
                "not managing the voting members: these members have not reported {VOTER_LIMIT}"
            );
        }
        return;
    }
    let limit = match cluster.get_by_name::<SettingsSpec>(SETTINGS_NAME) {
        Ok(settings) => settings.map(|s| s.spec).unwrap_or_default().voter_limit(),
        Err(_) => return,
    };
    // 클러스터 정보의 관찰 노드 목록(클러스터 정보가 없으면 관찰 노드를 둘 곳이 없다)
    let Ok((_, infos)) = cluster.list::<ClusterSpec>() else {
        return;
    };
    let info = infos.into_iter().next();
    let listed: BTreeMap<NodeId, String> = info
        .as_ref()
        .map(|i| {
            i.status
                .observers
                .iter()
                .map(|(id, addr)| (NodeId(*id), addr.clone()))
                .collect()
        })
        .unwrap_or_default();
    state.listed_since.retain(|id, _| listed.contains_key(id));
    if let Some(info) = &info
        && fix_listed(
            cluster, config, state, &raft, &m, info, &listed, &voters, &name,
        )
        .await
    {
        return;
    }
    let now = Instant::now();
    let age = |at: Instant| now.saturating_duration_since(at);
    let observers = listed
        .keys()
        .copied()
        .filter(|id| membership.get_node(id).is_none());
    let members: Vec<Member> = membership
        .nodes()
        .map(|(&id, _)| id)
        .chain(observers)
        .map(|id| {
            let node = nodes.get(&id);
            let voter = voters.contains(&id);
            let observer = membership.get_node(&id).is_none();
            let me = id == cluster.id();
            let renewed = leases.renewed.get(&id).copied();
            let last = if me {
                Some(now)
            } else if voter {
                Some(renewed.map_or(since, |r| r.max(since)))
            } else {
                renewed
            };
            Member {
                id,
                voter,
                witness: node.is_some_and(|n| n.spec.witness),
                zone: node.and_then(|n| n.labels.get(ZONE_LABEL).cloned()),
                healthy: last.is_some_and(|at| age(at) < config.healthy_within),
                // 기준점의 유예 없이 이 임기에 실제로 받은 갱신으로만 본다.
                alive: me || renewed.is_some_and(|at| age(at) < config.alive_within),
                offline: last.is_none_or(|at| age(at) >= config.offline_after),
                caught_up: voter || (!observer && caught_up(&m, id)),
                observer,
            }
        })
        .collect();
    // 모든 멤버와 관찰 노드가 관찰 노드 기능을 알 때만 learner를 내린다(옛 노드는 목록을 모른다).
    let observers_ready = members.iter().all(|member| {
        nodes
            .get(&member.id)
            .is_some_and(|n| n.status.features.iter().any(|f| f == OBSERVER))
    });
    let settling = age(since) < config.healthy_within;
    let Some(change) = plan(cluster.id(), &members, limit, settling, observers_ready) else {
        return;
    };
    // 계획은 장벽·목록 읽기 전의 지표로 세웠다. 그 사이 구성이 바뀌었으면(다른 변경, 리더 교체) 다음 틱에 다시 세운다.
    if !same_config(cluster, &m) {
        return;
    }
    let (changes, done) = match change {
        Change::Promote(id) => (
            ChangeMembers::AddVoterIds(BTreeSet::from([id])),
            format!("promoted {} to a voting member", name(id)),
        ),
        // learner로 남긴다(`retain`).
        Change::Demote(id) => (
            ChangeMembers::RemoveVoters(BTreeSet::from([id])),
            format!("demoted {} to a learner", name(id)),
        ),
        Change::Replace { out, by } => (
            ChangeMembers::ReplaceAllVoters(change.voters_after(&voters)),
            format!(
                "replaced the offline voting member {} with {}",
                name(out),
                name(by)
            ),
        ),
        Change::ToObserver(id) => {
            let (Some(info), Some(node)) = (&info, membership.get_node(&id)) else {
                return;
            };
            let mut next = listed.clone();
            next.insert(id, node.addr().to_string());
            match write_observers(cluster, info, next).await {
                Ok(_) => tracing::info!("moving {} out of Raft as an observer", name(id)),
                Err(e) => tracing::warn!(error = %e, ?change, "cannot list an observer"),
            }
            return;
        }
        Change::ToLearner(id) => {
            return_observer(cluster, config, &raft, id, &listed, &name).await;
            return;
        }
    };
    state.unfinished = true;
    match change_members(&raft, changes, config.change_timeout).await {
        Outcome::Done => {
            state.unfinished = false;
            tracing::info!(limit, "{done}");
        }
        // 다른 변경(위트니스 승격, API의 노드 제거)이 커밋 중이다. 아무것도 쓰지 않았으므로 다음 틱에 그 뒤의
        // 구성으로 계획을 다시 세운다.
        Outcome::Busy => state.unfinished = false,
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, ?change, "cannot change the voting members")
        }
    }
}

/// 목록과 멤버십이 겹친 것을 고친다. 무언가 했거나 기다려야 하면 `true`(이번 틱에는 다른 것을 하지 않는다).
/// - 목록에 있는 투표 멤버는 목록에서 뺀다(투표 멤버는 관찰 노드로 내리지 않는다).
/// - 목록에 있는 learner(내려가는 중)는 그 노드가 목록을 받을 시간(간격 두 번, 리더가 목록 쓰기까지 복제했거나
///   `change_timeout`이 지남)을 준 뒤 멤버십에서 뺀다(`RemoveNodes`, openraft는 투표 멤버를 이것으로 빼지 않는다).
///   꺼져 있어 목록을 받지 못한 노드는 다시 켜졌을 때 `RoleKeeper`의 대비책이 관찰 노드로 만든다.
#[allow(clippy::too_many_arguments)]
async fn fix_listed(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    state: &mut State,
    raft: &Raft<TypeConfig>,
    m: &RaftMetrics<NodeId, OrbNode>,
    info: &ClusterInfo,
    listed: &BTreeMap<NodeId, String>,
    voters: &BTreeSet<NodeId>,
    name: &impl Fn(NodeId) -> String,
) -> bool {
    let stray: Vec<NodeId> = listed
        .keys()
        .copied()
        .filter(|id| voters.contains(id))
        .collect();
    if !stray.is_empty() {
        let next = listed
            .iter()
            .filter(|(id, _)| !voters.contains(id))
            .map(|(id, addr)| (*id, addr.clone()))
            .collect();
        match write_observers(cluster, info, next).await {
            Ok(_) => tracing::warn!(
                nodes = %stray.iter().map(|id| name(*id)).collect::<Vec<_>>().join(","),
                "voting members were listed as observers; unlisted them"
            ),
            Err(e) => tracing::warn!(error = %e, "cannot unlist voting members from the observers"),
        }
        return true;
    }
    let membership = m.membership_config.membership();
    let pending: Vec<NodeId> = listed
        .keys()
        .copied()
        .filter(|id| membership.get_node(id).is_some())
        .collect();
    if pending.is_empty() {
        return false;
    }
    let now = Instant::now();
    let ready: BTreeSet<NodeId> = pending
        .into_iter()
        .filter(|id| {
            let waited =
                now.saturating_duration_since(*state.listed_since.entry(*id).or_insert(now));
            waited >= config.interval * 2
                && (matched(m, *id) >= Some(info.revision) || waited >= config.change_timeout)
        })
        .collect();
    if ready.is_empty() || !same_config(cluster, m) {
        return true;
    }
    let names: Vec<String> = ready.iter().map(|id| name(*id)).collect();
    match change_members(
        raft,
        ChangeMembers::RemoveNodes(ready),
        config.change_timeout,
    )
    .await
    {
        Outcome::Done => {
            tracing::info!(nodes = %names.join(","), "moved learners out of Raft as observers")
        }
        Outcome::Busy => {}
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, nodes = %names.join(","), "cannot move learners out of Raft")
        }
    }
    true
}

/// 관찰 노드를 learner로 더한 뒤 목록에서 뺀다. 목록에서 빼지 못하면 다음 틱에 "목록에 있는 learner"로 보여 다시
/// 관찰 노드가 된다(목록과 멤버십 어느 쪽에도 없는 노드는 생기지 않는다).
async fn return_observer(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    raft: &Raft<TypeConfig>,
    id: NodeId,
    listed: &BTreeMap<NodeId, String>,
    name: &impl Fn(NodeId) -> String,
) {
    let Some(addr) = listed.get(&id) else {
        return;
    };
    let added = tokio::time::timeout(
        config.change_timeout,
        raft.add_learner(id, OrbNode::single(addr.clone()), false),
    )
    .await;
    match outcome(added) {
        Outcome::Done => {}
        Outcome::Busy => return,
        Outcome::Failed(e) => {
            tracing::warn!(error = %e, node = %name(id), "cannot add an observer back as a learner");
            return;
        }
    }
    // learner를 더한 쓰기는 이 노드(리더)에 적용된 뒤 돌아온다. 목록은 그 뒤에 다시 읽는다.
    let Ok((_, infos)) = cluster.list::<ClusterSpec>() else {
        return;
    };
    let Some(info) = infos.into_iter().next() else {
        return;
    };
    let mut next: BTreeMap<NodeId, String> = info
        .status
        .observers
        .iter()
        .map(|(id, addr)| (NodeId(*id), addr.clone()))
        .collect();
    next.remove(&id);
    match write_observers(cluster, &info, next).await {
        Ok(_) => tracing::info!("added the observer {} back as a learner", name(id)),
        Err(e) => tracing::warn!(error = %e, node = %name(id), "cannot unlist an observer"),
    }
}

/// 클러스터 정보의 관찰 노드 목록을 `observers`로 바꾼다(읽은 뒤 다른 쪽이 썼으면 `RevisionMismatch`).
async fn write_observers(
    cluster: &Cluster,
    info: &ClusterInfo,
    observers: BTreeMap<NodeId, String>,
) -> Result<u64, ClusterError> {
    let status = ClusterStatus {
        observers: observers
            .into_iter()
            .map(|(id, addr)| (id.0, addr))
            .collect(),
    };
    cluster
        .update_status_checked::<ClusterSpec>(info.id, info.revision, &status)
        .await
}

/// 리더가 그 노드에 복제했다고 아는 마지막 로그 인덱스
fn matched(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> Option<u64> {
    m.replication
        .as_ref()
        .and_then(|r| r.get(&id).copied().flatten())
        .map(|l| l.index)
}

/// 두 단계 변경의 첫 단계(joint)까지만 된 구성을 uniform으로 마무리한다. 다른 쪽의 변경이 곧 두 번째 단계를
/// 제안할 수 있으므로 이 키퍼의 변경이 끊겼거나, joint가 이전 임기의 것이거나(그 리더의 두 번째 단계는 오지
/// 않는다), `change_timeout` 넘게 그대로일 때만 한다. 어느 때든 joint가 바뀌지 않은 채 간격 두 번 넘게 보였어야
/// 한다(살아 있는 두 번째 단계가 먼저 끝나게).
///
/// 마무리는 빠지는 투표 멤버를 learner로 남긴다. 노드를 지우는 변경(`LeaderRequest::Remove`)의 joint를 마무리했으면
/// 그 요청이 끝난 뒤 멤버십을 다시 읽어 learner로 남은 노드를 지운다(`leader::execute`).
async fn finish_joint(
    cluster: &Cluster,
    config: &VoterKeeperConfig,
    state: &mut State,
    m: &RaftMetrics<NodeId, OrbNode>,
) {
    let log_id = *m.membership_config.log_id();
    let now = Instant::now();
    let seen = match state.joint {
        Some((id, at)) if Some(id) == log_id => at,
        _ => {
            state.joint = log_id.map(|id| (id, now));
            now
        }
    };
    let earlier_term = log_id.is_some_and(|l| l.leader_id.term < m.current_term);
    let unchanged = now.saturating_duration_since(seen);
    if unchanged < config.interval * 2
        || (!state.unfinished && !earlier_term && unchanged < config.change_timeout)
    {
        return;
    }
    // 아무도 더하지 않는 승격은 joint를 그 마지막(새) 구성 하나로 만든다(openraft 0.9.25 `Membership::change`의
    // `AddVoterIds`: 새 투표 멤버 = 마지막 구성 ∪ {}, `find_coherent`는 그것을 품은 joint에서 uniform을 준다).
    let finish = ChangeMembers::AddVoterIds(BTreeSet::new());
    let Some(raft) = cluster.raft() else {
        return;
    };
    match change_members(&raft, finish, config.change_timeout).await {
        Outcome::Done => {
            state.unfinished = false;
            state.joint = None;
            tracing::info!("finished an interrupted change of the voting members");
        }
        // joint가 아직 커밋되지 않았다(새 리더의 빈 항목이 커밋하면 다음 틱에 마무리한다).
        Outcome::Busy => {}
        Outcome::Failed(e) => {
            if state.finish_warned != log_id {
                state.finish_warned = log_id;
                tracing::warn!(error = %e, "cannot finish an interrupted change of the voting members");
            }
        }
    }
}

/// 지금도 리더이고 멤버십 구성이 `m`(계획을 세운 지표)의 것 그대로다.
fn same_config(cluster: &Cluster, m: &RaftMetrics<NodeId, OrbNode>) -> bool {
    let Some(raft) = cluster.raft() else {
        return false;
    };
    let now = raft.metrics().borrow().clone();
    now.state == ServerState::Leader
        && now.current_term == m.current_term
        && now.membership_config.log_id() == m.membership_config.log_id()
}

/// 멤버십 변경 한 번의 결과
enum Outcome {
    Done,
    /// 앞선 멤버십 변경이 아직 커밋 중이라 거절됐다(아무것도 쓰지 않았다).
    Busy,
    /// 실패했거나 시간 안에 끝나지 않았다. 첫 단계(joint)만 커밋됐을 수 있다.
    Failed(String),
}

/// 투표 멤버를 바꾼다. 빠진 투표 멤버는 learner로 남긴다(`retain`).
async fn change_members(
    raft: &Raft<TypeConfig>,
    changes: ChangeMembers<NodeId, OrbNode>,
    timeout: Duration,
) -> Outcome {
    outcome(tokio::time::timeout(timeout, raft.change_membership(changes, true)).await)
}

type MembershipResult = Result<
    Result<ClientWriteResponse<TypeConfig>, RaftError<NodeId, ClientWriteError<NodeId, OrbNode>>>,
    tokio::time::error::Elapsed,
>;

fn outcome(result: MembershipResult) -> Outcome {
    match result {
        Err(_) => Outcome::Failed("timed out".into()),
        Ok(Ok(_)) => Outcome::Done,
        Ok(Err(RaftError::APIError(ClientWriteError::ChangeMembershipError(
            ChangeMembershipError::InProgress(_),
        )))) => Outcome::Busy,
        Ok(Err(e)) => Outcome::Failed(e.to_string()),
    }
}

/// learner가 리더의 마지막 로그를 거의 따라잡았는지(리더의 복제 지표)
fn caught_up(m: &RaftMetrics<NodeId, OrbNode>, id: NodeId) -> bool {
    let matched = m
        .replication
        .as_ref()
        .and_then(|r| r.get(&id).copied().flatten())
        .map(|l| l.index);
    let last = m.last_log_index.unwrap_or(0);
    matched.is_some_and(|i| i + CATCH_UP_SLACK >= last)
}

#[cfg(test)]
mod tests {
    use opl_types::node::NodeStatus;
    use opl_types::resource::Resource;

    use super::*;

    fn member(n: u64, voter: bool, zone: Option<&str>) -> Member {
        Member {
            id: NodeId::from(n),
            voter,
            witness: false,
            zone: zone.map(str::to_string),
            healthy: true,
            alive: true,
            offline: false,
            caught_up: true,
            observer: false,
        }
    }

    /// 꺼진 지 `offline_after`가 지난 멤버
    fn dead(m: &mut Member) {
        m.healthy = false;
        m.alive = false;
        m.offline = true;
    }

    fn id(n: u64) -> NodeId {
        NodeId::from(n)
    }

    #[test]
    fn learners_are_promoted_up_to_the_limit_preferring_other_zones() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, false, Some("a")),
            member(4, false, Some("b")),
            member(5, false, None),
        ];
        // zone b·없음에 투표 멤버가 없다. 같으면 id가 작은 것.
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(4)))
        );
        members[3].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(5)))
        );
        members[4].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(3)))
        );
        members[2].voter = true;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        // 노드가 상한보다 적으면 모두 투표 멤버다.
        assert_eq!(plan(id(1), &members, 7, false, false), None);
    }

    #[test]
    fn only_healthy_caught_up_learners_are_promoted() {
        let mut members = vec![member(1, true, None), member(2, false, None)];
        members[1].healthy = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        members[1].healthy = true;
        members[1].caught_up = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        members[1].caught_up = true;
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Promote(id(2)))
        );
    }

    #[test]
    fn extra_voters_are_demoted_from_crowded_zones_but_never_the_leader_or_a_witness() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, true, Some("a")),
            member(4, true, Some("b")),
            member(5, true, Some("b")),
            member(6, true, Some("c")),
            member(7, true, Some("c")),
        ];
        // zone a가 셋으로 가장 많다. 리더(3)는 빼고 id가 큰 것.
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(2)))
        );
        members[1].voter = false;
        // 이제 모든 zone이 둘이다. 건강하지 않은 것 먼저.
        members[3].healthy = false;
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(4)))
        );
        members[3].healthy = true;
        // 위트니스는 내리지 않는다.
        members[6].witness = true;
        assert_eq!(
            plan(id(3), &members, 5, false, false),
            Some(Change::Demote(id(6)))
        );
        // 새 임기의 처음에는 개수 때문에 내리지 않는다.
        assert_eq!(plan(id(3), &members, 5, true, false), None);
    }

    #[test]
    fn witnesses_are_always_voters_and_count_toward_the_limit() {
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, false, None),
        ];
        members[3].witness = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Promote(id(4)))
        );
        members[3].voter = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(3)))
        );
    }

    /// 상한만큼의 투표 멤버 가운데 하나가 오프라인이면 learner 하나로 한 번에 바꾼다(빠진 zone의 것 먼저).
    #[test]
    fn an_offline_voter_is_replaced_by_a_learner_in_one_change() {
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("b")),
            member(3, true, Some("c")),
            member(4, false, Some("a")),
            member(5, false, Some("c")),
        ];
        dead(&mut members[2]);
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Replace {
                out: id(3),
                by: id(5)
            })
        );
        // 오프라인 위트니스는 바꾸지 않는다.
        members[2].witness = true;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 올릴 수 있는 learner가 없으면 그대로 둔다.
        members[2].witness = false;
        members[3].healthy = false;
        members[4].caught_up = false;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    /// 노드 여섯·상한 다섯에서 죽은 투표 멤버는 남은 learner로 한 번에 바뀐다.
    #[test]
    fn six_nodes_replace_a_dead_voter_in_one_change() {
        let mut members: Vec<Member> = (1..=6).map(|i| member(i, i <= 5, None)).collect();
        dead(&mut members[4]);
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Replace {
                out: id(5),
                by: id(6)
            })
        );
    }

    /// 노드가 상한 이하면 오프라인이어도 투표 멤버로 남는다: 노드 둘과 위트니스에서 B를 내리면 그 뒤 A가 죽었을 때
    /// B와 위트니스로 정족수가 안 된다.
    #[test]
    fn an_offline_voter_without_a_replacement_stays_a_voter() {
        // 노드 둘(1, 2)과 위트니스(3)
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
        ];
        members[2].witness = true;
        dead(&mut members[1]);
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 노드 셋
        members[2].witness = false;
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    #[test]
    fn nothing_changes_without_a_live_majority() {
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, true, None),
            member(5, true, None),
            member(6, false, None),
        ];
        for m in &mut members[2..5] {
            dead(m);
        }
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        // 셋이 살아 있으면(과반) 오프라인 하나를 learner로 바꿀 수 있다(바꾼 뒤 다섯 중 넷이 살아 있다).
        members[2] = member(3, true, None);
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::Replace {
                out: id(4),
                by: id(6)
            })
        );
        // 상한보다 많아도 내린 뒤 과반이 살아 있지 않으면 내리지 않는다: 다섯 중 살아 있는 셋(리더, 2, 3) 가운데
        // 하나를 내리면 넷 중 둘만 살아 있다(살아 있지 않은 둘은 위트니스라 내리지 않는다).
        let mut members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            member(4, true, None),
            member(5, true, None),
        ];
        for m in &mut members[3..] {
            m.witness = true;
            m.healthy = false;
            m.alive = false;
        }
        assert_eq!(plan(id(1), &members, 3, false, false), None);
        // 위트니스 하나가 살아 있으면 내릴 수 있다.
        members[3].healthy = true;
        members[3].alive = true;
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(3)))
        );
    }

    /// 새 리더는 리스 표가 비어 투표 멤버를 기준점에 갱신한 것으로 쳐 건강하다고 본다. 과반은 그 유예 없이
    /// 실제로 받은 갱신(살아 있음)으로 세므로, 죽은 투표 멤버가 있으면 살아 있는 투표 멤버를 내리지 않는다.
    #[test]
    fn a_new_leader_does_not_demote_a_live_voter_on_stale_health() {
        // 다섯 가운데 4·5가 죽었지만 아직 건강하다고 보인다. 살아 있는 셋은 붐비는 zone a에 있다.
        let mut members = vec![
            member(1, true, Some("a")),
            member(2, true, Some("a")),
            member(3, true, Some("a")),
            member(4, true, Some("b")),
            member(5, true, Some("c")),
        ];
        for m in &mut members[3..] {
            m.alive = false;
        }
        // 새 임기의 처음에는 내리지 않는다.
        assert_eq!(plan(id(1), &members, 3, true, false), None);
        // 그 뒤에는 살아 있지 않은 것을 먼저 내린다(살아 있는 2·3을 내리면 넷 중 둘만 살아 있다).
        assert_eq!(
            plan(id(1), &members, 3, false, false),
            Some(Change::Demote(id(5)))
        );
        // 살아 있는 투표 멤버만 남은 후보라면 과반이 깨지므로 내리지 않는다.
        members[3].witness = true;
        members[4].witness = true;
        assert_eq!(plan(id(1), &members, 3, false, false), None);
    }

    /// Raft 밖의 관찰 노드
    fn observer(n: u64, zone: Option<&str>) -> Member {
        Member {
            observer: true,
            caught_up: false,
            ..member(n, false, zone)
        }
    }

    /// 투표 멤버 다섯(1..=5)과 learner `learners`
    fn five_voters_and(learners: &[u64]) -> Vec<Member> {
        (1..=5)
            .map(|i| member(i, true, None))
            .chain(learners.iter().map(|&i| member(i, false, None)))
            .collect()
    }

    /// 대기 learner가 둘보다 많으면 모든 노드가 기능을 알 때만 하나를 관찰 노드로 내린다(대기 learner가 많은 zone의
    /// 것, 나중에 들어온 것). 건강하지 않은 learner와 위트니스는 세지도 내리지도 않고, 새 임기의 처음에는 하지 않는다.
    #[test]
    fn extra_standby_learners_become_observers() {
        let mut members = five_voters_and(&[6, 7, 8]);
        assert_eq!(plan(id(1), &members, 5, false, false), None);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToObserver(id(8)))
        );
        assert_eq!(plan(id(1), &members, 5, true, true), None);
        // zone a에 대기 learner가 둘이면 그 가운데 나중 것
        members[5].zone = Some("a".into());
        members[6].zone = Some("a".into());
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToObserver(id(7)))
        );
        // 건강하지 않은 learner는 대기로 세지 않는다(둘만 남아 아무것도 하지 않는다).
        members[7].healthy = false;
        assert_eq!(plan(id(1), &members, 5, false, true), None);
        // 위트니스 learner는 먼저 투표 멤버로 올린다(관찰 노드로 내리지 않는다).
        members[7].healthy = true;
        members[7].witness = true;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Promote(id(8)))
        );
        // 오프라인 투표 멤버가 있으면(바꿀 learner가 따라잡는 중) 내리지 않는다.
        let mut members = five_voters_and(&[6, 7, 8]);
        dead(&mut members[4]);
        for m in &mut members[5..] {
            m.caught_up = false;
        }
        assert_eq!(plan(id(1), &members, 5, false, true), None);
    }

    /// 대기 learner가 둘보다 적거나 투표 멤버가 모자란데 올릴 learner가 없으면 건강한 관찰 노드를 learner로 올린다
    /// (투표 멤버가 적은 zone의 것 먼저). 새 임기의 처음에는 하지 않는다.
    #[test]
    fn observers_come_back_as_learners_when_standbys_run_short() {
        let mut members = five_voters_and(&[6]);
        members.push(observer(7, None));
        members.push(observer(8, Some("b")));
        // 투표 멤버는 모두 zone 없음이다. zone b의 관찰 노드가 먼저.
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(8)))
        );
        assert_eq!(plan(id(1), &members, 5, true, true), None);
        // 건강하지 않은 관찰 노드는 올리지 않는다.
        members[7].healthy = false;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(7)))
        );
        // 대기 learner가 둘이면 그대로다.
        members[6].observer = false;
        members[6].caught_up = true;
        assert_eq!(plan(id(1), &members, 5, false, true), None);
        // 투표 멤버가 상한보다 적은데 올릴 learner가 없다(따라잡는 중이 아니라 아예 없다).
        let members = vec![
            member(1, true, None),
            member(2, true, None),
            member(3, true, None),
            observer(4, None),
        ];
        assert_eq!(
            plan(id(1), &members, 5, false, false),
            Some(Change::ToLearner(id(4)))
        );
    }

    /// 죽은 투표 멤버를 대기 learner로 바꾸면 대기가 줄어 관찰 노드가 learner로 올라온다(그 뒤 다시 죽은 투표
    /// 멤버가 생기면 그것이 올라간다).
    #[test]
    fn a_dead_voter_pulls_an_observer_through_learner_to_voter() {
        let mut members = five_voters_and(&[6, 7]);
        members.push(observer(8, Some("z")));
        dead(&mut members[4]);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Replace {
                out: id(5),
                by: id(6)
            })
        );
        // 바꾼 뒤: 5는 죽은 learner, 6은 투표 멤버. 건강한 대기는 7 하나.
        members[4].voter = false;
        members[5].voter = true;
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::ToLearner(id(8)))
        );
        // 8이 learner로 따라잡은 뒤 투표 멤버 하나가 더 죽으면 zone이 다른 8이 올라간다.
        members[7].observer = false;
        members[7].caught_up = true;
        dead(&mut members[3]);
        assert_eq!(
            plan(id(1), &members, 5, false, true),
            Some(Change::Replace {
                out: id(4),
                by: id(8)
            })
        );
    }

    /// 어떤 경우에도 투표 멤버를 관찰 노드로 내리지 않는다.
    #[test]
    fn voters_never_become_observers() {
        for n in 1..=9u64 {
            for voters in 1..=n {
                for limit in [3, 5, 7] {
                    for settling in [false, true] {
                        let members: Vec<Member> =
                            (1..=n).map(|i| member(i, i <= voters, None)).collect();
                        if let Some(Change::ToObserver(id)) =
                            plan(NodeId::from(1), &members, limit, settling, true)
                        {
                            assert!(!members.iter().any(|m| m.id == id && m.voter));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_observer_feature_is_listed() {
        assert!(opl_types::node::FEATURES.contains(&OBSERVER));
    }

    fn node(n: u64, features: &[&str], version: Option<&str>) -> (NodeId, Node) {
        let mut node = Resource::new(
            format!("n{n}"),
            NodeSpec {
                address: format!("node-{n}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = id(n).0;
        node.status = NodeStatus {
            features: features.iter().map(|f| f.to_string()).collect(),
            version: version.map(str::to_string),
            ..NodeStatus::default()
        };
        (id(n), node)
    }

    /// 합류하자마자 죽어 등록하지 못한 learner(기능·버전 없음, 리스 없음)는 키퍼를 막지 않는다. 옛 노드(버전이
    /// 있거나 리스를 갱신한다)와 기능을 모르는 투표 멤버는 막는다.
    #[test]
    fn a_learner_that_never_registered_does_not_block() {
        let nodes: BTreeMap<NodeId, Node> = [
            node(1, &[VOTER_LIMIT], Some("0.3.0")),
            node(2, &[], None),
            node(3, &[], Some("0.2.0")),
            node(4, &[], None),
            node(6, &[], None),
        ]
        .into_iter()
        .collect();
        let voters = BTreeSet::from([id(1), id(6)]);
        let renewed = BTreeMap::from([(id(4), Instant::now())]);
        // 2: 등록 못 한 learner, 5: Node 기록도 없는 learner — 빼고 본다.
        // 3: 옛 learner, 4: 리스를 갱신하는 learner, 6: 기능 없는 투표 멤버 — 막는다.
        let members = (1..=6).map(id);
        assert_eq!(
            blocking_members(members, &voters, &nodes, &renewed),
            vec![id(3), id(4), id(6)]
        );
        // 노드 쪽이 쓰는 공용 판단도 같다: 등록 못 한 learner만 있으면 리더가 고른다.
        assert!(!leader_manages_voters(
            (1..=6).map(id),
            &voters,
            &nodes,
            &renewed
        ));
        let fine = BTreeSet::from([id(1)]);
        assert!(leader_manages_voters(
            [1, 2, 5].map(id),
            &fine,
            &nodes,
            &BTreeMap::new()
        ));
        // 이 임기에 리스를 갱신한 learner는 등록 전이어도 막는다(리스 표를 아는 리더만).
        let renewing = BTreeMap::from([(id(2), Instant::now())]);
        assert!(!leader_manages_voters(
            [1, 2].map(id),
            &fine,
            &nodes,
            &renewing
        ));
    }

    #[test]
    fn the_feature_is_listed() {
        assert!(opl_types::node::FEATURES.contains(&VOTER_LIMIT));
    }
}
