//! Raft로 복제되는 opLite 리소스 저장소.

mod log_store;
mod state_machine;
pub mod store;
pub mod types;
pub mod watch;

pub use log_store::LogStore;
pub use state_machine::StateMachine;
