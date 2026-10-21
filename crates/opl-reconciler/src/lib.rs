//! 원하는 상태(Raft에 기록된 spec)와 실제 상태(하이퍼바이저)를 맞추는 조정 루프.
//!
//! 노드마다 하나씩 돈다. 이 노드에 배정된 VM마다 작업자 태스크를 하나 두어 같은 VM에 대한
//! 드라이버 호출을 직렬화하고, 서로 다른 VM은 병렬로 처리한다.

mod backoff;
mod config;

pub use config::{ReconcilerConfig, RestartBackoff};
