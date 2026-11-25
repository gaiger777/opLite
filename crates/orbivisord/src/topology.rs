//! 노드 NUMA 토폴로지 탐침과 호스트 자원 설정 적용(계획 64 §6·§7). `/sys`·`/proc`에서 소켓·코어·스레드·NUMA 노드·노드
//! 메모리·hugepages·거리·L3 묶음·`isolcpus`를 읽어 `NodeStatus.topology`([`NodeTopology`])에 쓴다. 노드 설정
//! `host_resources`가 있으면 실행 중 hugepages 예약(`nr_hugepages` 쓰기)과 opld 자신의 CPU 친화(예약 CPU)를 건다.
//!
//! 등록 전에 한 번 읽고([`TopologyWatch::refresh`]), 그 뒤에는 검사 루프가 [`PROBE_EVERY`] 회차(30초)마다, 또는 노드
//! 설정이 바뀐 회차에 다시 읽는다([`TopologyWatch::poll`]). 값이 바뀔 때만 `probed_at`을 새로 하므로 노드 상태도 바뀔
//! 때만 쓰인다. Linux에서만 띄운다(macOS·위트니스는 토폴로지가 없다).
//!
//! 모든 노드가 기능 `numa`를 알릴 때까지는 읽기만 하고 노드 상태에 쓰지 않는다([`numa_ready`], 계획 63과 같은 까닭 —
//! 옛 리더가 모르는 `topology`를 지우고 다시 쓰기를 되풀이하지 않게). 설정 적용은 기다리지 않는다(설정은 기능이 켜진
//! 뒤에만 생긴다).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbivisor_cluster::Cluster;
use orbivisor_types::node::NUMA;
use orbivisor_types::numa::{
    CpuSet, HostNode, HostResources, HugepagePool, HugepageReserve, LiveNode, LivePool,
    NodeTopology, TopologyLive,
};

/// 검사 회차(5초) 몇 번마다 `/sys`를 다시 읽는지(30초)
pub const PROBE_EVERY: u32 = 6;

/// 이 노드 밖의 모든 노드가 기능 `numa`를 알린다(이 노드는 이 빌드라 안다). 읽지 못하면 false다.
pub fn numa_ready(cluster: &Cluster, me: &str) -> bool {
    match orbivisor_api::cluster::missing_feature(cluster, NUMA) {
        Ok(old) => old.iter().all(|n| n == me),
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the node features");
            false
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// /sys 읽기
// ---------------------------------------------------------------------------------------------------------------

fn cpu_dir(sys: &Path) -> PathBuf {
    sys.join("devices/system/cpu")
}

fn node_dir(sys: &Path) -> PathBuf {
    sys.join("devices/system/node")
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// 꼭 있어야 하는 cpulist 파일
fn cpus(path: &Path) -> Result<CpuSet, String> {
    read(path)?
        .trim()
        .parse()
        .map_err(|_| format!("{} is not a CPU list", path.display()))
}

/// 없어도 되는 cpulist 파일(`isolated`·`nohz_full`). 없거나 읽을 수 없으면(옛 커널의 `(null)`) 비었다고 본다.
fn cpus_or_empty(path: &Path) -> CpuSet {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or_default()
}

fn number(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// 디렉터리의 `<prefix><n>` 항목 번호(오름차순)
fn numbered(dir: &Path, prefix: &str) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<u32> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.strip_prefix(prefix)?.parse().ok())
        .collect();
    found.sort_unstable();
    found
}

/// `hugepages/hugepages-<kB>kB/nr_hugepages` 목록(크기 순). 수를 읽지 못한 크기는 0이다.
fn pools(dir: &Path) -> Vec<HugepagePool> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<HugepagePool> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let kib = name
                .to_str()?
                .strip_prefix("hugepages-")?
                .strip_suffix("kB")?;
            let size_kib = kib.parse().ok()?;
            let total = number(&e.path().join("nr_hugepages"))
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0);
            Some(HugepagePool { size_kib, total })
        })
        .collect();
    found.sort_by_key(|p| p.size_kib);
    found
}

/// `meminfo` 꼴 글에서 `key`(예: `MemTotal:`)의 KiB 값. 노드 파일의 `Node 0 ` 머리는 건너뛴다.
fn meminfo_kib(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let rest = &line[line.find(key)? + key.len()..];
        rest.split_whitespace().next()?.parse().ok()
    })
}

/// `/sys`·`/proc`(뿌리 `sys`·`proc`)에서 이 노드의 토폴로지를 읽는다. 온라인 CPU만 센다. `node` 디렉터리가 없으면
/// (`CONFIG_NUMA`가 없는 커널) 모든 CPU와 `MemTotal`을 가진 노드 0 하나로 본다. `pinning`은 늘 참이고(Linux에서만
/// 부른다), `cpuset`은 cgroup v2에 `cpuset` 컨트롤러가 있는지다(VM 스코프 설정은 부르는 쪽이 더한다). `warnings`·
/// `probed_at`은 비워 둔다.
pub fn probe(sys: &Path, proc: &Path) -> Result<NodeTopology, String> {
    let cpu = cpu_dir(sys);
    let online = cpus(&cpu.join("online"))?;
    if online.is_empty() {
        return Err(format!("{} lists no CPUs", cpu.join("online").display()));
    }
    let mut packages = BTreeSet::new();
    let mut siblings: BTreeSet<CpuSet> = BTreeSet::new();
    let mut l3: BTreeSet<CpuSet> = BTreeSet::new();
    for id in online.iter() {
        let dir = cpu.join(format!("cpu{id}"));
        // 패키지 번호가 없거나 -1이면(일부 가상 기계) 소켓 0이다.
        packages.insert(number(&dir.join("topology/physical_package_id")).map_or(0, |p| p.max(0)));
        let group = std::fs::read_to_string(dir.join("topology/thread_siblings_list"))
            .ok()
            .and_then(|t| t.trim().parse::<CpuSet>().ok())
            .map(|s| s.intersection(&online))
            .filter(|s| s.contains(id))
            .unwrap_or_else(|| CpuSet::from_iter([id]));
        siblings.insert(group);
        for index in numbered(&dir.join("cache"), "index") {
            let cache = dir.join(format!("cache/index{index}"));
            let unified =
                std::fs::read_to_string(cache.join("type")).is_ok_and(|t| t.trim() == "Unified");
            if unified && number(&cache.join("level")) == Some(3) {
                let shared = cpus_or_empty(&cache.join("shared_cpu_list")).intersection(&online);
                if !shared.is_empty() {
                    l3.insert(shared);
                }
            }
        }
    }
    let threads_per_core = siblings.iter().map(CpuSet::len).max().unwrap_or(1);
    // 커널의 형제 목록이 어긋나도(겹침) 앞 묶음에 든 CPU는 다시 세지 않는다.
    let mut taken = CpuSet::new();
    let mut groups: Vec<CpuSet> = Vec::new();
    for group in siblings {
        let group = group.difference(&taken);
        taken = taken.union(&group);
        if group.len() >= 2 {
            groups.push(group);
        }
    }
    groups.sort_by_key(CpuSet::first);
    let mut l3: Vec<CpuSet> = l3.into_iter().collect();
    l3.sort_by_key(CpuSet::first);

    let (nodes, distances) = host_nodes(sys, proc, &online)?;
    let mut page_sizes_kib: Vec<u32> = pools(&sys.join("kernel/mm/hugepages"))
        .iter()
        .map(|p| p.size_kib)
        .collect();
    if page_sizes_kib.is_empty() {
        // `/sys/kernel/mm/hugepages`가 없으면 기본 크기 하나(`Hugepagesize`)
        if let Some(kib) = std::fs::read_to_string(proc.join("meminfo"))
            .ok()
            .and_then(|t| meminfo_kib(&t, "Hugepagesize:"))
            .and_then(|k| u32::try_from(k).ok())
        {
            page_sizes_kib.push(kib);
        }
    }
    let cpuset = std::fs::read_to_string(sys.join("fs/cgroup/cgroup.controllers"))
        .is_ok_and(|t| t.split_whitespace().any(|c| c == "cpuset"));
    Ok(NodeTopology {
        sockets: u16::try_from(packages.len()).unwrap_or(u16::MAX).max(1),
        threads_per_core: u8::try_from(threads_per_core).unwrap_or(u8::MAX),
        cpus: online.clone(),
        nodes,
        distances,
        siblings: groups,
        l3,
        page_sizes_kib,
        isolated: cpus_or_empty(&cpu.join("isolated")).intersection(&online),
        nohz_full: cpus_or_empty(&cpu.join("nohz_full")).intersection(&online),
        pinning: true,
        cpuset,
        warnings: Vec::new(),
        error: None,
        probed_at: 0,
    })
}

/// 호스트 NUMA 노드와 거리. `node` 디렉터리가 없으면 노드 0 하나(거리 없음).
fn host_nodes(
    sys: &Path,
    proc: &Path,
    online: &CpuSet,
) -> Result<(Vec<HostNode>, Vec<Vec<u8>>), String> {
    let dir = node_dir(sys);
    let ids = numbered(&dir, "node");
    if ids.is_empty() {
        let meminfo = read(&proc.join("meminfo"))?;
        let kib = meminfo_kib(&meminfo, "MemTotal:")
            .ok_or_else(|| format!("{} has no MemTotal", proc.join("meminfo").display()))?;
        let node = HostNode {
            id: 0,
            cpus: online.clone(),
            memory_mib: kib / 1024,
            hugepages: present(pools(&sys.join("kernel/mm/hugepages"))),
        };
        return Ok((vec![node], Vec::new()));
    }
    let mut nodes = Vec::with_capacity(ids.len());
    let mut distances = Vec::with_capacity(ids.len());
    for id in ids {
        let node = dir.join(format!("node{id}"));
        let id = u16::try_from(id).map_err(|_| format!("NUMA node {id} is out of range"))?;
        let meminfo = read(&node.join("meminfo"))?;
        let kib = meminfo_kib(&meminfo, "MemTotal:")
            .ok_or_else(|| format!("{} has no MemTotal", node.join("meminfo").display()))?;
        nodes.push(HostNode {
            id,
            cpus: cpus_or_empty(&node.join("cpulist")).intersection(online),
            memory_mib: kib / 1024,
            hugepages: present(pools(&node.join("hugepages"))),
        });
        let row: Option<Vec<u8>> = std::fs::read_to_string(node.join("distance"))
            .ok()
            .and_then(|t| t.split_whitespace().map(|d| d.parse().ok()).collect());
        distances.push(row.unwrap_or_default());
    }
    // 줄 하나라도 어긋나면(노드 수와 다름) 거리를 모르는 것으로 둔다.
    if distances.iter().any(|row| row.len() != nodes.len()) {
        distances.clear();
    }
    Ok((nodes, distances))
}

/// `hugepages-<kB>kB/{free,surplus}_hugepages` 목록(크기 순, 계획 64 `ReadTopology`). 예약도 더 받은 것도 없는 크기는
/// 뺀다.
fn live_pools(dir: &Path) -> Vec<LivePool> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let count = |path: PathBuf| {
        number(&path)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0)
    };
    let mut found: Vec<LivePool> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            let size_kib = name
                .to_str()?
                .strip_prefix("hugepages-")?
                .strip_suffix("kB")?
                .parse()
                .ok()?;
            // 예약도 더 받은 것도 없는 크기는 뺀다(노드 상태의 풀처럼).
            let total = count(e.path().join("nr_hugepages"));
            let surplus = count(e.path().join("surplus_hugepages"));
            (total > 0 || surplus > 0).then(|| LivePool {
                size_kib,
                free: count(e.path().join("free_hugepages")),
                surplus,
            })
        })
        .collect();
    found.sort_by_key(|p| p.size_kib);
    found
}

/// 이 노드 토폴로지의 지금 값(계획 64 `ReadTopology`): 호스트 노드마다 빈 메모리(`MemFree`)와 hugepages의 빈·더 받은
/// 수, 그리고 orbivisord(`proc/self`)의 친화가 예약 CPU(`reserved`, 온라인과 교집합 — 비면 온라인 전체)와 맞는지.
/// `node` 디렉터리가 없으면 노드 0 하나(`/proc/meminfo`, `/sys/kernel/mm/hugepages`)다.
pub fn live(sys: &Path, proc: &Path, reserved: &CpuSet) -> Result<TopologyLive, String> {
    let online = cpus(&cpu_dir(sys).join("online"))?;
    let dir = node_dir(sys);
    let ids = numbered(&dir, "node");
    let mut nodes = Vec::new();
    if ids.is_empty() {
        let meminfo = read(&proc.join("meminfo"))?;
        nodes.push(LiveNode {
            id: 0,
            memory_free_mib: meminfo_kib(&meminfo, "MemFree:").unwrap_or(0) / 1024,
            hugepages: live_pools(&sys.join("kernel/mm/hugepages")),
        });
    }
    for id in ids {
        let node = dir.join(format!("node{id}"));
        let id = u16::try_from(id).map_err(|_| format!("NUMA node {id} is out of range"))?;
        let meminfo = read(&node.join("meminfo"))?;
        nodes.push(LiveNode {
            id,
            memory_free_mib: meminfo_kib(&meminfo, "MemFree:").unwrap_or(0) / 1024,
            hugepages: live_pools(&node.join("hugepages")),
        });
    }
    let reserved = reserved.intersection(&online);
    // 예약이 없으면 뜰 때의 친화(관리자가 좁힌 그대로, 리뷰 2 N3)
    let want = if reserved.is_empty() {
        orbivisor_driver::affinity::start_cpus()
            .map(|c| c.intersection(&online))
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| online.clone())
    } else {
        reserved
    };
    let affinity_ok = std::fs::read_to_string(proc.join("self/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?
                .trim()
                .parse::<CpuSet>()
                .ok()
        })
        .is_some_and(|allowed| allowed.intersection(&online) == want);
    Ok(TopologyLive { nodes, affinity_ok })
}

/// 다른 노드와 이 노드의 API가 묻는 지금 값([`live`])을 답한다(Linux에서만 넣는다). 예약 CPU는 저장된 노드 설정이다.
pub struct LiveSource {
    pub cluster: std::sync::Arc<Cluster>,
    pub node: uuid::Uuid,
    pub sys: PathBuf,
    pub proc: PathBuf,
}

#[async_trait::async_trait]
impl orbivisor_peer::TopologySource for LiveSource {
    async fn live(&self) -> Result<TopologyLive, String> {
        let reserved = self
            .cluster
            .get::<orbivisor_types::node::NodeSpec>(self.node)
            .map_err(|e| e.to_string())?
            .and_then(|n| n.spec.host_resources)
            .map(|r| r.reserved_cpus)
            .unwrap_or_default();
        let (sys, proc) = (self.sys.clone(), self.proc.clone());
        tokio::task::spawn_blocking(move || live(&sys, &proc, &reserved))
            .await
            .map_err(|e| format!("reading the topology panicked: {e}"))?
    }
}

/// 예약된 풀만(노드 상태를 작게)
fn present(pools: Vec<HugepagePool>) -> Vec<HugepagePool> {
    pools.into_iter().filter(|p| p.total > 0).collect()
}

/// PID 1의 `Cpus_allowed_list`(호스트 프로세스가 돌 수 있는 CPU). 읽지 못하면 None이다.
pub fn pid1_cpus(proc: &Path) -> Option<CpuSet> {
    let status = std::fs::read_to_string(proc.join("1/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?
        .trim()
        .parse()
        .ok()
}

// ---------------------------------------------------------------------------------------------------------------
// 경고
// ---------------------------------------------------------------------------------------------------------------

/// 호스트 준비가 덜 된 것(§6, 화면에 그대로): 오프라인인 예약·전용 CPU, 호스트 프로세스가 돌 수 있는 전용 CPU(`isolated`가
/// 아니고 PID 1의 친화에 듦), SMT 형제를 나누는 전용 집합, 모자란 hugepages 예약.
pub fn warnings(
    topology: &NodeTopology,
    resources: Option<&HostResources>,
    pid1: Option<&CpuSet>,
) -> Vec<String> {
    let Some(r) = resources else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, set) in [
        ("reserved", &r.reserved_cpus),
        ("dedicated", &r.dedicated_cpus),
    ] {
        let off = set.difference(&topology.cpus);
        if !off.is_empty() {
            out.push(format!("{name} CPUs {off} are offline"));
        }
    }
    if topology
        .cpus
        .difference(&r.reserved_cpus)
        .difference(&r.dedicated_cpus)
        .is_empty()
    {
        out.push(
            "no shared CPU is online (reserved and dedicated CPUs cover every online CPU); shared VMs and emulator threads run on all online CPUs".into(),
        );
    }
    let dedicated = r.dedicated_cpus.intersection(&topology.cpus);
    if let Some(pid1) = pid1 {
        let exposed = dedicated.difference(&topology.isolated).intersection(pid1);
        if !exposed.is_empty() {
            out.push(format!(
                "host processes may run on the dedicated CPUs {exposed}; see the host preparation guide"
            ));
        }
    }
    let split: Vec<String> = topology
        .siblings
        .iter()
        .filter(|g| !g.is_disjoint(&dedicated) && !g.is_subset(&dedicated))
        .map(CpuSet::to_string)
        .collect();
    if !split.is_empty() {
        out.push(format!(
            "the dedicated CPUs split SMT sibling groups {}",
            split.join(" ")
        ));
    }
    for want in &r.hugepages {
        let got: u64 = match want.host_node {
            Some(n) => topology
                .nodes
                .iter()
                .filter(|h| h.id == n)
                .map(|h| h.pages(want.size))
                .sum(),
            None => topology.pages(want.size),
        };
        if got < u64::from(want.count) {
            let at = want
                .host_node
                .map(|n| format!("node {n}: "))
                .unwrap_or_default();
            out.push(format!(
                "{at}{} hugepages {} wanted, {got} reserved",
                want.size, want.count
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// 설정 적용
// ---------------------------------------------------------------------------------------------------------------

/// 예약 하나의 `nr_hugepages` 파일: 호스트 노드를 정하면 그 노드의 것, 아니면 커널 전체의 것
pub fn nr_hugepages(sys: &Path, reserve: &HugepageReserve) -> PathBuf {
    let pool = format!("hugepages-{}kB/nr_hugepages", reserve.size.size_kib());
    match reserve.host_node {
        Some(n) => node_dir(sys).join(format!("node{n}/hugepages/{pool}")),
        None => sys.join("kernel/mm/hugepages").join(pool),
    }
}

/// 실행 중 hugepages 예약(§7): 예약마다 지금 수가 다르면 `nr_hugepages`에 쓴다. 커널이 모자라게 잡으면 오류가 아니다
/// (다음 탐침이 얻은 수를 읽고 경고한다). 쓰지 못한 것의 까닭을 돌려준다.
pub fn reserve_hugepages(sys: &Path, wanted: &[HugepageReserve]) -> Vec<String> {
    let mut errors = Vec::new();
    for r in wanted {
        let path = nr_hugepages(sys, r);
        let what = match r.host_node {
            Some(n) => format!("{} hugepages on node {n}", r.size),
            None => format!("{} hugepages", r.size),
        };
        if number(&path) == Some(i64::from(r.count)) {
            continue;
        }
        match std::fs::write(&path, format!("{}\n", r.count)) {
            Ok(()) => tracing::info!(pages = r.count, "reserved {what}"),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "cannot reserve {what}");
                errors.push(format!("cannot reserve {} {what}: {e}", r.count));
            }
        }
    }
    errors
}

#[cfg(target_os = "linux")]
pub use orbivisor_driver::affinity::pin_tasks;

/// opld 자신(모든 스레드)의 친화를 `cpus`로 둔다. Linux가 아니면 하지 않는다.
fn pin_self(cpus: &CpuSet) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let n = pin_tasks(Path::new("/proc/self/task"), cpus)?;
        tracing::info!(cpus = %cpus, threads = n, "set the CPU affinity of orbivisord");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cpus;
    Ok(())
}

/// 두 토폴로지가 `probed_at`만 다르다.
pub fn same(a: &NodeTopology, b: &NodeTopology) -> bool {
    NodeTopology {
        probed_at: b.probed_at,
        ..a.clone()
    } == *b
}

/// 쓸 값: 저장된 것과 `probed_at`만 다르면 저장된 것(시각을 바꾸지 않아 다시 쓰지 않는다)
pub fn keep_time(stored: Option<&NodeTopology>, new: &NodeTopology) -> NodeTopology {
    match stored {
        Some(s) if same(s, new) => s.clone(),
        _ => new.clone(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 검사 루프의 탐침
// ---------------------------------------------------------------------------------------------------------------

/// 탐침의 상태: 마지막 결과, 적용한 설정, opld에 건 친화
pub struct TopologyWatch {
    sys: PathBuf,
    proc: PathBuf,
    /// `[hypervisor] systemd_scope`(꺼져 있으면 `AllowedCPUs`를 걸 스코프가 없다)
    systemd_scope: bool,
    /// 마지막으로 적용한 노드 설정(한 번도 적용하지 않았으면 None)
    applied: Option<Option<HostResources>>,
    /// 마지막 hugepages 예약에서 쓰지 못한 것(다음 적용까지 경고로 남긴다)
    reserve_errors: Vec<String>,
    /// opld에 건 친화(예약 CPU). 건 적이 없으면 None이다.
    pinned: Option<CpuSet>,
    /// 친화를 걸지 못한 까닭(경고)
    pin_error: Option<String>,
    last: Option<NodeTopology>,
    /// 마지막으로 읽은 뒤 지난 회차
    rounds: u32,
    /// `systemctl --version`의 판(읽지 않았으면 None, 읽지 못했으면 Some(None)). `AllowedCPUs=`는 systemd 244부터다.
    systemd: Option<Option<u32>>,
}

/// 스코프에 `AllowedCPUs=`를 걸 수 있는 가장 낮은 systemd 판
const SYSTEMD_ALLOWED_CPUS: u32 = 244;

/// `systemctl --version` 첫 줄의 판(`systemd 255 (255.4-1ubuntu8.18)` → 255)
pub fn systemd_version(text: &str) -> Option<u32> {
    text.lines()
        .next()?
        .strip_prefix("systemd ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// 이 호스트의 systemd 판(읽지 못하면 None)
async fn read_systemd_version() -> Option<u32> {
    let out = tokio::process::Command::new("systemctl")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), out)
        .await
        .ok()?
        .ok()?;
    systemd_version(&String::from_utf8_lossy(&out.stdout))
}

/// 막히는 쪽(spawn_blocking)에서 한 일
struct Outcome {
    result: Result<NodeTopology, String>,
    pid1: Option<CpuSet>,
    reserve_errors: Option<Vec<String>>,
}

impl TopologyWatch {
    /// `sys`·`proc`는 `/sys`·`/proc`의 뿌리(시험은 고정물 트리)다.
    pub fn new(sys: impl Into<PathBuf>, proc: impl Into<PathBuf>, systemd_scope: bool) -> Self {
        Self {
            sys: sys.into(),
            proc: proc.into(),
            systemd_scope,
            applied: None,
            reserve_errors: Vec::new(),
            pinned: None,
            pin_error: None,
            last: None,
            rounds: 0,
            systemd: None,
        }
    }

    /// systemd 판을 읽지 않고 정한다(시험).
    #[cfg(test)]
    pub fn with_systemd_version(mut self, version: Option<u32>) -> Self {
        self.systemd = Some(version);
        self
    }

    /// 마지막 결과(아직 읽지 않았으면 None)
    pub fn last(&self) -> Option<&NodeTopology> {
        self.last.as_ref()
    }

    /// 검사 회차마다: 노드 설정이 바뀌었거나 [`PROBE_EVERY`] 회차가 지났으면 [`Self::refresh`]한다.
    pub async fn poll(&mut self, resources: Option<&HostResources>, now: u64) {
        self.rounds += 1;
        let changed = self.applied.as_ref().map(Option::as_ref) != Some(resources);
        if changed || self.last.is_none() || self.rounds >= PROBE_EVERY {
            self.refresh(resources, now).await;
        }
    }

    /// 설정이 바뀌었으면 hugepages를 예약하고, 읽고, opld 친화를 맞추고, 경고를 셈한다. 값이 바뀌었을 때만 `probed_at`이
    /// `now`다. 읽지 못하면 앞 결과에 `error`만 단다.
    pub async fn refresh(&mut self, resources: Option<&HostResources>, now: u64) -> &NodeTopology {
        self.rounds = 0;
        if self.systemd_scope && self.systemd.is_none() {
            self.systemd = Some(read_systemd_version().await);
        }
        let changed = self.applied.as_ref().map(Option::as_ref) != Some(resources);
        let (sys, proc) = (self.sys.clone(), self.proc.clone());
        let wanted = resources
            .filter(|_| changed)
            .map(|r| r.hugepages.clone())
            .unwrap_or_default();
        let outcome = tokio::task::spawn_blocking(move || {
            // 설정에서 뺀 예약은 그대로 둔다(다른 쓰임새일 수 있다, §7).
            let reserve_errors = changed.then(|| reserve_hugepages(&sys, &wanted));
            Outcome {
                result: probe(&sys, &proc),
                pid1: pid1_cpus(&proc),
                reserve_errors,
            }
        })
        .await
        .unwrap_or_else(|e| Outcome {
            result: Err(format!("the topology probe panicked: {e}")),
            pid1: None,
            reserve_errors: None,
        });
        if let Some(errors) = outcome.reserve_errors {
            self.reserve_errors = errors;
        }
        self.applied = Some(resources.cloned());
        let mut topology = match outcome.result {
            Ok(mut t) => {
                // 옛 systemd(244 전)는 `--property=AllowedCPUs=`를 모르는 칸으로 거절해 모든 VM 켜기가 실패한다: 스코프에는
                // 걸지 않고 스레드 친화만 건다(리뷰 1).
                let version = self.systemd.flatten();
                let old_systemd = t.cpuset
                    && self.systemd_scope
                    && version.is_none_or(|v| v < SYSTEMD_ALLOWED_CPUS);
                t.cpuset = t.cpuset && self.systemd_scope && !old_systemd;
                self.pin(resources, &t.cpus);
                let mut warn = warnings(&t, resources, outcome.pid1.as_ref());
                if old_systemd && resources.is_some() {
                    warn.push(format!(
                        "systemd {} cannot limit VM scopes to CPUs (AllowedCPUs needs systemd {SYSTEMD_ALLOWED_CPUS}); only the thread affinity is set",
                        version.map_or_else(|| "of unknown version".into(), |v| v.to_string())
                    ));
                }
                warn.extend(self.reserve_errors.iter().cloned());
                warn.extend(self.pin_error.iter().cloned());
                t.warnings = warn;
                t
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the NUMA topology");
                NodeTopology {
                    error: Some(e),
                    ..self.last.clone().unwrap_or_default()
                }
            }
        };
        topology.probed_at = now;
        let topology = keep_time(self.last.as_ref(), &topology);
        self.last.insert(topology)
    }

    /// 예약 CPU가 있으면 opld의 친화를 그것으로, 없어졌으면 뜰 때의 친화(관리자가 좁힌 그대로, 모르면 온라인 CPU 전체)로
    /// 되돌린다(바뀔 때만, 리뷰 2 N3). 좁혔는지를 드라이버에 알린다(고정 없는 QEMU가 예약 CPU를 물려받지 않게).
    fn pin(&mut self, resources: Option<&HostResources>, online: &CpuSet) {
        let reserved = resources
            .map(|r| r.reserved_cpus.intersection(online))
            .unwrap_or_default();
        let reserved_empty = reserved.is_empty();
        let want = if !reserved_empty {
            reserved
        } else if self.pinned.is_some() {
            orbivisor_driver::affinity::start_cpus().unwrap_or_else(|| online.clone())
        } else {
            return;
        };
        if self.pinned.as_ref() == Some(&want) && self.pin_error.is_none() {
            return;
        }
        match pin_self(&want) {
            Ok(()) => {
                self.pin_error = None;
                // 뜰 때의 친화로 되돌렸으면 건 것이 없다.
                self.pinned = (!reserved_empty).then_some(want);
                orbivisor_driver::affinity::set_narrowed(!reserved_empty);
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot pin orbivisord to the reserved CPUs");
                self.pin_error = Some(format!(
                    "cannot pin orbivisord to the reserved CPUs {want}: {e}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbivisor_types::numa::PageKind;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/topology")
            .join(name)
    }

    fn probe_fixture(name: &str) -> NodeTopology {
        let root = fixture(name);
        probe(&root.join("sys"), &root.join("proc")).unwrap()
    }

    fn set(s: &str) -> CpuSet {
        s.parse().unwrap()
    }

    /// 고정물을 임시 디렉터리로 옮긴다(쓰는 시험용).
    fn copy_fixture(name: &str) -> tempfile::TempDir {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                let target = to.join(e.file_name());
                if e.file_type().unwrap().is_dir() {
                    copy(&e.path(), &target);
                } else {
                    std::fs::copy(e.path(), target).unwrap();
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        copy(&fixture(name), dir.path());
        dir
    }

    /// x86 2소켓(손으로 만든 고정물): 소켓 2 × 코어 8 × 스레드 2, NUMA 노드 2(거리 10·21), L3 둘, 노드마다 1G 4·2M 512.
    #[test]
    fn probes_a_two_socket_x86_host() {
        let t = probe_fixture("x86-2s");
        assert_eq!((t.sockets, t.threads_per_core), (2, 2));
        assert_eq!(t.cpus, set("0-31"));
        assert_eq!(t.nodes.len(), 2);
        assert_eq!(
            (t.nodes[0].id, &t.nodes[0].cpus, t.nodes[0].memory_mib),
            (0, &set("0-7,16-23"), 64300)
        );
        assert_eq!((t.nodes[1].id, &t.nodes[1].cpus), (1, &set("8-15,24-31")));
        assert_eq!(
            t.nodes[1].hugepages,
            [
                HugepagePool {
                    size_kib: 2048,
                    total: 512
                },
                HugepagePool {
                    size_kib: 1048576,
                    total: 4
                }
            ]
        );
        assert_eq!(t.pages(PageKind::Size1G), 8);
        assert_eq!(t.distances, [[10, 21], [21, 10]]);
        assert_eq!(t.siblings.len(), 16);
        assert_eq!(
            (&t.siblings[0], &t.siblings[15]),
            (&set("0,16"), &set("15,31"))
        );
        assert_eq!(t.l3, [set("0-7,16-23"), set("8-15,24-31")]);
        assert_eq!(t.page_sizes_kib, [2048, 1048576]);
        assert!(t.isolated.is_empty() && t.nohz_full.is_empty());
        assert!(t.pinning && t.cpuset);
        assert!(t.warnings.is_empty() && t.error.is_none());
    }

    /// svc01에서 기록한 aarch64 노드 하나: 4 CPU, SMT·L3 없음(L2까지), hugepages 0(크기 넷), cpuset 있음.
    #[test]
    fn probes_a_one_node_aarch64_host() {
        let t = probe_fixture("aarch64-1n");
        assert_eq!((t.sockets, t.threads_per_core), (1, 1));
        assert_eq!(t.cpus, set("0-3"));
        assert_eq!(t.nodes.len(), 1);
        assert_eq!((t.nodes[0].id, &t.nodes[0].cpus), (0, &set("0-3")));
        assert_eq!(t.nodes[0].memory_mib, 8109676 / 1024);
        assert!(t.nodes[0].hugepages.is_empty());
        assert_eq!(t.distances, [[10]]);
        assert!(t.siblings.is_empty() && t.l3.is_empty());
        assert_eq!(t.page_sizes_kib, [64, 2048, 32768, 1048576]);
        assert!(t.cpuset);
        assert_eq!(pid1_cpus(&fixture("aarch64-1n/proc")), Some(set("0-3")));
    }

    /// `node` 디렉터리가 없는 커널: 노드 0 하나(모든 CPU, `MemTotal`), 풀은 커널 전체의 것, 거리 없음. cpuset 컨트롤러가
    /// 없으면 `cpuset`은 거짓이다.
    #[test]
    fn a_kernel_without_numa_is_one_node() {
        let t = probe_fixture("x86-no-numa");
        assert_eq!((t.sockets, t.threads_per_core), (1, 2));
        assert_eq!(t.nodes.len(), 1);
        assert_eq!(
            (&t.nodes[0].cpus, t.nodes[0].memory_mib),
            (&set("0-3"), 16314368 / 1024)
        );
        assert_eq!(t.nodes[0].pages(PageKind::Size2M), 128);
        assert!(t.distances.is_empty());
        assert_eq!(t.siblings, [set("0-1"), set("2-3")]);
        assert_eq!(t.l3, [set("0-3")]);
        assert!(!t.cpuset);
    }

    /// 오프라인 CPU는 어디에도 세지 않는다(형제·L3·노드 CPU). `isolated`를 읽고, 옛 커널의 `nohz_full` `(null)`은 빈 것이다.
    #[test]
    fn offline_cpus_are_left_out() {
        let t = probe_fixture("x86-offline");
        assert_eq!(t.cpus, set("0-6"));
        assert_eq!(t.threads_per_core, 2);
        assert_eq!(t.siblings, [set("0,4"), set("1,5"), set("2,6")]);
        assert_eq!(t.l3, [set("0-6")]);
        assert_eq!(t.nodes[0].cpus, set("0-6"));
        assert_eq!(t.isolated, set("2-3,6"));
        assert!(t.nohz_full.is_empty());
        assert_eq!(
            pid1_cpus(&fixture("x86-offline/proc")),
            Some(set("0-1,4-5"))
        );
    }

    /// 온라인 목록이 없으면 탐침 실패다.
    #[test]
    fn a_missing_online_list_fails() {
        let dir = tempfile::tempdir().unwrap();
        let err = probe(&dir.path().join("sys"), &dir.path().join("proc")).unwrap_err();
        assert!(err.contains("devices/system/cpu/online"), "{err}");
    }

    fn resources(
        reserved: &str,
        dedicated: &str,
        hugepages: Vec<HugepageReserve>,
    ) -> HostResources {
        HostResources {
            reserved_cpus: set(reserved),
            dedicated_cpus: set(dedicated),
            hugepages,
        }
    }

    /// 경고: 설정이 없으면 없다. 오프라인 전용 CPU, isolcpus 밖이고 PID 1이 도는 전용 CPU, 형제를 나누는 전용 집합,
    /// 모자란 예약(노드마다·전체).
    #[test]
    fn warns_about_host_preparation() {
        let t = probe_fixture("x86-offline");
        let pid1 = pid1_cpus(&fixture("x86-offline/proc"));
        assert!(warnings(&t, None, pid1.as_ref()).is_empty());

        // 2-3·6은 isolated, 7은 오프라인. 형제 묶음 2,6은 통째로 든다.
        let r = resources("0", "2-3,6-7", vec![]);
        assert_eq!(
            warnings(&t, Some(&r), pid1.as_ref()),
            ["dedicated CPUs 7 are offline"]
        );

        // 4-5는 isolated가 아니고 PID 1이 돈다. 4·5의 형제 0·1은 전용 밖이다.
        let r = resources(
            "",
            "4-5",
            vec![
                HugepageReserve {
                    size: PageKind::Size2M,
                    count: 100,
                    host_node: Some(0),
                },
                HugepageReserve {
                    size: PageKind::Size1G,
                    count: 2,
                    host_node: None,
                },
            ],
        );
        assert_eq!(
            warnings(&t, Some(&r), pid1.as_ref()),
            [
                "host processes may run on the dedicated CPUs 4-5; see the host preparation guide",
                "the dedicated CPUs split SMT sibling groups 0,4 1,5",
                "node 0: 2M hugepages 100 wanted, 64 reserved",
                "1G hugepages 2 wanted, 0 reserved",
            ]
        );
        // PID 1을 모르면 그 경고는 없다.
        assert_eq!(warnings(&t, Some(&r), None).len(), 3);

        // 리뷰 1 M7: CPU가 오프라인이 되어 공유가 비면 알린다(공유 VM·에뮬레이터는 온라인 CPU 전체에서 돈다).
        let all = resources(
            "0-1",
            &format!("2-{}", t.cpus.iter().max().unwrap()),
            vec![],
        );
        assert!(
            warnings(&t, Some(&all), None)
                .iter()
                .any(|w| w.starts_with("no shared CPU is online")),
            "{:?}",
            warnings(&t, Some(&all), None)
        );
    }

    /// 예약은 노드마다·전체 `nr_hugepages`에 지금 수와 다를 때만 쓰고, 다음 탐침이 얻은 수를 읽는다. 쓰지 못하면 까닭을
    /// 돌려준다.
    #[test]
    fn reserves_hugepages_by_writing_nr_hugepages() {
        let dir = copy_fixture("x86-2s");
        let sys = dir.path().join("sys");
        let wanted = [
            HugepageReserve {
                size: PageKind::Size2M,
                count: 1024,
                host_node: Some(1),
            },
            HugepageReserve {
                size: PageKind::Size1G,
                count: 4,
                host_node: Some(0),
            },
            HugepageReserve {
                size: PageKind::Size2M,
                count: 2048,
                host_node: None,
            },
        ];
        assert!(reserve_hugepages(&sys, &wanted).is_empty());
        let read = |p: &str| std::fs::read_to_string(sys.join(p)).unwrap();
        assert_eq!(
            read("devices/system/node/node1/hugepages/hugepages-2048kB/nr_hugepages"),
            "1024\n"
        );
        assert_eq!(
            read("kernel/mm/hugepages/hugepages-2048kB/nr_hugepages"),
            "2048\n"
        );
        // 같은 수는 쓰지 않는다(원래 글 그대로).
        assert_eq!(
            read("devices/system/node/node0/hugepages/hugepages-1048576kB/nr_hugepages"),
            "4\n"
        );
        let t = probe(&sys, &dir.path().join("proc")).unwrap();
        assert_eq!(t.nodes[1].pages(PageKind::Size2M), 1024);

        let missing = [HugepageReserve {
            size: PageKind::Size2M,
            count: 8,
            host_node: Some(7),
        }];
        let errors = reserve_hugepages(&sys, &missing);
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].starts_with("cannot reserve 8 2M hugepages on node 7: "),
            "{errors:?}"
        );
    }

    /// 탐침: 처음 읽을 때와 설정이 바뀐 회차, 여섯 회차마다 읽는다. 값이 같으면 `probed_at`을 바꾸지 않는다. 설정이 바뀌면
    /// 예약을 쓰고 경고를 다시 센다. 스코프를 쓰지 않으면 `cpuset`은 거짓이다.
    #[tokio::test]
    async fn the_watch_applies_settings_and_keeps_the_time_when_nothing_changes() {
        let dir = copy_fixture("x86-2s");
        let sys = dir.path().join("sys");
        let mut watch =
            TopologyWatch::new(&sys, dir.path().join("proc"), true).with_systemd_version(Some(255));
        watch.poll(None, 100).await;
        let first = watch.last().unwrap().clone();
        assert_eq!(first.probed_at, 100);
        assert!(first.cpuset);
        for now in 101..110 {
            watch.poll(None, now).await;
        }
        assert_eq!(watch.last(), Some(&first));

        // 설정이 바뀜: 다음 회차에 바로 예약하고 다시 읽는다.
        let r = resources(
            "",
            "",
            vec![HugepageReserve {
                size: PageKind::Size2M,
                count: 600,
                host_node: Some(0),
            }],
        );
        watch.poll(Some(&r), 200).await;
        let after = watch.last().unwrap();
        assert_eq!(after.probed_at, 200);
        assert_eq!(after.nodes[0].pages(PageKind::Size2M), 600);
        assert!(after.warnings.is_empty());

        // 밖에서 수가 바뀜(커널이 덜 돌려줌): 여섯 회차 안에 보이고 경고한다. 설정이 같으면 다시 쓰지 않는다.
        std::fs::write(
            sys.join("devices/system/node/node0/hugepages/hugepages-2048kB/nr_hugepages"),
            "512\n",
        )
        .unwrap();
        for now in 201..=206 {
            watch.poll(Some(&r), now).await;
        }
        let short = watch.last().unwrap().clone();
        assert_eq!(short.probed_at, 206);
        assert_eq!(
            short.warnings,
            ["node 0: 2M hugepages 600 wanted, 512 reserved"]
        );

        // 읽지 못하면 앞 값에 오류만
        std::fs::remove_file(sys.join("devices/system/cpu/online")).unwrap();
        let failed = watch.refresh(Some(&r), 300).await.clone();
        assert_eq!(failed.nodes, short.nodes);
        assert!(failed.error.unwrap().contains("cannot read"));

        let mut unscoped = TopologyWatch::new(fixture("x86-2s/sys"), fixture("x86-2s/proc"), false);
        assert!(!unscoped.refresh(None, 1).await.cpuset);
    }

    /// 리뷰 1 M4: systemd 244 전(또는 판을 모름)이면 스코프에 `AllowedCPUs`를 걸지 않고(`cpuset` 거짓) 노드 설정이 있을
    /// 때 경고한다.
    #[tokio::test]
    async fn old_systemd_gets_no_allowed_cpus() {
        assert_eq!(
            systemd_version("systemd 255 (255.4-1ubuntu8.18)\n+PAM +AUDIT"),
            Some(255)
        );
        assert_eq!(systemd_version("systemd 241 (241)"), Some(241));
        assert_eq!(systemd_version("nope"), None);
        let r = resources("", "4-15", Vec::new());
        let root = (fixture("x86-2s/sys"), fixture("x86-2s/proc"));
        let mut old = TopologyWatch::new(&root.0, &root.1, true).with_systemd_version(Some(241));
        let t = old.refresh(Some(&r), 1).await.clone();
        assert!(!t.cpuset);
        assert!(
            t.warnings
                .iter()
                .any(|w| w.contains("systemd 241 cannot limit VM scopes")),
            "{:?}",
            t.warnings
        );
        let mut unknown = TopologyWatch::new(&root.0, &root.1, true).with_systemd_version(None);
        assert!(!unknown.refresh(None, 1).await.cpuset);
        let mut new = TopologyWatch::new(&root.0, &root.1, true).with_systemd_version(Some(244));
        assert!(new.refresh(Some(&r), 1).await.cpuset);
    }

    /// 크기(§6): 2소켓 × 64코어 × 2스레드(256 CPU), 노드 2, L3 16의 JSON. 4 KB를 넘으면 형제 묶음을 줄여야 한다.
    #[test]
    fn a_large_topology_stays_small() {
        let half = 128u16;
        let topology = NodeTopology {
            sockets: 2,
            threads_per_core: 2,
            cpus: (0..256).collect(),
            nodes: (0..2)
                .map(|n| HostNode {
                    id: n,
                    cpus: (0..256).filter(|c| (c % half) / 64 == n).collect(),
                    memory_mib: 515_000,
                    hugepages: vec![
                        HugepagePool {
                            size_kib: 2048,
                            total: 16384,
                        },
                        HugepagePool {
                            size_kib: 1048576,
                            total: 64,
                        },
                    ],
                })
                .collect(),
            distances: vec![vec![10, 32], vec![32, 10]],
            siblings: (0..half)
                .map(|c| CpuSet::from_iter([c, c + half]))
                .collect(),
            l3: (0..16)
                .map(|g| (0..256).filter(|c| (c % half) / 8 == g).collect())
                .collect(),
            page_sizes_kib: vec![2048, 1048576],
            isolated: set("8-63,72-127,136-191,200-255"),
            pinning: true,
            cpuset: true,
            probed_at: 1_800_000_000,
            ..NodeTopology::default()
        };
        let size = serde_json::to_string(&topology).unwrap().len();
        println!("256-CPU topology: {size} bytes");
        assert!(size < 4096, "{size}");
        let small = serde_json::to_string(&probe_fixture("x86-2s"))
            .unwrap()
            .len();
        println!("x86-2s topology: {small} bytes");
        assert!(small < 1024, "{small}");
    }

    /// `same`·`keep_time`은 `probed_at`만 무시한다.
    #[test]
    fn keeps_the_stored_time_when_only_the_time_differs() {
        let a = NodeTopology {
            sockets: 1,
            probed_at: 1,
            ..NodeTopology::default()
        };
        let b = NodeTopology {
            probed_at: 2,
            ..a.clone()
        };
        assert!(same(&a, &b));
        assert_eq!(keep_time(Some(&a), &b).probed_at, 1);
        let c = NodeTopology {
            sockets: 2,
            ..b.clone()
        };
        assert_eq!(keep_time(Some(&a), &c), c);
        assert_eq!(keep_time(None, &b), b);
    }

    #[test]
    fn live_values_are_read_per_host_node() {
        let root = fixture("x86-2s");
        let live = live(&root.join("sys"), &root.join("proc"), &CpuSet::new()).unwrap();
        assert_eq!(live.nodes.len(), 2);
        assert_eq!(live.nodes[0].id, 0);
        assert_eq!(live.nodes[0].memory_free_mib, 32921966 / 1024);
        let sizes: Vec<u32> = live.nodes[0].hugepages.iter().map(|p| p.size_kib).collect();
        assert_eq!(sizes, [2048, 1048576]);
        assert_eq!(live.nodes[0].hugepages[0].free, 512);
        // 고정물에 `self/status`가 없으면 친화를 모른다.
        assert!(!live.affinity_ok);

        let dir = copy_fixture("x86-no-numa");
        let (sys, proc) = (dir.path().join("sys"), dir.path().join("proc"));
        std::fs::create_dir_all(proc.join("self")).unwrap();
        let online = probe(&sys, &proc).unwrap().cpus;
        std::fs::write(
            proc.join("self/status"),
            format!("Name:\topld\nCpus_allowed_list:\t{online}\n"),
        )
        .unwrap();
        let live = super::live(&sys, &proc, &CpuSet::new()).unwrap();
        assert_eq!(live.nodes.len(), 1);
        assert_eq!(live.nodes[0].memory_free_mib, 8157184 / 1024);
        assert!(live.affinity_ok);
        // 예약 CPU가 있는데 opld가 온라인 전체에 돌면 맞지 않는다.
        assert!(!super::live(&sys, &proc, &set("0")).unwrap().affinity_ok);
        std::fs::write(proc.join("self/status"), "Cpus_allowed_list:\t0\n").unwrap();
        assert!(super::live(&sys, &proc, &set("0")).unwrap().affinity_ok);
    }
}

/// 실제 Linux 호스트에서(CI `linux-kvm`, root): 이 호스트를 읽고(고정물로 옮길 수 있게 찍는다), 2M hugepages를 실행 중
/// 예약했다가 되돌리고, 다른 프로세스의 스레드 친화를 건다. `/sys`에 쓰므로 `OPL_TEST_NUMA_SYS=1`일 때만 돈다(배포 서버의
/// 호스트 설정을 바꾸지 않게).
#[cfg(all(test, feature = "numa-it", target_os = "linux"))]
mod real {
    use super::*;
    use orbivisor_types::numa::PageKind;

    fn enabled() -> bool {
        let on = std::env::var("OPL_TEST_NUMA_SYS").as_deref() == Ok("1")
            && unsafe { libc::geteuid() } == 0;
        if !on {
            eprintln!("skipped: needs root and OPL_TEST_NUMA_SYS=1");
        }
        on
    }

    #[test]
    fn probes_this_host() {
        let t = probe(Path::new("/sys"), Path::new("/proc")).unwrap();
        println!("{}", serde_json::to_string_pretty(&t).unwrap());
        assert!(!t.cpus.is_empty() && !t.nodes.is_empty());
        let in_nodes = t
            .nodes
            .iter()
            .fold(CpuSet::new(), |all, n| all.union(&n.cpus));
        assert_eq!(in_nodes, t.cpus);
        assert!(t.page_sizes_kib.contains(&2048));
    }

    /// 쓴 `nr_hugepages`를 끝에(실패해도) 되돌린다.
    struct Restore(Vec<(PathBuf, String)>);

    impl Drop for Restore {
        fn drop(&mut self) {
            for (path, text) in &self.0 {
                let _ = std::fs::write(path, text);
            }
        }
    }

    #[test]
    fn reserves_2m_hugepages_at_runtime() {
        if !enabled() {
            return;
        }
        let sys = Path::new("/sys");
        let node = HugepageReserve {
            size: PageKind::Size2M,
            count: 0,
            host_node: Some(0),
        };
        let all = HugepageReserve {
            host_node: None,
            ..node
        };
        let (node_path, all_path) = (nr_hugepages(sys, &node), nr_hugepages(sys, &all));
        let original = |p: &Path| std::fs::read_to_string(p).unwrap();
        let _restore = Restore(vec![
            (node_path.clone(), original(&node_path)),
            (all_path.clone(), original(&all_path)),
        ]);
        let before = probe(sys, Path::new("/proc")).unwrap().nodes[0].pages(PageKind::Size2M);
        let want = u32::try_from(before).unwrap() + 16;
        let errors = reserve_hugepages(
            sys,
            &[HugepageReserve {
                count: want,
                ..node
            }],
        );
        assert!(errors.is_empty(), "{errors:?}");
        let t = probe(sys, Path::new("/proc")).unwrap();
        assert_eq!(t.nodes[0].pages(PageKind::Size2M), u64::from(want));
        let r = HostResources {
            hugepages: vec![HugepageReserve {
                count: want,
                ..node
            }],
            ..HostResources::default()
        };
        assert!(warnings(&t, Some(&r), None).is_empty());
        // 전체 수로 되돌림
        let errors = reserve_hugepages(
            sys,
            &[HugepageReserve {
                count: u32::try_from(before).unwrap(),
                ..all
            }],
        );
        assert!(errors.is_empty(), "{errors:?}");
        let t = probe(sys, Path::new("/proc")).unwrap();
        assert_eq!(t.pages(PageKind::Size2M), before);
    }

    /// 다른 프로세스(sleep)의 모든 스레드 친화를 CPU 하나로 두고 `Cpus_allowed_list`로 확인한다.
    #[test]
    fn pins_the_threads_of_a_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let online = probe(Path::new("/sys"), Path::new("/proc")).unwrap().cpus;
        let last = CpuSet::from_iter([online.iter().last().unwrap()]);
        let tasks = PathBuf::from(format!("/proc/{}/task", child.id()));
        let pinned = pin_tasks(&tasks, &last);
        let status = std::fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(pinned, Ok(1));
        let allowed: CpuSet = status
            .lines()
            .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(allowed, last);
    }
}
