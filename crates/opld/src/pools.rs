//! 노드별 풀 상태. 이 노드가 쓸 수 있는 풀마다 준비 여부와 용량을 `NodeStatus.pools`에 적는다.
//! NFS 풀은 `{data_dir}/pools/{풀 id}`에 마운트하고, 이 노드가 쓸 수 없게 된 NFS 풀은 언마운트한다.
//! opld가 내려가도 마운트는 그대로 둔다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Capacity;
use opl_types::node::{NodeSpec, NodeStatus, PoolState};
use opl_types::resource::Kind;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, VolumeSpec};
use opl_types::vm::{VmPhase, VmSpec};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// `mount`·`umount`를 기다리는 시간
pub const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);
/// 쓰기 확인과 용량 읽기를 기다리는 시간. NFS 서버가 멈추면 이 작업이 멈춘다.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const NFS_UNSUPPORTED: &str = "nfs pools are not supported on macOS";
const LVM_UNSUPPORTED: &str = "lvm-thin pools are not supported yet";
const NFS_NOT_RESPONDING: &str = "nfs server not responding";
const DIR_NOT_RESPONDING: &str = "pool directory not responding";
const MOUNT_IN_PROGRESS: &str = "mount in progress";
/// 마운트에 실패한 뒤 다시 시도하기까지 처음 기다리는 시간. 실패할 때마다 두 배로 늘린다.
const MOUNT_BACKOFF_INITIAL: Duration = Duration::from_secs(15);
const MOUNT_BACKOFF_MAX: Duration = Duration::from_secs(300);
/// 용량이 앞서 적은 값과 이만큼 바뀌어야 노드 상태를 다시 쓴다.
const GIB: u64 = 1 << 30;

/// 마운트와 풀 디렉터리 확인. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait MountOps: Send + Sync + 'static {
    /// `/proc/self/mountinfo`의 내용
    fn mountinfo(&self) -> std::io::Result<String>;
    /// `mount <args>`. 실패하면 stderr.
    async fn mount(&self, args: &[OsString]) -> Result<(), String>;
    /// `umount <dir>`. 실패하면 stderr.
    async fn umount(&self, dir: &Path) -> Result<(), String>;
    /// `dir`에 `probe` 파일을 만들고 지운 뒤 용량을 읽는다. 블로킹이고, 멈춘 NFS에서는 끝나지 않을 수 있다.
    fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity>;
}

/// 실제 명령과 파일시스템
pub struct SystemMount;

#[async_trait]
impl MountOps for SystemMount {
    fn mountinfo(&self) -> std::io::Result<String> {
        std::fs::read_to_string(opl_driver::mounts::MOUNTINFO)
    }

    async fn mount(&self, args: &[OsString]) -> Result<(), String> {
        run("mount", args).await
    }

    async fn umount(&self, dir: &Path) -> Result<(), String> {
        run("umount", &[dir.as_os_str().to_owned()]).await
    }

    fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity> {
        let file = dir.join(probe);
        std::fs::write(&file, b"opl")?;
        std::fs::remove_file(&file)?;
        let st = rustix::fs::statvfs(dir)?;
        Ok(Capacity {
            total_bytes: st.f_blocks * st.f_frsize,
            available_bytes: st.f_bavail * st.f_frsize,
        })
    }
}

/// 명령을 실행한다. 시간 제한으로 버려지면 프로세스를 죽인다.
async fn run(program: &str, args: &[OsString]) -> Result<(), String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(
            || format!("{program} failed: {}", out.status),
            str::to_string,
        ))
}

/// `mount -t nfs -o nosuid,nodev,noexec,retry=0[,options] server:export dir`의 인자. 풀에는 이미지
/// 파일만 두므로 setuid·장치 파일·실행을 늘 막는다. `retry=0`은 시간 제한으로 `mount`를 끝낸 뒤
/// mount.nfs가 앞에서 계속 다시 시도하지 않게 한다(다시 시도는 마운터가 한다). 사용자 옵션은 그
/// 뒤에 붙어 `retry`도 바꿀 수 있다. IPv6 서버는 `[...]`로 감싼다.
pub fn mount_args(server: &str, export: &str, options: Option<&str>, dir: &Path) -> Vec<OsString> {
    let source = if server.contains(':') {
        format!("[{server}]:{export}")
    } else {
        format!("{server}:{export}")
    };
    let mut opts = String::from("nosuid,nodev,noexec,retry=0");
    if let Some(options) = options {
        opts.push(',');
        opts.push_str(options);
    }
    vec![
        "-t".into(),
        "nfs".into(),
        "-o".into(),
        opts.into(),
        source.into(),
        dir.as_os_str().to_owned(),
    ]
}

/// 풀 상태를 주기적으로 새로 고치는 작업
pub struct PoolMounter {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl PoolMounter {
    pub fn spawn(cluster: Arc<Cluster>, mount_root: PathBuf, refresh: Duration) -> Self {
        let pools = Pools::new(cluster, mount_root, Arc::new(SystemMount));
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_pools(pools, refresh, rx)),
        }
    }

    /// 멈춘다. 마운트는 그대로 둔다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_pools(mut pools: Pools, refresh: Duration, mut shutdown: oneshot::Receiver<()>) {
    let done = pools.done.clone();
    let mut interval = tokio::time::interval(refresh);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut watch = None;
    loop {
        // 풀이 바뀌면(만들기·지우기) 곧바로 새로 고친다. 배치와 볼륨 Task는 준비된 노드만 고른다.
        tokio::select! {
            _ = &mut shutdown => return,
            // 마운트·언마운트가 끝나면 곧바로 결과를 적는다.
            () = done.notified() => {}
            _ = interval.tick() => {}
            () = pool_changed(&mut watch) => {}
        }
        // 이 새로 고침이 읽을 목록 뒤의 변경을 놓치지 않게 먼저 구독한다.
        if watch.is_none() {
            watch = pools
                .cluster
                .list::<StoragePoolSpec>()
                .ok()
                .and_then(|(revision, _)| pools.cluster.watch(revision).ok());
        }
        // 마운트·기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            r = pools.tick() => if let Err(e) = r {
                tracing::warn!(error = format!("{e:#}"), "cannot refresh storage pool states");
            }
        }
    }
}

/// 풀 리소스가 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 새로
/// 고친 뒤 다시 구독하게 한다. 구독이 없으면 기다리기만 한다.
async fn pool_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return std::future::pending().await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::StoragePool,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::StoragePool,
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

/// 한 번의 새로 고침에 필요한 것
struct Pools {
    cluster: Arc<Cluster>,
    node: Uuid,
    /// NFS 풀을 마운트하는 디렉터리(`{data_dir}/pools`)
    mount_root: PathBuf,
    ops: Arc<dyn MountOps>,
    /// 이 OS가 NFS 마운트를 지원하는지(Linux)
    nfs_supported: bool,
    mount_timeout: Duration,
    check_timeout: Duration,
    /// 시간 안에 끝나지 않은 확인 작업(풀 id별). 끝날 때까지 그 풀은 건너뛴다.
    stuck: HashMap<Uuid, JoinHandle<std::io::Result<Capacity>>>,
    /// 진행 중인 마운트(풀 id)와 언마운트(마운트 지점). 풀마다 따로 돌고, 끝나기 전에는 같은 풀의
    /// 마운트·언마운트를 새로 시작하지 않는다.
    mounting: HashSet<Uuid>,
    unmounting: HashSet<PathBuf>,
    /// 끝난 마운트·언마운트의 결과. 작업이 넣고 `done`으로 알린다.
    finished: Arc<Mutex<Vec<Finished>>>,
    /// 마운트·언마운트가 끝났음을 알린다. 새로 고침이 주기를 기다리지 않고 결과를 적는다.
    done: Arc<Notify>,
    /// 마운트에 실패한 풀의 다음 시도 시각(풀 id별)
    backoff: HashMap<Uuid, Backoff>,
    /// 마지막 언마운트 실패(같은 실패를 tick마다 기록하지 않는다)
    umount_errors: HashMap<PathBuf, String>,
}

/// 끝난 마운트·언마운트 작업
enum Finished {
    Mount(Uuid, Result<(), String>),
    Unmount(PathBuf, Result<(), String>),
}

/// 마운트 실패 뒤 다음 시도를 미룬다. 그동안 그 풀의 상태는 마지막 오류다.
struct Backoff {
    until: Instant,
    delay: Duration,
    error: String,
}

/// 풀 하나를 이번 새로 고침에서 어떻게 할지
enum Step {
    /// 상태가 정해졌다(마운트 실패, 마운트 중 등).
    State(PoolState),
    /// 이 디렉터리를 확인한다. 멈추면 이 오류를 적는다.
    Check(PathBuf, &'static str),
}

/// 마운트에 실패한 뒤 다음 시도까지 기다리는 시간: 15초부터 실패할 때마다 두 배, 최대 5분
fn next_backoff(prev: Option<Duration>) -> Duration {
    prev.map_or(MOUNT_BACKOFF_INITIAL, |d| (d * 2).min(MOUNT_BACKOFF_MAX))
}

impl Pools {
    fn new(cluster: Arc<Cluster>, mount_root: PathBuf, ops: Arc<dyn MountOps>) -> Self {
        let NodeId(node) = cluster.id();
        Self {
            cluster,
            node,
            mount_root,
            ops,
            nfs_supported: cfg!(target_os = "linux"),
            mount_timeout: MOUNT_TIMEOUT,
            check_timeout: CHECK_TIMEOUT,
            stuck: HashMap::new(),
            mounting: HashSet::new(),
            unmounting: HashSet::new(),
            finished: Arc::default(),
            done: Arc::default(),
            backoff: HashMap::new(),
            umount_errors: HashMap::new(),
        }
    }

    /// 풀 상태를 새로 고친다. 마운트·언마운트는 기다리지 않고(끝나면 `done`이 알린다), 확인은 풀마다
    /// 동시에 해 한 풀이 다른 풀의 상태 기록을 늦추지 않는다.
    async fn tick(&mut self) -> anyhow::Result<()> {
        let Some(me) = self.cluster.get::<NodeSpec>(self.node)? else {
            // 아직 Node 리소스를 기록하지 않았다.
            return Ok(());
        };
        let (_, pools) = self.cluster.list::<StoragePoolSpec>()?;
        let usable: Vec<&StoragePool> = pools
            .iter()
            .filter(|p| p.spec.nodes.is_empty() || p.spec.nodes.contains(&self.node))
            .collect();
        // mountinfo는 실제 경로를 적으므로 마운트 지점도 실제 경로로 비교한다.
        std::fs::create_dir_all(&self.mount_root)?;
        self.mount_root = std::fs::canonicalize(&self.mount_root)?;
        // 끝난 마운트를 먼저 거둔다. 그 뒤에 읽는 mountinfo에 성공한 마운트가 보인다.
        let mut mount_results = self.collect_finished();
        let mounted = if self.nfs_supported {
            opl_driver::mounts::nfs_mount_points(&self.ops.mountinfo()?)
        } else {
            BTreeSet::new()
        };
        self.backoff
            .retain(|id, _| usable.iter().any(|p| p.id == *id));
        let mut states = BTreeMap::new();
        let mut checks = Vec::new();
        for pool in &usable {
            match self.step(pool, &mounted, mount_results.remove(&pool.id)) {
                Step::State(state) => {
                    states.insert(pool.name.clone(), state);
                }
                Step::Check(dir, not_responding) => checks.push((*pool, dir, not_responding)),
            }
        }
        for (pool, state) in self.check_all(checks).await {
            let state = match state {
                Some(state) => state,
                // 앞 확인이 아직 멈춰 있다. 같은 풀(id)의 지난 상태를 그대로 둔다.
                None => me
                    .status
                    .pools
                    .get(&pool.name)
                    .filter(|s| s.pool_id == pool.id)
                    .cloned()
                    .unwrap_or_else(|| not_ready(pool.id, NFS_NOT_RESPONDING.into())),
            };
            states.insert(pool.name.clone(), state);
        }
        if self.nfs_supported {
            self.unmount_unused(&pools, &usable, &mounted)?;
        }
        for (name, state) in &states {
            if me.status.pools.get(name).map(|s| (s.ready, &s.error))
                != Some((state.ready, &state.error))
            {
                match &state.error {
                    None => tracing::info!(pool = %name, "storage pool is ready"),
                    Some(error) => {
                        tracing::warn!(pool = %name, %error, "storage pool is not ready")
                    }
                }
            }
        }
        self.write(states).await
    }

    /// 풀 하나를 이번에 어떻게 할지 정한다. 마운트되지 않은 NFS 풀은 마운트를 시작한다.
    /// `mount_result`는 이번에 거둔 이 풀의 마운트 결과다.
    fn step(
        &mut self,
        pool: &StoragePool,
        mounted: &BTreeSet<PathBuf>,
        mount_result: Option<Result<(), String>>,
    ) -> Step {
        let (server, export, options) = match &pool.spec.backend {
            PoolBackend::Dir { path } => return Step::Check(path.clone(), DIR_NOT_RESPONDING),
            PoolBackend::LvmThin { .. } => {
                return Step::State(not_ready(pool.id, LVM_UNSUPPORTED.into()));
            }
            PoolBackend::Nfs {
                server,
                export,
                options,
            } => (server, export, options),
        };
        if !self.nfs_supported {
            return Step::State(not_ready(pool.id, NFS_UNSUPPORTED.into()));
        }
        let dir = self.mount_root.join(pool.id.to_string());
        // 언마운트 중이면 mountinfo에 아직 보여도 곧 풀린다. 끝난 뒤에 다시 마운트한다.
        if self.unmounting.contains(&dir) {
            return Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()));
        }
        if mounted.contains(&dir) {
            self.backoff.remove(&pool.id);
            return Step::Check(dir, NFS_NOT_RESPONDING);
        }
        if let Some(result) = mount_result {
            // 마운트가 실패했다(성공했다면 mountinfo에 보였다). 다음 시도를 미룬다.
            let error = result
                .err()
                .unwrap_or_else(|| format!("{} is not mounted after mount", dir.display()));
            let delay = next_backoff(self.backoff.get(&pool.id).map(|b| b.delay));
            tracing::debug!(pool = %pool.name, %error, ?delay, "nfs mount failed; retrying later");
            let backoff = Backoff {
                until: Instant::now() + delay,
                delay,
                error: error.clone(),
            };
            self.backoff.insert(pool.id, backoff);
            return Step::State(not_ready(pool.id, error));
        }
        if self.mounting.contains(&pool.id) {
            return Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()));
        }
        if let Some(backoff) = self.backoff.get(&pool.id)
            && Instant::now() < backoff.until
        {
            return Step::State(not_ready(pool.id, backoff.error.clone()));
        }
        // 마운트되지 않은 지점만 건드린다(멈춘 NFS 마운트 지점은 stat도 멈춘다).
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Step::State(not_ready(
                pool.id,
                format!("cannot create {}: {e}", dir.display()),
            ));
        }
        let args = mount_args(server, export, options.as_deref(), &dir);
        self.mounting.insert(pool.id);
        let (ops, finished, done) = (self.ops.clone(), self.finished.clone(), self.done.clone());
        let (id, name, timeout) = (pool.id, pool.name.clone(), self.mount_timeout);
        tokio::spawn(async move {
            let result = tokio::time::timeout(timeout, ops.mount(&args))
                .await
                .unwrap_or_else(|_| Err(format!("mount timed out after {}s", timeout.as_secs())));
            if result.is_ok() {
                tracing::info!(pool = %name, dir = %dir.display(), "mounted nfs pool");
            }
            finished.lock().unwrap().push(Finished::Mount(id, result));
            done.notify_one();
        });
        Step::State(not_ready(pool.id, MOUNT_IN_PROGRESS.into()))
    }

    /// 끝난 마운트·언마운트를 거둔다. 마운트 결과는 풀 id별로 돌려주고, 언마운트 결과는 기록한다.
    fn collect_finished(&mut self) -> HashMap<Uuid, Result<(), String>> {
        let mut mounts = HashMap::new();
        for finished in std::mem::take(&mut *self.finished.lock().unwrap()) {
            match finished {
                Finished::Mount(pool, result) => {
                    self.mounting.remove(&pool);
                    mounts.insert(pool, result);
                }
                Finished::Unmount(dir, result) => {
                    self.unmounting.remove(&dir);
                    match result {
                        Ok(()) => {
                            tracing::info!(dir = %dir.display(), "unmounted nfs pool no longer used on this node");
                            self.umount_errors.remove(&dir);
                        }
                        Err(e) => {
                            if self.umount_errors.get(&dir) != Some(&e) {
                                tracing::warn!(dir = %dir.display(), error = %e, "cannot unmount nfs pool");
                            }
                            self.umount_errors.insert(dir, e);
                        }
                    }
                }
            }
        }
        mounts
    }

    /// 쓰기 확인과 용량을 풀마다 별도 blocking 작업으로 한꺼번에 시작하고, 모두 같은 시간 제한
    /// 안에서 기다린다. 앞 확인이 아직 멈춰 있어 건너뛴 풀은 `None`.
    async fn check_all<'a>(
        &mut self,
        checks: Vec<(&'a StoragePool, PathBuf, &'static str)>,
    ) -> Vec<(&'a StoragePool, Option<PoolState>)> {
        let deadline = Instant::now() + self.check_timeout;
        let mut out = Vec::new();
        let mut running = Vec::new();
        for (pool, dir, not_responding) in checks {
            if let Some(task) = self.stuck.get(&pool.id) {
                if !task.is_finished() {
                    out.push((pool, None));
                    continue;
                }
                self.stuck.remove(&pool.id);
            }
            let ops = self.ops.clone();
            let probe = format!(".opl-check-{}", self.node);
            let checked = dir.clone();
            let task = tokio::task::spawn_blocking(move || ops.check(&checked, &probe));
            running.push((pool, dir, not_responding, task));
        }
        for (pool, dir, not_responding, mut task) in running {
            let state = match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(Ok(Ok(capacity))) => PoolState {
                    pool_id: pool.id,
                    ready: true,
                    error: None,
                    total_bytes: capacity.total_bytes,
                    available_bytes: capacity.available_bytes,
                },
                Ok(Ok(Err(e))) => not_ready(pool.id, format!("{}: {e}", dir.display())),
                Ok(Err(e)) => not_ready(pool.id, format!("{}: {e}", dir.display())),
                Err(_) => {
                    self.stuck.insert(pool.id, task);
                    not_ready(pool.id, not_responding.into())
                }
            };
            out.push((pool, Some(state)));
        }
        out
    }

    /// 이 노드가 쓸 수 없게 된 NFS 풀(지워졌거나 `nodes`에서 빠짐)의 언마운트를 시작한다. 끝나면
    /// 마운트 지점을 지운다. 그 풀의 디스크를 쓰는 VM이 이 노드에서 돌고 있거나 앞 언마운트가 아직
    /// 끝나지 않았으면 다음 tick으로 미룬다. `umount -f`는 쓰지 않는다(쓰던 데이터를 잃을 수 있다).
    fn unmount_unused(
        &mut self,
        pools: &[StoragePool],
        usable: &[&StoragePool],
        mounted: &BTreeSet<PathBuf>,
    ) -> anyhow::Result<()> {
        let keep: BTreeSet<PathBuf> = usable
            .iter()
            .filter(|p| matches!(p.spec.backend, PoolBackend::Nfs { .. }))
            .map(|p| self.mount_root.join(p.id.to_string()))
            .collect();
        let pool_dir = |path: &Path| {
            path.parent() == Some(self.mount_root.as_path())
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| Uuid::try_parse(n).is_ok())
        };
        let unused: Vec<PathBuf> = mounted
            .iter()
            .filter(|p| pool_dir(p) && !keep.contains(*p) && !self.unmounting.contains(*p))
            .cloned()
            .collect();
        if !unused.is_empty() {
            let busy = self.busy_pool_dirs(pools)?;
            for dir in unused {
                if busy.contains(&dir) {
                    tracing::debug!(dir = %dir.display(), "a running VM uses this nfs pool; not unmounting yet");
                    continue;
                }
                self.unmounting.insert(dir.clone());
                let (ops, finished, done) =
                    (self.ops.clone(), self.finished.clone(), self.done.clone());
                let timeout = self.mount_timeout;
                tokio::spawn(async move {
                    let result = tokio::time::timeout(timeout, ops.umount(&dir))
                        .await
                        .unwrap_or_else(|_| Err("umount timed out".into()));
                    if result.is_ok() {
                        let _ = std::fs::remove_dir(&dir);
                    }
                    finished
                        .lock()
                        .unwrap()
                        .push(Finished::Unmount(dir, result));
                    done.notify_one();
                });
            }
        }
        // 마운트되지 않은 남은 마운트 지점(빈 디렉터리)을 지운다.
        for entry in std::fs::read_dir(&self.mount_root)?.flatten() {
            let path = entry.path();
            if pool_dir(&path) && !keep.contains(&path) && !mounted.contains(&path) {
                let _ = std::fs::remove_dir(&path);
            }
        }
        Ok(())
    }

    /// 이 노드에서 도는 VM의 디스크가 있는 풀의 마운트 지점
    fn busy_pool_dirs(&self, pools: &[StoragePool]) -> anyhow::Result<BTreeSet<PathBuf>> {
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        let (_, volumes) = self.cluster.list::<VolumeSpec>()?;
        let mut dirs = BTreeSet::new();
        let running = vms.iter().filter(|vm| {
            vm.status.node == Some(self.node)
                && matches!(
                    vm.status.phase,
                    VmPhase::Starting | VmPhase::Running | VmPhase::Stopping
                )
        });
        for disk in running.flat_map(|vm| &vm.spec.disks) {
            let pool = volumes
                .iter()
                .find(|v| v.name == disk.volume)
                .and_then(|v| pools.iter().find(|p| p.name == v.spec.pool));
            if let Some(pool) = pool {
                dirs.insert(self.mount_root.join(pool.id.to_string()));
            }
        }
        Ok(dirs)
    }

    /// 바뀌었으면(용량은 1 GiB 넘게, [`keep_capacity`]) `NodeStatus.pools`에 쓴다. 다른 쪽이 그 사이
    /// status를 썼으면 다시 읽고 쓴다.
    async fn write(&self, pools: BTreeMap<String, PoolState>) -> anyhow::Result<()> {
        loop {
            let Some(node) = self.cluster.get::<NodeSpec>(self.node)? else {
                return Ok(());
            };
            let pools = keep_capacity(&node.status.pools, pools.clone());
            if node.status.pools == pools {
                return Ok(());
            }
            let status = NodeStatus {
                pools,
                ..node.status
            };
            match self
                .cluster
                .update_status_checked::<NodeSpec>(self.node, node.revision, &status)
                .await
            {
                Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// 앞서 적은 상태와 용량만 1 GiB 넘게 바뀌지 않았으면 앞 용량을 둔다. 쓰기·지우기로 조금씩 바뀌는
/// 용량 때문에 tick마다 노드 상태를 다시 쓰지 않게 한다.
fn keep_capacity(
    old: &BTreeMap<String, PoolState>,
    mut new: BTreeMap<String, PoolState>,
) -> BTreeMap<String, PoolState> {
    for (name, state) in &mut new {
        if let Some(prev) = old.get(name)
            && prev.pool_id == state.pool_id
            && prev.ready == state.ready
            && prev.error == state.error
            && prev.total_bytes.abs_diff(state.total_bytes) < GIB
            && prev.available_bytes.abs_diff(state.available_bytes) < GIB
        {
            state.total_bytes = prev.total_bytes;
            state.available_bytes = prev.available_bytes;
        }
    }
    new
}

fn not_ready(pool: Uuid, error: String) -> PoolState {
    PoolState {
        pool_id: pool,
        ready: false,
        error: Some(error),
        total_bytes: 0,
        available_bytes: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use opl_types::resource::Resource;
    use opl_types::vm::VmStatus;

    use super::*;
    use crate::bootstrap::tests::single_node;

    #[test]
    fn builds_mount_arguments() {
        let dir = Path::new("/var/lib/opl/pools/p1");
        // 사용자 옵션이 있든 없든 nosuid,nodev,noexec,retry=0을 먼저 붙인다(사용자 옵션이 이긴다).
        assert_eq!(
            mount_args("nas.lan", "/srv/vm", None, dir),
            [
                "-t",
                "nfs",
                "-o",
                "nosuid,nodev,noexec,retry=0",
                "nas.lan:/srv/vm",
                "/var/lib/opl/pools/p1"
            ]
        );
        assert_eq!(
            mount_args("10.0.0.5", "/srv/vm", Some("vers=4.2,hard"), dir),
            [
                "-t",
                "nfs",
                "-o",
                "nosuid,nodev,noexec,retry=0,vers=4.2,hard",
                "10.0.0.5:/srv/vm",
                "/var/lib/opl/pools/p1"
            ]
        );
        assert_eq!(
            mount_args("fd00::5", "/srv/vm", None, dir)[4],
            "[fd00::5]:/srv/vm"
        );
    }

    /// 마운트를 메모리에만 기록하는 가짜
    #[derive(Default)]
    struct FakeMount {
        mounted: Mutex<BTreeSet<PathBuf>>,
        calls: Mutex<Vec<String>>,
        mount_error: Mutex<Option<String>>,
        /// 참이면 확인이 풀릴 때까지 멈춘다(멈춘 NFS 서버).
        hang: AtomicBool,
        /// 참이면 `/srv/slow` 마운트가 풀릴 때까지 멈춘다.
        hang_slow_mount: AtomicBool,
        checks: Mutex<usize>,
        /// 용량에 더하는 바이트(쓰기·지우기로 조금씩 바뀌는 용량)
        extra: AtomicU64,
    }

    impl FakeMount {
        fn calls(&self) -> Vec<String> {
            std::mem::take(&mut self.calls.lock().unwrap())
        }
    }

    #[async_trait]
    impl MountOps for FakeMount {
        fn mountinfo(&self) -> std::io::Result<String> {
            let mounted = self.mounted.lock().unwrap();
            Ok(mounted
                .iter()
                .map(|p| format!("40 22 0:45 / {} rw - nfs4 nas:/srv rw\n", p.display()))
                .collect())
        }

        async fn mount(&self, args: &[OsString]) -> Result<(), String> {
            let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
            self.calls
                .lock()
                .unwrap()
                .push(format!("mount {}", args.join(" ")));
            while self.hang_slow_mount.load(Ordering::SeqCst)
                && args.iter().any(|a| a.ends_with(":/srv/slow"))
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if let Some(e) = self.mount_error.lock().unwrap().clone() {
                return Err(e);
            }
            self.mounted
                .lock()
                .unwrap()
                .insert(PathBuf::from(args.last().unwrap().as_ref()));
            Ok(())
        }

        async fn umount(&self, dir: &Path) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("umount {}", dir.display()));
            self.mounted.lock().unwrap().remove(dir);
            Ok(())
        }

        fn check(&self, dir: &Path, probe: &str) -> std::io::Result<Capacity> {
            *self.checks.lock().unwrap() += 1;
            while self.hang.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(probe.starts_with(".opl-check-"));
            if !dir.is_dir() {
                return Err(std::io::ErrorKind::NotFound.into());
            }
            let extra = self.extra.load(Ordering::SeqCst);
            Ok(Capacity {
                total_bytes: 100 * GIB + extra,
                available_bytes: 40 * GIB + extra,
            })
        }
    }

    struct Setup {
        _dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        node: Uuid,
        root: PathBuf,
        local: PathBuf,
        fake: Arc<FakeMount>,
        pools: Pools,
    }

    async fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(node)) = single_node(&dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let mut me = Resource::new(
            "n1",
            NodeSpec {
                address: "h:8006".into(),
                maintenance: false,
            },
        );
        me.id = node;
        cluster.create(&me).await.unwrap();
        let root = dir.path().canonicalize().unwrap().join("pools");
        let local = dir.path().join("local");
        std::fs::create_dir(&local).unwrap();
        let fake = Arc::new(FakeMount::default());
        let mut pools = Pools::new(cluster.clone(), root.clone(), fake.clone());
        pools.nfs_supported = true;
        pools.check_timeout = Duration::from_millis(200);
        Setup {
            _dir: dir,
            cluster,
            node,
            root,
            local,
            fake,
            pools,
        }
    }

    impl Pools {
        /// 새로 고치고, 그때 시작한 마운트·언마운트가 끝나면 한 번 더 새로 고친다.
        async fn settle(&mut self) {
            self.tick().await.unwrap();
            while self.finished.lock().unwrap().len() < self.mounting.len() + self.unmounting.len()
            {
                tokio::time::timeout(Duration::from_secs(10), self.done.notified())
                    .await
                    .expect("mount or unmount did not finish");
            }
            self.tick().await.unwrap();
        }
    }

    impl Setup {
        async fn pool(&self, name: &str, backend: PoolBackend, nodes: Vec<Uuid>) -> StoragePool {
            let pool = Resource::new(
                name,
                StoragePoolSpec {
                    backend,
                    nodes,
                    shared: false,
                },
            );
            self.cluster.create(&pool).await.unwrap();
            pool
        }

        fn states(&self) -> BTreeMap<String, PoolState> {
            self.cluster
                .get::<NodeSpec>(self.node)
                .unwrap()
                .unwrap()
                .status
                .pools
        }
    }

    fn nfs() -> PoolBackend {
        PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: Some("vers=4.2".into()),
        }
    }

    fn ready(pool: &StoragePool) -> PoolState {
        PoolState {
            pool_id: pool.id,
            ready: true,
            error: None,
            total_bytes: 100 * GIB,
            available_bytes: 40 * GIB,
        }
    }

    #[tokio::test]
    async fn records_dir_and_nfs_pools_usable_on_this_node() {
        let mut s = setup().await;
        let local = s
            .pool(
                "local",
                PoolBackend::Dir {
                    path: s.local.clone(),
                },
                vec![],
            )
            .await;
        let missing = s
            .pool(
                "missing",
                PoolBackend::Dir {
                    path: s.root.join("nope"),
                },
                vec![s.node],
            )
            .await;
        // 다른 노드만 쓰는 풀은 적지 않는다.
        s.pool("elsewhere", nfs(), vec![Uuid::now_v7()]).await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        let mount_dir = s.root.join(nas.id.to_string());

        s.pools.settle().await;
        assert_eq!(
            s.fake.calls(),
            [format!(
                "mount -t nfs -o nosuid,nodev,noexec,retry=0,vers=4.2 nas:/srv/vm {}",
                mount_dir.display()
            )]
        );
        let states = s.states();
        assert_eq!(
            states.keys().collect::<Vec<_>>(),
            ["local", "missing", "nas"]
        );
        // 상태마다 어느 풀의 것인지(id) 적는다.
        assert_eq!(states["local"], ready(&local));
        assert_eq!(states["nas"], ready(&nas));
        assert_eq!(states["missing"].pool_id, missing.id);
        assert!(!states["missing"].ready);
        assert!(states["missing"].error.as_ref().unwrap().contains("nope"));

        // 이미 마운트돼 있으면 다시 마운트하지 않고, 바뀐 것이 없으면 쓰지 않는다.
        let revision = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        let after = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        assert_eq!(after, revision);

        // 용량이 1 GiB 넘게 바뀌지 않았으면 다시 쓰지 않는다.
        s.fake.extra.store(GIB - 1, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        let after = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap().revision;
        assert_eq!(after, revision);
        assert_eq!(s.states()["nas"], ready(&nas));
        s.fake.extra.store(GIB, Ordering::SeqCst);
        s.pools.tick().await.unwrap();
        let state = &s.states()["nas"];
        assert_eq!(state.total_bytes, 101 * GIB);
        assert_eq!(state.available_bytes, 41 * GIB);
        s.cluster.shutdown().await.unwrap();
    }

    /// LVM-thin 풀은 아직 확인하지 않고 준비되지 않았다고 적는다.
    #[tokio::test]
    async fn lvm_thin_pools_are_not_ready_yet() {
        let mut s = setup().await;
        let backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        };
        let vg = s.pool("vg", backend, vec![]).await;
        s.pools.settle().await;
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["vg"],
            not_ready(vg.id, "lvm-thin pools are not supported yet".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn refreshes_as_soon_as_a_pool_is_created() {
        let s = setup().await;
        let dir = |path: &Path| PoolBackend::Dir {
            path: path.to_path_buf(),
        };
        s.pool("first", dir(&s.local), vec![]).await;
        let (stop, rx) = oneshot::channel();
        // 주기는 길게 둔다. 첫 새로 고침 뒤에 만든 풀도 주기를 기다리지 않고 적는다.
        let task = tokio::spawn(run_pools(s.pools, Duration::from_secs(3600), rx));
        let wait_ready = |name: &'static str| {
            let cluster = s.cluster.clone();
            let node = s.node;
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let states = cluster.get::<NodeSpec>(node).unwrap().unwrap().status.pools;
                    if states.get(name).is_some_and(|p| p.ready) {
                        return;
                    }
                    assert!(tokio::time::Instant::now() < deadline, "{name}: {states:?}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        wait_ready("first").await;
        let local = Resource::new(
            "local",
            StoragePoolSpec {
                backend: dir(&s.local),
                nodes: vec![],
                shared: false,
            },
        );
        s.cluster.create(&local).await.unwrap();
        wait_ready("local").await;
        stop.send(()).unwrap();
        task.await.unwrap();
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn mount_failure_is_reported_and_retried() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        *s.fake.mount_error.lock().unwrap() = Some("mount.nfs: access denied by server".into());
        let denied = not_ready(nas.id, "mount.nfs: access denied by server".into());
        s.pools.settle().await;
        assert_eq!(s.states()["nas"], denied);
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.pools.backoff[&nas.id].delay, Duration::from_secs(15));
        // 백오프 동안에는 다시 마운트하지 않고 마지막 오류를 둔다.
        *s.fake.mount_error.lock().unwrap() = None;
        s.pools.settle().await;
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(s.states()["nas"], denied);
        // 시간이 지나 다시 실패하면 두 배로 기다린다.
        *s.fake.mount_error.lock().unwrap() = Some("mount.nfs: access denied by server".into());
        s.pools.backoff.get_mut(&nas.id).unwrap().until = Instant::now();
        s.pools.settle().await;
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.pools.backoff[&nas.id].delay, Duration::from_secs(30));
        // 다음 시도가 성공하면 준비되고 백오프를 잊는다.
        *s.fake.mount_error.lock().unwrap() = None;
        s.pools.backoff.get_mut(&nas.id).unwrap().until = Instant::now();
        s.pools.settle().await;
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.states()["nas"], ready(&nas));
        assert!(s.pools.backoff.is_empty());
        s.cluster.shutdown().await.unwrap();
    }

    #[test]
    fn mount_backoff_doubles_up_to_five_minutes() {
        let mut delays = vec![next_backoff(None)];
        for _ in 0..6 {
            delays.push(next_backoff(delays.last().copied()));
        }
        assert_eq!(
            delays,
            [15, 30, 60, 120, 240, 300, 300].map(Duration::from_secs)
        );
    }

    #[tokio::test]
    async fn a_hung_mount_does_not_hold_up_other_pools() {
        let mut s = setup().await;
        let local = PoolBackend::Dir {
            path: s.local.clone(),
        };
        let local = s.pool("local", local, vec![]).await;
        let slow = PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/slow".into(),
            options: None,
        };
        let slow = s.pool("slow", slow, vec![]).await;
        s.fake.hang_slow_mount.store(true, Ordering::SeqCst);
        let in_progress = not_ready(slow.id, "mount in progress".into());

        // 멈춘 마운트(시간 제한 30초)를 기다리지 않고 다른 풀의 상태를 적는다.
        let started = Instant::now();
        s.pools.tick().await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(s.states()["local"], ready(&local));
        assert_eq!(s.states()["slow"], in_progress);
        // 진행 중인 마운트가 있으면 새로 시작하지 않는다.
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls().len(), 1);
        assert_eq!(s.states()["slow"], in_progress);

        // 그동안 만든 다른 NFS 풀은 따로 마운트돼 준비된다.
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.tick().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), s.pools.done.notified())
            .await
            .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(s.states()["nas"], ready(&nas));
        assert_eq!(s.states()["slow"], in_progress);
        assert_eq!(s.fake.calls().len(), 1);

        s.fake.hang_slow_mount.store(false, Ordering::SeqCst);
        s.pools.settle().await;
        assert_eq!(s.states()["slow"], ready(&slow));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pool_being_unmounted_is_not_ready_while_still_mounted() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.settle().await;
        assert_eq!(s.states()["nas"], ready(&nas));
        // 이 노드에서 쓰지 않게 돼 언마운트를 시작했고, 끝나기 전에 다시 쓰게 됐다. 아직 mountinfo에
        // 보여도 곧 풀린다.
        s.pools.unmounting.insert(s.root.join(nas.id.to_string()));
        s.pools.tick().await.unwrap();
        let state = &s.states()["nas"];
        assert!(!state.ready);
        assert_eq!(state.error.as_deref(), Some(MOUNT_IN_PROGRESS));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn nfs_is_not_mounted_where_unsupported() {
        let mut s = setup().await;
        s.pools.nfs_supported = false;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs pools are not supported on macOS".into())
        );
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn stuck_check_reports_not_responding_and_is_skipped_until_it_ends() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        s.fake.hang.store(true, Ordering::SeqCst);
        s.pools.settle().await;
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        // 앞 확인이 멈춰 있는 동안 새 확인을 시작하지 않는다.
        s.pools.tick().await.unwrap();
        assert_eq!(*s.fake.checks.lock().unwrap(), 1);
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        // 기록된 상태가 같은 이름의 다른 풀(지우고 다시 만든 풀)의 것이면 그대로 두지 않는다.
        let mut me = s.cluster.get::<NodeSpec>(s.node).unwrap().unwrap();
        let stale = PoolState {
            pool_id: Uuid::now_v7(),
            ..ready(&nas)
        };
        me.status.pools.insert("nas".into(), stale);
        s.cluster
            .update_status::<NodeSpec>(s.node, &me.status)
            .await
            .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(
            s.states()["nas"],
            not_ready(nas.id, "nfs server not responding".into())
        );
        s.fake.hang.store(false, Ordering::SeqCst);
        // 멈췄던 확인이 실제로 끝날 때까지 기다린다(느린 머신에서는 50ms로 모자랐다).
        tokio::time::timeout(Duration::from_secs(10), async {
            while !s.pools.stuck[&nas.id].is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        s.pools.tick().await.unwrap();
        assert_eq!(*s.fake.checks.lock().unwrap(), 2);
        assert_eq!(s.states()["nas"], ready(&nas));
        s.cluster.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unmounts_nfs_pools_no_longer_usable_unless_a_running_vm_uses_them() {
        let mut s = setup().await;
        let nas = s.pool("nas", nfs(), vec![]).await;
        let mount_dir = s.root.join(nas.id.to_string());
        s.pools.settle().await;
        s.fake.calls();
        // 이 노드에서 도는 VM이 이 풀의 볼륨을 쓴다.
        let volume = Resource::new(
            "disk1",
            VolumeSpec {
                pool: "nas".into(),
                size_bytes: 1 << 20,
                format: opl_types::runtime::ImageFormat::Qcow2,
            },
        );
        s.cluster.create(&volume).await.unwrap();
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "sockets": 1, "cores": 1 },
            "memory_mib": 512,
            "machine": "q35",
            "disks": [{ "volume": "disk1", "bus": "virtio-blk" }],
        }))
        .unwrap();
        let vm = Resource::new("vm1", spec);
        s.cluster.create(&vm).await.unwrap();
        let running = VmStatus {
            phase: VmPhase::Running,
            node: Some(s.node),
            ..Default::default()
        };
        s.cluster
            .update_status::<VmSpec>(vm.id, &running)
            .await
            .unwrap();
        // 풀에서 이 노드를 뺀다.
        let spec = StoragePoolSpec {
            nodes: vec![Uuid::now_v7()],
            ..nas.spec.clone()
        };
        s.cluster
            .update_spec(nas.id, None, Default::default(), &spec)
            .await
            .unwrap();

        s.pools.tick().await.unwrap();
        assert_eq!(s.fake.calls(), Vec::<String>::new());
        assert!(s.states().is_empty());
        assert!(mount_dir.is_dir());

        // VM이 멈추면 언마운트하고 마운트 지점을 지운다.
        s.cluster
            .update_status::<VmSpec>(vm.id, &VmStatus::default())
            .await
            .unwrap();
        s.pools.settle().await;
        assert_eq!(s.fake.calls(), [format!("umount {}", mount_dir.display())]);
        assert!(!mount_dir.exists());
        s.cluster.shutdown().await.unwrap();
    }
}
