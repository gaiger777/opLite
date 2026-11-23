//! 파일 단위 복원(계획 69 설계 §9·§11). 도우미 VM 안의 에이전트(`orbivisor-restore-agent`, PID 1)와 opld 사이의
//! 통신 규약([`proto`]), 에이전트의 요청 처리 몸통([`agent`] — 볼륨 찾기·마운트·나열·읽기·걷기), 여러 항목을 흘려
//! 내려받는 zip·tar.gz 작성기([`archive`]).
//!
//! 이 crate는 도우미 이미지의 정적 musl 바이너리로도 빌드되므로 C를 쓰는 의존성을 들이지 않는다(opl-types도 쓰지
//! 않는다 — ring). opld는 같은 crate로 에이전트의 답을 검사하고([`proto::StreamCheck`]) 아카이브를 만든다.

pub mod agent;
pub mod archive;
pub mod proto;
