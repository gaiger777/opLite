//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.
//! API와 노드 간 서버는 [`StoreSource`]로 읽는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use opl_peer::{MetricsReply, MetricsSource};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use opl_types::placement::{LoadSummary, VmLoad};
use uuid::Uuid;

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
        push(&mut self.inner.lock().unwrap().node, sample);
    }

    pub fn push_vm(&self, vm: Uuid, sample: VmSample) {
        push(
            self.inner.lock().unwrap().vms.entry(vm).or_default(),
            sample,
        );
    }

    /// 마지막 점이 `now`(유닉스 밀리초)보다 [`VM_SERIES_TTL_MS`] 넘게 앞선 VM 시리즈를 지운다.
    pub fn prune(&self, now: i64) {
        self.inner.lock().unwrap().vms.retain(|_, series| {
            series
                .back()
                .is_some_and(|last| now - last.t <= VM_SERIES_TTL_MS)
        });
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

/// 이 노드(`node`)의 저장소를 지표 API와 노드 간 서버(`ReadMetrics`)에 읽어 준다.
pub struct StoreSource {
    node: Uuid,
    store: Arc<MetricsStore>,
}

impl StoreSource {
    pub fn new(node: Uuid, store: Arc<MetricsStore>) -> Self {
        Self { node, store }
    }
}

impl MetricsSource for StoreSource {
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
