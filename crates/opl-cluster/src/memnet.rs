//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크와 리더 전달자. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 `opl-peer`에 있다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::Raft;
use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};

use crate::cluster::{Cluster, ClusterError, NodeHandle};
use crate::follow::{FollowClient, FollowFrame};
use crate::leader::{self, Forwarder};
use crate::links::{Failure, LinkBook, LinkSelector};
use crate::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};

/// 전달받은 요청을 리더가 처리하는 최대 시간
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Routes {
    /// 노드의 백엔드. RPC를 보낼 때마다 지금 백엔드를 읽는다.
    nodes: HashMap<NodeId, NodeHandle>,
    isolated: HashSet<NodeId>,
    /// Raft RPC만 끊긴 노드. 리더 전달은 된다.
    raft_blocked: HashSet<NodeId>,
    /// 보낸 Raft RPC는 닿지만 답을 받지 못하는 노드
    replies_lost: HashSet<NodeId>,
    /// 노드에 건 Follow 수
    follows: HashMap<NodeId, usize>,
    /// Follow 프레임을 받지 못하고 멈춘 노드(풀면 이어 받는다)
    follow_paused: HashSet<NodeId>,
    /// 스냅샷 조각을 하나마다 이만큼 늦게 받는 노드
    slow_snapshots: HashMap<NodeId, Duration>,
    /// 보내는 노드의 링크 목록과 링크 선택(`Peers`처럼 노드마다 하나)
    senders: HashMap<NodeId, Sender>,
    /// 끊긴 (보내는 노드, 링크). 그 노드가 그 링크로 보내는 호출은 연결하지 못한다.
    cut: HashSet<(NodeId, String)>,
    /// 링크마다 보낸 호출 수(끊긴 링크로 보낸 것 포함)
    sent: HashMap<String, u64>,
}

/// 보내는 노드 하나가 링크를 고르는 것
#[derive(Clone)]
struct Sender {
    book: Arc<LinkBook>,
    links: Arc<LinkSelector>,
}

/// 호출 하나가 고른 링크. 결과를 보내는 노드의 링크 선택에 알린다.
struct LinkUse {
    links: Option<Arc<LinkSelector>>,
    addr: String,
    link: String,
    /// 고른 때
    sent: tokio::time::Instant,
}

impl LinkUse {
    fn report(&self, reached: bool) {
        if let Some(links) = &self.links {
            if reached {
                links.ok(&self.addr, &self.link, self.sent);
            } else {
                links.failed(&self.addr, &self.link, Failure::Connect);
            }
        }
    }
}

/// 노드 등록부. 등록되지 않았거나 격리된 노드로 가는 RPC는 `Unreachable`이 된다.
#[derive(Clone, Default)]
pub struct MemRouter {
    routes: Arc<Mutex<Routes>>,
}

impl MemRouter {
    /// `from` 노드가 쓸 네트워크. `Cluster::open`에 넘긴다.
    pub fn network(&self, from: NodeId) -> MemNetwork {
        MemNetwork {
            from,
            router: self.clone(),
        }
    }

    /// `from` 노드가 쓸 리더 전달자. `Cluster::with_forwarder`에 넘긴다.
    pub fn forwarder(&self, from: NodeId) -> Arc<MemForwarder> {
        Arc::new(MemForwarder {
            from,
            router: self.clone(),
        })
    }

    /// `from` 노드(관찰 노드)가 쓸 Follow 통로. `Cluster::open_observer`에 넘긴다.
    pub fn follower(&self, from: NodeId) -> Arc<MemFollow> {
        Arc::new(MemFollow {
            from,
            router: self.clone(),
        })
    }

    /// 노드를 등록한다. 그 노드가 보내는 호출은 노드의 링크 책(멤버십의 `OrbNode`)과 이 노드의 링크 선택으로
    /// 링크를 고른다.
    pub fn register(&self, id: NodeId, cluster: &Cluster) {
        let mut routes = self.routes.lock().unwrap();
        routes.nodes.insert(id, cluster.node_handle());
        routes.senders.insert(
            id,
            Sender {
                book: cluster.link_book(),
                links: Arc::new(LinkSelector::new(cluster.link_retry())),
            },
        );
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    /// 그 노드로 가거나 나가는 Raft RPC만 끊는다. 리더 전달은 그대로 된다(복제가 늦은 노드).
    pub fn block_raft(&self, id: NodeId) {
        self.routes.lock().unwrap().raft_blocked.insert(id);
    }

    /// 그 노드가 보낸 Raft RPC는 상대가 처리하지만 답은 사라진다(리더가 보낸 항목을 팔로워는 받았는데 리더는
    /// 커밋을 모른다).
    pub fn lose_replies(&self, id: NodeId) {
        self.routes.lock().unwrap().replies_lost.insert(id);
    }

    /// 그 노드가 받는 Follow 프레임을 멈춘다(연결은 그대로, 풀면 이어 받는다). 프레임이 늦게 오는 관찰 노드.
    pub fn pause_follow(&self, id: NodeId) {
        self.routes.lock().unwrap().follow_paused.insert(id);
    }

    /// 그 노드가 받는 스냅샷 조각을 하나마다 `delay`씩 늦춘다(다른 프레임은 그대로). 스냅샷이 오래 걸리는 관찰 노드.
    pub fn slow_snapshots(&self, id: NodeId, delay: Duration) {
        self.routes.lock().unwrap().slow_snapshots.insert(id, delay);
    }

    /// 노드 `to`에 건 Follow 수
    pub fn follows(&self, to: NodeId) -> usize {
        self.routes
            .lock()
            .unwrap()
            .follows
            .get(&to)
            .copied()
            .unwrap_or(0)
    }

    /// `from`이 `link`로 보내는 호출을 끊는다(연결 실패). 다른 링크는 그대로다.
    pub fn cut_link(&self, from: NodeId, link: &str) {
        self.routes
            .lock()
            .unwrap()
            .cut
            .insert((from, link.to_string()));
    }

    /// 모든 노드가 `link`로 보낸 호출 수(끊긴 동안 보낸 것 포함)
    pub fn sent(&self, link: &str) -> u64 {
        self.routes
            .lock()
            .unwrap()
            .sent
            .get(link)
            .copied()
            .unwrap_or(0)
    }

    /// 노드의 격리·끊긴 Raft·끊긴 링크(`cut_link`) 등을 모두 푼다.
    pub fn heal(&self, id: NodeId) {
        let mut routes = self.routes.lock().unwrap();
        routes.cut.retain(|(from, _)| *from != id);
        routes.isolated.remove(&id);
        routes.raft_blocked.remove(&id);
        routes.replies_lost.remove(&id);
        routes.follow_paused.remove(&id);
        routes.slow_snapshots.remove(&id);
    }

    /// `to`가 받을 Follow 프레임을 멈춘 동안, 또는 늦춘 스냅샷 조각이면 그만큼 붙잡는다.
    async fn hold_frame(&self, to: NodeId, frame: &FollowFrame) {
        let started = tokio::time::Instant::now();
        loop {
            let hold = {
                let routes = self.routes.lock().unwrap();
                routes.follow_paused.contains(&to)
                    || (matches!(frame, FollowFrame::SnapshotPart { .. })
                        && routes
                            .slow_snapshots
                            .get(&to)
                            .is_some_and(|d| started.elapsed() < *d))
            };
            if !hold {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// `from`이 첫 링크가 `addr`인 노드로 보낼 호출의 링크를 고른다. `links`가 없으면 `from`의 링크 책에서 찾는다.
    fn pick(&self, from: NodeId, addr: &str, links: Option<&[String]>) -> LinkUse {
        let sender = self.routes.lock().unwrap().senders.get(&from).cloned();
        let links = match (links, &sender) {
            (Some(links), _) => links.to_vec(),
            (None, Some(sender)) => sender.book.links(addr),
            (None, None) => vec![addr.to_string()],
        };
        let (link, sent) = match &sender {
            Some(sender) => {
                let pick = sender.links.pick(addr, &links);
                (pick.link, pick.sent)
            }
            None => (
                links.first().map_or(addr, String::as_str).to_string(),
                tokio::time::Instant::now(),
            ),
        };
        *self
            .routes
            .lock()
            .unwrap()
            .sent
            .entry(link.clone())
            .or_default() += 1;
        LinkUse {
            links: sender.map(|s| s.links),
            addr: addr.to_string(),
            link,
            sent,
        }
    }

    fn is_cut(&self, from: NodeId, link: &str) -> bool {
        self.routes
            .lock()
            .unwrap()
            .cut
            .contains(&(from, link.to_string()))
    }

    /// `from`이 고른 링크로 `to`에 닿는다. 끊긴 링크거나 닿지 않으면 연결 실패로 알린다.
    fn route_via(
        &self,
        from: NodeId,
        to: NodeId,
        link: &LinkUse,
    ) -> Result<NodeHandle, Unreachable> {
        let routed = if self.is_cut(from, &link.link) {
            Err(unreachable("link cut"))
        } else {
            self.route(from, to)
        };
        link.report(routed.is_ok());
        routed
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<NodeHandle, Unreachable> {
        let routes = self.routes.lock().unwrap();
        if routes.isolated.contains(&from) || routes.isolated.contains(&to) {
            return Err(unreachable("isolated"));
        }
        routes
            .nodes
            .get(&to)
            .cloned()
            .ok_or_else(|| unreachable("not registered"))
    }

    /// 받는 노드의 Raft와, 답을 버려야 하는지
    fn raft_route(
        &self,
        from: NodeId,
        to: NodeId,
        link: &LinkUse,
    ) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let node = self.route_via(from, to, link)?;
        let lost = {
            let routes = self.routes.lock().unwrap();
            if routes.raft_blocked.contains(&from) || routes.raft_blocked.contains(&to) {
                return Err(unreachable("raft blocked"));
            }
            routes.replies_lost.contains(&from)
        };
        let raft = node
            .raft()
            .ok_or_else(|| unreachable("not a Raft member"))?;
        Ok((raft, lost))
    }
}

fn unreachable(why: &str) -> Unreachable {
    Unreachable::new(&std::io::Error::other(why.to_string()))
}

pub struct MemForwarder {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl Forwarder for MemForwarder {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let link = self.router.pick(self.from, addr, None);
        let node = self
            .router
            .route_via(self.from, leader, &link)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        match node.leader_ctx() {
            Some(ctx) => leader::execute(&ctx, req, FORWARD_TIMEOUT).await,
            None => Err(node.not_leader()),
        }
    }
}

pub struct MemFollow {
    from: NodeId,
    router: MemRouter,
}

#[async_trait]
impl FollowClient for MemFollow {
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        *self
            .router
            .routes
            .lock()
            .unwrap()
            .follows
            .entry(to)
            .or_default() += 1;
        let link = self.router.pick(self.from, addr, None);
        let node = self
            .router
            .route_via(self.from, to, &link)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        let frames = node.follow(self.from, since)?;
        // 프레임마다 길을 다시 본다. 두 노드 가운데 하나가 격리되거나 등록이 풀리거나 쓰는 링크가 끊기면 연결이 끊긴
        // 것처럼 끝난다.
        let (router, from, link) = (self.router.clone(), self.from, link.link);
        let held = router.clone();
        Ok(frames
            .then(move |frame| {
                let router = held.clone();
                async move {
                    router.hold_frame(from, &frame).await;
                    frame
                }
            })
            .take_while(move |_| {
                std::future::ready(router.route(from, to).is_ok() && !router.is_cut(from, &link))
            })
            .map(Ok)
            .boxed())
    }
}

#[derive(Clone)]
pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            node: node.clone(),
            router: self.router.clone(),
        }
    }
}

/// Raft RPC 연결. 링크는 `new_client`가 받은 멤버십의 노드 값에서 고른다.
pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    node: OrbNode,
    router: MemRouter,
}

impl MemConnection {
    fn route(&self) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let link = self
            .router
            .pick(self.from, self.node.addr(), Some(&self.node.addrs));
        self.router.raft_route(self.from, self.target, &link)
    }
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let (raft, lost) = self.route()?;
        let result = raft
            .append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let (raft, lost) = self.route()?;
        let result = raft
            .install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let (raft, lost) = self.route()?;
        let result = raft
            .vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)));
        // 받은 쪽은 처리했지만 보낸 쪽은 답을 받지 못한다.
        if lost {
            return Err(unreachable("reply lost").into());
        }
        result
    }
}
