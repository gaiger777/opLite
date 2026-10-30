//! 노드 간 통신과 클러스터 PKI.
//!
//! 노드는 gRPC(HTTP/2) over mTLS로 이야기한다. 클러스터 CA가 서명한 인증서를 가진 노드만 연결할 수
//! 있고, 서버는 상대 인증서의 노드 id가 지금 멤버인지도 확인한다. 메시지는 Rust 타입을 serde JSON으로
//! 보낸다(`build.rs`). 서비스는 Raft RPC, 리더 전달, 생존 확인, VM 콘솔 중계, 라이브 마이그레이션 받기다.

mod client;
mod codec;
pub mod migration;
pub mod monitor;
pub mod pki;
mod server;
pub mod tls;
mod wire;

pub use client::{ConsoleError, ConsoleReceiver, ConsoleSender, ConsoleStream, GrpcNetwork, Peers};
pub use migration::{
    IncomingError, IncomingMigration, MigrateIncomingRequest, MigrateIncomingResponse,
    MigrationReceiver,
};
pub use monitor::{NodeMonitor, NodeMonitorConfig};
pub use server::{PeerService, ServeError, serve};

/// 생성된 gRPC 클라이언트와 서버. 생성 코드가 단위 구조체 코덱을 `default()`로 만든다.
#[allow(clippy::default_constructed_unit_structs)]
mod proto {
    tonic::include_proto!("opl.peer.v1.Peer");
}

/// 콘솔 스트림에서 VM id를 담는 메타데이터 키
pub const VM_METADATA: &str = "opl-vm";

/// 한 메시지의 최대 크기. Raft 로그 묶음과 스냅샷 조각을 JSON으로 보내므로 기본값(4MiB)보다 크게 둔다.
pub const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// HTTP/2 keepalive ping 간격(TCP keepalive도 같은 값). 반쯤 끊긴 연결을 알아채려고 쓴다.
pub(crate) const KEEPALIVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// keepalive ping 응답을 기다리는 최대 시간. 넘으면 연결을 끊는다.
pub(crate) const KEEPALIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
