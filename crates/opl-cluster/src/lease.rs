//! 리더의 HA 리스 표. 노드가 보낸 갱신(`RenewLease`)을 받은 시각을 임기별로 메모리에 둔다.
//! 안전 조건은 설계 문서(2026-11-02-ha-fencing-witness-design.md) §3.2·§4.2를 따른다.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openraft::error::{CheckIsLeaderError, RaftError};
use openraft::{LogId, Raft, ServerState};
use opl_types::ha_check::CheckReport;
use opl_types::placement::LoadSummary;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::cluster::ClusterError;
use crate::leader::{LeaderError, not_leader};
use crate::types::{NodeId, Rejection, TypeConfig};

/// 한 노드의 갱신에 실린 HA 검사 결과 수 상한. 넘으면 앞의 것만 둔다.
pub const MAX_LEASE_CHECKS: usize = 128;

/// 리더가 아는 리스. `renewed`는 `term` 임기에 이 리더가 확인해 준 노드별 마지막 갱신 시각이다.
/// `checks`는 같은 임기에 노드가 마지막 갱신에 실어 보낸 HA 검사 결과다(보내지 않은 노드는 없다).
/// `loads`는 같은 임기에 노드가 마지막으로 보낸 부하 요약이다(요약이 없는 갱신은 지난 것을 둔다).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Leases {
    pub term: u64,
    pub renewed: BTreeMap<NodeId, Instant>,
    pub checks: BTreeMap<NodeId, Vec<CheckReport>>,
    pub loads: BTreeMap<NodeId, LoadReport>,
}

/// 리더가 받은 노드의 부하 요약과 받은 시각. 오래됐는지는 쓰는 쪽이 `at`으로 판단한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    pub at: Instant,
    pub load: LoadSummary,
}

#[derive(Default)]
struct Inner {
    /// `renewed`가 속한 임기. 더 높은 임기를 보면 `renewed`를 비운다.
    term: u64,
    renewed: BTreeMap<NodeId, Instant>,
    /// `renewed`와 같은 임기에 받은 HA 검사 결과
    checks: BTreeMap<NodeId, Vec<CheckReport>>,
    /// `renewed`와 같은 임기에 받은 부하 요약
    loads: BTreeMap<NodeId, LoadReport>,
    /// 펜싱 중인 노드. 이 노드의 갱신은 `Fenced`로 거부한다. 임기가 바뀌어도 지우지 않는다.
    /// 지우면 이전 임기에 시작한 복구 쓰기가 끝나기 전에 그 노드가 리스를 받을 수 있다.
    fencing: BTreeSet<NodeId>,
}

impl Inner {
    /// 임기가 바뀌었으면 갱신 기록과 검사 결과, 부하 요약을 비운다.
    fn enter(&mut self, term: u64) {
        if term > self.term {
            self.term = term;
            self.renewed.clear();
            self.checks.clear();
            self.loads.clear();
        }
    }
}

#[derive(Default)]
pub(crate) struct LeaseTable {
    inner: Mutex<Inner>,
}

impl LeaseTable {
    /// 리더로서 `node`의 갱신을 처리한다. 성공하면 리더가 적용한 마지막 로그 인덱스를 돌려준다. 갱신을 적을 때
    /// 그 노드의 검사 결과를 `checks`로 통째로 바꾸고, `load`가 있으면 그 노드의 부하 요약을 바꾼다(없으면 지난
    /// 요약을 둔다).
    pub(crate) async fn renew(
        &self,
        raft: &Raft<TypeConfig>,
        confirm: &ConfirmBatch,
        node: NodeId,
        mut checks: Vec<CheckReport>,
        load: Option<LoadSummary>,
        timeout: Duration,
    ) -> Result<u64, ClusterError> {
        let term = {
            let m = raft.metrics().borrow().clone();
            if m.state != ServerState::Leader {
                return Err(not_leader(&m));
            }
            m.current_term
        };
        let deadline = Instant::now() + timeout;
        // 아직 과반수의 리더인지 확인한다. 노드는 보내기 전에 시각을 쟀으므로 이 확인 뒤에 적는
        // 시각은 그보다 늦다. 새 임기의 과반수가 투표한 뒤에는 이 확인이 성공할 수 없다.
        // 확인은 다른 갱신과 함께 쓰지만 이 요청을 받은 뒤 시작한 것만 기다린다(`ConfirmBatch`).
        confirm.confirm(deadline).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            let m = raft.metrics().borrow().clone();
            // 확인하는 사이 임기가 바뀌었으면 그 임기의 표에 적지 않는다.
            if m.state != ServerState::Leader || m.current_term != term {
                return Err(not_leader(&m));
            }
            // 펜싱 검사와 기록은 한 잠금 안에서 한다(`Cluster::begin_fence` 참고).
            if inner.fencing.contains(&node) {
                return Err(ClusterError::Rejected(Rejection::Fenced { node }));
            }
            inner.enter(term);
            let now = Instant::now();
            inner.renewed.insert(node, now);
            if let Some(mut load) = load {
                load.truncate_vms();
                inner.loads.insert(node, LoadReport { at: now, load });
            }
            checks.truncate(MAX_LEASE_CHECKS);
            if checks.is_empty() {
                inner.checks.remove(&node);
            } else {
                inner.checks.insert(node, checks);
            }
        }
        // 돌려주는 인덱스는 펜싱 검사 뒤에 다시 확인한 커밋 인덱스다. 펜싱 표시를 지우기 전에 끝난
        // 복구 쓰기가 모두 들어 있다. 첫 확인의 인덱스는 그보다 앞설 수 있고, 지표(metrics)의 적용
        // 인덱스는 쓰기 응답보다 늦게 바뀔 수 있어 쓰지 않는다.
        let read = confirm.confirm(deadline).await?;
        Ok(read.map_or(0, |l| l.index))
    }

    /// 지금 이 노드의 임기 기준의 리스. 표가 더 낮은 임기의 것이면 비운다.
    pub(crate) fn leases(&self, raft: &Raft<TypeConfig>) -> Leases {
        let mut inner = self.inner.lock().unwrap();
        let term = raft.metrics().borrow().current_term;
        inner.enter(term);
        Leases {
            term,
            renewed: inner.renewed.clone(),
            checks: inner.checks.clone(),
            loads: inner.loads.clone(),
        }
    }

    pub(crate) fn begin_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.insert(node);
    }

    pub(crate) fn end_fence(&self, node: NodeId) {
        self.inner.lock().unwrap().fencing.remove(&node);
    }
}

/// 확인 한 번의 결과. 기다리던 요청이 모두 나눠 가지므로 복제할 수 있는 `LeaderError`로 둔다.
type Confirmed = Result<Option<LogId<NodeId>>, LeaderError>;

/// 확인 한 번을 하는 함수. 실제로는 `confirm_leader`이고, 시험은 가짜로 횟수를 센다.
type ConfirmFn = dyn Fn(Instant) -> Pin<Box<dyn Future<Output = Result<Option<LogId<NodeId>>, ClusterError>> + Send>>
    + Send
    + Sync;

/// 아직 시작하지 않은 확인 한 번과 그것을 기다리는 요청들
struct Round {
    /// 기다리는 요청 가운데 가장 늦은 제한 시각. 확인은 이때까지 한다.
    deadline: Instant,
    result: watch::Sender<Option<Confirmed>>,
}

#[derive(Default)]
struct BatchState {
    /// 다음에 시작할 확인. 여기 든 요청은 모두 그 확인이 시작하기 전에 왔다.
    next: Option<Round>,
    /// 확인을 도는 작업이 있다.
    running: bool,
}

struct BatchShared {
    confirm: Box<ConfirmFn>,
    state: Mutex<BatchState>,
    /// 확인을 도는 작업을 띄울 실행기. 요청한 쪽의 실행기가 아니라 만들 때(`Cluster::open`, core)의 것이다.
    runtime: tokio::runtime::Handle,
}

/// 리스 갱신(HA·VIP)의 리더 확인(`confirm_leader`)을 함께 쓴다. 갱신마다 과반 왕복을 하지 않고, 같은
/// 때에 온 요청은 확인 한 번을 나눠 쓴다.
///
/// 안전 조건: 요청은 자기가 온 뒤에 *시작한* 확인의 결과만 쓴다. 노드는 보내기 전에 시각을 쟀으므로
/// 그 확인은 그 시각보다 뒤의 리더십을 보여 준다. 그래서 확인이 도는 중에 온 요청은 그 확인에 끼지
/// 않고 다음 확인(`next`)을 기다린다. 다음 확인은 지금 확인이 끝나면 곧 시작하므로 요청 하나는 확인을
/// 많아야 두 번 기다린다.
#[derive(Clone)]
pub(crate) struct ConfirmBatch {
    shared: Arc<BatchShared>,
}

impl ConfirmBatch {
    /// 확인 작업은 지금 실행기에서 돈다.
    pub(crate) fn new(
        confirm: impl Fn(
            Instant,
        ) -> Pin<
            Box<dyn Future<Output = Result<Option<LogId<NodeId>>, ClusterError>> + Send>,
        > + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(BatchShared {
                confirm: Box::new(confirm),
                state: Mutex::default(),
                runtime: tokio::runtime::Handle::current(),
            }),
        }
    }

    /// `raft`의 `confirm_leader`를 묶는다.
    pub(crate) fn for_raft(raft: Raft<TypeConfig>) -> Self {
        // `Cluster::open`에서 부르므로 확인 작업은 그 실행기(core)에서 돈다.
        Self::new(move |deadline| {
            let raft = raft.clone();
            Box::pin(async move { confirm_leader(&raft, deadline).await })
        })
    }

    /// 이 요청을 받은 뒤 시작한 확인의 결과. `deadline`까지 끝나지 않으면 `Timeout`이다.
    pub(crate) async fn confirm(
        &self,
        deadline: Instant,
    ) -> Result<Option<LogId<NodeId>>, ClusterError> {
        let (mut rx, start) = {
            let mut state = self.shared.state.lock().unwrap();
            let round = state.next.get_or_insert_with(|| Round {
                deadline,
                result: watch::channel(None).0,
            });
            round.deadline = round.deadline.max(deadline);
            let rx = round.result.subscribe();
            let start = !state.running;
            state.running = true;
            (rx, start)
        };
        if start {
            // 요청한 쪽이 기다리다 그만두어도 확인은 끝까지 돌아 다른 요청에 결과를 준다. 가드는 띄우기 전에
            // 만들어 넘긴다: 실행기가 멈춰 작업이 한 번도 돌지 못하고 버려져도 되돌린다(그때는 `spawn` 안에서
            // 가드가 잠금을 잡으므로 잠금 밖에서 띄운다).
            let guard = DriveGuard {
                shared: self.shared.clone(),
                finished: false,
            };
            self.shared.runtime.spawn(drive(guard));
        }
        match tokio::time::timeout_at(deadline, rx.wait_for(Option::is_some)).await {
            Err(_) => Err(ClusterError::Timeout),
            Ok(Err(_)) => Err(ClusterError::Raft("leader confirmation was dropped".into())),
            Ok(Ok(result)) => result
                .clone()
                .expect("checked by wait_for")
                .map_err(Into::into),
        }
    }
}

/// 확인 작업이 제대로 끝나지 않으면(panic, 실행기가 멈춰 도중에 또는 돌기 전에 버려짐) `running`을 되돌리고
/// 다음 확인을 기다리던 요청에 오류를 준다. 그러지 않으면 뒤의 요청이 모두 확인 없이 제한 시간까지 기다린다(돌던
/// 확인의 요청은 보낸 쪽이 사라져 오류를 받는다). 기다리는 요청이 없어 정상으로 끝날 때만 `finished`를 세운다.
struct DriveGuard {
    shared: Arc<BatchShared>,
    finished: bool,
}

impl Drop for DriveGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.running = false;
        if let Some(round) = state.next.take() {
            round.result.send_replace(Some(Err(LeaderError::Other {
                message: "leader confirmation stopped".into(),
            })));
        }
    }
}

/// 기다리는 요청이 있는 동안 확인을 하나씩 한다.
async fn drive(mut guard: DriveGuard) {
    let shared = guard.shared.clone();
    loop {
        let round = {
            let mut state = shared.state.lock().unwrap();
            match state.next.take() {
                Some(round) => round,
                None => {
                    state.running = false;
                    guard.finished = true;
                    return;
                }
            }
        };
        // 여기서 꺼낸 뒤에 온 요청은 새 `next`에 든다. 이 확인은 꺼낸 요청이 모두 온 뒤에 시작한다.
        let result = (shared.confirm)(round.deadline).await;
        round
            .result
            .send_replace(Some(result.map_err(LeaderError::from)));
    }
}

/// 과반수에게 아직 리더인지 확인하고, 그 시점의 커밋 인덱스까지 적용되기를 기다린다.
/// `deadline`까지 끝나지 않으면 `Timeout`이다.
pub(crate) async fn confirm_leader(
    raft: &Raft<TypeConfig>,
    deadline: Instant,
) -> Result<Option<LogId<NodeId>>, ClusterError> {
    match tokio::time::timeout_at(deadline, raft.ensure_linearizable()).await {
        Err(_) => Err(ClusterError::Timeout),
        Ok(Err(RaftError::APIError(CheckIsLeaderError::ForwardToLeader(f)))) => {
            Err(ClusterError::NotLeader {
                leader_id: f.leader_id,
                leader_addr: f.leader_node.map(|n| n.addr().to_string()),
            })
        }
        Ok(Err(e)) => Err(ClusterError::Raft(e.to_string())),
        Ok(Ok(read)) => Ok(read),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use openraft::CommittedLeaderId;
    use tokio::sync::Semaphore;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    /// 확인을 셀 수 있고, 시험이 허락할 때까지 끝나지 않는 가짜 확인. n번째 확인은 인덱스 n을 돌려준다.
    struct Fake {
        batch: ConfirmBatch,
        calls: Arc<AtomicU64>,
        /// 확인 하나를 끝내도록 허락한다.
        release: Arc<Semaphore>,
    }

    fn fake(fail: bool) -> Fake {
        let calls = Arc::new(AtomicU64::new(0));
        let release = Arc::new(Semaphore::new(0));
        let batch = {
            let (calls, release) = (calls.clone(), release.clone());
            ConfirmBatch::new(move |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let release = release.clone();
                Box::pin(async move {
                    release.acquire().await.unwrap().forget();
                    if fail {
                        return Err(ClusterError::NotLeader {
                            leader_id: None,
                            leader_addr: None,
                        });
                    }
                    Ok(Some(LogId::new(
                        CommittedLeaderId::new(1, NodeId::from(1)),
                        n,
                    )))
                })
            })
        };
        Fake {
            batch,
            calls,
            release,
        }
    }

    fn start(f: &Fake, n: usize) -> Vec<tokio::task::JoinHandle<Result<u64, String>>> {
        (0..n)
            .map(|_| {
                let batch = f.batch.clone();
                tokio::spawn(async move {
                    batch
                        .confirm(Instant::now() + WAIT)
                        .await
                        .map(|l| l.unwrap().index)
                        .map_err(|e| e.to_string())
                })
            })
            .collect()
    }

    /// 확인이 `n`번 시작할 때까지 기다린다.
    async fn wait_calls(f: &Fake, n: u64) {
        while f.calls.load(Ordering::SeqCst) < n {
            tokio::task::yield_now().await;
        }
    }

    /// 다음 확인을 기다리는 요청이 `n`개가 될 때까지 기다린다.
    async fn wait_queued(f: &Fake, n: usize) {
        loop {
            let queued = {
                let state = f.batch.shared.state.lock().unwrap();
                state.next.as_ref().map_or(0, |r| r.result.receiver_count())
            };
            if queued >= n {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn results(
        tasks: Vec<tokio::task::JoinHandle<Result<u64, String>>>,
    ) -> Vec<Result<u64, String>> {
        let mut out = Vec::new();
        for t in tasks {
            out.push(t.await.unwrap());
        }
        out
    }

    /// 확인 하나가 도는 동안 온 요청 100개는 다음 확인 한 번을 함께 쓴다.
    #[tokio::test]
    async fn concurrent_requests_share_one_or_two_confirmations() {
        let f = fake(false);
        let first = start(&f, 1);
        wait_calls(&f, 1).await;
        let rest = start(&f, 100);
        wait_queued(&f, 100).await;
        f.release.add_permits(2);

        // 처음 요청은 첫 확인을, 그 확인이 시작한 뒤에 온 요청은 모두 두 번째 확인을 쓴다.
        assert_eq!(results(first).await, vec![Ok(1)]);
        assert!(results(rest).await.iter().all(|r| *r == Ok(2)));
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    /// 확인이 시작한 뒤에 온 요청은 그 확인의 결과를 쓰지 않는다(시작 전 시각 규칙).
    #[tokio::test]
    async fn a_request_never_uses_a_confirmation_that_started_before_it() {
        let f = fake(false);
        let first = start(&f, 1);
        wait_calls(&f, 1).await;
        let late = start(&f, 1);
        wait_queued(&f, 1).await;
        // 첫 확인만 끝낸다. 늦은 요청은 아직 끝나지 않아야 한다.
        f.release.add_permits(1);
        assert_eq!(results(first).await, vec![Ok(1)]);
        wait_calls(&f, 2).await;
        assert!(!late[0].is_finished());

        f.release.add_permits(1);
        assert_eq!(results(late).await, vec![Ok(2)]);
        // 기다리는 요청이 없으면 확인을 더 하지 않는다.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    /// 확인이 실패하면(리더가 아님) 그 확인을 기다린 요청이 모두 실패한다.
    #[tokio::test]
    async fn a_failed_confirmation_fails_every_waiting_request() {
        let f = fake(true);
        let first = start(&f, 1);
        wait_calls(&f, 1).await;
        let rest = start(&f, 20);
        wait_queued(&f, 20).await;
        f.release.add_permits(2);

        for r in results(first).await.into_iter().chain(results(rest).await) {
            let e = r.unwrap_err();
            assert!(e.contains("not the leader"), "{e}");
        }
        assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    }

    /// 확인 함수가 panic해도 묶음이 멈추지 않는다: 그 확인과 다음 확인을 기다리던 요청은 오류를 받고, 그 뒤의
    /// 요청은 새 확인을 받는다.
    #[tokio::test]
    async fn a_panicking_confirmation_does_not_wedge_the_batch() {
        let calls = Arc::new(AtomicU64::new(0));
        let release = Arc::new(Semaphore::new(0));
        let batch = {
            let (calls, release) = (calls.clone(), release.clone());
            ConfirmBatch::new(move |_| {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let release = release.clone();
                Box::pin(async move {
                    release.acquire().await.unwrap().forget();
                    assert!(n != 1, "confirmation panicked");
                    Ok(Some(LogId::new(
                        CommittedLeaderId::new(1, NodeId::from(1)),
                        n,
                    )))
                })
            })
        };
        let f = Fake {
            batch,
            calls,
            release,
        };
        let first = start(&f, 1);
        wait_calls(&f, 1).await;
        let queued = start(&f, 2);
        wait_queued(&f, 2).await;
        f.release.add_permits(1);
        for r in results(first)
            .await
            .into_iter()
            .chain(results(queued).await)
        {
            assert!(r.is_err(), "{r:?}");
        }
        let later = start(&f, 1);
        f.release.add_permits(1);
        assert_eq!(results(later).await, vec![Ok(2)]);
    }

    /// 확인 작업을 띄운 실행기가 멈춰도(작업이 도중에 버려지거나 띄우자마자 버려짐) 묶음이 멈추지 않는다:
    /// 기다리던 요청은 제한 시간을 다 기다리지 않고 오류를 받고, 그 뒤의 요청도 곧바로 오류를 받는다.
    #[tokio::test]
    async fn a_stopped_runtime_does_not_wedge_the_batch() {
        let other = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        // 확인 작업은 `other`에서 돈다.
        let f = {
            let _enter = other.enter();
            fake(false)
        };
        let first = start(&f, 1);
        wait_calls(&f, 1).await;
        let queued = start(&f, 1);
        wait_queued(&f, 1).await;
        other.shutdown_background();
        let within = Duration::from_secs(2);
        let done = tokio::time::timeout(within, results(first.into_iter().chain(queued).collect()))
            .await
            .expect("requests waited out their deadline");
        assert!(done.iter().all(Result::is_err), "{done:?}");
        // 멈춘 실행기에 띄운 작업은 곧바로 버려진다.
        let later = tokio::time::timeout(within, results(start(&f, 1)))
            .await
            .expect("a later request waited out its deadline");
        assert!(later[0].is_err(), "{later:?}");
    }

    /// 제한 시각까지 확인이 끝나지 않으면 그 요청만 `Timeout`이고, 확인은 남은 요청에 결과를 준다.
    #[tokio::test]
    async fn a_request_times_out_without_breaking_the_round() {
        let f = fake(false);
        let short = f.batch.clone();
        let short = tokio::spawn(async move {
            short
                .confirm(Instant::now() + Duration::from_millis(50))
                .await
        });
        wait_calls(&f, 1).await;
        let err = short.await.unwrap().unwrap_err();
        assert!(matches!(err, ClusterError::Timeout), "{err:?}");

        let later = start(&f, 3);
        wait_queued(&f, 3).await;
        f.release.add_permits(2);
        assert_eq!(results(later).await, vec![Ok(2); 3]);
    }
}
