use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use orbivisor_driver::Storage;

use crate::firewall::FirewallGate;
use crate::firmware::FirmwareFetcher;
use crate::guest::GuestInfoSink;
use crate::ha::HaGuard;
use crate::host_alloc::HostAllocator;
use uuid::Uuid;

/// 게스트 감시가 게스트 에이전트를 부르는 기본 간격
pub const GUEST_PING_INTERVAL: Duration = Duration::from_secs(5);

/// TPM VM을 돌리는 노드가 TPM 상태 파일의 해시를 다시 보고 펌웨어 사본을 밀어 두는 간격(계획 59 §5)
pub const FIRMWARE_SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// 게스트 에이전트에서 게스트 정보를 모으는 간격(계획 59 §8)
pub const GUEST_INFO_INTERVAL: Duration = Duration::from_secs(60);

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
    /// 게스트 감시(`ha.guest_watch`)가 게스트 에이전트에 `guest-ping`을 보내는 간격
    pub guest_ping_interval: Duration,
    /// 다른 노드에 있는 UEFI 변수 파일을 받아 온다. 없으면 받지 않고 이 노드의 파일로 켠다. TPM VM(계획 59)의 TPM
    /// 상태도 받아 오고 사본을 밀어 둔다. 없으면 다른 노드의 TPM 상태가 필요한 VM은 켜지 않는다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
    /// 드라이버의 `state_dir`(TPM 상태 파일이 있는 곳). 없으면 TPM 상태의 해시를 셈하지 않는다.
    pub state_dir: Option<PathBuf>,
    /// TPM 상태의 해시를 다시 보고 사본을 밀어 두는 간격
    pub firmware_sync_interval: Duration,
    /// 게스트 정보를 둘 곳(opld의 캐시). 없으면 모으지 않는다(에이전트 종료를 고르려고 답했는지만 본다).
    pub guest_info: Option<Arc<dyn GuestInfoSink>>,
    /// 게스트 에이전트에서 게스트 정보를 모으는 간격
    pub guest_info_interval: Duration,
    /// 이 노드의 HA 리스와 watchdog. None이면 펜싱이 꺼져 있어 HA VM을 켜지 않는다.
    pub ha: Option<Arc<HaGuard>>,
    /// 이 노드의 방화벽. None이면 방화벽이 없어 보안 그룹을 건 NIC가 있는 VM을 켜지 않는다.
    pub firewall: Option<Arc<dyn FirewallGate>>,
    /// 복제본 기록 파일의 디렉터리([`crate::replicas`], opld의 `{data_dir}/replicas`)
    pub replica_dir: PathBuf,
    /// 이 노드의 할당기(계획 64): 켤 때 pCPU·호스트 노드·hugepages를 고른다. 마이그레이션 받는 쪽과 나눈다.
    pub host_alloc: Arc<HostAllocator>,
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
    /// 복제본 기록 디렉터리는 마운트 디렉터리 옆의 `replicas`다(opld는 `{data_dir}/pools` 옆).
    pub fn new(node: Uuid, mount_root: impl Into<PathBuf>, storage: Arc<dyn Storage>) -> Self {
        let mount_root = mount_root.into();
        Self {
            node,
            replica_dir: mount_root.with_file_name("replicas"),
            mount_root,
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
            guest_ping_interval: GUEST_PING_INTERVAL,
            firmware: None,
            state_dir: None,
            firmware_sync_interval: FIRMWARE_SYNC_INTERVAL,
            guest_info: None,
            guest_info_interval: GUEST_INFO_INTERVAL,
            ha: None,
            firewall: None,
            host_alloc: Arc::new(HostAllocator::new(node, None)),
        }
    }
}
