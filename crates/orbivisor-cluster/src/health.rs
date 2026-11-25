//! 헬스 체크(설계 §4)가 보는 이 노드의 Raft 상태.
//!
//! openraft 0.9는 팔로워에게 리더의 커밋 인덱스를 알려 주지 않는다. 그래서 "적용이 리더 커밋보다 얼마나
//! 뒤처졌나"를 이 노드 로그로 어림한다: 어느 때 로그에 있던 마지막 엔트리를 아직 적용하지 못했으면 그때부터
//! 지금까지가 뒤처진 시간이다(리더에게서 받은 엔트리는 보통 다음 하트비트에 커밋된다).

use std::time::{Duration, Instant};

use crate::types::NodeId;

/// 헬스 체크가 보는 이 노드의 Raft 상태
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaftHealth {
    /// 이 노드가 아는 리더
    pub leader: Option<NodeId>,
    /// 리더를 안다. 이 노드가 리더면 과반수가 [`QUORUM_ACK_MAX`] 안에 답했다.
    pub quorum: bool,
    /// 로그에 들어온 엔트리를 적용하지 못한 채 지난 시간([`ApplyLag`])
    pub apply_lag: Duration,
}

/// 리더가 과반수의 답을 이보다 오래 받지 못하면 정족수가 없다고 본다.
pub const QUORUM_ACK_MAX: Duration = Duration::from_secs(5);

/// 적용이 로그를 따라잡지 못한 시간을 잰다. 볼 때마다(`observe`) 로그의 마지막 인덱스와 적용한
/// 인덱스를 넣는다.
#[derive(Debug, Default)]
pub struct ApplyLag {
    /// 아직 적용되지 않은 로그의 마지막 인덱스와 그것을 처음 본 때
    pending: Option<(u64, Instant)>,
}

impl ApplyLag {
    /// `now`에 본 로그의 마지막 인덱스(`last_log`)와 적용한 인덱스(`applied`)로 뒤처진 시간을 돌려준다.
    /// 앞서 본 마지막 인덱스까지 적용됐으면 0부터 다시 잰다.
    pub fn observe(
        &mut self,
        last_log: Option<u64>,
        applied: Option<u64>,
        now: Instant,
    ) -> Duration {
        if let Some((target, since)) = self.pending {
            if applied >= Some(target) {
                self.pending = None;
            } else {
                return now.saturating_duration_since(since);
            }
        }
        if let Some(last) = last_log
            && applied < Some(last)
        {
            self.pending = Some((last, now));
        }
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag_counts_from_when_an_unapplied_entry_was_first_seen() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut lag = ApplyLag::default();
        // 다 적용했으면 0이다.
        assert_eq!(lag.observe(Some(5), Some(5), t0), Duration::ZERO);
        assert_eq!(lag.observe(None, None, t0), Duration::ZERO);
        // 처음 본 미적용 엔트리는 0부터 잰다.
        assert_eq!(lag.observe(Some(7), Some(5), t0), Duration::ZERO);
        // 그 엔트리(7)를 적용하지 못하는 동안은 처음 본 때부터 늘어난다(새 엔트리가 와도).
        assert_eq!(lag.observe(Some(9), Some(6), t0 + s(3)), s(3));
        assert_eq!(lag.observe(Some(9), Some(6), t0 + s(6)), s(6));
        // 7까지 적용하면 0이 되고, 남은 9는 지금부터 잰다.
        assert_eq!(lag.observe(Some(9), Some(7), t0 + s(7)), Duration::ZERO);
        assert_eq!(lag.observe(Some(9), Some(8), t0 + s(9)), s(2));
        assert_eq!(lag.observe(Some(9), Some(9), t0 + s(10)), Duration::ZERO);
        assert_eq!(lag.observe(Some(9), Some(9), t0 + s(20)), Duration::ZERO);
    }

    #[test]
    fn steady_writes_never_lag() {
        // 쓰기가 이어지면 적용은 늘 조금 뒤지만, 앞서 본 엔트리는 다음에 볼 때 적용돼 있다.
        let t0 = Instant::now();
        let mut lag = ApplyLag::default();
        for i in 0..100u64 {
            let now = t0 + Duration::from_secs(i);
            assert_eq!(
                lag.observe(Some(i * 10 + 5), Some(i * 10), now),
                Duration::ZERO
            );
        }
    }
}
