//! 노드에서 Ceph를 세우는 단계들: 꾸러미 설치, 설정 파일·키링 쓰기, mon·mgr·OSD 만들기, 풀 만들기, 상태
//! 읽기. 모든 단계는 상태를 보고 이미 된 일을 건너뛰므로(멱등) 실패한 Task를 다시 돌려도 된다. 명령은
//! [`CommandRunner`]로 돌리고(시험은 [`FakeRunner`](super::FakeRunner)), 파일은 [`CephPaths`] 아래에 직접
//! 쓴다(시험은 임시 디렉터리).

use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use orbivisor_types::ceph::{CLIENT_ID, CephKeys, CephReport, DiskState, MIN_MAJOR, NodeDisk};
use thiserror::Error;
use uuid::Uuid;

use super::conf::{is_managed, mon_addrv, render_keyring};
use super::disks::{DiskContext, LSBLK_COLUMNS, classify, parse_lsblk, parse_open_vgs, parse_pvs};
use super::json::{
    CephVersion, ReportInput, parse_mgr_dump, parse_osd_tree, parse_pg_ls, parse_pool_ls,
    parse_quorum, parse_version, parse_volume_list, report,
};
use super::runner::{Cmd, CmdOutput, CommandRunner};
use crate::error::DriverError;

/// 데몬 노드(mon·mgr·OSD)에 까는 꾸러미의 기본 목록
pub const DAEMON_PACKAGES: &[&str] = &[
    "ceph-mon",
    "ceph-mgr",
    "ceph-osd",
    "ceph-volume",
    "ceph-common",
    "lvm2",
    "qemu-block-extra",
];
/// RBD 풀만 쓰는 노드에 까는 꾸러미의 기본 목록
pub const CLIENT_PACKAGES: &[&str] = &["ceph-common", "qemu-block-extra"];
/// apt가 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 기다리는 시간(초)
const APT_LOCK: &str = "DPkg::Lock::Timeout=600";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// `ceph`가 mon에 닿기를 기다리는 시간(초)
const CONNECT_TIMEOUT: &str = "10";
/// 상태 보고서 명령 하나의 시간 한도
const REPORT_TIMEOUT: Duration = Duration::from_secs(15);
/// `ceph-volume lvm zap --destroy`의 시간 한도
const ZAP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// `ceph-volume lvm create`의 시간 한도
const OSD_CREATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// 새 OSD 유닛이 active가 되기를 기다리는 시간의 상한
const OSD_ACTIVE_LIMIT: Duration = Duration::from_secs(2 * 60);
/// 데몬 디렉터리에 Orbivisor가 남기는 이 Ceph 클러스터의 fsid(mon·mgr)
const FSID_FILE: &str = "fsid";
/// ceph-volume이 OSD 디렉터리에 쓰는 클러스터 fsid
const OSD_FSID_FILE: &str = "ceph_fsid";
/// mon 저장소를 다 만들었다는 표시. 내용은 그 monmap의 출처(`init` — 새 클러스터, `join` — 기존 정족수)다.
const DONE_FILE: &str = "done";

/// `client.admin`의 caps
const ADMIN_CAPS: &[(&str, &str)] = &[
    ("mon", "allow *"),
    ("osd", "allow *"),
    ("mds", "allow *"),
    ("mgr", "allow *"),
];
/// `mon.`의 caps
const MON_CAPS: &[(&str, &str)] = &[("mon", "allow *")];
/// `client.bootstrap-osd`의 caps(ceph-volume이 OSD를 등록하는 데 쓴다)
const BOOTSTRAP_OSD_CAPS: &[(&str, &str)] = &[("mon", "profile bootstrap-osd"), ("mgr", "allow r")];
/// `client.orbivisor`의 caps(모든 RBD 풀)
const CLIENT_CAPS: &[(&str, &str)] = &[
    ("mon", "profile rbd"),
    ("osd", "profile rbd"),
    ("mgr", "profile rbd"),
];

/// Ceph 구성 단계의 실패
#[derive(Debug, Error)]
pub enum CephError {
    /// 이 노드에 Orbivisor가 만들지 않은 Ceph가 있다(건드리지 않는다).
    #[error("{0}")]
    Foreign(String),
    /// 명령이 실패했거나 결과가 맞지 않다.
    #[error("{0}")]
    Command(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// 이 노드에서는 할 수 없다(Linux가 아님, Ceph 판이 낮음 등).
    #[error("{0}")]
    Unsupported(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<DriverError> for CephError {
    fn from(e: DriverError) -> Self {
        match e {
            DriverError::Timeout(what) => Self::Timeout(what),
            DriverError::Io(e) => Self::Io(e),
            e => Self::Command(e.to_string()),
        }
    }
}

/// Ceph 파일이 있는 디렉터리들
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CephPaths {
    /// 데몬 노드의 설정·admin 키링(`/etc/ceph`)
    pub etc_ceph: PathBuf,
    /// 데몬 저장소(`/var/lib/ceph`)
    pub var_lib_ceph: PathBuf,
    /// 모든 노드의 클라이언트 설정(`/etc/opl/ceph`)
    pub client_dir: PathBuf,
    /// 디스크의 이름 링크(`/dev/disk/by-id`)
    pub by_id: PathBuf,
}

impl Default for CephPaths {
    fn default() -> Self {
        Self {
            etc_ceph: "/etc/ceph".into(),
            var_lib_ceph: "/var/lib/ceph".into(),
            client_dir: "/etc/opl/ceph".into(),
            by_id: "/dev/disk/by-id".into(),
        }
    }
}

/// Ceph 구성의 설정(opld `[ceph]`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CephOptions {
    /// 데몬 노드에 까는 꾸러미
    pub packages: Vec<String>,
    /// 클라이언트 노드에 까는 꾸러미
    pub client_packages: Vec<String>,
    /// `apt-get update`·`install` 하나의 시간 한도
    pub install_timeout: Duration,
    /// 루프 장치도 OSD 디스크로 쓴다(시험용).
    pub allow_loop: bool,
    /// 기다리는 동안 다시 보는 간격
    pub wait_step: Duration,
    /// 정족수·mgr·PG를 기다리는 시간의 상한(OSD 유닛은 이것과 2분 가운데 짧은 것)
    pub wait_limit: Duration,
    /// Linux가 아닌 곳에서도 단계를 돌린다(시험용 — 명령은 가짜 실행기가 받는다).
    pub any_os: bool,
}

impl Default for CephOptions {
    fn default() -> Self {
        Self {
            packages: DAEMON_PACKAGES.iter().map(|p| p.to_string()).collect(),
            client_packages: CLIENT_PACKAGES.iter().map(|p| p.to_string()).collect(),
            install_timeout: Duration::from_secs(30 * 60),
            allow_loop: false,
            wait_step: Duration::from_secs(5),
            wait_limit: Duration::from_secs(5 * 60),
            any_os: false,
        }
    }
}

/// mon 저장소의 monmap을 어디서 얻는지
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonmapSource {
    /// 새 클러스터: 이 mon 하나로 만든다.
    New,
    /// 기존 정족수에서 받아 이 mon을 더한다.
    Existing,
}

/// 만들 mon 하나
#[derive(Debug, Clone, Copy)]
pub struct MonInit<'a> {
    /// mon 이름(노드 이름)
    pub name: &'a str,
    pub addr: IpAddr,
    pub fsid: Uuid,
    pub keys: &'a CephKeys,
    pub monmap: MonmapSource,
    /// 이 노드가 지금 부트스트랩 노드(`CephStatus.init_node`)인지. 아니면 부트스트랩으로 만든(`init`) 저장소를
    /// 기존 정족수에 넣지 않는다.
    pub init_node: bool,
}

/// 만들 Ceph 풀
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolPlan {
    /// Ceph 풀 이름
    pub pool: String,
    /// 등록할 Orbivisor RBD 풀 이름(Ceph 풀에 표시로 단다)
    pub name: String,
    /// 복제 수(1~3)
    pub size: u8,
    /// OSD가 up인 호스트 수
    pub osd_hosts: u32,
}

/// 노드 `node`의 Ceph 판 `have`가 클러스터의 판(`CephStatus.release`, `19.2.1 squid`)과 주 판이 같은지.
/// 클러스터 판을 아직 모르면 통과다.
pub fn check_release(
    node: &str,
    have: &CephVersion,
    cluster: Option<&str>,
) -> Result<(), CephError> {
    let Some(cluster) = cluster else {
        return Ok(());
    };
    let major = cluster
        .split(['.', ' '])
        .next()
        .and_then(|m| m.parse::<u32>().ok());
    if major == Some(have.major) {
        return Ok(());
    }
    Err(CephError::Unsupported(format!(
        "node {node:?} has ceph {} but the cluster runs {cluster}; upgrade the packages first",
        have.release()
    )))
}

/// 노드에서 Ceph를 세우는 단계들
pub struct CephHost {
    runner: Arc<dyn CommandRunner>,
    paths: CephPaths,
    options: CephOptions,
}

impl CephHost {
    pub fn new(runner: Arc<dyn CommandRunner>, paths: CephPaths, options: CephOptions) -> Self {
        Self {
            runner,
            paths,
            options,
        }
    }

    pub fn paths(&self) -> &CephPaths {
        &self.paths
    }

    pub fn options(&self) -> &CephOptions {
        &self.options
    }

    /// 클라이언트 설정 파일(`{client_dir}/ceph.conf` — RBD 풀의 `conf`)
    pub fn client_conf(&self) -> PathBuf {
        self.paths.client_dir.join("ceph.conf")
    }

    /// `client.orbivisor` 키링(`{client_dir}/ceph.client.orbivisor.keyring`)
    pub fn client_keyring(&self) -> PathBuf {
        self.paths
            .client_dir
            .join(format!("ceph.client.{CLIENT_ID}.keyring"))
    }

    fn daemon_conf(&self) -> PathBuf {
        self.paths.etc_ceph.join("ceph.conf")
    }

    fn mon_dir(&self, name: &str) -> PathBuf {
        self.paths.var_lib_ceph.join(format!("mon/ceph-{name}"))
    }

    fn mgr_dir(&self, name: &str) -> PathBuf {
        self.paths.var_lib_ceph.join(format!("mgr/ceph-{name}"))
    }

    /// 단계가 쓰는 임시 디렉터리(`/var/lib/ceph/tmp/opl-{what}`)
    fn tmp_dir(&self, what: &str) -> PathBuf {
        self.paths.var_lib_ceph.join(format!("tmp/opl-{what}"))
    }

    /// Ceph는 Linux 노드에만 세운다.
    pub fn check_platform(&self) -> Result<(), CephError> {
        if cfg!(target_os = "linux") || self.options.any_os {
            Ok(())
        } else {
            Err(CephError::Unsupported(
                "Ceph can be set up only on Linux nodes".into(),
            ))
        }
    }

    /// 노드 `node`에 다른 Ceph 클러스터가 있으면 실패한다: 표시 없는 `/etc/ceph/ceph.conf`, 또는 fsid가
    /// `fsid`가 아니거나 알 수 없는 `/var/lib/ceph/{mon,mgr,osd}/ceph-*`(빈 디렉터리는 본다 — 잃을 것이
    /// 없다). 아무것도 바꾸지 않는다.
    pub fn check_foreign(&self, node: &str, fsid: Uuid) -> Result<(), CephError> {
        if let Some(text) = read_optional(&self.daemon_conf())?
            && !is_managed(&text)
        {
            return Err(CephError::Foreign(format!(
                "node {node:?} already has {} not written by Orbivisor (another Ceph cluster?); \
                 Orbivisor does not adopt an existing Ceph — use it as an rbd storage pool \
                 instead, or move the file away",
                self.daemon_conf().display()
            )));
        }
        // cephadm은 데몬을 `/var/lib/ceph/{fsid}/`에 둔다(우리는 그런 디렉터리를 만들지 않는다).
        match std::fs::read_dir(&self.paths.var_lib_ceph) {
            Ok(entries) => {
                let mut dirs: Vec<PathBuf> = entries
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_dir()
                            && p.file_name()
                                .and_then(|n| n.to_str())
                                .is_some_and(|n| n.parse::<Uuid>().is_ok())
                    })
                    .collect();
                dirs.sort();
                if let Some(dir) = dirs.first() {
                    return Err(CephError::Foreign(format!(
                        "node {node:?}: {} belongs to another Ceph cluster (deployed by cephadm)",
                        dir.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_at(&self.paths.var_lib_ceph, e)),
        }
        let ours = fsid.to_string();
        for (kind, file) in [
            ("mon", FSID_FILE),
            ("mgr", FSID_FILE),
            ("osd", OSD_FSID_FILE),
        ] {
            let parent = self.paths.var_lib_ceph.join(kind);
            let entries = match std::fs::read_dir(&parent) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_at(&parent, e)),
            };
            let mut dirs: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("ceph-"))
                })
                .collect();
            dirs.sort();
            for dir in dirs {
                if std::fs::read_dir(&dir)
                    .map_err(|e| io_at(&dir, e))?
                    .next()
                    .is_none()
                {
                    continue;
                }
                let found = read_optional(&dir.join(file))?;
                if found.as_deref().map(str::trim) != Some(ours.as_str()) {
                    return Err(CephError::Foreign(format!(
                        "node {node:?}: {} belongs to another Ceph cluster",
                        dir.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// `packages` 가운데 깔리지 않은 것만 apt로 깔고 설치된 Ceph의 판을 돌려준다. Ceph 18(reef) 미만이면
    /// 실패한다.
    pub async fn ensure_packages(&self, packages: &[String]) -> Result<CephVersion, CephError> {
        let missing = self.missing_packages(packages).await?;
        if !missing.is_empty() {
            let apt = |args: Vec<String>| {
                Cmd::new("apt-get", &args)
                    .env("DEBIAN_FRONTEND", "noninteractive")
                    .timeout(self.options.install_timeout)
            };
            self.run_ok(&apt(vec![
                "update".into(),
                "-q".into(),
                "-o".into(),
                APT_LOCK.into(),
            ]))
            .await?;
            let mut args: Vec<String> = ["install", "-y", "-q", "-o", APT_LOCK]
                .iter()
                .map(|a| a.to_string())
                .collect();
            args.extend(missing);
            self.run_ok(&apt(args)).await?;
        }
        let out = self.run_ok(&Cmd::new("ceph", &["--version"])).await?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let version = parse_version(&line).ok_or_else(|| {
            CephError::Command(format!("cannot tell the Ceph version from {line:?}"))
        })?;
        if version.major < MIN_MAJOR {
            return Err(CephError::Unsupported(format!(
                "ceph {} is too old; Orbivisor sets up Ceph 18 (reef) or newer",
                version.release()
            )));
        }
        Ok(version)
    }

    /// `dpkg-query`가 `install ok installed`로 보이지 않는 꾸러미(차례 그대로)
    async fn missing_packages(&self, packages: &[String]) -> Result<Vec<String>, CephError> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec![
            "-W".to_string(),
            "-f".into(),
            "${Package} ${Status}\n".into(),
        ];
        args.extend(packages.iter().cloned());
        // 모르는 꾸러미가 있으면 종료 1이지만 아는 것은 출력에 있다.
        let out = self.exec(&Cmd::new("dpkg-query", &args)).await?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let installed: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter(|(_, status)| status.trim() == "install ok installed")
            .map(|(name, _)| name)
            .collect();
        Ok(packages
            .iter()
            .filter(|p| !installed.contains(&p.as_str()))
            .cloned()
            .collect())
    }

    /// 데몬 노드의 `/etc/ceph/ceph.conf`를 쓴다. Orbivisor가 쓰지 않은 파일이 있으면 쓰지 않는다. 내용이
    /// 같으면 그대로 두고 `false`다.
    pub fn write_conf(&self, text: &str) -> Result<bool, CephError> {
        let path = self.daemon_conf();
        if let Some(old) = read_optional(&path)?
            && !is_managed(&old)
        {
            return Err(CephError::Foreign(format!(
                "{} was not written by Orbivisor; not overwriting it",
                path.display()
            )));
        }
        write_file(&path, text.as_bytes(), 0o644)
    }

    /// 클라이언트 설정(`conf`는 [`render_client_conf`](super::render_client_conf)의 결과)과
    /// `client.orbivisor` 키링(0600)을 쓴다. 둘 다 같으면 `false`다.
    pub fn write_client(&self, conf: &str, key: &str) -> Result<bool, CephError> {
        let entity = format!("client.{CLIENT_ID}");
        let keyring = render_keyring(&[(entity.as_str(), key, &[])]);
        let a = write_file(&self.client_keyring(), keyring.as_bytes(), 0o600)?;
        let b = write_file(&self.client_conf(), conf.as_bytes(), 0o644)?;
        Ok(a || b)
    }

    /// mon·mgr 노드의 admin 키링(`/etc/ceph/ceph.client.admin.keyring`, 0600)
    pub fn write_admin_keyring(&self, key: &str) -> Result<bool, CephError> {
        let keyring = render_keyring(&[("client.admin", key, ADMIN_CAPS)]);
        write_file(
            &self.paths.etc_ceph.join("ceph.client.admin.keyring"),
            keyring.as_bytes(),
            0o600,
        )
    }

    /// OSD 노드의 `bootstrap-osd` 키링(`/var/lib/ceph/bootstrap-osd/ceph.keyring`, ceph:ceph 0600)
    pub async fn write_bootstrap_osd(&self, key: &str) -> Result<bool, CephError> {
        let keyring = render_keyring(&[("client.bootstrap-osd", key, BOOTSTRAP_OSD_CAPS)]);
        let path = self.paths.var_lib_ceph.join("bootstrap-osd/ceph.keyring");
        let changed = write_file(&path, keyring.as_bytes(), 0o600)?;
        // 쓴 뒤 chown 전에 멈췄으면 ceph-volume이 키링을 읽지 못하므로 늘 맞춘다.
        self.chown(&path, false).await?;
        Ok(changed)
    }

    /// mon 저장소를 만든다(이미 다 만들었으면 — `done`이 있으면 — 아무것도 안 한다). 저장소는 임시
    /// 디렉터리에서 만들고 `fsid`·`done`을 쓴 뒤 제자리로 옮긴다: `/var/lib/ceph/mon/ceph-{name}`은 없거나
    /// 다 만든 것이다(`ceph-mon --mkfs`는 비지 않은 디렉터리를 말없이 건너뛴다). `true`면 새로 만들었다.
    /// 기존 정족수에 더하는데 저장소가 부트스트랩(`init`)으로 만든 것이고 이 노드가 지금 부트스트랩 노드가
    /// 아니면(다른 노드로 옮겨 간 버려진 부트스트랩이 남긴 것) 그 저장소는 다른 클러스터의 것이므로 실패한다 —
    /// 지우지 않고 손으로 치우는 명령을 알린다.
    pub async fn create_monitor(&self, mon: &MonInit<'_>) -> Result<bool, CephError> {
        let target = self.mon_dir(mon.name);
        if let Some(done) = read_optional(&target.join(DONE_FILE))? {
            if mon.monmap == MonmapSource::Existing && done.trim() == "init" && !mon.init_node {
                return Err(CephError::Foreign(format!(
                    "{} was made by an abandoned Ceph bootstrap on this node and is not part of \
                     the cluster; stop it and move it away by hand: systemctl disable --now \
                     ceph-mon@{}; mv {} {}.old",
                    target.display(),
                    mon.name,
                    target.display(),
                    target.display()
                )));
            }
            return Ok(false);
        }
        let tmp = self.tmp_dir(&format!("mon-{}", mon.name));
        remove_dir(&tmp)?;
        make_dir(&tmp, 0o700)?;
        let result = self.build_monitor(mon, &tmp, &target).await;
        // 지우지 못해도 만들기의 결과(실패면 그 이유)를 돌려준다.
        if let Err(e) = remove_dir(&tmp) {
            tracing::warn!(error = %e, "cannot remove the temporary monitor directory");
        }
        result.map(|()| true)
    }

    async fn build_monitor(
        &self,
        mon: &MonInit<'_>,
        tmp: &Path,
        target: &Path,
    ) -> Result<(), CephError> {
        let keyring = tmp.join("keyring");
        let monmap = tmp.join("monmap");
        let store = tmp.join("mon");
        let keys = mon.keys;
        let text = match mon.monmap {
            // mkfs가 키링의 모든 항목을 auth DB에 넣는다(`ceph auth`가 따로 필요 없다).
            MonmapSource::New => render_keyring(&[
                ("mon.", &keys.mon, MON_CAPS),
                ("client.admin", &keys.admin, ADMIN_CAPS),
                (
                    "client.bootstrap-osd",
                    &keys.bootstrap_osd,
                    BOOTSTRAP_OSD_CAPS,
                ),
                (&format!("client.{CLIENT_ID}"), &keys.client, CLIENT_CAPS),
            ]),
            // 기존 클러스터의 auth DB는 정족수에서 받는다.
            MonmapSource::Existing => render_keyring(&[("mon.", &keys.mon, MON_CAPS)]),
        };
        write_file(&keyring, text.as_bytes(), 0o600)?;
        let addrv = mon_addrv(mon.addr);
        let monmap_arg = path_str(&monmap);
        match mon.monmap {
            MonmapSource::New => {
                let fsid = mon.fsid.to_string();
                self.run_ok(&Cmd::new(
                    "monmaptool",
                    &[
                        "--create",
                        "--addv",
                        mon.name,
                        &addrv,
                        "--fsid",
                        &fsid,
                        &monmap_arg,
                    ],
                ))
                .await?;
            }
            MonmapSource::Existing => {
                self.ceph(&["mon", "getmap", "-o", &monmap_arg]).await?;
                let printed = self
                    .run_ok(&Cmd::new("monmaptool", &["--print", &monmap_arg]))
                    .await?;
                let entity = format!("mon.{}", mon.name);
                let listed = String::from_utf8_lossy(&printed.stdout)
                    .lines()
                    .any(|l| l.split_whitespace().last() == Some(entity.as_str()));
                if !listed {
                    self.run_ok(&Cmd::new(
                        "monmaptool",
                        &["--addv", mon.name, &addrv, &monmap_arg],
                    ))
                    .await?;
                }
            }
        }
        make_dir(&store, 0o750)?;
        self.chown(tmp, true).await?;
        let store_arg = path_str(&store);
        self.run_ok(&Cmd::new(
            "ceph-mon",
            &[
                "--mkfs",
                "-i",
                mon.name,
                "--mon-data",
                &store_arg,
                "--monmap",
                &monmap_arg,
                "--keyring",
                &path_str(&keyring),
                "--setuser",
                "ceph",
                "--setgroup",
                "ceph",
            ],
        ))
        .await?;
        write_file(
            &store.join(FSID_FILE),
            format!("{}\n", mon.fsid).as_bytes(),
            0o644,
        )?;
        let source = match mon.monmap {
            MonmapSource::New => "init\n",
            MonmapSource::Existing => "join\n",
        };
        write_file(&store.join(DONE_FILE), source.as_bytes(), 0o644)?;
        // `done` 없는 우리 저장소(이전 판이 남긴 것)는 지우고 바꾼다.
        remove_dir(target)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
        }
        std::fs::rename(&store, target).map_err(|e| io_at(target, e))?;
        Ok(())
    }

    /// mon 유닛을 켜고 부팅 때도 켜지게 한다.
    pub async fn start_monitor(&self, name: &str) -> Result<(), CephError> {
        self.run_ok(&Cmd::new(
            "systemctl",
            &["enable", "ceph.target", "ceph-mon.target"],
        ))
        .await?;
        self.run_ok(&Cmd::new(
            "systemctl",
            &["enable", "--now", &format!("ceph-mon@{name}")],
        ))
        .await?;
        Ok(())
    }

    /// mon `name`이 정족수에 들 때까지 기다린다. `mons`(기존 정족수의 mon 주소 — Orbivisor가 아는 것)가 있으면 그
    /// mon들에만 묻는다(`-m`): 데몬 conf의 `mon_host`에는 이 노드의 새 mon도 있어, 다른 클러스터의 저장소로 떠 있는
    /// 이 노드의 mon이 혼자 정족수라고 답할 수 있다.
    pub async fn wait_quorum(&self, name: &str, mons: &[IpAddr]) -> Result<(), CephError> {
        let host = mons
            .iter()
            .map(|ip| mon_addrv(*ip))
            .collect::<Vec<_>>()
            .join(" ");
        let host = host.as_str();
        self.wait_for(
            "the monitor to join the quorum",
            self.options.wait_limit,
            move || async move {
                let mut args = vec!["quorum_status"];
                if !host.is_empty() {
                    args.extend(["-m", host]);
                }
                self.ceph_json(&args)
                    .await
                    .ok()
                    .and_then(|out| parse_quorum(&out).ok())
                    .is_some_and(|q| q.iter().any(|n| n == name))
            },
        )
        .await
    }

    /// mgr `name`을 만들고 켠다(키링이 이미 있으면 다시 만들지 않는다). 유닛이 active가 될 때까지 기다린다.
    pub async fn create_manager(&self, name: &str, fsid: Uuid) -> Result<(), CephError> {
        let dir = self.mgr_dir(name);
        let keyring = dir.join("keyring");
        if !keyring.exists() {
            make_dir(&dir, 0o750)?;
            write_file(&dir.join(FSID_FILE), format!("{fsid}\n").as_bytes(), 0o644)?;
            self.ceph(&[
                "auth",
                "get-or-create",
                &format!("mgr.{name}"),
                "mon",
                "allow profile mgr",
                "osd",
                "allow *",
                "mds",
                "allow *",
                "-o",
                &path_str(&keyring),
            ])
            .await?;
        }
        // 키링을 만든 뒤 chown 전에 멈췄으면 mgr가 키링을 읽지 못하므로 늘 맞춘다.
        self.chown(&dir, true).await?;
        self.run_ok(&Cmd::new("systemctl", &["enable", "ceph-mgr.target"]))
            .await?;
        let unit = format!("ceph-mgr@{name}");
        self.run_ok(&Cmd::new("systemctl", &["enable", "--now", &unit]))
            .await?;
        self.wait_unit(
            &unit,
            "the manager unit to become active",
            self.options.wait_limit,
        )
        .await
    }

    /// 클러스터에 쓸 수 있는 mgr가 생길 때까지 기다린다.
    pub async fn wait_manager(&self) -> Result<(), CephError> {
        self.wait_for(
            "a Ceph manager to become available",
            self.options.wait_limit,
            move || async move {
                self.ceph_json(&["mgr", "stat"])
                    .await
                    .ok()
                    .and_then(|out| parse_mgr_dump(&out).ok())
                    .is_some_and(|m| m.available)
            },
        )
        .await
    }

    /// 이 노드의 디스크마다 OSD로 쓸 수 있는지. `pool_vgs`는 이 노드가 쓰는 Orbivisor LVM 풀의
    /// (VG, 풀 이름), `fsid`는 Orbivisor Ceph의 fsid(없으면 아직 클러스터가 없다)다.
    pub async fn disks(
        &self,
        pool_vgs: &[(String, String)],
        fsid: Option<Uuid>,
    ) -> Result<Vec<NodeDisk>, CephError> {
        let out = self
            .run_ok(&Cmd::new(
                "lsblk",
                &["--json", "--bytes", "--paths", "--output", LSBLK_COLUMNS],
            ))
            .await?;
        let lsblk = parse_lsblk(&out.stdout).map_err(CephError::Command)?;
        // lvm2·ceph-volume이 아직 없는 노드(Ceph를 세우기 전)는 PV·OSD도 없다.
        let pv_vgs = match self
            .run_optional(&Cmd::new(
                "pvs",
                &["--reportformat", "json", "-o", "pv_name,vg_name"],
            ))
            .await?
        {
            Some(out) => parse_pvs(&out.stdout).map_err(CephError::Command)?,
            None => Vec::new(),
        };
        let open_vgs = match self
            .run_optional(&Cmd::new(
                "lvs",
                &["--reportformat", "json", "-o", "lv_name,vg_name,lv_attr"],
            ))
            .await?
        {
            Some(out) => parse_open_vgs(&out.stdout).map_err(CephError::Command)?,
            None => Vec::new(),
        };
        let ceph_osds = match fsid {
            Some(fsid) => match self
                .run_optional(&self.volume(&["lvm", "list", "--format", "json"]))
                .await?
            {
                Some(out) => parse_volume_list(&out.stdout)
                    .map_err(CephError::Command)?
                    .into_iter()
                    .filter(|o| o.cluster_fsid == fsid.to_string())
                    .collect(),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        let ctx = DiskContext {
            pool_vgs: pool_vgs.to_vec(),
            pv_vgs,
            open_vgs,
            ceph_osds,
            allow_loop: self.options.allow_loop,
            by_id: self.by_id_links()?,
        };
        Ok(classify(&lsblk, &ctx))
    }

    /// `device`(`/dev/sdb` 또는 by-id 경로)에 BlueStore OSD를 만들고 그 id를 돌려준다. 노드에서 디스크를
    /// 다시 가른다: 쓰는 중이면 실패, 비지 않았는데 `wipe`가 아니면 실패, 이미 이 클러스터의 OSD면 켜기만
    /// 한다(다시 하기).
    pub async fn create_osd(
        &self,
        device: &str,
        wipe: bool,
        fsid: Uuid,
        pool_vgs: &[(String, String)],
    ) -> Result<u32, CephError> {
        let disks = self.disks(pool_vgs, Some(fsid)).await?;
        let path = self.resolve_device(device)?;
        let disk = disks
            .iter()
            .find(|d| d.path == path || d.by_id.as_deref() == Some(device))
            .ok_or_else(|| CephError::Command(format!("{device} is not a disk on this node")))?;
        let path = disk.path.clone();
        if let Some(osd) = self.our_osd(&path, fsid).await? {
            // 이미 만든 OSD(이전 Task가 유닛을 기다리다 멈춤 등): 그 OSD만 켠다(이 노드의 다른 OSD는 건드리지
            // 않는다).
            let id = osd.id.to_string();
            // `lvm create`가 도중에 멈춰 켤 수 없는 OSD면 손으로 치우게 한다.
            self.run_ok(&self.volume(&["lvm", "activate", &id, &osd.fsid]))
                .await
                .map_err(|e| {
                    CephError::Command(format!(
                        "{e}; osd.{id} on {path} may be half made — remove it by hand with: ceph \
                         osd purge {id} --yes-i-really-mean-it; ceph-volume lvm zap --destroy \
                         {path}"
                    ))
                })?;
            self.wait_osd(osd.id).await?;
            return Ok(osd.id);
        }
        match &disk.state {
            DiskState::InUse { reason } => {
                return Err(CephError::Command(format!(
                    "{path} cannot hold an OSD: {reason}"
                )));
            }
            DiskState::Wipeable { reasons } if !wipe => {
                return Err(CephError::Command(format!(
                    "{path} is not empty ({}); pass wipe to erase it",
                    reasons.join(", ")
                )));
            }
            DiskState::Wipeable { .. } => {
                self.run_ok(
                    &self
                        .volume(&["lvm", "zap", "--destroy", &path])
                        .timeout(ZAP_TIMEOUT),
                )
                .await?;
            }
            DiskState::Available => {}
        }
        self.run_ok(
            &self
                .volume(&["lvm", "create", "--bluestore", "--data", &path])
                .timeout(OSD_CREATE_TIMEOUT),
        )
        .await?;
        let out = self
            .run_ok(&self.volume(&["lvm", "list", &path, "--format", "json"]))
            .await?;
        let osds = parse_volume_list(&out.stdout).map_err(CephError::Command)?;
        let osd = osds
            .first()
            .ok_or_else(|| CephError::Command(format!("ceph-volume made no OSD on {path}")))?;
        if osd.cluster_fsid != fsid.to_string() {
            return Err(CephError::Command(format!(
                "the OSD on {path} belongs to Ceph cluster {}, not {fsid}",
                osd.cluster_fsid
            )));
        }
        self.wait_osd(osd.id).await?;
        Ok(osd.id)
    }

    /// `path`에 이 클러스터의 OSD가 이미 있으면 그 OSD
    async fn our_osd(
        &self,
        path: &str,
        fsid: Uuid,
    ) -> Result<Option<super::json::VolumeOsd>, CephError> {
        let Some(out) = self
            .run_optional(&self.volume(&["lvm", "list", "--format", "json"]))
            .await?
        else {
            return Ok(None);
        };
        Ok(parse_volume_list(&out.stdout)
            .map_err(CephError::Command)?
            .into_iter()
            .find(|o| o.cluster_fsid == fsid.to_string() && o.devices.iter().any(|d| d == path)))
    }

    /// OSD `id`의 유닛이 active가 되고 그 OSD가 `ceph osd tree`에서 up이 될 때까지 기다린다(유닛이 켜진 뒤에도
    /// OSD가 mon에 올라 up이 되기까지 시간이 걸린다). mon·mgr이 없는 OSD 노드에는 admin 키링이 없으므로
    /// `client.orbivisor`(클라이언트 설정, mon caps `profile rbd`가 osd map을 읽을 수 있다)로 묻는다.
    async fn wait_osd(&self, id: u32) -> Result<(), CephError> {
        self.wait_unit(
            &format!("ceph-osd@{id}"),
            "the OSD unit to become active",
            self.options.wait_limit.min(OSD_ACTIVE_LIMIT),
        )
        .await?;
        self.wait_for(
            "the OSD to come up",
            self.options.wait_limit,
            move || async move {
                self.client_json(&["osd", "tree"])
                    .await
                    .ok()
                    .and_then(|out| parse_osd_tree(&out).ok())
                    .is_some_and(|osds| osds.iter().any(|o| o.id == id && o.up))
            },
        )
        .await
        .map_err(|e| match e {
            CephError::Timeout(_) => CephError::Command(format!(
                "OSD {id} did not come up: its unit is active but ceph osd tree does not show it up"
            )),
            e => e,
        })
    }

    /// Ceph 풀을 만들고(`size`, 1이면 명시한 단일 사본) PG가 모두 active가 될 때까지 기다린 뒤 RBD용으로
    /// 준비한다. 만든 풀에는 곧바로 rbd application의 표시 `orbivisor-pool = {name}`을 단다. 같은 이름의 Ceph 풀이
    /// 이미 있으면, application이 없거나(만든 뒤 표시 전에 멈춤) 그 표시가 `name`일 때만 앞 Task가 만들다 멈춘 풀로
    /// 보고 이어서 한다(복제 수를 다시 맞추고 PG를 기다리고 `rbd pool init`). 다른 풀(다른 application, 표시 없는
    /// rbd 풀, 다른 Orbivisor 풀의 표시)은 바꾸지 않고 실패한다 — 쓰는 풀의 복제 수를 바꾸지 않게. `.mgr` 풀의 복제
    /// 수가 호스트 수보다 크면 낮춘다(호스트가 적은 클러스터가 HEALTH_WARN에 머물지 않게).
    pub async fn create_pool(&self, plan: &PoolPlan) -> Result<(), CephError> {
        let pool = plan.pool.as_str();
        let existing = parse_pool_ls(&self.ceph_json(&["osd", "pool", "ls"]).await?)
            .map_err(CephError::Command)?;
        let resume = existing.iter().any(|p| p.name == pool);
        let mut marked = false;
        if resume {
            let found = parse_pool_ls(&self.ceph_json(&["osd", "pool", "ls", "detail"]).await?)
                .map_err(CephError::Command)?
                .into_iter()
                .find(|p| p.name == pool);
            let (apps, mark) = found
                .map(|p| (p.applications, p.orbivisor_pool))
                .unwrap_or_default();
            marked = mark.as_deref() == Some(plan.name.as_str());
            if !apps.is_empty() && !marked {
                let used = match mark {
                    Some(other) => format!("Orbivisor storage pool {other}"),
                    None => apps.join(", "),
                };
                return Err(CephError::Command(format!(
                    "Ceph pool {pool} already exists for {used}; Orbivisor does not change it — \
                     choose another Ceph pool name"
                )));
            }
        }
        if plan.size == 1 {
            self.ceph(&["config", "set", "global", "mon_allow_pool_size_one", "true"])
                .await?;
        }
        if !resume {
            self.ceph(&["osd", "pool", "create", pool]).await?;
        }
        if !marked {
            // 표시는 application이 켜져 있어야 달 수 있다.
            self.ceph(&["osd", "pool", "application", "enable", pool, "rbd"])
                .await?;
            self.ceph(&[
                "osd",
                "pool",
                "application",
                "set",
                pool,
                "rbd",
                super::json::ORBIVISOR_POOL_KEY,
                &plan.name,
            ])
            .await?;
        }
        self.set_pool_size(pool, plan.size).await?;
        let target = plan.osd_hosts.clamp(1, 3);
        let mgr_size = parse_pool_ls(&self.ceph_json(&["osd", "pool", "ls", "detail"]).await?)
            .map_err(CephError::Command)?
            .into_iter()
            .find(|p| p.name == ".mgr")
            .and_then(|p| p.size);
        // 사본 하나로 낮추는 것은 사용자가 size 1을 명시했을 때만이다.
        if let Some(size) = mgr_size
            && size > target
            && (target > 1 || plan.size == 1)
        {
            self.set_pool_size(".mgr", target as u8).await?;
        }
        // `rbd pool init`은 풀에 쓰므로 PG가 active가 된 뒤에 부른다. 클러스터 전체(`pg stat`)가 아니라 이
        // 풀의 PG를 본다 — 막 만든 풀은 mgr가 아직 몰라 전체 합계에 없을 수 있다.
        self.wait_for(
            "the placement groups to become active",
            self.options.wait_limit,
            move || async move {
                self.ceph_json(&["pg", "ls-by-pool", pool])
                    .await
                    .ok()
                    .and_then(|out| parse_pg_ls(&out).ok())
                    .is_some_and(|(total, active)| total > 0 && active == total)
            },
        )
        .await?;
        self.run_ok(&Cmd::new("rbd", &["pool", "init", pool]))
            .await?;
        Ok(())
    }

    async fn set_pool_size(&self, pool: &str, size: u8) -> Result<(), CephError> {
        let size = size.to_string();
        let mut args = vec!["osd", "pool", "set", pool, "size", size.as_str()];
        if size == "1" {
            args.push("--yes-i-really-mean-it");
        }
        self.ceph(&args).await.map(drop)
    }

    /// 상태 보고서. 명령 여덟을 함께 돌리고 하나라도 실패하면 실패다. `pools`는 (Orbivisor 풀 이름, Ceph
    /// 풀 이름)이다. `read_at`·`via`는 부르는 쪽이 채운다.
    pub async fn report(&self, pools: &[(String, String)]) -> Result<CephReport, CephError> {
        let (status, quorum, mgr, tree, osd_df, df, metadata, pool_ls) = tokio::join!(
            self.ceph_read(&["status"]),
            self.ceph_read(&["quorum_status"]),
            self.ceph_read(&["mgr", "dump"]),
            self.ceph_read(&["osd", "tree"]),
            self.ceph_read(&["osd", "df"]),
            self.ceph_read(&["df"]),
            self.ceph_read(&["osd", "metadata"]),
            self.ceph_read(&["osd", "pool", "ls", "detail"]),
        );
        let input = ReportInput {
            status: &status?,
            quorum_status: &quorum?,
            mgr_dump: &mgr?,
            osd_tree: &tree?,
            osd_df: &osd_df?,
            df: &df?,
            osd_metadata: &metadata?,
            pool_ls: &pool_ls?,
        };
        report(&input, pools).map_err(CephError::Command)
    }

    /// `ceph --connect-timeout 10 {args}`의 표준 출력. 실패하면 `ceph {args}: {stderr 첫 줄}`이다.
    pub async fn ceph(&self, args: &[&str]) -> Result<Vec<u8>, CephError> {
        self.ceph_with(args, super::runner::DEFAULT_TIMEOUT).await
    }

    async fn ceph_json(&self, args: &[&str]) -> Result<Vec<u8>, CephError> {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        self.ceph(&all).await
    }

    async fn ceph_read(&self, args: &[&str]) -> Result<Vec<u8>, CephError> {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        self.ceph_with(&all, REPORT_TIMEOUT).await
    }

    async fn ceph_with(&self, args: &[&str], timeout: Duration) -> Result<Vec<u8>, CephError> {
        self.ceph_as(None, args, timeout).await
    }

    /// `client.orbivisor`(클라이언트 설정)로 `ceph … --format json`. admin 키링이 없는 노드에서 읽기만 한다.
    async fn client_json(&self, args: &[&str]) -> Result<Vec<u8>, CephError> {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        let conf = path_str(&self.client_conf());
        self.ceph_as(Some(&conf), &all, super::runner::DEFAULT_TIMEOUT)
            .await
    }

    /// `ceph`를 admin(`conf`가 없으면 — `/etc/ceph`)이나 `client.orbivisor`(`conf`)로 부른다.
    async fn ceph_as(
        &self,
        conf: Option<&str>,
        args: &[&str],
        timeout: Duration,
    ) -> Result<Vec<u8>, CephError> {
        let mut all = vec![];
        if let Some(conf) = conf {
            all.extend(["--conf", conf, "--id", CLIENT_ID]);
        }
        all.extend(["--connect-timeout", CONNECT_TIMEOUT]);
        all.extend(args);
        let out = self.exec(&Cmd::new("ceph", &all).timeout(timeout)).await?;
        if out.success() {
            return Ok(out.stdout);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(|| format!("exit status {}", out.status), str::to_string);
        Err(CephError::Command(format!(
            "ceph {}: {first}",
            args.join(" ")
        )))
    }

    /// `ceph-volume {args}`(루프 장치를 허락하면 `CEPH_VOLUME_ALLOW_LOOP_DEVICES=1`)
    fn volume(&self, args: &[&str]) -> Cmd {
        let cmd = Cmd::new("ceph-volume", args);
        if self.options.allow_loop {
            cmd.env("CEPH_VOLUME_ALLOW_LOOP_DEVICES", "1")
        } else {
            cmd
        }
    }

    /// `chown [-R] ceph:ceph {path}`(사용자 `ceph`는 꾸러미가 만든다)
    async fn chown(&self, path: &Path, recursive: bool) -> Result<(), CephError> {
        let path = path_str(path);
        let mut args = vec![];
        if recursive {
            args.push("-R");
        }
        args.extend(["ceph:ceph", path.as_str()]);
        self.run_ok(&Cmd::new("chown", &args)).await.map(drop)
    }

    /// 명령을 돌린다. 시간이 넘으면 그 명령 줄을 담아 실패한다(명령 줄에는 비밀이 없다 — 키는 파일로 준다).
    async fn exec(&self, cmd: &Cmd) -> Result<CmdOutput, CephError> {
        match self.runner.run(cmd).await {
            Err(DriverError::Timeout(_)) => Err(CephError::Command(format!(
                "{} timed out after {}s",
                cmd.line(),
                cmd.timeout.as_secs()
            ))),
            other => Ok(other?),
        }
    }

    /// 명령을 돌려 종료 0이 아니면 `{명령 줄} failed: {stderr 끝 줄}`로 실패한다.
    async fn run_ok(&self, cmd: &Cmd) -> Result<CmdOutput, CephError> {
        let out = self.exec(cmd).await?;
        if out.success() {
            return Ok(out);
        }
        Err(CephError::Command(format!(
            "{} failed: {}",
            cmd.line(),
            tail(&String::from_utf8_lossy(&out.stderr), ERROR_LINES)
                .unwrap_or_else(|| format!("exit status {}", out.status))
        )))
    }

    /// [`run_ok`](Self::run_ok)와 같되 프로그램이 없으면 `None`이다.
    async fn run_optional(&self, cmd: &Cmd) -> Result<Option<CmdOutput>, CephError> {
        match self.run_ok(cmd).await {
            Err(CephError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            other => other.map(Some),
        }
    }

    /// 유닛 `unit`이 active가 될 때까지 기다린다.
    async fn wait_unit(
        &self,
        unit: &str,
        what: &'static str,
        limit: Duration,
    ) -> Result<(), CephError> {
        self.wait_for(what, limit, move || async move {
            self.runner
                .run(&Cmd::new("systemctl", &["is-active", unit]))
                .await
                .is_ok_and(|out| out.success())
        })
        .await
    }

    /// `check`가 참이 될 때까지 `wait_step`마다 다시 본다. `limit`이 지나면 `Timeout(what)`이다. (`AsyncFnMut`
    /// 클로저의 future는 `tokio::spawn`이 요구하는 `Send`를 갖지 못해 future를 돌려주는 클로저를 받는다.)
    async fn wait_for<F: Future<Output = bool>>(
        &self,
        what: &'static str,
        limit: Duration,
        mut check: impl FnMut() -> F,
    ) -> Result<(), CephError> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if check().await {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CephError::Timeout(what));
            }
            tokio::time::sleep(self.options.wait_step).await;
        }
    }

    /// `/dev/disk/by-id`의 (링크, 가리키는 장치) — 링크의 상대 경로를 풀어서
    fn by_id_links(&self) -> Result<Vec<(String, String)>, CephError> {
        let dir = &self.paths.by_id;
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_at(dir, e)),
        };
        let mut links: Vec<(String, String)> = entries
            .filter_map(Result::ok)
            .filter_map(|e| {
                let link = e.path();
                let target = std::fs::read_link(&link).ok()?;
                Some((path_str(&link), path_str(&normalize(&dir.join(target)))))
            })
            .collect();
        links.sort();
        Ok(links)
    }

    /// 장치 경로를 디스크 경로로: by-id 링크면 가리키는 장치, 있는 경로면 실제 경로, 아니면 그대로
    fn resolve_device(&self, device: &str) -> Result<String, CephError> {
        if let Some((_, target)) = self.by_id_links()?.into_iter().find(|(l, _)| l == device) {
            return Ok(target);
        }
        Ok(std::fs::canonicalize(device).map_or_else(|_| device.to_string(), |p| path_str(&p)))
    }
}

/// `..`·`.`을 글자로 푼 경로(`/dev/disk/by-id/../../sdb` → `/dev/sdb`)
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 비지 않은 끝 `n`줄(모두 비었으면 `None`)
fn tail(text: &str, n: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[lines.len().saturating_sub(n)..].join("\n"))
}

fn io_at(path: &Path, e: std::io::Error) -> CephError {
    CephError::Io(std::io::Error::new(
        e.kind(),
        format!("{}: {e}", path.display()),
    ))
}

/// 파일 내용(없으면 `None`)
fn read_optional(path: &Path) -> Result<Option<String>, CephError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(path, e)),
    }
}

/// 디렉터리를 만들고(부모까지) 모드를 맞춘다.
fn make_dir(path: &Path, mode: u32) -> Result<(), CephError> {
    std::fs::create_dir_all(path).map_err(|e| io_at(path, e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io_at(path, e))
}

/// 디렉터리를 통째로 지운다(없으면 그대로).
fn remove_dir(path: &Path) -> Result<(), CephError> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_at(path, e)),
        _ => Ok(()),
    }
}

/// 같은 디렉터리의 고유한 임시 파일 이름(`.{name}.opl-tmp-{pid}-{n}`) — 에이전트와 Task가 같은 파일을 함께
/// 써도 서로의 임시 파일을 덮지 않는다.
fn temp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or(Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{name}.opl-tmp-{}-{n}", std::process::id()))
}

/// 파일을 임시 파일 → fsync → rename으로 바꾼다(부모 디렉터리도 만든다). 내용과 모드가 같으면 그대로
/// 두고 `false`다.
fn write_file(path: &Path, content: &[u8], mode: u32) -> Result<bool, CephError> {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o7777 == mode
        && std::fs::read(path).is_ok_and(|old| old == content)
    {
        return Ok(false);
    }
    let parent = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
    let tmp = temp_path(path);
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        // umask나 이미 있던 임시 파일과 상관없이 모드를 맞춘다.
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.write_all(content)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_at(path, e)
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use orbivisor_types::ceph::CephMon;

    use super::*;
    use crate::ceph::conf::{render_client_conf, render_conf};
    use crate::ceph::runner::FakeRunner;

    const FSID: &str = "18796e2f-112c-44cf-9d48-09f825f0a14b";
    const VERSION: &str =
        "ceph version 19.2.3 (c92aebb279828e9c3c1f5d24613efca272649e62) squid (stable)\n";
    const ROOT: &[u8] = include_bytes!("fixtures/lsblk-root.json");
    const LVM: &[u8] = include_bytes!("fixtures/lsblk-lvm.json");
    const LOOP: &[u8] = include_bytes!("fixtures/lsblk-loop.json");
    const PVS: &[u8] = include_bytes!("fixtures/pvs.json");
    const VOLUME_LIST: &[u8] = include_bytes!("fixtures/ceph-volume-list.json");
    const NO_LVS: &str = r#"{"report":[{"lv":[]}],"log":[]}"#;

    fn fsid() -> Uuid {
        FSID.parse().unwrap()
    }

    fn keys() -> CephKeys {
        CephKeys {
            mon: "AQmon".into(),
            admin: "AQadmin".into(),
            bootstrap_osd: "AQosd".into(),
            client: "AQclient".into(),
        }
    }

    /// 명령을 가짜 실행기에 넘기고, `ceph-mon --mkfs`가 불릴 때 그 키링 파일의 내용을 남긴다.
    #[derive(Default)]
    struct Spy {
        fake: FakeRunner,
        keyrings: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CommandRunner for Spy {
        async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
            if cmd.program == "ceph-mon"
                && let Some(i) = cmd.args.iter().position(|a| a == "--keyring")
            {
                let text = std::fs::read_to_string(&cmd.args[i + 1]).unwrap();
                self.keyrings.lock().unwrap().push(text);
            }
            self.fake.run(cmd).await
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        root: PathBuf,
        spy: Arc<Spy>,
        host: CephHost,
    }

    impl Env {
        fn new() -> Self {
            Self::with(|_| {})
        }

        fn with(change: impl FnOnce(&mut CephOptions)) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let paths = CephPaths {
                etc_ceph: root.join("etc/ceph"),
                var_lib_ceph: root.join("var/lib/ceph"),
                client_dir: root.join("etc/opl/ceph"),
                by_id: root.join("dev/disk/by-id"),
            };
            let mut options = CephOptions {
                wait_step: Duration::from_millis(1),
                wait_limit: Duration::from_millis(200),
                any_os: true,
                ..CephOptions::default()
            };
            change(&mut options);
            let spy = Arc::new(Spy::default());
            let host = CephHost::new(spy.clone(), paths, options);
            Self {
                _dir: dir,
                root,
                spy,
                host,
            }
        }

        fn fake(&self) -> &FakeRunner {
            &self.spy.fake
        }

        fn calls(&self) -> Vec<String> {
            self.fake().calls()
        }

        /// 기록 가운데 `prefix`로 시작하는 줄
        fn calls_of(&self, prefix: &str) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|c| c.starts_with(prefix))
                .collect()
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn p(&self, rel: &str) -> String {
            path_str(&self.path(rel))
        }

        fn installed(&self, packages: &[&str]) {
            let out: String = packages
                .iter()
                .map(|p| format!("{p} install ok installed\n"))
                .collect();
            self.fake().reply("dpkg-query", CmdOutput::ok(out));
            self.fake().reply("ceph --version", CmdOutput::ok(VERSION));
        }
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// 다 깔려 있으면 apt를 부르지 않고 판을 돌려준다.
    #[tokio::test]
    async fn ensure_packages_skips_apt_when_everything_is_installed() {
        let env = Env::new();
        env.installed(&["ceph-common", "qemu-block-extra"]);
        let v = env
            .host
            .ensure_packages(&strings(&["ceph-common", "qemu-block-extra"]))
            .await
            .unwrap();
        assert_eq!(v.release(), "19.2.3 squid");
        assert_eq!(
            env.calls(),
            [
                "dpkg-query -W -f ${Package} ${Status}\n ceph-common qemu-block-extra",
                "ceph --version",
            ]
        );
    }

    /// 빠진 꾸러미만 `update` 뒤 `install`한다(비대화식, 설치 한도). 지운 뒤 설정만 남은 꾸러미도 빠진
    /// 것이다.
    #[tokio::test]
    async fn ensure_packages_installs_only_the_missing_ones() {
        let env = Env::with(|o| o.install_timeout = Duration::from_secs(77));
        env.fake().reply(
            "dpkg-query",
            CmdOutput {
                status: 1,
                stdout: b"ceph-common install ok installed\nceph-mon deinstall ok config-files\n"
                    .to_vec(),
                stderr: b"dpkg-query: no packages found matching qemu-block-extra\n".to_vec(),
            },
        );
        env.fake().reply("ceph --version", CmdOutput::ok(VERSION));
        env.host
            .ensure_packages(&strings(&["ceph-mon", "ceph-common", "qemu-block-extra"]))
            .await
            .unwrap();
        let calls = env.calls();
        assert_eq!(
            calls[1..],
            [
                "apt-get update -q -o DPkg::Lock::Timeout=600",
                "apt-get install -y -q -o DPkg::Lock::Timeout=600 ceph-mon qemu-block-extra",
                "ceph --version",
            ]
        );
        for cmd in &env.fake().commands()[1..3] {
            assert_eq!(
                cmd.env,
                [("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string())]
            );
            assert_eq!(cmd.timeout, Duration::from_secs(77));
        }
    }

    /// apt가 실패하면 명령과 stderr 끝 다섯 줄. Ceph 18 미만은 거절한다.
    #[tokio::test]
    async fn ensure_packages_reports_apt_failures_and_old_releases() {
        let env = Env::new();
        env.fake().reply("dpkg-query", CmdOutput::ok(""));
        env.fake().reply(
            "apt-get install",
            CmdOutput::fail(
                100,
                "l1\nl2\nl3\nl4\nl5\nE: Unable to locate package ceph-volume\n",
            ),
        );
        let err = env
            .host
            .ensure_packages(&strings(&["ceph-volume"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "apt-get install -y -q -o DPkg::Lock::Timeout=600 ceph-volume failed: \
             l2\nl3\nl4\nl5\nE: Unable to locate package ceph-volume"
        );
        assert!(matches!(err, CephError::Command(_)));

        let env = Env::new();
        env.installed(&["ceph-common"]);
        env.fake().reply(
            "ceph --version",
            CmdOutput::ok(
                "ceph version 17.2.7 (b12291d110049b2f35e32e0de30d70e9a4c060d2) quincy (stable)\n",
            ),
        );
        let err = env
            .host
            .ensure_packages(&strings(&["ceph-common"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ceph 17.2.7 quincy is too old; Orbivisor sets up Ceph 18 (reef) or newer"
        );
    }

    #[test]
    fn by_id_links_resolve_relative_targets() {
        assert_eq!(
            normalize(Path::new("/dev/disk/by-id/../../sdb")),
            PathBuf::from("/dev/sdb")
        );
        assert_eq!(
            normalize(Path::new("/dev/disk/by-id/./../../nvme0n1")),
            PathBuf::from("/dev/nvme0n1")
        );
    }

    #[test]
    fn release_must_match_the_cluster() {
        let have = parse_version(VERSION).unwrap();
        check_release("node2", &have, None).unwrap();
        check_release("node2", &have, Some("19.2.1 squid")).unwrap();
        let err = check_release("node2", &have, Some("18.2.7 reef")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "node \"node2\" has ceph 19.2.3 squid but the cluster runs 18.2.7 reef; \
             upgrade the packages first"
        );
    }

    #[test]
    fn linux_only_unless_tests_allow_any_os() {
        let env = Env::with(|o| o.any_os = false);
        let result = env.host.check_platform();
        if cfg!(target_os = "linux") {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "Ceph can be set up only on Linux nodes"
            );
        }
        Env::new().host.check_platform().unwrap();
    }

    /// 표시 없는 `/etc/ceph/ceph.conf`, 다른 fsid의 mon·mgr 디렉터리, 다른 클러스터의 OSD 디렉터리면
    /// 아무 명령 없이 실패한다. 빈 디렉터리와 우리 것은 통과한다. 남의 conf는 쓰지도 않는다.
    #[tokio::test]
    async fn foreign_ceph_is_left_alone() {
        let env = Env::new();
        env.host.check_foreign("node1", fsid()).unwrap();
        std::fs::create_dir_all(env.path("etc/ceph")).unwrap();
        std::fs::write(env.path("etc/ceph/ceph.conf"), "[global]\nfsid = x\n").unwrap();
        let err = env.host.check_foreign("node1", fsid()).unwrap_err();
        assert!(matches!(err, CephError::Foreign(_)));
        assert_eq!(
            err.to_string(),
            format!(
                "node \"node1\" already has {} not written by Orbivisor (another Ceph cluster?); \
                 Orbivisor does not adopt an existing Ceph — use it as an rbd storage pool \
                 instead, or move the file away",
                env.p("etc/ceph/ceph.conf")
            )
        );
        assert!(matches!(
            env.host
                .write_conf("# managed by orbivisord; edits are overwritten\n"),
            Err(CephError::Foreign(_))
        ));
        assert_eq!(
            std::fs::read_to_string(env.path("etc/ceph/ceph.conf")).unwrap(),
            "[global]\nfsid = x\n"
        );

        // 우리 conf, 빈 디렉터리, 우리 fsid
        let env = Env::new();
        env.host
            .write_conf("# managed by orbivisord; edits are overwritten\n[global]\n")
            .unwrap();
        for dir in [
            "mon/ceph-node1",
            "mgr/ceph-node1",
            "osd/ceph-0",
            "osd/ceph-1",
        ] {
            std::fs::create_dir_all(env.path(&format!("var/lib/ceph/{dir}"))).unwrap();
        }
        std::fs::write(
            env.path("var/lib/ceph/mon/ceph-node1/fsid"),
            format!("{FSID}\n"),
        )
        .unwrap();
        std::fs::write(
            env.path("var/lib/ceph/osd/ceph-0/ceph_fsid"),
            format!("{FSID}\n"),
        )
        .unwrap();
        std::fs::write(env.path("var/lib/ceph/osd/ceph-0/fsid"), "osd-uuid\n").unwrap();
        env.host.check_foreign("node1", fsid()).unwrap();

        // fsid가 없는(알 수 없는) 디렉터리와 다른 fsid
        std::fs::write(env.path("var/lib/ceph/osd/ceph-1/whoami"), "1\n").unwrap();
        assert_eq!(
            env.host
                .check_foreign("node1", fsid())
                .unwrap_err()
                .to_string(),
            format!(
                "node \"node1\": {} belongs to another Ceph cluster",
                env.p("var/lib/ceph/osd/ceph-1")
            )
        );
        std::fs::remove_dir_all(env.path("var/lib/ceph/osd/ceph-1")).unwrap();
        std::fs::write(env.path("var/lib/ceph/mgr/ceph-node1/fsid"), "0b5c6f39\n").unwrap();
        assert!(
            env.host
                .check_foreign("node1", fsid())
                .unwrap_err()
                .to_string()
                .ends_with("mgr/ceph-node1 belongs to another Ceph cluster")
        );
        assert!(env.calls().is_empty());
    }

    /// 파일은 모드를 맞춰 쓰고, 내용이 같으면 다시 쓰지 않는다. bootstrap-osd 키링은 ceph 소유다.
    #[tokio::test]
    async fn writes_config_files_and_keyrings() {
        let env = Env::new();
        let spec = orbivisor_types::ceph::CephSpec {
            fsid: fsid(),
            public_network: "10.0.0.0/24".parse().unwrap(),
            cluster_network: None,
            keys: keys(),
        };
        let mons = [CephMon {
            node: Uuid::nil(),
            name: "node1".into(),
            addr: "10.0.0.1".parse().unwrap(),
        }];
        let conf = render_conf(&spec, &mons, None);
        assert!(env.host.write_conf(&conf).unwrap());
        assert!(!env.host.write_conf(&conf).unwrap());
        assert_eq!(
            std::fs::read_to_string(env.path("etc/ceph/ceph.conf")).unwrap(),
            conf
        );
        assert_eq!(mode(&env.path("etc/ceph/ceph.conf")), 0o644);

        let client = render_client_conf(&spec, &mons, &path_str(&env.host.client_keyring()));
        assert!(env.host.write_client(&client, "AQclient").unwrap());
        assert!(!env.host.write_client(&client, "AQclient").unwrap());
        assert_eq!(env.host.client_conf(), env.path("etc/opl/ceph/ceph.conf"));
        assert_eq!(
            std::fs::read_to_string(env.path("etc/opl/ceph/ceph.client.orbivisor.keyring"))
                .unwrap(),
            "[client.orbivisor]\n\tkey = AQclient\n"
        );
        assert_eq!(mode(&env.host.client_keyring()), 0o600);
        assert!(env.host.write_client(&client, "AQother").unwrap());

        assert!(env.host.write_admin_keyring("AQadmin").unwrap());
        assert_eq!(
            std::fs::read_to_string(env.path("etc/ceph/ceph.client.admin.keyring")).unwrap(),
            "[client.admin]\n\tkey = AQadmin\n\tcaps mon = \"allow *\"\n\tcaps osd = \"allow *\"\n\
             \tcaps mds = \"allow *\"\n\tcaps mgr = \"allow *\"\n"
        );
        assert_eq!(mode(&env.path("etc/ceph/ceph.client.admin.keyring")), 0o600);

        assert!(env.host.write_bootstrap_osd("AQosd").await.unwrap());
        assert!(!env.host.write_bootstrap_osd("AQosd").await.unwrap());
        let path = env.path("var/lib/ceph/bootstrap-osd/ceph.keyring");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[client.bootstrap-osd]\n\tkey = AQosd\n\tcaps mon = \"profile bootstrap-osd\"\n\
             \tcaps mgr = \"allow r\"\n"
        );
        assert_eq!(mode(&path), 0o600);
        // chown은 내용이 같아도 늘 한다(앞 Task가 쓴 뒤 chown 전에 멈췄을 수 있다).
        assert_eq!(
            env.calls(),
            [
                format!("chown ceph:ceph {}", path_str(&path)),
                format!("chown ceph:ceph {}", path_str(&path))
            ]
        );
        // 임시 파일은 남지 않는다.
        let left: Vec<_> = std::fs::read_dir(env.path("etc/ceph"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(left.iter().all(|n| !n.contains("opl-tmp")), "{left:?}");
    }

    fn mon_init<'a>(keys: &'a CephKeys, monmap: MonmapSource) -> MonInit<'a> {
        MonInit {
            name: "node1",
            addr: "10.0.0.1".parse().unwrap(),
            fsid: fsid(),
            keys,
            monmap,
            init_node: true,
        }
    }

    /// 부트스트랩 전체 차례: 설치 확인 → monmap → mkfs(임시 디렉터리, 키링 네 항목) → mon 켜기 → 정족수
    /// 기다림(처음엔 빔) → mgr 키 → mgr 켜기 → mgr available. 다시 하면 mkfs를 건너뛴다.
    #[tokio::test]
    async fn bootstraps_a_monitor_and_a_manager() {
        let env = Env::new();
        let fake = env.fake();
        env.installed(DAEMON_PACKAGES);
        fake.reply_seq(
            "ceph --connect-timeout 10 quorum_status",
            vec![
                CmdOutput::ok(r#"{"quorum_names":[]}"#),
                CmdOutput::fail(1, "[errno 110] RADOS timed out"),
                CmdOutput::ok(r#"{"quorum_names":["node1"]}"#),
            ],
        );
        fake.reply(
            "ceph --connect-timeout 10 mgr stat",
            CmdOutput::ok(r#"{"epoch":5,"available":true,"active_name":"node1","num_standby":0}"#),
        );
        let keys = keys();
        env.host.check_foreign("node1", fsid()).unwrap();
        env.host
            .ensure_packages(&env.host.options().packages)
            .await
            .unwrap();
        assert!(
            env.host
                .create_monitor(&mon_init(&keys, MonmapSource::New))
                .await
                .unwrap()
        );
        env.host.start_monitor("node1").await.unwrap();
        env.host.wait_quorum("node1", &[]).await.unwrap();
        env.host.create_manager("node1", fsid()).await.unwrap();
        env.host.wait_manager().await.unwrap();

        let tmp = env.p("var/lib/ceph/tmp/opl-mon-node1");
        let mgr = env.p("var/lib/ceph/mgr/ceph-node1");
        assert_eq!(
            env.calls(),
            [
                "dpkg-query -W -f ${Package} ${Status}\n ceph-mon ceph-mgr ceph-osd ceph-volume \
                 ceph-common lvm2 qemu-block-extra"
                    .to_string(),
                "ceph --version".into(),
                format!(
                    "monmaptool --create --addv node1 [v2:10.0.0.1:3300,v1:10.0.0.1:6789] \
                     --fsid {FSID} {tmp}/monmap"
                ),
                format!("chown -R ceph:ceph {tmp}"),
                format!(
                    "ceph-mon --mkfs -i node1 --mon-data {tmp}/mon --monmap {tmp}/monmap \
                     --keyring {tmp}/keyring --setuser ceph --setgroup ceph"
                ),
                "systemctl enable ceph.target ceph-mon.target".into(),
                "systemctl enable --now ceph-mon@node1".into(),
                "ceph --connect-timeout 10 quorum_status --format json".into(),
                "ceph --connect-timeout 10 quorum_status --format json".into(),
                "ceph --connect-timeout 10 quorum_status --format json".into(),
                format!(
                    "ceph --connect-timeout 10 auth get-or-create mgr.node1 mon allow profile mgr \
                     osd allow * mds allow * -o {mgr}/keyring"
                ),
                format!("chown -R ceph:ceph {mgr}"),
                "systemctl enable ceph-mgr.target".into(),
                "systemctl enable --now ceph-mgr@node1".into(),
                "systemctl is-active ceph-mgr@node1".into(),
                "ceph --connect-timeout 10 mgr stat --format json".into(),
            ]
        );
        // mkfs가 auth DB에 넣는 키링: 네 항목과 caps
        assert_eq!(
            env.spy.keyrings.lock().unwrap().as_slice(),
            ["[mon.]\n\tkey = AQmon\n\tcaps mon = \"allow *\"\n\
              [client.admin]\n\tkey = AQadmin\n\tcaps mon = \"allow *\"\n\tcaps osd = \"allow *\"\n\
              \tcaps mds = \"allow *\"\n\tcaps mgr = \"allow *\"\n\
              [client.bootstrap-osd]\n\tkey = AQosd\n\tcaps mon = \"profile bootstrap-osd\"\n\
              \tcaps mgr = \"allow r\"\n\
              [client.orbivisor]\n\tkey = AQclient\n\tcaps mon = \"profile rbd\"\n\
              \tcaps osd = \"profile rbd\"\n\tcaps mgr = \"profile rbd\"\n"]
        );
        // 임시 디렉터리(키 포함)는 지우고, 저장소는 표시와 함께 제자리에
        assert!(!env.path("var/lib/ceph/tmp/opl-mon-node1").exists());
        let store = env.path("var/lib/ceph/mon/ceph-node1");
        assert_eq!(
            std::fs::read_to_string(store.join("done")).unwrap(),
            "init\n"
        );
        assert_eq!(
            std::fs::read_to_string(store.join("fsid")).unwrap(),
            format!("{FSID}\n")
        );
        assert_eq!(mode(&store), 0o750);
        assert_eq!(
            std::fs::read_to_string(env.path("var/lib/ceph/mgr/ceph-node1/fsid")).unwrap(),
            format!("{FSID}\n")
        );
        // 만든 저장소는 우리 것이다.
        env.host.check_foreign("node1", fsid()).unwrap();

        // 다시 하기: `done`이 있으면 mkfs를 건너뛴다.
        let before = env.calls().len();
        assert!(
            !env.host
                .create_monitor(&mon_init(&keys, MonmapSource::New))
                .await
                .unwrap()
        );
        assert_eq!(env.calls().len(), before);
    }

    /// mkfs가 실패하면 임시 디렉터리를 지우고 mon 디렉터리는 만들지 않는다(다시 하면 처음부터).
    #[tokio::test]
    async fn a_failed_mkfs_leaves_no_monitor_directory() {
        let env = Env::new();
        env.fake()
            .reply("ceph-mon --mkfs", CmdOutput::fail(1, "mkfs failed\n"));
        let keys = keys();
        let err = env
            .host
            .create_monitor(&mon_init(&keys, MonmapSource::New))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("failed: mkfs failed"), "{err}");
        assert!(!env.path("var/lib/ceph/mon/ceph-node1").exists());
        assert!(!env.path("var/lib/ceph/tmp/opl-mon-node1").exists());
        env.host.check_foreign("node1", fsid()).unwrap();
    }

    /// 기존 정족수에 mon 더하기: monmap을 받아 이 mon을 더한다(이미 있으면 `--addv`를 건너뜀). 키링은
    /// `mon.`뿐이다.
    #[tokio::test]
    async fn adds_a_monitor_to_an_existing_quorum() {
        let keys = keys();
        let env = Env::new();
        env.fake().reply(
            "monmaptool --print",
            CmdOutput::ok(
                "epoch 1\nfsid x\n0: [v2:10.0.0.2:3300/0,v1:10.0.0.2:6789/0] mon.node2\n",
            ),
        );
        env.host
            .create_monitor(&mon_init(&keys, MonmapSource::Existing))
            .await
            .unwrap();
        let tmp = env.p("var/lib/ceph/tmp/opl-mon-node1");
        assert_eq!(
            env.calls()[..3],
            [
                format!("ceph --connect-timeout 10 mon getmap -o {tmp}/monmap"),
                format!("monmaptool --print {tmp}/monmap"),
                format!("monmaptool --addv node1 [v2:10.0.0.1:3300,v1:10.0.0.1:6789] {tmp}/monmap"),
            ]
        );
        assert_eq!(
            env.spy.keyrings.lock().unwrap().as_slice(),
            ["[mon.]\n\tkey = AQmon\n\tcaps mon = \"allow *\"\n"]
        );
        assert_eq!(
            std::fs::read_to_string(env.path("var/lib/ceph/mon/ceph-node1/done")).unwrap(),
            "join\n"
        );

        // 받은 monmap에 이미 있다(앞 Task가 더한 뒤 멈춤).
        let env = Env::new();
        env.fake().reply(
            "monmaptool --print",
            CmdOutput::ok(
                "0: [v2:10.0.0.2:3300/0,v1:10.0.0.2:6789/0] mon.node2\n\
                           1: [v2:10.0.0.1:3300/0,v1:10.0.0.1:6789/0] mon.node1\n",
            ),
        );
        env.host
            .create_monitor(&mon_init(&keys, MonmapSource::Existing))
            .await
            .unwrap();
        assert!(env.calls_of("monmaptool --addv").is_empty());
        assert_eq!(env.calls_of("ceph-mon --mkfs").len(), 1);
    }

    /// 버려진 부트스트랩(이 노드가 이제 부트스트랩 노드가 아님)이 만든 저장소는 기존 정족수에 넣지 않는다 — 다른
    /// 클러스터의 mon이다. 지우지 않고 손으로 치우는 명령을 알린다. 부트스트랩 노드 자신이나 `join` 저장소는 다시
    /// 하기로 건너뛴다.
    #[tokio::test]
    async fn an_abandoned_bootstrap_store_does_not_join() {
        let keys = keys();
        let env = Env::new();
        env.host
            .create_monitor(&mon_init(&keys, MonmapSource::New))
            .await
            .unwrap();
        let before = env.calls().len();
        let join = MonInit {
            init_node: false,
            ..mon_init(&keys, MonmapSource::Existing)
        };
        let err = env.host.create_monitor(&join).await.unwrap_err();
        let store = env.p("var/lib/ceph/mon/ceph-node1");
        assert_eq!(
            err.to_string(),
            format!(
                "{store} was made by an abandoned Ceph bootstrap on this node and is not part of \
                 the cluster; stop it and move it away by hand: systemctl disable --now \
                 ceph-mon@node1; mv {store} {store}.old"
            )
        );
        assert!(env.path("var/lib/ceph/mon/ceph-node1/done").exists());
        assert!(
            !env.host
                .create_monitor(&mon_init(&keys, MonmapSource::Existing))
                .await
                .unwrap()
        );
        assert_eq!(env.calls().len(), before);

        let env = Env::new();
        env.fake().reply(
            "monmaptool --print",
            CmdOutput::ok("0: [v2:10.0.0.2:3300/0,v1:10.0.0.2:6789/0] mon.node2\n"),
        );
        env.host.create_monitor(&join).await.unwrap();
        assert!(!env.host.create_monitor(&join).await.unwrap());
    }

    /// 정족수에 끝내 들지 않으면 시간 초과
    #[tokio::test]
    async fn quorum_wait_times_out() {
        let env = Env::new();
        env.fake().reply(
            "ceph --connect-timeout 10 quorum_status",
            CmdOutput::ok(r#"{"quorum_names":["node2"]}"#),
        );
        let err = env.host.wait_quorum("node1", &[]).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "timed out waiting for the monitor to join the quorum"
        );
        assert!(env.calls().len() > 1);
    }

    /// 기존 정족수에 더한 mon은 그 정족수의 mon들에만 묻는다(이 노드의 mon이 답하지 않게).
    #[tokio::test]
    async fn a_joining_monitor_asks_the_existing_quorum() {
        let env = Env::new();
        env.fake().reply(
            "ceph --connect-timeout 10 quorum_status",
            CmdOutput::ok(r#"{"quorum_names":["node1","node2"]}"#),
        );
        let mons = ["10.0.0.2".parse().unwrap(), "10.0.0.3".parse().unwrap()];
        env.host.wait_quorum("node1", &mons).await.unwrap();
        assert_eq!(
            env.calls(),
            [
                "ceph --connect-timeout 10 quorum_status -m [v2:10.0.0.2:3300,v1:10.0.0.2:6789] \
              [v2:10.0.0.3:3300,v1:10.0.0.3:6789] --format json"
            ]
        );
    }

    /// admin 키링이 없는 OSD 노드: admin으로 부른 `ceph`는 실패한다.
    fn osd_env(lsblk: &[u8], change: impl FnOnce(&mut CephOptions)) -> Env {
        let env = Env::with(change);
        env.fake().reply("lsblk", CmdOutput::ok(lsblk));
        env.fake().reply("pvs", CmdOutput::ok(PVS));
        env.fake().reply("lvs", CmdOutput::ok(NO_LVS));
        env.fake()
            .reply("ceph-volume lvm list --format json", CmdOutput::ok("{}"));
        env.fake().reply(
            "ceph --connect-timeout",
            CmdOutput::fail(
                1,
                "2026-11-16T07:00:00.000+0000 7f auth: unable to find a keyring on \
                 /etc/ceph/ceph.client.admin.keyring: (2) No such file or directory\n",
            ),
        );
        env
    }

    /// `client.orbivisor`로 부른 `ceph osd tree`의 앞부분
    fn client_tree(env: &Env) -> String {
        format!(
            "ceph --conf {} --id orbivisor --connect-timeout 10 osd tree",
            env.p("etc/opl/ceph/ceph.conf")
        )
    }

    /// 호스트 node1에 OSD `id` 하나가 있는 `ceph osd tree`
    fn tree(id: u32, up: bool) -> CmdOutput {
        let status = if up { "up" } else { "down" };
        CmdOutput::ok(format!(
            r#"{{"nodes":[{{"id":-1,"name":"default","type":"root","type_id":11,"children":[-3]}},{{"id":-3,"name":"node1","type":"host","type_id":1,"pool_weights":{{}},"children":[{id}]}},{{"id":{id},"device_class":"hdd","name":"osd.{id}","type":"osd","type_id":0,"crush_weight":0.009796142578125,"depth":2,"pool_weights":{{}},"exists":1,"status":"{status}","reweight":1,"primary_affinity":1}}],"stray":[]}}"#
        ))
    }

    fn listed(id: u32, device: &str, cluster: &str) -> CmdOutput {
        CmdOutput::ok(format!(
            r#"{{"{id}":[{{"devices":["{device}"],"type":"block","tags":{{"ceph.cluster_fsid":"{cluster}","ceph.osd_fsid":"0851efa7-c274-4d7d-a734-b3a881914253","ceph.osd_id":"{id}"}}}}]}}"#
        ))
    }

    /// 빈 디스크: 지우지 않고 `lvm create`, 만든 OSD의 클러스터를 확인하고 유닛과 OSD가 up이 되기를 기다린다.
    /// by-id 경로로 줘도 같은 디스크다.
    #[tokio::test]
    async fn creates_an_osd_on_an_empty_disk() {
        let env = osd_env(ROOT, |_| {});
        std::fs::create_dir_all(env.path("dev/disk/by-id")).unwrap();
        let link = env.path("dev/disk/by-id/wwn-0x5000c500a1b2c3d5");
        std::os::unix::fs::symlink("/dev/sdb", &link).unwrap();
        env.fake()
            .reply("ceph-volume lvm list /dev/sdb", listed(3, "/dev/sdb", FSID));
        env.fake().reply_seq(
            "systemctl is-active",
            vec![CmdOutput::fail(3, ""), CmdOutput::ok("active\n")],
        );
        env.fake()
            .reply_seq(&client_tree(&env), vec![tree(3, false), tree(3, true)]);
        let id = env
            .host
            .create_osd(&path_str(&link), false, fsid(), &[])
            .await
            .unwrap();
        assert_eq!(id, 3);
        let tree_call = format!("{} --format json", client_tree(&env));
        assert_eq!(
            env.calls(),
            [
                format!("lsblk --json --bytes --paths --output {LSBLK_COLUMNS}"),
                "pvs --reportformat json -o pv_name,vg_name".into(),
                "lvs --reportformat json -o lv_name,vg_name,lv_attr".into(),
                "ceph-volume lvm list --format json".into(),
                "ceph-volume lvm list --format json".into(),
                "ceph-volume lvm create --bluestore --data /dev/sdb".into(),
                "ceph-volume lvm list /dev/sdb --format json".into(),
                "systemctl is-active ceph-osd@3".into(),
                "systemctl is-active ceph-osd@3".into(),
                tree_call.clone(),
                tree_call,
            ]
        );
        let create = &env.fake().commands()[5];
        assert_eq!(create.timeout, Duration::from_secs(15 * 60));
        assert!(create.env.is_empty());
    }

    /// 비지 않은 디스크는 `wipe`가 있어야 하고, 그러면 `zap --destroy`를 먼저 한다. 쓰는 중인 디스크는
    /// 명령 없이 실패한다.
    #[tokio::test]
    async fn wipes_only_when_asked_and_never_disks_in_use() {
        let env = osd_env(ROOT, |_| {});
        let err = env
            .host
            .create_osd("/dev/sdc", false, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "/dev/sdc is not empty (gpt partition table, 1 partition(s), ext4 filesystem on \
             /dev/sdc1); pass wipe to erase it"
        );
        let err = env
            .host
            .create_osd("/dev/sda", true, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "/dev/sda cannot hold an OSD: system disk (/ is on it)"
        );
        let err = env
            .host
            .create_osd("/dev/sdz", true, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "/dev/sdz is not a disk on this node");
        assert!(env.calls_of("ceph-volume lvm zap").is_empty());
        assert!(env.calls_of("ceph-volume lvm create").is_empty());

        env.fake()
            .reply("ceph-volume lvm list /dev/sdc", listed(4, "/dev/sdc", FSID));
        env.fake().reply(&client_tree(&env), tree(4, true));
        env.host
            .create_osd("/dev/sdc", true, fsid(), &[])
            .await
            .unwrap();
        let calls = env.calls_of("ceph-volume lvm ");
        let n = calls.len();
        assert_eq!(
            calls[n - 3..],
            [
                "ceph-volume lvm zap --destroy /dev/sdc",
                "ceph-volume lvm create --bluestore --data /dev/sdc",
                "ceph-volume lvm list /dev/sdc --format json",
            ]
        );
        let zap = env
            .fake()
            .commands()
            .into_iter()
            .find(|c| c.line().starts_with("ceph-volume lvm zap"))
            .unwrap();
        assert_eq!(zap.timeout, Duration::from_secs(10 * 60));
    }

    /// 풀 VG의 PV는 지우기로도 쓰지 않는다(이 노드의 풀 VG를 가르기에 넘김).
    #[tokio::test]
    async fn pool_volume_groups_are_in_use() {
        let env = osd_env(LVM, |_| {});
        let pools = [("vg0".to_string(), "fast".to_string())];
        let err = env
            .host
            .create_osd("/dev/sdh", true, fsid(), &pools)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "/dev/sdh cannot hold an OSD: physical volume of VG vg0 used by storage pool fast"
        );
    }

    /// 이미 이 클러스터의 OSD면(다시 하기) 그 OSD만 켜고(`activate {id} {osd fsid}`) 유닛·up 기다림만 한다.
    #[tokio::test]
    async fn an_existing_osd_of_this_cluster_is_only_activated() {
        let env = osd_env(LVM, |_| {});
        env.fake().reply(
            "ceph-volume lvm list --format json",
            CmdOutput::ok(VOLUME_LIST),
        );
        env.fake().reply(&client_tree(&env), tree(0, true));
        let id = env
            .host
            .create_osd("/dev/sdn", false, fsid(), &[])
            .await
            .unwrap();
        assert_eq!(id, 0);
        let calls = env.calls();
        assert_eq!(
            calls[calls.len() - 3..],
            [
                "ceph-volume lvm activate 0 0851efa7-c274-4d7d-a734-b3a881914253".to_string(),
                "systemctl is-active ceph-osd@0".into(),
                format!("{} --format json", client_tree(&env)),
            ]
        );
        assert!(env.calls_of("ceph-volume lvm create").is_empty());

        // `lvm create`가 도중에 멈춰 켤 수 없는 OSD: 손으로 치우는 명령을 알린다.
        env.fake().reply(
            "ceph-volume lvm activate",
            CmdOutput::fail(
                1,
                "RuntimeError: could not find osd.0 with osd_fsid 0851efa7\n",
            ),
        );
        let err = env
            .host
            .create_osd("/dev/sdn", false, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ceph-volume lvm activate 0 0851efa7-c274-4d7d-a734-b3a881914253 failed: \
             RuntimeError: could not find osd.0 with osd_fsid 0851efa7; osd.0 on /dev/sdn may be \
             half made — remove it by hand with: ceph osd purge 0 --yes-i-really-mean-it; \
             ceph-volume lvm zap --destroy /dev/sdn"
        );
    }

    /// 유닛이 active여도 OSD가 `ceph osd tree`에서 up이 되지 않으면(트리에 없거나 down) 실패다.
    #[tokio::test]
    async fn fails_when_the_osd_does_not_come_up() {
        let env = osd_env(ROOT, |_| {});
        env.fake()
            .reply("ceph-volume lvm list /dev/sdb", listed(3, "/dev/sdb", FSID));
        env.fake().reply_seq(
            &client_tree(&env),
            vec![CmdOutput::ok(r#"{"nodes":[],"stray":[]}"#), tree(3, false)],
        );
        let err = env
            .host
            .create_osd("/dev/sdb", false, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "OSD 3 did not come up: its unit is active but ceph osd tree does not show it up"
        );
        assert!(
            env.calls_of(&client_tree(&env)).len() > 2,
            "{:?}",
            env.calls()
        );
    }

    /// 루프 장치는 허락할 때만 보이고 그때 ceph-volume에 허락 환경을 준다. 만든 OSD가 다른 클러스터의
    /// 것이면 실패다.
    #[tokio::test]
    async fn loop_devices_and_foreign_cluster_osds() {
        let env = osd_env(LOOP, |_| {});
        let err = env
            .host
            .create_osd("/dev/loop2", false, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "/dev/loop2 is not a disk on this node");

        let env = osd_env(LOOP, |o| o.allow_loop = true);
        env.fake().reply(
            "ceph-volume lvm list /dev/loop2",
            listed(7, "/dev/loop2", "0b5c6f39-0000-4000-8000-00000000c0de"),
        );
        let err = env
            .host
            .create_osd("/dev/loop2", false, fsid(), &[])
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "the OSD on /dev/loop2 belongs to Ceph cluster \
                 0b5c6f39-0000-4000-8000-00000000c0de, not {FSID}"
            )
        );
        let create = env
            .fake()
            .commands()
            .into_iter()
            .find(|c| c.line().starts_with("ceph-volume lvm create"))
            .unwrap();
        assert_eq!(
            create.env,
            [(
                "CEPH_VOLUME_ALLOW_LOOP_DEVICES".to_string(),
                "1".to_string()
            )]
        );
    }

    /// lvm2·ceph-volume이 아직 없는 노드도 디스크 목록을 준다. 클러스터가 없으면 OSD 목록을 묻지 않는다.
    #[tokio::test]
    async fn lists_disks_before_ceph_is_installed() {
        let env = Env::new();
        env.fake().reply("lsblk", CmdOutput::ok(ROOT));
        // FakeRunner는 없는 프로그램을 흉내 내지 못하므로 실제 실행기로 없는 pvs를 부른다.
        let host = CephHost::new(
            Arc::new(Missing(env.spy.clone())),
            env.host.paths().clone(),
            env.host.options().clone(),
        );
        let disks = host.disks(&[], None).await.unwrap();
        assert_eq!(disks.len(), 8);
        assert_eq!(disks[1].state, DiskState::Available);
        assert_eq!(
            env.calls(),
            [format!(
                "lsblk --json --bytes --paths --output {LSBLK_COLUMNS}"
            )]
        );
    }

    /// `lsblk` 밖의 프로그램은 없는 것처럼(`NotFound`) 하는 실행기
    struct Missing(Arc<Spy>);

    #[async_trait]
    impl CommandRunner for Missing {
        async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
            if cmd.program == "lsblk" {
                return self.0.run(cmd).await;
            }
            Err(DriverError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such program",
            )))
        }
    }

    fn plan(size: u8, osd_hosts: u32) -> PoolPlan {
        PoolPlan {
            pool: "vmpool".into(),
            name: "vms".into(),
            size,
            osd_hosts,
        }
    }

    fn pool_env() -> Env {
        let env = Env::new();
        env.fake().reply(
            "ceph --connect-timeout 10 osd pool ls --format json",
            CmdOutput::ok(r#"[".mgr"]"#),
        );
        env.fake().reply(
            "ceph --connect-timeout 10 osd pool ls detail",
            CmdOutput::ok(r#"[{"pool_name":".mgr","size":3},{"pool_name":"vmpool","size":1}]"#),
        );
        // mgr가 아직 풀을 모름(빈 목록) → PG 하나가 아직 creating → 모두 active
        env.fake().reply_seq(
            "ceph --connect-timeout 10 pg ls-by-pool vmpool",
            vec![
                CmdOutput::ok(r#"{"pg_ready":true,"pg_stats":[]}"#),
                CmdOutput::ok(
                    r#"{"pg_ready":true,"pg_stats":[{"pgid":"2.0","state":"active+clean"},{"pgid":"2.1","state":"creating+peering"}]}"#,
                ),
                CmdOutput::ok(
                    r#"{"pg_ready":true,"pg_stats":[{"pgid":"2.0","state":"active+clean"},{"pgid":"2.1","state":"active+clean"}]}"#,
                ),
            ],
        );
        env
    }

    /// size 1: 단일 사본 허락, `--yes-i-really-mean-it`, `.mgr`도 1로, PG active 뒤 `rbd pool init`
    #[tokio::test]
    async fn creates_a_single_copy_pool() {
        let env = pool_env();
        env.host.create_pool(&plan(1, 1)).await.unwrap();
        assert_eq!(
            env.calls(),
            [
                "ceph --connect-timeout 10 osd pool ls --format json",
                "ceph --connect-timeout 10 config set global mon_allow_pool_size_one true",
                "ceph --connect-timeout 10 osd pool create vmpool",
                "ceph --connect-timeout 10 osd pool application enable vmpool rbd",
                "ceph --connect-timeout 10 osd pool application set vmpool rbd orbivisor-pool vms",
                "ceph --connect-timeout 10 osd pool set vmpool size 1 --yes-i-really-mean-it",
                "ceph --connect-timeout 10 osd pool ls detail --format json",
                "ceph --connect-timeout 10 osd pool set .mgr size 1 --yes-i-really-mean-it",
                "ceph --connect-timeout 10 pg ls-by-pool vmpool --format json",
                "ceph --connect-timeout 10 pg ls-by-pool vmpool --format json",
                "ceph --connect-timeout 10 pg ls-by-pool vmpool --format json",
                "rbd pool init vmpool",
            ]
        );
    }

    /// size 2·호스트 2: `.mgr`(3)를 2로 낮춘다(확인 깃발 없음). 호스트가 셋이면 그대로 둔다.
    #[tokio::test]
    async fn lowers_the_mgr_pool_only_below_its_size() {
        let env = pool_env();
        env.host.create_pool(&plan(2, 2)).await.unwrap();
        assert_eq!(
            env.calls_of("ceph --connect-timeout 10 osd pool set"),
            [
                "ceph --connect-timeout 10 osd pool set vmpool size 2",
                "ceph --connect-timeout 10 osd pool set .mgr size 2",
            ]
        );
        assert!(
            env.calls_of("ceph --connect-timeout 10 config set")
                .is_empty()
        );

        let env = pool_env();
        env.host.create_pool(&plan(3, 4)).await.unwrap();
        assert_eq!(
            env.calls_of("ceph --connect-timeout 10 osd pool set"),
            ["ceph --connect-timeout 10 osd pool set vmpool size 3"]
        );
    }

    /// 다른 application(cephfs 등)이 쓰는 같은 이름의 Ceph 풀은 바꾸지 않는다. 명령 실패는 `ceph {args}: {첫 줄}`.
    #[tokio::test]
    async fn refuses_an_existing_pool_and_reports_ceph_errors() {
        let env = pool_env();
        env.fake().reply(
            "ceph --connect-timeout 10 osd pool ls --format json",
            CmdOutput::ok(r#"[".mgr","vmpool"]"#),
        );
        env.fake().reply(
            "ceph --connect-timeout 10 osd pool ls detail",
            CmdOutput::ok(
                r#"[{"pool_name":".mgr","size":3,"application_metadata":{"mgr":{}}},{"pool_name":"vmpool","size":3,"application_metadata":{"cephfs":{}}}]"#,
            ),
        );
        let err = env.host.create_pool(&plan(3, 3)).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Ceph pool vmpool already exists for cephfs; Orbivisor does not change it — choose \
             another Ceph pool name"
        );
        assert_eq!(env.calls().len(), 2);

        let env = pool_env();
        env.fake().reply(
            "ceph --connect-timeout 10 osd pool create",
            CmdOutput::fail(
                22,
                "\nError ERANGE: pg_num 32 size 3 would mean 96 total pgs\nmore\n",
            ),
        );
        let err = env.host.create_pool(&plan(3, 3)).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "ceph osd pool create vmpool: Error ERANGE: pg_num 32 size 3 would mean 96 total pgs"
        );
    }

    /// 상태 보고서의 명령 여덟이 함께 돈다: 실행기가 여덟이 모두 들어올 때까지 답하지 않으면, 차례로
    /// 부르는 구현은 끝나지 않는다.
    struct Gate {
        barrier: tokio::sync::Barrier,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl CommandRunner for Gate {
        async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
            let line = cmd.line();
            self.calls.lock().unwrap().push(line.clone());
            assert_eq!(cmd.timeout, Duration::from_secs(15));
            tokio::time::timeout(Duration::from_secs(5), self.barrier.wait())
                .await
                .map_err(|_| DriverError::Timeout("the other report commands"))?;
            let rest = line
                .strip_prefix("ceph --connect-timeout 10 ")
                .and_then(|l| l.strip_suffix(" --format json"))
                .unwrap();
            let out: &[u8] = match rest {
                "status" => include_bytes!("fixtures/status-ok.json"),
                "quorum_status" => include_bytes!("fixtures/quorum-status.json"),
                "mgr dump" => include_bytes!("fixtures/mgr-dump.json"),
                "osd tree" => include_bytes!("fixtures/osd-tree.json"),
                "osd df" => include_bytes!("fixtures/osd-df.json"),
                "df" => include_bytes!("fixtures/df.json"),
                "osd metadata" => include_bytes!("fixtures/osd-metadata.json"),
                "osd pool ls detail" => include_bytes!("fixtures/pool-ls-detail.json"),
                other => panic!("unexpected {other}"),
            };
            Ok(CmdOutput::ok(out))
        }
    }

    #[tokio::test]
    async fn report_runs_the_commands_together() {
        let gate = Arc::new(Gate {
            barrier: tokio::sync::Barrier::new(8),
            calls: Mutex::default(),
        });
        let host = CephHost::new(gate.clone(), CephPaths::default(), CephOptions::default());
        let report = host
            .report(&[("vms".to_string(), "vmpool".to_string())])
            .await
            .unwrap();
        assert_eq!(report.health.status, "HEALTH_OK");
        assert_eq!(report.osd_hosts_up, 2);
        assert!(
            report
                .pools
                .iter()
                .any(|p| p.name == "vmpool" && p.orbivisor_pool.as_deref() == Some("vms"))
        );
        let mut calls = gate.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(calls.len(), 8);
        assert_eq!(calls[0], "ceph --connect-timeout 10 df --format json");
    }

    /// 명령 하나가 실패하면 보고서도 실패다.
    #[tokio::test]
    async fn report_fails_when_a_command_fails() {
        let env = Env::new();
        env.fake().reply(
            "ceph --connect-timeout 10 osd tree",
            CmdOutput::fail(
                1,
                "[errno 110] RADOS timed out (error connecting to the cluster)\n",
            ),
        );
        let err = env.host.report(&[]).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "ceph osd tree --format json: [errno 110] RADOS timed out (error connecting to the \
             cluster)"
        );
        assert_eq!(env.calls().len(), 8);
    }

    /// 앞 Task가 만들다 멈춘 풀(application이 없거나 이 Orbivisor 풀의 표시가 있음)은 만들지 않고 이어서 한다:
    /// 표시를 (없으면) 달고 복제 수를 다시 맞추고 PG를 기다리고 `rbd pool init`. 표시 없는 rbd 풀이나 다른 Orbivisor
    /// 풀의 것은 쓰는 풀일 수 있으므로 복제 수를 바꾸지 않고 실패한다.
    #[tokio::test]
    async fn resumes_a_pool_left_by_a_failed_task() {
        let pool = |apps: &str| {
            let env = pool_env();
            env.fake().reply(
                "ceph --connect-timeout 10 osd pool ls --format json",
                CmdOutput::ok(r#"[".mgr","vmpool"]"#),
            );
            env.fake().reply(
                "ceph --connect-timeout 10 osd pool ls detail",
                CmdOutput::ok(format!(
                    r#"[{{"pool_name":".mgr","size":1,"application_metadata":{{"mgr":{{}}}}}},{{"pool_name":"vmpool","size":3,"application_metadata":{apps}}}]"#
                )),
            );
            env
        };
        for (apps, marks) in [("{}", 2), (r#"{"rbd":{"orbivisor-pool":"vms"}}"#, 0)] {
            let env = pool(apps);
            env.host.create_pool(&plan(1, 1)).await.unwrap();
            assert!(
                env.calls_of("ceph --connect-timeout 10 osd pool create")
                    .is_empty(),
                "{:?}",
                env.calls()
            );
            assert_eq!(
                env.calls_of("ceph --connect-timeout 10 osd pool application")
                    .len(),
                marks
            );
            assert_eq!(
                env.calls_of("ceph --connect-timeout 10 osd pool set vmpool"),
                ["ceph --connect-timeout 10 osd pool set vmpool size 1 --yes-i-really-mean-it"]
            );
            assert_eq!(env.calls_of("rbd pool init vmpool").len(), 1);
        }
        for (apps, used) in [
            (r#"{"rbd":{}}"#, "rbd"),
            (
                r#"{"rbd":{"orbivisor-pool":"other"}}"#,
                "Orbivisor storage pool other",
            ),
        ] {
            let env = pool(apps);
            let err = env.host.create_pool(&plan(1, 1)).await.unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "Ceph pool vmpool already exists for {used}; Orbivisor does not change it — \
                     choose another Ceph pool name"
                )
            );
            assert_eq!(env.calls().len(), 2, "{:?}", env.calls());
        }
    }

    /// cephadm이 세운 남의 클러스터(`/var/lib/ceph/{fsid}/`)가 있으면 아무 명령 없이 실패한다.
    #[tokio::test]
    async fn a_cephadm_cluster_is_foreign() {
        let env = Env::new();
        std::fs::create_dir_all(env.path("var/lib/ceph/tmp")).unwrap();
        env.host.check_foreign("node1", fsid()).unwrap();
        let dir = env.path("var/lib/ceph/0b5c6f39-0000-4000-8000-00000000c0de/osd.3");
        std::fs::create_dir_all(&dir).unwrap();
        let err = env.host.check_foreign("node1", fsid()).unwrap_err();
        assert!(matches!(err, CephError::Foreign(_)));
        assert_eq!(
            err.to_string(),
            format!(
                "node \"node1\": {} belongs to another Ceph cluster (deployed by cephadm)",
                env.p("var/lib/ceph/0b5c6f39-0000-4000-8000-00000000c0de")
            )
        );
        assert!(env.calls().is_empty());
    }

    /// mgr 키링이 이미 있어도(앞 Task가 chown 전에 멈춤) 늘 ceph 소유로 맞춘다.
    #[tokio::test]
    async fn the_manager_keyring_is_always_owned_by_ceph() {
        let env = Env::new();
        let dir = env.path("var/lib/ceph/mgr/ceph-node1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keyring"), "[mgr.node1]\n").unwrap();
        env.host.create_manager("node1", fsid()).await.unwrap();
        assert!(env.calls_of("ceph --connect-timeout 10 auth").is_empty());
        assert_eq!(
            env.calls_of("chown"),
            [format!("chown -R ceph:ceph {}", path_str(&dir))]
        );
    }

    /// 임시 디렉터리를 지우지 못해도 mkfs의 실패가 그대로 보인다.
    #[tokio::test]
    async fn a_cleanup_failure_does_not_hide_the_error() {
        let env = Env::new();
        let keys = keys();
        // mkfs가 실패하면서 임시 디렉터리를 지울 수 없게 한다(부모를 읽기 전용으로).
        let tmp_parent = env.path("var/lib/ceph/tmp");
        struct Lock(PathBuf);
        #[async_trait]
        impl CommandRunner for Lock {
            async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
                if cmd.program == "ceph-mon" {
                    std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o500))
                        .unwrap();
                    return Ok(CmdOutput::fail(1, "mkfs failed\n"));
                }
                Ok(CmdOutput::default())
            }
        }
        let host = CephHost::new(
            Arc::new(Lock(tmp_parent.clone())),
            env.host.paths().clone(),
            env.host.options().clone(),
        );
        let err = host
            .create_monitor(&mon_init(&keys, MonmapSource::New))
            .await
            .unwrap_err();
        std::fs::set_permissions(&tmp_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(err.to_string().contains("failed: mkfs failed"), "{err}");
    }

    /// 시간을 넘긴 명령은 그 명령 줄과 한도를 알린다.
    #[tokio::test]
    async fn timeouts_name_the_command() {
        struct Slow;
        #[async_trait]
        impl CommandRunner for Slow {
            async fn run(&self, cmd: &Cmd) -> Result<CmdOutput, DriverError> {
                if cmd.program == "apt-get" || cmd.program == "ceph" {
                    return Err(DriverError::Timeout("a Ceph setup command"));
                }
                Ok(CmdOutput::default())
            }
        }
        let env = Env::with(|o| o.install_timeout = Duration::from_secs(77));
        let host = CephHost::new(
            Arc::new(Slow),
            env.host.paths().clone(),
            env.host.options().clone(),
        );
        let err = host
            .ensure_packages(&strings(&["ceph-common"]))
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "apt-get update -q -o DPkg::Lock::Timeout=600 timed out after 77s"
        );
        assert_eq!(
            host.ceph(&["osd", "tree"]).await.unwrap_err().to_string(),
            "ceph --connect-timeout 10 osd tree timed out after 60s"
        );
    }

    /// 같은 파일을 함께 쓰는 쪽(에이전트와 Task)이 서로의 임시 파일을 덮지 않게 임시 이름은 매번 다르다.
    #[test]
    fn temporary_files_have_unique_names() {
        let path = Path::new("/etc/opl/ceph/ceph.conf");
        let (a, b) = (temp_path(path), temp_path(path));
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert_eq!(t.parent(), path.parent());
            let name = t.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with(&format!(".ceph.conf.opl-tmp-{}-", std::process::id())),
                "{name}"
            );
        }
    }
}
