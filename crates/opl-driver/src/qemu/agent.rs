//! 시점을 찍는 QMP 앞뒤의 게스트 파일 시스템 얼리기(계획 59 §9). [`Freezer`]는 얼리기부터 녹이기까지만 VM의
//! 에이전트 잠금([`VmHandle::agent`](super::VmHandle))을 쥔다. 백업·복제 작업 전체가 아니라 그 창만 쥐어야
//! 작업자의 ping·정보 수집·에이전트 종료가 몇 시간씩 기다리지 않는다.

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_qemu::RuntimePaths;
use opl_qemu::qga::{self, FREEZE_TIMEOUT, SYNC_TIMEOUT, Session};
use std::sync::Arc;

use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

use crate::hypervisor::Quiesced;

/// 얼린 동안 실행 디렉터리에 두는 표시. opld가 그사이 죽으면 다시 붙을 때 녹인다.
pub(super) const FSFROZEN_MARK: &str = "fsfrozen";
/// 녹이기를 다시 해 보는 최소 횟수(세션마다 새로 맞춘다)
const THAW_TRIES: u32 = 3;
/// 녹이기를 다시 해 보는 시간. 시간이 넘은 freeze를 에이전트가 아직 하고 있을 수 있으므로 [`FREEZE_TIMEOUT`]보다 길다.
const THAW_WINDOW: Duration = FREEZE_TIMEOUT.saturating_add(Duration::from_secs(5));
/// 실패한 녹이기 사이의 쉼
const THAW_PAUSE: Duration = Duration::from_millis(500);

pub(super) struct Freezer {
    want: bool,
    sock: PathBuf,
    mark: PathBuf,
    lock: Option<Arc<Mutex<()>>>,
    /// 얼리기부터 녹이기까지 쥐는 에이전트 잠금. 버려진 뒤 띄운 녹이기도 끝날 때까지 쥔다(리뷰 2 #4).
    guard: Option<OwnedMutexGuard<()>>,
    session: Option<Session>,
    outcome: Quiesced,
    /// 얼렸고 아직 녹이지 않았다(떨어뜨리면 녹이기를 띄운다).
    frozen: bool,
}

impl Freezer {
    /// 얼리지 않는다.
    pub(super) fn off() -> Self {
        Self {
            want: false,
            sock: PathBuf::new(),
            mark: PathBuf::new(),
            lock: None,
            guard: None,
            session: None,
            outcome: Quiesced::No,
            frozen: false,
        }
    }
}

impl Freezer {
    /// `want`면 이 VM의 게스트 에이전트 채널로 얼린다. `lock`은 VM의 에이전트 잠금이다.
    pub(super) fn new(want: bool, paths: &RuntimePaths, lock: &Arc<Mutex<()>>) -> Self {
        Self {
            want,
            sock: paths.qga_socket.clone(),
            mark: paths.dir.join(FSFROZEN_MARK),
            lock: Some(lock.clone()),
            guard: None,
            session: None,
            outcome: Quiesced::No,
            frozen: false,
        }
    }

    /// 얼린다(`guest-fsfreeze-freeze`). 얼리지 못하면 이유를 남기고 계속한다(crash-consistent). 얼리다 시간이 넘으면
    /// 얼었을 수 있으므로 녹여 둔다.
    pub(super) async fn freeze(&mut self, id: Uuid) {
        if !self.want {
            return;
        }
        if !self.sock.exists() {
            self.outcome = Quiesced::Skipped("the VM has no guest agent channel".into());
            return;
        }
        if let Some(lock) = &self.lock {
            self.guard = Some(lock.clone().lock_owned().await);
        }
        if let Err(e) = std::fs::write(&self.mark, b"") {
            self.outcome = Quiesced::Skipped(format!("cannot write the freeze mark: {e}"));
            self.guard = None;
            return;
        }
        let mut session = match Session::connect(&self.sock, SYNC_TIMEOUT).await {
            Ok(s) => s,
            Err(e) => {
                self.outcome = Quiesced::Skipped(e.to_string());
                remove_mark(&self.mark);
                self.guard = None;
                return;
            }
        };
        // 여기서부터 이 future가 버려져도(취소) 떨어뜨릴 때 녹인다.
        self.frozen = true;
        match qga::freeze(&mut session).await {
            Ok(n) => {
                tracing::debug!(%id, filesystems = n, "froze guest file systems");
                self.session = Some(session);
                self.outcome = Quiesced::Yes;
            }
            Err(e) => {
                tracing::warn!(%id, error = %e, "guest agent did not freeze file systems");
                self.outcome = Quiesced::Skipped(e.to_string());
                // 실패한 freeze는 에이전트가 녹이지만, 시간 초과면 얼었을 수 있다.
                let session = e.is_command().then_some(session);
                if thaw_tries(&self.sock, session).await {
                    remove_mark(&self.mark);
                } else {
                    tracing::error!(%id, "cannot thaw guest file systems after a failed freeze; will try again");
                }
                self.frozen = false;
                self.guard = None;
            }
        }
    }

    /// 얼렸으면 녹인다(`guest-fsfreeze-thaw`)고 에이전트 잠금을 놓는다. 녹이지 못하면 표시를 남긴다. 표시는 다음
    /// ping·정보 수집([`thaw_left`])이나 다시 붙을 때 녹인다.
    pub(super) async fn thaw(&mut self, id: Uuid) {
        if self.outcome == Quiesced::Yes {
            if thaw_tries(&self.sock, self.session.take()).await {
                remove_mark(&self.mark);
            } else {
                tracing::error!(%id, "cannot thaw guest file systems; will try again");
            }
        }
        self.frozen = false;
        self.guard = None;
    }

    pub(super) fn outcome(mut self) -> Quiesced {
        std::mem::take(&mut self.outcome)
    }
}

impl Drop for Freezer {
    /// 얼린 뒤 녹이기 전에 버려졌으면(호출한 future가 취소됨) 녹이기를 띄운다. 띄운 녹이기가 에이전트 잠금을 넘겨받아
    /// 끝날 때까지 쥔다(리뷰 2 #4): 그사이 다음 회차가 얼린 것을 이 녹이기가 녹이거나 그 표시를 지우지 않게. 띄울
    /// 런타임이 없으면 표시가 남아 다음 ping·정보 수집이 녹인다.
    fn drop(&mut self) {
        if !self.frozen {
            return;
        }
        let (sock, mark, session) = (self.sock.clone(), self.mark.clone(), self.session.take());
        let guard = self.guard.take();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            tracing::warn!(
                ?sock,
                "freeze abandoned before thawing; thawing in the background"
            );
            rt.spawn(async move {
                if thaw_tries(&sock, session).await {
                    remove_mark(&mark);
                }
                drop(guard);
            });
        }
    }
}

/// 얼림 표시가 남았으면(녹이기 실패, 얼린 채 opld가 죽음) 녹이고 표시를 지운다. 다시 붙을 때와 에이전트 잠금을
/// 쥔 ping·정보 수집이 부른다(잠금을 쥐었으니 얼리는 중인 [`Freezer`]는 없다).
pub(super) async fn thaw_left(id: Uuid, paths: &RuntimePaths) {
    let mark = paths.dir.join(FSFROZEN_MARK);
    if !mark.exists() {
        return;
    }
    tracing::warn!(%id, "guest file systems were left frozen; thawing");
    if thaw_tries(&paths.qga_socket, None).await {
        remove_mark(&mark);
    } else {
        tracing::error!(%id, "cannot thaw guest file systems left frozen; will try again");
    }
}

/// `session`(없으면 새 세션)으로 녹이고, 안 되면 새 세션으로 다시 해 본다. 적어도 [`THAW_TRIES`]번, [`THAW_WINDOW`]가
/// 지날 때까지 해 본다. 채널 소켓이 없으면(QEMU가 끝남) 곧바로 그만둔다.
async fn thaw_tries(sock: &Path, mut session: Option<Session>) -> bool {
    let start = tokio::time::Instant::now();
    let mut tries = 0;
    while tries < THAW_TRIES || start.elapsed() < THAW_WINDOW {
        tries += 1;
        let s = match session.take() {
            Some(s) => Some(s),
            None if !sock.exists() => return false,
            None => Session::connect(sock, SYNC_TIMEOUT).await.ok(),
        };
        if let Some(mut s) = s
            && qga::thaw(&mut s).await.is_ok()
        {
            return true;
        }
        tokio::time::sleep(THAW_PAUSE).await;
    }
    false
}

fn remove_mark(mark: &Path) {
    if let Err(e) = std::fs::remove_file(mark)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(?mark, error = %e, "cannot remove the freeze mark");
    }
}
