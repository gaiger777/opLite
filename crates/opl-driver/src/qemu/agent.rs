//! 시점을 찍는 QMP 앞뒤의 게스트 파일 시스템 얼리기(계획 59 §9). 호출자는 VM의 에이전트 잠금
//! ([`VmHandle::agent`](super::VmHandle))을 쥔 채로 쓴다.

use std::path::{Path, PathBuf};

use opl_qemu::RuntimePaths;
use opl_qemu::qga::{self, SYNC_TIMEOUT, Session};
use uuid::Uuid;

use crate::hypervisor::Quiesced;

/// 얼린 동안 실행 디렉터리에 두는 표시. opld가 그사이 죽으면 다시 붙을 때 녹인다.
pub(super) const FSFROZEN_MARK: &str = "fsfrozen";
/// 녹이기를 다시 해 보는 횟수(세션마다 새로 맞춘다)
const THAW_TRIES: u32 = 3;

pub(super) struct Freezer {
    want: bool,
    sock: PathBuf,
    mark: PathBuf,
    session: Option<Session>,
    outcome: Quiesced,
}

impl Freezer {
    /// 얼리지 않는다.
    pub(super) fn off() -> Self {
        Self {
            want: false,
            sock: PathBuf::new(),
            mark: PathBuf::new(),
            session: None,
            outcome: Quiesced::No,
        }
    }

    /// `want`면 이 VM의 게스트 에이전트 채널로 얼린다.
    pub(super) fn new(want: bool, paths: &RuntimePaths) -> Self {
        Self {
            want,
            sock: paths.qga_socket.clone(),
            mark: paths.dir.join(FSFROZEN_MARK),
            session: None,
            outcome: Quiesced::No,
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
        if let Err(e) = std::fs::write(&self.mark, b"") {
            self.outcome = Quiesced::Skipped(format!("cannot write the freeze mark: {e}"));
            return;
        }
        let mut session = match Session::connect(&self.sock, SYNC_TIMEOUT).await {
            Ok(s) => s,
            Err(e) => {
                self.outcome = Quiesced::Skipped(e.to_string());
                remove_mark(&self.mark);
                return;
            }
        };
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
                    tracing::error!(%id, "cannot thaw guest file systems after a failed freeze");
                }
            }
        }
    }

    /// 얼렸으면 녹인다(`guest-fsfreeze-thaw`). 녹이지 못하면 표시를 남겨 다시 붙을 때 녹인다.
    pub(super) async fn thaw(&mut self, id: Uuid) {
        if self.outcome != Quiesced::Yes {
            return;
        }
        if thaw_tries(&self.sock, self.session.take()).await {
            remove_mark(&self.mark);
        } else {
            tracing::error!(%id, "cannot thaw guest file systems; will try again when reattaching");
        }
    }

    pub(super) fn outcome(self) -> Quiesced {
        self.outcome
    }
}

/// 다시 붙은 VM에 얼림 표시가 남았으면(얼린 채 opld가 죽었다) 녹이고 표시를 지운다.
pub(super) async fn thaw_left(id: Uuid, paths: &RuntimePaths) {
    let mark = paths.dir.join(FSFROZEN_MARK);
    if !mark.exists() {
        return;
    }
    tracing::warn!(%id, "guest file systems were left frozen; thawing");
    if thaw_tries(&paths.qga_socket, None).await {
        remove_mark(&mark);
    } else {
        tracing::error!(%id, "cannot thaw guest file systems left frozen");
    }
}

/// `session`(없으면 새 세션)으로 녹이고, 안 되면 새 세션으로 [`THAW_TRIES`]번까지 해 본다.
async fn thaw_tries(sock: &Path, mut session: Option<Session>) -> bool {
    for _ in 0..THAW_TRIES {
        let mut s = match session.take() {
            Some(s) => s,
            None => match Session::connect(sock, SYNC_TIMEOUT).await {
                Ok(s) => s,
                Err(_) => continue,
            },
        };
        if qga::thaw(&mut s).await.is_ok() {
            return true;
        }
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
