//! 측정기. 5초마다 이 노드와 이 노드에서 도는 VM을 재어 [`MetricsStore`]에 넣는다.
//!
//! 비율은 직전 측정과의 차이를 걸린 시간(단조 시계)으로 나눈다. 첫 측정(시작, VM 시작·붙기, pid
//! 바뀜, 카운터가 줄어듦)은 기준만 잡고 점을 만들지 않는다.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::join_all;
use opl_cluster::Cluster;
use opl_driver::{DiskCounters, Hypervisor, RuntimeStatus};
use opl_types::metrics::{NodeSample, VmSample};
use opl_types::network::{USER_NETWORK, tap_name};
use opl_types::vm::VmSpec;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

use super::host::{CpuTimes, HostProbe};
use super::store::MetricsStore;

/// 측정 간격(고정)
pub const INTERVAL: Duration = Duration::from_secs(5);
/// VM 하나의 `query-blockstats`를 기다리는 시간. 넘으면 그 VM의 디스크 필드만 `null`이다.
pub const DISK_STATS_TIMEOUT: Duration = Duration::from_secs(1);
/// 한 번의 측정에서 VM들을 재는 시간. 넘은 VM은 이번 점이 없다.
pub const VM_TIMEOUT: Duration = Duration::from_secs(4);
/// 같은 VM의 실패 경고를 다시 남기는 간격
pub const WARN_EVERY: Duration = Duration::from_secs(600);

pub struct MetricsSampler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl MetricsSampler {
    /// `data_dir`은 노드 점의 파일 시스템 값을 읽는 데이터 디렉터리다.
    pub fn spawn(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        let sampler = Sampler::new(hv, probe, cluster, data_dir, store);
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_sampler(sampler, rx)),
        }
    }

    /// 멈춘다. 저장소의 점은 그대로 둔다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_sampler(mut sampler: Sampler, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = sampler.tick() => {}
        }
    }
}

/// 노드의 직전 누적 카운터
struct NodeCounters {
    at: Instant,
    cpu: Option<CpuTimes>,
    disk: Option<(u64, u64)>,
    nic: Option<(u64, u64)>,
}

/// VM의 직전 누적 카운터. pid가 바뀌면 다시 기준을 잡는다.
#[derive(Debug, Clone, Copy)]
struct VmCounters {
    pid: u32,
    /// 프로세스·tap을 읽은 시각(이번 측정의 VM 측정 시작, `disk_stats`를 기다리기 전)
    at: Instant,
    cpu_ns: Option<u64>,
    disk: Option<DiskCounters>,
    /// `disk_stats`가 답한 시각. QMP 응답은 늦을 수 있어(최대 1초) 디스크 비율은 이 시각의 차이로 나눈다.
    disk_at: Instant,
    /// 모든 tap의 호스트 쪽 (받은, 보낸) 합
    tap: Option<(u64, u64)>,
}

/// VM 하나를 잰 것
struct VmReading {
    counters: VmCounters,
    rss_mib: Option<u64>,
    /// `query-blockstats`가 실패했으면 그 오류
    disk_error: Option<String>,
}

/// 카운터가 직전보다 줄었다(다시 기준을 잡는다).
struct Decreased;

/// 누적 값의 초당 증가. 어느 한쪽이라도 없으면 `None`이다.
fn rate(prev: Option<u64>, now: Option<u64>, secs: f64) -> Result<Option<f64>, Decreased> {
    match (prev, now) {
        (Some(prev), Some(now)) => {
            let delta = now.checked_sub(prev).ok_or(Decreased)?;
            Ok(Some(delta as f64 / secs))
        }
        _ => Ok(None),
    }
}

fn node_sample(
    prev: &NodeCounters,
    now: &NodeCounters,
    t: i64,
    memory: Option<(u64, u64)>,
    fs: Option<(u64, u64)>,
) -> Result<NodeSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    let cpu = match (prev.cpu, now.cpu) {
        (Some(prev), Some(now)) => {
            let busy = now.busy.checked_sub(prev.busy).ok_or(Decreased)?;
            let total = now.total.checked_sub(prev.total).ok_or(Decreased)?;
            (total > 0).then(|| busy as f64 * 100.0 / total as f64)
        }
        _ => None,
    };
    let first = |c: Option<(u64, u64)>| c.map(|c| c.0);
    let second = |c: Option<(u64, u64)>| c.map(|c| c.1);
    Ok(NodeSample {
        t,
        cpu,
        mem_used_mib: memory.map(|m| m.0),
        mem_total_mib: memory.map(|m| m.1),
        disk_read_bps: rate(first(prev.disk), first(now.disk), secs)?,
        disk_write_bps: rate(second(prev.disk), second(now.disk), secs)?,
        net_rx_bps: rate(first(prev.nic), first(now.nic), secs)?,
        net_tx_bps: rate(second(prev.nic), second(now.nic), secs)?,
        fs_used_mib: fs.map(|f| f.0),
        fs_total_mib: fs.map(|f| f.1),
    })
}

fn vm_sample(
    prev: &VmCounters,
    now: &VmCounters,
    t: i64,
    vcpus: Option<u32>,
    rss_mib: Option<u64>,
) -> Result<VmSample, Decreased> {
    let secs = now.at.duration_since(prev.at).as_secs_f64();
    let disk_secs = now.disk_at.duration_since(prev.disk_at).as_secs_f64();
    // 바쁜 vCPU 수 × 100. QEMU 프로세스 전체라 100을 조금 넘을 수 있고 자르지 않는다.
    let cpu = rate(prev.cpu_ns, now.cpu_ns, secs)?
        .zip(vcpus.filter(|&n| n > 0))
        .map(|(ns, n)| ns / 1e9 * 100.0 / f64::from(n));
    let disk = |f: fn(&DiskCounters) -> u64| {
        rate(
            prev.disk.as_ref().map(f),
            now.disk.as_ref().map(f),
            disk_secs,
        )
    };
    let tap =
        |f: fn(&(u64, u64)) -> u64| rate(prev.tap.as_ref().map(f), now.tap.as_ref().map(f), secs);
    Ok(VmSample {
        t,
        cpu,
        mem_rss_mib: rss_mib,
        disk_read_bps: disk(|d| d.rd_bytes)?,
        disk_write_bps: disk(|d| d.wr_bytes)?,
        disk_read_iops: disk(|d| d.rd_operations)?,
        disk_write_iops: disk(|d| d.wr_operations)?,
        // tap은 호스트 쪽 방향이다. 게스트가 받은 것은 호스트가 tap으로 보낸 것이다.
        net_rx_bps: tap(|t| t.1)?,
        net_tx_bps: tap(|t| t.0)?,
    })
}

/// 지금 벽시계(유닉스 밀리초)
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 마지막 경고에서 [`WARN_EVERY`]가 지났으면 지금을 적고 true
fn warn_due(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < WARN_EVERY) {
        return false;
    }
    *last = Some(now);
    true
}

struct Sampler {
    hv: Arc<dyn Hypervisor>,
    probe: Arc<dyn HostProbe>,
    cluster: Arc<Cluster>,
    data_dir: PathBuf,
    store: Arc<MetricsStore>,
    node: Option<NodeCounters>,
    vms: HashMap<Uuid, VmCounters>,
    /// VM마다 마지막 디스크 실패 경고 시각
    disk_warned: HashMap<Uuid, Option<Instant>>,
    list_warned: Option<Instant>,
}

impl Sampler {
    fn new(
        hv: Arc<dyn Hypervisor>,
        probe: Arc<dyn HostProbe>,
        cluster: Arc<Cluster>,
        data_dir: PathBuf,
        store: Arc<MetricsStore>,
    ) -> Self {
        Self {
            hv,
            probe,
            cluster,
            data_dir,
            store,
            node: None,
            vms: HashMap::new(),
            disk_warned: HashMap::new(),
            list_warned: None,
        }
    }

    /// 한 번 잰다.
    async fn tick(&mut self) {
        let started = Instant::now();
        let t = now_ms();
        self.sample_node(started, t);
        self.sample_vms(started).await;
        self.store.prune(t);
    }

    fn sample_node(&mut self, at: Instant, t: i64) {
        let now = NodeCounters {
            at,
            cpu: self.probe.cpu_times(),
            disk: self.probe.disk_bytes(),
            nic: self.probe.nic_bytes(),
        };
        if let Some(prev) = &self.node
            && let Ok(sample) = node_sample(
                prev,
                &now,
                t,
                self.probe.memory(),
                self.probe.fs(&self.data_dir),
            )
        {
            self.store.push_node(sample);
        }
        self.node = Some(now);
    }

    async fn sample_vms(&mut self, started: Instant) {
        let running = match self.hv.list().await {
            Ok(vms) => vms,
            Err(e) => {
                if warn_due(&mut self.list_warned, started) {
                    tracing::warn!(error = %e, "cannot list VMs for metrics");
                }
                return;
            }
        };
        let running: Vec<_> = running
            .into_iter()
            .filter(|vm| matches!(vm.status, RuntimeStatus::Running | RuntimeStatus::Paused))
            .collect();
        // vCPU 수와 tap NIC 번호는 클러스터의 VM 스펙에서 온다(없으면 CPU·네트워크가 `null`). 사용자
        // 모드 NIC(`user`)는 tap이 없어 뺀다.
        let specs: HashMap<Uuid, (u32, Vec<usize>)> = self
            .cluster
            .list::<VmSpec>()
            .map(|(_, vms)| {
                vms.into_iter()
                    .map(|vm| {
                        let cpu = &vm.spec.cpu;
                        let vcpus = u32::from(cpu.sockets) * u32::from(cpu.cores);
                        let taps = (vm.spec.nics.iter().enumerate())
                            .filter(|(_, nic)| nic.network != USER_NETWORK)
                            .map(|(i, _)| i)
                            .collect();
                        (vm.id, (vcpus, taps))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 시각(단조·벽시계)과 마감은 `list()`가 끝난 뒤에 잡는다(QEMU의 `list()`는 VM마다 QMP를 차례로
        // 불러 응답 없는 VM 하나로 늦어질 수 있다).
        let at = Instant::now();
        let t = now_ms();
        let deadline = at + VM_TIMEOUT;
        let this = &*self;
        let readings = join_all(running.iter().map(|vm| {
            let taps = specs.get(&vm.id).map_or(&[][..], |s| &s.1[..]);
            async move {
                let reading =
                    tokio::time::timeout_at(deadline, this.measure_vm(vm.id, vm.pid, taps, at))
                        .await;
                (vm.id, reading.ok())
            }
        }))
        .await;

        // 사라진 VM의 카운터는 지운다(다시 나타나면 기준부터).
        let live: HashSet<Uuid> = running.iter().map(|vm| vm.id).collect();
        self.vms.retain(|id, _| live.contains(id));
        self.disk_warned.retain(|id, _| live.contains(id));
        for (id, reading) in readings {
            // 측정 시간을 넘었다. 직전 카운터를 두고 다음에 잰다.
            let Some(reading) = reading else { continue };
            if let Some(error) = &reading.disk_error
                && warn_due(self.disk_warned.entry(id).or_default(), started)
            {
                tracing::warn!(vm = %id, error, "cannot read VM disk counters for metrics");
            }
            let now = reading.counters;
            if let Some(prev) = self.vms.get(&id)
                && prev.pid == now.pid
                && let Ok(sample) =
                    vm_sample(prev, &now, t, specs.get(&id).map(|s| s.0), reading.rss_mib)
            {
                self.store.push_vm(id, sample);
            }
            self.vms.insert(id, now);
        }
    }

    /// `at`은 이번 측정의 VM 측정 시작 시각이다. 프로세스·tap은 기다리지 않고 곧바로 읽으므로 그 시각으로
    /// 적고, 디스크는 `disk_stats`의 응답을 기다린 뒤 응답 시각으로 따로 적는다.
    async fn measure_vm(&self, id: Uuid, pid: u32, taps: &[usize], at: Instant) -> VmReading {
        let process = self.probe.process(pid);
        // tap NIC가 없거나(사용자 모드 NIC뿐, NIC 없음) tap 하나라도 읽지 못하면 네트워크는 `null`이다.
        let tap = (!taps.is_empty())
            .then(|| {
                taps.iter().try_fold((0u64, 0u64), |(rx, tx), &i| {
                    let (r, t) = self.probe.tap_bytes(&tap_name(id, i))?;
                    Some((rx + r, tx + t))
                })
            })
            .flatten();
        let (disk, disk_error) =
            match tokio::time::timeout(DISK_STATS_TIMEOUT, self.hv.disk_stats(id)).await {
                Ok(Ok(counters)) => (Some(counters), None),
                Ok(Err(e)) => (None, Some(e.to_string())),
                Err(_) => (None, Some("query-blockstats timed out".to_string())),
            };
        VmReading {
            counters: VmCounters {
                pid,
                at,
                cpu_ns: process.map(|p| p.cpu_ns),
                disk,
                disk_at: Instant::now(),
                tap,
            },
            rss_mib: process.map(|p| p.rss_mib),
            disk_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use opl_driver::{MockHypervisor, StopMode};
    use opl_types::resource::Resource;
    use opl_types::runtime::VmRuntimeSpec;
    use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType, NicModel, NicSpec, PowerState};

    use super::super::host::ProcStat;
    use super::*;
    use crate::bootstrap::tests::single_node;
    use opl_types::metrics::{MetricsQuery, MetricsScope};

    /// 시험이 정한 값을 돌려주는 [`HostProbe`]
    #[derive(Default)]
    struct FakeProbe(Mutex<FakeValues>);

    #[derive(Default, Clone)]
    struct FakeValues {
        cpu: Option<CpuTimes>,
        memory: Option<(u64, u64)>,
        disk: Option<(u64, u64)>,
        nic: Option<(u64, u64)>,
        processes: HashMap<u32, ProcStat>,
        taps: HashMap<String, (u64, u64)>,
        fs: Option<(u64, u64)>,
        /// `process()`가 불린 시각들
        process_calls: Vec<Instant>,
    }

    impl FakeProbe {
        fn set(&self, f: impl FnOnce(&mut FakeValues)) {
            f(&mut self.0.lock().unwrap());
        }
    }

    impl HostProbe for FakeProbe {
        fn cpu_times(&self) -> Option<CpuTimes> {
            self.0.lock().unwrap().cpu
        }
        fn memory(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().memory
        }
        fn disk_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().disk
        }
        fn nic_bytes(&self) -> Option<(u64, u64)> {
            self.0.lock().unwrap().nic
        }
        fn process(&self, pid: u32) -> Option<ProcStat> {
            let mut v = self.0.lock().unwrap();
            v.process_calls.push(Instant::now());
            v.processes.get(&pid).copied()
        }
        fn tap_bytes(&self, name: &str) -> Option<(u64, u64)> {
            self.0.lock().unwrap().taps.get(name).copied()
        }
        fn fs(&self, _path: &Path) -> Option<(u64, u64)> {
            self.0.lock().unwrap().fs
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        cluster: Arc<Cluster>,
        hv: Arc<MockHypervisor>,
        probe: Arc<FakeProbe>,
        store: Arc<MetricsStore>,
        sampler: Sampler,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, _) = single_node(dir.path()).await;
        let cluster = Arc::new(cluster);
        let hv = Arc::new(MockHypervisor::default());
        let probe = Arc::new(FakeProbe::default());
        let store = Arc::new(MetricsStore::default());
        let sampler = Sampler::new(
            hv.clone(),
            probe.clone(),
            cluster.clone(),
            dir.path().to_path_buf(),
            store.clone(),
        );
        Fixture {
            _dir: dir,
            cluster,
            hv,
            probe,
            store,
            sampler,
        }
    }

    fn node_samples(store: &MetricsStore) -> Vec<NodeSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None,
            })
            .node
            .unwrap()
    }

    fn vm_samples(store: &MetricsStore, id: Uuid) -> Vec<VmSample> {
        store
            .read(&MetricsQuery {
                scope: MetricsScope::Vm { id },
                since: None,
            })
            .vm
            .unwrap_or_default()
    }

    /// 클러스터에 vCPU 2개, NIC 2개(`lan`)인 VM을 만들고 하이퍼바이저에서 켠다.
    async fn running_vm(f: &Fixture) -> Uuid {
        running_vm_on(f, &["lan", "lan"]).await
    }

    /// [`running_vm`]과 같고 NIC마다 네트워크를 정한다.
    async fn running_vm_on(f: &Fixture, networks: &[&str]) -> Uuid {
        let cpu = CpuSpec {
            sockets: 1,
            cores: 2,
            model: CpuModel::Host,
        };
        let nics = networks
            .iter()
            .map(|network| NicSpec {
                network: network.to_string(),
                model: NicModel::Virtio,
                mac: None,
                security_groups: vec![],
            })
            .collect();
        let vm = Resource::new(
            "web",
            VmSpec {
                cpu: cpu.clone(),
                memory_mib: 256,
                firmware: Default::default(),
                machine: MachineType::Virt,
                disks: vec![],
                nics,
                power: PowerState::Running,
                restart: Default::default(),
                node: None,
                snapshot_schedule: None,
                ha: None,
            },
        );
        f.cluster.create(&vm).await.unwrap();
        start(f, vm.id).await;
        vm.id
    }

    async fn start(f: &Fixture, id: Uuid) {
        f.hv.start(
            &VmRuntimeSpec {
                id,
                name: "web".into(),
                cpu: CpuSpec {
                    sockets: 1,
                    cores: 2,
                    model: CpuModel::Host,
                },
                memory_mib: 256,
                firmware: Firmware::Uefi,
                machine: MachineType::Virt,
                disks: vec![],
                nics: vec![],
            },
            None,
        )
        .await
        .unwrap();
    }

    fn pid_of(hv: &MockHypervisor, id: Uuid) -> u32 {
        futures_util::FutureExt::now_or_never(hv.list())
            .unwrap()
            .unwrap()
            .into_iter()
            .find(|vm| vm.id == id)
            .unwrap()
            .pid
    }

    /// VM 카운터를 정한다: 프로세스 CPU 시간·RSS, 디스크 카운터, 두 tap의 호스트 쪽 (받은, 보낸).
    fn set_vm(f: &Fixture, id: Uuid, cpu_ns: u64, disk: DiskCounters, tap: (u64, u64)) {
        let pid = pid_of(&f.hv, id);
        f.probe.set(|v| {
            v.processes.insert(
                pid,
                ProcStat {
                    cpu_ns,
                    rss_mib: 300,
                },
            );
            v.taps.insert(tap_name(id, 0), tap);
            v.taps.insert(tap_name(id, 1), tap);
        });
        f.hv.set_disk_stats(id, disk);
    }

    fn disk(bytes: u64, ops: u64) -> DiskCounters {
        DiskCounters {
            rd_bytes: bytes,
            wr_bytes: bytes * 2,
            rd_operations: ops,
            wr_operations: ops * 2,
        }
    }

    #[tokio::test]
    async fn node_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 100,
                total: 1_000,
            });
            v.memory = Some((1_000, 4_000));
            v.disk = Some((0, 0));
            v.nic = Some((1_000, 2_000));
            v.fs = Some((10, 100));
        });
        f.sampler.tick().await;
        // 첫 측정은 기준만 잡는다.
        assert!(node_samples(&f.store).is_empty());

        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| {
            v.cpu = Some(CpuTimes {
                busy: 600,
                total: 2_000,
            });
            v.disk = Some((5_000, 10_000));
            v.nic = Some((6_000, 2_000));
        });
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(
            (s.mem_used_mib, s.mem_total_mib),
            (Some(1_000), Some(4_000))
        );
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(1_000.0), Some(2_000.0))
        );
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_000.0), Some(0.0)));
        assert_eq!((s.fs_used_mib, s.fs_total_mib), (Some(10), Some(100)));
        assert!((s.t - now_ms()).abs() < 60_000);

        // NIC 카운터가 줄면(32비트 카운터가 넘침) 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((10, 2_000)));
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::advance(INTERVAL).await;
        f.probe.set(|v| v.nic = Some((5_010, 2_000)));
        f.sampler.tick().await;
        let samples = node_samples(&f.store);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].net_rx_bps, Some(1_000.0));
        // 못 읽은 값은 `null`이다.
        assert_eq!(samples[1].disk_read_bps, Some(0.0));
        f.probe.set(|v| v.disk = None);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(node_samples(&f.store)[2].disk_read_bps, None);
    }

    #[tokio::test]
    async fn vm_rates_start_on_the_second_tick() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(vm_samples(&f.store, id).is_empty());

        tokio::time::advance(INTERVAL).await;
        // vCPU 2개 중 하나가 5초 내내 바빴다.
        set_vm(&f, id, 5_000_000_000, disk(50_000, 50), (1_000, 3_000));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(
            (s.disk_read_bps, s.disk_write_bps),
            (Some(10_000.0), Some(20_000.0))
        );
        assert_eq!(
            (s.disk_read_iops, s.disk_write_iops),
            (Some(10.0), Some(20.0))
        );
        // 게스트 기준: 받기는 tap의 보내기(두 tap 합)다.
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(1_200.0), Some(400.0)));
    }

    /// 사용자 모드 NIC(`user`)는 tap이 없으므로 빼고 tap NIC만 합한다.
    #[tokio::test]
    async fn user_mode_nics_are_left_out_of_the_network_rates() {
        let mut f = fixture().await;
        let id = running_vm_on(&f, &["lan", "user"]).await;
        let only_tap0 = |f: &Fixture| {
            f.probe.set(|v| {
                v.taps.remove(&tap_name(id, 1));
            })
        };
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        only_tap0(&f);
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 0, disk(0, 0), (1_000, 3_000));
        only_tap0(&f);
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!((s.net_rx_bps, s.net_tx_bps), (Some(600.0), Some(200.0)));
    }

    #[tokio::test]
    async fn vm_baseline_is_taken_again_when_the_pid_changes_or_a_counter_drops() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(1_000, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);

        // 다시 켜면 pid가 바뀐다. 카운터가 줄지 않았어도 이번 점은 없다(다른 프로세스의 값이다).
        f.hv.stop(id, StopMode::Force).await.unwrap();
        start(&f, id).await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 10_000_000_000, disk(2_000, 2), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 15_000_000_000, disk(2_500, 7), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].cpu, Some(50.0));
        assert_eq!(samples[1].disk_read_bps, Some(100.0));

        // 카운터가 줄면 이번 점 없이 기준을 다시 잡는다.
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 16_000_000_000, disk(100, 1), (0, 0));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 2);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 17_000_000_000, disk(600, 2), (0, 0));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 3);
        assert_eq!(samples[2].disk_read_bps, Some(100.0));
    }

    #[tokio::test]
    async fn a_disk_stats_failure_nulls_only_the_disk_fields() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        f.hv.fail_disk_stats(id, true);
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (500, 500));
        f.sampler.tick().await;
        let samples = vm_samples(&f.store, id);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.cpu, Some(50.0));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.net_rx_bps, Some(200.0));
        assert_eq!(
            (
                s.disk_read_bps,
                s.disk_write_bps,
                s.disk_read_iops,
                s.disk_write_iops
            ),
            (None, None, None, None)
        );
        // 경고는 10분에 한 번이다.
        let warned = f.sampler.disk_warned[&id];
        assert!(warned.is_some());
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert_eq!(f.sampler.disk_warned[&id], warned);
        tokio::time::advance(WARN_EVERY).await;
        f.sampler.tick().await;
        assert!(f.sampler.disk_warned[&id] > warned);

        // 다시 읽히면 그다음 점부터 디스크 값이 있다.
        f.hv.fail_disk_stats(id, false);
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(6_000, 1), (500, 500));
        f.sampler.tick().await;
        let last = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(last.disk_read_bps, Some(1_000.0));
    }

    /// `list()`가 오래 걸려도(응답 없는 VM) 마감은 그 뒤부터라 다른 VM의 점이 생긴다.
    #[tokio::test]
    async fn a_slow_list_does_not_use_up_the_vm_deadline() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(1_000, 1), (0, 0));
        f.hv.delay_lists(VM_TIMEOUT + Duration::from_secs(1));
        f.hv.delay_disk_stats(Duration::from_millis(10));
        f.sampler.tick().await;
        assert_eq!(vm_samples(&f.store, id).len(), 1);
    }

    /// VM 점의 벽시계 `t`는 `list()`가 끝난 뒤의 시각이다(노드 점은 측정 시작 시각).
    #[tokio::test]
    async fn vm_sample_time_is_taken_after_the_list() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        f.probe.set(|v| v.memory = Some((1, 2)));
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        // 실제 시간으로 기다린다(벽시계는 `tokio::time::pause`로 멈추지 않는다).
        f.hv.delay_lists(Duration::from_millis(200));
        f.sampler.tick().await;
        f.sampler.tick().await;
        let node_t = node_samples(&f.store).pop().unwrap().t;
        let vm_t = vm_samples(&f.store, id).pop().unwrap().t;
        assert!(vm_t - node_t >= 200, "node {node_t}, vm {vm_t}");
    }

    /// 프로세스·tap은 `disk_stats`를 기다리기 전에 읽는다. QMP가 늦게 답해도 CPU 비율은 측정 간격으로,
    /// 디스크 비율은 응답 시각의 간격으로 나눈다.
    #[tokio::test]
    async fn a_slow_disk_stats_answer_does_not_skew_the_rates() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 5_000_000_000, disk(5_900, 5), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(900));
        let started = Instant::now();
        f.sampler.tick().await;
        assert_eq!(
            f.probe.0.lock().unwrap().process_calls.last(),
            Some(&started)
        );
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!(s.cpu, Some(50.0));
        // 디스크 응답은 0초 뒤와 0.9초 뒤라 5.9초 사이의 증가다.
        assert_near(s.disk_read_bps, 1_000.0);
    }

    /// QMP 응답 지연이 측정마다 다르면 디스크 비율은 응답 시각의 차이로 나눈다(CPU는 측정 시작의 차이).
    #[tokio::test]
    async fn disk_rates_use_the_disk_answer_times() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(900));
        f.sampler.tick().await;
        // 다음 측정은 처음 측정을 시작하고 5초 뒤다.
        tokio::time::advance(INTERVAL - Duration::from_millis(900)).await;
        set_vm(&f, id, 5_000_000_000, disk(4_200, 0), (0, 0));
        f.hv.delay_disk_stats(Duration::from_millis(100));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_near(s.cpu, 50.0);
        // 디스크 응답은 0.9초 뒤와 5.1초 뒤다.
        assert_near(s.disk_read_bps, 1_000.0);
    }

    /// `want`와 0.1% 안이다(tokio 타이머가 기다림을 밀리초로 올려 시각이 1ms쯤 밀린다).
    fn assert_near(got: Option<f64>, want: f64) {
        let got = got.unwrap();
        assert!((got - want).abs() < want * 1e-3, "{got} != {want}");
    }

    #[tokio::test]
    async fn counters_of_vanished_vms_are_dropped() {
        let mut f = fixture().await;
        let id = running_vm(&f).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        assert!(f.sampler.vms.contains_key(&id));
        f.hv.stop(id, StopMode::Force).await.unwrap();
        tokio::time::advance(INTERVAL).await;
        f.sampler.tick().await;
        assert!(f.sampler.vms.is_empty());
        assert!(f.sampler.disk_warned.is_empty());
    }

    /// VM이 클러스터에 없으면(스펙을 모름) vCPU·NIC 수를 몰라 CPU와 네트워크가 `null`이다.
    #[tokio::test]
    async fn a_vm_without_a_spec_has_no_cpu_or_network() {
        let mut f = fixture().await;
        let id = Uuid::now_v7();
        start(&f, id).await;
        tokio::time::pause();
        set_vm(&f, id, 0, disk(0, 0), (0, 0));
        f.sampler.tick().await;
        tokio::time::advance(INTERVAL).await;
        set_vm(&f, id, 1_000_000_000, disk(5_000, 0), (10, 10));
        f.sampler.tick().await;
        let s = vm_samples(&f.store, id).pop().unwrap();
        assert_eq!((s.cpu, s.net_rx_bps, s.net_tx_bps), (None, None, None));
        assert_eq!(s.mem_rss_mib, Some(300));
        assert_eq!(s.disk_read_bps, Some(1_000.0));
    }

    #[tokio::test]
    async fn spawned_sampler_samples_every_five_seconds_until_shut_down() {
        let f = fixture().await;
        tokio::time::pause();
        f.probe.set(|v| v.memory = Some((1, 2)));
        let sampler = MetricsSampler::spawn(
            f.hv.clone(),
            f.probe.clone(),
            f.cluster.clone(),
            PathBuf::from("/"),
            f.store.clone(),
        );
        // 첫 측정은 곧바로(기준만), 그다음은 5초 뒤다.
        tokio::time::sleep(INTERVAL / 2).await;
        assert!(node_samples(&f.store).is_empty());
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 1);
        tokio::time::sleep(INTERVAL).await;
        assert_eq!(node_samples(&f.store).len(), 2);
        sampler.shutdown().await;
        tokio::time::sleep(INTERVAL * 2).await;
        assert_eq!(node_samples(&f.store).len(), 2);
    }
}
