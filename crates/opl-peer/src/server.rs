//! 다른 노드의 요청을 받는 gRPC 서버.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::Raft;
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{Cluster, FollowFrame, FollowGate};
use opl_driver::{DriverError, Hypervisor, tpm_state_path, uefi_vars_path};
use opl_types::alert::{AlertsQuery, AlertsReply, MAX_HISTORY_LIMIT};
use opl_types::audit::AuditQuery;
use opl_types::metrics::{HistoryQuery, HistoryReply, MetricsQuery};
use opl_types::placement::PlacementStatus;
use opl_types::vm::VmSpec;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::alerts::AlertsSource;
use crate::audit::{AuditReply, AuditSource};
use crate::ceph::{
    CephSource, CephStatusReply, CephStatusRequest, NodeDisksReply, NodeDisksRequest,
};
use crate::fence::{FenceSource, FenceStatusReply, FenceStatusRequest};
use crate::file_restore::{
    DownloadPiece, FileRestoreCallRequest, FileRestoreChunk, FileRestoreDownloadRequest,
    FileRestoreReply, FileRestoreSource,
};
use crate::firmware::{
    PurgeFirmwareReply, PurgeFirmwareRequest, PushFirmwareReply, PushFirmwareRequest,
    TpmStateRequest, UefiVarsChunk, UefiVarsRequest, VarsTemplates,
};
use crate::guest::{
    GuestAgentError, GuestAgentOp, GuestAgentReply, GuestAgentRequest, GuestAgentSource,
};
use crate::metrics::{
    MAX_HISTORY_BYTES, MetricsReply, MetricsSource, PushMetricsHistoryReply,
    PushMetricsHistoryRequest,
};
use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use crate::pki::{NodeTls, node_id_of};
use crate::placement::{PlacementRequest, PlacementSource};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::replica::{
    ReplicaAbortRequest, ReplicaFinishRequest, ReplicaPrepareRequest, ReplicaReceiver,
};
use crate::topology::{TopologyRequest, TopologySource};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, FollowRequest, IncomingReply, InstallSnapshotReply,
    InstallSnapshotRequest, LeaderReply, MigrateIncomingReply, ReplicaPrepareReply, ReplicaReply,
    VoteReply, VoteRequest,
};
use crate::{
    FIRMWARE_EPOCH_METADATA, KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES,
    TARGET_METADATA, VM_METADATA,
};
use opl_types::numa::TopologyLive;

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버(투표 멤버·learner·관찰 노드)여야
/// 한다. 아직 멤버십을 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다. 관찰 노드는
/// Raft RPC를 받지 않는다(`failed_precondition`).
pub struct PeerService {
    cluster: Arc<Cluster>,
    hv: Option<Arc<dyn Hypervisor>>,
    migrations: Option<Arc<dyn MigrationReceiver>>,
    replicas: Option<Arc<dyn ReplicaReceiver>>,
    audit: Option<Arc<dyn AuditSource>>,
    /// 이 노드의 경보와 보낸 기록(계획 60). 없으면 `unimplemented`로 거부한다(옛 노드·위트니스와 같다).
    alerts: Option<Arc<dyn AlertsSource>>,
    metrics: Option<Arc<dyn MetricsSource>>,
    fence: Option<Arc<dyn FenceSource>>,
    placement: Option<Arc<dyn PlacementSource>>,
    ceph: Option<Arc<dyn CephSource>>,
    /// 이 노드 토폴로지의 지금 값(계획 64). 없으면 `unimplemented`로 거부한다(옛 노드·macOS·위트니스와 같다).
    topology: Option<Arc<dyn TopologySource>>,
    /// 드라이버의 `state_dir`. UEFI 변수·TPM 상태 파일을 여기서 찾아 보낸다.
    state_dir: Option<PathBuf>,
    /// 펌웨어 사본(`PushFirmware`)을 받을 때 변수 파일의 크기를 견줄 템플릿. 없으면 받지 않는다.
    firmware_copies: Option<VarsTemplates>,
    /// 이 노드에서 도는 VM의 게스트 에이전트(계획 59). 없으면 `unimplemented`로 거부한다(옛 노드와 같다).
    guest: Option<Arc<dyn GuestAgentSource>>,
    /// 이 노드의 파일 복원 세션(계획 69). 없으면 `unimplemented`로 거부한다(옛 노드와 같다).
    file_restore: Option<Arc<dyn FileRestoreSource>>,
    /// Raft·리스가 아닌 처리(마이그레이션 받기, 콘솔 중계, UEFI 변수·감사 로그·지표 읽기)를 돌릴
    /// 실행기. 없으면 서버의 실행기에서 돈다.
    work: Option<Handle>,
}

impl PeerService {
    /// `hv`는 콘솔 중계에 쓴다. 없으면 콘솔 요청을 거부한다.
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self {
            cluster,
            hv,
            migrations: None,
            replicas: None,
            audit: None,
            alerts: None,
            metrics: None,
            fence: None,
            placement: None,
            ceph: None,
            topology: None,
            state_dir: None,
            firmware_copies: None,
            guest: None,
            file_restore: None,
            work: None,
        }
    }

    /// Raft·리스가 아닌 처리를 `work` 실행기에서 돌린다. 그 처리가 띄우는 작업(받은 VM의 QMP·모니터,
    /// 콘솔 중계, 파일 보내기)도 그 실행기에 생긴다.
    pub fn with_work(mut self, work: Handle) -> Self {
        self.work = Some(work);
        self
    }

    /// 드라이버의 `state_dir`에 있는 VM의 UEFI 변수 파일을 보낸다. 없으면 보내기 요청을 거부한다.
    pub fn with_uefi_vars(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(state_dir.into());
        self
    }

    /// 다른 노드가 밀어 두는 펌웨어 사본(TPM 상태 + UEFI 변수)을 `with_uefi_vars`의 `state_dir`에 받는다. 변수 파일의
    /// 크기는 `templates`와 견준다. 없으면 받기 요청을 `unimplemented`로 거부한다.
    pub fn with_firmware_copies(mut self, templates: VarsTemplates) -> Self {
        self.firmware_copies = Some(templates);
        self
    }

    /// 이 노드에서 도는 VM의 게스트 정보 읽기·비밀번호 설정을 `source`로 한다. 없으면 `unimplemented`로 거부한다.
    pub fn with_guest_agent(mut self, source: Arc<dyn GuestAgentSource>) -> Self {
        self.guest = Some(source);
        self
    }

    /// 이 노드의 파일 복원 세션을 `source`로 다룬다. 없으면 `unimplemented`로 거부한다.
    pub fn with_file_restore(mut self, source: Arc<dyn FileRestoreSource>) -> Self {
        self.file_restore = Some(source);
        self
    }

    /// 라이브 마이그레이션을 `receiver`로 받는다. 없으면 받기 요청을 거부한다.
    pub fn with_migrations(mut self, receiver: Arc<dyn MigrationReceiver>) -> Self {
        self.migrations = Some(receiver);
        self
    }

    /// 로컬 디스크 복제 회차를 `receiver`로 받는다. 없으면 받기 요청을 `unimplemented`로 거부한다(옛 노드와
    /// 같다).
    pub fn with_replicas(mut self, receiver: Arc<dyn ReplicaReceiver>) -> Self {
        self.replicas = Some(receiver);
        self
    }

    /// 이 노드의 감사 로그를 `source`로 읽어 준다. 없으면 읽기 요청을 거부한다.
    pub fn with_audit(mut self, source: Arc<dyn AuditSource>) -> Self {
        self.audit = Some(source);
        self
    }

    /// 이 노드의 경보와 보낸 기록(계획 60 `ReadAlerts`)을 `source`로 읽어 준다. 없으면 `unimplemented`로 거부한다.
    pub fn with_alerts(mut self, source: Arc<dyn AlertsSource>) -> Self {
        self.alerts = Some(source);
        self
    }

    /// 이 노드의 지표와 기록(계획 60 `ReadMetricsHistory`·`PushMetricsHistory`)을 `source`로 읽고 받는다. 없으면
    /// 요청을 `unimplemented`로 거부한다(옛 노드와 같다).
    pub fn with_metrics(mut self, source: Arc<dyn MetricsSource>) -> Self {
        self.metrics = Some(source);
        self
    }

    /// 다른 노드의 전원을 그 노드의 펜싱 방법으로 `source`가 읽어 준다. 없으면 `unimplemented`로 거부한다(옛 노드와
    /// 같다).
    pub fn with_fence(mut self, source: Arc<dyn FenceSource>) -> Self {
        self.fence = Some(source);
        self
    }

    /// 리더면 최적 배치 상태를 `source`가 만들어 준다. 없으면 `unimplemented`로 거부한다(옛 노드와 같다).
    pub fn with_placement(mut self, source: Arc<dyn PlacementSource>) -> Self {
        self.placement = Some(source);
        self
    }

    /// 이 노드의 디스크 목록과 Ceph 상태를 `source`가 읽어 준다. 없으면(위트니스) `unimplemented`로 거부한다(옛 노드와
    /// 같다).
    pub fn with_ceph(mut self, source: Arc<dyn CephSource>) -> Self {
        self.ceph = Some(source);
        self
    }

    /// 이 노드 토폴로지의 지금 값(빈 메모리·빈 hugepages·친화)을 `source`가 읽어 준다(계획 64). 없으면 `unimplemented`로
    /// 거부한다(옛 노드와 같다).
    pub fn with_topology(mut self, source: Arc<dyn TopologySource>) -> Self {
        self.topology = Some(source);
        self
    }

    fn ceph(&self) -> Result<Arc<dyn CephSource>, Status> {
        self.ceph
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not set up Ceph"))
    }

    fn metrics_source(&self) -> Result<Arc<dyn MetricsSource>, Status> {
        self.metrics
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not keep metrics"))
    }

    fn migrations(&self) -> Result<Arc<dyn MigrationReceiver>, Status> {
        self.migrations
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not receive migrations"))
    }

    fn replicas(&self) -> Result<Arc<dyn ReplicaReceiver>, Status> {
        self.replicas
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not receive replicas"))
    }

    /// `fut`을 `work` 실행기(없으면 여기)에서 돌리고 기다린다. 처리가 panic하면 다시 던지지 않고
    /// 로그를 남기고 `internal`로 답한다(같은 h2 연결의 Raft RPC를 함께 끊지 않게).
    async fn offload<F>(&self, fut: F) -> Result<F::Output, Status>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let Some(work) = &self.work else {
            return Ok(fut.await);
        };
        match work.spawn(fut).await {
            Ok(output) => Ok(output),
            Err(e) if e.is_panic() => {
                let panic = e.into_panic();
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|m| m.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(panic = %message, "a peer request handler panicked");
                Err(Status::internal("the request handler panicked"))
            }
            Err(_) => Err(Status::unavailable("this node is shutting down")),
        }
    }

    /// 이 노드의 Raft. Raft 구성원이 아니면 Raft RPC를 받지 않는다.
    fn raft(&self) -> Result<Raft<TypeConfig>, Status> {
        self.cluster
            .raft()
            .ok_or_else(|| Status::failed_precondition("this node is not a Raft member"))
    }

    /// 부른 노드의 id. 클러스터에서 빠진 노드는 `permission_denied`다. 다른 노드에 보낸 호출(대상 id
    /// [`TARGET_METADATA`]가 이 노드가 아님)도 `permission_denied`이고 이 노드의 id를 실어 답한다(옛 노드는 대상을
    /// 싣지 않으므로 받는다).
    fn authorize<T>(&self, req: &Request<T>) -> Result<NodeId, Status> {
        #[cfg(feature = "fault-it")]
        crate::fault::check_receive()?;
        let certs = req
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let id = certs
            .first()
            .and_then(|c| node_id_of(c))
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        if let Some(target) = req.metadata().get(TARGET_METADATA) {
            let me = self.cluster.id();
            if target.to_str().ok().and_then(|t| Uuid::try_parse(t).ok()) != Some(me.0) {
                let mut status = Status::permission_denied(format!(
                    "this is node {me}, not {target:?}; a cluster link points to the wrong node"
                ));
                if let Ok(value) = me.0.to_string().parse() {
                    status.metadata_mut().insert(TARGET_METADATA, value);
                }
                return Err(status);
            }
        }
        let members = self.cluster.members();
        let uninitialized = members.voters.is_empty() && members.learners.is_empty();
        if uninitialized || members.contains(NodeId(id)) {
            Ok(NodeId(id))
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
        let reply = self.raft()?.append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn install_snapshot(
        &self,
        req: Request<InstallSnapshotRequest>,
    ) -> Result<Response<InstallSnapshotReply>, Status> {
        self.authorize(&req)?;
        let reply = self.raft()?.install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn vote(&self, req: Request<VoteRequest>) -> Result<Response<VoteReply>, Status> {
        self.authorize(&req)?;
        let reply = self.raft()?.vote(req.into_inner()).await;
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

    async fn migrate_incoming(
        &self,
        req: Request<MigrateIncomingRequest>,
    ) -> Result<Response<MigrateIncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.incoming(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn migrate_incoming_wait(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.wait(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn migrate_incoming_abort(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.abort(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn replica_prepare(
        &self,
        req: Request<ReplicaPrepareRequest>,
    ) -> Result<Response<ReplicaPrepareReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.replicas()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.prepare(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn replica_finish(
        &self,
        req: Request<ReplicaFinishRequest>,
    ) -> Result<Response<ReplicaReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.replicas()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.finish(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn replica_abort(
        &self,
        req: Request<ReplicaAbortRequest>,
    ) -> Result<Response<ReplicaReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.replicas()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.abort(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn read_audit(&self, req: Request<AuditQuery>) -> Result<Response<AuditReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .audit
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not keep an audit log"))?;
        if !source.enabled() {
            return Err(Status::failed_precondition("audit log disabled"));
        }
        let mut query = req.into_inner();
        query.limit = query.limit.min(AuditQuery::MAX_LIMIT);
        let entries = self
            .offload(async move { source.read(query).await })
            .await?
            .map_err(Status::internal)?;
        Ok(Response::new(AuditReply { entries }))
    }

    async fn read_alerts(
        &self,
        req: Request<AlertsQuery>,
    ) -> Result<Response<AlertsReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .alerts
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not evaluate alerts"))?;
        let mut query = req.into_inner();
        if let Some(w) = &mut query.history {
            w.limit = w.limit.min(MAX_HISTORY_LIMIT);
        }
        let reply = self
            .offload(async move { source.read(query).await })
            .await?
            .map_err(Status::internal)?;
        Ok(Response::new(reply))
    }

    async fn read_metrics(
        &self,
        req: Request<MetricsQuery>,
    ) -> Result<Response<MetricsReply>, Status> {
        self.authorize(&req)?;
        let source = self.metrics_source()?;
        let query = req.into_inner();
        let reply = self.offload(async move { source.read(&query) }).await?;
        Ok(Response::new(reply))
    }

    async fn read_metrics_history(
        &self,
        req: Request<HistoryQuery>,
    ) -> Result<Response<HistoryReply>, Status> {
        self.authorize(&req)?;
        let source = self.metrics_source()?;
        let query = req.into_inner();
        query
            .validate()
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // 기록 파일을 읽으므로 막히는 일을 하는 스레드에서 한다.
        let reply = self
            .offload(
                async move { tokio::task::spawn_blocking(move || source.history(&query)).await },
            )
            .await?
            .map_err(|e| Status::internal(format!("metrics history read failed: {e}")))??;
        Ok(Response::new(reply))
    }

    async fn push_metrics_history(
        &self,
        req: Request<PushMetricsHistoryRequest>,
    ) -> Result<Response<PushMetricsHistoryReply>, Status> {
        self.authorize(&req)?;
        let source = self.metrics_source()?;
        let PushMetricsHistoryRequest { vm, bytes } = req.into_inner();
        if bytes.len() > MAX_HISTORY_BYTES {
            return Err(Status::invalid_argument(format!(
                "a metrics history file is at most {MAX_HISTORY_BYTES} bytes"
            )));
        }
        self.offload(async move {
            tokio::task::spawn_blocking(move || source.push_history(vm, &bytes)).await
        })
        .await?
        .map_err(|e| Status::internal(format!("metrics history merge failed: {e}")))??;
        Ok(Response::new(PushMetricsHistoryReply))
    }

    async fn fence_status(
        &self,
        req: Request<FenceStatusRequest>,
    ) -> Result<Response<FenceStatusReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .fence
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not check power fencing"))?;
        let node = req.into_inner().node;
        let methods = self
            .offload(async move { source.status(node).await })
            .await?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(FenceStatusReply { methods }))
    }

    async fn read_placement(
        &self,
        req: Request<PlacementRequest>,
    ) -> Result<Response<PlacementStatus>, Status> {
        self.authorize(&req)?;
        let source = self
            .placement
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not keep placement"))?;
        let status = self
            .offload(async move { source.status().await })
            .await?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(status))
    }

    async fn node_disks(
        &self,
        req: Request<NodeDisksRequest>,
    ) -> Result<Response<NodeDisksReply>, Status> {
        self.authorize(&req)?;
        let source = self.ceph()?;
        let disks = self
            .offload(async move { source.disks().await })
            .await?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(NodeDisksReply { disks }))
    }

    async fn ceph_status(
        &self,
        req: Request<CephStatusRequest>,
    ) -> Result<Response<CephStatusReply>, Status> {
        self.authorize(&req)?;
        let source = self.ceph()?;
        let report = self
            .offload(async move { source.status().await })
            .await?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(CephStatusReply { report }))
    }

    async fn read_topology(
        &self,
        req: Request<TopologyRequest>,
    ) -> Result<Response<TopologyLive>, Status> {
        self.authorize(&req)?;
        let source = self
            .topology
            .clone()
            .ok_or_else(|| Status::unimplemented("this node has no NUMA topology"))?;
        let live = self
            .offload(async move { source.live().await })
            .await?
            .map_err(Status::failed_precondition)?;
        Ok(Response::new(live))
    }

    type UefiVarsStream = ReceiverStream<Result<UefiVarsChunk, Status>>;

    async fn uefi_vars(
        &self,
        req: Request<UefiVarsRequest>,
    ) -> Result<Response<Self::UefiVarsStream>, Status> {
        self.authorize(&req)?;
        let dir = self
            .state_dir
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve UEFI variables"))?;
        let requested = req
            .metadata()
            .get(FIRMWARE_EPOCH_METADATA)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok());
        let vm = req.into_inner().vm;
        // 이 노드의 회차가 VM의 회차와 다르면 이 노드의 변수는 초기화 전의 것이다(오프라인 동안 초기화된 사본 노드 등):
        // 주지 않는다(리뷰 2 #3). 회차 파일을 읽지 못하면 모르는 것이라 준다. VM의 회차는 이 노드의 클러스터 상태와
        // 요청한 노드가 아는 것 중 큰 것이다(회차는 늘기만 한다): 이 노드에 초기화가 아직 복제되지 않았어도 초기화 전
        // 변수를 주지 않는다.
        let epoch = self
            .cluster
            .get::<VmSpec>(vm)
            .map_err(|e| Status::unavailable(e.to_string()))?
            .map(|v| v.status.firmware_epoch)
            .max(requested);
        if let Some(epoch) = epoch
            && opl_driver::firmware_epoch_differs(&dir, vm, epoch)
        {
            return Err(Status::failed_precondition(format!(
                "the UEFI variables of VM {vm} on this node are from before a firmware reset"
            )));
        }
        let stream = self
            .offload(async move {
                let file = match tokio::fs::File::open(uefi_vars_path(&dir, vm)).await {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(Status::not_found(format!(
                            "no UEFI variables for VM {vm} on this node"
                        )));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!(
                            "cannot open the UEFI variables of VM {vm}: {e}"
                        )));
                    }
                };
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(crate::firmware::send_file(file, tx));
                Ok(ReceiverStream::new(rx))
            })
            .await??;
        Ok(Response::new(stream))
    }

    type TpmStateStream = ReceiverStream<Result<UefiVarsChunk, Status>>;

    async fn tpm_state(
        &self,
        req: Request<TpmStateRequest>,
    ) -> Result<Response<Self::TpmStateStream>, Status> {
        self.authorize(&req)?;
        let dir = self
            .state_dir
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve TPM state"))?;
        let vm = req.into_inner().vm;
        let stream = self
            .offload(async move {
                let file = match tokio::fs::File::open(tpm_state_path(&dir, vm)).await {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(Status::not_found(format!(
                            "no TPM state for VM {vm} on this node"
                        )));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!(
                            "cannot open the TPM state of VM {vm}: {e}"
                        )));
                    }
                };
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(crate::firmware::send_file(file, tx));
                Ok(ReceiverStream::new(rx))
            })
            .await??;
        Ok(Response::new(stream))
    }

    async fn push_firmware(
        &self,
        req: Request<PushFirmwareRequest>,
    ) -> Result<Response<PushFirmwareReply>, Status> {
        self.authorize(&req)?;
        let (Some(dir), Some(templates)) = (self.state_dir.clone(), self.firmware_copies.clone())
        else {
            return Err(Status::unimplemented(
                "this node does not keep firmware copies",
            ));
        };
        let req = req.into_inner();
        let vm = req.vm;
        // 이 노드가 지금 것을 가졌으면(VM이 여기서 돌거나 `firmware_node`·`tpm_node`) 낡을 수 있는 사본으로 덮지 않는다.
        let me = self.cluster.id().0;
        let holds = self
            .cluster
            .get::<VmSpec>(vm)
            .map_err(|e| Status::unavailable(e.to_string()))?
            .is_some_and(|v| v.status.firmware_node == Some(me) || v.status.tpm_node == Some(me));
        if holds {
            return Err(Status::failed_precondition(format!(
                "this node holds the current firmware files of VM {vm}"
            )));
        }
        let hv = self.hv.clone();
        self.offload(async move {
            if let Some(hv) = hv
                && hv
                    .inspect(vm)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?
                    .is_some()
            {
                return Err(Status::failed_precondition(format!(
                    "VM {vm} is running on this node"
                )));
            }
            crate::firmware::receive_push(&dir, &templates, req).await
        })
        .await??;
        Ok(Response::new(PushFirmwareReply))
    }

    async fn purge_firmware(
        &self,
        req: Request<PurgeFirmwareRequest>,
    ) -> Result<Response<PurgeFirmwareReply>, Status> {
        self.authorize(&req)?;
        let Some(dir) = self.state_dir.clone() else {
            return Err(Status::unimplemented("this node keeps no firmware files"));
        };
        let req = req.into_inner();
        let hv = self.hv.clone();
        self.offload(async move {
            // 이 노드에서 도는 VM의 파일은 지금 것이다.
            if req.epoch.is_some()
                && let Some(hv) = hv
                && hv
                    .inspect(req.vm)
                    .await
                    .map_err(|e| Status::internal(e.to_string()))?
                    .is_some()
            {
                return Err(Status::failed_precondition(format!(
                    "VM {} is running on this node",
                    req.vm
                )));
            }
            crate::firmware::purge(&dir, &req)
                .map_err(|e| Status::internal(format!("cannot remove firmware files: {e}")))
        })
        .await??;
        Ok(Response::new(PurgeFirmwareReply {}))
    }

    async fn guest_agent(
        &self,
        req: Request<GuestAgentRequest>,
    ) -> Result<Response<GuestAgentReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .guest
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve the guest agent"))?;
        let GuestAgentRequest { vm, op } = req.into_inner();
        let reply = self
            .offload(async move {
                match op {
                    GuestAgentOp::Info { refresh } => GuestAgentReply {
                        info: Some(source.info(vm, refresh).await),
                        error: None,
                        file: None,
                    },
                    GuestAgentOp::SetPassword { user, password } => {
                        let error =
                            match opl_types::guest::validate_guest_password(&user, &password) {
                                Err(e) => Some(GuestAgentError::Invalid(e.to_string())),
                                Ok(()) => source.set_password(vm, &user, &password).await.err(),
                            };
                        GuestAgentReply {
                            info: None,
                            error,
                            file: None,
                        }
                    }
                    GuestAgentOp::File { file } => match source.file(vm, file).await {
                        Ok(answer) => GuestAgentReply {
                            info: None,
                            error: None,
                            file: Some(answer),
                        },
                        Err(e) => GuestAgentReply {
                            info: None,
                            error: Some(e),
                            file: None,
                        },
                    },
                }
            })
            .await?;
        Ok(Response::new(reply))
    }

    async fn file_restore_call(
        &self,
        req: Request<FileRestoreCallRequest>,
    ) -> Result<Response<FileRestoreReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .file_restore
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve file restore"))?;
        let req = req.into_inner();
        let reply = self.offload(async move { source.call(req).await }).await?;
        Ok(Response::new(reply))
    }

    type FileRestoreDownloadStream = BoxStream<'static, Result<FileRestoreChunk, Status>>;

    /// 내려받기: 머리(또는 거절) 하나 뒤 조각들. 받는 쪽이 흐름을 버리면 조각 흐름도 버려져 세션 노드의 만들기가 멈춘다.
    async fn file_restore_download(
        &self,
        req: Request<FileRestoreDownloadRequest>,
    ) -> Result<Response<Self::FileRestoreDownloadStream>, Status> {
        self.authorize(&req)?;
        let source = self
            .file_restore
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve file restore"))?;
        let req = req.into_inner();
        let started = self
            .offload(async move { source.download(req).await })
            .await?;
        let stream = match started {
            Err(f) => futures_util::stream::iter([Ok(FileRestoreChunk::Fault(f))]).boxed(),
            Ok((head, rx)) => futures_util::stream::iter([Ok(FileRestoreChunk::Head(head))])
                .chain(ReceiverStream::new(rx).map(|piece| {
                    Ok(match piece {
                        Ok(DownloadPiece::Data(d)) => FileRestoreChunk::Data(d),
                        Ok(DownloadPiece::Done { entries }) => FileRestoreChunk::Done { entries },
                        Err(m) => FileRestoreChunk::Failed(m),
                    })
                }))
                .boxed(),
        };
        Ok(Response::new(stream))
    }

    type FollowStream = BoxStream<'static, Result<FollowFrame, Status>>;

    /// 관찰 노드에 이 노드의 상태를 스트림으로 보낸다. 리스 갱신처럼 Raft 쪽 일이라 `work`로 넘기지 않는다.
    async fn follow(
        &self,
        req: Request<FollowRequest>,
    ) -> Result<Response<Self::FollowStream>, Status> {
        let caller = self.authorize(&req)?;
        let frames = self
            .cluster
            .follow(caller, req.into_inner().since)
            .map_err(|e| Status::internal(e.to_string()))?;
        // 열린 스트림도 프레임마다 다시 허가한다: 지운 노드(멤버십과 관찰 노드 목록 모두에서 빠짐)는 그 뒤의 쓰기를
        // 받지 못하고 스트림이 끝난다(목록에서 빼는 프레임부터, `FollowGate`).
        let cluster = Arc::downgrade(&self.cluster);
        let mut gate = FollowGate::new(caller);
        let frames = frames.take_while(move |frame| {
            let admitted = cluster.upgrade().is_some_and(|c| gate.admits(&c, frame));
            if !admitted {
                tracing::info!(%caller, "closing the follow stream of a node removed from the cluster");
            }
            std::future::ready(admitted)
        });
        Ok(Response::new(frames.map(Ok).boxed()))
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
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve consoles"))?;
        let inbound = req.into_inner();
        // VNC 소켓은 연 실행기에 묶이므로 연결과 중계를 함께 work에서 한다.
        let stream = self
            .offload(async move {
                let not_running =
                    || Status::failed_precondition(format!("VM {vm} is not running here"));
                let path = match hv.console_endpoint(vm).await {
                    Ok(path) => path,
                    Err(DriverError::NotRunning(_)) => return Err(not_running()),
                    Err(e) => return Err(Status::internal(e.to_string())),
                };
                let vnc = UnixStream::connect(&path)
                    .await
                    .map_err(|_| not_running())?;
                let (tx, rx) = mpsc::channel(16);
                tokio::spawn(bridge(inbound, vnc, tx));
                Ok(ReceiverStream::new(rx))
            })
            .await??;
        Ok(Response::new(stream))
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
