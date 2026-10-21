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
        while let Some(&first) = self.failures.front() {
            if now.duration_since(first) < rule.window {
                break;
            }
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        let n = self.failures.len();
        if n >= rule.max_failures {
            self.failures.clear();
            return Next::GiveUp;
        }
        let factor = 1u32 << (n - 1).min(16);
        Next::RetryAfter(rule.initial.saturating_mul(factor).min(rule.max))
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
