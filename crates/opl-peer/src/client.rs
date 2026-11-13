//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 관찰 노드의 Follow, 콘솔,
//! 마이그레이션 받기, UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::types::{LeaderRequest, NodeId, OrbNode, TypeConfig};
use opl_cluster::{
    ClusterError, Failure, FollowClient, FollowFrame, Forwarder, LinkBook, LinkSelector, Pick,
};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::metrics::MetricsQuery;
use tokio::runtime::Handle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Status, Streaming};
use uuid::Uuid;

use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::metrics::MetricsReply;
use crate::migration::{
    IncomingError, IncomingMigration, MigrateIncomingRequest, MigrateIncomingResponse,
};
use crate::pki::NodeTls;
use crate::proto::peer_client::PeerClient;
use crate::wire::FollowRequest;
use crate::{
    KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, TARGET_METADATA, VM_METADATA,
};

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 리더 요청 하나를 기다리는 최대 시간. 리더가 AddVoter에서 새 노드를 따라잡게 기다리는 시간
/// (`opl_cluster::CATCH_UP_TIMEOUT`, 60초)에 쓰기 시간을 더한 것보다 길어야 한다.
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// 리스 갱신(`RenewLease`·`RenewVip`) 하나를 기다리는 최대 시간. 갱신하는 쪽이 future를 버리는 시간(HA 리스는
/// `fence_after`가 20초 이상일 때 4초)보다 짧으면 끊긴 링크를 이 제한 시간 초과로 보고, 바깥이 더 짧으면(짧은
/// `fence_after`) 버려진 호출을 가드([`Attempt`])가 초과로 센다.
const LEASE_RPC_TIMEOUT: Duration = Duration::from_secs(3);

/// Follow를 열고 응답 헤더를 받기까지 기다리는 최대 시간
const FOLLOW_OPEN_TIMEOUT: Duration = Duration::from_secs(3);

/// 받을 준비(`MigrateIncoming`)를 기다리는 최대 시간. 대상 노드가 VM 상태를 따라잡고(최대 10초),
/// 디스크를 켜고, QEMU를 띄우는 시간이다.
const MIGRATE_INCOMING_TIMEOUT: Duration = Duration::from_secs(120);

/// 받기 확인(`MigrateIncomingWait`)을 기다리는 최대 시간. 대상 노드의 드라이버가 받기를 최대 120초
/// 기다리므로(`incoming_wait`) 그보다 길어야 그 결과를 받는다.
const MIGRATE_INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

/// 받기 취소(`MigrateIncomingAbort`)를 기다리는 최대 시간. 받는 QEMU를 강제로 끄고 디스크를 끈다.
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);

/// UEFI 변수 파일 받기(`UefiVars`)를 기다리는 최대 시간. AAVMF 변수 파일은 64 MiB지만 대부분 0이라
/// 길이만 온다.
const UEFI_VARS_TIMEOUT: Duration = Duration::from_secs(60);

/// 다른 노드로 가는 연결. 주소마다 채널 하나를 만들어 다시 쓴다(끊기면 tonic이 다시 잇는다).
/// 연결할 때 이 노드의 인증서를 내밀고, 상대 인증서는 클러스터 CA로 확인한다.
///
/// 노드는 첫 링크(클러스터 주소)로 가리킨다. 링크가 둘 이상인 노드(`set_link_book`의 책, 책을 붙이기 전의 Raft는
/// 멤버십의 `OrbNode`)는 호출마다 [`LinkSelector`]로 링크를 고르고 그 링크의 채널로 보내며, 결과로 넘어가기를
/// 정한다(설계 §7). 요청이 나가지 않은 호출(연결하지 못함)은 다시 고른 다른 링크로 한 번 더 보낸다. 우선 링크로
/// 보내 보는 시험 호출은
/// control 채널의 짧은 호출만이다. 채널은 링크마다 따로다.
///
/// 실행기를 주면(`with_runtimes`) 주소마다 채널을 둘 둔다. Raft·리더 전달(리스 갱신과 쓰기 전달)·ping·Follow는
/// control 채널, 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk 채널이고, 채널의 연결
/// 작업은 부른 쪽 실행기와 관계없이 그 채널의 실행기에서 돈다. 주지 않으면 채널 하나를 처음 부른
/// 쪽 실행기에서 돌린다.
pub struct Peers {
    tls: ClientTlsConfig,
    channels: Mutex<HashMap<(Lane, String), Channel>>,
    /// control·bulk 채널의 연결 작업을 돌릴 실행기
    runtimes: Option<(Handle, Handle)>,
    /// 노드마다 지금 쓰는 링크
    links: LinkSelector,
    /// 노드마다의 링크 목록. 붙이기 전에는 모든 노드가 링크 하나다(Raft는 멤버십의 링크를 쓴다).
    book: OnceLock<Arc<LinkBook>>,
}

/// 호출 결과 가운데 링크 실패로 셀 것
#[derive(Clone, Copy, PartialEq, Eq)]
enum Counts {
    /// 연결 실패와 제한 시간 초과(짧은 호출)
    All,
    /// 연결 실패만(오래 걸릴 수 있는 호출)
    ConnectOnly,
}

/// 호출 하나가 고른 링크. 결과를 링크 선택에 알린다. 알리지 않고 버려지면(부른 쪽의 제한 시간) 제한 시간 초과로
/// 센다. openraft가 다른 까닭(복제 스트림을 다시 만듦)으로 버린 호출도 초과로 세지만, 넘어가기는 연속 두 번이나
/// 마지막 성공 뒤 400 ms(`LINK_DEAD_AFTER`)를 요구하므로 살아 있는 링크는 대개 버리지 않는다.
struct Attempt<'a> {
    links: &'a LinkSelector,
    addr: String,
    pick: Pick,
    counts: Counts,
    done: bool,
}

impl Attempt<'_> {
    /// 결과를 링크 선택에 알린다.
    fn finish<T>(mut self, result: &Result<T, Status>) {
        self.done = true;
        let failure = match result {
            Ok(_) => None,
            Err(s) => match s.code() {
                // 서버가 내려가는 중이라 답한 것("shutting down"), 이미 보낸 호출이 keepalive 만료로 끝난 것도 여기에
                // 든다. 곧 닿지 않을 노드·연결이다(다시 보낼지는 [`not_sent`]가 따로 본다).
                Code::Unavailable => Some(Failure::Connect),
                // 링크가 다른 노드를 가리킨다(그 노드는 처리하지 않았다).
                Code::PermissionDenied if s.metadata().contains_key(TARGET_METADATA) => {
                    Some(Failure::Connect)
                }
                // tonic은 자기 제한 시간을 `Cancelled`로 돌려준다.
                Code::DeadlineExceeded | Code::Cancelled => Some(Failure::Timeout),
                // 요청 중에 끊긴 연결이거나 처리 중의 오류다. 어느 쪽인지 몰라 세지 않는다.
                Code::Unknown | Code::Internal => return,
                // 상대가 답했다(권한·요청 오류).
                _ => None,
            },
        };
        match failure {
            None => self.links.ok(&self.addr, &self.pick.link, self.pick.sent),
            Some(Failure::Timeout) if self.counts == Counts::ConnectOnly => {}
            Some(failure) => self.links.failed(&self.addr, &self.pick.link, failure),
        }
    }
}

/// 요청이 상대에게 나가지 않았음이 확실한 실패: 연결하지 못함(TCP·TLS, 오류 원인 사슬의 `tonic::ConnectError`)이거나
/// 다른 노드가 처리하지 않고 거절함(대상 id). `Unavailable`이라도 이미 보낸 호출(keepalive ping 만료, 서버의
/// "shutting down")은 상대가 받았을 수 있다. `tonic::transport::Error`는 보낸 뒤의 오류도 감싸므로 보지 않는다.
fn not_sent(s: &Status) -> bool {
    if s.code() == Code::PermissionDenied && s.metadata().contains_key(TARGET_METADATA) {
        return true;
    }
    let mut source = std::error::Error::source(s);
    while let Some(e) = source {
        if e.is::<tonic::ConnectError>() {
            return true;
        }
        source = e.source();
    }
    false
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.done && self.counts == Counts::All {
            self.links
                .failed(&self.addr, &self.pick.link, Failure::Timeout);
        }
    }
}

/// 채널의 쓰임
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Control,
    Bulk,
}

/// tonic 채널의 작업(요청 버퍼, h2 연결)을 정한 실행기에 띄운다.
#[derive(Clone)]
struct OnRuntime(Handle);

impl<F> hyper::rt::Executor<F> for OnRuntime
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        self.0.spawn(fut);
    }
}

impl Peers {
    pub fn new(tls: &NodeTls) -> Self {
        Self {
            tls: ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca_pem))
                .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem)),
            channels: Mutex::new(HashMap::new()),
            runtimes: None,
            links: LinkSelector::default(),
            book: OnceLock::new(),
        }
    }

    /// 노드의 링크 목록을 이 책에서 찾는다(처음 한 번만 붙는다). 클러스터 저장소를 연 뒤 붙인다.
    pub fn set_link_book(&self, book: Arc<LinkBook>) {
        let _ = self.book.set(book);
    }

    /// 우선 링크를 `retry`마다 다시 시도한다(기본 30초). 시험용이다.
    pub fn with_link_retry(mut self, retry: Duration) -> Self {
        self.links = LinkSelector::new(retry);
        self
    }

    /// 첫 링크가 `addr`인 노드에 지금 쓰는 링크(`host:port`)
    pub fn active_link(&self, addr: &str) -> String {
        self.links.active(addr).unwrap_or_else(|| addr.to_string())
    }

    /// 첫 링크가 `addr`인 노드의 링크 목록. 책을 붙이기 전에는 `fallback`이다.
    fn node_links(&self, addr: &str, fallback: impl FnOnce() -> Vec<String>) -> Vec<String> {
        match self.book.get() {
            Some(book) => book.links(addr),
            None => fallback(),
        }
    }

    /// control 채널의 연결 작업은 `control`, bulk 채널의 것은 `bulk` 실행기에서 돌린다.
    pub fn with_runtimes(mut self, control: Handle, bulk: Handle) -> Self {
        self.runtimes = Some((control, bulk));
        self
    }

    /// 호출 하나의 링크. 우선 링크로 보내 보는 시험 호출은 control 채널의 짧은 호출만 한다(큰·오래 걸리는 호출은
    /// 늘 지금 링크).
    fn pick(&self, lane: Lane, addr: &str, links: &[String], counts: Counts) -> Pick {
        if lane == Lane::Control && counts == Counts::All {
            self.links.pick(addr, links)
        } else {
            self.links.current(addr, links)
        }
    }

    /// 고른 링크의 클라이언트와 결과를 알릴 것. 링크로 채널을 만들지 못하면(잘못된 주소) 연결 실패로 센다(시험
    /// 호출이면 시험을 끝낸다).
    fn attempt(
        &self,
        lane: Lane,
        addr: &str,
        pick: Pick,
        counts: Counts,
    ) -> Result<(PeerClient<Channel>, Attempt<'_>), Status> {
        match self.channel(lane, &pick.link) {
            Ok(client) => Ok((
                client,
                Attempt {
                    links: &self.links,
                    addr: addr.to_string(),
                    pick,
                    counts,
                    done: false,
                },
            )),
            Err(e) => {
                self.links.failed(addr, &pick.link, Failure::Connect);
                Err(e)
            }
        }
    }

    /// 첫 링크가 `addr`이고 링크가 `links`인 노드로 `call`을 보낸다. 요청이 나가지 않았으면([`not_sent`], 채널을 만들지
    /// 못함) 다시 고른 다른 링크로 한 번 더 보낸다. 제한 시간 초과와 이미 보낸 호출의 다른 실패는 다시 보내지
    /// 않는다(받았을 수 있다).
    async fn send<R, F, Fut>(
        &self,
        lane: Lane,
        addr: &str,
        links: &[String],
        counts: Counts,
        mut call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let mut pick = self.pick(lane, addr, links, counts);
        let mut again = true;
        loop {
            let link = pick.link.clone();
            let (reply, unsent) = match self.attempt(lane, addr, pick, counts) {
                Ok((client, attempt)) => {
                    let reply = call(client).await;
                    attempt.finish(&reply);
                    let unsent = reply.as_ref().err().is_some_and(not_sent);
                    (reply, unsent)
                }
                Err(e) => (Err(e), true),
            };
            if unsent && std::mem::take(&mut again) {
                let next = self.links.current(addr, links);
                if next.link != link {
                    pick = next;
                    continue;
                }
            }
            return reply;
        }
    }

    /// [`send`](Self::send)를 control 채널로, 링크는 책에서
    async fn send_control<R, F, Fut>(
        &self,
        addr: &str,
        counts: Counts,
        call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.node_links(addr, || vec![addr.to_string()]);
        self.send(Lane::Control, addr, &links, counts, call).await
    }

    /// 오래 걸릴 수 있는 bulk 호출이라 연결 실패만 센다.
    async fn send_bulk<R, F, Fut>(&self, addr: &str, call: F) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.node_links(addr, || vec![addr.to_string()]);
        self.send(Lane::Bulk, addr, &links, Counts::ConnectOnly, call)
            .await
    }

    /// 링크 `addr`로 가는 채널의 클라이언트
    fn channel(&self, lane: Lane, addr: &str) -> Result<PeerClient<Channel>, Status> {
        let (lane, executor) = match &self.runtimes {
            Some((control, bulk)) => match lane {
                Lane::Control => (lane, Some(control.clone())),
                Lane::Bulk => (lane, Some(bulk.clone())),
            },
            None => (Lane::Control, None),
        };
        let mut channels = self.channels.lock().unwrap();
        let channel = match channels.get(&(lane, addr.to_string())) {
            Some(channel) => channel.clone(),
            None => {
                let mut endpoint = Endpoint::from_shared(format!("https://{addr}"))
                    .and_then(|e| e.tls_config(self.tls.clone()))
                    .map_err(|e| Status::invalid_argument(format!("bad address {addr}: {e}")))?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
                    .keep_alive_timeout(KEEPALIVE_TIMEOUT)
                    .keep_alive_while_idle(true)
                    .tcp_keepalive(Some(KEEPALIVE_INTERVAL));
                if let Some(handle) = executor {
                    endpoint = endpoint.executor(OnRuntime(handle));
                }
                let channel = endpoint.connect_lazy();
                channels.insert((lane, addr.to_string()), channel.clone());
                channel
            }
        };
        Ok(PeerClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES))
    }

    /// 노드 `to`(첫 링크 `addr`)가 응답하면 그 노드의 id를 돌려준다. 링크가 다른 노드를 가리키면 그 노드가 거절해
    /// 다음 링크로 다시 보낸다.
    pub async fn ping(&self, to: NodeId, addr: &str, timeout: Duration) -> Result<NodeId, Status> {
        let reply = self
            .send_control(addr, Counts::All, |mut client| {
                let mut req = Request::new(());
                req.set_timeout(timeout);
                addressed(&mut req, Some(to));
                async move { within(timeout, client.ping(req)).await }
            })
            .await;
        Ok(reply?.into_inner())
    }

    /// `addr`의 노드에 리더 요청을 보낸다. 그 노드가 리더가 아니면 `NotLeader`다. 리스 갱신은 짧게 기다리고
    /// 제한 시간 초과도 링크 실패로 센다. 다른 요청(쓰기, 따라잡기를 기다리는 `AddVoter`)은 연결 실패만 센다.
    pub async fn leader_request(
        &self,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request_to(addr, None, req).await
    }

    /// [`leader_request`](Self::leader_request)와 같고, 리더의 id를 알면(`leader`) 실어 보내 다른 노드를 가리키는
    /// 링크면 다른 링크로 보낸다.
    async fn leader_request_to(
        &self,
        addr: &str,
        leader: Option<NodeId>,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let (timeout, counts) = match req {
            LeaderRequest::RenewLease { .. } | LeaderRequest::RenewVip { .. } => {
                (LEASE_RPC_TIMEOUT, Counts::All)
            }
            _ => (LEADER_REQUEST_TIMEOUT, Counts::ConnectOnly),
        };
        // 서버는 처리 결과를 늘 응답 본문에 담아 보내므로, `Status`는 전송 문제다. 새로 연결하지
        // 못하면 `Unavailable`이지만 요청 중에 끊기면 `Cancelled`, `Unknown`, `Internal` 등이 오므로
        // 권한·요청 오류가 아니면 모두 `Unreachable`로 돌려 다시 시도하게 한다.
        let reply = self
            .send_control(addr, counts, |mut client| {
                let mut req = Request::new(req.clone());
                req.set_timeout(timeout);
                addressed(&mut req, leader);
                async move { within(timeout, client.leader(req)).await }
            })
            .await;
        let reply = reply.map_err(|s| match s.code() {
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
        // 요청 스트림은 한 번만 보낼 수 있어 다른 링크로 다시 보내지 않는다.
        let links = self.node_links(addr, || vec![addr.to_string()]);
        let pick = self.pick(Lane::Bulk, addr, &links, Counts::ConnectOnly);
        let (mut client, attempt) = self
            .attempt(Lane::Bulk, addr, pick, Counts::ConnectOnly)
            .map_err(unavailable)?;
        let reply = client.console(req).await;
        attempt.finish(&reply);
        let inbound = reply.map_err(unavailable)?.into_inner();
        Ok(ConsoleStream {
            sender: ConsoleSender { tx },
            receiver: ConsoleReceiver { inbound },
        })
    }
}

impl Peers {
    /// `addr`의 노드에 VM을 받을 준비를 시킨다. 받는 QEMU가 연 포트(디스크를 복사하면 NBD 포트도)를
    /// 돌려준다.
    pub async fn migrate_incoming(
        &self,
        addr: &str,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(req.clone());
                req.set_timeout(MIGRATE_INCOMING_TIMEOUT);
                async move { client.migrate_incoming(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }

    /// `addr`의 노드에서 받는 QEMU가 받기를 마칠 때까지 기다린다.
    pub async fn migrate_incoming_wait(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(migration);
                req.set_timeout(MIGRATE_INCOMING_WAIT_TIMEOUT);
                async move { client.migrate_incoming_wait(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }

    /// `addr`의 노드에 확정 전의 마이그레이션을 버리게 한다(받는 QEMU를 끄고 디스크를 끈다).
    pub async fn migrate_incoming_abort(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(migration);
                req.set_timeout(MIGRATE_INCOMING_ABORT_TIMEOUT);
                async move { client.migrate_incoming_abort(req).await }
            })
            .await;
        reply.map_err(unreachable)?.into_inner()
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에
    /// 받아 fsync한 뒤 이름을 바꾼다). 받은 크기가 `expect_len`과 다르거나 실패하면 `dest`는 그대로다.
    /// 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(
        &self,
        from: NodeId,
        addr: &str,
        vm: Uuid,
        dest: &Path,
        expect_len: Option<u64>,
    ) -> Result<u64, UefiVarsError> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(UefiVarsRequest { vm });
                req.set_timeout(UEFI_VARS_TIMEOUT);
                addressed(&mut req, Some(from));
                async move { client.uefi_vars(req).await }
            })
            .await;
        let chunks = reply?.into_inner();
        crate::firmware::write_file(chunks, dest, expect_len).await
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)의 감사 로그에서 `query`에 맞는 항목을 읽는다(새 것부터, 최대 `limit + 1`개).
    /// 그 노드가 감사 로그를 껐으면 None이다.
    pub async fn read_audit(
        &self,
        from: NodeId,
        addr: &str,
        query: AuditQuery,
        timeout: Duration,
    ) -> Result<Option<Vec<AuditEntry>>, Status> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(query.clone());
                req.set_timeout(timeout);
                addressed(&mut req, Some(from));
                async move { client.read_audit(req).await }
            })
            .await;
        match reply {
            Ok(reply) => Ok(Some(reply.into_inner().entries)),
            Err(s) if s.code() == Code::FailedPrecondition => Ok(None),
            Err(s) => Err(s),
        }
    }
}

impl Peers {
    /// 노드 `from`(첫 링크 `addr`)의 메모리에 있는 지표를 읽는다. 그 노드가 지표를 모르면(옛 노드, 지표 소스가
    /// 없는 노드) None이다.
    pub async fn read_metrics(
        &self,
        from: NodeId,
        addr: &str,
        query: MetricsQuery,
        timeout: Duration,
    ) -> Result<Option<MetricsReply>, Status> {
        let reply = self
            .send_bulk(addr, |mut client| {
                let mut req = Request::new(query.clone());
                req.set_timeout(timeout);
                addressed(&mut req, Some(from));
                async move { client.read_metrics(req).await }
            })
            .await;
        match reply {
            Ok(reply) => Ok(Some(reply.into_inner())),
            Err(s) if s.code() == Code::Unimplemented => Ok(None),
            Err(s) => Err(s),
        }
    }
}

/// `fut`을 `timeout`까지만 기다린다. gRPC 제한 시간은 요청을 보낸 뒤부터 재고 tonic의 연결 제한 시간은 TCP
/// 연결까지라, 받고 답하지 않는 상대(TLS 핸드셰이크에서 멈춤)에 보낸 호출은 이것이 끝낸다.
async fn within<T>(
    timeout: Duration,
    fut: impl Future<Output = Result<T, Status>>,
) -> Result<T, Status> {
    tokio::time::timeout(timeout, fut)
        .await
        .unwrap_or_else(|_| Err(Status::deadline_exceeded("timed out")))
}

/// 가려는 노드의 id(`target`)를 요청에 싣는다(받는 노드가 다른 노드면 거절한다).
fn addressed<T>(req: &mut Request<T>, target: Option<NodeId>) {
    if let Some(value) = target.and_then(|t| MetadataValue::try_from(t.0.to_string()).ok()) {
        req.metadata_mut().insert(TARGET_METADATA, value);
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
fn unreachable(s: Status) -> IncomingError {
    IncomingError::Unavailable(s.message().to_string())
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

/// 관찰 노드가 투표 멤버에 거는 Follow(control 채널). 여는 데(응답 헤더까지) 3초를 넘기면 실패이고, 연 뒤에는
/// 제한 시간 없이 이어진다. 끊긴 연결은 keepalive가, 멈춘 스트림은 복제본이(진행 3초 없음) 알아챈다.
#[async_trait]
impl FollowClient for Peers {
    async fn follow(
        &self,
        to: NodeId,
        addr: &str,
        since: u64,
    ) -> Result<BoxStream<'static, Result<FollowFrame, ClusterError>>, ClusterError> {
        let unreachable = |s: Status| ClusterError::Unreachable(s.message().to_string());
        let reply = self
            .send_control(addr, Counts::All, |mut client| {
                let mut req = Request::new(FollowRequest { since });
                addressed(&mut req, Some(to));
                async move {
                    tokio::time::timeout(FOLLOW_OPEN_TIMEOUT, client.follow(req))
                        .await
                        .unwrap_or_else(|_| {
                            Err(Status::deadline_exceeded(format!(
                                "the follow stream did not open within {FOLLOW_OPEN_TIMEOUT:?}"
                            )))
                        })
                }
            })
            .await;
        let frames = reply.map_err(unreachable)?.into_inner();
        Ok(frames.map(move |f| f.map_err(unreachable)).boxed())
    }
}

#[async_trait]
impl Forwarder for Peers {
    async fn forward(
        &self,
        leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request_to(addr, Some(leader), req).await
    }
}

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `OrbNode::addr()`(첫 링크)는 노드의 클러스터 주소이고,
/// 링크는 링크 책에서, 책을 붙이기 전에는 멤버십의 `OrbNode.addrs`에서 고른다(다른 호출과 같은 목록이어야 링크
/// 선택이 처음부터 다시 세지 않는다). openraft가 제한 시간(`hard_ttl`)에 버린 RPC는 링크의 제한 시간 초과로 센다.
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

    async fn new_client(&mut self, target: NodeId, node: &OrbNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr().to_string(),
            links: node.addrs.clone(),
            peers: self.peers.clone(),
        }
    }
}

pub struct GrpcConnection {
    target: NodeId,
    addr: String,
    links: Vec<String>,
    peers: Arc<Peers>,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, OrbNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, OrbNode, RaftError<NodeId, E>> {
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
        addressed(&mut req, Some(self.target));
        req
    }

    /// 대상 노드의 링크 목록
    fn links(&self) -> Vec<String> {
        self.peers.node_links(&self.addr, || self.links.clone())
    }

    /// `rpc`를 보낸다. 연결하지 못하면 다른 링크로 다시 보내므로, 링크가 둘 이상이면 보낼 때마다 복사한다.
    async fn send<T: Clone, R, F, Fut>(
        &self,
        rpc: T,
        option: &RPCOption,
        mut call: F,
    ) -> Result<R, Status>
    where
        F: FnMut(PeerClient<Channel>, Request<T>) -> Fut,
        Fut: Future<Output = Result<R, Status>>,
    {
        let links = self.links();
        let again = links.len() > 1;
        let mut rpc = Some(rpc);
        self.peers
            .send(Lane::Control, &self.addr, &links, Counts::All, |client| {
                let message = if again { rpc.clone() } else { rpc.take() };
                let message = message.expect("a single link is tried once");
                call(client, self.request(message, option))
            })
            .await
    }
}

impl RaftNetwork<TypeConfig> for GrpcConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.append_entries(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.install_snapshot(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let reply = self
            .send(rpc, &option, |mut client, req| async move {
                client.vote(req).await
            })
            .await;
        let reply = reply.map_err(rpc_error)?.into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
