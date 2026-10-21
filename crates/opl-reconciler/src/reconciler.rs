use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::Hypervisor;
use opl_types::resource::Kind;
use opl_types::vm::VmSpec;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::ReconcilerConfig;
use crate::worker::{Trigger, Worker};

/// 한 노드의 조정 루프. `spawn`으로 시작하고 `shutdown`으로 멈춘다.
///
/// 멈춰도 VM은 건드리지 않는다. 다시 시작하면(하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙은 뒤)
/// 현재 실제 상태에서부터 이어서 맞춘다.
pub struct Reconciler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Reconciler {
    pub fn spawn(cluster: Arc<Cluster>, hv: Arc<dyn Hypervisor>, config: ReconcilerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        let dispatcher = Dispatcher {
            cluster,
            hv,
            config: Arc::new(config),
            workers: HashMap::new(),
            retired: HashMap::new(),
        };
        Self {
            shutdown,
            task: tokio::spawn(dispatcher.run(rx)),
        }
    }

    /// 진행 중인 단계가 끝나기를 기다린 뒤 돌아온다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

#[derive(Debug, thiserror::Error)]
enum SyncError {
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    #[error(transparent)]
    Watch(#[from] WatchError),
}

struct WorkerHandle {
    tx: mpsc::UnboundedSender<Trigger>,
    task: JoinHandle<()>,
}

/// 저장소 변경, 하이퍼바이저 이벤트, 주기적 재동기화를 VM별 작업자에게 나눠 준다.
struct Dispatcher {
    cluster: Arc<Cluster>,
    hv: Arc<dyn Hypervisor>,
    config: Arc<ReconcilerConfig>,
    /// 이 노드에 배정된 VM의 작업자
    workers: HashMap<Uuid, WorkerHandle>,
    /// 내보냈지만 아직 끝나지 않았을 수 있는 작업자. 같은 VM이 다시 배정되면 새 작업자가 기다린다.
    retired: HashMap<Uuid, JoinHandle<()>>,
}

impl Dispatcher {
    async fn run(mut self, mut shutdown: oneshot::Receiver<()>) {
        // 목록을 읽기 전에 구독해야 그 사이에 난 이벤트를 놓치지 않는다.
        let mut events = self.hv.subscribe();
        'sync: loop {
            let mut watch = match self.full_sync().await {
                Ok(watch) => watch,
                Err(e) => {
                    tracing::warn!(error = %e, "full sync failed; will retry");
                    tokio::select! {
                        _ = &mut shutdown => break,
                        () = tokio::time::sleep(self.config.retry_interval) => continue,
                    }
                }
            };
            let resync = tokio::time::sleep(self.config.resync_interval);
            tokio::pin!(resync);
            loop {
                tokio::select! {
                    _ = &mut shutdown => break 'sync,
                    () = &mut resync => continue 'sync,
                    ev = watch.next() => match ev {
                        Ok(WatchEvent::Put { kind: Kind::Vm, resource })
                        | Ok(WatchEvent::Deleted { kind: Kind::Vm, resource, .. }) => {
                            self.vm_changed(resource.id);
                        }
                        Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                        Ok(_) => {}
                    },
                    ev = events.recv() => match ev {
                        Ok(ev) => {
                            if !self.notify(ev.id, Trigger::Event(ev.kind)) {
                                tracing::debug!(vm = %ev.id, "event for a VM not assigned to this node");
                            }
                        }
                        // 이벤트를 놓쳤다. 실제 상태를 전부 다시 맞춘다.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue 'sync,
                        Err(broadcast::error::RecvError::Closed) => break 'sync,
                    },
                }
            }
        }
        let workers = self.workers.drain().map(|(_, w)| w.task);
        let tasks: Vec<_> = workers
            .chain(self.retired.drain().map(|(_, t)| t))
            .collect();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// 목록을 다시 읽어 작업자를 맞추고, 그 revision부터 구독한다.
    async fn full_sync(&mut self) -> Result<Watch, SyncError> {
        self.retired.retain(|_, task| !task.is_finished());
        let (revision, vms) = self.cluster.list::<VmSpec>()?;
        let mine: HashSet<Uuid> = vms
            .iter()
            .filter(|vm| vm.spec.node == Some(self.config.node))
            .map(|vm| vm.id)
            .collect();
        let gone: Vec<Uuid> = self
            .workers
            .keys()
            .filter(|id| !mine.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.retire(id);
        }
        for &id in &mine {
            self.sync(id);
        }
        // 경고를 남기려는 것뿐이므로 실패해도 동기화는 이어 간다.
        match self.hv.list().await {
            Ok(states) => {
                for state in states {
                    if !mine.contains(&state.id) {
                        tracing::warn!(vm = %state.id, "VM is running on this node but not assigned to it; leaving it alone");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "cannot list VMs on this node"),
        }
        Ok(self.cluster.watch(revision)?)
    }

    fn vm_changed(&mut self, id: Uuid) {
        match self.cluster.get::<VmSpec>(id) {
            Ok(Some(vm)) if vm.spec.node == Some(self.config.node) => self.sync(id),
            Ok(_) => self.retire(id),
            Err(e) => {
                tracing::warn!(vm = %id, error = %e, "cannot read VM");
                self.notify(id, Trigger::Sync);
            }
        }
    }

    fn notify(&self, id: Uuid, trigger: Trigger) -> bool {
        self.workers
            .get(&id)
            .is_some_and(|w| w.tx.send(trigger).is_ok())
    }

    fn sync(&mut self, id: Uuid) {
        if self.notify(id, Trigger::Sync) {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = Worker::new(
            id,
            self.cluster.clone(),
            self.hv.clone(),
            self.config.clone(),
        );
        let previous = self.retired.remove(&id);
        let task = tokio::spawn(worker.run(previous, rx));
        let _ = tx.send(Trigger::Sync);
        self.workers.insert(id, WorkerHandle { tx, task });
    }

    /// 삭제되었거나 다른 노드로 배정된 VM의 작업자를 내보낸다. 작업자는 마지막으로 한 번 확인하고 끝난다.
    fn retire(&mut self, id: Uuid) {
        if let Some(w) = self.workers.remove(&id) {
            let _ = w.tx.send(Trigger::Sync);
            self.retired.insert(id, w.task);
        }
    }
}
