//! 지표 저장소. 이 노드의 시리즈 하나와 이 노드에서 잰 VM마다 시리즈 하나를 메모리에 둔다.
//! API와 노드 간 서버는 [`StoreSource`]로 읽는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use opl_peer::{MetricsReply, MetricsSource};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries, VmSample, VmSeries};
use uuid::Uuid;

/// 시리즈마다 두는 점 수(5초 × 1시간). 넘으면 앞을 버린다.
pub const MAX_SAMPLES: usize = 720;
/// 마지막 점이 이만큼(밀리초) 지난 VM 시리즈는 지운다(옮겨 나간 VM, 지운 VM).
pub const VM_SERIES_TTL_MS: i64 = 3_600_000;

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
