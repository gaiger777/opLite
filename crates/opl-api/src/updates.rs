//! 노드의 APT 업데이트: opLite 저장소 소스 파일 관리, 업데이트 확인(`apt-get -s dist-upgrade`),
//! 주기 확인 에이전트, 다시 확인 API(`POST /nodes/{key}/updates/refresh`).

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
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::version::{BUILD, SUITE};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, now};
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

    /// 다른 확인이 끝나기를 기다렸다가 확인한다. 잠금 줄에 서지 않고 `poll`마다 다시 해 본다.
    async fn check_waiting(&self) -> NodeUpdates {
        loop {
            if let Ok(_guard) = self.lock.try_lock() {
                return self.check_locked().await;
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

    /// [`Self::refresh`]와 같지만 다른 확인이 도는 중이면 끝나기를 기다린다(사용자가 요청한 확인).
    pub(crate) async fn refresh_waiting(
        &self,
        cluster: &Cluster,
        node: Uuid,
    ) -> Result<(), String> {
        let updates = self.check_waiting().await;
        self.record_check(cluster, node, updates).await
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
                tokio::select! {
                    _ = &mut shutdown => return,
                    result = updates.refresh(&cluster, node) => {
                        if let Err(e) = result {
                            tracing::warn!(error = %e, "update check failed");
                        }
                    }
                }
                next = Instant::now() + updates.config.check_interval;
            }
            () = settings_changed(&mut watch) => sync = true,
        }
    }
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
/// 옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다).
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
        let waiting = updates.check_waiting();
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        drop(running);
        let got = waiting.await;
        assert!(got.error.unwrap().starts_with("cannot run "));
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
