//! 벽시계(유닉스 ms). 기록 쓰기(계획 60 `HistoryWriter`)와 경보 엔진이 받아 시험이 시계를 앞뒤로 뛰게 한다.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 지금 시각(유닉스 밀리초)을 주는 시계
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// 시스템 벽시계. 1970년 전으로 돌아간 시계는 0이다.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// 손으로 맞추는 시계(시험용)
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }

    pub fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    /// `ms`만큼 앞으로(음수면 뒤로) 옮긴다.
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_both_ways() {
        let c = ManualClock::new(1_000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1_500);
        c.advance(-2_000);
        assert_eq!(c.now_ms(), -500);
        c.set(7);
        assert_eq!(c.now_ms(), 7);
        assert!(SystemClock.now_ms() > 1_700_000_000_000);
    }
}
