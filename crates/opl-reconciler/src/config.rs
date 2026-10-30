use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opl_driver::Storage;

use crate::firmware::FirmwareFetcher;
use uuid::Uuid;

/// reconciler 설정. 기본값은 설계 문서 5.3절을 따른다.
#[derive(Clone)]
pub struct ReconcilerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 VM만 다룬다.
    pub node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 디스크 경로를 만들 때 쓴다.
    pub mount_root: PathBuf,
    /// VM을 시작하기 전에 디스크 볼륨을 활성화하는 저장소
    pub storage: Arc<dyn Storage>,
    /// 이벤트를 놓쳤더라도 원하는 상태와 실제 상태를 다시 맞추는 주기
    pub resync_interval: Duration,
    /// ACPI 종료를 요청한 뒤 강제 종료로 넘어가기까지 기다리는 시간
    pub acpi_timeout: Duration,
    /// 클러스터 쓰기나 드라이버 호출이 실패했을 때 다시 시도하기까지의 간격.
    /// 과반수가 없어 status를 못 쓰는 동안에도 이 간격으로 다시 쓴다.
    pub retry_interval: Duration,
    pub restart: RestartBackoff,
    /// 다른 노드에 있는 UEFI 변수 파일을 받아 온다. 없으면 받지 않고 이 노드의 파일로 켠다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
}

/// 크래시나 일시적 기동 실패 뒤 다시 시작하는 규칙.
/// `window` 안에 `max_failures`번 실패하면 포기한다.
#[derive(Debug, Clone)]
pub struct RestartBackoff {
    pub initial: Duration,
    pub max: Duration,
    pub max_failures: usize,
    pub window: Duration,
}

impl ReconcilerConfig {
    pub fn new(node: Uuid, mount_root: impl Into<PathBuf>, storage: Arc<dyn Storage>) -> Self {
        Self {
            node,
            mount_root: mount_root.into(),
            storage,
            resync_interval: Duration::from_secs(30),
            acpi_timeout: Duration::from_secs(60),
            retry_interval: Duration::from_secs(2),
            restart: RestartBackoff {
                initial: Duration::from_secs(1),
                max: Duration::from_secs(60),
                max_failures: 5,
                window: Duration::from_secs(600),
            },
            firmware: None,
        }
    }
}
