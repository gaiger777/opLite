//! 사용량 지표 수집. 노드와 그 노드의 VM을 5초마다 재어 메모리에 1시간 둔다.

pub mod host;
// 기록 파일 형식(계획 60 스테이지 2). 쓰는 쪽(`HistoryWriter`)은 스테이지 3이 붙이고 이 허용을 뗀다.
#[allow(dead_code)]
pub mod rrd;
pub mod sampler;
pub mod store;
