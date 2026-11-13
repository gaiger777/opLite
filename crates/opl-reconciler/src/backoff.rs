use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use crate::config::RestartBackoff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    RetryAfter(Duration),
    GiveUp,
}

/// 최근 실패 시각. `window`보다 오래된 실패는 잊는다.
#[derive(Debug, Default)]
pub(crate) struct FailureTracker {
    failures: VecDeque<Instant>,
}

impl FailureTracker {
    /// 실패를 기록하고 다음 행동을 정한다. n번째 실패 뒤에는 `initial * 2^(n-1)`(최대 `max`)만큼
    /// 기다리고, `window` 안에서 `max_failures`번째 실패면 포기한다.
    pub(crate) fn record(&mut self, now: Instant, rule: &RestartBackoff) -> Next {
        self.record_up_to(now, rule, rule.max_failures)
    }

    /// [`FailureTracker::record`]와 같되 `window` 안에서 `max_failures`번째 실패면 포기한다(HA VM의
    /// `max_restart` + 1).
    pub(crate) fn record_up_to(
        &mut self,
        now: Instant,
        rule: &RestartBackoff,
        max_failures: usize,
    ) -> Next {
        while let Some(&first) = self.failures.front() {
            if now.duration_since(first) < rule.window {
                break;
            }
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        let n = self.failures.len();
        if n >= max_failures {
            self.failures.clear();
            return Next::GiveUp;
        }
        let factor = 1u32 << (n - 1).min(16);
        Next::RetryAfter(rule.initial.saturating_mul(factor).min(rule.max))
    }

    /// [`FailureTracker::record_up_to`]와 같되 `window` 안의 첫 실패는 기다리지 않는다(HA VM의 같은 노드 빠른
    /// 재시작). 둘째부터는 같은 백오프다.
    pub(crate) fn record_first_free(
        &mut self,
        now: Instant,
        rule: &RestartBackoff,
        max_failures: usize,
    ) -> Next {
        match self.record_up_to(now, rule, max_failures) {
            Next::RetryAfter(_) if self.failures.len() == 1 => Next::RetryAfter(Duration::ZERO),
            next => next,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.failures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> RestartBackoff {
        RestartBackoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(5),
            max_failures: 5,
            window: Duration::from_secs(600),
        }
    }

    #[test]
    fn doubles_up_to_max_then_gives_up() {
        let mut t = FailureTracker::default();
        let now = Instant::now();
        let delays: Vec<_> = (0..4).map(|_| t.record(now, &rule())).collect();
        assert_eq!(
            delays,
            [1, 2, 4, 5].map(|s| Next::RetryAfter(Duration::from_secs(s)))
        );
        assert_eq!(t.record(now, &rule()), Next::GiveUp);
        // 포기하면 기록을 비우므로 다시 처음부터 센다.
        assert_eq!(
            t.record(now, &rule()),
            Next::RetryAfter(Duration::from_secs(1))
        );
    }

    #[test]
    fn gives_up_at_the_given_limit() {
        let mut t = FailureTracker::default();
        let now = Instant::now();
        // 한도 1이면 첫 실패에 포기한다.
        assert_eq!(t.record_up_to(now, &rule(), 1), Next::GiveUp);
        // 한도 2: 한 번 다시 시도하고 두 번째에 포기한다.
        assert_eq!(
            t.record_up_to(now, &rule(), 2),
            Next::RetryAfter(Duration::from_secs(1))
        );
        assert_eq!(t.record_up_to(now, &rule(), 2), Next::GiveUp);
        // 창 밖의 실패는 세지 않는다.
        t.record_up_to(now, &rule(), 2);
        assert_eq!(
            t.record_up_to(now + Duration::from_secs(600), &rule(), 2),
            Next::RetryAfter(Duration::from_secs(1))
        );
    }

    #[test]
    fn the_first_failure_in_the_window_retries_at_once() {
        let mut t = FailureTracker::default();
        let now = Instant::now();
        let next = |t: &mut FailureTracker, at| t.record_first_free(at, &rule(), 3);
        assert_eq!(next(&mut t, now), Next::RetryAfter(Duration::ZERO));
        // 둘째는 지금 백오프(`initial * 2`)다.
        assert_eq!(next(&mut t, now), Next::RetryAfter(Duration::from_secs(2)));
        assert_eq!(next(&mut t, now), Next::GiveUp);
        // 포기 뒤나 창이 지난 뒤의 첫 실패는 다시 곧바로다.
        assert_eq!(next(&mut t, now), Next::RetryAfter(Duration::ZERO));
        let later = now + Duration::from_secs(600);
        assert_eq!(next(&mut t, later), Next::RetryAfter(Duration::ZERO));
        // 한도 1이면 첫 실패에 포기한다.
        assert_eq!(t.record_first_free(later, &rule(), 1), Next::GiveUp);
    }

    #[test]
    fn forgets_failures_older_than_window() {
        let mut t = FailureTracker::default();
        let start = Instant::now();
        for _ in 0..4 {
            t.record(start, &rule());
        }
        let later = start + Duration::from_secs(600);
        assert_eq!(
            t.record(later, &rule()),
            Next::RetryAfter(Duration::from_secs(1))
        );
    }
}
