//! Raft로 복제되는 Orbivisor 리소스 저장소.

mod cluster;
mod exclusive;
mod follow;
mod health;
mod leader;
mod lease;
pub mod links;
mod log_store;
pub mod memnet;
mod replica;
mod roles;
mod state_machine;
pub mod store;
pub mod types;
mod vip;
mod voters;
pub mod watch;
mod writer;

pub use cluster::{Cluster, ClusterConfig, ClusterError, DB_FILE, Members, Typed};
pub use follow::{FollowClient, FollowFrame, FollowGate, NodeRole, PROGRESS_INTERVAL};
pub use health::{QUORUM_ACK_MAX, RaftHealth};
pub use leader::{CATCH_UP_TIMEOUT, Forwarder, LeaderError};
pub use lease::Leases;
pub use links::{Failure, LINK_DEAD_AFTER, LINK_RETRY, LinkBook, LinkSelector, Pick};
pub use log_store::LogStore;
pub use roles::{RoleKeeper, RoleKeeperConfig};
pub use state_machine::StateMachine;
pub use types::OrbNode;
pub use vip::{VipLeases, VipRenewal};
pub use voters::{
    OBSERVER, STANDBY_LEARNERS, VOTER_LIMIT, VoterKeeper, VoterKeeperConfig, ZONE_LABEL,
    leader_manages_voters,
};
