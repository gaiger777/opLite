//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use openraft::BasicNode;
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, Forwarder};
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Status, Streaming};
use uuid::Uuid;

use crate::pki::NodeTls;
use crate::proto::peer_client::PeerClient;
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 리더 요청 하나를 기다리는 최대 시간. 리더가 AddVoter에서 새 노드를 따라잡게 기다리는 시간
/// (`opl_cluster::CATCH_UP_TIMEOUT`, 60초)에 쓰기 시간을 더한 것보다 길어야 한다.
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// 다른 노드로 가는 연결. 주소마다 채널 하나를 만들어 다시 쓴다(끊기면 tonic이 다시 잇는다).
/// 연결할 때 이 노드의 인증서를 내밀고, 상대 인증서는 클러스터 CA로 확인한다.
pub struct Peers {
    tls: ClientTlsConfig,
    channels: Mutex<HashMap<String, Channel>>,
}

impl Peers {
    pub fn new(tls: &NodeTls) -> Self {
        Self {
            tls: ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca_pem))
                .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem)),
            channels: Mutex::new(HashMap::new()),
        }
    }

    fn client(&self, addr: &str) -> Result<PeerClient<Channel>, Status> {
        let mut channels = self.channels.lock().unwrap();
        let channel = match channels.get(addr) {
            Some(channel) => channel.clone(),
            None => {
                let channel = Endpoint::from_shared(format!("https://{addr}"))
                    .and_then(|e| e.tls_config(self.tls.clone()))
                    .map_err(|e| Status::invalid_argument(format!("bad address {addr}: {e}")))?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
                    .keep_alive_timeout(KEEPALIVE_TIMEOUT)
                    .keep_alive_while_idle(true)
                    .tcp_keepalive(Some(KEEPALIVE_INTERVAL))
                    .connect_lazy();
                channels.insert(addr.to_string(), channel.clone());
                channel
            }
        };
        Ok(PeerClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES))
    }

    /// `addr`의 노드가 응답하면 그 노드의 id를 돌려준다.
    pub async fn ping(&self, addr: &str, timeout: Duration) -> Result<NodeId, Status> {
        let mut req = Request::new(());
        req.set_timeout(timeout);
        Ok(self.client(addr)?.ping(req).await?.into_inner())
    }

    /// `addr`의 노드에 리더 요청을 보낸다. 그 노드가 리더가 아니면 `NotLeader`다.
    pub async fn leader_request(
        &self,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let mut req = Request::new(req);
        req.set_timeout(LEADER_REQUEST_TIMEOUT);
        // 서버는 처리 결과를 늘 응답 본문에 담아 보내므로, `Status`는 전송 문제다. 새로 연결하지
        // 못하면 `Unavailable`이지만 요청 중에 끊기면 `Cancelled`, `Unknown`, `Internal` 등이 오므로
        // 권한·요청 오류가 아니면 모두 `Unreachable`로 돌려 다시 시도하게 한다.
        let reply = self
            .client(addr)
            .map_err(|s| ClusterError::Raft(s.to_string()))?
            .leader(req)
            .await
            .map_err(|s| match s.code() {
                Code::PermissionDenied | Code::Unauthenticated | Code::InvalidArgument => {
                    ClusterError::Raft(s.to_string())
                }
                _ => ClusterError::Unreachable(s.message().to_string()),
            })?;
        reply.into_inner().map_err(ClusterError::from)
    }

    /// `addr`의 노드에서 도는 VM의 콘솔(VNC 바이트 스트림)을 연다.
    pub async fn console(&self, addr: &str, vm: Uuid) -> Result<ConsoleStream, ConsoleError> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut req = Request::new(ReceiverStream::new(rx));
        let value = MetadataValue::try_from(vm.to_string())
            .map_err(|e| ConsoleError::Unavailable(e.to_string()))?;
        req.metadata_mut().insert(VM_METADATA, value);
        let unavailable = |s: Status| match s.code() {
            Code::FailedPrecondition => ConsoleError::NotRunning,
            _ => ConsoleError::Unavailable(s.message().to_string()),
        };
        let inbound = self
            .client(addr)
            .map_err(unavailable)?
            .console(req)
            .await
            .map_err(unavailable)?
            .into_inner();
        Ok(ConsoleStream {
            sender: ConsoleSender { tx },
            receiver: ConsoleReceiver { inbound },
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    #[error("the VM is not running on that node")]
    NotRunning,
    #[error("cannot open the console: {0}")]
    Unavailable(String),
}

/// 다른 노드의 VM 콘솔 연결
pub struct ConsoleStream {
    sender: ConsoleSender,
    receiver: ConsoleReceiver,
}

impl ConsoleStream {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.sender.send(data).await
    }

    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.receiver.recv().await
    }

    /// 보내는 쪽과 받는 쪽으로 나눈다(두 방향을 따로 돌릴 때).
    pub fn into_split(self) -> (ConsoleSender, ConsoleReceiver) {
        (self.sender, self.receiver)
    }
}

/// 콘솔 연결의 보내는 쪽
pub struct ConsoleSender {
    tx: tokio::sync::mpsc::Sender<Bytes>,
}

impl ConsoleSender {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(data).await.is_ok()
    }
}

/// 콘솔 연결의 받는 쪽
pub struct ConsoleReceiver {
    inbound: Streaming<Bytes>,
}

impl ConsoleReceiver {
    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.inbound.message().await.ok().flatten()
    }
}

#[async_trait]
impl Forwarder for Peers {
    async fn forward(
        &self,
        _leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request(addr, req).await
    }
}

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `BasicNode.addr`는 노드의 클러스터 주소다.
#[derive(Clone)]
pub struct GrpcNetwork {
    peers: Arc<Peers>,
}

impl GrpcNetwork {
    pub fn new(peers: Arc<Peers>) -> Self {
        Self { peers }
    }
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetwork {
    type Network = GrpcConnection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr.clone(),
            peers: self.peers.clone(),
        }
    }
}

pub struct GrpcConnection {
    target: NodeId,
    addr: String,
    peers: Arc<Peers>,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, BasicNode, RaftError<NodeId, E>> {
    if matches!(
        s.code(),
        Code::Unavailable | Code::PermissionDenied | Code::Unauthenticated
    ) {
        RPCError::Unreachable(Unreachable::new(&s))
    } else {
        RPCError::Network(NetworkError::new(&s))
    }
}

impl GrpcConnection {
    fn request<T>(&self, message: T, option: &RPCOption) -> Request<T> {
        let mut req = Request::new(message);
        req.set_timeout(option.hard_ttl());
        req
    }
}

impl RaftNetwork<TypeConfig> for GrpcConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .append_entries(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .install_snapshot(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .vote(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
