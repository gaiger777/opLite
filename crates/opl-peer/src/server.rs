//! 다른 노드의 요청을 받는 gRPC 서버.

use std::future::Future;
use std::sync::Arc;

use bytes::Bytes;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::pki::{NodeTls, node_id_of};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, InstallSnapshotReply, InstallSnapshotRequest,
    LeaderReply, VoteReply, VoteRequest,
};
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버여야 한다. 아직 멤버십을
/// 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다.
pub struct PeerService {
    cluster: Arc<Cluster>,
    hv: Option<Arc<dyn Hypervisor>>,
}

impl PeerService {
    /// `hv`는 콘솔 중계에 쓴다. 없으면 콘솔 요청을 거부한다.
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self { cluster, hv }
    }

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
        let certs = req
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let id = certs
            .first()
            .and_then(|c| node_id_of(c))
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        let members = self.cluster.members();
        let uninitialized = members.voters.is_empty() && members.learners.is_empty();
        if uninitialized || members.contains(NodeId(id)) {
            Ok(())
        } else {
            Err(Status::permission_denied(format!(
                "node {id} is not a member of this cluster"
            )))
        }
    }
}

#[tonic::async_trait]
impl Peer for PeerService {
    async fn append_entries(
        &self,
        req: Request<AppendEntriesRequest>,
    ) -> Result<Response<AppendEntriesReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn install_snapshot(
        &self,
        req: Request<InstallSnapshotRequest>,
    ) -> Result<Response<InstallSnapshotReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn vote(&self, req: Request<VoteRequest>) -> Result<Response<VoteReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().vote(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn leader(&self, req: Request<LeaderRequest>) -> Result<Response<LeaderReply>, Status> {
        self.authorize(&req)?;
        let reply = self
            .cluster
            .execute_as_leader(req.into_inner())
            .await
            .map_err(Into::into);
        Ok(Response::new(reply))
    }

    async fn ping(&self, req: Request<()>) -> Result<Response<NodeId>, Status> {
        self.authorize(&req)?;
        Ok(Response::new(self.cluster.id()))
    }

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;

    async fn console(
        &self,
        req: Request<Streaming<Bytes>>,
    ) -> Result<Response<Self::ConsoleStream>, Status> {
        self.authorize(&req)?;
        let vm = req
            .metadata()
            .get(VM_METADATA)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::try_parse(s).ok())
            .ok_or_else(|| Status::invalid_argument("missing VM id"))?;
        let hv = self
            .hv
            .as_ref()
            .ok_or_else(|| Status::unimplemented("this node does not serve consoles"))?;
        let not_running = || Status::failed_precondition(format!("VM {vm} is not running here"));
        let path = match hv.console_endpoint(vm).await {
            Ok(path) => path,
            Err(DriverError::NotRunning(_)) => return Err(not_running()),
            Err(e) => return Err(Status::internal(e.to_string())),
        };
        let vnc = UnixStream::connect(&path)
            .await
            .map_err(|_| not_running())?;
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(bridge(req.into_inner(), vnc, tx));
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// 요청 스트림을 VNC 소켓으로, VNC 소켓을 응답 스트림으로 잇는다. 어느 쪽이든 끝나면 모두 닫는다.
/// 두 방향은 따로 돈다. 한쪽 쓰기가 막혀도 다른 쪽은 계속 흐른다.
async fn bridge(
    mut inbound: Streaming<Bytes>,
    vnc: UnixStream,
    tx: mpsc::Sender<Result<Bytes, Status>>,
) {
    let (mut vnc_read, mut vnc_write) = vnc.into_split();
    let to_vnc = async {
        while let Ok(Some(data)) = inbound.message().await {
            if vnc_write.write_all(&data).await.is_err() {
                break;
            }
        }
    };
    let from_vnc = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match vnc_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx
                        .send(Ok(Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    tokio::select! {
        _ = to_vnc => {}
        _ = from_vnc => {}
    }
}

/// 노드 간 서버가 멈춘 이유
pub type ServeError = tonic::transport::Error;

/// `listener`에서 노드 간 gRPC를 받는다. 클라이언트 인증서가 클러스터 CA로 서명되어 있어야 한다.
/// `shutdown`이 끝나면 멈춘다.
pub async fn serve(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem))
        .client_ca_root(Certificate::from_pem(&tls.ca_pem));
    let service = PeerServer::new(service)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    Server::builder()
        .http2_keepalive_interval(Some(KEEPALIVE_INTERVAL))
        .http2_keepalive_timeout(Some(KEEPALIVE_TIMEOUT))
        .tcp_keepalive(Some(KEEPALIVE_INTERVAL))
        .tls_config(tls)?
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
}
