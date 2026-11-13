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

    pub fn register(&self, id: NodeId, cluster: &Cluster) {
        self.routes
            .lock()
            .unwrap()
            .nodes
            .insert(id, cluster.node_handle());
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

    pub fn heal(&self, id: NodeId) {
        let mut routes = self.routes.lock().unwrap();
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
    ) -> Result<(Raft<TypeConfig>, bool), Unreachable> {
        let node = self.route(from, to)?;
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
        _addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let node = self
            .router
            .route(self.from, leader)
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
        _addr: &str,
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
        let node = self
            .router
            .route(self.from, to)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        let frames = node.follow(self.from, since)?;
        // 프레임마다 길을 다시 본다. 두 노드 가운데 하나가 격리되거나 등록이 풀리면 연결이 끊긴 것처럼 끝난다.
        let (router, from) = (self.router.clone(), self.from);
        let held = router.clone();
        Ok(frames
            .then(move |frame| {
                let router = held.clone();
                async move {
                    router.hold_frame(from, &frame).await;
                    frame
                }
            })
            .take_while(move |_| std::future::ready(router.route(from, to).is_ok()))
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

    async fn new_client(&mut self, target: NodeId, _node: &OrbNode) -> MemConnection {
        MemConnection {
            from: self.from,
            target,
            router: self.router.clone(),
        }
    }
}

pub struct MemConnection {
    from: NodeId,
    target: NodeId,
    router: MemRouter,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let (raft, lost) = self.router.raft_route(self.from, self.target)?;
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
        let (raft, lost) = self.router.raft_route(self.from, self.target)?;
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
        let (raft, lost) = self.router.raft_route(self.from, self.target)?;
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
