//! 노드 간 통신과 클러스터 PKI.
//!
//! 노드는 gRPC(HTTP/2) over mTLS로 이야기한다. 클러스터 CA가 서명한 인증서를 가진 노드만 연결할 수
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, 관찰 노드의 Follow, VM 콘솔 중계, 라이브 마이그레이션
//! 받기, 로컬 디스크 복제 받기, UEFI 변수·TPM 상태 파일 보내기와 펌웨어 사본 받기, 게스트 에이전트, 감사 로그 읽기, 경보·알림 기록 읽기, 지표와 지표 기록 읽기·VM 기록 밀어 넘기기, 펜싱 방법으로 전원 읽기, 리더의 최적 배치 상태 읽기,
//! Ceph를 세울 노드의 디스크 목록과 Ceph 상태 읽기, 노드 토폴로지의 지금 값 읽기다.

pub mod alerts;
pub mod audit;
pub mod ceph;
mod client;
mod codec;
#[cfg(feature = "fault-it")]
pub mod fault;
pub mod fence;
pub mod file_restore;
pub mod firmware;
pub mod guest;
pub mod metrics;
pub mod migration;
pub mod monitor;
pub mod pki;
pub mod placement;
pub mod replica;
mod server;
pub mod tls;
pub mod topology;
mod wire;

pub use alerts::AlertsSource;
pub use audit::{AuditReply, AuditSource};
pub use ceph::{CephSource, CephStatusReply, CephStatusRequest, NodeDisksReply, NodeDisksRequest};
pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use fence::{FenceSource, FenceStatusReply, FenceStatusRequest};
pub use file_restore::{
    DownloadHead, DownloadPiece, DownloadStream, FileRestoreAnswer, FileRestoreCallRequest,
    FileRestoreDownloadRequest, FileRestoreFault, FileRestoreOp, FileRestoreReply,
    FileRestoreSource,
};
pub use firmware::{
    PurgeFirmwareRequest, PushFirmwareRequest, TpmStateError, UefiVarsChunk, UefiVarsError,
    VarsTemplates, read_chunks, sha256_hex,
};
pub use guest::{
    GuestAgentError, GuestAgentOp, GuestAgentReply, GuestAgentRequest, GuestAgentSource,
};
pub use metrics::{
    HistoryError, MAX_HISTORY_BYTES, MetricsReply, MetricsSource, PushMetricsHistoryReply,
    PushMetricsHistoryRequest,
};
pub use migration::{
    IncomingDisk, IncomingError, IncomingMigration, MigrateIncomingRequest,
    MigrateIncomingResponse, MigrationReceiver,
};
pub use monitor::{LEASE_LIVENESS, NodeMonitor, NodeMonitorConfig, renew_liveness};
pub use placement::{PlacementRequest, PlacementSource};
pub use replica::{
    PrepareMode, PrepareVolume, ReplicaAbortRequest, ReplicaError, ReplicaFinishRequest,
    ReplicaPrepareRequest, ReplicaPrepareResponse, ReplicaReceiver,
};
pub use server::{PeerService, ServeError, serve};
pub use topology::{TopologyRequest, TopologySource};

/// 생성된 gRPC 클라이언트와 서버. 생성 코드가 단위 구조체 코덱을 `default()`로 만든다.
#[allow(clippy::default_constructed_unit_structs)]
mod proto {
    tonic::include_proto!("opl.peer.v1.Peer");
}

/// 콘솔 스트림에서 VM id를 담는 메타데이터 키
pub const VM_METADATA: &str = "opl-vm";

/// `UefiVars`에서 요청한 노드가 아는 VM의 펌웨어 회차를 담는 메타데이터 키. 주는 노드는 자기 클러스터 상태가 아직
/// 초기화를 받지 못했어도 이 회차보다 낡은 변수를 주지 않는다. 옛 노드는 키를 모르고 무시한다.
pub const FIRMWARE_EPOCH_METADATA: &str = "opl-firmware-epoch";

/// 호출이 가려는 노드의 id를 담는 메타데이터 키. 받는 노드는 이것이 자기 id가 아니면(다른 노드를 가리키는 링크)
/// 처리하지 않고 `permission_denied`로 답하며 같은 키에 자기 id를 싣는다. 보내는 쪽은 그것을 링크 실패로 센다.
pub const TARGET_METADATA: &str = "orbivisor-target";

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// HTTP/2 keepalive ping 간격(TCP keepalive도 같은 값). 반쯤 끊긴 연결을 알아채려고 쓴다.
pub(crate) const KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// keepalive ping 응답을 기다리는 최대 시간. 넘으면 연결을 끊는다.
pub(crate) const KEEPALIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
