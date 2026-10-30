//! gRPC로 주고받는 타입. Raft 오류도 그대로 보내 상대 노드의 openraft가 해석하게 한다.

use openraft::error::{InstallSnapshotError, RaftError};
use opl_cluster::LeaderError;
use opl_cluster::types::{NodeId, TypeConfig};

use crate::migration::{IncomingError, MigrateIncomingResponse};

pub type AppendEntriesRequest = openraft::raft::AppendEntriesRequest<TypeConfig>;
pub type AppendEntriesReply =
    Result<openraft::raft::AppendEntriesResponse<NodeId>, RaftError<NodeId>>;
pub type InstallSnapshotRequest = openraft::raft::InstallSnapshotRequest<TypeConfig>;
pub type InstallSnapshotReply = Result<
    openraft::raft::InstallSnapshotResponse<NodeId>,
    RaftError<NodeId, InstallSnapshotError>,
>;
pub type VoteRequest = openraft::raft::VoteRequest<NodeId>;
pub type VoteReply = Result<openraft::raft::VoteResponse<NodeId>, RaftError<NodeId>>;
pub type LeaderReply = Result<u64, LeaderError>;
/// 내용 없는 메시지(JSON `null`)
pub type Empty = ();
/// 받을 준비의 결과. 받는 쪽의 거절·실패도 본문에 담는다.
pub type MigrateIncomingReply = Result<MigrateIncomingResponse, IncomingError>;
/// 받기 확인·취소의 결과
pub type IncomingReply = Result<(), IncomingError>;
