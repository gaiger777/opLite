//! 노드 CPU 탐침(계획 63 §6·§7·§12). 드라이버([`Hypervisor::cpu_catalog`])로 이 노드 QEMU의 CPU 카탈로그를 읽어
//! `NodeStatus.cpu`([`NodeCpu`])에 그 해시를 쓰고, 본문은 이름이 해시인 `CpuCatalog` 리소스로 한 번 올린다(같은
//! QEMU·CPU·가속기의 노드는 하나를 같이 쓴다). 등록 전에 한 번 읽고([`CpuWatch::probe_now`]), 그 뒤에는 검사 루프가
//! 회차마다 QEMU 바이너리의 (경로, 크기, mtime)를 보고 바뀌면(apt로 QEMU를 올림) 다시 읽는다([`CpuWatch::poll`]).
//! 실패하면 `NodeCpu.error`만 쓰고 앞 카탈로그를 그대로 둔다.
//!
//! 모든 노드가 기능 `cpu-models`를 알릴 때까지는 읽기만 하고 `NodeCpu`도 카탈로그도 쓰지 않는다([`cpu_models_ready`]).
//! 옛 노드는 카탈로그 종류를 모르고, 옛 리더가 노드 상태를 다시 쓰며 모르는 `cpu`를 지우면 회차마다 다시 쓰게 된다.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use orbivisor_cluster::Cluster;
use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::Rejection;
use orbivisor_driver::{CpuProbe, DriverError, Hypervisor};
use orbivisor_types::cpu::{CpuCatalog, CpuCatalogSpec, NodeCpu};
use orbivisor_types::node::CPU_MODELS;
use orbivisor_types::resource::Resource;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// 실패한 탐침을 다시 하는 간격(바이너리가 바뀌지 않아도)
pub const RETRY_FAILED: Duration = Duration::from_secs(600);

/// QEMU 바이너리의 실제 파일과 크기·mtime. 바뀌면 QEMU를 바꾼 것으로 본다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryStamp {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// `binary`(이름만이면 `PATH`에서 찾는다)의 실제 파일(심볼릭 링크를 따라감). 없으면 None이다.
pub fn stamp(binary: &Path) -> Option<BinaryStamp> {
    let found = if binary.components().count() > 1 {
        binary.to_path_buf()
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(binary))
            .find(|p| p.is_file())?
    };
    let path = std::fs::canonicalize(&found).ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    Some(BinaryStamp {
        path,
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// 탐침 한 번의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probed {
    Ok {
        catalog: Box<CpuCatalogSpec>,
        cpu: NodeCpu,
    },
    Failed {
        error: String,
        accel: String,
        at: u64,
    },
}

impl Probed {
    /// 드라이버의 결과. `accel`은 이 노드의 가속기(`kvm`·`hvf`·`tcg`), `now`는 유닉스 초다.
    pub fn new(result: Result<CpuProbe, DriverError>, accel: &str, now: u64) -> Self {
        match result {
            Ok(probe) => Probed::Ok {
                cpu: NodeCpu {
                    catalog: probe.catalog.hash(),
                    qemu: probe.catalog.qemu.clone(),
                    accel: probe.catalog.accel.clone(),
                    vendor: probe.vendor,
                    model_id: probe.model_id,
                    probed_at: now,
                    error: None,
                },
                catalog: Box::new(probe.catalog),
            },
            Err(e) => Probed::Failed {
                error: e.to_string(),
                accel: accel.to_string(),
                at: now,
            },
        }
    }

    /// 노드 상태에 쓸 값. 실패면 저장된 값(`previous`)의 카탈로그를 두고 `error`만 쓴다(처음이면 카탈로그 없음 =
    /// `host`·`max`만).
    pub fn node_cpu(&self, previous: Option<&NodeCpu>) -> NodeCpu {
        match self {
            Probed::Ok { cpu, .. } => cpu.clone(),
            Probed::Failed { error, accel, at } => match previous {
                Some(p) => NodeCpu {
                    error: Some(error.clone()),
                    ..p.clone()
                },
                None => NodeCpu {
                    accel: accel.clone(),
                    probed_at: *at,
                    error: Some(error.clone()),
                    ..NodeCpu::default()
                },
            },
        }
    }
}

/// 이 노드 밖의 모든 노드가 기능 `cpu-models`를 알린다(이 노드는 이 빌드라 안다). 읽지 못하면 false다.
pub fn cpu_models_ready(cluster: &Cluster, me: &str) -> bool {
    match orbivisor_api::cluster::missing_feature(cluster, CPU_MODELS) {
        Ok(old) => old.iter().all(|n| n == me),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the node features");
            false
        }
    }
}

/// 카탈로그 리소스(이름 = 해시)가 없으면 만든다. 다른 노드가 먼저 만들었으면(`name_taken`) 된 것이다. 카탈로그가
/// 클러스터에 있으면 true다. 실패하면(false) 부르는 쪽은 `NodeStatus.cpu`를 새 해시로 옮기지 않고 앞 것을 두며, 다음
/// 검사 회차가 다시 한다(리뷰 2 #2 — 없는 카탈로그의 해시를 알리지 않게).
pub async fn publish(cluster: &Cluster, hash: &str, catalog: &CpuCatalogSpec) -> bool {
    match cluster.get_by_name::<CpuCatalogSpec>(hash) {
        Ok(Some(_)) => return true,
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the CPU catalogs");
            return false;
        }
    }
    let resource: CpuCatalog = Resource::new(hash, catalog.clone());
    match cluster.create(&resource).await {
        Ok(_) => {
            tracing::info!(
                catalog = %hash,
                models = catalog.models.len(),
                "published this node's CPU catalog"
            );
            true
        }
        Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => true,
        Err(e) => {
            tracing::warn!(error = %e, "cannot publish this node's CPU catalog; will retry");
            false
        }
    }
}

/// 관리자 다시 읽기 요청(Task `node-cpu-probe`): 요청 뒤에 띄운 탐침의 결과를 받는다.
type Reply = oneshot::Sender<Probed>;

/// 탐침의 상태: 마지막 결과와, 그때의 바이너리, 도는 탐침
pub struct CpuWatch {
    hv: Arc<dyn Hypervisor>,
    binary: PathBuf,
    accel: String,
    last: Option<Probed>,
    /// 마지막 탐침을 띄울 때의 바이너리(한 번도 띄우지 않았으면 None)
    stamp: Option<Option<BinaryStamp>>,
    /// 마지막 탐침을 띄운 때
    started: Option<Instant>,
    running: Option<JoinHandle<Result<CpuProbe, DriverError>>>,
    /// 다시 읽기 요청(검사 회차가 거둔다)
    requests: mpsc::UnboundedReceiver<Reply>,
    sender: mpsc::UnboundedSender<Reply>,
    /// 아직 띄우지 않은 요청
    queued: Vec<Reply>,
    /// 도는 탐침을 기다리는 요청
    waiting: Vec<Reply>,
}

/// 다시 읽기를 검사 루프의 탐침([`CpuWatch`])에 맡긴다. 검사 루프가 다음 회차에 같은 결과를 쓰므로 앞 결과로 되돌아가지
/// 않는다. 결과를 받으면 카탈로그를 올리고 `NodeStatus.cpu`를 바로 쓴다(검사 회차를 기다리지 않게).
pub struct CpuRefresh {
    cluster: Arc<Cluster>,
    name: String,
    sender: mpsc::UnboundedSender<Reply>,
}

/// 탐침이 돌고 끝난 결과를 검사 회차가 거둘 때까지 기다리는 한도. 요청이 도는 탐침 중에 오면 그 탐침(30초까지)이
/// 끝나고 다음 회차에 새 탐침(30초까지)을 띄운 뒤 그다음 회차가 결과를 거둔다 — 탐침 둘 + 회차 셋(리뷰 1 M3).
const REFRESH_TIMEOUT: Duration = Duration::from_secs(
    2 * orbivisor_qemu::cpu::PROBE_TIMEOUT.as_secs() + 3 * crate::checks::CHECK_INTERVAL.as_secs(),
);

#[async_trait::async_trait]
impl orbivisor_api::CpuRefresher for CpuRefresh {
    async fn refresh(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(tx)
            .map_err(|_| "the node health checker is not running".to_string())?;
        let probed = tokio::time::timeout(REFRESH_TIMEOUT, rx)
            .await
            .map_err(|_| "timed out waiting for the CPU probe".to_string())?
            .map_err(|_| "the node health checker stopped".to_string())?;
        if !cpu_models_ready(&self.cluster, &self.name) {
            return Err("not every node knows the cpu-models feature yet".into());
        }
        if let Probed::Ok { catalog, cpu } = &probed
            && !publish(&self.cluster, &cpu.catalog, catalog).await
        {
            return Err("cannot publish the CPU catalog; the node keeps its previous one".into());
        }
        crate::bootstrap::update_node_status(&self.cluster, |status| {
            let cpu = probed.node_cpu(status.cpu.as_ref());
            if status.cpu.as_ref() == Some(&cpu) {
                return false;
            }
            status.cpu = Some(cpu);
            true
        })
        .await
        .map_err(|e| format!("cannot record the CPU probe: {e:#}"))?;
        match probed {
            Probed::Ok { .. } => Ok(()),
            Probed::Failed { error, .. } => Err(error),
        }
    }
}

impl CpuWatch {
    /// `binary`는 드라이버가 띄우는 QEMU(`HostProfile::qemu_binary`), `accel`은 그 가속기다.
    pub fn new(hv: Arc<dyn Hypervisor>, binary: PathBuf, accel: &str) -> Self {
        let (sender, requests) = mpsc::unbounded_channel();
        Self {
            hv,
            binary,
            accel: accel.to_string(),
            last: None,
            stamp: None,
            started: None,
            running: None,
            requests,
            sender,
            queued: Vec::new(),
            waiting: Vec::new(),
        }
    }

    /// 관리자 다시 읽기(`node-cpu-probe` Task 실행기에 넘긴다). `name`은 이 노드 이름이다.
    pub fn refresher(&self, cluster: Arc<Cluster>, name: &str) -> CpuRefresh {
        CpuRefresh {
            cluster,
            name: name.to_string(),
            sender: self.sender.clone(),
        }
    }

    /// 마지막 결과(아직 읽지 않았으면 None)
    pub fn last(&self) -> Option<&Probed> {
        self.last.as_ref()
    }

    /// 지금 읽고 기다린다(등록 전).
    pub async fn probe_now(&mut self) -> &Probed {
        self.start();
        if let Some(task) = self.running.take() {
            self.finish(task.await);
        }
        self.last
            .as_ref()
            .expect("a finished probe leaves a result")
    }

    /// 끝난 탐침을 거두고, 바이너리가 바뀌었거나(처음 포함) 실패한 뒤 [`RETRY_FAILED`]가 지났으면 새로 띄운다. 띄운
    /// 탐침을 기다리지 않는다(검사 회차를 늦추지 않게).
    pub async fn poll(&mut self) {
        if self.running.as_ref().is_some_and(|t| t.is_finished())
            && let Some(task) = self.running.take()
        {
            self.finish(task.await);
        }
        while let Ok(reply) = self.requests.try_recv() {
            self.queued.push(reply);
        }
        if self.running.is_some() {
            return;
        }
        let changed = self.stamp.as_ref() != Some(&stamp(&self.binary));
        let retry = matches!(self.last, Some(Probed::Failed { .. }))
            && self.started.is_some_and(|at| at.elapsed() >= RETRY_FAILED);
        if !self.queued.is_empty() {
            tracing::info!("reading the CPU models again (requested)");
            self.start();
        } else if changed || retry {
            if changed && self.stamp.is_some() {
                tracing::info!(binary = ?self.binary, "the QEMU binary changed; reading the CPU models again");
            }
            self.start();
        }
    }

    fn start(&mut self) {
        self.waiting.append(&mut self.queued);
        self.stamp = Some(stamp(&self.binary));
        self.started = Some(Instant::now());
        let hv = self.hv.clone();
        self.running = Some(tokio::spawn(async move { hv.cpu_catalog().await }));
    }

    fn finish(&mut self, joined: Result<Result<CpuProbe, DriverError>, tokio::task::JoinError>) {
        let result = joined.unwrap_or_else(|e| Err(DriverError::CpuProbe(e.to_string())));
        let probed = Probed::new(result, &self.accel, unix_now());
        match &probed {
            Probed::Ok { catalog, cpu } => tracing::info!(
                catalog = %cpu.catalog,
                qemu = %cpu.qemu,
                accel = %cpu.accel,
                models = catalog.models.len(),
                flags = catalog.flags.len(),
                "read this node's CPU models"
            ),
            Probed::Failed { error, .. } => {
                tracing::warn!(%error, "cannot read this node's CPU models; keeping the last catalog")
            }
        }
        for reply in self.waiting.drain(..) {
            let _ = reply.send(probed.clone());
        }
        self.last = Some(probed);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use orbivisor_driver::{MockHypervisor, mock_cpu_probe};

    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    /// 리뷰 1 M3: 도는 탐침을 기다린 뒤 새 탐침까지 끝나도 다시 읽기 Task가 먼저 시간을 넘기지 않는다.
    #[test]
    fn a_refresh_outlasts_a_running_probe_and_a_new_one() {
        let worst = 2 * orbivisor_qemu::cpu::PROBE_TIMEOUT + 2 * crate::checks::CHECK_INTERVAL;
        assert!(REFRESH_TIMEOUT > worst, "{REFRESH_TIMEOUT:?} <= {worst:?}");
    }

    #[test]
    fn stamps_follow_the_file_and_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("qemu-system-x86_64");
        assert_eq!(stamp(&bin), None);
        write(&bin, "a");
        let first = stamp(&bin).unwrap();
        assert_eq!(first.size, 1);
        write(&bin, "abc");
        assert_ne!(stamp(&bin), Some(first));
        // 심볼릭 링크는 따라간다(apt·brew는 링크가 가리키는 파일을 바꾼다).
        let link = dir.path().join("qemu");
        std::os::unix::fs::symlink(&bin, &link).unwrap();
        assert_eq!(stamp(&link), stamp(&bin));
        // 이름만이면 PATH에서 찾는다(sh는 어디에나 있다).
        assert!(stamp(Path::new("sh")).is_some());
        assert_eq!(stamp(Path::new("no-such-qemu-binary")), None);
    }

    #[test]
    fn a_failure_keeps_the_last_catalog() {
        let ok = Probed::new(Ok(mock_cpu_probe()), "kvm", 100);
        let cpu = ok.node_cpu(None);
        assert_eq!(cpu.catalog, mock_cpu_probe().catalog.hash());
        assert_eq!(
            (cpu.qemu.as_str(), cpu.accel.as_str(), cpu.probed_at),
            (mock_cpu_probe().catalog.qemu.as_str(), "kvm", 100)
        );
        assert_eq!(cpu.vendor.as_deref(), Some("GenuineIntel"));
        assert_eq!(cpu.error, None);

        let failed = Probed::new(Err(DriverError::CpuProbe("boom".into())), "kvm", 200);
        let kept = failed.node_cpu(Some(&cpu));
        assert_eq!(
            kept,
            NodeCpu {
                error: Some("CPU probe failed: boom".into()),
                ..cpu.clone()
            }
        );
        // 다시 써도 같다(회차마다 쓰지 않는다).
        assert_eq!(failed.node_cpu(Some(&kept)), kept);
        let first = failed.node_cpu(None);
        assert_eq!(
            (
                first.catalog.as_str(),
                first.accel.as_str(),
                first.probed_at
            ),
            ("", "kvm", 200)
        );
        assert!(first.error.is_some());
    }

    /// 바이너리가 바뀔 때만 다시 읽고, 탐침을 기다리지 않는다.
    #[tokio::test]
    async fn the_watch_probes_again_when_the_binary_changes() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("qemu");
        write(&bin, "v1");
        let hv = Arc::new(MockHypervisor::default());
        let mut watch = CpuWatch::new(hv.clone(), bin.clone(), "kvm");
        assert!(matches!(watch.probe_now().await, Probed::Ok { .. }));
        assert_eq!(hv.cpu_probes(), 1);
        for _ in 0..3 {
            watch.poll().await;
        }
        assert_eq!(hv.cpu_probes(), 1);

        // QEMU를 올림: 다른 카탈로그
        let mut newer = mock_cpu_probe();
        newer.catalog.qemu = "9.0.0".into();
        hv.set_cpu_catalog(Ok(newer.clone()));
        write(&bin, "v2 longer");
        watch.poll().await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(watch.last(), Some(Probed::Ok { cpu, .. }) if cpu.qemu == "9.0.0") {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
            watch.poll().await;
        }
        assert_eq!(hv.cpu_probes(), 2);
        assert_eq!(
            watch.last().unwrap().node_cpu(None).catalog,
            newer.catalog.hash()
        );
    }

    /// 다시 읽기 요청은 바이너리가 그대로여도 새 탐침을 띄우고, 요청 뒤에 띄운 탐침의 결과를 받는다. 마지막 결과도 그것이라
    /// 검사 루프가 앞 결과로 되돌리지 않는다.
    #[tokio::test]
    async fn a_request_probes_again_and_gets_the_new_result() {
        let hv = Arc::new(MockHypervisor::default());
        let mut watch = CpuWatch::new(hv.clone(), "/nonexistent/qemu".into(), "kvm");
        watch.probe_now().await;
        watch.poll().await;
        assert_eq!(hv.cpu_probes(), 1);
        let mut newer = mock_cpu_probe();
        newer.catalog.qemu = "9.0.0".into();
        hv.set_cpu_catalog(Ok(newer.clone()));
        let (tx, mut rx) = oneshot::channel();
        watch.sender.send(tx).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let probed = loop {
            watch.poll().await;
            if let Ok(p) = rx.try_recv() {
                break p;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(hv.cpu_probes(), 2);
        assert!(matches!(&probed, Probed::Ok { cpu, .. } if cpu.qemu == "9.0.0"));
        assert_eq!(watch.last(), Some(&probed));
        // 실패도 그대로 받는다.
        hv.set_cpu_catalog(Err("boom".into()));
        let (tx, mut rx) = oneshot::channel();
        watch.sender.send(tx).unwrap();
        let failed = loop {
            watch.poll().await;
            if let Ok(p) = rx.try_recv() {
                break p;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(matches!(failed, Probed::Failed { .. }));
    }

    /// 실패는 바이너리가 그대로여도 [`RETRY_FAILED`] 뒤에 다시 읽는다.
    #[tokio::test(start_paused = true)]
    async fn a_failed_probe_is_retried_later() {
        let hv = Arc::new(MockHypervisor::default());
        hv.set_cpu_catalog(Err("no kvm".into()));
        let mut watch = CpuWatch::new(hv.clone(), "/nonexistent/qemu".into(), "kvm");
        assert!(matches!(watch.probe_now().await, Probed::Failed { .. }));
        watch.poll().await;
        assert_eq!(hv.cpu_probes(), 1);
        hv.set_cpu_catalog(Ok(mock_cpu_probe()));
        tokio::time::advance(RETRY_FAILED).await;
        watch.poll().await;
        while hv.cpu_probes() < 2 || watch.running.is_some() {
            tokio::task::yield_now().await;
            watch.poll().await;
        }
        assert!(matches!(watch.last(), Some(Probed::Ok { .. })));
    }
}
