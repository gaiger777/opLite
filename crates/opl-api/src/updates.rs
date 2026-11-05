//! 노드의 APT 업데이트: opLite 저장소 소스 파일 관리, 업데이트 확인(`apt-get -s dist-upgrade`),
//! 주기 확인 에이전트, 다시 확인 API(`POST /nodes/{key}/updates/refresh`), 업그레이드(`POST
//! /nodes/{key}/upgrade`, `systemd-run`의 일시 유닛에서 `apt-get dist-upgrade`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec, NodeStatus, NodeUpdates, PackageUpdate};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::settings::{Channel, SettingsSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::version::{BUILD, SUITE};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, node_name, now};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// opld가 쓴 소스 파일의 첫 줄. 이 줄이 없는 파일은 관리자가 직접 관리하는 것으로 보고 건드리지 않는다.
pub const SOURCES_MARK: &str = "# managed by opld; edits are overwritten";
/// 패키지가 싣는 저장소 공개 키(`UpdatesConfig.keyring`의 기본값)
pub const KEYRING: &str = "/usr/share/keyrings/oplite-archive-keyring.gpg";
/// `NodeStatus.updates`에 담는 패키지 수의 상한
pub const MAX_PACKAGES: usize = 500;
/// 다른 확인이 도는 중일 때의 오류
const CHECK_BUSY: &str = "another update check is running";
/// 확인 명령이 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 60초까지 기다리게 하는 apt 설정
const CHECK_LOCK_TIMEOUT: &str = "DPkg::Lock::Timeout=60";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// 설정 구독을 하지 못했을 때 다시 해 보는 간격
const RESUBSCRIBE: Duration = Duration::from_secs(10);
/// 업그레이드 셸이 로그 끝에 남기는 종료 상태 줄의 앞부분. `--collect`로 유닛이 사라진 뒤에도 결과를 안다.
const EXIT_MARK: &str = "opl-upgrade: exit ";
/// 업그레이드 셸이 맨 처음 로그에 남기는 줄. 없으면 유닛이 셸을 시작하지 못했다.
const START_MARK: &str = "opl-upgrade: start";
/// apt가 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 기다리는 시간(초)
const LOCK_TIMEOUT: u32 = 600;
/// 실패한 업그레이드의 오류에 담는 로그 끝 줄 수
const LOG_LINES: usize = 20;

/// 노드 업데이트 설정(opld `[updates]`를 푼 값). 업데이트가 꺼진 노드에는 없다.
#[derive(Debug, Clone)]
pub struct UpdatesConfig {
    /// 주기 확인 간격
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    pub first_check: Duration,
    /// opLite APT 저장소 주소. 비면 소스 파일을 쓰지 않는다.
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    pub sources_file: PathBuf,
    /// 저장소 공개 키. 없으면 소스 파일을 쓰지 않는다(apt가 서명을 확인하지 못한다).
    pub keyring: PathBuf,
    pub apt_get: PathBuf,
    pub systemd_run: PathBuf,
    pub systemctl: PathBuf,
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    pub poll: Duration,
}

/// 이 노드의 업데이트 확인. 확인은 한 번에 하나만 돈다.
#[derive(Debug)]
pub struct Updates {
    pub config: UpdatesConfig,
    lock: tokio::sync::Mutex<()>,
}

impl Updates {
    pub fn new(config: UpdatesConfig) -> Self {
        Self {
            config,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `apt-get update -q` 뒤 `apt-get -s -q dist-upgrade`의 `Inst` 줄로 올릴 패키지를 찾는다. 명령이
    /// 실패하거나 다른 확인이 도는 중이면(기다리지 않는다) `error`에 이유를 담는다.
    pub async fn check(&self) -> NodeUpdates {
        let Ok(_guard) = self.lock.try_lock() else {
            return failed(CHECK_BUSY.into());
        };
        self.check_locked().await
    }

    /// 다른 확인이 끝나기를 기다렸다가 확인한다. 잠금 줄에 서지 않고 `poll`마다 다시 해 본다. 잠금이
    /// 잡혀 있고 `give_up()`이면(업그레이드가 잠금을 잡았다) 확인하지 않고 `None`이다.
    async fn check_waiting(&self, give_up: impl Fn() -> bool) -> Option<NodeUpdates> {
        loop {
            if let Ok(_guard) = self.lock.try_lock() {
                return Some(self.check_locked().await);
            }
            if give_up() {
                return None;
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    async fn check_locked(&self) -> NodeUpdates {
        if let Err(e) = self.apt(&["update", "-q", "-o", CHECK_LOCK_TIMEOUT]).await {
            return failed(e);
        }
        match self
            .apt(&["-s", "-q", "-o", CHECK_LOCK_TIMEOUT, "dist-upgrade"])
            .await
        {
            Ok(out) => found(parse_simulate(&out), now()),
            Err(e) => failed(e),
        }
    }

    /// 확인해 결과를 노드 `node`의 `status.updates`에 쓴다. 확인이 실패했으면(결과를 쓴 뒤) 그 이유가
    /// 오류다. 다른 확인이 도는 중이면 쓰지 않는다(그 확인이 쓴다).
    pub(crate) async fn refresh(&self, cluster: &Cluster, node: Uuid) -> Result<(), String> {
        let updates = self.check().await;
        if updates.error.as_deref() == Some(CHECK_BUSY) {
            return Err(CHECK_BUSY.into());
        }
        self.record_check(cluster, node, updates).await
    }

    /// [`Self::refresh`]와 같지만 다른 확인이 도는 중이면 끝나기를 기다린다(사용자가 요청한 확인). 이
    /// 노드를 업그레이드하는 중이면 확인하지 않고 성공이다(업그레이드가 끝나면 확인한다).
    pub(crate) async fn refresh_waiting(
        &self,
        cluster: &Cluster,
        node: Uuid,
    ) -> Result<(), String> {
        match self.check_waiting(|| upgrading(cluster, node)).await {
            Some(updates) => self.record_check(cluster, node, updates).await,
            None => Ok(()),
        }
    }

    async fn record_check(
        &self,
        cluster: &Cluster,
        node: Uuid,
        updates: NodeUpdates,
    ) -> Result<(), String> {
        record(cluster, node, &updates)
            .await
            .map_err(|e| format!("cannot record the update check: {e}"))?;
        match updates.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// `apt-get <args>`(LANG=C)의 표준 출력. 실패하면 명령과 stderr 끝 줄이 오류다.
    async fn apt(&self, args: &[&str]) -> Result<String, String> {
        let apt_get = &self.config.apt_get;
        let output = tokio::process::Command::new(apt_get)
            .args(args)
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            // 에이전트가 멈추면(opld 종료) 확인도 끝낸다.
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", apt_get.display()))?;
        if !output.status.success() {
            return Err(format!(
                "apt-get {} failed ({}): {}",
                args.join(" "),
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// 업그레이드 Task `task`를 실행한다: 일시 유닛에서 apt를 돌리고(`resume`이면 이전 opld가 띄운
    /// 유닛을 이어서 본다) 유닛이 끝날 때까지 지켜본다. 성공이든 실패든 끝나면 업데이트를 다시 확인한다.
    /// 클러스터 어디든 다른 업그레이드가 돌고 있으면(두 요청이 함께 지나갔다) 시작하지 않는다.
    /// 업그레이드하는 동안 확인 잠금을 잡아 apt 잠금을 다투지 않고, 잠금을 잡은 채로 마지막 확인을 한다.
    pub(crate) async fn upgrade(
        &self,
        cluster: &Cluster,
        node: Uuid,
        task: Uuid,
        resume: bool,
    ) -> Result<(), String> {
        if !resume && let Some(other) = other_upgrade(cluster, task) {
            return Err(format!(
                "upgrade_in_progress: node {other:?} is being upgraded"
            ));
        }
        let (result, updates) = {
            let _guard = self.lock.lock().await;
            let result = self.run_upgrade(task, resume).await;
            (result, self.check_locked().await)
        };
        if let Err(e) = self.record_check(cluster, node, updates).await {
            tracing::warn!(error = %e, "update check after the upgrade failed");
        }
        result
    }

    async fn run_upgrade(&self, task: Uuid, resume: bool) -> Result<(), String> {
        let unit = upgrade_unit(task);
        let log = self.config.log_dir.join(format!("upgrade-{task}.log"));
        // 이 opld가 유닛을 띄웠는지. 이어 볼 때는 이전 opld가 띄우지 못했을 수 있다.
        let mut started = !resume;
        if started {
            self.start_unit(&unit, &log).await?;
        }
        loop {
            match self.unit_state(&unit).await? {
                Some(UnitState::Running) | None => {}
                Some(UnitState::Succeeded) => return Ok(()),
                Some(UnitState::Failed) => return Err(upgrade_failed(&log).await),
                Some(UnitState::Gone) => {
                    let text = read_log(&log).await;
                    if text.lines().any(|l| l == START_MARK) {
                        return self.judge_gone(&log, &text).await;
                    }
                    if started {
                        return Err("the upgrade did not start; try again".into());
                    }
                    // 이전 opld가 유닛을 띄우기 전에 멈췄다. apt는 다시 돌려도 된다.
                    self.start_unit(&unit, &log).await?;
                    started = true;
                    continue;
                }
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    /// `systemd-run`으로 apt를 opld 밖의 일시 유닛에서 띄운다. 패키지의 postinst가 opld를 다시
    /// 시작해도 apt는 끊기지 않는다. 출력은 로그 파일에 더한다.
    async fn start_unit(&self, unit: &str, log: &Path) -> Result<(), String> {
        let c = &self.config;
        tokio::fs::create_dir_all(&c.log_dir)
            .await
            .map_err(|e| format!("cannot create {}: {e}", c.log_dir.display()))?;
        let output = tokio::process::Command::new(&c.systemd_run)
            .arg(format!("--unit={unit}"))
            .arg("--collect")
            .arg("--property=Type=exec")
            .arg(format!(
                "--property=StandardOutput=append:{}",
                log.display()
            ))
            .arg(format!("--property=StandardError=append:{}", log.display()))
            .arg("--setenv=DEBIAN_FRONTEND=noninteractive")
            .arg("--setenv=LANG=C")
            .args(["/bin/sh", "-c", &upgrade_script(&c.apt_get)])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", c.systemd_run.display()))?;
        if !output.status.success() {
            return Err(format!(
                "systemd-run failed ({}): {}",
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(())
    }

    /// `systemctl show`로 읽은 유닛 상태. `systemctl`이 실패하면(systemd 자신을 올리는 중 등) `None`이다
    /// (다음에 다시 본다).
    async fn unit_state(&self, unit: &str) -> Result<Option<UnitState>, String> {
        let systemctl = &self.config.systemctl;
        let output = tokio::process::Command::new(systemctl)
            .args([
                "show",
                "-p",
                "ActiveState",
                "-p",
                "Result",
                "-p",
                "LoadState",
            ])
            .arg(format!("{unit}.service"))
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", systemctl.display()))?;
        if !output.status.success() {
            tracing::warn!(
                unit,
                status = %output.status,
                stderr = %tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES),
                "cannot read the upgrade unit; will retry"
            );
            return Ok(None);
        }
        Ok(Some(unit_state(&String::from_utf8_lossy(&output.stdout))))
    }

    /// 셸을 시작한 유닛이 사라졌다(`--collect`가 치웠다). 로그 `text`의 종료 상태 줄이 있으면 그것으로
    /// 판단한다. 없으면 apt가 도중에 끊겼다(전원이 꺼진 경우 등): `dpkg --audit`가 무언가 알리면 dpkg를
    /// 마저 설정하라고, 아니면 다시 시작하라고 실패한다.
    async fn judge_gone(&self, log: &Path, text: &str) -> Result<(), String> {
        match exit_status(text) {
            Some(0) => return Ok(()),
            Some(_) => return Err(upgrade_failed(log).await),
            None => {}
        }
        let dpkg = &self.config.dpkg;
        let output = tokio::process::Command::new(dpkg)
            .arg("--audit")
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", dpkg.display()))?;
        if output.status.success() && output.stdout.iter().all(u8::is_ascii_whitespace) {
            Err("the upgrade was interrupted; start it again".into())
        } else {
            Err("the upgrade was interrupted; run \"dpkg --configure -a\" on the node".into())
        }
    }

    /// 소스 파일을 지금 설정의 채널로 맞춘다(`apt_url`이 있을 때만).
    /// 파일을 새로 썼으면 true다.
    fn sync_sources(&self, cluster: &Cluster) -> bool {
        if self.config.apt_url.is_empty() {
            return false;
        }
        let channel = match crate::settings::current(cluster) {
            Ok(settings) => settings.spec.updates.channel,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the settings for the APT sources");
                return false;
            }
        };
        let c = &self.config;
        let path = &c.sources_file;
        match write_sources(path, &c.apt_url, channel, &c.keyring) {
            Ok(SourcesOutcome::Written) => {
                tracing::info!(file = %path.display(), channel = channel.as_str(), "wrote the APT sources");
                true
            }
            Ok(
                SourcesOutcome::Unchanged | SourcesOutcome::Unmanaged | SourcesOutcome::NoKeyring,
            ) => false,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot write the APT sources");
                false
            }
        }
    }
}

fn failed(error: String) -> NodeUpdates {
    NodeUpdates {
        checked_at: Some(now()),
        error: Some(error),
        ..NodeUpdates::default()
    }
}

/// 찾은 패키지의 확인 결과. [`MAX_PACKAGES`]가 넘으면 앞의 것만 담는다.
fn found(mut packages: Vec<PackageUpdate>, checked_at: u64) -> NodeUpdates {
    let truncated = packages.len() > MAX_PACKAGES;
    packages.truncate(MAX_PACKAGES);
    NodeUpdates {
        checked_at: Some(checked_at),
        packages,
        truncated,
        error: None,
    }
}

/// 비지 않은 마지막 `n`줄
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 업그레이드 Task `task`의 일시 유닛 이름(Task id 전체). opld가 다시 시작해도 Task id로 다시 만든다.
fn upgrade_unit(task: Uuid) -> String {
    format!("opl-upgrade-{}", task.simple())
}

/// 업그레이드 유닛이 돌리는 셸 명령. 처음에 시작 줄([`START_MARK`]), 끝에 종료 상태 줄([`EXIT_MARK`])을
/// 로그에 남긴다. apt는 다른 apt의 잠금을 [`LOCK_TIMEOUT`]초까지 기다린다.
fn upgrade_script(apt_get: &Path) -> String {
    let apt = sh_quote(&apt_get.display().to_string());
    let lock = format!("-o DPkg::Lock::Timeout={LOCK_TIMEOUT}");
    format!(
        "echo \"{START_MARK}\"; {apt} update -q {lock} && {apt} -y -q {lock} \
         -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
         s=$?; echo \"{EXIT_MARK}$s\"; exit $s"
    )
}

/// 셸의 작은따옴표 인용
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 업그레이드 유닛의 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitState {
    Running,
    Succeeded,
    Failed,
    /// 유닛이 없다(끝나고 치워졌거나 노드가 다시 켜졌다).
    Gone,
}

/// `systemctl show -p ActiveState -p Result -p LoadState`의 `KEY=VALUE` 줄
fn unit_state(show: &str) -> UnitState {
    let value = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
    };
    let result = value("Result");
    match value("ActiveState") {
        _ if value("LoadState") == "not-found" => UnitState::Gone,
        "failed" => UnitState::Failed,
        "inactive" if result.is_empty() => UnitState::Gone,
        "inactive" if result == "success" => UnitState::Succeeded,
        "inactive" => UnitState::Failed,
        _ => UnitState::Running,
    }
}

/// 로그 끝의 종료 상태 줄([`EXIT_MARK`])에 적힌 값
fn exit_status(log: &str) -> Option<i32> {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix(EXIT_MARK))?
        .trim()
        .parse()
        .ok()
}

/// 업그레이드 로그(없으면 빈 문자열)
async fn read_log(log: &Path) -> String {
    tokio::fs::read(log)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// 실패한 업그레이드의 오류: 로그 끝 줄
async fn upgrade_failed(log: &Path) -> String {
    format!(
        "the upgrade failed; last log lines:\n{}",
        tail(&read_log(log).await, LOG_LINES)
    )
}

/// `apt-get -s dist-upgrade` 출력의 `Inst <pkg> [<cur>] (<new> <suite…> [<arch>])` 줄. `[<cur>]`가
/// 없으면 새로 설치할 패키지다. `Conf`·`Remv`와 모양이 다른 줄은 건너뛴다.
pub(crate) fn parse_simulate(text: &str) -> Vec<PackageUpdate> {
    text.lines().filter_map(parse_inst).collect()
}

fn parse_inst(line: &str) -> Option<PackageUpdate> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    let mut rest = rest.trim_start();
    let mut current = None;
    if let Some(r) = rest.strip_prefix('[') {
        let (cur, r) = r.split_once(']')?;
        current = Some(cur.to_string());
        rest = r.trim_start();
    }
    let candidate = rest.strip_prefix('(')?.split_whitespace().next()?;
    Some(PackageUpdate {
        name: name.to_string(),
        current,
        candidate: candidate.trim_end_matches(')').to_string(),
    })
}

/// opLite 저장소의 deb822 소스 파일 내용
pub(crate) fn sources_contents(url: &str, channel: Channel, keyring: &Path) -> String {
    format!(
        "{SOURCES_MARK}\nTypes: deb\nURIs: {url}\nSuites: {SUITE}\nComponents: {}\nSigned-By: {}\n",
        channel.as_str(),
        keyring.display()
    )
}

/// [`write_sources`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcesOutcome {
    Written,
    /// 이미 같은 내용이다.
    Unchanged,
    /// 관리 표시가 없는 파일이다(관리자가 직접 관리한다). 건드리지 않았다.
    Unmanaged,
    /// 저장소 공개 키가 없다. 쓰지 않았고 opld가 쓴 파일이 있으면 지웠다.
    NoKeyring,
}

/// 소스 파일을 쓴다. 파일이 있는데 첫 줄이 [`SOURCES_MARK`]가 아니면(비었거나 공백뿐이면 opld 것으로
/// 본다) 건드리지 않고 경고한다. 공개 키 `keyring`이 없으면 apt가 서명을 확인하지 못해 `apt-get
/// update`가 실패하므로 쓰지 않고, opld가 쓴 파일은 지운다. 임시 파일(apt가 무시하는 `.dpkg-tmp`)에
/// 쓰고 디스크에 내린 뒤 바꿔 넣어 apt가 반쯤 쓴 파일을 읽지 않게 한다.
pub(crate) fn write_sources(
    path: &Path,
    url: &str,
    channel: Channel,
    keyring: &Path,
) -> std::io::Result<SourcesOutcome> {
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old
        && !old.trim().is_empty()
        && old.lines().next() != Some(SOURCES_MARK)
    {
        tracing::warn!(
            file = %path.display(),
            "the APT sources file has no opld mark; leaving it as it is"
        );
        return Ok(SourcesOutcome::Unmanaged);
    }
    if !keyring.exists() {
        tracing::warn!(
            keyring = %keyring.display(),
            "the opLite archive key is missing; not writing the APT sources"
        );
        if old.is_some() {
            std::fs::remove_file(path)?;
        }
        return Ok(SourcesOutcome::NoKeyring);
    }
    let contents = sources_contents(url, channel, keyring);
    if old.as_deref() == Some(contents.as_str()) {
        return Ok(SourcesOutcome::Unchanged);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".dpkg-tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        // 남은 임시 파일이나 umask와 상관없이 누구나 읽게 한다.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // 바꿔 넣은 것도 디스크에 내린다. 실패해도 파일은 이미 바뀌었다.
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(SourcesOutcome::Written)
}

/// 노드 `node`의 `status.updates`와 이 노드의 버전을 쓴다.
async fn record(cluster: &Cluster, node: Uuid, updates: &NodeUpdates) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        status.updates = Some(updates.clone());
        status.version = Some(BUILD.into());
        true
    })
    .await
}

/// 이 노드 status의 `version`과 `updates`가 비었으면(이 필드를 모르는 옛 opld가 리더일 때 status를
/// 다시 쓰며 지웠다) 다시 쓴다. 확인 결과는 남긴다.
async fn restore(cluster: &Cluster, node: Uuid) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        let stale = status.version.as_deref() != Some(BUILD) || status.updates.is_none();
        status.version = Some(BUILD.into());
        status.updates.get_or_insert_with(NodeUpdates::default);
        stale
    })
    .await
}

/// 노드 `node`의 status를 `change`로 고쳐 쓴다(false면 쓰지 않는다). status의 다른 필드는 opld도
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn change_status(
    cluster: &Cluster,
    node: Uuid,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<NodeSpec>(node)? else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(node, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 소스 파일을 시작할 때와 설정이 바뀔 때 맞추고, `first_check` 뒤 처음, 그 뒤 `check_interval`마다
/// 업데이트를 확인하는 에이전트. 업데이트가 켜진 노드에서 돈다.
pub struct UpdateAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl UpdateAgent {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, updates: Arc<Updates>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_agent(cluster, node, updates, rx)),
        }
    }

    /// 멈춘다. 도는 확인은 기다리지 않는다(apt 명령은 혼자 끝난다).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_agent(
    cluster: Arc<Cluster>,
    node: Uuid,
    updates: Arc<Updates>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut next = Instant::now() + updates.config.first_check;
    let mut watch = None;
    let mut sync = true;
    let mut started = false;
    if let Err(e) = restore(&cluster, node).await {
        tracing::warn!(error = %e, "cannot restore the node status");
    }
    loop {
        if sync {
            // 이번에 읽을 설정 뒤의 변경을 놓치지 않게 먼저 구독한다.
            if watch.is_none() {
                watch = cluster
                    .list::<SettingsSpec>()
                    .ok()
                    .and_then(|(revision, _)| cluster.watch(revision).ok());
            }
            // 채널을 바꿔 소스 파일을 새로 썼으면 곧 확인해 새 채널의 업데이트를 보인다.
            if updates.sync_sources(&cluster) && started {
                next = Instant::now();
            }
            sync = false;
            started = true;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = tokio::time::sleep_until(next) => {
                // 업그레이드 중에는 apt 잠금을 다투지 않는다. 업그레이드가 끝나면 스스로 확인한다.
                if upgrading(&cluster, node) {
                    tracing::debug!("skipping the update check during an upgrade");
                } else {
                    tokio::select! {
                        _ = &mut shutdown => return,
                        result = updates.refresh(&cluster, node) => {
                            if let Err(e) = result {
                                tracing::warn!(error = %e, "update check failed");
                            }
                        }
                    }
                }
                next = Instant::now() + updates.config.check_interval;
            }
            () = settings_changed(&mut watch) => sync = true,
        }
    }
}

/// 노드 `node`에서 업그레이드 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn upgrading(cluster: &Cluster, node: Uuid) -> bool {
    let Ok((_, tasks)) = cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node && !t.status.state.is_finished()
    })
}

/// 업그레이드 Task `task` 말고 running인 업그레이드가 있으면 그 노드 이름(없는 노드면 id). 두 요청이
/// 함께 지나가 두 Task가 생겨도 running 쓰기는 Raft 순서대로 적용되므로 나중 것이 앞의 것을 본다.
fn other_upgrade(cluster: &Cluster, task: Uuid) -> Option<String> {
    let (_, tasks) = cluster.list::<TaskSpec>().ok()?;
    let other = tasks.into_iter().find(|t| {
        t.id != task && t.spec.op == TaskOp::NodeUpgrade && t.status.state == TaskState::Running
    })?;
    Some(match cluster.get::<NodeSpec>(other.spec.node) {
        Ok(Some(node)) => node.name,
        _ => other.spec.node.to_string(),
    })
}

/// 설정이 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 다시
/// 맞추게 한다. 구독이 없으면 잠시 뒤 돌아가 다시 구독하게 한다.
async fn settings_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Settings,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Settings,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

/// 업데이트를 확인하지 않는 노드(위트니스, 업데이트가 꺼진 노드, 옛 노드)면 400 `updates_unsupported`
pub(crate) fn check_updates_supported(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness || node.status.updates.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "updates_unsupported",
            format!("node {:?} does not check for updates", node.name),
        ));
    }
    Ok(())
}

/// 노드의 업데이트를 다시 확인할 Task(`node-update-check`, 그 노드에서 돈다)를 돌려준다(202). 결과는
/// Node `status.updates`에 쓴다. 그 노드에 끝나지 않은 확인 Task가 있으면 새로 만들지 않고 그것을
/// 돌려준다. 업데이트를 확인하지 않는 노드면 400 `updates_unsupported`, 오프라인이면 409 `node_offline`,
/// 옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다), 그 노드를
/// 업그레이드하는 중이면 409 `upgrade_in_progress`.
#[utoipa::path(post, path = "/nodes/{key}/updates/refresh", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_updates(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    // 업그레이드가 apt 잠금을 잡고 있다. 끝나면 실행기가 스스로 확인한다.
    if tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is being upgraded; it checks for updates when the upgrade ends",
                node.name
            ),
        ));
    }
    if let Some(task) = tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::NodeUpdateCheck
            && t.spec.node == node.id
            && !t.status.state.is_finished()
    }) {
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpdateCheck, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드의 패키지를 올릴 Task(`node-upgrade`, 그 노드에서 돈다)를 돌려준다(202). apt는 opld 밖의 일시
/// 유닛에서 돌므로 VM은 계속 돌고 opld만 잠깐 다시 시작한다. 업데이트를 확인하지 않는 노드면 400
/// `updates_unsupported`, 오프라인이면 409 `node_offline`, 클러스터 어디든 끝나지 않은 업그레이드가
/// 있으면 409 `upgrade_in_progress`(한 번에 한 노드 — 나머지 노드가 과반수와 서비스를 지킨다), 옛 노드가
/// 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/upgrade", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upgrade_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    // 이 프로세스에 함께 온 요청이 둘 다 검사를 지나 업그레이드 둘을 만들지 않게 한다. 다른 노드로 온
    // 요청과의 경합은 실행기가 막는다(`other_upgrade`).
    static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REQUESTS.lock().await;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(task) = tasks
        .iter()
        .find(|t| t.spec.op == TaskOp::NodeUpgrade && !t.status.state.is_finished())
    {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is still being upgraded; upgrade one node at a time",
                node_name(&state, task.spec.node)?
            ),
        ));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpgrade, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debian 13에서 `LANG=C apt-get -s -q dist-upgrade`가 내는 모양(보안 업데이트, 새 커널 의존성,
    /// 지울 패키지, opLite 저장소 패키지가 섞임)
    const SIMULATE: &str = "\
Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following packages will be REMOVED:
  libfoo1
The following NEW packages will be installed:
  linux-image-6.12.48+deb13-amd64
The following packages will be upgraded:
  base-files libc-bin libc6 linux-image-amd64 oplite
5 upgraded, 1 newly installed, 1 to remove and 0 not upgraded.
Remv libfoo1 [1.2-3]
Inst base-files [13.8+deb13u1] (13.8+deb13u2 Debian:13.2/stable [amd64])
Conf base-files (13.8+deb13u2 Debian:13.2/stable [amd64])
Inst libc6 [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64]) [libc-bin:amd64 ]
Inst libc-bin [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc6 (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc-bin (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Inst linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Inst linux-image-amd64 [6.12.41-1] (6.12.48-1 Debian:13.2/stable [amd64])
Inst oplite [0.2.0] (1:0.2.1~rc1 opLite:trixie [amd64])
Conf linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf linux-image-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf oplite (1:0.2.1~rc1 opLite:trixie [amd64])
";

    fn pkg(name: &str, current: Option<&str>, candidate: &str) -> PackageUpdate {
        PackageUpdate {
            name: name.into(),
            current: current.map(Into::into),
            candidate: candidate.into(),
        }
    }

    #[test]
    fn parse_simulate_reads_inst_lines() {
        assert_eq!(
            parse_simulate(SIMULATE),
            [
                pkg("base-files", Some("13.8+deb13u1"), "13.8+deb13u2"),
                pkg("libc6", Some("2.41-12"), "2.41-12+deb13u1"),
                pkg("libc-bin", Some("2.41-12"), "2.41-12+deb13u1"),
                // 새 커널 의존성: 설치된 버전이 없다.
                pkg("linux-image-6.12.48+deb13-amd64", None, "6.12.48-1"),
                pkg("linux-image-amd64", Some("6.12.41-1"), "6.12.48-1"),
                pkg("oplite", Some("0.2.0"), "1:0.2.1~rc1"),
            ]
        );
        assert!(parse_simulate("").is_empty());
        // 모양이 다른 줄은 건너뛴다.
        assert!(parse_simulate("Inst\nInst x [1.0\nInst y [1.0]\nInst z").is_empty());
    }

    #[test]
    fn found_keeps_at_most_max_packages() {
        let many: String = (0..600)
            .map(|i| format!("Inst p{i} [1.0] (1.1 Debian:13.2/stable [amd64])\n"))
            .collect();
        let packages = parse_simulate(&many);
        assert_eq!(packages.len(), 600);
        let updates = found(packages, 7);
        assert_eq!(updates.checked_at, Some(7));
        assert_eq!(updates.packages.len(), MAX_PACKAGES);
        assert_eq!(updates.packages[0].name, "p0");
        assert!(updates.truncated);
        assert_eq!(updates.error, None);
        let few = found(parse_simulate(SIMULATE), 7);
        assert_eq!(few.packages.len(), 6);
        assert!(!few.truncated);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\n\nc\nd\ne\nf\n", 5), "b\nc\nd\ne\nf");
        assert_eq!(tail("only\n", 5), "only");
        assert_eq!(tail("", 5), "");
    }

    fn missing_tools() -> Updates {
        let dir = PathBuf::from("/nonexistent/opl-updates");
        Updates::new(UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: dir.join("oplite.sources"),
            keyring: dir.join("keyring.gpg"),
            apt_get: dir.join("apt-get"),
            systemd_run: dir.join("systemd-run"),
            systemctl: dir.join("systemctl"),
            dpkg: dir.join("dpkg"),
            log_dir: dir.join("log"),
            poll: Duration::from_millis(20),
        })
    }

    #[tokio::test]
    async fn check_reports_a_missing_apt_get_and_does_not_wait_for_another_check() {
        let updates = missing_tools();
        let got = updates.check().await;
        assert_eq!(
            got.error.as_deref(),
            Some(
                "cannot run /nonexistent/opl-updates/apt-get: No such file or directory (os error 2)"
            )
        );
        assert!(got.checked_at.is_some());
        let _running = updates.lock.try_lock().unwrap();
        let got = updates.check().await;
        assert_eq!(got.error.as_deref(), Some(CHECK_BUSY));
        assert!(got.packages.is_empty());
    }

    /// 사용자가 요청한 확인은 도는 확인을 기다렸다가 확인한다.
    #[tokio::test]
    async fn a_waiting_check_waits_for_the_running_one() {
        let updates = missing_tools();
        let running = updates.lock.try_lock().unwrap();
        let waiting = updates.check_waiting(|| false);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        drop(running);
        let got = waiting.await.unwrap();
        assert!(got.error.unwrap().starts_with("cannot run "));
    }

    /// 업그레이드가 잠금을 잡았으면 기다리지 않고 확인하지 않는다.
    #[tokio::test]
    async fn a_waiting_check_gives_up_during_an_upgrade() {
        let updates = missing_tools();
        let _upgrading = updates.lock.try_lock().unwrap();
        assert!(updates.check_waiting(|| true).await.is_none());
    }

    #[test]
    fn unit_state_reads_systemctl_show() {
        let show = |active: &str, result: &str, load: &str| {
            unit_state(&format!(
                "ActiveState={active}\nResult={result}\nLoadState={load}\n"
            ))
        };
        assert_eq!(show("active", "success", "loaded"), UnitState::Running);
        assert_eq!(show("activating", "success", "loaded"), UnitState::Running);
        assert_eq!(
            show("deactivating", "success", "loaded"),
            UnitState::Running
        );
        assert_eq!(show("inactive", "success", "loaded"), UnitState::Succeeded);
        assert_eq!(show("failed", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "", "loaded"), UnitState::Gone);
        assert_eq!(show("inactive", "success", "not-found"), UnitState::Gone);
        // 줄 순서는 상관없다(systemctl은 자기 순서로 낸다).
        assert_eq!(
            unit_state("LoadState=loaded\nResult=success\nActiveState=inactive\n"),
            UnitState::Succeeded
        );
        // 알 수 없는 상태는 계속 지켜본다.
        assert_eq!(unit_state(""), UnitState::Running);
        assert_eq!(show("reloading", "success", "loaded"), UnitState::Running);
    }

    #[test]
    fn exit_status_reads_the_last_mark() {
        assert_eq!(
            exit_status("Setting up oplite (0.2.1) ...\nopl-upgrade: exit 0\n"),
            Some(0)
        );
        assert_eq!(
            exit_status("opl-upgrade: exit 0\nopl-upgrade: exit 100\n"),
            Some(100)
        );
        assert_eq!(exit_status("Unpacking oplite (0.2.1) ...\n"), None);
        assert_eq!(exit_status(""), None);
    }

    #[test]
    fn upgrade_unit_and_script() {
        let id: Uuid = "0193a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".parse().unwrap();
        assert_eq!(
            upgrade_unit(id),
            "opl-upgrade-0193a1b2c3d47e5f8a9b0c1d2e3f4a5b"
        );
        assert_eq!(
            upgrade_script(Path::new("/usr/bin/apt-get")),
            "echo \"opl-upgrade: start\"; \
             '/usr/bin/apt-get' update -q -o DPkg::Lock::Timeout=600 && \
             '/usr/bin/apt-get' -y -q -o DPkg::Lock::Timeout=600 \
             -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
             s=$?; echo \"opl-upgrade: exit $s\"; exit $s"
        );
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    /// 셸이 정말 돌면 로그는 시작 줄로 시작해 apt의 종료 상태 줄로 끝난다(따옴표가 든 경로도).
    #[test]
    fn upgrade_script_runs_in_sh() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("it's tools");
        std::fs::create_dir(&tools).unwrap();
        let apt_get = tools.join("apt-get");
        let run = |exit: u8| {
            std::fs::write(
                &apt_get,
                format!("#!/bin/sh\necho \"apt-get $*\"\nexit {exit}\n"),
            )
            .unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("chmod 755 \"$1\" && {}", upgrade_script(&apt_get)),
                    "sh",
                ])
                .arg(&apt_get)
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8(output.stdout).unwrap(),
            )
        };
        let (code, log) = run(0);
        assert_eq!(code, Some(0));
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines[0], START_MARK);
        assert_eq!(lines[1], "apt-get update -q -o DPkg::Lock::Timeout=600");
        assert!(lines[2].ends_with(" dist-upgrade"), "{log}");
        assert_eq!(lines[3], "opl-upgrade: exit 0");
        assert_eq!(exit_status(&log), Some(0));
        let (code, log) = run(100);
        assert_eq!(code, Some(100));
        assert_eq!(log.lines().last(), Some("opl-upgrade: exit 100"));
        assert_eq!(exit_status(&log), Some(100));
    }

    #[test]
    fn sources_are_deb822_with_the_mark() {
        assert_eq!(
            sources_contents(
                "https://apt.example.com/oplite",
                Channel::Testing,
                Path::new(KEYRING)
            ),
            "# managed by opld; edits are overwritten\n\
             Types: deb\n\
             URIs: https://apt.example.com/oplite\n\
             Suites: trixie\n\
             Components: testing\n\
             Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg\n"
        );
    }

    #[test]
    fn write_sources_writes_replaces_and_leaves_unmanaged_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        std::fs::write(&keyring, "").unwrap();
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sources_contents(url, Channel::Stable, &keyring)
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unchanged
        );
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Components: testing\n")
        );
        // 관리 표시가 없는 파일(관리자가 직접 쓴 것)은 그대로 둔다.
        let own =
            "Types: deb\nURIs: http://mirror.lan/oplite\nSuites: trixie\nComponents: stable\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
        // 비었거나 공백뿐인 파일은 opld 것으로 본다.
        for empty in ["", " \n\n"] {
            std::fs::write(&path, empty).unwrap();
            assert_eq!(
                write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
                SourcesOutcome::Written
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                sources_contents(url, Channel::Stable, &keyring)
            );
        }
        assert!(!dir.path().join("oplite.sources.dpkg-tmp").exists());
    }

    /// 공개 키가 없으면 쓰지 않고, opld가 쓴 파일은 지우고, 관리자의 파일은 그대로 둔다.
    #[test]
    fn write_sources_needs_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        std::fs::write(&path, sources_contents(url, Channel::Stable, &keyring)).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        let own = "Types: deb\nURIs: http://mirror.lan/oplite\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
    }
}
