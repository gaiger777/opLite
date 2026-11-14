//! HA 상태 검사의 노드 쪽 검사기(설계 §8.2). [`CHECK_INTERVAL`]마다 켜진 `HaCheck`와 클러스터 링크의 자동 검사
//! (`cluster-link-<n>`)를 이 노드의 대상으로 동시에 돌려 결과를 리스 갱신에 싣고(`Cluster::set_lease_checks`),
//! 저장된 것과 다르면 `NodeStatus.checks`에 쓴다(화면용). 인터페이스 목록(`NodeStatus.interfaces`)도 여기서
//! 쓰고, 본딩 상태가 바뀌면 감사 항목과 기록 Task(`bond-degraded`)로 알린다. 위트니스는 띄우지 않는다.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use opl_api::AuditLog;
use opl_api::audit::system_entry;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_driver::{NetFs, PING_TIMEOUT, PROBE_TIMEOUT, ProbeError};
use opl_types::audit::AuditTarget;
use opl_types::ha_check::{AUTO_PREFIX, CheckReport, CheckState, HaCheckSpec, HaCheckTarget};
use opl_types::node::{BondState, BondStatus, HA_CHECKS, NodeInterface, NodeSpec, PortState};
use opl_types::resource::Resource;
use opl_types::storage::{IscsiTarget, PoolBackend, StoragePool, StoragePoolSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::bootstrap;

/// 검사 회차의 간격
pub const CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// `ip -j -d addr show`로 인터페이스 목록을 다시 읽는 간격(그 사이 회차는 sysfs만 다시 읽는다)
pub const INTERFACES_EVERY: Duration = Duration::from_secs(30);
/// 공유 LVM 풀의 PV 장치(`pvs`)를 다시 읽는 간격
const PVS_EVERY: Duration = Duration::from_secs(60);
/// 클러스터 링크의 호스트 이름을 푸는 시간
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
/// `ip`를 기다리는 시간
const IP_TIMEOUT: Duration = Duration::from_secs(5);
/// 본딩의 새 상태가 이만큼의 회차(15초) 이어져야 알린다(흔들리는 포트가 알림을 쏟아내지 않게).
const BOND_STEADY_ROUNDS: u32 = 3;
/// 본딩마다 저하·끊김 알림(감사 항목·기록 Task)의 최소 간격
const BOND_ALERT_EVERY: Duration = Duration::from_secs(600);

/// 검사기 설정
#[derive(Debug, Clone)]
pub struct CheckerConfig {
    /// 시스템 `ping`(`[ha] ping`)
    pub ping: PathBuf,
    /// sysfs·procfs의 뿌리(`ha-it`은 `[test] health_root`)
    pub net: NetFs,
    /// NFS 풀의 마운트 지점이 있는 디렉터리(`{data_dir}/pools`)
    pub mount_root: PathBuf,
    /// 클러스터 링크의 호스트(`Config::cluster_hosts`, 앞이 `cluster-link-1`)
    pub hosts: Vec<String>,
}

/// 디렉터리 풀 probe. 시험은 멈추는 가짜로 바꾼다.
pub trait DirProbe: Send + Sync + 'static {
    /// 블로킹이고 멈춘 NFS에서는 끝나지 않을 수 있다.
    fn probe(&self, dir: &Path, file: &str) -> std::io::Result<()>;
}

/// 실제 파일시스템([`opl_driver::probe_dir`])
pub struct SystemProbe;

impl DirProbe for SystemProbe {
    fn probe(&self, dir: &Path, file: &str) -> std::io::Result<()> {
        opl_driver::probe_dir(dir, file)
    }
}

/// 검사 반복 작업
pub struct HealthChecker {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl HealthChecker {
    pub fn spawn(
        cluster: Arc<Cluster>,
        node_name: String,
        config: CheckerConfig,
        audit: Arc<AuditLog>,
    ) -> Self {
        let state = State::new(cluster, node_name, config, audit);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, rx)),
        }
    }

    /// 멈춘다. 멈춘 probe 작업은 기다리지 않는다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(mut state: State, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = run_once(&mut state) => {}
        }
    }
}

/// 클러스터 링크의 호스트. `ips`가 None이면 아직 풀지 못한 것이다(다음 회차에 다시 푼다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    pub ips: Option<Vec<IpAddr>>,
}

/// 검사 회차 사이에 남기는 것
pub struct State {
    cluster: Arc<Cluster>,
    node: Uuid,
    name: String,
    config: CheckerConfig,
    audit: Arc<AuditLog>,
    probe: Arc<dyn DirProbe>,
    /// 부르는 명령(시험은 가짜 스크립트로 바꾼다)
    ip: PathBuf,
    pvs: PathBuf,
    rados: PathBuf,
    /// NFS·공유 LVM·RBD 풀 검사를 하는지(Linux)
    linux: bool,
    probe_timeout: Duration,
    hosts: Vec<Host>,
    /// 마지막으로 읽은 인터페이스 목록(모두, 노드 상태에는 64개까지). 한 번도 읽지 못했으면 None이다(노드
    /// 상태의 목록을 지우지 않는다).
    interfaces: Option<Vec<NodeInterface>>,
    /// 이 노드에서 본 인터페이스 이름(저장된 노드 상태의 목록에서 시작해 다시 시작해도 남는다). 본 적 없는
    /// 인터페이스의 `link` 검사와 핑의 `via`는 `unknown`(노드마다 이름이 다를 수 있다), 본 뒤에 사라진 것은
    /// `failing`이다.
    seen: HashSet<String>,
    /// 자동 검사 이름 → 마지막으로 짝지은 인터페이스(주소를 잃은 인터페이스도 계속 검사한다)
    auto_ifaces: HashMap<String, String>,
    /// 마지막으로 `ip`를 부른 때
    interfaces_at: Option<Instant>,
    /// 마지막 `ip` 실패(같은 실패를 회차마다 적지 않는다)
    ip_error: Option<String>,
    /// 검사 이름 → (실패·degraded 상태, 노드 시계로 시작된 유닉스 초)
    since: HashMap<String, (CheckState, u64)>,
    /// 시간 안에 끝나지 않은 디렉터리 probe(디렉터리별). 끝날 때까지 새로 띄우지 않는다.
    stuck: HashMap<PathBuf, JoinHandle<std::io::Result<()>>>,
    /// VG → (읽은 때, PV 장치 또는 오류). 다시 읽기가 실패하면 지난 장치를 둔다.
    pvs_cache: HashMap<String, (Instant, Result<Vec<String>, String>)>,
    /// 본딩 상태 알림
    bonds: BondWatch,
}

impl State {
    pub fn new(
        cluster: Arc<Cluster>,
        node_name: String,
        config: CheckerConfig,
        audit: Arc<AuditLog>,
    ) -> Self {
        let NodeId(node) = cluster.id();
        // 다시 시작한 뒤에도 사라진 인터페이스를 알아보게, 저장된 이 노드의 인터페이스를 본 것으로 둔다.
        let seen = match cluster.get::<NodeSpec>(node) {
            Ok(Some(me)) => me.status.interfaces.into_iter().map(|i| i.name).collect(),
            _ => HashSet::new(),
        };
        let hosts = config
            .hosts
            .iter()
            .map(|h| Host {
                name: h.clone(),
                ips: h.parse::<IpAddr>().ok().map(|ip| vec![ip]),
            })
            .collect();
        Self {
            cluster,
            node,
            name: node_name,
            config,
            audit,
            probe: Arc::new(SystemProbe),
            ip: "ip".into(),
            pvs: "pvs".into(),
            rados: "rados".into(),
            linux: cfg!(target_os = "linux"),
            probe_timeout: PROBE_TIMEOUT,
            hosts,
            interfaces: None,
            seen,
            auto_ifaces: HashMap::new(),
            interfaces_at: None,
            ip_error: None,
            since: HashMap::new(),
            stuck: HashMap::new(),
            pvs_cache: HashMap::new(),
            bonds: BondWatch::default(),
        }
    }

    /// 아직 풀지 못한 링크 호스트 이름을 푼다(하나에 2초까지).
    async fn resolve_hosts(&mut self) {
        for host in self.hosts.iter_mut().filter(|h| h.ips.is_none()) {
            let found =
                tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((&*host.name, 0)))
                    .await;
            match found {
                Ok(Ok(addrs)) => host.ips = Some(addrs.map(|a| a.ip()).collect()),
                Ok(Err(e)) => {
                    tracing::debug!(host = %host.name, error = %e, "cannot resolve a cluster link host")
                }
                Err(_) => {
                    tracing::debug!(host = %host.name, "resolving a cluster link host timed out")
                }
            }
        }
    }

    /// [`INTERFACES_EVERY`]마다 `ip`로 목록을 다시 읽고, 그 사이에는 sysfs로 상태만 새로 고친다. `ip`가 없거나
    /// 실패하면 지난 목록을 둔다.
    async fn refresh_interfaces(&mut self) {
        let due = self
            .interfaces_at
            .is_none_or(|at| at.elapsed() >= INTERFACES_EVERY);
        if due {
            self.interfaces_at = Some(Instant::now());
            let read = ip_addr_json(&self.ip)
                .await
                .and_then(|json| opl_driver::interfaces(&self.config.net, &json));
            match read {
                Ok(list) => {
                    self.seen.extend(list.iter().map(|i| i.name.clone()));
                    self.interfaces = Some(list);
                    self.ip_error = None;
                    return;
                }
                Err(e) => {
                    if self.ip_error.as_ref() != Some(&e) {
                        tracing::info!(error = %e, "cannot list the network interfaces");
                    }
                    self.ip_error = Some(e);
                }
            }
        }
        if let Some(list) = &mut self.interfaces {
            opl_driver::refresh_interfaces(&self.config.net, list);
        }
    }

    /// 인터페이스 `iface`가 이 노드에 없으면 그 결과다: 본 적이 없으면 `unknown`, 본 뒤 사라졌으면 `failing`.
    /// 있으면 본 것으로 적고 None이다.
    fn missing_iface(&mut self, iface: &str) -> Option<Pending> {
        if self.config.net.sys.join("class/net").join(iface).exists() {
            self.seen.insert(iface.to_string());
            return None;
        }
        Some(if self.seen.contains(iface) {
            Pending::Done(
                CheckState::Failing,
                Some(format!("interface {iface} disappeared from this node")),
            )
        } else {
            Pending::Done(
                CheckState::Unknown,
                Some(format!("interface {iface} not found on this node")),
            )
        })
    }

    /// 자동 검사가 짝지은 인터페이스를 기억한다. 짝을 찾지 못하면(그 인터페이스가 주소를 잃음) 마지막 짝을 계속
    /// 검사한다.
    fn remember_auto(
        &mut self,
        autos: Vec<(String, Result<HaCheckTarget, String>)>,
    ) -> Vec<(String, Result<HaCheckTarget, String>)> {
        autos
            .into_iter()
            .map(|(name, target)| match target {
                Ok(HaCheckTarget::Link { iface }) => {
                    self.auto_ifaces.insert(name.clone(), iface.clone());
                    (name, Ok(HaCheckTarget::Link { iface }))
                }
                Err(e) => match self.auto_ifaces.get(&name) {
                    Some(iface) => (
                        name,
                        Ok(HaCheckTarget::Link {
                            iface: iface.clone(),
                        }),
                    ),
                    None => (name, Err(e)),
                },
                other => (name, other),
            })
            .collect()
    }

    /// 지금 필요한 VG의 PV 장치를 [`PVS_EVERY`]마다(처음엔 곧바로) `pvs`로 다시 읽는다.
    async fn refresh_pvs(&mut self, vgs: Vec<String>) {
        let due: Vec<String> = vgs
            .into_iter()
            .filter(|vg| {
                self.pvs_cache
                    .get(vg)
                    .is_none_or(|(at, _)| at.elapsed() >= PVS_EVERY)
            })
            .collect();
        let bin = self.pvs.clone();
        let results = join_all(due.iter().map(|vg| opl_driver::pvs(&bin, vg))).await;
        let now = Instant::now();
        for (vg, result) in due.into_iter().zip(results) {
            let entry = match (result, self.pvs_cache.remove(&vg)) {
                (Ok(devices), _) => Ok(devices),
                // 지난 장치를 두고 다음 간격에 다시 읽는다.
                (Err(e), Some((_, Ok(devices)))) => {
                    tracing::debug!(vg = %vg, error = %e, "cannot list the physical volumes; keeping the last list");
                    Ok(devices)
                }
                (Err(e), _) => Err(e),
            };
            self.pvs_cache.insert(vg, (now, entry));
        }
    }

    /// 디렉터리 probe를 띄운다. 앞 probe가 아직 멈춰 있으면 띄우지 않고 실패다.
    fn dir_pending(&mut self, dir: PathBuf) -> Pending {
        if let Some(task) = self.stuck.get(&dir) {
            if !task.is_finished() {
                return Pending::Done(CheckState::Failing, Some(not_responding(&dir)));
            }
            self.stuck.remove(&dir);
        }
        let probe = self.probe.clone();
        let file = format!(".orbivisor-health-{}", self.node);
        let probed = dir.clone();
        let task = tokio::task::spawn_blocking(move || probe.probe(&probed, &file));
        Pending::Dir { dir, task }
    }

    /// `pool { name }` 검사를 어떻게 할지 정한다.
    fn pool_pending(&mut self, name: &str, pools: &[StoragePool]) -> Pending {
        let unknown = |e: String| Pending::Done(CheckState::Unknown, Some(e));
        let Some(pool) = pools.iter().find(|p| p.name == name) else {
            return unknown(format!("no pool {name}"));
        };
        if !pool.spec.nodes.is_empty() && !pool.spec.nodes.contains(&self.node) {
            return unknown(format!("this node does not use pool {name}"));
        }
        let unsupported =
            |kind: &str| unknown(format!("{kind} pool checks are not supported on this OS"));
        match &pool.spec.backend {
            PoolBackend::Dir { path } => self.dir_pending(path.clone()),
            PoolBackend::Nfs { .. } if !self.linux => unsupported("nfs"),
            PoolBackend::Nfs { .. } => {
                // mountinfo는 실제 경로를 적는다.
                let root = std::fs::canonicalize(&self.config.mount_root)
                    .unwrap_or_else(|_| self.config.mount_root.clone());
                let dir = root.join(pool.id.to_string());
                match std::fs::read_to_string(self.config.net.proc.join("self/mountinfo")) {
                    Ok(text) if opl_driver::mounts::nfs_mount_points(&text).contains(&dir) => {
                        self.dir_pending(dir)
                    }
                    Ok(_) => {
                        Pending::Done(CheckState::Failing, Some("nfs pool is not mounted".into()))
                    }
                    Err(e) => Pending::Done(
                        CheckState::Failing,
                        Some(format!("cannot read mountinfo: {e}")),
                    ),
                }
            }
            PoolBackend::LvmShared { .. } if !self.linux => unsupported("lvm-shared"),
            PoolBackend::Rbd { .. } if !self.linux => unsupported("rbd"),
            PoolBackend::LvmShared { vg, iscsi } => {
                let (state, error) = self.lvm_shared(vg, iscsi.as_ref());
                Pending::Done(state, error)
            }
            PoolBackend::Rbd {
                pool,
                namespace,
                user,
                conf,
            } => Pending::Rados(opl_driver::rados_args(
                pool,
                namespace.as_deref(),
                user.as_deref(),
                conf.as_deref(),
            )),
            _ => unknown("pool checks need a shared pool".into()),
        }
    }

    /// 공유 LVM 풀: iSCSI 세션(있으면)과 VG의 PV 아래 경로(multipath)를 sysfs로 본다.
    fn lvm_shared(&self, vg: &str, iscsi: Option<&IscsiTarget>) -> (CheckState, Option<String>) {
        let net = &self.config.net;
        if let Some(t) = iscsi
            && !opl_driver::iscsi_logged_in(net, &t.target)
        {
            return (
                CheckState::Failing,
                Some(format!("no logged-in iscsi session to {}", t.target)),
            );
        }
        let devices = match self.pvs_cache.get(vg).map(|(_, r)| r) {
            Some(Ok(devices)) => devices,
            Some(Err(e)) => return (CheckState::Unknown, Some(e.clone())),
            None => {
                return (
                    CheckState::Unknown,
                    Some("physical volumes are not known yet".into()),
                );
            }
        };
        if devices.is_empty() {
            return (
                CheckState::Failing,
                Some(format!("volume group {vg} has no physical volumes")),
            );
        }
        let (total, running) = devices.iter().fold((0, 0), |(t, r), dev| {
            let p = opl_driver::pv_paths(net, dev);
            (t + p.total, r + p.running)
        });
        let detail = format!("{running} of {total} paths running");
        if running == 0 {
            (CheckState::Failing, Some(detail))
        } else if running < total {
            (CheckState::Degraded, Some(detail))
        } else {
            (CheckState::Ok, None)
        }
    }

    /// 실패·degraded가 시작된 때. 같은 상태가 이어지면 처음 시각을 둔다.
    fn since(&mut self, name: &str, state: CheckState, now: u64) -> Option<u64> {
        match state {
            CheckState::Failing | CheckState::Degraded => {
                let entry = self.since.entry(name.to_string()).or_insert((state, now));
                if entry.0 != state {
                    *entry = (state, now);
                }
                Some(entry.1)
            }
            CheckState::Ok | CheckState::Unknown => {
                self.since.remove(name);
                None
            }
        }
    }

    /// 본딩 상태가 바뀌어 이어지면([`BondWatch`]) 감사 항목을 적고, `degraded`·`down`이 되었으면 기록 Task도
    /// 쓴다. 알림 간격 안의 저하·끊김은 로그만 남긴다.
    async fn notify_bonds(&mut self) {
        let Some(list) = &self.interfaces else {
            return;
        };
        let now: BTreeMap<String, BondStatus> = list
            .iter()
            .filter_map(|i| Some((i.name.clone(), i.bond.clone()?)))
            .collect();
        let changes = self.bonds.observe(Instant::now(), &now);
        for change in changes {
            let action = match change.state {
                BondState::Up => {
                    tracing::info!(bond = %change.bond, "all bond ports are up again");
                    "node.bond-recovered"
                }
                BondState::Degraded => {
                    tracing::warn!(detail = %change.detail, "a bond is degraded");
                    "node.bond-degraded"
                }
                BondState::Down => {
                    tracing::warn!(detail = %change.detail, "a bond is down");
                    "node.bond-down"
                }
                // 이 빌드는 만들지 않는다.
                BondState::Unknown => continue,
            };
            if !change.alert {
                tracing::info!(bond = %change.bond, "not reporting the bond change again within {BOND_ALERT_EVERY:?}");
                continue;
            }
            let target = AuditTarget {
                kind: "nodes".into(),
                key: self.name.clone(),
            };
            self.audit
                .record(system_entry(action, target, change.detail.clone()));
            if change.state != BondState::Up {
                self.record_bond(&change).await;
            }
        }
    }

    /// 기록 Task `bond-degraded`(모든 노드가 `ha-checks`를 알 때만 — 옛 노드는 모르는 Task 종류를 읽지 못한다)
    async fn record_bond(&self, change: &BondChange) {
        match opl_api::cluster::missing_feature(&self.cluster, HA_CHECKS) {
            Ok(old) if old.is_empty() => {}
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "cannot record the bond task");
                return;
            }
        }
        let mut record: Task = Resource::new(
            "",
            TaskSpec {
                op: TaskOp::BondDegraded {
                    bond: change.bond.clone(),
                    ports: change.ports.clone(),
                },
                target: self.node,
                node: self.node,
            },
        );
        record.name = record.id.to_string();
        // 실행기는 끝난 Task를 실행하지 않는다.
        record.status = TaskStatus {
            state: TaskState::Succeeded,
            error: None,
            progress: None,
        };
        if let Err(e) = self.cluster.create(&record).await {
            tracing::warn!(error = %e, "cannot record the bond task");
        }
    }

    /// `NodeStatus.checks`·`interfaces`를 저장된 값과 다를 때만 쓴다(인터페이스는 읽은 적이 있을 때만, 64개까지).
    async fn write_status(&self, checks: &[CheckReport]) {
        let interfaces = self
            .interfaces
            .as_deref()
            .map(opl_driver::reported_interfaces);
        let interfaces = interfaces.as_ref();
        let written = bootstrap::update_node_status(&self.cluster, |status| {
            let mut changed = false;
            if status.checks != checks {
                status.checks = checks.to_vec();
                changed = true;
            }
            if let Some(list) = interfaces
                && status.interfaces != *list
            {
                status.interfaces = list.clone();
                changed = true;
            }
            changed
        })
        .await;
        if let Err(e) = written {
            tracing::warn!(
                error = format!("{e:#}"),
                "cannot record the HA check results"
            );
        }
    }
}

/// 검사 하나를 이번 회차에 어떻게 할지
enum Pending {
    Done(CheckState, Option<String>),
    Ping {
        addresses: Vec<IpAddr>,
        via: Option<String>,
        need: u8,
    },
    Dir {
        dir: PathBuf,
        task: JoinHandle<std::io::Result<()>>,
    },
    Rados(Vec<String>),
}

fn not_responding(dir: &Path) -> String {
    format!("{} is not responding", dir.display())
}

/// 한 회차: 이 노드의 모든 검사를 동시에 돌려 리스 갱신에 싣고, 바뀌었으면 노드 상태에 쓴다. 결과는 이름 순이다.
pub async fn run_once(state: &mut State) -> Vec<CheckReport> {
    state.resolve_hosts().await;
    state.refresh_interfaces().await;
    let checks = match state.cluster.list::<HaCheckSpec>() {
        Ok((_, checks)) => checks,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the HA checks");
            return state.cluster.lease_checks();
        }
    };
    let mut targets: Vec<(String, Result<HaCheckTarget, String>)> = checks
        .iter()
        .filter(|c| c.spec.enabled)
        .map(|c| (c.name.clone(), Ok(c.spec.target_for(&state.name).clone())))
        .collect();
    let interfaces = state.interfaces.as_deref().unwrap_or_default();
    let autos = auto_checks(&state.hosts, interfaces);
    targets.extend(state.remember_auto(autos));
    targets.sort_by(|a, b| a.0.cmp(&b.0));

    let has_pool = targets
        .iter()
        .any(|(_, t)| matches!(t, Ok(HaCheckTarget::Pool { .. })));
    let pools = if has_pool {
        match state.cluster.list::<StoragePoolSpec>() {
            Ok((_, pools)) => pools,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the storage pools");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if state.linux {
        let vgs: Vec<String> = targets
            .iter()
            .filter_map(|(_, t)| match t {
                Ok(HaCheckTarget::Pool { name }) => pools.iter().find(|p| p.name == *name),
                _ => None,
            })
            .filter_map(|p| match &p.spec.backend {
                PoolBackend::LvmShared { vg, .. } => Some(vg.clone()),
                _ => None,
            })
            .collect();
        state.refresh_pvs(vgs).await;
    }

    let mut pending = Vec::with_capacity(targets.len());
    for (_, target) in &targets {
        let p = match target {
            Err(e) => Pending::Done(CheckState::Unknown, Some(e.clone())),
            Ok(HaCheckTarget::Link { iface }) => state.missing_iface(iface).unwrap_or_else(|| {
                let probe = opl_driver::link_state(&state.config.net, iface);
                Pending::Done(probe.state, probe.error)
            }),
            // 보낼 인터페이스(`via`)가 없으면 링크 검사와 같게 본다.
            Ok(HaCheckTarget::Ping {
                addresses,
                via,
                need,
            }) => via
                .as_deref()
                .and_then(|v| state.missing_iface(v))
                .unwrap_or_else(|| Pending::Ping {
                    addresses: addresses.clone(),
                    via: via.clone(),
                    need: *need,
                }),
            Ok(HaCheckTarget::Pool { name }) => state.pool_pending(name, &pools),
        };
        pending.push(p);
    }

    let deadline = Instant::now() + state.probe_timeout;
    let (ping, rados) = (state.config.ping.clone(), state.rados.clone());
    let results = join_all(
        pending
            .into_iter()
            .map(|p| evaluate(p, &ping, &rados, deadline)),
    )
    .await;

    let now = unix_now();
    let mut reports = Vec::with_capacity(results.len());
    for ((name, _), (check_state, error, stuck)) in targets.into_iter().zip(results) {
        if let Some((dir, task)) = stuck {
            state.stuck.insert(dir, task);
        }
        let since = state.since(&name, check_state, now);
        reports.push(CheckReport::new(name, check_state, error, since));
    }
    state
        .since
        .retain(|name, _| reports.iter().any(|r| r.name == *name));
    state.cluster.set_lease_checks(reports.clone());
    state.notify_bonds().await;
    state.write_status(&reports).await;
    reports
}

/// 정해진 검사 하나를 끝낸다. 시간을 넘긴 디렉터리 probe는 그 작업을 돌려준다(`stuck`에 둔다).
async fn evaluate(
    pending: Pending,
    ping: &Path,
    rados: &Path,
    deadline: Instant,
) -> (
    CheckState,
    Option<String>,
    Option<(PathBuf, JoinHandle<std::io::Result<()>>)>,
) {
    match pending {
        Pending::Done(state, error) => (state, error, None),
        Pending::Ping {
            addresses,
            via,
            need,
        } => {
            let results = join_all(
                addresses
                    .iter()
                    .map(|a| opl_driver::ping(ping, *a, via.as_deref(), PING_TIMEOUT)),
            )
            .await;
            let answered = results.iter().filter(|r| r.is_ok()).count();
            if answered >= usize::from(need) {
                return (CheckState::Ok, None, None);
            }
            // 핑을 돌릴 수 없으면(없음, 권한) 대상의 실패가 아니다.
            if let Some(e) = results.iter().find_map(|r| match r {
                Err(e @ ProbeError::Unavailable(_)) => Some(e),
                _ => None,
            }) {
                return (e.state(), Some(e.to_string()), None);
            }
            let first = results
                .iter()
                .find_map(|r| r.as_ref().err())
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            let error = format!(
                "{answered} of {} addresses answered{first}",
                addresses.len()
            );
            (CheckState::Failing, Some(error), None)
        }
        Pending::Dir { dir, mut task } => {
            match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(Ok(Ok(()))) => (CheckState::Ok, None, None),
                Ok(Ok(Err(e))) => (
                    CheckState::Failing,
                    Some(format!("{}: {e}", dir.display())),
                    None,
                ),
                Ok(Err(e)) => (CheckState::Failing, Some(e.to_string()), None),
                Err(_) => (
                    CheckState::Failing,
                    Some(not_responding(&dir)),
                    Some((dir, task)),
                ),
            }
        }
        Pending::Rados(args) => match opl_driver::rados(rados, &args).await {
            Ok(()) => (CheckState::Ok, None, None),
            Err(e) => (e.state(), Some(e.to_string()), None),
        },
    }
}

/// 클러스터 링크마다의 자동 관리 검사(`cluster-link-1`부터): 그 호스트의 IP를 가진 인터페이스의 `link`. 그런
/// 인터페이스가 없거나 이름을 풀지 못했으면 이유(`unknown`)다.
pub fn auto_checks(
    hosts: &[Host],
    interfaces: &[NodeInterface],
) -> Vec<(String, Result<HaCheckTarget, String>)> {
    hosts
        .iter()
        .enumerate()
        .map(|(i, host)| {
            let name = format!("{AUTO_PREFIX}{}", i + 1);
            let Some(ips) = &host.ips else {
                return (name, Err(format!("cannot resolve {}", host.name)));
            };
            let found = interfaces.iter().find(|iface| {
                iface.addresses.iter().any(|cidr| {
                    let addr = cidr.split_once('/').map_or(cidr.as_str(), |(a, _)| a);
                    addr.parse::<IpAddr>().is_ok_and(|a| ips.contains(&a))
                })
            });
            let target = match found {
                Some(iface) => Ok(HaCheckTarget::Link {
                    iface: iface.name.clone(),
                }),
                None => Err(format!("no interface has address {}", host.name)),
            };
            (name, target)
        })
        .collect()
}

/// 본딩 하나의 상태 바뀜
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BondChange {
    pub bond: String,
    /// 새 상태
    pub state: BondState,
    /// 감사 항목의 설명(`bond0: eno2 down, eno3 lacp_mismatch`)
    pub detail: String,
    /// `up`이 아닌 포트(`eno2: down`)
    pub ports: Vec<String>,
    /// 감사 항목(저하·끊김이면 기록 Task도)을 남긴다. 알림 간격 안의 저하·끊김(나빠진 `degraded → down`은
    /// 빼고)과 알리지 않은 저하·끊김의 회복은 false다(로그만).
    pub alert: bool,
}

/// 본딩 상태 알림. 새 상태가 [`BOND_STEADY_ROUNDS`]회차 이어져야 바뀐 것으로 보고, 저하·끊김 알림은 본딩마다
/// [`BOND_ALERT_EVERY`]에 한 번까지다. 다만 `degraded`에서 `down`으로 나빠지면 간격과 상관없이 알린다. 회복은
/// 저하·끊김을 알렸을 때만 알린다(알리지 않은 저하의 회복만 알리지 않게). 처음 보는 본딩(노드 시작 포함)은 `up`에서
/// 시작한다.
#[derive(Debug, Default)]
pub struct BondWatch {
    bonds: BTreeMap<String, BondTrack>,
}

#[derive(Debug)]
struct BondTrack {
    /// 마지막으로 바뀐 것으로 본 상태
    state: BondState,
    /// 다른 상태와 그 상태가 이어진 회차 수
    pending: Option<(BondState, u32)>,
    /// 마지막 저하·끊김 알림
    alerted: Option<Instant>,
    /// 지금의 저하·끊김을 알렸다(회복은 그때만 알린다).
    reported: bool,
}

impl BondWatch {
    /// 이번 회차의 본딩 상태(`now`, 시각 `at`)를 보고 알릴 바뀜을 돌려준다. 사라진 본딩은 잊는다.
    pub fn observe(&mut self, at: Instant, now: &BTreeMap<String, BondStatus>) -> Vec<BondChange> {
        self.bonds.retain(|name, _| now.contains_key(name));
        let mut changes = vec![];
        for (name, status) in now {
            let track = self.bonds.entry(name.clone()).or_insert(BondTrack {
                state: BondState::Up,
                pending: None,
                alerted: None,
                reported: false,
            });
            if status.state == track.state {
                track.pending = None;
                continue;
            }
            let rounds = match track.pending {
                Some((state, n)) if state == status.state => n + 1,
                _ => 1,
            };
            if rounds < BOND_STEADY_ROUNDS {
                track.pending = Some((status.state, rounds));
                continue;
            }
            let worse = track.state == BondState::Degraded && status.state == BondState::Down;
            track.state = status.state;
            track.pending = None;
            let alert = match status.state {
                BondState::Up => std::mem::take(&mut track.reported),
                BondState::Unknown => false,
                BondState::Degraded | BondState::Down => {
                    worse
                        || track
                            .alerted
                            .is_none_or(|t| at.saturating_duration_since(t) >= BOND_ALERT_EVERY)
                }
            };
            if alert && status.state != BondState::Up {
                track.alerted = Some(at);
                track.reported = true;
            }
            changes.push(BondChange {
                bond: name.clone(),
                state: status.state,
                detail: if status.state == BondState::Up {
                    format!("{name}: all ports up")
                } else {
                    opl_driver::bond_detail(name, status)
                },
                ports: status
                    .ports
                    .iter()
                    .filter(|p| p.state != PortState::Up)
                    .map(|p| {
                        format!(
                            "{}: {}",
                            p.name,
                            opl_driver::hostcheck::port_state_name(p.state)
                        )
                    })
                    .collect(),
                alert,
            });
        }
        changes
    }
}

/// `ip -j -d addr show`의 출력
async fn ip_addr_json(bin: &Path) -> Result<String, String> {
    let run = tokio::process::Command::new(bin)
        .args(["-j", "-d", "addr", "show"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(IP_TIMEOUT, run).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("cannot run ip: {e}")),
        Err(_) => return Err(format!("ip timed out after {IP_TIMEOUT:?}")),
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stderr.lines().map(str::trim).find(|l| !l.is_empty());
        return Err(line.map_or_else(|| format!("ip failed: {}", out.status), str::to_string));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("unexpected ip output: {e}"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use opl_api::AuditConfig;
    use opl_types::audit::AuditQuery;
    use opl_types::ha_check::{HaCheckRole, HaCheckSpec};
    use opl_types::node::{InterfaceKind, NodeSpec};

    use super::*;
    use crate::bootstrap::tests::single_node;

    struct Setup {
        dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        node: Uuid,
        net: NetFs,
        audit: Arc<AuditLog>,
    }

    /// 한 노드 클러스터와 이름 `n1`인 Node(`ha-checks`를 앎), 빈 가짜 sysfs·procfs, 감사 로그
    async fn setup() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, NodeId(node)) = single_node(&dir.path().join("cluster")).await;
        let cluster = Arc::new(cluster);
        let mut me: opl_types::node::Node = Resource::new(
            "n1",
            NodeSpec {
                address: "h:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        me.id = node;
        me.status.features = vec![HA_CHECKS.into()];
        cluster.create(&me).await.unwrap();
        let net = NetFs::under(dir.path().join("root"));
        std::fs::create_dir_all(&net.sys).unwrap();
        std::fs::create_dir_all(&net.proc).unwrap();
        let audit = Arc::new(
            AuditLog::open(AuditConfig {
                dir: dir.path().join("audit"),
                node: "n1".into(),
                max_file_size: 1 << 20,
                max_files: 2,
            })
            .unwrap(),
        );
        Setup {
            dir,
            cluster,
            node,
            net,
            audit,
        }
    }

    impl Setup {
        /// 링크 호스트가 `hosts`인 검사기 상태. `ip`는 없는 명령이다(인터페이스를 모름).
        fn state(&self, hosts: &[&str]) -> State {
            let mut state = State::new(
                self.cluster.clone(),
                "n1".into(),
                CheckerConfig {
                    ping: "/nonexistent/ping".into(),
                    net: self.net.clone(),
                    mount_root: self.dir.path().join("pools"),
                    hosts: hosts.iter().map(|h| h.to_string()).collect(),
                },
                self.audit.clone(),
            );
            state.ip = "/nonexistent/ip".into();
            state
        }

        /// 가짜 sysfs에 인터페이스 하나(operstate, carrier)
        fn link(&self, name: &str, operstate: &str, carrier: u8) {
            let dir = self.net.sys.join("class/net").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("operstate"), format!("{operstate}\n")).unwrap();
            std::fs::write(dir.join("carrier"), format!("{carrier}\n")).unwrap();
        }

        async fn check(&self, name: &str, spec: HaCheckSpec) {
            self.cluster
                .create(&Resource::new(name, spec))
                .await
                .unwrap();
        }

        fn node(&self) -> opl_types::node::Node {
            self.cluster.get::<NodeSpec>(self.node).unwrap().unwrap()
        }

        fn tasks(&self) -> Vec<Task> {
            self.cluster.list::<TaskSpec>().unwrap().1
        }

        /// 실행 파일 셸 스크립트를 쓴다(쓰는 동안 연 파일을 다른 스레드의 fork가 물려받지 않게 셸이 쓴다).
        fn script(&self, name: &str, text: &str) -> PathBuf {
            use std::io::Write;
            let path = self.dir.path().join(name);
            let mut sh = std::process::Command::new("/bin/sh")
                .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
                .arg(&path)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
            assert!(sh.wait().unwrap().success());
            path
        }

        /// `ip -j -d addr show` 대신 `json`을 찍는 가짜 `ip`
        fn fake_ip(&self, state: &mut State, json: &str) {
            state.ip = self.script("fake-ip", &format!("#!/bin/sh\ncat <<'EOF'\n{json}\nEOF\n"));
        }
    }

    fn spec(role: HaCheckRole, target: HaCheckTarget) -> HaCheckSpec {
        HaCheckSpec {
            role,
            target,
            on_fail: None,
            grace_secs: 10,
            nodes: BTreeMap::new(),
            enabled: true,
        }
    }

    fn link(iface: &str) -> HaCheckTarget {
        HaCheckTarget::Link {
            iface: iface.into(),
        }
    }

    fn states(reports: &[CheckReport]) -> Vec<(&str, CheckState)> {
        reports.iter().map(|r| (r.name.as_str(), r.state)).collect()
    }

    /// 검사가 없으면 결과가 비고, 리스 갱신에 실을 결과도 비운다.
    #[tokio::test]
    async fn no_checks_report_nothing() {
        let s = setup().await;
        s.cluster
            .set_lease_checks(vec![CheckReport::new("old", CheckState::Ok, None, None)]);
        let mut state = s.state(&[]);
        assert!(run_once(&mut state).await.is_empty());
        assert!(s.cluster.lease_checks().is_empty());
        assert!(s.node().status.checks.is_empty());
    }

    /// 링크 검사: carrier가 0이 되면 다음 회차에 실패이고 시작 시각이 붙으며 노드 상태에 쓴다. 결과가 같으면 다시
    /// 쓰지 않는다.
    #[tokio::test]
    async fn a_link_check_fails_when_the_carrier_drops() {
        let s = setup().await;
        s.link("eth1", "up", 1);
        s.check("stor", spec(HaCheckRole::Storage, link("eth1")))
            .await;
        let mut state = s.state(&[]);

        let reports = run_once(&mut state).await;
        assert_eq!(
            reports,
            [CheckReport::new("stor", CheckState::Ok, None, None)]
        );
        assert_eq!(s.cluster.lease_checks(), reports);
        let node = s.node();
        assert_eq!(node.status.checks, reports);
        // 같은 결과면 쓰지 않는다.
        run_once(&mut state).await;
        assert_eq!(s.node().revision, node.revision);

        s.link("eth1", "up", 0);
        let reports = run_once(&mut state).await;
        assert_eq!(reports[0].state, CheckState::Failing);
        let since = reports[0].since.expect("a failing check has a start");
        assert!(reports[0].error.as_deref().unwrap().contains("carrier 0"));
        assert_eq!(s.cluster.lease_checks(), reports);
        assert_eq!(s.node().status.checks, reports);
        assert_eq!(s.node().status.failing_checks(), ["stor"]);
        // 실패가 이어지면 시작 시각은 그대로이고 다시 쓰지 않는다.
        let revision = s.node().revision;
        let again = run_once(&mut state).await;
        assert_eq!(again[0].since, Some(since));
        assert_eq!(s.node().revision, revision);

        s.link("eth1", "up", 1);
        let reports = run_once(&mut state).await;
        assert_eq!(reports[0].state, CheckState::Ok);
        assert_eq!(reports[0].since, None);
    }

    /// 노드 덮기가 있으면 그 대상을 쓰고, 꺼진 검사는 돌리지 않는다.
    #[tokio::test]
    async fn node_overrides_are_used_and_disabled_checks_are_skipped() {
        let s = setup().await;
        s.link("eth1", "up", 1);
        let mut over = spec(HaCheckRole::Vm, link("eth1"));
        over.nodes.insert("n1".into(), link("eth9"));
        over.nodes.insert("n2".into(), link("eth1"));
        s.check("vmnet", over).await;
        let mut off = spec(HaCheckRole::Vm, link("eth1"));
        off.enabled = false;
        s.check("off", off).await;
        let mut state = s.state(&[]);
        let reports = run_once(&mut state).await;
        // 이 노드에 없는 인터페이스(덮기의 eth9)는 해당 없음이다.
        assert_eq!(states(&reports), [("vmnet", CheckState::Unknown)]);
        assert_eq!(
            reports[0].error.as_deref(),
            Some("interface eth9 not found on this node")
        );
    }

    /// 본 적 없는 인터페이스는 `unknown`(노드마다 이름이 다를 수 있다), 본 뒤에 사라지면 `failing`이다.
    #[tokio::test]
    async fn a_missing_interface_fails_only_after_it_was_seen() {
        let s = setup().await;
        s.check("vmnet", spec(HaCheckRole::Vm, link("eth1"))).await;
        let mut state = s.state(&[]);
        assert_eq!(run_once(&mut state).await[0].state, CheckState::Unknown);
        s.link("eth1", "up", 1);
        assert_eq!(run_once(&mut state).await[0].state, CheckState::Ok);
        std::fs::remove_dir_all(s.net.sys.join("class/net/eth1")).unwrap();
        let reports = run_once(&mut state).await;
        assert_eq!(reports[0].state, CheckState::Failing);
        assert_eq!(
            reports[0].error.as_deref(),
            Some("interface eth1 disappeared from this node")
        );
        // `ip`로 본 인터페이스도 본 것이다.
        let mut fresh = s.state(&[]);
        s.fake_ip(
            &mut fresh,
            r#"[{"ifname":"eth1","operstate":"UP","link_type":"ether"}]"#,
        );
        assert_eq!(run_once(&mut fresh).await[0].state, CheckState::Failing);
    }

    /// 풀어 줄 때까지 막히는 probe(멈춘 NFS처럼). 부른 수를 센다.
    struct StuckProbe {
        calls: AtomicUsize,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl DirProbe for StuckProbe {
        fn probe(&self, _dir: &Path, _file: &str) -> std::io::Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _ = self.release.lock().unwrap().recv();
            Err(std::io::Error::other("released"))
        }
    }

    /// 풀 검사: 없는 풀·이 노드가 쓰지 않는 풀은 `unknown`, 공유 디렉터리 풀은 probe 파일을 읽어 통과, probe가
    /// 멈추면 시간 뒤 실패이고 끝날 때까지 새로 띄우지 않는다.
    #[tokio::test]
    async fn pool_checks_probe_the_pool_directory() {
        let s = setup().await;
        let shared = s.dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        let pool = |nodes: Vec<Uuid>| StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: shared.clone(),
            },
            nodes,
            shared: true,
            backups: false,
        };
        s.cluster
            .create(&Resource::new("nas", pool(vec![])))
            .await
            .unwrap();
        s.cluster
            .create(&Resource::new("elsewhere", pool(vec![Uuid::now_v7()])))
            .await
            .unwrap();
        let target = |name: &str| HaCheckTarget::Pool { name: name.into() };
        s.check("a-missing", spec(HaCheckRole::Storage, target("gone")))
            .await;
        s.check(
            "b-elsewhere",
            spec(HaCheckRole::Storage, target("elsewhere")),
        )
        .await;
        s.check("c-nas", spec(HaCheckRole::Storage, target("nas")))
            .await;
        let mut state = s.state(&[]);
        let reports = run_once(&mut state).await;
        assert_eq!(
            states(&reports),
            [
                ("a-missing", CheckState::Unknown),
                ("b-elsewhere", CheckState::Unknown),
                ("c-nas", CheckState::Ok),
            ]
        );
        assert_eq!(reports[0].error.as_deref(), Some("no pool gone"));
        assert_eq!(
            reports[1].error.as_deref(),
            Some("this node does not use pool elsewhere")
        );
        assert!(
            shared
                .join(format!(".orbivisor-health-{}", s.node))
                .exists()
        );

        let (release, rx) = std::sync::mpsc::channel();
        let stuck = Arc::new(StuckProbe {
            calls: AtomicUsize::new(0),
            release: Mutex::new(rx),
        });
        state.probe = stuck.clone();
        state.probe_timeout = Duration::from_millis(200);
        let reports = run_once(&mut state).await;
        let not_responding = format!("{} is not responding", shared.display());
        assert_eq!(reports[2].state, CheckState::Failing);
        assert_eq!(reports[2].error.as_deref(), Some(not_responding.as_str()));
        let reports = run_once(&mut state).await;
        assert_eq!(reports[2].error.as_deref(), Some(not_responding.as_str()));
        assert_eq!(stuck.calls.load(Ordering::SeqCst), 1, "no second probe");
        // 막힌 작업을 풀어 런타임이 끝날 수 있게 한다.
        drop(release);
    }

    /// NFS 풀은 마운트되어 있어야 한다(mountinfo).
    #[tokio::test]
    async fn an_unmounted_nfs_pool_fails() {
        let s = setup().await;
        s.cluster
            .create(&Resource::new(
                "nfs",
                StoragePoolSpec {
                    backend: PoolBackend::Nfs {
                        server: "nas".into(),
                        export: "/srv".into(),
                        options: None,
                    },
                    nodes: vec![],
                    shared: false,
                    backups: false,
                },
            ))
            .await
            .unwrap();
        s.check(
            "nfs",
            spec(
                HaCheckRole::Storage,
                HaCheckTarget::Pool { name: "nfs".into() },
            ),
        )
        .await;
        std::fs::create_dir_all(s.net.proc.join("self")).unwrap();
        std::fs::write(s.net.proc.join("self/mountinfo"), "").unwrap();
        let mut state = s.state(&[]);
        state.linux = true;
        let reports = run_once(&mut state).await;
        assert_eq!(reports[0].state, CheckState::Failing);
        assert_eq!(reports[0].error.as_deref(), Some("nfs pool is not mounted"));
        state.linux = false;
        assert_eq!(run_once(&mut state).await[0].state, CheckState::Unknown);
    }

    /// 핑 검사: `need`개 이상 답해야 통과다.
    #[tokio::test]
    async fn ping_checks_need_enough_answers() {
        let s = setup().await;
        let ping = s.script(
            "fake-ping",
            "#!/bin/sh\nfor last; do :; done\n[ \"$last\" = 192.0.2.1 ]\n",
        );
        let target = |need| HaCheckTarget::Ping {
            addresses: vec!["192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap()],
            via: None,
            need,
        };
        s.check("gw1", spec(HaCheckRole::Vm, target(1))).await;
        s.check("gw2", spec(HaCheckRole::Vm, target(2))).await;
        let mut state = s.state(&[]);
        state.config.ping = ping;
        let reports = run_once(&mut state).await;
        assert_eq!(
            states(&reports),
            [("gw1", CheckState::Ok), ("gw2", CheckState::Failing)]
        );
        assert!(
            reports[1]
                .error
                .as_deref()
                .unwrap()
                .starts_with("1 of 2 addresses answered: ping 192.0.2.2"),
            "{:?}",
            reports[1].error
        );

        // 핑이 없으면 대상의 실패가 아니라 `unknown`이다.
        state.config.ping = s.dir.path().join("none/ping");
        let reports = run_once(&mut state).await;
        assert_eq!(
            states(&reports),
            [("gw1", CheckState::Unknown), ("gw2", CheckState::Unknown)]
        );
        assert_eq!(
            reports[0].error.as_deref(),
            Some("ping not found: install iputils-ping")
        );
    }

    fn iface(name: &str, kind: InterfaceKind, addresses: &[&str]) -> NodeInterface {
        NodeInterface {
            name: name.into(),
            kind,
            state: "up".into(),
            carrier: Some(true),
            speed_mbps: None,
            mac: None,
            master: None,
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            bond: None,
        }
    }

    /// 자동 검사: 링크마다 그 호스트의 IP를 가진 인터페이스의 링크 검사, 없거나 풀지 못했으면 이유
    #[test]
    fn auto_checks_follow_the_cluster_links() {
        let host = |name: &str, ips: Option<&[&str]>| Host {
            name: name.into(),
            ips: ips.map(|ips| ips.iter().map(|ip| ip.parse().unwrap()).collect()),
        };
        let interfaces = [
            iface("eno1", InterfaceKind::Ethernet, &[]),
            iface(
                "vmbr0",
                InterfaceKind::Bridge,
                &["10.0.0.5/24", "fd00::5/64"],
            ),
            iface("bond1", InterfaceKind::Bond, &["10.1.0.5/24"]),
        ];
        let hosts = [
            host("node1.lan", Some(&["fd00::5"])),
            host("10.1.0.5", Some(&["10.1.0.5"])),
            host("10.2.0.5", Some(&["10.2.0.5"])),
            host("node1-san", None),
        ];
        assert_eq!(
            auto_checks(&hosts, &interfaces),
            [
                ("cluster-link-1".to_string(), Ok(link("vmbr0"))),
                ("cluster-link-2".to_string(), Ok(link("bond1"))),
                (
                    "cluster-link-3".to_string(),
                    Err("no interface has address 10.2.0.5".to_string())
                ),
                (
                    "cluster-link-4".to_string(),
                    Err("cannot resolve node1-san".to_string())
                ),
            ]
        );
    }

    const IP_JSON: &str = r#"[
        {"ifname":"lo","operstate":"UNKNOWN","link_type":"loopback","addr_info":[{"local":"127.0.0.1","prefixlen":8}]},
        {"ifname":"eno1","operstate":"UP","master":"bond0","link_type":"ether","address":"52:54:00:00:00:01"},
        {"ifname":"eno2","operstate":"UP","master":"bond0","link_type":"ether","address":"52:54:00:00:00:02"},
        {"ifname":"bond0","operstate":"UP","link_type":"ether","linkinfo":{"info_kind":"bond"},
         "addr_info":[{"local":"10.0.0.5","prefixlen":24}]}
    ]"#;

    impl Setup {
        /// 가짜 sysfs의 active-backup 본딩 `bond0`(포트 eno1·eno2)
        fn bond(&self, eno2_carrier: u8) {
            self.link("eno1", "up", 1);
            self.link(
                "eno2",
                if eno2_carrier == 1 { "up" } else { "down" },
                eno2_carrier,
            );
            self.link("bond0", "up", 1);
            let dir = self.net.sys.join("class/net/bond0/bonding");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("mode"), "active-backup 1\n").unwrap();
            std::fs::write(dir.join("slaves"), "eno1 eno2\n").unwrap();
        }

        async fn audit_actions(&self) -> Vec<(String, Option<String>)> {
            self.audit.shutdown().await;
            let mut entries = self
                .audit
                .read(AuditQuery {
                    since: 0,
                    until: u64::MAX,
                    user: None,
                    action: None,
                    limit: 100,
                })
                .await
                .unwrap();
            entries.reverse();
            for e in &entries {
                assert_eq!(
                    e.target,
                    Some(AuditTarget {
                        kind: "nodes".into(),
                        key: "n1".into()
                    })
                );
            }
            entries.into_iter().map(|e| (e.action, e.detail)).collect()
        }
    }

    /// `ip`로 읽은 인터페이스를 노드 상태에 쓰고, 그 IP를 가진 인터페이스로 자동 검사를 만든다.
    #[tokio::test]
    async fn interfaces_are_recorded_and_feed_the_auto_checks() {
        let s = setup().await;
        s.bond(1);
        let mut state = s.state(&["10.0.0.5", "10.9.0.5"]);
        s.fake_ip(&mut state, IP_JSON);
        let reports = run_once(&mut state).await;
        assert_eq!(
            states(&reports),
            [
                ("cluster-link-1", CheckState::Ok),
                ("cluster-link-2", CheckState::Unknown),
            ]
        );
        let interfaces = s.node().status.interfaces;
        let names: Vec<&str> = interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["eno1", "eno2", "bond0"]);
        assert_eq!(interfaces[2].kind, InterfaceKind::Bond);
        assert_eq!(interfaces[2].bond.as_ref().unwrap().state, BondState::Up);

        // `ip`가 실패해도 기록된 목록은 지우지 않는다.
        state.ip = "/nonexistent/ip".into();
        state.interfaces_at = None;
        run_once(&mut state).await;
        assert_eq!(s.node().status.interfaces, interfaces);
        // 처음부터 `ip`가 없으면 목록을 쓰지 않는다.
        let mut fresh = s.state(&[]);
        run_once(&mut fresh).await;
        assert_eq!(s.node().status.interfaces, interfaces);
    }

    /// 인터페이스가 64개를 넘어도 자동 검사는 모두에서 찾고, 노드 상태에는 주소가 있는 것을 남긴 64개를 쓴다.
    #[tokio::test]
    async fn auto_checks_see_interfaces_beyond_the_reported_cap() {
        let s = setup().await;
        let links: Vec<String> = (0..70)
            .map(|i| {
                let addr = if i == 69 {
                    r#","addr_info":[{"local":"10.0.0.5","prefixlen":24}]"#
                } else {
                    ""
                };
                format!(r#"{{"ifname":"eth{i}","operstate":"UP","link_type":"ether"{addr}}}"#)
            })
            .collect();
        s.link("eth69", "up", 1);
        let mut state = s.state(&["10.0.0.5"]);
        s.fake_ip(&mut state, &format!("[{}]", links.join(",")));
        let reports = run_once(&mut state).await;
        assert_eq!(states(&reports), [("cluster-link-1", CheckState::Ok)]);
        let interfaces = s.node().status.interfaces;
        assert_eq!(interfaces.len(), opl_types::node::MAX_NODE_INTERFACES);
        assert_eq!(interfaces.last().unwrap().name, "eth69");
    }

    /// 본딩의 새 상태가 세 회차 이어지면 알린다: degraded가 되면 감사 항목과 기록 Task 하나씩, 같은 상태면 없음,
    /// 회복은 감사만. 모든 노드가 `ha-checks`를 알지 않으면 Task를 쓰지 않는다.
    #[tokio::test]
    async fn bond_changes_are_audited_and_recorded() {
        let s = setup().await;
        s.bond(1);
        let mut state = s.state(&[]);
        s.fake_ip(&mut state, IP_JSON);
        run_once(&mut state).await;
        assert!(s.tasks().is_empty(), "a healthy start is not reported");

        s.bond(0);
        run_once(&mut state).await;
        run_once(&mut state).await;
        assert!(s.tasks().is_empty(), "not steady yet");
        run_once(&mut state).await;
        let tasks = s.tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0].spec.op,
            TaskOp::BondDegraded {
                bond: "bond0".into(),
                ports: vec!["eno2: down".into()],
            }
        );
        assert_eq!((tasks[0].spec.target, tasks[0].spec.node), (s.node, s.node));
        assert_eq!(tasks[0].status.state, TaskState::Succeeded);
        run_once(&mut state).await;
        assert_eq!(s.tasks().len(), 1, "the same state is not reported again");

        s.bond(1);
        for _ in 0..3 {
            run_once(&mut state).await;
        }
        assert_eq!(s.tasks().len(), 1, "a recovery has no task");

        // 옛 노드가 있으면 Task 없이 감사만 한다(알림 간격을 새로 시작하려고 새 검사기로).
        bootstrap::update_node_status(&s.cluster, |status| {
            status.features.clear();
            true
        })
        .await
        .unwrap();
        let mut state = s.state(&[]);
        s.fake_ip(&mut state, IP_JSON);
        s.bond(0);
        for _ in 0..3 {
            run_once(&mut state).await;
        }
        assert_eq!(s.tasks().len(), 1);

        assert_eq!(
            s.audit_actions().await,
            [
                (
                    "node.bond-degraded".to_string(),
                    Some("bond0: eno2 down".to_string())
                ),
                (
                    "node.bond-recovered".to_string(),
                    Some("bond0: all ports up".to_string())
                ),
                (
                    "node.bond-degraded".to_string(),
                    Some("bond0: eno2 down".to_string())
                ),
            ]
        );
    }

    /// 처음부터 degraded인 본딩은 이어지면 한 번 알린다.
    #[tokio::test]
    async fn a_bond_degraded_at_start_is_reported_once() {
        let s = setup().await;
        s.bond(0);
        let mut state = s.state(&[]);
        s.fake_ip(&mut state, IP_JSON);
        for _ in 0..5 {
            run_once(&mut state).await;
        }
        assert_eq!(s.tasks().len(), 1);
        assert_eq!(s.audit_actions().await.len(), 1);
    }

    fn bond_status(state: BondState) -> BondStatus {
        BondStatus {
            mode: "802.3ad".into(),
            state,
            ports: vec![
                opl_types::node::BondPort {
                    name: "eno1".into(),
                    state: PortState::Up,
                    speed_mbps: None,
                    aggregator: None,
                },
                opl_types::node::BondPort {
                    name: "eno2".into(),
                    state: if state == BondState::Up {
                        PortState::Up
                    } else {
                        PortState::LacpMismatch
                    },
                    speed_mbps: None,
                    aggregator: None,
                },
            ],
        }
    }

    /// `bond0`이 `state`인 회차
    fn bonds(state: BondState) -> BTreeMap<String, BondStatus> {
        BTreeMap::from([("bond0".to_string(), bond_status(state))])
    }

    /// 새 상태는 세 회차 이어져야 알리고(흔들리는 포트는 알리지 않음), 처음 보는 본딩은 `up`에서 시작한다.
    #[test]
    fn bond_changes_must_be_steady() {
        let t0 = Instant::now();
        let round = |n: u64| t0 + CHECK_INTERVAL * n as u32;
        let mut watch = BondWatch::default();
        assert!(watch.observe(round(0), &bonds(BondState::Up)).is_empty());
        // 흔들림: degraded와 up이 번갈아 오면 알리지 않는다.
        for n in 1..=10 {
            let state = if n % 2 == 1 {
                BondState::Degraded
            } else {
                BondState::Up
            };
            assert!(watch.observe(round(n), &bonds(state)).is_empty(), "{n}");
        }
        assert!(
            watch
                .observe(round(11), &bonds(BondState::Degraded))
                .is_empty()
        );
        assert!(
            watch
                .observe(round(12), &bonds(BondState::Degraded))
                .is_empty()
        );
        let changes = watch.observe(round(13), &bonds(BondState::Degraded));
        assert_eq!(
            changes,
            [BondChange {
                bond: "bond0".into(),
                state: BondState::Degraded,
                detail: "bond0: eno2 lacp_mismatch".into(),
                ports: vec!["eno2: lacp_mismatch".into()],
                alert: true,
            }]
        );
        // 같은 상태가 이어지면 다시 알리지 않는다.
        assert!(
            watch
                .observe(round(14), &bonds(BondState::Degraded))
                .is_empty()
        );

        // 처음부터 degraded인 본딩도 이어지면 한 번 알린다.
        let mut watch = BondWatch::default();
        let first: Vec<usize> = (0..5)
            .map(|n| watch.observe(round(n), &bonds(BondState::Degraded)).len())
            .collect();
        assert_eq!(first, [0, 0, 1, 0, 0]);
    }

    /// 저하·끊김 알림은 본딩마다 10분에 한 번까지(회복 알림은 알린 저하·끊김의 것만).
    #[test]
    fn bond_alerts_are_limited_per_bond() {
        let t0 = Instant::now();
        let mut watch = BondWatch::default();
        let mut steady = |at: Instant, state: BondState| {
            let mut changes = vec![];
            for i in 0..BOND_STEADY_ROUNDS {
                changes.extend(watch.observe(at + CHECK_INTERVAL * i, &bonds(state)));
            }
            assert_eq!(changes.len(), 1, "{state:?}");
            (changes[0].state, changes[0].alert)
        };
        assert_eq!(steady(t0, BondState::Degraded), (BondState::Degraded, true));
        let minute = Duration::from_secs(60);
        assert_eq!(steady(t0 + minute, BondState::Up), (BondState::Up, true));
        // 10분 안의 두 번째 저하·끊김은 알리지 않고(로그만) 그 회복도 알리지 않는다.
        assert_eq!(
            steady(t0 + minute * 2, BondState::Down),
            (BondState::Down, false)
        );
        assert_eq!(
            steady(t0 + minute * 3, BondState::Up),
            (BondState::Up, false)
        );
        assert_eq!(
            steady(t0 + minute * 11, BondState::Degraded),
            (BondState::Degraded, true)
        );
    }

    /// `degraded`에서 `down`으로 나빠지면 알림 간격 안이어도 알리고, 그 회복도 알린다.
    #[test]
    fn a_bond_going_down_is_reported_within_the_alert_interval() {
        let t0 = Instant::now();
        let mut watch = BondWatch::default();
        let mut steady = |at: Instant, state: BondState| {
            let mut changes = vec![];
            for i in 0..BOND_STEADY_ROUNDS {
                changes.extend(watch.observe(at + CHECK_INTERVAL * i, &bonds(state)));
            }
            assert_eq!(changes.len(), 1, "{state:?}");
            (changes[0].state, changes[0].alert)
        };
        let minute = Duration::from_secs(60);
        assert_eq!(steady(t0, BondState::Degraded), (BondState::Degraded, true));
        assert_eq!(
            steady(t0 + minute, BondState::Down),
            (BondState::Down, true)
        );
        assert_eq!(
            steady(t0 + minute * 2, BondState::Up),
            (BondState::Up, true)
        );
        // 간격 안의 저하는 알리지 않지만 그것이 끊김으로 나빠지면 알린다.
        assert_eq!(
            steady(t0 + minute * 3, BondState::Degraded),
            (BondState::Degraded, false)
        );
        assert_eq!(
            steady(t0 + minute * 4, BondState::Down),
            (BondState::Down, true)
        );
        assert_eq!(
            steady(t0 + minute * 5, BondState::Up),
            (BondState::Up, true)
        );
    }

    /// 핑의 `via`가 이 노드에 없으면 링크 검사처럼 본 적이 없으면 `unknown`, 본 뒤 사라졌으면 `failing`이다.
    #[tokio::test]
    async fn a_missing_ping_interface_is_unknown_until_it_was_seen() {
        let s = setup().await;
        let ping = s.script("fake-ping", "#!/bin/sh\nexit 0\n");
        let target = HaCheckTarget::Ping {
            addresses: vec!["192.0.2.1".parse().unwrap()],
            via: Some("eth9".into()),
            need: 1,
        };
        s.check("gw", spec(HaCheckRole::Vm, target)).await;
        let mut state = s.state(&[]);
        state.config.ping = ping;
        let reports = run_once(&mut state).await;
        assert_eq!(states(&reports), [("gw", CheckState::Unknown)]);
        assert_eq!(
            reports[0].error.as_deref(),
            Some("interface eth9 not found on this node")
        );
        s.link("eth9", "up", 1);
        // 있으면 핑한다(macOS의 핑은 `via`를 받지 않아 실패다).
        let state_now = run_once(&mut state).await[0].state;
        let want = if cfg!(target_os = "linux") {
            CheckState::Ok
        } else {
            CheckState::Failing
        };
        assert_eq!(state_now, want);
        std::fs::remove_dir_all(s.net.sys.join("class/net/eth9")).unwrap();
        let reports = run_once(&mut state).await;
        assert_eq!(states(&reports), [("gw", CheckState::Failing)]);
        assert_eq!(
            reports[0].error.as_deref(),
            Some("interface eth9 disappeared from this node")
        );
    }

    /// 다시 시작한 검사기도 저장된 노드 상태의 인터페이스를 본 것으로 둔다: 사라졌으면 `failing`이다.
    #[tokio::test]
    async fn interfaces_recorded_before_a_restart_count_as_seen() {
        let s = setup().await;
        s.link("eth1", "up", 1);
        s.check("vmnet", spec(HaCheckRole::Vm, link("eth1"))).await;
        let mut state = s.state(&[]);
        s.fake_ip(
            &mut state,
            r#"[{"ifname":"eth1","operstate":"UP","link_type":"ether"}]"#,
        );
        assert_eq!(run_once(&mut state).await[0].state, CheckState::Ok);
        assert_eq!(s.node().status.interfaces[0].name, "eth1");
        std::fs::remove_dir_all(s.net.sys.join("class/net/eth1")).unwrap();
        // 새 검사기(`ip` 없음)
        let mut restarted = s.state(&[]);
        let reports = run_once(&mut restarted).await;
        assert_eq!(states(&reports), [("vmnet", CheckState::Failing)]);
        assert_eq!(
            reports[0].error.as_deref(),
            Some("interface eth1 disappeared from this node")
        );
    }

    /// 자동 검사는 짝지은 인터페이스가 주소를 잃어도 그 인터페이스를 계속 검사한다.
    #[tokio::test]
    async fn auto_checks_keep_the_interface_that_lost_its_address() {
        let s = setup().await;
        s.link("eth1", "up", 1);
        let mut state = s.state(&["10.0.0.5"]);
        s.fake_ip(
            &mut state,
            r#"[{"ifname":"eth1","operstate":"UP","link_type":"ether","addr_info":[{"local":"10.0.0.5","prefixlen":24}]}]"#,
        );
        assert_eq!(
            states(&run_once(&mut state).await),
            [("cluster-link-1", CheckState::Ok)]
        );
        s.link("eth1", "down", 0);
        s.fake_ip(
            &mut state,
            r#"[{"ifname":"eth1","operstate":"DOWN","link_type":"ether"}]"#,
        );
        state.interfaces_at = None;
        let reports = run_once(&mut state).await;
        assert_eq!(states(&reports), [("cluster-link-1", CheckState::Failing)]);
        assert!(
            reports[0]
                .error
                .as_deref()
                .unwrap()
                .starts_with("eth1 is down"),
            "{:?}",
            reports[0].error
        );
        // 처음부터 짝이 없으면 그대로 `unknown`이다.
        let mut fresh = s.state(&["10.0.0.5"]);
        s.fake_ip(
            &mut fresh,
            r#"[{"ifname":"eth1","operstate":"DOWN","link_type":"ether"}]"#,
        );
        assert_eq!(
            states(&run_once(&mut fresh).await),
            [("cluster-link-1", CheckState::Unknown)]
        );
    }
}
