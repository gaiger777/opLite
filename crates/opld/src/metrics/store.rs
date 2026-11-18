//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.
//! API와 노드 간 서버는 [`StoreSource`]로 읽는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use opl_peer::{HistoryError, MetricsReply, MetricsSource};
use opl_types::metrics::{
    HistoryQuery, HistoryReply, MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample,
    VmSeries,
};
use opl_types::placement::{LoadSummary, VmLoad};
use uuid::Uuid;

use super::history::History;

/// 시리즈마다 두는 점 수(5초 × 1시간). 넘으면 앞을 버린다.
pub const MAX_SAMPLES: usize = 720;
/// 마지막 점이 이만큼(밀리초) 지난 VM 시리즈는 지운다(옮겨 나간 VM, 지운 VM).
pub const VM_SERIES_TTL_MS: i64 = 3_600_000;
/// 부하 요약에 넣는 VM: 마지막 점이 이만큼(밀리초) 안인 시리즈만(옮겨 나간 VM, 멈춘 VM은 뺀다)
pub const LOAD_VM_FRESH_MS: i64 = 15_000;
/// 부하 요약의 짧은·긴 CPU 평균 창(밀리초)
const LOAD_SHORT_MS: i64 = 60_000;
const LOAD_LONG_MS: i64 = 300_000;

#[derive(Default)]
pub struct MetricsStore {
    inner: Mutex<Series>,
}

#[derive(Default)]
struct Series {
    node: VecDeque<NodeSample>,
    vms: HashMap<Uuid, VecDeque<VmSample>>,
    /// 시리즈마다 넣은 점의 번호(마지막 점까지 센 값). 기록 쓰기([`MetricsStore::take_pending`])가 앞서 가져간 뒤의 점을
    /// 찾는다. 시각이 아니라 넣은 순서로 세므로 벽시계가 뒤로 가도 새 점을 놓치지 않는다. 새 VM 시리즈는 모든 시리즈에
    /// 넣은 점 수(`total`)부터 세므로, 지워졌다 다시 생긴 시리즈도 앞서 가져간 자리보다 크다.
    node_pushed: u64,
    vm_pushed: HashMap<Uuid, u64>,
    total: u64,
}

/// 기록 쓰기가 마지막으로 가져간 자리(시리즈마다 넣은 점 수)
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Marks {
    node: u64,
    vms: HashMap<Uuid, u64>,
}

/// 앞서 가져간 뒤에 들어온 점들(넣은 순서)
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Pending {
    pub node: Vec<NodeSample>,
    pub vms: Vec<(Uuid, Vec<VmSample>)>,
}

/// 시리즈의 마지막 점 번호가 `pushed`이고 앞서 `mark`까지 가져갔을 때 새 점(뒤쪽 `pushed - mark`개, 남은 것까지)
fn newer<T: Clone>(series: &VecDeque<T>, pushed: u64, mark: u64) -> Vec<T> {
    let n = if pushed >= mark {
        usize::try_from(pushed - mark).unwrap_or(usize::MAX)
    } else {
        series.len()
    };
    let skip = series.len().saturating_sub(n);
    series.iter().skip(skip).cloned().collect()
}

/// 저장소에서 읽은 점들(시각 순). 노드 id는 읽는 쪽이 붙인다.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsLocal {
    /// `Nodes`를 물었을 때 이 노드의 점들
    pub node: Option<Vec<NodeSample>>,
    /// `Vm`을 물었을 때 그 VM의 점들(이 노드에서 잰 적이 없으면 `None`)
    pub vm: Option<Vec<VmSample>>,
}

fn push<T>(series: &mut VecDeque<T>, sample: T) {
    if series.len() == MAX_SAMPLES {
        series.pop_front();
    }
    series.push_back(sample);
}

impl MetricsStore {
    pub fn push_node(&self, sample: NodeSample) {
        let mut inner = self.inner.lock().unwrap();
        push(&mut inner.node, sample);
        inner.node_pushed += 1;
        inner.total += 1;
    }

    pub fn push_vm(&self, vm: Uuid, sample: VmSample) {
        let inner = &mut *self.inner.lock().unwrap();
        push(inner.vms.entry(vm).or_default(), sample);
        *inner.vm_pushed.entry(vm).or_insert(inner.total) += 1;
        inner.total += 1;
    }

    /// 마지막 점이 `now`(유닉스 밀리초)보다 [`VM_SERIES_TTL_MS`] 넘게 앞선 VM 시리즈를 지운다.
    pub fn prune(&self, now: i64) {
        let inner = &mut *self.inner.lock().unwrap();
        inner.vms.retain(|_, series| {
            series
                .back()
                .is_some_and(|last| now - last.t <= VM_SERIES_TTL_MS)
        });
        let vms = &inner.vms;
        inner.vm_pushed.retain(|id, _| vms.contains_key(id));
    }

    /// `marks` 뒤에 들어온 점들을 돌려주고 `marks`를 지금으로 옮긴다(기록 쓰기가 분마다 부른다). 저장소가 그 사이
    /// 잘라 낸 점은 없다(1시간 안에 가져가므로).
    pub fn take_pending(&self, marks: &mut Marks) -> Pending {
        let inner = self.inner.lock().unwrap();
        let node = newer(&inner.node, inner.node_pushed, marks.node);
        marks.node = inner.node_pushed;
        let mut vms = Vec::new();
        for (id, series) in &inner.vms {
            let pushed = inner.vm_pushed.get(id).copied().unwrap_or_default();
            let mark = marks.vms.get(id).copied().unwrap_or_default();
            let points = newer(series, pushed, mark);
            if !points.is_empty() {
                vms.push((*id, points));
            }
        }
        vms.sort_by_key(|(id, _)| *id);
        marks.vms = inner.vm_pushed.clone();
        Pending { node, vms }
    }

    /// VM 시리즈의 마지막 점 시각(이 노드에서 잰 적이 없거나 지워졌으면 `None`)
    pub fn last_vm_t(&self, vm: Uuid) -> Option<i64> {
        let inner = self.inner.lock().unwrap();
        inner.vms.get(&vm).and_then(|s| s.back()).map(|s| s.t)
    }

    /// 시리즈가 있는 VM들
    pub fn vm_ids(&self) -> Vec<Uuid> {
        self.inner.lock().unwrap().vms.keys().copied().collect()
    }

    /// `from ≤ t ≤ until`인 노드 점들
    pub fn node_window(&self, from: i64, until: i64) -> Vec<NodeSample> {
        let inner = self.inner.lock().unwrap();
        (inner.node.iter())
            .filter(|s| (from..=until).contains(&s.t))
            .cloned()
            .collect()
    }

    /// `from ≤ t ≤ until`인 VM 점들
    pub fn vm_window(&self, vm: Uuid, from: i64, until: i64) -> Vec<VmSample> {
        let inner = self.inner.lock().unwrap();
        (inner.vms.get(&vm).into_iter().flatten())
            .filter(|s| (from..=until).contains(&s.t))
            .cloned()
            .collect()
    }

    /// 리스 갱신에 실을 부하 요약(설계 §8.3). 노드 CPU는 `now`(유닉스 밀리초)까지 1분·5분 동안의 평균 ‰,
    /// 메모리는 마지막 점의 사용량이다. VM은 마지막 점이 [`LOAD_VM_FRESH_MS`] 안인 것만 5분 CPU 평균 ‰와 마지막
    /// RSS로 넣고, `MAX_LOAD_VMS`를 넘으면 RSS 큰 것부터 둔다. 값 없는 점은 평균에서 빠진다.
    pub fn summary(&self, now: i64) -> LoadSummary {
        let inner = self.inner.lock().unwrap();
        let node_cpu = |window: i64| {
            permille(
                (inner.node.iter())
                    .filter(|s| s.t >= now - window)
                    .filter_map(|s| s.cpu),
            )
        };
        let mut vms: Vec<VmLoad> = (inner.vms.iter())
            .filter_map(|(id, series)| {
                let last = series.back()?;
                (now - last.t <= LOAD_VM_FRESH_MS).then(|| VmLoad {
                    id: *id,
                    cpu: permille(
                        (series.iter())
                            .filter(|s| s.t >= now - LOAD_LONG_MS)
                            .filter_map(|s| s.cpu),
                    ),
                    mem: last.mem_rss_mib,
                })
            })
            .collect();
        // 순서를 정해 둔다(같은 측정이면 같은 요약).
        vms.sort_by_key(|v| v.id);
        let mut load = LoadSummary {
            cpu1: node_cpu(LOAD_SHORT_MS),
            cpu5: node_cpu(LOAD_LONG_MS),
            mem: inner.node.back().and_then(|s| s.mem_used_mib),
            vms,
        };
        load.truncate_vms();
        load
    }

    /// `since`보다 `t`가 큰 점만 돌려준다.
    pub fn read(&self, q: &MetricsQuery) -> MetricsLocal {
        let after = |t: i64| q.since.is_none_or(|since| t > since);
        let inner = self.inner.lock().unwrap();
        match &q.scope {
            MetricsScope::Nodes => MetricsLocal {
                node: Some(inner.node.iter().filter(|s| after(s.t)).cloned().collect()),
                vm: None,
            },
            MetricsScope::Vm { id } => MetricsLocal {
                node: None,
                vm: inner
                    .vms
                    .get(id)
                    .map(|series| series.iter().filter(|s| after(s.t)).cloned().collect()),
            },
        }
    }
}

/// 백분율 값들의 평균을 ‰로(반올림). 값이 없으면 `None`이다.
fn permille(values: impl Iterator<Item = f64>) -> Option<u16> {
    let (sum, count) = values.fold((0.0, 0u32), |(sum, count), v| (sum + v, count + 1));
    (count > 0).then(|| {
        (sum / f64::from(count) * 10.0)
            .round()
            .clamp(0.0, f64::from(u16::MAX)) as u16
    })
}

/// 이 노드(`node`)의 저장소를 지표 API와 노드 간 서버(`ReadMetrics`)에 읽어 준다. 기록([`History`])이 있으면 기록
/// 조회(`ReadMetricsHistory`)와 밀어 넘긴 VM 기록 받기(`PushMetricsHistory`)도 한다.
pub struct StoreSource {
    node: Uuid,
    store: Arc<MetricsStore>,
    history: Option<Arc<History>>,
}

impl StoreSource {
    pub fn new(node: Uuid, store: Arc<MetricsStore>) -> Self {
        Self {
            node,
            store,
            history: None,
        }
    }

    pub fn with_history(mut self, history: Arc<History>) -> Self {
        self.history = Some(history);
        self
    }
}

impl MetricsSource for StoreSource {
    fn history(&self, q: &HistoryQuery) -> Result<HistoryReply, HistoryError> {
        self.history
            .as_ref()
            .ok_or(HistoryError::Unsupported)?
            .read(q)
    }

    fn push_history(&self, vm: Uuid, bytes: &[u8]) -> Result<(), HistoryError> {
        self.history
            .as_ref()
            .ok_or(HistoryError::Unsupported)?
            .receive(vm, bytes)
    }

    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        let local = self.store.read(q);
        let vms = match (&q.scope, local.vm) {
            (MetricsScope::Vm { id }, Some(samples)) => vec![VmSeries {
                vm: *id,
                node: self.node,
                samples,
            }],
            _ => vec![],
        };
        MetricsReply {
            node: local.node.map(|samples| NodeSeries {
                node: self.node,
                samples,
            }),
            vms,
        }
    }
}

#[cfg(test)]
mod tests {
    use opl_types::placement::MAX_LOAD_VMS;

    use super::*;

    fn node(t: i64) -> NodeSample {
        NodeSample {
            t,
            cpu: Some(1.0),
            mem_used_mib: None,
            mem_total_mib: None,
            disk_read_bps: None,
            disk_write_bps: None,
            net_rx_bps: None,
            net_tx_bps: None,
            fs_used_mib: None,
            fs_total_mib: None,
        }
    }

    fn vm(t: i64) -> VmSample {
        VmSample {
            t,
            cpu: Some(1.0),
            mem_rss_mib: None,
            disk_read_bps: None,
            disk_write_bps: None,
            disk_read_iops: None,
            disk_write_iops: None,
            net_rx_bps: None,
            net_tx_bps: None,
        }
    }

    fn query(scope: MetricsScope, since: Option<i64>) -> MetricsQuery {
        MetricsQuery { scope, since }
    }

    fn times<T>(samples: &[T], t: impl Fn(&T) -> i64) -> Vec<i64> {
        samples.iter().map(t).collect()
    }

    #[test]
    fn series_keep_the_last_720_samples() {
        let store = MetricsStore::default();
        let id = Uuid::now_v7();
        for t in 0..MAX_SAMPLES as i64 + 5 {
            store.push_node(node(t));
            store.push_vm(id, vm(t));
        }
        let nodes = store.read(&query(MetricsScope::Nodes, None)).node.unwrap();
        assert_eq!(nodes.len(), MAX_SAMPLES);
        assert_eq!(nodes[0].t, 5);
        assert_eq!(nodes.last().unwrap().t, MAX_SAMPLES as i64 + 4);
        let vms = store
            .read(&query(MetricsScope::Vm { id }, None))
            .vm
            .unwrap();
        assert_eq!(vms.len(), MAX_SAMPLES);
        assert_eq!(vms[0].t, 5);
    }

    #[test]
    fn read_returns_samples_after_since() {
        let store = MetricsStore::default();
        let id = Uuid::now_v7();
        for t in [1_000, 2_000, 3_000] {
            store.push_node(node(t));
            store.push_vm(id, vm(t));
        }
        let all = store.read(&query(MetricsScope::Nodes, None));
        assert_eq!(times(&all.node.unwrap(), |s| s.t), [1_000, 2_000, 3_000]);
        assert_eq!(all.vm, None);
        let after = store.read(&query(MetricsScope::Nodes, Some(2_000)));
        assert_eq!(times(&after.node.unwrap(), |s| s.t), [3_000]);
        let none = store.read(&query(MetricsScope::Nodes, Some(3_000)));
        assert_eq!(none.node, Some(vec![]));

        let vm_after = store.read(&query(MetricsScope::Vm { id }, Some(1_000)));
        assert_eq!(vm_after.node, None);
        assert_eq!(times(&vm_after.vm.unwrap(), |s| s.t), [2_000, 3_000]);
        // 이 노드에서 잰 적이 없는 VM
        let other = store.read(&query(MetricsScope::Vm { id: Uuid::now_v7() }, None));
        assert_eq!(other.vm, None);
    }

    #[test]
    fn prune_drops_vm_series_older_than_an_hour() {
        let store = MetricsStore::default();
        let (old, fresh) = (Uuid::now_v7(), Uuid::now_v7());
        let now = 10_000_000;
        store.push_vm(old, vm(now - VM_SERIES_TTL_MS - 1));
        store.push_vm(fresh, vm(now - VM_SERIES_TTL_MS - 1));
        store.push_vm(fresh, vm(now - VM_SERIES_TTL_MS));
        store.push_node(node(0));
        store.prune(now);
        let read = |id| store.read(&query(MetricsScope::Vm { id }, None)).vm;
        assert_eq!(read(old), None);
        assert_eq!(read(fresh).unwrap().len(), 2);
        // 노드 시리즈는 상한으로만 줄어든다.
        let nodes = store.read(&query(MetricsScope::Nodes, None)).node.unwrap();
        assert_eq!(nodes.len(), 1);
    }

    /// 6분 동안 5초마다: 앞 3분은 10%, 뒤 3분은 30%. 1분 평균은 30%, 5분 평균은 둘이 섞인다.
    /// 값 없는 점은 평균에서 빠지고, 메모리는 마지막 점이다.
    #[test]
    fn summary_averages_node_cpu_over_one_and_five_minutes() {
        let store = MetricsStore::default();
        assert_eq!(store.summary(1_000_000), LoadSummary::default());
        let now = 10_000_000;
        let start = now - 360_000;
        for k in 1..=72 {
            let t = start + k * 5_000;
            let mut s = node(t);
            s.cpu = Some(if t <= start + 180_000 { 10.0 } else { 30.0 });
            s.mem_used_mib = Some(k as u64);
            store.push_node(s);
            // 값 없는 점은 세지 않는다.
            let mut empty = node(t + 1);
            empty.cpu = None;
            empty.mem_used_mib = Some(1_000 + k as u64);
            store.push_node(empty);
        }
        let s = store.summary(now);
        assert_eq!(s.cpu1, Some(300));
        // 창 안(t ≥ now − 300초)의 점: 10%가 25개, 30%가 36개 → (250 + 1080) / 61 = 21.80%
        assert_eq!(s.cpu5, Some(218));
        assert_eq!(s.mem, Some(1_072));
        assert!(s.vms.is_empty());

        // CPU 값이 하나도 없으면 None
        let store = MetricsStore::default();
        let mut s = node(now);
        s.cpu = None;
        store.push_node(s);
        assert_eq!(store.summary(now).cpu1, None);
        assert_eq!(store.summary(now).cpu5, None);
    }

    /// VM은 마지막 점이 15초 안인 것만, 5분 CPU 평균과 마지막 RSS로 넣는다.
    #[test]
    fn summary_has_fresh_vms_only() {
        let store = MetricsStore::default();
        let now = 10_000_000;
        let (fresh, gone) = (Uuid::from_u128(1), Uuid::from_u128(2));
        for (t, cpu, rss) in [
            (now - 400_000, 500.0, 100),
            (now - 10_000, 20.0, 200),
            (now - 5_000, 125.5, 300),
        ] {
            let mut s = vm(t);
            s.cpu = Some(cpu);
            s.mem_rss_mib = Some(rss);
            store.push_vm(fresh, s);
        }
        let mut no_cpu = vm(now - LOAD_VM_FRESH_MS);
        no_cpu.cpu = None;
        no_cpu.mem_rss_mib = None;
        store.push_vm(Uuid::from_u128(3), no_cpu);
        store.push_vm(gone, vm(now - 20_000));
        let s = store.summary(now);
        assert_eq!(
            s.vms,
            vec![
                VmLoad {
                    id: fresh,
                    // 400초 전 점은 5분 창 밖이다. (20 + 125.5) / 2 = 72.75% → 728‰
                    cpu: Some(728),
                    mem: Some(300),
                },
                VmLoad {
                    id: Uuid::from_u128(3),
                    cpu: None,
                    mem: None,
                },
            ]
        );
        // 노드 점이 없으면 노드 값은 없다.
        assert_eq!((s.cpu1, s.cpu5, s.mem), (None, None, None));
    }

    /// VM이 상한을 넘으면 RSS 큰 것부터 둔다.
    #[test]
    fn summary_keeps_the_largest_vms() {
        let store = MetricsStore::default();
        let now = 10_000_000;
        for i in 0..MAX_LOAD_VMS as u64 + 10 {
            let mut s = vm(now);
            s.mem_rss_mib = Some(i);
            store.push_vm(Uuid::from_u128(i.into()), s);
        }
        let s = store.summary(now);
        assert_eq!(s.vms.len(), MAX_LOAD_VMS);
        assert_eq!(s.vms.iter().filter_map(|v| v.mem).min(), Some(10));
    }

    /// 기록 쓰기는 앞서 가져간 뒤의 점만 받는다. 넣은 순서로 세므로 시각이 뒤로 간 점도 받고, 지워졌다 다시 생긴
    /// VM 시리즈는 처음부터 받는다.
    #[test]
    fn pending_points_are_the_ones_pushed_since_the_last_take() {
        let store = MetricsStore::default();
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut marks = Marks::default();
        assert_eq!(store.take_pending(&mut marks), Pending::default());
        store.push_node(node(1_000));
        store.push_vm(a, vm(1_000));
        let p = store.take_pending(&mut marks);
        assert_eq!(times(&p.node, |s| s.t), [1_000]);
        assert_eq!(p.vms.len(), 1);
        assert_eq!(p.vms[0].0, a);
        // 아무것도 넣지 않으면 비었다.
        assert_eq!(store.take_pending(&mut marks), Pending::default());
        // 시계가 뒤로 간 점도 새 점이다.
        store.push_node(node(500));
        store.push_vm(b, vm(2_000));
        store.push_vm(b, vm(3_000));
        let p = store.take_pending(&mut marks);
        assert_eq!(times(&p.node, |s| s.t), [500]);
        assert_eq!(p.vms.len(), 1);
        assert_eq!(
            (p.vms[0].0, times(&p.vms[0].1, |s| s.t)),
            (b, vec![2_000, 3_000])
        );
        // 지워진 뒤 다시 생긴 시리즈
        store.prune(10_000_000);
        store.push_vm(a, vm(10_000_000));
        let p = store.take_pending(&mut marks);
        assert_eq!(p.vms.len(), 1);
        assert_eq!(times(&p.vms[0].1, |s| s.t), [10_000_000]);
        // 상한보다 많이 쌓여도 남은 것까지만
        for t in 0..(MAX_SAMPLES as i64 + 10) {
            store.push_node(node(20_000_000 + t));
        }
        assert_eq!(store.take_pending(&mut marks).node.len(), MAX_SAMPLES);
    }

    #[test]
    fn windows_and_last_points() {
        let store = MetricsStore::default();
        let id = Uuid::from_u128(9);
        for t in [1_000, 2_000, 3_000] {
            store.push_node(node(t));
            store.push_vm(id, vm(t));
        }
        assert_eq!(
            times(&store.node_window(2_000, 3_000), |s| s.t),
            [2_000, 3_000]
        );
        assert_eq!(times(&store.vm_window(id, 0, 1_000), |s| s.t), [1_000]);
        assert!(store.vm_window(Uuid::nil(), 0, 5_000).is_empty());
        assert_eq!(store.last_vm_t(id), Some(3_000));
        assert_eq!(store.last_vm_t(Uuid::nil()), None);
        assert_eq!(store.vm_ids(), [id]);
    }

    #[test]
    fn source_names_this_node_and_the_vm() {
        let store = Arc::new(MetricsStore::default());
        let (me, id) = (Uuid::now_v7(), Uuid::now_v7());
        store.push_node(node(1_000));
        store.push_node(node(2_000));
        store.push_vm(id, vm(1_000));
        let source = StoreSource::new(me, store);

        let reply = source.read(&query(MetricsScope::Nodes, Some(1_000)));
        let series = reply.node.unwrap();
        assert_eq!(series.node, me);
        assert_eq!(times(&series.samples, |s| s.t), [2_000]);
        assert!(reply.vms.is_empty());

        let reply = source.read(&query(MetricsScope::Vm { id }, None));
        assert_eq!(reply.node, None);
        assert_eq!(reply.vms.len(), 1);
        assert_eq!((reply.vms[0].vm, reply.vms[0].node), (id, me));
        assert_eq!(times(&reply.vms[0].samples, |s| s.t), [1_000]);
        // 이 노드에서 잰 적이 없는 VM은 시리즈가 없다.
        let other = source.read(&query(MetricsScope::Vm { id: Uuid::now_v7() }, None));
        assert!(other.vms.is_empty());
    }
}
