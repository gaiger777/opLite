//! 시험 고리(`fault-it` 기능, 시험 빌드에만): 이 프로세스의 노드 간 통신을 모두 끊어 망에서 떼어 낸 노드처럼
//! 만든다. 끊긴 동안 [`Peers`](crate::Peers)의 모든 보내기와 [`PeerService`](crate::PeerService)의 모든 받기가
//! `unavailable`이다. 프로세스 전역 값이라 한 프로세스에 노드 하나인 시험(opld `ha-it`)에서 쓴다.

use std::sync::atomic::{AtomicBool, Ordering};

use tonic::Status;

static ISOLATED: AtomicBool = AtomicBool::new(false);

/// 노드 간 통신을 끊는다.
pub fn isolate() {
    ISOLATED.store(true, Ordering::SeqCst);
}

/// 끊은 통신을 다시 잇는다.
pub fn heal() {
    ISOLATED.store(false, Ordering::SeqCst);
}

/// 지금 끊겨 있는지
pub fn isolated() -> bool {
    ISOLATED.load(Ordering::SeqCst)
}

/// 보내기 앞의 확인
pub(crate) fn check_send() -> Result<(), Status> {
    if isolated() {
        return Err(Status::unavailable(
            "this node is isolated (test); not sending",
        ));
    }
    Ok(())
}

/// 받기 앞의 확인
pub(crate) fn check_receive() -> Result<(), Status> {
    if isolated() {
        return Err(Status::unavailable(
            "this node is isolated (test); not receiving",
        ));
    }
    Ok(())
}
