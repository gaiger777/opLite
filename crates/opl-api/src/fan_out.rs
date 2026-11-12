//! 여러 노드에 묻는 일(지표·감사 로그 모으기)의 동시성 상한. 노드가 수백 대여도 한꺼번에 여는 연결은
//! `FAN_OUT`개까지다(설계 §5).

use std::future::Future;

use futures_util::StreamExt;

/// 한꺼번에 묻는 노드 수
pub(crate) const FAN_OUT: usize = 32;

/// `futs`를 동시에 `FAN_OUT`개까지 돌려 모든 결과를 모은다. 끝난 것부터 다음 것을 시작하므로 느린 노드
/// 하나가 나머지를 막지 않는다. 결과는 `futs`의 순서다(`join_all`과 같아, 같은 시각의 지표 표본 순서가
/// 바뀌지 않는다).
pub(crate) async fn gather<F: Future>(futs: Vec<F>) -> Vec<F::Output> {
    let mut out: Vec<(usize, F::Output)> = futures_util::stream::iter(
        futs.into_iter()
            .enumerate()
            .map(|(i, f)| async move { (i, f.await) }),
    )
    .buffer_unordered(FAN_OUT)
    .collect()
    .await;
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn at_most_fan_out_run_at_once_and_results_keep_their_order() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let out = gather(
            (0..100u64)
                .map(|i| {
                    let (running, peak) = (&running, &peak);
                    async move {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        // 뒤의 것이 먼저 끝나도 결과 순서는 그대로다.
                        tokio::time::sleep(Duration::from_millis(100 - i)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        i
                    }
                })
                .collect(),
        )
        .await;
        assert_eq!(peak.load(Ordering::SeqCst), FAN_OUT);
        assert_eq!(out, (0..100).collect::<Vec<_>>());
    }

    /// 첫 노드만 느려도 나머지는 남은 자리로 계속 돈다: 전체 시간은 느린 하나의 시간 정도다(차례대로
    /// 돌리면 느린 하나가 끝날 때까지 그 뒤의 것이 시작하지 못해 그만큼 더 걸린다).
    #[tokio::test(start_paused = true)]
    async fn one_slow_node_does_not_hold_back_the_rest() {
        let started = tokio::time::Instant::now();
        let out = gather(
            (0..FAN_OUT * 10)
                .map(|i| async move {
                    let ms = if i == 0 { 1000 } else { 50 };
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    i
                })
                .collect(),
        )
        .await;
        let took = started.elapsed();
        assert!(took < Duration::from_millis(1100), "took {took:?}");
        assert_eq!(out, (0..FAN_OUT * 10).collect::<Vec<_>>());
    }
}
