//! 백업 저장소(계획 68 설계 §7·§9·§10). 중복 제거 청크 저장소의 형식(청크 객체·고정 인덱스·매니페스트 v3),
//! 클라이언트 쪽 암호화(키 계층·복구 키·escrow), 백엔드(백업 풀 안 디렉터리·S3 호환), GC 표시·쓸기와 검증 엔진.
//! 백업을 읽고 내보내는 NBD 클라이언트·서버와 `backup-serve` 헬퍼의 몸통(§8·§11.1)도 여기 있다.
//! Raft와 Task는 모른다 — 그쪽은 orbivisor-api가 이 crate의 걸음을 차례로 부른다.

pub mod backend;
pub mod bloom;
pub mod crypto;
#[cfg(any(test, feature = "fake-s3"))]
pub mod fake_s3;
pub mod format;
pub mod gc;
pub mod nbd;
pub mod store;
#[cfg(any(test, feature = "test-util"))]
pub mod testing;
pub mod verify;

pub use backend::{Backend, StoreError};
pub use store::Store;
#[cfg(test)]
mod engine_tests;
