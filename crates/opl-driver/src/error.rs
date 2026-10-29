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
    /// NFS 풀이 이 노드에 마운트돼 있지 않다. opld의 풀 새로 고침이 다시 마운트하면 풀린다.
    #[error("storage pool is not mounted at {}", .0.display())]
    PoolNotMounted(std::path::PathBuf),
    /// 풀의 파일시스템 호출이 제한 시간 안에 끝나지 않았다(NFS 서버가 멈춤 등). 서버가 돌아오면 풀린다.
    #[error("storage pool at {} is not responding", .0.display())]
    PoolNotResponding(std::path::PathBuf),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// LVM 명령이 실패했다. 내용은 stderr의 첫 줄이다.
    #[error("lvm failed: {0}")]
    Lvm(String),
    /// 블록 장치 볼륨의 장치 노드가 이 노드에 없다(활성화돼 있지 않거나 노드가 아직 생기지 않았다).
    /// 저장소의 `activate_volume`이 활성화하면 풀린다.
    #[error("volume device node missing: {}", .0.display())]
    VolumeNotActive(std::path::PathBuf),
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
            DriverError::Qmp(_)
            | DriverError::Timeout(_)
            | DriverError::NotAttached(_)
            | DriverError::PoolNotMounted(_)
            | DriverError::PoolNotResponding(_)
            | DriverError::VolumeNotActive(_) => true,
            DriverError::NotRunning(_)
            | DriverError::InvalidSpec(_)
            | DriverError::StartFailed(_)
            | DriverError::VolumeNotFound(_)
            | DriverError::VolumeExists(_)
            | DriverError::ToolFailed { .. }
            | DriverError::Lvm(_) => false,
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
        // NFS 풀을 다시 마운트하면 풀린다.
        assert!(DriverError::PoolNotMounted("/var/lib/opl/pools/p".into()).is_transient());
        assert!(DriverError::PoolNotResponding("/var/lib/opl/pools/p".into()).is_transient());
        // 활성화하면 장치 노드가 생긴다.
        assert!(DriverError::VolumeNotActive("/dev/vg0/opl-x".into()).is_transient());
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
        assert!(!DriverError::Lvm("Volume group \"vg0\" not found".into()).is_transient());
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
