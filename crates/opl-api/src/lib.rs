//! opLite REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod auth;
pub mod error;
pub mod scheduler;
mod state;
mod tasks;

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
pub use tasks::{TaskRunner, TaskRunnerConfig};
