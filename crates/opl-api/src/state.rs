use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::Cluster;
use opl_driver::Hypervisor;
use opl_peer::{MetricsSource, Peers};
use opl_reconciler::HaGuard;
use opl_types::security_group::Cidr;
use uuid::Uuid;

use crate::audit::AuditLog;
use crate::auth::{Authorizer, RoleAuthorizer, Sessions};
use crate::health::RuntimeProbe;
use crate::limit::AttemptLimiter;
use crate::oidc::OidcCache;
use crate::placement_board::PlacementBoard;
use crate::power_fence::{Fencer, PowerFence};
use crate::vm_imports::ImportCredentials;

/// 모든 핸들러가 공유하는 노드의 상태.
pub struct AppState {
    pub cluster: Arc<Cluster>,
    /// 이 노드의 id
    pub node: Uuid,
    pub hv: Arc<dyn Hypervisor>,
    pub authorizer: Arc<dyn Authorizer>,
    /// 올린 이미지를 가져오기 전에 잠시 두는 디렉터리. 볼륨 풀과 같은 파일시스템이면 좋다.
    pub upload_dir: PathBuf,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 다른 클러스터가 보내는 백업 파일을 받을
    /// 때 백업 풀의 경로를 정한다(`TaskRunnerConfig::mount_root`와 같다).
    pub mount_root: PathBuf,
    /// 다른 노드에서 도는 VM의 콘솔을 중계할 때 쓴다. 없으면 이 노드의 VM 콘솔만 연다.
    pub peers: Option<Arc<Peers>>,
    /// 세션 쿠키에 `Secure`를 붙인다(HTTPS로 서비스할 때).
    pub secure_cookies: bool,
    /// 믿는 프록시(opld `[api] trusted_proxies`). 이 주소에서 온 요청만 `X-Forwarded-For`·`-Host`·
    /// `-Proto`를 믿는다.
    pub trusted_proxies: Vec<Cidr>,
    pub(crate) sessions: Sessions,
    /// 로그인과 현재 비밀번호 확인의 실패 기록(노드마다 따로)
    pub(crate) attempts: AttemptLimiter,
    /// 이 노드에서 VM spec의 검증과 쓰기를 한 번에 하나씩 한다(MAC·볼륨 중복 확인이 경합하지 않게).
    /// 여러 노드에 걸친 경합은 나중에 클러스터 수준의 확인이 필요하다.
    pub(crate) vm_lock: tokio::sync::Mutex<()>,
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명(메모리에만). 같은 노드의 Task 실행기와 나눠 쓴다.
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 감사 로그. 기본은 꺼져 있다(opld가 연다).
    pub audit: Arc<AuditLog>,
    /// 이 노드의 지표(opld의 측정기 저장소). 없으면 지표 API가 이 노드를 `unsupported`로 보인다.
    pub metrics: Option<Arc<dyn MetricsSource>>,
    /// OIDC 영역의 discovery·JWKS(노드 메모리)
    pub(crate) oidc: OidcCache,
    /// 이 노드가 지금 받고 있는 백업 사본 파일(백업 id, 파일 이름). 같은 파일을 두 요청이 함께 쓰지 않게 한다.
    pub(crate) receiving_files: std::sync::Mutex<std::collections::HashSet<(Uuid, String)>>,
    /// 받는 백업 사본 파일의 본문 조각을 기다리는 시간(기본 60초). 넘으면 그 업로드를 끊는다.
    pub receive_idle_timeout: Duration,
    /// 이 노드의 HA 리스(펜싱이 꺼졌으면 없다). 헬스 체크가 본다.
    pub ha: Option<Arc<HaGuard>>,
    /// 실행기 확인(opld의 `Liveness`). 없으면 헬스 체크가 실행기를 보지 않는다.
    pub runtimes: Option<Arc<dyn RuntimeProbe>>,
    /// 노드 전원 끄기 펜싱의 실행기(`fence-test`, 리더의 끄기). 기본은 PATH의 `ipmitool`을 쓰는 [`Fencer`]다.
    pub fence: Arc<dyn PowerFence>,
    /// 균형 조정기의 마지막 결과(리더일 때). 노드 사이 RPC(`ReadPlacement`)의 답과 같은 것을 쓴다.
    pub placement: Arc<PlacementBoard>,
    /// 리더에 점수용 부하를 묻지 못했을 때 다시 묻지 않는 끝(`score::scorer`)
    pub(crate) placement_backoff: std::sync::Mutex<Option<tokio::time::Instant>>,
}

impl AppState {
    pub fn new(
        cluster: Arc<Cluster>,
        node: Uuid,
        hv: Arc<dyn Hypervisor>,
        upload_dir: PathBuf,
    ) -> Self {
        Self {
            cluster,
            node,
            hv,
            authorizer: Arc::new(RoleAuthorizer),
            upload_dir,
            mount_root: PathBuf::new(),
            peers: None,
            secure_cookies: false,
            trusted_proxies: Vec::new(),
            sessions: Sessions::default(),
            attempts: AttemptLimiter::default(),
            vm_lock: tokio::sync::Mutex::new(()),
            import_credentials: Arc::default(),
            audit: Arc::new(AuditLog::disabled()),
            metrics: None,
            oidc: OidcCache::default(),
            receiving_files: Default::default(),
            receive_idle_timeout: Duration::from_secs(60),
            ha: None,
            runtimes: None,
            fence: Arc::new(Fencer::default()),
            placement: Arc::default(),
            placement_backoff: Default::default(),
        }
    }
}
