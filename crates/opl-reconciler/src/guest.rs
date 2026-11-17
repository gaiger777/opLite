//! 게스트 정보 캐시로 보내기(계획 59 §8). 작업자가 도는 VM의 게스트 에이전트에서 모은 정보를 opld의 노드 메모리
//! 캐시(`GuestInfoStore`)에 둔다. Raft에는 쓰지 않는다.

use opl_types::guest::GuestInfo;
use uuid::Uuid;

/// 작업자가 모은 게스트 정보를 받는 곳
pub trait GuestInfoSink: Send + Sync {
    /// 이 노드에서 도는 VM `vm`의 에이전트가 답한 정보. 같은 VM의 앞 정보를 바꾼다.
    fn put(&self, vm: Uuid, info: GuestInfo);
    /// VM이 꺼졌거나 이 노드를 떠났거나 새 QEMU로 다시 켰다: 그 정보를 지운다.
    fn remove(&self, vm: Uuid);
}
