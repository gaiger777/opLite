//! 한 프로세스 안의 노드들을 잇는 Raft 네트워크와 리더 전달자. 클러스터 테스트에 쓴다.
//! 노드 간 실제 통신(gRPC + mTLS)은 `opl-peer`에 있다.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use openraft::error::{InstallSnapshotError, RPCError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::{BasicNode, Raft};

use crate::cluster::ClusterError;
use crate::leader::{self, Forwarder};
use crate::types::{LeaderRequest, NodeId, TypeConfig};

/// 전달받은 요청을 리더가 처리하는 최대 시간
const FORWARD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Routes {
    nodes: HashMap<NodeId, Raft<TypeConfig>>,
    isolated: HashSet<NodeId>,
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

    pub fn register(&self, id: NodeId, raft: Raft<TypeConfig>) {
        self.routes.lock().unwrap().nodes.insert(id, raft);
    }

    pub fn unregister(&self, id: NodeId) {
        self.routes.lock().unwrap().nodes.remove(&id);
    }

    /// 노드를 네트워크에서 끊는다. 그 노드로 가거나 그 노드에서 나가는 RPC가 모두 실패한다.
    pub fn isolate(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.insert(id);
    }

    pub fn heal(&self, id: NodeId) {
        self.routes.lock().unwrap().isolated.remove(&id);
    }

    fn route(&self, from: NodeId, to: NodeId) -> Result<Raft<TypeConfig>, Unreachable> {
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
        let raft = self
            .router
            .route(self.from, leader)
            .map_err(|e| ClusterError::Unreachable(e.to_string()))?;
        leader::execute(&raft, req, FORWARD_TIMEOUT).await
    }
}

pub struct MemNetwork {
    from: NodeId,
    router: MemRouter,
}

impl RaftNetworkFactory<TypeConfig> for MemNetwork {
    type Network = MemConnection;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> MemConnection {
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
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

impl RaftNetwork<TypeConfig> for MemConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.append_entries(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        _option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let raft = self.router.route(self.from, self.target)?;
        raft.install_snapshot(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let raft = self.router.route(self.from, self.target)?;
        raft.vote(rpc)
            .await
            .map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
