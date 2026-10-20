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
            // 바이너리나 풀 디렉터리가 없거나, 권한이 없거나, 파일시스템이 기능(hard link 등)을
            // 지원하지 않으면 다시 해도 실패한다.
            DriverError::Io(e) => !matches!(
                e.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::InvalidInput
                    | std::io::ErrorKind::Unsupported
            ),
            DriverError::Qmp(_) | DriverError::Timeout(_) => true,
            DriverError::NotRunning(_)
            | DriverError::InvalidSpec(_)
            | DriverError::StartFailed(_)
            | DriverError::VolumeNotFound(_)
            | DriverError::VolumeExists(_)
            | DriverError::ToolFailed { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_errors() {
        assert!(DriverError::Timeout("qmp").is_transient());
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
        assert!(!DriverError::StartFailed("x".into()).is_transient());
        assert!(!DriverError::InvalidSpec("x".into()).is_transient());
        assert!(!DriverError::VolumeNotFound(Uuid::nil()).is_transient());
    }
}
