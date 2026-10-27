use opl_qemu::QmpError;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DriverError {
    #[error("VM {0} is not running")]
    NotRunning(Uuid),
    #[error("invalid VM definition: {0}")]
    InvalidSpec(String),
    #[error("QEMU failed to start: {0}")]
    StartFailed(String),
    #[error("volume {0} not found")]
    VolumeNotFound(Uuid),
    #[error("volume {0} already exists")]
    VolumeExists(Uuid),
    #[error("{tool} failed: {stderr}")]
    ToolFailed { tool: &'static str, stderr: String },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// 살아 있지만 아직 다시 붙지 못해 QMP로 다룰 수 없는 VM. 요청을 보내지 않았으므로
    /// 하이퍼바이저 안에서 돌고 있는 작업은 없다.
    #[error("VM {0} is running but not attached yet")]
    NotAttached(Uuid),
    #[error(transparent)]
    Qmp(#[from] QmpError),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl DriverError {
    /// 같은 요청을 잠시 뒤 다시 시도하면 성공할 수 있는 오류인지.
    /// 영구 오류는 status에 기록하고 사용자에게 알려야 한다.
    pub fn is_transient(&self) -> bool {
        match self {
            DriverError::Qmp(QmpError::Command { .. }) => false,
            // 작업이 오류로 끝났으면 다시 해도 같다. 시간 초과면 작업이 QEMU에 남아 있을 수 있다.
            DriverError::Qmp(QmpError::JobFailed { .. } | QmpError::JobTimeout { .. }) => false,
            // 바이너리나 풀 디렉터리가 없거나, 권한이 없거나, 파일시스템이 기능(hard link 등)을
            // 지원하지 않으면 다시 해도 실패한다.
            DriverError::Io(e) => !matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::InvalidInput
                    | std::io::ErrorKind::Unsupported
            ),
            DriverError::Qmp(_) | DriverError::Timeout(_) | DriverError::NotAttached(_) => true,
            DriverError::NotRunning(_)
            | DriverError::InvalidSpec(_)
            | DriverError::StartFailed(_)
            | DriverError::VolumeNotFound(_)
            | DriverError::VolumeExists(_)
            | DriverError::ToolFailed { .. } => false,
        }
    }

    /// 응답을 기다리다 시간이 다 된 오류인지. 요청한 작업(스냅샷 저장 등)이 하이퍼바이저 안에서
    /// 아직 돌고 있을 수 있으므로, 그 결과를 곧바로 되돌리거나 지우면 안 된다.
    pub fn may_be_in_progress(&self) -> bool {
        matches!(
            self,
            DriverError::Qmp(QmpError::JobTimeout { .. }) | DriverError::Timeout(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_errors() {
        assert!(DriverError::Timeout("qmp").is_transient());
        assert!(DriverError::NotAttached(Uuid::nil()).is_transient());
        assert!(DriverError::Qmp(QmpError::Disconnected).is_transient());
        assert!(DriverError::Io(std::io::Error::other("x")).is_transient());
        // 바이너리나 풀 디렉터리가 없거나 권한이 없으면 다시 해도 실패한다.
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::InvalidInput,
            // 하드 링크를 지원하지 않는 파일시스템(exFAT 등)
            std::io::ErrorKind::Unsupported,
        ] {
            assert!(!DriverError::Io(kind.into()).is_transient(), "{kind:?}");
        }

        let cmd = QmpError::Command {
            class: "GenericError".into(),
            desc: "x".into(),
        };
        assert!(!DriverError::Qmp(cmd).is_transient());
        // 스냅샷 작업이 오류로 끝났거나(없는 태그 등) 제한 시간을 넘겼다. 시간 초과면 작업이
        // QEMU에 남아 있을 수 있으므로 같은 요청을 곧바로 다시 보내지 않는다.
        let failed = QmpError::JobFailed {
            id: "j".into(),
            error: "x".into(),
        };
        assert!(!DriverError::Qmp(failed).is_transient());
        let timeout = QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(1),
        };
        assert!(!DriverError::Qmp(timeout).is_transient());
        assert!(!DriverError::StartFailed("x".into()).is_transient());
        assert!(!DriverError::InvalidSpec("x".into()).is_transient());
        assert!(!DriverError::VolumeNotFound(Uuid::nil()).is_transient());
    }

    #[test]
    fn timeouts_may_leave_work_in_progress() {
        let timeout = QmpError::JobTimeout {
            id: "j".into(),
            timeout: std::time::Duration::from_secs(1),
        };
        assert!(DriverError::Qmp(timeout).may_be_in_progress());
        assert!(DriverError::Timeout("QMP transaction").may_be_in_progress());
        let failed = QmpError::JobFailed {
            id: "j".into(),
            error: "x".into(),
        };
        assert!(!DriverError::Qmp(failed).may_be_in_progress());
        assert!(!DriverError::NotRunning(Uuid::nil()).may_be_in_progress());
        // 다시 붙지 못한 VM에는 요청을 보내지 않았다.
        assert!(!DriverError::NotAttached(Uuid::nil()).may_be_in_progress());
    }
}
