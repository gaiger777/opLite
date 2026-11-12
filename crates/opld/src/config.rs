use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_api::{
    AuditConfig, BackupJobSchedulerConfig, ClusterJanitorConfig, RemotePollerConfig,
    SnapshotSchedulerConfig, TelemetryConfig, UpdatesConfig, VipAgentConfig,
};
use opl_driver::VipAddress;
use opl_qemu::{Accel, HostProfile, kvm_available};
use opl_reconciler::HaConfig;
use opl_reconciler::ha::{DeviceWatchdog, WatchdogOpener};
use opl_types::node::{Fencing, NodeHa};
use opl_types::security_group::Cidr;
use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// 다른 노드와 클라이언트가 이 노드에 닿는 호스트(IP나 DNS 이름). 노드 인증서에 들어가고,
    /// 클러스터에 알리는 주소(`address:포트`)가 된다. 나중에 바꿀 수 없다.
    pub address: String,
    /// 위트니스 노드: VM을 돌리지 않고 선거에 나서지 않는 Raft 투표 멤버. 합류만 할 수 있고, HTTPS
    /// API를 열지 않으며 `[hypervisor]`·`[pools]`·`[migration]`을 쓰지 않는다(QEMU가 없어도 된다).
    #[serde(default)]
    pub witness: bool,
    /// API(HTTPS)를 받을 소켓 주소
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// 노드 간 통신(gRPC over mTLS)을 받을 소켓 주소
    #[serde(default = "default_cluster_listen")]
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
    /// opld를 실행한 사용자만 쓸 수 있다(0600).
    pub control_socket: Option<PathBuf>,
    /// 클러스터 저장소, 노드 인증서, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리(macOS). Linux는 Debian 경로를 쓴다(`[hypervisor]`로 바꾼다).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub hypervisor: HypervisorSection,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
    #[serde(default)]
    pub schedule: ScheduleSection,
    #[serde(default)]
    pub janitor: JanitorSection,
    #[serde(default)]
    pub backup: BackupSection,
    #[serde(default)]
    pub pools: PoolsSection,
    #[serde(default)]
    pub migration: MigrationSection,
    #[serde(default)]
    pub ha: HaSection,
    #[serde(default)]
    pub network: NetworkSection,
    #[serde(default)]
    pub updates: UpdatesSection,
    #[serde(default)]
    pub telemetry: TelemetrySection,
    #[serde(default)]
    pub audit: AuditSection,
    #[serde(default)]
    pub remotes: RemotesSection,
    #[serde(default)]
    pub runtime: RuntimeSection,
    #[serde(default)]
    pub api: ApiSection,
    #[serde(default)]
    pub vip: VipSection,
    #[serde(default)]
    pub web: WebSection,
    #[serde(default)]
    pub node: NodeSection,
    /// 시험 빌드(`starve-it`)에만 있다. 배포 빌드는 `[test]`를 모르는 키로 거부한다.
    #[cfg(feature = "starve-it")]
    #[serde(default)]
    pub test: TestSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HypervisorSection {
    /// `auto`(macOS는 hvf, Linux는 KVM이 있으면 kvm, 없으면 tcg), `kvm`, `tcg`, `hvf`
    #[serde(default)]
    pub accel: AccelSetting,
    /// QEMU 실행 파일. 기본은 호스트에 맞는 `qemu-system-*`이다.
    pub qemu_binary: Option<PathBuf>,
    /// UEFI 펌웨어 코드와 VM마다 복사하는 변수 저장소 원본
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
    /// QEMU를 `systemd-run --scope`로 감싼다(VM마다 cgroup). Linux에서 기본으로 켠다.
    #[serde(default = "default_systemd_scope")]
    pub systemd_scope: bool,
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            accel: AccelSetting::default(),
            qemu_binary: None,
            uefi_code: None,
            uefi_vars: None,
            systemd_scope: default_systemd_scope(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccelSetting {
    #[default]
    Auto,
    Kvm,
    Tcg,
    Hvf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilerSection {
    /// ACPI 종료를 요청한 뒤 강제 종료하기까지 기다리는 시간(초)
    #[serde(default = "default_acpi_timeout")]
    pub acpi_timeout_secs: u64,
}

impl Default for ReconcilerSection {
    fn default() -> Self {
        Self {
            acpi_timeout_secs: default_acpi_timeout(),
        }
    }
}

impl ReconcilerSection {
    pub fn acpi_timeout(&self) -> Duration {
        Duration::from_secs(self.acpi_timeout_secs)
    }
}

/// 예약 스냅샷 작업. 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    /// 예약을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 주기의 한 시간(초). 매일은 24배, 매주는 168배다.
    #[serde(default = "default_hour_secs")]
    pub hour_secs: u64,
}

impl Default for ScheduleSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            hour_secs: default_hour_secs(),
        }
    }
}

impl ScheduleSection {
    pub fn scheduler_config(&self) -> SnapshotSchedulerConfig {
        SnapshotSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            hour: Duration::from_secs(self.hour_secs),
        }
    }
}

/// 클러스터 정리 작업. 간격·유예는 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JanitorSection {
    /// 목록을 확인하는 간격(초)
    #[serde(default = "default_janitor_tick_secs")]
    pub tick_secs: u64,
    /// 오프라인 노드의 `pending` Task를 실패로 끝내기까지 기다리는 시간(초)
    #[serde(default = "default_offline_grace_secs")]
    pub offline_grace_secs: u64,
    /// 만든 지 이 날수가 넘은 끝난 Task를 지운다.
    #[serde(default = "default_task_max_age_days")]
    pub task_max_age_days: u64,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    #[serde(default = "default_task_max_finished")]
    pub task_max_finished: usize,
}

impl Default for JanitorSection {
    fn default() -> Self {
        Self {
            tick_secs: default_janitor_tick_secs(),
            offline_grace_secs: default_offline_grace_secs(),
            task_max_age_days: default_task_max_age_days(),
            task_max_finished: default_task_max_finished(),
        }
    }
}

impl JanitorSection {
    pub fn janitor_config(&self) -> ClusterJanitorConfig {
        ClusterJanitorConfig {
            tick: Duration::from_secs(self.tick_secs),
            offline_grace: Duration::from_secs(self.offline_grace_secs),
            task_max_age: Duration::from_secs(self.task_max_age_days * 24 * 3600),
            task_max_finished: self.task_max_finished,
            // 고아 스냅샷·끝난 Task의 유예는 설정하지 않는다(기본 30초·10분).
            ..ClusterJanitorConfig::default()
        }
    }
}

/// 백업 작업 스케줄러(리더)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSection {
    /// 작업을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 노드마다 동시에 도는 백업 Task의 최대 수
    #[serde(default = "default_backup_max_concurrent")]
    pub max_concurrent: usize,
}

impl Default for BackupSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            max_concurrent: default_backup_max_concurrent(),
        }
    }
}

impl BackupSection {
    pub fn scheduler_config(&self) -> BackupJobSchedulerConfig {
        BackupJobSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            max_concurrent: self.max_concurrent,
        }
    }
}

/// 노드별 풀 상태 새로 고침(NFS 마운트 포함)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolsSection {
    /// 풀 상태를 확인하는 간격(초)
    #[serde(default = "default_pools_refresh_secs")]
    pub refresh_secs: u64,
}

impl Default for PoolsSection {
    fn default() -> Self {
        Self {
            refresh_secs: default_pools_refresh_secs(),
        }
    }
}

impl PoolsSection {
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_secs)
    }
}

/// 라이브 마이그레이션(설계 §4)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSection {
    /// 받는 쪽 QEMU가 마이그레이션을 받는 포트 범위(`"A-B"`, 둘 다 포함). 노드 사이 방화벽에서
    /// 열어야 한다.
    #[serde(
        default = "default_migration_ports",
        deserialize_with = "deserialize_port_range"
    )]
    pub ports: RangeInclusive<u16>,
    /// 보내는 대역폭 상한(MiB/s). 0이면 QEMU 기본값이다.
    #[serde(default)]
    pub bandwidth_mib: u64,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다(`"30m"`, `"1h30m"`, `"90s"`).
    #[serde(
        default = "default_migration_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub timeout: Duration,
    /// 노드 로컬 풀의 디스크를 복사하는 마이그레이션에서 디스크 복사가 따라잡기까지의 한도(`"12h"`).
    /// 메모리 단계는 `timeout`이다.
    #[serde(
        default = "default_migration_disk_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub disk_timeout: Duration,
}

impl Default for MigrationSection {
    fn default() -> Self {
        Self {
            ports: default_migration_ports(),
            bandwidth_mib: 0,
            timeout: default_migration_timeout(),
            disk_timeout: default_migration_disk_timeout(),
        }
    }
}

/// HA 리스와 자기 펜싱(설계 §3.1). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaSection {
    /// `watchdog`(opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작), `process`(opld가
    /// 끄는 것만 — 시험·개발용), `off`(이 노드는 HA VM을 돌리지 않는다). 기본은 Linux `watchdog`,
    /// macOS `off`다.
    #[serde(default)]
    pub fencing: FencingSetting,
    /// Linux watchdog 장치(하드웨어 또는 `softdog`)
    #[serde(default = "default_watchdog_device")]
    pub watchdog_device: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    #[serde(default = "default_watchdog_timeout_secs")]
    pub watchdog_timeout_secs: u64,
    /// 마지막으로 성공한 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(초).
    #[serde(default = "default_fence_after_secs")]
    pub fence_after_secs: u64,
}

impl Default for HaSection {
    fn default() -> Self {
        Self {
            fencing: FencingSetting::default(),
            watchdog_device: default_watchdog_device(),
            watchdog_timeout_secs: default_watchdog_timeout_secs(),
            fence_after_secs: default_fence_after_secs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FencingSetting {
    Watchdog,
    Process,
    Off,
}

impl Default for FencingSetting {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Self::Watchdog
        } else {
            Self::Off
        }
    }
}

/// `watchdog_timeout_secs`의 하한. 1초마다 쓰다듬으므로 한두 번 늦어도 재시작되지 않게 둔다.
const MIN_WATCHDOG_TIMEOUT_SECS: u64 = 5;
/// `watchdog_timeout_secs`의 상한(1시간)
const MAX_WATCHDOG_TIMEOUT_SECS: u64 = 3600;
/// 리더가 옮기기 전에 더 기다리는 여유(초)
const RECOVER_MARGIN_SECS: u64 = 5;

impl HaSection {
    /// 등록할 때 쓰는 이 노드의 HA 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn node_ha(&self) -> Option<NodeHa> {
        let (fencing, watchdog) = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => (Fencing::Watchdog, self.watchdog_timeout_secs),
            FencingSetting::Process => (Fencing::Process, 0),
        };
        Some(NodeHa {
            fencing,
            recover_after_secs: self
                .fence_after_secs
                .saturating_add(watchdog)
                .saturating_add(RECOVER_MARGIN_SECS),
        })
    }
}

impl HaSection {
    pub fn fence_after(&self) -> Duration {
        Duration::from_secs(self.fence_after_secs)
    }

    /// 리스 갱신 간격: `fence_after`의 10분의 1을 0.2초 이상 2초 이하로 자른 값
    pub fn renew_interval(&self) -> Duration {
        (self.fence_after() / 10).clamp(Duration::from_millis(200), Duration::from_secs(2))
    }

    /// 노드의 `HaGuard` 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn guard_config(&self) -> Option<HaConfig> {
        let watchdog: Option<Arc<dyn WatchdogOpener>> = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => Some(Arc::new(DeviceWatchdog {
                path: self.watchdog_device.clone(),
                timeout_secs: self.watchdog_timeout_secs,
            })),
            FencingSetting::Process => None,
        };
        Some(HaConfig {
            fence_after: self.fence_after(),
            renew_interval: self.renew_interval(),
            watchdog,
        })
    }
}

/// 노드 네트워크(VXLAN). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// VXLAN 패킷을 주고받을 이 노드의 주소. 없으면 `address`가 IP면 그것, 이름이면 시작할 때 푼
    /// 주소(loopback·link-local은 빼고 IPv4 먼저)다.
    #[serde(default)]
    pub underlay: Option<IpAddr>,
}

/// 노드의 APT 업데이트(소스 파일, 업데이트 확인). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesSection {
    /// 업데이트를 확인한다. 없으면 `apt_get`이 있을 때(Debian) 켜진다.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 업데이트를 확인하는 간격
    #[serde(
        default = "default_check_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    #[serde(
        default = "default_first_check",
        deserialize_with = "deserialize_duration"
    )]
    pub first_check: Duration,
    /// Orbivisor APT 저장소 주소. 비면 빌드 값(`OPL_APT_URL`), 그것도 비면 소스 파일을 쓰지 않는다.
    #[serde(default)]
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    #[serde(default = "default_sources_file")]
    pub sources_file: PathBuf,
    #[serde(default = "default_apt_get")]
    pub apt_get: PathBuf,
    #[serde(default = "default_systemd_run")]
    pub systemd_run: PathBuf,
    #[serde(default = "default_systemctl")]
    pub systemctl: PathBuf,
    #[serde(default = "default_dpkg")]
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    #[serde(
        default = "default_upgrade_poll",
        deserialize_with = "deserialize_duration"
    )]
    pub poll: Duration,
}

impl Default for UpdatesSection {
    fn default() -> Self {
        Self {
            enabled: None,
            check_interval: default_check_interval(),
            first_check: default_first_check(),
            apt_url: String::new(),
            sources_file: default_sources_file(),
            apt_get: default_apt_get(),
            systemd_run: default_systemd_run(),
            systemctl: default_systemctl(),
            dpkg: default_dpkg(),
            log_dir: default_log_dir(),
            poll: default_upgrade_poll(),
        }
    }
}

impl UpdatesSection {
    /// 업데이트를 확인하는지: `enabled`, 없으면 `apt_get`이 있는지
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or_else(|| self.apt_get.exists())
    }

    /// API의 업데이트 설정. 꺼졌으면 None이다.
    pub fn updates_config(&self) -> Option<UpdatesConfig> {
        if !self.enabled() {
            return None;
        }
        let apt_url = if self.apt_url.is_empty() {
            opl_types::version::APT_URL.to_string()
        } else {
            self.apt_url.clone()
        };
        Some(UpdatesConfig {
            check_interval: self.check_interval,
            first_check: self.first_check,
            apt_url,
            sources_file: self.sources_file.clone(),
            keyring: opl_api::KEYRING.into(),
            auth_file: opl_api::AUTH_FILE.into(),
            apt_get: self.apt_get.clone(),
            systemd_run: self.systemd_run.clone(),
            systemctl: self.systemctl.clone(),
            dpkg: self.dpkg.clone(),
            log_dir: self.log_dir.clone(),
            poll: self.poll,
        })
    }
}

/// 텔레메트리 리포터(리더). 켜고 끄기와 끝점은 클러스터 설정(`PATCH /settings`)이 정한다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySection {
    /// 보낼 때가 됐는지 확인하는 간격
    #[serde(
        default = "default_telemetry_tick",
        deserialize_with = "deserialize_duration"
    )]
    pub tick: Duration,
    /// 보고를 보내는 간격
    #[serde(
        default = "default_telemetry_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Duration,
    /// 보내지 못한 뒤 다시 보내기까지 기다리는 시간
    #[serde(
        default = "default_telemetry_retry",
        deserialize_with = "deserialize_duration"
    )]
    pub retry: Duration,
    /// 시스템 루트에 더해 믿는 CA 인증서(PEM, 여럿이어도 된다). 사설 수집 서버에 쓴다.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
}

impl Default for TelemetrySection {
    fn default() -> Self {
        Self {
            tick: default_telemetry_tick(),
            interval: default_telemetry_interval(),
            retry: default_telemetry_retry(),
            ca_file: None,
        }
    }
}

impl TelemetrySection {
    /// API의 리포터 설정. `ca_file`을 읽지 못하거나 인증서가 없으면 오류다.
    pub fn telemetry_config(&self) -> anyhow::Result<TelemetryConfig> {
        let mut extra_roots = Vec::new();
        if let Some(path) = &self.ca_file {
            let pem = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
            let certs = opl_peer::pki::certs(&pem)
                .map_err(|e| anyhow::anyhow!("telemetry.ca_file {}: {e}", path.display()))?;
            extra_roots = certs.into_iter().map(|c| c.to_vec()).collect();
        }
        Ok(TelemetryConfig {
            tick: self.tick,
            interval: self.interval,
            retry: self.retry,
            extra_roots,
            ..TelemetryConfig::default()
        })
    }
}

/// 원격 클러스터 상태 수집(리더). 원격은 API(`/remotes`)로 등록한다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemotesSection {
    /// 원격마다 상태를 모으는 간격
    #[serde(
        default = "default_remotes_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub interval: Duration,
    /// 모을 때가 됐는지 확인하는 간격
    #[serde(
        default = "default_remotes_tick",
        deserialize_with = "deserialize_duration"
    )]
    pub tick: Duration,
}

impl Default for RemotesSection {
    fn default() -> Self {
        Self {
            interval: default_remotes_interval(),
            tick: default_remotes_tick(),
        }
    }
}

impl RemotesSection {
    pub fn poller_config(&self) -> RemotePollerConfig {
        RemotePollerConfig {
            tick: self.tick,
            interval: self.interval,
        }
    }
}

fn default_remotes_interval() -> Duration {
    Duration::from_secs(30)
}
fn default_remotes_tick() -> Duration {
    Duration::from_secs(5)
}

/// API(HTTPS)를 앞단 프록시 뒤에 둘 때. 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiSection {
    /// 믿는 프록시의 주소 범위(`"10.0.0.0/8"`, `"fd00::/8"`, `"192.0.2.10"`). 이 주소에서 온 요청만
    /// `X-Forwarded-For`(감사 로그·로그인 제한의 요청 주소)와 `X-Forwarded-Host`·`X-Forwarded-Proto`(같은
    /// 출처 검사, OIDC 콜백 주소)를 믿는다. 비어 있으면 아무 헤더도 믿지 않는다.
    #[serde(default)]
    pub trusted_proxies: Vec<Cidr>,
    /// 관리자가 준 API 인증서(PEM, 중간 인증서를 이어 붙여도 됨)와 키 파일. 둘 다 있어야 하고, 있으면 API
    /// 리스너가 이것을 쓰며 클러스터 CA로 API 인증서를 다시 발급하지 않는다(`api_names`를 따르지 않음).
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

impl ApiSection {
    /// 관리자가 준 API 인증서와 키(PEM). 없으면 None, 읽지 못하면 오류다.
    pub fn read_tls(&self) -> anyhow::Result<Option<(String, String)>> {
        let (Some(cert), Some(key)) = (&self.tls_cert, &self.tls_key) else {
            return Ok(None);
        };
        let read = |path: &PathBuf| {
            std::fs::read_to_string(path).map_err(|e| {
                anyhow::anyhow!("cannot read the API TLS file {}: {e}", path.display())
            })
        };
        Ok(Some((read(cert)?, read(key)?)))
    }
}

/// 내장 VIP(설계 §4, Linux만). 이 노드가 가질 수 있는 VIP다. 리더가 VIP마다 헬스 체크가 200인 노드 하나를
/// 골라 주고, 그 노드가 `interface`에 붙인다. 모든 API 노드에 같게 둔다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VipSection {
    /// VIP(`"192.168.0.200/24"`, IPv4). 접두사는 인터페이스에 붙일 때 쓴다(없으면 `/32`).
    #[serde(default)]
    pub addresses: Vec<VipAddress>,
    /// VIP를 붙일 인터페이스(예: `eth0`, `vmbr0`)
    pub interface: Option<String>,
}

impl VipSection {
    /// VIP 에이전트 설정. VIP가 없으면 None이다.
    pub fn agent_config(&self) -> Option<VipAgentConfig> {
        let interface = self.interface.as_ref()?;
        (!self.addresses.is_empty())
            .then(|| VipAgentConfig::new(interface.clone(), self.addresses.clone()))
    }
}

/// 내장 웹 UI. 끄면 API(`/api/`)만 열고 UI는 따로 둔 포털(`packaging/portal/`)이 준다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    #[serde(default = "default_web_enabled")]
    pub enabled: bool,
}

impl Default for WebSection {
    fn default() -> Self {
        Self {
            enabled: default_web_enabled(),
        }
    }
}

fn default_web_enabled() -> bool {
    true
}

/// 이 노드에 대한 것
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSection {
    /// 장애 영역(랙·전원·방 이름). 등록할 때 노드 레이블 `zone`으로 적고, 리더가 투표 멤버를 고를 때 서로 다른
    /// zone을 먼저 고른다. 앞뒤 공백은 잘라 적는다. 없으면 레이블을 건드리지 않는다(API로 둔 레이블은 그대로).
    pub zone: Option<String>,
}

/// 실행기 크기(설계 §3.1). `core` 실행기는 2로 고정이다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    /// API(axum) 실행기의 작업자 스레드 수. 기본은 CPU 수의 절반(최소 2)이다.
    pub api_threads: Option<usize>,
    /// reconciler·Task 실행기·스케줄러 등의 실행기의 작업자 스레드 수. 기본은 CPU 수의 절반(최소 2)이다.
    pub work_threads: Option<usize>,
}

impl RuntimeSection {
    pub fn api_threads(&self) -> usize {
        self.api_threads.unwrap_or_else(default_runtime_threads)
    }

    pub fn work_threads(&self) -> usize {
        self.work_threads.unwrap_or_else(default_runtime_threads)
    }
}

/// CPU 수의 절반, 최소 2
fn default_runtime_threads() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cpus / 2).max(2)
}

/// 굶김 시험의 고리(설계 §3.4, 시험 빌드에만)
#[cfg(feature = "starve-it")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSection {
    /// 제어 소켓의 `POST /v1/test/block-api`가 `api` 실행기의 작업자를 모두 막는 시간(밀리초)
    #[serde(default)]
    pub block_api_ms: u64,
}

#[cfg(feature = "starve-it")]
impl TestSection {
    pub fn block_api(&self) -> Duration {
        Duration::from_millis(self.block_api_ms)
    }
}

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditSection {
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    /// 한 파일의 최대 크기(MiB). 넘으면 회전한다.
    #[serde(default = "default_audit_max_file_mib")]
    pub max_file_mib: u64,
    /// 지금 파일을 포함해 남기는 파일 수
    #[serde(default = "default_audit_max_files")]
    pub max_files: usize,
}

impl Default for AuditSection {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            max_file_mib: default_audit_max_file_mib(),
            max_files: default_audit_max_files(),
        }
    }
}

impl AuditSection {
    /// API의 감사 로그 설정. 꺼졌으면 None이다.
    pub fn audit_config(&self, data_dir: &Path, node: &str) -> Option<AuditConfig> {
        self.enabled.then(|| AuditConfig {
            dir: data_dir.join("audit"),
            node: node.into(),
            max_file_size: self.max_file_mib << 20,
            max_files: self.max_files,
        })
    }
}

fn default_audit_enabled() -> bool {
    true
}
fn default_audit_max_file_mib() -> u64 {
    16
}
fn default_audit_max_files() -> usize {
    8
}

fn default_telemetry_tick() -> Duration {
    Duration::from_secs(60)
}
fn default_telemetry_interval() -> Duration {
    Duration::from_secs(24 * 3600)
}
fn default_telemetry_retry() -> Duration {
    Duration::from_secs(3600)
}

fn default_check_interval() -> Duration {
    Duration::from_secs(24 * 3600)
}
fn default_first_check() -> Duration {
    Duration::from_secs(60)
}
fn default_sources_file() -> PathBuf {
    // 이미 쓴 소스 파일을 이어 관리하도록 제품 이름이 바뀌어도 그대로 둔다.
    "/etc/apt/sources.list.d/oplite.sources".into()
}
fn default_apt_get() -> PathBuf {
    "/usr/bin/apt-get".into()
}
fn default_systemd_run() -> PathBuf {
    "/usr/bin/systemd-run".into()
}
fn default_systemctl() -> PathBuf {
    "/usr/bin/systemctl".into()
}
fn default_dpkg() -> PathBuf {
    "/usr/bin/dpkg".into()
}
fn default_log_dir() -> PathBuf {
    "/var/log/opl".into()
}
fn default_upgrade_poll() -> Duration {
    Duration::from_secs(2)
}

/// 노드 underlay를 정하는 방법
#[derive(Debug, PartialEq, Eq)]
pub enum UnderlaySource<'a> {
    /// 설정했거나 `address`가 IP 리터럴이다.
    Ip(IpAddr),
    /// `address`가 이름이라 풀어야 한다.
    Lookup(&'a str),
}

fn default_watchdog_device() -> PathBuf {
    "/dev/watchdog".into()
}
fn default_watchdog_timeout_secs() -> u64 {
    30
}
fn default_fence_after_secs() -> u64 {
    30
}

fn default_migration_ports() -> RangeInclusive<u16> {
    60000..=60099
}
fn default_migration_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}
fn default_migration_disk_timeout() -> Duration {
    Duration::from_secs(12 * 3600)
}

/// `"A-B"`: 0이 아닌 포트 A부터 B까지(A ≤ B)
fn parse_port_range(s: &str) -> Result<RangeInclusive<u16>, String> {
    let err =
        || format!("invalid port range {s:?} (expected \"FIRST-LAST\", e.g. \"60000-60099\")");
    let (a, b) = s.split_once('-').ok_or_else(err)?;
    let a: u16 = a.trim().parse().map_err(|_| err())?;
    let b: u16 = b.trim().parse().map_err(|_| err())?;
    if a == 0 || a > b {
        return Err(err());
    }
    Ok(a..=b)
}

/// 숫자와 단위(`h`, `m`, `s`)를 이어 쓴 시간: `"30m"`, `"1h30m"`. 0보다 커야 한다.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let err = || format!("invalid duration {s:?} (e.g. \"30m\", \"1h30m\", \"90s\")");
    let mut rest = s.trim();
    let mut secs: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
        let n: u64 = rest[..digits].parse().map_err(|_| err())?;
        let unit = match rest.as_bytes()[digits] {
            b'h' => 3600,
            b'm' => 60,
            b's' => 1,
            _ => return Err(err()),
        };
        secs = n
            .checked_mul(unit)
            .and_then(|v| secs.checked_add(v))
            .ok_or_else(err)?;
        rest = &rest[digits + 1..];
    }
    if secs == 0 {
        return Err(err());
    }
    Ok(Duration::from_secs(secs))
}

fn deserialize_port_range<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<RangeInclusive<u16>, D::Error> {
    parse_port_range(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    parse_duration(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8006".parse().unwrap()
}
fn default_cluster_listen() -> SocketAddr {
    "0.0.0.0:8007".parse().unwrap()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/opl".into()
}
fn default_run_dir() -> PathBuf {
    "/run/opl".into()
}
fn default_qemu_share_dir() -> PathBuf {
    "/opt/homebrew/share/qemu".into()
}
fn default_systemd_scope() -> bool {
    cfg!(target_os = "linux")
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}
fn default_tick_secs() -> u64 {
    60
}
fn default_hour_secs() -> u64 {
    3600
}
fn default_backup_max_concurrent() -> usize {
    2
}
fn default_janitor_tick_secs() -> u64 {
    10
}
fn default_offline_grace_secs() -> u64 {
    60
}
/// `task_max_age_days`의 상한(100년)
const MAX_TASK_AGE_DAYS: u64 = 36500;
fn default_task_max_age_days() -> u64 {
    7
}
fn default_task_max_finished() -> usize {
    1000
}
fn default_pools_refresh_secs() -> u64 {
    15
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
fn with_port(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

impl Config {
    /// 설정 파일에서 `data_dir`만 읽는다(없으면 기본값). 나머지가 틀려도 된다(`--release-vips`).
    pub fn data_dir_of(path: &Path) -> anyhow::Result<PathBuf> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let table: toml::Table = toml::from_str(&text)?;
        match table.get("data_dir") {
            Some(toml::Value::String(dir)) => Ok(dir.into()),
            Some(_) => anyhow::bail!("data_dir must be a path"),
            None => Ok(default_data_dir()),
        }
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        opl_types::resource::validate_name(&self.name)?;
        let address = &self.address;
        let ok = !address.is_empty()
            && (address.parse::<IpAddr>().is_ok()
                || address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !ok {
            anyhow::bail!(
                "address must be an IP address or a host name without a port (got {address:?})"
            );
        }
        // 다른 노드가 닿을 주소이므로 0.0.0.0이나 ::은 안 된다.
        if address
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified())
        {
            anyhow::bail!("address must not be an unspecified address (got {address:?})");
        }
        for (key, addr) in [
            ("listen", self.listen),
            ("cluster_listen", self.cluster_listen),
        ] {
            if addr.port() == 0 {
                anyhow::bail!("{key} must have a port other than 0 (got {addr})");
            }
        }
        let linux = cfg!(target_os = "linux");
        match self.hypervisor.accel {
            AccelSetting::Kvm if !linux => anyhow::bail!("accel = \"kvm\" needs Linux"),
            AccelSetting::Hvf if linux => anyhow::bail!("accel = \"hvf\" needs macOS"),
            _ => {}
        }
        if self.hypervisor.systemd_scope && !linux {
            anyhow::bail!("systemd_scope needs Linux; set it to false");
        }
        if self.schedule.tick_secs == 0 || self.schedule.hour_secs == 0 {
            anyhow::bail!("schedule.tick_secs and schedule.hour_secs must be at least 1");
        }
        if self.janitor.tick_secs == 0 || self.janitor.offline_grace_secs == 0 {
            anyhow::bail!("janitor.tick_secs and janitor.offline_grace_secs must be at least 1");
        }
        // 0이면 끝난 Task를 곧바로 지워 Task를 기다리는 쪽이 결과를 못 본다.
        if self.janitor.task_max_age_days == 0 || self.janitor.task_max_finished == 0 {
            anyhow::bail!(
                "janitor.task_max_age_days and janitor.task_max_finished must be at least 1"
            );
        }
        // 날수를 초로 바꿀 때 넘치지 않도록 100년까지만 받는다.
        if self.janitor.task_max_age_days > MAX_TASK_AGE_DAYS {
            anyhow::bail!("janitor.task_max_age_days must be at most {MAX_TASK_AGE_DAYS}");
        }
        if self.backup.tick_secs == 0 || self.backup.max_concurrent == 0 {
            anyhow::bail!("backup.tick_secs and backup.max_concurrent must be at least 1");
        }
        if self.pools.refresh_secs == 0 {
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
        // 감사 로그를 읽을 때 파일을 통째로 메모리에 올리므로 256 MiB까지만 받는다.
        if self.audit.max_file_mib == 0
            || self.audit.max_files == 0
            || self.audit.max_file_mib > 256
        {
            anyhow::bail!(
                "audit.max_file_mib must be between 1 and 256 and audit.max_files at least 1"
            );
        }
        if self.runtime.api_threads == Some(0) || self.runtime.work_threads == Some(0) {
            anyhow::bail!("runtime.api_threads and runtime.work_threads must be at least 1");
        }
        if self.api.tls_cert.is_some() != self.api.tls_key.is_some() {
            anyhow::bail!("api.tls_cert and api.tls_key must be given together");
        }
        if self.ha.fence_after_secs == 0 {
            anyhow::bail!("ha.fence_after_secs must be at least 1");
        }
        if self.ha.fencing == FencingSetting::Watchdog {
            if !(MIN_WATCHDOG_TIMEOUT_SECS..=MAX_WATCHDOG_TIMEOUT_SECS)
                .contains(&self.ha.watchdog_timeout_secs)
            {
                anyhow::bail!(
                    "ha.watchdog_timeout_secs must be between {MIN_WATCHDOG_TIMEOUT_SECS} and {MAX_WATCHDOG_TIMEOUT_SECS}"
                );
            }
            if !linux {
                anyhow::bail!("ha.fencing = \"watchdog\" needs Linux; use \"process\" or \"off\"");
            }
        }
        // 다른 노드가 VXLAN 패킷을 보낼 주소이므로 0.0.0.0이나 ::은 안 된다.
        if let Some(ip) = self.network.underlay
            && ip.is_unspecified()
        {
            anyhow::bail!("network.underlay must not be an unspecified address (got {ip})");
        }
        self.validate_vip(linux)?;
        if self.node.zone.as_ref().is_some_and(|z| z.trim().is_empty()) {
            anyhow::bail!("node.zone must not be empty");
        }
        // 알리는 주소는 `address`에 포트만 붙이므로 포트가 같으면 둘을 구별할 수 없다.
        if self.listen.port() == self.cluster_listen.port() {
            anyhow::bail!(
                "listen and cluster_listen must use different ports (got {} for both)",
                self.listen.port()
            );
        }
        Ok(())
    }

    /// `[vip]`: Linux만, 위트니스는 안 됨, 인터페이스가 있어야 하고 같은 IP를 두 번 쓰지 않는다.
    fn validate_vip(&self, linux: bool) -> anyhow::Result<()> {
        let vip = &self.vip;
        if vip.addresses.is_empty() {
            return Ok(());
        }
        if !linux {
            anyhow::bail!("vip.addresses needs Linux");
        }
        if self.witness {
            anyhow::bail!("a witness has no API and cannot hold a VIP; remove [vip]");
        }
        let ok = vip.interface.as_deref().is_some_and(|i| {
            (1..=15).contains(&i.len())
                && i.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        });
        if !ok {
            anyhow::bail!("vip.interface must name the network interface for vip.addresses");
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in &vip.addresses {
            if !seen.insert(a.ip) {
                anyhow::bail!("vip.addresses lists {} more than once", a.ip);
            }
        }
        Ok(())
    }

    /// 이 호스트에서 QEMU를 실행하는 방법. `accel = "kvm"`인데 `/dev/kvm`을 열 수 없으면 오류다.
    pub fn host_profile(&self) -> anyhow::Result<HostProfile> {
        let h = &self.hypervisor;
        let mut profile = HostProfile::native(&self.qemu_share_dir);
        match h.accel {
            AccelSetting::Auto => {}
            AccelSetting::Kvm if !kvm_available() => {
                anyhow::bail!("accel = \"kvm\" but /dev/kvm cannot be opened")
            }
            AccelSetting::Kvm => profile.accel = Accel::Kvm,
            AccelSetting::Tcg => profile.accel = Accel::Tcg,
            AccelSetting::Hvf => profile.accel = Accel::Hvf,
        }
        if let Some(binary) = &h.qemu_binary {
            profile.qemu_binary = binary.clone();
        }
        if let Some(code) = &h.uefi_code {
            profile.uefi_code = code.clone();
        }
        if let Some(vars) = &h.uefi_vars {
            profile.uefi_vars_template = vars.clone();
        }
        Ok(profile)
    }

    /// 클러스터에 알리는 API 주소
    pub fn api_address(&self) -> String {
        with_port(&self.address, self.listen.port())
    }

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }

    /// 마이그레이션을 받는 QEMU가 들을 호스트: 노드 간 통신을 받는 IP(`cluster_listen`, 기본
    /// `0.0.0.0`). `address`는 원본이 접속하는 이름일 뿐 이 기계의 주소로 풀리지 않을 수 있다(예:
    /// `/etc/hosts`의 `127.0.1.1`).
    pub fn migration_listen_host(&self) -> String {
        self.cluster_listen.ip().to_string()
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
        }
        names
    }

    /// underlay를 정하는 방법: `[network] underlay`, 없으면 `address`
    pub fn underlay_source(&self) -> UnderlaySource<'_> {
        if let Some(ip) = self.network.underlay {
            return UnderlaySource::Ip(ip);
        }
        match self.address.parse::<IpAddr>() {
            Ok(ip) => UnderlaySource::Ip(ip),
            Err(_) => UnderlaySource::Lookup(&self.address),
        }
    }

    /// Node status에 기록할 underlay. 이름을 풀지 못하면 경고하고 None이다(이 노드에 VXLAN 장치를
    /// 만들지 않는다).
    pub async fn underlay(&self) -> Option<IpAddr> {
        self.underlay_with(|host| async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
        .await
    }

    /// `underlay`와 같되 이름 풀기(`lookup`)를 받는다(시험이 DNS에 기대지 않게).
    async fn underlay_with<F, Fut>(&self, lookup: F) -> Option<IpAddr>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = std::io::Result<Vec<IpAddr>>>,
    {
        let host = match self.underlay_source() {
            UnderlaySource::Ip(ip) => return Some(ip),
            UnderlaySource::Lookup(host) => host,
        };
        match lookup(host.to_string()).await {
            Ok(addrs) => match pick_underlay(addrs) {
                Some(ip) => Some(ip),
                None => {
                    tracing::warn!(
                        host,
                        "address has no usable IP (only loopback or link-local); set [network] underlay for VXLAN"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    host,
                    error = %e,
                    "cannot resolve address; set [network] underlay for VXLAN"
                );
                None
            }
        }
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.run_dir.join("opld.sock"))
    }
}

/// 이름을 푼 주소 가운데 underlay로 쓸 것. 다른 노드가 닿을 수 없는 loopback·link-local·미지정 주소는
/// 빼고(예: `/etc/hosts`의 `127.0.1.1`), IPv4를 먼저, 없으면 IPv6의 첫 주소다.
fn pick_underlay(addrs: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    let usable: Vec<IpAddr> = addrs
        .into_iter()
        .filter(|ip| {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && match ip {
                    IpAddr::V4(v4) => !v4.is_link_local(),
                    IpAddr::V6(v6) => !v6.is_unicast_link_local(),
                }
        })
        .collect();
    let v4 = usable.iter().find(|ip| ip.is_ipv4());
    v4.or(usable.first()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn fills_defaults() {
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert!(!c.witness);
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
        assert_eq!(c.api_address(), "10.0.0.5:8006");
        assert_eq!(c.cluster_address(), "10.0.0.5:8007");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.control_socket(), Path::new("/run/opl/opld.sock"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(60));
        assert_eq!(schedule.hour, Duration::from_secs(3600));
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(10));
        assert_eq!(janitor.offline_grace, Duration::from_secs(60));
        assert_eq!(janitor.orphan_grace, Duration::from_secs(30));
        assert_eq!(janitor.task_max_age, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 1000);
        assert_eq!(janitor.finished_grace, Duration::from_secs(600));
        let backup = c.backup.scheduler_config();
        assert_eq!(backup.tick, Duration::from_secs(60));
        assert_eq!(backup.max_concurrent, 2);
        assert_eq!(c.pools.refresh(), Duration::from_secs(15));
        assert_eq!(c.migration.ports, 60000..=60099);
        assert_eq!(c.migration.bandwidth_mib, 0);
        assert_eq!(c.migration.timeout, Duration::from_secs(30 * 60));
        assert_eq!(c.migration.disk_timeout, Duration::from_secs(12 * 3600));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
        assert_eq!(c.hypervisor.systemd_scope, cfg!(target_os = "linux"));
        assert_eq!(
            c.cert_names(),
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn witness_is_a_top_level_flag() {
        let c = parse(
            "name = \"w\"\naddress = \"h\"\nwitness = true\n[hypervisor]\nqemu_binary = \"/nonexistent/qemu\"",
        );
        assert!(c.witness);
        c.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_keys_and_missing_address() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\nlisten_addr = \"x\"").is_err()
        );
        assert!(toml::from_str::<Config>("name = \"n\"").is_err());
        let c = parse("name = \"n\"\naddress = \"h\"\n[reconciler]\nacpi_timeout_secs = 5");
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn validates_name_and_address() {
        let with = |name: &str, address: &str| Config {
            name: name.into(),
            address: address.into(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with("node1", "10.0.0.5").validate().is_ok());
        assert!(with("node1", "node1.lan").validate().is_ok());
        assert!(with("node1", "::1").validate().is_ok());
        assert!(with("Node_1", "10.0.0.5").validate().is_err());
        for bad in ["", "10.0.0.5:8006", "a b", "[::1]"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
        assert_eq!(with("n", "::1").api_address(), "[::1]:8006");
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn migration_listens_on_the_cluster_listen_ip() {
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(c.migration_listen_host(), "0.0.0.0");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"10.0.0.5:8007\"");
        assert_eq!(c.migration_listen_host(), "10.0.0.5");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"[::]:8007\"");
        assert_eq!(c.migration_listen_host(), "::");
    }

    #[test]
    fn validates_ports() {
        let ok = parse("name = \"n\"\naddress = \"h\"");
        assert!(ok.validate().is_ok());
        let same = Config {
            cluster_listen: "127.0.0.1:8006".parse().unwrap(),
            ..ok.clone()
        };
        assert!(same.validate().is_err());
        let zero = Config {
            listen: "0.0.0.0:0".parse().unwrap(),
            ..ok.clone()
        };
        assert!(zero.validate().is_err());
        let zero = Config {
            cluster_listen: "0.0.0.0:0".parse().unwrap(),
            ..ok
        };
        assert!(zero.validate().is_err());
    }

    #[test]
    fn schedule_section_shortens_periods_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[schedule]\ntick_secs = 1\nhour_secs = 2");
        assert!(c.validate().is_ok());
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(1));
        assert_eq!(schedule.hour, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "hour_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[schedule]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[schedule]\ntick = 1")
                .is_err()
        );
    }

    #[test]
    fn janitor_section_shortens_periods_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntick_secs = 1\noffline_grace_secs = 2",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(1));
        assert_eq!(janitor.offline_grace, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "offline_grace_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[janitor]\ngrace = 1")
                .is_err()
        );
    }

    #[test]
    fn janitor_section_sets_task_retention_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 2\ntask_max_finished = 50",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.task_max_age, Duration::from_secs(2 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 50);
        let c = parse("name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 36500");
        assert!(c.validate().is_ok());
        for bad in [
            "task_max_age_days = 0",
            "task_max_finished = 0",
            "task_max_age_days = 36501",
        ] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn backup_section_sets_the_tick_and_the_limit_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[backup]\ntick_secs = 1\nmax_concurrent = 4");
        assert!(c.validate().is_ok());
        let backup = c.backup.scheduler_config();
        assert_eq!(backup.tick, Duration::from_secs(1));
        assert_eq!(backup.max_concurrent, 4);
        for bad in ["tick_secs = 0", "max_concurrent = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[backup]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[backup]\ntick = 1").is_err()
        );
    }

    #[test]
    fn pools_section_sets_the_refresh_interval_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 2");
        assert!(c.validate().is_ok());
        assert_eq!(c.pools.refresh(), Duration::from_secs(2));
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 0");
        assert!(c.validate().is_err());
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh = 1")
                .is_err()
        );
    }

    #[test]
    fn updates_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        let u = &c.updates;
        assert_eq!(u.enabled, None);
        assert_eq!(u.check_interval, Duration::from_secs(24 * 3600));
        assert_eq!(u.first_check, Duration::from_secs(60));
        assert_eq!(u.apt_url, "");
        assert_eq!(
            u.sources_file,
            Path::new("/etc/apt/sources.list.d/oplite.sources")
        );
        assert_eq!(u.apt_get, Path::new("/usr/bin/apt-get"));
        assert_eq!(u.systemd_run, Path::new("/usr/bin/systemd-run"));
        assert_eq!(u.systemctl, Path::new("/usr/bin/systemctl"));
        assert_eq!(u.dpkg, Path::new("/usr/bin/dpkg"));
        assert_eq!(u.log_dir, Path::new("/var/log/opl"));
        assert_eq!(u.poll, Duration::from_secs(2));

        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[updates]\nenabled = true\ncheck_interval = \"6h\"\n\
             first_check = \"1s\"\napt_url = \"https://apt.example.com/orbivisor\"\n\
             sources_file = \"/tmp/x.sources\"\napt_get = \"/opt/apt-get\"\npoll = \"1m30s\"",
        );
        let config = c.updates.updates_config().unwrap();
        assert_eq!(config.check_interval, Duration::from_secs(6 * 3600));
        assert_eq!(config.first_check, Duration::from_secs(1));
        assert_eq!(config.apt_url, "https://apt.example.com/orbivisor");
        assert_eq!(config.sources_file, Path::new("/tmp/x.sources"));
        assert_eq!(
            config.auth_file,
            Path::new("/etc/apt/auth.conf.d/oplite.conf")
        );
        assert_eq!(config.apt_get, Path::new("/opt/apt-get"));
        assert_eq!(config.poll, Duration::from_secs(90));
        // 비어 있으면 빌드 값을 쓴다.
        let c = parse("name = \"n\"\naddress = \"h\"\n[updates]\nenabled = true");
        assert_eq!(
            c.updates.updates_config().unwrap().apt_url,
            opl_types::version::APT_URL
        );

        for bad in [
            "check_interval = \"0s\"",
            "first_check = \"soon\"",
            "poll = 2",
            "interval = \"1h\"",
        ] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[updates]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn updates_are_enabled_when_apt_get_exists_unless_set() {
        let dir = tempfile::tempdir().unwrap();
        let apt_get = dir.path().join("apt-get");
        let with = |extra: &str| {
            parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[updates]\napt_get = \"{}\"\n{extra}",
                apt_get.display()
            ))
        };
        assert!(!with("").updates.enabled());
        assert!(with("").updates.updates_config().is_none());
        assert!(with("enabled = true").updates.enabled());
        std::fs::write(&apt_get, "").unwrap();
        assert!(with("").updates.enabled());
        assert!(with("").updates.updates_config().is_some());
        assert!(!with("enabled = false").updates.enabled());
        assert!(with("enabled = false").updates.updates_config().is_none());
    }

    #[test]
    fn audit_section_defaults_and_rejects_zero() {
        let c = parse("name = \"n1\"\naddress = \"h\"\ndata_dir = \"/d\"");
        assert_eq!(c.audit, AuditSection::default());
        let a = c.audit.audit_config(&c.data_dir, &c.name).unwrap();
        assert_eq!(a.dir, Path::new("/d/audit"));
        assert_eq!(a.node, "n1");
        assert_eq!(a.max_file_size, 16 << 20);
        assert_eq!(a.max_files, 8);

        let c = parse("name = \"n\"\naddress = \"h\"\n[audit]\nmax_file_mib = 1\nmax_files = 2");
        c.validate().unwrap();
        // 읽을 때 파일을 통째로 메모리에 올리므로 256 MiB까지
        parse("name = \"n\"\naddress = \"h\"\n[audit]\nmax_file_mib = 256")
            .validate()
            .unwrap();
        let a = c.audit.audit_config(&c.data_dir, &c.name).unwrap();
        assert_eq!((a.max_file_size, a.max_files), (1 << 20, 2));
        let off = parse("name = \"n\"\naddress = \"h\"\n[audit]\nenabled = false");
        assert!(off.audit.audit_config(&off.data_dir, &off.name).is_none());

        for bad in ["max_file_mib = 0", "max_files = 0", "max_file_mib = 257"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[audit]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[audit]\ndir = \"/x\"")
                .is_err()
        );
    }

    #[test]
    fn runtime_section_defaults_to_half_the_cpus_and_rejects_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.runtime, RuntimeSection::default());
        let cpus = std::thread::available_parallelism().unwrap().get();
        assert_eq!(c.runtime.api_threads(), (cpus / 2).max(2));
        assert_eq!(c.runtime.work_threads(), (cpus / 2).max(2));

        let c =
            parse("name = \"n\"\naddress = \"h\"\n[runtime]\napi_threads = 1\nwork_threads = 6");
        c.validate().unwrap();
        assert_eq!((c.runtime.api_threads(), c.runtime.work_threads()), (1, 6));

        for bad in ["api_threads = 0", "work_threads = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[runtime]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        // core는 고정이다.
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[runtime]\ncore_threads = 4")
                .is_err()
        );
    }

    #[test]
    fn api_tls_files_go_together() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert!(c.api.read_tls().unwrap().is_none());
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("api.crt");
        let key = dir.path().join("api.key");
        std::fs::write(&cert, "CERT").unwrap();
        std::fs::write(&key, "KEY").unwrap();
        let text = |files: &str| format!("name = \"n\"\naddress = \"h\"\n[api]\n{files}");
        let both = format!(
            "tls_cert = {:?}\ntls_key = {:?}",
            cert.display().to_string(),
            key.display().to_string()
        );
        let c = parse(&text(&both));
        c.validate().unwrap();
        assert_eq!(
            c.api.read_tls().unwrap(),
            Some(("CERT".to_string(), "KEY".to_string()))
        );
        let only = format!("tls_cert = {:?}", cert.display().to_string());
        let err = parse(&text(&only)).validate().unwrap_err().to_string();
        assert!(err.contains("together"), "{err}");
        // 읽지 못하면 오류
        std::fs::remove_file(&key).unwrap();
        let err = parse(&text(&both)).api.read_tls().unwrap_err().to_string();
        assert!(err.contains("api.key"), "{err}");
    }

    #[test]
    fn api_section_lists_trusted_proxies() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert!(c.api.trusted_proxies.is_empty());
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[api]\ntrusted_proxies = [\"10.0.0.0/8\", \"fd00::/8\", \"192.0.2.10\"]",
        );
        let shown: Vec<String> = c
            .api
            .trusted_proxies
            .iter()
            .map(|c| c.to_string())
            .collect();
        assert_eq!(shown, ["10.0.0.0/8", "fd00::/8", "192.0.2.10/32"]);
        for bad in ["\"10.0.0.1/8\"", "\"proxy.lan\"", "\"10.0.0.0/33\""] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[api]\ntrusted_proxies = [{bad}]");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn data_dir_is_read_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opld.toml");
        // 다른 키가 틀려도 data_dir만 읽는다.
        std::fs::write(
            &path,
            "data_dir = \"/d\"\nunknown = 1\n[vip]\naddresses = 5",
        )
        .unwrap();
        assert_eq!(Config::data_dir_of(&path).unwrap(), Path::new("/d"));
        std::fs::write(&path, "name = \"n\"").unwrap();
        assert_eq!(
            Config::data_dir_of(&path).unwrap(),
            Path::new("/var/lib/opl")
        );
        std::fs::write(&path, "data_dir = 5").unwrap();
        assert!(Config::data_dir_of(&path).is_err());
        assert!(Config::data_dir_of(&dir.path().join("none")).is_err());
    }

    #[test]
    fn node_zone() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.node.zone, None);
        let c = parse("name = \"n\"\naddress = \"h\"\n[node]\nzone = \"rack-a\"");
        assert_eq!(c.node.zone.as_deref(), Some("rack-a"));
        c.validate().unwrap();
        let c = parse("name = \"n\"\naddress = \"h\"\n[node]\nzone = \" \"");
        assert!(c.validate().unwrap_err().to_string().contains("node.zone"));
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[node]\nrack = \"a\"")
                .is_err()
        );
    }

    #[test]
    fn vip_and_web_sections() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert!(c.vip.addresses.is_empty());
        assert!(c.vip.agent_config().is_none());
        assert!(c.web.enabled);
        c.validate().unwrap();

        let text = "name = \"n\"\naddress = \"h\"\n[vip]\naddresses = [\"192.168.0.200/24\", \"192.168.0.201\"]\ninterface = \"eth0\"\n[web]\nenabled = false";
        let c = parse(text);
        assert!(!c.web.enabled);
        let agent = c.vip.agent_config().unwrap();
        assert_eq!(agent.interface, "eth0");
        let shown: Vec<String> = agent.addresses.iter().map(|a| a.to_string()).collect();
        assert_eq!(shown, ["192.168.0.200/24", "192.168.0.201/32"]);
        // macOS는 VIP를 지원하지 않는다.
        assert_eq!(c.validate().is_ok(), cfg!(target_os = "linux"));
        if cfg!(target_os = "linux") {
            let with = |extra: &str| {
                let c: Config = toml::from_str(&format!(
                    "name = \"n\"\naddress = \"h\"\n{extra}\n[vip]\naddresses = [\"10.0.0.5/24\"]"
                ))
                .unwrap();
                c.validate()
            };
            assert!(with("").is_err(), "no interface");
            assert!(with("witness = true").is_err());
            let mut dup = parse(text);
            dup.vip.addresses.push("192.168.0.200/25".parse().unwrap());
            assert!(dup.validate().is_err());
            let mut bad = parse(text);
            bad.vip.interface = Some("eth 0".into());
            assert!(bad.validate().is_err());
        } else {
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("needs Linux"), "{err}");
        }
        for bad in ["\"fd00::1/64\"", "\"10.0.0.5/0\"", "\"eth0\""] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[vip]\naddresses = [{bad}]");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    /// `[test]`는 시험 빌드에만 있다.
    #[test]
    fn the_test_section_exists_only_in_starve_builds() {
        let parsed =
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[test]\nblock_api_ms = 15000");
        #[cfg(feature = "starve-it")]
        assert_eq!(parsed.unwrap().test.block_api(), Duration::from_secs(15));
        #[cfg(not(feature = "starve-it"))]
        assert!(parsed.is_err());
    }

    #[test]
    fn telemetry_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.telemetry, TelemetrySection::default());
        let t = c.telemetry.telemetry_config().unwrap();
        assert_eq!(t.tick, Duration::from_secs(60));
        assert_eq!(t.interval, Duration::from_secs(24 * 3600));
        assert_eq!(t.retry, Duration::from_secs(3600));
        assert_eq!(t.timeout, Duration::from_secs(30));
        assert!(t.extra_roots.is_empty());

        // ca_file의 인증서를 모두(DER로) 믿는다.
        let dir = tempfile::tempdir().unwrap();
        let ca_file = dir.path().join("ca.pem");
        let a = opl_peer::pki::ClusterCa::generate("a").unwrap().cert_pem;
        let b = opl_peer::pki::ClusterCa::generate("b").unwrap().cert_pem;
        std::fs::write(&ca_file, format!("{a}{b}")).unwrap();
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[telemetry]\ntick = \"1s\"\ninterval = \"6h\"\n\
             retry = \"10m\"\nca_file = \"{}\"",
            ca_file.display()
        ));
        let t = c.telemetry.telemetry_config().unwrap();
        assert_eq!(t.tick, Duration::from_secs(1));
        assert_eq!(t.interval, Duration::from_secs(6 * 3600));
        assert_eq!(t.retry, Duration::from_secs(600));
        assert_eq!(
            t.extra_roots,
            vec![
                opl_peer::pki::first_cert(&a).unwrap().to_vec(),
                opl_peer::pki::first_cert(&b).unwrap().to_vec()
            ]
        );

        // 읽지 못하거나 인증서가 없는 파일은 시작 오류다.
        let with_ca = |path: &Path| {
            parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[telemetry]\nca_file = \"{}\"",
                path.display()
            ))
        };
        let missing = dir.path().join("missing.pem");
        let err = with_ca(&missing).telemetry.telemetry_config().unwrap_err();
        assert!(err.to_string().contains("missing.pem"), "{err}");
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        assert!(with_ca(&empty).telemetry.telemetry_config().is_err());

        for bad in ["interval = \"0s\"", "retry = \"1d\"", "endpoint = \"x\""] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[telemetry]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn remotes_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.remotes, RemotesSection::default());
        let p = c.remotes.poller_config();
        assert_eq!(p.interval, Duration::from_secs(30));
        assert_eq!(p.tick, Duration::from_secs(5));
        let c = parse("name = \"n\"\naddress = \"h\"\n[remotes]\ninterval = \"2s\"\ntick = \"1s\"");
        let p = c.remotes.poller_config();
        assert_eq!(p.interval, Duration::from_secs(2));
        assert_eq!(p.tick, Duration::from_secs(1));
        for bad in ["interval = \"0s\"", "tick = \"x\"", "url = \"https://h\""] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[remotes]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\n\
             qemu_binary = \"/opt/qemu/bin/qemu-system-x86_64\"\nsystemd_scope = false",
        );
        assert!(c.validate().is_ok());
        let p = c.host_profile().unwrap();
        assert_eq!(p.accel, Accel::Tcg);
        assert_eq!(p.qemu_binary, Path::new("/opt/qemu/bin/qemu-system-x86_64"));

        // 다른 OS의 가속기와 macOS의 systemd 스코프는 거부한다.
        let other = if cfg!(target_os = "linux") {
            "hvf"
        } else {
            "kvm"
        };
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"{other}\"\nsystemd_scope = false"
        ));
        assert!(c.validate().is_err());
        if !cfg!(target_os = "linux") {
            let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\nsystemd_scope = true");
            assert!(c.validate().is_err());
        }
        assert!(
            toml::from_str::<Config>(
                "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"xen\""
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_keys_in_hypervisor_section() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[hypervisor]\nacel = \"kvm\"")
                .is_err()
        );
    }

    #[test]
    fn explicit_kvm_needs_dev_kvm() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"kvm\"");
        // `/dev/kvm`이 있는 호스트(Linux CI)에서는 KVM을 쓰고, 없으면 오류다.
        match c.host_profile() {
            Ok(p) => {
                assert!(kvm_available());
                assert_eq!(p.accel, Accel::Kvm);
            }
            Err(e) => {
                assert!(!kvm_available());
                assert!(e.to_string().contains("/dev/kvm"), "{e}");
            }
        }
    }

    #[test]
    fn ha_section_defaults_by_os() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        c.validate().unwrap();
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog"));
        assert_eq!(c.ha.watchdog_timeout_secs, 30);
        assert_eq!(c.ha.fence_after_secs, 30);
        assert_eq!(c.ha.fence_after(), Duration::from_secs(30));
        if cfg!(target_os = "linux") {
            assert_eq!(c.ha.fencing, FencingSetting::Watchdog);
            assert_eq!(
                c.ha.node_ha(),
                Some(NodeHa {
                    fencing: Fencing::Watchdog,
                    recover_after_secs: 65,
                })
            );
        } else {
            assert_eq!(c.ha.fencing, FencingSetting::Off);
            assert_eq!(c.ha.node_ha(), None);
        }
    }

    #[test]
    fn ha_section_sets_fencing_and_recover_after() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 2",
        );
        c.validate().unwrap();
        // process는 watchdog 제한 시간을 더하지 않는다.
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        let c = parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"");
        c.validate().unwrap();
        assert_eq!(c.ha.node_ha(), None);
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\n\
             watchdog_device = \"/dev/watchdog1\"\nwatchdog_timeout_secs = 10\nfence_after_secs = 20",
        );
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog1"));
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Watchdog,
                recover_after_secs: 35,
            })
        );
        // watchdog 장치는 Linux에만 있다.
        assert_eq!(c.validate().is_ok(), cfg!(target_os = "linux"));
        for bad in ["fencing = \"ipmi\"", "fence_after = 1"] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[ha]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn ha_section_validates_times() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 0",
        );
        assert!(c.validate().is_err());
        // 꺼져 있어도 fence_after는 검사한다.
        let c =
            parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"\nfence_after_secs = 0");
        assert!(c.validate().is_err());
        // 제한 시간은 5초 이상이다. 장치를 1초마다 쓰다듬으므로 너무 짧으면 쓰다듬기 한 번만 늦어도
        // 노드가 재시작된다. Linux가 아니면 범위 안이어도 장치가 없어 실패한다.
        for (timeout, ok) in [
            (0, false),
            (4, false),
            (5, true),
            (3600, true),
            (3601, false),
        ] {
            let c = parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\nwatchdog_timeout_secs = {timeout}"
            ));
            match c.validate() {
                Ok(()) => assert!(ok && cfg!(target_os = "linux"), "{timeout}"),
                Err(e) => assert_eq!(
                    e.to_string().contains("watchdog_timeout_secs"),
                    !ok,
                    "{timeout}: {e}"
                ),
            }
        }
        // watchdog가 아니면 장치 제한 시간은 쓰지 않으므로 보지 않는다.
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nwatchdog_timeout_secs = 0",
        );
        c.validate().unwrap();
    }

    /// 펜싱이 꺼져 있으면 guard가 없고, process는 watchdog 없이, watchdog는 장치와 함께 만든다.
    #[test]
    fn guard_config_follows_the_fencing() {
        let mut ha = HaSection {
            fencing: FencingSetting::Off,
            fence_after_secs: 5,
            ..HaSection::default()
        };
        assert!(ha.guard_config().is_none());
        ha.fencing = FencingSetting::Process;
        let config = ha.guard_config().unwrap();
        assert_eq!(config.fence_after, Duration::from_secs(5));
        assert_eq!(config.renew_interval, Duration::from_millis(500));
        assert!(config.watchdog.is_none());
        ha.fencing = FencingSetting::Watchdog;
        assert!(ha.guard_config().unwrap().watchdog.is_some());
    }

    #[test]
    fn renew_interval_is_a_tenth_of_fence_after_within_bounds() {
        let interval = |fence_after_secs| {
            HaSection {
                fence_after_secs,
                ..HaSection::default()
            }
            .renew_interval()
        };
        assert_eq!(interval(1), Duration::from_millis(200));
        assert_eq!(interval(2), Duration::from_millis(200));
        assert_eq!(interval(5), Duration::from_millis(500));
        assert_eq!(interval(20), Duration::from_secs(2));
        assert_eq!(interval(30), Duration::from_secs(2));
        assert_eq!(interval(u64::MAX), Duration::from_secs(2));
    }

    #[test]
    fn migration_section_parses_ports_and_timeout() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"61000-61009\"\n\
             bandwidth_mib = 100\ntimeout = \"1h30m\"\ndisk_timeout = \"2h\"",
        );
        assert_eq!(c.migration.ports, 61000..=61009);
        assert_eq!(c.migration.bandwidth_mib, 100);
        assert_eq!(c.migration.timeout, Duration::from_secs(5400));
        assert_eq!(c.migration.disk_timeout, Duration::from_secs(7200));
        let one = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"60000-60000\"\ntimeout = \"90s\"",
        );
        assert_eq!(one.migration.ports, 60000..=60000);
        assert_eq!(one.migration.timeout, Duration::from_secs(90));
        for bad in [
            "ports = \"60099-60000\"",
            "ports = \"0-10\"",
            "ports = \"60000\"",
            "ports = \"60000-70000\"",
            "ports = \"a-b\"",
            "timeout = \"0s\"",
            "timeout = \"30\"",
            "timeout = \"500ms\"",
            "timeout = \"m\"",
            "timeout = \"\"",
            "disk_timeout = \"0s\"",
            "disk_timeout = \"12\"",
            "bandwidth = 1",
        ] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[migration]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn underlay_comes_from_the_setting_or_the_address() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // 설정이 address보다 앞선다.
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"\n[network]\nunderlay = \"10.1.0.5\"");
        c.validate().unwrap();
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("10.1.0.5")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\n[network]\nunderlay = \"fd00::5\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("fd00::5")));
        // 없으면 address: IPv4·IPv6 리터럴은 그대로, 이름은 푼다.
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.network, NetworkSection::default());
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("10.0.0.5")));
        let c = parse("name = \"n\"\naddress = \"::1\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("::1")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Lookup("node1.lan"));
        for bad in [
            "underlay = \"node1.lan\"",
            "underlay = \"10.0.0.5:4789\"",
            "mtu = 1450",
        ] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[network]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
        for bad in ["0.0.0.0", "::"] {
            let c = parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[network]\nunderlay = \"{bad}\""
            ));
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("network.underlay"), "{err}");
        }
    }

    #[test]
    fn resolved_underlay_skips_local_addresses_and_prefers_ipv4() {
        let pick = |addrs: &[&str]| pick_underlay(addrs.iter().map(|a| a.parse().unwrap()));
        // `/etc/hosts`의 127.0.1.1 같은 loopback, link-local, 0.0.0.0은 다른 노드가 닿을 수 없다.
        assert_eq!(pick(&["127.0.1.1"]), None);
        assert_eq!(pick(&["169.254.1.1", "fe80::1", "::", "0.0.0.0"]), None);
        assert_eq!(pick(&[]), None);
        assert_eq!(
            pick(&["::1", "10.0.0.5"]),
            Some("10.0.0.5".parse().unwrap())
        );
        assert_eq!(
            pick(&["fe80::1", "2001:db8::5"]),
            Some("2001:db8::5".parse().unwrap())
        );
        // IPv4가 먼저다.
        assert_eq!(
            pick(&["2001:db8::5", "10.0.0.5", "10.0.0.6"]),
            Some("10.0.0.5".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn underlay_resolves_a_name_or_is_none() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // 이름 풀기를 바꿔 끼워 DNS에 기대지 않는다.
        let lookup = |result: std::io::Result<Vec<IpAddr>>| {
            move |host: String| {
                assert_eq!(host, "node1.lan");
                std::future::ready(result)
            }
        };
        let no_lookup =
            |_: String| -> std::future::Ready<std::io::Result<Vec<IpAddr>>> { panic!("lookup") };
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.underlay().await, Some(ip("10.0.0.5")));
        // 리터럴과 설정은 풀지 않는다. 리터럴은 loopback이어도 그대로 쓴다.
        let c = parse("name = \"n\"\naddress = \"127.0.0.1\"");
        assert_eq!(c.underlay_with(no_lookup).await, Some(ip("127.0.0.1")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\n[network]\nunderlay = \"10.1.0.5\"");
        assert_eq!(c.underlay_with(no_lookup).await, Some(ip("10.1.0.5")));
        // 이름은 풀어서 쓸 수 있는 주소를 고른다. loopback만 나오거나 풀지 못하면 None이다.
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(
            c.underlay_with(lookup(Ok(vec![ip("127.0.1.1"), ip("10.0.0.7")])))
                .await,
            Some(ip("10.0.0.7"))
        );
        assert_eq!(
            c.underlay_with(lookup(Ok(vec![ip("127.0.1.1")]))).await,
            None
        );
        assert_eq!(
            c.underlay_with(lookup(Err(std::io::Error::other("no such host"))))
                .await,
            None
        );
    }
}
