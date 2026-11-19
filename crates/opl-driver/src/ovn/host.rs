//! 노드에서 OVN을 세우는 단계들(계획 65 §7): 꾸러미 설치, 노드 TLS 사본, 배포판 중앙 단위 끄기, NB·SB Raft 클러스터
//! 만들기·합류·떠나기, Orbivisor 소유 단위, chassis 구성(로컬 OVS DB), 상태 읽기, 오래된 OVS 포트 청소. 모든 단계는 상태를
//! 먼저 보고 이미 된 일을 건너뛰므로(멱등) 실패한 Task를 다시 돌려도 된다. 관리 표시가 없는 설정·남의 DB는 거절한다
//! ("아무것도 몰래 지우지 않는다", §7.4). 명령은 Ceph와 같은 [`CommandRunner`]로 돌리고(시험은 가짜), 파일은
//! [`OvnPaths`] 아래에 직접 쓴다(시험은 임시 디렉터리).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opl_types::sdn::{CentralHealth, MIN_OVN, RaftRole};
use thiserror::Error;
use uuid::Uuid;

use super::status::{
    ClusterStatus, OvsInterface, SslConfig, major_minor, parse_cluster_status, parse_external_ids,
    parse_interfaces, parse_ssl, parse_table, parse_version,
};
use super::units::{Db, NORTHD_UNIT, db_unit, is_managed, northd_unit, remotes, ssl_addr};
use crate::ceph::runner::{Cmd, CmdOutput, CommandRunner};
use crate::error::DriverError;

/// 중앙 노드에 까는 꾸러미의 기본 목록(중앙 노드도 chassis다)
pub const CENTRAL_PACKAGES: &[&str] = &["ovn-central", "ovn-host", "openvswitch-switch"];
/// chassis에 까는 꾸러미의 기본 목록
pub const HOST_PACKAGES: &[&str] = &["ovn-host", "openvswitch-switch"];
/// Orbivisor 단위로 바꾸며 끄고 가리는 배포판 중앙 단위(Ubuntu 24.04·Debian의 `ovn-central`)
pub const DISTRO_CENTRAL_UNITS: &[&str] = &[
    "ovn-central.service",
    "ovn-northd.service",
    "ovn-ovsdb-server-nb.service",
    "ovn-ovsdb-server-sb.service",
];
/// chassis의 배포판 단위: `ovn-host`(켜 두는 묶음)와 `ovn-controller`(static, `ovn-host`가 Wants)
pub const HOST_UNIT: &str = "ovn-host.service";
pub const CONTROLLER_UNIT: &str = "ovn-controller.service";
/// 로컬 OVS DB `Open_vSwitch` 행과 `Interface` 행의 클러스터 표시 칸
pub const TAG_KEY: &str = "orbivisor-cluster";
/// apt가 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 기다리는 시간(초)
const APT_LOCK: &str = "DPkg::Lock::Timeout=600";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// `ovs-appctl`·`ovn-appctl`·`ovn-sbctl`이 답을 기다리는 시간(초)
const CTL_TIMEOUT: &str = "5";
/// 스키마 변환(`ovsdb-client convert`)의 시간 한도
const CONVERT_TIMEOUT: Duration = Duration::from_secs(120);
/// 배포판 단위 중 Orbivisor가 끈 것의 기록(`{etc}/distro-units`, 줄마다 단위 이름)
const DISTRO_UNITS_FILE: &str = "distro-units";

/// OVN 구성 단계의 실패
#[derive(Debug, Error)]
pub enum OvnError {
    /// 이 노드에 Orbivisor가 만들지 않은 OVN 설정·DB가 있다(건드리지 않는다).
    #[error("{0}")]
    Foreign(String),
    /// 명령이 실패했거나 결과가 맞지 않다.
    #[error("{0}")]
    Command(String),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    /// 이 노드에서는 할 수 없다(Linux가 아님, OVN 판이 낮음 등).
    #[error("{0}")]
    Unsupported(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<DriverError> for OvnError {
    fn from(e: DriverError) -> Self {
        match e {
            DriverError::Timeout(what) => Self::Timeout(what),
            DriverError::Io(e) => Self::Io(e),
            e => Self::Command(e.to_string()),
        }
    }
}

/// OVN 파일이 있는 자리들
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvnPaths {
    /// 노드 TLS 사본과 배포판 단위 기록(`/etc/opl/ovn`, 0700)
    pub etc: PathBuf,
    /// 중앙 DB(`/var/lib/opl/ovn/{nb,sb}.db`)
    pub var_lib: PathBuf,
    /// 중앙 제어 소켓·pid 파일(`/run/opl/ovn`)
    pub run: PathBuf,
    /// 중앙 로그(`/var/log/opl`)
    pub log: PathBuf,
    /// Orbivisor 단위 파일을 두는 곳(`/etc/systemd/system`)
    pub systemd: PathBuf,
    /// 노드 TLS 원본(opld `{data_dir}/pki`)
    pub pki: PathBuf,
    /// 꾸러미의 스키마(`/usr/share/ovn`)
    pub schemas: PathBuf,
    /// 배포판 NB DB(`/var/lib/ovn/ovnnb_db.db` — 논리 스위치가 있으면 남의 OVN이다)
    pub distro_nb_db: PathBuf,
    /// `ovs-ctl --system-id=random`이 OVS를 켤 때마다 `system-id`로 다시 쓰는 파일
    pub ovs_system_id: PathBuf,
}

impl Default for OvnPaths {
    fn default() -> Self {
        Self {
            etc: "/etc/opl/ovn".into(),
            var_lib: "/var/lib/opl/ovn".into(),
            run: "/run/opl/ovn".into(),
            log: "/var/log/opl".into(),
            systemd: "/etc/systemd/system".into(),
            pki: "/var/lib/opl/pki".into(),
            schemas: "/usr/share/ovn".into(),
            distro_nb_db: "/var/lib/ovn/ovnnb_db.db".into(),
            ovs_system_id: "/etc/openvswitch/system-id.conf".into(),
        }
    }
}

impl OvnPaths {
    pub fn db_file(&self, db: Db) -> PathBuf {
        self.var_lib.join(format!("{}.db", db.short()))
    }

    /// root만 쓰는 로컬 소켓(디버깅: `ovn-nbctl --db=unix:/run/opl/ovn/nb.sock show`)
    pub fn db_socket(&self, db: Db) -> PathBuf {
        self.run.join(format!("{}.sock", db.short()))
    }

    pub fn db_ctl(&self, db: Db) -> PathBuf {
        self.run.join(format!("{}.ctl", db.short()))
    }

    pub fn northd_ctl(&self) -> PathBuf {
        self.run.join("northd.ctl")
    }

    pub fn tls_key(&self) -> PathBuf {
        self.etc.join("node.key")
    }

    pub fn tls_cert(&self) -> PathBuf {
        self.etc.join("node.crt")
    }

    pub fn tls_ca(&self) -> PathBuf {
        self.etc.join("ca.crt")
    }

    pub fn schema(&self, db: Db) -> PathBuf {
        self.schemas.join(db.schema_file())
    }

    fn unit_file(&self, unit: &str) -> PathBuf {
        self.systemd.join(unit)
    }
}

/// OVN 구성의 설정(opld `[sdn]`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvnOptions {
    /// 중앙 노드에 까는 꾸러미
    pub central_packages: Vec<String>,
    /// chassis에 까는 꾸러미
    pub host_packages: Vec<String>,
    /// `apt-get update`·`install` 하나의 시간 한도
    pub install_timeout: Duration,
    /// 기다리는 동안 다시 보는 간격
    pub wait_step: Duration,
    /// NB·SB 리더·멤버·떠나기를 기다리는 시간의 상한(설계 §7.1 60초)
    pub wait_limit: Duration,
    /// Linux가 아닌 곳에서도 단계를 돌린다(시험용 — 명령은 가짜 실행기가 받는다).
    pub any_os: bool,
}

impl Default for OvnOptions {
    fn default() -> Self {
        Self {
            central_packages: CENTRAL_PACKAGES.iter().map(|p| p.to_string()).collect(),
            host_packages: HOST_PACKAGES.iter().map(|p| p.to_string()).collect(),
            install_timeout: Duration::from_secs(30 * 60),
            wait_step: Duration::from_secs(1),
            wait_limit: Duration::from_secs(60),
            any_os: false,
        }
    }
}

/// 깔린 OVN·OVS 판(`24.03.8`, `3.3.9`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OvnVersions {
    pub ovn: String,
    pub ovs: String,
}

/// 이 노드의 OVN Raft 클러스터 id(Raft `SdnStatus`가 아는 것)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KnownCids {
    pub nb: Option<Uuid>,
    pub sb: Option<Uuid>,
}

impl KnownCids {
    fn get(&self, db: Db) -> Option<Uuid> {
        match db {
            Db::Nb => self.nb,
            Db::Sb => self.sb,
        }
    }
}

/// 이 노드의 chassis 구성(로컬 OVS DB `Open_vSwitch` 행)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chassis<'a> {
    /// chassis 이름(`system-id`) = 노드 UUID(노드 인증서 CN과 같다)
    pub node: Uuid,
    /// SB에 붙을 중앙 주소들(`SdnStatus.centrals` 차례)
    pub centrals: &'a [IpAddr],
    /// Geneve 터널의 이 노드 주소(`NodeStatus.underlay`)
    pub encap_ip: IpAddr,
}

/// 이 노드에서 읽은 chassis 상태
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChassisProbe {
    /// OVS가 없거나 이 클러스터의 chassis로 구성되지 않았다.
    NotConfigured,
    /// 이 클러스터의 chassis다. `connected`는 ovn-controller가 SB에 붙었는지, `registered`는 SB에 이 노드의
    /// `Chassis` 행이 있는지(읽지 못했으면 None)
    Configured {
        connected: bool,
        registered: Option<bool>,
        encap_ip: Option<IpAddr>,
    },
}

/// 노드에서 OVN을 세우는 단계들
pub struct OvnHost {
    runner: Arc<dyn CommandRunner>,
    paths: OvnPaths,
    options: OvnOptions,
    /// 클러스터 태그(CA 지문 앞 12자) — OVS의 `external_ids:orbivisor-cluster`
    tag: String,
}

impl OvnHost {
    pub fn new(
        runner: Arc<dyn CommandRunner>,
        paths: OvnPaths,
        options: OvnOptions,
        tag: String,
    ) -> Self {
        Self {
            runner,
            paths,
            options,
            tag,
        }
    }

    pub fn paths(&self) -> &OvnPaths {
        &self.paths
    }

    pub fn options(&self) -> &OvnOptions {
        &self.options
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// OVN은 Linux 노드에만 세운다.
    pub fn check_platform(&self) -> Result<(), OvnError> {
        if cfg!(target_os = "linux") || self.options.any_os {
            Ok(())
        } else {
            Err(OvnError::Unsupported(
                "SDN (OVN) can be set up only on Linux nodes".into(),
            ))
        }
    }

    // -------------------------------------------------------------------------------------------------------
    // 거절 검사(§7.4)
    // -------------------------------------------------------------------------------------------------------

    /// 노드 `node`에 다른 OVN·OpenStack이나 다른 Orbivisor 클러스터의 OVN이 있으면 실패한다. 아무것도 바꾸지 않는다.
    ///
    /// - 로컬 OVS DB의 `external_ids:orbivisor-cluster`가 다른 태그, 또는 이 표시 없이 `ovn-remote`가 있음(꾸러미가
    ///   정한 무작위 `system-id`만 있는 것은 괜찮다 — 덮어쓴다)
    /// - `/var/lib/opl/ovn/{nb,sb}.db`가 있고 그 클러스터 id가 `known`(Raft가 아는 것)과 다름. 합류 중인 DB(id를 아직
    ///   모름)는 `joining`일 때만 받는다.
    /// - 배포판 NB DB에 논리 스위치가 하나라도 있음, 또는 그 DB가 클러스터 DB임(꾸러미가 막 만든 빈 DB는 괜찮다)
    ///
    /// 돌려주는 것은 경고다(`br-int`에 Orbivisor가 붙이지 않은 포트가 있음).
    pub async fn check_foreign(
        &self,
        node: &str,
        known: KnownCids,
        joining: bool,
    ) -> Result<Vec<String>, OvnError> {
        if let Some(ids) = self.external_ids().await? {
            match (ids.get(TAG_KEY), ids.get("ovn-remote")) {
                (Some(tag), _) if *tag != self.tag => {
                    return Err(OvnError::Foreign(format!(
                        "node {node:?} is an OVN chassis of another Orbivisor cluster \
                         (external_ids:{TAG_KEY}={tag}); remove it from that cluster first"
                    )));
                }
                (None, Some(remote)) => {
                    return Err(OvnError::Foreign(format!(
                        "node {node:?} is already an OVN chassis of another OVN deployment \
                         (external_ids:ovn-remote={remote}); Orbivisor does not adopt it"
                    )));
                }
                _ => {}
            }
        }
        for db in Db::ALL {
            let path = self.paths.db_file(db);
            if !path.exists() {
                continue;
            }
            let cid = self.db_cid(&path).await?;
            let ok = match (cid, known.get(db)) {
                (Some(cid), Some(known)) => cid == known,
                (None, Some(_)) => joining,
                _ => false,
            };
            if !ok {
                return Err(OvnError::Foreign(format!(
                    "node {node:?}: {} belongs to another OVN cluster{}; move it away first",
                    path.display(),
                    cid.map(|c| format!(" ({c})")).unwrap_or_default()
                )));
            }
        }
        self.check_distro_db(node).await?;
        Ok(self.br_int_warning().await.into_iter().collect())
    }

    /// 배포판 NB DB에 논리 스위치가 있으면(또는 클러스터 DB면) 실패한다. 파일이 없으면 통과다.
    pub async fn check_distro_db(&self, node: &str) -> Result<(), OvnError> {
        let path = &self.paths.distro_nb_db;
        if !path.exists() {
            return Ok(());
        }
        let file = path_str(path);
        let clustered = self
            .exec(&Cmd::new("ovsdb-tool", &["db-is-clustered", &file]))
            .await?;
        if clustered.success() {
            return Err(OvnError::Foreign(format!(
                "node {node:?}: {file} is a clustered OVN database (another OVN deployment); \
                 Orbivisor does not adopt it"
            )));
        }
        let query = r#"["OVN_Northbound",{"op":"select","table":"Logical_Switch","where":[],"columns":["name"]}]"#;
        let out = self
            .run_ok(&Cmd::new("ovsdb-tool", &["query", &file, query]))
            .await?;
        let rows: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| {
            OvnError::Command(format!("cannot read the logical switches in {file}: {e}"))
        })?;
        let count = rows[0]["rows"].as_array().map_or(0, Vec::len);
        if count > 0 {
            return Err(OvnError::Foreign(format!(
                "node {node:?}: {file} has {count} logical switch(es) of another OVN deployment; \
                 Orbivisor does not adopt it"
            )));
        }
        Ok(())
    }

    /// `br-int`에 이 클러스터 표시가 없는 포트가 있으면 경고(ovn-controller의 터널 `ovn-…`는 뺀다). 경고일 뿐이므로
    /// 읽지 못하면 없는 것으로 한다.
    async fn br_int_warning(&self) -> Option<String> {
        let out = self
            .run_ok(&Cmd::new("ovs-vsctl", &["list-ports", "br-int"]))
            .await
            .ok()?;
        let ports: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|p| !p.is_empty() && !p.starts_with("ovn-"))
            .map(str::to_string)
            .collect();
        if ports.is_empty() {
            return None;
        }
        let ours: BTreeSet<String> = self
            .tagged_interfaces()
            .await
            .ok()?
            .into_iter()
            .map(|i| i.name)
            .collect();
        let foreign: Vec<String> = ports.into_iter().filter(|p| !ours.contains(p)).collect();
        (!foreign.is_empty()).then(|| {
            format!(
                "br-int already has ports Orbivisor did not add: {}",
                foreign.join(", ")
            )
        })
    }

    // -------------------------------------------------------------------------------------------------------
    // 꾸러미·TLS·배포판 단위
    // -------------------------------------------------------------------------------------------------------

    /// `packages` 가운데 깔리지 않은 것만 apt로 깔고 깔린 OVN·OVS 판을 돌려준다. OVN이 [`MIN_OVN`] 미만이면 실패한다.
    pub async fn ensure_packages(&self, packages: &[String]) -> Result<OvnVersions, OvnError> {
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
        let versions = self.versions().await?.ok_or_else(|| {
            OvnError::Command("ovn-controller or ovs-vsctl is missing after the install".into())
        })?;
        if major_minor(&versions.ovn).is_none_or(|v| v < MIN_OVN) {
            return Err(OvnError::Unsupported(format!(
                "OVN {} is too old; Orbivisor sets up OVN {}.{:02} or newer",
                versions.ovn, MIN_OVN.0, MIN_OVN.1
            )));
        }
        Ok(versions)
    }

    /// 깔린 OVN(`ovn-controller --version`)·OVS(`ovs-vsctl --version`) 판. 둘 중 하나라도 없으면 None
    pub async fn versions(&self) -> Result<Option<OvnVersions>, OvnError> {
        let mut found = Vec::new();
        for program in ["ovn-controller", "ovs-vsctl"] {
            let Some(out) = self
                .run_optional(&Cmd::new(program, &["--version"]))
                .await?
            else {
                return Ok(None);
            };
            let text = String::from_utf8_lossy(&out.stdout);
            found.push(parse_version(&text).ok_or_else(|| {
                OvnError::Command(format!("cannot tell the {program} version from {text:?}"))
            })?);
        }
        let ovs = found.pop().unwrap_or_default();
        let ovn = found.pop().unwrap_or_default();
        Ok(Some(OvnVersions { ovn, ovs }))
    }

    /// `dpkg-query`가 `install ok installed`로 보이지 않는 꾸러미(차례 그대로)
    async fn missing_packages(&self, packages: &[String]) -> Result<Vec<String>, OvnError> {
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

    /// 노드 TLS(`{pki}/{ca.crt,node.crt,node.key}`)를 `{etc}`에 0600으로 베낀다(디렉터리 0700). 같으면 쓰지 않고
    /// `false`다(인증서를 다시 서명했으면 바뀐다).
    pub fn write_tls(&self) -> Result<bool, OvnError> {
        make_dir(&self.paths.etc, 0o700)?;
        let mut changed = false;
        for (from, to) in [
            ("ca.crt", self.paths.tls_ca()),
            ("node.crt", self.paths.tls_cert()),
            ("node.key", self.paths.tls_key()),
        ] {
            let src = self.paths.pki.join(from);
            let content = std::fs::read(&src).map_err(|e| io_at(&src, e))?;
            changed |= write_file(&to, &content, 0o600)?;
        }
        Ok(changed)
    }

    /// 배포판 중앙 단위를 끄고(`disable --now`) 가린다(`mask`). 없는 단위·이미 가린 단위는 건너뛴다. 끈 단위는
    /// `{etc}/distro-units`에 더해 적는다. 끈 단위 이름을 돌려준다.
    pub async fn disable_distro_units(&self) -> Result<Vec<String>, OvnError> {
        let mut units = Vec::new();
        for unit in DISTRO_CENTRAL_UNITS {
            let out = self
                .run_ok(&Cmd::new(
                    "systemctl",
                    &["show", "-p", "LoadState", "--value", unit],
                ))
                .await?;
            match String::from_utf8_lossy(&out.stdout).trim() {
                "not-found" | "masked" => {}
                _ => units.push(unit.to_string()),
            }
        }
        if units.is_empty() {
            return Ok(units);
        }
        let record = self.paths.etc.join(DISTRO_UNITS_FILE);
        let mut listed: Vec<String> = read_optional(&record)?
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        for unit in &units {
            if !listed.contains(unit) {
                listed.push(unit.clone());
            }
        }
        // 끄기 전에 적는다(도중에 멈춰도 무엇을 끄려 했는지 남는다).
        make_dir(&self.paths.etc, 0o700)?;
        write_file(
            &record,
            format!("{}\n", listed.join("\n")).as_bytes(),
            0o644,
        )?;
        let mut args = vec!["disable".to_string(), "--now".into()];
        args.extend(units.iter().cloned());
        self.run_ok(&Cmd::new("systemctl", &args)).await?;
        let mut args = vec!["mask".to_string()];
        args.extend(units.iter().cloned());
        self.run_ok(&Cmd::new("systemctl", &args)).await?;
        Ok(units)
    }

    // -------------------------------------------------------------------------------------------------------
    // 중앙 DB 클러스터
    // -------------------------------------------------------------------------------------------------------

    /// DB 파일의 클러스터 id(`ovsdb-tool db-cid`). 합류 중이라 아직 모르면 None
    pub async fn db_cid(&self, path: &Path) -> Result<Option<Uuid>, OvnError> {
        let out = self
            .exec(&Cmd::new("ovsdb-tool", &["db-cid", &path_str(path)]))
            .await?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if let Ok(cid) = text.trim().parse::<Uuid>() {
            return Ok(Some(cid));
        }
        if text.contains("not yet known") {
            return Ok(None);
        }
        Err(OvnError::Command(format!(
            "ovsdb-tool db-cid {} failed: {}",
            path.display(),
            tail(&text, ERROR_LINES).unwrap_or_else(|| format!("exit status {}", out.status))
        )))
    }

    /// 새 클러스터 DB `db`를 임시 파일(`.{nb,sb}.db.opl-new`)에 만들고(`ovsdb-tool create-cluster`, Raft 주소
    /// `ssl:ADDR:6643·6644`) 그 클러스터 id를 돌려준다. 부르는 쪽이 id를 Raft에 적은 뒤 [`commit_db`](Self::commit_db)로
    /// 제자리에 옮긴다 — 그래서 제자리의 DB는 늘 Raft가 아는 id다(§7.4 검사). DB가 이미 제자리에 있으면 None
    pub async fn create_cluster(&self, db: Db, address: IpAddr) -> Result<Option<Uuid>, OvnError> {
        if self.paths.db_file(db).exists() {
            return Ok(None);
        }
        let tmp = self.new_db(db);
        remove_file(&tmp)?;
        make_dir(&self.paths.var_lib, 0o750)?;
        self.run_ok(&Cmd::new(
            "ovsdb-tool",
            &[
                "create-cluster",
                &path_str(&tmp),
                &path_str(&self.paths.schema(db)),
                &ssl_addr(address, db.raft_port()),
            ],
        ))
        .await?;
        let cid = self
            .db_cid(&tmp)
            .await?
            .ok_or_else(|| OvnError::Command(format!("{} has no cluster id", tmp.display())))?;
        Ok(Some(cid))
    }

    /// 기존 클러스터에 합류할 DB `db`를 만든다(`ovsdb-tool join-cluster`, 이 노드 `address`, 기존 중앙 `remotes`의 Raft
    /// 주소). 임시 파일에 만들어 제자리로 옮긴다. 이미 있으면 `false`
    pub async fn join_cluster(
        &self,
        db: Db,
        address: IpAddr,
        others: &[IpAddr],
    ) -> Result<bool, OvnError> {
        if self.paths.db_file(db).exists() {
            return Ok(false);
        }
        if others.is_empty() {
            return Err(OvnError::Command("no existing OVN central to join".into()));
        }
        let tmp = self.new_db(db);
        remove_file(&tmp)?;
        make_dir(&self.paths.var_lib, 0o750)?;
        let mut args = vec![
            "join-cluster".to_string(),
            path_str(&tmp),
            db.name().into(),
            ssl_addr(address, db.raft_port()),
        ];
        args.extend(others.iter().map(|ip| ssl_addr(*ip, db.raft_port())));
        self.run_ok(&Cmd::new("ovsdb-tool", &args)).await?;
        self.commit_db(db)?;
        Ok(true)
    }

    /// [`create_cluster`](Self::create_cluster)가 만든 임시 DB를 제자리로 옮긴다(이미 옮겼으면 그대로).
    pub fn commit_db(&self, db: Db) -> Result<(), OvnError> {
        let tmp = self.new_db(db);
        let target = self.paths.db_file(db);
        if target.exists() && !tmp.exists() {
            return Ok(());
        }
        std::fs::rename(&tmp, &target).map_err(|e| io_at(&target, e))?;
        sync_dir(&self.paths.var_lib);
        Ok(())
    }

    fn new_db(&self, db: Db) -> PathBuf {
        self.paths
            .var_lib
            .join(format!(".{}.db.opl-new", db.short()))
    }

    /// Orbivisor 단위 셋을 쓴다(DB 둘은 이 노드 주소 `address`, northd는 중앙 모두 `centrals`). 관리 표시가 없는 같은
    /// 이름의 파일은 덮지 않는다. 바뀐 단위 이름을 돌려주고, 바뀌었으면 `systemctl daemon-reload`를 한다.
    pub async fn write_units(
        &self,
        address: IpAddr,
        centrals: &[IpAddr],
    ) -> Result<Vec<String>, OvnError> {
        let mut changed = Vec::new();
        let mut files: Vec<(String, String)> = Db::ALL
            .iter()
            .map(|db| (db.unit(), db_unit(*db, address, &self.paths)))
            .collect();
        files.push((NORTHD_UNIT.to_string(), northd_unit(centrals, &self.paths)));
        for (unit, text) in files {
            let path = self.paths.unit_file(&unit);
            if let Some(old) = read_optional(&path)?
                && !is_managed(&old)
            {
                return Err(OvnError::Foreign(format!(
                    "{} was not written by Orbivisor; not overwriting it",
                    path.display()
                )));
            }
            if write_file(&path, text.as_bytes(), 0o644)? {
                changed.push(unit);
            }
        }
        if !changed.is_empty() {
            self.run_ok(&Cmd::new("systemctl", &["daemon-reload"]))
                .await?;
        }
        Ok(changed)
    }

    /// Orbivisor 단위 셋을 켜고 부팅 때도 켜지게 한다. `restart`에 든 단위(파일이 바뀐 것)는 다시 시작한다.
    pub async fn start_central(&self, restart: &[String]) -> Result<(), OvnError> {
        let units = central_units();
        let mut args = vec!["enable".to_string()];
        args.extend(units.iter().cloned());
        self.run_ok(&Cmd::new("systemctl", &args)).await?;
        for unit in units {
            let verb = if restart.contains(&unit) {
                "restart"
            } else {
                "start"
            };
            self.run_ok(&Cmd::new("systemctl", &[verb, &unit])).await?;
        }
        Ok(())
    }

    /// 멈춘 Orbivisor 단위를 다시 켠다(에이전트). 켠 단위 이름을 돌려준다.
    pub async fn ensure_central_running(&self) -> Result<Vec<String>, OvnError> {
        let mut started = Vec::new();
        for unit in central_units() {
            let active = self
                .exec(&Cmd::new("systemctl", &["is-active", &unit]))
                .await?
                .success();
            if !active {
                self.run_ok(&Cmd::new("systemctl", &["start", &unit]))
                    .await?;
                started.push(unit);
            }
        }
        Ok(started)
    }

    /// Orbivisor 단위 셋을 멈추고 끈다(파일은 남긴다).
    pub async fn stop_central(&self) -> Result<(), OvnError> {
        let mut args = vec!["disable".to_string(), "--now".into()];
        args.extend(central_units());
        self.run_ok(&Cmd::new("systemctl", &args)).await.map(drop)
    }

    /// DB `db`의 `cluster/status`
    pub async fn cluster_status(&self, db: Db) -> Result<ClusterStatus, OvnError> {
        let out = self
            .run_ok(&Cmd::new(
                "ovs-appctl",
                &[
                    "--timeout",
                    CTL_TIMEOUT,
                    "-t",
                    &path_str(&self.paths.db_ctl(db)),
                    "cluster/status",
                    db.name(),
                ],
            ))
            .await?;
        parse_cluster_status(&String::from_utf8_lossy(&out.stdout)).map_err(OvnError::Command)
    }

    /// DB `db`의 이 서버가 리더가 될 때까지 기다린다(새 클러스터).
    pub async fn wait_leader(&self, db: Db) -> Result<ClusterStatus, OvnError> {
        let what = match db {
            Db::Nb => "the OVN northbound database to elect a leader",
            Db::Sb => "the OVN southbound database to elect a leader",
        };
        self.wait_status(db, what, |s| s.is_leader()).await
    }

    /// DB `db`의 이 서버가 클러스터 `cid`의 멤버가 될 때까지 기다린다(합류).
    pub async fn wait_member(&self, db: Db, cid: Option<Uuid>) -> Result<ClusterStatus, OvnError> {
        let what = match db {
            Db::Nb => "the OVN northbound database to join the cluster",
            Db::Sb => "the OVN southbound database to join the cluster",
        };
        self.wait_status(db, what, |s| {
            s.is_member() && (cid.is_none() || s.cid == cid)
        })
        .await
    }

    async fn wait_status(
        &self,
        db: Db,
        what: &'static str,
        done: impl Fn(&ClusterStatus) -> bool,
    ) -> Result<ClusterStatus, OvnError> {
        let deadline = tokio::time::Instant::now() + self.options.wait_limit;
        loop {
            if let Ok(status) = self.cluster_status(db).await
                && done(&status)
            {
                return Ok(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(OvnError::Timeout(what));
            }
            tokio::time::sleep(self.options.wait_step).await;
        }
    }

    /// DB `db`의 이 서버가 클러스터를 떠난다(`cluster/leave`) — 떠난 뒤 서버는 그 DB를 내려 `cluster/status`가
    /// `unknown cluster`다. 이미 떠났거나 서버가 돌지 않으면 그대로다(DB 파일이 있으면 떠나지 않은 것일 수 있으므로
    /// 서버가 도는지 먼저 본다).
    pub async fn leave_cluster(&self, db: Db) -> Result<(), OvnError> {
        let ctl = path_str(&self.paths.db_ctl(db));
        let status = |out: &CmdOutput| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        let left = |text: &str| text.contains("unknown cluster");
        let probe = Cmd::new(
            "ovs-appctl",
            &[
                "--timeout",
                CTL_TIMEOUT,
                "-t",
                &ctl,
                "cluster/status",
                db.name(),
            ],
        );
        let out = self.exec(&probe).await?;
        if !out.success() {
            if left(&status(&out)) {
                return Ok(());
            }
            return Err(OvnError::Command(format!(
                "cannot leave the {} cluster: its server is not running ({})",
                db.name(),
                tail(&status(&out), ERROR_LINES).unwrap_or_default()
            )));
        }
        self.run_ok(&Cmd::new(
            "ovs-appctl",
            &[
                "--timeout",
                CTL_TIMEOUT,
                "-t",
                &ctl,
                "cluster/leave",
                db.name(),
            ],
        ))
        .await?;
        let what = match db {
            Db::Nb => "the OVN northbound database to leave the cluster",
            Db::Sb => "the OVN southbound database to leave the cluster",
        };
        let deadline = tokio::time::Instant::now() + self.options.wait_limit;
        loop {
            let out = self.exec(&probe).await?;
            if !out.success() && left(&status(&out)) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(OvnError::Timeout(what));
            }
            tokio::time::sleep(self.options.wait_step).await;
        }
    }

    /// 떠난 DB 파일을 지우지 않고 `{nb,sb}.db.left-<유닉스 초>`로 이름만 바꾼다. 바꾼 파일을 돌려준다.
    pub fn retire_dbs(&self, now: u64) -> Result<Vec<PathBuf>, OvnError> {
        let mut moved = Vec::new();
        for db in Db::ALL {
            let path = self.paths.db_file(db);
            if !path.exists() {
                continue;
            }
            let to = self
                .paths
                .var_lib
                .join(format!("{}.db.left-{now}", db.short()));
            std::fs::rename(&path, &to).map_err(|e| io_at(&path, e))?;
            moved.push(to);
        }
        sync_dir(&self.paths.var_lib);
        Ok(moved)
    }

    /// 업그레이드한 꾸러미의 스키마가 DB보다 새로우면 이 서버가 리더일 때 변환한다(`ovsdb-client convert` — 배포판
    /// `ovn-ctl`의 업그레이드가 하던 일). 변환했으면 참
    pub async fn convert_if_needed(&self, db: Db) -> Result<bool, OvnError> {
        if !self.cluster_status(db).await?.is_leader() {
            return Ok(false);
        }
        let server = format!("unix:{}", self.paths.db_socket(db).display());
        let schema = path_str(&self.paths.schema(db));
        let out = self
            .run_ok(&Cmd::new(
                "ovsdb-client",
                &["needs-conversion", &server, &schema],
            ))
            .await?;
        if String::from_utf8_lossy(&out.stdout).trim() != "yes" {
            return Ok(false);
        }
        self.run_ok(
            &Cmd::new("ovsdb-client", &["convert", &server, &schema]).timeout(CONVERT_TIMEOUT),
        )
        .await?;
        Ok(true)
    }

    /// 이 노드가 중앙이면 NB·SB의 Raft 역할과 northd가 일하는지(읽지 못한 것은 `Down`·거짓)
    pub async fn central_health(&self) -> CentralHealth {
        let role = |s: Result<ClusterStatus, OvnError>| s.map_or(RaftRole::Down, |s| s.raft_role());
        let nb = role(self.cluster_status(Db::Nb).await);
        let sb = role(self.cluster_status(Db::Sb).await);
        let northd_active = self
            .run_ok(&Cmd::new(
                "ovn-appctl",
                &[
                    "--timeout",
                    CTL_TIMEOUT,
                    "-t",
                    &path_str(&self.paths.northd_ctl()),
                    "status",
                ],
            ))
            .await
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "Status: active");
        CentralHealth {
            nb,
            sb,
            northd_active,
        }
    }

    // -------------------------------------------------------------------------------------------------------
    // chassis
    // -------------------------------------------------------------------------------------------------------

    /// 로컬 OVS DB의 `external_ids`. OVS가 없으면 None
    pub async fn external_ids(&self) -> Result<Option<BTreeMap<String, String>>, OvnError> {
        let Some(out) = self
            .run_optional(&Cmd::new(
                "ovs-vsctl",
                &[
                    "--format=json",
                    "--columns=external_ids",
                    "list",
                    "Open_vSwitch",
                ],
            ))
            .await?
        else {
            return Ok(None);
        };
        parse_external_ids(&out.stdout)
            .map(Some)
            .map_err(OvnError::Command)
    }

    /// 이 노드를 이 클러스터의 chassis로 구성한다(§7.1): `external_ids`의 `system-id`(노드 UUID)·`ovn-remote`(중앙
    /// 모두의 SB)·`ovn-encap-type`(geneve)·`ovn-encap-ip`·`orbivisor-cluster`(태그), ovn-controller가 읽는 OVS
    /// `SSL`(노드 TLS 사본), OVS가 켜질 때마다 `system-id`로 다시 쓰는 `system-id.conf`, 그리고 `ovn-host`·
    /// `ovn-controller`를 켠다. 거절 검사는 부르는 쪽이 먼저 한다. 무엇이든 바꿨으면 참
    pub async fn configure_chassis(&self, chassis: &Chassis<'_>) -> Result<bool, OvnError> {
        let changed = self.apply_chassis(chassis).await?;
        self.run_ok(&Cmd::new("systemctl", &["enable", "--now", HOST_UNIT]))
            .await?;
        self.run_ok(&Cmd::new("systemctl", &["start", CONTROLLER_UNIT]))
            .await?;
        Ok(changed)
    }

    /// 이미 이 클러스터의 chassis인 노드의 구성을 맞춘다(에이전트 — 중앙이 바뀌거나 underlay가 바뀌면). 단위는 건드리지
    /// 않는다. 이 클러스터의 chassis가 아니면 아무것도 하지 않고 거짓
    pub async fn reconcile_chassis(&self, chassis: &Chassis<'_>) -> Result<bool, OvnError> {
        match self.external_ids().await? {
            Some(ids) if ids.get(TAG_KEY) == Some(&self.tag) => self.apply_chassis(chassis).await,
            _ => Ok(false),
        }
    }

    async fn apply_chassis(&self, chassis: &Chassis<'_>) -> Result<bool, OvnError> {
        if chassis.centrals.is_empty() {
            return Err(OvnError::Command("the SDN has no OVN central yet".into()));
        }
        let ids = self.external_ids().await?.ok_or_else(|| {
            OvnError::Command("Open vSwitch is not installed (ovs-vsctl is missing)".into())
        })?;
        let mut changed = false;
        let ssl = self.ssl().await?;
        let want_ssl = SslConfig {
            private_key: Some(path_str(&self.paths.tls_key())),
            certificate: Some(path_str(&self.paths.tls_cert())),
            ca_cert: Some(path_str(&self.paths.tls_ca())),
        };
        if ssl != want_ssl {
            self.run_ok(&Cmd::new(
                "ovs-vsctl",
                &[
                    "set-ssl",
                    &path_str(&self.paths.tls_key()),
                    &path_str(&self.paths.tls_cert()),
                    &path_str(&self.paths.tls_ca()),
                ],
            ))
            .await?;
            changed = true;
        }
        // SSL을 먼저 둔다(ovn-controller가 `ssl:` 원격을 보자마자 붙을 수 있게).
        let want = chassis_ids(chassis, &self.tag);
        let mut args = vec!["set".to_string(), "Open_vSwitch".into(), ".".into()];
        for (key, value) in &want {
            if ids.get(*key) != Some(value) {
                args.push(format!("external_ids:{key}={}", quote(value)));
            }
        }
        if args.len() > 3 {
            self.run_ok(&Cmd::new("ovs-vsctl", &args)).await?;
            changed = true;
        }
        let id = format!("{}\n", chassis.node);
        if read_optional(&self.paths.ovs_system_id)?
            .as_deref()
            .map(str::trim)
            != Some(chassis.node.to_string().as_str())
        {
            write_file(&self.paths.ovs_system_id, id.as_bytes(), 0o644)?;
            changed = true;
        }
        Ok(changed)
    }

    async fn ssl(&self) -> Result<SslConfig, OvnError> {
        let out = self.run_ok(&Cmd::new("ovs-vsctl", &["get-ssl"])).await?;
        Ok(parse_ssl(&String::from_utf8_lossy(&out.stdout)))
    }

    /// 이 노드의 chassis 상태: 이 클러스터 표시가 있으면 ovn-controller의 SB 연결(`connection-status`)과 SB의
    /// `Chassis` 행(`ovn-sbctl find Chassis name=<노드 UUID>`, 노드 TLS로 중앙에)
    pub async fn chassis_probe(&self, node: Uuid, centrals: &[IpAddr]) -> ChassisProbe {
        let ids = match self.external_ids().await {
            Ok(Some(ids)) if ids.get(TAG_KEY) == Some(&self.tag) => ids,
            _ => return ChassisProbe::NotConfigured,
        };
        let encap_ip = ids.get("ovn-encap-ip").and_then(|ip| ip.parse().ok());
        let connected = self
            .run_ok(&Cmd::new(
                "ovn-appctl",
                &[
                    "--timeout",
                    CTL_TIMEOUT,
                    "-t",
                    "ovn-controller",
                    "connection-status",
                ],
            ))
            .await
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "connected");
        let registered = if centrals.is_empty() {
            None
        } else {
            self.run_ok(&Cmd::new(
                "ovn-sbctl",
                &[
                    &format!("--db={}", remotes(centrals, Db::Sb.port())),
                    &format!("--private-key={}", path_str(&self.paths.tls_key())),
                    &format!("--certificate={}", path_str(&self.paths.tls_cert())),
                    &format!("--ca-cert={}", path_str(&self.paths.tls_ca())),
                    &format!("--timeout={CTL_TIMEOUT}"),
                    "--no-leader-only",
                    "--format=json",
                    "--columns=name",
                    "find",
                    "Chassis",
                    &format!("name={node}"),
                ],
            ))
            .await
            .ok()
            .and_then(|out| parse_table(&out.stdout).ok())
            .map(|rows| !rows.is_empty())
        };
        ChassisProbe::Configured {
            connected,
            registered,
            encap_ip,
        }
    }

    /// 클러스터에서 빠질 때: 이 클러스터의 chassis면 `orbivisor-cluster`·`ovn-remote`만 지우고 ovn-controller를
    /// 멈춘다(꾸러미·다른 칸은 남김). 지웠으면 참
    pub async fn unconfigure_chassis(&self) -> Result<bool, OvnError> {
        match self.external_ids().await? {
            Some(ids) if ids.get(TAG_KEY) == Some(&self.tag) => {}
            _ => return Ok(false),
        }
        self.run_ok(&Cmd::new(
            "ovs-vsctl",
            &[
                "remove",
                "Open_vSwitch",
                ".",
                "external_ids",
                TAG_KEY,
                "ovn-remote",
            ],
        ))
        .await?;
        self.run_ok(&Cmd::new("systemctl", &["stop", CONTROLLER_UNIT]))
            .await?;
        Ok(true)
    }

    /// 이 클러스터 표시가 있는 OVS `Interface`들
    pub async fn tagged_interfaces(&self) -> Result<Vec<OvsInterface>, OvnError> {
        let Some(out) = self
            .run_optional(&Cmd::new(
                "ovs-vsctl",
                &[
                    "--format=json",
                    "--columns=name,ofport,external_ids",
                    "find",
                    "Interface",
                    &format!("external_ids:{TAG_KEY}={}", self.tag),
                ],
            ))
            .await?
        else {
            return Ok(Vec::new());
        };
        parse_interfaces(&out.stdout).map_err(OvnError::Command)
    }

    /// 오래된 OVS 포트를 지운다(§7.2): 이 클러스터 표시가 있고 장치가 없으며(`ofport` -1 — 재부팅 뒤 OVS DB에 남은
    /// 포트) `keep`(이 노드에서 도는 VM의 tap)에 없는 것. 남의 표시·표시 없는 포트는 건드리지 않는다. 지운 포트 이름
    pub async fn remove_stale_ports(
        &self,
        keep: &BTreeSet<String>,
    ) -> Result<Vec<String>, OvnError> {
        let stale = stale_ports(&self.tagged_interfaces().await?, keep);
        for name in &stale {
            self.run_ok(&Cmd::new(
                "ovs-vsctl",
                &["--if-exists", "del-port", "br-int", name],
            ))
            .await?;
        }
        Ok(stale)
    }

    // -------------------------------------------------------------------------------------------------------
    // 명령
    // -------------------------------------------------------------------------------------------------------

    /// 명령을 돌린다. 시간이 넘으면 그 명령 줄을 담아 실패한다.
    async fn exec(&self, cmd: &Cmd) -> Result<CmdOutput, OvnError> {
        match self.runner.run(cmd).await {
            Err(DriverError::Timeout(_)) => Err(OvnError::Command(format!(
                "{} timed out after {}s",
                cmd.line(),
                cmd.timeout.as_secs()
            ))),
            other => Ok(other?),
        }
    }

    /// 명령을 돌려 종료 0이 아니면 `{명령 줄} failed: {stderr 끝 줄}`로 실패한다.
    async fn run_ok(&self, cmd: &Cmd) -> Result<CmdOutput, OvnError> {
        let out = self.exec(cmd).await?;
        if out.success() {
            return Ok(out);
        }
        Err(OvnError::Command(format!(
            "{} failed: {}",
            cmd.line(),
            tail(&String::from_utf8_lossy(&out.stderr), ERROR_LINES)
                .unwrap_or_else(|| format!("exit status {}", out.status))
        )))
    }

    /// [`run_ok`](Self::run_ok)와 같되 프로그램이 없으면 None이다.
    async fn run_optional(&self, cmd: &Cmd) -> Result<Option<CmdOutput>, OvnError> {
        match self.run_ok(cmd).await {
            Err(OvnError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            other => other.map(Some),
        }
    }
}

/// Orbivisor 단위 셋(NB, SB, northd)
pub fn central_units() -> Vec<String> {
    let mut units: Vec<String> = Db::ALL.iter().map(|db| db.unit()).collect();
    units.push(NORTHD_UNIT.to_string());
    units
}

/// chassis의 바라는 `external_ids`(키 → 값)
pub fn chassis_ids(chassis: &Chassis<'_>, tag: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("system-id", chassis.node.to_string()),
        ("ovn-remote", remotes(chassis.centrals, Db::Sb.port())),
        ("ovn-encap-type", "geneve".to_string()),
        ("ovn-encap-ip", chassis.encap_ip.to_string()),
        (TAG_KEY, tag.to_string()),
    ])
}

/// 지울 오래된 포트: 장치가 없고(`ofport` < 0) `keep`에 없는 것(이름 순)
pub fn stale_ports(interfaces: &[OvsInterface], keep: &BTreeSet<String>) -> Vec<String> {
    let mut stale: Vec<String> = interfaces
        .iter()
        .filter(|i| i.ofport.is_some_and(|p| p < 0) && !keep.contains(&i.name))
        .map(|i| i.name.clone())
        .collect();
    stale.sort();
    stale
}

/// `ovs-vsctl set`의 문자열 값(쉼표·콜론이 든 값도 한 값으로 — OVSDB 문자열 문법의 JSON 따옴표)
pub fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 비지 않은 끝 `n`줄(모두 비었으면 None)
fn tail(text: &str, n: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines[lines.len().saturating_sub(n)..].join("\n"))
}

fn io_at(path: &Path, e: std::io::Error) -> OvnError {
    OvnError::Io(std::io::Error::new(
        e.kind(),
        format!("{}: {e}", path.display()),
    ))
}

/// 파일 내용(없으면 None)
fn read_optional(path: &Path) -> Result<Option<String>, OvnError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_at(path, e)),
    }
}

/// 디렉터리를 만들고(부모까지) 모드를 맞춘다.
fn make_dir(path: &Path, mode: u32) -> Result<(), OvnError> {
    std::fs::create_dir_all(path).map_err(|e| io_at(path, e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io_at(path, e))
}

/// 파일을 지운다(없으면 그대로).
fn remove_file(path: &Path) -> Result<(), OvnError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_at(path, e)),
        _ => Ok(()),
    }
}

/// 디렉터리의 이름 바꾸기를 디스크에 쓴다(실패는 무시 — 다음 이름 바꾸기가 다시 한다).
fn sync_dir(path: &Path) {
    if let Ok(dir) = std::fs::File::open(path) {
        let _ = dir.sync_all();
    }
}

/// 같은 디렉터리의 고유한 임시 파일 이름(`.{name}.opl-tmp-{pid}-{n}`) — 에이전트와 Task가 같은 파일을 함께 써도
/// 서로의 임시 파일을 덮지 않는다.
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

/// 파일을 임시 파일 → fsync → rename으로 바꾼다(부모 디렉터리도 만든다). 내용과 모드가 같으면 그대로 두고 `false`다.
fn write_file(path: &Path, content: &[u8], mode: u32) -> Result<bool, OvnError> {
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
mod tests;
